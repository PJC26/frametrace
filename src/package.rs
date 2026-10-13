use crate::audit;
use crate::util::{json_escape, now_unix, write_text};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PackageResult {
    pub output_dir: PathBuf,
    pub file_count: usize,
    pub manifest_path: PathBuf,
}

#[derive(Debug, Clone)]
struct PackageFile {
    relative_path: PathBuf,
    sha256: String,
    size_bytes: u64,
}

pub fn package_case(case_dir: &Path, output_dir: Option<&Path>) -> Result<PackageResult, String> {
    let created_unix = now_unix()?;
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
        copy_package_file(case_dir, &output_dir, Path::new(rel), &mut files)?;
    }
    for rel in optional_package_files() {
        copy_optional_package_file(
            case_dir,
            &output_dir,
            Path::new(rel),
            &mut files,
            &mut missing_optional_files,
        )?;
    }
    for rel_dir in recursive_package_dirs() {
        copy_package_dir(case_dir, &output_dir, Path::new(rel_dir), &mut files)?;
    }
    let signoff_source = copy_signoff(case_dir, &output_dir, &mut files)?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

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
    write_text(
        &output_dir.join("README.txt"),
        &format!(
            "FrameTrace case package\n\n{report_note} Verify package contents with manifest.sha256 before transfer (`frametrace verify-package <package-dir>` checks the manifest, audit chains and sign-off and reports what it could not verify).\n\nSnapshot scope: see snapshot_scope in package-manifest.json. For Amped FIVE or Magnet DVR Examiner handoff steps, see docs/COMMERCIAL_HANDOFF.md in the FrameTrace repository.\n"
        ),
    )
    .map_err(|err| format!("failed to write package README: {err}"))?;

    Ok(PackageResult {
        output_dir,
        file_count: files.len(),
        manifest_path,
    })
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
            copy_package_dir(case_dir, output_dir, &rel, files)?;
        } else if file_type.is_file() && is_report_file(&rel) {
            copy_package_file(case_dir, output_dir, &rel, files)?;
        }
    }
    Ok(())
}

fn copy_package_file(
    case_dir: &Path,
    output_dir: &Path,
    rel: &Path,
    files: &mut Vec<PackageFile>,
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
    if rel == Path::new("db/case.db") {
        let conn = crate::case_db::open_readonly_case_db(&source)?;
        conn.backup(rusqlite::MAIN_DB, &target, None)
            .map_err(|err| format!("failed to snapshot package SQLite database: {err}"))?;
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
const SIGNOFF_PENDING_REL_PATH: &str = "review/signoff.pending.json";

/// Packages the sign-off the packaged report was rendered with, as
/// `review/signoff.json`: during finalize that is the pending record
/// (make-report ran with `--signoff <pending>`, and promotion happens only
/// after this package succeeds); otherwise the promoted record. Returns the
/// case-relative source, which the manifest records as `signoff_source`.
fn copy_signoff(
    case_dir: &Path,
    output_dir: &Path,
    files: &mut Vec<PackageFile>,
) -> Result<Option<&'static str>, String> {
    let promoted = crate::report::SIGNOFF_REL_PATH;
    for source_rel in [SIGNOFF_PENDING_REL_PATH, promoted] {
        let source = case_dir.join(source_rel);
        let metadata = match fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "package source cannot be a symlink: {}",
                source.display()
            ));
        }
        if !metadata.is_file() {
            continue;
        }
        let target = output_dir.join(promoted);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create package directory: {err}"))?;
        }
        fs::copy(&source, &target).map_err(|err| {
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
        return Ok(Some(source_rel));
    }
    Ok(None)
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
) -> Result<(), String> {
    if !case_dir.join(rel).is_file() {
        missing_optional_files.push(rel.to_path_buf());
        return Ok(());
    }
    copy_package_file(case_dir, output_dir, rel, files)
}

fn package_manifest_json(
    created_unix: u64,
    files: &[PackageFile],
    missing_optional_files: &[PathBuf],
    running_jobs: &[crate::case_db::RunningJob],
    quiescent: bool,
    signoff_source: Option<&str>,
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
            fs::write(
                case_dir.join("review/signoff.pending.json"),
                format!(
                    "{{\"schema_version\":3,\"examiner\":\"홍길동\",\"signed_unix\":5,\"content_sha256\":\"{content}\"}}\n"
                ),
            )
            .unwrap();
            fs::create_dir_all(case_dir.join("reports")).unwrap();
            fs::write(
                case_dir.join("reports/case-report.html"),
                "<html><body><div>검토자: <strong>홍길동</strong></div></body></html>",
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
