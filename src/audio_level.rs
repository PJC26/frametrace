//! Measured sound level per indexed video (ffmpeg `volumedetect`).
//!
//! Each audio stream (at most [`MAX_STREAMS`]) is decoded END TO END and
//! its peak / RMS level in dBFS recorded. A stream whose peak is at or
//! below [`SILENT_MAX_DBFS`] is a silent track; anything louder has sound.
//! A decode failure, timeout or `n_samples: 0` is an ERROR (unknown) —
//! never "silent". Nothing here claims speech or conversation.
//!
//! Results go to the chained lane `evidence/logs/audio-level-log.jsonl`,
//! one entry per [`LOG_CHUNK`] files (appends re-read the whole log, so
//! per-file entries would be O(N²)). The case index is not touched.
//! A measurement applies to a record only while the record's current
//! `size_bytes` / `modified_unix` (and SHA-256 when both sides have one)
//! still equal the measured values; the newest entry for an id wins.

use crate::audit;
use crate::checkpoint::{self, ResumeMode, RunCheckpoint};
use crate::tool_policy::{command_version, resolve_tool_binary};
use crate::util::now_unix;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SILENT_MAX_DBFS: f64 = -70.0;
pub const METHOD: &str = "ffmpeg-volumedetect";
pub const LOG_REL: &str = "evidence/logs/audio-level-log.jsonl";
pub const CHECKPOINT_REL: &str = "db/audio-level-progress.jsonl";
/// Files per chained log entry.
pub const LOG_CHUNK: usize = 200;
/// Audio streams measured per file.
pub const MAX_STREAMS: usize = 4;
pub const WORKERS: usize = 4;

/// Aggregated `volumedetect` numbers for one stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumeStats {
    pub n_samples: u64,
    /// dBFS; `-inf` for digital zero.
    pub max_db: f64,
    pub mean_db: f64,
}

fn parse_db(text: &str) -> Option<f64> {
    let value = text.trim().trim_end_matches("dB").trim();
    match value {
        "-inf" => Some(f64::NEG_INFINITY),
        "inf" | "+inf" => Some(f64::INFINITY),
        other => other.parse::<f64>().ok().filter(|v| v.is_finite()),
    }
}

/// Parses ffmpeg's `volumedetect` stderr. A filter graph can be
/// (re)initialised more than once and every instance prints its own block
/// (a first block with `n_samples: 0` is common); blocks with samples are
/// combined — peak = max, mean = sample-weighted power mean. No samples at
/// all, or a block missing its numbers, is an error, never silence.
pub fn parse_volumedetect(stderr: &str) -> Result<VolumeStats, String> {
    #[derive(Default)]
    struct Block {
        n: u64,
        max: Option<f64>,
        mean: Option<f64>,
    }
    let mut blocks: Vec<Block> = Vec::new();
    for line in stderr.lines() {
        let Some(pos) = line.find("] ") else {
            continue;
        };
        if !line[..pos].contains("volumedetect") {
            continue;
        }
        let body = line[pos + 2..].trim();
        if let Some(rest) = body.strip_prefix("n_samples:") {
            let n = rest
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("unreadable n_samples: {}", rest.trim()))?;
            blocks.push(Block {
                n,
                ..Block::default()
            });
        } else if let Some(rest) = body.strip_prefix("mean_volume:") {
            let block = blocks
                .last_mut()
                .ok_or("volumedetect mean_volume without n_samples")?;
            block.mean =
                Some(parse_db(rest).ok_or_else(|| format!("unreadable mean_volume: {rest}"))?);
        } else if let Some(rest) = body.strip_prefix("max_volume:") {
            let block = blocks
                .last_mut()
                .ok_or("volumedetect max_volume without n_samples")?;
            block.max =
                Some(parse_db(rest).ok_or_else(|| format!("unreadable max_volume: {rest}"))?);
        }
    }
    if blocks.is_empty() {
        return Err("volumedetect printed no measurement (n_samples missing)".to_string());
    }
    let mut n_total = 0u64;
    let mut max_db = f64::NEG_INFINITY;
    let mut power_sum = 0.0f64;
    for block in blocks.iter().filter(|b| b.n > 0) {
        let (Some(max), Some(mean)) = (block.max, block.mean) else {
            return Err("volumedetect output incomplete (max/mean missing)".to_string());
        };
        n_total += block.n;
        max_db = max_db.max(max);
        if mean.is_finite() {
            power_sum += block.n as f64 * 10f64.powf(mean / 10.0);
        }
    }
    if n_total == 0 {
        return Err("n_samples 0 — no audio samples were decoded".to_string());
    }
    let mean_db = if power_sum > 0.0 {
        10.0 * (power_sum / n_total as f64).log10()
    } else {
        f64::NEG_INFINITY
    };
    Ok(VolumeStats {
        n_samples: n_total,
        max_db,
        mean_db,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Sound,
    Silent,
    NoAudioStream,
    Error,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sound => "sound",
            Self::Silent => "silent",
            Self::NoAudioStream => "no-audio-stream",
            Self::Error => "error",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "sound" => Some(Self::Sound),
            "silent" => Some(Self::Silent),
            "no-audio-stream" => Some(Self::NoAudioStream),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Peak at or below the threshold (or digital `-inf`) is silent.
pub fn stream_verdict(stats: &VolumeStats) -> Verdict {
    if stats.max_db <= SILENT_MAX_DBFS {
        Verdict::Silent
    } else {
        Verdict::Sound
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamResult {
    /// Absolute ffprobe stream index (None when the index had no stream list).
    pub index: Option<u64>,
    pub codec: Option<String>,
    pub n_samples: Option<u64>,
    pub measured_seconds: Option<f64>,
    /// None when `-inf` (see `max_is_neg_inf`) or on error.
    pub max_volume_db: Option<f64>,
    pub mean_volume_db: Option<f64>,
    pub max_is_neg_inf: bool,
    pub verdict: Verdict,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileResult {
    pub selector: String,
    pub source_path: String,
    pub size_bytes: Option<u64>,
    pub modified_unix: Option<u64>,
    pub sha256_indexed: Option<String>,
    pub result: Verdict,
    pub streams: Vec<StreamResult>,
    pub error: Option<String>,
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some((value * 100.0).round() / 100.0)
}

impl FileResult {
    pub fn to_json(&self) -> Value {
        json!({
            "selector": self.selector,
            "source_path": self.source_path,
            "size_bytes": self.size_bytes,
            "modified_unix": self.modified_unix,
            "sha256_indexed": self.sha256_indexed,
            "result": self.result.as_str(),
            "streams": self.streams.iter().map(|s| json!({
                "index": s.index,
                "codec": s.codec,
                "n_samples": s.n_samples,
                "measured_seconds": s.measured_seconds.map(|v| (v * 1000.0).round() / 1000.0),
                "max_volume_db": s.max_volume_db,
                "mean_volume_db": s.mean_volume_db,
                "max_is_neg_inf": s.max_is_neg_inf,
                "result": s.verdict.as_str(),
                "error": s.error,
            })).collect::<Vec<_>>(),
            "error": self.error,
        })
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let streams = value
            .get("streams")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| StreamResult {
                        index: item.get("index").and_then(Value::as_u64),
                        codec: s(item, "codec"),
                        n_samples: item.get("n_samples").and_then(Value::as_u64),
                        measured_seconds: item.get("measured_seconds").and_then(Value::as_f64),
                        max_volume_db: item.get("max_volume_db").and_then(Value::as_f64),
                        mean_volume_db: item.get("mean_volume_db").and_then(Value::as_f64),
                        max_is_neg_inf: item.get("max_is_neg_inf").and_then(Value::as_bool)
                            == Some(true),
                        verdict: item
                            .get("result")
                            .and_then(Value::as_str)
                            .and_then(Verdict::parse)
                            .unwrap_or(Verdict::Error),
                        error: s(item, "error"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            selector: s(value, "selector")?,
            source_path: s(value, "source_path").unwrap_or_default(),
            size_bytes: value.get("size_bytes").and_then(Value::as_u64),
            modified_unix: value.get("modified_unix").and_then(Value::as_u64),
            sha256_indexed: s(value, "sha256_indexed"),
            result: Verdict::parse(value.get("result")?.as_str()?)?,
            streams,
            error: s(value, "error"),
        })
    }
}

/// Record level: any stream with sound → sound; all silent → silent;
/// otherwise (an error and no sound) → error.
pub fn record_verdict(streams: &[StreamResult]) -> Verdict {
    if streams.is_empty() {
        return Verdict::NoAudioStream;
    }
    if streams.iter().any(|s| s.verdict == Verdict::Sound) {
        Verdict::Sound
    } else if streams.iter().all(|s| s.verdict == Verdict::Silent) {
        Verdict::Silent
    } else {
        Verdict::Error
    }
}

/// The argv shape logged as `command_template` (`<src>` / `<k>` stand in
/// for the source path and the audio stream ordinal).
pub fn command_template() -> Vec<String> {
    stream_args("<src>", "<k>")
}

fn stream_args(src: &str, k: &str) -> Vec<String> {
    ["-nostdin", "-hide_banner", "-v", "info", "-i", src, "-map"]
        .iter()
        .map(|s| s.to_string())
        .chain([format!("0:a:{k}")])
        .chain(
            [
                "-vn",
                "-sn",
                "-dn",
                "-af",
                "volumedetect",
                "-f",
                "null",
                "-",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .collect()
}

/// One indexed audio stream to measure.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioStreamInfo {
    pub index: Option<u64>,
    pub codec: Option<String>,
    pub sample_rate: Option<u64>,
    pub channels: Option<u64>,
}

/// An indexed video row as far as sound measurement cares.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexRow {
    pub id: String,
    pub relative_path: String,
    pub source_path: String,
    pub size_bytes: Option<u64>,
    pub modified_unix: Option<u64>,
    pub sha256: Option<String>,
    pub audio_codec: Option<String>,
    pub duration_seconds: Option<f64>,
    pub stale: bool,
    /// `ffprobe_ok` — without a probe "no audio codec" means "unknown".
    pub probed: bool,
    pub audio_streams: Vec<AudioStreamInfo>,
}

fn num(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|t| t.trim().parse().ok()))
}

impl IndexRow {
    pub fn from_json(row: &Value) -> Option<Self> {
        let s = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_string);
        let probe_owned;
        let probe = match row.get("ffprobe") {
            Some(Value::String(text)) => {
                probe_owned = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
                &probe_owned
            }
            Some(value) => value,
            None => &Value::Null,
        };
        let audio_streams = probe
            .get("streams")
            .and_then(Value::as_array)
            .map(|streams| {
                streams
                    .iter()
                    .filter(|st| st.get("codec_type").and_then(Value::as_str) == Some("audio"))
                    .map(|st| AudioStreamInfo {
                        index: st.get("index").and_then(Value::as_u64),
                        codec: st
                            .get("codec_name")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        sample_rate: st.get("sample_rate").and_then(num),
                        channels: st.get("channels").and_then(num),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            id: s("id")?,
            relative_path: s("relative_path").unwrap_or_default(),
            source_path: s("source_path").unwrap_or_default(),
            size_bytes: row.get("size_bytes").and_then(Value::as_u64),
            modified_unix: row.get("modified_unix").and_then(Value::as_u64),
            sha256: s("sha256"),
            audio_codec: s("audio_codec"),
            duration_seconds: row.get("duration_seconds").and_then(Value::as_f64),
            stale: s("index_status").as_deref() == Some("stale"),
            probed: row.get("ffprobe_ok").and_then(Value::as_bool) == Some(true),
            audio_streams,
        })
    }

    /// Streams to measure: the indexed audio streams, or one unnamed
    /// stream when only `audio_codec` is known.
    fn measurable_streams(&self) -> Vec<AudioStreamInfo> {
        if !self.audio_streams.is_empty() {
            return self.audio_streams.clone();
        }
        if self.audio_codec.is_some() {
            return vec![AudioStreamInfo {
                index: None,
                codec: self.audio_codec.clone(),
                sample_rate: None,
                channels: None,
            }];
        }
        Vec::new()
    }
}

/// Reads `db/videos.jsonl` (missing → empty).
pub fn read_index(case_dir: &Path) -> Result<Vec<IndexRow>, String> {
    let path = case_dir.join("db/videos.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
    };
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|row| IndexRow::from_json(&row))
        .collect())
}

/// True when `result` was measured on the bytes the record describes now:
/// size and mtime equal, and SHA-256 equal when both sides have one.
pub fn measurement_is_current(row: &IndexRow, result: &FileResult) -> bool {
    if row.size_bytes != result.size_bytes || row.modified_unix != result.modified_unix {
        return false;
    }
    match (&row.sha256, &result.sha256_indexed) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => true,
    }
}

fn run_stream(
    ffmpeg: &str,
    path: &str,
    ordinal: usize,
    info: &AudioStreamInfo,
    timeout_secs: u64,
) -> StreamResult {
    let mut out = StreamResult {
        index: info.index,
        codec: info.codec.clone(),
        n_samples: None,
        measured_seconds: None,
        max_volume_db: None,
        mean_volume_db: None,
        max_is_neg_inf: false,
        verdict: Verdict::Error,
        error: None,
    };
    let mut command = Command::new(ffmpeg);
    command.args(stream_args(path, &ordinal.to_string()));
    let output = match crate::util::run_with_timeout(&mut command, Some(timeout_secs)) {
        Ok(output) => output,
        Err(err) => {
            out.error = Some(err);
            return out;
        }
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        let tail = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        out.error = Some(format!("ffmpeg decode failed: {tail}"));
        return out;
    }
    match parse_volumedetect(&stderr) {
        Ok(stats) => {
            out.n_samples = Some(stats.n_samples);
            if let (Some(rate), Some(channels)) = (info.sample_rate, info.channels)
                && rate > 0
                && channels > 0
            {
                out.measured_seconds = Some(stats.n_samples as f64 / (rate * channels) as f64);
            }
            out.max_is_neg_inf = stats.max_db == f64::NEG_INFINITY;
            out.max_volume_db = finite(stats.max_db);
            out.mean_volume_db = finite(stats.mean_db);
            out.verdict = stream_verdict(&stats);
        }
        Err(err) => out.error = Some(err),
    }
    out
}

/// Measures every audio stream (up to [`MAX_STREAMS`]) of one record.
pub fn measure_row(ffmpeg: &str, row: &IndexRow) -> FileResult {
    let streams = row.measurable_streams();
    let timeout = row
        .duration_seconds
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(|d| (d * 2.0).ceil() as u64)
        .unwrap_or(0)
        .max(60);
    let results: Vec<StreamResult> = streams
        .iter()
        .take(MAX_STREAMS)
        .enumerate()
        .map(|(k, info)| run_stream(ffmpeg, &row.source_path, k, info, timeout))
        .collect();
    let mut verdict = record_verdict(&results);
    let mut error = results.iter().find_map(|s| s.error.clone());
    if streams.len() > MAX_STREAMS && verdict != Verdict::Sound {
        // An unmeasured stream could carry sound: never claim silence.
        verdict = Verdict::Error;
        error = Some(format!(
            "오디오 트랙 {}개 중 {MAX_STREAMS}개만 측정했습니다",
            streams.len()
        ));
    }
    FileResult {
        selector: row.id.clone(),
        source_path: row.source_path.clone(),
        size_bytes: row.size_bytes,
        modified_unix: row.modified_unix,
        sha256_indexed: row.sha256.clone(),
        result: verdict,
        streams: results,
        error: if verdict == Verdict::Sound {
            None
        } else {
            error
        },
    }
}

/// One chained log entry for a chunk of results.
pub fn log_entry_json(measured_unix: u64, ffmpeg_version: &str, results: &[FileResult]) -> Value {
    json!({
        "schema_version": 1,
        "event": "audio-level",
        "measured_unix": measured_unix,
        "method": METHOD,
        "threshold_silent_max_dbfs": SILENT_MAX_DBFS,
        "ffmpeg_version": ffmpeg_version,
        "command_template": command_template(),
        "results": results.iter().map(FileResult::to_json).collect::<Vec<_>>(),
    })
}

/// `selector -> (result, measured_unix, method, threshold_silent_max_dbfs)`.
pub type LoggedResults = BTreeMap<String, (FileResult, u64, String, f64)>;

/// Latest logged result per selector with its entry's `measured_unix`,
/// method and threshold. Later lines (and later results within a line)
/// win. A missing log is empty.
pub fn latest_logged_results(case_dir: &Path) -> Result<LoggedResults, String> {
    let path = case_dir.join(LOG_REL);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
    };
    let mut out = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("event").and_then(Value::as_str) != Some("audio-level") {
            continue;
        }
        let measured = entry
            .get("measured_unix")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let method = entry
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let threshold = entry
            .get("threshold_silent_max_dbfs")
            .and_then(Value::as_f64)
            .unwrap_or(f64::NAN);
        for item in entry
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(result) = FileResult::from_json(item) {
                out.insert(
                    result.selector.clone(),
                    (result, measured, method.clone(), threshold),
                );
            }
        }
    }
    Ok(out)
}

/// The per-record state handed to the viewer / report (§2.4).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioLevelState {
    /// `sound` | `silent` | `error` | `stale`.
    pub r: &'static str,
    /// Loudest stream's peak (dBFS); None for `-inf`, error, or stale.
    pub max: Option<f64>,
    pub mean: Option<f64>,
    /// Longest measured stream in seconds.
    pub sec: Option<f64>,
    /// `measured_unix` of the entry.
    pub t: u64,
    /// The record-level error text (error / stale reason). Not part of the
    /// compact JSON map.
    pub error: Option<String>,
    /// First measured stream's codec. Not part of the compact JSON map.
    pub codec: Option<String>,
    /// True when the loudest stream's peak was `-inf` (digital zero).
    pub max_is_neg_inf: bool,
}

impl AudioLevelState {
    pub fn to_json(&self) -> Value {
        json!({"r": self.r, "max": self.max, "mean": self.mean, "sec": self.sec, "t": self.t})
    }
}

fn state_from(result: &FileResult, measured: u64) -> Option<AudioLevelState> {
    let r = match result.result {
        Verdict::Sound => "sound",
        Verdict::Silent => "silent",
        Verdict::Error => "error",
        // ffprobe already says "no track"; nothing measured to add.
        Verdict::NoAudioStream => return None,
    };
    let loudest = result
        .streams
        .iter()
        .filter(|s| s.n_samples.is_some())
        .max_by(|a, b| {
            let key = |s: &StreamResult| {
                if s.max_is_neg_inf {
                    f64::NEG_INFINITY
                } else {
                    s.max_volume_db.unwrap_or(f64::NEG_INFINITY)
                }
            };
            key(a).total_cmp(&key(b))
        });
    let (max, mean, neg_inf) = match (r, loudest) {
        ("error", _) | (_, None) => (None, None, false),
        (_, Some(s)) => (s.max_volume_db, s.mean_volume_db, s.max_is_neg_inf),
    };
    Some(AudioLevelState {
        r,
        max,
        mean,
        sec: result
            .streams
            .iter()
            .filter_map(|s| s.measured_seconds)
            .fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| a.max(v)))
            }),
        t: measured,
        error: result.error.clone(),
        codec: result.streams.first().and_then(|s| s.codec.clone()),
        max_is_neg_inf: neg_inf,
    })
}

/// Applies the latest measurement per id to the current index: a
/// measurement whose size/mtime/hash binding no longer matches becomes
/// `stale` ("측정 후 파일 변경 — 미확인"). Ids absent from the index are
/// dropped. This is the single invalidation rule the server and report use.
pub fn audio_levels_for_case(case_dir: &Path) -> Result<BTreeMap<String, AudioLevelState>, String> {
    let rows = read_index(case_dir)?;
    let logged = latest_logged_results(case_dir)?;
    Ok(apply_levels(&rows, &logged))
}

/// Pure core of [`audio_levels_for_case`].
pub fn apply_levels(
    rows: &[IndexRow],
    logged: &LoggedResults,
) -> BTreeMap<String, AudioLevelState> {
    let mut out = BTreeMap::new();
    for row in rows {
        let Some((result, measured, _, _)) = logged.get(&row.id) else {
            continue;
        };
        if !measurement_is_current(row, result) {
            out.insert(
                row.id.clone(),
                AudioLevelState {
                    r: "stale",
                    max: None,
                    mean: None,
                    sec: None,
                    t: *measured,
                    error: Some("측정 후 파일 변경 — 미확인".to_string()),
                    codec: None,
                    max_is_neg_inf: false,
                },
            );
            continue;
        }
        if let Some(state) = state_from(result, *measured) {
            out.insert(row.id.clone(), state);
        }
    }
    out
}

/// The compact `audioLevels` map: `{id: {r, max, mean, sec, t}}`.
pub fn audio_levels_json(levels: &BTreeMap<String, AudioLevelState>) -> Value {
    Value::Object(
        levels
            .iter()
            .map(|(id, state)| (id.clone(), state.to_json()))
            .collect(),
    )
}

#[derive(Debug, Clone)]
pub struct MeasureOptions {
    /// Limit to these selectors (ids or relative/source paths).
    pub selectors: Option<Vec<String>>,
    /// Re-measure files that already have a valid measurement.
    pub force: bool,
    pub ffmpeg: String,
    pub resume: ResumeMode,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        Self {
            selectors: None,
            force: false,
            ffmpeg: "ffmpeg".to_string(),
            resume: ResumeMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeasureSummary {
    pub targets: usize,
    pub measured: usize,
    pub resumed: usize,
    pub skipped_valid: usize,
    pub skipped_no_audio: usize,
    pub skipped_unprobed: usize,
    pub not_found: Vec<String>,
    pub sound: usize,
    pub silent: usize,
    pub error: usize,
    pub log_entries: usize,
}

impl MeasureSummary {
    pub fn to_json(&self) -> Value {
        json!({
            "targets": self.targets,
            "measured": self.measured,
            "resumed": self.resumed,
            "skipped_valid": self.skipped_valid,
            "skipped_no_audio": self.skipped_no_audio,
            "skipped_unprobed": self.skipped_unprobed,
            "not_found": self.not_found,
            "sound": self.sound,
            "silent": self.silent,
            "error": self.error,
            "log_entries": self.log_entries,
        })
    }
}

/// Splits `results` into log chunks of [`LOG_CHUNK`].
pub fn chunk_results(results: &[FileResult]) -> Vec<&[FileResult]> {
    results.chunks(LOG_CHUNK).collect()
}

/// Measures the case's indexed videos that have an audio track (all of
/// them, or `options.selectors`), skipping those with a current valid
/// measurement unless `force`. `progress(done, total)` is called as files
/// finish. Logs one chained entry per [`LOG_CHUNK`] files; crash-resume
/// state lives in `db/audio-level-progress.jsonl`.
pub fn measure_case(
    case_dir: &Path,
    options: &MeasureOptions,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<MeasureSummary, String> {
    let rows = read_index(case_dir)?;
    let logged = latest_logged_results(case_dir)?;
    let mut summary = MeasureSummary::default();

    let selected: Vec<&IndexRow> = match &options.selectors {
        None => rows.iter().filter(|r| !r.stale).collect(),
        Some(selectors) => {
            let mut picked = Vec::new();
            let mut seen = HashSet::new();
            for selector in selectors {
                match rows.iter().find(|r| {
                    !r.stale
                        && (r.id == *selector
                            || r.relative_path == *selector
                            || r.source_path == *selector)
                }) {
                    Some(row) => {
                        if seen.insert(row.id.clone()) {
                            picked.push(row);
                        }
                    }
                    None => summary.not_found.push(selector.clone()),
                }
            }
            picked
        }
    };
    let mut targets: Vec<IndexRow> = Vec::new();
    for row in selected {
        if row.audio_codec.is_none() && row.audio_streams.is_empty() {
            if row.probed {
                summary.skipped_no_audio += 1;
            } else {
                summary.skipped_unprobed += 1;
            }
            continue;
        }
        if !options.force
            && let Some((result, _, method, threshold)) = logged.get(&row.id)
            && method == METHOD
            && *threshold == SILENT_MAX_DBFS
            && result.result != Verdict::Error
            && measurement_is_current(row, result)
        {
            summary.skipped_valid += 1;
            continue;
        }
        targets.push(row.clone());
    }
    summary.targets = targets.len();
    if targets.is_empty() {
        return Ok(summary);
    }
    let ffmpeg = resolve_tool_binary(&options.ffmpeg, &["ffmpeg"])
        .map_err(|err| format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)"))?;
    let ffmpeg_version = command_version(&options.ffmpeg, &["ffmpeg"], "-version");

    let fingerprint =
        checkpoint::fingerprint(&["measure-audio", METHOD, &format!("{SILENT_MAX_DBFS:.1}")]);
    let mut checkpoint = RunCheckpoint::begin(
        &case_dir.join(CHECKPOINT_REL),
        "measure-audio",
        &fingerprint,
        options.resume,
    )?;
    // Replay computed-but-unlogged results that still describe current bytes.
    let mut logged_in_ckpt: HashSet<String> = HashSet::new();
    let mut replay: BTreeMap<String, FileResult> = BTreeMap::new();
    for line in checkpoint.data_lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(result) = value.get("computed").and_then(FileResult::from_json) {
            replay.insert(result.selector.clone(), result);
        } else if let Some(ids) = value.get("logged").and_then(Value::as_array) {
            logged_in_ckpt.extend(ids.iter().filter_map(Value::as_str).map(str::to_string));
        }
    }
    let tally = |summary: &mut MeasureSummary, verdict: Verdict| match verdict {
        Verdict::Sound => summary.sound += 1,
        Verdict::Silent => summary.silent += 1,
        _ => summary.error += 1,
    };
    let mut buffer: Vec<FileResult> = Vec::new();
    let mut pending: Vec<IndexRow> = Vec::new();
    for row in targets.iter() {
        match replay.get(&row.id) {
            Some(result)
                if !logged_in_ckpt.contains(&row.id) && measurement_is_current(row, result) =>
            {
                tally(&mut summary, result.result);
                buffer.push(result.clone());
                summary.resumed += 1;
            }
            _ => pending.push(row.clone()),
        }
    }
    let total = targets.len();
    let mut done = summary.resumed;
    progress(done, total);

    let flush = |buffer: &mut Vec<FileResult>,
                 checkpoint: &mut RunCheckpoint,
                 summary: &mut MeasureSummary|
     -> Result<(), String> {
        if buffer.is_empty() {
            return Ok(());
        }
        let entry = log_entry_json(now_unix()?, &ffmpeg_version, buffer);
        audit::append_chained_jsonl(&case_dir.join(LOG_REL), &entry.to_string())?;
        summary.log_entries += 1;
        let ids: Vec<&str> = buffer.iter().map(|r| r.selector.as_str()).collect();
        checkpoint.append_line(&json!({ "logged": ids }).to_string())?;
        buffer.clear();
        Ok(())
    };
    while buffer.len() >= LOG_CHUNK {
        let mut head: Vec<FileResult> = buffer.drain(..LOG_CHUNK).collect();
        flush(&mut head, &mut checkpoint, &mut summary)?;
    }

    let (sender, receiver) = std::sync::mpsc::channel::<FileResult>();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let pending_ref = &pending;
    let ffmpeg_ref = &ffmpeg;
    let mut outcome: Result<(), String> = Ok(());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(pending_ref.len()) {
            let sender = sender.clone();
            let next = &next;
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some(row) = pending_ref.get(i) else {
                        break;
                    };
                    if sender.send(measure_row(ffmpeg_ref, row)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        for result in receiver {
            if outcome.is_err() {
                // Stop feeding new work; workers drain naturally.
                next.store(usize::MAX / 2, std::sync::atomic::Ordering::SeqCst);
                continue;
            }
            let line = json!({ "computed": result.to_json() }).to_string();
            if let Err(err) = checkpoint.append_line(&line) {
                outcome = Err(err);
                continue;
            }
            summary.measured += 1;
            done += 1;
            progress(done, total);
            tally(&mut summary, result.result);
            buffer.push(result);
            if buffer.len() >= LOG_CHUNK
                && let Err(err) = flush(&mut buffer, &mut checkpoint, &mut summary)
            {
                outcome = Err(err);
            }
        }
    });
    outcome?;
    flush(&mut buffer, &mut checkpoint, &mut summary)?;
    checkpoint.finish()?;
    Ok(summary)
}

/// Absolute path of the lane for a case.
pub fn log_path(case_dir: &Path) -> PathBuf {
    case_dir.join(LOG_REL)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "[Parsed_volumedetect_0 @ 000001fb76375a40] n_samples: 96256\n\
[Parsed_volumedetect_0 @ 000001fb76375a40] mean_volume: -21.1 dB\n\
[Parsed_volumedetect_0 @ 000001fb76375a40] max_volume: -16.1 dB\n\
[Parsed_volumedetect_0 @ 000001fb76375a40] histogram_16db: 31\n";

    #[test]
    fn parses_numbers_inf_and_empty_first_block() {
        let stats = parse_volumedetect(ONE).unwrap();
        assert_eq!(stats.n_samples, 96256);
        assert_eq!(stats.max_db, -16.1);
        assert!((stats.mean_db - -21.1).abs() < 1e-9);

        // ffmpeg 7.1 prints an empty first instance for anullsrc tracks.
        let silent = "[Parsed_volumedetect_0 @ 0000013e0f9da9c0] n_samples: 0\n\
[Parsed_volumedetect_0 @ 0000013e0fa6fb80] n_samples: 96256\n\
[Parsed_volumedetect_0 @ 0000013e0fa6fb80] mean_volume: -91.0 dB\n\
[Parsed_volumedetect_0 @ 0000013e0fa6fb80] max_volume: -91.0 dB\n";
        let stats = parse_volumedetect(silent).unwrap();
        assert_eq!(stats.max_db, -91.0);
        assert_eq!(stream_verdict(&stats), Verdict::Silent);

        let inf = "[Parsed_volumedetect_0 @ 0] n_samples: 10\n\
[Parsed_volumedetect_0 @ 0] mean_volume: -inf dB\n\
[Parsed_volumedetect_0 @ 0] max_volume: -inf dB\n";
        let stats = parse_volumedetect(inf).unwrap();
        assert_eq!(stats.max_db, f64::NEG_INFINITY);
        assert_eq!(stats.mean_db, f64::NEG_INFINITY);
        assert_eq!(stream_verdict(&stats), Verdict::Silent);
    }

    #[test]
    fn zero_samples_and_missing_lines_are_errors_not_silence() {
        assert!(parse_volumedetect("[Parsed_volumedetect_0 @ 0] n_samples: 0\n").is_err());
        assert!(parse_volumedetect("some unrelated stderr\n").is_err());
        let missing_max = "[Parsed_volumedetect_0 @ 0] n_samples: 5\n\
[Parsed_volumedetect_0 @ 0] mean_volume: -20.0 dB\n";
        assert!(parse_volumedetect(missing_max).is_err());
    }

    #[test]
    fn multiple_blocks_combine_peak_and_power_mean() {
        let text = "[Parsed_volumedetect_0 @ a] n_samples: 100\n\
[Parsed_volumedetect_0 @ a] mean_volume: -20.0 dB\n\
[Parsed_volumedetect_0 @ a] max_volume: -10.0 dB\n\
[Parsed_volumedetect_1 @ b] n_samples: 100\n\
[Parsed_volumedetect_1 @ b] mean_volume: -inf dB\n\
[Parsed_volumedetect_1 @ b] max_volume: -inf dB\n";
        let stats = parse_volumedetect(text).unwrap();
        assert_eq!(stats.n_samples, 200);
        assert_eq!(stats.max_db, -10.0);
        assert!((stats.mean_db - (-20.0 - 10.0 * 2f64.log10())).abs() < 1e-9);
    }

    #[test]
    fn threshold_boundary() {
        let at = VolumeStats {
            n_samples: 1,
            max_db: -70.0,
            mean_db: -80.0,
        };
        assert_eq!(stream_verdict(&at), Verdict::Silent);
        let above = VolumeStats {
            max_db: -69.9,
            ..at
        };
        assert_eq!(stream_verdict(&above), Verdict::Sound);
    }

    fn stream(verdict: Verdict, max: Option<f64>) -> StreamResult {
        StreamResult {
            index: Some(1),
            codec: Some("aac".into()),
            n_samples: (verdict != Verdict::Error).then_some(48000),
            measured_seconds: Some(1.0),
            max_volume_db: max,
            mean_volume_db: max.map(|m| m - 20.0),
            max_is_neg_inf: false,
            verdict,
            error: (verdict == Verdict::Error).then(|| "decode failed".to_string()),
        }
    }

    #[test]
    fn record_level_verdicts() {
        use Verdict::*;
        assert_eq!(
            record_verdict(&[stream(Silent, Some(-90.0)), stream(Sound, Some(-12.0))]),
            Sound
        );
        assert_eq!(
            record_verdict(&[stream(Silent, Some(-90.0)), stream(Silent, Some(-91.0))]),
            Silent
        );
        assert_eq!(
            record_verdict(&[stream(Silent, Some(-90.0)), stream(Error, None)]),
            Error
        );
        assert_eq!(
            record_verdict(&[stream(Error, None), stream(Sound, Some(-1.0))]),
            Sound
        );
        assert_eq!(record_verdict(&[]), NoAudioStream);
    }

    fn row(id: &str) -> IndexRow {
        IndexRow {
            id: id.into(),
            relative_path: format!("{id}.mp4"),
            source_path: format!("/e/{id}.mp4"),
            size_bytes: Some(100),
            modified_unix: Some(5),
            sha256: Some("aa".into()),
            audio_codec: Some("aac".into()),
            duration_seconds: Some(2.0),
            stale: false,
            probed: true,
            audio_streams: Vec::new(),
        }
    }

    fn result_for(row: &IndexRow, verdict: Verdict, max: f64) -> FileResult {
        FileResult {
            selector: row.id.clone(),
            source_path: row.source_path.clone(),
            size_bytes: row.size_bytes,
            modified_unix: row.modified_unix,
            sha256_indexed: row.sha256.clone(),
            result: verdict,
            streams: vec![stream(verdict, Some(max))],
            error: None,
        }
    }

    #[test]
    fn invalidation_binds_size_mtime_and_hash() {
        let base = row("vid_1");
        let result = result_for(&base, Verdict::Sound, -12.3);
        assert!(measurement_is_current(&base, &result));
        let mut sized = base.clone();
        sized.size_bytes = Some(101);
        assert!(!measurement_is_current(&sized, &result));
        let mut touched = base.clone();
        touched.modified_unix = Some(6);
        assert!(!measurement_is_current(&touched, &result));
        let mut rehashed = base.clone();
        rehashed.sha256 = Some("bb".into());
        assert!(!measurement_is_current(&rehashed, &result));
        let mut unhashed = base.clone();
        unhashed.sha256 = None;
        assert!(
            measurement_is_current(&unhashed, &result),
            "fast mode: size+mtime only"
        );

        let mut logged = BTreeMap::new();
        logged.insert(
            "vid_1".to_string(),
            (result.clone(), 10, METHOD.to_string(), SILENT_MAX_DBFS),
        );
        let levels = apply_levels(&[sized], &logged);
        assert_eq!(levels["vid_1"].r, "stale");
        let levels = apply_levels(&[base], &logged);
        assert_eq!(levels["vid_1"].r, "sound");
        assert_eq!(levels["vid_1"].max, Some(-12.3));
        assert_eq!(
            audio_levels_json(&levels).to_string(),
            r#"{"vid_1":{"r":"sound","max":-12.3,"mean":-32.3,"sec":1.0,"t":10}}"#
        );
    }

    #[test]
    fn latest_entry_wins_and_log_lines_chain() {
        let dir = std::env::temp_dir().join(format!(
            "ft-audio-level-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(dir.join("db")).unwrap();
        let r = row("vid_1");
        let old = result_for(&r, Verdict::Silent, -91.0);
        let new = result_for(&r, Verdict::Sound, -20.0);
        let log = dir.join(LOG_REL);
        audit::append_chained_jsonl_keyed(
            &log,
            &log_entry_json(1, "ffmpeg x", &[old]).to_string(),
            None,
        )
        .unwrap();
        audit::append_chained_jsonl_keyed(
            &log,
            &log_entry_json(2, "ffmpeg x", &[new]).to_string(),
            None,
        )
        .unwrap();
        let mut row_json = json!({
            "id": "vid_1", "relative_path": "vid_1.mp4", "source_path": "/e/vid_1.mp4",
            "size_bytes": 100, "modified_unix": 5, "sha256": "aa", "audio_codec": "aac",
            "ffprobe": {"streams": [{"index": 1, "codec_type": "audio", "codec_name": "aac",
                                      "sample_rate": "48000", "channels": 1}]}
        });
        std::fs::write(dir.join("db/videos.jsonl"), format!("{row_json}\n")).unwrap();
        let levels = audio_levels_for_case(&dir).unwrap();
        assert_eq!(levels["vid_1"].r, "sound");
        assert_eq!(levels["vid_1"].t, 2);
        let verified = audit::verify_chained_jsonl(&log).unwrap();
        assert_eq!(verified.entries, 2);
        // Parsed stream list feeds sample-rate based duration.
        row_json["ffprobe"]["streams"][0]["channels"] = json!(2);
        let parsed = IndexRow::from_json(&row_json).unwrap();
        assert_eq!(parsed.audio_streams[0].channels, Some(2));
        assert_eq!(parsed.audio_streams[0].sample_rate, Some(48000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn results_chunk_by_200() {
        let r = row("vid_1");
        let results: Vec<FileResult> = (0..450)
            .map(|_| result_for(&r, Verdict::Sound, -1.0))
            .collect();
        let sizes: Vec<usize> = chunk_results(&results).iter().map(|c| c.len()).collect();
        assert_eq!(sizes, vec![200, 200, 50]);
        assert_eq!(LOG_CHUNK, 200);
    }

    #[test]
    fn file_result_round_trips_and_template_shape() {
        let r = row("vid_1");
        let mut result = result_for(&r, Verdict::Silent, -91.0);
        result.streams[0].max_is_neg_inf = true;
        result.streams[0].max_volume_db = None;
        let back = FileResult::from_json(&result.to_json()).unwrap();
        assert_eq!(back, result);
        let template = command_template();
        assert!(template.windows(2).any(|w| w == ["-af", "volumedetect"]));
        assert_eq!(template.last().map(String::as_str), Some("-"));
    }
}
