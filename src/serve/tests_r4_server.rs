//! Round-4 track B (docs/PLAN_R4_2026-10-14.md §2): server, security and
//! marks-queue fixes B1–B22. Every test here fails on the code before the
//! fix (most of them by exercising the exact misbehaviour the review found).

use super::*;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r4b-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A minimal case folder the in-process marks import accepts.
fn temp_case(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-r4"}"#).unwrap();
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

fn marks_request(id: &str, note: &str) -> Request {
    let marks = serde_json::json!({
        "schema_version": 1,
        "case_id": "FT-r4",
        "marks": [{"id": id, "status": "important", "note": note}],
    });
    request(
        "POST",
        "/api/import-marks",
        "",
        &serde_json::json!({"marks_json": marks.to_string()}).to_string(),
    )
}

fn stored_ids(case_dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = crate::case_db::load_review_marks(case_dir)
        .unwrap()
        .into_iter()
        .map(|mark| mark.record_id)
        .collect();
    ids.sort();
    ids
}

/// Polls `condition` (no fixed sleep) until it holds or `limit` passes.
fn wait_until(what: &str, limit: Duration, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + limit;
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// A fake frametrace CLI: prints its argv and the expected-key variable,
/// then exits with the code configured for its subcommand (0 otherwise).
/// The subcommand is found after an optional leading `--key-source X`.
fn fake_cli(dir: &Path, exits: &[(&str, i32)]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut body = String::from(
            "#!/bin/sh\necho \"ARGS: $*\"\necho \"KEYTAG: $FRAMETRACE_EXPECTED_AUDIT_KEY\"\nsub=\"$1\"\nif [ \"$1\" = \"--key-source\" ]; then sub=\"$3\"; fi\n",
        );
        for (sub, code) in exits {
            body.push_str(&format!(
                "if [ \"$sub\" = \"{sub}\" ]; then echo partial-listing >&2; exit {code}; fi\n"
            ));
        }
        body.push_str("exit 0\n");
        let path = dir.join("fake-r4.sh");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    #[cfg(windows)]
    {
        let mut body = String::from(
            "@echo off\r\necho ARGS: %*\r\necho KEYTAG: %FRAMETRACE_EXPECTED_AUDIT_KEY%\r\nset sub=%~1\r\nif \"%~1\"==\"--key-source\" set sub=%~3\r\n",
        );
        for (sub, code) in exits {
            body.push_str(&format!(
                "if \"%sub%\"==\"{sub}\" (\r\n  echo partial-listing 1>&2\r\n  exit /b {code}\r\n)\r\n"
            ));
        }
        body.push_str("exit /b 0\r\n");
        let path = dir.join("fake-r4.cmd");
        std::fs::write(&path, body).unwrap();
        path
    }
}

/// Raw HTTP exchange with a test server; returns the whole response.
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

// ---- B1: only an instance-file-verified server is reused ----

/// A fake listener that answers every connection with a complete
/// workstation-shaped `/api/status` body carrying `instance`.
fn impostor(instance_field: &str) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = format!(
        "{{\"ok\":true,\"app\":\"frametrace\",\"instance\":\"{instance_field}\",\"has_job\":false,\"case_dir\":null}}"
    );
    thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    port
}

#[test]
fn b1_a_lookalike_server_is_never_reused_without_instance_proof() {
    let port = impostor("someone-else");
    // No instance record: no structural fallback any more — not reused.
    assert_eq!(super::discovery::verified_running_server(None), None);
    // A record naming that port but another nonce: not reused either.
    assert_eq!(
        super::discovery::verified_running_server(Some((port, "our-nonce".into(), None))),
        None
    );
    // The real thing (fingerprint of the recorded nonce) is reused.
    let genuine = impostor(&instance_fingerprint("our-nonce"));
    assert_eq!(
        super::discovery::verified_running_server(Some((genuine, "our-nonce".into(), None))),
        Some(genuine)
    );
    // Skipped preferred ports are announced.
    let warning = occupied_ports_warning(&[8477, 8478]).unwrap();
    assert!(warning.contains("8477, 8478"), "{warning}");
    assert_eq!(occupied_ports_warning(&[]), None);
}

// ---- B2: one-time launch codes ----

fn token_state(token: &str) -> SharedState {
    let mut state = JobState::new();
    state.token = Some(token.to_string());
    Arc::new(Mutex::new(state))
}

#[test]
fn b2_launch_code_plants_the_cookie_once_and_expires() {
    let token = "tok-r4-launch-0001";
    let state = token_state(token);
    let port = start_server(&state);
    let code = mint_launch_code(&state).expect("code minted while a token is set");
    let url = launch_url(port, Some(&code));
    assert!(!url.contains(token), "{url}");
    assert_eq!(url, format!("http://127.0.0.1:{port}/?launch={code}"));
    let get = |target: &str, cookie: Option<&str>| {
        exchange(
            port,
            &format!(
                "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}Connection: close\r\n\r\n",
                cookie
                    .map(|c| format!("Cookie: {c}\r\n"))
                    .unwrap_or_default()
            ),
        )
    };
    let first = get(&format!("/?launch={code}"), None);
    let head = head_of(&first);
    // R6-S3: a same-origin step page (200) instead of a 303 redirect.
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        first.contains("location.replace(\"/\" + location.hash)"),
        "{first}"
    );
    assert!(
        head.contains(&format!(
            "Set-Cookie: ft_token_{port}={token}; HttpOnly; SameSite=Strict"
        )),
        "{head}"
    );
    // Single use.
    let reused = get(&format!("/?launch={code}"), None);
    assert!(head_of(&reused).starts_with("HTTP/1.1 401"), "{reused}");
    assert!(!head_of(&reused).contains("Set-Cookie"), "{reused}");
    // Expired (30 s TTL): an outstanding but stale code is refused.
    let stale = "0123456789abcdef0123456789abcdef".to_string();
    state_lock(&state).launch_codes.push((
        stale.clone(),
        std::time::Instant::now() - Duration::from_secs(1),
    ));
    let expired = get(&format!("/?launch={stale}"), None);
    assert!(head_of(&expired).starts_with("HTTP/1.1 401"), "{expired}");
    // A browser that already holds the cookie is just sent to `/`.
    let with_cookie = get(
        &format!("/?launch={stale}"),
        Some(&format!("ft_token_{port}={token}")),
    );
    assert!(
        head_of(&with_cookie).starts_with("HTTP/1.1 200"),
        "{with_cookie}"
    );
    assert!(
        !head_of(&with_cookie).contains("Set-Cookie"),
        "{with_cookie}"
    );
    // ?token= keeps working for scripts and harnesses.
    let scripted = get(&format!("/api/status?token={token}"), None);
    assert!(head_of(&scripted).starts_with("HTTP/1.1 200"), "{scripted}");
    assert_eq!(LAUNCH_CODE_TTL, Duration::from_secs(30));
    state_lock(&state).request_shutdown();
}

#[test]
fn b2_relaunch_gets_a_code_over_the_token_header_and_urls_never_carry_it() {
    let token = "tok-r4-launch-0002";
    let state = token_state(token);
    let port = start_server(&state);
    let code = super::discovery::request_launch_code(port, token).expect("code over header");
    assert_eq!(code.len(), 32);
    assert!(
        state_lock(&state)
            .launch_codes
            .iter()
            .any(|(c, _)| *c == code)
    );
    assert_eq!(
        super::discovery::request_launch_code(port, "wrong-token-000"),
        None
    );
    // The shell's tokenized URL is exchanged before it reaches a browser.
    let safe = browser_safe_url(
        &format!("http://127.0.0.1:{port}/?token={token}"),
        &|p, t| {
            assert_eq!((p, t), (port, token));
            Some("c0ffee".into())
        },
    );
    assert_eq!(safe, format!("http://127.0.0.1:{port}/?launch=c0ffee"));
    let fallback = browser_safe_url(
        &format!("http://127.0.0.1:{port}/?token={token}"),
        &|_, _| None,
    );
    assert_eq!(fallback, format!("http://127.0.0.1:{port}/"));
    // No token configured: nothing to mint, the bare page is used.
    assert_eq!(
        mint_launch_code(&Arc::new(Mutex::new(JobState::new()))),
        None
    );
    state_lock(&state).request_shutdown();
}

// ---- B3: indexed journal, cached count, compaction, pagination ----

fn write_journal(case: &Path, lines: &[String]) {
    let path = case.join(super::jobs::PENDING_MARK_CONFLICTS_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn conflict_line(id: &str, revision: u64) -> String {
    serde_json::json!({"recorded_unix": 1, "id": id, "server_revision": revision,
        "server": {"status": "reviewed"}, "mine": {"mark": null}})
    .to_string()
}

#[test]
fn b3_status_count_of_a_50k_journal_is_cached_and_fast() {
    let case = temp_case("b3-big");
    let lines: Vec<String> = (0..50_000)
        .map(|i| conflict_line(&format!("vid_{i:06}"), 1))
        .collect();
    write_journal(&case, &lines);
    let parses = || super::jobs::CONFLICT_JOURNAL_PARSES.with(std::cell::Cell::get);
    let before = parses();
    let first = super::jobs::pending_mark_conflicts(&case);
    assert_eq!(first["count"], 50_000);
    assert_eq!(parses(), before + 1);
    let state = bound(&case, "review-ready");
    let started = std::time::Instant::now();
    let status = parse(&api_status(&state));
    let elapsed = started.elapsed();
    assert_eq!(status["mark_conflicts"]["count"], 50_000);
    assert_eq!(
        status["mark_conflicts"]["ids"].as_array().unwrap().len(),
        50
    );
    assert_eq!(
        parses(),
        before + 1,
        "the unchanged journal is not re-parsed"
    );
    assert!(
        elapsed < Duration::from_millis(50),
        "status with a 50k-entry journal took {elapsed:?}"
    );
    let _ = std::fs::remove_dir_all(case);
}

#[test]
fn b3_mark_conflicts_is_paginated() {
    let case = temp_case("b3-page");
    let lines: Vec<String> = (0..1200)
        .map(|i| conflict_line(&format!("vid_{i:04}"), 2))
        .collect();
    write_journal(&case, &lines);
    let state = bound(&case, "review-ready");
    let page = |query: &str| {
        parse(&super::jobs::api_mark_conflicts_page(
            &request("GET", "/api/mark-conflicts", query, ""),
            &state,
        ))
    };
    let first = page("");
    assert_eq!(first["count"], 1200);
    assert_eq!(first["limit"], 500);
    assert_eq!(first["conflicts"].as_array().unwrap().len(), 500);
    assert_eq!(first["conflicts"][0]["id"], "vid_0000");
    assert_eq!(first["next_offset"], 500);
    let last = page("offset=1000&limit=500");
    assert_eq!(last["conflicts"].as_array().unwrap().len(), 200);
    assert_eq!(last["conflicts"][0]["id"], "vid_1000");
    assert!(last["next_offset"].is_null(), "{}", last["next_offset"]);
    // The route is wired with the query.
    let mut routed = request("GET", "/api/mark-conflicts", "offset=1199&limit=5", "");
    routed.host = Some("127.0.0.1:8477".into());
    let response = String::from_utf8_lossy(&route(&routed, &state)).to_string();
    let body = parse(&response[response.find("\r\n\r\n").unwrap() + 4..]);
    assert_eq!(body["conflicts"].as_array().unwrap().len(), 1, "{body}");
    let _ = std::fs::remove_dir_all(case);
}

#[test]
fn b3_compaction_archives_history_and_keeps_the_open_set() {
    let case = temp_case("b3-compact");
    let mut lines = Vec::new();
    for i in 0..600 {
        let id = format!("vid_{i:04}");
        lines.push(conflict_line(&id, 1));
        if i % 3 != 0 {
            lines.push(serde_json::json!({"resolved": id, "revision": 2, "unix": 1}).to_string());
        }
    }
    // An id resolved and then conflicting again stays open.
    lines.push(conflict_line("vid_0001", 5));
    lines.push("not json".to_string());
    write_journal(&case, &lines);
    let before = super::jobs::pending_mark_conflicts(&case);
    let (archived, kept) = super::jobs::compact_conflict_journal(&case).unwrap();
    let after = super::jobs::pending_mark_conflicts(&case);
    assert_eq!(before, after, "compaction must not change the open set");
    assert_eq!(after["count"], 201);
    assert_eq!(kept, 201);
    assert_eq!(archived + kept, lines.len());
    let archive =
        std::fs::read_to_string(case.join(super::jobs::MARK_CONFLICTS_ARCHIVE_FILE)).unwrap();
    assert_eq!(archive.lines().count(), archived);
    assert!(archive.contains("not json"), "nothing is dropped");
    let _ = std::fs::remove_dir_all(case);
}

// ---- B4: saves never strand in a queue nothing drains ----

#[test]
fn b4_index_only_case_save_is_not_queued_behind_a_short_busy_job() {
    let case = temp_case("b4-index-only");
    // open-case of an index-only case → review-partial, no enrichment.
    let state = bound(&case, "review-partial");
    let held = try_acquire_busy(&state).unwrap(); // e.g. verify-sources
    let refused = parse(&api_import_marks(&marks_request("vid_1", "n"), &state));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["retry"], true, "{refused}");
    assert_eq!(
        pending_marks_count(&case),
        0,
        "must not be queued: nothing would ever drain it"
    );
    drop(held);
    let saved = parse(&api_import_marks(&marks_request("vid_1", "n"), &state));
    assert_eq!(saved["ok"], true, "{saved}");
    assert_eq!(saved["deferred"], false, "{saved}");
    assert_eq!(stored_ids(&case), ["vid_1"]);
    let _ = std::fs::remove_dir_all(case);
}

#[test]
fn b4_queued_marks_are_applied_when_busy_is_released() {
    let case = temp_case("b4-drain");
    let queued = serde_json::json!({
        "schema_version": 1, "case_id": "FT-r4",
        "marks": [{"id": "vid_9", "status": "important", "note": "stranded"}],
    });
    let dir = case.join("marks-pending");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{:032}.json", 7)), queued.to_string()).unwrap();
    let state = bound(&case, "review-partial");
    assert_eq!(pending_marks_count(&case), 1);
    let busy = try_acquire_busy(&state).unwrap();
    drop(busy);
    assert_eq!(pending_marks_count(&case), 0, "drained on release");
    assert_eq!(stored_ids(&case), ["vid_9"]);
    let _ = std::fs::remove_dir_all(case);
}

#[test]
fn b4_queue_is_only_used_while_a_draining_job_runs() {
    let mut guard = JobState::new();
    guard.busy = true;
    guard.phase = "review-partial";
    assert!(!super::jobs::marks_must_queue(&guard), "index-only + busy");
    guard.enrichment = Some(Enrichment {
        step: "hash",
        label: "t",
        stage: 1,
        stages: 3,
    });
    assert!(super::jobs::marks_must_queue(&guard));
    guard.enrichment = None;
    guard.case_job = Some(CaseJob {
        kind: "carve",
        started_unix: 0,
        finished_unix: None,
        stage: 1,
        stages: 2,
        stage_label: "t",
        state: "running",
        error: None,
    });
    assert!(super::jobs::marks_must_queue(&guard));
}

// ---- B5: generated pages are trusted by verified content, not location ----

#[test]
fn b5_tampered_or_unlisted_generated_page_is_sandboxed() {
    let case = temp_case("b5-pages");
    let page = case.join("review/index.html");
    std::fs::create_dir_all(page.parent().unwrap()).unwrap();
    std::fs::write(&page, "<html><script>window.ok=1</script></html>").unwrap();
    std::fs::create_dir_all(case.join("reports")).unwrap();
    std::fs::write(case.join("reports/case-report.html"), "<html>report</html>").unwrap();
    let state = bound(&case, "review-ready");
    let port = start_server(&state);
    let get = |target: &str| {
        let response = exchange(
            port,
            &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        );
        head_of(&response).to_string()
    };
    // Unlisted (no manifest): rendered, but sandboxed — no script.
    let unlisted = get("/review/index.html");
    assert!(unlisted.contains("200 OK"), "{unlisted}");
    assert!(
        unlisted.contains("Content-Security-Policy: sandbox"),
        "{unlisted}"
    );
    assert!(!unlisted.contains("Content-Disposition"), "{unlisted}");
    // Recorded by the generator: trusted, with a script-src hash list.
    let recorded = crate::util::generated_pages::record_generated_pages(
        &case,
        &["review/index.html", "reports/case-report.html"],
    )
    .unwrap();
    assert_eq!(recorded, 2);
    let trusted = get("/review/index.html");
    assert!(!trusted.contains("sandbox"), "{trusted}");
    assert!(trusted.contains("frame-ancestors 'self'"), "{trusted}");
    assert!(trusted.contains("script-src 'self' 'sha256-"), "{trusted}");
    assert!(!get("/case/reports/case-report.html").contains("sandbox"));
    // Tampered after generation: sandboxed again.
    std::fs::write(
        &page,
        "<html><script>fetch('/api/finalize')</script></html>",
    )
    .unwrap();
    assert!(get("/review/index.html").contains("Content-Security-Policy: sandbox"));
    // A forged manifest (hash added without this user's key) is ignored.
    let manifest = case.join(crate::util::generated_pages::MANIFEST_REL);
    let mut forged: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    forged["hashes"].as_array_mut().unwrap().push(
        crate::util::generated_pages::file_sha256(&page)
            .unwrap()
            .into(),
    );
    std::fs::write(&manifest, forged.to_string()).unwrap();
    assert!(get("/review/index.html").contains("Content-Security-Policy: sandbox"));
    assert!(
        get("/case/reports/case-report.html").contains("sandbox"),
        "a manifest that fails its MAC trusts nothing"
    );
    state_lock(&state).request_shutdown();
    let _ = std::fs::remove_dir_all(case);
}

#[test]
fn b5_script_src_hashes_inline_scripts_and_skips_inline_handlers() {
    use crate::util::generated_pages::inline_script_sources;
    let sources = inline_script_sources(
        "<html><script src=\"data-bundle.js\"></script><script>alert(1)</script></html>",
    )
    .unwrap();
    // sha256("alert(1)") in base64.
    assert_eq!(
        sources,
        "'self' 'sha256-bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI='"
    );
    assert_eq!(
        inline_script_sources("<button onclick=\"go()\">x</button><script>a</script>"),
        None
    );
    assert_eq!(
        inline_script_sources("<a href=\"javascript:x\">x</a>"),
        None
    );
    assert_eq!(crate::util::base64_encode(b"ab"), "YWI=");
}

// ---- B6: /api/marks-pending ----

#[test]
fn b6_marks_pending_folds_the_queue_to_the_latest_value_per_id() {
    let case = temp_case("b6-pending");
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
    for (id, note) in [("vid_1", "first"), ("vid_2", "x"), ("vid_1", "second")] {
        let queued = parse(&api_import_marks(&marks_request(id, note), &state));
        assert_eq!(queued["deferred"], true, "{queued}");
    }
    let deleted = serde_json::json!({"schema_version": 2, "protocol": "patch-v1", "case_id": "FT-r4", "marks": [],
        "deleted_ids": ["vid_2"]});
    let queued = parse(&api_import_marks(
        &request(
            "POST",
            "/api/import-marks",
            "",
            &serde_json::json!({"marks_json": deleted.to_string()}).to_string(),
        ),
        &state,
    ));
    assert_eq!(queued["deferred"], true, "{queued}");
    let mut routed = request("GET", "/api/marks-pending", "", "");
    routed.host = Some("127.0.0.1:8477".into());
    let response = String::from_utf8_lossy(&route(&routed, &state)).to_string();
    let body = parse(&response[response.find("\r\n\r\n").unwrap() + 4..]);
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["files"], 4);
    assert_eq!(body["count"], 2);
    assert_eq!(body["entries"][0]["id"], "vid_1");
    assert_eq!(body["entries"][0]["mark"]["note"], "second");
    assert_eq!(body["entries"][0]["deleted"], false);
    assert_eq!(body["entries"][1]["id"], "vid_2");
    assert_eq!(body["entries"][1]["deleted"], true);
    assert!(body["entries"][1]["mark"].is_null());
    assert!(body["next_offset"].is_null());
    let paged = super::jobs::marks_pending_json(&case, 1, 1);
    assert_eq!(paged["entries"][0]["id"], "vid_2");
    let _ = std::fs::remove_dir_all(case);
}

// ---- B7: a failed report refresh does not turn a stored save into a failure ----

#[test]
fn b7_import_reports_success_and_the_report_error_separately() {
    let case = temp_case("b7-report");
    let state = bound(&case, "review-ready");
    let reply = parse(&super::jobs::api_import_marks_with_runner(
        &marks_request("vid_7", "kept"),
        &state,
        &|args| {
            assert_eq!(args[0], "make-report");
            Err("make-report 실패 (exit Some(1)): disk full".into())
        },
    ));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["applied"]["vid_7"], 1, "{reply}");
    assert!(
        reply["report_error"]
            .as_str()
            .unwrap()
            .contains("disk full")
    );
    assert!(reply["report_url"].is_null(), "{reply}");
    assert_eq!(stored_ids(&case), ["vid_7"]);
    let status = parse(&api_status(&state));
    assert!(
        status["warnings"]
            .to_string()
            .contains("보고서를 갱신하지 못했습니다"),
        "{status}"
    );
    let _ = std::fs::remove_dir_all(case);
}

// ---- B8: open-case forgets the previous package ----

#[test]
fn b8_open_case_clears_the_previous_package_in_every_branch() {
    let case = temp_case("b8-open");
    std::fs::create_dir_all(case.join("review")).unwrap();
    std::fs::write(case.join("review/index.html"), "<html></html>").unwrap();
    let state = bound(&case, "done");
    state_lock(&state).package_dir = Some(PathBuf::from("/old/case/reports/package_1"));
    let body = serde_json::json!({"case_dir": case.to_string_lossy()}).to_string();
    let opened = parse(&api_open_case(
        &request("POST", "/api/open-case", "", &body),
        &state,
    ));
    assert_eq!(opened["ok"], true, "{opened}");
    assert_eq!(opened["has_review"], true);
    assert_eq!(state_lock(&state).package_dir, None);
    let _ = std::fs::remove_dir_all(case);
}

// ---- B9: drive-relative device names ----

#[test]
fn b9_drive_relative_device_names_are_refused() {
    for raw in ["C:NUL", "c:nul.txt", "D:COM1", r"E:aux.mp4", r"C:\x\D:CON"] {
        assert!(crate::util::reserved_device_refused(raw), "{raw}");
        assert_eq!(
            super::http::reject_remote_path_on(raw, &[], true),
            Err(super::http::RESERVED_DEVICE_ERROR),
            "{raw}"
        );
    }
    for raw in ["C:notes.mp4", r"C:\cases\null.mp4", "C:", r"C:\COMPANY"] {
        assert!(!crate::util::reserved_device_refused(raw), "{raw}");
    }
}

// ---- B10: monotonic, collision-free queue names ----

#[test]
fn b10_queue_names_are_monotonic_even_after_a_clock_step_back() {
    let case = temp_case("b10-queue");
    let dir = case.join("marks-pending");
    std::fs::create_dir_all(&dir).unwrap();
    // A file named by a wall clock far "ahead" (clock later stepped back).
    let ahead = dir.join(format!("{:032}.json", 9_000_000_000_000_000_000u128));
    std::fs::write(&ahead, "{}").unwrap();
    let first = super::jobs::claim_queue_file(&dir).unwrap();
    let second = super::jobs::claim_queue_file(&dir).unwrap();
    assert!(first > ahead, "{first:?} must sort after {ahead:?}");
    assert!(second > first, "{second:?} after {first:?}");
    // Claims are empty until written; the queue ignores empty files.
    assert_eq!(pending_marks_count(&case), 1);
    std::fs::write(&first, "{\"marks\":[]}").unwrap();
    assert_eq!(pending_marks_count(&case), 2);
    let _ = std::fs::remove_dir_all(case);
}

// ---- B11: finalize / import re-check the case under busy ----

#[test]
fn b11_finalize_and_import_refuse_when_the_case_switched_before_busy() {
    let case = temp_case("b11-a");
    let other = temp_case("b11-b");
    let state = bound(&case, "review-ready");
    let switch = || state_lock(&state).case_dir = Some(other.clone());
    let finalize = parse(&super::jobs::finalize_with_hooks(
        &request("POST", "/api/finalize", "", r#"{"examiner":"Kim"}"#),
        &state,
        &switch,
    ));
    assert_eq!(finalize["ok"], false, "{finalize}");
    assert_eq!(finalize["error"], super::jobs::CASE_SWITCHED_ERROR);
    state_lock(&state).case_dir = Some(case.clone());
    let import = parse(&super::jobs::import_marks_with_hooks(
        &marks_request("vid_1", "n"),
        &state,
        &|_| Ok(String::new()),
        &switch,
    ));
    assert_eq!(import["ok"], false, "{import}");
    assert_eq!(import["error"], super::jobs::CASE_SWITCHED_ERROR);
    assert!(
        stored_ids(&other).is_empty(),
        "nothing landed in the other case"
    );
    let _ = std::fs::remove_dir_all(case);
    let _ = std::fs::remove_dir_all(other);
}

// ---- B12: slowloris bounds ----

#[test]
fn b12_unauthenticated_connections_are_capped() {
    // R5-S7: the cap is server-wide (`MAX_PREAUTH`), not per peer.
    assert!(super::http::HEADER_READ_TIMEOUT <= Duration::from_secs(10));
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let port = start_server(&state);
    let limits = Arc::clone(&state_lock(&state).conn_limits);
    let preauth = || limits.preauth.load(Ordering::SeqCst);
    let idle: Vec<TcpStream> = (0..super::http::MAX_PREAUTH)
        .map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap())
        .collect();
    wait_until(
        "idle connections registered",
        Duration::from_secs(10),
        || preauth() == super::http::MAX_PREAUTH,
    );
    // One more is refused before a byte of it is read.
    let mut extra = TcpStream::connect(("127.0.0.1", port)).unwrap();
    extra
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut refused = Vec::new();
    let _ = extra.read_to_end(&mut refused);
    let refused = String::from_utf8_lossy(&refused);
    assert!(refused.starts_with("HTTP/1.1 503"), "{refused}");
    drop(idle);
    wait_until("slots released", Duration::from_secs(10), || preauth() == 0);
    let served = exchange(
        port,
        &format!("GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert!(served.starts_with("HTTP/1.1 200"), "{served}");
    state_lock(&state).request_shutdown();
}

// ---- B13: anti-framing headers on every HTML response ----

#[test]
fn b13_html_responses_refuse_foreign_framing() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let mut root = request("GET", "/", "", "");
    root.host = Some("127.0.0.1:8477".into());
    let examiner = String::from_utf8_lossy(&route(&root, &state)).to_string();
    let head = head_of(&examiner);
    assert!(head.contains("frame-ancestors 'self'"), "{head}");
    assert!(head.contains("X-Frame-Options: SAMEORIGIN"), "{head}");
    let viewer = String::from_utf8_lossy(&viewer_page()).to_string();
    assert!(head_of(&viewer).contains("X-Frame-Options: SAMEORIGIN"));
    let json_head = String::from_utf8_lossy(&json("{}".into())).to_string();
    assert!(!head_of(&json_head).contains("X-Frame-Options"));
    assert_eq!(
        super::http::html_frame_headers("Content-Security-Policy: frame-ancestors 'self'\r\n"),
        "X-Frame-Options: SAMEORIGIN\r\n"
    );
}

// ---- B14: opener failures are reported ----

#[test]
fn b14_opener_failure_is_an_error_not_a_silent_success() {
    assert_eq!(system_opener("macos"), "open");
    assert_eq!(system_opener("linux"), "xdg-open");
    let missing = run_opener(
        "frametrace-no-such-opener-r4",
        std::ffi::OsStr::new("http://127.0.0.1/"),
    );
    assert!(missing.is_err(), "{missing:?}");
    #[cfg(unix)]
    {
        let failed = run_opener("false", std::ffi::OsStr::new("/tmp"));
        assert!(
            failed.unwrap_err().contains("exit"),
            "non-zero exit reported"
        );
        assert_eq!(run_opener("true", std::ffi::OsStr::new("/tmp")), Ok(()));
    }
}

// ---- B15: the key source reaches every CLI child ----

#[test]
fn b15_children_get_the_workstation_key_source_and_expected_key() {
    let args = vec!["make-review".to_string(), "/case".to_string()];
    assert_eq!(
        super::jobs::child_command_args(Some("env:FT_KEY"), &args),
        ["--key-source", "env:FT_KEY", "make-review", "/case"]
    );
    assert_eq!(super::jobs::child_command_args(None, &args), args);
    let named = vec!["--key-source=file:/k".to_string(), "x".to_string()];
    assert_eq!(
        super::jobs::child_command_args(Some("env:A"), &named),
        named
    );
    let dir = scratch("b15-cli");
    let exe = fake_cli(&dir, &[]);
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    {
        let mut guard = state_lock(&state);
        guard.key_source = Some("env:FT_R4_KEY".into());
        guard.audit_key_tag = Some("default:0123456789ab".into());
    }
    let output = run_step(&exe, &["verify-audit".into(), "x".into()], &state).unwrap();
    assert!(
        output.contains("ARGS: --key-source env:FT_R4_KEY verify-audit x"),
        "{output}"
    );
    assert!(output.contains("KEYTAG: default:0123456789ab"), "{output}");
    // Child side: the announced key is compared with the child's own.
    assert_eq!(key_tag_mismatch("default:aa", "default:aa"), None);
    assert!(
        key_tag_mismatch("default:aa", "none")
            .unwrap()
            .contains("다릅니다")
    );
    let tag = audit_key_tag(Ok(Some(crate::audit_key::AuditKey::new("k1", &[7u8; 32]))));
    assert!(tag.starts_with("k1:") && tag.len() == 3 + 12, "{tag}");
    assert_eq!(audit_key_tag(Ok(None)), "none");
    assert_eq!(audit_key_tag(Err("bad".into())), "error");
    let _ = std::fs::remove_dir_all(dir);
}

// ---- B16: exit code 5 = completed with a partial result ----

#[test]
fn b16_partial_exit_is_kept_and_warned_not_failed() {
    let dir = scratch("b16-step");
    let exe = fake_cli(&dir, &[("inspect-image", 5)]);
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let output = run_step(&exe, &["inspect-image".into(), "c".into()], &state).unwrap();
    assert!(output.contains("ARGS:"), "{output}");
    let status = parse(&api_status(&state));
    let warnings = status["warnings"].to_string();
    assert!(warnings.contains("부분 결과"), "{status}");
    assert!(warnings.contains("partial-listing"), "{status}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn b16_partial_inspection_marks_the_step_partial_and_the_run_continues() {
    let case = temp_case("b16-e01");
    let exe = fake_cli(&case, &[("inspect-image", 5)]);
    let source = case.join("image.E01");
    std::fs::write(&source, b"EVF").unwrap();
    let state = bound(&case, "running");
    {
        let mut guard = state_lock(&state);
        guard.busy = true;
        guard.step_names = InputKind::E01.step_names().to_vec();
    }
    let job = PipelineJob {
        kind: InputKind::E01,
        case_dir: case.clone(),
        source_path: source,
        with_hash: false,
        with_ffprobe: false,
        with_deepfake: false,
        with_audio: false,
        skip_e01_verify: true,
        e01_direct: false,
    };
    super::jobs::run_e01_pipeline_with(Arc::clone(&state), job, &exe);
    let status = parse(&api_status(&state));
    assert_eq!(status["phase"], "review-ready", "{status}");
    assert_eq!(status["steps"][2], "partial", "{status}");
    assert_eq!(status["steps"][4], "done", "{status}");
    assert!(
        status["warnings"].to_string().contains("부분 결과"),
        "{status}"
    );
    let _ = std::fs::remove_dir_all(case);
}

// ---- B17: case jobs check every side writer and the case under busy ----

#[test]
fn b17_case_job_refuses_while_a_write_op_runs_or_the_case_switched() {
    let case = temp_case("b17-job");
    let exe = fake_cli(&case, &[]);
    let state = bound(&case, "review-ready");
    state_lock(&state).write_ops = 1;
    let refused = parse(&super::jobs::audio_measure_with_exe(
        &request("POST", "/api/audio-measure", "", "{}"),
        &state,
        &exe,
    ));
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("프레임 캡처"),
        "{refused}"
    );
    assert!(!state_lock(&state).busy);
    state_lock(&state).write_ops = 0;
    let switched = parse(&super::jobs::start_case_job(
        &state,
        &exe,
        super::jobs::CaseJobSpec {
            kind: "carve",
            stage_label: "t",
            case_dir: case.join("elsewhere"),
            main_args: vec!["carve-file".into()],
            start_log: String::new(),
            done_log: "",
        },
    ));
    assert_eq!(
        switched["error"],
        super::jobs::CASE_SWITCHED_ERROR,
        "{switched}"
    );
    assert!(!state_lock(&state).busy, "the lease was released");
    assert!(state_lock(&state).case_job.is_none());
    let _ = std::fs::remove_dir_all(case);
}

// ---- B18: finalize chains the sign-off into the audit trail ----

#[test]
fn b18_finalize_records_the_signoff_audit_and_fails_closed() {
    let case = temp_case("b18-final");
    let exe = fake_cli(&case, &[]);
    let state = bound(&case, "review-ready");
    super::jobs::SIGNOFF_AUDIT_CALLS.with(|calls| calls.borrow_mut().clear());
    let reply = parse(&{
        let _busy = try_acquire_busy(&state).unwrap();
        finalize_with_exe(&exe, &case, "Kim", &state)
    });
    assert_eq!(reply["ok"], true, "{reply}");
    let calls = super::jobs::SIGNOFF_AUDIT_CALLS.with(|calls| calls.borrow().clone());
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, case);
    assert!(
        calls[0].1.contains("\"examiner\":\"Kim\""),
        "{}",
        calls[0].1
    );
    assert!(case.join(crate::report::SIGNOFF_REL_PATH).is_file());
    // A failed audit append fails the finalize; no sign-off is promoted.
    std::fs::remove_file(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap();
    super::jobs::SIGNOFF_AUDIT_FAIL.with(|fail| *fail.borrow_mut() = Some("disk full".into()));
    let failed = parse(&{
        let _busy = try_acquire_busy(&state).unwrap();
        finalize_with_exe(&exe, &case, "Kim", &state)
    });
    super::jobs::SIGNOFF_AUDIT_FAIL.with(|fail| *fail.borrow_mut() = None);
    assert_eq!(failed["ok"], false, "{failed}");
    assert!(
        failed["error"].as_str().unwrap().contains("감사 로그"),
        "{failed}"
    );
    assert!(!case.join(crate::report::SIGNOFF_REL_PATH).exists());
    assert!(!case.join(super::jobs::SIGNOFF_PENDING_REL_PATH).exists());
    let _ = std::fs::remove_dir_all(case);
}

// ---- B19: /api/advanced path fields are gated for every route caller ----

#[test]
fn b19_advanced_extra_unc_path_is_refused_before_the_handler() {
    let case = temp_case("b19-adv");
    let state = bound(&case, "review-ready");
    for tool in ["compare", "merge", "known-hash"] {
        let body = serde_json::json!({"tool": tool, "extra": r"\\evil\share\case"}).to_string();
        let mut advanced = request("POST", "/api/advanced", "", &body);
        advanced.host = Some("127.0.0.1:8477".into());
        let response = String::from_utf8_lossy(&route(&advanced, &state)).to_string();
        assert!(response.starts_with("HTTP/1.1 400"), "{tool}: {response}");
        assert!(
            response.contains("UNC and device paths are refused"),
            "{response}"
        );
    }
    assert!(!state_lock(&state).busy);
    let _ = std::fs::remove_dir_all(case);
}

// ---- B21: the real process error mode ----

#[cfg(windows)]
#[test]
fn b21_error_mode_is_read_back_from_the_os() {
    let wanted = crate::util::SEM_FAILCRITICALERRORS
        | crate::util::SEM_NOGPFAULTERRORBOX
        | crate::util::SEM_NOOPENFILEERRORBOX;
    crate::util::suppress_os_error_dialogs();
    let actual = crate::util::current_error_mode();
    assert_eq!(actual & wanted, wanted, "GetErrorMode() = {actual:#x}");
}

// ---- B22: shutdown decisions can see every running job ----

#[test]
fn b22_status_lists_running_work_for_the_shutdown_prompt() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    {
        let mut guard = state_lock(&state);
        guard.busy = true;
        guard.phase = "running";
        guard.write_ops = 1;
    }
    let status = parse(&api_status(&state));
    assert_eq!(status["busy"], true);
    assert_eq!(status["job_kind"], "pipeline");
    assert_eq!(status["writes_running"], 1);
    assert_eq!(status["clips_running"], 0);
    assert!(status["clip_jobs"].is_array());
    assert_eq!(parse(&api_shutdown(&state))["running"], true);
}
