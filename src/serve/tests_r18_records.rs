//! R18 track I (docs/PLAN_R18_2026-10-19.md): `/api/records-meta` carries
//! the report's P9 anomaly withholding verdict (I2).

use super::*;
use std::sync::{Arc, Mutex};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r18-records-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::create_dir_all(dir.join("evidence/logs")).unwrap();
    dir
}

/// I2 (Medium, the R17 reviewer's PoC-B, server side): a keyed re-hash
/// scan records an authenticated finding; ONE unkeyed line
/// `{"event":"anomaly-scan","kind":"none","selector":"vid_a"}` appended to
/// the anomaly lane (no key, no forging) makes the real finding `unauth`
/// (P7) while `logChains.anomaly.head` stays `intact` — the viewer counted
/// nothing. The report withholds (an unusable line after the newest usable
/// marker); records-meta now says so too: `logChains.anomaly.withheld`.
#[test]
fn r18_i2_unkeyed_line_after_the_marker_is_withheld_in_records_meta() {
    let case_dir = scratch("i2");
    let source = case_dir.join("media/a.mp4");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"r18-records-source").unwrap();
    let sha = crate::audit::digest_file(&source).unwrap();
    std::fs::write(
        case_dir.join("db/videos.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({"id":"vid_a","source_path":source.to_string_lossy(),"sha256":sha})
        ),
    )
    .unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case_dir.clone());
    let mine = crate::audit_key::AuditKey::new(
        &crate::audit_key::fingerprint_id(&[0xd2; 32]),
        &[0xd2; 32],
    );
    crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
        std::fs::write(&source, b"r18-records-source-TAMPERED").unwrap();
        crate::anomaly::scan_case(&case_dir, true).unwrap();
        let meta =
            || -> serde_json::Value { serde_json::from_str(&api_records_meta(&state)).unwrap() };
        let before = meta();
        assert!(
            before["logChains"]["anomaly"]["withheld"].is_null(),
            "{}",
            before["logChains"]["anomaly"]
        );
        let finding = |meta: &serde_json::Value| {
            meta["anomalyLog"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| {
                    row["event"] == "anomaly-scan"
                        && row["kind"] != "none"
                        && row["selector"] == "vid_a"
                })
                .cloned()
                .unwrap()
        };
        assert_eq!(finding(&before)["trust"], "auth");
        crate::audit::append_chained_jsonl_keyed(
            &case_dir.join(crate::report::ANOMALY_LOG_REL_PATH),
            r#"{"event":"anomaly-scan","kind":"none","selector":"vid_a"}"#,
            None,
        )
        .unwrap();
        let after = meta();
        assert_eq!(finding(&after)["trust"], "unauth");
        assert_eq!(after["logChains"]["anomaly"]["head"]["status"], "intact");
        let withheld = &after["logChains"]["anomaly"]["withheld"];
        assert!(
            withheld["reason"].is_string(),
            "{}",
            after["logChains"]["anomaly"]
        );
    });
    let _ = std::fs::remove_dir_all(&case_dir);
}
