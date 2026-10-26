//! R14 track I tests: P20 "a run is judged by its span, not its position"
//! (I1), the `hash-verify-head` anchor in the sign-off lane (I3), and the
//! rest of the R14 trust rows. See `docs/PLAN_R14_2026-10-16.md`
//! "§I14 as implemented" for which were run on the R13 code (40126c3).

use super::append_chained_jsonl_keyed;
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use std::fs;
use std::path::{Path, PathBuf};

fn r14_case(name: &str, case_id: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r14-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("evidence/logs")).unwrap();
    fs::create_dir_all(dir.join("db")).unwrap();
    fs::write(
        dir.join("case.json"),
        format!("{{\"case_id\":\"{case_id}\"}}"),
    )
    .unwrap();
    dir
}

fn key(seed: u8) -> AuditKey {
    AuditKey::new(&fingerprint_id(&[seed; 32]), &[seed; 32])
}

fn hv(run: i64, run_id: &str, id: &str, status: &str) -> String {
    serde_json::json!({"schema_version":1,"event":"hash-verify","run_unix":run,"run_id":run_id,
        "id":id,"source_path":format!("/e/{id}.mp4"),"recorded_sha256":"a".repeat(64),
        "observed_sha256":if status == "match" { "a".repeat(64) } else { "b".repeat(64) },
        "status":status})
    .to_string()
}

fn hv_done(run: i64, run_id: &str, records: usize) -> String {
    serde_json::json!({"schema_version":1,"event":"hash-verify-run","run_unix":run,
        "run_id":run_id,"records":records})
    .to_string()
}

fn write_lane(lane: &Path, lines: &[String], key: &AuditKey) {
    for line in lines {
        append_chained_jsonl_keyed(lane, line, Some(key)).unwrap();
    }
}

/// Deletes the last `count` lines of `lane` — no key, no forging: the
/// chain that remains verifies.
fn delete_last_lines(lane: &Path, count: usize) {
    let mut text = fs::read_to_string(lane).unwrap();
    for _ in 0..count {
        let body = text.strip_suffix('\n').unwrap();
        let keep = body.rfind('\n').map_or(0, |at| at + 1);
        text.truncate(keep);
    }
    fs::write(lane, &text).unwrap();
}

/// What a completed run appends to the sign-off lane (written by hand here
/// so the test also runs on the R13 code, which ignores it): the
/// re-verification lane's entry count and last `entry_sha256`.
fn append_head(dir: &Path, run_id: &str, key: &AuditKey) {
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let head = super::lane_head(&lane).unwrap().unwrap();
    let body = serde_json::json!({"schema_version":1,"event":"hash-verify-head",
        "run_id":run_id,"entries":head.entries,"last_entry_sha256":head.last_entry_sha256});
    append_chained_jsonl_keyed(
        &dir.join(crate::report::SIGNOFF_LOG_REL_PATH),
        &body.to_string(),
        Some(key),
    )
    .unwrap();
}

/// The custody statement's green section-5 "everything matched" banner.
const ALL_MATCH_OK: &str = "<p class=\"banner ok\">All ";

/// I1 (Medium, the R13 reviewer's PoC): two runs interleaved — A (match),
/// B (mismatch), A's marker, B's marker. Deleting B's marker (the last
/// line; no key) left B's last usable row BEFORE A's marker, so R13's
/// "positioned after the result's marker" test did not see B as newer:
/// A came back green with B's MISMATCH hidden. P20: an unfinished run
/// whose first row is after the result run's first row (or whose
/// `run_unix` is greater) is newer.
#[test]
fn r14_i1_interleaved_run_without_its_marker_withholds_the_result() {
    let dir = r14_case("i1", "C-i1");
    let mine = key(0xe1);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(100, "100-aaaa", "vid_a", "match"),
            hv(200, "200-bbbb", "vid_a", "mismatch"),
            hv_done(100, "100-aaaa", 1),
            hv_done(200, "200-bbbb", 1),
        ],
        &mine,
    );
    delete_last_lines(&lane, 1);
    with_test_keys(std::slice::from_ref(&mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(
            !summary.is_result,
            "A came back as the result with B's mismatch hidden: {summary:?}"
        );
        assert!(!summary.verified, "{summary:?}");
        assert_eq!(summary.newer_mismatched, 1, "{summary:?}");
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(!html.contains(ALL_MATCH_OK), "green banner:\n{html}");
        assert!(html.contains("MISMATCH"), "{html}");
    });
    // The same by `run_unix` alone: B's first row positioned before A's
    // first row but stamped later (a run clock is not proof, but a greater
    // run_unix is never "older"). R15 I4: only while B is not WHOLLY
    // before A's first row — B here spans it (`tests_r15` covers the
    // wholly-before crash, which no longer withholds).
    let dir2 = r14_case("i1-unix", "C-i1u");
    let lane2 = dir2.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane2,
        &[
            hv(200, "200-bbbb", "vid_a", "mismatch"),
            hv(100, "100-aaaa", "vid_a", "match"),
            hv(200, "200-bbbb", "vid_b", "mismatch"),
            hv_done(100, "100-aaaa", 1),
        ],
        &mine,
    );
    with_test_keys(std::slice::from_ref(&mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(&dir2).unwrap();
        assert!(!summary.is_result, "{summary:?}");
    });
    // An older crash (first row and run_unix both before the result) still
    // does not withhold.
    let dir3 = r14_case("i1-old", "C-i1o");
    let lane3 = dir3.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane3,
        &[
            hv(50, "50-cccc", "vid_a", "mismatch"),
            hv(100, "100-aaaa", "vid_a", "match"),
            hv_done(100, "100-aaaa", 1),
        ],
        &mine,
    );
    with_test_keys(std::slice::from_ref(&mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(&dir3).unwrap();
        assert!(summary.is_result && summary.verified, "{summary:?}");
    });
    for dir in [dir, dir2, dir3] {
        let _ = fs::remove_dir_all(&dir);
    }
}

/// I3 (Medium, the R13 reviewer's PoC): R1 (match) + marker, R2
/// (mismatch) + marker; deleting the LAST TWO lines (R2's row and marker)
/// leaves a shorter lane that verifies, and R1 was green again. The
/// `hash-verify-head` entry each completed run writes in the sign-off lane
/// records the lane's entry count and last `entry_sha256`: a lane shorter
/// than its head is "꼬리 잘림 — 결과 없음".
#[test]
fn r14_i3_two_deleted_lines_are_caught_by_the_signoff_lane_head() {
    let dir = r14_case("i3", "C-i3");
    let mine = key(0xe3);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(100, "100-aaaa", "vid_a", "match"),
            hv_done(100, "100-aaaa", 1),
        ],
        &mine,
    );
    append_head(&dir, "100-aaaa", &mine);
    write_lane(
        &lane,
        &[
            hv(200, "200-bbbb", "vid_a", "mismatch"),
            hv_done(200, "200-bbbb", 1),
        ],
        &mine,
    );
    append_head(&dir, "200-bbbb", &mine);
    delete_last_lines(&lane, 2);
    with_test_keys(std::slice::from_ref(&mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(
            !summary.is_result,
            "R1 came back green after R2's two lines were deleted: {summary:?}"
        );
        assert!(!summary.verified, "{summary:?}");
        assert!(
            summary
                .withheld_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("꼬리 잘림 — 결과 없음")),
            "{summary:?}"
        );
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(!html.contains(ALL_MATCH_OK), "green banner:\n{html}");
        assert!(html.contains("No current re-verification result"), "{html}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// A case with one indexed source file whose recorded hash is its bytes'.
fn indexed_case(name: &str) -> (PathBuf, PathBuf) {
    let dir = r14_case(name, &format!("C-{name}"));
    let source = dir.join("media/a.mp4");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, b"r14-source-bytes").unwrap();
    let sha = super::digest_file(&source).unwrap();
    fs::write(
        dir.join("db/videos.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({"id":"vid_a","source_path":source.to_string_lossy(),"sha256":sha})
        ),
    )
    .unwrap();
    (dir, source)
}

/// I3 through the real writer: every completed `scan_case --rehash` run
/// appends its `hash-verify-head` to the sign-off lane (the marker says
/// so), the head reads intact, and cutting the newest run's two lines is
/// "꼬리 잘림 — 결과 없음" in the summary, the custody statement and the
/// package-side check.
#[test]
fn r14_i3_scan_case_writes_the_head_and_a_cut_tail_is_no_result() {
    let (dir, source) = indexed_case("i3-scan");
    let mine = key(0xe4);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(summary.is_result && summary.verified, "{summary:?}");
        assert_eq!(summary.head_status, "intact", "{summary:?}");
        let signoff = fs::read_to_string(dir.join(crate::report::SIGNOFF_LOG_REL_PATH)).unwrap();
        assert_eq!(
            signoff
                .matches(crate::report::HASH_VERIFY_HEAD_EVENT)
                .count(),
            1
        );
        let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
        assert!(
            fs::read_to_string(&lane)
                .unwrap()
                .contains(crate::report::HASH_VERIFY_MARKER_HEAD_FIELD)
        );
        // R2: the source changed → MISMATCH; then its row + marker cut.
        fs::write(&source, b"r14-source-bytes-TAMPERED").unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
        delete_last_lines(&lane, 2);
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert_eq!(summary.head_status, "truncated", "{summary:?}");
        assert!(
            summary
                .withheld_reason
                .as_deref()
                .is_some_and(|r| r.contains(crate::report::HASH_VERIFY_HEAD_TRUNCATED)),
            "{summary:?}"
        );
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(!html.contains(ALL_MATCH_OK), "{html}");
        assert!(html.contains("TAIL CUT"), "{html}");
        assert!(matches!(
            crate::report::hash_verify_head_check(&dir, None, false),
            crate::report::HashVerifyHeadCheck::Truncated { entries: 4, now: 2 }
        ));
        // The next completed run anchors the lane again.
        crate::anomaly::scan_case(&dir, true).unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(summary.is_result, "{summary:?}");
        assert_eq!((summary.mismatched, summary.head_status), (1, "intact"));
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I3: the head itself cannot be quietly dropped — a result run whose
/// marker says it anchored the lane but whose head is gone (the sign-off
/// lane's tail cut), or an unusable line after the newest head (it may be
/// a newer head), is no result.
#[test]
fn r14_i3_a_missing_or_shadowed_head_is_no_result() {
    let (dir, _source) = indexed_case("i3-shadow");
    let mine = key(0xe5);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        delete_last_lines(&signoff, 1);
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert_eq!(summary.head_status, "unknown", "{summary:?}");
        crate::anomaly::scan_case(&dir, true).unwrap();
        assert!(
            crate::anomaly::latest_hash_verify_summary(&dir)
                .unwrap()
                .is_result
        );
        // An unkeyed line after the newest head: unusable here.
        append_chained_jsonl_keyed(&signoff, r#"{"event":"note"}"#, None).unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert_eq!(summary.head_status, "unknown", "{summary:?}");
    });
    let _ = fs::remove_dir_all(&dir);
}

const REPAIR_MARKER: &str = r#"{"event":"lane-repair","kept_entries":1,"removed_lines":1,"removed_bytes":10,"removed_sha256":"aa"}"#;

/// I5: the newest E01 import of an image positioned before a usable
/// `lane-repair` marker is not known to be the newest — refused (re-import),
/// never reused; an import after the marker is trusted again.
#[test]
fn r14_i5_e01_import_before_a_lane_repair_is_unknown() {
    let dir = r14_case("i5-e01", "C-i5e");
    fs::create_dir_all(dir.join("evidence/images")).unwrap();
    let e01 = dir.join("disk.E01");
    fs::write(&e01, b"ewf").unwrap();
    let raw = dir.join("evidence/images/evidence.raw");
    fs::write(&raw, b"complete-image").unwrap();
    let sha = super::digest_file(&raw).unwrap();
    let lane = dir.join("evidence/logs/e01-audit.jsonl");
    let entry = |unix: u64| {
        serde_json::json!({"schema_version": 1, "event": "import-e01",
            "imported_unix": unix, "e01_path": e01.to_string_lossy(),
            "raw_output_path": raw.to_string_lossy(), "raw_sha256": sha, "max_bytes": null})
        .to_string()
    };
    append_chained_jsonl_keyed(&lane, &entry(7), None).unwrap();
    assert!(
        crate::e01::recorded_raw_import_with_keys(&dir, &raw, &[])
            .unwrap()
            .is_some()
    );
    append_chained_jsonl_keyed(&lane, REPAIR_MARKER, None).unwrap();
    let err = crate::e01::recorded_raw_import_with_keys(&dir, &raw, &[]).unwrap_err();
    assert_eq!(err, crate::e01::E01_REPAIRED_AFTER_NOTE);
    append_chained_jsonl_keyed(&lane, &entry(9), None).unwrap();
    let record = crate::e01::recorded_raw_import_with_keys(&dir, &raw, &[])
        .unwrap()
        .unwrap();
    assert_eq!(record.imported_unix, 9);
    let _ = fs::remove_dir_all(&dir);
}

/// I5: the transcode queue summary is the lane's NEWEST line only — not an
/// older summary behind an unusable line (it may be a newer summary) or a
/// `lane-repair` marker.
#[test]
fn r14_i5_transcode_summary_is_the_newest_line_only() {
    let dir = r14_case("i5-tq", "C-i5t");
    let lane = dir.join("artifacts/logs/transcode-queue-log.jsonl");
    fs::create_dir_all(lane.parent().unwrap()).unwrap();
    let mine = key(0xe6);
    let summary = r#"{"schema_version":1,"event":"transcode-queue","unix":1,"total":1,"proxied":1,"skipped_existing":0,"failed":0}"#;
    with_test_keys(std::slice::from_ref(&mine), || {
        append_chained_jsonl_keyed(&lane, summary, Some(&mine)).unwrap();
        assert!(crate::transcode::latest_summary(&dir).is_some());
        // An unkeyed (unusable here) newer line of another kind.
        append_chained_jsonl_keyed(&lane, r#"{"event":"x-note"}"#, None).unwrap();
        assert!(crate::transcode::latest_summary(&dir).is_none());
        append_chained_jsonl_keyed(&lane, summary, Some(&mine)).unwrap();
        assert!(crate::transcode::latest_summary(&dir).is_some());
        append_chained_jsonl_keyed(&lane, REPAIR_MARKER, Some(&mine)).unwrap();
        assert!(crate::transcode::latest_summary(&dir).is_none());
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I6: the `FRAMETRACE_` prefix no longer admits the app's own setting
/// variables as key sources (adopting one removed it from the process —
/// e.g. the workstation's token); key-material names still pass.
#[test]
fn r14_i6_app_variables_are_not_key_sources() {
    use crate::audit_key::{APP_ENV_VARS, key_source_var_refusal};
    for refused in [
        "FRAMETRACE_TOKEN",
        "frametrace_token",
        "FRAMETRACE_INSTANCE_FILE",
        "FRAMETRACE_AUDIT_KEY_FILE",
        "FRAMETRACE_AUDIT_KEYRING_FILE",
        "FRAMETRACE_DEEPFAKE_LENS_HOME",
        "FRAMETRACE_EXPECTED_AUDIT_KEY",
        "FRAMETRACE_PAGES_KEY_FILE",
        "FRAMETRACE_KEY_SOURCE",
    ] {
        let why = key_source_var_refusal(refused);
        assert!(why.is_some(), "{refused}");
    }
    assert!(
        key_source_var_refusal("FRAMETRACE_TOKEN")
            .unwrap()
            .contains("FrameTrace's own setting")
    );
    for allowed in [
        "FRAMETRACE_AUDIT_KEY",
        "FRAMETRACE_AUDIT_KEY_SECRET",
        "FRAMETRACE_CASE_KEY",
        "VAULT_SECRET",
    ] {
        assert_eq!(key_source_var_refusal(allowed), None, "{allowed}");
    }
    assert!(
        APP_ENV_VARS
            .iter()
            .all(|name| name.starts_with("FRAMETRACE_"))
    );
}

/// I7: the `.removed` archive is hashed through one handle that does not
/// follow a link, and a verdict for a settled file (mtime ≥ 2 s old) is
/// cached per (path, size, mtime, sha256); a rewrite changes the key.
#[test]
fn r14_i7_removed_archive_verdicts_are_cached_by_size_and_mtime() {
    let dir = r14_case("i7", "C-i7");
    let mine = key(0xe7);
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    let entry = |n: u32| serde_json::json!({"event":"validate","n":n}).to_string();
    write_lane(&lane, &[entry(1), entry(2)], &mine);
    let text = fs::read_to_string(&lane).unwrap();
    fs::write(&lane, &text[..text.len() - 20]).unwrap();
    with_test_keys(std::slice::from_ref(&mine), || {
        let saved = super::repair_lane(&lane, false)
            .unwrap()
            .removed_saved_to
            .unwrap();
        let state = || {
            super::read_lane_rows(&lane)
                .unwrap()
                .removed_archive()
                .unwrap()
        };
        // Fresh (racy) mtime: hashed, not cached.
        assert_eq!(state(), super::RemovedArchive::Match);
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::OpenOptions::new()
            .write(true)
            .open(&saved)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(state(), super::RemovedArchive::Match);
        assert_eq!(
            super::removed_archive_cached(&saved),
            Some(super::RemovedArchive::Match)
        );
        // Rewritten (same size): a new mtime, a new key — differs.
        let mut bytes = fs::read(&saved).unwrap();
        bytes[0] ^= 0x01;
        fs::write(&saved, &bytes).unwrap();
        assert_eq!(state(), super::RemovedArchive::Differs);
        // Unix: a link in its place (to the original bytes) is not followed.
        #[cfg(unix)]
        {
            bytes[0] ^= 0x01;
            let copy = dir.join("copy.removed");
            fs::write(&copy, &bytes).unwrap();
            fs::remove_file(&saved).unwrap();
            std::os::unix::fs::symlink(&copy, &saved).unwrap();
            assert_eq!(state(), super::RemovedArchive::Differs);
        }
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I8: a carve event timed from `db/carve_results.json` (its lane entry
/// has no `carved_unix`) says so in the data, not only in the detail text:
/// `"time_unaudited": true` (every other event: false).
#[test]
fn r14_i8_carve_time_unaudited_is_a_field() {
    let dir = r14_case("i8", "C-i8");
    let mine = key(0xe8);
    fs::create_dir_all(dir.join("artifacts/carved")).unwrap();
    let lane = dir.join("artifacts/carved/carve-log.jsonl");
    write_lane(
        &lane,
        &[
            serde_json::json!({"event":"carve-file","id":"c_old","output_path":"/c/old.mp4","source_path":"/ev/i.raw","offset":1}).to_string(),
            serde_json::json!({"event":"carve-file","id":"c_new","output_path":"/c/new.mp4","source_path":"/ev/i.raw","offset":2,"carved_unix":150}).to_string(),
        ],
        &mine,
    );
    fs::write(
        dir.join("db/carve_results.json"),
        "{\"carved_unix\":160,\"artifacts\":[{\"id\":\"c_old\",\"output_path\":\"/c/old.mp4\",\"source_path\":\"/ev/i.raw\",\"offset\":1}]}",
    )
    .unwrap();
    let output = dir.join("db/timeline.jsonl");
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::timeline::generate_timeline(&dir, &output).unwrap();
    });
    let events: Vec<serde_json::Value> = fs::read_to_string(&output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect();
    let flag = |path: &str| {
        events
            .iter()
            .find(|event| event["path"] == path)
            .map(|event| event["time_unaudited"].clone())
    };
    assert_eq!(
        flag("/c/old.mp4"),
        Some(serde_json::Value::Bool(true)),
        "{events:?}"
    );
    assert_eq!(
        flag("/c/new.mp4"),
        Some(serde_json::Value::Bool(false)),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .all(|event| event["time_unaudited"].is_boolean())
    );
    let _ = fs::remove_dir_all(&dir);
}

/// P20: one scan at a time per case — a second scan while the lock is
/// held is refused (and writes nothing).
#[test]
fn r14_p20_scan_lock_refuses_a_concurrent_scan() {
    use fs2::FileExt;
    let (dir, _source) = indexed_case("lock");
    let lock_path = dir.join(crate::anomaly::SCAN_LOCK_REL_PATH);
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    held.try_lock_exclusive().unwrap();
    with_test_keys(&[], || {
        let err = crate::anomaly::scan_case(&dir, true).unwrap_err();
        assert!(err.contains("실행 중"), "{err}");
        assert!(!dir.join(crate::anomaly::HASH_VERIFY_REL_PATH).exists());
        assert!(!dir.join("evidence/logs/anomaly-log.jsonl").exists());
        FileExt::unlock(&held).unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
    });
    let _ = fs::remove_dir_all(&dir);
}
