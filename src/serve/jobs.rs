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
    let skip_e01_verify = body_value(&request.body, "skip_e01_verify").as_deref() == Some("true");
    let case_dir = match body_value(&request.body, "case_dir") {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()),
        _ => default_case_dir(),
    };
    let job = PipelineJob {
        kind: input_kind,
        case_dir: case_dir.clone(),
        source_path: source_path.clone(),
        with_hash,
        with_ffprobe,
        with_deepfake,
        skip_e01_verify,
        e01_direct,
    };
    {
        let mut guard = state_lock(state);
        if guard.busy {
            return "{\"ok\":false,\"error\":\"이미 분석이 진행 중입니다.\"}".to_string();
        }
        guard.busy = true;
        guard.phase = "running";
        guard.restored = false;
        guard.cancel_requested = false;
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
        if input_kind == InputKind::Folder && source_is_volume_root(&source_path) {
            let message = format!(
                "소스가 볼륨 루트입니다 ({}) — 이 볼륨 전체가 검토 범위(/media)로 열립니다. 증거 폴더만 지정하는 것을 권장합니다.",
                source_path.display()
            );
            guard.logs.push(format!("경고: {message}"));
            guard.warnings.push((1, message));
        }
    }
    save_session(
        &case_dir,
        (input_kind == InputKind::Folder).then_some(source_path.as_path()),
    );
    let worker_state = Arc::clone(state);
    let spawned = thread::Builder::new()
        .name("ft-pipeline".into())
        .spawn(move || run_pipeline(worker_state, job));
    if spawned.is_err() {
        let mut guard = state_lock(state);
        guard.busy = false;
        guard.phase = "idle";
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

pub(crate) fn default_case_dir() -> PathBuf {
    let root = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    root.join("FrameTrace").join(format!("case-{stamp}"))
}

pub(crate) fn run_pipeline(state: SharedState, job: PipelineJob) {
    match job.kind {
        InputKind::Folder => run_folder_pipeline(state, job),
        InputKind::E01 | InputKind::E01Direct => run_e01_pipeline(state, job),
    }
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
        if matches!(key, "validate" | "anomalies")
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
    run_step_cancellable(exe, args, &|| state_lock(state).cancel_requested)
}

/// [`run_step`] with an explicit cancel predicate, for jobs that do not use
/// the global cancel flag (clip exports cancel per job, UX-09). A `true`
/// from `cancelled` kills the whole process tree.
pub(crate) fn run_step_cancellable(
    exe: &Path,
    args: &[String],
    cancelled: &dyn Fn() -> bool,
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
        match tracked.child.try_wait() {
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

/// Applies every queued marks import in arrival order. A file is removed
/// only after its import committed, so a failure leaves it queued. The
/// caller holds `MARKS_GATE`. Returns how many were applied.
fn drain_pending_marks(state: &SharedState, case_dir: &Path) -> Result<usize, String> {
    let mut applied = 0;
    for path in pending_marks_files(case_dir) {
        crate::cli::handlers::import_marks_quiet(case_dir, &path)
            .map_err(|err| format!("대기 중이던 마크 반영 실패 ({}): {err}", path.display()))?;
        let _ = std::fs::remove_file(&path);
        applied += 1;
        state_lock(state).marks_epoch += 1;
    }
    if applied > 0 {
        state_lock(state).logs.push(format!(
            "저장 대기 중이던 판독 마크 {applied}건을 반영했습니다."
        ));
    }
    Ok(applied)
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
    let direct =
        matches!(step, Some("validate") | Some("anomalies")) && pending_marks_count(case_dir) == 0;
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
        return serde_json::json!({
            "ok": true,
            "deferred": true,
            "pending": pending_marks_count(case_dir),
        })
        .to_string();
    }
    let marks_path = case_dir.join("marks-imported.json");
    if let Err(err) = crate::util::write_text_atomic(&marks_path, marks_body) {
        return api_err(&err.to_string());
    }
    match crate::cli::handlers::import_marks_quiet(case_dir, &marks_path) {
        Ok(stored) => {
            state_lock(state).marks_epoch += 1;
            serde_json::json!({"ok": true, "applied": true, "stored": stored}).to_string()
        }
        Err(err) => api_err(&err),
    }
}

/// Accepts the viewer's downloaded marks JSON, stores it in the case, and
/// refreshes the report so examiner marks land in the deliverable.
pub(crate) fn api_import_marks(request: &Request, state: &SharedState) -> String {
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
    let preflight =
        crate::selection::parse_marks_text(&marks_body, &marks_path).and_then(|marks| {
            crate::selection::enforce_case_binding(
                &case_dir,
                marks.case_id.as_deref(),
                &marks_path,
                crate::selection::ImportKind::Marks,
            )
        });
    if let Err(err) = preflight {
        return serde_json::json!({"ok": false, "error": err}).to_string();
    }
    let enriching = marks_must_queue(&state_lock(state));
    if enriching {
        return import_marks_during_enrichment(state, &case_dir, &marks_body);
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
    if let Err(err) = std::fs::write(&marks_path, &marks_body) {
        return format!(
            "{{\"ok\":false,\"error\":{}}}",
            json_string(&err.to_string())
        );
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            return format!(
                "{{\"ok\":false,\"error\":{}}}",
                json_string(&err.to_string())
            );
        }
    };
    let case_text = case_dir.to_string_lossy().to_string();
    if let Err(err) = run_step(
        &exe,
        &[
            "import-marks".into(),
            case_text.clone(),
            marks_path.to_string_lossy().to_string(),
        ],
        state,
    ) {
        return serde_json::json!({"ok": false, "error": err}).to_string();
    }
    if let Err(err) = run_step(&exe, &["make-report".into(), case_text.clone()], state) {
        return serde_json::json!({"ok": false, "error": err}).to_string();
    }
    {
        let mut guard = state_lock(state);
        guard
            .logs
            .push("판독 마크를 반영해 보고서를 갱신했습니다.".into());
    }
    "{\"ok\":true,\"report_url\":\"case/reports/case-report.html\"}".to_string()
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
/// to the current review marks (`content_sha256`, see
/// `report::signoff_content_sha256`) so a later make-report can say whether
/// the marks changed after the sign-off. Returns the signed unix time.
pub(crate) fn write_signoff(
    case_dir: &Path,
    rel_path: &str,
    examiner: &str,
) -> Result<i64, String> {
    let signed_unix = crate::util::now_unix()? as i64;
    let content_sha256 =
        crate::report::signoff_content_sha256(&crate::cli::handlers::review_marks_json(case_dir));
    let body = format!(
        "{{\"schema_version\":2,\"examiner\":{},\"signed_unix\":{signed_unix},\"content\":\"review marks\",\"content_sha256\":{},\"note\":\"examiner name recorded as typed at finalize; not an authenticated identity\"}}\n",
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
    // UX-09: clip exports run without `busy`; sealing while one is still
    // writing into artifacts/clips would package a torn or missing clip.
    if running_clip_job_count(state) > 0 {
        return api_err("최종화 차단: 클립 내보내기가 진행 중입니다 — 끝난 뒤 다시 시도하십시오.");
    }
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
    // The sign-off is written to a pending file that make-report reads
    // (--signoff) so the report names the reviewer, and is promoted to
    // review/signoff.json only after report AND package succeeded: a failed
    // finalize never leaves a sign-off behind (SHOULD-FIX 6).
    let pending_signoff = case_dir.join(SIGNOFF_PENDING_REL_PATH);
    if let Err(err) = write_signoff(case_dir, SIGNOFF_PENDING_REL_PATH, examiner) {
        return api_err(&err);
    }
    {
        let mut guard = state_lock(state);
        guard.phase = "finalizing";
        guard.busy = true;
        guard.error = None;
        guard.logs.push("결과 보고서 생성 중…".into());
    }
    let case_text = case_dir.to_string_lossy().to_string();
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
            guard.busy = false;
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
            guard.busy = false;
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
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return api_err(&err.to_string()),
    };
    state_lock(state)
        .logs
        .push("원본 재검증: 색인된 모든 원본을 다시 해시하는 중…".into());
    let case_text = case_dir.to_string_lossy().to_string();
    if let Err(err) = run_step(
        &exe,
        &["qa".into(), "anomalies".into(), case_text.clone()],
        state,
    ) {
        return api_err(&err);
    }
    // Best effort: the custody statement is regenerated at package time too.
    if let Err(err) = run_step(&exe, &["make-custody".into(), case_text], state) {
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
        return api_err("클립 내보내기가 진행 중입니다 — 내보내기가 끝난 뒤 다시 시도하십시오.");
    }
    let started_unix = crate::util::now_unix().unwrap_or(0);
    let prior_phase = {
        let mut guard = state_lock(state);
        let prior = guard.phase;
        guard.phase = "finalizing";
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
        Ok(output) => state_lock(state).logs.push(output),
        Err(_) if cancelled() => return end_with(lease, CaseJobEnd::Cancelled),
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
