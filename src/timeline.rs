//! Candidate timeline event stream (FORENSIC_HARDENING_PLAN Corpus D).
//!
//! Merges timestamps already recorded in the case — indexed file mtimes and
//! stale markings, ffprobe media metadata (`creation_time` tags), carve-run
//! times, and chained audit-log entries (`evidence/logs/*.jsonl` and
//! `artifacts/**/*.jsonl`) — into one JSONL stream sorted by `ts_unix`.
//! Events are candidate-grade: they come from recorded metadata, not
//! validated ground truth, and records without a timestamp are excluded
//! rather than zeroed.

use crate::audit;
use crate::util::{json_escape, now_unix, read_to_string};
use std::path::{Path, PathBuf};

/// Candidate-grade label: timestamps are recorded metadata, not truth.
pub const LABEL: &str = "candidate-timeline";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEvent {
    pub ts_unix: u64,
    pub source: &'static str,
    pub kind: &'static str,
    pub path: String,
    pub detail: String,
}

impl TimelineEvent {
    fn to_json(&self) -> String {
        format!(
            "{{\"label\":\"{}\",\"ts_unix\":{},\"source\":\"{}\",\"kind\":\"{}\",\"path\":\"{}\",\"detail\":\"{}\"}}",
            LABEL,
            self.ts_unix,
            json_escape(self.source),
            json_escape(self.kind),
            json_escape(&self.path),
            json_escape(&self.detail),
        )
    }
}

#[derive(Debug, Clone)]
pub struct TimelineResult {
    pub event_count: usize,
    pub skipped_no_timestamp: usize,
    pub output_path: PathBuf,
    pub generated_unix: u64,
}

/// Build the merged event stream, write it as JSONL inside the case, and
/// append a chained entry to `evidence/logs/timeline-log.jsonl`.
pub fn generate_timeline(case_dir: &Path, output_path: &Path) -> Result<TimelineResult, String> {
    crate::tool_policy::require_case_output_path(case_dir, output_path, "timeline")?;
    let (events, skipped_no_timestamp) = collect_events(case_dir, output_path)?;
    let generated_unix = now_unix()?;
    let mut body = String::new();
    for event in &events {
        body.push_str(&event.to_json());
        body.push('\n');
    }
    crate::tool_policy::write_case_report(
        case_dir,
        output_path,
        "db/timeline.jsonl",
        "timeline",
        &body,
    )?;
    let line = format!(
        "{{\"schema_version\":1,\"event\":\"timeline\",\"generated_unix\":{},\"label\":\"{}\",\"output_path\":\"{}\",\"event_count\":{},\"skipped_no_timestamp\":{},\"detail\":\"candidate-grade merge of index mtimes and stale marks, ffprobe creation_time tags, carve-run times, and audit-log entries\"}}",
        generated_unix,
        LABEL,
        json_escape(&output_path.to_string_lossy()),
        events.len(),
        skipped_no_timestamp,
    );
    audit::append_chained_jsonl(&case_dir.join("evidence/logs/timeline-log.jsonl"), &line)?;
    Ok(TimelineResult {
        event_count: events.len(),
        skipped_no_timestamp,
        output_path: output_path.to_path_buf(),
        generated_unix,
    })
}

/// Merge every timestamped source into one deterministically ordered event
/// list. Returns the events plus how many input records carried no usable
/// timestamp at all.
fn collect_events(
    case_dir: &Path,
    output_path: &Path,
) -> Result<(Vec<TimelineEvent>, usize), String> {
    let mut events = Vec::new();
    let mut skipped = index_events(case_dir, &mut events)?;
    skipped += carve_events(case_dir, &mut events)?;
    skipped += audit_log_events(case_dir, output_path, &mut events)?;
    // Deterministic ordering: repeated runs over unchanged inputs must emit
    // byte-identical streams (Corpus D), so ties break on every field.
    events.sort_by(|a, b| {
        a.ts_unix
            .cmp(&b.ts_unix)
            .then_with(|| a.source.cmp(b.source))
            .then_with(|| a.kind.cmp(b.kind))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.detail.cmp(&b.detail))
    });
    Ok((events, skipped))
}

/// One event per indexed file mtime plus one per embedded ffprobe
/// `creation_time` tag. Returns how many index rows produced no event.
fn index_events(case_dir: &Path, events: &mut Vec<TimelineEvent>) -> Result<usize, String> {
    let path = case_dir.join("db/videos.jsonl");
    let text = match read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(format!(
                "failed to read indexed videos {}: {err}",
                path.display()
            ));
        }
    };
    let mut skipped = 0usize;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let source_path = value
            .get("source_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut emitted = false;
        // An mtime of 0 is a "no timestamp" placeholder (unset FAT/zip
        // entries, failed conversions), not a real 1970 event.
        if let Some(mtime) = value
            .get("modified_unix")
            .and_then(serde_json::Value::as_u64)
            .filter(|mtime| *mtime > 0)
        {
            emitted = true;
            events.push(TimelineEvent {
                ts_unix: mtime,
                source: "index",
                kind: "file-modified",
                path: source_path.clone(),
                detail: format!("indexed video {id} filesystem mtime"),
            });
        }
        // Stale markers appended by rescan merges carry their own timestamp.
        if let Some(stale_unix) = value
            .get("stale_since_unix")
            .and_then(serde_json::Value::as_u64)
        {
            emitted = true;
            events.push(TimelineEvent {
                ts_unix: stale_unix,
                source: "index",
                kind: "file-marked-stale",
                path: source_path.clone(),
                detail: format!("indexed video {id} marked stale by rescan"),
            });
        }
        for (location, raw) in creation_time_tags(&value) {
            // A tag that will not parse is excluded, never emitted as 0.
            if let Some((ts, tz_assumed)) = parse_creation_time(&raw) {
                emitted = true;
                let caveat = if tz_assumed {
                    " (tz_assumed: tag has no zone, interpreted as UTC; recorder clocks are often local time)"
                } else {
                    ""
                };
                events.push(TimelineEvent {
                    ts_unix: ts,
                    source: "ffprobe",
                    kind: "media-creation-time",
                    path: source_path.clone(),
                    detail: format!(
                        "indexed video {id} ffprobe {location} creation_time {raw}{caveat}"
                    ),
                });
            }
        }
        if !emitted {
            skipped += 1;
        }
    }
    Ok(skipped)
}

/// One event per carved artifact stamped with its run's `carved_unix` from
/// `db/carve_results.json` (the per-artifact carve-log lines carry no
/// timestamp of their own). Returns how many artifacts were skipped.
fn carve_events(case_dir: &Path, events: &mut Vec<TimelineEvent>) -> Result<usize, String> {
    let path = case_dir.join("db/carve_results.json");
    let text = match read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => {
            return Err(format!(
                "failed to read carve results {}: {err}",
                path.display()
            ));
        }
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(0); // unparsable snapshot is not fatal to the timeline
    };
    let Some(carved_unix) = value.get("carved_unix").and_then(serde_json::Value::as_u64) else {
        return Ok(0);
    };
    let mut skipped = 0usize;
    let Some(artifacts) = value.get("artifacts").and_then(|v| v.as_array()) else {
        return Ok(0);
    };
    for artifact in artifacts {
        let output_path = artifact
            .get("output_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if output_path.is_empty() {
            skipped += 1;
            continue;
        }
        let id = artifact
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let source = artifact
            .get("source_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let offset = artifact
            .get("offset")
            .and_then(serde_json::Value::as_u64)
            .map(|offset| offset.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        events.push(TimelineEvent {
            ts_unix: carved_unix,
            source: "carve",
            kind: "carved-candidate",
            path: output_path.to_string(),
            detail: format!("carved candidate {id} from {source} at offset {offset}"),
        });
    }
    Ok(skipped)
}

/// One event per chained audit-log entry that carries a `*_unix` timestamp.
/// Every `*.jsonl` under `evidence/logs` and `artifacts` is a chained audit
/// log by convention (export-log, validation-log, carve-log, tsk-audit,
/// timeline-log itself, …); each parseable line contributes an
/// `audit-event` stamped with its first `*_unix` field, in document order.
/// Entries without a timestamp — and torn final lines, the documented
/// crash-survivability mode — are counted as skipped, never emitted as 0.
/// The timeline's own output file is skipped when it sits under a scanned
/// root so a custom `--output` is not ingested as an event.
fn audit_log_events(
    case_dir: &Path,
    output_path: &Path,
    events: &mut Vec<TimelineEvent>,
) -> Result<usize, String> {
    let mut files = Vec::new();
    for root in [case_dir.join("evidence/logs"), case_dir.join("artifacts")] {
        collect_jsonl_files(&root, &mut files)?;
    }
    files.sort(); // deterministic input order for the merge
    let output_canonical = output_path.canonicalize().ok();
    let mut skipped = 0usize;
    for file in files {
        if output_canonical
            .as_deref()
            .is_some_and(|out| Some(out) == file.canonicalize().ok().as_deref())
        {
            continue;
        }
        let text = match read_to_string(&file) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(format!(
                    "failed to read audit log {}: {err}",
                    file.display()
                ));
            }
        };
        let rel = file
            .strip_prefix(case_dir)
            .unwrap_or(&file)
            .to_string_lossy()
            .to_string();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                skipped += 1;
                continue;
            };
            let Some(object) = value.as_object() else {
                skipped += 1;
                continue;
            };
            let Some((ts_key, ts)) = object.iter().find_map(|(key, value)| {
                if key.ends_with("_unix") {
                    value.as_u64().map(|ts| (key.clone(), ts))
                } else {
                    None
                }
            }) else {
                skipped += 1;
                continue;
            };
            let event = object
                .get("event")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("audit-entry");
            // Prefer the entry's subject path so the event sorts alongside
            // the index/carve events it describes; fall back to the log.
            let path = [
                "output_path",
                "source_path",
                "target_path",
                "image_path",
                "report_path",
                "selector",
            ]
            .iter()
            .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| rel.clone());
            events.push(TimelineEvent {
                ts_unix: ts,
                source: "audit-log",
                kind: "audit-event",
                path,
                detail: format!("audit event '{event}' recorded in {rel} ({ts_key})"),
            });
        }
    }
    Ok(skipped)
}

fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(format!("failed to read directory {}: {err}", dir.display()));
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|err| format!("failed to read directory entry in {}: {err}", dir.display()))?;
        let path = entry.path();
        // file_type() does not follow symlinks, so a planted symlink inside
        // the case tree is never walked out of the case.
        let file_type = entry
            .file_type()
            .map_err(|err| format!("failed to read file type {}: {err}", path.display()))?;
        if file_type.is_dir() {
            collect_jsonl_files(&path, out)?;
        } else if file_type.is_file()
            && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Pull `creation_time` tag values out of a videos.jsonl row's embedded
/// ffprobe JSON — format-level first, then per-stream. The `ffprobe` field is
/// normally an inline JSON object; legacy rows may carry it as an escaped
/// string, which is parsed too.
fn creation_time_tags(row: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(probe) = row.get("ffprobe") else {
        return out;
    };
    let owned;
    let probe = match probe {
        serde_json::Value::Object(_) => probe,
        serde_json::Value::String(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(parsed) => {
                owned = parsed;
                &owned
            }
            Err(_) => return out,
        },
        _ => return out,
    };
    let tag_value = |tags: &serde_json::Value| -> Option<String> {
        tags.as_object()?
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("creation_time"))
            .and_then(|(_, value)| value.as_str().map(str::to_string))
    };
    if let Some(tags) = probe.get("format").and_then(|f| f.get("tags"))
        && let Some(value) = tag_value(tags)
    {
        out.push(("format.tags".to_string(), value));
    }
    if let Some(streams) = probe.get("streams").and_then(|s| s.as_array()) {
        for (index, stream) in streams.iter().enumerate() {
            if let Some(tags) = stream.get("tags")
                && let Some(value) = tag_value(tags)
            {
                out.push((format!("streams[{index}].tags"), value));
            }
        }
    }
    out
}

/// Parses `YYYY-MM-DD[T ]HH:MM:SS[.frac][Z|±HH[:MM]]` into unix seconds.
/// Returns `None` for anything unparseable or before the epoch — missing or
/// malformed timestamps are excluded from the stream, never zeroed.
///
/// `#[doc(hidden)]`: exposed so the fuzz harness can feed it arbitrary
/// strings; not part of the supported API.
#[doc(hidden)]
pub fn parse_creation_time_unix(text: &str) -> Option<u64> {
    parse_creation_time(text).map(|(unix, _)| unix)
}

/// `parse_creation_time_unix` plus whether the zone was assumed: `true`
/// when the text carried no `Z`/offset and was interpreted as UTC.
/// Impossible calendar dates (Feb 30) and second 60 are rejected.
pub fn parse_creation_time(text: &str) -> Option<(u64, bool)> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: u32 = text.get(5..7)?.parse().ok()?;
    let day: u32 = text.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    let mut index = 10usize;
    let mut secs_of_day: i64 = 0;
    if bytes
        .get(index)
        .is_some_and(|byte| *byte == b'T' || *byte == b' ')
    {
        index += 1;
        let time = text.get(index..index + 8)?;
        let time_bytes = time.as_bytes();
        if time_bytes[2] != b':' || time_bytes[5] != b':' {
            return None;
        }
        let hour: i64 = time.get(0..2)?.parse().ok()?;
        let minute: i64 = time.get(3..5)?.parse().ok()?;
        let second: i64 = time.get(6..8)?.parse().ok()?;
        if hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        secs_of_day = hour * 3600 + minute * 60 + second;
        index += 8;
        if bytes.get(index) == Some(&b'.') {
            index += 1;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
        }
    }
    let mut offset_secs: i64 = 0;
    let mut tz_assumed = false;
    match bytes.get(index) {
        None => tz_assumed = true,
        Some(b'Z') => {}
        Some(sign @ (b'+' | b'-')) => {
            let digits: String = text
                .get(index + 1..)?
                .chars()
                .filter(|ch| ch.is_ascii_digit())
                .collect();
            if digits.len() < 2 {
                return None;
            }
            let hours: i64 = digits.get(0..2)?.parse().ok()?;
            let minutes: i64 = if digits.len() >= 4 {
                digits.get(2..4)?.parse().ok()?
            } else {
                0
            };
            offset_secs = (hours * 3600 + minutes * 60) * if *sign == b'-' { -1 } else { 1 };
        }
        Some(_) => return None,
    }
    let unix = days_from_civil(year, month, day) * 86_400 + secs_of_day - offset_secs;
    u64::try_from(unix).ok().map(|unix| (unix, tz_assumed))
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since the unix epoch for a civil date (Howard Hinnant's algorithm;
/// valid for proleptic Gregorian dates).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month as i64 + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_case(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("frametrace-timeline-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("db")).unwrap();
        dir
    }

    fn read_events(path: &Path) -> Vec<serde_json::Value> {
        read_to_string(path)
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn merges_index_ffprobe_and_carve_sources_sorted_by_ts() {
        let case_dir = temp_case("mixed");
        fs::write(case_dir.join("case.json"), "{}").unwrap();
        fs::write(
            case_dir.join("db/videos.jsonl"),
            concat!(
                "{\"id\":\"vid_1\",\"source_path\":\"/ev/a.mp4\",\"size_bytes\":10,\"modified_unix\":200,\"ffprobe\":{\"format\":{\"tags\":{\"creation_time\":\"2021-01-01T00:00:00Z\"}}}}\n",
                "{\"id\":\"vid_2\",\"source_path\":\"/ev/b.mp4\",\"size_bytes\":20,\"modified_unix\":100,\"ffprobe\":null}\n",
            ),
        )
        .unwrap();
        fs::write(
            case_dir.join("db/carve_results.json"),
            "{\"carved_unix\":300,\"artifacts\":[{\"id\":\"carve_000001\",\"output_path\":\"/case/artifacts/carved/x.mp4\",\"source_path\":\"/ev/image.raw\",\"offset\":64}]}",
        )
        .unwrap();

        let output = case_dir.join("db/timeline.jsonl");
        let result = generate_timeline(&case_dir, &output).unwrap();
        let events = read_events(&output);
        assert_eq!(events.len(), 4);
        assert_eq!(result.event_count, 4);

        // Sorted by ts: 100, 200, 300, 1609459200.
        let stamps: Vec<u64> = events
            .iter()
            .map(|event| event["ts_unix"].as_u64().unwrap())
            .collect();
        assert_eq!(stamps, vec![100, 200, 300, 1_609_459_200]);

        // All three sources contributed.
        let sources: Vec<&str> = events
            .iter()
            .map(|event| event["source"].as_str().unwrap())
            .collect();
        assert!(sources.contains(&"index"));
        assert!(sources.contains(&"carve"));
        assert!(sources.contains(&"ffprobe"));
        let kinds: Vec<&str> = events
            .iter()
            .map(|event| event["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds[0], "file-modified");
        assert_eq!(kinds[2], "carved-candidate");
        assert_eq!(kinds[3], "media-creation-time");

        // The run is audit-logged and the chain verifies.
        let log = case_dir.join("evidence/logs/timeline-log.jsonl");
        let verification = audit::verify_chained_jsonl(&log).unwrap();
        assert_eq!(verification.entries, 1);
        assert!(read_to_string(&log).unwrap().contains(LABEL));
        let _ = fs::remove_dir_all(&case_dir);
    }

    #[test]
    fn missing_timestamps_are_excluded_not_zeroed() {
        let case_dir = temp_case("missing");
        fs::write(case_dir.join("case.json"), "{}").unwrap();
        fs::write(
            case_dir.join("db/videos.jsonl"),
            concat!(
                "{\"id\":\"vid_1\",\"source_path\":\"/ev/a.mp4\",\"size_bytes\":10,\"modified_unix\":null,\"ffprobe\":null}\n",
                "{\"id\":\"vid_2\",\"source_path\":\"/ev/b.mp4\",\"size_bytes\":20,\"modified_unix\":42,\"ffprobe\":{\"format\":{\"tags\":{\"creation_time\":\"garbage\"}}}}\n",
            ),
        )
        .unwrap();

        let output = case_dir.join("db/timeline.jsonl");
        let result = generate_timeline(&case_dir, &output).unwrap();
        let body = read_to_string(&output).unwrap();
        let events = read_events(&output);
        // Only vid_2's mtime survives; the null mtime and the unparseable
        // creation_time are excluded, never written as ts_unix 0.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["ts_unix"].as_u64(), Some(42));
        assert!(!body.contains("\"ts_unix\":0"), "{body}");
        assert_eq!(result.skipped_no_timestamp, 1);
        let _ = fs::remove_dir_all(&case_dir);
    }

    #[test]
    fn stale_marks_and_audit_log_entries_become_events() {
        let case_dir = temp_case("audit-sources");
        fs::write(case_dir.join("case.json"), "{}").unwrap();
        fs::write(
            case_dir.join("db/videos.jsonl"),
            "{\"id\":\"vid_1\",\"source_path\":\"/ev/gone.mp4\",\"size_bytes\":10,\"modified_unix\":null,\"index_status\":\"stale\",\"stale_since_unix\":77,\"ffprobe\":null}\n",
        )
        .unwrap();
        fs::create_dir_all(case_dir.join("evidence/logs")).unwrap();
        fs::write(
            case_dir.join("evidence/logs/validation-log.jsonl"),
            concat!(
                "{\"event\":\"validate-artifact\",\"validated_unix\":400,\"target_path\":\"/ev/a.mp4\",\"validation_status\":\"ok\"}\n",
                "{\"event\":\"no-timestamp-here\",\"selector\":\"vid_9\"}\n",
            ),
        )
        .unwrap();
        fs::create_dir_all(case_dir.join("artifacts/clips")).unwrap();
        fs::write(
            case_dir.join("artifacts/clips/export-log.jsonl"),
            "{\"event\":\"export-video\",\"exported_unix\":500,\"source_path\":\"/ev/a.mp4\",\"output_path\":\"/case/artifacts/clips/a.mp4\"}\n",
        )
        .unwrap();

        let output = case_dir.join("db/timeline.jsonl");
        let result = generate_timeline(&case_dir, &output).unwrap();
        let events = read_events(&output);

        let kinds: Vec<&str> = events
            .iter()
            .map(|event| event["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"file-marked-stale"), "{kinds:?}");
        assert!(kinds.contains(&"audit-event"), "{kinds:?}");

        let stale = events
            .iter()
            .find(|event| event["kind"] == "file-marked-stale")
            .unwrap();
        assert_eq!(stale["ts_unix"].as_u64(), Some(77));
        assert_eq!(stale["source"].as_str(), Some("index"));

        let audit_events: Vec<_> = events
            .iter()
            .filter(|event| event["source"] == "audit-log")
            .collect();
        assert_eq!(audit_events.len(), 2);
        assert_eq!(audit_events[0]["ts_unix"].as_u64(), Some(400));
        assert_eq!(audit_events[1]["ts_unix"].as_u64(), Some(500));
        // The entry's subject path is used, not the log's own path.
        assert_eq!(audit_events[0]["path"].as_str(), Some("/ev/a.mp4"));
        assert_eq!(
            audit_events[1]["path"].as_str(),
            Some("/case/artifacts/clips/a.mp4")
        );

        // The timestamp-less audit line is counted as skipped, not zeroed.
        assert_eq!(result.skipped_no_timestamp, 1);

        let _ = fs::remove_dir_all(&case_dir);
    }

    #[test]
    fn custom_output_inside_a_scanned_root_is_not_ingested() {
        let case_dir = temp_case("self-output");
        fs::write(case_dir.join("case.json"), "{}").unwrap();
        let output = case_dir.join("evidence/logs/timeline.jsonl");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, "{\"event\":\"timeline\",\"generated_unix\":42}\n").unwrap();
        audit::append_chained_jsonl(
            &case_dir.join("evidence/logs/timeline-log.jsonl"),
            "{\"event\":\"timeline\",\"generated_unix\":43}",
        )
        .unwrap();
        let (collected, _) = collect_events(&case_dir, &output).unwrap();
        let events: Vec<serde_json::Value> = collected
            .iter()
            .map(|event| serde_json::from_str(&event.to_json()).unwrap())
            .collect();
        assert!(
            events
                .iter()
                .all(|event| event["path"] != "evidence/logs/timeline.jsonl"),
            "timeline output must not feed back into itself"
        );
        let audit_events = events
            .iter()
            .filter(|event| event["source"] == "audit-log")
            .count();
        assert_eq!(audit_events, 1);
        assert_eq!(collected.len(), 1);
        let _ = fs::remove_dir_all(&case_dir);
    }

    #[test]
    fn output_must_stay_inside_the_case() {
        let case_dir = temp_case("confined");
        fs::write(case_dir.join("case.json"), "{}").unwrap();
        let outside = case_dir.parent().unwrap().join("timeline-outside.jsonl");
        let err = generate_timeline(&case_dir, &outside).unwrap_err();
        assert!(err.contains("inside the case directory"), "{err}");
        assert!(!outside.exists());
        let _ = fs::remove_dir_all(&case_dir);
    }

    #[test]
    fn rejects_impossible_dates_and_flags_assumed_zone() {
        assert_eq!(parse_creation_time_unix("2021-02-30T00:00:00Z"), None);
        assert_eq!(parse_creation_time_unix("2021-02-29T00:00:00Z"), None);
        assert!(parse_creation_time_unix("2020-02-29T00:00:00Z").is_some());
        assert_eq!(parse_creation_time_unix("2021-04-31T00:00:00Z"), None);
        assert_eq!(parse_creation_time_unix("2021-01-01T00:00:60Z"), None);
        assert_eq!(
            parse_creation_time("2021-01-01 00:00:00"),
            Some((1_609_459_200, true))
        );
        assert_eq!(
            parse_creation_time("2021-01-01T00:00:00Z"),
            Some((1_609_459_200, false))
        );
        assert_eq!(
            parse_creation_time("2021-01-01T09:00:00+09:00"),
            Some((1_609_459_200, false))
        );
    }

    #[test]
    fn zero_mtime_is_not_a_timeline_event() {
        let case_dir = temp_case("zero-mtime");
        fs::write(
            case_dir.join("db/videos.jsonl"),
            "{\"id\":\"vid_1\",\"source_path\":\"/ev/a.mp4\",\"modified_unix\":0,\"ffprobe\":{\"format\":{\"tags\":{\"creation_time\":\"2021-01-01 00:00:00\"}}}}\n",
        )
        .unwrap();
        let mut events = Vec::new();
        let skipped = index_events(&case_dir, &mut events).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "media-creation-time");
        assert!(events[0].detail.contains("tz_assumed"));
        let _ = fs::remove_dir_all(&case_dir);
    }

    #[test]
    fn parses_ffprobe_creation_time_variants() {
        assert_eq!(
            parse_creation_time_unix("2021-01-01T00:00:00Z"),
            Some(1_609_459_200)
        );
        assert_eq!(
            parse_creation_time_unix("2021-01-01 00:00:00"),
            Some(1_609_459_200)
        );
        assert_eq!(
            parse_creation_time_unix("2021-01-01T01:00:00+01:00"),
            Some(1_609_459_200)
        );
        assert_eq!(
            parse_creation_time_unix("2020-12-31T19:00:00-05:00"),
            Some(1_609_459_200)
        );
        assert_eq!(
            parse_creation_time_unix("2021-01-01T00:00:00.500000Z"),
            Some(1_609_459_200)
        );
        assert_eq!(parse_creation_time_unix("not a date"), None);
        assert_eq!(parse_creation_time_unix("2021-13-40T99:99:99Z"), None);
        // Pre-epoch timestamps are excluded rather than clamped to zero.
        assert_eq!(parse_creation_time_unix("1969-12-31T23:59:59Z"), None);
    }
}
