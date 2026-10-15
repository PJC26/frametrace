//! Background server jobs (UX-06 recover/carve, UX-09 clip export), the
//! read-only case-info / preflight probes (UX-10/11) and the SEC-06
//! argv / media-root confinement checks.

use super::*;

// Included per test file (no mod.rs line needed); see the helper's header.
#[allow(clippy::duplicate_mod)]
#[path = "tests_fake_tools.rs"]
mod fake_tools;
// mod.rs re-exports this only for unix (B-owned gate): reach it by path elsewhere.
#[cfg(not(unix))]
use super::media::exhibit_arg;
use fake_tools::{
    CMD_FIND_OUTPUT, CMD_SPIN, cmd_grandchild_sleeper, cmd_sleep, cmd_wait_for_file, pid_alive,
    write_fake,
};

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

/// Writes a fake CLI (`/bin/sh` on unix, `.cmd` on Windows); `$1` / `%1` is
/// the subcommand.
fn fake_cli(dir: &Path, unix_body: &str, cmd_body: &str) -> PathBuf {
    write_fake(dir, unix_body, cmd_body)
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

/// A patch-v2 request body for one record (dirty id only), optionally
/// chunked as `batch {id, index, total}`.
fn v2_request(
    id: &str,
    note: &str,
    base: u64,
    client: &str,
    batch: Option<(&str, u64, u64)>,
) -> Request {
    let mut marks = serde_json::json!({
        "schema_version": 2,
        "protocol": "patch-v2",
        "case_id": "FT-jobs",
        "examiner": "kim",
        "client_id": client,
        "exported_unix": 1,
        "base_revisions": { id: base },
        "marks": [{"id": id, "status": "important", "marked_unix": 100, "note": note,
                   "examiner": "kim", "range": {"in": 1.0, "out": 2.0}, "tags": ["사고"]}],
        "tags": [],
        "deleted_ids": [],
    });
    if let Some((batch_id, index, total)) = batch {
        marks["batch"] = serde_json::json!({"id": batch_id, "index": index, "total": total});
    }
    request(
        "POST",
        "/api/import-marks",
        "",
        &serde_json::json!({"marks_json": marks.to_string()}).to_string(),
    )
}

/// `make-report` is rebuilt once per batch: never for a middle chunk,
/// once on the last chunk, and every time for a request without `batch`
/// (legacy clients keep their current behaviour).
#[test]
fn import_marks_batch_runs_make_report_once_on_last_chunk() {
    let case_dir = temp_case("batch-report");
    let state = bound_state(&case_dir, "review-ready");
    let reports = std::cell::Cell::new(0usize);
    let runner = |args: &[String]| {
        assert_eq!(args[0], "make-report", "{args:?}");
        assert_eq!(args[1], case_dir.to_string_lossy());
        reports.set(reports.get() + 1);
        Ok(String::new())
    };
    let send = |req: Request| {
        parse(&super::jobs::api_import_marks_with_runner(
            &req, &state, &runner,
        ))
    };

    let first = send(v2_request("vid_1", "a", 0, "w-a", Some(("b-1", 0, 3))));
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["batch_done"], false, "{first}");
    assert!(first["report_url"].is_null(), "{first}");
    assert_eq!(reports.get(), 0);
    let middle = send(v2_request("vid_2", "b", 0, "w-a", Some(("b-1", 1, 3))));
    assert_eq!(middle["batch_done"], false, "{middle}");
    assert_eq!(reports.get(), 0);
    let last = send(v2_request("vid_3", "c", 0, "w-a", Some(("b-1", 2, 3))));
    assert_eq!(last["ok"], true, "{last}");
    assert_eq!(last["batch_done"], true, "{last}");
    assert_eq!(last["report_url"], "case/reports/case-report.html");
    assert_eq!(reports.get(), 1, "exactly one make-report per batch");
    // Every chunk was applied as its own transaction.
    let mut ids: Vec<String> = crate::case_db::load_review_marks(&case_dir)
        .unwrap()
        .into_iter()
        .map(|m| m.record_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["vid_1", "vid_2", "vid_3"]);
    // No batch (single request) and a legacy body: report every time.
    let single = send(v2_request("vid_4", "d", 0, "w-a", None));
    assert_eq!(single["batch_done"], true, "{single}");
    assert_eq!(reports.get(), 2);
    let legacy = send(request(
        "POST",
        "/api/import-marks",
        "",
        &serde_json::json!({"marks_json": serde_json::json!({
            "schema_version": 2, "protocol": "patch-v1", "case_id": "FT-jobs",
            "marks": [{"id": "vid_1", "status": "noted", "note": "legacy"}], "tags": [], "deleted_ids": []
        }).to_string()}).to_string(),
    ));
    assert_eq!(legacy["ok"], true, "{legacy}");
    assert_eq!(legacy["protocol"], "patch-v1");
    assert_eq!(
        legacy["applied"]["vid_1"], 2,
        "legacy writes still bump the revision"
    );
    assert_eq!(reports.get(), 3);
    // The busy lease is released after each request.
    assert!(!state_lock(&state).busy);
    let _ = std::fs::remove_dir_all(case_dir);
}

/// §1.4 response contract: a stale id is reported with the server value
/// and not applied, the other ids in the same body are applied, and a
/// resend with the returned revision goes through.
#[test]
fn import_marks_conflict_response_shape() {
    let case_dir = temp_case("conflict-shape");
    let state = bound_state(&case_dir, "review-ready");
    let runner = |_: &[String]| Ok(String::new());
    let send = |req: Request| {
        parse(&super::jobs::api_import_marks_with_runner(
            &req, &state, &runner,
        ))
    };
    // Window B saves vid_1 first (revision 1).
    let b = send(v2_request("vid_1", "from B", 0, "w-b", None));
    assert_eq!(b["applied"], serde_json::json!({"vid_1": 1}), "{b}");
    assert_eq!(b["conflicts"], serde_json::json!([]), "{b}");
    // Window A, still at base 0 for vid_1, also saves a fresh vid_2.
    let mut marks = serde_json::json!({
        "schema_version": 2, "protocol": "patch-v2", "case_id": "FT-jobs", "examiner": "lee",
        "client_id": "w-a", "base_revisions": {"vid_1": 0, "vid_2": 0},
        "marks": [
            {"id": "vid_1", "status": "noted", "marked_unix": 7, "note": "from A", "examiner": "lee", "tags": []},
            {"id": "vid_2", "status": "reviewed", "marked_unix": 8, "note": "", "examiner": "lee", "tags": ["x"]}
        ],
        "tags": [], "deleted_ids": []
    });
    let a = send(request(
        "POST",
        "/api/import-marks",
        "",
        &serde_json::json!({"marks_json": marks.to_string()}).to_string(),
    ));
    assert_eq!(a["ok"], true, "{a}");
    assert_eq!(a["protocol"], "patch-v2");
    assert_eq!(a["stored"], 1);
    assert_eq!(a["applied"], serde_json::json!({"vid_2": 1}), "{a}");
    assert_eq!(a["batch_done"], true);
    assert_eq!(a["deferred"], false);
    assert_eq!(a["report_url"], "case/reports/case-report.html");
    let conflicts = a["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{a}");
    assert_eq!(
        conflicts[0],
        serde_json::json!({
            "id": "vid_1",
            "server_revision": 1,
            "server": {
                "status": "important", "marked_unix": 100, "examiner": "kim", "note": "from B",
                "range": {"in": 1.0, "out": 2.0}, "tags": ["사고"]
            }
        }),
        "{a}"
    );
    let stored = crate::case_db::load_review_marks(&case_dir).unwrap();
    let vid_1 = stored.iter().find(|m| m.record_id == "vid_1").unwrap();
    assert_eq!(
        vid_1.note.as_deref(),
        Some("from B"),
        "stale save must not apply"
    );
    // "내 값으로 덮어쓰기": resend with the server revision as base.
    marks["base_revisions"] = serde_json::json!({"vid_1": 1});
    marks["marks"].as_array_mut().unwrap().truncate(1);
    let retry = send(request(
        "POST",
        "/api/import-marks",
        "",
        &serde_json::json!({"marks_json": marks.to_string()}).to_string(),
    ));
    assert_eq!(retry["applied"], serde_json::json!({"vid_1": 2}), "{retry}");
    assert_eq!(retry["conflicts"], serde_json::json!([]));
    let stored = crate::case_db::load_review_marks(&case_dir).unwrap();
    let vid_1 = stored.iter().find(|m| m.record_id == "vid_1").unwrap();
    assert_eq!(vid_1.note.as_deref(), Some("from A"));
    assert!(
        crate::case_db::load_review_tags(&case_dir)
            .unwrap()
            .iter()
            .all(|t| t.id != "vid_1"),
        "explicit empty tags cleared the server tags"
    );
    let _ = std::fs::remove_dir_all(case_dir);
}

/// A body over the 4 MiB cap is refused with HTTP 413 before anything is
/// read or applied (the client halves its chunk size on this status).
/// The 413 body is plain text, not JSON — clients branch on the status.
#[test]
fn import_marks_oversize_body_is_413() {
    let case_dir = temp_case("oversize");
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let state = bound_state(&case_dir, "review-ready");
    let server_state = Arc::clone(&state);
    thread::spawn(move || serve_on(listener, server_state));
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let five_mib = 5 * 1024 * 1024;
    write!(
        stream,
        "POST /api/import-marks HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {five_mib}\r\n\r\n"
    )
    .unwrap();
    let _ = stream.write_all(&vec![b'x'; 64 * 1024]);
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    let response = String::from_utf8_lossy(&buffer);
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    assert!(
        crate::case_db::load_review_marks(&case_dir)
            .unwrap()
            .is_empty()
    );
    state_lock(&state).request_shutdown();
    let _ = std::fs::remove_dir_all(case_dir);
}

/// A v2 body queued during a long step whose ids moved on before the
/// drain: the conflict is journaled with the client's original entry
/// (never dropped), `/api/status.mark_conflicts` reports it — also from a
/// fresh server state (restart) — and a later save of that id with a
/// current base resolves it while the journal keeps the original line.
#[test]
fn deferred_drain_records_conflicts_file_and_status_count() {
    let case_dir = temp_case("drain-conflict");
    let state = {
        let mut job_state = JobState::new();
        job_state.case_dir = Some(case_dir.clone());
        job_state.phase = "review-partial";
        job_state.busy = true;
        job_state.enrichment = Some(Enrichment {
            step: "hash",
            label: "test",
            stage: 1,
            stages: 4,
        });
        Arc::new(Mutex::new(job_state))
    };
    let queued = parse(&api_import_marks(
        &v2_request("vid_1", "mine", 0, "w-a", None),
        &state,
    ));
    assert_eq!(queued["ok"], true, "{queued}");
    assert_eq!(queued["deferred"], true);
    assert_eq!(queued["pending"], 1);
    assert_eq!(queued["applied"], serde_json::json!({}));
    assert_eq!(queued["conflicts"], serde_json::json!([]));
    // Meanwhile another window's save lands (revision 1 for vid_1).
    let server_mark = crate::case_db::ReviewMarkRow {
        record_id: "vid_1".into(),
        status: "reviewed".into(),
        marked_unix: 50,
        examiner: Some("lee".into()),
        note: Some("server".into()),
        ..Default::default()
    };
    crate::case_db::patch_review_annotations_by(&case_dir, &[server_mark], &[], &[], Some("lee"))
        .unwrap();
    // The drain at the next short step applies the queue.
    let job = PipelineJob {
        kind: InputKind::Folder,
        case_dir: case_dir.clone(),
        source_path: PathBuf::from("/ev"),
        with_hash: true,
        with_ffprobe: false,
        with_deepfake: false,
        with_audio: false,
        skip_e01_verify: false,
        e01_direct: false,
    };
    run_folder_enrichment(&state, &job, |_| Ok("ok".into()));
    assert_eq!(pending_marks_count(&case_dir), 0);
    assert_eq!(state_lock(&state).phase, "review-ready");
    let stored = crate::case_db::load_review_marks(&case_dir).unwrap();
    assert_eq!(
        stored[0].note.as_deref(),
        Some("server"),
        "stale queued save not applied"
    );
    let journal_path = case_dir.join(super::jobs::PENDING_MARK_CONFLICTS_FILE);
    let journal = std::fs::read_to_string(&journal_path).unwrap();
    let lines: Vec<serde_json::Value> = journal.lines().map(parse).collect();
    assert_eq!(lines.len(), 1, "{journal}");
    assert_eq!(lines[0]["id"], "vid_1");
    assert_eq!(lines[0]["server_revision"], 1);
    assert_eq!(lines[0]["server"]["note"], "server");
    assert_eq!(
        lines[0]["mine"]["mark"]["note"], "mine",
        "original entry kept verbatim"
    );
    assert_eq!(
        lines[0]["mine"]["mark"]["tags"],
        serde_json::json!(["사고"])
    );
    assert_eq!(lines[0]["mine"]["base_revision"], 0);
    assert_eq!(lines[0]["mine"]["deleted"], false);
    assert_eq!(lines[0]["client_id"], "w-a");
    assert!(lines[0]["recorded_unix"].as_u64().unwrap() > 0);
    let logs = state_lock(&state).logs.join("\n");
    assert!(logs.contains("충돌 1건"), "{logs}");
    let status = parse(&api_status(&state));
    assert_eq!(
        status["mark_conflicts"],
        serde_json::json!({"count": 1, "ids": ["vid_1"]}),
        "{status}"
    );
    // Survives a server restart: a fresh state reads the same journal.
    let restarted = bound_state(&case_dir, "review-ready");
    assert_eq!(parse(&api_status(&restarted))["mark_conflicts"]["count"], 1);
    // The examiner decides ("내 값") and saves with the current base.
    let runner = |_: &[String]| Ok(String::new());
    let resolved = parse(&super::jobs::api_import_marks_with_runner(
        &v2_request("vid_1", "mine again", 1, "w-a", None),
        &state,
        &runner,
    ));
    assert_eq!(
        resolved["applied"],
        serde_json::json!({"vid_1": 2}),
        "{resolved}"
    );
    let status = parse(&api_status(&state));
    assert_eq!(
        status["mark_conflicts"],
        serde_json::json!({"count": 0, "ids": []}),
        "{status}"
    );
    let journal = std::fs::read_to_string(&journal_path).unwrap();
    let lines: Vec<serde_json::Value> = journal.lines().map(parse).collect();
    assert_eq!(lines.len(), 2, "journal is append-only: {journal}");
    assert_eq!(lines[0]["mine"]["mark"]["note"], "mine");
    assert_eq!(lines[1]["resolved"], "vid_1");
    assert_eq!(lines[1]["revision"], 2);
    let _ = std::fs::remove_dir_all(case_dir);
}

fn import_body(state: &SharedState, body: serde_json::Value) -> serde_json::Value {
    let runner = |_: &[String]| Ok(String::new());
    parse(&super::jobs::api_import_marks_with_runner(
        &request(
            "POST",
            "/api/import-marks",
            "",
            &serde_json::json!({"marks_json": body.to_string()}).to_string(),
        ),
        state,
        &runner,
    ))
}

/// Queues one v2 body during a long step, lets another (legacy) writer
/// land revision 1 for the same id, and drains — leaving one open queue
/// conflict for `vid_1` at server revision 1.
fn open_queue_conflict(tag: &str) -> (PathBuf, SharedState) {
    let case_dir = temp_case(tag);
    let state = {
        let mut job_state = JobState::new();
        job_state.case_dir = Some(case_dir.clone());
        job_state.phase = "review-partial";
        job_state.busy = true;
        job_state.enrichment = Some(Enrichment {
            step: "hash",
            label: "test",
            stage: 1,
            stages: 4,
        });
        Arc::new(Mutex::new(job_state))
    };
    let queued = parse(&api_import_marks(
        &v2_request("vid_1", "mine", 0, "w-a", None),
        &state,
    ));
    assert_eq!(queued["deferred"], true, "{queued}");
    let server_mark = crate::case_db::ReviewMarkRow {
        record_id: "vid_1".into(),
        status: "reviewed".into(),
        marked_unix: 50,
        examiner: Some("lee".into()),
        note: Some("server".into()),
        ..Default::default()
    };
    crate::case_db::patch_review_annotations_by(&case_dir, &[server_mark], &[], &[], Some("lee"))
        .unwrap();
    let job = PipelineJob {
        kind: InputKind::Folder,
        case_dir: case_dir.clone(),
        source_path: PathBuf::from("/ev"),
        with_hash: true,
        with_ffprobe: false,
        with_deepfake: false,
        with_audio: false,
        skip_e01_verify: false,
        e01_direct: false,
    };
    run_folder_enrichment(&state, &job, |_| Ok("ok".into()));
    assert_eq!(state_lock(&state).phase, "review-ready");
    (case_dir, state)
}

/// §10.2: `GET /api/mark-conflicts` lists every open queue conflict with
/// the server value and the entry as originally queued; finalize refuses
/// while one is open; only a patch-v2 save that applies the id with a base
/// at least the journaled server revision resolves it — not a legacy/v1
/// body, not an unchanged-content save carrying an older base.
#[test]
fn mark_conflicts_endpoint_resolution_rule_and_finalize_block() {
    let (case_dir, state) = open_queue_conflict("conflict-endpoint");
    let listed = parse(&api_mark_conflicts(&state));
    assert_eq!(listed["ok"], true, "{listed}");
    assert_eq!(listed["count"], 1);
    let entry = &listed["conflicts"][0];
    assert_eq!(entry["id"], "vid_1");
    assert_eq!(entry["server_revision"], 1);
    assert_eq!(entry["server"]["note"], "server");
    assert_eq!(entry["server"]["status"], "reviewed");
    assert_eq!(entry["queued"]["mark"]["note"], "mine", "{entry}");
    assert_eq!(entry["queued"]["mark"]["tags"], serde_json::json!(["사고"]));
    assert_eq!(entry["queued"]["base_revision"], 0);
    assert_eq!(entry["queued"]["deleted"], false);
    assert_eq!(entry["client_id"], "w-a");
    assert!(entry["recorded_unix"].as_u64().unwrap() > 0);
    assert_eq!(entry["entries"], 1);
    // Routed as a plain API GET.
    let routed = route(&request("GET", "/api/mark-conflicts", "", ""), &state);
    let routed = String::from_utf8_lossy(&routed);
    let body = routed.split("\r\n\r\n").nth(1).unwrap_or_default();
    assert_eq!(parse(body)["count"], 1, "{routed}");

    // Finalize is refused before anything is written.
    let refused = parse(&finalize_with_exe(
        Path::new("/nonexistent/frametrace"),
        &case_dir,
        "kim",
        &state,
    ));
    assert_eq!(refused["ok"], false);
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("저장 충돌 1건을 먼저 해결하십시오"),
        "{refused}"
    );
    assert!(
        !case_dir
            .join(super::jobs::SIGNOFF_PENDING_REL_PATH)
            .exists()
    );
    assert!(!case_dir.join(crate::report::SIGNOFF_REL_PATH).exists());

    // Identical content from another window with an old base: applied as
    // a no-op at revision 1, but it is not a decision — still open.
    let same = import_body(
        &state,
        serde_json::json!({
            "schema_version": 2, "protocol": "patch-v2", "case_id": "FT-jobs",
            "client_id": "w-c", "base_revisions": {"vid_1": 0},
            "marks": [{"id": "vid_1", "status": "reviewed", "marked_unix": 50,
                       "examiner": "lee", "note": "server"}],
        }),
    );
    assert_eq!(same["applied"], serde_json::json!({"vid_1": 1}), "{same}");
    assert_eq!(parse(&api_mark_conflicts(&state))["count"], 1);
    // A patch-v1 (last write wins) body never resolves.
    let v1 = import_body(
        &state,
        serde_json::json!({
            "schema_version": 2, "protocol": "patch-v1", "case_id": "FT-jobs",
            "marks": [{"id": "vid_1", "status": "reviewed", "marked_unix": 50, "note": "legacy"}],
            "tags": [], "deleted_ids": [],
        }),
    );
    assert_eq!(v1["applied"], serde_json::json!({"vid_1": 2}), "{v1}");
    assert_eq!(parse(&api_status(&state))["mark_conflicts"]["count"], 1);
    // The window decides with the current base (2 >= journaled 1).
    let decided = parse(&super::jobs::api_import_marks_with_runner(
        &v2_request("vid_1", "decided", 2, "w-a", None),
        &state,
        &|_: &[String]| Ok(String::new()),
    ));
    assert_eq!(
        decided["applied"],
        serde_json::json!({"vid_1": 3}),
        "{decided}"
    );
    let listed = parse(&api_mark_conflicts(&state));
    assert_eq!(
        listed,
        serde_json::json!({"ok": true, "count": 0, "conflicts": []})
    );
    assert_eq!(
        parse(&api_status(&state))["mark_conflicts"],
        serde_json::json!({"count": 0, "ids": []})
    );
    let _ = std::fs::remove_dir_all(case_dir);
}

/// `/api/records-meta` annotations carry the per-record revision map the
/// v2 client seeds `state.revisions` from (empty before any save).
#[test]
fn records_meta_annotations_include_revisions() {
    let case_dir = temp_case("meta-revisions");
    let state = bound_state(&case_dir, "review-ready");
    let empty = parse(&api_records_meta(&state));
    assert_eq!(empty["ok"], true, "{empty}");
    assert_eq!(empty["annotations"]["marks"], serde_json::json!([]));
    assert_eq!(empty["annotations"]["revisions"], serde_json::json!({}));
    let runner = |_: &[String]| Ok(String::new());
    let saved = parse(&super::jobs::api_import_marks_with_runner(
        &v2_request("vid_1", "n", 0, "w-a", None),
        &state,
        &runner,
    ));
    assert_eq!(saved["ok"], true, "{saved}");
    crate::case_db::patch_review_annotations_by(
        &case_dir,
        &[],
        &[crate::selection::TagEntry {
            id: "vid_9".into(),
            tags: vec!["t".into()],
        }],
        &[],
        None,
    )
    .unwrap();
    let meta = parse(&api_records_meta(&state));
    let annotations = &meta["annotations"];
    assert_eq!(
        annotations["marks"][0]["record_id"], "vid_1",
        "{annotations}"
    );
    assert_eq!(
        annotations["marks"][0]["range"],
        serde_json::json!({"in": 1.0, "out": 2.0})
    );
    assert_eq!(
        annotations["tags"],
        serde_json::json!([{"id": "vid_1", "tags": ["사고"]}, {"id": "vid_9", "tags": ["t"]}])
    );
    assert_eq!(
        annotations["revisions"],
        serde_json::json!({"vid_1": 1, "vid_9": 1}),
        "{annotations}"
    );
    // §10.2: the window that wrote each current revision (null = legacy).
    assert_eq!(
        annotations["writers"],
        serde_json::json!({"vid_1": "w-a", "vid_9": null}),
        "{annotations}"
    );
    // Same object make-review embeds in the data bundle, minus `writers`
    // (writer tokens stay out of the packaged bundle).
    let bundle: serde_json::Value =
        serde_json::from_str(&crate::case_db::review_annotations_json(&case_dir).unwrap()).unwrap();
    assert!(bundle.get("writers").is_none(), "{bundle}");
    let mut served = annotations.clone();
    served.as_object_mut().unwrap().remove("writers");
    assert_eq!(bundle, served);
    let _ = std::fs::remove_dir_all(case_dir);
}

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
        &format!(
            "if \"%1\"==\"recover-batch\" (\n{}\necho recovered 3 candidates\nexit /b 0\n)\nif \"%1\"==\"make-review\" (\necho review rebuilt\nexit /b 0\n)\nexit /b 9",
            cmd_sleep(1)
        ),
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
        &format!(
            "if \"%1\"==\"recover-batch\" (\n{}\nexit /b 0\n)\necho ok",
            cmd_grandchild_sleeper(&pid_file)
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

#[test]
fn a_failed_recover_step_is_reported_not_swallowed() {
    let case_dir = temp_case("recover-fail");
    write_inspection(&case_dir);
    let exe = fake_cli(
        &case_dir,
        r#"echo "image unreadable" >&2; exit 3"#,
        "echo image unreadable 1>&2\nexit /b 3",
    );
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
        &format!(
            "if \"%1\"==\"recover-batch\" (\n{}\necho recovered\nexit /b 0\n)\nif \"%1\"==\"make-review\" echo review\nexit /b 0",
            cmd_wait_for_file(&gate)
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

/// Batch twin of [`FAKE_EXPORT`]: the raw argument line goes to argv.txt
/// (cmd splits `--exhibit=<v>` on `=`, so per-arg lines are not possible).
fn fake_export_cmd() -> String {
    format!(
        "echo %* >> \"%~dp0argv.txt\"\nset first=%1\n{CMD_FIND_OUTPUT}\nif \"%first%\"==\"export-video\" (\n{}\necho clip-bytes-%out% > \"%out%\"\necho exported\nexit /b 0\n)\nexit /b 0",
        cmd_sleep(1)
    )
}

fn clip_body(extra: &str) -> String {
    format!(r#"{{"id":"vid_000001","start":"1.500","duration":"4.000"{extra}}}"#)
}

#[test]
fn double_export_returns_the_same_job_and_clip_sha256() {
    let case_dir = temp_case("clip-dedupe");
    let exe = fake_cli(&case_dir, FAKE_EXPORT, &fake_export_cmd());
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
    assert!(
        rel.starts_with("artifacts/clips/vid_000001__1s+4s"),
        "{rel}"
    );
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
            r#"{"id":"vid_000001","start":"9","duration":"1"}"#,
        ),
        &state,
        &exe,
    ));
    assert_ne!(other["job"]["id"], id.as_str());
}

/// SEC-06: the exhibit label is one `--exhibit=<value>` argv token, so a
/// value starting with `-` cannot become a separate flag.
#[test]
fn exhibit_label_travels_as_a_single_argv_token() {
    let case_dir = temp_case("clip-exhibit");
    let exe = fake_cli(&case_dir, FAKE_EXPORT, &fake_export_cmd());
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
    // Unix logs one argv per line, the batch twin the raw line: compare tokens.
    let lines: Vec<&str> = argv
        .split_whitespace()
        .map(|token| token.trim_matches('"'))
        .collect();
    assert!(lines.contains(&"--exhibit=--hash-source"), "{argv}");
    assert!(!lines.contains(&"--hash-source"), "{argv}");
    assert!(!lines.contains(&"--exhibit"), "{argv}");
    assert_eq!(exhibit_arg("-x"), "--exhibit=-x");
}

/// UX-09: marks saving keeps working while an export runs, and finalize
/// waits for the export instead of sealing a torn clip.
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
        &format!(
            "{CMD_FIND_OUTPUT}\n{}\necho clip> \"%out%\"",
            cmd_wait_for_file(&gate)
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
        finalize["error"]
            .as_str()
            .unwrap()
            .contains("내보내기·이어붙이기"),
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

#[test]
fn clip_cancel_endpoint_kills_only_that_export() {
    let case_dir = temp_case("clip-cancel");
    let exe = fake_cli(&case_dir, "sleep 300", CMD_SPIN);
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
    state_lock(&state).request_shutdown();
}

/// R4 F11 (P12): `marks-imported.json` is replaced wholesale through the
/// atomic temp+rename writer: a stale earlier import never survives and no
/// `.tmp-` sibling is left behind. (Failure-path cleanup needs a writer seam
/// from track B; see the F11 integration TODO.)
#[test]
fn marks_imported_json_is_rewritten_atomically_without_temp_leftovers() {
    let case_dir = temp_case("marks-atomic");
    let marks_path = case_dir.join("marks-imported.json");
    std::fs::write(&marks_path, b"stale half-written import").unwrap();
    let state = bound_state(&case_dir, "review-ready");
    let saved = parse(&super::jobs::api_import_marks_with_runner(
        &marks_request("vid_0007"),
        &state,
        &|_| Ok(String::new()),
    ));
    assert_eq!(saved["ok"], true, "{saved}");
    let written = std::fs::read_to_string(&marks_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&written)
        .unwrap_or_else(|err| panic!("marks-imported.json is not whole JSON ({err}): {written}"));
    assert_eq!(json["marks"][0]["id"], "vid_0007");
    let leftovers: Vec<String> = std::fs::read_dir(&case_dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(case_dir);
}
