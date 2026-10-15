use crate::audit;
use crate::tool_policy::{command_version, require_case_output_path, resolve_tool_binary};
use crate::util::{canonicalize_display, json_escape, now_unix, unique_path, write_text};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct E01Options {
    pub output_path: Option<PathBuf>,
    pub max_bytes: Option<u64>,
    pub skip_verify: bool,
    pub hash_e01: bool,
    pub ewfinfo_bin: String,
    pub ewfverify_bin: String,
    pub ewfexport_bin: String,
    pub timeout_secs: Option<u64>,
}

impl Default for E01Options {
    fn default() -> Self {
        Self {
            output_path: None,
            max_bytes: None,
            skip_verify: false,
            hash_e01: false,
            ewfinfo_bin: "ewfinfo".to_string(),
            ewfverify_bin: "ewfverify".to_string(),
            ewfexport_bin: "ewfexport".to_string(),
            timeout_secs: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct E01ImportResult {
    pub e01_path: PathBuf,
    pub raw_output_path: PathBuf,
    pub ewfinfo_log_path: PathBuf,
    pub ewfverify_log_path: Option<PathBuf>,
    pub ewfexport_log_path: PathBuf,
    pub raw_sha256: String,
    pub e01_sha256: Option<String>,
    pub is_corrupted: bool,
}

pub fn inspect_e01(case_dir: &Path, e01_path: &Path, options: &E01Options) -> Result<bool, String> {
    let e01_path = canonical_e01_path(e01_path)?;
    let inspected_unix = now_unix()?;
    // ewfinfo reads every segment header, so its runtime scales with
    // segment count — a 60-segment set already exceeds the 120s probe
    // default on a slow disk. Respect --timeout; default to unlimited,
    // matching ewfverify/ewfexport.
    let info = run_capture(
        &options.ewfinfo_bin,
        &["ewfinfo"],
        &["-f", "text", &audit::path_string(&e01_path)],
        options.timeout_secs,
    )?;
    let info_log_path = unique_path(
        &case_dir
            .join("evidence/logs")
            .join(format!("e01-info-{inspected_unix}.txt")),
    );
    write_text(&info_log_path, &info.stdout)
        .map_err(|err| format!("failed to write E01 info log: {err}"))?;

    let is_corrupted = ewfinfo_is_corrupted(&info.stdout);
    let e01_sha256 = if options.hash_e01 {
        Some(audit::digest_file(&e01_path)?)
    } else {
        None
    };
    append_e01_audit(
        case_dir,
        &format!(
            "{{\"schema_version\":1,\"event\":\"inspect-e01\",\"inspected_unix\":{},\"e01_path\":\"{}\",\"e01_sha256\":{},\"is_corrupted\":{},\"ewfinfo_version\":\"{}\",\"ewfinfo_log_path\":\"{}\"}}",
            inspected_unix,
            json_escape(&e01_path.to_string_lossy()),
            audit::optional_string(e01_sha256.as_deref()),
            is_corrupted,
            json_escape(&ewf_command_version(&options.ewfinfo_bin, &["ewfinfo"])),
            json_escape(&info_log_path.to_string_lossy())
        ),
    )?;
    Ok(is_corrupted)
}

pub fn import_e01(
    case_dir: &Path,
    e01_path: &Path,
    options: &E01Options,
) -> Result<E01ImportResult, String> {
    let e01_path = canonical_e01_path(e01_path)?;
    let imported_unix = now_unix()?;

    // Same scaling issue as inspect_e01: ewfinfo reads every segment
    // header, so the fixed probe timeout is wrong for large segment sets.
    let info = run_capture(
        &options.ewfinfo_bin,
        &["ewfinfo"],
        &["-f", "text", &audit::path_string(&e01_path)],
        options.timeout_secs,
    )?;
    let info_log_path = unique_path(
        &case_dir
            .join("evidence/logs")
            .join(format!("e01-info-{imported_unix}.txt")),
    );
    write_text(&info_log_path, &info.stdout)
        .map_err(|err| format!("failed to write E01 info log: {err}"))?;
    let is_corrupted = ewfinfo_is_corrupted(&info.stdout);
    if is_corrupted {
        // Not fatal: a damaged set may still export a partial image, and
        // ewfverify (when not skipped) does its own integrity check. But
        // the flag must be surfaced and audited so a damaged source is
        // never reported as clean.
        eprintln!(
            "warning: ewfinfo reports this EWF set as corrupted or incomplete (missing segments?); results may be partial"
        );
    }

    let verify_log_path = if options.skip_verify {
        None
    } else {
        let path = unique_path(
            &case_dir
                .join("evidence/logs")
                .join(format!("e01-verify-{imported_unix}.txt")),
        );
        let args = vec![
            "-q".to_string(),
            "-d".to_string(),
            "sha256".to_string(),
            "-l".to_string(),
            audit::path_string(&path),
            audit::path_string(&e01_path),
        ];
        run_status(
            &options.ewfverify_bin,
            &["ewfverify"],
            &args,
            options.timeout_secs,
        )?;
        Some(path)
    };

    let requested_raw_path = options.output_path.clone().unwrap_or_else(|| {
        case_dir
            .join("evidence/images")
            .join(default_raw_filename(&e01_path))
    });
    require_case_output_path(case_dir, &requested_raw_path, "E01 raw")?;
    if requested_raw_path.exists() {
        return Err(format!(
            "output already exists: {} (choose a new --output path)",
            requested_raw_path.display()
        ));
    }
    // Disk preflight: the raw image is the source media's full byte size.
    // ewfinfo reports it as "Media size  ... (N bytes)"; when the field is
    // absent, fall back to the (compressed) E01 size as a lower bound so a
    // clearly-insufficient volume still fails before ewfexport runs.
    let e01_size = std::fs::metadata(&e01_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let expected_raw = ewfinfo_media_size(&info.stdout)
        .unwrap_or(e01_size)
        .max(e01_size);
    // --max-bytes caps the export: only require what will actually be
    // written, not the whole media size.
    let expected_raw = match options.max_bytes {
        Some(cap) => expected_raw.min(cap),
        None => expected_raw,
    };
    crate::diskspace::ensure_available(&requested_raw_path, expected_raw, "import-e01")?;
    if let Some(parent) = requested_raw_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create E01 output directory: {err}"))?;
    }
    // C1: ewfexport writes a pid-tagged in-progress image
    // (`.<name>.ft<pid>.part` + the `.raw` ewfexport always appends); only
    // a complete, hashed export is renamed to the final name, and only
    // then is the completion marker written. A killed import can never
    // leave a torn image under `evidence.raw` for a later run to reuse or
    // carve.
    if let Some(parent) = requested_raw_path.parent() {
        for (path, result) in crate::join::sweep_stale_parts(parent) {
            if let Err(err) = result {
                eprintln!("warning: {err} ({})", path.display());
            }
        }
    }
    let export_target = crate::join::pid_part_path(&requested_raw_path, std::process::id());
    let generated_raw_path = expected_ewfexport_output(&export_target);
    crate::join::remove_file_retry(&generated_raw_path)?;
    let remove_part = || {
        let _ = crate::join::remove_file_retry(&generated_raw_path);
    };

    let export_log_path = unique_path(
        &case_dir
            .join("evidence/logs")
            .join(format!("e01-export-{imported_unix}.txt")),
    );
    let export_args = ewfexport_args(&e01_path, &export_target, options, &export_log_path);
    if let Err(error) = run_status(
        &options.ewfexport_bin,
        &["ewfexport"],
        &export_args,
        options.timeout_secs,
    ) {
        // ewfexport can leave a partial raw behind when it is killed or fails;
        // never let a torn image masquerade as a completed export.
        remove_part();
        return Err(error);
    }
    let generated_raw_path = match resolve_ewfexport_output(&generated_raw_path) {
        Ok(path) => path,
        Err(err) => {
            remove_part();
            return Err(err);
        }
    };
    let raw_sha256 = match audit::digest_file(&generated_raw_path) {
        Ok(digest) => digest,
        Err(err) => {
            remove_part();
            return Err(err);
        }
    };
    match crate::join::publish_part_as(&generated_raw_path, &requested_raw_path) {
        Ok(true) => {}
        Ok(false) => {
            remove_part();
            return Err(format!(
                "output already exists: {} (choose a new --output path)",
                requested_raw_path.display()
            ));
        }
        Err(err) => {
            remove_part();
            return Err(err);
        }
    }
    let raw_output_path = canonicalize_display(&requested_raw_path)
        .map_err(|err| format!("failed to canonicalize raw E01 output: {err}"))?;
    let e01_sha256 = if options.hash_e01 {
        Some(audit::digest_file(&e01_path)?)
    } else {
        None
    };

    append_e01_audit(
        case_dir,
        &format!(
            "{{\"schema_version\":1,\"event\":\"import-e01\",\"imported_unix\":{},\"e01_path\":\"{}\",\"e01_sha256\":{},\"raw_output_path\":\"{}\",\"raw_sha256\":\"{}\",\"max_bytes\":{},\"verified\":{},\"is_corrupted\":{},\"ewfinfo_version\":\"{}\",\"ewfverify_version\":\"{}\",\"ewfexport_version\":\"{}\",\"ewfinfo_log_path\":\"{}\",\"ewfverify_log_path\":{},\"ewfexport_log_path\":\"{}\",\"command\":\"{}\",\"command_args\":{}}}",
            imported_unix,
            json_escape(&e01_path.to_string_lossy()),
            audit::optional_string(e01_sha256.as_deref()),
            json_escape(&raw_output_path.to_string_lossy()),
            json_escape(&raw_sha256),
            options
                .max_bytes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".to_string()),
            !options.skip_verify,
            is_corrupted,
            json_escape(&ewf_command_version(&options.ewfinfo_bin, &["ewfinfo"])),
            json_escape(&ewf_command_version(&options.ewfverify_bin, &["ewfverify"])),
            json_escape(&ewf_command_version(&options.ewfexport_bin, &["ewfexport"])),
            json_escape(&info_log_path.to_string_lossy()),
            audit::optional_string(
                verify_log_path
                    .as_ref()
                    .map(|path| path.to_string_lossy())
                    .as_deref()
            ),
            json_escape(&export_log_path.to_string_lossy()),
            json_escape(&options.ewfexport_bin),
            audit::json_string_array(&export_args)
        ),
    )?;
    // C1: written last (after the audit entry), and its failure is the
    // import's failure — a raw without a matching marker is never reused
    // or carved.
    write_raw_done_marker(&raw_output_path, &raw_sha256, &e01_path, imported_unix)?;

    Ok(E01ImportResult {
        e01_path,
        raw_output_path,
        ewfinfo_log_path: info_log_path,
        ewfverify_log_path: verify_log_path,
        ewfexport_log_path: export_log_path,
        raw_sha256,
        e01_sha256,
        is_corrupted,
    })
}

/// C1: the completion marker beside an imported raw image
/// (`<raw>.done.json`).
pub fn raw_done_marker_path(raw: &Path) -> PathBuf {
    let mut name = raw.as_os_str().to_os_string();
    name.push(".done.json");
    PathBuf::from(name)
}

/// The legacy server-side binding (`evidence.raw.source`, the E01 path
/// only — written by servers before C1).
fn legacy_binding_path(raw: &Path) -> PathBuf {
    raw.with_extension("raw.source")
}

fn modified_nanos(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
}

fn write_raw_done_marker(
    raw: &Path,
    raw_sha256: &str,
    e01_path: &Path,
    imported_unix: u64,
) -> Result<(), String> {
    let meta = std::fs::metadata(raw)
        .map_err(|err| format!("failed to stat imported raw {}: {err}", raw.display()))?;
    let body = serde_json::json!({
        "schema_version": 1,
        "kind": "frametrace-e01-raw-complete",
        "raw_path": raw.to_string_lossy(),
        "size_bytes": meta.len(),
        "modified_nanos": modified_nanos(&meta),
        "sha256": raw_sha256,
        "e01_path": e01_path.to_string_lossy(),
        "e01_size_bytes": std::fs::metadata(e01_path).map(|m| m.len()).ok(),
        "imported_unix": imported_unix,
    });
    crate::util::write_text_atomic(&raw_done_marker_path(raw), &format!("{body:#}\n")).map_err(
        |err| {
            format!(
                "failed to write the import completion marker for {} — the raw image will not be reused or carved: {err}",
                raw.display()
            )
        },
    )
}

/// C1: whether an imported raw image is known complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawImportStatus {
    /// No raw image at that path.
    Missing,
    /// The completion marker matches the file (size + modification time).
    Complete { e01_path: PathBuf, sha256: String },
    /// Present but not provably complete: no marker (a killed import, or
    /// one made before markers existed) or a marker that no longer matches.
    Unverified(String),
}

/// Reads `raw`'s completion marker and compares it with the file.
pub fn raw_import_status(raw: &Path) -> RawImportStatus {
    let Ok(meta) = std::fs::metadata(raw) else {
        return RawImportStatus::Missing;
    };
    let marker_path = raw_done_marker_path(raw);
    let text = match std::fs::read_to_string(&marker_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return RawImportStatus::Unverified(
                "완료 표식이 없습니다 (중단된 임포트의 부분 이미지일 수 있음)".to_string(),
            );
        }
        Err(err) => {
            return RawImportStatus::Unverified(format!("완료 표식을 읽지 못했습니다: {err}"));
        }
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return RawImportStatus::Unverified("완료 표식이 손상되었습니다".to_string());
    };
    let size = value.get("size_bytes").and_then(serde_json::Value::as_u64);
    let nanos = value
        .get("modified_nanos")
        .and_then(serde_json::Value::as_u64);
    if size != Some(meta.len()) || nanos.is_none() || nanos != modified_nanos(&meta) {
        return RawImportStatus::Unverified(
            "raw 이미지가 완료 표식과 다릅니다 (크기 또는 수정 시각)".to_string(),
        );
    }
    match (
        value.get("e01_path").and_then(serde_json::Value::as_str),
        value.get("sha256").and_then(serde_json::Value::as_str),
    ) {
        (Some(e01), Some(sha)) if !e01.is_empty() && !sha.is_empty() => RawImportStatus::Complete {
            e01_path: PathBuf::from(e01),
            sha256: sha.to_string(),
        },
        _ => RawImportStatus::Unverified("완료 표식에 원본 E01·해시가 없습니다".to_string()),
    }
}

/// C1: what the workstation does with an existing `raw` before importing
/// `source` into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawReuse {
    /// Nothing there: import.
    Absent,
    /// Complete import of this very E01: reuse it.
    Reuse,
    /// Unverifiable, changed or from another E01. R5-S6: never discarded
    /// automatically — the examiner decides (`/api/reimport-raw`); the
    /// text says why.
    NeedsReimport(String),
}

/// R5-S6: the newest SHA-256 (and E01 path, import time) the case's
/// chained E01 audit lane recorded for a raw image named like `raw` —
/// the evidence an old (pre-marker) or moved case still has.
pub fn recorded_raw_import(case_dir: &Path, raw: &Path) -> Option<(String, PathBuf, u64)> {
    let name = raw.file_name()?.to_string_lossy().to_string();
    let text = crate::util::read_to_string(&case_dir.join("evidence/logs/e01-audit.jsonl")).ok()?;
    text.lines().rev().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        if value.get("event").and_then(serde_json::Value::as_str) != Some("import-e01") {
            return None;
        }
        let logged = value.get("raw_output_path")?.as_str()?;
        let logged_name = logged.rsplit(['/', '\\']).next().unwrap_or(logged);
        if logged_name != name {
            return None;
        }
        // A capped (--max-bytes) export is not the whole image.
        if value.get("max_bytes").is_some_and(|v| !v.is_null()) {
            return None;
        }
        let sha = value.get("raw_sha256")?.as_str()?.to_ascii_lowercase();
        (sha.len() == 64).then(|| {
            (
                sha,
                PathBuf::from(value.get("e01_path").and_then(|v| v.as_str()).unwrap_or("")),
                value
                    .get("imported_unix")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            )
        })
    })
}

/// R5-S6: [`raw_reuse_decision`] that re-verifies instead of giving up.
/// A raw image whose completion marker no longer matches (a moved or
/// copied case: new modification time) or that predates markers is
/// re-hashed against the SHA-256 the marker or the E01 audit lane
/// recorded; when it matches, the marker is rewritten and the image is
/// judged again (same E01 → reuse). Nothing is ever deleted here: a
/// mismatch or a missing record is [`RawReuse::NeedsReimport`] for the
/// examiner to decide. `on_rehash` is told before the (possibly long)
/// re-hash starts. Runs outside any server lock (file hashing).
pub fn reconcile_raw(
    case_dir: &Path,
    raw: &Path,
    source: &Path,
    on_rehash: &dyn Fn(&str),
) -> RawReuse {
    let first = raw_reuse_decision(raw, source);
    let RawImportStatus::Unverified(reason) = raw_import_status(raw) else {
        return first;
    };
    let from_marker = std::fs::read_to_string(raw_done_marker_path(raw))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| {
            let sha = value.get("sha256")?.as_str()?.to_ascii_lowercase();
            let e01 = value.get("e01_path")?.as_str()?.to_string();
            let unix = value
                .get("imported_unix")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            (sha.len() == 64 && !e01.is_empty()).then(|| (sha, PathBuf::from(e01), unix))
        });
    let Some((recorded_sha, e01_path, imported_unix)) =
        from_marker.or_else(|| recorded_raw_import(case_dir, raw))
    else {
        return RawReuse::NeedsReimport(format!(
            "기존 raw 이미지를 완료된 임포트로 확인할 수 없습니다 — {reason}; 비교할 SHA-256 기록도 없습니다"
        ));
    };
    on_rehash(&recorded_sha);
    let current = match audit::digest_file(raw) {
        Ok(digest) => digest.to_ascii_lowercase(),
        Err(err) => {
            return RawReuse::NeedsReimport(format!(
                "기존 raw 이미지의 SHA-256을 다시 계산하지 못했습니다: {err}"
            ));
        }
    };
    if current != recorded_sha {
        return RawReuse::NeedsReimport(format!(
            "raw 이미지의 SHA-256이 임포트 기록과 다릅니다 (기록 {recorded_sha}, 현재 {current})"
        ));
    }
    let e01_for_marker = if e01_path.as_os_str().is_empty() {
        source.to_path_buf()
    } else {
        e01_path
    };
    if let Err(err) = write_raw_done_marker(raw, &recorded_sha, &e01_for_marker, imported_unix) {
        return RawReuse::NeedsReimport(format!(
            "raw 이미지는 기록과 같지만 완료 표식을 갱신하지 못했습니다: {err}"
        ));
    }
    raw_reuse_decision(raw, source)
}

/// Pure decision on top of [`raw_import_status`]. The E01 paths are
/// compared canonically when possible.
pub fn raw_reuse_decision(raw: &Path, source: &Path) -> RawReuse {
    let same = |a: &Path, b: &Path| {
        let a = canonicalize_display(a).unwrap_or_else(|_| a.to_path_buf());
        let b = canonicalize_display(b).unwrap_or_else(|_| b.to_path_buf());
        a == b
    };
    match raw_import_status(raw) {
        RawImportStatus::Missing => RawReuse::Absent,
        RawImportStatus::Complete { e01_path, .. } if same(&e01_path, source) => RawReuse::Reuse,
        RawImportStatus::Complete { e01_path, .. } => RawReuse::NeedsReimport(format!(
            "기존 raw 이미지는 다른 E01({})에서 임포트된 것입니다",
            e01_path.display()
        )),
        RawImportStatus::Unverified(reason) => RawReuse::NeedsReimport(format!(
            "기존 raw 이미지를 완료된 임포트로 확인할 수 없습니다 — {reason}"
        )),
    }
}

/// C1: removes a raw image that will be re-imported, with its marker and
/// the legacy binding file. Any failure is returned (the caller fails the
/// step rather than importing next to stale bytes).
pub fn discard_raw(raw: &Path) -> Result<(), String> {
    for path in [
        raw.to_path_buf(),
        raw_done_marker_path(raw),
        legacy_binding_path(raw),
    ] {
        crate::join::remove_file_retry(&path)?;
    }
    Ok(())
}

/// C1: carving / analysis of a raw image inside the case's
/// `evidence/images` (where imports land) requires a matching completion
/// marker; images elsewhere are the examiner's own and are not judged.
pub fn require_complete_case_raw(case_dir: &Path, source: &Path) -> Result<(), String> {
    let images = case_dir.join("evidence/images");
    let (Ok(images), Ok(canonical)) = (canonicalize_display(&images), canonicalize_display(source))
    else {
        return Ok(());
    };
    if !canonical.starts_with(&images) {
        return Ok(());
    }
    match raw_import_status(&canonical) {
        RawImportStatus::Complete { .. } | RawImportStatus::Missing => Ok(()),
        RawImportStatus::Unverified(reason) => Err(format!(
            "{}: {reason}. 완료가 확인되지 않은 raw 이미지는 카빙하지 않습니다 — import-e01을 다시 실행하십시오 (refusing to carve a raw image without a matching completion marker; re-run import-e01)",
            canonical.display()
        )),
    }
}

/// Parses the media byte size out of `ewfinfo -f text` output, whose
/// `Media size` line ends in e.g. `500.1 GB (500107862016 bytes)`.
/// Returns `None` when the field is missing or not parseable — callers
/// then fall back to the E01 file size as a lower bound.
fn ewfinfo_media_size(stdout: &str) -> Option<u64> {
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("Media size"))?;
    let open = line.rfind('(')?;
    let close = line.rfind(')')?;
    if close <= open {
        return None;
    }
    let inside = &line[open + 1..close];
    let digits: String = inside.chars().filter(|ch| ch.is_ascii_digit()).collect();
    digits.parse::<u64>().ok().filter(|value| *value > 0)
}

/// Parses the `Is corrupted` flag from `ewfinfo -f text` output. libewf
/// sets it for incomplete segment sets and damaged data; the info log
/// records it, but callers must also surface it so a damaged source is
/// never reported as clean.
fn ewfinfo_is_corrupted(stdout: &str) -> bool {
    stdout
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("Is corrupted"))
        .any(|rest| rest.rsplit(':').next().map(str::trim) == Some("yes"))
}

fn canonical_e01_path(path: &Path) -> Result<PathBuf, String> {
    let path = canonicalize_display(path)
        .map_err(|err| format!("failed to canonicalize E01 path: {err}"))?;
    if !path.is_file() {
        return Err(format!("E01 path is not a file: {}", path.display()));
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    if !extension.eq_ignore_ascii_case("e01")
        && !extension.eq_ignore_ascii_case("ex01")
        && !extension.eq_ignore_ascii_case("s01")
        && !extension.eq_ignore_ascii_case("l01")
    {
        return Err(format!(
            "unsupported EWF extension .{} (expected E01/Ex01/S01/L01 first segment)",
            extension
        ));
    }
    // libewf treats the final path argument as a segment GLOB, so evidence
    // filenames (seized media is attacker-controlled) containing `[`, `]`,
    // `?`, or `*` can select a different segment set or silently match
    // nothing — the wrong bytes would then be hashed and recorded as
    // verified. Refuse instead of guessing.
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if ['[', ']', '?', '*']
        .iter()
        .any(|meta| file_name.contains(*meta))
    {
        return Err(format!(
            "E01 file name contains glob metacharacters libewf would expand ({file_name}); rename the segment files without '[', ']', '?', '*' before importing"
        ));
    }
    Ok(path)
}

fn default_raw_filename(e01_path: &Path) -> String {
    let stem = e01_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("e01-export");
    format!("{stem}.raw")
}

fn ewfexport_args(
    e01_path: &Path,
    export_target: &Path,
    options: &E01Options,
    export_log_path: &Path,
) -> Vec<String> {
    let mut args = vec![
        "-u".to_string(),
        "-q".to_string(),
        "-f".to_string(),
        "raw".to_string(),
        "-l".to_string(),
        audit::path_string(export_log_path),
    ];
    if let Some(max_bytes) = options.max_bytes {
        args.push("-B".to_string());
        args.push(max_bytes.to_string());
    }
    args.extend([
        "-t".to_string(),
        // ewfexport's glob fails silently (exit 0, no output) on paths with
        // mixed separators like "case\evidence/images"; normalize to the
        // platform separator before handing the target over.
        audit::path_string(export_target).replace('/', std::path::MAIN_SEPARATOR_STR),
        audit::path_string(e01_path),
    ]);
    args
}

/// ewfexport's -t is a *prefix*: it always appends ".raw". We only strip a
/// literal ".raw" suffix — never a generic extension — because a basename
/// like "4. 한주연 HDD.raw" contains interior dots that `with_extension`
/// would wrongly treat as the extension boundary (producing "4.raw").
#[cfg(test)]
fn ewfexport_target_for_output(raw_path: &Path) -> PathBuf {
    match raw_path.file_name().and_then(|n| n.to_str()) {
        Some(name) if name.to_ascii_lowercase().ends_with(".raw") => {
            raw_path.with_file_name(&name[..name.len() - 4])
        }
        _ => raw_path.to_path_buf(),
    }
}

fn expected_ewfexport_output(export_target: &Path) -> PathBuf {
    let mut text = export_target.to_string_lossy().into_owned();
    text.push_str(".raw");
    PathBuf::from(text)
}

fn resolve_ewfexport_output(expected: &Path) -> Result<PathBuf, String> {
    if expected.is_file() {
        return expected
            .canonicalize()
            .map_err(|err| format!("failed to canonicalize raw E01 output: {err}"));
    }

    Err(format!(
        "ewfexport completed but output file was not found at {}",
        expected.display()
    ))
}

fn run_capture(
    binary: &str,
    allowed: &[&str],
    args: &[&str],
    timeout_secs: Option<u64>,
) -> Result<CommandOutput, String> {
    let resolved_binary = resolve_tool_binary(binary, allowed)
        .map_err(|err| format!("{err} (install libewf tools and ensure {binary} is in PATH)"))?;
    let mut command = Command::new(&resolved_binary);
    command.args(args);
    let output = crate::util::run_with_timeout(&mut command, timeout_secs).map_err(|err| {
        format!(
            "failed to run {binary}: {err} (install libewf tools and ensure {binary} is in PATH)"
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "{binary} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
    })
}

fn run_status(
    binary: &str,
    allowed: &[&str],
    args: &[String],
    timeout_secs: Option<u64>,
) -> Result<(), String> {
    let resolved_binary = resolve_tool_binary(binary, allowed)
        .map_err(|err| format!("{err} (install libewf tools and ensure {binary} is in PATH)"))?;
    let mut command = Command::new(&resolved_binary);
    command.args(args);
    let output = crate::util::run_with_timeout(&mut command, timeout_secs).map_err(|err| {
        format!(
            "failed to run {binary}: {err} (install libewf tools and ensure {binary} is in PATH)"
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "{binary} failed: {}{}",
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        ));
    }
    Ok(())
}

fn ewf_command_version(binary: &str, allowed: &[&str]) -> String {
    command_version(binary, allowed, "-V")
}

fn append_e01_audit(case_dir: &Path, body_json: &str) -> Result<(), String> {
    audit::append_chained_jsonl(&case_dir.join("evidence/logs/e01-audit.jsonl"), body_json)
}

struct CommandOutput {
    stdout: String,
}

#[cfg(test)]
mod tests {
    use super::{
        E01Options, canonical_e01_path, default_raw_filename, ewf_command_version, ewfexport_args,
        ewfexport_target_for_output, ewfinfo_is_corrupted, ewfinfo_media_size,
        expected_ewfexport_output, resolve_ewfexport_output,
    };
    use std::fs;
    use std::path::Path;

    #[test]
    fn rejects_e01_filenames_with_glob_metacharacters() {
        // libewf expands the final argument as a segment glob; an evidence
        // name like x[abc].E01 would silently select the wrong segment set.
        // ('*' and '?' cannot exist in a Windows filename at all, so the
        // fixture uses bracket metacharacters which are legal everywhere.)
        let dir = std::env::temp_dir().join(format!("ft-e01-glob-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let evil = dir.join("x[abc].E01");
        fs::write(&evil, b"segment").unwrap();
        let err = canonical_e01_path(&evil).unwrap_err();
        assert!(err.contains("glob metacharacters"), "{err}");

        let plain = dir.join("blackbox.E01");
        fs::write(&plain, b"segment").unwrap();
        let resolved = canonical_e01_path(&plain).unwrap();
        assert!(resolved.ends_with("blackbox.E01"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_media_size_from_ewfinfo_text() {
        let sample = "ewfinfo 20201230\n\nAcquiry information\n\tMedia size\t\t\t500.1 GB (500107862016 bytes)\n\tBytes per sector\t512\n";
        assert_eq!(ewfinfo_media_size(sample), Some(500107862016));
        assert_eq!(ewfinfo_media_size("no media line at all"), None);
        assert_eq!(
            ewfinfo_media_size("\tMedia size\t\t\t1.0 KiB (1024 bytes)"),
            Some(1024)
        );
        // Zero or garbled values are rejected so callers take the fallback.
        assert_eq!(ewfinfo_media_size("Media size 0 (0 bytes)"), None);
        assert_eq!(ewfinfo_media_size("Media size ???"), None);
    }

    #[test]
    fn detects_corrupted_flag_in_ewfinfo_text() {
        // libewf emits this for incomplete segment sets; the flag must not
        // be lost between the info log and the audit record.
        let damaged = "ewfinfo 20240506\n\nEWF information:\n\tFile format:\t\tEnCase 7\n\tIs corrupted:\t\tyes\n\nMedia information:\n\tMedia size:\t\t223 GiB (240057409536 bytes)\n";
        assert!(ewfinfo_is_corrupted(damaged));
        let clean = "ewfinfo 20240506\n\nEWF information:\n\tFile format:\t\tEnCase 7\n\nMedia information:\n\tMedia size:\t\t223 GiB (240057409536 bytes)\n";
        assert!(!ewfinfo_is_corrupted(clean));
        assert!(!ewfinfo_is_corrupted("\tIs corrupted:\t\tno\n"));
        assert!(!ewfinfo_is_corrupted("no such field"));
    }

    #[test]
    fn builds_default_raw_filename() {
        assert_eq!(
            default_raw_filename(Path::new("blackbox.E01")),
            "blackbox.raw"
        );
    }

    #[test]
    fn builds_ewfexport_args() {
        let options = E01Options {
            max_bytes: Some(1024),
            ..E01Options::default()
        };
        let args = ewfexport_args(
            Path::new("input.E01"),
            Path::new("output"),
            &options,
            Path::new("export.log"),
        );
        assert!(args.contains(&"-u".to_string()));
        assert!(args.contains(&"-B".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("input.E01"));
    }

    #[test]
    fn maps_requested_raw_path_to_ewfexport_target() {
        assert_eq!(
            ewfexport_target_for_output(Path::new("output.raw")),
            Path::new("output")
        );
        assert_eq!(
            expected_ewfexport_output(Path::new("output")),
            Path::new("output.raw")
        );
        // Basenames with interior dots must round-trip as prefix + ".raw",
        // not collapse at the first dot (real-world case: "4. 한주연 HDD.E01").
        assert_eq!(
            ewfexport_target_for_output(Path::new("dir/4. 한주연 HDD.raw")),
            Path::new("dir/4. 한주연 HDD")
        );
        assert_eq!(
            expected_ewfexport_output(Path::new("dir/4. 한주연 HDD")),
            Path::new("dir/4. 한주연 HDD.raw")
        );
        assert_eq!(
            expected_ewfexport_output(&ewfexport_target_for_output(Path::new(
                "dir/4. 한주연 HDD.raw"
            ))),
            Path::new("dir/4. 한주연 HDD.raw")
        );
    }

    #[test]
    fn resolves_requested_ewfexport_output() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-e01-output-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.raw");
        fs::write(&path, b"raw").unwrap();
        assert_eq!(
            resolve_ewfexport_output(&path).unwrap(),
            path.canonicalize().unwrap()
        );
        let _ = fs::remove_dir_all(dir);
    }

    fn c1_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ft-c1-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("evidence/images")).unwrap();
        dir
    }

    /// C1: a raw image is reusable only when its completion marker matches
    /// the file and names the same E01; a torn / pre-marker / changed /
    /// foreign image is re-imported. The old binding (`.raw.source`, the
    /// E01 path only) reused any bytes under the name.
    #[test]
    fn raw_completion_marker_decides_reuse() {
        use super::{
            RawImportStatus, RawReuse, discard_raw, raw_done_marker_path, raw_import_status,
            raw_reuse_decision, write_raw_done_marker,
        };
        let dir = c1_dir("reuse");
        let e01 = dir.join("disk.E01");
        let other = dir.join("other.E01");
        fs::write(&e01, b"ewf").unwrap();
        fs::write(&other, b"ewf2").unwrap();
        let raw = dir.join("evidence/images/evidence.raw");
        assert_eq!(raw_import_status(&raw), RawImportStatus::Missing);
        assert_eq!(raw_reuse_decision(&raw, &e01), RawReuse::Absent);
        // A torn image from a killed import (no marker) — even with the
        // legacy binding naming this very E01.
        fs::write(&raw, b"torn").unwrap();
        fs::write(
            raw.with_extension("raw.source"),
            e01.to_string_lossy().as_bytes(),
        )
        .unwrap();
        assert!(matches!(
            raw_import_status(&raw),
            RawImportStatus::Unverified(_)
        ));
        assert!(matches!(
            raw_reuse_decision(&raw, &e01),
            RawReuse::NeedsReimport(_)
        ));
        // Completed import.
        fs::write(&raw, b"complete-image").unwrap();
        write_raw_done_marker(&raw, "ab".repeat(32).as_str(), &e01, 1).unwrap();
        assert!(matches!(
            raw_import_status(&raw),
            RawImportStatus::Complete { .. }
        ));
        assert_eq!(raw_reuse_decision(&raw, &e01), RawReuse::Reuse);
        match raw_reuse_decision(&raw, &other) {
            RawReuse::NeedsReimport(reason) => assert!(reason.contains("다른 E01"), "{reason}"),
            other => panic!("{other:?}"),
        }
        // Bytes changed after the marker.
        fs::write(&raw, b"complete-image-plus").unwrap();
        assert!(matches!(
            raw_reuse_decision(&raw, &e01),
            RawReuse::NeedsReimport(_)
        ));
        discard_raw(&raw).unwrap();
        assert!(!raw.exists());
        assert!(!raw_done_marker_path(&raw).exists());
        assert!(!raw.with_extension("raw.source").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// R5-S6: a moved/copied case (marker mtime no longer matches) or a
    /// pre-marker case is re-hashed against the recorded SHA-256 instead
    /// of being thrown away: a match refreshes the marker and reuses the
    /// image; a mismatch or a missing record asks the examiner — nothing
    /// is deleted either way. Before, every one of these was discarded.
    #[test]
    fn reconcile_rehashes_instead_of_discarding() {
        use super::{RawReuse, raw_done_marker_path, reconcile_raw, write_raw_done_marker};
        let dir = c1_dir("reconcile");
        let e01 = dir.join("disk.E01");
        fs::write(&e01, b"ewf").unwrap();
        let raw = dir.join("evidence/images/evidence.raw");
        fs::write(&raw, b"complete-image").unwrap();
        let sha = crate::audit::digest_file(&raw).unwrap();
        write_raw_done_marker(&raw, &sha, &e01, 7).unwrap();
        // "Moved" case: same bytes, new modification time.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&raw)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let rehashed = std::cell::Cell::new(false);
        let decision = reconcile_raw(&dir, &raw, &e01, &|_| rehashed.set(true));
        assert_eq!(decision, RawReuse::Reuse);
        assert!(rehashed.get());
        assert!(raw.is_file(), "never deleted");
        // The marker was refreshed: the next decision needs no re-hash.
        let rehashed = std::cell::Cell::new(false);
        assert_eq!(
            reconcile_raw(&dir, &raw, &e01, &|_| rehashed.set(true)),
            RawReuse::Reuse
        );
        assert!(!rehashed.get());
        // Changed bytes: the examiner decides; the image stays.
        fs::write(&raw, b"complete-imagX").unwrap();
        match reconcile_raw(&dir, &raw, &e01, &|_| {}) {
            RawReuse::NeedsReimport(reason) => assert!(reason.contains("SHA-256"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(raw.is_file());
        // Pre-marker case: the E01 audit lane's record is used.
        fs::remove_file(raw_done_marker_path(&raw)).unwrap();
        fs::write(&raw, b"old-case-image").unwrap();
        let old_sha = crate::audit::digest_file(&raw).unwrap();
        match reconcile_raw(&dir, &raw, &e01, &|_| {}) {
            RawReuse::NeedsReimport(reason) => assert!(reason.contains("기록"), "{reason}"),
            other => panic!("{other:?}"),
        }
        fs::create_dir_all(dir.join("evidence/logs")).unwrap();
        crate::audit::append_chained_jsonl_keyed(
            &dir.join("evidence/logs/e01-audit.jsonl"),
            &serde_json::json!({
                "schema_version": 1, "event": "import-e01", "imported_unix": 3,
                "e01_path": e01.to_string_lossy(),
                "raw_output_path": "C:/elsewhere/evidence/images/evidence.raw",
                "raw_sha256": old_sha, "max_bytes": null,
            })
            .to_string(),
            None,
        )
        .unwrap();
        assert_eq!(reconcile_raw(&dir, &raw, &e01, &|_| {}), RawReuse::Reuse);
        assert!(raw_done_marker_path(&raw).is_file(), "marker written");
        let _ = fs::remove_dir_all(&dir);
    }

    /// C1: carving an imported image (inside `evidence/images`) requires
    /// its completion marker; the examiner's own images elsewhere are not
    /// judged.
    #[test]
    fn carving_requires_a_completed_case_raw() {
        use super::{require_complete_case_raw, write_raw_done_marker};
        let dir = c1_dir("carve");
        let raw = dir.join("evidence/images/evidence.raw");
        fs::write(&raw, b"torn").unwrap();
        let err = require_complete_case_raw(&dir, &raw).unwrap_err();
        assert!(err.contains("import-e01"), "{err}");
        let e01 = dir.join("disk.E01");
        fs::write(&e01, b"ewf").unwrap();
        write_raw_done_marker(&raw, &"cd".repeat(32), &e01, 1).unwrap();
        require_complete_case_raw(&dir, &raw).unwrap();
        let own = dir.join("my-own.dd");
        fs::write(&own, b"examiner image").unwrap();
        require_complete_case_raw(&dir, &own).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    /// C1 end to end (fake libewf tools): a failing export leaves no image
    /// under the final name and no in-progress file; a completed one is
    /// published with its marker.
    #[cfg(unix)]
    #[test]
    fn import_publishes_only_complete_exports() {
        use super::{E01Options, RawImportStatus, import_e01, raw_import_status};
        use std::os::unix::fs::PermissionsExt;
        let dir = c1_dir("import");
        fs::create_dir_all(dir.join("evidence/logs")).unwrap();
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let script = |name: &str, body: &str| {
            let path = bin.join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path.to_string_lossy().to_string()
        };
        let ewfinfo = script("ewfinfo", "echo 'Media size: 1 KiB (16 bytes)'");
        let ewfverify = script("ewfverify", "exit 0");
        let export_body = |fail: bool| {
            format!(
                "t=''; while [ $# -gt 0 ]; do if [ \"$1\" = -t ]; then t=\"$2\"; shift; fi; shift; done\nprintf 'RAWDATA-RAWDATA!' > \"$t.raw\"\n{}",
                if fail { "exit 1" } else { "exit 0" }
            )
        };
        let e01 = dir.join("disk.E01");
        fs::write(&e01, b"ewf-segment").unwrap();
        let raw = dir.join("evidence/images/evidence.raw");
        let options = |ewfexport: String| E01Options {
            output_path: Some(raw.clone()),
            ewfinfo_bin: ewfinfo.clone(),
            ewfverify_bin: ewfverify.clone(),
            ewfexport_bin: ewfexport,
            ..E01Options::default()
        };
        let failing = script("ewfexport", &export_body(true));
        assert!(import_e01(&dir, &e01, &options(failing)).is_err());
        assert!(
            !raw.exists(),
            "a failed export must not land under the final name"
        );
        let leftovers: Vec<String> = fs::read_dir(dir.join("evidence/images"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let working = script("ewfexport", &export_body(false));
        let result = import_e01(&dir, &e01, &options(working)).unwrap();
        assert!(result.raw_output_path.ends_with("evidence.raw"));
        assert_eq!(fs::read(&raw).unwrap(), b"RAWDATA-RAWDATA!");
        assert!(matches!(
            raw_import_status(&raw),
            RawImportStatus::Complete { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_ewf_version_reports_unavailable() {
        assert!(
            ewf_command_version("frametrace-missing-ewf-binary", &["ewfinfo"])
                .contains("unavailable")
        );
    }
}
