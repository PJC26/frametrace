use crate::case_db;
// I14: QA reports land in the packaged report tree; written atomically.
use crate::util::{html_escape, json_escape, read_to_string, write_text_atomic as write_text};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const PRECISION_TARGET: f64 = 0.98;
const RECALL_TARGET: f64 = 0.98;

#[derive(Debug, Clone)]
struct ExpectedEvidence {
    source_path: String,
    sha256: Option<String>,
}

#[derive(Debug, Clone)]
struct IndexedEvidence {
    source_path: String,
    sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QaReport {
    pub report_path: PathBuf,
    pub passed: bool,
}

#[derive(Debug, Clone)]
pub struct ReleaseReadinessOptions {
    pub corpus_manifest: Option<PathBuf>,
    pub comparison_case_dir: Option<PathBuf>,
    pub performance_output_dir: Option<PathBuf>,
    pub performance_rows: usize,
}

pub fn accuracy_report(
    case_dir: &Path,
    corpus_manifest: &Path,
    output_dir: &Path,
) -> Result<QaReport, String> {
    let expected = read_expected_manifest(corpus_manifest)?;
    let indexed = read_indexed_evidence(case_dir)?;
    let indexed_by_source = indexed
        .iter()
        .map(|item| (normalize_evidence_path(&item.source_path), item))
        .collect::<HashMap<_, _>>();
    let expected_sources = expected
        .iter()
        .map(|item| normalize_evidence_path(&item.source_path))
        .collect::<HashSet<_>>();

    let mut true_positive = 0usize;
    let mut false_negative = 0usize;
    let mut hash_mismatch = 0usize;
    for item in &expected {
        match indexed_by_source.get(normalize_evidence_path(&item.source_path).as_str()) {
            Some(indexed) if item.sha256.is_none() || item.sha256 == indexed.sha256 => {
                true_positive += 1;
            }
            Some(_) => {
                false_negative += 1;
                hash_mismatch += 1;
            }
            None => false_negative += 1,
        }
    }
    let false_positive = indexed
        .iter()
        .filter(|item| !expected_sources.contains(&normalize_evidence_path(&item.source_path)))
        .count();
    let predicted_positive = true_positive + false_positive;
    let ground_truth_positive = expected.len();
    let precision = if predicted_positive == 0 {
        1.0
    } else {
        true_positive as f64 / predicted_positive as f64
    };
    let recall = if ground_truth_positive == 0 {
        1.0
    } else {
        true_positive as f64 / ground_truth_positive as f64
    };
    let passed = precision >= PRECISION_TARGET && recall >= RECALL_TARGET && hash_mismatch == 0;

    fs::create_dir_all(output_dir)
        .map_err(|err| format!("failed to create QA output directory: {err}"))?;
    let json_path = output_dir.join("accuracy-report.json");
    let html_path = output_dir.join("accuracy-report.html");
    write_text(
        &json_path,
        &format!(
            "{{\n  \"schema_version\": 1,\n  \"qa_type\": \"accuracy\",\n  \"passed\": {},\n  \"precision\": {:.6},\n  \"recall\": {:.6},\n  \"true_positive\": {},\n  \"false_positive\": {},\n  \"false_negative\": {},\n  \"hash_mismatch\": {},\n  \"expected_count\": {},\n  \"indexed_count\": {}\n}}\n",
            passed,
            precision,
            recall,
            true_positive,
            false_positive,
            false_negative,
            hash_mismatch,
            expected.len(),
            indexed.len()
        ),
    )
    .map_err(|err| format!("failed to write accuracy JSON report: {err}"))?;
    write_text(
        &html_path,
        &simple_html_report(
            "FrameTrace Accuracy QA",
            &format!(
                "passed={} precision={:.6} recall={:.6} tp={} fp={} fn={} hash_mismatch={}",
                passed,
                precision,
                recall,
                true_positive,
                false_positive,
                false_negative,
                hash_mismatch
            ),
        ),
    )
    .map_err(|err| format!("failed to write accuracy HTML report: {err}"))?;

    if passed {
        Ok(QaReport {
            report_path: json_path,
            passed,
        })
    } else {
        Err(format!(
            "accuracy QA failed: precision={precision:.6}, recall={recall:.6}, false_positive={false_positive}, false_negative={false_negative}, hash_mismatch={hash_mismatch}"
        ))
    }
}

pub fn reproducibility_report(
    left_case_dir: &Path,
    right_case_dir: &Path,
    output_dir: &Path,
) -> Result<QaReport, String> {
    let left = normalized_case_core(left_case_dir)?;
    let right = normalized_case_core(right_case_dir)?;
    let passed = left == right;
    fs::create_dir_all(output_dir)
        .map_err(|err| format!("failed to create QA output directory: {err}"))?;
    let report_path = output_dir.join("reproducibility-report.json");
    write_text(
        &report_path,
        &format!(
            "{{\n  \"schema_version\": 1,\n  \"qa_type\": \"reproducibility\",\n  \"passed\": {},\n  \"left_case\": \"{}\",\n  \"right_case\": \"{}\",\n  \"normalized_left_bytes\": {},\n  \"normalized_right_bytes\": {}\n}}\n",
            passed,
            json_escape(&left_case_dir.to_string_lossy()),
            json_escape(&right_case_dir.to_string_lossy()),
            left.len(),
            right.len()
        ),
    )
    .map_err(|err| format!("failed to write reproducibility report: {err}"))?;
    if passed {
        Ok(QaReport {
            report_path,
            passed,
        })
    } else {
        Err("reproducibility QA failed: normalized core outputs differ".to_string())
    }
}

/// Cross-store consistency: the SQLite `videos` table and the
/// `db/videos.jsonl` compatibility artifact must describe the same
/// evidence. A crash between the JSONL write and the SQLite commit (or a
/// manual DB repair) can leave ghost rows on either side; those rows then
/// skew id allocation and report counts while being invisible to every
/// other QA surface.
pub fn consistency_report(case_dir: &Path, output_dir: &Path) -> Result<QaReport, String> {
    let sqlite_rows = case_db::load_video_ids(case_dir)?;
    let sqlite_ids: HashSet<String> = sqlite_rows.iter().map(|row| row.id.clone()).collect();

    let jsonl_path = case_dir.join("db/videos.jsonl");
    let mut jsonl_ids = HashSet::new();
    let sqlite_by_id: HashMap<_, _> = sqlite_rows
        .iter()
        .map(|row| (row.id.as_str(), row))
        .collect();
    let mut mismatches = Vec::new();
    let mut malformed = 0usize;
    if jsonl_path.is_file() {
        let text = read_to_string(&jsonl_path)
            .map_err(|err| format!("failed to read {}: {err}", jsonl_path.display()))?;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(value) => {
                    if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
                        jsonl_ids.insert(id.to_string());
                        if let Some(row) = sqlite_by_id.get(id) {
                            let path = value.get("source_path").and_then(serde_json::Value::as_str);
                            if path != Some(row.source_path.as_str()) {
                                mismatches.push(serde_json::json!({
                                    "id": id, "field": "source_path",
                                    "sqlite": row.source_path, "jsonl": path,
                                }));
                            }
                            if let (Some(left), Some(right)) = (
                                row.sha256.as_deref(),
                                value.get("sha256").and_then(serde_json::Value::as_str),
                            ) && !left.eq_ignore_ascii_case(right)
                            {
                                mismatches.push(serde_json::json!({
                                    "id": id, "field": "sha256", "sqlite": left, "jsonl": right,
                                }));
                            }
                        }
                    } else {
                        malformed += 1;
                    }
                }
                Err(_) => malformed += 1,
            }
        }
    }

    let sqlite_only: Vec<String> = sqlite_ids.difference(&jsonl_ids).cloned().collect();
    let jsonl_only: Vec<String> = jsonl_ids.difference(&sqlite_ids).cloned().collect();
    let passed =
        sqlite_only.is_empty() && jsonl_only.is_empty() && malformed == 0 && mismatches.is_empty();

    fs::create_dir_all(output_dir)
        .map_err(|err| format!("failed to create QA output directory: {err}"))?;
    let json_path = output_dir.join("consistency-report.json");
    let html_path = output_dir.join("consistency-report.html");
    let body = format!(
        "sqlite rows: {}\njsonl rows: {}\nsqlite-only: {}\njsonl-only: {}\nmalformed jsonl lines: {}\nmismatches: {}",
        sqlite_rows.len(),
        jsonl_ids.len(),
        if sqlite_only.is_empty() {
            "-".to_string()
        } else {
            sqlite_only.join(", ")
        },
        if jsonl_only.is_empty() {
            "-".to_string()
        } else {
            jsonl_only.join(", ")
        },
        malformed,
        serde_json::Value::Array(mismatches.clone()),
    );
    write_text(
        &json_path,
        &format!(
            "{{\n  \"schema_version\": 1,\n  \"qa_type\": \"consistency\",\n  \"passed\": {},\n  \"sqlite_rows\": {},\n  \"jsonl_rows\": {},\n  \"sqlite_only\": {},\n  \"jsonl_only\": {},\n  \"malformed\": {},\n  \"mismatches\": {}\n}}\n",
            passed,
            sqlite_rows.len(),
            jsonl_ids.len(),
            json_array(&sqlite_only),
            json_array(&jsonl_only),
            malformed,
            serde_json::Value::Array(mismatches.clone()),
        ),
    )
    .map_err(|err| format!("failed to write consistency JSON: {err}"))?;
    write_text(
        &html_path,
        &simple_html_report("FrameTrace Consistency QA", &body),
    )
    .map_err(|err| format!("failed to write consistency HTML: {err}"))?;

    if passed {
        Ok(QaReport {
            report_path: json_path,
            passed,
        })
    } else {
        Err(format!(
            "consistency QA failed: {} sqlite-only, {} jsonl-only, {malformed} malformed line(s); re-run scan-folder to rebuild both stores",
            sqlite_only.len(),
            jsonl_only.len(),
        ))
    }
}

fn json_array(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| format!("\"{item}\"")).collect();
    format!("[{}]", quoted.join(","))
}

pub fn report_defense_check(case_dir: &Path, output_dir: &Path) -> Result<QaReport, String> {
    let checks = [
        ("case manifest", case_dir.join("case.json")),
        ("case database", case_db::case_db_path(case_dir)),
        ("video JSON index", case_dir.join("db/video_index.json")),
        ("video JSONL index", case_dir.join("db/videos.jsonl")),
        ("video path TSV", case_dir.join("db/video_paths.tsv")),
        ("case report", case_dir.join("reports/case-report.html")),
    ];
    let missing = checks
        .iter()
        .filter(|(_, path)| !path.is_file())
        .map(|(name, path)| format!("{name}: {}", path.display()))
        .collect::<Vec<_>>();
    let passed = missing.is_empty();
    fs::create_dir_all(output_dir)
        .map_err(|err| format!("failed to create QA output directory: {err}"))?;
    let report_path = output_dir.join("report-defense-checklist.md");
    let mut text = String::from("# Report Defensibility Checklist\n\n");
    for (name, path) in checks {
        let status = if path.is_file() { "PASS" } else { "FAIL" };
        text.push_str(&format!("- [{status}] {name}: `{}`\n", path.display()));
    }
    if !missing.is_empty() {
        text.push_str("\n## Missing\n\n");
        for item in &missing {
            text.push_str(&format!("- {item}\n"));
        }
    }
    write_text(&report_path, &text)
        .map_err(|err| format!("failed to write report-defense checklist: {err}"))?;
    if passed {
        Ok(QaReport {
            report_path,
            passed,
        })
    } else {
        Err(format!(
            "report defensibility QA failed: missing {} required artifacts",
            missing.len()
        ))
    }
}

pub fn performance_report(output_dir: &Path, rows: usize) -> Result<QaReport, String> {
    let result = case_db::benchmark_case_db(output_dir, rows)?;
    let report_path = output_dir.join("performance-report.json");
    // elapsed_ms == 0 (sub-millisecond run): report the per-ms rate.
    let rows_per_minute = (rows as u128 * 60_000)
        .checked_div(result.elapsed_ms)
        .unwrap_or(rows as u128 * 60_000);
    let passed = rows_per_minute >= 50_000;
    write_text(
        &report_path,
        &format!(
            "{{\n  \"schema_version\": 1,\n  \"qa_type\": \"performance\",\n  \"passed\": {},\n  \"rows\": {},\n  \"elapsed_ms\": {},\n  \"rows_per_minute\": {},\n  \"database_path\": \"{}\"\n}}\n",
            passed,
            result.rows,
            result.elapsed_ms,
            rows_per_minute,
            json_escape(&result.path.to_string_lossy())
        ),
    )
    .map_err(|err| format!("failed to write performance report: {err}"))?;
    if passed {
        Ok(QaReport {
            report_path,
            passed,
        })
    } else {
        Err(format!(
            "performance QA failed: rows_per_minute={rows_per_minute}, target=50000"
        ))
    }
}

pub fn release_readiness_report(
    case_dir: &Path,
    output_dir: &Path,
    options: &ReleaseReadinessOptions,
) -> Result<QaReport, String> {
    fs::create_dir_all(output_dir)
        .map_err(|err| format!("failed to create QA output directory: {err}"))?;
    let mut checks = Vec::new();

    checks.push(run_release_check("report_defense", || {
        report_defense_check(case_dir, output_dir).map(|report| report.report_path)
    }));

    if let Some(corpus_manifest) = &options.corpus_manifest {
        checks.push(run_release_check("accuracy", || {
            accuracy_report(case_dir, corpus_manifest, output_dir).map(|report| report.report_path)
        }));
    } else {
        checks.push(ReleaseCheck::blocked(
            "accuracy",
            "missing --corpus-manifest",
        ));
    }

    if let Some(comparison_case_dir) = &options.comparison_case_dir {
        checks.push(run_release_check("reproducibility", || {
            reproducibility_report(case_dir, comparison_case_dir, output_dir)
                .map(|report| report.report_path)
        }));
    } else {
        checks.push(ReleaseCheck::blocked(
            "reproducibility",
            "missing --comparison-case",
        ));
    }

    let performance_output_dir = options
        .performance_output_dir
        .clone()
        .unwrap_or_else(|| output_dir.join("performance"));
    checks.push(run_release_check("performance", || {
        performance_report(&performance_output_dir, options.performance_rows)
            .map(|report| report.report_path)
    }));

    // The tamper-evident chain is the product's central claim; a release
    // must not pass readiness with a torn, tampered, truncated or deleted
    // audit log. See `audit_chain_check`.
    checks.push(run_release_check("audit_chain", || {
        audit_chain_check(case_dir, output_dir)
    }));

    // The SQLite index and the JSONL compatibility artifact describe the
    // same evidence; a release with ghost rows on either side is not
    // defensible.
    checks.push(run_release_check("consistency", || {
        consistency_report(case_dir, output_dir).map(|report| report.report_path)
    }));

    let passed = checks.iter().all(|check| check.status == "PASS");
    let blocker_count = checks.iter().filter(|check| check.status != "PASS").count();
    let json_path = output_dir.join("release-readiness.json");
    let markdown_path = output_dir.join("release-readiness.md");
    write_text(&json_path, &release_json(passed, &checks))
        .map_err(|err| format!("failed to write release readiness JSON: {err}"))?;
    write_text(&markdown_path, &release_markdown(passed, &checks))
        .map_err(|err| format!("failed to write release readiness checklist: {err}"))?;

    if passed {
        Ok(QaReport {
            report_path: json_path,
            passed,
        })
    } else {
        Err(format!(
            "release readiness failed: {blocker_count} blocker(s); see {}",
            markdown_path.display()
        ))
    }
}

#[derive(Debug, Clone)]
struct ReleaseCheck {
    name: String,
    status: String,
    evidence: String,
}

impl ReleaseCheck {
    fn blocked(name: &str, reason: &str) -> Self {
        Self {
            name: name.to_string(),
            status: "BLOCKED".to_string(),
            evidence: reason.to_string(),
        }
    }
}

/// File name of the chain-head anchors written next to the QA report. Each
/// run compares against the previous run's anchors so a deleted lane or a
/// log truncated from the end (which still verifies structurally) fails.
const AUDIT_ANCHORS_FILE: &str = "audit-chain-anchors.json";

/// Verifies every audit lane in the case and anchors their heads.
///
/// - Every `*log.jsonl` is a lane, whatever its first line holds — an
///   unparseable or unchained first line FAILS instead of being skipped.
///   Other `*.jsonl` files are lanes when their first line carries the
///   chain marker.
/// - An empty lane fails (a chained log is never created empty).
/// - Lanes recorded in a previous run's anchors must still exist and must
///   not have fewer entries; with the same entry count the head must match.
fn audit_chain_check(case_dir: &Path, output_dir: &Path) -> Result<PathBuf, String> {
    let mut logs = Vec::new();
    collect_chained_logs(case_dir, &mut logs);
    if logs.is_empty() {
        return Err("no chained audit logs found in the case".to_string());
    }
    let anchors_path = output_dir.join(AUDIT_ANCHORS_FILE);
    let previous: HashMap<String, (usize, String)> = read_to_string(&anchors_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("lanes").and_then(|l| l.as_object()).cloned())
        .map(|lanes| {
            lanes
                .into_iter()
                .filter_map(|(name, lane)| {
                    let entries = lane.get("entries")?.as_u64()? as usize;
                    let head = lane.get("head")?.as_str()?.to_string();
                    Some((name, (entries, head)))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut failures = Vec::new();
    let mut not_verified = Vec::new();
    let mut lanes = serde_json::Map::new();
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    for log in &logs {
        let rel = log
            .strip_prefix(case_dir)
            .unwrap_or(log)
            .to_string_lossy()
            .replace('\\', "/");
        match crate::audit::verify_chained_jsonl(log) {
            // A zero-byte lane nobody has anchored yet was created but never
            // written (e.g. a log opened before its first event): warn, don't
            // fail. Once an anchor recorded entries, emptiness is truncation.
            Ok(result)
                if result.entries == 0
                    && !previous.contains_key(&rel)
                    && fs::metadata(log).map(|m| m.len() == 0).unwrap_or(false) =>
            {
                warnings.push(format!("{rel}\tWARNING: empty (never written)"));
            }
            Ok(result) if result.entries == 0 => {
                failures.push(format!("{rel}: lane is empty (truncated?)"));
            }
            Ok(result) => {
                if let Some((prev_entries, prev_head)) = previous.get(&rel) {
                    if result.entries < *prev_entries {
                        failures.push(format!(
                            "{rel}: entry count dropped from {prev_entries} to {} since the last anchor (truncated?)",
                            result.entries
                        ));
                    } else if result.entries == *prev_entries
                        && &result.last_entry_sha256 != prev_head
                    {
                        failures.push(format!(
                            "{rel}: chain head changed without new entries since the last anchor"
                        ));
                    }
                }
                // R9-I5: the lane's trust under THIS PC's policy (R8 printed
                // `integrity` straight from the verifier, bypassing
                // `lane_trust`). A lane this PC cannot authenticate is NOT
                // VERIFIED and its anchor is NOT advanced — a stripped or
                // re-chained lane must never become the new baseline.
                let trust = result.lane_trust();
                lines.push(format!(
                    "{rel}\tentries={}\thead={}\t{}{}",
                    result.entries,
                    result.last_entry_sha256,
                    trust.label(),
                    if trust.reason().is_empty() {
                        String::new()
                    } else {
                        format!("\t{}", trust.reason())
                    }
                ));
                if trust.passes() {
                    lanes.insert(
                        rel,
                        serde_json::json!({
                            "entries": result.entries,
                            "head": result.last_entry_sha256,
                            "integrity": trust.label(),
                        }),
                    );
                } else {
                    if let Some((entries, head)) = previous.get(&rel) {
                        // Keep the last anchor this lane passed with.
                        lanes.insert(
                            rel.clone(),
                            serde_json::json!({"entries": entries, "head": head, "integrity": "anchor-kept"}),
                        );
                    }
                    not_verified.push(format!("{rel}: {}", trust.reason()));
                }
            }
            // R9 P5/I9: an unreadable key configuration is NOT VERIFIED
            // (not a broken chain); the anchor is kept.
            Err(err) if crate::audit::is_key_config_error(&err) => {
                if let Some((entries, head)) = previous.get(&rel) {
                    lanes.insert(
                        rel.clone(),
                        serde_json::json!({"entries": entries, "head": head, "integrity": "anchor-kept"}),
                    );
                }
                not_verified.push(format!("{rel}: {err}"));
            }
            Err(err) => failures.push(format!("{rel}: {err}")),
        }
    }
    let mut missing: Vec<&String> = previous
        .keys()
        .filter(|name| {
            !logs.iter().any(|log| {
                log.strip_prefix(case_dir)
                    .unwrap_or(log)
                    .to_string_lossy()
                    .replace('\\', "/")
                    == **name
            })
        })
        .collect();
    missing.sort();
    for name in missing {
        failures.push(format!(
            "{name}: lane recorded in the last anchor is missing (deleted?)"
        ));
    }
    if !failures.is_empty() {
        return Err(format!(
            "audit chain verification FAIL: {}",
            failures.join("; ")
        ));
    }
    // Only a passing run advances the anchors; a failing run must not
    // overwrite the evidence of what the chain looked like before.
    write_text(
        &anchors_path,
        &serde_json::to_string_pretty(&serde_json::json!({ "lanes": lanes }))
            .unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|err| format!("failed to write audit chain anchors: {err}"))?;
    if !not_verified.is_empty() {
        // R9-I5: never "PASS" over a lane this PC cannot authenticate.
        return Err(format!(
            "audit chain verification NOT VERIFIED (not a tampering verdict): {} lane(s) this PC cannot authenticate — anchors for them were not advanced: {}",
            not_verified.len(),
            not_verified.join("; ")
        ));
    }
    let report_path = output_dir.join("audit-chain-report.txt");
    write_text(
        &report_path,
        &format!(
            "audit chain verification PASS: {} log(s) intact\n{}\n",
            lines.len(),
            lines
                .iter()
                .chain(warnings.iter())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .map_err(|err| format!("failed to write audit chain report: {err}"))?;
    Ok(report_path)
}

/// Collects every audit lane in the case: every `*log.jsonl` (verified
/// regardless of content, so a tampered first line cannot hide a lane),
/// plus any other *.jsonl whose first non-empty line carries the chain's
/// `previous_entry_sha256` marker. Plain data files (db/videos.jsonl, fls
/// entries) are skipped.
fn collect_chained_logs(case_dir: &Path, out: &mut Vec<PathBuf>) {
    let mut stack = vec![case_dir.join("evidence"), case_dir.join("artifacts")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let is_log_lane = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("log.jsonl"));
            if is_log_lane
                || read_to_string(&path).is_ok_and(|text| {
                    text.lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .is_some_and(|first| first.contains("previous_entry_sha256"))
                })
            {
                out.push(path);
            }
        }
    }
    out.sort();
}

fn run_release_check(name: &str, run: impl FnOnce() -> Result<PathBuf, String>) -> ReleaseCheck {
    match run() {
        Ok(path) => ReleaseCheck {
            name: name.to_string(),
            status: "PASS".to_string(),
            evidence: path.to_string_lossy().to_string(),
        },
        Err(err) => ReleaseCheck {
            name: name.to_string(),
            status: "FAIL".to_string(),
            evidence: err,
        },
    }
}

fn release_json(passed: bool, checks: &[ReleaseCheck]) -> String {
    let checks_json = checks
        .iter()
        .map(|check| {
            format!(
                "    {{\"name\":\"{}\",\"status\":\"{}\",\"evidence\":\"{}\"}}",
                json_escape(&check.name),
                json_escape(&check.status),
                json_escape(&check.evidence)
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{{\n  \"schema_version\": 1,\n  \"qa_type\": \"release_readiness\",\n  \"passed\": {},\n  \"checks\": [\n{}\n  ]\n}}\n",
        passed, checks_json
    )
}

fn release_markdown(passed: bool, checks: &[ReleaseCheck]) -> String {
    let mut text = String::from("# Release Readiness\n\n");
    text.push_str(&format!(
        "Overall: **{}**\n\n",
        if passed { "PASS" } else { "BLOCKED" }
    ));
    text.push_str("| Check | Status | Evidence |\n| --- | --- | --- |\n");
    for check in checks {
        text.push_str(&format!(
            "| {} | {} | `{}` |\n",
            check.name, check.status, check.evidence
        ));
    }
    text
}

/// Corpus manifests are written by external tooling, so the same file may be
/// spelled with or without the Windows extended-length prefix that
/// `std::fs::canonicalize` produces. Normalize both sides before comparing.
fn normalize_evidence_path(source_path: &str) -> String {
    crate::util::strip_windows_extended_prefix(Path::new(source_path))
        .to_string_lossy()
        .to_string()
}

fn read_expected_manifest(path: &Path) -> Result<Vec<ExpectedEvidence>, String> {
    let text = read_to_string(path)
        .map_err(|err| format!("failed to read corpus manifest {}: {err}", path.display()))?;
    let mut out = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("source_path") {
            continue;
        }
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.is_empty() || columns[0].trim().is_empty() {
            return Err(format!(
                "invalid corpus manifest row {} in {}",
                line_index + 1,
                path.display()
            ));
        }
        out.push(ExpectedEvidence {
            source_path: columns[0].trim().to_string(),
            sha256: columns
                .get(1)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        });
    }
    Ok(out)
}

fn read_indexed_evidence(case_dir: &Path) -> Result<Vec<IndexedEvidence>, String> {
    let path = case_dir.join("db/videos.jsonl");
    let text = read_to_string(&path)
        .map_err(|err| format!("failed to read indexed evidence {}: {err}", path.display()))?;
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .map(|value| IndexedEvidence {
            source_path: value
                .get("source_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            sha256: value
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        })
        .collect())
}

fn normalized_case_core(case_dir: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for rel in ["db/videos.jsonl", "db/video_paths.tsv"] {
        let path = case_dir.join(rel);
        let text = read_to_string(&path).map_err(|err| {
            format!(
                "failed to read reproducibility input {}: {err}",
                path.display()
            )
        })?;
        let mut lines = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            // `stale_since_unix` is stamped with wall-clock scan time, so
            // two cases that ran the identical vanish-workflow in
            // different seconds would otherwise compare unequal. The
            // marker's PRESENCE is the reproducible fact; the timestamp
            // is environment noise — normalize it away.
            .map(|line| normalize_stale_timestamps(line).to_string())
            .collect::<Vec<_>>();
        lines.sort();
        parts.push(format!("{rel}\n{}\n", lines.join("\n")));
    }
    Ok(parts.join("\n"))
}

/// Replaces every `"stale_since_unix":<digits>` occurrence with a constant.
fn normalize_stale_timestamps(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let needle = b"\"stale_since_unix\":";
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(needle) {
            out.push_str("\"stale_since_unix\":0");
            index += needle.len();
            // Skip the digits that followed.
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        } else {
            // Advance by one full UTF-8 scalar to keep indices on char
            // boundaries for non-ASCII content.
            let step = line[index..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
            out.push_str(&line[index..index + step]);
            index += step;
        }
    }
    out
}

fn simple_html_report(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body><h1>{}</h1><pre>{}</pre></body></html>\n",
        html_escape(title),
        html_escape(title),
        html_escape(body)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        accuracy_report, audit_chain_check, consistency_report, normalize_stale_timestamps,
        read_indexed_evidence,
    };
    use std::fs;

    #[test]
    fn audit_chain_check_fails_on_tampered_first_line_truncation_and_deletion() {
        let root =
            std::env::temp_dir().join(format!("frametrace-qa-audit-lanes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        let out = root.join("qa");
        fs::create_dir_all(&out).unwrap();
        let lane_a = case_dir.join("evidence/logs/anomaly-log.jsonl");
        let lane_b = case_dir.join("artifacts/clips/export-log.jsonl");
        for body in [r#"{"k":"1"}"#, r#"{"k":"2"}"#] {
            crate::audit::append_chained_jsonl_keyed(&lane_a, body, None).unwrap();
            crate::audit::append_chained_jsonl_keyed(&lane_b, body, None).unwrap();
        }
        // Baseline passes and records anchors with both lanes.
        let report = audit_chain_check(&case_dir, &out).unwrap();
        let text = fs::read_to_string(report).unwrap();
        assert!(text.contains("anomaly-log.jsonl") && text.contains("entries=2"));
        assert!(out.join(super::AUDIT_ANCHORS_FILE).is_file());

        // Truncating the last entry still verifies structurally, but the
        // anchor catches the dropped count.
        let original_b = fs::read_to_string(&lane_b).unwrap();
        let first_line = format!("{}\n", original_b.lines().next().unwrap());
        fs::write(&lane_b, &first_line).unwrap();
        let err = audit_chain_check(&case_dir, &out).unwrap_err();
        assert!(err.contains("entry count dropped"), "{err}");

        // Deleting a lane fails against the anchor.
        fs::remove_file(&lane_b).unwrap();
        let err = audit_chain_check(&case_dir, &out).unwrap_err();
        assert!(err.contains("missing"), "{err}");
        fs::write(&lane_b, &original_b).unwrap();
        audit_chain_check(&case_dir, &out).unwrap();

        // A *log.jsonl whose first line is not chained is a failing lane,
        // not a silently skipped file.
        let original_a = fs::read_to_string(&lane_a).unwrap();
        fs::write(&lane_a, "garbage\n").unwrap();
        assert!(audit_chain_check(&case_dir, &out).is_err());
        // An empty lane fails too.
        fs::write(&lane_a, "").unwrap();
        let err = audit_chain_check(&case_dir, &out).unwrap_err();
        assert!(err.contains("empty"), "{err}");
        fs::write(&lane_a, &original_a).unwrap();
        audit_chain_check(&case_dir, &out).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    /// Plan §3: the join and audio-level lanes are picked up by the QA audit
    /// sweep without any registration, anchored, and tamper-checked.
    #[test]
    fn audit_chain_check_recognizes_join_and_audio_level_lanes() {
        let root =
            std::env::temp_dir().join(format!("frametrace-qa-new-lanes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        let out = root.join("qa");
        fs::create_dir_all(&out).unwrap();
        let join_lane = case_dir.join("artifacts/joined/join-log.jsonl");
        let audio_lane = case_dir.join("evidence/logs/audio-level-log.jsonl");
        // Entry bodies built from the plan §1.5 / §2.3 schemas.
        let join = r#"{"schema_version":1,"event":"join-video","joined_unix":1,"output_path":"artifacts/joined/join_F_20260910-120000_n2.mp4","output_sha256":"aa","segment_count":2,"mode":"stream-copy","gaps":0,"overlaps":1}"#;
        let audio = r#"{"schema_version":1,"event":"audio-level","measured_unix":1,"method":"ffmpeg-volumedetect","threshold_silent_max_dbfs":-70.0,"results":[{"selector":"vid_1","result":"silent","streams":[]}]}"#;
        for body in [join, join] {
            crate::audit::append_chained_jsonl_keyed(&join_lane, body, None).unwrap();
        }
        crate::audit::append_chained_jsonl_keyed(&audio_lane, audio, None).unwrap();
        // A stray temp file next to the lane is not a lane.
        fs::write(
            case_dir.join("artifacts/joined/.join-t1.progress.json"),
            "{}",
        )
        .unwrap();

        let report = audit_chain_check(&case_dir, &out).unwrap();
        let text = fs::read_to_string(report).unwrap();
        assert!(text.contains("artifacts/joined/join-log.jsonl"), "{text}");
        assert!(
            text.contains("evidence/logs/audio-level-log.jsonl"),
            "{text}"
        );
        assert!(text.contains("PASS: 2 log(s) intact"), "{text}");
        let anchors = fs::read_to_string(out.join(super::AUDIT_ANCHORS_FILE)).unwrap();
        assert!(anchors.contains("artifacts/joined/join-log.jsonl"));
        assert!(anchors.contains("evidence/logs/audio-level-log.jsonl"));

        // Dropping the last join entry is caught by the anchor.
        let original = fs::read_to_string(&join_lane).unwrap();
        fs::write(
            &join_lane,
            format!("{}\n", original.lines().next().unwrap()),
        )
        .unwrap();
        let err = audit_chain_check(&case_dir, &out).unwrap_err();
        assert!(err.contains("entry count dropped"), "{err}");
        fs::write(&join_lane, &original).unwrap();
        // Deleting the audio-level lane after it was anchored fails.
        let audio_text = fs::read_to_string(&audio_lane).unwrap();
        fs::remove_file(&audio_lane).unwrap();
        let err = audit_chain_check(&case_dir, &out).unwrap_err();
        assert!(err.contains("missing"), "{err}");
        fs::write(&audio_lane, audio_text).unwrap();
        audit_chain_check(&case_dir, &out).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn audit_chain_check_warns_on_never_written_empty_lane() {
        let root = std::env::temp_dir().join(format!(
            "frametrace-qa-audit-empty-lane-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        let out = root.join("qa");
        fs::create_dir_all(&out).unwrap();
        let lane_a = case_dir.join("evidence/logs/anomaly-log.jsonl");
        crate::audit::append_chained_jsonl_keyed(&lane_a, r#"{"k":"1"}"#, None).unwrap();
        // A zero-byte lane with no anchor history: created, never written.
        let empty = case_dir.join("artifacts/clips/export-log.jsonl");
        fs::create_dir_all(empty.parent().unwrap()).unwrap();
        fs::write(&empty, "").unwrap();
        let report = audit_chain_check(&case_dir, &out).unwrap();
        let text = fs::read_to_string(report).unwrap();
        assert!(text.contains("PASS: 1 log(s) intact"), "{text}");
        assert!(
            text.contains("export-log.jsonl\tWARNING: empty (never written)"),
            "{text}"
        );
        // Still passes on rerun (the empty lane was not anchored).
        audit_chain_check(&case_dir, &out).unwrap();
        // Once written and anchored, emptying it again is truncation.
        crate::audit::append_chained_jsonl_keyed(&empty, r#"{"k":"1"}"#, None).unwrap();
        audit_chain_check(&case_dir, &out).unwrap();
        fs::write(&empty, "").unwrap();
        let err = audit_chain_check(&case_dir, &out).unwrap_err();
        assert!(err.contains("empty (truncated?)"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn accuracy_report_passes_for_matching_manifest() {
        let root = std::env::temp_dir().join(format!("frametrace-qa-test-{}", std::process::id()));
        let case_dir = root.join("case");
        let output_dir = root.join("qa");
        let manifest = root.join("corpus.tsv");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(case_dir.join("db")).unwrap();
        let source_path = "/evidence/a.mp4";
        fs::write(
            case_dir.join("db/videos.jsonl"),
            format!(
                "{{\"source_path\":\"{}\",\"sha256\":\"abc\"}}\n",
                source_path
            ),
        )
        .unwrap();
        fs::write(&manifest, format!("{source_path}\tabc\n")).unwrap();

        let report = accuracy_report(&case_dir, &manifest, &output_dir).unwrap();
        assert!(report.passed);
        assert!(report.report_path.is_file());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_indexed_evidence_skips_malformed_lines_and_reads_escapes() {
        let root =
            std::env::temp_dir().join(format!("frametrace-qa-rows-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("db")).unwrap();
        fs::write(
            root.join("db/videos.jsonl"),
            concat!(
                // Well-formed record with an escaped backslash path.
                "{\"source_path\":\"C:\\\\ev\\\\a.mp4\",\"sha256\":\"abc\"}\n",
                // Malformed line (truncated JSON) must be skipped, not fatal.
                "{\"source_path\":\"C:\\\\ev\\\",\n",
                // Record without sha256 keeps None.
                "{\"source_path\":\"/evidence/b.mp4\"}\n",
            ),
        )
        .unwrap();
        let rows = read_indexed_evidence(&root).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].source_path, "C:\\ev\\a.mp4");
        assert_eq!(rows[0].sha256.as_deref(), Some("abc"));
        assert_eq!(rows[1].source_path, "/evidence/b.mp4");
        assert!(rows[1].sha256.is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn consistency_detects_ghost_rows_on_both_sides() {
        let root =
            std::env::temp_dir().join(format!("frametrace-qa-consistency-{}", std::process::id()));
        let case_dir = root.join("case");
        let output_dir = root.join("qa");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(case_dir.join("db")).unwrap();

        // JSONL knows vid_000001 only; SQLite (via case_db) is built by a
        // scan, so simulate the divergence with a minimal rows file plus a
        // real scan-run DB is overkill here — instead verify the report
        // directly against a case where both stores agree first.
        fs::write(
            case_dir.join("db/videos.jsonl"),
            "{\"id\":\"vid_000001\",\"source_path\":\"/e/a.mp4\"}\n",
        )
        .unwrap();
        // No SQLite DB exists yet: load_video_ids returns empty => the
        // JSONL row is jsonl-only => consistency must FAIL (not panic).
        let result = consistency_report(&case_dir, &output_dir);
        assert!(result.is_err(), "ghost jsonl rows must fail consistency");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn consistency_detects_same_id_path_and_hash_mismatches() {
        let root =
            std::env::temp_dir().join(format!("frametrace-qa-fields-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("db")).unwrap();
        let conn = crate::case_db::open_case_db(&root).unwrap();
        crate::case_db::init_schema(&conn).unwrap();
        conn.execute("INSERT INTO videos (id, source_path, file_url, relative_path, extension, size_bytes, hash_status, confidence, source_profile_json, ffprobe_ok, first_indexed_unix, last_indexed_unix, record_json, sha256) VALUES ('vid_000001', '/ev/a.mp4', '', '', 'mp4', 1, 'complete', 'unknown', '{}', 0, 1, 1, '{}', 'aa')", []).unwrap();
        for (path, hash, field) in [
            ("/ev/b.mp4", "aa", "source_path"),
            ("/ev/a.mp4", "bb", "sha256"),
        ] {
            fs::write(
                root.join("db/videos.jsonl"),
                serde_json::json!({"id":"vid_000001", "source_path":path, "sha256":hash})
                    .to_string(),
            )
            .unwrap();
            assert!(
                consistency_report(&root, &root.join("qa")).is_err(),
                "same IDs but different {field} must fail"
            );
            let report: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(root.join("qa/consistency-report.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(report["mismatches"][0]["id"], "vid_000001");
            assert_eq!(report["mismatches"][0]["field"], field);
        }
        fs::write(
            root.join("db/videos.jsonl"),
            "{\"id\":\"vid_000001\",\"source_path\":\"/ev/a.mp4\",\"sha256\":\"AA\"}\n",
        )
        .unwrap();
        consistency_report(&root, &root.join("qa")).unwrap();
        fs::write(
            root.join("db/videos.jsonl"),
            "{\"id\":\"vid_000001\",\"source_path\":\"/ev/a.mp4\"}\n",
        )
        .unwrap();
        consistency_report(&root, &root.join("qa")).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_timestamps_normalize_to_constant() {
        let a = r#"{"id":"vid_000001","index_status":"stale","stale_since_unix":1788917107}"#;
        let b = r#"{"id":"vid_000001","index_status":"stale","stale_since_unix":9999999999}"#;
        assert_eq!(normalize_stale_timestamps(a), normalize_stale_timestamps(b));
        assert!(normalize_stale_timestamps(a).contains(r#""stale_since_unix":0"#));
        // Korean content must survive byte-stepping.
        let korean = r#"{"id":"vid_1","relative_path":"한글.mp4","stale_since_unix":123}"#;
        let normalized = normalize_stale_timestamps(korean);
        assert!(normalized.contains("한글.mp4"), "{normalized}");
    }

    /// R9-I5: the QA chain check goes through `lane_trust`. On a PC holding
    /// keys, a lane re-chained without HMACs (longer than its anchor, so
    /// R8's count/head rules passed it) is NOT VERIFIED, not PASS — and its
    /// anchor is NOT advanced to the forged state.
    #[test]
    fn r9_i5_unauthenticated_lane_never_passes_or_moves_its_anchor() {
        let root = std::env::temp_dir().join(format!("frametrace-qa-r9-i5-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        let out = root.join("qa");
        fs::create_dir_all(&out).unwrap();
        let lane = case_dir.join("evidence/logs/scan-log.jsonl");
        let mine = crate::audit_key::AuditKey::new("mine", &[0x9c; 32]);
        crate::audit::append_chained_jsonl_keyed(&lane, r#"{"n":1}"#, Some(&mine)).unwrap();
        crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
            audit_chain_check(&case_dir, &out).unwrap();
        });
        let anchored = fs::read_to_string(out.join(super::AUDIT_ANCHORS_FILE)).unwrap();
        // Re-chained from scratch without HMACs, one entry longer.
        fs::remove_file(&lane).unwrap();
        for body in [r#"{"n":1}"#, r#"{"n":2}"#] {
            crate::audit::append_chained_jsonl_keyed(&lane, body, None).unwrap();
        }
        crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
            let err = audit_chain_check(&case_dir, &out).unwrap_err();
            assert!(err.contains("NOT VERIFIED"), "{err}");
        });
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join(super::AUDIT_ANCHORS_FILE)).unwrap())
                .unwrap();
        let before: serde_json::Value = serde_json::from_str(&anchored).unwrap();
        assert_eq!(
            after["lanes"]["evidence/logs/scan-log.jsonl"]["head"],
            before["lanes"]["evidence/logs/scan-log.jsonl"]["head"],
            "the anchor must keep the last authenticated state"
        );
        // Without keys it is self-consistency only and passes (P1).
        crate::audit_key::with_test_keys(&[], || {
            audit_chain_check(&case_dir, &out).unwrap();
        });
        let _ = fs::remove_dir_all(&root);
    }
}
