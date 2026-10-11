use crate::audit;
use crate::tool_policy::{command_version, require_case_output_path, resolve_tool_binary};
use crate::util::{canonicalize_display, json_escape, now_unix, read_to_string, unique_path};
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
        unique_path(&case_dir.join("artifacts/clips").join(format!(
            "{}_{}.{}",
            sanitize_filename(selector),
            export_unix,
            options.format.extension()
        )))
    };

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create output directory: {err}"))?;
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

    run_ffmpeg_export(case_dir, &source_path, &output_path, options, label_sha256)?;
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
    options: &ExportOptions,
    source_sha256: Option<&str>,
) -> Result<(), String> {
    let args = ffmpeg_export_args(case_dir, source_path, output_path, options, source_sha256);
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
        let label_path = burn_in_label_path(output_path);
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

pub fn resolve_video_source(case_dir: &Path, selector: &str) -> Result<PathBuf, String> {
    let direct = PathBuf::from(selector);
    if direct.is_file() {
        return canonicalize_display(&direct)
            .map_err(|err| format!("failed to canonicalize source path: {err}"));
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
        if selector == id || selector == source_path || selector == relative_path {
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
