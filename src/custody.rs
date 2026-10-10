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
use crate::util::{html_escape, now_unix, read_to_string, write_text_atomic};
use std::path::{Path, PathBuf};

pub const CUSTODY_REL_PATH: &str = "reports/custody-statement.html";

/// Directories whose `*-log.jsonl` files are chained audit lanes worth
/// verifying in the statement. Reviewer data files (videos.jsonl,
/// timeline.jsonl) are index formats, not audit chains, and stay out.
const AUDIT_LOG_DIRS: &[&str] = &["artifacts", "evidence/logs"];

/// Generates (or refreshes) `reports/custody-statement.html`.
/// Returns the written path.
pub fn generate(case_dir: &Path) -> Result<PathBuf, String> {
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
    let sources = load_evidence_sources(case_dir);
    let jobs = load_jobs(case_dir);
    let coverage = load_hash_coverage(case_dir);
    let logs = verify_audit_logs(case_dir);
    let hash_verify_html = render_hash_verify(case_dir);
    let html = render(
        case_dir,
        &manifest,
        &sources,
        &jobs,
        &coverage,
        &logs,
        &hash_verify_html,
        generated_unix as i64,
    );

    let out = case_dir.join(CUSTODY_REL_PATH);
    write_text_atomic(&out, &html)
        .map_err(|err| format!("failed to write custody statement {}: {err}", out.display()))?;
    Ok(out)
}

struct SourceRow {
    source_id: String,
    kind: String,
    path: String,
    registered_unix: i64,
    write_protect: Option<String>,
    acquisition_tool: Option<String>,
    evidence_hash: Option<String>,
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
    let mut stmt = conn
        .prepare(
            "SELECT source_id, kind, path, registered_unix, write_protect,
                acquisition_tool, evidence_hash
         FROM evidence_sources ORDER BY registered_unix ASC",
        )
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
            })
        })
        .map_err(|err| format!("failed to query evidence sources: {err}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("failed to read evidence source row: {err}"))
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
fn render_hash_verify(case_dir: &Path) -> String {
    let path = case_dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let Some((latest, entries)) = crate::anomaly::latest_hash_verify_run(case_dir) else {
        return "<p class=\"note warn\">No completed live source re-hash has been recorded for this case (run 원본 재검증 / <code>frametrace qa anomalies</code>). Hashes in section 2 are as-scanned, not re-verified.</p>".to_string();
    };
    let chain = match audit::verify_chained_jsonl(&path) {
        Ok(v) => format!("log chain: {} ({} entries)", v.integrity.label(), v.entries),
        Err(err) => format!("log chain verification FAILED: {err}"),
    };
    let mut rows = String::new();
    let (mut matched, mut mismatched, mut other) = (0usize, 0usize, 0usize);
    for entry in &entries {
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
    let banner = if mismatched > 0 {
        format!(
            "<p class=\"banner bad\">{mismatched} indexed file(s) have a live SHA-256 that differs from the recorded one. This is an observation of a difference, not a finding of who or what changed the file.</p>"
        )
    } else {
        format!(
            "<p class=\"banner ok\">{matched} indexed file(s) re-hashed live and equal to the recorded SHA-256 ({other} not comparable).</p>"
        )
    };
    format!(
        "<p class=note>Live re-hash run at {} (newest run in {}); {chain}.</p>\n{banner}\n<table><tr><th>Record</th><th>Path</th><th>Recorded SHA-256</th><th>Observed SHA-256 (live)</th><th>Result</th></tr>\n{rows}</table>",
        ts(latest),
        crate::anomaly::HASH_VERIFY_REL_PATH,
    )
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
            .is_some_and(|name| name.ends_with("log.jsonl"))
        {
            out.push(path);
        }
    }
}

fn esc(value: &str) -> String {
    html_escape(value)
}

fn opt(value: &Option<String>) -> String {
    value.as_deref().map(esc).unwrap_or_else(|| "—".to_string())
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
) -> String {
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
                    opt(&s.write_protect),
                    opt(&s.acquisition_tool),
                    opt(&s.evidence_hash),
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
    let mut broken = 0usize;
    for l in logs {
        match &l.result {
            Ok(v) => {
                verified += 1;
                let keyed = v.integrity == audit::AuditIntegrity::Keyed;
                if !keyed {
                    structural_only += 1;
                }
                let mut notes = v.warnings.clone();
                if !keyed {
                    notes.push(
                        "structural only: detects partial edits, not a full rewrite by someone with file access"
                            .to_string(),
                    );
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
                    v.integrity.label(),
                    esc(&v.last_entry_sha256[..v.last_entry_sha256.len().min(16)]),
                    v.keyed_entries,
                    caveat,
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
td.ok {{ color: #1d7a38; }}
td.bad, p.banner.bad {{ color: #b3261e; }}
td.warn, p.note.warn {{ color: #8a5a00; }}
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
<table><tr><th>Source ID</th><th>Kind</th><th>Path</th><th>Registered (UTC)</th><th>Write-protect</th><th>Acquisition tool</th><th>Evidence hash (as recorded, not re-verified)</th><th>Observed at generation</th></tr>
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
access would not be detected. A failed lane is disclosed in section 4 rather than omitted.</p>
</body></html>"#,
        case_id = field("case_id"),
        title = field("title"),
        operator = field("operator"),
        host = field("host"),
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

    fn ok_lane(integrity: audit::AuditIntegrity) -> LogRow {
        LogRow {
            rel_path: "evidence/logs/anomaly-log.jsonl".to_string(),
            result: Ok(audit::AuditChainVerification {
                entries: 2,
                last_entry_sha256: "ab".repeat(32),
                keyed_entries: 0,
                unauthenticated_keyed_entries: 0,
                integrity,
                warnings: Vec::new(),
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

    #[test]
    fn renders_utc_timestamp() {
        assert_eq!(ts(1704110400), "2024-01-01 12:00:00Z");
        assert_eq!(ts(0), "—");
    }
}
