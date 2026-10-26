//! Recent-projects store for the stage-1 start screen.
//!
//! `workstation-session.json` remembers only the LAST case (for restart
//! restore). `recent-cases.json`, in the same config directory, keeps the
//! examiner's recently opened/analysed cases so stage 1 can list them for
//! one-click reopening. The file is small (capped at [`RECENT_CAP`]),
//! written atomically, deduplicated by canonical case directory, and read
//! tolerantly: a missing or corrupt file is an empty list, never an error.

use super::*;

/// Most entries kept on disk (the UI shows the first few).
pub(crate) const RECENT_CAP: usize = 20;
/// Wall-clock budget for the per-entry DB counts in one listing; entries
/// past it report `reviewed`/`total` as null instead of stalling stage 1.
const RECENT_DB_BUDGET_MS: u128 = 1_500;
/// Checkpoint files larger than this are not line-counted (stat only).
const RECENT_CHECKPOINT_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Serializes read-modify-write of the store across handler threads.
static RECENT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecentCase {
    pub(crate) case_dir: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_path: Option<String>,
    #[serde(default)]
    pub(crate) last_opened_unix: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct RecentFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    cases: Vec<RecentCase>,
}

#[cfg(test)]
thread_local! {
    /// Test override: this thread's recent-cases file. Tests that read or
    /// write the list set their own file, so parallel tests in one process
    /// never share (and on Windows lock — error 32) one file.
    static RECENT_FILE_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test helper: points [`recent_cases_file`] at `file` on this thread
/// until the returned guard is dropped.
#[cfg(test)]
pub(crate) fn override_recent_cases_file(file: PathBuf) -> RecentFileOverride {
    RECENT_FILE_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(file));
    RecentFileOverride
}

/// Clears the thread's override ([`override_recent_cases_file`]) on drop.
#[cfg(test)]
pub(crate) struct RecentFileOverride;

#[cfg(test)]
impl Drop for RecentFileOverride {
    fn drop(&mut self) {
        RECENT_FILE_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    }
}

/// `<config>/frametrace/recent-cases.json`. Unit tests never touch the
/// examiner's real list (same isolation as `session_file`): a thread's
/// override when set, else a per-process temp file (best-effort writes of
/// in-process open-case/start calls in other tests land there).
#[cfg(not(test))]
pub(crate) fn recent_cases_file() -> Option<PathBuf> {
    crate::audit_key::config_dir().map(|dir| dir.join("recent-cases.json"))
}

/// Test build of [`recent_cases_file`]: never the real config.
#[cfg(test)]
pub(crate) fn recent_cases_file() -> Option<PathBuf> {
    if let Some(file) = RECENT_FILE_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return Some(file);
    }
    Some(std::env::temp_dir().join(format!(
        "frametrace-test-recent-{}.json",
        std::process::id()
    )))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Canonical form used for display and dedupe: the resolved path when the
/// folder exists (without Windows' `\\?\` prefix), otherwise the path as
/// given minus trailing separators.
pub(crate) fn canonical_case_dir(path: &Path) -> String {
    canonical_case_dir_with(path, &|path| path.canonicalize())
}

/// [`canonical_case_dir`] with the resolver injected (tests). S5: a UNC or
/// device path is never resolved — `canonicalize` would open an SMB
/// session (handing the examiner's NTLM credentials to the named host),
/// and `/api/recent-cases/forget` takes that text from any request body —
/// it keeps its textual form instead.
pub(crate) fn canonical_case_dir_with(
    path: &Path,
    resolve: &dyn Fn(&Path) -> std::io::Result<PathBuf>,
) -> String {
    let resolved = if super::http::reject_remote_path(&path.to_string_lossy(), &[]).is_ok() {
        resolve(path)
    } else {
        Err(std::io::Error::other("remote path: not resolved"))
    };
    match resolved {
        Ok(resolved) => shell_path(resolved).display().to_string(),
        Err(_) => {
            let text = path.display().to_string();
            let trimmed = text.trim_end_matches(['/', '\\']);
            if trimmed.is_empty() {
                text
            } else {
                trimmed.to_string()
            }
        }
    }
}

/// Dedupe key: canonical path; case-insensitive and separator-agnostic on
/// Windows, where `C:\Cases\A` and `c:/cases/a` are the same folder.
fn dedupe_key(case_dir: &str) -> String {
    let canonical = canonical_case_dir(Path::new(case_dir));
    if cfg!(windows) {
        canonical.replace('/', "\\").to_lowercase()
    } else {
        canonical
    }
}

fn case_display_name(case_dir: &str) -> String {
    Path::new(case_dir)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| case_dir.to_string())
}

/// Reads the store. Missing, unreadable or corrupt → empty list; entries
/// without a case_dir are dropped; order is most recent first.
pub(crate) fn load_recent(file: &Path) -> Vec<RecentCase> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let mut cases = match serde_json::from_str::<RecentFile>(&text) {
        Ok(parsed) => parsed.cases,
        // An older/hand-edited bare array is accepted too.
        Err(_) => serde_json::from_str::<Vec<RecentCase>>(&text).unwrap_or_default(),
    };
    cases.retain(|entry| !entry.case_dir.trim().is_empty());
    cases.sort_by_key(|entry| std::cmp::Reverse(entry.last_opened_unix));
    cases
}

fn save_recent(file: &Path, cases: &[RecentCase]) -> std::io::Result<()> {
    let body = serde_json::to_string_pretty(&RecentFile {
        version: 1,
        cases: cases.to_vec(),
    })
    .map_err(std::io::Error::other)?;
    crate::util::write_text_atomic(file, &body)
}

/// Pure: puts `entry` first, merging with an existing entry for the same
/// folder (a missing source_path keeps the remembered one), and caps the
/// list at [`RECENT_CAP`].
pub(crate) fn upsert_recent(mut cases: Vec<RecentCase>, entry: RecentCase) -> Vec<RecentCase> {
    let key = dedupe_key(&entry.case_dir);
    let mut merged = entry;
    if let Some(pos) = cases.iter().position(|c| dedupe_key(&c.case_dir) == key) {
        let old = cases.remove(pos);
        if merged.source_path.is_none() {
            merged.source_path = old.source_path;
        }
    }
    // Any further duplicates (a list written by an older build) go too.
    cases.retain(|c| dedupe_key(&c.case_dir) != key);
    cases.insert(0, merged);
    cases.truncate(RECENT_CAP);
    cases
}

/// Records an open/start/finish of `case_dir` in `file`.
pub(crate) fn record_recent_in(
    file: &Path,
    case_dir: &Path,
    source_path: Option<&Path>,
    when_unix: u64,
) -> std::io::Result<()> {
    let _lock = RECENT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let canonical = canonical_case_dir(case_dir);
    let entry = RecentCase {
        name: case_display_name(&canonical),
        case_dir: canonical,
        source_path: source_path.map(|p| p.display().to_string()),
        last_opened_unix: when_unix,
    };
    let cases = upsert_recent(load_recent(file), entry);
    save_recent(file, &cases)
}

/// Removes `case_dir` from `file`. Returns whether an entry was removed.
pub(crate) fn forget_recent_in(file: &Path, case_dir: &str) -> std::io::Result<bool> {
    let _lock = RECENT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let key = dedupe_key(case_dir);
    let mut cases = load_recent(file);
    let before = cases.len();
    cases.retain(|c| dedupe_key(&c.case_dir) != key && c.case_dir != case_dir);
    if cases.len() == before {
        return Ok(false);
    }
    save_recent(file, &cases)?;
    Ok(true)
}

/// Best-effort hook for the handlers: a failed write never fails the open
/// or the analysis it describes.
pub(crate) fn record_recent_case(case_dir: &Path, source_path: Option<&Path>) {
    if let Some(file) = recent_cases_file() {
        let _ = record_recent_in(&file, case_dir, source_path, now_unix());
    }
}

/// Completed record lines in a scan checkpoint (header excluded), or
/// `None` when the file is absent or too large to count cheaply.
fn checkpoint_done(case_dir: &Path) -> Option<u64> {
    let path = scan_progress_path(case_dir);
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > RECENT_CHECKPOINT_MAX_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    Some(
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.contains("\"checkpoint\""))
            .count() as u64,
    )
}

/// `(reviewed, total)` from the case DB, opened read-only with a short
/// busy timeout so a DB locked by a running scan never stalls stage 1.
fn db_counts(case_dir: &Path) -> (Option<u64>, Option<u64>) {
    let path = crate::case_db::case_db_path(case_dir);
    if !path.is_file() {
        return (None, None);
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return (None, None);
    };
    let _ = conn.busy_timeout(Duration::from_millis(100));
    let count = |sql: &str| {
        conn.query_row(sql, [], |row| row.get::<_, i64>(0))
            .ok()
            .map(|n| n.max(0) as u64)
    };
    let total = count("SELECT COUNT(*) FROM videos");
    let reviewed = count("SELECT COUNT(*) FROM review_marks").or(total.map(|_| 0));
    (reviewed, total)
}

/// Cheap state of one remembered case for the stage-1 list.
/// `phase_hint`: missing | interrupted | partial | review | finalized | empty.
pub(crate) fn recent_case_json(entry: &RecentCase, with_db: bool) -> serde_json::Value {
    let dir = Path::new(&entry.case_dir);
    let exists = dir.join("case.json").is_file();
    let mut reviewed = None;
    let mut total = None;
    let mut done = None;
    let phase_hint = if !exists {
        "missing"
    } else {
        let has_review = dir.join("review/index.html").is_file();
        let has_index = dir.join("db/video_index.json").is_file();
        let has_checkpoint = scan_progress_path(dir).is_file();
        if with_db {
            (reviewed, total) = db_counts(dir);
        }
        if has_checkpoint && !has_review {
            done = checkpoint_done(dir);
            "interrupted"
        } else if dir.join("reports/case-report.html").is_file() {
            "finalized"
        } else if has_review {
            "review"
        } else if has_index {
            "partial"
        } else {
            "empty"
        }
    };
    serde_json::json!({
        "case_dir": entry.case_dir,
        "name": if entry.name.is_empty() { case_display_name(&entry.case_dir) } else { entry.name.clone() },
        "source_path": entry.source_path,
        "last_opened_unix": entry.last_opened_unix,
        "exists": exists,
        "phase_hint": phase_hint,
        "reviewed": reviewed,
        "total": total,
        "checkpoint_done": done,
    })
}

/// Listing for `file`, most recent first, with DB counts until the time
/// budget runs out.
pub(crate) fn recent_cases_json_from(file: &Path) -> serde_json::Value {
    let started = std::time::Instant::now();
    let items: Vec<serde_json::Value> = load_recent(file)
        .iter()
        .map(|entry| {
            let with_db = started.elapsed().as_millis() < RECENT_DB_BUDGET_MS;
            recent_case_json(entry, with_db)
        })
        .collect();
    serde_json::Value::Array(items)
}

/// `GET /api/recent-cases` → `[{case_dir, name, source_path,
/// last_opened_unix, exists, phase_hint, reviewed, total, checkpoint_done}]`.
pub(crate) fn api_recent_cases() -> String {
    match recent_cases_file() {
        Some(file) => recent_cases_json_from(&file).to_string(),
        None => "[]".to_string(),
    }
}

/// `POST /api/recent-cases/forget {case_dir}` — drops one entry (the case
/// folder itself is never touched).
pub(crate) fn api_recent_cases_forget(request: &Request) -> String {
    let case_dir = body_value(&request.body, "case_dir").unwrap_or_default();
    if case_dir.trim().is_empty() {
        return api_err("케이스 폴더 경로가 비어 있습니다.");
    }
    let Some(file) = recent_cases_file() else {
        return api_err("설정 폴더를 찾을 수 없습니다.");
    };
    match forget_recent_in(&file, case_dir.trim()) {
        Ok(removed) => serde_json::json!({"ok": true, "removed": removed}).to_string(),
        Err(err) => api_err(&format!("목록을 저장하지 못했습니다: {err}")),
    }
}
