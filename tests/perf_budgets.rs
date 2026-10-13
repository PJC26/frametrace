//! Opt-in performance harness over synthetic workloads. `std::time` only —
//! no extra dependencies — and every test is `#[ignore]`d so the default
//! `cargo test` stays fast.
//!
//! Run all three:
//!
//! ```text
//! cargo test --locked --test perf_budgets -- --ignored --nocapture
//! ```
//!
//! Budgets are documented in `docs/PERFORMANCE_VALIDATION.md` ("In-tree
//! benchmark harness"). The assertions below are generous sanity ceilings
//! for a debug build — an order-of-magnitude regression fails, ordinary
//! machine noise does not. They complement (not replace) the field budgets
//! in `docs/archive/ROADMAP-v2.md` §3, which target release builds on real media.

use std::fs;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use frametrace::audit;
use frametrace::case_db;
use frametrace::checkpoint::ResumeMode;
use frametrace::model::ScanOptions;
use frametrace::scan;

const INDEX_ROWS: usize = 10_000;
const AUDIT_ENTRIES: usize = 1_000;
const SCAN_FILES: usize = 1_000;

fn unique_dir(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "frametrace-perf-{name}-{}-{stamp}",
        std::process::id()
    ))
}

fn report(metric: &str, count: usize, elapsed_ms: u128, ceiling_ms: u128) {
    let per_k = if count == 0 {
        0
    } else {
        elapsed_ms * 1_000 / count as u128
    };
    eprintln!(
        "[perf] {metric}: {count} items in {elapsed_ms} ms ({per_k} ms/1k, sanity ceiling {ceiling_ms} ms)"
    );
    assert!(
        elapsed_ms <= ceiling_ms,
        "{metric} exceeded sanity ceiling: {elapsed_ms} ms > {ceiling_ms} ms"
    );
}

/// SQLite index write (single transaction, production `videos` schema) plus
/// the read paths used by scans (`load_video_ids`) and `inspect`
/// (`summarize_case_db`). Field reference: 10k rows ≈ 591 ms release on
/// Apple Silicon (`docs/PERFORMANCE_VALIDATION.md`, `docs/archive/ROADMAP-v2.md` §1).
#[test]
#[ignore = "perf harness; run with --ignored --nocapture"]
fn index_write_read_10k_rows() {
    let dir = unique_dir("index");
    let result = case_db::benchmark_case_db(&dir, INDEX_ROWS).expect("benchmark write");
    report("index write", result.rows, result.elapsed_ms, 30_000);

    let started = Instant::now();
    let ids = case_db::load_video_ids(&dir).expect("load ids");
    assert_eq!(ids.len(), INDEX_ROWS);
    report(
        "index read (load_video_ids)",
        INDEX_ROWS,
        started.elapsed().as_millis(),
        10_000,
    );

    let started = Instant::now();
    let summary = case_db::summarize_case_db(&dir)
        .expect("summarize")
        .expect("db exists");
    assert_eq!(summary.video_count, INDEX_ROWS as u64);
    report(
        "index read (summarize)",
        1,
        started.elapsed().as_millis(),
        10_000,
    );

    let _ = fs::remove_dir_all(dir);
}

/// Audit-log append is the hot path of every artifact-producing command:
/// lock + full read + chain + single append + fsync per entry, so it is
/// deliberately O(total log bytes) — this measures that contract end to end
/// plus a full chain verification.
#[test]
#[ignore = "perf harness; run with --ignored --nocapture"]
fn audit_append_verify_1k_entries() {
    let dir = unique_dir("audit");
    fs::create_dir_all(&dir).unwrap();
    let log = dir.join("audit.jsonl");

    let started = Instant::now();
    for index in 0..AUDIT_ENTRIES {
        audit::append_chained_jsonl(&log, &format!("{{\"kind\":\"bench\",\"i\":{index}}}"))
            .expect("audit append");
    }
    report(
        "audit chained append",
        AUDIT_ENTRIES,
        started.elapsed().as_millis(),
        60_000,
    );

    let started = Instant::now();
    let verification = audit::verify_chained_jsonl(&log).expect("verify");
    assert_eq!(verification.entries, AUDIT_ENTRIES);
    report(
        "audit chain verify",
        AUDIT_ENTRIES,
        started.elapsed().as_millis(),
        10_000,
    );

    let _ = fs::remove_dir_all(dir);
}

/// Scan/index path over a synthetic tree: directory walk, extension
/// classification, and all index outputs (SQLite, JSON index, JSONL, TSV).
/// `--no-ffprobe` and no hashing, matching the terabyte-aware defaults in
/// `docs/PERFORMANCE_VALIDATION.md` "Field Performance Rules".
#[test]
#[ignore = "perf harness; run with --ignored --nocapture"]
fn scan_synthetic_tree_1k_files() {
    let root = unique_dir("scan");
    let source = root.join("source");
    let case_dir = root.join("case");
    for group in 0..10usize {
        let dir = source.join(format!("camera_{group:02}"));
        fs::create_dir_all(&dir).unwrap();
        for index in 0..(SCAN_FILES / 10) {
            fs::write(
                dir.join(format!("clip_{index:05}.mp4")),
                b"frametrace synthetic perf fixture",
            )
            .unwrap();
        }
    }
    fs::create_dir_all(&case_dir).unwrap();

    let options = ScanOptions {
        hash_files: false,
        use_ffprobe: false,
        max_depth: None,
        incremental: false,
        deepfake_screen: false,
    };
    let started = Instant::now();
    let result =
        scan::scan_folder(&case_dir, &source, &options, ResumeMode::Auto, None).expect("scan");
    let elapsed = started.elapsed().as_millis();
    assert_eq!(result.video_count, SCAN_FILES);
    report("scan-folder + index outputs", SCAN_FILES, elapsed, 30_000);
    assert!(case_dir.join("db/videos.jsonl").is_file());
    assert!(case_db::case_db_path(&case_dir).is_file());

    let _ = fs::remove_dir_all(root);
}
