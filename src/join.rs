//! Lossless join of consecutive recordings (stream copy only).
//!
//! `plan` probes the selected indexed videos live (`ffprobe -show_data_hash
//! sha256`, so SPS/PPS extradata is compared too), orders them on ONE time
//! basis (file-name time, else `creation_time`, else natural file-name
//! order — never mixed, never mtime), classifies every junction against the
//! tolerance and lists everything that blocks a byte-exact `-c copy`
//! concat. `run` performs the join through ffmpeg's concat demuxer with
//! ASCII-only argv (`cwd = artifacts/joined`, every path lives in a UTF-8
//! list file), verifies the output, re-checks the sources, and only then
//! publishes output + sidecar manifest + one chained audit entry. Any
//! failure removes everything it created.
//!
//! Overlaps are never trimmed and gaps never filled: the manifest records
//! where each source sits in the output so nothing is silently altered.

use crate::audit;
use crate::rectime::{self, NaiveRecTime};
use crate::tool_policy::{command_version, resolve_tool_binary};
use crate::util::now_unix;
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

pub const DEFAULT_TOLERANCE_SECONDS: f64 = 1.0;
pub const MIN_TOLERANCE_SECONDS: f64 = 0.5;
pub const MAX_TOLERANCE_SECONDS: f64 = 5.0;
pub const MIN_SEGMENTS: usize = 2;
pub const MAX_SEGMENTS: usize = 500;
pub const MAX_ID_BYTES: usize = 512;
/// Live probes run at most this many at once.
pub const PROBE_PARALLELISM: usize = 4;
/// Output directory relative to the case.
pub const JOINED_DIR: &str = "artifacts/joined";
/// Chained audit lane inside [`JOINED_DIR`].
pub const LOG_FILE: &str = "join-log.jsonl";
/// Free space required before a join, relative to the summed source size.
pub const DISK_FACTOR: f64 = 1.05;
/// Exit code of `join-video` when the plan changed since the check.
pub const EXIT_PLAN_CHANGED: i32 = 3;
pub const PLAN_CHANGED_MESSAGE: &str = "검사 이후 파일이 바뀌었습니다 — 다시 검사하십시오";

const TIME_NOTE_FILENAME: &str = "파일명 시각은 시간대 없는 장치 기록값(naive local)";
const TIME_NOTE_CREATION: &str = "ffprobe creation_time 태그값(컨테이너에 기록된 값, 보통 UTC) — 장치 시계 정확성은 검증하지 않음";
const TIME_NOTE_ORDER: &str =
    "녹화 시각을 쓰지 않고 파일명 자연 정렬 순서로 이어붙임 — 끊김·겹침 확인 불가";
const NOT_CARRIED_DATA: &str = "data/subtitle streams (e.g. GPS telemetry)";
const NOT_CARRIED_VIDEO: &str = "additional video streams (only the first video stream is carried)";
const CAVEATS: [&str; 4] = [
    "시각은 장치가 기록한 값이며 장치 시계의 정확성은 검증하지 않았습니다.",
    "겹침 구간은 잘라내지 않았습니다 — 같은 장면이 두 번 나올 수 있습니다.",
    "출력 내 위치는 ffprobe가 잰 각 원본 길이의 누적합으로 계산했습니다.",
    "원본 SHA-256은 ffmpeg 실행 전에 계산했고, 실행 뒤에는 크기와 수정 시각으로만 변경 여부를 다시 확인했습니다.",
];

/// External tool names or paths (allow-listed through `tool_policy`).
#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: String,
    pub ffprobe: String,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".to_string(),
            ffprobe: "ffprobe".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderBasis {
    FilenameTime,
    CreationTime,
    FilenameOrder,
}

impl OrderBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FilenameTime => "filename-time",
            Self::CreationTime => "creation-time",
            Self::FilenameOrder => "filename-order",
        }
    }

    fn time_note(self) -> &'static str {
        match self {
            Self::FilenameTime => TIME_NOTE_FILENAME,
            Self::CreationTime => TIME_NOTE_CREATION,
            Self::FilenameOrder => TIME_NOTE_ORDER,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mkv,
}

impl Container {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
        }
    }

    fn ffmpeg_format(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "matroska",
        }
    }

    /// mp4 when the video is h264/hevc and audio is aac/mp3/absent — the
    /// mp4 muxer refuses other codecs (pcm, ...) under `-c copy`.
    pub fn choose(video_codec: Option<&str>, audio_codec: Option<&str>) -> Self {
        let video_ok = matches!(video_codec, Some("h264" | "hevc"));
        let audio_ok = matches!(audio_codec, None | Some("aac" | "mp3"));
        if video_ok && audio_ok {
            Self::Mp4
        } else {
            Self::Mkv
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JunctionKind {
    Continuous,
    Gap,
    Overlap,
    Unknown,
}

impl JunctionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continuous => "continuous",
            Self::Gap => "gap",
            Self::Overlap => "overlap",
            Self::Unknown => "unknown",
        }
    }
}

/// Junction classification: `|delta| <= tolerance` (inclusive) is
/// continuous, beyond it a gap (positive) or overlap (negative).
pub fn classify_junction(delta_seconds: f64, tolerance: f64) -> JunctionKind {
    const EPS: f64 = 1e-9;
    if delta_seconds.abs() <= tolerance + EPS {
        JunctionKind::Continuous
    } else if delta_seconds > 0.0 {
        JunctionKind::Gap
    } else {
        JunctionKind::Overlap
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct VideoParams {
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub pix_fmt: Option<String>,
    pub r_frame_rate: Option<String>,
    pub avg_frame_rate: Option<String>,
    pub time_base: Option<String>,
    pub extradata_hash: Option<String>,
    /// B6: decoder-relevant parameters ffprobe reports outside extradata,
    /// compared when no segment has an extradata hash: `(field, value)` in
    /// [`VIDEO_FALLBACK_FIELDS`] order.
    pub fallback: Vec<(&'static str, Option<String>)>,
}

/// B6: ffprobe stream fields compared (blocking) when the SPS/PPS hash is
/// unavailable for every segment — `(ffprobe key, Korean subject)`.
pub const VIDEO_FALLBACK_FIELDS: [(&str, &str); 11] = [
    ("level", "코덱 레벨이"),
    ("refs", "참조 프레임 수가"),
    ("has_b_frames", "B-프레임 설정이"),
    ("field_order", "필드 순서(인터레이스)가"),
    ("color_range", "색 범위가"),
    ("color_space", "색 공간이"),
    ("color_primaries", "색 원색 규격이"),
    ("color_transfer", "색 전달 특성이"),
    ("chroma_location", "색차 위치가"),
    ("is_avc", "비트스트림 형식(avcC/Annex B)이"),
    ("nal_length_size", "NAL 길이 크기가"),
];

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioParams {
    pub codec: Option<String>,
    pub sample_rate: Option<u64>,
    pub channels: Option<u64>,
    /// B5: compared too — a profile or AudioSpecificConfig change makes a
    /// copied stream undecodable after the junction.
    pub profile: Option<String>,
    pub extradata_hash: Option<String>,
}

/// What one live ffprobe run established about a file.
#[derive(Debug, Clone, Default)]
pub struct ProbeFacts {
    pub ok: bool,
    pub error: Option<String>,
    pub duration_seconds: Option<f64>,
    /// First video stream.
    pub video: Option<VideoParams>,
    pub video_streams: usize,
    pub audio: Vec<AudioParams>,
    /// Data / subtitle / attachment streams (not carried by the join).
    pub other_streams: usize,
    /// Raw `format.tags.creation_time`.
    pub creation_time: Option<String>,
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|text| !text.is_empty())
}

/// A scalar ffprobe field as text (numbers and strings alike).
fn any_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn num_field(value: &Value, key: &str) -> Option<u64> {
    let field = value.get(key)?;
    field
        .as_u64()
        .or_else(|| field.as_str().and_then(|text| text.trim().parse().ok()))
}

/// Maps ffprobe `-show_streams -show_format [-show_data_hash]` JSON.
pub fn parse_probe_facts(raw: &str) -> ProbeFacts {
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(err) => {
            return ProbeFacts {
                error: Some(format!("invalid ffprobe JSON output: {err}")),
                ..ProbeFacts::default()
            };
        }
    };
    let mut facts = ProbeFacts {
        ok: true,
        ..ProbeFacts::default()
    };
    let empty = Vec::new();
    let streams = parsed
        .get("streams")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    for stream in streams {
        match stream.get("codec_type").and_then(Value::as_str) {
            Some("video") => {
                facts.video_streams += 1;
                if facts.video.is_none() {
                    facts.video = Some(VideoParams {
                        codec: str_field(stream, "codec_name"),
                        profile: str_field(stream, "profile"),
                        width: num_field(stream, "width"),
                        height: num_field(stream, "height"),
                        pix_fmt: str_field(stream, "pix_fmt"),
                        r_frame_rate: str_field(stream, "r_frame_rate"),
                        avg_frame_rate: str_field(stream, "avg_frame_rate"),
                        time_base: str_field(stream, "time_base"),
                        extradata_hash: str_field(stream, "extradata_hash"),
                        fallback: VIDEO_FALLBACK_FIELDS
                            .iter()
                            .map(|(key, _)| (*key, any_field(stream, key)))
                            .collect(),
                    });
                }
            }
            Some("audio") => facts.audio.push(AudioParams {
                codec: str_field(stream, "codec_name"),
                sample_rate: num_field(stream, "sample_rate"),
                channels: num_field(stream, "channels"),
                profile: str_field(stream, "profile"),
                extradata_hash: str_field(stream, "extradata_hash"),
            }),
            _ => facts.other_streams += 1,
        }
    }
    let format = parsed.get("format");
    facts.duration_seconds = format
        .and_then(|f| f.get("duration"))
        .and_then(Value::as_str)
        .and_then(|text| text.trim().parse::<f64>().ok())
        .or_else(|| {
            streams
                .iter()
                .find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video"))
                .and_then(|s| s.get("duration"))
                .and_then(Value::as_str)
                .and_then(|text| text.trim().parse::<f64>().ok())
        })
        .filter(|value| value.is_finite() && *value > 0.0);
    facts.creation_time = format
        .and_then(|f| f.get("tags"))
        .and_then(Value::as_object)
        .and_then(|tags| {
            tags.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("creation_time"))
                .and_then(|(_, value)| value.as_str().map(str::to_string))
        });
    facts
}

/// Runs a live ffprobe (with the SHA-256 extradata hash) on one file.
pub fn probe_file(ffprobe: &str, path: &Path) -> ProbeFacts {
    let failed = |error: String| ProbeFacts {
        error: Some(error),
        ..ProbeFacts::default()
    };
    let binary = match resolve_tool_binary(ffprobe, &["ffprobe"]) {
        Ok(binary) => binary,
        Err(err) => return failed(err),
    };
    let mut command = Command::new(&binary);
    command
        .args(["-v", "error", "-print_format", "json"])
        .args(["-show_streams", "-show_format", "-show_data_hash", "sha256"])
        .arg(path);
    match crate::util::run_with_timeout(&mut command, Some(crate::util::PROBE_TIMEOUT_SECS)) {
        Ok(output) if output.status.success() => {
            parse_probe_facts(&String::from_utf8_lossy(&output.stdout))
        }
        Ok(output) => failed(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(err) => failed(format!("failed to run ffprobe: {err}")),
    }
}

/// C14: allowed |observed − expected| output duration: max(1 s, 0.2 %).
pub fn duration_tolerance(expected_seconds: f64) -> f64 {
    (expected_seconds * 0.002).max(1.0)
}

/// C14: packet-level verification of a joined output against its plan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SegmentCheck {
    pub video_packets_expected: u64,
    pub video_packets_observed: u64,
    pub audio_packets_expected: Option<u64>,
    pub audio_packets_observed: Option<u64>,
    /// `(1-based segment, expected start s, observed start s)` per
    /// boundary that could be located in the output.
    pub boundaries: Vec<(usize, f64, Option<f64>)>,
    /// R5-S13: per segment, the seconds of edit-list pre-roll (frames the
    /// source's edit list hides) that the output PRESENTS — stream copy
    /// through the concat demuxer can keep those packets as visible frames.
    pub presented_preroll_seconds: Vec<f64>,
    /// R5-S13: how output packets were matched to the sources:
    /// `all-packets` (every source packet, pre-roll included) or
    /// `edit-list-applied` (the pre-roll packets were dropped).
    pub mode: &'static str,
    pub ok: bool,
    pub problems: Vec<String>,
    /// R5-S13: facts the manifest states next to a passing check.
    pub notes: Vec<String>,
}

impl SegmentCheck {
    pub fn to_json(&self) -> Value {
        json!({
            "video_packets_expected": self.video_packets_expected,
            "video_packets_observed": self.video_packets_observed,
            "audio_packets_expected": self.audio_packets_expected,
            "audio_packets_observed": self.audio_packets_observed,
            "mode": self.mode,
            "boundaries": self.boundaries.iter().enumerate().map(|(at, (index, expected, observed))| json!({
                "segment": index,
                "expected_start_seconds": r3(*expected),
                "observed_start_seconds": observed.map(r3),
                "presented_edit_list_preroll_seconds": r3(self.presented_preroll_seconds.get(at).copied().unwrap_or(0.0)),
            })).collect::<Vec<_>>(),
            "ok": self.ok,
            "problems": self.problems,
            "notes": self.notes,
        })
    }

    /// Seconds the output is longer than the plan because edit-list
    /// pre-roll is presented (R5-S13).
    pub fn presented_preroll_total(&self) -> f64 {
        self.presented_preroll_seconds.iter().sum()
    }
}

/// R5-S13: one video packet as ffprobe lists it (file order). `pts`/`dts`
/// are `None` where ffprobe reports `N/A` (Matroska stores no dts for the
/// first packets of a B-frame stream); `discard` is the `D` flag (outside
/// the edit list).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VideoPacket {
    pub pts: Option<f64>,
    pub dts: Option<f64>,
    pub discard: bool,
    /// R6-S6: the `K` flag (key frame).
    pub key: bool,
    /// R6-S6: the first 64 bits of the packet payload's SHA-256
    /// (`-show_data_hash sha256`), `None` when ffprobe gave none.
    pub hash: Option<u64>,
}

/// R5-S13: a source's packet facts for the segment check.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourcePackets {
    /// Every video packet in the file.
    pub video_total: u64,
    /// Video packets not flagged discard (inside the edit list).
    pub video_kept: u64,
    /// Per video packet (file order): flagged discard by the source's edit
    /// list. Stream copy keeps packet order, so packet `i` of the source
    /// is packet `start + i` of its output range.
    pub discard_mask: Vec<bool>,
    pub audio: Option<u64>,
    /// R6-S6: payload hashes of the KEPT, non-key packets in file order —
    /// what an output that applied the edit list must carry for this
    /// segment. Key frames are left out: stream copy may rewrite them
    /// (ffmpeg re-inserts parameter sets), every other packet is copied
    /// byte for byte. `None` when a hash is missing (cannot confirm).
    pub kept_fingerprint: Option<Vec<u64>>,
    /// R7-S4: payload hashes of EVERY video packet in file order (key
    /// frames and pre-roll included), `None` when one is missing. An
    /// all-intra source has no non-key packets — its kept fingerprint is
    /// empty — and only these tell an applied edit list from a kept
    /// pre-roll.
    pub packet_hashes: Option<Vec<u64>>,
}

impl SourcePackets {
    /// Facts from a source's video packet list plus its audio count.
    pub fn from_packets(packets: &[VideoPacket], audio: Option<u64>) -> Self {
        let discard_mask: Vec<bool> = packets.iter().map(|p| p.discard).collect();
        SourcePackets {
            video_total: packets.len() as u64,
            video_kept: discard_mask.iter().filter(|d| !**d).count() as u64,
            discard_mask,
            audio,
            kept_fingerprint: nonkey_hashes(packets.iter().filter(|p| !p.discard)),
            packet_hashes: packets.iter().map(|p| p.hash).collect(),
        }
    }

    /// Plain counts (no edit list).
    pub fn plain(video: u64, audio: Option<u64>) -> Self {
        SourcePackets {
            video_total: video,
            video_kept: video,
            discard_mask: vec![false; video as usize],
            audio,
            kept_fingerprint: None,
            packet_hashes: None,
        }
    }
}

/// R6-S6: hashes of the non-key packets, in order (`None` if any lacks
/// one).
fn nonkey_hashes<'a>(packets: impl Iterator<Item = &'a VideoPacket>) -> Option<Vec<u64>> {
    packets.filter(|p| !p.key).map(|p| p.hash).collect()
}

/// Packet counts of the first video and first audio stream
/// (`ffprobe -count_packets`, which demuxes the whole file).
pub fn stream_packet_counts(
    ffprobe: &str,
    path: &Path,
    timeout_secs: Option<u64>,
) -> Result<(u64, Option<u64>), String> {
    let binary = resolve_tool_binary(ffprobe, &["ffprobe"])?;
    let mut command = Command::new(&binary);
    command
        .args(["-v", "error", "-count_packets", "-print_format", "json"])
        .args(["-show_entries", "stream=codec_type,nb_read_packets"])
        .arg(path);
    let output = crate::util::run_with_timeout(&mut command, timeout_secs)?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe packet count failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("invalid ffprobe packet JSON: {err}"))?;
    let streams = value
        .get("streams")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let first = |kind: &str| {
        streams
            .iter()
            .find(|s| s.get("codec_type").and_then(Value::as_str) == Some(kind))
            .and_then(|s| num_field(s, "nb_read_packets"))
    };
    let video = first("video").ok_or_else(|| {
        format!(
            "ffprobe could not count video packets of {}",
            path.display()
        )
    })?;
    Ok((video, first("audio")))
}

/// R5-S13: parses ffprobe `-show_entries packet=pts_time,dts_time,flags
/// -of compact=p=0` output (`pts_time=…|dts_time=…|flags=…` per line).
pub fn parse_video_packets(text: &str) -> Vec<VideoPacket> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut packet = VideoPacket::default();
        let mut any = false;
        for field in line.split('|') {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let time = || value.trim().parse::<f64>().ok().filter(|v| v.is_finite());
            match key.trim() {
                "pts_time" => {
                    packet.pts = time();
                    any = true;
                }
                "dts_time" => {
                    packet.dts = time();
                    any = true;
                }
                "flags" => {
                    packet.discard = value.contains('D');
                    packet.key = value.contains('K');
                    any = true;
                }
                "data_hash" => {
                    // `SHA256:<hex>`: the first 16 hex digits.
                    let hex = value.trim().rsplit(':').next().unwrap_or("");
                    packet.hash = hex.get(..16).and_then(|h| u64::from_str_radix(h, 16).ok());
                }
                _ => {}
            }
        }
        if any {
            out.push(packet);
        }
    }
    out
}

/// R5-S13: the first video stream's packets (pts, dts, discard flag) in
/// file order. R7-S3: payload hashes only `with_hashes` — hashing every
/// packet of every source and of the output on each join read all the
/// material once more for a check that rarely needs it.
pub fn video_packets(
    ffprobe: &str,
    path: &Path,
    timeout_secs: Option<u64>,
    with_hashes: bool,
) -> Result<Vec<VideoPacket>, String> {
    let binary = resolve_tool_binary(ffprobe, &["ffprobe"])?;
    let mut command = Command::new(&binary);
    command.args(video_packet_args(with_hashes)).arg(path);
    let output = crate::util::run_with_timeout(&mut command, timeout_secs)?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe packet listing failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(parse_video_packets(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// R7-S3: the ffprobe arguments (before the path) of [`video_packets`].
pub fn video_packet_args(with_hashes: bool) -> Vec<&'static str> {
    let mut args = vec!["-v", "error", "-select_streams", "v:0"];
    if with_hashes {
        // R6-S6: payload hashes confirm an edit-list-applied output.
        args.extend(["-show_data_hash", "sha256"]);
        args.extend(["-show_entries", "packet=pts_time,dts_time,flags,data_hash"]);
    } else {
        args.extend(["-show_entries", "packet=pts_time,dts_time,flags"]);
    }
    args.extend(["-of", "compact=p=0"]);
    args
}

/// R9-S2: the bitstream filters that make a packet payload comparable
/// across what stream copy does to it: Annex B framing (the concat
/// demuxer's `auto_convert` runs `h264_mp4toannexb`, a TS output is Annex
/// B, an MP4 output length-prefixes again) and parameter sets / access
/// unit delimiters (re-inserted before IDR frames, added by the TS muxer).
/// The picture data (slices, SEI) is untouched. `None` for codecs stream
/// copy does not rewrite (their raw payloads are compared).
pub fn normalizing_bsf(codec: Option<&str>) -> Option<&'static str> {
    match codec {
        Some("h264") => Some("h264_mp4toannexb,filter_units=remove_types=7|8|9"),
        Some("hevc") => Some("hevc_mp4toannexb,filter_units=remove_types=32|33|34|35"),
        _ => None,
    }
}

/// R10-S8: [`normalizing_bsf`] in its two parts — the Annex B filter and
/// the NAL unit types it removes — for the parameter-set-only proof of
/// [`align_normalized_hashes`].
pub fn normalizing_parts(codec: Option<&str>) -> Option<(&'static str, &'static str)> {
    match codec {
        Some("h264") => Some(("h264_mp4toannexb", "7|8|9")),
        Some("hevc") => Some(("hevc_mp4toannexb", "32|33|34|35")),
        _ => None,
    }
}

/// R10-S8: the hash a packet whose payload normalization removed entirely
/// (only parameter sets / access unit delimiters) stands for: the first
/// 64 bits of the SHA-256 of the empty payload.
pub const EMPTY_PAYLOAD_HASH: u64 = 0xe3b0_c442_98fc_1c14;

/// R9-S2: the ffmpeg arguments that print one SHA-256 per video packet of
/// `path` (stream copy through [`normalizing_bsf`], `framehash` muxer).
///
/// R10-S8: `-copyinkf` — stream copy otherwise drops every packet before
/// the first key frame, so a clip that starts on a non-key frame (cut
/// mid-GOP: CCTV exports, a byte-cut MPEG-TS) printed fewer hashes than
/// ffprobe lists packets and every join of it was refused as "cannot
/// verify". `-copyts` keeps the demuxer's timestamps (in the stream's own
/// time base, as ffprobe lists them), so a packet the filter drops can be
/// located ([`align_normalized_hashes`]).
pub fn normalized_hash_args(path: &Path, bsf: Option<&str>) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = ["-v", "error", "-nostdin", "-copyts", "-i"]
        .iter()
        .map(Into::into)
        .collect();
    args.push(path.as_os_str().to_os_string());
    for arg in ["-map", "0:v:0", "-c", "copy", "-copyinkf"] {
        args.push(arg.into());
    }
    if let Some(bsf) = bsf {
        args.push("-bsf:v".into());
        args.push(bsf.into());
    }
    for arg in ["-f", "framehash", "-hash", "sha256", "-"] {
        args.push(arg.into());
    }
    args
}

/// R10-S8: one `framehash` line — `stream, dts, pts, duration, size,
/// hash[, side data…]` — with the timestamps (`None` for ffmpeg's
/// no-timestamp value) and the first 64 bits of the hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHashLine {
    pub dts: Option<i64>,
    pub pts: Option<i64>,
    pub hash: u64,
}

/// R10-S8: every packet line of `framehash` output (`#` lines are
/// headers). `None` when a line has no readable timestamps or hash.
pub fn parse_framehash_lines(text: &str) -> Option<Vec<FrameHashLine>> {
    let stamp = |field: &str| -> Option<Option<i64>> {
        let value: i64 = field.trim().parse().ok()?;
        Some((value != i64::MIN).then_some(value))
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            let hash = fields.get(5)?.trim();
            Some(FrameHashLine {
                dts: stamp(fields.get(1)?)?,
                pts: stamp(fields.get(2)?)?,
                hash: hash
                    .get(..16)
                    .and_then(|h| u64::from_str_radix(h, 16).ok())?,
            })
        })
        .collect()
}

/// R9-S2: the first 64 bits of each packet hash of `framehash` output.
/// `None` when a line has no readable hash.
pub fn parse_framehash(text: &str) -> Option<Vec<u64>> {
    parse_framehash_lines(text).map(|lines| lines.iter().map(|line| line.hash).collect())
}

/// R10-S8: per packet (file order) its timestamps in the stream's own
/// time base, as ffprobe lists them (`None` = `N/A`).
///
/// R11-S1: a struct with named fields, read from ffprobe's `key=value`
/// output. R10 kept an unnamed `(dts, pts)` pair parsed by position, but
/// ffprobe prints `pts` before `dts` whatever order `-show_entries` names
/// them in: with B-frames (pts ≠ dts) every packet then "differed from
/// ffmpeg's timestamps" and every such source was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PacketStamp {
    pub pts: Option<i64>,
    pub dts: Option<i64>,
}

/// R11-S1: which timestamp identifies a packet in
/// [`align_normalized_hashes`]: the presentation time (unique per
/// packet, and what Matroska always stores — it reports no dts for the
/// first packets of a B-frame stream, where ffmpeg's framehash computes
/// one), or the decode time for a stream with no pts at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StampKey {
    Pts,
    Dts,
}

impl StampKey {
    fn of_stamp(self, stamp: &PacketStamp) -> Option<i64> {
        match self {
            StampKey::Pts => stamp.pts,
            StampKey::Dts => stamp.dts,
        }
    }

    fn of_line(self, line: &FrameHashLine) -> Option<i64> {
        match self {
            StampKey::Pts => line.pts,
            StampKey::Dts => line.dts,
        }
    }
}

/// R10-S8: one hash per listed packet when the normalizing filter dropped
/// some packets (`normalized` is shorter than `stamps`).
///
/// `filter_units` drops a packet when nothing is left of it — it carried
/// only parameter sets / access unit delimiters (some DVRs write SPS/PPS
/// as packets of their own). R9 refused every such file ("hashed N
/// packets but ffprobe lists M"). A dropped packet is taken as
/// [`EMPTY_PAYLOAD_HASH`] only when that is PROVEN: `framed` (the same
/// packets through the Annex B filter and a no-op unit filter, so
/// serialized the same way) lists every packet with these timestamps, and
/// `ps_only` (only the removed unit types kept) carries exactly the same
/// bytes for it. Anything else — a timestamp that is missing, repeated or
/// not found, a dropped packet that had picture data — is an error (the
/// check cannot run; the join is refused, never passed unverified).
///
/// R11-S1: packets are identified by pts alone (by dts only when the
/// stream lists no pts at all); a dts ffprobe lists must still equal
/// ffmpeg's, one it reports as `N/A` is not compared.
pub fn align_normalized_hashes(
    stamps: &[PacketStamp],
    normalized: &[FrameHashLine],
    framed: &[FrameHashLine],
    ps_only: &[FrameHashLine],
) -> Result<Vec<u64>, String> {
    if framed.len() != stamps.len() {
        return Err(format!(
            "the unfiltered pass hashed {} packets but ffprobe lists {}",
            framed.len(),
            stamps.len()
        ));
    }
    let key = if stamps.iter().all(|s| s.pts.is_some()) {
        StampKey::Pts
    } else if stamps.iter().all(|s| s.pts.is_none() && s.dts.is_some()) {
        StampKey::Dts
    } else {
        let index = stamps
            .iter()
            .position(|s| s.pts.is_none())
            .unwrap_or_default();
        return Err(format!(
            "packet {} has no presentation timestamp",
            index + 1
        ));
    };
    let mut seen = std::collections::HashSet::new();
    for (index, (stamp, line)) in stamps.iter().zip(framed).enumerate() {
        let Some(value) = key.of_stamp(stamp) else {
            return Err(format!("packet {} has no timestamp", index + 1));
        };
        if !seen.insert(value) {
            return Err(format!("packet {} repeats a timestamp", index + 1));
        }
        let differs = line.pts != stamp.pts && (key == StampKey::Pts || stamp.pts.is_some())
            || stamp.dts.is_some() && line.dts != stamp.dts;
        if differs {
            return Err(format!(
                "packet {}: ffmpeg's timestamps (pts {:?}, dts {:?}) differ from ffprobe's (pts {:?}, dts {:?})",
                index + 1,
                line.pts,
                line.dts,
                stamp.pts,
                stamp.dts
            ));
        }
    }
    let ps_only: std::collections::HashMap<i64, u64> = ps_only
        .iter()
        .filter_map(|line| Some((key.of_line(line)?, line.hash)))
        .collect();
    let mut out = Vec::with_capacity(stamps.len());
    let mut next = normalized.iter().peekable();
    for (index, (stamp, line)) in stamps.iter().zip(framed).enumerate() {
        let value = key.of_stamp(stamp);
        match next.peek() {
            Some(kept) if key.of_line(kept) == value => {
                out.push(kept.hash);
                next.next();
            }
            _ if value.and_then(|v| ps_only.get(&v)) == Some(&line.hash) => {
                out.push(EMPTY_PAYLOAD_HASH)
            }
            _ => {
                return Err(format!(
                    "packet {} was dropped by the normalizing filter but is not parameter sets only",
                    index + 1
                ));
            }
        }
    }
    if next.next().is_some() {
        return Err("the normalizing filter printed a packet ffprobe does not list".to_string());
    }
    Ok(out)
}

/// One `framehash` pass of `path` through `bsf`.
fn framehash_lines(
    ffmpeg: &str,
    path: &Path,
    bsf: Option<&str>,
    timeout_secs: Option<u64>,
) -> Result<Vec<FrameHashLine>, String> {
    let binary = resolve_tool_binary(ffmpeg, &["ffmpeg"])?;
    let mut command = Command::new(&binary);
    command.args(normalized_hash_args(path, bsf));
    let output = crate::util::run_with_timeout(&mut command, timeout_secs)?;
    if !output.status.success() {
        return Err(format!(
            "ffmpeg packet hashing failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_framehash_lines(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        format!(
            "ffmpeg packet hashing of {} printed an unreadable line",
            path.display()
        )
    })
}

/// R10-S8: the first video stream's packet timestamps in its own time
/// base. R11-S1: printed as `key=value` (`csv=p=0:nk=0`) and parsed by
/// name — ffprobe's field order is its own, not the `-show_entries`
/// order.
pub fn packet_stamps(
    ffprobe: &str,
    path: &Path,
    timeout_secs: Option<u64>,
) -> Result<Vec<PacketStamp>, String> {
    let binary = resolve_tool_binary(ffprobe, &["ffprobe"])?;
    let mut command = Command::new(&binary);
    command.args(packet_stamp_args()).arg(path);
    let output = crate::util::run_with_timeout(&mut command, timeout_secs)?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe packet timestamps failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_packet_stamps(&String::from_utf8_lossy(&output.stdout)).map_err(|why| {
        format!(
            "ffprobe packet timestamps of {} are unreadable: {why}",
            path.display()
        )
    })
}

/// R11-S1: the ffprobe arguments (before the path) of [`packet_stamps`].
pub fn packet_stamp_args() -> [&'static str; 8] {
    [
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "packet=pts,dts",
        "-of",
        "csv=p=0:nk=0",
    ]
}

/// R11-S1: `pts=…,dts=…` per line, in any order (`N/A` = none). A line
/// without both keys, or with a value that is neither a number nor
/// `N/A`, is an error — never read by position.
pub fn parse_packet_stamps(text: &str) -> Result<Vec<PacketStamp>, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, line)| {
            let mut pts = None;
            let mut dts = None;
            for field in line.split(',').map(str::trim).filter(|f| !f.is_empty()) {
                let (key, value) = field
                    .split_once('=')
                    .ok_or_else(|| format!("line {}: {field:?} has no key", index + 1))?;
                let value = match value.trim() {
                    "N/A" => None,
                    number => Some(number.parse::<i64>().map_err(|_| {
                        format!("line {}: {key}={number:?} is not a timestamp", index + 1)
                    })?),
                };
                match key.trim() {
                    "pts" => pts = Some(value),
                    "dts" => dts = Some(value),
                    _ => {}
                }
            }
            match (pts, dts) {
                (Some(pts), Some(dts)) => Ok(PacketStamp { pts, dts }),
                _ => Err(format!("line {}: {line:?} lacks pts or dts", index + 1)),
            }
        })
        .collect()
}

/// R9-S2: [`video_packets`] with each packet's hash taken from its
/// NORMALIZED payload ([`normalizing_bsf`]) instead of its raw bytes.
///
/// R8 compared raw payload hashes, which stream copy changes for every
/// IDR frame (parameter sets re-inserted, Annex B framing), so an
/// all-intra segment could only be judged "rewritten" (no comparison —
/// and a first packet that was no source packet passed unchecked) or
/// compared raw (a mix of rewritten IDR frames and untouched frames was
/// refused). Normalized, the same picture data compares equal whatever
/// the muxer did, and every position is compared exactly.
///
/// R10-S8: leading non-key packets are hashed too (`-copyinkf`), and a
/// packet the filter empties (parameter sets only) is accounted for by
/// [`align_normalized_hashes`] instead of failing the count.
pub fn normalized_video_packets(
    ffprobe: &str,
    ffmpeg: &str,
    path: &Path,
    timeout_secs: Option<u64>,
) -> Result<Vec<VideoPacket>, String> {
    let mut packets = video_packets(ffprobe, path, timeout_secs, false)?;
    let codec = video_codec_name(ffprobe, path, timeout_secs)?;
    let lines = framehash_lines(
        ffmpeg,
        path,
        normalizing_bsf(codec.as_deref()),
        timeout_secs,
    )?;
    let mismatch = |why: String| {
        format!(
            "ffmpeg hashed {} video packets of {} but ffprobe lists {}{why}",
            lines.len(),
            path.display(),
            packets.len()
        )
    };
    let hashes: Vec<u64> = match normalizing_parts(codec.as_deref()) {
        _ if lines.len() == packets.len() => lines.iter().map(|line| line.hash).collect(),
        Some((annexb, types)) if lines.len() < packets.len() => {
            let stamps = packet_stamps(ffprobe, path, timeout_secs)?;
            if stamps.len() != packets.len() {
                return Err(mismatch(format!(
                    " (and {} in its timestamp listing)",
                    stamps.len()
                )));
            }
            let framed = framehash_lines(
                ffmpeg,
                path,
                Some(&format!("{annexb},filter_units=remove_types=0")),
                timeout_secs,
            )?;
            let ps_only = framehash_lines(
                ffmpeg,
                path,
                Some(&format!("{annexb},filter_units=pass_types={types}")),
                timeout_secs,
            )?;
            align_normalized_hashes(&stamps, &lines, &framed, &ps_only)
                .map_err(|why| mismatch(format!(" ({why})")))?
        }
        _ => return Err(mismatch(String::new())),
    };
    for (packet, hash) in packets.iter_mut().zip(hashes) {
        packet.hash = Some(hash);
    }
    Ok(packets)
}

/// R9-S2: the first video stream's codec name (`None` when ffprobe names
/// none).
fn video_codec_name(
    ffprobe: &str,
    path: &Path,
    timeout_secs: Option<u64>,
) -> Result<Option<String>, String> {
    let binary = resolve_tool_binary(ffprobe, &["ffprobe"])?;
    let mut command = Command::new(&binary);
    command
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=codec_name", "-of", "csv=p=0"])
        .arg(path);
    let output = crate::util::run_with_timeout(&mut command, timeout_secs)?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe codec query failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string))
}

fn min_time(times: impl Iterator<Item = f64>) -> Option<f64> {
    times.fold(None, |acc: Option<f64>, t| {
        Some(acc.map_or(t, |a| a.min(t)))
    })
}

/// Pure C14 decision. `sources`: per segment (output order) its packet
/// facts; `offsets`: each segment's planned output start; `output_audio`:
/// the output's audio packet count; `output`: the output's video packets
/// in file order.
///
/// Missing material shows up as a video packet deficit (stream copy moves
/// every packet, so the counts must be equal) or as a segment that does
/// not start where the plan puts it (± max(1 s, 0.2 %)). Audio is allowed
/// one packet of slack per segment (encoder priming at a junction).
///
/// R5-S13 — what made it refuse correct joins:
/// - A segment's start is now the EARLIEST PRESENTATION time (min pts) of
///   its packets, not the dts of its first packet: Matroska reports no
///   dts (`N/A`) for the first packets of a B-frame stream (they used to
///   repeat the previous value), and decode order is not presentation
///   order.
/// - Packets a source's edit list discards (`D` flag) are accounted for:
///   the output may carry every source packet (the concat demuxer keeps
///   them, as visible frames — the following segments then start later by
///   the pre-roll the output really presents, measured from the output)
///   or only the kept ones (`edit-list-applied`). Either count is
///   accepted; anything else is still a failure.
pub fn evaluate_segment_check(
    sources: &[SourcePackets],
    offsets: &[f64],
    output_audio: Option<u64>,
    output: &[VideoPacket],
) -> SegmentCheck {
    let total: u64 = sources.iter().map(|s| s.video_total).sum();
    let kept: u64 = sources.iter().map(|s| s.video_kept).sum();
    let observed = output.len() as u64;
    // R6-S6: a count equal to "every packet minus the edit-list pre-roll"
    // is not proof by itself — an output that KEPT the pre-roll and lost
    // as many real frames elsewhere has the same count (and, rebased to
    // zero, the same timing). `edit-list-applied` is accepted only when
    // each edited segment's output packets carry exactly the source's
    // kept packets (payload hashes of the non-key packets, in order —
    // the first output frame of the segment is the first kept frame).
    let mut unconfirmed_applied: Option<String> = None;
    if observed != total && observed == kept {
        unconfirmed_applied = edit_list_applied_mismatch(sources, output);
    }
    let dropped_mode = observed != total && observed == kept && unconfirmed_applied.is_none();
    let mut check = SegmentCheck {
        video_packets_expected: if dropped_mode { kept } else { total },
        video_packets_observed: observed,
        audio_packets_observed: output_audio,
        mode: if dropped_mode {
            "edit-list-applied"
        } else {
            "all-packets"
        },
        ..SegmentCheck::default()
    };
    if sources.iter().all(|s| s.audio.is_some()) && !sources.is_empty() {
        check.audio_packets_expected = Some(sources.iter().filter_map(|s| s.audio).sum());
    }
    if let Some(why) = unconfirmed_applied {
        check.problems.push(format!(
            "출력 영상 패킷 수({observed})가 편집 목록 제외 합계와 같지만, 출력이 원본의 유지 프레임을 그대로 담았는지 확인하지 못했습니다 ({why}) — 실제 누락일 수 있어 통과시키지 않습니다"
        ));
    } else if observed != total && observed != kept {
        check.problems.push(if kept == total {
            format!("영상 패킷 수가 원본 합계와 다릅니다 (출력 {observed}, 원본 합계 {total})")
        } else {
            format!(
                "영상 패킷 수가 원본 합계와 다릅니다 (출력 {observed}, 원본 합계 {total} — 편집 목록 제외 시 {kept})"
            )
        });
    }
    match (check.audio_packets_expected, output_audio) {
        (Some(expected), Some(observed)) => {
            if expected.abs_diff(observed) > sources.len() as u64 {
                check.problems.push(format!(
                    "소리 패킷 수가 원본 합계와 다릅니다 (출력 {observed}, 원본 합계 {expected})"
                ));
            }
        }
        (Some(expected), None) => check.problems.push(format!(
            "원본에 소리가 있는데 출력에 소리 트랙이 없습니다 (원본 패킷 {expected})"
        )),
        _ => {}
    }
    // Each segment's packet range in the output: its earliest presented
    // time (non-discard output packets; dts only where no pts exists at
    // all) and the pre-roll it presents.
    let mut starts: Vec<Option<f64>> = Vec::with_capacity(sources.len());
    let mut start_index = 0usize;
    for source in sources {
        let count = if dropped_mode {
            source.video_kept
        } else {
            source.video_total
        } as usize;
        let begin = start_index.min(output.len());
        let end = start_index.saturating_add(count).min(output.len());
        let range = &output[begin..end];
        // In `all-packets` mode, output packet `i` of the range is source
        // packet `i`: was it the source's edit-list pre-roll?
        let pre_roll =
            |i: usize| !dropped_mode && source.discard_mask.get(i).copied().unwrap_or(false);
        let presented_start = min_time(range.iter().filter(|p| !p.discard).filter_map(|p| p.pts))
            .or_else(|| min_time(range.iter().filter(|p| !p.discard).filter_map(|p| p.dts)));
        let kept_start = min_time(
            range
                .iter()
                .enumerate()
                .filter(|(i, p)| !p.discard && !pre_roll(*i))
                .filter_map(|(_, p)| p.pts.or(p.dts)),
        );
        starts.push(presented_start);
        // How much earlier than the source's first kept frame the
        // segment's first visible frame is: the concat demuxer may keep
        // pre-roll frames at their own times or squeeze them together —
        // measured on the output, never assumed.
        let presented = match (presented_start, kept_start) {
            (Some(first), Some(kept)) if kept > first => kept - first,
            _ => 0.0,
        };
        check.presented_preroll_seconds.push(presented);
        start_index = start_index.saturating_add(count);
    }
    let origin = starts.first().copied().flatten().unwrap_or(0.0);
    let base = offsets.first().copied().unwrap_or(0.0);
    let mut shift = 0.0f64;
    for (index, observed) in starts.iter().enumerate() {
        let expected = offsets.get(index).copied().unwrap_or(0.0) - base + shift;
        let observed = observed.map(|start| start - origin);
        check.boundaries.push((index + 1, expected, observed));
        match observed {
            Some(observed) if (observed - expected).abs() <= duration_tolerance(expected) => {}
            Some(observed) => check.problems.push(format!(
                "{}번째 원본이 출력의 예상 위치에서 시작하지 않습니다 (예상 {expected:.3}초, 관측 {observed:.3}초)",
                index + 1
            )),
            None => check.problems.push(format!(
                "{}번째 원본의 영상 패킷이 출력에 없습니다 (또는 시각 정보가 없어 위치를 확인할 수 없습니다)",
                index + 1
            )),
        }
        let presented = check.presented_preroll_seconds[index];
        if presented >= 0.001 {
            check.notes.push(format!(
                "{}번째 원본의 편집 목록(edit list)이 가리던 앞부분 {presented:.3}초가 출력에 그대로 들어갔습니다 — 이후 원본의 출력 내 위치는 이만큼 뒤로 밀리며, 정확한 위치는 segment_check.boundaries의 관측값입니다.",
                index + 1
            ));
        }
        shift += presented;
    }
    if sources.is_empty() {
        check.problems.push("검증할 원본이 없습니다".to_string());
    }
    check.ok = check.problems.is_empty();
    check
}

/// R6-S6: why an output whose packet count equals the kept total is NOT
/// confirmed as edit-list-applied (`None` = confirmed): per segment in
/// output order, its `video_kept` output packets must carry the source's
/// kept non-key payloads, in order.
fn edit_list_applied_mismatch(sources: &[SourcePackets], output: &[VideoPacket]) -> Option<String> {
    let mut start = 0usize;
    for (index, source) in sources.iter().enumerate() {
        let count = source.video_kept as usize;
        let range = &output[start.min(output.len())..start.saturating_add(count).min(output.len())];
        start = start.saturating_add(count);
        if source.video_kept == source.video_total {
            continue;
        }
        let Some(expected) = source.kept_fingerprint.as_ref() else {
            return Some(format!(
                "{}번째 원본의 패킷 해시를 얻지 못했습니다",
                index + 1
            ));
        };
        if expected.is_empty() {
            // R7-S4: every kept packet is a key frame (all-intra). R6
            // refused this as "no hashes", so a correct join of such a
            // source always failed.
            if let Some(why) = all_key_segment_mismatch(index, source, range) {
                return Some(why);
            }
            continue;
        }
        let Some(observed) = nonkey_hashes(range.iter()) else {
            return Some(format!(
                "{}번째 구간 출력 패킷의 해시를 얻지 못했습니다",
                index + 1
            ));
        };
        if &observed != expected {
            return Some(format!(
                "{}번째 구간의 출력 프레임이 원본의 유지 프레임과 다릅니다",
                index + 1
            ));
        }
    }
    None
}

/// R7-S4: [`edit_list_applied_mismatch`] for a segment whose kept packets
/// are all key frames: the empty non-key fingerprints compare equal, and
/// the key frames themselves are compared.
///
/// R8-S1 decided by set membership whether the muxer had rewritten the
/// key frames: an output with no source payload at all was accepted on
/// count and timing, and the segment's first packet was exempt whenever
/// it was no source packet — so one wrong frame in that place (a pre-roll
/// frame with its parameter sets re-inserted, a one-frame segment) passed,
/// while an output where only the IDR frames were rewritten (a mix) was
/// refused.
///
/// R9-S2: the hashes are of NORMALIZED payloads
/// ([`normalized_video_packets`]): parameter sets and framing no longer
/// differ, so the output must carry exactly the kept packets, in order,
/// at EVERY position — no exemption, no "rewritten" escape.
fn all_key_segment_mismatch(
    index: usize,
    source: &SourcePackets,
    range: &[VideoPacket],
) -> Option<String> {
    if range.iter().any(|p| !p.key) {
        return Some(format!(
            "{}번째 구간의 출력에 원본에 없는 비키프레임이 있습니다",
            index + 1
        ));
    }
    let Some(all) = source.packet_hashes.as_ref() else {
        return Some(format!(
            "{}번째 원본의 패킷 해시를 얻지 못했습니다",
            index + 1
        ));
    };
    let Some(observed) = range.iter().map(|p| p.hash).collect::<Option<Vec<u64>>>() else {
        return Some(format!(
            "{}번째 구간 출력 패킷의 해시를 얻지 못했습니다",
            index + 1
        ));
    };
    let kept: Vec<u64> = all
        .iter()
        .zip(&source.discard_mask)
        .filter(|(_, discard)| !**discard)
        .map(|(hash, _)| *hash)
        .collect();
    if observed != kept {
        let at = observed
            .iter()
            .zip(&kept)
            .position(|(seen, wanted)| seen != wanted)
            .unwrap_or(observed.len().min(kept.len()));
        return Some(format!(
            "{}번째 구간의 출력 프레임이 원본의 유지 프레임과 다릅니다 (구간 안 {}번째 프레임부터)",
            index + 1,
            at + 1
        ));
    }
    None
}

/// C14: lists every source's packets and the output's video packets,
/// then applies [`evaluate_segment_check`]. `Err` = the check itself
/// could not run (which fails the join too — never published
/// unverified).
pub fn verify_segments(
    ffprobe: &str,
    ffmpeg: &str,
    output: &Path,
    plan: &JoinPlan,
    timeout_secs: Option<u64>,
) -> Result<SegmentCheck, String> {
    let fail = |err: String| format!("이어붙인 결과물의 구간 검증을 실행하지 못했습니다: {err}");
    let sources: Vec<PathBuf> = plan
        .segments
        .iter()
        .map(|segment| segment.source_path.clone())
        .collect();
    let offsets: Vec<f64> = plan
        .segments
        .iter()
        .map(|segment| segment.output_offset_seconds)
        .collect();
    verify_segments_with(
        &sources,
        &offsets,
        output,
        // R9-S2: hashes are of normalized payloads (ffmpeg).
        &mut |path, hashes| {
            if hashes {
                normalized_video_packets(ffprobe, ffmpeg, path, timeout_secs)
            } else {
                video_packets(ffprobe, path, timeout_secs, false)
            }
        },
        &mut |path| stream_packet_counts(ffprobe, path, timeout_secs),
    )
    .map_err(fail)
}

/// `list(path, with_hashes)` of [`verify_segments_with`].
pub type PacketLister<'a> = dyn FnMut(&Path, bool) -> Result<Vec<VideoPacket>, String> + 'a;
/// `count(path)` → (video, audio) packet counts, of [`verify_segments_with`].
pub type PacketCounter<'a> = dyn FnMut(&Path) -> Result<(u64, Option<u64>), String> + 'a;

/// [`verify_segments`] with the ffprobe calls injected: `list(path,
/// with_hashes)` lists video packets, `count(path)` counts packets.
///
/// R7-S3: packets are listed WITHOUT payload hashes first. Hashes only
/// decide `edit-list-applied` — a source has edit-list pre-roll and the
/// output's packet count equals the kept total — and only then are the
/// edited sources and the output listed again, with hashes.
pub fn verify_segments_with(
    sources: &[PathBuf],
    offsets: &[f64],
    output: &Path,
    list: &mut PacketLister<'_>,
    count: &mut PacketCounter<'_>,
) -> Result<SegmentCheck, String> {
    let mut facts = Vec::with_capacity(sources.len());
    for source in sources {
        let (_, audio) = count(source)?;
        let packets = list(source, false)?;
        facts.push(SourcePackets::from_packets(&packets, audio));
    }
    let (_, output_audio) = count(output)?;
    let mut packets = list(output, false)?;
    let total: u64 = facts.iter().map(|s| s.video_total).sum();
    let kept: u64 = facts.iter().map(|s| s.video_kept).sum();
    if kept != total && packets.len() as u64 == kept {
        for (source, fact) in sources.iter().zip(facts.iter_mut()) {
            if fact.video_kept != fact.video_total {
                let hashed = list(source, true)?;
                *fact = SourcePackets::from_packets(&hashed, fact.audio);
            }
        }
        packets = list(output, true)?;
    }
    Ok(evaluate_segment_check(
        &facts,
        offsets,
        output_audio,
        &packets,
    ))
}

/// A recording start on the plan's basis.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedStart {
    /// Seconds on the basis' own scale (naive wall clock or unix).
    pub secs: i64,
    pub text: String,
    /// `"filename"` or `"creation_time"`.
    pub source: &'static str,
}

/// One selected source as the planner sees it.
#[derive(Debug, Clone)]
pub struct Segment {
    pub id: String,
    /// The index's `relative_path` (the viewer's record name).
    pub name: String,
    pub source_path: PathBuf,
    pub size_bytes: u64,
    pub modified_unix: Option<u64>,
    /// Modification time in nanoseconds (D17): what [`stat_matches`]
    /// compares, so a rewrite within the same second is still caught.
    /// Not part of the plan id (which stays on whole seconds).
    pub modified_nanos: Option<u128>,
    pub sha256_indexed: Option<String>,
    pub vendor: Option<String>,
    pub name_time: Option<NaiveRecTime>,
    pub creation: Option<RecordedStart>,
    pub channel: Option<char>,
    pub probe: ProbeFacts,
    /// Start on the chosen basis (None for `filename-order`).
    pub start: Option<RecordedStart>,
    pub output_offset_seconds: f64,
}

impl Segment {
    fn duration(&self) -> f64 {
        self.probe.duration_seconds.unwrap_or(0.0)
    }

    /// Display start: the basis value, else whatever recorded time is known.
    fn display_start(&self) -> Option<RecordedStart> {
        self.start.clone().or_else(|| {
            self.name_time
                .as_ref()
                .filter(|t| t.has_time)
                .map(|t| RecordedStart {
                    secs: t.secs_naive,
                    text: t.text.clone(),
                    source: "filename",
                })
                .or_else(|| self.creation.clone())
        })
    }
}

/// Inputs for [`build_plan`] — one per requested id, already looked up and
/// probed. Separated from I/O so the decision matrix is unit-testable.
#[derive(Debug, Clone)]
pub enum SegmentInput {
    Found(Box<Segment>),
    /// The id cannot be joined; `message` says why (code `not-indexed`).
    NotIndexed {
        id: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Junction {
    /// 1-based index of the segment before the junction.
    pub after_index: usize,
    pub delta_seconds: Option<f64>,
    pub kind: JunctionKind,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totals {
    pub segments: usize,
    pub duration_seconds: f64,
    pub gaps: usize,
    pub gap_seconds: f64,
    pub gap_max_seconds: f64,
    pub overlaps: usize,
    pub overlap_seconds: f64,
    pub overlap_max_seconds: f64,
    /// B2: junctions whose gap/overlap could not be judged (no recorded
    /// time on the basis — every junction of a `filename-order` join).
    pub unknown_junctions: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub code: &'static str,
    pub message: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct JoinPlan {
    pub plan_id: String,
    pub order_basis: OrderBasis,
    pub tolerance_seconds: f64,
    pub channel: Option<char>,
    pub container: Container,
    /// Ordered segments (output order).
    pub segments: Vec<Segment>,
    pub junctions: Vec<Junction>,
    pub totals: Totals,
    pub blocking: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

/// The viewer's channel label (without the code suffix).
pub fn channel_label(code: char) -> &'static str {
    match code {
        'F' => "전방",
        'R' => "후방",
        'I' => "내부",
        'B' => "후방2",
        _ => "미확인",
    }
}

fn is_channel_sep(ch: char) -> bool {
    matches!(ch, '_' | '-' | '.' | ' ')
}

/// Port of the viewer's `channelCodeOf(record)` for an indexed video
/// record: the viewer tests `` `${originalPath} ${name}` `` with an empty
/// original path, i.e. `" " + relative_path`.
///
/// `/[_\-. ]([FRIB])(?:[_.\- ]|[a-z0-9]*$)/i`, then `front`, `rear`,
/// `interior|inside` anywhere (case-insensitive).
pub fn channel_code(relative_path: &str) -> Option<char> {
    let text = format!(" {relative_path}");
    channel_code_of_text(&text)
}

/// `channelCodeOf` on the already-combined text.
pub fn channel_code_of_text(text: &str) -> Option<char> {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if !is_channel_sep(chars[i]) {
            continue;
        }
        let Some(&code) = chars.get(i + 1) else {
            continue;
        };
        let upper = code.to_ascii_uppercase();
        if !matches!(upper, 'F' | 'R' | 'I' | 'B') {
            continue;
        }
        let rest = &chars[i + 2..];
        let sep_next = rest.first().is_some_and(|ch| is_channel_sep(*ch));
        let alnum_to_end = rest.iter().all(|ch| ch.is_ascii_alphanumeric());
        if sep_next || alnum_to_end {
            return Some(upper);
        }
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("front") {
        Some('F')
    } else if lower.contains("rear") {
        Some('R')
    } else if lower.contains("interior") || lower.contains("inside") {
        Some('I')
    } else {
        None
    }
}

/// Natural ordering: digit runs compare numerically, everything else
/// case-insensitively.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na: String = a[si..i].iter().collect::<String>();
            let nb: String = b[sj..j].iter().collect::<String>();
            let ta = na.trim_start_matches('0');
            let tb = nb.trim_start_matches('0');
            let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
            if ord != Ordering::Equal {
                return ord;
            }
            continue;
        }
        let ca = a[i].to_lowercase().collect::<String>();
        let cb = b[j].to_lowercase().collect::<String>();
        let ord = ca.cmp(&cb);
        if ord != Ordering::Equal {
            return ord;
        }
        i += 1;
        j += 1;
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn file_name_of(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

fn parent_of(name: &str) -> &str {
    match name.rfind(['/', '\\']) {
        Some(pos) => &name[..pos],
        None => "",
    }
}

fn r3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// `M분 SS초`, or `H시간 MM분 SS초` from one hour.
pub fn format_duration_ko(seconds: f64) -> String {
    let total = if seconds.is_finite() && seconds > 0.0 {
        seconds.round() as u64
    } else {
        0
    };
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}시간 {m:02}분 {s:02}초")
    } else {
        format!("{m}분 {s:02}초")
    }
}

fn lengths_suffix(lengths: &[f64]) -> String {
    let Some(first) = lengths.first() else {
        return String::new();
    };
    let first_round = first.round();
    if lengths.iter().all(|value| value.round() == first_round) {
        format!("(각 {}초)", first_round as u64)
    } else {
        let sum: f64 = lengths.iter().sum();
        let max = lengths.iter().cloned().fold(0.0, f64::max);
        format!("(합계 {sum:.1}초, 최대 {max:.1}초)")
    }
}

impl JoinPlan {
    pub fn is_blocked(&self) -> bool {
        !self.blocking.is_empty()
    }

    pub fn ids(&self) -> Vec<String> {
        self.segments.iter().map(|s| s.id.clone()).collect()
    }

    pub fn total_bytes(&self) -> u64 {
        self.segments.iter().map(|s| s.size_bytes).sum()
    }

    fn junction_lengths(&self, kind: JunctionKind) -> Vec<f64> {
        self.junctions
            .iter()
            .filter(|j| j.kind == kind)
            .filter_map(|j| j.delta_seconds.map(f64::abs))
            .collect()
    }

    /// Server-built one-line summary, e.g.
    /// `전방 · 10개 · 10분 02초 · 끊김 0곳 / 겹침 2곳(각 1초)`.
    pub fn summary_text(&self) -> String {
        let channel = self
            .channel
            .map(|code| channel_label(code).to_string())
            .unwrap_or_else(|| "채널 미확인".to_string());
        let head = format!(
            "{channel} · {}개 · {}",
            self.segments.len(),
            format_duration_ko(self.totals.duration_seconds)
        );
        if self.order_basis == OrderBasis::FilenameOrder {
            return format!("{head} · 끊김·겹침 확인 불가");
        }
        format!(
            "{head} · 끊김 {}곳{} / 겹침 {}곳{}",
            self.totals.gaps,
            lengths_suffix(&self.junction_lengths(JunctionKind::Gap)),
            self.totals.overlaps,
            lengths_suffix(&self.junction_lengths(JunctionKind::Overlap)),
        )
    }

    fn channel_json(&self) -> Value {
        match self.channel {
            Some(code) => json!({
                "code": code.to_string(),
                "label": channel_label(code),
                "basis": "filename-or-path-token",
            }),
            None => json!({"code": null, "label": null, "basis": "unknown"}),
        }
    }

    /// The §1.6 `/api/join-check` response `plan` object.
    pub fn to_json(&self) -> Value {
        let segments = self
            .segments
            .iter()
            .map(|segment| {
                let start = segment.display_start();
                json!({
                    "id": segment.id,
                    "name": segment.name,
                    "recorded_start_text": start.as_ref().map(|s| s.text.clone()),
                    "time_source": start.as_ref().map(|s| s.source),
                    "duration_seconds": segment.probe.duration_seconds.map(r3),
                    "size_bytes": segment.size_bytes,
                    "video": segment.probe.video.as_ref().map(|v| json!({
                        "codec": v.codec,
                        "profile": v.profile,
                        "width": v.width,
                        "height": v.height,
                        "pix_fmt": v.pix_fmt,
                        "r_frame_rate": v.r_frame_rate,
                        "time_base": v.time_base,
                        "extradata_hash": v.extradata_hash,
                    })),
                    "audio": segment.probe.audio.first().map(audio_json),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "plan_id": self.plan_id,
            "order_basis": self.order_basis.as_str(),
            "tolerance_seconds": self.tolerance_seconds,
            "channel": self.channel_json(),
            "container": self.container.as_str(),
            "segments": segments,
            "junctions": self.junctions.iter().map(junction_json).collect::<Vec<_>>(),
            "totals": {
                "segments": self.totals.segments,
                "duration_seconds": r3(self.totals.duration_seconds),
                "gaps": self.totals.gaps,
                "gap_seconds": r3(self.totals.gap_seconds),
                "overlaps": self.totals.overlaps,
                "overlap_seconds": r3(self.totals.overlap_seconds),
                "overlap_max_seconds": r3(self.totals.overlap_max_seconds),
                "unknown_junctions": self.totals.unknown_junctions,
            },
            "blocking": self.blocking.iter().map(|issue| json!({
                "code": issue.code,
                "message": issue.message,
                "ids": issue.ids,
            })).collect::<Vec<_>>(),
            "warnings": self.warnings.iter().map(|issue| json!({
                "code": issue.code,
                "message": issue.message,
            })).collect::<Vec<_>>(),
            "summary_text": self.summary_text(),
        })
    }
}

fn audio_json(audio: &AudioParams) -> Value {
    json!({
        "codec": audio.codec,
        "sample_rate": audio.sample_rate,
        "channels": audio.channels,
    })
}

fn junction_json(junction: &Junction) -> Value {
    json!({
        "after_index": junction.after_index,
        "delta_seconds": junction.delta_seconds.map(r3),
        "kind": junction.kind.as_str(),
    })
}

/// Validates the request shape shared by the CLI and the server check.
pub fn validate_request(ids: &[String], tolerance: f64) -> Result<(), String> {
    if !tolerance.is_finite()
        || !(MIN_TOLERANCE_SECONDS..=MAX_TOLERANCE_SECONDS).contains(&tolerance)
    {
        return Err(format!(
            "허용 오차는 {MIN_TOLERANCE_SECONDS}~{MAX_TOLERANCE_SECONDS}초 사이여야 합니다 (받은 값: {tolerance})"
        ));
    }
    if ids.len() < MIN_SEGMENTS {
        return Err("영상을 2개 이상 선택하십시오".to_string());
    }
    if ids.len() > MAX_SEGMENTS {
        return Err(format!(
            "한 번에 이어붙일 수 있는 영상은 {MAX_SEGMENTS}개까지입니다 ({}개 선택)",
            ids.len()
        ));
    }
    for id in ids {
        if id.is_empty() || id.len() > MAX_ID_BYTES || id.chars().any(char::is_control) {
            return Err(format!("잘못된 id: {:?}", truncate_for_message(id)));
        }
    }
    Ok(())
}

fn truncate_for_message(text: &str) -> String {
    text.chars().take(64).collect()
}

fn modified_unix(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}

/// Reads `db/videos.jsonl` into an id-keyed map of raw rows.
fn index_rows(case_dir: &Path) -> Result<BTreeMap<String, Value>, String> {
    let path = case_dir.join("db/videos.jsonl");
    let text = std::fs::read_to_string(&path).map_err(|err| {
        format!(
            "failed to read {}: {err} (run scan-folder first)",
            path.display()
        )
    })?;
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?.to_string();
            Some((id, row))
        })
        .collect())
}

/// Builds the join plan for `ids`: index lookup, live stat + ffprobe (at
/// most [`PROBE_PARALLELISM`] at once), ordering and every decision.
/// `Err` only for malformed requests or an unreadable index; everything
/// about the files themselves is reported in `blocking` / `warnings`.
pub fn plan(case_dir: &Path, ids: &[String], tolerance: f64) -> Result<JoinPlan, String> {
    plan_with_tools(case_dir, ids, tolerance, &Tools::default())
}

pub fn plan_with_tools(
    case_dir: &Path,
    ids: &[String],
    tolerance: f64,
    tools: &Tools,
) -> Result<JoinPlan, String> {
    validate_request(ids, tolerance)?;
    let rows = index_rows(case_dir)?;
    let mut inputs: Vec<Option<SegmentInput>> = Vec::with_capacity(ids.len());
    let mut to_probe: Vec<(usize, PathBuf)> = Vec::new();
    for id in ids {
        let not_indexed = |message: &str| {
            Some(SegmentInput::NotIndexed {
                id: id.clone(),
                message: message.to_string(),
            })
        };
        if !id.starts_with("vid_") {
            inputs.push(not_indexed(
                "색인된 원본 영상(vid_*)만 이어붙일 수 있습니다 (후보·복구본·결과물 제외)",
            ));
            continue;
        }
        let Some(row) = rows.get(id) else {
            inputs.push(not_indexed("색인에 없는 영상입니다"));
            continue;
        };
        if row.get("index_status").and_then(Value::as_str) == Some("stale") {
            inputs.push(not_indexed("다시 스캔할 때 원본이 사라진 영상입니다"));
            continue;
        }
        // R5-P3: the lossless path (`source_path_raw_hex` first), so a
        // non-Unicode name resolves to its own file, not to the lossy
        // display spelling.
        let raw_path = crate::model::record_os_path(row).unwrap_or_default();
        let source_path = match crate::model::canonicalize_lossless(&raw_path) {
            Ok(path) if path.is_file() => path,
            _ => {
                inputs.push(not_indexed("원본 파일을 찾을 수 없습니다"));
                continue;
            }
        };
        let meta = match std::fs::metadata(&source_path) {
            Ok(meta) => meta,
            Err(_) => {
                inputs.push(not_indexed("원본 파일을 읽을 수 없습니다"));
                continue;
            }
        };
        let name = row
            .get("relative_path")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();
        let source_text = source_path.to_string_lossy().to_string();
        let segment = Segment {
            id: id.clone(),
            name_time: rectime::parse_record_time(&["", &name, &source_text]),
            channel: channel_code(&name),
            name,
            size_bytes: meta.len(),
            modified_unix: modified_unix(&meta),
            modified_nanos: crate::audit::modified_nanos(&meta),
            sha256_indexed: row
                .get("sha256")
                .and_then(Value::as_str)
                .map(str::to_string),
            vendor: row
                .get("source_profile")
                .and_then(|p| p.get("vendor"))
                .and_then(Value::as_str)
                .map(str::to_string),
            source_path: source_path.clone(),
            creation: None,
            probe: ProbeFacts::default(),
            start: None,
            output_offset_seconds: 0.0,
        };
        to_probe.push((inputs.len(), source_path));
        inputs.push(Some(SegmentInput::Found(Box::new(segment))));
    }

    let probes = probe_parallel(&tools.ffprobe, &to_probe);
    for ((slot, _), facts) in to_probe.iter().zip(probes) {
        if let Some(SegmentInput::Found(segment)) = inputs[*slot].as_mut() {
            segment.creation = facts.creation_time.as_deref().and_then(|text| {
                crate::timeline::parse_creation_time(text).map(|(unix, _)| RecordedStart {
                    secs: unix as i64,
                    text: text.trim().to_string(),
                    source: "creation_time",
                })
            });
            segment.probe = facts;
        }
    }
    Ok(build_plan(
        inputs.into_iter().flatten().collect(),
        tolerance,
    ))
}

fn probe_parallel(ffprobe: &str, jobs: &[(usize, PathBuf)]) -> Vec<ProbeFacts> {
    let slots: std::sync::Mutex<Vec<Option<ProbeFacts>>> =
        std::sync::Mutex::new(vec![None; jobs.len()]);
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..PROBE_PARALLELISM.min(jobs.len().max(1)) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some((_, path)) = jobs.get(index) else {
                        break;
                    };
                    let facts = probe_file(ffprobe, path);
                    slots.lock().unwrap_or_else(|p| p.into_inner())[index] = Some(facts);
                }
            });
        }
    });
    slots
        .into_inner()
        .unwrap_or_else(|p| p.into_inner())
        .into_iter()
        .map(|facts| facts.unwrap_or_default())
        .collect()
}

/// Counts values preserving first-seen order: `[(value, count, ids)]`.
fn tally<T: PartialEq + Clone>(items: &[(T, String)]) -> Vec<(T, usize, Vec<String>)> {
    let mut out: Vec<(T, usize, Vec<String>)> = Vec::new();
    for (value, id) in items {
        if let Some(entry) = out.iter_mut().find(|entry| entry.0 == *value) {
            entry.1 += 1;
            entry.2.push(id.clone());
        } else {
            out.push((value.clone(), 1, vec![id.clone()]));
        }
    }
    out
}

fn opt_text(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "없음".to_string())
}

fn opt_num(value: Option<u64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "없음".to_string())
}

/// Adds a `stream-mismatch` issue when `values` are not all equal.
/// `subject` carries its particle ("해상도가", "영상 코덱이").
fn mismatch(
    blocking: &mut Vec<Issue>,
    subject: &str,
    values: &[(String, String)],
    describe_shortly: bool,
) {
    let groups = tally(values);
    if groups.len() <= 1 {
        return;
    }
    let first = &groups[0].0;
    let detail = groups
        .iter()
        .map(|(value, count, _)| {
            let shown = if describe_shortly && value.chars().count() > 20 {
                format!("{}…", value.chars().take(16).collect::<String>())
            } else {
                value.clone()
            };
            format!("{shown} ({count}개)")
        })
        .collect::<Vec<_>>()
        .join(" / ");
    let ids = values
        .iter()
        .filter(|(value, _)| value != first)
        .map(|(_, id)| id.clone())
        .collect();
    blocking.push(Issue {
        code: "stream-mismatch",
        message: format!("{subject} 다릅니다: {detail}"),
        ids,
    });
}

/// The decision matrix over already-probed inputs (pure; no I/O).
pub fn build_plan(inputs: Vec<SegmentInput>, tolerance: f64) -> JoinPlan {
    let mut blocking: Vec<Issue> = Vec::new();
    let mut warnings: Vec<Issue> = Vec::new();
    let mut segments: Vec<Segment> = Vec::new();
    let mut not_indexed: Vec<(String, String)> = Vec::new();
    for input in inputs {
        match input {
            SegmentInput::Found(segment) => segments.push(*segment),
            SegmentInput::NotIndexed { id, message } => not_indexed.push((id, message)),
        }
    }
    for (message, count, ids) in tally(
        &not_indexed
            .iter()
            .map(|(id, message)| (message.clone(), id.clone()))
            .collect::<Vec<_>>(),
    ) {
        blocking.push(Issue {
            code: "not-indexed",
            message: format!("{message} ({count}개)"),
            ids,
        });
    }

    // Paths are written into the concat list; control characters (a
    // newline would start a new directive) cannot be escaped there.
    let bad_paths: Vec<String> = segments
        .iter()
        .filter(|s| {
            s.source_path
                .to_string_lossy()
                .chars()
                .any(char::is_control)
        })
        .map(|s| s.id.clone())
        .collect();
    if !bad_paths.is_empty() {
        blocking.push(Issue {
            code: "path-unsupported",
            message: format!(
                "경로에 줄바꿈 같은 제어 문자가 있는 파일 {}개 — 이어붙일 수 없습니다",
                bad_paths.len()
            ),
            ids: bad_paths,
        });
    }

    // B7: the concat list is UTF-8; a path that is not valid Unicode
    // cannot be written into it faithfully (lossy text would name another
    // file), so it is refused with its own code.
    let non_utf8: Vec<String> = segments
        .iter()
        .filter(|s| s.source_path.to_str().is_none())
        .map(|s| s.id.clone())
        .collect();
    if !non_utf8.is_empty() {
        blocking.push(Issue {
            code: "path-not-utf8",
            message: format!(
                "경로를 UTF-8로 나타낼 수 없는 파일 {}개 — 이어붙일 수 없습니다 (폴더·파일 이름을 바꾸십시오)",
                non_utf8.len()
            ),
            ids: non_utf8,
        });
    }

    // Duplicates: same id, same file, or same indexed content hash.
    let mut duplicate_ids: Vec<String> = Vec::new();
    for (i, a) in segments.iter().enumerate() {
        for b in segments.iter().skip(i + 1) {
            let same_hash = matches!((&a.sha256_indexed, &b.sha256_indexed),
                (Some(x), Some(y)) if x.eq_ignore_ascii_case(y));
            if a.id == b.id || a.source_path == b.source_path || same_hash {
                for id in [&a.id, &b.id] {
                    if !duplicate_ids.contains(id) {
                        duplicate_ids.push(id.clone());
                    }
                }
            }
        }
    }
    if !duplicate_ids.is_empty() {
        blocking.push(Issue {
            code: "duplicate-file",
            message: format!(
                "같은 파일(경로 또는 해시가 같음)이 두 번 이상 선택됐습니다 ({}개)",
                duplicate_ids.len()
            ),
            ids: duplicate_ids,
        });
    }

    let unknown_duration: Vec<String> = segments
        .iter()
        .filter(|s| !s.probe.ok || s.probe.duration_seconds.is_none())
        .map(|s| s.id.clone())
        .collect();
    if !unknown_duration.is_empty() {
        blocking.push(Issue {
            code: "unknown-duration",
            message: format!(
                "길이를 알 수 없는 파일 {}개 — 손상 의심",
                unknown_duration.len()
            ),
            ids: unknown_duration,
        });
    }

    // Order basis: one basis for every segment, never mixed.
    let all_name_time = !segments.is_empty()
        && segments
            .iter()
            .all(|s| s.name_time.as_ref().is_some_and(|t| t.has_time));
    let all_creation = !segments.is_empty() && segments.iter().all(|s| s.creation.is_some());
    let order_basis = if all_name_time {
        OrderBasis::FilenameTime
    } else if all_creation {
        OrderBasis::CreationTime
    } else {
        OrderBasis::FilenameOrder
    };
    for segment in &mut segments {
        segment.start = match order_basis {
            OrderBasis::FilenameTime => segment.name_time.as_ref().map(|t| RecordedStart {
                secs: t.secs_naive,
                text: t.text.clone(),
                source: "filename",
            }),
            OrderBasis::CreationTime => segment.creation.clone(),
            OrderBasis::FilenameOrder => None,
        };
    }
    if order_basis == OrderBasis::FilenameOrder && !segments.is_empty() {
        let some_time = segments
            .iter()
            .any(|s| s.name_time.as_ref().is_some_and(|t| t.has_time) || s.creation.is_some());
        warnings.push(Issue {
            code: "order-fallback",
            message: if some_time {
                "일부 파일에만 녹화 시각이 있어 파일명 순으로 이어붙입니다 — 끊김·겹침은 확인할 수 없습니다".to_string()
            } else {
                "녹화 시각이 없어 파일명 순으로 이어붙입니다 — 끊김·겹침은 확인할 수 없습니다"
                    .to_string()
            },
            ids: Vec::new(),
        });
    }
    let by_name = |a: &Segment, b: &Segment| {
        natural_cmp(file_name_of(&a.name), file_name_of(&b.name))
            .then_with(|| natural_cmp(&a.name, &b.name))
            .then_with(|| a.id.cmp(&b.id))
    };
    segments.sort_by(|a, b| match (&a.start, &b.start) {
        (Some(x), Some(y)) => x.secs.cmp(&y.secs).then_with(|| by_name(a, b)),
        _ => by_name(a, b),
    });

    if order_basis != OrderBasis::FilenameOrder {
        let starts: Vec<(i64, String)> = segments
            .iter()
            .filter_map(|s| s.start.as_ref().map(|t| (t.secs, s.id.clone())))
            .collect();
        let same: Vec<String> = tally(&starts)
            .into_iter()
            .filter(|(_, count, _)| *count > 1)
            .flat_map(|(_, _, ids)| ids)
            .collect();
        if !same.is_empty() {
            blocking.push(Issue {
                code: "same-start",
                message: format!(
                    "녹화 시작 시각이 같은 파일이 있습니다 ({}개) — 다른 폴더의 복사본이나 다른 채널일 수 있습니다",
                    same.len()
                ),
                ids: same,
            });
        }
    }

    // Channel.
    let codes: Vec<(Option<char>, String)> =
        segments.iter().map(|s| (s.channel, s.id.clone())).collect();
    let code_groups = tally(&codes);
    let channel = match code_groups.as_slice() {
        [(Some(code), _, _)] => Some(*code),
        [(None, _, _)] => {
            warnings.push(Issue {
                code: "channel-unknown",
                message:
                    "파일 이름으로 채널을 알 수 없습니다 — 같은 카메라 영상인지 직접 확인하십시오"
                        .to_string(),
                ids: Vec::new(),
            });
            None
        }
        [] => None,
        groups => {
            let detail = groups
                .iter()
                .map(|(code, count, _)| {
                    format!("{} {count}", code.map(channel_label).unwrap_or("미확인"))
                })
                .collect::<Vec<_>>()
                .join(" · ");
            let first = groups[0].0;
            blocking.push(Issue {
                code: "channel-mixed",
                message: format!("채널이 섞여 있습니다 ({detail})"),
                ids: codes
                    .iter()
                    .filter(|(code, _)| *code != first)
                    .map(|(_, id)| id.clone())
                    .collect(),
            });
            None
        }
    };

    // Stream parameters (only among probed files).
    let probed: Vec<&Segment> = segments.iter().filter(|s| s.probe.ok).collect();
    let no_video: Vec<String> = probed
        .iter()
        .filter(|s| s.probe.video.is_none())
        .map(|s| s.id.clone())
        .collect();
    if !no_video.is_empty() {
        // R5-S13: said plainly at planning time — a sound-only file cannot
        // be part of a lossless VIDEO join (and its output could not be
        // verified packet by packet); before, the message read like a
        // parameter mismatch and the join could run until the output check.
        let audio_only = probed
            .iter()
            .filter(|s| s.probe.video.is_none() && !s.probe.audio.is_empty())
            .count();
        blocking.push(Issue {
            code: if audio_only > 0 { "audio-only" } else { "stream-mismatch" },
            message: if audio_only > 0 {
                format!(
                    "영상 없이 소리만 있는 파일 {audio_only}개 — 영상 이어붙이기에 넣을 수 없습니다 (선택에서 빼십시오){}",
                    if no_video.len() > audio_only {
                        format!(", 영상 스트림이 없는 파일 모두 {}개", no_video.len())
                    } else {
                        String::new()
                    }
                )
            } else {
                format!("영상 스트림이 없는 파일 {}개", no_video.len())
            },
            ids: no_video,
        });
    }
    let with_video: Vec<(&VideoParams, String)> = probed
        .iter()
        .filter_map(|s| s.probe.video.as_ref().map(|v| (v, s.id.clone())))
        .collect();
    let field = |f: &dyn Fn(&VideoParams) -> String| -> Vec<(String, String)> {
        with_video
            .iter()
            .map(|(v, id)| (f(v), id.clone()))
            .collect()
    };
    mismatch(
        &mut blocking,
        "영상 코덱이",
        &field(&|v| opt_text(&v.codec)),
        false,
    );
    mismatch(
        &mut blocking,
        "코덱 프로필이",
        &field(&|v| opt_text(&v.profile)),
        false,
    );
    mismatch(
        &mut blocking,
        "해상도가",
        &field(&|v| format!("{}×{}", opt_num(v.width), opt_num(v.height))),
        false,
    );
    mismatch(
        &mut blocking,
        "픽셀 형식이",
        &field(&|v| opt_text(&v.pix_fmt)),
        false,
    );
    mismatch(
        &mut blocking,
        "프레임 속도가",
        &field(&|v| opt_text(&v.r_frame_rate)),
        false,
    );
    mismatch(
        &mut blocking,
        "타임베이스가",
        &field(&|v| opt_text(&v.time_base)),
        false,
    );
    let extradata = field(&|v| opt_text(&v.extradata_hash));
    if !with_video.is_empty() && with_video.iter().all(|(v, _)| v.extradata_hash.is_none()) {
        // B6: without SPS/PPS hashes the decoder-relevant values ffprobe
        // does report must all agree (blocking), and the gap is stated.
        warnings.push(Issue {
            code: "extradata-unavailable",
            message: "코덱 설정(SPS/PPS) 해시를 읽지 못해 직접 비교하지 못했습니다 — 대신 레벨·참조 프레임·B-프레임 등 ffprobe 값으로 비교했습니다".to_string(),
            ids: Vec::new(),
        });
        for (position, (key, subject)) in VIDEO_FALLBACK_FIELDS.iter().enumerate() {
            let values = field(&|v| {
                v.fallback
                    .get(position)
                    .filter(|(k, _)| k == key)
                    .and_then(|(_, value)| value.clone())
                    .unwrap_or_else(|| "없음".to_string())
            });
            mismatch(&mut blocking, subject, &values, false);
        }
    } else {
        mismatch(&mut blocking, "코덱 설정(SPS/PPS)이", &extradata, true);
    }
    let avg = tally(&field(&|v| opt_text(&v.avg_frame_rate)));
    if avg.len() > 1 {
        warnings.push(Issue {
            code: "avg-frame-rate-differs",
            message: format!(
                "평균 프레임 속도가 다릅니다(가변 프레임일 수 있음): {}",
                avg.iter()
                    .map(|(value, count, _)| format!("{value} ({count}개)"))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
            ids: Vec::new(),
        });
    }

    // Audio: presence, track count, parameters.
    let without_audio: Vec<String> = probed
        .iter()
        .filter(|s| s.probe.audio.is_empty())
        .map(|s| s.id.clone())
        .collect();
    if !without_audio.is_empty() && without_audio.len() < probed.len() {
        blocking.push(Issue {
            code: "audio-presence",
            message: format!(
                "오디오 트랙이 일부에만 있습니다 ({}개 없음)",
                without_audio.len()
            ),
            ids: without_audio,
        });
    } else {
        let with_audio: Vec<&&Segment> = probed
            .iter()
            .filter(|s| !s.probe.audio.is_empty())
            .collect();
        mismatch(
            &mut blocking,
            "오디오 트랙 수가",
            &with_audio
                .iter()
                .map(|s| (s.probe.audio.len().to_string(), s.id.clone()))
                .collect::<Vec<_>>(),
            false,
        );
        let tracks = with_audio
            .iter()
            .map(|s| s.probe.audio.len())
            .min()
            .unwrap_or(0);
        for track in 0..tracks {
            let label = |what: &str| {
                if tracks > 1 {
                    format!("오디오 {} {what}", track + 1)
                } else {
                    format!("오디오 {what}")
                }
            };
            let values = |f: &dyn Fn(&AudioParams) -> String| -> Vec<(String, String)> {
                with_audio
                    .iter()
                    .map(|s| (f(&s.probe.audio[track]), s.id.clone()))
                    .collect()
            };
            mismatch(
                &mut blocking,
                &label("코덱이"),
                &values(&|a| opt_text(&a.codec)),
                false,
            );
            mismatch(
                &mut blocking,
                &label("샘플레이트가"),
                &values(&|a| opt_num(a.sample_rate)),
                false,
            );
            mismatch(
                &mut blocking,
                &label("채널 수가"),
                &values(&|a| opt_num(a.channels)),
                false,
            );
            mismatch(
                &mut blocking,
                &label("프로필이"),
                &values(&|a| opt_text(&a.profile)),
                false,
            );
            // The AudioSpecificConfig (AAC) / codec extradata: compared when
            // any segment reports it (none → nothing to compare).
            if with_audio
                .iter()
                .any(|s| s.probe.audio[track].extradata_hash.is_some())
            {
                mismatch(
                    &mut blocking,
                    &label("코덱 설정(extradata)이"),
                    &values(&|a| opt_text(&a.extradata_hash)),
                    true,
                );
            }
        }
    }

    // B4: streams the join does not carry are said up front, not only in
    // the manifest.
    let extra_video: usize = segments
        .iter()
        .map(|s| s.probe.video_streams.saturating_sub(1))
        .sum();
    if extra_video > 0 {
        warnings.push(Issue {
            code: "extra-video-streams",
            message: format!(
                "추가 영상 스트림 {extra_video}개는 포함되지 않습니다 (첫 번째 영상 스트림만 이어붙임)"
            ),
            ids: segments
                .iter()
                .filter(|s| s.probe.video_streams > 1)
                .map(|s| s.id.clone())
                .collect(),
        });
    }
    let data_streams: usize = segments.iter().map(|s| s.probe.other_streams).sum();
    if data_streams > 0 {
        warnings.push(Issue {
            code: "data-streams-dropped",
            message: format!("데이터·자막 스트림 {data_streams}개(GPS 등)는 포함되지 않습니다"),
            ids: segments
                .iter()
                .filter(|s| s.probe.other_streams > 0)
                .map(|s| s.id.clone())
                .collect(),
        });
    }

    // Informational warnings.
    let vendors = tally(
        &segments
            .iter()
            .map(|s| (s.vendor.clone().unwrap_or_default(), s.id.clone()))
            .collect::<Vec<_>>(),
    );
    if vendors.len() > 1 {
        warnings.push(Issue {
            code: "vendor-differs",
            message: format!(
                "제조사 판별이 서로 다릅니다: {}",
                vendors
                    .iter()
                    .map(|(v, count, _)| format!(
                        "{} ({count}개)",
                        if v.is_empty() { "미확인" } else { v }
                    ))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
            ids: Vec::new(),
        });
    }
    let folders = tally(
        &segments
            .iter()
            .map(|s| (parent_of(&s.name).to_string(), s.id.clone()))
            .collect::<Vec<_>>(),
    );
    if folders.len() > 1 {
        warnings.push(Issue {
            code: "folders-differ",
            message: format!("서로 다른 폴더 {}곳의 파일입니다", folders.len()),
            ids: Vec::new(),
        });
    }
    let unhashed = segments
        .iter()
        .filter(|s| s.sha256_indexed.is_none())
        .count();
    if unhashed > 0 {
        warnings.push(Issue {
            code: "hash-unavailable",
            message: format!(
                "색인 해시가 없는 파일 {unhashed}개 — 이어붙일 때 새로 계산해 기록합니다"
            ),
            ids: Vec::new(),
        });
    }

    // Offsets and junctions.
    let mut offset = 0.0;
    for segment in &mut segments {
        segment.output_offset_seconds = offset;
        offset += segment.duration();
    }
    let mut totals = Totals {
        segments: segments.len(),
        duration_seconds: offset,
        ..Totals::default()
    };
    let mut junctions = Vec::new();
    for (index, pair) in segments.windows(2).enumerate() {
        let delta = match (&pair[0].start, &pair[1].start) {
            (Some(a), Some(b)) if pair[0].probe.duration_seconds.is_some() => {
                Some(b.secs as f64 - (a.secs as f64 + pair[0].duration()))
            }
            _ => None,
        };
        let kind = delta
            .map(|d| classify_junction(d, tolerance))
            .unwrap_or(JunctionKind::Unknown);
        match (kind, delta) {
            (JunctionKind::Gap, Some(d)) => {
                totals.gaps += 1;
                totals.gap_seconds += d;
                totals.gap_max_seconds = totals.gap_max_seconds.max(d);
            }
            (JunctionKind::Overlap, Some(d)) => {
                totals.overlaps += 1;
                totals.overlap_seconds += -d;
                totals.overlap_max_seconds = totals.overlap_max_seconds.max(-d);
            }
            (JunctionKind::Unknown, _) => totals.unknown_junctions += 1,
            _ => {}
        }
        junctions.push(Junction {
            after_index: index + 1,
            delta_seconds: delta,
            kind,
        });
    }

    let container = segments
        .iter()
        .find_map(|s| s.probe.video.as_ref().map(|v| (v, s)))
        .map(|(v, s)| {
            Container::choose(
                v.codec.as_deref(),
                s.probe.audio.first().and_then(|a| a.codec.as_deref()),
            )
        })
        .unwrap_or(Container::Mkv);

    let mut plan = JoinPlan {
        plan_id: String::new(),
        order_basis,
        tolerance_seconds: tolerance,
        channel,
        container,
        segments,
        junctions,
        totals,
        blocking,
        warnings,
    };
    plan.plan_id = compute_plan_id(&plan, &not_indexed);
    plan
}

/// SHA-256 over the normalized plan: ordered ids, paths, size, mtime,
/// stream parameters, recorded times and tolerance.
fn compute_plan_id(plan: &JoinPlan, not_indexed: &[(String, String)]) -> String {
    let segments = plan
        .segments
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "path": s.source_path.to_string_lossy(),
                "size": s.size_bytes,
                "mtime": s.modified_unix,
                "sha256_indexed": s.sha256_indexed,
                "duration": s.probe.duration_seconds.map(|d| format!("{d:.6}")),
                "probe_ok": s.probe.ok,
                "video": s.probe.video.as_ref().map(|v| json!([
                    v.codec, v.profile, v.width, v.height, v.pix_fmt,
                    v.r_frame_rate, v.avg_frame_rate, v.time_base, v.extradata_hash,
                    v.fallback.iter().map(|(_, value)| value.clone()).collect::<Vec<_>>()
                ])),
                "audio": s.probe.audio.iter().map(|a| json!([a.codec, a.sample_rate, a.channels, a.profile, a.extradata_hash])).collect::<Vec<_>>(),
                "other_streams": s.probe.other_streams,
                "video_streams": s.probe.video_streams,
                "start": s.start.as_ref().map(|t| json!([t.secs, t.source])),
            })
        })
        .collect::<Vec<_>>();
    let material = json!({
        "v": 1,
        "tolerance": format!("{:.3}", plan.tolerance_seconds),
        "segments": segments,
        "not_indexed": not_indexed.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
    });
    crate::sha256::digest_bytes(material.to_string().as_bytes())
}

/// One `file '…'` line for the concat list: `\\?\` stripped, `/`
/// separators, `'` escaped as `'\''`. Control characters are refused.
pub fn concat_file_line(path: &Path) -> Result<String, String> {
    let display = crate::util::strip_windows_extended_prefix(path);
    let Some(text) = display.to_str() else {
        return Err(format!(
            "경로를 UTF-8로 나타낼 수 없어 concat 목록에 쓸 수 없습니다: {}",
            display.to_string_lossy()
        ));
    };
    // `\` is a separator only on Windows; elsewhere it is an ordinary file
    // name byte and must stay as is.
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.to_string()
    };
    if text.chars().any(char::is_control) {
        return Err(format!(
            "경로에 제어 문자가 있어 concat 목록에 쓸 수 없습니다: {}",
            text.escape_debug()
        ));
    }
    Ok(format!("file '{}'", text.replace('\'', "'\\''")))
}

/// The UTF-8 (no BOM) ffconcat list for the plan's segment order.
pub fn concat_list(plan: &JoinPlan) -> Result<String, String> {
    let mut out = String::from("ffconcat version 1.0\n");
    for segment in &plan.segments {
        out.push_str(&concat_file_line(&segment.source_path)?);
        out.push('\n');
    }
    Ok(out)
}

/// Job tags name temp files and ride in argv: ASCII `[A-Za-z0-9_-]{1,64}`.
pub fn validate_job_tag(tag: &str) -> Result<(), String> {
    if tag.is_empty()
        || tag.len() > 64
        || !tag
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(format!(
            "invalid job tag {:?}: use 1-64 ASCII letters, digits, '-' or '_'",
            truncate_for_message(tag)
        ));
    }
    Ok(())
}

/// Temp files of one join job (inside `artifacts/joined`). All start with
/// `.join-` so packaging skips them and crash cleanup can find them.
#[derive(Debug, Clone)]
pub struct TempPaths {
    pub concat_list: PathBuf,
    pub part: PathBuf,
    pub progress: PathBuf,
    /// B8: `{"output", "manifest"}` this job published, written right after
    /// the output gets its final name. A killed job's cleanup deletes only
    /// what its own claim names (never another job's same-plan output).
    pub claim: PathBuf,
    /// B13: held under an exclusive OS lock for the whole run, so another
    /// process's crash cleanup can tell a live join from leftovers.
    pub lock: PathBuf,
}

pub fn temp_paths(case_dir: &Path, tag: &str) -> TempPaths {
    let dir = case_dir.join(JOINED_DIR);
    TempPaths {
        concat_list: dir.join(format!(".join-{tag}.ffconcat")),
        part: dir.join(format!(".join-{tag}.part")),
        progress: dir.join(format!(".join-{tag}.progress.json")),
        claim: dir.join(format!(".join-{tag}.claim.json")),
        lock: dir.join(format!(".join-{tag}.lock")),
    }
}

impl TempPaths {
    /// Every temp file except the lock (removed last, after unlocking).
    pub fn all(&self) -> [&Path; 4] {
        [&self.concat_list, &self.part, &self.progress, &self.claim]
    }
}

/// B8: the output + manifest a job recorded in its claim file (None when
/// it never published, or the claim is unreadable).
pub fn read_claim(claim: &Path) -> Option<(PathBuf, PathBuf)> {
    read_join_claim(claim).map(|claim| (claim.output, claim.manifest))
}

/// C3: what one join job's claim file says it reserved / published.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinClaim {
    pub output: PathBuf,
    pub manifest: PathBuf,
    /// `Some(false)`: written BEFORE the output got its final name (the
    /// manifest name is reserved, the output may or may not exist yet);
    /// `Some(true)` / `None` (claims written before C3): the output under
    /// that name is this job's.
    pub published: Option<bool>,
    /// Size of the verified `.part` — identifies an unconfirmed output.
    pub part_size: Option<u64>,
}

/// C3: the full claim record (None when absent or unreadable).
pub fn read_join_claim(claim: &Path) -> Option<JoinClaim> {
    let value: Value = serde_json::from_slice(&std::fs::read(claim).ok()?).ok()?;
    Some(JoinClaim {
        output: PathBuf::from(value.get("output")?.as_str()?),
        manifest: PathBuf::from(value.get("manifest")?.as_str()?),
        published: value.get("published").and_then(Value::as_bool),
        part_size: value.get("part_size").and_then(Value::as_u64),
    })
}

fn write_join_claim(path: &Path, claim: &JoinClaim, job_tag: &str) -> Result<(), String> {
    let body = json!({
        "output": claim.output.to_string_lossy(),
        "manifest": claim.manifest.to_string_lossy(),
        "job_tag": job_tag,
        "published": claim.published,
        "part_size": claim.part_size,
    });
    crate::util::write_text_atomic(path, &body.to_string()).map_err(|err| {
        format!(
            "failed to record the join output claim in {}: {err}",
            path.display()
        )
    })
}

/// Canonical output + manifest paths every parseable join-log entry names
/// (a logged artifact is never deleted from under its record).
pub fn logged_join_artifacts(case_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let path = case_dir.join(JOINED_DIR).join(LOG_FILE);
    // R9 P3: through the one lane reader. EVERY entry counts here, usable
    // or not — this list only protects files from deletion, so the safe
    // direction is to protect whatever any entry names.
    let lane = audit::read_lane_rows(&path)
        .map_err(|err| format!("{} 읽기 실패: {err}", path.display()))?;
    Ok(lane
        .rows
        .into_iter()
        .map(|row| row.value)
        .flat_map(|entry| {
            ["output_path", "manifest_path"]
                .into_iter()
                .filter_map(|key| entry.get(key).and_then(Value::as_str).map(str::to_string))
                .map(|p| audit::canonical_or_original(Path::new(&p)))
                .collect::<Vec<_>>()
        })
        .collect())
}

/// Canonical `path` when it is an existing file inside
/// `case_dir/artifacts/joined`.
fn joined_file(case_dir: &Path, path: &Path) -> Option<PathBuf> {
    let root = case_dir.join(JOINED_DIR).canonicalize().ok()?;
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        case_dir.join(path)
    };
    let canonical = resolved.canonicalize().ok()?;
    (canonical.starts_with(&root) && canonical != root && canonical.is_file()).then_some(canonical)
}

/// C3: removes what a dead join job's claim names but the join log never
/// recorded — the reserved/written manifest and, when it is provably this
/// job's (published, or the size of its verified `.part`), the output.
/// Files outside `artifacts/joined` and logged artifacts are never touched.
/// Returns each removal attempted with its result; a join-log read failure
/// removes nothing and is returned as one `Err` (against the claimed
/// output path).
pub fn remove_unlogged_claimed(
    case_dir: &Path,
    claim: &JoinClaim,
) -> Vec<(PathBuf, Result<(), String>)> {
    let logged = match logged_join_artifacts(case_dir) {
        Ok(logged) => logged,
        Err(err) => {
            return vec![(
                claim.output.clone(),
                Err(format!(
                    "이어붙이기 로그를 읽지 못해 남은 결과물을 확인하지 못했습니다: {err}"
                )),
            )];
        }
    };
    let output_is_ours = |path: &Path| match claim.published {
        Some(false) => claim
            .part_size
            .is_some_and(|size| std::fs::metadata(path).is_ok_and(|m| m.len() == size)),
        _ => true,
    };
    let mut out = Vec::new();
    for (path, check_owner) in [(&claim.output, true), (&claim.manifest, false)] {
        let Some(canonical) = joined_file(case_dir, path) else {
            continue;
        };
        if logged.contains(&canonical) || (check_owner && !output_is_ours(&canonical)) {
            continue;
        }
        let result = remove_file_retry(&canonical);
        out.push((canonical, result));
    }
    out
}

/// The viewer/report record id of a joined output (U2): stable for the
/// bytes, independent of the file name — `join:<first 16 hex of sha256>`.
pub fn joined_record_id(output_sha256: &str) -> Option<String> {
    let hex = output_sha256.trim().to_ascii_lowercase();
    (hex.len() >= 16 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("join:{}", &hex[..16]))
}

/// Adds `record_id` (from `output_sha256`) to a join-log entry that lacks
/// one — entries written before the field existed.
pub fn with_record_id(mut entry: Value) -> Value {
    if entry.get("record_id").and_then(Value::as_str).is_none()
        && let Some(id) = entry
            .get("output_sha256")
            .and_then(Value::as_str)
            .and_then(joined_record_id)
        && let Some(map) = entry.as_object_mut()
    {
        map.insert("record_id".to_string(), Value::String(id));
    }
    entry
}

/// [`with_record_id`] over JSONL text (unparseable lines kept verbatim).
pub fn join_log_with_record_ids(jsonl: &str) -> String {
    let mut out = String::with_capacity(jsonl.len());
    for line in jsonl.lines() {
        match serde_json::from_str::<Value>(line) {
            Ok(value) if value.is_object() => out.push_str(&with_record_id(value).to_string()),
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// The ffmpeg argv (relative names only — every byte is ASCII).
pub fn ffmpeg_argv(tag: &str, container: Container) -> Vec<String> {
    let mut args: Vec<String> = [
        "-y",
        "-hide_banner",
        "-nostdin",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(format!(".join-{tag}.ffconcat"));
    for arg in [
        "-map",
        "0:v:0",
        "-map",
        "0:a?",
        "-c",
        "copy",
        // R10-S8: stream copy drops every packet before the first key
        // frame unless told otherwise, so a first source that starts on a
        // non-key frame (cut mid-GOP) lost those frames and the segment
        // check refused the join on the packet count.
        "-copyinkf",
        "-avoid_negative_ts",
        "make_zero",
    ] {
        args.push(arg.to_string());
    }
    if container == Container::Mp4 {
        args.push("-movflags".to_string());
        args.push("+faststart".to_string());
    }
    args.push("-f".to_string());
    args.push(container.ffmpeg_format().to_string());
    args.push(format!(".join-{tag}.part"));
    args
}

/// Removes a file, retrying 5×200 ms (antivirus / indexers can briefly
/// hold handles on Windows). A missing file is success.
pub fn remove_file_retry(path: &Path) -> Result<(), String> {
    let mut last_err = None;
    for attempt in 0..5 {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => last_err = Some(err),
        }
        if attempt < 4 {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    Err(format!(
        "임시 파일을 지우지 못했습니다: {} ({})",
        path.display(),
        last_err.map(|e| e.to_string()).unwrap_or_default()
    ))
}

/// Leftovers without a lock file are removed only once this old (B13).
pub const STALE_TEMP_AGE_SECS: u64 = 3600;

/// True when another handle (any process) holds the job's run lock.
fn lock_is_held(lock: &Path) -> bool {
    use fs2::FileExt;
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock)
    else {
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

/// Crash recovery: removes `.join-<tag>.*` leftovers in `artifacts/joined`
/// of jobs that are certainly dead. A tag is left alone when it is in
/// `active_tags` (this server's running jobs) or its run lock is held by
/// any process (a CLI join running elsewhere — B13). Without a lock file
/// (older runs) its files go only once all are older than
/// [`STALE_TEMP_AGE_SECS`]. Returns each removed path with its result.
pub fn cleanup_stale_temp_files(
    case_dir: &Path,
    active_tags: &[String],
) -> Vec<(PathBuf, Result<(), String>)> {
    let dir = case_dir.join(JOINED_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    // tag -> files (tags never contain '.').
    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(".join-") else {
            continue;
        };
        let Some((tag, _)) = rest.split_once('.') else {
            continue;
        };
        if active_tags.iter().any(|active| active == tag) || !entry.path().is_file() {
            continue;
        }
        groups
            .entry(tag.to_string())
            .or_default()
            .push(entry.path());
    }
    let now = std::time::SystemTime::now();
    let mut out = Vec::new();
    for (tag, files) in groups {
        let lock = dir.join(format!(".join-{tag}.lock"));
        let claim_path = dir.join(format!(".join-{tag}.claim.json"));
        let dead = if lock.is_file() {
            !lock_is_held(&lock)
        } else {
            files.iter().all(|path| {
                std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .is_some_and(|age| age.as_secs() >= STALE_TEMP_AGE_SECS)
            })
        };
        if !dead {
            continue;
        }
        // C3: a dead job's claim names the output/manifest it reserved or
        // published but never logged — those go first (the claim, read
        // here, is removed with the temp files below). A claim that cannot
        // be read keeps the tag's files, so the next sweep sees it again.
        if claim_path.is_file() {
            match read_join_claim(&claim_path) {
                Some(claim) => {
                    let removed = remove_unlogged_claimed(case_dir, &claim);
                    let failed = removed.iter().any(|(_, result)| result.is_err());
                    out.extend(removed);
                    if failed {
                        continue;
                    }
                }
                None => {
                    out.push((
                        claim_path.clone(),
                        Err(format!(
                            "이어붙이기 기록 파일을 읽지 못해 남은 결과물을 지우지 않았습니다: {}",
                            claim_path.display()
                        )),
                    ));
                    continue;
                }
            }
        }
        // The lock goes last so a crash mid-cleanup still leaves the tag
        // recognisable as dead.
        let mut ordered: Vec<PathBuf> = files.into_iter().filter(|p| *p != lock).collect();
        if lock.is_file() {
            ordered.push(lock);
        }
        for path in ordered {
            let result = remove_file_retry(&path);
            out.push((path, result));
        }
    }
    out
}

fn write_progress(path: &Path, stage: &str, done: u64, total: u64) {
    let body = json!({"stage": stage, "done": done, "total": total}).to_string();
    let temp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    // Best effort: progress is a UI hint, never evidence.
    if std::fs::write(&temp, body).is_ok() && std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub job_tag: String,
    pub tools: Tools,
    /// Kill ffmpeg after this many seconds (None = no limit).
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct JoinOutcome {
    pub plan_id: String,
    pub output_path: PathBuf,
    pub manifest_path: PathBuf,
    pub output_sha256: String,
    pub output_size_bytes: u64,
    pub manifest_sha256: String,
    pub duration_expected: f64,
    pub duration_observed: Option<f64>,
    pub duration_check_ok: bool,
    pub hash_mismatches: usize,
    pub segment_count: usize,
    pub summary_text: String,
}

impl JoinOutcome {
    /// The single JSON line `join-video` prints on success.
    pub fn to_json(&self) -> Value {
        json!({
            "ok": true,
            "plan_id": self.plan_id,
            "output_path": self.output_path.to_string_lossy(),
            "manifest_path": self.manifest_path.to_string_lossy(),
            "sha256": self.output_sha256,
            "size_bytes": self.output_size_bytes,
            "manifest_sha256": self.manifest_sha256,
            "duration_expected_seconds": r3(self.duration_expected),
            "duration_observed_seconds": self.duration_observed.map(r3),
            "duration_check_ok": self.duration_check_ok,
            "hash_mismatches": self.hash_mismatches,
            "segments": self.segment_count,
            "summary_text": self.summary_text,
        })
    }
}

/// Runs a checked plan. Order: disk check → live source hashes → ffmpeg
/// `-c copy` into `.part` → ffprobe verify → re-stat sources → output
/// SHA-256 → rename → atomic manifest → chained log. Any failure removes
/// the `.part`, the published output and manifest; temp files are always
/// removed (with retries).
pub fn run(case_dir: &Path, plan: &JoinPlan, options: &RunOptions) -> Result<JoinOutcome, String> {
    validate_job_tag(&options.job_tag)?;
    if let Some(issue) = plan.blocking.first() {
        return Err(format!("이어붙일 수 없습니다: {}", issue.message));
    }
    if plan.segments.len() < MIN_SEGMENTS {
        return Err("영상을 2개 이상 선택하십시오".to_string());
    }
    let joined_dir = case_dir.join(JOINED_DIR);
    std::fs::create_dir_all(&joined_dir)
        .map_err(|err| format!("failed to create {}: {err}", joined_dir.display()))?;
    let temps = temp_paths(case_dir, &options.job_tag);
    // B13: the run lock marks this tag live for every other process's
    // crash cleanup until the run ends.
    let lock = {
        use fs2::FileExt;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&temps.lock)
            .map_err(|err| format!("failed to create {}: {err}", temps.lock.display()))?;
        file.try_lock_exclusive().map_err(|_| {
            format!(
                "같은 작업 태그({})의 이어붙이기가 이미 실행 중입니다",
                options.job_tag
            )
        })?;
        file
    };
    for path in temps.all() {
        remove_file_retry(path)?;
    }
    let mut published: Vec<PathBuf> = Vec::new();
    let result = run_inner(case_dir, plan, options, &temps, &mut published);
    let mut cleanup_errors = Vec::new();
    for path in temps.all() {
        if let Err(err) = remove_file_retry(path) {
            cleanup_errors.push(err);
        }
    }
    {
        use fs2::FileExt;
        let _ = FileExt::unlock(&lock);
    }
    drop(lock);
    if let Err(err) = remove_file_retry(&temps.lock) {
        cleanup_errors.push(err);
    }
    match result {
        Ok(outcome) => {
            for err in &cleanup_errors {
                eprintln!("warning: {err}");
            }
            Ok(outcome)
        }
        Err(err) => {
            for path in &published {
                if let Err(clean) = remove_file_retry(path) {
                    cleanup_errors.push(clean);
                }
            }
            if cleanup_errors.is_empty() {
                Err(err)
            } else {
                Err(format!("{err}; {}", cleanup_errors.join("; ")))
            }
        }
    }
}

fn stat_matches(segment: &Segment) -> Result<(), String> {
    let meta = std::fs::metadata(&segment.source_path).map_err(|err| {
        format!(
            "원본을 다시 확인하지 못했습니다: {} ({err})",
            segment.source_path.display()
        )
    })?;
    // D17: nanosecond mtime (the same helper the audio cache uses) — a
    // whole-second comparison misses a source rewritten within the second
    // the plan was made in.
    let changed_mtime = match segment.modified_nanos {
        Some(nanos) => crate::audit::modified_nanos(&meta) != Some(nanos),
        None => modified_unix(&meta) != segment.modified_unix,
    };
    if meta.len() != segment.size_bytes || changed_mtime {
        return Err(format!(
            "이어붙이는 동안 원본이 바뀌었습니다: {}",
            segment.source_path.display()
        ));
    }
    Ok(())
}

fn output_base_name(plan: &JoinPlan) -> String {
    let code = plan.channel.unwrap_or('U');
    let stamp = plan
        .segments
        .first()
        .and_then(|s| s.start.as_ref())
        .map(|t| rectime::compact_stamp(t.secs))
        .unwrap_or_else(|| "order".to_string());
    format!("join_{code}_{stamp}_n{}", plan.segments.len())
}

/// Candidate `(<stem>.<ext>, <stem>.manifest.json)` names, `__N` suffixed
/// from the second on.
fn output_names(dir: &Path, base: &str, ext: &str, n: usize) -> (PathBuf, PathBuf) {
    let stem = if n == 1 {
        base.to_string()
    } else {
        format!("{base}__{n}")
    };
    (
        dir.join(format!("{stem}.{ext}")),
        dir.join(format!("{stem}.manifest.json")),
    )
}

/// One attempt to give a finished `.part` the final name `output` without
/// ever clobbering and without ever exposing partial bytes under `output`:
/// a hard link (fails if the name exists), else — FAT / exFAT have no hard
/// links — the name is reserved with `create_new` (B14: check and creation
/// are one atomic step, so two writers can never both take it) and the
/// `.part` is renamed over that empty reservation (C3: no byte copy, so a
/// kill can only ever leave the empty reservation, never a torn file).
/// `Ok(false)` = the name is taken. Shared by every `.part` publisher
/// (join, inode recovery, E01 export, clip export, selection export).
pub fn publish_part_as(part: &Path, output: &Path) -> Result<bool, String> {
    match std::fs::hard_link(part, output) {
        Ok(()) => {
            let _ = remove_file_retry(part);
            return Ok(true);
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(_) => {}
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
    {
        // Closed right away: Windows refuses to replace an open file.
        Ok(file) => drop(file),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => {
            return Err(format!(
                "failed to reserve output name {}: {err}",
                output.display()
            ));
        }
    }
    let mut last_err = None;
    for attempt in 0..5 {
        match std::fs::rename(part, output) {
            Ok(()) => {
                crate::util::sync_parent_directory(output);
                return Ok(true);
            }
            Err(err) => last_err = Some(err),
        }
        if attempt < 4 {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    // The empty reservation was ours (create_new): never leave it behind.
    let _ = remove_file_retry(output);
    Err(format!(
        "failed to publish {} as {}: {}",
        part.display(),
        output.display(),
        last_err.map(|e| e.to_string()).unwrap_or_default()
    ))
}

/// Publishes `.part` under the first free `<base>[__N]` name whose
/// manifest name is free too. Returns `(output, manifest)`.
///
/// C3: the manifest name is reserved first (`create_new`, so every other
/// join skips that stem), then `before_publish(output, manifest)` records
/// the claim, and only then does the output get its name — a kill at any
/// point leaves a claim naming exactly what this job reserved.
fn publish_part(
    part: &Path,
    dir: &Path,
    base: &str,
    ext: &str,
    before_publish: &mut dyn FnMut(&Path, &Path) -> Result<(), String>,
) -> Result<(PathBuf, PathBuf), String> {
    for n in 1..10_000usize {
        let (output, manifest) = output_names(dir, base, ext, n);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest)
        {
            Ok(file) => drop(file),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(format!(
                    "failed to reserve manifest name {}: {err}",
                    manifest.display()
                ));
            }
        }
        let release = |manifest: &Path| {
            let _ = remove_file_retry(manifest);
        };
        if output.exists() {
            release(&manifest);
            continue;
        }
        if let Err(err) = before_publish(&output, &manifest) {
            release(&manifest);
            return Err(err);
        }
        match publish_part_as(part, &output) {
            Ok(true) => return Ok((output, manifest)),
            Ok(false) => release(&manifest),
            Err(err) => {
                release(&manifest);
                return Err(err);
            }
        }
    }
    Err(format!(
        "no free output name for {base} in {}",
        dir.display()
    ))
}

/// C10: how long a pipe-reader thread may still run after its child
/// exited. The pipes reach EOF when the child exits — unless a grandchild
/// that escaped tree tracking inherited them; then the reader is abandoned
/// (its retained output is lost) instead of hanging the caller forever.
pub const READER_JOIN_CAP: Duration = Duration::from_secs(10);

/// Joins `handle` if it finishes within `cap`; `None` (thread left
/// detached) otherwise or when it panicked.
pub fn join_reader_bounded<T>(handle: std::thread::JoinHandle<T>, cap: Duration) -> Option<T> {
    let deadline = std::time::Instant::now() + cap;
    while !handle.is_finished() {
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.join().ok()
}

/// C1/C2/C4: the in-progress name of `final_path` written by process
/// `pid`: `.<file name>.ft<pid>.part` beside it. Hidden (leading dot, so
/// packaging and listings skip it) and pid-tagged, so the server can sweep
/// exactly what a killed step left ([`sweep_pid_parts`]).
pub fn pid_part_path(final_path: &Path, pid: u32) -> PathBuf {
    let name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());
    final_path.with_file_name(format!(".{name}.ft{pid}.part"))
}

/// Whether `name` is an in-progress name (file or directory) of `pid`
/// (`None` = any pid).
pub fn is_pid_part_name(name: &str, pid: Option<u32>) -> bool {
    if !name.starts_with('.') || name.ends_with(PRESERVED_PART_SUFFIX) {
        return false;
    }
    let Some(pos) = name.rfind(".ft") else {
        return false;
    };
    let rest = &name[pos + 3..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with(".part") {
        return false;
    }
    pid.is_none_or(|pid| digits == pid.to_string())
}

fn remove_path_retry(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        let mut last = None;
        for attempt in 0..5 {
            match std::fs::remove_dir_all(path) {
                Ok(()) => return Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(err) => last = Some(err),
            }
            if attempt < 4 {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        Err(format!(
            "임시 폴더를 지우지 못했습니다: {} ({})",
            path.display(),
            last.map(|e| e.to_string()).unwrap_or_default()
        ))
    } else {
        remove_file_retry(path)
    }
}

/// Removes every in-progress file/directory process `pid` left in `dir`
/// (the server's sweep after it killed that step). Returns each removal.
pub fn sweep_pid_parts(dir: &Path, pid: u32) -> Vec<(PathBuf, Result<(), String>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| is_pid_part_name(&entry.file_name().to_string_lossy(), Some(pid)))
        // R6-S5: a kept (finished but unrenamed) package is never swept,
        // not even right after its own step was killed.
        .filter(|entry| !part_is_preserved(&entry.path()))
        .map(|entry| {
            let path = entry.path();
            let result = remove_path_retry(&path);
            (path, result)
        })
        .collect()
}

/// Newest modification time under `path` (a directory is walked).
fn newest_mtime(path: &Path) -> Option<std::time::SystemTime> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let own = meta.modified().ok();
    if !meta.is_dir() {
        return own;
    }
    let mut newest = own;
    for entry in std::fs::read_dir(path).ok()?.flatten() {
        if let Some(time) = newest_mtime(&entry.path())
            && newest.is_none_or(|current| time > current)
        {
            newest = Some(time);
        }
    }
    newest
}

/// The writer pid an in-progress name carries (`.….ft<pid>.part…`).
pub fn part_name_pid(name: &str) -> Option<u32> {
    if !is_pid_part_name(name, None) {
        return None;
    }
    let pos = name.rfind(".ft")?;
    let digits: String = name[pos + 3..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// R5-S12: the marker a preserved package work folder (its publish
/// failed) carried INSIDE it — still honoured for folders kept by R5.
pub const PRESERVED_PART_MARKER: &str = "FRAMETRACE-KEEP.txt";

/// R6-S5: suffix of the marker BESIDE a preserved work folder
/// (`.package_<ts>.ft<pid>.part.FRAMETRACE-KEEP.txt`). A marker inside
/// the folder became part of the finished package and failed its
/// verification; beside it, it starts with `.` and is never packaged.
pub const PRESERVED_PART_SUFFIX: &str = ".FRAMETRACE-KEEP.txt";

/// R6-S5: the marker path that keeps `part` (a work folder) from sweeps.
pub fn preserved_part_marker(part: &Path) -> PathBuf {
    let mut name = part.as_os_str().to_os_string();
    name.push(PRESERVED_PART_SUFFIX);
    PathBuf::from(name)
}

/// Whether `part` is marked preserved (beside it, or the R5 marker inside).
pub fn part_is_preserved(part: &Path) -> bool {
    preserved_part_marker(part).exists() || part.join(PRESERVED_PART_MARKER).exists()
}

/// R6-S8: the default package name a work folder `.package_<ts>.ft<pid>
/// .part` reserved (`package_<ts>`), if `name` is one.
pub fn package_reservation_of(name: &str) -> Option<String> {
    let pid = part_name_pid(name)?;
    let reserved = name
        .strip_prefix('.')?
        .split_once(&format!(".ft{pid}.part"))?
        .0;
    reserved
        .starts_with("package_")
        .then(|| reserved.to_string())
}

/// R7-S5: the file inside a reserved (still empty) default package folder
/// that names the package-case owning the reservation (`pid=<pid>`). A
/// work folder `.package_<ts>.ft<pid>.part` only says which name its
/// writer TRIED: when two package-cases race for one name, the loser's
/// work folder names the winner's reservation — and R6 swept that live
/// reservation along with the loser's dead work folder.
pub const RESERVATION_MARKER: &str = ".frametrace-reservation";

/// R7-S5: the text of [`RESERVATION_MARKER`] for `pid`.
pub fn reservation_marker_text(pid: u32) -> String {
    format!("pid={pid}\n")
}

/// R7-S5: who holds the reservation `dir`: `Some(Some(pid))` when it
/// holds only a marker naming `pid`, `Some(None)` when it is empty (no
/// marker yet — or a pre-R7 reservation), `None` when it is not a bare
/// reservation (missing, not a folder, other content, unreadable or
/// foreign marker text) and must be left alone.
pub fn reservation_owner(dir: &Path) -> Option<Option<u32>> {
    let mut entries = std::fs::read_dir(dir).ok()?;
    let Some(first) = entries.next() else {
        return Some(None);
    };
    let first = first.ok()?;
    if entries.next().is_some() || first.file_name() != RESERVATION_MARKER {
        return None;
    }
    let text = std::fs::read_to_string(first.path()).ok()?;
    let pid = text.trim().strip_prefix("pid=")?.parse().ok()?;
    Some(Some(pid))
}

/// R7-S5: removes the reservation `dir` only when its marker names `pid`
/// (marker first, then the then-empty folder — `remove_dir` never removes
/// a folder with content). `true` when it was removed.
pub fn release_reservation(dir: &Path, pid: u32) -> bool {
    matches!(
        release_own_reservation(dir, pid),
        ReservationRelease::Released
    )
}

/// R8-S5: how releasing a reservation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservationRelease {
    /// The marker named `pid`; marker and folder are gone.
    Released,
    /// Not this pid's reservation (another marker, other content, or
    /// missing before anything was removed): left alone.
    NotOurs,
    /// It was this pid's, but removing it kept failing (on Windows an
    /// antivirus scan or the indexer briefly holds a handle) — the folder
    /// may remain, empty or still marked. NOT a sign that someone else
    /// took the name.
    Failed(String),
}

/// Delays between the R8-S5 release attempts (5 attempts in all).
const RELEASE_BACKOFF_MS: [u64; 4] = [100, 250, 500, 1000];

#[cfg(test)]
thread_local! {
    /// R8-S5 test hook: how many reservation removals (marker or folder)
    /// fail before one works.
    pub(crate) static FAIL_RESERVATION_REMOVALS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

fn reservation_remove(remove: impl FnOnce() -> std::io::Result<()>) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_RESERVATION_REMOVALS.with(|left| {
        let n = left.get();
        left.set(n.saturating_sub(1));
        n > 0
    }) {
        return Err(std::io::Error::other("injected removal failure"));
    }
    remove()
}

/// R8-S5: [`release_reservation`] that says why it did not release. R7
/// tried the removal once and reported any failure as "not ours", so a
/// transient removal failure at publish time made package-case keep the
/// finished package aside as if its name had been stolen. Removal is now
/// retried with backoff; after the marker is gone, an EMPTY folder is
/// still this release's to remove.
pub fn release_own_reservation(dir: &Path, pid: u32) -> ReservationRelease {
    let mut marker_removed = false;
    let mut last_error = String::new();
    for attempt in 0..=RELEASE_BACKOFF_MS.len() {
        let owner = reservation_owner(dir);
        let step = match owner {
            Some(Some(owner)) if owner == pid && !marker_removed => {
                reservation_remove(|| std::fs::remove_file(dir.join(RESERVATION_MARKER)))
                    .map(|()| marker_removed = true)
                    .and_then(|()| reservation_remove(|| std::fs::remove_dir(dir)))
            }
            Some(None) if marker_removed => reservation_remove(|| std::fs::remove_dir(dir)),
            None if marker_removed && !dir.exists() => return ReservationRelease::Released,
            _ => return ReservationRelease::NotOurs,
        };
        match step {
            Ok(()) => return ReservationRelease::Released,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && marker_removed => {
                return ReservationRelease::Released;
            }
            Err(err) => last_error = err.to_string(),
        }
        if let Some(delay) = RELEASE_BACKOFF_MS.get(attempt) {
            std::thread::sleep(Duration::from_millis(*delay));
        }
    }
    ReservationRelease::Failed(format!(
        "예약 폴더 {}를 지우지 못했습니다: {last_error}",
        dir.display()
    ))
}

/// R8-S5: what is left of THIS process's reservation at `dir` after a
/// failed release is cleared before publishing over it: its marker (when
/// it still names `pid`), then the folder when it is empty. Best effort —
/// the publish rename that follows decides.
pub fn clear_own_reservation_remnant(dir: &Path, pid: u32) {
    if reservation_owner(dir) == Some(Some(pid)) {
        let _ = std::fs::remove_file(dir.join(RESERVATION_MARKER));
    }
    if reservation_owner(dir) == Some(None) {
        let _ = std::fs::remove_dir(dir);
    }
}

/// R8-S6: the staging folder a reservation is prepared in (marker
/// written) before it is renamed onto the reserved name — a pid-tagged
/// in-progress name, so a kill sweep or stale sweep removes it when the
/// writer died in between.
pub fn reservation_staging_path(reserved: &Path, pid: u32) -> PathBuf {
    let name = reserved
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "package".to_string());
    pid_part_path(&reserved.with_file_name(format!("{name}.reserve")), pid)
}

/// Crash recovery for `.part` leftovers of OTHER processes in `dir`.
///
/// R5-S5: an in-progress name is removed only when its newest write is at
/// least [`STALE_TEMP_AGE_SECS`] old AND the writer its name carries is
/// provably not running: no process with that pid, or one that started
/// after the part was born (a recycled pid). Age alone used to delete the
/// work file of a live job that simply had not written for an hour (a
/// long hash or `ewfexport` stall). This process's own parts and folders
/// marked [`PRESERVED_PART_MARKER`] are never touched.
pub fn sweep_stale_parts(dir: &Path) -> Vec<(PathBuf, Result<(), String>)> {
    sweep_stale_parts_with(dir, &crate::util::probe_process, STALE_TEMP_AGE_SECS)
}

/// [`sweep_stale_parts`] with the process probe and the minimum age
/// injected (tests).
pub fn sweep_stale_parts_with(
    dir: &Path,
    probe: &dyn Fn(u32) -> crate::util::ProcessProbe,
    min_age_secs: u64,
) -> Vec<(PathBuf, Result<(), String>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let own = std::process::id();
    let now = std::time::SystemTime::now();
    let old_enough = |path: &Path| {
        newest_mtime(path)
            .and_then(|time| now.duration_since(time).ok())
            .is_some_and(|age| age.as_secs() >= min_age_secs)
    };
    let writer_gone = |path: &Path, pid: u32| {
        let born = std::fs::symlink_metadata(path)
            .ok()
            .and_then(|meta| meta.created().or_else(|_| meta.modified()).ok());
        !crate::util::writer_may_be_alive(probe(pid), born)
    };
    let mut swept: Vec<(PathBuf, Result<(), String>)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let pid = part_name_pid(&name)?;
            (pid != own).then_some((entry, pid))
        })
        .filter(|(entry, _)| old_enough(&entry.path()))
        .filter(|(entry, _)| !part_is_preserved(&entry.path()))
        .filter(|(entry, pid)| writer_gone(&entry.path(), *pid))
        .map(|(entry, pid)| {
            let path = entry.path();
            let result = remove_path_retry(&path);
            // R6-S8: the dead package-case's empty name reservation goes
            // with its work folder. R7-S5: only when the reservation's
            // marker names that writer (or it is still empty — the writer
            // died before writing the marker): the name a work folder
            // carries may be another, live package-case's reservation.
            if result.is_ok()
                && let Some(reserved) = package_reservation_of(&entry.file_name().to_string_lossy())
            {
                let reserved = dir.join(reserved);
                match reservation_owner(&reserved) {
                    Some(Some(owner)) if owner == pid => {
                        release_reservation(&reserved, pid);
                    }
                    Some(None) => {
                        let _ = std::fs::remove_dir(&reserved);
                    }
                    _ => {}
                }
            }
            (path, result)
        })
        .collect();
    // R7-S5: a reservation whose owner died after removing its work
    // folder (failed run, killed before releasing the name) is named by
    // no work folder; its marker identifies the dead owner.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_name().to_string_lossy().starts_with("package_") {
                continue;
            }
            if let Some(Some(owner)) = reservation_owner(&path)
                && owner != own
                && old_enough(&path)
                && writer_gone(&path, owner)
                && release_reservation(&path, owner)
            {
                swept.push((path, Ok(())));
            }
        }
    }
    swept
}

fn run_ffmpeg(
    joined_dir: &Path,
    args: &[String],
    options: &RunOptions,
    temps: &TempPaths,
    total_bytes: u64,
) -> Result<(), String> {
    let ffmpeg = resolve_tool_binary(&options.tools.ffmpeg, &["ffmpeg"])
        .map_err(|err| format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)"))?;
    let mut command = Command::new(&ffmpeg);
    command
        .args(args)
        .current_dir(joined_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    crate::util::hide_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|err| format!("failed to run ffmpeg: {err}"))?;
    let mut stderr = child.stderr.take().expect("piped stderr");
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut buffer);
        buffer
    });
    let started = std::time::Instant::now();
    let status = loop {
        use wait_timeout::ChildExt;
        match child.wait_timeout(Duration::from_millis(500)) {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let written = std::fs::metadata(&temps.part).map(|m| m.len()).unwrap_or(0);
                write_progress(&temps.progress, "concat", written, total_bytes);
                if let Some(limit) = options.timeout_secs
                    && started.elapsed() > Duration::from_secs(limit)
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "ffmpeg did not finish within {limit}s and was terminated"
                    ));
                }
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("failed to wait for ffmpeg: {err}"));
            }
        }
    };
    // C10: a grandchild still holding the pipe must not hang the join.
    let stderr = join_reader_bounded(reader, READER_JOIN_CAP).unwrap_or_default();
    if !status.success() {
        let text = String::from_utf8_lossy(&stderr);
        let tail: String = text
            .lines()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("ffmpeg join failed: {}", tail.trim()));
    }
    Ok(())
}

fn run_inner(
    case_dir: &Path,
    plan: &JoinPlan,
    options: &RunOptions,
    temps: &TempPaths,
    published: &mut Vec<PathBuf>,
) -> Result<JoinOutcome, String> {
    let joined_dir = case_dir.join(JOINED_DIR);
    let total_bytes = plan.total_bytes();
    let n = plan.segments.len() as u64;

    // 1. Disk space for the output (stream copy ≈ source bytes).
    let needed = (total_bytes as f64 * DISK_FACTOR).ceil() as u64;
    crate::diskspace::ensure_available(&joined_dir, needed, "join-video")?;

    // 2. Live source hashes.
    let mut live_hashes = Vec::with_capacity(plan.segments.len());
    for (index, segment) in plan.segments.iter().enumerate() {
        write_progress(&temps.progress, "hash", index as u64, n);
        stat_matches(segment)?;
        live_hashes.push(audit::digest_file(&segment.source_path)?);
    }
    write_progress(&temps.progress, "hash", n, n);

    // 3. Concat list (UTF-8, no BOM) + ffmpeg stream copy into .part.
    let list = concat_list(plan)?;
    std::fs::write(&temps.concat_list, list.as_bytes()).map_err(|err| {
        format!(
            "failed to write concat list {}: {err}",
            temps.concat_list.display()
        )
    })?;
    let args = ffmpeg_argv(&options.job_tag, plan.container);
    if let Some(bad) = args.iter().find(|arg| !arg.is_ascii()) {
        return Err(format!("internal error: non-ASCII ffmpeg argument {bad:?}"));
    }
    write_progress(&temps.progress, "concat", 0, total_bytes);
    run_ffmpeg(&joined_dir, &args, options, temps, total_bytes)?;
    let part_size = std::fs::metadata(&temps.part).map(|m| m.len()).unwrap_or(0);
    if part_size == 0 {
        return Err("ffmpeg reported success but wrote no output bytes".to_string());
    }

    // 4. Verify the output.
    write_progress(&temps.progress, "verify", 0, 1);
    let probed = probe_file(&options.tools.ffprobe, &temps.part);
    if !probed.ok {
        return Err(format!(
            "joined output failed ffprobe verification: {}",
            probed.error.unwrap_or_default()
        ));
    }
    let Some(out_video) = probed.video.clone() else {
        return Err("joined output has no video stream".to_string());
    };
    // C14: packet-level check of every segment boundary in the output.
    let segment_check = verify_segments(
        &options.tools.ffprobe,
        &options.tools.ffmpeg,
        &temps.part,
        plan,
        options.timeout_secs,
    )?;
    // R5-S13: edit-list pre-roll the output presents (measured by the
    // segment check) makes it that much longer than the plan's sum.
    let expected = plan.totals.duration_seconds + segment_check.presented_preroll_total();
    let observed = probed.duration_seconds;
    // C14: max(1 s, 0.2 %) — the old max(1 s, 1 %) let a whole short
    // segment go missing from a long join unnoticed.
    let tolerance = duration_tolerance(expected);
    let duration_check_ok = observed.is_some_and(|value| (value - expected).abs() <= tolerance);
    if !duration_check_ok || !segment_check.ok {
        let mut problems = Vec::new();
        if !duration_check_ok {
            problems.push(match observed {
                Some(value) => format!(
                    "출력 길이 {value:.3}초가 원본 합계 {expected:.3}초와 허용치 {tolerance:.3}초 넘게 다릅니다"
                ),
                None => "출력 길이를 확인할 수 없습니다".to_string(),
            });
        }
        problems.extend(segment_check.problems.iter().cloned());
        return Err(format!(
            "이어붙인 결과물 검증 실패 — 빠진 구간이 있을 수 있어 게시하지 않았습니다: {}",
            problems.join("; ")
        ));
    }

    // 5. Sources unchanged while ffmpeg read them.
    for segment in &plan.segments {
        stat_matches(segment)?;
    }

    // 6. Output identity, publish, manifest, log.
    let output_sha256 = audit::digest_file(&temps.part)?;
    // C3 / B8: the claim names exactly what this job reserves BEFORE the
    // output gets its name, so a kill during publishing still leaves a
    // record the server (or the next join's stale sweep) cleans up from —
    // never touching another job's output.
    let claim_path = temps.claim.clone();
    let job_tag = options.job_tag.clone();
    let (output_path, manifest_path) = publish_part(
        &temps.part,
        &joined_dir,
        &output_base_name(plan),
        plan.container.as_str(),
        &mut |output, manifest| {
            write_join_claim(
                &claim_path,
                &JoinClaim {
                    output: output.to_path_buf(),
                    manifest: manifest.to_path_buf(),
                    published: Some(false),
                    part_size: Some(part_size),
                },
                &job_tag,
            )
        },
    )?;
    published.push(manifest_path.clone());
    published.push(output_path.clone());
    write_join_claim(
        &temps.claim,
        &JoinClaim {
            output: output_path.clone(),
            manifest: manifest_path.clone(),
            published: Some(true),
            part_size: Some(part_size),
        },
        &options.job_tag,
    )?;
    write_progress(&temps.progress, "verify", 1, 1);
    let record_id = joined_record_id(&output_sha256).unwrap_or_default();
    // B2: a filename-order join has no recorded time to judge junctions:
    // counts are unknown (null), not zero.
    let judged = plan.order_basis != OrderBasis::FilenameOrder;
    let gaps = judged.then_some(plan.totals.gaps);
    let overlaps = judged.then_some(plan.totals.overlaps);

    let ffmpeg_version = command_version(&options.tools.ffmpeg, &["ffmpeg"], "-version");
    let list_lines: Vec<String> = list.lines().skip(1).map(str::to_string).collect();
    let created_unix = now_unix()?;
    let case_id = crate::selection::read_case_id(case_dir).unwrap_or_default();
    let output_name = output_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut hash_mismatches = 0usize;
    let segments_json = plan
        .segments
        .iter()
        .zip(&live_hashes)
        .enumerate()
        .map(|(index, (segment, live))| {
            let mismatch = segment
                .sha256_indexed
                .as_deref()
                .is_some_and(|indexed| !indexed.eq_ignore_ascii_case(live));
            if mismatch {
                hash_mismatches += 1;
            }
            let start = segment.display_start();
            json!({
                "index": index + 1,
                "record_id": segment.id,
                "source_path": segment.source_path.to_string_lossy(),
                "size_bytes": segment.size_bytes,
                "modified_unix": segment.modified_unix,
                "sha256_at_join": live,
                "sha256_indexed": segment.sha256_indexed,
                "hash_mismatch": mismatch,
                "recorded_start": {
                    "text": start.as_ref().map(|s| s.text.clone()),
                    "source": start.as_ref().map(|s| s.source),
                },
                "duration_seconds": segment.probe.duration_seconds.map(r3),
                "output_offset_seconds": r3(segment.output_offset_seconds),
                "output_end_seconds": r3(segment.output_offset_seconds + segment.duration()),
            })
        })
        .collect::<Vec<_>>();
    let mut not_carried = Vec::new();
    if plan.segments.iter().any(|s| s.probe.other_streams > 0) {
        not_carried.push(NOT_CARRIED_DATA);
    }
    if plan.segments.iter().any(|s| s.probe.video_streams > 1) {
        not_carried.push(NOT_CARRIED_VIDEO);
    }
    let manifest = json!({
        "schema_version": 1,
        "kind": "frametrace-join-manifest",
        "plan_id": plan.plan_id,
        "record_id": record_id,
        "job_tag": options.job_tag,
        "case_id": case_id,
        "created_unix": created_unix,
        "tool": {"name": "frametrace", "version": env!("CARGO_PKG_VERSION")},
        "output": {
            "path_rel": format!("{JOINED_DIR}/{output_name}"),
            "sha256": output_sha256,
            "size_bytes": part_size,
            "container": plan.container.as_str(),
            "duration_seconds_probed": observed.map(r3),
            "video": {
                "codec": out_video.codec,
                "width": out_video.width,
                "height": out_video.height,
                "r_frame_rate": out_video.r_frame_rate,
            },
            "audio": probed.audio.first().map(audio_json),
        },
        "method": {
            "mode": "stream-copy",
            "reencode": false,
            "demuxer": "concat",
            "ffmpeg_version": ffmpeg_version,
            "ffmpeg_argv": args,
            "concat_list_sha256": crate::sha256::digest_bytes(list.as_bytes()),
            "concat_list": list_lines,
        },
        "order": {
            "basis": plan.order_basis.as_str(),
            "tolerance_seconds": plan.tolerance_seconds,
            "time_note": plan.order_basis.time_note(),
        },
        "channel": plan.channel_json(),
        "segments": segments_json,
        "junctions": plan.junctions.iter().map(junction_json).collect::<Vec<_>>(),
        "totals": {
            "segments": plan.totals.segments,
            "duration_seconds": r3(plan.totals.duration_seconds),
            "gaps": gaps,
            "gap_seconds": judged.then(|| r3(plan.totals.gap_seconds)),
            "overlaps": overlaps,
            "overlap_seconds": judged.then(|| r3(plan.totals.overlap_seconds)),
            "unknown_junctions": plan.totals.unknown_junctions,
        },
        "duration_check": {
            "expected": r3(expected),
            "observed": observed.map(r3),
            "tolerance_seconds": r3(tolerance),
            "ok": duration_check_ok,
        },
        "segment_check": segment_check.to_json(),
        "not_carried": not_carried,
        "caveats": CAVEATS
            .iter()
            .map(|text| text.to_string())
            .chain(segment_check.notes.iter().cloned())
            .collect::<Vec<_>>(),
    });
    let manifest_text = format!(
        "{}\n",
        serde_json::to_string_pretty(&manifest)
            .map_err(|err| format!("failed to serialize join manifest: {err}"))?
    );
    crate::util::write_text_atomic(&manifest_path, &manifest_text).map_err(|err| {
        format!(
            "failed to write join manifest {}: {err}",
            manifest_path.display()
        )
    })?;
    let manifest_sha256 = crate::sha256::digest_bytes(manifest_text.as_bytes());

    let log_line = json!({
        "schema_version": 1,
        "event": "join-video",
        "record_id": record_id,
        // C13: the server attributes a log entry to its job by this tag
        // only (never by plan id + time, which another process's join of
        // the same plan also matches).
        "job_tag": options.job_tag,
        "joined_unix": created_unix,
        "output_path": output_path.to_string_lossy(),
        "output_sha256": output_sha256,
        "output_size_bytes": part_size,
        "manifest_path": manifest_path.to_string_lossy(),
        "manifest_sha256": manifest_sha256,
        "plan_id": plan.plan_id,
        "segment_count": plan.segments.len(),
        "duration_seconds": r3(plan.totals.duration_seconds),
        "source_ids": plan.ids(),
        "source_sha256": live_hashes,
        "hash_mismatches": hash_mismatches,
        "order_basis": plan.order_basis.as_str(),
        "channel": plan.channel.map(|c| c.to_string()),
        "tolerance_seconds": plan.tolerance_seconds,
        "gaps": gaps,
        "overlaps": overlaps,
        "unknown_junctions": plan.totals.unknown_junctions,
        "mode": "stream-copy",
        "duration_check_ok": duration_check_ok,
        "segment_check_ok": segment_check.ok,
        "ffmpeg_version": ffmpeg_version,
        "command": "ffmpeg",
        "command_args": args,
    });
    audit::append_chained_jsonl(&joined_dir.join(LOG_FILE), &log_line.to_string())?;

    Ok(JoinOutcome {
        plan_id: plan.plan_id.clone(),
        output_path,
        manifest_path,
        output_sha256,
        output_size_bytes: part_size,
        manifest_sha256,
        duration_expected: expected,
        duration_observed: observed,
        duration_check_ok,
        hash_mismatches,
        segment_count: plan.segments.len(),
        summary_text: plan.summary_text(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video() -> VideoParams {
        VideoParams {
            codec: Some("h264".into()),
            profile: Some("High".into()),
            width: Some(1920),
            height: Some(1080),
            pix_fmt: Some("yuv420p".into()),
            r_frame_rate: Some("30/1".into()),
            avg_frame_rate: Some("30/1".into()),
            time_base: Some("1/15360".into()),
            extradata_hash: Some("SHA256:aa".into()),
            fallback: VIDEO_FALLBACK_FIELDS
                .iter()
                .map(|(key, _)| (*key, Some(format!("v-{key}"))))
                .collect(),
        }
    }

    fn aac() -> AudioParams {
        AudioParams {
            codec: Some("aac".into()),
            sample_rate: Some(48000),
            channels: Some(1),
            profile: Some("LC".into()),
            extradata_hash: Some("SHA256:asc".into()),
        }
    }

    fn seg(id: &str, name: &str, duration: f64) -> Segment {
        let source_path = PathBuf::from(format!("/evidence/{name}"));
        let source_text = source_path.to_string_lossy().to_string();
        Segment {
            id: id.into(),
            name_time: rectime::parse_record_time(&["", name, &source_text]),
            channel: channel_code(name),
            name: name.into(),
            source_path,
            size_bytes: 1000,
            modified_unix: Some(1),
            modified_nanos: None,
            sha256_indexed: Some(format!("hash-{id}")),
            vendor: Some("Generic media".into()),
            creation: None,
            probe: ProbeFacts {
                ok: true,
                error: None,
                duration_seconds: Some(duration),
                video: Some(video()),
                video_streams: 1,
                audio: vec![aac()],
                other_streams: 0,
                creation_time: None,
            },
            start: None,
            output_offset_seconds: 0.0,
        }
    }

    fn plan_of(segments: Vec<Segment>) -> JoinPlan {
        build_plan(
            segments
                .into_iter()
                .map(|s| SegmentInput::Found(Box::new(s)))
                .collect(),
            DEFAULT_TOLERANCE_SECONDS,
        )
    }

    fn codes(issues: &[Issue]) -> Vec<&'static str> {
        issues.iter().map(|i| i.code).collect()
    }

    #[test]
    fn channel_port_matches_viewer_rules() {
        assert_eq!(channel_code("DCIM/FRONT/20260910_120000.mp4"), Some('F'));
        assert_eq!(channel_code("20260910_120000_R.mp4"), Some('R'));
        assert_eq!(channel_code("20260910_120000_r.MP4"), Some('R'));
        assert_eq!(channel_code("NORMAL/20260910_120000_I.mp4"), Some('I'));
        assert_eq!(channel_code("cam_B"), Some('B'));
        assert_eq!(channel_code("clip_F2"), Some('F'), "[a-z0-9]*$ tail");
        assert_eq!(channel_code("clip_Fx.mp4"), None, "dot after alnum tail");
        assert_eq!(channel_code("20260910_120000.mp4"), None);
        assert_eq!(channel_code("Rear-cam/a.mp4"), Some('R'));
        assert_eq!(channel_code("inside/a.mp4"), Some('I'));
        // Leading space from `${originalPath} ${name}` lets a first token match.
        assert_eq!(channel_code("F_20260910.mp4"), Some('F'));
        // BlackVue / Korean folders are NOT recognised (owner decision Q5).
        assert_eq!(channel_code("20260910_120000_NF.mp4"), None);
        assert_eq!(channel_code("전방/20260910_120000.mp4"), None);
    }

    #[test]
    fn order_basis_filename_time_and_offsets() {
        let plan = plan_of(vec![
            seg("vid_2", "FRONT/20260910_120100_F.mp4", 60.0),
            seg("vid_1", "FRONT/20260910_120000_F.mp4", 60.0),
            seg("vid_3", "FRONT/20260910_120200_F.mp4", 30.5),
        ]);
        assert_eq!(plan.order_basis, OrderBasis::FilenameTime);
        assert_eq!(plan.ids(), vec!["vid_1", "vid_2", "vid_3"]);
        assert!(plan.blocking.is_empty(), "{:?}", plan.blocking);
        assert_eq!(plan.segments[2].output_offset_seconds, 120.0);
        assert_eq!(plan.totals.duration_seconds, 150.5);
        assert_eq!(plan.channel, Some('F'));
        assert_eq!(plan.container, Container::Mp4);
        assert!(
            plan.junctions
                .iter()
                .all(|j| j.kind == JunctionKind::Continuous)
        );
    }

    #[test]
    fn creation_time_basis_and_mixed_fallback() {
        let mut a = seg("vid_1", "a.mp4", 10.0);
        let mut b = seg("vid_2", "b.mp4", 10.0);
        a.creation = Some(RecordedStart {
            secs: 1000,
            text: "x".into(),
            source: "creation_time",
        });
        b.creation = Some(RecordedStart {
            secs: 1013,
            text: "y".into(),
            source: "creation_time",
        });
        let plan = plan_of(vec![b.clone(), a.clone()]);
        assert_eq!(plan.order_basis, OrderBasis::CreationTime);
        assert_eq!(plan.ids(), vec!["vid_1", "vid_2"]);
        assert_eq!(plan.junctions[0].kind, JunctionKind::Gap);
        assert_eq!(plan.junctions[0].delta_seconds, Some(3.0));

        // One file has a file-name time, the other only creation_time: no
        // single basis covers both → creation-time only if ALL have it.
        let c = seg("vid_3", "20260910_120000.mp4", 10.0);
        let plan = plan_of(vec![a.clone(), c]);
        assert_eq!(plan.order_basis, OrderBasis::FilenameOrder);
        assert!(codes(&plan.warnings).contains(&"order-fallback"));
        assert!(
            plan.junctions
                .iter()
                .all(|j| j.kind == JunctionKind::Unknown && j.delta_seconds.is_none())
        );
        assert!(plan.summary_text().ends_with("끊김·겹침 확인 불가"));
    }

    #[test]
    fn date_only_names_never_order_by_time() {
        let plan = plan_of(vec![
            seg("vid_1", "20260910/clip10.mp4", 5.0),
            seg("vid_2", "20260910/clip9.mp4", 5.0),
        ]);
        assert_eq!(plan.order_basis, OrderBasis::FilenameOrder);
        assert_eq!(plan.ids(), vec!["vid_2", "vid_1"], "natural order");
    }

    #[test]
    fn junction_boundaries_are_inclusive() {
        assert_eq!(classify_junction(1.0, 1.0), JunctionKind::Continuous);
        assert_eq!(classify_junction(-1.0, 1.0), JunctionKind::Continuous);
        assert_eq!(classify_junction(1.001, 1.0), JunctionKind::Gap);
        assert_eq!(classify_junction(-1.001, 1.0), JunctionKind::Overlap);
        // 61 s apart with 60 s duration → exactly +1.0 → continuous.
        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000_F.mp4", 60.0),
            seg("vid_2", "20260910_120101_F.mp4", 60.0),
        ]);
        assert_eq!(plan.junctions[0].kind, JunctionKind::Continuous);
    }

    #[test]
    fn same_start_and_duplicates_block() {
        let plan = plan_of(vec![
            seg("vid_1", "A/20260910_120000_F.mp4", 60.0),
            seg("vid_2", "B/20260910_120000_F.mp4", 60.0),
        ]);
        assert!(codes(&plan.blocking).contains(&"same-start"));
        assert!(codes(&plan.warnings).contains(&"folders-differ"));

        let a = seg("vid_1", "20260910_120000_F.mp4", 60.0);
        let mut b = seg("vid_2", "20260910_120100_F.mp4", 60.0);
        b.sha256_indexed = a.sha256_indexed.clone();
        let plan = plan_of(vec![a.clone(), b]);
        let dup = plan
            .blocking
            .iter()
            .find(|i| i.code == "duplicate-file")
            .unwrap();
        assert_eq!(dup.ids, vec!["vid_1", "vid_2"]);

        let mut c = seg("vid_3", "20260910_120100_F.mp4", 60.0);
        c.source_path = a.source_path.clone();
        c.sha256_indexed = None;
        assert!(codes(&plan_of(vec![a, c]).blocking).contains(&"duplicate-file"));
    }

    #[test]
    fn channel_mixed_and_unknown() {
        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000_F.mp4", 60.0),
            seg("vid_2", "20260910_120100_R.mp4", 60.0),
        ]);
        let issue = plan
            .blocking
            .iter()
            .find(|i| i.code == "channel-mixed")
            .unwrap();
        assert_eq!(issue.message, "채널이 섞여 있습니다 (전방 1 · 후방 1)");
        assert_eq!(issue.ids, vec!["vid_2"]);

        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000_F.mp4", 60.0),
            seg("vid_2", "20260910_120100.mp4", 60.0),
        ]);
        assert!(
            codes(&plan.blocking).contains(&"channel-mixed"),
            "partly known"
        );

        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000.mp4", 60.0),
            seg("vid_2", "20260910_120100.mp4", 60.0),
        ]);
        assert!(plan.blocking.is_empty());
        assert!(codes(&plan.warnings).contains(&"channel-unknown"));
        assert!(
            plan.summary_text()
                .starts_with("채널 미확인 · 2개 · 2분 00초")
        );
    }

    #[test]
    fn stream_mismatch_matrix_one_field_at_a_time() {
        type Mutator = fn(&mut Segment);
        let cases: [(&str, Mutator); 8] = [
            ("codec", |s| {
                s.probe.video.as_mut().unwrap().codec = Some("hevc".into())
            }),
            ("profile", |s| {
                s.probe.video.as_mut().unwrap().profile = Some("Main".into())
            }),
            ("width", |s| {
                s.probe.video.as_mut().unwrap().width = Some(1280)
            }),
            ("height", |s| {
                s.probe.video.as_mut().unwrap().height = Some(720)
            }),
            ("pix_fmt", |s| {
                s.probe.video.as_mut().unwrap().pix_fmt = Some("yuvj420p".into())
            }),
            ("r_frame_rate", |s| {
                s.probe.video.as_mut().unwrap().r_frame_rate = Some("25/1".into())
            }),
            ("time_base", |s| {
                s.probe.video.as_mut().unwrap().time_base = Some("1/90000".into())
            }),
            ("extradata", |s| {
                s.probe.video.as_mut().unwrap().extradata_hash = Some("SHA256:bb".into())
            }),
        ];
        for (label, mutate) in cases {
            let a = seg("vid_1", "20260910_120000_F.mp4", 60.0);
            let mut b = seg("vid_2", "20260910_120100_F.mp4", 60.0);
            mutate(&mut b);
            let plan = plan_of(vec![a, b]);
            let issues: Vec<&Issue> = plan
                .blocking
                .iter()
                .filter(|i| i.code == "stream-mismatch")
                .collect();
            assert_eq!(issues.len(), 1, "{label}: {:?}", plan.blocking);
            assert_eq!(issues[0].ids, vec!["vid_2"], "{label}");
        }
        // Resolution message format.
        let a = seg("vid_1", "20260910_120000_F.mp4", 60.0);
        let mut b = seg("vid_2", "20260910_120100_F.mp4", 60.0);
        b.probe.video.as_mut().unwrap().width = Some(1280);
        b.probe.video.as_mut().unwrap().height = Some(720);
        let plan = plan_of(vec![a, b]);
        assert_eq!(
            plan.blocking[0].message,
            "해상도가 다릅니다: 1920×1080 (1개) / 1280×720 (1개)"
        );
        // Audio parameters.
        for mutate in [
            (|a: &mut AudioParams| a.codec = Some("mp3".into())) as fn(&mut AudioParams),
            |a| a.sample_rate = Some(44100),
            |a| a.channels = Some(2),
            // B5: profile and AudioSpecificConfig.
            |a| a.profile = Some("HE-AAC".into()),
            |a| a.extradata_hash = Some("SHA256:other".into()),
        ] {
            let a = seg("vid_1", "20260910_120000_F.mp4", 60.0);
            let mut b = seg("vid_2", "20260910_120100_F.mp4", 60.0);
            mutate(&mut b.probe.audio[0]);
            assert_eq!(
                codes(&plan_of(vec![a, b]).blocking),
                vec!["stream-mismatch"]
            );
        }
        // avg_frame_rate only warns.
        let a = seg("vid_1", "20260910_120000_F.mp4", 60.0);
        let mut b = seg("vid_2", "20260910_120100_F.mp4", 60.0);
        b.probe.video.as_mut().unwrap().avg_frame_rate = Some("2997/100".into());
        let plan = plan_of(vec![a, b]);
        assert!(plan.blocking.is_empty());
        assert!(codes(&plan.warnings).contains(&"avg-frame-rate-differs"));
    }

    #[test]
    fn audio_presence_and_unknown_duration_block() {
        let a = seg("vid_1", "20260910_120000_F.mp4", 60.0);
        let mut b = seg("vid_2", "20260910_120100_F.mp4", 60.0);
        b.probe.audio.clear();
        let plan = plan_of(vec![a.clone(), b]);
        let issue = plan
            .blocking
            .iter()
            .find(|i| i.code == "audio-presence")
            .unwrap();
        assert_eq!(issue.message, "오디오 트랙이 일부에만 있습니다 (1개 없음)");

        let mut c = seg("vid_3", "20260910_120100_F.mp4", 60.0);
        c.probe = ProbeFacts {
            error: Some("moov atom not found".into()),
            ..ProbeFacts::default()
        };
        let plan = plan_of(vec![a, c]);
        assert!(codes(&plan.blocking).contains(&"unknown-duration"));
    }

    /// R5-P3: the plan finds an indexed file through its lossless path
    /// (`source_path_raw_hex`); a non-Unicode name is then refused for the
    /// concat list with its own code, not reported as a missing file.
    #[test]
    fn plan_resolves_indexed_files_through_the_lossless_path() {
        let case = std::env::temp_dir().join(format!("ft-join-p3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&case);
        std::fs::create_dir_all(case.join("db")).unwrap();
        let odd = case.join(crate::model::test_non_unicode_name("20260910_120000_F"));
        if std::fs::write(&odd, b"x").is_err() {
            eprintln!("skipped: this filesystem refuses non-Unicode names");
            let _ = std::fs::remove_dir_all(&case);
            return;
        }
        let plain = case.join("20260910_120100_F.mp4");
        std::fs::write(&plain, b"y").unwrap();
        let rows = [
            serde_json::json!({
                "id": "vid_000001", "relative_path": "a.mp4",
                "source_path": odd.to_string_lossy(),
                "source_path_raw_hex": crate::model::path_raw_hex(&odd).unwrap(),
            }),
            serde_json::json!({
                "id": "vid_000002", "relative_path": "20260910_120100_F.mp4",
                "source_path": plain.to_string_lossy(),
            }),
        ];
        let text: String = rows.iter().map(|row| format!("{row}\n")).collect();
        std::fs::write(case.join("db/videos.jsonl"), text).unwrap();
        let tools = Tools {
            ffmpeg: "no-such-ffmpeg".into(),
            ffprobe: "no-such-ffprobe".into(),
        };
        let plan = plan_with_tools(
            &case,
            &["vid_000001".to_string(), "vid_000002".to_string()],
            1.0,
            &tools,
        )
        .unwrap();
        assert!(
            plan.blocking
                .iter()
                .all(|issue| issue.code != "not-indexed"),
            "{:?}",
            plan.blocking
        );
        let utf8 = plan
            .blocking
            .iter()
            .find(|issue| issue.code == "path-not-utf8")
            .expect("non-Unicode name is found, then refused for the concat list");
        assert_eq!(utf8.ids, vec!["vid_000001"]);
        let _ = std::fs::remove_dir_all(&case);
    }

    #[test]
    fn not_indexed_inputs_block() {
        let plan = build_plan(
            vec![
                SegmentInput::Found(Box::new(seg("vid_1", "20260910_120000_F.mp4", 60.0))),
                SegmentInput::NotIndexed {
                    id: "carve:1".into(),
                    message: "색인된 원본 영상(vid_*)만 이어붙일 수 있습니다".into(),
                },
            ],
            1.0,
        );
        let issue = plan
            .blocking
            .iter()
            .find(|i| i.code == "not-indexed")
            .unwrap();
        assert_eq!(issue.ids, vec!["carve:1"]);
    }

    #[test]
    fn container_choice() {
        assert_eq!(Container::choose(Some("h264"), Some("aac")), Container::Mp4);
        assert_eq!(Container::choose(Some("hevc"), None), Container::Mp4);
        assert_eq!(Container::choose(Some("h264"), Some("mp3")), Container::Mp4);
        assert_eq!(
            Container::choose(Some("h264"), Some("pcm_s16le")),
            Container::Mkv
        );
        assert_eq!(
            Container::choose(Some("mpeg4"), Some("aac")),
            Container::Mkv
        );
        assert_eq!(Container::choose(None, None), Container::Mkv);
    }

    #[test]
    fn concat_list_escaping_and_no_bom() {
        #[cfg(windows)]
        {
            let line = concat_file_line(Path::new(
                r"\\?\C:\증거 '따옴표' 폴더\FRONT\20260910_120000_F.mp4",
            ))
            .unwrap();
            assert_eq!(
                line,
                "file 'C:/증거 '\\''따옴표'\\'' 폴더/FRONT/20260910_120000_F.mp4'"
            );
        }
        // B7: off Windows a backslash is a file-name byte, never a separator.
        #[cfg(not(windows))]
        assert_eq!(
            concat_file_line(Path::new("/e/a\\b '1'.mp4")).unwrap(),
            "file '/e/a\\b '\\''1'\\''.mp4'"
        );
        assert!(concat_file_line(Path::new("C:/a\nb.mp4")).is_err());
        assert!(concat_file_line(Path::new("C:/a\rb.mp4")).is_err());
        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000_F.mp4", 60.0),
            seg("vid_2", "20260910_120100_F.mp4", 60.0),
        ]);
        let list = concat_list(&plan).unwrap();
        assert!(list.starts_with("ffconcat version 1.0\nfile '"));
        assert!(!list.as_bytes().starts_with(&[0xEF, 0xBB, 0xBF]));
        assert_eq!(list.lines().count(), 3);
    }

    #[test]
    fn ffmpeg_argv_is_ascii_only() {
        for container in [Container::Mp4, Container::Mkv] {
            let args = ffmpeg_argv("job-7_x", container);
            assert!(args.iter().all(|a| a.is_ascii()), "{args:?}");
            assert!(args.contains(&".join-job-7_x.ffconcat".to_string()));
            assert_eq!(args.last().unwrap(), ".join-job-7_x.part");
            assert!(args.windows(2).any(|w| w == ["-c", "copy"]));
            // R10-S8: leading non-key frames are kept.
            assert!(args.contains(&"-copyinkf".to_string()), "{args:?}");
            assert_eq!(
                args.contains(&"+faststart".to_string()),
                container == Container::Mp4
            );
        }
        assert!(validate_job_tag("ok-1_2").is_ok());
        assert!(validate_job_tag("한글").is_err());
        assert!(validate_job_tag("a b").is_err());
        assert!(validate_job_tag("").is_err());
    }

    /// D17: a source rewritten within the same second (same size, same
    /// whole-second mtime) is caught by the nanosecond comparison.
    #[test]
    fn stat_matches_compares_nanosecond_mtime() {
        let dir = std::env::temp_dir().join(format!("ft-join-stat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.mp4");
        std::fs::write(&path, b"0123456789").unwrap();
        let base = std::time::UNIX_EPOCH + std::time::Duration::new(1_800_000_000, 100_000_000);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(base).unwrap();
        drop(file);
        let meta = std::fs::metadata(&path).unwrap();
        let mut segment = seg("vid_1", "a.mp4", 1.0);
        segment.source_path = path.clone();
        segment.size_bytes = meta.len();
        segment.modified_unix = modified_unix(&meta);
        segment.modified_nanos = crate::audit::modified_nanos(&meta);
        stat_matches(&segment).unwrap();
        // Same second, other sub-second time.
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(base + std::time::Duration::from_millis(500))
            .unwrap();
        drop(file);
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(modified_unix(&meta), segment.modified_unix, "same second");
        let err = stat_matches(&segment).expect_err("sub-second change is a change");
        assert!(err.contains("원본이 바뀌었습니다"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_id_is_stable_and_binds_size_and_mtime() {
        let make = || {
            vec![
                seg("vid_1", "20260910_120000_F.mp4", 60.0),
                seg("vid_2", "20260910_120100_F.mp4", 60.0),
            ]
        };
        let base = plan_of(make()).plan_id;
        assert_eq!(base.len(), 64);
        assert_eq!(plan_of(make()).plan_id, base);
        let mut reversed = make();
        reversed.reverse();
        assert_eq!(plan_of(reversed).plan_id, base, "input order is irrelevant");
        let mut sized = make();
        sized[0].size_bytes += 1;
        assert_ne!(plan_of(sized).plan_id, base);
        let mut touched = make();
        touched[1].modified_unix = Some(2);
        assert_ne!(plan_of(touched).plan_id, base);
        let other_tolerance = build_plan(
            make()
                .into_iter()
                .map(|s| SegmentInput::Found(Box::new(s)))
                .collect(),
            2.0,
        );
        assert_ne!(other_tolerance.plan_id, base);
    }

    #[test]
    fn summary_text_formats() {
        // The plan's own example: two overlaps of 1.02 s each.
        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000_F.mp4", 61.02),
            seg("vid_2", "20260910_120100_F.mp4", 61.02),
            seg("vid_3", "20260910_120200_F.mp4", 60.0),
        ]);
        assert_eq!(
            plan.summary_text(),
            "전방 · 3개 · 3분 02초 · 끊김 0곳 / 겹침 2곳(각 1초)"
        );
        assert_eq!(plan.junctions[0].delta_seconds.map(r3), Some(-1.02));
        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000_F.mp4", 61.4),
            seg("vid_2", "20260910_120100_F.mp4", 61.4),
            seg("vid_3", "20260910_120200_F.mp4", 60.0),
        ]);
        assert_eq!(
            plan.summary_text(),
            "전방 · 3개 · 3분 03초 · 끊김 0곳 / 겹침 2곳(각 1초)"
        );
        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000_F.mp4", 61.5),
            seg("vid_2", "20260910_120100_F.mp4", 63.0),
            seg("vid_3", "20260910_120200_F.mp4", 60.0),
        ]);
        assert_eq!(
            plan.summary_text(),
            "전방 · 3개 · 3분 05초 · 끊김 0곳 / 겹침 2곳(합계 4.5초, 최대 3.0초)"
        );
        assert_eq!(plan.totals.overlap_max_seconds, 3.0);
        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000_F.mp4", 3600.0),
            seg("vid_2", "20260910_130003_F.mp4", 62.0),
        ]);
        assert_eq!(
            plan.summary_text(),
            "전방 · 2개 · 1시간 01분 02초 · 끊김 1곳(각 3초) / 겹침 0곳"
        );
        assert_eq!(format_duration_ko(602.1), "10분 02초");
    }

    #[test]
    fn plan_json_matches_api_shape() {
        let plan = plan_of(vec![
            seg("vid_1", "20260910_120000_F.mp4", 60.0),
            seg("vid_2", "20260910_120100_F.mp4", 60.0),
        ]);
        let value = plan.to_json();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "plan_id",
                "order_basis",
                "tolerance_seconds",
                "channel",
                "container",
                "segments",
                "junctions",
                "totals",
                "blocking",
                "warnings",
                "summary_text"
            ]
        );
        assert_eq!(value["channel"]["label"], "전방");
        let segment = &value["segments"][0];
        assert_eq!(segment["recorded_start_text"], "2026-09-10 12:00:00");
        assert_eq!(segment["time_source"], "filename");
        assert_eq!(segment["video"]["extradata_hash"], "SHA256:aa");
        assert_eq!(segment["audio"]["sample_rate"], 48000);
        assert_eq!(value["junctions"][0]["kind"], "continuous");
        assert_eq!(value["totals"]["overlap_max_seconds"], 0.0);
    }

    #[test]
    fn probe_facts_parse_data_hash_and_streams() {
        let raw = r#"{"streams":[
            {"codec_type":"video","codec_name":"h264","profile":"High","width":640,"height":360,
             "pix_fmt":"yuv420p","r_frame_rate":"30/1","avg_frame_rate":"30/1","time_base":"1/15360",
             "extradata_hash":"SHA256:abc"},
            {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":1},
            {"codec_type":"data","codec_name":"bin_data"}],
            "format":{"duration":"2.000000","tags":{"creation_time":"2026-09-10T03:00:00.000000Z"}}}"#;
        let facts = parse_probe_facts(raw);
        assert!(facts.ok);
        assert_eq!(facts.duration_seconds, Some(2.0));
        assert_eq!(
            facts.video.unwrap().extradata_hash.as_deref(),
            Some("SHA256:abc")
        );
        assert_eq!(facts.audio[0].sample_rate, Some(48000));
        assert_eq!(facts.other_streams, 1);
        assert_eq!(
            facts.creation_time.as_deref(),
            Some("2026-09-10T03:00:00.000000Z")
        );
        assert!(!parse_probe_facts("nope").ok);
    }

    /// B13: crash cleanup removes only certainly-dead tags: an unlocked
    /// lock file, or (no lock file) everything older than an hour. Active
    /// tags, tags whose lock another handle holds (a CLI join running in
    /// another process), and fresh lock-less files stay.
    #[test]
    fn temp_cleanup_spares_live_locked_and_fresh_tags() {
        use fs2::FileExt;
        let dir = std::env::temp_dir().join(format!(
            "ft-join-clean-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        let joined = dir.join(JOINED_DIR);
        std::fs::create_dir_all(&joined).unwrap();
        for name in [
            ".join-live.part",
            ".join-dead.part",
            ".join-dead.ffconcat",
            ".join-dead.lock",
            ".join-running.part",
            ".join-running.lock",
            ".join-fresh.ffconcat",
            ".join-old.part",
            "join_F_x_n2.mp4",
        ] {
            std::fs::write(joined.join(name), b"x").unwrap();
        }
        let old = std::time::SystemTime::now() - Duration::from_secs(2 * STALE_TEMP_AGE_SECS);
        std::fs::File::options()
            .write(true)
            .open(joined.join(".join-old.part"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        // Another "process" holds the running tag's lock.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(joined.join(".join-running.lock"))
            .unwrap();
        held.lock_exclusive().unwrap();
        let removed = cleanup_stale_temp_files(&dir, &["live".to_string()]);
        let mut names: Vec<String> = removed
            .iter()
            .map(|(p, r)| {
                assert!(r.is_ok(), "{r:?}");
                p.file_name().unwrap().to_string_lossy().to_string()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                ".join-dead.ffconcat",
                ".join-dead.lock",
                ".join-dead.part",
                ".join-old.part"
            ]
        );
        for kept in [
            ".join-live.part",
            ".join-running.part",
            ".join-running.lock",
            ".join-fresh.ffconcat",
            "join_F_x_n2.mp4",
        ] {
            assert!(joined.join(kept).exists(), "{kept} was removed");
        }
        let _ = FileExt::unlock(&held);
        drop(held);
        assert!(remove_file_retry(&joined.join("missing")).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B14: publishing never clobbers — a taken name (even on a filesystem
    /// without hard links, where the fallback is a create_new copy) moves
    /// on to the next `__N` name; a free manifest name is required too.
    #[test]
    fn publish_never_clobbers_and_takes_next_name() {
        let dir = std::env::temp_dir().join(format!(
            "ft-join-publish-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("join_F_x_n2.mp4"), b"theirs").unwrap();
        std::fs::write(dir.join("join_F_x_n2__2.manifest.json"), b"{}").unwrap();
        let part = dir.join(".join-t.part");
        std::fs::write(&part, b"ours").unwrap();
        assert!(!publish_part_as(&part, &dir.join("join_F_x_n2.mp4")).unwrap());
        let mut claimed = Vec::new();
        let (output, manifest) = publish_part(
            &part,
            &dir,
            "join_F_x_n2",
            "mp4",
            &mut |output, manifest| {
                // C3: the claim is recorded while the output name is still
                // free and the manifest name already reserved.
                assert!(!output.exists(), "claim must precede the publish");
                assert!(manifest.exists(), "manifest name reserved first");
                claimed.push(output.to_path_buf());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(claimed, [dir.join("join_F_x_n2__3.mp4")]);
        assert_eq!(output, dir.join("join_F_x_n2__3.mp4"));
        assert_eq!(manifest, dir.join("join_F_x_n2__3.manifest.json"));
        // The reservation of the taken stem was released.
        assert!(!dir.join("join_F_x_n2.manifest.json").exists());
        assert_eq!(std::fs::read(&output).unwrap(), b"ours");
        assert_eq!(
            std::fs::read(dir.join("join_F_x_n2.mp4")).unwrap(),
            b"theirs"
        );
        assert!(!part.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// B8 / U2: claims round-trip; record ids derive from the output bytes.
    #[test]
    fn claim_and_record_id_helpers() {
        let dir = std::env::temp_dir().join(format!(
            "ft-join-claim-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let claim = dir.join(".join-t.claim.json");
        assert!(read_claim(&claim).is_none());
        std::fs::write(
            &claim,
            r#"{"output":"/a/o.mp4","manifest":"/a/o.manifest.json"}"#,
        )
        .unwrap();
        assert_eq!(
            read_claim(&claim),
            Some((
                PathBuf::from("/a/o.mp4"),
                PathBuf::from("/a/o.manifest.json")
            ))
        );
        let _ = std::fs::remove_dir_all(&dir);
        let sha = "ABCDEF0123456789".repeat(4);
        assert_eq!(
            joined_record_id(&sha).as_deref(),
            Some("join:abcdef0123456789")
        );
        assert_eq!(joined_record_id("xyz"), None);
        let text = format!(
            "{}\n{{\"record_id\":\"join:keep\",\"output_sha256\":\"{sha}\"}}\n{{torn\n",
            json!({"event": "join-video", "output_sha256": sha})
        );
        let out = join_log_with_record_ids(&text);
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].contains("\"record_id\":\"join:abcdef0123456789\""),
            "{out}"
        );
        assert!(lines[1].contains("\"record_id\":\"join:keep\""), "{out}");
        assert_eq!(lines[2], "{torn");
    }

    /// B2/B4/B6/B7 matrix additions.
    #[test]
    fn unknown_junctions_stream_warnings_and_extradata_fallback() {
        // B2: a filename-order plan counts its junctions as unknown.
        let plan = plan_of(vec![
            seg("vid_1", "a1.mp4", 10.0),
            seg("vid_2", "a2.mp4", 10.0),
            seg("vid_3", "a3.mp4", 10.0),
        ]);
        assert_eq!(plan.order_basis, OrderBasis::FilenameOrder);
        assert_eq!(plan.totals.unknown_junctions, 2);
        assert_eq!(plan.to_json()["totals"]["unknown_junctions"], 2);
        assert!(plan.summary_text().ends_with("끊김·겹침 확인 불가"));

        // B4: extra video / data streams are warned about.
        let a = seg("vid_1", "20260910_120000_F.mp4", 60.0);
        let mut b = seg("vid_2", "20260910_120100_F.mp4", 60.0);
        b.probe.video_streams = 3;
        b.probe.other_streams = 1;
        let plan = plan_of(vec![a, b]);
        let extra = plan
            .warnings
            .iter()
            .find(|w| w.code == "extra-video-streams")
            .unwrap();
        assert!(
            extra
                .message
                .contains("추가 영상 스트림 2개는 포함되지 않습니다")
        );
        assert_eq!(extra.ids, vec!["vid_2"]);
        assert!(codes(&plan.warnings).contains(&"data-streams-dropped"));
        assert!(plan.blocking.is_empty(), "{:?}", plan.blocking);

        // B6: no SPS/PPS hash anywhere → fallback fields block, and the
        // warning says what was compared instead.
        let strip = |mut s: Segment| {
            s.probe.video.as_mut().unwrap().extradata_hash = None;
            s
        };
        let a = strip(seg("vid_1", "20260910_120000_F.mp4", 60.0));
        let b = strip(seg("vid_2", "20260910_120100_F.mp4", 60.0));
        let plan = plan_of(vec![a.clone(), b.clone()]);
        assert!(plan.blocking.is_empty(), "{:?}", plan.blocking);
        assert!(codes(&plan.warnings).contains(&"extradata-unavailable"));
        for (position, (key, subject)) in VIDEO_FALLBACK_FIELDS.iter().enumerate() {
            let mut changed = b.clone();
            changed.probe.video.as_mut().unwrap().fallback[position].1 = Some("other".into());
            let plan = plan_of(vec![a.clone(), changed]);
            let issue = plan
                .blocking
                .iter()
                .find(|i| i.code == "stream-mismatch")
                .unwrap_or_else(|| panic!("{key}: not blocked"));
            assert!(
                issue.message.starts_with(subject),
                "{key}: {}",
                issue.message
            );
        }
        // With hashes present the fallback fields are not compared (the
        // hash already covers them) and there is no extradata warning.
        let a = seg("vid_1", "20260910_120000_F.mp4", 60.0);
        let mut b = seg("vid_2", "20260910_120100_F.mp4", 60.0);
        b.probe.video.as_mut().unwrap().fallback[0].1 = Some("other".into());
        let plan = plan_of(vec![a, b]);
        assert!(plan.blocking.is_empty(), "{:?}", plan.blocking);
        assert!(!codes(&plan.warnings).contains(&"extradata-unavailable"));
    }

    /// B7: a path that is not valid Unicode blocks with its own code and is
    /// refused by the concat writer.
    #[test]
    fn non_utf8_paths_block() {
        #[cfg(unix)]
        let bad = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(b"/e/\xff\xfe.mp4".to_vec()))
        };
        #[cfg(windows)]
        let bad = {
            use std::os::windows::ffi::OsStringExt;
            let mut wide: Vec<u16> = "C:\\e\\".encode_utf16().collect();
            wide.push(0xD800);
            wide.extend(".mp4".encode_utf16());
            PathBuf::from(std::ffi::OsString::from_wide(&wide))
        };
        assert!(concat_file_line(&bad).is_err());
        let mut b = seg("vid_2", "20260910_120100_F.mp4", 60.0);
        b.source_path = bad;
        let plan = plan_of(vec![seg("vid_1", "20260910_120000_F.mp4", 60.0), b]);
        let issue = plan
            .blocking
            .iter()
            .find(|i| i.code == "path-not-utf8")
            .unwrap();
        assert_eq!(issue.ids, vec!["vid_2"]);
    }

    #[test]
    fn request_validation() {
        let ids = |n: usize| (0..n).map(|i| format!("vid_{i:06}")).collect::<Vec<_>>();
        assert!(validate_request(&ids(2), 1.0).is_ok());
        assert!(validate_request(&ids(1), 1.0).is_err());
        assert!(validate_request(&ids(501), 1.0).is_err());
        assert!(validate_request(&ids(2), 0.4).is_err());
        assert!(validate_request(&ids(2), 5.1).is_err());
        assert!(validate_request(&ids(2), f64::NAN).is_err());
        assert!(validate_request(&["vid_1".into(), "a\nb".into()], 1.0).is_err());
        assert!(validate_request(&["vid_1".into(), "x".repeat(513)], 1.0).is_err());
    }

    /// C14: the tolerance is max(1 s, 0.2 %) — the old max(1 s, 1 %) let a
    /// 30 s segment vanish from a one-hour join.
    #[test]
    fn duration_tolerance_is_tight() {
        assert_eq!(duration_tolerance(10.0), 1.0);
        assert!((duration_tolerance(3600.0) - 7.2).abs() < 1e-9);
        // A missing 30 s segment of a 1 h join exceeds it (old: 36 s).
        assert!(30.0 > duration_tolerance(3600.0));
    }

    fn dts_run(start: f64, count: u64) -> Vec<VideoPacket> {
        (0..count)
            .map(|i| {
                let t = start + i as f64 / 30.0;
                VideoPacket {
                    pts: Some(t),
                    dts: Some(t),
                    discard: false,
                    ..VideoPacket::default()
                }
            })
            .collect()
    }

    /// C14: a complete output passes; a dropped segment (packet deficit and
    /// a misplaced boundary) or a truncated tail fails; audio may differ
    /// by one packet per segment.
    #[test]
    fn segment_check_table() {
        let sources = [
            SourcePackets::plain(63, Some(100)),
            SourcePackets::plain(66, Some(105)),
            SourcePackets::plain(69, Some(109)),
        ];
        let offsets = [0.0, 2.1, 4.3];
        let mut full = dts_run(-0.0007, 63);
        full.extend(dts_run(2.099, 66));
        full.extend(dts_run(4.266, 69));
        let check = evaluate_segment_check(&sources, &offsets, Some(314), &full);
        assert!(check.ok, "{:?}", check.problems);
        assert_eq!(check.video_packets_expected, 198);
        assert_eq!(check.boundaries.len(), 3);
        // Audio within slack (one per segment).
        assert!(evaluate_segment_check(&sources, &offsets, Some(312), &full).ok);
        assert!(!evaluate_segment_check(&sources, &offsets, Some(300), &full).ok);
        assert!(!evaluate_segment_check(&sources, &offsets, None, &full).ok);

        // The middle segment is missing from the output.
        let mut missing = dts_run(0.0, 63);
        missing.extend(dts_run(2.1, 69));
        let check = evaluate_segment_check(&sources, &offsets, Some(209), &missing);
        assert!(!check.ok);
        assert!(
            check.problems.iter().any(|p| p.contains("패킷 수")),
            "{:?}",
            check.problems
        );

        // Same packet count but the third segment starts 3 s late (a gap
        // ffmpeg inserted where the plan has none).
        let mut shifted = dts_run(0.0, 63);
        shifted.extend(dts_run(2.1, 66));
        shifted.extend(dts_run(7.3, 69));
        let check = evaluate_segment_check(&sources, &offsets, Some(314), &shifted);
        assert!(!check.ok);
        assert!(check.problems.iter().any(|p| p.contains("3번째")));

        // Truncated tail: last segment only partly present.
        let check = evaluate_segment_check(&sources, &offsets, Some(314), &full[..150]);
        assert!(!check.ok);
    }

    /// R5-S13: Matroska B-frame output — the first packets have no dts
    /// (`N/A`) and presentation order differs from decode order. The
    /// boundary is the earliest pts of a segment's packets. Before, an
    /// N/A dts repeated the previous value (0 at the very start), and a
    /// segment whose first packets lacked a dts was placed wrongly.
    #[test]
    fn segment_check_uses_presentation_times() {
        // Decode order I P B B … with pts reordered; dts N/A on the first
        // two packets of every segment.
        let segment = |start: f64, count: u64| -> Vec<VideoPacket> {
            (0..count)
                .map(|i| {
                    let pts_index = match i % 3 {
                        0 => i,
                        1 => i + 1,
                        _ => i - 1,
                    };
                    VideoPacket {
                        pts: Some(start + pts_index as f64 / 30.0),
                        dts: (i >= 2).then(|| start + (i as f64 - 2.0) / 30.0),
                        discard: false,
                        ..VideoPacket::default()
                    }
                })
                .collect()
        };
        let sources = [
            SourcePackets::plain(60, None),
            SourcePackets::plain(60, None),
        ];
        let mut output = segment(0.0, 60);
        output.extend(segment(2.0, 60));
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &output);
        assert!(check.ok, "{:?}", check.problems);
        assert!((check.boundaries[1].2.unwrap() - 2.0).abs() < 1e-9);
        // No timing at all for a segment: reported, never guessed.
        let mut blind = segment(0.0, 60);
        blind.extend((0..60).map(|_| VideoPacket::default()));
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &blind);
        assert!(!check.ok);
        assert!(
            check.problems.iter().any(|p| p.contains("2번째")),
            "{:?}",
            check.problems
        );
        let parsed = parse_video_packets(
            "pts_time=0.066000|dts_time=N/A|flags=K__\npts_time=-0.5|dts_time=-0.566|flags=KD_\n",
        );
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].dts, None);
        assert!(parsed[1].discard);
    }

    /// R5-S13: an MP4 source with an edit list (a stream-copy cut at a
    /// non-key frame: the lead-in packets carry the discard flag). The
    /// concat output may keep those packets as visible frames — the later
    /// segments then start later by the pre-roll it presents — or drop
    /// them. Both are accepted; a missing segment still fails.
    #[test]
    fn segment_check_accounts_for_edit_list_preroll() {
        // Source 1: 45 pre-roll packets (1.5 s) + 75 kept; source 2: 60.
        let mut first: Vec<VideoPacket> = (0..120)
            .map(|i| VideoPacket {
                pts: Some(-1.5 + i as f64 / 30.0),
                dts: Some(-1.5 + i as f64 / 30.0),
                discard: i < 45,
                // R6-S6: payload hashes (one key frame per second).
                key: i % 30 == 0,
                hash: Some(1000 + i),
            })
            .collect();
        let sources = [
            SourcePackets::from_packets(&first, None),
            SourcePackets::plain(60, None),
        ];
        assert_eq!(sources[0].video_kept, 75);
        // Output keeps every packet, pre-roll presented (no D flags).
        for packet in &mut first {
            packet.discard = false;
            packet.pts = packet.pts.map(|t| t + 1.5);
            packet.dts = packet.dts.map(|t| t + 1.5);
        }
        let mut output = first.clone();
        output.extend(dts_run(4.0, 60));
        let check = evaluate_segment_check(&sources, &[0.0, 2.5], None, &output);
        assert!(check.ok, "{:?}", check.problems);
        assert_eq!(check.mode, "all-packets");
        assert!((check.presented_preroll_total() - 1.5).abs() < 1e-6);
        assert!(!check.notes.is_empty());
        // Output that applied the edit list: only the kept packets.
        let mut applied: Vec<VideoPacket> = output[45..120].to_vec();
        for packet in &mut applied {
            packet.pts = packet.pts.map(|t| t - 1.5);
        }
        applied.extend(dts_run(2.5, 60));
        let check = evaluate_segment_check(&sources, &[0.0, 2.5], None, &applied);
        assert!(check.ok, "{:?}", check.problems);
        assert_eq!(check.mode, "edit-list-applied");
        assert_eq!(check.presented_preroll_total(), 0.0);
        // R6-S6: the same packet count as the kept total, but the output
        // KEPT the pre-roll and lost the last 45 real frames — rebased to
        // zero its timing is identical to an applied edit list. Before,
        // it passed as `edit-list-applied`; the payloads now say no.
        let mut lost: Vec<VideoPacket> = output[..75].to_vec();
        lost.extend(dts_run(2.5, 60));
        let check = evaluate_segment_check(&sources, &[0.0, 2.5], None, &lost);
        assert!(!check.ok, "missing frames passed: {check:?}");
        assert_ne!(check.mode, "edit-list-applied");
        assert!(
            check.problems.iter().any(|p| p.contains("유지 프레임")),
            "{:?}",
            check.problems
        );
        // No payload hashes (an older ffprobe): never assumed applied.
        let mut unhashed = applied.clone();
        for packet in &mut unhashed {
            packet.hash = None;
        }
        let check = evaluate_segment_check(&sources, &[0.0, 2.5], None, &unhashed);
        assert!(!check.ok);
        let parsed = parse_video_packets(
            "pts_time=0.1|dts_time=0.1|flags=___|data_hash=SHA256:00000000000000ffabcdef\n",
        );
        assert_eq!(parsed[0].hash, Some(0xff));
        assert!(!parsed[0].key);
        // A missing second segment still fails.
        let check = evaluate_segment_check(&sources, &[0.0, 2.5], None, &output[..120]);
        assert!(!check.ok);
    }

    /// R7-S4: an all-intra source (every packet a key frame) with an edit
    /// list has an EMPTY non-key fingerprint; R6 took that for "no hashes"
    /// and failed every correct join of it. The key frames now decide.
    #[test]
    fn all_intra_edit_list_source_joins() {
        // 30 pre-roll + 60 kept, all key frames.
        let source: Vec<VideoPacket> = (0..90)
            .map(|i| VideoPacket {
                pts: Some(-1.0 + i as f64 / 30.0),
                dts: Some(-1.0 + i as f64 / 30.0),
                discard: i < 30,
                key: true,
                hash: Some(5000 + i),
            })
            .collect();
        let sources = [
            SourcePackets::from_packets(&source, None),
            SourcePackets::plain(60, None),
        ];
        assert_eq!(sources[0].kept_fingerprint, Some(Vec::new()));
        let rebase = |packets: &[VideoPacket], by: f64| -> Vec<VideoPacket> {
            packets
                .iter()
                .map(|p| VideoPacket {
                    pts: p.pts.map(|t| t + by),
                    dts: p.dts.map(|t| t + by),
                    discard: false,
                    ..*p
                })
                .collect()
        };
        // The edit list applied: the kept packets, byte for byte.
        let mut applied = rebase(&source[30..], 0.0);
        applied.extend(dts_run(2.0, 60));
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &applied);
        assert!(check.ok, "{:?}", check.problems);
        assert_eq!(check.mode, "edit-list-applied");
        // R9-S2: hashes are of normalized payloads, so a muxer rewrite
        // (parameter sets, framing) no longer changes them: an output
        // whose frames are all different is different, and refused.
        let mut rewritten = applied.clone();
        for (i, packet) in rewritten.iter_mut().take(60).enumerate() {
            packet.hash = Some(9_000_000 + i as u64);
        }
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &rewritten);
        assert!(!check.ok, "{:?}", check.problems);
        // Pre-roll kept, the last 30 real frames lost: same count, refused.
        let mut lost = rebase(&source[..60], 1.0);
        lost.extend(dts_run(2.0, 60));
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &lost);
        assert!(!check.ok, "missing frames passed: {check:?}");
        // Partly different frames: refused.
        let mut partial = applied.clone();
        partial[10].hash = Some(1);
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &partial);
        assert!(!check.ok);
    }

    /// R8-S1: an all-intra output shifted PART-way into the pre-roll (10 of
    /// 30 pre-roll frames kept, 10 real frames lost at the end) agrees
    /// with neither position-wise alignment R7 compared — and R7 took
    /// that for "every key frame rewritten" and passed it. A one-frame
    /// kept segment carrying a pre-roll frame passed too (R7 skipped the
    /// first packet and compared nothing).
    #[test]
    fn all_intra_partial_shift_and_single_kept_frame_are_refused() {
        let all_key = |count: u64, discarded: u64, base: u64| -> Vec<VideoPacket> {
            (0..count)
                .map(|i| VideoPacket {
                    pts: Some(-1.0 + i as f64 / 30.0),
                    dts: Some(-1.0 + i as f64 / 30.0),
                    discard: i < discarded,
                    key: true,
                    hash: Some(base + i),
                })
                .collect()
        };
        let presented = |packets: &[VideoPacket]| -> Vec<VideoPacket> {
            packets
                .iter()
                .map(|p| VideoPacket {
                    discard: false,
                    ..*p
                })
                .collect()
        };
        let source = all_key(90, 30, 5000);
        let sources = [
            SourcePackets::from_packets(&source, None),
            SourcePackets::plain(60, None),
        ];
        let mut shifted = presented(&source[10..70]);
        shifted.extend(dts_run(2.0, 60));
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &shifted);
        assert!(
            check.problems.iter().any(|p| p.contains("유지 프레임")),
            "partial shift passed: {check:?}"
        );
        // The exact kept packets still pass. R9-S2: a different first
        // packet no longer does (normalized hashes; see
        // `all_intra_first_packet_and_foreign_frames_are_refused`).
        let mut applied = presented(&source[30..]);
        applied.extend(dts_run(2.0, 60));
        assert!(evaluate_segment_check(&sources, &[0.0, 2.0], None, &applied).ok);
        let mut first_rewritten = applied.clone();
        first_rewritten[0].hash = Some(42);
        assert!(!evaluate_segment_check(&sources, &[0.0, 2.0], None, &first_rewritten).ok);
        // A source packet in the wrong place is never "rewritten".
        let mut first_moved = applied.clone();
        first_moved[0].hash = Some(5000);
        assert!(!evaluate_segment_check(&sources, &[0.0, 2.0], None, &first_moved).ok);
        // One kept frame: a pre-roll frame in its place is refused.
        let single = all_key(3, 2, 8000);
        let sources = [
            SourcePackets::from_packets(&single, None),
            SourcePackets::plain(60, None),
        ];
        let one_kept = |frame: &VideoPacket| -> Vec<VideoPacket> {
            let mut out = presented(std::slice::from_ref(frame));
            out.extend(dts_run(frame.pts.unwrap() + 2.0, 60));
            out
        };
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &one_kept(&single[0]));
        assert!(
            check.problems.iter().any(|p| p.contains("유지 프레임")),
            "pre-roll frame as the kept frame passed: {check:?}"
        );
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &one_kept(&single[2]));
        assert!(check.ok, "{check:?}");
    }

    /// R9-S2: R8 exempted an all-intra segment's first output packet when
    /// it was no source packet (meant for re-inserted parameter sets) and
    /// accepted a segment none of whose packets was a source packet
    /// ("every key frame rewritten"). One wrong frame in the first place —
    /// a pre-roll frame kept as the only "kept" frame, rewritten by the
    /// muxer — therefore passed, and so did entirely foreign frames. With
    /// normalized payload hashes every position is compared exactly.
    #[test]
    fn all_intra_first_packet_and_foreign_frames_are_refused() {
        let all_key = |count: u64, discarded: u64, base: u64| -> Vec<VideoPacket> {
            (0..count)
                .map(|i| VideoPacket {
                    pts: Some(-1.0 + i as f64 / 30.0),
                    dts: Some(-1.0 + i as f64 / 30.0),
                    discard: i < discarded,
                    key: true,
                    hash: Some(base + i),
                })
                .collect()
        };
        let presented = |packets: &[VideoPacket]| -> Vec<VideoPacket> {
            packets
                .iter()
                .map(|p| VideoPacket {
                    discard: false,
                    ..*p
                })
                .collect()
        };
        let source = all_key(90, 30, 5000);
        let sources = [
            SourcePackets::from_packets(&source, None),
            SourcePackets::plain(60, None),
        ];
        let mut applied = presented(&source[30..]);
        applied.extend(dts_run(2.0, 60));
        assert!(evaluate_segment_check(&sources, &[0.0, 2.0], None, &applied).ok);
        // The first kept frame replaced by a frame that is no source frame.
        let mut first_foreign = applied.clone();
        first_foreign[0].hash = Some(77);
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &first_foreign);
        assert!(
            check
                .problems
                .iter()
                .any(|p| p.contains("1번째 프레임부터")),
            "a wrong first frame passed: {check:?}"
        );
        // Every frame foreign: no longer "rewritten by the muxer".
        let mut all_foreign = applied.clone();
        for (i, packet) in all_foreign.iter_mut().take(60).enumerate() {
            packet.hash = Some(1_000 + i as u64);
        }
        assert!(!evaluate_segment_check(&sources, &[0.0, 2.0], None, &all_foreign).ok);
        // One kept frame, replaced by a foreign one (R8: exempt as "first").
        let single = all_key(3, 2, 8000);
        let sources = [
            SourcePackets::from_packets(&single, None),
            SourcePackets::plain(60, None),
        ];
        let mut one = presented(&single[2..]);
        one[0].hash = Some(4242);
        one.extend(dts_run(single[2].pts.unwrap() + 2.0, 60));
        let check = evaluate_segment_check(&sources, &[0.0, 2.0], None, &one);
        assert!(!check.ok, "a foreign single kept frame passed: {check:?}");
    }

    /// R9-S2: the normalizing hash pipeline's pieces.
    #[test]
    fn normalized_hash_pipeline_parts() {
        assert_eq!(
            normalizing_bsf(Some("h264")),
            Some("h264_mp4toannexb,filter_units=remove_types=7|8|9")
        );
        assert!(
            normalizing_bsf(Some("hevc"))
                .unwrap()
                .contains("32|33|34|35")
        );
        assert_eq!(normalizing_bsf(Some("mjpeg")), None);
        let args: Vec<String> =
            normalized_hash_args(Path::new("in.mp4"), normalizing_bsf(Some("h264")))
                .iter()
                .map(|a| a.to_string_lossy().to_string())
                .collect();
        assert!(args.windows(2).any(|w| w == ["-c", "copy"]), "{args:?}");
        assert!(args.windows(2).any(|w| w[0] == "-bsf:v"), "{args:?}");
        assert!(args.ends_with(&[
            "-f".into(),
            "framehash".into(),
            "-hash".into(),
            "sha256".into(),
            "-".into()
        ]));
        // Side-data columns (MPEG-TS) come after the packet hash.
        let text = "#format: frame checksums\n#hash: SHA256\n#stream#, dts, pts, duration, size, hash\n0, 0, 0, 9000, 1671, db74c93919c9d461efce4de1047319601cc2d060889fd1edce1ba8813836f88f, S=1, 1, 7d8c5da7\n0, 1024, 1024, 1024, 1647, 0123456789abcdef00\n";
        assert_eq!(
            parse_framehash(text),
            Some(vec![0xdb74c93919c9d461, 0x0123456789abcdef])
        );
        assert_eq!(parse_framehash("0, 0, 0, 1, 2\n"), None);
        // R10-S8: leading non-key packets are hashed, timestamps kept.
        assert!(args.contains(&"-copyinkf".to_string()), "{args:?}");
        assert!(args.contains(&"-copyts".to_string()), "{args:?}");
        for codec in ["h264", "hevc"] {
            let (annexb, types) = normalizing_parts(Some(codec)).unwrap();
            assert_eq!(
                normalizing_bsf(Some(codec)).unwrap(),
                format!("{annexb},filter_units=remove_types={types}")
            );
        }
        assert_eq!(normalizing_parts(Some("mjpeg")), None);
        let lines = parse_framehash_lines(
            "0, -1024, -1024, 1024, 260, 7cf784de4454d50c26d1\n0, -9223372036854775808, 0, 1, 2, 0123456789abcdef\n",
        )
        .unwrap();
        assert_eq!(lines[0].dts, Some(-1024));
        assert_eq!(lines[1].dts, None);
        assert_eq!(lines[1].pts, Some(0));
        let stamp = |pts, dts| PacketStamp { pts, dts };
        // R11-S1: by key name, in ffprobe's own order (pts first), with
        // Matroska's `N/A` dts and MPEG-TS's trailing comma.
        assert_eq!(
            parse_packet_stamps("pts=0,dts=-1024\npts=100,dts=N/A\npts=144000,dts=126000,\n")
                .unwrap(),
            vec![
                stamp(Some(0), Some(-1024)),
                stamp(Some(100), None),
                stamp(Some(144000), Some(126000))
            ]
        );
        assert_eq!(
            parse_packet_stamps("dts=-1024,pts=0\n").unwrap(),
            vec![stamp(Some(0), Some(-1024))]
        );
        // Never by position: a keyless line, a missing key or a value
        // that is no timestamp is an error.
        assert!(parse_packet_stamps("0,-1024\n").is_err());
        assert!(parse_packet_stamps("pts=0\n").is_err());
        assert!(parse_packet_stamps("pts=0,dts=x\n").is_err());
        assert!(packet_stamp_args().contains(&"csv=p=0:nk=0"));
        // SHA-256 of the empty string.
        let empty: [u8; 32] = {
            use sha2::Digest;
            sha2::Sha256::digest(b"").into()
        };
        assert_eq!(
            EMPTY_PAYLOAD_HASH,
            u64::from_be_bytes(empty[..8].try_into().unwrap())
        );
    }

    /// R10-S8: a packet the normalizing filter dropped is accounted for
    /// only when it is proven to be parameter sets only; R9 refused every
    /// file with such a packet on the count.
    #[test]
    fn dropped_parameter_set_packets_are_proven_before_they_count_as_empty() {
        let line = |ts: i64, hash: u64| FrameHashLine {
            dts: Some(ts),
            pts: Some(ts),
            hash,
        };
        let stamps: Vec<PacketStamp> = (0..4)
            .map(|t| PacketStamp {
                pts: Some(t * 10),
                dts: Some(t * 10),
            })
            .collect();
        // Packet 2 (ts 10) carried only SPS/PPS: dropped by the filter;
        // the unfiltered and the parameter-sets-only passes agree on it.
        let normalized = [line(0, 100), line(20, 120), line(30, 130)];
        let framed = [line(0, 900), line(10, 777), line(20, 920), line(30, 930)];
        let ps_only = [line(0, 555), line(10, 777)];
        assert_eq!(
            align_normalized_hashes(&stamps, &normalized, &framed, &ps_only).unwrap(),
            vec![100, EMPTY_PAYLOAD_HASH, 120, 130]
        );
        // The dropped packet had picture data too (its parameter sets
        // alone are not the whole packet): refused.
        let partial = [line(0, 555), line(10, 778)];
        let err = align_normalized_hashes(&stamps, &normalized, &framed, &partial).unwrap_err();
        assert!(err.contains("not parameter sets only"), "{err}");
        // A dropped packet with no parameter sets at all: refused.
        assert!(align_normalized_hashes(&stamps, &normalized, &framed, &[]).is_err());
        // The unfiltered pass lost a packet, or its timestamps differ
        // from ffprobe's (another time base): refused.
        assert!(align_normalized_hashes(&stamps, &normalized, &framed[..3], &ps_only).is_err());
        let mut shifted = framed;
        shifted[3].dts = Some(31);
        assert!(align_normalized_hashes(&stamps, &normalized, &shifted, &ps_only).is_err());
        // Missing or repeated timestamps cannot be aligned: refused.
        let mut none = stamps.clone();
        none[1] = PacketStamp::default();
        assert!(align_normalized_hashes(&none, &normalized, &framed, &ps_only).is_err());
        // A pts missing on one packet only: refused (no mixed keys).
        let mut no_pts = stamps.clone();
        no_pts[1].pts = None;
        assert!(align_normalized_hashes(&no_pts, &normalized, &framed, &ps_only).is_err());
        let mut repeated = stamps.clone();
        repeated[2] = repeated[1];
        assert!(align_normalized_hashes(&repeated, &normalized, &framed, &ps_only).is_err());
        // A normalized line ffprobe does not list: refused.
        let extra = [line(0, 100), line(20, 120), line(30, 130), line(40, 140)];
        assert!(align_normalized_hashes(&stamps, &extra, &framed, &ps_only).is_err());
    }

    /// R11-S1: B-frames — decode order is not presentation order and pts
    /// ≠ dts. Packets are matched by pts; Matroska's `N/A` dts (ffmpeg's
    /// framehash computes one) is not compared, a listed dts is.
    #[test]
    fn bframe_packets_are_aligned_by_presentation_time() {
        // Decode order I P B B (pts 0 30 10 20, dts -20 -10 0 10), then
        // a parameter-set-only packet (pts = dts = 40).
        let ts = [(0, -20), (30, -10), (10, 0), (20, 10), (40, 40)];
        let line = |(pts, dts): (i64, i64), hash: u64| FrameHashLine {
            pts: Some(pts),
            dts: Some(dts),
            hash,
        };
        let framed: Vec<FrameHashLine> = ts.iter().map(|t| line(*t, 900 + t.0 as u64)).collect();
        let normalized: Vec<FrameHashLine> =
            ts[..4].iter().map(|t| line(*t, 100 + t.0 as u64)).collect();
        let ps_only = [line(ts[0], 1), line(ts[4], 940)];
        let want = vec![100, 130, 110, 120, EMPTY_PAYLOAD_HASH];
        let listed: Vec<PacketStamp> = ts
            .iter()
            .map(|(pts, dts)| PacketStamp {
                pts: Some(*pts),
                dts: Some(*dts),
            })
            .collect();
        assert_eq!(
            align_normalized_hashes(&listed, &normalized, &framed, &ps_only).unwrap(),
            want
        );
        // Matroska: ffprobe has no dts for the first two packets.
        let mut mkv = listed.clone();
        mkv[0].dts = None;
        mkv[1].dts = None;
        assert_eq!(
            align_normalized_hashes(&mkv, &normalized, &framed, &ps_only).unwrap(),
            want
        );
        // R10's misreading — the pair swapped — never aligns.
        let swapped: Vec<PacketStamp> = listed
            .iter()
            .map(|s| PacketStamp {
                pts: s.dts,
                dts: s.pts,
            })
            .collect();
        assert!(align_normalized_hashes(&swapped, &normalized, &framed, &ps_only).is_err());
        // A listed dts that differs from ffmpeg's: refused.
        let mut off = listed.clone();
        off[2].dts = Some(1);
        assert!(align_normalized_hashes(&off, &normalized, &framed, &ps_only).is_err());
        // A stream with no pts at all is matched by dts.
        let dts_only: Vec<PacketStamp> = listed
            .iter()
            .map(|s| PacketStamp {
                pts: None,
                dts: s.dts,
            })
            .collect();
        assert_eq!(
            align_normalized_hashes(&dts_only, &normalized, &framed, &ps_only).unwrap(),
            want
        );
    }

    /// R7-S3: the segment check lists packets without payload hashes and
    /// asks for hashes only for an edited source whose output count
    /// equals the kept total (and for the output then). R6 hashed every
    /// packet of every source and of the output on every join.
    #[test]
    fn segment_check_hashes_only_when_an_edit_list_needs_it() {
        assert!(!video_packet_args(false).contains(&"-show_data_hash"));
        assert!(video_packet_args(true).contains(&"-show_data_hash"));
        let plain_a: Vec<VideoPacket> = dts_run(0.0, 60);
        let plain_b: Vec<VideoPacket> = dts_run(2.0, 60);
        let edited: Vec<VideoPacket> = (0..90)
            .map(|i| VideoPacket {
                pts: Some(-1.0 + i as f64 / 30.0),
                dts: Some(-1.0 + i as f64 / 30.0),
                discard: i < 30,
                key: i % 30 == 0,
                hash: Some(7000 + i),
            })
            .collect();
        let run = |sources: Vec<(&str, Vec<VideoPacket>)>, output: Vec<VideoPacket>| {
            let mut calls: Vec<(String, bool)> = Vec::new();
            let paths: Vec<PathBuf> = sources.iter().map(|(n, _)| PathBuf::from(n)).collect();
            let offsets: Vec<f64> = (0..sources.len()).map(|i| i as f64 * 2.0).collect();
            let check = verify_segments_with(
                &paths,
                &offsets,
                Path::new("out"),
                &mut |path, hashes| {
                    calls.push((path.display().to_string(), hashes));
                    let mut packets = if path == Path::new("out") {
                        output.clone()
                    } else {
                        sources
                            .iter()
                            .find(|(n, _)| Path::new(n) == path)
                            .unwrap()
                            .1
                            .clone()
                    };
                    if !hashes {
                        for packet in &mut packets {
                            packet.hash = None;
                        }
                    }
                    Ok(packets)
                },
                &mut |_| Ok((0, None)),
            )
            .unwrap();
            (check, calls)
        };
        // No edit list: no hashes at all.
        let mut output = plain_a.clone();
        output.extend(plain_b.clone());
        let (check, calls) = run(vec![("a", plain_a.clone()), ("b", plain_b.clone())], output);
        assert!(check.ok, "{:?}", check.problems);
        assert!(calls.iter().all(|(_, hashes)| !hashes), "{calls:?}");
        // Edit list, output keeps every packet: still no hashes.
        let mut all = edited.clone();
        for packet in &mut all {
            packet.discard = false;
            packet.pts = packet.pts.map(|t| t + 1.0);
            packet.dts = packet.dts.map(|t| t + 1.0);
        }
        let mut output = all.clone();
        output.extend(dts_run(3.0, 60));
        let (check, calls) = run(vec![("e", edited.clone()), ("b", plain_b.clone())], output);
        assert!(check.ok, "{:?}", check.problems);
        assert!(calls.iter().all(|(_, hashes)| !hashes), "{calls:?}");
        // Edit list applied (count = kept): hashes for "e" and the output
        // only, and the payloads confirm it.
        let mut output = all[30..].to_vec();
        for packet in &mut output {
            packet.pts = packet.pts.map(|t| t - 1.0);
            packet.dts = packet.dts.map(|t| t - 1.0);
        }
        output.extend(dts_run(2.0, 60));
        let (check, calls) = run(vec![("e", edited.clone()), ("b", plain_b.clone())], output);
        assert!(check.ok, "{:?}", check.problems);
        assert_eq!(check.mode, "edit-list-applied");
        let hashed: Vec<&str> = calls
            .iter()
            .filter(|(_, hashes)| *hashes)
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(hashed, ["e", "out"]);
    }

    fn joined_case(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ft-join-c3-{tag}-{}-{}",
            std::process::id(),
            now_unix().unwrap()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(JOINED_DIR)).unwrap();
        dir
    }

    fn age(path: &Path) {
        let old = std::time::SystemTime::now() - Duration::from_secs(2 * STALE_TEMP_AGE_SECS);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    /// C3: a join killed while publishing (claim written, manifest name
    /// reserved, output published but never logged) is cleaned up by the
    /// next join's stale sweep through its claim — the old sweep removed
    /// only the `.join-*` temp files and left the unlogged output and
    /// manifest behind forever. A logged output named by a claim stays.
    #[test]
    fn stale_sweep_removes_unlogged_claimed_outputs() {
        let case = joined_case("sweep");
        let joined = case.join(JOINED_DIR);
        let output = joined.join("join_F_x_n2.mp4");
        let manifest = joined.join("join_F_x_n2.manifest.json");
        std::fs::write(&output, b"0123456789").unwrap();
        std::fs::write(&manifest, b"").unwrap();
        let claim = joined.join(".join-dead.claim.json");
        write_join_claim(
            &claim,
            &JoinClaim {
                output: output.clone(),
                manifest: manifest.clone(),
                published: Some(true),
                part_size: Some(10),
            },
            "dead",
        )
        .unwrap();
        std::fs::write(joined.join(".join-dead.lock"), b"").unwrap();
        // A logged join of another tag whose claim was left behind.
        let kept = joined.join("join_F_y_n2.mp4");
        let kept_manifest = joined.join("join_F_y_n2.manifest.json");
        std::fs::write(&kept, b"logged").unwrap();
        std::fs::write(&kept_manifest, b"{}").unwrap();
        std::fs::write(
            joined.join(LOG_FILE),
            format!(
                "{}\n",
                json!({"event": "join-video", "output_path": kept.to_string_lossy(), "manifest_path": kept_manifest.to_string_lossy()})
            ),
        )
        .unwrap();
        let kept_claim = joined.join(".join-old.claim.json");
        write_join_claim(
            &kept_claim,
            &JoinClaim {
                output: kept.clone(),
                manifest: kept_manifest.clone(),
                published: Some(true),
                part_size: Some(6),
            },
            "old",
        )
        .unwrap();
        age(&kept_claim);

        let removed = cleanup_stale_temp_files(&case, &[]);
        for (path, result) in &removed {
            assert!(result.is_ok(), "{path:?}: {result:?}");
        }
        assert!(!output.exists(), "unlogged claimed output must go");
        assert!(!manifest.exists(), "unlogged claimed manifest must go");
        assert!(!claim.exists());
        assert!(kept.exists() && kept_manifest.exists(), "logged join kept");
        assert!(!kept_claim.exists(), "the stale claim itself goes");
        let _ = std::fs::remove_dir_all(&case);
    }

    /// C3: a claim written before the publish (`published:false`) only
    /// lets the sweep remove an output of the claimed `.part` size — a
    /// different file that took the name is never this job's.
    #[test]
    fn unpublished_claim_removes_only_its_own_output() {
        let case = joined_case("unpub");
        let joined = case.join(JOINED_DIR);
        let output = joined.join("join_F_z_n2.mp4");
        let manifest = joined.join("join_F_z_n2.manifest.json");
        std::fs::write(&output, b"someone else").unwrap();
        std::fs::write(&manifest, b"").unwrap();
        let claim = JoinClaim {
            output: output.clone(),
            manifest: manifest.clone(),
            published: Some(false),
            part_size: Some(3),
        };
        let removed = remove_unlogged_claimed(&case, &claim);
        assert_eq!(removed.len(), 1, "{removed:?}");
        assert!(output.exists(), "not ours (size differs) — kept");
        assert!(!manifest.exists(), "our reservation — removed");
        std::fs::write(&output, b"abc").unwrap();
        let removed = remove_unlogged_claimed(&case, &claim);
        assert_eq!(removed.len(), 1);
        assert!(!output.exists(), "size of our .part — ours, removed");
        let _ = std::fs::remove_dir_all(&case);
    }

    /// C1/C2/C4: pid-tagged in-progress names and their sweeps.
    #[test]
    fn pid_part_names_and_sweeps() {
        let dir = joined_case("pid");
        let final_path = dir.join("evidence.raw");
        let part = pid_part_path(&final_path, 4242);
        assert_eq!(
            part.file_name().unwrap().to_string_lossy(),
            ".evidence.raw.ft4242.part"
        );
        assert!(is_pid_part_name(".evidence.raw.ft4242.part", Some(4242)));
        assert!(is_pid_part_name(
            ".evidence.raw.ft4242.part.raw",
            Some(4242)
        ));
        assert!(!is_pid_part_name(".evidence.raw.ft4242.part", Some(42)));
        assert!(!is_pid_part_name("evidence.raw.ft4242.part", None));
        assert!(!is_pid_part_name(".evidence.raw", None));
        std::fs::write(&part, b"torn").unwrap();
        std::fs::create_dir_all(dir.join(".package_1.ft4242.part/sub")).unwrap();
        std::fs::write(dir.join(".package_1.ft4242.part/sub/f"), b"x").unwrap();
        let other = dir.join(".other.bin.ft7.part");
        std::fs::write(&other, b"live").unwrap();
        std::fs::write(&final_path, b"done").unwrap();
        let removed = sweep_pid_parts(&dir, 4242);
        assert_eq!(removed.len(), 2, "{removed:?}");
        assert!(!part.exists() && !dir.join(".package_1.ft4242.part").exists());
        assert!(other.exists() && final_path.exists());
        // Stale sweep: only old parts of other processes.
        assert!(sweep_stale_parts(&dir).is_empty(), "fresh part kept");
        age(&other);
        // (R5-S5: pid 7 is declared dead here; liveness has its own test.)
        let dead = |_: u32| crate::util::ProcessProbe::Gone;
        let removed = sweep_stale_parts_with(&dir, &dead, STALE_TEMP_AGE_SECS);
        assert_eq!(removed.len(), 1);
        assert!(!other.exists() && final_path.exists());
        let own = pid_part_path(&final_path, std::process::id());
        std::fs::write(&own, b"mine").unwrap();
        age(&own);
        assert!(
            sweep_stale_parts_with(&dir, &dead, STALE_TEMP_AGE_SECS).is_empty(),
            "own part never swept"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R5-S5: an hour-old `.part` whose writer is still running (a long
    /// hash, a stalled ewfexport) is kept; a dead writer's, or a recycled
    /// pid's (a process that started after the part was born), goes; a
    /// preserved package folder is never swept. Before, age alone decided
    /// and the live job's work file was deleted under it.
    #[test]
    fn stale_sweep_spares_parts_of_live_writers() {
        use crate::util::ProcessProbe;
        let dir = joined_case("r5-live");
        // A real live writer: a child of this test.
        let mut live = if cfg!(windows) {
            let mut command = std::process::Command::new("ping");
            command.args(["-n", "30", "127.0.0.1"]);
            command
        } else {
            let mut command = std::process::Command::new("sleep");
            command.arg("30");
            command
        };
        let mut child = live
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn live writer");
        std::thread::sleep(Duration::from_millis(50));
        let live_part = dir.join(format!(".evidence.raw.ft{}.part", child.id()));
        std::fs::write(&live_part, b"hashing").unwrap();
        age(&live_part);
        assert!(
            sweep_stale_parts(&dir).is_empty(),
            "a live writer's part must survive the stale sweep"
        );
        assert!(live_part.exists());
        let _ = child.kill();
        let _ = child.wait();
        // Same name, writer gone now: removed.
        let removed = sweep_stale_parts(&dir);
        assert_eq!(removed.len(), 1, "{removed:?}");
        assert!(!live_part.exists());
        // Recycled pid: running, but started after the part was born.
        let recycled = dir.join(".x.bin.ft55.part");
        std::fs::write(&recycled, b"old").unwrap();
        age(&recycled);
        let later = std::time::SystemTime::now() + Duration::from_secs(60);
        let started_later = |_: u32| ProcessProbe::Running {
            started: Some(later),
        };
        assert_eq!(
            sweep_stale_parts_with(&dir, &started_later, STALE_TEMP_AGE_SECS).len(),
            1
        );
        // Started before the part (could be the writer) / unknown: kept.
        std::fs::write(&recycled, b"old").unwrap();
        age(&recycled);
        let earlier = std::time::SystemTime::now() - Duration::from_secs(7200);
        let started_before = |_: u32| ProcessProbe::Running {
            started: Some(earlier),
        };
        assert!(sweep_stale_parts_with(&dir, &started_before, STALE_TEMP_AGE_SECS).is_empty());
        assert!(
            sweep_stale_parts_with(&dir, &|_| ProcessProbe::Unknown, STALE_TEMP_AGE_SECS)
                .is_empty()
        );
        // R5-S12: a preserved package folder stays even with a dead writer.
        let kept = dir.join(".package_9.ft56.part");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join(PRESERVED_PART_MARKER), b"keep").unwrap();
        let removed = sweep_stale_parts_with(&dir, &|_| ProcessProbe::Gone, 0);
        assert!(removed.iter().all(|(path, _)| path != &kept), "{removed:?}");
        assert!(kept.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_liveness_table() {
        use crate::util::{ProcessProbe, writer_may_be_alive};
        let born = std::time::SystemTime::now();
        let at = |secs: i64| {
            if secs >= 0 {
                born + Duration::from_secs(secs as u64)
            } else {
                born - Duration::from_secs((-secs) as u64)
            }
        };
        assert!(!writer_may_be_alive(ProcessProbe::Gone, Some(born)));
        assert!(writer_may_be_alive(ProcessProbe::Unknown, Some(born)));
        assert!(writer_may_be_alive(
            ProcessProbe::Running { started: None },
            Some(born)
        ));
        assert!(writer_may_be_alive(
            ProcessProbe::Running {
                started: Some(at(-10))
            },
            Some(born)
        ));
        assert!(writer_may_be_alive(
            ProcessProbe::Running {
                started: Some(at(1))
            },
            Some(born)
        ));
        assert!(!writer_may_be_alive(
            ProcessProbe::Running {
                started: Some(at(30))
            },
            Some(born)
        ));
        assert!(writer_may_be_alive(
            ProcessProbe::Running {
                started: Some(at(30))
            },
            None
        ));
        // The probe itself: this process is running and started before now.
        match crate::util::probe_process(std::process::id()) {
            ProcessProbe::Running { started } => {
                if let Some(started) = started {
                    assert!(started <= std::time::SystemTime::now() + Duration::from_secs(2));
                }
            }
            other => panic!("own process probed as {other:?}"),
        }
        assert_eq!(part_name_pid(".evidence.raw.ft4242.part.raw"), Some(4242));
        assert_eq!(part_name_pid("evidence.raw"), None);
    }
}
