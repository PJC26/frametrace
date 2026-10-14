use super::*;

#[test]
fn decodes_percent_and_plus_in_paths() {
    // Path components must keep `+` as-is (legal in filenames);
    assert_eq!(
        percent_decode_component("C%3A%5CUsers%5C%ED%95%9C%EA%B8%80%5Ca+b.mp4", false),
        "C:\\Users\\한글\\a+b.mp4"
    );
    assert_eq!(percent_decode_component("plain", false), "plain");
    assert_eq!(percent_decode_component("bad%2", false), "bad%2");
    assert_eq!(percent_decode_component("tail%", false), "tail%");
    // Query values do translate `+` to space.
    assert_eq!(percent_decode_component("a+b%20c", true), "a b c");
    // A bare flag pair no longer aborts later query parsing.
    assert_eq!(
        query_value("flag&path=%2Fev", "path").as_deref(),
        Some("/ev")
    );
}

#[test]
fn parses_media_range_headers() {
    assert_eq!(parse_range("bytes=0-1023", 2048), Some((0, Some(1023))));
    assert_eq!(parse_range("bytes=100-", 2048), Some((100, None)));
    // Suffix ranges: last N bytes of a 2048-byte entity.
    assert_eq!(parse_range("bytes=-500", 2048), Some((1548, None)));
    // A suffix longer than the entity clamps to the whole entity.
    assert_eq!(parse_range("bytes=-99999", 2048), Some((0, None)));
    // A zero suffix is unsatisfiable (start >= total → caller maps 416).
    assert_eq!(parse_range("bytes=-0", 2048), Some((u64::MAX, None)));
    assert_eq!(parse_range("items=1-2", 2048), None);
    assert_eq!(parse_range("bytes=abc-", 2048), None);
    assert_eq!(parse_range("bytes=-abc", 2048), None);
    // Zero-byte entity: a suffix range resolves to start 0, and the
    // caller's `start < total` check maps it to 416 rather than
    // trying to stream a byte that does not exist.
    assert_eq!(parse_range("bytes=-10", 0), Some((0, None)));
}

#[test]
fn progress_query_does_not_create_an_uninitialized_case() {
    let case_dir =
        std::env::temp_dir().join(format!("ft-progress-uninitialized-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&case_dir);
    assert!(progress_json(Some(&case_dir), None).is_null());
    assert!(!case_dir.exists());
}

#[test]
fn progress_json_reports_running_job_with_eta() {
    let case_dir = std::env::temp_dir().join(format!("ft-progress-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&case_dir);
    std::fs::create_dir_all(&case_dir).unwrap();

    std::fs::write(case_dir.join("case.json"), b"{}").unwrap();
    assert!(progress_json(Some(&case_dir), None).is_null());
    assert!(!crate::case_db::case_db_path(&case_dir).exists());

    let job = crate::case_db::start_job(
        &case_dir,
        "scan-folder",
        Path::new("/evidence"),
        Some(100),
        "{}",
    )
    .unwrap();
    crate::case_db::report_job_progress(&case_dir, &job.job_id, Some(100), 50).unwrap();

    let json = progress_json(Some(&case_dir), None).to_string();
    assert!(json.contains("\"job_type\":\"scan-folder\""), "{json}");
    assert!(json.contains("\"done\":50"), "{json}");
    assert!(json.contains("\"total\":100"), "{json}");
    assert!(json.contains("\"elapsed_secs\":"), "{json}");

    crate::case_db::complete_job(&case_dir, &job.job_id, 100, "done").unwrap();
    assert!(progress_json(Some(&case_dir), None).is_null());

    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn byte_progress_reports_growing_output_file() {
    let case_dir =
        std::env::temp_dir().join(format!("ft-byteprogress-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&case_dir);
    std::fs::create_dir_all(&case_dir).unwrap();
    let raw = case_dir.join("evidence.raw");
    std::fs::write(&raw, vec![0u8; 4096]).unwrap();

    let json = byte_progress_json(Some(&(raw.clone(), None))).to_string();
    assert!(json.contains("\"done\":4096"), "{json}");
    assert!(json.contains("\"total\":0"), "{json}");

    // A known larger total yields a real numerator/denominator pair.
    let json = byte_progress_json(Some(&(raw.clone(), Some(8192)))).to_string();
    assert!(json.contains("\"total\":8192"), "{json}");

    // Missing file or absent hint degrades to null, never an error.
    std::fs::remove_file(&raw).unwrap();
    assert!(byte_progress_json(Some(&(raw, None))).is_null());
    assert!(byte_progress_json(None).is_null());

    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn extracts_json_body_values() {
    let body = r#"{"input_kind":"e01","source_path":"C:\\a\\b.mp4","with_hash":true,"rows":3,"empty":false}"#;
    assert_eq!(body_value(body, "input_kind").as_deref(), Some("e01"));
    assert_eq!(
        body_value(body, "source_path").as_deref(),
        Some("C:\\a\\b.mp4")
    );
    assert_eq!(body_value(body, "with_hash").as_deref(), Some("true"));
    assert_eq!(body_value(body, "empty").as_deref(), Some("false"));
    assert_eq!(body_value(body, "rows").as_deref(), Some("3"));
    assert_eq!(body_value(body, "missing"), None);
    // Korean and other non-ASCII string escapes must decode.
    assert_eq!(
        body_value(r#"{"notes":"\ud655\uc778"}"#, "notes").as_deref(),
        Some("확인")
    );
    // Surrogate pairs decode to the intended non-BMP scalar.
    assert_eq!(
        body_value(r#"{"notes":"\ud83d\ude00"}"#, "notes").as_deref(),
        Some("😀")
    );
}

#[test]
fn body_parser_rejects_malformed_and_value_embedded_keys() {
    // Malformed JSON (here: a truncated \u escape) rejects the whole
    // body instead of yielding a partial value.
    assert_eq!(body_value(r#"{"notes":"a\u12"}"#, "notes"), None);
    assert_eq!(body_value("not json at all", "path"), None);
    assert_eq!(body_value(r#"{"path":"unterminated"#, "path"), None);
    // The old substring scan treated a `"key":` literal inside a
    // string *value* as a real field — the exact confusion serde_json
    // removes.
    let body = r#"{"note":"see {\"path\":\"C:\\evil\"} quoted","path":"C:\\real"}"#;
    assert_eq!(body_value(body, "path").as_deref(), Some("C:\\real"));
    let body = r#"{"note":"contains \"source_path\":\"C:\\evil\" text"}"#;
    assert_eq!(body_value(body, "source_path"), None);
    // Keys nested inside other objects are not top-level fields.
    let body = r#"{"outer":{"path":"C:\\nested"},"path":null}"#;
    assert_eq!(body_value(body, "path"), None);
    // A real top-level marks_json string holding embedded JSON still
    // round-trips as a plain string.
    let body = r#"{"marks_json":"{\"marks\":[{\"t\":1}]}"}"#;
    assert_eq!(
        body_value(body, "marks_json").as_deref(),
        Some(r#"{"marks":[{"t":1}]}"#)
    );
}

/// Every API response is hand-assembled via `format!` — the safety
/// invariant is that all dynamic values pass `json_string`. This test
/// feeds hostile characters (quotes, backslashes, newlines, unicode)
/// through reachable response paths and asserts each still parses as
/// valid JSON via serde_json.
#[test]
fn api_responses_stay_valid_json_with_hostile_strings() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let post = |path: &str, body: &str| Request {
        method: "POST".into(),
        path: path.into(),
        query: String::new(),
        body: body.into(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    };
    let parse = |label: &str, json: &str| -> serde_json::Value {
        serde_json::from_str(json)
            .unwrap_or_else(|err| panic!("{label} produced invalid JSON: {err}\n{json}"))
    };

    // Idle status + error paths with no case loaded.
    parse("api_status", &api_status(&state));
    parse("api_carve", &api_carve(&post("/api/carve", "{}"), &state));
    parse("api_recover_deleted", &api_recover_deleted(&state));
    parse(
        "api_export_selected",
        &api_export_selected(&post("/api/export-selected", "{}"), &state),
    );
    parse(
        "api_advanced(unknown)",
        &api_advanced(&post("/api/advanced", r#"{"tool":"nope"}"#), &state),
    );
    parse(
        "api_advanced(hostile extra)",
        &api_advanced(
            &post(
                "/api/advanced",
                r#"{"tool":"known-hash","extra":"C:\\evil \"quoted\" \\path"}"#,
            ),
            &state,
        ),
    );
    parse(
        "api_export_clip",
        &api_export_clip(
            &post("/api/export-clip", r#"{"id":"v\"1","start":0,"end":1}"#),
            &state,
        ),
    );
    parse(
        "api_proxy",
        &api_proxy(&post("/api/proxy", r#"{"id":"v\"1"}"#), &state),
    );

    // Logs/errors containing quotes and newlines still serialize.
    {
        let mut guard = state_lock(&state);
        guard.logs.push("line \"one\"\nline \\ two".into());
        guard.error = Some("error: \"bad\" \\ path".into());
    }
    let value = parse("api_status(hostile)", &api_status(&state));
    assert_eq!(value["error"].as_str().unwrap(), "error: \"bad\" \\ path");
    parse("api_cancel", &api_cancel(&state));
}

#[test]
fn shutdown_refuses_while_busy_then_accepts() {
    let parse = |json: &str| serde_json::from_str::<serde_json::Value>(json).unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).busy = true;
    let refused = parse(&api_shutdown(&state));
    assert_eq!(refused["ok"].as_bool(), Some(false));
    assert_eq!(refused["running"].as_bool(), Some(true));
    assert!(!state_lock(&state).shutdown_requested());
    state_lock(&state).busy = false;
    let granted = parse(&api_shutdown(&state));
    assert_eq!(granted["ok"].as_bool(), Some(true));
    assert!(state_lock(&state).shutdown_requested());
}

#[test]
fn reads_query_values() {
    assert_eq!(
        query_value("path=C%3A%5Cx.mp4&range=0-1", "path").as_deref(),
        Some("C:\\x.mp4")
    );
    assert_eq!(query_value("a=1", "b"), None);
}

#[test]
fn containment_rejects_prefix_siblings() {
    let base = std::env::temp_dir().join(format!("ft_under_root_{}", std::process::id()));
    let sibling = std::env::temp_dir().join(format!("ft_under_root_{}_x", std::process::id()));
    let child = base.join("sub");
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&sibling);
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let root_canon = base.canonicalize().unwrap();
    let child_canon = child.canonicalize().unwrap();
    let sibling_canon = sibling.canonicalize().unwrap();
    assert!(path_is_under(&root_canon, &child_canon));
    assert!(path_is_under(&root_canon, &root_canon));
    // On case-insensitive filesystems (Windows, default APFS) an
    // uppercased root still canonicalizes to the same directory; on
    // case-sensitive filesystems the uppercased path does not exist
    // and canonicalize fails — either way containment holds.
    let upper = PathBuf::from(root_canon.to_string_lossy().to_uppercase());
    if upper.canonicalize().is_ok() {
        assert!(path_is_under(&upper, &child_canon));
    }
    assert!(!path_is_under(&root_canon, &sibling_canon));
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&sibling);
}

#[test]
fn containment_rejects_case_variant_siblings_on_case_sensitive_fs() {
    let pid = std::process::id();
    let lower = std::env::temp_dir().join(format!("ft_casevar_{pid}"));
    let upper = std::env::temp_dir().join(format!("FT_CASEVAR_{pid}"));
    let _ = std::fs::remove_dir_all(&lower);
    let _ = std::fs::remove_dir_all(&upper);
    std::fs::create_dir_all(&lower).unwrap();
    std::fs::create_dir_all(&upper).unwrap();
    let lower_canon = lower.canonicalize().unwrap();
    let upper_canon = upper.canonicalize().unwrap();
    // On a case-sensitive filesystem the two paths are DIFFERENT
    // directories, so the case-variant sibling must fail containment
    // (the old ASCII case-folded prefix compare would have passed it).
    if lower_canon != upper_canon {
        assert!(!path_is_under(&lower_canon, &upper_canon));
    }
    let _ = std::fs::remove_dir_all(&lower);
    let _ = std::fs::remove_dir_all(&upper);
}

#[test]
fn case_binding_api_rejects_marks_before_writing() {
    let base = std::env::temp_dir().join(format!("ft_marks_binding_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("case.json"), r#"{"case_id":"FT-current"}"#).unwrap();
    let marks_path = base.join("marks-imported.json");
    std::fs::write(&marks_path, b"preserve existing import").unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(base.clone());
    for (field, value, expected) in [
        ("case_id", serde_json::json!("FT-other"), "case_id mismatch"),
        ("case_id", serde_json::json!(123), "case_id"),
        ("case_id", serde_json::json!(""), "case_id"),
        ("schema_version", serde_json::json!(99), "schema_version"),
    ] {
        let mut marks = serde_json::json!({"schema_version": 1, "case_id": "FT-current", "marks": [{"id": "vid_1", "status": "important"}]});
        marks[field] = value;
        let request = Request {
            method: "POST".into(),
            path: "/api/import-marks".into(),
            query: String::new(),
            body: serde_json::json!({"marks_json": marks.to_string()}).to_string(),
            range: None,
            origin: None,
            host: None,
            cookie: None,
            token_header: None,
            nonce_header: None,
        };
        let response: serde_json::Value =
            serde_json::from_str(&api_import_marks(&request, &state)).unwrap();
        assert_eq!(response["ok"], false);
        assert!(
            response["error"].as_str().unwrap().contains(expected),
            "{response}"
        );
        assert_eq!(
            std::fs::read(&marks_path).unwrap(),
            b"preserve existing import"
        );
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 2);
        assert!(state_lock(&state).logs.is_empty());
    }
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn selection_file_only_takes_indexed_vid_ids() {
    let base = std::env::temp_dir().join(format!("ft_sel_case_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("db")).unwrap();
    let index = r#"{"schema_version":3,"case_id":"FT-1","videos":[
        {"id":"vid_000001","ffprobe":{"streams":[{"id":"0x1","codec_type":"video"}]}},
        {"id":"vid_000002","ffprobe":{"streams":[{"id":"0x2","codec_type":"video"}]}}
    ]}"#;
    std::fs::write(base.join("db/video_index.json"), index).unwrap();
    std::fs::write(base.join("case.json"), r#"{"case_id":"FT-actual"}"#).unwrap();
    let out = base.join("selection-all.json");
    let count = build_selection_file(&base, &out).unwrap();
    assert_eq!(count, 2);
    let content = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&content).unwrap()["case_id"],
        "FT-actual"
    );
    assert!(content.contains("\"selector\":\"vid_000001\""));
    assert!(content.contains("\"selector\":\"vid_000002\""));
    assert!(!content.contains("0x1"));
    assert!(content.contains("\"action\":\"validate\""));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn selection_file_ignores_id_literals_inside_string_values() {
    let base = std::env::temp_dir().join(format!("ft_sel_embed_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("db")).unwrap();
    // A `"id":"vid_..."` literal inside a string value (ffprobe error
    // text quoting JSON) must not produce a phantom selector — the old
    // substring scan would have collected vid_evil.
    let index = r#"{"schema_version":3,"videos":[
        {"id":"vid_000001","ffprobe_error":"bad blob {\"id\":\"vid_evil\"} here"},
        {"id":"vid_000002"}
    ]}"#;
    std::fs::write(base.join("db/video_index.json"), index).unwrap();
    std::fs::write(base.join("case.json"), r#"{"case_id":"FT-actual"}"#).unwrap();
    let out = base.join("selection-all.json");
    let count = build_selection_file(&base, &out).unwrap();
    assert_eq!(count, 2);
    let content = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&content).unwrap()["case_id"],
        "FT-actual"
    );
    assert!(content.contains("\"selector\":\"vid_000001\""));
    assert!(content.contains("\"selector\":\"vid_000002\""));
    assert!(!content.contains("vid_evil"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn selection_file_errors_without_videos() {
    let base = std::env::temp_dir().join(format!("ft_sel_empty_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("db")).unwrap();
    std::fs::write(base.join("db/video_index.json"), r#"{"videos":[]}"#).unwrap();
    let out = base.join("selection-all.json");
    assert!(build_selection_file(&base, &out).is_err());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn browse_lists_dirs_and_first_segment_images() {
    let base = std::env::temp_dir().join(format!("ft_browse_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("sub")).unwrap();
    std::fs::write(base.join("img.E01"), b"e").unwrap();
    std::fs::write(base.join("img.E02"), b"e").unwrap();
    std::fs::write(base.join("note.txt"), b"n").unwrap();

    let listing = browse_json(&base.display().to_string(), true);
    assert!(listing.contains("\"name\":\"sub\""));
    assert!(listing.contains("\"name\":\"img.E01\""));
    // Continuation segments and unrelated files stay hidden — the
    // examiner picks the first segment and libewf opens the rest.
    assert!(!listing.contains("img.E02"));
    assert!(!listing.contains("note.txt"));

    let dirs_only = browse_json(&base.display().to_string(), false);
    assert!(dirs_only.contains("\"name\":\"sub\""));
    assert!(!dirs_only.contains("img.E01"));

    let file = browse_json(&base.join("img.E01").display().to_string(), true);
    assert!(file.contains("\"is_file\":true"));

    let missing = browse_json(&base.join("nope").display().to_string(), false);
    assert!(missing.contains("\"exists\":false"));

    let roots = browse_json("", false);
    assert!(roots.contains("\"ok\":true"));
    assert!(roots.contains("\"entries\":["));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn export_selected_copies_allowed_files_and_writes_manifest() {
    let base = std::env::temp_dir().join(format!("ft_export_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let case_dir = base.join("case");
    let source_dir = base.join("source");
    std::fs::create_dir_all(case_dir.join("evidence/logs")).unwrap();
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(source_dir.join("clip.mp4"), b"video-bytes").unwrap();
    std::fs::write(base.join("outside.bin"), b"out").unwrap();

    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    {
        let mut guard = state.lock().unwrap();
        guard.case_dir = Some(case_dir.clone());
        guard.media_roots = vec![case_dir.clone(), source_dir.clone()];
    }
    let body = format!(
        "{{\"items\":[\
            {{\"id\":\"vid_1\",\"name\":\"clip.mp4\",\"path\":{},\"kind\":\"video\",\"status\":\"ffprobe-video-stream-confirmed\",\"mark\":\"important\",\"tags\":[\"사고\"],\"warnings\":[]}},\
            {{\"id\":\"fls:7\",\"name\":\"\",\"path\":\"\",\"kind\":\"candidate\",\"status\":\"candidate-unvalidated\"}},\
            {{\"id\":\"evil\",\"name\":\"outside.bin\",\"path\":{},\"kind\":\"video\"}}\
        ]}}",
        json_string(&source_dir.join("clip.mp4").display().to_string()),
        json_string(&base.join("outside.bin").display().to_string())
    );
    let request = Request {
        method: "POST".into(),
        path: "/api/export-selected".into(),
        query: String::new(),
        body,
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    };
    let out = api_export_selected(&request, &state);
    assert!(out.contains("\"ok\":true"), "{out}");
    assert!(out.contains("\"copied\":1"), "{out}");
    assert!(out.contains("\"skipped\":2"), "{out}");

    let export_dir = std::fs::read_dir(case_dir.join("exports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(export_dir.join("vid_1__clip.mp4").is_file());
    let manifest = std::fs::read_to_string(export_dir.join("manifest.csv")).unwrap();
    assert!(manifest.contains("vid_1"));
    assert!(manifest.contains("no-file(pre-recovery candidate)"));
    // A path outside the approved roots is refused, not copied.
    assert!(manifest.contains("outside-approved-roots"));
    assert!(!export_dir.join("evil__outside.bin").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn export_safe_names_keep_unicode_and_strip_forbidden_chars() {
    assert_eq!(export_safe_name("블랙박스 영상.mp4"), "블랙박스 영상.mp4");
    assert_eq!(
        export_safe_name("a/b\\c:d*e?f\"g<h>i|j"),
        "a_b_c_d_e_f_g_h_i_j"
    );
    assert_eq!(export_safe_name("name."), "name");
    assert_eq!(export_safe_name("..."), "item");
    assert_eq!(export_safe_name(""), "item");
}

#[test]
fn origin_gate_requires_exact_loopback_authority() {
    let request = |origin: Option<&str>, host: Option<&str>| Request {
        method: "POST".into(),
        path: "/api/start".into(),
        query: String::new(),
        body: String::new(),
        range: None,
        origin: origin.map(str::to_string),
        host: host.map(str::to_string),
        cookie: None,
        token_header: None,
        nonce_header: None,
    };
    // Prefix-spoofed authorities that the old starts_with gate let
    // through: a loopback literal as a subdomain prefix, as userinfo,
    // or under a non-http scheme must all be rejected.
    for origin in [
        "http://127.0.0.1.evil.com",
        "http://127.0.0.1@evil.example",
        "http://localhost.attacker.io",
        "https://127.0.0.1",
        "null",
    ] {
        assert!(
            !request_is_localhost_trusted(&request(Some(origin), Some("127.0.0.1"))),
            "origin {origin} must be rejected"
        );
    }
    // Exact loopback hosts with an optional port stay accepted when the
    // Origin authority equals the Host authority (item 7: port included).
    for (origin, host) in [
        ("http://127.0.0.1", "127.0.0.1"),
        ("http://127.0.0.1:8477", "127.0.0.1:8477"),
        ("http://localhost", "localhost"),
        ("http://localhost:3000", "localhost:3000"),
        ("http://[::1]", "[::1]"),
        ("http://[::1]:9000", "[::1]:9000"),
    ] {
        assert!(
            request_is_localhost_trusted(&request(Some(origin), Some(host))),
            "origin {origin} must be trusted for host {host}"
        );
    }
    // ...and a loopback Origin of another port/host is a different origin.
    for (origin, host) in [
        ("http://127.0.0.1:9999", "127.0.0.1:8477"),
        ("http://localhost:3000", "127.0.0.1:3000"),
        ("http://127.0.0.1", "127.0.0.1:8477"),
    ] {
        assert!(
            !request_is_localhost_trusted(&request(Some(origin), Some(host))),
            "origin {origin} must be rejected for host {host}"
        );
    }
    // Local tools send no Origin at all; the Host gate still applies.
    assert!(request_is_localhost_trusted(&request(
        None,
        Some("127.0.0.1")
    )));
    assert!(!request_is_localhost_trusted(&request(
        None,
        Some("evil.example")
    )));
}

/// Helper child for `run_step_drains_large_child_output`: re-runs this
/// test binary filtered to `spew_stderr_helper`, which floods stderr
/// well past the OS pipe buffer.
#[test]
fn spew_stderr_helper() {
    if std::env::var("FRAMETRACE_TEST_SPEW").as_deref() != Ok("1") {
        return;
    }
    for index in 0..8000 {
        eprintln!("spew line {index}: {}", "x".repeat(80));
    }
}

/// Regression: a pipeline child writing more than the ~64 KiB pipe
/// buffer to stderr must not deadlock the workstation. Before the
/// reader threads, try_wait() polled forever while the child blocked
/// on a full pipe — this test would hang instead of failing.
#[test]
fn run_step_drains_large_child_output() {
    let exe = std::env::current_exe().unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    unsafe {
        std::env::set_var("FRAMETRACE_TEST_SPEW", "1");
    }
    let result = run_step(
        &exe,
        &[
            "serve::tests::spew_stderr_helper".to_string(),
            "--exact".to_string(),
            "--nocapture".to_string(),
        ],
        &state,
    );
    unsafe {
        std::env::remove_var("FRAMETRACE_TEST_SPEW");
    }
    let text = result.expect("spewing child must complete, not deadlock");
    assert!(text.contains("spew line"), "{text}");
    // Retained output stays bounded at the tail cap even though the
    // child emitted ~640 KiB.
    assert!(
        text.len() <= STEP_OUTPUT_TAIL_BYTES + 4096,
        "retained output {} exceeded tail cap",
        text.len()
    );
}

#[test]
fn drain_capped_retains_only_the_tail() {
    let input: Vec<u8> = (0..(STEP_OUTPUT_TAIL_BYTES * 2))
        .map(|index| (index % 251) as u8)
        .collect();
    let drained = drain_capped(&mut input.as_slice());
    assert_eq!(drained.len(), STEP_OUTPUT_TAIL_BYTES);
    assert_eq!(
        drained.as_slice(),
        &input[input.len() - STEP_OUTPUT_TAIL_BYTES..]
    );
}

#[test]
fn mimes_map_to_expected_types() {
    assert_eq!(mime_for(Path::new("a.html")), "text/html; charset=utf-8");
    assert_eq!(mime_for(Path::new("b.MP4")), "video/mp4");
    assert_eq!(mime_for(Path::new("c.unknown")), "application/octet-stream");
}

#[test]
fn server_answers_status_env_and_guards() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let raw_nonce = state_lock(&state).instance.clone();
    thread::spawn(move || serve_on(listener, state));
    let response = |request: &str| {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8_lossy(&buffer).to_string()
    };
    let status = response("GET /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    // SEC-04: the status body publishes only the nonce fingerprint.
    let body = &status[status.find("\r\n\r\n").unwrap() + 4..];
    assert!(!body.contains(&raw_nonce), "raw nonce leaked: {body}");
    assert!(body.contains(&format!(
        "\"instance\":\"{}\"",
        instance_fingerprint(&raw_nonce)
    )));
    // IPv6 loopback Host with a port is a genuine local client.
    let ipv6 = response("GET /api/status HTTP/1.1\r\nHost: [::1]:8477\r\n\r\n");
    assert!(ipv6.contains("200 OK"), "{ipv6}");
    assert!(status.contains("200 OK"));
    assert!(status.contains("\"has_job\":false"));
    assert!(status.contains("\"phase\":\"idle\""));
    let env = response("GET /api/env HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(env.contains("\"ok\":true"));
    assert!(env.contains("\"ffmpeg\":"));
    let review = response("GET /review/nope.html HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(review.contains("404"));
    let media = response("GET /media HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(media.contains("400"));
    // DNS-rebinding gate: a non-loopback Host must be rejected on every
    // route, including the evidence-streaming endpoint.
    let rebinding = response("GET /api/status HTTP/1.1\r\nHost: attacker.example\r\n\r\n");
    assert!(rebinding.contains("403"), "{rebinding}");
    // Cross-site form POST: a foreign Origin on a state-changing route
    // must be rejected without a preflight.
    let csrf = response(
        "POST /api/open-folder HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://evil.example\r\nContent-Type: text/plain\r\nContent-Length: 23\r\n\r\n{\"path\":\"C:\\\\evil.exe\"}",
    );
    assert!(csrf.contains("403"), "{csrf}");
    // Nonce gate (no FRAMETRACE_TOKEN configured): a mutating POST with
    // no nonce proof is refused before reaching the handler. The nonce
    // itself is the per-process instance value (user-private file / the
    // planted ft_nonce cookie), not anything /api/status echoes.
    let nonce = raw_nonce.as_str();
    assert!(!nonce.is_empty());
    // The fingerprint is public, so it must not satisfy the gate.
    let fingerprint_nonce = response(&format!(
        "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nX-FrameTrace-Nonce: {}\r\nContent-Type: application/json\r\nContent-Length: 33\r\n\r\n{{\"source_path\":\"C:\\\\nope\\\\missing\"}}",
        instance_fingerprint(nonce)
    ));
    assert!(fingerprint_nonce.contains("401"), "{fingerprint_nonce}");
    let no_nonce = response(
        "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 33\r\n\r\n{\"source_path\":\"C:\\nope\\missing\"}",
    );
    assert!(no_nonce.contains("401"), "{no_nonce}");
    let bad_nonce = response(
        "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nX-FrameTrace-Nonce: wrong\r\nContent-Type: application/json\r\nContent-Length: 33\r\n\r\n{\"source_path\":\"C:\\nope\\missing\"}",
    );
    assert!(bad_nonce.contains("401"), "{bad_nonce}");
    // The planted ft_nonce cookie and the explicit header both satisfy
    // the gate — the request then reaches the handler's own error path.
    let header = response(&format!(
        "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nX-FrameTrace-Nonce: {nonce}\r\nContent-Type: application/json\r\nContent-Length: 33\r\n\r\n{{\"source_path\":\"C:\\\\nope\\\\missing\"}}"
    ));
    assert!(header.contains("\"ok\":false"), "{header}");
    let cookie = response(&format!(
        "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: ft_nonce={nonce}\r\nContent-Type: application/json\r\nContent-Length: 33\r\n\r\n{{\"source_path\":\"C:\\\\nope\\\\missing\"}}"
    ));
    assert!(cookie.contains("\"ok\":false"), "{cookie}");
    // GETs plant the ft_nonce cookie so the served page's same-origin
    // fetches carry the proof automatically.
    // C2: named after this server's port.
    assert!(
        status.contains(&format!("Set-Cookie: ft_nonce_{}=", addr.port())),
        "GET responses must plant the per-port nonce cookie: {status}"
    );
}

#[test]
fn token_auth_accepts_header_query_and_cookie() {
    let base = |query: &str, cookie: Option<&str>, header: Option<&str>| Request {
        method: "GET".into(),
        path: "/api/status".into(),
        query: query.into(),
        body: String::new(),
        range: None,
        origin: None,
        host: None,
        cookie: cookie.map(str::to_string),
        token_header: header.map(str::to_string),
        nonce_header: None,
    };
    let token = "abc12345-x";
    assert_eq!(
        request_authorized(&base("", None, Some(token)), token),
        AuthProof::Header
    );
    assert_eq!(
        request_authorized(&base("token=abc12345-x", None, None), token),
        AuthProof::Query
    );
    assert_eq!(
        request_authorized(
            &base("", Some("other=1; ft_token=abc12345-x ;x=y"), None),
            token
        ),
        AuthProof::Cookie
    );
    for request in [
        base("", None, None),
        base("", Some("ft_token=wrong"), None),
        base("token=wrong", None, None),
        base("", None, Some("wrong")),
        // A cookie-name prefix must not authenticate.
        base("", Some("ft_tokenx=abc12345-x"), None),
        base("", Some("ft_token=abc12345-x,y=z"), None),
    ] {
        assert_eq!(request_authorized(&request, token), AuthProof::Denied);
    }
    assert!(valid_token("abcdefgh"));
    assert!(!valid_token("short"));
    assert!(!valid_token("has;semi"));
    assert!(!valid_token("has space0"));
}

#[test]
fn token_gate_over_socket_plants_cookie() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).token = Some("testtoken-1".into());
    thread::spawn(move || serve_on(listener, state));
    let response = |request: &str| {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8_lossy(&buffer).to_string()
    };
    // No credential at all -> 401, even for a plain GET.
    let denied = response("GET /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(denied.contains("401"), "{denied}");
    // Query token authenticates and plants the ft_token cookie.
    let query = response("GET /api/status?token=testtoken-1 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(query.contains("200"), "{query}");
    // C2: the cookie is named after the server's port.
    assert!(
        query.contains(&format!(
            "Set-Cookie: ft_token_{}=testtoken-1; HttpOnly; SameSite=Strict",
            addr.port()
        )),
        "{query}"
    );
    let scoped = response(&format!(
        "GET /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: ft_token_{}=testtoken-1\r\n\r\n",
        addr.port()
    ));
    assert!(scoped.contains("200"), "{scoped}");
    // Another port's cookie is not this server's proof.
    let foreign = response(&format!(
        "GET /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: ft_token_{}=testtoken-1\r\n\r\n",
        addr.port().wrapping_add(1)
    ));
    assert!(foreign.contains("401"), "{foreign}");
    // The legacy (pre-C2) cookie name still authorizes for one release
    // when its value validates — header-less follow-ups included (video
    // elements cannot set headers).
    let cookie =
        response("GET /media HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: ft_token=testtoken-1\r\n\r\n");
    assert!(cookie.contains("400"), "{cookie}");
    // And the explicit header works for scripts.
    let header = response(
        "GET /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\nX-FrameTrace-Token: testtoken-1\r\n\r\n",
    );
    assert!(header.contains("200"), "{header}");
    // A token that only prefixes the real one must not authenticate.
    let prefix = response(
        "GET /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: ft_token=testtoken\r\n\r\n",
    );
    assert!(prefix.contains("401"), "{prefix}");
}

#[test]
fn case_files_stream_with_content_length() {
    let case_dir = std::env::temp_dir().join(format!("ft-stream-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&case_dir);
    std::fs::create_dir_all(case_dir.join("reports")).unwrap();
    let payload = "x".repeat(150 * 1024); // larger than one 64 KiB chunk
    std::fs::write(case_dir.join("reports/big.txt"), &payload).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case_dir.clone());
    thread::spawn(move || serve_on(listener, state));
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(b"GET /case/reports/big.txt HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .unwrap();
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let head_end = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response head");
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    assert!(head.contains("200 OK"), "{head}");
    assert!(
        head.contains(&format!("Content-Length: {}", payload.len())),
        "{head}"
    );
    assert_eq!(buffer[head_end + 4..].len(), payload.len());
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn export_copy_allocates_unique_names_atomically() {
    let dir = std::env::temp_dir().join(format!("ft-copyuniq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.bin");
    std::fs::write(&source, b"payload").unwrap();
    let first = copy_unique(&source, &dir, "same__name.mp4").unwrap();
    let second = copy_unique(&source, &dir, "same__name.mp4").unwrap();
    assert_ne!(first, second);
    assert!(first.ends_with("same__name.mp4"));
    assert_eq!(std::fs::read(&first).unwrap(), b"payload");
    assert_eq!(std::fs::read(&second).unwrap(), b"payload");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn download_header_preserves_utf8_filenames() {
    // Korean evidence names must not collapse to underscores: the ASCII
    // fallback stays sanitized while filename*=UTF-8'' carries the real
    // name per RFC 5987.
    let encoded = percent_encode_header_value("사고_블랙박스_전방_20260926_1400.mp4");
    assert!(encoded.contains(".mp4"));
    assert!(encoded.contains('%'), "{encoded}");
    // UTF-8 사 → EC 82 AC
    assert!(encoded.contains("%EC%82%AC"), "{encoded}");
    assert_eq!(
        percent_encode_header_value("plain-file_1.mp4"),
        "plain-file_1.mp4"
    );
    assert_eq!(percent_encode_header_value("a b.mp4"), "a%20b.mp4");
    assert_eq!(percent_encode_header_value("x\"y.mp4"), "x%22y.mp4");
}

#[test]
fn examiner_page_auto_detects_image_extension() {
    // Regression guard: the page must switch inputKind to "e01" when the
    // pasted source path ends in a forensic-image extension, so an E01 is
    // never routed into the folder pipeline by a stale radio selection.
    assert!(EXAMINER_PAGE.contains("looksImage"));
    assert!(EXAMINER_PAGE.contains("\\.(e01|ex01|l01|s01|e02|ex02|l02|s02)$"));
    assert!(EXAMINER_PAGE.contains("input[name='inputKind'][value='e01']"));
}

#[test]
fn legacy_examiner_page_served_when_env_set() {
    // FRAMETRACE_UI=legacy (docs/ui-rebuild-contract.md §9.3) serves the
    // pre-rebuild shell with its iframe viewer; anything else, the current one.
    assert!(legacy_ui_requested(Some("legacy")));
    assert!(legacy_ui_requested(Some(" LEGACY ")));
    for other in [None, Some(""), Some("new"), Some("0")] {
        assert!(!legacy_ui_requested(other), "{other:?}");
    }
    assert_eq!(examiner_page(false), EXAMINER_PAGE);
    let legacy = examiner_page(true);
    assert_eq!(legacy, EXAMINER_PAGE_LEGACY);
    assert!(legacy.contains(r#"id="stageNav""#));
    assert!(legacy.contains(r#"<iframe id="reviewFrame""#));
    assert!(legacy.contains("/review/evidence-viewer.html?embed=1"));
    // A self-contained page: bundles inlined, no viewer fragment of its own.
    assert!(!legacy.contains("<!-- @bundle:app") && !legacy.contains("<!-- @include:"));
    assert!(!legacy.contains(r#"id="ftViewer""#));
    // Same E01 auto-detect guard as the current shell.
    assert!(legacy.contains("looksImage"));
}

#[test]
fn examiner_page_hosts_the_shared_viewer_fragment_at_most_once() {
    // The integrated shell hosts the viewer (<html data-ft-host>): exactly
    // one expanded fragment plus the viewer bundle (window.FTViewer), and no
    // marker text left over (build.rs refuses markers inside shell parts).
    assert!(
        !EXAMINER_PAGE.contains("<!-- @include:") && !EXAMINER_PAGE.contains("<!-- @bundle:app")
    );
    let roots = EXAMINER_PAGE.matches(r#"id="ftViewer""#).count();
    let html_tag = EXAMINER_PAGE
        .find("<html")
        .map(|at| &EXAMINER_PAGE[at..at + EXAMINER_PAGE[at..].find('>').unwrap()])
        .unwrap_or("");
    if html_tag.contains("data-ft-host") {
        assert_eq!(roots, 1);
        assert!(EXAMINER_PAGE.contains(r#"data-ft-viewer="2""#));
        assert_eq!(EXAMINER_PAGE.matches(r#"id="recordGrid""#).count(), 1);
        assert_eq!(
            EXAMINER_PAGE
                .matches("/* ==== assets/viewer/00-host.js ==== */")
                .count(),
            1
        );
        assert_eq!(
            EXAMINER_PAGE
                .matches("/* ==== assets/kit.css ==== */")
                .count(),
            1
        );
    } else {
        assert_eq!(roots, 0);
    }
}

fn post_request(path: &str, body: &str) -> Request {
    Request {
        method: "POST".into(),
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

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// UX-07: a cancel aimed at one job must not poison every later job.
#[test]
fn busy_acquire_resets_stale_cancel_flag() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    {
        let mut guard = state_lock(&state);
        guard.busy = true;
    }
    // Cancel the "running" job, then let it end.
    assert!(api_cancel(&state).contains("\"ok\":true"));
    state_lock(&state).busy = false;
    assert!(state_lock(&state).cancel_requested);
    // The next job acquires a fresh flag and its step is not killed.
    let busy = try_acquire_busy(&state).unwrap();
    assert!(!state_lock(&state).cancel_requested);
    let exe = std::env::current_exe().unwrap();
    let result = run_step(
        &exe,
        &[
            "serve::tests::spew_stderr_helper".to_string(),
            "--exact".to_string(),
        ],
        &state,
    );
    assert!(result.is_ok(), "{result:?}");
    drop(busy);
    assert!(!state_lock(&state).busy);
}

/// SEC-01: open-folder only opens existing directories inside the case or
/// approved roots — never files, missing paths, or foreign folders.
#[test]
fn open_folder_is_confined_to_case_directories() {
    let root = scratch_dir("open-folder");
    let case_dir = root.join("case");
    let outside = root.join("outside");
    std::fs::create_dir_all(case_dir.join("exports/selection-1")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(case_dir.join("exports/run.exe"), b"MZ").unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    // No case bound: nothing can be opened.
    assert!(resolve_open_folder(&state, &case_dir.to_string_lossy()).is_err());
    {
        let mut guard = state_lock(&state);
        guard.case_dir = Some(case_dir.clone());
        guard.media_roots = vec![case_dir.clone()];
    }
    let ok = resolve_open_folder(
        &state,
        &case_dir.join("exports/selection-1").to_string_lossy(),
    );
    assert!(ok.is_ok(), "{ok:?}");
    for bad in [
        String::new(),
        case_dir
            .join("exports/run.exe")
            .to_string_lossy()
            .into_owned(),
        case_dir.join("missing").to_string_lossy().into_owned(),
        outside.to_string_lossy().into_owned(),
        case_dir.join("../outside").to_string_lossy().into_owned(),
        "-help".to_string(),
    ] {
        assert!(resolve_open_folder(&state, &bad).is_err(), "accepted {bad}");
    }
    let refused = api_open_folder(
        &post_request(
            "/api/open-folder",
            &serde_json::json!({"path": outside.to_string_lossy()}).to_string(),
        ),
        &state,
    );
    assert!(refused.contains("\"ok\":false"), "{refused}");
    assert_eq!(
        shell_path(PathBuf::from(r"\\?\C:\case")),
        PathBuf::from(r"C:\case")
    );
    assert_eq!(
        shell_path(PathBuf::from(r"\\?\UNC\srv\share")),
        PathBuf::from(r"\\srv\share")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn host_gate_accepts_bracketed_ipv6_loopback() {
    let request = |host: &str| Request {
        method: "GET".into(),
        path: "/".into(),
        query: String::new(),
        body: String::new(),
        range: None,
        origin: None,
        host: Some(host.to_string()),
        cookie: None,
        token_header: None,
        nonce_header: None,
    };
    for good in [
        "[::1]:8477",
        "[::1]",
        "127.0.0.1:8477",
        "LOCALHOST:1",
        "localhost",
    ] {
        assert!(request_is_localhost_trusted(&request(good)), "{good}");
    }
    for bad in [
        "[::2]:8477",
        "[::1]x",
        "127.0.0.1.evil.com",
        "evil.com:8477",
        "127.0.0.1@evil.com",
        "127.0.0.1:notaport",
        "",
    ] {
        assert!(!request_is_localhost_trusted(&request(bad)), "{bad}");
    }
}

#[test]
fn secret_comparison_is_exact() {
    assert!(secret_eq("abc12345", "abc12345"));
    assert!(!secret_eq("abc12345", "abc12346"));
    assert!(!secret_eq("abc1234", "abc12345"));
    assert!(!secret_eq("", "abc12345"));
    let fingerprint = instance_fingerprint("0123456789abcdef");
    assert_eq!(fingerprint.len(), 24);
    assert_ne!(fingerprint, "0123456789abcdef");
    assert_eq!(instance_fingerprint(""), "");
}

/// SEC-02: generated viewer/report pages render normally; any other
/// script-capable document under the case or media roots is sandboxed
/// and forced to download.
#[test]
fn untrusted_case_documents_are_sandboxed_and_downloaded() {
    let case_dir = scratch_dir("sec02");
    for (rel, body) in [
        ("review/index.html", "<html>viewer</html>"),
        ("review/evidence-viewer.html", "<html>viewer</html>"),
        ("reports/case-report.html", "<html>report</html>"),
        ("reports/qa/consistency-report.html", "<html>qa</html>"),
        ("reports/package_1/review/index.html", "<html>pkg</html>"),
        (
            "reports/package_1/artifacts/carved/x.html",
            "<script>evil</script>",
        ),
        ("evidence/fs/evil.html", "<script>evil</script>"),
        ("evidence/fs/evil.svg", "<svg onload=alert(1)/>"),
        ("review/thumbs/a.jpg", "jpg"),
    ] {
        let path = case_dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let canon = |rel: &str| case_dir.join(rel).canonicalize().unwrap();
    for trusted in [
        "review/index.html",
        "review/evidence-viewer.html",
        "reports/case-report.html",
        "reports/qa/consistency-report.html",
        "reports/package_1/review/index.html",
    ] {
        assert!(is_generated_page(&case_dir, &canon(trusted)), "{trusted}");
        // R4-B5: location alone no longer trusts a page — FrameTrace's
        // generators record what they wrote.
        crate::util::generated_pages::record_generated_pages(&case_dir, &[trusted]).unwrap();
    }
    for untrusted in [
        "reports/package_1/artifacts/carved/x.html",
        "evidence/fs/evil.html",
        "evidence/fs/evil.svg",
        "review/thumbs/a.jpg",
    ] {
        assert!(
            !is_generated_page(&case_dir, &canon(untrusted)),
            "{untrusted}"
        );
    }

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    {
        let mut guard = state_lock(&state);
        guard.case_dir = Some(case_dir.clone());
        guard.media_roots = vec![case_dir.clone()];
    }
    thread::spawn(move || serve_on(listener, state));
    let head = |target: &str| {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .unwrap();
        let mut buffer = Vec::new();
        stream.read_to_end(&mut buffer).unwrap();
        let text = String::from_utf8_lossy(&buffer).to_string();
        text[..text.find("\r\n\r\n").unwrap()].to_string()
    };
    let viewer = head("/review/index.html");
    assert!(viewer.contains("200 OK"), "{viewer}");
    assert!(viewer.contains("text/html"), "{viewer}");
    assert!(viewer.contains("nosniff"), "{viewer}");
    assert!(viewer.contains("frame-ancestors 'self'"), "{viewer}");
    assert!(!viewer.contains("sandbox"), "{viewer}");
    assert!(!viewer.contains("Content-Disposition"), "{viewer}");
    let report = head("/case/reports/case-report.html");
    assert!(!report.contains("Content-Disposition"), "{report}");
    for evil in [
        "/case/evidence/fs/evil.html",
        "/case/evidence/fs/evil.svg",
        "/case/reports/package_1/artifacts/carved/x.html",
    ] {
        let response = head(evil);
        assert!(response.contains("200 OK"), "{response}");
        assert!(response.contains("nosniff"), "{response}");
        assert!(
            response.contains("Content-Security-Policy: sandbox"),
            "{response}"
        );
        assert!(
            response.contains("Content-Disposition: attachment"),
            "{response}"
        );
    }
    let thumb = head("/review/thumbs/a.jpg");
    assert!(thumb.contains("image/jpeg"), "{thumb}");
    assert!(!thumb.contains("Content-Disposition"), "{thumb}");
    // /media never trusts documents, even the generated viewer.
    let media_html = head(&format!(
        "/media?path={}",
        crate::serve::percent_encode_header_value(&canon("review/index.html").to_string_lossy())
    ));
    assert!(
        media_html.contains("Content-Security-Policy: sandbox"),
        "{media_html}"
    );
    assert!(
        media_html.contains("Content-Disposition: attachment"),
        "{media_html}"
    );
    let _ = std::fs::remove_dir_all(&case_dir);
}

/// FOR-06: a capture whose audit entry cannot be written must fail and
/// must not leave an unlogged artifact behind.
#[test]
fn capture_frame_fails_when_audit_append_fails() {
    let case_dir = scratch_dir("capture-audit");
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case_dir.clone());
    let body = |id: &str, time: &str| {
        serde_json::json!({"id": id, "image": "/9j/4AAA", "time": time}).to_string()
    };
    // Invalid id / time are rejected before anything is written.
    for (id, time) in [
        ("", "1"),
        ("vid_1", "NaN"),
        ("vid_1", "-3"),
        ("vid_1", "abc"),
    ] {
        let reply = api_capture_frame(&post_request("/api/capture-frame", &body(id, time)), &state);
        assert!(reply.contains("\"ok\":false"), "{id}/{time}: {reply}");
    }
    let ok = api_capture_frame(
        &post_request("/api/capture-frame", &body("vid_1", "1.5")),
        &state,
    );
    assert!(ok.contains("\"ok\":true"), "{ok}");
    // Make the capture log unwritable (a directory in its place).
    let log = case_dir.join("evidence/logs/capture-log.jsonl");
    std::fs::remove_file(&log).unwrap();
    std::fs::create_dir_all(&log).unwrap();
    let before = std::fs::read_dir(case_dir.join("artifacts/captures"))
        .unwrap()
        .count();
    let failed = api_capture_frame(
        &post_request("/api/capture-frame", &body("vid_2", "2")),
        &state,
    );
    assert!(failed.contains("\"ok\":false"), "{failed}");
    let after = std::fs::read_dir(case_dir.join("artifacts/captures"))
        .unwrap()
        .count();
    assert_eq!(before, after, "unlogged capture must be removed");
    let _ = std::fs::remove_dir_all(&case_dir);
}

/// A capture whose base64 exceeds the generic 4 MiB MAX_BODY (a 4K
/// frame) is accepted: the route allows MAX_CAPTURE_FRAME_BODY. Above
/// that cap it is still refused.
#[test]
fn capture_frame_accepts_payload_above_generic_body_cap() {
    let case_dir = scratch_dir("capture-large");
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case_dir.clone());
    // "/9j/" = FF D8 FF (JPEG magic); each "AAAA" = three zero bytes.
    let groups = 1_200_000; // ~4.8 MB of base64, ~3.6 MB decoded
    let image = format!("/9j/{}", "AAAA".repeat(groups));
    let decoded_len = 3 + 3 * groups as u64;
    assert!(image.len() > MAX_BODY && image.len() < MAX_CAPTURE_FRAME_BODY);
    let body = serde_json::json!({"id": "vid_1", "image": image, "time": "2"}).to_string();
    let reply = api_capture_frame(&post_request("/api/capture-frame", &body), &state);
    assert!(reply.contains("\"ok\":true"), "{reply}");
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    let saved = case_dir.join(reply["path"].as_str().unwrap());
    assert_eq!(std::fs::metadata(&saved).unwrap().len(), decoded_len);
    let too_big = "A".repeat(MAX_CAPTURE_FRAME_BODY + 4);
    let body = serde_json::json!({"id": "vid_1", "image": too_big}).to_string();
    let reply = api_capture_frame(&post_request("/api/capture-frame", &body), &state);
    assert!(reply.contains("\"ok\":false"), "{reply}");
    let _ = std::fs::remove_dir_all(&case_dir);
}

/// FOR-06: selection export reports failure when its audit entry fails.
#[test]
fn export_selected_surfaces_audit_failure() {
    let case_dir = scratch_dir("export-audit");
    let source = case_dir.join("src/a.mp4");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"video").unwrap();
    std::fs::create_dir_all(case_dir.join("evidence/logs/export-log.jsonl")).unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    {
        let mut guard = state_lock(&state);
        guard.case_dir = Some(case_dir.clone());
        guard.media_roots = vec![case_dir.clone()];
    }
    let body = serde_json::json!({"items": [{"id": "vid_1", "path": source.to_string_lossy()}]})
        .to_string();
    let reply: serde_json::Value = serde_json::from_str(&api_export_selected(
        &post_request("/api/export-selected", &body),
        &state,
    ))
    .unwrap();
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(reply["unlogged"], true, "{reply}");
    assert_eq!(reply["copied"], 1, "{reply}");
    let _ = std::fs::remove_dir_all(&case_dir);
}

/// FOR-08: non-fatal step failures are visible in /api/status.
#[test]
fn step_warnings_are_reported_in_status() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    warn_step(&state, 3, "일괄 검증 실패, 건너뜀: boom".into());
    let status: serde_json::Value = serde_json::from_str(&api_status(&state)).unwrap();
    let warnings = status["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["step"], 3);
    assert_eq!(warnings[0]["step_name"], "재생성 검증");
    assert!(warnings[0]["message"].as_str().unwrap().contains("boom"));
    assert!(status["logs"].to_string().contains("경고"));
}

/// PERF-01: the parsed index is shared via Arc (no deep clone per page),
/// refreshed when the file changes, and reused by records-meta.
#[test]
fn records_share_cached_index_and_meta_omits_videos() {
    let case_dir = scratch_dir("records-cache");
    std::fs::create_dir_all(case_dir.join("db")).unwrap();
    let index = case_dir.join("db/video_index.json");
    std::fs::write(
        &index,
        r#"{"schema":"x","videos":[{"id":"vid_1","name":"a.mp4"},{"id":"vid_2","name":"b.mp4"}]}"#,
    )
    .unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case_dir.clone());
    let first = cached_index(&state, &case_dir).unwrap();
    let second = cached_index(&state, &case_dir).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    let request = Request {
        method: "GET".into(),
        path: "/api/records".into(),
        query: "offset=1&limit=1".into(),
        body: String::new(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    };
    let page: serde_json::Value = serde_json::from_str(&api_records(&request, &state)).unwrap();
    assert_eq!(page["total"], 2);
    assert_eq!(page["videos"][0]["id"], "vid_2");
    let meta: serde_json::Value = serde_json::from_str(&api_records_meta(&state)).unwrap();
    assert_eq!(meta["scan"]["schema"], "x");
    assert_eq!(meta["scan"]["videos"].as_array().unwrap().len(), 0);
    // A changed index (different length) invalidates the cache.
    std::fs::write(&index, r#"{"schema":"y","videos":[{"id":"vid_9"}]}"#).unwrap();
    let third = cached_index(&state, &case_dir).unwrap();
    assert!(!Arc::ptr_eq(&first, &third));
    assert_eq!(third["schema"], "y");
    let _ = std::fs::remove_dir_all(&case_dir);
}
