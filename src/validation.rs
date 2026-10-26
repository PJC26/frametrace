use crate::audit;
use crate::ffprobe;
use crate::model::ProbeSummary;
use crate::tool_policy::command_version;
use crate::util::{canonicalize_display, json_escape, now_unix};
use crate::video_export::resolve_video_source;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ValidationOptions {
    pub ffprobe_bin: String,
}

impl Default for ValidationOptions {
    fn default() -> Self {
        Self {
            ffprobe_bin: "ffprobe".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub selector: String,
    pub target_path: PathBuf,
    pub target_sha256: String,
    pub validation_status: String,
    pub validation_note: String,
    pub probe: ProbeSummary,
    pub validated_unix: u64,
    pub anomaly_flags: Vec<String>,
}

pub fn validate_artifact(
    case_dir: &Path,
    selector: &str,
    options: &ValidationOptions,
) -> Result<ValidationResult, String> {
    // An unreadable index previously surfaced as "no anomaly flag", so keep
    // that failure-soft behaviour by falling back to an empty map.
    let index = crate::anomaly::index_by_id(case_dir).unwrap_or_default();
    let result = compute_validation(case_dir, selector, options, &index)?;
    append_validation_log(case_dir, &result, options)?;
    Ok(result)
}

/// Compute phase of a validation (resolve target, hash, ffprobe) with no side
/// effects on the case. Batch commands run this in parallel and then append
/// the validation log entries sequentially so the hash chain stays intact.
/// `index` is the case's video index keyed by record id, loaded once by the
/// caller rather than re-read per item.
pub fn compute_validation(
    case_dir: &Path,
    selector: &str,
    options: &ValidationOptions,
    index: &std::collections::HashMap<String, crate::anomaly::IndexedRow>,
) -> Result<ValidationResult, String> {
    let target_path = resolve_artifact_path(case_dir, selector)?;
    let validated_unix = now_unix()?;
    let target_sha256 = audit::digest_file(&target_path)?;
    let probe = ffprobe::probe_with_binary(&options.ffprobe_bin, &target_path);
    let (validation_status, validation_note) = validation_status(&probe);
    let mut anomaly_flags = Vec::new();
    if let Some(finding) = crate::anomaly::hash_mismatch_finding(index, selector, &target_sha256) {
        anomaly_flags.push(finding.kind.to_string());
    }
    Ok(ValidationResult {
        selector: selector.to_string(),
        target_path,
        target_sha256,
        validation_status: validation_status.to_string(),
        validation_note: validation_note.to_string(),
        probe,
        validated_unix,
        anomaly_flags,
    })
}

/// Logs whose entries map an artifact id / selector to an output file.
const ARTIFACT_LOGS: [&str; 5] = [
    "artifacts/carved/carve-log.jsonl",
    "artifacts/clips/export-log.jsonl",
    "artifacts/proxies/proxy-log.jsonl",
    "artifacts/thumbnails/thumbnail-log.jsonl",
    "evidence/logs/tsk-audit.jsonl",
];

/// Resolves an indexed video id, artifact id, inode recovery id, or direct
/// path to an existing file. Shared by validate-artifact and the batch
/// commands so carved/recovered selectors work everywhere.
pub fn resolve_artifact_path(case_dir: &Path, selector: &str) -> Result<PathBuf, String> {
    let direct = PathBuf::from(selector);
    if direct.is_file() {
        return canonicalize_display(&direct)
            .map_err(|err| format!("failed to canonicalize validation target: {err}"));
    }

    if let Ok(path) = resolve_video_source(case_dir, selector) {
        return Ok(path);
    }

    for rel_log in ARTIFACT_LOGS {
        let log_path = case_dir.join(rel_log);
        let existing: Vec<PathBuf> = resolve_all_from_log(&log_path, selector)
            .into_iter()
            .filter(|path| path.is_file())
            .collect();
        match existing.as_slice() {
            [] => continue,
            [path] => {
                return canonicalize_display(path)
                    .map_err(|err| format!("failed to canonicalize validation target: {err}"));
            }
            many => {
                // E4: carve ids before run-unique ids (`carve_000001`) were
                // reused by every carve run. Picking the first logged match
                // validated whichever run's file came first — refuse and
                // make the examiner choose the (id, output path) pair.
                return Err(format!(
                    "ambiguous selector {selector}: {rel_log} maps it to {} different existing files ({}) — an id reused by several carve runs; pass the output path itself to choose one",
                    many.len(),
                    many.iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }

    Err(format!(
        "validation target not found: {selector} (use an indexed video id, artifact id, inode recovery path, or direct file path)"
    ))
}

/// Resolves an artifact by its (id, output path) PAIR (E4): the logged
/// entry must carry both, so a reused legacy id can never select another
/// run's file. Returns the canonical output path when it still exists.
pub fn resolve_artifact_pair(
    case_dir: &Path,
    id: &str,
    output_path: &Path,
) -> Result<PathBuf, String> {
    let wanted = output_path.to_string_lossy();
    for rel_log in ARTIFACT_LOGS {
        // R9 P3: only entries usable here resolve a target to validate.
        let Ok(lane) = audit::read_lane_rows(&case_dir.join(rel_log)) else {
            continue;
        };
        for row in lane.usable() {
            let value = &row.value;
            let field = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
            let id_matches = field("id") == Some(id)
                || field("inode") == Some(id)
                || field("selector") == Some(id);
            if id_matches && field("output_path") == Some(wanted.as_ref()) {
                let path = PathBuf::from(wanted.as_ref());
                if path.is_file() {
                    return canonicalize_display(&path)
                        .map_err(|err| format!("failed to canonicalize validation target: {err}"));
                }
                return Err(format!(
                    "artifact {id} at {wanted} is logged but the file no longer exists"
                ));
            }
        }
    }
    Err(format!(
        "no logged artifact pairs id {id} with output path {wanted}"
    ))
}

/// Every distinct output path a log maps `selector` to, in log order.
fn resolve_all_from_log(log_path: &Path, selector: &str) -> Vec<PathBuf> {
    // R9 P3: through the one lane reader; an entry this PC cannot
    // authenticate (keys held) never names a validation target.
    let Ok(lane) = audit::read_lane_rows(log_path) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for row in lane.usable() {
        let Some(path) = match_line(&row.raw, selector) else {
            continue;
        };
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
fn resolve_from_log(log_path: &Path, selector: &str) -> Option<PathBuf> {
    resolve_all_from_log(log_path, selector).into_iter().next()
}

/// The output path of one log line when its id / inode / selector /
/// output path equals `selector`.
fn match_line(line: &str, selector: &str) -> Option<PathBuf> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    let field = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
    let output_path = field("output_path");
    let matches = field("id") == Some(selector)
        || field("inode") == Some(selector)
        || field("selector") == Some(selector)
        || output_path == Some(selector);
    matches.then(|| output_path.map(PathBuf::from)).flatten()
}

fn validation_status(probe: &ProbeSummary) -> (&'static str, &'static str) {
    if !probe.ok {
        return (
            "validation-failed",
            "ffprobe could not parse the file; keep as candidate until manual/vendor-player validation.",
        );
    }
    if probe.video_codec.is_none() {
        return (
            "validation-failed",
            "ffprobe parsed the container but found no video stream.",
        );
    }
    // The catch-all "bin" demuxer accepts any byte blob and reports it as a
    // bintext "video" stream (real case: a recovered NTFS tracking.log).
    // That is a guess, not container recognition, so it must not count as
    // a confirmed video stream.
    if probe.format_name.as_deref() == Some("bin") {
        return (
            "validation-failed",
            "ffprobe only matched the generic binary demuxer (bin/bintext), which guesses any byte blob; keep as candidate until manual/vendor-player validation.",
        );
    }
    (
        "ffprobe-video-stream-confirmed",
        "ffprobe parsed a video stream; examiner playback review is still required before final reporting.",
    )
}

/// Serializes a completed compute result into a validate-batch checkpoint
/// line (`{"computed":{...}}`). Internal resume state — not a published
/// contract — but still hand-rolled except the probe sub-object, which
/// serde_json emits deterministically in field order.
pub fn checkpoint_target_is_current(
    case_dir: &Path,
    selector: &str,
    target_path: &Path,
    target_sha256: &str,
) -> bool {
    resolve_artifact_path(case_dir, selector).is_ok_and(|path| {
        path == target_path && audit::digest_file(&path).is_ok_and(|digest| digest == target_sha256)
    })
}

pub fn checkpoint_line(index: usize, result: &ValidationResult) -> String {
    let (size, modified) = std::fs::metadata(&result.target_path)
        .map(|metadata| {
            (
                metadata.len(),
                metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs()),
            )
        })
        .unwrap_or((0, None));
    let flags = format!(
        "[{}]",
        result
            .anomaly_flags
            .iter()
            .map(|flag| format!("\"{}\"", json_escape(flag)))
            .collect::<Vec<_>>()
            .join(",")
    );
    let probe_json = serde_json::to_string(&result.probe).unwrap_or_else(|_| "null".to_string());
    format!(
        "{{\"computed\":{{\"index\":{},\"result\":{{\"selector\":\"{}\",\"target_path\":\"{}\",\"target_sha256\":\"{}\",\"validation_status\":\"{}\",\"validation_note\":\"{}\",\"probe\":{},\"validated_unix\":{},\"anomaly_flags\":{},\"target_size_bytes\":{},\"target_modified_unix\":{}}}}}}}",
        index,
        json_escape(&result.selector),
        json_escape(&result.target_path.to_string_lossy()),
        json_escape(&result.target_sha256),
        json_escape(&result.validation_status),
        json_escape(&result.validation_note),
        probe_json,
        result.validated_unix,
        flags,
        size,
        modified
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string())
    )
}

#[derive(serde::Deserialize)]
struct CheckpointComputed {
    index: usize,
    result: CheckpointResult,
}

#[derive(serde::Deserialize)]
struct CheckpointResult {
    selector: String,
    target_path: PathBuf,
    target_sha256: String,
    validation_status: String,
    validation_note: String,
    probe: ProbeSummary,
    validated_unix: u64,
    #[serde(default)]
    anomaly_flags: Vec<String>,
    #[serde(default)]
    target_size_bytes: u64,
    target_modified_unix: Option<u64>,
}

/// Decodes a `{"computed":{...}}` checkpoint line back into its item index
/// and result. Returns `None` when the entry does not parse, or when the
/// target file's size/mtime moved since the result was computed — a
/// changed file must be re-validated, not replayed stale.
pub fn from_checkpoint_line(line: &str) -> Option<(usize, ValidationResult)> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let computed: CheckpointComputed =
        serde_json::from_value(value.get("computed")?.clone()).ok()?;
    let result = computed.result;
    let fresh = std::fs::metadata(&result.target_path)
        .map(|metadata| {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs());
            metadata.len() == result.target_size_bytes && modified == result.target_modified_unix
        })
        .unwrap_or(false);
    if !fresh {
        return None;
    }
    Some((
        computed.index,
        ValidationResult {
            selector: result.selector,
            target_path: result.target_path,
            target_sha256: result.target_sha256,
            validation_status: result.validation_status,
            validation_note: result.validation_note,
            probe: result.probe,
            validated_unix: result.validated_unix,
            anomaly_flags: result.anomaly_flags,
        },
    ))
}

pub fn append_validation_log(
    case_dir: &Path,
    result: &ValidationResult,
    options: &ValidationOptions,
) -> Result<(), String> {
    let flags = if result.anomaly_flags.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            result
                .anomaly_flags
                .iter()
                .map(|flag| format!("\"{}\"", json_escape(flag)))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let line = format!(
        "{{\"schema_version\":1,\"event\":\"validate-artifact\",\"validated_unix\":{},\"selector\":\"{}\",\"target_path\":\"{}\",\"target_sha256\":\"{}\",\"validation_status\":\"{}\",\"validation_note\":\"{}\",\"anomaly_flags\":{},\"label\":{},\"duration_seconds\":{},\"format_name\":{},\"video_codec\":{},\"audio_codec\":{},\"width\":{},\"height\":{},\"ffprobe_ok\":{},\"ffprobe_error\":{},\"ffprobe_version\":\"{}\",\"command\":\"{}\"}}",
        result.validated_unix,
        json_escape(&result.selector),
        json_escape(&result.target_path.to_string_lossy()),
        json_escape(&result.target_sha256),
        json_escape(&result.validation_status),
        json_escape(&result.validation_note),
        flags,
        if result.anomaly_flags.is_empty() {
            "null".to_string()
        } else {
            format!("\"{}\"", crate::anomaly::LABEL)
        },
        optional_f64(result.probe.duration_seconds),
        audit::optional_string(result.probe.format_name.as_deref()),
        audit::optional_string(result.probe.video_codec.as_deref()),
        audit::optional_string(result.probe.audio_codec.as_deref()),
        optional_u32(result.probe.width),
        optional_u32(result.probe.height),
        result.probe.ok,
        audit::optional_string(result.probe.error.as_deref()),
        json_escape(&command_version(
            &options.ffprobe_bin,
            &["ffprobe"],
            "-version"
        )),
        json_escape(&options.ffprobe_bin)
    );
    audit::append_chained_jsonl(&case_dir.join("evidence/logs/validation-log.jsonl"), &line)
}

fn optional_f64(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.3}"))
        .unwrap_or_else(|| "null".to_string())
}

fn optional_u32(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_artifact_pair, resolve_artifact_path, resolve_from_log, validation_status,
    };
    use crate::model::ProbeSummary;
    use std::fs;

    #[test]
    fn classifies_successful_video_probe_as_ffprobe_confirmed() {
        let probe = ProbeSummary {
            ok: true,
            raw_json: Some("{}".to_string()),
            error: None,
            duration_seconds: Some(1.0),
            format_name: Some("mov,mp4".to_string()),
            video_codec: Some("h264".to_string()),
            audio_codec: None,
            width: Some(1920),
            height: Some(1080),
            fps: None,
        };
        assert_eq!(
            validation_status(&probe).0,
            "ffprobe-video-stream-confirmed"
        );
    }

    /// ffprobe's catch-all "bin" demuxer claims any byte blob is a bintext
    /// "video" stream (real case: recovered NTFS tracking.log probed as
    /// bin/bintext with score 50). It must not be reported as a confirmed
    /// video stream.
    #[test]
    fn classifies_generic_bin_demuxer_as_failed_not_confirmed() {
        let probe = ProbeSummary {
            ok: true,
            raw_json: Some("{}".to_string()),
            error: None,
            duration_seconds: Some(1.0),
            format_name: Some("bin".to_string()),
            video_codec: Some("bintext".to_string()),
            audio_codec: None,
            width: None,
            height: None,
            fps: None,
        };
        assert_eq!(validation_status(&probe).0, "validation-failed");
    }

    #[test]
    fn classifies_missing_video_stream_as_failed() {
        let probe = ProbeSummary {
            ok: true,
            raw_json: Some("{}".to_string()),
            error: None,
            duration_seconds: Some(1.0),
            format_name: Some("mp3".to_string()),
            video_codec: None,
            audio_codec: Some("mp3".to_string()),
            width: None,
            height: None,
            fps: None,
        };
        assert_eq!(validation_status(&probe).0, "validation-failed");
    }

    #[test]
    fn resolves_artifact_path_from_jsonl_log() {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-validation-log-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log.jsonl");
        crate::audit::test_write_lane(
            &log,
            r#"{"id":"carve_000001","output_path":"/tmp/out.mp4"}"#,
        );
        assert_eq!(
            resolve_from_log(&log, "carve_000001")
                .unwrap()
                .to_string_lossy(),
            "/tmp/out.mp4"
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// E4: two legacy carve runs both logged `carve_000001`. The old
    /// resolver silently returned the first run's file; now a bare id that
    /// names two existing files is refused, and the (id, output path) pair
    /// selects exactly one.
    #[test]
    fn reused_legacy_id_is_ambiguous_but_the_pair_resolves() {
        let dir = crate::util::unique_dir(
            &std::env::temp_dir()
                .join(format!("frametrace-validation-pair-{}", std::process::id())),
        );
        let carved = dir.join("artifacts/carved");
        fs::create_dir_all(&carved).unwrap();
        let run1 = carved.join("carve_000001_run1.mp4");
        let run2 = carved.join("carve_000001_run2.mp4");
        fs::write(&run1, b"one").unwrap();
        fs::write(&run2, b"two").unwrap();
        let line = |path: &std::path::Path| {
            serde_json::json!({"event": "carve-file", "id": "carve_000001", "output_path": path.to_string_lossy()})
                .to_string()
        };
        crate::audit::test_write_lane(
            &carved.join("carve-log.jsonl"),
            &format!("{}\n{}\n{}\n", line(&run1), line(&run2), line(&run2)),
        );
        let err = resolve_artifact_path(&dir, "carve_000001").unwrap_err();
        assert!(err.contains("ambiguous selector"), "{err}");
        let chosen = resolve_artifact_pair(&dir, "carve_000001", &run2).unwrap();
        assert_eq!(fs::read(chosen).unwrap(), b"two");
        assert!(resolve_artifact_pair(&dir, "carve_000002", &run2).is_err());
        // Once only one of the files exists the id is unambiguous again.
        fs::remove_file(&run1).unwrap();
        let only = resolve_artifact_path(&dir, "carve_000001").unwrap();
        assert_eq!(fs::read(only).unwrap(), b"two");
        let _ = fs::remove_dir_all(dir);
    }
}
