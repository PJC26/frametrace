//! Proprietary-format transcode queue.
//!
//! CCTV/dashcam exports that browsers cannot play (Dahua DAV, Hanwha
//! NOV, IDIS/Avigilon containers, raw .264 streams) are queued one at a
//! time through the same ffmpeg H.264 proxy pipeline the single-file
//! proxy button uses — so every output lands in artifacts/proxies with
//! the chained source/output sha256 logging that path already provides.
//! A per-item queue log records *why* each file was queued and its
//! honest outcome, including formats ffmpeg simply cannot parse — an
//! examiner sees which files need vendor tools instead of a silent skip.

use crate::artifacts::{ProxyOptions, generate_proxy};
use crate::audit;
use crate::util::{json_escape, now_unix, read_to_string};
use std::path::{Path, PathBuf};

const QUEUE_LOG: &str = "artifacts/logs/transcode-queue-log.jsonl";
const BROWSER_CODECS: &[&str] = &["h264", "vp8", "vp9", "av1", "theora"];
const BROWSER_CONTAINERS: &[&str] = &["mp4", "mov", "webm", "matroska", "ogg"];
const PROPRIETARY_EXTS: &[&str] = &["dav", "nov", "ave", "h264", "264", "h265", "sec", "ts"];

#[derive(Debug)]
pub struct QueueItem {
    pub selector: String,
    pub source_path: String,
    pub why: String,
}

#[derive(Debug, Default)]
pub struct QueueSummary {
    pub total: usize,
    pub proxied: usize,
    pub skipped_existing: usize,
    pub failed: usize,
}

/// Honest browser-playability gate: a record needs a proxy when ffprobe's
/// container or codec isn't in the browser-safe set, the extension marks
/// a proprietary recorder export, or ffprobe failed outright (corrupt or
/// proprietary container — queued anyway so its failure is logged, not
/// hidden).
pub fn needs_transcode(video: &serde_json::Value) -> Option<String> {
    let ext = video
        .get("extension")
        .or_else(|| video.get("relative_path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .rsplit('.')
        .next()
        .unwrap_or("");
    unplayable_reason(
        ext,
        video.get("ffprobe_ok").and_then(|v| v.as_bool()),
        video
            .get("format_name")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        video
            .get("video_codec")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    )
}

/// Field-level playability check shared by the index-driven queue and by
/// callers that only have a probed file (recovered/carved outputs that
/// never entered the video index).
pub fn unplayable_reason(
    ext: &str,
    probe_ok: Option<bool>,
    format_name: &str,
    video_codec: &str,
) -> Option<String> {
    let ext = ext.to_ascii_lowercase();
    if PROPRIETARY_EXTS.contains(&ext.as_str()) {
        return Some(format!("proprietary extension .{ext}"));
    }
    if probe_ok == Some(false) {
        return Some("ffprobe parse failed — proprietary or corrupt container".to_string());
    }
    let format = format_name.to_ascii_lowercase();
    if !format.is_empty() && !BROWSER_CONTAINERS.iter().any(|c| format.contains(c)) {
        return Some(format!("container '{format}' not browser-playable"));
    }
    let codec = video_codec.to_ascii_lowercase();
    if !codec.is_empty() && !BROWSER_CODECS.contains(&codec.as_str()) {
        return Some(format!("codec '{codec}' not browser-playable"));
    }
    None
}

/// D5: the shared cache lookup — path-free `proxy_<id>_<hash8>.mp4`
/// names first, then pre-D5 `<sanitize(selector)>_proxy_*` names.
fn existing_proxy(case_dir: &Path, selector: &str) -> Option<PathBuf> {
    crate::artifacts::find_existing_proxy(case_dir, selector)
}

/// Runs the queue: for each candidate, generate_proxy sequentially.
/// Per-item results + a summary entry go to the chained queue log.
pub fn run_queue(
    case_dir: &Path,
    only: Option<&[String]>,
    force: bool,
) -> Result<QueueSummary, String> {
    let index_text = read_to_string(&case_dir.join("db/video_index.json"))
        .map_err(|e| format!("failed to read video index: {e}"))?;
    let index: serde_json::Value =
        serde_json::from_str(&index_text).map_err(|e| format!("bad video index: {e}"))?;
    let videos = index
        .get("videos")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut items: Vec<QueueItem> = Vec::new();
    for video in &videos {
        let id = video
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        if let Some(only) = only
            && !only.iter().any(|o| o == &id)
        {
            continue;
        }
        if let Some(why) = needs_transcode(video) {
            items.push(QueueItem {
                selector: id,
                source_path: video
                    .get("source_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                why,
            });
        }
    }

    let log_path = case_dir.join(QUEUE_LOG);
    let mut summary = QueueSummary {
        total: items.len(),
        ..Default::default()
    };
    for (i, item) in items.iter().enumerate() {
        let (result, output, error) = match existing_proxy(case_dir, &item.selector) {
            Some(p) if !force => ("skipped-existing", Some(p), None),
            _ => match generate_proxy(case_dir, &item.selector, &ProxyOptions::default()) {
                Ok(a) => ("proxied", Some(a.output_path), None),
                Err(e) => ("failed", None, Some(e)),
            },
        };
        match result {
            "proxied" => summary.proxied += 1,
            "skipped-existing" => summary.skipped_existing += 1,
            _ => summary.failed += 1,
        }
        println!(
            "transcode queue {}/{}: {} → {}",
            i + 1,
            items.len(),
            item.selector,
            result
        );
        let line = format!(
            "{{\"schema_version\":1,\"event\":\"transcode-queue-item\",\"unix\":{},\"selector\":\"{}\",\"source_path\":\"{}\",\"why\":\"{}\",\"result\":\"{}\",\"output_path\":{},\"error\":{}}}",
            now_unix()?,
            json_escape(&item.selector),
            json_escape(&item.source_path),
            json_escape(&item.why),
            result,
            output
                .as_ref()
                .map(|p| format!("\"{}\"", json_escape(&p.to_string_lossy())))
                .unwrap_or_else(|| "null".into()),
            error
                .as_ref()
                .map(|e| format!("\"{}\"", json_escape(e)))
                .unwrap_or_else(|| "null".into()),
        );
        audit::append_chained_jsonl(&log_path, &line)?;
    }
    let summary_line = format!(
        "{{\"schema_version\":1,\"event\":\"transcode-queue\",\"unix\":{},\"total\":{},\"proxied\":{},\"skipped_existing\":{},\"failed\":{}}}",
        now_unix()?,
        summary.total,
        summary.proxied,
        summary.skipped_existing,
        summary.failed
    );
    audit::append_chained_jsonl(&log_path, &summary_line)?;
    println!("QUEUE-RESULT {summary_line}");
    Ok(summary)
}

/// Latest queue summary for status display.
/// R9 P3: the newest entry usable here (read through
/// `audit::read_lane_rows`), with its `trust` code.
///
/// R14 I5: the NEWEST line of the lane, whatever it is — an unusable line
/// after the last usable summary may itself be a newer summary (P12: its
/// content is never read), and a summary before a `lane-repair` marker is
/// not known to be the latest. Either way: `None` (no summary claimed).
pub fn latest_summary(case_dir: &Path) -> Option<serde_json::Value> {
    let lane = crate::audit::read_lane_rows(&case_dir.join(QUEUE_LOG)).ok()?;
    let row = lane.rows.last()?;
    if !row.usable() || lane.repaired_after(row.index) {
        return None;
    }
    Some(row.value_with_trust())
        .filter(|v| v.get("event").and_then(|e| e.as_str()) == Some("transcode-queue"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(fields: &[(&str, serde_json::Value)]) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        for (k, v) in fields {
            m.insert(k.to_string(), v.clone());
        }
        serde_json::Value::Object(m)
    }

    #[test]
    fn queues_proprietary_and_unplayable() {
        assert!(needs_transcode(&video(&[("extension", "dav".into())])).is_some());
        assert!(needs_transcode(&video(&[("extension", "nov".into())])).is_some());
        assert!(
            needs_transcode(&video(&[
                ("format_name", "dav".into()),
                ("video_codec", "h264".into())
            ]))
            .is_some()
        );
        assert!(
            needs_transcode(&video(&[
                ("format_name", "mov,mp4".into()),
                ("video_codec", "mpeg2video".into())
            ]))
            .is_some()
        );
        assert!(needs_transcode(&video(&[("ffprobe_ok", false.into())])).is_some());
    }

    #[test]
    fn passes_browser_friendly() {
        assert!(
            needs_transcode(&video(&[
                ("extension", "mp4".into()),
                ("format_name", "mov,mp4,m4a".into()),
                ("video_codec", "h264".into()),
                ("ffprobe_ok", true.into()),
            ]))
            .is_none()
        );
        assert!(
            needs_transcode(&video(&[
                ("extension", "webm".into()),
                ("format_name", "matroska,webm".into()),
                ("video_codec", "vp9".into()),
            ]))
            .is_none()
        );
    }

    /// Integration (D5 × queue): a proxy the queue generated under the
    /// path-free `proxy_<id>_<hash8>.mp4` name is found by the next run
    /// (before: the lookup only knew `<selector>_proxy_*`, so every run
    /// re-transcoded), and a pre-D5 `_proxy_` file still counts.
    /// Uses the real ffmpeg; skipped when it is not on PATH.
    #[test]
    fn queue_finds_path_free_and_legacy_proxies() {
        if crate::tool_policy::resolve_tool_binary("ffmpeg", &["ffmpeg"]).is_err() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let case_dir =
            std::env::temp_dir().join(format!("frametrace-transcode-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&case_dir);
        std::fs::create_dir_all(case_dir.join("db")).unwrap();
        let source = case_dir.join("clip.dav");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc=size=160x120:rate=5:duration=1"])
            .args(["-c:v", "mpeg4", "-f", "mp4"])
            .arg(&source)
            .status()
            .unwrap();
        assert!(made.success());
        let source_text = source.to_string_lossy().to_string();
        std::fs::write(
            case_dir.join("db/video_paths.tsv"),
            format!("id\tsource_path\trelative_path\nvid_000001\t{source_text}\tclip.dav\n"),
        )
        .unwrap();
        std::fs::write(
            case_dir.join("db/video_index.json"),
            serde_json::json!({"videos": [{"id": "vid_000001", "extension": "dav", "source_path": source_text}]})
                .to_string(),
        )
        .unwrap();

        let first = run_queue(&case_dir, None, false).unwrap();
        assert_eq!((first.proxied, first.failed), (1, 0), "{first:?}");
        let made = existing_proxy(&case_dir, "vid_000001").expect("generated proxy is found");
        let name = made.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("proxy_"), "{name}");
        let second = run_queue(&case_dir, None, false).unwrap();
        assert_eq!(
            (second.proxied, second.skipped_existing),
            (0, 1),
            "the path-free proxy is reused: {second:?}"
        );

        // A proxy written before D5 is still reused.
        std::fs::remove_file(&made).unwrap();
        let legacy = case_dir.join("artifacts/proxies/vid_000001_proxy_1700000000.mp4");
        std::fs::write(&legacy, b"legacy").unwrap();
        assert_eq!(existing_proxy(&case_dir, "vid_000001"), Some(legacy));
        let third = run_queue(&case_dir, None, false).unwrap();
        assert_eq!((third.proxied, third.skipped_existing), (0, 1), "{third:?}");
        let _ = std::fs::remove_dir_all(&case_dir);
    }
}
