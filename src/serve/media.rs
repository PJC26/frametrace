//! Media API handlers: frame capture, clip export, telemetry, transcode queue, proxy, batch export.

use super::*;

/// `POST /api/capture-frame`: store an examiner-captured video frame as a
/// case artifact. The viewer sends the canvas JPEG base64; we verify the
/// magic bytes, hash the capture, and chain an audit event so the frame is
/// traceable back to the record and playback position.
pub(crate) fn api_capture_frame(request: &Request, state: &SharedState) -> String {
    let case_dir = {
        let guard = state_lock(state);
        guard.case_dir.clone()
    };
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 케이스를 열거나 분석을 실행하십시오.\"}"
            .to_string();
    };
    let id = body_value(&request.body, "id").unwrap_or_default();
    if id.trim().is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return api_err("증거 id가 올바르지 않습니다.");
    }
    // The playback position is part of the custody record: it must be a
    // finite, non-negative number of seconds (or omitted).
    let time = body_value(&request.body, "time").unwrap_or_default();
    let time = time.trim().to_string();
    if !time.is_empty() && !time.parse::<f64>().is_ok_and(|t| t.is_finite() && t >= 0.0) {
        return api_err("재생 시각 값이 올바르지 않습니다.");
    }
    let image_b64 = body_value(&request.body, "image").unwrap_or_default();
    // The route's request cap is MAX_CAPTURE_FRAME_BODY (http.rs): a 4K
    // JPEG's base64 easily exceeds the generic 4 MiB MAX_BODY.
    if image_b64.is_empty() || image_b64.len() > MAX_CAPTURE_FRAME_BODY {
        return "{\"ok\":false,\"error\":\"프레임 이미지가 비었거나 너무 큽니다.\"}".to_string();
    }
    let Some(bytes) = crate::audit_key::base64_decode(&image_b64) else {
        return "{\"ok\":false,\"error\":\"이미지 디코딩에 실패했습니다.\"}".to_string();
    };
    if !bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "{\"ok\":false,\"error\":\"JPEG 프레임이 아닙니다.\"}".to_string();
    }
    let stamp = match crate::util::now_unix() {
        Ok(stamp) => stamp,
        Err(err) => return serde_json::json!({"ok": false, "error": err}).to_string(),
    };
    let dir = case_dir.join("artifacts/captures");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        return format!(
            "{{\"ok\":false,\"error\":{}}}",
            json_string(&err.to_string())
        );
    }
    let target = crate::util::unique_available_path(&dir.join(format!(
        "{}__{}.jpg",
        export_safe_name(&id),
        stamp
    )));
    if let Err(err) = std::fs::write(&target, &bytes) {
        return format!(
            "{{\"ok\":false,\"error\":{}}}",
            json_string(&err.to_string())
        );
    }
    let rel = target
        .strip_prefix(&case_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| target.to_string_lossy().to_string());
    // FOR-06: a capture that is not hashed and chained must not exist as
    // an artifact while the UI reports success — remove it and fail.
    let digest = match audit::digest_file(&target) {
        Ok(digest) => digest,
        Err(err) => {
            let _ = std::fs::remove_file(&target);
            return api_err(&format!("캡처 해시 계산 실패: {err}"));
        }
    };
    let logged = audit::append_chained_jsonl(
        &case_dir.join("evidence/logs/capture-log.jsonl"),
        &format!(
            "{{\"event\":\"frame-capture\",\"record_id\":{},\"output\":{},\"sha256\":{},\"video_time_seconds\":{},\"unix\":{stamp}}}",
            json_string(&id),
            json_string(&rel),
            json_string(&digest),
            json_string(&time)
        ),
    );
    if let Err(err) = logged {
        let _ = std::fs::remove_file(&target);
        return api_err(&format!(
            "감사 로그 기록 실패 — 캡처를 저장하지 않았습니다: {err}"
        ));
    }
    format!(
        "{{\"ok\":true,\"path\":{},\"sha256\":{}}}",
        json_string(&rel),
        json_string(&digest)
    )
}

/// `POST /api/export-clip`: export the viewer's in/out range of one record
/// through `export-video`, producing a deliverable clip under
/// artifacts/clips/ with its own chained export-log entry.
///
/// UX-09: the export is a background job that does NOT hold the global
/// busy lock, so marks autosave keeps working while ffmpeg runs. The
/// response is immediate: `{ok:true, job:{id, kind:"export-clip", state,
/// started_unix, ...}, deduped}`; an identical in-flight (or finished, with
/// its clip still on disk) request returns the same job. A finished job
/// carries `path` and the clip's `sha256` (also mirrored at the top level).
/// Poll `GET /api/export-clip-status?job=ID` (or `clip_jobs` in
/// /api/status); cancel with `POST /api/export-clip-cancel {job}`.
pub(crate) fn api_export_clip(request: &Request, state: &SharedState) -> String {
    match std::env::current_exe() {
        Ok(exe) => export_clip_with_exe(request, state, &exe),
        Err(err) => api_err(&err.to_string()),
    }
}

/// Upper bound on concurrently running clip exports (each is an ffmpeg).
pub(crate) const MAX_RUNNING_CLIP_JOBS: usize = 4;
/// Finished clip jobs kept for dedupe/status before the oldest is dropped.
const KEPT_FINISHED_CLIP_JOBS: usize = 32;

/// [`api_export_clip`] parameterised on the CLI executable (tests).
pub(crate) fn export_clip_with_exe(request: &Request, state: &SharedState, exe: &Path) -> String {
    let (case_dir, busy) = {
        let guard = state_lock(state);
        (guard.case_dir.clone(), guard.busy)
    };
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 케이스를 열거나 분석을 실행하십시오.\"}"
            .to_string();
    };
    let id = body_value(&request.body, "id").unwrap_or_default();
    let start = body_value(&request.body, "start")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(-1.0);
    let duration = body_value(&request.body, "duration")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(-1.0);
    // NaN/negative/parse failures are all rejected here.
    if id.trim().is_empty()
        || id.len() > 512
        || id.chars().any(char::is_control)
        || !start.is_finite()
        || start < 0.0
        || !duration.is_finite()
        || duration <= 0.0
    {
        return "{\"ok\":false,\"error\":\"구간 값이 올바르지 않습니다 (id, start, duration).\"}"
            .to_string();
    }
    // export-video resolves vid_* ids and indexed source paths. Carved /
    // filesystem records carry non-indexed ids (carve_*, fls_*) — for
    // those the viewer also sends the record's file path, which we only
    // honor when it sits inside the approved media roots. The canonical
    // (absolute) path is what reaches argv, so it can never read as a flag.
    let selector = if id.starts_with("vid_") {
        id.clone()
    } else {
        let alt = body_value(&request.body, "path").unwrap_or_default();
        let roots = state_lock(state).media_roots.clone();
        let canonical = PathBuf::from(alt.trim())
            .canonicalize()
            .ok()
            .filter(|canonical| {
                roots
                    .iter()
                    .filter_map(|root| root.canonicalize().ok())
                    .any(|root| path_is_under(&root, canonical))
            });
        let Some(canonical) = canonical else {
            return "{\"ok\":false,\"error\":\"색인된 영상 또는 허용된 경로의 파일만 클립으로 보낼 수 있습니다.\"}"
                .to_string();
        };
        canonical.to_string_lossy().to_string()
    };
    let burn_in = body_value(&request.body, "burn_in").as_deref() == Some("true");
    let exhibit = body_value(&request.body, "exhibit")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && burn_in);
    if exhibit
        .as_deref()
        .is_some_and(|v| v.len() > 200 || v.chars().any(char::is_control))
    {
        return api_err("증거번호(exhibit) 값이 올바르지 않습니다.");
    }
    let hash_source = body_value(&request.body, "hash_source").as_deref() == Some("true");
    let key = format!(
        "{}\u{1f}{selector}\u{1f}{start:.3}\u{1f}{duration:.3}\u{1f}{burn_in}\u{1f}{}\u{1f}{hash_source}",
        case_dir.display(),
        exhibit.as_deref().unwrap_or_default()
    );
    // Dedupe first: an identical request joins the existing job even while
    // another job holds `busy` (it is the same, already-approved export).
    if let Some(existing) = find_reusable_clip_job(state, &key) {
        return clip_response(&existing, true);
    }
    if busy {
        return api_err("분석 작업이 진행 중입니다 — 완료 후 다시 시도하십시오.");
    }
    let dir = case_dir.join("artifacts/clips");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        return api_err(&err.to_string());
    }
    let base_name = format!(
        "{}__{}s+{}s",
        export_safe_name(&id),
        start as u64,
        duration as u64
    );
    // Registered under the state lock (re-checking dedupe and the running
    // cap), so two racing identical POSTs still yield exactly one job and
    // two different jobs never get the same not-yet-created output path.
    let job = {
        let mut guard = state_lock(state);
        if let Some(existing) = guard
            .clip_jobs
            .iter()
            .find(|job| job.key == key && job.state == "running")
        {
            return clip_response(existing, true);
        }
        if guard
            .clip_jobs
            .iter()
            .filter(|job| job.state == "running")
            .count()
            >= MAX_RUNNING_CLIP_JOBS
        {
            return api_err(
                "동시에 진행 중인 클립 내보내기가 너무 많습니다 — 하나가 끝난 뒤 다시 시도하십시오.",
            );
        }
        guard.clip_seq += 1;
        let job_id = format!("clip-{}", guard.clip_seq);
        let mut output = crate::util::unique_available_path(&dir.join(format!("{base_name}.mp4")));
        if guard
            .clip_jobs
            .iter()
            .any(|job| job.state == "running" && job.output == output)
        {
            output =
                crate::util::unique_available_path(&dir.join(format!("{base_name}__{job_id}.mp4")));
        }
        let rel_path = output
            .strip_prefix(&case_dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| output.to_string_lossy().to_string());
        let job = ClipJob {
            id: job_id,
            key,
            record_id: id.clone(),
            output,
            rel_path,
            started_unix: crate::util::now_unix().unwrap_or(0),
            finished_unix: None,
            state: "running",
            sha256: None,
            size_bytes: None,
            error: None,
            cancel_requested: false,
        };
        guard.clip_jobs.push(job.clone());
        prune_finished_clip_jobs(&mut guard.clip_jobs);
        job
    };
    let mut args = vec![
        "export-video".into(),
        case_dir.to_string_lossy().to_string(),
        selector,
        "--format".into(),
        "mp4".into(),
        "--start".into(),
        format!("{start:.3}"),
        "--duration".into(),
        format!("{duration:.3}"),
        "--output".into(),
        job.output.to_string_lossy().to_string(),
    ];
    // Court-submission burn-in: exhibit label optional — case id, hash
    // prefix, and ms timecode are always stamped when requested. SEC-06:
    // the label travels as ONE `--exhibit=<value>` token, so a value
    // starting with `-` can never be parsed as a separate flag.
    if burn_in {
        args.push("--burn-in".into());
        if let Some(exhibit) = exhibit {
            args.push(exhibit_arg(&exhibit));
        }
    }
    if hash_source {
        args.push("--hash-source".into());
    }
    let worker_state = Arc::clone(state);
    let worker_exe = exe.to_path_buf();
    let job_id = job.id.clone();
    let spawned = thread::Builder::new()
        .name("ft-clip".into())
        .spawn(move || run_clip_job(&worker_state, &worker_exe, &job_id, &args));
    if spawned.is_err() {
        update_clip_job(state, &job.id, |job| {
            job.state = "failed";
            job.error = Some("작업 스레드 시작 실패".into());
            job.finished_unix = crate::util::now_unix().ok();
        });
        return api_err("작업 스레드 시작 실패");
    }
    state_lock(state)
        .logs
        .push(format!("클립 내보내기 시작: {} ({})", job.rel_path, job.id));
    clip_response(&job, false)
}

/// The single argv token carrying the burn-in exhibit label (SEC-06).
pub(crate) fn exhibit_arg(exhibit: &str) -> String {
    format!("--exhibit={exhibit}")
}

/// A running job with `key`, or a finished one whose clip is still on disk
/// with the recorded size (so a deleted/replaced clip is exported anew).
fn find_reusable_clip_job(state: &SharedState, key: &str) -> Option<ClipJob> {
    let guard = state_lock(state);
    guard
        .clip_jobs
        .iter()
        .rev()
        .find(|job| {
            job.key == key
                && match job.state {
                    "running" => true,
                    "done" => std::fs::metadata(&job.output)
                        .is_ok_and(|meta| Some(meta.len()) == job.size_bytes),
                    _ => false,
                }
        })
        .cloned()
}

fn prune_finished_clip_jobs(jobs: &mut Vec<ClipJob>) {
    while jobs.iter().filter(|job| job.state != "running").count() > KEPT_FINISHED_CLIP_JOBS {
        match jobs.iter().position(|job| job.state != "running") {
            Some(index) => {
                jobs.remove(index);
            }
            None => break,
        }
    }
}

fn update_clip_job(state: &SharedState, id: &str, update: impl FnOnce(&mut ClipJob)) {
    if let Some(job) = state_lock(state)
        .clip_jobs
        .iter_mut()
        .find(|job| job.id == id)
    {
        update(job);
    }
}

/// Worker: runs export-video (killed as a tree on a per-job cancel), then
/// hashes the finished clip. A failed/cancelled export never leaves a
/// partial file posing as a deliverable.
fn run_clip_job(state: &SharedState, exe: &Path, id: &str, args: &[String]) {
    let cancelled = || {
        state_lock(state)
            .clip_jobs
            .iter()
            .find(|job| job.id == id)
            .is_none_or(|job| job.cancel_requested)
    };
    let output = state_lock(state)
        .clip_jobs
        .iter()
        .find(|job| job.id == id)
        .map(|job| job.output.clone());
    let Some(output) = output else { return };
    let result = run_step_cancellable(exe, args, &cancelled);
    let finished = crate::util::now_unix().ok();
    let outcome = match result {
        Ok(_) => match crate::audit::digest_file(&output) {
            Ok(digest) => Ok((
                digest,
                std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0),
            )),
            Err(err) => Err(format!("클립은 만들어졌지만 SHA-256 계산 실패: {err}")),
        },
        Err(err) => Err(err),
    };
    let was_cancelled = cancelled();
    if outcome.is_err() {
        // A torn or unverified clip must not linger as a deliverable.
        let _ = std::fs::remove_file(&output);
    }
    let mut guard = state_lock(state);
    let mut log = None;
    if let Some(job) = guard.clip_jobs.iter_mut().find(|job| job.id == id) {
        job.finished_unix = finished;
        match outcome {
            Ok((digest, size)) => {
                log = Some(format!(
                    "클립 내보내기 완료: {} · SHA-256 {digest}",
                    job.rel_path
                ));
                job.state = "done";
                job.sha256 = Some(digest);
                job.size_bytes = Some(size);
            }
            Err(_) if was_cancelled => {
                job.state = "cancelled";
                job.error = Some("사용자가 클립 내보내기를 중단했습니다.".into());
                log = Some(format!("클립 내보내기 중단: {}", job.rel_path));
            }
            Err(err) => {
                log = Some(format!(
                    "오류: 클립 내보내기 실패 ({}): {err}",
                    job.rel_path
                ));
                job.state = "failed";
                job.error = Some(err);
            }
        }
    }
    if let Some(line) = log {
        guard.logs.push(line);
    }
}

/// Number of clip exports still running (finalize/recover/carve/shutdown
/// wait for them).
pub(crate) fn running_clip_job_count(state: &SharedState) -> usize {
    state_lock(state)
        .clip_jobs
        .iter()
        .filter(|job| job.state == "running")
        .count()
}

/// Requests cancel of every running clip export; returns how many.
pub(crate) fn cancel_all_clip_jobs(guard: &mut JobState) -> usize {
    let mut count = 0;
    for job in guard
        .clip_jobs
        .iter_mut()
        .filter(|job| job.state == "running")
    {
        job.cancel_requested = true;
        count += 1;
    }
    count
}

/// Public view of one clip job.
pub(crate) fn clip_job_json(job: &ClipJob) -> serde_json::Value {
    let end = job
        .finished_unix
        .or_else(|| crate::util::now_unix().ok())
        .unwrap_or(job.started_unix);
    // ffmpeg writes the clip progressively: its size is the honest
    // progress signal (no total is known up front).
    let bytes_written = if job.state == "running" {
        std::fs::metadata(&job.output).map(|m| m.len()).unwrap_or(0)
    } else {
        job.size_bytes.unwrap_or(0)
    };
    serde_json::json!({
        "id": job.id,
        "kind": "export-clip",
        "record_id": job.record_id,
        "state": job.state,
        "started_unix": job.started_unix,
        "finished_unix": job.finished_unix,
        "elapsed_secs": end.saturating_sub(job.started_unix),
        "path": job.rel_path,
        "bytes_written": bytes_written,
        "sha256": job.sha256,
        "size_bytes": job.size_bytes,
        "error": job.error,
        "cancel_requested": job.cancel_requested,
    })
}

fn clip_response(job: &ClipJob, deduped: bool) -> String {
    let mut body = serde_json::json!({
        "ok": true,
        "job": clip_job_json(job),
        "deduped": deduped,
    });
    if job.state == "done"
        && let Some(map) = body.as_object_mut()
    {
        map.insert("path".into(), serde_json::json!(job.rel_path));
        map.insert("sha256".into(), serde_json::json!(job.sha256));
    }
    body.to_string()
}

/// `GET /api/export-clip-status?job=ID`.
pub(crate) fn api_export_clip_status(request: &Request, state: &SharedState) -> String {
    let id = query_value(&request.query, "job").unwrap_or_default();
    let guard = state_lock(state);
    match guard.clip_jobs.iter().find(|job| job.id == id) {
        Some(job) => clip_response(job, false),
        None => api_err("해당 클립 내보내기 작업을 찾을 수 없습니다."),
    }
}

/// `POST /api/export-clip-cancel {job}`: kills that export's process tree.
pub(crate) fn api_export_clip_cancel(request: &Request, state: &SharedState) -> String {
    let id = body_value(&request.body, "job").unwrap_or_default();
    let mut guard = state_lock(state);
    match guard.clip_jobs.iter_mut().find(|job| job.id == id) {
        Some(job) if job.state == "running" => {
            job.cancel_requested = true;
            "{\"ok\":true}".to_string()
        }
        Some(_) => api_err("이미 끝난 클립 내보내기입니다."),
        None => api_err("해당 클립 내보내기 작업을 찾을 수 없습니다."),
    }
}

/// `POST /api/telemetry`: return the telemetry artifact for a record,
/// extracting it lazily via `extract-telemetry` when absent. Same
/// selector/path confinement rules as export-clip.
pub(crate) fn api_telemetry(request: &Request, state: &SharedState) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 케이스를 열거나 분석을 실행하십시오.\"}"
            .to_string();
    };
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
    let id = body_value(&request.body, "id").unwrap_or_default();
    if id.trim().is_empty() {
        return "{\"ok\":false,\"error\":\"증거 id가 비어 있습니다.\"}".to_string();
    }
    let selector = if id.starts_with("vid_") {
        id.clone()
    } else {
        let alt = body_value(&request.body, "path").unwrap_or_default();
        let candidate = PathBuf::from(alt.trim());
        let roots = state_lock(state).media_roots.clone();
        let allowed = candidate
            .canonicalize()
            .map(|canonical| {
                roots
                    .iter()
                    .filter_map(|root| root.canonicalize().ok())
                    .any(|root| path_is_under(&root, &canonical))
            })
            .unwrap_or(false);
        if !allowed {
            return "{\"ok\":false,\"error\":\"색인된 영상 또는 허용된 경로의 파일만 텔레메트리를 추출할 수 있습니다.\"}"
                .to_string();
        }
        candidate.to_string_lossy().to_string()
    };
    // The artifact is keyed by the record id — the viewer joins
    // DATA.telemetry[sanitized-id], so a path selector must never leak
    // into the filename.
    let artifact = crate::telemetry::artifact_path(&case_dir, &id);
    if !artifact.is_file() {
        let source = match crate::video_export::resolve_video_source(&case_dir, &selector) {
            Ok(p) => p,
            Err(err) => return serde_json::json!({"ok": false, "error": err}).to_string(),
        };
        let mut report = match crate::telemetry::extract_file(&source) {
            Ok(r) => r,
            Err(err) => return serde_json::json!({"ok": false, "error": err}).to_string(),
        };
        report.selector = id.clone();
        let dir = case_dir.join("artifacts/telemetry");
        if let Err(err) = std::fs::create_dir_all(&dir) {
            return format!(
                "{{\"ok\":false,\"error\":{}}}",
                json_string(&err.to_string())
            );
        }
        let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into());
        if let Err(err) = crate::util::write_text_atomic(&artifact, &text) {
            return format!(
                "{{\"ok\":false,\"error\":{}}}",
                json_string(&err.to_string())
            );
        }
    }
    match std::fs::read_to_string(&artifact) {
        Ok(text) => format!("{{\"ok\":true,\"report\":{}}}", text),
        Err(err) => format!(
            "{{\"ok\":false,\"error\":{}}}",
            json_string(&format!("텔레메트리 결과를 읽지 못했습니다: {err}"))
        ),
    }
}

/// `POST /api/transcode-queue`: batch-transcode every browser-unplayable /
/// proprietary-format record to an H.264 review proxy via the CLI queue.
/// `ids` (comma-separated) restricts the queue; `force` re-runs even when
/// a proxy already exists.
pub(crate) fn api_transcode_queue(request: &Request, state: &SharedState) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 케이스를 열거나 분석을 실행하십시오.\"}"
            .to_string();
    };
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            return format!(
                "{{\"ok\":false,\"error\":{}}}",
                json_string(&err.to_string())
            );
        }
    };
    let mut args = vec![
        "transcode-queue".into(),
        case_dir.to_string_lossy().to_string(),
    ];
    let ids = body_value(&request.body, "ids").unwrap_or_default();
    let ids = ids.trim();
    if !ids.is_empty() {
        // Ids travel as a comma list; keep anything that could look like a
        // flag out of argv by rejecting leading dashes.
        if ids.split(',').any(|v| v.trim().starts_with('-')) {
            return "{\"ok\":false,\"error\":\"id 값이 올바르지 않습니다.\"}".to_string();
        }
        args.push("--only".into());
        args.push(ids.to_string());
    }
    if body_value(&request.body, "force").as_deref() == Some("true") {
        args.push("--force".into());
    }
    match run_step(&exe, &args, state) {
        Ok(stdout) => {
            let result = stdout
                .lines()
                .find_map(|l| l.strip_prefix("QUEUE-RESULT "))
                .map(str::to_string)
                .unwrap_or_else(|| "{}".to_string());
            let latest = crate::transcode::latest_summary(&case_dir)
                .map(|v| v.to_string())
                .unwrap_or_else(|| result.clone());
            format!("{{\"ok\":true,\"summary\":{}}}", latest)
        }
        Err(err) => serde_json::json!({"ok": false, "error": err}).to_string(),
    }
}

/// `POST /api/proxy`: return (or lazily generate via `make-proxy`) the
/// review proxy for an indexed video so the viewer can offer smooth
/// playback of heavy originals without forcing a full proxy pass.
pub(crate) fn api_proxy(request: &Request, state: &SharedState) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 케이스를 열거나 분석을 실행하십시오.\"}"
            .to_string();
    };
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
    let id = body_value(&request.body, "id").unwrap_or_default();
    if id.trim().is_empty() {
        return "{\"ok\":false,\"error\":\"증거 id가 비어 있습니다.\"}".to_string();
    }
    // make-proxy resolves vid_* ids and indexed source paths. Carved /
    // filesystem records carry non-indexed ids (carve_*, fls_*) — for
    // those the viewer also sends the record's file path, which we only
    // honor when it sits inside the approved media roots.
    let selector = if id.starts_with("vid_") {
        id.clone()
    } else {
        let alt = body_value(&request.body, "path").unwrap_or_default();
        let candidate = PathBuf::from(alt.trim());
        let roots = state_lock(state).media_roots.clone();
        let allowed = candidate
            .canonicalize()
            .map(|canonical| {
                roots
                    .iter()
                    .filter_map(|root| root.canonicalize().ok())
                    .any(|root| path_is_under(&root, &canonical))
            })
            .unwrap_or(false);
        if !allowed {
            return "{\"ok\":false,\"error\":\"색인된 영상 또는 허용된 경로의 파일만 프록시로 재생할 수 있습니다.\"}"
                .to_string();
        }
        candidate.to_string_lossy().to_string()
    };
    let dir = case_dir.join("artifacts/proxies");
    let find_existing = |dir: &Path| -> Option<PathBuf> {
        // generate_proxy names outputs with video_export::sanitize_filename,
        // not export_safe_name — match that or the cache lookup misses.
        let prefix = format!(
            "{}_proxy_",
            crate::video_export::sanitize_filename(&selector)
        );
        std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name.ends_with(".mp4") {
                Some(entry.path())
            } else {
                None
            }
        })
    };
    let proxy = match find_existing(&dir) {
        Some(path) => path,
        None => {
            let exe = match std::env::current_exe() {
                Ok(exe) => exe,
                Err(err) => {
                    return format!(
                        "{{\"ok\":false,\"error\":{}}}",
                        json_string(&err.to_string())
                    );
                }
            };
            let args = vec![
                "make-proxy".into(),
                case_dir.to_string_lossy().to_string(),
                selector.clone(),
            ];
            if let Err(err) = run_step(&exe, &args, state) {
                return serde_json::json!({"ok": false, "error": err}).to_string();
            }
            match find_existing(&dir) {
                Some(path) => path,
                None => {
                    return "{\"ok\":false,\"error\":\"프록시 생성 후 파일을 찾지 못했습니다.\"}"
                        .to_string();
                }
            }
        }
    };
    format!(
        "{{\"ok\":true,\"path\":{}}}",
        json_string(proxy.to_string_lossy().as_ref())
    )
}

/// `POST /api/advanced`: run a CLI-only case tool from the workstation.
/// `tool` is an allow-listed name; `extra` is an optional path argument
/// (hash list for known-hash, peer case dir for merge/compare) that is
/// validated to exist — never a free-form command line.
pub(crate) fn api_advanced(request: &Request, state: &SharedState) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 케이스를 열거나 분석을 실행하십시오.\"}"
            .to_string();
    };
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
    let tool = body_value(&request.body, "tool").unwrap_or_default();
    let extra = body_value(&request.body, "extra").unwrap_or_default();
    let case_text = case_dir.to_string_lossy().to_string();
    let (args, output_rel): (Vec<String>, &str) = match tool.as_str() {
        "dfxml" => (
            vec!["export-dfxml".into(), case_text],
            "reports/case-index.dfxml",
        ),
        "timeline" => (vec!["timeline".into(), case_text], "db/timeline.jsonl"),
        "deepfake" => (
            vec!["deepfake-scan".into(), case_text],
            "review/evidence-viewer.html",
        ),
        "qa-consistency" => (
            vec!["qa".into(), "consistency".into(), case_text],
            "reports/qa/consistency-report.html",
        ),
        "qa-defense" => (
            vec!["qa".into(), "report-defense".into(), case_text],
            "reports/qa/report-defense-checklist.md",
        ),
        "known-hash" => {
            if extra.trim().is_empty() || !PathBuf::from(&extra).is_file() {
                return "{\"ok\":false,\"error\":\"sha256 해시 목록 파일 경로를 지정하십시오 (한 줄에 하나, 또는 'sha256,설명').\"}".to_string();
            }
            (
                vec!["known-hash-filter".into(), case_text, extra],
                "reports/known-hash-filter.json",
            )
        }
        "compare" | "merge" => {
            if extra.trim().is_empty() || !crate::case_db::case_db_path(Path::new(&extra)).is_file()
            {
                return "{\"ok\":false,\"error\":\"비교/병합할 다른 케이스 폴더를 지정하십시오 (db/case.db 포함).\"}".to_string();
            }
            let sub = if tool == "compare" {
                "compare-cases"
            } else {
                "merge-cases"
            };
            (
                vec![sub.into(), case_text, extra],
                "reports/case-compare.json",
            )
        }
        _ => {
            return "{\"ok\":false,\"error\":\"지원하지 않는 도구입니다.\"}".to_string();
        }
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            return format!(
                "{{\"ok\":false,\"error\":{}}}",
                json_string(&err.to_string())
            );
        }
    };
    match run_step(&exe, &args, state) {
        Ok(out) => {
            let url = if case_dir.join(output_rel).is_file() {
                output_rel.to_string()
            } else {
                String::new()
            };
            let tail: Vec<&str> = out.lines().rev().take(6).collect();
            let mut tail = tail;
            tail.reverse();
            format!(
                "{{\"ok\":true,\"url\":{},\"output\":{}}}",
                json_string(&url),
                json_string(&tail.join("\n"))
            )
        }
        Err(err) => serde_json::json!({"ok": false, "error": err}).to_string(),
    }
}

/// Copies reviewer-selected evidence into an organized handoff folder:
/// `case/exports/selection-<unix>/` holds the files plus a hash manifest
/// (CSV + JSONL + README) — the material package an examiner hands to a
/// requester. Only paths under the approved media roots are copied, and
/// items without a file (e.g. pre-recovery candidates) are recorded in
/// the manifest as skipped instead of silently vanishing.
pub(crate) fn api_export_selected(request: &Request, state: &SharedState) -> String {
    let (case_dir, roots) = {
        let guard = state_lock(state);
        (guard.case_dir.clone(), guard.media_roots.clone())
    };
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"열린 케이스가 없습니다.\"}".to_string();
    };
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&request.body) else {
        return "{\"ok\":false,\"error\":\"invalid export payload\"}".to_string();
    };
    let items = body
        .get("items")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        return "{\"ok\":false,\"error\":\"선택된 항목이 없습니다.\"}".to_string();
    }
    if items.len() > 5000 {
        return "{\"ok\":false,\"error\":\"한 번에 최대 5000개 항목까지 내보낼 수 있습니다.\"}"
            .to_string();
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0);
    let export_dir =
        crate::util::unique_dir(&case_dir.join("exports").join(format!("selection-{stamp}")));
    if let Err(err) = std::fs::create_dir_all(&export_dir) {
        return format!(
            "{{\"ok\":false,\"error\":{}}}",
            json_string(&format!("내보내기 폴더 생성 실패: {err}"))
        );
    }
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    let field = |item: &serde_json::Value, key: &str| {
        item.get(key)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let joined = |item: &serde_json::Value, key: &str| {
        item.get(key)
            .and_then(|value| value.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|entry| entry.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default()
    };
    // Copy + SHA-256 of every selected file is the expensive part of the
    // batch and is fully independent per item, so it runs on a small worker
    // pool; manifest assembly below stays serial and ordered.
    let outcomes: Mutex<Vec<Option<ExportOutcome>>> =
        Mutex::new((0..items.len()).map(|_| None).collect());
    let next_index = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .clamp(1, 4);
    {
        let items = &items;
        let roots = &roots;
        let export_dir = &export_dir;
        let outcomes = &outcomes;
        let next_index = &next_index;
        let field = &field;
        thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(move || {
                    loop {
                        let index = next_index.fetch_add(1, Ordering::SeqCst);
                        let Some(item) = items.get(index) else {
                            break;
                        };
                        let outcome = export_one(item, roots, export_dir, field);
                        outcomes.lock().unwrap_or_else(|e| e.into_inner())[index] = Some(outcome);
                    }
                });
            }
        });
    }
    let outcomes = outcomes
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut csv_rows = vec![
        "id,exported_file,kind,status,mark,tags,sha256_source,sha256_copy,size_bytes,recorded_time,original_path,source_path,warnings,note".to_string(),
    ];
    let mut jsonl_rows: Vec<String> = Vec::new();
    let mut copied = 0usize;
    let mut skipped = 0usize;
    for (item, outcome) in items.iter().zip(outcomes) {
        let Some(outcome) = outcome else {
            continue;
        };
        let id = field(item, "id");
        let path_text = field(item, "path");
        let ExportOutcome {
            exported,
            copy_hash,
            size,
            skip_reason,
        } = outcome;
        if skip_reason.is_empty() && !exported.is_empty() {
            copied += 1;
        }
        if !skip_reason.is_empty() {
            skipped += 1;
        }
        let base_warnings = joined(item, "warnings");
        let warnings = if skip_reason.is_empty() {
            base_warnings
        } else if base_warnings.is_empty() {
            skip_reason.clone()
        } else {
            format!("{base_warnings}; {skip_reason}")
        };
        csv_rows.push(csv_row(&[
            id.clone(),
            exported.clone(),
            field(item, "kind"),
            field(item, "status"),
            field(item, "mark"),
            joined(item, "tags"),
            field(item, "sha256"),
            copy_hash.clone(),
            size,
            field(item, "rec_time"),
            field(item, "original_path"),
            path_text.clone(),
            warnings,
            field(item, "note"),
        ]));
        let mut row = item.clone();
        row["exported_file"] = serde_json::Value::String(exported);
        row["sha256_copy"] = serde_json::Value::String(copy_hash);
        row["skipped"] = if skip_reason.is_empty() {
            serde_json::Value::Bool(false)
        } else {
            serde_json::Value::String(skip_reason)
        };
        jsonl_rows.push(row.to_string());
    }
    let audit_log = case_dir.join("evidence/logs/export-log.jsonl");
    // FOR-06: the export must not report ok:true when its audit entry was
    // never written — the copies would sit outside the chain of custody.
    let audit_result = audit::append_chained_jsonl(
        &audit_log,
        &format!(
            "{{\"event\":\"selection-export\",\"export_dir\":{},\"items\":{},\"copied\":{copied},\"skipped\":{skipped},\"unix\":{stamp}}}",
            json_string(&export_dir.to_string_lossy()),
            items.len()
        ),
    );
    let write = |name: &str, contents: String| -> Result<(), String> {
        std::fs::write(export_dir.join(name), contents)
            .map_err(|err| format!("manifest write failed for {name}: {err}"))
    };
    let head = audit::chain_head(&audit_log)
        .ok()
        .flatten()
        .unwrap_or_default();
    let result = write("manifest.csv", format!("\u{feff}{}\n", csv_rows.join("\n")))
        .and_then(|_| write("manifest.jsonl", jsonl_rows.join("\n") + "\n"))
        .and_then(|_| {
            write(
                "manifest-audit.json",
                format!(
                    "{{\"audit_log\":{},\"chain_head_sha256\":{},\"audit_logged\":{},\"unix\":{stamp}}}\n",
                    json_string("evidence/logs/export-log.jsonl"),
                    json_string(&head),
                    audit_result.is_ok()
                ),
            )
        })
        .and_then(|_| {
            write(
                "README.txt",
                format!(
                    "FrameTrace 선별 증거 묶음\n\n\
                     생성 시각(unix): {stamp}\n\
                     항목: {}개 — 복사 {copied}개, 제외 {skipped}개\n\n\
                     - 복사된 파일은 원본의 사본입니다. manifest의 sha256_copy가\n  \
                     sha256_source와 동일하면 복사가 정확히 이루어진 것입니다.\n\
                     - manifest.csv: Excel로 여는 항목별 메타데이터 (UTF-8 BOM).\n\
                     - manifest.jsonl: 같은 내용의 기계 판독용 JSONL.\n\
                     - manifest-audit.json: 이 묶음을 만든 감사 체인 상태\n  \
                     (chain_head_sha256가 케이스 export-log의 마지막 줄 해시와\n  \
                     일치하면 해당 시점 감사 기록에서 생성된 묶음입니다).\n\
                     - 복구 전 후보처럼 파일이 없는 항목은 복사되지 않고\n  \
                     manifest에 skipped 사유가 기록됩니다.\n",
                    items.len()
                ),
            )
        });
    if let Err(err) = result {
        return serde_json::json!({"ok": false, "error": err}).to_string();
    }
    if let Err(err) = audit_result {
        return serde_json::json!({
            "ok": false,
            "unlogged": true,
            "error": format!("감사 로그 기록 실패 — 내보낸 사본이 감사 체인에 기록되지 않았습니다: {err}"),
            "export_dir": export_dir.to_string_lossy(),
            "copied": copied,
            "skipped": skipped,
        })
        .to_string();
    }
    format!(
        "{{\"ok\":true,\"export_dir\":{},\"copied\":{copied},\"skipped\":{skipped}}}",
        json_string(&export_dir.to_string_lossy())
    )
}

#[derive(Debug, Default)]
pub(crate) struct ExportOutcome {
    pub(crate) exported: String,
    pub(crate) copy_hash: String,
    pub(crate) size: String,
    pub(crate) skip_reason: String,
}

/// Copies one selected item into the export directory and hashes the copy.
/// Containment against the approved media roots is checked on the
/// canonical path — the same rule the /media endpoint applies.
pub(crate) fn export_one(
    item: &serde_json::Value,
    roots: &[PathBuf],
    export_dir: &Path,
    field: &dyn Fn(&serde_json::Value, &str) -> String,
) -> ExportOutcome {
    let mut outcome = ExportOutcome::default();
    let id = field(item, "id");
    let path_text = field(item, "path");
    if path_text.is_empty() {
        outcome.skip_reason = "no-file(pre-recovery candidate)".to_string();
        return outcome;
    }
    let canonical = match PathBuf::from(&path_text).canonicalize() {
        Ok(canonical) => canonical,
        Err(_) => {
            outcome.skip_reason = "source-missing".to_string();
            return outcome;
        }
    };
    if !roots.iter().any(|root| path_is_under(root, &canonical)) {
        outcome.skip_reason = "outside-approved-roots".to_string();
        return outcome;
    }
    let filename = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| field(item, "name"));
    let base_name = format!("{}__{}", export_safe_name(&id), export_safe_name(&filename));
    match copy_unique(&canonical, export_dir, &base_name) {
        Ok(target) => {
            outcome.exported = target
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            outcome.copy_hash = audit::digest_file(&target).unwrap_or_default();
            outcome.size = std::fs::metadata(&target)
                .map(|meta| meta.len().to_string())
                .unwrap_or_default();
        }
        Err(err) => outcome.skip_reason = format!("copy-failed: {err}"),
    }
    outcome
}

/// Copies `source` to `dir/base_name`, atomically allocating a unique name
/// with create_new: the batch runs on parallel workers, so a check-then-
/// create helper would race when two items produce the same name.
pub(crate) fn copy_unique(source: &Path, dir: &Path, base_name: &str) -> Result<PathBuf, String> {
    let mut reader =
        std::fs::File::open(source).map_err(|err| format!("open source failed: {err}"))?;
    let base = Path::new(base_name);
    let stem = base
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("item")
        .to_string();
    let ext = base
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default();
    for attempt in 0..10_000u32 {
        let candidate = if attempt == 0 {
            dir.join(base_name)
        } else {
            dir.join(format!("{stem}-{attempt}{ext}"))
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut target) => {
                std::io::copy(&mut reader, &mut target)
                    .map_err(|err| format!("copy failed: {err}"))?;
                if let Ok(meta) = source.metadata() {
                    let _ = target.set_permissions(meta.permissions());
                }
                return Ok(candidate);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(format!("create target failed: {err}")),
        }
    }
    Err("could not allocate a unique export filename".to_string())
}

pub(crate) fn csv_row(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| crate::util::csv_safe_cell(field))
        .collect::<Vec<_>>()
        .join(",")
}

/// Windows-safe filename that keeps non-ASCII (e.g. Korean) evidence
/// names — only path separators, forbidden characters, and control
/// characters are replaced; trailing dots/spaces are trimmed.
pub(crate) fn export_safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| {
            if matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || ch.is_control()
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if trimmed.is_empty() {
        "item".to_string()
    } else {
        trimmed
    }
}

pub(crate) fn opt_path(value: &Option<PathBuf>) -> String {
    match value {
        Some(path) => json_string(&path.to_string_lossy()),
        None => "null".to_string(),
    }
}

pub(crate) fn newest_package_dir(case_dir: &Path) -> Option<PathBuf> {
    let entries = case_dir.join("reports").read_dir().ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("package_") || !entry.path().is_dir() {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified())
            && best.as_ref().is_none_or(|(time, _)| modified > *time)
        {
            best = Some((modified, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

#[cfg(test)]
mod csv_row_tests {
    use super::csv_row;

    #[test]
    fn csv_row_neutralizes_formulas_but_keeps_numbers() {
        let row = csv_row(&[
            "=HYPERLINK(\"x\")".to_string(),
            "@SUM(A1)".to_string(),
            "-3600".to_string(),
            "a,b".to_string(),
            "plain".to_string(),
        ]);
        assert_eq!(
            row,
            "\"'=HYPERLINK(\"\"x\"\")\",'@SUM(A1),-3600,\"a,b\",plain"
        );
    }
}
