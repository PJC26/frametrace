//! Marks saved while phase-2 enrichment is running (review-partial): they
//! are applied or queued — never refused — and reach the final review.

use super::*;

fn temp_case(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-mlive-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-live"}"#).unwrap();
    dir
}

fn partial_state(case_dir: &Path, step: Option<&'static str>) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case_dir.to_path_buf());
    state.phase = "review-partial";
    state.busy = true;
    state.enrichment = step.map(|step| Enrichment {
        step,
        label: "test",
        stage: 1,
        stages: 4,
    });
    Arc::new(Mutex::new(state))
}

fn import_request(id: &str, case_id: &str) -> Request {
    let marks = serde_json::json!({
        "schema_version": 1,
        "case_id": case_id,
        "marks": [{"id": id, "status": "important", "note": "n"}],
    });
    Request {
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
    }
}

fn import_as(state: &SharedState, id: &str, case_id: &str) -> serde_json::Value {
    serde_json::from_str(&api_import_marks(&import_request(id, case_id), state)).unwrap()
}

fn import(state: &SharedState, id: &str) -> serde_json::Value {
    import_as(state, id, "FT-live")
}

fn stored_ids(case_dir: &Path) -> Vec<String> {
    crate::case_db::load_review_marks(case_dir)
        .unwrap()
        .into_iter()
        .map(|mark| mark.record_id)
        .collect()
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

#[test]
fn marks_apply_immediately_during_short_enrichment_steps() {
    let case_dir = temp_case("direct");
    let state = partial_state(&case_dir, Some("anomalies"));
    let response = import(&state, "vid_000001");
    assert_eq!(response["ok"], true, "{response}");
    // Save protocol v2: `applied` is the {id: revision} map (truthy to
    // the harnesses that only check it is set).
    assert_eq!(response["applied"]["vid_000001"], 1, "{response}");
    assert_eq!(response["deferred"], false);
    assert_eq!(stored_ids(&case_dir), ["vid_000001"]);
    assert_eq!(state_lock(&state).phase, "review-partial");
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn marks_queue_durably_during_hash_and_apply_after() {
    let case_dir = temp_case("queue");
    let state = partial_state(&case_dir, Some("hash"));
    let response = import(&state, "vid_000001");
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["deferred"], true);
    assert_eq!(response["pending"], 1);
    assert!(
        stored_ids(&case_dir).is_empty(),
        "not applied under the scan"
    );
    let status: serde_json::Value = serde_json::from_str(&api_status(&state)).unwrap();
    assert_eq!(status["pending_marks"], 1);
    // Entering a short step applies the queue.
    let job = job_for(&case_dir);
    run_folder_enrichment(&state, &job, |_| Ok("ok".into()));
    assert_eq!(stored_ids(&case_dir), ["vid_000001"]);
    assert_eq!(pending_marks_count(&case_dir), 0);
    assert_eq!(state_lock(&state).phase, "review-ready");
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn marks_saved_during_every_phase_two_step_reach_review_ready() {
    let case_dir = temp_case("e2e");
    let state = partial_state(&case_dir, None);
    let job = job_for(&case_dir);
    let mut calls: Vec<String> = Vec::new();
    let mut make_review_runs = 0;
    let mut marks_seen_by_last_review = 0;
    run_folder_enrichment(&state, &job, |args| {
        calls.push(args[0].clone());
        match args[0].as_str() {
            // During the hash scan: queued.
            "scan-folder" => {
                let r = import(&state, "vid_000001");
                assert_eq!(r["deferred"], true, "{r}");
            }
            // During the anomaly pass: applied at once.
            "qa" => {
                let r = import(&state, "vid_000002");
                assert_eq!(r["applied"]["vid_000002"], 1, "{r}");
                assert_eq!(r["deferred"], false, "{r}");
            }
            "make-review" => {
                make_review_runs += 1;
                if make_review_runs == 1 {
                    // During make-review: queued, forces a second run.
                    let r = import(&state, "vid_000003");
                    assert_eq!(r["deferred"], true, "{r}");
                }
                // What this make-review run would read (queued marks are
                // drained after the run, before the next one).
                marks_seen_by_last_review = stored_ids(&case_dir).len();
            }
            _ => {}
        }
        Ok("ok".into())
    });
    assert_eq!(calls, ["scan-folder", "qa", "make-review", "make-review"]);
    assert_eq!(
        marks_seen_by_last_review, 3,
        "final review includes all marks"
    );
    let mut ids = stored_ids(&case_dir);
    ids.sort();
    assert_eq!(ids, ["vid_000001", "vid_000002", "vid_000003"]);
    assert_eq!(pending_marks_count(&case_dir), 0);
    let status: serde_json::Value = serde_json::from_str(&api_status(&state)).unwrap();
    assert_eq!(status["phase"], "review-ready");
    assert_eq!(status["busy"], false);
    // After review-ready the normal path no longer takes the enrichment route.
    let late = import(&state, "vid_000004");
    // The v2 response always carries `deferred`; off the enrichment route
    // it is false and the report was refreshed.
    assert_eq!(late["deferred"], false, "{late}");
    assert_eq!(
        late["report_url"], "case/reports/case-report.html",
        "{late}"
    );
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn queued_marks_survive_a_cancelled_enrichment() {
    let case_dir = temp_case("cancel");
    let state = partial_state(&case_dir, None);
    let job = job_for(&case_dir);
    run_folder_enrichment(&state, &job, |_| {
        let r = import(&state, "vid_000009");
        assert_eq!(r["deferred"], true, "{r}");
        state_lock(&state).cancel_requested = true;
        Err("사용자가 분석을 중단했습니다.".into())
    });
    assert_eq!(stored_ids(&case_dir), ["vid_000009"]);
    assert_eq!(pending_marks_count(&case_dir), 0);
    assert_eq!(state_lock(&state).phase, "review-partial");
    let _ = std::fs::remove_dir_all(&case_dir);
}

#[test]
fn foreign_case_marks_are_still_rejected_during_enrichment() {
    let case_dir = temp_case("foreign");
    let state = partial_state(&case_dir, Some("anomalies"));
    let response = import_as(&state, "vid_1", "FT-other");
    assert_eq!(response["ok"], false);
    assert!(stored_ids(&case_dir).is_empty());
    assert_eq!(pending_marks_count(&case_dir), 0);
    let _ = std::fs::remove_dir_all(&case_dir);
}
