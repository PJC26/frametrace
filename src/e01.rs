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
            "{{\"schema_version\":1,\"event\":\"import-e01\",\"imported_unix\":{},\"e01_path\":\"{}\",\"e01_sha256\":{},\"raw_output_path\":\"{}\",\"raw_sha256\":\"{}\",\"raw_size_bytes\":{},\"max_bytes\":{},\"verified\":{},\"is_corrupted\":{},\"ewfinfo_version\":\"{}\",\"ewfverify_version\":\"{}\",\"ewfexport_version\":\"{}\",\"ewfinfo_log_path\":\"{}\",\"ewfverify_log_path\":{},\"ewfexport_log_path\":\"{}\",\"command\":\"{}\",\"command_args\":{}}}",
            imported_unix,
            json_escape(&e01_path.to_string_lossy()),
            audit::optional_string(e01_sha256.as_deref()),
            json_escape(&raw_output_path.to_string_lossy()),
            json_escape(&raw_sha256),
            std::fs::metadata(&raw_output_path)
                .map(|meta| meta.len().to_string())
                .unwrap_or_else(|_| "null".to_string()),
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
    // or carved. R7-S10: the marker names the audit entry it completes
    // (`audit_entry_sha256`), so a marker cannot be written for bytes no
    // audit entry recorded.
    let audit_entry = import_entry_sha256(case_dir, &raw_sha256, imported_unix);
    write_raw_done_marker_bound(
        &raw_output_path,
        &raw_sha256,
        &e01_path,
        imported_unix,
        audit_entry.as_deref(),
    )?;

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

#[cfg(test)]
fn write_raw_done_marker(
    raw: &Path,
    raw_sha256: &str,
    e01_path: &Path,
    imported_unix: u64,
) -> Result<(), String> {
    write_raw_done_marker_bound(raw, raw_sha256, e01_path, imported_unix, None)
}

/// The E01 audit lane of the case an imported raw image belongs to
/// (`<case>/evidence/images/<raw>` → `<case>/evidence/logs/e01-audit.jsonl`),
/// or `None` for an image outside a case's `evidence/images`.
///
/// R9-I8: at ANY depth below `evidence/images` — the same scope
/// [`require_complete_case_raw`] judges. R8 only looked at direct
/// children, so a raw image in a subfolder of `evidence/images` was judged
/// as a case image but its trust check (lane binding, P0) was skipped.
fn case_e01_lane(raw: &Path) -> Option<PathBuf> {
    raw.ancestors().skip(1).find_map(|images| {
        let evidence = images.parent()?;
        (images.file_name()? == "images" && evidence.file_name()? == "evidence")
            .then(|| evidence.join("logs").join("e01-audit.jsonl"))
    })
}

/// R7-S10: the `entry_sha256` of the newest `import-e01` entry of the
/// case's E01 lane recording `raw_sha256` at `imported_unix` (the entry an
/// import just appended). `None` when it cannot be found.
fn import_entry_sha256(case_dir: &Path, raw_sha256: &str, imported_unix: u64) -> Option<String> {
    // R9 P3: only an entry usable here is bound to.
    let lane = audit::read_lane_rows(&case_dir.join("evidence/logs/e01-audit.jsonl")).ok()?;
    lane.usable().rev().find_map(|row| {
        let value = &row.value;
        (value.get("event").and_then(serde_json::Value::as_str) == Some("import-e01")
            && value.get("raw_sha256").and_then(serde_json::Value::as_str) == Some(raw_sha256)
            && value
                .get("imported_unix")
                .and_then(serde_json::Value::as_u64)
                == Some(imported_unix))
        .then(|| value.get("entry_sha256")?.as_str().map(str::to_string))
        .flatten()
    })
}

/// R7-S10: what the E01 audit lane says about the entry `entry_sha256`:
/// `Ok(())` when the lane verifies, holds that `import-e01` entry with
/// `raw_sha256`, and — with audit keys on this PC — the entry is
/// HMAC-authenticated here.
///
/// R8-I4: the entry must also be an import of THIS image (its
/// `raw_output_path` names this file — compared by file name, so a moved
/// case still matches), of the E01 the marker names (`e01_path` equal), and
/// the NEWEST full import of this image in the lane — a marker bound to a
/// superseded import is not this image's record.
fn check_bound_import_entry(
    lane: &Path,
    raw: &Path,
    entry_sha256: &str,
    raw_sha256: &str,
    marker_e01: &str,
    keys: &[crate::audit_key::AuditKey],
) -> Result<(), String> {
    // R9 P3: the lane's rows through the one lane reader.
    let rows = match audit::read_lane_rows_with_keys(lane, keys) {
        Ok(rows) if !rows.present => {
            return Err("E01 감사 기록(e01-audit.jsonl)이 없습니다".to_string());
        }
        Ok(rows) => rows,
        Err(err) => return Err(format!("E01 감사 기록을 읽지 못했습니다: {err}")),
    };
    if let Some(err) = &rows.failure {
        return Err(format!("E01 감사 기록의 체인 검증 실패: {err}"));
    }
    let Some(verification) = rows.verification.as_ref() else {
        return Err("E01 감사 기록을 확인하지 못했습니다".to_string());
    };
    let name = raw
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let lines: Vec<&str> = rows.rows.iter().map(|row| row.raw.as_str()).collect();
    let newest = newest_full_import_of(&lines, &name).map(|(index, _)| index);
    for row in &rows.rows {
        let (index, value) = (row.index, &row.value);
        if value
            .get("entry_sha256")
            .and_then(serde_json::Value::as_str)
            != Some(entry_sha256)
        {
            continue;
        }
        if value.get("event").and_then(serde_json::Value::as_str) != Some("import-e01")
            || !value
                .get("raw_sha256")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|sha| sha.eq_ignore_ascii_case(raw_sha256))
        {
            return Err(
                "완료 표식이 가리키는 E01 감사 항목이 이 raw 이미지의 임포트 기록이 아닙니다"
                    .to_string(),
            );
        }
        if raw_output_name(value).as_deref() != Some(name.as_str()) {
            return Err(format!(
                "완료 표식이 가리키는 E01 감사 항목은 다른 raw 파일({})의 임포트 기록입니다",
                value
                    .get("raw_output_path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("기록 없음")
            ));
        }
        if value.get("e01_path").and_then(serde_json::Value::as_str) != Some(marker_e01) {
            return Err(
                "완료 표식의 원본 E01과 E01 감사 항목의 원본 E01(e01_path)이 다릅니다".to_string(),
            );
        }
        if newest != Some(index) {
            return Err(
                "완료 표식이 가리키는 E01 감사 항목이 이 raw 이미지의 최신 임포트 기록이 아닙니다 (이후 다시 임포트됨)"
                    .to_string(),
            );
        }
        // R14 I5: a lane-repair marker after it — the cut tail may have
        // held a newer import of this image.
        if rows.repaired_after(index) {
            return Err(E01_REPAIRED_AFTER_NOTE.to_string());
        }
        return import_entry_authenticated(verification, index);
    }
    Err("완료 표식이 가리키는 E01 감사 항목이 감사 기록에 없습니다".to_string())
}

/// R14 I5: the E01 lane was repaired (`lane-repair`) after the newest
/// import of an image — the cut tail may have held a newer one.
pub const E01_REPAIRED_AFTER_NOTE: &str = "E01 감사 기록(e01-audit.jsonl)이 이 이미지의 최신 임포트 항목 뒤에서 수리됨(lane-repair) — 잘린 꼬리에 더 새 임포트가 있었을 수 있어 이 기록이 최신인지 알 수 없습니다 (재검증 필요: 다시 임포트하십시오)";

/// The file name an `import-e01` entry's `raw_output_path` names.
fn raw_output_name(value: &serde_json::Value) -> Option<String> {
    let logged = value.get("raw_output_path")?.as_str()?;
    Some(
        logged
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(logged)
            .to_string(),
    )
}

/// The newest full (non `--max-bytes`) `import-e01` entry of the lane whose
/// raw output is named `name`: (line index, entry).
fn newest_full_import_of(lines: &[&str], name: &str) -> Option<(usize, serde_json::Value)> {
    lines.iter().enumerate().rev().find_map(|(index, line)| {
        let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        if value.get("event").and_then(serde_json::Value::as_str) != Some("import-e01") {
            return None;
        }
        // A capped (--max-bytes) export is not the whole image.
        (raw_output_name(&value).as_deref() == Some(name)
            && value.get("max_bytes").is_none_or(|v| v.is_null()))
        .then_some((index, value))
    })
}

/// R7-S10 / R8 P0: with audit keys on this PC the import entry must be
/// HMAC-authenticated here (`line_trust`, decided by this PC holding keys
/// — not by the lane carrying HMACs); without keys only self-consistency
/// is checked (P1). P2: an entry signed under another PC's key is "not
/// authenticable here — check on the PC holding key <id>".
fn import_entry_authenticated(
    verification: &audit::AuditChainVerification,
    index: usize,
) -> Result<(), String> {
    match verification.line_trust(index) {
        audit::LineTrust::SelfConsistentOnly { .. } => Ok(()),
        audit::LineTrust::Authenticated => match &verification.rewrite_not_excluded {
            Some(reason) => Err(format!("E01 감사 기록을 확인할 수 없습니다: {reason}")),
            None => Ok(()),
        },
        trust => Err(format!(
            "E01 감사 기록의 임포트 항목이 이 PC의 감사 키로 인증되지 않았습니다 — {} — 기록을 믿을 수 없습니다 (raw 이미지를 다시 확인해야 합니다)",
            trust.note()
        )),
    }
}

/// R7-S10: what a completion marker's lane check rests on.
const SELF_CONSISTENT_ONLY: &str =
    "e01-audit.jsonl (자기 일관성만 확인 — 감사 키가 없어 인증 아님)";
const HMAC_AUTHENTICATED: &str = "e01-audit.jsonl (감사 키 HMAC으로 인증됨)";

fn write_raw_done_marker_bound(
    raw: &Path,
    raw_sha256: &str,
    e01_path: &Path,
    imported_unix: u64,
    audit_entry_sha256: Option<&str>,
) -> Result<(), String> {
    let meta = std::fs::metadata(raw)
        .map_err(|err| format!("failed to stat imported raw {}: {err}", raw.display()))?;
    let keyed = crate::audit_key::verification_keys().is_ok_and(|keys| !keys.is_empty());
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
        "audit_entry_sha256": audit_entry_sha256,
        "audit_basis": audit_entry_sha256.map(|_| {
            if keyed { HMAC_AUTHENTICATED } else { SELF_CONSISTENT_ONLY }
        }),
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
///
/// R7-S10: for an image inside a case's `evidence/images`, a marker that
/// names its audit entry (`audit_entry_sha256`) must name an `import-e01`
/// entry of the case's verified E01 lane recording the same SHA-256 —
/// HMAC-authenticated when this PC holds audit keys. With keys, a marker
/// that names no entry (written before R7) is not trusted while the case
/// has an E01 lane: [`reconcile_raw`] re-hashes it against the
/// authenticated lane record and rewrites a bound marker.
pub fn raw_import_status(raw: &Path) -> RawImportStatus {
    match crate::audit_key::verification_keys() {
        Ok(keys) => raw_import_status_with_keys(raw, &keys),
        Err(err) => {
            if std::fs::metadata(raw).is_err() {
                return RawImportStatus::Missing;
            }
            RawImportStatus::Unverified(format!("감사 키를 읽지 못했습니다: {err}"))
        }
    }
}

/// [`raw_import_status`] with an explicit verification key set.
pub fn raw_import_status_with_keys(
    raw: &Path,
    keys: &[crate::audit_key::AuditKey],
) -> RawImportStatus {
    let status = raw_marker_status(raw);
    let RawImportStatus::Complete { sha256, .. } = &status else {
        return status;
    };
    let Some(lane) = case_e01_lane(raw) else {
        return status;
    };
    let marker = std::fs::read_to_string(raw_done_marker_path(raw))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let bound = marker
        .as_ref()
        .and_then(|value| value.get("audit_entry_sha256"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let marker_e01 = marker
        .as_ref()
        .and_then(|value| value.get("e01_path"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    match bound {
        Some(entry) => {
            match check_bound_import_entry(&lane, raw, &entry, sha256, &marker_e01, keys) {
                Ok(()) => status,
                Err(why) => RawImportStatus::Unverified(format!(
                    "완료 표식을 E01 감사 기록으로 확인할 수 없습니다 — {why}"
                )),
            }
        }
        // R8-I3 (P0): with audit keys on this PC a marker is Complete ONLY
        // through an authenticated import entry it names. A marker naming
        // none (written before R7, or a forged "legacy" marker) needs
        // re-verification — whether or not the case still HAS an E01 lane:
        // deleting the lane must not turn the marker back into trust.
        None if audit::trust_policy(keys).keys_held() => {
            RawImportStatus::Unverified(if lane.is_file() {
                "완료 표식이 E01 감사 기록의 항목과 결합되어 있지 않습니다(이전 형식) — 이 PC에는 감사 키가 있어 인증된 임포트 기록으로 다시 확인해야 합니다 (재검증 필요)"
                .to_string()
            } else {
                "완료 표식이 E01 감사 기록의 항목과 결합되어 있지 않고 케이스에 E01 감사 기록(e01-audit.jsonl)도 없습니다 — 이 PC에는 감사 키가 있어 인증되지 않은 표식을 믿지 않습니다 (재검증 필요: 다시 임포트하십시오)"
                .to_string()
            })
        }
        None => status,
    }
}

/// The marker-only part of [`raw_import_status`] (size, mtime, fields).
fn raw_marker_status(raw: &Path) -> RawImportStatus {
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
    /// R6-S4: the re-hash was cancelled; nothing was decided or changed.
    Cancelled,
}

/// What a raw image's import record says (marker or E01 audit lane).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawImportRecord {
    pub sha256: String,
    pub e01_path: PathBuf,
    pub imported_unix: u64,
    /// Byte size of the raw image, when the record has it.
    pub size_bytes: Option<u64>,
    /// R7-S10: the `entry_sha256` of the E01 audit entry the record came
    /// from (`None` for a record read from a marker without one).
    pub audit_entry_sha256: Option<String>,
    /// R7-S10: the audit entry was HMAC-authenticated on this PC; `false`
    /// = self-consistency only (no audit key here, or read from a marker).
    pub authenticated: bool,
}

/// R5-S6: the newest import record the case's chained E01 audit lane has
/// for a raw image named like `raw` — the evidence an old (pre-marker)
/// or moved case still has.
///
/// R6-S10: the lane is trusted only when its chain verifies
/// (`Err(reason)` otherwise — a hand-edited line can no longer supply the
/// hash a raw image is "confirmed" against), and the newest matching
/// record must name its E01 (`Err` when `e01_path` is missing or empty —
/// it used to be replaced by the CURRENT source, so any E01 inherited an
/// old image). `Ok(None)`: no log, or no matching full (non `--max-bytes`)
/// import.
pub fn recorded_raw_import(case_dir: &Path, raw: &Path) -> Result<Option<RawImportRecord>, String> {
    let keys = crate::audit_key::verification_keys()
        .map_err(|err| format!("감사 키를 읽지 못해 E01 감사 기록을 확인할 수 없습니다: {err}"))?;
    recorded_raw_import_with_keys(case_dir, raw, &keys)
}

/// [`recorded_raw_import`] with an explicit verification key set.
///
/// R7-S10: chain linkage alone is not enough when this PC holds audit
/// keys — the matching entry must be HMAC-authenticated here (an entry
/// re-chained without a key, or under an invented key id, is anyone's).
/// Without keys the record is self-consistency only
/// ([`RawImportRecord::authenticated`] false).
pub fn recorded_raw_import_with_keys(
    case_dir: &Path,
    raw: &Path,
    keys: &[crate::audit_key::AuditKey],
) -> Result<Option<RawImportRecord>, String> {
    let Some(name) = raw.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return Ok(None);
    };
    let log = case_dir.join("evidence/logs/e01-audit.jsonl");
    // R9 P3: the lane's rows through the one lane reader.
    let rows = match audit::read_lane_rows_with_keys(&log, keys) {
        Ok(rows) if !rows.present => return Ok(None),
        Ok(rows) => rows,
        Err(err) => return Err(format!("E01 감사 기록을 읽지 못했습니다: {err}")),
    };
    if let Some(err) = &rows.failure {
        return Err(format!(
            "E01 감사 기록(e01-audit.jsonl)의 체인 검증 실패 — 기록을 믿을 수 없습니다: {err}"
        ));
    }
    let Some(verification) = rows.verification.as_ref() else {
        return Ok(None);
    };
    let lines: Vec<&str> = rows.rows.iter().map(|row| row.raw.as_str()).collect();
    let Some((index, value)) = newest_full_import_of(&lines, &name) else {
        return Ok(None);
    };
    // R14 I5: not known to be the newest import after a lane repair.
    if rows.repaired_after(index) {
        return Err(E01_REPAIRED_AFTER_NOTE.to_string());
    }
    import_entry_authenticated(verification, index)?;
    let e01 = value
        .get("e01_path")
        .and_then(serde_json::Value::as_str)
        .filter(|e01| !e01.trim().is_empty())
        .ok_or_else(|| {
            "E01 감사 기록의 임포트 항목에 원본 E01 경로(e01_path)가 없습니다 — 어느 E01의 이미지인지 확인할 수 없습니다".to_string()
        })?;
    let sha = value
        .get("raw_sha256")
        .and_then(serde_json::Value::as_str)
        .map(str::to_ascii_lowercase)
        .filter(|sha| sha.len() == 64)
        .ok_or_else(|| "E01 감사 기록의 임포트 항목에 raw SHA-256이 없습니다".to_string())?;
    Ok(Some(RawImportRecord {
        sha256: sha,
        e01_path: PathBuf::from(e01),
        imported_unix: value
            .get("imported_unix")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        size_bytes: value
            .get("raw_size_bytes")
            .and_then(serde_json::Value::as_u64),
        audit_entry_sha256: value
            .get("entry_sha256")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        authenticated: audit::trust_policy(keys).keys_held(),
    }))
}

/// R6-S4: hooks of [`reconcile_raw`]'s (possibly hours-long) re-hash.
pub struct RehashHooks<'a> {
    /// Told the recorded SHA-256 right before hashing starts.
    pub on_rehash: &'a dyn Fn(&str),
    /// `(bytes hashed, total bytes)`, at most a few times per second.
    pub progress: &'a dyn Fn(u64, u64),
    /// Polled while hashing; `true` stops it ([`RawReuse::Cancelled`]).
    pub cancelled: &'a dyn Fn() -> bool,
}

/// Same E01 path (canonical when both resolve).
fn same_e01(a: &Path, b: &Path) -> bool {
    let a = canonicalize_display(a).unwrap_or_else(|_| a.to_path_buf());
    let b = canonicalize_display(b).unwrap_or_else(|_| b.to_path_buf());
    a == b
}

/// SHA-256 (lowercase hex) of `path` with progress and cancel (R6-S4).
/// `Ok(None)` when cancelled.
fn digest_file_with_progress(
    path: &Path,
    hooks: &RehashHooks<'_>,
) -> Result<Option<String>, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let total = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut done = 0u64;
    let mut last_report = std::time::Instant::now();
    (hooks.progress)(0, total);
    loop {
        if (hooks.cancelled)() {
            return Ok(None);
        }
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };
        hasher.update(&buffer[..read]);
        done += read as u64;
        if last_report.elapsed() >= std::time::Duration::from_millis(250) {
            (hooks.progress)(done, total);
            last_report = std::time::Instant::now();
        }
    }
    (hooks.progress)(done, total);
    Ok(Some(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    ))
}

/// R5-S6: [`raw_reuse_decision`] that re-verifies instead of giving up.
/// A raw image whose completion marker no longer matches (a moved or
/// copied case: new modification time) or that predates markers is
/// re-hashed against the SHA-256 the marker or the E01 audit lane
/// recorded; when it matches, the marker is rewritten and the image is
/// judged again (same E01 → reuse). Nothing is ever deleted here: a
/// mismatch or a missing record is [`RawReuse::NeedsReimport`] for the
/// examiner to decide. Runs outside any server lock (file hashing).
///
/// R6-S4: the cheap checks come first — a record of ANOTHER E01 or of a
/// different byte size is a mismatch without hashing (it used to hash
/// hundreds of GB to learn nothing); the hash reports progress and can be
/// cancelled ([`RehashHooks`]).
///
/// R8-I4 (P0): on a PC holding audit keys a Complete image of THIS E01 in
/// the case is re-hashed once before it is reused — the marker's size and
/// modification time are unauthenticated claims anyone can restore after
/// swapping the bytes — and compared with the SHA-256 of the authenticated
/// import entry the marker is bound to (progress and cancel as above).
pub fn reconcile_raw(
    case_dir: &Path,
    raw: &Path,
    source: &Path,
    hooks: &RehashHooks<'_>,
) -> RawReuse {
    let keys = match crate::audit_key::verification_keys() {
        Ok(keys) => keys,
        Err(err) => {
            if std::fs::metadata(raw).is_err() {
                return RawReuse::Absent;
            }
            return RawReuse::NeedsReimport(format!(
                "기존 raw 이미지를 완료된 임포트로 확인할 수 없습니다 — 감사 키를 읽지 못했습니다: {err}"
            ));
        }
    };
    let status = raw_import_status_with_keys(raw, &keys);
    let first = reuse_from_status(&status, source);
    if let (RawImportStatus::Complete { sha256, .. }, RawReuse::Reuse) = (&status, &first)
        && audit::trust_policy(&keys).keys_held()
        && case_e01_lane(raw).is_some()
    {
        (hooks.on_rehash)(sha256);
        return match digest_file_with_progress(raw, hooks) {
            Ok(Some(current)) if current.eq_ignore_ascii_case(sha256) => RawReuse::Reuse,
            Ok(Some(current)) => RawReuse::NeedsReimport(format!(
                "raw 이미지의 SHA-256이 인증된 임포트 기록과 다릅니다 (기록 {sha256}, 현재 {current}) — 완료 표식의 크기·수정 시각은 같지만 내용이 바뀜"
            )),
            Ok(None) => RawReuse::Cancelled,
            Err(err) => RawReuse::NeedsReimport(format!(
                "재사용 전에 raw 이미지의 SHA-256을 다시 계산하지 못했습니다: {err}"
            )),
        };
    }
    let RawImportStatus::Unverified(reason) = status else {
        return first;
    };
    // R7-S10: with audit keys on this PC the SHA-256 to compare with comes
    // from the AUTHENTICATED E01 audit entry only — the marker beside the
    // image is an unkeyed file anyone can write. Without keys the marker
    // (then the lane) is used, as before: self-consistency only.
    let from_marker = std::fs::read_to_string(raw_done_marker_path(raw))
        .ok()
        .filter(|_| !audit::trust_policy(&keys).keys_held())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| {
            let sha = value.get("sha256")?.as_str()?.to_ascii_lowercase();
            let e01 = value.get("e01_path")?.as_str()?.to_string();
            (sha.len() == 64 && !e01.trim().is_empty()).then(|| RawImportRecord {
                sha256: sha,
                e01_path: PathBuf::from(e01),
                imported_unix: value
                    .get("imported_unix")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                size_bytes: value.get("size_bytes").and_then(serde_json::Value::as_u64),
                audit_entry_sha256: value
                    .get("audit_entry_sha256")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                authenticated: false,
            })
        });
    let record = match from_marker {
        Some(record) => record,
        None => match recorded_raw_import_with_keys(case_dir, raw, &keys) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return RawReuse::NeedsReimport(format!(
                    "기존 raw 이미지를 완료된 임포트로 확인할 수 없습니다 — {reason}; 비교할 SHA-256 기록도 없습니다"
                ));
            }
            Err(err) => {
                return RawReuse::NeedsReimport(format!(
                    "기존 raw 이미지를 완료된 임포트로 확인할 수 없습니다 — {reason}; {err}"
                ));
            }
        },
    };
    if !same_e01(&record.e01_path, source) {
        return RawReuse::NeedsReimport(format!(
            "기존 raw 이미지는 다른 E01({})에서 임포트된 것입니다 (다시 해시하지 않음)",
            record.e01_path.display()
        ));
    }
    let current_size = match std::fs::metadata(raw) {
        Ok(meta) => meta.len(),
        Err(err) => {
            return RawReuse::NeedsReimport(format!(
                "기존 raw 이미지의 크기를 읽지 못했습니다: {err}"
            ));
        }
    };
    if let Some(recorded_size) = record.size_bytes
        && recorded_size != current_size
    {
        return RawReuse::NeedsReimport(format!(
            "raw 이미지의 크기가 임포트 기록과 다릅니다 (기록 {recorded_size}바이트, 현재 {current_size}바이트 — 다시 해시하지 않음)"
        ));
    }
    (hooks.on_rehash)(&record.sha256);
    let current = match digest_file_with_progress(raw, hooks) {
        Ok(Some(digest)) => digest,
        Ok(None) => return RawReuse::Cancelled,
        Err(err) => {
            return RawReuse::NeedsReimport(format!(
                "기존 raw 이미지의 SHA-256을 다시 계산하지 못했습니다: {err}"
            ));
        }
    };
    if current != record.sha256 {
        return RawReuse::NeedsReimport(format!(
            "raw 이미지의 SHA-256이 임포트 기록과 다릅니다 (기록 {}, 현재 {current})",
            record.sha256
        ));
    }
    if let Err(err) = write_raw_done_marker_bound(
        raw,
        &record.sha256,
        &record.e01_path,
        record.imported_unix,
        record.audit_entry_sha256.as_deref(),
    ) {
        return RawReuse::NeedsReimport(format!(
            "raw 이미지는 기록과 같지만 완료 표식을 갱신하지 못했습니다: {err}"
        ));
    }
    raw_reuse_decision(raw, source)
}

/// Pure decision on top of [`raw_import_status`]. The E01 paths are
/// compared canonically when possible.
pub fn raw_reuse_decision(raw: &Path, source: &Path) -> RawReuse {
    reuse_from_status(&raw_import_status(raw), source)
}

/// Pure core of [`raw_reuse_decision`].
fn reuse_from_status(status: &RawImportStatus, source: &Path) -> RawReuse {
    match status {
        RawImportStatus::Missing => RawReuse::Absent,
        RawImportStatus::Complete { e01_path, .. } if same_e01(e01_path, source) => RawReuse::Reuse,
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

    /// Re-hash hooks for the tests: `on_rehash` as given, no progress
    /// sink, never cancelled.
    fn hooks<'a>(on_rehash: &'a dyn Fn(&str)) -> super::RehashHooks<'a> {
        super::RehashHooks {
            on_rehash,
            progress: &|_, _| {},
            cancelled: &|| false,
        }
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
        let decision = reconcile_raw(&dir, &raw, &e01, &hooks(&|_| rehashed.set(true)));
        assert_eq!(decision, RawReuse::Reuse);
        assert!(rehashed.get());
        assert!(raw.is_file(), "never deleted");
        // The marker was refreshed: the next decision needs no re-hash.
        let rehashed = std::cell::Cell::new(false);
        assert_eq!(
            reconcile_raw(&dir, &raw, &e01, &hooks(&|_| rehashed.set(true))),
            RawReuse::Reuse
        );
        assert!(!rehashed.get());
        // Changed bytes (same size): the examiner decides; the image stays.
        fs::write(&raw, b"complete-imagX").unwrap();
        match reconcile_raw(&dir, &raw, &e01, &hooks(&|_| {})) {
            RawReuse::NeedsReimport(reason) => assert!(reason.contains("SHA-256"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(raw.is_file());
        // Pre-marker case: the E01 audit lane's record is used.
        fs::remove_file(raw_done_marker_path(&raw)).unwrap();
        fs::write(&raw, b"old-case-image").unwrap();
        let old_sha = crate::audit::digest_file(&raw).unwrap();
        match reconcile_raw(&dir, &raw, &e01, &hooks(&|_| {})) {
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
        assert_eq!(
            reconcile_raw(&dir, &raw, &e01, &hooks(&|_| {})),
            RawReuse::Reuse
        );
        assert!(raw_done_marker_path(&raw).is_file(), "marker written");
        let _ = fs::remove_dir_all(&dir);
    }

    /// R6-S4: a record of another E01 or of another byte size is a
    /// mismatch WITHOUT hashing (it used to hash the whole image first —
    /// hours for a large disk — to report what the record already said);
    /// the hash itself reports progress and stops on cancel.
    #[test]
    fn s4_reconcile_checks_size_and_e01_before_hashing() {
        use super::{RawReuse, reconcile_raw, write_raw_done_marker};
        let dir = c1_dir("s4");
        let e01 = dir.join("disk.E01");
        let other = dir.join("other.E01");
        fs::write(&e01, b"ewf").unwrap();
        fs::write(&other, b"ewf2").unwrap();
        let raw = dir.join("evidence/images/evidence.raw");
        fs::write(&raw, b"complete-image").unwrap();
        let sha = crate::audit::digest_file(&raw).unwrap();
        write_raw_done_marker(&raw, &sha, &e01, 7).unwrap();
        // Grow the image: the marker's size no longer matches.
        fs::write(&raw, b"complete-image-and-more").unwrap();
        let rehashed = std::cell::Cell::new(false);
        match reconcile_raw(&dir, &raw, &e01, &hooks(&|_| rehashed.set(true))) {
            RawReuse::NeedsReimport(reason) => assert!(reason.contains("크기"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(
            !rehashed.get(),
            "a size mismatch is decided without hashing"
        );
        // Same size, but the marker names another E01: no hash either.
        fs::write(&raw, b"complete-imagX").unwrap();
        match reconcile_raw(&dir, &raw, &other, &hooks(&|_| rehashed.set(true))) {
            RawReuse::NeedsReimport(reason) => assert!(reason.contains("다른 E01"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(!rehashed.get(), "another E01 is decided without hashing");
        // Progress is reported and a cancel stops the hash.
        let seen = std::cell::Cell::new(0u64);
        let cancelled = super::RehashHooks {
            on_rehash: &|_| {},
            progress: &|_, total| seen.set(total),
            cancelled: &|| true,
        };
        assert_eq!(
            reconcile_raw(&dir, &raw, &e01, &cancelled),
            RawReuse::Cancelled
        );
        assert_eq!(seen.get(), 14, "total bytes reported before hashing");
        assert!(raw.is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    /// R6-S10: the E01 audit lane vouches for a raw image only when its
    /// chain verifies, and only with the E01 it names. Before, a line
    /// edited into the log was trusted as is, and a record without
    /// `e01_path` was bound to whatever E01 was being opened now.
    #[test]
    fn s10_audit_lane_must_verify_and_name_its_e01() {
        use super::{RawReuse, reconcile_raw};
        let dir = c1_dir("s10");
        let e01 = dir.join("disk.E01");
        fs::write(&e01, b"ewf").unwrap();
        let raw = dir.join("evidence/images/evidence.raw");
        fs::write(&raw, b"old-case-image").unwrap();
        let sha = crate::audit::digest_file(&raw).unwrap();
        let log = dir.join("evidence/logs/e01-audit.jsonl");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        // A record without e01_path: refused, never bound to `e01`.
        crate::audit::append_chained_jsonl_keyed(
            &log,
            &serde_json::json!({
                "schema_version": 1, "event": "import-e01", "imported_unix": 3,
                "raw_output_path": "/x/evidence/images/evidence.raw",
                "raw_sha256": sha, "max_bytes": null,
            })
            .to_string(),
            None,
        )
        .unwrap();
        match reconcile_raw(&dir, &raw, &e01, &hooks(&|_| {})) {
            RawReuse::NeedsReimport(reason) => assert!(reason.contains("e01_path"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(!super::raw_done_marker_path(&raw).exists());
        // A hand-written line (no chain fields) after it: the lane no
        // longer verifies, so nothing in it is trusted.
        let mut text = fs::read_to_string(&log).unwrap();
        text.push_str(
            &serde_json::json!({
                "schema_version": 1, "event": "import-e01", "imported_unix": 4,
                "e01_path": e01.to_string_lossy(),
                "raw_output_path": "/x/evidence/images/evidence.raw",
                "raw_sha256": sha, "max_bytes": null,
            })
            .to_string(),
        );
        text.push('\n');
        fs::write(&log, text).unwrap();
        match reconcile_raw(&dir, &raw, &e01, &hooks(&|_| {})) {
            RawReuse::NeedsReimport(reason) => assert!(reason.contains("체인"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(!super::raw_done_marker_path(&raw).exists());
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

    /// R7-S10: with audit keys on this PC, the E01 lane's import record is
    /// trusted only when its entry is HMAC-authenticated here (chain
    /// linkage alone is anyone's), and a completion marker bound to an
    /// audit entry must name an authenticated import entry recording the
    /// same SHA-256. Without keys both are self-consistency only.
    #[test]
    fn e01_import_record_and_marker_need_an_authenticated_entry() {
        use super::{
            RawImportStatus, raw_done_marker_path, raw_import_status_with_keys,
            recorded_raw_import_with_keys, write_raw_done_marker, write_raw_done_marker_bound,
        };
        let dir = c1_dir("r7-s10");
        let e01 = dir.join("disk.E01");
        fs::write(&e01, b"ewf").unwrap();
        let raw = dir.join("evidence/images/evidence.raw");
        fs::write(&raw, b"complete-image").unwrap();
        let sha = crate::audit::digest_file(&raw).unwrap();
        let lane = dir.join("evidence/logs/e01-audit.jsonl");
        let entry = |unix: u64| {
            serde_json::json!({"schema_version": 1, "event": "import-e01",
                "imported_unix": unix, "e01_path": e01.to_string_lossy(),
                "raw_output_path": raw.to_string_lossy(), "raw_sha256": sha,
                "max_bytes": null})
            .to_string()
        };
        let mine = crate::audit_key::AuditKey::new("mine", &[0x51; 32]);
        let keys = std::slice::from_ref(&mine);

        // Unkeyed import entry: fine without keys, refused with keys.
        crate::audit::append_chained_jsonl_keyed(&lane, &entry(7), None).unwrap();
        let record = recorded_raw_import_with_keys(&dir, &raw, &[])
            .unwrap()
            .unwrap();
        assert!(!record.authenticated);
        let err = recorded_raw_import_with_keys(&dir, &raw, keys).unwrap_err();
        assert!(err.contains("인증되지 않았습니다"), "{err}");
        // Keyed under an invented key id: refused too.
        let bogus = crate::audit_key::AuditKey::new("bogus-id", &[0x52; 32]);
        crate::audit::append_chained_jsonl_keyed(&lane, &entry(8), Some(&bogus)).unwrap();
        assert!(recorded_raw_import_with_keys(&dir, &raw, keys).is_err());
        // Authenticated entry: trusted, and it names its entry hash.
        crate::audit::append_chained_jsonl_keyed(&lane, &entry(9), Some(&mine)).unwrap();
        let record = recorded_raw_import_with_keys(&dir, &raw, keys)
            .unwrap()
            .unwrap();
        assert!(record.authenticated);
        assert_eq!(record.imported_unix, 9);
        let entry_sha = record.audit_entry_sha256.clone().unwrap();

        // Marker bound to the authenticated entry: complete.
        write_raw_done_marker_bound(&raw, &sha, &e01, 9, Some(&entry_sha)).unwrap();
        assert!(matches!(
            raw_import_status_with_keys(&raw, keys),
            RawImportStatus::Complete { .. }
        ));
        // Bound to the unkeyed entry: not trusted with keys — and (R8-I4)
        // not even without keys, because it is no longer this image's
        // newest import (entry 9 superseded it).
        let unkeyed_entry = super::import_entry_sha256(&dir, &sha, 7).unwrap();
        write_raw_done_marker_bound(&raw, &sha, &e01, 7, Some(&unkeyed_entry)).unwrap();
        assert!(matches!(
            raw_import_status_with_keys(&raw, keys),
            RawImportStatus::Unverified(_)
        ));
        assert!(matches!(
            raw_import_status_with_keys(&raw, &[]),
            RawImportStatus::Unverified(why) if why.contains("최신 임포트 기록이 아닙니다")
        ));
        // Bound to an entry the lane does not hold: never complete.
        write_raw_done_marker_bound(&raw, &sha, &e01, 9, Some(&"0".repeat(64))).unwrap();
        assert!(matches!(
            raw_import_status_with_keys(&raw, &[]),
            RawImportStatus::Unverified(_)
        ));
        // A legacy marker (no entry named) is not trusted with keys while
        // the case has an E01 lane; without keys it still is.
        write_raw_done_marker(&raw, &sha, &e01, 9).unwrap();
        let marker = fs::read_to_string(raw_done_marker_path(&raw)).unwrap();
        assert!(marker.contains("\"audit_entry_sha256\": null"), "{marker}");
        assert!(matches!(
            raw_import_status_with_keys(&raw, keys),
            RawImportStatus::Unverified(_)
        ));
        assert!(matches!(
            raw_import_status_with_keys(&raw, &[]),
            RawImportStatus::Complete { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    fn r8_import_entry(raw_name: &str, e01: &str, sha: &str, unix: u64) -> String {
        serde_json::json!({"schema_version": 1, "event": "import-e01",
            "imported_unix": unix, "e01_path": e01,
            "raw_output_path": format!("C:/elsewhere/evidence/images/{raw_name}"),
            "raw_sha256": sha, "max_bytes": null})
        .to_string()
    }

    /// R8-I3 (P0): on a PC holding audit keys, deleting the case's E01
    /// lane no longer turns a legacy (unbound) completion marker back into
    /// trust — R7 only distrusted it while the lane existed. P2: an import
    /// entry signed on another PC is "not authenticable here — check on the
    /// PC holding key <id>"; that PC trusts it.
    #[test]
    fn r8_i3_deleted_lane_or_other_pcs_entry_is_never_complete_here() {
        use super::{RawImportStatus, raw_import_status_with_keys, write_raw_done_marker};
        let dir = c1_dir("r8-i3");
        let e01 = dir.join("disk.E01");
        fs::write(&e01, b"ewf").unwrap();
        let raw = dir.join("evidence/images/evidence.raw");
        fs::write(&raw, b"complete-image").unwrap();
        let sha = crate::audit::digest_file(&raw).unwrap();
        let mine = crate::audit_key::AuditKey::new("pc-a", &[0xa1; 32]);
        let other = crate::audit_key::AuditKey::new("pc-b", &[0xa2; 32]);
        // A legacy marker and NO E01 lane (deleted).
        write_raw_done_marker(&raw, &sha, &e01, 9).unwrap();
        assert!(!dir.join("evidence/logs/e01-audit.jsonl").exists());
        assert!(matches!(
            raw_import_status_with_keys(&raw, std::slice::from_ref(&mine)),
            RawImportStatus::Unverified(why) if why.contains("재검증 필요")
        ));
        // P1: without keys it is self-consistency only, as before.
        assert!(matches!(
            raw_import_status_with_keys(&raw, &[]),
            RawImportStatus::Complete { .. }
        ));
        // P2: the import was recorded on PC B.
        let lane = dir.join("evidence/logs/e01-audit.jsonl");
        crate::audit::append_chained_jsonl_keyed(
            &lane,
            &r8_import_entry("evidence.raw", &e01.to_string_lossy(), &sha, 9),
            Some(&other),
        )
        .unwrap();
        let entry = super::import_entry_sha256(&dir, &sha, 9).unwrap();
        super::write_raw_done_marker_bound(&raw, &sha, &e01, 9, Some(&entry)).unwrap();
        match raw_import_status_with_keys(&raw, std::slice::from_ref(&mine)) {
            RawImportStatus::Unverified(why) => {
                assert!(why.contains("\"pc-b\" 보유 PC에서 확인"), "{why}")
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            raw_import_status_with_keys(&raw, std::slice::from_ref(&other)),
            RawImportStatus::Complete { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    /// R9-I8: a raw image in a SUBFOLDER of `evidence/images` is judged
    /// like one directly in it (the scope `require_complete_case_raw`
    /// uses). R8 resolved no E01 lane for it and returned the legacy
    /// marker's Complete unchecked, even on a PC holding keys.
    #[test]
    fn r9_i8_subfolder_raw_gets_the_same_trust_check() {
        use super::{RawImportStatus, raw_import_status_with_keys, write_raw_done_marker};
        let dir = c1_dir("r9-i8");
        let e01 = dir.join("disk.E01");
        fs::write(&e01, b"ewf").unwrap();
        fs::create_dir_all(dir.join("evidence/images/sub")).unwrap();
        let raw = dir.join("evidence/images/sub/evidence.raw");
        fs::write(&raw, b"complete-image").unwrap();
        let sha = crate::audit::digest_file(&raw).unwrap();
        let mine = crate::audit_key::AuditKey::new("pc-a", &[0xa9; 32]);
        write_raw_done_marker(&raw, &sha, &e01, 9).unwrap();
        assert_eq!(
            super::case_e01_lane(&raw),
            Some(dir.join("evidence").join("logs").join("e01-audit.jsonl"))
        );
        assert!(matches!(
            raw_import_status_with_keys(&raw, std::slice::from_ref(&mine)),
            RawImportStatus::Unverified(why) if why.contains("재검증 필요")
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    /// R8-I4: the import entry a marker names must be an import of THIS
    /// image (raw file name), of the marker's E01, and the newest one; and
    /// with keys held a Complete image is re-hashed before reuse — bytes
    /// swapped under a restored size and modification time were reused in
    /// R7.
    #[test]
    fn r8_i4_marker_entry_binding_and_rehash_before_reuse() {
        use super::{
            RawImportStatus, RawReuse, raw_import_status_with_keys, reconcile_raw,
            write_raw_done_marker_bound,
        };
        let dir = c1_dir("r8-i4");
        let e01 = dir.join("disk.E01");
        fs::write(&e01, b"ewf").unwrap();
        let raw = dir.join("evidence/images/evidence.raw");
        fs::write(&raw, b"complete-image").unwrap();
        let sha = crate::audit::digest_file(&raw).unwrap();
        let mine = crate::audit_key::AuditKey::new("pc-a", &[0xa3; 32]);
        let keys = std::slice::from_ref(&mine);
        let lane = dir.join("evidence/logs/e01-audit.jsonl");
        let e01_text = e01.to_string_lossy().to_string();
        // An authenticated import of ANOTHER raw file with the same bytes.
        crate::audit::append_chained_jsonl_keyed(
            &lane,
            &r8_import_entry("other.raw", &e01_text, &sha, 5),
            Some(&mine),
        )
        .unwrap();
        let other_entry = super::import_entry_sha256(&dir, &sha, 5).unwrap();
        write_raw_done_marker_bound(&raw, &sha, &e01, 5, Some(&other_entry)).unwrap();
        assert!(matches!(
            raw_import_status_with_keys(&raw, keys),
            RawImportStatus::Unverified(why) if why.contains("다른 raw 파일")
        ));
        // An import of this file but from another E01 than the marker's.
        crate::audit::append_chained_jsonl_keyed(
            &lane,
            &r8_import_entry("evidence.raw", "D:/other.E01", &sha, 6),
            Some(&mine),
        )
        .unwrap();
        let wrong_e01 = super::import_entry_sha256(&dir, &sha, 6).unwrap();
        write_raw_done_marker_bound(&raw, &sha, &e01, 6, Some(&wrong_e01)).unwrap();
        assert!(matches!(
            raw_import_status_with_keys(&raw, keys),
            RawImportStatus::Unverified(why) if why.contains("e01_path")
        ));
        // The newest import of this file from this E01: complete.
        crate::audit::append_chained_jsonl_keyed(
            &lane,
            &r8_import_entry("evidence.raw", &e01_text, &sha, 7),
            Some(&mine),
        )
        .unwrap();
        let entry = super::import_entry_sha256(&dir, &sha, 7).unwrap();
        write_raw_done_marker_bound(&raw, &sha, &e01, 7, Some(&entry)).unwrap();
        assert!(matches!(
            raw_import_status_with_keys(&raw, keys),
            RawImportStatus::Complete { .. }
        ));
        let rehashed = std::cell::Cell::new(false);
        crate::audit_key::with_test_keys(keys, || {
            assert_eq!(
                reconcile_raw(&dir, &raw, &e01, &hooks(&|_| rehashed.set(true))),
                RawReuse::Reuse
            );
            assert!(rehashed.get(), "hashed once before reuse");
            // Same size, bytes swapped, modification time restored.
            let modified = fs::metadata(&raw).unwrap().modified().unwrap();
            fs::write(&raw, b"COMPLETE-IMAGE").unwrap();
            fs::OpenOptions::new()
                .write(true)
                .open(&raw)
                .unwrap()
                .set_modified(modified)
                .unwrap();
            assert!(matches!(
                raw_import_status_with_keys(&raw, keys),
                RawImportStatus::Complete { .. }
            ));
            assert!(matches!(
                reconcile_raw(&dir, &raw, &e01, &hooks(&|_| {})),
                RawReuse::NeedsReimport(why) if why.contains("SHA-256")
            ));
        });
        let _ = fs::remove_dir_all(&dir);
    }
}
