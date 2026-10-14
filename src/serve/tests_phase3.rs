//! Phase-3 track A (server / OS): write-op registration (S1), reserved
//! device names (S2), auth before body (S3), side work vs. a new start
//! (S4), remote-path stats (S5/S6), shutdown vs. job starts (S7), POSIX
//! `//` paths (S8), default case folder (P1), busy lease epochs (P2),
//! process-tracking warnings (P6), error mode (P7), fatal-signal cleanup
//! (P9) and the in-flight slot guard (P11).

use super::*;
use std::ffi::OsString;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-p3-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn request(method: &str, path: &str, body: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: String::new(),
        body: body.into(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    }
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("invalid JSON {err}: {text}"))
}

fn bound(case: &Path) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case.to_path_buf());
    state.media_roots = vec![case.to_path_buf()];
    state.phase = "review-ready";
    Arc::new(Mutex::new(state))
}

fn running_clip(output: PathBuf) -> ClipJob {
    ClipJob {
        id: "clip-1".into(),
        kind: "export-clip",
        key: "k".into(),
        record_id: "vid_0001".into(),
        rel_path: output.to_string_lossy().to_string(),
        output,
        started_unix: 1,
        finished_unix: None,
        state: "running",
        sha256: None,
        size_bytes: None,
        error: None,
        cancel_requested: false,
        progress_path: None,
        expected_bytes: None,
        join: None,
    }
}

fn start_body(source: &Path, case: &Path) -> String {
    serde_json::json!({
        "source_path": source.to_string_lossy(),
        "case_dir": case.to_string_lossy(),
    })
    .to_string()
}

// ---- S1: export-selected / capture-frame are registered write ops ----

#[test]
fn write_ops_block_shutdown_finalize_open_case_and_start_until_released() {
    let case = scratch("s1-case");
    let other = scratch("s1-other");
    std::fs::write(other.join("case.json"), br#"{"case_id":"FT-o"}"#).unwrap();
    let state = bound(&case);
    let (op, op_case) = try_begin_write_op(&state).expect("write op");
    assert_eq!(op_case, case);
    assert_eq!(state_lock(&state).write_ops, 1);
    assert_eq!(parse(&api_status(&state))["writes_running"], 1);

    // Shutdown reports running work instead of exiting mid-write.
    let shutdown = parse(&api_shutdown(&state));
    assert_eq!(shutdown["running"], true, "{shutdown}");
    assert!(!state_lock(&state).shutdown_requested());
    // R3: cancel asks the write op to stop between items and says what
    // still holds shutdown back.
    let cancel = parse(&api_cancel(&state));
    assert_eq!(cancel["writes_running"], 1, "{cancel}");
    assert_eq!(cancel["writes_cancelled"], 1, "{cancel}");
    assert!(op.cancelled(), "the running write op sees the cancel");
    // Finalize cannot start sealing.
    let seal = try_begin_seal(&state).err().expect("seal refused");
    assert!(seal.contains("최종화 차단"), "{seal}");
    // Open-case cannot rebind the case under the write.
    let body = serde_json::json!({"case_dir": other.to_string_lossy()}).to_string();
    let opened = parse(&api_open_case(
        &request("POST", "/api/open-case", &body),
        &state,
    ));
    assert_eq!(opened["ok"], false, "{opened}");
    assert_eq!(state_lock(&state).case_dir.as_deref(), Some(case.as_path()));
    // A new analysis cannot rebind it either (S4).
    let started = parse(&api_start(
        &request("POST", "/api/start", &start_body(&other, &other)),
        &state,
    ));
    assert_eq!(started["ok"], false, "{started}");
    assert!(!state_lock(&state).busy);

    drop(op);
    assert_eq!(state_lock(&state).write_ops, 0);
    let seal = try_begin_seal(&state).expect("seal once the write is done");
    assert!(state_lock(&state).sealing);
    // While sealing, new writes are refused (capture-frame through the API).
    let capture = parse(&api_capture_frame(
        &request(
            "POST",
            "/api/capture-frame",
            r#"{"id":"vid_1","image":"/9j/"}"#,
        ),
        &state,
    ));
    assert_eq!(capture["ok"], false, "{capture}");
    assert!(
        capture["error"].as_str().unwrap().contains("최종화"),
        "{capture}"
    );
    assert!(try_begin_write_op(&state).is_err());
    drop(seal);
    assert!(!state_lock(&state).sealing);
    assert!(try_begin_write_op(&state).is_ok());
    assert_eq!(state_lock(&state).write_ops, 0, "dropped temporary lease");
    let _ = std::fs::remove_dir_all(case);
    let _ = std::fs::remove_dir_all(other);
}

#[test]
fn write_op_lease_is_released_when_its_thread_panics() {
    let case = scratch("s1-panic");
    let state = bound(&case);
    let worker = Arc::clone(&state);
    let joined = thread::spawn(move || {
        let _op = try_begin_write_op(&worker).unwrap();
        panic!("handler failure");
    })
    .join();
    assert!(joined.is_err());
    assert_eq!(state_lock(&state).write_ops, 0);
    let _ = std::fs::remove_dir_all(case);
}

#[test]
fn export_selected_refuses_while_sealing() {
    let case = scratch("s1-export");
    let state = bound(&case);
    state_lock(&state).sealing = true;
    let response = parse(&api_export_selected(
        &request(
            "POST",
            "/api/export-selected",
            r#"{"items":[{"id":"vid_1","path":"x"}]}"#,
        ),
        &state,
    ));
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        !case.join("exports").exists(),
        "nothing may be written while finalize seals the case"
    );
    let _ = std::fs::remove_dir_all(case);
}

// ---- S2: reserved Windows device names, regular-file signature probe ----

#[test]
fn reserved_device_names_table() {
    use super::http::is_reserved_device_name as reserved;
    for name in [
        "CON",
        "con",
        "PRN",
        "AUX",
        "NUL",
        "nul.txt",
        "NUL.tar.gz",
        "COM1",
        "com9",
        "COM0",
        "LPT1",
        "lpt9.log",
        "COM¹",
        "LPT³",
        "CONIN$",
        "conout$",
        "CLOCK$",
        "NUL ",
        "aux .mp4",
        "COM1:stream",
    ] {
        assert!(reserved(name), "{name} must be reserved");
    }
    for name in [
        "",
        "CONSOLE",
        "COM10",
        "COM",
        "LPT",
        "COMA",
        "nulls",
        "auxiliary.mp4",
        "C:",
        "case1",
        " NUL",
        "COM1x",
    ] {
        assert!(!reserved(name), "{name} is an ordinary name");
    }
}

#[test]
fn reserved_device_paths_are_refused_with_windows_semantics_only() {
    use super::http::{RESERVED_DEVICE_ERROR, reject_device_path_on, reject_remote_path_on};
    for raw in [
        r"C:\x\COM1",
        r"C:\evidence\nul.txt",
        "C:/x/LPT1/a.mp4",
        r"D:\AUX",
        r"\\?\C:\x\CON",
        r"\\nas\share\COM2\a.mp4",
    ] {
        assert_eq!(
            reject_remote_path_on(raw, &[], true),
            Err(RESERVED_DEVICE_ERROR),
            "{raw}"
        );
        assert_eq!(
            reject_device_path_on(raw, true),
            Err(RESERVED_DEVICE_ERROR),
            "{raw}"
        );
    }
    for raw in [r"C:\x\COM10", r"C:\x\console.mp4", r"C:\cases\a"] {
        assert_eq!(reject_remote_path_on(raw, &[], true), Ok(()), "{raw}");
    }
    // Ordinary file names off Windows.
    assert_eq!(reject_remote_path_on("/evidence/COM1", &[], false), Ok(()));
    assert_eq!(reject_device_path_on("/evidence/nul.txt", false), Ok(()));
}

/// R4: plain (Win32-namespace) spellings stay refused even with an
/// extension (legacy device semantics); the verbatim local form skips
/// Win32 name parsing, so `\\?\C:\…\aux.mp4` is an ordinary file — only a
/// bare device component stays refused there.
#[test]
fn verbatim_local_paths_admit_device_like_file_names_with_an_extension() {
    use super::http::{RESERVED_DEVICE_ERROR, reject_device_path_on, reject_remote_path_on};
    for raw in [
        r"\\?\C:\evidence\aux.mp4",
        r"\\?\C:\evidence\NUL.txt",
        r"\\?\D:\cam\com1.avi",
    ] {
        assert_eq!(reject_remote_path_on(raw, &[], true), Ok(()), "{raw}");
        assert_eq!(reject_device_path_on(raw, true), Ok(()), "{raw}");
    }
    for raw in [
        r"C:\evidence\aux.mp4",
        r"C:\evidence\AUX",
        r"\\?\C:\evidence\AUX",
        r"\\?\C:\evidence\nul.",
        r"\\?\C:\evidence\CON ",
        r"\\?\C:\COM1\a.mp4",
        r"\\?\C:\x\con:stream",
    ] {
        assert_eq!(
            reject_remote_path_on(raw, &[], true),
            Err(RESERVED_DEVICE_ERROR),
            "{raw}"
        );
    }
}

#[test]
fn signature_probe_opens_only_regular_files_with_eight_bytes() {
    use super::system::{has_ewf_signature_file, signature_probe_allowed};
    let dir = scratch("s2-probe");
    let short = dir.join("short.E01");
    std::fs::write(&short, b"EVF").unwrap();
    let full = dir.join("full.E01");
    std::fs::write(&full, b"EVF\x09\x0d\x0a\xff\x00rest").unwrap();
    assert!(!signature_probe_allowed(&std::fs::metadata(&dir).unwrap()));
    assert!(!signature_probe_allowed(
        &std::fs::metadata(&short).unwrap()
    ));
    assert!(signature_probe_allowed(&std::fs::metadata(&full).unwrap()));
    assert!(!has_ewf_signature_file(&short));
    assert!(has_ewf_signature_file(&full));
    assert!(!has_ewf_signature_file(&dir));
    let _ = std::fs::remove_dir_all(dir);
}

/// The NUL device is not a regular file: the probe must decline it
/// without opening (a read on a real serial device would block).
#[cfg(windows)]
#[test]
fn signature_probe_declines_the_nul_device_on_windows() {
    assert!(!super::system::has_ewf_signature_file(Path::new("NUL")));
    assert!(!super::system::has_ewf_signature_file(Path::new(
        r"C:\definitely-missing\COM1"
    )));
    // The central guard refuses the reserved name before any handler.
    let mut browse = request("GET", "/api/browse", "");
    browse.query = r"path=C:\x\COM1".into();
    assert_eq!(
        super::http::remote_path_violation(&browse, &[]),
        Some(super::http::RESERVED_DEVICE_ERROR)
    );
}

// ---- S3: auth runs on the header alone ----

#[test]
fn unauthenticated_request_is_refused_before_its_body_is_read() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).token = Some("phase3-token".into());
    let server_state = Arc::clone(&state);
    thread::spawn(move || serve_on(listener, server_state));
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // A 1 MB body is announced but never sent: the old reader waited for
    // it (up to 4x the header deadline) before checking the token.
    stream
        .write_all(
            b"POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 1000000\r\n\r\n",
        )
        .unwrap();
    let started = std::time::Instant::now();
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&buffer).to_string();
    assert!(text.starts_with("HTTP/1.1 401"), "{text}");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "refusal must not wait for the body ({:?})",
        started.elapsed()
    );
    state_lock(&state).request_shutdown();
}

// ---- S4 / S7: start refusals ----

#[test]
fn start_is_refused_while_a_clip_export_or_join_runs() {
    let case = scratch("s4-case");
    let source = scratch("s4-src");
    let state = bound(&case);
    state_lock(&state)
        .clip_jobs
        .push(running_clip(case.join("artifacts/clips/a.mp4")));
    let response = parse(&api_start(
        &request("POST", "/api/start", &start_body(&source, &case)),
        &state,
    ));
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("내보내기·이어붙이기"),
        "{response}"
    );
    let guard = state_lock(&state);
    assert!(!guard.busy);
    assert_eq!(guard.phase, "review-ready", "the bound case is untouched");
    drop(guard);
    let _ = std::fs::remove_dir_all(case);
    let _ = std::fs::remove_dir_all(source);
}

#[test]
fn no_job_starts_once_shutdown_was_requested() {
    let case = scratch("s7-case");
    let source = scratch("s7-src");
    let state = bound(&case);
    state_lock(&state).request_shutdown();
    let refused = |result: Result<(), String>| {
        let err = result.expect_err("must be refused after shutdown");
        assert_eq!(err, super::state::SHUTDOWN_ERROR);
    };
    refused(try_acquire_busy(&state).map(|_| ()));
    refused(try_lease_busy(&state).map(|_| ()));
    refused(try_begin_write_op(&state).map(|_| ()));
    let started = parse(&api_start(
        &request("POST", "/api/start", &start_body(&source, &case)),
        &state,
    ));
    assert_eq!(started["ok"], false, "{started}");
    assert_eq!(
        started["error"].as_str(),
        Some(super::state::SHUTDOWN_ERROR)
    );
    // Clip exports register under the same check.
    let exe = PathBuf::from("frametrace-not-run");
    let clip = parse(&super::media::export_clip_with_exe(
        &request(
            "POST",
            "/api/export-clip",
            r#"{"id":"vid_000001","start":"0","duration":"1"}"#,
        ),
        &state,
        &exe,
    ));
    assert_eq!(clip["ok"], false, "{clip}");
    assert_eq!(clip["error"].as_str(), Some(super::state::SHUTDOWN_ERROR));
    let guard = state_lock(&state);
    assert!(!guard.busy);
    assert!(guard.clip_jobs.is_empty());
    drop(guard);
    let _ = std::fs::remove_dir_all(case);
    let _ = std::fs::remove_dir_all(source);
}

// ---- S5 / S6: no stat of remote paths ----

#[test]
fn recent_case_keys_never_resolve_remote_paths() {
    use super::recent::canonical_case_dir_with;
    let resolve_forbidden = |path: &Path| -> std::io::Result<PathBuf> {
        panic!("{} must not be resolved", path.display())
    };
    assert_eq!(
        canonical_case_dir_with(Path::new(r"\\evil\share\case\"), &resolve_forbidden),
        r"\\evil\share\case"
    );
    assert_eq!(
        canonical_case_dir_with(Path::new(r"\\.\PhysicalDrive0"), &resolve_forbidden),
        r"\\.\PhysicalDrive0"
    );
    // A local path is still resolved.
    let local = scratch("s5-local");
    let resolved = std::cell::Cell::new(false);
    let _ = canonical_case_dir_with(&local, &|path| {
        resolved.set(true);
        path.canonicalize()
    });
    assert!(resolved.get());
    // forget with a UNC case_dir goes through the same key (no stat).
    let file = local.join("recent.json");
    assert!(!forget_recent_in(&file, r"\\evil\share\case").unwrap());
    let _ = std::fs::remove_dir_all(local);
}

#[test]
fn case_info_checkpoint_skips_remote_source_paths() {
    use super::system::checkpoint_summary_json;
    let dir = scratch("s6");
    let local = dir.join("a.mp4");
    std::fs::write(&local, b"12345").unwrap();
    let mtime = std::fs::metadata(&local)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let text = [
        serde_json::json!({"checkpoint": "scan-folder", "run_id": "r"}).to_string(),
        serde_json::json!({"id": "vid_1", "source_path": r"\\evil\share\a.mp4", "size_bytes": 5, "modified_unix": mtime}).to_string(),
        serde_json::json!({"id": "vid_2", "source_path": local.to_string_lossy(), "size_bytes": 5, "modified_unix": mtime}).to_string(),
    ]
    .join("\n");
    let summary = checkpoint_summary_json(&text, Some(2), &[]);
    assert_eq!(summary["done"], 2, "{summary}");
    assert_eq!(summary["fingerprint_skipped_remote"], 1, "{summary}");
    assert_eq!(summary["fingerprint_matches"], 1, "{summary}");
    // Under an approved UNC root the share path is no longer refused.
    let roots = vec![PathBuf::from(r"\\evil\share")];
    let admitted = checkpoint_summary_json(&text, Some(2), &roots);
    assert_eq!(admitted["fingerprint_skipped_remote"], 0, "{admitted}");
    let _ = std::fs::remove_dir_all(dir);
}

// ---- S8: `//x` is local off Windows ----

#[test]
fn double_slash_is_unc_only_with_windows_semantics() {
    use super::http::{REMOTE_PATH_ERROR, reject_device_path_on, reject_remote_path_on};
    for raw in ["//mnt/evidence/a.mp4", "//srv/share", "/??/x", "///x"] {
        assert_eq!(reject_remote_path_on(raw, &[], false), Ok(()), "{raw}");
        assert_eq!(reject_device_path_on(raw, false), Ok(()), "{raw}");
    }
    assert_eq!(
        reject_remote_path_on("//srv/share", &[], true),
        Err(REMOTE_PATH_ERROR)
    );
    // Backslash UNC spellings stay refused everywhere.
    for windows in [true, false] {
        assert_eq!(
            reject_remote_path_on(r"\\srv\share", &[], windows),
            Err(REMOTE_PATH_ERROR)
        );
        assert_eq!(
            reject_remote_path_on(r"\/srv/share", &[], windows),
            Err(REMOTE_PATH_ERROR)
        );
    }
    // The platform default follows the host OS.
    let expected = if cfg!(windows) {
        Err(REMOTE_PATH_ERROR)
    } else {
        Ok(())
    };
    assert_eq!(super::http::reject_remote_path("//mnt/x", &[]), expected);
}

// ---- P1: default case folder ----

#[test]
fn default_case_root_table() {
    use super::jobs::{default_case_root, temp_case_dir_warning};
    let temp = PathBuf::from("tmp-root");
    let home = PathBuf::from("home-k");
    let docs = home.join("Documents");
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    };
    let none_dir = |_: &Path| false;
    let docs_dir = |path: &Path| path == docs.as_path();
    // Windows: USERPROFILE, never HOME.
    assert_eq!(
        default_case_root(
            true,
            &env(&[("USERPROFILE", "profile-k"), ("HOME", "home-k")]),
            &none_dir,
            &temp
        ),
        (PathBuf::from("profile-k"), false)
    );
    assert_eq!(
        default_case_root(true, &env(&[("HOME", "home-k")]), &none_dir, &temp),
        (temp.clone(), true)
    );
    // Elsewhere: $HOME/Documents when it exists, else $HOME.
    assert_eq!(
        default_case_root(false, &env(&[("HOME", "home-k")]), &docs_dir, &temp),
        (docs.clone(), false)
    );
    assert_eq!(
        default_case_root(false, &env(&[("HOME", "home-k")]), &none_dir, &temp),
        (home.clone(), false)
    );
    // USERPROFILE alone does not count off Windows; empty HOME neither.
    assert_eq!(
        default_case_root(
            false,
            &env(&[("USERPROFILE", "profile-k")]),
            &none_dir,
            &temp
        ),
        (temp.clone(), true)
    );
    assert_eq!(
        default_case_root(false, &env(&[("HOME", "")]), &none_dir, &temp),
        (temp.clone(), true)
    );
    let warning = temp_case_dir_warning(&temp.join("FrameTrace"));
    assert!(warning.contains("임시 폴더"), "{warning}");
    assert!(warning.contains("tmp-root"), "{warning}");
}

// ---- P2: busy lease epochs ----

#[test]
fn stale_busy_lease_never_clears_a_newer_jobs_flag() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let old = try_lease_busy(&state).unwrap();
    // The old job releases `busy` itself in its final update (as the
    // pipeline does), then a new job takes it.
    state_lock(&state).busy = false;
    let new = try_lease_busy(&state).unwrap();
    assert_ne!(old.epoch(), new.epoch());
    drop(old);
    assert!(state_lock(&state).busy, "the newer job keeps busy");
    drop(new);
    assert!(!state_lock(&state).busy);
    // Same for the synchronous guard.
    let guard = try_acquire_busy(&state).unwrap();
    state_lock(&state).busy = false;
    let lease = try_lease_busy(&state).unwrap();
    drop(guard);
    assert!(state_lock(&state).busy);
    drop(lease);
}

#[test]
fn panicking_pipeline_worker_frees_busy_and_reports_the_failure() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let lease = {
        let mut guard = state_lock(&state);
        let lease = super::state::lease_busy_locked(&mut guard, &state).unwrap();
        guard.phase = "running";
        lease
    };
    let joined = thread::spawn(move || {
        let _lease = lease;
        panic!("pipeline bug");
    })
    .join();
    assert!(joined.is_err());
    let guard = state_lock(&state);
    assert!(!guard.busy, "a dead worker must not leave busy set");
    assert_eq!(guard.phase, "error");
    assert_eq!(
        guard.error.as_deref(),
        Some(super::state::ABANDONED_JOB_ERROR)
    );
    assert!(guard.logs.iter().any(|line| line.contains("비정상 종료")));
}

// ---- P6: process-tracking failure is a job warning ----

#[test]
fn process_tracking_warning_is_reported_once_per_message() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).steps[2] = StepStatus::Running;
    let message = super::proc::tracking_warning("scan-folder", "access denied");
    assert!(message.contains("scan-folder") && message.contains("access denied"));
    super::jobs::report_process_warning(&state, message.clone());
    super::jobs::report_process_warning(&state, message.clone());
    let guard = state_lock(&state);
    assert_eq!(guard.warnings, vec![(2, message.clone())]);
    assert_eq!(
        guard
            .logs
            .iter()
            .filter(|line| line.contains("access denied"))
            .count(),
        1
    );
    drop(guard);
    let status = parse(&api_status(&state));
    assert_eq!(status["warnings"][0]["step"], 2, "{status}");
}

// ---- P7: no modal OS error dialogs ----

#[cfg(windows)]
#[test]
fn error_mode_suppresses_critical_and_open_file_dialogs() {
    let mode = crate::util::suppress_os_error_dialogs();
    assert_eq!(
        mode & crate::util::SEM_FAILCRITICALERRORS,
        crate::util::SEM_FAILCRITICALERRORS
    );
    assert_eq!(
        mode & crate::util::SEM_NOOPENFILEERRORBOX,
        crate::util::SEM_NOOPENFILEERRORBOX
    );
    // R7: a faulting child must not park on the WER crash dialog.
    assert_eq!(
        mode & crate::util::SEM_NOGPFAULTERRORBOX,
        crate::util::SEM_NOGPFAULTERRORBOX
    );
    // Idempotent: a second call keeps the flags.
    assert_eq!(crate::util::suppress_os_error_dialogs() & mode, mode);
}

// ---- P9: fatal-signal cleanup ----

#[test]
fn fatal_signal_cleanup_kills_trees_then_drops_the_instance_file() {
    let order = std::cell::RefCell::new(Vec::new());
    let removed = super::proc::fatal_signal_cleanup(&|| order.borrow_mut().push("kill"), &|| {
        order.borrow_mut().push("remove");
        true
    });
    assert!(removed);
    assert_eq!(*order.borrow(), vec!["kill", "remove"]);
}

// ---- P11: in-flight slot ----

#[test]
fn in_flight_slot_is_released_when_the_handler_panics() {
    let counter = Arc::new(AtomicUsize::new(1));
    let slot = InFlightSlot(Arc::clone(&counter));
    let joined = thread::spawn(move || {
        let _slot = slot;
        panic!("handler bug");
    })
    .join();
    assert!(joined.is_err());
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    // And a slot that never reached its thread (failed spawn) too.
    counter.store(1, Ordering::SeqCst);
    drop(InFlightSlot(Arc::clone(&counter)));
    assert_eq!(counter.load(Ordering::SeqCst), 0);
}
