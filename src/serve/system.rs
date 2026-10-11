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
pub(crate) fn api_env(request: &Request) -> String {
    static ENV_CACHE: Mutex<Option<String>> = Mutex::new(None);
    let refresh = query_value(&request.query, "refresh").as_deref() == Some("1");
    let mut guard = ENV_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if refresh || guard.is_none() {
        *guard = Some(probe_env());
    }
    guard
        .clone()
        .unwrap_or_else(|| "{\"ok\":false}".to_string())
}

pub(crate) fn probe_env() -> String {
    let tools: Vec<(&str, bool)> = PROBED_TOOLS
        .iter()
        .map(|(name, arg)| (*name, tool_available(name, arg)))
        .collect();
    let has = |name: &str| tools.iter().any(|(n, ok)| *n == name && *ok);
    // Workflows require every binary in their set.
    let media = has("ffmpeg") && has("ffprobe");
    let ewf = has("ewfinfo") && has("ewfverify") && has("ewfexport");
    let tsk = has("mmls") && has("fls") && has("icat");
    // deepfake-lens resolves via FRAMETRACE_DEEPFAKE_LENS first, then PATH /
    // tools/bin — mirror that order so the badge matches what the pipeline
    // would actually run.
    let deepfake = std::env::var("FRAMETRACE_DEEPFAKE_LENS")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(&v).is_file() || tool_available(&v, ""))
        .unwrap_or(false)
        || has("deepfake-lens");
    let tools_map: serde_json::Map<String, serde_json::Value> = tools
        .iter()
        .map(|(name, ok)| (name.to_string(), serde_json::Value::Bool(*ok)))
        .collect();
    serde_json::json!({
        "ok": true,
        "ffmpeg": has("ffmpeg"),
        "ffprobe": has("ffprobe"),
        "ewf": ewf,
        "deepfake": deepfake,
        "tools": tools_map,
        "workflows": {"media": media, "e01": ewf, "filesystem": tsk},
        "hints": {
            "media": "FFmpeg 설치 후 ffmpeg/ffprobe가 PATH에 있어야 합니다. portable 배포본은 tools/bin에 복사하면 자동 인식됩니다.",
            "e01": "libewf(ewfinfo/ewfverify/ewfexport) 설치 후 PATH 등록 또는 tools/bin에 복사하십시오.",
            "filesystem": "Sleuth Kit(mmls/fls/icat) 설치 후 PATH 등록 또는 tools/bin에 복사하십시오.",
        }
    })
    .to_string()
}

pub(crate) fn tool_available(name: &str, version_arg: &str) -> bool {
    // resolve_tool_binary covers PATH plus the portable tools/bin layout, so
    // a bare binary dropped next to frametrace-app.exe is detected.
    let Ok(resolved) = crate::tool_policy::resolve_tool_binary(name, &[name]) else {
        return false;
    };
    // An empty version_arg means "resolution is enough" — TSK tools have no
    // -V flag and print usage to stderr, so executing them tells us nothing.
    if version_arg.is_empty() {
        return true;
    }
    let mut command = Command::new(&resolved);
    command.arg(version_arg);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
    let path = PathBuf::from(raw);
    let display = path.display().to_string();
    let Ok(meta) = std::fs::metadata(&path) else {
        return serde_json::json!({"ok": true, "exists": false, "path": display}).to_string();
    };
    if meta.is_file() {
        return serde_json::json!({
            "ok": true, "exists": true, "is_file": true, "is_dir": false,
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
        "ok": true, "exists": true, "is_dir": true,
        "is_case": crate::case_db::case_db_path(&path).is_file(),
        "path": display,
        "entries": entries,
        "truncated": truncated,
    });
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
    let guard = state_lock(state);
    let case_dir = guard.case_dir.clone();
    let byte_progress = guard.byte_progress.clone();
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
    let progress = progress_json(case_dir.as_deref(), byte_progress.as_ref());
    // UX-02: which phase-2 step is enriching the partial index. `done` /
    // `total` are the running job's unit progress when the step reports
    // one (files hashed), else the step position within the plan.
    let enrichment = guard.enrichment.map(|step| {
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
        "progress": progress,
        "enrichment": enrichment,
        "index_generation": index_generation,
    })
    .to_string()
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
    let job = crate::case_db::latest_running_job(case_dir).ok().flatten();
    let Some(job) = job else {
        // Fallback: external-tool steps (e.g. ewfexport) don't touch the
        // jobs table — report output bytes seen so far as an indeterminate
        // progress signal.
        return byte_progress_json(byte_progress);
    };
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

/// `POST /api/verify-audit`: verify every chained audit log under
/// `case_dir/evidence/logs/*.jsonl` in-process and report per-log and
/// overall integrity (keyed vs structural-only vs failed).
pub(crate) fn api_cancel(state: &SharedState) -> String {
    let mut guard = state_lock(state);
    if !guard.busy {
        return "{\"ok\":false,\"error\":\"진행 중인 작업이 없습니다.\"}".to_string();
    }
    guard.cancel_requested = true;
    guard
        .logs
        .push("중단 요청을 받았습니다. 현재 단계를 멈추는 중입니다.".to_string());
    "{\"ok\":true}".to_string()
}

/// POST /api/shutdown — the only clean way to stop the windowed
/// `frametrace-app.exe` (no console to close). Refuses while a job runs so
/// a mid-pipeline exit cannot truncate audit/carve output; the UI offers a
/// cancel-then-shutdown flow for that case.
pub(crate) fn api_shutdown(state: &SharedState) -> String {
    let mut guard = state_lock(state);
    if guard.busy {
        return "{\"ok\":false,\"running\":true,\"error\":\"작업이 진행 중입니다 — 중단 후 종료하십시오.\"}"
            .to_string();
    }
    guard.shutdown_requested = true;
    "{\"ok\":true}".to_string()
}

pub(crate) fn api_open_case(request: &Request, state: &SharedState) -> String {
    let dir_text = body_value(&request.body, "case_dir").unwrap_or_default();
    if dir_text.trim().is_empty() {
        return "{\"ok\":false,\"error\":\"케이스 폴더 경로를 입력하십시오.\"}".to_string();
    }
    let case_dir = PathBuf::from(dir_text.trim());
    if !case_dir.join("case.json").is_file() {
        return "{\"ok\":false,\"error\":\"해당 폴더에 case.json이 없습니다 — 기존 케이스 폴더가 아닙니다.\"}"
            .to_string();
    }
    let has_review = case_dir.join("review/index.html").is_file();
    // A case whose enrichment pass was cancelled (or failed) before
    // make-review still has a reviewable index (UX-02).
    let has_index = case_dir.join("db/video_index.json").is_file();
    let mut guard = state_lock(state);
    if guard.busy {
        return "{\"ok\":false,\"error\":\"분석이 진행 중입니다.\"}".to_string();
    }
    guard.case_dir = Some(case_dir.clone());
    // Reopening the same case must keep extra media roots the earlier
    // session recorded (e.g. a carving workspace linked into the case) —
    // resetting to just the case dir would 403 artifacts that resolve
    // through that root.
    let prior_roots = restore_session()
        .and_then(|(dir, roots)| (dir == case_dir).then_some(roots))
        .unwrap_or_else(|| vec![case_dir.clone()]);
    let extra_root = prior_roots.iter().find(|root| **root != case_dir).cloned();
    guard.media_roots = prior_roots;
    // An explicit open is a deliberate choice — the restored-session
    // prompt must not reappear over it.
    guard.restored = false;
    guard.warnings = Vec::new();
    guard.enrichment = None;
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
        guard.logs.push(format!(
            "케이스를 열었습니다 (검토 결과 없음): {}",
            case_dir.display()
        ));
    }
    drop(guard);
    save_session(&case_dir, extra_root.as_deref());
    format!(
        "{{\"ok\":true,\"has_review\":{}}}",
        if has_review { "true" } else { "false" }
    )
}

pub(crate) fn api_verify_audit(state: &SharedState) -> String {
    let case_dir = {
        let guard = state_lock(state);
        guard.case_dir.clone()
    };
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"먼저 INPUT 분석을 실행하십시오.\"}".to_string();
    };
    let logs_dir = case_dir.join("evidence/logs");
    let mut log_paths: Vec<PathBuf> = match std::fs::read_dir(&logs_dir) {
        Ok(read_dir) => read_dir
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("jsonl"))
            .collect(),
        Err(err) => {
            return serde_json::json!({
                "ok": false,
                "error": format!("감사 로그 디렉터리를 읽지 못했습니다: {err}"),
            })
            .to_string();
        }
    };
    log_paths.sort();
    if log_paths.is_empty() {
        return "{\"ok\":false,\"error\":\"검증할 감사 로그가 없습니다.\"}".to_string();
    }
    let mut logs_json = Vec::new();
    let mut failed = 0usize;
    let mut any_structural_only = false;
    let mut total_entries = 0usize;
    for path in &log_paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        match audit::verify_chained_jsonl(path) {
            Ok(result) => {
                total_entries += result.entries;
                if result.integrity == audit::AuditIntegrity::StructuralOnly {
                    any_structural_only = true;
                }
                logs_json.push(serde_json::json!({
                    "name": name,
                    "entries": result.entries,
                    "integrity": result.integrity.label(),
                    "keyed_entries": result.keyed_entries,
                    "unauthenticated_keyed_entries": result.unauthenticated_keyed_entries,
                    "warnings": result.warnings,
                }));
            }
            Err(err) => {
                failed += 1;
                logs_json.push(serde_json::json!({
                    "name": name,
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
