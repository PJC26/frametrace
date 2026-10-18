//! R19 track I (docs/PLAN_R19_2026-10-19.md): `/api/records-meta` builds
//! `logChains.anomaly.withheld` from the same lane read as `anomalyLog`
//! (I2).

use super::*;
use std::sync::{Arc, Mutex};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r19-records-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::create_dir_all(dir.join("evidence/logs")).unwrap();
    dir
}

/// I2: an anomaly lane that cannot be read is `unreadable` in `withheld`
/// (with the read error), next to an empty `anomalyLog` — and the
/// interleaved-run lane of I1 is not withheld, its rows sent as read.
#[test]
fn r19_i2_records_meta_withheld_comes_from_the_rows_read() {
    let case_dir = scratch("i2");
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case_dir.clone());
    let lane = case_dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    let meta = || -> serde_json::Value { serde_json::from_str(&api_records_meta(&state)).unwrap() };
    crate::audit_key::with_test_keys(&[], || {
        // A directory where the lane file belongs: the read fails.
        std::fs::create_dir_all(&lane).unwrap();
        let body = meta();
        assert_eq!(body["logChains"]["anomalyLog"]["status"], "unreadable");
        assert_eq!(body["anomalyLog"], serde_json::json!([]));
        let withheld = &body["logChains"]["anomaly"]["withheld"];
        assert_eq!(withheld["reason_code"], "unreadable", "{withheld}");
        assert_eq!(withheld["codes"], serde_json::json!(["unreadable"]));
        let detail = body["logChains"]["anomalyLog"]["detail"].as_str().unwrap();
        assert!(
            withheld["reason"].as_str().unwrap().ends_with(detail),
            "{withheld} / {detail}"
        );
        std::fs::remove_dir_all(&lane).unwrap();
        for (ts, line) in [
            (100, r#""kind":"hash-mismatch","selector":"vid_a""#),
            (200, r#""kind":"hash-mismatch","selector":"vid_b""#),
        ] {
            crate::audit::append_chained_jsonl_keyed(
                &lane,
                &format!(
                    r#"{{"schema_version":1,"event":"anomaly-scan","scanned_unix":{ts},{line}}}"#
                ),
                None,
            )
            .unwrap();
        }
        for ts in [100, 200] {
            crate::audit::append_chained_jsonl_keyed(
                &lane,
                &format!(
                    r#"{{"schema_version":1,"event":"anomaly-scan-run","scanned_unix":{ts}}}"#
                ),
                None,
            )
            .unwrap();
        }
        let body = meta();
        assert_eq!(body["anomalyLog"].as_array().unwrap().len(), 4);
        assert!(
            body["logChains"]["anomaly"]["withheld"].is_null(),
            "{}",
            body["logChains"]["anomaly"]
        );
    });
    let _ = std::fs::remove_dir_all(&case_dir);
}
