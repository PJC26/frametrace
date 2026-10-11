//! Background server jobs (UX-06 recover/carve, UX-09 clip export), the
//! read-only case-info / preflight probes (UX-10/11) and the SEC-06
//! argv / media-root confinement checks.

use super::*;

fn temp_case(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-jobs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-jobs"}"#).unwrap();
    dir
}

fn bound_state(case_dir: &Path, phase: &'static str) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case_dir.to_path_buf());
    state.media_roots = vec![case_dir.to_path_buf()];
    state.phase = phase;
    Arc::new(Mutex::new(state))
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

fn status(state: &SharedState) -> serde_json::Value {
    parse(&api_status(state))
}

fn wait_for(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
    while !check() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// Writes an executable `/bin/sh` script standing in for the frametrace
/// CLI; `$1` is the subcommand.
#[cfg(unix)]
fn fake_cli(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-frametrace.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn write_inspection(case_dir: &Path) {
    let dir = case_dir.join("db/filesystem");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tsk-inspection-1.json"),
        br#"{"image_path":"/nonexistent/evidence.raw","partition_offset":2048}"#,
    )
    .unwrap();
}

fn marks_request(id: &str) -> Request {
    let marks = serde_json::json!({
        "schema_version": 1,
        "case_id": "FT-jobs",
        "marks": [{"id": id, "status": "important", "note": "n"}],
    });
    request(
        "POST",
        "/api/import-marks",
        "",
        &serde_json::json!({"marks_json": marks.to_string()}).to_string(),
    )
}

#[cfg(unix)]
fn pid_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
#[test]
fn recover_returns_immediately_and_status_reaches_done() {
    let case_dir = temp_case("recover-done");
    write_inspection(&case_dir);
    let exe = fake_cli(
        &case_dir,
        r#"case "$1" in
  recover-batch) sleep 1; echo "recovered 3 candidates" ;;
  make-review) echo "review rebuilt" ;;
  *) exit 9 ;;
esac"#,
    );
    let state = bound_state(&case_dir, "review-ready");
    let started = std::time::Instant::now();
    let response = parse(&recover_deleted_with_exe(&state, &exe));
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "recover must return before the step finishes"
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["job"]["kind"], "recover-deleted");
    assert!(response["job"]["started_unix"].as_u64().unwrap() > 0);

    let running = status(&state);
    assert_eq!(running["busy"], true);
    assert_eq!(running["job_kind"], "recover-deleted");
    assert_eq!(running["job"]["state"], "running");
    // A second job is refused while the first holds the busy lease.
    let second = parse(&recover_deleted_with_exe(&state, &exe));
    assert_eq!(second["ok"], false, "{second}");

    wait_for("recover job done", Duration::from_secs(20), || {
        status(&state)["job"]["state"] == "done"
    });
    let done = status(&state);
    assert_eq!(done["busy"], false);
    assert_eq!(done["phase"], "review-ready");
    assert!(done["job_kind"].is_null());
    assert_eq!(done["job"]["stage"], 2);
    assert!(done["job"]["finished_unix"].as_u64().is_some());
    let logs = done["logs"].to_string();
    assert!(logs.contains("recovered 3 candidates"), "{logs}");
    assert!(logs.contains("review rebuilt"), "{logs}");
}

#[cfg(unix)]
#[test]
fn cancel_kills_the_recover_step_tree() {
    let case_dir = temp_case("recover-cancel");
    write_inspection(&case_dir);
    let pid_file = case_dir.join("grandchild.pid");
    let exe = fake_cli(
        &case_dir,
        &format!(
            r#"case "$1" in
  recover-batch) sleep 300 & echo $! > "{}"; wait ;;
  *) echo ok ;;
esac"#,
            pid_file.display()
        ),
    );
    let state = bound_state(&case_dir, "review-partial");
    let response = parse(&recover_deleted_with_exe(&state, &exe));
    assert_eq!(response["ok"], true, "{response}");
    wait_for("grandchild started", Duration::from_secs(10), || {
        std::fs::read_to_string(&pid_file).is_ok_and(|text| !text.trim().is_empty())
    });
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_string();
    assert!(pid_alive(&pid));
    assert_eq!(parse(&api_cancel(&state))["ok"], true);
    wait_for("job cancelled", Duration::from_secs(10), || {
        status(&state)["job"]["state"] == "cancelled"
    });
    let after = status(&state);
    assert_eq!(after["busy"], false);
    // An index-only case stays index-only: make-review never ran.
    assert_eq!(after["phase"], "review-partial");
    assert!(after["job"]["error"].as_str().unwrap().contains("중단"));
    wait_for("grandchild killed", Duration::from_secs(5), || {
        !pid_alive(&pid)
    });
}

#[test]
fn recover_and_carve_reject_missing_inputs_without_taking_busy() {
    let case_dir = temp_case("reject");
    let state = bound_state(&case_dir, "review-ready");
    let exe = PathBuf::from("/nonexistent/frametrace");
    let recover = parse(&recover_deleted_with_exe(&state, &exe));
    assert_eq!(recover["ok"], false);
    let carve = parse(&carve_with_exe(
        &request("POST", "/api/carve", "", "{}"),
        &state,
        &exe,
    ));
    assert_eq!(carve["ok"], false);
    assert!(!state_lock(&state).busy);
    assert!(state_lock(&state).case_job.is_none());
}

#[cfg(unix)]
#[test]
fn a_failed_recover_step_is_reported_not_swallowed() {
    let case_dir = temp_case("recover-fail");
    write_inspection(&case_dir);
    let exe = fake_cli(&case_dir, r#"echo "image unreadable" >&2; exit 3"#);
    let state = bound_state(&case_dir, "review-ready");
    assert_eq!(parse(&recover_deleted_with_exe(&state, &exe))["ok"], true);
    wait_for("job failed", Duration::from_secs(10), || {
        status(&state)["job"]["state"] == "failed"
    });
    let after = status(&state);
    assert_eq!(after["busy"], false);
    assert!(
        after["error"]
            .as_str()
            .unwrap()
            .contains("image unreadable")
    );
    assert!(after["job"]["error"].as_str().unwrap().contains("exit"));
}

/// Marks saved while a recover job runs are queued durably and applied
/// before the job releases `busy` (make-review re-runs to include them).
#[cfg(unix)]
#[test]
fn marks_saved_during_recover_are_queued_and_applied() {
    let case_dir = temp_case("recover-marks");
    write_inspection(&case_dir);
    let gate = case_dir.join("release-recover");
    let exe = fake_cli(
        &case_dir,
        &format!(
            r#"case "$1" in
  recover-batch) while [ ! -f "{}" ]; do sleep 0.05; done; echo recovered ;;
  make-review) echo review ;;
esac"#,
            gate.display()
        ),
    );
    let state = bound_state(&case_dir, "review-ready");
    assert_eq!(parse(&recover_deleted_with_exe(&state, &exe))["ok"], true);
    let saved = parse(&api_import_marks(&marks_request("vid_0042"), &state));
    assert_eq!(saved["ok"], true, "{saved}");
    assert_eq!(saved["deferred"], true, "{saved}");
    assert_eq!(status(&state)["pending_marks"], 1);
    std::fs::write(&gate, b"go").unwrap();
    wait_for("job done", Duration::from_secs(20), || {
        status(&state)["job"]["state"] == "done"
    });
    assert_eq!(status(&state)["pending_marks"], 0);
    let stored: Vec<String> = crate::case_db::load_review_marks(&case_dir)
        .unwrap()
        .into_iter()
        .map(|mark| mark.record_id)
        .collect();
    assert_eq!(stored, vec!["vid_0042".to_string()]);
}

#[cfg(unix)]
const FAKE_EXPORT: &str = r#"out=""; prev=""
for a in "$@"; do
  printf '%s\n' "$a" >> "$(dirname "$0")/argv.txt"
  if [ "$prev" = "--output" ]; then out="$a"; fi
  prev="$a"
done
case "$1" in
  export-video) sleep ${FAKE_SLEEP:-1}; printf 'clip-bytes-%s' "$out" > "$out"; echo exported ;;
  *) exit 0 ;;
esac"#;

fn clip_body(extra: &str) -> String {
    format!(r#"{{"id":"vid_0001","start":"1.500","duration":"4.000"{extra}}}"#)
}

#[cfg(unix)]
#[test]
fn double_export_returns_the_same_job_and_clip_sha256() {
    let case_dir = temp_case("clip-dedupe");
    let exe = fake_cli(&case_dir, FAKE_EXPORT);
    let state = bound_state(&case_dir, "review-ready");
    let req = request("POST", "/api/export-clip", "", &clip_body(""));
    let first = parse(&export_clip_with_exe(&req, &state, &exe));
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["job"]["state"], "running");
    assert_eq!(first["deduped"], false);
    let second = parse(&export_clip_with_exe(&req, &state, &exe));
    assert_eq!(second["job"]["id"], first["job"]["id"], "{second}");
    assert_eq!(second["deduped"], true);
    // The export does not hold the global busy lock.
    assert_eq!(status(&state)["busy"], false);
    assert_eq!(status(&state)["clips_running"], 1);

    let id = first["job"]["id"].as_str().unwrap().to_string();
    let query = format!("job={id}");
    wait_for("clip done", Duration::from_secs(20), || {
        parse(&api_export_clip_status(
            &request("GET", "/api/export-clip-status", &query, ""),
            &state,
        ))["job"]["state"]
            == "done"
    });
    let done = parse(&api_export_clip_status(
        &request("GET", "/api/export-clip-status", &query, ""),
        &state,
    ));
    let rel = done["path"].as_str().unwrap();
    assert!(rel.starts_with("artifacts/clips/vid_0001__1s+4s"), "{rel}");
    let digest = crate::audit::digest_file(&case_dir.join(rel)).unwrap();
    assert_eq!(done["sha256"], digest);
    assert_eq!(done["job"]["sha256"], digest);
    // Identical request after completion: same job, same clip and hash.
    let third = parse(&export_clip_with_exe(&req, &state, &exe));
    assert_eq!(third["job"]["id"], id.as_str());
    assert_eq!(third["sha256"], digest);
    // Exactly one export ran.
    let argv = std::fs::read_to_string(case_dir.join("argv.txt")).unwrap();
    assert_eq!(argv.matches("export-video").count(), 1, "{argv}");
    // A different range is a different job.
    let other = parse(&export_clip_with_exe(
        &request(
            "POST",
            "/api/export-clip",
            "",
            r#"{"id":"vid_0001","start":"9","duration":"1"}"#,
        ),
        &state,
        &exe,
    ));
    assert_ne!(other["job"]["id"], id.as_str());
}

/// SEC-06: the exhibit label is one `--exhibit=<value>` argv token, so a
/// value starting with `-` cannot become a separate flag.
#[cfg(unix)]
#[test]
fn exhibit_label_travels_as_a_single_argv_token() {
    let case_dir = temp_case("clip-exhibit");
    let exe = fake_cli(&case_dir, FAKE_EXPORT);
    let state = bound_state(&case_dir, "review-ready");
    let req = request(
        "POST",
        "/api/export-clip",
        "",
        &clip_body(r#","burn_in":"true","exhibit":"--hash-source""#),
    );
    let response = parse(&export_clip_with_exe(&req, &state, &exe));
    assert_eq!(response["ok"], true, "{response}");
    let id = response["job"]["id"].as_str().unwrap().to_string();
    wait_for("clip finished", Duration::from_secs(20), || {
        state_lock(&state)
            .clip_jobs
            .iter()
            .any(|job| job.id == id && job.state != "running")
    });
    let argv = std::fs::read_to_string(case_dir.join("argv.txt")).unwrap();
    let lines: Vec<&str> = argv.lines().collect();
    assert!(lines.contains(&"--exhibit=--hash-source"), "{argv}");
    assert!(!lines.contains(&"--hash-source"), "{argv}");
    assert!(!lines.contains(&"--exhibit"), "{argv}");
    assert_eq!(exhibit_arg("-x"), "--exhibit=-x");
}

/// UX-09: marks saving keeps working while an export runs, and finalize
/// waits for the export instead of sealing a torn clip.
#[cfg(unix)]
#[test]
fn marks_import_succeeds_while_an_export_runs() {
    let case_dir = temp_case("clip-marks");
    let gate = case_dir.join("release-export");
    let exe = fake_cli(
        &case_dir,
        &format!(
            r#"out=""; prev=""
for a in "$@"; do if [ "$prev" = "--output" ]; then out="$a"; fi; prev="$a"; done
while [ ! -f "{}" ]; do sleep 0.05; done
printf clip > "$out""#,
            gate.display()
        ),
    );
    let state = bound_state(&case_dir, "review-ready");
    let started = parse(&export_clip_with_exe(
        &request("POST", "/api/export-clip", "", &clip_body("")),
        &state,
        &exe,
    ));
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(running_clip_job_count(&state), 1);
    // The marks path takes `busy` itself; the running export must not
    // hold it. (The test binary stands in for import-marks/make-report:
    // with unknown filters it runs no tests and exits 0.)
    let saved = parse(&api_import_marks(&marks_request("vid_0001"), &state));
    assert_eq!(saved["ok"], true, "{saved}");
    assert_eq!(running_clip_job_count(&state), 1, "export still running");
    let finalize = parse(&api_finalize(
        &request("POST", "/api/finalize", "", r#"{"examiner":"Kim"}"#),
        &state,
    ));
    assert_eq!(finalize["ok"], false);
    assert!(
        finalize["error"].as_str().unwrap().contains("클립"),
        "{finalize}"
    );
    // Shutdown refuses while the export runs; the global cancel stops it.
    assert_eq!(parse(&api_shutdown(&state))["running"], true);
    assert_eq!(parse(&api_cancel(&state))["ok"], true);
    wait_for("clip cancelled", Duration::from_secs(10), || {
        running_clip_job_count(&state) == 0
    });
    let job = state_lock(&state).clip_jobs[0].clone();
    assert_eq!(job.state, "cancelled");
    assert!(!job.output.exists(), "a cancelled clip must not linger");
    std::fs::write(&gate, b"go").unwrap();
}

#[cfg(unix)]
#[test]
fn clip_cancel_endpoint_kills_only_that_export() {
    let case_dir = temp_case("clip-cancel");
    let exe = fake_cli(&case_dir, "sleep 300");
    let state = bound_state(&case_dir, "review-ready");
    let started = parse(&export_clip_with_exe(
        &request("POST", "/api/export-clip", "", &clip_body("")),
        &state,
        &exe,
    ));
    let id = started["job"]["id"].as_str().unwrap().to_string();
    let missing = parse(&api_export_clip_cancel(
        &request(
            "POST",
            "/api/export-clip-cancel",
            "",
            r#"{"job":"clip-999"}"#,
        ),
        &state,
    ));
    assert_eq!(missing["ok"], false);
    let cancel = parse(&api_export_clip_cancel(
        &request(
            "POST",
            "/api/export-clip-cancel",
            "",
            &format!(r#"{{"job":"{id}"}}"#),
        ),
        &state,
    ));
    assert_eq!(cancel["ok"], true, "{cancel}");
    wait_for("clip cancelled", Duration::from_secs(10), || {
        running_clip_job_count(&state) == 0
    });
    let listed = status(&state)["clip_jobs"][0].clone();
    assert_eq!(listed["state"], "cancelled");
    assert!(listed["sha256"].is_null());
}

#[test]
fn clip_export_rejects_bad_input_and_foreign_paths() {
    let case_dir = temp_case("clip-reject");
    let state = bound_state(&case_dir, "review-ready");
    let exe = PathBuf::from("/nonexistent/frametrace");
    for body in [
        r#"{"id":"vid_1","start":"-1","duration":"2"}"#,
        r#"{"id":"vid_1","start":"0","duration":"0"}"#,
        r#"{"id":"","start":"0","duration":"2"}"#,
        r#"{"id":"carve_1","start":"0","duration":"2","path":"/etc/hosts"}"#,
    ] {
        let response = parse(&export_clip_with_exe(
            &request("POST", "/api/export-clip", "", body),
            &state,
            &exe,
        ));
        assert_eq!(response["ok"], false, "{body}: {response}");
    }
    // A new export is refused while another job holds `busy`.
    state_lock(&state).busy = true;
    let busy = parse(&export_clip_with_exe(
        &request("POST", "/api/export-clip", "", &clip_body("")),
        &state,
        &exe,
    ));
    assert_eq!(busy["ok"], false);
    assert!(state_lock(&state).clip_jobs.is_empty());
}

#[test]
fn case_info_counts_from_a_synthetic_checkpoint() {
    let case_dir = temp_case("case-info");
    let evidence = case_dir.join("evidence-src");
    std::fs::create_dir_all(&evidence).unwrap();
    let mut lines = vec![
        r#"{"schema_version":1,"checkpoint":"scan-folder","run_id":"scan-folder-1","fingerprint":"abc"}"#
            .to_string(),
    ];
    for (index, name) in ["a.mp4", "b.mp4", "c.mp4"].iter().enumerate() {
        let path = evidence.join(name);
        std::fs::write(&path, vec![0u8; 100 + index]).unwrap();
        let mtime = std::fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // c.mp4's recorded size no longer matches the file on disk.
        let size = if index == 2 { 999 } else { 100 + index };
        lines.push(
            serde_json::json!({
                "id": format!("vid_{index:04}"),
                "source_path": path.to_string_lossy(),
                "size_bytes": size,
                "modified_unix": mtime,
            })
            .to_string(),
        );
    }
    // A torn tail line is counted, never silently dropped.
    lines.push(r#"{"id":"vid_0003","source_pa"#.to_string());
    std::fs::write(scan_progress_path(&case_dir), lines.join("\n")).unwrap();
    std::fs::write(
        case_dir.join("db/video_index.json"),
        r#"{"videos":[{"id":"vid_0000"},{"id":"vid_0001"},{"id":"vid_0002"},{"id":"vid_0003"},{"id":"vid_0004"}]}"#,
    )
    .unwrap();
    let state = bound_state(&case_dir, "review-partial");
    let info = parse(&api_case_info(
        &request("GET", "/api/case-info", "", ""),
        &state,
    ));
    assert_eq!(info["ok"], true, "{info}");
    assert_eq!(info["checkpoint"]["done"], 3, "{info}");
    assert_eq!(info["checkpoint"]["total"], 5);
    assert_eq!(info["checkpoint"]["fingerprint_matches"], 2);
    assert_eq!(info["checkpoint"]["unreadable_lines"], 1);
    assert_eq!(info["checkpoint"]["kind"], "scan-folder");
    assert_eq!(info["has_index"], true);
    assert_eq!(info["has_review"], false);
    assert_eq!(info["phase"], "review-partial");
    assert_eq!(info["bound"], true);

    // Another case folder (not bound): phase derived from its files, no
    // checkpoint → null.
    let other = temp_case("case-info-other");
    let query = format!("case_dir={}", other.display());
    let info = parse(&api_case_info(
        &request("GET", "/api/case-info", &query, ""),
        &state,
    ));
    assert_eq!(info["ok"], true, "{info}");
    assert!(info["checkpoint"].is_null());
    assert_eq!(info["phase"], "idle");
    assert_eq!(info["has_index"], false);
    // Not a case folder → explicit error.
    let query = format!("case_dir={}", evidence.display());
    let info = parse(&api_case_info(
        &request("GET", "/api/case-info", &query, ""),
        &state,
    ));
    assert_eq!(info["ok"], false);
}

#[test]
fn preflight_flags_case_inside_source_and_reports_free_space() {
    let root = temp_case("preflight");
    let source = root.join("source");
    std::fs::create_dir_all(source.join("sub")).unwrap();
    std::fs::write(source.join("a.mp4"), vec![0u8; 1000]).unwrap();
    std::fs::write(source.join("sub/b.DAV"), vec![0u8; 500]).unwrap();
    std::fs::write(source.join("notes.txt"), b"x").unwrap();

    let inside = preflight_json(
        &source,
        &source.join("case-new"),
        10_000,
        Duration::from_secs(5),
    );
    assert_eq!(inside["ok"], true, "{inside}");
    assert_eq!(inside["case_inside_source"], true);
    assert!(
        inside["warnings"].to_string().contains("증거 소스 안"),
        "{inside}"
    );
    assert!(inside["free_bytes"].as_u64().unwrap() > 0);
    assert_eq!(inside["complete"], true);
    assert_eq!(inside["est_files"], 3);
    assert_eq!(inside["est_video_files"], 2);
    assert_eq!(inside["est_total_bytes"], 1501);
    assert_eq!(inside["sampled_bytes"], 1501);

    let outside = preflight_json(
        &source,
        &root.join("cases/new"),
        10_000,
        Duration::from_secs(5),
    );
    assert_eq!(outside["case_inside_source"], false, "{outside}");
    assert!(outside["free_bytes"].as_u64().is_some());

    // The walk is bounded: a tiny entry cap yields a flagged estimate.
    for index in 0..20 {
        std::fs::create_dir_all(source.join(format!("d{index}"))).unwrap();
        std::fs::write(source.join(format!("d{index}/clip.mp4")), b"0123").unwrap();
    }
    let capped = preflight_json(&source, &root.join("cases/new"), 5, Duration::from_secs(5));
    assert_eq!(capped["complete"], false, "{capped}");
    assert!(capped["sampled_files"].as_u64().unwrap() <= 5);
    assert!(capped["warnings"].to_string().contains("추정치"));

    // Over HTTP-shaped input: a missing source is an explicit error.
    let missing = parse(&api_preflight(&request(
        "POST",
        "/api/preflight",
        "",
        r#"{"source_path":"/nonexistent/ft-preflight","case_dir":"/tmp/x"}"#,
    )));
    assert_eq!(missing["ok"], false);
}

/// SEC-06: /media serves only below the approved roots — the case plus
/// exactly the registered source. A sibling of the source, or a symlink
/// inside it pointing elsewhere, is refused; a volume-root source is
/// flagged rather than silently widening anything.
#[test]
fn media_is_confined_to_the_registered_source() {
    let root = temp_case("media-roots");
    let case_dir = root.join("case");
    let source = root.join("source");
    std::fs::create_dir_all(&case_dir).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("in.mp4"), b"inside").unwrap();
    std::fs::write(root.join("outside.mp4"), b"outside").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("outside.mp4"), source.join("link.mp4")).unwrap();

    let roots = start_media_roots(InputKind::Folder, &case_dir, &source);
    assert_eq!(roots, vec![case_dir.clone(), source.clone()]);
    assert_eq!(
        start_media_roots(InputKind::E01, &case_dir, &source),
        vec![case_dir.clone()]
    );
    assert!(source_is_volume_root(Path::new("/")));
    assert!(!source_is_volume_root(&source));

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let mut job_state = JobState::new();
    job_state.case_dir = Some(case_dir.clone());
    job_state.media_roots = roots;
    let state: SharedState = Arc::new(Mutex::new(job_state));
    let server_state = Arc::clone(&state);
    thread::spawn(move || serve_on(listener, server_state));
    let get = |path: &Path| {
        let mut stream = TcpStream::connect(addr).unwrap();
        let encoded: String = path
            .to_string_lossy()
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'/' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        write!(
            stream,
            "GET /media?path={encoded} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
        )
        .unwrap();
        let mut text = String::new();
        let _ = stream.read_to_string(&mut text);
        text
    };
    let inside = get(&source.join("in.mp4"));
    assert!(inside.starts_with("HTTP/1.1 200"), "{inside}");
    let outside = get(&root.join("outside.mp4"));
    assert!(outside.starts_with("HTTP/1.1 403"), "{outside}");
    #[cfg(unix)]
    {
        let link = get(&source.join("link.mp4"));
        assert!(link.starts_with("HTTP/1.1 403"), "{link}");
    }
    state_lock(&state).shutdown_requested = true;
}
