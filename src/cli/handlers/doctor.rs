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
    // case's chained lanes instead of dying on the directory read. R9 P4:
    // the WHOLE case (`audit::case_lane_paths` — artifacts/** lanes too),
    // R8 only checked evidence/logs.
    if log_path.is_dir() {
        let logs = audit::case_lane_paths(log_path);
        if logs.is_empty() {
            return Err(format!(
                "no audit .jsonl logs found under {} (evidence/logs, artifacts)",
                log_path.display()
            ));
        }
        let mut failures = Vec::new();
        let mut not_verified = Vec::new();
        for log in &logs {
            match audit::verify_chained_jsonl(log) {
                Ok(result) => {
                    // R8 P0/P2: with audit keys on this PC, a lane holding
                    // any line not authenticated here is NOT VERIFIED.
                    let trust = result.lane_trust();
                    if trust.passes() {
                        println!(
                            "audit verified: {} (entries: {}, integrity: {}){}",
                            log.display(),
                            result.entries,
                            trust.label(),
                            if trust.reason().is_empty() {
                                String::new()
                            } else {
                                format!(" — {}", trust.reason())
                            }
                        );
                        print_lane_repairs(log);
                    } else {
                        println!(
                            "audit NOT VERIFIED: {} (entries: {}): {}",
                            log.display(),
                            result.entries,
                            trust.reason()
                        );
                        not_verified.push(log.display().to_string());
                    }
                }
                // R9 P5/I9: an unreadable key configuration is NOT
                // VERIFIED (fail closed), never a FAIL / tamper verdict.
                Err(err) if audit::is_key_config_error(&err) => {
                    println!("audit NOT VERIFIED: {}: {err}", log.display());
                    not_verified.push(log.display().to_string());
                }
                Err(err) => {
                    println!("audit FAILED: {}: {err}", log.display());
                    println!("hint: {}", audit::REPAIR_LANE_HINT);
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
                "{} audit log(s) could not be verified on this PC (NOT VERIFIED — lines this PC cannot authenticate; not a tampering verdict — check lines signed under another PC's key on the PC holding that key)",
                not_verified.len()
            ));
        }
        return Ok(());
    }
    let result = audit::verify_chained_jsonl(log_path).map_err(|err| {
        if audit::is_key_config_error(&err) {
            println!("audit NOT VERIFIED: {}", log_path.display());
            format!(
                "audit log {} could not be verified on this PC: {err}",
                log_path.display()
            )
        } else if err.contains("repair-lane") {
            err
        } else {
            format!("{err} — {}", audit::REPAIR_LANE_HINT)
        }
    })?;
    let trust = result.lane_trust();
    if !trust.passes() {
        println!("audit NOT VERIFIED: {}", log_path.display());
        println!("entries: {}", result.entries);
        for warning in &result.warnings {
            println!("warning: {warning}");
        }
        return Err(format!(
            "audit log {} could not be verified on this PC: {}",
            log_path.display(),
            trust.reason()
        ));
    }
    println!("audit verified: {}", log_path.display());
    println!("entries: {}", result.entries);
    println!("last entry sha256: {}", result.last_entry_sha256);
    println!("integrity: {}", trust.label());
    if !trust.reason().is_empty() {
        println!("trust: {}", trust.reason());
    }
    if result.keyed_entries > 0 {
        println!(
            "keyed entries: {} ({} unauthenticated)",
            result.keyed_entries, result.unauthenticated_keyed_entries
        );
    }
    for warning in &result.warnings {
        println!("warning: {warning}");
    }
    print_lane_repairs(log_path);
    Ok(())
}

/// R12-I3: prints the "수리됨" line of every repair recorded in the lane
/// (`verify-audit` output). R13-I5: and the state of the `.removed`
/// archive it names — hashed against the marker: `removed-archive:
/// match|missing|differs` (a `missing` / `differs` archive is also a
/// `warning:` line; it does not change the exit code — the lane's own
/// chain decides that).
fn print_lane_repairs(log: &Path) {
    if let Ok(rows) = audit::read_lane_rows(log) {
        for repair in &rows.repairs {
            println!("repaired: {} — {}", log.display(), repair.describe());
            let archive = repair.removed_archive;
            println!(
                "removed-archive: {} {} — {}",
                archive.as_str(),
                repair.removed_saved_as.as_deref().unwrap_or("(이름 없음)"),
                archive.describe()
            );
            if archive != audit::RemovedArchive::Match {
                println!(
                    "warning: 수리 보관 파일 {} {} — 레인 {}번째 줄의 lane-repair 표식(sha256 {})과 맞지 않음",
                    repair.removed_saved_as.as_deref().unwrap_or("(이름 없음)"),
                    archive.as_str(),
                    repair.index + 1,
                    repair.removed_sha256
                );
            }
        }
    }
}

/// `repair-lane` (R12-I3): [`audit::repair_lane`] with a report of what
/// was cut and where it was kept.
pub fn repair_lane(log_path: &Path, dry_run: bool) -> Result<(), String> {
    let outcome = audit::repair_lane(log_path, dry_run)?;
    if !outcome.needed {
        println!(
            "lane verifies — nothing to repair: {} (entries: {})",
            log_path.display(),
            outcome.kept_entries
        );
        return Ok(());
    }
    println!(
        "{}: {}",
        if dry_run {
            "would repair (dry run, nothing changed)"
        } else {
            "lane repaired (수리됨)"
        },
        log_path.display()
    );
    println!(
        "kept: {} entr{} ({} bytes)",
        outcome.kept_entries,
        if outcome.kept_entries == 1 {
            "y"
        } else {
            "ies"
        },
        outcome.kept_bytes
    );
    println!(
        "cut: {} line(s), {} bytes, sha256 {}",
        outcome.removed_lines, outcome.removed_bytes, outcome.removed_sha256
    );
    if let Some(failure) = &outcome.failure {
        println!("reason: {failure}");
    }
    if let Some(saved) = &outcome.removed_saved_to {
        println!("cut bytes kept in: {}", saved.display());
        println!(
            "marker: {} appended ({})",
            audit::LANE_REPAIR_EVENT,
            if outcome.keyed {
                "HMAC-signed with this PC's audit key"
            } else {
                "unkeyed — no audit key configured"
            }
        );
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
///
/// R10-I9: `init` allows creating a new keyring at a missing explicit
/// `FRAMETRACE_AUDIT_KEYRING_FILE` path ([`crate::audit_key::rotate`]).
pub fn rotate_audit_key(
    key_id: Option<&str>,
    marker_logs: &[PathBuf],
    init: bool,
) -> Result<(), String> {
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
    let targets = dedup_lanes(targets);
    // R8-I8: which lanes hold entries but no keyed entry (before rotating).
    // R10-I9: decided per CASE, not per lane — a lane with no HMAC in a
    // case where another lane holds HMAC entries was keyed and stripped
    // (or continued without the key), never "written before keying".
    let (keyless, refused) = introduction_candidates(&targets);
    let rotation = crate::audit_key::rotate(key_id, init)?;
    if rotation.previous_id.is_none() {
        for (lane, _) in targets
            .iter()
            .zip(&refused)
            .filter(|(_, refused)| **refused)
        {
            println!(
                "key introduction refused: {} — another lane of this case holds HMAC entries, so this lane's unkeyed entries are not a pre-key prefix (it gets the rotation marker; its unkeyed entries stay NOT VERIFIED where keys are held) (키 도입 거부: 같은 케이스의 다른 레인에 HMAC 항목이 있음)",
                lane.display()
            );
        }
    }
    println!("audit key rotated");
    println!("new key id: {}", rotation.new_key.id);
    match &rotation.previous_id {
        Some(id) => {
            println!("retired key id: {id} (kept in the keyring — its entries still verify)")
        }
        None => println!("no previous key configured — audit keying enabled"),
    }
    println!("keyring: {}", rotation.keyring_path.display());
    // R8-I8: without a case folder, no case gets a marker — every entry the
    // retired key signed then reads "retired key, not proven" (and, from
    // now on, every unauthenticated line of every case is not trusted on
    // this PC).
    if !marker_logs.iter().any(|log| log.is_dir()) {
        eprintln!(
            "warning: no case folder was given with --log — no case's lanes got the rotation marker. Entries signed by the retired key are authenticated only when a newer key's entry follows them, so existing sign-offs and measurements now read \"cannot be verified\" (re-finalize / re-measure them); when rotating, pass every case folder you keep working on: rotate-audit-key --log <case folder> (경고: --log <케이스 폴더> 없이 회전함 — 케이스 레인에 회전 표식이 없습니다)"
        );
    }
    for note in &skipped {
        println!("marker skipped: {note}");
    }
    let failed = append_rotation_markers(
        &targets,
        &keyless,
        rotation.previous_id.as_deref(),
        &rotation.new_key,
    );
    if !failed.is_empty() {
        return Err(format!(
            "the audit key WAS rotated (new key id {}), but the rotation marker could not be appended to {} lane(s): {} — their retired-key entries stay unproven until a newer key's entry follows them",
            rotation.new_key.id,
            failed.len(),
            failed.join("; ")
        ));
    }
    Ok(())
}

/// Appends the rotation marker to every target (signed under the NEW key
/// explicitly — the design's rotation rule — even if a --key-source
/// override still names the old key for this process). Returns the
/// failures.
///
/// R8-I10: one lane failing does not stop the others (the keyring is
/// already rotated); every failure is reported.
///
/// R8-I8: a lane that held entries but no keyed entry (`keyless`), on a PC
/// that had no key before this rotation (`previous_id` none), gets a
/// key-INTRODUCTION marker instead: its earlier entries were written
/// before keying (self-consistency only, never authenticated) — the case
/// is not flipped to "every earlier line unauthenticated". A PC that
/// already held a key never introduces keying (a lane stripped of its
/// HMACs stays NOT VERIFIED there).
fn append_rotation_markers(
    targets: &[PathBuf],
    keyless: &[bool],
    previous_id: Option<&str>,
    new_key: &crate::audit_key::AuditKey,
) -> Vec<String> {
    let mut failed: Vec<String> = Vec::new();
    for (log, keyless) in targets.iter().zip(keyless) {
        let marker = if *keyless && previous_id.is_none() {
            format!(
                "{{\"kind\":\"{}\",\"to\":{},\"note\":\"entries before this marker were written before audit keying was introduced for this lane on this PC: self-consistency only, not authenticated\"}}",
                audit::KEY_INTRODUCED_KIND,
                audit::optional_string(Some(&new_key.id)),
            )
        } else {
            format!(
                "{{\"kind\":\"audit-key-rotate\",\"from\":{},\"to\":{}}}",
                audit::optional_string(previous_id),
                audit::optional_string(Some(&new_key.id)),
            )
        };
        match audit::append_chained_jsonl_keyed(log, &marker, Some(new_key)) {
            Ok(()) => println!("marker appended: {}", log.display()),
            Err(err) => {
                println!("marker FAILED: {}: {err}", log.display());
                failed.push(format!("{}: {err}", log.display()));
            }
        }
    }
    failed
}

/// R8-I10: the marker targets without duplicates — the same lane named
/// twice (a case folder and one of its lanes, `case` and `case/`, another
/// spelling of the path) gets ONE marker. Order is kept.
fn dedup_lanes(targets: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::BTreeSet::new();
    targets
        .into_iter()
        .filter(|lane| {
            let key = match (lane.parent().map(fs::canonicalize), lane.file_name()) {
                (Some(Ok(parent)), Some(name)) => parent.join(name),
                _ => lane.clone(),
            };
            seen.insert(key)
        })
        .collect()
}

/// R8-I8: the lane holds entries and none of them carries an HMAC.
fn lane_is_keyless(lane: &Path) -> bool {
    // Structure only (no keys): does any entry carry an HMAC?
    audit::read_lane_rows_with_keys(lane, &[])
        .ok()
        .filter(|rows| rows.failure.is_none())
        .and_then(|rows| rows.verification)
        .is_some_and(|result| result.entries > 0 && result.keyed_entries == 0)
}

/// R8-I8 / R10-I9: per target, whether it may get a key-INTRODUCTION
/// marker (holds entries, none keyed, and no other lane of its case holds
/// HMAC entries), and whether it was refused only because of that case
/// rule.
fn introduction_candidates(targets: &[PathBuf]) -> (Vec<bool>, Vec<bool>) {
    let mut keyless = Vec::with_capacity(targets.len());
    let mut refused = Vec::with_capacity(targets.len());
    for lane in targets {
        let lane_keyless = lane_is_keyless(lane);
        let case_keyed = lane_keyless && case_holds_keyed_lane(lane);
        keyless.push(lane_keyless && !case_keyed);
        refused.push(case_keyed);
    }
    (keyless, refused)
}

/// R10-I9: whether another chained lane of the case `lane` belongs to
/// holds an HMAC-signed entry (structure only — no keys needed). A lane
/// that is not under a case root counts as holding none.
fn case_holds_keyed_lane(lane: &Path) -> bool {
    let Some(binding) = audit::lane_binding_of(lane) else {
        return false;
    };
    let me = audit::lane_rel_key(&binding.rel);
    audit::case_lane_paths(&binding.root)
        .iter()
        .filter(|path| {
            path.strip_prefix(&binding.root)
                .map(|rel| audit::lane_rel_key(&rel.to_string_lossy()) != me)
                .unwrap_or(true)
        })
        .any(|path| {
            audit::read_lane_rows_with_keys(path, &[])
                .ok()
                .and_then(|rows| rows.verification)
                .is_some_and(|verification| verification.keyed_entries > 0)
        })
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
            format!(
                "활성 키 '{}' — HMAC 서명 감사 사용 · 이 PC에서 인증되지 않은 감사 항목은 신뢰하지 않음",
                key.id
            ),
        )),
        Ok(None) => checks.push(DoctorCheck::warn(
            "audit keyring",
            match crate::audit_key::key_location_warning() {
                // R10-I15: no config directory — default key locations
                // were not even looked at.
                Some(warning) => format!(
                    "키 없음 — {warning} · 감사 로그는 구조만 확인, 모든 확인 결과는 자기 일관성만(인증 아님)"
                ),
                None => "키 없음 — 감사 로그는 구조만 확인, 모든 확인 결과는 자기 일관성만(인증 아님)"
                    .into(),
            },
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

        // R9 P4: every chained lane of the case (`audit::case_lane_paths` —
        // all of artifacts/**, not three folders).
        {
            for path in audit::case_lane_paths(case_dir) {
                let name = format!(
                    "audit log {}",
                    path.strip_prefix(case_dir).unwrap_or(&path).display()
                );
                match audit::verify_chained_jsonl(&path) {
                    Ok(result) => {
                        // R8-I10: the label is the lane's trust under this
                        // PC's policy (R7 printed `integrity-structural-only`
                        // for a NOT VERIFIED lane).
                        let trust = result.lane_trust();
                        let detail = format!(
                            "{}건 · {}{}{}",
                            result.entries,
                            trust.label(),
                            if trust.reason().is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", trust.reason())
                            },
                            if result.warnings.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", result.warnings.join("; "))
                            }
                        );
                        checks.push(if trust == audit::LaneTrust::Authenticated {
                            DoctorCheck::ok(&name, detail)
                        } else {
                            DoctorCheck::warn(&name, detail)
                        });
                    }
                    // R9 P5/I9: not a broken chain — the key configuration.
                    Err(err) if audit::is_key_config_error(&err) => {
                        checks.push(DoctorCheck::warn(&name, err))
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

    /// R10-I9: key introduction is decided per CASE — a lane without any
    /// HMAC in a case where another lane holds HMAC entries was stripped
    /// (or continued without the key), so it never gets the introduction
    /// marker that would make its entries a "pre-key" prefix. R8/R9
    /// decided per lane.
    #[test]
    fn r10_i9_key_introduction_refused_when_the_case_was_keyed() {
        let case = std::env::temp_dir().join(format!("ft-r10-i9-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case);
        fs::create_dir_all(case.join("evidence/logs")).unwrap();
        fs::write(case.join("case.json"), r#"{"case_id":"C-i9"}"#).unwrap();
        let stripped = case.join("evidence/logs/scan-log.jsonl");
        audit::append_chained_jsonl_keyed(&stripped, r#"{"n":1}"#, None).unwrap();
        // Only unkeyed lanes: introduction allowed.
        let (keyless, refused) = super::introduction_candidates(std::slice::from_ref(&stripped));
        assert_eq!((keyless, refused), (vec![true], vec![false]));
        // Another lane of the case carries an HMAC: refused.
        let other = AuditKey::new("other", &[0x5a; 32]);
        audit::append_chained_jsonl_keyed(
            &case.join("evidence/logs/validation-log.jsonl"),
            r#"{"n":1}"#,
            Some(&other),
        )
        .unwrap();
        let (keyless, refused) = super::introduction_candidates(std::slice::from_ref(&stripped));
        assert_eq!((keyless, refused), (vec![false], vec![true]));
        let _ = fs::remove_dir_all(&case);
    }

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

    fn r8_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ft-r8-doctor-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("evidence/logs")).unwrap();
        dir
    }

    /// R8 P0/P2 for `verify-audit`: a lane stripped of its HMACs was
    /// "audit verified" (exit 0) on a PC holding the key in R7; now it is
    /// NOT VERIFIED (non-zero). A lane continued on another PC names that
    /// PC's key and is not "FAILED". Without keys: verified, with the
    /// self-consistency caveat.
    #[test]
    fn r8_verify_audit_trust_comes_from_keys_held() {
        let case = r8_dir("verify");
        let lane = case.join("evidence/logs/scan-log.jsonl");
        audit::append_chained_jsonl_keyed(&lane, r#"{"n":1}"#, None).unwrap();
        let mine = AuditKey::new("pc-a", &[0xf1; 32]);
        let other = AuditKey::new("pc-b", &[0xf2; 32]);
        crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
            let err = super::verify_audit(&lane).unwrap_err();
            assert!(err.contains("could not be verified on this PC"), "{err}");
            assert!(super::verify_audit(&case).is_err());
        });
        crate::audit_key::with_test_keys(&[], || {
            assert!(super::verify_audit(&lane).is_ok());
            assert!(super::verify_audit(&case).is_ok());
        });
        let multi = case.join("evidence/logs/anomaly-log.jsonl");
        audit::append_chained_jsonl_keyed(&multi, r#"{"n":1}"#, Some(&mine)).unwrap();
        audit::append_chained_jsonl_keyed(&multi, r#"{"n":2}"#, Some(&other)).unwrap();
        crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
            let err = super::verify_audit(&multi).unwrap_err();
            assert!(err.contains("\"pc-b\" 보유 PC에서 확인"), "{err}");
            assert!(!err.contains("failed verification"), "{err}");
        });
        let _ = fs::remove_dir_all(&case);
    }

    /// R8-I10: the same lane named twice (a case folder and one of its
    /// lanes, another spelling of the path) gets ONE marker, and one lane
    /// failing does not stop the others — every failure is reported.
    #[test]
    fn r8_i10_rotation_markers_dedup_and_survive_a_failing_lane() {
        let case = r8_dir("rotate");
        let lane = case.join("evidence/logs/scan-log.jsonl");
        audit::append_chained_jsonl_keyed(&lane, r#"{"n":1}"#, None).unwrap();
        let twin = case.join("evidence/./logs/scan-log.jsonl");
        let targets = super::dedup_lanes(vec![lane.clone(), twin, lane.clone()]);
        assert_eq!(targets.len(), 1, "{targets:?}");
        // A lane whose parent is a FILE cannot be appended to.
        let blocker = case.join("blocker");
        fs::write(&blocker, b"x").unwrap();
        let bad = blocker.join("x.jsonl");
        let new = AuditKey::new("new", &[0xf3; 32]);
        let failed = super::append_rotation_markers(
            &[bad.clone(), lane.clone()],
            &[false, false],
            Some("old"),
            &new,
        );
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].contains("x.jsonl"), "{failed:?}");
        let text = fs::read_to_string(&lane).unwrap();
        assert!(text.contains("audit-key-rotate"), "{text}");
        let _ = fs::remove_dir_all(&case);
    }

    /// R8-I8: enabling keying (no previous key) on a lane that held only
    /// unkeyed entries stamps a key-INTRODUCTION marker, so the earlier
    /// entries read self-consistency only instead of "not authenticated";
    /// a PC that already held a key stamps the ordinary rotation marker
    /// (a stripped lane is never "introduced" there).
    #[test]
    fn r8_i8_first_keying_introduces_instead_of_flipping_the_lane() {
        let case = r8_dir("introduce");
        let first = case.join("evidence/logs/scan-log.jsonl");
        let second = case.join("evidence/logs/anomaly-log.jsonl");
        for lane in [&first, &second] {
            audit::append_chained_jsonl_keyed(lane, r#"{"n":1}"#, None).unwrap();
        }
        assert!(super::lane_is_keyless(&first));
        let new = AuditKey::new("new", &[0xf4; 32]);
        assert!(
            super::append_rotation_markers(std::slice::from_ref(&first), &[true], None, &new)
                .is_empty()
        );
        assert!(
            super::append_rotation_markers(
                std::slice::from_ref(&second),
                &[true],
                Some("old"),
                &new
            )
            .is_empty()
        );
        let keys = std::slice::from_ref(&new);
        let introduced = audit::verify_chained_jsonl_keyed(&first, keys).unwrap();
        assert_eq!(introduced.pre_key_entries, 1);
        assert!(introduced.lane_trust().passes());
        assert!(!super::lane_is_keyless(&first));
        let rotated = audit::verify_chained_jsonl_keyed(&second, keys).unwrap();
        assert_eq!(rotated.pre_key_entries, 0);
        assert!(!rotated.lane_trust().passes());
        let _ = fs::remove_dir_all(&case);
    }

    /// R9-I2/P4: `verify-audit <case>` covers every lane of the case — a
    /// tampered artifacts/** lane fails it (R8 looked at evidence/logs only
    /// and passed).
    #[test]
    fn r9_i2_verify_audit_case_covers_artifact_lanes() {
        let case = std::env::temp_dir().join(format!("ft-doctor-r9-i2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&case);
        let scan = case.join("evidence/logs/scan-log.jsonl");
        let carve = case.join("artifacts/carved/carve-log.jsonl");
        audit::append_chained_jsonl_keyed(&scan, r#"{"n":1}"#, None).unwrap();
        audit::append_chained_jsonl_keyed(&carve, r#"{"id":"c1","sha256":"aa"}"#, None).unwrap();
        crate::audit_key::with_test_keys(&[], || {
            super::verify_audit(&case).unwrap();
            let text = fs::read_to_string(&carve).unwrap();
            fs::write(&carve, text.replace("\"aa\"", "\"bb\"")).unwrap();
            let err = super::verify_audit(&case).unwrap_err();
            assert!(err.contains("failed verification"), "{err}");
        });
        let _ = fs::remove_dir_all(&case);
    }
}
