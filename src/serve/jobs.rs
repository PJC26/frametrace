//! Pipeline-driving API handlers: scan/E01/folder pipelines, recover, carve, finalize, marks.

use super::*;

pub(crate) fn api_start(request: &Request, state: &SharedState) -> String {
    let e01_direct = body_value(&request.body, "e01_direct").as_deref() == Some("true");
    let input_kind = match body_value(&request.body, "input_kind").as_deref() {
        Some("e01") if e01_direct => InputKind::E01Direct,
        Some("e01") => InputKind::E01,
        _ => InputKind::Folder,
    };
    let source_path = match body_value(&request.body, "source_path") {
        Some(path) if !path.trim().is_empty() => PathBuf::from(path.trim()),
        _ => {
            return "{\"ok\":false,\"error\":\"증거 소스 경로를 입력하십시오.\"}".to_string();
        }
    };
    // Item 8 policy (see http::reject_remote_path): a typed UNC share is
    // honoured on this nonce-gated POST, device-namespace paths are not.
    // Checked before `exists()` touches the filesystem.
    for field in ["source_path", "case_dir"] {
        if let Some(text) = body_value(&request.body, field)
            && let Err(reason) = super::http::reject_device_path(&text)
        {
            return api_err(reason);
        }
    }
    if !source_path.exists() {
        return format!(
            "{{\"ok\":false,\"error\":{}}}",
            json_string(&format!(
                "소스 경로가 존재하지 않습니다: {}",
                source_path.display()
            ))
        );
    }
    if matches!(input_kind, InputKind::E01 | InputKind::E01Direct)
        && !tool_available("ewfinfo", "-V")
    {
        return "{\"ok\":false,\"error\":\"E01 처리에는 libewf 도구(ewfinfo/ewfverify/ewfexport)가 필요합니다. 도구를 설치한 뒤 다시 시도하십시오.\"}".to_string();
    }
    let with_hash = body_value(&request.body, "with_hash").as_deref() == Some("true");
    let with_ffprobe = body_value(&request.body, "with_ffprobe").as_deref() == Some("true");
    let with_deepfake = body_value(&request.body, "with_deepfake").as_deref() == Some("true");
    // Plan §2.2: measuring sound needs ffprobe to pick the targets, so the
    // option only takes effect together with with_ffprobe (default off).
    let with_audio = body_value(&request.body, "with_audio").as_deref() == Some("true");
    let skip_e01_verify = body_value(&request.body, "skip_e01_verify").as_deref() == Some("true");
    let (case_dir, case_dir_warning) = match body_value(&request.body, "case_dir") {
        Some(dir) if !dir.trim().is_empty() => (PathBuf::from(dir.trim()), None),
        _ => default_case_dir_checked(),
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
        guard.phase = "running";
        guard.restored = false;
        guard.warnings = Vec::new();
        guard.enrichment = None;
        guard.steps = [StepStatus::Pending; 5];
        guard.step_names = job.kind.step_names().to_vec();
        guard.logs = Vec::new();
        guard.error = None;
        guard.package_dir = None;
        guard.case_dir = Some(case_dir.clone());
        guard.case_job = None;
        guard.media_roots = start_media_roots(input_kind, &case_dir, &source_path);
        // SEC-06: /media serves anything under a media root. A folder
        // source at a volume root (E:\ of a dashcam card) is legitimate,
        // but then the whole volume is reviewable — say so, never silently.
        if volume_root {
            let message = format!(
                "소스가 볼륨 루트입니다 ({}) — 이 볼륨 전체가 검토 범위(/media)로 열립니다. 증거 폴더만 지정하는 것을 권장합니다.",
                source_path.display()
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
        match run_step(
            &exe,
            &[
                "inspect-e01".into(),
                case_text.clone(),
                source_text.clone(),
                "--filesystem".into(),
            ],
            &state,
        ) {
            Ok(output) => {
                log(&state, output);
                set_step(&state, 1, StepStatus::Done);
                set_step(&state, 2, StepStatus::Done);
            }
            Err(err) => {
                set_step(&state, 1, StepStatus::Failed);
                fail(&state, &err);
                return;
            }
        }
        run_e01_pipeline_tail(state, job);
        return;
    }

    // Step 2: import the E01 (ewfverify runs unless explicitly skipped).
    // A pre-existing raw image is reused ONLY when it was imported from
    // the same E01 path; otherwise the wizard would silently analyze the
    // previous evidence end-to-end (wrong-evidence results).
    set_step(&state, 1, StepStatus::Running);
    let raw_source_binding = raw_path.with_extension("raw.source");
    let reuse_existing_raw = raw_path.exists() && {
        let previous_source = std::fs::read_to_string(&raw_source_binding)
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        let same_source = previous_source == source_text;
        if !same_source && !previous_source.is_empty() {
            log(
                &state,
                format!(
                    "기존 raw 이미지는 다른 E01에서 임포트된 것입니다. 재임포트합니다: {raw_text}"
                ),
            );
        }
        same_source
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
                // Bind the imported raw to this E01 so a later run with a
                // different E01 re-imports instead of reusing stale bytes.
                let _ = std::fs::write(&raw_source_binding, &source_text);
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
    match run_step(
        &exe,
        &["inspect-image".into(), case_text.clone(), raw_text.clone()],
        &state,
    ) {
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

    run_e01_pipeline_tail(state, job);
}

/// Shared E01 tail: refresh the logical index over the case evidence
/// tree, run the non-fatal anomaly scan, then emit the review bundle.
pub(crate) fn run_e01_pipeline_tail(state: SharedState, job: PipelineJob) {
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
    let case_text = job.case_dir.to_string_lossy().to_string();
    set_step(&state, 3, StepStatus::Running);
    let mut scan_args: Vec<String> = vec![
        "scan-folder".into(),
        case_text.clone(),
        job.case_dir.join("evidence").to_string_lossy().to_string(),
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
    // Spawned as a process tree (own group / Job object) so cancel reaches
    // the ffmpeg/ffprobe grandchildren, not just the direct child.
    let mut tracked = super::proc::spawn_tracked(
        Command::new(exe)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .map_err(|err| format!("{} 실행 실패: {err}", exe.display()))?;
    if let Some(err) = tracked.tracking_error.take() {
        let program = args.first().map(String::as_str).unwrap_or("command");
        report_process_warning(state, super::proc::tracking_warning(program, &err));
    }
    let child = &mut tracked.child;
    // Both pipes must be drained while the child runs: a child that fills
    // the OS pipe buffer blocks on write, and a parent that only polls
    // try_wait() then waits forever — the classic pipe deadlock. tsk.rs
    // uses the same reader-thread pattern for icat.
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_reader = thread::spawn(move || drain_capped(&mut stdout_pipe));
    let stderr_reader = thread::spawn(move || drain_capped(&mut stderr_pipe));
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
    // The child has exited, so both pipes are at EOF and these joins
    // return immediately with whatever tail was retained.
    let stdout_tail = stdout_reader.join().unwrap_or_default();
    let stderr_tail = stderr_reader.join().unwrap_or_default();
    if killed || cancelled() {
        return Err("사용자가 분석을 중단했습니다.".to_string());
    }
    let mut text = String::from_utf8_lossy(&stdout_tail).to_string();
    let stderr = String::from_utf8_lossy(&stderr_tail);
    if !stderr.trim().is_empty() {
        text.push_str(&stderr);
    }
    if !status.success() {
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
    Ok(text.trim_end().to_string())
}

/// Drains `pipe` to EOF, retaining only the last `STEP_OUTPUT_TAIL_BYTES`.
/// Retention stays bounded even when a step emits unbounded progress
/// output, while the drain itself keeps the child's write end from ever
/// blocking on a full pipe.
pub(crate) fn drain_capped(pipe: &mut impl std::io::Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.len() > STEP_OUTPUT_TAIL_BYTES {
                    let excess = buffer.len() - STEP_OUTPUT_TAIL_BYTES;
                    buffer.drain(..excess);
                }
            }
        }
    }
    buffer
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
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
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
    let lines: Vec<String> = outcome
        .conflicts
        .iter()
        .map(|conflict| {
            let deleted = body
                .get("deleted_ids")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(&conflict.id)));
            serde_json::json!({
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
struct OpenQueueConflict {
    id: String,
    latest: serde_json::Value,
    server_revision: u64,
    entries: usize,
}

/// Unresolved queue conflicts: distinct ids whose last journal entry is a
/// conflict (not a `resolved` line), in first-seen order.
fn open_queue_conflicts(case_dir: &Path) -> Vec<OpenQueueConflict> {
    let Ok(text) = crate::util::read_to_string(&pending_mark_conflicts_path(case_dir)) else {
        return Vec::new();
    };
    let mut open: Vec<OpenQueueConflict> = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(id) = value.get("resolved").and_then(serde_json::Value::as_str) {
            open.retain(|entry| entry.id != id);
        } else if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
            let revision = value
                .get("server_revision")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            match open.iter_mut().find(|entry| entry.id == id) {
                Some(entry) => {
                    entry.server_revision = entry.server_revision.max(revision);
                    entry.entries += 1;
                    entry.latest = value;
                }
                None => open.push(OpenQueueConflict {
                    id: id.to_string(),
                    latest: value,
                    server_revision: revision,
                    entries: 1,
                }),
            }
        }
    }
    open
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

/// `GET /api/mark-conflicts` (contract §10.2): every unresolved queue
/// conflict, one per record id (no cap), with the server value it met and
/// the entry the window originally queued (`queued`, verbatim from the
/// queued body: `{mark, tags, deleted, base_revision}`), so the viewer can
/// show "my value" exactly as it was submitted.
pub(crate) fn api_mark_conflicts(state: &SharedState) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let open = case_dir
        .as_deref()
        .map(open_queue_conflicts)
        .unwrap_or_default();
    let conflicts: Vec<serde_json::Value> = open
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
    serde_json::json!({
        "ok": true,
        "count": conflicts.len(),
        "conflicts": conflicts,
    })
    .to_string()
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
                &entry.id == id && bases.get(id).copied().unwrap_or(0) >= entry.server_revision
            })
        })
        .map(|(id, revision)| {
            serde_json::json!({"resolved": id, "revision": revision, "unix": now}).to_string()
        })
        .collect();
    // Best effort: a failure here leaves the conflict visible (never hides it).
    let _ = append_jsonl_lines(&pending_mark_conflicts_path(case_dir), &lines);
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
pub(crate) fn marks_must_queue(guard: &JobState) -> bool {
    guard.busy
        && (guard.phase == "review-partial"
            || guard
                .case_job
                .as_ref()
                .is_some_and(|job| job.state == "running"))
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
    let (partial, step) = {
        let guard = state_lock(state);
        (marks_must_queue(&guard), guard.enrichment.map(|e| e.step))
    };
    if !partial {
        return api_err("진행 중이던 작업이 방금 끝났습니다 — 잠시 후 다시 저장하십시오.");
    }
    let direct = matches!(step, Some("validate") | Some("audio") | Some("anomalies"))
        && pending_marks_count(case_dir) == 0;
    if !direct {
        let dir = case_dir.join(PENDING_MARKS_DIR);
        if let Err(err) = std::fs::create_dir_all(&dir) {
            return api_err(&format!("대기열 폴더를 만들 수 없습니다: {err}"));
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!("{nanos:032}.json"));
        if let Err(err) = crate::util::write_text_atomic(&path, marks_body) {
            return api_err(&format!("대기열 저장 실패: {err}"));
        }
        state_lock(state).logs.push(
            "작업 진행 중이라 판독 마크를 대기열에 저장했습니다 (해당 단계 직후 반영).".into(),
        );
        return import_marks_response(
            marks,
            &crate::case_db::PatchOutcome::default(),
            Some(pending_marks_count(case_dir)),
            false,
        );
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
    let enriching = marks_must_queue(&state_lock(state));
    if enriching {
        return import_marks_during_enrichment(state, &case_dir, &marks_body, &marks);
    }
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
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
    if refresh_report {
        let case_text = case_dir.to_string_lossy().to_string();
        if let Err(err) = runner(&["make-report".into(), case_text]) {
            return serde_json::json!({"ok": false, "error": err}).to_string();
        }
        let mut guard = state_lock(state);
        guard
            .logs
            .push("판독 마크를 반영해 보고서를 갱신했습니다.".into());
        if !outcome.conflicts.is_empty() {
            guard.logs.push(format!(
                "판독 마크 {}건은 다른 창에서 먼저 바뀌어 적용하지 않았습니다 — 뷰어에서 확인하십시오.",
                outcome.conflicts.len()
            ));
        }
    }
    import_marks_response(&marks, &outcome, None, refresh_report)
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

/// Where an in-progress finalize keeps its sign-off until report and
/// package both succeeded; only then is it promoted to
/// `review/signoff.json` (SHOULD-FIX 6).
pub(crate) const SIGNOFF_PENDING_REL_PATH: &str = "review/signoff.pending.json";

/// Writes a sign-off record atomically to `rel_path`. It binds the examiner
/// to the current review marks, their IN-OUT ranges and the tag map
/// (`content_sha256`, see
/// `report::signoff_content_sha256`) so a later make-report can say whether
/// the marks changed after the sign-off. Returns the signed unix time.
pub(crate) fn write_signoff(
    case_dir: &Path,
    rel_path: &str,
    examiner: &str,
) -> Result<i64, String> {
    let signed_unix = crate::util::now_unix()? as i64;
    // A read failure refuses the sign-off (finding 8): binding it to an
    // empty mark list would misstate what the examiner signed.
    let content = crate::cli::handlers::signoff_content_json(case_dir).map_err(|err| {
        format!("최종화 차단: 검토자 기록에 묶을 판독 내용을 읽지 못했습니다 — {err}")
    })?;
    let content_sha256 = crate::report::signoff_content_sha256(&content);
    let body = format!(
        "{{\"schema_version\":{},\"examiner\":{},\"signed_unix\":{signed_unix},\"content\":\"review marks (status, note, IN-OUT range) and tags\",\"content_sha256\":{},\"note\":\"examiner name recorded as typed at finalize; not an authenticated identity\"}}\n",
        crate::report::SIGNOFF_SCHEMA_VERSION,
        json_string(examiner),
        json_string(&content_sha256)
    );
    let path = case_dir.join(rel_path);
    crate::util::write_text_atomic(&path, &body)
        .map_err(|err| format!("검토자 기록 저장 실패 {}: {err}", path.display()))?;
    Ok(signed_unix)
}

/// SHA-256 of the package's `manifest.sha256`: one value an examiner can
/// write down to pin the whole packaged file list.
pub(crate) fn package_manifest_sha256(package_dir: &Path) -> Option<String> {
    audit::digest_file(&package_dir.join("manifest.sha256")).ok()
}

pub(crate) fn api_finalize(request: &Request, state: &SharedState) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let examiner = sanitize_examiner(&body_value(&request.body, "examiner").unwrap_or_default());
    if examiner.is_empty() {
        return api_err("검토자 이름을 입력하십시오 (보고서에 입력값 그대로 기록됩니다).");
    }
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
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
    // review/signoff.json only after report AND package succeeded: a failed
    // finalize never leaves a sign-off behind (SHOULD-FIX 6).
    let pending_signoff = case_dir.join(SIGNOFF_PENDING_REL_PATH);
    if let Err(err) = write_signoff(case_dir, SIGNOFF_PENDING_REL_PATH, examiner) {
        let mut guard = state_lock(state);
        guard.phase = "review-ready";
        guard.error = Some(err.clone());
        return api_err(&err);
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
    let packaging = if report.is_ok() {
        run_step(exe, &["package-case".into(), case_text.clone()], state)
    } else {
        report.clone()
    };
    let promoted = match (&report, &packaging) {
        (Ok(_), Ok(_)) => std::fs::rename(
            &pending_signoff,
            case_dir.join(crate::report::SIGNOFF_REL_PATH),
        )
        .map_err(|err| format!("검토자 기록 확정 실패: {err}")),
        _ => {
            let _ = std::fs::remove_file(&pending_signoff);
            // The report on disk was rendered with the abandoned sign-off;
            // re-render it from the promoted state (best effort) so it does
            // not name a reviewer for a finalize that never completed.
            if report.is_ok() {
                let _ = run_step(exe, &["make-report".into(), case_text.clone()], state);
            }
            Ok(())
        }
    };
    let packaging = match (packaging, promoted) {
        (Ok(out), Ok(())) => Ok(out),
        (Ok(_), Err(err)) | (Err(err), _) => Err(err),
    };
    let mut guard = state_lock(state);
    match (report, packaging) {
        (Ok(_), Ok(_)) => {
            let package_dir = newest_package_dir(case_dir);
            let package_url = package_dir
                .as_ref()
                .and_then(|path| path.strip_prefix(case_dir).ok())
                .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            guard.phase = "done";
            guard.byte_progress = None;
            guard.package_dir = package_dir.clone();
            let manifest_sha256 = package_dir.as_deref().and_then(package_manifest_sha256);
            guard.logs.push(format!(
                "보고서·패키지 생성 완료. 검토자(입력값 기록): {examiner}{}",
                manifest_sha256
                    .as_deref()
                    .map(|hash| format!(" · manifest.sha256 SHA-256 {hash}"))
                    .unwrap_or_default()
            ));
            format!(
                "{{\"ok\":true,\"package_url\":{},\"package_dir\":{},\"examiner\":{},\"manifest_sha256\":{}}}",
                json_string(&package_url),
                opt_path(&package_dir),
                json_string(examiner),
                manifest_sha256
                    .as_deref()
                    .map(json_string)
                    .unwrap_or_else(|| "null".to_string())
            )
        }
        (report, packaging) => {
            let error = report.err().or_else(|| packaging.err()).unwrap_or_default();
            guard.phase = "review-ready";
            guard.error = Some(error.clone());
            serde_json::json!({"ok": false, "error": error}).to_string()
        }
    }
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
    if let Err(err) = run_step(
        exe,
        &["qa".into(), "anomalies".into(), case_text.clone()],
        state,
    ) {
        return api_err(&err);
    }
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
        Some(summary) => {
            state_lock(state).logs.push(format!(
                "원본 재검증 완료: 일치 {} · 불일치 {} · 비교 불가 {}",
                summary.matched, summary.mismatched, summary.not_comparable
            ));
            serde_json::json!({
                "ok": true,
                "run_unix": summary.run_unix,
                "matched": summary.matched,
                "mismatched": summary.mismatched,
                "not_comparable": summary.not_comparable,
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
    let lease = match try_lease_busy(state) {
        Ok(lease) => lease,
        Err(err) => return api_err(&err),
    };
    // A clip export writes into the case while make-review/recovery would
    // rewrite it; recover/carve wait for exports like finalize does.
    if running_clip_job_count(state) > 0 {
        return api_err("내보내기·이어붙이기 작업이 진행 중입니다 — 끝난 뒤 다시 시도하십시오.");
    }
    let started_unix = crate::util::now_unix().unwrap_or(0);
    let prior_phase = {
        let mut guard = state_lock(state);
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
        prior
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
