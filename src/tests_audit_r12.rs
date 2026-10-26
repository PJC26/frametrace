//! R12 track I tests: P12 "an unusable line's content is never used for a
//! decision — only its position" (I1), and the rest of the R12 trust rows.
//! See `docs/PLAN_R12_2026-10-16.md` "§I12 as implemented" for which were
//! run on the R11 code (632f557).

use super::append_chained_jsonl_keyed;
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use std::fs;
use std::path::{Path, PathBuf};

fn r12_case(name: &str, case_id: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r12-{name}-{}", std::process::id()));
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

/// The R11 reviewer's PoC: replaces the LAST line with an UNSIGNED line
/// that keeps its `previous_entry_sha256` and recomputes `entry_sha256`
/// (the chain stays structurally intact), after `edit` rewrites the bytes
/// the entry hash covers.
fn forge_last_line(lane: &Path, edit: impl Fn(&str) -> String) {
    let text = fs::read_to_string(lane).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let last = lines.pop().unwrap();
    let cut = last.find(",\"entry_sha256\":").unwrap();
    let covered = edit(&format!("{}}}", &last[..cut]));
    let entry = crate::sha256::digest_bytes(covered.as_bytes());
    lines.push(format!(
        "{},\"entry_sha256\":\"{entry}\"}}",
        &covered[..covered.len() - 1]
    ));
    fs::write(lane, format!("{}\n", lines.join("\n"))).unwrap();
}

/// The custody statement's green section-5 "everything matched" banner.
const ALL_MATCH_OK: &str = "<p class=\"banner ok\">All ";

fn assert_withheld(dir: &Path, mine: &AuditKey, what: &str) {
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let rows = super::read_lane_rows_with_keys(&lane, std::slice::from_ref(mine)).unwrap();
    assert!(rows.failure.is_none(), "{what}: {:?}", rows.failure);
    with_test_keys(std::slice::from_ref(mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(dir).unwrap();
        assert!(
            !summary.is_result,
            "{what}: the older all-match run came back as the result: {summary:?}"
        );
        assert_ne!(summary.run_unix, 100, "{what}: {summary:?}");
        assert!(!summary.verified, "{what}: {summary:?}");
        assert!(summary.unauthenticated >= 1, "{what}: {summary:?}");
        assert!(
            crate::anomaly::latest_hash_verify_run(dir).is_none(),
            "{what}"
        );
        let html = crate::custody::render_statement(dir, false).unwrap();
        assert!(
            !html.contains(ALL_MATCH_OK),
            "{what}: green banner:\n{html}"
        );
        assert!(
            html.contains("No current re-verification result"),
            "{what}: {html}"
        );
    });
}

/// I1 (High) variant A: the newest run's completion marker (the last line)
/// is replaced by an unsigned line whose `event` is "x-note" — R11's
/// `runs_from_lane` skipped it as "not a hash-verify entry", so the newer
/// run looked merely interrupted and the older all-match run was the
/// result. P12: an unusable line after the result run's marker withholds
/// the result, whatever it says.
#[test]
fn r12_i1_a_unusable_tail_with_another_event_withholds_the_result() {
    let dir = r12_case("i1a", "C-i1a");
    let mine = key(0xc1);
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
    forge_last_line(&lane, |covered| {
        covered.replace("\"event\":\"hash-verify-run\"", "\"event\":\"x-note\"")
    });
    assert_withheld(&dir, &mine, "variant A (event x-note)");
    let _ = fs::remove_dir_all(&dir);
}

/// I1 (High) variant B: the newest run's completion marker is replaced by
/// an unsigned line naming an OLDER run's id (R0) — R11 counted it as R0's
/// unusable entry, and since R0 sorts before the completed R1, the run
/// index comparison let R1 stand as the result. P12: position only.
#[test]
fn r12_i1_b_unusable_tail_naming_an_older_run_withholds_the_result() {
    let dir = r12_case("i1b", "C-i1b");
    let mine = key(0xc2);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(50, "50-0000", "vid_a", "match"),
            hv_done(50, "50-0000", 1),
            hv(100, "100-aaaa", "vid_a", "match"),
            hv_done(100, "100-aaaa", 1),
            hv(200, "200-bbbb", "vid_a", "mismatch"),
            hv_done(200, "200-bbbb", 1),
        ],
        &mine,
    );
    forge_last_line(&lane, |covered| {
        covered
            .replace("\"run_id\":\"200-bbbb\"", "\"run_id\":\"50-0000\"")
            .replace("\"run_unix\":200", "\"run_unix\":50")
    });
    assert_withheld(&dir, &mine, "variant B (run_id of R0)");
    let _ = fs::remove_dir_all(&dir);
}

/// I1 (P12): a tail line that is not even JSON (unusable, `value` Null) —
/// the content of an unusable line is never read, so this withholds too.
#[test]
fn r12_i1_unparseable_unusable_tail_withholds_the_result() {
    let dir = r12_case("i1c", "C-i1c");
    let mine = key(0xc3);
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
    // Keeps the chain fields but makes the covered bytes a JSON object
    // with nothing but junk keys.
    forge_last_line(&lane, |covered| {
        let at = covered.find(",\"previous_entry_sha256\"").unwrap();
        format!("{{\"zz\":1{}", &covered[at..])
    });
    assert_withheld(&dir, &mine, "variant C (junk object)");
    let _ = fs::remove_dir_all(&dir);
}

/// P12 does not over-reach: a lane whose only unusable lines sit BEFORE
/// the result run (another PC's older run) still has a result — and a
/// pristine lane is verified.
#[test]
fn r12_i1_unusable_lines_before_the_result_run_do_not_withhold() {
    let dir = r12_case("i1d", "C-i1d");
    let mine = key(0xc4);
    let other = key(0xc5);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(50, "50-0000", "vid_a", "mismatch"),
            hv_done(50, "50-0000", 1),
        ],
        &other,
    );
    write_lane(
        &lane,
        &[
            hv(100, "100-aaaa", "vid_a", "match"),
            hv_done(100, "100-aaaa", 1),
        ],
        &mine,
    );
    with_test_keys(std::slice::from_ref(&mine), || {
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(summary.is_result, "{summary:?}");
        assert_eq!(summary.run_unix, 100);
        assert_eq!((summary.matched, summary.mismatched), (1, 0));
        assert_eq!(summary.unauthenticated, 0, "{summary:?}");
    });
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

/// I3: a torn final line made the whole lane unusable with no way back.
/// `repair-lane` cuts it to the verifying prefix, keeps the cut bytes
/// beside the lane, signs a `lane-repair` marker with their SHA-256, and
/// the lane reads as "수리됨" — its prefix usable again, appends work.
#[test]
fn r12_i3_repair_lane_cuts_to_the_verified_prefix_and_records_it() {
    let dir = r12_case("i3", "C-i3");
    let mine = key(0xc6);
    fs::create_dir_all(dir.join("artifacts/carved")).unwrap();
    let real = dir.join("artifacts/carved/real.mp4");
    fs::write(&real, b"x").unwrap();
    let lane = dir.join("artifacts/carved/carve-log.jsonl");
    let entry = |id: &str| {
        serde_json::json!({"event":"carve-file","id":id,"output_path":real.to_string_lossy(),"sha256":"a".repeat(64)})
            .to_string()
    };
    write_lane(&lane, &[entry("c1"), entry("c2")], &mine);
    tear_last_line(&lane);
    let before = fs::read(&lane).unwrap();
    with_test_keys(std::slice::from_ref(&mine), || {
        let rows = super::read_lane_rows(&lane).unwrap();
        assert!(rows.failure.is_some());
        assert_eq!(rows.usable().count(), 0);
        assert!(
            rows.detail().contains("repair-lane"),
            "the failure names the command: {}",
            rows.detail()
        );
        // Appending is refused with the same guidance.
        let err = append_chained_jsonl_keyed(&lane, &entry("c3"), Some(&mine)).unwrap_err();
        assert!(err.contains("frametrace repair-lane"), "{err}");
        // Dry run: reports, changes nothing.
        let dry = super::repair_lane(&lane, true).unwrap();
        assert!(dry.needed && dry.removed_saved_to.is_none());
        assert_eq!((dry.kept_entries, dry.removed_lines), (1, 1));
        assert_eq!(fs::read(&lane).unwrap(), before);
        // Repair.
        let done = super::repair_lane(&lane, false).unwrap();
        assert!(done.keyed);
        let saved = done.removed_saved_to.clone().unwrap();
        let cut = fs::read(&saved).unwrap();
        assert_eq!(cut, before[done.kept_bytes as usize..].to_vec());
        assert_eq!(done.removed_sha256, crate::sha256::digest_bytes(&cut));
        let after = fs::read(&lane).unwrap();
        assert_eq!(
            after[..done.kept_bytes as usize],
            before[..done.kept_bytes as usize]
        );
        let rows = super::read_lane_rows(&lane).unwrap();
        assert!(rows.failure.is_none(), "{:?}", rows.failure);
        assert_eq!(rows.lane.status(), "keyed", "{}", rows.detail());
        assert_eq!(rows.repairs.len(), 1);
        assert_eq!(rows.repairs[0].removed_sha256, done.removed_sha256);
        assert!(
            rows.detail().contains(super::LANE_REPAIRED_NOTE),
            "{}",
            rows.detail()
        );
        let chain = rows.chain_json("artifacts/carved/carve-log.jsonl");
        assert_eq!(chain["repaired"], true, "{chain}");
        // Track U contract: `logChains[*].status === "repaired"`.
        assert_eq!(chain["status"], "repaired", "{chain}");
        assert_eq!(chain["base_status"], "keyed", "{chain}");
        assert_eq!(chain["unauthenticated"], 0, "{chain}");
        assert_eq!(
            chain["repairs"][0]["removed_sha256"],
            done.removed_sha256.as_str()
        );
        // The prefix is usable again; the lane takes appends.
        assert_eq!(
            crate::report::evidence_hashes(&dir).unwrap().get("c1"),
            Some(&"a".repeat(64))
        );
        append_chained_jsonl_keyed(&lane, &entry("c3"), Some(&mine)).unwrap();
        assert!(super::read_lane_rows(&lane).unwrap().failure.is_none());
        // A verifying lane: nothing to repair, nothing written.
        let again = super::repair_lane(&lane, false).unwrap();
        assert!(!again.needed);
        // Never listed as a lane by the case-wide check.
        assert!(
            !super::case_lane_paths(&dir)
                .iter()
                .any(|path| path.to_string_lossy().ends_with(".removed"))
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I3 + P12: repairing the re-hash lane cuts a tail that may have held a
/// newer run — the older completed run is not the result afterwards.
#[test]
fn r12_i3_repaired_hash_verify_lane_withholds_the_older_result() {
    let dir = r12_case("i3-hv", "C-i3hv");
    let mine = key(0xc7);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(100, "100-aaaa", "vid_a", "match"),
            hv_done(100, "100-aaaa", 1),
            hv(200, "200-bbbb", "vid_a", "mismatch"),
        ],
        &mine,
    );
    tear_last_line(&lane);
    with_test_keys(std::slice::from_ref(&mine), || {
        super::repair_lane(&lane, false).unwrap();
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.is_result, "{summary:?}");
        assert!(
            summary
                .not_verified_reason
                .as_deref()
                .is_some_and(|r| r.contains("lane-repair")),
            "{summary:?}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I3: a sound-measurement lane another lane already continues is not
/// repaired (its continuation marker checks it by hash).
#[test]
fn r12_i3_continued_sound_lane_is_not_repaired() {
    let dir = r12_case("i3-sound", "C-i3s");
    let first = dir.join("evidence/logs/audio-level-log.jsonl");
    let next = dir.join("evidence/logs/audio-level-log.1.jsonl");
    super::test_write_lane(&first, "{\"event\":\"x\"}\n{\"event\":\"y\"}\n");
    tear_last_line(&first);
    super::test_write_lane(&next, "{\"event\":\"audio-level-lane-continued\"}\n");
    with_test_keys(&[], || {
        let err = super::repair_lane(&first, false).unwrap_err();
        assert!(err.contains("이어졌습니다"), "{err}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I2: a carve snapshot artifact (db/carve_results.json, outside the
/// chain) whose lane entry cannot be authenticated came back as a plain
/// `carved-candidate`. Now: unaudited unless a usable lane entry names it;
/// a broken lane makes the snapshot unused.
#[test]
fn r12_i2_carve_snapshot_is_never_a_candidate_without_a_usable_entry() {
    let dir = r12_case("i2", "C-i2");
    let mine = key(0xc8);
    fs::create_dir_all(dir.join("artifacts/carved")).unwrap();
    let lane = dir.join("artifacts/carved/carve-log.jsonl");
    let entry = |id: &str, out: &str| {
        serde_json::json!({"event":"carve-file","id":id,"output_path":out,"source_path":"/ev/i.raw","offset":1,"carved_unix":150})
            .to_string()
    };
    write_lane(&lane, &[entry("c1", "/c/one.mp4")], &mine);
    append_chained_jsonl_keyed(&lane, &entry("c2", "/c/two.mp4"), None).unwrap();
    fs::write(
        dir.join("db/carve_results.json"),
        "{\"carved_unix\":160,\"artifacts\":[{\"id\":\"c1\",\"output_path\":\"/c/one.mp4\",\"source_path\":\"/ev/i.raw\",\"offset\":1},{\"id\":\"c2\",\"output_path\":\"/c/two.mp4\",\"source_path\":\"/ev/i.raw\",\"offset\":2}]}",
    )
    .unwrap();
    let output = dir.join("db/timeline.jsonl");
    let kinds_for = |path: &str| -> Vec<String> {
        fs::read_to_string(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|event| event["path"] == path && event["source"] == "carve")
            .map(|event| event["kind"].as_str().unwrap().to_string())
            .collect()
    };
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::timeline::generate_timeline(&dir, &output).unwrap();
        assert_eq!(kinds_for("/c/one.mp4"), vec!["carved-candidate"]);
        let two = kinds_for("/c/two.mp4");
        assert!(!two.iter().any(|k| k == "carved-candidate"), "{two:?}");
        assert!(
            two.iter()
                .any(|k| k == crate::timeline::CARVED_UNAUDITED_KIND),
            "{two:?}"
        );
    });
    // Broken lane: the snapshot is not used at all.
    tear_last_line(&lane);
    with_test_keys(std::slice::from_ref(&mine), || {
        crate::timeline::generate_timeline(&dir, &output).unwrap();
        for path in ["/c/one.mp4", "/c/two.mp4"] {
            let kinds = kinds_for(path);
            assert!(
                !kinds.iter().any(|k| k == "carved-candidate"
                    || k == crate::timeline::CARVED_UNAUDITED_KIND),
                "{path}: {kinds:?}"
            );
        }
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I5: `--key-source env:PATH` (or `FRAMETRACE_KEY_SOURCE=env:SystemRoot`)
/// adopted — removed — a system variable. Refused, never adopted.
#[test]
fn r12_i5_system_variables_are_never_key_sources() {
    use crate::audit_key::{is_system_env_var, key_source_env_vars};
    for name in [
        "PATH",
        "Path",
        "SystemRoot",
        "TEMP",
        "APPDATA",
        "HOME",
        "LC_ALL",
        "XDG_CONFIG_HOME",
    ] {
        assert!(is_system_env_var(name), "{name}");
    }
    assert!(!is_system_env_var("FRAMETRACE_AUDIT_KEY_SECRET"));
    let vars = key_source_env_vars(
        ["--key-source", "env:PATH", "verify-audit", "x"].map(std::ffi::OsString::from),
        Some("env:SystemRoot".into()),
    );
    assert!(vars.is_empty(), "{vars:?}");
    let vars = key_source_env_vars(
        ["--key-source=env:MY_KEY"].map(std::ffi::OsString::from),
        None,
    );
    assert_eq!(vars, vec!["MY_KEY".to_string()]);
    let err = crate::audit_key::install_source_override("env:PATH").unwrap_err();
    assert!(err.contains("system environment variable"), "{err}");
    assert!(std::env::var_os("PATH").is_some());
}
