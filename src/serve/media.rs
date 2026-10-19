//! Media API handlers: frame capture, clip export, telemetry, transcode queue, proxy, batch export.

use super::*;

/// `POST /api/capture-frame`: store an examiner-captured video frame as a
/// case artifact. The viewer sends the canvas JPEG base64; we verify the
/// magic bytes, hash the capture, and chain an audit event so the frame is
/// traceable back to the record and playback position.
pub(crate) fn api_capture_frame(request: &Request, state: &SharedState) -> String {
    // S1: registered as a write op for its whole run, so shutdown,
    // finalize and open-case wait for (refuse during) the capture instead
    // of racing it.
    let (_write_op, case_dir) = match try_begin_write_op(state) {
        Ok(op) => op,
        Err(err) => return api_err(&err),
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

const CLIP_BUSY_ERROR: &str = "분석 작업이 진행 중입니다 — 완료 후 다시 시도하십시오.";

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
    // `busy` here is only a fast-path snapshot; the authoritative check is
    // repeated under the registration lock below (finding 4).
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
    // C12: a `vid_*` id is honored only in its exact index form and is
    // resolved through the index alone (never as a path).
    let selector = if id.starts_with("vid_") {
        if let Err(err) = check_index_video_id(&id) {
            return api_err(&err);
        }
        id.clone()
    } else {
        let alt = body_value(&request.body, "path").unwrap_or_default();
        let roots = state_lock(state).media_roots.clone();
        // Item 8: a UNC/device path never reaches canonicalize.
        let canonical = Some(alt.trim())
            .filter(|alt| super::http::reject_remote_path(alt, &roots).is_ok())
            .and_then(|alt| PathBuf::from(alt).canonicalize().ok())
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
        return api_err(CLIP_BUSY_ERROR);
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
            .find(|job| job.key == key && job.state == "running" && !job.cancel_requested)
            .cloned()
        {
            // The response stats files: render it after the lock is gone.
            drop(guard);
            return clip_response(&existing, true);
        }
        // finalize/recover/carve take `busy` first and then count running
        // clips; re-checking `busy` in the same lock as the registration
        // closes the window where both could proceed (finding 4).
        // S7: no new job once shutdown was accepted.
        if let Some(refusal) = shutdown_refusal(&guard) {
            return api_err(&refusal);
        }
        if guard.busy {
            return api_err(CLIP_BUSY_ERROR);
        }
        // The case may have been switched since the snapshot above.
        if guard.case_dir.as_deref() != Some(case_dir.as_path()) {
            return api_err("케이스가 바뀌었습니다 — 다시 시도하십시오.");
        }
        if guard
            .clip_jobs
            .iter()
            .filter(|job| job.state == "running")
            .count()
            >= MAX_RUNNING_CLIP_JOBS
        {
            return api_err(
                "동시에 진행 중인 클립 내보내기·이어붙이기가 너무 많습니다 — 하나가 끝난 뒤 다시 시도하십시오.",
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
            kind: "export-clip",
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
            progress_path: None,
            expected_bytes: None,
            join: None,
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

/// C12: a `vid_*` record id must be exactly `^vid_\d{6,}$` (what the
/// scanner assigns). Anything else carrying the prefix (`vid_x\..\..\f`)
/// is refused instead of being handed to a resolver that also accepts
/// paths — the prefix used to skip the approved-roots check.
pub(crate) fn check_index_video_id(id: &str) -> Result<(), String> {
    if crate::video_export::is_index_video_id(id) {
        Ok(())
    } else {
        Err(format!(
            "영상 id 형식이 올바르지 않습니다 (vid_ 뒤에 숫자 6자리 이상): {}",
            id.chars().take(64).collect::<String>().escape_debug()
        ))
    }
}

/// A running (not cancel-pending) job with `key`, or a finished one whose clip is still on disk
/// with the recorded size (so a deleted/replaced clip is exported anew).
fn find_reusable_clip_job(state: &SharedState, key: &str) -> Option<ClipJob> {
    // Candidates are cloned under the lock; the on-disk check runs after
    // it is released (no file I/O while holding the state lock).
    let candidates: Vec<ClipJob> = state_lock(state)
        .clip_jobs
        .iter()
        .rev()
        .filter(|job| {
            job.key == key
                && match job.state {
                    // A cancel-pending job is about to die: joining it would
                    // hand the caller a job that ends "cancelled" (finding 3).
                    "running" => !job.cancel_requested,
                    "done" => true,
                    _ => false,
                }
        })
        .cloned()
        .collect();
    candidates.into_iter().find(|job| {
        job.state == "running"
            || std::fs::metadata(&job.output).is_ok_and(|meta| Some(meta.len()) == job.size_bytes)
    })
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
    let result = run_step_cancellable(exe, args, &cancelled, state);
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

/// The `{stage, done, total}` the join CLI writes atomically (None when
/// absent or unreadable — it is a UI hint, never evidence).
fn read_join_progress(path: &Path) -> Option<(String, u64, u64)> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some((
        value.get("stage")?.as_str()?.to_string(),
        value.get("done")?.as_u64()?,
        value.get("total")?.as_u64()?,
    ))
}

/// Public view of one clip export or join job.
///
/// Reads files (the growing output / `.part` and the join progress file),
/// so callers render it from a clone AFTER releasing the state lock.
pub(crate) fn clip_job_json(job: &ClipJob) -> serde_json::Value {
    let end = job
        .finished_unix
        .or_else(|| crate::util::now_unix().ok())
        .unwrap_or(job.started_unix);
    let running = job.state == "running";
    let progress = job
        .progress_path
        .as_deref()
        .filter(|_| running)
        .and_then(read_join_progress);
    // ffmpeg writes progressively: the size of what it is writing (the
    // clip itself, or a join's `.part`) is the honest progress signal.
    let bytes_written = if running {
        let growing = job
            .join
            .as_ref()
            .map(|join| join.temps.part.as_path())
            .unwrap_or(job.output.as_path());
        std::fs::metadata(growing).map(|m| m.len()).unwrap_or(0)
    } else {
        job.size_bytes.unwrap_or(0)
    };
    // A join copies every source byte: the check's summed size, else the
    // CLI's own concat-stage total.
    let expected_bytes = job.expected_bytes.or_else(|| {
        progress
            .as_ref()
            .filter(|(stage, _, _)| stage == "concat")
            .map(|(_, _, total)| *total)
    });
    let path = if job.rel_path.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::json!(job.rel_path)
    };
    let mut value = serde_json::json!({
        "id": job.id,
        "kind": job.kind,
        "record_id": job.record_id,
        "state": job.state,
        "started_unix": job.started_unix,
        "finished_unix": job.finished_unix,
        "elapsed_secs": end.saturating_sub(job.started_unix),
        "path": path,
        "stage": progress.as_ref().map(|(stage, _, _)| stage.clone()),
        "stage_done": progress.as_ref().map(|(_, done, _)| *done),
        "stage_total": progress.as_ref().map(|(_, _, total)| *total),
        "bytes_written": bytes_written,
        "expected_bytes": expected_bytes,
        "sha256": job.sha256,
        "size_bytes": job.size_bytes,
        "error": job.error,
        "cancel_requested": job.cancel_requested,
    });
    if let (Some(join), Some(map)) = (job.join.as_ref(), value.as_object_mut()) {
        map.insert("plan_id".into(), serde_json::json!(join.plan_id));
        map.insert("segments".into(), serde_json::json!(join.ids.len()));
        map.insert("manifest_path".into(), serde_json::json!(join.manifest_rel));
        map.insert(
            "duration_check_ok".into(),
            serde_json::json!(join.duration_check_ok),
        );
        map.insert(
            "duration_check".into(),
            join.duration_check
                .clone()
                .unwrap_or(serde_json::Value::Null),
        );
    }
    value
}

/// Renders the API response for `job` (reads files: call without the
/// state lock held).
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
        if let Some(join) = job.join.as_ref() {
            map.insert("manifest_path".into(), serde_json::json!(join.manifest_rel));
            map.insert(
                "duration_check".into(),
                join.duration_check
                    .clone()
                    .unwrap_or(serde_json::Value::Null),
            );
        }
    }
    body.to_string()
}

/// `GET /api/export-clip-status?job=ID` (clip exports and joins).
pub(crate) fn api_export_clip_status(request: &Request, state: &SharedState) -> String {
    let id = query_value(&request.query, "job").unwrap_or_default();
    let job = state_lock(state)
        .clip_jobs
        .iter()
        .find(|job| job.id == id)
        .cloned();
    match job {
        Some(job) => clip_response(&job, false),
        None => api_err("해당 내보내기·이어붙이기 작업을 찾을 수 없습니다."),
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
        Some(job) if job.kind == "join" => api_err("이미 끝난 이어붙이기입니다."),
        Some(_) => api_err("이미 끝난 클립 내보내기입니다."),
        None => api_err("해당 내보내기·이어붙이기 작업을 찾을 수 없습니다."),
    }
}

// ---------------------------------------------------------------------------
// Lossless join (plan §1.6–1.7): `/api/join-check` plans synchronously,
// `/api/join` runs `join-video` as a clip-style background job.
// ---------------------------------------------------------------------------

/// T1's "id not in the index" blocking message: the server refuses such a
/// request outright (§1.6) instead of returning a blocked plan.
const JOIN_UNKNOWN_ID_MESSAGE: &str = "색인에 없는 영상입니다";
/// Plans remembered for `expected_bytes` (one per recent check).
const KEPT_JOIN_PLANS: usize = 16;

/// Serializes crash-recovery cleanup of `.join-*` leftovers against job
/// starts, so a cleanup never sees a just-registered job's temp files as
/// stale (every job's CLI is spawned only after it passed this gate).
static JOIN_TEMP_GATE: Mutex<()> = Mutex::new(());

/// Parsed `{"ids":[...], "tolerance":1.0, "plan_id":"…"}`.
struct JoinRequest {
    ids: Vec<String>,
    tolerance: f64,
    plan_id: Option<String>,
}

/// Parses and validates a join body (JSON array → serde_json, like
/// export-selected). Ids must be indexed originals (`vid_*`).
fn parse_join_request(body: &str) -> Result<JoinRequest, String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| "요청 형식이 올바르지 않습니다 (ids, tolerance).".to_string())?;
    let ids = value
        .get("ids")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "영상을 2개 이상 선택하십시오".to_string())?
        .iter()
        .map(|id| {
            id.as_str()
                .map(str::to_string)
                .ok_or_else(|| "id는 문자열이어야 합니다.".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tolerance = match value.get("tolerance") {
        None | Some(serde_json::Value::Null) => crate::join::DEFAULT_TOLERANCE_SECONDS,
        Some(raw) => raw
            .as_f64()
            .or_else(|| raw.as_str().and_then(|text| text.trim().parse().ok()))
            .ok_or_else(|| "허용 오차 값이 올바르지 않습니다.".to_string())?,
    };
    crate::join::validate_request(&ids, tolerance)?;
    if let Some(bad) = ids
        .iter()
        .find(|id| !id.starts_with("vid_") || id.contains(','))
    {
        return Err(format!(
            "색인된 원본 영상(vid_*)만 이어붙일 수 있습니다 (후보·복구본·결과물 제외): {}",
            bad.chars().take(64).collect::<String>()
        ));
    }
    let plan_id = match value.get("plan_id") {
        None | Some(serde_json::Value::Null) => None,
        Some(raw) => {
            let text = raw.as_str().unwrap_or_default().trim().to_ascii_lowercase();
            if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("plan_id가 올바르지 않습니다 — 다시 검사하십시오.".to_string());
            }
            Some(text)
        }
    };
    Ok(JoinRequest {
        ids,
        tolerance,
        plan_id,
    })
}

/// `POST /api/join-check {ids, tolerance}`: plans the join (live ffprobe of
/// every segment, at most 4 at once) and returns `{ok:true, plan}` — the
/// plan carries any `blocking` reasons; the request itself is refused only
/// for no case, a malformed/oversized selection, non-`vid_*` or unknown
/// ids, or while an analysis job holds `busy`.
pub(crate) fn api_join_check(request: &Request, state: &SharedState) -> String {
    join_check_with_tools(request, state, &crate::join::Tools::default())
}

/// [`api_join_check`] parameterised on ffmpeg/ffprobe (tests).
pub(crate) fn join_check_with_tools(
    request: &Request,
    state: &SharedState,
    tools: &crate::join::Tools,
) -> String {
    let (case_dir, busy) = {
        let guard = state_lock(state);
        (guard.case_dir.clone(), guard.busy)
    };
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 케이스를 열거나 분석을 실행하십시오.\"}"
            .to_string();
    };
    let parsed = match parse_join_request(&request.body) {
        Ok(parsed) => parsed,
        Err(err) => return api_err(&err),
    };
    if busy {
        return api_err(CLIP_BUSY_ERROR);
    }
    // Index read + up to 500 ffprobes: never under the state lock.
    let plan = match crate::join::plan_with_tools(&case_dir, &parsed.ids, parsed.tolerance, tools) {
        Ok(plan) => plan,
        Err(err) => return api_err(&err),
    };
    let unknown: Vec<&str> = plan
        .blocking
        .iter()
        .filter(|issue| {
            issue.code == "not-indexed" && issue.message.starts_with(JOIN_UNKNOWN_ID_MESSAGE)
        })
        .flat_map(|issue| issue.ids.iter().map(String::as_str))
        .collect();
    if !unknown.is_empty() {
        let shown = unknown
            .iter()
            .take(10)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let more = unknown.len().saturating_sub(10);
        return api_err(&format!(
            "{JOIN_UNKNOWN_ID_MESSAGE}: {shown}{}",
            if more > 0 {
                format!(" 외 {more}개")
            } else {
                String::new()
            }
        ));
    }
    {
        let mut guard = state_lock(state);
        if guard.case_dir.as_deref() == Some(case_dir.as_path()) {
            let total = plan.total_bytes();
            guard.join_plan_bytes.retain(|(id, _)| *id != plan.plan_id);
            guard.join_plan_bytes.push((plan.plan_id.clone(), total));
            let excess = guard.join_plan_bytes.len().saturating_sub(KEPT_JOIN_PLANS);
            guard.join_plan_bytes.drain(..excess);
        }
    }
    serde_json::json!({"ok": true, "plan": plan.to_json()}).to_string()
}

/// `POST /api/join {ids, plan_id, tolerance}`: starts `join-video` as a
/// background job sharing the clip-export list, lock, running cap (4) and
/// protections. The CLI re-plans and refuses (exit 3) when the files
/// changed since the check. Response: `{ok:true, job:{…, kind:"join"},
/// deduped}`; status/cancel through export-clip-status / -cancel.
pub(crate) fn api_join(request: &Request, state: &SharedState) -> String {
    match std::env::current_exe() {
        Ok(exe) => join_with_exe(request, state, &exe),
        Err(err) => api_err(&err.to_string()),
    }
}

/// [`api_join`] parameterised on the CLI executable (tests).
pub(crate) fn join_with_exe(request: &Request, state: &SharedState, exe: &Path) -> String {
    let (case_dir, busy) = {
        let guard = state_lock(state);
        (guard.case_dir.clone(), guard.busy)
    };
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 케이스를 열거나 분석을 실행하십시오.\"}"
            .to_string();
    };
    let parsed = match parse_join_request(&request.body) {
        Ok(parsed) => parsed,
        Err(err) => return api_err(&err),
    };
    let Some(plan_id) = parsed.plan_id.clone() else {
        return api_err("plan_id가 없습니다 — 먼저 검사하십시오.");
    };
    let mut sorted = parsed.ids.clone();
    sorted.sort();
    let key = format!(
        "join\u{1f}{}\u{1f}{}\u{1f}{:.3}\u{1f}{plan_id}",
        case_dir.display(),
        sorted.join("\u{1e}"),
        parsed.tolerance
    );
    if let Some(existing) = find_reusable_clip_job(state, &key) {
        return clip_response(&existing, true);
    }
    if busy {
        return api_err(CLIP_BUSY_ERROR);
    }
    let job = {
        let mut guard = state_lock(state);
        if let Some(existing) = guard
            .clip_jobs
            .iter()
            .find(|job| job.key == key && job.state == "running" && !job.cancel_requested)
            .cloned()
        {
            drop(guard);
            return clip_response(&existing, true);
        }
        // S7: no new job once shutdown was accepted.
        if let Some(refusal) = shutdown_refusal(&guard) {
            return api_err(&refusal);
        }
        if guard.busy {
            return api_err(CLIP_BUSY_ERROR);
        }
        if guard.case_dir.as_deref() != Some(case_dir.as_path()) {
            return api_err("케이스가 바뀌었습니다 — 다시 시도하십시오.");
        }
        if guard
            .clip_jobs
            .iter()
            .filter(|job| job.state == "running")
            .count()
            >= MAX_RUNNING_CLIP_JOBS
        {
            return api_err(
                "동시에 진행 중인 클립 내보내기·이어붙이기가 너무 많습니다 — 하나가 끝난 뒤 다시 시도하십시오.",
            );
        }
        guard.clip_seq += 1;
        let started_unix = crate::util::now_unix().unwrap_or(0);
        let job_id = format!("join-{}", guard.clip_seq);
        // Unique across server restarts, so a crashed run's leftovers never
        // share a tag with a live job.
        let tag = format!("srv-{started_unix}-{}", guard.clip_seq);
        let temps = crate::join::temp_paths(&case_dir, &tag);
        let expected_bytes = guard
            .join_plan_bytes
            .iter()
            .find(|(id, _)| *id == plan_id)
            .map(|(_, bytes)| *bytes);
        let job = ClipJob {
            id: job_id,
            kind: "join",
            key,
            record_id: parsed.ids[0].clone(),
            // The CLI picks the final name; known once the join is logged.
            output: PathBuf::new(),
            rel_path: String::new(),
            started_unix,
            finished_unix: None,
            state: "running",
            sha256: None,
            size_bytes: None,
            error: None,
            cancel_requested: false,
            progress_path: Some(temps.progress.clone()),
            expected_bytes,
            join: Some(JoinJobInfo {
                tag,
                plan_id: plan_id.clone(),
                ids: parsed.ids.clone(),
                temps,
                manifest_rel: None,
                duration_check_ok: None,
                duration_check: None,
            }),
        };
        guard.clip_jobs.push(job.clone());
        prune_finished_clip_jobs(&mut guard.clip_jobs);
        job
    };
    let tag = job.join.as_ref().map(|j| j.tag.clone()).unwrap_or_default();
    let args: Vec<String> = vec![
        "join-video".into(),
        case_dir.to_string_lossy().to_string(),
        "--ids".into(),
        parsed.ids.join(","),
        "--tolerance".into(),
        format!("{}", parsed.tolerance),
        "--plan-id".into(),
        plan_id,
        "--job-tag".into(),
        tag,
    ];
    let worker_state = Arc::clone(state);
    let worker_exe = exe.to_path_buf();
    let job_id = job.id.clone();
    let worker_case = case_dir.clone();
    let spawned = thread::Builder::new()
        .name("ft-join".into())
        .spawn(move || run_join_job(&worker_state, &worker_exe, &job_id, &args, &worker_case));
    if spawned.is_err() {
        update_clip_job(state, &job.id, |job| {
            job.state = "failed";
            job.error = Some("작업 스레드 시작 실패".into());
            job.finished_unix = crate::util::now_unix().ok();
        });
        return api_err("작업 스레드 시작 실패");
    }
    state_lock(state).logs.push(format!(
        "영상 이어붙이기 시작: {}개 ({})",
        parsed.ids.len(),
        job.id
    ));
    clip_response(&job, false)
}

/// A finished, logged join as the server verified it.
struct JoinDone {
    output: PathBuf,
    rel_path: String,
    manifest_rel: String,
    sha256: String,
    size: u64,
    duration_check_ok: Option<bool>,
    duration_check: serde_json::Value,
}

/// Every `join-log.jsonl` entry usable here (missing log → empty). R9 P3:
/// read through `audit::read_lane_rows` — an entry this PC cannot
/// authenticate (keys held) never reports a job's output.
fn read_join_log(case_dir: &Path) -> Result<Vec<serde_json::Value>, String> {
    let path = case_dir
        .join(crate::join::JOINED_DIR)
        .join(crate::join::LOG_FILE);
    crate::audit::read_lane_rows(&path)
        .map(|lane| lane.usable_values())
        .map_err(|err| format!("{} 읽기 실패: {err}", path.display()))
}

/// C13: the log entry THIS job's CLI wrote — matched by its unique
/// `job_tag` (and plan id) only. Plan id + time alone also match another
/// process's join of the same plan (a CLI run, a second workstation),
/// whose output was then reported as this job's.
pub(crate) fn logged_join_entry(
    entries: &[serde_json::Value],
    job_tag: &str,
    plan_id: &str,
) -> Option<serde_json::Value> {
    entries
        .iter()
        .rev()
        .find(|entry| {
            entry.get("event").and_then(|v| v.as_str()) == Some("join-video")
                && entry.get("job_tag").and_then(|v| v.as_str()) == Some(job_tag)
                && entry
                    .get("plan_id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| id.eq_ignore_ascii_case(plan_id))
        })
        .cloned()
}

/// Canonical `path` when it lies inside `case_dir/artifacts/joined`.
fn joined_artifact(case_dir: &Path, path: &Path) -> Option<PathBuf> {
    let root = case_dir.join(crate::join::JOINED_DIR).canonicalize().ok()?;
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        case_dir.join(path)
    };
    let canonical = resolved.canonicalize().ok()?;
    path_is_under(&root, &canonical).then_some(canonical)
}

fn case_relative(case_dir: &Path, path: &Path) -> String {
    let base = case_dir
        .canonicalize()
        .unwrap_or_else(|_| case_dir.to_path_buf());
    path.strip_prefix(&base)
        .or_else(|_| path.strip_prefix(case_dir))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().to_string())
}

/// Plan §1.6: on success the server re-hashes the output and requires it
/// to equal the log entry and the manifest's `output.sha256` (and the
/// manifest bytes to equal the logged `manifest_sha256`).
fn verify_logged_join(case_dir: &Path, entry: &serde_json::Value) -> Result<JoinDone, String> {
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("이어붙이기 로그 항목에 {key}가 없습니다"))
    };
    let output_raw = text("output_path")?;
    let manifest_raw = text("manifest_path")?;
    let logged_sha = text("output_sha256")?.to_ascii_lowercase();
    let logged_manifest_sha = text("manifest_sha256")?.to_ascii_lowercase();
    let output = joined_artifact(case_dir, Path::new(&output_raw)).ok_or_else(|| {
        format!("이어붙인 결과물이 없거나 artifacts/joined 밖에 있습니다: {output_raw}")
    })?;
    let manifest = joined_artifact(case_dir, Path::new(&manifest_raw)).ok_or_else(|| {
        format!("매니페스트가 없거나 artifacts/joined 밖에 있습니다: {manifest_raw}")
    })?;
    let digest = crate::audit::digest_file(&output)
        .map_err(|err| format!("결과물 SHA-256 재계산 실패: {err}"))?;
    let manifest_bytes =
        std::fs::read(&manifest).map_err(|err| format!("매니페스트 읽기 실패: {err}"))?;
    let manifest_value: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|err| format!("매니페스트 JSON 오류: {err}"))?;
    let manifest_sha = manifest_value
        .pointer("/output/sha256")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !digest.eq_ignore_ascii_case(&logged_sha) || digest.to_ascii_lowercase() != manifest_sha {
        return Err(format!(
            "결과물 SHA-256이 기록과 다릅니다 (재계산 {digest}, 로그 {logged_sha}, 매니페스트 {manifest_sha}) — 결과물과 매니페스트를 보존했습니다. 확인하십시오."
        ));
    }
    let manifest_digest = crate::sha256::digest_bytes(&manifest_bytes);
    if !manifest_digest.eq_ignore_ascii_case(&logged_manifest_sha) {
        return Err(format!(
            "매니페스트 SHA-256이 로그와 다릅니다 (재계산 {manifest_digest}, 로그 {logged_manifest_sha}) — 확인하십시오."
        ));
    }
    let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    let duration_check_ok = entry.get("duration_check_ok").and_then(|v| v.as_bool());
    // `{expected, observed, ok}` from the manifest; the log's totals when a
    // manifest lacks it.
    let duration_check = manifest_value
        .get("duration_check")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| {
            serde_json::json!({
                "expected": entry.get("duration_seconds").cloned().unwrap_or_default(),
                "observed": null,
                "ok": duration_check_ok,
            })
        });
    Ok(JoinDone {
        rel_path: case_relative(case_dir, &output),
        manifest_rel: case_relative(case_dir, &manifest),
        output,
        sha256: digest.to_ascii_lowercase(),
        size,
        duration_check_ok,
        duration_check,
    })
}

/// After a failed or cancelled join: removes this job's temp files and the
/// output + manifest the CLI recorded in its own claim file
/// (`.join-<tag>.claim.json`) but never logged (a kill between publish and
/// the log append). B8: nothing is inferred from names, plan ids or times —
/// another job's (or a concurrent CLI join's) output of the same plan is
/// never touched. Returns every removal failure.
fn cleanup_failed_join(case_dir: &Path, info: &JoinJobInfo) -> Vec<String> {
    let mut errors = Vec::new();
    // Read the claim BEFORE the temp files (the claim is one of them) go.
    // C3: the CLI writes it BEFORE the output gets its final name, so a
    // kill mid-publish is covered too.
    let claimed = crate::join::read_join_claim(&info.temps.claim);
    if claimed.is_none() && info.temps.claim.exists() {
        errors.push(format!(
            "이어붙이기 기록 파일을 읽지 못해 게시된 결과물이 남았을 수 있습니다: {}",
            info.temps.claim.display()
        ));
    }
    let progress_tmp = info.temps.progress.with_file_name(format!(
        "{}.tmp",
        info.temps
            .progress
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    for path in info
        .temps
        .all()
        .into_iter()
        .chain([progress_tmp.as_path(), info.temps.lock.as_path()])
    {
        if let Err(err) = crate::join::remove_file_retry(path) {
            errors.push(err);
        }
    }
    let Some(claim) = claimed else {
        return errors;
    };
    // Only existing files inside artifacts/joined that no log entry names
    // (a logged artifact is never deleted from under its record).
    errors.extend(
        crate::join::remove_unlogged_claimed(case_dir, &claim)
            .into_iter()
            .filter_map(|(_, result)| result.err()),
    );
    errors
}
// Worker: stale `.join-*` cleanup → `join-video` (killed as a tree on a
/// per-job cancel) → verify the logged output, or clean up everything the
/// job created. Removal failures are surfaced, never swallowed.
fn run_join_job(state: &SharedState, exe: &Path, id: &str, args: &[String], case_dir: &Path) {
    let Some(info) = state_lock(state)
        .clip_jobs
        .iter()
        .find(|job| job.id == id)
        .and_then(|job| job.join.clone())
    else {
        return;
    };
    {
        let _gate = JOIN_TEMP_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let active: Vec<String> = state_lock(state)
            .clip_jobs
            .iter()
            .filter(|job| job.state == "running")
            .filter_map(|job| job.join.as_ref().map(|join| join.tag.clone()))
            .collect();
        let removed = crate::join::cleanup_stale_temp_files(case_dir, &active);
        if !removed.is_empty() {
            let mut guard = state_lock(state);
            for (path, result) in removed {
                guard.logs.push(match result {
                    Ok(()) => format!(
                        "이전 이어붙이기의 잔여 임시 파일을 지웠습니다: {}",
                        path.display()
                    ),
                    Err(err) => format!("경고: {err}"),
                });
            }
        }
    }
    let cancelled = || {
        state_lock(state)
            .clip_jobs
            .iter()
            .find(|job| job.id == id)
            .is_none_or(|job| job.cancel_requested)
    };
    let result = run_step_cancellable(exe, args, &cancelled, state);
    let was_cancelled = cancelled();
    // The log entry is the authority on whether the join completed: a
    // cancel that lost the race with completion still yields a logged,
    // complete output, which must not be deleted from under its record.
    // C13: only the entry carrying this job's tag counts.
    let logged = read_join_log(case_dir)
        .map(|entries| logged_join_entry(&entries, &info.tag, &info.plan_id));
    // `Err((message, keep_files))`: a logged join that fails re-verification
    // keeps its output and manifest for review (deleting them would orphan
    // the chained record); every other failure removes what the job made.
    let outcome: Result<JoinDone, (String, bool)> = match (&result, logged) {
        (_, Ok(Some(entry))) => verify_logged_join(case_dir, &entry).map_err(|err| (err, true)),
        (Ok(_), Ok(None)) => Err((
            "join-video가 성공을 보고했지만 이어붙이기 로그에 기록이 없습니다".to_string(),
            false,
        )),
        (_, Err(err)) => Err((err, false)),
        (Err(err), Ok(None)) => {
            if err.contains("(exit Some(3))") || err.contains(crate::join::PLAN_CHANGED_MESSAGE) {
                Err((crate::join::PLAN_CHANGED_MESSAGE.to_string(), false))
            } else {
                Err((err.clone(), false))
            }
        }
    };
    // Temp files go in every case (the CLI normally removes them itself;
    // a killed CLI cannot). Published-but-unlogged outputs go only when
    // nothing was logged for this job.
    let cleanup_errors = match &outcome {
        Ok(_) | Err((_, true)) => {
            let mut errors = Vec::new();
            for path in info
                .temps
                .all()
                .into_iter()
                .chain(std::iter::once(info.temps.lock.as_path()))
            {
                if let Err(err) = crate::join::remove_file_retry(path) {
                    errors.push(err);
                }
            }
            errors
        }
        Err((_, false)) => cleanup_failed_join(case_dir, &info),
    };
    let finished = crate::util::now_unix().ok();
    let mut guard = state_lock(state);
    let mut log_lines = Vec::new();
    if let Some(job) = guard.clip_jobs.iter_mut().find(|job| job.id == id) {
        job.finished_unix = finished;
        match outcome {
            Ok(done) => {
                job.state = "done";
                job.output = done.output;
                job.rel_path = done.rel_path;
                job.sha256 = Some(done.sha256.clone());
                job.size_bytes = Some(done.size);
                if let Some(join) = job.join.as_mut() {
                    join.manifest_rel = Some(done.manifest_rel);
                    join.duration_check_ok = done.duration_check_ok;
                    join.duration_check = Some(done.duration_check);
                }
                log_lines.push(format!(
                    "영상 이어붙이기 완료: {} · SHA-256 {}",
                    job.rel_path, done.sha256
                ));
                if done.duration_check_ok == Some(false) {
                    log_lines.push(format!(
                        "경고: 이어붙인 영상 길이가 원본 합계와 다릅니다 — 매니페스트를 확인하십시오 ({})",
                        job.rel_path
                    ));
                }
                if !cleanup_errors.is_empty() {
                    log_lines.push(format!("경고: {}", cleanup_errors.join("; ")));
                }
            }
            Err((err, kept)) => {
                let cleanup_note = (!cleanup_errors.is_empty()).then(|| cleanup_errors.join("; "));
                if was_cancelled && cleanup_note.is_none() && !kept {
                    job.state = "cancelled";
                    job.error =
                        Some("이어붙이기를 중단했습니다 — 만들던 파일은 지웠습니다.".into());
                    log_lines.push(format!("영상 이어붙이기 중단 ({})", job.id));
                } else {
                    // A leftover that could not be removed is a failure even
                    // after a cancel: the examiner must see the path.
                    let message = match (&cleanup_note, was_cancelled) {
                        (Some(note), true) => format!("이어붙이기를 중단했지만 {note}"),
                        (Some(note), false) => format!("{err}; {note}"),
                        (None, _) => err,
                    };
                    job.state = "failed";
                    log_lines.push(format!(
                        "오류: 영상 이어붙이기 실패 ({}): {message}",
                        job.id
                    ));
                    job.error = Some(message);
                }
            }
        }
    }
    guard.logs.extend(log_lines);
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
        // C12: exact index id, resolved through the index only.
        if let Err(err) = check_index_video_id(&id) {
            return api_err(&err);
        }
        id.clone()
    } else {
        let alt = body_value(&request.body, "path").unwrap_or_default();
        let candidate = PathBuf::from(alt.trim());
        let roots = state_lock(state).media_roots.clone();
        // Item 8: a UNC/device path never reaches canonicalize.
        let allowed = super::http::reject_remote_path(&alt, &roots).is_ok()
            && candidate
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
        // C12: exact index id, resolved through the index only.
        if let Err(err) = check_index_video_id(&id) {
            return api_err(&err);
        }
        id.clone()
    } else {
        let alt = body_value(&request.body, "path").unwrap_or_default();
        let candidate = PathBuf::from(alt.trim());
        let roots = state_lock(state).media_roots.clone();
        // Item 8: a UNC/device path never reaches canonicalize.
        let allowed = super::http::reject_remote_path(&alt, &roots).is_ok()
            && candidate
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
    // D5: proxies are named `proxy_<record id>_<hash8>.mp4` (path-free);
    // the shared lookup prefers those and still finds pre-D5
    // `<sanitize(selector)>_proxy_*` names.
    let find_existing = || crate::artifacts::find_existing_proxy(&case_dir, &selector);
    let proxy = match find_existing() {
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
            match find_existing() {
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
    let tool = body_value(&request.body, "tool").unwrap_or_default();
    if tool == "deepfake"
        && let Some(refusal) = super::tools::deepfake_missing_refusal()
    {
        return refusal;
    }
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
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
            if let Err(reason) = super::http::reject_remote_path(&extra, &[]) {
                return api_err(reason);
            }
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
///
/// R3: the batch can be long (up to 5000 copies + hashes), so it is
/// cancellable through `/api/cancel`: the flag is checked between items
/// (a copy in progress is finished, never torn), and a cancelled export
/// still writes its full manifest — every selected item appears, the ones
/// never reached as `not-exported(cancelled)` — with `cancelled`,
/// `items_done` and `items_total` in `manifest-audit.json`, the README and
/// the audit event. The reply is then `ok:false, cancelled:true`.
pub(crate) fn api_export_selected(request: &Request, state: &SharedState) -> String {
    api_export_selected_with(request, state, &|_| {})
}

/// Skip reason recorded for selected items a cancel stopped the batch
/// before (R3).
pub(crate) const EXPORT_CANCELLED_REASON: &str = "not-exported(cancelled)";

/// [`api_export_selected`] with a hook run before each item is exported
/// (tests use it to cancel mid-batch deterministically).
pub(crate) fn api_export_selected_with(
    request: &Request,
    state: &SharedState,
    before_item: &(dyn Fn(usize) + Sync),
) -> String {
    if state_lock(state).case_dir.is_none() {
        return "{\"ok\":false,\"error\":\"열린 케이스가 없습니다.\"}".to_string();
    }
    // S1: a write op for the whole batch (copies, hashes, manifest), so
    // shutdown / finalize / open-case cannot tear the export bundle.
    let (write_op, case_dir) = match try_begin_write_op(state) {
        Ok(op) => op,
        Err(err) => return api_err(&err),
    };
    let roots = state_lock(state).media_roots.clone();
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
    sweep_dead_export_parts(&case_dir.join("exports"));
    let export_dir =
        crate::util::unique_dir(&case_dir.join("exports").join(format!("selection-{stamp}")));
    if let Err(err) = std::fs::create_dir_all(&export_dir) {
        return format!(
            "{{\"ok\":false,\"error\":{}}}",
            json_string(&format!("내보내기 폴더 생성 실패: {err}"))
        );
    }
    // C4: the bundle says "in progress" from its first byte; only the
    // finished manifest replaces that. A process killed mid-export leaves
    // a folder that plainly reads as incomplete (and the forced-exit path
    // stamps it "중단됨", see [`mark_abandoned_exports`]).
    let _in_progress = match begin_export_bundle(&export_dir, stamp, items.len()) {
        Ok(registration) => registration,
        Err(err) => return api_err(&err),
    };
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
        let cancel = write_op.cancel_flag();
        thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(move || {
                    loop {
                        // R3: checked before taking the next item.
                        if cancel.load(Ordering::SeqCst) {
                            break;
                        }
                        let index = next_index.fetch_add(1, Ordering::SeqCst);
                        let Some(item) = items.get(index) else {
                            break;
                        };
                        before_item(index);
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
    let items_total = items.len();
    let items_done = outcomes.iter().filter(|outcome| outcome.is_some()).count();
    // Partial only when items were actually left out; a cancel that
    // arrived after the last item changed nothing.
    let cancelled = items_done < items_total;
    for (item, outcome) in items.iter().zip(outcomes) {
        // R3: an item the cancel stopped the batch before stays in the
        // manifest — a selection is never silently shortened.
        let outcome = outcome.unwrap_or_else(|| ExportOutcome {
            skip_reason: EXPORT_CANCELLED_REASON.to_string(),
            ..ExportOutcome::default()
        });
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
            "{{\"event\":\"selection-export\",\"export_dir\":{},\"items\":{items_total},\"items_done\":{items_done},\"cancelled\":{cancelled},\"copied\":{copied},\"skipped\":{skipped},\"unix\":{stamp}}}",
            json_string(&export_dir.to_string_lossy()),
        ),
    );
    // R13-S14: the final manifests replace the in-progress ones
    // atomically (R12 used a plain write — a crash left a torn file).
    let write = |name: &str, contents: String| -> Result<(), String> {
        crate::util::write_text_atomic(&export_dir.join(name), &contents)
            .map_err(|err| format!("manifest write failed for {name}: {err}"))
    };
    let head = audit::chain_head(&audit_log)
        .ok()
        .flatten()
        .unwrap_or_default();
    let status_line = if cancelled {
        format!(
            "상태: 중단됨 — 선택 {items_total}개 중 {items_done}개만 처리했습니다. 나머지는 manifest에 {EXPORT_CANCELLED_REASON}로 기록되어 있으며 이 폴더에 사본이 없습니다."
        )
    } else {
        "상태: 완료 — 선택한 모든 항목을 처리했습니다.".to_string()
    };
    let result = write("manifest.csv", format!("\u{feff}{}\n", csv_rows.join("\n")))
        .and_then(|_| write("manifest.jsonl", jsonl_rows.join("\n") + "\n"))
        .and_then(|_| {
            write(
                "manifest-audit.json",
                format!(
                    "{{\"state\":{},\"audit_log\":{},\"chain_head_sha256\":{},\"audit_logged\":{},\"cancelled\":{cancelled},\"items_done\":{items_done},\"items_total\":{items_total},\"unix\":{stamp}}}\n",
                    json_string(if cancelled { "cancelled" } else { "complete" }),
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
                     {status_line}\n\
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
        // Best effort: a folder whose manifest could not be written says so
        // itself, so nobody mistakes the bare copies for a complete bundle.
        let _ = std::fs::write(
            export_dir.join("MANIFEST-WRITE-FAILED.txt"),
            format!(
                "이 선별 묶음의 manifest를 쓰지 못했습니다 — 묶음으로 사용하지 마십시오.\n{err}\n"
            ),
        );
        return serde_json::json!({"ok": false, "error": err}).to_string();
    }
    // The bundle is now final (complete or marked cancelled): the
    // in-progress note goes.
    if let Err(err) = crate::join::remove_file_retry(&export_dir.join(EXPORT_IN_PROGRESS_FILE)) {
        return serde_json::json!({"ok": false, "error": err}).to_string();
    }
    if cancelled {
        let audit_note = match &audit_result {
            Ok(()) => String::new(),
            Err(err) => format!(" 감사 로그 기록도 실패했습니다: {err}"),
        };
        return serde_json::json!({
            "ok": false,
            "cancelled": true,
            "partial": true,
            "unlogged": audit_result.is_err(),
            "error": format!(
                "선택 내보내기를 중단했습니다 — {items_total}개 중 {items_done}개 처리. 폴더에 부분 묶음과 manifest(중단 표시)가 남아 있습니다.{audit_note}"
            ),
            "export_dir": export_dir.to_string_lossy(),
            "items_done": items_done,
            "items_total": items_total,
            "copied": copied,
            "skipped": skipped,
        })
        .to_string();
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
    // Item 8: refuse UNC/device paths before canonicalize (`roots` are
    // canonical, so a UNC root keeps its `\\?\UNC\` spelling - handled).
    if super::http::reject_remote_path(&path_text, roots).is_err() {
        outcome.skip_reason = "remote-path-refused".to_string();
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
    match copy_hashed_unique(&canonical, export_dir, &base_name) {
        Ok(copy) => {
            outcome.exported = copy
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            outcome.copy_hash = copy.sha256;
            outcome.size = copy.size.to_string();
        }
        // C5: a failed copy or hash is a failed item — never a "copied"
        // row with an empty hash, never a torn file under the final name.
        Err(err) => outcome.skip_reason = err,
    }
    outcome
}

/// One finished, hashed export copy.
pub(crate) struct ExportCopy {
    pub(crate) path: PathBuf,
    pub(crate) sha256: String,
    pub(crate) size: u64,
}

/// Distinguishes the `.part` names of parallel workers in one process.
static EXPORT_PART_SEQ: AtomicUsize = AtomicUsize::new(0);

/// C4/C5: copies `source` into a pid-tagged `.part` in `dir`, syncs and
/// hashes it, and only then publishes it under the first free
/// `base_name[-N]` (no clobbering: the batch runs on parallel workers).
/// Any failure removes the `.part`; the `Err` text is the item's skip
/// reason (`copy-failed: …` / `hash-failed: …`).
pub(crate) fn copy_hashed_unique(
    source: &Path,
    dir: &Path,
    base_name: &str,
) -> Result<ExportCopy, String> {
    let mut reader =
        std::fs::File::open(source).map_err(|err| format!("copy-failed: open source: {err}"))?;
    let seq = EXPORT_PART_SEQ.fetch_add(1, Ordering::SeqCst);
    let part =
        crate::join::pid_part_path(&dir.join(format!("{base_name}.{seq}")), std::process::id());
    let remove_part = || {
        let _ = crate::join::remove_file_retry(&part);
    };
    let copied = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .and_then(|mut target| {
            std::io::copy(&mut reader, &mut target)?;
            target.sync_all()?;
            if let Ok(meta) = source.metadata() {
                let _ = target.set_permissions(writable_copy_permissions(meta.permissions()));
            }
            Ok(())
        });
    if let Err(err) = copied {
        remove_part();
        return Err(format!("copy-failed: {err}"));
    }
    let sha256 = match audit::digest_file(&part) {
        Ok(digest) if !digest.is_empty() => digest,
        Ok(_) => {
            remove_part();
            return Err("hash-failed: empty digest".to_string());
        }
        Err(err) => {
            remove_part();
            return Err(format!("hash-failed: {err}"));
        }
    };
    let size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
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
        match crate::join::publish_part_as(&part, &candidate) {
            Ok(true) => {
                return Ok(ExportCopy {
                    path: candidate,
                    sha256,
                    size,
                });
            }
            Ok(false) => continue,
            Err(err) => {
                remove_part();
                return Err(format!("copy-failed: {err}"));
            }
        }
    }
    remove_part();
    Err("copy-failed: could not allocate a unique export filename".to_string())
}

/// [`copy_hashed_unique`] returning only the published path (tests).
#[cfg(test)]
pub(crate) fn copy_unique(source: &Path, dir: &Path, base_name: &str) -> Result<PathBuf, String> {
    copy_hashed_unique(source, dir, base_name).map(|copy| copy.path)
}

/// C4: note file present while a selection export is being written.
pub(crate) const EXPORT_IN_PROGRESS_FILE: &str = "EXPORT-IN-PROGRESS.txt";
/// C4: note file written when an in-progress export was abandoned.
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
pub(crate) const EXPORT_INCOMPLETE_FILE: &str = "EXPORT-INCOMPLETE.txt";

/// Selection exports being written by this process (C4).
static EXPORTS_IN_PROGRESS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn exports_in_progress() -> MutexGuard<'static, Vec<PathBuf>> {
    EXPORTS_IN_PROGRESS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Deregisters an export bundle when the export returns (any path).
pub(crate) struct ExportBundleRegistration {
    dir: PathBuf,
}

impl Drop for ExportBundleRegistration {
    fn drop(&mut self) {
        let dir = self.dir.clone();
        exports_in_progress().retain(|entry| *entry != dir);
    }
}

/// C4: marks `export_dir` "in progress" on disk (an in-progress
/// `manifest-audit.json` plus a note file) and registers it for
/// [`mark_abandoned_exports`].
pub(crate) fn begin_export_bundle(
    export_dir: &Path,
    stamp: u64,
    items_total: usize,
) -> Result<ExportBundleRegistration, String> {
    let note = format!(
        "FrameTrace 선별 증거 묶음 — 작성 중\n\n이 폴더는 아직 완성되지 않았습니다 (생성 시작 unix {stamp}, 항목 {items_total}개).\n이 파일이 남아 있다면 내보내기가 끝나지 못한 것이므로 묶음으로 사용하지 마십시오.\n"
    );
    // R5-S14: atomic (temp + rename) so a crash never leaves a torn file.
    crate::util::write_text_atomic(&export_dir.join(EXPORT_IN_PROGRESS_FILE), &note)
        .and_then(|_| {
            crate::util::write_text_atomic(
                &export_dir.join("manifest-audit.json"),
                &format!(
                    "{{\"state\":\"in-progress\",\"items_total\":{items_total},\"unix\":{stamp}}}\n"
                ),
            )
        })
        .map_err(|err| format!("내보내기 시작 기록 실패: {err}"))?;
    exports_in_progress().push(export_dir.to_path_buf());
    Ok(ExportBundleRegistration {
        dir: export_dir.to_path_buf(),
    })
}

/// C4: the forced-exit path (the shell gave up waiting for running
/// writes): stamps every export bundle this process is still writing as
/// "중단됨" — an incomplete-note file and an `aborted` manifest-audit —
/// so the folder can never be mistaken for a finished bundle. Returns how
/// many were marked.
/// R13-S14: the C4 and S14 tests both register an export and stamp EVERY
/// registered one; run in parallel they stamped each other's folders.
#[cfg(test)]
pub(crate) static ABANDONED_EXPORTS_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg_attr(not(any(test, windows)), allow(dead_code))]
pub(crate) fn mark_abandoned_exports() -> usize {
    let dirs = exports_in_progress().clone();
    let mut marked = 0;
    for dir in dirs {
        let note = "FrameTrace 선별 증거 묶음 — 중단됨\n\n프로그램이 종료되면서 이 내보내기는 끝나지 못했습니다.\n일부 파일만 들어 있을 수 있으며 manifest가 없거나 불완전합니다. 묶음으로 사용하지 말고 다시 내보내십시오.\n";
        // R5-S14: the "aborted" manifest replaces the in-progress one
        // atomically — a process killed mid-write (this runs on the forced
        // exit path) leaves the old or the new file, never a torn one.
        let written = crate::util::write_text_atomic(&dir.join(EXPORT_INCOMPLETE_FILE), note)
            .and_then(|_| {
                crate::util::write_text_atomic(
                    &dir.join("manifest-audit.json"),
                    "{\"state\":\"aborted\",\"reason\":\"process-exit\"}\n",
                )
            });
        if written.is_ok() {
            marked += 1;
        } else {
            eprintln!(
                "frametrace: could not mark abandoned export {}",
                dir.display()
            );
        }
    }
    marked
}

/// Removes in-progress copy `.part` files that dead exports left in
/// `exports/selection-*` folders this process is not writing (C4).
fn sweep_dead_export_parts(exports: &Path) {
    let Ok(entries) = std::fs::read_dir(exports) else {
        return;
    };
    let live = exports_in_progress().clone();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !live.contains(&path) {
            for (path, result) in crate::join::sweep_stale_parts(&path) {
                if let Err(err) = result {
                    eprintln!("frametrace: {err} ({})", path.display());
                }
            }
        }
    }
}

/// Permissions for an exported copy: the source's, minus read-only. Evidence
/// on write-protected media (or marked read-only on Windows) would otherwise
/// hand the examiner copies they cannot later move, re-hash into a package
/// or delete without an attribute fight (Windows review, Low).
pub(crate) fn writable_copy_permissions(
    mut permissions: std::fs::Permissions,
) -> std::fs::Permissions {
    if permissions.readonly() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Owner write only - never widen group/other access.
            permissions.set_mode(permissions.mode() | 0o200);
        }
        // Not unix: only the read-only attribute exists (no mode bits to
        // widen), so the lint's world-writable concern does not apply.
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        {
            permissions.set_readonly(false);
        }
    }
    permissions
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

/// The most recently modified finished package in `reports/`.
///
/// R10-S2: a package of an incomplete finalize
/// (`crate::package::finalize_incomplete_reason` — renamed
/// `…_INCOMPLETE` or marked beside) is never "the" package. R10-S3:
/// finalize no longer guesses its package this way (it reads the step's
/// `output:` line, [`published_package_dir`]); tests still use it.
#[cfg(test)]
pub(crate) fn newest_package_dir(case_dir: &Path) -> Option<PathBuf> {
    let entries = case_dir.join("reports").read_dir().ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("package_") || !entry.path().is_dir() {
            continue;
        }
        if crate::package::finalize_incomplete_reason(&entry.path()).is_some() {
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

/// R10-S2: every package folder in `reports/` that an incomplete finalize
/// left (`crate::package::finalize_incomplete_reason`), sorted by path.
/// Never deleted by FrameTrace — the examiner decides.
pub(crate) fn incomplete_package_dirs(case_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = case_dir.join("reports").read_dir() else {
        return Vec::new();
    };
    // R11-S4: the entry's own type (no link followed — R10 called
    // `is_dir()` on every `package_*` entry at each /api/status poll, so a
    // junction or symlink to a slow or offline share was traversed every
    // poll); links and other reparse points are skipped.
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("package_"))
        .filter(is_plain_dir_entry)
        .map(|entry| entry.path())
        .filter(|path| crate::package::finalize_incomplete_reason(path).is_some())
        .collect();
    found.sort();
    found
}

/// R11-S4: a directory entry that is a real directory — not a symlink, a
/// junction or any other reparse point — judged from the entry itself
/// (`DirEntry::file_type` / its metadata do not follow links).
pub(crate) fn is_plain_dir_entry(entry: &std::fs::DirEntry) -> bool {
    let Ok(file_type) = entry.file_type() else {
        return false;
    };
    // R12-S9: `is_symlink()` alone. On Windows it is true for the
    // name-surrogate reparse points — symbolic links AND junctions (mount
    // points) — and false for the others, such as OneDrive / cloud-files
    // placeholders. R11 also excluded every FILE_ATTRIBUTE_REPARSE_POINT
    // entry, which dropped real package folders in a OneDrive-synced case
    // from the listing.
    file_type.is_dir() && !file_type.is_symlink()
}

/// R10-S3: the package a `package-case` step published, from its own
/// `output: <dir>` line — never guessed from folder times (another
/// package-case, a copy or a restore can make a different folder the
/// newest). Only a `reports/package_*` folder of this case that exists is
/// accepted.
pub(crate) fn published_package_dir(case_dir: &Path, step_output: &str) -> Option<PathBuf> {
    let reports = case_dir.join("reports");
    step_output
        .lines()
        .find_map(|line| line.strip_prefix("output: "))
        .map(|text| PathBuf::from(text.trim_end()))
        .filter(|path| {
            path.parent() == Some(reports.as_path())
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("package_"))
                && path.is_dir()
        })
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

#[cfg(test)]
mod copy_permission_tests {
    use super::*;

    /// Windows review (Low): a read-only source must not yield a read-only
    /// export copy; the copy's bytes are unchanged.
    #[test]
    fn copy_of_read_only_source_is_writable() {
        let dir = std::env::temp_dir().join(format!("ft-copy-ro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("src.mp4");
        std::fs::write(&source, b"evidence").unwrap();
        let mut perms = std::fs::metadata(&source).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&source, perms).unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let copy = copy_unique(&source, &out, "copy.mp4").unwrap();
        assert!(
            !std::fs::metadata(&copy).unwrap().permissions().readonly(),
            "export copy must not inherit read-only"
        );
        assert_eq!(std::fs::read(&copy).unwrap(), b"evidence");
        // The source keeps its attribute (copying never touches evidence).
        assert!(std::fs::metadata(&source).unwrap().permissions().readonly());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode & 0o022, 0, "group/other write must not be added");
        }
        // Clean-up needs the read-only bit cleared on Windows.
        let mut perms = std::fs::metadata(&source).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(&source, perms);
        let _ = std::fs::remove_dir_all(dir);
    }
}
/// C10: job warning for a step whose output readers had to be abandoned.
pub(crate) fn reader_abandoned_warning(args: &[String], cap: Duration) -> String {
    format!(
        "{} 단계가 끝난 뒤에도 출력 파이프가 {}초 안에 닫히지 않았습니다 — 하위 프로세스가 남아 있을 수 있어 출력 일부를 버리고 계속합니다. 작업 관리자에서 남은 ffmpeg 등을 확인하십시오.",
        args.first().map(String::as_str).unwrap_or("command"),
        cap.as_secs()
    )
}

/// Every directory a killed step may have left pid-tagged `.part` files
/// in (C1/C2/C4): the step's case directories plus the parent of an
/// explicit `--output`.
pub(crate) fn step_part_dirs(args: &[String]) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(case) = args.get(1) {
        let case = std::path::Path::new(case);
        for rel in [
            "evidence/images",
            "artifacts/recovered/filesystem",
            "artifacts/clips",
            "reports",
            "review/thumbs",
        ] {
            dirs.push(case.join(rel));
        }
    }
    if let Some(output) = args
        .iter()
        .position(|arg| arg == "--output")
        .and_then(|index| args.get(index + 1))
        && let Some(parent) = std::path::Path::new(output).parent()
    {
        dirs.push(parent.to_path_buf());
    }
    dirs
}

/// C1/C2/C4: after the server killed a step (cancel, shutdown), removes
/// the in-progress `.part` files and directories that step's process
/// (`pid`) left, plus empty `package_*` reservations in `reports/`.
/// Returns one log line per removal (`Ok`) or failure (`Err`).
pub(crate) fn sweep_killed_step(args: &[String], pid: u32) -> Vec<Result<String, String>> {
    let mut out = Vec::new();
    // R5-S14: the killed package-case's own reservations, read off its
    // work folders (`.package_<ts>.ft<pid>.part` reserves `package_<ts>`)
    // BEFORE those are swept. Any other empty `package_*` folder may be a
    // live package-case's reservation (a CLI run, another workstation) and
    // is left alone.
    let mut own_reservations: Vec<PathBuf> = Vec::new();
    if args.first().map(String::as_str) == Some("package-case")
        && let Some(case) = args.get(1)
    {
        let reports = std::path::Path::new(case).join("reports");
        if let Ok(entries) = std::fs::read_dir(&reports) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if crate::join::is_pid_part_name(&name, Some(pid))
                    && let Some(reserved) = name
                        .strip_prefix('.')
                        .and_then(|rest| rest.split_once(&format!(".ft{pid}.part")))
                        .map(|(reserved, _)| reserved.to_string())
                    && reserved.starts_with("package_")
                {
                    own_reservations.push(reports.join(reserved));
                }
            }
        }
    }
    for dir in step_part_dirs(args) {
        for (path, result) in crate::join::sweep_pid_parts(&dir, pid) {
            out.push(match result {
                Ok(()) => Ok(format!(
                    "중단된 단계가 남긴 임시 파일을 지웠습니다: {}",
                    path.display()
                )),
                Err(err) => Err(err),
            });
        }
    }
    for reserved in own_reservations {
        // remove_dir only removes an EMPTY directory: a finished package
        // is never touched. R7-S5: and only a reservation whose marker
        // names the killed pid (or that is still empty: killed before
        // the marker was written) — the name a work folder carries may be
        // another package-case's live reservation it lost the race for.
        let removed = match crate::join::reservation_owner(&reserved) {
            Some(Some(owner)) => owner == pid && crate::join::release_reservation(&reserved, pid),
            Some(None) => std::fs::remove_dir(&reserved).is_ok(),
            None => false,
        };
        if removed {
            out.push(Ok(format!(
                "중단된 패키지 생성의 빈 폴더를 지웠습니다: {}",
                reserved.display()
            )));
        }
    }
    out
}
