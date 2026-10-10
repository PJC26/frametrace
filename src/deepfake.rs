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
//!   2. `deepfake-lens` on PATH (i.e. `pip install deepfake-lens`)
//!
//! Install: `pip install deepfake-lens` (or `pip install -e D:\devin\deepfake-lens`
//! for the local checkout). Optional model weights live under the
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
    let configured =
        std::env::var("FRAMETRACE_DEEPFAKE_LENS").unwrap_or_else(|_| "deepfake-lens".to_string());
    screen_with_binary(&configured, path)
}

pub fn screen_with_binary(binary: &str, path: &Path) -> DeepfakeSummary {
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
        return DeepfakeSummary::failed(format!(
            "deepfake-lens exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
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
    collect_jsonl_targets(
        &case_dir.join("db/videos.jsonl"),
        "id",
        "source_path",
        "index",
        &mut out,
    );
    collect_jsonl_targets(
        &case_dir.join("artifacts/carved/carve-log.jsonl"),
        "id",
        "output_path",
        "carve",
        &mut out,
    );
    // Filesystem recovery records use the viewer's composite id
    // (`inode:<partition_offset>:<inode>`) so the badge maps correctly.
    if let Ok(text) = fs::read_to_string(case_dir.join("evidence/logs/tsk-audit.jsonl")) {
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
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
            });
        }
    }
    out
}

fn collect_jsonl_targets(
    path: &Path,
    id_key: &str,
    path_key: &str,
    origin: &'static str,
    out: &mut Vec<ScreenTarget>,
) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        // A torn tail line (documented crash-survivability mode) is not
        // valid JSON — skip it rather than aborting the pass.
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let (Some(id), Some(file)) = (
            v.get(id_key).and_then(|x| x.as_str()),
            v.get(path_key).and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        out.push(ScreenTarget {
            id: id.to_string(),
            path: std::path::PathBuf::from(file),
            origin,
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
        if artifact.is_file() && !force && !(retry_failed && artifact_failed(&artifact)) {
            stats.skipped_existing += 1;
            continue;
        }
        if !target.path.is_file() {
            stats.skipped_missing += 1;
            continue;
        }
        let screening = screen(&target.path);
        let screen_error = screening.error.clone();
        let body = screening.raw_json.clone().unwrap_or_else(|| {
            format!(
                "{{\"ok\":false,\"error\":\"{}\",\"origin\":\"{}\"}}",
                crate::util::json_escape(screen_error.as_deref().unwrap_or("unknown")),
                target.origin
            )
        });
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
        if artifact.is_file() && !force && !(retry_failed && artifact_failed(&artifact)) {
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
            let body = screening.raw_json.clone().unwrap_or_else(|| {
                format!(
                    "{{\"ok\":false,\"error\":\"{}\",\"origin\":\"{}\"}}",
                    crate::util::json_escape(screen_error.as_deref().unwrap_or("unknown")),
                    target.origin
                )
            });
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
            assert!(!summary.ok);
            assert!(
                summary
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .contains("fallback also unavailable")
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
        fs::write(
            carved.join("carve-log.jsonl"),
            "{\"id\":\"carve_1\",\"output_path\":\"/tmp/b.mp4\"}\n",
        )
        .unwrap();
        let logs = dir.join("evidence/logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join("tsk-audit.jsonl"),
            "{\"event\":\"recover-inode\",\"output_path\":\"/tmp/c.mp4\",\"partition_offset\":2048,\"inode\":\"1304\"}\n",
        )
        .unwrap();
        let targets = collect_targets(&dir);
        let ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["vid_1", "carve_1", "inode:2048:1304"]);
        let _ = fs::remove_dir_all(dir);
    }
}
