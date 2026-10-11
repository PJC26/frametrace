use super::*;
use crate::case_db;
use crate::checkpoint::ResumeMode;
use crate::html_report;
use crate::model::{CaseManifest, ScanOptions};
use crate::package;
use crate::report;
use crate::scan;
use crate::util::{create_case_layout, now_unix, read_to_string, write_text};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Default)]
pub struct InitCaseOptions {
    pub title: Option<String>,
    pub operator: Option<String>,
    pub device_id: Option<String>,
    pub device_serial: Option<String>,
    pub write_protect: Option<String>,
    pub acquisition_tool: Option<String>,
    pub evidence_hash: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RegisterSourceOptions {
    pub kind: String,
    pub source_id: Option<String>,
    pub write_protect: Option<String>,
    pub acquisition_tool: Option<String>,
    pub evidence_hash: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PackageOptions {
    pub output_dir: Option<PathBuf>,
}

pub fn init_case(case_dir: &Path, options: &InitCaseOptions) -> Result<(), String> {
    let manifest_path = case_dir.join("case.json");
    if fs::symlink_metadata(&manifest_path).is_ok() {
        return Err(format!(
            "case manifest already exists: {}",
            manifest_path.display()
        ));
    }
    if case_dir.is_dir()
        && fs::read_dir(case_dir)
            .map_err(|err| format!("failed to inspect case directory: {err}"))?
            .next()
            .transpose()
            .map_err(|err| format!("failed to inspect case directory entry: {err}"))?
            .is_some()
    {
        return Err("case directory already exists and is not empty".to_string());
    }
    let created_unix = now_unix()?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|err| format!("failed to generate case id: {err}"))?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let manifest = CaseManifest {
        schema_version: 1,
        case_id: format!("FT-{created_unix}-{suffix}"),
        title: options
            .title
            .clone()
            .unwrap_or_else(|| "Untitled FrameTrace case".to_string()),
        created_unix,
        tool_name: env!("CARGO_PKG_NAME").to_string(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: env::consts::OS.to_string(),
        operator: options.operator.clone().or_else(default_operator),
        host: default_host(),
        device_id: options.device_id.clone(),
        device_serial: options.device_serial.clone(),
        write_protect: options.write_protect.clone(),
        acquisition_tool: options.acquisition_tool.clone(),
        evidence_hash: options.evidence_hash.clone(),
        notes: options.notes.clone(),
    };

    let manifest_json = manifest.to_json();
    publish_case_manifest(case_dir, &manifest_json, |file, text| {
        file.write_all(text.as_bytes())
    })
    .map_err(|err| {
        format!("failed to publish new case manifest (existing files are never replaced): {err}")
    })?;

    println!("case created: {}", case_dir.display());
    println!("case id: {}", manifest.case_id);
    Ok(())
}

pub(crate) fn publish_case_manifest(
    case_dir: &Path,
    manifest_json: &str,
    write: impl FnOnce(&mut fs::File, &str) -> std::io::Result<()>,
) -> std::io::Result<()> {
    fs::create_dir_all(case_dir)?;
    let manifest_path = case_dir.join("case.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest_path)?;
    create_case_layout(case_dir)?;
    write(&mut file, manifest_json)?;
    file.sync_all()?;
    crate::util::sync_parent_directory(&manifest_path);
    Ok(())
}

pub fn scan_folder(
    case_dir: &Path,
    source_dir: &Path,
    options: ScanOptions,
    resume: ResumeMode,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    if !source_dir.is_dir() {
        return Err(format!(
            "source is not a directory: {}",
            source_dir.display()
        ));
    }

    let source = case_db::register_evidence_source(
        case_dir,
        &case_db::EvidenceSourceInput {
            kind: "folder".to_string(),
            path: source_dir.to_path_buf(),
            source_id: None,
            write_protect: None,
            acquisition_tool: None,
            evidence_hash: None,
            notes: Some("Auto-registered by scan-folder".to_string()),
            metadata_json: Some(scan_options_json(&options)),
        },
    )?;
    let job = case_db::start_job(
        case_dir,
        "scan-folder",
        source_dir,
        None,
        &scan_options_json(&options),
    )?;
    let progress_job_id = job.job_id.clone();
    let progress = move |done: u64, total: u64| {
        // Progress ticks are best-effort: a transient SQLite write failure
        // must never fail the scan itself.
        let _ = case_db::report_job_progress(case_dir, &progress_job_id, Some(total), done);
    };
    let result = match scan::scan_folder(case_dir, source_dir, &options, resume, Some(&progress)) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(
        case_dir,
        &job.job_id,
        result.video_count as u64,
        "scan-folder completed",
    )?;
    println!("scan complete");
    println!("source registered: {} ({})", source.source_id, source.kind);
    println!("job: {} ({})", job.job_id, job.job_type);
    println!("videos indexed: {}", result.video_count);
    if result.resumed_from_checkpoint > 0 {
        println!(
            "resumed from checkpoint: {} record(s) reused",
            result.resumed_from_checkpoint
        );
    }
    if options.incremental {
        println!(
            "unchanged files skipped: {} (size+mtime still match the index)",
            result.unchanged_files
        );
    }
    println!("bytes indexed: {}", result.total_bytes);
    println!("index: {}", case_dir.join("db/video_index.json").display());
    println!("sqlite: {}", case_db::case_db_path(case_dir).display());

    // Deepfake screening runs as its own job after indexing completes, so
    // the scan step finishes fast and screening progress is visible as a
    // separate pass. A small worker pool overlaps the per-file subprocess
    // overhead instead of serializing deepfake-lens invocations.
    if options.deepfake_screen {
        let dj = case_db::start_job(case_dir, "deepfake-scan", source_dir, None, "{}")?;
        let progress_job_id = dj.job_id.clone();
        let stats = match crate::deepfake::screen_case_parallel(
            case_dir,
            false,
            false,
            &move |done, total, id| {
                let _ = case_db::report_job_progress(
                    case_dir,
                    &progress_job_id,
                    Some(total as u64),
                    done as u64,
                );
                if !id.is_empty() {
                    println!("deepfake screen {done}/{total}: {id}");
                }
            },
            3,
        ) {
            Ok(stats) => stats,
            Err(err) => {
                let _ = case_db::fail_job(case_dir, &dj.job_id, &err);
                return Err(err);
            }
        };
        case_db::complete_job(
            case_dir,
            &dj.job_id,
            stats.screened as u64,
            "deepfake-scan completed",
        )?;
        println!(
            "deepfake screening: {} screened, {} already done, {} missing, {} failed",
            stats.screened, stats.skipped_existing, stats.skipped_missing, stats.failed
        );
    }
    Ok(())
}

pub fn register_source(
    case_dir: &Path,
    source_path: &Path,
    options: RegisterSourceOptions,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let row = case_db::register_evidence_source(
        case_dir,
        &case_db::EvidenceSourceInput {
            kind: options.kind,
            path: source_path.to_path_buf(),
            source_id: options.source_id,
            write_protect: options.write_protect,
            acquisition_tool: options.acquisition_tool,
            evidence_hash: options.evidence_hash,
            notes: options.notes,
            metadata_json: None,
        },
    )?;
    println!("source registered: {}", row.source_id);
    println!("kind: {}", row.kind);
    println!("path: {}", row.path);
    println!("sqlite: {}", case_db::case_db_path(case_dir).display());
    Ok(())
}

pub fn make_review(case_dir: &Path, redact_paths: bool, build_proxies: bool) -> Result<(), String> {
    ensure_case(case_dir)?;
    let index_path = case_dir.join("db/video_index.json");
    let index_json = read_to_string(&index_path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            format!(
                "no case index yet at {} — run scan-folder (or import-e01) before make-review",
                index_path.display()
            )
        } else {
            format!("failed to read {}: {err}", index_path.display())
        }
    })?;
    let manifest_path = case_dir.join("case.json");
    let manifest_json = read_to_string(&manifest_path)
        .map_err(|err| format!("failed to read {}: {err}", manifest_path.display()))?;
    let redact = |text: &str| -> String {
        if redact_paths {
            crate::redact::redact_json_text(text, case_dir)
        } else {
            text.to_string()
        }
    };
    let manifest_json = redact(&manifest_json);
    let index_json = redact(&index_json);
    let html = html_report::render_review_html(&manifest_json, &index_json);
    let review_path = case_dir.join("review/index.html");
    write_text(&review_path, &inject_redaction_banner(html, redact_paths))
        .map_err(|err| format!("failed to write review html: {err}"))?;
    let carve_log = redact(
        &read_to_string(&case_dir.join("artifacts/carved/carve-log.jsonl")).unwrap_or_default(),
    );
    let filesystem_log = redact(
        &read_to_string(&case_dir.join("evidence/logs/tsk-audit.jsonl")).unwrap_or_default(),
    );
    let validation_log = redact(
        &read_to_string(&case_dir.join("evidence/logs/validation-log.jsonl")).unwrap_or_default(),
    );
    // Prefer an existing anomaly log; scan on demand so make-review stays usable
    // without forcing a full-case digest pass (make-report / qa anomalies refresh).
    let anomaly_log = redact(
        &read_to_string(&case_dir.join("evidence/logs/anomaly-log.jsonl")).unwrap_or_default(),
    );
    let fls_entries = redact(&latest_fls_entries_jsonl(case_dir));
    // Optional proxy pre-build: browser-unplayable containers (AVI, DAV,
    // proprietary recorder exports) can't decode in the review page at all,
    // so an examiner who needs the standalone bundle to just play asks for
    // proxies up front instead of per-file through the workstation button.
    if build_proxies {
        build_unplayable_proxies(case_dir, &carve_log, &filesystem_log)?;
    }
    let videos = collect_index_videos(&index_json);
    let (thumbs_json, thumb_stats) = generate_review_thumbnails(case_dir, &videos)?;
    let annotations_json = serde_json::to_string(&serde_json::json!({
        "marks": crate::case_db::load_review_marks(case_dir).unwrap_or_default(),
        "tags": crate::case_db::load_review_tags(case_dir).unwrap_or_default(),
    }))
    .map_err(|err| err.to_string())?;
    let deepfake_reports = crate::deepfake::collect_reports(case_dir).to_string();
    let telemetry_reports = crate::telemetry::collect_reports(case_dir).to_string();
    // Proxies already on disk are embedded so the viewer (including the
    // standalone file:// bundle) can prefer them for unplayable originals.
    let proxies_json = collect_proxies_json(case_dir);
    // The viewer page stays slim; the (potentially huge) record payload
    // lives in data-bundle.js beside it. Standalone file:// use loads the
    // bundle through a script tag; workstation serving pages the same
    // data through /api/records instead.
    let data_bundle = html_report::render_data_bundle_js(
        &manifest_json,
        &index_json,
        &carve_log,
        &filesystem_log,
        &validation_log,
        &anomaly_log,
        &fls_entries,
        &thumbs_json,
        &annotations_json,
        &deepfake_reports,
        &telemetry_reports,
        &proxies_json,
        case_dir,
    );
    write_text(&case_dir.join("review/data-bundle.js"), &data_bundle)
        .map_err(|err| format!("failed to write review data bundle: {err}"))?;
    let evidence_viewer = html_report::render_evidence_viewer_html_slim();
    let evidence_viewer_path = case_dir.join("review/evidence-viewer.html");
    write_text(
        &evidence_viewer_path,
        &inject_redaction_banner(evidence_viewer, redact_paths),
    )
    .map_err(|err| format!("failed to write evidence viewer html: {err}"))?;
    // Dedicated carve view: a raw-image carve can return hundreds of
    // artifacts that would drown the evidence grid, so carving results get
    // their own page beside the viewer.
    if !carve_log.trim().is_empty() {
        let carve_results =
            redact(&read_to_string(&case_dir.join("db/carve_results.json")).unwrap_or_default());
        let carve_report = html_report::render_carve_report_html(
            &manifest_json,
            &carve_log,
            &carve_results,
            &proxies_json,
            case_dir,
            &index_json,
        );
        let carve_report_path = case_dir.join("review/carve-report.html");
        write_text(
            &carve_report_path,
            &inject_redaction_banner(carve_report, redact_paths),
        )
        .map_err(|err| format!("failed to write carve report html: {err}"))?;
        println!("carve report written: {}", carve_report_path.display());
    }
    println!("review written: {}", review_path.display());
    println!(
        "evidence viewer written: {}",
        evidence_viewer_path.display()
    );
    println!(
        "thumbnails: {} created, {} cached, {} unavailable{}",
        thumb_stats.created,
        thumb_stats.cached,
        thumb_stats.skipped,
        if thumb_stats.ffmpeg_missing {
            " (ffmpeg not found; rerun with ffmpeg in PATH)"
        } else {
            ""
        }
    );
    Ok(())
}

/// Inserts the redaction notice right after `<body>` so generated pages that
/// do not take an explicit flag still visibly mark themselves as redacted.
fn inject_redaction_banner(html: String, redact_paths: bool) -> String {
    if !redact_paths {
        return html;
    }
    let banner = format!(
        "<div style=\"padding:8px 16px;background:#fff7ed;border-bottom:1px solid #f4c790;font-size:13px\">{}</div>",
        crate::util::html_escape(crate::redact::REDACTION_NOTE)
    );
    match html.find("<body>") {
        Some(index) => format!(
            "{}{}{}",
            &html[..index + "<body>".len()],
            banner,
            &html[index + "<body>".len()..]
        ),
        None => format!("{banner}{html}"),
    }
}

/// The review marks exactly as make-report embeds them (before path
/// redaction). The finalize sign-off binds to its digest
/// ([`report::signoff_content_sha256`]) so a later report can say whether
/// the marks changed after the sign-off.
pub(crate) fn review_marks_json(case_dir: &Path) -> String {
    match crate::case_db::load_review_marks(case_dir) {
        Ok(rows) => {
            let entries = rows
                .iter()
                .map(|mark| {
                    format!(
                        "{{\"id\":\"{}\",\"status\":\"{}\",\"marked_unix\":{}{}{}}}",
                        crate::util::json_escape(&mark.record_id),
                        crate::util::json_escape(&mark.status),
                        mark.marked_unix,
                        mark.examiner
                            .as_deref()
                            .map(|name| format!(
                                ",\"examiner\":\"{}\"",
                                crate::util::json_escape(name)
                            ))
                            .unwrap_or_default(),
                        mark.note
                            .as_deref()
                            .map(|note| format!(",\"note\":\"{}\"", crate::util::json_escape(note)))
                            .unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("[{entries}]")
        }
        Err(_) => "[]".to_string(),
    }
}

pub fn make_report(
    case_dir: &Path,
    rehash: bool,
    redact_paths: bool,
    signoff_path: Option<&Path>,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let index_path = case_dir.join("db/video_index.json");
    let index_json = read_to_string(&index_path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            format!(
                "no case index yet at {} — run scan-folder (or import-e01) before make-review",
                index_path.display()
            )
        } else {
            format!("failed to read {}: {err}", index_path.display())
        }
    })?;
    let anomaly_scan = crate::anomaly::scan_case(case_dir, rehash)?;
    println!(
        "anomaly scan: {} candidate finding(s) (hash mode: {})",
        anomaly_scan.findings.len(),
        if rehash {
            "rehash — live digest of every indexed file"
        } else {
            "stored index hashes (pass --rehash for full revalidation)"
        }
    );
    let manifest_path = case_dir.join("case.json");
    let manifest_json = read_to_string(&manifest_path)
        .map_err(|err| format!("failed to read {}: {err}", manifest_path.display()))?;
    let export_log =
        read_to_string(&case_dir.join("artifacts/clips/export-log.jsonl")).unwrap_or_default();
    let proxy_log =
        read_to_string(&case_dir.join("artifacts/proxies/proxy-log.jsonl")).unwrap_or_default();
    let thumbnail_log = read_to_string(&case_dir.join("artifacts/thumbnails/thumbnail-log.jsonl"))
        .unwrap_or_default();
    let carve_log =
        read_to_string(&case_dir.join("artifacts/carved/carve-log.jsonl")).unwrap_or_default();
    let filesystem_log =
        read_to_string(&case_dir.join("evidence/logs/tsk-audit.jsonl")).unwrap_or_default();
    let validation_log =
        read_to_string(&case_dir.join("evidence/logs/validation-log.jsonl")).unwrap_or_default();
    let anomaly_log =
        read_to_string(&case_dir.join("evidence/logs/anomaly-log.jsonl")).unwrap_or_default();
    let batch_log =
        read_to_string(&case_dir.join("artifacts/logs/batch-log.jsonl")).unwrap_or_default();
    let scan_runs_json = read_scan_runs_json(case_dir);
    let marks_json = review_marks_json(case_dir);
    let signoff_content = report::signoff_content_sha256(&marks_json);
    let redact = |text: String| -> String {
        if redact_paths {
            crate::redact::redact_json_text(&text, case_dir)
        } else {
            text
        }
    };
    let manifest_json = redact(manifest_json);
    let index_json = redact(index_json);
    let export_log = redact(export_log);
    let proxy_log = redact(proxy_log);
    let thumbnail_log = redact(thumbnail_log);
    let carve_log = redact(carve_log);
    let filesystem_log = redact(filesystem_log);
    let validation_log = redact(validation_log);
    let anomaly_log = redact(anomaly_log);
    let batch_log = redact(batch_log);
    let scan_runs_json = redact(scan_runs_json);
    let marks_json = redact(marks_json);
    let html = report::render_case_report(&report::ReportInputs {
        manifest_json: &manifest_json,
        index_json: &index_json,
        export_log_jsonl: &export_log,
        proxy_log_jsonl: &proxy_log,
        thumbnail_log_jsonl: &thumbnail_log,
        carve_log_jsonl: &carve_log,
        filesystem_log_jsonl: &filesystem_log,
        validation_log_jsonl: &validation_log,
        anomaly_log_jsonl: &anomaly_log,
        batch_log_jsonl: &batch_log,
        scan_runs_json: &scan_runs_json,
        marks_json: &marks_json,
        redaction_applied: redact_paths,
    });
    // Finalize sign-off: the reviewer name recorded by the workstation (as
    // typed - not an authenticated identity) is printed and shown in the report.
    // `signoff_path` is the workstation's not-yet-promoted record of an
    // in-progress finalize; otherwise the promoted review/signoff.json.
    let signoff_file = signoff_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| case_dir.join(report::SIGNOFF_REL_PATH));
    let signoff_text = read_to_string(&signoff_file).ok();
    let html = match signoff_text.as_deref().and_then(report::parse_signoff) {
        Some((examiner, signed_unix)) => {
            let currency = report::signoff_currency(
                signoff_text.as_deref().unwrap_or_default(),
                &signoff_content,
            );
            println!(
                "검토자: {examiner} (최종화 시 입력된 이름, 신원 미인증) — {}",
                currency.label()
            );
            report::inject_signoff_banner(html, &examiner, signed_unix, currency)
        }
        None => html,
    };
    let report_path = case_dir.join("reports/case-report.html");
    write_text(&report_path, &html).map_err(|err| format!("failed to write report html: {err}"))?;
    println!("report written: {}", report_path.display());
    Ok(())
}

pub fn make_custody(case_dir: &Path) -> Result<(), String> {
    ensure_case(case_dir)?;
    let path = crate::custody::generate(case_dir)?;
    println!("custody statement written: {}", path.display());
    Ok(())
}

/// Reads every db/scan_runs/*.json snapshot and returns them joined into a
/// JSON array literal for the report script.
fn read_scan_runs_json(case_dir: &Path) -> String {
    let runs_dir = case_dir.join("db/scan_runs");
    let Ok(entries) = std::fs::read_dir(&runs_dir) else {
        return "[]".to_string();
    };
    let mut runs: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = read_to_string(&path) else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        runs.push((name, content));
    }
    runs.sort_by_key(|(name, _)| name.clone());
    let joined = runs
        .iter()
        .map(|(_, content)| content.trim())
        .collect::<Vec<_>>()
        .join(",");
    format!("[{joined}]")
}

pub fn package_case(case_dir: &Path, options: PackageOptions) -> Result<(), String> {
    ensure_case(case_dir)?;
    // A package should never ship without its custody statement; refresh it
    // best-effort so a stale statement can't be sealed into a checksum
    // manifest. Generation failure must not block packaging — the missing
    // file is itself disclosed by the package README.
    if let Err(err) = crate::custody::generate(case_dir) {
        eprintln!("warning: custody statement not refreshed: {err}");
    }
    let result = package::package_case(case_dir, options.output_dir.as_deref())?;
    println!("case package written");
    println!("output: {}", result.output_dir.display());
    println!("files: {}", result.file_count);
    println!("manifest: {}", result.manifest_path.display());
    Ok(())
}

pub fn inspect(case_dir: &Path) -> Result<(), String> {
    ensure_case(case_dir)?;
    let manifest_path = case_dir.join("case.json");
    let index_path = case_dir.join("db/video_index.json");
    println!("case: {}", case_dir.display());
    println!("manifest: {}", manifest_path.display());
    if index_path.exists() {
        let text = read_to_string(&index_path)
            .map_err(|err| format!("failed to read {}: {err}", index_path.display()))?;
        println!("index: {}", index_path.display());
        println!(
            "videos indexed: {}",
            extract_json_number(&text, "video_count").unwrap_or_else(|| "unknown".to_string())
        );
        println!(
            "bytes indexed: {}",
            extract_json_number(&text, "total_bytes").unwrap_or_else(|| "unknown".to_string())
        );
    } else {
        println!("index: not created yet");
    }
    match case_db::summarize_case_db(case_dir)? {
        Some(summary) => {
            println!("sqlite: {}", summary.path.display());
            println!("sqlite videos: {}", summary.video_count);
            println!("sqlite scan runs: {}", summary.scan_run_count);
            println!("sqlite evidence sources: {}", summary.evidence_source_count);
            println!("sqlite jobs: {}", summary.job_count);
            println!("sqlite active jobs: {}", summary.active_job_count);
        }
        None => println!("sqlite: not created yet"),
    }
    Ok(())
}

/// (id, source path) pairs for every video in the case index.
fn collect_index_videos(index_json: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(items) = crate::selection::json_array_field(index_json, "videos") {
        for object in crate::selection::json_objects_in_array(&items) {
            if let (Some(id), Some(source)) = (
                crate::selection::json_string_field(&object, "id"),
                crate::selection::json_string_field(&object, "source_path"),
            ) {
                out.push((id, source));
            }
        }
    }
    out
}

/// Absolute paths of every generated review proxy under
/// artifacts/proxies, as a JSON array literal for the data bundle.
fn collect_proxies_json(case_dir: &Path) -> String {
    let mut paths: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(case_dir.join("artifacts/proxies")) {
        for entry in entries.flatten() {
            // Canonicalize so the bundle carries absolute paths — a
            // relative case_dir arg would otherwise produce file://-broken
            // relative URLs in the standalone viewer.
            let path = crate::audit::canonical_or_original(&entry.path());
            if path.extension().and_then(|ext| ext.to_str()) == Some("mp4") {
                paths.push(crate::audit::path_string(&path));
            }
        }
    }
    paths.sort();
    serde_json::to_string(&paths).unwrap_or_else(|_| "[]".to_string())
}

/// Generates proxies for every record the viewer can list but a browser
/// cannot decode: indexed videos that trip `needs_transcode`, plus carved
/// and filesystem-recovered outputs (which bypass the index, so they are
/// probed directly). Failures are reported per item, never hidden.
fn build_unplayable_proxies(
    case_dir: &Path,
    carve_log: &str,
    filesystem_log: &str,
) -> Result<(), String> {
    // Indexed videos: the existing queue applies the shared playability gate.
    let queue = crate::transcode::run_queue(case_dir, None, false)?;
    println!(
        "proxy build (indexed): {} candidates · {} proxied · {} cached · {} failed",
        queue.total, queue.proxied, queue.skipped_existing, queue.failed
    );

    // Non-indexed outputs: carved candidates and recover-inode artifacts.
    let mut outputs: Vec<String> = Vec::new();
    for line in carve_log
        .lines()
        .chain(filesystem_log.lines())
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let Ok(item) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let is_recover = item.get("event").and_then(|v| v.as_str()) == Some("recover-inode")
            || item.get("output_path").is_some();
        let Some(path) = item.get("output_path").and_then(|v| v.as_str()) else {
            continue;
        };
        if !is_recover
            || path.is_empty()
            || item.get("size_bytes").and_then(|v| v.as_u64()) == Some(0)
        {
            continue;
        }
        // Logs can hold invocation-cwd-relative paths (pre-canonicalize
        // carve runs, cross-case copies); resolve them the same way the
        // carve report does or the artifact is silently skipped here.
        let resolved = crate::util::resolve_case_artifact_path(case_dir, path);
        if !resolved.is_file() {
            continue;
        }
        let resolved = crate::audit::path_string(&resolved);
        if !outputs.iter().any(|p| p == &resolved) {
            outputs.push(resolved);
        }
    }

    let mut proxied = 0usize;
    let mut skipped_existing = 0usize;
    let mut skipped_playable = 0usize;
    let mut failed = 0usize;
    for path in outputs {
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let probe = crate::ffprobe::probe(Path::new(&path));
        // A probe counts as failed only when ffprobe itself errored or the
        // output carried no usable media identification at all.
        let probe_ok = Some(probe.ok && probe.video_codec.is_some());
        let Some(why) = crate::transcode::unplayable_reason(
            ext,
            probe_ok,
            probe.format_name.as_deref().unwrap_or(""),
            probe.video_codec.as_deref().unwrap_or(""),
        ) else {
            skipped_playable += 1;
            continue;
        };
        let prefix = format!("{}_proxy_", crate::video_export::sanitize_filename(&path));
        let exists = std::fs::read_dir(case_dir.join("artifacts/proxies"))
            .ok()
            .map(|entries| {
                entries.flatten().any(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    name.starts_with(&prefix) && name.ends_with(".mp4")
                })
            })
            .unwrap_or(false);
        if exists {
            skipped_existing += 1;
            continue;
        }
        match crate::artifacts::generate_proxy(
            case_dir,
            &path,
            &crate::artifacts::ProxyOptions::default(),
        ) {
            Ok(result) => {
                proxied += 1;
                println!(
                    "proxy built: {} ← {} ({why})",
                    result.output_path.display(),
                    path
                );
            }
            Err(err) => {
                failed += 1;
                println!("proxy failed: {path} ({why}) — {err}");
            }
        }
    }
    println!(
        "proxy build (recovered/carved): {} proxied · {} cached · {} already playable · {} failed",
        proxied, skipped_existing, skipped_playable, failed
    );
    Ok(())
}

#[derive(Debug, Default)]
struct ThumbnailStats {
    created: usize,
    cached: usize,
    skipped: usize,
    ffmpeg_missing: bool,
}

/// Generates a representative frame per video into review/thumbs/<id>.jpg for
/// the viewer's thumbnail grid. ffmpeg is optional: without it the viewer
/// shows placeholders. Existing thumbs newer than their source are reused.
fn generate_review_thumbnails(
    case_dir: &Path,
    videos: &[(String, String)],
) -> Result<(String, ThumbnailStats), String> {
    let mut stats = ThumbnailStats::default();
    let thumbs_dir = case_dir.join("review/thumbs");
    std::fs::create_dir_all(&thumbs_dir)
        .map_err(|err| format!("failed to create thumbnail directory: {err}"))?;

    let ffmpeg = match crate::tool_policy::resolve_tool_binary("ffmpeg", &["ffmpeg"]) {
        Ok(binary) => binary,
        Err(_) => {
            stats.ffmpeg_missing = true;
            stats.skipped = videos.len();
            return Ok(("{}".to_string(), stats));
        }
    };

    let mut map = std::collections::BTreeMap::new();
    // Fresh thumbnails are resolved up front; the ffmpeg extraction runs are
    // then fanned out across a small worker pool (extraction dominates the
    // runtime on multi-thousand-record cases, and each invocation is an
    // independent subprocess so parallelism is safe here — audit log appends
    // are not part of this loop).
    let mut cached_ids = Vec::new();
    let mut pending: Vec<(&String, &String)> = Vec::new();
    for (id, source) in videos {
        let output = thumbs_dir.join(format!("{id}.jpg"));
        if thumbnail_is_fresh(Path::new(source), &output) {
            cached_ids.push(id.clone());
        } else {
            pending.push((id, source));
        }
    }
    stats.cached = cached_ids.len();

    let created_ids: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let skipped = std::sync::atomic::AtomicUsize::new(0);
    let next_index = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let pending = &pending;
    let created_ids = &created_ids;
    let skipped = &skipped;
    let next_index = &next_index;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some((id, source)) = pending.get(index) else {
                        break;
                    };
                    let output = thumbs_dir.join(format!("{id}.jpg"));
                    let mut created = false;
                    for seek in ["5", "0"] {
                        let result = Command::new(&ffmpeg)
                            .args([
                                "-y",
                                "-loglevel",
                                "error",
                                "-ss",
                                seek,
                                "-i",
                                source,
                                "-frames:v",
                                "1",
                                "-vf",
                                "scale=288:-2",
                                "-q:v",
                                "5",
                            ])
                            .arg(&output)
                            .output();
                        match result {
                            Ok(result) if result.status.success() => {
                                created = true;
                                break;
                            }
                            _ => {
                                let _ = std::fs::remove_file(&output);
                            }
                        }
                    }
                    if created {
                        lock_or_recover(created_ids).push((*id).clone());
                    } else {
                        skipped.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            });
        }
    });
    stats.created = lock_or_recover(created_ids).len();
    stats.skipped = skipped.load(std::sync::atomic::Ordering::SeqCst);
    map.extend(
        cached_ids
            .into_iter()
            .map(|id| (id.clone(), format!("thumbs/{id}.jpg"))),
    );
    map.extend(
        lock_or_recover(created_ids)
            .iter()
            .map(|id| (id.clone(), format!("thumbs/{id}.jpg"))),
    );

    let entries = map
        .iter()
        .map(|(id, path)| {
            format!(
                "\"{}\":\"{}\"",
                crate::util::json_escape(id),
                crate::util::json_escape(path)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    Ok((format!("{{{entries}}}"), stats))
}

fn thumbnail_is_fresh(source: &Path, thumb: &Path) -> bool {
    let (Ok(source_meta), Ok(thumb_meta)) = (std::fs::metadata(source), std::fs::metadata(thumb))
    else {
        return false;
    };
    let (Ok(source_time), Ok(thumb_time)) = (source_meta.modified(), thumb_meta.modified()) else {
        return false;
    };
    thumb_time >= source_time
}
