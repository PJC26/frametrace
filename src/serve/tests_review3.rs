//! Round-3 adversarial review findings: sign-off binding of ranges/tags,
//! sign-off on a failed mark read, cancel scoping, clip-job races, phase
//! during recover/carve, open-case vs running exports, verify-sources
//! cancel.

use super::*;

// Included per test file (no mod.rs line needed); see the helper's header.
#[allow(clippy::duplicate_mod)]
#[path = "tests_fake_tools.rs"]
mod fake_tools;
use fake_tools::{CMD_FIND_OUTPUT, CMD_SPIN, cmd_sleep, write_fake};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-rev3-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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
        "frametrace binary not built at {}",
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

/// Case with one indexed source. Returns (root, case).
fn case_fixture(label: &str) -> (PathBuf, PathBuf) {
    let root = scratch(label);
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("a.mp4"), b"fixture-video-bytes-0123456789").unwrap();
    let case = root.join("case");
    run_cli(&["init-case", case.to_str().unwrap()]);
    run_cli(&[
        "scan-folder",
        case.to_str().unwrap(),
        src_dir.to_str().unwrap(),
        "--hash",
        "--no-ffprobe",
    ]);
    (root, case)
}

fn bound(case: &Path, phase: &'static str) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case.to_path_buf());
    state.media_roots = vec![case.to_path_buf()];
    state.phase = phase;
    Arc::new(Mutex::new(state))
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("invalid JSON {err}: {text}"))
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

/// Imports one mark (fixed `marked_unix`, so only the range/tags differ
/// between imports) plus optional tags.
fn import_mark(root: &Path, case: &Path, range_out: f64, tags: &[&str]) {
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(case.join("case.json")).unwrap()).unwrap();
    let mut marks = serde_json::json!({
        "schema_version": 2,
        "case_id": manifest["case_id"],
        "marks": [{
            "id": "vid_000001", "status": "important", "note": "n",
            "examiner": "kim", "marked_unix": 100,
            "range": {"in": 1.0, "out": range_out},
        }],
    });
    if !tags.is_empty() {
        marks["tags"] = serde_json::json!([{"id": "vid_000001", "tags": tags}]);
    }
    let path = root.join("marks.json");
    std::fs::write(&path, marks.to_string()).unwrap();
    crate::cli::handlers::import_marks_quiet(case, &path).unwrap();
}

fn signoff_check(case: &Path, root: &Path, tag: &str) -> crate::package::PackageCheck {
    let out = root.join(format!("pkg-{tag}"));
    crate::package::package_case(case, Some(&out)).unwrap();
    let verification = crate::package::verify_package(&out).unwrap();
    verification
        .checks
        .into_iter()
        .find(|c| c.subject == crate::report::SIGNOFF_REL_PATH)
        .expect("sign-off check present")
}

/// BLOCKER 1: after a sign-off, moving a mark's IN-OUT range (or adding a
/// tag) must make make-report and verify-package say the sign-off no
/// longer covers the content.
#[test]
fn signoff_detects_range_and_tag_changes() {
    let (root, case) = case_fixture("signoff-range");
    import_mark(&root, &case, 2.0, &[]);
    let state = bound(&case, "review-ready");
    let response = finalize_with_exe(&cli_bin(), &case, "홍길동", &state);
    assert!(response.contains("\"ok\":true"), "{response}");
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(
        report.contains("판독 마크는 변경되지 않았습니다"),
        "fresh sign-off"
    );
    assert_eq!(
        signoff_check(&case, &root, "fresh").status,
        crate::package::CheckStatus::Pass
    );

    // Range moved (same status / note / marked_unix).
    import_mark(&root, &case, 9.5, &[]);
    crate::cli::handlers::make_report(&case, false, false, None).unwrap();
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(
        report.contains("이 기록 이후 판독 마크가 변경되었습니다"),
        "range change after sign-off must be reported"
    );
    let check = signoff_check(&case, &root, "range");
    assert_eq!(
        check.status,
        crate::package::CheckStatus::Fail,
        "{}",
        check.detail
    );

    // Range restored, tag added: still a change.
    import_mark(&root, &case, 2.0, &["번호판"]);
    crate::cli::handlers::make_report(&case, false, false, None).unwrap();
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(report.contains("이 기록 이후 판독 마크가 변경되었습니다"));
    assert_eq!(
        signoff_check(&case, &root, "tags").status,
        crate::package::CheckStatus::Fail
    );
    let signoff = std::fs::read_to_string(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap();
    assert!(signoff.contains("\"schema_version\":4"), "{signoff}");
    let _ = std::fs::remove_dir_all(root);
}

/// BLOCKER 1: a pre-v3 sign-off (status/note binding only) must read as
/// "binding unknown", never as "unchanged".
#[test]
fn legacy_signoff_reads_as_binding_unknown() {
    let (root, case) = case_fixture("signoff-legacy");
    import_mark(&root, &case, 2.0, &[]);
    let legacy =
        crate::report::signoff_content_sha256(&crate::cli::handlers::review_marks_json_from_rows(
            &crate::case_db::load_review_marks(&case).unwrap(),
        ));
    std::fs::write(
        case.join(crate::report::SIGNOFF_REL_PATH),
        format!(r#"{{"schema_version":2,"examiner":"홍길동","signed_unix":5,"content_sha256":"{legacy}"}}"#),
    )
    .unwrap();
    crate::cli::handlers::make_report(&case, false, false, None).unwrap();
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(
        !report.contains("변경되지 않았습니다"),
        "legacy must not read as unchanged"
    );
    assert!(report.contains("IN-OUT 구간·태그가 이후 바뀌었는지는 확인할 수 없습니다"));
    let check = signoff_check(&case, &root, "legacy");
    assert_eq!(
        check.status,
        crate::package::CheckStatus::NotVerified,
        "{}",
        check.detail
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Digest pin (save protocol v2): the sign-off content must not include
/// the v7 revision table, so an existing sign-off still reads as
/// "unchanged" after the migration and after revision bumps that change
/// no content. Both the JSON string and its SHA-256 are pinned.
#[test]
fn signoff_content_json_unchanged_by_revision_table() {
    let rows = vec![
        crate::case_db::ReviewMarkRow {
            record_id: "vid_000002".into(),
            status: "reviewed".into(),
            marked_unix: 200,
            ..Default::default()
        },
        crate::case_db::ReviewMarkRow {
            record_id: "vid_000001".into(),
            status: "important".into(),
            marked_unix: 100,
            record_path: Some("/e/a.mp4".into()),
            examiner: Some("kim".into()),
            note: Some("메모 \"q\"".into()),
            range: Some(crate::case_db::RangeSpan {
                start: Some(1.5),
                end: Some(4.0),
            }),
        },
    ];
    let tags = vec![
        crate::selection::TagEntry {
            id: "vid_000009".into(),
            tags: vec!["tag-only".into()],
        },
        crate::selection::TagEntry {
            id: "vid_000001".into(),
            tags: vec!["사고".into(), "보류".into(), "사고".into()],
        },
    ];
    let expected = concat!(
        r#"{"schema":3,"marks":[{"id":"vid_000001","status":"important","marked_unix":100,"examiner":"kim","note":"메모 \"q\"","range_in":1.5,"range_out":4.0},"#,
        r#"{"id":"vid_000002","status":"reviewed","marked_unix":200,"examiner":null,"note":null,"range_in":null,"range_out":null}],"#,
        r#""tags":{"vid_000001":["보류","사고"],"vid_000009":["tag-only"]}}"#
    );
    let content = crate::cli::handlers::signoff_content_json_from(&rows, &tags);
    assert_eq!(content, expected);
    assert_eq!(
        crate::report::signoff_content_sha256(&content),
        "e16c86c1dc661a526d17237a1b349a0db2eff7a1b265712262dc3e90c96aba50"
    );
    // Through a real v7 case: revisions move, the digest does not.
    let case = scratch("signoff-rev-pin");
    std::fs::create_dir_all(case.join("db")).unwrap();
    std::fs::write(case.join("case.json"), br#"{"case_id":"FT-pin"}"#).unwrap();
    let base = std::collections::BTreeMap::new();
    let patch = crate::case_db::AnnotationPatch {
        marks: &rows,
        tags: &tags,
        deleted_ids: &[],
        base_revisions: Some(&base),
    };
    crate::case_db::patch_review_annotations_v2(&case, &patch, Some("kim"), Some("w-1")).unwrap();
    let stored = crate::cli::handlers::signoff_content_json(&case).unwrap();
    assert_eq!(stored, expected);
    assert!(!stored.contains("revision"));
    // Legacy digest helper is byte-stable too (older sign-offs compare to it).
    let loaded = crate::case_db::load_review_marks(&case).unwrap();
    assert_eq!(
        crate::cli::handlers::review_marks_json_from_rows(&loaded),
        r#"[{"id":"vid_000001","status":"important","marked_unix":100,"examiner":"kim","note":"메모 \"q\""},{"id":"vid_000002","status":"reviewed","marked_unix":200}]"#
    );
    assert_eq!(
        crate::case_db::load_review_revisions(&case).unwrap().len(),
        3
    );
    let _ = std::fs::remove_dir_all(case);
}

/// Finding 8: a review-mark read failure must refuse the sign-off rather
/// than bind it to an empty mark list.
#[test]
fn signoff_refused_when_marks_cannot_be_read() {
    let root = scratch("signoff-dberr");
    let case = root.join("case");
    std::fs::create_dir_all(case.join("db")).unwrap();
    std::fs::create_dir_all(case.join("review")).unwrap();
    std::fs::write(case.join("case.json"), br#"{"case_id":"FT-x"}"#).unwrap();
    std::fs::write(
        crate::case_db::case_db_path(&case),
        b"this is not a sqlite database at all, just garbage bytes.................",
    )
    .unwrap();
    let err = super::jobs::write_signoff(&case, crate::report::SIGNOFF_REL_PATH, "홍길동")
        .expect_err("sign-off over an unreadable DB must fail");
    assert!(err.contains("최종화 차단"), "{err}");
    assert!(!case.join(crate::report::SIGNOFF_REL_PATH).exists());
    let _ = std::fs::remove_dir_all(root);
}

fn running_clip(id: &str, key: &str, output: PathBuf) -> ClipJob {
    ClipJob {
        id: id.into(),
        kind: "export-clip",
        key: key.into(),
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

/// Finding 2: cancelling the job that holds `busy` (a re-hash) must not
/// kill an unrelated clip export; with nothing busy, cancel stops clips.
#[test]
fn cancel_targets_the_busy_job_not_clip_exports() {
    let root = scratch("cancel-scope");
    let state = bound(&root, "review-ready");
    {
        let mut guard = state_lock(&state);
        guard
            .clip_jobs
            .push(running_clip("clip-1", "k", root.join("c.mp4")));
        guard.busy = true;
    }
    let response = parse(&api_cancel(&state));
    assert_eq!(response["ok"], true, "{response}");
    {
        let guard = state_lock(&state);
        assert!(guard.cancel_requested);
        assert!(
            !guard.clip_jobs[0].cancel_requested,
            "a busy-job cancel must leave the clip export running"
        );
    }
    assert_eq!(response["cancelled"], "job", "{response}");
    assert_eq!(response["clips_cancelled"], 0, "{response}");
    state_lock(&state).busy = false;
    let response = parse(&api_cancel(&state));
    assert_eq!(response["cancelled"], "clips", "{response}");
    assert_eq!(response["clips_cancelled"], 1, "{response}");
    assert!(state_lock(&state).clip_jobs[0].cancel_requested);
    let _ = std::fs::remove_dir_all(root);
}

/// Writes a fake CLI (`/bin/sh` on unix, `.cmd` on Windows).
fn fake_cli(dir: &Path, unix_body: &str, cmd_body: &str) -> PathBuf {
    write_fake(dir, unix_body, cmd_body)
}

const SLOW_EXPORT: &str = r#"out=""; prev=""
for a in "$@"; do
  if [ "$prev" = "--output" ]; then out="$a"; fi
  prev="$a"
done
case "$1" in
  export-video) sleep 3; printf 'clip' > "$out"; echo exported ;;
  *) exit 0 ;;
esac"#;

fn slow_export_cmd() -> String {
    format!(
        "set first=%1\n{CMD_FIND_OUTPUT}\nif \"%first%\"==\"export-video\" (\n{}\necho clip> \"%out%\"\necho exported\nexit /b 0\n)\nexit /b 0",
        cmd_sleep(3)
    )
}

fn temp_case(label: &str) -> PathBuf {
    let dir = scratch(label);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-rev3"}"#).unwrap();
    dir
}

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !check() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// Finding 3: an identical export request must not join a job whose
/// cancel is already pending (it would end "cancelled" under the caller).
#[test]
fn export_dedupe_skips_cancel_pending_jobs() {
    let case = temp_case("clip-cancel-dedupe");
    let exe = fake_cli(&case, SLOW_EXPORT, &slow_export_cmd());
    let state = bound(&case, "review-ready");
    let body = r#"{"id":"vid_000001","start":"1","duration":"2"}"#;
    let req = request("POST", "/api/export-clip", "", body);
    let first = parse(&export_clip_with_exe(&req, &state, &exe));
    let first_id = first["job"]["id"].as_str().unwrap().to_string();
    let cancel = parse(&api_export_clip_cancel(
        &request(
            "POST",
            "/api/export-clip-cancel",
            "",
            &format!(r#"{{"job":"{first_id}"}}"#),
        ),
        &state,
    ));
    assert_eq!(cancel["ok"], true, "{cancel}");
    let second = parse(&export_clip_with_exe(&req, &state, &exe));
    assert_eq!(second["ok"], true, "{second}");
    assert_ne!(
        second["job"]["id"], first_id,
        "a cancel-pending job must not be reused: {second}"
    );
    assert_eq!(second["deduped"], false);
    let second_id = second["job"]["id"].as_str().unwrap().to_string();
    wait_for("second clip done", || {
        state_lock(&state)
            .clip_jobs
            .iter()
            .any(|job| job.id == second_id && job.state == "done")
    });
    let _ = std::fs::remove_dir_all(case);
}

/// Finding 9: a recover/carve job keeps the case's review phase (marks
/// queue during it, so a shell reloaded mid-job must be able to open the
/// review stage); the job itself is reported via job/job_kind, and
/// finalize is still refused while it holds busy.
#[test]
fn case_job_keeps_review_phase_and_still_blocks_finalize() {
    let case = temp_case("job-phase");
    let dir = case.join("db/filesystem");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tsk-inspection-1.json"),
        br#"{"image_path":"/nonexistent/evidence.raw","partition_offset":2048}"#,
    )
    .unwrap();
    let exe = fake_cli(
        &case,
        r#"case "$1" in
  recover-batch) sleep 2; echo ok ;;
  *) echo ok ;;
esac"#,
        &format!(
            "if \"%1\"==\"recover-batch\" (\n{}\n)\necho ok",
            cmd_sleep(2)
        ),
    );
    let state = bound(&case, "review-ready");
    let started = parse(&recover_deleted_with_exe(&state, &exe));
    assert_eq!(started["ok"], true, "{started}");
    let status = parse(&api_status(&state));
    assert_eq!(status["busy"], true, "{status}");
    assert_eq!(status["job_kind"], "recover-deleted", "{status}");
    assert_eq!(
        status["phase"], "review-ready",
        "the review stage must stay open during a case job: {status}"
    );
    let refused = api_finalize(
        &request("POST", "/api/finalize", "", r#"{"examiner":"홍길동"}"#),
        &state,
    );
    assert!(refused.contains("\"ok\":false"), "{refused}");
    wait_for("recover done", || {
        parse(&api_status(&state))["job"]["state"] == "done"
    });
    assert_eq!(parse(&api_status(&state))["phase"], "review-ready");
    let _ = std::fs::remove_dir_all(case);
}

/// NICE: opening another case while a clip export runs is refused — the
/// export would otherwise block the new case's finalize.
#[test]
fn open_case_refused_while_clip_exports_run() {
    let old = scratch("open-old");
    let new = scratch("open-new");
    std::fs::write(new.join("case.json"), br#"{"case_id":"FT-new"}"#).unwrap();
    let state = bound(&old, "review-ready");
    state_lock(&state)
        .clip_jobs
        .push(running_clip("clip-1", "k", old.join("c.mp4")));
    let body = serde_json::json!({"case_dir": new.to_string_lossy()}).to_string();
    let response = parse(&api_open_case(
        &request("POST", "/api/open-case", "", &body),
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
    assert_eq!(state_lock(&state).case_dir.as_deref(), Some(old.as_path()));
    let _ = std::fs::remove_dir_all(old);
    let _ = std::fs::remove_dir_all(new);
}

/// NICE: finalize_with_exe must not release `busy` while the caller's
/// BusyGuard is still alive (another job could start in that window).
#[test]
fn finalize_leaves_busy_to_the_callers_guard() {
    let case = temp_case("finalize-busy");
    let exe = fake_cli(&case, "exit 1", "exit /b 1");
    let state = bound(&case, "review-ready");
    let guard = try_acquire_busy(&state).unwrap();
    let response = finalize_with_exe(&exe, &case, "홍길동", &state);
    assert!(response.contains("\"ok\":false"), "{response}");
    assert!(
        state_lock(&state).busy,
        "busy must stay held until the caller's guard drops"
    );
    drop(guard);
    assert!(!state_lock(&state).busy);
    let _ = std::fs::remove_dir_all(case);
}

/// NICE: a cancel between `qa anomalies` and `make-custody` is reported as
/// cancelled, not as a completed verification.
#[test]
fn verify_sources_cancelled_before_custody_is_not_ok() {
    let (root, case) = case_fixture("verify-cancel");
    // A completed hash-verify run so the summary exists.
    crate::anomaly::scan_case(&case, true).unwrap();
    let marker = root.join("custody-started");
    let exe = fake_cli(
        &root,
        &format!(
            r#"case "$1" in
  qa) exit 0 ;;
  make-custody) touch "{}"; sleep 10 ;;
  *) exit 0 ;;
esac"#,
            marker.display()
        ),
        &format!(
            "if \"%1\"==\"make-custody\" (\necho x> \"{}\"\ngoto spin\n)\nexit /b 0\n{}",
            marker.display(),
            CMD_SPIN
        ),
    );
    let state = bound(&case, "review-ready");
    let worker_state = Arc::clone(&state);
    let worker_exe = exe.clone();
    let worker =
        thread::spawn(move || super::jobs::verify_sources_with_exe(&worker_state, &worker_exe));
    wait_for("make-custody started", || marker.exists());
    assert_eq!(parse(&api_cancel(&state))["cancelled"], "job");
    let response = parse(&worker.join().unwrap());
    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(response["cancelled"], true, "{response}");
    let _ = std::fs::remove_dir_all(root);
}
