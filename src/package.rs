use crate::audit;
// I14: the package's manifest files and README are written atomically.
use crate::util::{json_escape, now_unix, write_text_atomic as write_text};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PackageResult {
    pub output_dir: PathBuf,
    pub file_count: usize,
    pub manifest_path: PathBuf,
    /// Things the package could not settle silently (a leftover pending
    /// sign-off, a report naming another sign-off record); also recorded
    /// in package-manifest.json and README.txt.
    pub warnings: Vec<String>,
    /// True when the package was built in path-redaction mode (I4).
    pub path_redacted: bool,
}

#[derive(Debug, Clone)]
struct PackageFile {
    relative_path: PathBuf,
    sha256: String,
    size_bytes: u64,
}

/// I4: what a path-redacted package did with each file.
struct PackageRedaction {
    redactor: crate::redact::Redactor,
    /// Package copies rewritten with paths redacted (they differ from the
    /// case files they were made from).
    rewritten: Vec<String>,
    /// Copied byte-for-byte although they may contain absolute paths: the
    /// database, index files, hash-chained logs (rewriting would break their
    /// chains) and binary media.
    not_redacted: Vec<String>,
}

/// Per-run state threaded through the copy helpers.
#[derive(Default)]
struct PackageContext {
    redaction: Option<PackageRedaction>,
}

/// Generated pages whose redaction state decides the package mode.
const REDACTABLE_PAGES: &[&str] = &[
    "reports/case-report.html",
    "review/index.html",
    VIEWER_REL,
    CARVE_REPORT_REL,
];

/// I4: a package is path-redacted when the case's generated pages were
/// written with `--redact-paths`. A mix (a redacted report next to an
/// unredacted review, say) is refused: shipping it would leak the very
/// paths the redacted page hides.
fn package_redaction_mode(case_dir: &Path) -> Result<bool, String> {
    let mut redacted = Vec::new();
    let mut plain = Vec::new();
    for rel in REDACTABLE_PAGES {
        let Ok(text) = fs::read_to_string(case_dir.join(rel)) else {
            continue;
        };
        if crate::redact::html_is_redacted(&text) {
            redacted.push(*rel);
        } else {
            plain.push(*rel);
        }
    }
    if !redacted.is_empty() && !plain.is_empty() {
        return Err(format!(
            "package-case refused: {} were generated with --redact-paths but {} were not; a package mixing them would expose the paths the redacted pages hide. Re-run `frametrace make-report --redact-paths` and `frametrace make-review --redact-paths` (or both without the flag) and package again.",
            redacted.join(", "),
            plain.join(", ")
        ));
    }
    Ok(!redacted.is_empty())
}

pub fn package_case(case_dir: &Path, output_dir: Option<&Path>) -> Result<PackageResult, String> {
    let created_unix = now_unix()?;
    let mut context = PackageContext {
        redaction: package_redaction_mode(case_dir)?.then(|| PackageRedaction {
            redactor: crate::redact::Redactor::new(case_dir),
            rewritten: Vec::new(),
            not_redacted: Vec::new(),
        }),
    };
    let running_jobs = match crate::case_db::latest_running_jobs(case_dir) {
        Ok(jobs) => jobs,
        Err(err) => {
            return Err(format!(
                "package-case cannot determine case quiescence; refusing to claim an unverified snapshot: {err}"
            ));
        }
    };
    let quiescent = running_jobs.is_empty();
    let output_dir = match output_dir {
        Some(path) => {
            reject_recursive_package_output(case_dir, path)?;
            if path.exists()
                && path
                    .read_dir()
                    .map_err(|err| format!("failed to inspect package output: {err}"))?
                    .next()
                    .is_some()
            {
                return Err(format!(
                    "package output already exists and is not empty: {}",
                    path.display()
                ));
            }
            path.to_path_buf()
        }
        None => crate::util::unique_dir(
            &case_dir
                .join("reports")
                .join(format!("package_{created_unix}")),
        ),
    };
    validate_required_package_files(case_dir)?;
    if case_dir.join(VIEWER_REL).is_file() && !case_dir.join(DATA_BUNDLE_REL).is_file() {
        return Err(format!(
            "{VIEWER_REL} is present but {DATA_BUNDLE_REL} is missing: the standalone viewer would be packaged without its data. Re-run `frametrace make-review <case-dir>` and package again."
        ));
    }
    // Preflight: the package duplicates every report/artifact under the
    // case dir, so ensure the target volume can hold a second copy before
    // the copy loop starts.
    crate::diskspace::ensure_available(
        &output_dir,
        estimate_package_bytes(case_dir, &output_dir),
        "package-case",
    )?;
    fs::create_dir_all(&output_dir)
        .map_err(|err| format!("failed to create package output: {err}"))?;

    let mut files = Vec::new();
    let mut missing_optional_files = Vec::new();
    for rel in required_package_files() {
        copy_package_file(
            case_dir,
            &output_dir,
            Path::new(rel),
            &mut files,
            &mut context,
        )?;
    }
    for rel in optional_package_files() {
        copy_optional_package_file(
            case_dir,
            &output_dir,
            Path::new(rel),
            &mut files,
            &mut missing_optional_files,
            &mut context,
        )?;
    }
    for rel_dir in recursive_package_dirs() {
        copy_package_dir(
            case_dir,
            &output_dir,
            Path::new(rel_dir),
            &mut files,
            &mut context,
        )?;
    }
    let (signoff_source, warnings) = copy_signoff(case_dir, &output_dir, &mut files)?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if let Some(redaction) = context.redaction.as_mut() {
        redaction.rewritten.sort();
        redaction.not_redacted.sort();
    }

    let checksum_text = files
        .iter()
        .map(|file| {
            format!(
                "{}  {}\n",
                file.sha256,
                rel_manifest_path(&file.relative_path)
            )
        })
        .collect::<String>();
    let checksum_path = output_dir.join("manifest.sha256");
    write_text(&checksum_path, &checksum_text)
        .map_err(|err| format!("failed to write package checksum manifest: {err}"))?;

    let manifest_json = package_manifest_json(
        created_unix,
        &files,
        &missing_optional_files,
        &running_jobs,
        quiescent,
        signoff_source,
        &warnings,
        context.redaction.as_ref(),
    );
    let manifest_path = output_dir.join("package-manifest.json");
    write_text(&manifest_path, &manifest_json)
        .map_err(|err| format!("failed to write package manifest: {err}"))?;

    let report_included = files
        .iter()
        .any(|file| file.relative_path == Path::new("reports/case-report.html"));
    let report_note = if report_included {
        "Open reports/case-report.html for the HTML report. Use the browser print dialog to create a PDF when required by the engagement."
    } else {
        "No HTML report is included — run `frametrace make-report <case-dir>` in the case before packaging to produce reports/case-report.html."
    };
    let mut readme = format!(
        "FrameTrace case package\n\n{report_note} Verify package contents with manifest.sha256 before transfer (`frametrace verify-package <package-dir>` checks the manifest, audit chains and sign-off and reports what it could not verify).\n\nSnapshot scope: see snapshot_scope in package-manifest.json. For Amped FIVE or Magnet DVR Examiner handoff steps, see docs/COMMERCIAL_HANDOFF.md in the FrameTrace repository.\n"
    );
    if let Some(redaction) = &context.redaction {
        readme.push_str(&redaction_readme_section(redaction));
    }
    if !warnings.is_empty() {
        readme.push_str("\nWARNINGS\n");
        for warning in &warnings {
            readme.push_str(&format!("- {warning}\n"));
        }
    }
    write_text(&output_dir.join("README.txt"), &readme)
        .map_err(|err| format!("failed to write package README: {err}"))?;

    Ok(PackageResult {
        output_dir,
        file_count: files.len(),
        manifest_path,
        warnings,
        path_redacted: context.redaction.is_some(),
    })
}

/// README section of a path-redacted package: what was rewritten and —
/// stated, never implied — which packaged files still carry absolute paths.
fn redaction_readme_section(redaction: &PackageRedaction) -> String {
    const LIST_LIMIT: usize = 200;
    let mut out = String::from(
        "\nPATH REDACTION\nThe report and review pages of this package were generated with --redact-paths, and package-case rewrote the documents listed below as path-redacted copies (absolute paths shown as <case>/... or <redacted:hash>). Rewritten copies differ from the case files they came from: a hash recorded elsewhere for the original (for example manifest_sha256 in artifacts/joined/join-log.jsonl) does not match the redacted copy.\n\nRewritten (path-redacted) copies:\n",
    );
    if redaction.rewritten.is_empty() {
        out.push_str("- (none)\n");
    }
    for rel in &redaction.rewritten {
        out.push_str(&format!("- {rel}\n"));
    }
    out.push_str("\nFILES NOT PATH-REDACTED (copied byte-for-byte; they may contain absolute source and case paths): the SQLite database and index files are the case data itself, hash-chained audit logs cannot be rewritten without breaking their chain hashes, and binary media/images were not inspected.\n");
    for rel in redaction.not_redacted.iter().take(LIST_LIMIT) {
        out.push_str(&format!("- {rel}\n"));
    }
    if redaction.not_redacted.len() > LIST_LIMIT {
        out.push_str(&format!(
            "- ... and {} more (full list: path_redaction.not_redacted_files in package-manifest.json)\n",
            redaction.not_redacted.len() - LIST_LIMIT
        ));
    }
    out
}

/// How a path-redacted package treats one file.
enum RedactedCopy {
    /// Copy verbatim; `listed` = it may hold absolute paths and goes into
    /// the not-redacted list.
    Verbatim { listed: bool },
    /// Write this redacted text instead of the case file.
    Rewrite(String),
}

fn is_text_document(rel: &Path) -> bool {
    matches!(
        rel.extension().and_then(|ext| ext.to_str()),
        Some("md" | "html" | "json" | "jsonl" | "csv" | "tsv" | "xml" | "txt" | "dfxml")
    )
}

/// A `.jsonl` whose first record carries chain fields.
fn is_chained_jsonl(path: &Path) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| !line.trim().is_empty())
                .and_then(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        })
        .is_some_and(|value| {
            value.get("previous_entry_sha256").is_some() || value.get("entry_sha256").is_some()
        })
}

fn classify_redacted_copy(
    case_dir: &Path,
    rel: &Path,
    source: &Path,
    redactor: &crate::redact::Redactor,
) -> Result<RedactedCopy, String> {
    let rel_text = rel_manifest_path(rel);
    if rel_text == crate::custody::CUSTODY_REL_PATH {
        return crate::custody::render_statement(case_dir, true)
            .map(RedactedCopy::Rewrite)
            .map_err(|err| {
                format!(
                    "package-case refused: cannot render the path-redacted custody statement: {err}"
                )
            });
    }
    if REDACTABLE_PAGES.contains(&rel_text.as_str()) || rel_text == DATA_BUNDLE_REL {
        // Generated redacted (mode check above); the bundle is written by
        // the same make-review run as the viewer page.
        return Ok(RedactedCopy::Verbatim { listed: false });
    }
    let raw_data = rel.starts_with("db") || rel == Path::new("case.json");
    if raw_data || !is_text_document(rel) {
        return Ok(RedactedCopy::Verbatim { listed: true });
    }
    let is_jsonl = rel.extension().and_then(|ext| ext.to_str()) == Some("jsonl");
    if is_jsonl && is_chained_jsonl(source) {
        return Ok(RedactedCopy::Verbatim { listed: true });
    }
    let text = fs::read_to_string(source).map_err(|err| {
        format!(
            "failed to read package source {} for path redaction: {err}",
            source.display()
        )
    })?;
    if rel.extension().and_then(|ext| ext.to_str()) == Some("html")
        && crate::redact::html_is_redacted(&text)
    {
        return Ok(RedactedCopy::Verbatim { listed: false });
    }
    // JSON is redacted value by value (string escapes decoded first); text
    // that does not parse as JSON/JSONL is scanned as plain text instead,
    // so a malformed document never passes through unredacted.
    let parses_as_json = serde_json::from_str::<serde_json::Value>(&text).is_ok()
        || text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok());
    let rewritten = match rel.extension().and_then(|ext| ext.to_str()) {
        Some("json" | "jsonl") if parses_as_json => redactor.json_text(&text),
        _ => redactor.text(&text),
    };
    Ok(RedactedCopy::Rewrite(rewritten))
}

fn estimate_package_bytes(case_dir: &Path, output_dir: &Path) -> u64 {
    fn dir_bytes(path: &Path, rel: &Path, output_dir: &Path, total: &mut u64) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let entry_rel = rel.join(entry.file_name());
            if excluded_package_path(&entry_path, output_dir) {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&entry_path) else {
                continue;
            };
            if metadata.is_file() && is_report_file(rel) {
                *total = total.saturating_add(metadata.len());
            } else if metadata.is_dir() {
                dir_bytes(&entry_path, &entry_rel, output_dir, total);
            }
        }
    }

    let mut total = 0u64;
    for rel in required_package_files()
        .iter()
        .chain(optional_package_files().iter())
    {
        total = total.saturating_add(
            fs::metadata(case_dir.join(rel))
                .map(|meta| meta.len())
                .unwrap_or(0),
        );
    }
    dir_bytes(
        &case_dir.join("reports"),
        Path::new("reports"),
        output_dir,
        &mut total,
    );
    dir_bytes(
        &case_dir.join("qa"),
        Path::new("qa"),
        output_dir,
        &mut total,
    );
    for rel_dir in recursive_package_dirs() {
        dir_bytes(
            &case_dir.join(rel_dir),
            Path::new(rel_dir),
            output_dir,
            &mut total,
        );
    }
    total
}

fn reject_recursive_package_output(case_dir: &Path, output_dir: &Path) -> Result<(), String> {
    for rel_dir in recursive_package_dirs() {
        let packaged_tree = case_dir.join(rel_dir);
        if output_dir.starts_with(&packaged_tree) {
            return Err(format!(
                "package output cannot be inside recursively packaged directory: {}",
                packaged_tree.display()
            ));
        }
    }
    Ok(())
}

fn required_package_files() -> &'static [&'static str] {
    &[
        "case.json",
        "db/case.db",
        "db/video_index.json",
        "db/videos.jsonl",
        "db/video_paths.tsv",
    ]
}

/// The standalone viewer and the data file it loads as a sibling.
const VIEWER_REL: &str = "review/evidence-viewer.html";
const DATA_BUNDLE_REL: &str = "review/data-bundle.js";
const CARVE_REPORT_REL: &str = "review/carve-report.html";
const CARVE_LOG_REL: &str = "artifacts/carved/carve-log.jsonl";

fn optional_package_files() -> &'static [&'static str] {
    &[
        "db/carve_results.json",
        "review/index.html",
        VIEWER_REL,
        DATA_BUNDLE_REL,
        CARVE_REPORT_REL,
        "review/thumbs.json",
        "artifacts/logs/batch-log.jsonl",
        "db/timeline.jsonl",
    ]
}

fn validate_required_package_files(case_dir: &Path) -> Result<(), String> {
    let missing = required_package_files()
        .iter()
        .filter(|rel| !case_dir.join(rel).is_file())
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "case package is missing required files: {}",
            missing.join(", ")
        ))
    }
}

fn recursive_package_dirs() -> &'static [&'static str] {
    &[
        "evidence/logs",
        "artifacts/clips",
        "artifacts/joined",
        "artifacts/proxies",
        "artifacts/thumbnails",
        "artifacts/carved",
        "artifacts/recovered",
        "db/filesystem",
        "db/scan_runs",
        "review/thumbs",
        "qa",
        "reports",
    ]
}

fn excluded_package_path(path: &Path, output_dir: &Path) -> bool {
    if path == output_dir || path.join("package-manifest.json").is_file() {
        return true;
    }
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    name.starts_with('.')
        || name.starts_with("package_")
        || matches!(
            name.as_str(),
            "audit-key" | "audit-keys.json" | "secrets" | "credentials" | "credentials.json"
        )
        || matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("key" | "pem" | "p12" | "pfx")
        )
}

// Manifest paths must be portable: a package written on Windows and
// verified on Linux has to carry identical relative paths, so always
// serialize with '/' separators rather than the host Path separator.
fn rel_manifest_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_report_file(rel: &Path) -> bool {
    if rel.starts_with("reports") || rel.starts_with("qa") {
        matches!(
            rel.extension().and_then(|ext| ext.to_str()),
            Some(
                "md" | "html" | "json" | "jsonl" | "csv" | "tsv" | "xml" | "txt" | "pdf" | "dfxml"
            )
        )
    } else {
        true
    }
}

fn copy_package_dir(
    case_dir: &Path,
    output_dir: &Path,
    rel_dir: &Path,
    files: &mut Vec<PackageFile>,
    context: &mut PackageContext,
) -> Result<(), String> {
    let source_dir = case_dir.join(rel_dir);
    if !source_dir.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(&source_dir).map_err(|err| {
        format!(
            "failed to read package directory {}: {err}",
            source_dir.display()
        )
    })?;
    for entry in entries {
        let entry =
            entry.map_err(|err| format!("failed to read package directory entry: {err}"))?;
        let path = entry.path();
        let rel = rel_dir.join(entry.file_name());
        if excluded_package_path(&path, output_dir) {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|err| format!("failed to read package file type {}: {err}", path.display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "package input contains unsupported symlink: {}",
                path.display()
            ));
        }
        if file_type.is_dir() {
            copy_package_dir(case_dir, output_dir, &rel, files, context)?;
        } else if file_type.is_file() && is_report_file(&rel) {
            copy_package_file(case_dir, output_dir, &rel, files, context)?;
        }
    }
    Ok(())
}

fn copy_package_file(
    case_dir: &Path,
    output_dir: &Path,
    rel: &Path,
    files: &mut Vec<PackageFile>,
    context: &mut PackageContext,
) -> Result<(), String> {
    let source = case_dir.join(rel);
    let metadata = match fs::symlink_metadata(&source) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(format!(
                "failed to inspect package source {}: {err}",
                source.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "package source cannot be a symlink: {}",
            source.display()
        ));
    }
    if !metadata.is_file() {
        return Ok(());
    }
    let target = output_dir.join(rel);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create package directory: {err}"))?;
    }
    let redacted_copy = match context.redaction.as_ref() {
        Some(redaction) => Some(classify_redacted_copy(
            case_dir,
            rel,
            &source,
            &redaction.redactor,
        )?),
        None => None,
    };
    if let (Some(redaction), Some(copy)) = (context.redaction.as_mut(), &redacted_copy) {
        match copy {
            RedactedCopy::Rewrite(_) => redaction.rewritten.push(rel_manifest_path(rel)),
            RedactedCopy::Verbatim { listed: true } => {
                redaction.not_redacted.push(rel_manifest_path(rel))
            }
            RedactedCopy::Verbatim { listed: false } => {}
        }
    }
    if rel == Path::new("db/case.db") {
        let conn = crate::case_db::open_readonly_case_db(&source)?;
        conn.backup(rusqlite::MAIN_DB, &target, None)
            .map_err(|err| format!("failed to snapshot package SQLite database: {err}"))?;
    } else if let Some(RedactedCopy::Rewrite(text)) = &redacted_copy {
        write_text(&target, text).map_err(|err| {
            format!(
                "failed to write path-redacted package copy {}: {err}",
                target.display()
            )
        })?;
    } else {
        fs::copy(&source, &target).map_err(|err| {
            format!(
                "failed to copy package file {} to {}: {err}",
                source.display(),
                target.display()
            )
        })?;
    }
    files.push(PackageFile {
        relative_path: rel.to_path_buf(),
        sha256: audit::digest_file(&target)?,
        size_bytes: fs::metadata(&target)
            .map_err(|err| format!("failed to stat final package file: {err}"))?
            .len(),
    });
    Ok(())
}

/// The workstation's not-yet-promoted finalize sign-off (mirrors
/// `serve::jobs::SIGNOFF_PENDING_REL_PATH`).
pub const SIGNOFF_PENDING_REL_PATH: &str = "review/signoff.pending.json";

/// Reads a sign-off record (`None` when absent; a symlink is refused).
fn read_signoff_record(case_dir: &Path, rel: &str) -> Result<Option<Vec<u8>>, String> {
    let source = case_dir.join(rel);
    let metadata = match fs::symlink_metadata(&source) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(None),
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "package source cannot be a symlink: {}",
            source.display()
        ));
    }
    if !metadata.is_file() {
        return Ok(None);
    }
    fs::read(&source)
        .map(Some)
        .map_err(|err| format!("failed to read sign-off {}: {err}", source.display()))
}

/// The packaged sign-off: its case-relative source and the bytes read.
type ChosenSignoff = Option<(&'static str, Vec<u8>)>;

/// I10: which sign-off record to package. The packaged report names the
/// record it was rendered from (`report::signoff_source_marker`):
/// - the pending record only when the report was rendered from it (a
///   finalize in progress: make-report ran with `--signoff <pending>`, and
///   promotion happens only after this package succeeds);
/// - otherwise the promoted record. A pending record left by an
///   interrupted finalize is never packaged in its place — it is reported
///   as a warning, as is a report naming a different record.
fn choose_signoff(case_dir: &Path) -> Result<(ChosenSignoff, Vec<String>), String> {
    let promoted_rel = crate::report::SIGNOFF_REL_PATH;
    let pending = read_signoff_record(case_dir, SIGNOFF_PENDING_REL_PATH)?;
    let promoted = read_signoff_record(case_dir, promoted_rel)?;
    let report = fs::read_to_string(case_dir.join("reports/case-report.html")).ok();
    let named = report
        .as_deref()
        .and_then(crate::report::report_signoff_sha256)
        .map(str::to_string);
    let digest = |bytes: &[u8]| crate::sha256::digest_bytes(bytes);
    let mut warnings = Vec::new();
    if let Some(text) = &pending
        && named.as_deref() == Some(digest(text).as_str())
    {
        return Ok((
            Some((SIGNOFF_PENDING_REL_PATH, pending.unwrap_or_default())),
            warnings,
        ));
    }
    if pending.is_some() {
        warnings.push(format!(
            "{SIGNOFF_PENDING_REL_PATH} was not packaged: the packaged report was not rendered from it (left over from an interrupted finalize?). {}",
            if promoted.is_some() {
                format!("The promoted {promoted_rel} was packaged instead.")
            } else {
                "No sign-off record was packaged.".to_string()
            }
        ));
    }
    let Some(promoted) = promoted else {
        return Ok((None, warnings));
    };
    if let Some(named) = &named
        && named != &digest(&promoted)
    {
        warnings.push(format!(
            "reports/case-report.html names a different sign-off record than the packaged {promoted_rel}; regenerate the report (`frametrace make-report <case-dir>`) and package again"
        ));
    }
    Ok((Some((promoted_rel, promoted)), warnings))
}

/// Packages the chosen sign-off ([`choose_signoff`]) as
/// `review/signoff.json`. Returns the case-relative source, which the
/// manifest records as `signoff_source`, and the warnings.
fn copy_signoff(
    case_dir: &Path,
    output_dir: &Path,
    files: &mut Vec<PackageFile>,
) -> Result<(Option<&'static str>, Vec<String>), String> {
    let promoted = crate::report::SIGNOFF_REL_PATH;
    let (chosen, warnings) = choose_signoff(case_dir)?;
    if let Some((source_rel, bytes)) = chosen {
        let source = case_dir.join(source_rel);
        let target = output_dir.join(promoted);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create package directory: {err}"))?;
        }
        // The bytes compared with the report's marker are the bytes packaged.
        fs::write(&target, &bytes).map_err(|err| {
            format!(
                "failed to copy sign-off {} into the package: {err}",
                source.display()
            )
        })?;
        files.push(PackageFile {
            relative_path: PathBuf::from(promoted),
            sha256: audit::digest_file(&target)?,
            size_bytes: fs::metadata(&target)
                .map_err(|err| format!("failed to stat packaged sign-off: {err}"))?
                .len(),
        });
        return Ok((Some(source_rel), warnings));
    }
    Ok((None, warnings))
}

/// Outcome of one `verify-package` check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Verified and consistent.
    Pass,
    /// Verified and inconsistent: the package is not what it claims.
    Fail,
    /// Could not be verified from the package alone; stated, not implied.
    NotVerified,
}

impl CheckStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::NotVerified => "NOT VERIFIED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCheck {
    pub status: CheckStatus,
    /// Package-relative file (or area) the check is about.
    pub subject: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct PackageVerification {
    pub checks: Vec<PackageCheck>,
    /// SHA-256 of `manifest.sha256` — compare with the value recorded at
    /// finalize; nothing inside the package can authenticate itself.
    pub manifest_sha256: Option<String>,
    pub files_verified: usize,
}

impl PackageVerification {
    pub fn failures(&self) -> impl Iterator<Item = &PackageCheck> {
        self.checks
            .iter()
            .filter(|check| check.status == CheckStatus::Fail)
    }

    pub fn passed(&self) -> bool {
        self.failures().next().is_none()
    }

    fn push(&mut self, status: CheckStatus, subject: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(PackageCheck {
            status,
            subject: subject.into(),
            detail: detail.into(),
        });
    }
}

/// Files `package_case` writes after (or outside) `manifest.sha256`.
const UNLISTED_PACKAGE_FILES: &[&str] = &["manifest.sha256", "package-manifest.json", "README.txt"];

/// Rejects manifest paths that could escape the package or are not the
/// portable '/'-separated relative form `package_case` writes.
fn safe_manifest_path(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains('\\')
        && !rel.contains(':')
        && rel
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn is_lower_hex_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Verifies a package written by [`package_case`] using only its own
/// contents (FOR-13). Checks, each reported with what it covers:
/// 1. every `manifest.sha256` entry names a regular file whose SHA-256
///    matches, and no other file is present (besides the manifest files
///    and README);
/// 2. `package-manifest.json` agrees with `manifest.sha256` and on-disk sizes;
/// 3. every packaged hash-chained `.jsonl` log verifies as a chain;
/// 4. the packaged SQLite snapshot passes `integrity_check`;
/// 5. a packaged sign-off's `content_sha256` matches the review marks
///    (with their IN-OUT ranges) and tags in the packaged database, and the
///    packaged report names that examiner;
/// 6. a packaged `review/evidence-viewer.html` has its `review/data-bundle.js`
///    sibling (NOT VERIFIED otherwise — older packages; package-case
///    refuses to create one), and a non-empty carve log comes with
///    `review/carve-report.html` (NOT VERIFIED otherwise).
///
/// Returns `Err` only when the directory cannot be inspected at all; every
/// finding (including failures) is a check in the result.
pub fn verify_package(package_dir: &Path) -> Result<PackageVerification, String> {
    if !package_dir.is_dir() {
        return Err(format!(
            "package directory not found: {}",
            package_dir.display()
        ));
    }
    let mut out = PackageVerification::default();
    let manifest_path = package_dir.join("manifest.sha256");
    let manifest_text = match fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(err) => {
            out.push(
                CheckStatus::Fail,
                "manifest.sha256",
                format!("cannot read the checksum manifest: {err}"),
            );
            return Ok(out);
        }
    };
    out.manifest_sha256 = audit::digest_file(&manifest_path).ok();

    // 1. manifest.sha256 entries vs files.
    let mut listed: std::collections::BTreeMap<String, String> = Default::default();
    let mut manifest_ok = true;
    let body = manifest_text.strip_suffix('\n').unwrap_or(&manifest_text);
    if !manifest_text.is_empty() && !manifest_text.ends_with('\n') {
        manifest_ok = false;
        out.push(
            CheckStatus::Fail,
            "manifest.sha256",
            "last line is not newline-terminated (truncated or appended)",
        );
    }
    for (index, line) in body.split('\n').enumerate() {
        if manifest_text.is_empty() {
            break;
        }
        let parsed = line
            .split_once("  ")
            .filter(|(hash, rel)| is_lower_hex_sha256(hash) && safe_manifest_path(rel));
        let Some((hash, rel)) = parsed else {
            manifest_ok = false;
            out.push(
                CheckStatus::Fail,
                "manifest.sha256",
                format!(
                    "line {} is not `<sha256>  <relative path>` with a safe relative path: {line:?}",
                    index + 1
                ),
            );
            continue;
        };
        if listed.insert(rel.to_string(), hash.to_string()).is_some() {
            manifest_ok = false;
            out.push(
                CheckStatus::Fail,
                rel,
                "listed more than once in manifest.sha256",
            );
        }
    }
    for (rel, expected) in &listed {
        let path = package_dir.join(rel);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                out.push(
                    CheckStatus::Fail,
                    rel,
                    "listed in manifest.sha256 but is not a regular file (symlink or directory)",
                );
                continue;
            }
            Err(_) => {
                out.push(
                    CheckStatus::Fail,
                    rel,
                    "listed in manifest.sha256 but missing from the package",
                );
                continue;
            }
        }
        match audit::digest_file(&path) {
            Ok(actual) if &actual == expected => out.files_verified += 1,
            Ok(actual) => out.push(
                CheckStatus::Fail,
                rel,
                format!("SHA-256 mismatch: manifest.sha256 records {expected}, file is {actual}"),
            ),
            Err(err) => out.push(CheckStatus::Fail, rel, err),
        }
    }
    let mut present = Vec::new();
    collect_package_files(package_dir, Path::new(""), &mut present, &mut out);
    for rel in present {
        if !listed.contains_key(&rel) && !UNLISTED_PACKAGE_FILES.contains(&rel.as_str()) {
            out.push(
                CheckStatus::Fail,
                &rel,
                "present in the package but not listed in manifest.sha256 (added after packaging)",
            );
        }
    }
    if manifest_ok && out.files_verified == listed.len() {
        out.push(
            CheckStatus::Pass,
            "manifest.sha256",
            format!(
                "{} listed file(s) present with matching SHA-256; no unlisted files",
                listed.len()
            ),
        );
    }

    // 2. package-manifest.json vs manifest.sha256 and sizes.
    verify_package_manifest_json(package_dir, &listed, &mut out);

    // 3. Hash-chained logs.
    for rel in listed.keys().filter(|rel| rel.ends_with(".jsonl")) {
        let path = package_dir.join(rel);
        let text = fs::read_to_string(&path).unwrap_or_default();
        let chained = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .and_then(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .is_some_and(|value| {
                value.get("previous_entry_sha256").is_some() || value.get("entry_sha256").is_some()
            });
        if !chained {
            out.push(
                CheckStatus::NotVerified,
                rel,
                "not a hash-chained log; covered by manifest.sha256 only",
            );
            continue;
        }
        match audit::verify_chained_jsonl(&path) {
            Ok(result) => {
                let mut detail = format!(
                    "audit chain verified: {} entr{}, {}",
                    result.entries,
                    if result.entries == 1 { "y" } else { "ies" },
                    result.integrity.label()
                );
                for warning in &result.warnings {
                    detail.push_str(&format!("; warning: {warning}"));
                }
                out.push(CheckStatus::Pass, rel, detail);
            }
            Err(err) => out.push(CheckStatus::Fail, rel, format!("audit chain broken: {err}")),
        }
    }

    // 4 + 5. Database snapshot and sign-off.
    verify_package_db_and_signoff(package_dir, &listed, &mut out);

    // 6. Viewer sibling data and carve report.
    verify_review_siblings(package_dir, &listed, &mut out);

    out.push(
        CheckStatus::NotVerified,
        "authenticity",
        format!(
            "manifest.sha256 is not signed: these checks show the package is internally consistent, not who produced it. Compare its SHA-256 ({}) with the value recorded at finalize.",
            out.manifest_sha256.as_deref().unwrap_or("unavailable")
        ),
    );
    out.push(
        CheckStatus::NotVerified,
        "source evidence",
        "original evidence media are not in the package and were not re-hashed (see the packaged custody/report re-hash results)",
    );
    Ok(out)
}

fn verify_review_siblings(
    package_dir: &Path,
    listed: &std::collections::BTreeMap<String, String>,
    out: &mut PackageVerification,
) {
    if listed.contains_key(VIEWER_REL) {
        if listed.contains_key(DATA_BUNDLE_REL) {
            out.push(
                CheckStatus::Pass,
                VIEWER_REL,
                format!("standalone viewer ships with its {DATA_BUNDLE_REL}"),
            );
        } else {
            // NOT VERIFIED, not FAIL (contract §10.2): packages sealed
            // before the bundle was split out carry the viewer alone and
            // are otherwise intact; package-case itself still refuses to
            // seal a viewer without its bundle.
            out.push(
                CheckStatus::NotVerified,
                VIEWER_REL,
                format!(
                    "standalone viewer is packaged without {DATA_BUNDLE_REL}: it opens with no data, so the packaged review cannot be checked from the viewer (older package layout); review the packaged database and report instead"
                ),
            );
        }
    }
    let carve_log_has_entries = listed.contains_key(CARVE_LOG_REL)
        && fs::read_to_string(package_dir.join(CARVE_LOG_REL))
            .map(|text| text.lines().any(|line| !line.trim().is_empty()))
            .unwrap_or(true);
    if carve_log_has_entries && !listed.contains_key(CARVE_REPORT_REL) {
        out.push(
            CheckStatus::NotVerified,
            CARVE_REPORT_REL,
            "the package has carve log entries but no carve report; the carving results cannot be reviewed from the package (run make-review before packaging)",
        );
    }
}

fn collect_package_files(
    root: &Path,
    rel: &Path,
    present: &mut Vec<String>,
    out: &mut PackageVerification,
) {
    let Ok(entries) = fs::read_dir(root.join(rel)) else {
        out.push(
            CheckStatus::Fail,
            rel_manifest_path(rel),
            "directory could not be listed",
        );
        return;
    };
    for entry in entries.flatten() {
        let child = rel.join(entry.file_name());
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_package_files(root, &child, present, out);
        } else if file_type.is_file() {
            present.push(rel_manifest_path(&child));
        } else {
            out.push(
                CheckStatus::Fail,
                rel_manifest_path(&child),
                "symlink or special file inside the package",
            );
        }
    }
}

fn verify_package_manifest_json(
    package_dir: &Path,
    listed: &std::collections::BTreeMap<String, String>,
    out: &mut PackageVerification,
) {
    let subject = "package-manifest.json";
    let value = match fs::read_to_string(package_dir.join(subject))
        .map_err(|err| err.to_string())
        .and_then(|text| {
            serde_json::from_str::<serde_json::Value>(&text).map_err(|err| err.to_string())
        }) {
        Ok(value) => value,
        Err(err) => {
            out.push(CheckStatus::Fail, subject, format!("unreadable: {err}"));
            return;
        }
    };
    let Some(files) = value.get("files").and_then(|files| files.as_array()) else {
        out.push(CheckStatus::Fail, subject, "has no files array");
        return;
    };
    let mut ok = true;
    let mut seen = std::collections::BTreeSet::new();
    for file in files {
        let rel = file
            .get("relative_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let sha = file.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
        seen.insert(rel.to_string());
        match listed.get(rel) {
            None => {
                ok = false;
                out.push(
                    CheckStatus::Fail,
                    subject,
                    format!("lists {rel:?}, which manifest.sha256 does not"),
                );
            }
            Some(expected) if expected != sha => {
                ok = false;
                out.push(
                    CheckStatus::Fail,
                    subject,
                    format!("{rel}: sha256 {sha} disagrees with manifest.sha256 {expected}"),
                );
            }
            Some(_) => {
                let size = file.get("size_bytes").and_then(|v| v.as_u64());
                let actual = fs::metadata(package_dir.join(rel)).ok().map(|m| m.len());
                if size != actual {
                    ok = false;
                    out.push(
                        CheckStatus::Fail,
                        subject,
                        format!("{rel}: size_bytes {size:?} disagrees with the file ({actual:?})"),
                    );
                }
            }
        }
    }
    for rel in listed.keys().filter(|rel| !seen.contains(*rel)) {
        ok = false;
        out.push(
            CheckStatus::Fail,
            subject,
            format!("omits {rel}, which manifest.sha256 lists"),
        );
    }
    if value.get("file_count").and_then(|v| v.as_u64()) != Some(files.len() as u64) {
        ok = false;
        out.push(
            CheckStatus::Fail,
            subject,
            "file_count disagrees with the files array",
        );
    }
    if ok {
        out.push(
            CheckStatus::Pass,
            subject,
            "file list, digests and sizes agree with manifest.sha256 (the JSON itself is not covered by manifest.sha256)",
        );
    }
}

fn verify_package_db_and_signoff(
    package_dir: &Path,
    listed: &std::collections::BTreeMap<String, String>,
    out: &mut PackageVerification,
) {
    let db_rel = "db/case.db";
    let signoff_rel = crate::report::SIGNOFF_REL_PATH;
    if !listed.contains_key(db_rel) {
        out.push(
            CheckStatus::Fail,
            db_rel,
            "required case database is not in manifest.sha256",
        );
        return;
    }
    // Work on a private copy so verification never creates -wal/-shm
    // files (or anything else) inside the package being verified.
    static SCRATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let scratch = std::env::temp_dir().join(format!(
        "frametrace-verify-package-{}-{}-{}",
        std::process::id(),
        now_unix().unwrap_or(0),
        SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    type PackagedReview = (
        Vec<crate::case_db::ReviewMarkRow>,
        Vec<crate::selection::TagEntry>,
    );
    let marks = (|| -> Result<PackagedReview, String> {
        fs::create_dir_all(&scratch).map_err(|err| err.to_string())?;
        let copy = scratch.join("case.db");
        fs::copy(package_dir.join(db_rel), &copy).map_err(|err| err.to_string())?;
        let conn = crate::case_db::open_readonly_case_db(&copy)?;
        let check: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|err| format!("integrity_check failed to run: {err}"))?;
        if check != "ok" {
            return Err(format!("SQLite integrity_check: {check}"));
        }
        let history = crate::case_db::load_review_mark_history_from(&conn, None)?.len();
        let marks = crate::case_db::load_review_marks_from(&conn)?;
        let tags = crate::case_db::load_review_tags_from(&conn)?;
        out.push(
            CheckStatus::Pass,
            db_rel,
            format!(
                "SQLite integrity_check ok; {} review mark(s), {} review mark history row(s)",
                marks.len(),
                history
            ),
        );
        Ok((marks, tags))
    })();
    let _ = fs::remove_dir_all(&scratch);
    let (marks, tags) = match marks {
        Ok(review) => review,
        Err(err) => {
            out.push(CheckStatus::Fail, db_rel, err);
            return;
        }
    };

    let report_rel = "reports/case-report.html";
    let report = listed
        .contains_key(report_rel)
        .then(|| fs::read_to_string(package_dir.join(report_rel)).ok())
        .flatten();
    if !listed.contains_key(signoff_rel) {
        let detail = if report
            .as_deref()
            .is_some_and(|html| html.contains("검토자: <strong>"))
        {
            "no sign-off record in the package, although the packaged report names a reviewer; the sign-off cannot be checked"
        } else {
            "no sign-off record in the package (not finalized, or packaged before sign-off records were included)"
        };
        out.push(CheckStatus::NotVerified, signoff_rel, detail);
        return;
    }
    let text = fs::read_to_string(package_dir.join(signoff_rel)).unwrap_or_default();
    let Some((examiner, _)) = crate::report::parse_signoff(&text) else {
        out.push(
            CheckStatus::Fail,
            signoff_rel,
            "sign-off record is not valid (no examiner)",
        );
        return;
    };
    let digests = crate::report::SignoffDigests {
        content: crate::report::signoff_content_sha256(
            &crate::cli::handlers::signoff_content_json_from(&marks, &tags),
        ),
        legacy: crate::report::signoff_content_sha256(
            &crate::cli::handlers::review_marks_json_from_rows(&marks),
        ),
    };
    match crate::report::signoff_currency(&text, &digests) {
        crate::report::SignoffCurrency::Current => out.push(
            CheckStatus::Pass,
            signoff_rel,
            format!(
                "content_sha256 matches the review marks (status, note, IN-OUT range) and tags in the packaged db/case.db (examiner recorded as typed: {examiner}; not an authenticated identity)"
            ),
        ),
        crate::report::SignoffCurrency::Changed => out.push(
            CheckStatus::Fail,
            signoff_rel,
            format!(
                "content_sha256 does not match the review marks / tags in the packaged db/case.db (now {}): the sign-off does not cover this package's marks",
                digests.content
            ),
        ),
        crate::report::SignoffCurrency::LegacyBinding => out.push(
            CheckStatus::NotVerified,
            signoff_rel,
            "sign-off uses an older binding (schema_version < 3) that covers mark status and notes only; those still match, but whether IN-OUT ranges or tags changed after the sign-off cannot be checked — finalize again to bind them",
        ),
        crate::report::SignoffCurrency::Unbound => out.push(
            CheckStatus::NotVerified,
            signoff_rel,
            "sign-off predates content binding (no content_sha256); which marks it covers cannot be checked",
        ),
    }
    match report {
        Some(html) => {
            let banner = format!(
                "검토자: <strong>{}</strong>",
                crate::util::html_escape(&examiner)
            );
            if html.contains(&banner) {
                out.push(
                    CheckStatus::Pass,
                    report_rel,
                    "report names the same examiner as the sign-off record",
                );
            } else {
                out.push(
                    CheckStatus::Fail,
                    report_rel,
                    "report does not name the examiner recorded in review/signoff.json",
                );
            }
        }
        None => out.push(
            CheckStatus::NotVerified,
            report_rel,
            "no packaged report to compare with the sign-off",
        ),
    }
}

fn copy_optional_package_file(
    case_dir: &Path,
    output_dir: &Path,
    rel: &Path,
    files: &mut Vec<PackageFile>,
    missing_optional_files: &mut Vec<PathBuf>,
    context: &mut PackageContext,
) -> Result<(), String> {
    if !case_dir.join(rel).is_file() {
        missing_optional_files.push(rel.to_path_buf());
        return Ok(());
    }
    copy_package_file(case_dir, output_dir, rel, files, context)
}

#[allow(clippy::too_many_arguments)]
fn package_manifest_json(
    created_unix: u64,
    files: &[PackageFile],
    missing_optional_files: &[PathBuf],
    running_jobs: &[crate::case_db::RunningJob],
    quiescent: bool,
    signoff_source: Option<&str>,
    warnings: &[String],
    redaction: Option<&PackageRedaction>,
) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"schema_version\": 1,\n");
    out.push_str("  \"package_type\": \"frametrace-case-package\",\n");
    out.push_str(&format!("  \"created_unix\": {},\n", created_unix));
    out.push_str(&format!("  \"file_count\": {},\n", files.len()));
    out.push_str("  \"files\": [\n");
    for (index, file) in files.iter().enumerate() {
        out.push_str(&format!(
            "    {{\"relative_path\":\"{}\",\"size_bytes\":{},\"sha256\":\"{}\"}}",
            json_escape(&rel_manifest_path(&file.relative_path)),
            file.size_bytes,
            json_escape(&file.sha256)
        ));
        if index + 1 != files.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ],\n");
    out.push_str("  \"missing_optional_files\": [\n");
    for (index, rel) in missing_optional_files.iter().enumerate() {
        out.push_str(&format!("    \"{}\"", json_escape(&rel.to_string_lossy())));
        if index + 1 != missing_optional_files.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ],\n");
    out.push_str("  \"quiescence\": {\n");
    out.push_str(&format!("    \"quiescent\": {},\n", quiescent));
    out.push_str("    \"running_jobs\": [\n");
    for (index, job) in running_jobs.iter().enumerate() {
        out.push_str(&format!(
            "    {{\"job_id\":\"{}\",\"job_type\":\"{}\",\"subject_path\":\"{}\",\"started_unix\":{}}}",
            json_escape(&job.job_id),
            json_escape(&job.job_type),
            json_escape(&job.subject_path),
            job.started_unix
        ));
        if index + 1 != running_jobs.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("    ]\n");
    out.push_str("  },\n");
    let snapshot_scope = if quiescent {
        "Packaged while the case showed no running jobs; the multi-file set is a best-effort near-point-in-time snapshot, not a single atomic point-in-time snapshot. Files captured at different moments during packaging; re-run tools that modify the case after packaging invalidates it."
    } else {
        "Jobs were running when this package was created, so the multi-file set is explicitly NOT a point-in-time snapshot and not a single atomic point-in-time snapshot; files captured mid-write may be internally inconsistent. Re-package after jobs complete."
    };
    out.push_str(&format!(
        "  \"snapshot_scope\": \"{}\",\n",
        json_escape(snapshot_scope)
    ));
    out.push_str(&format!(
        "  \"signoff_source\": {},\n",
        signoff_source
            .map(|rel| format!("\"{}\"", json_escape(rel)))
            .unwrap_or_else(|| "null".to_string())
    ));
    out.push_str(&format!(
        "  \"warnings\": {},\n",
        serde_json::to_string(warnings).unwrap_or_else(|_| "[]".to_string())
    ));
    let path_redaction = match redaction {
        Some(redaction) => serde_json::json!({
            "applied": true,
            "rewritten_files": redaction.rewritten,
            "not_redacted_files": redaction.not_redacted,
            "note": "Rewritten files are path-redacted copies and differ from the case files (hashes recorded elsewhere for the originals do not match them). Files in not_redacted_files were copied byte-for-byte and may contain absolute paths: database, index files, hash-chained audit logs (never rewritten, to keep their chain hashes), binary media.",
        }),
        None => serde_json::json!({"applied": false}),
    };
    out.push_str(&format!("  \"path_redaction\": {path_redaction},\n"));
    let pdf_note = if files
        .iter()
        .any(|file| file.relative_path == Path::new("reports/case-report.html"))
    {
        "Open reports/case-report.html and print to PDF after examiner review."
    } else {
        "No HTML report is included; run `frametrace make-report <case-dir>` before packaging to produce one."
    };
    out.push_str(&format!("  \"pdf_ready_note\": \"{pdf_note}\"\n"));
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::package_case;

    mod verify {
        use super::super::{CheckStatus, PackageVerification, package_case, verify_package};
        use super::fixture;
        use std::fs;
        use std::path::{Path, PathBuf};

        fn failing_subjects(result: &PackageVerification) -> Vec<String> {
            result.failures().map(|c| c.subject.clone()).collect()
        }

        /// A case with a chained audit log, marks, a sign-off bound to
        /// those marks and a report naming the examiner — then packaged.
        fn signed_package(name: &str) -> (PathBuf, PathBuf, PathBuf) {
            let root = fixture(name);
            let case_dir = root.join("case");
            crate::audit::append_chained_jsonl_keyed(
                &case_dir.join("evidence/logs/scan-log.jsonl"),
                r#"{"kind":"one"}"#,
                None,
            )
            .unwrap();
            crate::audit::append_chained_jsonl_keyed(
                &case_dir.join("evidence/logs/scan-log.jsonl"),
                r#"{"kind":"two"}"#,
                None,
            )
            .unwrap();
            crate::case_db::upsert_review_marks(
                &case_dir,
                &[crate::case_db::ReviewMarkRow {
                    record_id: "vid_1".into(),
                    status: "important".into(),
                    marked_unix: 9,
                    examiner: Some("kim".into()),
                    ..Default::default()
                }],
            )
            .unwrap();
            let content = crate::report::signoff_content_sha256(
                &crate::cli::handlers::signoff_content_json(&case_dir).unwrap(),
            );
            fs::create_dir_all(case_dir.join("review")).unwrap();
            let pending = format!(
                "{{\"schema_version\":3,\"examiner\":\"홍길동\",\"signed_unix\":5,\"content_sha256\":\"{content}\"}}\n"
            );
            fs::write(case_dir.join("review/signoff.pending.json"), &pending).unwrap();
            fs::create_dir_all(case_dir.join("reports")).unwrap();
            // A report rendered from the pending record names it (I10).
            fs::write(
                case_dir.join("reports/case-report.html"),
                format!(
                    "<html><body>{}<div>검토자: <strong>홍길동</strong></div></body></html>",
                    crate::report::signoff_source_marker(&pending)
                ),
            )
            .unwrap();
            let output = root.join("package");
            package_case(&case_dir, Some(&output)).unwrap();
            (root, case_dir, output)
        }

        fn check<'a>(
            result: &'a PackageVerification,
            subject: &str,
        ) -> Option<&'a super::super::PackageCheck> {
            result.checks.iter().find(|c| c.subject == subject)
        }

        #[test]
        fn fresh_package_passes_and_states_what_is_not_verified() {
            let (root, _case, output) = signed_package("verify-fresh");
            let result = verify_package(&output).unwrap();
            assert!(result.passed(), "{:?}", result.checks);
            assert_eq!(
                check(&result, "evidence/logs/scan-log.jsonl")
                    .unwrap()
                    .status,
                CheckStatus::Pass
            );
            let signoff = check(&result, "review/signoff.json").unwrap();
            assert_eq!(signoff.status, CheckStatus::Pass, "{signoff:?}");
            assert_eq!(
                check(&result, "reports/case-report.html").unwrap().status,
                CheckStatus::Pass
            );
            assert_eq!(
                check(&result, "authenticity").unwrap().status,
                CheckStatus::NotVerified
            );
            // Unchained jsonl is reported as not verified, never as a pass.
            assert_eq!(
                check(&result, "db/videos.jsonl").unwrap().status,
                CheckStatus::NotVerified
            );
            let manifest: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(output.join("package-manifest.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(manifest["signoff_source"], "review/signoff.pending.json");
            // Verification wrote nothing into the package.
            assert!(!output.join("db/case.db-shm").exists());
            assert!(!output.join("db/case.db-wal").exists());
            fs::remove_dir_all(root).unwrap();
        }

        fn rewrite(path: &Path, from: &str, to: &str) {
            let text = fs::read_to_string(path).unwrap();
            assert!(text.contains(from), "{from} not in {}", path.display());
            fs::write(path, text.replacen(from, to, 1)).unwrap();
        }

        #[test]
        fn tampered_file_manifest_and_audit_line_fail_naming_the_file() {
            // Tampered packaged file.
            let (root, _case, output) = signed_package("verify-file");
            fs::write(output.join("case.json"), b"{\"forged\":true}").unwrap();
            let result = verify_package(&output).unwrap();
            assert!(failing_subjects(&result).contains(&"case.json".to_string()));
            assert!(
                result
                    .failures()
                    .any(|c| c.detail.contains("SHA-256 mismatch"))
            );
            fs::remove_dir_all(root).unwrap();

            // Tampered manifest.sha256 (digest edited for one entry).
            let (root, _case, output) = signed_package("verify-manifest");
            let manifest = output.join("manifest.sha256");
            let text = fs::read_to_string(&manifest).unwrap();
            let line = text
                .lines()
                .find(|line| line.ends_with("  db/video_paths.tsv"))
                .unwrap();
            rewrite(&manifest, &line[..64], &"0".repeat(64));
            let result = verify_package(&output).unwrap();
            let failing = failing_subjects(&result);
            assert!(
                failing.contains(&"db/video_paths.tsv".to_string()),
                "{failing:?}"
            );
            // package-manifest.json now disagrees with manifest.sha256 too.
            assert!(failing.contains(&"package-manifest.json".to_string()));
            fs::remove_dir_all(root).unwrap();

            // Tampered audit line, with manifest.sha256 "fixed up" to match:
            // the chain check still names the log.
            let (root, _case, output) = signed_package("verify-audit");
            let log = output.join("evidence/logs/scan-log.jsonl");
            let before = crate::audit::digest_file(&log).unwrap();
            rewrite(&log, "\"kind\":\"one\"", "\"kind\":\"forged\"");
            let after = crate::audit::digest_file(&log).unwrap();
            rewrite(&output.join("manifest.sha256"), &before, &after);
            let result = verify_package(&output).unwrap();
            let failure = result
                .failures()
                .find(|c| c.subject == "evidence/logs/scan-log.jsonl")
                .expect("audit log named");
            assert!(failure.detail.contains("audit chain broken"), "{failure:?}");
            fs::remove_dir_all(root).unwrap();

            // Junk appended after the last audit entry.
            let (root, _case, output) = signed_package("verify-junk");
            let log = output.join("evidence/logs/scan-log.jsonl");
            let mut text = fs::read_to_string(&log).unwrap();
            text.push_str("{\"kind\":\"appended\"}\n");
            fs::write(&log, text).unwrap();
            let result = verify_package(&output).unwrap();
            assert!(
                failing_subjects(&result).contains(&"evidence/logs/scan-log.jsonl".to_string())
            );
            fs::remove_dir_all(root).unwrap();
        }

        #[test]
        fn added_files_and_stale_signoff_fail() {
            let (root, case_dir, _output) = signed_package("verify-extra");
            let output = root.join("package");
            fs::write(output.join("reports/extra.html"), b"added later").unwrap();
            let result = verify_package(&output).unwrap();
            assert!(failing_subjects(&result).contains(&"reports/extra.html".to_string()));

            // Marks changed after the sign-off: a new package carrying the
            // old sign-off must not verify as signed for its content.
            fs::remove_file(case_dir.join("review/signoff.pending.json")).ok();
            crate::case_db::upsert_review_marks(
                &case_dir,
                &[crate::case_db::ReviewMarkRow {
                    record_id: "vid_2".into(),
                    status: "relevant".into(),
                    marked_unix: 10,
                    ..Default::default()
                }],
            )
            .unwrap();
            fs::write(
                case_dir.join("review/signoff.json"),
                fs::read_to_string(output.join("review/signoff.json")).unwrap(),
            )
            .unwrap();
            let second = root.join("package2");
            package_case(&case_dir, Some(&second)).unwrap();
            let result = verify_package(&second).unwrap();
            let failure = result
                .failures()
                .find(|c| c.subject == "review/signoff.json")
                .expect("stale sign-off fails");
            assert!(failure.detail.contains("does not cover"), "{failure:?}");
            fs::remove_dir_all(root).unwrap();
        }
    }
    use std::fs;

    fn manifest_of(output: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(output.join("package-manifest.json")).unwrap())
            .unwrap()
    }

    /// I10: a pending record left by an interrupted finalize never replaces
    /// the promoted sign-off the report was rendered from; it is a warning.
    #[test]
    fn stale_pending_signoff_is_not_packaged_over_the_promoted_one() {
        let root = fixture("stale-pending");
        let case_dir = root.join("case");
        fs::create_dir_all(case_dir.join("review")).unwrap();
        fs::create_dir_all(case_dir.join("reports")).unwrap();
        let promoted = "{\"schema_version\":3,\"examiner\":\"A\",\"signed_unix\":5}\n";
        let stale = "{\"schema_version\":3,\"examiner\":\"B\",\"signed_unix\":9}\n";
        fs::write(case_dir.join("review/signoff.json"), promoted).unwrap();
        fs::write(case_dir.join("review/signoff.pending.json"), stale).unwrap();
        fs::write(
            case_dir.join("reports/case-report.html"),
            format!(
                "<html><body>{}검토자: <strong>A</strong></body></html>",
                crate::report::signoff_source_marker(promoted)
            ),
        )
        .unwrap();
        let output = root.join("package");
        let result = package_case(&case_dir, Some(&output)).unwrap();
        assert_eq!(
            fs::read_to_string(output.join("review/signoff.json")).unwrap(),
            promoted
        );
        let manifest = manifest_of(&output);
        assert_eq!(manifest["signoff_source"], "review/signoff.json");
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(result.warnings[0].contains("signoff.pending.json was not packaged"));
        assert_eq!(manifest["warnings"][0], result.warnings[0].as_str());
        assert!(
            fs::read_to_string(output.join("README.txt"))
                .unwrap()
                .contains("WARNINGS")
        );

        // Only a pending record the report does not name: nothing packaged.
        fs::remove_file(case_dir.join("review/signoff.json")).unwrap();
        let second = root.join("package2");
        let result = package_case(&case_dir, Some(&second)).unwrap();
        assert!(!second.join("review/signoff.json").exists());
        assert!(manifest_of(&second)["signoff_source"].is_null());
        assert!(result.warnings[0].contains("No sign-off record was packaged"));

        // The report names the pending record (finalize in progress): it
        // is packaged, with no warning.
        fs::write(
            case_dir.join("reports/case-report.html"),
            format!(
                "<html><body>{}</body></html>",
                crate::report::signoff_source_marker(stale)
            ),
        )
        .unwrap();
        fs::write(case_dir.join("review/signoff.json"), promoted).unwrap();
        let third = root.join("package3");
        let result = package_case(&case_dir, Some(&third)).unwrap();
        assert_eq!(
            fs::read_to_string(third.join("review/signoff.json")).unwrap(),
            stale
        );
        assert_eq!(
            manifest_of(&third)["signoff_source"],
            "review/signoff.pending.json"
        );
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        fs::remove_dir_all(root).unwrap();
    }

    /// I4: a package built from `--redact-paths` pages also redacts the
    /// custody statement, report-side text documents and join manifests,
    /// copies chained logs and the database verbatim (chains intact), and
    /// lists every file it could not redact.
    #[test]
    fn redacted_package_covers_documents_and_lists_what_it_cannot_redact() {
        let root = fixture("redact");
        let case_dir = root.join("case");
        let secret = if cfg!(windows) {
            r"D:\Secret Evidence\cam1.mp4"
        } else {
            "/secret-evidence/cam1.mp4"
        };
        let case_text = case_dir.to_string_lossy().to_string();
        let redacted_page = format!(
            "<html><body>{}<p>redacted</p></body></html>",
            crate::redact::REDACTION_MARKER
        );
        fs::create_dir_all(case_dir.join("reports")).unwrap();
        fs::create_dir_all(case_dir.join("review")).unwrap();
        fs::write(case_dir.join("reports/case-report.html"), &redacted_page).unwrap();
        fs::write(case_dir.join("review/index.html"), &redacted_page).unwrap();
        fs::create_dir_all(case_dir.join("artifacts/joined")).unwrap();
        let manifest_rel = "artifacts/joined/join_x.manifest.json";
        fs::write(
            case_dir.join(manifest_rel),
            serde_json::to_string_pretty(&serde_json::json!({
                "kind": "frametrace-join-manifest",
                "segments": [{"source_path": secret}],
                "output": case_dir.join("artifacts/joined/join_x.mp4"),
            }))
            .unwrap(),
        )
        .unwrap();
        let log = case_dir.join("artifacts/joined/join-log.jsonl");
        crate::audit::append_chained_jsonl_keyed(
            &log,
            &serde_json::json!({"event": "join-video", "source": secret}).to_string(),
            None,
        )
        .unwrap();
        fs::write(
            case_dir.join("reports/notes.txt"),
            format!("exported from {secret} and kept in {case_text}\n"),
        )
        .unwrap();
        fs::write(
            case_dir.join("reports/known-hash-filter.json"),
            serde_json::json!({"source": secret, "note": format!("see {secret} again")})
                .to_string(),
        )
        .unwrap();
        crate::custody::generate(&case_dir).unwrap();
        let output = root.join("package");
        let result = package_case(&case_dir, Some(&output)).unwrap();
        assert!(result.path_redacted);

        let leaks = |rel: &str| {
            let text = fs::read_to_string(output.join(rel)).unwrap();
            text.contains("Secret") || text.contains("secret-evidence") || text.contains(&case_text)
        };
        for rel in [
            manifest_rel,
            "reports/notes.txt",
            "reports/known-hash-filter.json",
            crate::custody::CUSTODY_REL_PATH,
        ] {
            assert!(!leaks(rel), "{rel} still carries an absolute path");
        }
        let custody = fs::read_to_string(output.join(crate::custody::CUSTODY_REL_PATH)).unwrap();
        assert!(crate::redact::html_is_redacted(&custody));
        // The case copy of the statement is untouched (redaction is
        // package-only).
        assert!(!crate::redact::html_is_redacted(
            &fs::read_to_string(case_dir.join(crate::custody::CUSTODY_REL_PATH)).unwrap()
        ));
        // Chained log: byte-identical, chain still verifies, listed.
        let packaged_log = output.join("artifacts/joined/join-log.jsonl");
        assert_eq!(fs::read(&packaged_log).unwrap(), fs::read(&log).unwrap());
        crate::audit::verify_chained_jsonl(&packaged_log).unwrap();
        let manifest = manifest_of(&output);
        let listed = |key: &str| -> Vec<String> {
            manifest["path_redaction"][key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(manifest["path_redaction"]["applied"], true);
        let not_redacted = listed("not_redacted_files");
        for rel in [
            "db/case.db",
            "artifacts/joined/join-log.jsonl",
            "db/videos.jsonl",
        ] {
            assert!(
                not_redacted.contains(&rel.to_string()),
                "{rel}: {not_redacted:?}"
            );
        }
        let rewritten = listed("rewritten_files");
        assert!(
            rewritten.contains(&manifest_rel.to_string()),
            "{rewritten:?}"
        );
        assert!(!rewritten.contains(&"reports/case-report.html".to_string()));
        let readme = fs::read_to_string(output.join("README.txt")).unwrap();
        assert!(readme.contains("FILES NOT PATH-REDACTED"));
        assert!(readme.contains("- db/case.db"));
        assert!(readme.contains("- artifacts/joined/join-log.jsonl"));
        // The redacted package still verifies.
        let verification = super::verify_package(&output).unwrap();
        assert!(verification.passed(), "{:?}", verification.checks);
        fs::remove_dir_all(root).unwrap();
    }

    /// I4: a redacted report next to an unredacted review would leak the
    /// paths the report hides — refused, naming both.
    #[test]
    fn mixed_redaction_state_is_refused() {
        let root = fixture("redact-mixed");
        let case_dir = root.join("case");
        fs::create_dir_all(case_dir.join("reports")).unwrap();
        fs::create_dir_all(case_dir.join("review")).unwrap();
        fs::write(
            case_dir.join("reports/case-report.html"),
            format!(
                "<html><body>{}</body></html>",
                crate::redact::REDACTION_MARKER
            ),
        )
        .unwrap();
        fs::write(
            case_dir.join("review/index.html"),
            "<html><body></body></html>",
        )
        .unwrap();
        let err = package_case(&case_dir, Some(&root.join("package"))).unwrap_err();
        assert!(err.contains("reports/case-report.html"), "{err}");
        assert!(err.contains("review/index.html"), "{err}");
        assert!(!root.join("package").exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("frametrace-package-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        fs::create_dir_all(case_dir.join("db")).unwrap();
        fs::write(case_dir.join("case.json"), b"{}").unwrap();
        fs::write(case_dir.join("db/video_index.json"), b"{}").unwrap();
        fs::write(case_dir.join("db/videos.jsonl"), b"").unwrap();
        fs::write(case_dir.join("db/video_paths.tsv"), b"id\tsource_path\n").unwrap();
        let conn = crate::case_db::open_case_db(&case_dir).unwrap();
        crate::case_db::init_schema(&conn).unwrap();
        root
    }

    #[test]
    fn wal_snapshot_preserves_committed_rows_and_final_file_metadata() {
        let root = fixture("wal");
        let case_dir = root.join("case");
        let output = root.join("package");
        let conn = rusqlite::Connection::open(case_dir.join("db/case.db")).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE snapshot_rows (value TEXT); INSERT INTO snapshot_rows VALUES ('committed');").unwrap();
        assert!(case_dir.join("db/case.db-wal").metadata().unwrap().len() > 0);
        package_case(&case_dir, Some(&output)).unwrap();
        let backup = rusqlite::Connection::open(output.join("db/case.db")).unwrap();
        assert_eq!(
            backup
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let rows = backup
            .prepare("SELECT value FROM snapshot_rows")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec!["committed"]);
        drop(backup);
        drop(conn);
        let manifest: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(output.join("package-manifest.json")).unwrap(),
        )
        .unwrap();
        for file in manifest["files"].as_array().unwrap() {
            let path = output.join(file["relative_path"].as_str().unwrap());
            assert_eq!(
                file["size_bytes"].as_u64().unwrap(),
                path.metadata().unwrap().len()
            );
            assert_eq!(
                file["sha256"].as_str().unwrap(),
                crate::audit::digest_file(&path).unwrap()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_includes_work_products_without_previous_packages_or_private_inputs() {
        let root = fixture("products");
        let case_dir = root.join("case");
        let products = [
            "artifacts/logs/batch-log.jsonl",
            "artifacts/joined/join_F_20260910-120000_n2.mp4",
            "artifacts/joined/join_F_20260910-120000_n2.manifest.json",
            "artifacts/joined/join-log.jsonl",
            "db/scan_runs/run_1.json",
            "db/timeline.jsonl",
            "review/thumbs/vid_1.jpg",
            "review/thumbs.json",
            "reports/qa/consistency-report.json",
            "reports/qa/report-defense-checklist.md",
            "qa/accuracy-report.html",
            "reports/comparison.json",
            "reports/timeline.csv",
            "reports/dfxml.xml",
            "reports/nested/export.tsv",
        ];
        let excluded = [
            "evidence/raw/original.raw",
            "evidence/originals/source.mp4",
            "db/job-locks/job.lock",
            "artifacts/joined/.join-tag1.ffconcat",
            "artifacts/joined/.join-tag1.part",
            "artifacts/joined/.join-tag1.progress.json",
            "reports/.env",
            "reports/private.key",
            "reports/audit-key",
            "reports/audit-keys.json",
            "reports/secrets/credentials.json",
            "reports/previous/package-manifest.json",
            "reports/previous/db/case.db",
        ];
        for rel in products.iter().chain(excluded.iter()) {
            let path = case_dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, rel).unwrap();
        }
        let first = package_case(&case_dir, None).unwrap();
        let second = package_case(&case_dir, None).unwrap();
        for output in [&first.output_dir, &second.output_dir] {
            let manifest: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(output.join("package-manifest.json")).unwrap(),
            )
            .unwrap();
            let listed = manifest["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|file| file["relative_path"].as_str().unwrap())
                .collect::<Vec<_>>();
            for rel in products {
                assert!(output.join(rel).is_file(), "missing {rel}");
                assert!(listed.contains(&rel), "not manifested: {rel}");
            }
            for rel in excluded {
                assert!(!output.join(rel).exists(), "included {rel}");
            }
            assert!(!listed.iter().any(|rel| rel.contains("package_")));
            assert_eq!(
                listed.len(),
                listed
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// Plan §1.9/§2.3: joined outputs, their manifest sidecar and the
    /// chained `join-log.jsonl` / `audio-level-log.jsonl` lanes travel in the
    /// package and verify; dot-prefixed `.join-*` temp files never do.
    #[test]
    fn package_carries_join_outputs_and_new_lanes_and_verifies() {
        let root = fixture("join-lanes");
        let case_dir = root.join("case");
        let joined = case_dir.join("artifacts/joined");
        fs::create_dir_all(&joined).unwrap();
        fs::write(joined.join("join_F_20260910-120000_n2.mp4"), b"mp4 bytes").unwrap();
        fs::write(
            joined.join("join_F_20260910-120000_n2.manifest.json"),
            b"{}",
        )
        .unwrap();
        for tmp in [
            ".join-t1.ffconcat",
            ".join-t1.part",
            ".join-t1.progress.json",
        ] {
            fs::write(joined.join(tmp), b"temp").unwrap();
        }
        crate::audit::append_chained_jsonl(
            &joined.join("join-log.jsonl"),
            r#"{"schema_version":1,"event":"join-video","joined_unix":1,"output_path":"artifacts/joined/join_F_20260910-120000_n2.mp4","segment_count":2}"#,
        )
        .unwrap();
        crate::audit::append_chained_jsonl(
            &case_dir.join("evidence/logs/audio-level-log.jsonl"),
            r#"{"schema_version":1,"event":"audio-level","measured_unix":1,"method":"ffmpeg-volumedetect","threshold_silent_max_dbfs":-70.0,"results":[]}"#,
        )
        .unwrap();
        let output = root.join("package");
        package_case(&case_dir, Some(&output)).unwrap();
        for rel in [
            "artifacts/joined/join_F_20260910-120000_n2.mp4",
            "artifacts/joined/join_F_20260910-120000_n2.manifest.json",
            "artifacts/joined/join-log.jsonl",
            "evidence/logs/audio-level-log.jsonl",
        ] {
            assert!(output.join(rel).is_file(), "missing {rel}");
        }
        for tmp in [
            ".join-t1.ffconcat",
            ".join-t1.part",
            ".join-t1.progress.json",
        ] {
            assert!(!output.join("artifacts/joined").join(tmp).exists());
        }
        let result = super::verify_package(&output).unwrap();
        assert!(result.passed(), "{:?}", result.checks);
        for lane in [
            "artifacts/joined/join-log.jsonl",
            "evidence/logs/audio-level-log.jsonl",
        ] {
            let check = result.checks.iter().find(|c| c.subject == lane).unwrap();
            assert_eq!(check.status, super::CheckStatus::Pass, "{check:?}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_records_running_jobs_and_snapshot_scope_limits() {
        let root = fixture("quiescence");
        let case_dir = root.join("case");
        let output = root.join("package");
        let job =
            crate::case_db::start_job(&case_dir, "scan-folder", &case_dir, None, "{}").unwrap();
        package_case(&case_dir, Some(&output)).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(output.join("package-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest["quiescence"]["quiescent"],
            serde_json::json!(false)
        );
        assert!(
            manifest["quiescence"]["running_jobs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["job_type"] == "scan-folder")
        );
        assert!(
            manifest["snapshot_scope"]
                .as_str()
                .unwrap()
                .contains("not a single atomic point-in-time snapshot")
        );
        crate::case_db::complete_job(&case_dir, &job.job_id, 0, "done").unwrap();
        let output2 = root.join("package2");
        package_case(&case_dir, Some(&output2)).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(output2.join("package-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["quiescence"]["quiescent"], serde_json::json!(true));
        assert_eq!(
            manifest["quiescence"]["running_jobs"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creates_checksummed_package_directory() {
        let root =
            std::env::temp_dir().join(format!("frametrace-package-test-{}", std::process::id()));
        let case_dir = root.join("case");
        let output_dir = root.join("package");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(case_dir.join("db")).unwrap();
        fs::create_dir_all(case_dir.join("reports")).unwrap();
        fs::write(case_dir.join("case.json"), b"{}").unwrap();
        let conn = crate::case_db::open_case_db(&case_dir).unwrap();
        crate::case_db::init_schema(&conn).unwrap();
        drop(conn);
        fs::write(case_dir.join("db/video_index.json"), b"{}").unwrap();
        fs::write(case_dir.join("db/videos.jsonl"), b"").unwrap();
        fs::write(case_dir.join("db/video_paths.tsv"), b"id\tsource_path\n").unwrap();
        fs::write(case_dir.join("reports/case-report.html"), b"<html></html>").unwrap();
        fs::write(case_dir.join("reports/summary.md"), b"# Summary").unwrap();

        let result = package_case(&case_dir, Some(&output_dir)).unwrap();
        assert!(output_dir.join("case.json").is_file());
        assert!(output_dir.join("db/case.db").is_file());
        assert!(output_dir.join("reports/summary.md").is_file());
        assert!(output_dir.join("manifest.sha256").is_file());
        assert!(result.manifest_path.is_file());
        let manifest = fs::read_to_string(&result.manifest_path).unwrap();
        assert!(manifest.contains("\"missing_optional_files\""));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_missing_required_package_files() {
        let root = std::env::temp_dir().join(format!(
            "frametrace-package-required-test-{}",
            std::process::id()
        ));
        let case_dir = root.join("case");
        let output_dir = root.join("package");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(case_dir.join("case.json"), b"{}").unwrap();

        let err = package_case(&case_dir, Some(&output_dir)).unwrap_err();
        assert!(err.contains("missing required files"));
        assert!(!output_dir.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_output_inside_recursively_packaged_tree() {
        let root = std::env::temp_dir().join(format!(
            "frametrace-package-recursive-test-{}",
            std::process::id()
        ));
        let case_dir = root.join("case");
        let output_dir = case_dir.join("evidence/logs/package");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(case_dir.join("evidence/logs")).unwrap();

        let err = package_case(&case_dir, Some(&output_dir)).unwrap_err();
        assert!(err.contains("recursively packaged directory"));

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_package_inputs() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "frametrace-package-symlink-test-{}",
            std::process::id()
        ));
        let case_dir = root.join("case");
        let output_dir = root.join("package");
        let outside = root.join("outside.txt");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(case_dir.join("db")).unwrap();
        fs::create_dir_all(case_dir.join("evidence/logs")).unwrap();
        fs::write(case_dir.join("case.json"), b"{}").unwrap();
        let conn = crate::case_db::open_case_db(&case_dir).unwrap();
        crate::case_db::init_schema(&conn).unwrap();
        drop(conn);
        fs::write(case_dir.join("db/video_index.json"), b"{}").unwrap();
        fs::write(case_dir.join("db/videos.jsonl"), b"").unwrap();
        fs::write(case_dir.join("db/video_paths.tsv"), b"id\tsource_path\n").unwrap();
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, case_dir.join("evidence/logs/leak.txt")).unwrap();

        let err = package_case(&case_dir, Some(&output_dir)).unwrap_err();
        assert!(err.contains("unsupported symlink"));

        let _ = fs::remove_dir_all(root);
    }
}
