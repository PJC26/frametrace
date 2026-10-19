//! Encoder selection for the resolved ffmpeg.
//!
//! The bundled FFmpeg is an LGPL build (FrameTrace is MIT), so it has no
//! libx264 (GPL). Proxies and MP4 exports therefore never hard-code an
//! encoder: the resolved ffmpeg is asked once for `-encoders` and the
//! best H.264 encoder it really has is used, in the order
//! libx264 → h264_mf (Windows Media Foundation) → libopenh264 → mpeg4
//! (MPEG-4 Part 2, built into every ffmpeg). h264_mf comes before
//! libopenh264 on purpose: the pinned BtbN LGPL build links openh264
//! built from source, and Cisco's patent licence covers only Cisco's own
//! prebuilt openh264 binaries — Media Foundation's encoder is licensed
//! with Windows. (h264_mf exists only on Windows, so elsewhere the order
//! is effectively libx264 → libopenh264 → mpeg4.) Audio prefers the native,
//! LGPL-safe `aac`. The pick is recorded in the audit log lines so an
//! output stays reproducible.
//!
//! Being listed is not enough: `h264_mf` is listed by every Windows build
//! but fails at `MFStartup` on Windows N/KN editions without the Media
//! Feature Pack and on Server Core — the bundle (no libx264) would then
//! fail every proxy and export. So each listed candidate, best first, is
//! tried once with a 1-frame encode of a 64×64 black frame using the very
//! options a proxy uses (`-f lavfi -i color=… -frames:v 1 <video args>
//! -f null -`); one that fails is skipped and the next one tried. A trial
//! that cannot run at all (spawn error, timeout) proves nothing: the
//! candidate is kept (the pre-trial behaviour) and the result is not
//! cached, so the next call tries again.
//!
//! The probe and the trials are cached together per ffmpeg path + mtime +
//! size and run through the bounded
//! [`crate::util::run_with_timeout_detailed`] (console hidden). Callers
//! must not hold the server's state lock while calling [`for_binary`].

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

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

    pub fn probed(&self) -> bool {
        self.probed
    }

    pub fn has(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// The best available video encoder — listed and not failed its trial
    /// encode; mpeg4 is native to every ffmpeg.
    pub fn video(&self) -> VideoEncoder {
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
    /// Media Feature Pack.
    pub fn build_note(&self) -> Option<String> {
        let video = self.video();
        if !self.probed || video == VideoEncoder::Libx264 {
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
            other => format!("LGPL 빌드 — H.264는 {} 사용", other.name()),
        };
        if self.trial_failed(VideoEncoder::H264Mf.name()) {
            note.push_str(
                " · h264_mf 사용 불가 — Media Feature Pack 필요 (Windows N/KN 에디션·Server Core)",
            );
        }
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

fn cache() -> &'static Mutex<HashMap<CacheKey, Encoders>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Encoders>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The encoders of the ffmpeg at `binary` (a resolved absolute path).
/// Cached per path + mtime + size; a failed probe is not cached and
/// yields [`Encoders::assumed_full`].
pub fn for_binary(binary: &str) -> Encoders {
    let meta = std::fs::metadata(Path::new(binary)).ok();
    let key: CacheKey = (
        binary.to_string(),
        meta.as_ref().and_then(|m| m.modified().ok()),
        meta.as_ref().map(|m| m.len()).unwrap_or(0),
    );
    if let Some(hit) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return hit.clone();
    }
    // Probe outside the cache lock.
    let secs = crate::util::version_probe_timeout_secs();
    let result = crate::util::run_with_timeout_detailed(
        Command::new(binary).args(["-hide_banner", "-encoders"]),
        Some(secs),
    );
    let mut encoders = match result {
        Ok(output) if output.status.success() => {
            Encoders::from_listing(&String::from_utf8_lossy(&output.stdout))
        }
        _ => return Encoders::assumed_full(),
    };
    if encoders.names.is_empty() {
        return Encoders::assumed_full();
    }
    // Trial-encode the listed candidates, best first, until one works.
    let mut conclusive = true;
    for candidate in VideoEncoder::PREFERENCE {
        if !encoders.has(candidate.name()) {
            continue;
        }
        match trial_encode(binary, candidate, secs) {
            Trial::Works => break,
            Trial::Fails => encoders.mark_failed(candidate),
            Trial::Unknown => {
                conclusive = false;
                break;
            }
        }
    }
    if conclusive {
        cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, encoders.clone());
    }
    encoders
}

enum Trial {
    Works,
    Fails,
    /// Could not run (spawn error, timeout): proves nothing.
    Unknown,
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
    match crate::util::run_with_timeout_detailed(
        Command::new(binary).args(trial_args(encoder)),
        Some(secs),
    ) {
        Ok(output) if output.status.success() => Trial::Works,
        Ok(_) => Trial::Fails,
        Err(_) => Trial::Unknown,
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

    /// A fake ffmpeg whose `-encoders` prints `listing` (every trial
    /// encode succeeds).
    pub fn fake_ffmpeg(dir: &Path, listing: &str) -> String {
        fake_ffmpeg_failing(dir, listing, &[])
    }

    /// [`fake_ffmpeg`] whose trial encode exits 1 for the `failing`
    /// encoders (e.g. h264_mf on Windows N without the Media Feature
    /// Pack). Every call is logged to `calls.log` next to it.
    pub fn fake_ffmpeg_failing(dir: &Path, listing: &str, failing: &[&str]) -> String {
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
            body.push_str("exit /b 0\r\n");
            std::fs::write(&path, body).unwrap();
            path.to_string_lossy().into_owned()
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.join("ffmpeg");
            let log = dir.join("calls.log");
            let mut script = format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\ncase \" $* \" in *\" -encoders \"*) cat <<'EOF'\n{listing}EOF\nexit 0;; esac\n",
                log.display()
            );
            for name in failing {
                script.push_str(&format!(
                    "case \" $* \" in *\" -c:v {name} \"*) exit 1;; esac\n"
                ));
            }
            script.push_str("exit 0\n");
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        // The pinned BtbN LGPL build has both: Media Foundation wins over
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
    fn btbn_lgpl_bundle_never_picks_libopenh264() {
        let enc = Encoders::from_listing(BTBN_LGPL);
        assert!(enc.has("libopenh264") && enc.has("h264_mf"));
        assert_eq!(enc.video(), VideoEncoder::H264Mf);
        assert_eq!(
            enc.build_note().as_deref(),
            Some("LGPL 빌드 — H.264는 h264_mf 사용")
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
            Some("LGPL 빌드 — H.264는 h264_mf 사용")
        );
        assert!(
            Encoders::from_listing(BARE)
                .build_note()
                .unwrap()
                .contains("mpeg4")
        );
    }

    /// B-5 (Fix round 2): h264_mf is listed but cannot start (Windows N/KN
    /// without the Media Feature Pack, Server Core) → the next listed
    /// candidate is used, the note says why, and the result is cached
    /// with the probe (no second trial).
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
        assert!(note.contains("Media Feature Pack 필요"), "{note}");
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
            Some("LGPL 빌드 — H.264는 h264_mf 사용")
        );
        let _ = std::fs::remove_dir_all(&dir);
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
