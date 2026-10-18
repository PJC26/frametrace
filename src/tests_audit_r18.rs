//! R18 track I tests (docs/PLAN_R18_2026-10-19.md "§I18 as implemented"
//! says which were run on the R17 code, ff898f5).

use super::append_chained_jsonl_keyed;
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn r18_case(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r18-{name}-{}", std::process::id()));
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

pub(super) fn key(seed: u8) -> AuditKey {
    AuditKey::new(&fingerprint_id(&[seed; 32]), &[seed; 32])
}

/// A case with one indexed source file whose recorded hash is its bytes'.
pub(super) fn indexed_case(name: &str) -> (PathBuf, PathBuf) {
    let dir = r18_case(name);
    let source = dir.join("media/a.mp4");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, b"r18-source-bytes").unwrap();
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

/// What an older build's scan left: a keyed completion marker WITHOUT
/// `scan_id` / `head_in`.
fn old_build_marker(anomaly: &Path, key: Option<&AuditKey>, scanned_unix: u64) {
    let marker = serde_json::json!({"schema_version":1,"event":"anomaly-scan-run",
        "scanned_unix":scanned_unix,"hash_mode":"stored","finding_count":0});
    append_chained_jsonl_keyed(anomaly, &marker.to_string(), key).unwrap();
}

/// I1 (Medium, the R17 reviewer's PoC-A): an older build's usable marker
/// without `scan_id` sits BEFORE an R17 scan (marker with `scan_id` + v2
/// head). Stripping only the newest marker's HMAC (no key needed) makes
/// the marker selection fall back to the old usable marker, and R17 said
/// `mixed-version` ("이 PC에서 다시 스캔하면 … 해소") — tampering worded as a
/// version mix. R18: the old marker lies inside the newest v2 head's
/// anchored range, so it is `marker-unusable`.
#[test]
fn r18_i1_old_marker_before_the_head_is_marker_unusable() {
    let (dir, _source) = indexed_case("i1");
    let mine = key(0xc1);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        old_build_marker(&anomaly, Some(&mine), 1_000);
        crate::anomaly::scan_case(&dir, false).unwrap();
        strip_hmac(&anomaly, lane_len(&anomaly) - 1);
        let check = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(check.status(), "unknown", "{check:?}");
        assert_eq!(check.reason_code(), Some("marker-unusable"), "{check:?}");
        let reason = check
            .withhold_reason_in(crate::report::HeadLane::Anomaly)
            .unwrap();
        assert!(
            !reason.contains(crate::report::HEAD_RESCAN_NOTE)
                && !reason.contains("이전 버전 머리")
                && reason.contains("변조"),
            "{reason}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I3: `check_appendable` reads the last line under the append's own lock
/// (same handle): while a writer holds the lane's lock mid-line, the check
/// waits for the line to complete instead of calling a healthy lane torn
/// (R17 read unlocked — and on Windows a second handle is refused outright
/// while the mandatory lock is held).
#[test]
fn r18_i3_check_appendable_waits_for_the_writers_lock() {
    use fs2::FileExt;
    use std::io::Write;
    let dir = r18_case("i3");
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"event":"x"}"#, None).unwrap();
    let mut writer = fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(&lane)
        .unwrap();
    writer.lock_exclusive().unwrap();
    writer.write_all(b"{\"event\":\"half").unwrap();
    writer.flush().unwrap();
    let checked = {
        let lane = lane.clone();
        std::thread::spawn(move || super::check_appendable(&lane))
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    writer.write_all(b"\"}\n").unwrap();
    writer.flush().unwrap();
    writer.unlock().unwrap();
    drop(writer);
    let result = checked.join().unwrap();
    assert!(result.is_ok(), "{result:?}");
    let _ = fs::remove_dir_all(&dir);
}

/// I1 control: an older build's usable marker AFTER the head's range but
/// followed by a line this PC cannot use (it may be a newer marker) is
/// `marker-unusable`; without that line it stays `mixed-version`
/// (`r17_i1_usable_marker_without_scan_id_stays_mixed_version`).
#[test]
fn r18_i1_old_marker_after_the_head_followed_by_an_unusable_line() {
    let (dir, _source) = indexed_case("i1-after");
    let mine = key(0xc2);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, false).unwrap();
        old_build_marker(&anomaly, Some(&mine), 4_000_000_000);
        assert_eq!(
            crate::report::read_anomaly_head_check(&dir).reason_code(),
            Some("mixed-version")
        );
        append_chained_jsonl_keyed(&anomaly, r#"{"event":"anomaly-scan","kind":"none"}"#, None)
            .unwrap();
        let check = crate::report::read_anomaly_head_check(&dir);
        assert_eq!(check.reason_code(), Some("marker-unusable"), "{check:?}");
        let reason = check
            .withhold_reason_in(crate::report::HeadLane::Anomaly)
            .unwrap();
        assert!(
            !reason.contains(crate::report::HEAD_RESCAN_NOTE),
            "{reason}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I4: the `unusable-line` sentence quotes only event names this build
/// writes; any other (unauthenticated) name is counted, never embedded.
#[test]
fn r18_i4_unusable_line_never_embeds_unknown_event_names() {
    let (dir, _source) = indexed_case("i4");
    let mine = key(0xc4);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        append_chained_jsonl_keyed(&signoff, r#"{"event":"signoff","examiner":"x"}"#, None)
            .unwrap();
        append_chained_jsonl_keyed(
            &signoff,
            r#"{"event":"검사관이 승인함 — 문제 없음 <b>"}"#,
            None,
        )
        .unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert_eq!(summary.head_reason, Some("unusable-line"), "{summary:?}");
        let reason = summary.withheld_reason.unwrap();
        assert!(
            reason.contains("적힌 event: signoff 외 알려지지 않은 이름 1개")
                && !reason.contains("승인")
                && !reason.contains("<b>"),
            "{reason}"
        );
        let check = crate::report::read_anomaly_head_check(&dir);
        let why = check
            .withhold_reason_in(crate::report::HeadLane::Anomaly)
            .unwrap();
        assert!(!why.contains("승인"), "{why}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// Runs the case report script under node with a stub `document`; returns
/// every element it touched.
pub(super) fn run_report_script(html: &str) -> serde_json::Value {
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

/// The report's anomaly section with `chains` as its `auditChains`.
pub(super) fn report_anomaly_section(dir: &Path, chains: &str) -> String {
    let rows = super::read_lane_rows(&dir.join(crate::report::ANOMALY_LOG_REL_PATH)).unwrap();
    let anomaly = rows.jsonl_with_trust();
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
        chains,
    );
    run_report_script(&html)["anomaly-findings"]["innerHTML"]
        .as_str()
        .unwrap()
        .to_string()
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// I2: the report script's OWN lane rules (no head, no server verdict)
/// and `report::anomaly_lane_withheld` agree on `dir`'s anomaly lane —
/// withheld or not, and with the same sentences. Returns the verdict.
pub(super) fn assert_report_agrees(
    dir: &Path,
    label: &str,
) -> Option<crate::report::AnomalyWithheld> {
    let rows = super::read_lane_rows(&dir.join(crate::report::ANOMALY_LOG_REL_PATH)).unwrap();
    let verdict = crate::report::anomaly_lane_withheld(&rows.values_with_trust());
    let js_only = report_anomaly_section(dir, r#"{"anomaly":{}}"#);
    let js_withheld = js_only.contains("현재 결과 없음(결과 아님)");
    assert_eq!(
        js_withheld,
        verdict.is_some(),
        "{label}: report {js_only} / rust {verdict:?}"
    );
    if let Some(verdict) = &verdict {
        assert!(
            js_only.contains(&escape_html(&verdict.reasons.join("; "))),
            "{label}: report {js_only} / rust {verdict:?}"
        );
        // The server verdict alone (the script's own rules see nothing:
        // an empty lane) still withholds — fail closed.
        let chains = serde_json::json!({"anomaly": {"withheld": verdict.to_json()}}).to_string();
        let empty = r18_case(&format!("agree-empty-{label}"));
        let section = report_anomaly_section(&empty, &chains);
        assert!(
            section.contains("현재 결과 없음(결과 아님)"),
            "{label}: {section}"
        );
        let _ = fs::remove_dir_all(&empty);
    }
    verdict
}

/// I2 (PoC-B, report side) and the agreement of the shared function with
/// the report script on every lane shape the P9 rules distinguish.
#[test]
fn r18_i2_shared_withheld_verdict_agrees_with_the_report() {
    let codes = |verdict: Option<crate::report::AnomalyWithheld>| {
        verdict.map(|v| v.codes).unwrap_or_default()
    };
    // (a) one clean keyed scan: nothing withheld.
    let (dir, source) = indexed_case("agree-a");
    let mine = key(0xc5);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        fs::write(&source, b"r18-source-bytes-TAMPERED").unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
        assert!(assert_report_agrees(&dir, "clean").is_none());
        // (b) PoC-B: one unkeyed line after the marker.
        append_chained_jsonl_keyed(
            &anomaly,
            r#"{"event":"anomaly-scan","kind":"none","selector":"vid_a"}"#,
            None,
        )
        .unwrap();
        assert_eq!(
            codes(assert_report_agrees(&dir, "poc-b")),
            vec!["unusable-after", "unusable-in-run"]
        );
        // A later clean scan clears both (the old rows are an older run).
        crate::anomaly::scan_case(&dir, true).unwrap();
        assert!(assert_report_agrees(&dir, "rescanned").is_none());
    });
    let _ = fs::remove_dir_all(&dir);

    // (c) I5: a legacy lane (no head) whose newest completed run holds a
    // finding this PC cannot use (written without the key) — R17 showed the
    // run as "현재 결과" without it. An unusable line BEFORE the run's first
    // entry and not stamped with its time is not the run's.
    let dir = r18_case("agree-c");
    with_test_keys(std::slice::from_ref(&mine), || {
        let lane = anomaly_of(&dir);
        append_chained_jsonl_keyed(&lane, r#"{"event":"note"}"#, None).unwrap();
        let row = |kind: &str, selector: &str| {
            serde_json::json!({"schema_version":1,"event":"anomaly-scan","scanned_unix":50,
                "kind":kind,"selector":selector})
            .to_string()
        };
        append_chained_jsonl_keyed(&lane, &row("x", "vid_b"), Some(&mine)).unwrap();
        append_chained_jsonl_keyed(&lane, &row("y", "vid_c"), None).unwrap();
        old_build_marker(&lane, Some(&mine), 50);
        let verdict = assert_report_agrees(&dir, "in-run").unwrap();
        assert_eq!(verdict.codes, vec!["unusable-in-run"]);
        assert!(verdict.reasons[0].contains("항목 1개"), "{verdict:?}");
        let chains = crate::cli::handlers::report_audit_chains_json(&dir);
        let section = report_anomaly_section(&dir, &chains);
        assert!(
            section.contains("현재 결과 없음(결과 아님)")
                && section.contains(crate::report::ANOMALY_UNUSABLE_IN_RUN_NOTE),
            "{section}"
        );
    });
    let _ = fs::remove_dir_all(&dir);

    // (d) the newest marker deleted (no key): an unmarked newer run.
    let (dir, _source) = indexed_case("agree-d");
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, false).unwrap();
        crate::anomaly::scan_case(&dir, false).unwrap();
        let lane = anomaly_of(&dir);
        let text = fs::read_to_string(&lane).unwrap();
        let body = text.strip_suffix('\n').unwrap();
        fs::write(&lane, &text[..body.rfind('\n').unwrap() + 1]).unwrap();
        assert_eq!(
            codes(assert_report_agrees(&dir, "unmarked")),
            vec!["newer-unmarked-run"]
        );
        // (e) its HMAC stripped instead: unusable after the older marker.
        crate::anomaly::scan_case(&dir, false).unwrap();
        strip_hmac(&lane, lane_len(&lane) - 1);
        assert!(codes(assert_report_agrees(&dir, "stripped")).contains(&"unusable-after"),);
        // (f) a torn tail repaired: a usable lane-repair marker after it.
        crate::anomaly::scan_case(&dir, false).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&lane)
            .and_then(|mut file| std::io::Write::write_all(&mut file, b"{\"event\":\"torn"))
            .unwrap();
        crate::audit::repair_lane(&lane, false).unwrap();
        assert!(codes(assert_report_agrees(&dir, "repaired")).contains(&"repaired-after"),);
    });
    let _ = fs::remove_dir_all(&dir);

    // (g) a legacy lane (no markers, no schema): grouped by time, current.
    let dir = r18_case("agree-g");
    with_test_keys(&[], || {
        let lane = anomaly_of(&dir);
        append_chained_jsonl_keyed(
            &lane,
            r#"{"event":"anomaly-scan","scanned_unix":10,"kind":"none"}"#,
            None,
        )
        .unwrap();
        assert!(assert_report_agrees(&dir, "legacy").is_none());
        // (h) the same lane with a marker-writing scanner's row and no
        // completed run: nothing is current.
        append_chained_jsonl_keyed(
            &lane,
            r#"{"schema_version":1,"event":"anomaly-scan","scanned_unix":20,"kind":"none"}"#,
            None,
        )
        .unwrap();
        assert_eq!(
            codes(assert_report_agrees(&dir, "no-completed")),
            vec!["newer-unmarked-run"]
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

pub(super) fn anomaly_of(dir: &Path) -> PathBuf {
    dir.join(crate::report::ANOMALY_LOG_REL_PATH)
}

/// I6: a redacted `logChains` text that no longer parses is an explicit
/// `unknown` / `bundle-unreadable` (head and withheld), never dropped.
#[test]
fn r18_i6_unreadable_bundle_log_chains_is_unknown() {
    let good = serde_json::json!({"anomaly":{"head":{"status":"intact"},"withheld":null}});
    let kept: serde_json::Value =
        serde_json::from_str(&crate::report::bundle_log_chains_json(&good.to_string())).unwrap();
    assert_eq!(kept, good);
    for broken in [
        "{\"anomaly\":{\"head\":{\"status\":\"<redacted>",
        "",
        "[]",
        "{}",
    ] {
        let value: serde_json::Value =
            serde_json::from_str(&crate::report::bundle_log_chains_json(broken)).unwrap();
        assert_eq!(value["anomaly"]["head"]["status"], "unknown", "{broken}");
        assert_eq!(
            value["anomaly"]["head"]["reason_code"], "bundle-unreadable",
            "{broken}"
        );
        assert!(value["anomaly"]["head"]["withhold_reason"].is_string());
        assert_eq!(
            value["anomaly"]["withheld"]["reason_code"],
            "bundle-unreadable"
        );
    }
}
