//! R17 track I tests: P27 "certain is certain, unknown is neutral" (I1,
//! I2, I3) and the rest of the R17 trust rows. See
//! `docs/PLAN_R17_2026-10-18.md` "§I17 as implemented" for which were run
//! on the R16 code (9bac14c).

use super::append_chained_jsonl_keyed;
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use std::fs;
use std::path::{Path, PathBuf};

fn r17_case(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r17-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("evidence/logs")).unwrap();
    fs::create_dir_all(dir.join("db")).unwrap();
    fs::write(
        dir.join("case.json"),
        format!("{{\"case_id\":\"C-{name}\"}}"),
    )
    .unwrap();
    dir
}

fn key(seed: u8) -> AuditKey {
    AuditKey::new(&fingerprint_id(&[seed; 32]), &[seed; 32])
}

/// A case with one indexed source file whose recorded hash is its bytes'.
fn indexed_case(name: &str) -> (PathBuf, PathBuf) {
    let dir = r17_case(name);
    let source = dir.join("media/a.mp4");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, b"r17-source-bytes").unwrap();
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

/// Deletes the last `count` lines of `lane` — no key, no forging.
fn delete_last_lines(lane: &Path, count: usize) {
    let mut text = fs::read_to_string(lane).unwrap();
    for _ in 0..count {
        let body = text.strip_suffix('\n').unwrap();
        let keep = body.rfind('\n').map_or(0, |at| at + 1);
        text.truncate(keep);
    }
    fs::write(lane, &text).unwrap();
}

fn lane_len(lane: &Path) -> usize {
    super::read_lane_rows(lane).unwrap().rows.len()
}

/// Strips the HMAC fields from line `index` (0-based) — the chain stays
/// structurally intact (`entry_sha256` does not cover them).
fn strip_hmac(lane: &Path, index: usize) {
    let text = fs::read_to_string(lane).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let cut = lines[index].find(",\"entry_hmac_sha256\"").unwrap();
    lines[index] = format!("{}}}", &lines[index][..cut]);
    fs::write(lane, format!("{}\n", lines.join("\n"))).unwrap();
}

/// I1 (Medium, the R16 reviewer's PoC-1): a pure R16 case, one keyed scan,
/// then only the HMAC of the anomaly lane's last line (the scan's
/// completion marker) stripped — no key needed. No usable marker remains,
/// so the result run has no `scan_id`, and R16 reported `mixed-version`
/// ("이전 버전 … 다시 스캔하면 해소"): tampering shown as a version mix.
/// P27: `mixed-version` only for a USABLE marker without `scan_id`;
/// otherwise `marker-unusable`, worded as possible tampering.
#[test]
fn r17_i1_stripped_marker_hmac_is_marker_unusable_not_mixed_version() {
    let (dir, _source) = indexed_case("i1");
    let mine = key(0xb1);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        strip_hmac(&anomaly, lane_len(&anomaly) - 1);
        let check = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(check.status(), "unknown", "{check:?}");
        assert_eq!(check.reason_code(), Some("marker-unusable"), "{check:?}");
        let reason = check
            .withhold_reason_in(crate::report::HeadLane::Anomaly)
            .unwrap();
        assert!(
            !reason.contains("이전 버전") && reason.contains("변조"),
            "{reason}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I2 (Medium, the R16 reviewer's PoC-2): two re-hash scans; the second
/// scan's rows + markers cut from BOTH result lanes (truncation is
/// certain against the second scan's head), then one chain-breaking line
/// appended to the sign-off lane. `lane_broken_at` made every head
/// unusable, so R16 returned `unknown` / `own-head-missing` (a warning)
/// instead of `truncated` (an error). P27: anchor candidates are the
/// head rows whose own trust is usable before the break.
#[test]
fn r17_i2_chain_break_after_the_heads_still_reports_truncated() {
    use std::io::Write;
    let (dir, source) = indexed_case("i2");
    let mine = key(0xb2);
    let hv_lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        let (hv_first, anomaly_first) = (lane_len(&hv_lane), lane_len(&anomaly));
        fs::write(&source, b"r17-source-bytes-TAMPERED").unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
        delete_last_lines(&hv_lane, lane_len(&hv_lane) - hv_first);
        delete_last_lines(&anomaly, lane_len(&anomaly) - anomaly_first);
        // One line that breaks the sign-off chain (no key, no forging).
        fs::OpenOptions::new()
            .append(true)
            .open(&signoff)
            .unwrap()
            .write_all(
                b"{\"event\":\"x-note\",\"prev_entry_sha256\":\"00\",\"entry_sha256\":\"00\"}\n",
            )
            .unwrap();
        assert!(super::read_lane_rows(&signoff).unwrap().failure.is_some());
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert_eq!(summary.head_status, "truncated", "{summary:?}");
        let check = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(check.status(), "truncated", "{check:?}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I1 control: a USABLE marker without `scan_id` (an older build's run)
/// while a v2 head anchors the lane stays `mixed-version` with the
/// rescan wording; the next scan clears it.
#[test]
fn r17_i1_usable_marker_without_scan_id_stays_mixed_version() {
    let (dir, _source) = indexed_case("i1-control");
    let mine = key(0xb3);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, false).unwrap();
        let marker = serde_json::json!({"schema_version":1,"event":"anomaly-scan-run",
            "scanned_unix":4_000_000_000u64,"hash_mode":"stored","finding_count":0});
        append_chained_jsonl_keyed(&anomaly, &marker.to_string(), Some(&mine)).unwrap();
        let check = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(check.reason_code(), Some("mixed-version"), "{check:?}");
        let reason = check
            .withhold_reason_in(crate::report::HeadLane::Anomaly)
            .unwrap();
        assert!(
            reason.contains(crate::report::HEAD_MIXED_VERSION_NOTE)
                && reason.contains(crate::report::HEAD_RESCAN_NOTE),
            "{reason}"
        );
        // A new scan here clears it.
        crate::anomaly::scan_case(&dir, false).unwrap();
        assert!(matches!(
            crate::report::read_anomaly_head_check(&dir),
            crate::report::HashVerifyHeadCheck::Intact { .. }
        ));
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I3 (PoC-3): an unauthenticated sign-off line that, by its own shape,
/// names another event is worded neutrally — no "not tampering", no
/// "structurally different" claim resting on its content.
#[test]
fn r17_i3_other_event_wording_rests_on_nothing_unauthenticated() {
    let (dir, _source) = indexed_case("i3");
    let mine = key(0xb4);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        append_chained_jsonl_keyed(&signoff, r#"{"event":"signoff","examiner":"x"}"#, None)
            .unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert_eq!(summary.head_reason, Some("unusable-line"), "{summary:?}");
        let reason = summary.withheld_reason.unwrap();
        assert!(
            reason.contains("모양상 다른 이벤트이나 내용은 인증 안 됨")
                && !reason.contains("변조 판정 아님")
                && !reason.contains("구조적으로"),
            "{reason}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I4: a lane torn AFTER the first lane check (while the scan hashed) still
/// refuses the scan before any result row — the lanes are checked again
/// right before the first result write. R16 checked once, at the start.
#[test]
fn r17_i4_lane_torn_during_the_scan_refuses_before_any_result_row() {
    use std::io::Write;
    let (dir, _source) = indexed_case("i4");
    let mine = key(0xb5);
    let hv_lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        let before = (fs::read(&hv_lane).unwrap(), fs::read(&anomaly).unwrap());
        crate::anomaly::BEFORE_RESULT_WRITE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(|case: &Path| {
                fs::OpenOptions::new()
                    .append(true)
                    .open(case.join(crate::report::SIGNOFF_LOG_REL_PATH))
                    .unwrap()
                    .write_all(b"{\"event\":\"torn")
                    .unwrap();
            }));
        });
        let result = crate::anomaly::scan_case(&dir, true);
        crate::anomaly::BEFORE_RESULT_WRITE.with(|hook| *hook.borrow_mut() = None);
        let err = result.unwrap_err();
        assert!(err.contains("시작하지 않았습니다"), "{err}");
        assert!(err.contains(crate::report::SIGNOFF_LOG_REL_PATH), "{err}");
        assert_eq!(fs::read(&hv_lane).unwrap(), before.0);
        assert_eq!(fs::read(&anomaly).unwrap(), before.1);
        assert!(fs::read(&signoff).unwrap().ends_with(b"torn"));
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I4: `check_appendable` opens the lane for append — a read-only lane
/// file is refused up front (R16 opened it read-only and passed). The
/// assertion is skipped where the permission does not bite (root).
#[test]
fn r17_i4_read_only_lane_is_not_appendable() {
    let dir = r17_case("i4-ro");
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"event":"x"}"#, None).unwrap();
    assert!(super::check_appendable(&lane).is_ok());
    let set_readonly = |readonly: bool| {
        let mut perms = fs::metadata(&lane).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(readonly);
        fs::set_permissions(&lane, perms).unwrap();
    };
    set_readonly(true);
    let bites = fs::OpenOptions::new().append(true).open(&lane).is_err();
    let result = super::check_appendable(&lane);
    set_readonly(false);
    if bites {
        assert!(result.is_err(), "{result:?}");
    }
    // A missing lane is still fine (the append creates it).
    assert!(super::check_appendable(&dir.join("evidence/logs/none.jsonl")).is_ok());
    let _ = fs::remove_dir_all(&dir);
}

/// I5 (P26): the re-verification result run has no head of its own (an
/// older build's run without `head_in`, after an R15 scan) while a v2
/// head names another run — `scan-mismatch`, withheld (R16: `intact`, the
/// older run shown as the current result); the next scan clears it.
#[test]
fn r17_i5_headless_result_run_after_another_runs_v2_head_is_scan_mismatch() {
    let (dir, source) = indexed_case("i5");
    let mine = key(0xb6);
    let hv_lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        let sha = super::digest_file(&source).unwrap();
        let run = "4000000000-01d0";
        let row = serde_json::json!({"schema_version":1,"event":"hash-verify","run_unix":4_000_000_000u64,
            "run_id":run,"id":"vid_a","source_path":source.to_string_lossy(),
            "recorded_sha256":sha,"observed_sha256":sha,"status":"match"});
        let done = serde_json::json!({"schema_version":1,"event":"hash-verify-run",
            "run_unix":4_000_000_000u64,"run_id":run,"records":1});
        append_chained_jsonl_keyed(&hv_lane, &row.to_string(), Some(&mine)).unwrap();
        append_chained_jsonl_keyed(&hv_lane, &done.to_string(), Some(&mine)).unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert_eq!(summary.head_reason, Some("scan-mismatch"), "{summary:?}");
        let reason = summary.withheld_reason.unwrap();
        assert!(
            reason.contains(crate::report::HEAD_SCAN_MISMATCH_NOTE) && reason.contains(run),
            "{reason}"
        );
        crate::anomaly::scan_case(&dir, true).unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(summary.is_result, "{summary:?}");
        assert_eq!(summary.head_status, "intact", "{summary:?}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I7: on a PC without audit keys an audit-lane entry is usable by
/// self-consistency only: its time stays the lane's (`time_unaudited:
/// false`) and says so (`time_self_consistent: true`); with the key the
/// same entry is authenticated (`false`); a time outside every chain is
/// never "self-consistent".
#[test]
fn r17_i7_self_consistent_event_times_are_flagged() {
    let dir = r17_case("i7");
    let mine = key(0xb7);
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    append_chained_jsonl_keyed(
        &lane,
        r#"{"event":"validate","target_path":"/e/keyed.mp4","validated_unix":100}"#,
        Some(&mine),
    )
    .unwrap();
    fs::write(
        dir.join("evidence/logs/notes-unchained.jsonl"),
        "{\"event\":\"note\",\"target_path\":\"/e/plain.mp4\",\"noted_unix\":300}\n",
    )
    .unwrap();
    let output = dir.join("db/timeline.jsonl");
    let flags = |keys: &[AuditKey]| {
        with_test_keys(keys, || {
            crate::timeline::generate_timeline(&dir, &output).unwrap();
        });
        let events: Vec<serde_json::Value> = fs::read_to_string(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect();
        assert!(
            events
                .iter()
                .all(|event| event["time_self_consistent"].is_boolean()),
            "{events:?}"
        );
        let of = |path: &str| {
            events
                .iter()
                .find(|event| event["path"] == path)
                .map(|event| {
                    (
                        event["kind"].as_str().unwrap().to_string(),
                        event["time_unaudited"].as_bool().unwrap(),
                        event["time_self_consistent"].as_bool().unwrap(),
                    )
                })
                .unwrap_or_else(|| panic!("{path}: {events:?}"))
        };
        (of("/e/keyed.mp4"), of("/e/plain.mp4"))
    };
    let (keyless, plain) = flags(&[]);
    assert_eq!(keyless, ("audit-event".to_string(), false, true));
    assert!(plain.1 && !plain.2, "{plain:?}");
    let (keyed, _) = flags(std::slice::from_ref(&mine));
    assert_eq!(keyed, ("audit-event".to_string(), false, false));
    let _ = fs::remove_dir_all(&dir);
}
