use crate::carve::CarveOptions;
use crate::e01::E01Options;
use crate::model::ScanOptions;
use crate::tsk::{TskInspectOptions, TskRecoverOptions};
use crate::util::read_to_string;
use crate::validation::ValidationOptions;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn lock_or_recover<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn ensure_case(case_dir: &Path) -> Result<(), String> {
    if !case_dir.join("case.json").is_file() {
        return Err(format!(
            "not a case directory: {} (run init-case first)",
            case_dir.display()
        ));
    }
    Ok(())
}

fn extract_json_number(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start();
    let end = rest
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some(rest[..end].to_string())
    }
}

fn e01_options_json(options: &E01Options) -> String {
    format!(
        "{{\"output_path\":{},\"max_bytes\":{},\"skip_verify\":{},\"hash_e01\":{}}}",
        options
            .output_path
            .as_ref()
            .map(|path| format!("\"{}\"", crate::util::json_escape(&path.to_string_lossy())))
            .unwrap_or_else(|| "null".to_string()),
        options
            .max_bytes
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        options.skip_verify,
        options.hash_e01
    )
}

fn carve_options_json(options: &CarveOptions) -> String {
    format!(
        "{{\"max_bytes\":{},\"max_candidates\":{},\"reassemble\":{},\"scan_offset\":{},\"scan_length\":{}}}",
        options.max_bytes,
        options.max_candidates,
        options.reassemble,
        options
            .scan_offset
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        options
            .scan_length
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string())
    )
}

fn tsk_inspect_options_json(options: &TskInspectOptions) -> String {
    format!(
        "{{\"partition_offset\":{},\"max_entries\":{},\"mmls_bin\":\"{}\",\"fls_bin\":\"{}\",\"timeout_secs\":{}}}",
        options
            .partition_offset
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        options.max_entries,
        crate::util::json_escape(&options.mmls_bin),
        crate::util::json_escape(&options.fls_bin),
        options
            .timeout_secs
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string())
    )
}

fn tsk_recover_options_json(options: &TskRecoverOptions) -> String {
    format!(
        "{{\"partition_offset\":{},\"inode\":\"{}\",\"output_path\":{},\"recover_deleted\":{},\"include_slack\":{},\"skip_sparse_holes\":{},\"icat_bin\":\"{}\"}}",
        options.partition_offset,
        crate::util::json_escape(&options.inode),
        options
            .output_path
            .as_ref()
            .map(|path| format!("\"{}\"", crate::util::json_escape(&path.to_string_lossy())))
            .unwrap_or_else(|| "null".to_string()),
        options.recover_deleted,
        options.include_slack,
        options.skip_sparse_holes,
        crate::util::json_escape(&options.icat_bin)
    )
}

fn validation_options_json(options: &ValidationOptions) -> String {
    format!(
        "{{\"ffprobe_bin\":\"{}\"}}",
        crate::util::json_escape(&options.ffprobe_bin)
    )
}

fn scan_options_json(options: &ScanOptions) -> String {
    format!(
        "{{\"hash_files\":{},\"use_ffprobe\":{},\"max_depth\":{},\"incremental\":{},\"deepfake_screen\":{}}}",
        options.hash_files,
        options.use_ffprobe,
        options
            .max_depth
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        options.incremental,
        options.deepfake_screen
    )
}

fn default_operator() -> Option<String> {
    env::var("USERNAME")
        .ok()
        .or_else(|| env::var("USER").ok())
        .filter(|value| !value.trim().is_empty())
}

fn default_host() -> Option<String> {
    env::var("COMPUTERNAME")
        .ok()
        .or_else(|| env::var("HOSTNAME").ok())
        .filter(|value| !value.trim().is_empty())
}

mod batch;
mod case;
mod doctor;
pub(crate) mod forensic;
mod join_audio;
mod media;
mod merge;
mod tools;

#[derive(Debug, Clone)]
struct BatchOutcome {
    selector: String,
    action: &'static str,
    status: &'static str,
    detail: String,
}
fn outcomes_json(outcomes: &[BatchOutcome]) -> String {
    let items = outcomes
        .iter()
        .map(|outcome| {
            format!(
                "{{\"selector\":\"{}\",\"action\":\"{}\",\"status\":\"{}\",\"detail\":\"{}\"}}",
                crate::util::json_escape(&outcome.selector),
                crate::util::json_escape(outcome.action),
                crate::util::json_escape(outcome.status),
                crate::util::json_escape(&outcome.detail),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{items}]")
}
/// Recovers-inode outputs are named inode_*.bin, but the Sleuth Kit listing
/// keeps each inode's original path (with recorder timestamps and channels).
/// Pass the newest listing to the viewer so recovered rows can show original
/// names and recording times.
fn latest_fls_entries_jsonl(case_dir: &Path) -> String {
    let entries_dir = case_dir.join("db/filesystem");
    let Ok(entries) = std::fs::read_dir(&entries_dir) else {
        return String::new();
    };
    let best = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tsk-files-") && name.ends_with(".jsonl"))
        })
        .max();
    let Some(path) = best else {
        return String::new();
    };
    read_to_string(&path).unwrap_or_default()
}
/// Batch-recovers viewer-selected deleted inodes (kind "candidate") from a
/// raw image. Each recovery appends its own tsk-audit entry; the batch outcome
/// is chained into artifacts/logs/batch-log.jsonl.
/// Newest `db/filesystem/tsk-files-*.jsonl` inspection output, or None
/// when no inspect-image/inspect-e01 --filesystem run exists yet. The
/// unix-timestamp suffix orders runs chronologically.
fn latest_entries_jsonl(case_dir: &Path) -> Option<PathBuf> {
    fs::read_dir(case_dir.join("db/filesystem"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tsk-files-") && name.ends_with(".jsonl"))
        })
        .max()
}
/// Deleted video-candidate selectors from an inspect-image entries file —
/// the auto-target list for `recover-batch --deleted-videos`. The
/// entries' inode becomes the selector; the filesystem path is kept as a
/// note so the audit record shows what each inode was.
fn deleted_video_selectors(
    entries_jsonl: &Path,
) -> Result<Vec<crate::selection::SelectionItem>, String> {
    let text = fs::read_to_string(entries_jsonl)
        .map_err(|err| format!("failed to read {}: {err}", entries_jsonl.display()))?;
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (line_no, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|err| {
            format!(
                "invalid entries line {}:{}: {err}",
                entries_jsonl.display(),
                line_no + 1
            )
        })?;
        let deleted = value.get("deleted").and_then(|v| v.as_bool()) == Some(true);
        let video = value.get("video_candidate").and_then(|v| v.as_bool()) == Some(true);
        let Some(inode) = value.get("inode").and_then(|v| v.as_str()) else {
            continue;
        };
        if deleted && video && !inode.is_empty() && seen.insert(inode.to_string()) {
            items.push(crate::selection::SelectionItem {
                selector: inode.to_string(),
                kind: Some("filesystem".to_string()),
                action: Some("recover".to_string()),
                format: None,
                time_seconds: None,
                notes: value
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            });
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests;

pub(crate) use batch::{
    export_batch, export_marks, import_marks, import_marks_quiet, validate_batch,
};
pub(crate) use case::{
    InitCaseOptions, PackageOptions, RegisterSourceOptions, init_case, inspect, make_custody,
    make_report, make_review, package_case_with_pending, register_source,
    review_marks_json_from_rows, scan_folder, signoff_content_json_from, verify_package_with,
};
#[cfg(test)]
pub(crate) use case::{report_audit_chains_json, signoff_content_json};
pub(crate) use doctor::{
    BenchmarkOptions, benchmark_db, deepfake_scan, deepfake_screen, doctor, repair_lane,
    rotate_audit_key, verify_audit,
};
pub(crate) use forensic::{
    carve_file, import_e01, inspect_e01, inspect_image, recover_batch, recover_inode,
};
pub(crate) use join_audio::{JoinVideoArgs, join_video, measure_audio, split_ids};
pub(crate) use media::{
    export_dav, export_hik, export_video, extract_telemetry, make_proxy, make_thumbnail,
    transcode_queue, validate_artifact,
};
pub(crate) use merge::{compare_cases, export_dfxml, known_hash_filter, merge_cases, timeline};
pub(crate) use tools::{tools_install, tools_list};
