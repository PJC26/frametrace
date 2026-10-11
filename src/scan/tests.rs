#[test]
fn hash_with_restat_records_error_instead_of_aborting() {
    let dir = std::env::temp_dir().join(format!(
        "frametrace-scan-hash-restat-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.mp4");
    std::fs::write(&path, b"abc").unwrap();
    let metadata = std::fs::metadata(&path).unwrap();

    let (sha, status, warning) = super::hash_with_restat(&path, &metadata);
    assert_eq!(status, "complete");
    assert_eq!(
        sha.as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert!(warning.is_none());

    // Grown after the "before" stat: the digest is not trusted.
    std::fs::write(&path, b"abcd").unwrap();
    let (sha, status, warning) = super::hash_with_restat(&path, &metadata);
    assert_eq!(status, "changed-during-hash");
    assert!(sha.is_none());
    assert!(warning.is_some());

    // Unreadable (vanished) file: recorded as an error, no panic/abort.
    std::fs::remove_file(&path).unwrap();
    let (sha, status, warning) = super::hash_with_restat(&path, &metadata);
    assert_eq!(status, "error");
    assert!(sha.is_none());
    assert!(warning.unwrap().contains("hash_status=error"));
    let _ = std::fs::remove_dir_all(&dir);
}

use super::{
    ResumeMode, RunCheckpoint, collect_video_candidates, excluded_case_dirs, looks_like_video,
    merge_existing_with_scan, scan_fingerprint, set_json_field,
};
use crate::model::{ProbeSummary, ScanOptions, ScanResult, SourceProfile, VideoRecord};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn set_json_field_appends_without_reordering_existing_keys() {
    // The JSONL contract is byte-stable for already-indexed evidence:
    // stale markers must not rewrite the whole object.
    let line = r#"{"z_last":1,"a_first":"x"}"#;
    let updated = set_json_field(line, "index_status", "\"stale\"");
    let expected = r#"{"z_last":1,"a_first":"x","index_status":"stale"}"#;
    assert_eq!(updated, expected);
}

#[test]
fn set_json_field_replaces_existing_scalar_in_place() {
    let line = r#"{"id":"vid_1","stale_since_unix":5,"ext":"mp4"}"#;
    let updated = set_json_field(line, "stale_since_unix", "9");
    assert_eq!(
        updated,
        r#"{"id":"vid_1","stale_since_unix":9,"ext":"mp4"}"#
    );
}

/// A nested key (e.g. an ffprobe metadata tag) sharing the stale
/// marker's name must never be touched: only depth-1 members match.
/// Evidence files can carry tags named `index_status` planted via
/// `ffmpeg -metadata`, so this is an adversarial-input guard.
#[test]
fn set_json_field_ignores_nested_keys_with_the_same_name() {
    let line = r#"{"id":"vid_1","ffprobe":{"index_status":"attacker tag","streams":[]},"index_status":null,"ext":"mp4"}"#;
    let updated = set_json_field(line, "index_status", "\"stale\"");
    assert!(
        updated.contains(r#""ffprobe":{"index_status":"attacker tag""#),
        "nested tag must stay byte-identical: {updated}"
    );
    assert!(updated.contains(r#""index_status":"stale""#), "{updated}");
}

#[test]
fn set_json_field_appends_when_top_level_key_missing() {
    let line = r#"{"id":"vid_1","ffprobe":{"index_status":"nested decoy"}}"#;
    let updated = set_json_field(line, "stale_since_unix", "7");
    assert!(updated.ends_with(r#","stale_since_unix":7}"#), "{updated}");
    assert!(updated.contains(r#""ffprobe":{"index_status":"nested decoy"}"#));
}

#[test]
fn recognizes_common_video_extensions() {
    assert!(looks_like_video(&PathBuf::from("a.MP4")));
    assert!(looks_like_video(&PathBuf::from("camera.dav")));
    assert!(looks_like_video(&PathBuf::from("export.g64x")));
    assert!(looks_like_video(&PathBuf::from("phone.glv")));
    assert!(!looks_like_video(&PathBuf::from("notes.txt")));
}

#[test]
fn recognizes_mp4_magic_without_extension() {
    let path = std::env::temp_dir().join(format!(
        "forensic-video-workstation-test-{}",
        std::process::id()
    ));
    fs::write(&path, b"\0\0\0\x18ftypmp42").unwrap();
    assert!(looks_like_video(&path));
    let _ = fs::remove_file(path);
}

#[test]
fn rejects_files_that_mention_ftyp_in_text() {
    let path = std::env::temp_dir().join(format!(
        "frametrace-false-magic-test-{}.jsonl",
        std::process::id()
    ));
    fs::write(
        &path,
        r#"{"signature":"mp4-ftyp","note":"audit text mentioning ftyp boxes"}"#,
    )
    .unwrap();
    assert!(!looks_like_video(&path));
    let _ = fs::remove_file(path);
}

#[test]
fn rejects_scanning_the_case_directory_as_source() {
    let root = std::env::temp_dir().join(format!(
        "frametrace-case-source-reject-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let canonical = root.canonicalize().unwrap();
    let err = excluded_case_dirs(&canonical, &canonical).unwrap_err();
    assert!(err.contains("source directory cannot be the FrameTrace case directory"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn collect_candidates_skips_excluded_case_output_tree() {
    let root = std::env::temp_dir().join(format!(
        "frametrace-scan-exclusion-test-{}",
        std::process::id()
    ));
    let source_dir = root.join("source");
    let case_dir = source_dir.join("case");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(source_dir.join("camera")).unwrap();
    fs::create_dir_all(case_dir.join("artifacts/clips")).unwrap();
    fs::write(source_dir.join("camera/real.mp4"), b"not actually media").unwrap();
    fs::write(
        case_dir.join("artifacts/clips/derived.mp4"),
        b"not actually media",
    )
    .unwrap();

    let collection =
        collect_video_candidates(&source_dir, None, std::slice::from_ref(&case_dir)).unwrap();
    let names = collection
        .files
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["real.mp4"]);
    assert!(
        collection
            .warnings
            .iter()
            .any(|warning| warning.contains("skipped FrameTrace case output directory"))
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn deserializes_published_jsonl_records_through_serde() {
    let line = r#"{"id":"vid_000001","source_path":"C:\\Evidence\\a\tb.mp4","relative_path":"a\tb.mp4","extension":"mp4","size_bytes":1,"modified_unix":null,"sha256":null,"hash_status":"skipped","confidence":"extension-candidate","source_profile":{"lane":"generic-video","vendor":"Generic media","parser":"generic_media","confidence":"medium","recommended_action":"Use ffprobe/FFmpeg first; preserve original and export derived clips only when requested.","evidence":["extension"]},"duration_seconds":12.5,"format_name":"mov,mp4,m4a","video_codec":"h264","audio_codec":null,"width":1920,"height":1080,"ffprobe_ok":true,"ffprobe_error":null,"ffprobe":null}"#;
    let record: VideoRecord = serde_json::from_str(line).unwrap();
    assert_eq!(record.id, "vid_000001");
    assert_eq!(record.source_path, PathBuf::from("C:\\Evidence\\a\tb.mp4"));
    assert!(record.probe.ok);
    assert_eq!(record.probe.width, Some(1920));
    assert_eq!(record.probe.duration_seconds, Some(12.5));

    // Round-trip keeps the published contract parseable.
    let reparsed: VideoRecord = serde_json::from_str(&record.to_json()).unwrap();
    assert_eq!(reparsed.id, record.id);
    assert_eq!(reparsed.probe.width, record.probe.width);
}

/// An indexed file that still exists but whose metadata cannot be read
/// (here: a self-referential symlink, ELOOP) must be carried forward
/// unchanged, not marked stale; a genuinely missing one is still stale.
#[cfg(unix)]
#[test]
fn metadata_error_carries_record_forward_instead_of_stale() {
    let case_dir = std::env::temp_dir().join(format!(
        "frametrace-metadata-error-merge-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&case_dir);
    fs::create_dir_all(case_dir.join("db")).unwrap();
    let looped = case_dir.join("loop.mp4");
    std::os::unix::fs::symlink(&looped, &looped).unwrap();
    assert!(fs::metadata(&looped).is_err());
    assert_eq!(
        super::source_presence(&looped),
        super::SourcePresence::MetadataError
    );
    let unreadable = VideoRecord {
        id: "vid_000001".to_string(),
        source_path: looped.clone(),
        relative_path: "loop.mp4".to_string(),
        extension: "mp4".to_string(),
        size_bytes: 1,
        modified_unix: None,
        sha256: None,
        hash_status: "skipped".to_string(),
        probe: ProbeSummary::skipped(),
        confidence: "extension-candidate".to_string(),
        source_profile: SourceProfile::generic_media("test"),
    };
    let gone = VideoRecord {
        id: "vid_000002".to_string(),
        source_path: case_dir.join("gone.mp4"),
        ..unreadable.clone()
    };
    let original_line = unreadable.to_json();
    fs::write(
        case_dir.join("db/videos.jsonl"),
        format!("{}\n{}\n", original_line, gone.to_json()),
    )
    .unwrap();
    let result = ScanResult {
        source_path: case_dir.clone(),
        scanned_unix: 5,
        video_count: 0,
        total_bytes: 0,
        unchanged_files: 0,
        resumed_from_checkpoint: 0,
        warnings: Vec::new(),
        options: ScanOptions::default(),
        records: Vec::new(),
    };
    let merged = merge_existing_with_scan(&case_dir, &result).unwrap();
    assert_eq!(merged.len(), 2, "{merged:?}");
    assert_eq!(merged[0].json_line, original_line.trim());
    assert!(!merged[0].json_line.contains("stale"));
    assert!(merged[1].json_line.contains(r#""index_status":"stale""#));
    let _ = fs::remove_dir_all(case_dir);
}

/// A file that vanished while indexed under BOTH the legacy `\\?\` and
/// clean spellings must converge to one stale record, not two.
#[test]
fn stale_marking_dedupes_legacy_and_clean_spellings() {
    let case_dir = std::env::temp_dir().join(format!(
        "frametrace-stale-dedupe-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&case_dir);
    fs::create_dir_all(case_dir.join("db")).unwrap();

    let legacy = VideoRecord {
        id: "vid_000001".to_string(),
        source_path: PathBuf::from(r"\\?\C:\gone\one.mp4"),
        relative_path: "one.mp4".to_string(),
        extension: "mp4".to_string(),
        size_bytes: 1,
        modified_unix: None,
        sha256: None,
        hash_status: "skipped".to_string(),
        probe: ProbeSummary::skipped(),
        confidence: "extension-candidate".to_string(),
        source_profile: SourceProfile::generic_media("test"),
    };
    let clean = VideoRecord {
        id: "vid_000002".to_string(),
        source_path: PathBuf::from(r"C:\gone\one.mp4"),
        ..legacy.clone()
    };
    fs::write(
        case_dir.join("db/videos.jsonl"),
        format!("{}\n{}\n", legacy.to_json(), clean.to_json()),
    )
    .unwrap();

    let result = ScanResult {
        source_path: PathBuf::from(r"C:\elsewhere"),
        scanned_unix: 5,
        video_count: 0,
        total_bytes: 0,
        unchanged_files: 0,
        resumed_from_checkpoint: 0,
        warnings: Vec::new(),
        options: ScanOptions::default(),
        records: Vec::new(),
    };

    let merged = merge_existing_with_scan(&case_dir, &result).unwrap();
    assert_eq!(
        merged.len(),
        1,
        "duplicate spellings must converge: {merged:?}"
    );
    assert!(merged[0].json_line.contains(r#""index_status":"stale""#));
    let _ = fs::remove_dir_all(case_dir);
}

#[test]
fn merges_scan_records_without_dropping_existing_case_index() {
    let case_dir =
        std::env::temp_dir().join(format!("frametrace-merge-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&case_dir);
    fs::create_dir_all(case_dir.join("db")).unwrap();

    let first = VideoRecord {
        id: "vid_000001".to_string(),
        source_path: PathBuf::from("/evidence/one.mp4"),
        relative_path: "one.mp4".to_string(),
        extension: "mp4".to_string(),
        size_bytes: 1,
        modified_unix: None,
        sha256: None,
        hash_status: "skipped".to_string(),
        probe: ProbeSummary::skipped(),
        confidence: "extension-candidate".to_string(),
        source_profile: SourceProfile::generic_media("test"),
    };
    let second = VideoRecord {
        id: "vid_000002".to_string(),
        source_path: PathBuf::from("/evidence/two.mp4"),
        relative_path: "two.mp4".to_string(),
        extension: "mp4".to_string(),
        size_bytes: 2,
        modified_unix: None,
        sha256: None,
        hash_status: "skipped".to_string(),
        probe: ProbeSummary::skipped(),
        confidence: "extension-candidate".to_string(),
        source_profile: SourceProfile::generic_media("test"),
    };
    fs::write(
        case_dir.join("db/videos.jsonl"),
        format!("{}\n{}\n", first.to_json(), second.to_json()),
    )
    .unwrap();

    let rescanned_second = VideoRecord {
        sha256: Some("abc".to_string()),
        hash_status: "complete".to_string(),
        ..second
    };
    let result = ScanResult {
        source_path: PathBuf::from("/evidence"),
        scanned_unix: 1,
        video_count: 1,
        total_bytes: 2,
        unchanged_files: 0,
        resumed_from_checkpoint: 0,
        warnings: Vec::new(),
        options: ScanOptions::default(),
        records: vec![rescanned_second],
    };

    let merged = merge_existing_with_scan(&case_dir, &result).unwrap();
    assert_eq!(merged.len(), 2);
    assert!(merged[0].json_line.contains("\"id\":\"vid_000001\""));
    assert!(merged[0].json_line.contains("\"index_status\":\"stale\""));
    assert!(merged[0].json_line.contains("\"stale_since_unix\":1"));
    assert!(merged[1].json_line.contains("\"id\":\"vid_000002\""));
    assert!(merged[1].json_line.contains("\"sha256\":\"abc\""));

    let _ = fs::remove_dir_all(case_dir);
}

#[test]
fn upgrades_legacy_extended_prefix_paths_without_duplicating_ids() {
    let case_dir = std::env::temp_dir().join(format!(
        "frametrace-legacy-path-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&case_dir);
    fs::create_dir_all(case_dir.join("db")).unwrap();

    let legacy = VideoRecord {
        id: "vid_000001".to_string(),
        source_path: PathBuf::from(r"\\?\C:\evidence\one.mp4"),
        relative_path: "one.mp4".to_string(),
        extension: "mp4".to_string(),
        size_bytes: 1,
        modified_unix: None,
        sha256: None,
        hash_status: "skipped".to_string(),
        probe: ProbeSummary::skipped(),
        confidence: "extension-candidate".to_string(),
        source_profile: SourceProfile::generic_media("test"),
    };
    fs::write(
        case_dir.join("db/videos.jsonl"),
        format!("{}\n", legacy.to_json()),
    )
    .unwrap();

    let rescanned = VideoRecord {
        source_path: PathBuf::from(r"C:\evidence\one.mp4"),
        sha256: Some("abc".to_string()),
        hash_status: "complete".to_string(),
        ..legacy
    };
    let result = ScanResult {
        source_path: PathBuf::from(r"C:\evidence"),
        scanned_unix: 2,
        video_count: 1,
        total_bytes: 1,
        unchanged_files: 0,
        resumed_from_checkpoint: 0,
        warnings: Vec::new(),
        options: ScanOptions::default(),
        records: vec![rescanned],
    };

    let merged = merge_existing_with_scan(&case_dir, &result).unwrap();
    assert_eq!(merged.len(), 1, "same file must not duplicate records");
    assert!(merged[0].json_line.contains("\"id\":\"vid_000001\""));
    assert!(merged[0].json_line.contains(r"C:\\evidence\\one.mp4"));
    assert!(!merged[0].json_line.contains("index_status"));

    let _ = fs::remove_dir_all(case_dir);
}

fn incremental_fixture(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "frametrace-incremental-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let case_dir = root.join("case");
    let source_dir = root.join("source");
    fs::create_dir_all(case_dir.join("db")).unwrap();
    fs::create_dir_all(&source_dir).unwrap();
    (case_dir, source_dir)
}

fn read_jsonl_ids_with_hash(case_dir: &std::path::Path) -> Vec<(String, bool)> {
    let text = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                value["id"].as_str().unwrap().to_string(),
                value["sha256"].is_string(),
            )
        })
        .collect()
}

/// Unchanged files (same size+mtime) are reused verbatim: no re-hash,
/// no re-probe, and the stored JSONL line survives byte-identical.
#[test]
fn incremental_rescan_skips_unchanged_files() {
    let (case_dir, source_dir) = incremental_fixture("skip");
    fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
    fs::write(source_dir.join("b.mp4"), b"\0\0\0\x18ftypmp42two").unwrap();
    let options = ScanOptions {
        hash_files: false,
        use_ffprobe: false,
        ..ScanOptions::default()
    };
    let first =
        super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
    assert_eq!(first.video_count, 2);
    let before = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();

    let rescan = super::scan_folder(
        &case_dir,
        &source_dir,
        &ScanOptions {
            incremental: true,
            ..options.clone()
        },
        ResumeMode::Auto,
        None,
    )
    .unwrap();
    assert_eq!(rescan.video_count, 0, "nothing was re-processed");
    assert_eq!(rescan.unchanged_files, 2);
    let after = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
    assert_eq!(before, after, "unchanged records must stay verbatim");

    // ...but --hash against never-hashed records must NOT skip them.
    let hashed = super::scan_folder(
        &case_dir,
        &source_dir,
        &ScanOptions {
            hash_files: true,
            incremental: true,
            ..ScanOptions::default()
        },
        ResumeMode::Auto,
        None,
    )
    .unwrap();
    assert_eq!(hashed.video_count, 2);
    assert_eq!(hashed.unchanged_files, 0);
    let rows = read_jsonl_ids_with_hash(&case_dir);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(_, hashed)| *hashed));

    let _ = fs::remove_dir_all(case_dir.parent().unwrap());
}

/// A file whose size changed is re-indexed; one that disappeared is
/// marked stale; a brand-new file is indexed with a fresh id.
#[test]
fn incremental_rescan_reindexes_changed_and_marks_deleted_stale() {
    let (case_dir, source_dir) = incremental_fixture("change");
    fs::write(source_dir.join("keep.mp4"), b"\0\0\0\x18ftypmp42keep").unwrap();
    fs::write(source_dir.join("change.mp4"), b"\0\0\0\x18ftypmp42v1").unwrap();
    fs::write(source_dir.join("gone.mp4"), b"\0\0\0\x18ftypmp42gone").unwrap();
    let options = ScanOptions {
        hash_files: true,
        use_ffprobe: false,
        ..ScanOptions::default()
    };
    let first =
        super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
    assert_eq!(first.video_count, 3);
    let index_before = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
    let keep_line = index_before
        .lines()
        .find(|line| line.contains("keep.mp4"))
        .unwrap()
        .to_string();
    let change_sha = {
        let line = index_before
            .lines()
            .find(|line| line.contains("change.mp4"))
            .unwrap();
        serde_json::from_str::<serde_json::Value>(line).unwrap()["sha256"]
            .as_str()
            .unwrap()
            .to_string()
    };

    fs::write(
        source_dir.join("change.mp4"),
        b"\0\0\0\x18ftypmp42v2-longer",
    )
    .unwrap();
    fs::remove_file(source_dir.join("gone.mp4")).unwrap();
    fs::write(source_dir.join("new.mov"), b"\0\0\0\x18ftypmp42new").unwrap();

    let rescan = super::scan_folder(
        &case_dir,
        &source_dir,
        &ScanOptions {
            incremental: true,
            ..options
        },
        ResumeMode::Auto,
        None,
    )
    .unwrap();
    assert_eq!(rescan.video_count, 2, "changed + new files processed");
    assert_eq!(rescan.unchanged_files, 1, "keep.mp4 skipped");

    let index_after = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
    let lines: Vec<&str> = index_after
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 4, "stale record stays in the index");

    let keep_after = lines.iter().find(|line| line.contains("keep.mp4")).unwrap();
    assert_eq!(*keep_after, keep_line, "unchanged record is verbatim");
    let gone = lines.iter().find(|line| line.contains("gone.mp4")).unwrap();
    assert!(gone.contains("\"index_status\":\"stale\""), "{gone}");
    assert!(gone.contains("\"stale_since_unix\""));
    let changed = lines
        .iter()
        .find(|line| line.contains("change.mp4"))
        .unwrap();
    let changed_sha = serde_json::from_str::<serde_json::Value>(changed).unwrap()["sha256"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(changed_sha, change_sha, "changed file re-hashed");
    assert!(!changed.contains("index_status"));
    assert!(lines.iter().any(|line| line.contains("new.mov")));

    let _ = fs::remove_dir_all(case_dir.parent().unwrap());
}

/// A record already flagged must not be skipped: reprocessing clears the
/// marker by regenerating the record.
#[test]
fn incremental_rescan_refreshes_flagged_records() {
    let (case_dir, source_dir) = incremental_fixture("flagged");
    fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
    let options = ScanOptions {
        use_ffprobe: false,
        ..ScanOptions::default()
    };
    super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
    // Flag the record, then rescan unchanged: the flag forces a fresh
    // record, so the flag does not linger on live evidence.
    let index_path = case_dir.join("db/videos.jsonl");
    let flagged = set_json_field(
        &fs::read_to_string(&index_path).unwrap(),
        "index_status",
        "\"stale\"",
    );
    fs::write(&index_path, flagged).unwrap();
    let rescan = super::scan_folder(
        &case_dir,
        &source_dir,
        &ScanOptions {
            incremental: true,
            ..options
        },
        ResumeMode::Auto,
        None,
    )
    .unwrap();
    assert_eq!(rescan.video_count, 1);
    assert_eq!(rescan.unchanged_files, 0);
    let index_after = fs::read_to_string(&index_path).unwrap();
    assert!(!index_after.contains("index_status"), "{index_after}");
    let _ = fs::remove_dir_all(case_dir.parent().unwrap());
}

/// Writes the checkpoint a crashed run would have left: header +
/// `record_lines` already-completed records.
fn leave_checkpoint(
    case_dir: &Path,
    source_dir: &Path,
    options: &ScanOptions,
    record_lines: &[String],
) {
    // scan_folder fingerprints the CANONICALIZED source path, so the
    // crafted checkpoint must be computed the same way.
    let source_dir = crate::util::canonicalize_display(source_dir).unwrap();
    let candidates = collect_video_candidates(&source_dir, None, &[]).unwrap();
    let fingerprint = scan_fingerprint(&source_dir, options, &candidates.files);
    let mut checkpoint = RunCheckpoint::begin(
        &case_dir.join("db/scan-progress.jsonl"),
        "scan-folder",
        &fingerprint,
        ResumeMode::Auto,
    )
    .unwrap();
    for line in record_lines {
        checkpoint.append_line(line).unwrap();
    }
    // Drop without finish: the interrupted run's state stays on disk.
}

/// An interrupted run's checkpoint replays finished records verbatim —
/// no re-hash, no re-probe — and the rerun still indexes everything.
#[test]
fn resume_replays_checkpointed_records() {
    let (case_dir, source_dir) = incremental_fixture("resume");
    fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
    fs::write(source_dir.join("b.mp4"), b"\0\0\0\x18ftypmp42two").unwrap();
    fs::write(source_dir.join("c.mp4"), b"\0\0\0\x18ftypmp42three").unwrap();
    let options = ScanOptions {
        hash_files: true,
        use_ffprobe: false,
        ..ScanOptions::default()
    };
    let first =
        super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
    assert_eq!(first.video_count, 3);
    let record_lines: Vec<String> = fs::read_to_string(case_dir.join("db/videos.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(record_lines.len(), 3);

    // Simulate the crash: index outputs never written, checkpoint with
    // the first two processed files survives.
    for file in [
        "db/videos.jsonl",
        "db/video_index.json",
        "db/video_paths.tsv",
    ] {
        let _ = fs::remove_file(case_dir.join(file));
    }
    leave_checkpoint(&case_dir, &source_dir, &options, &record_lines[..2]);

    let second =
        super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
    assert_eq!(second.resumed_from_checkpoint, 2);
    assert_eq!(second.video_count, 3);
    assert!(
        !case_dir.join("db/scan-progress.jsonl").exists(),
        "checkpoint deleted on full success"
    );
    let index = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
    for name in ["a.mp4", "b.mp4", "c.mp4"] {
        assert!(index.contains(name), "index missing {name}");
    }
    // The replayed lines are byte-identical to what the crashed run
    // computed — resume does not rewrite prior work.
    for line in index.lines().filter(|line| line.contains(".mp4")) {
        if line.contains("a.mp4") {
            assert_eq!(line, record_lines[0]);
        } else if line.contains("b.mp4") {
            assert_eq!(line, record_lines[1]);
        }
    }
    // The resume is audit-logged.
    let scan_log = fs::read_to_string(case_dir.join("evidence/logs/scan-log.jsonl")).unwrap();
    assert!(scan_log.contains("\"event\":\"scan-resume\""), "{scan_log}");
    assert!(scan_log.contains("\"resumed_records\":2"), "{scan_log}");

    let _ = fs::remove_dir_all(case_dir.parent().unwrap());
}

/// A checkpoint whose input fingerprint does not match (e.g. recorded
/// against different scan options) is discarded, not silently reused.
#[test]
fn resume_rejects_stale_fingerprint_and_scans_fresh() {
    let (case_dir, source_dir) = incremental_fixture("stalefp");
    fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
    let options = ScanOptions {
        use_ffprobe: false,
        ..ScanOptions::default()
    };
    let checkpoint_path = case_dir.join("db/scan-progress.jsonl");
    {
        let mut checkpoint =
            RunCheckpoint::begin(&checkpoint_path, "scan-folder", "bogus", ResumeMode::Auto)
                .unwrap();
        checkpoint.append_line("{\"id\":\"vid_000001\"}").unwrap();
    }

    let result =
        super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
    assert_eq!(result.resumed_from_checkpoint, 0);
    assert_eq!(result.video_count, 1);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("different inputs")),
        "expected a stale-checkpoint warning: {:?}",
        result.warnings
    );
    let _ = fs::remove_dir_all(case_dir.parent().unwrap());
}

/// A checkpoint file torn mid-append drops only the incomplete tail:
/// complete records still replay.
#[test]
fn resume_tolerates_torn_checkpoint_tail() {
    let (case_dir, source_dir) = incremental_fixture("torntail");
    fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
    fs::write(source_dir.join("b.mp4"), b"\0\0\0\x18ftypmp42two").unwrap();
    let options = ScanOptions {
        use_ffprobe: false,
        ..ScanOptions::default()
    };
    let first =
        super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
    assert_eq!(first.video_count, 2);
    let record_lines: Vec<String> = fs::read_to_string(case_dir.join("db/videos.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect();
    for file in [
        "db/videos.jsonl",
        "db/video_index.json",
        "db/video_paths.tsv",
    ] {
        let _ = fs::remove_file(case_dir.join(file));
    }
    leave_checkpoint(&case_dir, &source_dir, &options, &record_lines[..1]);
    // Torn tail: partial JSON appended without a newline.
    use std::io::Write;
    let mut handle = fs::OpenOptions::new()
        .append(true)
        .open(case_dir.join("db/scan-progress.jsonl"))
        .unwrap();
    handle.write_all(b"{\"id\":\"vid_").unwrap();
    drop(handle);

    let second =
        super::scan_folder(&case_dir, &source_dir, &options, ResumeMode::Auto, None).unwrap();
    assert_eq!(second.resumed_from_checkpoint, 1);
    assert_eq!(second.video_count, 2);
    let _ = fs::remove_dir_all(case_dir.parent().unwrap());
}

fn hashes_of_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> std::collections::HashMap<String, (Option<String>, String)> {
    lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                value["relative_path"].as_str().unwrap().to_string(),
                (
                    value["sha256"].as_str().map(str::to_string),
                    value["hash_status"].as_str().unwrap().to_string(),
                ),
            )
        })
        .collect()
}

fn jsonl_hashes(case_dir: &Path) -> std::collections::HashMap<String, (Option<String>, String)> {
    let text = fs::read_to_string(case_dir.join("db/videos.jsonl")).unwrap();
    hashes_of_lines(text.lines())
}

/// Blocker 1: a lighter rescan (no --hash, with or without --incremental —
/// the workstation's phase 1 on an existing case) must never downgrade a
/// recorded SHA-256 to `hash_status=skipped`, in the JSONL index, the
/// index JSON or case.db.
#[test]
fn lighter_rescan_keeps_recorded_hashes() {
    let (case_dir, source_dir) = incremental_fixture("keephash");
    fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
    fs::write(source_dir.join("b.mp4"), b"\0\0\0\x18ftypmp42two").unwrap();
    let hashed = ScanOptions {
        hash_files: true,
        use_ffprobe: false,
        ..ScanOptions::default()
    };
    super::scan_folder(&case_dir, &source_dir, &hashed, ResumeMode::Auto, None).unwrap();
    let before = jsonl_hashes(&case_dir);
    assert_eq!(before.len(), 2);
    assert!(
        before
            .values()
            .all(|(sha, status)| sha.is_some() && status == "complete")
    );

    for incremental in [false, true] {
        super::scan_folder(
            &case_dir,
            &source_dir,
            &ScanOptions {
                hash_files: false,
                use_ffprobe: false,
                incremental,
                ..ScanOptions::default()
            },
            ResumeMode::Auto,
            None,
        )
        .unwrap();
        assert_eq!(
            jsonl_hashes(&case_dir),
            before,
            "incremental={incremental}: lighter pass dropped hashes"
        );
        let index = fs::read_to_string(case_dir.join("db/video_index.json")).unwrap();
        assert!(!index.contains("\"hash_status\":\"skipped\""), "{index}");
        let db_lines = crate::case_db::load_video_record_lines(&case_dir).unwrap();
        assert_eq!(
            hashes_of_lines(db_lines.iter().map(String::as_str)),
            before,
            "incremental={incremental}: case.db lost hashes"
        );
    }

    // A changed file is never credited with its old hash.
    fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one-changed").unwrap();
    super::scan_folder(
        &case_dir,
        &source_dir,
        &ScanOptions {
            hash_files: false,
            use_ffprobe: false,
            ..ScanOptions::default()
        },
        ResumeMode::Auto,
        None,
    )
    .unwrap();
    let after = jsonl_hashes(&case_dir);
    assert_eq!(after["a.mp4"], (None, "skipped".to_string()));
    assert_eq!(after["b.mp4"], before["b.mp4"]);
    let _ = fs::remove_dir_all(case_dir.parent().unwrap());
}

/// Blocker 2: cancel the phase-2 `--incremental --hash` pass mid-way,
/// re-run the analysis (phase 1 = `--incremental --no-ffprobe`, a
/// different checkpoint fingerprint), then phase 2 again. The files the
/// cancelled pass already hashed must not be hashed again. The crafted
/// checkpoint carries a sentinel digest no real hash of these files
/// produces, so its survival proves no re-hash happened.
#[test]
fn rerun_after_cancelled_hash_pass_reuses_computed_hashes() {
    let (case_dir, source_dir) = incremental_fixture("resumehash");
    fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
    fs::write(source_dir.join("b.mp4"), b"\0\0\0\x18ftypmp42two").unwrap();
    fs::write(source_dir.join("c.mp4"), b"\0\0\0\x18ftypmp42three").unwrap();
    // Exactly the options the workstation passes (phase1_scan_args and
    // run_folder_enrichment with hashing on, ffprobe off).
    let phase1 = ScanOptions {
        hash_files: false,
        use_ffprobe: false,
        incremental: true,
        ..ScanOptions::default()
    };
    let phase2 = ScanOptions {
        hash_files: true,
        ..phase1.clone()
    };
    super::scan_folder(&case_dir, &source_dir, &phase1, ResumeMode::Auto, None).unwrap();

    // The cancelled phase 2 checkpointed a.mp4 and b.mp4 before the kill.
    let sentinel = "f".repeat(64);
    let lines: Vec<String> = fs::read_to_string(case_dir.join("db/videos.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| line.contains("a.mp4") || line.contains("b.mp4"))
        .map(|line| {
            let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
            value["sha256"] = serde_json::Value::String(sentinel.clone());
            value["hash_status"] = serde_json::Value::String("complete".into());
            value.to_string()
        })
        .collect();
    assert_eq!(lines.len(), 2);
    leave_checkpoint(&case_dir, &source_dir, &phase2, &lines);

    // Re-analysis: phase 1 first (its fingerprint resets the checkpoint)...
    super::scan_folder(&case_dir, &source_dir, &phase1, ResumeMode::Auto, None).unwrap();
    let mid = jsonl_hashes(&case_dir);
    assert_eq!(mid["a.mp4"].0.as_deref(), Some(sentinel.as_str()));
    assert_eq!(mid["b.mp4"].0.as_deref(), Some(sentinel.as_str()));
    assert_eq!(mid["c.mp4"].0, None);

    // ...then phase 2 hashes only the file the cancelled run never reached.
    let rerun =
        super::scan_folder(&case_dir, &source_dir, &phase2, ResumeMode::Auto, None).unwrap();
    assert_eq!(rerun.unchanged_files, 2, "a/b reused, not re-hashed");
    assert_eq!(rerun.video_count, 1, "only c.mp4 processed");
    let done = jsonl_hashes(&case_dir);
    assert_eq!(done["a.mp4"].0.as_deref(), Some(sentinel.as_str()));
    assert_eq!(done["b.mp4"].0.as_deref(), Some(sentinel.as_str()));
    let c_sha = done["c.mp4"].0.clone().expect("c.mp4 hashed");
    assert_ne!(c_sha, sentinel);
    let _ = fs::remove_dir_all(case_dir.parent().unwrap());
}
