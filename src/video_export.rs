use crate::audit;
use crate::tool_policy::{command_version, require_case_output_path, resolve_tool_binary};
use crate::util::{
    canonicalize_display, json_escape, now_unix, read_to_string, unique_available_path,
};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Mp4,
    Avi,
}

impl ExportFormat {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.to_ascii_lowercase().as_str() {
            "mp4" => Ok(Self::Mp4),
            "avi" => Ok(Self::Avi),
            other => Err(format!(
                "unsupported export format: {other} (use mp4 or avi)"
            )),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Avi => "avi",
        }
    }
}

/// Court-submission burn-in request: draw the exhibit label, case id,
/// and a short source-hash prefix onto every frame plus a millisecond
/// timecode, so a derived clip stays self-identifying when detached
/// from the case report.
#[derive(Debug, Clone)]
pub struct BurnInSpec {
    /// Exhibit label such as "갑 제3호증" — empty still burns the
    /// case id, hash prefix, and timecode.
    pub exhibit: String,
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub start_seconds: Option<f64>,
    pub duration_seconds: Option<f64>,
    pub output_path: Option<PathBuf>,
    pub timeout_secs: Option<u64>,
    pub burn_in: Option<BurnInSpec>,
    /// Digest the source at export time so the clip's recorded identity
    /// does not rest solely on a possibly-stale index entry. The live
    /// digest is logged as `source_sha256` and a disagreement with the
    /// indexed hash is logged as `source_hash_mismatch` rather than
    /// silently preferring either value.
    pub hash_source: bool,
}

#[derive(Debug, Clone)]
pub struct ExportResult {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub format: ExportFormat,
}

pub fn export_video(
    case_dir: &Path,
    selector: &str,
    options: &ExportOptions,
) -> Result<ExportResult, String> {
    // Time-range args must be sane BEFORE ffmpeg runs: a negative start
    // silently exports the full video while the audit log records the
    // bogus value, and a start past EOF produces a "successful" 0-frame
    // clip. Reject both instead of handing over mislabeled deliverables.
    for (label, value) in [
        ("start", options.start_seconds),
        ("duration", options.duration_seconds),
    ] {
        if let Some(value) = value
            && (value.is_sign_negative() || !value.is_finite())
        {
            return Err(format!(
                "invalid {label} seconds {value}: must be a finite non-negative number"
            ));
        }
    }
    let source_path = resolve_video_source(case_dir, selector)?;
    let export_unix = now_unix()?;
    let output_path = if let Some(output_path) = &options.output_path {
        require_case_output_path(case_dir, output_path, "video export")?;
        if output_path.exists() {
            return Err(format!(
                "output already exists: {} (choose a new --output path)",
                output_path.display()
            ));
        }
        output_path.clone()
    } else {
        // Not reserved (no empty placeholder under the final name): the
        // clip is written to a `.part` and published without clobbering.
        unique_available_path(&case_dir.join("artifacts/clips").join(format!(
            "{}_{}.{}",
            sanitize_filename(selector),
            export_unix,
            options.format.extension()
        )))
    };

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create output directory: {err}"))?;
        // Leftovers of an export that was killed long ago (C4).
        for (path, result) in crate::join::sweep_stale_parts(parent) {
            if let Err(err) = result {
                eprintln!("warning: {err} ({})", path.display());
            }
        }
    }

    let indexed_sha256 = audit::indexed_source_hash(case_dir, selector, &source_path);
    // --hash-source asks for a live digest so a stale index cannot carry
    // the deliverable's identity; burn-in auto-digests rather than stamp
    // "unhashed" on a court submission.
    let export_sha256 =
        if options.hash_source || (options.burn_in.is_some() && indexed_sha256.is_none()) {
            Some(audit::digest_file(&source_path)?)
        } else {
            None
        };
    // A fresh digest is the honest current-state identity, so it wins the
    // burn-in label when present; otherwise the acquisition-time indexed
    // hash is the evidence identity.
    let label_sha256 = export_sha256.as_deref().or(indexed_sha256.as_deref());

    // C4: ffmpeg writes a pid-tagged `.part`; only a complete, non-empty
    // clip ever gets the deliverable's name (a killed export leaves the
    // `.part`, which the server sweeps).
    let part = crate::join::pid_part_path(&output_path, std::process::id());
    if let Err(err) = run_ffmpeg_export(
        case_dir,
        &source_path,
        &output_path,
        &part,
        options,
        label_sha256,
    ) {
        let _ = crate::join::remove_file_retry(&part);
        return Err(err);
    }
    match crate::join::publish_part_as(&part, &output_path) {
        Ok(true) => {}
        Ok(false) => {
            let _ = crate::join::remove_file_retry(&part);
            return Err(format!(
                "output already exists: {} (choose a new --output path)",
                output_path.display()
            ));
        }
        Err(err) => {
            let _ = crate::join::remove_file_retry(&part);
            return Err(err);
        }
    }
    write_export_log(
        case_dir,
        selector,
        &source_path,
        &output_path,
        options,
        indexed_sha256.as_deref(),
        export_sha256.as_deref(),
    )?;

    Ok(ExportResult {
        source_path,
        output_path,
        format: options.format,
    })
}

fn run_ffmpeg_export(
    case_dir: &Path,
    source_path: &Path,
    output_path: &Path,
    write_path: &Path,
    options: &ExportOptions,
    source_sha256: Option<&str>,
) -> Result<(), String> {
    let mut args = ffmpeg_export_args(case_dir, source_path, output_path, options, source_sha256);
    // The container is named by `-f` (the `.part` name has no usable
    // extension); ffmpeg writes to the in-progress name.
    if let Some(last) = args.last_mut() {
        *last = audit::path_string(write_path);
    }
    let final_path = output_path;
    let output_path = write_path;
    let ffmpeg = resolve_tool_binary("ffmpeg", &["ffmpeg"])
        .map_err(|err| format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)"))?;
    // Windows ffmpeg builds neither accept non-ASCII argv text nor resolve
    // absolute font paths inside filter options (any scheme — C:/, /c/ —
    // silently falls back to a Latin-only font, boxing Hangul). The label
    // is written to a UTF-8 sidecar and Malgun Gothic is copied beside the
    // clip; both are referenced by relative name with ffmpeg's cwd pinned
    // to the clips dir, so no path or label byte crosses argv at all.
    let label_cwd = if let Some(spec) = &options.burn_in {
        let info = burn_in_info_text(case_dir, source_path, spec, source_sha256);
        let label_path = burn_in_label_path(final_path);
        // A UTF-8 BOM keeps every drawtext build on the UTF-8 path.
        std::fs::write(&label_path, format!("\u{feff}{info}"))
            .map_err(|err| format!("failed to write burn-in label file: {err}"))?;
        let clips_dir = label_path.parent().map(Path::to_path_buf);
        if burn_in_font_available()
            && let Some(dir) = &clips_dir
        {
            let font_dest = dir.join(BURN_IN_FONT_NAME);
            if !font_dest.exists() {
                std::fs::copy(BURN_IN_FONT_SOURCE, &font_dest)
                    .map_err(|err| format!("failed to stage burn-in font: {err}"))?;
            }
        }
        clips_dir
    } else {
        None
    };
    let mut command = Command::new(&ffmpeg);
    command.args(&args);
    if let Some(dir) = &label_cwd {
        command.current_dir(dir);
    }
    let output =
        crate::util::run_with_timeout(&mut command, options.timeout_secs).map_err(|err| {
            if err.contains("os error 2") {
                format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)")
            } else {
                err
            }
        })?;

    if !output.status.success() {
        let _ = std::fs::remove_file(output_path);
        return Err(format!(
            "ffmpeg export failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // ffmpeg can exit 0 while refusing to write (historically with -n);
    // a "successful" export must always produce a non-empty file, and a
    // 0-byte deliverable must never reach the audit log.
    let size = std::fs::metadata(output_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    if size == 0 {
        let _ = std::fs::remove_file(output_path);
        return Err(format!(
            "ffmpeg reported success but wrote no output bytes: {}",
            output_path.display()
        ));
    }
    Ok(())
}

fn ffmpeg_export_args(
    case_dir: &Path,
    source_path: &Path,
    output_path: &Path,
    options: &ExportOptions,
    source_sha256: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        // The output path was claimed exclusively by unique_path's O_EXCL
        // reservation, so overwriting our own placeholder with -y is the
        // intended flow. (-n refused the placeholder and ffmpeg still
        // exited 0, which produced 0-byte deliverables logged as success.)
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-i".to_string(),
        audit::path_string(source_path),
    ];
    if let Some(start) = options.start_seconds {
        args.push("-ss".to_string());
        args.push(format!("{start:.3}"));
    }
    if let Some(duration) = options.duration_seconds {
        args.push("-t".to_string());
        args.push(format!("{duration:.3}"));
    }
    args.extend([
        "-map".to_string(),
        "0:v:0".to_string(),
        "-map".to_string(),
        "0:a?".to_string(),
    ]);

    if let Some(burn_in) = &options.burn_in {
        let label_name = burn_in_label_path(output_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        args.extend([
            "-vf".to_string(),
            burn_in_filter(
                case_dir,
                source_path,
                burn_in,
                Some(&label_name),
                source_sha256,
            ),
        ]);
    }

    match options.format {
        ExportFormat::Mp4 => {
            args.extend(
                [
                    "-c:v",
                    "libx264",
                    "-preset",
                    "veryfast",
                    "-crf",
                    "20",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-movflags",
                    "+faststart",
                ]
                .iter()
                .map(|arg| arg.to_string()),
            );
        }
        ExportFormat::Avi => {
            args.extend(
                ["-c:v", "mpeg4", "-q:v", "3", "-c:a", "libmp3lame"]
                    .iter()
                    .map(|arg| arg.to_string()),
            );
        }
    }
    args.push("-f".to_string());
    args.push(
        match options.format {
            ExportFormat::Mp4 => "mp4",
            ExportFormat::Avi => "avi",
        }
        .to_string(),
    );
    args.push(audit::path_string(output_path));
    args
}

/// The UTF-8 sidecar that carries the burn-in info line to drawtext's
/// `textfile=` option. Sits next to the exported clip inside the case dir.
fn burn_in_label_path(output_path: &Path) -> PathBuf {
    let mut name = output_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "clip".to_string());
    name.push_str(".burn-in.txt");
    output_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(name)
}

/// The human-readable burn-in info line: optional exhibit label, case id,
/// and the source's sha256 prefix.
fn burn_in_info_text(
    case_dir: &Path,
    source_path: &Path,
    spec: &BurnInSpec,
    source_sha256: Option<&str>,
) -> String {
    let case_id = read_to_string(&case_dir.join("case.json"))
        .ok()
        .and_then(|text| {
            serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("case_id")
                        .and_then(|id| id.as_str().map(str::to_string))
                })
        })
        .unwrap_or_else(|| "case".to_string());
    let hash_prefix = source_sha256
        .map(|hash| hash.chars().take(8).collect::<String>())
        .or_else(|| {
            audit::indexed_source_hash(case_dir, "", source_path)
                .map(|hash| hash.chars().take(8).collect::<String>())
        })
        .unwrap_or_else(|| "unhashed".to_string());
    if spec.exhibit.trim().is_empty() {
        format!("{case_id} · sha256:{hash_prefix}")
    } else {
        format!("{} · {case_id} · sha256:{hash_prefix}", spec.exhibit.trim())
    }
}

/// Builds the drawtext chain for court-submission burn-in: an info line
/// (exhibit label · case id · sha256 prefix) at the top-left and a
/// millisecond pts:hms timecode at the bottom-left. Malgun Gothic is
/// used when present so Korean exhibit labels do not render as boxes.
///
/// `label_file` is the basename of the UTF-8 sidecar written beside the
/// output — drawtext reads it via `textfile=` because Windows ffmpeg
/// builds mangle non-ASCII argv. When absent (tests), the info line is
/// inlined into `text=`.
const BURN_IN_FONT_SOURCE: &str = "C:/Windows/Fonts/malgun.ttf";
const BURN_IN_FONT_NAME: &str = "malgun.ttf";

fn burn_in_font_available() -> bool {
    Path::new(BURN_IN_FONT_SOURCE).is_file()
}

fn burn_in_filter(
    case_dir: &Path,
    source_path: &Path,
    spec: &BurnInSpec,
    label_file: Option<&str>,
    source_sha256: Option<&str>,
) -> String {
    // The font is staged next to the clip and referenced by basename —
    // absolute font paths inside filter options silently fail to load on
    // Windows ffmpeg builds, boxing every Hangul glyph.
    let font_arg = if burn_in_font_available() {
        ":fontfile='malgun.ttf'"
    } else {
        ""
    };
    let label_arg = match label_file {
        Some(name) => format!("textfile='{}'", escape_drawtext(name)),
        None => format!(
            "text='{}'",
            escape_drawtext(&burn_in_info_text(
                case_dir,
                source_path,
                spec,
                source_sha256
            ))
        ),
    };
    format!(
        "drawtext={label_arg}:x=12:y=10:fontsize=22:fontcolor=white:borderw=2:bordercolor=black@0.85{font_arg},\
         drawtext=text='%{{pts\\:hms}}':x=12:y=h-th-12:fontsize=24:fontcolor=white:borderw=2:bordercolor=black@0.85{font_arg}"
    )
}

/// Escapes text for the drawtext `text='...'` option: `\`, `'`, `:`,
/// `,`, and `%` all have filter-graph or expansion meaning.
fn escape_drawtext(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            ':' => out.push_str("\\:"),
            ',' => out.push_str("\\,"),
            '%' => out.push_str("%%"),
            _ => out.push(ch),
        }
    }
    out
}

/// C6: per-thumbnail bound for make-review's ffmpeg frame grab. A damaged
/// or endlessly-streaming source used to hang make-review (and with it the
/// whole pipeline) forever.
pub const REVIEW_THUMB_TIMEOUT_SECS: u64 = 60;

/// C6: grabs one review thumbnail (`seek` seconds in, 288 px wide) from
/// `source` into `output` through a pid-tagged `.part`, bounded by
/// `timeout_secs`. Only a complete, non-empty JPEG ever gets the final
/// name; on any failure (exit, timeout, empty) the `.part` is removed and
/// the caller shows a placeholder plus a warning.
pub fn extract_review_thumbnail(
    ffmpeg: &Path,
    source: &str,
    output: &Path,
    seek: &str,
    timeout_secs: u64,
) -> Result<(), String> {
    let mut part = crate::join::pid_part_path(output, std::process::id()).into_os_string();
    // Keep a `.jpg` suffix so ffmpeg's image2 muxer accepts the name.
    part.push(".jpg");
    let part = PathBuf::from(part);
    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-y",
            "-loglevel",
            "error",
            "-ss",
            seek,
            "-i",
            source,
            "-frames:v",
            "1",
            "-vf",
            "scale=288:-2",
            "-q:v",
            "5",
        ])
        .arg(&part);
    let result = crate::util::run_with_timeout(&mut command, Some(timeout_secs));
    let outcome = match result {
        Ok(out) if out.status.success() => {
            if std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) == 0 {
                Err("ffmpeg wrote no thumbnail bytes".to_string())
            } else {
                // Replacing an older thumbnail is intended here (make-review
                // regenerates stale ones): rename over it atomically.
                std::fs::rename(&part, output)
                    .map_err(|err| format!("failed to publish thumbnail: {err}"))
            }
        }
        Ok(out) => Err(format!(
            "ffmpeg thumbnail failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(err) => Err(err),
    };
    if outcome.is_err() {
        let _ = crate::join::remove_file_retry(&part);
    }
    outcome
}

/// C12: an index record id exactly as the scanner assigns it,
/// `^vid_\d{6,}$`.
pub fn is_index_video_id(id: &str) -> bool {
    id.strip_prefix("vid_")
        .is_some_and(|digits| digits.len() >= 6 && digits.bytes().all(|b| b.is_ascii_digit()))
}

pub fn resolve_video_source(case_dir: &Path, selector: &str) -> Result<PathBuf, String> {
    // C12: anything carrying the `vid_` prefix is an index id and is
    // resolved through the index ONLY — never as a (relative) path, so
    // `vid_x/../../f` cannot reach a file outside the index.
    let id_shaped = selector.starts_with("vid_");
    if id_shaped && !is_index_video_id(selector) {
        return Err(format!(
            "invalid video id {:?}: expected vid_ followed by at least 6 digits",
            selector.chars().take(64).collect::<String>()
        ));
    }
    let direct = PathBuf::from(selector);
    if !id_shaped && direct.is_file() {
        return canonicalize_display(&direct)
            .map_err(|err| format!("failed to canonicalize source path: {err}"));
    }

    if let Some(found) = resolve_through_records(case_dir, selector, id_shaped) {
        return found;
    }

    let index_path = case_dir.join("db/video_paths.tsv");
    let text = read_to_string(&index_path).map_err(|err| {
        format!(
            "failed to read {}: {err} (run scan-folder first)",
            index_path.display()
        )
    })?;

    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 3 {
            continue;
        }
        let id = fields[0];
        let source_path = tsv_unescape(fields[1]);
        let relative_path = tsv_unescape(fields[2]);
        let matched = if id_shaped {
            selector == id
        } else {
            selector == id || selector == source_path || selector == relative_path
        };
        if matched {
            let path = PathBuf::from(&source_path);
            if path.is_file() {
                return canonicalize_display(&path)
                    .map_err(|err| format!("failed to canonicalize source path: {err}"));
            }
            return Err(format!(
                "indexed source file no longer exists: {source_path}"
            ));
        }
    }

    Err(format!(
        "video selector not found: {selector} (use an indexed id like vid_000001 or a source path)"
    ))
}

/// R6-I10: [`resolve_video_source`] through `db/videos.jsonl`, which keeps
/// the lossless raw form of a non-Unicode name (`source_path_raw_hex`) —
/// the path index (`video_paths.tsv`) only has the lossy display string,
/// which names another (usually nonexistent) file. `None` when the records
/// cannot be read (the TSV fallback then applies) or name no such record.
/// A display/relative path shared by several records (non-Unicode twins)
/// is ambiguous — refused, never resolved to the first.
fn resolve_through_records(
    case_dir: &Path,
    selector: &str,
    id_shaped: bool,
) -> Option<Result<PathBuf, String>> {
    let text = read_to_string(&case_dir.join("db/videos.jsonl")).ok()?;
    let rows: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();
    let field = |row: &serde_json::Value, key: &str| {
        row.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let found = match rows
        .iter()
        .find(|row| field(row, "id").as_deref() == Some(selector))
    {
        Some(row) => row,
        None if id_shaped => return None,
        None => {
            let matches: Vec<&serde_json::Value> = rows
                .iter()
                .filter(|row| {
                    field(row, "source_path").as_deref() == Some(selector)
                        || field(row, "relative_path").as_deref() == Some(selector)
                })
                .collect();
            match matches.as_slice() {
                [] => return None,
                [row] => *row,
                _ => {
                    return Some(Err(format!(
                        "video selector {selector} names {} indexed files (names that differ only in non-Unicode characters) — use the record id (vid_…)",
                        matches.len()
                    )));
                }
            }
        }
    };
    let path = crate::model::record_os_path(found)?;
    Some(if path.is_file() {
        crate::model::canonicalize_lossless(&path)
            .map_err(|err| format!("failed to canonicalize source path: {err}"))
    } else {
        Err(format!(
            "indexed source file no longer exists: {}",
            path.display()
        ))
    })
}

fn write_export_log(
    case_dir: &Path,
    selector: &str,
    source_path: &Path,
    output_path: &Path,
    options: &ExportOptions,
    source_index_sha256: Option<&str>,
    source_export_sha256: Option<&str>,
) -> Result<(), String> {
    let path = case_dir.join("artifacts/clips/export-log.jsonl");
    let exported_unix = now_unix()?;
    let output_sha256 = audit::digest_file(output_path)?;
    let args = ffmpeg_export_args(case_dir, source_path, output_path, options, None);
    let source_hash_mismatch = match (source_index_sha256, source_export_sha256) {
        (Some(indexed), Some(live)) => Some(indexed != live),
        _ => None,
    };
    let line = format!(
        "{{\"schema_version\":2,\"event\":\"export-video\",\"exported_unix\":{},\"selector\":\"{}\",\"source_path\":\"{}\",\"source_index_sha256\":{},\"source_sha256\":{},\"source_hash_mismatch\":{},\"output_path\":\"{}\",\"output_sha256\":\"{}\",\"format\":\"{}\",\"start_seconds\":{},\"duration_seconds\":{},\"burn_in\":{},\"ffmpeg_version\":\"{}\",\"command\":\"ffmpeg\",\"command_args\":{}}}",
        exported_unix,
        json_escape(selector),
        json_escape(&source_path.to_string_lossy()),
        audit::optional_string(source_index_sha256),
        audit::optional_string(source_export_sha256),
        source_hash_mismatch
            .map(|flag| flag.to_string())
            .unwrap_or_else(|| "null".to_string()),
        json_escape(&output_path.to_string_lossy()),
        json_escape(&output_sha256),
        options.format.extension(),
        optional_f64(options.start_seconds),
        optional_f64(options.duration_seconds),
        options
            .burn_in
            .as_ref()
            .map(|spec| format!("{{\"exhibit\":\"{}\"}}", json_escape(&spec.exhibit)))
            .unwrap_or_else(|| "null".to_string()),
        json_escape(&command_version("ffmpeg", &["ffmpeg"], "-version")),
        audit::json_string_array(&args)
    );
    audit::append_chained_jsonl(&path, &line)
}

fn optional_f64(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.3}"))
        .unwrap_or_else(|| "null".to_string())
}

fn tsv_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

pub use crate::util::sanitize_filename;

#[cfg(test)]
mod tests {
    use super::{ExportFormat, ExportOptions, ffmpeg_export_args, sanitize_filename, tsv_unescape};
    use std::path::Path;

    /// R6-I10: an indexed non-Unicode name resolves to ITS file (the path
    /// index's lossy display string named another, nonexistent one), and a
    /// display path shared by twins is refused as ambiguous.
    #[test]
    fn resolves_non_unicode_records_losslessly() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-i10-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db")).unwrap();
        let one = dir.join(crate::audit::test_non_unicode_twin("clip", 0));
        let two = dir.join(crate::audit::test_non_unicode_twin("clip", 1));
        if std::fs::write(&two, b"two").is_err() {
            eprintln!("filesystem refuses non-Unicode names; skipped");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        std::fs::write(&one, b"one").unwrap();
        let display = two.to_string_lossy().to_string();
        let row = |id: &str, path: &Path| {
            serde_json::json!({"id": id, "source_path": path.to_string_lossy(),
                "relative_path": "clip", "source_path_raw_hex": crate::model::path_raw_hex(path)})
            .to_string()
        };
        std::fs::write(
            dir.join("db/videos.jsonl"),
            format!("{}\n{}\n", row("vid_000001", &one), row("vid_000002", &two)),
        )
        .unwrap();
        std::fs::write(
            dir.join("db/video_paths.tsv"),
            format!("id\tsource_path\trelative_path\nvid_000001\t{display}\tclip\nvid_000002\t{display}\tclip\n"),
        )
        .unwrap();
        let resolved = super::resolve_video_source(&dir, "vid_000002").unwrap();
        assert_eq!(std::fs::read(&resolved).unwrap(), b"two");
        let err = super::resolve_video_source(&dir, &display).unwrap_err();
        assert!(err.contains("2 indexed files"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_export_formats() {
        assert_eq!(ExportFormat::parse("mp4").unwrap(), ExportFormat::Mp4);
        assert_eq!(ExportFormat::parse("AVI").unwrap(), ExportFormat::Avi);
        assert!(ExportFormat::parse("mkv").is_err());
    }

    #[test]
    fn sanitizes_clip_names() {
        assert_eq!(sanitize_filename("vid_000001"), "vid_000001");
        assert_eq!(sanitize_filename("a/b:c.mp4"), "a_b_c_mp4");
    }

    #[test]
    fn builds_export_command_args() {
        let options = ExportOptions {
            format: ExportFormat::Mp4,
            start_seconds: Some(1.0),
            duration_seconds: Some(2.0),
            output_path: None,
            timeout_secs: None,
            burn_in: None,
            hash_source: false,
        };
        let args = ffmpeg_export_args(
            Path::new("."),
            Path::new("in.mp4"),
            Path::new("out.mp4"),
            &options,
            None,
        );
        assert!(args.contains(&"-y".to_string()));
        assert!(!args.contains(&"-n".to_string()));
        assert!(args.contains(&"libx264".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("out.mp4"));
    }

    #[test]
    fn unescapes_tsv_paths() {
        assert_eq!(tsv_unescape("a\\tb\\\\c"), "a\tb\\c");
    }

    /// C12: only the scanner's exact id form counts as an index id.
    #[test]
    fn index_video_id_table() {
        use super::is_index_video_id;
        for ok in ["vid_000001", "vid_123456", "vid_0000001234"] {
            assert!(is_index_video_id(ok), "{ok}");
        }
        for bad in [
            "vid_1",
            "vid_00001",
            "vid_x",
            "vid_000001/../x",
            "vid_x\\..\\..\\f.mp4",
            "vid_000001 ",
            "vid_",
            "VID_000001",
            "carve_000001",
            "vid_٠٠٠٠٠١",
        ] {
            assert!(!is_index_video_id(bad), "{bad}");
        }
    }

    /// C12: a `vid_`-prefixed selector is never resolved as a path — the
    /// traversal form used to reach any file (Windows resolves `..`
    /// lexically, so `vid_x` need not exist), and a well-formed id is
    /// looked up in the index only.
    #[test]
    fn vid_selectors_resolve_through_the_index_only() {
        use super::resolve_video_source;
        let dir = std::env::temp_dir().join(format!("ft-c12-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db")).unwrap();
        let target = dir.join("outside.mp4");
        std::fs::write(&target, b"x").unwrap();
        let cwd = std::env::current_dir().unwrap();
        let depth = cwd.components().count();
        let mut rel = String::from("vid_x");
        for _ in 0..depth {
            rel.push_str("/..");
        }
        let tail: std::path::PathBuf = target
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .collect();
        rel.push('/');
        rel.push_str(&tail.to_string_lossy().replace('\\', "/"));
        let err = resolve_video_source(&dir, &rel).unwrap_err();
        assert!(err.contains("invalid video id"), "{err}");
        // Well-formed id: index only.
        let indexed = dir.join("indexed.mp4");
        std::fs::write(&indexed, b"y").unwrap();
        std::fs::write(
            dir.join("db/video_paths.tsv"),
            format!(
                "id\tsource_path\trelative_path\nvid_000001\t{}\tindexed.mp4\n",
                indexed.display()
            ),
        )
        .unwrap();
        let resolved = resolve_video_source(&dir, "vid_000001").unwrap();
        assert!(resolved.ends_with("indexed.mp4"), "{resolved:?}");
        assert!(resolve_video_source(&dir, "vid_000002").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// C6: a frame grab that never finishes is killed at the bound and
    /// leaves neither the thumbnail nor its `.part` behind.
    #[test]
    fn review_thumbnail_grab_is_bounded() {
        use super::extract_review_thumbnail;
        let dir = std::env::temp_dir().join(format!("ft-c6-thumb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = if cfg!(windows) {
            let path = dir.join("hang.cmd");
            // Idle, not a CPU-pinning `goto` loop (parallel tests).
            std::fs::write(
                &path,
                "@echo off\r\n:spin\r\nping -n 2 127.0.0.1 >nul 2>&1\r\ngoto spin\r\n",
            )
            .unwrap();
            path
        } else {
            let path = dir.join("hang.sh");
            std::fs::write(&path, "#!/bin/sh\nexec sleep 60\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };
        let output = dir.join("vid_000001.jpg");
        let started = std::time::Instant::now();
        let result = extract_review_thumbnail(&fake, "in.mp4", &output, "5", 1);
        assert!(result.is_err());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "{:?}",
            started.elapsed()
        );
        assert!(!output.exists());
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn burn_in_filter_stamps_label_hash_and_timecode() {
        let spec = super::BurnInSpec {
            exhibit: "갑 제3호증".to_string(),
        };
        let filter = super::burn_in_filter(
            Path::new("."),
            Path::new("in.mp4"),
            &spec,
            Some("clip.burn-in.txt"),
            Some("0123456789abcdef"),
        );
        assert!(filter.contains("drawtext"));
        assert!(filter.contains("pts\\:hms"), "ms timecode: {filter}");
        assert!(
            filter.contains("textfile='clip.burn-in.txt'"),
            "label rides a UTF-8 sidecar so non-ASCII argv never mangles it: {filter}"
        );
        let info = super::burn_in_info_text(
            Path::new("."),
            Path::new("in.mp4"),
            &spec,
            Some("0123456789abcdef"),
        );
        assert!(info.contains("갑 제3호증"), "exhibit label: {info}");
        assert!(info.contains("sha256:01234567"), "hash prefix: {info}");
    }

    #[test]
    fn burn_in_prefers_fresh_digest_over_unhashed() {
        let spec = super::BurnInSpec {
            exhibit: String::new(),
        };
        let info = super::burn_in_info_text(
            Path::new("."),
            Path::new("in.mp4"),
            &spec,
            Some("feedface01234567"),
        );
        assert!(
            info.contains("sha256:feedface"),
            "export-time digest should label the clip: {info}"
        );
        assert!(!info.contains("unhashed"), "{info}");
    }

    #[test]
    fn drawtext_escapes_filter_graph_chars() {
        assert_eq!(
            super::escape_drawtext("a:b,c'd\\e%f"),
            "a\\:b\\,c\\'d\\\\e%%f"
        );
    }
}
