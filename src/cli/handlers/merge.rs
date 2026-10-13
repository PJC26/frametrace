use super::*;
use crate::case_db;
use std::path::{Path, PathBuf};

pub fn merge_cases(case_dir: &Path, source_case_dirs: &[PathBuf]) -> Result<(), String> {
    ensure_case(case_dir)?;
    for source in source_case_dirs {
        ensure_case(source)?;
    }
    crate::case_merge::load_target_lines(case_dir)?;
    let job = case_db::start_job(
        case_dir,
        "merge-cases",
        case_dir,
        Some(source_case_dirs.len() as u64),
        "{}",
    )?;
    let result = match crate::case_merge::merge_cases(case_dir, source_case_dirs) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(
        case_dir,
        &job.job_id,
        result.merged_records as u64,
        "merge-cases completed",
    )?;
    println!("cases merged");
    println!("job: {} ({})", job.job_id, job.job_type);
    for source in &result.sources {
        let notes = &source.annotations;
        println!(
            "  {}: {} merged, {} duplicate(s); annotations: {} mark(s) merged, {} tag set(s) merged, {} note(s) appended, {} mark conflict(s) kept target, {} history row(s) copied, {} unmapped",
            source.case_dir.display(),
            source.merged_records,
            source.duplicate_records,
            notes.marks_merged,
            notes.tags_merged,
            notes.notes_appended,
            notes.marks_conflicted,
            notes.history_rows_copied,
            notes.annotations_unmapped,
        );
    }
    let totals = result.annotation_totals();
    println!("merged records: {}", result.merged_records);
    println!("duplicates marked: {}", result.duplicate_records);
    println!("total index records: {}", result.total_records);
    println!("marks merged: {}", totals.marks_merged);
    println!("tags merged: {}", totals.tags_merged);
    println!("notes appended: {}", totals.notes_appended);
    println!(
        "marks conflicted (target kept, source mark preserved in history): {}",
        totals.marks_conflicted
    );
    println!("history rows copied: {}", totals.history_rows_copied);
    if totals.annotations_unmapped > 0 {
        println!(
            "warning: {} source record id(s) have annotations but no record in their source index; they were not merged (source case left unchanged)",
            totals.annotations_unmapped
        );
    }
    println!(
        "label: {} (merged rows are copied index claims, not re-verified evidence)",
        crate::case_merge::LABEL
    );
    Ok(())
}

/// Diffs this case's video index against another case's and writes a
/// candidate-grade JSON report inside THIS case (reports/case-compare.json by
/// default). Audit-chained under evidence/logs/case-compare-log.jsonl.
pub fn compare_cases(
    case_dir: &Path,
    other_case_dir: &Path,
    output: Option<PathBuf>,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    ensure_case(other_case_dir)?;
    let output_path = output.unwrap_or_else(|| case_dir.join("reports/case-compare.json"));
    crate::tool_policy::require_case_report_path(
        case_dir,
        &output_path,
        "reports/case-compare.json",
        "case comparison",
    )?;
    let job = case_db::start_job(case_dir, "compare-cases", other_case_dir, None, "{}")?;
    let result = match crate::case_compare::compare_cases(case_dir, other_case_dir, &output_path) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(
        case_dir,
        &job.job_id,
        (result.both_count
            + result.only_in_a_count
            + result.only_in_b_count
            + result.hash_mismatch_count) as u64,
        "compare-cases completed",
    )?;
    println!("case comparison written: {}", result.report_path.display());
    println!("present in both: {}", result.both_count);
    println!("only in this case: {}", result.only_in_a_count);
    println!("only in other case: {}", result.only_in_b_count);
    println!(
        "hash mismatches on same path: {}",
        result.hash_mismatch_count
    );
    println!(
        "label: {} (index rows are recorded claims, not re-verified content)",
        crate::case_compare::LABEL
    );
    Ok(())
}

/// Splits the case index into known/unknown against a user-supplied sha256
/// list (reports/known-hash-filter.json by default), audit-chained under
/// evidence/logs/known-hash-log.jsonl. The hash list is an input and may
/// live outside the case; the report stays confined inside it.
pub fn known_hash_filter(
    case_dir: &Path,
    hash_list: &Path,
    output: Option<PathBuf>,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let output_path = output.unwrap_or_else(|| case_dir.join("reports/known-hash-filter.json"));
    crate::tool_policy::require_case_report_path(
        case_dir,
        &output_path,
        "reports/known-hash-filter.json",
        "known-hash filter",
    )?;
    let job = case_db::start_job(case_dir, "known-hash-filter", hash_list, None, "{}")?;
    let result = match crate::known_hash::filter_known_hashes(case_dir, hash_list, &output_path) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(
        case_dir,
        &job.job_id,
        (result.known_count + result.unknown_count + result.unhashed_count) as u64,
        "known-hash-filter completed",
    )?;
    println!(
        "known-hash report written: {}",
        result.report_path.display()
    );
    println!("list digests: {}", result.list_size);
    println!("known: {}", result.known_count);
    println!("unknown: {}", result.unknown_count);
    println!("unhashed: {}", result.unhashed_count);
    println!(
        "label: {} (matches reflect recorded index hashes, not re-verified content)",
        crate::known_hash::LABEL
    );
    Ok(())
}

/// Exports the case video index as DFXML (reports/case-index.dfxml by
/// default), confined to the case directory and audit-chained under
/// evidence/logs/dfxml-export-log.jsonl. Values are recorded index claims —
/// the export carries the `candidate-export` label.
pub fn export_dfxml(case_dir: &Path, output: Option<PathBuf>) -> Result<(), String> {
    ensure_case(case_dir)?;
    let output_path = output.unwrap_or_else(|| case_dir.join("reports/case-index.dfxml"));
    crate::tool_policy::require_case_report_path(
        case_dir,
        &output_path,
        "reports/case-index.dfxml",
        "DFXML export",
    )?;
    let job = case_db::start_job(case_dir, "export-dfxml", &output_path, None, "{}")?;
    let result = match crate::dfxml::export_dfxml(case_dir, &output_path) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(
        case_dir,
        &job.job_id,
        result.object_count as u64,
        "export-dfxml completed",
    )?;
    println!("dfxml written: {}", result.output_path.display());
    println!("fileobjects: {}", result.object_count);
    println!("with sha256: {}", result.hashed_count);
    println!(
        "label: {} (index values are recorded claims, not re-verified measurements)",
        crate::dfxml::LABEL
    );
    Ok(())
}

/// Emits the merged candidate timeline (db/timeline.jsonl by default). The
/// stream is audit-logged under evidence/logs/timeline-log.jsonl and tracked
/// as a job like the other case-mutating commands.
pub fn timeline(case_dir: &Path, output: Option<PathBuf>) -> Result<(), String> {
    ensure_case(case_dir)?;
    let output_path = output.unwrap_or_else(|| case_dir.join("db/timeline.jsonl"));
    crate::tool_policy::require_case_report_path(
        case_dir,
        &output_path,
        "db/timeline.jsonl",
        "timeline",
    )?;
    let job = case_db::start_job(case_dir, "timeline", case_dir, None, "{}")?;
    let result = match crate::timeline::generate_timeline(case_dir, &output_path) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(
        case_dir,
        &job.job_id,
        result.event_count as u64,
        "timeline completed",
    )?;
    println!("timeline written: {}", result.output_path.display());
    println!("events: {}", result.event_count);
    println!(
        "skipped without timestamps: {}",
        result.skipped_no_timestamp
    );
    println!(
        "label: {} (timestamps are recorded metadata, not validated truth)",
        crate::timeline::LABEL
    );
    Ok(())
}
