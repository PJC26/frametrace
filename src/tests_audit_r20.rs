//! R20 track I tests (docs/PLAN_R20_2026-10-19.md "§I20 as implemented"
//! says which were run on the R19 code, 1593a61).

use super::append_chained_jsonl_keyed;
use super::tests_r18::{
    anomaly_of, assert_report_agrees, indexed_case, key, r18_case, report_anomaly_section,
};
use crate::audit_key::with_test_keys;
use std::fs;
use std::path::Path;

fn finding(scanned_unix: u64, scan_id: Option<&str>, selector: &str) -> String {
    let mut row = serde_json::json!({"schema_version":1,"event":"anomaly-scan",
        "scanned_unix":scanned_unix,"kind":"hash-mismatch","selector":selector,
        "detail":format!("finding {selector}")});
    if let Some(id) = scan_id {
        row["scan_id"] = serde_json::json!(id);
    }
    row.to_string()
}

fn marker(scanned_unix: u64, scan_id: &str) -> String {
    serde_json::json!({"schema_version":1,"event":"anomaly-scan-run","scanned_unix":scanned_unix,
        "scan_id":scan_id,"hash_mode":"index"})
    .to_string()
}

fn write_lane(dir: &Path, lines: &[String]) {
    let lane = anomaly_of(dir);
    let _ = fs::remove_file(&lane);
    for line in lines {
        append_chained_jsonl_keyed(&lane, line, None).unwrap();
    }
}

fn verdict_codes(dir: &Path, label: &str) -> Vec<&'static str> {
    assert_report_agrees(dir, label)
        .map(|v| v.codes)
        .unwrap_or_default()
}

/// The report's anomaly section with no server verdict (its own rules).
fn current_section(dir: &Path) -> String {
    report_anomaly_section(dir, r#"{"anomaly":{}}"#)
}

/// The "현재 결과" part of the section (up to the history).
fn current_part(section: &str) -> &str {
    section.split("이전 실행").next().unwrap()
}

/// I2: `scanned_unix` has second resolution — two runs in ONE second
/// (interleaved) grouped by time put both runs' findings into whichever
/// marker came first. A finding row now carries its scan's `scan_id` and
/// belongs to the marker with that id only; server and report agree, and
/// rows without `scan_id` (older scanners) keep the time rule.
#[test]
fn r20_i2_same_second_runs_group_by_scan_id() {
    let dir = r18_case("r20-i2");
    let (a, b, c) = ("100-aaaa", "100-bbbb", "100-cccc");
    with_test_keys(&[], || {
        // A's finding, B's finding, A's marker, B's marker — one second.
        // R19: both findings in A's run, B (current) read "후보 0건".
        write_lane(
            &dir,
            &[
                finding(100, Some(a), "vid_a"),
                finding(100, Some(b), "vid_b"),
                marker(100, a),
                marker(100, b),
            ],
        );
        assert!(verdict_codes(&dir, "same-second").is_empty());
        let section = current_section(&dir);
        let current = current_part(&section);
        assert!(
            current.contains("현재 결과 — ")
                && current.contains("후보 1건")
                && current.contains("vid_b")
                && !current.contains("vid_a"),
            "B's finding only in B's (current) run: {section}"
        );

        // Reversed completion: B's marker first, then A's — A is current
        // and holds vid_a only (R19: vid_a and vid_b in B, A "후보 0건").
        write_lane(
            &dir,
            &[
                finding(100, Some(a), "vid_a"),
                finding(100, Some(b), "vid_b"),
                marker(100, b),
                marker(100, a),
            ],
        );
        assert!(verdict_codes(&dir, "same-second-reversed").is_empty());
        let section = current_section(&dir);
        let current = current_part(&section);
        assert!(
            current.contains("후보 1건") && current.contains("vid_a") && !current.contains("vid_b"),
            "{section}"
        );

        // A crashed run C in the same second, before A's marker: its
        // finding is not A's — a newer unmarked run, withheld by both
        // (R19: vid_c shown as A's current candidate, nothing withheld).
        write_lane(
            &dir,
            &[
                finding(100, Some(a), "vid_a"),
                finding(100, Some(c), "vid_c"),
                marker(100, a),
            ],
        );
        assert_eq!(
            verdict_codes(&dir, "same-second-crashed"),
            vec!["newer-unmarked-run"]
        );
        let section = current_section(&dir);
        assert!(
            section.contains("현재 결과 없음(결과 아님)") && section.contains("vid_c"),
            "{section}"
        );

        // Two flushed rows of B around A's: both join B, in lane order.
        write_lane(
            &dir,
            &[
                finding(100, Some(b), "vid_b"),
                finding(100, Some(a), "vid_a"),
                finding(100, Some(b), "vid_d"),
                marker(100, a),
                marker(100, b),
            ],
        );
        assert!(verdict_codes(&dir, "same-second-two").is_empty());
        let section = current_section(&dir);
        let current = current_part(&section);
        assert!(
            current.contains("후보 2건")
                && !current.contains("vid_a")
                && current.find("vid_b").unwrap() < current.find("vid_d").unwrap(),
            "{section}"
        );

        // Legacy fallback: rows without `scan_id` (an older scanner) still
        // group by `scanned_unix` — also next to markers that have one.
        write_lane(
            &dir,
            &[
                finding(100, None, "vid_old"),
                marker(100, a),
                finding(200, None, "vid_x"),
                finding(300, None, "vid_y"),
                marker(200, b),
            ],
        );
        assert_eq!(
            verdict_codes(&dir, "legacy-mixed"),
            vec!["newer-unmarked-run"]
        );
        write_lane(
            &dir,
            &[
                finding(200, None, "vid_b"),
                finding(100, None, "vid_a"),
                marker(100, a),
                marker(200, b),
            ],
        );
        assert!(verdict_codes(&dir, "legacy-interleaved").is_empty());
        let section = current_section(&dir);
        assert!(
            current_part(&section).contains("후보 1건") && current_part(&section).contains("vid_b"),
            "{section}"
        );

        // A `scan_id` no marker has, in a marked second: never moved into
        // that marker's run by time (it is its own unmarked run).
        write_lane(
            &dir,
            &[
                marker(90, "90-0000"),
                finding(100, Some(c), "vid_c"),
                finding(100, Some(a), "vid_a"),
                marker(100, a),
            ],
        );
        // C started before A's first row: an older crashed run, not newer.
        assert!(verdict_codes(&dir, "orphan-id").is_empty());
        let section = current_section(&dir);
        let current = current_part(&section);
        assert!(
            current.contains("후보 1건") && current.contains("vid_a") && !current.contains("vid_c"),
            "{section}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I2: `scan_case` writes its `scan_id` on every finding row (and the
/// "none" row), the marker's; the rows still verify (the field is inside
/// the HMAC'd bytes — older readers parse it as an unknown field).
#[test]
fn r20_i2_scan_writes_scan_id_on_every_finding_row() {
    let (dir, source) = indexed_case("r20-i2-scan");
    let mine = key(0x2e);
    with_test_keys(std::slice::from_ref(&mine), || {
        fs::write(&source, b"r18-source-bytes-TAMPERED").unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
        crate::anomaly::scan_case(&dir, false).unwrap();
        let rows = super::read_lane_rows(&anomaly_of(&dir)).unwrap();
        assert!(rows.failure.is_none(), "{:?}", rows.failure);
        let mut current: Vec<&serde_json::Value> = Vec::new();
        let mut runs = 0;
        for row in &rows.rows {
            assert_eq!(row.code(), "auth", "{}", row.raw);
            match row.value["event"].as_str() {
                Some("anomaly-scan") => current.push(&row.value),
                Some("anomaly-scan-run") => {
                    let id = row.value["scan_id"].as_str().unwrap();
                    assert!(!current.is_empty());
                    for finding in current.drain(..) {
                        assert_eq!(finding["scan_id"].as_str(), Some(id), "{finding}");
                    }
                    runs += 1;
                }
                _ => {}
            }
        }
        assert_eq!(runs, 2);
        assert!(assert_report_agrees(&dir, "scanned").is_none());
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I1: the anomaly head is computed from the caller's read of the lane —
/// a lane cut AFTER that read does not change the verdict on those rows
/// (R19 re-read it twice: rows, `withheld` and head from three lanes);
/// a scan finishing between that read and the sign-off read is
/// `lane-changed` (unknown), never "꼬리 잘림"; a real cut still is.
#[test]
fn r20_i1_head_comes_from_the_callers_read() {
    let (dir, _source) = indexed_case("r20-i1");
    let mine = key(0x2f);
    let lane = anomaly_of(&dir);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, false).unwrap();
        let read = super::read_lane_rows(&lane);
        let full = fs::read_to_string(&lane).unwrap();
        // Cut the last line after the read: the verdict on the rows read
        // is still intact; a fresh read is truncated.
        let lines: Vec<&str> = full.lines().collect();
        fs::write(&lane, format!("{}\n", lines[..lines.len() - 1].join("\n"))).unwrap();
        let on_read = crate::report::anomaly_head_check(&dir, &read);
        assert_eq!(on_read.status(), "intact", "{on_read:?}");
        let fresh = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(fresh.status(), "truncated", "{fresh:?}");
        // A real cut read by the caller is confirmed: still truncated.
        let cut_read = super::read_lane_rows(&lane);
        let cut = crate::report::anomaly_head_check(&dir, &cut_read);
        assert_eq!(cut.status(), "truncated", "{cut:?}");

        // A scan finishing between the caller's read and the sign-off
        // read: the rows are shorter than the newest head, the lane is not.
        fs::write(&lane, &full).unwrap();
        let before = super::read_lane_rows(&lane);
        crate::anomaly::scan_case(&dir, false).unwrap();
        let raced = crate::report::anomaly_head_check(&dir, &before);
        assert_eq!(raced.status(), "unknown", "{raced:?}");
        assert_eq!(raced.reason_code(), Some("lane-changed"), "{raced:?}");
        let reason = raced
            .withhold_reason_in(crate::report::HeadLane::Anomaly)
            .unwrap();
        assert!(
            reason.contains(crate::report::HEAD_LANE_CHANGED_NOTE)
                && !reason.contains(crate::report::HASH_VERIFY_HEAD_TRUNCATED),
            "{reason}"
        );
        assert_eq!(
            crate::report::read_anomaly_head_check(&dir).status(),
            "intact"
        );

        // An unreadable read is `unreadable` without reading again.
        let err: Result<super::LaneRows, String> = Err("boom".to_string());
        let unreadable = crate::report::anomaly_head_check(&dir, &err);
        assert_eq!(unreadable.reason_code(), Some("unreadable"));
        assert!(
            unreadable
                .withhold_reason_in(crate::report::HeadLane::Anomaly)
                .unwrap()
                .contains("boom")
        );
    });
    let _ = fs::remove_dir_all(&dir);
}
