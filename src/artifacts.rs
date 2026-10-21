use crate::audit;
use crate::tool_policy::{command_version, require_case_output_path, resolve_tool_binary};
use crate::util::{json_escape, now_unix, unique_path};
use crate::video_export::{resolve_video_source, sanitize_filename};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Transcode-class lanes default to no wall-clock cap (large legitimate
/// media can encode for a long time), but a hung decode must never wedge
/// the examiner's session forever: this hard ceiling only catches hangs,
/// not slow work. Callers can pass a larger value via options later.
const TRANSCODE_HARD_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone)]
pub struct ProxyOptions {
    pub output_path: Option<PathBuf>,
    pub max_width: u32,
    /// Timeout in seconds for the ffmpeg run (None = hard default above).
    pub timeout_secs: Option<u64>,
}

impl Default for ProxyOptions {
    fn default() -> Self {
        Self {
            output_path: None,
            max_width: 1280,
            timeout_secs: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThumbnailOptions {
    pub output_path: Option<PathBuf>,
    pub time_seconds: f64,
    /// Timeout in seconds for the ffmpeg run (None = hard default above).
    pub timeout_secs: Option<u64>,
}

impl Default for ThumbnailOptions {
    fn default() -> Self {
        Self {
            output_path: None,
            time_seconds: 0.0,
            timeout_secs: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DerivedArtifact {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub kind: String,
    pub created_unix: u64,
}

pub fn generate_proxy(
    case_dir: &Path,
    selector: &str,
    options: &ProxyOptions,
) -> Result<DerivedArtifact, String> {
    if options.max_width < 160 {
        return Err("--max-width must be at least 160".to_string());
    }
    let source_path = resolve_video_source(case_dir, selector)?;
    let created_unix = now_unix()?;
    let output_path = if let Some(output_path) = &options.output_path {
        require_case_output_path(case_dir, output_path, "proxy")?;
        if output_path.exists() {
            return Err(format!(
                "output already exists: {} (choose a new --output path)",
                output_path.display()
            ));
        }
        output_path.clone()
    } else {
        // D5: path-free name — `proxy_<record id>_<hash8>.mp4`, never the
        // (often absolute) selector itself.
        let record_id = artifact_record_id(case_dir, selector, &source_path);
        unique_path(&case_dir.join("artifacts/proxies").join(artifact_file_name(
            "proxy",
            &record_id,
            &source_path,
            "mp4",
        )))
    };
    ensure_parent(&output_path)?;

    let ffmpeg = resolve_tool_binary("ffmpeg", &["ffmpeg"])
        .map_err(|err| format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)"))?;
    // A build without libx264 (e.g. the BtbN build the installer fetches):
    // use the best H.264 encoder this build really has (recorded in the
    // proxy log).
    let encoders = crate::ffmpeg_encoders::for_binary(&ffmpeg);
    let codecs = encoders.choice(encoders.video(), encoders.aac());
    let args = proxy_ffmpeg_args(&source_path, &output_path, options, &codecs);
    // Fix round 12 L2 (as round 11 H5 for exports): the version of the
    // binary that encodes, asked BEFORE encoding from the same resolved
    // path — asked afterwards by bare name, a guided install finishing
    // meanwhile would record the new build for a proxy made by the old one.
    let ffmpeg_version = command_version(&ffmpeg, &["ffmpeg"], "-version");
    let mut command = Command::new(&ffmpeg);
    command.args(&args);
    let output = crate::util::run_with_timeout(
        &mut command,
        Some(options.timeout_secs.unwrap_or(TRANSCODE_HARD_TIMEOUT_SECS)),
    )
    .map_err(|err| {
        if err.contains("os error 2") {
            format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)")
        } else {
            format!("failed to run ffmpeg for proxy: {err}")
        }
    })?;

    if !output.status.success() {
        let _ = std::fs::remove_file(output_path);
        return Err(format!(
            "proxy generation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // A 0-byte proxy must never be logged as a generated artifact.
    if std::fs::metadata(&output_path)
        .map(|meta| meta.len())
        .unwrap_or(0)
        == 0
    {
        let _ = std::fs::remove_file(&output_path);
        return Err(format!(
            "ffmpeg reported success but wrote no output bytes: {}",
            output_path.display()
        ));
    }

    let artifact = DerivedArtifact {
        source_path,
        output_path,
        kind: "proxy".to_string(),
        created_unix,
    };
    append_artifact_log(
        case_dir,
        "artifacts/proxies/proxy-log.jsonl",
        &artifact,
        &args,
        Some(&codecs),
        &ffmpeg_version,
    )?;
    Ok(artifact)
}

pub fn generate_thumbnail(
    case_dir: &Path,
    selector: &str,
    options: &ThumbnailOptions,
) -> Result<DerivedArtifact, String> {
    if options.time_seconds.is_sign_negative() || !options.time_seconds.is_finite() {
        return Err("--time must be a non-negative finite number".to_string());
    }
    let source_path = resolve_video_source(case_dir, selector)?;
    let created_unix = now_unix()?;
    let output_path = if let Some(output_path) = &options.output_path {
        require_case_output_path(case_dir, output_path, "thumbnail")?;
        if output_path.exists() {
            return Err(format!(
                "output already exists: {} (choose a new --output path)",
                output_path.display()
            ));
        }
        output_path.clone()
    } else {
        // D5: path-free name — `thumb_<record id>_<hash8>.jpg`.
        let record_id = artifact_record_id(case_dir, selector, &source_path);
        unique_path(
            &case_dir
                .join("artifacts/thumbnails")
                .join(artifact_file_name("thumb", &record_id, &source_path, "jpg")),
        )
    };
    ensure_parent(&output_path)?;

    let args = thumbnail_ffmpeg_args(&source_path, &output_path, options);
    let ffmpeg = resolve_tool_binary("ffmpeg", &["ffmpeg"])
        .map_err(|err| format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)"))?;
    // Fix round 12 L2: asked before running, from the resolved path.
    let ffmpeg_version = command_version(&ffmpeg, &["ffmpeg"], "-version");
    let mut command = Command::new(&ffmpeg);
    command.args(&args);
    let output = crate::util::run_with_timeout(
        &mut command,
        Some(options.timeout_secs.unwrap_or(TRANSCODE_HARD_TIMEOUT_SECS)),
    )
    .map_err(|err| {
        if err.contains("os error 2") {
            format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)")
        } else {
            format!("failed to run ffmpeg for thumbnail: {err}")
        }
    })?;

    if !output.status.success() {
        let _ = std::fs::remove_file(output_path);
        return Err(format!(
            "thumbnail generation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // A 0-byte thumbnail must never be logged as a generated artifact.
    if std::fs::metadata(&output_path)
        .map(|meta| meta.len())
        .unwrap_or(0)
        == 0
    {
        let _ = std::fs::remove_file(&output_path);
        return Err(format!(
            "ffmpeg reported success but wrote no output bytes: {}",
            output_path.display()
        ));
    }

    let artifact = DerivedArtifact {
        source_path,
        output_path,
        kind: "thumbnail".to_string(),
        created_unix,
    };
    append_artifact_log(
        case_dir,
        "artifacts/thumbnails/thumbnail-log.jsonl",
        &artifact,
        &args,
        None,
        &ffmpeg_version,
    )?;
    Ok(artifact)
}

/// D5: the path-free file name of a derived artifact:
/// `<kind>_<record id>_<hash8>.<ext>` (contract §8: `proxy_<record_id>_
/// <hash8>.mp4`, `thumb_<record_id>_<hash8>.jpg`). The record id is
/// sanitized for the file system; `hash8` is the first 8 hex digits of the
/// SHA-256 of the source path, so two sources behind one id (a re-scanned
/// case) never share a name, while the name reveals nothing of the path.
pub fn artifact_file_name(kind: &str, record_id: &str, source_path: &Path, ext: &str) -> String {
    format!(
        "{kind}_{}_{}.{ext}",
        sanitize_filename(record_id),
        source_hash8(source_path)
    )
}

/// First 8 hex digits of the SHA-256 of the source path as a string.
pub fn source_hash8(source_path: &Path) -> String {
    crate::sha256::digest_bytes(audit::path_string(source_path).as_bytes())[..8].to_string()
}

/// The record id a derived artifact of `selector` is filed under (D5): the
/// indexed video id, the carve id, `inode:<offset>:<inode>` for a
/// recovered file, the joined record id — or `file` for a source FrameTrace
/// has no record of. Never a path.
pub fn artifact_record_id(case_dir: &Path, selector: &str, source_path: &Path) -> String {
    let source = audit::path_string(source_path);
    // Only an existing case database is consulted (a lookup never creates one).
    if crate::case_db::case_db_path(case_dir).is_file()
        && let Ok(rows) = crate::case_db::load_video_ids(case_dir)
        && let Some(row) = rows.iter().find(|row| row.id == selector).or_else(|| {
            rows.iter()
                .find(|row| row.source_path == selector || row.source_path == source)
        })
    {
        return row.id.clone();
    }
    let same_file = |logged: &str| {
        let resolved = crate::util::resolve_case_artifact_path(case_dir, logged);
        resolved == source_path
            || audit::canonical_or_original(&resolved) == audit::canonical_or_original(source_path)
    };
    // R9 P3: only entries usable here name an artifact's record.
    let log_entries = |rel: &str| -> Vec<serde_json::Value> {
        audit::read_lane_rows(&case_dir.join(rel))
            .map(|lane| lane.usable_values())
            .unwrap_or_default()
    };
    let text = |value: &serde_json::Value, key: &str| {
        value.get(key).and_then(|v| v.as_str()).map(str::to_string)
    };
    for entry in log_entries("artifacts/carved/carve-log.jsonl").iter().rev() {
        if let (Some(id), Some(output)) = (text(entry, "id"), text(entry, "output_path"))
            && (id == selector || same_file(&output))
        {
            return id;
        }
    }
    for entry in log_entries("evidence/logs/tsk-audit.jsonl").iter().rev() {
        if text(entry, "event").as_deref() != Some("recover-inode") {
            continue;
        }
        if let Some(output) = text(entry, "output_path")
            && same_file(&output)
        {
            let offset = entry
                .get("partition_offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let inode = entry
                .get("inode")
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_default();
            return format!("inode:{offset}:{inode}");
        }
    }
    for entry in log_entries("artifacts/joined/join-log.jsonl").iter().rev() {
        let entry = crate::join::with_record_id(entry.clone());
        if let (Some(id), Some(output)) = (text(&entry, "record_id"), text(&entry, "output_path"))
            && (id == selector || same_file(&output))
        {
            return id;
        }
    }
    "file".to_string()
}

/// D5: the cached proxy of `selector`, if one exists: a path-free
/// `proxy_<record id>_<hash8>[_NNN].mp4`, or — backward compatible — a
/// proxy written before D5 under `<sanitize(selector)>_proxy_*.mp4`. The
/// first match in name order wins, so the lookup is deterministic.
pub fn find_existing_proxy(case_dir: &Path, selector: &str) -> Option<PathBuf> {
    let dir = case_dir.join("artifacts/proxies");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".mp4"))
        .collect();
    names.sort();
    if let Ok(source_path) = resolve_video_source(case_dir, selector) {
        let record_id = artifact_record_id(case_dir, selector, &source_path);
        let stem = format!(
            "proxy_{}_{}",
            sanitize_filename(&record_id),
            source_hash8(&source_path)
        );
        if let Some(name) = names.iter().find(|name| {
            name.strip_prefix(&stem)
                .and_then(|rest| rest.strip_suffix(".mp4"))
                .is_some_and(|rest| {
                    rest.is_empty()
                        || rest
                            .strip_prefix('_')
                            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                })
        }) {
            return Some(dir.join(name));
        }
    }
    let legacy = format!("{}_proxy_", sanitize_filename(selector));
    names
        .iter()
        .find(|name| name.starts_with(&legacy))
        .map(|name| dir.join(name))
}

fn proxy_ffmpeg_args(
    source_path: &Path,
    output_path: &Path,
    options: &ProxyOptions,
    codecs: &crate::ffmpeg_encoders::CodecChoice,
) -> Vec<String> {
    let scale = format!("scale='min({},{})':-2", options.max_width, "iw");
    let mut args = vec![
        // Output path was exclusively claimed by unique_path's O_EXCL
        // reservation; -y overwrites our own placeholder by design.
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-i".to_string(),
        audit::path_string(source_path),
        "-vf".to_string(),
        scale,
        "-map".to_string(),
        "0:v:0".to_string(),
        "-map".to_string(),
        "0:a?".to_string(),
    ];
    args.extend(codecs.video_args(crate::ffmpeg_encoders::Purpose::Proxy));
    args.extend([
        "-c:a".to_string(),
        codecs.audio.to_string(),
        "-movflags".to_string(),
        "+faststart".to_string(),
        audit::path_string(output_path),
    ]);
    args
}

fn thumbnail_ffmpeg_args(
    source_path: &Path,
    output_path: &Path,
    options: &ThumbnailOptions,
) -> Vec<String> {
    vec![
        // Output path was exclusively claimed by unique_path's O_EXCL
        // reservation; -y overwrites our own placeholder by design.
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-ss".to_string(),
        format!("{:.3}", options.time_seconds),
        "-i".to_string(),
        audit::path_string(source_path),
        "-frames:v".to_string(),
        "1".to_string(),
        "-q:v".to_string(),
        "3".to_string(),
        audit::path_string(output_path),
    ]
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create output directory: {err}"))?;
    }
    Ok(())
}

fn append_artifact_log(
    case_dir: &Path,
    relative_log: &str,
    artifact: &DerivedArtifact,
    command_args: &[String],
    codecs: Option<&crate::ffmpeg_encoders::CodecChoice>,
    ffmpeg_version: &str,
) -> Result<(), String> {
    let path = case_dir.join(relative_log);
    let source_sha256 = audit::indexed_source_hash(
        case_dir,
        &artifact.source_path.to_string_lossy(),
        &artifact.source_path,
    );
    let output_sha256 = audit::digest_file(&artifact.output_path)?;
    let line = format!(
        "{{\"schema_version\":2,\"event\":\"make-{}\",\"created_unix\":{},\"kind\":\"{}\",\"source_path\":\"{}\",\"source_index_sha256\":{},\"output_path\":\"{}\",\"output_sha256\":\"{}\",\"ffmpeg_version\":\"{}\",\"command\":\"ffmpeg\",\"command_args\":{}{}}}",
        json_escape(&artifact.kind),
        artifact.created_unix,
        json_escape(&artifact.kind),
        json_escape(&artifact.source_path.to_string_lossy()),
        audit::optional_string(source_sha256.as_deref()),
        json_escape(&artifact.output_path.to_string_lossy()),
        json_escape(&output_sha256),
        json_escape(ffmpeg_version),
        audit::json_string_array(command_args),
        codecs
            .map(|codecs| codecs.audit_fields())
            .unwrap_or_default()
    );
    audit::append_chained_jsonl(&path, &line)
}

#[cfg(test)]
mod tests {
    use super::{ProxyOptions, ThumbnailOptions, proxy_ffmpeg_args, thumbnail_ffmpeg_args};
    use std::path::Path;

    /// Fix round 12 L2: the proxy/thumbnail audit line records the version
    /// the caller asked before encoding, never a fresh bare-name query made
    /// after the artifact was written.
    #[test]
    fn artifact_log_records_the_version_asked_before_encoding() {
        let case = std::env::temp_dir().join(format!("ft-artifact-ver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&case);
        let proxies = case.join("artifacts/proxies");
        std::fs::create_dir_all(&proxies).unwrap();
        let output = proxies.join("proxy.mp4");
        std::fs::write(&output, b"proxy").unwrap();
        let artifact = super::DerivedArtifact {
            source_path: case.join("in.mp4"),
            output_path: output,
            kind: "proxy".to_string(),
            created_unix: 1,
        };
        super::append_artifact_log(
            &case,
            "artifacts/proxies/proxy-log.jsonl",
            &artifact,
            &["-i".to_string()],
            None,
            "ffmpeg version OLD-BUILD-before-install",
        )
        .unwrap();
        let log = std::fs::read_to_string(proxies.join("proxy-log.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(
            line["ffmpeg_version"], "ffmpeg version OLD-BUILD-before-install",
            "{line}"
        );
        let _ = std::fs::remove_dir_all(&case);
    }

    #[test]
    fn default_proxy_is_review_sized() {
        assert_eq!(ProxyOptions::default().max_width, 1280);
    }

    #[test]
    fn default_thumbnail_starts_at_zero() {
        assert_eq!(ThumbnailOptions::default().time_seconds, 0.0);
    }

    /// D5: derived-artifact names carry the record id and a short hash of
    /// the source path — never the path itself.
    #[test]
    fn artifact_names_are_path_free() {
        let source = Path::new(r"C:\Users\kim\Videos\cam1.mp4");
        let name = super::artifact_file_name("proxy", "vid_000001", source, "mp4");
        let hash8 = super::source_hash8(source);
        assert_eq!(name, format!("proxy_vid_000001_{hash8}.mp4"));
        assert_eq!(hash8.len(), 8);
        assert!(hash8.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!name.contains("kim") && !name.contains("Users"), "{name}");
        assert_eq!(
            super::artifact_file_name("thumb", "join:abcdef0123456789", source, "jpg"),
            format!("thumb_join_abcdef0123456789_{hash8}.jpg")
        );
        // Another source behind the same id gets another name.
        assert_ne!(
            super::artifact_file_name("proxy", "vid_000001", Path::new("/e/other.mp4"), "mp4"),
            name
        );
    }

    /// D5: the cache lookup finds a path-free proxy, and still finds a proxy
    /// written before D5 under `<sanitize(selector)>_proxy_*`.
    #[test]
    fn find_existing_proxy_reads_new_and_legacy_names() {
        let case_dir =
            std::env::temp_dir().join(format!("frametrace-artifact-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&case_dir);
        std::fs::create_dir_all(case_dir.join("db")).unwrap();
        std::fs::create_dir_all(case_dir.join("artifacts/proxies")).unwrap();
        let source = case_dir.join("src.mp4");
        std::fs::write(&source, b"x").unwrap();
        let source = crate::util::canonicalize_display(&source).unwrap();
        let selector = source.to_string_lossy().to_string();
        assert_eq!(super::find_existing_proxy(&case_dir, &selector), None);

        let legacy = format!(
            "{}_proxy_1700000000.mp4",
            crate::video_export::sanitize_filename(&selector)
        );
        std::fs::write(case_dir.join("artifacts/proxies").join(&legacy), b"p").unwrap();
        assert_eq!(
            super::find_existing_proxy(&case_dir, &selector),
            Some(case_dir.join("artifacts/proxies").join(&legacy))
        );

        // A source FrameTrace has no record of is filed under `file`.
        let record_id = super::artifact_record_id(&case_dir, &selector, &source);
        assert_eq!(record_id, "file");
        let fresh = super::artifact_file_name("proxy", &record_id, &source, "mp4");
        let numbered = fresh.replace(".mp4", "_001.mp4");
        std::fs::write(case_dir.join("artifacts/proxies").join(&numbered), b"p").unwrap();
        assert_eq!(
            super::find_existing_proxy(&case_dir, &selector),
            Some(case_dir.join("artifacts/proxies").join(&numbered)),
            "a path-free name wins over a legacy one"
        );
        // A carved output is filed under its carve id.
        std::fs::create_dir_all(case_dir.join("artifacts/carved")).unwrap();
        // R9 P3: a real (chained) lane — lane rows come through
        // `audit::read_lane_rows`, which trusts nothing past a chain break.
        crate::audit::append_chained_jsonl_keyed(
            &case_dir.join("artifacts/carved/carve-log.jsonl"),
            &serde_json::json!({"id": "carve_7", "output_path": selector}).to_string(),
            None,
        )
        .unwrap();
        assert_eq!(
            super::artifact_record_id(&case_dir, &selector, &source),
            "carve_7"
        );
        let _ = std::fs::remove_dir_all(&case_dir);
    }

    #[test]
    fn builds_artifact_command_args() {
        let proxy_args = proxy_ffmpeg_args(
            Path::new("in.mp4"),
            Path::new("proxy.mp4"),
            &ProxyOptions::default(),
            &crate::ffmpeg_encoders::CodecChoice::legacy_h264(),
        );
        assert!(proxy_args.contains(&"libx264".to_string()));
        assert_eq!(proxy_args.last().map(String::as_str), Some("proxy.mp4"));

        let thumb_args = thumbnail_ffmpeg_args(
            Path::new("in.mp4"),
            Path::new("thumb.jpg"),
            &ThumbnailOptions::default(),
        );
        assert!(thumb_args.contains(&"-frames:v".to_string()));
        assert_eq!(thumb_args.last().map(String::as_str), Some("thumb.jpg"));
    }

    /// LGPL-like fake ffmpeg (no libx264, has h264_mf and aac) → h264_mf;
    /// a full build keeps libx264; a build with no H.264 → mpeg4.
    #[test]
    fn proxy_args_follow_detected_encoders() {
        use crate::ffmpeg_encoders::{self as enc, tests as fixtures};
        let dir = std::env::temp_dir().join(format!("ft-proxy-enc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let build = |name: &str, listing: &str| {
            let encoders = enc::for_binary(&fixtures::fake_ffmpeg(&dir.join(name), listing));
            assert!(encoders.probed());
            let codecs = encoders.choice(encoders.video(), encoders.aac());
            proxy_ffmpeg_args(
                Path::new("in.mp4"),
                Path::new("proxy.mp4"),
                &ProxyOptions::default(),
                &codecs,
            )
        };
        let lgpl = build("lgpl", fixtures::LGPL);
        assert!(
            lgpl.windows(2).any(|w| w == ["-c:v", "h264_mf"]),
            "{lgpl:?}"
        );
        assert!(!lgpl.contains(&"libx264".to_string()));
        assert!(!lgpl.contains(&"-crf".to_string()));
        assert!(lgpl.windows(2).any(|w| w == ["-c:a", "aac"]));
        let full = build("full", fixtures::FULL);
        assert_eq!(
            full,
            proxy_ffmpeg_args(
                Path::new("in.mp4"),
                Path::new("proxy.mp4"),
                &ProxyOptions::default(),
                &enc::CodecChoice::legacy_h264(),
            ),
            "a full build keeps the historical libx264 command"
        );
        let bare = build("bare", fixtures::BARE);
        assert!(bare.windows(2).any(|w| w == ["-c:v", "mpeg4"]), "{bare:?}");
        assert_eq!(bare.last().map(String::as_str), Some("proxy.mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
