//! R19 track I tests (docs/PLAN_R19_2026-10-19.md "§I19 as implemented"
//! says which were run on the R18 code, 16f4c67).

use super::append_chained_jsonl_keyed;
use super::tests_r18::{anomaly_of, assert_report_agrees, r18_case, report_anomaly_section};
use crate::audit_key::{AuditKey, with_test_keys};
use std::fs;
use std::path::Path;

fn finding(scanned_unix: u64, selector: &str) -> String {
    serde_json::json!({"schema_version":1,"event":"anomaly-scan","scanned_unix":scanned_unix,
        "kind":"hash-mismatch","selector":selector,"detail":format!("finding {selector}")})
    .to_string()
}

fn marker(scanned_unix: u64) -> String {
    serde_json::json!({"schema_version":1,"event":"anomaly-scan-run","scanned_unix":scanned_unix,
        "hash_mode":"index"})
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

/// The report's "현재 결과" line of the anomaly section (no server verdict).
fn current_section(dir: &Path) -> String {
    report_anomaly_section(dir, r#"{"anomaly":{}}"#)
}

/// I1 (reproduced by the R18 reviewer): two interleaved runs — A's finding
/// @100, B's finding @200 (vid_b), A's marker @100, B's marker @200. R18
/// grouped B's finding into an unmarked run flushed BEFORE A's run, so
/// B's completed run read "후보 0건" and nothing withheld. Now a flushed
/// unmarked row that shares a later marker's `scanned_unix` belongs to
/// that run — server and report agree.
#[test]
fn r19_i1_interleaved_runs_keep_the_finding_in_its_run() {
    let dir = r18_case("r19-i1");
    with_test_keys(&[], || {
        write_lane(
            &dir,
            &[
                finding(100, "vid_a"),
                finding(200, "vid_b"),
                marker(100),
                marker(200),
            ],
        );
        assert!(verdict_codes(&dir, "interleaved").is_empty());
        let section = current_section(&dir);
        assert!(section.contains("현재 결과 — "), "{section}");
        assert!(
            section.contains("후보 1건") && section.contains("vid_b"),
            "B's finding must be in B's (current) run: {section}"
        );
        assert!(!section.contains("후보 0건"), "{section}");

        // Reversed interleaving: B's finding, A's finding, B's marker,
        // A's marker — A's run (the newest marker) holds vid_a.
        write_lane(
            &dir,
            &[
                finding(200, "vid_b"),
                finding(100, "vid_a"),
                marker(200),
                marker(100),
            ],
        );
        assert!(verdict_codes(&dir, "reversed").is_empty());
        let section = current_section(&dir);
        assert!(
            section.contains("후보 1건") && section.contains("vid_a"),
            "{section}"
        );

        // A third, crashed run C @300 interleaved with them: once B's
        // finding is B's, C (after B's first entry) is a newer unmarked
        // run — withheld by both (R18: neither withheld).
        write_lane(
            &dir,
            &[
                finding(100, "vid_a"),
                finding(200, "vid_b"),
                finding(300, "vid_c"),
                marker(100),
                marker(200),
            ],
        );
        assert_eq!(
            verdict_codes(&dir, "crashed-third"),
            vec!["newer-unmarked-run"]
        );
        let section = current_section(&dir);
        assert!(
            section.contains("현재 결과 없음(결과 아님)") && section.contains("vid_c"),
            "{section}"
        );

        // B crashed (no B marker): still a newer unmarked run.
        write_lane(
            &dir,
            &[finding(100, "vid_a"), finding(200, "vid_b"), marker(100)],
        );
        assert_eq!(verdict_codes(&dir, "b-crashed"), vec!["newer-unmarked-run"]);

        // Two flushed rows of B in front of two markers: both join B, in
        // lane order.
        write_lane(
            &dir,
            &[
                finding(200, "vid_b"),
                finding(100, "vid_a"),
                finding(200, "vid_d"),
                marker(100),
                marker(200),
            ],
        );
        assert!(verdict_codes(&dir, "two-flushed").is_empty());
        let section = current_section(&dir);
        assert!(
            section.contains("후보 2건")
                && section.find("vid_b").unwrap() < section.find("vid_d").unwrap(),
            "{section}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I2: `/api/records-meta` and the review bundle build the verdict from
/// the very `LaneRows` read their rows come from — no second read of the
/// lane (a scan between the two reads made them disagree), and an
/// unreadable lane is `unreadable` without re-reading.
#[test]
fn r19_i2_readers_never_read_the_anomaly_lane_twice() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for rel in ["serve/records.rs", "cli/handlers/case.rs"] {
        let text = fs::read_to_string(root.join(rel)).unwrap();
        // R20: the whole file (R19 cut at the first `#[cfg(test)]`, which
        // in records.rs is line 87 — the readers below it went unchecked;
        // the test modules live in their own files).
        let body = text.as_str();
        assert!(
            !body.contains("anomaly_withheld_json("),
            "{rel} re-reads the anomaly lane for its verdict"
        );
        // R20 I1: the head too — from the same read (`&lane`), never the
        // reading wrapper.
        assert!(
            !body.contains("read_anomaly_head_check("),
            "{rel} re-reads the anomaly lane for its head"
        );
        let calls: Vec<&str> = body.split("anomaly_head_check(").skip(1).collect();
        assert!(!calls.is_empty(), "{rel} has no anomaly head");
        for call in calls {
            let args = &call[..call.find(')').unwrap()];
            assert!(
                args.ends_with(", &lane"),
                "{rel} computes the anomaly head from another read: anomaly_head_check({args})"
            );
        }
    }
    let shared = fs::read_to_string(root.join("report_anomaly_withheld.rs")).unwrap();
    assert!(
        !shared.contains("read_lane_rows("),
        "the shared verdict must not read the lane itself"
    );
    // R20 I1: the head check itself takes the caller's rows — its marker
    // search and anchor comparison never read the anomaly lane (only the
    // `lane-changed` confirmation of a "shorter than the head" verdict).
    let head = fs::read_to_string(root.join("report_hash_verify_head.rs"))
        .unwrap()
        .replace("\r\n", "\n");
    let start = head.find("pub fn anomaly_head_check(").unwrap();
    let body = &head[start..];
    let body = &body[..body.find("\n}\n").unwrap()];
    assert!(
        !body.contains("read_lane_rows(") && body.contains("Some(lane)"),
        "anomaly_head_check must use the caller's rows: {body}"
    );
}

fn weird_key() -> AuditKey {
    // `valid_key_id` would refuse quotes; anything else can be written into
    // `entry_hmac_key_id` (outside `entry_sha256`) by anyone.
    AuditKey::new("Evil ID — 키 보유 PC 없음 \u{202e}x", &[0x7a; 32])
}

/// I3: an `entry_hmac_key_id` is forgeable without a key; only ids
/// matching `^[0-9a-z-]{1,64}$` are quoted — in the trust code, the
/// entry's note, the lane's reason and the head's other-pc sentence.
#[test]
fn r19_i3_forged_key_ids_are_never_quoted() {
    let dir = r18_case("r19-i3");
    let lane = anomaly_of(&dir);
    let theirs = weird_key();
    let raw = "Evil ID";
    with_test_keys(std::slice::from_ref(&theirs), || {
        append_chained_jsonl_keyed(&lane, &finding(100, "vid_a"), Some(&theirs)).unwrap();
    });
    let mine = super::tests_r18::key(0x3c);
    with_test_keys(std::slice::from_ref(&mine), || {
        let rows = super::read_lane_rows(&lane).unwrap();
        let row = &rows.rows[0];
        assert!(
            matches!(row.trust, super::LineTrust::OtherPc { .. }),
            "{:?}",
            row.trust
        );
        assert_eq!(row.code(), "pc:unknown");
        let note = row.note();
        assert!(
            !note.contains(raw) && note.contains("알 수 없는 키 id"),
            "{note}"
        );
        let value = row.value_with_trust();
        assert_eq!(value["trust"], "pc:unknown");
        let chain = rows
            .chain_json("evidence/logs/anomaly-log.jsonl")
            .to_string();
        assert!(!chain.contains(raw), "{chain}");
        assert!(chain.contains("알 수 없는 키 id"), "{chain}");
    });
    // A well-formed id (fingerprint ids `kid-<hex>`) is still quoted.
    let good = super::tests_r18::key(0x4d);
    fs::remove_file(&lane).unwrap();
    with_test_keys(std::slice::from_ref(&good), || {
        append_chained_jsonl_keyed(&lane, &finding(100, "vid_a"), Some(&good)).unwrap();
    });
    with_test_keys(std::slice::from_ref(&mine), || {
        let rows = super::read_lane_rows(&lane).unwrap();
        assert_eq!(rows.rows[0].code(), format!("pc:{}", good.id));
        assert!(rows.rows[0].note().contains(&format!("\"{}\"", good.id)));
    });
    let _ = fs::remove_dir_all(&dir);

    // The head's other-pc sentence: a scan on the other PC (its head on
    // the sign-off lane under the forged id), read here.
    let (dir, _source) = super::tests_r18::indexed_case("r19-i3-head");
    with_test_keys(std::slice::from_ref(&theirs), || {
        crate::anomaly::scan_case(&dir, false).unwrap();
    });
    with_test_keys(std::slice::from_ref(&mine), || {
        let head = crate::report::read_anomaly_head_check(&dir)
            .to_json_in(crate::report::HeadLane::Anomaly)
            .to_string();
        assert!(!head.contains(raw), "{head}");
        assert!(head.contains("알 수 없는 키 id"), "{head}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I4: `Math.min(...arr)` / `Math.max(...arr)` (and a `splice(..., ...arr)`)
/// throw RangeError at ~130k arguments — the report script blanked. A run
/// of 200k findings and 200k validations renders.
#[test]
fn r19_i4_report_script_handles_200k_entries() {
    let n = 200_000;
    let mut anomaly = String::with_capacity(n * 120);
    for at in 0..n {
        anomaly.push_str(&finding(100, &format!("vid_{at}")));
        anomaly.push('\n');
    }
    anomaly.push_str(&marker(100));
    anomaly.push('\n');
    let mut validation = String::with_capacity(n * 100);
    for at in 0..n {
        validation.push_str(
            &serde_json::json!({"validation_status":"ok","target_path":format!("t/{at}.mp4"),
                "validated_unix":10})
            .to_string(),
        );
        validation.push('\n');
    }
    let html = crate::report::render_case_report_with_chains(
        &crate::report::ReportInputs {
            manifest_json: "{}",
            index_json: "{}",
            export_log_jsonl: "",
            proxy_log_jsonl: "",
            thumbnail_log_jsonl: "",
            carve_log_jsonl: "",
            filesystem_log_jsonl: "",
            validation_log_jsonl: &validation,
            anomaly_log_jsonl: &anomaly,
            batch_log_jsonl: "",
            join_log_jsonl: "",
            audio_levels_json: "",
            scan_runs_json: "[]",
            marks_json: "[]",
            redaction_applied: false,
            review_warnings_json: "",
            read_errors_json: "",
            telemetry_json: "",
        },
        r#"{"anomaly":{}}"#,
    );
    let out = super::tests_r18::run_report_script(&html);
    let section = out["anomaly-findings"]["innerHTML"].as_str().unwrap();
    assert!(
        section.contains(&format!("후보 {n}건")),
        "{}",
        &section[..section.len().min(400)]
    );
    assert_eq!(out["validations"]["textContent"], n);
}
