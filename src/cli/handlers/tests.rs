use super::case::publish_case_manifest;
use super::*;
use crate::audit;
use crate::checkpoint::{self, ResumeMode, RunCheckpoint};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[test]
fn manifest_write_failure_retains_partial_file_and_blocks_reinit() {
    let root = std::env::temp_dir().join(format!("frametrace-init-partial-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let err = publish_case_manifest(&root, "{\"case_id\":\"FT-test\"}", |file, _| {
        file.write_all(b"{\"case_id\":")?;
        Err(std::io::Error::other("injected write failure"))
    })
    .unwrap_err();
    assert!(err.to_string().contains("injected write failure"));
    let manifest_path = root.join("case.json");
    assert_eq!(fs::read(&manifest_path).unwrap(), b"{\"case_id\":");
    assert!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&manifest_path).unwrap()).is_err()
    );
    assert!(init_case(&root, &InitCaseOptions::default()).is_err());
    assert_eq!(fs::read(&manifest_path).unwrap(), b"{\"case_id\":");
    assert!(!root.join(".case-test.tmp").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_publication_exclusively_creates_final_path() {
    let root =
        std::env::temp_dir().join(format!("frametrace-init-exclusive-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let manifest_path = root.join("case.json");
    let text = "{\"case_id\":\"FT-test\"}";
    publish_case_manifest(&root, text, |file, text| {
        assert!(manifest_path.is_file());
        assert!(!root.join(".case-test.tmp").exists());
        file.write_all(text.as_bytes())
    })
    .unwrap();
    let err = publish_case_manifest(&root, "replacement", |_, _| {
        panic!("must not write an existing manifest")
    })
    .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(manifest_path).unwrap(), text);
    assert!(root.join("evidence/logs").is_dir());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn export_marks_output_stays_inside_the_case() {
    let base = std::env::temp_dir().join(format!(
        "frametrace-marks-export-test-{}",
        std::process::id()
    ));
    let case_dir = base.join("case");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&case_dir).unwrap();
    std::fs::write(
        case_dir.join("case.json"),
        r#"{"case_id":"FT-marks-export"}"#,
    )
    .unwrap();
    // A sibling that shares the case directory's name prefix must not
    // pass containment — the check compares canonical parents, not
    // string prefixes, so `<case>-evil/out.json` resolves outside.
    let sibling = base.join("case-evil").join("out.json");
    let err = export_marks(&case_dir, Some(&sibling)).unwrap_err();
    assert!(err.contains("inside the case directory"), "{err}");
    assert!(!sibling.exists());
    // An in-case output stays writable.
    let inside = case_dir.join("db/exported-marks.json");
    export_marks(&case_dir, Some(&inside)).unwrap();
    assert!(inside.is_file());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn batch_lock_recovers_inner_state_after_poison() {
    // A panicking worker must not crash the whole batch: the poisoned
    // mutex still holds consistent slot state, so recover it.
    let mutex = std::sync::Mutex::new(vec![7usize]);
    std::thread::scope(|scope| {
        let _ = scope
            .spawn(|| {
                let _guard = mutex.lock().unwrap();
                panic!("simulated worker panic");
            })
            .join();
    });
    assert!(mutex.lock().is_err(), "mutex must be poisoned");
    assert_eq!(*lock_or_recover(&mutex), vec![7]);
}

/// A case dir plus a two-item selection of real files, ready for
/// `validate_batch`.
fn validate_batch_fixture(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let base =
        std::env::temp_dir().join(format!("frametrace-vbatch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let case_dir = base.join("case");
    std::fs::create_dir_all(case_dir.join("db")).unwrap();
    std::fs::write(case_dir.join("case.json"), r#"{"case_id":"FT-vbatch"}"#).unwrap();
    let target_a = base.join("one.mp4");
    let target_b = base.join("two.mp4");
    std::fs::write(&target_a, b"\0\0\0\x18ftypmp42one").unwrap();
    std::fs::write(&target_b, b"\0\0\0\x18ftypmp42two").unwrap();
    let selection = base.join("selection.json");
    std::fs::write(
        &selection,
        format!(
            "{{\"items\":[{{\"selector\":\"{}\"}},{{\"selector\":\"{}\"}}]}}",
            crate::util::json_escape(&target_a.to_string_lossy()),
            crate::util::json_escape(&target_b.to_string_lossy()),
        ),
    )
    .unwrap();
    (case_dir, selection, target_a, target_b)
}

fn validate_batch_fingerprint(selection: &Path) -> String {
    checkpoint::fingerprint(&[
        "validate-batch",
        &audit::digest_file(selection).unwrap(),
        "ffprobe",
    ])
}

/// Items marked `done` in a checkpoint are replayed without
/// revalidating them — the resumed run appends log entries only for
/// the remaining items.
#[test]
fn validate_batch_changed_done_item_is_revalidated() {
    let (case_dir, selection, target_a, _) = validate_batch_fixture("changed-done");
    {
        let mut checkpoint = RunCheckpoint::begin(
            &case_dir.join("db/validate-batch-progress.jsonl"),
            "validate-batch",
            &validate_batch_fingerprint(&selection),
            ResumeMode::Auto,
        )
        .unwrap();
        checkpoint
            .append_line(
                &serde_json::json!({"done": {
                    "index": 0, "selector": target_a, "status": "ok", "detail": "old",
                    "target_path": crate::util::canonicalize_display(&target_a).unwrap(),
                    "target_sha256": audit::digest_file(&target_a).unwrap()
                }})
                .to_string(),
            )
            .unwrap();
    }
    std::fs::write(&target_a, b"changed media").unwrap();
    let _ = validate_batch(&case_dir, &selection, ResumeMode::Auto);
    let log = std::fs::read_to_string(case_dir.join("evidence/logs/validation-log.jsonl")).unwrap();
    assert_eq!(
        log.lines().count(),
        2,
        "changed done item must be validated: {log}"
    );
    assert!(log.contains(&audit::digest_file(&target_a).unwrap()));
    let _ = std::fs::remove_dir_all(case_dir.parent().unwrap());
}

#[test]
fn validate_batch_resume_replays_done_items() {
    let (case_dir, selection, target_a, _target_b) = validate_batch_fixture("done");
    let checkpoint_path = case_dir.join("db/validate-batch-progress.jsonl");
    {
        let mut checkpoint = RunCheckpoint::begin(
            &checkpoint_path,
            "validate-batch",
            &validate_batch_fingerprint(&selection),
            ResumeMode::Auto,
        )
        .unwrap();
        checkpoint
            .append_line(&format!(
                "{{\"done\":{{\"index\":0,\"selector\":\"{}\",\"status\":\"ok\",\"detail\":\"ffprobe-video-stream-confirmed (abc)\",\"target_path\":\"{}\",\"target_sha256\":\"{}\"}}}}",
                crate::util::json_escape(&target_a.to_string_lossy()),
                crate::util::json_escape(&crate::util::canonicalize_display(&target_a).unwrap().to_string_lossy()),
                audit::digest_file(&target_a).unwrap(),
            ))
            .unwrap();
    }

    validate_batch(&case_dir, &selection, ResumeMode::Auto).unwrap();

    // Only item 1's validation-log entry is new; item 0's was already
    // durable when the crashed run recorded it done.
    let log = std::fs::read_to_string(case_dir.join("evidence/logs/validation-log.jsonl")).unwrap();
    let entries: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    assert_eq!(entries.len(), 1, "{log}");
    assert!(entries[0].contains("two.mp4"), "{log}");
    let batch = std::fs::read_to_string(case_dir.join("artifacts/logs/batch-log.jsonl")).unwrap();
    assert!(
        batch.contains("\"event\":\"validate-batch-resume\""),
        "{batch}"
    );
    assert!(batch.contains("\"skipped_items\":1"), "{batch}");
    assert!(batch.contains("\"requested\":2"), "{batch}");
    assert!(
        !checkpoint_path.exists(),
        "checkpoint deleted on full success"
    );
    let _ = std::fs::remove_dir_all(case_dir.parent().unwrap());
}

/// A `computed` line (result durable, log append never landed) replays
/// the stored result and only retries the log append.
#[test]
fn validate_batch_resume_reuses_pending_computed_results() {
    let (case_dir, selection, target_a, target_b) = validate_batch_fixture("computed");
    let checkpoint_path = case_dir.join("db/validate-batch-progress.jsonl");
    let result = crate::validation::compute_validation(
        &case_dir,
        &target_a.to_string_lossy(),
        &ValidationOptions::default(),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    {
        let mut checkpoint = RunCheckpoint::begin(
            &checkpoint_path,
            "validate-batch",
            &validate_batch_fingerprint(&selection),
            ResumeMode::Auto,
        )
        .unwrap();
        checkpoint
            .append_line(&crate::validation::checkpoint_line(0, &result))
            .unwrap();
        // Item 1 fully completed before the crash.
        checkpoint
            .append_line(
                &serde_json::json!({"done": {
                    "index": 1, "selector": target_b, "status": "ok", "detail": "ok",
                    "target_path": crate::util::canonicalize_display(&target_b).unwrap(),
                    "target_sha256": audit::digest_file(&target_b).unwrap()
                }})
                .to_string(),
            )
            .unwrap();
    }

    validate_batch(&case_dir, &selection, ResumeMode::Auto).unwrap();

    let batch = std::fs::read_to_string(case_dir.join("artifacts/logs/batch-log.jsonl")).unwrap();
    assert!(
        batch.contains("\"event\":\"validate-batch-resume\""),
        "{batch}"
    );
    assert!(batch.contains("\"skipped_items\":1"), "{batch}");
    assert!(batch.contains("\"reused_computed\":1"), "{batch}");
    // Exactly one fresh log append — the replayed item 0 result.
    let log = std::fs::read_to_string(case_dir.join("evidence/logs/validation-log.jsonl")).unwrap();
    let entries: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    assert_eq!(entries.len(), 1, "{log}");
    assert!(entries[0].contains("one.mp4"), "{log}");
    let _ = std::fs::remove_dir_all(case_dir.parent().unwrap());
}

/// A checkpoint recorded against a different selection file must not
/// skip anything: the fingerprint mismatch resets the run.
#[test]
fn validate_batch_stale_fingerprint_validates_everything() {
    let (case_dir, selection, _target_a, _target_b) = validate_batch_fixture("stale");
    {
        let mut checkpoint = RunCheckpoint::begin(
            &case_dir.join("db/validate-batch-progress.jsonl"),
            "validate-batch",
            "bogus-fingerprint",
            ResumeMode::Auto,
        )
        .unwrap();
        checkpoint
            .append_line(
                "{\"done\":{\"index\":0,\"selector\":\"one.mp4\",\"status\":\"ok\",\"detail\":\"stale\"}}",
            )
            .unwrap();
    }

    // Both items are computed — the result may be a validation failure
    // for fake media, but the work must not be skipped. The all-failed
    // batch may legitimately return Err; the log evidence is what
    // matters.
    let _ = validate_batch(&case_dir, &selection, ResumeMode::Auto);
    let log = std::fs::read_to_string(case_dir.join("evidence/logs/validation-log.jsonl")).unwrap();
    let entries: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    assert_eq!(entries.len(), 2, "{log}");
    let batch = std::fs::read_to_string(case_dir.join("artifacts/logs/batch-log.jsonl")).unwrap();
    assert!(!batch.contains("validate-batch-resume"), "{batch}");
    let _ = std::fs::remove_dir_all(case_dir.parent().unwrap());
}

/// --deleted-videos auto-selection picks only deleted entries whose
/// path ends in a video extension, dedupes reallocated inodes, and
/// keeps the filesystem path as a note for the audit record.
#[test]
fn deleted_video_selectors_filters_dedupes_and_keeps_paths() {
    let base = std::env::temp_dir().join(format!("frametrace-delvid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let entries = base.join("tsk-files-1.jsonl");
    std::fs::write(
        &entries,
        concat!(
            "{\"raw_line\":\"r/r * 100-128-1:\\tdel.mp4\",\"file_type\":\"r/r\",\"inode\":\"100-128-1\",\"path\":\"del.mp4\",\"deleted\":true,\"video_candidate\":true}\n",
            "{\"raw_line\":\"r/r 101-128-1:\\tlive.mp4\",\"file_type\":\"r/r\",\"inode\":\"101-128-1\",\"path\":\"live.mp4\",\"deleted\":false,\"video_candidate\":true}\n",
            "{\"raw_line\":\"r/r * 102-128-1:\\tnotes.txt\",\"file_type\":\"r/r\",\"inode\":\"102-128-1\",\"path\":\"notes.txt\",\"deleted\":true,\"video_candidate\":false}\n",
            "{\"raw_line\":\"d/d * 103-128-1:\\tdir\",\"file_type\":\"d/d\",\"inode\":\"103-128-1\",\"path\":\"dir\",\"deleted\":true,\"video_candidate\":false}\n",
            "{\"raw_line\":\"r/r * 100-128-1:\\tdel.mp4\",\"file_type\":\"r/r\",\"inode\":\"100-128-1\",\"path\":\"del.mp4\",\"deleted\":true,\"video_candidate\":true}\n",
            "{\"raw_line\":\"r/r * 104-128-1:\\tdeep/show.avi\",\"file_type\":\"r/r\",\"inode\":\"104-128-1\",\"path\":\"deep/show.avi\",\"deleted\":true,\"video_candidate\":true}\n"
        ),
    )
    .unwrap();
    let items = deleted_video_selectors(&entries).unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0].selector, "100-128-1");
    assert_eq!(items[0].notes.as_deref(), Some("del.mp4"));
    assert_eq!(items[1].selector, "104-128-1");
    let _ = std::fs::remove_dir_all(&base);
}

/// --deleted-videos uses the newest tsk-files-*.jsonl; a case with no
/// inspection run fails with a clear "run inspect first" error.
#[test]
fn latest_entries_jsonl_picks_newest_and_handles_missing() {
    let base =
        std::env::temp_dir().join(format!("frametrace-latest-entries-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let case_dir = base.join("case");
    std::fs::create_dir_all(case_dir.join("db/filesystem")).unwrap();
    assert!(latest_entries_jsonl(&case_dir).is_none());
    let older = case_dir.join("db/filesystem/tsk-files-100.jsonl");
    let newer = case_dir.join("db/filesystem/tsk-files-200.jsonl");
    std::fs::write(&older, "").unwrap();
    std::fs::write(&newer, "").unwrap();
    // Unrelated files in the same directory must not be picked.
    std::fs::write(case_dir.join("db/filesystem/tsk-inspection-300.json"), "").unwrap();
    assert_eq!(latest_entries_jsonl(&case_dir).unwrap(), newer);
    let _ = std::fs::remove_dir_all(&base);
}

/// Integration: `package-case --allow-pending` reaches
/// `package_case_with_pending` — without the flag a case with queued marks
/// is refused (D9), with it the package is written.
#[test]
fn package_case_cli_allow_pending_flag() {
    let base = std::env::temp_dir().join(format!("ft-cli-allow-pending-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let case_dir = base.join("case");
    fs::create_dir_all(case_dir.join("db")).unwrap();
    fs::write(case_dir.join("case.json"), b"{}").unwrap();
    fs::write(case_dir.join("db/video_index.json"), b"{}").unwrap();
    fs::write(case_dir.join("db/videos.jsonl"), b"").unwrap();
    fs::write(case_dir.join("db/video_paths.tsv"), b"id\tsource_path\n").unwrap();
    let conn = crate::case_db::open_case_db(&case_dir).unwrap();
    crate::case_db::init_schema(&conn).unwrap();
    drop(conn);
    fs::create_dir_all(case_dir.join("marks-pending")).unwrap();
    fs::write(
        case_dir.join("marks-pending/00000000000000000000000000000001.json"),
        b"{\"marks\":[]}",
    )
    .unwrap();
    let args = |output: &Path, allow: bool| {
        let mut args = vec![
            "frametrace".to_string(),
            "package-case".to_string(),
            case_dir.to_string_lossy().to_string(),
            "--output".to_string(),
            output.to_string_lossy().to_string(),
        ];
        if allow {
            args.push("--allow-pending".to_string());
        }
        args
    };
    let refused = base.join("refused");
    let err = crate::cli::run(args(&refused, false)).unwrap_err();
    assert!(err.contains("--allow-pending"), "{err}");
    assert!(!refused.exists());
    let allowed = base.join("allowed");
    crate::cli::run(args(&allowed, true)).unwrap();
    assert!(allowed.join("manifest.sha256").is_file());
    let _ = fs::remove_dir_all(&base);
}
