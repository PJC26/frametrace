//! Candidate-grade diff of two case video indexes (`db/videos.jsonl`).
//!
//! Reports which indexed files are present in both cases (matched by sha256
//! when both sides carry one, else by path+size), which appear on only one
//! side, and where the same source path carries different hashes. The output
//! is a JSON report written inside the invoking case and a chained entry in
//! `evidence/logs/case-compare-log.jsonl`. Findings are candidate-grade: an
//! index row is a recorded claim, not re-verified content.

use crate::audit;
use crate::util::{json_escape, now_unix};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Candidate-grade label: comparisons reflect recorded index rows only.
pub const LABEL: &str = "candidate-comparison";

#[derive(Debug, Clone)]
struct CaseRow {
    id: String,
    source_path: String,
    /// Path identity used for matching: the lossless raw encoding for a
    /// non-Unicode name (E1), else the display path.
    path_key: String,
    size_bytes: Option<u64>,
    sha256: Option<String>,
    /// Marked stale by a later rescan of that case (file was absent).
    stale: bool,
}

#[derive(Debug, Clone)]
struct BothEntry {
    id_a: String,
    id_b: String,
    path_a: String,
    path_b: String,
    matched_by: &'static str,
    sha256: Option<String>,
    stale_a: bool,
    stale_b: bool,
}

#[derive(Debug, Clone)]
struct SideEntry {
    id: String,
    path: String,
    size_bytes: Option<u64>,
    sha256: Option<String>,
    stale: bool,
}

#[derive(Debug, Clone)]
struct MismatchEntry {
    id_a: String,
    id_b: String,
    path: String,
    sha256_a: String,
    sha256_b: String,
    stale_a: bool,
    stale_b: bool,
}

/// What part of one case index the comparison actually covered (E8).
#[derive(Debug, Clone, Default)]
struct Coverage {
    rows: usize,
    stale: usize,
    skipped_lines: usize,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CompareResult {
    pub report_path: PathBuf,
    pub generated_unix: u64,
    pub both_count: usize,
    pub only_in_a_count: usize,
    pub only_in_b_count: usize,
    pub hash_mismatch_count: usize,
    /// Index rows of case A / B marked stale (compared, but flagged).
    pub stale_rows_a: usize,
    pub stale_rows_b: usize,
    /// Index lines of case A / B that could not be read into a row; a
    /// non-zero value means that side was compared incompletely.
    pub skipped_index_lines_a: usize,
    pub skipped_index_lines_b: usize,
    pub warnings: Vec<String>,
}

/// Diff case B's index against case A's, write the JSON report inside case A,
/// and append a chained audit entry. `output_path` must resolve inside
/// `case_dir` (enforced via `require_case_output_path`).
pub fn compare_cases(
    case_dir: &Path,
    other_case_dir: &Path,
    output_path: &Path,
) -> Result<CompareResult, String> {
    crate::tool_policy::require_case_output_path(case_dir, output_path, "case comparison")?;
    let (rows_a, coverage_a) = read_case_rows(case_dir)?;
    let (rows_b, coverage_b) = read_case_rows(other_case_dir)?;
    let (both, only_a, only_b, mismatches) = diff_rows(&rows_a, &rows_b);
    let generated_unix = now_unix()?;
    let mut warnings = Vec::new();
    warnings.extend(coverage_a.warnings.iter().map(|w| format!("case A: {w}")));
    warnings.extend(coverage_b.warnings.iter().map(|w| format!("case B: {w}")));
    let report = report_json(
        ReportHeader {
            case_dir,
            other_case_dir,
            generated_unix,
            coverage_a: &coverage_a,
            coverage_b: &coverage_b,
            warnings: &warnings,
        },
        &both,
        &only_a,
        &only_b,
        &mismatches,
    );
    crate::tool_policy::write_case_report(
        case_dir,
        output_path,
        "reports/case-compare.json",
        "case comparison",
        &report,
    )?;
    let line = format!(
        "{{\"schema_version\":1,\"event\":\"compare-cases\",\"generated_unix\":{},\"label\":\"{}\",\"case_a\":\"{}\",\"case_b\":\"{}\",\"report_path\":\"{}\",\"both\":{},\"only_in_a\":{},\"only_in_b\":{},\"hash_mismatch_same_path\":{},\"stale_rows_a\":{},\"stale_rows_b\":{},\"skipped_index_lines_a\":{},\"skipped_index_lines_b\":{}}}",
        generated_unix,
        LABEL,
        json_escape(&case_dir.to_string_lossy()),
        json_escape(&other_case_dir.to_string_lossy()),
        json_escape(&output_path.to_string_lossy()),
        both.len(),
        only_a.len(),
        only_b.len(),
        mismatches.len(),
        coverage_a.stale,
        coverage_b.stale,
        coverage_a.skipped_lines,
        coverage_b.skipped_lines,
    );
    audit::append_chained_jsonl(
        &case_dir.join("evidence/logs/case-compare-log.jsonl"),
        &line,
    )?;
    Ok(CompareResult {
        report_path: output_path.to_path_buf(),
        generated_unix,
        both_count: both.len(),
        only_in_a_count: only_a.len(),
        only_in_b_count: only_b.len(),
        hash_mismatch_count: mismatches.len(),
        stale_rows_a: coverage_a.stale,
        stale_rows_b: coverage_b.stale,
        skipped_index_lines_a: coverage_a.skipped_lines,
        skipped_index_lines_b: coverage_b.skipped_lines,
        warnings,
    })
}

/// E8: the shared index reader counts unparseable lines and flags stale
/// rows instead of silently dropping / presenting them as present files.
fn read_case_rows(case_dir: &Path) -> Result<(Vec<CaseRow>, Coverage), String> {
    let index = crate::scan::read_index_rows(case_dir)?;
    let coverage = Coverage {
        rows: index.rows.len(),
        stale: index.stale_count(),
        skipped_lines: index.skipped_lines(),
        warnings: index.warnings(),
    };
    let rows = index
        .rows
        .into_iter()
        .map(|row| CaseRow {
            path_key: row
                .source_path_raw_hex
                .clone()
                .unwrap_or_else(|| row.source_path.clone()),
            id: row.id,
            source_path: row.source_path,
            size_bytes: row.size_bytes,
            sha256: row.sha256,
            stale: row.stale,
        })
        .collect();
    Ok((rows, coverage))
}

/// Match A rows against B rows. sha256 identity wins when both sides carry a
/// hash (content equality even across renamed paths); otherwise a row pairs
/// on identical path+size. A same-path pair whose recorded hashes differ is a
/// `hash-mismatch-same-path` candidate, not a match.
fn diff_rows(
    rows_a: &[CaseRow],
    rows_b: &[CaseRow],
) -> (
    Vec<BothEntry>,
    Vec<SideEntry>,
    Vec<SideEntry>,
    Vec<MismatchEntry>,
) {
    let mut b_by_sha: HashMap<String, Vec<usize>> = HashMap::new();
    let mut b_by_path: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, row) in rows_b.iter().enumerate() {
        if let Some(sha) = row.sha256.as_deref() {
            b_by_sha
                .entry(sha.to_ascii_lowercase())
                .or_default()
                .push(index);
        }
        b_by_path
            .entry(row.path_key.as_str())
            .or_default()
            .push(index);
    }
    let mut b_matched = vec![false; rows_b.len()];

    let mut both = Vec::new();
    let mut only_a = Vec::new();
    let mut mismatches = Vec::new();

    'next_a: for a_row in rows_a {
        // 1) Content identity: same sha256 on both sides.
        if let Some(sha) = a_row.sha256.as_deref() {
            let key = sha.to_ascii_lowercase();
            if let Some(candidates) = b_by_sha.get_mut(&key) {
                while let Some(&index) = candidates.first() {
                    candidates.remove(0);
                    if b_matched[index] {
                        continue;
                    }
                    b_matched[index] = true;
                    let b_row = &rows_b[index];
                    both.push(BothEntry {
                        id_a: a_row.id.clone(),
                        id_b: b_row.id.clone(),
                        path_a: a_row.source_path.clone(),
                        path_b: b_row.source_path.clone(),
                        matched_by: "sha256",
                        sha256: Some(sha.to_string()),
                        stale_a: a_row.stale,
                        stale_b: b_row.stale,
                    });
                    continue 'next_a;
                }
            }
        }
        // 2) Same source path: equal hashes cannot reach here (step 1 would
        // have matched), so a hashed B row means a mismatch; an unhashed B
        // row pairs on path+size.
        if let Some(candidates) = b_by_path.get(a_row.path_key.as_str()) {
            let mut size_pair: Option<usize> = None;
            let mut mismatch: Option<usize> = None;
            for &index in candidates {
                if b_matched[index] {
                    continue;
                }
                let b_row = &rows_b[index];
                match (&a_row.sha256, &b_row.sha256) {
                    (Some(sha_a), Some(sha_b)) => {
                        if !sha_a.eq_ignore_ascii_case(sha_b) && mismatch.is_none() {
                            mismatch = Some(index);
                        }
                    }
                    _ => {
                        if size_pair.is_none() && b_row.size_bytes == a_row.size_bytes {
                            size_pair = Some(index);
                        }
                    }
                }
            }
            // A same-path hash conflict outranks a weaker path+size pairing:
            // it is the recorded evidence that the file changed.
            if let Some(index) = mismatch.or(size_pair) {
                b_matched[index] = true;
                let b_row = &rows_b[index];
                if mismatch == Some(index) {
                    mismatches.push(MismatchEntry {
                        id_a: a_row.id.clone(),
                        id_b: b_row.id.clone(),
                        path: a_row.source_path.clone(),
                        sha256_a: a_row.sha256.clone().unwrap_or_default(),
                        sha256_b: b_row.sha256.clone().unwrap_or_default(),
                        stale_a: a_row.stale,
                        stale_b: b_row.stale,
                    });
                } else {
                    both.push(BothEntry {
                        id_a: a_row.id.clone(),
                        id_b: b_row.id.clone(),
                        path_a: a_row.source_path.clone(),
                        path_b: b_row.source_path.clone(),
                        matched_by: "path+size",
                        sha256: a_row.sha256.clone().or_else(|| b_row.sha256.clone()),
                        stale_a: a_row.stale,
                        stale_b: b_row.stale,
                    });
                }
                continue;
            }
        }
        only_a.push(SideEntry {
            id: a_row.id.clone(),
            path: a_row.source_path.clone(),
            size_bytes: a_row.size_bytes,
            sha256: a_row.sha256.clone(),
            stale: a_row.stale,
        });
    }

    let mut only_b = Vec::new();
    for (index, b_row) in rows_b.iter().enumerate() {
        if !b_matched[index] {
            only_b.push(SideEntry {
                id: b_row.id.clone(),
                path: b_row.source_path.clone(),
                size_bytes: b_row.size_bytes,
                sha256: b_row.sha256.clone(),
                stale: b_row.stale,
            });
        }
    }

    // Deterministic ordering: repeated runs over unchanged indexes must emit
    // byte-identical reports.
    both.sort_by(|a, b| a.path_a.cmp(&b.path_a).then_with(|| a.id_a.cmp(&b.id_a)));
    only_a.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.id.cmp(&b.id)));
    only_b.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.id.cmp(&b.id)));
    mismatches.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.id_a.cmp(&b.id_a)));
    (both, only_a, only_b, mismatches)
}

fn optional_string_json(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

fn optional_u64_json(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

struct ReportHeader<'a> {
    case_dir: &'a Path,
    other_case_dir: &'a Path,
    generated_unix: u64,
    coverage_a: &'a Coverage,
    coverage_b: &'a Coverage,
    warnings: &'a [String],
}

fn coverage_json(coverage: &Coverage) -> String {
    format!(
        "{{\"rows\": {}, \"stale_rows\": {}, \"skipped_index_lines\": {}}}",
        coverage.rows, coverage.stale, coverage.skipped_lines
    )
}

fn report_json(
    header: ReportHeader<'_>,
    both: &[BothEntry],
    only_a: &[SideEntry],
    only_b: &[SideEntry],
    mismatches: &[MismatchEntry],
) -> String {
    let both_json = both
        .iter()
        .map(|entry| {
            format!(
                "{{\"id_a\":\"{}\",\"id_b\":\"{}\",\"path_a\":\"{}\",\"path_b\":\"{}\",\"matched_by\":\"{}\",\"sha256\":{},\"stale_a\":{},\"stale_b\":{}}}",
                json_escape(&entry.id_a),
                json_escape(&entry.id_b),
                json_escape(&entry.path_a),
                json_escape(&entry.path_b),
                entry.matched_by,
                optional_string_json(entry.sha256.as_deref()),
                entry.stale_a,
                entry.stale_b,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let side_json = |entries: &[SideEntry]| {
        entries
            .iter()
            .map(|entry| {
                format!(
                    "{{\"id\":\"{}\",\"path\":\"{}\",\"size_bytes\":{},\"sha256\":{},\"stale\":{}}}",
                    json_escape(&entry.id),
                    json_escape(&entry.path),
                    optional_u64_json(entry.size_bytes),
                    optional_string_json(entry.sha256.as_deref()),
                    entry.stale,
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    let mismatch_json = mismatches
        .iter()
        .map(|entry| {
            format!(
                "{{\"id_a\":\"{}\",\"id_b\":\"{}\",\"path\":\"{}\",\"sha256_a\":\"{}\",\"sha256_b\":\"{}\",\"stale_a\":{},\"stale_b\":{}}}",
                json_escape(&entry.id_a),
                json_escape(&entry.id_b),
                json_escape(&entry.path),
                json_escape(&entry.sha256_a),
                json_escape(&entry.sha256_b),
                entry.stale_a,
                entry.stale_b,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\n  \"schema_version\": 1,\n  \"label\": \"{}\",\n  \"generated_unix\": {},\n  \"case_a\": \"{}\",\n  \"case_b\": \"{}\",\n  \"summary\": {{\"both\": {}, \"only_in_a\": {}, \"only_in_b\": {}, \"hash_mismatch_same_path\": {}}},\n  \"index_coverage\": {{\"a\": {}, \"b\": {}}},\n  \"warnings\": [{}],\n  \"both\": [{}],\n  \"only_in_a\": [{}],\n  \"only_in_b\": [{}],\n  \"hash_mismatch_same_path\": [{}]\n}}\n",
        LABEL,
        header.generated_unix,
        json_escape(&header.case_dir.to_string_lossy()),
        json_escape(&header.other_case_dir.to_string_lossy()),
        both.len(),
        only_a.len(),
        only_b.len(),
        mismatches.len(),
        coverage_json(header.coverage_a),
        coverage_json(header.coverage_b),
        header
            .warnings
            .iter()
            .map(|warning| format!("\"{}\"", json_escape(warning)))
            .collect::<Vec<_>>()
            .join(","),
        both_json,
        side_json(only_a),
        side_json(only_b),
        mismatch_json,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::read_to_string;
    use std::fs;

    fn temp_case(name: &str, videos_jsonl: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("frametrace-compare-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("db")).unwrap();
        fs::write(dir.join("case.json"), "{}").unwrap();
        fs::write(dir.join("db/videos.jsonl"), videos_jsonl).unwrap();
        dir
    }

    fn row(id: &str, path: &str, size: u64, sha: Option<&str>) -> String {
        format!(
            "{{\"id\":\"{id}\",\"source_path\":\"{path}\",\"size_bytes\":{size},\"sha256\":{}}}\n",
            sha.map(|s| format!("\"{s}\""))
                .unwrap_or_else(|| "null".into())
        )
    }

    #[test]
    fn identical_indexes_report_everything_in_both_and_empty_diff() {
        let rows = format!(
            "{}{}",
            row("vid_1", "/ev/a.mp4", 10, Some("aa")),
            row("vid_2", "/ev/b.mp4", 20, Some("bb")),
        );
        let case_a = temp_case("ident-a", &rows);
        let case_b = temp_case("ident-b", &rows);
        let output = case_a.join("reports/case-compare.json");
        let result = compare_cases(&case_a, &case_b, &output).unwrap();
        assert_eq!(result.both_count, 2);
        assert_eq!(result.only_in_a_count, 0);
        assert_eq!(result.only_in_b_count, 0);
        assert_eq!(result.hash_mismatch_count, 0);
        let report = read_to_string(&output).unwrap();
        assert!(report.contains("\"matched_by\":\"sha256\""));
        assert!(report.contains(LABEL));
        let verification =
            audit::verify_chained_jsonl(&case_a.join("evidence/logs/case-compare-log.jsonl"))
                .unwrap();
        assert_eq!(verification.entries, 1);
        let _ = fs::remove_dir_all(&case_a);
        let _ = fs::remove_dir_all(&case_b);
    }

    #[test]
    fn disjoint_indexes_report_only_in_each_side() {
        let case_a = temp_case("disj-a", &row("vid_1", "/ev/a.mp4", 10, Some("aa")));
        let case_b = temp_case("disj-b", &row("vid_9", "/ev/z.mp4", 99, Some("zz")));
        let output = case_a.join("reports/case-compare.json");
        let result = compare_cases(&case_a, &case_b, &output).unwrap();
        assert_eq!(result.both_count, 0);
        assert_eq!(result.only_in_a_count, 1);
        assert_eq!(result.only_in_b_count, 1);
        assert_eq!(result.hash_mismatch_count, 0);
        let _ = fs::remove_dir_all(&case_a);
        let _ = fs::remove_dir_all(&case_b);
    }

    #[test]
    fn same_path_different_hash_is_a_mismatch_not_a_match() {
        let case_a = temp_case("mm-a", &row("vid_1", "/ev/a.mp4", 10, Some("aa")));
        let case_b = temp_case("mm-b", &row("vid_7", "/ev/a.mp4", 10, Some("cc")));
        let output = case_a.join("reports/case-compare.json");
        let result = compare_cases(&case_a, &case_b, &output).unwrap();
        assert_eq!(result.both_count, 0);
        assert_eq!(result.only_in_a_count, 0);
        assert_eq!(result.only_in_b_count, 0);
        assert_eq!(result.hash_mismatch_count, 1);
        let report = read_to_string(&output).unwrap();
        assert!(report.contains("\"sha256_a\":\"aa\""));
        assert!(report.contains("\"sha256_b\":\"cc\""));
        let _ = fs::remove_dir_all(&case_a);
        let _ = fs::remove_dir_all(&case_b);
    }

    #[test]
    fn same_hash_different_path_pairs_by_content() {
        let case_a = temp_case("ren-a", &row("vid_1", "/ev/a.mp4", 10, Some("aa")));
        let case_b = temp_case("ren-b", &row("vid_3", "/ev/renamed.mp4", 10, Some("aa")));
        let output = case_a.join("reports/case-compare.json");
        let result = compare_cases(&case_a, &case_b, &output).unwrap();
        assert_eq!(result.both_count, 1);
        assert_eq!(result.only_in_a_count, 0);
        assert_eq!(result.only_in_b_count, 0);
        let report = read_to_string(&output).unwrap();
        assert!(report.contains("\"path_b\":\"/ev/renamed.mp4\""));
        let _ = fs::remove_dir_all(&case_a);
        let _ = fs::remove_dir_all(&case_b);
    }

    #[test]
    fn unhashed_rows_pair_on_path_and_size() {
        let case_a = temp_case("ph-a", &row("vid_1", "/ev/a.mp4", 10, None));
        // Same path + size pairs; same path with different size does not.
        let case_b = temp_case(
            "ph-b",
            &format!(
                "{}{}",
                row("vid_5", "/ev/a.mp4", 10, None),
                row("vid_6", "/ev/a.mp4", 10, None)
            ),
        );
        let output = case_a.join("reports/case-compare.json");
        let result = compare_cases(&case_a, &case_b, &output).unwrap();
        assert_eq!(result.both_count, 1);
        assert_eq!(result.only_in_b_count, 1);
        let report = read_to_string(&output).unwrap();
        assert!(report.contains("\"matched_by\":\"path+size\""));
        let _ = fs::remove_dir_all(&case_a);
        let _ = fs::remove_dir_all(&case_b);
    }

    /// E8: a torn/garbage index line used to vanish from the comparison
    /// without a trace, and stale rows (file absent at the last rescan)
    /// were indistinguishable from present files.
    #[test]
    fn broken_lines_are_counted_and_stale_rows_flagged() {
        let case_a = temp_case(
            "e8-a",
            &format!(
                "{}{}not json\n",
                row("vid_1", "/ev/a.mp4", 10, Some("aa")),
                "{\"id\":\"vid_2\",\"source_path\":\"/ev/gone.mp4\",\"size_bytes\":5,\"sha256\":\"dd\",\"index_status\":\"stale\",\"stale_since_unix\":7}\n",
            ),
        );
        let case_b = temp_case("e8-b", &row("vid_1", "/ev/a.mp4", 10, Some("aa")));
        let output = case_a.join("reports/case-compare.json");
        let result = compare_cases(&case_a, &case_b, &output).unwrap();
        assert_eq!(result.skipped_index_lines_a, 1);
        assert_eq!(result.skipped_index_lines_b, 0);
        assert_eq!(result.stale_rows_a, 1);
        assert_eq!(result.only_in_a_count, 1);
        let report: serde_json::Value =
            serde_json::from_str(&read_to_string(&output).unwrap()).unwrap();
        assert_eq!(report["index_coverage"]["a"]["skipped_index_lines"], 1);
        assert_eq!(report["only_in_a"][0]["stale"], true);
        assert_eq!(report["both"][0]["stale_a"], false);
        assert!(
            report["warnings"][0]
                .as_str()
                .unwrap()
                .starts_with("case A:")
        );
        let _ = fs::remove_dir_all(&case_a);
        let _ = fs::remove_dir_all(&case_b);
    }

    #[test]
    fn report_must_stay_inside_the_invoking_case() {
        let case_a = temp_case("conf-a", &row("vid_1", "/ev/a.mp4", 10, Some("aa")));
        let case_b = temp_case("conf-b", &row("vid_1", "/ev/a.mp4", 10, Some("aa")));
        let outside = case_a.parent().unwrap().join("compare-outside.json");
        let err = compare_cases(&case_a, &case_b, &outside).unwrap_err();
        assert!(err.contains("inside the case directory"), "{err}");
        assert!(!outside.exists());
        let _ = fs::remove_dir_all(&case_a);
        let _ = fs::remove_dir_all(&case_b);
    }
}
