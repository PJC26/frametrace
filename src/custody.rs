//! Chain-of-custody statement generator.
//!
//! Produces `reports/custody-statement.html` — a print-friendly document
//! an examiner can hand to opposing counsel or a court together with the
//! case package. It records the case identity, every registered evidence
//! source (with acquisition metadata and what was observed on disk at
//! generation time), the full job ledger, and the verification status of
//! each chained audit log — re-verified at generation time, never restated
//! from a cached claim.
//!
//! The statement only claims what it actually checked: audit lanes are
//! re-verified, evidence sources are re-stat'ed (presence/size), but
//! original evidence is NOT re-hashed here, so the wording never asserts
//! that originals are unmodified. A database that cannot be read is shown
//! as an error, never as "no sources".
//!
//! The statement deliberately distinguishes original evidence from
//! derived work products (proxies, clips, thumbnails, carved candidates)
//! so nobody can read a derived artifact as an authenticated original.

use crate::audit;
use crate::case_db;
use crate::util::{html_escape as esc, now_unix, read_to_string, write_text_atomic};
use std::path::{Path, PathBuf};

pub const CUSTODY_REL_PATH: &str = "reports/custody-statement.html";

/// Directories whose `*-log.jsonl` files are chained audit lanes worth
/// verifying in the statement. Reviewer data files (videos.jsonl,
/// timeline.jsonl) are index formats, not audit chains, and stay out.
const AUDIT_LOG_DIRS: &[&str] = &["artifacts", "evidence/logs"];

/// Generates (or refreshes) `reports/custody-statement.html`.
/// Returns the written path.
pub fn generate(case_dir: &Path) -> Result<PathBuf, String> {
    let html = render_statement(case_dir, false)?;
    let out = case_dir.join(CUSTODY_REL_PATH);
    write_text_atomic(&out, &html)
        .map_err(|err| format!("failed to write custody statement {}: {err}", out.display()))?;
    Ok(out)
}

/// Renders the statement without writing it. With `redact_paths` (I4, a
/// path-redacted package) every path-bearing value — the case directory,
/// source paths, job subjects, error texts — goes through
/// [`crate::redact::Redactor`], and the page carries the redaction marker
/// and note. The live checks (re-stat, chain verification) still run on the
/// real paths; only what is printed is rewritten.
pub fn render_statement(case_dir: &Path, redact_paths: bool) -> Result<String, String> {
    if !case_dir.join("case.json").is_file() {
        return Err(format!(
            "not a FrameTrace case directory: {}",
            case_dir.display()
        ));
    }
    let generated_unix = now_unix()?;
    let manifest = read_to_string(&case_dir.join("case.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .unwrap_or(serde_json::Value::Null);
    let redactor = redact_paths.then(|| crate::redact::Redactor::new(case_dir));
    let sources = load_evidence_sources(case_dir);
    let jobs = load_jobs(case_dir);
    let coverage = load_hash_coverage(case_dir);
    let logs = verify_audit_logs(case_dir);
    let hash_verify_html = render_hash_verify(case_dir, redactor.as_ref());
    Ok(render(
        case_dir,
        &manifest,
        &sources,
        &jobs,
        &coverage,
        &logs,
        &hash_verify_html,
        generated_unix as i64,
        redactor.as_ref(),
    ))
}

/// HTML-escapes `text`, path-redacting it first when a redactor is given.
fn esc_with(redactor: Option<&crate::redact::Redactor>, text: &str) -> String {
    match redactor {
        Some(redactor) => esc(&redactor.text(text)),
        None => esc(text),
    }
}

/// `YYYY-MM-DD HH:MM:SS` in UTC (the statement's time basis), `—` for an
/// unset time. Shared with the case report so both documents print the
/// same instant the same way (I6).
pub fn utc_timestamp(unix: i64) -> String {
    ts(unix).trim_end_matches('Z').to_string()
}

struct SourceRow {
    source_id: String,
    kind: String,
    path: String,
    registered_unix: i64,
    write_protect: Option<String>,
    acquisition_tool: Option<String>,
    evidence_hash: Option<String>,
    /// `evidence_hash_algorithm` (schema v6; `None` on older databases).
    hash_algorithm: Option<String>,
    /// `evidence_hash_origin`: "computed" / "operator-supplied" (schema
    /// v6; `None` = not recorded).
    hash_origin: Option<String>,
    /// What a re-stat of `path` observed at statement generation time.
    observed: String,
}

/// Indexed-file hash coverage from the `videos` table.
struct HashCoverage {
    total: i64,
    hashed: i64,
    errors: i64,
}

struct JobRow {
    job_id: String,
    job_type: String,
    status: String,
    subject_path: String,
    started_unix: i64,
    completed_unix: Option<i64>,
    total_units: Option<i64>,
    completed_units: i64,
    error: Option<String>,
}

struct LogRow {
    rel_path: String,
    /// Ok(verification) when the log parsed and chain-checked, Err(text)
    /// when the lane could not be verified — printed verbatim so a broken
    /// lane is disclosed, never hidden.
    result: Result<audit::AuditChainVerification, String>,
}

/// Opens the case DB read-only, distinguishing "no database" from
/// "database unreadable" so neither renders as an empty table.
fn open_case_db_for_statement(case_dir: &Path) -> Result<rusqlite::Connection, String> {
    let db = case_db::case_db_path(case_dir);
    if !db.is_file() {
        return Err(format!("case database not found: {}", db.display()));
    }
    case_db::open_readonly_case_db(&db)
}

fn observe_path(path: &str) -> String {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => format!("present (file, {} bytes)", meta.len()),
        Ok(meta) if meta.is_dir() => "present (directory)".to_string(),
        Ok(_) => "present (other)".to_string(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            "not reachable at statement time".to_string()
        }
        Err(err) => format!("not readable: {err}"),
    }
}

fn load_evidence_sources(case_dir: &Path) -> Result<Vec<SourceRow>, String> {
    let conn = open_case_db_for_statement(case_dir)?;
    // Read-only connections skip init_schema: a pre-v6 database has no
    // algorithm/origin columns — read NULLs (shown as "origin not
    // recorded") instead of failing the statement.
    let has = |col: &str| case_db::table_has_column(&conn, "evidence_sources", col);
    let algo_col = if has("evidence_hash_algorithm")? {
        "evidence_hash_algorithm"
    } else {
        "NULL"
    };
    let origin_col = if has("evidence_hash_origin")? {
        "evidence_hash_origin"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT source_id, kind, path, registered_unix, write_protect,
                acquisition_tool, evidence_hash, {algo_col}, {origin_col}
         FROM evidence_sources ORDER BY registered_unix ASC"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|err| format!("failed to query evidence sources: {err}"))?;
    let rows = stmt
        .query_map([], |row| {
            let path: String = row.get(2)?;
            Ok(SourceRow {
                source_id: row.get(0)?,
                kind: row.get(1)?,
                observed: observe_path(&path),
                path,
                registered_unix: row.get(3)?,
                write_protect: row.get(4)?,
                acquisition_tool: row.get(5)?,
                evidence_hash: row.get(6)?,
                hash_algorithm: row.get(7)?,
                hash_origin: row.get(8)?,
            })
        })
        .map_err(|err| format!("failed to query evidence sources: {err}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("failed to read evidence source row: {err}"))
}

/// The evidence-hash cell: the value plus where it came from (FOR-04), so
/// an operator-typed hash never reads as one FrameTrace computed.
fn evidence_hash_cell(s: &SourceRow) -> String {
    let Some(hash) = s.evidence_hash.as_deref().filter(|h| !h.is_empty()) else {
        return "—".to_string();
    };
    let class = if s.hash_origin.as_deref() == Some("computed") {
        "ok"
    } else {
        "warn"
    };
    let algo = s
        .hash_algorithm
        .as_deref()
        .map(|a| format!("{}:", esc(a)))
        .unwrap_or_default();
    format!(
        "{algo}{}<br><span class={class}>{}</span>",
        esc(hash),
        esc(case_db::describe_hash_origin(s.hash_origin.as_deref()))
    )
}

fn load_jobs(case_dir: &Path) -> Result<Vec<JobRow>, String> {
    let conn = open_case_db_for_statement(case_dir)?;
    let mut stmt = conn
        .prepare(
            "SELECT job_id, job_type, status, subject_path, started_unix,
                completed_unix, total_units, completed_units, error
         FROM jobs ORDER BY started_unix ASC",
        )
        .map_err(|err| format!("failed to query jobs: {err}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(JobRow {
                job_id: row.get(0)?,
                job_type: row.get(1)?,
                status: row.get(2)?,
                subject_path: row.get(3)?,
                started_unix: row.get(4)?,
                completed_unix: row.get(5)?,
                total_units: row.get(6)?,
                completed_units: row.get(7)?,
                error: row.get(8)?,
            })
        })
        .map_err(|err| format!("failed to query jobs: {err}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("failed to read job row: {err}"))
}

fn load_hash_coverage(case_dir: &Path) -> Result<HashCoverage, String> {
    let conn = open_case_db_for_statement(case_dir)?;
    conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN sha256 IS NOT NULL AND sha256 <> '' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN hash_status IN ('error', 'changed-during-hash') THEN 1 ELSE 0 END), 0)
         FROM videos",
        [],
        |row| {
            Ok(HashCoverage {
                total: row.get(0)?,
                hashed: row.get(1)?,
                errors: row.get(2)?,
            })
        },
    )
    .map_err(|err| format!("failed to read indexed file hash coverage: {err}"))
}

fn verify_audit_logs(case_dir: &Path) -> Vec<LogRow> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for rel_dir in AUDIT_LOG_DIRS {
        collect_log_files(&case_dir.join(rel_dir), &mut paths, 0);
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| LogRow {
            rel_path: path
                .strip_prefix(case_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/"),
            result: audit::verify_chained_jsonl(&path),
        })
        .collect()
}

/// FOR-03: renders the "recorded vs observed" table from the newest run in
/// `evidence/logs/hash-verify.jsonl` (written by `qa anomalies`, which the
/// workstation's 원본 재검증 button runs). The statement itself never
/// hashes anything; when no run exists it says so instead of implying the
/// sources were verified.
///
/// R11 P9: the decision is [`crate::anomaly::hash_verify_assessment`]'s —
/// a missing (unusable) row is not good news: no green banner for a run
/// that is not `verified`, and no older run as "latest" when a newer one
/// cannot be authenticated (banner states: see `hash_verify_banner`).
fn render_hash_verify(case_dir: &Path, redactor: Option<&crate::redact::Redactor>) -> String {
    let esc = |text: &str| esc_with(redactor, text);
    // D11: an unreadable log is an error, never "no run recorded".
    let assessment = match crate::anomaly::hash_verify_assessment(case_dir) {
        Ok(assessment) => assessment,
        Err(err) => {
            return format!(
                "<p class=\"note bad\">The live source re-hash log could not be read — re-verification status unknown: {}</p>",
                esc(&err)
            );
        }
    };
    let lane = &assessment.lane;
    // D4: runs are grouped by run id; only the newest COMPLETED run is the
    // result, and a newer run without its completion marker is disclosed
    // as interrupted instead of being mixed into (or shown as) the result.
    // R13 P16: such a run WITHHOLDS the result (its marker may have been
    // deleted — no key is needed for that), and its usable MISMATCH rows
    // are listed here, never hidden.
    let mut interrupted_html = assessment
        .newer_runs()
        .map(|run| {
            let mismatched = run.mismatches().count();
            format!(
                "<p class=\"note {}\">Run {} started {} has no completion marker ({} record(s) logged{}; 표식 없는 더 새 실행): its marker may have been deleted or the run was cut short — it is not a result, and it means NO earlier run is the current result.</p>\n",
                if mismatched > 0 { "bad" } else { "warn" },
                esc(&run.run_id),
                ts(run.run_unix),
                run.entries.len(),
                if mismatched > 0 {
                    format!(", {mismatched} MISMATCH")
                } else {
                    String::new()
                }
            )
        })
        .collect::<String>();
    let newer_mismatches = assessment.newer_mismatches();
    if !newer_mismatches.is_empty() {
        let rows = newer_mismatches
            .iter()
            .map(|row| {
                let dash = |value: &Option<String>| {
                    value
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .map(esc)
                        .unwrap_or_else(|| "—".to_string())
                };
                format!(
                    "<tr><td>{}</td><td>{}</td><td class=path>{}</td><td class=hash>{}</td><td class=hash>{}</td><td class=bad>MISMATCH</td></tr>\n",
                    esc(&row.run_id),
                    esc(&row.id),
                    esc(&row.source_path),
                    dash(&row.recorded_sha256),
                    dash(&row.observed_sha256),
                )
            })
            .collect::<String>();
        interrupted_html.push_str(&format!(
            "<p class=\"banner bad\">{} MISMATCH row(s) recorded by a re-hash run without a completion marker — shown here because no completed run may stand in for it.</p>\n<table><tr><th>Run</th><th>Record</th><th>Path</th><th>Recorded SHA-256</th><th>Observed SHA-256 (live)</th><th>Result</th></tr>\n{rows}</table>\n",
            newer_mismatches.len()
        ));
    }
    let unauthenticated = assessment.unauthenticated;
    let unauth_html = if unauthenticated > 0 {
        format!(
            "<p class=\"note warn\">{unauthenticated} re-hash log entr{} cannot be authenticated on this PC (not HMAC-authenticated here while this PC holds audit keys, signed under another PC's key, superseded by a newer entry this PC cannot authenticate, or in a log whose chain does not verify) — NOT used as re-verification results anywhere below, and a record missing for that reason is NOT counted as a match (unauthenticated; not a tampering verdict for another PC's key: check it on the PC holding that key).</p>\n",
            if unauthenticated == 1 { "y" } else { "ies" }
        )
    } else {
        String::new()
    };
    // A chain that does not verify: say so (nothing from it is used).
    let broken_html = if lane.failure.is_some() {
        "<p class=\"banner bad\">The live re-hash log (hash-verify.jsonl) does not verify as a chain — no re-verification result from it is relied on.</p>\n".to_string()
    } else {
        String::new()
    };
    // R14 I3: the sign-off lane's `hash-verify-head` against this lane —
    // a tail cut (even two lines, no key) is said here, never hidden.
    let head = &assessment.head;
    let head_html = match head {
        crate::report::HashVerifyHeadCheck::Truncated { .. }
        | crate::report::HashVerifyHeadCheck::Rewritten { .. } => format!(
            "<p class=\"banner bad\">Re-verification lane anchor: {}.</p>\n",
            esc(&head.describe())
        ),
        crate::report::HashVerifyHeadCheck::Unknown { .. }
        | crate::report::HashVerifyHeadCheck::NotAuthenticated(_) => format!(
            "<p class=\"note warn\">Re-verification lane anchor: {}.</p>\n",
            esc(&head.describe())
        ),
        crate::report::HashVerifyHeadCheck::Intact { .. }
        | crate::report::HashVerifyHeadCheck::Absent => format!(
            "<p class=note>Re-verification lane anchor: {}.</p>\n",
            esc(&head.describe())
        ),
    };
    let Some(latest_run) = assessment.result_run() else {
        // R11 I2: a newer run this PC cannot authenticate (e.g. its
        // completion marker's HMAC stripped) — the previous completed run
        // is NOT the current result.
        let withheld_html = match &assessment.withheld {
            Some(reason) if lane.failure.is_none() => format!(
                "<p class=\"banner warn\">No current re-verification result: the newest re-hash run recorded in hash-verify.jsonl has no outcome this PC can rely on (an entry it cannot authenticate, a run without a completion marker, or a lane repair after the last completed run) — an earlier completed run is NOT presented as the latest result (결과 아님: {}).</p>\n",
                esc(reason)
            ),
            _ => String::new(),
        };
        return format!(
            "{broken_html}{head_html}{withheld_html}{interrupted_html}{unauth_html}<p class=\"note warn\">No completed live source re-hash usable on this PC has been recorded for this case (run 원본 재검증 / <code>frametrace qa anomalies</code>). Hashes in section 2 are as-scanned, not re-verified.</p>"
        );
    };
    let (latest, entries) = (latest_run.run_unix, &latest_run.entries);
    // R8-I10 (P0/P1): the re-hash results are only as good as the log
    // they are read from. R9 P3: the run's records are only the entries
    // usable here; entries this PC cannot authenticate are counted, never
    // results.
    let chain = format!(
        "log chain: {} ({} entries){}",
        lane.lane.label(),
        lane.rows.len(),
        Some(lane.lane.reason())
            .filter(|reason| !reason.is_empty())
            .map(|reason| format!(" — {reason}"))
            .unwrap_or_default()
    );
    let self_consistent_rows = entries
        .iter()
        .any(|entry| entry.get("trust").and_then(|t| t.as_str()) != Some("auth"));
    // Error text quotes the log's absolute path: escaped (and redacted).
    let chain = esc(&chain);
    let mut rows = String::new();
    let (mut matched, mut mismatched, mut other) = (0usize, 0usize, 0usize);
    for entry in entries {
        let get = |key: &str| entry.get(key).and_then(|v| v.as_str()).unwrap_or("");
        let status = get("status");
        let (class, label) = match status {
            "match" => {
                matched += 1;
                ("ok", "MATCH")
            }
            "mismatch" => {
                mismatched += 1;
                ("bad", "MISMATCH")
            }
            other_status => {
                other += 1;
                (
                    "warn",
                    match other_status {
                        "missing" => "not reachable",
                        "unreadable" => "unreadable",
                        _ => "no recorded hash",
                    },
                )
            }
        };
        let dash = |value: &str| {
            if value.is_empty() {
                "—".to_string()
            } else {
                esc(value)
            }
        };
        rows.push_str(&format!(
            "<tr><td>{}</td><td class=path>{}</td><td class=hash>{}</td><td class=hash>{}</td><td class={class}>{label}</td></tr>\n",
            esc(get("id")),
            esc(get("source_path")),
            dash(get("recorded_sha256")),
            dash(get("observed_sha256")),
        ));
    }
    let banner = if !assessment.not_verified.is_empty() && mismatched == 0 {
        // R11 P9: never green (nor "re-hashed and equal") for a run that
        // is not verified here — rows may be missing from it.
        format!(
            "<p class=\"banner warn\">This re-hash run is NOT VERIFIED on this PC — {matched} file(s) shown as matching, but the run cannot be relied on as a complete re-verification (missing or unauthenticated rows are not good news): {}. It does not confirm that the sources are unchanged.</p>",
            esc(&assessment.not_verified.join("; "))
        )
    } else if !assessment.not_verified.is_empty() {
        hash_verify_banner(matched, mismatched, other).replacen(
            "</p>",
            &format!(
                " This run is also NOT VERIFIED on this PC: {}.</p>",
                esc(&assessment.not_verified.join("; "))
            ),
            1,
        )
    } else if let audit::LaneTrust::SelfConsistentOnly(_) = &lane.lane {
        hash_verify_banner(matched, mismatched, other).replacen(
            "</p>",
            " (Self-consistency only, not authentication: the re-hash log is not HMAC-authenticated on this PC.)</p>",
            1,
        )
    } else if self_consistent_rows {
        // R9 P3: the run shown is built from usable entries only; when
        // some of them are only self-consistent (the pre-key prefix) the
        // banner says so.
        hash_verify_banner(matched, mismatched, other).replacen(
            "</p>",
            " (Self-consistency only for entries written before audit keying was introduced — not authentication.)</p>",
            1,
        )
    } else {
        hash_verify_banner(matched, mismatched, other)
    };
    format!(
        "{broken_html}{head_html}{interrupted_html}{unauth_html}<p class=note>Latest completed live re-hash run at {} (run {}, newest completed run in {}); {chain}.</p>\n{banner}\n<table><tr><th>Record</th><th>Path</th><th>Recorded SHA-256</th><th>Observed SHA-256 (live)</th><th>Result</th></tr>\n{rows}</table>",
        ts(latest),
        esc(&latest_run.run_id),
        crate::anomaly::HASH_VERIFY_REL_PATH,
    )
}

/// Section-5 banner. Green ("ok") only when at least one file was actually
/// compared and every file was comparable (SHOULD-FIX 4): a run where
/// nothing matched — or some files could not be compared — must never read
/// as a clean re-verification. R11 P9: the caller never uses it unchanged
/// for a run that is not verified here.
fn hash_verify_banner(matched: usize, mismatched: usize, other: usize) -> String {
    if mismatched > 0 {
        format!(
            "<p class=\"banner bad\">{mismatched} indexed file(s) have a live SHA-256 that differs from the recorded one. This is an observation of a difference, not a finding of who or what changed the file.</p>"
        )
    } else if matched == 0 {
        format!(
            "<p class=\"banner warn\">No indexed file was re-verified: 0 files were compared against a recorded SHA-256 ({other} not comparable — not reachable, unreadable, or no recorded hash). This run does not confirm that any source is unchanged.</p>"
        )
    } else if other > 0 {
        format!(
            "<p class=\"banner warn\">{matched} indexed file(s) re-hashed live and equal to the recorded SHA-256, but {other} file(s) could not be compared (not reachable, unreadable, or no recorded hash) and are NOT re-verified.</p>"
        )
    } else {
        format!(
            "<p class=\"banner ok\">All {matched} indexed file(s) re-hashed live and equal to the recorded SHA-256.</p>"
        )
    }
}

/// Chained audit lanes: `*log.jsonl` plus the forensic-tool lanes named
/// `*-audit.jsonl` (`e01-audit.jsonl`, `tsk-audit.jsonl`), which are
/// written through the same `append_chained_jsonl` and must be verified
/// too — leaving them out would silently skip their chain check.
fn is_chained_log_name(name: &str) -> bool {
    name.ends_with("log.jsonl") || name.ends_with("-audit.jsonl")
}

fn collect_log_files(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    if depth > 4 || !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_log_files(&path, out, depth + 1);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_chained_log_name)
        {
            out.push(path);
        }
    }
}

/// An optional field, escaped — and path-redacted when a redactor is
/// given (D8: write-protect / acquisition-tool notes can quote paths).
fn opt(redactor: Option<&crate::redact::Redactor>, value: &Option<String>) -> String {
    value
        .as_deref()
        .map(|text| esc_with(redactor, text))
        .unwrap_or_else(|| "—".to_string())
}

/// D8: the host name in a redacted statement — a stable hash token (the
/// machine name can identify a person or organisation); the operator is
/// printed as recorded and the redaction banner says so.
fn host_cell(redactor: Option<&crate::redact::Redactor>, host: Option<&str>) -> String {
    match (redactor, host.filter(|h| !h.is_empty())) {
        (_, None) => "—".to_string(),
        (Some(_), Some(host)) => esc(&format!(
            "<redacted-host:{}>",
            &crate::sha256::digest_bytes(host.as_bytes())[..16]
        )),
        (None, Some(host)) => esc(host),
    }
}

fn ts(unix: i64) -> String {
    // UTC ISO-8601 without a date crate: days-from-civil.
    if unix <= 0 {
        return "—".to_string();
    }
    let days = unix / 86400;
    let secs = unix % 86400;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

#[allow(clippy::too_many_arguments)]
fn render(
    case_dir: &Path,
    manifest: &serde_json::Value,
    sources: &Result<Vec<SourceRow>, String>,
    jobs: &Result<Vec<JobRow>, String>,
    coverage: &Result<HashCoverage, String>,
    logs: &[LogRow],
    hash_verify_html: &str,
    generated_unix: i64,
    redactor: Option<&crate::redact::Redactor>,
) -> String {
    // Every printed value goes through the (optional) path redactor.
    let esc = |text: &str| esc_with(redactor, text);
    let redaction_banner = if redactor.is_some() {
        format!(
            "{}\n<p class=\"banner warn\">{} The host name is replaced by a hash token; the operator name is shown as recorded at case creation.</p>",
            crate::redact::REDACTION_MARKER,
            crate::util::html_escape(crate::redact::REDACTION_NOTE)
        )
    } else {
        String::new()
    };
    let field = |key: &str| -> String {
        manifest
            .get(key)
            .and_then(|v| v.as_str())
            .map(esc)
            .unwrap_or_else(|| "—".to_string())
    };
    let case_dir_disp = esc(&case_dir.display().to_string());
    let generated = ts(generated_unix);

    let mut src_rows = String::new();
    let mut db_error: Option<String> = None;
    match sources {
        Ok(sources) => {
            for s in sources {
                src_rows.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td class=path>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=hash>{}</td><td>{}</td></tr>\n",
                    esc(&s.source_id),
                    esc(&s.kind),
                    esc(&s.path),
                    ts(s.registered_unix),
                    opt(redactor, &s.write_protect),
                    opt(redactor, &s.acquisition_tool),
                    evidence_hash_cell(s),
                    esc(&s.observed),
                ));
            }
            if sources.is_empty() {
                src_rows.push_str(
                    "<tr><td colspan=8 class=empty>No evidence sources registered.</td></tr>\n",
                );
            }
        }
        Err(err) => {
            db_error = Some(err.clone());
            src_rows.push_str(&format!(
                "<tr><td colspan=8 class=bad>Case database unreadable — evidence sources could not be listed: {}</td></tr>\n",
                esc(err)
            ));
        }
    }

    let coverage_line = match coverage {
        Ok(c) if c.total == 0 => {
            "<p class=note>Indexed files: none recorded in the case database.</p>".to_string()
        }
        Ok(c) => {
            let class = if c.hashed < c.total { "bad" } else { "ok" };
            let errors = if c.errors > 0 {
                format!(" ({} hash error(s))", c.errors)
            } else {
                String::new()
            };
            format!(
                "<p class=\"note {class}\">Indexed files with a recorded SHA-256: {} of {}{errors}. Files without a recorded hash have no integrity value in this case.</p>",
                c.hashed, c.total
            )
        }
        Err(err) => {
            if db_error.is_none() {
                db_error = Some(err.clone());
            }
            format!(
                "<p class=\"note bad\">Indexed file hash coverage unavailable: {}</p>",
                esc(err)
            )
        }
    };

    let mut job_rows = String::new();
    let jobs = match jobs {
        Ok(jobs) => jobs.as_slice(),
        Err(err) => {
            if db_error.is_none() {
                db_error = Some(err.clone());
            }
            job_rows.push_str(&format!(
                "<tr><td colspan=7 class=bad>Case database unreadable — job ledger could not be listed: {}</td></tr>\n",
                esc(err)
            ));
            &[]
        }
    };
    for j in jobs {
        let units = match j.total_units {
            Some(t) => format!("{}/{}", j.completed_units, t),
            None => format!("{}", j.completed_units),
        };
        let status = match &j.error {
            Some(err) => format!("{}: {}", j.status, err),
            None => j.status.clone(),
        };
        job_rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td class=path>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            esc(&j.job_id),
            esc(&j.job_type),
            esc(&status),
            esc(&j.subject_path),
            ts(j.started_unix),
            j.completed_unix.map(ts).unwrap_or_else(|| "—".to_string()),
            esc(&units),
        ));
    }
    if jobs.is_empty() && job_rows.is_empty() {
        job_rows.push_str("<tr><td colspan=7 class=empty>No jobs recorded.</td></tr>\n");
    }

    let mut log_rows = String::new();
    let mut verified = 0usize;
    let mut structural_only = 0usize;
    let mut not_verified = 0usize;
    let mut broken = 0usize;
    for l in logs {
        match &l.result {
            Ok(v) => {
                // R8 P0/P1/P2: the lane's trust under THIS PC's policy.
                // Keys held + any line not authenticated here = NOT
                // VERIFIED (never "verified", never "failed"); no keys =
                // structural, self-consistency only.
                let trust = v.lane_trust();
                let keyed = trust == audit::LaneTrust::Authenticated;
                match &trust {
                    audit::LaneTrust::Authenticated => verified += 1,
                    audit::LaneTrust::SelfConsistentOnly(_) => {
                        verified += 1;
                        structural_only += 1;
                    }
                    audit::LaneTrust::NotVerified { .. } => not_verified += 1,
                }
                let mut notes = v.warnings.clone();
                match &trust {
                    audit::LaneTrust::NotVerified { reason, .. } => {
                        notes.insert(0, format!("not verified on this PC: {reason}"));
                    }
                    audit::LaneTrust::SelfConsistentOnly(reason) => {
                        notes.insert(0, reason.clone());
                        notes.push(
                            "structural only: detects partial edits, not a full rewrite by someone with file access"
                                .to_string(),
                        );
                    }
                    audit::LaneTrust::Authenticated => {}
                }
                let caveat = if notes.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", esc(&notes.join("; ")))
                };
                log_rows.push_str(&format!(
                    "<tr><td class=path>{}</td><td>{}</td><td class={}>{}</td><td class=hash>{}</td><td>{}{}</td></tr>\n",
                    esc(&l.rel_path),
                    v.entries,
                    if keyed { "ok" } else { "warn" },
                    trust.label(),
                    esc(&v.last_entry_sha256[..v.last_entry_sha256.len().min(16)]),
                    v.keyed_entries,
                    caveat,
                ));
            }
            // R9 P5/I9: an unreadable audit key configuration is NOT
            // VERIFIED (fail closed) — never "verification failed".
            Err(err) if audit::is_key_config_error(err) => {
                not_verified += 1;
                log_rows.push_str(&format!(
                    "<tr><td class=path>{}</td><td colspan=4 class=warn>NOT VERIFIED on this PC (not a tampering verdict): {}</td></tr>\n",
                    esc(&l.rel_path),
                    esc(err),
                ));
            }
            Err(err) => {
                broken += 1;
                log_rows.push_str(&format!(
                    "<tr><td class=path>{}</td><td colspan=4 class=bad>verification failed: {}</td></tr>\n",
                    esc(&l.rel_path),
                    esc(err),
                ));
            }
        }
    }
    if logs.is_empty() {
        log_rows.push_str("<tr><td colspan=5 class=empty>No chained audit logs found.</td></tr>\n");
    }

    let mut integrity_banner = if broken > 0 {
        "<p class=\"banner bad\">WARNING: one or more audit lanes failed verification — inspect the ledger below before relying on this package.</p>".to_string()
    } else if not_verified > 0 {
        format!(
            "<p class=\"banner warn\">Audit lanes re-checked at statement generation time: {not_verified} lane(s) NOT VERIFIED on this PC — this PC holds audit keys and those lanes contain entries it cannot authenticate (entries signed under another PC's key: check them on the PC holding that key); {verified} other lane(s) passed.</p>"
        )
    } else if verified > 0 && structural_only > 0 {
        format!(
            "<p class=\"banner warn\">Audit lanes re-checked at statement generation time: {verified} observed internally consistent; {structural_only} of them are structural-only (unsigned) and are not tamper-proof against a complete rewrite.</p>"
        )
    } else if verified > 0 {
        format!(
            "<p class=\"banner ok\">Audit lanes re-checked at statement generation time: all {verified} observed consistent with HMAC verification against a configured key.</p>"
        )
    } else {
        "<p class=banner>No chained audit logs were present to verify.</p>".to_string()
    };
    if let Some(err) = &db_error {
        integrity_banner.push_str(&format!(
            "\n<p class=\"banner bad\">Case database unreadable: {} — sections 2 and 3 are incomplete.</p>",
            esc(err)
        ));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>Chain of Custody Statement — {case_id}</title>
<style>
body {{ font-family: -apple-system, "Segoe UI", Malgun Gothic, sans-serif; max-width: 980px; margin: 24px auto; padding: 0 20px; color: #1a1a1a; font-size: 13px; }}
h1 {{ font-size: 20px; border-bottom: 3px solid #2c5f8a; padding-bottom: 8px; }}
h2 {{ font-size: 15px; margin-top: 28px; color: #2c5f8a; }}
table {{ border-collapse: collapse; width: 100%; margin-top: 8px; }}
th, td {{ border: 1px solid #ccc; padding: 5px 8px; text-align: left; vertical-align: top; }}
th {{ background: #eef4f8; }}
td.path {{ font-family: Consolas, monospace; font-size: 11px; word-break: break-all; }}
td.hash {{ font-family: Consolas, monospace; font-size: 11px; }}
td.empty {{ color: #888; font-style: italic; }}
td.ok, span.ok {{ color: #1d7a38; }}
td.bad, p.banner.bad {{ color: #b3261e; }}
td.warn, span.warn, p.note.warn {{ color: #8a5a00; }}
p.note.bad {{ color: #b3261e; font-weight: 600; }}
p.note.ok {{ color: #1d7a38; }}
p.banner {{ padding: 10px 14px; border-radius: 6px; font-weight: 600; }}
p.banner.ok {{ background: #e6f4ea; color: #1d7a38; }}
p.banner.bad {{ background: #fce8e6; }}
p.banner.warn {{ background: #fff4e0; color: #8a5a00; }}
p.banner:not(.ok):not(.bad):not(.warn) {{ background: #f1f3f4; color: #444; }}
.meta td:first-child {{ font-weight: 600; width: 200px; background: #f7f9fb; }}
.note {{ font-size: 12px; color: #555; }}
@media print {{ body {{ margin: 0; }} }}
</style></head><body>
<h1>Chain of Custody Statement</h1>
{redaction_banner}
<p class=note>Generated {generated} by FrameTrace. At generation time the audit lanes in section 4 were re-verified and the evidence source paths in section 2 were re-stat'ed (presence/size only); original evidence was not re-hashed by this statement (section 5 shows the result of a separate live re-hash run, if one was recorded). Print to PDF for submission if required.</p>
{integrity_banner}
<h2>1. Case Identity</h2>
<table class=meta>
<tr><td>Case ID</td><td>{case_id}</td></tr>
<tr><td>Title</td><td>{title}</td></tr>
<tr><td>Operator</td><td>{operator}</td></tr>
<tr><td>Host</td><td>{host}</td></tr>
<tr><td>Created</td><td>{created}</td></tr>
<tr><td>Tool</td><td>{tool} v{tool_version} ({platform})</td></tr>
<tr><td>Case directory</td><td class=path>{case_dir_disp}</td></tr>
</table>
<h2>2. Evidence Sources</h2>
<table><tr><th>Source ID</th><th>Kind</th><th>Path</th><th>Registered (UTC)</th><th>Write-protect</th><th>Acquisition tool</th><th>Evidence hash and origin (as recorded, not re-verified by this statement)</th><th>Observed at generation</th></tr>
{src_rows}</table>
{coverage_line}
<h2>3. Processing Ledger</h2>
<table><tr><th>Job ID</th><th>Type</th><th>Status</th><th>Subject</th><th>Started (UTC)</th><th>Completed (UTC)</th><th>Units</th></tr>
{job_rows}</table>
<h2>4. Audit Trail Integrity</h2>
<table><tr><th>Log</th><th>Entries</th><th>Integrity</th><th>Last entry SHA-256</th><th>Keyed</th></tr>
{log_rows}</table>
<h2>5. Source Hash Re-verification (recorded vs observed)</h2>
{hash_verify_html}
<h2>6. Scope Statement</h2>
<p>FrameTrace is designed to open registered evidence read-only and to write all derived
outputs (proxies, clips, thumbnails, carved candidates, recovered entries, telemetry reports)
as work products under the case directory, logged to the chained audit lanes in section 4.
This statement does not by itself establish that original evidence is unchanged: it did not
re-hash the sources in section 2. To support that claim, re-hash each source and compare it with
the recorded evidence hash and with the indexed SHA-256 values. Carved and recovered items are
candidates produced by heuristic signature/filesystem analysis and must not be represented as
authenticated originals. Hash verification of any packaged file can be performed against
<code>manifest.sha256</code> / <code>package-manifest.json</code>.</p>
<p class=note>Integrity labels: <code>integrity-keyed</code> = every entry HMAC-verified against a
configured key; <code>integrity-structural-only</code> = hash-chain structure observed consistent but
entries are unsigned or unauthenticated, so a complete rewrite of the log by someone with file
access would not be detected (self-consistency only, not authentication);
<code>integrity-not-verified</code> = this PC holds audit keys and the lane contains entries it cannot
authenticate (no HMAC, or signed under another PC's key — check those on the PC holding that key), so
the lane is not relied on here. A failed lane is disclosed in section 4 rather than omitted.</p>
</body></html>"#,
        case_id = field("case_id"),
        title = field("title"),
        operator = field("operator"),
        host = host_cell(redactor, manifest.get("host").and_then(|v| v.as_str())),
        created = manifest
            .get("created_unix")
            .and_then(|v| v.as_i64())
            .map(ts)
            .unwrap_or_else(|| "—".to_string()),
        tool = field("tool_name"),
        tool_version = field("tool_version"),
        platform = field("platform"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_case_dir() {
        let dir = std::env::temp_dir().join(format!("ft-custody-{}", now_unix().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(generate(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Item 15: the forensic-tool lanes (`e01-audit.jsonl`,
    /// `tsk-audit.jsonl`) are collected and chain-verified with the rest.
    #[test]
    fn audit_lanes_named_dash_audit_are_verified() {
        let dir = std::env::temp_dir().join(format!(
            "ft-custody-lanes-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        let logs = dir.join("evidence/logs");
        std::fs::create_dir_all(&logs).unwrap();
        audit::append_chained_jsonl_keyed(&logs.join("e01-audit.jsonl"), r#"{"k":"e01"}"#, None)
            .unwrap();
        audit::append_chained_jsonl_keyed(&logs.join("tsk-audit.jsonl"), r#"{"k":"tsk"}"#, None)
            .unwrap();
        audit::append_chained_jsonl_keyed(&logs.join("scan-log.jsonl"), r#"{"k":"scan"}"#, None)
            .unwrap();
        // Not a chained lane: must stay out.
        std::fs::write(logs.join("hash-notes.jsonl"), "{}\n").unwrap();

        let rows = verify_audit_logs(&dir);
        let names: Vec<&str> = rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(
            names,
            [
                "evidence/logs/e01-audit.jsonl",
                "evidence/logs/scan-log.jsonl",
                "evidence/logs/tsk-audit.jsonl",
            ]
        );
        assert!(rows.iter().all(|row| row.result.is_ok()));

        // A tampered tool lane is now reported, not skipped.
        let e01 = logs.join("e01-audit.jsonl");
        let text = std::fs::read_to_string(&e01).unwrap();
        std::fs::write(&e01, text.replace("\"e01\"", "\"e02\"")).unwrap();
        let rows = verify_audit_logs(&dir);
        assert!(rows[0].result.is_err(), "{:?}", rows[0].result);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn ok_lane(integrity: audit::AuditIntegrity) -> LogRow {
        let keyed = integrity == audit::AuditIntegrity::Keyed;
        LogRow {
            rel_path: "evidence/logs/anomaly-log.jsonl".to_string(),
            result: Ok(audit::AuditChainVerification {
                entries: 2,
                last_entry_sha256: "ab".repeat(32),
                keyed_entries: if keyed { 2 } else { 0 },
                unauthenticated_keyed_entries: 0,
                entry_auth: if keyed {
                    vec![audit::EntryAuth::Authenticated; 2]
                } else {
                    vec![audit::EntryAuth::Unkeyed; 2]
                },
                rewrite_not_excluded: None,
                integrity,
                warnings: Vec::new(),
                requirement: if keyed {
                    audit::AuthRequirement::Required
                } else {
                    audit::AuthRequirement::SelfConsistentOnly
                },
                entry_key_ids: vec![keyed.then(|| "k".to_string()); 2],
                pre_key_entries: 0,
                pre_key_other_pc: None,
                rewrite_suspect_lines: Vec::new(),
                location_unverified: None,
            }),
        }
    }

    #[test]
    fn statement_never_overclaims_and_marks_structural_lanes_amber() {
        let html = render(
            Path::new("/case"),
            &serde_json::json!({"case_id": "FT-1"}),
            &Ok(Vec::new()),
            &Ok(Vec::new()),
            &Ok(HashCoverage {
                total: 4,
                hashed: 3,
                errors: 1,
            }),
            &[ok_lane(audit::AuditIntegrity::StructuralOnly)],
            "",
            1_704_110_400,
            None,
        );
        assert!(!html.contains("were not modified"));
        assert!(!html.contains("verified live"));
        assert!(html.contains("not re-hashed"));
        assert!(html.contains("banner warn"));
        assert!(html.contains("class=warn>integrity-structural-only"));
        assert!(html.contains("3 of 4"));
        assert!(html.contains("1 hash error"));
        assert!(html.contains("No evidence sources registered."));
    }

    #[test]
    fn evidence_hash_shows_whether_frametrace_computed_it() {
        let source = |id: &str, origin: Option<&str>| SourceRow {
            source_id: id.into(),
            kind: "e01".into(),
            path: format!("/e/{id}.E01"),
            registered_unix: 1_704_110_400,
            write_protect: None,
            acquisition_tool: None,
            evidence_hash: Some("ab".repeat(32)),
            hash_algorithm: Some("sha256".into()),
            hash_origin: origin.map(str::to_string),
            observed: "present".into(),
        };
        let html = render(
            Path::new("/case"),
            &serde_json::Value::Null,
            &Ok(vec![
                source("COMP", Some("computed")),
                source("TYPED", Some("operator-supplied")),
                source("OLD", None),
            ]),
            &Ok(Vec::new()),
            &Err("n/a".to_string()),
            &[],
            "",
            1_704_110_400,
            None,
        );
        assert!(
            html.contains("<span class=ok>computed by FrameTrace</span>"),
            "{html}"
        );
        assert!(
            html.contains("<span class=warn>operator-supplied, NOT verified by FrameTrace</span>")
        );
        assert!(html.contains("origin not recorded"));
        assert!(html.contains(&format!("sha256:{}", "ab".repeat(32))));
    }

    #[test]
    fn statement_reads_origin_from_case_db() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-custody-origin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db")).unwrap();
        let input = |path: &str| case_db::EvidenceSourceInput {
            kind: "e01".into(),
            path: path.into(),
            source_id: None,
            write_protect: None,
            acquisition_tool: None,
            evidence_hash: Some("cd".repeat(32)),
            notes: None,
            metadata_json: None,
        };
        case_db::register_evidence_source(&dir, &input("/e/typed.E01")).unwrap();
        case_db::register_evidence_source_with_origin(
            &dir,
            &input("/e/hashed.E01"),
            case_db::HashOrigin::Computed,
        )
        .unwrap();
        let rows = load_evidence_sources(&dir).unwrap();
        let origin = |p: &str| {
            rows.iter()
                .find(|r| r.path == p)
                .unwrap()
                .hash_origin
                .clone()
        };
        assert_eq!(origin("/e/typed.E01").as_deref(), Some("operator-supplied"));
        assert_eq!(origin("/e/hashed.E01").as_deref(), Some("computed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_database_is_shown_as_error_not_empty() {
        let err = || Err::<Vec<SourceRow>, String>("disk I/O error".to_string());
        let html = render(
            Path::new("/case"),
            &serde_json::Value::Null,
            &err(),
            &Err("disk I/O error".to_string()),
            &Err("disk I/O error".to_string()),
            &[ok_lane(audit::AuditIntegrity::Keyed)],
            "",
            1_704_110_400,
            None,
        );
        assert!(!html.contains("No evidence sources registered."));
        assert!(!html.contains("No jobs recorded."));
        assert!(html.contains("Case database unreadable"));
        assert!(html.contains("disk I/O error"));
        assert!(html.contains("banner ok"));
        assert!(html.contains("banner bad"));
    }

    #[test]
    fn generate_without_db_reports_missing_database() {
        let dir = std::env::temp_dir().join(format!(
            "ft-custody-nodb-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("case.json"), r#"{"case_id":"FT-X"}"#).unwrap();
        let out = generate(&dir).unwrap();
        let html = std::fs::read_to_string(out).unwrap();
        assert!(html.contains("case database not found"));
        assert!(!html.contains("No evidence sources registered."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn observe_path_reports_presence() {
        let file = std::env::temp_dir().join(format!("ft-custody-obs-{}", std::process::id()));
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            observe_path(file.to_str().unwrap()),
            "present (file, 3 bytes)"
        );
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            observe_path(file.to_str().unwrap()),
            "not reachable at statement time"
        );
    }

    #[test]
    fn hash_coverage_counts_changed_during_hash_as_error() {
        let dir = std::env::temp_dir().join(format!(
            "ft-custody-coverage-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(dir.join("db")).unwrap();
        {
            let conn = rusqlite::Connection::open(case_db::case_db_path(&dir)).unwrap();
            conn.execute_batch(
                "CREATE TABLE videos (sha256 TEXT, hash_status TEXT NOT NULL);
                 INSERT INTO videos VALUES ('ab', 'ok'), (NULL, 'error'),
                     (NULL, 'changed-during-hash'), (NULL, 'skipped');",
            )
            .unwrap();
        }
        let coverage = load_hash_coverage(&dir).unwrap();
        assert_eq!(coverage.total, 4);
        assert_eq!(coverage.hashed, 1);
        assert_eq!(coverage.errors, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SHOULD-FIX 4: nothing compared (or some not comparable) is never green.
    #[test]
    fn hash_verify_banner_is_ok_only_when_everything_was_compared() {
        let none = hash_verify_banner(0, 0, 5);
        assert!(none.contains("banner warn"), "{none}");
        assert!(none.contains("No indexed file was re-verified"), "{none}");
        let empty = hash_verify_banner(0, 0, 0);
        assert!(empty.contains("banner warn"), "{empty}");
        let partial = hash_verify_banner(3, 0, 2);
        assert!(partial.contains("banner warn"), "{partial}");
        assert!(partial.contains("NOT re-verified"), "{partial}");
        assert!(hash_verify_banner(3, 0, 0).contains("banner ok"));
        assert!(hash_verify_banner(3, 1, 0).contains("banner bad"));
    }

    /// Writes `evidence/logs/hash-verify.jsonl` lines (unchained is fine
    /// for the reader) under a fresh case dir.
    fn hash_verify_case(name: &str, lines: &[serde_json::Value]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ft-custody-hv-{name}-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(dir.join("evidence/logs")).unwrap();
        // R8-I10: the real log is chained; a log that does not verify no
        // longer gets a green re-verification banner.
        for line in lines {
            crate::audit::append_chained_jsonl_keyed(
                &dir.join(crate::anomaly::HASH_VERIFY_REL_PATH),
                &line.to_string(),
                None,
            )
            .unwrap();
        }
        dir
    }

    fn hv(run: i64, run_id: Option<&str>, id: &str, status: &str) -> serde_json::Value {
        let mut value = serde_json::json!({"event": "hash-verify", "run_unix": run, "id": id,
            "source_path": format!("/e/{id}.mp4"), "status": status});
        if let Some(run_id) = run_id {
            value["run_id"] = run_id.into();
        }
        value
    }

    fn hv_done(run: i64, run_id: Option<&str>) -> serde_json::Value {
        let mut value = serde_json::json!({"event": "hash-verify-run", "run_unix": run});
        if let Some(run_id) = run_id {
            value["run_id"] = run_id.into();
        }
        value
    }

    /// D4: an interrupted run's records are never mixed into the next
    /// completed run — only the newest COMPLETED run is the result. R13
    /// P16: an interrupted run AFTER it withholds the result (its marker
    /// may have been deleted) and its mismatch rows are shown.
    #[test]
    fn hash_verify_groups_by_run_and_shows_interrupted_runs_separately() {
        let lines = [
            hv(100, Some("100-aaaa"), "vid_old", "match"),
            hv_done(100, Some("100-aaaa")),
            // Interrupted (no marker) — must not join run 300.
            hv(200, Some("200-bbbb"), "vid_cut", "mismatch"),
            hv(300, Some("300-cccc"), "vid_new", "match"),
            hv_done(300, Some("300-cccc")),
        ];
        let dir = hash_verify_case("mixed", &lines);
        let (latest, entries) = crate::anomaly::latest_hash_verify_run(&dir).unwrap();
        assert_eq!(latest, 300);
        let ids: Vec<&str> = entries.iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["vid_new"], "only the completed run's own records");
        let html = render_hash_verify(&dir, None);
        assert!(html.contains("Latest completed live re-hash run"), "{html}");
        assert!(html.contains("vid_new"), "{html}");
        assert!(!html.contains("vid_cut"), "{html}");
        assert!(html.contains("banner ok"), "{html}");

        // A newer run without a completion marker: no current result, and
        // its mismatch is listed.
        crate::audit::append_chained_jsonl_keyed(
            &dir.join(crate::anomaly::HASH_VERIFY_REL_PATH),
            &hv(400, Some("400-dddd"), "vid_late", "mismatch").to_string(),
            None,
        )
        .unwrap();
        assert!(crate::anomaly::latest_hash_verify_run(&dir).is_none());
        let html = render_hash_verify(&dir, None);
        assert!(html.contains("No current re-verification result"), "{html}");
        assert!(html.contains("표식 없는 더 새 실행"), "{html}");
        assert!(
            html.contains("vid_late") && html.contains("MISMATCH"),
            "{html}"
        );
        assert!(!html.contains("banner ok"), "{html}");
        assert!(!html.contains("vid_cut"), "{html}");

        // Logs written before run ids existed group by run_unix the same way.
        let legacy = hash_verify_case(
            "legacy",
            &[
                hv(200, None, "vid_cut", "mismatch"),
                hv(300, None, "vid_new", "match"),
                hv_done(300, None),
            ],
        );
        let (_, entries) = crate::anomaly::latest_hash_verify_run(&legacy).unwrap();
        assert_eq!(entries.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&legacy);
    }

    /// D11: a re-hash log that exists but cannot be read is an error in the
    /// statement, never "no re-hash recorded".
    #[test]
    fn unreadable_hash_verify_log_is_an_error_not_absence() {
        let dir = std::env::temp_dir().join(format!(
            "ft-custody-hv-unreadable-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        // A directory where the log should be: exists, cannot be read.
        std::fs::create_dir_all(dir.join(crate::anomaly::HASH_VERIFY_REL_PATH)).unwrap();
        let html = render_hash_verify(&dir, None);
        assert!(html.contains("could not be read"), "{html}");
        assert!(!html.contains("No completed live source re-hash"), "{html}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn renders_utc_timestamp() {
        assert_eq!(ts(1704110400), "2024-01-01 12:00:00Z");
        assert_eq!(ts(0), "—");
        assert_eq!(utc_timestamp(1704110400), "2024-01-01 12:00:00");
    }

    /// I4: a statement rendered for a path-redacted package prints no
    /// absolute path — case directory, sources, job subjects and errors —
    /// and says so.
    #[test]
    fn redacted_statement_hides_every_absolute_path() {
        let source = SourceRow {
            source_id: "SRC-1".into(),
            kind: "e01".into(),
            path: "/evidence/secret/disk.E01".into(),
            registered_unix: 1_704_110_400,
            // D8: free-text custody fields quoting paths are redacted too.
            write_protect: Some("blocker log at /evidence/secret/blocker.txt".into()),
            acquisition_tool: Some(r"ewfacquire from C:\Users\secret\tools".into()),
            evidence_hash: None,
            hash_algorithm: None,
            hash_origin: None,
            observed: "present".into(),
        };
        let job = JobRow {
            job_id: "job-1".into(),
            job_type: "scan".into(),
            status: "failed".into(),
            subject_path: "/evidence/secret".into(),
            started_unix: 1_704_110_400,
            completed_unix: None,
            total_units: None,
            completed_units: 0,
            error: Some("failed to read /evidence/secret/x.mp4: denied".into()),
        };
        let case_dir = Path::new("/cases/secret-case");
        let redactor = crate::redact::Redactor::new(case_dir);
        let html = render(
            case_dir,
            &serde_json::json!({"case_id": "FT-1", "host": "SECRET-LAPTOP", "operator": "Examiner Lee"}),
            &Ok(vec![source]),
            &Ok(vec![job]),
            &Err("cannot open /cases/secret-case/db/case.db".to_string()),
            &[],
            "",
            1_704_110_400,
            Some(&redactor),
        );
        assert!(!html.to_ascii_lowercase().contains("secret"), "{html}");
        assert!(html.contains("&lt;redacted-host:"), "{html}");
        assert!(html.contains("Examiner Lee"), "operator shown as recorded");
        assert!(
            html.contains("operator name is shown as recorded"),
            "{html}"
        );
        assert!(crate::redact::html_is_redacted(&html));
        assert!(html.contains("&lt;redacted:"), "{html}");
        assert!(html.contains("&lt;case&gt;/db/case.db"), "{html}");
        assert!(html.contains(": denied"), "{html}");
        let plain = render(
            case_dir,
            &serde_json::Value::Null,
            &Ok(Vec::new()),
            &Ok(Vec::new()),
            &Err("n/a".into()),
            &[],
            "",
            1,
            None,
        );
        assert!(!crate::redact::html_is_redacted(&plain));
        assert!(plain.contains("secret-case"));
    }

    fn r8_render_lane(lane: &std::path::Path, keys: &[crate::audit_key::AuditKey]) -> String {
        render(
            Path::new("/case"),
            &serde_json::json!({"case_id": "FT-R8"}),
            &Ok(Vec::new()),
            &Ok(Vec::new()),
            &Err("n/a".to_string()),
            &[LogRow {
                rel_path: "evidence/logs/scan-log.jsonl".to_string(),
                result: audit::verify_chained_jsonl_keyed(lane, keys),
            }],
            "",
            1_704_110_400,
            None,
        )
    }

    /// R8 P0/P2 for the custody statement: a lane stripped of its HMACs was
    /// counted "observed internally consistent" (structural) on a PC
    /// holding the key; now it is NOT VERIFIED there. A lane continued on
    /// another PC names that PC's key and is not "failed".
    #[test]
    fn r8_custody_lane_trust_comes_from_keys_held() {
        let dir = std::env::temp_dir().join(format!("ft-custody-r8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mine = crate::audit_key::AuditKey::new("pc-a", &[0xc1; 32]);
        let other = crate::audit_key::AuditKey::new("pc-b", &[0xc2; 32]);
        let stripped = dir.join("stripped.jsonl");
        audit::append_chained_jsonl_keyed(&stripped, r#"{"n":1}"#, None).unwrap();
        let html = r8_render_lane(&stripped, std::slice::from_ref(&mine));
        assert!(html.contains("class=warn>integrity-not-verified"), "{html}");
        assert!(html.contains("NOT VERIFIED on this PC"), "{html}");
        assert!(!html.contains("banner ok"), "{html}");
        let multi = dir.join("multi.jsonl");
        audit::append_chained_jsonl_keyed(&multi, r#"{"n":1}"#, Some(&mine)).unwrap();
        audit::append_chained_jsonl_keyed(&multi, r#"{"n":2}"#, Some(&other)).unwrap();
        let html = r8_render_lane(&multi, std::slice::from_ref(&mine));
        assert!(html.contains("&quot;pc-b&quot; 보유 PC에서 확인"), "{html}");
        assert!(
            !html.contains("failed verification") && html.contains("NOT VERIFIED on this PC"),
            "never failed: {html}"
        );
        // P1: without keys, structural — self-consistency only.
        let html = r8_render_lane(&stripped, &[]);
        assert!(
            html.contains("class=warn>integrity-structural-only"),
            "{html}"
        );
        assert!(html.contains("자기 일관성만 확인(인증 아님)"), "{html}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R8-I10: section 5 no longer prints a green "all re-hashed and equal"
    /// banner over a re-hash log whose chain FAILED, or (keys held) over
    /// entries this PC cannot authenticate.
    #[test]
    fn r8_rehash_clause_follows_the_log_trust() {
        let lines = [
            hv(300, Some("300-a"), "vid_a", "match"),
            hv_done(300, Some("300-a")),
        ];
        let dir = hash_verify_case("r8-trust", &lines);
        let mine = crate::audit_key::AuditKey::new("pc-a", &[0xc3; 32]);
        crate::audit_key::with_test_keys(&[], || {
            let html = render_hash_verify(&dir, None);
            assert!(html.contains("banner ok"), "{html}");
            assert!(html.contains("Self-consistency only"), "{html}");
        });
        crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
            let html = render_hash_verify(&dir, None);
            assert!(!html.contains("banner ok"), "{html}");
            // R9 P3: the unauthenticated entries are not a result at all.
            assert!(
                html.contains("cannot be authenticated on this PC"),
                "{html}"
            );
            assert!(html.contains("No completed live source re-hash"), "{html}");
        });
        // A forged result line (chain not recomputed): never green.
        let path = dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"match\"", "\"match\" ")).unwrap();
        crate::audit_key::with_test_keys(&[], || {
            let html = render_hash_verify(&dir, None);
            assert!(!html.contains("banner ok"), "{html}");
            assert!(html.contains("banner bad"), "{html}");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
