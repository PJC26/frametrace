//! Candidate anomaly findings for examiner review (ROADMAP M4).
//!
//! Labels are `candidate-finding` only — never claim tampering, authenticity
//! failure, or legal proof. Findings are written to
//! `evidence/logs/anomaly-log.jsonl` and surfaced in the case report.

use crate::audit;
use crate::util::{json_escape, now_unix, read_to_string};
use std::path::{Path, PathBuf};

pub const LABEL: &str = "candidate-finding";
const GAP_SECS: u64 = 7 * 24 * 60 * 60;
const DAV_FRAME_GAP_SECS: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: &'static str,
    pub selector: String,
    pub source_path: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct IndexedRow {
    pub id: String,
    pub source_path: String,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub modified_unix: Option<u64>,
    pub duration_seconds: Option<f64>,
    pub format_name: Option<String>,
    pub video_codec: Option<String>,
    pub ffprobe_ok: Option<bool>,
    /// R5-P3: the lossless OS path when the record carries
    /// `source_path_raw_hex` (a non-Unicode name); `None` = `source_path`
    /// names the file exactly.
    pub raw_path: Option<std::path::PathBuf>,
}

impl IndexedRow {
    /// The file this record names, losslessly (`model::record_os_path`
    /// rule): every open, stat and hash goes through this, never through
    /// the lossy display string.
    pub fn os_path(&self) -> std::path::PathBuf {
        self.raw_path
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from(&self.source_path))
    }
}

#[derive(Debug, Clone, Default)]
pub struct RehashCoverage {
    pub skipped_no_hash: usize,
    pub skipped_missing: usize,
    pub skipped_error: usize,
}

impl RehashCoverage {
    pub fn limited(&self) -> bool {
        self.skipped_no_hash != 0 || self.skipped_missing != 0 || self.skipped_error != 0
    }
}

#[derive(Debug, Clone)]
pub struct AnomalyScanResult {
    pub findings: Vec<Finding>,
    pub log_path: PathBuf,
    pub scanned_unix: u64,
    pub coverage: RehashCoverage,
    /// What of the index the scan did not cover (E8 counted reader).
    pub index: IndexCoverage,
    /// R5-P2: the per-record re-hash results of a `--rehash` scan (empty
    /// otherwise), so finalize's sign-off check reuses them instead of
    /// reading every original twice.
    pub checks: Vec<HashCheck>,
    /// R12-I4: the run id this `--rehash` scan wrote to `hash-verify.jsonl`
    /// (`None` without `--rehash`). `qa anomalies` prints it as
    /// [`HASH_VERIFY_RUN_ID_PREFIX`] so the workstation can check that the
    /// result it reports IS this run.
    pub hash_verify_run_id: Option<String>,
}

/// R12-I4: the stdout line prefix `qa anomalies` prints the run id with
/// (printed first, before any finding text).
pub const HASH_VERIFY_RUN_ID_PREFIX: &str = "hash-verify-run-id: ";

/// R12-I4: the run id from `qa anomalies` output — the FIRST line starting
/// with [`HASH_VERIFY_RUN_ID_PREFIX`] (printed before any finding text, so
/// a finding detail cannot forge it).
pub fn printed_hash_verify_run_id(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.trim_end()
            .strip_prefix(HASH_VERIFY_RUN_ID_PREFIX)
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
    })
}

/// Index lines and records an anomaly scan did not examine (E8): lines
/// that are not JSON objects, records without an id, and records a rescan
/// marked `stale` (the file was absent then — their facts are not current
/// and must not yield current findings). Disclosed in the run record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexCoverage {
    pub unparseable_lines: usize,
    pub rows_without_id: usize,
    pub stale_rows: usize,
}

impl IndexCoverage {
    pub fn limited(&self) -> bool {
        self.unparseable_lines != 0 || self.rows_without_id != 0 || self.stale_rows != 0
    }

    /// One disclosure sentence, empty when everything was covered.
    pub fn note(&self) -> String {
        if !self.limited() {
            return String::new();
        }
        format!(
            "; index coverage: {} unparseable line(s) and {} record(s) without id were not examined, {} stale record(s) (file absent at the last scan) were excluded",
            self.unparseable_lines, self.rows_without_id, self.stale_rows
        )
    }
}

/// [`read_indexed_rows`] through the counted index reader
/// (`scan::read_index_rows`): stale records are left out and everything
/// not examined is counted.
pub fn read_indexed_rows_checked(
    case_dir: &Path,
) -> Result<(Vec<IndexedRow>, IndexCoverage), String> {
    let index = crate::scan::read_index_rows(case_dir)?;
    let stale: std::collections::HashSet<&str> = index
        .rows
        .iter()
        .filter(|row| row.stale)
        .map(|row| row.id.as_str())
        .collect();
    let rows = read_indexed_rows(case_dir)?
        .into_iter()
        .filter(|row| !stale.contains(row.id.as_str()))
        .collect();
    Ok((
        rows,
        IndexCoverage {
            unparseable_lines: index.unparseable_lines,
            rows_without_id: index.rows_without_id,
            stale_rows: stale.len(),
        },
    ))
}

/// Scan the case index for candidate anomalies and append a chained log.
pub fn scan_case(case_dir: &Path, rehash: bool) -> Result<AnomalyScanResult, String> {
    // R14 P20: one scan at a time per case — two writers interleaving
    // their runs is what made position-based run judgement unsafe.
    let _lock = ScanLock::acquire(case_dir)?;
    let (rows, index_coverage) = read_indexed_rows_checked(case_dir)?;
    let mut findings = Vec::new();
    let mut coverage = RehashCoverage::default();
    let mut checks: Vec<HashCheck> = Vec::new();
    if rehash {
        findings.extend(hash_revalidation_findings(
            &rows,
            &mut coverage,
            &mut checks,
        ));
    } else {
        findings.extend(index_staleness_findings(&rows));
    }
    findings.extend(timestamp_findings(&rows));
    findings.extend(container_stream_findings(&rows));
    findings.extend(dav_frame_gap_findings(case_dir, &rows));

    let scanned_unix = now_unix()?;
    // D4 / R15 P22: one id per scan — the re-verification run id when it
    // re-hashes, and the anomaly marker's `scan_id` — named by the head
    // the scan writes LAST.
    let mut nonce = [0u8; 2];
    getrandom::fill(&mut nonce).map_err(|err| format!("failed to draw a run id: {err}"))?;
    let scan_id = format!("{scanned_unix}-{:02x}{:02x}", nonce[0], nonce[1]);
    let hash_verify_run_id = if rehash {
        write_hash_verify_log(case_dir, scanned_unix, &scan_id, &checks)?;
        Some(scan_id.clone())
    } else {
        None
    };
    let log_path = case_dir.join(crate::report::ANOMALY_LOG_REL_PATH);
    for finding in &findings {
        let line = format!(
            "{{\"schema_version\":1,\"event\":\"anomaly-scan\",\"scanned_unix\":{},\"label\":\"{}\",\"kind\":\"{}\",\"selector\":\"{}\",\"source_path\":\"{}\",\"detail\":\"{}\"}}",
            scanned_unix,
            LABEL,
            json_escape(finding.kind),
            json_escape(&finding.selector),
            json_escape(&finding.source_path),
            json_escape(&finding.detail),
        );
        audit::append_chained_jsonl(&log_path, &line)?;
    }
    if findings.is_empty() {
        let line = format!(
            "{{\"schema_version\":1,\"event\":\"anomaly-scan\",\"scanned_unix\":{},\"label\":\"none\",\"kind\":\"none\",\"selector\":\"\",\"source_path\":\"\",\"detail\":\"no candidate findings on indexed videos\"}}",
            scanned_unix
        );
        audit::append_chained_jsonl(&log_path, &line)?;
    }
    // Every scan records which integrity lane ran so a report generated from
    // stored hashes is never mistaken for a full revalidation pass.
    let mtime_unavailable = mtime_unavailable_count(&rows);
    let mtime_note = if mtime_unavailable > 0 {
        format!(
            "; mtime-unavailable: {mtime_unavailable} record(s) excluded from timestamp-gap/regression checks"
        )
    } else {
        String::new()
    } + &index_coverage.note();
    let detail = if !rehash {
        format!("stored index hashes only; pass --rehash for full revalidation{mtime_note}")
    } else {
        format!(
            "live re-hashing; coverage: {} of {} records rehashed, {} no stored hash, {} missing on disk, {} unreadable or unhashable{}",
            rows.len()
                - coverage.skipped_no_hash
                - coverage.skipped_missing
                - coverage.skipped_error,
            rows.len(),
            coverage.skipped_no_hash,
            coverage.skipped_missing,
            coverage.skipped_error,
            if coverage.limited() {
                "; coverage limitation: not every indexed file was revalidated"
            } else {
                ""
            },
        ) + &mtime_note
    };
    let line = format!(
        "{{\"schema_version\":1,\"event\":\"anomaly-scan-run\",\"scanned_unix\":{},\"scan_id\":\"{}\",\"{}\":\"{}\",\"hash_mode\":\"{}\",\"finding_count\":{},\"mtime_unavailable\":{},\"skipped_no_hash\":{},\"skipped_missing\":{},\"skipped_error\":{},\"index_unparseable_lines\":{},\"index_rows_without_id\":{},\"index_stale_rows\":{},\"coverage_limited\":{},\"detail\":\"{}\"}}",
        scanned_unix,
        json_escape(&scan_id),
        crate::report::HASH_VERIFY_MARKER_HEAD_FIELD,
        crate::report::SIGNOFF_LOG_REL_PATH,
        if rehash { "rehash" } else { "stored" },
        findings.len(),
        mtime_unavailable,
        coverage.skipped_no_hash,
        coverage.skipped_missing,
        coverage.skipped_error,
        index_coverage.unparseable_lines,
        index_coverage.rows_without_id,
        index_coverage.stale_rows,
        (coverage.limited() && rehash) || index_coverage.limited(),
        json_escape(&detail),
    );
    audit::append_chained_jsonl(&log_path, &line)?;
    // R15 P22: the head LAST — after both result lanes are complete — so
    // it anchors the newest run of each (R14 wrote it before the anomaly
    // rows and anchored the re-verification lane only).
    crate::report::record_scan_head(
        case_dir,
        &scan_id,
        hash_verify_run_id.as_deref(),
        scanned_unix,
    )?;
    Ok(AnomalyScanResult {
        findings,
        log_path,
        scanned_unix,
        coverage,
        index: index_coverage,
        checks,
        hash_verify_run_id,
    })
}

/// R14 P20: the case-level exclusive lock of an anomaly / re-hash scan.
pub const SCAN_LOCK_REL_PATH: &str = "db/anomaly-scan.lock";

/// R14 P20: held (fs2 exclusive lock on [`SCAN_LOCK_REL_PATH`]) for the
/// whole of [`scan_case`]; unlocks on drop. The sidecar file is never
/// deleted (see `checkpoint::RunLock` for why), and `db/` top-level files
/// enter a package only by explicit list.
struct ScanLock {
    file: std::fs::File,
}

impl ScanLock {
    fn acquire(case_dir: &Path) -> Result<Self, String> {
        use fs2::FileExt;
        let path = case_dir.join(SCAN_LOCK_REL_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| format!("failed to open scan lock {}: {err}", path.display()))?;
        file.try_lock_exclusive()
            .map_err(|err| scan_lock_error(&path, &err))?;
        Ok(Self { file })
    }
}

/// R15 I6: only lock CONTENTION (`WouldBlock`, or the platform's
/// contended-lock error fs2 reports — Windows `ERROR_LOCK_VIOLATION`) is
/// "another scan is running"; any other failure is reported as itself.
pub(crate) fn scan_lock_error(path: &Path, err: &std::io::Error) -> String {
    let contended = err.kind() == std::io::ErrorKind::WouldBlock
        || (err.raw_os_error().is_some()
            && err.raw_os_error() == fs2::lock_contended_error().raw_os_error());
    if contended {
        format!(
            "이 케이스에서 다른 이상 징후 검사·원본 재검증이 실행 중입니다 — 끝난 뒤 다시 실행하십시오 ({}: {err})",
            path.display()
        )
    } else {
        format!(
            "이상 징후 검사 잠금을 걸지 못했습니다 {}: {err}",
            path.display()
        )
    }
}

impl Drop for ScanLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// Load the video index into an id-keyed map so batch paths pay one read of
/// `db/videos.jsonl` instead of one per item.
pub fn index_by_id(
    case_dir: &Path,
) -> Result<std::collections::HashMap<String, IndexedRow>, String> {
    Ok(read_indexed_rows(case_dir)?
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect())
}

/// Compare a live digest against the indexed hash for one selector, if known.
/// The index is preloaded by the caller and shared across lookups.
pub fn hash_mismatch_finding(
    index: &std::collections::HashMap<String, IndexedRow>,
    selector: &str,
    live_sha256: &str,
) -> Option<Finding> {
    let row = index.get(selector)?;
    let indexed = row.sha256.as_deref()?;
    if indexed.eq_ignore_ascii_case(live_sha256) {
        return None;
    }
    Some(Finding {
        kind: "hash-revalidation-mismatch",
        selector: row.id.clone(),
        source_path: row.source_path.clone(),
        detail: format!("indexed sha256 {indexed} != live sha256 {live_sha256}"),
    })
}

/// One recorded-vs-observed comparison from a live re-hash pass (FOR-03).
/// `status` is one of `match`, `mismatch`, `missing`, `unreadable`,
/// `no-recorded-hash` — only `match`/`mismatch` carry an observed digest.
#[derive(Debug, Clone)]
pub struct HashCheck {
    pub id: String,
    pub source_path: String,
    pub recorded: Option<String>,
    pub observed: Option<String>,
    pub status: &'static str,
}

/// Relative path of the per-record hash re-verification log.
pub const HASH_VERIFY_REL_PATH: &str = "evidence/logs/hash-verify.jsonl";

/// Counts from the newest run recorded in `hash-verify.jsonl`.
///
/// R11 P9: the counts are only the run's USABLE records — a record that
/// cannot be authenticated here is missing from them, and a missing row is
/// not good news. "All match" may be claimed only when `verified` is true
/// AND `mismatched == 0 && not_comparable == 0 && matched > 0`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HashVerifySummary {
    pub run_unix: i64,
    /// The result run's id (`None` when `is_result` is false).
    pub run_id: Option<String>,
    pub matched: usize,
    pub mismatched: usize,
    pub not_comparable: usize,
    /// R11 P9: `hash-verify` / `hash-verify-run` entries that may not feed
    /// a result here (not authenticated where keys are held, another PC's,
    /// superseded under P7, in a lane whose chain is broken) in the result
    /// run and every newer run — or in every run when there is no result.
    pub unauthenticated: usize,
    /// R11 P9: the result run may be relied on as a whole — the lane's
    /// chain verifies and the lane is not NOT VERIFIED here, no entry of
    /// this run or a newer one is unusable, and the completion marker's
    /// `records` count equals the usable records.
    pub verified: bool,
    /// R11 I2: false = there is NO current result (a newer run holds an
    /// entry this PC cannot use, e.g. its completion marker's HMAC was
    /// stripped, or the chain is broken): the counts are zero and no older
    /// run stands in for it.
    pub is_result: bool,
    /// Why `verified` is false (Korean, `; `-joined), `None` when verified.
    pub not_verified_reason: Option<String>,
    /// The lane's status word ([`audit::LaneRows::status`]): `keyed`,
    /// `structural` (self-consistency only), `not-verified`, `failed`,
    /// `absent`.
    pub lane_status: &'static str,
    /// R13: why there is no current result (`is_result` false) — the
    /// assessment's `withheld` (or the workstation's run-id check);
    /// `None` when there is a result. Also part of `not_verified_reason`.
    pub withheld_reason: Option<String>,
    /// R13 P16: runs without a completion marker positioned after the
    /// last completed run (every such run when none completed) — each one
    /// withholds the result ([`NEWER_UNMARKED_RUN_REASON`]).
    pub newer_runs: usize,
    /// R13 P16: the usable per-record entries of those runs.
    pub newer_records: usize,
    /// R13 P16: how many of them are `mismatch` — shown, never hidden.
    pub newer_mismatched: usize,
    /// R13 P16: those mismatch rows.
    pub newer_mismatches: Vec<NewerMismatch>,
    /// R14 I3: [`crate::report::HashVerifyHeadCheck::status`] — `absent`,
    /// `intact`, `truncated`, `rewritten`, `unknown`, `not-authenticated`
    /// (`""` only in a hand-built summary).
    pub head_status: &'static str,
    /// R15 I5: [`crate::report::HashVerifyHeadCheck::reason_code`] when
    /// `head_status` is `unknown` (`other-pc` = "다른 PC 머리일 수 있음").
    pub head_reason: Option<&'static str>,
    /// R15 I4: runs whose `run_unix` is more than
    /// [`CLOCK_SKEW_MARGIN_SECS`] after this PC's clock now.
    pub clock_anomaly_runs: usize,
    /// R15 I4: the "시계 이상" sentence ([`CLOCK_ANOMALY_NOTE`] first),
    /// `None` without such a run. Not a withholding reason by itself.
    pub clock_note: Option<String>,
}

/// R15 I4: how far in this PC's future a run's `run_unix` may lie before
/// it is shown as a clock anomaly.
pub const CLOCK_SKEW_MARGIN_SECS: i64 = 600;

/// R15 I4: the start of [`HashVerifySummary::clock_note`].
pub const CLOCK_ANOMALY_NOTE: &str = "시계 이상";

/// R15 I3: the reason when the result run's span overlaps another
/// completed run's (two runs written into each other).
pub const OVERLAPPING_RUNS_REASON: &str = "완료 실행 구간 겹침";

/// R13 P16: one usable `mismatch` row of a run without a completion
/// marker that withholds the result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewerMismatch {
    pub run_id: String,
    pub run_unix: i64,
    pub id: String,
    pub source_path: String,
    pub recorded_sha256: Option<String>,
    pub observed_sha256: Option<String>,
}

impl NewerMismatch {
    fn from_entry(run: &HashVerifyRun, entry: &serde_json::Value) -> Self {
        let text = |key: &str| entry.get(key).and_then(|v| v.as_str()).map(str::to_string);
        Self {
            run_id: run.run_id.clone(),
            run_unix: run.run_unix,
            id: text("id").unwrap_or_default(),
            source_path: text("source_path").unwrap_or_default(),
            recorded_sha256: text("recorded_sha256"),
            observed_sha256: text("observed_sha256"),
        }
    }

    /// The `/api/verify-sources` `newer_mismatches[]` object.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "run_id": self.run_id,
            "run_unix": self.run_unix,
            "id": self.id,
            "source_path": self.source_path,
            "recorded_sha256": self.recorded_sha256,
            "observed_sha256": self.observed_sha256,
        })
    }
}

/// R13 P16: the reason prefix when a run without a completion marker sits
/// after the last completed run (its marker may have been deleted — no
/// key is needed for that — or the run was cut short).
pub const NEWER_UNMARKED_RUN_REASON: &str = "표식 없는 더 새 실행 있음";

/// One run in `hash-verify.jsonl`, grouped by its run id (D4).
#[derive(Debug, Clone, PartialEq)]
pub struct HashVerifyRun {
    /// `run_id` (`<run_unix>-<random>`), or the `run_unix` of a run logged
    /// before run ids existed.
    pub run_id: String,
    pub run_unix: i64,
    /// Per-record `hash-verify` entries of this run only.
    pub entries: Vec<serde_json::Value>,
    /// True when the run's `hash-verify-run` completion marker is present.
    pub completed: bool,
    /// R9 P3 / R12 P12: unusable lines positioned inside this run's span
    /// (its first usable entry through its completion marker, or the end
    /// of the lane) — counted by POSITION, never by what they claim
    /// (`event`, `run_id`); never part of `entries` / `completed`.
    pub unauthenticated: usize,
    /// R11 I1: the `records` count the (usable) completion marker states —
    /// `None` without a marker, or for a marker written before the field.
    pub marker_records: Option<usize>,
    /// R12 P12: lane index of the run's first usable entry.
    pub first_index: usize,
    /// R12 P12: lane index of the run's usable completion marker.
    pub marker_index: Option<usize>,
    /// R13 P16: lane index of the run's last usable entry (its marker
    /// when completed).
    pub last_index: usize,
    /// R14 I3: the usable marker says the run anchored the lane in the
    /// sign-off lane ([`crate::report::HASH_VERIFY_MARKER_HEAD_FIELD`]).
    pub marker_wants_head: bool,
}

impl HashVerifyRun {
    /// R13 P16: the run's usable `mismatch` entries.
    pub fn mismatches(&self) -> impl Iterator<Item = &serde_json::Value> {
        self.entries
            .iter()
            .filter(|entry| entry.get("status").and_then(|v| v.as_str()) == Some("mismatch"))
    }
}

/// Every run in `hash-verify.jsonl`, in first-appearance order. Entries are
/// grouped by their run id — never by position — so the records of a run
/// cut short by a crash or cancel cannot be absorbed into the next run's
/// result (D4). `Ok(empty)` when there is no log; an unreadable log is an
/// error, never "no run" (D11).
pub fn hash_verify_runs(case_dir: &Path) -> Result<Vec<HashVerifyRun>, String> {
    Ok(hash_verify_assessment(case_dir)?.runs)
}

/// R12 P12: groups the USABLE `hash-verify` / `hash-verify-run` entries by
/// run id. The content of an unusable line (its `event`, `run_id`, whether
/// it even parses) is never read here — R11 read it, so an unsigned tail
/// line claiming another event ("x-note") or an older run's id was not
/// counted against the newer run and the older all-match run stood as the
/// result. Unusable lines are judged by POSITION only ([`assess_runs`]).
fn runs_from_lane(lane: &audit::LaneRows) -> Vec<HashVerifyRun> {
    let mut runs: Vec<HashVerifyRun> = Vec::new();
    for row in lane.usable() {
        let value = &row.value;
        let event = value.get("event").and_then(|e| e.as_str());
        if !matches!(event, Some("hash-verify" | "hash-verify-run")) {
            continue;
        }
        let run_unix = value.get("run_unix").and_then(|v| v.as_i64()).unwrap_or(0);
        let run_id = value
            .get("run_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| run_unix.to_string());
        let index = match runs.iter().position(|run| run.run_id == run_id) {
            Some(index) => index,
            None => {
                runs.push(HashVerifyRun {
                    run_id,
                    run_unix,
                    entries: Vec::new(),
                    completed: false,
                    unauthenticated: 0,
                    marker_records: None,
                    first_index: row.index,
                    marker_index: None,
                    last_index: row.index,
                    marker_wants_head: false,
                });
                runs.len() - 1
            }
        };
        runs[index].last_index = row.index;
        if event == Some("hash-verify-run") {
            runs[index].completed = true;
            runs[index].marker_index = Some(row.index);
            runs[index].marker_records = value
                .get("records")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            runs[index].marker_wants_head = value
                .get(crate::report::HASH_VERIFY_MARKER_HEAD_FIELD)
                .is_some_and(|v| !v.is_null());
        } else {
            runs[index].entries.push(row.value_with_trust());
        }
    }
    // P12: a run's unusable count is the unusable lines positioned inside
    // its span (first usable entry .. its marker, or the end of the lane).
    let unusable: Vec<usize> = lane.unusable().map(|row| row.index).collect();
    for run in &mut runs {
        run.unauthenticated = unusable
            .iter()
            .filter(|at| **at >= run.first_index && run.marker_index.is_none_or(|m| **at <= m))
            .count();
    }
    runs
}

/// R11 P9: what `hash-verify.jsonl` may be relied on for — THE decision
/// every consumer (custody statement section 5, `/api/verify-sources`)
/// uses.
#[derive(Debug, Clone)]
pub struct HashVerifyAssessment {
    /// Every run, in first-appearance order ([`hash_verify_runs`]).
    pub runs: Vec<HashVerifyRun>,
    /// Index into `runs` of the current result: the newest completed run,
    /// unless `withheld`.
    pub result: Option<usize>,
    /// Runs that never completed (no usable completion marker) with a
    /// usable entry positioned after the last usable completion marker —
    /// R14 P20: or whose first usable entry is after the result run's
    /// first entry, or whose `run_unix` is greater (interleaved runs) —
    /// (every such run when no run completed) — cut short by a crash or
    /// cancel, OR their marker was deleted (no key is needed for that).
    /// R13 P16: each one WITHHOLDS the result (an older completed run is
    /// not the current result) and its usable mismatch rows are shown.
    pub interrupted: Vec<usize>,
    /// R11 I2: why there is no current result although runs exist — a run
    /// newer than every completed one holds an entry this PC cannot use
    /// (a stripped completion marker), or the chain is broken. An older
    /// run is then NOT presented as the latest result.
    pub withheld: Option<String>,
    /// R11 P9: why the result run may not be relied on as a whole (empty =
    /// it may).
    pub not_verified: Vec<String>,
    /// See [`HashVerifySummary::unauthenticated`].
    pub unauthenticated: usize,
    /// The lane as read ([`audit::read_lane_rows`]).
    pub lane: audit::LaneRows,
    /// R14 I3: the sign-off lane's `hash-verify-head` against this lane.
    pub head: crate::report::HashVerifyHeadCheck,
    /// R15 I4: indices into `runs` stamped more than
    /// [`CLOCK_SKEW_MARGIN_SECS`] in this PC's future.
    pub clock_anomalies: Vec<usize>,
}

impl HashVerifyAssessment {
    pub fn result_run(&self) -> Option<&HashVerifyRun> {
        self.result.map(|index| &self.runs[index])
    }

    /// R13 P16: the runs without a completion marker that withhold the
    /// result ([`Self::interrupted`]).
    pub fn newer_runs(&self) -> impl Iterator<Item = &HashVerifyRun> {
        self.interrupted.iter().map(|index| &self.runs[*index])
    }

    /// R13 P16: their usable per-record entries.
    pub fn newer_records(&self) -> usize {
        self.newer_runs().map(|run| run.entries.len()).sum()
    }

    /// R13 P16: their usable `mismatch` rows — shown, never hidden.
    pub fn newer_mismatches(&self) -> Vec<NewerMismatch> {
        self.newer_runs()
            .flat_map(|run| {
                run.mismatches()
                    .map(move |entry| NewerMismatch::from_entry(run, entry))
            })
            .collect()
    }

    /// R15 I4: the "시계 이상" sentence, `None` without such a run.
    pub fn clock_note(&self) -> Option<String> {
        if self.clock_anomalies.is_empty() {
            return None;
        }
        let ids: Vec<&str> = self
            .clock_anomalies
            .iter()
            .map(|index| self.runs[*index].run_id.as_str())
            .collect();
        Some(format!(
            "{CLOCK_ANOMALY_NOTE}: 실행 시각이 이 PC의 현재 시각보다 {}초 넘게 뒤인 실행 {}개({}) — 그 실행을 기록한 PC의 시계가 틀렸을 수 있음 (실행 시각은 순서의 근거가 아님)",
            CLOCK_SKEW_MARGIN_SECS,
            ids.len(),
            ids.join(", ")
        ))
    }

    /// The API summary; `None` when nothing usable was ever recorded and
    /// nothing is withheld.
    pub fn summary(&self) -> Option<HashVerifySummary> {
        let lane_status = self.lane.status();
        let Some(run) = self.result_run() else {
            return self.withheld.as_ref().map(|reason| {
                let newer_mismatches = self.newer_mismatches();
                HashVerifySummary {
                    unauthenticated: self.unauthenticated,
                    not_verified_reason: Some(reason.clone()),
                    withheld_reason: Some(reason.clone()),
                    lane_status,
                    newer_runs: self.interrupted.len(),
                    newer_records: self.newer_records(),
                    newer_mismatched: newer_mismatches.len(),
                    newer_mismatches,
                    head_status: self.head.status(),
                    head_reason: self.head.reason_code(),
                    clock_anomaly_runs: self.clock_anomalies.len(),
                    clock_note: self.clock_note(),
                    ..Default::default()
                }
            });
        };
        let mut summary = HashVerifySummary {
            run_unix: run.run_unix,
            run_id: Some(run.run_id.clone()),
            unauthenticated: self.unauthenticated,
            verified: self.not_verified.is_empty(),
            is_result: true,
            not_verified_reason: (!self.not_verified.is_empty())
                .then(|| self.not_verified.join("; ")),
            lane_status,
            head_status: self.head.status(),
            head_reason: self.head.reason_code(),
            clock_anomaly_runs: self.clock_anomalies.len(),
            clock_note: self.clock_note(),
            ..Default::default()
        };
        for entry in &run.entries {
            match entry.get("status").and_then(|v| v.as_str()) {
                Some("match") => summary.matched += 1,
                Some("mismatch") => summary.mismatched += 1,
                _ => summary.not_comparable += 1,
            }
        }
        Some(summary)
    }
}

/// R11 P9: reads `hash-verify.jsonl` through the lane reader and decides
/// the current result: the newest completed run — but no result at all
/// when a NEWER run holds an unusable entry (I2: stripping the newest
/// run's completion-marker HMAC must not roll the result back to the
/// previous run) or the chain is broken (I4); and the result is not
/// `verified` when it or a newer run holds an unusable entry, its marker's
/// `records` count disagrees with its usable records (I1), or the lane is
/// NOT VERIFIED here.
pub fn hash_verify_assessment(case_dir: &Path) -> Result<HashVerifyAssessment, String> {
    let path = case_dir.join(HASH_VERIFY_REL_PATH);
    // R9 P3: rows come through the one lane reader, each with its trust.
    let lane = audit::read_lane_rows(&path).map_err(|err| {
        format!(
            "원본 재검증 기록을 읽지 못했습니다 {}: {err}",
            path.display()
        )
    })?;
    let now = now_unix().ok().map(|now| now as i64);
    Ok(assess_runs(runs_from_lane(&lane), lane, now, |result| {
        crate::report::hash_verify_head_check(
            case_dir,
            result.map(|run| run.run_id.as_str()),
            result.is_some_and(|run| run.marker_wants_head),
        )
    }))
}

fn assess_runs(
    runs: Vec<HashVerifyRun>,
    lane: audit::LaneRows,
    now: Option<i64>,
    head_check: impl FnOnce(Option<&HashVerifyRun>) -> crate::report::HashVerifyHeadCheck,
) -> HashVerifyAssessment {
    // R12 P12: the candidate is the run whose usable completion marker sits
    // LAST in the lane; every decision about unusable lines is by position
    // (R11 compared run indices derived from an unusable line's `run_id`).
    let latest = runs
        .iter()
        .enumerate()
        .filter_map(|(index, run)| run.marker_index.map(|at| (at, index)))
        .max()
        .map(|(_, index)| index);
    let marker_at = latest.and_then(|index| runs[index].marker_index);
    let after_marker = |at: usize| marker_at.is_none_or(|m| at > m);
    let unusable_after = lane
        .unusable()
        .filter(|row| after_marker(row.index))
        .count();
    // R12 I3 / R13 I2: a `lane-repair` marker after the result run's
    // marker — the repair cut a tail that may have held a newer run.
    let repaired_after = match marker_at {
        Some(m) => lane.repaired_after(m),
        None => !lane.repairs.is_empty(),
    };
    // R13 P16: runs that never completed with a usable entry after the
    // last completed run's marker (every such run when none completed).
    // Deleting the newest run's marker needs no key — the newer run then
    // looks "interrupted"; R12 let the older run stand as the result.
    // R14 P20: by SPAN, not position — runs interleave (two writers, or
    // a crash mid-run before the lock existed), so B's last usable row can
    // sit before A's marker once B's own marker is deleted. An unfinished
    // run is also newer when its first row is after the result run's
    // first row, or its `run_unix` is greater.
    // R15 I4: (c) `run_unix` greater — only for a run not wholly BEFORE
    // the result run's first row: a crashed run stamped by a clock set in
    // the future, positioned entirely before the result, withheld every
    // later result forever (its stamp stayed "newer").
    let result_first = latest.map(|index| runs[index].first_index);
    let result_unix = latest.map(|index| runs[index].run_unix);
    let interrupted: Vec<usize> = (0..runs.len())
        .filter(|index| {
            let run = &runs[*index];
            !run.completed
                && (after_marker(run.last_index)
                    || result_first.is_some_and(|first| run.first_index > first)
                    || (result_unix.is_some_and(|unix| run.run_unix > unix)
                        && result_first.is_some_and(|first| run.last_index > first)))
        })
        .collect();
    // R15 I3: two COMPLETED runs written into each other (a lane from
    // before the scan lock, or a crash-resumed writer): which one is newer
    // cannot be told from position — the result run's span (first row ..
    // marker) overlapping another completed run's span withholds it.
    let overlapping: Vec<usize> = match latest {
        Some(result) => {
            let (first, last) = (runs[result].first_index, runs[result].last_index);
            (0..runs.len())
                .filter(|index| {
                    let run = &runs[*index];
                    *index != result
                        && run.completed
                        && run.first_index <= last
                        && run.last_index >= first
                })
                .collect()
        }
        None => Vec::new(),
    };
    // R15 I4: runs stamped in this PC's future.
    let clock_anomalies: Vec<usize> = match now {
        Some(now) => (0..runs.len())
            .filter(|index| runs[*index].run_unix > now.saturating_add(CLOCK_SKEW_MARGIN_SECS))
            .collect(),
        None => Vec::new(),
    };
    // R14 I3: the newest `hash-verify-head` in the sign-off lane — a lane
    // shorter than it lost its tail (two deleted lines need no key).
    let head = head_check(latest.map(|index| &runs[index]));
    let newer_records: usize = interrupted.iter().map(|i| runs[*i].entries.len()).sum();
    let newer_mismatched: usize = interrupted
        .iter()
        .map(|i| runs[*i].mismatches().count())
        .sum();
    let mut reasons: Vec<String> = Vec::new();
    if let Some(failure) = &lane.failure {
        reasons.push(format!(
            "재검증 기록의 감사 체인 검증 실패 — 어떤 실행도 현재 결과로 쓰지 않음: {failure}"
        ));
    }
    if lane.failure.is_none() && unusable_after > 0 {
        reasons.push(match latest {
            Some(index) => format!(
                "마지막 완료 실행({})의 완료 표식 뒤에 이 PC에서 쓸 수 없는 줄 {unusable_after}개 — 그 줄의 내용(이벤트·실행 id)은 믿을 수 없고 더 새 실행의 결과일 수 있어, 이 실행을 현재 결과로 쓰지 않음",
                runs[index].run_id
            ),
            None => format!(
                "이 PC에서 쓸 수 있는 완료 실행이 없고 쓸 수 없는 줄 {unusable_after}개가 있음 — 그 줄의 내용은 믿을 수 없어 결과를 알 수 없음"
            ),
        });
    }
    if lane.failure.is_none() && repaired_after {
        reasons.push(
            "마지막 완료 실행 뒤에 레인 수리(lane-repair) 표식이 있음 — 수리로 잘린 꼬리에 더 새 실행이 있었을 수 있어 이 실행을 현재 결과로 쓰지 않음 (원본 재검증을 다시 실행하십시오)"
                .to_string(),
        );
    }
    if lane.failure.is_none() && !interrupted.is_empty() {
        let mismatch_note = if newer_mismatched > 0 {
            format!(", 그중 불일치(MISMATCH) {newer_mismatched}개")
        } else {
            String::new()
        };
        reasons.push(match latest {
            Some(index) => format!(
                "{NEWER_UNMARKED_RUN_REASON} — 마지막 완료 실행({}) 뒤에 있거나 그 뒤에 시작한 완료 표식 없는 실행 {}개(기록 {newer_records}개{mismatch_note}): 완료 표식이 지워졌거나 실행이 중단됐을 수 있어 이전 실행을 현재 결과로 쓰지 않음 (원본 재검증을 다시 실행하십시오)",
                runs[index].run_id,
                interrupted.len()
            ),
            None => format!(
                "{NEWER_UNMARKED_RUN_REASON} — 완료된 실행이 없고 완료 표식 없는 실행 {}개(기록 {newer_records}개{mismatch_note})만 있음: 결과 아님 (원본 재검증을 다시 실행하십시오)",
                interrupted.len()
            ),
        });
    }
    if lane.failure.is_none()
        && let Some(index) = latest
        && !overlapping.is_empty()
    {
        let others: Vec<&str> = overlapping
            .iter()
            .map(|i| runs[*i].run_id.as_str())
            .collect();
        reasons.push(format!(
            "{OVERLAPPING_RUNS_REASON} — 마지막 완료 실행({})의 구간(첫 항목~완료 표식)이 다른 완료 실행({})의 구간과 겹침: 두 실행이 섞여 기록돼 어느 쪽이 최신인지 위치로 알 수 없어 현재 결과로 쓰지 않음 (원본 재검증을 다시 실행하십시오)",
            runs[index].run_id,
            others.join(", ")
        ));
    }
    if lane.failure.is_none()
        && let Some(reason) = head.withhold_reason()
    {
        reasons.push(reason);
    }
    // R15 I4: a withholding run stamped in the future says so.
    if lane.failure.is_none()
        && interrupted
            .iter()
            .any(|index| clock_anomalies.contains(index))
    {
        reasons.push(format!(
            "{CLOCK_ANOMALY_NOTE}: 결과를 보류하는 완료 표식 없는 실행의 시각이 이 PC의 현재 시각보다 뒤임 — 실행 시각이 아니라 레인 위치로 판단함"
        ));
    }
    let withheld = (!reasons.is_empty()).then(|| reasons.join("; "));
    let result = if withheld.is_some() { None } else { latest };
    // Unusable lines from the candidate run's first entry on (every one
    // when there is no candidate), by position.
    let start = latest.map_or(0, |index| runs[index].first_index);
    let unauthenticated = lane.unusable().filter(|row| row.index >= start).count();
    let mut not_verified = Vec::new();
    match result {
        None => not_verified.extend(withheld.clone()),
        Some(index) => {
            let run = &runs[index];
            if unauthenticated > 0 {
                not_verified.push(format!(
                    "이 실행 또는 그 뒤 실행의 항목 {unauthenticated}개를 이 PC에서 인증할 수 없음 — 빠진 항목은 일치로 보지 않음"
                ));
            }
            if let Some(records) = run.marker_records
                && records != run.entries.len()
            {
                not_verified.push(format!(
                    "완료 표식의 기록 수({records})와 사용 가능한 기록 수({})가 다름",
                    run.entries.len()
                ));
            }
            if let audit::LaneTrust::NotVerified { reason, .. } = &lane.lane {
                not_verified.push(format!("재검증 기록 레인 NOT VERIFIED: {reason}"));
            }
            not_verified.extend(head.not_verified_reason());
        }
    }
    HashVerifyAssessment {
        runs,
        result,
        interrupted,
        withheld,
        not_verified,
        unauthenticated,
        lane,
        head,
        clock_anomalies,
    }
}

/// Newest *completed* run in `hash-verify.jsonl`: per-record entries are
/// followed by a `hash-verify-run` marker, so a run cut short by a crash or
/// cancel (records without a marker) is never presented as a result. Only
/// the completed run's own records are returned (grouped by run id).
/// R11 I2: `None` when the result is withheld ([`hash_verify_assessment`]).
pub fn latest_hash_verify_run(case_dir: &Path) -> Option<(i64, Vec<serde_json::Value>)> {
    let assessment = hash_verify_assessment(case_dir).ok()?;
    assessment
        .result_run()
        .map(|run| (run.run_unix, run.entries.clone()))
}

/// R11 P9: the API summary ([`HashVerifyAssessment::summary`]). `None`
/// when the log cannot be read or nothing was ever recorded; `Some` with
/// `is_result: false` when the result is withheld.
pub fn latest_hash_verify_summary(case_dir: &Path) -> Option<HashVerifySummary> {
    hash_verify_assessment(case_dir).ok()?.summary()
}

fn write_hash_verify_log(
    case_dir: &Path,
    run_unix: u64,
    run_id: &str,
    checks: &[HashCheck],
) -> Result<(), String> {
    let log_path = case_dir.join(HASH_VERIFY_REL_PATH);
    let opt = |value: &Option<String>| match value {
        Some(text) => format!("\"{}\"", json_escape(text)),
        None => "null".to_string(),
    };
    // D4: every entry names its run, so readers group by run, not position.
    let run_id = json_escape(run_id);
    for check in checks {
        let line = format!(
            "{{\"schema_version\":1,\"event\":\"hash-verify\",\"run_unix\":{run_unix},\"run_id\":\"{run_id}\",\"id\":\"{}\",\"source_path\":\"{}\",\"recorded_sha256\":{},\"observed_sha256\":{},\"status\":\"{}\"}}",
            json_escape(&check.id),
            json_escape(&check.source_path),
            opt(&check.recorded),
            opt(&check.observed),
            check.status,
        );
        audit::append_chained_jsonl(&log_path, &line)?;
    }
    // R14 I3: the marker names where this run anchors the lane; R15 P22:
    // the anchor itself (`hash-verify-head` in the sign-off lane) is
    // written by `scan_case` at the END of the scan.
    let summary = format!(
        "{{\"schema_version\":1,\"event\":\"hash-verify-run\",\"run_unix\":{run_unix},\"run_id\":\"{run_id}\",\"records\":{},\"{}\":\"{}\"}}",
        checks.len(),
        crate::report::HASH_VERIFY_MARKER_HEAD_FIELD,
        crate::report::SIGNOFF_LOG_REL_PATH,
    );
    audit::append_chained_jsonl(&log_path, &summary)
}

fn hash_revalidation_findings(
    rows: &[IndexedRow],
    coverage: &mut RehashCoverage,
    checks: &mut Vec<HashCheck>,
) -> Vec<Finding> {
    let mut out = Vec::new();
    for row in rows {
        let mut check = HashCheck {
            id: row.id.clone(),
            source_path: row.source_path.clone(),
            recorded: row.sha256.clone(),
            observed: None,
            status: "no-recorded-hash",
        };
        let Some(indexed) = row.sha256.as_deref() else {
            coverage.skipped_no_hash += 1;
            checks.push(check);
            continue;
        };
        let os_path = row.os_path();
        let path = os_path.as_path();
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                coverage.skipped_missing += 1;
                check.status = "missing";
                checks.push(check);
                continue;
            }
            _ => {
                coverage.skipped_error += 1;
                check.status = "unreadable";
                checks.push(check);
                continue;
            }
        }
        let Ok(live) = audit::digest_file(path) else {
            coverage.skipped_error += 1;
            check.status = "unreadable";
            checks.push(check);
            continue;
        };
        check.observed = Some(live.clone());
        check.status = if indexed.eq_ignore_ascii_case(&live) {
            "match"
        } else {
            "mismatch"
        };
        checks.push(check);
        if !indexed.eq_ignore_ascii_case(&live) {
            out.push(Finding {
                kind: "hash-revalidation-mismatch",
                selector: row.id.clone(),
                source_path: row.source_path.clone(),
                detail: format!("indexed sha256 {indexed} != live sha256 {live}"),
            });
        }
    }
    out
}

/// Cheap staleness lane used when `scan_case` runs without re-hashing. Only
/// `metadata()` calls (no file reads) compare each indexed row against the
/// live filesystem: a missing or drifted file means the stored sha256 may no
/// longer describe the bytes on disk — reported as candidate staleness, never
/// as a verified mismatch.
fn index_staleness_findings(rows: &[IndexedRow]) -> Vec<Finding> {
    let mut out = Vec::new();
    for row in rows {
        let Some(indexed) = row.sha256.as_deref() else {
            continue; // no stored hash: nothing to be stale about
        };
        let os_path = row.os_path();
        let path = os_path.as_path();
        let Ok(metadata) = std::fs::metadata(path) else {
            out.push(Finding {
                kind: "index-record-stale",
                selector: row.id.clone(),
                source_path: row.source_path.clone(),
                detail: format!(
                    "indexed sha256 {indexed} on record but the source file is no longer readable; stored hash is the last verified state (rerun with --rehash for full revalidation)"
                ),
            });
            continue;
        };
        let mut drift = Vec::new();
        if let Some(indexed_size) = row.size_bytes
            && metadata.len() != indexed_size
        {
            drift.push(format!(
                "size {} != indexed {}",
                metadata.len(),
                indexed_size
            ));
        }
        if let Some(indexed_mtime) = row.modified_unix
            && let Ok(live_mtime) = metadata.modified()
            && let Ok(live) = live_mtime.duration_since(std::time::UNIX_EPOCH)
            && live.as_secs() != indexed_mtime
        {
            drift.push(format!(
                "mtime {} != indexed {}",
                live.as_secs(),
                indexed_mtime
            ));
        }
        if !drift.is_empty() {
            out.push(Finding {
                kind: "index-record-stale",
                selector: row.id.clone(),
                source_path: row.source_path.clone(),
                detail: format!(
                    "{}; stored sha256 {indexed} may no longer describe the file (rerun with --rehash for full revalidation)",
                    drift.join("; ")
                ),
            });
        }
    }
    out
}

/// A usable mtime: `None` and the `0` placeholder (unset FAT/zip entry,
/// failed conversion) are "unavailable", never a 1970 timestamp — feeding
/// them into gap/regression checks fabricates findings.
fn usable_mtime(row: &IndexedRow) -> Option<u64> {
    row.modified_unix.filter(|mtime| *mtime > 0)
}

/// Rows excluded from timestamp checks for lack of an mtime; reported in
/// the run record so the exclusion is disclosed, not silent.
fn mtime_unavailable_count(rows: &[IndexedRow]) -> usize {
    rows.iter()
        .filter(|row| usable_mtime(row).is_none())
        .count()
}

fn timestamp_findings(rows: &[IndexedRow]) -> Vec<Finding> {
    // R6-I10: grouped and ordered by the lossless OS path — two folders
    // whose names differ only in non-Unicode units share one lossy display
    // string and must not be merged into one sequence.
    let mut by_parent: std::collections::BTreeMap<std::path::PathBuf, Vec<(&IndexedRow, u64)>> =
        std::collections::BTreeMap::new();
    for row in rows {
        let Some(mtime) = usable_mtime(row) else {
            continue;
        };
        let parent = row
            .os_path()
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        by_parent.entry(parent).or_default().push((row, mtime));
    }

    let mut out = Vec::new();
    for (_parent, mut group) in by_parent {
        group.sort_by(|a, b| {
            a.0.source_path
                .cmp(&b.0.source_path)
                .then_with(|| a.0.os_path().cmp(&b.0.os_path()))
        });
        for window in group.windows(2) {
            let (left, left_ts) = window[0];
            let (right, right_ts) = window[1];
            if right_ts < left_ts {
                out.push(Finding {
                    kind: "timestamp-regression",
                    selector: right.id.clone(),
                    source_path: right.source_path.clone(),
                    detail: format!(
                        "path-sorted after {} but mtime {} < {}",
                        left.source_path, right_ts, left_ts
                    ),
                });
            } else if right_ts.saturating_sub(left_ts) > GAP_SECS {
                out.push(Finding {
                    kind: "timestamp-gap",
                    selector: right.id.clone(),
                    source_path: right.source_path.clone(),
                    detail: format!(
                        "mtime gap {}s from {} (threshold {}s)",
                        right_ts - left_ts,
                        left.source_path,
                        GAP_SECS
                    ),
                });
            }
        }
    }
    out
}

fn container_stream_findings(rows: &[IndexedRow]) -> Vec<Finding> {
    let mut out = Vec::new();
    for row in rows {
        if row.ffprobe_ok != Some(true) {
            continue;
        }
        let format = row
            .format_name
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        let codec = row
            .video_codec
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        if (format.contains("image2") || format == "png_pipe" || format == "jpeg_pipe")
            && !codec.is_empty()
        {
            out.push(Finding {
                kind: "container-stream-mismatch",
                selector: row.id.clone(),
                source_path: row.source_path.clone(),
                detail: format!(
                    "format `{format}` looks like still image but video codec is `{codec}`"
                ),
            });
        }
        if let Some(duration) = row.duration_seconds
            && duration == 0.0
            && !codec.is_empty()
        {
            out.push(Finding {
                kind: "container-stream-mismatch",
                selector: row.id.clone(),
                source_path: row.source_path.clone(),
                detail: format!("duration 0 with video codec `{codec}`"),
            });
        }
    }
    out
}

/// DAV frame-interval findings: consecutive video frames on the same channel
/// whose embedded recording times jump by more than `DAV_FRAME_GAP_SECS`,
/// indicating dropped frames or a recorder pause. Runs only for indexed DAV
/// sources whose files are still reachable (missing files are skipped). A
/// reachable DAV the frame walker cannot parse yields a
/// `dav-parse-limitation` finding instead of being dropped silently.
fn dav_frame_gap_findings(case_dir: &Path, rows: &[IndexedRow]) -> Vec<Finding> {
    let _ = case_dir;
    let mut out = Vec::new();
    for row in rows {
        let os_path = row.os_path();
        let path = os_path.as_path();
        if !path.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("dav"))
        {
            continue;
        }
        // `walk_frames` stops at the first frame it cannot parse. Never drop
        // the file silently: record that its frame-interval check did not
        // run, the same way invalid dates become a coverage limitation.
        let frames = match crate::dav::walk_frames(path) {
            Ok(frames) => frames,
            Err(err) => {
                out.push(Finding {
                    kind: "dav-parse-limitation",
                    selector: row.id.clone(),
                    source_path: row.source_path.clone(),
                    detail: format!(
                        "DAV frame walk stopped: {err}; frame-interval (gap/regression) checks were not run for this file — a parser limitation, not a finding about the recording"
                    ),
                });
                continue;
            }
        };

        out.extend(dav_sequence_findings(row, &frames));
    }
    out
}

fn dav_sequence_findings(row: &IndexedRow, frames: &[crate::dav::DavFrame]) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut predecessors = std::collections::HashMap::<u16, (u64, i64)>::new();
    for frame in frames.iter().filter(|frame| {
        matches!(
            frame.stream_type,
            crate::dav::STREAM_VIDEO_P | crate::dav::STREAM_VIDEO_I
        )
    }) {
        let Some(seconds) = packed_date_seconds(frame) else {
            predecessors.remove(&frame.channel);
            out.push(Finding {
                kind: "dav-date-coverage-limitation",
                selector: row.id.clone(),
                source_path: row.source_path.clone(),
                detail: format!(
                    "skipped invalid Gregorian date at byte offset {} on channel {}; adjacent interval comparisons unavailable",
                    frame.offset, frame.channel
                ),
            });
            continue;
        };
        let Some((prev_offset, prev_seconds)) =
            predecessors.insert(frame.channel, (frame.offset, seconds))
        else {
            continue;
        };
        let delta = seconds - prev_seconds;
        let kind = if delta < 0 {
            "dav-frame-timestamp-regression"
        } else if delta > DAV_FRAME_GAP_SECS {
            "dav-frame-gap"
        } else {
            continue;
        };
        out.push(Finding {
            kind,
            selector: row.id.clone(),
            source_path: row.source_path.clone(),
            detail: format!(
                "channel {} video frames at byte offsets {} -> {} have packed civil-time span {delta}s (gap threshold {DAV_FRAME_GAP_SECS}s); 1-second resolution, timezone unknown, free-running counter not used as wall-clock milliseconds; candidate clock discontinuity, not proof of dropped frames",
                frame.channel, prev_offset, frame.offset
            ),
        });
    }
    out
}

fn packed_date_seconds(frame: &crate::dav::DavFrame) -> Option<i64> {
    let (year, month, day, hour, minute, second) = frame.date_breakdown();
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&month) || day == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    if day > month_days[(month - 1) as usize] {
        return None;
    }
    let previous_year = i64::from(year) - 1;
    let days = previous_year * 365 + previous_year / 4 - previous_year / 100
        + previous_year / 400
        + i64::from(month_days[..(month - 1) as usize].iter().sum::<u32>())
        + i64::from(day - 1);
    Some(days * 86400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))
}

pub fn read_indexed_rows(case_dir: &Path) -> Result<Vec<IndexedRow>, String> {
    let path = case_dir.join("db/videos.jsonl");
    let text = match read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(format!(
                "failed to read indexed videos {}: {err}",
                path.display()
            ));
        }
    };
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .map(|value| IndexedRow {
            id: value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            source_path: value
                .get("source_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            sha256: value
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            size_bytes: value.get("size_bytes").and_then(serde_json::Value::as_u64),
            // E8: 0 is the "no timestamp" placeholder, never 1970.
            modified_unix: value
                .get("modified_unix")
                .and_then(serde_json::Value::as_u64)
                .filter(|mtime| *mtime > 0),
            duration_seconds: value
                .get("duration_seconds")
                .and_then(serde_json::Value::as_f64),
            format_name: value
                .get("format_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            video_codec: value
                .get("video_codec")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            ffprobe_ok: value.get("ffprobe_ok").and_then(serde_json::Value::as_bool),
            raw_path: value
                .get("source_path_raw_hex")
                .and_then(serde_json::Value::as_str)
                .and_then(crate::model::path_from_raw_hex),
        })
        .filter(|row| !row.id.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dav_frame_gap_finding_marks_large_jumps() {
        // Two DAV video frames whose embedded recording clocks are 10s apart.
        let dir =
            std::env::temp_dir().join(format!("frametrace-dav-gap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dav_path = dir.join("cam.dav");
        std::fs::write(&dav_path, dav_fixture_with_dates(0, 10)).unwrap();

        let rows = vec![IndexedRow {
            id: "vid_1".into(),
            source_path: dav_path.to_string_lossy().to_string(),
            sha256: None,
            size_bytes: None,
            modified_unix: None,
            duration_seconds: None,
            format_name: None,
            video_codec: None,
            ffprobe_ok: None,
            raw_path: None,
        }];
        let findings = dav_frame_gap_findings(&dir, &rows);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "dav-frame-gap");
        assert!(
            findings[0].detail.contains("span 10s") && !findings[0].detail.contains("10.500"),
            "{detail}",
            detail = findings[0].detail
        );

        // A normal 1s cadence stays clean even across the 65535 ms wrap.
        let steady_path = dir.join("steady.dav");
        std::fs::write(&steady_path, dav_fixture_with_dates(0, 1)).unwrap();
        let steady_rows = vec![IndexedRow {
            id: "vid_2".into(),
            source_path: steady_path.to_string_lossy().to_string(),
            sha256: None,
            size_bytes: None,
            modified_unix: None,
            duration_seconds: None,
            format_name: None,
            video_codec: None,
            ffprobe_ok: None,
            raw_path: None,
        }];
        assert!(dav_frame_gap_findings(&dir, &steady_rows).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Item 16: a DAV whose frame walk fails (here: the second frame's
    /// DHAV magic is corrupted) yields one `dav-parse-limitation` finding
    /// instead of vanishing; another DAV's gap finding is unaffected.
    #[test]
    fn broken_dav_reports_parse_limitation_instead_of_skipping() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-dav-broken-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let row = |id: &str, path: &Path| IndexedRow {
            id: id.into(),
            source_path: path.to_string_lossy().to_string(),
            sha256: None,
            size_bytes: None,
            modified_unix: None,
            duration_seconds: None,
            format_name: None,
            video_codec: None,
            ffprobe_ok: None,
            raw_path: None,
        };
        let mut broken = dav_fixture_with_dates(0, 1);
        let second_frame = broken.len() / 2;
        assert_eq!(&broken[second_frame..second_frame + 4], b"DHAV");
        broken[second_frame..second_frame + 4].copy_from_slice(b"XXXX");
        let broken_path = dir.join("broken.dav");
        std::fs::write(&broken_path, &broken).unwrap();
        let gap_path = dir.join("gap.dav");
        std::fs::write(&gap_path, dav_fixture_with_dates(0, 10)).unwrap();

        let findings =
            dav_frame_gap_findings(&dir, &[row("vid_1", &broken_path), row("vid_2", &gap_path)]);
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert_eq!(findings[0].kind, "dav-parse-limitation");
        assert_eq!(findings[0].selector, "vid_1");
        assert!(
            findings[0]
                .detail
                .contains(&format!("offset {second_frame}"))
                && findings[0].detail.contains("DHAV frame magic"),
            "{}",
            findings[0].detail
        );
        assert_eq!(findings[1].kind, "dav-frame-gap");
        assert_eq!(findings[1].selector, "vid_2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Builds a minimal two-video-frame DAV whose second frame's packed date
    /// is `gap_secs` after the first (same minute, so only seconds differ).
    /// The ms counter sits near its 65535 wrap so a naive seconds-style
    /// reading of it would produce a wildly wrong span.
    fn dav_fixture_with_dates(first_sec: u32, second_sec: u32) -> Vec<u8> {
        fn frame(channel: u8, second: u32, timestamp_ms: u16, payload: &[u8]) -> Vec<u8> {
            let header_len = 24u32;
            let frame_length = header_len + payload.len() as u32 + 8;
            let date = (26 << 26) | (1 << 22) | (1 << 17) | (second & 0x3F);
            let mut bytes = Vec::with_capacity(frame_length as usize);
            bytes.extend_from_slice(b"DHAV");
            bytes.push(0xFD); // key video frame
            bytes.push(0);
            bytes.push(channel);
            bytes.push(0);
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&frame_length.to_le_bytes());
            bytes.extend_from_slice(&date.to_le_bytes());
            bytes.extend_from_slice(&timestamp_ms.to_le_bytes());
            bytes.push(0); // ext_length
            bytes.push(0); // checksum
            bytes.extend_from_slice(payload);
            bytes.extend_from_slice(b"dhav");
            bytes.extend_from_slice(&frame_length.to_le_bytes());
            bytes
        }
        let mut out = Vec::new();
        out.extend(frame(1, first_sec, 65000, b"V1"));
        out.extend(frame(1, second_sec, 500, b"V2"));
        out
    }

    fn dated_frame(date: (u32, u32, u32, u32, u32, u32), channel: u16) -> crate::dav::DavFrame {
        let (year, month, day, hour, minute, second) = date;
        crate::dav::DavFrame {
            offset: 0,
            stream_type: crate::dav::STREAM_VIDEO_I,
            channel,
            date: ((year - 2000) << 26)
                | (month << 22)
                | (day << 17)
                | (hour << 12)
                | (minute << 6)
                | second,
            timestamp_ms: 65500,
            payload_offset: 24,
            payload_size: 2,
        }
    }

    fn dav_test_row() -> IndexedRow {
        IndexedRow {
            id: "vid_1".into(),
            source_path: "cam.dav".into(),
            sha256: None,
            size_bytes: None,
            modified_unix: None,
            duration_seconds: None,
            format_name: None,
            video_codec: None,
            ffprobe_ok: None,
            raw_path: None,
        }
    }

    #[test]
    fn dav_gregorian_boundaries_have_one_second_cadence() {
        for (left, right) in [
            ((2026, 1, 31), (2026, 2, 1)),
            ((2026, 4, 30), (2026, 5, 1)),
            ((2026, 2, 28), (2026, 3, 1)),
            ((2024, 2, 28), (2024, 2, 29)),
            ((2024, 2, 29), (2024, 3, 1)),
            ((2000, 2, 29), (2000, 3, 1)),
            ((2023, 12, 31), (2024, 1, 1)),
            ((2024, 12, 31), (2025, 1, 1)),
        ] {
            let prev = dated_frame((left.0, left.1, left.2, 23, 59, 59), 1);
            let mut next = dated_frame((right.0, right.1, right.2, 0, 0, 0), 1);
            next.timestamp_ms = 500;
            assert_eq!(
                packed_date_seconds(&next).unwrap() - packed_date_seconds(&prev).unwrap(),
                1
            );
            assert!(dav_sequence_findings(&dav_test_row(), &[prev, next]).is_empty());
        }
    }

    #[test]
    fn dav_interleaved_channels_keep_independent_predecessors() {
        let frames = [
            dated_frame((2026, 1, 1, 0, 0, 0), 1),
            dated_frame((2026, 1, 1, 12, 0, 1), 2),
            dated_frame((2026, 1, 1, 0, 0, 10), 1),
            dated_frame((2026, 1, 1, 12, 0, 0), 2),
        ];
        assert!(dav_sequence_findings(&dav_test_row(), &frames[..2]).is_empty());
        let findings = dav_sequence_findings(&dav_test_row(), &frames);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].kind, "dav-frame-gap");
        assert!(findings[0].detail.contains("span 10s"));
        assert_eq!(findings[1].kind, "dav-frame-timestamp-regression");
    }

    #[test]
    fn dav_invalid_dates_limit_coverage_without_bridging_intervals() {
        for date in [
            (2026, 0, 1, 0, 0, 0),
            (2026, 13, 1, 0, 0, 0),
            (2026, 1, 0, 0, 0, 0),
            (2026, 2, 29, 0, 0, 0),
            (2026, 4, 31, 0, 0, 0),
            (2026, 1, 1, 24, 0, 0),
            (2026, 1, 1, 0, 60, 0),
            (2026, 1, 1, 0, 0, 60),
        ] {
            let invalid = dated_frame(date, 1);
            assert_eq!(packed_date_seconds(&invalid), None);
            let findings = dav_sequence_findings(
                &dav_test_row(),
                &[
                    dated_frame((2026, 1, 1, 0, 0, 0), 1),
                    invalid,
                    dated_frame((2026, 1, 1, 0, 0, 10), 1),
                ],
            );
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].kind, "dav-date-coverage-limitation");
        }
    }

    #[test]
    fn missing_or_zero_mtime_never_creates_timestamp_findings() {
        let row = |id: &str, path: &str, mtime: Option<u64>| IndexedRow {
            id: id.into(),
            source_path: path.into(),
            sha256: None,
            size_bytes: None,
            modified_unix: mtime,
            duration_seconds: None,
            format_name: None,
            video_codec: None,
            ffprobe_ok: None,
            raw_path: None,
        };
        let rows = vec![
            row("vid_1", "C:/ev/a.mp4", Some(1_700_000_000)),
            row("vid_2", "C:/ev/b.mp4", None),
            row("vid_3", "C:/ev/c.mp4", Some(0)),
            row("vid_4", "C:/ev/d.mp4", Some(1_700_000_010)),
        ];
        // b/c have no usable mtime: no regression (0 < a) and no gap (d - 0).
        assert!(timestamp_findings(&rows).is_empty());
        assert_eq!(mtime_unavailable_count(&rows), 2);
    }

    #[test]
    fn detects_timestamp_regression_within_folder() {
        let rows = vec![
            IndexedRow {
                id: "vid_1".into(),
                source_path: "C:/ev/a.mp4".into(),
                sha256: None,
                size_bytes: None,
                modified_unix: Some(200),
                duration_seconds: None,
                format_name: None,
                video_codec: None,
                ffprobe_ok: None,
                raw_path: None,
            },
            IndexedRow {
                id: "vid_2".into(),
                source_path: "C:/ev/b.mp4".into(),
                sha256: None,
                size_bytes: None,
                modified_unix: Some(100),
                duration_seconds: None,
                format_name: None,
                video_codec: None,
                ffprobe_ok: None,
                raw_path: None,
            },
        ];
        let findings = timestamp_findings(&rows);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "timestamp-regression");
        assert_eq!(findings[0].selector, "vid_2");
    }

    /// R6-I10: two folders whose names differ only in non-Unicode units are
    /// two sequences — merged under their shared lossy spelling, a later
    /// file of one folder read as a "timestamp regression" of the other.
    #[test]
    fn timestamp_groups_use_the_lossless_folder() {
        let base = std::path::PathBuf::from(if cfg!(windows) { r"C:\ev" } else { "/ev" });
        let row = |id: &str, folder: u8, name: &str, mtime: u64| {
            let path = base
                .join(crate::audit::test_non_unicode_twin("cam", folder))
                .join(name);
            IndexedRow {
                id: id.into(),
                source_path: path.to_string_lossy().to_string(),
                sha256: None,
                size_bytes: None,
                modified_unix: Some(mtime),
                duration_seconds: None,
                format_name: None,
                video_codec: None,
                ffprobe_ok: None,
                raw_path: Some(path),
            }
        };
        let rows = vec![row("vid_1", 0, "a.mp4", 200), row("vid_2", 1, "b.mp4", 100)];
        assert_eq!(
            rows[0].source_path[..rows[0].source_path.len() - 5],
            rows[1].source_path[..rows[1].source_path.len() - 5]
        );
        assert!(
            timestamp_findings(&rows).is_empty(),
            "{:?}",
            timestamp_findings(&rows).len()
        );
    }

    /// R5-P3: re-hashing opens the record's lossless path. A non-Unicode
    /// file name (display spelling has U+FFFD) is hashed and matches — it
    /// is not "missing".
    #[test]
    fn rehash_opens_the_lossless_path_of_non_unicode_names() {
        let dir = std::env::temp_dir().join(format!("frametrace-p3-rehash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db")).unwrap();
        let media = dir.join(crate::model::test_non_unicode_name("clip"));
        if std::fs::write(&media, b"payload").is_err() {
            eprintln!("skipped: this filesystem refuses non-Unicode names");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let sha = crate::sha256::digest_bytes(b"payload");
        let row = serde_json::json!({
            "id": "vid_000001",
            "source_path": media.to_string_lossy(),
            "source_path_raw_hex": crate::model::path_raw_hex(&media).unwrap(),
            "sha256": sha,
        });
        std::fs::write(dir.join("db/videos.jsonl"), format!("{row}\n")).unwrap();
        let rows = read_indexed_rows(&dir).unwrap();
        let mut coverage = RehashCoverage::default();
        let mut checks = Vec::new();
        let findings = hash_revalidation_findings(&rows, &mut coverage, &mut checks);
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(checks[0].status, "match", "{checks:?}");
        assert_eq!(checks[0].observed.as_deref(), Some(sha.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stored_hash_lane_flags_drift_and_missing_files_without_rehashing() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-staleness-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let media = dir.join("clip.mp4");
        std::fs::write(&media, b"payload").unwrap();
        let metadata = std::fs::metadata(&media).unwrap();
        let live_mtime = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let row = |size: Option<u64>, mtime: Option<u64>, path: &Path| IndexedRow {
            id: "vid_1".into(),
            source_path: path.to_string_lossy().to_string(),
            sha256: Some("deadbeef".into()),
            size_bytes: size,
            modified_unix: mtime,
            duration_seconds: None,
            format_name: None,
            video_codec: None,
            ffprobe_ok: None,
            raw_path: None,
        };

        // Recorded metadata still matches: stored hash is treated as current.
        assert!(
            index_staleness_findings(&[row(Some(metadata.len()), Some(live_mtime), &media)])
                .is_empty()
        );

        // Size drift means the stored hash may no longer describe the file.
        let drifted =
            index_staleness_findings(&[row(Some(metadata.len() + 1), Some(live_mtime), &media)]);
        assert_eq!(drifted.len(), 1);
        assert_eq!(drifted[0].kind, "index-record-stale");
        assert!(drifted[0].detail.contains("size"), "{drifted:?}");

        // A vanished source is reported from the record, not silently skipped.
        let missing = index_staleness_findings(&[row(
            Some(metadata.len()),
            Some(live_mtime),
            &dir.join("gone.mp4"),
        )]);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].detail.contains("no longer readable"));

        // Rows without a stored hash stay out of the staleness lane.
        let mut unhashed = row(Some(metadata.len()), Some(live_mtime), &media);
        unhashed.sha256 = None;
        assert!(index_staleness_findings(&[unhashed]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_zero_duration_with_video_codec() {
        let rows = vec![IndexedRow {
            id: "vid_1".into(),
            source_path: "C:/ev/a.mp4".into(),
            sha256: None,
            size_bytes: None,
            modified_unix: None,
            duration_seconds: Some(0.0),
            format_name: Some("mov,mp4,m4a,3gp,3g2,mj2".into()),
            video_codec: Some("h264".into()),
            ffprobe_ok: Some(true),
            raw_path: None,
        }];
        let findings = container_stream_findings(&rows);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "container-stream-mismatch");
    }

    #[test]
    fn label_is_candidate_finding_not_legal_claim() {
        assert_eq!(LABEL, "candidate-finding");
        assert!(!LABEL.contains("tamper"));
        assert!(!LABEL.contains("authentic"));
    }

    /// E8 follow-up: the scan reads the index through the counted reader —
    /// a torn line and an id-less record are counted, a stale record (file
    /// absent at the last scan) yields no current finding, mtime 0 is "no
    /// time" — and the run record discloses all of it.
    #[test]
    fn scan_discloses_index_lines_and_stale_records_it_did_not_examine() {
        let dir = std::env::temp_dir().join(format!(
            "ft-anomaly-index-coverage-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(dir.join("db")).unwrap();
        let missing = dir.join("gone.mp4").to_string_lossy().to_string();
        let lines = [
            serde_json::json!({"id": "vid_1", "source_path": missing, "sha256": "ab", "size_bytes": 3, "modified_unix": 5, "index_status": "stale"}).to_string(),
            serde_json::json!({"source_path": "/x.mp4"}).to_string(),
            serde_json::json!({"id": "vid_2", "source_path": "/y.mp4", "modified_unix": 0}).to_string(),
            "{\"id\":\"vid_3\",\"sour".to_string(),
        ];
        std::fs::write(dir.join("db/videos.jsonl"), lines.join("\n")).unwrap();
        let result = scan_case(&dir, false).unwrap();
        assert_eq!(
            result.index,
            IndexCoverage {
                unparseable_lines: 1,
                rows_without_id: 1,
                stale_rows: 1
            }
        );
        assert!(
            !result.findings.iter().any(|f| f.selector == "vid_1"),
            "stale record yields no current finding: {:?}",
            result.findings
        );
        let rows = read_indexed_rows(&dir).unwrap();
        assert_eq!(
            rows.iter().find(|r| r.id == "vid_2").unwrap().modified_unix,
            None
        );
        let log = std::fs::read_to_string(&result.log_path).unwrap();
        let marker: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!(marker["index_stale_rows"], 1);
        assert_eq!(marker["index_unparseable_lines"], 1);
        assert_eq!(marker["coverage_limited"], true);
        assert!(marker["detail"].as_str().unwrap().contains("stale record"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
