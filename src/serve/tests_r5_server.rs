//! Round-5 track S (docs/PLAN_R5_2026-10-14.md): server and process fixes
//! S1–S15. Each test exercises the exact misbehaviour the round-4 review
//! found and fails on the code before the fix (where a test can reach
//! it; see the plan's "§S as implemented" for the exceptions).

use super::*;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r5s-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A minimal case folder the in-process marks import accepts.
fn temp_case(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::create_dir_all(dir.join("evidence/images")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-r5"}"#).unwrap();
    dir
}

fn request(method: &str, path: &str, query: &str, body: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: query.into(),
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

fn bound(case: &Path, phase: &'static str) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case.to_path_buf());
    state.media_roots = vec![case.to_path_buf()];
    state.phase = phase;
    Arc::new(Mutex::new(state))
}

fn wait_until(what: &str, limit: Duration, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + limit;
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn exchange(port: u16, raw: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).to_string()
}

fn start_server(state: &SharedState) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = Arc::clone(state);
    thread::spawn(move || serve_on(listener, serving));
    port
}

fn head_of(response: &str) -> &str {
    &response[..response.find("\r\n\r\n").unwrap_or(response.len())]
}

fn get(port: u16, target: &str) -> String {
    exchange(
        port,
        &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
    )
}

/// A fake frametrace CLI: every subcommand exits 0; `stderr_line`, when
/// given, is printed to stderr first (after a chatty stdout).
fn fake_cli(dir: &Path, stderr_line: Option<&str>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut body = String::from("#!/bin/sh\n");
        if let Some(line) = stderr_line {
            // Double quotes: R8-S7 lines carry `${FRAMETRACE_STEP_WARNINGS}`.
            body.push_str(&format!("echo \"{line}\" >&2\n"));
            body.push_str("i=0; while [ $i -lt 3000 ]; do echo \"noise line $i ................................\" >&2; i=$((i+1)); done\n");
        }
        body.push_str("echo \"ARGS: $*\"\nexit 0\n");
        let path = dir.join("fake-r5.sh");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    #[cfg(windows)]
    {
        let mut body = String::from("@echo off\r\n");
        if let Some(line) = stderr_line {
            body.push_str(&format!("echo {line} 1>&2\r\n"));
            body.push_str("for /L %%i in (1,1,3000) do echo noise line %%i ................................ 1>&2\r\n");
        }
        body.push_str("echo ARGS: %*\r\nexit /b 0\r\n");
        let path = dir.join("fake-r5.cmd");
        std::fs::write(&path, body).unwrap();
        path
    }
}

// ---- S1: a generator vouches only for the files it wrote ----

fn scanned_case(tag: &str) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let case_dir = root.join("case");
    let source = root.join("src");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
    crate::cli::handlers::init_case(&case_dir, &crate::cli::handlers::InitCaseOptions::default())
        .unwrap();
    crate::scan::scan_folder(
        &case_dir,
        &source,
        &crate::model::ScanOptions {
            hash_files: false,
            use_ffprobe: false,
            ..crate::model::ScanOptions::default()
        },
        crate::checkpoint::ResumeMode::Auto,
        None,
    )
    .unwrap();
    (root, case_dir)
}

/// S1: re-running a generator (make-report, make-custody) used to record
/// EVERY HTML file under review/ and reports/ — a page planted in a case
/// handed over by someone else became trusted (scripts allowed in the
/// workstation origin) the moment the examiner regenerated the report.
/// Now each generator records only the files it wrote.
#[test]
fn s1_regenerating_a_report_never_vouches_for_a_planted_page() {
    use crate::util::generated_pages::page_is_trusted;
    let (root, case_dir) = scanned_case("s1");
    std::fs::create_dir_all(case_dir.join("reports/extra")).unwrap();
    std::fs::create_dir_all(case_dir.join("review")).unwrap();
    for planted in ["reports/extra/planted.html", "review/planted.html"] {
        std::fs::write(
            case_dir.join(planted),
            "<html><script>fetch('/api/finalize',{method:'POST'})</script></html>",
        )
        .unwrap();
    }
    crate::cli::handlers::make_report(&case_dir, false, false, None).unwrap();
    crate::cli::handlers::make_custody(&case_dir).unwrap();
    assert!(page_is_trusted(&case_dir, "reports/case-report.html"));
    assert!(page_is_trusted(&case_dir, "reports/custody-statement.html"));
    for planted in ["reports/extra/planted.html", "review/planted.html"] {
        assert!(
            !page_is_trusted(&case_dir, planted),
            "{planted} was vouched for by a generator that never wrote it"
        );
    }
    // The workstation's own generator steps no longer walk the folders
    // either (the step child records what it wrote).
    let state = bound(&case_dir, "review-ready");
    let exe = fake_cli(&root, None);
    run_step(
        &exe,
        &["make-report".into(), case_dir.to_string_lossy().to_string()],
        &state,
    )
    .unwrap();
    assert!(!page_is_trusted(&case_dir, "review/planted.html"));
    let _ = std::fs::remove_dir_all(root);
}

// ---- S2: scripts loaded by trusted pages are pinned too ----

/// S2: `review/data-bundle.js` is loaded by the trusted viewer page but
/// was served whatever its bytes — an edited bundle ran in the workstation
/// origin under a page whose own hash was fine. Now a case script is
/// served only when the HMAC'd manifest lists its bytes, and evidence is
/// never served with a script MIME type.
#[test]
fn s2_case_scripts_must_be_listed_and_evidence_is_never_a_script() {
    let case = temp_case("s2");
    std::fs::create_dir_all(case.join("review")).unwrap();
    std::fs::write(
        case.join("review/evidence-viewer.html"),
        "<html>viewer</html>",
    )
    .unwrap();
    std::fs::write(
        case.join("review/data-bundle.js"),
        "window.__FRAMETRACE_DATA__={};",
    )
    .unwrap();
    crate::util::generated_pages::record_generated_pages(
        &case,
        &["review/evidence-viewer.html", "review/data-bundle.js"],
    )
    .unwrap();
    let state = bound(&case, "review-ready");
    let port = start_server(&state);
    let listed = get(port, "/review/data-bundle.js");
    assert!(head_of(&listed).starts_with("HTTP/1.1 200"), "{listed}");
    assert!(listed.ends_with("window.__FRAMETRACE_DATA__={};"));
    // Same through the page-relative /viewer/ spelling.
    assert!(get(port, "/viewer/data-bundle.js").starts_with("HTTP/1.1 200"));
    // Tampered after generation: refused, not served.
    std::fs::write(
        case.join("review/data-bundle.js"),
        "fetch('/api/finalize',{method:'POST'});",
    )
    .unwrap();
    let tampered = get(port, "/review/data-bundle.js");
    assert!(head_of(&tampered).starts_with("HTTP/1.1 403"), "{tampered}");
    assert!(!tampered.contains("api/finalize"));
    // A planted script anywhere under the case: refused.
    std::fs::create_dir_all(case.join("exports")).unwrap();
    std::fs::write(case.join("exports/x.js"), "alert(1)").unwrap();
    assert!(get(port, "/case/exports/x.js").starts_with("HTTP/1.1 403"));
    // Evidence `.js` through /media: bytes, never a script type.
    let media = get(
        port,
        &format!(
            "/media?path={}",
            super::percent_encode_header_value(&case.join("exports/x.js").to_string_lossy())
        ),
    );
    let media_head = head_of(&media);
    assert!(media_head.starts_with("HTTP/1.1 200"), "{media_head}");
    assert!(
        media_head.contains("Content-Type: application/octet-stream"),
        "{media_head}"
    );
    assert!(!media_head.contains("javascript"), "{media_head}");
    state_lock(&state).request_shutdown();
    let _ = std::fs::remove_dir_all(case);
}

// ---- S6: an unconfirmed raw image is never deleted without asking ----

fn e01_job(case: &Path, source: &Path) -> PipelineJob {
    PipelineJob {
        kind: InputKind::E01,
        case_dir: case.to_path_buf(),
        source_path: source.to_path_buf(),
        with_hash: false,
        with_ffprobe: false,
        with_deepfake: false,
        with_audio: false,
        skip_e01_verify: true,
        e01_direct: false,
    }
}

/// S6: a raw image whose completion marker no longer matches (a moved or
/// copied case: new mtime) was deleted and re-imported without asking —
/// hours of ewfexport thrown away for identical bytes. Now it is re-hashed
/// and reused when it matches the recorded SHA-256; a real mismatch stops
/// the run with "raw 불일치 — 재임포트 필요" and deletes nothing until the
/// examiner calls `/api/reimport-raw`, which audits the discard first.
#[test]
fn s6_raw_mismatch_asks_before_deleting_and_reimport_raw_decides() {
    let case = temp_case("s6");
    let exe = fake_cli(&case, None);
    let source = case.join("disk.E01");
    std::fs::write(&source, b"EVF").unwrap();
    let raw = case.join("evidence/images/evidence.raw");
    std::fs::write(&raw, b"imported-image-bytes").unwrap();
    let sha = crate::audit::digest_file(&raw).unwrap();
    // A marker from the original import, then the "move": new mtime.
    let marker = serde_json::json!({
        "schema_version": 1, "kind": "frametrace-e01-raw-complete",
        "size_bytes": 20, "modified_nanos": 1, "sha256": sha,
        "e01_path": source.to_string_lossy(), "imported_unix": 5,
    });
    std::fs::write(crate::e01::raw_done_marker_path(&raw), marker.to_string()).unwrap();
    let state = bound(&case, "running");
    state_lock(&state).busy = true;
    super::jobs::run_e01_pipeline_with(Arc::clone(&state), e01_job(&case, &source), &exe);
    let status = parse(&api_status(&state));
    assert_eq!(status["steps"][1], "done", "same bytes: reused, {status}");
    assert!(raw.is_file());
    assert!(
        !status["logs"].to_string().contains("ARGS: import-e01"),
        "no re-import of identical bytes: {status}"
    );
    // Now the bytes really differ: the run stops, nothing is deleted.
    std::fs::write(&raw, b"imported-image-bytez").unwrap();
    state_lock(&state).busy = true;
    super::jobs::run_e01_pipeline_with(Arc::clone(&state), e01_job(&case, &source), &exe);
    let status = parse(&api_status(&state));
    assert_eq!(status["steps"][1], "failed", "{status}");
    let error = status["error"].as_str().unwrap_or_default().to_string();
    assert!(error.contains("재임포트 필요"), "{error}");
    assert!(
        raw.is_file(),
        "the image must not be deleted without asking"
    );
    assert_eq!(
        status["raw_reimport"]["raw_path"],
        raw.to_string_lossy().as_ref()
    );
    assert_eq!(status["raw_reimport"]["endpoint"], "/api/reimport-raw");
    state_lock(&state).busy = false;
    // A stale button naming another image is refused.
    let wrong = parse(&super::jobs::api_reimport_raw_with(
        &request(
            "POST",
            "/api/reimport-raw",
            "",
            &serde_json::json!({"raw_path": case.join("other.raw").to_string_lossy()}).to_string(),
        ),
        &state,
        Some(exe.clone()),
    ));
    assert_eq!(wrong["ok"], false, "{wrong}");
    assert!(raw.is_file());
    // The examiner decides: audited discard, then the same run again.
    let decided = parse(&super::jobs::api_reimport_raw_with(
        &request("POST", "/api/reimport-raw", "", "{}"),
        &state,
        Some(exe.clone()),
    ));
    assert_eq!(decided["ok"], true, "{decided}");
    wait_until("re-run finished", Duration::from_secs(60), || {
        !state_lock(&state).busy
    });
    let status = parse(&api_status(&state));
    assert!(status["raw_reimport"].is_null(), "{status}");
    assert!(
        status["logs"].to_string().contains("ARGS: import-e01"),
        "re-imported: {status}"
    );
    let audit =
        std::fs::read_to_string(case.join("evidence/logs/e01-audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("\"discard-raw\""), "{audit}");
    // Nothing pending any more: a second click is refused.
    let again = parse(&super::jobs::api_reimport_raw_with(
        &request("POST", "/api/reimport-raw", "", "{}"),
        &state,
        Some(exe),
    ));
    assert_eq!(again["ok"], false, "{again}");
    // The route is wired.
    let mut routed = request("POST", "/api/reimport-raw", "", "{}");
    routed.host = Some("127.0.0.1:8477".into());
    let response = String::from_utf8_lossy(&route(&routed, &state)).to_string();
    assert!(response.contains("재임포트를 기다리는"), "{response}");
    let _ = std::fs::remove_dir_all(case);
}

// ---- S7: the viewer's own bursts are not refused ----

/// S7: every local client is 127.0.0.1, so the R4 per-peer cap (16) was a
/// global one — 16 slow or idle connections (or a burst of the viewer's
/// own parallel requests) turned the next request into a 503. The header
/// phase now has its own server-wide pool of 48 and authenticated
/// requests a separate pool of 64.
#[test]
fn s7_more_than_sixteen_pending_connections_do_not_refuse_a_request() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let port = start_server(&state);
    let limits = Arc::clone(&state_lock(&state).conn_limits);
    let idle: Vec<TcpStream> = (0..24)
        .map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap())
        .collect();
    wait_until("idle connections counted", Duration::from_secs(10), || {
        limits.preauth.load(Ordering::SeqCst) == 24
    });
    let served = get(port, "/api/status");
    assert!(served.starts_with("HTTP/1.1 200"), "{served}");
    drop(idle);
    // The pools are independent and capped.
    let pools = Arc::new(super::http::ConnLimits::default());
    let pre: Vec<_> = (0..super::http::MAX_PREAUTH)
        .map(|_| super::http::take_conn_slot(&pools, false).unwrap())
        .collect();
    assert!(super::http::take_conn_slot(&pools, false).is_none());
    let authed: Vec<_> = (0..super::http::MAX_AUTHED)
        .map(|_| super::http::take_conn_slot(&pools, true).unwrap())
        .collect();
    assert!(super::http::take_conn_slot(&pools, true).is_none());
    drop(pre);
    assert!(super::http::take_conn_slot(&pools, false).is_some());
    drop(authed);
    assert_eq!(pools.authed.load(Ordering::SeqCst), 0);
    state_lock(&state).request_shutdown();
}

// ---- S8: launch codes off the command line; none for the shell ----

#[test]
fn s8_launch_page_carries_the_code_and_the_shell_gets_none() {
    let browser = ServeOptions {
        case_dir: None,
        port: None,
        open_browser: true,
    };
    let shell = ServeOptions {
        case_dir: None,
        port: None,
        open_browser: false,
    };
    assert!(launch_code_wanted(&browser, true));
    assert!(
        !launch_code_wanted(&shell, true),
        "the shell never uses a code"
    );
    assert!(!launch_code_wanted(&browser, false), "no token, no code");
    let url = "http://127.0.0.1:8477/?launch=0123456789abcdef0123456789abcdef";
    let page = launcher_page_html(url);
    assert!(
        page.contains("url=http://127.0.0.1:8477/?launch=0123"),
        "{page}"
    );
    assert!(page.contains("location.replace(\"http://127.0.0.1:8477/?launch="));
    assert!(page.contains("no-referrer"));
    // The launch redirect keeps the other query parameters (U14 contract).
    assert_eq!(super::http::launch_redirect_target("launch=abc"), "/");
    assert_eq!(
        super::http::launch_redirect_target("launch=abc&case=x%20y&view=grid"),
        "/?case=x%20y&view=grid"
    );
    assert_eq!(
        super::http::launch_redirect_target("token=t&launch=abc&a=1"),
        "/?a=1"
    );
}

/// S8 (unix): the launch page is created 0600 inside a 0700 folder, never
/// reusing an existing name, and old pages are swept.
#[cfg(unix)]
#[test]
fn s8_launch_page_is_user_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("s8").join("launch");
    let url = "http://127.0.0.1:8477/?launch=0123456789abcdef0123456789abcdef";
    let path = write_launcher_file(&dir, url).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&dir), 0o700);
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("launch=0123")
    );
    let second = write_launcher_file(&dir, url).unwrap();
    assert_ne!(path, second);
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

// ---- S9: a child's key mismatch becomes a job warning ----

/// S9: a child that resolves another audit key than the workstation
/// printed a stderr line nobody saw (the windowed app has no console), and
/// the line was usually dropped from the retained tail of a chatty step.
/// Now the machine-readable line is collected wherever it appears and
/// becomes a job warning.
#[test]
fn s9_key_mismatch_line_becomes_a_job_warning() {
    let dir = scratch("s9");
    let line = format!("{}audit key differs (r5-test)", super::KEY_MISMATCH_MARKER);
    // R8-S7: the child binds the line to its step token.
    let child_line = format!(
        "{}audit key differs (r5-test)",
        crate::util::tokened_marker(super::KEY_MISMATCH_MARKER, &step_token_ref())
    );
    let exe = fake_cli(&dir, Some(&child_line));
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let output = run_step(&exe, &["verify-audit".into(), "x".into()], &state).unwrap();
    assert!(output.contains("ARGS:"), "{output}");
    let status = parse(&api_status(&state));
    let warnings = status["warnings"].to_string();
    assert!(warnings.contains("audit key differs (r5-test)"), "{status}");
    assert!(warnings.contains("verify-audit"), "{status}");
    // The collector itself: found even far outside the retained tail.
    let mut text = format!("{line}\n").into_bytes();
    text.extend(std::iter::repeat_n(b'x', 3 * super::STEP_OUTPUT_TAIL_BYTES));
    let (tail, found) =
        super::jobs::drain_capped_collecting(&mut text.as_slice(), &[super::KEY_MISMATCH_MARKER]);
    assert!(tail.len() <= super::STEP_OUTPUT_TAIL_BYTES);
    assert_eq!(found, ["audit key differs (r5-test)"]);
    let _ = std::fs::remove_dir_all(dir);
}

/// R7-S6: a step's warning (make-review embedding an unreadable
/// `case.json` as null) was a stderr `warning:` line nobody saw in the
/// windowed app. The workstation now tells its children to print warnings
/// as step-warning lines and shows them as job warnings.
#[test]
fn r7_s6_step_warning_lines_become_job_warnings() {
    let dir = scratch("r7-s6");
    // R8-S7: the line carries the step token the server put in the
    // environment (R7: the variable was `1`).
    let line = format!(
        "{}case.json is not valid JSON",
        crate::util::tokened_marker(crate::util::STEP_WARNING_MARKER, &step_token_ref())
    );
    let exe = fake_cli(&dir, Some(&line));
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    run_step(&exe, &["make-review".into(), "x".into()], &state).unwrap();
    let status = parse(&api_status(&state));
    let warnings = status["warnings"].to_string();
    assert!(
        warnings.contains("make-review: case.json is not valid JSON"),
        "{status}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// R8-S7: the step-token environment variable as the fake CLI's shell
/// expands it.
fn step_token_ref() -> String {
    if cfg!(windows) {
        format!("%{}%", crate::util::STEP_WARNING_ENV)
    } else {
        format!("${{{}}}", crate::util::STEP_WARNING_ENV)
    }
}

// ---- S10: a save queued while the job ends is applied ----

/// S10: the job released `busy` while the save still held the marks gate
/// (it had just queued the body): the job's drain skipped ("the gate
/// holder drains") and the gate holder never did — the save sat in the
/// queue until some later job ended. Now the save drains after releasing
/// the gate when nothing holds `busy`.
#[test]
fn s10_save_queued_as_the_job_ends_is_applied_at_once() {
    let case = temp_case("s10");
    let state = bound(&case, "review-partial");
    {
        let mut guard = state_lock(&state);
        guard.busy = true;
        guard.enrichment = Some(Enrichment {
            step: "hash",
            label: "t",
            stage: 1,
            stages: 3,
        });
    }
    let hook_state = Arc::clone(&state);
    super::jobs::AFTER_QUEUE_WRITE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            // The job ends right here: busy released, its own drain runs
            // (and skips — this request holds the gate).
            {
                let mut guard = state_lock(&hook_state);
                guard.busy = false;
                guard.enrichment = None;
            }
            super::jobs::drain_after_busy_release(&hook_state);
        }));
    });
    let marks = serde_json::json!({
        "schema_version": 1, "case_id": "FT-r5",
        "marks": [{"id": "vid_000001", "status": "important", "note": "s10"}],
    });
    let saved = parse(&api_import_marks(
        &request(
            "POST",
            "/api/import-marks",
            "",
            &serde_json::json!({"marks_json": marks.to_string()}).to_string(),
        ),
        &state,
    ));
    super::jobs::AFTER_QUEUE_WRITE.with(|hook| *hook.borrow_mut() = None);
    assert_eq!(saved["ok"], true, "{saved}");
    assert_eq!(
        pending_marks_count(&case),
        0,
        "the queued save must not be stranded"
    );
    let stored: Vec<String> = crate::case_db::load_review_marks(&case)
        .unwrap()
        .into_iter()
        .map(|mark| mark.record_id)
        .collect();
    assert_eq!(stored, ["vid_000001"]);
    let _ = std::fs::remove_dir_all(case);
}

// ---- S11: stable cursors for the conflict and queue pages ----

fn conflict_line(id: &str, revision: u64) -> String {
    serde_json::json!({"recorded_unix": 1, "id": id, "server_revision": revision,
        "server": {"status": "reviewed"}, "mine": {"mark": null}})
    .to_string()
}

/// S11: offset paging skipped entries when earlier ones were resolved
/// between two page requests (everything shifted left by the resolved
/// count). The `after=<seq>` cursor names the last entry seen; resolving
/// or adding entries meanwhile never skips one.
#[test]
fn s11_conflict_pages_by_cursor_never_skip_after_resolutions() {
    let case = temp_case("s11");
    let path = case.join(super::jobs::PENDING_MARK_CONFLICTS_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lines: Vec<String> = (0..10)
        .map(|i| conflict_line(&format!("vid_{i:06}"), 1))
        .collect();
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let state = bound(&case, "review-ready");
    let page = |query: &str| {
        parse(&super::jobs::api_mark_conflicts_page(
            &request("GET", "/api/mark-conflicts", query, ""),
            &state,
        ))
    };
    let first = page("limit=4");
    let ids = |page: &serde_json::Value| -> Vec<String> {
        page["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        ids(&first),
        ["vid_000000", "vid_000001", "vid_000002", "vid_000003"]
    );
    let cursor = first["next_after"].as_u64().expect("cursor");
    // Between the requests the first three are resolved.
    let mut text = std::fs::read_to_string(&path).unwrap();
    for i in 0..3 {
        text.push_str(
            &(serde_json::json!({"resolved": format!("vid_{i:06}"), "revision": 2, "unix": 1})
                .to_string()
                + "\n"),
        );
    }
    std::fs::write(&path, &text).unwrap();
    let offset_page = page("offset=4&limit=4");
    assert_eq!(
        ids(&offset_page)[0],
        "vid_000007",
        "offset paging skips vid_000004..6 — the bug the cursor fixes"
    );
    let second = page(&format!("after={cursor}&limit=4"));
    assert_eq!(
        ids(&second),
        ["vid_000004", "vid_000005", "vid_000006", "vid_000007"]
    );
    let third = page(&format!(
        "after={}&limit=4",
        second["next_after"].as_u64().unwrap()
    ));
    assert_eq!(ids(&third), ["vid_000008", "vid_000009"]);
    assert!(third["next_after"].is_null(), "{third}");
    // Compaction keeps the numbers (and never re-issues one).
    let before: Vec<u64> = page("limit=100")["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["seq"].as_u64().unwrap())
        .collect();
    super::jobs::compact_conflict_journal(&case).unwrap();
    let after: Vec<u64> = page("limit=100")["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(before, after);
    let compacted = std::fs::read_to_string(&path).unwrap();
    assert!(compacted.contains("seq_high"), "{compacted}");
    let _ = std::fs::remove_dir_all(case);
}

#[test]
fn s11_marks_pending_pages_by_queue_sequence() {
    let case = temp_case("s11-q");
    let dir = case.join("marks-pending");
    std::fs::create_dir_all(&dir).unwrap();
    for n in 1..=5u32 {
        let body = serde_json::json!({
            "schema_version": 1, "case_id": "FT-r5",
            "marks": [{"id": format!("vid_{n:06}"), "status": "important"}],
        });
        std::fs::write(dir.join(format!("{n:032}.json")), body.to_string()).unwrap();
    }
    let state = bound(&case, "review-ready");
    let page = |query: &str| {
        parse(&super::jobs::api_marks_pending(
            &request("GET", "/api/marks-pending", query, ""),
            &state,
        ))
    };
    let first = page("limit=2");
    assert_eq!(first["entries"][0]["seq"], 1);
    // R6-S2: string cursors; the bare file number is still accepted.
    assert_eq!(first["next_after"], "2:0");
    // Files 1–3 drain between the requests: nothing after the cursor is
    // skipped (offset=2 would now start at vid_000005).
    for n in 1..=3u32 {
        std::fs::remove_file(dir.join(format!("{n:032}.json"))).unwrap();
    }
    let second = page("after=2&limit=2");
    let ids: Vec<&str> = second["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["vid_000004", "vid_000005"]);
    assert!(second["next_after"].is_null(), "{second}");
    let _ = std::fs::remove_dir_all(case);
}

// ---- S14: only the killed step's own package reservation is removed ----

/// S14: after killing a package-case step the server removed EVERY empty
/// `package_*` folder — including the reservation of a package-case
/// running in another process (a CLI run), whose finished package then
/// lost its name. Only the killed pid's own reservation goes now.
#[test]
fn s14_killed_package_step_removes_only_its_own_reservation() {
    let case = temp_case("s14");
    let reports = case.join("reports");
    std::fs::create_dir_all(reports.join("package_100")).unwrap();
    std::fs::create_dir_all(reports.join(".package_100.ft4242.part")).unwrap();
    // Another process's live reservation and work folder.
    std::fs::create_dir_all(reports.join("package_200")).unwrap();
    std::fs::create_dir_all(reports.join(".package_200.ft777.part")).unwrap();
    let args = vec![
        "package-case".to_string(),
        case.to_string_lossy().to_string(),
    ];
    let lines = super::media::sweep_killed_step(&args, 4242);
    assert!(lines.iter().all(Result::is_ok), "{lines:?}");
    assert!(!reports.join("package_100").exists());
    assert!(!reports.join(".package_100.ft4242.part").exists());
    assert!(
        reports.join("package_200").is_dir(),
        "another process's reservation was removed"
    );
    assert!(reports.join(".package_200.ft777.part").is_dir());
    let _ = std::fs::remove_dir_all(case);
}

/// R7-S5: a work folder only names the reservation its writer TRIED.
/// Process 4242 lost the race for `package_300` to process 777 (whose
/// marker is inside) and was killed before removing its work folder: R6
/// removed 777's live reservation with it. The marker now decides — in
/// the kill sweep and in the stale sweep — and a reservation whose dead
/// owner left no work folder is released by its marker.
#[test]
fn r7_s5_reservation_cleanup_needs_the_owner_marker() {
    let case = temp_case("r7-s5");
    let reports = case.join("reports");
    let reserve = |name: &str, pid: u32| {
        let dir = reports.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(crate::join::RESERVATION_MARKER),
            crate::join::reservation_marker_text(pid),
        )
        .unwrap();
        dir
    };
    let winner = reserve("package_300", 777);
    std::fs::create_dir_all(reports.join(".package_300.ft777.part")).unwrap();
    std::fs::create_dir_all(reports.join(".package_300.ft4242.part")).unwrap();
    let own = reserve("package_400", 4242);
    std::fs::create_dir_all(reports.join(".package_400.ft4242.part")).unwrap();
    let args = vec![
        "package-case".to_string(),
        case.to_string_lossy().to_string(),
    ];
    let lines = super::media::sweep_killed_step(&args, 4242);
    assert!(lines.iter().all(Result::is_ok), "{lines:?}");
    assert!(!own.exists(), "the killed pid's own reservation stays");
    assert!(
        winner.join(crate::join::RESERVATION_MARKER).is_file(),
        "another process's reservation was removed"
    );
    // Stale sweep: 4242's work folder for package_300 (dead writer) goes,
    // 777's reservation stays while 777 lives.
    std::fs::create_dir_all(reports.join(".package_300.ft4242.part")).unwrap();
    let alive_777 = |pid: u32| {
        if pid == 777 {
            crate::util::ProcessProbe::Unknown
        } else {
            crate::util::ProcessProbe::Gone
        }
    };
    crate::join::sweep_stale_parts_with(&reports, &alive_777, 0);
    assert!(!reports.join(".package_300.ft4242.part").exists());
    assert!(winner.join(crate::join::RESERVATION_MARKER).is_file());
    // An orphan reservation (owner 555 dead, no work folder) is released.
    let orphan = reserve("package_500", 555);
    let swept = crate::join::sweep_stale_parts_with(&reports, &alive_777, 0);
    assert!(!orphan.exists(), "{swept:?}");
    assert!(winner.is_dir());
    let _ = std::fs::remove_dir_all(case);
}

/// S14: the "aborted" export manifest is written atomically (no torn
/// file on a forced exit) and is valid JSON.
#[test]
fn s14_abandoned_export_manifest_is_whole() {
    // R13-S14: serialized with the C4 test; assertions are about this
    // test's own folder only (another test's export may be registered).
    let _serial = super::media::ABANDONED_EXPORTS_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let case = temp_case("s14-export");
    let export = case.join("exports/selection-1");
    std::fs::create_dir_all(&export).unwrap();
    let registration = super::media::begin_export_bundle(&export, 1, 3).unwrap();
    assert!(super::media::mark_abandoned_exports() >= 1);
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(export.join("manifest-audit.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["state"], "aborted");
    let leftovers: Vec<String> = std::fs::read_dir(&export)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".tmp") || name.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    drop(registration);
    let _ = std::fs::remove_dir_all(case);
}

// ---- S15: records-meta names each record's proxy ----

/// S15 (with P9): the viewer guessed a record's proxy from file-name
/// prefixes, so a legacy carve id reused by a later carve run picked the
/// proxy made for the earlier run's (different) file. The server now
/// names each record's proxy in `records-meta.proxy_paths`, matching the
/// D5 name's source hash; legacy `<id>_proxy_*` names still count.
#[test]
fn s15_records_meta_names_each_records_own_proxy() {
    let case = temp_case("s15");
    let carved = case.join("artifacts/carved");
    std::fs::create_dir_all(&carved).unwrap();
    let new_output = carved.join("carve_000001_run2.mp4");
    std::fs::write(&new_output, b"run 2 bytes").unwrap();
    // R9 P3: a real chained lane (rows come through audit::read_lane_rows).
    crate::audit::test_write_lane(
        &carved.join("carve-log.jsonl"),
        &(serde_json::json!({"id": "carve_000001", "output_path": new_output.to_string_lossy(),
            "size_bytes": 11})
        .to_string()
            + "\n"
            + &serde_json::json!({"output_path": "artifacts/carved/legacy.mp4", "size_bytes": 3})
                .to_string()
            + "\n"),
    );
    std::fs::write(carved.join("legacy.mp4"), b"leg").unwrap();
    let proxies = case.join("artifacts/proxies");
    std::fs::create_dir_all(&proxies).unwrap();
    // The earlier run's proxy (another file, same carve id) sorts first.
    let stale = proxies.join("proxy_carve_000001_00000000.mp4");
    std::fs::write(&stale, b"p").unwrap();
    let canonical = crate::util::canonicalize_display(&new_output).unwrap();
    let own = proxies.join(format!(
        "proxy_carve_000001_{}.mp4",
        crate::artifacts::source_hash8(&canonical)
    ));
    std::fs::write(&own, b"p").unwrap();
    let legacy_proxy = proxies.join(format!(
        "{}_proxy_1700000000.mp4",
        crate::util::sanitize_filename("artifacts/carved/legacy.mp4")
    ));
    std::fs::write(&legacy_proxy, b"p").unwrap();
    // An indexed record without a proxy gets no entry.
    std::fs::write(
        case.join("db/video_index.json"),
        serde_json::json!({"videos": [{"id": "vid_000001", "source_path": "/nowhere/a.mp4"}]})
            .to_string(),
    )
    .unwrap();
    let state = bound(&case, "review-ready");
    let meta = parse(&super::records::api_records_meta(&state));
    let paths = &meta["proxy_paths"];
    let name = |value: &serde_json::Value| {
        Path::new(value.as_str().unwrap_or(""))
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        name(&paths["carve_000001"]),
        own.file_name().unwrap().to_string_lossy(),
        "{paths}"
    );
    assert_eq!(
        name(&paths["artifacts/carved/legacy.mp4"]),
        legacy_proxy.file_name().unwrap().to_string_lossy(),
        "{paths}"
    );
    assert!(paths.get("vid_000001").is_none(), "{paths}");
    let _ = std::fs::remove_dir_all(case);
}
