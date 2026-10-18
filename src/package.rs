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
    /// Pre-D5 artifact files whose names embed the selector (often an
    /// absolute source path), shipped under a path-free name instead:
    /// (package path, SHA-256 of the withheld original name).
    renamed: Vec<(String, String)>,
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
    // D12: the viewer's data bundle carries every record's paths; it is
    // shipped verbatim in redacted mode, so its own marker must agree.
    if let Ok(js) = fs::read_to_string(case_dir.join(DATA_BUNDLE_REL)) {
        if crate::redact::bundle_is_redacted(&js) {
            redacted.push(DATA_BUNDLE_REL);
        } else {
            plain.push(DATA_BUNDLE_REL);
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

/// C2: builds the package in a hidden, pid-tagged sibling directory
/// (`.<name>.ft<pid>.part`, skipped by packaging and package listings)
/// and renames it to its final name only once complete; any failure
/// removes the temporary directory. A killed package-case therefore never
/// leaves a half-written `package_*` folder that looks like a deliverable
/// — the server sweeps the `.part` directory of the killed step.
pub fn package_case(case_dir: &Path, output_dir: Option<&Path>) -> Result<PackageResult, String> {
    package_case_named(case_dir, output_dir, None)
}

/// R13-S2 (P15): whether `name` may be passed as `package-case
/// --package-name` — `package_` followed by ASCII letters, digits, `_`
/// or `-` (no separator, no `..`), not an `_INCOMPLETE` name.
pub fn valid_reserved_package_name(name: &str) -> bool {
    name.len() <= 96
        && name.strip_prefix("package_").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        && !is_incomplete_package_name(name)
}

/// R13-S2 (P15): [`package_case`] that publishes under EXACTLY
/// `reports/<package_name>` (a finalize recorded that name in its
/// `finalize-begin` lane entry; a recovery marks only it). Fails when the
/// name is taken — never falls back to another name.
pub fn package_case_named(
    case_dir: &Path,
    output_dir: Option<&Path>,
    package_name: Option<&str>,
) -> Result<PackageResult, String> {
    if let Some(name) = package_name {
        if output_dir.is_some() {
            return Err("--package-name and --output cannot be combined".to_string());
        }
        if !valid_reserved_package_name(name) {
            return Err(format!(
                "--package-name {name}: not a package folder name (package_<letters, digits, _ or ->)"
            ));
        }
    }
    let created_unix = now_unix()?;
    let pid = std::process::id();
    let reports = case_dir.join("reports");
    let sweep = |parent: &Path| {
        for (path, result) in crate::join::sweep_stale_parts(parent) {
            if let Err(err) = result {
                eprintln!("warning: {err} ({})", path.display());
            }
        }
    };
    let (final_dir, work_dir) = match output_dir {
        Some(path) => {
            let final_dir = path.to_path_buf();
            if let Some(parent) = final_dir.parent() {
                sweep(parent);
            }
            let work_dir = crate::join::pid_part_path(&final_dir, pid);
            if work_dir.exists() {
                fs::remove_dir_all(&work_dir)
                    .map_err(|err| format!("failed to clear {}: {err}", work_dir.display()))?;
            }
            (final_dir, work_dir)
        }
        None => {
            sweep(&reports);
            // R6-S8: the pid-tagged work folder exists BEFORE the default
            // name is reserved, so a package-case killed at any point
            // leaves a `.package_<ts>.ft<pid>.part` naming its reservation
            // — the server's kill sweep and the next run's stale sweep
            // remove both. Reserving first leaked an empty `package_<ts>`
            // when the process died before the work folder existed.
            match package_name {
                Some(name) => {
                    reserve_package_dirs_with(&reports.join(name), pid, &reservation_rename, true)?
                }
                None => {
                    reserve_package_dirs(&reports.join(format!("package_{created_unix}")), pid)?
                }
            }
        }
    };
    // The default name was reserved by creating it; an explicit output
    // may exist only as an empty directory (checked below).
    let reserved_default = output_dir.is_none();
    let result = package_case_into(case_dir, output_dir, &work_dir, created_unix);
    let release_reservation = || {
        if reserved_default {
            crate::join::release_reservation(&final_dir, pid);
        }
    };
    let mut result = match result {
        Ok(result) => result,
        Err(err) => {
            let _ = fs::remove_dir_all(&work_dir);
            release_reservation();
            return Err(err);
        }
    };
    // The final name holds at most an empty directory (our reservation or
    // an empty explicit output); it is replaced by the finished package.
    // R7-S5: our reservation is released through its marker — one that no
    // longer names this process is not ours to replace.
    // The finished package is kept (marked, never swept) when the name
    // is held by something else now. R8-S5: a release that only FAILED
    // (removal kept failing — an antivirus or indexer handle) is not a
    // stolen name: publishing goes on, and the publish rename (retried,
    // clearing what is left of our reservation first) decides.
    if reserved_default
        && final_dir.exists()
        && crate::join::release_own_reservation(&final_dir, pid)
            == crate::join::ReservationRelease::NotOurs
    {
        let _ = fs::write(
            crate::join::preserved_part_marker(&work_dir),
            "FrameTrace: 이 폴더는 완성된 패키지입니다. 예약한 이름을 다른 것이 쓰고 있어 그대로 두었습니다.\nThis folder is a complete package; its reserved name is in use by something else, so it was kept as is.\n",
        );
        return Err(format!(
            "패키지 이름 {}의 예약이 더 이상 이 실행의 것이 아닙니다 — 완성된 패키지는 {}에 남겨 두었습니다.",
            final_dir.display(),
            work_dir.display()
        ));
    }
    if !reserved_default
        && final_dir.exists()
        && let Err(err) = fs::remove_dir(&final_dir)
    {
        let _ = fs::remove_dir_all(&work_dir);
        return Err(format!(
            "package output {} is not empty or cannot be replaced: {err}",
            final_dir.display()
        ));
    }
    // R5-S12: a finished package is never thrown away because one rename
    // failed (on Windows an antivirus scan or the indexer briefly holds a
    // handle inside the fresh folder): the rename is retried with backoff,
    // and if it still fails the complete work folder is KEPT, marked so
    // no stale sweep removes it, and its path is reported.
    let publish = |from: &Path, to: &Path| -> std::io::Result<()> {
        #[cfg(test)]
        if FAIL_PUBLISH_RENAMES.with(|left| {
            let n = left.get();
            left.set(n.saturating_sub(1));
            n > 0
        }) {
            return Err(std::io::Error::other("injected rename failure"));
        }
        if reserved_default {
            // R8-S5: what a failed release left of OUR reservation.
            crate::join::clear_own_reservation_remnant(to, pid);
        }
        fs::rename(from, to)
    };
    let mut reported = 0u64;
    let mut waiting = |waited: std::time::Duration| {
        // R11-S7: the workstation shows a step's stderr in its job log.
        let secs = waited.as_secs();
        if secs > reported {
            reported = secs;
            eprintln!("package-case: 패키지 폴더가 사용 중입니다, 재시도 {secs}초");
        }
    };
    if let Err(err) = rename_with_retry(&work_dir, &final_dir, &publish, &mut waiting) {
        // R6-S5: the marker sits BESIDE the kept folder, never inside it:
        // a file inside made the finished package fail verify-package
        // (an unlisted file), so keeping it "safe" broke it.
        let marker = crate::join::preserved_part_marker(&work_dir);
        let _ = fs::write(
            &marker,
            format!(
                "FrameTrace: 이 폴더는 완성된 패키지입니다. 최종 이름({})으로 바꾸지 못해 그대로 두었습니다.\nThis folder is a complete package that could not be renamed to its final name; it was kept as is.\n",
                final_dir.display()
            ),
        );
        return Err(format!(
            "완성된 패키지를 최종 이름 {}(으)로 바꾸지 못했습니다 ({err}) — 패키지는 지우지 않고 {}에 남겨 두었습니다. 다른 프로그램(백신·탐색기)이 폴더를 놓은 뒤 이름을 직접 바꾸거나 다시 패키징하십시오.",
            final_dir.display(),
            work_dir.display()
        ));
    }
    crate::util::sync_parent_directory(&final_dir);
    result.manifest_path = final_dir.join("package-manifest.json");
    result.output_dir = final_dir;
    Ok(result)
}

/// R6-S8: reserves a default package name under `base`'s parent —
/// `base`, then `base_001`, … — creating this process's pid-tagged work
/// folder for a candidate FIRST and the (empty) final-name reservation
/// second. Returns `(final, work)`; both exist.
fn reserve_package_dirs(base: &Path, pid: u32) -> Result<(PathBuf, PathBuf), String> {
    reserve_package_dirs_with(base, pid, &reservation_rename, false)
}

/// [`reserve_package_dirs`] with the placing rename injected (tests).
fn reserve_package_dirs_with(
    base: &Path,
    pid: u32,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
    exact: bool,
) -> Result<(PathBuf, PathBuf), String> {
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    let name = base
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "package".to_string());
    // R13-S2: an exact name (a finalize's reserved one) is tried alone.
    let attempts = if exact { 1 } else { 10_000u32 };
    for attempt in 0..attempts {
        let candidate = if attempt == 0 {
            base.to_path_buf()
        } else {
            parent.join(format!("{name}_{attempt:03}"))
        };
        // R12-S5: a name an unfinished finalize's marker still names (a
        // kept work folder meant to get it) is not reused — the marker
        // would make the new package read as incomplete.
        if candidate.exists() || finalize_incomplete_marker(&candidate).exists() {
            continue;
        }
        let work = crate::join::pid_part_path(&candidate, pid);
        if work.exists() {
            fs::remove_dir_all(&work)
                .map_err(|err| format!("failed to clear {}: {err}", work.display()))?;
        }
        fs::create_dir(&work)
            .map_err(|err| format!("failed to create {}: {err}", work.display()))?;
        // R7-S5: the reservation names its owner; sweeps release it only
        // for this pid (the work folder alone cannot prove the name is
        // ours — see `join::RESERVATION_MARKER`).
        // R8-S6: R7 created the folder and wrote the marker after it, so
        // for a moment the reservation was an EMPTY folder — which a
        // sweep treats as "owner died before marking" and removes, and
        // which a unix `rename` (another package-case publishing) silently
        // replaces. The marker is now written into a pid-tagged staging
        // folder that is renamed onto the name: the reservation appears
        // already marked, never empty; a rename onto an existing folder
        // fails on Windows and, the reservation being non-empty, on unix.
        let staging = crate::join::reservation_staging_path(&candidate, pid);
        let marked = (|| {
            if staging.exists() {
                fs::remove_dir_all(&staging)?;
            }
            fs::create_dir(&staging)?;
            fs::write(
                staging.join(crate::join::RESERVATION_MARKER),
                crate::join::reservation_marker_text(pid),
            )
        })();
        if let Err(err) = marked {
            let _ = fs::remove_dir_all(&staging);
            let _ = fs::remove_dir(&work);
            return Err(format!(
                "failed to mark package reservation {}: {err}",
                candidate.display()
            ));
        }
        match place_reservation_with(&staging, &candidate, rename) {
            Placement::Placed => return Ok((candidate, work)),
            // Taken meanwhile (another package-case): next name.
            Placement::Taken => {
                let _ = fs::remove_dir_all(&staging);
                let _ = fs::remove_dir(&work);
            }
            // R9-S3: any other failure fails the reservation now. R8 went
            // on to the next name, so a rename that kept failing for every
            // name (no permission to rename in the folder, a filter
            // driver) cost 10,000 names x the 1.85 s backoff - hours.
            Placement::Failed(err) => {
                let _ = fs::remove_dir_all(&staging);
                let _ = fs::remove_dir(&work);
                return Err(format!(
                    "패키지 폴더 이름 {}을(를) 예약하지 못했습니다 ({err}) — 폴더의 쓰기·이름 바꾸기 권한과 백신·동기화 프로그램을 확인한 뒤 다시 패키징하십시오.",
                    candidate.display()
                ));
            }
        }
    }
    if exact {
        return Err(format!(
            "패키지 이름 {}이(가) 이미 있어 예약하지 못했습니다 (--package-name은 다른 이름으로 바꾸지 않음)",
            base.display()
        ));
    }
    Err(format!(
        "no free package folder name next to {}",
        base.display()
    ))
}

/// R9-S3: what placing a reservation came to.
#[derive(Debug)]
enum Placement {
    Placed,
    /// The name exists (taken - never replaced).
    Taken,
    /// The rename still failed after the R5-S12 backoff.
    Failed(std::io::Error),
}

/// R8-S6: renames the marked `staging` folder onto `reserved`.
#[cfg(test)]
fn place_reservation(staging: &Path, reserved: &Path) -> bool {
    matches!(
        place_reservation_with(staging, reserved, &reservation_rename),
        Placement::Placed
    )
}

/// R8-S6: renames the marked `staging` folder onto `reserved` with
/// `rename`: [`Placement::Taken`] when the name exists
/// ([`reservation_rename`] refuses an existing target), else retried with
/// backoff (a fresh folder briefly held by an antivirus scan) and
/// [`Placement::Failed`] with the last error when it never succeeds.
///
/// R10-S4: retried for [`rename_retry_budget`] in all (R9: five attempts,
/// 1.85 s — shorter than a real-time antivirus scan of a fresh folder
/// holds it, so a transient ACCESS_DENIED failed the whole package).
fn place_reservation_with(
    staging: &Path,
    reserved: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Placement {
    let mut taken = false;
    let result = retry_within(
        rename_retry_budget(),
        &mut || {
            let result = rename(staging, reserved);
            taken = matches!(&result, Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists);
            result
        },
        &|err| err.kind() == std::io::ErrorKind::AlreadyExists,
    );
    match result {
        Ok(()) => Placement::Placed,
        Err(_) if taken => Placement::Taken,
        Err(err) => Placement::Failed(err),
    }
}

/// R8-S6: `rename(from, to)` that never replaces an existing `to`. The
/// existence check covers an empty pre-R8 reservation (a unix `rename`
/// would replace an empty folder); a reservation made by this code is
/// never empty, so a rename racing past the check fails on it (Windows:
/// the target exists; unix: the target is not empty).
fn reservation_rename(from: &Path, to: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} exists", to.display()),
        ));
    }
    match fs::rename(from, to) {
        Err(_) if fs::symlink_metadata(to).is_ok() => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} exists", to.display()),
        )),
        other => other,
    }
}

/// Delays between the R5-S12 rename attempts; after the last one, every
/// further attempt waits as long as the last (R10-S4).
const RENAME_BACKOFF_MS: [u64; 4] = [100, 250, 500, 1000];

/// R10-S4: how long a rename that keeps failing (an antivirus, indexer or
/// sync client holding a fresh folder or file on Windows) is retried in
/// all before the operation gives up.
pub(crate) const RENAME_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(12);

#[cfg(test)]
thread_local! {
    /// Test hook: a shorter [`RENAME_RETRY_BUDGET`] on this thread.
    pub(crate) static RENAME_BUDGET_OVERRIDE: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

/// [`RENAME_RETRY_BUDGET`] (tests may shorten it per thread).
pub(crate) fn rename_retry_budget() -> std::time::Duration {
    #[cfg(test)]
    if let Some(budget) = RENAME_BUDGET_OVERRIDE.with(|cell| cell.get()) {
        return budget;
    }
    RENAME_RETRY_BUDGET
}

/// R10-S4: runs `op` until it succeeds, fails with an error `final_err`
/// accepts as final, or `budget` is spent — always at least five attempts
/// (the R5-S12 backoff), then one per second while the budget lasts. The
/// last error is returned.
pub(crate) fn retry_within(
    budget: std::time::Duration,
    op: &mut dyn FnMut() -> std::io::Result<()>,
    final_err: &dyn Fn(&std::io::Error) -> bool,
) -> std::io::Result<()> {
    retry_within_notify(budget, op, final_err, &mut |_| {})
}

/// R11-S7: [`retry_within`] that calls `waiting(elapsed)` before each wait
/// (callers log "file in use, retrying n s" — a silent 12 s wait looked
/// like a hang).
pub(crate) fn retry_within_notify(
    budget: std::time::Duration,
    op: &mut dyn FnMut() -> std::io::Result<()>,
    final_err: &dyn Fn(&std::io::Error) -> bool,
    waiting: &mut dyn FnMut(std::time::Duration),
) -> std::io::Result<()> {
    let started = std::time::Instant::now();
    let mut attempt = 0usize;
    loop {
        let err = match op() {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };
        if final_err(&err) {
            return Err(err);
        }
        let delay = std::time::Duration::from_millis(
            RENAME_BACKOFF_MS[attempt.min(RENAME_BACKOFF_MS.len() - 1)],
        );
        if attempt >= RENAME_BACKOFF_MS.len() && started.elapsed() + delay > budget {
            return Err(err);
        }
        waiting(started.elapsed() + delay);
        std::thread::sleep(delay);
        attempt += 1;
    }
}

/// R11-S7: a rename error waiting cannot cure — the source (or the
/// target's folder) does not exist. R10 retried even that for the whole
/// budget (12 s) before reporting it.
pub(crate) fn is_final_rename_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::NotFound
}

/// R5-S12: `rename(from, to)` retried with backoff; R10-S4: for
/// [`rename_retry_budget`] in all. R11-S7: NotFound is final
/// ([`is_final_rename_error`]) and each wait is reported to `waiting`.
/// The last error is returned when every attempt failed.
pub(crate) fn rename_with_retry(
    from: &Path,
    to: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
    waiting: &mut dyn FnMut(std::time::Duration),
) -> std::io::Result<()> {
    retry_within_notify(
        rename_retry_budget(),
        &mut || rename(from, to),
        &is_final_rename_error,
        waiting,
    )
}

#[cfg(test)]
thread_local! {
    /// Test hook: how many package publish renames fail before one works.
    static FAIL_PUBLISH_RENAMES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn package_case_into(
    case_dir: &Path,
    requested_output: Option<&Path>,
    work_dir: &Path,
    created_unix: u64,
) -> Result<PackageResult, String> {
    let mut context = PackageContext {
        redaction: package_redaction_mode(case_dir)?.then(|| PackageRedaction {
            redactor: crate::redact::Redactor::new(case_dir),
            rewritten: Vec::new(),
            not_redacted: Vec::new(),
            renamed: Vec::new(),
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
    if let Some(path) = requested_output {
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
    }
    // Everything is written into the in-progress directory (C2).
    let output_dir = work_dir.to_path_buf();
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
        redaction.renamed.sort();
    }

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
    // D10: README.txt is covered by manifest.sha256 like every other
    // packaged file (only the two manifests themselves are unlisted).
    let readme_path = output_dir.join(README_REL);
    write_text(&readme_path, &readme)
        .map_err(|err| format!("failed to write package README: {err}"))?;
    files.push(PackageFile {
        relative_path: PathBuf::from(README_REL),
        sha256: audit::digest_file(&readme_path)?,
        size_bytes: fs::metadata(&readme_path)
            .map_err(|err| format!("failed to stat package README: {err}"))?
            .len(),
    });
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
        &warnings,
        context.redaction.as_ref(),
    );
    let manifest_path = output_dir.join("package-manifest.json");
    write_text(&manifest_path, &manifest_json)
        .map_err(|err| format!("failed to write package manifest: {err}"))?;

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
    if !redaction.renamed.is_empty() {
        out.push_str("\nRENAMED ARTIFACT FILES\nThese proxy/thumbnail files were written before path-free artifact names and their case file names embed the source selector (often an absolute path). They are packaged under path-free names; the original name is withheld and identified only by its SHA-256. Chained logs (e.g. proxy-log.jsonl) still record the original names and are listed above as not path-redacted.\n");
        for (rel, name_sha256) in &redaction.renamed {
            out.push_str(&format!(
                "- {rel} (original file name SHA-256 {name_sha256})\n"
            ));
        }
    }
    out
}

/// D5 in packages: a pre-D5 proxy / thumbnail name
/// (`<sanitize(selector)>_proxy_<ts>.mp4`, `<sanitize(selector)>_thumb_<ts>.jpg`)
/// embeds the selector — often an absolute source path. A path-redacted
/// package ships such a file as `legacy_<kind>_<sha256(name)[..12]>.<ext>`.
/// `None` for every other file (path-free `proxy_<id>_<hash8>` names
/// included).
fn legacy_artifact_package_rel(rel: &Path) -> Option<(PathBuf, String)> {
    let parent = rel.parent()?;
    let name = rel.file_name()?.to_string_lossy().into_owned();
    let (kind, marker, ext) = if parent == Path::new("artifacts/proxies") {
        ("proxy", "_proxy_", ".mp4")
    } else if parent == Path::new("artifacts/thumbnails") {
        ("thumb", "_thumb_", ".jpg")
    } else {
        return None;
    };
    if name.starts_with(&format!("{kind}_")) || !name.contains(marker) || !name.ends_with(ext) {
        return None;
    }
    let name_sha256 = crate::sha256::digest_bytes(name.as_bytes());
    Some((
        parent.join(format!("legacy_{kind}_{}{ext}", &name_sha256[..12])),
        name_sha256,
    ))
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
    // D5: a redacted package never ships a file name that embeds a path.
    let renamed = context
        .redaction
        .as_ref()
        .and_then(|_| legacy_artifact_package_rel(rel));
    let package_rel = renamed
        .as_ref()
        .map(|(package_rel, _)| package_rel.clone())
        .unwrap_or_else(|| rel.to_path_buf());
    let target = output_dir.join(&package_rel);
    if renamed.is_some() && target.exists() {
        return Err(format!(
            "package-case refused: two case files map to the package path {}",
            rel_manifest_path(&package_rel)
        ));
    }
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
            RedactedCopy::Rewrite(_) => redaction.rewritten.push(rel_manifest_path(&package_rel)),
            RedactedCopy::Verbatim { listed: true } => {
                redaction.not_redacted.push(rel_manifest_path(&package_rel))
            }
            RedactedCopy::Verbatim { listed: false } => {}
        }
        if let Some((_, name_sha256)) = &renamed {
            redaction
                .renamed
                .push((rel_manifest_path(&package_rel), name_sha256.clone()));
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
        relative_path: package_rel,
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
    /// R10-S2: the package is marked as written by a finalize that did not
    /// complete ([`finalize_incomplete_reason`]); reported as a NOT
    /// VERIFIED check with subject [`FINALIZE_CHECK_SUBJECT`].
    pub finalize_incomplete: bool,
    /// R13-S12: the packaged sign-off's audit-lane entry matched, but only
    /// structurally — no HMAC was checked here (a keyless case, or a PC
    /// without the key): the line passes, `--strict` exits 3.
    pub signoff_unauthenticated: bool,
}

/// R10-S2: the `verify-package` check subject of the finalize state.
pub const FINALIZE_CHECK_SUBJECT: &str = "finalize";

/// R10-S2: what a finalize appends to the name of a package folder it
/// published but could not complete (the sign-off was not promoted).
/// Matched case-insensitively; a second incomplete package of the same
/// name gets `_INCOMPLETE_2`, `_INCOMPLETE_3`, ….
pub const INCOMPLETE_PACKAGE_SUFFIX: &str = "_INCOMPLETE";

/// R10-S2 (R9-S4): the marker file name suffix written BESIDE such a
/// package (`<folder name>.finalize-incomplete.txt`; inside it would be
/// an unlisted file and fail verify-package, R6-S5).
pub const FINALIZE_INCOMPLETE_MARKER_SUFFIX: &str = ".finalize-incomplete.txt";

/// R10-S2: the marker beside `package_dir`.
pub fn finalize_incomplete_marker(package_dir: &Path) -> PathBuf {
    let mut name = package_dir
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(FINALIZE_INCOMPLETE_MARKER_SUFFIX);
    package_dir.with_file_name(name)
}

/// R11-S2: what a folder copy appends to a name — Windows Explorer's
/// ` - 복사본` / ` - Copy` (any case; R12-S10: the other [`COPY_TAILS`]
/// too) and ` (n)`, as often as copies of
/// copies add them — with the trailing dots and spaces Windows drops from
/// a name. Returns the name without them.
pub fn strip_copy_tails(name: &str) -> &str {
    let mut rest = name;
    loop {
        let trimmed = rest.trim_end_matches([' ', '.']);
        let next = strip_numbered_tail(trimmed).or_else(|| {
            COPY_TAILS
                .iter()
                .find_map(|tail| strip_suffix_ignore_case(trimmed, tail))
        });
        match next {
            Some(shorter) => rest = shorter,
            None => return trimmed,
        }
    }
}

/// R11-S2 / R12-S10 / R13-S13: the copy tails Windows Explorer appends in
/// the display languages an examiner's PC is likely to use — Korean,
/// English, German, Japanese, Chinese (Simplified and Traditional), French,
/// Spanish/Italian, Portuguese, Russian — matched case-insensitively with
/// Unicode lowercase (R12 compared ASCII case only, so ` - КОПИЯ` or
/// ` - CÓPIA` was missed). Other locales' tails (and copies made by other
/// tools) are NOT recognized: the marker beside the original then only
/// helps for a copy made next to it, and a copied folder keeps the
/// `_INCOMPLETE` in its own name only when the tail comes after it.
const COPY_TAILS: &[&str] = &[
    " - 복사본",
    " - copy",
    " - kopie",
    " - コピー",
    " - 副本",
    " - 複製",
    " - copie",
    " - copia",
    " - cópia",
    " - копия",
];

/// ` (n)` at the end of `name` (n = one or more ASCII digits).
fn strip_numbered_tail(name: &str) -> Option<&str> {
    let inner = name.strip_suffix(')')?;
    let open = inner.rfind(" (")?;
    let digits = &inner[open + 2..];
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| &name[..open])
}

/// R13-S13: `name` without `suffix` (a lowercase tail), compared by
/// Unicode lowercase character by character — the tail of `name` is the
/// same number of characters as `suffix`, whatever its byte length.
fn strip_suffix_ignore_case<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let chars = suffix.chars().count();
    let cut = name
        .char_indices()
        .rev()
        .nth(chars.checked_sub(1)?)
        .map(|(at, _)| at)?;
    let tail = &name[cut..];
    (tail.to_lowercase() == suffix.to_lowercase()).then(|| &name[..cut])
}

/// R10-S2: whether a folder NAME marks an incomplete finalize
/// (`…_INCOMPLETE` or `…_INCOMPLETE_<n>`, any case). R11-S2: also with
/// copy tails ([`strip_copy_tails`]: `…_INCOMPLETE - 복사본`,
/// `…_INCOMPLETE (2)`, `…_INCOMPLETE_2 - Copy (3)`) and trailing dots or
/// spaces — a copy of an incomplete package is still one.
pub fn is_incomplete_package_name(name: &str) -> bool {
    let upper = strip_copy_tails(name).to_ascii_uppercase();
    match upper.rfind(INCOMPLETE_PACKAGE_SUFFIX) {
        Some(at) => {
            let rest = &upper[at + INCOMPLETE_PACKAGE_SUFFIX.len()..];
            rest.is_empty()
                || rest
                    .strip_prefix('_')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }
        None => false,
    }
}

/// R11-S2: the folder `package_dir` really names — canonicalized (`.`,
/// `..`, symlinks / junctions, 8.3 short names and Windows' dropped
/// trailing dots and spaces all resolved to the long name on disk) with
/// the `\\?\` prefix removed. A path that cannot be canonicalized (gone,
/// no access) is normalized lexically: `.`/`..` components folded and the
/// trailing dots and spaces of its last name dropped (as Windows does).
pub fn canonical_package_dir(package_dir: &Path) -> PathBuf {
    if let Ok(dir) = crate::util::canonicalize_display(package_dir) {
        return dir;
    }
    let mut out = PathBuf::new();
    for component in package_dir.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if cfg!(windows)
        && let Some(name) = out.file_name().and_then(|n| n.to_str())
    {
        let trimmed = name.trim_end_matches([' ', '.']).to_string();
        if !trimmed.is_empty() && trimmed != name {
            out.set_file_name(trimmed);
        }
    }
    out
}

/// R10-S2: why `package_dir` is from an incomplete finalize — its name
/// ([`is_incomplete_package_name`], which survives copying the folder
/// alone) or the marker beside it — or `None`.
///
/// R11-S2 (P11): judged on the CANONICAL folder ([`canonical_package_dir`]).
/// R10 read the name of the path as given: `verify-package .` inside the
/// package, `…\package_1_INCOMPLETE\..\package_1_INCOMPLETE`, the 8.3
/// name `PACKAG~1`, or `package_1_INCOMPLETE.` (Windows opens the same
/// folder) all read as a final package. The marker is looked for beside
/// the canonical folder under its own name and under the name without
/// copy tails (a copy made next to the original).
pub fn finalize_incomplete_reason(package_dir: &Path) -> Option<String> {
    let dir = canonical_package_dir(package_dir);
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if is_incomplete_package_name(&name) {
        return Some(format!("folder name ends with {INCOMPLETE_PACKAGE_SUFFIX}"));
    }
    let original = strip_copy_tails(&name);
    [name.as_str(), original]
        .into_iter()
        .filter(|candidate| !candidate.is_empty())
        .map(|candidate| {
            dir.with_file_name(format!("{candidate}{FINALIZE_INCOMPLETE_MARKER_SUFFIX}"))
        })
        .find(|marker| fs::symlink_metadata(marker).is_ok())
        .map(|marker| {
            format!(
                "marker {} beside the folder",
                marker
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            )
        })
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
const UNLISTED_PACKAGE_FILES: &[&str] = &["manifest.sha256", "package-manifest.json"];

/// The package README; listed in `manifest.sha256` since D10 (older
/// packages left it unlisted — reported as not verified, not as added).
const README_REL: &str = "README.txt";

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
    // R10-S2: a package whose finalize did not complete is not a final
    // result, however well its files check out.
    if let Some(why) = finalize_incomplete_reason(package_dir) {
        out.finalize_incomplete = true;
        out.push(
            CheckStatus::NotVerified,
            FINALIZE_CHECK_SUBJECT,
            format!(
                "FINALIZE INCOMPLETE ({why}): this package was written by a finalize that did not complete (the sign-off it carries was never confirmed), so it is NOT a final result — finalize the case again and use that package / 최종화 미완료 — 최종 결과로 쓰지 마십시오"
            ),
        );
    }
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
    let mut unlisted_found = false;
    for rel in present {
        if listed.contains_key(&rel) || UNLISTED_PACKAGE_FILES.contains(&rel.as_str()) {
            continue;
        }
        if rel == README_REL {
            out.push(
                CheckStatus::NotVerified,
                README_REL,
                "not listed in manifest.sha256 (package written before the README was covered); its text is not verified",
            );
            continue;
        }
        unlisted_found = true;
        out.push(
            CheckStatus::Fail,
            &rel,
            "present in the package but not listed in manifest.sha256 (added after packaging)",
        );
    }
    // D10: never claim "no unlisted files" next to an unlisted-file FAIL.
    if manifest_ok && !unlisted_found && out.files_verified == listed.len() {
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

    // 3. Hash-chained logs. A lane that is a chain lane by name (D16) is
    // always chain-verified, whatever its first line looks like — a lane
    // rewritten as plain JSON must not pass as "not a chain".
    for rel in listed
        .keys()
        .filter(|rel| rel.ends_with(".jsonl") || audit::is_known_chain_lane(rel))
    {
        let path = package_dir.join(rel);
        let known_lane = audit::is_known_chain_lane(rel);
        let chained = known_lane || is_chained_jsonl(&path);
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
                // R8 P0/P1/P2: the lane's trust under THIS PC's policy —
                // with audit keys here, any line not authenticated here
                // (stripped HMAC, another PC's key, an unproven retired
                // key) makes the lane NOT VERIFIED, never PASS and never
                // FAIL; without keys it passes as self-consistency only.
                let trust = result.lane_trust();
                let mut detail = format!(
                    "{}: {} entr{}, {}",
                    match &trust {
                        audit::LaneTrust::Authenticated =>
                            "audit chain verified (every entry HMAC-authenticated on this PC)",
                        audit::LaneTrust::SelfConsistentOnly(_) =>
                            "audit chain verified (structure only) — self-consistency only, NOT authentication",
                        audit::LaneTrust::NotVerified { .. } =>
                            "audit chain structure intact but NOT VERIFIED on this PC",
                    },
                    result.entries,
                    if result.entries == 1 { "y" } else { "ies" },
                    trust.label()
                );
                if !trust.reason().is_empty() {
                    detail.push_str(&format!("; {}", trust.reason()));
                }
                for warning in &result.warnings {
                    detail.push_str(&format!("; warning: {warning}"));
                }
                let status = if trust.passes() {
                    CheckStatus::Pass
                } else {
                    CheckStatus::NotVerified
                };
                out.push(status, rel, detail);
            }
            // R9 P5/I9: the audit key configuration exists but cannot be
            // read — NOT VERIFIED (fail closed), never "audit chain broken".
            Err(err) if audit::is_key_config_error(&err) => {
                out.push(CheckStatus::NotVerified, rel, err)
            }
            Err(err) if known_lane => out.push(
                CheckStatus::Fail,
                rel,
                format!(
                    "audit chain broken (known audit chain lane does not verify as a chain): {err}"
                ),
            ),
            Err(err) => out.push(CheckStatus::Fail, rel, format!("audit chain broken: {err}")),
        }
    }

    // 3b. R14 I3: the re-verification lane against the `hash-verify-head`
    // its runs recorded in the sign-off lane (a cut tail → FAIL).
    verify_package_hash_verify_head(package_dir, &listed, &mut out);

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
        // R9 P3: through the one lane reader; ANY entry (trusted or not)
        // asks for the carve report — this only adds a "not verified".
        && crate::audit::read_lane_rows(&package_dir.join(CARVE_LOG_REL))
            .map(|lane| !lane.rows.is_empty())
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

/// R14 I3 / R15 I7 (track I): each packaged result lane — the
/// re-verification lane (`evidence/logs/hash-verify.jsonl`) and the
/// anomaly lane (`evidence/logs/anomaly-log.jsonl`) — against the newest
/// usable `hash-verify-head` in the PACKAGE's own sign-off lane, with the
/// package's result run (its marker's `head_in`: its own head must be
/// there — R14 checked with no result run, so a cut head of the result
/// run was not seen). Nothing is said for a lane not in the package or
/// when there is no head and nothing could hide one (`Absent`).
///
/// R16 I6 — what changed for `verify-package` users (R15 → R16): these two
/// lines are NEW in R15 for the anomaly lane (subject
/// `evidence/logs/anomaly-log.jsonl`), and since R15 a re-verification
/// line can be NOT VERIFIED where R14 printed PASS (the result run's own
/// head missing: `own-head-missing`; no usable head but an unusable /
/// broken / repaired sign-off lane). R16 P26: FAIL (TAIL CUT / REWRITTEN)
/// now wins over an unusable sign-off line after the head (R15: NOT
/// VERIFIED), and a package whose result run is followed by an older
/// build's head or another scan's head is NOT VERIFIED
/// (`mixed-version` / `scan-mismatch`; re-scan and re-package to clear).
fn verify_package_hash_verify_head(
    package_dir: &Path,
    listed: &std::collections::BTreeMap<String, String>,
    out: &mut PackageVerification,
) {
    use crate::report::{HashVerifyHeadCheck, HeadLane};
    if !listed.contains_key(crate::report::SIGNOFF_LOG_REL_PATH) {
        return;
    }
    for lane in HeadLane::ALL {
        if !listed.contains_key(lane.rel()) {
            continue;
        }
        let check = match lane {
            HeadLane::HashVerify => match crate::anomaly::hash_verify_assessment(package_dir) {
                Ok(assessment) => assessment.head,
                Err(err) => HashVerifyHeadCheck::Unknown {
                    code: "unreadable",
                    why: err,
                },
            },
            HeadLane::Anomaly => crate::report::read_anomaly_head_check(package_dir),
        };
        let status = match &check {
            HashVerifyHeadCheck::Absent => continue,
            HashVerifyHeadCheck::Intact { .. } => CheckStatus::Pass,
            HashVerifyHeadCheck::Truncated { .. } | HashVerifyHeadCheck::Rewritten { .. } => {
                CheckStatus::Fail
            }
            HashVerifyHeadCheck::Unknown { .. } | HashVerifyHeadCheck::NotAuthenticated(_) => {
                CheckStatus::NotVerified
            }
        };
        let label = match lane {
            HeadLane::HashVerify => "re-verification lane anchor",
            HeadLane::Anomaly => "anomaly lane anchor",
        };
        out.push(
            status,
            lane.rel(),
            format!("{label}: {}", check.describe_in(lane)),
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
    // D13: a schema-v4 record is checked against the packaged evidence
    // hashes and every packaged audit lane's anchor as well; older records
    // keep their legacy meaning.
    let is_listed = |rel: &str| listed.contains_key(rel);
    let check = crate::report::check_signoff(
        package_dir,
        &text,
        &digests,
        crate::report::SignoffScope::Package(&is_listed),
    );
    let v4 = crate::report::Signoff::parse(&text)
        // R7-I2: exactly v4 — a higher schema is rejected by the check
        // (Unbound with a reason), never treated as v4.
        .is_some_and(|signoff| signoff.schema_version == crate::report::SIGNOFF_SCHEMA_V4);
    match check.currency {
        crate::report::SignoffCurrency::Current if v4 => out.push(
            CheckStatus::Pass,
            signoff_rel,
            format!(
                "sign-off v4: marks, the case records' target hashes and audit-lane anchors match the package, and this sign-off is the newest sign-off entry in the sign-off audit lane as found{} (removing later entries by cutting the lane's tail is not detected; examiner recorded as typed: {examiner}; not an authenticated identity)",
                if check.authenticated {
                    ", HMAC-authenticated"
                } else {
                    // R6-I6: a keyless match is self-consistency only.
                    " — self-consistency only, NOT authentication: the audit entry is not HMAC-verified here, so anyone holding the case folder could have recomputed it (see the evidence/logs/signoff-log.jsonl line below for how to authenticate it)"
                }
            ),
        ),
        // R6-I1: every checked binding matches, but something could not be
        // checked — the sign-off cannot be confirmed (the items follow).
        crate::report::SignoffCurrency::PartiallyVerified => out.push(
            CheckStatus::NotVerified,
            signoff_rel,
            format!(
                "sign-off v4: the checked bindings match, but {} item(s) could not be verified — the sign-off cannot be confirmed to match this package",
                check.not_verified.len()
            ),
        ),
        crate::report::SignoffCurrency::Changed if v4 => {
            for change in &check.changes {
                out.push(CheckStatus::Fail, signoff_rel, change.clone());
            }
        }
        crate::report::SignoffCurrency::Current => out.push(
            CheckStatus::Pass,
            signoff_rel,
            format!(
                "content_sha256 matches the review marks (status, note, IN-OUT range) and tags in the packaged db/case.db — self-consistency only, NOT authentication: an older-schema record anyone can recompute, shown as current only on a PC without audit keys (examiner recorded as typed: {examiner}; not an authenticated identity)"
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
        // R5-P1: a v4 record that the sign-off audit lane does not
        // confirm is "cannot be verified", not "predates content binding"
        // (the reason itself is listed under not_verified below).
        crate::report::SignoffCurrency::Unbound if v4 => out.push(
            CheckStatus::NotVerified,
            signoff_rel,
            "sign-off v4 is not confirmed by the sign-off audit lane (evidence/logs/signoff-log.jsonl) — the sign-off cannot be verified",
        ),
        crate::report::SignoffCurrency::LegacyBinding => out.push(
            CheckStatus::NotVerified,
            signoff_rel,
            "sign-off uses an older binding (schema_version < 3) that covers mark status and notes only; those still match, but whether IN-OUT ranges or tags changed after the sign-off cannot be checked — finalize again to bind them",
        ),
        // R7-I2: an older-schema record in a case whose sign-off lane has
        // entries, or an unsupported schema — the reasons follow.
        crate::report::SignoffCurrency::Unbound if !check.not_verified.is_empty() => out.push(
            CheckStatus::NotVerified,
            signoff_rel,
            "the sign-off record cannot be verified (reasons below)",
        ),
        crate::report::SignoffCurrency::Unbound => out.push(
            CheckStatus::NotVerified,
            signoff_rel,
            "sign-off predates content binding (no content_sha256); which marks it covers cannot be checked",
        ),
    }
    for item in &check.not_verified {
        out.push(CheckStatus::NotVerified, signoff_rel, item.clone());
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
            // D10: the report must have been rendered from THIS record
            // (marker = SHA-256 of the record bytes), not merely name the
            // same examiner.
            match crate::report::report_matches_signoff(&html, &text) {
                Ok(()) => out.push(
                    CheckStatus::Pass,
                    report_rel,
                    "report was rendered from the packaged sign-off record (signoff-sha256 marker matches the record bytes)",
                ),
                Err(err) => out.push(CheckStatus::Fail, report_rel, err),
            }
        }
        None => out.push(
            CheckStatus::NotVerified,
            report_rel,
            "no packaged report to compare with the sign-off",
        ),
    }
    // D13 / R5-P1: whether the packaged sign-off audit lane records this
    // very record. A lane that contradicts it (broken chain, only other
    // sign-offs, another content digest) is a FAIL, not "not verified".
    let record = crate::report::signoff_audit_record(
        package_dir,
        &text,
        listed.contains_key(crate::report::SIGNOFF_LOG_REL_PATH),
    );
    if let crate::report::SignoffAuditRecord::Verified {
        authenticated: false,
        ..
    } = &record
    {
        out.signoff_unauthenticated = true;
    }
    let status = match &record {
        crate::report::SignoffAuditRecord::Verified { .. } => CheckStatus::Pass,
        crate::report::SignoffAuditRecord::NotVerifiable(_) => CheckStatus::NotVerified,
        crate::report::SignoffAuditRecord::Contradicted(_) => CheckStatus::Fail,
    };
    out.push(status, crate::report::SIGNOFF_LOG_REL_PATH, record.note());
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
        // D6: a redacted package never names the job's subject path.
        let subject_path = match redaction {
            Some(redaction) => redaction.redactor.text(&job.subject_path),
            None => job.subject_path.clone(),
        };
        out.push_str(&format!(
            "    {{\"job_id\":\"{}\",\"job_type\":\"{}\",\"subject_path\":\"{}\",\"started_unix\":{}}}",
            json_escape(&job.job_id),
            json_escape(&job.job_type),
            json_escape(&subject_path),
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
            "renamed_files": redaction.renamed.iter().map(|(rel, name_sha256)| serde_json::json!({"package_path": rel, "original_name_sha256": name_sha256})).collect::<Vec<_>>(),
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

        /// R7-I2: once the sign-off lane holds entries, an older-schema
        /// record (a v4 record re-labelled v3 to skip the lane) is NOT
        /// VERIFIED, never PASS; a schema this build never writes is
        /// rejected and never described as "sign-off v4"; and a lane whose
        /// tail may have been re-written (R7-I3) is never a PASS either.
        #[test]
        fn downgraded_and_future_schema_signoffs_never_pass() {
            let (root, case_dir, _output) = signed_package("verify-r7i2");
            crate::audit::append_chained_jsonl_keyed(
                &case_dir.join(crate::report::SIGNOFF_LOG_REL_PATH),
                r#"{"schema_version":1,"event":"signoff","signoff_record_sha256":"x"}"#,
                None,
            )
            .unwrap();
            let output = root.join("package-v3");
            package_case(&case_dir, Some(&output)).unwrap();
            let result = verify_package(&output).unwrap();
            let signoff = check(&result, "review/signoff.json").unwrap();
            assert_eq!(signoff.status, CheckStatus::NotVerified, "{signoff:?}");
            assert!(
                result.checks.iter().any(|c| c.subject == "review/signoff.json"
                    && c.detail.contains("signoff-log")),
                "{:?}",
                result.checks
            );

            let v5 = fs::read_to_string(output.join("review/signoff.json"))
                .unwrap()
                .replace("\"schema_version\":3", "\"schema_version\":5");
            rewrite_listed(&output, "review/signoff.json", &v5);
            let result = verify_package(&output).unwrap();
            let signoff = check(&result, "review/signoff.json").unwrap();
            assert_ne!(signoff.status, CheckStatus::Pass, "{signoff:?}");
            assert!(!signoff.detail.contains("sign-off v4"), "{signoff:?}");
            assert!(
                result
                    .checks
                    .iter()
                    .any(|c| c.subject == "review/signoff.json"
                        && c.detail.contains("schema_version 5")),
                "{:?}",
                result.checks
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

        /// Rewrites a packaged file and recomputes both manifests, so only
        /// the content checks can catch the change.
        fn rewrite_listed(package: &Path, rel: &str, text: &str) {
            fs::write(package.join(rel), text).unwrap();
            let sha = crate::sha256::digest_bytes(text.as_bytes());
            let manifest: String = fs::read_to_string(package.join("manifest.sha256"))
                .unwrap()
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

        /// D10: README.txt is in manifest.sha256 and checked; a package
        /// from before (README unlisted) reads NOT VERIFIED, not FAIL.
        #[test]
        fn readme_is_listed_and_checked() {
            let (root, _case, output) = signed_package("verify-readme");
            let manifest = fs::read_to_string(output.join("manifest.sha256")).unwrap();
            assert!(manifest.contains("  README.txt\n"), "{manifest}");
            let result = verify_package(&output).unwrap();
            assert!(result.passed(), "{:?}", result.checks);
            fs::write(output.join("README.txt"), "edited after packaging").unwrap();
            let result = verify_package(&output).unwrap();
            assert!(
                failing_subjects(&result).contains(&"README.txt".to_string()),
                "{:?}",
                result.checks
            );
            // Older layout: README not listed anywhere.
            let manifest: String = manifest
                .lines()
                .filter(|line| !line.ends_with("  README.txt"))
                .map(|line| format!("{line}\n"))
                .collect();
            fs::write(output.join("manifest.sha256"), manifest).unwrap();
            let path = output.join("package-manifest.json");
            let mut json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            json["files"]
                .as_array_mut()
                .unwrap()
                .retain(|file| file["relative_path"] != "README.txt");
            json["file_count"] = (json["files"].as_array().unwrap().len() as u64).into();
            fs::write(&path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
            let result = verify_package(&output).unwrap();
            assert!(result.passed(), "{:?}", result.checks);
            assert_eq!(
                check(&result, "README.txt").unwrap().status,
                CheckStatus::NotVerified
            );
            fs::remove_dir_all(root).unwrap();
        }

        /// D10/D16 in the library check (not only the CLI): a known chain
        /// lane rewritten as plain JSON (manifests recomputed) FAILS, the
        /// report marker is compared, and "no unlisted files" is never
        /// printed next to an unlisted-file FAIL.
        #[test]
        fn library_verify_has_marker_known_lane_and_unlisted_rules() {
            let (root, _case, output) = signed_package("verify-lib-d10");
            let result = verify_package(&output).unwrap();
            assert!(
                result
                    .checks
                    .iter()
                    .any(|c| c.subject == "reports/case-report.html"
                        && c.status == CheckStatus::Pass
                        && c.detail.contains("signoff-sha256 marker")),
                "{:?}",
                result.checks
            );
            let lane = "evidence/logs/scan-log.jsonl";
            rewrite_listed(&output, lane, "{\"plain\":true}\n");
            let result = verify_package(&output).unwrap();
            assert!(
                result.failures().any(|c| c.subject == lane
                    && c.detail
                        .contains("known audit chain lane does not verify as a chain")),
                "{:?}",
                result.checks
            );
            fs::write(output.join("reports/extra.txt"), "added").unwrap();
            let result = verify_package(&output).unwrap();
            assert!(failing_subjects(&result).contains(&"reports/extra.txt".to_string()));
            assert!(
                !result.checks.iter().any(
                    |c| c.status == CheckStatus::Pass && c.detail.contains("no unlisted files")
                ),
                "contradictory PASS line"
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
    use std::fs;

    /// D6/D12/D5 in a path-redacted package: the data bundle's own marker
    /// takes part in the mode decision, a running job's subject path is
    /// redacted in package-manifest.json, and pre-D5 artifact names that
    /// embed a path are shipped under path-free names (listed in README).
    #[test]
    fn redacted_package_redacts_job_paths_bundle_mode_and_legacy_names() {
        let root = fixture("redact-r4i");
        let case_dir = root.join("case");
        let secret_dir = root.join("Secret Evidence");
        fs::create_dir_all(&secret_dir).unwrap();
        let redacted_page = format!(
            "<html><body>{}<p>redacted</p></body></html>",
            crate::redact::REDACTION_MARKER
        );
        fs::create_dir_all(case_dir.join("reports")).unwrap();
        fs::create_dir_all(case_dir.join("review")).unwrap();
        fs::write(case_dir.join("reports/case-report.html"), &redacted_page).unwrap();
        fs::write(case_dir.join("review/index.html"), &redacted_page).unwrap();
        // D12: an unredacted bundle next to redacted pages is a mix.
        fs::write(
            case_dir.join("review/data-bundle.js"),
            "window.FT_DATA = {};\n",
        )
        .unwrap();
        let err = package_case(&case_dir, Some(&root.join("refused"))).unwrap_err();
        assert!(err.contains("review/data-bundle.js"), "{err}");
        fs::write(
            case_dir.join("review/data-bundle.js"),
            crate::redact::mark_bundle_redacted("window.FT_DATA = {};\n"),
        )
        .unwrap();
        // D6: a running job naming a path outside the case.
        let job =
            crate::case_db::start_job(&case_dir, "scan-folder", &secret_dir, None, "{}").unwrap();
        // D5: one pre-D5 proxy and thumbnail (names embed the path), one
        // path-free proxy.
        fs::create_dir_all(case_dir.join("artifacts/proxies")).unwrap();
        fs::create_dir_all(case_dir.join("artifacts/thumbnails")).unwrap();
        let legacy_proxy = format!(
            "{}_proxy_1700000000.mp4",
            crate::video_export::sanitize_filename(&secret_dir.join("cam1.mp4").to_string_lossy())
        );
        assert!(legacy_proxy.contains("Secret"), "{legacy_proxy}");
        fs::write(case_dir.join("artifacts/proxies").join(&legacy_proxy), b"p").unwrap();
        let legacy_thumb = legacy_proxy
            .replace("_proxy_", "_thumb_")
            .replace(".mp4", ".jpg");
        fs::write(
            case_dir.join("artifacts/thumbnails").join(&legacy_thumb),
            b"t",
        )
        .unwrap();
        fs::write(
            case_dir.join("artifacts/proxies/proxy_vid_000001_0123abcd.mp4"),
            b"n",
        )
        .unwrap();

        let output = root.join("package");
        let result = package_case(&case_dir, Some(&output)).unwrap();
        assert!(result.path_redacted);
        let manifest_text = fs::read_to_string(output.join("package-manifest.json")).unwrap();
        assert!(
            !manifest_text.contains("Secret"),
            "a path leaked into package-manifest.json: {manifest_text}"
        );
        let manifest = manifest_of(&output);
        assert_eq!(manifest["quiescence"]["quiescent"], false);
        let subject = manifest["quiescence"]["running_jobs"][0]["subject_path"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(subject.contains("redacted"), "{subject}");
        // No packaged file name carries the path; both legacy files are
        // there under path-free names, the path-free proxy as it was.
        let checksums = fs::read_to_string(output.join("manifest.sha256")).unwrap();
        assert!(!checksums.contains("Secret"), "{checksums}");
        assert!(
            output
                .join("artifacts/proxies/proxy_vid_000001_0123abcd.mp4")
                .is_file()
        );
        let renamed = manifest["path_redaction"]["renamed_files"]
            .as_array()
            .unwrap();
        assert_eq!(renamed.len(), 2, "{renamed:?}");
        let readme = fs::read_to_string(output.join("README.txt")).unwrap();
        assert!(readme.contains("RENAMED ARTIFACT FILES"), "{readme}");
        assert!(!readme.contains("Secret"), "{readme}");
        for entry in renamed {
            let rel = entry["package_path"].as_str().unwrap();
            assert!(
                rel.starts_with("artifacts/proxies/legacy_proxy_")
                    || rel.starts_with("artifacts/thumbnails/legacy_thumb_"),
                "{rel}"
            );
            assert!(output.join(rel).is_file(), "{rel}");
            assert!(readme.contains(rel), "{rel} not in README");
        }
        let verification = super::verify_package(&output).unwrap();
        assert!(verification.passed(), "{:?}", verification.checks);
        crate::case_db::complete_job(&case_dir, &job.job_id, 0, "done").unwrap();
        fs::remove_dir_all(root).unwrap();
    }

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

    fn minimal_packageable_case(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("frametrace-package-{tag}-{}", std::process::id()));
        let case_dir = root.join("case");
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
        (root, case_dir)
    }

    /// R5-S12: a transient rename failure (antivirus / indexer handle on
    /// Windows) is retried; a persistent one keeps the complete package in
    /// its work folder (marked, so no stale sweep deletes it) and names it
    /// in the error. Before, one failed rename deleted the finished
    /// package.
    #[test]
    fn package_publish_retries_and_never_deletes_a_finished_package() {
        let (root, case_dir) = minimal_packageable_case("r5-rename");
        super::FAIL_PUBLISH_RENAMES.with(|left| left.set(3));
        let result = package_case(&case_dir, None).expect("published after retries");
        assert!(result.output_dir.join("manifest.sha256").is_file());
        super::FAIL_PUBLISH_RENAMES.with(|left| left.set(99));
        // R10-S4: retried for the whole budget; a short one here.
        super::RENAME_BUDGET_OVERRIDE
            .with(|cell| cell.set(Some(std::time::Duration::from_millis(500))));
        let err = package_case(&case_dir, None).unwrap_err();
        super::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(None));
        super::FAIL_PUBLISH_RENAMES.with(|left| left.set(0));
        assert!(err.contains("남겨 두었습니다"), "{err}");
        let kept: Vec<std::path::PathBuf> = fs::read_dir(case_dir.join("reports"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                crate::join::is_pid_part_name(
                    &path.file_name().unwrap().to_string_lossy(),
                    Some(std::process::id()),
                )
            })
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(
            kept[0].join("manifest.sha256").is_file(),
            "package kept whole"
        );
        // R6-S5: the marker is BESIDE the kept folder, so the kept
        // package verifies as delivered (a marker inside was an unlisted
        // file: verify-package failed), and no sweep removes it.
        assert!(crate::join::preserved_part_marker(&kept[0]).is_file());
        assert!(!kept[0].join(crate::join::PRESERVED_PART_MARKER).exists());
        let verification = super::verify_package(&kept[0]).expect("verify kept package");
        assert_eq!(
            verification.failures().count(),
            0,
            "{:?}",
            verification
                .failures()
                .map(|c| c.subject.clone())
                .collect::<Vec<_>>()
        );
        assert!(err.contains(&kept[0].display().to_string()), "{err}");
        let swept = crate::join::sweep_pid_parts(&case_dir.join("reports"), std::process::id());
        assert!(swept.is_empty(), "a kept package is never swept: {swept:?}");
        assert!(kept[0].is_dir());
        let _ = fs::remove_dir_all(root);
    }

    /// R8-S5: releasing our reservation at publish time failed once (a
    /// transient removal failure) and R7 took that for "the name is not
    /// ours any more": the finished package was set aside with an error.
    /// Removal is now retried; a release that keeps failing still
    /// publishes (the publish step clears what is left of OUR reservation).
    #[test]
    fn r8_s5_reservation_release_failures_do_not_strand_the_package() {
        let (root, case_dir) = minimal_packageable_case("r8-s5");
        // Two failed removals, then success: retried, published.
        crate::join::FAIL_RESERVATION_REMOVALS.with(|left| left.set(2));
        let result = package_case(&case_dir, None).expect("published after retried release");
        assert!(result.output_dir.join("manifest.sha256").is_file());
        assert!(
            !result
                .output_dir
                .join(crate::join::RESERVATION_MARKER)
                .exists()
        );
        // Every release attempt fails: still published under the name.
        crate::join::FAIL_RESERVATION_REMOVALS.with(|left| left.set(5));
        let result = package_case(&case_dir, None);
        crate::join::FAIL_RESERVATION_REMOVALS.with(|left| left.set(0));
        let result = result.expect("a failed release is not a stolen name");
        assert!(result.output_dir.join("manifest.sha256").is_file());
        let verification = super::verify_package(&result.output_dir).expect("verify");
        assert_eq!(verification.failures().count(), 0);
        // Another pid's reservation is still never ours.
        let other = case_dir.join("reports/package_other");
        fs::create_dir_all(&other).unwrap();
        fs::write(
            other.join(crate::join::RESERVATION_MARKER),
            crate::join::reservation_marker_text(4242),
        )
        .unwrap();
        assert_eq!(
            crate::join::release_own_reservation(&other, std::process::id()),
            crate::join::ReservationRelease::NotOurs
        );
        assert!(other.join(crate::join::RESERVATION_MARKER).is_file());
        let _ = fs::remove_dir_all(root);
    }

    /// R8-S5 (Windows): a marker briefly held open without delete sharing
    /// (an antivirus scan) made R7's single removal attempt fail, and the
    /// release reported the reservation as not ours.
    #[cfg(windows)]
    #[test]
    fn r8_s5_briefly_locked_marker_is_released() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = std::env::temp_dir().join(format!("ft-r8-s5-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let reserved = root.join("package_7");
        fs::create_dir_all(&reserved).unwrap();
        let marker = reserved.join(crate::join::RESERVATION_MARKER);
        fs::write(&marker, crate::join::reservation_marker_text(777)).unwrap();
        // FILE_SHARE_READ only: deleting fails while the handle is open.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&marker)
            .unwrap();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        assert!(crate::join::release_reservation(&reserved, 777));
        holder.join().unwrap();
        assert!(!reserved.exists());
        let _ = fs::remove_dir_all(root);
    }

    /// R8-S6: a reservation is born marked (staged, then renamed onto the
    /// name) — never an empty folder a sweep would take for "owner died
    /// before marking", or that a unix rename would replace — and placing
    /// it never replaces an existing folder, even an empty one.
    #[test]
    fn r8_s6_reservation_is_placed_marked_and_never_replaces() {
        let root = std::env::temp_dir().join(format!("ft-r8-s6-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let reports = root.join("reports");
        fs::create_dir_all(&reports).unwrap();
        let (reserved, work) =
            super::reserve_package_dirs(&reports.join("package_5"), 4242).expect("reserve");
        assert_eq!(crate::join::reservation_owner(&reserved), Some(Some(4242)));
        assert!(work.is_dir());
        let staging = crate::join::reservation_staging_path(&reserved, 4242);
        assert!(!staging.exists(), "no staging folder is left behind");
        assert!(crate::join::is_pid_part_name(
            &staging.file_name().unwrap().to_string_lossy(),
            Some(4242)
        ));
        // A name that appears between the existence check and the rename
        // (here: an empty folder) is never replaced.
        let from = reports.join(".staged");
        fs::create_dir_all(&from).unwrap();
        fs::write(from.join(crate::join::RESERVATION_MARKER), "pid=1\n").unwrap();
        let taken = reports.join("package_6");
        fs::create_dir_all(&taken).unwrap();
        let err = super::reservation_rename(&from, &taken).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(crate::join::reservation_owner(&taken), Some(None));
        assert!(from.join(crate::join::RESERVATION_MARKER).is_file());
        assert!(!super::place_reservation(&from, &taken));
        // A leftover staging folder of a dead writer is swept.
        fs::create_dir_all(crate::join::reservation_staging_path(
            &reports.join("package_8"),
            4243,
        ))
        .unwrap();
        crate::join::sweep_stale_parts_with(&reports, &|_| crate::util::ProcessProbe::Gone, 0);
        assert!(!crate::join::reservation_staging_path(&reports.join("package_8"), 4243).exists());
        let _ = fs::remove_dir_all(root);
    }

    /// R9-S3: a placing rename that fails for a reason other than "the
    /// name exists" fails the reservation after the one backoff (5
    /// attempts) - R8 moved on to the next of 10,000 names, each with the
    /// full backoff, so a persistent failure kept package-case busy for
    /// hours. Nothing is left behind, and a taken name still moves on.
    #[test]
    fn r9_s3_persistent_reservation_failure_fails_fast() {
        let root = std::env::temp_dir().join(format!("ft-r9-s3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let reports = root.join("reports");
        fs::create_dir_all(&reports).unwrap();
        let calls = std::cell::Cell::new(0usize);
        let denied = |_: &std::path::Path, _: &std::path::Path| -> std::io::Result<()> {
            calls.set(calls.get() + 1);
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "access denied",
            ))
        };
        let started = std::time::Instant::now();
        // R10-S4: the retry budget (12 s) shortened to the bare backoff.
        super::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(Some(std::time::Duration::ZERO)));
        let err =
            super::reserve_package_dirs_with(&reports.join("package_5"), 4242, &denied, false)
                .expect_err("a persistent failure must fail the reservation");
        super::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(None));
        assert_eq!(calls.get(), 5, "one name, one backoff");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(
            err.contains("access denied") && err.contains("package_5"),
            "{err}"
        );
        let left: Vec<_> = fs::read_dir(&reports).unwrap().flatten().collect();
        assert!(left.is_empty(), "left behind: {left:?}");
        // A taken name still moves on to the next one.
        fs::create_dir_all(reports.join("package_5")).unwrap();
        let (reserved, work) =
            super::reserve_package_dirs(&reports.join("package_5"), 4242).expect("reserve");
        assert_eq!(reserved, reports.join("package_5_001"));
        assert!(work.is_dir());
        let _ = fs::remove_dir_all(root);
    }

    /// R10-S2: R9 left the "finalize incomplete" marker beside the package
    /// and nothing read it: verify-package passed the package as is. A
    /// package renamed `…_INCOMPLETE` (or with the marker beside it) is
    /// now reported NOT VERIFIED "finalize", while its files still check
    /// out; the same package under a plain name is unaffected.
    #[test]
    fn r10_s2_verify_package_reports_an_incomplete_finalize() {
        assert!(super::is_incomplete_package_name("package_1_INCOMPLETE"));
        assert!(super::is_incomplete_package_name("package_1_incomplete_2"));
        assert!(!super::is_incomplete_package_name("package_1"));
        assert!(!super::is_incomplete_package_name("package_1_INCOMPLETE_x"));
        assert!(!super::is_incomplete_package_name("package_INCOMPLETEness"));
        let (root, case_dir) = minimal_packageable_case("r10-s2");
        let result = package_case(&case_dir, None).expect("package");
        let finalize = |checked: &super::PackageVerification| {
            checked
                .checks
                .iter()
                .filter(|c| c.subject == super::FINALIZE_CHECK_SUBJECT)
                .map(|c| c.status)
                .collect::<Vec<_>>()
        };
        let plain = super::verify_package(&result.output_dir).unwrap();
        assert!(!plain.finalize_incomplete && finalize(&plain).is_empty());
        // Marker beside (R9's only signal) is read now.
        let marker = super::finalize_incomplete_marker(&result.output_dir);
        fs::write(&marker, "x").unwrap();
        let marked = super::verify_package(&result.output_dir).unwrap();
        assert!(marked.finalize_incomplete);
        assert_eq!(finalize(&marked), vec![super::CheckStatus::NotVerified]);
        fs::remove_file(&marker).unwrap();
        // Renamed: the state travels with the folder alone.
        let renamed = result.output_dir.with_file_name(format!(
            "{}_INCOMPLETE",
            result.output_dir.file_name().unwrap().to_string_lossy()
        ));
        fs::rename(&result.output_dir, &renamed).unwrap();
        let checked = super::verify_package(&renamed).unwrap();
        assert!(checked.finalize_incomplete);
        assert_eq!(finalize(&checked), vec![super::CheckStatus::NotVerified]);
        assert!(
            checked.passed(),
            "its files still check out: {:?}",
            checked.checks
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The 8.3 short form of an existing path (Windows; `None` when the
    /// volume keeps no short names — the caller then says SKIPPED).
    #[cfg(windows)]
    fn short_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/C for %I in (\"{}\") do @echo %~sI",
                path.display()
            ))
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let short = std::path::PathBuf::from(text);
        (short.file_name() != path.file_name() && short.exists()).then_some(short)
    }

    /// R11-S2 (P11, real file system): the `_INCOMPLETE` judgement is made
    /// on the canonical folder. R10 read the name of the path as given, so
    /// `verify-package .` run inside an incomplete package, a `..` path,
    /// the 8.3 short name, a trailing dot or space (Windows opens the same
    /// folder) and an Explorer copy (` - 복사본`, ` (2)`) all read as a
    /// final package.
    #[test]
    fn r11_s2_incomplete_judgement_uses_the_canonical_folder() {
        let (root, case_dir) = minimal_packageable_case("r11-s2");
        let result = package_case(&case_dir, None).expect("package");
        let incomplete = result.output_dir.with_file_name(format!(
            "{}_INCOMPLETE",
            result.output_dir.file_name().unwrap().to_string_lossy()
        ));
        fs::rename(&result.output_dir, &incomplete).unwrap();
        let reason = |path: &std::path::Path| super::finalize_incomplete_reason(path);
        // `<sub>/..` only opens the folder when `<sub>` exists (Unix resolves
        // `..` on disk), so pick a real subfolder of the package.
        let sub = fs::read_dir(&incomplete)
            .unwrap()
            .flatten()
            .find(|e| e.path().is_dir())
            .map(|e| e.path().join(".."))
            .unwrap_or_else(|| incomplete.join("."));
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut spellings = vec![
            incomplete.join("."),
            sub,
            incomplete
                .parent()
                .unwrap()
                .join(".")
                .join(incomplete.file_name().unwrap()),
        ];
        #[cfg(windows)]
        {
            let name = incomplete
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            spellings.push(incomplete.with_file_name(format!("{name}.")));
            spellings.push(incomplete.with_file_name(format!("{name} ")));
            spellings.push(incomplete.with_file_name(format!("{name}. .")));
            match short_path(&incomplete) {
                Some(short) => {
                    assert!(
                        !short.to_string_lossy().contains("_INCOMPLETE"),
                        "{}",
                        short.display()
                    );
                    spellings.push(short);
                }
                None => eprintln!("SKIPPED (8.3): no short names on this volume"),
            }
        }
        for path in &spellings {
            assert!(
                path.exists(),
                "the spelling opens the folder: {}",
                path.display()
            );
            assert!(reason(path).is_some(), "read as final: {}", path.display());
            // verify-package itself, as `verify-package <spelling>` runs it.
            let checked = super::verify_package(path).unwrap();
            assert!(checked.finalize_incomplete, "{}", path.display());
        }
        // The canonical folder is the long name without `\\?\`.
        let canonical = super::canonical_package_dir(&incomplete.join("."));
        assert_eq!(canonical.file_name(), incomplete.file_name());
        assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
        // Copies of an incomplete package are incomplete packages.
        let reports = incomplete.parent().unwrap();
        for copy in [
            "package_7_INCOMPLETE - 복사본",
            "package_7_INCOMPLETE - 복사본 (2)",
            "package_7_INCOMPLETE (3)",
            "package_7_incomplete_2 - Copy",
            "package_7_INCOMPLETE_2 - Copy (4) - 복사본",
        ] {
            let dir = reports.join(copy);
            fs::create_dir_all(&dir).unwrap();
            assert!(reason(&dir).is_some(), "{copy}");
        }
        // A copy made beside a package marked only by its marker.
        let marked = reports.join("package_8");
        fs::create_dir_all(&marked).unwrap();
        fs::write(super::finalize_incomplete_marker(&marked), "x").unwrap();
        let copy = reports.join("package_8 - 복사본");
        fs::create_dir_all(&copy).unwrap();
        assert!(reason(&copy).is_some(), "marker of the original");
        // Not incomplete: a plain name, near misses, a copy of a plain one.
        for plain in [
            "package_9",
            "package_9 - 복사본",
            "package_9_INCOMPLETEX",
            "package_9_INCOMPLETE_x (2)",
            "package_9 (2)_INCOMPLETEness",
        ] {
            let dir = reports.join(plain);
            fs::create_dir_all(&dir).unwrap();
            assert!(reason(&dir).is_none(), "{plain}");
        }
        // A missing path is normalized lexically.
        let gone = reports.join("package_5_INCOMPLETE").join("sub").join("..");
        assert!(reason(&gone).is_some());
        assert_eq!(super::strip_copy_tails("a - 복사본 (2). "), "a");
        assert_eq!(super::strip_copy_tails("a (b)"), "a (b)");
        let _ = fs::remove_dir_all(root);
    }

    /// R12-S10: Explorer's copy tails of German, Japanese, Chinese and
    /// French Windows (R11 knew Korean and English only), alone, repeated
    /// and mixed with ` (n)`; near misses stay plain.
    #[test]
    fn r12_s10_other_locale_copy_tails_are_stripped() {
        for copy in [
            "package_7_INCOMPLETE - Kopie",
            "package_7_INCOMPLETE - kopie (2)",
            "package_7_INCOMPLETE - コピー",
            "package_7_INCOMPLETE - 副本",
            "package_7_INCOMPLETE - Copie",
            "package_7_INCOMPLETE_2 - Copie (3) - コピー - 副本",
        ] {
            assert!(super::is_incomplete_package_name(copy), "{copy}");
        }
        assert_eq!(super::strip_copy_tails("a - コピー (2)"), "a");
        assert_eq!(super::strip_copy_tails("a - 副本 - Kopie"), "a");
        for plain in [
            "package_9 - Kopie",
            "package_9_INCOMPLETE-Kopie",
            "package_9_INCOMPLETE - Kopien",
        ] {
            assert!(!super::is_incomplete_package_name(plain), "{plain}");
        }
    }

    /// R13-S13: more locales' tails, and non-ASCII case. a4fb793 kept
    /// ` - 複製`, ` - copia`, ` - Cópia`, ` - копия` (unknown tails) and
    /// compared ASCII case only (` - КОПИЯ`, ` - CÓPIA` missed).
    #[test]
    fn r13_s13_more_copy_tails_and_unicode_case() {
        for copy in [
            "package_7_INCOMPLETE - 複製",
            "package_7_INCOMPLETE - Copia",
            "package_7_INCOMPLETE - Cópia",
            "package_7_INCOMPLETE - CÓPIA (2)",
            "package_7_INCOMPLETE - копия",
            "package_7_INCOMPLETE - КОПИЯ",
            "package_7_INCOMPLETE - Копия - 복사본",
        ] {
            assert!(super::is_incomplete_package_name(copy), "{copy}");
        }
        assert_eq!(super::strip_copy_tails("가 - КОПИЯ"), "가");
        assert_eq!(super::strip_copy_tails("ab"), "ab");
        assert!(!super::is_incomplete_package_name(
            "package_9_INCOMPLETE - копиям"
        ));
    }

    /// R11-S7: a rename whose source is gone fails at once (R10 retried
    /// NotFound for the whole 12 s budget), and a busy file reports each
    /// wait to the caller.
    #[test]
    fn r11_s7_not_found_is_final_and_waits_are_reported() {
        let root = std::env::temp_dir().join(format!("ft-r11-s7-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let started = std::time::Instant::now();
        let mut waits = 0usize;
        let err = super::rename_with_retry(
            &root.join("missing"),
            &root.join("target"),
            &|from, to| fs::rename(from, to),
            &mut |_| waits += 1,
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(waits, 0, "never waited");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        // Busy three times, then free: three waits reported, then success.
        let left = std::cell::Cell::new(3usize);
        let mut reported = Vec::new();
        super::rename_with_retry(
            &root.join("a"),
            &root.join("b"),
            &|_, _| {
                if left.get() > 0 {
                    left.set(left.get() - 1);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "in use",
                    ));
                }
                Ok(())
            },
            &mut |waited| reported.push(waited),
        )
        .unwrap();
        assert_eq!(reported.len(), 3);
        assert!(reported.windows(2).all(|w| w[0] <= w[1]));
        let _ = fs::remove_dir_all(root);
    }

    /// R10-S4: a reservation rename refused for longer than R9's five
    /// attempts (1.85 s) — a real-time antivirus scan of the fresh staging
    /// folder — failed the whole package. It is now retried for the
    /// budget, and the first name is still the one reserved; a failure
    /// that persists past the budget still fails, soon after it.
    #[test]
    fn r10_s4_transient_access_denied_longer_than_the_backoff_still_reserves() {
        let root = std::env::temp_dir().join(format!("ft-r10-s4-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let reports = root.join("reports");
        fs::create_dir_all(&reports).unwrap();
        let started = std::time::Instant::now();
        let calls = std::cell::Cell::new(0usize);
        let busy_for_a_while = |from: &std::path::Path, to: &std::path::Path| {
            calls.set(calls.get() + 1);
            if started.elapsed() < std::time::Duration::from_millis(2600) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "access denied (scan in progress)",
                ));
            }
            super::reservation_rename(from, to)
        };
        let (reserved, work) = super::reserve_package_dirs_with(
            &reports.join("package_7"),
            4242,
            &busy_for_a_while,
            false,
        )
        .expect("a transient denial within the budget reserves the name");
        assert_eq!(reserved, reports.join("package_7"));
        assert!(reserved.is_dir() && work.is_dir());
        assert!(
            calls.get() > 5,
            "retried past the R9 backoff: {}",
            calls.get()
        );
        // Persistent: gives up after the budget, not long after.
        let budget = std::time::Duration::from_millis(1500);
        super::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(Some(budget)));
        let started = std::time::Instant::now();
        let denied = |_: &std::path::Path, _: &std::path::Path| -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "access denied",
            ))
        };
        let err =
            super::reserve_package_dirs_with(&reports.join("package_9"), 4242, &denied, false)
                .expect_err("persistent");
        super::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(None));
        let took = started.elapsed();
        assert!(took >= budget, "{took:?}");
        assert!(
            took < budget + std::time::Duration::from_secs(3),
            "{took:?}"
        );
        assert!(err.contains("access denied"), "{err}");
        let _ = fs::remove_dir_all(root);
    }

    /// R6-S8: the default name was reserved (an empty `package_<ts>`)
    /// BEFORE the pid-tagged work folder existed, so a package-case that
    /// died in between leaked the empty folder for good — nothing named
    /// it. Now the work folder comes first, and the stale sweep of a dead
    /// writer's work folder removes the reservation it names.
    #[test]
    fn s8_reservation_follows_the_work_folder() {
        let root = std::env::temp_dir().join(format!("ft-r6-s8-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let reports = root.join("reports");
        fs::create_dir_all(&reports).unwrap();
        let (final_dir, work) =
            super::reserve_package_dirs(&reports.join("package_5"), 4242).expect("reserve");
        assert_eq!(final_dir, reports.join("package_5"));
        assert_eq!(work, reports.join(".package_5.ft4242.part"));
        assert!(final_dir.is_dir() && work.is_dir());
        // Taken name: the next candidate, its own work folder.
        let (second, second_work) =
            super::reserve_package_dirs(&reports.join("package_5"), 4242).unwrap();
        assert_eq!(second, reports.join("package_5_001"));
        assert_eq!(second_work, reports.join(".package_5_001.ft4242.part"));
        // pid 4242 is dead: the stale sweep removes work folder AND
        // reservation (old enough: min age 0).
        let swept =
            crate::join::sweep_stale_parts_with(&reports, &|_| crate::util::ProcessProbe::Gone, 0);
        assert_eq!(swept.len(), 2, "{swept:?}");
        assert!(!final_dir.exists() && !work.exists());
        assert!(!second.exists() && !second_work.exists());
        // A finished package with the same name is never touched.
        fs::create_dir_all(reports.join(".package_9.ft4242.part")).unwrap();
        fs::create_dir_all(reports.join("package_9")).unwrap();
        fs::write(reports.join("package_9/manifest.sha256"), b"x").unwrap();
        crate::join::sweep_stale_parts_with(&reports, &|_| crate::util::ProcessProbe::Gone, 0);
        assert!(reports.join("package_9/manifest.sha256").is_file());
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

    /// A package holding only `evidence/logs/scan-log.jsonl` (listed).
    fn lane_only_package(
        tag: &str,
        bodies: &[(&str, Option<&crate::audit_key::AuditKey>)],
    ) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ft-pkg-r8-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let lane = dir.join("evidence/logs/scan-log.jsonl");
        fs::create_dir_all(lane.parent().unwrap()).unwrap();
        for (body, key) in bodies {
            crate::audit::append_chained_jsonl_keyed(&lane, body, *key).unwrap();
        }
        fs::write(
            dir.join("manifest.sha256"),
            format!(
                "{}  evidence/logs/scan-log.jsonl\n",
                crate::audit::digest_file(&lane).unwrap()
            ),
        )
        .unwrap();
        dir
    }

    /// R8 P0 for verify-package: a packaged lane re-chained with its HMACs
    /// stripped PASSED in R7 on a PC holding the key (graded structural);
    /// now it is NOT VERIFIED there. P2: a lane continued on another PC is
    /// NOT VERIFIED naming that PC's key — never FAIL. P1: no keys → PASS
    /// as self-consistency only.
    #[test]
    fn r8_verify_package_lane_trust_comes_from_keys_held() {
        use super::{CheckStatus, verify_package};
        let mine = crate::audit_key::AuditKey::new("pc-a", &[0xb1; 32]);
        let other = crate::audit_key::AuditKey::new("pc-b", &[0xb2; 32]);
        let lane_check = |dir: &std::path::Path| {
            verify_package(dir)
                .unwrap()
                .checks
                .into_iter()
                .find(|c| c.subject == "evidence/logs/scan-log.jsonl")
                .unwrap()
        };
        let stripped = lane_only_package("strip", &[(r#"{"n":1}"#, None), (r#"{"n":2}"#, None)]);
        let multi = lane_only_package(
            "multi",
            &[(r#"{"n":1}"#, Some(&mine)), (r#"{"n":2}"#, Some(&other))],
        );
        let genuine = lane_only_package("genuine", &[(r#"{"n":1}"#, Some(&mine))]);
        crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
            let check = lane_check(&stripped);
            assert_eq!(check.status, CheckStatus::NotVerified, "{check:?}");
            let check = lane_check(&multi);
            assert_eq!(check.status, CheckStatus::NotVerified, "{check:?}");
            assert!(
                check.detail.contains("\"pc-b\" 보유 PC에서 확인"),
                "{check:?}"
            );
            assert_eq!(lane_check(&genuine).status, CheckStatus::Pass);
        });
        crate::audit_key::with_test_keys(&[], || {
            let check = lane_check(&stripped);
            assert_eq!(check.status, CheckStatus::Pass);
            assert!(check.detail.contains("self-consistency only"), "{check:?}");
        });
        for dir in [stripped, multi, genuine] {
            let _ = fs::remove_dir_all(dir);
        }
    }
}
