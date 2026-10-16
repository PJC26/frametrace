//! R9 track I tests: row-level trust (P3), full-case lane scope (P4),
//! fail-closed key configuration (P5), lane/case binding (I11).

use super::{LineTrust, append_chained_jsonl_keyed, read_lane_rows_with_keys};
use crate::audit_key::{AuditKey, fingerprint_id};
use std::fs;
use std::path::{Path, PathBuf};

fn r9_case(name: &str, case_id: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-audit-r9-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("evidence/logs")).unwrap();
    fs::create_dir_all(dir.join("artifacts/carved")).unwrap();
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

/// P3: every row comes back with its own trust — authenticated, not
/// authenticated here (stripped / unkeyed), another PC's key — and the
/// per-entry `trust` code the viewer contract carries. Without keys every
/// row is self-consistency only; past a chain break nothing is usable.
#[test]
fn r9_p3_rows_carry_their_trust() {
    let dir = r9_case("rows", "C-rows");
    let (mine, other) = (key(0x91), key(0x92));
    let lane = dir.join("evidence/logs/validation-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"event":"validate","n":1}"#, Some(&mine)).unwrap();
    append_chained_jsonl_keyed(&lane, r#"{"event":"validate","n":2}"#, None).unwrap();
    append_chained_jsonl_keyed(&lane, r#"{"event":"validate","n":3}"#, Some(&other)).unwrap();
    let rows = read_lane_rows_with_keys(&lane, std::slice::from_ref(&mine)).unwrap();
    let codes: Vec<String> = rows.rows.iter().map(|row| row.trust.code()).collect();
    // R10-I6: the other key's line follows a line this PC authenticated —
    // "재작성 배제 불가" (`unauth`), not `pc:` (see r9_contract_… for `pc:`).
    assert_eq!(
        codes,
        vec![
            "auth".to_string(),
            "unauth".to_string(),
            "unauth".to_string()
        ]
    );
    assert!(matches!(
        rows.rows[2].trust,
        LineTrust::RewriteNotExcluded { .. }
    ));
    assert_eq!(rows.usable().count(), 1);
    assert_eq!(rows.status(), "not-verified");
    let shown = rows.values_with_trust();
    assert_eq!(shown[0]["trust"], "auth");
    assert!(shown[0].get("trust_note").is_none());
    assert_eq!(shown[1]["trust"], "unauth");
    assert!(
        shown[1]["trust_note"]
            .as_str()
            .unwrap()
            .contains("인증되지 않음")
    );
    assert_eq!(rows.chain_json("x")["unauthenticated"], 2);
    // P1: no keys here.
    let keyless = read_lane_rows_with_keys(&lane, &[]).unwrap();
    assert!(keyless.rows.iter().all(|row| row.trust.code() == "sc"));
    assert_eq!(keyless.usable().count(), 3);
    // A chain break: nothing from the break on is usable, on any PC.
    let text = fs::read_to_string(&lane).unwrap();
    fs::write(&lane, text.replacen("\"n\":2", "\"n\":9", 1)).unwrap();
    let broken = read_lane_rows_with_keys(&lane, &[]).unwrap();
    assert!(broken.failure.is_some());
    assert_eq!(broken.status(), "failed");
    assert_eq!(broken.rows[1].trust, LineTrust::ChainBroken);
    // R11-I4 (P9): the prefix before the break is not usable either.
    assert_eq!(broken.usable().count(), 0);
    assert_eq!(broken.rows[0].lane_broken_at, Some(1));
    assert_eq!(broken.rows[0].code(), "unauth");
    // Missing lane: present = false, no rows.
    let absent = read_lane_rows_with_keys(&dir.join("evidence/logs/scan-log.jsonl"), &[]).unwrap();
    assert!(!absent.present && absent.rows.is_empty());
    assert_eq!(absent.status(), "absent");
    let _ = fs::remove_dir_all(&dir);
}

/// I1: the case report shows only usable entries as results (counts,
/// tables) and lists the rest separately as "인증 안 됨". R8 counted and
/// showed a stripped validation entry as a confirmed validation.
#[test]
fn r9_i1_report_counts_only_usable_rows_and_lists_the_rest() {
    let validation = concat!(
        r#"{"event":"validate-artifact","target_path":"/ok.mp4","validation_status":"ffprobe-video-stream-confirmed","trust":"auth"}"#,
        "\n",
        r#"{"event":"validate-artifact","target_path":"/forged.mp4","validation_status":"ffprobe-video-stream-confirmed","trust":"unauth","trust_note":"stripped"}"#,
        "\n",
        r#"{"event":"validate-artifact","target_path":"/other-pc.mp4","trust":"pc:kid-12345678"}"#,
        "\n"
    );
    let html = crate::report::render_case_report_with_chains(
        &crate::report::ReportInputs {
            manifest_json: "{}",
            index_json: "{}",
            export_log_jsonl: "",
            proxy_log_jsonl: "",
            thumbnail_log_jsonl: "",
            carve_log_jsonl: "",
            filesystem_log_jsonl: "",
            validation_log_jsonl: validation,
            anomaly_log_jsonl: "",
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
        r#"{"validation":{"path":"evidence/logs/validation-log.jsonl","status":"not-verified","detail":"키 설정을 읽지 못함: 감사 키 파일 X — 폴더 (변조 판정 아님)"}}"#,
    );
    let Some((elements, label)) = run_report_script(&html, "chainLabel(\"validation\")") else {
        return;
    };
    assert_eq!(elements["validations"]["textContent"], 1, "{elements}");
    let listed = elements["unauthenticated-entries"]["innerHTML"]
        .as_str()
        .unwrap_or_default();
    assert!(listed.contains("/forged.mp4") && listed.contains("인증 안 된 감사 항목 2건"));
    assert!(
        listed.contains("kid-12345678&quot; 보유 PC에서 확인"),
        "{listed}"
    );
    // I7/I9: the label carries the reader's own reason (not one fixed
    // sentence, and a key-configuration problem is not "tampering").
    assert!(label.contains("키 설정을 읽지 못함"), "{label}");
    assert!(label.contains("변조 판정 아님"), "{label}");
}

/// Runs the report page script under a stub DOM; returns the elements and
/// the value of `extra` (a JS expression). `None` without node.
fn run_report_script(html: &str, extra: &str) -> Option<(serde_json::Value, String)> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let script = html
        .split_once("<script>")
        .unwrap()
        .1
        .split_once("</script>")
        .unwrap()
        .0;
    let Ok(mut child) = Command::new("node")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        eprintln!("node not found; report script run skipped");
        return None;
    };
    let mut stdin = child.stdin.take().unwrap();
    write!(
        stdin,
        "const elements = new Map(); const document = {{getElementById(id) {{ if (!elements.has(id)) elements.set(id, {{}}); return elements.get(id); }}}};\n{script}\nprocess.stdout.write(JSON.stringify({{elements: Object.fromEntries(elements), extra: String({extra})}}));"
    )
    .unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "the report script threw: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    Some((
        value["elements"].clone(),
        value["extra"].as_str().unwrap_or_default().to_string(),
    ))
}

/// I3: a sign-off's target paths and hashes come only from the LAST entry
/// usable here. R8 took the last entry of any kind, so an unauthenticated
/// carve entry redirected the re-hash to another file / hash.
#[test]
fn r9_i3_signoff_targets_and_hashes_come_from_usable_rows_only() {
    let dir = r9_case("targets", "C-targets");
    let mine = key(0x93);
    let lane = dir.join("artifacts/carved/carve-log.jsonl");
    let good = "a".repeat(64);
    let forged = "b".repeat(64);
    append_chained_jsonl_keyed(
        &lane,
        &serde_json::json!({"event":"carve-file","id":"c1","output_path":"artifacts/carved/real.mp4","sha256":good}).to_string(),
        Some(&mine),
    )
    .unwrap();
    for (id, path) in [
        ("c1", "artifacts/carved/planted.mp4"),
        ("c2", "artifacts/carved/only-forged.mp4"),
    ] {
        append_chained_jsonl_keyed(
            &lane,
            &serde_json::json!({"event":"carve-file","id":id,"output_path":path,"sha256":forged})
                .to_string(),
            None,
        )
        .unwrap();
    }
    crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
        let hashes = crate::report::evidence_hashes(&dir).unwrap();
        // R10 P7: c1's NEWEST entry is not authenticated — c1 is not rolled
        // back to its older authenticated hash either ("확인 불가").
        assert_eq!(hashes.get("c1"), None);
        assert_eq!(
            hashes.get("c2"),
            None,
            "확인 불가 — never the unauthenticated hash"
        );
        let targets = crate::report::evidence_target_paths(&dir);
        assert!(!targets.contains_key("c1"), "{targets:?}");
        assert!(!targets.contains_key("c2"));
    });
    // P1: without keys every entry is self-consistent; the last one wins.
    crate::audit_key::with_test_keys(&[], || {
        let hashes = crate::report::evidence_hashes(&dir).unwrap();
        assert_eq!(hashes.get("c1"), Some(&forged));
    });
    let _ = fs::remove_dir_all(&dir);
}

/// P4: a case-wide lane check covers artifacts/** lanes too (R8's
/// verify-audit <case>, workstation badge and doctor looked at
/// evidence/logs only).
#[test]
fn r9_p4_case_lane_paths_cover_the_whole_case() {
    let dir = r9_case("scope", "C-scope");
    append_chained_jsonl_keyed(
        &dir.join("artifacts/carved/carve-log.jsonl"),
        r#"{"n":1}"#,
        None,
    )
    .unwrap();
    append_chained_jsonl_keyed(
        &dir.join("evidence/logs/scan-log.jsonl"),
        r#"{"n":1}"#,
        None,
    )
    .unwrap();
    fs::write(dir.join("evidence/logs/notes.jsonl"), "{}\n").unwrap();
    let rels: Vec<String> = super::case_lane_paths(&dir)
        .iter()
        .map(|path| {
            path.strip_prefix(&dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(
        rels,
        vec![
            "artifacts/carved/carve-log.jsonl",
            "evidence/logs/notes.jsonl",
            "evidence/logs/scan-log.jsonl"
        ]
    );
    let _ = fs::remove_dir_all(&dir);
}

/// P5/I9: a key configuration that exists but cannot be read fails CLOSED
/// (keys held, nothing authenticated) with non-tamper wording — R8 read a
/// configured-but-missing path, a directory or a broken link as "no key".
#[test]
fn r9_p5_unreadable_key_configuration_fails_closed_without_tamper_wording() {
    use crate::audit_key::{KEY_CONFIG_UNREADABLE, probe_key_path};
    let dir = r9_case("p5", "C-p5");
    let missing = dir.join("no-such-key");
    assert_eq!(
        probe_key_path(&missing, "k", None),
        Ok(false),
        "default path: no key"
    );
    let err = probe_key_path(&missing, "k", Some("FRAMETRACE_AUDIT_KEY_FILE")).unwrap_err();
    assert!(
        err.starts_with(KEY_CONFIG_UNREADABLE) && err.contains("변조 판정 아님"),
        "{err}"
    );
    assert!(super::is_key_config_error(&err));
    let folder = dir.join("key-folder");
    fs::create_dir_all(&folder).unwrap();
    assert!(
        probe_key_path(&folder, "k", None)
            .unwrap_err()
            .contains("폴더")
    );
    let file = dir.join("key-file");
    fs::write(&file, "x").unwrap();
    assert_eq!(probe_key_path(&file, "k", None), Ok(true));
    #[cfg(unix)]
    {
        let link = dir.join("key-link");
        std::os::unix::fs::symlink(dir.join("gone"), &link).unwrap();
        assert!(
            probe_key_path(&link, "k", None)
                .unwrap_err()
                .contains("끊긴 링크")
        );
    }
    #[cfg(windows)]
    {
        let link = dir.join("key-link");
        if std::os::windows::fs::symlink_file(dir.join("gone"), &link).is_ok() {
            assert!(
                probe_key_path(&link, "k", None)
                    .unwrap_err()
                    .contains("끊긴 링크")
            );
        }
    }
    // Readers: every row unauthenticated, lane NOT VERIFIED (not failed).
    let lane = dir.join("evidence/logs/scan-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"n":1}"#, Some(&key(0x95))).unwrap();
    let rows = super::read_lane_rows_with_key_error(&lane, &err).unwrap();
    assert_eq!(rows.usable().count(), 0);
    assert_eq!(rows.status(), "not-verified");
    assert!(
        rows.detail().contains(KEY_CONFIG_UNREADABLE),
        "{}",
        rows.detail()
    );
    assert!(!rows.detail().contains("체인 검증 실패"));
    let _ = fs::remove_dir_all(&dir);
}

/// I10: entries before ANOTHER PC's key-introduction marker are that PC's
/// pre-key prefix — "check on the PC holding key <id>", not "HMAC
/// stripped".
#[test]
fn r9_i10_other_pcs_pre_key_prefix_names_the_key() {
    let dir = r9_case("prekey", "C-prekey");
    let (a, b) = (key(0x96), key(0x97));
    let lane = dir.join("evidence/logs/scan-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"n":1}"#, None).unwrap();
    append_chained_jsonl_keyed(
        &lane,
        &format!(r#"{{"kind":"{}","to":"a"}}"#, super::KEY_INTRODUCED_KIND),
        Some(&a),
    )
    .unwrap();
    let here = super::verify_chained_jsonl_keyed(&lane, std::slice::from_ref(&b)).unwrap();
    assert_eq!(
        here.line_trust(0),
        LineTrust::OtherPc {
            key_id: a.id.clone()
        }
    );
    let trust = here.lane_trust();
    assert!(!trust.passes());
    assert!(
        trust
            .reason()
            .contains(&format!("\"{}\" 보유 PC에서 확인", a.id)),
        "{trust:?}"
    );
    assert!(!trust.reason().contains("HMAC 없는 줄"), "{trust:?}");
    // On the introducing PC the prefix is the documented pre-key prefix.
    let there = super::verify_chained_jsonl_keyed(&lane, std::slice::from_ref(&a)).unwrap();
    assert_eq!(there.line_trust(0), LineTrust::PreKey);
    let _ = fs::remove_dir_all(&dir);
}

/// I11: a keyed entry names its lane and case inside the HMAC-covered
/// bytes; an authenticated lane copied to another case or lane no longer
/// verifies there. Entries written before R9 carry no binding and are
/// reported as legacy.
#[test]
fn r9_i11_lane_and_case_binding() {
    let one = r9_case("bind1", "C-one");
    let two = r9_case("bind2", "C-two");
    let mine = key(0x98);
    let keys = std::slice::from_ref(&mine);
    let lane = one.join("evidence/logs/validation-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"event":"v"}"#, Some(&mine)).unwrap();
    let text = fs::read_to_string(&lane).unwrap();
    assert!(
        text.contains(r#""audit_lane":"evidence/logs/validation-log.jsonl""#),
        "{text}"
    );
    assert!(text.contains(r#""audit_case_id":"C-one""#), "{text}");
    assert!(super::verify_chained_jsonl_keyed(&lane, keys).is_ok());
    // Unkeyed appends are byte-identical to before (no binding fields).
    append_chained_jsonl_keyed(
        &one.join("evidence/logs/scan-log.jsonl"),
        r#"{"n":1}"#,
        None,
    )
    .unwrap();
    assert!(
        !fs::read_to_string(one.join("evidence/logs/scan-log.jsonl"))
            .unwrap()
            .contains("audit_lane")
    );
    // Copied into another case: FAIL there.
    let moved = two.join("evidence/logs/validation-log.jsonl");
    fs::write(&moved, &text).unwrap();
    let err = super::verify_chained_jsonl_keyed(&moved, keys).unwrap_err();
    assert!(err.contains("다른 케이스"), "{err}");
    let rows = read_lane_rows_with_keys(&moved, keys).unwrap();
    assert_eq!(rows.status(), "failed");
    assert_eq!(rows.usable().count(), 0);
    // Copied into another lane of the same case: FAIL there.
    let other_lane = one.join("evidence/logs/anomaly-log.jsonl");
    fs::write(&other_lane, &text).unwrap();
    let err = super::verify_chained_jsonl_keyed(&other_lane, keys).unwrap_err();
    assert!(err.contains("다른 레인"), "{err}");
    // A legacy (pre-R9) keyed entry has no binding: verifies, flagged.
    let scratch = one.join("scratch.jsonl");
    append_chained_jsonl_keyed(&scratch, r#"{"event":"old"}"#, Some(&mine)).unwrap();
    let legacy = one.join("evidence/logs/known-hash-log.jsonl");
    fs::copy(&scratch, &legacy).unwrap();
    let verified = super::verify_chained_jsonl_keyed(&legacy, keys).unwrap();
    assert!(
        verified.warnings.iter().any(|w| w.contains("legacy")),
        "{:?}",
        verified.warnings
    );
    let rows = read_lane_rows_with_keys(&legacy, keys).unwrap();
    assert_eq!(rows.unbound_authenticated, 1);
    assert_eq!(rows.usable().count(), 1);
    assert!(rows.detail().contains("레거시"));
    let _ = fs::remove_dir_all(&one);
    let _ = fs::remove_dir_all(&two);
}

/// Contract with track U: `/api/verify-audit` per-log counts and the
/// sound-level map's per-entry `trust`.
#[test]
fn r9_contract_counts_and_audio_level_trust() {
    let dir = r9_case("counts", "C-counts");
    let (mine, other) = (key(0x99), key(0x9a));
    let lane = dir.join("evidence/logs/scan-log.jsonl");
    append_chained_jsonl_keyed(&lane, r#"{"n":1}"#, None).unwrap();
    append_chained_jsonl_keyed(&lane, r#"{"n":2}"#, Some(&other)).unwrap();
    append_chained_jsonl_keyed(&lane, r#"{"n":3}"#, Some(&mine)).unwrap();
    let v = super::verify_chained_jsonl_keyed(&lane, std::slice::from_ref(&mine)).unwrap();
    assert_eq!(v.trust_counts(), (1, 1));
    let keyless = super::verify_chained_jsonl_keyed(&lane, &[]).unwrap();
    assert_eq!(keyless.trust_counts(), (0, 0));
    let state = |r: &'static str, sc: bool, pc: Option<&str>| crate::audio_level::AudioLevelState {
        r,
        max: None,
        mean: None,
        sec: None,
        t: 1,
        error: None,
        codec: None,
        max_is_neg_inf: false,
        self_consistent_only: sc,
        other_pc_key: pc.map(str::to_string),
    };
    assert_eq!(state("level", false, None).to_json()["trust"], "auth");
    assert_eq!(state("level", true, None).to_json()["trust"], "sc");
    assert_eq!(
        state("unverified", false, None).to_json()["trust"],
        "unauth"
    );
    assert_eq!(
        state("unverified", false, Some("kid-1")).to_json()["trust"],
        "pc:kid-1"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Tokens that name a chained audit lane in source: every known lane's file
/// name, the constants that spell lane paths, and the lane enumerators.
fn lane_tokens() -> Vec<String> {
    let mut tokens: Vec<String> = super::KNOWN_CHAIN_LANES
        .iter()
        .map(|rel| rel.rsplit('/').next().unwrap().to_string())
        .collect();
    tokens.extend(
        [
            "audio-level-log",
            "HASH_VERIFY_REL_PATH",
            "SIGNOFF_LOG_REL_PATH",
            "EVIDENCE_SOURCE_LOG_REL_PATH",
            "audio_level::LOG_REL",
            "join::LOG_FILE",
            "CARVE_LOG_REL",
            "QUEUE_LOG",
            "collect_chain_lanes(",
            "KNOWN_CHAIN_LANES",
            "logs/e01-audit",
            // Generic walkers and path parameters that reach lanes.
            "collect_jsonl_files(",
            "ARTIFACT_LOGS",
            "log_path",
            "lane_path",
            "LOG_REL",
            "LOG_FILE",
            "lane_rels(",
        ]
        .map(str::to_string),
    );
    tokens.sort();
    tokens.dedup();
    tokens
}

/// Raw read primitives that would bypass `read_lane_rows`. R10-I13: plus
/// `OpenOptions`, `read_to_end(` and splitting text into lines by hand.
const RAW_READS: &[&str] = &[
    "read_to_string(",
    "fs::read(",
    "File::open(",
    "OpenOptions",
    "read_to_end(",
    "read_log_locked(",
    "BufReader::new(",
    "verify_chained_jsonl_text(",
    "split('\\n')",
];

/// R10-I13: the production part of a source file — everything except
/// INLINE `#[cfg(test)] mod name { … }` blocks, found by tracking braces
/// outside string / char literals and comments. R9 cut the file at the
/// first `#[cfg(test)]\nmod `, so a `#[cfg(test)] mod tests;` DECLARATION
/// near the top hid every function after it from the scan.
pub(crate) fn production_source(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &text[i..];
        if let Some(after_attr) = rest.strip_prefix("#[cfg(test)]") {
            let trimmed = after_attr.trim_start();
            let skip_ws = after_attr.len() - trimmed.len();
            let item = trimmed
                .strip_prefix("pub(crate) ")
                .or_else(|| trimmed.strip_prefix("pub "))
                .unwrap_or(trimmed);
            if let Some(after_mod) = item.strip_prefix("mod ") {
                let name_len = after_mod
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(after_mod.len());
                let after_name = after_mod[name_len..].trim_start();
                if after_name.starts_with('{') {
                    // Skip to the matching closing brace.
                    let start = i
                        + "#[cfg(test)]".len()
                        + skip_ws
                        + (trimmed.len() - item.len())
                        + "mod ".len()
                        + name_len
                        + (after_mod[name_len..].len() - after_name.len());
                    i = matching_brace_end(text, start);
                    continue;
                }
            }
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Index just past the `}` matching the `{` at `open`, skipping string,
/// raw string, char literals and comments.
fn matching_brace_end(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'r' if matches!(bytes.get(i + 1), Some(b'"' | b'#'))
                && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')) =>
            {
                let mut j = i + 1;
                let mut hashes = 0usize;
                while bytes.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if bytes.get(j) == Some(&b'"') {
                    let close = format!("\"{}", "#".repeat(hashes));
                    match text[j + 1..].find(&close) {
                        Some(end) => i = j + 1 + end + close.len(),
                        None => return bytes.len(),
                    }
                    continue;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            b'\'' => {
                // A char literal ('x', '\n', '{'); a lifetime has no
                // closing quote right after one character.
                if bytes.get(i + 1) == Some(&b'\\') {
                    if let Some(end) = text[i + 2..].find('\'') {
                        i += 2 + end + 1;
                        continue;
                    }
                } else if let Some(ch) = text[i + 1..].chars().next()
                    && text[i + 1 + ch.len_utf8()..].starts_with('\'')
                {
                    i += 1 + ch.len_utf8() + 1;
                    continue;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// Splits production source into (fn name, body) chunks at every line that
/// starts a function item.
fn functions(production: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in production.lines() {
        let trimmed = line.trim_start();
        let item = trimmed
            .strip_prefix("pub(crate) ")
            .or_else(|| trimmed.strip_prefix("pub(super) "))
            .or_else(|| trimmed.strip_prefix("pub "))
            .unwrap_or(trimmed);
        let item = item.strip_prefix("async ").unwrap_or(item);
        // Any other top-level item ends the function before it.
        let top_level = !line.starts_with(' ') && !line.starts_with('}');
        if top_level
            && [
                "struct ", "enum ", "impl", "const ", "static ", "mod ", "type ",
            ]
            .iter()
            .any(|kw| item.starts_with(kw))
        {
            out.push((String::new(), String::new()));
        }
        if let Some(rest) = item.strip_prefix("fn ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            out.push((name, String::new()));
        }
        if let Some((_, body)) = out.last_mut() {
            // Comments do not read files.
            if !trimmed.starts_with("//") {
                body.push_str(line);
                body.push('\n');
            }
        }
    }
    out
}

/// P3: every non-test function that names a chained lane and reads a file
/// goes through `audit::read_lane_rows` (or is listed here with the reason
/// it does not read ROWS of a lane: appenders, raw byte copies, the
/// verifier itself). Like the `hide_console` scan, a new raw reader of a
/// lane fails this test.
#[test]
fn r9_p3_every_lane_row_reader_goes_through_read_lane_rows() {
    /// (file, fn) pairs that touch a lane file without reading its rows.
    const NOT_ROW_READERS: &[(&str, &str)] = &[
        // The reader and verifier themselves.
        ("audit.rs", "read_lane_rows"),
        ("audit.rs", "read_lane_rows_text"),
        ("audit.rs", "read_log_locked"),
        ("audit.rs", "read_log_for_verification"),
        ("audit.rs", "verify_chained_jsonl"),
        ("audit.rs", "verify_chained_jsonl_keyed"),
        ("audit.rs", "verified_prefix"),
        ("audit.rs", "lane_head"),
        ("audit.rs", "check_lane_anchor"),
        ("audit.rs", "chain_head"),
        ("audit.rs", "collect_chain_lanes"),
        ("audit.rs", "lane_binding_of"),
        ("audit.rs", "case_lane_paths"),
        ("audit.rs", "apply_lane_binding"),
        // Sound lanes: `read_lanes` → `lane_statuses` →
        // `read_logged_results_with_keys` judge every line with
        // `line_trust` (R8-I1); this fn only checks a continuation
        // marker's STRUCTURE over text already read there.
        ("audio_level.rs", "continuation_proof"),
        // Reads the raw lane bytes the continuation proof hashes (SHA-256
        // of the previous lane); rows are then judged per line by
        // `line_trust` in `read_logged_results_with_keys` (R8-I1/I6).
        ("audio_level.rs", "read_lanes"),
        // Appenders / non-lane reads that merely name a lane.
        ("join.rs", "run_inner"),
        ("transcode.rs", "run_queue"),
        ("cli/handlers/media.rs", "export_dav_with"),
        // Reads the completion marker (not the lane); the lane goes
        // through `check_bound_import_entry` → `read_lane_rows_with_keys`.
        ("e01.rs", "raw_import_status_with_keys"),
        // Reads the packaged report / sign-off record, not a lane.
        ("package.rs", "verify_package_db_and_signoff"),
    ];
    let tokens = lane_tokens();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src.clone()];
    let mut offenders = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name == "tests.rs" || name.starts_with("tests_") || name.contains("_tests") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
            let production = production_source(&text);
            for (function, body) in functions(&production) {
                if function.is_empty() {
                    continue;
                }
                let names_lane =
                    |text: &str| tokens.iter().any(|token| text.contains(token.as_str()));
                let reads = |text: &str| {
                    RAW_READS.iter().any(|read| text.contains(read)) || text.contains(".lines()")
                };
                // A function that names a lane and reads files raw must
                // route its lane reads through the reader; and no single
                // statement may name a lane AND read it raw (a function
                // that also reads case.json / the index may not hide a raw
                // lane read next to a `read_lane_rows` call).
                let routes = body.contains("read_lane_rows") || body.contains("review_lane(");
                let raw_lane_statement = body
                    .split(';')
                    .any(|statement| names_lane(statement) && reads(statement));
                if !(names_lane(&body) && reads(&body) && (!routes || raw_lane_statement)) {
                    continue;
                }
                if NOT_ROW_READERS.contains(&(rel.as_str(), function.as_str())) {
                    continue;
                }
                offenders.push(format!("{rel}::{function}"));
            }
        }
    }
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "functions that read a chained audit lane without audit::read_lane_rows: {offenders:#?}"
    );
}
