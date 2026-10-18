//! R16 track I tests: P26 "a head is paired with its own scan" (I1, I3)
//! and the rest of the R16 trust rows. See `docs/PLAN_R16_2026-10-18.md`
//! "§I16 as implemented" for which were run on the R15 code (82a6cb1).

use super::append_chained_jsonl_keyed;
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use std::fs;
use std::path::{Path, PathBuf};

fn r16_case(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r16-{name}-{}", std::process::id()));
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
    let dir = r16_case(name);
    let source = dir.join("media/a.mp4");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, b"r16-source-bytes").unwrap();
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

fn lane_len(lane: &Path) -> usize {
    super::read_lane_rows(lane).unwrap().rows.len()
}

/// Runs the case report script under node with a stub `document`; returns
/// every element it touched.
fn run_report_script(html: &str) -> serde_json::Value {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let script = html
        .split_once("<script>")
        .unwrap()
        .1
        .split_once("</script>")
        .unwrap()
        .0;
    let mut child = Command::new("node")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("node is required for report rendering tests");
    let mut stdin = child.stdin.take().unwrap();
    write!(stdin, "const elements = new Map(); const document = {{getElementById(id) {{ if (!elements.has(id)) elements.set(id, {{}}); return elements.get(id); }}}};\n{script}\nprocess.stdout.write(JSON.stringify(Object.fromEntries(elements)));").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The report's anomaly section for the case as `make-report` builds it.
fn report_anomaly_section(dir: &Path) -> String {
    let rows = super::read_lane_rows(&dir.join("evidence/logs/anomaly-log.jsonl")).unwrap();
    let anomaly = rows.jsonl_with_trust();
    let chains = crate::cli::handlers::report_audit_chains_json(dir);
    let html = crate::report::render_case_report_with_chains(
        &crate::report::ReportInputs {
            manifest_json: "{}",
            index_json: "{}",
            export_log_jsonl: "",
            proxy_log_jsonl: "",
            thumbnail_log_jsonl: "",
            carve_log_jsonl: "",
            filesystem_log_jsonl: "",
            validation_log_jsonl: "",
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
        &chains,
    );
    run_report_script(&html)["anomaly-findings"]["innerHTML"]
        .as_str()
        .unwrap()
        .to_string()
}

/// What an R14 build's scan (stored hashes) wrote, keyed: its v1 head
/// (`hash-verify-head` without `lanes`, anchoring the re-verification lane
/// at the top level) BEFORE its anomaly rows, then one anomaly row and
/// its marker without `scan_id` / `head_in`.
fn r14_build_scan(dir: &Path, key: &AuditKey, scanned_unix: u64) {
    let hv_lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    let hv_head = super::lane_head(&hv_lane).unwrap().unwrap();
    let head = serde_json::json!({"schema_version":1,"event":"hash-verify-head",
        "run_id":null,"run_unix":scanned_unix,"lane":crate::anomaly::HASH_VERIFY_REL_PATH,
        "entries":hv_head.entries,"last_entry_sha256":hv_head.last_entry_sha256});
    append_chained_jsonl_keyed(&signoff, &head.to_string(), Some(key)).unwrap();
    let row = serde_json::json!({"schema_version":1,"event":"anomaly-scan","scanned_unix":scanned_unix,
        "label":"none","kind":"none","selector":"","source_path":"",
        "detail":"no candidate findings on indexed videos"});
    let marker = serde_json::json!({"schema_version":1,"event":"anomaly-scan-run",
        "scanned_unix":scanned_unix,"hash_mode":"stored","finding_count":0,
        "detail":"stored index hashes only; pass --rehash for full revalidation"});
    append_chained_jsonl_keyed(&anomaly, &row.to_string(), Some(key)).unwrap();
    append_chained_jsonl_keyed(&anomaly, &marker.to_string(), Some(key)).unwrap();
}

/// I1 (Medium, the R15 reviewer's PoC-A): an R15 scan (v2 head anchoring
/// both lanes), then an R14 build's scan with the same key — its v1 head
/// sits before its anomaly rows and does not anchor the anomaly lane.
/// Cutting that R14 scan's anomaly rows + marker (no key) made the newest
/// head the v1 one → `anomaly_head_check` = `Absent`, and the report
/// showed the R15 run as "현재 결과 — 후보 0건". P26: the result run's
/// own (v2, same `scan_id`) head is the reference; a v1 head after it →
/// withheld (`mixed-version`).
#[test]
fn r16_i1_v1_head_after_the_result_runs_head_is_mixed_version() {
    let (dir, _source) = indexed_case("i1");
    let mine = key(0xa1);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        let r15 = lane_len(&anomaly);
        assert!(matches!(
            crate::report::read_anomaly_head_check(&dir),
            crate::report::HashVerifyHeadCheck::Intact { .. }
        ));
        r14_build_scan(&dir, &mine, 4_000_000_000);
        // The R14 scan's anomaly rows and marker cut.
        delete_last_lines(&anomaly, lane_len(&anomaly) - r15);
        assert_eq!(super::read_lane_rows(&anomaly).unwrap().status(), "keyed");
        let check = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(check.status(), "unknown", "{check:?}");
        assert_eq!(check.reason_code(), Some("mixed-version"), "{check:?}");
        let current = report_anomaly_section(&dir);
        assert!(
            !current.contains("현재 결과 —"),
            "the R15 run came back as the current result: {current}"
        );
        assert!(current.contains("현재 결과 없음(결과 아님)"), "{current}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I3 (the R15 reviewer's PoC-B): the re-verification lane is shorter
/// than its newest head (truncation is certain), but one unkeyed line
/// appended to the sign-off lane after that head made the check return
/// `unknown` / `unusable-line` BEFORE the anchor comparison — the error
/// "꼬리 잘림" became a warning. P26: anchors first.
#[test]
fn r16_i3_anchor_comparison_comes_before_the_trailing_line() {
    let (dir, source) = indexed_case("i3");
    let mine = key(0xa3);
    let hv_lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        let (hv_first, anomaly_first) = (lane_len(&hv_lane), lane_len(&anomaly));
        fs::write(&source, b"r16-source-bytes-TAMPERED").unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
        // Run 2's rows + markers cut from both result lanes (no key).
        delete_last_lines(&hv_lane, lane_len(&hv_lane) - hv_first);
        delete_last_lines(&anomaly, lane_len(&anomaly) - anomaly_first);
        // One unkeyed line after the last head.
        append_chained_jsonl_keyed(&signoff, r#"{"event":"x-note"}"#, None).unwrap();
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
        let check = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(check.status(), "truncated", "{check:?}");
    });
    let _ = fs::remove_dir_all(&dir);
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

/// P26: a head naming ANOTHER scan after the result run's own head
/// withholds (`scan-mismatch`). Here the newer scan's anomaly marker lost
/// its HMAC (no key needed) — the older run became the newest usable
/// marker; R15 compared the newest head only (`not-authenticated`).
#[test]
fn r16_p26_another_scans_head_after_the_result_is_scan_mismatch() {
    let (dir, source) = indexed_case("p26-mismatch");
    let mine = key(0xa6);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, false).unwrap();
        fs::write(&source, b"r16-source-bytes-TAMPERED").unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
        strip_hmac(&anomaly, lane_len(&anomaly) - 1);
        let check = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(check.reason_code(), Some("scan-mismatch"), "{check:?}");
        let reason = check
            .withhold_reason_in(crate::report::HeadLane::Anomaly)
            .unwrap();
        assert!(
            reason.contains(crate::report::HEAD_SCAN_MISMATCH_NOTE)
                && reason.contains(crate::report::HEAD_RESCAN_NOTE),
            "{reason}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// P26 with a real upgrade path: an older build's re-hash scan after an
/// R15 scan (rows intact) is still anchored for the re-verification lane
/// (its own v1 head names its run); the anomaly lane's result is then the
/// older build's marker (no `scan_id`) while a v2 head exists →
/// `mixed-version`, withheld; the next scan with this build clears it.
#[test]
fn r16_p26_older_build_scan_is_withheld_until_the_next_scan() {
    let (dir, source) = indexed_case("p26-upgrade");
    let mine = key(0xa7);
    let hv_lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        // The older build's re-hash run: row, marker with head_in, v1 head
        // naming the run, then its anomaly marker (no scan_id).
        let sha = super::digest_file(&source).unwrap();
        let run = "4000000000-beef";
        let row = serde_json::json!({"schema_version":1,"event":"hash-verify","run_unix":4_000_000_000u64,
            "run_id":run,"id":"vid_a","source_path":source.to_string_lossy(),
            "recorded_sha256":sha,"observed_sha256":sha,"status":"match"});
        let done = serde_json::json!({"schema_version":1,"event":"hash-verify-run","run_unix":4_000_000_000u64,
            "run_id":run,"records":1,"head_in":crate::report::SIGNOFF_LOG_REL_PATH});
        append_chained_jsonl_keyed(&hv_lane, &row.to_string(), Some(&mine)).unwrap();
        append_chained_jsonl_keyed(&hv_lane, &done.to_string(), Some(&mine)).unwrap();
        let hv_head = super::lane_head(&hv_lane).unwrap().unwrap();
        let head = serde_json::json!({"schema_version":1,"event":"hash-verify-head",
            "run_id":run,"run_unix":4_000_000_000u64,
            "lane":crate::anomaly::HASH_VERIFY_REL_PATH,
            "entries":hv_head.entries,"last_entry_sha256":hv_head.last_entry_sha256});
        append_chained_jsonl_keyed(&signoff, &head.to_string(), Some(&mine)).unwrap();
        let marker = serde_json::json!({"schema_version":1,"event":"anomaly-scan-run",
            "scanned_unix":4_000_000_000u64,"hash_mode":"rehash","finding_count":0});
        append_chained_jsonl_keyed(&anomaly, &marker.to_string(), Some(&mine)).unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(summary.is_result, "{summary:?}");
        assert_eq!(summary.run_id.as_deref(), Some(run));
        assert_eq!(summary.head_status, "intact", "{summary:?}");
        let check = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(check.reason_code(), Some("mixed-version"), "{check:?}");
        // The next scan with this build: both lanes anchored again.
        crate::anomaly::scan_case(&dir, false).unwrap();
        assert!(matches!(
            crate::report::read_anomaly_head_check(&dir),
            crate::report::HashVerifyHeadCheck::Intact { .. }
        ));
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert_eq!(summary.head_status, "intact", "{summary:?}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I4: in a shared case another examiner's sign-off written without this
/// PC's key withholds the result (the line could be anything), but the
/// wording says it is, by its shape, ANOTHER event — no "stripped head" —
/// and that a re-scan here clears it. A head-shaped unusable line keeps
/// the "may be a stripped head" wording.
#[test]
fn r16_i4_other_event_lines_are_worded_apart_from_unknown_ones() {
    let (dir, _source) = indexed_case("i4");
    let mine = key(0xa4);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        append_chained_jsonl_keyed(&signoff, r#"{"event":"signoff","examiner":"other"}"#, None)
            .unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert_eq!(summary.head_reason, Some("unusable-line"), "{summary:?}");
        let reason = summary.withheld_reason.clone().unwrap();
        assert!(
            reason.contains(crate::report::HEAD_OTHER_EVENT_NOTE)
                && reason.contains("signoff")
                && reason.contains(crate::report::HEAD_RESCAN_NOTE)
                && !reason.contains("HMAC이 제거된"),
            "{reason}"
        );
        // A head-shaped unusable line: may be a stripped head.
        append_chained_jsonl_keyed(&signoff, r#"{"event":"hash-verify-head"}"#, None).unwrap();
        let reason = crate::anomaly::latest_hash_verify_summary(&dir)
            .unwrap()
            .withheld_reason
            .unwrap();
        assert!(
            !reason.contains(crate::report::HEAD_OTHER_EVENT_NOTE)
                && reason.contains("HMAC이 제거된"),
            "{reason}"
        );
        // A re-scan here writes a new head after them: resolved.
        crate::anomaly::scan_case(&dir, true).unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(summary.is_result, "{summary:?}");
        assert_eq!(summary.head_status, "intact", "{summary:?}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I5: a torn sign-off lane (its last line has no newline) made the scan
/// write both result lanes and fail on its head. The scan now checks
/// every lane it appends to first and refuses before touching any.
#[test]
fn r16_i5_torn_signoff_lane_refuses_the_scan_before_any_result_row() {
    use std::io::Write;
    let (dir, _source) = indexed_case("i5");
    let mine = key(0xa5);
    let hv_lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        let before = (fs::read(&hv_lane).unwrap(), fs::read(&anomaly).unwrap());
        fs::OpenOptions::new()
            .append(true)
            .open(&signoff)
            .unwrap()
            .write_all(b"{\"event\":\"torn")
            .unwrap();
        let err = crate::anomaly::scan_case(&dir, true).unwrap_err();
        assert!(err.contains("시작하지 않았습니다"), "{err}");
        assert!(err.contains(crate::report::SIGNOFF_LOG_REL_PATH), "{err}");
        assert_eq!(fs::read(&hv_lane).unwrap(), before.0);
        assert_eq!(fs::read(&anomaly).unwrap(), before.1);
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I7: an unchained log's entry and an audit entry not authenticated here
/// are `time_unaudited: true` (R15: false); an authenticated lane entry
/// stays false.
#[test]
fn r16_i7_unchained_and_unauthenticated_event_times_are_unaudited() {
    let dir = r16_case("i7");
    let mine = key(0xa8);
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    append_chained_jsonl_keyed(
        &lane,
        r#"{"event":"validate","target_path":"/e/ok.mp4","validated_unix":100}"#,
        Some(&mine),
    )
    .unwrap();
    append_chained_jsonl_keyed(
        &lane,
        r#"{"event":"validate","target_path":"/e/unauth.mp4","validated_unix":200}"#,
        None,
    )
    .unwrap();
    fs::write(
        dir.join("evidence/logs/notes-unchained.jsonl"),
        "{\"event\":\"note\",\"target_path\":\"/e/plain.mp4\",\"noted_unix\":300}\n",
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
            .map(|event| (event["kind"].clone(), event["time_unaudited"].clone()))
            .unwrap_or_else(|| panic!("{path}: {events:?}"))
    };
    let (kind, ok) = flag("/e/ok.mp4");
    assert_eq!(kind, "audit-event", "{events:?}");
    assert_eq!(ok, serde_json::Value::Bool(false), "{events:?}");
    let (kind, unauth) = flag("/e/unauth.mp4");
    assert_ne!(kind, "audit-event", "{events:?}");
    assert_eq!(unauth, serde_json::Value::Bool(true), "{events:?}");
    let (_, plain) = flag("/e/plain.mp4");
    assert_eq!(plain, serde_json::Value::Bool(true), "{events:?}");
    let _ = fs::remove_dir_all(&dir);
}
