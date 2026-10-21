//! System-facing API handlers: environment probe, browse, status, cancel/shutdown, case open, audit verify.

use super::*;

/// Tools probed at startup. `(binary, version-arg)`: TSK tools print usage
/// to stderr and exit non-zero for a bare `-V`, so presence is checked by
/// binary resolution alone (arg `""` → resolve only, don't execute).
pub(crate) const PROBED_TOOLS: &[(&str, &str)] = &[
    ("ffmpeg", "-version"),
    ("ffprobe", "-version"),
    ("ewfinfo", "-V"),
    ("ewfverify", "-V"),
    ("ewfexport", "-V"),
    ("mmls", ""),
    ("fls", ""),
    ("icat", ""),
    // deepfake-lens is optional; resolution-only so the UI can grey out the
    // screening controls instead of failing at click time.
    ("deepfake-lens", ""),
];

/// The env probe spawns every probed binary with a version flag, which
/// costs seconds on a cold call — the page's header badges used to stay
/// empty for the whole probe window and read as "broken". Tool presence
/// does not change during a workstation session, so the result is cached
/// after the first probe; `?refresh=1` forces a re-probe after the
/// examiner installs a missing tool.
///
/// R6: the probes run in parallel threads (one slow tool no longer adds
/// its whole bound to every other), outside the cache lock and never
/// under `state_lock`. Concurrent cold callers share one probe (the
/// single-flight lock), and a result with a timed-out probe is not cached
/// — the next call re-probes instead of pinning "timed out" for the
/// session.
///
/// Fix round 11 L3: the entry also carries a fingerprint of
/// `<exe>/tools/bin` (file names, sizes, mtimes — one directory read), so
/// an install made outside this server (`frametrace tools install ffmpeg`
/// from a console while the workstation runs) invalidates it at the next
/// `/api/env` or `/api/tools` instead of "ffmpeg 없음" until a restart.
///
/// Fix round 12 L1: it also covers `<exe>/tools/python` (each venv's name
/// and mtime plus its `Scripts`/`bin` directory mtime), where an on-demand
/// pip tool (deepfake-lens) lands — `/api/env` reports that one through
/// `private_venv_executable`, so a console install must miss the cache too.
static ENV_CACHE: Mutex<Option<EnvCacheEntry>> = Mutex::new(None);

pub(crate) struct EnvCacheEntry {
    fingerprint: String,
    body: String,
}

/// Drops the cached env probe (a guided tool install just changed what is
/// installed — track S4), so the next `/api/env` re-probes.
pub(crate) fn invalidate_env_cache() {
    *ENV_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Names, sizes and mtimes of the files directly in `dir` (sorted), as
/// one string; empty when the directory is missing or unreadable.
pub(crate) fn tools_bin_fingerprint(dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return String::new();
    };
    let mut rows: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let meta = entry.metadata().ok();
            let len = meta.as_ref().map_or(0, |meta| meta.len());
            let modified = meta
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            format!(
                "{}\u{0}{len}\u{0}{modified}",
                entry.file_name().to_string_lossy()
            )
        })
        .collect();
    rows.sort();
    rows.join("\n")
}

fn mtime_nanos(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos())
}

/// The private venvs directly under `dir` (`tools/python`): each entry's
/// name and mtime, and the mtime of its console-script directory
/// (`Scripts` on Windows, `bin` elsewhere — a pip install adds the tool's
/// script there), sorted; empty when `dir` is missing or unreadable.
pub(crate) fn tools_python_fingerprint(dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return String::new();
    };
    let scripts = if cfg!(windows) { "Scripts" } else { "bin" };
    let mut rows: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        // Fix round 13 H2: transient sweep names (`.<tool>.old-*` while a
        // venv is rebuilt, `….rm` while one is removed) are not the
        // resolution's business and must not cause cache misses.
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !(name.starts_with('.') || name.ends_with(".rm"))
        })
        .map(|entry| {
            let path = entry.path();
            format!(
                "{}\u{0}{}\u{0}{}",
                entry.file_name().to_string_lossy(),
                mtime_nanos(&path),
                mtime_nanos(&path.join(scripts))
            )
        })
        .collect();
    rows.sort();
    rows.join("\n")
}

/// One fingerprint over `tools/bin` and `tools/python`.
pub(crate) fn tools_fingerprint(bin: &Path, python: &Path) -> String {
    format!(
        "{}\n\u{1}python\n{}",
        tools_bin_fingerprint(bin),
        tools_python_fingerprint(python)
    )
}

/// The fingerprint of this executable's `tools/bin` and `tools/python`.
fn current_tools_fingerprint() -> String {
    crate::tool_install::InstallDirs::for_current_exe()
        .map(|dirs| tools_fingerprint(&dirs.bin(), &dirs.python_root()))
        .unwrap_or_default()
}

/// The cached body when its fingerprint is `fingerprint`; a stale entry
/// (tools/bin changed since the probe) is dropped.
pub(crate) fn env_cache_hit(
    cache: &Mutex<Option<EnvCacheEntry>>,
    fingerprint: &str,
) -> Option<String> {
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(entry) if entry.fingerprint == fingerprint => Some(entry.body.clone()),
        Some(_) => {
            *guard = None;
            None
        }
        None => None,
    }
}

pub(crate) fn env_cache_store(
    cache: &Mutex<Option<EnvCacheEntry>>,
    fingerprint: String,
    body: String,
) {
    *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(EnvCacheEntry { fingerprint, body });
}

/// `/api/tools`: drops the env cache when tools/bin changed under it.
pub(crate) fn revalidate_env_cache() {
    let _ = env_cache_hit(&ENV_CACHE, &current_tools_fingerprint());
}

pub(crate) fn api_env(request: &Request) -> String {
    static PROBING: Mutex<()> = Mutex::new(());
    let refresh = query_value(&request.query, "refresh").as_deref() == Some("1");
    let fingerprint = current_tools_fingerprint();
    if !refresh && let Some(hit) = env_cache_hit(&ENV_CACHE, &fingerprint) {
        return hit;
    }
    let _single_flight = PROBING.lock().unwrap_or_else(|e| e.into_inner());
    // Another caller may have finished the probe while this one waited.
    if !refresh && let Some(hit) = env_cache_hit(&ENV_CACHE, &fingerprint) {
        return hit;
    }
    // Taken before the probe: a change during it leaves a stale
    // fingerprint, so the next call re-probes (never the other way round).
    let (body, cacheable) = probe_env_outcome();
    if cacheable {
        env_cache_store(&ENV_CACHE, fingerprint, body.clone());
    }
    body
}

/// Outcome of one tool presence probe (R6). A timeout is its own fact:
/// the binary resolved and started but did not answer within the bound —
/// not proof that it is missing or broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolProbe {
    Present,
    Missing,
    Failed,
    TimedOut,
}

impl ToolProbe {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ToolProbe::Present => "present",
            ToolProbe::Missing => "missing",
            ToolProbe::Failed => "failed",
            ToolProbe::TimedOut => "timed-out",
        }
    }
}

/// Probes every tool in parallel; returns the env JSON and whether it may
/// be cached (no probe timed out).
pub(crate) fn probe_env_outcome() -> (String, bool) {
    let probes: Vec<(&str, ToolProbe)> = std::thread::scope(|scope| {
        let handles: Vec<_> = PROBED_TOOLS
            .iter()
            .map(|(name, arg)| (*name, scope.spawn(move || tool_probe(name, arg))))
            .collect();
        handles
            .into_iter()
            .map(|(name, handle)| (name, handle.join().unwrap_or(ToolProbe::Failed)))
            .collect()
    });
    let timed_out: Vec<&str> = probes
        .iter()
        .filter(|(_, probe)| *probe == ToolProbe::TimedOut)
        .map(|(name, _)| *name)
        .collect();
    let tools: Vec<(&str, bool)> = probes
        .iter()
        .map(|(name, probe)| (*name, *probe == ToolProbe::Present))
        .collect();
    let has = |name: &str| tools.iter().any(|(n, ok)| *n == name && *ok);
    // Workflows require every binary in their set.
    let media = has("ffmpeg") && has("ffprobe");
    let ewf = has("ewfinfo") && has("ewfverify") && has("ewfexport");
    let tsk = has("mmls") && has("fls") && has("icat");
    // deepfake-lens resolves via FRAMETRACE_DEEPFAKE_LENS first, then tools/bin /
    // PATH — mirror that order so the badge matches what the pipeline
    // would actually run.
    let deepfake = std::env::var("FRAMETRACE_DEEPFAKE_LENS")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(&v).is_file() || tool_available(&v, ""))
        .unwrap_or(false)
        || has("deepfake-lens")
        // Track S: the private venv of an on-demand install.
        || crate::tool_install::private_venv_executable("deepfake-lens").is_some();
    let tools_map: serde_json::Map<String, serde_json::Value> = tools
        .iter()
        .map(|(name, ok)| (name.to_string(), serde_json::Value::Bool(*ok)))
        .collect();
    let body = serde_json::json!({
        "ok": true,
        "ffmpeg": has("ffmpeg"),
        "ffprobe": has("ffprobe"),
        "ewf": ewf,
        "deepfake": deepfake,
        "tools": tools_map,
        // R6: per-tool probe outcome — `timed-out` is distinct from
        // `missing`/`failed` (the tool may well work; it was just slow).
        "tool_probe": probes
            .iter()
            .map(|(name, probe)| (name.to_string(), serde_json::Value::from(probe.as_str())))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
        "probe_timed_out": timed_out,
        "probe_timeout_secs": crate::util::version_probe_timeout_secs(),
        "workflows": {"media": media, "e01": ewf, "filesystem": tsk},
        "hints": {
            "media": "FFmpeg 설치 후 ffmpeg/ffprobe가 PATH에 있어야 합니다. portable 배포본은 tools/bin에 복사하면 자동 인식됩니다.",
            "e01": "libewf(ewfinfo/ewfverify/ewfexport) 설치 후 PATH 등록 또는 tools/bin에 복사하십시오.",
            "filesystem": "Sleuth Kit(mmls/fls/icat) 설치 후 PATH 등록 또는 tools/bin에 복사하십시오.",
            "probe_timeout": "버전 확인이 시간 안에 끝나지 않은 도구는 '없음'이 아니라 '확인 시간 초과'입니다 — 백신 검사 등으로 첫 실행이 느릴 수 있습니다. 잠시 후 다시 확인하거나 FRAMETRACE_TOOL_PROBE_TIMEOUT(초)을 늘리십시오.",
        }
    })
    .to_string();
    (body, timed_out.is_empty())
}

pub(crate) fn tool_available(name: &str, version_arg: &str) -> bool {
    tool_probe(name, version_arg) == ToolProbe::Present
}

/// One presence probe (R6 outcome, see [`ToolProbe`]).
pub(crate) fn tool_probe(name: &str, version_arg: &str) -> ToolProbe {
    // resolve_tool_binary covers PATH plus the portable tools/bin layout, so
    // a bare binary dropped next to frametrace-app.exe is detected.
    let Ok(resolved) = crate::tool_policy::resolve_tool_binary(name, &[name]) else {
        return ToolProbe::Missing;
    };
    // An empty version_arg means "resolution is enough" — TSK tools have no
    // -V flag and print usage to stderr, so executing them tells us nothing.
    if version_arg.is_empty() {
        return ToolProbe::Present;
    }
    // P7: bounded — this runs inside HTTP handlers (/api/env, /api/start),
    // and a tool wedged on start (or held by a loader dialog) must not pin
    // the handler thread.
    let mut command = Command::new(&resolved);
    command.arg(version_arg);
    classify_tool_probe(crate::util::run_with_timeout_detailed(
        &mut command,
        Some(crate::util::version_probe_timeout_secs()),
    ))
}

/// Pure classification of a version run (R6).
pub(crate) fn classify_tool_probe(
    result: Result<std::process::Output, crate::util::RunError>,
) -> ToolProbe {
    match result {
        Ok(output) if output.status.success() => ToolProbe::Present,
        Ok(_) | Err(crate::util::RunError::Failed(_)) => ToolProbe::Failed,
        Err(crate::util::RunError::TimedOut(_)) => ToolProbe::TimedOut,
    }
}

/// Directory listing for the in-app path picker. An empty `path` returns
/// drive roots; a directory returns its children — subdirectories always,
/// first-segment E01-family files only when `files=e01`. Existence and
/// entry type are always reported, so the same endpoint also validates
/// paths typed directly into the form.
pub(crate) fn api_browse(request: &Request) -> String {
    let raw = query_value(&request.query, "path").unwrap_or_default();
    let want_files = query_value(&request.query, "files").as_deref() == Some("e01");
    browse_json(raw.trim(), want_files)
}

pub(crate) fn browse_json(raw: &str, want_files: bool) -> String {
    if raw.is_empty() {
        let entries: Vec<serde_json::Value> = drive_roots()
            .iter()
            .map(|root| browse_entry_json(root, root, true, false))
            .collect();
        return serde_json::json!({
            "ok": true, "exists": true, "is_dir": true, "path": "", "entries": entries
        })
        .to_string();
    }
    // Item 8: the picker has no approved roots of its own, so every UNC /
    // device path is refused before `metadata` could open an SMB session.
    if let Err(reason) = super::http::reject_remote_path(raw, &[]) {
        return serde_json::json!({"ok": false, "exists": false, "path": raw, "error": reason})
            .to_string();
    }
    let path = PathBuf::from(raw);
    let display = path.display().to_string();
    let Ok(meta) = std::fs::metadata(&path) else {
        return serde_json::json!({"ok": true, "exists": false, "kind": "missing", "path": display})
            .to_string();
    };
    if meta.is_file() {
        let magic = has_ewf_signature_file(&path);
        return serde_json::json!({
            "ok": true, "exists": true, "is_file": true, "is_dir": false,
            "kind": source_kind(true, &display, magic),
            "e01_magic": magic,
            "path": display,
            "name": path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
        .to_string();
    }
    let read = match std::fs::read_dir(&path) {
        Ok(read) => read,
        Err(error) => {
            return serde_json::json!({
                "ok": false, "exists": true, "is_dir": true,
                "path": display,
                "error": format!("폴더를 읽을 수 없습니다: {error}"),
            })
            .to_string();
        }
    };
    // Cap keeps the picker responsive on huge directories; the status
    // line tells the examiner the listing was truncated.
    const MAX_ENTRIES: usize = 500;
    let mut dirs: Vec<(String, String)> = Vec::new();
    let mut files: Vec<(String, String)> = Vec::new();
    let mut truncated = false;
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        let shown = is_dir || (want_files && is_e01_first_segment(&name));
        if !shown {
            continue;
        }
        if dirs.len() + files.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let full = path.join(&name).display().to_string();
        if is_dir {
            dirs.push((name, full));
        } else {
            files.push((name, full));
        }
    }
    let by_name =
        |a: &(String, String), b: &(String, String)| a.0.to_lowercase().cmp(&b.0.to_lowercase());
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    let entries: Vec<serde_json::Value> = dirs
        .iter()
        .map(|(name, full)| {
            // A directory holding db/case.db is an openable case — the
            // picker surfaces it as one-click openable.
            let is_case = crate::case_db::case_db_path(Path::new(full)).is_file();
            browse_entry_json(name, full, true, is_case)
        })
        .chain(
            files
                .iter()
                .map(|(name, full)| browse_entry_json(name, full, false, false)),
        )
        .collect();
    let mut body = serde_json::json!({
        "ok": true, "exists": true, "is_dir": true, "kind": "folder",
        "is_case": crate::case_db::case_db_path(&path).is_file(),
        "path": display,
        "entries": entries,
        "truncated": truncated,
    });
    // A case folder also reports its canonical form so the start screen
    // can match it against the recent-projects list (which stores
    // canonical paths) without listing the same case twice.
    if crate::case_db::case_db_path(&path).is_file()
        && let Some(map) = body.as_object_mut()
    {
        map.insert(
            "canonical".to_string(),
            serde_json::Value::String(super::recent::canonical_case_dir(&path)),
        );
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && let Some(map) = body.as_object_mut()
    {
        map.insert(
            "parent".to_string(),
            serde_json::Value::String(parent.display().to_string()),
        );
    }
    body.to_string()
}

pub(crate) fn browse_entry_json(
    name: &str,
    path: &str,
    dir: bool,
    is_case: bool,
) -> serde_json::Value {
    serde_json::json!({"name": name, "path": path, "dir": dir, "is_case": is_case})
}

/// Stage-1 auto-detection of the evidence kind for an existing path:
/// a directory is `folder`; a file is `e01` when its name is a first
/// E01-family segment or its header carries an EWF signature, otherwise
/// `other` (not accepted as evidence input).
pub(crate) fn source_kind(is_file: bool, name: &str, ewf_magic: bool) -> &'static str {
    if !is_file {
        "folder"
    } else if ewf_magic || is_e01_first_segment(name) {
        "e01"
    } else {
        "other"
    }
}

/// EWF file signatures: EWF1 `EVF\t\r\n\xff\0` (E01) / `LVF…` (L01) and
/// EWF2 `EVF2\r\n\x81\0` (Ex01) / `LEF2\r\n\x81\0` (Lx01).
pub(crate) fn has_ewf_signature(head: &[u8]) -> bool {
    const SIGS: [&[u8; 8]; 4] = [
        b"EVF\x09\x0d\x0a\xff\x00",
        b"LVF\x09\x0d\x0a\xff\x00",
        b"EVF2\x0d\x0a\x81\x00",
        b"LEF2\x0d\x0a\x81\x00",
    ];
    head.len() >= 8 && SIGS.iter().any(|sig| &head[..8] == sig.as_slice())
}

/// Reads at most the first 8 bytes of `path` (read-only). S2: only a
/// regular file of at least 8 bytes is opened — a device, FIFO or other
/// special file can block `read_exact` forever (and pin the handler).
pub(crate) fn has_ewf_signature_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !signature_probe_allowed(&meta) {
        return false;
    }
    let mut head = [0u8; 8];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .map(|_| has_ewf_signature(&head))
        .unwrap_or(false)
}

/// Whether the EWF signature probe may open a file with this metadata: a
/// regular file large enough to hold the 8-byte signature.
pub(crate) fn signature_probe_allowed(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_file() && meta.len() >= 8
}

/// `true` for the first segment of a split forensic image family. The
/// examiner picks `.E01`/`.Ex01`/`.L01`/`.S01`; later segments (.E02…)
/// are opened implicitly by libewf, so the picker hides them.
pub(crate) fn is_e01_first_segment(name: &str) -> bool {
    matches!(
        name.rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "e01" | "ex01" | "l01" | "s01"
    )
}

#[cfg(windows)]
pub(crate) fn drive_roots() -> Vec<String> {
    (b'A'..=b'Z')
        .map(|letter| format!("{}:\\", letter as char))
        .filter(|root| Path::new(root).exists())
        .collect()
}

#[cfg(not(windows))]
pub(crate) fn drive_roots() -> Vec<String> {
    vec!["/".to_string()]
}

pub(crate) fn api_status(state: &SharedState) -> String {
    // Taken before the main lock: index_generation stats files and locks
    // briefly on its own.
    let bound_case = state_lock(state).case_dir.clone();
    let index_generation = bound_case
        .as_deref()
        .map(|dir| index_generation(state, dir))
        .unwrap_or(0);
    // Everything that touches the disk — the case DB (SQLite: reaping,
    // schema check and a query, each of which can wait out the busy
    // timeout while a running job writes) and the marks queue directory —
    // runs AFTER the state lock is released. Holding the global lock across
    // those reads stalled every other endpoint for seconds while a carve
    // job was writing (measured: up to 4 s per /api/status poll).
    let (
        mut body,
        case_dir,
        byte_progress,
        rehash_progress,
        enrichment_step,
        recent_clips,
        recovering_case,
    ) = {
        let guard = state_lock(state);
        (
            status_snapshot(&guard),
            guard.case_dir.clone(),
            guard.byte_progress.clone(),
            guard.rehash_progress,
            guard.enrichment,
            // Cloned here, rendered below: a job's JSON stats its growing
            // output / `.part` and reads the join progress file.
            guard
                .clip_jobs
                .iter()
                .rev()
                .take(8)
                .cloned()
                .collect::<Vec<_>>(),
            guard.finalize_recovering.clone(),
        )
    };
    // UX-09: clip exports and joins run beside `busy`; running + recent ones.
    body["clip_jobs"] = recent_clips
        .iter()
        .map(clip_job_json)
        .collect::<Vec<_>>()
        .into();
    let progress = match rehash_progress {
        // R6-S4: an existing raw image is being re-hashed in-process.
        Some((done, total)) => serde_json::json!({
            "job_type": "raw-rehash",
            "done": done,
            "total": total,
            "elapsed_secs": 0,
            "eta_secs": 0,
        }),
        None => progress_json(case_dir.as_deref(), byte_progress.as_ref()),
    };
    // UX-02: which phase-2 step is enriching the partial index. `done` /
    // `total` are the running job's unit progress when the step reports
    // one (files hashed), else the step position within the plan.
    let enrichment = enrichment_step.map(|step| {
        let (done, total, unit) = match (
            progress.get("done").and_then(|v| v.as_u64()),
            progress.get("total").and_then(|v| v.as_u64()),
        ) {
            (Some(done), Some(total)) if total > 0 => (done, total, "units"),
            _ => (
                step.stage.saturating_sub(1) as u64,
                step.stages as u64,
                "steps",
            ),
        };
        serde_json::json!({
            "step": step.step,
            "label": step.label,
            "stage": step.stage,
            "stages": step.stages,
            "done": done,
            "total": total,
            "unit": unit,
        })
    });
    body["progress"] = progress;
    body["enrichment"] = enrichment.unwrap_or(serde_json::Value::Null);
    body["index_generation"] = index_generation.into();
    // Marks accepted while a hash/make-review step held the case DB;
    // applied automatically right after that step.
    body["pending_marks"] = case_dir
        .as_deref()
        .map(pending_marks_count)
        .unwrap_or(0)
        .into();
    // Save protocol v2: ids a queued import could not apply because the
    // case had moved on (journaled in marks-pending/conflicts.jsonl, never
    // dropped). `{count, ids}` with at most 50 ids; the viewer resolves
    // them by saving the id again with a current base.
    body["mark_conflicts"] = case_dir
        .as_deref()
        .map(super::jobs::pending_mark_conflicts)
        .unwrap_or_else(|| serde_json::json!({"count": 0, "ids": []}));
    // R10-S2: package folders an incomplete finalize left in reports/
    // (`…_INCOMPLETE` or marked beside) — "최종화 미완료", never final.
    body["incomplete_packages"] = case_dir
        .as_deref()
        .map(super::media::incomplete_package_dirs)
        .unwrap_or_default()
        .iter()
        .map(|dir| dir.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .into();
    // R10-S2: whether `package_dir` is such a package (an incomplete
    // finalize sets it to the renamed folder).
    let package_incomplete = body["package_dir"]
        .as_str()
        .is_some_and(|dir| crate::package::finalize_incomplete_reason(Path::new(dir)).is_some());
    body["package_incomplete"] = package_incomplete.into();
    // R12-S2: a finalize journal on disk (an interrupted finalize not yet
    // fully recovered, or — `active` — the finalize running now).
    // R13-S7: `recovering` while the background start-up recovery runs.
    let active = body["phase"].as_str() == Some("finalizing");
    body["finalize_journal"] = case_dir
        .as_deref()
        .map(|dir| {
            let recovering = recovering_case.as_deref() == Some(dir);
            super::finalize_journal::status_value(dir, active, recovering)
        })
        .unwrap_or(serde_json::Value::Null);
    body.to_string()
}

/// The in-memory part of /api/status, built under the state lock (no I/O).
fn status_snapshot(guard: &JobState) -> serde_json::Value {
    let steps: Vec<&str> = guard.steps.iter().map(|step| step.as_str()).collect();
    let current = guard
        .steps
        .iter()
        .position(|step| *step == StepStatus::Running)
        .map(|index| index.to_string())
        .unwrap_or_default();
    let logs: Vec<String> = guard.logs.iter().rev().take(60).rev().cloned().collect();
    let warnings: Vec<serde_json::Value> = guard
        .warnings
        .iter()
        .map(|(step, message)| {
            serde_json::json!({
                "step": step,
                "step_name": guard.step_names.get(*step).copied().unwrap_or_default(),
                "message": message,
            })
        })
        .collect();
    let opt = |value: &Option<PathBuf>| {
        value
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
    };
    serde_json::json!({
        "ok": true,
        "app": "frametrace",
        // SEC-04: a public fingerprint, never the raw mutation nonce.
        "instance": instance_fingerprint(&guard.instance),
        "has_job": guard.phase != "idle",
        "phase": guard.phase,
        "restored": guard.restored,
        "busy": guard.busy,
        "steps": steps,
        // FOR-08: non-fatal step failures; non-empty means the run is
        // "done with warnings", not a clean pass.
        "warnings": warnings,
        "step_names": guard.step_names,
        "current": current,
        "logs": logs,
        "case_dir": opt(&guard.case_dir),
        "package_dir": opt(&guard.package_dir),
        "error": guard.error,
        // R11-S5: the case report could not be re-rendered after a failed
        // finalize (it still names that finalize's unfinished sign-off).
        "report_stale": guard.report_stale,
        // R11-S6: the last failed finalize left something that must not
        // be read as final (sign-off not restored, report stale, abandon
        // record missing); `finalize_incomplete_detail` says what.
        "finalize_incomplete": guard.finalize_incomplete.is_some(),
        "finalize_incomplete_detail": guard.finalize_incomplete,
        // R12-S2: an interrupted finalize (journal found at server start,
        // open-case or the next finalize) was recovered; the detail says
        // what was done and what still needs a look ("확인 필요").
        "finalize_recovered": guard.finalize_recovered.is_some(),
        "finalize_recovered_detail": guard.finalize_recovered,
        // UX-06: which kind of job holds `busy` (null when idle), and the
        // current/last background case job with its stage and outcome.
        "job_kind": status_job_kind(guard),
        "job": guard.case_job.as_ref().map(case_job_json),
        // `clip_jobs` is filled in by api_status outside the lock.
        "clips_running": guard.clip_jobs.iter().filter(|job| job.state == "running").count(),
        // S1: export-selected / capture-frame writes in flight.
        "writes_running": guard.write_ops,
        // R5-S6: the last E01 run stopped on a raw image it could not
        // confirm; `POST /api/reimport-raw` discards it and re-runs.
        "raw_reimport": guard.raw_reimport.as_ref().map(|pending| serde_json::json!({
            "raw_path": pending.raw_path.to_string_lossy(),
            "reason": pending.reason,
            "endpoint": "/api/reimport-raw",
        })),
    })
}

/// `job_kind` for /api/status: the background case job when one runs,
/// else the pipeline / finalize / short busy job, else `null`.
pub(crate) fn status_job_kind(guard: &JobState) -> Option<&'static str> {
    if !guard.busy {
        return None;
    }
    if let Some(job) = guard.case_job.as_ref().filter(|job| job.state == "running") {
        return Some(job.kind);
    }
    Some(match guard.phase {
        "running" => "pipeline",
        "review-partial" => "enrichment",
        "finalizing" => "finalize",
        _ => "task",
    })
}

/// Reads the newest running job's progress from the case DB so the UI can
/// render a real progress bar + ETA instead of an indeterminate spinner.
/// Failures degrade to `"progress":null` — status must never 500 because
/// a progress read raced the job writer.
pub(crate) fn progress_json(
    case_dir: Option<&Path>,
    byte_progress: Option<&(PathBuf, Option<u64>)>,
) -> serde_json::Value {
    let Some(case_dir) = case_dir else {
        return serde_json::Value::Null;
    };
    if !case_dir.join("case.json").is_file() || !crate::case_db::case_db_path(case_dir).is_file() {
        return byte_progress_json(byte_progress);
    }
    // R10: read-only, write-free, bounded wait. A writer holding the DB
    // past STATUS_DB_BUSY no longer stalls the poll (it used to wait out
    // the 5 s busy timeout, beyond the shell's 1 s status timeout): the
    // last progress seen for this case is reported, marked `stale`.
    STATUS_PROGRESS_READS.with(|reads| reads.set(reads.get() + 1));
    let job = match crate::case_db::latest_running_job_quick(case_dir, STATUS_DB_BUSY) {
        Ok(job) => {
            remember_progress(case_dir, job.as_ref());
            job
        }
        Err(_) => {
            STATUS_PROGRESS_BUSY_FALLBACKS.with(|count| count.set(count.get() + 1));
            if let Some(mut last) = remembered_progress(case_dir) {
                last["stale"] = true.into();
                return last;
            }
            None
        }
    };
    let Some(job) = job else {
        // Fallback: external-tool steps (e.g. ewfexport) don't touch the
        // jobs table — report output bytes seen so far as an indeterminate
        // progress signal.
        return byte_progress_json(byte_progress);
    };
    job_progress_json(&job)
}

/// Progress JSON for one running job row.
fn job_progress_json(job: &crate::case_db::JobProgress) -> serde_json::Value {
    let now = crate::util::now_unix().unwrap_or(job.updated_unix);
    let elapsed = now.saturating_sub(job.started_unix).max(1);
    let total = job.total_units.unwrap_or(0);
    let done = job.completed_units;
    // ETA from the average rate since job start; caller treats it as an
    // estimate ("계산 중" below some completeness) and rounds for display.
    let eta = if total > 0 && done > 0 && done < total {
        let rate = done as f64 / elapsed as f64;
        if rate > 0.0 {
            ((total - done) as f64 / rate) as u64
        } else {
            0
        }
    } else {
        0
    };
    serde_json::json!({
        "job_type": job.job_type,
        "done": done,
        "total": total,
        "elapsed_secs": elapsed,
        "eta_secs": eta,
    })
}

thread_local! {
    /// R4-B20: bounded (`latest_running_job_quick`) progress reads made by
    /// `/api/status` on this thread — the only case-DB access it makes.
    pub(crate) static STATUS_PROGRESS_READS: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
    /// R4-B20: of those, reads that found the DB locked and fell back to
    /// the remembered (stale) progress instead of waiting.
    pub(crate) static STATUS_PROGRESS_BUSY_FALLBACKS: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// R10: how long one `/api/status` progress read may wait for a case-DB
/// lock (the whole poll targets < 200 ms under a carve writer).
pub(crate) const STATUS_DB_BUSY: Duration = Duration::from_millis(100);

/// Last progress JSON read per case (R10 fallback when the DB is busy).
/// Keyed by case so polls of different cases (tests, a case switch) never
/// clear each other's entry; bounded so it cannot grow without limit.
static LAST_PROGRESS: Mutex<Vec<(PathBuf, serde_json::Value)>> = Mutex::new(Vec::new());
const LAST_PROGRESS_CAP: usize = 16;

fn remember_progress(case_dir: &Path, job: Option<&crate::case_db::JobProgress>) {
    let mut last = LAST_PROGRESS.lock().unwrap_or_else(|e| e.into_inner());
    last.retain(|(dir, _)| dir != case_dir);
    if let Some(job) = job {
        if last.len() >= LAST_PROGRESS_CAP {
            last.remove(0);
        }
        last.push((case_dir.to_path_buf(), job_progress_json(job)));
    }
}

fn remembered_progress(case_dir: &Path) -> Option<serde_json::Value> {
    LAST_PROGRESS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(dir, _)| dir == case_dir)
        .map(|(_, value)| value.clone())
}

/// Byte-level fallback for steps whose work happens inside an external
/// tool (ewfexport): the pipeline records the output path being written
/// and an optional total (E01 source size ≈ lower bound of raw bytes).
pub(crate) fn byte_progress_json(
    byte_progress: Option<&(PathBuf, Option<u64>)>,
) -> serde_json::Value {
    let Some((path, total)) = byte_progress else {
        return serde_json::Value::Null;
    };
    let done = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if done == 0 {
        return serde_json::Value::Null;
    }
    let total = match total {
        Some(t) if *t > done => *t,
        _ => 0, // unknown total → UI shows indeterminate + bytes written
    };
    serde_json::json!({
        "job_type": "import-e01",
        "done": done,
        "total": total,
        "elapsed_secs": 0,
        "eta_secs": 0,
    })
}

/// `POST /api/cancel`: stops whatever holds `busy` — a guided tool install
/// through its own latch (`cancelled: "tool-install"` with `requested` —
/// false when the worker had already ended and nothing was asked — or
/// `ok:false` `code: "not_cancellable"` while files move into place), any other job
/// through `cancel_requested` (`cancelled: "job"`) — else the running clip
/// exports / write ops.
pub(crate) fn api_cancel(state: &SharedState) -> String {
    let mut guard = state_lock(state);
    // Fix round 11 M1: a guided tool install holds `busy` but never reads
    // `cancel_requested` — it watches its own Progress latch. Ask that
    // latch (the same as POST /api/tools/install-cancel) instead of the
    // job path, which would log a false "중단 요청" line and never stop it.
    let install = guard
        .tool_install
        .as_ref()
        .filter(|job| guard.busy && job.state == "running")
        .map(|job| {
            (
                job.name.clone(),
                Arc::clone(&job.progress),
                super::tools::install_job_json(job),
            )
        });
    if let Some((name, progress, snapshot)) = install {
        let finishing = matches!(
            progress.stage(),
            crate::tool_install::Stage::Done | crate::tool_install::Stage::Failed
        );
        // A worker that already ended releases `busy` momentarily: nothing
        // to ask, nothing to log.
        let asked = if finishing {
            Ok(false)
        } else {
            progress.request_cancel().map(|()| true)
        };
        return match asked {
            Ok(asked) => {
                if asked {
                    guard
                        .logs
                        .push(format!("도구 설치({name}) 중단 요청을 받았습니다."));
                }
                serde_json::json!({
                    "ok": true,
                    "cancelled": "tool-install",
                    "job_kind": "tool-install",
                    // Fix round 15: false when the install had already
                    // ended (Done/Failed) and nothing was asked — the quit
                    // dialog must not then say it was cancelled.
                    "requested": asked,
                    "install": snapshot,
                    "clips_cancelled": 0,
                    "clips_running": 0,
                })
                .to_string()
            }
            Err(message) => serde_json::json!({
                "ok": false,
                "code": "not_cancellable",
                "error": message,
                "job_kind": "tool-install",
                "install": snapshot,
            })
            .to_string(),
        };
    }
    if guard.busy {
        // Cancel only the job that holds `busy` (finding 2): a clip export
        // runs beside it and is unrelated — stopping a re-hash must not
        // kill it. Clips are cancelled per job (export-clip-cancel) or by a
        // cancel issued while nothing else runs.
        guard.cancel_requested = true;
        let job_kind = status_job_kind(&guard);
        let clips_running = guard
            .clip_jobs
            .iter()
            .filter(|job| job.state == "running")
            .count();
        guard
            .logs
            .push("중단 요청을 받았습니다. 현재 단계를 멈추는 중입니다.".to_string());
        return serde_json::json!({
            "ok": true,
            "cancelled": "job",
            "job_kind": job_kind,
            "clips_cancelled": 0,
            "clips_running": clips_running,
        })
        .to_string();
    }
    // Nothing holds `busy`: the global cancel stops running clip exports
    // (UX-09) so "중단 후 종료" never leaves an ffmpeg behind, and (R3)
    // asks every write op to stop — export-selected checks its flag
    // between items and still writes a manifest marked cancelled.
    let clips = cancel_all_clip_jobs(&mut guard);
    let writes = cancel_write_ops(&mut guard);
    if clips > 0 || writes > 0 {
        if clips > 0 {
            guard.logs.push(format!(
                "내보내기·이어붙이기 {clips}건 중단 요청을 받았습니다."
            ));
        }
        if writes > 0 {
            guard.logs.push(format!(
                "선택 내보내기·프레임 캡처 {writes}건 중단 요청을 받았습니다 — 진행 중인 항목을 마치고 멈춥니다."
            ));
        }
        return serde_json::json!({
            "ok": true,
            "cancelled": if clips > 0 { "clips" } else { "writes" },
            "job_kind": null,
            "clips_cancelled": clips,
            "clips_running": clips,
            "writes_cancelled": writes,
            "writes_running": guard.write_ops,
        })
        .to_string();
    }
    "{\"ok\":false,\"error\":\"진행 중인 작업이 없습니다.\"}".to_string()
}

/// POST /api/shutdown — the only clean way to stop the windowed
/// `frametrace-app.exe` (no console to close). Refuses while a job runs so
/// a mid-pipeline exit cannot truncate audit/carve output; the UI offers a
/// cancel-then-shutdown flow for that case.
pub(crate) fn api_shutdown(state: &SharedState) -> String {
    let guard = state_lock(state);
    let clips_running = guard.clip_jobs.iter().any(|job| job.state == "running");
    // S1: an export-selected / capture-frame still writing into the case
    // counts as running work too (it is short; the UI retries).
    if guard.busy || clips_running || guard.write_ops > 0 {
        return "{\"ok\":false,\"running\":true,\"error\":\"작업이 진행 중입니다 — 중단 후 종료하십시오.\"}"
            .to_string();
    }
    guard.request_shutdown();
    "{\"ok\":true}".to_string()
}

pub(crate) fn api_open_case(request: &Request, state: &SharedState) -> String {
    let dir_text = body_value(&request.body, "case_dir").unwrap_or_default();
    if dir_text.trim().is_empty() {
        return "{\"ok\":false,\"error\":\"케이스 폴더 경로를 입력하십시오.\"}".to_string();
    }
    // R19-S1 (P32): the bound case folder is normalized — absolute, no
    // trailing separator, a drive root `X:\.` — so every path the status
    // shows and every command built from it is the same, argv-safe text
    // (`E:\` or `C:\cases\c 1\` typed with a trailing backslash made the
    // pasted command's closing quote literal). Lexical only (no disk).
    // R20-S1 (P34): the UNC/device/reserved check runs on the typed text
    // AND on the normalized path, before any filesystem call — the R19
    // check on the text alone let `\\?\C:\x\//srv/sh` normalize to the
    // UNC `//srv/sh` and the `case.json` probe below open an SMB session.
    let roots = super::http::approved_path_roots(state);
    let case_dir = match super::state::bind_path(Path::new(dir_text.trim()), |text| {
        super::http::reject_remote_path(text, &roots)
    }) {
        Ok(case_dir) => case_dir,
        Err(reason) => return api_err(reason),
    };
    if !case_dir.join("case.json").is_file() {
        return "{\"ok\":false,\"error\":\"해당 폴더에 case.json이 없습니다 — 기존 케이스 폴더가 아닙니다.\"}"
            .to_string();
    }
    let has_review = case_dir.join("review/index.html").is_file();
    // A case whose enrichment pass was cancelled (or failed) before
    // make-review still has a reviewable index (UX-02).
    let has_index = case_dir.join("db/video_index.json").is_file();
    // R4-B5: pages this user's FrameTrace did not generate open without
    // script — say so instead of leaving a blank dashboard unexplained.
    let unverified_pages = has_review
        && !crate::util::generated_pages::page_is_trusted(&case_dir, "review/index.html");
    // R13-S10: the refusals are checked BEFORE a recovery changes the
    // case (R12 recovered first and then could refuse the open, so what
    // the recovery did was only in the job log).
    {
        let guard = state_lock(state);
        if guard.busy {
            return "{\"ok\":false,\"error\":\"분석이 진행 중입니다.\"}".to_string();
        }
        if let Some(refusal) = side_work_refusal(&guard, "케이스를 여십시오") {
            return api_err(&refusal);
        }
    }
    // R12-S2 (P13): an interrupted finalize of this case is cleaned up
    // before the case is bound — holding `busy` (so no finalize of this
    // workstation is running) and outside the state lock. R13: only one
    // its lane entry pairs with (see finalize_journal).
    let recovery = if super::finalize_journal::journal_path(&case_dir).exists() {
        let _busy = match try_acquire_busy(state) {
            Ok(guard) => guard,
            Err(err) => return api_err(&err),
        };
        super::finalize_journal::recover(&case_dir, state, crate::package::rename_retry_budget())
    } else {
        None
    };
    // Reopening the same case must keep extra media roots the earlier
    // session recorded (e.g. a carving workspace linked into the case) —
    // resetting to just the case dir would 403 artifacts that resolve
    // through that root. Read (file + probes) before the state lock.
    let prior_roots = restore_session()
        .and_then(|(dir, roots)| (dir == case_dir).then_some(roots))
        .unwrap_or_else(|| vec![case_dir.clone()]);
    let mut guard = state_lock(state);
    // R13-S10: a refusal that still comes now (a job or a clip export
    // started meanwhile) carries what the recovery did.
    let recovered_note = recovery
        .as_ref()
        .map(|recovery| format!(" (참고: {})", recovery.detail))
        .unwrap_or_default();
    if guard.busy {
        return api_err(&format!("분석이 진행 중입니다.{recovered_note}"));
    }
    // Clip exports (and S1 write ops) write into the bound case and block
    // its finalize; a switch while one runs would leave the new case's
    // finalize refused for the old case's export (and the export landing
    // under a case no longer shown).
    if let Some(refusal) = side_work_refusal(&guard, "케이스를 여십시오") {
        return api_err(&format!("{refusal}{recovered_note}"));
    }
    if guard.case_dir.as_deref() != Some(case_dir.as_path()) {
        // Finished clip jobs belong to the previous binding.
        guard.clip_jobs.clear();
        // R11-S5/S6: so did the last finalize's leftovers.
        guard.report_stale = false;
        guard.finalize_incomplete = None;
        guard.finalize_recovered = None;
    }
    if let Some(recovery) = recovery {
        super::jobs::apply_recovery_locked(&mut guard, recovery);
    }
    guard.case_dir = Some(case_dir.clone());
    let extra_root = prior_roots.iter().find(|root| **root != case_dir).cloned();
    guard.media_roots = prior_roots;
    // An explicit open is a deliberate choice — the restored-session
    // prompt must not reappear over it.
    guard.restored = false;
    guard.warnings = Vec::new();
    guard.enrichment = None;
    // The last recover/carve outcome belonged to the previous binding.
    guard.case_job = None;
    // R4-B8: so did the last package (the review-ready branch used to keep
    // it, so "open package folder" offered the previous case's package and
    // /api/open-folder admitted it as an approved root).
    guard.package_dir = None;
    guard.raw_reimport = None;
    guard.error = None;
    if has_review {
        guard.phase = "review-ready";
        guard.steps = [StepStatus::Done; 5];
        guard
            .logs
            .push(format!("기존 케이스를 열었습니다: {}", case_dir.display()));
    } else if has_index {
        guard.phase = "review-partial";
        guard.steps = [
            StepStatus::Done,
            StepStatus::Done,
            StepStatus::Done,
            StepStatus::Pending,
            StepStatus::Pending,
        ];
        guard.package_dir = None;
        guard.raw_reimport = None;
        guard.logs.push(format!(
            "케이스를 열었습니다 (색인만 있음 — 보강·리뷰 생성 전): {}",
            case_dir.display()
        ));
    } else {
        // Without a review bundle the new binding has nothing to show —
        // leaving a previous case's "review-ready" phase would report a
        // state this case never reached.
        guard.phase = "idle";
        guard.steps = [StepStatus::Pending; 5];
        guard.package_dir = None;
        guard.raw_reimport = None;
        guard.logs.push(format!(
            "케이스를 열었습니다 (검토 결과 없음): {}",
            case_dir.display()
        ));
    }
    if unverified_pages {
        let message = "이 케이스의 대시보드·보고서 페이지는 이 컴퓨터의 FrameTrace가 만든 것으로 확인되지 않아 스크립트 없이(보기 전용) 열립니다 — 최종화하거나 다시 분석하면 새로 생성됩니다. 검토 화면(뷰어)은 영향을 받지 않습니다.".to_string();
        guard.logs.push(format!("경고: {message}"));
        guard.warnings.push((4, message));
    }
    drop(guard);
    save_session(&case_dir, extra_root.as_deref());
    record_recent_case(&case_dir, None);
    serde_json::json!({"ok": true, "has_review": has_review, "has_index": has_index}).to_string()
}

/// `POST /api/verify-audit`: verify every chained audit log under
/// `case_dir/evidence/logs/*.jsonl` in-process and report per-log and
/// overall integrity (keyed vs structural-only vs failed).
pub(crate) fn api_verify_audit(state: &SharedState) -> String {
    let case_dir = {
        let guard = state_lock(state);
        guard.case_dir.clone()
    };
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let logs_dir = case_dir.join("evidence/logs");
    if let Err(err) = std::fs::read_dir(&logs_dir) {
        return serde_json::json!({
            "ok": false,
            "error": format!("감사 로그 디렉터리를 읽지 못했습니다: {err}"),
        })
        .to_string();
    }
    // R9 P4: the WHOLE case — every chained lane under artifacts/** as well
    // as evidence/logs (R8 checked evidence/logs only, so a tampered
    // artifacts lane never reached the workstation badge).
    let log_paths: Vec<PathBuf> = audit::case_lane_paths(&case_dir);
    if log_paths.is_empty() {
        return "{\"ok\":false,\"error\":\"검증할 감사 로그가 없습니다.\"}".to_string();
    }
    let mut logs_json = Vec::new();
    let mut failed = 0usize;
    let mut any_structural_only = false;
    let mut any_not_verified = false;
    let mut total_entries = 0usize;
    for path in &log_paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let rel = path
            .strip_prefix(&case_dir)
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| name.clone());
        match audit::verify_chained_jsonl(path) {
            Ok(result) => {
                total_entries += result.entries;
                let (unauthenticated_lines, other_pc_lines) = result.trust_counts();
                // R8 P0/P1/P2: one mapping for every lane consumer — keys
                // held here + any line not authenticated here = NOT
                // VERIFIED (naming other PCs' keys); no keys = structural
                // (self-consistency only).
                let trust = result.lane_trust();
                match &trust {
                    audit::LaneTrust::Authenticated => {}
                    audit::LaneTrust::SelfConsistentOnly(_) => any_structural_only = true,
                    audit::LaneTrust::NotVerified { .. } => any_not_verified = true,
                }
                let other_pc_key_ids = match &trust {
                    audit::LaneTrust::NotVerified {
                        other_pc_key_ids, ..
                    } => other_pc_key_ids.clone(),
                    _ => Vec::new(),
                };
                logs_json.push(serde_json::json!({
                    "name": name,
                    // R9 P4: case-relative path (artifacts lanes share file
                    // names with evidence/logs lanes, e.g. export-log.jsonl).
                    "path": rel,
                    // R9 (contract with track U): numeric counts so the
                    // badge never parses the reason text.
                    "unauthenticated_lines": unauthenticated_lines,
                    "other_pc_lines": other_pc_lines,
                    "entries": result.entries,
                    "integrity": if trust.passes() {
                        trust.label()
                    } else {
                        "not-verified"
                    },
                    "trust": trust.reason(),
                    "keys_held": result.requirement.keys_held(),
                    "other_pc_key_ids": other_pc_key_ids,
                    "rewrite_not_excluded": result.rewrite_not_excluded,
                    "keyed_entries": result.keyed_entries,
                    "unauthenticated_keyed_entries": result.unauthenticated_keyed_entries,
                    "warnings": result.warnings,
                }));
            }
            // R9 P5/I9: an audit key configuration that exists but cannot
            // be read is NOT VERIFIED (fail closed) — never "failed"
            // (a tamper verdict).
            Err(err) if audit::is_key_config_error(&err) => {
                any_not_verified = true;
                logs_json.push(serde_json::json!({
                    "name": name,
                    "path": rel,
                    "entries": 0,
                    "integrity": "not-verified",
                    "trust": err,
                    "keys_held": true,
                    "key_config_error": true,
                    "other_pc_key_ids": Vec::<String>::new(),
                    "unauthenticated_lines": 0,
                    "other_pc_lines": 0,
                    "warnings": Vec::<String>::new(),
                }));
            }
            Err(err) => {
                failed += 1;
                logs_json.push(serde_json::json!({
                    "name": name,
                    "path": rel,
                    "entries": 0,
                    "integrity": "failed",
                    "error": err,
                    "warnings": Vec::<String>::new(),
                }));
            }
        }
    }
    let overall = if failed > 0 {
        "integrity-failed"
    } else if any_not_verified {
        "integrity-not-verified"
    } else if any_structural_only {
        "integrity-structural-only"
    } else {
        "integrity-keyed"
    };
    serde_json::json!({
        "ok": failed == 0,
        "verified": log_paths.len() - failed,
        "failed": failed,
        "entries": total_entries,
        "integrity": overall,
        "logs": logs_json,
    })
    .to_string()
}

/// Stat budget for `checkpoint.fingerprint_matches` (one `stat` per line).
const CASE_INFO_STAT_CAP: usize = 20_000;

/// `GET /api/case-info[?case_dir=...]` (UX-10): what a case already holds,
/// derived read-only from `db/scan-progress.jsonl` and the index — no DB
/// open, no writes. Defaults to the bound case; `case_dir` inspects another
/// case folder (it must hold case.json) before opening it.
pub(crate) fn api_case_info(request: &Request, state: &SharedState) -> String {
    let (bound, phase) = {
        let guard = state_lock(state);
        (guard.case_dir.clone(), guard.phase)
    };
    let requested_text = query_value(&request.query, "case_dir")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if let Some(text) = requested_text.as_deref()
        && let Err(reason) =
            super::http::reject_remote_path(text, &super::http::approved_path_roots(state))
    {
        return api_err(reason);
    }
    let requested = requested_text.map(PathBuf::from);
    let Some(case_dir) = requested.or_else(|| bound.clone()) else {
        return api_err("열린 케이스가 없습니다.");
    };
    if !case_dir.join("case.json").is_file() {
        return api_err("해당 폴더에 case.json이 없습니다 — 케이스 폴더가 아닙니다.");
    }
    // R21-S3: the bound folder is in normalized (plain) form; the typed
    // text is compared in that form too (lexical — no disk).
    let is_bound = bound.as_deref().is_some_and(|bound| {
        bound == case_dir
            || crate::paste_path::normalize_checked(&case_dir).as_deref() == Some(bound)
    });
    let video_count = if is_bound {
        cached_index(state, &case_dir)
            .ok()
            .and_then(|index| index_video_count(&index))
    } else {
        crate::util::read_to_string(&case_dir.join("db/video_index.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|index| index_video_count(&index))
    };
    let mut info = case_info_json(
        &case_dir,
        video_count,
        &super::http::approved_path_roots(state),
    );
    if let Some(map) = info.as_object_mut() {
        map.insert("ok".into(), serde_json::Value::Bool(true));
        map.insert(
            "case_dir".into(),
            serde_json::json!(case_dir.to_string_lossy()),
        );
        // The live phase only describes the bound case; another folder
        // keeps the phase derived from its files.
        if is_bound {
            map.insert("phase".into(), serde_json::json!(phase));
        }
        map.insert("bound".into(), serde_json::Value::Bool(is_bound));
    }
    info.to_string()
}

fn index_video_count(index: &serde_json::Value) -> Option<u64> {
    index
        .get("videos")
        .and_then(|videos| videos.as_array())
        .map(|videos| videos.len() as u64)
}

/// Pure part of [`api_case_info`]. `checkpoint` is `null` without a
/// checkpoint file; otherwise `done` = completed record lines, `total` =
/// files in the last completed index (`null` before any index exists —
/// the checkpoint itself never records a total), `fingerprint_matches` =
/// records whose file still has the recorded size+mtime (the oracle a
/// rerun uses to reuse them instead of re-hashing).
pub(crate) fn case_info_json(
    case_dir: &Path,
    video_count: Option<u64>,
    approved_roots: &[PathBuf],
) -> serde_json::Value {
    let has_index = case_dir.join("db/video_index.json").is_file();
    let has_review = case_dir.join("review/index.html").is_file();
    let checkpoint = crate::util::read_to_string(&scan_progress_path(case_dir))
        .ok()
        .map(|text| checkpoint_summary_json(&text, video_count, approved_roots));
    let phase = if has_review {
        "review-ready"
    } else if has_index {
        "review-partial"
    } else {
        "idle"
    };
    serde_json::json!({
        "checkpoint": checkpoint,
        "has_index": has_index,
        "has_review": has_review,
        "video_count": video_count,
        "phase": phase,
    })
}

pub(crate) fn checkpoint_summary_json(
    text: &str,
    video_count: Option<u64>,
    approved_roots: &[PathBuf],
) -> serde_json::Value {
    let mut kind = serde_json::Value::Null;
    let mut run_id = serde_json::Value::Null;
    let mut done = 0u64;
    let mut unreadable = 0u64;
    let mut matches = 0u64;
    let mut checked = 0usize;
    let mut skipped = 0u64;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            unreadable += 1;
            continue;
        };
        if let Some(header) = value.get("checkpoint") {
            kind = header.clone();
            run_id = value.get("run_id").cloned().unwrap_or_default();
            continue;
        }
        if !value.is_object() || value.get("id").is_none() {
            unreadable += 1;
            continue;
        }
        done += 1;
        if checked >= CASE_INFO_STAT_CAP {
            continue;
        }
        checked += 1;
        let path = value.get("source_path").and_then(|v| v.as_str());
        let size = value.get("size_bytes").and_then(|v| v.as_u64());
        let mtime = value.get("modified_unix").and_then(|v| v.as_u64());
        // S6: the checkpoint may come from another (external) case: a UNC
        // or device source path is never stat'ed (an SMB session would
        // hand out the examiner's NTLM credentials) unless it lies under
        // an approved root — it is counted as skipped, not as a match.
        if let Some(path) = path
            && super::http::reject_remote_path(path, approved_roots).is_err()
        {
            skipped += 1;
            continue;
        }
        if let (Some(path), Some(size)) = (path, size)
            && let Ok(meta) = std::fs::metadata(path)
        {
            let disk_mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            if meta.is_file() && meta.len() == size && disk_mtime == mtime {
                matches += 1;
            }
        }
    }
    serde_json::json!({
        "kind": kind,
        "run_id": run_id,
        "done": done,
        "total": video_count,
        "fingerprint_matches": matches,
        "fingerprint_checked": checked,
        "fingerprint_skipped_remote": skipped,
        "unreadable_lines": unreadable,
    })
}

/// Bounds for the preflight walk: it must answer in about a second even on
/// a multi-terabyte source.
const PREFLIGHT_MAX_ENTRIES: usize = 20_000;
const PREFLIGHT_MAX_MILLIS: u64 = 1_500;
/// Below this much free space on the case volume the preflight warns.
const PREFLIGHT_MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;
const PREFLIGHT_VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "wmv", "asf", "mpg", "mpeg", "mts", "m2ts", "ts", "3gp",
    "webm", "flv", "dav", "dav_", "nov", "ave", "g64", "g64x", "glv", "blk", "264", "265", "h264",
    "h265", "hevc",
];

/// `POST /api/preflight {source_path, case_dir}` (UX-11): a bounded look at
/// the source before an analysis starts. Read-only: nothing is created.
pub(crate) fn api_preflight(request: &Request) -> String {
    let source = body_value(&request.body, "source_path")
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    if source.is_empty() {
        return api_err("증거 소스 경로를 입력하십시오.");
    }
    let case_text = body_value(&request.body, "case_dir")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    // Item 8 policy: preflight is a nonce-gated, Origin-checked POST the
    // examiner fills in by hand (same as /api/start), so an explicitly
    // typed UNC share is honoured; device-namespace paths stay refused.
    // The SMB/NTLM leak vector — cross-origin GETs — is covered by the
    // strict check on every GET route.
    for text in std::iter::once(source.as_str()).chain(case_text.as_deref()) {
        if let Err(reason) = super::http::reject_device_path(text) {
            return api_err(reason);
        }
    }
    let (case_dir, case_dir_warning) = match case_text {
        Some(text) => (PathBuf::from(text), None),
        None => default_case_dir_checked(),
    };
    let mut body = preflight_json(
        Path::new(&source),
        &case_dir,
        PREFLIGHT_MAX_ENTRIES,
        Duration::from_millis(PREFLIGHT_MAX_MILLIS),
    );
    // P1: a temp-dir default case folder is announced before the start.
    if let Some(warning) = case_dir_warning
        && let Some(list) = body.get_mut("warnings").and_then(|w| w.as_array_mut())
    {
        list.push(serde_json::Value::String(warning));
    }
    body.to_string()
}

/// `path` resolved through its nearest existing ancestor, so a case folder
/// that does not exist yet still compares against the canonical source.
/// R21-S5: the missing tail is joined as TEXT (one separator per name) —
/// `PathBuf::push` replaced the canonical buffer for a typed name that
/// parses as a prefix or root (`…\missing\D:` became `D:`).
pub(crate) fn canonical_via_ancestor(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        if let Ok(canonical) = cursor.canonicalize() {
            let mut joined = canonical.into_os_string();
            for part in missing.iter().rev() {
                let ends_with_separator = joined
                    .as_encoded_bytes()
                    .last()
                    .is_some_and(|&byte| std::path::is_separator(char::from(byte)));
                if !ends_with_separator {
                    joined.push(std::path::MAIN_SEPARATOR_STR);
                }
                joined.push(part);
            }
            return PathBuf::from(joined);
        }
        match (
            cursor.file_name().map(|n| n.to_os_string()),
            cursor.parent(),
        ) {
            (Some(name), Some(parent)) => {
                missing.push(name);
                cursor = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Pure body of [`api_preflight`] with explicit bounds (tests).
pub(crate) fn preflight_json(
    source: &Path,
    case_dir: &Path,
    max_entries: usize,
    max_time: Duration,
) -> serde_json::Value {
    let mut warnings: Vec<String> = Vec::new();
    let Ok(source_meta) = std::fs::metadata(source) else {
        return serde_json::json!({
            "ok": false,
            "error": format!("소스 경로가 존재하지 않습니다: {}", source.display()),
        });
    };
    let source_canonical = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    let case_canonical = canonical_via_ancestor(case_dir);
    let case_inside_source = source_meta.is_dir() && case_canonical.starts_with(&source_canonical);
    if case_inside_source {
        warnings.push(if case_canonical == source_canonical {
            "케이스 폴더가 증거 소스 폴더 자체입니다 — 다른 위치를 지정하십시오.".to_string()
        } else {
            "케이스 폴더가 증거 소스 안에 있습니다 — 분석 결과가 증거 매체에 기록됩니다. 증거 밖의 위치를 지정하십시오.".to_string()
        });
    }
    if source_meta.is_dir()
        && source_canonical != case_canonical
        && source_canonical.starts_with(&case_canonical)
    {
        warnings.push("증거 소스가 케이스 폴더 안에 있습니다.".to_string());
    }
    if source_meta.is_dir() && source_canonical.parent().is_none() {
        warnings.push(format!(
            "소스가 볼륨 루트입니다 ({}) — 볼륨 전체가 검토 범위로 열립니다. 증거 폴더만 지정하는 것을 권장합니다.",
            source_canonical.display()
        ));
    }
    let started = std::time::Instant::now();
    let sample = if source_meta.is_file() {
        sample_image_family(source, source_meta.len())
    } else {
        sample_source_tree(
            &source_canonical,
            case_inside_source.then_some(case_canonical.as_path()),
            max_entries,
            started + max_time,
        )
    };
    // A truncated walk extrapolates from the directories it fully saw over
    // the ones still queued — an estimate, flagged as such.
    let scale = if sample.complete || sample.visited_dirs == 0 {
        1.0
    } else {
        (sample.visited_dirs + sample.pending_dirs) as f64 / sample.visited_dirs as f64
    };
    let estimate = |value: u64| (value as f64 * scale) as u64;
    let est_total_bytes = estimate(sample.bytes);
    if !sample.complete {
        warnings.push(format!(
            "소스가 커서 일부({}개 폴더)만 표본 조사했습니다 — 파일 수·용량은 추정치입니다.",
            sample.visited_dirs
        ));
    }
    if sample.unreadable_dirs > 0 {
        warnings.push(format!(
            "읽을 수 없는 폴더 {}개가 있습니다.",
            sample.unreadable_dirs
        ));
    }
    if source_meta.is_dir() && sample.complete && sample.video_files == 0 {
        warnings.push(
            "영상 확장자 파일을 찾지 못했습니다 (확장자 없는 영상은 분석 때 시그니처로 찾습니다)."
                .to_string(),
        );
    }
    let free_bytes = match crate::diskspace::available_bytes(case_dir) {
        Ok(free) => Some(free),
        Err(err) => {
            warnings.push(format!(
                "케이스 위치의 여유 공간을 확인하지 못했습니다: {err}"
            ));
            None
        }
    };
    if let Some(free) = free_bytes {
        if free < PREFLIGHT_MIN_FREE_BYTES {
            warnings.push(format!(
                "케이스 위치의 여유 공간이 부족합니다 ({}).",
                crate::diskspace::human_bytes(free)
            ));
        }
        // A full E01 export writes at least the (compressed) image size.
        if source_meta.is_file() && free < est_total_bytes {
            warnings.push(format!(
                "E01 전체 export에는 최소 {} 이상이 필요하지만 여유 공간은 {}입니다 — '빠른 검토'를 고려하십시오.",
                crate::diskspace::human_bytes(est_total_bytes),
                crate::diskspace::human_bytes(free)
            ));
        }
    }
    serde_json::json!({
        "ok": true,
        "source_kind": if source_meta.is_file() { "file" } else { "folder" },
        "sampled_bytes": sample.bytes,
        "sampled_files": sample.files,
        "sampled_dirs": sample.visited_dirs,
        "complete": sample.complete,
        "est_files": estimate(sample.files),
        "est_video_files": estimate(sample.video_files),
        "est_total_bytes": est_total_bytes,
        "free_bytes": free_bytes,
        "case_inside_source": case_inside_source,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "warnings": warnings,
    })
}

#[derive(Default)]
struct PreflightSample {
    files: u64,
    video_files: u64,
    bytes: u64,
    visited_dirs: u64,
    pending_dirs: u64,
    unreadable_dirs: u64,
    complete: bool,
}

/// An image file (E01 family): the segment set next to it, same stem.
fn sample_image_family(source: &Path, own_len: u64) -> PreflightSample {
    let mut sample = PreflightSample {
        complete: true,
        ..PreflightSample::default()
    };
    let stem = source.file_stem().map(|s| s.to_os_string());
    let siblings = source
        .parent()
        .and_then(|parent| std::fs::read_dir(parent).ok());
    for entry in siblings.into_iter().flatten().flatten() {
        if entry.path().file_stem().map(|s| s.to_os_string()) == stem
            && let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            sample.files += 1;
            sample.bytes += meta.len();
        }
    }
    if sample.files == 0 {
        sample.files = 1;
        sample.bytes = own_len;
    }
    sample
}

/// Breadth-first walk bounded by an entry count and a deadline. The case
/// folder (when inside the source) is skipped, as scan-folder does.
fn sample_source_tree(
    root: &Path,
    skip: Option<&Path>,
    max_entries: usize,
    deadline: std::time::Instant,
) -> PreflightSample {
    let mut sample = PreflightSample {
        complete: true,
        ..PreflightSample::default()
    };
    let mut pending = std::collections::VecDeque::from([root.to_path_buf()]);
    let mut entries = 0usize;
    'walk: while let Some(dir) = pending.pop_front() {
        if skip.is_some_and(|skip| dir.starts_with(skip)) {
            continue;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            sample.unreadable_dirs += 1;
            continue;
        };
        for entry in read.flatten() {
            entries += 1;
            if entries > max_entries || std::time::Instant::now() > deadline {
                // This directory was only partly read: it counts as pending.
                sample.complete = false;
                pending.push_front(dir);
                break 'walk;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                pending.push_back(path);
            } else if kind.is_file() {
                sample.files += 1;
                let video = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| {
                        PREFLIGHT_VIDEO_EXTENSIONS
                            .iter()
                            .any(|known| ext.eq_ignore_ascii_case(known))
                    });
                if video {
                    sample.video_files += 1;
                }
                sample.bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        sample.visited_dirs += 1;
    }
    sample.pending_dirs = pending.len() as u64;
    sample
}

#[cfg(test)]
mod tests_r8_audit_api {
    use super::*;

    fn audit_case(
        tag: &str,
        bodies: &[(&str, Option<&crate::audit_key::AuditKey>)],
    ) -> SharedState {
        let case = std::env::temp_dir().join(format!("ft-r8-api-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&case);
        let lane = case.join("evidence/logs/scan-log.jsonl");
        std::fs::create_dir_all(lane.parent().unwrap()).unwrap();
        for (body, key) in bodies {
            crate::audit::append_chained_jsonl_keyed(&lane, body, *key).unwrap();
        }
        let state: SharedState = Arc::new(Mutex::new(JobState::new()));
        state_lock(&state).case_dir = Some(case);
        state
    }

    /// R8 P0/P1/P2 for the workstation audit API: a lane stripped of its
    /// HMACs read "integrity-structural-only" (a pass) on a PC holding the
    /// key in R7; now it is "integrity-not-verified" there. A lane
    /// continued on another PC names that PC's key and is not "failed".
    #[test]
    fn r8_audit_api_trust_comes_from_keys_held() {
        let mine = crate::audit_key::AuditKey::new("pc-a", &[0xd1; 32]);
        let other = crate::audit_key::AuditKey::new("pc-b", &[0xd2; 32]);
        let stripped = audit_case("strip", &[(r#"{"n":1}"#, None)]);
        let multi = audit_case(
            "multi",
            &[(r#"{"n":1}"#, Some(&mine)), (r#"{"n":2}"#, Some(&other))],
        );
        let parse = |state: &SharedState| -> serde_json::Value {
            serde_json::from_str(&api_verify_audit(state)).unwrap()
        };
        crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
            let body = parse(&stripped);
            assert_eq!(body["integrity"], "integrity-not-verified", "{body}");
            assert_eq!(body["logs"][0]["keys_held"], true, "{body}");
            let body = parse(&multi);
            assert_eq!(body["ok"], true, "not failed: {body}");
            assert_eq!(body["integrity"], "integrity-not-verified", "{body}");
            // R10-I6: pc-b's line follows a line this PC authenticated —
            // "재작성 배제 불가", counted as unauthenticated, not other-PC.
            assert_eq!(body["logs"][0]["unauthenticated_lines"], 1, "{body}");
            assert!(
                body["logs"][0]["trust"]
                    .as_str()
                    .unwrap()
                    .contains("재작성 배제 불가"),
                "{body}"
            );
        });
        crate::audit_key::with_test_keys(&[], || {
            let body = parse(&stripped);
            assert_eq!(body["integrity"], "integrity-structural-only", "{body}");
            assert!(
                body["logs"][0]["trust"]
                    .as_str()
                    .unwrap()
                    .contains("자기 일관성만 확인(인증 아님)"),
                "{body}"
            );
        });
        for state in [stripped, multi] {
            let case = state_lock(&state).case_dir.clone();
            if let Some(case) = case {
                let _ = std::fs::remove_dir_all(case);
            }
        }
    }
}
