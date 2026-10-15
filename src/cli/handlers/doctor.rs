use super::*;
use crate::audit;
use crate::case_db;
use crate::util::{json_escape, write_text_atomic};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct BenchmarkOptions {
    pub rows: usize,
}

pub fn benchmark_db(output_dir: &Path, options: BenchmarkOptions) -> Result<(), String> {
    let result = case_db::benchmark_case_db(output_dir, options.rows)?;
    println!("SQLite benchmark complete");
    println!("rows: {}", result.rows);
    println!("elapsed_ms: {}", result.elapsed_ms);
    println!("db: {}", result.path.display());
    Ok(())
}

pub fn verify_audit(log_path: &Path) -> Result<(), String> {
    // Examiners naturally point this at the case directory; discover the
    // standard chained logs under evidence/logs instead of dying on the
    // directory read.
    if log_path.is_dir() {
        let logs_dir = log_path.join("evidence/logs");
        let mut logs: Vec<PathBuf> = Vec::new();
        let entries = std::fs::read_dir(&logs_dir).map_err(|err| {
            format!(
                "{} is a directory but has no readable evidence/logs: {err}",
                log_path.display()
            )
        })?;
        for entry in entries {
            let path = entry
                .map_err(|err| format!("failed to list {}: {err}", logs_dir.display()))?
                .path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                logs.push(path);
            }
        }
        logs.sort();
        if logs.is_empty() {
            return Err(format!(
                "no audit .jsonl logs found under {}",
                logs_dir.display()
            ));
        }
        let mut failures = Vec::new();
        let mut not_verified = Vec::new();
        for log in &logs {
            match audit::verify_chained_jsonl(log) {
                // R7-I3: a possible tail rewrite is never "verified".
                Ok(result) if result.rewrite_not_excluded.is_some() => {
                    println!(
                        "audit NOT VERIFIED: {} (entries: {}): {}",
                        log.display(),
                        result.entries,
                        result.rewrite_not_excluded.as_deref().unwrap_or_default()
                    );
                    not_verified.push(log.display().to_string());
                }
                Ok(result) => {
                    println!(
                        "audit verified: {} (entries: {}, integrity: {})",
                        log.display(),
                        result.entries,
                        result.integrity.label()
                    );
                }
                Err(err) => {
                    println!("audit FAILED: {}: {err}", log.display());
                    failures.push(log.display().to_string());
                }
            }
        }
        if !failures.is_empty() {
            return Err(format!(
                "{} audit log(s) failed verification",
                failures.len()
            ));
        }
        if !not_verified.is_empty() {
            return Err(format!(
                "{} audit log(s) could not be verified (NOT VERIFIED — not a tampering verdict, but a re-write cannot be ruled out)",
                not_verified.len()
            ));
        }
        return Ok(());
    }
    let result = audit::verify_chained_jsonl(log_path)?;
    if let Some(reason) = &result.rewrite_not_excluded {
        println!("audit NOT VERIFIED: {}", log_path.display());
        println!("entries: {}", result.entries);
        for warning in &result.warnings {
            println!("warning: {warning}");
        }
        return Err(format!(
            "audit log {} could not be verified: {reason}",
            log_path.display()
        ));
    }
    println!("audit verified: {}", log_path.display());
    println!("entries: {}", result.entries);
    println!("last entry sha256: {}", result.last_entry_sha256);
    println!("integrity: {}", result.integrity.label());
    if result.keyed_entries > 0 {
        println!(
            "keyed entries: {} ({} unauthenticated)",
            result.keyed_entries, result.unauthenticated_keyed_entries
        );
    }
    for warning in &result.warnings {
        println!("warning: {warning}");
    }
    Ok(())
}

/// `rotate-audit-key`: generate a fresh key, make it the keyring's active
/// key, retire the previous key, and optionally stamp signed marker entries
/// into the logs that grow across the boundary.
///
/// R7-I6: a `--log` that is a CASE DIRECTORY stamps the marker into every
/// known chain lane of that case ([`rotation_marker_lanes`]) — the
/// sign-off lane above all: an entry signed by the now-retired key is only
/// authenticated when a newer key's entry follows it, so without the marker
/// every earlier sign-off would read "cannot be verified" after rotation.
pub fn rotate_audit_key(key_id: Option<&str>, marker_logs: &[PathBuf]) -> Result<(), String> {
    // Resolve the lanes BEFORE rotating, so a bad --log path fails without
    // changing the keyring.
    let mut targets: Vec<PathBuf> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for log in marker_logs {
        if log.is_dir() {
            let (lanes, skip) = rotation_marker_lanes(log)?;
            targets.extend(lanes);
            skipped.extend(skip);
        } else {
            targets.push(log.clone());
        }
    }
    targets.dedup();
    let rotation = crate::audit_key::rotate(key_id)?;
    println!("audit key rotated");
    println!("new key id: {}", rotation.new_key.id);
    match &rotation.previous_id {
        Some(id) => {
            println!("retired key id: {id} (kept in the keyring — its entries still verify)")
        }
        None => println!("no previous key configured — audit keying enabled"),
    }
    println!("keyring: {}", rotation.keyring_path.display());
    for note in &skipped {
        println!("marker skipped: {note}");
    }
    for log in &targets {
        let marker = format!(
            "{{\"kind\":\"audit-key-rotate\",\"from\":{},\"to\":{}}}",
            audit::optional_string(rotation.previous_id.as_deref()),
            audit::optional_string(Some(&rotation.new_key.id)),
        );
        // The marker is signed under the NEW key explicitly (the design's
        // rotation rule), even if a --key-source override still names the
        // old key for this process.
        audit::append_chained_jsonl_keyed(log, &marker, Some(&rotation.new_key))?;
        println!("marker appended: {}", log.display());
    }
    Ok(())
}

/// R7-I6: the lanes of the case at `case_dir` that get a rotation marker:
/// every known chain lane present with at least one entry whose chain
/// verifies now — for the sound measurements only the lane new entries go
/// to. A lane that does not verify is left exactly as found (named in the
/// second list): appending to it would not prove anything and a damaged
/// sound lane must never be written to (R5-P8).
pub(crate) fn rotation_marker_lanes(
    case_dir: &Path,
) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    if !case_dir.join("evidence/logs").is_dir() {
        return Err(format!(
            "--log {}: a directory must be a case directory (no evidence/logs)",
            case_dir.display()
        ));
    }
    let active_audio = crate::audio_level::verified_active_lane(case_dir)?;
    let mut lanes = Vec::new();
    let mut skipped = Vec::new();
    for rel in audit::collect_chain_lanes(case_dir) {
        let path = case_dir.join(&rel);
        if audit::audio_level_lane_number(&rel).is_some() {
            if active_audio.as_deref() == Some(rel.as_str()) {
                lanes.push(path);
            } else {
                skipped.push(format!(
                    "{rel} (not the verified active sound-measurement lane — left unchanged)"
                ));
            }
            continue;
        }
        match audit::verify_chained_jsonl(&path) {
            Ok(result) if result.entries > 0 => lanes.push(path),
            Ok(_) => skipped.push(format!("{rel} (empty)")),
            Err(err) => skipped.push(format!(
                "{rel} (chain does not verify — left unchanged: {err})"
            )),
        }
    }
    Ok((lanes, skipped))
}

/// Screen one file with the deepfake-lens sidecar and emit its JSON report.
///
/// The report keeps deepfake-lens's own framing: scores are review
/// priorities, not authenticity verdicts.
pub fn deepfake_screen(file: &Path, json_out: Option<&Path>) -> Result<(), String> {
    let summary = crate::deepfake::screen(file);
    if !summary.ok {
        return Err(format!(
            "deepfake-lens screening failed: {}",
            summary.error.unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    let raw = summary.raw_json.clone().unwrap_or_else(|| "{}".to_string());
    match json_out {
        Some(out) => {
            write_text_atomic(out, &raw)
                .map_err(|err| format!("failed to write {}: {err}", out.display()))?;
            println!("deepfake report written: {}", out.display());
        }
        None => println!("{raw}"),
    }
    Ok(())
}

pub fn deepfake_scan(case_dir: &Path, force: bool, retry_failed: bool) -> Result<(), String> {
    ensure_case(case_dir)?;
    let job = case_db::start_job(
        case_dir,
        "deepfake-scan",
        case_dir,
        None,
        &format!("{{\"force\":{force},\"retry_failed\":{retry_failed}}}"),
    )?;
    let progress = |done: usize, total: usize, id: &str| {
        if !id.is_empty() {
            println!("deepfake screen {done}/{total}: {id}");
        }
    };
    let stats =
        match crate::deepfake::screen_case_parallel(case_dir, force, retry_failed, &progress, 3) {
            Ok(stats) => stats,
            Err(err) => {
                let _ = case_db::fail_job(case_dir, &job.job_id, &err);
                return Err(err);
            }
        };
    case_db::complete_job(
        case_dir,
        &job.job_id,
        stats.screened.max(1) as u64,
        "deepfake-scan completed",
    )?;
    println!("deepfake scan complete");
    println!(
        "screened: {} · already present: {} · file missing: {} · failed: {}",
        stats.screened, stats.skipped_existing, stats.skipped_missing, stats.failed
    );
    println!(
        "artifacts: {}",
        case_dir.join("artifacts/deepfake").display()
    );
    println!(
        "next: make-review {} — 뷰어에 '합성의심' 배지로 표시됩니다",
        case_dir.display()
    );
    Ok(())
}

/// One diagnostic line for `frametrace doctor`. `level` renders as
/// OK/WARN/FAIL — WARN means the workstation runs but a feature lane is
/// unavailable, FAIL means a blocking defect (corrupt db, broken keyring).
struct DoctorCheck {
    level: &'static str,
    name: String,
    detail: String,
}

impl DoctorCheck {
    fn ok(name: &str, detail: String) -> Self {
        Self {
            level: "OK",
            name: name.to_string(),
            detail,
        }
    }
    fn warn(name: &str, detail: String) -> Self {
        Self {
            level: "WARN",
            name: name.to_string(),
            detail,
        }
    }
    fn fail(name: &str, detail: String) -> Self {
        Self {
            level: "FAIL",
            name: name.to_string(),
            detail,
        }
    }
}

/// Preflight/field diagnostic: which forensic lanes are usable right now,
/// whether the target volume is writable with headroom, and — when a case
/// is given — whether its SQLite index and chained audit logs are intact.
/// Hardware write blockers cannot be detected from software; the writable
/// probe is the actionable equivalent.
pub fn doctor(case_dir: Option<&Path>, json: bool) -> Result<(), String> {
    let mut checks = Vec::new();

    for (name, feature) in [
        ("ffmpeg", "클립/프록시/썸네일/포맷 변환"),
        ("ffprobe", "영상 메타데이터·스트림 검증"),
        ("ewfinfo", "E01 이미지 메타데이터"),
        ("ewfverify", "E01 무결성 검증"),
        ("ewfexport", "E01 → RAW 추출"),
        ("mmls", "파티션 맵 (삭제 영상 복구 전제)"),
        ("fls", "삭제 파일 나열"),
        ("icat", "inode 내용 복구"),
    ] {
        match crate::tool_policy::resolve_tool_binary(name, &[name]) {
            Ok(found) => checks.push(DoctorCheck::ok(name, format!("{found} — {feature}"))),
            Err(err) => checks.push(DoctorCheck::warn(
                name,
                format!("미설치 — {feature} 불가 ({err})"),
            )),
        }
    }

    // deepfake-lens mirrors api_env: env override file, PATH shim, then
    // python interpreter fallback (the lane only needs `python -m
    // deepfake_lens.cli` importable).
    let deepfake_ok = std::env::var("FRAMETRACE_DEEPFAKE_LENS")
        .ok()
        .map(|v| !v.trim().is_empty() && PathBuf::from(v.trim()).is_file())
        .unwrap_or(false)
        || crate::tool_policy::resolve_tool_binary("deepfake-lens", &["deepfake-lens"]).is_ok()
        || crate::tool_policy::resolve_tool_binary("python", &["python", "python3", "py"]).is_ok();
    checks.push(if deepfake_ok {
        DoctorCheck::ok("deepfake-lens", "합성 영상 스크리닝 사용 가능".into())
    } else {
        DoctorCheck::warn(
            "deepfake-lens",
            "미설치 — 합성 영상 스크리닝 불가 (선택 기능)".into(),
        )
    });

    // Writable probe + free space on the case (or current) volume.
    let probe_dir = case_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let probe = probe_dir.join(format!(".frametrace-doctor-{}", std::process::id()));
    match fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            checks.push(DoctorCheck::ok(
                "case volume writable",
                format!("{} — 쓰기 가능", probe_dir.display()),
            ));
        }
        Err(err) => checks.push(DoctorCheck::fail(
            "case volume writable",
            format!("{} — 쓰기 불가: {err}", probe_dir.display()),
        )),
    }
    match fs2::free_space(&probe_dir) {
        Ok(free) => {
            let gib = free as f64 / (1024.0 * 1024.0 * 1024.0);
            checks.push(if free < 10 * 1024 * 1024 * 1024 {
                DoctorCheck::warn(
                    "free space",
                    format!("{gib:.1} GiB 여유 — 대용량 이미지 추출 전 용량 확보 권장"),
                )
            } else {
                DoctorCheck::ok("free space", format!("{gib:.1} GiB 여유"))
            });
        }
        Err(err) => checks.push(DoctorCheck::warn(
            "free space",
            format!("여유 공간 조회 실패: {err}"),
        )),
    }

    // HMAC keyring: absent is a warning (structural-only audit), present-
    // but-broken is a fail (keyed entries unverifiable).
    match crate::audit_key::configured() {
        Ok(Some(key)) => checks.push(DoctorCheck::ok(
            "audit keyring",
            format!("활성 키 '{}' — HMAC 서명 감사 사용", key.id),
        )),
        Ok(None) => checks.push(DoctorCheck::warn(
            "audit keyring",
            "키 없음 — 감사 로그가 구조 검증 전용으로 동작".into(),
        )),
        Err(err) => checks.push(DoctorCheck::fail(
            "audit keyring",
            format!("키링 손상 — {err}"),
        )),
    }

    if let Some(case_dir) = case_dir {
        ensure_case(case_dir)?;
        let db_path = case_db::case_db_path(case_dir);
        if db_path.is_file() {
            match rusqlite::Connection::open(&db_path).and_then(|conn| {
                conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            }) {
                Ok(result) if result == "ok" => {
                    let videos = rusqlite::Connection::open(&db_path)
                        .and_then(|conn| {
                            conn.query_row("SELECT COUNT(*) FROM videos", [], |row| {
                                row.get::<_, i64>(0)
                            })
                        })
                        .unwrap_or(-1);
                    checks.push(DoctorCheck::ok(
                        "sqlite integrity",
                        format!("integrity_check ok — 색인 {}건", videos.max(0)),
                    ));
                }
                Ok(other) => checks.push(DoctorCheck::fail(
                    "sqlite integrity",
                    format!("integrity_check: {other}"),
                )),
                Err(err) => checks.push(DoctorCheck::fail(
                    "sqlite integrity",
                    format!("case.db 열기 실패: {err}"),
                )),
            }
        } else {
            checks.push(DoctorCheck::warn(
                "sqlite integrity",
                "case.db 없음 — 아직 스캔되지 않은 케이스".into(),
            ));
        }

        for log_dir in [
            case_dir.join("evidence/logs"),
            case_dir.join("artifacts/logs"),
            case_dir.join("artifacts/clips"),
        ] {
            let Ok(entries) = fs::read_dir(&log_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let name = format!(
                    "audit log {}",
                    path.strip_prefix(case_dir).unwrap_or(&path).display()
                );
                match audit::verify_chained_jsonl(&path) {
                    Ok(result) => {
                        let detail = format!(
                            "{}건 · {}{}",
                            result.entries,
                            result.integrity.label(),
                            if result.warnings.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", result.warnings.join("; "))
                            }
                        );
                        checks.push(if result.integrity == crate::audit::AuditIntegrity::Keyed {
                            DoctorCheck::ok(&name, detail)
                        } else {
                            DoctorCheck::warn(&name, detail)
                        });
                    }
                    Err(err) => checks.push(DoctorCheck::fail(&name, err)),
                }
            }
        }
    }

    let fails = checks.iter().filter(|c| c.level == "FAIL").count();
    let warns = checks.iter().filter(|c| c.level == "WARN").count();
    if json {
        let body = checks
            .iter()
            .map(|c| {
                format!(
                    "{{\"level\":\"{}\",\"name\":\"{}\",\"detail\":\"{}\"}}",
                    c.level,
                    json_escape(&c.name),
                    json_escape(&c.detail)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{{\"ok\":{},\"fails\":{fails},\"warns\":{warns},\"checks\":[{body}]}}",
            fails == 0
        );
    } else {
        for check in &checks {
            println!("[{}] {} — {}", check.level, check.name, check.detail);
        }
        println!("doctor: {fails} fail · {warns} warn");
    }
    if fails > 0 {
        Err(format!("doctor: {fails} blocking problem(s) found"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::audit;
    use crate::audit_key::AuditKey;
    use std::fs;

    /// R7-I6: `rotate-audit-key --log <case dir>` stamps the rotation
    /// marker into every verifying chain lane of the case — the sign-off
    /// lane included — and leaves damaged lanes untouched. With the marker
    /// (signed by the new key) after it, a sign-off entry signed by the now
    /// retired key is authenticated again instead of "cannot be verified".
    #[test]
    fn rotation_markers_reach_every_verifying_case_lane() {
        let case = std::env::temp_dir().join(format!("ft-r7-i6-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case);
        fs::create_dir_all(case.join("evidence/logs")).unwrap();
        let old = AuditKey::new("old", &[0x61; 32]);
        let signoff = case.join(crate::report::SIGNOFF_LOG_REL_PATH);
        audit::append_chained_jsonl_keyed(&signoff, r#"{"event":"signoff"}"#, Some(&old)).unwrap();
        let scan = case.join("evidence/logs/scan-log.jsonl");
        audit::append_chained_jsonl_keyed(&scan, r#"{"kind":"one"}"#, Some(&old)).unwrap();
        let damaged = case.join("evidence/logs/anomaly-log.jsonl");
        fs::write(&damaged, b"{\"torn\":").unwrap();
        let damaged_before = fs::read(&damaged).unwrap();

        let (lanes, skipped) = super::rotation_marker_lanes(&case).unwrap();
        assert!(lanes.contains(&signoff), "{lanes:?}");
        assert!(lanes.contains(&scan), "{lanes:?}");
        assert!(!lanes.contains(&damaged), "{lanes:?}");
        assert!(
            skipped.iter().any(|s| s.contains("anomaly-log")),
            "{skipped:?}"
        );
        assert!(super::rotation_marker_lanes(&case.join("evidence")).is_err());

        // After rotation `old` is retired; the marker under the new key
        // proves the earlier entry's age.
        let retired = old.clone().retired_at(100);
        let new = AuditKey::new("new", &[0x62; 32]);
        let keys = [new.clone(), retired];
        let before = audit::verify_chained_jsonl_keyed(&signoff, &keys).unwrap();
        assert_eq!(before.entry_auth, [audit::EntryAuth::RetiredUnproven]);
        for lane in &lanes {
            audit::append_chained_jsonl_keyed(
                lane,
                r#"{"kind":"audit-key-rotate","from":"old","to":"new"}"#,
                Some(&new),
            )
            .unwrap();
        }
        let after = audit::verify_chained_jsonl_keyed(&signoff, &keys).unwrap();
        assert_eq!(after.entry_auth[0], audit::EntryAuth::Authenticated);
        assert_eq!(fs::read(&damaged).unwrap(), damaged_before);
        let _ = fs::remove_dir_all(&case);
    }
}
