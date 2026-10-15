//! Dashcam telemetry extraction (GPS track / G-sensor side data).
//!
//! Honest scope: NMEA sentences are decoded fully — whether they ride a
//! dedicated `gps`/`text` data stream (common in Thinkware/FineVi/BlackVue
//! MP4s) or sit as free-box strings inside the file. camm (Camera Motion
//! Metadata) tracks are decoded per the public spec — implemented and
//! unit-tested, but flagged as not-yet-validated against a real recorder
//! sample. Vendor-proprietary `bin_data` streams are still reported as
//! present-but-unparsed rather than guessed at.
//!
//! Timestamps come from NMEA RMC date+time fields and are treated as UTC —
//! dashcams that log local time still produce a consistent, documented
//! timeline rather than a silently shifted one.

use crate::tool_policy::resolve_tool_binary;
use crate::util::run_with_timeout;
use crate::video_export::{resolve_video_source, sanitize_filename};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const EDGE_SCAN_BYTES: usize = 4 * 1024 * 1024;
const MAX_POINTS: usize = 20_000;
const TOOL_TIMEOUT: u64 = 60;

#[derive(Debug, Clone, Serialize)]
pub struct TelemetryPoint {
    pub lat: f64,
    pub lon: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts_unix: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_kmh: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt_m: Option<f64>,
}

/// Bounded point collector (E9): keeps at most `MAX_POINTS` points in
/// memory but counts every decoded point, so a capped report can say how
/// many it dropped instead of truncating silently.
#[derive(Debug, Default)]
struct PointSink {
    points: Vec<TelemetryPoint>,
    total: usize,
}

impl PointSink {
    fn push(&mut self, point: TelemetryPoint) {
        self.total += 1;
        if self.points.len() < MAX_POINTS {
            self.points.push(point);
        }
    }

    fn truncated(&self) -> bool {
        self.total > self.points.len()
    }
}

/// Per-case counts from a camm (Camera Motion Metadata) stream — kept
/// alongside the decoded points so the examiner sees how much of the
/// stream the spec-based parser covered versus skipped.
#[derive(Debug, Serialize)]
pub struct CammStats {
    pub packets: usize,
    /// case 5 (lat/lon/alt) + case 6 (full GPS) packets that yielded a point.
    pub gps_points: usize,
    /// case 6 packets skipped because gps_fix_type was 0 (no fix).
    pub gps_no_fix: usize,
    /// cases 0/1/2/3/4/7 (angle-axis, exposure, gyro, acceleration,
    /// position, magnetic field) counted but not surfaced as track points.
    pub imu_packets: usize,
    pub unknown_packets: usize,
}

#[derive(Debug, Serialize)]
pub struct TelemetryReport {
    pub schema_version: u32,
    pub event: &'static str,
    pub extracted_unix: u64,
    pub selector: String,
    pub source_path: String,
    /// "nmea-data-stream" | "nmea-file-scan" | "camm-gps" | combos | "none"
    pub source: String,
    pub points: Vec<TelemetryPoint>,
    /// Telemetry-looking streams we detected but cannot decode honestly
    /// (vendor-proprietary `bin_data` payloads, failed dumps).
    pub unparsed_streams: Vec<String>,
    /// Present when a camm data stream was found and decoded per the
    /// public Camera Motion Metadata spec — the layout is implemented
    /// and unit-tested, but no real recorder sample has validated it
    /// here, so points carry the caveat in `note`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camm: Option<CammStats>,
    /// Points kept in `points` (at most 20,000).
    pub point_count: usize,
    /// Every point decoded, including those beyond the cap (E9).
    pub total_points: usize,
    /// `true` when `points` holds fewer than `total_points`: the track was
    /// cut at the cap and `first/last_ts_unix`/`max_speed_kmh` describe the
    /// kept points only.
    pub truncated: bool,
    pub first_ts_unix: Option<f64>,
    pub last_ts_unix: Option<f64>,
    pub max_speed_kmh: Option<f64>,
    pub note: String,
}

/// Parses one NMEA sentence (RMC or GGA, any `$G?` talker). Returns a
/// point only when a valid lat/lon pair is present.
fn parse_nmea(sentence: &str) -> Option<TelemetryPoint> {
    let s = sentence.trim();
    if s.len() < 10 || !s.starts_with("$G") {
        return None;
    }
    let s = s.split('*').next().unwrap_or(s);
    let f: Vec<&str> = s.split(',').collect();
    let kind = f[0].get(3..)?;
    match kind {
        "RMC" => {
            if f.len() < 10 || f[2] != "A" {
                return None;
            }
            let lat = parse_coord(f[3], f[4])?;
            let lon = parse_coord(f[5], f[6])?;
            let speed = f[7].parse::<f64>().ok().map(|k| k * 1.852);
            let ts = rmc_unix(f[1], f[9]);
            Some(TelemetryPoint {
                lat,
                lon,
                ts_unix: ts,
                speed_kmh: speed,
                alt_m: None,
            })
        }
        "GGA" => {
            if f.len() < 10 {
                return None;
            }
            let lat = parse_coord(f[2], f[3])?;
            let lon = parse_coord(f[4], f[5])?;
            let alt = f[9].parse::<f64>().ok();
            Some(TelemetryPoint {
                lat,
                lon,
                ts_unix: None,
                speed_kmh: None,
                alt_m: alt,
            })
        }
        _ => None,
    }
}

/// ddmm.mmmm / dddmm.mmmm + hemisphere → signed decimal degrees.
fn parse_coord(raw: &str, hemi: &str) -> Option<f64> {
    let v: f64 = raw.parse().ok()?;
    if !(0.0..18000.0).contains(&v) {
        return None;
    }
    let deg = (v / 100.0).floor();
    let min = v - deg * 100.0;
    if min >= 60.0 {
        return None;
    }
    let mut out = deg + min / 60.0;
    if hemi == "S" || hemi == "W" {
        out = -out;
    }
    if !(-180.0..=180.0).contains(&out) {
        return None;
    }
    Some(out)
}

/// hhmmss.ss + ddmmyy → unix epoch (UTC assumption, documented).
fn rmc_unix(time: &str, date: &str) -> Option<f64> {
    let t: f64 = time.parse().ok()?;
    let d: i64 = date.parse().ok()?;
    let (sec, min, hour) = (
        t % 100.0,
        ((t / 100.0) as i64) % 100,
        ((t / 10000.0) as i64) % 100,
    );
    let (day, mon, mut yr) = (d / 10000, (d / 100) % 100, d % 100);
    yr += if yr < 80 { 2000 } else { 1900 };
    // days-from-civil (Howard Hinnant) — no external date crate needed.
    let y = if mon <= 2 { yr - 1 } else { yr };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mon + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    if hour > 23 || min > 59 || sec >= 60.0 || !(1..=31).contains(&day) || !(1..=12).contains(&mon)
    {
        return None;
    }
    Some(days as f64 * 86400.0 + hour as f64 * 3600.0 + min as f64 * 60.0 + sec)
}

/// Scans raw bytes for NMEA sentences ($G?RMC / $G?GGA). Sentences are
/// located anywhere in the byte stream — dashcams often embed them
/// mid-record between binary fields, so no line-start alignment is
/// assumed; each candidate is bounded by the next CR/LF or 200 bytes.
fn scan_nmea_bytes(bytes: &[u8], points: &mut PointSink) {
    let mut pos = 0usize;
    while pos + 1 < bytes.len() {
        let Some(off) = bytes[pos..].windows(2).position(|w| w == b"$G") else {
            return;
        };
        let start = pos + off;
        let hard_end = (start + 200).min(bytes.len());
        let end = bytes[start..hard_end]
            .iter()
            .position(|b| *b == b'\n' || *b == b'\r')
            .map(|e| start + e)
            .unwrap_or(hard_end);
        let line = String::from_utf8_lossy(&bytes[start..end]);
        if line.len() > 10
            && line
                .get(3..)
                .is_some_and(|rest| rest.starts_with("RMC,") || rest.starts_with("GGA,"))
            && let Some(p) = parse_nmea(&line)
        {
            points.push(p);
        }
        pos = start + 2;
    }
}

fn run_tool(binary: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    run_tool_timed(binary, args, TOOL_TIMEOUT)
}

fn run_tool_timed(binary: &str, args: &[&str], timeout_secs: u64) -> Result<Vec<u8>, String> {
    let bin = resolve_tool_binary(binary, &[binary])?;
    let mut cmd = Command::new(&bin);
    cmd.args(args);
    let out = run_with_timeout(&mut cmd, Some(timeout_secs))?;
    if !out.status.success() {
        return Err(format!(
            "{binary} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Full-file demux passes (`-show_packets`, `-f data`) read every byte of
/// the container, so a fixed probe budget can kill legitimate multi-gigabyte
/// telemetry sources. Scale the budget with file size — ~25 MiB/s floor for
/// slow external/NASD media — capped so a hung tool still dies.
fn demux_timeout(path: &Path) -> u64 {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    (TOOL_TIMEOUT + bytes / (25 * 1024 * 1024)).min(3600)
}

/// Data/other stream indexes from ffprobe, split by payload kind:
/// `nmea` = generic data/other streams worth a free-scan, `camm` =
/// Camera Motion Metadata tracks decoded per the public spec.
fn telemetry_streams(path: &Path) -> (Vec<u32>, Vec<u32>) {
    let probe = crate::ffprobe::probe(path);
    let mut nmea = Vec::new();
    let mut camm = Vec::new();
    if let Some(raw) = probe.raw_json
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(streams) = json.get("streams").and_then(|s| s.as_array())
    {
        for st in streams {
            let idx = st.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
            let codec_type = st.get("codec_type").and_then(|c| c.as_str()).unwrap_or("");
            let codec_name = st.get("codec_name").and_then(|c| c.as_str()).unwrap_or("");
            // Real camm tracks surface as codec_name=bin_data with
            // codec_tag_string=camm (the mov demuxer maps the 4CC), so
            // the tag — not the codec name — is the reliable signal.
            let tag = st
                .get("codec_tag_string")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if codec_name == "camm" || tag == "camm" {
                camm.push(idx);
            } else if codec_type == "data" || codec_type == "other" {
                nmea.push(idx);
            }
        }
    }
    (nmea, camm)
}

/// Seconds between the GPS epoch (1980-01-06T00:00:00Z) and the Unix
/// epoch. camm case-6 `time_gps_epoch` counts GPS seconds; the ~18s of
/// accumulated leap seconds are not corrected here — flagged in `note`.
const GPS_EPOCH_OFFSET: f64 = 315_964_800.0;

fn le_f32(b: &[u8], off: usize) -> f64 {
    f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]) as f64
}

fn le_f64(b: &[u8], off: usize) -> f64 {
    f64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

fn le_i32(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Packet sizes (4-byte header included) per the public Camera Motion
/// Metadata Spec, Google Street View Publish API —
/// <https://developers.google.com/streetview/publish/camm-spec>
/// (page "Last updated 2025-08-28 UTC", fetched 2026-10-14). The spec
/// declares a header of `uint16 reserved; uint16 type;` followed by:
///
/// ```text
/// case 0: float angle_axis[3]                                 (12 B)
/// case 1: int32 pixel_exposure_time; int32 rolling_shutter_skew_time (8 B)
/// case 2: float gyro[3]                                       (12 B)
/// case 3: float acceleration[3]                               (12 B)
/// case 4: float position[3]                                   (12 B)
/// case 5: double latitude; double longitude; double altitude  (24 B)
/// case 6: double time_gps_epoch; int gps_fix_type;
///         double latitude; double longitude; float altitude;
///         float horizontal_accuracy; float vertical_accuracy;
///         float velocity_east; float velocity_north; float velocity_up;
///         float speed_accuracy                                (56 B)
/// case 7: float magnetic_field[3]                             (12 B)
/// ```
///
/// "All fields are little-endian ... and the 32-bit floating points are
/// of IEEE 754-1985 format"; the spec states no padding, so fields are
/// packed in declaration order. E6 corrected this parser: it previously
/// read the full GPS record as case 7 (which is the magnetometer) with a
/// 4-byte float `time_gps_epoch`, and dropped case 5's altitude.
const CAMM_CASE5_LEN: usize = 4 + 24;
const CAMM_CASE6_LEN: usize = 4 + 56;

/// R5-P11: a camm position worth a track point — finite, in range, and
/// not the (0, 0) "null island" a recorder writes before it has a fix.
fn plausible_fix(lat: f64, lon: f64) -> bool {
    lat.is_finite()
        && lon.is_finite()
        && lat.abs() <= 90.0
        && lon.abs() <= 180.0
        && !(lat == 0.0 && lon == 0.0)
}

/// Parses one camm sample (4-byte header + type-specific payload).
/// Cases 5 and 6 produce track points; the other spec cases only bump
/// counters — a packet shorter than the spec size is counted unknown
/// rather than mis-parsed.
fn parse_camm_packet(packet: &[u8], points: &mut PointSink, stats: &mut CammStats) {
    if packet.len() < 4 {
        stats.unknown_packets += 1;
        return;
    }
    // `uint16 type`, little-endian, at bytes 2..4.
    let ty = u16::from_le_bytes([packet[2], packet[3]]);
    match ty {
        5 if packet.len() >= CAMM_CASE5_LEN => {
            let (lat, lon, alt) = (le_f64(packet, 4), le_f64(packet, 12), le_f64(packet, 20));
            if plausible_fix(lat, lon) {
                points.push(TelemetryPoint {
                    lat,
                    lon,
                    ts_unix: None,
                    speed_kmh: None,
                    alt_m: alt.is_finite().then_some(alt),
                });
                stats.gps_points += 1;
            } else {
                stats.unknown_packets += 1;
            }
        }
        6 if packet.len() >= CAMM_CASE6_LEN => {
            // Offsets (header included): 4 time_gps_epoch(f64) ·
            // 12 gps_fix_type(i32) · 16 lat(f64) · 24 lon(f64) ·
            // 32 alt(f32) · 36 h_acc · 40 v_acc · 44 vel_e · 48 vel_n ·
            // 52 vel_up · 56 speed_acc.
            let fix = le_i32(packet, 12);
            if fix == 0 {
                stats.gps_no_fix += 1;
                return;
            }
            let (lat, lon) = (le_f64(packet, 16), le_f64(packet, 24));
            if !plausible_fix(lat, lon) {
                stats.unknown_packets += 1;
                return;
            }
            // R5-P11: ground speed is the HORIZONTAL velocity (east, north);
            // velocity_up (offset 52) is climb rate, not road speed.
            let speed_ms = (le_f32(packet, 44).powi(2) + le_f32(packet, 48).powi(2)).sqrt();
            let gps_seconds = le_f64(packet, 4);
            points.push(TelemetryPoint {
                lat,
                lon,
                ts_unix: gps_seconds
                    .is_finite()
                    .then_some(gps_seconds + GPS_EPOCH_OFFSET),
                speed_kmh: speed_ms.is_finite().then_some(speed_ms * 3.6),
                alt_m: Some(le_f32(packet, 32)).filter(|alt| alt.is_finite()),
            });
            stats.gps_points += 1;
        }
        0..=4 | 7 => {
            stats.imu_packets += 1;
        }
        _ => {
            stats.unknown_packets += 1;
        }
    }
}

/// Packet payload sizes for one stream, from ffprobe -show_packets —
/// `-f data` dumps concatenate payloads with no delimiters, so sample
/// boundaries must come from the demuxer, not the byte stream itself.
fn packet_sizes(path: &Path, stream_index: u32) -> Result<Vec<usize>, String> {
    let idx = stream_index.to_string();
    // One size per row keeps the output small — the default packet dump
    // emits a dozen fields per packet for a stream we only need lengths for.
    let bytes = run_tool_timed(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            &idx,
            "-show_entries",
            "packet=size",
            "-of",
            "csv=p=0",
            &path.to_string_lossy(),
        ],
        demux_timeout(path),
    )?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| line.rsplit(',').next()?.trim().parse::<usize>().ok())
        .collect())
}

/// Decodes one camm data stream into track points + coverage stats.
/// Packet boundaries come from ffprobe; each declared-size packet is
/// then parsed per the spec. A short dump or a packet smaller than the
/// spec minimum ends the walk — what was decoded stays decoded.
fn extract_camm_stream(
    path: &Path,
    stream_index: u32,
    points: &mut PointSink,
) -> Result<CammStats, String> {
    let sizes = packet_sizes(path, stream_index)?;
    if sizes.is_empty() {
        return Err(format!("stream #{stream_index} — no packets reported"));
    }
    let idx = stream_index.to_string();
    let dump = run_tool_timed(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            &path.to_string_lossy(),
            "-map",
            &format!("0:{idx}"),
            "-c",
            "copy",
            "-f",
            "data",
            "-",
        ],
        demux_timeout(path),
    )?;
    let mut stats = CammStats {
        packets: 0,
        gps_points: 0,
        gps_no_fix: 0,
        imu_packets: 0,
        unknown_packets: 0,
    };
    parse_camm_dump(&dump, &sizes, points, &mut stats);
    Ok(stats)
}

/// Walks a concatenated `-f data` dump using the demuxer's packet
/// sizes — the only honest boundary source, since the payloads carry
/// no length prefix. A dump shorter than the declared sizes marks the
/// remainder unknown and stops; a trailing tail beyond the last
/// declared packet is ignored (muxer padding).
fn parse_camm_dump(dump: &[u8], sizes: &[usize], points: &mut PointSink, stats: &mut CammStats) {
    let mut cursor = 0usize;
    for size in sizes {
        if cursor + size > dump.len() {
            stats.unknown_packets += 1;
            return;
        }
        stats.packets += 1;
        // Walks every packet even past the point cap: the sink keeps the
        // first MAX_POINTS and counts the rest (E9).
        parse_camm_packet(&dump[cursor..cursor + size], points, stats);
        cursor += size;
    }
}

/// Extracts a telemetry report for one video file.
pub fn extract_file(path: &Path) -> Result<TelemetryReport, String> {
    let (stream_indexes, camm_indexes) = telemetry_streams(path);
    let mut unparsed = Vec::new();
    let mut points = PointSink::default();
    let mut sources: Vec<&str> = Vec::new();
    let mut camm_stats: Option<CammStats> = None;

    // Pass 1: dedicated data streams (dashcam NMEA subtitle/data tracks).
    for idx in &stream_indexes {
        let idx = idx.to_string();
        match run_tool(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-i",
                &path.to_string_lossy(),
                "-map",
                &format!("0:{idx}"),
                "-c",
                "copy",
                "-f",
                "data",
                "-",
            ],
        ) {
            Ok(bytes) => {
                let before = points.total;
                scan_nmea_bytes(&bytes, &mut points);
                if points.total > before && !sources.contains(&"nmea-data-stream") {
                    sources.push("nmea-data-stream");
                }
            }
            Err(_) => unparsed.push(format!("stream #{idx} — payload dump failed")),
        }
    }

    // camm tracks decode per the public Camera Motion Metadata spec —
    // implemented and unit-tested, but no real recorder sample has
    // validated the layout, so stats + the note carry that caveat.
    for idx in &camm_indexes {
        match extract_camm_stream(path, *idx, &mut points) {
            Ok(stats) => {
                if stats.gps_points > 0 && !sources.contains(&"camm-gps") {
                    sources.push("camm-gps");
                }
                match &mut camm_stats {
                    Some(agg) => {
                        agg.packets += stats.packets;
                        agg.gps_points += stats.gps_points;
                        agg.gps_no_fix += stats.gps_no_fix;
                        agg.imu_packets += stats.imu_packets;
                        agg.unknown_packets += stats.unknown_packets;
                    }
                    None => camm_stats = Some(stats),
                }
            }
            Err(err) => unparsed.push(format!("stream #{idx} camm — {err}")),
        }
    }

    // Pass 2 (fallback): free-box / in-mdat NMEA strings — some recorders
    // embed sentences without declaring a data stream at all. Only the
    // head and tail are read: a multi-GB evidence file never enters
    // memory whole.
    if points.total == 0 {
        use std::io::{Read, Seek, SeekFrom};
        if let Ok(mut f) = fs::File::open(path) {
            let mut head = vec![0u8; EDGE_SCAN_BYTES];
            let n = f.read(&mut head).unwrap_or(0);
            scan_nmea_bytes(&head[..n], &mut points);
            if points.total == 0 {
                let mut tail = vec![0u8; EDGE_SCAN_BYTES];
                if f.seek(SeekFrom::End(-(EDGE_SCAN_BYTES as i64))).is_ok()
                    && f.read_exact(&mut tail).is_ok()
                {
                    scan_nmea_bytes(&tail, &mut points);
                }
            }
        }
        if points.total > 0 {
            sources.push("nmea-file-scan");
        }
    }
    let total_points = points.total;
    let truncated = points.truncated();
    let mut points = points.points;
    let source = if sources.is_empty() {
        "none".to_string()
    } else {
        sources.join("+")
    };
    let has_camm = camm_stats.is_some();

    // Sort by timestamp where present; keep stream order otherwise.
    points.sort_by(|a, b| {
        a.ts_unix
            .partial_cmp(&b.ts_unix)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let tss: Vec<f64> = points.iter().filter_map(|p| p.ts_unix).collect();
    let max_speed = points
        .iter()
        .filter_map(|p| p.speed_kmh)
        .fold(None, |acc: Option<f64>, s| {
            Some(acc.map_or(s, |m| m.max(s)))
        });

    Ok(TelemetryReport {
        schema_version: 2,
        event: "telemetry-extract",
        extracted_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        selector: String::new(),
        source_path: crate::util::canonicalize_display(path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string()),
        source: source.clone(),
        point_count: points.len(),
        total_points,
        truncated,
        first_ts_unix: tss.first().copied(),
        last_ts_unix: tss.last().copied(),
        max_speed_kmh: max_speed,
        points,
        unparsed_streams: unparsed,
        camm: camm_stats,
        note: build_note(&source, has_camm, truncated.then_some(total_points)),
    })
}

fn build_note(source: &str, has_camm: bool, truncated_total: Option<usize>) -> String {
    let mut parts = Vec::new();
    if let Some(total) = truncated_total {
        parts.push(format!(
            "TRUNCATED: {total} points were decoded but only the first {MAX_POINTS} are kept; the track, first/last timestamps and maximum speed cover the kept points only."
        ));
    }
    if source == "none" {
        parts.push(
            "No NMEA/camm telemetry found. Dashcams that record GPS only in proprietary tracks report them as unparsed streams — absence here does not prove absence in the original."
                .to_string(),
        );
    }
    if source.contains("nmea") {
        parts.push(
            "NMEA-derived track points; timestamps are UTC per the RMC date field.".to_string(),
        );
    }
    if has_camm {
        parts.push(
            "camm GPS points decoded per the public Camera Motion Metadata spec — the layout is unit-tested but NOT yet validated against a real recorder sample; GPS-epoch timestamps ignore leap seconds (~18s). Verify against vendor tools before reporting positions as fact."
                .to_string(),
        );
    }
    parts.push(
        "Verify against recorder vendor tools before reporting positions as fact.".to_string(),
    );
    parts.join(" ")
}

/// Extract + persist `artifacts/telemetry/<sanitized-selector>.json`.
/// Returns the artifact path.
pub fn extract_telemetry(
    case_dir: &Path,
    selector: &str,
) -> Result<(PathBuf, TelemetryReport), String> {
    let source = resolve_video_source(case_dir, selector)?;
    let mut report = extract_file(&source)?;
    report.selector = selector.to_string();
    let dir = case_dir.join("artifacts/telemetry");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(format!("{}.json", sanitize_filename(selector)));
    crate::util::write_text_atomic(
        &out,
        &serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok((out, report))
}

/// Artifact path for a selector, whether or not it exists yet.
pub fn artifact_path(case_dir: &Path, selector: &str) -> PathBuf {
    case_dir
        .join("artifacts/telemetry")
        .join(format!("{}.json", sanitize_filename(selector)))
}

/// Collects every telemetry artifact into the viewer data map
/// (`{ sanitized_id: report }`) — same convention as the deepfake map.
pub fn collect_reports(case_dir: &Path) -> serde_json::Value {
    let dir = case_dir.join("artifacts/telemetry");
    let mut map = serde_json::Map::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return serde_json::Value::Object(map),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if json.get("event").and_then(|e| e.as_str()) != Some("telemetry-extract") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        map.insert(stem, json);
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rmc_sentence() {
        let p = parse_nmea("$GPRMC,123519.00,A,4807.038,N,01131.000,E,22.4,84.4,230394,003.1,W*6A")
            .unwrap();
        assert!((p.lat - 48.1173).abs() < 0.001);
        assert!((p.lon - 11.5167).abs() < 0.001);
        assert!((p.speed_kmh.unwrap() - 41.48).abs() < 0.1);
        // 1994-03-23 12:35:19 UTC
        assert_eq!(p.ts_unix.unwrap() as i64, 764426119);
    }

    #[test]
    fn parses_gga_with_altitude() {
        let p =
            parse_nmea("$GNGGA,025458.00,3723.2475,N,12158.3416,W,1,07,1.0,9.0,M,,M,,").unwrap();
        assert!((p.lat - 37.387458).abs() < 0.0001);
        assert!((p.lon + 121.97236).abs() < 0.0001);
        assert_eq!(p.alt_m, Some(9.0));
    }

    #[test]
    fn rejects_void_and_garbage() {
        assert!(parse_nmea("$GPRMC,,V,,,,,,,,,,N*53").is_none());
        assert!(parse_nmea("not nmea").is_none());
        assert!(parse_nmea("$GPRMC,123519,A,9999.99,N,01131.000,E,,,,").is_none());
    }

    #[test]
    fn scans_sentences_out_of_binary_bytes() {
        let mut bytes = vec![0u8; 64];
        bytes.extend_from_slice(
            b"$GNRMC,120000.00,A,3723.2000,N,12658.0000,E,30.0,0.0,010124,0,E*00\r\n",
        );
        bytes.extend_from_slice(&[0xAB; 32]);
        let mut points = PointSink::default();
        scan_nmea_bytes(&bytes, &mut points);
        assert_eq!(points.points.len(), 1);
        assert!((points.points[0].lat - 37.386666).abs() < 0.001);
    }

    /// Builds a camm case-6 GPS packet exactly as the public Camera Motion
    /// Metadata Spec lays it out (uint16 reserved, uint16 type = 6, then
    /// packed little-endian fields: f64 time_gps_epoch, i32 fix, f64 lat,
    /// f64 lon, f32 alt, f32 h_acc, f32 v_acc, f32 vel_e, f32 vel_n,
    /// f32 vel_up, f32 speed_acc) — 60 bytes.
    #[allow(clippy::too_many_arguments)]
    fn camm_case6(
        gps_epoch: f64,
        fix: i32,
        lat: f64,
        lon: f64,
        alt: f32,
        ve: f32,
        vn: f32,
        vup: f32,
    ) -> Vec<u8> {
        let mut p = vec![0u8, 0, 6, 0];
        p.extend_from_slice(&gps_epoch.to_le_bytes());
        p.extend_from_slice(&fix.to_le_bytes());
        p.extend_from_slice(&lat.to_le_bytes());
        p.extend_from_slice(&lon.to_le_bytes());
        p.extend_from_slice(&alt.to_le_bytes());
        p.extend_from_slice(&5.0f32.to_le_bytes()); // h_acc
        p.extend_from_slice(&5.0f32.to_le_bytes()); // v_acc
        p.extend_from_slice(&ve.to_le_bytes());
        p.extend_from_slice(&vn.to_le_bytes());
        p.extend_from_slice(&vup.to_le_bytes());
        p.extend_from_slice(&1.0f32.to_le_bytes()); // speed_acc
        p
    }

    #[test]
    fn parses_camm_case6_gps_fix() {
        let mut points = PointSink::default();
        let mut stats = CammStats {
            packets: 0,
            gps_points: 0,
            gps_no_fix: 0,
            imu_packets: 0,
            unknown_packets: 0,
        };
        // gps_epoch 1400000000 → unix ≈ 1715964800; 3-4-5 velocity = 5 m/s.
        let packet = camm_case6(1_400_000_000.0, 3, 37.5, 127.0, 25.0, 3.0, 4.0, 0.0);
        assert_eq!(packet.len(), 60);
        parse_camm_packet(&packet, &mut points, &mut stats);
        assert_eq!(stats.gps_points, 1);
        let p = &points.points[0];
        assert!((p.lat - 37.5).abs() < 1e-9);
        assert!((p.lon - 127.0).abs() < 1e-9);
        assert_eq!(p.alt_m, Some(25.0));
        assert!((p.speed_kmh.unwrap() - 18.0).abs() < 0.01);
        assert!((p.ts_unix.unwrap() - (1_400_000_000.0 + 315_964_800.0)).abs() < 1.0);
    }

    /// R5-P11: speed is horizontal (a 3 m/s climb adds nothing), and a
    /// (0, 0) or non-finite position is no track point.
    #[test]
    fn camm_speed_is_horizontal_and_null_island_is_dropped() {
        let mut points = PointSink::default();
        let mut stats = CammStats {
            packets: 0,
            gps_points: 0,
            gps_no_fix: 0,
            imu_packets: 0,
            unknown_packets: 0,
        };
        let climbing = camm_case6(1_400_000_000.0, 3, 37.5, 127.0, 25.0, 3.0, 4.0, 12.0);
        parse_camm_packet(&climbing, &mut points, &mut stats);
        assert_eq!(points.points.len(), 1);
        assert!(
            (points.points[0].speed_kmh.unwrap() - 18.0).abs() < 0.01,
            "{:?}",
            points.points[0].speed_kmh
        );
        for (lat, lon) in [(0.0, 0.0), (f64::NAN, 127.0), (37.5, f64::INFINITY)] {
            parse_camm_packet(
                &camm_case6(1_400_000_000.0, 3, lat, lon, 0.0, 1.0, 1.0, 0.0),
                &mut points,
                &mut stats,
            );
        }
        let mut case5 = vec![0u8, 0, 5, 0];
        case5.extend_from_slice(&0.0f64.to_le_bytes());
        case5.extend_from_slice(&0.0f64.to_le_bytes());
        case5.extend_from_slice(&10.0f64.to_le_bytes());
        parse_camm_packet(&case5, &mut points, &mut stats);
        assert_eq!(points.points.len(), 1, "no (0,0)/non-finite points");
        assert_eq!(stats.unknown_packets, 4);
        assert_eq!(stats.gps_points, 1);
    }

    #[test]
    fn skips_camm_case6_without_fix() {
        let mut points = PointSink::default();
        let mut stats = CammStats {
            packets: 0,
            gps_points: 0,
            gps_no_fix: 0,
            imu_packets: 0,
            unknown_packets: 0,
        };
        let packet = camm_case6(1_400_000_000.0, 0, 37.5, 127.0, 25.0, 0.0, 0.0, 0.0);
        parse_camm_packet(&packet, &mut points, &mut stats);
        assert!(points.points.is_empty());
        assert_eq!(stats.gps_no_fix, 1);
    }

    #[test]
    fn parses_camm_case5_and_counts_imu() {
        let mut points = PointSink::default();
        let mut stats = CammStats {
            packets: 0,
            gps_points: 0,
            gps_no_fix: 0,
            imu_packets: 0,
            unknown_packets: 0,
        };
        let mut case5 = vec![0u8, 0, 5, 0];
        case5.extend_from_slice(&37.123f64.to_le_bytes());
        case5.extend_from_slice(&127.456f64.to_le_bytes());
        case5.extend_from_slice(&88.5f64.to_le_bytes());
        parse_camm_packet(&case5, &mut points, &mut stats);
        assert_eq!(points.points.len(), 1);
        assert!((points.points[0].lat - 37.123).abs() < 1e-9);
        assert_eq!(points.points[0].alt_m, Some(88.5));
        // IMU case (gyro) counts but yields no point.
        let mut gyro = vec![0u8, 0, 2, 0];
        gyro.extend_from_slice(&[0u8; 12]);
        parse_camm_packet(&gyro, &mut points, &mut stats);
        assert_eq!(points.points.len(), 1);
        assert_eq!(stats.imu_packets, 1);
    }

    #[test]
    fn camm_malformed_and_unknown_types_are_counted_not_parsed() {
        let mut points = PointSink::default();
        let mut stats = CammStats {
            packets: 0,
            gps_points: 0,
            gps_no_fix: 0,
            imu_packets: 0,
            unknown_packets: 0,
        };
        parse_camm_packet(&[0u8, 0], &mut points, &mut stats); // too short
        parse_camm_packet(&[0u8, 0, 99, 0, 0, 0, 0, 0], &mut points, &mut stats); // unknown type
        let short6 = vec![0u8, 0, 6, 0, 0, 0]; // truncated case 6
        parse_camm_packet(&short6, &mut points, &mut stats);
        // out-of-range coords in a well-formed case 6 → unknown, no point
        let bad = camm_case6(1.0, 3, 95.0, 200.0, 0.0, 0.0, 0.0, 0.0);
        parse_camm_packet(&bad, &mut points, &mut stats);
        assert!(points.points.is_empty());
        assert_eq!(stats.unknown_packets, 4);
    }

    #[test]
    fn camm_dump_walk_uses_declared_packet_boundaries() {
        // Simulates `ffmpeg -f data` output: three packets concatenated
        // (GPS fix, gyro, no-fix) — the same shape a real camm stream
        // yields once ffprobe packet sizes are applied.
        let p1 = camm_case6(1_400_000_000.0, 3, 37.5, 127.0, 25.0, 3.0, 4.0, 0.0);
        let mut p2 = vec![0u8, 0, 2, 0];
        p2.extend_from_slice(&[0u8; 12]);
        let p3 = camm_case6(1_400_000_001.0, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let mut dump = p1.clone();
        dump.extend_from_slice(&p2);
        dump.extend_from_slice(&p3);
        let sizes = [p1.len(), p2.len(), p3.len()];

        let mut points = PointSink::default();
        let mut stats = CammStats {
            packets: 0,
            gps_points: 0,
            gps_no_fix: 0,
            imu_packets: 0,
            unknown_packets: 0,
        };
        parse_camm_dump(&dump, &sizes, &mut points, &mut stats);
        assert_eq!(stats.packets, 3);
        assert_eq!(stats.gps_points, 1);
        assert_eq!(stats.imu_packets, 1);
        assert_eq!(stats.gps_no_fix, 1);
        assert_eq!(points.points.len(), 1);
        assert!((points.points[0].lat - 37.5).abs() < 1e-9);

        // A truncated dump marks the missing packet unknown and stops.
        let mut points2 = PointSink::default();
        let mut stats2 = CammStats {
            packets: 0,
            gps_points: 0,
            gps_no_fix: 0,
            imu_packets: 0,
            unknown_packets: 0,
        };
        parse_camm_dump(&dump[..p1.len() + 4], &sizes, &mut points2, &mut stats2);
        assert_eq!(stats2.packets, 1);
        assert_eq!(stats2.unknown_packets, 1);
    }

    /// E6: spec case 7 is the magnetometer (`float magnetic_field[3]`) —
    /// never a GPS fix — and case 6 carries `time_gps_epoch` as a double.
    /// The old parser read a 56+-byte case 7 as GPS and decoded a float
    /// epoch, so a spec-conformant case-6 packet produced no point at all.
    #[test]
    fn camm_follows_spec_case_numbers_and_field_widths() {
        let mut points = PointSink::default();
        let mut stats = CammStats {
            packets: 0,
            gps_points: 0,
            gps_no_fix: 0,
            imu_packets: 0,
            unknown_packets: 0,
        };
        // Spec case 6 with a double epoch that a float would round away.
        let epoch = 1_400_000_000.25f64;
        let gps = camm_case6(epoch, 3, 37.5, 127.0, 25.0, 3.0, 4.0, 0.0);
        parse_camm_packet(&gps, &mut points, &mut stats);
        assert_eq!(stats.gps_points, 1);
        let ts = points.points[0].ts_unix.unwrap();
        assert!((ts - (epoch + GPS_EPOCH_OFFSET)).abs() < 1e-6, "{ts}");
        // A case-7 magnetometer sample padded to GPS size is still IMU data.
        let mut magnet = vec![0u8, 0, 7, 0];
        magnet.extend_from_slice(&[0x41u8; 56]);
        parse_camm_packet(&magnet, &mut points, &mut stats);
        assert_eq!(points.points.len(), 1, "magnetometer is not a GPS point");
        assert_eq!(stats.imu_packets, 1);
        // The type is a little-endian uint16: 0x0105 is not case 5.
        let mut wide = vec![0u8, 0, 5, 1];
        wide.extend_from_slice(&[0u8; 24]);
        parse_camm_packet(&wide, &mut points, &mut stats);
        assert_eq!(stats.unknown_packets, 1);
    }

    /// E9: the 20,000-point cap used to cut the track silently. The kept
    /// points stay capped, but every decoded point is counted.
    #[test]
    fn point_cap_is_reported_not_silent() {
        let sentence = b"$GNRMC,120000.00,A,3723.2000,N,12658.0000,E,30.0,0.0,010124,0,E*00\r\n";
        let mut bytes = Vec::with_capacity(sentence.len() * (MAX_POINTS + 5));
        for _ in 0..MAX_POINTS + 5 {
            bytes.extend_from_slice(sentence);
        }
        let mut points = PointSink::default();
        scan_nmea_bytes(&bytes, &mut points);
        assert_eq!(points.points.len(), MAX_POINTS);
        assert_eq!(points.total, MAX_POINTS + 5);
        assert!(points.truncated());
        let note = build_note("nmea-data-stream", false, Some(points.total));
        assert!(note.starts_with("TRUNCATED: 20005 points"), "{note}");
        // The report serializes both fields.
        let report = TelemetryReport {
            schema_version: 2,
            event: "telemetry-extract",
            extracted_unix: 0,
            selector: String::new(),
            source_path: String::new(),
            source: "nmea-data-stream".to_string(),
            point_count: points.points.len(),
            total_points: points.total,
            truncated: points.truncated(),
            points: Vec::new(),
            unparsed_streams: Vec::new(),
            camm: None,
            first_ts_unix: None,
            last_ts_unix: None,
            max_speed_kmh: None,
            note,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["truncated"], true);
        assert_eq!(json["total_points"], 20005);
    }

    #[test]
    fn does_not_panic_on_binary_after_marker() {
        // Regression: a real dashcam MP4 put invalid-UTF-8 bytes right
        // after "$G"; the old str-slicing path panicked on a char
        // boundary. Byte-level scanning must tolerate any payload.
        let mut bytes = b"$G".to_vec();
        bytes.extend_from_slice(&[0xF0, 0x9F, 0x98]); // truncated 4-byte seq
        bytes.extend(std::iter::repeat_n(0x80, 210));
        let mut points = PointSink::default();
        scan_nmea_bytes(&bytes, &mut points);
        assert!(points.points.is_empty());
    }

    /// Minimal MP4 box (`size`,`type`,payload).
    fn mp4_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + payload.len());
        v.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
        v.extend_from_slice(typ);
        v.extend_from_slice(payload);
        v
    }

    /// A synthetically valid MP4 with one `camm` sample-entry data track.
    /// ffmpeg has no camm muxer, so the container is built by hand: a
    /// `meta`-handler track whose stsd entry is `camm` — exactly how the
    /// mov demuxer reports real recorder files (`bin_data`/`camm` tag).
    /// stco is patched after moov is assembled since it is an absolute
    /// file offset into mdat.
    fn synthetic_camm_mp4(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut mdat_payload = Vec::new();
        for p in packets {
            mdat_payload.extend_from_slice(p);
        }

        let mut ftyp_payload = b"isom".to_vec();
        ftyp_payload.extend_from_slice(&512u32.to_be_bytes());
        ftyp_payload.extend_from_slice(b"isomiso2mp41");
        let ftyp = mp4_box(b"ftyp", &ftyp_payload);

        let mut mvhd_p = vec![0, 0, 0, 0];
        mvhd_p.extend_from_slice(&0u32.to_be_bytes()); // creation
        mvhd_p.extend_from_slice(&0u32.to_be_bytes()); // modification
        mvhd_p.extend_from_slice(&1000u32.to_be_bytes()); // timescale
        mvhd_p.extend_from_slice(&120u32.to_be_bytes()); // duration
        mvhd_p.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
        mvhd_p.extend_from_slice(&0x0100u16.to_be_bytes()); // volume
        mvhd_p.extend_from_slice(&[0u8; 10]);
        mvhd_p.extend_from_slice(
            &[0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000]
                .iter()
                .flat_map(|v| v.to_be_bytes())
                .collect::<Vec<u8>>(),
        );
        mvhd_p.extend_from_slice(&[0u8; 24]);
        mvhd_p.extend_from_slice(&3u32.to_be_bytes()); // next_track_id
        let mvhd = mp4_box(b"mvhd", &mvhd_p);

        let mut tkhd_p = vec![0, 0, 0, 7]; // enabled|in_movie|in_preview
        tkhd_p.extend_from_slice(&0u32.to_be_bytes());
        tkhd_p.extend_from_slice(&0u32.to_be_bytes());
        tkhd_p.extend_from_slice(&2u32.to_be_bytes()); // track_id
        tkhd_p.extend_from_slice(&[0u8; 4]);
        tkhd_p.extend_from_slice(&120u32.to_be_bytes()); // duration
        tkhd_p.extend_from_slice(&[0u8; 8]);
        tkhd_p.extend_from_slice(&[0u8; 8]); // layer/alt/volume/reserved
        tkhd_p.extend_from_slice(
            &[0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000]
                .iter()
                .flat_map(|v| v.to_be_bytes())
                .collect::<Vec<u8>>(),
        );
        tkhd_p.extend_from_slice(&0u32.to_be_bytes());
        tkhd_p.extend_from_slice(&0u32.to_be_bytes());
        let tkhd = mp4_box(b"tkhd", &tkhd_p);

        let mut mdhd_p = vec![0, 0, 0, 0];
        mdhd_p.extend_from_slice(&0u32.to_be_bytes());
        mdhd_p.extend_from_slice(&0u32.to_be_bytes());
        mdhd_p.extend_from_slice(&1000u32.to_be_bytes());
        mdhd_p.extend_from_slice(&120u32.to_be_bytes());
        mdhd_p.extend_from_slice(&0x55c4u16.to_be_bytes()); // und
        mdhd_p.extend_from_slice(&0u16.to_be_bytes());
        let mdhd = mp4_box(b"mdhd", &mdhd_p);

        let mut hdlr_p = vec![0, 0, 0, 0];
        hdlr_p.extend_from_slice(&[0u8; 4]);
        hdlr_p.extend_from_slice(b"meta");
        hdlr_p.extend_from_slice(&[0u8; 12]);
        hdlr_p.extend_from_slice(b"camm\x00");
        let hdlr = mp4_box(b"hdlr", &hdlr_p);

        let nmhd = mp4_box(b"nmhd", &[0, 0, 0, 0]);
        let url = mp4_box(b"url ", &[0, 0, 0, 1]);
        let mut dref_p = vec![0, 0, 0, 0];
        dref_p.extend_from_slice(&1u32.to_be_bytes());
        dref_p.extend_from_slice(&url);
        let dinf = mp4_box(b"dinf", &mp4_box(b"dref", &dref_p));

        let mut stsd_entry_p = vec![0u8; 6];
        stsd_entry_p.extend_from_slice(&1u16.to_be_bytes()); // data_ref_idx
        let stsd_entry = mp4_box(b"camm", &stsd_entry_p);
        let mut stsd_p = vec![0, 0, 0, 0];
        stsd_p.extend_from_slice(&1u32.to_be_bytes());
        stsd_p.extend_from_slice(&stsd_entry);
        let stsd = mp4_box(b"stsd", &stsd_p);

        let mut stts_p = vec![0, 0, 0, 0];
        stts_p.extend_from_slice(&1u32.to_be_bytes());
        stts_p.extend_from_slice(&(packets.len() as u32).to_be_bytes());
        stts_p.extend_from_slice(&40u32.to_be_bytes());
        let stts = mp4_box(b"stts", &stts_p);

        let mut stsc_p = vec![0, 0, 0, 0];
        stsc_p.extend_from_slice(&1u32.to_be_bytes());
        stsc_p.extend_from_slice(&1u32.to_be_bytes());
        stsc_p.extend_from_slice(&(packets.len() as u32).to_be_bytes());
        stsc_p.extend_from_slice(&1u32.to_be_bytes());
        let stsc = mp4_box(b"stsc", &stsc_p);

        let mut stsz_p = vec![0, 0, 0, 0];
        stsz_p.extend_from_slice(&0u32.to_be_bytes());
        stsz_p.extend_from_slice(&(packets.len() as u32).to_be_bytes());
        for p in packets {
            stsz_p.extend_from_slice(&(p.len() as u32).to_be_bytes());
        }
        let stsz = mp4_box(b"stsz", &stsz_p);

        let build_moov = |chunk_offset: u32| -> Vec<u8> {
            let mut stco_p = vec![0, 0, 0, 0];
            stco_p.extend_from_slice(&1u32.to_be_bytes());
            stco_p.extend_from_slice(&chunk_offset.to_be_bytes());
            let stco = mp4_box(b"stco", &stco_p);
            let mut stbl_p = Vec::new();
            for b in [&stsd, &stts, &stsc, &stsz, &stco] {
                stbl_p.extend_from_slice(b);
            }
            let mut minf_p = Vec::new();
            for b in [&nmhd, &dinf] {
                minf_p.extend_from_slice(b);
            }
            minf_p.extend_from_slice(&mp4_box(b"stbl", &stbl_p));
            let mut mdia_p = Vec::new();
            for b in [&mdhd, &hdlr] {
                mdia_p.extend_from_slice(b);
            }
            mdia_p.extend_from_slice(&mp4_box(b"minf", &minf_p));
            let mut trak_p = tkhd.clone();
            trak_p.extend_from_slice(&mp4_box(b"mdia", &mdia_p));
            let mut moov_p = mvhd.clone();
            moov_p.extend_from_slice(&mp4_box(b"trak", &trak_p));
            mp4_box(b"moov", &moov_p)
        };

        let moov = build_moov(0);
        let chunk_offset = (ftyp.len() + moov.len() + 8) as u32;
        let moov = build_moov(chunk_offset);
        assert_eq!(ftyp.len() + moov.len() + 8, chunk_offset as usize);

        let mut out = ftyp;
        out.extend_from_slice(&moov);
        out.extend_from_slice(&mp4_box(b"mdat", &mdat_payload));
        out
    }

    /// End-to-end: hand-built MP4 with a `camm` sample-entry track through
    /// the real ffprobe/ffffmpeg path. Still a spec-based synthetic fixture —
    /// this validates the extraction pipeline, not recorder conformance.
    #[test]
    fn camm_stream_extracts_points_end_to_end() {
        if resolve_tool_binary("ffprobe", &["ffprobe"]).is_err()
            || resolve_tool_binary("ffmpeg", &["ffmpeg"]).is_err()
        {
            eprintln!("skipping camm E2E — ffmpeg/ffprobe not installed");
            return;
        }
        let mut p5 = vec![0u8, 0, 5, 0];
        p5.extend_from_slice(&37.2636f64.to_le_bytes());
        p5.extend_from_slice(&127.0286f64.to_le_bytes());
        p5.extend_from_slice(&10.0f64.to_le_bytes());
        let p6 = camm_case6(1_400_000_000.0, 3, 37.2637, 127.0287, 42.0, 13.8, 0.0, 0.0);
        let mut p5b = vec![0u8, 0, 5, 0];
        p5b.extend_from_slice(&37.2638f64.to_le_bytes());
        p5b.extend_from_slice(&127.0288f64.to_le_bytes());
        p5b.extend_from_slice(&11.0f64.to_le_bytes());
        let mp4 = synthetic_camm_mp4(&[p5, p6, p5b]);

        let dir = std::env::temp_dir().join(format!("frametrace-camm-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("synthetic-camm.mp4");
        fs::write(&file, &mp4).unwrap();

        let report = extract_file(&file).expect("extract_file failed on synthetic camm mp4");
        let _ = fs::remove_dir_all(&dir);

        assert!(report.source.contains("camm-gps"), "{}", report.source);
        let camm = report.camm.expect("camm stats missing");
        assert_eq!(camm.packets, 3);
        assert_eq!(camm.gps_points, 3);
        assert_eq!(report.point_count, 3);
        // The case-6 point carries timestamp, altitude, and km/h speed.
        let fixed = report
            .points
            .iter()
            .find(|p| p.ts_unix.is_some())
            .expect("no timestamped camm point");
        assert!((fixed.lat - 37.2637).abs() < 1e-9);
        assert_eq!(fixed.alt_m, Some(42.0));
        assert!((fixed.speed_kmh.unwrap() - 49.68).abs() < 0.01);
    }
}
