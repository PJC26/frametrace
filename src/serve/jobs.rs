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
        guard.steps = [StepStatus::Pending; 5];
        guard.step_names = job.kind.step_names().to_vec();
        guard.logs = Vec::new();
        guard.error = None;
        guard.package_dir = None;
        guard.case_dir = Some(case_dir.clone());
        guard.media_roots = match input_kind {
            InputKind::Folder => vec![case_dir.clone(), source_path.clone()],
            InputKind::E01 | InputKind::E01Direct => vec![case_dir.clone()],
        };
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

pub(crate) fn run_folder_pipeline(state: SharedState, job: PipelineJob) {
    let case_dir = job.case_dir;
    let source_path = job.source_path;
    let with_hash = job.with_hash;
    let with_ffprobe = job.with_ffprobe;
    let with_deepfake = job.with_deepfake;
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
    let case_text = case_dir.to_string_lossy().to_string();
    let source_text = source_path.to_string_lossy().to_string();

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

    set_step(&state, 2, StepStatus::Running);
    let mut scan_args: Vec<String> = vec!["scan-folder".into(), case_text.clone(), source_text];
    if with_hash {
        scan_args.push("--hash".into());
    }
    if !with_ffprobe {
        scan_args.push("--no-ffprobe".into());
    }
    if with_deepfake {
        scan_args.push("--deepfake".into());
    }
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

    set_step(&state, 3, StepStatus::Running);
    if with_ffprobe {
        let selection_path = case_dir.join("selection-all.json");
        match build_selection_file(&case_dir, &selection_path) {
            Ok(count) => {
                log(&state, format!("검증 대상 {count}건"));
                match run_step(
                    &exe,
                    &[
                        "validate-batch".into(),
                        case_text.clone(),
                        selection_path.to_string_lossy().to_string(),
                    ],
                    &state,
                ) {
                    Ok(output) => {
                        log(&state, output);
                        set_step(&state, 3, StepStatus::Done);
                    }
                    Err(err) => {
                        warn_step(&state, 3, format!("일괄 검증 실패, 건너뜀: {err}"));
                        set_step(&state, 3, StepStatus::Done);
                    }
                }
            }
            Err(err) => {
                warn_step(
                    &state,
                    3,
                    format!("선택 목록 생성 실패, 검증 건너뜀: {err}"),
                );
                set_step(&state, 3, StepStatus::Done);
            }
        }
    } else {
        log(&state, "ffprobe 검증이 꺼져 있어 건너뜁니다.".to_string());
        set_step(&state, 3, StepStatus::Done);
    }

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
            guard
                .logs
                .push("검토 화면이 준비되었습니다. 뷰어에서 증거를 확인하십시오.".into());
        }
        Err(err) => {
            set_step(&state, 4, StepStatus::Failed);
            fail(&state, &err);
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
        if !killed && state_lock(state).cancel_requested {
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
    if state_lock(state).cancel_requested {
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
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
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

/// Writes `review/signoff.json` atomically. Returns the signed unix time.
pub(crate) fn write_signoff(case_dir: &Path, examiner: &str) -> Result<i64, String> {
    let signed_unix = crate::util::now_unix()? as i64;
    let body = format!(
        "{{\"schema_version\":1,\"examiner\":{},\"signed_unix\":{signed_unix},\"note\":\"examiner name recorded as typed at finalize; not an authenticated identity\"}}\n",
        json_string(examiner)
    );
    let path = case_dir.join(crate::report::SIGNOFF_REL_PATH);
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
    // The sign-off must exist before make-report so the report names the reviewer.
    if let Err(err) = write_signoff(case_dir, examiner) {
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
        &["make-report".into(), case_text.clone(), "--rehash".into()],
        state,
    );
    let packaging = if report.is_ok() {
        run_step(exe, &["package-case".into(), case_text.clone()], state)
    } else {
        report.clone()
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
pub(crate) fn api_recover_deleted(state: &SharedState) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
    let Some((image_path, partition_offset)) = newest_tsk_inspection(&case_dir) else {
        return "{\"ok\":false,\"error\":\"파일시스템 조사 결과가 없습니다 — 먼저 E01/이미지 분석을 실행하십시오.\"}"
            .to_string();
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
    {
        let mut guard = state_lock(state);
        guard.phase = "finalizing";
        guard.busy = true;
        guard.error = None;
        guard.logs.push(format!(
            "삭제 영상 후보 복구 중… ({} @ 오프셋 {partition_offset})",
            image_path.display()
        ));
    }
    let case_text = case_dir.to_string_lossy().to_string();
    let recover = run_step(
        &exe,
        &[
            "recover-batch".into(),
            case_text.clone(),
            image_path.to_string_lossy().to_string(),
            "--deleted-videos".into(),
            "--partition-offset".into(),
            partition_offset.to_string(),
        ],
        state,
    );
    let review = if recover.is_ok() {
        run_step(&exe, &["make-review".into(), case_text.clone()], state)
    } else {
        recover.clone()
    };
    let mut guard = state_lock(state);
    match (recover, review) {
        (Ok(recover_out), Ok(_)) => {
            guard.phase = "review-ready";
            guard.busy = false;
            guard.logs.push(recover_out.clone());
            guard
                .logs
                .push("복구 완료 — 뷰어를 새로 열면 복구된 항목이 보입니다.".into());
            "{\"ok\":true}".to_string()
        }
        (recover, review) => {
            let error = recover.err().or_else(|| review.err()).unwrap_or_default();
            guard.phase = "review-ready";
            guard.busy = false;
            guard.error = Some(error.clone());
            serde_json::json!({"ok": false, "error": error}).to_string()
        }
    }
}

/// `POST /api/carve`: run bounded signature carving over the case's
/// exported raw image (evidence/images/evidence.raw), then regenerate the
/// review bundle so carved candidates show up in the viewer. Requires the
/// full E01 pipeline (direct triage never exports a raw image).
pub(crate) fn api_carve(request: &Request, state: &SharedState) -> String {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let _busy = match try_acquire_busy(state) {
        Ok(guard) => guard,
        Err(err) => return api_err(&err),
    };
    let raw = case_dir.join("evidence/images/evidence.raw");
    if !raw.is_file() {
        return "{\"ok\":false,\"error\":\"카빙할 raw 이미지가 없습니다 — E01을 '빠른 검토' 없이 분석(전체 export)하면 사용할 수 있습니다.\"}".to_string();
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
    {
        let mut guard = state_lock(state);
        guard.phase = "finalizing";
        guard.busy = true;
        guard.error = None;
        guard
            .logs
            .push(format!("시그니처 카빙 실행 중… ({})", raw.display()));
    }
    let case_text = case_dir.to_string_lossy().to_string();
    let mut args = vec![
        "carve-file".into(),
        case_text.clone(),
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
    let carve = run_step(&exe, &args, state);
    let review = if carve.is_ok() {
        run_step(&exe, &["make-review".into(), case_text.clone()], state)
    } else {
        carve.clone()
    };
    let mut guard = state_lock(state);
    match (carve, review) {
        (Ok(carve_out), Ok(_)) => {
            guard.phase = "review-ready";
            guard.busy = false;
            guard.logs.push(carve_out.clone());
            guard
                .logs
                .push("카빙 완료 — 뷰어를 새로 열면 카빙 후보가 보입니다.".into());
            "{\"ok\":true}".to_string()
        }
        (carve, review) => {
            let error = carve.err().or_else(|| review.err()).unwrap_or_default();
            guard.phase = "review-ready";
            guard.busy = false;
            guard.error = Some(error.clone());
            serde_json::json!({"ok": false, "error": error}).to_string()
        }
    }
}
