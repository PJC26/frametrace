//! Dahua DAV / DHAV container support (first proprietary lane).
//!
//! Layout follows FFmpeg `libavformat/dhav.c` (the only public demuxer reference):
//! - optional `DAHUA` 0x400-byte file preamble, otherwise frames begin at offset 0
//! - per-frame `DHAV` header: type/subtype/channel/subnumber/frame#/length/date
//!   then (except type 0xF1) timestamp + ext_length + checksum + extension TLVs
//! - video types `0xFD` (key) / `0xFC` (non-key); audio `0xF0`; skip `0xF1`
//! - each frame ends with footer `dhav` + 4-byte back-pointer
//!
//! Remux prefers FFmpeg's native DHAV demuxer (`ffmpeg -i file.dav -c copy`).
//! Elementary-stream extraction remains for carved fragments and unit fixtures.
//! Real recorder exports still need examiner intake via
//! `scripts/validate-dav-samples.ps1` before claiming field validation.

use crate::audit;
use crate::util::run_with_timeout;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;

const DAHUA_PREAMBLE: u64 = 0x400;
const MIN_FRAME_LEN: u32 = 24;
const FOOTER_MAGIC: [u8; 4] = *b"dhav";
const STREAM_AUDIO: u8 = 0xF0;
const STREAM_SKIP: u8 = 0xF1;
/// Non-key video frame type (public for the anomaly scanner).
pub const STREAM_VIDEO_P: u8 = 0xFC;
/// Key video frame type (public for the anomaly scanner).
pub const STREAM_VIDEO_I: u8 = 0xFD;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavFrame {
    pub offset: u64,
    pub stream_type: u8,
    pub channel: u16,
    /// Packed Dahua date field with 1-second resolution (FFmpeg
    /// `get_timeinfo`): year-2000 << 26 | month << 22 | day << 17 |
    /// hour << 12 | minute << 6 | second.
    pub date: u32,
    /// Free-running millisecond counter that wraps at 65535 (FFmpeg
    /// `get_pts`: `av_rescale`/diff added into a millisecond-domain pts,
    /// `+ 65535` wrap). NOT clock seconds — do not add to `date` as-is.
    pub timestamp_ms: u16,
    pub payload_offset: u64,
    pub payload_size: u64,
}

impl DavFrame {
    /// Raw packed date value, kept for byte-stable comparisons.
    pub fn date_packed(&self) -> u32 {
        self.date
    }

    pub fn date_breakdown(&self) -> (u32, u32, u32, u32, u32, u32) {
        // Same unpacking order FFmpeg uses for the DHAV date field.
        let year = 2000 + (self.date >> 26);
        let month = (self.date >> 22) & 0xF;
        let day = (self.date >> 17) & 0x1F;
        let hour = (self.date >> 12) & 0x1F;
        let minute = (self.date >> 6) & 0x3F;
        let second = self.date & 0x3F;
        (year, month, day, hour, minute, second)
    }
}

pub fn is_dav_header(bytes: &[u8]) -> bool {
    if bytes.len() >= 5 && &bytes[0..5] == b"DAHUA" {
        return true;
    }
    if bytes.len() >= 5 && &bytes[0..4] == b"DHAV" {
        return matches!(
            bytes[4],
            STREAM_AUDIO | STREAM_SKIP | STREAM_VIDEO_P | STREAM_VIDEO_I
        );
    }
    false
}

fn read_u32_le(buf: &[u8]) -> u32 {
    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
}

/// Locates the first DHAV frame (skipping an optional DAHUA preamble).
fn locate_stream_start(file: &mut File) -> Result<u64, String> {
    let mut head = [0u8; 5];
    file.seek(SeekFrom::Start(0))
        .map_err(|err| format!("failed to seek DAV start: {err}"))?;
    file.read_exact(&mut head)
        .map_err(|err| format!("failed to read DAV header: {err}"))?;
    if &head[0..5] == b"DAHUA" {
        file.seek(SeekFrom::Start(DAHUA_PREAMBLE))
            .map_err(|err| format!("failed to seek past DAHUA preamble: {err}"))?;
        return Ok(DAHUA_PREAMBLE);
    }
    if &head[0..4] == b"DHAV" {
        file.seek(SeekFrom::Start(0))
            .map_err(|err| format!("failed to rewind DAV: {err}"))?;
        return Ok(0);
    }
    Err("not a DAV container (missing DHAV/DAHUA magic)".to_string())
}

/// Streaming frame iterator (E10): yields one audio/video frame record at
/// a time in FFmpeg order, holding only the current frame header in
/// memory — a multi-gigabyte DAV never materializes as a frame list.
/// The first parse error is yielded as `Err` and ends the iteration.
pub struct DavFrames {
    file: File,
    file_len: u64,
    pos: u64,
    done: bool,
}

/// Opens `path` and positions a [`DavFrames`] iterator at the first frame.
pub fn frames(path: &Path) -> Result<DavFrames, String> {
    let mut file =
        File::open(path).map_err(|err| format!("failed to open DAV {}: {err}", path.display()))?;
    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|err| format!("failed to stat DAV size: {err}"))?;
    let pos = locate_stream_start(&mut file)?;
    Ok(DavFrames {
        file,
        file_len,
        pos,
        done: false,
    })
}

impl Iterator for DavFrames {
    type Item = Result<DavFrame, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_frame() {
            Ok(Some(frame)) => Some(Ok(frame)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(err) => {
                self.done = true;
                Some(Err(err))
            }
        }
    }
}

/// Collects every frame record. Memory grows with the frame count — use
/// [`frames`] (streaming) or [`summarize_frames`] (counts only) for large
/// inputs; kept for callers that need random access to the frame list.
pub fn walk_frames(path: &Path) -> Result<Vec<DavFrame>, String> {
    frames(path)?.collect()
}

/// Per-channel frame counts from one streaming pass (E10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DavFrameSummary {
    /// Video (key + non-key) frames per DHAV channel number.
    pub video_frames_by_channel: BTreeMap<u16, u64>,
    pub audio_frames: u64,
    /// Channel of the first video frame — the one the ES fallback pins.
    pub first_video_channel: Option<u16>,
}

impl DavFrameSummary {
    pub fn video_frames(&self) -> u64 {
        self.video_frames_by_channel.values().sum()
    }

    /// Video frames of the pinned (first) channel — what an ES-extract
    /// export actually contains.
    pub fn pinned_channel_frames(&self) -> u64 {
        self.first_video_channel
            .and_then(|channel| self.video_frames_by_channel.get(&channel).copied())
            .unwrap_or(0)
    }

    /// `{"1":120,"2":118}` — channel → video frame count.
    pub fn channels_json(&self) -> String {
        let body = self
            .video_frames_by_channel
            .iter()
            .map(|(channel, count)| format!("\"{channel}\":{count}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{{body}}}")
    }
}

/// Counts frames per channel without retaining them. Any walk failure is
/// an error — a DAV whose frames cannot all be walked must not be logged
/// as "0 frames" or as a partial count.
pub fn summarize_frames(path: &Path) -> Result<DavFrameSummary, String> {
    let mut summary = DavFrameSummary::default();
    for frame in frames(path)? {
        let frame = frame?;
        if is_video_frame(frame.stream_type) {
            *summary
                .video_frames_by_channel
                .entry(frame.channel)
                .or_insert(0) += 1;
            summary.first_video_channel.get_or_insert(frame.channel);
        } else if frame.stream_type == STREAM_AUDIO {
            summary.audio_frames += 1;
        }
    }
    Ok(summary)
}

impl DavFrames {
    fn next_frame(&mut self) -> Result<Option<DavFrame>, String> {
        let file_len = self.file_len;
        let file = &mut self.file;
        loop {
            let pos = self.pos;
            if pos + 20 > file_len {
                return Ok(None);
            }
            match Self::read_frame(file, file_len, pos)? {
                FrameStep::End => return Ok(None),
                FrameStep::Skip(next) => self.pos = next,
                FrameStep::Frame(frame, next) => {
                    self.pos = next;
                    return Ok(Some(frame));
                }
            }
        }
    }

    fn read_frame(file: &mut File, file_len: u64, pos: u64) -> Result<FrameStep, String> {
        file.seek(SeekFrom::Start(pos))
            .map_err(|err| format!("failed to seek DAV frame at {pos}: {err}"))?;
        let mut prefix = [0u8; 20];
        match file.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(ref err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(FrameStep::End);
            }
            Err(err) => return Err(format!("failed to read DAV frame header: {err}")),
        }
        if &prefix[0..4] != b"DHAV" {
            return Err(format!(
                "DAV frame at offset {pos} is missing the DHAV frame magic (unsupported variant; real-sample validation pending)"
            ));
        }
        let stream_type = prefix[4];
        let channel = prefix[6] as u16;
        let date = read_u32_le(&prefix[16..20]);
        let frame_length = read_u32_le(&prefix[12..16]);
        if frame_length < MIN_FRAME_LEN {
            return Err(format!(
                "DAV frame at offset {pos} has invalid length {frame_length}"
            ));
        }
        let frame_end = pos.saturating_add(frame_length as u64);
        if frame_end > file_len {
            return Err(format!(
                "DAV frame at offset {pos} extends past EOF (length {frame_length})"
            ));
        }

        if stream_type == STREAM_SKIP {
            return Ok(FrameStep::Skip(frame_end));
        }

        let mut cursor = pos + 20;
        if cursor + 4 > file_len {
            return Ok(FrameStep::End);
        }
        let mut meta = [0u8; 4];
        file.seek(SeekFrom::Start(cursor))
            .map_err(|err| format!("failed to seek DAV meta: {err}"))?;
        file.read_exact(&mut meta)
            .map_err(|err| format!("failed to read DAV meta: {err}"))?;
        let timestamp_ms = u16::from_le_bytes([meta[0], meta[1]]);
        let ext_length = meta[2] as u64;
        cursor += 4 + ext_length;

        let footer_start = frame_end.saturating_sub(8);
        if cursor > footer_start {
            return Err(format!(
                "DAV frame at offset {pos} has no room for payload/footer"
            ));
        }
        let payload_size = footer_start - cursor;

        file.seek(SeekFrom::Start(footer_start))
            .map_err(|err| format!("failed to seek DAV footer: {err}"))?;
        let mut footer = [0u8; 8];
        file.read_exact(&mut footer)
            .map_err(|err| format!("failed to read DAV footer: {err}"))?;
        if footer[0..4] != FOOTER_MAGIC {
            return Err(format!(
                "DAV frame at offset {pos} has a corrupted end marker (found {:02x?})",
                &footer[0..4]
            ));
        }

        if matches!(stream_type, STREAM_AUDIO | STREAM_VIDEO_P | STREAM_VIDEO_I) {
            return Ok(FrameStep::Frame(
                DavFrame {
                    offset: pos,
                    stream_type,
                    channel,
                    date,
                    timestamp_ms,
                    payload_offset: cursor,
                    payload_size,
                },
                frame_end,
            ));
        }
        Ok(FrameStep::Skip(frame_end))
    }
}

enum FrameStep {
    End,
    Skip(u64),
    Frame(DavFrame, u64),
}

fn is_video_frame(stream_type: u8) -> bool {
    matches!(stream_type, STREAM_VIDEO_P | STREAM_VIDEO_I)
}

/// Copies the video-frame payloads of a single channel into an Annex-B
/// elementary stream. Multi-channel recorders interleave frames per
/// channel; concatenating across channels produces an unplayable mix, so
/// the first video frame's channel is pinned for the whole stream.
///
/// One streaming pass (E10): frames are copied as the walk reaches them,
/// so memory stays bounded by one 64 KiB copy buffer regardless of the
/// file's frame count. A walk error mid-file fails the extraction and
/// removes the partial ES output — it is never returned as a short
/// "successful" stream.
pub fn extract_video_es(
    dav_path: &Path,
    es_output: &Path,
) -> Result<(u64, usize, Option<u16>), String> {
    let walk = frames(dav_path)?;
    if let Some(parent) = es_output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create ES output directory: {err}"))?;
    }
    // R5-P12: the output is created here, never truncated or replaced —
    // an existing file at that name is refused — so the cleanup below can
    // only ever remove the file this call made.
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(es_output)
        .map_err(|err| {
            format!(
                "failed to create ES output {} (an existing file is never overwritten): {err}",
                es_output.display()
            )
        })?;
    let result = copy_pinned_channel(walk, dav_path, output);
    if result.is_err() {
        let _ = std::fs::remove_file(es_output);
    }
    result
}

fn copy_pinned_channel(
    walk: DavFrames,
    dav_path: &Path,
    mut output: File,
) -> Result<(u64, usize, Option<u16>), String> {
    let mut input =
        BufReader::new(File::open(dav_path).map_err(|err| format!("failed to open DAV: {err}"))?);
    let mut channel: Option<u16> = None;
    let mut copied_frames = 0usize;
    let mut written = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    for frame in walk {
        let frame = frame?;
        if !is_video_frame(frame.stream_type) {
            continue;
        }
        let pinned = *channel.get_or_insert(frame.channel);
        if frame.channel != pinned {
            continue;
        }
        input
            .seek(SeekFrom::Start(frame.payload_offset))
            .map_err(|err| format!("failed to seek DAV frame: {err}"))?;
        let mut remaining = frame.payload_size;
        while remaining > 0 {
            let want = remaining.min(chunk.len() as u64) as usize;
            input
                .read_exact(&mut chunk[..want])
                .map_err(|err| format!("failed to read DAV payload: {err}"))?;
            output
                .write_all(&chunk[..want])
                .map_err(|err| format!("failed to write ES output: {err}"))?;
            written += want as u64;
            remaining -= want as u64;
        }
        copied_frames += 1;
    }
    if copied_frames == 0 {
        return Err("DAV contains no video frames".to_string());
    }
    output
        .flush()
        .and_then(|_| output.sync_all())
        .map_err(|err| format!("failed to finalize ES output: {err}"))?;
    Ok((written, copied_frames, channel))
}

/// Prefer FFmpeg's native DHAV demuxer; fall back to ES extract + remux.
pub fn remux_dav_to_mp4(
    dav_path: &Path,
    mp4_output: &Path,
    timeout_secs: Option<u64>,
) -> Result<&'static str, String> {
    let ffmpeg = crate::tool_policy::resolve_tool_binary("ffmpeg", &["ffmpeg"])
        .map_err(|err| format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)"))?;
    if let Some(parent) = mp4_output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create export directory: {err}"))?;
    }

    let mut command = Command::new(&ffmpeg);
    command
        .args(["-y", "-v", "error", "-f", "dhav", "-i"])
        .arg(dav_path)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(mp4_output);
    let output = run_with_timeout(&mut command, timeout_secs)?;
    if output.status.success() && mp4_output.exists() {
        return Ok("ffmpeg-dhav-demux");
    }
    let native_err = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let _ = std::fs::remove_file(mp4_output);

    // R5-P12: a fresh name next to the output (never an existing file).
    let es_path = crate::util::unique_available_path(&mp4_output.with_extension("es.bin"));
    let _ = extract_video_es(dav_path, &es_path)?;
    remux_es_to_mp4(&es_path, mp4_output, timeout_secs).map_err(|err| {
        format!("ffmpeg native demux failed ({native_err}); ES remux also failed: {err}")
    })?;
    let _ = std::fs::remove_file(&es_path);
    Ok("es-extract-remux")
}

/// Remuxes an Annex-B elementary stream into MP4 without re-encoding.
pub fn remux_es_to_mp4(
    es_path: &Path,
    mp4_output: &Path,
    timeout_secs: Option<u64>,
) -> Result<(), String> {
    let ffmpeg = crate::tool_policy::resolve_tool_binary("ffmpeg", &["ffmpeg"])
        .map_err(|err| format!("{err} (install FFmpeg and ensure ffmpeg is in PATH)"))?;
    if let Some(parent) = mp4_output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create export directory: {err}"))?;
    }
    let mut last_error = String::new();
    for format in ["h264", "hevc"] {
        let mut command = Command::new(&ffmpeg);
        command
            .args(["-y", "-v", "error", "-f", format, "-i"])
            .arg(es_path)
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(mp4_output);
        let output = run_with_timeout(&mut command, timeout_secs)?;
        if output.status.success() && mp4_output.exists() {
            return Ok(());
        }
        last_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let _ = std::fs::remove_file(mp4_output);
    }
    Err(format!(
        "DAV elementary stream could not be remuxed as h264 or hevc: {last_error}"
    ))
}

pub fn digest(path: &Path) -> Result<String, String> {
    audit::digest_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_frame(stream_type: u8, channel: u8, payload: &[u8]) -> Vec<u8> {
        let header_len = 24u32;
        let frame_length = header_len + payload.len() as u32 + 8;
        let mut bytes = Vec::with_capacity(frame_length as usize);
        bytes.extend_from_slice(b"DHAV");
        bytes.push(stream_type);
        bytes.push(0); // subtype
        bytes.push(channel);
        bytes.push(0); // frame_subnumber
        bytes.extend_from_slice(&1u32.to_le_bytes()); // frame_number
        bytes.extend_from_slice(&frame_length.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // date
        bytes.extend_from_slice(&0u16.to_le_bytes()); // timestamp
        bytes.push(0); // ext_length
        bytes.push(0); // checksum
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&FOOTER_MAGIC);
        bytes.extend_from_slice(&frame_length.to_le_bytes());
        bytes
    }

    fn fixture_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(build_frame(STREAM_VIDEO_I, 1, b"VIDEO_PAYLOAD_ONE"));
        bytes.extend(build_frame(STREAM_AUDIO, 1, b"AUDIO"));
        bytes.extend(build_frame(STREAM_VIDEO_P, 1, b"VIDEO_PAYLOAD_TWO"));
        bytes
    }

    fn fixture_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, fixture_bytes()).unwrap();
        path
    }

    #[test]
    fn header_gate_rejects_mp4_and_accepts_dav() {
        // Ordinary MP4 must never enter the DAV export path — ffmpeg would
        // happily remux it with `-c copy` and a pass would be miscounted.
        assert!(!is_dav_header(b"\x00\x00\x00\x20ftypisom"));
        assert!(!is_dav_header(b"RIFF...."));
        assert!(is_dav_header(b"DAHUA"));
        assert!(is_dav_header(b"DHAV\xFD"));
        assert!(is_dav_header(b"DHAV\xF0"));
        assert!(!is_dav_header(b"DHAV\x99")); // unknown frame type
        assert!(!is_dav_header(b"DHA"));
    }

    #[test]
    fn rejects_files_without_dhav_magic() {
        let path = std::env::temp_dir().join(format!("ft-dav-bad-{}", std::process::id()));
        std::fs::write(&path, b"not a dav file at all").unwrap();
        let error = walk_frames(&path).unwrap_err();
        assert!(error.contains("not a DAV container"), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn walks_video_and_audio_frames_in_order() {
        let path = fixture_path("ft-dav-walk");
        let frames = walk_frames(&path).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].stream_type, STREAM_VIDEO_I);
        assert_eq!(frames[0].payload_size, b"VIDEO_PAYLOAD_ONE".len() as u64);
        assert_eq!(frames[1].stream_type, STREAM_AUDIO);
        assert_eq!(frames[2].channel, 1);
        assert_eq!(frames[2].payload_size, b"VIDEO_PAYLOAD_TWO".len() as u64);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn extract_concatenates_only_video_payloads() {
        let path = fixture_path("ft-dav-extract");
        let es = std::env::temp_dir().join(format!("ft-dav-es-{}.bin", std::process::id()));
        let (written, frames, channel) = extract_video_es(&path, &es).unwrap();
        assert_eq!(
            written,
            (b"VIDEO_PAYLOAD_ONE".len() + b"VIDEO_PAYLOAD_TWO".len()) as u64
        );
        assert_eq!(frames, 2);
        assert_eq!(channel, Some(1));
        let content = std::fs::read(&es).unwrap();
        assert_eq!(content, b"VIDEO_PAYLOAD_ONEVIDEO_PAYLOAD_TWO".to_vec());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&es);
    }

    /// Multi-channel recorders interleave frames per channel; the ES must
    /// contain only the pinned first channel's payloads, never a mix.
    #[test]
    fn extract_pins_one_channel_in_multichannel_dav() {
        let mut bytes = Vec::new();
        bytes.extend(build_frame(STREAM_VIDEO_I, 1, b"CH1_A"));
        bytes.extend(build_frame(STREAM_VIDEO_I, 2, b"CH2_A"));
        bytes.extend(build_frame(STREAM_VIDEO_P, 1, b"CH1_B"));
        bytes.extend(build_frame(STREAM_VIDEO_P, 2, b"CH2_B"));
        let path = std::env::temp_dir().join(format!("ft-dav-mc-{}", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let es = std::env::temp_dir().join(format!("ft-dav-mc-es-{}.bin", std::process::id()));
        let (written, frames, channel) = extract_video_es(&path, &es).unwrap();
        assert_eq!(channel, Some(1));
        assert_eq!(frames, 2);
        let content = std::fs::read(&es).unwrap();
        assert_eq!(content, b"CH1_ACH1_B".to_vec());
        assert_eq!(written, (b"CH1_A".len() + b"CH1_B".len()) as u64);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&es);
    }

    /// E10: per-channel counts in one streaming pass. The DAV export used
    /// to log every channel's video frames as "video_frames" next to the
    /// first channel's number (and 0 when the walk failed); the summary
    /// keeps channels apart and a walk failure is an error.
    #[test]
    fn summary_counts_frames_per_channel_and_fails_on_walk_errors() {
        let mut bytes = Vec::new();
        bytes.extend(build_frame(STREAM_VIDEO_I, 2, b"A"));
        bytes.extend(build_frame(STREAM_VIDEO_I, 1, b"B"));
        bytes.extend(build_frame(STREAM_AUDIO, 2, b"C"));
        bytes.extend(build_frame(STREAM_VIDEO_P, 2, b"D"));
        bytes.extend(build_frame(STREAM_VIDEO_P, 2, b"E"));
        let path = std::env::temp_dir().join(format!("ft-dav-summary-{}", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let summary = summarize_frames(&path).unwrap();
        assert_eq!(summary.first_video_channel, Some(2));
        assert_eq!(summary.video_frames(), 4);
        assert_eq!(summary.pinned_channel_frames(), 3);
        assert_eq!(summary.audio_frames, 1);
        assert_eq!(summary.channels_json(), r#"{"1":1,"2":3}"#);

        // Same file with a corrupted last footer: an error, never a count.
        let last = bytes.len() - 8;
        bytes[last] = 0xFF;
        std::fs::write(&path, &bytes).unwrap();
        let err = summarize_frames(&path).unwrap_err();
        assert!(err.contains("corrupted end marker"), "{err}");
        // The iterator yields the good frames, then the error, then stops.
        let items: Vec<_> = frames(&path).unwrap().collect();
        assert_eq!(items.len(), 5);
        assert!(items[..4].iter().all(Result::is_ok));
        assert!(items[4].is_err());
        // ES extraction refuses and leaves no partial output behind.
        let es = std::env::temp_dir().join(format!("ft-dav-summary-es-{}.bin", std::process::id()));
        assert!(extract_video_es(&path, &es).is_err());
        assert!(!es.exists(), "partial ES output must be removed");
        let _ = std::fs::remove_file(&path);
    }

    /// R5-P12: ES extraction never deletes or truncates a file it did not
    /// create — an existing file at the output name is refused and kept.
    #[test]
    fn es_extract_never_removes_an_existing_file() {
        let mut bytes = fixture_bytes();
        let last_magic = bytes.len() - 8;
        bytes[last_magic] = 0xFF; // walk fails mid-file
        let path = std::env::temp_dir().join(format!("ft-dav-keep-{}", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let es = std::env::temp_dir().join(format!("ft-dav-keep-es-{}.bin", std::process::id()));
        std::fs::write(&es, b"examiner's own file").unwrap();
        assert!(extract_video_es(&path, &es).is_err());
        assert_eq!(std::fs::read(&es).unwrap(), b"examiner's own file");
        // Same for a walk that would succeed: refused, not overwritten.
        let good = fixture_path("ft-dav-keep-good");
        assert!(extract_video_es(&good, &es).is_err());
        assert_eq!(std::fs::read(&es).unwrap(), b"examiner's own file");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&es);
        let _ = std::fs::remove_file(&good);
    }

    #[test]
    fn corrupt_end_marker_is_reported_not_guessed() {
        let mut bytes = fixture_bytes();
        let last_magic = bytes.len() - 8;
        bytes[last_magic] = 0xFF;
        let path = std::env::temp_dir().join(format!("ft-dav-corrupt-{}", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let error = walk_frames(&path).unwrap_err();
        assert!(error.contains("corrupted end marker"), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn accepts_dahua_preamble() {
        let mut bytes = vec![0u8; DAHUA_PREAMBLE as usize];
        bytes[0..5].copy_from_slice(b"DAHUA");
        bytes.extend(build_frame(STREAM_VIDEO_I, 2, b"PREAMBLE_VIDEO"));
        let path = std::env::temp_dir().join(format!("ft-dav-preamble-{}", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let frames = walk_frames(&path).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].channel, 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parses_packed_date_and_clock_timestamp() {
        // 2026-09-08 14:03:07 in the FFmpeg get_date() packing:
        // (year-2000)<<26 | month<<22 | day<<17 | hour<<12 | minute<<6 | second.
        let packed = (26u32 << 26) | (9 << 22) | (8 << 17) | (14 << 12) | (3 << 6) | 7;
        let mut bytes = Vec::new();
        bytes.extend(build_frame_with_date(
            STREAM_VIDEO_I,
            1,
            packed,
            42,
            b"DATED",
        ));
        let path = std::env::temp_dir().join(format!("ft-dav-date-{}", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let frames = walk_frames(&path).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].date_packed(), packed);
        assert_eq!(frames[0].timestamp_ms, 42);
        assert_eq!(frames[0].date_breakdown(), (2026, 9, 8, 14, 3, 7));
        let _ = std::fs::remove_file(&path);
    }

    fn build_frame_with_date(
        stream_type: u8,
        channel: u8,
        date: u32,
        timestamp: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let header_len = 24u32;
        let frame_length = header_len + payload.len() as u32 + 8;
        let mut bytes = Vec::with_capacity(frame_length as usize);
        bytes.extend_from_slice(b"DHAV");
        bytes.push(stream_type);
        bytes.push(0);
        bytes.push(channel);
        bytes.push(0);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&frame_length.to_le_bytes());
        bytes.extend_from_slice(&date.to_le_bytes());
        bytes.extend_from_slice(&timestamp.to_le_bytes());
        bytes.push(0);
        bytes.push(0);
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&FOOTER_MAGIC);
        bytes.extend_from_slice(&frame_length.to_le_bytes());
        bytes
    }
}
