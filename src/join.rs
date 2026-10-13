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
use crate::util::{canonicalize_display, now_unix};
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
        let raw_path = row
            .get("source_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let source_path = match canonicalize_display(Path::new(raw_path)) {
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
        blocking.push(Issue {
            code: "stream-mismatch",
            message: format!("영상 스트림이 없는 파일 {}개", no_video.len()),
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
    let value: Value = serde_json::from_slice(&std::fs::read(claim).ok()?).ok()?;
    Some((
        PathBuf::from(value.get("output")?.as_str()?),
        PathBuf::from(value.get("manifest")?.as_str()?),
    ))
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
    if meta.len() != segment.size_bytes || modified_unix(&meta) != segment.modified_unix {
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

/// One attempt to give the verified `.part` the final name `output` without
/// ever clobbering: a hard link (fails if the name exists), else — FAT /
/// exFAT have no hard links — a copy into a `create_new` target (B14: the
/// existence check and the creation are one atomic step, so two jobs can
/// never both take a name). `Ok(false)` = the name is taken.
fn publish_part_as(part: &Path, output: &Path) -> Result<bool, String> {
    match std::fs::hard_link(part, output) {
        Ok(()) => {
            let _ = std::fs::remove_file(part);
            return Ok(true);
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(_) => {}
    }
    let mut target = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => {
            return Err(format!(
                "failed to create joined output {}: {err}",
                output.display()
            ));
        }
    };
    let copied = std::fs::File::open(part)
        .and_then(|mut source| std::io::copy(&mut source, &mut target))
        .and_then(|_| target.sync_all());
    drop(target);
    match copied {
        Ok(()) => {
            let _ = std::fs::remove_file(part);
            Ok(true)
        }
        Err(err) => {
            // The name was ours (create_new): never leave a partial copy.
            let _ = remove_file_retry(output);
            Err(format!(
                "failed to copy joined output to {}: {err}",
                output.display()
            ))
        }
    }
}

/// Publishes `.part` under the first free `<base>[__N]` name whose
/// manifest name is free too. Returns `(output, manifest)`.
fn publish_part(
    part: &Path,
    dir: &Path,
    base: &str,
    ext: &str,
) -> Result<(PathBuf, PathBuf), String> {
    for n in 1..10_000usize {
        let (output, manifest) = output_names(dir, base, ext, n);
        if manifest.exists() {
            continue;
        }
        if publish_part_as(part, &output)? {
            return Ok((output, manifest));
        }
    }
    Err(format!(
        "no free output name for {base} in {}",
        dir.display()
    ))
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
    let stderr = reader.join().unwrap_or_default();
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
    let expected = plan.totals.duration_seconds;
    let observed = probed.duration_seconds;
    let duration_check_ok =
        observed.is_some_and(|value| (value - expected).abs() <= (expected * 0.01).max(1.0));

    // 5. Sources unchanged while ffmpeg read them.
    for segment in &plan.segments {
        stat_matches(segment)?;
    }

    // 6. Output identity, publish, manifest, log.
    let output_sha256 = audit::digest_file(&temps.part)?;
    let (output_path, manifest_path) = publish_part(
        &temps.part,
        &joined_dir,
        &output_base_name(plan),
        plan.container.as_str(),
    )?;
    published.push(output_path.clone());
    // B8: name exactly what this job owns, so a killed run's cleanup never
    // has to guess (and never touches another job's output).
    let claim = json!({
        "output": output_path.to_string_lossy(),
        "manifest": manifest_path.to_string_lossy(),
        "job_tag": options.job_tag,
    });
    std::fs::write(&temps.claim, claim.to_string()).map_err(|err| {
        format!(
            "failed to record the published output in {}: {err}",
            temps.claim.display()
        )
    })?;
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
            "ok": duration_check_ok,
        },
        "not_carried": not_carried,
        "caveats": CAVEATS,
    });
    let manifest_text = format!(
        "{}\n",
        serde_json::to_string_pretty(&manifest)
            .map_err(|err| format!("failed to serialize join manifest: {err}"))?
    );
    published.push(manifest_path.clone());
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
        let (output, manifest) = publish_part(&part, &dir, "join_F_x_n2", "mp4").unwrap();
        assert_eq!(output, dir.join("join_F_x_n2__3.mp4"));
        assert_eq!(manifest, dir.join("join_F_x_n2__3.manifest.json"));
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
}
