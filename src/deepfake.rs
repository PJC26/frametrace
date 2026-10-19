//! deepfake-lens sidecar lane.
//!
//! deepfake-lens (Python, local-first) screens a file for synthetic-media
//! signals — images, audio, video, text/documents, archives. It is invoked
//! as a subprocess exactly like ffprobe: `deepfake-lens forensic <file>
//! --format json` prints a stable JSON report on stdout.
//!
//! IMPORTANT CONTRACT: deepfake-lens scores are review-priority signals,
//! not authenticity verdicts. Any UI/record surface that shows them must
//! keep that framing — "high" means "review first", never "confirmed
//! synthetic".
//!
//! Resolution order for the tool binary:
//!   1. `FRAMETRACE_DEEPFAKE_LENS` env var (full path or bare name)
//!   2. the private venv of an on-demand install
//!      (`<exe dir>/tools/python/deepfake-lens`, src/tool_install)
//!   3. `deepfake-lens` on PATH
//!
//! Install: deepfake-lens is not published on PyPI, so install it from
//! its checkout or wheel (`pip install -e <checkout>` / `pip install
//! <wheel>`), or set `FRAMETRACE_DEEPFAKE_LENS_HOME=<checkout root>` for
//! the `python -m deepfake_lens.cli` fallback. Optional model weights live under the
//! package's models/ dir; missing weights degrade to heuristic scoring.

use crate::tool_policy::resolve_tool_binary;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Neural-model analysis can run for minutes on CPU-only machines.
pub const DEEPFAKE_TIMEOUT_SECS: u64 = 900;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeepfakeSummary {
    pub ok: bool,
    pub score: Option<i64>,
    pub band: Option<String>,
    pub band_label: Option<String>,
    pub verdict: Option<String>,
    pub signal_titles: Vec<String>,
    pub limitation_count: usize,
    pub has_c2pa: bool,
    pub has_synthid: bool,
    pub has_watermark: bool,
    pub raw_json: Option<String>,
    pub error: Option<String>,
}

impl DeepfakeSummary {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Deserialize)]
struct ForensicSignal {
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ForensicReport {
    score: Option<i64>,
    band: Option<String>,
    band_label: Option<String>,
    verdict: Option<String>,
    signals: Option<Vec<ForensicSignal>>,
    limitations: Option<Vec<serde_json::Value>>,
    has_c2pa: Option<bool>,
    has_synthid: Option<bool>,
    has_watermark: Option<bool>,
}

/// Screen `path` through the deepfake-lens forensic report.
pub fn screen(path: &Path) -> DeepfakeSummary {
    screen_with_binary(&configured_lens_binary(), path)
}

/// The deepfake-lens binary the lane runs: `FRAMETRACE_DEEPFAKE_LENS`,
/// else the private venv `frametrace tools install deepfake-lens` built
/// (`<exe dir>/tools/python/deepfake-lens`), else the bare name (tools/bin,
/// then PATH — a manifest tool, see `tool_policy`).
pub fn configured_lens_binary() -> String {
    if let Ok(value) = std::env::var("FRAMETRACE_DEEPFAKE_LENS")
        && !value.trim().is_empty()
    {
        return value;
    }
    if let Some(exe) = crate::tool_install::private_venv_executable("deepfake-lens") {
        return exe.to_string_lossy().into_owned();
    }
    "deepfake-lens".to_string()
}

/// Absolute path of the deepfake-lens console script the lane would run,
/// or `None` (resolution only — nothing is spawned).
pub fn resolved_lens_binary() -> Option<String> {
    resolve_tool_binary(
        configured_lens_binary().trim(),
        &["deepfake-lens", "deepfake-lens.exe"],
    )
    .ok()
}

/// Whether screening can run: a resolvable console script, or an
/// operator-configured source checkout (`FRAMETRACE_DEEPFAKE_LENS_HOME`)
/// plus an interpreter for `python -m deepfake_lens.cli`.
pub fn lens_available() -> bool {
    resolved_lens_binary().is_some()
        || (std::env::var("FRAMETRACE_DEEPFAKE_LENS_HOME")
            .is_ok_and(|home| !home.trim().is_empty())
            && resolve_tool_binary("python", &["python", "python3", "py"]).is_ok())
}

pub fn screen_with_binary(binary: &str, path: &Path) -> DeepfakeSummary {
    // Set when the console script was refused and we ran the interpreter
    // entry point instead, so failures name what actually ran.
    let mut python_fallback: Option<String> = None;
    let mut command = match resolve_tool_binary(binary, &["deepfake-lens", "deepfake-lens.exe"]) {
        Ok(binary) => {
            let mut command = Command::new(&binary);
            command
                .arg("forensic")
                .arg(path)
                .arg("--format")
                .arg("json");
            command
        }
        Err(binary_error) => {
            // No console script on PATH — fall back to the interpreter
            // entry point (`python -m deepfake_lens.cli ...`), which works
            // for source checkouts and venv installs alike.
            let python = match resolve_tool_binary("python", &["python", "python3", "py"]) {
                Ok(python) => python,
                Err(python_error) => {
                    return DeepfakeSummary::failed(format!(
                        "{binary_error}; python fallback also unavailable: {python_error}"
                    ));
                }
            };
            python_fallback = Some(binary_error.to_string());
            let mut command = Command::new(&python);
            command
                .arg("-m")
                .arg("deepfake_lens.cli")
                .arg("forensic")
                .arg(path)
                .arg("--format")
                .arg("json");
            // Source checkouts aren't importable by default; point the
            // interpreter at the repo root when the operator tells us
            // where it lives.
            if let Ok(home) = std::env::var("FRAMETRACE_DEEPFAKE_LENS_HOME") {
                let existing = std::env::var("PYTHONPATH").unwrap_or_default();
                let merged = if existing.is_empty() {
                    home
                } else {
                    let sep = if cfg!(windows) { ";" } else { ":" };
                    format!("{home}{sep}{existing}")
                };
                command.env("PYTHONPATH", merged);
            }
            command
        }
    };

    let output = crate::util::run_with_timeout(&mut command, Some(DEEPFAKE_TIMEOUT_SECS));
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return DeepfakeSummary::failed(format!("failed to run deepfake-lens: {error}"));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return DeepfakeSummary::failed(match &python_fallback {
            Some(binary_error) => format!(
                "{binary_error}; python fallback (python -m deepfake_lens.cli) exited {}: {}",
                output.status,
                stderr.trim()
            ),
            None => format!("deepfake-lens exited {}: {}", output.status, stderr.trim()),
        });
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let report: ForensicReport = match serde_json::from_str(&raw) {
        Ok(report) => report,
        Err(error) => {
            return DeepfakeSummary::failed(format!("unparseable deepfake-lens JSON: {error}"));
        }
    };

    DeepfakeSummary {
        ok: true,
        score: report.score,
        band: report.band,
        band_label: report.band_label,
        verdict: report.verdict,
        signal_titles: report
            .signals
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| s.title)
            .collect(),
        limitation_count: report.limitations.map(|v| v.len()).unwrap_or(0),
        has_c2pa: report.has_c2pa.unwrap_or(false),
        has_synthid: report.has_synthid.unwrap_or(false),
        has_watermark: report.has_watermark.unwrap_or(false),
        raw_json: Some(raw),
        error: None,
    }
}

/// Collects every `artifacts/deepfake/<id>.json` report under a case into a
/// `{video_id: report}` JSON object for the evidence-viewer data bundle.
/// Missing/unreadable/invalid reports are skipped — a partial artifact must
/// not blank the generated page.
pub fn collect_reports(case_dir: &Path) -> serde_json::Value {
    let dir = case_dir.join("artifacts/deepfake");
    let mut map = serde_json::Map::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return serde_json::Value::Object(map),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(report) = serde_json::from_str::<serde_json::Value>(&text) {
            map.insert(id.to_string(), report);
        }
    }
    serde_json::Value::Object(map)
}

/// One screenable record inside a case: the viewer-facing record id plus
/// the on-disk file to analyze.
pub struct ScreenTarget {
    pub id: String,
    pub path: std::path::PathBuf,
    pub origin: &'static str,
    /// The id was logged with more than one output path (pre-E4 carve ids
    /// such as `carve_000001` repeat across runs). A report without a
    /// recorded target path cannot be trusted to describe `path`.
    pub id_reused: bool,
}

/// Key the screened file's path is recorded under inside each
/// `artifacts/deepfake/<id>.json` (E4: reports are matched by the
/// (id, path) pair, never by id alone).
pub const TARGET_PATH_KEY: &str = "frametrace_target_path";

/// Whether an existing report may stand for `target` without screening.
/// A report recorded for a different file (same reused id, other carve
/// run) is stale; a legacy report with no recorded path is trusted only
/// while the id is unambiguous.
fn report_matches_target(artifact: &Path, target: &ScreenTarget) -> bool {
    let Ok(text) = fs::read_to_string(artifact) else {
        return false;
    };
    let recorded = serde_json::from_str::<serde_json::Value>(text.trim())
        .ok()
        .and_then(|value| {
            value
                .get(TARGET_PATH_KEY)
                .and_then(|path| path.as_str())
                .map(str::to_string)
        });
    match recorded {
        Some(path) => path == target.path.to_string_lossy(),
        None => !target.id_reused,
    }
}

/// Screening decision for one target (shared by both screen passes).
fn needs_screening(
    artifact: &Path,
    target: &ScreenTarget,
    force: bool,
    retry_failed: bool,
) -> bool {
    !artifact.is_file()
        || force
        || (retry_failed && artifact_failed(artifact))
        || !report_matches_target(artifact, target)
}

/// The report body written for `target`: the tool's JSON (or a failure
/// object) with the screened path recorded under [`TARGET_PATH_KEY`].
fn report_body(screening: &DeepfakeSummary, target: &ScreenTarget) -> String {
    let body = screening.raw_json.clone().unwrap_or_else(|| {
        format!(
            "{{\"ok\":false,\"error\":\"{}\",\"origin\":\"{}\"}}",
            crate::util::json_escape(screening.error.as_deref().unwrap_or("unknown")),
            target.origin
        )
    });
    let path_json = format!(
        "\"{}\"",
        crate::util::json_escape(&target.path.to_string_lossy())
    );
    crate::scan::try_set_json_field(&body, TARGET_PATH_KEY, &path_json).unwrap_or(body)
}

/// Artifact filenames derive from record ids, which can contain
/// characters that are illegal on Windows (`inode:<offset>:<ino>`,
/// `fls:<ino>`). Sanitize once — the viewer applies the same mapping
/// when looking up reports in its data bundle.
pub fn artifact_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if matches!(c, ':' | '\\' | '/') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Enumerates every screenable record in a case — indexed logical files,
/// carved candidates, filesystem-recovered files — so a post-import pass
/// covers E01/carve/recover pipelines that never pass through
/// `scan-folder`.
pub fn collect_targets(case_dir: &Path) -> Vec<ScreenTarget> {
    let mut out = Vec::new();
    // A torn tail line (documented crash-survivability mode) is not
    // valid JSON — skipped rather than aborting the pass.
    let index_rows: Vec<serde_json::Value> = fs::read_to_string(case_dir.join("db/videos.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect();
    collect_jsonl_targets(&index_rows, "id", "source_path", "index", &mut out);
    // R9 P3: lane rows through the one reader — only entries usable here
    // name a target to screen.
    let lane_rows = |rel: &str| -> Vec<serde_json::Value> {
        crate::audit::read_lane_rows(&case_dir.join(rel))
            .map(|lane| lane.usable_values())
            .unwrap_or_default()
    };
    collect_jsonl_targets(
        &lane_rows("artifacts/carved/carve-log.jsonl"),
        "id",
        "output_path",
        "carve",
        &mut out,
    );
    // Filesystem recovery records use the viewer's composite id
    // (`inode:<partition_offset>:<inode>`) so the badge maps correctly.
    {
        for v in lane_rows("evidence/logs/tsk-audit.jsonl") {
            if v.get("event").and_then(|e| e.as_str()) != Some("recover-inode") {
                continue;
            }
            let Some(path) = v.get("output_path").and_then(|x| x.as_str()) else {
                continue;
            };
            let offset = v
                .get("partition_offset")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let inode = v.get("inode").and_then(|x| x.as_str()).unwrap_or(path);
            out.push(ScreenTarget {
                id: format!("inode:{offset}:{inode}"),
                path: std::path::PathBuf::from(path),
                origin: "recover",
                id_reused: false,
            });
        }
    }
    out
}

fn collect_jsonl_targets(
    rows: &[serde_json::Value],
    id_key: &str,
    path_key: &str,
    origin: &'static str,
    out: &mut Vec<ScreenTarget>,
) {
    // One target per id, the LAST logged path winning — the record the
    // viewer shows for a reused legacy id is the latest run's (E4). Every
    // distinct path an id was logged with is remembered so a report
    // screened for an older run's file is not reused for the newer one.
    let mut order: Vec<String> = Vec::new();
    let mut latest: std::collections::HashMap<String, (String, usize)> =
        std::collections::HashMap::new();
    for v in rows {
        let (Some(id), Some(file)) = (
            v.get(id_key).and_then(|x| x.as_str()),
            v.get(path_key).and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        match latest.get_mut(id) {
            Some((current, distinct)) => {
                if current != file {
                    *distinct += 1;
                    *current = file.to_string();
                }
            }
            None => {
                order.push(id.to_string());
                latest.insert(id.to_string(), (file.to_string(), 1));
            }
        }
    }
    for id in order {
        let (file, distinct) = &latest[&id];
        out.push(ScreenTarget {
            path: std::path::PathBuf::from(file),
            origin,
            id_reused: *distinct > 1,
            id,
        });
    }
}

#[derive(Default)]
pub struct ScreenCaseStats {
    pub screened: usize,
    pub skipped_existing: usize,
    pub skipped_missing: usize,
    pub failed: usize,
}

/// Returns true when an existing artifact JSON records a failed
/// screening (`"ok":false` or a non-null `"error"` field), meaning
/// `--retry-failed` should re-screen the record. Unparseable artifacts
/// count as failed — a torn write should not be trusted as a result.
fn artifact_failed(artifact: &Path) -> bool {
    let Ok(text) = fs::read_to_string(artifact) else {
        return true;
    };
    match serde_json::from_str::<serde_json::Value>(text.trim()) {
        Ok(v) => {
            v.get("ok").and_then(|x| x.as_bool()) == Some(false)
                || v.get("error").map(|e| !e.is_null()).unwrap_or(false)
        }
        Err(_) => true,
    }
}

/// Screens every case record that lacks a deepfake artifact (or all of
/// them with `force`), writing `artifacts/deepfake/<id>.json` per record.
/// With `retry_failed`, artifacts that recorded a screening failure are
/// re-screened while successful results are left untouched.
/// Individual failures never abort the pass — a case can contain a file
/// deepfake-lens cannot parse.
pub fn screen_case(
    case_dir: &Path,
    force: bool,
    retry_failed: bool,
    progress: &dyn Fn(usize, usize, &str),
) -> Result<ScreenCaseStats, String> {
    let targets = collect_targets(case_dir);
    let artifact_dir = case_dir.join("artifacts/deepfake");
    fs::create_dir_all(&artifact_dir)
        .map_err(|err| format!("deepfake artifact dir failed: {err}"))?;
    let total = targets.len();
    let mut stats = ScreenCaseStats::default();
    for (idx, target) in targets.into_iter().enumerate() {
        progress(idx, total, &target.id);
        let artifact = artifact_dir.join(format!("{}.json", artifact_name(&target.id)));
        if !needs_screening(&artifact, &target, force, retry_failed) {
            stats.skipped_existing += 1;
            continue;
        }
        if !target.path.is_file() {
            stats.skipped_missing += 1;
            continue;
        }
        let screening = screen(&target.path);
        let screen_error = screening.error.clone();
        let body = report_body(&screening, &target);
        match crate::util::write_text_atomic(&artifact, &body) {
            Ok(()) => {
                if screen_error.is_some() {
                    stats.failed += 1;
                } else {
                    stats.screened += 1;
                }
            }
            Err(_) => stats.failed += 1,
        }
    }
    progress(total, total, "");
    Ok(stats)
}

/// Parallel variant of `screen_case`: every screening spawns a
/// deepfake-lens subprocess, so a small worker pool overlaps interpreter
/// startup and inference instead of serializing seconds-to-minutes per
/// file. Policy skips, artifact writes, and progress callbacks still run
/// on the calling thread — `progress(done, queued, &id)` reports queue
/// progress where `queued` counts only records actually screened.
pub fn screen_case_parallel(
    case_dir: &Path,
    force: bool,
    retry_failed: bool,
    progress: &dyn Fn(usize, usize, &str),
    workers: usize,
) -> Result<ScreenCaseStats, String> {
    let targets = collect_targets(case_dir);
    let artifact_dir = case_dir.join("artifacts/deepfake");
    fs::create_dir_all(&artifact_dir)
        .map_err(|err| format!("deepfake artifact dir failed: {err}"))?;
    let mut stats = ScreenCaseStats::default();

    // Policy skips are resolved up front so the worker pool only carries
    // records that will actually invoke deepfake-lens.
    let mut queue = Vec::new();
    for target in targets {
        let artifact = artifact_dir.join(format!("{}.json", artifact_name(&target.id)));
        if !needs_screening(&artifact, &target, force, retry_failed) {
            stats.skipped_existing += 1;
            continue;
        }
        if !target.path.is_file() {
            stats.skipped_missing += 1;
            continue;
        }
        queue.push(target);
    }

    let queued = queue.len();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel::<(usize, DeepfakeSummary)>();
    let worker_count = workers.clamp(1, 8).min(queued.max(1));
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let tx = tx.clone();
            let queue = &queue;
            let next = &next;
            scope.spawn(move || {
                loop {
                    let idx = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(target) = queue.get(idx) else {
                        break;
                    };
                    if tx.send((idx, screen(&target.path))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for (done, (idx, screening)) in rx.into_iter().enumerate() {
            let target = &queue[idx];
            progress(done, queued, &target.id);
            let screen_error = screening.error.clone();
            let body = report_body(&screening, target);
            let artifact = artifact_dir.join(format!("{}.json", artifact_name(&target.id)));
            match crate::util::write_text_atomic(&artifact, &body) {
                Ok(()) => {
                    if screen_error.is_some() {
                        stats.failed += 1;
                    } else {
                        stats.screened += 1;
                    }
                }
                Err(_) => stats.failed += 1,
            }
        }
    });
    progress(queued, queued, "");
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        // Root under the OS temp dir — a relative base would litter the
        // repo root with `deepfake-test*` dirs on every `cargo test` run.
        crate::util::unique_dir(
            &std::env::temp_dir().join(format!("frametrace-deepfake-test-{}", std::process::id())),
        )
    }

    /// A real executable masquerading as `deepfake-lens.exe` so
    /// `resolve_tool_binary`'s allowlist passes while the process behavior
    /// (garbage stdout / nonzero exit) drives the failure path under test.
    #[cfg(windows)]
    fn fake_binary(source: &Path) -> Option<std::path::PathBuf> {
        let dir = temp_dir();
        let shim = dir.join("deepfake-lens.exe");
        fs::copy(source, &shim).ok()?;
        Some(shim)
    }

    #[cfg(windows)]
    fn system_exe(name: &str) -> Option<std::path::PathBuf> {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from)?;
        let path = root.join("System32").join(name);
        path.is_file().then_some(path)
    }

    fn python_module_available() -> bool {
        resolve_tool_binary("python", &["python", "python3", "py"])
            .ok()
            .and_then(|python| {
                Command::new(python)
                    .args(["-c", "import deepfake_lens"])
                    .output()
                    .ok()
            })
            .is_some_and(|output| output.status.success())
    }

    #[test]
    fn refused_binary_name_falls_back_to_python_module() {
        // A non-allowlisted binary name is never executed; the lane falls
        // back to `python -m deepfake_lens.cli`. When that module is
        // importable the screen still completes (missing input is a
        // legitimate report); when it is not, the error must say both
        // resolution stages failed.
        let summary = screen_with_binary("not-an-allowed-tool", Path::new("x.png"));
        if python_module_available() {
            assert!(
                summary.ok,
                "python fallback should rescue the screen: {:?}",
                summary.error
            );
        } else {
            // Either no interpreter resolves at all, or one does (e.g. the
            // Windows CI image ships python) but the module isn't
            // installed; both must report the python fallback stage.
            assert!(!summary.ok);
            let error = summary.error.as_deref().unwrap_or("");
            assert!(
                error.contains("fallback also unavailable") || error.contains("python fallback"),
                "unexpected error: {error}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn non_json_stdout_is_reported_as_unparseable() {
        // tree.com treats the first arg as a folder path, prints a usage
        // error on stdout, yet exits 0 — the success exit + invalid JSON
        // combination must surface as a clean parse error, not "exited".
        let Some(host) = system_exe("tree.com") else {
            return;
        };
        let Some(shim) = fake_binary(&host) else {
            return;
        };
        let summary = screen_with_binary(shim.to_str().unwrap(), Path::new("x.png"));
        assert!(!summary.ok);
        assert!(
            summary
                .error
                .as_deref()
                .unwrap_or("")
                .contains("unparseable")
        );
    }

    #[test]
    fn screen_case_skips_missing_files_and_survives_torn_index() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("db")).unwrap();
        fs::write(
            dir.join("db/videos.jsonl"),
            "{\"id\":\"vid_1\",\"source_path\":\"/nonexistent/a.mp4\"}\n{torn\n",
        )
        .unwrap();
        let stats = screen_case(&dir, false, false, &|_, _, _| {}).unwrap();
        assert_eq!(stats.skipped_missing, 1);
        assert_eq!(stats.screened, 0);
        assert!(!dir.join("artifacts/deepfake/vid_1.json").is_file());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(windows)]
    #[test]
    fn screen_case_writes_error_artifact_on_screen_failure() {
        // Route screen() at a shim that exits 0 with non-JSON output; the
        // pass must record the failure and write an error artifact rather
        // than aborting.
        let Some(host) = system_exe("tree.com") else {
            return;
        };
        let Some(shim) = fake_binary(&host) else {
            return;
        };
        let dir = temp_dir();
        fs::create_dir_all(dir.join("db")).unwrap();
        let target = dir.join("clip.mp4");
        fs::write(&target, b"not a real video").unwrap();
        fs::write(
            dir.join("db/videos.jsonl"),
            format!(
                "{{\"id\":\"vid_1\",\"source_path\":\"{}\"}}\n",
                crate::util::json_escape(&target.to_string_lossy())
            ),
        )
        .unwrap();
        unsafe {
            std::env::set_var("FRAMETRACE_DEEPFAKE_LENS", &shim);
        }
        let stats = screen_case(&dir, false, false, &|_, _, _| {}).unwrap();
        unsafe {
            std::env::remove_var("FRAMETRACE_DEEPFAKE_LENS");
        }
        assert_eq!(stats.failed, 1);
        let artifact = fs::read_to_string(dir.join("artifacts/deepfake/vid_1.json")).unwrap();
        assert!(artifact.contains("\"ok\":false"));
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(windows)]
    #[test]
    fn nonzero_exit_reports_stderr() {
        // Python invoked as `deepfake-lens.exe forensic <x> --format json`
        // treats "forensic" as a script path and exits nonzero.
        let python = resolve_tool_binary("python", &["python", "python3", "py"]).map(PathBuf::from);
        let Ok(python) = python else {
            return;
        };
        let Some(shim) = fake_binary(&python) else {
            return;
        };
        let summary = screen_with_binary(shim.to_str().unwrap(), Path::new("x.png"));
        assert!(!summary.ok);
        assert!(summary.error.as_deref().unwrap_or("").contains("exited"));
    }

    #[test]
    fn artifact_failed_detects_error_and_torn_artifacts() {
        let dir = temp_dir();
        let ok = dir.join("ok.json");
        fs::write(&ok, "{\"ok\":true,\"score\":12}").unwrap();
        assert!(!artifact_failed(&ok));
        let err = dir.join("err.json");
        fs::write(&err, "{\"ok\":false,\"error\":\"unparseable output\"}").unwrap();
        assert!(artifact_failed(&err));
        let error_field = dir.join("error_field.json");
        fs::write(&error_field, "{\"score\":0,\"error\":\"crashed\"}").unwrap();
        assert!(artifact_failed(&error_field));
        let torn = dir.join("torn.json");
        fs::write(&torn, "{\"ok\":tru").unwrap();
        assert!(artifact_failed(&torn));
        assert!(artifact_failed(&dir.join("missing.json")));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn artifact_name_sanitizes_windows_illegal_chars() {
        assert_eq!(artifact_name("inode:2048:1304"), "inode_2048_1304");
        assert_eq!(artifact_name("fls:1304"), "fls_1304");
        assert_eq!(artifact_name("a\\b/c"), "a_b_c");
        assert_eq!(artifact_name("vid_000001"), "vid_000001");
    }

    #[test]
    fn collect_reports_skips_broken_artifacts() {
        let dir = temp_dir();
        let artifacts = dir.join("artifacts/deepfake");
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(artifacts.join("ok_1.json"), "{\"score\":50}").unwrap();
        fs::write(artifacts.join("bad_1.json"), "not json {").unwrap();
        fs::write(artifacts.join("note.txt"), "ignored").unwrap();
        let map = collect_reports(&dir);
        assert!(map.get("ok_1").is_some());
        assert!(map.get("bad_1").is_none());
        assert_eq!(map.as_object().unwrap().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    /// E4: a pre-E4 carve id reused by two runs yields ONE target (the
    /// latest run's file, as the viewer shows it), and a report screened
    /// for the other run's file — or a legacy report with no recorded
    /// path — is not accepted for it. The old pass skipped the second
    /// run's file because `carve_000001.json` already existed.
    #[test]
    fn reused_carve_id_report_is_matched_by_id_and_path() {
        let dir = temp_dir();
        let carved = dir.join("artifacts/carved");
        fs::create_dir_all(&carved).unwrap();
        crate::audit::test_write_lane(
            &carved.join("carve-log.jsonl"),
            "{\"id\":\"carve_000001\",\"output_path\":\"/run1/a.mp4\"}\n{\"id\":\"carve_000001\",\"output_path\":\"/run2/a.mp4\"}\n{\"id\":\"carve_9-abcd_000001\",\"output_path\":\"/run3/b.mp4\"}\n",
        );
        let targets = collect_targets(&dir);
        let carve: Vec<(&str, &Path, bool)> = targets
            .iter()
            .map(|t| (t.id.as_str(), t.path.as_path(), t.id_reused))
            .collect();
        assert_eq!(
            carve,
            vec![
                ("carve_000001", Path::new("/run2/a.mp4"), true),
                ("carve_9-abcd_000001", Path::new("/run3/b.mp4"), false),
            ]
        );
        let reports = dir.join("artifacts/deepfake");
        fs::create_dir_all(&reports).unwrap();
        let legacy = reports.join("carve_000001.json");
        fs::write(&legacy, "{\"ok\":true,\"score\":10}").unwrap();
        assert!(needs_screening(&legacy, &targets[0], false, false));
        let summary = DeepfakeSummary {
            ok: true,
            raw_json: Some("{\"ok\":true,\"score\":10}".to_string()),
            ..DeepfakeSummary::default()
        };
        fs::write(&legacy, report_body(&summary, &targets[0])).unwrap();
        assert!(!needs_screening(&legacy, &targets[0], false, false));
        let other_run = ScreenTarget {
            id: "carve_000001".to_string(),
            path: "/run1/a.mp4".into(),
            origin: "carve",
            id_reused: true,
        };
        assert!(needs_screening(&legacy, &other_run, false, false));
        // An unambiguous legacy report is still trusted (no rescreen churn).
        let fresh = reports.join("carve_9-abcd_000001.json");
        fs::write(&fresh, "{\"ok\":true}").unwrap();
        assert!(!needs_screening(&fresh, &targets[1], false, false));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn collect_targets_reads_index_carve_and_recover() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join("db")).unwrap();
        fs::write(
            dir.join("db/videos.jsonl"),
            "{\"id\":\"vid_1\",\"source_path\":\"/tmp/a.mp4\"}\n{torn\n",
        )
        .unwrap();
        let carved = dir.join("artifacts/carved");
        fs::create_dir_all(&carved).unwrap();
        crate::audit::test_write_lane(
            &carved.join("carve-log.jsonl"),
            "{\"id\":\"carve_1\",\"output_path\":\"/tmp/b.mp4\"}\n",
        );
        let logs = dir.join("evidence/logs");
        fs::create_dir_all(&logs).unwrap();
        crate::audit::test_write_lane(
            &logs.join("tsk-audit.jsonl"),
            "{\"event\":\"recover-inode\",\"output_path\":\"/tmp/c.mp4\",\"partition_offset\":2048,\"inode\":\"1304\"}\n",
        );
        let targets = collect_targets(&dir);
        let ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["vid_1", "carve_1", "inode:2048:1304"]);
        let _ = fs::remove_dir_all(dir);
    }
}
