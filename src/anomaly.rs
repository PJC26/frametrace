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
}

/// Scan the case index for candidate anomalies and append a chained log.
pub fn scan_case(case_dir: &Path, rehash: bool) -> Result<AnomalyScanResult, String> {
    let rows = read_indexed_rows(case_dir)?;
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
    if rehash {
        write_hash_verify_log(case_dir, scanned_unix, &checks)?;
    }
    let log_path = case_dir.join("evidence/logs/anomaly-log.jsonl");
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
    };
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
        "{{\"schema_version\":1,\"event\":\"anomaly-scan-run\",\"scanned_unix\":{},\"hash_mode\":\"{}\",\"finding_count\":{},\"mtime_unavailable\":{},\"skipped_no_hash\":{},\"skipped_missing\":{},\"skipped_error\":{},\"coverage_limited\":{},\"detail\":\"{}\"}}",
        scanned_unix,
        if rehash { "rehash" } else { "stored" },
        findings.len(),
        mtime_unavailable,
        coverage.skipped_no_hash,
        coverage.skipped_missing,
        coverage.skipped_error,
        coverage.limited() && rehash,
        json_escape(&detail),
    );
    audit::append_chained_jsonl(&log_path, &line)?;
    Ok(AnomalyScanResult {
        findings,
        log_path,
        scanned_unix,
        coverage,
    })
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HashVerifySummary {
    pub run_unix: i64,
    pub matched: usize,
    pub mismatched: usize,
    pub not_comparable: usize,
}

/// Newest *completed* run in `hash-verify.jsonl`: per-record entries are
/// followed by a `hash-verify-run` marker, so a run cut short by a crash or
/// cancel (records without a marker) is never presented as a result.
pub fn latest_hash_verify_run(case_dir: &Path) -> Option<(i64, Vec<serde_json::Value>)> {
    let text = read_to_string(&case_dir.join(HASH_VERIFY_REL_PATH)).ok()?;
    let mut pending: Vec<serde_json::Value> = Vec::new();
    let mut latest: Option<(i64, Vec<serde_json::Value>)> = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("event").and_then(|e| e.as_str()) {
            Some("hash-verify") => pending.push(value),
            Some("hash-verify-run") => {
                let run_unix = value.get("run_unix").and_then(|v| v.as_i64()).unwrap_or(0);
                latest = Some((run_unix, std::mem::take(&mut pending)));
            }
            _ => {}
        }
    }
    latest
}

pub fn latest_hash_verify_summary(case_dir: &Path) -> Option<HashVerifySummary> {
    let (run_unix, entries) = latest_hash_verify_run(case_dir)?;
    let mut summary = HashVerifySummary {
        run_unix,
        ..Default::default()
    };
    for entry in &entries {
        match entry.get("status").and_then(|v| v.as_str()) {
            Some("match") => summary.matched += 1,
            Some("mismatch") => summary.mismatched += 1,
            _ => summary.not_comparable += 1,
        }
    }
    Some(summary)
}

fn write_hash_verify_log(
    case_dir: &Path,
    run_unix: u64,
    checks: &[HashCheck],
) -> Result<(), String> {
    let log_path = case_dir.join(HASH_VERIFY_REL_PATH);
    let opt = |value: &Option<String>| match value {
        Some(text) => format!("\"{}\"", json_escape(text)),
        None => "null".to_string(),
    };
    for check in checks {
        let line = format!(
            "{{\"schema_version\":1,\"event\":\"hash-verify\",\"run_unix\":{run_unix},\"id\":\"{}\",\"source_path\":\"{}\",\"recorded_sha256\":{},\"observed_sha256\":{},\"status\":\"{}\"}}",
            json_escape(&check.id),
            json_escape(&check.source_path),
            opt(&check.recorded),
            opt(&check.observed),
            check.status,
        );
        audit::append_chained_jsonl(&log_path, &line)?;
    }
    let summary = format!(
        "{{\"schema_version\":1,\"event\":\"hash-verify-run\",\"run_unix\":{run_unix},\"records\":{}}}",
        checks.len()
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
        let path = Path::new(&row.source_path);
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
        let path = Path::new(&row.source_path);
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
    let mut by_parent: std::collections::BTreeMap<String, Vec<(&IndexedRow, u64)>> =
        std::collections::BTreeMap::new();
    for row in rows {
        let Some(mtime) = usable_mtime(row) else {
            continue;
        };
        let parent = Path::new(&row.source_path)
            .parent()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default();
        by_parent.entry(parent).or_default().push((row, mtime));
    }

    let mut out = Vec::new();
    for (_parent, mut group) in by_parent {
        group.sort_by(|a, b| a.0.source_path.cmp(&b.0.source_path));
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
        let path = Path::new(&row.source_path);
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
            modified_unix: value
                .get("modified_unix")
                .and_then(serde_json::Value::as_u64),
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
            },
        ];
        let findings = timestamp_findings(&rows);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "timestamp-regression");
        assert_eq!(findings[0].selector, "vid_2");
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
}
