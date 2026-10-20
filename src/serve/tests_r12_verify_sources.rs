//! R12-I4: `/api/verify-sources` reports the run it just made — the run id
//! the `qa anomalies` child printed is compared with the result run.

use super::verify_sources_with_exe;
use crate::audit_key::{AuditKey, fingerprint_id, with_test_keys};
use crate::serve::state::JobState;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn fake_cli(dir: &Path, printed: Option<&str>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let echo = printed
            .map(|id| format!("echo \"hash-verify-run-id: {id}\"\n"))
            .unwrap_or_default();
        let path = dir.join("fake-frametrace.sh");
        std::fs::write(
            &path,
            format!("#!/bin/sh\n[ \"$1\" = --warm ] && exit 0\nif [ \"$1\" = \"qa\" ]; then\n{echo}echo done\nfi\nexit 0\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::test_support::warm_unix_script(&path);
        path
    }
    #[cfg(not(unix))]
    {
        let echo = printed
            .map(|id| format!("echo hash-verify-run-id: {id}\r\n"))
            .unwrap_or_default();
        let path = dir.join("fake-frametrace.cmd");
        std::fs::write(
            &path,
            format!("@echo off\r\nif \"%1\"==\"qa\" (\r\n{echo}echo done\r\n)\r\nexit /b 0\r\n"),
        )
        .unwrap();
        path
    }
}

fn hv_lane(case: &Path, key: &AuditKey) {
    let lane = case.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    for line in [
        serde_json::json!({"schema_version":1,"event":"hash-verify","run_unix":100,
            "run_id":"100-aaaa","id":"vid_a","source_path":"/e/a.mp4",
            "recorded_sha256":"a".repeat(64),"observed_sha256":"a".repeat(64),"status":"match"}),
        serde_json::json!({"schema_version":1,"event":"hash-verify-run","run_unix":100,
            "run_id":"100-aaaa","records":1}),
    ] {
        crate::audit::append_chained_jsonl_keyed(&lane, &line.to_string(), Some(key)).unwrap();
    }
}

/// The child printed another run's id (its own run did not land as the
/// log's result), printed none, or printed the result's id.
#[test]
fn r12_i4_verify_sources_compares_the_just_run_run_id() {
    let root =
        std::env::temp_dir().join(format!("ft-r12-i4-verify-sources-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let case = root.join("case");
    std::fs::create_dir_all(case.join("evidence/logs")).unwrap();
    std::fs::write(case.join("case.json"), "{\"case_id\":\"C-i4\"}").unwrap();
    let key = AuditKey::new(&fingerprint_id(&[0xd4; 32]), &[0xd4; 32]);
    hv_lane(&case, &key);
    let state = {
        let mut state = JobState::new();
        state.case_dir = Some(case.clone());
        state.phase = "review-ready";
        Arc::new(Mutex::new(state))
    };
    let run = |printed: Option<&str>| {
        let exe = fake_cli(&root, printed);
        let response = with_test_keys(std::slice::from_ref(&key), || {
            verify_sources_with_exe(&state, &exe)
        });
        serde_json::from_str::<serde_json::Value>(&response).unwrap()
    };
    // Another run id: the older run in the log is NOT this run's result.
    let other = run(Some("200-bbbb"));
    assert_eq!(other["ok"], true, "{other}");
    assert_eq!(other["is_result"], false, "{other}");
    assert_eq!(other["verified"], false, "{other}");
    assert_eq!(other["matched"], 0, "{other}");
    assert_eq!(other["expected_run_id"], "200-bbbb", "{other}");
    assert!(
        other["not_verified_reason"]
            .as_str()
            .is_some_and(|r| r.contains("200-bbbb")),
        "{other}"
    );
    // No run id printed: cannot be matched — no result.
    let silent = run(None);
    assert_eq!(silent["is_result"], false, "{silent}");
    // The just-run id IS the result: reported.
    let same = run(Some("100-aaaa"));
    assert_eq!(same["is_result"], true, "{same}");
    assert_eq!(same["run_id"], "100-aaaa", "{same}");
    assert_eq!(same["matched"], 1, "{same}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn r12_i4_printed_run_id_is_the_first_prefixed_line() {
    use crate::anomaly::printed_hash_verify_run_id as id;
    assert_eq!(
        id("hash-verify-run-id: 1-ab\r\nanomaly scan complete\n- [x] hash-verify-run-id: 2-cd\n")
            .as_deref(),
        Some("1-ab")
    );
    assert_eq!(id("anomaly scan complete\n"), None);
    assert_eq!(id("hash-verify-run-id: \n"), None);
}

/// R13 I1 (P16): the response names why there is no result and lists the
/// newer unmarked run's mismatch rows (track U contract fields).
#[test]
fn r13_i1_verify_sources_surfaces_the_newer_unmarked_run() {
    let root =
        std::env::temp_dir().join(format!("ft-r13-i1-verify-sources-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let case = root.join("case");
    std::fs::create_dir_all(case.join("evidence/logs")).unwrap();
    std::fs::write(case.join("case.json"), "{\"case_id\":\"C-r13i1\"}").unwrap();
    let key = AuditKey::new(&fingerprint_id(&[0xd5; 32]), &[0xd5; 32]);
    hv_lane(&case, &key);
    crate::audit::append_chained_jsonl_keyed(
        &case.join(crate::anomaly::HASH_VERIFY_REL_PATH),
        &serde_json::json!({"schema_version":1,"event":"hash-verify","run_unix":200,
            "run_id":"200-bbbb","id":"vid_a","source_path":"/e/a.mp4",
            "recorded_sha256":"a".repeat(64),"observed_sha256":"b".repeat(64),"status":"mismatch"})
        .to_string(),
        Some(&key),
    )
    .unwrap();
    let state = {
        let mut state = JobState::new();
        state.case_dir = Some(case.clone());
        state.phase = "review-ready";
        Arc::new(Mutex::new(state))
    };
    let exe = fake_cli(&root, Some("100-aaaa"));
    let response = with_test_keys(std::slice::from_ref(&key), || {
        verify_sources_with_exe(&state, &exe)
    });
    let body: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["is_result"], false, "{body}");
    assert_eq!(body["verified"], false, "{body}");
    assert_eq!(body["matched"], 0, "{body}");
    assert!(
        body["withheld_reason"]
            .as_str()
            .is_some_and(|r| r.starts_with(crate::anomaly::NEWER_UNMARKED_RUN_REASON)),
        "{body}"
    );
    assert_eq!(body["newer_runs"], 1, "{body}");
    assert_eq!(body["newer_records"], 1, "{body}");
    assert_eq!(body["newer_mismatched"], 1, "{body}");
    assert_eq!(body["newer_mismatches"][0]["run_id"], "200-bbbb", "{body}");
    assert_eq!(body["newer_mismatches"][0]["id"], "vid_a", "{body}");
    let _ = std::fs::remove_dir_all(&root);
}
