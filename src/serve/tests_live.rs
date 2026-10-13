//! UX-02 two-stage pipeline: review-partial status, live /api/records over
//! the scan checkpoint, index generation, and cancel during enrichment.

use super::*;

fn temp_case(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-live-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("case.json"), b"{}").unwrap();
    dir
}

fn get(path: &str, query: &str) -> Request {
    Request {
        method: "GET".into(),
        path: path.into(),
        query: query.into(),
        body: String::new(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    }
}

fn shared(case_dir: &Path, phase: &'static str) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case_dir.to_path_buf());
    state.phase = phase;
    Arc::new(Mutex::new(state))
}

fn video(id: &str, sha: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "source_path": format!("/ev/{id}.mp4"),
        "relative_path": format!("{id}.mp4"),
        "sha256": sha,
        "hash_status": if sha.is_some() { "complete" } else { "skipped" },
    })
}

fn write_index(case_dir: &Path, videos: &[serde_json::Value]) {
    let index = serde_json::json!({"video_count": videos.len(), "videos": videos});
    std::fs::write(case_dir.join("db/video_index.json"), index.to_string()).unwrap();
}

fn job_for(case_dir: &Path) -> PipelineJob {
    PipelineJob {
        kind: InputKind::Folder,
        case_dir: case_dir.to_path_buf(),
        source_path: PathBuf::from("/ev"),
        with_hash: true,
        with_ffprobe: false,
        with_deepfake: false,
        with_audio: false,
        skip_e01_verify: false,
        e01_direct: false,
    }
}

fn status(state: &SharedState) -> serde_json::Value {
    serde_json::from_str(&api_status(state)).unwrap()
}

#[test]
fn status_exposes_review_partial_after_phase_one() {
    let case_dir = temp_case("status");
    write_index(&case_dir, &[video("vid_000001", None)]);
    let state = shared(&case_dir, "running");
    state_lock(&state).busy = true;
    enter_review_partial(&state);
    state_lock(&state).enrichment = Some(Enrichment {
        step: "hash",
        label: "해시 · 메타데이터 보강",
        stage: 1,
        stages: 3,
    });
    let st = status(&state);
    assert_eq!(st["phase"], "review-partial");
    assert_eq!(st["has_job"], true);
    assert_eq!(st["busy"], true);
    assert_eq!(st["enrichment"]["step"], "hash");
    assert_eq!(st["enrichment"]["stage"], 1);
    assert_eq!(st["enrichment"]["total"], 3);
    assert!(st["index_generation"].as_u64().unwrap() >= 1);
    // Outside phase 2 there is no enrichment block.
    state_lock(&state).enrichment = None;
    assert!(status(&state)["enrichment"].is_null());
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn live_records_overlay_a_synthetic_checkpoint() {
    let case_dir = temp_case("overlay");
    write_index(
        &case_dir,
        &[video("vid_000001", None), video("vid_000002", None)],
    );
    // Header, one enriched record for an indexed file, one brand-new
    // record, and a torn trailing line from a write in progress.
    let checkpoint = format!(
        "{}\n{}\n{}\n{{\"id\":\"vid_0000",
        r#"{"schema_version":1,"checkpoint":"scan-folder","run_id":"r","fingerprint":"f"}"#,
        video("vid_000002", Some("ab12")),
        video("vid_000003", Some("cd34")),
    );
    std::fs::write(case_dir.join("db/scan-progress.jsonl"), checkpoint).unwrap();
    let state = shared(&case_dir, "review-partial");
    // Phase 2 is actually running: only then is the checkpoint live.
    state_lock(&state).busy = true;

    let live: serde_json::Value =
        serde_json::from_str(&api_records(&get("/api/records", "live=1"), &state)).unwrap();
    assert_eq!(live["ok"], true);
    assert_eq!(live["live"], true);
    assert_eq!(live["total"], 3);
    assert_eq!(live["live_records"], 2);
    assert_eq!(live["live_skipped"], 1, "torn line is counted, not hidden");
    let videos = live["videos"].as_array().unwrap();
    // Replaced in place (offsets stay stable), new record appended.
    assert_eq!(videos[0]["id"], "vid_000001");
    assert_eq!(videos[1]["id"], "vid_000002");
    assert_eq!(videos[1]["sha256"], "ab12");
    assert_eq!(videos[2]["id"], "vid_000003");
    assert!(live["generation"].as_u64().is_some());

    // Paging works over the merged list.
    let page: serde_json::Value = serde_json::from_str(&api_records(
        &get("/api/records", "live=1&offset=2&limit=1"),
        &state,
    ))
    .unwrap();
    assert_eq!(page["videos"][0]["id"], "vid_000003");

    // Without live=1 the index alone is served.
    let strict: serde_json::Value =
        serde_json::from_str(&api_records(&get("/api/records", ""), &state)).unwrap();
    assert_eq!(strict["live"], false);
    assert_eq!(strict["total"], 2);
    assert!(strict["videos"][1]["sha256"].is_null());
    assert!(strict["etag"].is_string());

    // A leftover checkpoint outside running/review-partial is ignored.
    state_lock(&state).phase = "review-ready";
    let ready: serde_json::Value =
        serde_json::from_str(&api_records(&get("/api/records", "live=1"), &state)).unwrap();
    assert_eq!(ready["live"], false);
    assert_eq!(ready["total"], 2);
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn live_records_serve_checkpoint_before_first_index() {
    let case_dir = temp_case("noindex");
    let checkpoint = format!(
        "{}\n{}\n",
        r#"{"schema_version":1,"checkpoint":"scan-folder","run_id":"r","fingerprint":"f"}"#,
        video("vid_000001", None),
    );
    std::fs::write(case_dir.join("db/scan-progress.jsonl"), checkpoint).unwrap();
    let state = shared(&case_dir, "running");
    state_lock(&state).busy = true;
    let live: serde_json::Value =
        serde_json::from_str(&api_records(&get("/api/records", "live=1"), &state)).unwrap();
    assert_eq!(live["ok"], true);
    assert_eq!(live["total"], 1);
    // The strict path still reports the missing index honestly.
    let strict: serde_json::Value =
        serde_json::from_str(&api_records(&get("/api/records", ""), &state)).unwrap();
    assert_eq!(strict["ok"], false);
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn generation_bumps_when_index_changes() {
    let case_dir = temp_case("generation");
    write_index(&case_dir, &[video("vid_000001", None)]);
    let state = shared(&case_dir, "review-partial");
    let first = index_generation(&state, &case_dir);
    assert_eq!(
        index_generation(&state, &case_dir),
        first,
        "stable while nothing changed"
    );
    // Different length guarantees a new fingerprint even on coarse mtimes.
    write_index(
        &case_dir,
        &[video("vid_000001", Some("ab12")), video("vid_000002", None)],
    );
    let second = index_generation(&state, &case_dir);
    assert!(second > first, "index rewrite bumps generation");
    // A checkpoint appearing (live data) bumps it too.
    std::fs::write(case_dir.join("db/scan-progress.jsonl"), "{}\n").unwrap();
    let third = index_generation(&state, &case_dir);
    assert!(third > second);
    // Status and records report the same counter.
    assert_eq!(status(&state)["index_generation"].as_u64(), Some(third));
    let records: serde_json::Value =
        serde_json::from_str(&api_records(&get("/api/records", "live=1"), &state)).unwrap();
    assert_eq!(records["generation"].as_u64(), Some(third));
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn cancel_during_phase_two_keeps_review_partial() {
    let case_dir = temp_case("cancel");
    write_index(&case_dir, &[video("vid_000001", None)]);
    let state = shared(&case_dir, "running");
    {
        let mut guard = state_lock(&state);
        guard.busy = true;
        guard.steps = [
            StepStatus::Done,
            StepStatus::Done,
            StepStatus::Done,
            StepStatus::Pending,
            StepStatus::Pending,
        ];
    }
    enter_review_partial(&state);
    let job = job_for(&case_dir);
    let mut calls: Vec<Vec<String>> = Vec::new();
    // The examiner cancels while the hash pass runs; run_step would kill
    // the child and report the cancel as an error.
    run_folder_enrichment(&state, &job, |args| {
        calls.push(args.to_vec());
        state_lock(&state).cancel_requested = true;
        Err("사용자가 분석을 중단했습니다.".to_string())
    });
    assert_eq!(calls.len(), 1, "no further step after the cancel");
    assert_eq!(calls[0][0], "scan-folder");
    assert!(calls[0].iter().any(|a| a == "--incremental"));
    assert!(calls[0].iter().any(|a| a == "--hash"));
    let st = status(&state);
    assert_eq!(st["phase"], "review-partial");
    assert_eq!(st["busy"], false);
    assert!(st["error"].is_null(), "a cancel is not an error");
    assert!(st["enrichment"].is_null());
    let warnings = st["warnings"].as_array().unwrap();
    assert_eq!(
        warnings.len(),
        1,
        "the incomplete run must not read as clean"
    );
    assert!(
        warnings[0]["message"]
            .as_str()
            .unwrap()
            .contains("해시 · 메타데이터 보강")
    );
    // The phase-1 index is still served.
    let records: serde_json::Value =
        serde_json::from_str(&api_records(&get("/api/records", "live=1"), &state)).unwrap();
    assert_eq!(records["total"], 1);
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn enrichment_failure_keeps_review_and_reports_error() {
    let case_dir = temp_case("failure");
    write_index(&case_dir, &[video("vid_000001", None)]);
    let state = shared(&case_dir, "review-partial");
    state_lock(&state).busy = true;
    let job = job_for(&case_dir);
    let mut steps: Vec<String> = Vec::new();
    run_folder_enrichment(&state, &job, |args| {
        steps.push(args[0].clone());
        match args[0].as_str() {
            "scan-folder" => Err("hash read error".to_string()),
            "make-review" => Err("review write failed".to_string()),
            _ => Ok("ok".to_string()),
        }
    });
    // A failed hash pass is a warning; the run continues to make-review.
    assert_eq!(steps, ["scan-folder", "qa", "make-review"]);
    let st = status(&state);
    assert_eq!(st["phase"], "review-partial");
    assert_eq!(st["busy"], false);
    assert_eq!(st["error"], "review write failed");
    assert_eq!(st["steps"][4], "failed");
    assert_eq!(st["warnings"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn enrichment_success_reaches_review_ready() {
    let case_dir = temp_case("success");
    write_index(&case_dir, &[video("vid_000001", None)]);
    let state = shared(&case_dir, "review-partial");
    state_lock(&state).busy = true;
    let mut job = job_for(&case_dir);
    job.with_hash = false;
    let mut steps: Vec<String> = Vec::new();
    run_folder_enrichment(&state, &job, |args| {
        steps.push(args[0].clone());
        Ok("ok".to_string())
    });
    // Nothing to enrich per file: no second scan at all.
    assert_eq!(steps, ["qa", "make-review"]);
    let st = status(&state);
    assert_eq!(st["phase"], "review-ready");
    assert_eq!(st["busy"], false);
    assert_eq!(st["steps"][3], "done");
    assert_eq!(st["steps"][4], "done");
    let _ = std::fs::remove_dir_all(&case_dir);
}

/// NICE: an index-only case opened via /api/open-case lands in
/// `review-partial` with no job running. A `db/scan-progress.jsonl` left
/// by a dead run must not be overlaid as "live" data then.
#[test]
fn stale_checkpoint_is_ignored_without_an_active_run() {
    let case_dir = temp_case("stalelive");
    write_index(&case_dir, &[video("vid_000001", None)]);
    let checkpoint = format!(
        "{}\n{}\n{}\n",
        r#"{"schema_version":1,"checkpoint":"scan-folder","run_id":"r","fingerprint":"f"}"#,
        video("vid_000001", Some("dead")),
        video("vid_000009", Some("beef")),
    );
    std::fs::write(case_dir.join("db/scan-progress.jsonl"), checkpoint).unwrap();
    let state = shared(&case_dir, "review-partial");
    assert!(!state_lock(&state).busy);
    let records: serde_json::Value =
        serde_json::from_str(&api_records(&get("/api/records", "live=1"), &state)).unwrap();
    assert_eq!(records["live"], false);
    assert_eq!(records["total"], 1);
    assert!(records["videos"][0]["sha256"].is_null());
    let _ = std::fs::remove_dir_all(&case_dir);
}

/// The live checkpoint is parsed once per (mtime, len), not on every poll.
#[test]
fn live_checkpoint_parse_is_cached_by_mtime_and_len() {
    let case_dir = temp_case("livecache");
    let header =
        r#"{"schema_version":1,"checkpoint":"scan-folder","run_id":"r","fingerprint":"f"}"#;
    let path = case_dir.join("db/scan-progress.jsonl");
    std::fs::write(&path, format!("{header}\n{}\n", video("vid_000001", None))).unwrap();
    let state = shared(&case_dir, "running");
    state_lock(&state).busy = true;
    let (first, _) = cached_live_checkpoint(&state, &case_dir);
    let (again, _) = cached_live_checkpoint(&state, &case_dir);
    assert!(
        Arc::ptr_eq(&first, &again),
        "unchanged checkpoint re-parsed"
    );
    std::fs::write(
        &path,
        format!(
            "{header}\n{}\n{}\n",
            video("vid_000001", None),
            video("vid_000002", None)
        ),
    )
    .unwrap();
    let (grown, _) = cached_live_checkpoint(&state, &case_dir);
    assert_eq!(grown.len(), 2, "appended line must invalidate the cache");
    let _ = std::fs::remove_dir_all(&case_dir);
}

/// Blocker 1/2: phase 1 is an incremental, hash-less scan, so re-analysing
/// an existing case reuses recorded hashes (scan::tests::
/// rerun_after_cancelled_hash_pass_reuses_computed_hashes runs these exact
/// options end to end).
#[test]
fn phase_one_scan_is_incremental() {
    let args = phase1_scan_args("/case", "/ev");
    assert_eq!(args[0], "scan-folder");
    assert!(args.iter().any(|a| a == "--incremental"), "{args:?}");
    assert!(args.iter().any(|a| a == "--no-ffprobe"), "{args:?}");
    assert!(!args.iter().any(|a| a == "--hash"), "{args:?}");
}

#[test]
fn viewer_route_serves_embedded_page() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    for path in ["/viewer/", "/viewer/index.html"] {
        let mut request = get(path, "embed=1");
        request.host = Some("127.0.0.1:8477".into());
        let response = String::from_utf8_lossy(&route(&request, &state)).to_string();
        assert!(response.starts_with("HTTP/1.1 200"), "{path}");
        assert!(response.contains("frame-ancestors 'self'"));
        assert!(response.contains("__FRAMETRACE_DATA__ = null"));
        assert!(response.contains("startLiveIndexChip"));
    }
    assert!(!is_viewer_page_path("/viewer/thumbs/a.jpg"));
}
