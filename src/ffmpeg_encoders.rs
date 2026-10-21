//! Encoder selection for the resolved ffmpeg.
//!
//! FrameTrace bundles no FFmpeg. The build the guided installer fetches
//! (BtbN win64-lgpl) has no libx264 (GPL), and the examiner may use any
//! other build, so proxies and MP4 exports never hard-code an encoder: the
//! resolved ffmpeg is asked once for `-encoders` and the
//! best H.264 encoder it really has is used, in the order
//! libx264 → h264_mf (Windows Media Foundation) → libopenh264 → mpeg4
//! (MPEG-4 Part 2, built into every ffmpeg). h264_mf comes before
//! libopenh264 on purpose: the BtbN build the installer fetches links openh264
//! built from source, and Cisco's patent licence covers only Cisco's own
//! prebuilt openh264 binaries — Media Foundation's encoder is licensed
//! with Windows. (h264_mf exists only on Windows, so elsewhere the order
//! is effectively libx264 → libopenh264 → mpeg4.) Audio prefers the native,
//! LGPL-safe `aac`. The pick is recorded in the audit log lines so an
//! output stays reproducible.
//!
//! Being listed is not enough: `h264_mf` is listed by every Windows build
//! but fails at `MFStartup` on Windows N/KN editions without the Media
//! Feature Pack and on Windows Server without the Media Foundation feature
//! (Server-Media-Foundation) — a build without libx264 would then fail
//! every proxy and export. So each listed candidate, best first, is
//! tried once with a 1-frame encode of a 64×64 black frame using the very
//! options a proxy uses (`-f lavfi -i color=… -frames:v 1 <video args>
//! -f null -`); one that fails is skipped and the next one tried. A trial
//! has its own short bound ([`TRIAL_TIMEOUT_SECS`]); one that times out is
//! tried once more at twice the bound (the first Media Foundation start,
//! or an antivirus scanning its DLLs, can be slow once). A candidate that
//! still times out is skipped for now — the next candidate is used — but
//! only for [`TIMED_OUT_TTL`]: a slow moment never pins a working h264_mf
//! as failed (nor the libopenh264 pick, nor the "Media Feature Pack"
//! hint) for the process lifetime. Only a trial that exits non-zero is a
//! failure kept for good. A trial that cannot be spawned at all proves
//! nothing: no failure is recorded and the list-based pick is used (the
//! pre-trial behaviour). When native `mpeg4` — the last resort, built into
//! every ffmpeg — is listed and exits non-zero too, the harness itself is
//! suspect, not the encoders: again no failure is recorded (no "Media
//! Feature Pack" misdiagnosis).
//!
//! The probe and the trials are cached together per ffmpeg path + mtime +
//! size and run through the bounded
//! [`crate::util::run_with_timeout_detailed`] (console hidden; `-encoders`
//! has its own [`ENCODERS_TIMEOUT_SECS`]). A conclusive result is kept for
//! the binary's lifetime; an inconclusive one (probe failed, trial not
//! spawnable, harness defect) for [`INCONCLUSIVE_TTL`], one with a
//! timed-out trial for [`TIMED_OUT_TTL`], so a wedged ffmpeg does not make every
//! `/api/tools`, doctor, proxy and export wait again. Concurrent callers
//! for the same binary share one probe (single flight). Callers must not
//! hold the server's state lock while calling [`for_binary`].
//!
//! Fix round 5: a timed-out `-encoders` is retried once at twice its bound
//! (antivirus scanning a freshly installed 170 MB exe). When it still
//! gives no listing — whatever the ffmpeg's location (Fix round 6: PATH,
//! a copy in `<exe dir>/tools/bin`, the guided install alike) — every
//! candidate (libx264, h264_mf, libopenh264, mpeg4; aac) is assumed
//! ([`Encoders::assumed_candidates`]) and trial-encoded best first, so a
//! build without libx264 (its trial fails in well under a second) gets the
//! next encoder that really works instead of a libx264 every proxy and
//! export would fail on; the note says the listing could not be read and
//! which encoder the trial chose. The whole probe
//! (`-encoders` and every trial) has one [`PROBE_BUDGET`]: once it is used
//! up no further trial runs — a remaining mpeg4 is used untried, any other
//! remaining candidate counts as timed out — and whenever anything timed
//! out the result is reused for [`TIMED_OUT_TTL`], never the shorter
//! [`INCONCLUSIVE_TTL`].
//!
//! Fix round 7: when the assumed candidates' trials prove nothing (the
//! mpeg4 control exits non-zero — a minimal build without lavfi — or a
//! trial cannot be spawned), mpeg4 is used, not an untried libx264, and
//! the note says so ("시험 인코딩 증거 없음, mpeg4 가정") whenever a trial
//! ran; a listing that failed (not timed out) followed by timed-out trials
//! is named in the note too.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Bound of one trial encode. One 64×64 frame normally takes well under a
/// second; an encoder still busy after this is tried once more at twice
/// the bound, then skipped (for [`TIMED_OUT_TTL`] only).
pub const TRIAL_TIMEOUT_SECS: u64 = 10;

/// Bound of `ffmpeg -encoders` (a listing; no device is opened). A listing
/// that times out is asked once more at twice the bound.
pub const ENCODERS_TIMEOUT_SECS: u64 = 10;

/// Upper bound of one whole probe (`-encoders` with its retry and every
/// trial encode with theirs): GET /api/tools, doctor and proxies wait for
/// it. Once used up, no further trial runs.
pub const PROBE_BUDGET: Duration = Duration::from_secs(45);

/// How long an inconclusive probe result is reused before probing again.
pub const INCONCLUSIVE_TTL: Duration = Duration::from_secs(60);

/// How long a result with a timed-out trial is reused: long enough that a
/// truly wedged encoder (30 s per probe) does not stall a caller every
/// minute, short enough that one slow moment is not permanent.
pub const TIMED_OUT_TTL: Duration = Duration::from_secs(10 * 60);

/// H.264-or-fallback video encoders, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoEncoder {
    Libx264,
    Libopenh264,
    H264Mf,
    Mpeg4,
}

impl VideoEncoder {
    pub const PREFERENCE: [VideoEncoder; 4] = [
        VideoEncoder::Libx264,
        VideoEncoder::H264Mf,
        VideoEncoder::Libopenh264,
        VideoEncoder::Mpeg4,
    ];

    pub fn name(self) -> &'static str {
        match self {
            VideoEncoder::Libx264 => "libx264",
            VideoEncoder::Libopenh264 => "libopenh264",
            VideoEncoder::H264Mf => "h264_mf",
            VideoEncoder::Mpeg4 => "mpeg4",
        }
    }

    pub fn is_h264(self) -> bool {
        self != VideoEncoder::Mpeg4
    }
}

/// What the encode is for: the quality targets differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Review proxy (≤1280 wide, small).
    Proxy,
    /// Delivered MP4 clip (visually lossless-ish).
    Export,
}

/// The encoders one ffmpeg build has (or, when the probe failed, the
/// historical assumption of a full build).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoders {
    names: BTreeSet<String>,
    probed: bool,
    /// Listed video encoders whose 1-frame trial encode failed.
    failed: BTreeSet<String>,
    /// Those of `failed` that only timed out (twice): skipped for now, not
    /// known to be broken.
    timed_out: BTreeSet<String>,
    /// `-encoders` gave no listing (`probed` false): `names` are every
    /// candidate ([`Encoders::assumed_candidates`]), trial-encoded best
    /// first.
    assumed_candidates: bool,
    /// A trial encode worked and chose [`Encoders::video`].
    chosen_by_trial: bool,
    /// `-encoders` timed out (twice) — the reason `probed` is false.
    listing_timed_out: bool,
    /// A trial encode ran to an outcome (exit status or timeout): the
    /// binary at least runs.
    trial_ran: bool,
    /// Assumed candidates whose trials proved nothing (the mpeg4 control
    /// failed, or a trial could not be spawned): mpeg4, built into every
    /// ffmpeg, is used rather than an untried libx264 (Fix round 7).
    no_evidence: bool,
    /// The probe gave up because a trial could not be spawned
    /// ([`Trial::Unknown`]), not because the mpeg4 control failed.
    gave_up_unknown: bool,
    /// The file exists, the listing failed (non-zero exit, empty listing
    /// or spawn failure — not a timeout) and the first trial could not be
    /// spawned (antivirus block, wrong architecture, not executable): h264
    /// falls back to mpeg4 and the note says that ffmpeg cannot be run —
    /// clips, proxies and exports may fail (Fix round 8; wording Fix round
    /// 9). False for a missing file.
    spawn_failed: bool,
}

impl Encoders {
    /// The pre-detection assumption (full build). Used only when the
    /// `-encoders` probe could not run, so behaviour stays as before and
    /// ffmpeg itself reports any missing encoder.
    pub fn assumed_full() -> Self {
        Self {
            names: ["libx264", "aac", "libmp3lame", "mpeg4"]
                .iter()
                .map(|name| name.to_string())
                .collect(),
            probed: false,
            failed: BTreeSet::new(),
            timed_out: BTreeSet::new(),
            assumed_candidates: false,
            chosen_by_trial: false,
            listing_timed_out: false,
            trial_ran: false,
            no_evidence: false,
            gave_up_unknown: false,
            spawn_failed: false,
        }
    }

    /// The assumption when `-encoders` gave no listing, whatever the build:
    /// every video candidate ([`VideoEncoder::PREFERENCE`]) and native aac,
    /// to be trial-encoded best first (a missing encoder fails its trial
    /// at once). No libmp3lame is assumed (AVI gets native mp2).
    pub fn assumed_candidates(listing_timed_out: bool) -> Self {
        let mut names: BTreeSet<String> = VideoEncoder::PREFERENCE
            .iter()
            .map(|encoder| encoder.name().to_string())
            .collect();
        names.insert("aac".to_string());
        Self {
            names,
            probed: false,
            failed: BTreeSet::new(),
            timed_out: BTreeSet::new(),
            assumed_candidates: true,
            chosen_by_trial: false,
            listing_timed_out,
            trial_ran: false,
            no_evidence: false,
            gave_up_unknown: false,
            spawn_failed: false,
        }
    }

    /// Parses `ffmpeg -hide_banner -encoders` stdout.
    pub fn from_listing(text: &str) -> Self {
        let mut lines: Vec<&str> = text.lines().collect();
        // Everything above the `------` rule is the flag legend.
        if let Some(rule) = lines.iter().position(|line| line.trim() == "------") {
            lines.drain(..=rule);
        }
        let names = lines
            .iter()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let flags = parts.next()?;
                let name = parts.next()?;
                let flags_ok = flags.len() == 6
                    && flags.starts_with(['V', 'A', 'S'])
                    && flags.chars().all(|c| c == '.' || c.is_ascii_alphabetic());
                (flags_ok && name != "=").then(|| name.to_string())
            })
            .collect();
        Self {
            names,
            probed: true,
            failed: BTreeSet::new(),
            timed_out: BTreeSet::new(),
            assumed_candidates: false,
            chosen_by_trial: false,
            listing_timed_out: false,
            trial_ran: false,
            no_evidence: false,
            gave_up_unknown: false,
            spawn_failed: false,
        }
    }

    /// Whether `name` is listed but failed its trial encode.
    pub fn trial_failed(&self, name: &str) -> bool {
        self.failed.contains(name)
    }

    /// Records a failed trial encode (the encoder is then skipped).
    pub fn mark_failed(&mut self, encoder: VideoEncoder) {
        self.failed.insert(encoder.name().to_string());
    }

    /// Whether `name` was skipped only because its trial timed out (it is
    /// tried again once the result expires).
    pub fn trial_timed_out(&self, name: &str) -> bool {
        self.timed_out.contains(name)
    }

    /// Records a candidate skipped because its trial timed out (or was not
    /// run because the probe budget was used up): not known to be broken.
    fn mark_timed_out(&mut self, encoder: VideoEncoder) {
        self.mark_failed(encoder);
        self.timed_out.insert(encoder.name().to_string());
    }

    /// Whether the encoders are the assumed candidate set (`-encoders`
    /// gave no listing).
    pub fn candidates_assumed(&self) -> bool {
        self.assumed_candidates
    }

    /// Whether a trial encode worked and chose [`Encoders::video`].
    pub fn chosen_by_trial(&self) -> bool {
        self.chosen_by_trial
    }

    /// Whether `-encoders` timed out (twice).
    pub fn listing_timed_out(&self) -> bool {
        self.listing_timed_out
    }

    /// Forgets every recorded trial failure (nothing was proven).
    fn clear_failures(&mut self) {
        self.failed.clear();
        self.timed_out.clear();
    }

    /// The trials of the assumed candidates proved nothing: blame no
    /// encoder (the timed-out ones stay skipped for now) and use mpeg4 —
    /// with no listing, nothing says libx264 is there (Fix round 7).
    fn assume_mpeg4_without_evidence(&mut self) {
        let timed_out = &self.timed_out;
        self.failed.retain(|name| timed_out.contains(name));
        self.no_evidence = true;
    }

    /// Whether the assumed candidates' trials proved nothing (mpeg4 used).
    pub fn no_trial_evidence(&self) -> bool {
        self.no_evidence
    }

    /// Whether the probe gave up on a trial that could not be spawned.
    pub fn gave_up_on_unknown_trial(&self) -> bool {
        self.gave_up_unknown
    }

    /// Whether the ffmpeg file exists but could not be run at all.
    pub fn spawn_failed(&self) -> bool {
        self.spawn_failed
    }

    pub fn probed(&self) -> bool {
        self.probed
    }

    pub fn has(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// The best available video encoder — listed and not failed its trial
    /// encode; mpeg4 is native to every ffmpeg.
    pub fn video(&self) -> VideoEncoder {
        if self.no_evidence {
            return VideoEncoder::Mpeg4;
        }
        VideoEncoder::PREFERENCE
            .into_iter()
            .find(|encoder| self.has(encoder.name()) && !self.trial_failed(encoder.name()))
            .unwrap_or(VideoEncoder::Mpeg4)
    }

    /// AAC for MP4: the native LGPL-safe `aac` first. A build without it
    /// still gets `aac` so ffmpeg reports the missing encoder loudly.
    pub fn aac(&self) -> &'static str {
        if !self.has("aac") && self.has("libfdk_aac") {
            "libfdk_aac"
        } else {
            "aac"
        }
    }

    /// Audio for AVI: MP3 when libmp3lame is present, else native MP2.
    pub fn avi_audio(&self) -> &'static str {
        if self.has("libmp3lame") {
            "libmp3lame"
        } else {
            "mp2"
        }
    }

    /// The full codec pick for one encode.
    pub fn choice(&self, video: VideoEncoder, audio: &'static str) -> CodecChoice {
        CodecChoice {
            video,
            audio,
            probed: self.probed,
        }
    }

    /// A one-line note when libx264 is not used (doctor, `tools list`,
    /// /api/tools), naming the H.264 encoder used instead and — when
    /// h264_mf is listed but its trial failed — that Windows needs the
    /// Media Feature Pack. Also when `-encoders` gave no listing: that it
    /// timed out (or failed) and which encoder the trial chose.
    pub fn build_note(&self) -> Option<String> {
        let video = self.video();
        if !self.probed {
            return self.assumed_note(video);
        }
        if video == VideoEncoder::Libx264 {
            return None;
        }
        let mut note = match video {
            VideoEncoder::Mpeg4 => {
                "쓸 수 있는 H.264 인코더 없음 — 프록시·MP4 내보내기는 mpeg4(MPEG-4 Part 2) 사용"
                    .to_string()
            }
            other if self.has("libx264") => {
                format!("libx264 실행 실패 — H.264는 {} 사용", other.name())
            }
            other => format!("libx264 없음 — H.264는 {} 사용", other.name()),
        };
        let mf = VideoEncoder::H264Mf.name();
        if self.trial_timed_out(mf) {
            note.push_str(" · h264_mf 시험 인코딩 시간 초과 — 잠시 뒤 다시 확인");
        } else if self.trial_failed(mf) {
            note.push_str(
                " · h264_mf 사용 불가 — Windows N/KN(Media Feature Pack)·Windows Server(Media Foundation 기능 미설치)",
            );
        }
        Some(note)
    }

    /// The note when `-encoders` gave no listing. None for the historical
    /// full-build assumption and for a missing ffmpeg file whose listing
    /// failed and none of whose trials ran (reported elsewhere) — but
    /// whenever a trial ran (also one that only timed out, or a failed
    /// mpeg4 control: Fix round 7) the assumed pick is named, and an
    /// existing file that cannot be run at all says so (Fix round 8). No
    /// Media Feature Pack hint: an assumed h264_mf that fails may simply
    /// not be in the build.
    fn assumed_note(&self, video: VideoEncoder) -> Option<String> {
        if self.spawn_failed {
            return Some(
                "ffmpeg 실행 실패 — 클립·프록시·내보내기가 실패할 수 있음 · 잠시 뒤 다시 확인"
                    .to_string(),
            );
        }
        if !self.assumed_candidates
            || (!self.listing_timed_out
                && !self.chosen_by_trial
                && !self.trial_ran
                && self.timed_out.is_empty())
        {
            return None;
        }
        let reason = if self.listing_timed_out {
            "시간 초과"
        } else {
            "실패"
        };
        let name = match video {
            VideoEncoder::Mpeg4 => "mpeg4(MPEG-4 Part 2)",
            other => other.name(),
        };
        let mut note = if self.chosen_by_trial {
            format!("ffmpeg -encoders 확인 {reason} — 시험 인코딩으로 {name} 선택")
        } else if self.no_evidence {
            format!("ffmpeg -encoders 확인 {reason} — 시험 인코딩 증거 없음, {name} 가정")
        } else {
            format!("ffmpeg -encoders 확인 {reason} — 시험 인코딩 결론 없음, {name} 가정")
        };
        if !self.timed_out.is_empty() {
            note.push_str(" · 일부 시험 인코딩 시간 초과");
        }
        note.push_str(" · 잠시 뒤 다시 확인");
        Some(note)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "probed": self.probed,
            "libx264": self.has("libx264"),
            "h264": self.video().name(),
            "aac": self.aac(),
            "avi_audio": self.avi_audio(),
            "note": self.build_note(),
            // Listed encoders whose 1-frame trial encode failed (skipped).
            "trial_failed": self.failed.iter().collect::<Vec<_>>(),
            // Those of trial_failed that only timed out (tried again later).
            "trial_timed_out": self.timed_out.iter().collect::<Vec<_>>(),
            // `-encoders` gave no listing; every candidate was assumed and
            // trial-encoded (probed is then false; the note names the pick).
            "assumed_candidates": self.assumed_candidates,
            // `-encoders` timed out (twice).
            "listing_timed_out": self.listing_timed_out,
        })
    }
}

/// The encoders actually used for one output (recorded in audit lines).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecChoice {
    pub video: VideoEncoder,
    pub audio: &'static str,
    pub probed: bool,
}

impl CodecChoice {
    /// The historical libx264 + aac pick (also the assumption when the
    /// probe fails).
    pub fn legacy_h264() -> Self {
        Encoders::assumed_full().choice(VideoEncoder::Libx264, "aac")
    }

    /// `-c:v …` options for this encoder (always yuv420p).
    pub fn video_args(&self, purpose: Purpose) -> Vec<String> {
        let proxy = purpose == Purpose::Proxy;
        let args: Vec<&str> = match self.video {
            VideoEncoder::Libx264 => vec![
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                if proxy { "28" } else { "20" },
            ],
            // openh264 has no CRF; a bitrate target sized to the use.
            VideoEncoder::Libopenh264 => {
                vec![
                    "-c:v",
                    "libopenh264",
                    "-b:v",
                    if proxy { "2M" } else { "8M" },
                ]
            }
            // Media Foundation: quality-based rate control (0-100).
            VideoEncoder::H264Mf => vec![
                "-c:v",
                "h264_mf",
                "-rate_control",
                "quality",
                "-quality",
                if proxy { "55" } else { "75" },
            ],
            VideoEncoder::Mpeg4 => vec!["-c:v", "mpeg4", "-q:v", if proxy { "5" } else { "3" }],
        };
        let mut out: Vec<String> = args.into_iter().map(str::to_string).collect();
        out.extend(["-pix_fmt".to_string(), "yuv420p".to_string()]);
        out
    }

    /// `,"video_encoder":…,"audio_encoder":…,"encoder_source":…` for a
    /// hand-built JSON audit line.
    pub fn audit_fields(&self) -> String {
        format!(
            ",\"video_encoder\":\"{}\",\"audio_encoder\":\"{}\",\"encoder_source\":\"{}\"",
            self.video.name(),
            self.audio,
            if self.probed {
                "ffmpeg -encoders"
            } else {
                "assumed (encoder probe failed)"
            }
        )
    }
}

type CacheKey = (String, Option<SystemTime>, u64);

/// One cache entry: a finished result (an inconclusive one expires), or a
/// probe in progress that other callers wait for.
enum Slot {
    Ready {
        encoders: Encoders,
        expires: Option<Instant>,
    },
    Pending(Arc<Flight>),
}

/// A probe in progress (single flight): its result, once published.
#[derive(Default)]
struct Flight {
    result: Mutex<Option<Encoders>>,
    done: Condvar,
}

impl Flight {
    fn publish(&self, encoders: Encoders) {
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(encoders);
        self.done.notify_all();
    }

    fn wait(&self) -> Encoders {
        let mut result = self.result.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(encoders) = result.as_ref() {
                return encoders.clone();
            }
            result = self.done.wait(result).unwrap_or_else(|e| e.into_inner());
        }
    }
}

fn cache() -> &'static Mutex<HashMap<CacheKey, Slot>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Slot>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Publishes the probe's result to the cache and to the waiting callers —
/// also when the probe panicked (then the assumption, uncached), so nobody
/// waits forever.
struct FlightGuard {
    key: CacheKey,
    flight: Arc<Flight>,
    done: bool,
}

impl FlightGuard {
    fn finish(&mut self, encoders: &Encoders, ttl: Option<Duration>) {
        let expires = ttl.map(|ttl| Instant::now() + ttl);
        cache().lock().unwrap_or_else(|e| e.into_inner()).insert(
            self.key.clone(),
            Slot::Ready {
                encoders: encoders.clone(),
                expires,
            },
        );
        self.flight.publish(encoders.clone());
        self.done = true;
    }
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        if matches!(cache.get(&self.key), Some(Slot::Pending(_))) {
            cache.remove(&self.key);
        }
        drop(cache);
        self.flight.publish(Encoders::assumed_full());
    }
}

/// The encoders of the ffmpeg at `binary` (a resolved absolute path).
/// Cached per path + mtime + size (an inconclusive result only for
/// [`INCONCLUSIVE_TTL`], one with a timed-out trial for [`TIMED_OUT_TTL`]);
/// a failed probe yields [`Encoders::assumed_full`].
/// Concurrent callers for the same binary share one probe.
pub fn for_binary(binary: &str) -> Encoders {
    let meta = std::fs::metadata(Path::new(binary)).ok();
    let key: CacheKey = (
        binary.to_string(),
        meta.as_ref().and_then(|m| m.modified().ok()),
        meta.as_ref().map(|m| m.len()).unwrap_or(0),
    );
    let flight = {
        let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        match cache.get(&key) {
            Some(Slot::Ready { encoders, expires }) => {
                let fresh = match expires {
                    Some(expires) => Instant::now() < *expires,
                    None => true,
                };
                if fresh {
                    return encoders.clone();
                }
            }
            Some(Slot::Pending(flight)) => {
                let flight = Arc::clone(flight);
                drop(cache);
                return flight.wait();
            }
            None => {}
        }
        let flight = Arc::new(Flight::default());
        cache.insert(key.clone(), Slot::Pending(Arc::clone(&flight)));
        flight
    };
    // Probe outside the cache lock; other callers wait on the flight only.
    let mut guard = FlightGuard {
        key,
        flight,
        done: false,
    };
    let (encoders, ttl) = probe(binary, meta.is_some());
    guard.finish(&encoders, ttl);
    encoders
}

/// What `ffmpeg -encoders` gave.
enum Listing {
    Listed(Encoders),
    /// Still running after the bound.
    TimedOut,
    /// Not spawnable, non-zero exit, or nothing listed.
    Failed,
}

fn run_listing(binary: &str, secs: u64) -> Listing {
    match crate::util::run_with_timeout_detailed(
        Command::new(binary).args(["-hide_banner", "-encoders"]),
        Some(secs),
    ) {
        Ok(output) if output.status.success() => {
            let encoders = Encoders::from_listing(&String::from_utf8_lossy(&output.stdout));
            if encoders.names.is_empty() {
                Listing::Failed
            } else {
                Listing::Listed(encoders)
            }
        }
        Err(crate::util::RunError::TimedOut(_)) => Listing::TimedOut,
        _ => Listing::Failed,
    }
}

/// `-encoders`, then the trial encodes. The second value is how long the
/// result may be reused (`None`: for the binary's lifetime). `exists`:
/// the file is there (a spawn failure then gets a note).
fn probe(binary: &str, exists: bool) -> (Encoders, Option<Duration>) {
    let started = Instant::now();
    // Whole seconds left of the budget (0: used up).
    let left = || PROBE_BUDGET.saturating_sub(started.elapsed()).as_secs();
    // Anything timed out → the result is reused for TIMED_OUT_TTL (also
    // when it is otherwise inconclusive: never the shorter TTL then).
    let mut timed_out = false;
    let mut listing = run_listing(binary, ENCODERS_TIMEOUT_SECS);
    if matches!(listing, Listing::TimedOut) {
        listing = run_listing(binary, (2 * ENCODERS_TIMEOUT_SECS).min(left().max(1)));
    }
    let mut encoders = match listing {
        Listing::Listed(encoders) => encoders,
        other => {
            let listing_timed_out = matches!(other, Listing::TimedOut);
            timed_out |= listing_timed_out;
            // Whatever the build or its location: assume every candidate
            // and trial them within what is left of the budget (a missing
            // libx264 fails at once), rather than a libx264 that every
            // proxy and export might then fail on.
            Encoders::assumed_candidates(listing_timed_out)
        }
    };
    // Trial-encode the listed candidates, best first, until one works. A
    // timed-out trial is tried once more at twice the bound; a candidate
    // that still times out is skipped, but the result is then reused for
    // TIMED_OUT_TTL only — never pinned for the process lifetime. Every
    // bound is cut to what is left of the budget; once that is used up no
    // trial runs: mpeg4 is used untried, other candidates count as timed
    // out.
    for candidate in VideoEncoder::PREFERENCE {
        if !encoders.has(candidate.name()) {
            continue;
        }
        let mut trial = bounded_trial(binary, candidate, TRIAL_TIMEOUT_SECS, left());
        if trial == Trial::TimedOut {
            trial = bounded_trial(binary, candidate, 2 * TRIAL_TIMEOUT_SECS, left());
        }
        if matches!(trial, Trial::Works | Trial::Fails | Trial::TimedOut) {
            encoders.trial_ran = true;
        }
        match trial {
            Trial::Works => {
                encoders.chosen_by_trial = true;
                break;
            }
            Trial::OverBudget if candidate == VideoEncoder::Mpeg4 => {
                timed_out = true;
                break;
            }
            Trial::Fails if candidate == VideoEncoder::Mpeg4 => {
                // Even the listed built-in encoder exits non-zero: the trial
                // harness is at fault (an odd build, lavfi missing), not
                // the encoders. Keep the list-based pick and blame nothing
                // — with no listing, mpeg4 (Fix round 7).
                give_up(&mut encoders);
                return (encoders, Some(inconclusive_ttl(timed_out)));
            }
            Trial::Fails => encoders.mark_failed(candidate),
            Trial::TimedOut | Trial::OverBudget => {
                encoders.mark_timed_out(candidate);
                timed_out = true;
            }
            Trial::Unknown => {
                // This probe proves nothing: blame nothing (earlier
                // failures are tried again with the rest next time) —
                // with no listing, mpeg4 (Fix round 7). An existing file
                // from which nothing at all could be run is named in the
                // note (Fix round 8).
                give_up(&mut encoders);
                encoders.gave_up_unknown = true;
                encoders.spawn_failed = exists
                    && encoders.assumed_candidates
                    && !encoders.trial_ran
                    && !encoders.listing_timed_out;
                return (encoders, Some(inconclusive_ttl(timed_out)));
            }
        }
    }
    let ttl = if timed_out {
        Some(TIMED_OUT_TTL)
    } else if !encoders.probed {
        // The candidates, assumed (no listing): never kept for good.
        Some(INCONCLUSIVE_TTL)
    } else {
        None
    };
    (encoders, ttl)
}

/// The trials proved nothing. Listed encoders: keep the list-based pick,
/// blame nothing. Assumed candidates (no listing): nothing says libx264
/// (or any H.264 encoder) is there, so mpeg4 — native to every ffmpeg —
/// rather than an untried libx264 that every proxy and export might fail
/// on for the TTL.
fn give_up(encoders: &mut Encoders) {
    if encoders.assumed_candidates {
        encoders.assume_mpeg4_without_evidence();
    } else {
        encoders.clear_failures();
    }
}

/// The reuse time of an inconclusive result: the longer one when anything
/// timed out (a wedged ffmpeg must not stall a caller every minute).
fn inconclusive_ttl(timed_out: bool) -> Duration {
    if timed_out {
        TIMED_OUT_TTL
    } else {
        INCONCLUSIVE_TTL
    }
}

/// One trial bounded by `secs`, cut to the `left` seconds of the budget;
/// none left → not run.
fn bounded_trial(binary: &str, encoder: VideoEncoder, secs: u64, left: u64) -> Trial {
    if left == 0 {
        return Trial::OverBudget;
    }
    trial_encode(binary, encoder, secs.min(left))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trial {
    Works,
    /// Exited non-zero.
    Fails,
    /// Still running after the bound (killed): slow, not proven broken.
    TimedOut,
    /// Could not be spawned: proves nothing.
    Unknown,
    /// Not run: the probe budget is used up.
    OverBudget,
}

/// The argv of one trial encode: one 64×64 black frame through `encoder`
/// with a proxy's exact options, to the null muxer (nothing is written).
pub(crate) fn trial_args(encoder: VideoEncoder) -> Vec<String> {
    let mut args: Vec<String> = [
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=64x64:d=0.04",
        "-frames:v",
        "1",
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect();
    let choice = CodecChoice {
        video: encoder,
        audio: "aac",
        probed: true,
    };
    args.extend(choice.video_args(Purpose::Proxy));
    args.extend(["-an", "-f", "null", "-"].iter().map(|arg| arg.to_string()));
    args
}

fn trial_encode(binary: &str, encoder: VideoEncoder, secs: u64) -> Trial {
    #[cfg(test)]
    if tests::trial_forced_unknown(binary) {
        return Trial::Unknown;
    }
    match crate::util::run_with_timeout_detailed(
        Command::new(binary).args(trial_args(encoder)),
        Some(secs),
    ) {
        Ok(output) if output.status.success() => Trial::Works,
        Ok(_) => Trial::Fails,
        // h264_mf wedged in MFStartup — or only a slow first start.
        Err(crate::util::RunError::TimedOut(_)) => Trial::TimedOut,
        Err(crate::util::RunError::Failed(_)) => Trial::Unknown,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const FULL: &str = "Encoders:\n V..... = Video\n A..... = Audio\n S..... = Subtitle\n .F.... = Frame-level multithreading\n ------\n V....D libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)\n V....D h264_mf              H264 via MediaFoundation (codec h264)\n V.S... mpeg4                MPEG-4 part 2\n A....D aac                  AAC (Advanced Audio Coding)\n A....D libmp3lame           libmp3lame MP3 (MPEG audio layer 3) (codec mp3)\n";
    pub const LGPL: &str = "Encoders:\n V..... = Video\n A..... = Audio\n ------\n V....D h264_mf              H264 via MediaFoundation (codec h264)\n V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)\n V.S... mpeg4                MPEG-4 part 2\n A....D aac                  AAC (Advanced Audio Coding)\n A....D mp2                  MP2 (MPEG audio layer 2)\n";
    /// What the pinned BtbN win64-lgpl build lists (libopenh264 built in
    /// from source, Media Foundation, hardware encoders).
    pub const BTBN_LGPL: &str = "Encoders:\n V..... = Video\n ------\n V....D libopenh264          OpenH264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)\n V....D h264_amf             AMD AMF H.264 Encoder (codec h264)\n V....D h264_mf              H264 via MediaFoundation (codec h264)\n V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)\n V....D h264_qsv             H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (Intel Quick Sync Video acceleration) (codec h264)\n V.S... mpeg4                MPEG-4 part 2\n A....D aac                  AAC (Advanced Audio Coding)\n";
    pub const BARE: &str = "Encoders:\n V..... = Video\n ------\n V.S... mpeg4                MPEG-4 part 2\n A....D mp2                  MP2 (MPEG audio layer 2)\n";

    /// Test hook: once a file `force-unknown` exists next to `binary`,
    /// every trial encode of it is [`Trial::Unknown`] (as if it could not
    /// be spawned). Windows needs it: std runs a `.cmd` through `cmd.exe
    /// /c`, so a deleted `.cmd` still spawns (and exits 1 — `Fails`), and
    /// a script cannot be an `.exe` that CreateProcess would refuse.
    pub(crate) fn trial_forced_unknown(binary: &str) -> bool {
        Path::new(binary)
            .parent()
            .is_some_and(|dir| dir.join("force-unknown").exists())
    }

    /// A fake ffmpeg whose `-encoders` prints `listing` (every trial
    /// encode succeeds).
    pub fn fake_ffmpeg(dir: &Path, listing: &str) -> String {
        fake_ffmpeg_failing(dir, listing, &[])
    }

    /// [`fake_ffmpeg`] whose trial encode exits 1 for the `failing`
    /// encoders (e.g. h264_mf on Windows N without the Media Feature
    /// Pack). Every call is logged to `calls.log` next to it.
    pub fn fake_ffmpeg_failing(dir: &Path, listing: &str, failing: &[&str]) -> String {
        fake_ffmpeg_slow(dir, listing, failing, &[])
    }

    /// [`fake_ffmpeg_failing`] whose trial encode of each `(encoder,
    /// seconds)` in `sleeping` first sleeps that long, then succeeds
    /// (h264_mf wedged in MFStartup, or just slow) — unless a file `awake`
    /// exists next to it (the slow moment has passed).
    pub fn fake_ffmpeg_slow(
        dir: &Path,
        listing: &str,
        failing: &[&str],
        sleeping: &[(&str, u32)],
    ) -> String {
        fake_ffmpeg_full(dir, listing, failing, sleeping, 0)
    }

    /// [`fake_ffmpeg_slow`] whose `-encoders` first sleeps `listing_secs`
    /// (0: not at all) unless `awake` exists — antivirus scanning a fresh
    /// exe.
    pub fn fake_ffmpeg_full(
        dir: &Path,
        listing: &str,
        failing: &[&str],
        sleeping: &[(&str, u32)],
        listing_secs: u32,
    ) -> String {
        std::fs::create_dir_all(dir).unwrap();
        #[cfg(windows)]
        {
            let path = dir.join("ffmpeg.cmd");
            let log = dir.join("calls.log");
            let mut body = String::from("@echo off\r\n");
            body.push_str(&format!(">>\"{}\" echo %*\r\n", log.display()));
            // findstr, not %VAR:x=y% compares: std may quote arguments
            // (`color=…`), and quotes inside an `if` compare break it.
            body.push_str("echo %*| findstr /c:\"-encoders\" >nul || goto trial\r\n");
            if listing_secs > 0 {
                body.push_str(&format!(
                    "if not exist \"{}\" ping -n {} 127.0.0.1 >nul\r\n",
                    dir.join("awake").display(),
                    listing_secs + 1
                ));
            }
            for line in listing.lines() {
                // `echo(` prints the rest verbatim (also blank lines).
                body.push_str(&format!("echo({line}\r\n"));
            }
            body.push_str("exit /b 0\r\n:trial\r\n");
            for name in failing {
                body.push_str(&format!(
                    "echo %*| findstr /c:\"-c:v {name} \" >nul && exit /b 1\r\n"
                ));
            }
            for (name, secs) in sleeping {
                // `timeout` refuses redirected stdin; ping waits ~1 s per echo.
                body.push_str(&format!(
                    "echo %*| findstr /c:\"-c:v {name} \" >nul && if not exist \"{}\" ping -n {} 127.0.0.1 >nul\r\n",
                    dir.join("awake").display(),
                    secs + 1
                ));
            }
            body.push_str("exit /b 0\r\n");
            std::fs::write(&path, body).unwrap();
            path.to_string_lossy().into_owned()
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("ffmpeg");
            let log = dir.join("calls.log");
            // `--warm` (first, never logged) lets the loop below make sure
            // the script can be exec'd before the test runs it: on Linux a
            // parallel test's fork may still hold the just-written file
            // open, and exec then fails with ETXTBSY (os error 26) — the
            // probe would fall back to `assumed_full` and the test would
            // see libx264 (ubuntu CI run 39228894042).
            let listing_sleep = if listing_secs > 0 {
                format!(
                    "[ -e '{}' ] || sleep {listing_secs}; ",
                    dir.join("awake").display()
                )
            } else {
                String::new()
            };
            let mut script = format!(
                "#!/bin/sh\n[ \"$1\" = --warm ] && exit 0\necho \"$*\" >> '{}'\ncase \" $* \" in *\" -encoders \"*) {listing_sleep}cat <<'EOF'\n{listing}EOF\nexit 0;; esac\n",
                log.display()
            );
            for name in failing {
                script.push_str(&format!(
                    "case \" $* \" in *\" -c:v {name} \"*) exit 1;; esac\n"
                ));
            }
            // A killed sh leaves its `sleep` child holding the stdout/stderr
            // pipes until it ends; harmless: on a timeout
            // `run_with_timeout_detailed` kills, reaps and returns without
            // joining its pipe readers (they finish on their own), so the
            // orphan cannot stretch the probe (Fix round 8 check).
            for (name, secs) in sleeping {
                script.push_str(&format!(
                    "case \" $* \" in *\" -c:v {name} \"*) [ -e '{}' ] || sleep {secs};; esac\n",
                    dir.join("awake").display()
                ));
            }
            script.push_str("exit 0\n");
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match Command::new(&path).arg("--warm").output() {
                    Ok(out) if out.status.success() => break,
                    Ok(out) => panic!("fake ffmpeg --warm failed: {out:?}"),
                    Err(e) if e.raw_os_error() == Some(26) && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => panic!("fake ffmpeg --warm: {e}"),
                }
            }
            path.to_string_lossy().into_owned()
        }
    }

    #[test]
    fn parses_listing_without_legend() {
        let enc = Encoders::from_listing(FULL);
        assert!(enc.probed());
        assert!(enc.has("libx264") && enc.has("aac") && enc.has("mpeg4"));
        assert!(!enc.has("="), "legend lines are skipped");
        assert!(!enc.has("Video"));
    }

    #[test]
    fn picks_in_preference_order() {
        assert_eq!(Encoders::from_listing(FULL).video(), VideoEncoder::Libx264);
        assert_eq!(Encoders::from_listing(LGPL).video(), VideoEncoder::H264Mf);
        assert_eq!(Encoders::from_listing(BARE).video(), VideoEncoder::Mpeg4);
        // The pinned BtbN win64-lgpl build has both: Media Foundation wins over
        // the source-built openh264 (not covered by Cisco's patent licence).
        let both =
            Encoders::from_listing(" ------\n V....D libopenh264  OpenH264\n V....D h264_mf  MF\n");
        assert_eq!(both.video(), VideoEncoder::H264Mf);
        // openh264 is still used when it is the only H.264 encoder.
        let openh264_only = Encoders::from_listing(" ------\n V....D libopenh264  OpenH264\n");
        assert_eq!(openh264_only.video(), VideoEncoder::Libopenh264);
        let x264_and_mf =
            Encoders::from_listing(" ------\n V....D h264_mf  MF\n V....D libx264  x264\n");
        assert_eq!(x264_and_mf.video(), VideoEncoder::Libx264);
        assert_eq!(
            VideoEncoder::PREFERENCE.map(VideoEncoder::name),
            ["libx264", "h264_mf", "libopenh264", "mpeg4"]
        );
        assert_eq!(Encoders::from_listing("").video(), VideoEncoder::Mpeg4);
        assert_eq!(Encoders::assumed_full().video(), VideoEncoder::Libx264);
    }

    #[test]
    fn btbn_lgpl_build_never_picks_libopenh264() {
        let enc = Encoders::from_listing(BTBN_LGPL);
        assert!(enc.has("libopenh264") && enc.has("h264_mf"));
        assert_eq!(enc.video(), VideoEncoder::H264Mf);
        assert_eq!(
            enc.build_note().as_deref(),
            Some("libx264 없음 — H.264는 h264_mf 사용")
        );
        assert_eq!(enc.to_json()["h264"], "h264_mf");
    }

    #[test]
    fn audio_picks() {
        assert_eq!(Encoders::from_listing(LGPL).aac(), "aac");
        assert_eq!(Encoders::from_listing(LGPL).avi_audio(), "mp2");
        assert_eq!(Encoders::from_listing(FULL).avi_audio(), "libmp3lame");
        let fdk_only = Encoders::from_listing(" ------\n A....D libfdk_aac  Fraunhofer\n");
        assert_eq!(fdk_only.aac(), "libfdk_aac");
        let both = Encoders::from_listing(" ------\n A....D aac  AAC\n A....D libfdk_aac  F\n");
        assert_eq!(both.aac(), "aac", "native aac is preferred");
    }

    #[test]
    fn per_encoder_options() {
        let mf = Encoders::from_listing(LGPL).choice(VideoEncoder::H264Mf, "aac");
        let args = mf.video_args(Purpose::Export);
        assert_eq!(
            args,
            [
                "-c:v",
                "h264_mf",
                "-rate_control",
                "quality",
                "-quality",
                "75",
                "-pix_fmt",
                "yuv420p"
            ]
        );
        assert!(!args.contains(&"-crf".to_string()));
        let oh = Encoders::assumed_full().choice(VideoEncoder::Libopenh264, "aac");
        assert!(
            oh.video_args(Purpose::Proxy)
                .windows(2)
                .any(|w| w == ["-b:v", "2M"])
        );
        let x = CodecChoice::legacy_h264().video_args(Purpose::Proxy);
        assert_eq!(
            x,
            [
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "28", "-pix_fmt", "yuv420p"
            ]
        );
        assert!(mf.audit_fields().contains("\"video_encoder\":\"h264_mf\""));
        assert!(
            mf.audit_fields()
                .contains("\"encoder_source\":\"ffmpeg -encoders\"")
        );
    }

    #[test]
    fn build_notes() {
        assert_eq!(Encoders::from_listing(FULL).build_note(), None);
        assert_eq!(Encoders::assumed_full().build_note(), None);
        assert_eq!(
            Encoders::from_listing(LGPL).build_note().as_deref(),
            Some("libx264 없음 — H.264는 h264_mf 사용")
        );
        assert!(
            Encoders::from_listing(BARE)
                .build_note()
                .unwrap()
                .contains("mpeg4")
        );
    }

    /// B-5 (Fix round 2): h264_mf is listed but cannot start (Windows N/KN
    /// without the Media Feature Pack, Windows Server without Media
    /// Foundation) → the next listed candidate is used, the note says why,
    /// and the result is cached with the probe (no second trial).
    #[test]
    fn a_listed_encoder_that_fails_its_trial_is_skipped() {
        let dir = std::env::temp_dir().join(format!("ft-enc-trial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_failing(&dir.join("mf-broken"), BTBN_LGPL, &["h264_mf"]);
        let enc = for_binary(&fake);
        assert!(enc.probed(), "{enc:?}");
        assert!(enc.has("h264_mf") && enc.trial_failed("h264_mf"));
        assert_eq!(enc.video(), VideoEncoder::Libopenh264);
        let note = enc.build_note().unwrap();
        assert!(note.contains("libopenh264 사용"), "{note}");
        assert!(
            note.contains(
                "h264_mf 사용 불가 — Windows N/KN(Media Feature Pack)·Windows Server(Media Foundation 기능 미설치)"
            ),
            "{note}"
        );
        assert_eq!(
            enc.to_json()["trial_failed"],
            serde_json::json!(["h264_mf"])
        );
        let log = dir.join("mf-broken").join("calls.log");
        let calls = std::fs::read_to_string(&log).unwrap();
        // -encoders, the failed h264_mf trial, the libopenh264 trial.
        assert_eq!(calls.lines().count(), 3, "{calls}");
        assert!(calls.lines().nth(1).unwrap().contains("h264_mf"), "{calls}");
        assert!(calls.contains("color=c=black:s=64x64:d=0.04"), "{calls}");
        assert!(calls.contains("-frames:v 1"), "{calls}");
        // Cached with the probe: no further run.
        assert_eq!(for_binary(&fake), enc);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), calls);
        // Fix round 4: a genuine non-zero exit is kept for good (no expiry).
        assert_eq!(expiry_of(&fake), Some(None), "permanent");
        expire(&fake);
        assert_eq!(for_binary(&fake), enc);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), calls);

        // Every H.264 candidate fails → mpeg4, with the MF hint.
        let fake = fake_ffmpeg_failing(&dir.join("none"), LGPL, &["h264_mf", "h264_nvenc"]);
        let enc = for_binary(&fake);
        assert_eq!(enc.video(), VideoEncoder::Mpeg4);
        assert!(enc.build_note().unwrap().contains("Media Feature Pack"));

        // A working h264_mf is tried once and used; later candidates are
        // never trialled.
        let fake = fake_ffmpeg_failing(&dir.join("mf-ok"), BTBN_LGPL, &[]);
        let enc = for_binary(&fake);
        assert_eq!(enc.video(), VideoEncoder::H264Mf);
        assert!(!enc.trial_failed("h264_mf"));
        let calls = std::fs::read_to_string(dir.join("mf-ok").join("calls.log")).unwrap();
        assert_eq!(calls.lines().count(), 2, "{calls}");
        assert_eq!(
            enc.build_note().as_deref(),
            Some("libx264 없음 — H.264는 h264_mf 사용")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn calls_of(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default()
    }

    /// Lets the cached result of `binary` expire now — only one cached with
    /// a TTL (a permanent one stays).
    fn expire(binary: &str) {
        for (key, slot) in cache().lock().unwrap().iter_mut() {
            if let (true, Slot::Ready { expires, .. }) = (key.0 == binary, slot)
                && expires.is_some()
            {
                *expires = Some(Instant::now());
            }
        }
    }

    /// The cached result's expiry: `None` = not cached, `Some(None)` =
    /// kept for good, `Some(Some(t))` = reused until `t`.
    fn expiry_of(binary: &str) -> Option<Option<Instant>> {
        cache()
            .lock()
            .unwrap()
            .iter()
            .find_map(|(key, slot)| match slot {
                Slot::Ready { expires, .. } if key.0 == binary => Some(*expires),
                _ => None,
            })
    }

    /// Fix round 4: a trial that hangs (h264_mf wedged in MFStartup — or a
    /// loaded moment: first MF start, antivirus scanning MFT DLLs) is tried
    /// once more at twice the bound, then skipped — but only for
    /// TIMED_OUT_TTL, never for the process lifetime; no "Media Feature
    /// Pack" hint. Once the TTL is over the probe runs again, and a working
    /// h264_mf is then picked and kept for good.
    #[test]
    fn a_timed_out_trial_is_retried_once_and_not_cached_for_good() {
        let dir = std::env::temp_dir().join(format!("ft-enc-hung-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_slow(&dir, BTBN_LGPL, &[], &[("h264_mf", 35)]);
        let started = Instant::now();
        let enc = for_binary(&fake);
        let took = started.elapsed();
        assert!(
            took < Duration::from_secs(3 * TRIAL_TIMEOUT_SECS + 8),
            "the hung trial must stop at the bound + one retry at twice it: {took:?}"
        );
        assert!(
            enc.trial_failed("h264_mf") && enc.trial_timed_out("h264_mf"),
            "{enc:?}"
        );
        assert_eq!(enc.video(), VideoEncoder::Libopenh264);
        assert_eq!(
            enc.to_json()["trial_timed_out"],
            serde_json::json!(["h264_mf"])
        );
        let note = enc.build_note().unwrap();
        assert!(
            note.contains("시간 초과") && !note.contains("Media Feature Pack"),
            "{note}"
        );
        let calls = calls_of(&dir);
        // -encoders, the h264_mf trial, its retry, the libopenh264 trial.
        assert_eq!(calls.lines().count(), 4, "{calls}");
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains("-c:v h264_mf"))
                .count(),
            2,
            "{calls}"
        );
        // Not permanent: reused only for TIMED_OUT_TTL.
        let expires = expiry_of(&fake).expect("cached").expect("with a TTL");
        assert!(expires > Instant::now() + TIMED_OUT_TTL - Duration::from_secs(60));
        let again = Instant::now();
        assert_eq!(for_binary(&fake), enc);
        assert!(
            again.elapsed() < Duration::from_secs(2),
            "cached within the TTL"
        );
        assert_eq!(calls_of(&dir), calls, "no second probe within the TTL");
        // The slow moment has passed: after the TTL h264_mf is tried again,
        // works, and that result is kept for good.
        std::fs::write(dir.join("awake"), b"").unwrap();
        expire(&fake);
        let enc = for_binary(&fake);
        assert_eq!(enc.video(), VideoEncoder::H264Mf, "{enc:?}");
        assert!(!enc.trial_failed("h264_mf") && !enc.trial_timed_out("h264_mf"));
        assert_eq!(calls_of(&dir).lines().count(), 6, "{}", calls_of(&dir));
        assert_eq!(expiry_of(&fake), Some(None), "a working pick is permanent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 4: a slow first trial (longer than the bound, shorter than
    /// twice it) succeeds on the retry — h264_mf is kept, for good.
    #[test]
    fn a_slow_first_trial_succeeds_on_the_retry() {
        let dir = std::env::temp_dir().join(format!("ft-enc-slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_slow(&dir, BTBN_LGPL, &[], &[("h264_mf", 13)]);
        let enc = for_binary(&fake);
        assert_eq!(enc.video(), VideoEncoder::H264Mf, "{enc:?}");
        assert!(!enc.trial_failed("h264_mf"));
        let calls = calls_of(&dir);
        // -encoders, the timed-out h264_mf trial, the successful retry.
        assert_eq!(calls.lines().count(), 3, "{calls}");
        assert!(!calls.contains("libopenh264"), "{calls}");
        assert_eq!(expiry_of(&fake), Some(None), "permanent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 4: a listed mpeg4 that only times out is not a harness
    /// defect — the genuine failures before it are kept (inconclusive TTL).
    #[test]
    fn a_timed_out_mpeg4_is_not_a_harness_defect() {
        let dir = std::env::temp_dir().join(format!("ft-enc-mpeg4slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_slow(
            &dir,
            " ------\n V....D h264_mf  MF\n V.S... mpeg4  MPEG-4 part 2\n",
            &["h264_mf"],
            &[("mpeg4", 35)],
        );
        let enc = for_binary(&fake);
        assert!(
            enc.trial_failed("h264_mf") && !enc.trial_timed_out("h264_mf"),
            "{enc:?}"
        );
        assert!(enc.trial_timed_out("mpeg4"), "{enc:?}");
        assert_eq!(enc.video(), VideoEncoder::Mpeg4);
        assert!(
            enc.build_note().unwrap().contains("Media Feature Pack"),
            "{:?}",
            enc.build_note()
        );
        assert!(matches!(expiry_of(&fake), Some(Some(_))), "not permanent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn count_calls(calls: &str, needle: &str) -> usize {
        calls.lines().filter(|line| line.contains(needle)).count()
    }

    /// The cached result's TTL is TIMED_OUT_TTL (not permanent, not the
    /// short inconclusive one).
    fn assert_timed_out_ttl(binary: &str) {
        let expires = expiry_of(binary).expect("cached").expect("with a TTL");
        assert!(
            expires > Instant::now() + TIMED_OUT_TTL - Duration::from_secs(120),
            "TIMED_OUT_TTL, not INCONCLUSIVE_TTL"
        );
    }

    /// Fix round 5 (M3): a slow `-encoders` (antivirus scanning the fresh
    /// exe) is asked once more at twice the bound; the listing then counts.
    #[test]
    fn a_timed_out_listing_is_retried_once() {
        let dir = std::env::temp_dir().join(format!("ft-enc-list-slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_full(&dir, BTBN_LGPL, &[], &[], 13);
        let enc = for_binary(&fake);
        assert!(enc.probed() && !enc.listing_timed_out(), "{enc:?}");
        assert_eq!(enc.video(), VideoEncoder::H264Mf);
        let calls = calls_of(&dir);
        // -encoders (timed out), -encoders (retry), the h264_mf trial.
        assert_eq!(count_calls(&calls, "-encoders"), 2, "{calls}");
        assert_eq!(calls.lines().count(), 3, "{calls}");
        assert_eq!(expiry_of(&fake), Some(None), "permanent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 6 (M): an ffmpeg anywhere (PATH here — no location rule)
    /// whose `-encoders` still times out on the retry, and which has no
    /// libx264 but a working h264_mf: every candidate is assumed and
    /// trialled — libx264 fails at once, h264_mf is chosen — with a note
    /// naming the timeout and the pick; reused for TIMED_OUT_TTL.
    #[test]
    fn a_timed_out_listing_trials_the_candidates_and_skips_a_missing_libx264() {
        let dir = std::env::temp_dir().join(format!("ft-enc-list-nox264-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_full(&dir, BTBN_LGPL, &["libx264"], &[], 35);
        let started = Instant::now();
        let enc = for_binary(&fake);
        let took = started.elapsed();
        assert!(took < PROBE_BUDGET + Duration::from_secs(8), "{took:?}");
        assert!(!enc.probed() && enc.candidates_assumed() && enc.listing_timed_out());
        assert!(enc.chosen_by_trial(), "{enc:?}");
        assert!(enc.trial_failed("libx264") && !enc.trial_timed_out("libx264"));
        assert_eq!(enc.video(), VideoEncoder::H264Mf, "{enc:?}");
        assert_eq!(enc.aac(), "aac");
        assert_eq!(enc.avi_audio(), "mp2");
        let note = enc.build_note().expect("a note");
        assert_eq!(
            note,
            "ffmpeg -encoders 확인 시간 초과 — 시험 인코딩으로 h264_mf 선택 · 잠시 뒤 다시 확인"
        );
        assert!(!note.contains("Media Feature Pack") && !note.contains("고정 빌드"));
        let json = enc.to_json();
        assert_eq!(json["probed"], false);
        assert_eq!(json["assumed_candidates"], true);
        assert_eq!(json["listing_timed_out"], true);
        assert_eq!(json["h264"], "h264_mf");
        assert_eq!(
            json["libx264"], true,
            "assumed (listed) — trial_failed says why"
        );
        assert_eq!(json["trial_failed"], serde_json::json!(["libx264"]));
        assert!(json.get("assumed_pinned_build").is_none(), "{json}");
        let calls = calls_of(&dir);
        // -encoders, its retry, the failed libx264 trial, the h264_mf trial.
        assert_eq!(count_calls(&calls, "-encoders"), 2, "{calls}");
        assert_eq!(count_calls(&calls, "-c:v libx264"), 1, "{calls}");
        assert_eq!(count_calls(&calls, "-c:v h264_mf"), 1, "{calls}");
        assert_eq!(calls.lines().count(), 4, "{calls}");
        assert_timed_out_ttl(&fake);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 6 (M): the same timed-out listing on a build that has
    /// libx264: its trial works and libx264 is chosen (no other trial); the
    /// note still says the listing timed out. A missing ffmpeg is not a
    /// timeout: its trial cannot be spawned — no note, the short TTL.
    #[test]
    fn a_timed_out_listing_chooses_a_working_libx264_by_trial() {
        let dir = std::env::temp_dir().join(format!("ft-enc-list-x264-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_full(&dir, FULL, &[], &[], 35);
        let enc = for_binary(&fake);
        assert!(!enc.probed() && enc.candidates_assumed() && enc.listing_timed_out());
        assert!(enc.chosen_by_trial() && enc.to_json()["trial_failed"] == serde_json::json!([]));
        assert_eq!(enc.video(), VideoEncoder::Libx264);
        assert_eq!(
            enc.build_note().as_deref(),
            Some(
                "ffmpeg -encoders 확인 시간 초과 — 시험 인코딩으로 libx264 선택 · 잠시 뒤 다시 확인"
            )
        );
        let calls = calls_of(&dir);
        assert_eq!(count_calls(&calls, "-encoders"), 2, "{calls}");
        assert_eq!(calls.lines().count(), 3, "one trial: {calls}");
        assert!(
            calls.lines().last().unwrap().contains("-c:v libx264"),
            "{calls}"
        );
        assert_timed_out_ttl(&fake);
        // A missing ffmpeg is not a timeout: no note, short TTL.
        let missing = dir
            .join("nope")
            .join("ffmpeg")
            .to_string_lossy()
            .into_owned();
        let enc = for_binary(&missing);
        assert!(!enc.probed() && !enc.listing_timed_out() && !enc.chosen_by_trial());
        // Fix round 7 (L2): nothing proven, nothing blamed — and no
        // untried libx264: mpeg4.
        assert!(enc.no_trial_evidence(), "{enc:?}");
        assert_eq!(enc.video(), VideoEncoder::Mpeg4, "{enc:?}");
        assert_eq!(enc.to_json()["trial_failed"], serde_json::json!([]));
        assert_eq!(enc.build_note(), None);
        let expires = expiry_of(&missing).expect("cached").expect("with a TTL");
        assert!(expires < Instant::now() + INCONCLUSIVE_TTL + Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 5 (L1): the whole probe has one budget. libx264 and
    /// h264_mf hang: libx264 uses 10 + 20 s, h264_mf's bounds are cut to
    /// what is left, then no trial runs — libopenh264 counts as timed out
    /// untried, mpeg4 is used untried; reused for TIMED_OUT_TTL.
    #[test]
    fn the_probe_budget_stops_trialling() {
        let dir = std::env::temp_dir().join(format!("ft-enc-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_slow(
            &dir,
            " ------\n V....D libx264  x264\n V....D h264_mf  MF\n V....D libopenh264  OH\n V.S... mpeg4  M\n A....D aac  AAC\n",
            &[],
            &[("libx264", 60), ("h264_mf", 60)],
        );
        let started = Instant::now();
        let enc = for_binary(&fake);
        let took = started.elapsed();
        assert!(
            took < PROBE_BUDGET + Duration::from_secs(8),
            "bounded by the budget: {took:?}"
        );
        assert_eq!(enc.video(), VideoEncoder::Mpeg4, "{enc:?}");
        for name in ["libx264", "h264_mf", "libopenh264"] {
            assert!(enc.trial_timed_out(name), "{name}: {enc:?}");
        }
        assert!(!enc.trial_failed("mpeg4"));
        let calls = calls_of(&dir);
        assert_eq!(count_calls(&calls, "-c:v libx264"), 2, "{calls}");
        assert!(count_calls(&calls, "-c:v h264_mf") >= 1, "{calls}");
        assert_eq!(count_calls(&calls, "-c:v libopenh264"), 0, "{calls}");
        assert_eq!(count_calls(&calls, "-c:v mpeg4"), 0, "{calls}");
        let note = enc.build_note().unwrap();
        assert!(
            note.contains("시간 초과") && !note.contains("Media Feature Pack"),
            "{note}"
        );
        assert_timed_out_ttl(&fake);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 5 (L1): h264_mf timed out, then the listed mpeg4 exits
    /// non-zero (harness defect: nothing blamed) — the result is still
    /// reused for TIMED_OUT_TTL, not the 60 s inconclusive TTL.
    #[test]
    fn a_harness_defect_after_a_timeout_keeps_the_longer_ttl() {
        let dir = std::env::temp_dir().join(format!("ft-enc-defect-slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_slow(
            &dir,
            " ------\n V....D h264_mf  MF\n V.S... mpeg4  M\n",
            &["mpeg4"],
            &[("h264_mf", 35)],
        );
        let enc = for_binary(&fake);
        assert_eq!(enc.to_json()["trial_failed"], serde_json::json!([]));
        assert_eq!(enc.video(), VideoEncoder::H264Mf, "list-based: {enc:?}");
        let calls = calls_of(&dir);
        assert_eq!(count_calls(&calls, "-c:v h264_mf"), 2, "{calls}");
        assert_eq!(count_calls(&calls, "-c:v mpeg4"), 1, "{calls}");
        assert_timed_out_ttl(&fake);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 7 (L1): the listing failed (not timed out), then every
    /// trial timed out or the budget ran out: mpeg4 is used untried for
    /// TIMED_OUT_TTL — and the note says so (it was None before).
    #[test]
    fn a_failed_listing_with_timed_out_trials_has_a_note() {
        let dir = std::env::temp_dir().join(format!("ft-enc-fail-slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_slow(&dir, "", &[], &[("libx264", 60), ("h264_mf", 60)]);
        let started = Instant::now();
        let enc = for_binary(&fake);
        assert!(started.elapsed() < PROBE_BUDGET + Duration::from_secs(8));
        assert!(!enc.probed() && enc.candidates_assumed() && !enc.listing_timed_out());
        assert!(
            !enc.chosen_by_trial() && !enc.no_trial_evidence(),
            "{enc:?}"
        );
        assert_eq!(enc.video(), VideoEncoder::Mpeg4, "{enc:?}");
        assert!(enc.trial_timed_out("libx264"), "{enc:?}");
        assert_eq!(
            enc.build_note().as_deref(),
            Some(
                "ffmpeg -encoders 확인 실패 — 시험 인코딩 결론 없음, mpeg4(MPEG-4 Part 2) 가정 · 일부 시험 인코딩 시간 초과 · 잠시 뒤 다시 확인"
            )
        );
        assert_eq!(count_calls(&calls_of(&dir), "-c:v mpeg4"), 0);
        assert_timed_out_ttl(&fake);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same note logic without running anything: assumed candidates
    /// after a failed listing, every candidate timed out.
    #[test]
    fn the_assumed_note_names_timed_out_trials_after_a_failed_listing() {
        let mut enc = Encoders::assumed_candidates(false);
        assert_eq!(enc.build_note(), None, "nothing ran: a missing ffmpeg");
        for encoder in VideoEncoder::PREFERENCE {
            enc.mark_timed_out(encoder);
        }
        assert_eq!(enc.video(), VideoEncoder::Mpeg4);
        assert!(
            enc.build_note()
                .unwrap()
                .contains("일부 시험 인코딩 시간 초과")
        );
    }

    /// Fix round 7 (L2): no listing, and even the mpeg4 control exits
    /// non-zero (a minimal build without lavfi: every trial fails). Nothing
    /// is blamed — and mpeg4 is used, not an untried libx264; the note
    /// says there is no evidence. Reused for INCONCLUSIVE_TTL.
    #[test]
    fn a_failed_listing_and_a_failing_mpeg4_control_assume_mpeg4() {
        let dir = std::env::temp_dir().join(format!("ft-enc-fail-nolavfi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_failing(&dir, "", &["libx264", "h264_mf", "libopenh264", "mpeg4"]);
        let enc = for_binary(&fake);
        assert!(!enc.probed() && enc.candidates_assumed() && enc.no_trial_evidence());
        assert!(
            !enc.gave_up_on_unknown_trial(),
            "the mpeg4-control path: {enc:?}"
        );
        assert_eq!(enc.video(), VideoEncoder::Mpeg4, "{enc:?}");
        let json = enc.to_json();
        assert_eq!(json["h264"], "mpeg4");
        assert_eq!(
            json["trial_failed"],
            serde_json::json!([]),
            "nothing blamed"
        );
        assert_eq!(
            enc.build_note().as_deref(),
            Some(
                "ffmpeg -encoders 확인 실패 — 시험 인코딩 증거 없음, mpeg4(MPEG-4 Part 2) 가정 · 잠시 뒤 다시 확인"
            )
        );
        let calls = calls_of(&dir);
        // -encoders and all four trials (the mpeg4 control last).
        assert_eq!(calls.lines().count(), 5, "{calls}");
        assert!(calls.lines().last().unwrap().contains("-c:v mpeg4"));
        let expires = expiry_of(&fake).expect("cached").expect("with a TTL");
        assert!(expires < Instant::now() + INCONCLUSIVE_TTL + Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 7 (L2): no listing, the libx264 trial runs (and fails),
    /// then the next trial cannot be spawned: `Unknown`, nothing proven —
    /// mpeg4, not the untried libx264, with a note (a trial did run).
    /// Unix: the fake deletes itself, so the next exec fails (ENOENT).
    /// Windows (Fix round 8): a deleted `.cmd` would still spawn through
    /// `cmd.exe /c` and exit 1 (`Fails`, the mpeg4-control path), so the
    /// fake instead creates `force-unknown` ([`trial_forced_unknown`]).
    /// `gave_up_on_unknown_trial` pins which path was taken.
    #[test]
    fn a_failed_listing_and_an_unknown_trial_assume_mpeg4() {
        let dir = std::env::temp_dir().join(format!("ft-enc-fail-unknown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("calls.log");
        #[cfg(windows)]
        let fake = {
            let path = dir.join("ffmpeg.cmd");
            let body = format!(
                "@echo off\r\n>>\"{}\" echo %*\r\necho %*| findstr /c:\"-encoders\" >nul && exit /b 1\r\ntype nul > \"{}\"\r\nexit /b 1\r\n",
                log.display(),
                dir.join("force-unknown").display()
            );
            std::fs::write(&path, body).unwrap();
            path.to_string_lossy().into_owned()
        };
        #[cfg(not(windows))]
        let fake = {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("ffmpeg");
            let script = format!(
                "#!/bin/sh\n[ \"$1\" = --warm ] && exit 0\necho \"$*\" >> '{}'\ncase \" $* \" in *\" -encoders \"*) exit 1;; esac\nrm -f \"$0\"\nexit 1\n",
                log.display()
            );
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            crate::test_support::warm_unix_script(&path);
            path.to_string_lossy().into_owned()
        };
        let enc = for_binary(&fake);
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        // -encoders (failed) and the libx264 trial; then nothing spawns.
        assert_eq!(calls.lines().count(), 2, "{calls}");
        assert!(calls.lines().last().unwrap().contains("-c:v libx264"));
        #[cfg(not(windows))]
        assert!(!Path::new(&fake).exists());
        #[cfg(windows)]
        assert!(dir.join("force-unknown").exists());
        assert!(!enc.probed() && enc.candidates_assumed() && enc.no_trial_evidence());
        assert!(enc.gave_up_on_unknown_trial(), "the Unknown path: {enc:?}");
        assert!(!enc.spawn_failed(), "a trial ran: {enc:?}");
        assert_eq!(enc.video(), VideoEncoder::Mpeg4, "{enc:?}");
        assert_eq!(enc.to_json()["trial_failed"], serde_json::json!([]));
        assert_eq!(
            enc.build_note().as_deref(),
            Some(
                "ffmpeg -encoders 확인 실패 — 시험 인코딩 증거 없음, mpeg4(MPEG-4 Part 2) 가정 · 잠시 뒤 다시 확인"
            )
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file at the ffmpeg path that exists but cannot be spawned:
    /// Windows a 0-byte `.exe` (CreateProcess: not a valid application),
    /// unix mode 0644 (EACCES).
    pub fn unrunnable_ffmpeg(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        #[cfg(windows)]
        {
            let path = dir.join("ffmpeg.exe");
            std::fs::write(&path, b"").unwrap();
            path.to_string_lossy().into_owned()
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("ffmpeg");
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            path.to_string_lossy().into_owned()
        }
    }

    /// Fix round 8 (L3): an ffmpeg that exists but cannot be run (AV
    /// block, wrong architecture, not executable): no listing, no trial
    /// ran, nothing timed out — mpeg4 is assumed, and the note says the
    /// ffmpeg cannot be run (it was None: doctor said ok and proxies or
    /// exports went mpeg4 silently). A missing file keeps no note.
    #[test]
    fn an_existing_ffmpeg_that_cannot_run_has_a_note() {
        let dir = std::env::temp_dir().join(format!("ft-enc-unrunnable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = unrunnable_ffmpeg(&dir);
        let enc = for_binary(&fake);
        assert!(!enc.probed() && enc.candidates_assumed(), "{enc:?}");
        assert!(enc.no_trial_evidence() && enc.gave_up_on_unknown_trial());
        assert!(enc.spawn_failed(), "{enc:?}");
        assert_eq!(enc.video(), VideoEncoder::Mpeg4);
        let json = enc.to_json();
        assert_eq!(json["h264"], "mpeg4");
        assert_eq!(json["trial_failed"], serde_json::json!([]));
        assert_eq!(
            enc.build_note().as_deref(),
            Some("ffmpeg 실행 실패 — 클립·프록시·내보내기가 실패할 수 있음 · 잠시 뒤 다시 확인")
        );
        let expires = expiry_of(&fake).expect("cached").expect("with a TTL");
        assert!(expires < Instant::now() + INCONCLUSIVE_TTL + Duration::from_secs(1));

        let missing = dir.join("no-such-ffmpeg").to_string_lossy().into_owned();
        let enc = for_binary(&missing);
        assert!(!enc.spawn_failed() && enc.no_trial_evidence(), "{enc:?}");
        assert_eq!(enc.video(), VideoEncoder::Mpeg4);
        assert_eq!(
            enc.build_note(),
            None,
            "a missing file is reported elsewhere"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 3: concurrent callers (GET /api/tools, doctor, proxies,
    /// exports) for the same binary share one probe.
    #[test]
    fn concurrent_callers_share_one_probe() {
        let dir = std::env::temp_dir().join(format!("ft-enc-flight-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg_slow(&dir, LGPL, &[], &[("h264_mf", 2)]);
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let fake = fake.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for_binary(&fake)
                })
            })
            .collect();
        let results: Vec<Encoders> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for enc in &results {
            assert_eq!(enc.video(), VideoEncoder::H264Mf, "{enc:?}");
            assert_eq!(enc, &results[0]);
        }
        let calls = calls_of(&dir);
        // One -encoders and one h264_mf trial for all four callers.
        assert_eq!(calls.lines().count(), 2, "{calls}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fix round 3: when even a listed native mpeg4 exits non-zero,
    /// the harness is at fault — no encoder is blamed, the list-based pick
    /// stays, no Media Feature Pack hint; the inconclusive result is reused
    /// only for INCONCLUSIVE_TTL. A probe that lists nothing is also
    /// negative-cached.
    #[test]
    fn a_failing_mpeg4_control_is_a_harness_defect_and_is_negative_cached() {
        let base = std::env::temp_dir().join(format!("ft-enc-harness-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("all");
        let fake = fake_ffmpeg_failing(&dir, BTBN_LGPL, &["h264_mf", "libopenh264", "mpeg4"]);
        let enc = for_binary(&fake);
        assert!(enc.probed());
        assert!(!enc.trial_failed("h264_mf"), "{enc:?}");
        assert_eq!(enc.to_json()["trial_failed"], serde_json::json!([]));
        assert_eq!(enc.video(), VideoEncoder::H264Mf);
        assert!(
            !enc.build_note()
                .unwrap_or_default()
                .contains("Media Feature Pack"),
            "{:?}",
            enc.build_note()
        );
        let calls = calls_of(&dir);
        // -encoders, h264_mf, libopenh264, the mpeg4 control.
        assert_eq!(calls.lines().count(), 4, "{calls}");
        assert!(calls.lines().last().unwrap().contains("-c:v mpeg4"));
        // Negative-cached: no new run within the TTL …
        assert_eq!(for_binary(&fake), enc);
        assert_eq!(calls_of(&dir), calls);
        // … probed again once it expired.
        expire(&fake);
        let _ = for_binary(&fake);
        assert_eq!(calls_of(&dir).lines().count(), 8);

        // Fix round 4: mpeg4 not listed — no control is run (an unlisted
        // encoder failing proves nothing), and h264_mf's non-zero exit is a
        // real failure, kept for good.
        let dir = base.join("unlisted");
        let fake = fake_ffmpeg_failing(
            &dir,
            " ------\n V....D h264_mf  MF\n",
            &["h264_mf", "mpeg4"],
        );
        let enc = for_binary(&fake);
        assert!(enc.trial_failed("h264_mf"), "{enc:?}");
        assert_eq!(enc.video(), VideoEncoder::Mpeg4);
        let calls = calls_of(&dir);
        assert_eq!(calls.lines().count(), 2, "{calls}");
        assert!(!calls.contains("-c:v mpeg4"), "{calls}");
        assert_eq!(expiry_of(&fake), Some(None), "permanent");
        // A broken h264_mf with a working listed mpeg4 is a real failure.
        let dir = base.join("control-ok");
        let fake = fake_ffmpeg_failing(
            &dir,
            " ------\n V....D h264_mf  MF\n V.S... mpeg4  M\n",
            &["h264_mf"],
        );
        let enc = for_binary(&fake);
        assert!(enc.trial_failed("h264_mf"));
        assert_eq!(enc.video(), VideoEncoder::Mpeg4);
        assert_eq!(expiry_of(&fake), Some(None), "permanent");

        // An empty listing (the listing proves nothing): every candidate is
        // assumed and trialled (Fix round 6), negative-cached.
        let dir = base.join("empty");
        let fake = fake_ffmpeg(&dir, "");
        let enc = for_binary(&fake);
        assert!(!enc.probed() && enc.candidates_assumed() && !enc.listing_timed_out());
        assert_eq!(
            enc.video(),
            VideoEncoder::Libx264,
            "chosen by trial: {enc:?}"
        );
        assert_eq!(
            enc.build_note().as_deref(),
            Some("ffmpeg -encoders 확인 실패 — 시험 인코딩으로 libx264 선택 · 잠시 뒤 다시 확인")
        );
        assert!(!for_binary(&fake).probed());
        // -encoders and the libx264 trial, once.
        assert_eq!(calls_of(&dir).lines().count(), 2, "{}", calls_of(&dir));
        let expires = expiry_of(&fake).expect("cached").expect("with a TTL");
        assert!(expires < Instant::now() + INCONCLUSIVE_TTL + Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn trial_uses_the_proxy_options_and_writes_nothing() {
        let args = trial_args(VideoEncoder::H264Mf);
        let joined = args.join(" ");
        assert!(
            joined.contains("-f lavfi -i color=c=black:s=64x64:d=0.04 -frames:v 1"),
            "{joined}"
        );
        assert!(joined.contains("-c:v h264_mf -rate_control quality -quality 55"));
        assert!(
            joined.ends_with("-pix_fmt yuv420p -an -f null -"),
            "{joined}"
        );
        assert!(args.contains(&"-nostdin".to_string()));
    }

    #[test]
    fn probes_fake_ffmpeg_and_caches() {
        let dir = std::env::temp_dir().join(format!("ft-enc-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fake = fake_ffmpeg(&dir, LGPL);
        let enc = for_binary(&fake);
        assert!(enc.probed(), "{enc:?}");
        assert_eq!(enc.video(), VideoEncoder::H264Mf);
        assert!(!enc.has("libx264"));
        assert!(cache().lock().unwrap().keys().any(|key| key.0 == fake));
        // A missing binary falls back to the full-build assumption.
        let missing = for_binary(&dir.join("nope").to_string_lossy());
        assert!(!missing.probed());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
