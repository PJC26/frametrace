//! Track D hardening tests: default auth token, process-tree cancel,
//! finalize sign-off + manifest checksum, source re-hash custody table.

use super::*;

// Included per test file (no mod.rs line needed); see the helper's header.
#[allow(clippy::duplicate_mod)]
#[path = "tests_fake_tools.rs"]
mod fake_tools;
use fake_tools::{cmd_grandchild_sleeper, kill_pid, pid_alive, write_fake};
// mod.rs re-exports this only for unix (B-owned gate): reach it by path elsewhere.
#[cfg(not(unix))]
use super::jobs::SIGNOFF_PENDING_REL_PATH;

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-hard-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn http_get(addr: std::net::SocketAddr, head: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(format!("{head}\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
        .unwrap();
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).unwrap();
    String::from_utf8_lossy(&buffer).to_string()
}

#[test]
fn token_is_random_by_default_and_env_controls_it() {
    let first = resolve_token(None, None).unwrap().expect("default token");
    let second = resolve_token(None, None).unwrap().unwrap();
    assert_eq!(first.len(), 32);
    assert!(valid_token(&first), "{first}");
    assert_ne!(first, second, "tokens must be random per process");
    // Blank values behave like unset.
    assert!(resolve_token(Some("  "), Some("0")).unwrap().is_some());
    // FRAMETRACE_NO_TOKEN=1 disables.
    assert_eq!(resolve_token(None, Some("1")).unwrap(), None);
    // An explicit token wins over the opt-out and is validated.
    assert_eq!(
        resolve_token(Some("pinned-token-1"), Some("1"))
            .unwrap()
            .as_deref(),
        Some("pinned-token-1")
    );
    assert!(resolve_token(Some("short"), None).is_err());
}

#[test]
fn default_token_gates_every_request_and_opt_out_does_not() {
    let serve = |token: Option<String>| {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let state: SharedState = Arc::new(Mutex::new(JobState::new()));
        state_lock(&state).token = token;
        thread::spawn(move || serve_on(listener, state));
        addr
    };
    let token = resolve_token(None, None).unwrap().unwrap();
    let addr = serve(Some(token.clone()));
    let denied = http_get(addr, "GET /api/status HTTP/1.1");
    assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
    let page_denied = http_get(addr, "GET / HTTP/1.1");
    assert!(page_denied.starts_with("HTTP/1.1 401"), "{page_denied}");
    let allowed = http_get(
        addr,
        &format!("GET /api/status HTTP/1.1\r\nX-FrameTrace-Token: {token}"),
    );
    assert!(allowed.starts_with("HTTP/1.1 200"), "{allowed}");
    // The workstation page itself loads via the ?token= URL and plants the
    // cookie that later fetches carry.
    let page = http_get(addr, &format!("GET /?token={token} HTTP/1.1"));
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(
        page.contains(&format!("Set-Cookie: ft_token_{}={token}", addr.port())),
        "{page}"
    );

    // FRAMETRACE_NO_TOKEN=1 -> no token on the state -> reads need no proof.
    let open = serve(resolve_token(None, Some("1")).unwrap());
    let status = http_get(open, "GET /api/status HTTP/1.1");
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
}

/// Serializes the tests in this file that mutate process-global env vars
/// (R4 F9); a poisoned lock (an earlier assertion failed) is still usable.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn instance_file_records_token_and_discovery_probe_sends_it() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("instance");
    let file = dir.join("server.json");
    // Only this test touches the override; other tests never read it.
    unsafe {
        std::env::set_var("FRAMETRACE_INSTANCE_FILE", &file);
    }
    write_instance_file(8477, "nonce-abc", Some("tok-0123456789"));
    let record = read_instance_record().expect("instance record");
    assert_eq!(record.0, 8477);
    assert_eq!(record.1, "nonce-abc");
    assert_eq!(record.2.as_deref(), Some("tok-0123456789"));
    assert_eq!(token_for_port(8477).as_deref(), Some("tok-0123456789"));
    assert_eq!(token_for_port(9999), None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "instance file holds a secret");
    }
    // Probe against a live token-gated server: succeeds only with the token.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).token = Some("tok-0123456789".into());
    thread::spawn(move || serve_on(listener, state));
    let without = probe_status_body(port, None).unwrap_or_default();
    assert!(String::from_utf8_lossy(&without).contains("401"));
    let with = probe_status_body(port, Some("tok-0123456789")).unwrap();
    assert!(String::from_utf8_lossy(&with).contains("\"app\":\"frametrace\""));
    unsafe {
        std::env::remove_var("FRAMETRACE_INSTANCE_FILE");
    }
    // No-token servers omit the field entirely.
    let plain = dir.join("plain.json");
    unsafe {
        std::env::set_var("FRAMETRACE_INSTANCE_FILE", &plain);
    }
    write_instance_file(8478, "nonce-def", None);
    assert_eq!(read_instance_record().unwrap().2, None);
    unsafe {
        std::env::remove_var("FRAMETRACE_INSTANCE_FILE");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// R5 U9: how long the unix grandchild sleeps, and the elapsed bound of
/// the cancel test below. The sleeper MUST outlive the bound: with the old
/// `sleep 30` under a 90 s bound, a cancel that killed nothing simply waited
/// for the sleeper to exit on its own, found it gone and passed.
const GRANDCHILD_SLEEP_SECS: u64 = 300;
const CANCEL_ELAPSED_BOUND_SECS: u64 = 90;

#[test]
fn cancel_test_sleeper_outlives_its_elapsed_bound() {
    const { assert!(GRANDCHILD_SLEEP_SECS > CANCEL_ELAPSED_BOUND_SECS) };
    let unix_body = grandchild_sleeper_unix(Path::new("/tmp/x.pid"));
    assert!(
        unix_body.contains(&format!("sleep {GRANDCHILD_SLEEP_SECS} &")),
        "{unix_body}"
    );
    // The Windows sleeper (`ping -n 300`) is bounded the same way.
    assert!(cmd_grandchild_sleeper(Path::new("x.pid")).contains("'-n','300'"));
}

/// R5 U10: no Windows fake tool waits by busy-spinning (`:spin` straight
/// into `goto spin`, or a `goto` gate loop without a sleep step).
#[test]
fn windows_fake_tools_never_busy_spin() {
    let bare_spin = [":spin", "\\n", "goto spin"].concat();
    for (name, src) in [
        ("tests_jobs.rs", include_str!("tests_jobs.rs")),
        ("tests_review3.rs", include_str!("tests_review3.rs")),
        ("tests_hardening.rs", include_str!("tests_hardening.rs")),
        ("tests_fake_tools.rs", include_str!("tests_fake_tools.rs")),
    ] {
        assert!(!src.contains(&bare_spin), "{name}: busy-spin wait loop");
    }
    let gate = fake_tools::cmd_wait_for_file(Path::new("gate"));
    assert!(
        gate.contains("ping -n 2"),
        "cmd_wait_for_file sleeps: {gate}"
    );
    assert!(fake_tools::CMD_SPIN.contains("ping -n 2"));
}

fn grandchild_sleeper_unix(pidfile: &Path) -> String {
    format!(
        "sleep {GRANDCHILD_SLEEP_SECS} & echo $! > '{}'; wait",
        pidfile.display()
    )
}

#[test]
fn run_step_cancel_kills_the_grandchildren_too() {
    let dir = scratch("tree");
    let pidfile = dir.join("grandchild.pid");
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let canceller = {
        let state = Arc::clone(&state);
        let pidfile = pidfile.clone();
        thread::spawn(move || {
            for _ in 0..1600 {
                if std::fs::read_to_string(&pidfile).is_ok_and(|text| !text.trim().is_empty()) {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            state_lock(&state).cancel_requested = true;
        })
    };
    let script = write_fake(
        &dir,
        &grandchild_sleeper_unix(&pidfile),
        &cmd_grandchild_sleeper(&pidfile),
    );
    let started = std::time::Instant::now();
    let result = run_step(&script, &[], &state);
    canceller.join().unwrap();
    assert!(result.is_err(), "cancelled step reports an error");
    // PowerShell start-up dominates on a loaded runner; a missed kill hangs
    // for the sleeper's 300 s (sleep or ping), well past this bound.
    assert!(started.elapsed() < Duration::from_secs(CANCEL_ELAPSED_BOUND_SECS));
    let pid: String = std::fs::read_to_string(&pidfile)
        .expect("grandchild pid recorded")
        .trim()
        .to_string();
    // Allow the kernel a moment to deliver SIGKILL and init to reap.
    let mut gone = false;
    for _ in 0..80 {
        if !pid_alive(&pid) {
            gone = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !gone {
        // Do not leak the sleeper if the assertion below fails.
        kill_pid(&pid);
    }
    assert!(gone, "grandchild {pid} survived cancel (orphaned)");
    let _ = std::fs::remove_dir_all(dir);
}

/// The real CLI binary next to the test executable (cargo builds the bins
/// for `cargo test`); `api_finalize` and fixtures drive it as a subprocess.
fn cli_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let name = if cfg!(windows) {
        "frametrace.exe"
    } else {
        "frametrace"
    };
    let bin = exe.parent().unwrap().parent().unwrap().join(name);
    assert!(
        bin.is_file(),
        "frametrace binary not built at {} (run `cargo test` / `cargo build` first)",
        bin.display()
    );
    bin
}

fn run_cli(args: &[&str]) {
    let output = Command::new(cli_bin()).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "frametrace {args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Case with one indexed (hashed) source file. Returns (root, case, source).
fn case_fixture(label: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(label);
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let source = src_dir.join("a.mp4");
    std::fs::write(&source, b"fixture-video-bytes-0123456789").unwrap();
    let case = root.join("case");
    run_cli(&["init-case", case.to_str().unwrap()]);
    run_cli(&[
        "scan-folder",
        case.to_str().unwrap(),
        src_dir.to_str().unwrap(),
        "--hash",
        "--no-ffprobe",
    ]);
    (root, case, source)
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn finalize_request(body: &str) -> Request {
    Request {
        method: "POST".into(),
        path: "/api/finalize".into(),
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

#[test]
fn finalize_requires_examiner_and_records_signoff_with_manifest_checksum() {
    let (root, case, _source) = case_fixture("finalize");
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());

    // No reviewer name -> refused before anything is written.
    let refused = api_finalize(&finalize_request("{}"), &state);
    assert!(refused.contains("\"ok\":false"), "{refused}");
    assert!(!case.join(crate::report::SIGNOFF_REL_PATH).exists());

    let examiner = "홍길동 <script>x</script>";
    let response = finalize_with_exe(&cli_bin(), &case, examiner, &state);
    let value: serde_json::Value = serde_json::from_str(&response).expect(&response);
    assert_eq!(value["ok"], true, "{response}");
    let checksum = value["manifest_sha256"].as_str().expect(&response);
    assert!(is_sha256_hex(checksum), "{checksum}");
    assert_eq!(value["examiner"], examiner);
    let package_dir = PathBuf::from(value["package_dir"].as_str().unwrap());
    assert_eq!(
        checksum,
        crate::audit::digest_file(&package_dir.join("manifest.sha256")).unwrap(),
        "checksum is the SHA-256 of the packaged manifest.sha256"
    );

    let signoff = std::fs::read_to_string(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap();
    assert_eq!(
        crate::report::parse_signoff(&signoff).map(|(name, _)| name),
        Some(examiner.to_string())
    );
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(
        report.contains("검토자: <strong>홍길동 &lt;script&gt;x&lt;/script&gt;</strong>"),
        "report must name the reviewer, HTML-escaped"
    );
    assert!(!report.contains("<script>x</script>"));
    assert!(report.contains("신원 인증은 아닙니다"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn custody_table_flags_tampered_source_and_confirms_untouched_one() {
    let (root, case, source) = case_fixture("custody");

    // No re-hash run yet: the statement says so instead of implying a pass.
    crate::custody::generate(&case).unwrap();
    let html = std::fs::read_to_string(case.join(crate::custody::CUSTODY_REL_PATH)).unwrap();
    assert!(html.contains("No completed live source re-hash"), "{html}");

    // Untouched source -> MATCH row, no mismatch banner.
    crate::anomaly::scan_case(&case, true).unwrap();
    crate::custody::generate(&case).unwrap();
    let html = std::fs::read_to_string(case.join(crate::custody::CUSTODY_REL_PATH)).unwrap();
    assert!(html.contains(">MATCH<"), "{html}");
    assert!(!html.contains(">MISMATCH<"));
    let summary = crate::anomaly::latest_hash_verify_summary(&case).unwrap();
    assert_eq!((summary.matched, summary.mismatched), (1, 0));

    // Tamper with the source after indexing, re-verify, regenerate.
    let recorded = crate::audit::digest_file(&source).unwrap();
    std::fs::write(&source, b"fixture-video-bytes-0123456789-TAMPERED").unwrap();
    crate::anomaly::scan_case(&case, true).unwrap();
    crate::custody::generate(&case).unwrap();
    let html = std::fs::read_to_string(case.join(crate::custody::CUSTODY_REL_PATH)).unwrap();
    assert!(html.contains(">MISMATCH<"), "{html}");
    assert!(html.contains("banner bad"), "{html}");
    let live = crate::audit::digest_file(&source).unwrap();
    assert!(html.contains(&live), "observed digest shown");
    assert!(html.contains(&recorded), "recorded digest shown beside it");
    let summary = crate::anomaly::latest_hash_verify_summary(&case).unwrap();
    assert_eq!((summary.matched, summary.mismatched), (0, 1));
    let _ = std::fs::remove_dir_all(root);
}

/// SHOULD-FIX 6: a finalize whose package step fails must not leave a
/// sign-off behind (it used to write review/signoff.json before
/// make-report, so every later report named a reviewer for a finalize that
/// never completed).
#[test]
fn failed_finalize_leaves_no_signoff() {
    let (root, case, _source) = case_fixture("finalize-fail");
    let fake = write_fake(
        &root,
        "case \"$1\" in package-case) echo 'disk full' >&2; exit 1;; *) exit 0;; esac",
        "if \"%1\"==\"package-case\" (\necho disk full 1>&2\nexit /b 1\n)\nexit /b 0",
    );
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let response = finalize_with_exe(&fake, &case, "홍길동", &state);
    assert!(response.contains("\"ok\":false"), "{response}");
    assert!(
        !case.join(crate::report::SIGNOFF_REL_PATH).exists(),
        "failed finalize must not record a sign-off"
    );
    assert!(!case.join(SIGNOFF_PENDING_REL_PATH).exists());
    // R8-I9: the chained sign-off entry is followed by an abandon event
    // naming the same record, so it never reads as the latest sign-off.
    let lane = std::fs::read_to_string(case.join(crate::report::SIGNOFF_LOG_REL_PATH))
        .expect("finalize chains its sign-off before packaging");
    let events: Vec<serde_json::Value> = lane
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let signed = events
        .iter()
        .find(|e| e["event"] == "signoff")
        .and_then(|e| e["signoff_record_sha256"].as_str())
        .expect("sign-off entry");
    assert!(
        events
            .iter()
            .any(|e| e["event"] == crate::report::SIGNOFF_ABANDONED_EVENT
                && e["signoff_record_sha256"] == signed
                && e["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("disk full"))),
        "{lane}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// SHOULD-FIX 6: after a sign-off, changing the review marks makes the
/// next report say the sign-off is not for the current content.
#[test]
fn report_flags_marks_changed_after_signoff() {
    let (root, case, _source) = case_fixture("signoff-stale");
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let response = finalize_with_exe(&cli_bin(), &case, "홍길동", &state);
    assert!(response.contains("\"ok\":true"), "{response}");
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(
        report.contains("판독 마크는 변경되지 않았습니다"),
        "fresh sign-off is current"
    );

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(case.join("case.json")).unwrap()).unwrap();
    let marks = serde_json::json!({
        "schema_version": 1,
        "case_id": manifest["case_id"],
        "marks": [{"id": "vid_000001", "status": "important", "note": "after sign-off"}],
    });
    let marks_path = root.join("marks.json");
    std::fs::write(&marks_path, marks.to_string()).unwrap();
    crate::cli::handlers::import_marks_quiet(&case, &marks_path).unwrap();
    crate::cli::handlers::make_report(&case, false, false, None).unwrap();
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(
        report.contains("이 기록 이후 판독 마크가 변경되었습니다"),
        "an old sign-off must not read as current"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// §10.2: finalize re-runs make-review before package-case, so the
/// packaged review/data-bundle.js carries a mark saved AFTER the last
/// analysis (the bundle used to be whatever the pipeline last wrote).
#[test]
fn finalize_rebuilds_review_bundle_with_marks_saved_after_analysis() {
    let (root, case, _source) = case_fixture("finalize-bundle");
    run_cli(&["make-review", case.to_str().unwrap()]);
    let bundle_before = std::fs::read_to_string(case.join("review/data-bundle.js")).unwrap();
    let note = "saved-after-analysis-7f3a";
    assert!(!bundle_before.contains(note));
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(case.join("case.json")).unwrap()).unwrap();
    let marks_path = root.join("marks.json");
    std::fs::write(
        &marks_path,
        serde_json::json!({
            "schema_version": 1,
            "case_id": manifest["case_id"],
            "marks": [{"id": "vid_000001", "status": "important", "note": note}],
        })
        .to_string(),
    )
    .unwrap();
    crate::cli::handlers::import_marks_quiet(&case, &marks_path).unwrap();

    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let response = finalize_with_exe(&cli_bin(), &case, "홍길동", &state);
    let value: serde_json::Value = serde_json::from_str(&response).expect(&response);
    assert_eq!(value["ok"], true, "{response}");
    let package_dir = PathBuf::from(value["package_dir"].as_str().unwrap());
    let packaged = std::fs::read_to_string(package_dir.join("review/data-bundle.js")).unwrap();
    assert!(
        packaged.contains(note),
        "packaged bundle must carry the mark saved after the last analysis"
    );
    let verification = crate::package::verify_package(&package_dir).unwrap();
    assert!(verification.passed(), "{:?}", verification.checks);
    let _ = std::fs::remove_dir_all(root);
}

/// SHOULD-FIX 5: marks still queued on disk are applied before sealing, so
/// the packaged review always carries the latest marks.
#[test]
fn finalize_applies_queued_marks_before_sealing() {
    let (root, case, _source) = case_fixture("finalize-queued");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(case.join("case.json")).unwrap()).unwrap();
    let queued = case.join("marks-pending");
    std::fs::create_dir_all(&queued).unwrap();
    std::fs::write(
        queued.join("0001.json"),
        serde_json::json!({
            "schema_version": 1,
            "case_id": manifest["case_id"],
            "marks": [{"id": "vid_000001", "status": "important", "note": "queued"}],
        })
        .to_string(),
    )
    .unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let response = finalize_with_exe(&cli_bin(), &case, "홍길동", &state);
    assert!(response.contains("\"ok\":true"), "{response}");
    assert_eq!(pending_marks_count(&case), 0);
    let marks = crate::case_db::load_review_marks(&case).unwrap();
    assert!(marks.iter().any(|m| m.note.as_deref() == Some("queued")));
    let _ = std::fs::remove_dir_all(root);
}

/// Sends a malformed request (answered with 400 before the handler ever
/// touches `JobState`) and returns the reply, or the I/O error when the
/// server did not answer within the timeout.
fn malformed_exchange(addr: std::net::SocketAddr) -> std::io::Result<String> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream
        .write_all(b"POST /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: x\r\n\r\n")?;
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer).to_string())
}

/// The accept loop must not depend on the global state lock: while some
/// handler holds `JobState`, new connections are still accepted and
/// served (here: requests that never need the lock). Previously the loop
/// took the lock every iteration to read the shutdown flag, so one long
/// lock hold stalled accepting and let the listen backlog overflow.
#[test]
fn serve_on_keeps_accepting_while_the_state_lock_is_held() {
    let listener = bind_loopback(0).unwrap();
    let addr = listener.local_addr().unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let server_state = Arc::clone(&state);
    let server = thread::spawn(move || serve_on(listener, server_state));
    let first = malformed_exchange(addr).expect("server answers");
    assert!(first.starts_with("HTTP/1.1 400"), "{first}");
    {
        let _held = state_lock(&state);
        for i in 0..8 {
            let reply = malformed_exchange(addr).unwrap_or_else(|err| {
                panic!("request {i} not served while the lock is held: {err}")
            });
            assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        }
    }
    state_lock(&state).request_shutdown();
    server.join().unwrap();
}
