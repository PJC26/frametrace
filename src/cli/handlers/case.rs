use super::*;
use crate::case_db;
use crate::checkpoint::ResumeMode;
use crate::html_report;
use crate::model::{CaseManifest, ScanOptions};
use crate::package;
use crate::report;
use crate::scan;
// I14: every report/review output is replaced atomically (temp + rename),
// so a crash mid-write never leaves a torn court document.
use crate::util::{create_case_layout, now_unix, read_to_string, write_text_atomic as write_text};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    // FOR-04: an evidence hash must be well-formed hex for its algorithm;
    // it is stored lowercase and reported as operator-supplied.
    let evidence_hash = options
        .evidence_hash
        .as_deref()
        .map(case_db::parse_evidence_hash)
        .transpose()?;
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
        evidence_hash: evidence_hash.as_ref().map(|hash| hash.hex.clone()),
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
    if let Some(hash) = &evidence_hash {
        println!(
            "evidence hash: {}:{} ({})",
            hash.algorithm,
            hash.hex,
            case_db::describe_hash_origin(Some(case_db::HashOrigin::OperatorSupplied.label()))
        );
    }
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
    println!(
        "source registered: {} ({})",
        row.source_id,
        if row.newly_registered {
            "new"
        } else {
            "existing"
        }
    );
    println!("kind: {}", row.kind);
    println!("path: {}", row.path);
    match (&row.evidence_hash, &row.hash_algorithm) {
        (Some(hash), algorithm) => println!(
            "evidence hash: {}:{hash} ({})",
            algorithm.as_deref().unwrap_or("unknown-algorithm"),
            case_db::describe_hash_origin(row.hash_origin.as_deref())
        ),
        (None, _) => println!("evidence hash: none recorded"),
    }
    print_source_changes(case_dir, &row);
    println!("sqlite: {}", case_db::case_db_path(case_dir).display());
    Ok(())
}

/// Prints the custody fields a re-registration changed (each one already
/// chained into the evidence source log) — never a silent overwrite.
pub(crate) fn print_source_changes(case_dir: &Path, row: &case_db::EvidenceSourceRow) {
    if row.changes.is_empty() {
        return;
    }
    println!(
        "source {} changed ({} field(s)):",
        row.source_id,
        row.changes.len()
    );
    for change in &row.changes {
        println!(
            "  {}: {} -> {}",
            change.field,
            change.old.as_deref().unwrap_or("(none)"),
            change.new.as_deref().unwrap_or("(none)")
        );
    }
    println!(
        "change audit log: {}",
        case_dir
            .join(case_db::EVIDENCE_SOURCE_LOG_REL_PATH)
            .display()
    );
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
    // R6-S1: both files are embedded into scripts of trusted pages; only
    // a JSON value ever gets there (re-serialized), anything else is null.
    let index_json = page_json_input("db/video_index.json", &index_json);
    let manifest_json = page_json_input("case.json", &manifest_json);
    let redactor = crate::redact::Redactor::new(case_dir);
    let redact = |text: &str| -> String {
        if redact_paths {
            redactor.json_text(text)
        } else {
            text.to_string()
        }
    };
    // I4: in redacted mode the generated pages never embed the real case
    // directory either (the viewer's `caseDir`, the carve report's root);
    // a placeholder root keeps relative artifact paths readable.
    let page_case_dir: &Path = if redact_paths {
        Path::new("<case>")
    } else {
        case_dir
    };
    // Thumbnails and proxies are built from the real paths; only what the
    // pages embed is redacted.
    let videos = collect_index_videos(&index_json);
    let manifest_json = redact(&manifest_json);
    let index_json = redact(&index_json);
    let html = html_report::render_review_html(&manifest_json, &index_json);
    let review_path = case_dir.join("review/index.html");
    let review_html = inject_redaction_banner(html, redact_paths);
    write_text(&review_path, &review_html)
        .map_err(|err| format!("failed to write review html: {err}"))?;
    // R9 P3 (contract with the viewer, track U): every log entry the
    // bundle embeds is read through `audit::read_lane_rows` and carries its
    // `trust` code ("auth" | "sc" | "unauth" | "pc:<key id>"); proxies are
    // only pre-built for entries usable here.
    let carve_lane = review_lane(case_dir, "artifacts/carved/carve-log.jsonl");
    let filesystem_lane = review_lane(case_dir, "evidence/logs/tsk-audit.jsonl");
    let carve_log_raw = carve_lane.0;
    let filesystem_log_raw = filesystem_lane.0;
    let carve_log = redact(&carve_log_raw);
    let filesystem_log = redact(&filesystem_log_raw);
    let validation_log = redact(&review_lane(case_dir, "evidence/logs/validation-log.jsonl").0);
    // Prefer an existing anomaly log; scan on demand so make-review stays usable
    // without forcing a full-case digest pass (make-report / qa anomalies refresh).
    // R19 I2: rows and `withheld` verdict from one read of the lane.
    let (anomaly_log_raw, anomaly_withheld, anomaly_head) = review_anomaly_lane(case_dir);
    let anomaly_log = redact(&anomaly_log_raw);
    let fls_entries = redact(&latest_fls_entries_jsonl(case_dir));
    // Optional proxy pre-build: browser-unplayable containers (AVI, DAV,
    // proprietary recorder exports) can't decode in the review page at all,
    // so an examiner who needs the standalone bundle to just play asks for
    // proxies up front instead of per-file through the workstation button.
    if build_proxies {
        build_unplayable_proxies(case_dir, &carve_lane.1, &filesystem_lane.1)?;
    }
    let (thumbs_json, thumb_stats) = generate_review_thumbnails(case_dir, &videos)?;
    // I4: notes, tags and reports can quote paths mid-string; redacted too.
    let annotations_json = redact(&crate::case_db::review_annotations_json(case_dir)?);
    let deepfake_reports = redact(&crate::deepfake::collect_reports(case_dir).to_string());
    let telemetry_reports = redact(&crate::telemetry::collect_reports(case_dir).to_string());
    // Proxies already on disk are embedded so the viewer (including the
    // standalone file:// bundle) can prefer them for unplayable originals.
    let proxies_json = redact(&collect_proxies_json(case_dir));
    // Court-exhibit data additions (plan §1.9/§2.4): the join audit lane and
    // the compact measured-sound map, both redacted/bound like the others.
    // U2: each entry carries the joined record's `record_id` (the id the
    // viewer files marks/tags under), also for entries logged before it.
    let join_log = redact(&crate::join::join_log_with_record_ids(
        &review_lane(case_dir, "artifacts/joined/join-log.jsonl").0,
    ));
    let audio_levels_json = measured_audio_levels_json(case_dir)?;
    // R17 I6: the standalone viewer shows the same anomaly-head banner as
    // the workstation — `logChains.anomaly.head`, the very object
    // `/api/records-meta` and the report's `auditChains.anomaly.head`
    // carry (as of this make-review; redacted like the rest).
    // R18 I2: plus the lane's own P9 verdict (`withheld`); I6: a redacted
    // text that no longer parses becomes an explicit "bundle-unreadable".
    let log_chains = crate::report::bundle_log_chains_json(&redact(
        &serde_json::json!({
            "anomaly": {
                "head": anomaly_head,
                "withheld": anomaly_withheld,
            },
        })
        .to_string(),
    ));
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
        &join_log,
        &audio_levels_json,
        &log_chains,
        page_case_dir,
    );
    // D12: a redacted bundle says so on its first line, so package-case can
    // tell it from an unredacted one like it does the HTML pages.
    let data_bundle = if redact_paths {
        crate::redact::mark_bundle_redacted(&data_bundle)
    } else {
        data_bundle
    };
    write_text(&case_dir.join("review/data-bundle.js"), &data_bundle)
        .map_err(|err| format!("failed to write review data bundle: {err}"))?;
    let evidence_viewer = html_report::render_evidence_viewer_html_slim();
    let evidence_viewer_path = case_dir.join("review/evidence-viewer.html");
    let evidence_viewer = inject_redaction_banner(evidence_viewer, redact_paths);
    write_text(&evidence_viewer_path, &evidence_viewer)
        .map_err(|err| format!("failed to write evidence viewer html: {err}"))?;
    // Dedicated carve view: a raw-image carve can return hundreds of
    // artifacts that would drown the evidence grid, so carving results get
    // their own page beside the viewer.
    // R5-S1: exactly the files this run wrote are recorded as generated.
    // R6-S7: by the bytes written, not by re-reading the files.
    let mut written: Vec<(&str, &[u8])> = vec![
        ("review/index.html", review_html.as_bytes()),
        ("review/data-bundle.js", data_bundle.as_bytes()),
        ("review/evidence-viewer.html", evidence_viewer.as_bytes()),
    ];
    let carve_report_page: String;
    if !carve_log.trim().is_empty() {
        let carve_results =
            redact(&read_to_string(&case_dir.join("db/carve_results.json")).unwrap_or_default());
        // Redacted in-case outputs read `<case>/…`; handed to the carve page
        // as case-relative paths they resolve against the placeholder root
        // instead of being re-joined onto it.
        let carve_log_for_page = if redact_paths {
            strip_case_placeholder(&carve_log)
        } else {
            carve_log.clone()
        };
        let carve_report = html_report::render_carve_report_html(
            &manifest_json,
            &carve_log_for_page,
            &carve_results,
            &proxies_json,
            page_case_dir,
            &index_json,
        );
        let carve_report_path = case_dir.join("review/carve-report.html");
        carve_report_page = inject_redaction_banner(carve_report, redact_paths);
        write_text(&carve_report_path, &carve_report_page)
            .map_err(|err| format!("failed to write carve report html: {err}"))?;
        written.push(("review/carve-report.html", carve_report_page.as_bytes()));
        println!("carve report written: {}", carve_report_path.display());
    }
    record_generated_contents(case_dir, &written)?;
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

/// R6-S1: an input file's text as compact JSON for a generated page's
/// script, or `"null"` with a printed warning when it is not exactly one
/// JSON value (a tampered `case.json` can no longer inject script).
/// R7-S6: run by the workstation, the warning becomes a job warning (the
/// pages also say the file could not be read instead of going blank).
fn page_json_input(label: &str, text: &str) -> String {
    match crate::util::script_json_input(label, text) {
        Ok(json) => json,
        Err(warning) => {
            crate::util::print_step_warning(&warning);
            "null".to_string()
        }
    }
}

/// Rewrites redacted `<case>/rel` output paths of a JSONL log back to the
/// case-relative `rel` (I4, carve page in redacted mode). Other values and
/// unparseable lines pass through.
fn strip_case_placeholder(jsonl: &str) -> String {
    jsonl
        .lines()
        .map(|line| {
            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) else {
                return line.to_string();
            };
            if let Some(rel) = value
                .get("output_path")
                .and_then(|v| v.as_str())
                .and_then(|p| p.strip_prefix("<case>/"))
                .map(str::to_string)
            {
                value["output_path"] = serde_json::Value::String(rel);
            }
            value.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Inserts the redaction notice right after `<body>` so generated pages that
/// do not take an explicit flag still visibly mark themselves as redacted.
fn inject_redaction_banner(html: String, redact_paths: bool) -> String {
    if !redact_paths {
        return html;
    }
    let banner = format!(
        "{}<div style=\"padding:8px 16px;background:#fff7ed;border-bottom:1px solid #f4c790;font-size:13px\">{}</div>",
        crate::redact::REDACTION_MARKER,
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

/// Version of the content a finalize sign-off binds to. v3 adds each
/// mark's IN-OUT range and the tag map to v2's id/status/time/examiner/
/// note; a sign-off with an older `schema_version` cannot say whether
/// ranges or tags changed after it.
pub const SIGNOFF_CONTENT_SCHEMA: u64 = 3;

/// The content a finalize sign-off binds to ([`report::signoff_content_sha256`]):
/// every review mark with its range, plus the tag map, in a stable order.
/// Finalize now builds the record with `report::Signoff::build_v4` (which
/// uses [`signoff_content_json_from`]); tests pin the bytes through this.
#[cfg(test)]
pub(crate) fn signoff_content_json(case_dir: &Path) -> Result<String, String> {
    let rows = crate::case_db::load_review_marks(case_dir)
        .map_err(|err| format!("판독 마크를 읽지 못했습니다: {err}"))?;
    let tags = crate::case_db::load_review_tags(case_dir)
        .map_err(|err| format!("판독 태그를 읽지 못했습니다: {err}"))?;
    Ok(signoff_content_json_from(&rows, &tags))
}

/// [`signoff_content_json`] over already-loaded rows (verify-package reads
/// them from the packaged database). Marks are ordered by id, the tag map
/// by id with each tag list sorted and de-duplicated, so the digest only
/// changes when the content does.
pub(crate) fn signoff_content_json_from(
    rows: &[case_db::ReviewMarkRow],
    tags: &[crate::selection::TagEntry],
) -> String {
    let number = |value: Option<f64>| {
        value
            .and_then(|v| serde_json::Number::from_f64(v).map(|n| n.to_string()))
            .unwrap_or_else(|| "null".to_string())
    };
    let mut sorted: Vec<&case_db::ReviewMarkRow> = rows.iter().collect();
    sorted.sort_by(|a, b| a.record_id.cmp(&b.record_id));
    let marks = sorted
        .iter()
        .map(|mark| {
            let (range_in, range_out) = mark.range.map_or((None, None), |r| (r.start, r.end));
            format!(
                "{{\"id\":\"{}\",\"status\":\"{}\",\"marked_unix\":{},\"examiner\":{},\"note\":{},\"range_in\":{},\"range_out\":{}}}",
                crate::util::json_escape(&mark.record_id),
                crate::util::json_escape(&mark.status),
                mark.marked_unix,
                mark.examiner
                    .as_deref()
                    .map(|v| format!("\"{}\"", crate::util::json_escape(v)))
                    .unwrap_or_else(|| "null".to_string()),
                mark.note
                    .as_deref()
                    .map(|v| format!("\"{}\"", crate::util::json_escape(v)))
                    .unwrap_or_else(|| "null".to_string()),
                number(range_in),
                number(range_out),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut tag_map: std::collections::BTreeMap<&str, std::collections::BTreeSet<&str>> =
        std::collections::BTreeMap::new();
    for entry in tags {
        tag_map
            .entry(entry.id.as_str())
            .or_default()
            .extend(entry.tags.iter().map(String::as_str));
    }
    let tags = tag_map
        .iter()
        .filter(|(_, values)| !values.is_empty())
        .map(|(id, values)| {
            format!(
                "\"{}\":[{}]",
                crate::util::json_escape(id),
                values
                    .iter()
                    .map(|v| format!("\"{}\"", crate::util::json_escape(v)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"schema\":{SIGNOFF_CONTENT_SCHEMA},\"marks\":[{marks}],\"tags\":{{{tags}}}}}")
}

/// The review annotations the court report displays (I3): every mark with
/// its IN-OUT range, plus the tag map (tags of records without a mark
/// included). Shape:
/// `{"marks":[{"id","status","marked_unix","examiner","note","range":{"in","out"}|null}],"tags":{"id":["tag",...]}}`.
/// Marks keep the stored (id) order; each tag list is sorted and
/// de-duplicated, ids without tags are omitted. Display only — sign-off
/// digests use [`signoff_content_json_from`] / [`review_marks_json_from_rows`].
pub(crate) fn report_annotations_json(
    rows: &[case_db::ReviewMarkRow],
    tags: &[crate::selection::TagEntry],
) -> String {
    let finite = |value: Option<f64>| value.filter(|v| v.is_finite());
    let marks: Vec<serde_json::Value> = rows
        .iter()
        .map(|mark| {
            let range = mark.range.and_then(|span| {
                let (start, end) = (finite(span.start), finite(span.end));
                (start.is_some() || end.is_some())
                    .then(|| serde_json::json!({"in": start, "out": end}))
            });
            serde_json::json!({
                "id": mark.record_id,
                "status": mark.status,
                "marked_unix": mark.marked_unix,
                "examiner": mark.examiner,
                "note": mark.note,
                "range": range,
            })
        })
        .collect();
    let mut tag_map: std::collections::BTreeMap<&str, std::collections::BTreeSet<&str>> =
        std::collections::BTreeMap::new();
    for entry in tags {
        tag_map
            .entry(entry.id.as_str())
            .or_default()
            .extend(entry.tags.iter().map(String::as_str));
    }
    let tag_object: serde_json::Map<String, serde_json::Value> = tag_map
        .into_iter()
        .filter(|(_, values)| !values.is_empty())
        .map(|(id, values)| (id.to_string(), serde_json::json!(values)))
        .collect();
    serde_json::json!({"marks": marks, "tags": tag_object}).to_string()
}

/// The marks JSON that make-report embedded before I3 (status / time /
/// examiner / note per mark), so make-report and verify-package can
/// recompute the legacy sign-off digest. A DB read failure upstream is an
/// error, never an empty list (finding 8).
///
/// LEGACY DIGEST INPUT — keep byte-identical. Pre-v3 (schema 2) sign-offs
/// recorded `sha256` of exactly this string; any change (field order,
/// added range/tags) would flip every old sign-off to "changed". The report
/// no longer displays it — see [`report_annotations_json`].
pub(crate) fn review_marks_json_from_rows(rows: &[case_db::ReviewMarkRow]) -> String {
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
                    .map(|name| format!(",\"examiner\":\"{}\"", crate::util::json_escape(name)))
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
    if anomaly_scan.index.limited() {
        eprintln!(
            "warning: anomaly scan{}",
            anomaly_scan.index.note().trim_start_matches(';')
        );
    }
    let manifest_path = case_dir.join("case.json");
    let manifest_json = read_to_string(&manifest_path)
        .map_err(|err| format!("failed to read {}: {err}", manifest_path.display()))?;
    // R6-S1: embedded into the report's script — JSON values only.
    let index_json = page_json_input("db/video_index.json", &index_json);
    let manifest_json = page_json_input("case.json", &manifest_json);
    // D11: a log that exists but cannot be read is named in the report
    // (and on stderr) — never rendered as an empty "none" section.
    let mut read_errors: std::collections::BTreeMap<String, String> = Default::default();
    // R9 P3: every quoted lane is read through `audit::read_lane_rows`;
    // each entry carries its `trust` code and the report shows only
    // "auth"/"sc" entries as current results (the rest separately, as
    // "인증 안 됨").
    let mut lane = |name: &str, rel: &str| -> String {
        match crate::audit::read_lane_rows(&case_dir.join(rel)) {
            Ok(lane) => lane.jsonl_with_trust(),
            Err(err) => {
                eprintln!("warning: {rel} could not be read: {err}");
                read_errors.insert(name.to_string(), format!("{rel}: {err}"));
                String::new()
            }
        }
    };
    let export_log = lane("export", "artifacts/clips/export-log.jsonl");
    let proxy_log = lane("proxy", "artifacts/proxies/proxy-log.jsonl");
    let thumbnail_log = lane("thumbnail", "artifacts/thumbnails/thumbnail-log.jsonl");
    let carve_log = lane("carve", "artifacts/carved/carve-log.jsonl");
    let filesystem_log = lane("filesystem", "evidence/logs/tsk-audit.jsonl");
    let validation_log = lane("validation", "evidence/logs/validation-log.jsonl");
    let anomaly_log = lane("anomaly", "evidence/logs/anomaly-log.jsonl");
    let batch_log = lane("batch", "artifacts/logs/batch-log.jsonl");
    // D3/D18: join entries carry the measured length from their manifest
    // and say whether their record id is the log's own or derived.
    let join_log = report_join_log(case_dir, &lane("join", "artifacts/joined/join-log.jsonl"));
    // Measured-sound map bound to the current index bytes (plan §2.4); the
    // map carries ids/numbers only, so it needs no redaction. D1: a log
    // whose chain fails yields `unverified` states, never levels; D11: a
    // read failure is named instead of aborting or claiming "unmeasured".
    let audio_levels_json = match measured_audio_levels_json(case_dir) {
        Ok(json) => json,
        Err(err) => {
            eprintln!("warning: sound measurements could not be read: {err}");
            read_errors.insert("audio".to_string(), err);
            "{}".to_string()
        }
    };
    let scan_runs_json = match read_scan_runs_json_checked(case_dir) {
        Ok(json) => json,
        Err(err) => {
            eprintln!("warning: {err}");
            read_errors.insert("scan_runs".to_string(), err);
            "[]".to_string()
        }
    };
    // D9: marks still queued or save conflicts still open mean the report
    // may not show the latest review — said on stderr and in the report.
    let review_warnings = review_state_warnings(case_dir);
    for warning in &review_warnings {
        eprintln!("warning: {warning}");
    }
    let review_warnings_json = serde_json::to_string(&review_warnings).unwrap_or_default();
    let read_errors_json = serde_json::to_string(&read_errors).unwrap_or_default();
    let telemetry_json = report_telemetry_json(case_dir);
    // One read of marks and tags feeds the displayed annotations and both
    // sign-off digests, so the banner judges exactly what the table shows.
    let mark_rows = crate::case_db::load_review_marks(case_dir)
        .map_err(|err| format!("판독 마크를 읽지 못했습니다: {err}"))?;
    let tag_rows = crate::case_db::load_review_tags(case_dir)
        .map_err(|err| format!("판독 태그를 읽지 못했습니다: {err}"))?;
    let signoff_digests = report::SignoffDigests {
        content: report::signoff_content_sha256(&signoff_content_json_from(&mark_rows, &tag_rows)),
        legacy: report::signoff_content_sha256(&review_marks_json_from_rows(&mark_rows)),
    };
    let annotations_json = report_annotations_json(&mark_rows, &tag_rows);
    let redactor = crate::redact::Redactor::new(case_dir);
    let redact = |text: String| -> String {
        if redact_paths {
            redactor.json_text(&text)
        } else {
            text
        }
    };
    // I9: re-verify every audit log the report quotes, so its chain columns
    // state what was checked instead of implying tamper-evidence.
    let audit_chains_json = redact(report_audit_chains_json(case_dir));
    let manifest_json = redact(if redact_paths {
        redact_manifest_host(&manifest_json)
    } else {
        manifest_json
    });
    let index_json = redact(index_json);
    let export_log = redact(export_log);
    let proxy_log = redact(proxy_log);
    let thumbnail_log = redact(thumbnail_log);
    let carve_log = redact(carve_log);
    let filesystem_log = redact(filesystem_log);
    let validation_log = redact(validation_log);
    let anomaly_log = redact(anomaly_log);
    let batch_log = redact(batch_log);
    let join_log = redact(join_log);
    let scan_runs_json = redact(scan_runs_json);
    let annotations_json = redact(annotations_json);
    let review_warnings_json = redact(review_warnings_json);
    let read_errors_json = redact(read_errors_json);
    let telemetry_json = redact(telemetry_json);
    let html = report::render_case_report_with_chains(
        &report::ReportInputs {
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
            join_log_jsonl: &join_log,
            audio_levels_json: &audio_levels_json,
            scan_runs_json: &scan_runs_json,
            marks_json: &annotations_json,
            redaction_applied: redact_paths,
            review_warnings_json: &review_warnings_json,
            read_errors_json: &read_errors_json,
            telemetry_json: &telemetry_json,
        },
        &audit_chains_json,
    );
    // I10: a pending record left by an interrupted finalize is never read
    // as the sign-off; say it is there instead of ignoring it silently.
    if signoff_path.is_none() && case_dir.join(package::SIGNOFF_PENDING_REL_PATH).is_file() {
        eprintln!(
            "warning: {} is left over from an interrupted finalize and was ignored; the report uses {} (finalize again, or remove the pending file)",
            package::SIGNOFF_PENDING_REL_PATH,
            report::SIGNOFF_REL_PATH
        );
    }
    // Finalize sign-off: the reviewer name recorded by the workstation (as
    // typed - not an authenticated identity) is printed and shown in the report.
    // `signoff_path` is the workstation's not-yet-promoted record of an
    // in-progress finalize; otherwise the promoted review/signoff.json.
    let signoff_file = signoff_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| case_dir.join(report::SIGNOFF_REL_PATH));
    let signoff_text = match read_optional_text(&signoff_file) {
        Ok(text) if text.is_empty() => None,
        Ok(text) => Some(text),
        Err(err) => {
            // D11: an unreadable sign-off is not "no sign-off".
            return Err(format!(
                "검토자 기록 {}을(를) 읽지 못했습니다: {err}",
                signoff_file.display()
            ));
        }
    };
    let html = match signoff_text.as_deref().and_then(report::Signoff::parse) {
        Some(signoff) => {
            // D13: a v4 record is checked against the evidence hashes and
            // the audit-lane anchors too; older records keep their legacy
            // binding meaning.
            // R5-P2: with --rehash (finalize passes it) the signed target
            // files are hashed again; the originals reuse this run's
            // anomaly re-hash instead of being read twice.
            let live = rehash.then(|| {
                let reuse: std::collections::BTreeMap<String, report::LiveHash> = anomaly_scan
                    .checks
                    .iter()
                    .map(|check| {
                        let live = match (check.status, &check.observed) {
                            ("match" | "mismatch", Some(observed)) => {
                                report::LiveHash::Hashed(observed.clone())
                            }
                            (status, _) => {
                                report::LiveHash::Unavailable(format!("원본 재해시 결과: {status}"))
                            }
                        };
                        (check.id.clone(), live)
                    })
                    .collect();
                report::rehash_signed_targets(case_dir, &signoff.text, &reuse)
            });
            let check = report::check_signoff_with(
                case_dir,
                &signoff.text,
                &signoff_digests,
                report::SignoffScope::Case,
                live.as_ref(),
            );
            let audit_note = report::signoff_audit_record(case_dir, &signoff.text, true).note();
            println!(
                "검토자: {} (최종화 시 입력된 이름, 신원 미인증) — {}",
                signoff.examiner,
                check.currency.label()
            );
            for change in &check.changes {
                println!("  변경: {change}");
            }
            for item in &check.not_verified {
                println!("  확인 불가: {item}");
            }
            println!("  {audit_note}");
            let audit_note = if redact_paths {
                redactor.text(&audit_note)
            } else {
                audit_note
            };
            let check = if redact_paths {
                report::SignoffCheck {
                    currency: check.currency,
                    changes: check.changes.iter().map(|c| redactor.text(c)).collect(),
                    not_verified: check
                        .not_verified
                        .iter()
                        .map(|c| redactor.text(c))
                        .collect(),
                    rehashed: check.rehashed,
                    authenticated: check.authenticated,
                }
            } else {
                check
            };
            report::inject_signoff_banner_checked(html, &signoff, &check, &audit_note)
        }
        None => html,
    };
    let report_path = case_dir.join("reports/case-report.html");
    write_text(&report_path, &html).map_err(|err| format!("failed to write report html: {err}"))?;
    record_generated_contents(case_dir, &[("reports/case-report.html", html.as_bytes())])?;
    println!("report written: {}", report_path.display());
    Ok(())
}

/// R5-P6: a redacted report shows the case host as the same
/// `<redacted-host:…>` token the redacted custody statement uses (a host
/// name is not a path, so the path redactor never touched it). A manifest
/// that does not parse is returned unchanged (the path redactor still
/// runs over it).
fn redact_manifest_host(manifest_json: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(manifest_json) else {
        return manifest_json.to_string();
    };
    if let Some(host) = value
        .get("host")
        .and_then(|v| v.as_str())
        .filter(|h| !h.is_empty())
        .map(crate::redact::host_token)
    {
        value["host"] = serde_json::Value::String(host);
    }
    value.to_string()
}

/// The audit logs the case report quotes, by the report's lane name. D1:
/// the sound-measurement lane is verified like the others; D13: so is the
/// sign-off lane.
const REPORT_AUDIT_LOGS: &[(&str, &str)] = &[
    ("export", "artifacts/clips/export-log.jsonl"),
    ("proxy", "artifacts/proxies/proxy-log.jsonl"),
    ("thumbnail", "artifacts/thumbnails/thumbnail-log.jsonl"),
    ("carve", "artifacts/carved/carve-log.jsonl"),
    ("filesystem", "evidence/logs/tsk-audit.jsonl"),
    ("validation", "evidence/logs/validation-log.jsonl"),
    ("anomaly", "evidence/logs/anomaly-log.jsonl"),
    ("batch", "artifacts/logs/batch-log.jsonl"),
    ("join", "artifacts/joined/join-log.jsonl"),
    ("audio", crate::audio_level::LOG_REL),
    ("signoff", report::SIGNOFF_LOG_REL_PATH),
];

/// R9 P3: a lane for the review bundle — every entry with its `trust`
/// code (JSONL), and only the entries usable here (JSONL). Unreadable =
/// empty (the bundle has no error slot; make-report names read errors).
fn review_lane(case_dir: &Path, rel: &str) -> (String, String) {
    review_lane_of(rel, &crate::audit::read_lane_rows(&case_dir.join(rel)))
}

/// R19 I2: the review bundle's anomaly lane (every entry with its `trust`,
/// JSONL) and its `logChains.anomaly.withheld` verdict, both from ONE read
/// of the lane (a scan between two reads made them disagree; an unreadable
/// lane is `unreadable` without a second read). R20 I1: and its
/// `logChains.anomaly.head` (`to_json_in(Anomaly)`), from that read too.
fn review_anomaly_lane(case_dir: &Path) -> (String, serde_json::Value, serde_json::Value) {
    let rel = crate::report::ANOMALY_LOG_REL_PATH;
    let lane = crate::audit::read_lane_rows(&case_dir.join(rel));
    let withheld = crate::report::anomaly_lane_read_withheld_json(&lane);
    let head = crate::report::anomaly_head_check(case_dir, &lane)
        .to_json_in(crate::report::HeadLane::Anomaly);
    (review_lane_of(rel, &lane).0, withheld, head)
}

/// [`review_lane`] over a lane already read.
fn review_lane_of(rel: &str, lane: &Result<crate::audit::LaneRows, String>) -> (String, String) {
    match lane {
        Ok(lane) => {
            let usable = lane
                .usable()
                .map(|row| format!("{}\n", row.value_with_trust()))
                .collect();
            (lane.jsonl_with_trust(), usable)
        }
        Err(err) => {
            eprintln!("warning: {rel} could not be read: {err}");
            (String::new(), String::new())
        }
    }
}

/// The text of an optional file: `""` when it does not exist, an error
/// when it exists but cannot be read (D11).
fn read_optional_text(path: &Path) -> Result<String, String> {
    match read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err.to_string()),
    }
}

/// D3/D18: the join log as the report shows it. Every entry carries a
/// `record_id`; one the log line did not have is derived from the output
/// hash and flagged `record_id_derived` (only such legacy entries may fall
/// back to `join:<file name>` annotations). An entry without a recorded
/// observed length gets its manifest's `duration_check` (expected /
/// observed / ok) when the manifest still has the SHA-256 the log
/// recorded. Unparseable lines are kept verbatim.
pub(crate) fn report_join_log(case_dir: &Path, jsonl: &str) -> String {
    jsonl
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) else {
                return line.to_string();
            };
            let Some(map) = value.as_object_mut() else {
                return line.to_string();
            };
            if map.get("record_id").and_then(|v| v.as_str()).is_none()
                && let Some(id) = map
                    .get("output_sha256")
                    .and_then(|v| v.as_str())
                    .and_then(crate::join::joined_record_id)
            {
                map.insert("record_id".into(), serde_json::Value::String(id));
                map.insert("record_id_derived".into(), serde_json::Value::Bool(true));
            }
            if (map.get("duration_check").is_none() || map.get("boundary_check").is_none())
                && let (Some(manifest_path), Some(manifest_sha)) = (
                    map.get("manifest_path").and_then(|v| v.as_str()),
                    map.get("manifest_sha256").and_then(|v| v.as_str()),
                )
            {
                let path = crate::util::resolve_case_artifact_path(case_dir, manifest_path);
                if let Ok(text) = read_to_string(&path)
                    && crate::sha256::digest_bytes(text.as_bytes()) == manifest_sha
                    && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text)
                {
                    if let Some(check) = manifest.get("duration_check")
                        && map.get("duration_check").is_none()
                    {
                        map.insert("duration_check".into(), check.clone());
                    }
                    // C14: the packet / segment-boundary verification.
                    if let Some(check) = manifest.get("segment_check")
                        && map.get("boundary_check").is_none()
                    {
                        map.insert("boundary_check".into(), segment_check_summary(check));
                    }
                }
            }
            value.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// D3: a join manifest's `segment_check` (C14) as the report's
/// `boundary_check`: `{ok, detail}` with the packet counts, the boundaries
/// located, and any problem the check recorded.
fn segment_check_summary(check: &serde_json::Value) -> serde_json::Value {
    let num = |key: &str| check.get(key).and_then(|v| v.as_u64());
    let mut parts = Vec::new();
    if let (Some(expected), Some(observed)) =
        (num("video_packets_expected"), num("video_packets_observed"))
    {
        parts.push(format!("영상 패킷 {observed}/{expected}"));
    }
    if let (Some(expected), Some(observed)) =
        (num("audio_packets_expected"), num("audio_packets_observed"))
    {
        parts.push(format!("소리 패킷 {observed}/{expected}"));
    }
    if let Some(boundaries) = check.get("boundaries").and_then(|v| v.as_array()) {
        let located = boundaries
            .iter()
            .filter(|b| !b["observed_start_seconds"].is_null())
            .count();
        parts.push(format!("경계 {located}/{}개 확인", boundaries.len()));
    }
    if let Some(problems) = check.get("problems").and_then(|v| v.as_array()) {
        parts.extend(
            problems
                .iter()
                .filter_map(|p| p.as_str())
                .map(str::to_string),
        );
    }
    serde_json::json!({
        "ok": check.get("ok").and_then(|v| v.as_bool()),
        "detail": parts.join("; "),
    })
}

/// The telemetry extracts the report lists (E9): every
/// `artifacts/telemetry/*.json` extract without its point array, keeping
/// `point_count` / `total_points` / `truncated` so a capped track is
/// never presented as complete.
pub(crate) fn report_telemetry_json(case_dir: &Path) -> String {
    let reports = crate::telemetry::collect_reports(case_dir);
    let mut out = serde_json::Map::new();
    if let Some(map) = reports.as_object() {
        for (name, report) in map {
            let mut summary = serde_json::Map::new();
            for key in [
                "selector",
                "source_path",
                "source",
                "point_count",
                "total_points",
                "truncated",
                "first_ts_unix",
                "last_ts_unix",
                "max_speed_kmh",
                "note",
            ] {
                if let Some(value) = report.get(key) {
                    summary.insert(key.to_string(), value.clone());
                }
            }
            out.insert(name.clone(), serde_json::Value::Object(summary));
        }
    }
    serde_json::Value::Object(out).to_string()
}

/// D9: marks accepted but not yet applied (`marks-pending/*.json`) and
/// unresolved save conflicts (`marks-pending/conflicts.jsonl`: ids whose
/// last journal line is a conflict, not a `resolved` line).
pub(crate) fn review_queue_state(case_dir: &Path) -> (usize, usize) {
    let dir = case_dir.join("marks-pending");
    let queued = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
                })
                .count()
        })
        .unwrap_or(0);
    let mut open: Vec<String> = Vec::new();
    for line in read_to_string(&dir.join("conflicts.jsonl"))
        .unwrap_or_default()
        .lines()
    {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(id) = value.get("resolved").and_then(|v| v.as_str()) {
            open.retain(|open_id| open_id != id);
        } else if let Some(id) = value.get("id").and_then(|v| v.as_str())
            && !open.iter().any(|open_id| open_id == id)
        {
            open.push(id.to_string());
        }
    }
    (queued, open.len())
}

/// D9: the review-state warnings make-report prints and shows.
fn review_state_warnings(case_dir: &Path) -> Vec<String> {
    let (queued, conflicts) = review_queue_state(case_dir);
    let mut out = Vec::new();
    if queued > 0 {
        out.push(format!(
            "저장 대기열에 아직 반영되지 않은 판독 마크 묶음 {queued}건이 있습니다 — 이 보고서에는 그 내용이 빠져 있습니다 (워크스테이션에서 반영 후 보고서를 다시 만드십시오)."
        ));
    }
    if conflicts > 0 {
        out.push(format!(
            "해결되지 않은 판독 저장 충돌 {conflicts}건이 있습니다 — 해당 기록은 어느 쪽 값도 확정되지 않았습니다 (뷰어의 충돌 목록에서 해결하십시오)."
        ));
    }
    out
}

/// I9: chain verification of each quoted log, as the report's
/// `auditChains` map (`status`: keyed / structural / failed / absent).
pub(crate) fn report_audit_chains_json(case_dir: &Path) -> String {
    let mut map = serde_json::Map::new();
    for (name, rel) in REPORT_AUDIT_LOGS {
        if *name == "audio" {
            // R5-P8: every sound-measurement lane, damaged ones named.
            map.insert(
                (*name).to_string(),
                crate::audio_level::chain_summary_json(case_dir),
            );
            continue;
        }
        let path = case_dir.join(rel);
        // R8 P0/P1/P2: the lane's trust under THIS PC's policy (keys held
        // → any line not authenticated here is NOT VERIFIED, naming other
        // PCs' keys; no keys → self-consistency only). R9 P3/P5/I7: read
        // through the same `read_lane_rows` the sections use, so the
        // status and the cause-specific `detail` (chain break, key
        // configuration unreadable — not tampering —, unauthenticated
        // entries, legacy unbound entries) describe exactly those rows.
        let lane = crate::audit::read_lane_rows(&path);
        let mut entry = match &lane {
            Ok(lane) => lane.chain_json(rel),
            Err(err) => serde_json::json!({
                "path": rel,
                "status": "failed",
                "detail": format!("레인을 읽지 못함: {err}"),
            }),
        };
        // R15 P22 / I2: the sign-off lane's head against the anomaly lane
        // — the report's anomaly "현재 결과" is withheld when it is cut.
        if *name == "anomaly" {
            // R20 I1: the head from the same read as well.
            entry["head"] = crate::report::anomaly_head_check(case_dir, &lane)
                .to_json_in(crate::report::HeadLane::Anomaly);
            // R18 I2: the lane's own P9 verdict (one Rust function; the
            // report script withholds when it or its own rules say so).
            // R19 I2: from the same read as this lane's chain status.
            entry["withheld"] = crate::report::anomaly_lane_read_withheld_json(&lane);
        }
        map.insert((*name).to_string(), entry);
    }
    serde_json::Value::Object(map).to_string()
}

pub fn make_custody(case_dir: &Path) -> Result<(), String> {
    ensure_case(case_dir)?;
    let path = write_custody_statement(case_dir)?;
    println!("custody statement written: {}", path.display());
    Ok(())
}

/// Renders and writes the custody statement and records the bytes written
/// in the generated-pages list (R6-S7: rendered and written here, as
/// `custody::generate` does, so the bytes written are the bytes
/// recorded). R7-S8: `package-case` refreshes the statement through this
/// too — it used `custody::generate`, which rewrote the page without
/// recording it, so the workstation served the fresh statement sandboxed.
fn write_custody_statement(case_dir: &Path) -> Result<std::path::PathBuf, String> {
    let html = crate::custody::render_statement(case_dir, false)?;
    let path = case_dir.join(crate::custody::CUSTODY_REL_PATH);
    write_text(&path, &html).map_err(|err| {
        format!(
            "failed to write custody statement {}: {err}",
            path.display()
        )
    })?;
    record_generated_contents(
        case_dir,
        &[(crate::custody::CUSTODY_REL_PATH, html.as_bytes())],
    )?;
    Ok(path)
}

/// B5: records the SHA-256 of the generated case pages (review/, reports/)
/// so the workstation serves them with their scripts allowed instead of
/// sandboxed. R5-S1: only the files the calling generator itself just
/// wrote (`rels`, case-relative) — never every HTML in the folders, which
/// let a planted page ride along with the next regeneration. R5-S2: the
/// scripts those pages load (`review/data-bundle.js`) are listed too.
fn record_generated_contents(case_dir: &Path, files: &[(&str, &[u8])]) -> Result<(), String> {
    crate::util::generated_pages::record_generated_contents(case_dir, files)
        .map(|_| ())
        .map_err(|err| {
            format!("생성 페이지 신뢰 목록(review/.generated.json)을 기록하지 못했습니다: {err}")
        })
}

/// The compact `audioLevels` map (`{id: {r,max,mean,sec,t}}`, plan §2.4)
/// for the court viewer and the report, with T1's invalidation rule (a
/// measurement of other bytes is `stale`). `{}` when nothing was measured;
/// an unreadable log or index is an error, never an empty map.
fn measured_audio_levels_json(case_dir: &Path) -> Result<String, String> {
    crate::audio_level::audio_levels_for_case(case_dir)
        .map(|levels| crate::audio_level::audio_levels_json(&levels).to_string())
}

/// Reads every db/scan_runs/*.json snapshot and returns them joined into a
/// JSON array literal for the report script. D11: a missing directory is
/// "no runs"; an unreadable directory or snapshot is an error.
fn read_scan_runs_json_checked(case_dir: &Path) -> Result<String, String> {
    let runs_dir = case_dir.join("db/scan_runs");
    let entries = match std::fs::read_dir(&runs_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok("[]".to_string()),
        Err(err) => {
            return Err(format!(
                "스캔 실행 기록 {}을(를) 읽지 못했습니다: {err}",
                runs_dir.display()
            ));
        }
    };
    let mut runs: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let content = read_to_string(&path).map_err(|err| {
            format!(
                "스캔 실행 기록 {}을(를) 읽지 못했습니다: {err}",
                path.display()
            )
        })?;
        // A snapshot that is not JSON would break the report script.
        if serde_json::from_str::<serde_json::Value>(&content).is_err() {
            return Err(format!(
                "스캔 실행 기록 {}이(가) 올바른 JSON이 아닙니다",
                path.display()
            ));
        }
        let name = entry.file_name().to_string_lossy().to_string();
        runs.push((name, content));
    }
    runs.sort_by_key(|(name, _)| name.clone());
    let joined = runs
        .iter()
        .map(|(_, content)| content.trim())
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!("[{joined}]"))
}

/// `package-case` without `--allow-pending` (tests; the CLI dispatches to
/// [`package_case_with_pending`]).
#[cfg(test)]
pub fn package_case(case_dir: &Path, options: PackageOptions) -> Result<(), String> {
    package_case_with_pending(case_dir, options, false, None)
}

/// `package-case` (D9): refuses while marks are still queued or save
/// conflicts are unresolved — the package would seal a review that is not
/// the examiner's latest — unless `allow_pending` (`--allow-pending`) is
/// given, in which case the state is printed as a warning. R13-S2:
/// `package_name` (`--package-name`) publishes under exactly that
/// `reports/` name or fails.
pub fn package_case_with_pending(
    case_dir: &Path,
    options: PackageOptions,
    allow_pending: bool,
    package_name: Option<&str>,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let (queued, conflicts) = review_queue_state(case_dir);
    if queued > 0 || conflicts > 0 {
        let state = format!(
            "저장 대기 중인 판독 마크 묶음 {queued}건, 해결되지 않은 저장 충돌 {conflicts}건"
        );
        if !allow_pending {
            return Err(format!(
                "package-case refused: {state} — 반영·해결한 뒤 다시 패키징하거나, 그대로 봉인하려면 --allow-pending을 주십시오."
            ));
        }
        eprintln!("warning: packaging with {state} (--allow-pending)");
    }
    // A package should never ship without its custody statement; refresh it
    // best-effort so a stale statement can't be sealed into a checksum
    // manifest. Generation failure must not block packaging — the missing
    // file is itself disclosed by the package README.
    if let Err(err) = write_custody_statement(case_dir) {
        eprintln!("warning: custody statement not refreshed: {err}");
    }
    let result =
        package::package_case_named(case_dir, options.output_dir.as_deref(), package_name)?;
    println!("case package written");
    println!("output: {}", result.output_dir.display());
    println!("files: {}", result.file_count);
    println!("manifest: {}", result.manifest_path.display());
    if result.path_redacted {
        println!(
            "path redaction: report/review/custody/text documents redacted; database, index files and chained audit logs are NOT path-redacted (listed in README.txt)"
        );
    }
    for warning in &result.warnings {
        println!("warning: {warning}");
    }
    Ok(())
}

/// R11-S8: the exit code `verify-package --strict` uses for a package
/// whose files all check out but whose finalize did not complete
/// (`_INCOMPLETE` name or marker beside it). Without `--strict` such a
/// package exits 0 (P8: NOT VERIFIED is not FAIL); a FAIL is exit 1 either
/// way.
pub const VERIFY_PACKAGE_INCOMPLETE_EXIT_CODE: i32 = 3;

/// `verify-package [--strict]`: checks a package directory written by
/// package-case and prints every check with its status — PASS, FAIL
/// (naming the file) or NOT VERIFIED (what the package alone cannot
/// prove). A failed check is the `Err` (exit 1). Returns the process exit
/// code otherwise — 0, or
/// [`VERIFY_PACKAGE_INCOMPLETE_EXIT_CODE`] for an incomplete finalize when
/// `strict` (a failed check is still the `Err`, exit 1).
pub fn verify_package_with(package_dir: &Path, strict: bool) -> Result<i32, String> {
    let result = verify_package_extended(package_dir)?;
    let code = verify_package_exit_code(&result, strict);
    print_package_verification(package_dir, &result)?;
    Ok(code)
}

/// R11-S8: the exit code of a verification without a FAIL.
///
/// R12-S8: `--strict` also exits 3 when the SIGN-OFF is NOT VERIFIED —
/// the sign-off record (`review/signoff.json`: none in the package, or one
/// the package cannot check) or its audit-lane entry (`signoff-log`, e.g.
/// keyed on another PC or recorded by an older build). R11 exited 0 for
/// those: a script using `--strict` as "final and signed" accepted a
/// package whose sign-off nothing confirmed. Other NOT VERIFIED lines
/// (what no package alone can prove) do not change the code.
pub fn verify_package_exit_code(result: &package::PackageVerification, strict: bool) -> i32 {
    let signoff_not_verified = result.checks.iter().any(|check| {
        check.status == package::CheckStatus::NotVerified
            && (check.subject == crate::report::SIGNOFF_REL_PATH
                || check.subject == crate::report::SIGNOFF_LOG_REL_PATH)
    });
    // R13-S12: a sign-off whose lane entry matched only structurally (no
    // HMAC checked: keyless, or no key on this PC) is not "final and
    // signed" either — anyone with the case folder can recompute it.
    if strict
        && result.passed()
        && (result.finalize_incomplete || signoff_not_verified || result.signoff_unauthenticated)
    {
        VERIFY_PACKAGE_INCOMPLETE_EXIT_CODE
    } else {
        0
    }
}

fn print_package_verification(
    package_dir: &Path,
    result: &package::PackageVerification,
) -> Result<(), String> {
    println!("package: {}", package_dir.display());
    if let Some(hash) = &result.manifest_sha256 {
        println!("manifest.sha256 SHA-256: {hash}");
    }
    for check in &result.checks {
        println!(
            "[{}] {}: {}",
            check.status.label(),
            check.subject,
            check.detail
        );
    }
    let failures: Vec<&str> = result
        .failures()
        .map(|check| check.subject.as_str())
        .collect();
    if failures.is_empty() && result.finalize_incomplete {
        // R10-S2: the files may all check out, but this is not a final
        // result — never the plain "package verified" line.
        println!(
            "package files match manifest.sha256 ({} file(s)), but FINALIZE INCOMPLETE: this package is NOT a final result (see the [NOT VERIFIED] finalize line)",
            result.files_verified
        );
        Ok(())
    } else if failures.is_empty() {
        println!(
            "package verified: {} file(s) match manifest.sha256 (see NOT VERIFIED lines for what this does not establish)",
            result.files_verified
        );
        Ok(())
    } else {
        let mut names = failures.clone();
        names.dedup();
        Err(format!(
            "package verification FAILED: {} check(s) failed ({})",
            failures.len(),
            names.join(", ")
        ))
    }
}

/// The checks `verify-package` prints. Since the R4 integration the
/// track-D checks (D10 report marker and "no unlisted files" only without
/// an unlisted FAIL, D13 v4 bindings and the sign-off audit lane, D16
/// known chain lanes) live in [`package::verify_package`] itself, so
/// library callers get them too; this name is kept for the CLI and tests.
pub fn verify_package_extended(package_dir: &Path) -> Result<package::PackageVerification, String> {
    package::verify_package(package_dir)
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
        // D5: path-free proxy names, with pre-D5 names still recognised.
        if crate::artifacts::find_existing_proxy(case_dir, &path).is_some() {
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
                    let mut last_error = String::new();
                    for seek in ["5", "0"] {
                        // C6: bounded per-thumbnail ffmpeg run, written
                        // to a part file and published only when complete.
                        match crate::video_export::extract_review_thumbnail(
                            Path::new(&ffmpeg),
                            source,
                            &output,
                            seek,
                            crate::video_export::REVIEW_THUMB_TIMEOUT_SECS,
                        ) {
                            Ok(()) => {
                                created = true;
                                break;
                            }
                            Err(err) => last_error = err,
                        }
                    }
                    if !created {
                        // No silent failure: the viewer shows a placeholder,
                        // and the run says which record and why.
                        eprintln!("warning: thumbnail for {id} not created: {last_error}");
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

#[cfg(test)]
mod annotation_json_tests {
    use super::{report_annotations_json, review_marks_json_from_rows, signoff_content_json_from};
    use crate::case_db::{RangeSpan, ReviewMarkRow};
    use crate::selection::TagEntry;

    /// Rows as an older case database holds them: a mark with every
    /// optional field (incl. a range the legacy JSON never carried) and a
    /// bare one.
    fn fixture() -> (Vec<ReviewMarkRow>, Vec<TagEntry>) {
        (
            vec![
                ReviewMarkRow {
                    record_id: "vid_000001".into(),
                    status: "important".into(),
                    marked_unix: 1_700_000_000,
                    record_path: Some("C:\\ev\\a.mp4".into()),
                    examiner: Some("홍길동".into()),
                    note: Some("line \"q\"\n<b>".into()),
                    range: Some(RangeSpan {
                        start: Some(1.5),
                        end: Some(4.0),
                    }),
                },
                ReviewMarkRow {
                    record_id: "vid_000002".into(),
                    status: "reviewed".into(),
                    marked_unix: 5,
                    ..Default::default()
                },
            ],
            vec![
                TagEntry {
                    id: "vid_000001".into(),
                    tags: vec!["보류".into(), "사고".into(), "보류".into()],
                },
                TagEntry {
                    id: "vid_000009".into(),
                    tags: vec!["<태그>".into()],
                },
                TagEntry {
                    id: "vid_000010".into(),
                    tags: vec![],
                },
            ],
        )
    }

    /// I3 / review point 8: pre-v3 sign-offs digested exactly this string.
    /// Both the bytes and the digest are pinned; changing either flips every
    /// legacy sign-off to "changed".
    #[test]
    fn legacy_marks_json_and_digest_unchanged() {
        let (rows, tags) = fixture();
        let legacy = review_marks_json_from_rows(&rows);
        assert_eq!(
            legacy,
            r#"[{"id":"vid_000001","status":"important","marked_unix":1700000000,"examiner":"홍길동","note":"line \"q\"\n<b>"},{"id":"vid_000002","status":"reviewed","marked_unix":5}]"#
        );
        assert_eq!(
            crate::report::signoff_content_sha256(&legacy),
            "31655e66b0196d4d1fc9d6fb10a921479af9b4f9ce978b015107da26b04ce3f2"
        );
        // The report's display JSON is a different string and never feeds
        // either digest.
        let display = report_annotations_json(&rows, &tags);
        assert_ne!(display, legacy);
        assert_ne!(display, signoff_content_json_from(&rows, &tags));
    }

    #[test]
    fn report_annotations_json_carries_ranges_and_tag_map() {
        let (rows, tags) = fixture();
        let value: serde_json::Value =
            serde_json::from_str(&report_annotations_json(&rows, &tags)).unwrap();
        assert_eq!(
            value["marks"][0],
            serde_json::json!({
                "id": "vid_000001", "status": "important", "marked_unix": 1_700_000_000u64,
                "examiner": "홍길동", "note": "line \"q\"\n<b>",
                "range": {"in": 1.5, "out": 4.0}
            })
        );
        assert_eq!(value["marks"][1]["range"], serde_json::Value::Null);
        assert_eq!(value["marks"][1]["examiner"], serde_json::Value::Null);
        assert!(value["marks"][0].get("record_path").is_none());
        // Sorted, de-duplicated; tag-only ids included; empty lists dropped.
        assert_eq!(
            value["tags"],
            serde_json::json!({"vid_000001": ["보류", "사고"], "vid_000009": ["<태그>"]})
        );
        let one_sided = ReviewMarkRow {
            range: Some(RangeSpan {
                start: Some(2.0),
                end: None,
            }),
            ..rows[1].clone()
        };
        let value: serde_json::Value =
            serde_json::from_str(&report_annotations_json(&[one_sided], &[])).unwrap();
        assert_eq!(
            value["marks"][0]["range"],
            serde_json::json!({"in": 2.0, "out": null})
        );
        assert_eq!(value["tags"], serde_json::json!({}));
    }
}

#[cfg(test)]
mod report_output_tests {
    use super::{InitCaseOptions, init_case, make_report, make_review};
    use crate::model::ScanOptions;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A scanned case (one indexed file, no ffprobe/hash) under a fresh
    /// temp root; the source folder name is distinctive so a leak is easy
    /// to spot. Returns `(root, case_dir, source_dir)`.
    fn scanned_case(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "frametrace-report-out-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        let source_dir = root.join("SourceSecretDir");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
        init_case(&case_dir, &InitCaseOptions::default()).unwrap();
        crate::scan::scan_folder(
            &case_dir,
            &source_dir,
            &ScanOptions {
                hash_files: false,
                use_ffprobe: false,
                ..ScanOptions::default()
            },
            crate::checkpoint::ResumeMode::Auto,
            None,
        )
        .unwrap();
        (root, case_dir, source_dir)
    }

    fn json_spellings(path: &Path) -> [String; 2] {
        let text = path.to_string_lossy().to_string();
        let escaped = serde_json::to_string(&text).unwrap();
        [text, escaped.trim_matches('"').to_string()]
    }

    /// R6-S1: `case.json` and `db/video_index.json` were embedded into the
    /// scripts of the review pages and `review/data-bundle.js` as raw text
    /// (only `<>&` escaped), and the generated-pages list then vouched for
    /// those files — a crafted case ran script in the workstation origin.
    /// Now every embedded input is parsed and re-serialized; text that is
    /// not one JSON value becomes `null`.
    #[test]
    fn s1_crafted_case_json_and_index_never_reach_a_script() {
        let (root, case_dir, _source) = scanned_case("s1-inject");
        let payload = "globalThis.__ft_pwned=1";
        fs::write(
            case_dir.join("case.json"),
            format!("{{\"case_id\":\"FT-s1\"}};{payload};//"),
        )
        .unwrap();
        let index = case_dir.join("db/video_index.json");
        let original_index = fs::read_to_string(&index).unwrap();
        fs::write(&index, format!("{original_index};{payload};0")).unwrap();
        make_review(&case_dir, false, false).unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        for rel in [
            "review/data-bundle.js",
            "review/index.html",
            "reports/case-report.html",
        ] {
            let text = fs::read_to_string(case_dir.join(rel)).unwrap();
            assert!(!text.contains(payload), "{rel} embeds the injected script");
        }
        let bundle = fs::read_to_string(case_dir.join("review/data-bundle.js")).unwrap();
        assert!(bundle.contains("manifest:null"), "{bundle}");
        assert!(bundle.contains("scan:null"));
        // A valid file is re-serialized, never copied: the same case.json
        // with a `</script>` inside a value stays one escaped string.
        fs::write(
            case_dir.join("case.json"),
            r#"{"case_id":"FT-s1","note":"</script><script>alert(1)</script>"}"#,
        )
        .unwrap();
        fs::write(&index, &original_index).unwrap();
        make_review(&case_dir, false, false).unwrap();
        let bundle = fs::read_to_string(case_dir.join("review/data-bundle.js")).unwrap();
        assert!(bundle.contains("\"case_id\":\"FT-s1\""), "{bundle}");
        assert!(!bundle.contains("</script>"));
        assert!(!bundle.contains("scan:null"));
        fs::remove_dir_all(root).unwrap();
    }

    /// I14: the report is replaced through a temp file + rename — a failed
    /// write leaves the previous report intact instead of a torn one.
    #[test]
    fn failed_report_write_keeps_the_previous_report() {
        let (root, case_dir, _source) = scanned_case("atomic");
        make_report(&case_dir, false, false, None).unwrap();
        let report = case_dir.join("reports/case-report.html");
        let before = fs::read(&report).unwrap();
        crate::util::FAIL_ATOMIC_WRITE.with(|fail| *fail.borrow_mut() = Some(report.clone()));
        let failed = make_report(&case_dir, false, false, None);
        crate::util::FAIL_ATOMIC_WRITE.with(|fail| *fail.borrow_mut() = None);
        assert!(failed.is_err());
        assert_eq!(fs::read(&report).unwrap(), before);
        // No temp file is left behind.
        let leftovers = fs::read_dir(case_dir.join("reports"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
        fs::remove_dir_all(root).unwrap();
    }

    /// I9: make-report re-verifies the quoted logs and embeds the result.
    #[test]
    fn report_embeds_audit_chain_verification() {
        let (root, case_dir, _source) = scanned_case("chains");
        make_report(&case_dir, false, false, None).unwrap();
        let html = fs::read_to_string(case_dir.join("reports/case-report.html")).unwrap();
        let line = html
            .lines()
            .find(|line| line.starts_with("const auditChains = "))
            .unwrap();
        let chains: serde_json::Value = serde_json::from_str(
            line.trim_start_matches("const auditChains = ")
                .trim_end_matches(';')
                .replace("\\u003c", "<")
                .replace("\\u003e", ">")
                .replace("\\u0026", "&")
                .as_str(),
        )
        .unwrap();
        // make-report's own anomaly scan wrote a chained log.
        let anomaly = chains["anomaly"]["status"].as_str().unwrap();
        assert!(matches!(anomaly, "structural" | "keyed"), "{chains}");
        assert_eq!(chains["export"]["status"], "absent");
        assert!(!html.contains("변조 방지"));
        fs::remove_dir_all(root).unwrap();
    }

    /// I4: `--redact-paths` reaches notes with mid-string paths and leaves
    /// no absolute source or case path in the report, review page or
    /// viewer data bundle.
    #[test]
    fn redacted_report_and_review_carry_no_absolute_paths() {
        let (root, case_dir, source_dir) = scanned_case("redact");
        let id = crate::case_db::load_video_ids(&case_dir).unwrap()[0]
            .id
            .clone();
        crate::case_db::upsert_review_marks(
            &case_dir,
            &[crate::case_db::ReviewMarkRow {
                record_id: id,
                status: "important".into(),
                marked_unix: 9,
                note: Some(format!(
                    "원본 사본은 {} 에 보관",
                    source_dir.join("copy.mp4").display()
                )),
                ..Default::default()
            }],
        )
        .unwrap();
        // D5: a proxy written before D5 is named after the flattened source
        // path; the redacted bundle must not reveal it through that name.
        fs::create_dir_all(case_dir.join("artifacts/proxies")).unwrap();
        fs::write(
            case_dir.join("artifacts/proxies").join(format!(
                "{}_proxy_1700000000.mp4",
                crate::video_export::sanitize_filename(&source_dir.join("a.mp4").to_string_lossy())
            )),
            b"p",
        )
        .unwrap();
        // E4/E9: the deepfake report's target path and a telemetry
        // extract's source path are redacted like every other path.
        fs::create_dir_all(case_dir.join("artifacts/deepfake")).unwrap();
        fs::write(
            case_dir.join("artifacts/deepfake/vid_000001.json"),
            serde_json::json!({"frametrace_target_path": source_dir.join("a.mp4"), "score": 0.1})
                .to_string(),
        )
        .unwrap();
        fs::create_dir_all(case_dir.join("artifacts/telemetry")).unwrap();
        fs::write(
            case_dir.join("artifacts/telemetry/vid_000001.json"),
            serde_json::json!({"event": "telemetry-extract", "source_path": source_dir.join("a.mp4"),
                "point_count": 2, "total_points": 30000, "truncated": true, "points": []})
            .to_string(),
        )
        .unwrap();
        make_report(&case_dir, false, true, None).unwrap();
        make_review(&case_dir, true, false).unwrap();
        // E9: the report lists the capped track as truncated.
        let telemetry = embedded_json(&report_html(&case_dir), "telemetry");
        assert_eq!(telemetry["vid_000001"]["truncated"], true, "{telemetry}");
        assert_eq!(telemetry["vid_000001"]["total_points"], 30000);
        // D12: the redacted bundle carries the redaction marker line.
        assert!(crate::redact::bundle_is_redacted(
            &fs::read_to_string(case_dir.join("review/data-bundle.js")).unwrap()
        ));
        let canonical_source =
            crate::util::strip_windows_extended_prefix(&source_dir.canonicalize().unwrap());
        let canonical_case =
            crate::util::strip_windows_extended_prefix(&case_dir.canonicalize().unwrap());
        for rel in [
            "reports/case-report.html",
            "review/index.html",
            "review/data-bundle.js",
        ] {
            let text = fs::read_to_string(case_dir.join(rel)).unwrap();
            for path in [&source_dir, &canonical_source, &case_dir, &canonical_case] {
                for spelling in json_spellings(path) {
                    assert!(!text.contains(&spelling), "{rel} leaks {spelling}");
                }
            }
            assert!(
                !text.contains("SourceSecretDir"),
                "{rel} leaks the source folder"
            );
        }
        for rel in [
            "reports/case-report.html",
            "review/index.html",
            "review/evidence-viewer.html",
        ] {
            let html = fs::read_to_string(case_dir.join(rel)).unwrap();
            assert!(crate::redact::html_is_redacted(&html), "{rel} unmarked");
        }
        assert!(
            fs::read_to_string(case_dir.join("review/data-bundle.js"))
                .unwrap()
                .contains("caseDir:\"\\u003ccase\\u003e\"")
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// R5-P6: a redacted report shows the host as the custody token, the
    /// unredacted one as recorded.
    #[test]
    fn redacted_report_tokenizes_the_host() {
        let (root, case_dir, _source) = scanned_case("redact-host");
        let manifest = case_dir.join("case.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
        value["host"] = serde_json::json!("SECRET-LAPTOP");
        fs::write(&manifest, value.to_string()).unwrap();
        make_report(&case_dir, false, true, None).unwrap();
        let html = report_html(&case_dir);
        assert!(
            !html.contains("SECRET-LAPTOP"),
            "redacted report leaks the host"
        );
        let token = crate::redact::host_token("SECRET-LAPTOP");
        assert!(
            html.contains(&token)
                || html.contains(&token.replace('<', "\\u003c").replace('>', "\\u003e")),
            "host token missing"
        );
        make_report(&case_dir, false, false, None).unwrap();
        assert!(report_html(&case_dir).contains("SECRET-LAPTOP"));
        fs::remove_dir_all(root).unwrap();
    }

    /// I10: without `--signoff`, a leftover pending record is not the
    /// report's sign-off; the report names the promoted record it used.
    #[test]
    fn report_uses_promoted_signoff_and_names_it() {
        let (root, case_dir, _source) = scanned_case("signoff");
        let promoted = "{\"schema_version\":3,\"examiner\":\"Promoted\",\"signed_unix\":5}\n";
        fs::create_dir_all(case_dir.join("review")).unwrap();
        fs::write(case_dir.join("review/signoff.json"), promoted).unwrap();
        fs::write(
            case_dir.join("review/signoff.pending.json"),
            "{\"schema_version\":3,\"examiner\":\"Stale\",\"signed_unix\":9}\n",
        )
        .unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        let html = fs::read_to_string(case_dir.join("reports/case-report.html")).unwrap();
        assert!(html.contains("<strong>Promoted</strong>"));
        assert!(!html.contains("Stale"));
        assert_eq!(
            crate::report::report_signoff_sha256(&html),
            Some(crate::sha256::digest_bytes(promoted.as_bytes()).as_str())
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// The embedded `const <name> = …;` JSON of a rendered report.
    fn embedded_json(html: &str, name: &str) -> serde_json::Value {
        let prefix = format!("const {name} = ");
        let line = html
            .lines()
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("no {name} in report"));
        serde_json::from_str(
            line.trim_start_matches(&prefix)
                .trim_end_matches(';')
                .replace("\\u003c", "<")
                .replace("\\u003e", ">")
                .replace("\\u0026", "&")
                .as_str(),
        )
        .unwrap()
    }

    fn report_html(case_dir: &Path) -> String {
        fs::read_to_string(case_dir.join("reports/case-report.html")).unwrap()
    }

    /// D1: make-report re-verifies the sound lane; one forged line and no
    /// record reads silent (or sound) — the chain table says why.
    #[test]
    fn forged_sound_log_line_never_becomes_a_silence_claim() {
        let (root, case_dir, _source) = scanned_case("audio-forged");
        let row: serde_json::Value = serde_json::from_str(
            fs::read_to_string(case_dir.join("db/videos.jsonl"))
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        let id = row["id"].as_str().unwrap().to_string();
        let result = crate::audio_level::FileResult {
            selector: id.clone(),
            source_path: row["source_path"].as_str().unwrap().to_string(),
            size_bytes: row["size_bytes"].as_u64(),
            modified_unix: row["modified_unix"].as_u64(),
            sha256_indexed: None,
            result: crate::audio_level::Verdict::Sound,
            streams: Vec::new(),
            error: None,
            measured_unix: None,
            replayed_unverified: false,
            chain_unverified: None,
            self_consistent_only: false,
            other_pc_key: None,
        };
        let log = case_dir.join(crate::audio_level::LOG_REL);
        crate::audit::append_chained_jsonl_keyed(
            &log,
            &crate::audio_level::log_entry_json(7, "ffmpeg x", &[result]).to_string(),
            None,
        )
        .unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        let levels = embedded_json(&report_html(&case_dir), "audioLevels");
        assert_eq!(levels[&id]["r"], "sound", "{levels}");

        let forged = fs::read_to_string(&log)
            .unwrap()
            .replace("\"result\":\"sound\"", "\"result\":\"silent\"");
        fs::write(&log, forged).unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        let html = report_html(&case_dir);
        let levels = embedded_json(&html, "audioLevels");
        assert_eq!(levels[&id]["r"], "unverified", "{levels}");
        let chains = embedded_json(&html, "auditChains");
        assert_eq!(chains["audio"]["status"], "failed", "{chains}");
        fs::remove_dir_all(root).unwrap();
    }

    /// D11: a log that exists but cannot be read is named in the report,
    /// not shown as "none". D9: queued marks / open conflicts are warned
    /// about and package-case refuses them without --allow-pending.
    #[test]
    fn unreadable_logs_and_pending_review_state_are_disclosed() {
        let (root, case_dir, _source) = scanned_case("read-errors");
        fs::create_dir_all(case_dir.join("artifacts/carved/carve-log.jsonl")).unwrap();
        fs::create_dir_all(case_dir.join("marks-pending")).unwrap();
        fs::write(case_dir.join("marks-pending/00000001.json"), "{}").unwrap();
        fs::write(
            case_dir.join("marks-pending/conflicts.jsonl"),
            "{\"id\":\"vid_000001\",\"server_revision\":2}\n{\"id\":\"vid_000002\"}\n{\"resolved\":\"vid_000002\"}\n",
        )
        .unwrap();
        assert_eq!(super::review_queue_state(&case_dir), (1, 1));
        make_report(&case_dir, false, false, None).unwrap();
        let html = report_html(&case_dir);
        let errors = embedded_json(&html, "readErrors");
        assert!(
            errors["carve"]
                .as_str()
                .unwrap()
                .contains("artifacts/carved/carve-log.jsonl"),
            "{errors}"
        );
        let warnings = embedded_json(&html, "reviewWarnings");
        assert_eq!(warnings.as_array().unwrap().len(), 2, "{warnings}");

        let err = super::package_case(&case_dir, super::PackageOptions::default()).unwrap_err();
        assert!(err.contains("--allow-pending"), "{err}");
        assert!(err.contains("1건"), "{err}");
        fs::remove_dir_all(root).unwrap();
    }

    /// D3/D18: join entries get their manifest's measured length (only
    /// when the manifest still has the recorded hash) and say whether
    /// their record id is the log's own.
    #[test]
    fn report_join_log_adds_manifest_length_and_legacy_flag() {
        let root =
            std::env::temp_dir().join(format!("frametrace-report-join-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("artifacts/joined")).unwrap();
        let manifest = "{\"duration_check\":{\"expected\":60.0,\"observed\":59.5,\"ok\":true},\"segment_check\":{\"video_packets_expected\":10,\"video_packets_observed\":9,\"boundaries\":[{\"segment\":2,\"expected_start_seconds\":30.0,\"observed_start_seconds\":null}],\"ok\":false,\"problems\":[\"segment 2 missing\"]}}\n";
        fs::write(root.join("artifacts/joined/a.manifest.json"), manifest).unwrap();
        let manifest_sha = crate::sha256::digest_bytes(manifest.as_bytes());
        let lines = [
            serde_json::json!({"output_sha256": "ab".repeat(32), "manifest_path": "artifacts/joined/a.manifest.json", "manifest_sha256": manifest_sha}),
            serde_json::json!({"record_id": "join:own", "output_sha256": "cd".repeat(32), "manifest_path": "artifacts/joined/a.manifest.json", "manifest_sha256": "0".repeat(64)}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        let out: Vec<serde_json::Value> = super::report_join_log(&root, &lines)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(out[0]["record_id"], "join:abababababababab");
        assert_eq!(out[0]["record_id_derived"], true);
        assert_eq!(out[0]["duration_check"]["observed"], 59.5);
        // C14's packet / boundary verification becomes boundary_check.
        assert_eq!(out[0]["boundary_check"]["ok"], false);
        assert_eq!(
            out[0]["boundary_check"]["detail"],
            "영상 패킷 9/10; 경계 0/1개 확인; segment 2 missing"
        );
        assert_eq!(out[1]["record_id"], "join:own");
        assert!(out[1].get("record_id_derived").is_none());
        assert!(
            out[1].get("duration_check").is_none(),
            "manifest hash mismatch: not trusted"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A scanned case WITH file hashes, one marked record, a two-entry
    /// validation lane, a v4 sign-off recorded in the sign-off lane, and a
    /// report. Returns `(root, case_dir, record id)`.
    fn signed_case(name: &str) -> (PathBuf, PathBuf, String) {
        let root = std::env::temp_dir().join(format!(
            "frametrace-report-out-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        let source_dir = root.join("src");
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42one").unwrap();
        init_case(&case_dir, &InitCaseOptions::default()).unwrap();
        crate::scan::scan_folder(
            &case_dir,
            &source_dir,
            &ScanOptions {
                hash_files: true,
                use_ffprobe: false,
                ..ScanOptions::default()
            },
            crate::checkpoint::ResumeMode::Auto,
            None,
        )
        .unwrap();
        let id = crate::case_db::load_video_ids(&case_dir).unwrap()[0]
            .id
            .clone();
        crate::case_db::upsert_review_marks(
            &case_dir,
            &[crate::case_db::ReviewMarkRow {
                record_id: id.clone(),
                status: "important".into(),
                marked_unix: 9,
                ..Default::default()
            }],
        )
        .unwrap();
        let lane = case_dir.join("evidence/logs/validation-log.jsonl");
        for n in 0..2 {
            crate::audit::append_chained_jsonl_keyed(&lane, &format!(r#"{{"n":{n}}}"#), None)
                .unwrap();
        }
        let signoff = crate::report::Signoff::build_v4(&case_dir, "홍길동", 1_700_000_000).unwrap();
        // Determinism: same state → same record bytes.
        assert_eq!(
            crate::report::Signoff::build_v4(&case_dir, "홍길동", 1_700_000_000)
                .unwrap()
                .text,
            signoff.text
        );
        fs::write(
            case_dir.join(crate::report::SIGNOFF_REL_PATH),
            &signoff.text,
        )
        .unwrap();
        crate::report::record_signoff_audit(&case_dir, &signoff).unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        (root, case_dir, id)
    }

    /// D13: make-report says a v4 sign-off is current (with the chain
    /// record), then "changed" when the marked record's target hash
    /// changes or an anchored lane loses its tail.
    #[test]
    fn report_flags_v4_signoff_changed_on_target_hash_or_lane_truncation() {
        let (root, case_dir, id) = signed_case("v4-report");
        let html = report_html(&case_dir);
        assert!(html.contains("변경되지 않았습니다"), "current at first");
        assert!(html.contains("서명 형식 v4"), "v4 detail shown");
        assert!(
            html.contains("서명 기록이 감사 체인(signoff-log"),
            "chain record found"
        );
        let record = fs::read_to_string(case_dir.join(crate::report::SIGNOFF_REL_PATH)).unwrap();
        assert!(record.contains("\"schema_version\":4"));
        assert!(
            record.contains("\"evidence/logs/validation-log.jsonl\":{\"entries\":2"),
            "{record}"
        );

        // Lane tail removed (the chain alone still verifies).
        let lane = case_dir.join("evidence/logs/validation-log.jsonl");
        let text = fs::read_to_string(&lane).unwrap();
        fs::write(&lane, format!("{}\n", text.lines().next().unwrap())).unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        let html = report_html(&case_dir);
        assert!(html.contains("변경되었습니다"), "truncation flagged");
        assert!(html.contains("validation-log.jsonl: 감사 레인 끝부분이 삭제됨"));
        fs::write(&lane, &text).unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        assert!(
            report_html(&case_dir).contains("변경되지 않았습니다"),
            "restored"
        );

        // The marked record's target hash changes (re-scan of other bytes).
        let videos = case_dir.join("db/videos.jsonl");
        let rows = fs::read_to_string(&videos).unwrap();
        let mut row: serde_json::Value =
            serde_json::from_str(rows.lines().next().unwrap()).unwrap();
        row["sha256"] = "f".repeat(64).into();
        fs::write(&videos, format!("{row}\n")).unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        let html = report_html(&case_dir);
        assert!(html.contains("변경되었습니다"));
        assert!(
            html.contains(&format!(
                "{id}: 케이스 기록의 대상 파일 해시가 서명 후 바뀜"
            )),
            "target change named"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// R5-P2: with `--rehash` (what finalize runs) the signed original is
    /// hashed again; bytes replaced behind an unchanged index read
    /// "changed". Without it the banner says only the case records were
    /// compared.
    #[test]
    fn report_rehash_catches_a_replaced_signed_original() {
        let (root, case_dir, id) = signed_case("v4-rehash");
        let html = report_html(&case_dir);
        assert!(html.contains("다시 해시하지 않음"), "records-only wording");
        make_report(&case_dir, true, false, None).unwrap();
        let html = report_html(&case_dir);
        assert!(
            html.contains("변경되지 않았습니다"),
            "unchanged file is current"
        );
        assert!(html.contains("다시 해시해"), "rehash stated");
        fs::write(root.join("src/a.mp4"), b"\0\0\0\x18ftypmp42two").unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        assert!(
            report_html(&case_dir).contains("변경되지 않았습니다"),
            "the records alone cannot see the swap"
        );
        make_report(&case_dir, true, false, None).unwrap();
        let html = report_html(&case_dir);
        assert!(html.contains("변경되었습니다"), "rehash flags it");
        assert!(
            html.contains(&format!("{id}: 대상 파일을 다시 해시한 값")),
            "the changed file is named"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Rewrites one packaged file and fixes both manifests for it — what an
    /// adversary who recomputes the package hashes does.
    fn rewrite_packaged_file(package: &Path, rel: &str, text: &str) {
        fs::write(package.join(rel), text).unwrap();
        let sha = crate::sha256::digest_bytes(text.as_bytes());
        let manifest = fs::read_to_string(package.join("manifest.sha256")).unwrap();
        let manifest: String = manifest
            .lines()
            .map(|line| {
                if line.ends_with(&format!("  {rel}")) {
                    format!("{sha}  {rel}\n")
                } else {
                    format!("{line}\n")
                }
            })
            .collect();
        fs::write(package.join("manifest.sha256"), manifest).unwrap();
        let path = package.join("package-manifest.json");
        let mut json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for file in json["files"].as_array_mut().unwrap() {
            if file["relative_path"] == rel {
                file["sha256"] = sha.clone().into();
                file["size_bytes"] = (text.len() as u64).into();
            }
        }
        fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    }

    fn failed_subjects(result: &crate::package::PackageVerification) -> Vec<String> {
        result
            .failures()
            .map(|check| format!("{}: {}", check.subject, check.detail))
            .collect()
    }

    /// D13/D10/D16: verify-package checks the v4 bindings against the
    /// package (a truncated lane or a swapped target hash FAILS even with
    /// recomputed manifests), checks the report marker, and chain-verifies
    /// a known lane whose first line no longer looks chained.
    #[test]
    fn verify_package_checks_v4_bindings_marker_and_known_lanes() {
        let (root, case_dir, id) = signed_case("v4-package");
        let package = root.join("package");
        super::package_case(
            &case_dir,
            super::PackageOptions {
                output_dir: Some(package.clone()),
            },
        )
        .unwrap();
        let result = super::verify_package_extended(&package).unwrap();
        assert!(result.passed(), "{:?}", failed_subjects(&result));
        let details: Vec<&str> = result.checks.iter().map(|c| c.detail.as_str()).collect();
        assert!(
            details
                .iter()
                .any(|d| d.contains("sign-off v4: marks, the case records' target hashes")),
            "{details:?}"
        );
        assert!(
            details
                .iter()
                .any(|d| d.contains("signoff-sha256 marker matches"))
        );

        // Truncated lane with recomputed manifests.
        let lane_rel = "evidence/logs/validation-log.jsonl";
        let lane = fs::read_to_string(package.join(lane_rel)).unwrap();
        rewrite_packaged_file(
            &package,
            lane_rel,
            &format!("{}\n", lane.lines().next().unwrap()),
        );
        let result = super::verify_package_extended(&package).unwrap();
        assert!(
            failed_subjects(&result)
                .iter()
                .any(|f| f.contains("끝부분이 삭제됨")),
            "{:?}",
            failed_subjects(&result)
        );
        rewrite_packaged_file(&package, lane_rel, &lane);

        // Swapped target hash in the packaged index.
        let videos = fs::read_to_string(package.join("db/videos.jsonl")).unwrap();
        let mut row: serde_json::Value =
            serde_json::from_str(videos.lines().next().unwrap()).unwrap();
        row["sha256"] = "e".repeat(64).into();
        rewrite_packaged_file(&package, "db/videos.jsonl", &format!("{row}\n"));
        let result = super::verify_package_extended(&package).unwrap();
        assert!(
            failed_subjects(&result)
                .iter()
                .any(|f| f.contains(&format!("{id}: 케이스 기록의 대상 파일 해시"))),
            "{:?}",
            failed_subjects(&result)
        );
        rewrite_packaged_file(&package, "db/videos.jsonl", &videos);

        // D16: a known lane whose first line is not chained still FAILS.
        rewrite_packaged_file(&package, lane_rel, "{\"plain\":true}\n");
        let result = super::verify_package_extended(&package).unwrap();
        assert!(
            failed_subjects(&result)
                .iter()
                .any(|f| f.starts_with(lane_rel) && f.contains("does not verify as a chain")),
            "{:?}",
            failed_subjects(&result)
        );
        rewrite_packaged_file(&package, lane_rel, &lane);

        // D10: an unlisted file FAILS and the "no unlisted files" PASS line
        // is not printed next to it.
        fs::write(package.join("reports/extra.txt"), "added").unwrap();
        let result = super::verify_package_extended(&package).unwrap();
        assert!(!result.passed());
        assert!(
            !result
                .checks
                .iter()
                .any(|c| c.status == crate::package::CheckStatus::Pass
                    && c.detail.contains("no unlisted files")),
            "contradictory PASS line kept"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// B5: CLI-built pages are recorded as generated (so the workstation
    /// serves them with scripts, not sandboxed), and the report and review
    /// pages carry no inline handlers (they qualify for a strict
    /// hash-based script-src).
    #[test]
    fn cli_built_pages_are_recorded_and_free_of_inline_handlers() {
        use crate::util::generated_pages::{inline_script_sources, page_is_trusted};
        let (root, case_dir, _source) = scanned_case("generated-pages");
        make_review(&case_dir, false, false).unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        super::make_custody(&case_dir).unwrap();
        for rel in [
            "reports/case-report.html",
            "review/index.html",
            "review/evidence-viewer.html",
            "reports/custody-statement.html",
        ] {
            assert!(page_is_trusted(&case_dir, rel), "{rel} not recorded");
        }
        let report = report_html(&case_dir);
        assert!(
            inline_script_sources(&report).is_some(),
            "case report must not use inline handlers"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// R7-S8: package-case refreshes the custody statement before sealing
    /// it; the refreshed page is recorded as generated. Before, it was
    /// rewritten through `custody::generate` without a record, so the
    /// workstation served the case's custody page sandboxed afterwards.
    #[test]
    fn package_case_records_the_refreshed_custody_statement() {
        use crate::util::generated_pages::page_is_trusted;
        let (root, case_dir, _source) = scanned_case("s8-custody");
        let custody = case_dir.join(crate::custody::CUSTODY_REL_PATH);
        assert!(!custody.exists());
        super::package_case(
            &case_dir,
            super::PackageOptions {
                output_dir: Some(root.join("package")),
            },
        )
        .unwrap();
        assert!(custody.is_file(), "package-case writes the statement");
        assert!(
            page_is_trusted(&case_dir, crate::custody::CUSTODY_REL_PATH),
            "refreshed custody statement not recorded"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// R8 P0/P1/P2 for the report's audit-chain table: a lane stripped of
    /// its HMACs read "structural" on a PC holding the key in R7; now it is
    /// "not-verified" there. A lane continued on another PC names that
    /// PC's key and is not "failed". Without keys: structural, with the
    /// self-consistency caveat.
    #[test]
    fn r8_report_chain_table_trust_comes_from_keys_held() {
        let case = std::env::temp_dir().join(format!("ft-r8-chains-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case);
        let validation = case.join("evidence/logs/validation-log.jsonl");
        let anomaly = case.join("evidence/logs/anomaly-log.jsonl");
        fs::create_dir_all(validation.parent().unwrap()).unwrap();
        let mine = crate::audit_key::AuditKey::new("pc-a", &[0xe1; 32]);
        let other = crate::audit_key::AuditKey::new("pc-b", &[0xe2; 32]);
        crate::audit::append_chained_jsonl_keyed(&validation, r#"{"n":1}"#, None).unwrap();
        crate::audit::append_chained_jsonl_keyed(&anomaly, r#"{"n":1}"#, Some(&mine)).unwrap();
        crate::audit::append_chained_jsonl_keyed(&anomaly, r#"{"n":2}"#, Some(&other)).unwrap();
        let chains = |keys: &[crate::audit_key::AuditKey]| -> serde_json::Value {
            crate::audit_key::with_test_keys(keys, || {
                serde_json::from_str(&super::report_audit_chains_json(&case)).unwrap()
            })
        };
        let held = chains(std::slice::from_ref(&mine));
        assert_eq!(held["validation"]["status"], "not-verified", "{held}");
        assert_eq!(held["anomaly"]["status"], "not-verified", "{held}");
        assert!(
            held["anomaly"]["detail"]
                .as_str()
                .unwrap()
                .contains("\"pc-b\" 보유 PC에서 확인"),
            "{held}"
        );
        let keyless = chains(&[]);
        assert_eq!(keyless["validation"]["status"], "structural", "{keyless}");
        assert!(
            keyless["validation"]["detail"]
                .as_str()
                .unwrap()
                .contains("자기 일관성만 확인(인증 아님)"),
            "{keyless}"
        );
        fs::remove_dir_all(&case).unwrap();
    }

    /// R11-S8: `verify-package --strict` exits 3 for a package whose files
    /// check out but whose finalize did not complete (R10 exited 0 — a
    /// script checking only the exit code took it as final). Without
    /// `--strict` it stays 0 (NOT VERIFIED is not FAIL); a complete
    /// package is 0 either way and a FAIL is still the error (exit 1).
    #[test]
    fn r11_s8_strict_verify_package_exits_3_for_an_incomplete_finalize() {
        let (root, case_dir, _id) = signed_case("r11-s8");
        let package = root.join("package_1");
        super::package_case(
            &case_dir,
            super::PackageOptions {
                output_dir: Some(package.clone()),
            },
        )
        .unwrap();
        assert_eq!(super::verify_package_with(&package, false), Ok(0));
        // R13-S12: keyless here — strict is 3 for the structural sign-off
        // already; the incomplete finalize is what the rest checks.
        assert!(
            crate::package::verify_package(&package)
                .unwrap()
                .signoff_unauthenticated
        );
        let incomplete = root.join("package_1_INCOMPLETE");
        fs::rename(&package, &incomplete).unwrap();
        assert_eq!(super::verify_package_with(&incomplete, false), Ok(0));
        assert_eq!(
            super::verify_package_with(&incomplete, true),
            Ok(super::VERIFY_PACKAGE_INCOMPLETE_EXIT_CODE)
        );
        assert_eq!(super::VERIFY_PACKAGE_INCOMPLETE_EXIT_CODE, 3);
        // `.` inside the folder (R11-S2) is the same package.
        assert_eq!(
            super::verify_package_with(&incomplete.join("."), true),
            Ok(3)
        );
        // A FAIL stays a failure, strict or not.
        fs::write(incomplete.join("README.txt"), "changed").unwrap();
        assert!(super::verify_package_with(&incomplete, true).is_err());
        let _ = fs::remove_dir_all(root);
    }

    /// R12-S8: `--strict` exits 3 when the sign-off is NOT VERIFIED — here
    /// a package with no sign-off record and one whose sign-off-lane entry
    /// is gone from the package. R11 exited 0 for both (only an incomplete
    /// finalize was 3). Without `--strict` both stay 0.
    #[test]
    fn r12_s8_strict_verify_package_exits_3_for_an_unverified_signoff() {
        let (root, case_dir, _id) = signed_case("r12-s8");
        // Signed and complete, keyless (structural sign-off): 0 without
        // --strict; R13-S12: 3 with it (R12 exited 0).
        let signed = root.join("package_signed");
        super::package_case(
            &case_dir,
            super::PackageOptions {
                output_dir: Some(signed.clone()),
            },
        )
        .unwrap();
        assert_eq!(super::verify_package_with(&signed, false), Ok(0));
        assert_eq!(
            super::verify_package_with(&signed, true),
            Ok(super::VERIFY_PACKAGE_INCOMPLETE_EXIT_CODE)
        );
        // No sign-off record at all.
        fs::remove_file(case_dir.join(crate::report::SIGNOFF_REL_PATH)).unwrap();
        make_report(&case_dir, false, false, None).unwrap();
        let unsigned = root.join("package_unsigned");
        super::package_case(
            &case_dir,
            super::PackageOptions {
                output_dir: Some(unsigned.clone()),
            },
        )
        .unwrap();
        let checked = crate::package::verify_package(&unsigned).unwrap();
        assert!(checked.passed(), "{:?}", checked.checks);
        assert_eq!(super::verify_package_with(&unsigned, false), Ok(0));
        assert_eq!(
            super::verify_package_with(&unsigned, true),
            Ok(super::VERIFY_PACKAGE_INCOMPLETE_EXIT_CODE)
        );
        let _ = fs::remove_dir_all(root);
    }

    /// R13-S12: `--strict` exits 3 when the sign-off lane entry matched
    /// only structurally (`authenticated == false`); an authenticated
    /// sign-off of a complete package is 0. a4fb793 exited 0 for both.
    #[test]
    fn r13_s12_strict_exits_3_for_a_structural_signoff() {
        let pass = crate::package::PackageCheck {
            status: crate::package::CheckStatus::Pass,
            subject: crate::report::SIGNOFF_LOG_REL_PATH.to_string(),
            detail: "matched".to_string(),
        };
        let mut result = crate::package::PackageVerification {
            checks: vec![pass],
            ..Default::default()
        };
        assert_eq!(super::verify_package_exit_code(&result, true), 0);
        result.signoff_unauthenticated = true;
        assert_eq!(super::verify_package_exit_code(&result, false), 0);
        assert_eq!(
            super::verify_package_exit_code(&result, true),
            super::VERIFY_PACKAGE_INCOMPLETE_EXIT_CODE
        );
    }
}
