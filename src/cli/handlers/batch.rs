use super::*;
use crate::artifacts::{self};
use crate::audit;
use crate::case_db;
use crate::checkpoint::{self, ResumeMode, RunCheckpoint};
use crate::util::{json_escape, now_unix};
use crate::validation::{self, ValidationOptions};
use crate::video_export::{self};
use std::path::{Path, PathBuf};

fn resolve_batch_selector(case_dir: &Path, selector: &str) -> Result<PathBuf, String> {
    // Keep BOTH resolvers' context: swallowing the index error hides why
    // an artifact selector also failed (e.g. typo'd vid id reported as a
    // carve-log miss instead of "not indexed").
    match crate::video_export::resolve_video_source(case_dir, selector) {
        Ok(path) => Ok(path),
        Err(index_err) => crate::validation::resolve_artifact_path(case_dir, selector)
            .map_err(|artifact_err| format!("{index_err}; artifact lookup: {artifact_err}")),
    }
}

/// Names batch outputs after the selection item (not the resolved source
/// path, which would produce path-mangled filenames).
fn batch_output_path(
    case_dir: &Path,
    relative_dir: &str,
    selector: &str,
    extension: &str,
) -> Result<PathBuf, String> {
    let unix = now_unix()?;
    // Batch items pass this path as the EXPLICIT output to export_video,
    // whose explicit branch hard-rejects any pre-existing target — so no
    // placeholder reservation here; the artifact is created immediately
    // after the check by the same call.
    Ok(crate::util::unique_available_path(
        &case_dir.join(relative_dir).join(format!(
            "{}_{}.{}",
            crate::video_export::sanitize_filename(selector),
            unix,
            extension
        )),
    ))
}

pub fn export_batch(case_dir: &Path, selection_path: &Path, dry_run: bool) -> Result<(), String> {
    ensure_case(case_dir)?;
    let selection = crate::selection::parse_selection_file(selection_path)?;
    crate::selection::enforce_case_binding(
        case_dir,
        selection.case_id.as_deref(),
        selection_path,
        crate::selection::ImportKind::Selection,
    )?;
    let job = case_db::start_job(
        case_dir,
        "export-batch",
        selection_path,
        Some(selection.items.len() as u64),
        &format!(
            "{{\"dry_run\":{dry_run},\"items\":{}}}",
            selection.items.len()
        ),
    )?;

    let mut outcomes = Vec::new();
    for item in &selection.items {
        let action = crate::selection::effective_action(item);
        let outcome = (|| -> Result<BatchOutcome, String> {
            match action {
                "export" => {
                    let format = crate::selection::effective_format(item)?;
                    let resolved = resolve_batch_selector(case_dir, &item.selector)?;
                    if dry_run {
                        Ok(BatchOutcome {
                            selector: item.selector.clone(),
                            action,
                            status: "dry-run-ok",
                            detail: format!("would export {} from {}", format, resolved.display()),
                        })
                    } else {
                        let options = crate::video_export::ExportOptions {
                            format: crate::video_export::ExportFormat::parse(format)?,
                            start_seconds: None,
                            duration_seconds: None,
                            output_path: Some(batch_output_path(
                                case_dir,
                                "artifacts/clips",
                                &item.selector,
                                format,
                            )?),
                            timeout_secs: None,
                            burn_in: None,
                            hash_source: false,
                        };
                        let selector = resolved.display().to_string();
                        let result = video_export::export_video(case_dir, &selector, &options)?;
                        Ok(BatchOutcome {
                            selector: item.selector.clone(),
                            action,
                            status: "ok",
                            detail: result.output_path.display().to_string(),
                        })
                    }
                }
                "proxy" => {
                    let resolved = resolve_batch_selector(case_dir, &item.selector)?;
                    if dry_run {
                        Ok(BatchOutcome {
                            selector: item.selector.clone(),
                            action,
                            status: "dry-run-ok",
                            detail: format!("would generate proxy from {}", resolved.display()),
                        })
                    } else {
                        let options = crate::artifacts::ProxyOptions {
                            output_path: Some(batch_output_path(
                                case_dir,
                                "artifacts/proxies",
                                &item.selector,
                                "mp4",
                            )?),
                            ..crate::artifacts::ProxyOptions::default()
                        };
                        let result = artifacts::generate_proxy(
                            case_dir,
                            &resolved.display().to_string(),
                            &options,
                        )?;
                        Ok(BatchOutcome {
                            selector: item.selector.clone(),
                            action,
                            status: "ok",
                            detail: result.output_path.display().to_string(),
                        })
                    }
                }
                "thumbnail" => {
                    let resolved = resolve_batch_selector(case_dir, &item.selector)?;
                    if dry_run {
                        Ok(BatchOutcome {
                            selector: item.selector.clone(),
                            action,
                            status: "dry-run-ok",
                            detail: format!("would generate thumbnail from {}", resolved.display()),
                        })
                    } else {
                        let options = crate::artifacts::ThumbnailOptions {
                            time_seconds: item.time_seconds.unwrap_or(0.0),
                            output_path: Some(batch_output_path(
                                case_dir,
                                "artifacts/thumbnails",
                                &item.selector,
                                "jpg",
                            )?),
                            timeout_secs: None,
                        };
                        let result = artifacts::generate_thumbnail(
                            case_dir,
                            &resolved.display().to_string(),
                            &options,
                        )?;
                        Ok(BatchOutcome {
                            selector: item.selector.clone(),
                            action,
                            status: "ok",
                            detail: result.output_path.display().to_string(),
                        })
                    }
                }
                _ => Ok(BatchOutcome {
                    selector: item.selector.clone(),
                    action,
                    status: "skipped",
                    detail: format!(
                        "action '{}' is not part of export-batch; use validate-batch",
                        action
                    ),
                }),
            }
        })();
        outcomes.push(match outcome {
            Ok(outcome) => outcome,
            Err(error) => BatchOutcome {
                selector: item.selector.clone(),
                action,
                status: "failed",
                detail: error,
            },
        });
    }

    let ok = outcomes
        .iter()
        .filter(|o| o.status == "ok" || o.status == "dry-run-ok")
        .count();
    let failed = outcomes.iter().filter(|o| o.status == "failed").count();
    let skipped = outcomes.iter().filter(|o| o.status == "skipped").count();

    if !dry_run {
        let line = format!(
            "{{\"schema_version\":1,\"event\":\"export-batch\",\"selection_path\":\"{}\",\"requested\":{},\"ok\":{},\"failed\":{},\"skipped\":{},\"results\":{}}}",
            json_escape(&selection_path.display().to_string()),
            outcomes.len(),
            ok,
            failed,
            skipped,
            outcomes_json(&outcomes),
        );
        audit::append_chained_jsonl(&case_dir.join("artifacts/logs/batch-log.jsonl"), &line)?;
    }

    // A batch whose every item failed is NOT a success: the job row must
    // reflect that, or `inspect` summarizes a fully-failed batch as complete.
    if ok == 0 && failed > 0 && failed == outcomes.len() {
        let _ = case_db::fail_job(
            case_dir,
            &job.job_id,
            &format!("all {failed} item(s) failed"),
        );
        return Err(format!(
            "export batch failed: {failed} of {} item(s) failed",
            outcomes.len()
        ));
    }

    case_db::complete_job(
        case_dir,
        &job.job_id,
        outcomes.len() as u64,
        if dry_run {
            "export-batch dry run"
        } else {
            "export-batch completed"
        },
    )?;

    println!(
        "export batch {}",
        if dry_run { "dry run" } else { "complete" }
    );
    println!("requested: {}", outcomes.len());
    println!("ok: {ok}");
    println!("failed: {failed}");
    println!("skipped: {skipped}");
    for outcome in &outcomes {
        println!(
            "  [{}] {} ({}): {}",
            outcome.status, outcome.selector, outcome.action, outcome.detail
        );
    }
    Ok(())
}

pub fn validate_batch(
    case_dir: &Path,
    selection_path: &Path,
    resume: ResumeMode,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let selection = crate::selection::parse_selection_file(selection_path)?;
    crate::selection::enforce_case_binding(
        case_dir,
        selection.case_id.as_deref(),
        selection_path,
        crate::selection::ImportKind::Selection,
    )?;
    let job = case_db::start_job(
        case_dir,
        "validate-batch",
        selection_path,
        Some(selection.items.len() as u64),
        &format!("{{\"items\":{}}}", selection.items.len()),
    )?;

    // Mid-run resume (R-1): the checkpoint fingerprints the selection file
    // content + ffprobe lane, so a different selection never silently skips
    // items. `done` lines replay the recorded outcome (their validation-log
    // entries are already durable); `computed` lines replay the stored
    // per-file result and only need the log append retried.
    let selection_digest = audit::digest_file(selection_path)?;
    let fingerprint = checkpoint::fingerprint(&["validate-batch", &selection_digest, "ffprobe"]);
    let checkpoint = RunCheckpoint::begin(
        &case_dir.join("db/validate-batch-progress.jsonl"),
        "validate-batch",
        &fingerprint,
        resume,
    )?;
    let mut done: std::collections::HashMap<usize, BatchOutcome> = std::collections::HashMap::new();
    let mut computed: std::collections::HashMap<usize, validation::ValidationResult> =
        std::collections::HashMap::new();
    for line in checkpoint.data_lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(done_value) = value.get("done") {
            let Some(index) = done_value
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .map(|index| index as usize)
            else {
                continue;
            };
            let field = |key: &str| {
                done_value
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            done.remove(&index);
            computed.remove(&index);
            if index >= selection.items.len()
                || field("status") != "ok"
                || field("selector") != selection.items[index].selector
                || !validation::checkpoint_target_is_current(
                    case_dir,
                    &field("selector"),
                    Path::new(&field("target_path")),
                    &field("target_sha256"),
                )
            {
                continue;
            }
            done.insert(
                index,
                BatchOutcome {
                    selector: field("selector"),
                    action: "validate",
                    status: if field("status") == "ok" {
                        "ok"
                    } else {
                        "failed"
                    },
                    detail: field("detail"),
                },
            );
        } else if let Some((index, result)) = validation::from_checkpoint_line(line)
            && index < selection.items.len()
        {
            computed.insert(index, result);
        }
    }
    let replayed_done = done.len();
    let mut replayed_computed = 0usize;

    // Compute phase runs in parallel (per-file SHA-256 + ffprobe dominate the
    // runtime); validation log appends then happen sequentially so the hash
    // chain stays ordered and verifiable. The video index is loaded once up
    // front — per-item reads of db/videos.jsonl made hash-mismatch checks
    // O(items x index size). An unreadable index previously surfaced as "no
    // anomaly flag" per item, so keep that failure-soft behaviour.
    let video_index = crate::anomaly::index_by_id(case_dir).unwrap_or_default();
    let options = ValidationOptions::default();
    let items = &selection.items;
    let pending: Vec<usize> = (0..items.len())
        .filter(|index| !done.contains_key(index) && !computed.contains_key(index))
        .collect();
    let slots: std::sync::Mutex<Vec<Option<Result<crate::validation::ValidationResult, String>>>> =
        std::sync::Mutex::new((0..items.len()).map(|_| None).collect());
    let next_index = std::sync::atomic::AtomicUsize::new(0);
    let completed_count = std::sync::atomic::AtomicUsize::new(replayed_done);
    let progress_job_id = job.job_id.clone();
    // The checkpoint moves into a mutex for the parallel sweep so workers
    // can persist each finished compute; it is taken back afterwards.
    let checkpoint_mutex = std::sync::Mutex::new(checkpoint);
    let items = &items;
    let slots = &slots;
    let next_index = &next_index;
    let pending = &pending;
    let video_index = &video_index;
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .clamp(1, 8);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let position = next_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some(&index) = pending.get(position) else {
                        break;
                    };
                    let item = &items[index];
                    let outcome = crate::validation::compute_validation(
                        case_dir,
                        &item.selector,
                        &options,
                        video_index,
                    );
                    // Persist each completed compute immediately: a crash
                    // mid-sweep then resumes without re-hashing/re-probing
                    // the finished items. A checkpoint write failure is
                    // recorded as the item's error so the batch still knows
                    // the compute result was not made durable.
                    if let Ok(result) = &outcome
                        && let Err(err) = lock_or_recover(&checkpoint_mutex)
                            .append_line(&validation::checkpoint_line(index, result))
                    {
                        lock_or_recover(slots)[index] = Some(Err(err));
                        continue;
                    }
                    lock_or_recover(slots)[index] = Some(outcome);
                    // Progress ticks land on whichever worker crosses each
                    // 16-item boundary — best-effort, so a SQLite hiccup
                    // never fails the batch.
                    let finished =
                        completed_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if finished.is_multiple_of(16) || finished == items.len() {
                        let _ = case_db::report_job_progress(
                            case_dir,
                            &progress_job_id,
                            None,
                            finished as u64,
                        );
                    }
                }
            });
        }
    });

    let mut checkpoint = checkpoint_mutex
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut outcomes = Vec::new();
    let mut results = lock_or_recover(slots).drain(..).collect::<Vec<_>>();
    for (index, item) in selection.items.iter().enumerate() {
        if let Some(outcome) = done.remove(&index) {
            outcomes.push(outcome);
            continue;
        }
        let computed_result = match computed.remove(&index) {
            Some(result) => {
                replayed_computed += 1;
                Ok(result)
            }
            None => results[index]
                .take()
                .unwrap_or_else(|| Err("validation worker lost its result".to_string())),
        };
        let outcome = match computed_result {
            Ok(result) => match validation::append_validation_log(case_dir, &result, &options) {
                Ok(()) => {
                    let outcome = BatchOutcome {
                        selector: item.selector.clone(),
                        action: "validate",
                        status: if result.validation_status == "validation-failed" {
                            "failed"
                        } else {
                            "ok"
                        },
                        detail: format!("{} ({})", result.validation_status, result.target_sha256),
                    };
                    // The log entry is durable → the item is done. Ordering
                    // matters: done-after-append risks a duplicate log line
                    // on resume, done-before-append risks a replayed outcome
                    // with no log entry behind it.
                    checkpoint.append_line(&format!(
                        "{{\"done\":{{\"index\":{},\"selector\":\"{}\",\"status\":\"{}\",\"detail\":\"{}\",\"target_path\":\"{}\",\"target_sha256\":\"{}\"}}}}",
                        index,
                        json_escape(&outcome.selector),
                        outcome.status,
                        json_escape(&outcome.detail),
                        json_escape(&result.target_path.to_string_lossy()),
                        json_escape(&result.target_sha256)
                    ))?;
                    outcome
                }
                Err(error) => BatchOutcome {
                    selector: item.selector.clone(),
                    action: "validate",
                    status: "failed",
                    detail: error,
                },
            },
            Err(error) => BatchOutcome {
                selector: item.selector.clone(),
                action: "validate",
                status: "failed",
                detail: error,
            },
        };
        outcomes.push(outcome);
    }

    let ok = outcomes.iter().filter(|o| o.status == "ok").count();
    let failed = outcomes.iter().filter(|o| o.status == "failed").count();
    if replayed_done > 0 || replayed_computed > 0 {
        let resume_line = format!(
            "{{\"schema_version\":1,\"event\":\"validate-batch-resume\",\"selection_path\":\"{}\",\"run_id\":\"{}\",\"skipped_items\":{},\"reused_computed\":{}}}",
            json_escape(&selection_path.display().to_string()),
            json_escape(checkpoint.run_id()),
            replayed_done,
            replayed_computed
        );
        audit::append_chained_jsonl(
            &case_dir.join("artifacts/logs/batch-log.jsonl"),
            &resume_line,
        )?;
    }
    let line = format!(
        "{{\"schema_version\":1,\"event\":\"validate-batch\",\"selection_path\":\"{}\",\"requested\":{},\"ok\":{},\"failed\":{},\"results\":{}}}",
        json_escape(&selection_path.display().to_string()),
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
            "validate batch failed: {failed} of {} item(s) failed",
            outcomes.len()
        ));
    }
    case_db::complete_job(
        case_dir,
        &job.job_id,
        outcomes.len() as u64,
        "validate-batch completed",
    )?;
    checkpoint.finish()?;

    println!("validate batch complete");
    println!("requested: {}", outcomes.len());
    println!("ok: {ok}");
    println!("failed: {failed}");
    if replayed_done > 0 || replayed_computed > 0 {
        println!(
            "resumed from checkpoint: {replayed_done} item(s) replayed, {replayed_computed} reused pending log"
        );
    }
    for outcome in &outcomes {
        println!(
            "  [{}] {}: {}",
            outcome.status, outcome.selector, outcome.detail
        );
    }
    Ok(())
}

pub fn import_marks(case_dir: &Path, marks_path: &Path) -> Result<(), String> {
    let outcome = import_marks_quiet(case_dir, marks_path)?;
    println!("marks imported");
    println!("source: {}", marks_path.display());
    println!("marks stored: {}", outcome.stored);
    println!("records applied: {}", outcome.applied.len());
    println!("conflicts: {}", outcome.conflicts.len());
    for conflict in &outcome.conflicts {
        println!(
            "  [conflict] {} server_revision={} server={}",
            conflict.id,
            conflict.server_revision,
            serde_json::to_string(&conflict.server).unwrap_or_else(|_| "null".into())
        );
    }
    println!("sqlite: {}", case_db::case_db_path(case_dir).display());
    if !outcome.conflicts.is_empty() {
        return Err(format!(
            "{} record(s) not applied: the file's base_revisions are older than the case (see conflicts above)",
            outcome.conflicts.len()
        ));
    }
    Ok(())
}

/// [`import_marks`] without the stdout report, so the workstation server
/// can apply marks in-process and return what was applied and what
/// conflicted (save protocol v2; legacy bodies never conflict).
pub fn import_marks_quiet(
    case_dir: &Path,
    marks_path: &Path,
) -> Result<case_db::PatchOutcome, String> {
    ensure_case(case_dir)?;
    let marks_file = crate::selection::parse_marks_file(marks_path)?;
    crate::selection::enforce_case_binding(
        case_dir,
        marks_file.case_id.as_deref(),
        marks_path,
        crate::selection::ImportKind::Marks,
    )?;
    let rows = marks_file
        .marks
        .iter()
        .map(|entry| {
            let marked_unix = match entry.marked_unix {
                Some(stamp) => stamp,
                // A clock failure must not silently stamp epoch-adjacent
                // times into the case record — fail the import instead.
                None => now_unix()?,
            };
            Ok(case_db::ReviewMarkRow {
                record_id: entry.id.clone(),
                status: entry.status.clone(),
                marked_unix,
                record_path: None,
                examiner: entry
                    .examiner
                    .clone()
                    .or_else(|| marks_file.examiner.clone()),
                note: entry.note.clone(),
                range: entry.range,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    // The file-level examiner is the only "who" deletions and tag edits
    // carry into review_mark_history (FOR-07).
    let patch = case_db::AnnotationPatch {
        marks: &rows,
        tags: &marks_file.tags,
        deleted_ids: &marks_file.deleted_ids,
        base_revisions: marks_file.base_revisions.as_ref(),
    };
    case_db::patch_review_annotations_v2(
        case_dir,
        &patch,
        marks_file.examiner.as_deref(),
        marks_file.client_id.as_deref(),
    )
}

pub fn export_marks(case_dir: &Path, output: Option<&Path>) -> Result<(), String> {
    ensure_case(case_dir)?;
    let case_id = crate::selection::read_case_id(case_dir)?;
    let marks = case_db::load_review_marks(case_dir)?;
    let entries = marks
        .iter()
        .map(|mark| {
            format!(
                "{{\"id\":\"{}\",\"status\":\"{}\",\"marked_unix\":{}{}{}}}",
                crate::util::json_escape(&mark.record_id),
                crate::util::json_escape(&mark.status),
                mark.marked_unix,
                mark.examiner
                    .as_deref()
                    .map(|name| format!(",\"examiner\":\"{}\"", crate::util::json_escape(name)))
                    .unwrap_or_default(),
                mark.note
                    .as_deref()
                    .map(|note| format!(",\"note\":\"{}\"", crate::util::json_escape(note)))
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join(",\n    ");
    let tags = serde_json::to_string(&case_db::load_review_tags(case_dir)?)
        .map_err(|err| err.to_string())?;
    let text = format!(
        "{{\n  \"schema_version\": 2,\n  \"case_id\": \"{}\",\n  \"exported_unix\": {},\n  \"marks\": [\n    {}\n  ],\n  \"tags\": {}\n}}\n",
        json_escape(&case_id),
        now_unix()?,
        entries,
        tags
    );
    let output_path = output
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| case_dir.join("db/review-marks.json"));
    crate::tool_policy::write_case_report(
        case_dir,
        &output_path,
        "db/review-marks.json",
        "marks export",
        &text,
    )?;
    println!("marks exported");
    println!("marks: {}", marks.len());
    println!("output: {}", output_path.display());
    Ok(())
}
