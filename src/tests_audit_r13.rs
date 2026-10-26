//! R13 track I tests: P16 "a usable interrupted run after the result run
//! withholds the result, and its mismatch rows are shown" (I1), the common
//! "repaired after" rule (I2), and the rest of the R13 trust rows. See
//! `docs/PLAN_R13_2026-10-16.md` "§I13 as implemented" for which were run
//! on the R12 code (a4fb793).

use super::append_chained_jsonl_keyed;
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use std::fs;
use std::path::{Path, PathBuf};

fn r13_case(name: &str, case_id: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r13-{name}-{}", std::process::id()));
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

/// Deletes the last line of `lane` — no key, no forging: the chain that
/// remains verifies.
fn delete_last_line(lane: &Path) {
    let text = fs::read_to_string(lane).unwrap();
    let body = text.strip_suffix('\n').unwrap();
    let keep = body.rfind('\n').map_or(0, |at| at + 1);
    fs::write(lane, &text[..keep]).unwrap();
}

/// The custody statement's green section-5 "everything matched" banner.
const ALL_MATCH_OK: &str = "<p class=\"banner ok\">All ";

/// I1 (Medium, the R12 reviewer's PoC): R1 (match) and marker, R2
/// (mismatch) and marker; deleting the LAST line (R2's marker) needs no key. R12 then
/// read R2 as merely "interrupted" and R1 came back as the result —
/// `run_unix:100, mismatched:0, verified:true, is_result:true` — with the
/// green "All … equal" custody banner, while R2's authenticated MISMATCH
/// row sat in the lane. P16: a usable interrupted run after the result
/// run's marker withholds the result, and its mismatch rows are shown.
#[test]
fn r13_i1_deleted_newest_marker_withholds_the_older_green_run() {
    let dir = r13_case("i1", "C-i1");
    let mine = key(0xd1);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(100, "100-aaaa", "vid_a", "match"),
            hv_done(100, "100-aaaa", 1),
            hv(200, "200-bbbb", "vid_a", "mismatch"),
            hv_done(200, "200-bbbb", 1),
        ],
        &mine,
    );
    delete_last_line(&lane);
    let rows = super::read_lane_rows_with_keys(&lane, std::slice::from_ref(&mine)).unwrap();
    assert!(rows.failure.is_none(), "{:?}", rows.failure);
    assert_eq!(rows.unusable().count(), 0, "every remaining line is usable");
    with_test_keys(std::slice::from_ref(&mine), || {
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(!html.contains(ALL_MATCH_OK), "green banner:\n{html}");
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(
            !summary.is_result,
            "the older all-match run came back as the result: {summary:?}"
        );
        assert!(!summary.verified, "{summary:?}");
        assert_ne!(summary.run_unix, 100, "{summary:?}");
        assert!(crate::anomaly::latest_hash_verify_run(&dir).is_none());
        // P16: the reason, and the newer run's rows — surfaced, not hidden.
        let reason = summary.withheld_reason.clone().unwrap_or_default();
        assert!(
            reason.starts_with(crate::anomaly::NEWER_UNMARKED_RUN_REASON),
            "{summary:?}"
        );
        assert!(
            summary
                .not_verified_reason
                .as_deref()
                .is_some_and(|r| r.contains(crate::anomaly::NEWER_UNMARKED_RUN_REASON)),
            "{summary:?}"
        );
        assert_eq!(
            (
                summary.newer_runs,
                summary.newer_records,
                summary.newer_mismatched
            ),
            (1, 1, 1),
            "{summary:?}"
        );
        assert_eq!(summary.newer_mismatches[0].run_id, "200-bbbb");
        assert_eq!(summary.newer_mismatches[0].id, "vid_a");
        assert_eq!(
            summary.newer_mismatches[0].observed_sha256.as_deref(),
            Some("b".repeat(64).as_str())
        );
        assert_eq!((summary.matched, summary.mismatched), (0, 0));
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(!html.contains(ALL_MATCH_OK), "green banner:\n{html}");
        assert!(html.contains("No current re-verification result"), "{html}");
        // The newer run's mismatch is shown, never hidden.
        assert!(html.contains("MISMATCH"), "{html}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I3: R12's deny-list let `DYLD_INSERT_LIBRARIES` through as a key
/// source (measured) — and adopting it removed it from the environment.
/// Now an allow-list: `FRAMETRACE_*`, `*_SECRET`, `*_KEY`; loader
/// variables (`DYLD_*`, `LD_*`) and system names refused, never adopted.
#[test]
fn r13_i3_key_source_variables_are_an_allow_list() {
    use crate::audit_key::{key_source_env_vars, key_source_var_refusal};
    for allowed in [
        "FRAMETRACE_AUDIT_KEY_SECRET",
        "frametrace_custom",
        "CASE_AUDIT_KEY",
        "vault_secret",
        "FT_R4_KEY",
    ] {
        assert_eq!(key_source_var_refusal(allowed), None, "{allowed}");
    }
    for refused in [
        "DYLD_INSERT_LIBRARIES",
        "dyld_framework_path",
        "LD_PRELOAD",
        "LD_AUDIT",
        "LD_SOMETHING_KEY",
        "DYLD_SECRET",
        "PATH",
        "HOME",
        "FRAMETRACE_KEY_SOURCE",
        "FRAMETRACE_AUDIT_KEY_ID",
        "MY_TOKEN",
        "KEY",
        "PROGRAMFILES(X86)",
        "A-B_KEY",
        "",
    ] {
        assert!(key_source_var_refusal(refused).is_some(), "{refused}");
    }
    let vars = key_source_env_vars(
        [
            "--key-source",
            "env:DYLD_INSERT_LIBRARIES",
            "verify-audit",
            "x",
        ]
        .map(std::ffi::OsString::from),
        Some("env:LD_PRELOAD".into()),
    );
    assert!(vars.is_empty(), "{vars:?}");
    let err = crate::audit_key::install_source_override("env:DYLD_INSERT_LIBRARIES").unwrap_err();
    assert!(err.contains("refused") && err.contains("DYLD_"), "{err}");
}

/// I4: an OLD carve-lane entry (no `carved_unix`) took its timeline time
/// from `db/carve_results.json` — outside every audit lane — and the
/// event read as fully audited. Now its detail says the time is not
/// audited; an entry with its own time carries no such note.
#[test]
fn r13_i4_carve_time_from_the_snapshot_is_marked_unaudited() {
    let dir = r13_case("i4", "C-i4");
    let mine = key(0xd4);
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
        .filter(|event| event["source"] == "carve")
        .collect();
    let detail = |path: &str| -> Vec<String> {
        events
            .iter()
            .filter(|event| event["path"] == path)
            .map(|event| event["detail"].as_str().unwrap().to_string())
            .collect()
    };
    let old = detail("/c/old.mp4");
    assert_eq!(old.len(), 1, "{events:?}");
    assert!(old[0].contains("시각 감사 안 됨"), "{old:?}");
    let new = detail("/c/new.mp4");
    assert_eq!(new.len(), 1, "{events:?}");
    assert!(!new[0].contains("시각 감사 안 됨"), "{new:?}");
    let _ = fs::remove_dir_all(&dir);
}

/// Tears the last line of `lane` (keeps the first third of it, no newline).
fn tear_last_line(lane: &Path) {
    let text = fs::read_to_string(lane).unwrap();
    let body = text.strip_suffix('\n').unwrap();
    let start = body.rfind('\n').map_or(0, |at| at + 1);
    let torn = &body[start..start + (body.len() - start) / 3];
    fs::write(lane, format!("{}{torn}", &body[..start])).unwrap();
}

/// I5: the `.removed` archive `repair-lane` keeps was never checked — it
/// could be deleted or rewritten unnoticed. Now every read hashes it
/// against the marker: `match`, `missing`, `differs` (chain object
/// `repairs[].removed_archive` and lane-level `removed_archive`, and the
/// detail sentence).
#[test]
fn r13_i5_removed_archive_is_checked_against_the_marker() {
    let dir = r13_case("i5", "C-i5");
    let mine = key(0xd5);
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    let entry = |n: u32| serde_json::json!({"event":"validate","n":n}).to_string();
    write_lane(&lane, &[entry(1), entry(2)], &mine);
    tear_last_line(&lane);
    with_test_keys(std::slice::from_ref(&mine), || {
        let done = super::repair_lane(&lane, false).unwrap();
        let saved = done.removed_saved_to.clone().unwrap();
        let state = || {
            let rows = super::read_lane_rows(&lane).unwrap();
            let chain = rows.chain_json("evidence/logs/validation-log.jsonl");
            assert_eq!(
                chain["repairs"][0]["removed_archive"],
                chain["removed_archive"]
            );
            (
                chain["removed_archive"].as_str().unwrap().to_string(),
                rows.detail(),
            )
        };
        let (archive, detail) = state();
        assert_eq!(archive, "match", "{detail}");
        assert!(detail.contains("보관 파일 일치"), "{detail}");
        // Rewritten (same size, other bytes).
        let mut bytes = fs::read(&saved).unwrap();
        bytes[0] ^= 0x01;
        fs::write(&saved, &bytes).unwrap();
        let (archive, detail) = state();
        assert_eq!(archive, "differs", "{detail}");
        assert!(detail.contains("보관 파일이 다름"), "{detail}");
        // Deleted.
        fs::remove_file(&saved).unwrap();
        let (archive, detail) = state();
        assert_eq!(archive, "missing", "{detail}");
        assert!(detail.contains("보관 파일 없음"), "{detail}");
        // A lane without a repair has no archive state.
        let other = dir.join("evidence/logs/other-log.jsonl");
        write_lane(&other, &[entry(3)], &mine);
        let chain = super::read_lane_rows(&other)
            .unwrap()
            .chain_json("evidence/logs/other-log.jsonl");
        assert!(chain["removed_archive"].is_null(), "{chain}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I5: a marker naming anything but a plain file name beside the lane
/// (a path elsewhere) is never followed — "missing".
#[test]
fn r13_i5_removed_archive_outside_the_lane_folder_is_not_followed() {
    let dir = r13_case("i5-path", "C-i5p");
    let mine = key(0xd6);
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    let outside = dir.join("outside.removed");
    fs::write(&outside, b"x").unwrap();
    write_lane(
        &lane,
        &[
            serde_json::json!({"event":"lane-repair","kept_entries":0,"removed_lines":1,
            "removed_bytes":1,"removed_sha256":crate::sha256::digest_bytes(b"x"),
            "removed_saved_as":"../../outside.removed"})
            .to_string(),
        ],
        &mine,
    );
    with_test_keys(std::slice::from_ref(&mine), || {
        let rows = super::read_lane_rows(&lane).unwrap();
        assert_eq!(rows.repairs.len(), 1);
        assert_eq!(rows.repairs[0].removed_archive.as_str(), "missing");
    });
    let _ = fs::remove_dir_all(&dir);
}
