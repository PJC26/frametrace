//! R10 track I tests: renderer trust (I1), Windows lane identity (P6:
//! I2–I5), recency (P7: I7/I8), "location unknown ≠ tampering" (P8), and
//! the rest of the R10 trust rows. Each test fails on the R9 code (or does
//! not compile against it where the row adds an API, as in R8/R9).

use super::{LineTrust, append_chained_jsonl_keyed, read_lane_rows_with_keys};
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use std::fs;
use std::path::{Path, PathBuf};

fn r10_case(name: &str, case_id: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r10-{name}-{}", std::process::id()));
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
/// chain stays structurally intact (the HMAC trails `entry_sha256`).
fn strip_hmac(lane: &Path, index: usize) {
    let text = fs::read_to_string(lane).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let cut = lines[index].find(",\"entry_hmac_sha256\"").unwrap();
    lines[index] = format!("{}}}", &lines[index][..cut]);
    fs::write(lane, format!("{}\n", lines.join("\n"))).unwrap();
}

/// Creates a directory junction (Windows) / symbolic link (Unix) `link` →
/// `target`. `false` when the platform refuses (then the test is skipped).
fn dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        // mklink wants `\` separators only.
        let link: PathBuf = link.components().collect();
        let target: PathBuf = target.components().collect();
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|out| out.status.success())
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
}

/// I1: the carve page (`review/carve-report.html`) shows only usable
/// entries as results; the rest are listed with "인증 안 됨". R9 dropped
/// the `trust` field and counted a stripped entry as a carved artifact.
#[test]
fn r10_i1_carve_report_never_counts_unauthenticated_entries() {
    let dir = r10_case("i1", "C-i1");
    let log = concat!(
        r#"{"event":"carve-file","id":"c1","output_path":"/a.mp4","sha256":"aa","trust":"auth"}"#,
        "\n",
        r#"{"event":"carve-file","id":"c2","output_path":"/b.mp4","sha256":"bb","trust":"unauth","trust_note":"stripped"}"#,
        "\n",
        r#"{"event":"carve-file","id":"c3","output_path":"/c.mp4","sha256":"cc","trust":"pc:kid-1234abcd"}"#,
        "\n"
    );
    let html = crate::html_report::render_carve_report_html("{}", log, "{}", "[]", &dir, "{}");
    let artifacts = html
        .split_once("const ARTIFACTS = ")
        .unwrap()
        .1
        .split_once(";\n")
        .unwrap()
        .0;
    let items: Vec<serde_json::Value> = serde_json::from_str(artifacts).unwrap();
    let ok: Vec<bool> = items
        .iter()
        .map(|item| item["trust_ok"].as_bool().unwrap())
        .collect();
    assert_eq!(items.len(), 3, "listed, not dropped");
    assert_eq!(ok.iter().filter(|ok| **ok).count(), 1, "{items:?}");
    let other_pc = items.iter().find(|item| item["id"] == "c3").unwrap();
    assert!(
        other_pc["trust_label"]
            .as_str()
            .unwrap()
            .contains("kid-1234abcd\" 보유 PC에서 확인")
    );
    assert!(html.contains("RESULT_ARTIFACTS.length") && html.contains("인증 안 됨"));
    let _ = fs::remove_dir_all(&dir);
}

/// I1 scan: a renderer that takes lane JSONL (a `…log…`/`…jsonl…` text
/// parameter, which carries the reader's per-entry `trust`) and parses it
/// must look at `trust` — or be listed with the reason it keeps the field.
#[test]
fn r10_i1_no_renderer_drops_the_trust_field() {
    /// (file, fn): renderers that pass every field through untouched.
    const KEEPS_TRUST: &[(&str, &str)] = &[
        // Embeds each parsed object whole (trust included) for the page
        // script, which filters on it (`usableLog`).
        ("html_report.rs", "jsonl_to_array"),
        // The same for the case report, whose script filters with
        // `usableLog` (R9-I1).
        ("report.rs", "jsonl_to_array"),
    ];
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    for name in ["html_report.rs", "report.rs"] {
        let text = fs::read_to_string(src.join(name))
            .unwrap()
            .replace("\r\n", "\n");
        let production = super::tests_r9::production_source(&text);
        for chunk in production
            .split("\nfn ")
            .chain(production.split("\npub fn "))
            .skip(1)
        {
            let fn_name: String = chunk
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let signature = chunk.split_once('{').map_or("", |(sig, _)| sig);
            let body = chunk.split("\n}\n").next().unwrap_or(chunk);
            let takes_lane_text = signature.split(',').any(|param| {
                param.contains(": &str") && (param.contains("jsonl") || param.contains("_log"))
            });
            let parses = body.contains("serde_json::from_str") || body.contains(".lines()");
            if takes_lane_text
                && parses
                && !body.contains("trust")
                && !KEEPS_TRUST.contains(&(name, fn_name.as_str()))
            {
                offenders.push(format!("{name}::{fn_name}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "renderers that parse lane JSONL without its trust: {offenders:?}"
    );
}

/// I2: a `*.jsonl` that is not a chained lane is never an `audit-event`
/// in the timeline — `unchained-log-entry` without keys, "not
/// authenticated" with keys held.
#[test]
fn r10_i2_timeline_marks_unchained_logs() {
    let dir = r10_case("i2", "C-i2");
    fs::write(
        dir.join("evidence/logs/notes.jsonl"),
        "{\"event\":\"planted\",\"logged_unix\":5,\"output_path\":\"/x\"}\n",
    )
    .unwrap();
    let kinds = |dir: &Path| -> Vec<String> {
        let out = dir.join("db/timeline.jsonl");
        crate::timeline::generate_timeline(dir, &out).unwrap();
        fs::read_to_string(&out)
            .unwrap()
            .lines()
            .filter(|line| line.contains("planted"))
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    };
    with_test_keys(&[], || {
        assert_eq!(kinds(&dir), vec![crate::timeline::UNCHAINED_KIND]);
    });
    with_test_keys(&[key(0xa1)], || {
        assert_eq!(kinds(&dir), vec![crate::timeline::UNAUTHENTICATED_KIND]);
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I2/I3 (P6, Windows / macOS): a lane under another letter case IS the
/// lane — the consumers open it through the constant path. R9's walk
/// compared names exactly: the timeline read it raw as `audit-event`, and
/// `collect_chain_lanes` / `case_lane_paths` never checked it.
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn r10_p6_case_varied_lane_is_the_lane() {
    let dir = std::env::temp_dir().join(format!("ft-audit-r10-case-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("Evidence/Logs")).unwrap();
    fs::create_dir_all(dir.join("db")).unwrap();
    fs::write(dir.join("case.json"), r#"{"case_id":"C-case"}"#).unwrap();
    let lane = dir.join("Evidence/Logs/Validation-Log.jsonl");
    let mine = key(0xa2);
    append_chained_jsonl_keyed(
        &lane,
        r#"{"event":"validate-artifact","logged_unix":7,"target_path":"/v.mp4"}"#,
        None,
    )
    .unwrap();
    assert_eq!(
        super::collect_chain_lanes(&dir),
        vec!["evidence/logs/validation-log.jsonl"]
    );
    let listed = super::case_lane_paths(&dir);
    assert_eq!(listed.len(), 1, "one identity, listed once: {listed:?}");
    with_test_keys(std::slice::from_ref(&mine), || {
        let out = dir.join("db/timeline.jsonl");
        crate::timeline::generate_timeline(&dir, &out).unwrap();
        let text = fs::read_to_string(&out).unwrap();
        let event = text.lines().find(|line| line.contains("/v.mp4")).unwrap();
        assert!(
            event.contains(crate::timeline::UNAUTHENTICATED_KIND),
            "{event}"
        );
    });
    // I4: a keyed entry read through the case-varied path is bound to
    // the canonical lane — verifies (R9: FAIL "레인이 옮겨짐").
    append_chained_jsonl_keyed(&lane, r#"{"event":"v2"}"#, Some(&mine)).unwrap();
    assert!(
        fs::read_to_string(&lane)
            .unwrap()
            .contains(r#""audit_lane":"evidence/logs/validation-log.jsonl""#)
    );
    let upper = dir
        .join("EVIDENCE")
        .join("LOGS")
        .join("VALIDATION-LOG.JSONL");
    assert!(super::verify_chained_jsonl_keyed(&upper, std::slice::from_ref(&mine)).is_ok());
    let _ = fs::remove_dir_all(&dir);
}

/// I3 (P6): a lane reached through a junction / symbolic link is found by
/// the case-wide enumeration (R9's walk did not follow links, so it was
/// never checked while the consumers read it) and is NOT VERIFIED — its
/// location cannot be confirmed — never PASS, never FAIL.
#[test]
fn r10_i3_lane_behind_a_junction_is_not_verified() {
    let dir = r10_case("i3", "C-i3");
    let elsewhere = dir.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::remove_dir_all(dir.join("artifacts/carved")).unwrap();
    if !dir_link(&elsewhere, &dir.join("artifacts/carved")) {
        eprintln!("junction / symlink not available; skipped");
        return;
    }
    let lane = dir.join("artifacts/carved/carve-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"event":"carve-file","id":"c1"}"#, None).unwrap();
    assert!(
        super::collect_chain_lanes(&dir).contains(&"artifacts/carved/carve-log.jsonl".to_string())
    );
    let rows = read_lane_rows_with_keys(&lane, &[]).unwrap();
    assert_eq!(rows.status(), "not-verified", "{}", rows.detail());
    assert!(rows.failure.is_none(), "not a FAIL");
    assert_eq!(rows.usable().count(), 0);
    assert_eq!(rows.rows[0].trust, LineTrust::LocationUnverified);
    assert!(rows.detail().contains(super::LOCATION_UNVERIFIED_NOTE));
    let _ = fs::remove_dir_all(&dir);
}

/// I4 (P8): an authenticated, bound entry found where no known lane sits
/// is NOT VERIFIED ("위치를 확인할 수 없음"), not a FAIL (R9: FAIL).
#[test]
fn r10_i4_bound_entry_at_unknown_location_is_not_verified() {
    let dir = r10_case("i4", "C-i4");
    let mine = key(0xa3);
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"event":"v"}"#, Some(&mine)).unwrap();
    let copy = dir.join("validation-copy.jsonl");
    fs::copy(&lane, &copy).unwrap();
    let verified =
        super::verify_chained_jsonl_keyed(&copy, std::slice::from_ref(&mine)).expect("not a FAIL");
    assert!(!verified.lane_trust().passes());
    let rows = read_lane_rows_with_keys(&copy, std::slice::from_ref(&mine)).unwrap();
    assert_eq!(rows.status(), "not-verified");
    assert_eq!(rows.usable().count(), 0);
    // `..` in the path is resolved before the binding is compared.
    let dotted = dir
        .join("db")
        .join("..")
        .join("evidence/logs/validation-log.jsonl");
    assert!(super::verify_chained_jsonl_keyed(&dotted, std::slice::from_ref(&mine)).is_ok());
    let _ = fs::remove_dir_all(&dir);
}

/// I5: the sound-measurement lanes go through the same lane/case binding
/// check — an authenticated sound lane copied from another case no longer
/// verifies here (R9 accepted it).
#[test]
fn r10_i5_sound_lane_copied_from_another_case_does_not_verify() {
    let one = r10_case("i5a", "C-one");
    let two = r10_case("i5b", "C-two");
    let mine = key(0xa4);
    let rel = "evidence/logs/audio-level-log.jsonl";
    append_chained_jsonl_keyed(
        &one.join(rel),
        r#"{"event":"audio-level","results":[]}"#,
        Some(&mine),
    )
    .unwrap();
    fs::copy(one.join(rel), two.join(rel)).unwrap();
    with_test_keys(std::slice::from_ref(&mine), || {
        assert_eq!(
            crate::audio_level::verified_active_lane(&one).unwrap(),
            Some(rel.to_string())
        );
        assert_eq!(
            crate::audio_level::verified_active_lane(&two).unwrap(),
            None
        );
    });
    let _ = fs::remove_dir_all(&one);
    let _ = fs::remove_dir_all(&two);
}

/// I6: a line under a key this PC does not hold AFTER a line this PC
/// authenticated is `unauth` "재작성 배제 불가" — R9 gave it the other-PC
/// (`pc:`) wording.
#[test]
fn r10_i6_unknown_key_after_authenticated_is_unauth() {
    let dir = r10_case("i6", "C-i6");
    let (mine, bogus) = (key(0xa5), key(0xa6));
    let lane = dir.join("evidence/logs/scan-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"n":1}"#, Some(&mine)).unwrap();
    append_chained_jsonl_keyed(&lane, r#"{"n":2}"#, Some(&bogus)).unwrap();
    let rows = read_lane_rows_with_keys(&lane, std::slice::from_ref(&mine)).unwrap();
    assert_eq!(rows.rows[1].code(), "unauth");
    let shown = rows.values_with_trust();
    assert!(
        shown[1]["trust_note"]
            .as_str()
            .unwrap()
            .contains("재작성 배제 불가"),
        "{shown:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// I7 (P7): stripping the HMAC of a record's NEWEST entry does not roll
/// the record back to its older authenticated entry — not in the reader's
/// rows (`trust_note` "더 최근 인증 안 된 항목 있음"), not in validation
/// target resolution.
#[test]
fn r10_i7_newest_unauthenticated_entry_is_never_rolled_back() {
    let dir = r10_case("i7", "C-i7");
    let mine = key(0xa7);
    let real = dir.join("artifacts/carved/real.mp4");
    fs::write(&real, b"x").unwrap();
    let lane = dir.join("artifacts/carved/carve-log.jsonl");
    for _ in 0..2 {
        append_chained_jsonl_keyed(
            &lane,
            &serde_json::json!({"event":"carve-file","id":"c1","output_path":real.to_string_lossy(),"sha256":"a".repeat(64)}).to_string(),
            Some(&mine),
        )
        .unwrap();
    }
    strip_hmac(&lane, 1);
    let rows = read_lane_rows_with_keys(&lane, std::slice::from_ref(&mine)).unwrap();
    assert!(rows.rows[0].trust.is_authenticated());
    assert!(!rows.rows[0].usable(), "superseded");
    let shown = rows.values_with_trust();
    assert_eq!(shown[0]["trust"], "unauth");
    assert!(
        shown[0]["trust_note"]
            .as_str()
            .unwrap()
            .starts_with(super::SUPERSEDED_NOTE)
    );
    assert_eq!(shown[0]["trust_superseded_by"], 2);
    with_test_keys(std::slice::from_ref(&mine), || {
        assert!(crate::validation::resolve_artifact_pair(&dir, "c1", &real).is_err());
        assert_eq!(
            crate::report::evidence_hashes(&dir).unwrap().get("c1"),
            None
        );
    });
    // Without keys (P1) nothing is superseded.
    with_test_keys(&[], || {
        assert!(crate::validation::resolve_artifact_pair(&dir, "c1", &real).is_ok());
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I7/I8: the sign-off anchor is checked through the lane reader — an
/// entry up to the anchor whose HMAC was stripped is NOT VERIFIED (R9
/// compared only the `entry_sha256` text: Intact), and a chain broken
/// before the anchor is "rewritten".
#[test]
fn r10_i8_anchor_checks_authentication_up_to_the_anchor() {
    use super::{AnchorStatus, check_lane_anchor, lane_head};
    let dir = r10_case("i8", "C-i8");
    let mine = key(0xa8);
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    for n in 0..3 {
        append_chained_jsonl_keyed(&lane, &format!(r#"{{"n":{n}}}"#), Some(&mine)).unwrap();
    }
    let head = lane_head(&lane).unwrap().unwrap();
    let pristine = fs::read_to_string(&lane).unwrap();
    with_test_keys(std::slice::from_ref(&mine), || {
        assert_eq!(check_lane_anchor(&lane, &head), AnchorStatus::Intact);
        strip_hmac(&lane, 2);
        assert!(matches!(
            check_lane_anchor(&lane, &head),
            AnchorStatus::NotAuthenticated(_)
        ));
        // An earlier line edited (the anchored last entry untouched).
        fs::write(&lane, pristine.replacen("\"n\":0", "\"n\":9", 1)).unwrap();
        assert_eq!(check_lane_anchor(&lane, &head), AnchorStatus::Rewritten);
    });
    let _ = fs::remove_dir_all(&dir);
}

/// I10: the binding is decided by the body's JSON keys — a VALUE that
/// mentions `"audit_lane"` no longer skips it — and a caller can never
/// supply the binding fields.
#[test]
fn r10_i10_binding_by_json_keys_and_caller_fields_refused() {
    let dir = r10_case("i10", "C-i10");
    let mine = key(0xa9);
    let lane = dir.join("evidence/logs/scan-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"note":"see \"audit_lane\""}"#, Some(&mine)).unwrap();
    assert!(
        fs::read_to_string(&lane)
            .unwrap()
            .contains(r#""audit_lane":"evidence/logs/scan-log.jsonl""#)
    );
    for body in [
        r#"{"audit_lane":"evidence/logs/scan-log.jsonl"}"#,
        r#"{"audit_case_id":"C-other"}"#,
    ] {
        assert!(append_chained_jsonl_keyed(&lane, body, Some(&mine)).is_err());
        assert!(append_chained_jsonl_keyed(&lane, body, None).is_err());
    }
    let _ = fs::remove_dir_all(&dir);
}

/// I11: an authenticated entry without lane/case binding keeps `auth` and
/// carries the "레거시" `trust_note`.
#[test]
fn r10_i11_unbound_authenticated_entry_has_a_legacy_note() {
    let dir = r10_case("i11", "C-i11");
    let mine = key(0xaa);
    let scratch = dir.join("scratch.jsonl");
    append_chained_jsonl_keyed(&scratch, r#"{"event":"old"}"#, Some(&mine)).unwrap();
    let lane = dir.join("evidence/logs/known-hash-log.jsonl");
    fs::copy(&scratch, &lane).unwrap();
    let rows = read_lane_rows_with_keys(&lane, std::slice::from_ref(&mine)).unwrap();
    let shown = rows.values_with_trust();
    assert_eq!(shown[0]["trust"], "auth");
    assert_eq!(shown[0]["trust_note"], super::UNBOUND_LEGACY_NOTE);
    let _ = fs::remove_dir_all(&dir);
}

/// I12 (P8): an UNAUTHENTICATED sign-off-lane entry naming the record
/// with other content does not contradict it ("changed") — R9 did.
#[test]
fn r10_i12_unusable_signoff_entry_never_contradicts() {
    let dir = r10_case("i12", "C-i12");
    let mine = key(0xab);
    let record =
        "{\"schema_version\":4,\"examiner\":\"A\",\"signed_unix\":5,\"content_sha256\":\"c0\"}\n";
    let sha = crate::sha256::digest_bytes(record.as_bytes());
    let lane = dir.join(crate::report::SIGNOFF_LOG_REL_PATH);
    for (content, key) in [("c0", Some(&mine)), ("forged", None)] {
        append_chained_jsonl_keyed(
            &lane,
            &serde_json::json!({"schema_version":1,"event":"signoff","signed_unix":5,"examiner":"A","signoff_schema_version":4,"content_sha256":content,"signoff_record_sha256":sha}).to_string(),
            key,
        )
        .unwrap();
    }
    let verdict = crate::report::signoff_audit_record_with_keys(
        &dir,
        record,
        true,
        std::slice::from_ref(&mine),
    );
    assert!(
        matches!(verdict, crate::report::SignoffAuditRecord::NotVerifiable(_)),
        "{verdict:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// I13: the scan's production source drops only INLINE `#[cfg(test)] mod
/// x { … }` blocks (braces inside strings / chars / comments ignored) — a
/// `mod tests;` declaration no longer hides the rest of the file.
#[test]
fn r10_i13_production_source_skips_only_inline_test_modules() {
    let text = concat!(
        "#[cfg(test)]\nmod tests;\n",
        "fn reader() { let _ = \"{\"; }\n",
        "#[cfg(test)]\nmod inline {\n    fn t() { let _ = '}'; let _ = \"}}\"; /* } */ }\n}\n",
        "fn after() {}\n"
    );
    let production = super::tests_r9::production_source(text);
    assert!(production.contains("fn reader()"), "{production}");
    assert!(production.contains("fn after()"), "{production}");
    assert!(!production.contains("fn t()"), "{production}");
}

/// I14: the raw key and key paths are removed from the process
/// environment at start — a tool the process runs does not inherit them —
/// and only FrameTrace's own step children get them back.
#[test]
fn r10_i14_key_env_is_not_inherited_by_tools() {
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "audit::tests_r10::r10_i14_helper",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FRAMETRACE_AUDIT_KEY", "11".repeat(32))
        .env("FT_R10_I14_HELPER", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(stdout.contains("ADOPTED=yes"), "{stdout}");
    assert!(
        stdout.contains("TOOL=[]") || stdout.contains("TOOL=[%FRAMETRACE_AUDIT_KEY%]"),
        "a tool inherited the key: {stdout}"
    );
    assert!(
        stdout.contains(&format!("STEP=[{}]", "11".repeat(32))),
        "{stdout}"
    );
}

/// Helper process for [`r10_i14_key_env_is_not_inherited_by_tools`].
#[test]
#[ignore]
fn r10_i14_helper() {
    if std::env::var_os("FT_R10_I14_HELPER").is_none() {
        return;
    }
    // SAFETY: this helper runs alone in its process (--exact,
    // --test-threads=1); nothing else touches the environment.
    unsafe { crate::audit_key::adopt_key_env() };
    let adopted = crate::audit_key::verification_keys().is_ok_and(|keys| !keys.is_empty());
    println!("ADOPTED={}", if adopted { "yes" } else { "no" });
    let echo = |forward: bool| {
        #[cfg(windows)]
        let mut command = {
            let mut command = std::process::Command::new("cmd");
            command.args(["/C", "echo [%FRAMETRACE_AUDIT_KEY%]"]);
            command
        };
        #[cfg(unix)]
        let mut command = {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", "echo \"[$FRAMETRACE_AUDIT_KEY]\""]);
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

/// I15: on this PC `std` reports a path under a junction whose target is
/// gone, and an unreachable network share, as `NotFound` — the default
/// key location there is "cannot confirm" (P5 error), not "no key".
#[test]
fn r10_i15_unconfirmable_default_key_location_fails_closed() {
    use crate::audit_key::{KEY_CONFIG_UNREADABLE, probe_key_path};
    let dir = r10_case("i15", "C-i15");
    let target = dir.join("gone");
    fs::create_dir_all(&target).unwrap();
    let link = dir.join("appdata");
    if dir_link(&target, &link) {
        fs::remove_dir_all(&target).unwrap();
        let key_path = link.join("frametrace").join("audit-key");
        let err = probe_key_path(&key_path, "감사 키 파일", None).unwrap_err();
        assert!(err.starts_with(KEY_CONFIG_UNREADABLE), "{err}");
        assert!(err.contains("끊긴 정션"), "{err}");
    } else {
        eprintln!("junction / symlink not available; dangling-link part skipped");
    }
    // An ordinary missing default location is still "no key".
    assert_eq!(
        probe_key_path(&dir.join("cfg/frametrace/audit-key"), "k", None),
        Ok(false)
    );
    #[cfg(windows)]
    {
        let unc = Path::new(r"\\nonexistent-host-ft-r10\share\frametrace\audit-key");
        let err = probe_key_path(unc, "감사 키 파일", None).unwrap_err();
        assert!(err.starts_with(KEY_CONFIG_UNREADABLE), "{err}");
    }
    let _ = fs::remove_dir_all(&dir);
}
