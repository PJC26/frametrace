pub mod core;
pub mod evidence;
pub mod helpers;
pub mod jobs;
pub mod marks;
pub mod merge_annotations;
pub mod metrics;
pub mod models;
pub mod scan;
pub mod sources;

pub use core::*;
pub use evidence::*;
pub(crate) use helpers::*;
pub use jobs::*;
pub use marks::*;
pub use metrics::*;
pub use models::*;
pub use scan::*;
pub use sources::*;

#[cfg(test)]
mod tests {
    use super::{
        EvidenceSourceInput, load_video_ids, register_evidence_source, summarize_case_db,
        write_scan_index,
    };
    use crate::case_db::IndexedVideoRow;
    use crate::model::{ProbeSummary, ScanOptions, ScanResult, SourceProfile, VideoRecord};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn writes_scan_records_to_sqlite_without_duplicate_rows() {
        let case_dir =
            std::env::temp_dir().join(format!("frametrace-case-db-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();

        let first = video("vid_000001", "/evidence/one.mp4", None);
        let first_row = indexed_row(&first);
        let result = scan_result(vec![first.clone()], 1);
        write_scan_index(&case_dir, &result, &[first_row]).unwrap();

        let rescanned = VideoRecord {
            sha256: Some("abc".to_string()),
            hash_status: "complete".to_string(),
            ..first
        };
        let result = scan_result(vec![rescanned.clone()], 2);
        write_scan_index(&case_dir, &result, &[indexed_row(&rescanned)]).unwrap();

        let summary = summarize_case_db(&case_dir).unwrap().unwrap();
        assert_eq!(summary.video_count, 1);
        assert_eq!(summary.scan_run_count, 2);

        let ids = load_video_ids(&case_dir).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].id, "vid_000001");
        assert_eq!(ids[0].source_path, "/evidence/one.mp4");

        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn preserves_manual_source_id_when_auto_registering_same_path() {
        let case_dir =
            std::env::temp_dir().join(format!("frametrace-source-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();
        let source_path = PathBuf::from("/evidence/source");

        let manual = register_evidence_source(
            &case_dir,
            &EvidenceSourceInput {
                kind: "folder".to_string(),
                path: source_path.clone(),
                source_id: Some("SD001".to_string()),
                write_protect: Some("hardware".to_string()),
                acquisition_tool: None,
                evidence_hash: None,
                notes: Some("intake".to_string()),
                metadata_json: None,
            },
        )
        .unwrap();
        let auto = register_evidence_source(
            &case_dir,
            &EvidenceSourceInput {
                kind: "folder".to_string(),
                path: source_path,
                source_id: None,
                write_protect: None,
                acquisition_tool: None,
                evidence_hash: None,
                notes: Some("auto".to_string()),
                metadata_json: None,
            },
        )
        .unwrap();

        assert_eq!(manual.source_id, "SD001");
        assert_eq!(auto.source_id, "SD001");
        assert_eq!(
            summarize_case_db(&case_dir)
                .unwrap()
                .unwrap()
                .evidence_source_count,
            1
        );

        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn source_reregistration_preserves_missing_custody_fields_and_accepts_updates() {
        let case_dir = std::env::temp_dir().join(format!(
            "frametrace-source-custody-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(case_dir.join("db")).unwrap();
        for manual_id in [None, Some("manual-source".to_string())] {
            let mut input = EvidenceSourceInput {
                kind: "raw-image".to_string(),
                path: PathBuf::from(if manual_id.is_some() {
                    "/evidence/manual.raw"
                } else {
                    "/evidence/automatic.raw"
                }),
                source_id: manual_id,
                write_protect: Some("hardware blocker".to_string()),
                acquisition_tool: Some("ewfexport".to_string()),
                evidence_hash: Some("a".repeat(64)),
                notes: Some("intake notes".to_string()),
                metadata_json: None,
            };
            let original = register_evidence_source(&case_dir, &input).unwrap();
            input.source_id = None;
            input.write_protect = None;
            input.acquisition_tool = None;
            input.evidence_hash = None;
            input.notes = None;
            let repeated = register_evidence_source(&case_dir, &input).unwrap();
            assert_eq!(original.source_id, repeated.source_id);
            let conn = super::open_case_db(&case_dir).unwrap();
            let read_fields = || {
                conn.query_row(
                    "SELECT write_protect, acquisition_tool, evidence_hash, notes FROM evidence_sources WHERE source_id = ?1",
                    [&original.source_id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?)),
                ).unwrap()
            };
            assert_eq!(
                read_fields(),
                (
                    Some("hardware blocker".to_string()),
                    Some("ewfexport".to_string()),
                    Some("a".repeat(64)),
                    Some("intake notes".to_string())
                )
            );
            input.write_protect = Some("read-only copy".to_string());
            input.acquisition_tool = Some("verified acquisition".to_string());
            input.evidence_hash = Some("b".repeat(64));
            input.notes = Some("updated notes".to_string());
            register_evidence_source(&case_dir, &input).unwrap();
            assert_eq!(
                read_fields(),
                (
                    input.write_protect,
                    input.acquisition_tool,
                    input.evidence_hash,
                    input.notes
                )
            );
        }
        let _ = fs::remove_dir_all(case_dir);
    }

    fn video(id: &str, source_path: &str, sha256: Option<String>) -> VideoRecord {
        VideoRecord {
            id: id.to_string(),
            source_path: PathBuf::from(source_path),
            relative_path: PathBuf::from(source_path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            extension: "mp4".to_string(),
            size_bytes: 42,
            modified_unix: Some(10),
            sha256,
            hash_status: "skipped".to_string(),
            probe: ProbeSummary::skipped(),
            confidence: "extension-candidate".to_string(),
            source_profile: SourceProfile::generic_media("test"),
        }
    }

    fn indexed_row(record: &VideoRecord) -> IndexedVideoRow {
        IndexedVideoRow {
            id: record.id.clone(),
            source_path: record.source_path.to_string_lossy().to_string(),
            file_url: format!("file://{}", record.source_path.display()),
            relative_path: record.relative_path.clone(),
            extension: record.extension.clone(),
            size_bytes: record.size_bytes,
            modified_unix: record.modified_unix,
            sha256: record.sha256.clone(),
            hash_status: record.hash_status.clone(),
            confidence: record.confidence.clone(),
            source_profile_json: record.source_profile.to_json(),
            duration_seconds: record.probe.duration_seconds,
            format_name: record.probe.format_name.clone(),
            video_codec: record.probe.video_codec.clone(),
            audio_codec: record.probe.audio_codec.clone(),
            width: record.probe.width.map(u64::from),
            height: record.probe.height.map(u64::from),
            ffprobe_ok: record.probe.ok,
            ffprobe_error: record.probe.error.clone(),
            ffprobe_json: record.probe.raw_json.clone(),
            record_json: record.to_json(),
        }
    }

    fn scan_result(records: Vec<VideoRecord>, scanned_unix: u64) -> ScanResult {
        ScanResult {
            source_path: PathBuf::from("/evidence"),
            scanned_unix,
            video_count: records.len(),
            total_bytes: records.iter().map(|record| record.size_bytes).sum(),
            unchanged_files: 0,
            resumed_from_checkpoint: 0,
            warnings: Vec::new(),
            options: ScanOptions::default(),
            records,
        }
    }
}
