//! Pipeline-driving API handlers: scan/E01/folder pipelines, recover, carve, finalize, marks.

use super::*;

pub(crate) fn api_start(request: &Request, state: &SharedState) -> String {
    let e01_direct = body_value(&request.body, "e01_direct").as_deref() == Some("true");
    let input_kind = match body_value(&request.body, "input_kind").as_deref() {
        Some("e01") if e01_direct => InputKind::E01Direct,
        Some("e01") => InputKind::E01,
        _ => InputKind::Folder,
    };
    let source_text = match body_value(&request.body, "source_path") {
        Some(path) if !path.trim().is_empty() => path.trim().to_string(),
        _ => {
            return "{\"ok\":false,\"error\":\"증거 소스 경로를 입력하십시오.\"}".to_string();
        }
    };
    // Item 8 policy (see http::reject_remote_path): a typed UNC share is
    // honoured on this nonce-gated POST, device-namespace paths are not.
    // R20-S1/S3 (P34): both paths are normalized and checked again on the
    // normalized text (`bind_path`) — before `exists()` touches the
    // filesystem — so the bound case compares equal to the same folder
    // bound by /api/open-case, `--case` or the restored session.
    let source_path =
        match super::state::bind_path(Path::new(&source_text), super::http::reject_device_path) {
            Ok(path) => path,
            Err(reason) => return api_err(reason),
        };
    let typed_case_dir = match body_value(&request.body, "case_dir") {
        Some(dir) if !dir.trim().is_empty() => {
            match super::state::bind_path(Path::new(dir.trim()), super::http::reject_device_path) {
                Ok(dir) => Some(dir),
                Err(reason) => return api_err(reason),
            }
        }
        _ => None,
    };
    if !source_path.exists() {
        return format!(
            "{{\"ok\":false,\"error\":{}}}",
            json_string(&format!(
                "소스 경로가 존재하지 않습니다: {}",
                source_path.display()
            ))
        );
    }
    if matches!(input_kind, InputKind::E01 | InputKind::E01Direct) {
        match super::system::tool_probe("ewfinfo", "-V") {
            super::system::ToolProbe::Present => {}
            // R6: a slow first start is not "not installed".
            super::system::ToolProbe::TimedOut => {
                return api_err(&format!(
                    "ewfinfo 버전 확인이 {}초 안에 끝나지 않았습니다 — 도구가 없는 것이 아니라 응답이 늦습니다(백신 검사 등). 잠시 후 다시 시도하거나 FRAMETRACE_TOOL_PROBE_TIMEOUT(초)을 늘리십시오.",
                    crate::util::version_probe_timeout_secs()
                ));
            }
            _ => return "{\"ok\":false,\"error\":\"E01 처리에는 libewf 도구(ewfinfo/ewfverify/ewfexport)가 필요합니다. 도구를 설치한 뒤 다시 시도하십시오.\"}".to_string(),
        }
    }
    let with_hash = body_value(&request.body, "with_hash").as_deref() == Some("true");
    let with_ffprobe = body_value(&request.body, "with_ffprobe").as_deref() == Some("true");
    let with_deepfake = body_value(&request.body, "with_deepfake").as_deref() == Some("true");
    // Plan §2.2: measuring sound needs ffprobe to pick the targets, so the
    // option only takes effect together with with_ffprobe (default off).
    let with_audio = body_value(&request.body, "with_audio").as_deref() == Some("true");
    let skip_e01_verify = body_value(&request.body, "skip_e01_verify").as_deref() == Some("true");
    let (case_dir, case_dir_warning) = match typed_case_dir {
        Some(dir) => (dir, None),
        None => {
            let (dir, warning) = default_case_dir_checked();
            match super::state::bind_path(&dir, super::http::reject_device_path) {
                Ok(dir) => (dir, warning),
                Err(reason) => return api_err(reason),
            }
        }
    };
    let job = PipelineJob {
        kind: input_kind,
        case_dir: case_dir.clone(),
        source_path: source_path.clone(),
        with_hash,
        with_ffprobe,
        with_deepfake,
        with_audio,
        skip_e01_verify,
        e01_direct,
    };
    // Stats the source (canonicalize): done before the state lock.
    let volume_root = input_kind == InputKind::Folder && source_is_volume_root(&source_path);
    // P2: the pipeline's `busy` is a lease the worker thread owns, so a
    // panicking worker can never leave the workstation "busy" for good.
    let lease = {
        let mut guard = state_lock(state);
        if guard.busy {
            return "{\"ok\":false,\"error\":\"이미 분석이 진행 중입니다.\"}".to_string();
        }
        // S4: a new analysis rebinds the case; a clip export / join or a
        // write op still writing into the current one must finish first.
        if let Some(refusal) = side_work_refusal(&guard, "새 분석을 시작하십시오") {
            return api_err(&refusal);
        }
        // S7 (shutdown requested) is refused inside the lease.
        let lease = match lease_busy_locked(&mut guard, state) {
            Ok(lease) => lease,
            Err(err) => return api_err(&err),
        };
        begin_pipeline_locked(&mut guard, &job, volume_root, case_dir_warning);
        lease
    };
    let epoch = lease.epoch();
    save_session(
        &case_dir,
        (input_kind == InputKind::Folder).then_some(source_path.as_path()),
    );
    record_recent_case(&case_dir, Some(source_path.as_path()));
    let worker_state = Arc::clone(state);
    let spawned = thread::Builder::new()
        .name("ft-pipeline".into())
        .spawn(move || {
            // Held for the whole run: the pipeline clears `busy` itself in
            // its final state update; if it unwinds instead, dropping the
            // lease frees `busy` and records the failure.
            let _lease = lease;
            run_pipeline(worker_state, job);
        });
    if spawned.is_err() {
        // The closure (and with it the lease) was dropped: `busy` is free.
        // Only reset the phase while no newer job took `busy` since.
        let mut guard = state_lock(state);
        if guard.busy_epoch == epoch {
            guard.phase = "idle";
        }
        return "{\"ok\":false,\"error\":\"작업 스레드 시작 실패\"}".to_string();
    }
    "{\"ok\":true}".to_string()
}

/// The state reset every pipeline run starts with (under the state lock,
/// `busy` already leased): shared by `/api/start` and `/api/reimport-raw`.
fn begin_pipeline_locked(
    guard: &mut JobState,
    job: &PipelineJob,
    volume_root: bool,
    case_dir_warning: Option<String>,
) {
    guard.phase = "running";
    guard.restored = false;
    guard.warnings = Vec::new();
    guard.enrichment = None;
    guard.steps = [StepStatus::Pending; 5];
    guard.step_names = job.kind.step_names().to_vec();
    guard.logs = Vec::new();
    guard.error = None;
    guard.package_dir = None;
    guard.raw_reimport = None;
    if guard.case_dir.as_deref() != Some(job.case_dir.as_path()) {
        // R11-S5/S6: a finalize's leftovers belong to its own case (the
        // pipeline renders no report, so the same case keeps them).
        guard.report_stale = false;
        guard.finalize_incomplete = None;
        guard.finalize_recovered = None;
    }
    guard.case_dir = Some(job.case_dir.clone());
    guard.case_job = None;
    guard.media_roots = start_media_roots(job.kind, &job.case_dir, &job.source_path);
    // SEC-06: /media serves anything under a media root. A folder
    // source at a volume root (E:\ of a dashcam card) is legitimate,
    // but then the whole volume is reviewable — say so, never silently.
    if volume_root {
        let message = format!(
            "소스가 볼륨 루트입니다 ({}) — 이 볼륨 전체가 검토 범위(/media)로 열립니다. 증거 폴더만 지정하는 것을 권장합니다.",
            job.source_path.display()
        );
        guard.logs.push(format!("경고: {message}"));
        guard.warnings.push((1, message));
    }
    // P1: the default case folder fell back to the temp directory
    // (no home directory found) — say so on screen.
    if let Some(message) = case_dir_warning {
        guard.logs.push(format!("경고: {message}"));
        guard.warnings.push((0, message));
    }
}

/// R5-S6: the job error of an E01 run stopped on an unconfirmed raw image.
pub(crate) fn raw_mismatch_error(reason: &str, raw_path: &Path) -> String {
    format!(
        "raw 불일치 — 재임포트 필요: {reason}. 기존 이미지({})는 지우지 않았습니다. 다시 임포트하려면 '재임포트'를 누르십시오 (기존 raw 이미지를 지우고 E01에서 다시 추출합니다).",
        raw_path.display()
    )
}

/// `POST /api/reimport-raw` (R5-S6): the examiner's decision after an E01
/// run stopped on a raw image it could not confirm. Records the discard
/// in the case's E01 audit lane, deletes the image (outside the state
/// lock, in the worker) and re-runs the same analysis. Optional body
/// `raw_path` must name the pending image (a stale button cannot discard
/// another one). Refused while busy, without a pending decision, or when
/// the bound case changed since.
pub(crate) fn api_reimport_raw(request: &Request, state: &SharedState) -> String {
    api_reimport_raw_with(request, state, None)
}

/// [`api_reimport_raw`] with the CLI executable injected (tests run a
/// fake; `None` = this executable).
pub(crate) fn api_reimport_raw_with(
    request: &Request,
    state: &SharedState,
    exe: Option<PathBuf>,
) -> String {
    let (lease, pending) = {
        let mut guard = state_lock(state);
        let Some(pending) = guard.raw_reimport.clone() else {
            return api_err("재임포트를 기다리는 raw 이미지가 없습니다.");
        };
        if let Some(named) = body_value(&request.body, "raw_path")
            && !named.trim().is_empty()
            && pending.raw_path != Path::new(named.trim())
        {
            return api_err("요청한 raw 이미지가 재임포트 대기 중인 이미지와 다릅니다.");
        }
        if guard.case_dir.as_deref() != Some(pending.job.case_dir.as_path()) {
            guard.raw_reimport = None;
            return api_err(CASE_SWITCHED_ERROR);
        }
        if guard.busy {
            return api_err("이미 분석이 진행 중입니다.");
        }
        if let Some(refusal) = side_work_refusal(&guard, "재임포트하십시오") {
            return api_err(&refusal);
        }
        let lease = match lease_busy_locked(&mut guard, state) {
            Ok(lease) => lease,
            Err(err) => return api_err(&err),
        };
        begin_pipeline_locked(&mut guard, &pending.job, false, None);
        guard.logs.push(format!(
            "검사자 결정: 기존 raw 이미지를 지우고 다시 임포트합니다 ({})",
            pending.raw_path.display()
        ));
        (lease, pending)
    };
    let epoch = lease.epoch();
    let worker_state = Arc::clone(state);
    let spawned = thread::Builder::new()
        .name("ft-pipeline".into())
        .spawn(move || {
            let _lease = lease;
            if let Err(err) = discard_raw_recorded(&pending) {
                fail(&worker_state, &err);
                return;
            }
            match exe {
                Some(exe) => run_e01_pipeline_with(worker_state, pending.job, &exe),
                None => run_pipeline(worker_state, pending.job),
            }
        });
    if spawned.is_err() {
        let mut guard = state_lock(state);
        if guard.busy_epoch == epoch {
            guard.phase = "idle";
        }
        return api_err("작업 스레드 시작 실패");
    }
    "{\"ok\":true}".to_string()
}

/// Audits, then deletes, the raw image the examiner chose to re-import
/// (R5-S6). An audit failure keeps the image: a deletion is never
/// unrecorded.
fn discard_raw_recorded(pending: &super::state::RawReimport) -> Result<(), String> {
    let body = serde_json::json!({
        "schema_version": 1,
        "event": "discard-raw",
        "unix": crate::util::now_unix().unwrap_or(0),
        "raw_output_path": pending.raw_path.to_string_lossy(),
        "e01_path": pending.job.source_path.to_string_lossy(),
        "reason": pending.reason,
        "decided_by": "examiner (/api/reimport-raw)",
    });
    crate::audit::append_chained_jsonl(
        &pending.job.case_dir.join("evidence/logs/e01-audit.jsonl"),
        &body.to_string(),
    )
    .map_err(|err| format!("raw 이미지 삭제를 감사 기록에 남기지 못해 지우지 않았습니다: {err}"))?;
    crate::e01::discard_raw(&pending.raw_path)
        .map_err(|err| format!("이전 raw 이미지를 지우지 못해 재임포트할 수 없습니다: {err}"))
}

/// Media roots a new analysis approves for `/media`: the case plus, for
/// folder input, exactly the registered source — nothing above it.
pub(crate) fn start_media_roots(
    kind: InputKind,
    case_dir: &Path,
    source_path: &Path,
) -> Vec<PathBuf> {
    match kind {
        InputKind::Folder => vec![case_dir.to_path_buf(), source_path.to_path_buf()],
        InputKind::E01 | InputKind::E01Direct => vec![case_dir.to_path_buf()],
    }
}

/// True when `path` is a filesystem/volume root (`/`, `E:\`, `\\srv\share\`).
pub(crate) fn source_is_volume_root(path: &Path) -> bool {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    resolved.parent().is_none()
}

/// Where new cases go when the examiner leaves the case folder empty (P1),
/// decided purely (environment and filesystem injected): Windows uses
/// `%USERPROFILE%`; elsewhere `$HOME/Documents` when that folder exists,
/// else `$HOME`. Only without any home directory does it fall back to the
/// temp directory — which reboots and cleaners empty — and then the second
/// value is `true` so the caller warns on screen.
pub(crate) fn default_case_root(
    windows: bool,
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    is_dir: &dyn Fn(&Path) -> bool,
    temp_dir: &Path,
) -> (PathBuf, bool) {
    let var = |name: &str| env(name).filter(|value| !value.is_empty());
    if windows {
        if let Some(profile) = var("USERPROFILE") {
            return (PathBuf::from(profile), false);
        }
    } else if let Some(home) = var("HOME") {
        let home = PathBuf::from(home);
        let documents = home.join("Documents");
        return if is_dir(&documents) {
            (documents, false)
        } else {
            (home, false)
        };
    }
    (temp_dir.to_path_buf(), true)
}

/// Warning shown when the default case folder lives in the temp directory.
pub(crate) fn temp_case_dir_warning(dir: &Path) -> String {
    format!(
        "홈 폴더를 찾지 못해 케이스를 임시 폴더에 만듭니다 ({}) — 재부팅이나 정리 도구가 지울 수 있으니 케이스 폴더를 직접 지정하십시오.",
        dir.display()
    )
}

/// The default case folder plus the temp-fallback warning, if any (P1).
pub(crate) fn default_case_dir_checked() -> (PathBuf, Option<String>) {
    let (root, temp_fallback) = default_case_root(
        cfg!(windows),
        &|name| std::env::var_os(name),
        &|path| path.is_dir(),
        &std::env::temp_dir(),
    );
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let dir = root.join("FrameTrace").join(format!("case-{stamp}"));
    let warning = temp_fallback.then(|| temp_case_dir_warning(&dir));
    (dir, warning)
}

pub(crate) fn run_pipeline(state: SharedState, job: PipelineJob) {
    let (case_dir, source_path) = (job.case_dir.clone(), job.source_path.clone());
    match job.kind {
        InputKind::Folder => run_folder_pipeline(state, job),
        InputKind::E01 | InputKind::E01Direct => run_e01_pipeline(state, job),
    }
    // Finished (or failed/cancelled): bump the stage-1 recent list so the
    // case's new state shows at the top.
    record_recent_case(&case_dir, Some(source_path.as_path()));
}

pub(crate) fn run_e01_pipeline(state: SharedState, job: PipelineJob) {
    match std::env::current_exe() {
        Ok(exe) => run_e01_pipeline_with(state, job, &exe),
        Err(err) => fail(
            &state,
            &format!("실행 파일 경로를 확인할 수 없습니다: {err}"),
        ),
    }
}

/// R4-B16: runs a filesystem-inspection step whose CLI may report a
/// partial result (exit code 5). A partial run marks `step` as `partial`
/// and records a job warning; the pipeline continues with what it got.
fn run_inspection_step(
    exe: &Path,
    args: &[String],
    state: &SharedState,
    step: usize,
) -> Result<String, String> {
    let run = run_step_detailed(exe, args, &|| state_lock(state).cancel_requested, state)?;
    if run.partial {
        let message = partial_step_warning(args, &run.output);
        let mut guard = state_lock(state);
        guard.steps[step] = StepStatus::Partial;
        guard.logs.push(format!("경고: {message}"));
        guard.warnings.push((step, message));
    } else {
        state_lock(state).steps[step] = StepStatus::Done;
    }
    Ok(run.output)
}

/// [`run_e01_pipeline`] on an explicit CLI executable (tests run a fake).
pub(crate) fn run_e01_pipeline_with(state: SharedState, job: PipelineJob, exe: &Path) {
    let log = |state: &SharedState, line: String| state_lock(state).logs.push(line);
    let set_step = |state: &SharedState, index: usize, status: StepStatus| {
        state_lock(state).steps[index] = status;
    };
    let exe = exe.to_path_buf();
    let case_text = job.case_dir.to_string_lossy().to_string();
    let source_text = job.source_path.to_string_lossy().to_string();
    let raw_path = job.case_dir.join("evidence/images/evidence.raw");
    let raw_text = raw_path.to_string_lossy().to_string();

    // Step 1: case init (reuses an existing case folder).
    set_step(&state, 0, StepStatus::Running);
    if job.case_dir.join("case.json").exists() {
        log(&state, format!("기존 케이스 재사용: {case_text}"));
        set_step(&state, 0, StepStatus::Done);
    } else {
        match run_step(
            &exe,
            &[
                "init-case".into(),
                case_text.clone(),
                "--title".into(),
                "FrameTrace E01 검수 케이스".into(),
            ],
            &state,
        ) {
            Ok(output) => {
                log(&state, output);
                set_step(&state, 0, StepStatus::Done);
            }
            Err(err) => {
                set_step(&state, 0, StepStatus::Failed);
                fail(&state, &err);
                return;
            }
        }
    }

    if job.e01_direct {
        // Triage mode: a single inspect-e01 --filesystem run does ewfinfo
        // metadata plus mmls/fls straight off the segment set — no raw
        // export, so even a ~1 TB image is reviewable in minutes.
        set_step(&state, 1, StepStatus::Running);
        match run_inspection_step(
            &exe,
            &[
                "inspect-e01".into(),
                case_text.clone(),
                source_text.clone(),
                "--filesystem".into(),
            ],
            &state,
            2,
        ) {
            Ok(output) => {
                log(&state, output);
                set_step(&state, 1, StepStatus::Done);
            }
            Err(err) => {
                set_step(&state, 1, StepStatus::Failed);
                fail(&state, &err);
                return;
            }
        }
        run_e01_pipeline_tail(state, job, &exe);
        return;
    }

    // Step 2: import the E01 (ewfverify runs unless explicitly skipped).
    // A pre-existing raw image is reused ONLY when it was imported from
    // the same E01 path; otherwise the wizard would silently analyze the
    // previous evidence end-to-end (wrong-evidence results).
    set_step(&state, 1, StepStatus::Running);
    // C1: reuse only an image whose completion marker (written by
    // import-e01 after the export, hash and audit entry) matches the file
    // and names this E01; anything else — a killed import's torn image,
    // a pre-marker import, another E01 — is discarded and re-imported.
    // R5-S6: an image that does not match its marker (a moved or copied
    // case, a pre-marker case) is re-hashed against the recorded SHA-256
    // first; one that still cannot be confirmed is NOT deleted — the run
    // stops and the examiner decides (`/api/reimport-raw`).
    // R6-S4: the re-hash reports progress (`/api/status.progress`,
    // job_type `raw-rehash`) and stops on cancel.
    let rehash_hooks = crate::e01::RehashHooks {
        on_rehash: &|_| {
            log(
                &state,
                format!("기존 raw 이미지의 SHA-256을 임포트 기록과 다시 비교합니다: {raw_text}"),
            )
        },
        progress: &|done, total| state_lock(&state).rehash_progress = Some((done, total)),
        cancelled: &|| state_lock(&state).cancel_requested,
    };
    let reconciled =
        crate::e01::reconcile_raw(&job.case_dir, &raw_path, &job.source_path, &rehash_hooks);
    state_lock(&state).rehash_progress = None;
    let reuse_existing_raw = match reconciled {
        crate::e01::RawReuse::Reuse => true,
        crate::e01::RawReuse::Absent => false,
        crate::e01::RawReuse::Cancelled => {
            set_step(&state, 1, StepStatus::Failed);
            fail(&state, "사용자가 분석을 중단했습니다.");
            return;
        }
        crate::e01::RawReuse::NeedsReimport(reason) => {
            set_step(&state, 1, StepStatus::Failed);
            let message = raw_mismatch_error(&reason, &raw_path);
            state_lock(&state).raw_reimport = Some(super::state::RawReimport {
                raw_path: raw_path.clone(),
                reason,
                job: job.clone(),
            });
            fail(&state, &message);
            return;
        }
    };
    if reuse_existing_raw {
        log(&state, format!("기존 raw 이미지 재사용: {raw_text}"));
        set_step(&state, 1, StepStatus::Done);
    } else {
        let mut args: Vec<String> = vec![
            "import-e01".into(),
            case_text.clone(),
            source_text.clone(),
            "--output".into(),
            raw_text.clone(),
        ];
        if job.skip_e01_verify {
            args.push("--skip-verify".into());
        }
        // ewfexport runs inside the CLI child, so no in-process progress
        // callback exists — the UI instead reports the growing raw file
        // size as a byte-level progress signal. E01 is compressed, so the
        // source size is only a lower bound; pass None (indeterminate).
        state_lock(&state).byte_progress = Some((raw_path.clone(), None));
        let import_result = run_step(&exe, &args, &state);
        state_lock(&state).byte_progress = None;
        match import_result {
            Ok(output) => {
                log(&state, output);
                // C1: import-e01 itself bound the raw to this E01 (its
                // completion marker); a marker write failure failed the step.
                set_step(&state, 1, StepStatus::Done);
            }
            Err(err) => {
                set_step(&state, 1, StepStatus::Failed);
                fail(&state, &err);
                return;
            }
        }
    }

    // Step 3: partition table + file listing (auto-selects the partition).
    set_step(&state, 2, StepStatus::Running);
    match run_inspection_step(
        &exe,
        &["inspect-image".into(), case_text.clone(), raw_text.clone()],
        &state,
        2,
    ) {
        Ok(output) => log(&state, output),
        Err(err) => {
            set_step(&state, 2, StepStatus::Failed);
            fail(&state, &err);
            return;
        }
    }

    run_e01_pipeline_tail(state, job, &exe);
}

/// Shared E01 tail: refresh the logical index over the case evidence
/// tree, run the non-fatal anomaly scan, then emit the review bundle.
pub(crate) fn run_e01_pipeline_tail(state: SharedState, job: PipelineJob, exe: &Path) {
    let log = |state: &SharedState, line: String| state_lock(state).logs.push(line);
    let set_step = |state: &SharedState, index: usize, status: StepStatus| {
        state_lock(state).steps[index] = status;
    };
    let exe = exe.to_path_buf();
    let case_text = job.case_dir.to_string_lossy().to_string();
    set_step(&state, 3, StepStatus::Running);

    // E01 filesystem triage: extract allocated videos preserving folder hierarchy
    if let Some((image_path, partition_offset)) = crate::serve::records::newest_tsk_inspection(&job.case_dir) {
        log(
            &state,
            format!(
                "정상 영상 추출 중 (기존 폴더 구조 유지: artifacts/extracted @ offset {partition_offset})…"
            ),
        );
        let extract_args = vec![
            "recover-batch".into(),
            case_text.clone(),
            image_path.to_string_lossy().to_string(),
            "--allocated-videos".into(),
            "--partition-offset".into(),
            partition_offset.to_string(),
        ];
        match run_step(&exe, &extract_args, &state) {
            Ok(output) => log(&state, output),
            Err(err) => log(&state, format!("정상 영상 자동 추출 건너뜀: {err}")),
        }
    }

    let extracted_dir = job.case_dir.join("artifacts/extracted");
    let scan_target = if extracted_dir.is_dir() {
        extracted_dir.to_string_lossy().to_string()
    } else {
        job.case_dir.join("evidence").to_string_lossy().to_string()
    };

    let mut scan_args: Vec<String> = vec![
        "scan-folder".into(),
        case_text.clone(),
        scan_target,
        "--no-ffprobe".into(),
    ];
    if job.with_hash {
        scan_args.push("--hash".into());
    }
    if job.with_deepfake {
        scan_args.push("--deepfake".into());
    }
    match run_step(&exe, &scan_args, &state) {
        Ok(output) => {
            log(&state, output);
            set_step(&state, 3, StepStatus::Done);
            if job.with_deepfake {
                log(&state, "딥페이크 스크리닝은 색인된 파일에 적용되었습니다 — 이후 카빙/inode 복구로 추가된 파일은 고급 도구의 '딥페이크 스크리닝'으로 보강하십시오.".into());
            }
        }
        Err(err) => {
            warn_step(&state, 3, format!("논리 색인 건너뜀: {err}"));
            set_step(&state, 3, StepStatus::Done);
        }
    }

    // Step 5: candidate anomaly scan (non-fatal) then review bundle.
    match run_step(
        &exe,
        &["qa".into(), "anomalies".into(), case_text.clone()],
        &state,
    ) {
        Ok(output) => log(&state, output),
        Err(err) => warn_step(&state, 4, format!("이상 징후 스캔 건너뜀: {err}")),
    }
    set_step(&state, 4, StepStatus::Running);
    match run_step(&exe, &["make-review".into(), case_text.clone()], &state) {
        Ok(output) => {
            log(&state, output);
            set_step(&state, 4, StepStatus::Done);
            let mut guard = state_lock(&state);
            guard.phase = "review-ready";
            guard.busy = false;
            guard.byte_progress = None;
            guard.logs.push(
                "검토 화면이 준비되었습니다. 삭제 영상 후보는 '삭제 영상 후보 복구' 버튼으로, 카빙·개별 inode 복구는 CLI(carve-file/recover-inode)로 수행한 뒤 재검토하십시오.".into(),
            );
        }
        Err(err) => {
            set_step(&state, 4, StepStatus::Failed);
            fail(&state, &err);
        }
    }
}

/// Folder pipeline, split in two stages (UX-02):
///
/// * Phase 1 — init-case → register-source → a fast `scan-folder` without
///   hashing, ffprobe or deepfake screening. As soon as it writes the
///   index the phase flips to `review-partial` and the examiner can start
///   reviewing.
/// * Phase 2 — [`run_folder_enrichment`]: an incremental `scan-folder`
///   that adds the requested hashes/probes (record_is_unchanged reprocesses
///   only records missing them), validate-batch, the anomaly pass and
///   make-review, ending in `review-ready`.
pub(crate) fn run_folder_pipeline(state: SharedState, job: PipelineJob) {
    let log = |state: &SharedState, line: String| state_lock(state).logs.push(line);
    let set_step = |state: &SharedState, index: usize, status: StepStatus| {
        state_lock(state).steps[index] = status;
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            fail(
                &state,
                &format!("실행 파일 경로를 확인할 수 없습니다: {err}"),
            );
            return;
        }
    };
    let case_dir = job.case_dir.clone();
    let case_text = case_dir.to_string_lossy().to_string();
    let source_text = job.source_path.to_string_lossy().to_string();

    set_step(&state, 0, StepStatus::Running);
    if case_dir.join("case.json").exists() {
        log(&state, format!("기존 케이스 재사용: {case_text}"));
        set_step(&state, 0, StepStatus::Done);
    } else {
        match run_step(
            &exe,
            &[
                "init-case".into(),
                case_text.clone(),
                "--title".into(),
                "FrameTrace 검수 케이스".into(),
            ],
            &state,
        ) {
            Ok(output) => {
                log(&state, output);
                set_step(&state, 0, StepStatus::Done);
            }
            Err(err) => {
                set_step(&state, 0, StepStatus::Failed);
                fail(&state, &err);
                return;
            }
        }
    }

    set_step(&state, 1, StepStatus::Running);
    match run_step(
        &exe,
        &[
            "register-source".into(),
            case_text.clone(),
            source_text.clone(),
            "--kind".into(),
            "folder".into(),
            "--write-protect".into(),
            "launcher-managed read-only review".into(),
        ],
        &state,
    ) {
        Ok(output) => {
            log(&state, output);
            set_step(&state, 1, StepStatus::Done);
        }
        Err(err) => {
            // register-source is an upsert, so re-registering an already
            // known source succeeds — any failure here is a real error the
            // examiner must see, not an "already registered" no-op (FOR-08).
            warn_step(&state, 1, format!("소스 등록 실패, 건너뜀: {err}"));
            set_step(&state, 1, StepStatus::Done);
        }
    }

    // Phase 1 scan: listing + container metadata only. Hashing and
    // ffprobe are the slow per-file passes; they move to phase 2.
    set_step(&state, 2, StepStatus::Running);
    let scan_args = phase1_scan_args(&case_text, &source_text);
    match run_step(&exe, &scan_args, &state) {
        Ok(output) => {
            log(&state, output);
            set_step(&state, 2, StepStatus::Done);
        }
        Err(err) => {
            set_step(&state, 2, StepStatus::Failed);
            fail(&state, &err);
            return;
        }
    }

    enter_review_partial(&state);
    run_folder_enrichment(&state, &job, |args| run_step(&exe, args, &state));
}

/// Phase-1 `scan-folder` arguments. `--incremental` makes a rerun on an
/// existing case reuse every unchanged record verbatim (hashes and ffprobe
/// metadata included) instead of rebuilding it hash-less; together with
/// scan-folder's carry rule (a pass never drops facts it did not compute)
/// and its salvage of a cancelled phase-2 checkpoint, re-analysing after a
/// cancel resumes hashing where it stopped (Blockers 1/2).
pub(crate) fn phase1_scan_args(case_text: &str, source_text: &str) -> Vec<String> {
    vec![
        "scan-folder".into(),
        case_text.into(),
        source_text.into(),
        "--no-ffprobe".into(),
        "--incremental".into(),
    ]
}

/// Phase 1 is done: the hash-less index is on disk and reviewable while
/// phase 2 keeps the job busy (UX-02).
pub(crate) fn enter_review_partial(state: &SharedState) {
    let mut guard = state_lock(state);
    guard.phase = "review-partial";
    guard.logs.push(
        "1단계 색인이 끝났습니다 — 지금부터 검토할 수 있습니다. 해시·검증·리뷰 생성은 백그라운드에서 이어서 진행됩니다."
            .into(),
    );
}

/// Ordered phase-2 steps for `job` as `(key, label)`.
pub(crate) fn enrichment_plan(job: &PipelineJob) -> Vec<(&'static str, &'static str)> {
    let mut plan = Vec::new();
    if job.with_hash || job.with_ffprobe || job.with_deepfake {
        plan.push(("hash", "해시 · 메타데이터 보강"));
    }
    if job.with_ffprobe {
        plan.push(("validate", "재생성 검증"));
    }
    if job.with_audio && job.with_ffprobe {
        plan.push(("audio", "소리 측정"));
    }
    plan.push(("anomalies", "이상 징후 스캔"));
    plan.push(("make-review", "리뷰 생성"));
    plan
}

/// Phase 2 of the folder pipeline. `runner` executes one CLI step (the
/// real pipeline passes `run_step`; tests inject a fake) and must honor
/// the cancel flag like `run_step` does.
///
/// A cancel or a fatal failure here never throws the examiner out of the
/// review that phase 1 already opened: the phase stays `review-partial`
/// (see [`stop_enrichment`]), with the reason visible in warnings/error.
pub(crate) fn run_folder_enrichment(
    state: &SharedState,
    job: &PipelineJob,
    mut runner: impl FnMut(&[String]) -> Result<String, String>,
) {
    let log = |line: String| state_lock(state).logs.push(line);
    let set_step = |index: usize, status: StepStatus| {
        state_lock(state).steps[index] = status;
    };
    let case_dir = &job.case_dir;
    let case_text = case_dir.to_string_lossy().to_string();
    let plan = enrichment_plan(job);
    let stages = plan.len();
    // Publishes the step and reports whether the run may proceed (false
    // once a cancel arrived between steps).
    let begin = |key: &'static str| -> bool {
        // The step flips under the marks gate so an import never sees a
        // stale step; entering a short-transaction step applies the marks
        // queued during the previous (long) one.
        let _gate = marks_gate();
        {
            let mut guard = state_lock(state);
            if guard.cancel_requested {
                return false;
            }
            if let Some(position) = plan.iter().position(|(step, _)| *step == key) {
                guard.enrichment = Some(Enrichment {
                    step: key,
                    label: plan[position].1,
                    stage: position + 1,
                    stages,
                });
            }
        }
        if matches!(key, "validate" | "audio" | "anomalies")
            && let Err(err) = drain_pending_marks(state, case_dir)
        {
            warn_step(state, 3, err);
        }
        true
    };
    let cancelled = || state_lock(state).cancel_requested;

    set_step(3, StepStatus::Running);
    if plan.iter().any(|(step, _)| *step == "hash") {
        if !begin("hash") {
            return stop_enrichment(state, 3, None);
        }
        let mut args: Vec<String> = vec![
            "scan-folder".into(),
            case_text.clone(),
            job.source_path.to_string_lossy().to_string(),
            "--incremental".into(),
        ];
        if job.with_hash {
            args.push("--hash".into());
        }
        if !job.with_ffprobe {
            args.push("--no-ffprobe".into());
        }
        if job.with_deepfake {
            args.push("--deepfake".into());
        }
        match runner(&args) {
            Ok(output) => log(output),
            Err(_) if cancelled() => return stop_enrichment(state, 3, None),
            Err(err) => warn_step(
                state,
                3,
                format!("해시·메타데이터 보강 실패 — 1단계 색인(해시 없음)을 유지합니다: {err}"),
            ),
        }
    }

    if job.with_ffprobe {
        if !begin("validate") {
            return stop_enrichment(state, 3, None);
        }
        let selection_path = case_dir.join("selection-all.json");
        match build_selection_file(case_dir, &selection_path) {
            Ok(count) => {
                log(format!("검증 대상 {count}건"));
                match runner(&[
                    "validate-batch".into(),
                    case_text.clone(),
                    selection_path.to_string_lossy().to_string(),
                ]) {
                    Ok(output) => log(output),
                    Err(_) if cancelled() => return stop_enrichment(state, 3, None),
                    Err(err) => warn_step(state, 3, format!("일괄 검증 실패, 건너뜀: {err}")),
                }
            }
            Err(err) => warn_step(state, 3, format!("선택 목록 생성 실패, 검증 건너뜀: {err}")),
        }
    } else {
        log("ffprobe 검증이 꺼져 있어 건너뜁니다.".to_string());
    }
    if plan.iter().any(|(step, _)| *step == "audio") {
        if !begin("audio") {
            return stop_enrichment(state, 3, None);
        }
        // measure-audio skips files that already carry a valid measurement
        // and chains one log entry per 200 files (plan §2.1).
        match runner(&["measure-audio".into(), case_text.clone()]) {
            Ok(output) => {
                if let Some(line) = crate::audio_level::summary_log_line(&output) {
                    log(line);
                }
                log(output)
            }
            Err(_) if cancelled() => return stop_enrichment(state, 3, None),
            // B15: exit 4 = completed and logged, some files unmeasurable.
            Err(err) => match crate::audio_level::partial_error_count(&err) {
                Some(errors) => warn_step(
                    state,
                    3,
                    format!(
                        "소리 측정 완료(오류 {errors}건) — 측정하지 못한 영상은 '미확인'으로 표시됩니다"
                    ),
                ),
                None => warn_step(state, 3, format!("소리 측정 실패, 건너뜀: {err}")),
            },
        }
    } else if job.with_audio {
        warn_step(
            state,
            3,
            "소리 측정은 ffprobe 메타데이터 보강이 켜져 있어야 합니다 — 건너뜁니다.".to_string(),
        );
    }
    set_step(3, StepStatus::Done);

    set_step(4, StepStatus::Running);
    if !begin("anomalies") {
        return stop_enrichment(state, 4, None);
    }
    match runner(&["qa".into(), "anomalies".into(), case_text.clone()]) {
        Ok(output) => log(output),
        Err(_) if cancelled() => return stop_enrichment(state, 4, None),
        Err(err) => warn_step(state, 4, format!("이상 징후 스캔 건너뜀: {err}")),
    }

    if !begin("make-review") {
        return stop_enrichment(state, 4, None);
    }
    // Marks saved during phase 2 must be in the review: queued ones are
    // applied under the gate, and make-review re-runs until no marks landed
    // since it last started. The flip to review-ready happens under the
    // same gate, so a late import either gets in before it or sees
    // review-ready and takes the normal path.
    let mut seen_epoch = {
        let _gate = marks_gate();
        if let Err(err) = drain_pending_marks(state, case_dir) {
            drop(_gate);
            return stop_enrichment(state, 4, Some(&err));
        }
        state_lock(state).marks_epoch
    };
    for _ in 0..8 {
        match runner(&["make-review".into(), case_text.clone()]) {
            Ok(output) => log(output),
            Err(_) if cancelled() => return stop_enrichment(state, 4, None),
            Err(err) => return stop_enrichment(state, 4, Some(&err)),
        }
        let _gate = marks_gate();
        if let Err(err) = drain_pending_marks(state, case_dir) {
            drop(_gate);
            return stop_enrichment(state, 4, Some(&err));
        }
        let mut guard = state_lock(state);
        if guard.marks_epoch == seen_epoch {
            guard.steps[4] = StepStatus::Done;
            guard.phase = "review-ready";
            guard.busy = false;
            guard.byte_progress = None;
            guard.enrichment = None;
            guard
                .logs
                .push("검토 화면이 준비되었습니다. 뷰어에서 증거를 확인하십시오.".into());
            return;
        }
        seen_epoch = guard.marks_epoch;
        guard
            .logs
            .push("리뷰 생성 중 저장된 판독 마크를 포함하도록 리뷰를 다시 생성합니다.".into());
    }
    stop_enrichment(
        state,
        4,
        Some("리뷰 생성 중 판독 마크가 계속 바뀌어 안정화하지 못했습니다 — 다시 분석하십시오."),
    );
}

/// Ends phase 2 early without losing phase 1 (UX-02): the phase stays
/// `review-partial` and the job is released. `failure = None` means the
/// examiner cancelled — recorded as a warning so the run never reads as a
/// clean pass; `Some(err)` is a real failure surfaced as `error`.
pub(crate) fn stop_enrichment(state: &SharedState, step: usize, failure: Option<&str>) {
    // Marks queued behind a long step are applied now so they are never
    // stranded; the gate keeps a racing import from queueing after this.
    let _gate = marks_gate();
    let case_dir = state_lock(state).case_dir.clone();
    let drain_failure = case_dir.and_then(|dir| drain_pending_marks(state, &dir).err());
    let mut guard = state_lock(state);
    if let Some(err) = drain_failure {
        guard.logs.push(format!("오류: {err}"));
        guard.warnings.push((step, err));
    }
    guard.phase = "review-partial";
    guard.busy = false;
    guard.byte_progress = None;
    let interrupted = guard.enrichment.take();
    let what = interrupted.map(|e| e.label).unwrap_or("보강");
    match failure {
        None => {
            guard.steps[step] = StepStatus::Pending;
            let message = format!(
                "{what} 단계에서 보강을 중단했습니다 — 해시·검증·리뷰 생성이 완료되지 않았습니다. 1단계 색인으로 검토는 계속할 수 있으며, 같은 소스로 다시 분석하면 이미 계산한 해시는 다시 계산하지 않고 남은 파일부터 이어서 진행합니다."
            );
            guard.logs.push(format!("경고: {message}"));
            guard.warnings.push((step, message));
        }
        Some(err) => {
            guard.steps[step] = StepStatus::Failed;
            guard.error = Some(err.to_string());
            guard.logs.push(format!(
                "오류: {what} 실패 — 1단계 색인으로 검토는 계속할 수 있습니다: {err}"
            ));
        }
    }
}

/// Records a non-fatal step failure (FOR-08): logged like before, but also
/// kept in `JobState::warnings` so /api/status reports the run as
/// "done with warnings" instead of a clean green pass.
pub(crate) fn warn_step(state: &SharedState, step: usize, message: String) {
    let mut guard = state_lock(state);
    guard.logs.push(format!("경고: {message}"));
    guard.warnings.push((step, message));
}

pub(crate) fn fail(state: &SharedState, message: &str) {
    let mut guard = state_lock(state);
    guard.phase = "error";
    guard.busy = false;
    guard.byte_progress = None;
    guard.error = Some(message.to_string());
    guard.logs.push(format!("오류: {message}"));
}

/// How much of a pipeline step's stdout/stderr the workstation keeps.
/// The full stream is drained (required to prevent the deadlock below) but
/// only this tail is retained for the log panel and error reporting.
pub(crate) const STEP_OUTPUT_TAIL_BYTES: usize = 256 * 1024;

pub(crate) fn run_step(exe: &Path, args: &[String], state: &SharedState) -> Result<String, String> {
    run_step_cancellable(exe, args, &|| state_lock(state).cancel_requested, state)
}

/// R4-B16 (contract with track C): a CLI child that exits with this code
/// completed but produced a PARTIAL result (e.g. an fls listing cut off by
/// its timeout). Never reported as a clean success.
pub(crate) const PARTIAL_EXIT_CODE: i32 = 5;

/// One finished CLI step: its retained output and whether the child
/// reported a partial result ([`PARTIAL_EXIT_CODE`]).
pub(crate) struct StepRun {
    pub(crate) output: String,
    pub(crate) partial: bool,
}

/// Job warning for a step that ended with [`PARTIAL_EXIT_CODE`].
pub(crate) fn partial_step_warning(args: &[String], output: &str) -> String {
    let program = args.first().map(String::as_str).unwrap_or("command");
    let tail = output.lines().rev().find(|line| !line.trim().is_empty());
    format!(
        "{program}: 부분 결과로 끝났습니다 (종료 코드 {PARTIAL_EXIT_CODE}) — 일부 항목이 빠졌을 수 있으니 결과를 완전한 것으로 보지 마십시오.{}",
        tail.map(|line| format!(" ({})", line.trim()))
            .unwrap_or_default()
    )
}

/// R4-B15: the argument vector a CLI child really gets — the workstation's
/// `--key-source` (a global flag, so it may precede the subcommand) comes
/// first unless the step already names one.
pub(crate) fn child_command_args(key_source: Option<&str>, args: &[String]) -> Vec<String> {
    let names_one = args
        .iter()
        .any(|arg| arg == "--key-source" || arg.starts_with("--key-source="));
    match key_source.map(str::trim).filter(|spec| !spec.is_empty()) {
        Some(spec) if !names_one => ["--key-source".to_string(), spec.to_string()]
            .into_iter()
            .chain(args.iter().cloned())
            .collect(),
        _ => args.to_vec(),
    }
}

/// R4-B15: environment variable a CLI child compares its own resolved
/// audit key against (`<key id>:<fingerprint>` or `none`), see
/// [`crate::serve::key_source_mismatch_warning`].
pub(crate) const EXPECTED_AUDIT_KEY_ENV: &str = "FRAMETRACE_EXPECTED_AUDIT_KEY";

/// P6: a step whose process tree could not be put in a Job object is
/// reported as a job warning (log + `warnings` in /api/status), once per
/// distinct message — the windowed app has no console for `eprintln!`.
pub(crate) fn report_process_warning(state: &SharedState, message: String) {
    eprintln!("frametrace: {message}");
    let mut guard = state_lock(state);
    if guard.warnings.iter().any(|(_, known)| *known == message) {
        return;
    }
    let step = guard
        .steps
        .iter()
        .position(|step| *step == StepStatus::Running)
        .unwrap_or(0);
    guard.logs.push(format!("경고: {message}"));
    guard.warnings.push((step, message));
}

/// [`run_step`] with an explicit cancel predicate, for jobs that do not use
/// the global cancel flag (clip exports cancel per job, UX-09). A `true`
/// from `cancelled` kills the whole process tree. `state` receives the
/// process-tracking warning, if any (P6).
pub(crate) fn run_step_cancellable(
    exe: &Path,
    args: &[String],
    cancelled: &dyn Fn() -> bool,
    state: &SharedState,
) -> Result<String, String> {
    let run = run_step_detailed(exe, args, cancelled, state)?;
    if run.partial {
        // R4-B16: completed with a partial result — kept, never silent.
        report_process_warning(state, partial_step_warning(args, &run.output));
    }
    Ok(run.output)
}

/// [`run_step_cancellable`] that reports a partial result (exit code
/// [`PARTIAL_EXIT_CODE`]) to the caller instead of turning it into a
/// generic warning, so a pipeline can mark the step `partial`.
pub(crate) fn run_step_detailed(
    exe: &Path,
    args: &[String],
    cancelled: &dyn Fn() -> bool,
    state: &SharedState,
) -> Result<StepRun, String> {
    run_step_detailed_opts(exe, args, cancelled, false, state)
}

#[cfg(test)]
thread_local! {
    /// R12-S1 test hook: a cancel arrives right after the step exited —
    /// in the gap between the last poll and the post-exit cancel check
    /// (sets the job's global cancel flag there, once, for the step whose
    /// first argument is the one named).
    pub(crate) static CANCEL_AFTER_EXIT: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

/// [`run_step_detailed`]; with `success_wins` (R12-S1, P14) a step whose
/// own exit code was observed as success (or partial) is a success even
/// when a cancel arrived meanwhile — R11 turned a package-case that had
/// exited 0 into "cancelled" when the cancel came in the 150 ms polling
/// gap, leaving a published package the finalize then treated as failed.
pub(crate) fn run_step_detailed_opts(
    exe: &Path,
    args: &[String],
    cancelled: &dyn Fn() -> bool,
    success_wins: bool,
    state: &SharedState,
) -> Result<StepRun, String> {
    let (key_source, key_tag) = {
        let guard = state_lock(state);
        (guard.key_source.clone(), guard_key_tag(&guard))
    };
    let child_args = child_command_args(key_source.as_deref(), args);
    // R8-S7: a fresh random token per step; only marker lines carrying it
    // are promoted (text the step merely relays cannot forge one).
    let token = step_marker_token()?;
    let markers = [
        crate::util::tokened_marker(super::KEY_MISMATCH_MARKER, &token),
        crate::util::tokened_marker(crate::util::STEP_WARNING_MARKER, &token),
    ];
    let mut command = Command::new(exe);
    command
        .args(&child_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // R7-S6: the child prints its warnings as STEP_WARNING_MARKER
        // lines, promoted to job warnings below.
        .env(crate::util::STEP_WARNING_ENV, &token);
    if let Some(tag) = key_tag {
        command.env(EXPECTED_AUDIT_KEY_ENV, tag);
    }
    // R10-I14: the audit key variables this process adopted (removed from
    // its environment at start) go to FrameTrace's own step child only.
    crate::audit_key::forward_adopted_key_env(&mut command);
    // Spawned as a process tree (own group / Job object) so cancel reaches
    // the ffmpeg/ffprobe grandchildren, not just the direct child.
    let mut tracked = super::proc::spawn_tracked(&mut command)
        .map_err(|err| format!("{} 실행 실패: {err}", exe.display()))?;
    if let Some(err) = tracked.tracking_error.take() {
        let program = args.first().map(String::as_str).unwrap_or("command");
        report_process_warning(state, super::proc::tracking_warning(program, &err));
    }
    let child_pid = tracked.child.id();
    let child = &mut tracked.child;
    // Both pipes must be drained while the child runs: a child that fills
    // the OS pipe buffer blocks on write, and a parent that only polls
    // try_wait() then waits forever — the classic pipe deadlock. tsk.rs
    // uses the same reader-thread pattern for icat.
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_reader = thread::spawn(move || drain_capped(&mut stdout_pipe));
    let stderr_reader = thread::spawn(move || {
        let markers: Vec<&str> = markers.iter().map(String::as_str).collect();
        drain_capped_collecting(&mut stderr_pipe, &markers)
    });
    let mut killed = false;
    let status = loop {
        if !killed && cancelled() {
            killed = true;
            tracked.kill_tree();
        }
        match tracked.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(150)),
            Err(err) => return Err(format!("{} 실행 대기 실패: {err}", exe.display())),
        }
    };
    #[cfg(test)]
    if CANCEL_AFTER_EXIT.with(|hook| {
        let fire = hook.get().is_some() && hook.get() == args.first().map(String::as_str);
        if fire {
            hook.set(None);
        }
        fire
    }) {
        state_lock(state).cancel_requested = true;
    }
    // The child has exited, so both pipes are normally at EOF and these
    // joins return immediately with whatever tail was retained. C10: a
    // grandchild that escaped tree tracking can still hold them — the
    // joins are bounded and the abandoned output is reported, never
    // waited on forever.
    let cap = crate::join::READER_JOIN_CAP;
    let stdout_tail = crate::join::join_reader_bounded(stdout_reader, cap);
    let stderr_tail = crate::join::join_reader_bounded(stderr_reader, cap);
    if stdout_tail.is_none() || stderr_tail.is_none() {
        report_process_warning(state, super::media::reader_abandoned_warning(args, cap));
    }
    let stdout_tail = stdout_tail.unwrap_or_default();
    let (stderr_tail, key_warnings) = stderr_tail.unwrap_or_default();
    // R5-S9: the child said it signs with another audit key than this
    // workstation — a job warning (status `warnings`, log), not just a
    // stderr line nobody sees in the windowed app. R7-S6: so is any
    // warning the child printed as a step warning.
    for warning in key_warnings {
        let program = args.first().map(String::as_str).unwrap_or("command");
        report_process_warning(state, format!("{program}: {warning}"));
    }
    if killed {
        // C1/C2/C4: the killed step's in-progress `.part` files go.
        for line in super::media::sweep_killed_step(args, child_pid) {
            match line {
                Ok(line) => state_lock(state).logs.push(line),
                Err(err) => report_process_warning(state, err),
            }
        }
    }
    let partial = status.code() == Some(PARTIAL_EXIT_CODE);
    // A tree kill ends the leader with exit code 1 (Windows job) or a
    // signal, so a success seen here is the step's own.
    let own_success = success_wins && (status.success() || partial);
    if !own_success && (killed || cancelled()) {
        return Err("사용자가 분석을 중단했습니다.".to_string());
    }
    let mut text = String::from_utf8_lossy(&stdout_tail).to_string();
    let stderr = String::from_utf8_lossy(&stderr_tail);
    if !stderr.trim().is_empty() {
        text.push_str(&stderr);
    }
    if !status.success() && !partial {
        let mut tail_lines: Vec<&str> = text.lines().rev().take(4).collect();
        tail_lines.reverse();
        let tail = tail_lines.join(" | ");
        return Err(format!(
            "{} 실패 (exit {:?}): {}",
            args.first().map(String::as_str).unwrap_or("command"),
            status.code(),
            tail
        ));
    }
    Ok(StepRun {
        output: text.trim_end().to_string(),
        partial,
    })
}

/// `<id>:<fingerprint>` of the workstation's audit key (or `none`), set
/// once at startup; `None` when it was never resolved (unit tests).
fn guard_key_tag(guard: &JobState) -> Option<String> {
    guard.audit_key_tag.clone()
}

/// Drains `pipe` to EOF, retaining only the last `STEP_OUTPUT_TAIL_BYTES`.
/// Retention stays bounded even when a step emits unbounded progress
/// output, while the drain itself keeps the child's write end from ever
/// blocking on a full pipe.
pub(crate) fn drain_capped(pipe: &mut impl std::io::Read) -> Vec<u8> {
    drain_capped_collecting(pipe, &[]).0
}

/// R8-S7: a fresh random token (32 hex digits) binding one step's marker
/// lines to that step.
pub(crate) fn step_marker_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|err| format!("단계 경고 토큰을 만들지 못했습니다 (entropy): {err}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Most marker lines one step may raise PER MARKER (R5-S9; R8-S7: the cap
/// was shared, so a flood of step warnings hid an audit-key mismatch).
const MAX_MARKER_LINES: usize = 8;

/// [`drain_capped`] that also returns every complete line starting with
/// one of `markers` (marker stripped, at most [`MAX_MARKER_LINES`] per
/// marker, then one line saying how many more were left out) wherever it
/// appeared — R5-S9: a child's audit-key mismatch warning is printed first
/// and would otherwise fall out of the retained tail of a chatty step.
pub(crate) fn drain_capped_collecting(
    pipe: &mut impl std::io::Read,
    markers: &[&str],
) -> (Vec<u8>, Vec<String>) {
    let mut buffer = Vec::new();
    let mut found: Vec<String> = Vec::new();
    let mut per_marker = vec![0usize; markers.len()];
    // Bytes of the current, not yet terminated line (bounded).
    let mut line: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut take_line = |line: &mut Vec<u8>, found: &mut Vec<String>| {
        if !markers.is_empty() {
            let text = String::from_utf8_lossy(line);
            let text = text.trim_end_matches('\r');
            if let Some((which, rest)) = markers
                .iter()
                .enumerate()
                .find_map(|(which, marker)| text.strip_prefix(marker).map(|rest| (which, rest)))
            {
                per_marker[which] += 1;
                if per_marker[which] <= MAX_MARKER_LINES {
                    found.push(rest.trim().to_string());
                }
            }
        }
        line.clear();
    };
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if !markers.is_empty() {
                    for &byte in &chunk[..read] {
                        if byte == b'\n' {
                            take_line(&mut line, &mut found);
                        } else if line.len() < 4096 {
                            line.push(byte);
                        }
                    }
                }
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.len() > STEP_OUTPUT_TAIL_BYTES {
                    let excess = buffer.len() - STEP_OUTPUT_TAIL_BYTES;
                    buffer.drain(..excess);
                }
            }
        }
    }
    if !line.is_empty() {
        take_line(&mut line, &mut found);
    }
    for count in per_marker {
        if count > MAX_MARKER_LINES {
            found.push(format!(
                "같은 종류의 경고 {}건을 더 생략했습니다 / {} more warning(s) of this kind omitted",
                count - MAX_MARKER_LINES,
                count - MAX_MARKER_LINES
            ));
        }
    }
    (buffer, found)
}

/// Extract every top-level `videos[].id` from db/video_index.json and
/// write a validate-batch selection file covering all of them. The index
/// is one JSON document (see `scan_index_json`); parsing it properly keeps
/// `"id"` literals inside string values or nested ffprobe records from
/// being mistaken for video ids.
pub(crate) fn build_selection_file(case_dir: &Path, output: &Path) -> Result<usize, String> {
    let index_path = case_dir.join("db/video_index.json");
    let index = std::fs::read_to_string(&index_path)
        .map_err(|err| format!("failed to read {}: {err}", index_path.display()))?;
    let parsed =
        serde_json::from_str::<serde_json::Value>(&index).unwrap_or(serde_json::Value::Null);
    let ids: Vec<String> = parsed
        .get("videos")
        .and_then(|videos| videos.as_array())
        .map(|videos| {
            videos
                .iter()
                .filter_map(|video| video.get("id").and_then(|id| id.as_str()))
                .filter(|id| id.starts_with("vid_"))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return Err("색인된 영상이 없습니다 (스캔 결과 확인 필요)".into());
    }
    let items = ids
        .iter()
        .map(|id| {
            format!(
                "{{\"selector\":{},\"kind\":\"video\",\"action\":\"validate\"}}",
                json_string(id)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let case_id = crate::selection::read_case_id(case_dir)?;
    let file = format!(
        "{{\"schema_version\":1,\"case_id\":{},\"items\":[{items}]}}",
        json_string(&case_id)
    );
    std::fs::write(output, file)
        .map_err(|err| format!("failed to write {}: {err}", output.display()))?;
    Ok(ids.len())
}

/// Serializes marks applies against the phase-2 steps that hold long case-DB
/// transactions (scan-folder hashing) or read the marks (make-review).
static MARKS_GATE: Mutex<()> = Mutex::new(());

fn marks_gate() -> MutexGuard<'static, ()> {
    MARKS_GATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

const PENDING_MARKS_DIR: &str = "marks-pending";

fn pending_marks_files(case_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(case_dir.join(PENDING_MARKS_DIR)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        // R4-B10: a zero-length file is a name claim whose write never
        // finished (crash between claim and rename) — it holds no marks.
        .filter(|entry| entry.metadata().is_ok_and(|meta| meta.len() > 0))
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

/// Arrival sequence of a queued file (`<digits>.json`), if its name is one.
fn queued_sequence(path: &Path) -> Option<u128> {
    path.file_stem()?.to_str()?.parse::<u128>().ok()
}

/// R4-B10: process-wide high-water mark of queue sequence numbers.
static QUEUE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Claims the next queue file name in `dir` and returns it. Names are a
/// monotonic sequence — strictly above every queued file already present
/// (also those named by a wall clock in older builds) and above every name
/// this process handed out — and are claimed with `create_new`, so neither
/// a clock step backwards nor two saves in the same instant can reorder
/// or overwrite queued marks. The caller replaces the empty claim with the
/// body atomically. Callers hold the marks gate.
pub(crate) fn claim_queue_file(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let on_disk = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|entry| queued_sequence(&entry.path()))
        .max()
        .unwrap_or(0);
    let mut next = on_disk
        .max(u128::from(QUEUE_SEQUENCE.load(Ordering::SeqCst)))
        .saturating_add(1);
    for _ in 0..1024 {
        let path = dir.join(format!("{next:032}.json"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => {
                QUEUE_SEQUENCE.fetch_max(u64::try_from(next).unwrap_or(u64::MAX), Ordering::SeqCst);
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => next += 1,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("no free queue file name"))
}

/// Number of queued (accepted but not yet applied) marks imports.
pub(crate) fn pending_marks_count(case_dir: &Path) -> usize {
    pending_marks_files(case_dir).len()
}

/// Append-only journal of ids a queued (deferred) import could not apply
/// because the case had moved on (save protocol v2). Each conflict line
/// keeps the client's original entry, the server value and the time, so
/// nothing an examiner typed is ever lost; a later patch that applies the
/// id appends a `resolved` line. `/api/status.mark_conflicts` counts the
/// unresolved ids.
pub(crate) const PENDING_MARK_CONFLICTS_FILE: &str = "marks-pending/conflicts.jsonl";

fn pending_mark_conflicts_path(case_dir: &Path) -> PathBuf {
    case_dir.join(PENDING_MARK_CONFLICTS_FILE)
}

fn append_jsonl_lines(path: &Path, lines: &[String]) -> Result<(), String> {
    use std::io::Write;
    if lines.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| format!("failed to open {}: {err}", path.display()))?;
    for line in lines {
        file.write_all(line.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|err| format!("failed to append to {}: {err}", path.display()))?;
    }
    file.sync_data()
        .map_err(|err| format!("failed to sync {}: {err}", path.display()))
}

/// Journals the conflicts of a queued import together with the client's
/// original entries (read back from the queued body, verbatim).
fn record_queue_conflicts(
    case_dir: &Path,
    queued_file: &Path,
    outcome: &crate::case_db::PatchOutcome,
) -> Result<(), String> {
    if outcome.conflicts.is_empty() {
        return Ok(());
    }
    let body: serde_json::Value = crate::util::read_to_string(queued_file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(serde_json::Value::Null);
    let find = |key: &str, id: &str| -> serde_json::Value {
        body.get(key)
            .and_then(serde_json::Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item.get("id").and_then(serde_json::Value::as_str) == Some(id))
            })
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    let now = crate::util::now_unix().unwrap_or(0);
    let source = queued_file
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    // R5-S11: every new conflict line carries the next journal sequence
    // number — the stable cursor `/api/mark-conflicts?after=` pages by.
    // Callers hold the marks gate, so no other writer appends meanwhile.
    let mut next_seq = crate::util::read_to_string(&pending_mark_conflicts_path(case_dir))
        .map(|text| replay_conflict_journal_seq(&text).1)
        .unwrap_or(0);
    let lines: Vec<String> = outcome
        .conflicts
        .iter()
        .map(|conflict| {
            next_seq += 1;
            let deleted = body
                .get("deleted_ids")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(&conflict.id)));
            serde_json::json!({
                "seq": next_seq,
                "recorded_unix": now,
                "source": source,
                "id": conflict.id,
                "server_revision": conflict.server_revision,
                "server": conflict.server,
                "mine": {
                    "mark": find("marks", &conflict.id),
                    "tags": find("tags", &conflict.id).get("tags").cloned().unwrap_or(serde_json::Value::Null),
                    "deleted": deleted,
                    "base_revision": body.get("base_revisions").and_then(|b| b.get(&conflict.id)).cloned().unwrap_or(serde_json::Value::Null),
                },
                "client_id": body.get("client_id").cloned().unwrap_or(serde_json::Value::Null),
                "examiner": body.get("examiner").cloned().unwrap_or(serde_json::Value::Null),
            })
            .to_string()
        })
        .collect();
    append_jsonl_lines(&pending_mark_conflicts_path(case_dir), &lines)
}

/// One record's open (unresolved) queue conflict: the latest journaled
/// entry for it, the highest server revision any of its open entries saw
/// (what a resolving save must have seen), and how many open entries the
/// journal holds for it.
pub(crate) struct OpenQueueConflict {
    /// R5-S11: journal sequence of the line that opened this conflict —
    /// stable for as long as it stays open (paging cursor).
    seq: u64,
    id: String,
    latest: serde_json::Value,
    server_revision: u64,
    entries: usize,
}

/// Replays journal text into the open conflicts (first-seen order of the
/// currently open run of each id). R4-B3: an id index keeps this linear —
/// the old `Vec` scan per line was quadratic in the number of ids.
fn replay_conflict_journal(text: &str) -> Vec<OpenQueueConflict> {
    replay_conflict_journal_seq(text).0
}

/// R5-S11: the journal sequence of each line, in order: a line's own
/// `seq`, else (lines written before R5) one more than the previous line;
/// a `seq_high` line (written by compaction) raises the running maximum so
/// numbers handed out before a compaction are never handed out again.
fn journal_line_seqs(values: &[Option<serde_json::Value>]) -> (Vec<u64>, u64) {
    let mut last = 0u64;
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        if let Some(high) = value
            .as_ref()
            .and_then(|v| v.get("seq_high"))
            .and_then(serde_json::Value::as_u64)
        {
            last = last.max(high);
            out.push(high);
            continue;
        }
        let seq = value
            .as_ref()
            .and_then(|v| v.get("seq"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(last + 1);
        last = last.max(seq);
        out.push(seq);
    }
    (out, last)
}

/// [`replay_conflict_journal`] plus the highest sequence number in use.
fn replay_conflict_journal_seq(text: &str) -> (Vec<OpenQueueConflict>, u64) {
    use std::collections::HashMap;
    let values: Vec<Option<serde_json::Value>> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();
    let (seqs, high) = journal_line_seqs(&values);
    // One slot per open run; a resolved id's slot is emptied and a later
    // conflict for it opens a new slot at the end.
    let mut slots: Vec<Option<OpenQueueConflict>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (value, seq) in values.into_iter().zip(seqs) {
        let Some(value) = value else {
            continue;
        };
        if let Some(id) = value.get("resolved").and_then(serde_json::Value::as_str) {
            if let Some(slot) = index.remove(id) {
                slots[slot] = None;
            }
        } else if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
            let revision = value
                .get("server_revision")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            match index.get(id).and_then(|&slot| slots[slot].as_mut()) {
                Some(entry) => {
                    entry.server_revision = entry.server_revision.max(revision);
                    entry.entries += 1;
                    entry.latest = value;
                }
                None => {
                    index.insert(id.to_string(), slots.len());
                    slots.push(Some(OpenQueueConflict {
                        seq,
                        id: id.to_string(),
                        latest: value,
                        server_revision: revision,
                        entries: 1,
                    }));
                }
            }
        }
    }
    (slots.into_iter().flatten().collect(), high)
}

/// `(path, len, mtime)` → parsed open conflicts (R4-B3 count cache):
/// `/api/status` polls every second, and the journal only changes by an
/// append or a compaction — both change the key.
type ConflictCacheEntry = (
    PathBuf,
    u64,
    Option<SystemTime>,
    Arc<Vec<OpenQueueConflict>>,
);
static CONFLICT_CACHE: Mutex<Vec<ConflictCacheEntry>> = Mutex::new(Vec::new());
const CONFLICT_CACHE_CAP: usize = 8;

thread_local! {
    /// Journal parses performed on this thread (tests prove the cache).
    pub(crate) static CONFLICT_JOURNAL_PARSES: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// Unresolved queue conflicts: distinct ids whose last journal entry is a
/// conflict (not a `resolved` line), in first-seen order. Cached per
/// journal file size + mtime (R4-B3).
fn open_queue_conflicts(case_dir: &Path) -> Arc<Vec<OpenQueueConflict>> {
    let path = pending_mark_conflicts_path(case_dir);
    let Ok(meta) = std::fs::metadata(&path) else {
        return Arc::new(Vec::new());
    };
    let (len, mtime) = (meta.len(), meta.modified().ok());
    let lock = || CONFLICT_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((_, _, _, hit)) = lock()
        .iter()
        .find(|(p, l, m, _)| *p == path && *l == len && *m == mtime)
    {
        return Arc::clone(hit);
    }
    let Ok(text) = crate::util::read_to_string(&path) else {
        return Arc::new(Vec::new());
    };
    CONFLICT_JOURNAL_PARSES.with(|count| count.set(count.get() + 1));
    let open = Arc::new(replay_conflict_journal(&text));
    // Only cache what the key really describes: a write that landed while
    // this read ran changed size/mtime and must not be cached as this one.
    let unchanged = std::fs::metadata(&path)
        .is_ok_and(|after| after.len() == len && after.modified().ok() == mtime)
        && text.len() as u64 == len;
    if unchanged {
        let mut cache = lock();
        cache.retain(|(p, _, _, _)| *p != path);
        if cache.len() >= CONFLICT_CACHE_CAP {
            cache.remove(0);
        }
        cache.push((path, len, mtime, Arc::clone(&open)));
    }
    open
}

/// R4-B3: journal archive — compaction moves every line that no longer
/// describes an open conflict here (appended, never rewritten), so the
/// complete history stays on disk.
pub(crate) const MARK_CONFLICTS_ARCHIVE_FILE: &str = "marks-pending/conflicts-archive.jsonl";

/// Compaction threshold: at least this many lines, more than half closed.
const CONFLICT_COMPACT_MIN_LINES: usize = 512;

/// R4-B3: rewrites the journal down to the lines of conflicts that are
/// still open (the ones after each open id's last `resolved` marker),
/// after first appending every other line — verbatim, in order — to
/// [`MARK_CONFLICTS_ARCHIVE_FILE`]. Archive first, then an atomic rewrite:
/// a crash in between leaves duplicates in the archive, never a loss.
/// Replaying the compacted journal yields exactly the same open set.
/// Returns `(archived, kept)` line counts. Callers hold the marks gate.
pub(crate) fn compact_conflict_journal(case_dir: &Path) -> Result<(usize, usize), String> {
    use std::collections::HashMap;
    let path = pending_mark_conflicts_path(case_dir);
    let text = match crate::util::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let parsed: Vec<Option<serde_json::Value>> = lines
        .iter()
        .map(|l| serde_json::from_str(l.trim()).ok())
        .collect();
    // R5-S11: kept lines keep their sequence numbers (stamped in when they
    // had none), and the high-water mark survives as a `seq_high` line.
    let (seqs, high) = journal_line_seqs(&parsed);
    // Index of the last `resolved` marker per id.
    let mut last_resolved: HashMap<String, usize> = HashMap::new();
    for (at, value) in parsed.iter().enumerate() {
        if let Some(id) = value
            .as_ref()
            .and_then(|v| v.get("resolved"))
            .and_then(serde_json::Value::as_str)
        {
            last_resolved.insert(id.to_string(), at);
        }
    }
    let mut keep = Vec::new();
    let mut archive = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let open_entry = parsed[at]
            .as_ref()
            .filter(|v| v.get("resolved").is_none())
            .and_then(|v| v.get("id"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| last_resolved.get(id).is_none_or(|&r| r < at));
        if open_entry {
            let stamped = match &parsed[at] {
                Some(serde_json::Value::Object(map)) if !map.contains_key("seq") => {
                    let mut map = map.clone();
                    map.insert("seq".into(), serde_json::json!(seqs[at]));
                    serde_json::Value::Object(map).to_string()
                }
                _ => line.to_string(),
            };
            keep.push(stamped);
        } else {
            archive.push(line.to_string());
        }
    }
    if archive.is_empty() {
        return Ok((0, keep.len()));
    }
    append_jsonl_lines(&case_dir.join(MARK_CONFLICTS_ARCHIVE_FILE), &archive)?;
    let kept = keep.len();
    keep.insert(0, serde_json::json!({ "seq_high": high }).to_string());
    let mut body = keep.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    crate::util::write_text_atomic(&path, &body)
        .map_err(|err| format!("failed to rewrite {}: {err}", path.display()))?;
    Ok((archive.len(), kept))
}

/// Compacts the journal once it is large and mostly history (R4-B3).
fn maybe_compact_conflict_journal(case_dir: &Path) -> Result<(), String> {
    let path = pending_mark_conflicts_path(case_dir);
    let Ok(text) = crate::util::read_to_string(&path) else {
        return Ok(());
    };
    let total = text.lines().filter(|l| !l.trim().is_empty()).count();
    if total < CONFLICT_COMPACT_MIN_LINES {
        return Ok(());
    }
    let open_lines: usize = replay_conflict_journal(&text)
        .iter()
        .map(|entry| entry.entries)
        .sum();
    if open_lines * 2 < total {
        compact_conflict_journal(case_dir).map(|_| ())
    } else {
        Ok(())
    }
}

/// Default and maximum page size of `/api/mark-conflicts` and
/// `/api/marks-pending` (R4-B3/B6).
pub(crate) const API_PAGE_DEFAULT: usize = 500;
pub(crate) const API_PAGE_MAX: usize = 5000;

/// `(offset, limit)` from `?offset=&limit=` (defaults 0 / 500, limit
/// clamped to 1..=5000; an unparsable value falls back to the default).
pub(crate) fn page_params(query: &str) -> (usize, usize) {
    let offset = query_value(query, "offset")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let limit = query_value(query, "limit")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(API_PAGE_DEFAULT)
        .clamp(1, API_PAGE_MAX);
    (offset, limit)
}

/// R5-S11: `?after=<seq>` — the stable cursor (entries with a larger
/// sequence number), `None` when absent or unparsable.
pub(crate) fn page_after(query: &str) -> Option<u64> {
    query_value(query, "after").and_then(|v| v.trim().parse::<u64>().ok())
}

/// Number of unresolved queue conflicts (distinct ids). `/api/finalize`
/// refuses while it is above zero.
pub(crate) fn unresolved_queue_conflict_count(case_dir: &Path) -> usize {
    open_queue_conflicts(case_dir).len()
}

/// `/api/status.mark_conflicts`: `{count, ids}` with at most 50 ids.
pub(crate) fn pending_mark_conflicts(case_dir: &Path) -> serde_json::Value {
    let open = open_queue_conflicts(case_dir);
    serde_json::json!({
        "count": open.len(),
        "ids": open.iter().take(50).map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
    })
}

/// `GET /api/mark-conflicts?offset=&limit=` (contract §10.2; paginated by
/// R4-B3): unresolved queue conflicts, one per record id in first-seen
/// order, with the server value each met and the entry the window
/// originally queued (`queued`, verbatim from the queued body: `{mark,
/// tags, deleted, base_revision}`), so the viewer can show "my value"
/// exactly as it was submitted. `count` is the total over all pages and
/// `next_offset` is `null` on the last page.
pub(crate) fn api_mark_conflicts_page(request: &Request, state: &SharedState) -> String {
    let (offset, limit) = page_params(&request.query);
    mark_conflicts_page_at(state, offset, limit, page_after(&request.query))
}

/// The pre-R4 unpaginated shape (`{ok, count, conflicts}`, every open
/// conflict), kept for the existing unit tests until track F ports them
/// to [`api_mark_conflicts_page`].
#[cfg(test)]
pub(crate) fn api_mark_conflicts(state: &SharedState) -> String {
    let mut body: serde_json::Value =
        serde_json::from_str(&mark_conflicts_page(state, 0, usize::MAX)).unwrap_or_default();
    if let Some(map) = body.as_object_mut() {
        for key in ["offset", "limit", "next_offset", "after", "next_after"] {
            map.remove(key);
        }
    }
    body.to_string()
}

#[cfg(test)]
fn mark_conflicts_page(state: &SharedState, offset: usize, limit: usize) -> String {
    mark_conflicts_page_at(state, offset, limit, None)
}

/// R5-S11: with `after = Some(seq)` the page is the first `limit` open
/// conflicts whose sequence number is above `seq` (offset ignored) and
/// `next_after` is the cursor of the next page (`null` on the last). An
/// open conflict keeps its number until it is resolved and a new one gets
/// a larger number, so resolving or adding entries between two requests
/// can never make a page skip one — the old `offset` paging could. The
/// `offset` form stays for older viewers.
fn mark_conflicts_page_at(
    state: &SharedState,
    offset: usize,
    limit: usize,
    after: Option<u64>,
) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let open = case_dir
        .as_deref()
        .map(open_queue_conflicts)
        .unwrap_or_default();
    let total = open.len();
    let skip = match after {
        Some(after) => open.iter().take_while(|entry| entry.seq <= after).count(),
        None => offset,
    };
    let page: Vec<&OpenQueueConflict> = open.iter().skip(skip).take(limit).collect();
    let next_after = (skip + page.len() < total)
        .then(|| page.last().map(|entry| entry.seq))
        .flatten();
    let conflicts: Vec<serde_json::Value> = page
        .iter()
        .map(|entry| {
            let field = |key: &str| {
                entry
                    .latest
                    .get(key)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
            };
            serde_json::json!({
                "seq": entry.seq,
                "id": entry.id,
                "server_revision": entry.server_revision,
                "server": field("server"),
                "queued": field("mine"),
                "client_id": field("client_id"),
                "recorded_unix": field("recorded_unix"),
                "entries": entry.entries,
            })
        })
        .collect();
    let end = skip.saturating_add(conflicts.len());
    serde_json::json!({
        "ok": true,
        "count": total,
        "offset": skip,
        "limit": limit,
        "after": after,
        "next_offset": (end < total).then_some(end),
        "next_after": next_after,
        "conflicts": conflicts,
    })
    .to_string()
}

/// Total bytes `/api/marks-pending` reads from the queue in one request.
const MARKS_PENDING_READ_CAP: u64 = 64 * 1024 * 1024;

/// `GET /api/marks-pending?offset=&limit=` (R4-B6, for the viewer's A4
/// reload path): what the durable queue holds, folded per record id in
/// arrival order — the LATEST queued value of each id (`mark` — the
/// queued mark object verbatim, `null` once a later body deleted it;
/// `tags`; `deleted`; `base_revision`; `queued_file`). Read-only. Files
/// that could not be read or parsed are listed in `unreadable` (never
/// skipped silently).
pub(crate) fn api_marks_pending(request: &Request, state: &SharedState) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return api_err("열린 케이스가 없습니다.");
    };
    let (offset, limit) = page_params(&request.query);
    marks_pending_json_at(&case_dir, offset, limit, pending_page_after(&request.query)).to_string()
}

/// R6-S2: a `/api/marks-pending` cursor — the position `(file_seq,
/// index)` of an id's FIRST appearance: the queued file's arrival number
/// and the index of the item inside that file (marks, then tags, then
/// deleted ids, counted together). Ordered lexicographically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PendingCursor {
    pub(crate) seq: u64,
    pub(crate) index: u64,
}

impl PendingCursor {
    /// `"<seq>:<index>"`, the string the API hands out.
    pub(crate) fn text(&self) -> String {
        format!("{}:{}", self.seq, self.index)
    }

    /// Parses `"<seq>:<index>"`, or a bare `<seq>` (the R5 numeric
    /// cursor: everything of that file counts as seen).
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        match text.split_once(':') {
            Some((seq, index)) => Some(PendingCursor {
                seq: seq.trim().parse().ok()?,
                index: index.trim().parse().ok()?,
            }),
            None => Some(PendingCursor {
                seq: text.parse().ok()?,
                index: u64::MAX,
            }),
        }
    }
}

/// `?after=` of `/api/marks-pending` (R6-S2): a string cursor or, from
/// an R5 viewer, a number. `None` when absent or unparsable.
pub(crate) fn pending_page_after(query: &str) -> Option<PendingCursor> {
    query_value(query, "after").and_then(|v| PendingCursor::parse(&v))
}

/// Pure part of [`api_marks_pending`] (offset paging).
#[cfg(test)]
pub(crate) fn marks_pending_json(
    case_dir: &Path,
    offset: usize,
    limit: usize,
) -> serde_json::Value {
    marks_pending_json_at(case_dir, offset, limit, None)
}

/// R5-S11 / R6-S2: each folded entry carries `seq` (the arrival number of
/// the queued file the id FIRST appears in) and `cursor` (`"<seq>:<index>"`,
/// the item's position inside that file). `after = Some(cursor)` pages by
/// it (offset ignored) and `next_after` is the next page's cursor STRING.
/// The R5 cursor was the bare file number, so a page that ended in the
/// middle of a big file (one save of 800 ids, limit 500) made the next
/// page skip the rest of that file. Draining a file between two requests
/// can only move an id to a LATER position (seen twice, never skipped),
/// and a new file always gets a larger number.
pub(crate) fn marks_pending_json_at(
    case_dir: &Path,
    offset: usize,
    limit: usize,
    after: Option<PendingCursor>,
) -> serde_json::Value {
    use std::collections::HashMap;
    let files = pending_marks_files(case_dir);
    let mut order: Vec<(PendingCursor, String)> = Vec::new();
    let mut entries: HashMap<String, serde_json::Map<String, serde_json::Value>> = HashMap::new();
    let mut unreadable: Vec<serde_json::Value> = Vec::new();
    let mut read_bytes = 0u64;
    let mut truncated = false;
    for path in &files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if read_bytes.saturating_add(size) > MARKS_PENDING_READ_CAP {
            truncated = true;
            break;
        }
        read_bytes += size;
        let body = match crate::util::read_to_string(path)
            .map_err(|err| err.to_string())
            .and_then(|text| {
                serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string())
            }) {
            Ok(body) => body,
            Err(err) => {
                unreadable.push(serde_json::json!({"file": name, "error": err}));
                continue;
            }
        };
        let items = |key: &str| {
            body.get(key)
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let file_seq = queued_sequence(path)
            .map(|n| u64::try_from(n).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let mut item_index = 0u64;
        let mut touch = |id: &str, fields: Vec<(&str, serde_json::Value)>| {
            let position = PendingCursor {
                seq: file_seq,
                index: item_index,
            };
            item_index += 1;
            if !entries.contains_key(id) {
                order.push((position, id.to_string()));
            }
            let item = entries.entry(id.to_string()).or_insert_with(|| {
                let mut map = serde_json::Map::new();
                map.insert("seq".into(), serde_json::json!(file_seq));
                map.insert("cursor".into(), serde_json::json!(position.text()));
                map.insert("id".into(), serde_json::Value::String(id.to_string()));
                map.insert("mark".into(), serde_json::Value::Null);
                map.insert("tags".into(), serde_json::Value::Null);
                map.insert("deleted".into(), serde_json::Value::Bool(false));
                map.insert("base_revision".into(), serde_json::Value::Null);
                map
            });
            item.insert(
                "queued_file".into(),
                serde_json::Value::String(name.clone()),
            );
            if let Some(base) = body.get("base_revisions").and_then(|b| b.get(id)) {
                item.insert("base_revision".into(), base.clone());
            }
            if let Some(client) = body.get("client_id") {
                item.insert("client_id".into(), client.clone());
            }
            for (key, value) in fields {
                item.insert(key.into(), value);
            }
        };
        for mark in items("marks") {
            if let Some(id) = mark.get("id").and_then(serde_json::Value::as_str) {
                let id = id.to_string();
                let mut fields = Vec::new();
                if let Some(tags) = mark.get("tags") {
                    fields.push(("tags", tags.clone()));
                }
                fields.push(("deleted", serde_json::Value::Bool(false)));
                fields.push(("mark", mark));
                touch(&id, fields);
            }
        }
        for tags in items("tags") {
            if let Some(id) = tags.get("id").and_then(serde_json::Value::as_str) {
                let value = tags.get("tags").cloned().unwrap_or(serde_json::Value::Null);
                touch(id, vec![("tags", value)]);
            }
        }
        for id in items("deleted_ids") {
            if let Some(id) = id.as_str() {
                touch(
                    id,
                    vec![
                        ("mark", serde_json::Value::Null),
                        ("deleted", serde_json::Value::Bool(true)),
                    ],
                );
            }
        }
    }
    let total = order.len();
    // `order` is first-appearance order, i.e. ascending cursor (files in
    // arrival order, items in file order).
    let skip = match after {
        Some(after) => order
            .iter()
            .take_while(|(position, _)| *position <= after)
            .count(),
        None => offset,
    };
    let mut last_cursor: Option<PendingCursor> = None;
    let page: Vec<serde_json::Value> = order
        .iter()
        .skip(skip)
        .take(limit)
        .filter_map(|(position, id)| {
            last_cursor = Some(*position);
            entries.remove(id).map(serde_json::Value::Object)
        })
        .collect();
    let end = skip.saturating_add(page.len());
    let next_after = (end < total)
        .then(|| last_cursor.map(|cursor| cursor.text()))
        .flatten();
    serde_json::json!({
        "ok": true,
        "files": files.len(),
        "count": total,
        "offset": skip,
        "limit": limit,
        "after": after.map(|cursor| if cursor.index == u64::MAX {
            cursor.seq.to_string()
        } else {
            cursor.text()
        }),
        "next_offset": (end < total).then_some(end),
        "next_after": next_after,
        "entries": page,
        "unreadable": unreadable,
        "truncated": truncated,
    })
}

/// After a successful in-process import: an id with an open queue
/// conflict is journaled as resolved only when a patch-v2 body APPLIED it
/// with a base at least the journaled server revision — i.e. the window
/// that saw the conflict decided it (contract §10.2). Legacy and patch-v1
/// bodies (`bases == None`) never resolve; neither does a v2 save applied
/// with an older base (same-window or unchanged-content exception).
fn resolve_queue_conflicts(
    case_dir: &Path,
    outcome: &crate::case_db::PatchOutcome,
    bases: Option<&std::collections::BTreeMap<String, u64>>,
) {
    let Some(bases) = bases else {
        return;
    };
    if outcome.applied.is_empty() || !pending_mark_conflicts_path(case_dir).is_file() {
        return;
    }
    let open = open_queue_conflicts(case_dir);
    if open.is_empty() {
        return;
    }
    let now = crate::util::now_unix().unwrap_or(0);
    let lines: Vec<String> = outcome
        .applied
        .iter()
        .filter(|(id, _)| {
            open.iter().any(|entry| {
                // R5 (base −1 contract): an "unsafe base" save (a draft
                // edited while the viewer was read-only) never decides a
                // conflict — it only lands when identical to the server
                // value, so it says nothing about which side the examiner
                // chose. (`u64::MAX >= revision` would otherwise resolve.)
                let base = bases.get(id).copied().unwrap_or(0);
                &entry.id == id
                    && base != crate::case_db::UNSAFE_BASE_REVISION
                    && base >= entry.server_revision
            })
        })
        .map(|(id, revision)| {
            serde_json::json!({"resolved": id, "revision": revision, "unix": now}).to_string()
        })
        .collect();
    // Best effort: a failure here leaves the conflict visible (never hides it).
    if append_jsonl_lines(&pending_mark_conflicts_path(case_dir), &lines).is_ok() {
        // R4-B3: keep the journal small once most of it is history; a
        // failed compaction leaves the complete journal as it was.
        let _ = maybe_compact_conflict_journal(case_dir);
    }
}

/// Applies every queued marks import in arrival order. A file is removed
/// only after its import committed, so a failure leaves it queued. Ids a
/// queued v2 body could no longer apply (the case moved on) are journaled
/// in `marks-pending/conflicts.jsonl` with the client's original entry —
/// never dropped — and surfaced through `/api/status.mark_conflicts`. The
/// caller holds `MARKS_GATE`. Returns how many files were applied.
fn drain_pending_marks(state: &SharedState, case_dir: &Path) -> Result<usize, String> {
    let mut applied = 0;
    let mut conflicts = 0usize;
    for path in pending_marks_files(case_dir) {
        let outcome = crate::cli::handlers::import_marks_quiet(case_dir, &path)
            .map_err(|err| format!("대기 중이던 마크 반영 실패 ({}): {err}", path.display()))?;
        // The journal is written BEFORE the queued file goes, so a crash in
        // between re-imports (idempotent) rather than losing the entry.
        record_queue_conflicts(case_dir, &path, &outcome).map_err(|err| {
            format!(
                "대기 중이던 마크 충돌 기록 실패 ({}): {err}",
                path.display()
            )
        })?;
        let bases = crate::selection::parse_marks_file(&path)
            .ok()
            .and_then(|marks| marks.base_revisions);
        resolve_queue_conflicts(case_dir, &outcome, bases.as_ref());
        conflicts += outcome.conflicts.len();
        let _ = std::fs::remove_file(&path);
        applied += 1;
        state_lock(state).marks_epoch += 1;
    }
    if applied > 0 {
        state_lock(state).logs.push(format!(
            "저장 대기 중이던 판독 마크 {applied}건을 반영했습니다."
        ));
    }
    if conflicts > 0 {
        state_lock(state).logs.push(format!(
            "대기열 반영 중 충돌 {conflicts}건 — 뷰어에서 확인하십시오 (marks-pending/conflicts.jsonl)."
        ));
    }
    Ok(applied)
}

/// The `/api/import-marks` success body (save protocol v2 contract, also
/// returned for legacy bodies): `applied` maps every applied id to its
/// revision after the patch, `conflicts` lists ids not applied with the
/// server's value, `batch_done` says whether this request completed its
/// batch (always true without `batch`), `report_url` is set only when
/// make-report ran.
fn import_marks_response(
    marks: &crate::selection::MarksFile,
    outcome: &crate::case_db::PatchOutcome,
    deferred: Option<usize>,
    report_refreshed: bool,
) -> String {
    let batch_done = marks.batch.as_ref().is_none_or(|batch| batch.is_last());
    let mut body = serde_json::json!({
        "ok": true,
        "protocol": marks.protocol.wire_name(),
        "stored": outcome.stored,
        "applied": outcome.applied_map(),
        "conflicts": outcome.conflicts,
        "batch_done": batch_done,
        "deferred": deferred.is_some(),
        "report_url": report_refreshed.then_some("case/reports/case-report.html"),
    });
    if let Some(pending) = deferred {
        body["pending"] = serde_json::json!(pending);
    }
    body.to_string()
}

/// True while a job that holds `busy` accepts marks through the durable
/// queue instead of refusing them: phase-2 enrichment (UX-02) and a
/// background recover/carve job (UX-06), whose make-review re-runs until
/// it has seen every queued mark.
///
/// R4-B4: exactly those two — an enrichment step in progress
/// (`enrichment` set) or a running case job. `review-partial` alone is
/// not enough: an index-only case opened for review is `review-partial`
/// too, and a short busy job there (verify-sources, an advanced tool) used
/// to strand saves in a queue nothing drained.
pub(crate) fn marks_must_queue(guard: &JobState) -> bool {
    guard.busy
        && (guard.enrichment.is_some()
            || guard
                .case_job
                .as_ref()
                .is_some_and(|job| job.state == "running"))
}

/// R4-B4: how long a save waits for a short busy job before it is told to
/// retry (never queued: nothing would drain it).
pub(crate) const MARKS_BUSY_WAIT: Duration = Duration::from_secs(2);

/// Refusal when the busy job did not end within [`MARKS_BUSY_WAIT`]: the
/// client keeps the edit dirty and retries (`retry: true`).
pub(crate) fn marks_busy_retry_response() -> String {
    serde_json::json!({
        "ok": false,
        "busy": true,
        "retry": true,
        "retry_after_ms": 1000,
        "error": "다른 작업이 진행 중이라 지금 저장하지 못했습니다 — 편집은 그대로 두고 잠시 후 자동으로 다시 저장합니다.",
    })
    .to_string()
}

/// R4-B4 outcome of waiting for `busy` on behalf of a marks save.
pub(crate) enum MarksBusy<'a> {
    /// `busy` taken for this save.
    Acquired(super::state::BusyGuard<'a>),
    /// A job that queues marks started meanwhile: take the queue route.
    Queue,
    /// Still busy after the wait (or shutdown): refusal body.
    Refused(String),
}

/// Takes `busy` for a marks save, waiting up to `wait` for a short busy
/// job to end (polling, no state lock held while waiting).
pub(crate) fn acquire_busy_for_marks(state: &SharedState, wait: Duration) -> MarksBusy<'_> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        {
            let guard = state_lock(state);
            if marks_must_queue(&guard) {
                return MarksBusy::Queue;
            }
            if let Some(refusal) = shutdown_refusal(&guard) {
                return MarksBusy::Refused(api_err(&refusal));
            }
        }
        match try_acquire_busy(state) {
            Ok(busy) => return MarksBusy::Acquired(busy),
            Err(_) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return MarksBusy::Refused(marks_busy_retry_response()),
        }
    }
}

/// R4-B4: applies marks queued while a job held `busy`, right after
/// `busy` is released (called by the busy guards on drop, outside the
/// state lock). Skipped when another thread — or this one, further up the
/// stack — holds the marks gate: that holder drains the queue itself.
pub(crate) fn drain_after_busy_release(state: &SharedState) {
    let case_dir = {
        let guard = state_lock(state);
        if guard.busy {
            return;
        }
        guard.case_dir.clone()
    };
    let Some(case_dir) = case_dir else {
        return;
    };
    if pending_marks_count(&case_dir) == 0 {
        return;
    }
    let _gate = match MARKS_GATE.try_lock() {
        Ok(gate) => gate,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    // Applied under `busy` so no job starts writing the case meanwhile; if
    // one already did, it drains at its own end.
    let Ok(_busy) = try_acquire_busy(state) else {
        return;
    };
    if state_lock(state).case_dir.as_deref() != Some(case_dir.as_path()) {
        return;
    }
    if let Err(err) = drain_pending_marks(state, &case_dir) {
        let mut guard = state_lock(state);
        if !guard.warnings.iter().any(|(_, known)| *known == err) {
            guard.logs.push(format!("오류: {err}"));
            guard.warnings.push((0, err));
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Test hook (R5-S10): runs right after a marks body was queued, with
    /// the marks gate still held — where a job's release used to strand it.
    pub(crate) static AFTER_QUEUE_WRITE: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Marks import while phase-2 enrichment is running (`review-partial` +
/// busy). Validate/anomaly steps only take short case-DB transactions, so
/// the marks are applied at once (SQLite's busy timeout covers the
/// overlap). During `hash` (one long scan transaction) and `make-review`
/// (reads the marks) the body is queued durably on disk and applied right
/// after that step, so nothing is lost and make-review always sees them.
fn import_marks_during_enrichment(
    state: &SharedState,
    case_dir: &Path,
    marks_body: &str,
    marks: &crate::selection::MarksFile,
) -> String {
    let _gate = marks_gate();
    // Re-checked under the gate: enrichment may have just finished.
    let (partial, step, rebound) = {
        let guard = state_lock(state);
        (
            marks_must_queue(&guard),
            guard.enrichment.map(|e| e.step),
            guard.case_dir.as_deref() != Some(case_dir),
        )
    };
    // R4-B11: the marks were checked against `case_dir`; a case switched
    // meanwhile must not receive them.
    if rebound {
        return api_err(CASE_SWITCHED_ERROR);
    }
    if !partial {
        return api_err("진행 중이던 작업이 방금 끝났습니다 — 잠시 후 다시 저장하십시오.");
    }
    let direct = matches!(step, Some("validate") | Some("audio") | Some("anomalies"))
        && pending_marks_count(case_dir) == 0;
    if !direct {
        let dir = case_dir.join(PENDING_MARKS_DIR);
        // R4-B10: a monotonic, create_new-claimed name instead of the wall
        // clock (which can step back or repeat and reorder the queue).
        let path = match claim_queue_file(&dir) {
            Ok(path) => path,
            Err(err) => return api_err(&format!("대기열 폴더를 만들 수 없습니다: {err}")),
        };
        if let Err(err) = crate::util::write_text_atomic(&path, marks_body) {
            // The empty claim holds nothing; never leave it half-written.
            let _ = std::fs::remove_file(&path);
            return api_err(&format!("대기열 저장 실패: {err}"));
        }
        state_lock(state).logs.push(
            "작업 진행 중이라 판독 마크를 대기열에 저장했습니다 (해당 단계 직후 반영).".into(),
        );
        #[cfg(test)]
        AFTER_QUEUE_WRITE.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook();
            }
        });
        let response = import_marks_response(
            marks,
            &crate::case_db::PatchOutcome::default(),
            Some(pending_marks_count(case_dir)),
            false,
        );
        // R5-S10: the job may have released `busy` while this request held
        // the marks gate — its drain then skipped (the gate holder is
        // supposed to drain) and the file just queued would sit until the
        // NEXT job ended. Re-check once the gate is free: if nothing holds
        // `busy` any more, apply the queue now (outside the state lock).
        drop(_gate);
        drain_after_busy_release(state);
        return response;
    }
    let marks_path = case_dir.join("marks-imported.json");
    if let Err(err) = crate::util::write_text_atomic(&marks_path, marks_body) {
        return api_err(&err.to_string());
    }
    match crate::cli::handlers::import_marks_quiet(case_dir, &marks_path) {
        Ok(outcome) => {
            resolve_queue_conflicts(case_dir, &outcome, marks.base_revisions.as_ref());
            state_lock(state).marks_epoch += 1;
            import_marks_response(marks, &outcome, None, false)
        }
        Err(err) => api_err(&err),
    }
}

/// Accepts the viewer's marks JSON (legacy export, patch-v1 or patch-v2),
/// applies it to the case in-process and refreshes the report so
/// examiner marks land in the deliverable.
pub(crate) fn api_import_marks(request: &Request, state: &SharedState) -> String {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return api_err(&err.to_string()),
    };
    api_import_marks_with_runner(request, state, &|args| run_step(&exe, args, state))
}

/// [`api_import_marks`] with the `make-report` child invocation injected
/// (tests count how often the report is rebuilt). The import itself runs
/// in-process so conflicts can be returned to the client.
///
/// `make-report` runs once per batch — for a request without `batch` or
/// for the last chunk (`index == total-1`) — never for an earlier chunk.
/// Each chunk is its own transaction; a chunk the client retries after a
/// failure is idempotent per record (revision-checked).
pub(crate) fn api_import_marks_with_runner(
    request: &Request,
    state: &SharedState,
    runner: &dyn Fn(&[String]) -> Result<String, String>,
) -> String {
    import_marks_with_hooks(request, state, runner, &|| {})
}

/// Refusal when the bound case changed between reading it and taking
/// `busy` (R4-B11).
pub(crate) const CASE_SWITCHED_ERROR: &str =
    "작업 대상 케이스가 방금 바뀌었습니다 — 화면을 새로 고친 뒤 다시 시도하십시오.";

/// [`api_import_marks_with_runner`] with a hook that runs after the marks
/// were checked against the bound case and before `busy` is taken (tests
/// switch the case there, R4-B11).
pub(crate) fn import_marks_with_hooks(
    request: &Request,
    state: &SharedState,
    runner: &dyn Fn(&[String]) -> Result<String, String>,
    before_busy: &dyn Fn(),
) -> String {
    let marks_body = match body_value(&request.body, "marks_json") {
        Some(text) if !text.trim().is_empty() => text,
        _ => {
            return "{\"ok\":false,\"error\":\"마크 JSON이 비어 있습니다.\"}".to_string();
        }
    };
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let marks_path = case_dir.join("marks-imported.json");
    let marks =
        match crate::selection::parse_marks_text(&marks_body, &marks_path).and_then(|marks| {
            crate::selection::enforce_case_binding(
                &case_dir,
                marks.case_id.as_deref(),
                &marks_path,
                crate::selection::ImportKind::Marks,
            )
            .map(|_| marks)
        }) {
            Ok(marks) => marks,
            Err(err) => return serde_json::json!({"ok": false, "error": err}).to_string(),
        };
    before_busy();
    // R4-B4: queue only while a job that drains the queue runs; any other
    // busy job is waited for briefly, then the client is told to retry.
    let _busy = match acquire_busy_for_marks(state, MARKS_BUSY_WAIT) {
        MarksBusy::Acquired(busy) => busy,
        MarksBusy::Queue => {
            return import_marks_during_enrichment(state, &case_dir, &marks_body, &marks);
        }
        MarksBusy::Refused(body) => return body,
    };
    // R4-B11: re-read under `busy` — open-case cannot switch the case any
    // more, and these marks were validated for `case_dir`.
    if state_lock(state).case_dir.as_deref() != Some(case_dir.as_path()) {
        return api_err(CASE_SWITCHED_ERROR);
    }
    // Anything queued during phase 2 is applied before this import.
    {
        let _gate = marks_gate();
        if let Err(err) = drain_pending_marks(state, &case_dir) {
            return api_err(&err);
        }
    }
    // P12: written atomically like the enrichment path — a torn file from
    // a crash mid-write must never be what import-marks reads.
    if let Err(err) = crate::util::write_text_atomic(&marks_path, &marks_body) {
        return api_err(&err.to_string());
    }
    let outcome = {
        let _gate = marks_gate();
        match crate::cli::handlers::import_marks_quiet(&case_dir, &marks_path) {
            Ok(outcome) => {
                resolve_queue_conflicts(&case_dir, &outcome, marks.base_revisions.as_ref());
                state_lock(state).marks_epoch += 1;
                outcome
            }
            Err(err) => return api_err(&err),
        }
    };
    let refresh_report = marks.batch.as_ref().is_none_or(|batch| batch.is_last());
    let mut report_error: Option<String> = None;
    if refresh_report {
        let case_text = case_dir.to_string_lossy().to_string();
        let refreshed = runner(&["make-report".into(), case_text]);
        // R12-S2: a recovered journal kept only for the stale report goes
        // once the report is rendered again (never a live finalize's).
        if refreshed.is_ok() && state_lock(state).phase != "finalizing" {
            super::finalize_journal::clear_if_resolved(&case_dir, state);
        }
        let mut guard = state_lock(state);
        match refreshed {
            Ok(_) => {
                // R11-S5: the report on disk is current again.
                guard.report_stale = false;
                guard
                    .logs
                    .push("판독 마크를 반영해 보고서를 갱신했습니다.".into())
            }
            // R4-B7: the marks ARE stored — reporting the save as failed
            // made the viewer retry (and conflict with itself). The report
            // failure is reported separately and stays visible.
            Err(err) => {
                let message = format!(
                    "판독 마크는 저장했지만 보고서를 갱신하지 못했습니다 — 최종화 전에 다시 생성됩니다: {err}"
                );
                guard.logs.push(format!("경고: {message}"));
                guard.warnings.push((4, message));
                report_error = Some(err);
            }
        }
        if !outcome.conflicts.is_empty() {
            guard.logs.push(format!(
                "판독 마크 {}건은 다른 창에서 먼저 바뀌어 적용하지 않았습니다 — 뷰어에서 확인하십시오.",
                outcome.conflicts.len()
            ));
        }
    }
    let mut body: serde_json::Value = serde_json::from_str(&import_marks_response(
        &marks,
        &outcome,
        None,
        refresh_report && report_error.is_none(),
    ))
    .unwrap_or_else(|_| serde_json::json!({"ok": true}));
    if let Some(err) = report_error {
        body["report_error"] = serde_json::Value::String(err);
    }
    body.to_string()
}

/// Reviewer name as typed in the finalize dialog: trimmed, control
/// characters dropped, capped. It is recorded, never authenticated.
pub(crate) fn sanitize_examiner(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(120)
        .collect()
}

/// Where an in-progress finalize keeps its sign-off until the report
/// rendered from it succeeded; only then is it promoted to
/// `review/signoff.json` (SHOULD-FIX 6), and taken back again when the
/// package then fails (R11-S3).
pub(crate) const SIGNOFF_PENDING_REL_PATH: &str = "review/signoff.pending.json";

/// Writes a schema-v4 sign-off record (`report::Signoff::build_v4`)
/// atomically to `rel_path`. It binds the examiner to the current review
/// marks, their IN-OUT ranges and the tag map (`annotations_sha256`), the
/// SHA-256 of every annotated record's target file and the head of every
/// audit lane (`content_sha256`), so a later make-report / verify-package
/// can say whether anything it covers changed after the sign-off.
pub(crate) fn write_signoff(
    case_dir: &Path,
    rel_path: &str,
    examiner: &str,
) -> Result<crate::report::Signoff, String> {
    let signed_unix = crate::util::now_unix()? as i64;
    // A read failure refuses the sign-off (finding 8): binding it to an
    // empty mark list would misstate what the examiner signed.
    let signoff =
        crate::report::Signoff::build_v4(case_dir, examiner, signed_unix).map_err(|err| {
            format!("최종화 차단: 검토자 기록에 묶을 판독 내용을 읽지 못했습니다 — {err}")
        })?;
    let path = case_dir.join(rel_path);
    crate::util::write_text_atomic(&path, &signoff.text)
        .map_err(|err| format!("검토자 기록 저장 실패 {}: {err}", path.display()))?;
    Ok(signoff)
}

#[cfg(test)]
thread_local! {
    /// Sign-offs handed to the audit hook on this thread (tests, R4-B18).
    pub(crate) static SIGNOFF_AUDIT_CALLS: std::cell::RefCell<Vec<(PathBuf, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Test override: the error the audit hook returns on this thread.
    pub(crate) static SIGNOFF_AUDIT_FAIL: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// R8-I9: records that a finalize abandoned the sign-off entry it had
/// already chained (the finalize is already failing; the sign-off is
/// simply never promoted).
///
/// R9-S5: a failure to record it was only a job log line, and the error
/// the finalize returned did not say that the audit trail now shows an
/// unfinished finalize's sign-off as the latest one. It returns that
/// explanation (also logged) for the caller to append to its error.
fn abandon_signoff(
    case_dir: &Path,
    signoff_text: &str,
    reason: &str,
    state: &SharedState,
) -> Option<String> {
    let err = crate::report::record_signoff_abandoned(case_dir, signoff_text, reason).err()?;
    let note = abandon_failure_note(&err);
    state_lock(state).logs.push(format!("경고: {note}"));
    Some(note)
}

/// R9-S5: what a failed abandon record means for the examiner.
fn abandon_failure_note(err: &str) -> String {
    format!(
        "또한 이 검토자 기록을 감사 로그에 '완료되지 않음'으로 남기지 못했습니다 ({err}). 감사 로그(evidence/logs/signoff-log.jsonl)에는 끝나지 않은 이 최종화의 검토자 기록이 마지막 기록으로 남아 있어, 이후 보고서·검증이 그것을 최신 검토자 기록으로 볼 수 있습니다. 감사 로그 폴더를 쓸 수 있는지 확인한 뒤 최종화를 다시 실행하면 새 검토자 기록이 그 뒤에 남습니다."
    )
}

#[cfg(test)]
thread_local! {
    /// R10-S1 test hook: how many sign-off promotion renames fail (as an
    /// antivirus / indexer handle would) before one is attempted for real.
    pub(crate) static FAIL_SIGNOFF_PROMOTIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// R10-S1: promotes the pending sign-off to `review/signoff.json`,
/// retried for the rename budget. R9 tried once: a handle an antivirus or
/// indexer held on the fresh file for a moment failed the finalize.
/// R11-S7: a missing pending file fails at once, and the wait is logged.
///
/// R11-S3: this runs BEFORE package-case (R10 promoted after publishing,
/// so a failed promotion left a published package with an unpromoted
/// sign-off: the `…_INCOMPLETE` packages, which finalize no longer makes).
fn promote_signoff(pending: &Path, target: &Path, state: &SharedState) -> std::io::Result<()> {
    retry_logged(state, "검토자 기록 확정", &mut || {
        #[cfg(test)]
        if FAIL_SIGNOFF_PROMOTIONS.with(|left| {
            let n = left.get();
            left.set(n.saturating_sub(1));
            n > 0
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected: file in use",
            ));
        }
        std::fs::rename(pending, target)
    })
}

/// R4-B18 call site for track D's `record_signoff_audit(case_dir,
/// &Signoff)` (D13: chains the sign-off into `signoff-log.jsonl`, keyed
/// when an audit key is configured).
///
/// Parses `signoff_json` (the exact pending `signoff.json` text) into
/// `report::Signoff` and appends it with `report::record_signoff_audit`.
pub(crate) fn record_signoff_audit(case_dir: &Path, signoff_json: &str) -> Result<(), String> {
    #[cfg(test)]
    {
        SIGNOFF_AUDIT_CALLS.with(|calls| {
            calls
                .borrow_mut()
                .push((case_dir.to_path_buf(), signoff_json.to_string()))
        });
        if let Some(err) = SIGNOFF_AUDIT_FAIL.with(|fail| fail.borrow().clone()) {
            return Err(err);
        }
    }
    // D13: chain the exact record bytes (any schema) into
    // `evidence/logs/signoff-log.jsonl` via track D's public function.
    let signoff = crate::report::Signoff::parse(signoff_json)
        .ok_or_else(|| "검토자 기록이 올바르지 않습니다 (검토자 이름 없음)".to_string())?;
    crate::report::record_signoff_audit(case_dir, &signoff)
}

/// SHA-256 of the package's `manifest.sha256`: one value an examiner can
/// write down to pin the whole packaged file list.
pub(crate) fn package_manifest_sha256(package_dir: &Path) -> Option<String> {
    audit::digest_file(&package_dir.join("manifest.sha256")).ok()
}

pub(crate) fn api_finalize(request: &Request, state: &SharedState) -> String {
    finalize_with_hooks(request, state, &|| {})
}

/// [`api_finalize`] with a hook between the first read of the bound case
/// and taking `busy` (tests switch the case there, R4-B11).
pub(crate) fn finalize_with_hooks(
    request: &Request,
    state: &SharedState,
    before_busy: &dyn Fn(),
) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let examiner = sanitize_examiner(&body_value(&request.body, "examiner").unwrap_or_default());
    if examiner.is_empty() {
        return api_err("검토자 이름을 입력하십시오 (보고서에 입력값 그대로 기록됩니다).");
    }
    before_busy();
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
    // R4-B11: the case the examiner confirmed must still be the bound one
    // now that `busy` (which open-case respects) is held.
    if state_lock(state).case_dir.as_deref() != Some(case_dir.as_path()) {
        return api_err(CASE_SWITCHED_ERROR);
    }
    // UX-09 / S1: clip exports, export-selected and frame captures run
    // without `busy`; sealing while one is still writing into the case
    // would package a torn or missing artifact. The check and the seal
    // flag (which refuses new write ops) share one lock.
    let _seal = match try_begin_seal(state) {
        Ok(seal) => seal,
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
    finalize_with_exe(&exe, &case_dir, &examiner, state)
}

/// Finalize body, parameterised on the `frametrace` executable so tests can
/// drive the real CLI (the test binary itself is not a frametrace CLI).
/// The caller holds the busy guard.
pub(crate) fn finalize_with_exe(
    exe: &Path,
    case_dir: &Path,
    examiner: &str,
    state: &SharedState,
) -> String {
    // R16 (track I, I5): make-report --rehash refuses to start when a lane
    // its scan appends to (sign-off, anomaly, re-verification) cannot take
    // an append (a torn last line). R17-S5: checked FIRST, read-only, so
    // such a case is refused before anything at all is written — no
    // recovery of an earlier journal, no superseded entry (R16 ran the
    // check after them), no begin entry, journal or local record.
    // R18-S6: in the finalize's own words (R17 said "이상 징후 검사를
    // 시작하지 않았습니다" here).
    if let Err((rel, err)) = crate::anomaly::scan_lanes_appendable(case_dir, true) {
        return finalize_refused(
            state,
            format!(
                "최종화 차단: {rel}에 이어 쓸 수 없음: {err} — 최종화는 아무것도 쓰지 않았습니다 (그 감사 로그 파일이 읽기 전용이거나 다른 프로그램이 잡고 있지 않은지, 마지막 줄이 찢기지 않았는지 확인하십시오)."
            ),
        );
    }
    // R12-S2: an earlier finalize of this case that was interrupted (its
    // journal is still on disk) is cleaned up first — the caller holds
    // `busy`, so it is not this workstation's live finalize.
    if super::finalize_journal::journal_path(case_dir).exists()
        && let Some(recovery) =
            super::finalize_journal::recover(case_dir, state, crate::package::rename_retry_budget())
    {
        let blocking = recovery.blocking;
        let set_aside = recovery.set_aside;
        let detail = recovery.detail.clone();
        apply_recovery(state, recovery);
        // Only a stale report is left for this finalize to cure; anything
        // else (a sign-off not taken back, no abandon record, R13: an
        // unpaired journal, another PC's recent one) would be overwritten
        // by a new journal — refused.
        if blocking {
            return api_err(&format!(
                "최종화 차단: 이전에 중단된 최종화를 정리하지 못했습니다 — {detail}"
            ));
        }
        // R13-S5: another PC's journal older than 24 h is moved aside
        // (never recovered here, never deleted). R14-S8: the package that
        // finalize reserved is marked beside it (never renamed).
        if set_aside {
            let marked = match super::finalize_journal::read_journal(case_dir) {
                Ok(Some(journal)) => {
                    super::finalize_journal::mark_other_host_package(case_dir, &journal)
                }
                _ => None,
            };
            match super::finalize_journal::set_aside(case_dir, "other-host") {
                Ok(path) => {
                    let mut guard = state_lock(state);
                    guard.logs.push(format!(
                        "다른 PC의 오래된 최종화 저널을 {}(으)로 옮겼습니다 (복구하지 않음).",
                        path.display()
                    ));
                    if let Some(note) = marked {
                        guard.logs.push(format!("경고: {note}"));
                    }
                }
                Err(err) => {
                    return api_err(&format!(
                        "최종화 차단: 다른 PC의 최종화 저널을 옮기지 못했습니다 — {err}"
                    ));
                }
            }
        }
        // A journal of this PC kept only for its stale report: this
        // finalize re-renders the report, so its begin entry is closed.
        match super::finalize_journal::assess(case_dir) {
            None => {}
            Some(super::finalize_journal::Assessment::Open(journal, _)) => {
                // R15 (P21): `resolved` before the journal goes, then removed.
                // R17-S4 (P28): a record that cannot advance loses its
                // `open` file; when that fails too the journal stays.
                let resolved = match crate::report::record_finalize_resolved(
                    case_dir,
                    &journal.finalize_id,
                    Some(&journal.signoff_sha256),
                    "superseded",
                ) {
                    Ok(()) => {
                        match super::finalize_journal::advance_or_drop(
                            &journal,
                            case_dir,
                            super::finalize_journal::LOCAL_RESOLVED,
                            None,
                        ) {
                            Ok(super::finalize_journal::Settled::Advanced) => {}
                            Ok(super::finalize_journal::Settled::Dropped) => {
                                state_lock(state).logs.push(format!(
                                    "경고: {}",
                                    super::finalize_journal::dropped_note(&journal.finalize_id)
                                ));
                            }
                            Err(err) => {
                                return api_err(&format!(
                                    "최종화 차단: 이전 최종화를 이 PC의 로컬 기록에 '정리됨'으로 남기지 못했고 열린 기록도 지우지 못했습니다 — {err} (로컬 설정 폴더에 쓸 수 있는지 확인한 뒤 다시 시도하십시오)"
                                ));
                            }
                        }
                        true
                    }
                    Err(err) => {
                        state_lock(state).logs.push(format!("경고: {err}"));
                        false
                    }
                };
                if let Err(err) =
                    super::finalize_journal::remove_journal(case_dir, &journal.finalize_id)
                {
                    return api_err(&format!(
                        "최종화 차단: 이전 최종화 저널을 지우지 못했습니다 — {err}"
                    ));
                }
                if resolved {
                    let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
                }
            }
            // R14: anything else still there (a finished finalize whose
            // complete entry could not be written, a journal whose removal
            // failed) would make this finalize's exclusive journal fail.
            Some(_) => {
                return api_err(&format!(
                    "최종화 차단: 이전 최종화 저널이 남아 있습니다 — {detail}"
                ));
            }
        }
    }
    // Never seal without the latest marks (SHOULD-FIX 5): marks queued on
    // disk by an earlier enrichment run are applied first, and a failure
    // blocks the finalize instead of packaging a stale review.
    {
        let _gate = marks_gate();
        if let Err(err) = drain_pending_marks(state, case_dir) {
            return api_err(&format!(
                "최종화 차단: 저장 대기 중인 판독 마크를 케이스에 반영하지 못했습니다 — {err}"
            ));
        }
    }
    // §10.2: a queued save that met another window's change is an open
    // decision — sealing now would sign one side of it unseen.
    let open_conflicts = unresolved_queue_conflict_count(case_dir);
    if open_conflicts > 0 {
        return api_err(&format!(
            "최종화 차단: 저장 충돌 {open_conflicts}건을 먼저 해결하십시오 (뷰어의 충돌 목록에서 서버 값 또는 내 값을 선택해 저장)."
        ));
    }
    let case_text = case_dir.to_string_lossy().to_string();
    {
        // `busy` belongs to the caller's guard: setting or clearing it here
        // would release it while that guard is still alive.
        let mut guard = state_lock(state);
        guard.phase = "finalizing";
        guard.error = None;
        guard.logs.push("리뷰 데이터 갱신 중…".into());
    }
    // §10.2: re-render the review bundle from the database being signed,
    // so the packaged review/data-bundle.js carries every mark saved since
    // the last analysis (it is otherwise only rebuilt by a pipeline run).
    if let Err(err) = run_step(exe, &["make-review".into(), case_text.clone()], state) {
        let message = format!(
            "최종화 차단: 리뷰 데이터(review/data-bundle.js)를 갱신하지 못했습니다 — {err}"
        );
        let mut guard = state_lock(state);
        guard.phase = "review-ready";
        guard.error = Some(message.clone());
        return api_err(&message);
    }
    // The sign-off is written to a pending file that make-report reads
    // (--signoff) so the report names the reviewer, and is promoted to
    // review/signoff.json after the report succeeded and BEFORE the package
    // is written (R11-S3); a package failure restores the previous record,
    // so a failed finalize never leaves a sign-off behind (SHOULD-FIX 6).
    let pending_signoff = case_dir.join(SIGNOFF_PENDING_REL_PATH);
    let signoff_target = case_dir.join(crate::report::SIGNOFF_REL_PATH);
    let signoff = match write_signoff(case_dir, SIGNOFF_PENDING_REL_PATH, examiner) {
        Ok(signoff) => signoff,
        Err(err) => return finalize_refused(state, err),
    };
    // R11-S3 / R12-S3: the record the promotion will replace is read
    // BEFORE anything durable happens (it goes into the journal, fsync'd —
    // R11 kept it in memory only); one that cannot be kept refuses here.
    let previous = match read_previous_signoff(&signoff_target) {
        Ok(previous) => previous,
        Err(err) => {
            let _ = std::fs::remove_file(&pending_signoff);
            return finalize_refused(
                state,
                format!(
                    "검토자 기록 확정 실패: {err} — 최종화는 끝나지 않았고 패키지는 만들지 않았습니다. 원인(파일을 잡고 있는 프로그램 등)을 해결한 뒤 최종화를 다시 실행하십시오."
                ),
            );
        }
    };
    // R12-S6: a "previous" record that was itself an abandoned finalize's
    // (its restore failed earlier) is never restored as if confirmed.
    // R13 I2 (P16): one whose lane entry a lane-repair follows is
    // "unknown" — never restored as current either (`previous_repaired`).
    let previous_lane = previous
        .as_deref()
        .map(crate::sha256::digest_bytes)
        .and_then(|sha| crate::report::signoff_lane_state_sha(case_dir, &sha).ok());
    let previous_abandoned = previous_lane.is_some_and(|lane| lane.abandoned);
    let previous_repaired = previous_lane.is_some_and(|lane| lane.repaired_after);
    // R13-S1/S2 (P15): the ONE reports/ name this finalize's package-case
    // may publish, recorded with the rest in the lane's begin entry.
    let finalize_id = match new_finalize_id() {
        Ok(id) => id,
        Err(err) => {
            let _ = std::fs::remove_file(&pending_signoff);
            return finalize_refused(state, err);
        }
    };
    let journal = super::finalize_journal::FinalizeJournal {
        finalize_id,
        host_id: crate::util::host_id(),
        writer: crate::util::current_writer(),
        writer_image_sha256: super::finalize_journal::own_exe_sha256(),
        started_unix: crate::util::now_unix().unwrap_or(0),
        examiner: examiner.to_string(),
        signoff_sha256: signoff.record_sha256(),
        previous_signoff_sha256: previous.as_deref().map(crate::sha256::digest_bytes),
        package_name: choose_package_name(case_dir),
        state: super::finalize_journal::STATE_IN_PROGRESS.to_string(),
    };
    let refuse_unjournaled = |err: String, state: &SharedState| {
        let _ = std::fs::remove_file(&pending_signoff);
        finalize_refused(
            state,
            format!(
                "최종화 차단: {err} — 중단되면 되돌릴 기록이 없어 최종화하지 않았습니다 (review 폴더와 감사 로그 폴더를 쓸 수 있는지 확인하십시오)."
            ),
        )
    };
    // (R16-I5 / R17-S5: the scan-lane preflight ran first of all, above.)
    // R14 (P18): this PC's local open-finalize record first — outside the
    // case folder, so a copy or share of the case never carries it; a
    // recovery acts only where it agrees with the journal and the lane.
    if let Err(err) = journal
        .local_record(case_dir, super::finalize_journal::LOCAL_OPEN)
        .write()
    {
        return refuse_unjournaled(err, state);
    }
    // Undoes what was written so far when a later preparation step fails
    // (nothing of the sign-off has been chained, rendered or promoted).
    let unwind = |journal_created: bool| {
        if journal_created {
            let _ = super::finalize_journal::remove_journal(case_dir, &journal.finalize_id);
        }
        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
    };
    // R13 (P15): the keyed begin entry — a journal is acted on only when
    // it pairs with it — then (R12-S2, P13) the journal, both before the
    // sign-off is chained, rendered or promoted, so an interruption
    // anywhere after this point is found and cleaned up on the next start
    // / open-case.
    if let Err(err) = crate::report::record_finalize_begin(case_dir, &journal.begin_entry()) {
        unwind(false);
        return refuse_unjournaled(err, state);
    }
    let refuse_begun = |err: String, journal_created: bool, state: &SharedState| {
        let _ = crate::report::record_finalize_resolved(
            case_dir,
            &journal.finalize_id,
            Some(&journal.signoff_sha256),
            "refused",
        );
        unwind(journal_created);
        refuse_unjournaled(err, state)
    };
    // R14-S4: created exclusively — another finalize's journal (another
    // PC on a shared case) is never overwritten.
    if let Err(err) = journal.create_exclusive(case_dir) {
        return refuse_begun(err, false, state);
    }
    // R13-S11: the previous record's bytes beside the journal (fsync'd) —
    // R14-S4: only once the journal is this finalize's, so a concurrent
    // finalize's copy is never overwritten.
    if let Err(err) = super::finalize_journal::write_previous_signoff(case_dir, previous.as_deref())
    {
        return refuse_begun(err, true, state);
    }
    // R4-B18 / D13: the sign-off is chained into the audit trail right
    // after it is written and BEFORE make-report and package-case, so the
    // packaged report and verify-package find it in `signoff-log.jsonl`.
    // A failed append fails the finalize (a seal whose sign-off is not in
    // the chain would claim more than it can prove). If report or package
    // fails later, the chained entry stays (append-only) but the sign-off
    // is never left promoted (abandoned in the chain, R8-I9).
    if let Err(err) = record_signoff_audit(case_dir, &signoff.text) {
        let _ = std::fs::remove_file(&pending_signoff);
        let _ = crate::report::record_finalize_resolved(
            case_dir,
            &journal.finalize_id,
            Some(&journal.signoff_sha256),
            "refused",
        );
        unwind(true);
        return finalize_refused(
            state,
            format!("검토자 기록을 감사 로그에 남기지 못했습니다: {err}"),
        );
    }
    #[cfg(test)]
    if STOP_FINALIZE_AT.with(|stop| stop.get()) == Some(FinalizeStop::Chain) {
        return "{\"ok\":false,\"error\":\"test: stopped after the chain append\"}".to_string();
    }
    state_lock(state).logs.push("결과 보고서 생성 중…".into());
    // --rehash makes the packaged report re-digest every indexed source file
    // and flag hash-revalidation mismatches — the integrity claim behind a
    // finalized package should rest on live digests, not stored scan hashes.
    let report = run_step(
        exe,
        &[
            "make-report".into(),
            case_text.clone(),
            "--rehash".into(),
            "--signoff".into(),
            pending_signoff.to_string_lossy().to_string(),
        ],
        state,
    );
    // R11-S3: the sign-off is promoted BEFORE the package is written, so
    // the package carries the promoted record and a published package is
    // a completed finalize. Now a failure at any point leaves no package
    // that reads as final (R12-S1: one already published is marked).
    let mut promoted = false;
    let promotion: Result<(), String> = match &report {
        Ok(_) => match promote_signoff(&pending_signoff, &signoff_target, state) {
            Ok(()) => {
                promoted = true;
                Ok(())
            }
            Err(err) => Err(format!(
                "검토자 기록 확정 실패: {}: {err} — 최종화는 끝나지 않았고 패키지는 만들지 않았습니다. 원인(파일을 잡고 있는 프로그램 등)을 해결한 뒤 최종화를 다시 실행하십시오.",
                signoff_target.display()
            )),
        },
        Err(err) => Err(err.clone()),
    };
    #[cfg(test)]
    if promoted && STOP_FINALIZE_AT.with(|stop| stop.get()) == Some(FinalizeStop::Promotion) {
        return "{\"ok\":false,\"error\":\"test: stopped after the promotion\"}".to_string();
    }
    let packaging = match &promotion {
        Ok(()) => run_package_step(exe, case_dir, &case_text, &journal.package_name, state),
        Err(err) => Err(err.clone()),
    };
    #[cfg(test)]
    if STOP_FINALIZE_AT.with(|stop| stop.get()) == Some(FinalizeStop::Package) {
        return "{\"ok\":false,\"error\":\"test: stopped after package-case\"}".to_string();
    }
    // R15 (P21): package-case succeeded — this PC's local record says so
    // right away, with the SHA-256 of the manifest it left (the only proof
    // a later recovery accepts that the finalize got past its package; the
    // case folder's own contents can be forged). A record that cannot be
    // advanced stays `open`: a crash before the complete entry then rolls
    // the finalize back (safe side).
    // R16 (P24): the `packaged` state is its own file (created, never a
    // rewrite — a program holding `<id>.json` cannot stop it); R16-S7: the
    // manifest is hashed streaming (any size).
    if packaging.is_ok() {
        let manifest = super::finalize_journal::published_reserved(case_dir, &journal.package_name)
            .and_then(|dir| package_manifest_sha256(&dir));
        if let Err(err) =
            journal.advance_local(case_dir, super::finalize_journal::LOCAL_PACKAGED, manifest)
        {
            state_lock(state).logs.push(format!(
                "경고: 열린 최종화 로컬 기록을 갱신하지 못했습니다: {err}"
            ));
        }
    }
    #[cfg(test)]
    if STOP_FINALIZE_AT.with(|stop| stop.get()) == Some(FinalizeStop::Packaged) {
        return "{\"ok\":false,\"error\":\"test: stopped after the packaged record\"}".to_string();
    }
    // R10-S3: the package this finalize's package-case published, from
    // its own `output:` line (R9 marked the folder with the newest mtime).
    let published = packaging
        .as_ref()
        .ok()
        .and_then(|out| super::media::published_package_dir(case_dir, out));
    // R9-S5: why the abandon record could not be written, if it failed.
    let mut abandon_note: Option<String> = None;
    // R11-S5/S6: what a failed finalize left that must not read as final.
    let mut leftover: Vec<String> = Vec::new();
    // R12-S2: something the journal must keep for a later recovery.
    let mut unresolved = false;
    // ... and something other than the stale report.
    let mut blocking_leftover = false;
    // R12-S6: `Some(stale)` once the re-render ran (R11 never cleared it).
    let mut rerendered_stale: Option<bool> = None;
    let mut incomplete_dirs: Vec<PathBuf> = Vec::new();
    if let Err(reason) = &packaging {
        // R12-S1 (P14): a package this finalize's package-case published
        // (killed or failing after its publish rename) or kept under its
        // work-folder name is renamed `…_INCOMPLETE` / marked beside —
        // R11 left it plainly named, and it verified as a final package.
        // R13-S2: only this finalize's reserved name (and the folder its
        // own `output:` line names). R13-S8: a folder that could not be
        // marked (kept work folder included) keeps the journal, blocking.
        let marked = super::finalize_journal::mark_unfinished_packages(
            case_dir,
            &journal.package_name,
            reason,
            "finalize failed after package-case published it",
            crate::package::rename_retry_budget(),
            None,
        );
        leftover.extend(marked.notes);
        incomplete_dirs = marked.dirs;
        if marked.failed
            || super::finalize_journal::published_reserved(case_dir, &journal.package_name)
                .is_some()
        {
            unresolved = true;
            blocking_leftover = true;
        }
        // R8-I9: the sign-off entry is already in the chain; mark it
        // abandoned so it never reads as the case's latest sign-off.
        abandon_note = abandon_signoff(case_dir, &signoff.text, reason, state);
        if abandon_note.is_some() {
            unresolved = true;
            blocking_leftover = true;
        }
        let _ = std::fs::remove_file(&pending_signoff);
        // R11-S3: a promoted sign-off of a finalize whose package failed
        // is taken back (the previous record restored, or none). R12-S4:
        // never cancellable; R12-S6: an abandoned previous record is not
        // restored; R12-S7: written back durably.
        if promoted {
            if previous_repaired {
                // R13 I2: kept for the examiner beside the kept journal.
                unresolved = true;
                blocking_leftover = true;
                leftover.push(format!(
                    "이전 검토자 기록은 서명 감사 레인이 그 항목 뒤에서 수리되어(lane-repair) 확정 여부를 알 수 없어 되돌리지 않았습니다 — 보관본 {}을 확인하십시오.",
                    super::finalize_journal::PREVIOUS_SIGNOFF_REL_PATH
                ));
            }
            match super::finalize_journal::restore_previous(
                &signoff_target,
                previous.as_deref().filter(|_| !previous_repaired),
                previous_abandoned,
                state,
                crate::package::rename_retry_budget(),
            ) {
                Ok(None) => {}
                Ok(Some(note)) => leftover.push(note),
                Err(err) => {
                    unresolved = true;
                    blocking_leftover = true;
                    leftover.push(format!(
                        "{}에 끝나지 않은 이번 최종화의 검토자 기록이 남아 있습니다 (이전 상태로 되돌리지 못함: {err}) — 감사 로그에는 '완료되지 않음'으로 기록되어 최신 검토자 기록으로 쓰이지 않지만, 최종화를 다시 실행해 새 기록으로 바꾸십시오.",
                        signoff_target.display()
                    ));
                }
            }
        }
        // The report on disk was rendered with the abandoned sign-off;
        // re-render it from the restored state so it does not name a
        // reviewer for a finalize that never completed. R11-S5: a failed
        // re-render is part of the error (and `report_stale`). R12-S4: a
        // cleanup step — a cancel that stopped the package does not kill it.
        if report.is_ok() {
            match run_step_cancellable(
                exe,
                &["make-report".into(), case_text.clone()],
                &|| false,
                state,
            ) {
                Ok(_) => rerendered_stale = Some(false),
                Err(rerender) => {
                    rerendered_stale = Some(true);
                    unresolved = true;
                    leftover.push(format!(
                        "보고서를 다시 만들지 못했습니다 — reports/case-report.html이 확정되지 않은 이번 검토자 기록으로 만들어진 채 남아 있습니다 (최종 결과로 쓰지 마십시오): {rerender}"
                    ));
                }
            }
        }
        // R12-S2: the journal stays only while something is left for a
        // later recovery to redo.
        // Kept, its state says whether only the stale report is left.
        let journal_result = if unresolved {
            let only_report = rerendered_stale == Some(true) && !blocking_leftover;
            journal.rewrite_as(
                if only_report {
                    super::finalize_journal::STATE_RECOVERED
                } else {
                    super::finalize_journal::STATE_INTERRUPTED
                },
                case_dir,
            )
        } else {
            // R13 (P15): cleaned up — the begin entry is closed first.
            // R14 (P18): the local record goes once the lane says so;
            // R15 (P21): rewritten `resolved` before the journal goes.
            // R17-S4 (P28): a record that cannot advance loses its `open`
            // file; when that fails too the journal is KEPT (R16 removed it
            // with the record still `open`).
            let mut settled = Ok(());
            let resolved = match crate::report::record_finalize_resolved(
                case_dir,
                &journal.finalize_id,
                Some(&journal.signoff_sha256),
                "failed",
            ) {
                Ok(()) => {
                    match super::finalize_journal::advance_or_drop(
                        &journal,
                        case_dir,
                        super::finalize_journal::LOCAL_RESOLVED,
                        None,
                    ) {
                        Ok(super::finalize_journal::Settled::Advanced) => {}
                        Ok(super::finalize_journal::Settled::Dropped) => {
                            state_lock(state).logs.push(format!(
                                "경고: {}",
                                super::finalize_journal::dropped_note(&journal.finalize_id)
                            ));
                        }
                        Err(err) => settled = Err(err),
                    }
                    true
                }
                Err(err) => {
                    state_lock(state).logs.push(format!("경고: {err}"));
                    false
                }
            };
            match settled {
                Err(err) => {
                    let detail = format!(
                        "이 PC의 최종화 로컬 기록에 '정리됨'을 남기지 못했고 열린 기록도 지우지 못해({err}) 최종화 저널을 남겼습니다 — 로컬 설정 폴더에 쓸 수 있는지 확인한 뒤 케이스를 다시 여십시오."
                    );
                    super::finalize_journal::remember_complete_failure(
                        &journal.finalize_id,
                        &detail,
                    );
                    leftover.push(detail);
                    Ok(())
                }
                Ok(()) => {
                    let removed =
                        super::finalize_journal::remove_journal(case_dir, &journal.finalize_id);
                    if removed.is_ok() && resolved {
                        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
                    }
                    removed
                }
            }
        };
        if let Err(err) = journal_result {
            state_lock(state).logs.push(format!(
                "경고: 최종화 저널을 쓰거나 지우지 못했습니다: {err}"
            ));
        }
    }
    // Every disk read happens before the state lock is taken.
    let finished = match &packaging {
        Ok(_) => {
            let manifest_sha256 = published.as_deref().and_then(package_manifest_sha256);
            // R10-S2: packages earlier finalizes left incomplete are named
            // (never deleted: the examiner decides what to keep).
            let leftovers = super::media::incomplete_package_dirs(case_dir);
            // R13-S3 (P15): the lane records the completion BEFORE the
            // journal goes, so a journal whose removal fails is only
            // deleted by the next recovery (R12 then treated the completed
            // finalize as interrupted and marked its package incomplete).
            #[cfg(test)]
            let injected = FAIL_FINALIZE_COMPLETE.with(|cell| cell.get());
            #[cfg(not(test))]
            let injected = false;
            let complete_error = if injected {
                Some("test: injected finalize-complete failure".to_string())
            } else {
                crate::report::record_finalize_complete(
                    case_dir,
                    &journal.finalize_id,
                    &journal.signoff_sha256,
                    &journal.package_name,
                    manifest_sha256.as_deref(),
                )
                .err()
            };
            // R12-S2: the finalize completed — its journal goes, and (R14
            // P18) its local record. R14-S2: when the lane did not take the
            // complete entry both are kept as `completed-unrecorded` (R13
            // removed the journal anyway, leaving the begin entry open for
            // good) and the next recovery writes the entry.
            let journal_left = match &complete_error {
                None => {
                    // R15 (P21): `completed` BEFORE the journal goes — R14
                    // left the record `open` when its removal (the last
                    // step) failed, and a cut complete entry plus a rebuilt
                    // journal then rolled the good finalize back (R14
                    // reviewer PoC A). R16 (P24): `completed` is its own
                    // file, created beside the others (R15 rewrote
                    // `<id>.json`, which a program holding it without
                    // delete sharing stopped — R15 reviewer PoC). When it
                    // cannot be created the journal is KEPT as
                    // `completed-unrecorded` (the next recovery records it
                    // here; the lane already holds the complete entry).
                    // R17-S1 (P28): when neither `packaged` nor `completed`
                    // could be created (a full disk) the `open` file is
                    // deleted on the spot — the record is then gone (a
                    // rebuilt journal is "로컬 기록 없음", never rolled
                    // back) and the journal goes; R16 left the record
                    // `open` beside a `completed-unrecorded` journal, and a
                    // cut lane tail rolled the good finalize back. Only when
                    // that deletion fails too is the journal kept.
                    match super::finalize_journal::advance_or_drop(
                        &journal,
                        case_dir,
                        super::finalize_journal::LOCAL_COMPLETED,
                        manifest_sha256.clone(),
                    )
                    .map(|settled| {
                        if settled == super::finalize_journal::Settled::Dropped {
                            state_lock(state).logs.push(format!(
                                "경고: 최종화는 끝났습니다. {}",
                                super::finalize_journal::dropped_note(&journal.finalize_id)
                            ));
                        }
                    }) {
                        Err(err) => {
                            state_lock(state).logs.push(format!(
                                "경고: 최종화는 끝났지만 이 PC의 최종화 로컬 기록에 'completed'를 남기지 못했습니다 ({err}) — 최종화 저널을 'completed-unrecorded'로 남겼고, 다음에 케이스를 열 때 다시 남깁니다."
                            ));
                            journal
                                .rewrite_as(
                                    super::finalize_journal::STATE_COMPLETED_UNRECORDED,
                                    case_dir,
                                )
                                .err()
                        }
                        Ok(()) => {
                            let left = super::finalize_journal::remove_journal(
                                case_dir,
                                &journal.finalize_id,
                            )
                            .err();
                            if left.is_none()
                                && let Err(err) = super::finalize_journal::remove_local_record(
                                    &journal.finalize_id,
                                )
                            {
                                state_lock(state).logs.push(format!(
                                    "경고: 최종화는 끝났지만 이 PC의 최종화 로컬 기록을 다 지우지 못했습니다 ({err}) — 'completed'로 남아 있어 복구 대상이 아니며, 서버를 다시 시작하면 30일 뒤 정리됩니다."
                                ));
                            }
                            left
                        }
                    }
                }
                Some(lane) => {
                    // R17-S1 (P28): `completed-unrecorded` not creatable
                    // with the record still `open` → the `open` file goes
                    // (the kept journal then reads "로컬 기록 없음": never
                    // rolled back, never recorded complete — dismiss it).
                    if let Ok(super::finalize_journal::Settled::Dropped) =
                        super::finalize_journal::advance_or_drop(
                            &journal,
                            case_dir,
                            super::finalize_journal::STATE_COMPLETED_UNRECORDED,
                            None,
                        )
                    {
                        state_lock(state).logs.push(format!(
                            "경고: {} 남은 최종화 저널은 '짝 없음(로컬 기록 없음)'으로 보이며, 이 PC는 그 완료를 다시 기록하지 않습니다 — review/signoff.json과 이번 패키지를 확인한 뒤 저널을 옮기십시오.",
                            super::finalize_journal::dropped_note(&journal.finalize_id)
                        ));
                    }
                    journal
                        .rewrite_as(
                            super::finalize_journal::STATE_COMPLETED_UNRECORDED,
                            case_dir,
                        )
                        .err()
                        .map(|err| format!("{err}; {lane}"))
                }
            };
            if let Some(err) = &complete_error {
                state_lock(state).logs.push(format!(
                    "경고: 최종화는 끝났지만 그 완료를 서명 감사 레인에 남기지 못했습니다 ({err}) — 최종화 저널을 'completed-unrecorded'로 남겼고, 다음에 케이스를 열 때 완료 기록을 다시 남깁니다."
                ));
            }
            Some((manifest_sha256, leftovers, journal_left))
        }
        Err(_) => None,
    };
    let mut guard = state_lock(state);
    match (packaging, finished) {
        (Ok(_), Some((manifest_sha256, leftovers, journal_left))) => {
            let package_dir = published;
            let package_url = package_dir
                .as_ref()
                .and_then(|path| path.strip_prefix(case_dir).ok())
                .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            guard.phase = "done";
            guard.byte_progress = None;
            guard.package_dir = package_dir.clone();
            // R11-S5/S6: this finalize rendered the report and completed.
            guard.report_stale = false;
            guard.finalize_incomplete = None;
            // R12-S2: so did whatever an interrupted one left.
            guard.finalize_recovered = None;
            if let Some(err) = journal_left {
                guard.logs.push(format!(
                    "경고: 최종화는 끝났지만 최종화 저널을 지우지 못했습니다 ({err}) — 서명 감사 레인에 완료가 기록되어 있으면 다음에 이 케이스를 열 때 저널만 지워집니다."
                ));
            }
            if package_dir.is_none() {
                guard.logs.push(
                    "경고: package-case 출력에서 패키지 위치(output: 줄)를 찾지 못했습니다 — reports 폴더에서 이번 패키지를 직접 확인하십시오.".to_string(),
                );
            }
            guard.logs.push(format!(
                "보고서·패키지 생성 완료. 검토자(입력값 기록): {examiner}{}",
                manifest_sha256
                    .as_deref()
                    .map(|hash| format!(" · manifest.sha256 SHA-256 {hash}"))
                    .unwrap_or_default()
            ));
            if !leftovers.is_empty() {
                guard.logs.push(format!(
                    "참고: 이전 최종화가 끝나지 않아 남은 미완료 패키지 {}개가 reports 폴더에 있습니다 (자동으로 지우지 않음 — 최종 결과로 쓰지 마십시오): {}",
                    leftovers.len(),
                    leftovers
                        .iter()
                        .map(|dir| dir.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            serde_json::json!({
                "ok": true,
                "package_url": package_url,
                "package_dir": package_dir.as_ref().map(|dir| dir.to_string_lossy().to_string()),
                "examiner": examiner,
                "manifest_sha256": manifest_sha256,
                "incomplete_packages": leftovers
                    .iter()
                    .map(|dir| dir.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
                "report_stale": false,
            })
            .to_string()
        }
        (packaging, _) => {
            if let Some(note) = abandon_note {
                leftover.insert(0, note);
            }
            let error = std::iter::once(packaging.err().unwrap_or_default())
                .chain(leftover.iter().cloned())
                .collect::<Vec<_>>()
                .join(" ");
            guard.phase = "review-ready";
            guard.error = Some(error.clone());
            // R12-S6: a re-render that succeeded clears it (R11 only ever
            // set it).
            if let Some(stale) = rerendered_stale {
                guard.report_stale = stale;
            }
            // R11-S6: kept in the job state (and /api/status) until a
            // finalize completes, so a reload still shows it.
            guard.finalize_incomplete = (!leftover.is_empty()).then(|| leftover.join(" "));
            serde_json::json!({
                "ok": false,
                "error": error,
                "report_stale": guard.report_stale,
                "finalize_incomplete": guard.finalize_incomplete.is_some(),
                // R12-S1: packages this finalize published / kept, now
                // `…_INCOMPLETE` or marked beside.
                "incomplete_package_dirs": incomplete_dirs
                    .iter()
                    .map(|dir| dir.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            })
            .to_string()
        }
    }
}

/// A finalize refused before anything was promoted: back to review-ready
/// with the error shown.
fn finalize_refused(state: &SharedState, message: String) -> String {
    let mut guard = state_lock(state);
    guard.phase = "review-ready";
    guard.error = Some(message.clone());
    api_err(&message)
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FinalizeStop {
    Chain,
    Promotion,
    Package,
    /// R15: after the `packaged` local record was written.
    Packaged,
}

#[cfg(test)]
thread_local! {
    /// R12-S2 test hook: the finalize returns at this point as if the
    /// process had died there (nothing after it runs — no cleanup, the
    /// journal stays).
    pub(crate) static STOP_FINALIZE_AT: std::cell::Cell<Option<FinalizeStop>> =
        const { std::cell::Cell::new(None) };
    /// R12-S1 test hook: the package step's cancel ignores a package that
    /// is already published (the kill lands after the publish rename —
    /// the race the published check cannot close).
    pub(crate) static CANCEL_IGNORES_PUBLISH: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// R13 test hook: the base of the package name a finalize reserves
    /// on this thread (fake CLIs publish a fixed `package_1`).
    pub(crate) static PACKAGE_BASE_FOR_TEST: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
    /// R14-S2 test hook: the `finalize-complete` lane append fails.
    pub(crate) static FAIL_FINALIZE_COMPLETE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// R12-S1 (P14): package-case for a finalize. A cancel (`/api/cancel`, the
/// shutdown dialog's "작업 중단 후 서버 종료") stops it only until it has
/// published its package: once a new `reports/package_*` holding a
/// manifest exists, the cancel is deferred and the step runs to its end;
/// and an exit code observed as success is a success whatever arrived
/// meanwhile. A cancel that still lands after the publish rename (the
/// check and the kill are not atomic) is caught by the failure path,
/// which marks the published package incomplete.
fn run_package_step(
    exe: &Path,
    case_dir: &Path,
    case_text: &str,
    package_name: &str,
    state: &SharedState,
) -> Result<String, String> {
    let deferred = std::cell::Cell::new(false);
    let cancelled = || {
        if !state_lock(state).cancel_requested || deferred.get() {
            return false;
        }
        #[cfg(test)]
        if CANCEL_IGNORES_PUBLISH.with(|hook| hook.get()) {
            return true;
        }
        // Disk check outside the state lock (R13-S2: its reserved name).
        if super::finalize_journal::published_reserved(case_dir, package_name).is_none() {
            return true;
        }
        deferred.set(true);
        state_lock(state).logs.push(
            "중단 요청을 받았지만 패키지가 이미 게시되어 패키지 단계를 끝까지 진행합니다 (게시된 뒤 끊으면 이름만 평범한 미완료 패키지가 남습니다)."
                .to_string(),
        );
        false
    };
    let args = [
        "package-case".to_string(),
        case_text.to_string(),
        "--package-name".to_string(),
        package_name.to_string(),
    ];
    let run = run_step_detailed_opts(exe, &args, &cancelled, true, state)?;
    if run.partial {
        report_process_warning(state, partial_step_warning(&args, &run.output));
    }
    Ok(run.output)
}

/// R13 (P15): a random id naming one finalize in its journal and lane
/// entries.
fn new_finalize_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|err| format!("최종화 ID를 만들지 못했습니다 (entropy: {err})"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// R13-S2 (P15): the `reports/` name this finalize's package-case is told
/// to publish under — `package_<unix>` (`_001`, … when taken), free of a
/// folder, of a finalize-incomplete marker naming it and of a kept work
/// folder meant for it. package-case fails rather than pick another.
fn choose_package_name(case_dir: &Path) -> String {
    let reports = case_dir.join("reports");
    let reserved: std::collections::BTreeSet<String> = std::fs::read_dir(&reports)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    crate::join::package_reservation_of(&entry.file_name().to_string_lossy())
                })
                .collect()
        })
        .unwrap_or_default();
    #[allow(unused_mut)]
    let mut base = format!("package_{}", crate::util::now_unix().unwrap_or(0));
    #[cfg(test)]
    if let Some(fixed) = PACKAGE_BASE_FOR_TEST.with(|cell| cell.get()) {
        base = fixed.to_string();
    }
    (0..10_000u32)
        .map(|n| {
            if n == 0 {
                base.clone()
            } else {
                format!("{base}_{n:03}")
            }
        })
        .find(|name| {
            let candidate = reports.join(name);
            std::fs::symlink_metadata(&candidate).is_err()
                && !crate::package::finalize_incomplete_marker(&candidate).exists()
                && !reserved.contains(name)
        })
        .unwrap_or(base)
}

/// R11-S3: what `target` holds now — kept (in the journal) so a finalize
/// whose package then fails can take its sign-off back. A previous record
/// that cannot be read is an error BEFORE anything is promoted (it could
/// not be put back).
fn read_previous_signoff(target: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(target) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("{}: {err}", target.display())),
        Ok(meta) if !meta.is_file() => Err(format!(
            "{}: 파일이 아닙니다 (기존 검토자 기록을 보관할 수 없음)",
            target.display()
        )),
        Ok(_) => std::fs::read(target)
            .map(Some)
            .map_err(|err| format!("{}: {err}", target.display())),
    }
}

/// R12-S2: puts what [`super::finalize_journal::recover`] did into the job
/// state (shown by /api/status until a finalize completes or another case
/// is bound).
pub(crate) fn apply_recovery(state: &SharedState, recovery: super::finalize_journal::Recovery) {
    apply_recovery_locked(&mut state_lock(state), recovery);
}

/// [`apply_recovery`] inside a lock the caller holds. R13: only a recovery
/// that cleaned something up is `finalize_recovered` ("복구됨"); a journal
/// left alone (unpaired, another PC's, a live writer, unreadable) is shown
/// by `finalize_journal` and the job log only.
pub(crate) fn apply_recovery_locked(
    guard: &mut JobState,
    recovery: super::finalize_journal::Recovery,
) {
    if recovery.report_stale {
        guard.report_stale = true;
    }
    if recovery.recovered {
        guard.finalize_recovered = Some(recovery.detail);
    }
}

/// R11-S7: [`crate::package::retry_within`] for the rename budget with
/// NotFound final (a missing file never appears by waiting — R10 retried
/// it for 12 s) and a job-log line while a busy file is waited out.
pub(crate) fn retry_logged(
    state: &SharedState,
    what: &str,
    op: &mut dyn FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    retry_logged_within(state, what, crate::package::rename_retry_budget(), op)
}

/// [`retry_logged`] with an explicit budget (R13-S7: the start-up
/// recovery's is shorter).
pub(crate) fn retry_logged_within(
    state: &SharedState,
    what: &str,
    budget: Duration,
    op: &mut dyn FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut reported = 0u64;
    crate::package::retry_within_notify(
        budget,
        op,
        &crate::package::is_final_rename_error,
        &mut |waited| {
            let secs = waited.as_secs();
            if secs > reported {
                reported = secs;
                state_lock(state)
                    .logs
                    .push(format!("{what}: 파일 사용 중, 재시도 {secs}초"));
            }
        },
    )
}

/// `POST /api/verify-sources` (FOR-03): live re-hash of every indexed
/// source against the recorded SHA-256 (`qa anomalies`, which writes
/// `evidence/logs/hash-verify.jsonl`), then refreshes the custody statement
/// so its recorded-vs-observed table reflects this run. Cancellable like any
/// other step.
pub(crate) fn api_verify_sources(state: &SharedState) -> String {
    match std::env::current_exe() {
        Ok(exe) => verify_sources_with_exe(state, &exe),
        Err(err) => api_err(&err.to_string()),
    }
}

/// [`api_verify_sources`] parameterised on the CLI executable (tests).
pub(crate) fn verify_sources_with_exe(state: &SharedState, exe: &Path) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
    state_lock(state)
        .logs
        .push("원본 재검증: 색인된 모든 원본을 다시 해시하는 중…".into());
    let case_text = case_dir.to_string_lossy().to_string();
    // R12-I4: the run id the child wrote — the reported result must be
    // THIS run, never an older one the log happens to end on.
    let just_run = match run_step(
        exe,
        &["qa".into(), "anomalies".into(), case_text.clone()],
        state,
    ) {
        Ok(output) => crate::anomaly::printed_hash_verify_run_id(&output),
        Err(err) => return api_err(&err),
    };
    // Best effort: the custody statement is regenerated at package time too.
    // A cancel is not "best effort": the user stopped the run, so it is
    // reported as cancelled, never as a completed verification.
    if let Err(err) = run_step(exe, &["make-custody".into(), case_text], state) {
        if state_lock(state).cancel_requested {
            state_lock(state).logs.push(
                "경고: 원본 재검증을 중단했습니다 — 보관 이력서는 갱신되지 않았습니다.".into(),
            );
            return serde_json::json!({
                "ok": false,
                "cancelled": true,
                "error": "원본 재검증을 중단했습니다 — 보관 이력서가 갱신되지 않았습니다.",
            })
            .to_string();
        }
        warn_step(state, 4, format!("보관 이력서 갱신 건너뜀: {err}"));
    }
    match crate::anomaly::latest_hash_verify_summary(&case_dir) {
        Some(mut summary) => {
            // R12-I4: the result is not the run just made (the child did
            // not say which run it wrote, or the log's result is another
            // run): no current result — never an older run's counts.
            if summary.is_result && (just_run.is_none() || summary.run_id != just_run) {
                let reason = match &just_run {
                    None => {
                        "방금 실행한 재검증이 실행 id를 알리지 않아 결과를 이 실행과 대조할 수 없음"
                            .to_string()
                    }
                    Some(id) => format!(
                        "방금 실행한 재검증({id})이 아닌 실행({})이 현재 결과로 읽힘 — 이 실행의 결과를 확인할 수 없음",
                        summary.run_id.as_deref().unwrap_or("?")
                    ),
                };
                summary = crate::anomaly::HashVerifySummary {
                    unauthenticated: summary.unauthenticated,
                    withheld_reason: Some(reason.clone()),
                    not_verified_reason: Some(reason),
                    lane_status: summary.lane_status,
                    head_status: summary.head_status,
                    head_reason: summary.head_reason,
                    clock_note: summary.clock_note.clone(),
                    ..Default::default()
                };
            }
            // R11 P9/I2: a summary that is not verified (rows this PC
            // cannot authenticate, a withheld result) is said so in the
            // log and the response — never "모두 일치".
            let tail = match (&summary.not_verified_reason, summary.is_result) {
                (_, false) => format!(
                    " — 현재 결과 없음(결과 아님): {}",
                    summary.not_verified_reason.as_deref().unwrap_or_default()
                ),
                (Some(reason), true) => format!(" — 확인 불가(NOT VERIFIED): {reason}"),
                (None, true) => String::new(),
            };
            state_lock(state).logs.push(format!(
                "원본 재검증 완료: 일치 {} · 불일치 {} · 비교 불가 {} · 인증 안 됨 {}{tail}",
                summary.matched,
                summary.mismatched,
                summary.not_comparable,
                summary.unauthenticated
            ));
            serde_json::json!({
                "ok": true,
                "run_unix": summary.run_unix,
                "run_id": summary.run_id,
                "expected_run_id": just_run,
                "matched": summary.matched,
                "mismatched": summary.mismatched,
                "not_comparable": summary.not_comparable,
                "unauthenticated": summary.unauthenticated,
                "verified": summary.verified,
                "is_result": summary.is_result,
                "not_verified_reason": summary.not_verified_reason,
                // R13 P16 (track U contract): why there is no current
                // result, and the runs without a completion marker after
                // the last completed run — their mismatches are shown.
                "withheld_reason": summary.withheld_reason,
                "newer_runs": summary.newer_runs,
                "newer_records": summary.newer_records,
                "newer_mismatched": summary.newer_mismatched,
                "newer_mismatches": summary
                    .newer_mismatches
                    .iter()
                    .map(crate::anomaly::NewerMismatch::to_json)
                    .collect::<Vec<_>>(),
                "lane_status": summary.lane_status,
                // R14 I3 (track U contract): the sign-off lane's
                // `hash-verify-head` against the lane.
                "head_status": summary.head_status,
                // R15 I4/I5 (track U contract): why the head is unknown
                // (`other-pc` = "다른 PC 머리일 수 있음"), and "시계 이상".
                "head_reason": summary.head_reason,
                "clock_note": summary.clock_note,
                "log": crate::anomaly::HASH_VERIFY_REL_PATH,
                "custody_url": "case/reports/custody-statement.html",
            })
            .to_string()
        }
        None => api_err("재검증 결과 기록(hash-verify.jsonl)을 읽지 못했습니다."),
    }
}

/// `POST /api/recover-deleted`: run recover-batch --deleted-videos on the
/// image from the latest filesystem inspection, then regenerate the
/// review bundle so the recovered items appear in the viewer.
///
/// UX-06: runs as a background job. The response is immediate —
/// `{ok:true, job:{kind, started_unix}}` — and progress, cancel and the
/// final outcome travel through `/api/status` (`job_kind`, `job`).
pub(crate) fn api_recover_deleted(state: &SharedState) -> String {
    match std::env::current_exe() {
        Ok(exe) => recover_deleted_with_exe(state, &exe),
        Err(err) => api_err(&err.to_string()),
    }
}

/// [`api_recover_deleted`] parameterised on the CLI executable (tests run
/// a fake step binary).
pub(crate) fn recover_deleted_with_exe(state: &SharedState, exe: &Path) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let Some((image_path, partition_offset)) = newest_tsk_inspection(&case_dir) else {
        return "{\"ok\":false,\"error\":\"파일시스템 조사 결과가 없습니다 — 먼저 E01/이미지 분석을 실행하십시오.\"}"
            .to_string();
    };
    let case_text = case_dir.to_string_lossy().to_string();
    let args = vec![
        "recover-batch".into(),
        case_text,
        image_path.to_string_lossy().to_string(),
        "--deleted-videos".into(),
        "--partition-offset".into(),
        partition_offset.to_string(),
    ];
    start_case_job(
        state,
        exe,
        CaseJobSpec {
            kind: "recover-deleted",
            stage_label: "삭제 영상 후보 복구",
            case_dir,
            main_args: args,
            start_log: format!(
                "삭제 영상 후보 복구 중… ({} @ 오프셋 {partition_offset})",
                image_path.display()
            ),
            done_log: "복구 완료 — 뷰어를 새로 열면 복구된 항목이 보입니다.",
        },
    )
}

/// `POST /api/carve`: run bounded signature carving over the case's
/// exported raw image (evidence/images/evidence.raw), then regenerate the
/// review bundle so carved candidates show up in the viewer. Requires the
/// full E01 pipeline (direct triage never exports a raw image).
///
/// UX-06: a background job with the same contract as recover-deleted.
pub(crate) fn api_carve(request: &Request, state: &SharedState) -> String {
    match std::env::current_exe() {
        Ok(exe) => carve_with_exe(request, state, &exe),
        Err(err) => api_err(&err.to_string()),
    }
}

/// [`api_carve`] parameterised on the CLI executable.
pub(crate) fn carve_with_exe(request: &Request, state: &SharedState, exe: &Path) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let raw = case_dir.join("evidence/images/evidence.raw");
    if !raw.is_file() {
        return "{\"ok\":false,\"error\":\"카빙할 raw 이미지가 없습니다 — E01을 '빠른 검토' 없이 분석(전체 export)하면 사용할 수 있습니다.\"}".to_string();
    }
    let case_text = case_dir.to_string_lossy().to_string();
    let mut args = vec![
        "carve-file".into(),
        case_text,
        raw.to_string_lossy().to_string(),
    ];
    if query_value(&request.query, "reassemble").as_deref() == Some("1") {
        args.push("--reassemble".into());
    }
    for (param, flag) in [
        ("scan_offset", "--scan-offset"),
        ("scan_length", "--scan-length"),
    ] {
        if let Some(value) = query_value(&request.query, param)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            && value.parse::<u64>().is_ok()
        {
            args.push(flag.into());
            args.push(value);
        }
    }
    start_case_job(
        state,
        exe,
        CaseJobSpec {
            kind: "carve",
            stage_label: "시그니처 카빙",
            case_dir,
            main_args: args,
            start_log: format!("시그니처 카빙 실행 중… ({})", raw.display()),
            done_log: "카빙 완료 — 뷰어를 새로 열면 카빙 후보가 보입니다.",
        },
    )
}

/// `POST /api/audio-measure {all?}`: measure the real sound level of the
/// case's indexed videos that have an audio track (plan §2.2 "나중에 따로
/// 실행"), then regenerate the review so the viewer sees `audioLevels`.
/// Already-valid measurements are skipped unless `all` is true.
///
/// A background case job (`kind: "audio-measure"`) with the same contract
/// as recover-deleted / carve: busy lease, marks queue meanwhile, progress
/// and outcome through `/api/status` (`job_kind`, `job`).
pub(crate) fn api_audio_measure(request: &Request, state: &SharedState) -> String {
    match std::env::current_exe() {
        Ok(exe) => audio_measure_with_exe(request, state, &exe),
        Err(err) => api_err(&err.to_string()),
    }
}

/// [`api_audio_measure`] parameterised on the CLI executable.
pub(crate) fn audio_measure_with_exe(request: &Request, state: &SharedState, exe: &Path) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 케이스를 열거나 분석을 실행하십시오.\"}"
            .to_string();
    };
    let mut args = vec![
        "measure-audio".into(),
        case_dir.to_string_lossy().to_string(),
    ];
    let all = body_value(&request.body, "all").as_deref() == Some("true");
    if all {
        args.push("--all".into());
    }
    start_case_job(
        state,
        exe,
        CaseJobSpec {
            kind: "audio-measure",
            stage_label: "소리 측정",
            case_dir,
            main_args: args,
            start_log: if all {
                "소리 측정 중… (전체 다시 측정)".to_string()
            } else {
                "소리 측정 중… (미측정 항목)".to_string()
            },
            done_log: "소리 측정 완료 — 뷰어를 새로 열면 측정 결과가 보입니다.",
        },
    )
}

/// What a background case job runs: one main CLI step, then make-review.
pub(crate) struct CaseJobSpec {
    pub(crate) kind: &'static str,
    pub(crate) stage_label: &'static str,
    pub(crate) case_dir: PathBuf,
    pub(crate) main_args: Vec<String>,
    pub(crate) start_log: String,
    pub(crate) done_log: &'static str,
}

/// Takes the busy lease, publishes the job in [`JobState::case_job`] and
/// runs it on a worker thread. Returns the immediate API response.
pub(crate) fn start_case_job(state: &SharedState, exe: &Path, spec: CaseJobSpec) -> String {
    // Fast refusal without touching `busy`; re-checked (with export-selected
    // and frame captures) under the lock below.
    if running_clip_job_count(state) > 0 {
        return api_err("내보내기·이어붙이기 작업이 진행 중입니다 — 끝난 뒤 다시 시도하십시오.");
    }
    let started_unix = crate::util::now_unix().unwrap_or(0);
    // R4-B17: busy, the side-work check and the case re-check share one
    // critical section — the caller read `spec.case_dir` before busy.
    let (lease, prior_phase) = {
        let mut guard = state_lock(state);
        // A clip export, join, export-selected or frame capture writes into
        // the case while make-review/recovery would rewrite it; case jobs
        // wait for all of them like finalize does.
        if let Some(refusal) = side_work_refusal(&guard, "다시 시도하십시오") {
            return api_err(&refusal);
        }
        let lease = match lease_busy_locked(&mut guard, state) {
            Ok(lease) => lease,
            Err(err) => return api_err(&err),
        };
        if guard.case_dir.as_deref() != Some(spec.case_dir.as_path()) {
            drop(guard);
            drop(lease);
            return api_err(CASE_SWITCHED_ERROR);
        }
        // The phase stays what it was (finding 9): marks queue during a
        // case job, so a shell reloaded mid-job must still open the review
        // stage. The job itself is reported via `job` / `job_kind`, and
        // `busy` (held by the lease) still blocks finalize and other jobs.
        let prior = guard.phase;
        guard.error = None;
        guard.case_job = Some(CaseJob {
            kind: spec.kind,
            started_unix,
            finished_unix: None,
            stage: 1,
            stages: 2,
            stage_label: spec.stage_label,
            state: "running",
            error: None,
        });
        guard.logs.push(spec.start_log.clone());
        (lease, prior)
    };
    let kind = spec.kind;
    let worker_state = Arc::clone(state);
    let exe = exe.to_path_buf();
    let spawned = thread::Builder::new()
        .name(format!("ft-{kind}"))
        .spawn(move || run_case_job(&worker_state, lease, &exe, spec, prior_phase));
    if spawned.is_err() {
        // The closure (and with it the lease) was dropped: busy is free.
        let mut guard = state_lock(state);
        guard.phase = prior_phase;
        guard.case_job = None;
        return api_err("작업 스레드 시작 실패");
    }
    serde_json::json!({
        "ok": true,
        "job": {"kind": kind, "started_unix": started_unix},
    })
    .to_string()
}

/// How a case job ended.
enum CaseJobEnd {
    Done,
    Cancelled,
    Failed(String),
}

/// Worker body: main step → make-review (re-run until it has seen every
/// mark queued meanwhile, like phase 2) → final state, released together
/// with `busy` under the marks gate.
fn run_case_job(
    state: &SharedState,
    lease: BusyLease,
    exe: &Path,
    spec: CaseJobSpec,
    prior_phase: &'static str,
) {
    let cancelled = || state_lock(state).cancel_requested;
    let end_with = |lease: BusyLease, end: CaseJobEnd| {
        let _gate = marks_gate();
        finish_case_job(state, lease, &spec, end, prior_phase);
    };
    match run_step(exe, &spec.main_args, state) {
        Ok(output) => {
            let mut guard = state_lock(state);
            if spec.kind == "audio-measure"
                && let Some(line) = crate::audio_level::summary_log_line(&output)
            {
                guard.logs.push(line);
            }
            guard.logs.push(output)
        }
        Err(_) if cancelled() => return end_with(lease, CaseJobEnd::Cancelled),
        // B15: measure-audio exit 4 completed (everything logged) with some
        // files unmeasurable — report the count and still regenerate the
        // review so the measured ones show.
        Err(err)
            if spec.kind == "audio-measure"
                && crate::audio_level::partial_error_count(&err).is_some() =>
        {
            let errors = crate::audio_level::partial_error_count(&err).unwrap_or(0);
            state_lock(state).logs.push(format!(
                "소리 측정 완료(오류 {errors}건) — 측정하지 못한 영상은 '미확인'으로 표시됩니다"
            ));
        }
        Err(err) => return end_with(lease, CaseJobEnd::Failed(err)),
    }
    if cancelled() {
        return end_with(lease, CaseJobEnd::Cancelled);
    }
    let mut seen_epoch = {
        let _gate = marks_gate();
        let drained = drain_pending_marks(state, &spec.case_dir);
        let mut guard = state_lock(state);
        if let Some(job) = guard.case_job.as_mut() {
            job.stage = 2;
            job.stage_label = "리뷰 생성";
        }
        if let Err(err) = drained {
            drop(guard);
            finish_case_job(state, lease, &spec, CaseJobEnd::Failed(err), prior_phase);
            return;
        }
        guard.marks_epoch
    };
    let case_text = spec.case_dir.to_string_lossy().to_string();
    for _ in 0..8 {
        match run_step(exe, &["make-review".into(), case_text.clone()], state) {
            Ok(output) => state_lock(state).logs.push(output),
            Err(_) if cancelled() => return end_with(lease, CaseJobEnd::Cancelled),
            Err(err) => return end_with(lease, CaseJobEnd::Failed(err)),
        }
        let _gate = marks_gate();
        if let Err(err) = drain_pending_marks(state, &spec.case_dir) {
            finish_case_job(state, lease, &spec, CaseJobEnd::Failed(err), prior_phase);
            return;
        }
        let epoch = state_lock(state).marks_epoch;
        if epoch == seen_epoch {
            finish_case_job(state, lease, &spec, CaseJobEnd::Done, prior_phase);
            return;
        }
        seen_epoch = epoch;
        state_lock(state)
            .logs
            .push("리뷰 생성 중 저장된 판독 마크를 포함하도록 리뷰를 다시 생성합니다.".into());
    }
    end_with(
        lease,
        CaseJobEnd::Failed(
            "리뷰 생성 중 판독 마크가 계속 바뀌어 안정화하지 못했습니다 — 다시 시도하십시오."
                .into(),
        ),
    );
}

/// Publishes the outcome and frees `busy` atomically. The caller holds the
/// marks gate; queued marks are applied first so none is stranded behind
/// a cancelled/failed job (a failure to apply them is a visible warning
/// and they stay queued — the finalize checklist then blocks on them).
fn finish_case_job(
    state: &SharedState,
    lease: BusyLease,
    spec: &CaseJobSpec,
    end: CaseJobEnd,
    prior_phase: &'static str,
) {
    let drain_failure = match end {
        CaseJobEnd::Done => None,
        _ => drain_pending_marks(state, &spec.case_dir).err(),
    };
    let finished = crate::util::now_unix().ok();
    lease.release_with(|guard| {
        if let Some(err) = drain_failure {
            guard.logs.push(format!("오류: {err}"));
            guard.warnings.push((4, err));
        }
        // make-review ran only on success; otherwise the phase the case
        // had before stays (an index-only case is not "review-ready").
        guard.phase = match (&end, prior_phase) {
            (CaseJobEnd::Done, _) => "review-ready",
            (_, "review-partial") => "review-partial",
            (_, "idle") => "idle",
            _ => "review-ready",
        };
        guard.byte_progress = None;
        let (job_state, error) = match &end {
            CaseJobEnd::Done => {
                guard.logs.push(spec.done_log.to_string());
                ("done", None)
            }
            CaseJobEnd::Cancelled => {
                let message = format!(
                    "{} 작업을 중단했습니다 — 결과가 리뷰에 반영되지 않았을 수 있습니다.",
                    spec.stage_label
                );
                guard.logs.push(format!("경고: {message}"));
                ("cancelled", Some(message))
            }
            CaseJobEnd::Failed(err) => {
                guard.logs.push(format!("오류: {err}"));
                guard.error = Some(err.clone());
                ("failed", Some(err.clone()))
            }
        };
        if let Some(job) = guard.case_job.as_mut() {
            job.state = job_state;
            job.error = error;
            job.finished_unix = finished;
        }
    });
}

/// `/api/status` view of the current/last case job (UX-06).
pub(crate) fn case_job_json(job: &CaseJob) -> serde_json::Value {
    let end = job
        .finished_unix
        .or_else(|| crate::util::now_unix().ok())
        .unwrap_or(job.started_unix);
    serde_json::json!({
        "kind": job.kind,
        "state": job.state,
        "started_unix": job.started_unix,
        "finished_unix": job.finished_unix,
        "elapsed_secs": end.saturating_sub(job.started_unix),
        "stage": job.stage,
        "stages": job.stages,
        "stage_label": job.stage_label,
        "error": job.error,
    })
}

#[cfg(test)]
#[path = "tests_r12_verify_sources.rs"]
mod tests_r12_verify_sources;
