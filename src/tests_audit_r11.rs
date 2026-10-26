//! R11 track I tests: "a missing row is not good news" (P9: I1, I2, I4),
//! secret adoption from `--key-source env:VAR` (P10: I3), and the rest of
//! the R11 trust rows. Each test fails on the R10 code (0c2da16) — see
//! `docs/PLAN_R11_2026-10-16.md` "§I11 as implemented" for which were run
//! there.

use super::{append_chained_jsonl_keyed, read_lane_rows_with_keys};
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use std::fs;
use std::path::{Path, PathBuf};

fn r11_case(name: &str, case_id: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r11-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("evidence/logs")).unwrap();
    fs::create_dir_all(dir.join("artifacts/carved")).unwrap();
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

/// Strips the HMAC fields from line `index` (0-based) of the lane — the
/// chain stays structurally intact (`entry_sha256` does not cover them).
fn strip_hmac(lane: &Path, index: usize) {
    let text = fs::read_to_string(lane).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let cut = lines[index].find(",\"entry_hmac_sha256\"").unwrap();
    lines[index] = format!("{}}}", &lines[index][..cut]);
    fs::write(lane, format!("{}\n", lines.join("\n"))).unwrap();
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

/// The custody statement's green section-5 "everything matched" banner.
const ALL_MATCH_OK: &str = "<p class=\"banner ok\">All ";

/// I1 (High): on a PC holding audit keys, stripping the HMAC of ONE row
/// made the custody statement green. The row is the LAST line (a newer,
/// interrupted run's record — so the chain stays intact: only a line's
/// successor covers its bytes), naming the record that MISMATCHED in the
/// newest completed run. R10's P7 marked that mismatch as superseded by
/// the newer unusable row, dropped it from the run, counted the rest — all
/// matches — and printed "All N re-hashed live and equal". P9: a run with
/// an unusable row (or a newer run holding one) is never "all match".
#[test]
fn r11_i1_stripped_row_never_yields_all_match() {
    let dir = r11_case("i1", "C-i1");
    let mine = key(0xb1);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(100, "100-aaaa", "vid_a", "match"),
            hv(100, "100-aaaa", "vid_b", "mismatch"),
            hv(100, "100-aaaa", "vid_c", "match"),
            hv_done(100, "100-aaaa", 3),
            // A newer run cut short after its first record.
            hv(200, "200-bbbb", "vid_b", "mismatch"),
        ],
        &mine,
    );
    with_test_keys(std::slice::from_ref(&mine), || {
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(html.contains(">MISMATCH<"), "pristine: the mismatch shows");
    });
    strip_hmac(&lane, 4);
    // The chain still verifies: only the stripped row is unusable.
    let rows = read_lane_rows_with_keys(&lane, std::slice::from_ref(&mine)).unwrap();
    assert!(rows.failure.is_none(), "{:?}", rows.failure);
    with_test_keys(std::slice::from_ref(&mine), || {
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(
            !html.contains(ALL_MATCH_OK),
            "a stripped row turned the statement green:\n{html}"
        );
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.verified && !summary.is_result, "{summary:?}");
        assert!(summary.unauthenticated >= 1, "{summary:?}");
        assert!(html.contains("No current re-verification result"), "{html}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I1: the marker's `records` count is compared with the usable records —
/// a run whose marker says more records than there are is not verified
/// even when every present row is authenticated (rows deleted in the
/// middle break the chain; this guards a marker written by a run whose
/// records were lost, and documents the rule).
#[test]
fn r11_i1_marker_record_count_must_match_usable_records() {
    let dir = r11_case("i1-count", "C-i1c");
    let mine = key(0xb2);
    let lane = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    write_lane(
        &lane,
        &[
            hv(100, "100-aaaa", "vid_a", "match"),
            hv_done(100, "100-aaaa", 2),
        ],
        &mine,
    );
    with_test_keys(std::slice::from_ref(&mine), || {
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(!html.contains(ALL_MATCH_OK), "{html}");
        let summary = crate::anomaly::latest_hash_verify_summary(&dir).unwrap();
        assert!(!summary.verified, "{summary:?}");
        assert!(summary.is_result, "{summary:?}");
        assert!(
            summary
                .not_verified_reason
                .as_deref()
                .is_some_and(|r| r.contains("기록 수(2)")),
            "{summary:?}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I2 (High): stripping the HMAC of the newest run's completion marker
/// made R10 treat that run as "interrupted" and roll the result back to
/// the previous completed run (all matches → green banner, API summary of
/// the OLD run). P9: a newer run holding an unusable entry means there is
/// no current result.
#[test]
fn r11_i2_stripped_completion_marker_never_rolls_back_to_an_older_run() {
    let dir = r11_case("i2", "C-i2");
    let mine = key(0xb3);
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
    strip_hmac(&lane, 3);
    with_test_keys(std::slice::from_ref(&mine), || {
        let html = crate::custody::render_statement(&dir, false).unwrap();
        assert!(
            !html.contains(ALL_MATCH_OK),
            "rolled back to the older run's all-match result:\n{html}"
        );
        assert!(
            !html.contains("Latest completed live re-hash run at 1970-01-01 00:01:40"),
            "the older run is presented as the latest result:\n{html}"
        );
        // The API summary is not the old run's result.
        let summary = crate::anomaly::latest_hash_verify_summary(&dir);
        assert!(
            summary.as_ref().is_none_or(|s| s.run_unix != 100),
            "{summary:?}"
        );
        let summary = summary.unwrap();
        assert!(!summary.is_result && !summary.verified, "{summary:?}");
        assert_eq!((summary.matched, summary.mismatched), (0, 0));
        assert!(summary.unauthenticated >= 1, "{summary:?}");
        assert!(crate::anomaly::latest_hash_verify_run(&dir).is_none());
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I3 (P10): the variable a `--key-source env:VAR` spec names is raw key
/// material: adopted at process start like the fixed key variables —
/// removed from the environment (a tool the process runs does not inherit
/// it), still the key this process signs with, and handed back only to
/// FrameTrace's own step children. R10 left it in the environment.
#[test]
fn r11_i3_key_source_env_var_is_not_inherited_by_tools() {
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "audit::tests_r11::r11_i3_helper",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FT_R11_KEYSRC_KEY", "22".repeat(32))
        .env("FRAMETRACE_KEY_SOURCE", "env:FT_R11_KEYSRC_KEY")
        .env("FT_R11_I3_HELPER", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("INSTALLED=ok"), "{stdout}");
    assert!(
        stdout.contains(&format!("SIGNS={}", fingerprint_id(&[0x22; 32]))),
        "{stdout}"
    );
    assert!(
        stdout.contains("TOOL=[]") || stdout.contains("TOOL=[%FT_R11_KEYSRC_KEY%]"),
        "a tool inherited the --key-source variable: {stdout}"
    );
    assert!(
        stdout.contains(&format!("STEP=[{}]", "22".repeat(32))),
        "{stdout}"
    );
}

/// Helper process for [`r11_i3_key_source_env_var_is_not_inherited_by_tools`].
#[test]
#[ignore]
fn r11_i3_helper() {
    if std::env::var_os("FT_R11_I3_HELPER").is_none() {
        return;
    }
    // SAFETY: this helper runs alone in its process (--exact,
    // --test-threads=1); nothing else touches the environment.
    unsafe { crate::audit_key::adopt_key_env() };
    // The workstation installs its FRAMETRACE_KEY_SOURCE spec.
    let installed = crate::audit_key::install_source_override("env:FT_R11_KEYSRC_KEY");
    println!(
        "INSTALLED={}",
        if installed.is_ok() { "ok" } else { "failed" }
    );
    if let Ok(Some(key)) = crate::audit_key::configured() {
        println!("SIGNS={}", fingerprint_id(&key.bytes));
    }
    let echo = |forward: bool| {
        #[cfg(windows)]
        let mut command = {
            let mut command = std::process::Command::new("cmd");
            command.args(["/C", "echo [%FT_R11_KEYSRC_KEY%]"]);
            command
        };
        #[cfg(unix)]
        let mut command = {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", "echo \"[$FT_R11_KEYSRC_KEY]\""]);
            command
        };
        if forward {
            crate::audit_key::forward_adopted_key_env(&mut command);
        }
        String::from_utf8_lossy(&command.output().unwrap().stdout)
            .trim()
            .to_string()
    };
    println!("TOOL={}", echo(false));
    println!("STEP={}", echo(true));
}

/// I3: which variables a key-source spec names — the `--key-source` flag
/// in either spelling (a step child gets it from the workstation), the
/// workstation's `FRAMETRACE_KEY_SOURCE`, never a `file:` path or an
/// argument after `--`.
#[test]
fn r11_i3_key_source_env_vars_from_args_and_ambient() {
    use crate::audit_key::key_source_env_vars;
    let args = |list: &[&str]| -> Vec<std::ffi::OsString> {
        list.iter().map(std::ffi::OsString::from).collect()
    };
    assert_eq!(
        key_source_env_vars(args(&["--key-source", "env:A_KEY", "qa", "x"]), None),
        ["A_KEY"]
    );
    assert_eq!(
        key_source_env_vars(
            args(&["qa", "--key-source=env:B_KEY"]),
            Some("env:C_KEY".into())
        ),
        ["B_KEY", "C_KEY"]
    );
    assert!(key_source_env_vars(args(&["--key-source", "file:/k"]), None).is_empty());
    assert!(key_source_env_vars(args(&["--", "--key-source", "env:D_KEY"]), None).is_empty());
    assert_eq!(
        key_source_env_vars(args(&[]), Some("env:E_KEY".into())),
        ["E_KEY"]
    );
}

/// I4 (P9): a chain break defeated P7 — editing the NEWEST entry of a
/// record broke the chain there, and R10 still used the verified prefix,
/// i.e. the record's previous value (a rollback). Now no entry of a lane
/// whose chain fails is usable, and a sign-off anchored before the break is
/// NOT VERIFIED (R10: Intact).
#[test]
fn r11_i4_chain_break_never_rolls_back_to_the_prefix() {
    use super::{AnchorStatus, check_lane_anchor, lane_head};
    let dir = r11_case("i4", "C-i4");
    let mine = key(0xb4);
    let real = dir.join("artifacts/carved/real.mp4");
    fs::write(&real, b"x").unwrap();
    let lane = dir.join("artifacts/carved/carve-log.jsonl");
    let entry = |sha: &str| {
        serde_json::json!({"event":"carve-file","id":"c1","output_path":real.to_string_lossy(),"sha256":sha.repeat(64)})
            .to_string()
    };
    append_chained_jsonl_keyed(&lane, &entry("a"), Some(&mine)).unwrap();
    let head = lane_head(&lane).unwrap().unwrap();
    append_chained_jsonl_keyed(&lane, &entry("b"), Some(&mine)).unwrap();
    with_test_keys(std::slice::from_ref(&mine), || {
        assert_eq!(
            crate::report::evidence_hashes(&dir).unwrap().get("c1"),
            Some(&"b".repeat(64))
        );
        assert_eq!(check_lane_anchor(&lane, &head), AnchorStatus::Intact);
    });
    // Cut the newest entry to a torn, unparseable line: the chain breaks at
    // line 2, and that line names no record — so R10's P7 had nothing to
    // supersede line 1 with, and the record rolled back to "a".
    let text = fs::read_to_string(&lane).unwrap();
    let first_end = text.find('\n').unwrap() + 1;
    let second = &text[first_end..];
    let edited = format!("{}{}", &text[..first_end], &second[..second.len() / 3]);
    fs::write(&lane, edited).unwrap();
    for keys in [vec![mine.clone()], vec![]] {
        with_test_keys(&keys, || {
            let rows = super::read_lane_rows(&lane).unwrap();
            assert!(rows.failure.is_some());
            assert_eq!(rows.usable().count(), 0, "prefix used after a break");
            assert_eq!(
                crate::report::evidence_hashes(&dir).unwrap().get("c1"),
                None,
                "rolled back to the prefix value"
            );
            assert!(crate::validation::resolve_artifact_pair(&dir, "c1", &real).is_err());
            assert!(
                matches!(
                    check_lane_anchor(&lane, &head),
                    AnchorStatus::NotAuthenticated(_)
                ),
                "{:?}",
                check_lane_anchor(&lane, &head)
            );
        });
    }
    let _ = fs::remove_dir_all(&dir);
}

/// I5: the carve page's run summary (scan complete, candidate limit,
/// source size, warnings) comes from `db/carve_results.json`, outside every
/// audit lane — R10 showed "전체 완료" in the ok colour. Now marked
/// "감사 안 됨", never ok-coloured.
#[test]
fn r11_i5_carve_page_summary_metrics_are_marked_unaudited() {
    let dir = r11_case("i5", "C-i5");
    let log = concat!(
        r#"{"event":"carve-file","id":"c1","output_path":"/a.mp4","sha256":"aa","trust":"auth"}"#,
        "\n"
    );
    let html = crate::html_report::render_carve_report_html(
        "{}",
        log,
        r#"{"scan_complete":true,"source_path":"/img.dd"}"#,
        "[]",
        &dir,
        "{}",
    );
    assert!(
        !html.contains(r#"R.scan_complete ? "ok""#),
        "an unaudited summary is shown in the ok colour"
    );
    assert!(html.contains("감사 안 됨"), "no unaudited marker");
    let _ = fs::remove_dir_all(&dir);
}

/// I6: the timeline compared the `.jsonl` extension case-sensitively, so
/// `validation-log.JSONL` — the validation lane on Windows / macOS (P6) —
/// was never read.
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn r11_i6_timeline_reads_an_upper_case_extension_lane() {
    let dir = r11_case("i6", "C-i6");
    fs::write(
        dir.join("evidence/logs/validation-log.JSONL"),
        "{\"event\":\"validate-artifact\",\"validated_unix\":777,\"target_path\":\"/x/r11-i6.mp4\"}\n",
    )
    .unwrap();
    with_test_keys(&[], || {
        let out = dir.join("db/timeline.jsonl");
        crate::timeline::generate_timeline(&dir, &out).unwrap();
        let text = fs::read_to_string(&out).unwrap();
        assert!(text.contains("r11-i6.mp4"), "lane not read: {text}");
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I7: in a case-sensitive folder (Windows `fsutil file
/// setCaseSensitiveInfo`), a copy spelled `Validation-Log.jsonl` beside
/// the real `validation-log.jsonl` sorted first, was read as THE lane
/// (its bound entries authenticated — the binding names the canonical
/// lane) and hid the real one from the timeline. Now the copy is NOT
/// VERIFIED and the real lane is always read.
#[cfg(windows)]
#[test]
fn r11_i7_differently_spelled_copy_never_hides_the_real_lane() {
    let dir = r11_case("i7", "C-i7");
    let logs = dir.join("evidence/logs");
    let enabled = std::process::Command::new("fsutil.exe")
        .args(["file", "setCaseSensitiveInfo"])
        .arg(&logs)
        .arg("enable")
        .output()
        .is_ok_and(|out| out.status.success());
    if !enabled {
        eprintln!("SKIPPED: fsutil setCaseSensitiveInfo not available here");
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    let mine = key(0xb7);
    let body = |name: &str| {
        serde_json::json!({"event":"validate-artifact","validated_unix":900,"target_path":format!("/x/{name}.mp4")})
            .to_string()
    };
    // The forged copy: a genuine keyed lane of a case with the same id,
    // copied in under another spelling.
    let other = r11_case("i7-src", "C-i7");
    let other_lane = other.join("evidence/logs/validation-log.jsonl");
    append_chained_jsonl_keyed(&other_lane, &body("fake"), Some(&mine)).unwrap();
    let copy_path = logs.join("Validation-Log.jsonl");
    fs::copy(&other_lane, &copy_path).unwrap();
    let real = logs.join("validation-log.jsonl");
    append_chained_jsonl_keyed(&real, &body("real"), Some(&mine)).unwrap();
    assert_eq!(
        fs::read_dir(&logs).unwrap().count(),
        2,
        "two distinct files"
    );
    with_test_keys(std::slice::from_ref(&mine), || {
        let copy = super::read_lane_rows(&copy_path).unwrap();
        assert!(
            copy.rows.iter().all(|row| !row.usable()),
            "copy read as the lane"
        );
        let lanes = super::case_lane_paths(&dir);
        assert!(lanes.contains(&real), "{lanes:?}");
        assert!(lanes.contains(&copy_path), "copy not checked: {lanes:?}");
        let out = dir.join("db/timeline.jsonl");
        crate::timeline::generate_timeline(&dir, &out).unwrap();
        let events: Vec<serde_json::Value> = fs::read_to_string(&out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let kind_of = |name: &str| {
            events
                .iter()
                .filter(|event| event["path"].as_str().is_some_and(|p| p.contains(name)))
                .map(|event| event["kind"].as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(kind_of("/x/real.mp4"), ["audit-event"], "{events:?}");
        assert_eq!(
            kind_of("/x/fake.mp4"),
            [crate::timeline::UNAUTHENTICATED_KIND],
            "{events:?}"
        );
    });
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&other);
}

/// I8: clip-family lanes name the SOURCE in `selector`; R10 keyed P7 on it,
/// so one unusable clip entry made every earlier clip of the same source
/// unusable. Now the record is the output file only.
#[test]
fn r11_i8_one_unusable_clip_does_not_supersede_other_clips_of_its_source() {
    let dir = r11_case("i8", "C-i8");
    fs::create_dir_all(dir.join("artifacts/clips")).unwrap();
    let mine = key(0xb8);
    let lane = dir.join("artifacts/clips/export-log.jsonl");
    for clip in ["a", "b"] {
        append_chained_jsonl_keyed(
            &lane,
            &serde_json::json!({"event":"export-video","selector":"vid_1","output_path":format!("/case/artifacts/clips/{clip}.mp4")})
                .to_string(),
            Some(&mine),
        )
        .unwrap();
    }
    strip_hmac(&lane, 1);
    let rows = read_lane_rows_with_keys(&lane, std::slice::from_ref(&mine)).unwrap();
    assert!(!rows.rows[1].usable());
    assert!(
        rows.rows[0].usable(),
        "clip a superseded by clip b: {:?}",
        rows.rows[0].superseded_by
    );
    // The same output file logged again still supersedes (P7 kept).
    append_chained_jsonl_keyed(
        &lane,
        &serde_json::json!({"event":"export-video","selector":"vid_1","output_path":"/case/artifacts/clips/a.mp4"})
            .to_string(),
        None,
    )
    .unwrap();
    let rows = read_lane_rows_with_keys(&lane, std::slice::from_ref(&mine)).unwrap();
    assert!(!rows.rows[0].usable());
    let _ = fs::remove_dir_all(&dir);
}

/// I2: `hash-verify` / `hash-verify-run` entries carry their `run_id` as a
/// P7 record key — a stripped completion marker supersedes its run's
/// records (shown "더 최근 인증 안 된 항목 있음").
#[test]
fn r11_i2_run_id_is_a_recency_key_of_the_hash_verify_lane() {
    let marker = serde_json::json!({"event":"hash-verify-run","run_id":"100-aaaa"});
    assert_eq!(
        super::recency_keys("evidence/logs/hash-verify.jsonl", &marker),
        ["run_id=100-aaaa"]
    );
    let record = serde_json::json!({"event":"hash-verify","run_id":"100-aaaa","id":"vid_a"});
    assert_eq!(
        super::recency_keys("evidence/logs/hash-verify.jsonl", &record),
        ["id=vid_a", "run_id=100-aaaa"]
    );
}
