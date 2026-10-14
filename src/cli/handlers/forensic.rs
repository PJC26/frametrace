use super::*;
use crate::audit;
use crate::carve::{self, CarveOptions};
use crate::case_db;
use crate::checkpoint::ResumeMode;
use crate::e01::{self, E01Options};
use crate::tsk::{self, TskInspectOptions, TskRecoverOptions};
use crate::util::json_escape;
use std::path::Path;

pub fn inspect_e01(
    case_dir: &Path,
    e01_file: &Path,
    options: E01Options,
    filesystem: bool,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let is_corrupted = e01::inspect_e01(case_dir, e01_file, &options)?;
    let row = case_db::register_evidence_source(
        case_dir,
        &case_db::EvidenceSourceInput {
            kind: "e01".to_string(),
            path: e01_file.to_path_buf(),
            source_id: None,
            write_protect: None,
            acquisition_tool: Some("libewf ewfinfo".to_string()),
            evidence_hash: None,
            notes: Some("Auto-registered by inspect-e01".to_string()),
            metadata_json: None,
        },
    )?;
    println!("E01 inspected");
    if is_corrupted {
        println!(
            "warning: ewfinfo flags this segment set as corrupted/incomplete; see the info log"
        );
    }
    println!("source registered: {} ({})", row.source_id, row.kind);
    println!("source: {}", e01_file.display());
    println!(
        "audit log: {}",
        case_dir.join("evidence/logs/e01-audit.jsonl").display()
    );
    if filesystem {
        // TSK reads the E01 through libewf, so filesystem triage needs no
        // raw export; decompression makes any probe-class timeout
        // meaningless, so run unbounded.
        println!("filesystem triage: mmls/fls read the E01 via libewf (no export)");
        let tsk_options = crate::tsk::TskInspectOptions {
            timeout_secs: None,
            ..Default::default()
        };
        // C7 / B16: a partial listing (direct triage) exits 5 — completed,
        // but the server must report it as partial, never as a clean pass.
        if let Some(reason) = inspect_image_run(case_dir, e01_file, tsk_options)? {
            exit_partial(&reason);
        }
    }
    Ok(())
}

/// C7 / B16 contract: the step completed and recorded everything, but its
/// result is PARTIAL. The last stderr line starts with
/// [`crate::tsk::PARTIAL_LINE_PREFIX`].
fn exit_partial(reason: &str) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    eprintln!("{}{reason}", crate::tsk::PARTIAL_LINE_PREFIX);
    let _ = std::io::stderr().flush();
    std::process::exit(crate::tsk::EXIT_PARTIAL);
}

pub fn import_e01(case_dir: &Path, e01_file: &Path, options: E01Options) -> Result<(), String> {
    ensure_case(case_dir)?;
    let source = case_db::register_evidence_source(
        case_dir,
        &case_db::EvidenceSourceInput {
            kind: "e01".to_string(),
            path: e01_file.to_path_buf(),
            source_id: None,
            write_protect: None,
            acquisition_tool: Some("libewf".to_string()),
            evidence_hash: None,
            notes: Some("Auto-registered by import-e01".to_string()),
            metadata_json: Some(e01_options_json(&options)),
        },
    )?;
    let job = case_db::start_job(
        case_dir,
        "import-e01",
        e01_file,
        None,
        &e01_options_json(&options),
    )?;
    let result = match e01::import_e01(case_dir, e01_file, &options) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    // FrameTrace digested the exported raw image itself, so its hash is
    // recorded as computed, not operator-supplied.
    let raw_source = case_db::register_evidence_source_with_origin(
        case_dir,
        &case_db::EvidenceSourceInput {
            kind: "raw-image".to_string(),
            path: result.raw_output_path.clone(),
            source_id: None,
            write_protect: Some("derived read-only image; preserve original E01".to_string()),
            acquisition_tool: Some("libewf ewfexport".to_string()),
            evidence_hash: Some(result.raw_sha256.clone()),
            notes: Some(format!("Exported from source {}", source.source_id)),
            metadata_json: None,
        },
        case_db::HashOrigin::Computed,
    )
    .inspect_err(|err| {
        let _ = case_db::fail_job(case_dir, &job.job_id, err);
    })?;
    super::case::print_source_changes(case_dir, &raw_source);
    case_db::complete_job(case_dir, &job.job_id, 1, "import-e01 completed")?;
    println!("E01 imported");
    println!("source registered: {} ({})", source.source_id, source.kind);
    println!("job: {} ({})", job.job_id, job.job_type);
    println!("source: {}", result.e01_path.display());
    println!("raw output: {}", result.raw_output_path.display());
    println!("raw sha256: {}", result.raw_sha256);
    if let Some(e01_sha256) = result.e01_sha256 {
        println!("E01 sha256: {e01_sha256}");
    }
    println!("info log: {}", result.ewfinfo_log_path.display());
    if let Some(path) = result.ewfverify_log_path {
        println!("verify log: {}", path.display());
    }
    println!("export log: {}", result.ewfexport_log_path.display());
    println!(
        "next: carve-file {} {}",
        case_dir.display(),
        result.raw_output_path.display()
    );
    Ok(())
}

pub fn carve_file(
    case_dir: &Path,
    source_file: &Path,
    options: CarveOptions,
    resume: ResumeMode,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    // C1: an imported raw image is carved only when its completion marker
    // proves the export finished (a killed import's torn image is refused).
    e01::require_complete_case_raw(case_dir, source_file)?;
    let source = case_db::register_evidence_source(
        case_dir,
        &case_db::EvidenceSourceInput {
            kind: "raw-image".to_string(),
            path: source_file.to_path_buf(),
            source_id: None,
            write_protect: None,
            acquisition_tool: None,
            evidence_hash: None,
            notes: Some("Auto-registered by carve-file".to_string()),
            metadata_json: Some(carve_options_json(&options)),
        },
    )?;
    let job = case_db::start_job(
        case_dir,
        "carve-file",
        source_file,
        Some(options.max_candidates as u64),
        &carve_options_json(&options),
    )?;
    let progress_job_id = job.job_id.clone();
    let progress = move |done: u64, total: u64| {
        let _ = case_db::report_job_progress(case_dir, &progress_job_id, Some(total), done);
    };
    let result = match carve::carve_file(case_dir, source_file, &options, resume, Some(&progress)) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(
        case_dir,
        &job.job_id,
        result.artifacts.len() as u64,
        "carve-file completed",
    )?;
    println!("carve complete");
    println!("source registered: {} ({})", source.source_id, source.kind);
    println!("job: {} ({})", job.job_id, job.job_type);
    println!("source: {}", result.source_path.display());
    println!("artifacts carved: {}", result.artifacts.len());
    if result.resumed_artifacts > 0 || result.resumed_scan_offset > 0 {
        println!(
            "resumed from checkpoint: {} artifact(s) reused, scan offset {}",
            result.resumed_artifacts, result.resumed_scan_offset
        );
    }
    println!(
        "results: {}",
        case_dir.join("db/carve_results.json").display()
    );
    Ok(())
}

pub fn inspect_image(
    case_dir: &Path,
    image_file: &Path,
    options: TskInspectOptions,
) -> Result<(), String> {
    // C7 / B16: exit code 5 when the listing is partial.
    if let Some(reason) = inspect_image_run(case_dir, image_file, options)? {
        exit_partial(&reason);
    }
    Ok(())
}

/// Runs the inspection; `Ok(Some(reason))` when the result is partial
/// (fls timed out, or the listing was truncated to `--max-entries`).
fn inspect_image_run(
    case_dir: &Path,
    image_file: &Path,
    options: TskInspectOptions,
) -> Result<Option<String>, String> {
    ensure_case(case_dir)?;
    let source = case_db::register_evidence_source(
        case_dir,
        &case_db::EvidenceSourceInput {
            kind: "forensic-image".to_string(),
            path: image_file.to_path_buf(),
            source_id: None,
            write_protect: Some(
                "treat image as read-only; recover into derived artifacts".to_string(),
            ),
            acquisition_tool: Some("Sleuth Kit mmls/fls".to_string()),
            evidence_hash: None,
            notes: Some("Auto-registered by inspect-image".to_string()),
            metadata_json: Some(tsk_inspect_options_json(&options)),
        },
    )?;
    let job = case_db::start_job(
        case_dir,
        "inspect-image",
        image_file,
        Some(options.max_entries as u64),
        &tsk_inspect_options_json(&options),
    )?;
    let progress_job_id = job.job_id.clone();
    let fls_progress = move |entries_seen: u64| {
        // fls reports no total up front — only the running entry count is
        // honest progress.
        let _ = case_db::report_job_progress(case_dir, &progress_job_id, None, entries_seen);
    };
    let result = match tsk::inspect_image(case_dir, image_file, &options, Some(&fls_progress)) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(
        case_dir,
        &job.job_id,
        result.entries.len() as u64,
        if result.fls_completed {
            "inspect-image completed"
        } else {
            "inspect-image completed with a PARTIAL fls listing (timeout)"
        },
    )?;
    println!("filesystem image inspected");
    println!("source registered: {} ({})", source.source_id, source.kind);
    println!("job: {} ({})", job.job_id, job.job_type);
    println!("image: {}", result.image_path.display());
    println!("partition offset: {}", result.partition_offset);
    println!("partitions: {}", result.partitions.len());
    println!("entries: {}", result.entries.len());
    println!(
        "deleted entries: {}",
        result.entries.iter().filter(|entry| entry.deleted).count()
    );
    println!(
        "video candidates: {}",
        result
            .entries
            .iter()
            .filter(|entry| entry.video_candidate)
            .count()
    );
    println!("summary: {}", result.summary_path.display());
    println!("entries jsonl: {}", result.entries_jsonl_path.display());
    println!("mmls log: {}", result.mmls_log_path.display());
    println!("fls log: {}", result.fls_log_path.display());
    if !result.warnings.is_empty() {
        println!("warnings: {}", result.warnings.len());
        for warning in &result.warnings {
            println!("warning: {warning}");
        }
    }
    Ok(crate::tsk::partial_reason(&result))
}

pub fn recover_inode(
    case_dir: &Path,
    image_file: &Path,
    options: TskRecoverOptions,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let source = case_db::register_evidence_source(
        case_dir,
        &case_db::EvidenceSourceInput {
            kind: "forensic-image".to_string(),
            path: image_file.to_path_buf(),
            source_id: None,
            write_protect: Some("source image read-only; inode output is derived".to_string()),
            acquisition_tool: Some("Sleuth Kit icat".to_string()),
            evidence_hash: None,
            notes: Some("Auto-registered by recover-inode".to_string()),
            metadata_json: Some(tsk_recover_options_json(&options)),
        },
    )?;
    let job = case_db::start_job(
        case_dir,
        "recover-inode",
        image_file,
        Some(1),
        &tsk_recover_options_json(&options),
    )?;
    let result = match tsk::recover_inode(case_dir, image_file, &options) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(case_dir, &job.job_id, 1, "recover-inode completed")?;
    println!("inode recovered");
    println!("source registered: {} ({})", source.source_id, source.kind);
    println!("job: {} ({})", job.job_id, job.job_type);
    println!("image: {}", result.image_path.display());
    println!("partition offset: {}", result.partition_offset);
    println!("inode: {}", result.inode);
    println!("output: {}", result.output_path.display());
    println!("size bytes: {}", result.size_bytes);
    println!("sha256: {}", result.sha256);
    println!("validation: {}", result.validation_status);
    for warning in &result.warnings {
        println!("warning: {warning}");
    }
    println!(
        "next: scan-folder {} {} --no-ffprobe",
        case_dir.display(),
        result
            .output_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .display()
    );
    Ok(())
}

pub fn recover_batch(
    case_dir: &Path,
    image_file: &Path,
    selection_path: Option<&Path>,
    partition_offset: u64,
    deleted_videos: bool,
    timeout_secs: Option<u64>,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let (selection, selection_label) = if deleted_videos {
        let entries_jsonl = latest_entries_jsonl(case_dir).ok_or_else(|| {
            "no filesystem inspection entries found; run inspect-image or inspect-e01 --filesystem first"
                .to_string()
        })?;
        let items = deleted_video_selectors(&entries_jsonl)?;
        if items.is_empty() {
            return Err(format!(
                "latest inspection {} lists no deleted video candidates",
                entries_jsonl.display()
            ));
        }
        println!("auto-selection: {}", entries_jsonl.display());
        (
            crate::selection::SelectionFile {
                case_id: None,
                items,
            },
            entries_jsonl.display().to_string(),
        )
    } else {
        let selection_path = selection_path.ok_or_else(|| {
            "recover-batch needs a selection file or --deleted-videos".to_string()
        })?;
        let parsed = crate::selection::parse_selection_file(selection_path)?;
        crate::selection::enforce_case_binding(
            case_dir,
            parsed.case_id.as_deref(),
            selection_path,
            crate::selection::ImportKind::Selection,
        )?;
        (parsed, selection_path.display().to_string())
    };
    let job = case_db::start_job(
        case_dir,
        "recover-batch",
        image_file,
        Some(selection.items.len() as u64),
        &format!(
            "{{\"items\":{},\"deleted_videos\":{deleted_videos}}}",
            selection.items.len()
        ),
    )?;

    let mut outcomes = Vec::new();
    for item in &selection.items {
        let inode = item
            .selector
            .trim()
            .strip_prefix("fls:")
            .unwrap_or(item.selector.trim())
            .to_string();
        let options = TskRecoverOptions {
            partition_offset,
            inode,
            output_path: None,
            recover_deleted: true,
            include_slack: false,
            skip_sparse_holes: true,
            icat_bin: "icat".to_string(),
            timeout_secs,
        };
        let outcome = recover_inode(case_dir, image_file, options);
        outcomes.push(match outcome {
            Ok(()) => BatchOutcome {
                selector: item.selector.clone(),
                action: "recover",
                status: "ok",
                detail: "recovered into artifacts/recovered/filesystem".to_string(),
            },
            Err(error) => BatchOutcome {
                selector: item.selector.clone(),
                action: "recover",
                status: "failed",
                detail: error,
            },
        });
    }

    let ok = outcomes.iter().filter(|o| o.status == "ok").count();
    let failed = outcomes.iter().filter(|o| o.status == "failed").count();
    let line = format!(
        "{{\"schema_version\":1,\"event\":\"recover-batch\",\"selection_path\":\"{}\",\"requested\":{},\"ok\":{},\"failed\":{},\"results\":{}}}",
        json_escape(&selection_label),
        outcomes.len(),
        ok,
        failed,
        outcomes_json(&outcomes),
    );
    audit::append_chained_jsonl(&case_dir.join("artifacts/logs/batch-log.jsonl"), &line)?;
    // All-items-failed batches must fail the job row, not record success.
    if ok == 0 && failed > 0 && failed == outcomes.len() {
        let _ = case_db::fail_job(
            case_dir,
            &job.job_id,
            &format!("all {failed} item(s) failed"),
        );
        return Err(format!(
            "recover batch failed: {failed} of {} item(s) failed",
            outcomes.len()
        ));
    }
    case_db::complete_job(
        case_dir,
        &job.job_id,
        outcomes.len() as u64,
        "recover-batch completed",
    )?;

    println!("recover batch complete");
    println!("requested: {}", outcomes.len());
    println!("ok: {ok}");
    println!("failed: {failed}");
    for outcome in &outcomes {
        println!(
            "  [{}] {}: {}",
            outcome.status, outcome.selector, outcome.detail
        );
    }
    Ok(())
}
