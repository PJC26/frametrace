//! Measured sound level per indexed video (ffmpeg `volumedetect`).
//!
//! Each audio stream (at most [`MAX_STREAMS`]) is decoded END TO END and
//! its peak / RMS level in dBFS recorded. A stream whose peak is at or
//! below [`SILENT_MAX_DBFS`] is a silent track; anything louder has sound.
//! A decode failure, a decoder complaint on stderr, a decode that covered
//! less than the probed track duration (truncated / corrupt file), a
//! timeout, `n_samples: 0`, or a file whose size/mtime no longer match the
//! index is an ERROR (unknown) — never "silent" or "sound". Nothing here
//! claims speech or conversation.
//!
//! Results go to the chained lane `evidence/logs/audio-level-log.jsonl`,
//! one entry per [`LOG_CHUNK`] files (each append fsyncs; per-file entries
//! would mean one fsync per file). The case index is not touched.
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
/// `measure-audio` exit code when the run completed but some files could
/// not be measured (B15). 0 means no errors at all.
pub const EXIT_SOME_ERRORS: i32 = 4;
/// Prefix of the last stderr line `measure-audio` prints with exit 4,
/// followed by the error count (`소리 측정 오류 3건 (측정 대상 10건)`).
pub const ERRORS_LINE_PREFIX: &str = "소리 측정 오류 ";

/// The error count from a failed `measure-audio` step's message when it
/// exited with [`EXIT_SOME_ERRORS`] (a completed run with errors), else
/// None (a real failure).
pub fn partial_error_count(step_error: &str) -> Option<usize> {
    if !step_error.contains(&format!("(exit Some({EXIT_SOME_ERRORS}))")) {
        return None;
    }
    let rest = &step_error[step_error.rfind(ERRORS_LINE_PREFIX)? + ERRORS_LINE_PREFIX.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// One Korean line summarising a `measure-audio` stdout summary JSON
/// (`소리 측정 완료: 소리 있음 2 · 무음 1 · 오류 0 · …`), None when the
/// output carries no summary.
pub fn summary_log_line(output: &str) -> Option<String> {
    let value: Value = output
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())?;
    let n = |k: &str| value.get(k).and_then(Value::as_u64).unwrap_or(0);
    value.get("targets")?;
    let errors = n("error");
    let head = if errors > 0 {
        format!("소리 측정 완료(오류 {errors}건)")
    } else {
        "소리 측정 완료".to_string()
    };
    Some(format!(
        "{head}: 소리 있음 {} · 무음 {} · 오류 {errors} · 이미 측정됨 {} · 트랙 없음 {} · 미프로브 {}",
        n("sound"),
        n("silent"),
        n("skipped_valid"),
        n("skipped_no_audio"),
        n("skipped_unprobed"),
    ))
}

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
    /// When this result was measured, if it differs from its log entry's
    /// `measured_unix` (results replayed from a crash checkpoint keep their
    /// original time). Omitted from JSON when None.
    pub measured_unix: Option<u64>,
    /// D2: the result was logged from a crash checkpoint without being
    /// measured again (entries flagged `replayed_from_checkpoint`). Such a
    /// result is never shown as sound/silent — it is re-measured instead.
    /// Set by the log reader, never serialized into a result.
    pub replayed_unverified: bool,
    /// R5-P8: the result was read from a measurement lane whose chain does
    /// not verify — the reason. Such a result is never shown as a level
    /// (the record reads `unverified`) and is measured again. Set by the
    /// log reader, never serialized.
    pub chain_unverified: Option<String>,
    /// R8 P1 / I8: the result's line was not authenticated by an audit key
    /// — this PC holds none (P1), or the line predates the lane's key
    /// introduction — so the level is shown with "자기 일관성만 확인(인증
    /// 아님)". Set by the log reader, never serialized.
    pub self_consistent_only: bool,
    /// R8 P2: the result's line is signed under this key id, which this PC
    /// does not hold — not authenticable here (`chain_unverified` says so;
    /// check it on the PC holding the key). Set by the log reader.
    pub other_pc_key: Option<String>,
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some((value * 100.0).round() / 100.0)
}

impl FileResult {
    pub fn to_json(&self) -> Value {
        let mut value = json!({
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
        });
        if let (Some(at), Some(map)) = (self.measured_unix, value.as_object_mut()) {
            map.insert("measured_unix".to_string(), json!(at));
        }
        value
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
            measured_unix: value.get("measured_unix").and_then(Value::as_u64),
            replayed_unverified: false,
            chain_unverified: None,
            self_consistent_only: false,
            other_pc_key: None,
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
    [
        "-nostdin",
        "-hide_banner",
        "-xerror",
        "-v",
        "info",
        "-i",
        src,
        "-map",
    ]
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
    /// The stream's own probed duration (seconds), when ffprobe gave one.
    pub duration_seconds: Option<f64>,
}

fn stream_infos(probe: &Value) -> Vec<AudioStreamInfo> {
    probe
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
                    duration_seconds: st
                        .get("duration")
                        .and_then(|d| {
                            d.as_f64()
                                .or_else(|| d.as_str().and_then(|t| t.trim().parse().ok()))
                        })
                        .filter(|d: &f64| d.is_finite() && *d > 0.0),
                })
                .collect()
        })
        .unwrap_or_default()
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
    /// R5-P3: the lossless OS path when the record carries
    /// `source_path_raw_hex`; `None` = `source_path` names the file.
    pub raw_path: Option<PathBuf>,
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
        let audio_streams = stream_infos(probe);
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
            raw_path: s("source_path_raw_hex")
                .as_deref()
                .and_then(crate::model::path_from_raw_hex),
        })
    }

    /// R5-P3: the file this record names, losslessly — what is stat'ed,
    /// probed and decoded (never the lossy display string).
    pub fn os_path(&self) -> PathBuf {
        self.raw_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(&self.source_path))
    }
}

/// B12: a row that only knows `audio_codec` (no indexed stream list) gets
/// its audio streams from a live ffprobe, so the number of tracks — and
/// each track's sample rate, channels and duration — is known before any
/// "silent" claim. `Err` when the probe fails.
pub fn probe_audio_streams(ffprobe: &str, path: &Path) -> Result<Vec<AudioStreamInfo>, String> {
    let binary = resolve_tool_binary(ffprobe, &["ffprobe"])?;
    let mut command = Command::new(binary);
    command
        .args(["-v", "error", "-print_format", "json", "-show_streams"])
        .args(["-select_streams", "a"])
        .arg(path);
    let output =
        crate::util::run_with_timeout(&mut command, Some(crate::util::PROBE_TIMEOUT_SECS))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("invalid ffprobe JSON output: {err}"))?;
    Ok(stream_infos(&value))
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

/// B1: a decoder complaint in ffmpeg's stderr. Indented lines are stream
/// / metadata descriptions (user-controlled tag text), so only top-level
/// and `[component @ …]` lines count; the volumedetect report never does.
pub fn decode_error_in(stderr: &str) -> Option<String> {
    const NEEDLES: [&str; 7] = [
        "error",
        "invalid data",
        "corrupt",
        "partial file",
        "truncat",
        "concealing",
        "missing",
    ];
    stderr
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter(|line| !line.contains("volumedetect"))
        .find(|line| {
            let lower = line.to_ascii_lowercase();
            NEEDLES.iter().any(|needle| lower.contains(needle))
        })
        .map(|line| line.trim().chars().take(240).collect())
}

/// B1: the decode covered the whole track. `expected` is the stream's own
/// probed duration (`stream_level`) or, failing that, the container's.
/// A stream duration is exact, so more than 1 s missing — or more than 5%
/// once over 0.25 s — is a truncated / partly undecodable track. A
/// container duration may legitimately exceed the audio track, so it gets
/// a wider margin (2 s or 5%).
pub fn coverage_error(
    measured: Option<f64>,
    expected: Option<f64>,
    stream_level: bool,
) -> Option<String> {
    let Some(expected) = expected.filter(|e| e.is_finite() && *e > 0.0) else {
        return Some("길이를 알 수 없어 트랙 전체를 측정했는지 확인하지 못했습니다".to_string());
    };
    let Some(measured) = measured else {
        return Some("샘플레이트·채널 수를 몰라 측정 길이를 확인하지 못했습니다".to_string());
    };
    let short = expected - measured;
    let truncated = if stream_level {
        short > 1.0 || (short > 0.25 && measured < expected * 0.95)
    } else {
        short > (expected * 0.05).max(2.0)
    };
    truncated.then(|| {
        format!(
            "디코드된 길이 {measured:.2}초가 트랙 길이 {expected:.2}초보다 짧습니다 — 잘리거나 손상된 트랙"
        )
    })
}

fn run_stream(
    ffmpeg: &str,
    path: &Path,
    ordinal: usize,
    info: &AudioStreamInfo,
    container_duration: Option<f64>,
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
    // The template with the real path as its own OS argument (R5-P3: a
    // non-Unicode name is passed exactly, never as a lossy string).
    let mut command = Command::new(ffmpeg);
    let template = stream_args("<src>", &ordinal.to_string());
    for arg in &template {
        if arg == "<src>" {
            command.arg(path);
        } else {
            command.arg(arg);
        }
    }
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
            // Numbers are kept for the record, but a decode complaint or a
            // short decode means the level describes part of the track
            // only: never "sound" or "silent" then.
            let problem = decode_error_in(&stderr)
                .map(|line| format!("디코드 오류: {line}"))
                .or_else(|| match info.duration_seconds {
                    Some(stream) => coverage_error(out.measured_seconds, Some(stream), true),
                    None => coverage_error(out.measured_seconds, container_duration, false),
                });
            match problem {
                Some(err) => out.error = Some(err),
                None => out.verdict = stream_verdict(&stats),
            }
        }
        Err(err) => out.error = Some(err),
    }
    out
}

/// A file's size and full-resolution mtime at one moment (D17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub size: u64,
    /// Nanoseconds since the epoch (as fine as the filesystem keeps).
    pub modified_nanos: Option<u128>,
}

impl FileStamp {
    /// The index binding (size, whole-second mtime) this stamp matches.
    pub fn binding(&self) -> (u64, Option<u64>) {
        (
            self.size,
            self.modified_nanos
                .map(|nanos| (nanos / 1_000_000_000) as u64),
        )
    }
}

/// `path`'s size and nanosecond mtime right now; `None` when it cannot be
/// stat'ed.
pub fn stat_stamp(path: impl AsRef<Path>) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        size: meta.len(),
        modified_nanos: audit::modified_nanos(&meta),
    })
}

/// D17: whether a file changed between two stamps — size, or the mtime at
/// full resolution (a whole-second comparison misses a rewrite within the
/// same second). A file that could not be stat'ed either time is changed.
pub fn stamp_changed(before: Option<FileStamp>, after: Option<FileStamp>) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => before != after,
        _ => true,
    }
}

/// Error text for a file whose bytes no longer match the index binding.
pub const CHANGED_ERROR: &str =
    "changed: 파일이 색인과 다릅니다(측정 전후 크기·수정 시각 확인) — 다시 스캔하십시오";

/// Measures every audio stream (up to [`MAX_STREAMS`]) of one record.
/// B3: the file is re-stat'ed before and after the decode; when its size or
/// mtime differs from the index binding the result is an error
/// ([`CHANGED_ERROR`]), never a level for other bytes. B12: without an
/// indexed stream list the streams come from a live ffprobe first.
pub fn measure_row(ffmpeg: &str, ffprobe: &str, row: &IndexRow) -> FileResult {
    let mut result = FileResult {
        selector: row.id.clone(),
        source_path: row.source_path.clone(),
        size_bytes: row.size_bytes,
        modified_unix: row.modified_unix,
        sha256_indexed: row.sha256.clone(),
        result: Verdict::Error,
        streams: Vec::new(),
        error: None,
        measured_unix: None,
        replayed_unverified: false,
        chain_unverified: None,
        self_consistent_only: false,
        other_pc_key: None,
    };
    let bound = row.size_bytes.map(|size| (size, row.modified_unix));
    let os_path = row.os_path();
    let before = stat_stamp(&os_path);
    if bound.is_none() || before.as_ref().map(FileStamp::binding) != bound {
        result.error = Some(CHANGED_ERROR.to_string());
        return result;
    }
    let streams = if row.audio_streams.is_empty() {
        match probe_audio_streams(ffprobe, &os_path) {
            Ok(streams) if streams.is_empty() => {
                result.error = Some(
                    "색인에는 오디오 코덱이 있지만 ffprobe가 오디오 스트림을 찾지 못했습니다"
                        .to_string(),
                );
                return result;
            }
            Ok(streams) => streams,
            Err(err) => {
                result.error = Some(format!(
                    "오디오 트랙 수를 확인하지 못했습니다 (ffprobe): {err}"
                ));
                return result;
            }
        }
    } else {
        row.audio_streams.clone()
    };
    let container = row.duration_seconds.filter(|d| d.is_finite() && *d > 0.0);
    let timeout = container
        .or_else(|| {
            streams
                .iter()
                .filter_map(|s| s.duration_seconds)
                .reduce(f64::max)
        })
        .map(|d| (d * 2.0).ceil() as u64)
        .unwrap_or(0)
        .max(60);
    let results: Vec<StreamResult> = streams
        .iter()
        .take(MAX_STREAMS)
        .enumerate()
        .map(|(k, info)| run_stream(ffmpeg, &os_path, k, info, container, timeout))
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
    // D17: before/after compare the full-resolution mtime, so a rewrite
    // within the same second as the measurement is still caught.
    if stamp_changed(before, stat_stamp(&os_path)) {
        verdict = Verdict::Error;
        error = Some(CHANGED_ERROR.to_string());
    }
    result.result = verdict;
    result.streams = results;
    result.error = if verdict == Verdict::Sound {
        None
    } else {
        error
    };
    result
}

// One chained log entry for a chunk of results.
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

/// B11: a log entry for results computed by an earlier, interrupted run
/// and replayed from the checkpoint. Provenance stays the original one:
/// `measured_unix` is the earliest original measurement, every result
/// carries its own `measured_unix`, the ffmpeg version is the one that
/// measured them, and `logged_unix` says when they were finally logged.
pub fn replayed_log_entry_json(
    logged_unix: u64,
    ffmpeg_version: &str,
    results: &[FileResult],
) -> Value {
    let earliest = results
        .iter()
        .filter_map(|r| r.measured_unix)
        .min()
        .unwrap_or(logged_unix);
    let mut entry = log_entry_json(earliest, ffmpeg_version, results);
    if let Some(map) = entry.as_object_mut() {
        map.insert("replayed_from_checkpoint".to_string(), json!(true));
        map.insert("logged_unix".to_string(), json!(logged_unix));
    }
    entry
}

/// `selector -> (result, measured_unix, method, threshold_silent_max_dbfs)`.
pub type LoggedResults = BTreeMap<String, (FileResult, u64, String, f64)>;

/// Latest logged result per selector with its entry's `measured_unix`,
/// method and threshold, over every measurement lane in continuation order
/// (R5-P8: `audio-level-log.jsonl`, then `audio-level-log.1.jsonl`, …).
/// Later lanes (and later lines, and later results within a line) win. A
/// missing log is empty.
///
/// D1/R5-P8: each lane is chain-verified; a result read from a lane whose
/// chain does not verify carries `chain_unverified` (the reason) and is
/// never read out as sound/silence — a later verified lane's measurement of
/// the same record replaces it. Only a read failure is an error.
pub fn latest_logged_results(case_dir: &Path) -> Result<LoggedResults, String> {
    Ok(read_logged_results(case_dir)?.0)
}

/// One sound-measurement lane as the reader found it (R5-P8).
#[derive(Debug, Clone, PartialEq)]
pub struct LaneStatus {
    /// Case-relative path (`evidence/logs/audio-level-log[.<n>].jsonl`).
    pub rel: String,
    /// The verification, or why the chain does not verify.
    pub verification: Result<audit::AuditChainVerification, String>,
}

/// Every sound-measurement lane present in the case, in continuation
/// order (R5-P8). An unreadable log directory is an error.
pub fn lane_rels(case_dir: &Path) -> Result<Vec<String>, String> {
    let dir = case_dir.join("evidence/logs");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(format!("failed to list {}: {err}", dir.display())),
    };
    let mut lanes: Vec<(u32, String)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("failed to list {}: {err}", dir.display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // R10 P6: the canonical lane spelling (a differently-cased name
        // on Windows / macOS IS that lane); a link at a lane name is read
        // too — the binding check reports it NOT VERIFIED.
        let Some(rel) = audit::canonical_lane_rel(&format!("evidence/logs/{name}")) else {
            continue;
        };
        if let Some(number) = audit::audio_level_lane_number(&rel)
            && entry
                .file_type()
                .is_ok_and(|kind| kind.is_file() || kind.is_symlink())
        {
            lanes.push((number, rel));
        }
    }
    lanes.sort();
    lanes.dedup();
    Ok(lanes.into_iter().map(|(_, rel)| rel).collect())
}

/// Reads, verifies and parses every lane (R5-P8). Results of a lane whose
/// chain does not verify are flagged `chain_unverified`; a read failure is
/// an error.
fn read_logged_results(case_dir: &Path) -> Result<(LoggedResults, Vec<LaneStatus>), String> {
    let keys = crate::audit_key::verification_keys()?;
    read_logged_results_with_keys(case_dir, &keys)
}

/// [`read_logged_results`] with an explicit verification key set.
fn read_logged_results_with_keys(
    case_dir: &Path,
    keys: &[crate::audit_key::AuditKey],
) -> Result<(LoggedResults, Vec<LaneStatus>), String> {
    let lanes = read_lanes(case_dir)?;
    // R8 P0 (I1): whether a line must be authenticated is decided by THIS
    // PC holding audit keys (`audit::trust_policy(keys)`, recorded in each
    // lane's verification and applied by `line_trust`) — never by the
    // lanes carrying HMAC fields, which an attacker can strip from every
    // line (R7 used "any lane line carries an HMAC" here).
    // R6-I3: on a PC WITHOUT keys, a case measured under an audit key (a
    // lane line carries an HMAC) still does not take measurements from
    // lines written without one. This only ever adds a restriction (P1:
    // everything there is self-consistency only anyway).
    let keyed_case = is_keyed_case(&lanes);
    let statuses = lane_statuses(case_dir, &lanes, keys);
    let mut out: LoggedResults = BTreeMap::new();
    for ((rel, text), status) in lanes.iter().zip(&statuses) {
        let chain_error = status
            .verification
            .as_ref()
            .err()
            .map(|err| format!("{UNVERIFIED_ERROR}: {rel}: {err}"));
        let line_flag = |index: usize| -> LineFlag {
            if let Some(error) = &chain_error {
                return LineFlag::unverified(error.clone());
            }
            let Ok(verification) = status.verification.as_ref() else {
                return LineFlag::unverified(format!("{UNVERIFIED_ERROR}: {rel}"));
            };
            match verification.line_trust(index) {
                audit::LineTrust::Authenticated => LineFlag::default(),
                audit::LineTrust::PreKey => LineFlag {
                    self_consistent_only: true,
                    ..LineFlag::default()
                },
                audit::LineTrust::SelfConsistentOnly { key_id } => {
                    if keyed_case && key_id.is_none() {
                        LineFlag::unverified(format!(
                            "{UNVERIFIED_ERROR}: {rel}: {}번째 줄은 감사 키로 기록된 케이스에서 키 없이 기록되어 측정값으로 쓰지 않습니다",
                            index + 1
                        ))
                    } else {
                        LineFlag {
                            self_consistent_only: true,
                            ..LineFlag::default()
                        }
                    }
                }
                trust @ audit::LineTrust::OtherPc { .. } => {
                    let audit::LineTrust::OtherPc { key_id } = &trust else {
                        unreachable!()
                    };
                    LineFlag {
                        unverified: Some(format!(
                            "{UNVERIFIED_ERROR}: {rel}: {}번째 줄은 측정값으로 쓰지 않습니다 — {}",
                            index + 1,
                            trust.note()
                        )),
                        self_consistent_only: false,
                        other_pc_key: Some(key_id.clone()),
                    }
                }
                trust @ (audit::LineTrust::NotAuthenticated { .. }
                | audit::LineTrust::ChainBroken
                | audit::LineTrust::RewriteNotExcluded { .. }
                | audit::LineTrust::LocationUnverified) => LineFlag::unverified(format!(
                    "{UNVERIFIED_ERROR}: {rel}: {}번째 줄은 측정값으로 쓰지 않습니다 — {}",
                    index + 1,
                    trust.note()
                )),
            }
        };
        // R14 I5: P7 across lanes only from a lane that verifies (a proven
        // continuation); within a lane always (see the line parser).
        let proven = status.verification.is_ok();
        for (selector, entry) in parse_logged_results_lines(text, &line_flag) {
            insert_logged_result(&mut out, selector, entry, proven);
        }
    }
    Ok((out, statuses))
}

/// R8: what the reader attaches to every result of one lane line.
#[derive(Debug, Clone, Default)]
struct LineFlag {
    /// Why the line is not a measurement (`FileResult::chain_unverified`).
    unverified: Option<String>,
    /// P1 / I8: a level, but self-consistency only.
    self_consistent_only: bool,
    /// P2: the key the line is signed under, held by another PC.
    other_pc_key: Option<String>,
}

impl LineFlag {
    fn unverified(reason: String) -> Self {
        Self {
            unverified: Some(reason),
            ..Self::default()
        }
    }
}

/// R6-I3: whether any lane line carries an HMAC (the case was measured
/// under an audit key). Read from the text so a damaged lane counts too.
/// R8: only ever used to ADD a restriction on a PC without keys — never
/// to decide whether authentication is required.
fn is_keyed_case(lanes: &[(String, String)]) -> bool {
    lanes
        .iter()
        .any(|(_, text)| text.contains("\"entry_hmac_sha256\""))
}

/// Every present lane's text, in continuation order (R5-P8).
fn read_lanes(case_dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut lanes = Vec::new();
    for rel in lane_rels(case_dir)? {
        let path = case_dir.join(&rel);
        match audit::read_log_locked(&path) {
            Ok(Some(text)) => lanes.push((rel, text)),
            Ok(None) => continue,
            Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
        }
    }
    Ok(lanes)
}

/// R6-I3: each lane's verification, with a continuation lane (`.<n>.`,
/// n ≥ 1) accepted only when it is a PROVEN continuation: its first entry
/// is an `audio-level-lane-continued` marker naming exactly the lane before
/// it (`previous_lane`, `previous_lane_sha256` = SHA-256 of that lane's
/// bytes now), and that lane is itself not accepted (its chain does not
/// verify, or it is an unproven continuation). Anything else — a lane
/// planted next to an intact one, a marker for another lane, a lane edited
/// after it was continued from — is reported as not verifying, so its
/// results never replace verified measurements.
///
/// R8-I6 (supersedes R7-I1's marker rule): whether a lane is DAMAGED is
/// structure only — chain verification plus this continuation proof. A
/// marker this PC cannot authenticate (signed on another PC, or written
/// without a key) is NOT damage: treating it as damage made two PCs open a
/// new lane for each other's lane forever (ping-pong). Trust is decided
/// per LINE instead (`AuditChainVerification::line_trust`, P0): with keys
/// held, every line of such a lane that is not authenticated here is not a
/// measurement, whatever lane it sits in. For the same reason "the previous
/// lane verifies here" only contradicts a marker THIS PC authenticated —
/// the PC that wrote any other marker may hold a key that refuted the
/// previous lane (P2). R7-I10: a previous lane REPAIRED by truncating it to
/// its verifying prefix still matches, through the prefix hash the marker
/// recorded (`previous_lane_verified_prefix_sha256`).
///
/// R10-I5: each lane also goes through the shared lane/case binding check
/// (`audit::apply_lane_binding`, R9-I11) — R9 verified sound lanes from
/// their text only, so an authenticated sound lane copied from another
/// case or lane still measured here. An authenticated entry bound
/// elsewhere makes the lane not verify (like a chain break: none of its
/// lines is a measurement); a lane whose location cannot be confirmed (a
/// junction / link on its path) has every line NOT VERIFIED.
fn lane_statuses(
    case_dir: &Path,
    lanes: &[(String, String)],
    keys: &[crate::audit_key::AuditKey],
) -> Vec<LaneStatus> {
    let mut statuses: Vec<LaneStatus> = Vec::with_capacity(lanes.len());
    for (index, (rel, text)) in lanes.iter().enumerate() {
        let mut verification =
            audit::verify_chained_jsonl_text(text, rel, keys).and_then(|verification| {
                audit::apply_lane_binding(&audit::join_rel(case_dir, rel), text, verification)
                    .map_err(|err| format!("{rel}: {err}"))
            });
        let number = audit::audio_level_lane_number(rel).unwrap_or(0);
        if number >= 1
            && let Ok(verified) = &verification
        {
            let previous = index
                .checked_sub(1)
                .map(|p| (&lanes[p], &statuses[p]))
                .filter(|((prev_rel, _), _)| {
                    audit::audio_level_lane_number(prev_rel) == Some(number - 1)
                });
            // Signed under a key this PC does not hold: the PC that wrote
            // it could see damage this PC cannot (a refuting key).
            let marker_from_key_not_held = matches!(
                verified.line_trust(0),
                audit::LineTrust::OtherPc { .. }
                    | audit::LineTrust::SelfConsistentOnly { key_id: Some(_) }
            );
            let proof = continuation_proof(text, number, previous, marker_from_key_not_held);
            if let Err(why) = proof {
                verification = Err(format!("연속 레인 연결 미검증 — {why}"));
            }
        }
        statuses.push(LaneStatus {
            rel: rel.clone(),
            verification,
        });
    }
    statuses
}

/// R7-I6: the newest sound-measurement lane when it is accepted (verifies
/// and, for a continuation lane, is a proven continuation) — the only lane
/// a rotation marker may be appended to. `None` without lanes or when the
/// newest lane is not accepted.
pub fn verified_active_lane(case_dir: &Path) -> Result<Option<String>, String> {
    let lanes = read_lanes(case_dir)?;
    let keys = crate::audit_key::verification_keys()?;
    let statuses = lane_statuses(case_dir, &lanes, &keys);
    Ok(statuses
        .last()
        .filter(|status| status.verification.is_ok())
        .map(|status| status.rel.clone()))
}

/// R7-I10: the warning every damaged-lane message carries.
pub const DO_NOT_REPAIR_LANE: &str = "손상된 측정 레인을 직접 고치지 마십시오 — 이미 다음 레인으로 이어진 레인은 수리하지 않습니다(측정은 다음 레인에서 계속되고, 연속 표식이 손상 당시 해시로 연결을 확인 — 검증되는 구간까지 잘라 낸 상태만 '수리 전 검증 구간' 해시로 인정됨). 아직 이어지지 않은 레인만 frametrace repair-lane <레인 파일>로 수리합니다(잘린 바이트 보관 + lane-repair 표식)";

/// Why `text` (continuation lane number `number`) is not a proven
/// continuation of `previous`, or `Ok` when it is (see [`lane_statuses`]).
fn continuation_proof(
    text: &str,
    number: u32,
    previous: Option<(&(String, String), &LaneStatus)>,
    marker_from_key_not_held: bool,
) -> Result<(), String> {
    let marker = text
        .lines()
        .next()
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| {
            entry.get("event").and_then(Value::as_str) == Some("audio-level-lane-continued")
        })
        .ok_or_else(|| {
            "첫 항목이 레인 연속 표식(audio-level-lane-continued)이 아닙니다".to_string()
        })?;
    let prefix_sha = marker
        .get("previous_lane_verified_prefix_sha256")
        .and_then(Value::as_str);
    let prefix_entries = marker
        .get("previous_lane_verified_prefix_entries")
        .and_then(Value::as_u64);
    let Some(((prev_rel, prev_text), prev_status)) = previous else {
        // R7-I10: a lane whose very first line was damaged has an empty
        // verifying prefix; "repairing" it removes the lane altogether.
        let expected = if number == 1 {
            LOG_REL.to_string()
        } else {
            format!("evidence/logs/audio-level-log.{}.jsonl", number - 1)
        };
        if prefix_entries == Some(0)
            && marker.get("previous_lane").and_then(Value::as_str) == Some(expected.as_str())
        {
            return Ok(());
        }
        return Err("바로 앞 번호의 측정 레인이 없습니다".to_string());
    };
    if marker.get("previous_lane").and_then(Value::as_str) != Some(prev_rel.as_str()) {
        return Err(format!(
            "연속 표식이 앞 레인 {prev_rel}을 지목하지 않습니다"
        ));
    }
    let recorded = marker
        .get("previous_lane_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| "연속 표식에 앞 레인 해시(previous_lane_sha256)가 없습니다".to_string())?;
    let now = crate::sha256::digest_bytes(prev_text.as_bytes());
    if recorded.eq_ignore_ascii_case(&now) {
        // R8-I6/P2: a marker signed under a key this PC does not hold was
        // written by a PC that may hold a key refuting the previous lane —
        // its "damaged" verdict is not a contradiction here (its lines are
        // not trusted here anyway, P0). Any other marker (authenticated
        // here, or unkeyed: a keyless PC sees no more damage than this one)
        // next to a previous lane that verifies is a planted lane.
        if prev_status.verification.is_ok() && !marker_from_key_not_held {
            return Err(format!(
                "앞 레인 {prev_rel}이 정상 검증되는데 새 레인이 시작되었습니다"
            ));
        }
        return Ok(());
    }
    // R7-I10: the damaged lane was repaired by cutting it back to the
    // prefix that verified when the continuation was opened.
    if prefix_sha.is_some_and(|sha| sha.eq_ignore_ascii_case(&now))
        && prev_status.verification.is_ok()
    {
        return Ok(());
    }
    Err(format!(
        "앞 레인 {prev_rel}이 연속 표식 기록 후 바뀌었습니다(해시 불일치 — 손상 당시와도, 수리 전 검증 구간과도 다름). {DO_NOT_REPAIR_LANE}"
    ))
}

/// The lane new measurement entries are appended to (R5-P8): the newest
/// lane while its chain verifies. When it does not (an edited line, a torn
/// tail), measurement continues in a NEW lane `audio-level-log.<n+1>.jsonl`
/// whose first entry names the damaged lane and why — the damaged lane is
/// left exactly as found (never repaired or rewritten), so nothing it holds
/// is lost and every later measurement is still chained.
///
/// R6-I3: "verifies" means accepted by [`lane_statuses`] (a continuation
/// lane must be a proven one), and the marker records the damaged lane's
/// SHA-256 (`previous_lane_sha256`) so the continuation can be checked.
/// R7-I10: it also records the damaged lane's verifying prefix (entries and
/// SHA-256), so a lane later repaired by truncation still matches.
pub fn active_lane_for_append(case_dir: &Path) -> Result<PathBuf, String> {
    let lanes = read_lanes(case_dir)?;
    let Some((last, last_text)) = lanes.last() else {
        return Ok(case_dir.join(LOG_REL));
    };
    let keys = crate::audit_key::verification_keys()?;
    let statuses = lane_statuses(case_dir, &lanes, &keys);
    let Some(Err(reason)) = statuses.last().map(|status| status.verification.clone()) else {
        return Ok(case_dir.join(last));
    };
    let number = audit::audio_level_lane_number(last).unwrap_or(0) + 1;
    let rel = format!("evidence/logs/audio-level-log.{number}.jsonl");
    let next = case_dir.join(&rel);
    let (prefix_entries, prefix_len) = audit::verified_prefix(last_text, &keys);
    let marker = json!({
        "schema_version": 1,
        "event": "audio-level-lane-continued",
        "logged_unix": now_unix()?,
        "previous_lane": last,
        "previous_lane_sha256": crate::sha256::digest_bytes(last_text.as_bytes()),
        "previous_lane_verified_prefix_entries": prefix_entries,
        "previous_lane_verified_prefix_sha256":
            crate::sha256::digest_bytes(&last_text.as_bytes()[..prefix_len]),
        "previous_lane_error": reason,
        "note": "the previous sound-measurement lane does not verify as a chain; it is kept unchanged and its results are not shown as levels — measurement continues in this lane (do not repair the previous lane: this marker checks it by hash)",
    });
    audit::append_chained_jsonl(&next, &marker.to_string())?;
    Ok(next)
}

/// R5-P8: one entry for the report's audit-chain table covering every
/// sound-measurement lane: the newest lane's verification, and — named in
/// `detail` — every earlier lane whose chain is damaged (its results are
/// shown as `unverified`).
pub fn chain_summary_json(case_dir: &Path) -> Value {
    let statuses = match read_logged_results(case_dir) {
        Ok((_, statuses)) => statuses,
        Err(err) => {
            return json!({"path": LOG_REL, "status": "failed", "detail": err});
        }
    };
    let Some(newest) = statuses.last() else {
        return json!({"path": LOG_REL, "status": "absent", "entries": 0});
    };
    let mut details: Vec<String> = Vec::new();
    for status in &statuses[..statuses.len() - 1] {
        if let Ok(verification) = &status.verification
            && let trust @ audit::LaneTrust::NotVerified { .. } = verification.lane_trust()
        {
            details.push(format!("이전 측정 레인 {}: {}", status.rel, trust.reason()));
        }
        if let Err(err) = &status.verification {
            details.push(format!(
                "이전 측정 레인 {} 체인 손상 — 그 레인에만 있는 측정은 표시하지 않음 (unverified): {err}. {DO_NOT_REPAIR_LANE}",
                status.rel
            ));
        }
    }
    if statuses.len() > 1 {
        details.push(format!(
            "측정 레인 {}개: {}",
            statuses.len(),
            statuses
                .iter()
                .map(|status| status.rel.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let entries: usize = statuses
        .iter()
        .filter_map(|status| status.verification.as_ref().ok().map(|v| v.entries))
        .sum();
    match &newest.verification {
        Ok(verification) => {
            let newest_trust = verification.lane_trust();
            if !newest_trust.reason().is_empty() {
                details.insert(0, newest_trust.reason().to_string());
            }
            details.extend(verification.warnings.iter().cloned());
            let damaged = statuses.iter().any(|status| status.verification.is_err());
            // R8 P0/P2 (and R7-I3): with keys held, a lane holding any line
            // not authenticated here is never shown as passing — NOT
            // VERIFIED, naming the other PCs' keys; never "failed".
            let trusts: Vec<audit::LaneTrust> = statuses
                .iter()
                .filter_map(|status| status.verification.as_ref().ok())
                .map(audit::AuditChainVerification::lane_trust)
                .collect();
            json!({
                "path": newest.rel,
                "status": if damaged {
                    "failed"
                } else if trusts.iter().any(|trust| !trust.passes()) {
                    "not-verified"
                } else if trusts.iter().all(|trust| *trust == audit::LaneTrust::Authenticated) {
                    "keyed"
                } else {
                    "structural"
                },
                "entries": entries,
                "detail": details.join("; "),
            })
        }
        Err(err) => {
            details.insert(0, err.clone());
            json!({"path": newest.rel, "status": "failed", "detail": details.join("; ")})
        }
    }
}

/// R5-P8: what a cached `audioLevels` map depends on besides the index —
/// every measurement lane's size and full-resolution mtime, and the
/// verification key set (a key added, removed or retired changes which
/// lanes verify, so a map computed under another keyring is stale).
pub fn levels_cache_key(case_dir: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    match lane_rels(case_dir) {
        Ok(lanes) => {
            for rel in lanes {
                let stamp = std::fs::metadata(case_dir.join(&rel))
                    .map(|meta| format!("{}:{:?}", meta.len(), audit::modified_nanos(&meta)))
                    .unwrap_or_else(|err| format!("err:{err}"));
                parts.push(format!("{rel}={stamp}"));
            }
        }
        Err(err) => parts.push(format!("lanes-err:{err}")),
    }
    parts.push(format!(
        "keys={}",
        crate::audit_key::verification_fingerprint()
    ));
    parts.join("|")
}

/// Error prefix for a measurement log whose chain did not verify (D1).
pub const UNVERIFIED_ERROR: &str =
    "소리 측정 기록 검증 실패 — 측정 기록(audio-level-log)의 체인이 맞지 않아 표시하지 않습니다";

/// Parses every `audio-level` entry of the lane text (no verification);
/// `line_flag(index)` is the `chain_unverified` reason of the results on
/// line `index` (0-based; R6-I3).
/// Results of entries replayed from a crash checkpoint are flagged
/// `replayed_unverified` (D2).
fn parse_logged_results_lines(text: &str, line_flag: &dyn Fn(usize) -> LineFlag) -> LoggedResults {
    let mut out = BTreeMap::new();
    // R14 I5: the last usable `lane-repair` marker of this lane — a
    // measurement before it is not known to be the newest of its record
    // (the cut tail may have held a newer one).
    let repaired_at = text
        .lines()
        .enumerate()
        .filter(|(index, line)| {
            serde_json::from_str::<Value>(line)
                .is_ok_and(|entry| audit::is_lane_repair_marker(&entry))
                && line_flag(*index).unverified.is_none()
        })
        .map(|(index, _)| index)
        .last();
    for (index, line) in text.lines().enumerate() {
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
        let flagged = |key: &str| entry.get(key).and_then(Value::as_bool) == Some(true);
        let replayed = flagged("replayed_from_checkpoint") || flagged("replayed_unverified");
        for item in entry
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(mut result) = FileResult::from_json(item) {
                result.replayed_unverified = replayed;
                let flag = line_flag(index);
                result.chain_unverified = flag.unverified.or_else(|| {
                    repaired_at
                        .filter(|at| *at > index)
                        .map(|at| format!("{UNVERIFIED_ERROR}: {AUDIO_REPAIRED_AFTER_NOTE} ({}번째 줄 뒤 {}번째 줄)", index + 1, at + 1))
                });
                result.self_consistent_only = flag.self_consistent_only;
                result.other_pc_key = flag.other_pc_key;
                let at = result.measured_unix.unwrap_or(measured);
                let selector = result.selector.clone();
                insert_logged_result(
                    &mut out,
                    selector,
                    (result, at, method.clone(), threshold),
                    true,
                );
            }
        }
    }
    out
}

/// R14 I5: a measurement positioned before a usable `lane-repair` marker
/// of its lane.
pub const AUDIO_REPAIRED_AFTER_NOTE: &str = "측정 레인이 이 측정 뒤에서 수리됨(lane-repair) — 잘린 꼬리에 같은 기록의 더 새 측정이 있었을 수 있어 최신 여부 알 수 없음 (다시 측정하십시오)";

/// Later results win — except that (R6-I3) a result that is not verified
/// never replaces a verified one: a planted lane or an unkeyed line can
/// add an unverified result, never overwrite a genuine measurement.
fn insert_logged_result(
    out: &mut LoggedResults,
    selector: String,
    entry: (FileResult, u64, String, f64),
    supersedes: bool,
) {
    // R8-I8: and a self-consistent-only result (pre-key / no keys) never
    // replaces one authenticated here.
    let rank = |result: &FileResult| match (&result.chain_unverified, result.self_consistent_only) {
        (Some(_), _) => 0,
        (None, true) => 1,
        (None, false) => 2,
    };
    // R14 I5 (P7): when `supersedes` (the newer result comes from the same
    // lane, or from a proven continuation lane), a NEWER result that is not
    // usable here (stripped HMAC, another PC's line, a line written without
    // a key in a keyed case) replaces the older usable one — the record is
    // then "unverified", never rolled back to the older measurement. A lane
    // that is not a proven continuation (planted next to an intact one)
    // still never replaces a verified measurement. Among usable results an
    // authenticated one is not replaced by a self-consistent-only one.
    let keeps_existing = !(supersedes && entry.0.chain_unverified.is_some())
        && out
            .get(&selector)
            .is_some_and(|(existing, ..)| rank(existing) > rank(&entry.0));
    if !keeps_existing {
        out.insert(selector, entry);
    }
}

/// The per-record state handed to the viewer / report (§2.4).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioLevelState {
    /// `sound` | `silent` | `error` | `stale` | `unverified` (D1: the
    /// measurement log's chain did not verify) | `replayed` (D2: logged
    /// from a crash checkpoint without a fresh measurement).
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
    /// R8 P1 / I8: the level's measurement line is self-consistency only
    /// (no audit key on this PC, or written before the lane's key
    /// introduction). Compact JSON `"sc": true` — shown with
    /// "자기 일관성만 확인(인증 아님)".
    pub self_consistent_only: bool,
    /// R8 P2: an `unverified` state whose line is signed under this key id,
    /// held by another PC. Compact JSON `"pc": "<key id>"` — shown as "이
    /// PC에서 인증 불가 — 키 <id> 보유 PC에서 확인".
    pub other_pc_key: Option<String>,
}

impl AudioLevelState {
    pub fn to_json(&self) -> Value {
        let mut value =
            json!({"r": self.r, "max": self.max, "mean": self.mean, "sec": self.sec, "t": self.t});
        if let Some(map) = value.as_object_mut() {
            if self.self_consistent_only {
                map.insert("sc".to_string(), json!(true));
            }
            if let Some(key) = &self.other_pc_key {
                map.insert("pc".to_string(), json!(key));
            }
            // R9 P3 (contract with track U): the same per-entry `trust`
            // code every other log entry carries.
            map.insert("trust".to_string(), json!(self.trust_code()));
            // R14 (track U contract): why an `unverified` / `replayed`
            // state is not a level — the viewer shows it as is.
            if matches!(self.r, "unverified" | "replayed")
                && let Some(why) = &self.error
            {
                map.insert("note".to_string(), json!(why));
            }
        }
        value
    }

    /// R9 P3: `"pc:<key id>"` (another PC's key), `"unauth"` (the state
    /// is `unverified` — not a measurement here), `"sc"` (self-consistency
    /// only), otherwise `"auth"` (authenticated here).
    pub fn trust_code(&self) -> String {
        if let Some(key) = &self.other_pc_key {
            format!("pc:{key}")
        } else if self.r == "unverified" {
            "unauth".to_string()
        } else if self.self_consistent_only {
            "sc".to_string()
        } else {
            "auth".to_string()
        }
    }
}

fn state_from(result: &FileResult, measured: u64) -> Option<AudioLevelState> {
    if let Some(reason) = &result.chain_unverified {
        // D1/R5-P8: read from a lane whose chain does not verify.
        let mut state = unconfirmed_state("unverified", measured, reason);
        state.other_pc_key = result.other_pc_key.clone();
        return Some(state);
    }
    if result.replayed_unverified {
        // D2: never a level — say why instead.
        return Some(unconfirmed_state(
            "replayed",
            measured,
            "체크포인트에서 재기록된 측정 — 다시 측정하기 전에는 미확인",
        ));
    }
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
        self_consistent_only: result.self_consistent_only,
        other_pc_key: None,
    })
}

/// A state that claims no level (`unverified` / `replayed`).
fn unconfirmed_state(r: &'static str, measured: u64, why: &str) -> AudioLevelState {
    AudioLevelState {
        r,
        max: None,
        mean: None,
        sec: None,
        t: measured,
        error: Some(why.to_string()),
        codec: None,
        max_is_neg_inf: false,
        self_consistent_only: false,
        other_pc_key: None,
    }
}

/// Applies the latest measurement per id to the current index: a
/// measurement whose size/mtime/hash binding no longer matches becomes
/// `stale` ("측정 후 파일 변경 — 미확인"). Ids absent from the index are
/// dropped. This is the single invalidation rule the server and report use.
///
/// D1: when the measurement log's chain does not verify, every record that
/// has a logged measurement is `unverified` — the report says the record
/// could not be trusted instead of claiming sound or silence.
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
        if result.chain_unverified.is_some() {
            // Read from a damaged lane: its size/mtime binding is no more
            // trustworthy than its level (R5-P8).
            if let Some(state) = state_from(result, *measured) {
                out.insert(row.id.clone(), state);
            }
            continue;
        }
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
                    self_consistent_only: false,
                    other_pc_key: None,
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
    pub ffprobe: String,
    pub resume: ResumeMode,
}

impl Default for MeasureOptions {
    fn default() -> Self {
        Self {
            selectors: None,
            force: false,
            ffmpeg: "ffmpeg".to_string(),
            ffprobe: "ffprobe".to_string(),
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
    /// D2: computed-but-unlogged results of an interrupted run that were
    /// discarded and measured again.
    pub discarded_checkpoint_results: usize,
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
            "discarded_checkpoint_results": self.discarded_checkpoint_results,
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
    // D1/R5-P8: a result from a lane whose chain does not verify vouches
    // for nothing — its record is measured again (into a new lane, see
    // `active_lane_for_append`).
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
            && !result.replayed_unverified
            && result.chain_unverified.is_none()
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

    // B11: the ffmpeg build is part of the method — a checkpoint written by
    // another build is not resumed into this run's log.
    let fingerprint = checkpoint::fingerprint(&[
        "measure-audio",
        METHOD,
        &format!("{SILENT_MAX_DBFS:.1}"),
        &ffmpeg_version,
    ]);
    let mut checkpoint = RunCheckpoint::begin(
        &case_dir.join(CHECKPOINT_REL),
        "measure-audio",
        &fingerprint,
        options.resume,
    )?;
    // D2: results computed by an interrupted run are NOT logged from the
    // checkpoint — a checkpoint line is not evidence the bytes were
    // measured (it can be edited, or describe other bytes). Every target
    // without a valid logged measurement is measured again; the checkpoint
    // only tells how many computed results were discarded.
    summary.discarded_checkpoint_results =
        discarded_checkpoint_results(checkpoint.data_lines().iter().map(String::as_str));
    let tally = |summary: &mut MeasureSummary, verdict: Verdict| match verdict {
        Verdict::Sound => summary.sound += 1,
        Verdict::Silent => summary.silent += 1,
        _ => summary.error += 1,
    };
    let pending: Vec<IndexRow> = targets.clone();
    let total = targets.len();
    let mut done = 0usize;
    progress(done, total);

    // R5-P8: a damaged newest lane is never appended to (that refused
    // every later measurement); a new lane continues the record.
    let lane_path = active_lane_for_append(case_dir)?;
    let flush_entry = |entry: Value,
                       buffer: &mut Vec<FileResult>,
                       checkpoint: &mut RunCheckpoint,
                       summary: &mut MeasureSummary|
     -> Result<(), String> {
        audit::append_chained_jsonl(&lane_path, &entry.to_string())?;
        summary.log_entries += 1;
        let ids: Vec<&str> = buffer.iter().map(|r| r.selector.as_str()).collect();
        checkpoint.append_line(&json!({ "logged": ids }).to_string())?;
        buffer.clear();
        Ok(())
    };
    let flush = |buffer: &mut Vec<FileResult>,
                 checkpoint: &mut RunCheckpoint,
                 summary: &mut MeasureSummary|
     -> Result<(), String> {
        if buffer.is_empty() {
            return Ok(());
        }
        let entry = log_entry_json(now_unix()?, &ffmpeg_version, buffer);
        flush_entry(entry, buffer, checkpoint, summary)
    };
    let mut buffer: Vec<FileResult> = Vec::new();

    let (sender, receiver) = std::sync::mpsc::channel::<FileResult>();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let pending_ref = &pending;
    let ffmpeg_ref = &ffmpeg;
    let ffprobe_ref = options.ffprobe.as_str();
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
                    if sender
                        .send(measure_row(ffmpeg_ref, ffprobe_ref, row))
                        .is_err()
                    {
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
            let line = json!({
                "computed": result.to_json(),
                "measured_unix": now_unix().unwrap_or(0),
                "ffmpeg_version": ffmpeg_version,
            })
            .to_string();
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

/// Computed-but-unlogged results an interrupted run left in the checkpoint
/// (D2: discarded and measured again, never logged as measurements).
fn discarded_checkpoint_results<'a>(lines: impl IntoIterator<Item = &'a str>) -> usize {
    let mut logged: HashSet<String> = HashSet::new();
    let mut computed: HashSet<String> = HashSet::new();
    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(result) = value.get("computed").and_then(FileResult::from_json) {
            computed.insert(result.selector);
        } else if let Some(ids) = value.get("logged").and_then(Value::as_array) {
            logged.extend(ids.iter().filter_map(Value::as_str).map(str::to_string));
        }
    }
    computed.difference(&logged).count()
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
            raw_path: None,
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
            measured_unix: None,
            replayed_unverified: false,
            chain_unverified: None,
            self_consistent_only: false,
            other_pc_key: None,
        }
    }

    /// B1: decoder complaints are found in top-level / component lines, not
    /// in indented metadata or the volumedetect report.
    #[test]
    fn decode_errors_are_detected_in_stderr() {
        let clean = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'a.mp4':\n  Metadata:\n    comment         : error corrupt missing\n[Parsed_volumedetect_0 @ a] n_samples: 10\nsize=N/A time=00:00:02.00 bitrate=N/A speed= 300x\n";
        assert_eq!(decode_error_in(clean), None);
        let bad = "[aac @ 0000] Error decoding AAC frame header.\nError while decoding stream #0:1: Invalid data found when processing input\n";
        assert!(decode_error_in(bad).unwrap().contains("Error decoding AAC"));
        let partial = "[mov,mp4,m4a,3gp,3g2,mj2 @ 01] stream 1, offset 0x30: partial file\n";
        assert!(decode_error_in(partial).is_some());
    }

    /// B1: a decode well short of the probed duration is not a full-track
    /// measurement; small priming differences are.
    #[test]
    fn coverage_requires_the_whole_track() {
        assert_eq!(coverage_error(Some(1.98), Some(2.0), true), None);
        assert_eq!(coverage_error(Some(599.3), Some(600.0), true), None);
        assert!(
            coverage_error(Some(598.5), Some(600.0), true).is_some(),
            "> 1 s short"
        );
        assert!(
            coverage_error(Some(1.0), Some(2.0), true).is_some(),
            "< 95%"
        );
        assert!(
            coverage_error(None, Some(2.0), true).is_some(),
            "unknown measured"
        );
        assert!(
            coverage_error(Some(2.0), None, false).is_some(),
            "unknown expected"
        );
        // A container duration allows the audio to end a little early.
        assert_eq!(coverage_error(Some(58.5), Some(60.0), false), None);
        assert!(coverage_error(Some(30.0), Some(60.0), false).is_some());
    }

    /// B3: a file whose size/mtime no longer match the index binding is not
    /// measured (no ffmpeg needed to prove it) — the result is an error.
    #[test]
    fn changed_file_is_an_error_not_a_level() {
        let dir = std::env::temp_dir().join(format!(
            "ft-audio-changed-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"0123456789").unwrap();
        let mut r = row("vid_1");
        r.source_path = file.to_string_lossy().to_string();
        r.size_bytes = Some(11);
        let result = measure_row("no-such-ffmpeg", "no-such-ffprobe", &r);
        assert_eq!(result.result, Verdict::Error);
        assert_eq!(result.error.as_deref(), Some(CHANGED_ERROR));
        assert!(result.streams.is_empty());
        // Missing file: same.
        r.source_path = dir.join("gone.mp4").to_string_lossy().to_string();
        assert_eq!(
            measure_row("x", "y", &r).error.as_deref(),
            Some(CHANGED_ERROR)
        );
        // B12: bytes match, but only `audio_codec` is indexed and the live
        // probe cannot run → error, never a silent/sound claim.
        let meta = std::fs::metadata(&file).unwrap();
        r.source_path = file.to_string_lossy().to_string();
        r.size_bytes = Some(meta.len());
        r.modified_unix = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        let result = measure_row("no-such-ffmpeg", "no-such-ffprobe", &r);
        assert_eq!(result.result, Verdict::Error);
        assert!(
            result.error.as_deref().unwrap().contains("오디오 트랙 수"),
            "{result:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R5-P3: a record whose name is not valid Unicode is stat'ed (and
    /// probed/decoded) through its lossless path — read from the index
    /// row's `source_path_raw_hex` — not reported "changed" because the
    /// U+FFFD display spelling names no file.
    #[test]
    fn non_unicode_names_are_measured_through_the_lossless_path() {
        let dir = std::env::temp_dir().join(format!(
            "ft-audio-p3-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(crate::model::test_non_unicode_name("a"));
        if std::fs::write(&file, b"0123456789").is_err() {
            eprintln!("skipped: this filesystem refuses non-Unicode names");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let meta = std::fs::metadata(&file).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let json = serde_json::json!({
            "id": "vid_1",
            "relative_path": "a.mp4",
            "source_path": file.to_string_lossy(),
            "source_path_raw_hex": crate::model::path_raw_hex(&file).unwrap(),
            "size_bytes": meta.len(),
            "modified_unix": mtime,
            "audio_codec": "aac",
            "ffprobe_ok": true,
        });
        let r = IndexRow::from_json(&json).unwrap();
        assert_eq!(r.os_path(), file);
        let result = measure_row("no-such-ffmpeg", "no-such-ffprobe", &r);
        assert_eq!(result.result, Verdict::Error);
        assert_ne!(result.error.as_deref(), Some(CHANGED_ERROR), "{result:?}");
        assert!(
            result.error.as_deref().unwrap().contains("오디오 트랙 수"),
            "the file was found and the probe step reached: {result:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B15: exit 4 is a completed run whose errors are counted; the summary
    /// log line reports the counts.
    #[test]
    fn exit_four_is_a_completed_run_with_counted_errors() {
        // The message shape `run_step` produces for a non-zero exit.
        let partial = "measure-audio 실패 (exit Some(4)): {\"targets\":2} | 소리 측정 오류 3건 (측정 대상 9건)";
        assert_eq!(partial_error_count(partial), Some(3));
        let real = "measure-audio 실패 (exit Some(1)): 소리 측정 오류 3건";
        assert_eq!(partial_error_count(real), None);
        assert_eq!(
            partial_error_count("measure-audio 실패 (exit Some(4)): x"),
            None
        );
        let line = summary_log_line(
            "noise\n{\"targets\":4,\"sound\":2,\"silent\":1,\"error\":1,\"skipped_valid\":5}",
        )
        .unwrap();
        assert!(
            line.starts_with("소리 측정 완료(오류 1건): 소리 있음 2 · 무음 1"),
            "{line}"
        );
        assert!(summary_log_line("no json").is_none());
        assert!(
            summary_log_line("{\"targets\":1,\"error\":0}")
                .unwrap()
                .starts_with("소리 측정 완료:")
        );
    }

    /// B11: replayed results keep their original measurement time and are
    /// flagged as replayed; the reader uses the per-result time.
    #[test]
    fn replayed_entries_keep_original_provenance() {
        let r = row("vid_1");
        let mut result = result_for(&r, Verdict::Sound, -10.0);
        result.measured_unix = Some(100);
        let entry = replayed_log_entry_json(500, "ffmpeg 7.1", &[result]);
        assert_eq!(entry["measured_unix"], 100);
        assert_eq!(entry["logged_unix"], 500);
        assert_eq!(entry["replayed_from_checkpoint"], true);
        assert_eq!(entry["ffmpeg_version"], "ffmpeg 7.1");
        assert_eq!(entry["results"][0]["measured_unix"], 100);
        let fresh = log_entry_json(7, "v", &[result_for(&r, Verdict::Sound, -1.0)]);
        assert!(fresh["results"][0].get("measured_unix").is_none());
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
            r#"{"vid_1":{"r":"sound","max":-12.3,"mean":-32.3,"sec":1.0,"t":10,"trust":"auth"}}"#
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

    /// A case dir with one indexed row (`vid_1`) and a chained audio lane
    /// holding `entries` (raw entry JSON values).
    fn audio_case(name: &str, entries: &[Value]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ft-audio-{name}-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db")).unwrap();
        let row_json = json!({
            "id": "vid_1", "relative_path": "vid_1.mp4", "source_path": "/e/vid_1.mp4",
            "size_bytes": 100, "modified_unix": 5, "sha256": "aa", "audio_codec": "aac"
        });
        std::fs::write(dir.join("db/videos.jsonl"), format!("{row_json}\n")).unwrap();
        for entry in entries {
            audit::append_chained_jsonl_keyed(&dir.join(LOG_REL), &entry.to_string(), None)
                .unwrap();
        }
        dir
    }

    /// D1: one forged line (sound → silent, chain not recomputed) and the
    /// lane vouches for nothing: no record reads silent or sound, the
    /// verified reader refuses, and the reason is the chain failure.
    #[test]
    fn forged_measurement_line_is_unverified_never_silent() {
        let r = row("vid_1");
        let dir = audio_case(
            "forged",
            &[log_entry_json(
                7,
                "ffmpeg x",
                &[result_for(&r, Verdict::Sound, -12.0)],
            )],
        );
        assert_eq!(audio_levels_for_case(&dir).unwrap()["vid_1"].r, "sound");
        let log = dir.join(LOG_REL);
        let forged = std::fs::read_to_string(&log)
            .unwrap()
            .replace("\"result\":\"sound\"", "\"result\":\"silent\"")
            .replace("-12.0", "-91.0");
        std::fs::write(&log, forged).unwrap();
        let levels = audio_levels_for_case(&dir).unwrap();
        assert_eq!(levels["vid_1"].r, "unverified");
        assert_eq!(levels["vid_1"].max, None);
        assert!(
            levels["vid_1"]
                .error
                .as_deref()
                .unwrap()
                .starts_with(UNVERIFIED_ERROR),
            "{:?}",
            levels["vid_1"].error
        );
        // R5-P8: the reader flags the lane's results instead of failing,
        // so a later verified lane can still be read.
        let logged = latest_logged_results(&dir).unwrap();
        let reason = logged["vid_1"].0.chain_unverified.as_deref().unwrap();
        assert!(reason.starts_with(UNVERIFIED_ERROR), "{reason}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R5-P8: a damaged lane no longer blocks measurement forever. The next
    /// append goes to a new lane (`audio-level-log.1.jsonl`) that names the
    /// damage; the damaged lane is kept byte for byte, its records read
    /// `unverified`, and a record measured again in the new lane reads its
    /// new level. The report's chain entry names the damaged lane.
    #[test]
    fn damaged_lane_continues_in_a_new_lane_and_is_named() {
        let r1 = row("vid_1");
        let mut r2 = row("vid_2");
        r2.source_path = "/e/vid_2.mp4".into();
        let dir = audio_case(
            "lanes",
            &[log_entry_json(
                7,
                "ffmpeg x",
                &[
                    result_for(&r1, Verdict::Sound, -12.0),
                    result_for(&r2, Verdict::Sound, -20.0),
                ],
            )],
        );
        let row2 = json!({
            "id": "vid_2", "relative_path": "vid_2.mp4", "source_path": "/e/vid_2.mp4",
            "size_bytes": 100, "modified_unix": 5, "sha256": "aa", "audio_codec": "aac"
        });
        let index = std::fs::read_to_string(dir.join("db/videos.jsonl")).unwrap();
        std::fs::write(dir.join("db/videos.jsonl"), format!("{index}{row2}\n")).unwrap();
        // Torn tail: before R5 every later append was refused.
        let lane0 = dir.join(LOG_REL);
        let mut damaged = std::fs::read(&lane0).unwrap();
        damaged.extend_from_slice(b"{\"torn\":");
        std::fs::write(&lane0, &damaged).unwrap();
        assert!(audit::append_chained_jsonl_keyed(&lane0, "{\"x\":1}", None).is_err());
        let next = active_lane_for_append(&dir).unwrap();
        assert_eq!(next, dir.join("evidence/logs/audio-level-log.1.jsonl"));
        assert_eq!(std::fs::read(&lane0).unwrap(), damaged, "damaged lane kept");
        let marker = std::fs::read_to_string(&next).unwrap();
        assert!(marker.contains("audio-level-lane-continued"), "{marker}");
        assert!(marker.contains(LOG_REL), "{marker}");
        // The new lane is now the append target (it verifies).
        assert_eq!(active_lane_for_append(&dir).unwrap(), next);
        audit::append_chained_jsonl_keyed(
            &next,
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            None,
        )
        .unwrap();
        let levels = audio_levels_for_case(&dir).unwrap();
        assert_eq!(levels["vid_1"].r, "silent", "re-measured in the new lane");
        assert_eq!(levels["vid_2"].r, "unverified", "only in the damaged lane");
        assert_eq!(levels["vid_2"].max, None);
        let summary = chain_summary_json(&dir);
        assert_eq!(summary["path"], "evidence/logs/audio-level-log.1.jsonl");
        assert_eq!(summary["status"], "failed", "{summary}");
        let detail = summary["detail"].as_str().unwrap();
        assert!(
            detail.contains(LOG_REL) && detail.contains("손상"),
            "{detail}"
        );
        // The cache key moves with every lane (a new lane, a new append).
        let before = levels_cache_key(&dir);
        audit::append_chained_jsonl_keyed(
            &next,
            &log_entry_json(10, "ffmpeg x", &[]).to_string(),
            None,
        )
        .unwrap();
        assert_ne!(levels_cache_key(&dir), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R6-I3: a continuation lane counts only when it is a proven
    /// continuation of a lane that does not verify. A lane planted next to
    /// an intact lane (fresh chain, marker naming lane 0) cannot replace
    /// its measurements; a damaged lane edited after it was continued from
    /// voids the continuation.
    #[test]
    fn continuation_lanes_must_prove_their_link() {
        let r1 = row("vid_1");
        let dir = audio_case(
            "i3-planted",
            &[log_entry_json(
                7,
                "ffmpeg x",
                &[result_for(&r1, Verdict::Sound, -12.0)],
            )],
        );
        let planted = dir.join("evidence/logs/audio-level-log.1.jsonl");
        let lane0 = std::fs::read_to_string(dir.join(LOG_REL)).unwrap();
        audit::append_chained_jsonl_keyed(
            &planted,
            &json!({"event": "audio-level-lane-continued", "previous_lane": LOG_REL,
                "previous_lane_sha256": crate::sha256::digest_bytes(lane0.as_bytes())})
            .to_string(),
            None,
        )
        .unwrap();
        audit::append_chained_jsonl_keyed(
            &planted,
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            None,
        )
        .unwrap();
        let levels = audio_levels_for_case(&dir).unwrap();
        assert_eq!(levels["vid_1"].r, "sound", "planted lane ignored");
        assert_eq!(chain_summary_json(&dir)["status"], "failed");
        // New measurements never go into the unproven lane.
        assert_eq!(
            active_lane_for_append(&dir).unwrap(),
            dir.join("evidence/logs/audio-level-log.2.jsonl")
        );
        let _ = std::fs::remove_dir_all(&dir);

        // A genuine continuation, then the damaged lane is edited again.
        let dir = audio_case(
            "i3-edited",
            &[log_entry_json(
                7,
                "ffmpeg x",
                &[result_for(&r1, Verdict::Sound, -12.0)],
            )],
        );
        let lane0 = dir.join(LOG_REL);
        let mut damaged = std::fs::read(&lane0).unwrap();
        damaged.extend_from_slice(b"{\"torn\":");
        std::fs::write(&lane0, &damaged).unwrap();
        let next = active_lane_for_append(&dir).unwrap();
        assert!(
            std::fs::read_to_string(&next)
                .unwrap()
                .contains("previous_lane_sha256")
        );
        audit::append_chained_jsonl_keyed(
            &next,
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            None,
        )
        .unwrap();
        assert_eq!(audio_levels_for_case(&dir).unwrap()["vid_1"].r, "silent");
        damaged.extend_from_slice(b"more");
        std::fs::write(&lane0, &damaged).unwrap();
        assert_eq!(
            audio_levels_for_case(&dir).unwrap()["vid_1"].r,
            "unverified"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R6-I3: in a case measured under an audit key, a measurement line
    /// written without one is not used as a level. R14 I5 (P7): nor does
    /// the older keyed measurement of the same record come back as the
    /// current value — the record reads "unverified".
    #[test]
    fn unkeyed_lines_in_a_keyed_case_are_not_levels() {
        let r1 = row("vid_1");
        let dir = audio_case("i3-keyed", &[]);
        let key = crate::audit_key::AuditKey::new("i3-test-key", &[9; 32]);
        audit::append_chained_jsonl_keyed(
            &dir.join(LOG_REL),
            &log_entry_json(7, "ffmpeg x", &[result_for(&r1, Verdict::Sound, -12.0)]).to_string(),
            Some(&key),
        )
        .unwrap();
        audit::append_chained_jsonl_keyed(
            &dir.join(LOG_REL),
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            None,
        )
        .unwrap();
        let logged = latest_logged_results(&dir).unwrap();
        // R14 I5 (P7): the unkeyed newer line is no level, and the keyed
        // older one is not the current value either — unverified.
        assert!(logged["vid_1"].0.chain_unverified.is_some(), "{logged:?}");
        assert_eq!(
            audio_levels_for_case(&dir).unwrap()["vid_1"].r,
            "unverified"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R5-P8: the key-set fingerprint in the cache key changes when a key
    /// is retired or added, and does not depend on order.
    #[test]
    fn key_set_fingerprint_tracks_retirement_and_membership() {
        use crate::audit_key::{AuditKey, key_set_fingerprint};
        let a = AuditKey::new("a", &[1; 32]);
        let b = AuditKey::new("b", &[2; 32]);
        let base = key_set_fingerprint(&[a.clone(), b.clone()]);
        assert_eq!(base, key_set_fingerprint(&[b.clone(), a.clone()]));
        assert_ne!(base, key_set_fingerprint(std::slice::from_ref(&a)));
        assert_ne!(
            base,
            key_set_fingerprint(&[a.clone().retired_at(5), b.clone()])
        );
        assert_ne!(base, key_set_fingerprint(&[]));
    }

    /// D2: a result logged from a crash checkpoint (never re-measured) is
    /// not a level — it reads `replayed` until measured again.
    #[test]
    fn replayed_checkpoint_result_is_not_a_level() {
        let r = row("vid_1");
        let mut result = result_for(&r, Verdict::Silent, -91.0);
        result.measured_unix = Some(3);
        let dir = audio_case(
            "replayed",
            &[replayed_log_entry_json(9, "ffmpeg x", &[result])],
        );
        let levels = audio_levels_for_case(&dir).unwrap();
        assert_eq!(levels["vid_1"].r, "replayed");
        assert_eq!(levels["vid_1"].max, None);
        let logged = latest_logged_results(&dir).unwrap();
        assert!(logged["vid_1"].0.replayed_unverified);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D2: a resumed run discards computed-but-unlogged checkpoint results
    /// (they are measured again) and counts them.
    #[test]
    fn checkpoint_results_are_counted_and_discarded() {
        let r = row("vid_1");
        let computed = |id: &str| {
            let mut result = result_for(&r, Verdict::Sound, -1.0);
            result.selector = id.to_string();
            json!({"computed": result.to_json(), "measured_unix": 1}).to_string()
        };
        let lines = [
            computed("vid_1"),
            computed("vid_2"),
            json!({"logged": ["vid_1"]}).to_string(),
            computed("vid_3"),
            "{torn".to_string(),
        ];
        assert_eq!(
            discarded_checkpoint_results(lines.iter().map(String::as_str)),
            2
        );
    }

    /// D17: a rewrite within the same second changes the stamp even though
    /// the whole-second index binding is unchanged.
    #[test]
    fn same_second_rewrite_changes_the_stamp() {
        let dir = std::env::temp_dir().join(format!(
            "ft-audio-stamp-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"0123456789").unwrap();
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_100);
        let set = |at: std::time::SystemTime| {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&file)
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        set(base);
        let path = file.to_string_lossy().to_string();
        let before = stat_stamp(&path);
        set(base + std::time::Duration::from_millis(500));
        let after = stat_stamp(&path);
        assert_eq!(
            before.unwrap().binding(),
            after.unwrap().binding(),
            "same size and whole second"
        );
        assert!(stamp_changed(before, after), "{before:?} vs {after:?}");
        assert!(!stamp_changed(after, stat_stamp(&path)));
        assert!(stamp_changed(after, None));
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

    /// R7-I1: in a keyed case on a PC that holds audit keys, a forged
    /// measurement line carrying an HMAC under an invented key id is not a
    /// level (it used to count because any HMAC line "is keyed"), and a
    /// continuation lane whose marker is not authenticated here is not a
    /// proven continuation.
    #[test]
    fn forged_key_id_lines_and_markers_are_not_levels_in_a_keyed_case() {
        let r1 = row("vid_1");
        let dir = audio_case("r7-i1", &[]);
        let mine = crate::audit_key::AuditKey::new("mine", &[0x41; 32]);
        let bogus = crate::audit_key::AuditKey::new("bogus-id", &[0x42; 32]);
        let keys = std::slice::from_ref(&mine);
        audit::append_chained_jsonl_keyed(
            &dir.join(LOG_REL),
            &log_entry_json(7, "ffmpeg x", &[result_for(&r1, Verdict::Sound, -12.0)]).to_string(),
            Some(&mine),
        )
        .unwrap();
        audit::append_chained_jsonl_keyed(
            &dir.join(LOG_REL),
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            Some(&bogus),
        )
        .unwrap();
        let (logged, statuses) = read_logged_results_with_keys(&dir, keys).unwrap();
        let (result, at, ..) = &logged["vid_1"];
        // R14 I5 (P7): the newer line is not a level — and the record is
        // not rolled back to the older measurement either (stripping a
        // newer line's HMAC must not bring the older one back): unverified.
        assert_eq!(*at, 9, "{logged:?}");
        assert!(result.chain_unverified.is_some(), "{logged:?}");
        // The lane itself is NOT VERIFIED (R7-I3), never "keyed"/"structural".
        assert!(
            statuses[0]
                .verification
                .as_ref()
                .unwrap()
                .rewrite_not_excluded
                .is_some()
        );
        let _ = std::fs::remove_dir_all(&dir);

        // A damaged keyed lane continued by a lane whose marker is signed
        // under an invented key id: not a proven continuation.
        let dir = audio_case("r7-i1-marker", &[]);
        audit::append_chained_jsonl_keyed(
            &dir.join(LOG_REL),
            &log_entry_json(7, "ffmpeg x", &[result_for(&r1, Verdict::Sound, -12.0)]).to_string(),
            Some(&mine),
        )
        .unwrap();
        let lane0 = dir.join(LOG_REL);
        let mut damaged = std::fs::read(&lane0).unwrap();
        damaged.extend_from_slice(b"{\"torn\":");
        std::fs::write(&lane0, &damaged).unwrap();
        let planted = dir.join("evidence/logs/audio-level-log.1.jsonl");
        audit::append_chained_jsonl_keyed(
            &planted,
            &json!({"event": "audio-level-lane-continued", "previous_lane": LOG_REL,
                "previous_lane_sha256": crate::sha256::digest_bytes(&damaged)})
            .to_string(),
            Some(&bogus),
        )
        .unwrap();
        audit::append_chained_jsonl_keyed(
            &planted,
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            Some(&bogus),
        )
        .unwrap();
        let (logged, statuses) = read_logged_results_with_keys(&dir, keys).unwrap();
        // R8-I6: a marker under a key this PC does not hold is not DAMAGE
        // (no new lane, no ping-pong) — but every line of the lane that is
        // not authenticated here is still not a measurement (P0), named as
        // another PC's (P2).
        assert!(statuses[1].verification.is_ok(), "{statuses:?}");
        let result = &logged["vid_1"].0;
        assert!(result.chain_unverified.is_some(), "{logged:?}");
        assert_eq!(result.other_pc_key.as_deref(), Some("bogus-id"));
        // Authenticated marker + line: a proven continuation again.
        std::fs::remove_file(&planted).unwrap();
        audit::append_chained_jsonl_keyed(
            &planted,
            &json!({"event": "audio-level-lane-continued", "previous_lane": LOG_REL,
                "previous_lane_sha256": crate::sha256::digest_bytes(&damaged)})
            .to_string(),
            Some(&mine),
        )
        .unwrap();
        audit::append_chained_jsonl_keyed(
            &planted,
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            Some(&mine),
        )
        .unwrap();
        let (logged, statuses) = read_logged_results_with_keys(&dir, keys).unwrap();
        assert!(statuses[1].verification.is_ok(), "{statuses:?}");
        assert!(logged["vid_1"].0.chain_unverified.is_none(), "{logged:?}");
        assert_eq!(logged["vid_1"].1, 9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R7-I10: repairing a damaged lane by cutting it back to its verifying
    /// prefix no longer invalidates the genuine continuation lane — the
    /// marker recorded the prefix hash too. Any other edit still does.
    #[test]
    fn repaired_lane_keeps_its_continuation() {
        let r1 = row("vid_1");
        let dir = audio_case(
            "r7-i10",
            &[log_entry_json(
                7,
                "ffmpeg x",
                &[result_for(&r1, Verdict::Sound, -12.0)],
            )],
        );
        let lane0 = dir.join(LOG_REL);
        let intact = std::fs::read(&lane0).unwrap();
        let mut damaged = intact.clone();
        damaged.extend_from_slice(b"{\"torn\":");
        std::fs::write(&lane0, &damaged).unwrap();
        let next = active_lane_for_append(&dir).unwrap();
        let marker = std::fs::read_to_string(&next).unwrap();
        assert!(
            marker.contains("previous_lane_verified_prefix_sha256"),
            "{marker}"
        );
        audit::append_chained_jsonl_keyed(
            &next,
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            None,
        )
        .unwrap();
        assert_eq!(audio_levels_for_case(&dir).unwrap()["vid_1"].r, "silent");
        // Repaired: the torn tail removed.
        std::fs::write(&lane0, &intact).unwrap();
        assert_eq!(audio_levels_for_case(&dir).unwrap()["vid_1"].r, "silent");
        assert_ne!(chain_summary_json(&dir)["status"], "failed");
        assert_eq!(active_lane_for_append(&dir).unwrap(), next);
        // Any other change to the previous lane still breaks the link.
        let mut edited = intact.clone();
        edited.extend_from_slice(b"{}\n");
        std::fs::write(&lane0, &edited).unwrap();
        let summary = chain_summary_json(&dir);
        assert_eq!(summary["status"], "failed");
        assert!(
            summary["detail"]
                .as_str()
                .unwrap()
                .contains("직접 고치지 마십시오"),
            "{summary}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R8-I1 (P0): a keyed case whose lanes were re-chained with every HMAC
    /// stripped (and forged levels) no longer reads as an "unkeyed case"
    /// on a PC holding audit keys — R7 decided "keyed case?" from the lane
    /// text, so the stripped lane's levels were shown as measured. P1: on a
    /// PC without keys the same levels show, marked self-consistency only.
    #[test]
    fn r8_i1_stripped_hmacs_never_yield_levels_where_keys_are_held() {
        let r1 = row("vid_1");
        let dir = audio_case(
            "r8-i1",
            &[log_entry_json(
                9,
                "ffmpeg x",
                &[result_for(&r1, Verdict::Silent, -91.0)],
            )],
        );
        let mine = crate::audit_key::AuditKey::new("mine", &[0x81; 32]);
        let keys = std::slice::from_ref(&mine);
        let (logged, _) = read_logged_results_with_keys(&dir, keys).unwrap();
        let reason = logged["vid_1"].0.chain_unverified.as_deref().unwrap_or("");
        assert!(reason.contains("이 PC에서 인증되지 않음"), "{logged:?}");
        crate::audit_key::with_test_keys(keys, || {
            assert_eq!(
                audio_levels_for_case(&dir).unwrap()["vid_1"].r,
                "unverified"
            );
            assert_eq!(chain_summary_json(&dir)["status"], "not-verified");
        });
        // P1: no keys here — a level, flagged self-consistency only.
        crate::audit_key::with_test_keys(&[], || {
            let levels = audio_levels_for_case(&dir).unwrap();
            assert_eq!(levels["vid_1"].r, "silent");
            assert_eq!(levels["vid_1"].to_json()["sc"], true);
            assert_eq!(chain_summary_json(&dir)["status"], "structural");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R8-I6 (P2): a continuation lane opened on ANOTHER PC (marker and
    /// lines under its key) is not damage here: no new lane is started
    /// (R7 started one, and the other PC then started one for that —
    /// ping-pong), and its lines read "not authenticable here — check on
    /// the PC holding key <id>", never as measured.
    #[test]
    fn r8_i6_other_pcs_continuation_lane_is_not_damage_here() {
        let r1 = row("vid_1");
        let dir = audio_case(
            "r8-i6",
            &[log_entry_json(
                7,
                "ffmpeg x",
                &[result_for(&r1, Verdict::Sound, -12.0)],
            )],
        );
        let here = crate::audit_key::AuditKey::new("pc-a", &[0x82; 32]);
        let there = crate::audit_key::AuditKey::new("pc-b", &[0x83; 32]);
        let lane0 = dir.join(LOG_REL);
        let mut damaged = std::fs::read(&lane0).unwrap();
        damaged.extend_from_slice(b"{\"torn\":");
        std::fs::write(&lane0, &damaged).unwrap();
        // PC B continued the damaged lane under its own key.
        let lane1 = dir.join("evidence/logs/audio-level-log.1.jsonl");
        audit::append_chained_jsonl_keyed(
            &lane1,
            &json!({"event": "audio-level-lane-continued", "previous_lane": LOG_REL,
                "previous_lane_sha256": crate::sha256::digest_bytes(&damaged)})
            .to_string(),
            Some(&there),
        )
        .unwrap();
        audit::append_chained_jsonl_keyed(
            &lane1,
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            Some(&there),
        )
        .unwrap();
        crate::audit_key::with_test_keys(std::slice::from_ref(&here), || {
            assert_eq!(active_lane_for_append(&dir).unwrap(), lane1);
            let levels = audio_levels_for_case(&dir).unwrap();
            assert_eq!(levels["vid_1"].r, "unverified");
            assert_eq!(levels["vid_1"].to_json()["pc"], "pc-b");
            let summary = chain_summary_json(&dir);
            assert!(
                summary["detail"]
                    .as_str()
                    .unwrap()
                    .contains("\"pc-b\" 보유 PC에서 확인"),
                "{summary}"
            );
        });
        assert!(
            !dir.join("evidence/logs/audio-level-log.2.jsonl").exists(),
            "no new lane"
        );
        // PC B itself authenticates its lane and measurement.
        crate::audit_key::with_test_keys(std::slice::from_ref(&there), || {
            assert_eq!(active_lane_for_append(&dir).unwrap(), lane1);
            assert_eq!(audio_levels_for_case(&dir).unwrap()["vid_1"].r, "silent");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R8-I8: measurements written before audit keying was introduced for
    /// the lane (an authenticated `audit-key-introduced` marker follows
    /// them) stay levels — marked self-consistency only — instead of every
    /// earlier measurement turning "unverified" at the first rotation; and
    /// such a level never replaces one authenticated here.
    #[test]
    fn r8_i8_pre_key_measurements_stay_self_consistent_levels() {
        let r1 = row("vid_1");
        let r2 = row("vid_2");
        let dir = audio_case(
            "r8-i8",
            &[log_entry_json(
                7,
                "ffmpeg x",
                &[
                    result_for(&r1, Verdict::Sound, -12.0),
                    result_for(&r2, Verdict::Sound, -20.0),
                ],
            )],
        );
        let mine = crate::audit_key::AuditKey::new("mine", &[0x84; 32]);
        let keys = std::slice::from_ref(&mine);
        audit::append_chained_jsonl_keyed(
            &dir.join(LOG_REL),
            &json!({"kind": audit::KEY_INTRODUCED_KIND, "to": "mine"}).to_string(),
            Some(&mine),
        )
        .unwrap();
        let (logged, _) = read_logged_results_with_keys(&dir, keys).unwrap();
        assert!(logged["vid_1"].0.chain_unverified.is_none(), "{logged:?}");
        assert!(logged["vid_1"].0.self_consistent_only);
        // A later authenticated measurement of vid_2 wins over the pre-key
        // one and is not flagged.
        audit::append_chained_jsonl_keyed(
            &dir.join(LOG_REL),
            &log_entry_json(9, "ffmpeg x", &[result_for(&r2, Verdict::Silent, -91.0)]).to_string(),
            Some(&mine),
        )
        .unwrap();
        let (logged, _) = read_logged_results_with_keys(&dir, keys).unwrap();
        assert_eq!(logged["vid_2"].1, 9);
        assert!(!logged["vid_2"].0.self_consistent_only);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R14 I5: a measurement positioned before a usable `lane-repair`
    /// marker of its lane is not known to be the newest of its record
    /// (the cut tail may have held a newer one) — unverified; a
    /// measurement after the marker is a level again.
    #[test]
    fn r14_i5_measurement_before_a_lane_repair_is_unknown() {
        let r1 = row("vid_1");
        let dir = audio_case(
            "r14-i5",
            &[
                log_entry_json(7, "ffmpeg x", &[result_for(&r1, Verdict::Sound, -12.0)]),
                json!({"event": "lane-repair", "kept_entries": 1, "removed_lines": 1,
                    "removed_bytes": 10, "removed_sha256": "aa"}),
            ],
        );
        let (logged, _) = read_logged_results_with_keys(&dir, &[]).unwrap();
        let reason = logged["vid_1"].0.chain_unverified.as_deref().unwrap_or("");
        assert!(reason.contains(AUDIO_REPAIRED_AFTER_NOTE), "{logged:?}");
        audit::append_chained_jsonl_keyed(
            &dir.join(LOG_REL),
            &log_entry_json(9, "ffmpeg x", &[result_for(&r1, Verdict::Silent, -91.0)]).to_string(),
            None,
        )
        .unwrap();
        let (logged, _) = read_logged_results_with_keys(&dir, &[]).unwrap();
        assert!(logged["vid_1"].0.chain_unverified.is_none(), "{logged:?}");
        assert_eq!(logged["vid_1"].1, 9);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
