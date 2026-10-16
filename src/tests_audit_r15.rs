//! R15 track I tests: P22 "the head is written at the END of the scan and
//! covers every result lane" (I1, I2) and the rest of the R15 trust rows.
//! See `docs/PLAN_R15_2026-10-16.md` "§I15 as implemented" for which were
//! run on the R14 code (7c3955e).

use super::append_chained_jsonl_keyed;
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use std::fs;
use std::path::{Path, PathBuf};

fn r15_case(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r15-{name}-{}", std::process::id()));
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
    let dir = r15_case(name);
    let source = dir.join("media/a.mp4");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, b"r15-source-bytes").unwrap();
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

/// Strips the HMAC fields from line `index` (0-based) — the chain stays
/// structurally intact (`entry_sha256` does not cover them).
fn strip_hmac(lane: &Path, index: usize) {
    let text = fs::read_to_string(lane).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let cut = lines[index].find(",\"entry_hmac_sha256\"").unwrap();
    lines[index] = format!("{}}}", &lines[index][..cut]);
    fs::write(lane, format!("{}\n", lines.join("\n"))).unwrap();
}

/// Line index (0-based) of the last line of `lane` containing `needle`.
fn last_line_with(lane: &Path, needle: &str) -> usize {
    fs::read_to_string(lane)
        .unwrap()
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle))
        .map(|(at, _)| at)
        .last()
        .unwrap()
}

/// The custody statement's green section-5 "everything matched" banner.
const ALL_MATCH_OK: &str = "<p class=\"banner ok\">All ";

/// I1 (Medium, the R14 reviewer's PoC 1): a pre-R14 run A (match, its
/// marker without `head_in`), then an R14 run B (mismatch) with head H_B.
/// Stripping H_B's HMAC fields leaves NO usable head — either H_B is the
/// sign-off lane's last line (the chain still passes) or a keyed line
/// follows it (the chain then breaks there: nothing in the lane is
/// usable). Cutting B's two rows from the re-verification lane then left
/// `head_status: "absent"` and A green. P22: no usable head + an unusable
/// line / a chain failure in the sign-off lane → `Unknown` (withheld) —
/// the stripped line's content is never read.
#[test]
fn r15_i1_no_usable_head_and_an_unusable_signoff_line_is_unknown() {
    for keyed_line_after in [false, true] {
        i1_case(keyed_line_after);
    }
}

fn i1_case(keyed_line_after: bool) {
    let (dir, source) = indexed_case(&format!("i1-{keyed_line_after}"));
    let mine = key(0xf1);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    // A: written the pre-R14 way (no `head_in`, no head).
    let sha = super::digest_file(&source).unwrap();
    let a_row = serde_json::json!({"schema_version":1,"event":"hash-verify","run_unix":100,
        "run_id":"100-aaaa","id":"vid_a","source_path":source.to_string_lossy(),
        "recorded_sha256":sha,"observed_sha256":sha,"status":"match"});
    let a_done = serde_json::json!({"schema_version":1,"event":"hash-verify-run","run_unix":100,
        "run_id":"100-aaaa","records":1});
    append_chained_jsonl_keyed(&lane, &a_row.to_string(), Some(&mine)).unwrap();
    append_chained_jsonl_keyed(&lane, &a_done.to_string(), Some(&mine)).unwrap();
    with_test_keys(std::slice::from_ref(&mine), || {
        // B: the real writer, the source changed → MISMATCH, head H_B.
        fs::write(&source, b"r15-source-bytes-TAMPERED").unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
        if keyed_line_after {
            // A keyed sign-off line after H_B (any later sign-off event).
            append_chained_jsonl_keyed(&signoff, r#"{"event":"x-later-note"}"#, Some(&mine))
                .unwrap();
        }
        let head_at = last_line_with(&signoff, crate::report::HASH_VERIFY_HEAD_EVENT);
        strip_hmac(&signoff, head_at);
        assert_eq!(
            super::read_lane_rows(&signoff).unwrap().failure.is_some(),
            keyed_line_after
        );
        // B's row and marker cut from the re-verification lane.
        let b_rows = fs::read_to_string(&lane)
            .unwrap()
            .lines()
            .filter(|line| !line.contains("100-aaaa"))
            .count();
        assert_eq!(b_rows, 2);
        delete_last_lines(&lane, 2);
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(
            !summary.is_result,
            "A came back as the result after H_B was stripped and B cut: {summary:?}"
        );
        assert!(!summary.verified, "{summary:?}");
        assert_eq!(summary.head_status, "unknown", "{summary:?}");
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(!html.contains(ALL_MATCH_OK), "green banner:\n{html}");
    });
    let _ = fs::remove_dir_all(&dir);
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

/// The report's anomaly section for the case as `make-report` builds it
/// (the lane rows with their trust, the audit-chain map).
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

/// I2 (Medium, the R14 reviewer's PoC 5): R14 wrote the head BEFORE the
/// anomaly rows and never anchored the anomaly lane, so cutting the newest
/// anomaly run's tail (its rows and marker; no key) left the lane `keyed`
/// and the report showed the previous run as "현재 결과". P22: the head is
/// written at the end of the scan and anchors the anomaly lane too; the
/// report's anomaly "current result" compares it.
#[test]
fn r15_i2_cut_anomaly_tail_is_no_current_result() {
    let (dir, source) = indexed_case("i2");
    let mine = key(0xf2);
    let anomaly = dir.join("evidence/logs/anomaly-log.jsonl");
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        let first = super::read_lane_rows(&anomaly).unwrap().rows.len();
        // Run 2: the source changed → a hash-revalidation-mismatch
        // finding, then the run's marker.
        fs::write(&source, b"r15-source-bytes-TAMPERED").unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
        let current = report_anomaly_section(&dir);
        assert!(current.contains("현재 결과 —"), "{current}");
        assert!(current.contains("hash-revalidation-mismatch"), "{current}");
        // Cut run 2's tail of the anomaly lane.
        let now = super::read_lane_rows(&anomaly).unwrap().rows.len();
        delete_last_lines(&anomaly, now - first);
        assert_eq!(super::read_lane_rows(&anomaly).unwrap().status(), "keyed");
        let current = report_anomaly_section(&dir);
        assert!(
            !current.contains("현재 결과 —"),
            "the previous run came back as the current result: {current}"
        );
        assert!(current.contains("현재 결과 없음(결과 아님)"), "{current}");
        assert!(
            current.contains(crate::report::HASH_VERIFY_HEAD_TRUNCATED),
            "{current}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
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

/// P22: the head is the LAST line a scan writes, it names both result
/// lanes (`lanes`), and the anomaly marker names its scan (`scan_id`,
/// `head_in`) — the same id as the re-verification run.
#[test]
fn r15_p22_head_is_written_last_and_anchors_both_lanes() {
    let (dir, _source) = indexed_case("p22");
    let mine = key(0xf0);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    let anomaly = dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    with_test_keys(std::slice::from_ref(&mine), || {
        let result = crate::anomaly::scan_case(&dir, true).unwrap();
        let rows = super::read_lane_rows(&signoff).unwrap();
        let head = &rows.rows.last().unwrap().value;
        assert_eq!(head["event"], crate::report::HASH_VERIFY_HEAD_EVENT);
        assert_eq!(head["schema_version"], crate::report::HEAD_SCHEMA_VERSION);
        let run_id = result.hash_verify_run_id.unwrap();
        assert_eq!(head["run_id"], run_id.as_str());
        assert_eq!(head["scan_id"], run_id.as_str());
        let anomaly_rows = super::read_lane_rows(&anomaly).unwrap();
        assert_eq!(
            head["lanes"][crate::report::ANOMALY_LOG_REL_PATH]["entries"],
            anomaly_rows.rows.len() as u64
        );
        let marker = &anomaly_rows.rows.last().unwrap().value;
        assert_eq!(marker["event"], "anomaly-scan-run");
        assert_eq!(marker["scan_id"], run_id.as_str());
        assert_eq!(
            marker[crate::report::HASH_VERIFY_MARKER_HEAD_FIELD],
            crate::report::SIGNOFF_LOG_REL_PATH
        );
        assert!(matches!(
            crate::report::anomaly_head_check(&dir),
            crate::report::HashVerifyHeadCheck::Intact { .. }
        ));
        // A stored-hash scan writes a head too (no re-hash run id); the
        // re-verification result still reads intact.
        crate::anomaly::scan_case(&dir, false).unwrap();
        let rows = super::read_lane_rows(&signoff).unwrap();
        let head = &rows.rows.last().unwrap().value;
        assert!(head["run_id"].is_null(), "{head}");
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(summary.is_result && summary.verified, "{summary:?}");
        assert_eq!(summary.head_status, "intact");
        // The anomaly result run's own head cut (the sign-off lane's last
        // line): its marker says it wrote one → unknown, no result.
        delete_last_lines(&signoff, 1);
        let check = crate::report::anomaly_head_check(&dir);
        assert_eq!(check.reason_code(), Some("own-head-missing"), "{check:?}");
        let current = report_anomaly_section(&dir);
        assert!(current.contains("현재 결과 없음(결과 아님)"), "{current}");
        assert!(current.contains("이상 징후 머리 확인 불가"), "{current}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I3: two COMPLETED runs written into each other in an old lane (before
/// the scan lock): A's row, B's row, B's marker, A's marker — A's marker is
/// last, but B started after A and found a MISMATCH. Which is newer cannot
/// be told by position: the result run's span overlapping another
/// completed run withholds it.
#[test]
fn r15_i3_overlapping_completed_runs_withhold_the_result() {
    let dir = r15_case("i3");
    let mine = key(0xf3);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(100, "100-aaaa", "vid_a", "match"),
            hv(200, "200-bbbb", "vid_a", "mismatch"),
            hv_done(200, "200-bbbb", 1),
            hv_done(100, "100-aaaa", 1),
        ],
        &mine,
    );
    with_test_keys(std::slice::from_ref(&mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert!(
            summary
                .withheld_reason
                .as_deref()
                .is_some_and(|r| r.contains(crate::anomaly::OVERLAPPING_RUNS_REASON)),
            "{summary:?}"
        );
        // A later run that does not overlap is the result again.
        write_lane(
            &lane,
            &[
                hv(300, "300-cccc", "vid_a", "match"),
                hv_done(300, "300-cccc", 1),
            ],
            &mine,
        );
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(summary.is_result && summary.verified, "{summary:?}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I4: a crashed run stamped by a clock set in the FUTURE, positioned
/// wholly before the result run's first row, withheld every later result
/// forever (R14 rule (c): a greater `run_unix` is newer). It no longer
/// does; the future stamp is shown as "시계 이상". In the report the same.
#[test]
fn r15_i4_future_stamped_crash_wholly_before_the_result_is_a_clock_anomaly() {
    let dir = r15_case("i4");
    let mine = key(0xf4);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let future = crate::util::now_unix().unwrap() as i64 + 10 * 365 * 24 * 3600;
    let future_id = format!("{future}-ffff");
    write_lane(
        &lane,
        &[
            hv(future, &future_id, "vid_a", "mismatch"),
            hv(100, "100-aaaa", "vid_a", "match"),
            hv_done(100, "100-aaaa", 1),
        ],
        &mine,
    );
    with_test_keys(std::slice::from_ref(&mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(summary.is_result && summary.verified, "{summary:?}");
        assert_eq!(summary.clock_anomaly_runs, 1, "{summary:?}");
        assert!(
            summary
                .clock_note
                .as_deref()
                .is_some_and(|note| note.starts_with(crate::anomaly::CLOCK_ANOMALY_NOTE)
                    && note.contains(&future_id)),
            "{summary:?}"
        );
        // Placed AFTER the result's first row, the future run still
        // withholds (rule (b)) and the reason says "시계 이상".
        write_lane(&lane, &[hv(future, &future_id, "vid_b", "mismatch")], &mine);
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert!(
            summary
                .withheld_reason
                .as_deref()
                .is_some_and(|r| r.contains(crate::anomaly::CLOCK_ANOMALY_NOTE)),
            "{summary:?}"
        );
    });
    // The report's anomaly lane: an unmarked run stamped in the future,
    // wholly before the completed run, does not withhold it.
    let rows = [
        serde_json::json!({"schema_version":1,"event":"anomaly-scan","scanned_unix":future,"kind":"gap","selector":"vid_old","detail":"o"}).to_string(),
        serde_json::json!({"schema_version":1,"event":"anomaly-scan","scanned_unix":100,"kind":"gap","selector":"vid_a","detail":"a"}).to_string(),
        serde_json::json!({"schema_version":1,"event":"anomaly-scan-run","scanned_unix":100,"hash_mode":"stored","finding_count":1}).to_string(),
    ]
    .join("\n");
    let html = crate::report::render_case_report(&crate::report::ReportInputs {
        manifest_json: "{}",
        index_json: "{}",
        export_log_jsonl: "",
        proxy_log_jsonl: "",
        thumbnail_log_jsonl: "",
        carve_log_jsonl: "",
        filesystem_log_jsonl: "",
        validation_log_jsonl: "",
        anomaly_log_jsonl: &rows,
        batch_log_jsonl: "",
        join_log_jsonl: "",
        audio_levels_json: "",
        scan_runs_json: "[]",
        marks_json: "[]",
        redaction_applied: false,
        review_warnings_json: "",
        read_errors_json: "",
        telemetry_json: "",
    });
    let elements = run_report_script(&html);
    let current = elements["anomaly-findings"]["innerHTML"].as_str().unwrap();
    assert!(
        current.contains("현재 결과 —") && current.contains("vid_a"),
        "{current}"
    );
    let history = elements["anomaly-history"]["innerHTML"].as_str().unwrap();
    assert!(history.contains("시계 이상"), "{history}");
    let _ = fs::remove_dir_all(&dir);
}

/// I5: the sign-off lane's only unusable lines are another PC's (signed
/// under a key this PC does not hold): still no result, but the reason is
/// "다른 PC 머리일 수 있음" (`head_reason: other-pc`), not "stripped".
#[test]
fn r15_i5_another_pcs_lines_may_be_its_head() {
    let dir = r15_case("i5");
    let mine = key(0xf5);
    let theirs = key(0xf6);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let signoff = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(100, "100-aaaa", "vid_a", "match"),
            hv_done(100, "100-aaaa", 1),
        ],
        &mine,
    );
    append_chained_jsonl_keyed(&signoff, r#"{"event":"hash-verify-head"}"#, Some(&theirs)).unwrap();
    with_test_keys(std::slice::from_ref(&mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert_eq!(
            (summary.head_status, summary.head_reason),
            ("unknown", Some("other-pc")),
            "{summary:?}"
        );
        assert!(
            summary
                .withheld_reason
                .as_deref()
                .is_some_and(|r| r.contains(crate::report::HEAD_OTHER_PC_NOTE)),
            "{summary:?}"
        );
    });
    // An unkeyed line too: not only another PC's — "unusable-line".
    append_chained_jsonl_keyed(&signoff, r#"{"event":"x-note"}"#, None).unwrap();
    with_test_keys(std::slice::from_ref(&mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert_eq!(summary.head_reason, Some("unusable-line"), "{summary:?}");
        assert!(
            !summary
                .withheld_reason
                .as_deref()
                .unwrap_or_default()
                .contains(crate::report::HEAD_OTHER_PC_NOTE),
            "{summary:?}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I6: only lock contention is "another scan is running"; any other lock
/// failure is reported as itself.
#[test]
fn r15_i6_only_contention_is_another_scan() {
    let path = Path::new("db/anomaly-scan.lock");
    let busy = crate::anomaly::scan_lock_error(path, &fs2::lock_contended_error());
    assert!(busy.contains("실행 중"), "{busy}");
    let would_block = std::io::Error::from(std::io::ErrorKind::WouldBlock);
    assert!(crate::anomaly::scan_lock_error(path, &would_block).contains("실행 중"));
    let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let other = crate::anomaly::scan_lock_error(path, &denied);
    assert!(!other.contains("실행 중"), "{other}");
    assert!(other.contains("잠금을 걸지 못했습니다"), "{other}");
}

/// I7: `verify-package` checks each packaged result lane against the
/// PACKAGE's sign-off lane WITH the package's result run: R14 passed
/// `(None, false)`, so a package whose result run's own head was cut
/// (the sign-off lane's last line; the older head still matched) passed.
#[test]
fn r15_i7_verify_package_checks_the_result_runs_own_head() {
    let (dir, source) = indexed_case("i7");
    fs::write(dir.join("db/video_index.json"), b"{}").unwrap();
    fs::write(dir.join("db/video_paths.tsv"), b"id\tsource_path\n").unwrap();
    let conn = crate::case_db::open_case_db(&dir).unwrap();
    crate::case_db::init_schema(&conn).unwrap();
    drop(conn);
    let mine = key(0xf7);
    let out = dir.with_extension("pkg");
    let _ = fs::remove_dir_all(&out);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::anomaly::scan_case(&dir, true).unwrap();
        fs::write(&source, b"r15-source-bytes-TAMPERED").unwrap();
        crate::anomaly::scan_case(&dir, true).unwrap();
        crate::package::package_case(&dir, Some(&out)).unwrap();
        let line = |result: &crate::package::PackageVerification, subject: &str| {
            result
                .checks
                .iter()
                .find(|check| check.subject == subject && check.detail.contains("lane anchor:"))
                .map(|check| (check.status, check.detail.clone()))
        };
        let result = crate::package::verify_package(&out).unwrap();
        for subject in [
            crate::anomaly::HASH_VERIFY_REL_PATH,
            crate::report::ANOMALY_LOG_REL_PATH,
        ] {
            let (status, detail) = line(&result, subject).unwrap();
            assert_eq!(status, crate::package::CheckStatus::Pass, "{detail}");
        }
        // The packaged sign-off lane's last line (run 2's head) cut.
        delete_last_lines(&out.join(crate::report::SIGNOFF_LOG_REL_PATH), 1);
        let result = crate::package::verify_package(&out).unwrap();
        for subject in [
            crate::anomaly::HASH_VERIFY_REL_PATH,
            crate::report::ANOMALY_LOG_REL_PATH,
        ] {
            let (status, detail) = line(&result, subject).unwrap();
            assert_eq!(
                status,
                crate::package::CheckStatus::NotVerified,
                "{subject}: {detail}"
            );
            assert!(detail.contains("own-head-missing"), "{detail}");
        }
        // And a cut anomaly tail in the package is FAIL (TAIL CUT).
        let _ = fs::remove_dir_all(&out);
        crate::package::package_case(&dir, Some(&out)).unwrap();
        delete_last_lines(&out.join(crate::report::ANOMALY_LOG_REL_PATH), 1);
        let result = crate::package::verify_package(&out).unwrap();
        let (status, detail) = line(&result, crate::report::ANOMALY_LOG_REL_PATH).unwrap();
        assert_eq!(status, crate::package::CheckStatus::Fail, "{detail}");
        assert!(detail.contains("TAIL CUT"), "{detail}");
    });
    let _ = fs::remove_dir_all(&out);
    let _ = fs::remove_dir_all(&dir);
}

/// I8: index events (file mtime, ffprobe creation_time) are timed from
/// `db/videos.jsonl`, which no audit chain covers: `time_unaudited: true`.
#[test]
fn r15_i8_index_event_times_are_unaudited() {
    let dir = r15_case("i8");
    fs::write(
        dir.join("db/videos.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({"id":"vid_a","source_path":"/e/a.mp4","modified_unix":1_700_000_000u64,
                "stale_since_unix":1_700_000_100u64})
        ),
    )
    .unwrap();
    let output = dir.join("db/timeline.jsonl");
    with_test_keys(&[], || {
        crate::timeline::generate_timeline(&dir, &output).unwrap();
    });
    let events: Vec<serde_json::Value> = fs::read_to_string(&output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect();
    let index: Vec<&serde_json::Value> = events
        .iter()
        .filter(|event| event["source"] == "index")
        .collect();
    assert_eq!(index.len(), 2, "{events:?}");
    assert!(
        index
            .iter()
            .all(|event| event["time_unaudited"] == serde_json::Value::Bool(true)),
        "{events:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// I9: every `FRAMETRACE_*` variable name the source names (a whole
/// string literal) is either in `APP_ENV_VARS` (refused as a key source)
/// or a documented key-MATERIAL / test key-source name. A new setting
/// variable read anywhere without being listed fails here.
#[test]
fn r15_i9_every_frametrace_variable_is_classified() {
    // Key material (allowed as `env:` key sources) and the test-only names
    // the key-source tests themselves use.
    const KEY_MATERIAL: &[&str] = &[
        "FRAMETRACE_AUDIT_KEY",
        "FRAMETRACE_AUDIT_KEY_SECRET",
        "FRAMETRACE_CASE_KEY",
        "FRAMETRACE_TEST_KEY_SOURCE",
        "FRAMETRACE_TEST_KEY_SOURCE_MISSING",
        "FRAMETRACE_R13_TEST_MATERIAL",
    ];
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for sub in ["src", "tests"] {
        if root.join(sub).is_dir() {
            walk(&root.join(sub), &mut files);
        }
    }
    if root.join("build.rs").is_file() {
        files.push(root.join("build.rs"));
    }
    let mut unlisted = std::collections::BTreeSet::new();
    let mut seen = 0usize;
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        let mut rest = text.as_str();
        while let Some(at) = rest.find("\"FRAMETRACE_") {
            let tail = &rest[at + 1..];
            let len = tail
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(tail.len());
            let name = &tail[..len];
            // A whole string literal naming a variable (not the bare
            // "FRAMETRACE_" prefix, nor text inside a longer literal).
            if tail[len..].starts_with('"') && name.len() > "FRAMETRACE_".len() {
                seen += 1;
                if !crate::audit_key::APP_ENV_VARS.contains(&name) && !KEY_MATERIAL.contains(&name)
                {
                    unlisted.insert(format!("{name} ({})", file.display()));
                }
            }
            rest = &tail[len..];
        }
    }
    assert!(seen > 20, "the scan found only {seen} names");
    assert!(
        unlisted.is_empty(),
        "FRAMETRACE_* variables not in audit_key::APP_ENV_VARS (add them, or classify them as key material here): {unlisted:?}"
    );
    for name in crate::audit_key::APP_ENV_VARS {
        assert!(
            crate::audit_key::key_source_var_refusal(name).is_some(),
            "{name}"
        );
    }
}
