use super::*;
use crate::artifacts::{self, ProxyOptions, ThumbnailOptions};
use crate::audit;
use crate::case_db;
use crate::telemetry;
use crate::tool_policy::require_case_output_path;
use crate::transcode;
use crate::util::json_escape;
use crate::validation::{self, ValidationOptions};
use crate::video_export::{self, ExportOptions};
use std::path::{Path, PathBuf};

pub fn export_dav(
    case_dir: &Path,
    dav_file: &Path,
    output: Option<PathBuf>,
    timeout_secs: Option<u64>,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let job = case_db::start_job(case_dir, "export-dav", dav_file, None, "{}")?;
    let result = export_dav_inner(case_dir, dav_file, output, timeout_secs);
    if let Err(err) = &result {
        // Early failures (bad output path, unparseable DAV, remux, digest)
        // must fail the job row too — otherwise `running` leaks forever.
        let _ = case_db::fail_job(case_dir, &job.job_id, err);
    }
    result?;
    case_db::complete_job(case_dir, &job.job_id, 1, "export-dav completed")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn export_dav_inner(
    case_dir: &Path,
    dav_file: &Path,
    output: Option<PathBuf>,
    timeout_secs: Option<u64>,
) -> Result<(), String> {
    // Type gate: ffmpeg can remux an ordinary MP4 with `-c copy`, so success
    // alone never proved DAV input. Require DAV/DAHUA magic up front so
    // non-DAV files are rejected as "not a DAV container" instead of being
    // silently counted as DAV exports.
    {
        let mut head = [0u8; 5];
        let mut f = std::fs::File::open(dav_file)
            .map_err(|err| format!("failed to open DAV {}: {err}", dav_file.display()))?;
        let read = std::io::Read::read(&mut f, &mut head)
            .map_err(|err| format!("failed to read DAV header: {err}"))?;
        if !crate::dav::is_dav_header(&head[..read]) {
            return Err(format!(
                "not a DAV container (missing DHAV/DAHUA magic): {}",
                dav_file.display()
            ));
        }
    }

    let stem = dav_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("dav");
    let requested_raw = output.unwrap_or_else(|| {
        crate::util::unique_available_path(
            &case_dir.join("artifacts/clips").join(format!("{stem}.mp4")),
        )
    });
    require_case_output_path(case_dir, &requested_raw, "DAV export")?;
    if requested_raw.exists() {
        return Err(format!(
            "output already exists: {} (choose a new --output path)",
            requested_raw.display()
        ));
    }

    // E10: a frame walk that fails fails the export (never logged as 0).
    let summary = crate::dav::summarize_frames(dav_file)?;
    let frames = summary.video_frames();
    let channel = summary.first_video_channel;
    let method =
        crate::dav::remux_dav_to_mp4(dav_file, &requested_raw, timeout_secs).inspect_err(|_| {
            let _ = std::fs::remove_file(&requested_raw);
        })?;
    let output_sha256 = audit::digest_file(&requested_raw)?;
    let validation = if method == "ffmpeg-dhav-demux" {
        "ffmpeg-native-demux"
    } else {
        "es-extract-remux-candidate"
    };

    // The ES fallback carries only the first video channel; ffmpeg's demuxer
    // decides itself, so its exported frame count is not known here.
    let (exported_video_frames, exported_note) = if method == "ffmpeg-dhav-demux" {
        (
            "null".to_string(),
            "ffmpeg dhav demuxer chose the exported stream; per-channel count not verified",
        )
    } else {
        (
            summary.pinned_channel_frames().to_string(),
            "es-extract-remux exports only the first video channel",
        )
    };
    let line = format!(
        "{{\"schema_version\":1,\"event\":\"export-dav\",\"selector\":\"{}\",\"source_path\":\"{}\",\"format\":\"mp4\",\"output_path\":\"{}\",\"output_sha256\":\"{}\",\"remux_method\":\"{}\",\"video_frames\":{},\"video_frames_by_channel\":{},\"audio_frames\":{},\"channel\":{},\"exported_video_frames\":{},\"exported_frames_note\":\"{}\",\"container_validation\":\"{}\"}}",
        json_escape(stem),
        json_escape(&dav_file.to_string_lossy()),
        json_escape(&requested_raw.to_string_lossy()),
        json_escape(&output_sha256),
        json_escape(method),
        frames,
        summary.channels_json(),
        summary.audio_frames,
        channel
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        exported_video_frames,
        json_escape(exported_note),
        json_escape(validation),
    );
    audit::append_chained_jsonl(&case_dir.join("artifacts/clips/export-log.jsonl"), &line)?;

    println!("dav remux complete");
    println!(
        "method: {method} · video frames: {frames} · channel: {}",
        channel
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!("output: {}", requested_raw.display());
    println!("output sha256: {output_sha256}");
    println!("note: keep the original DAV; field validation still requires real recorder samples.");
    Ok(())
}

/// Remuxes a Hikvision IMKH-prefixed export to MP4 (strip 40-byte header).
pub fn export_hik(
    case_dir: &Path,
    hik_file: &Path,
    output: Option<PathBuf>,
    timeout_secs: Option<u64>,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let job = case_db::start_job(case_dir, "export-hik", hik_file, None, "{}")?;
    let result = export_hik_inner(case_dir, hik_file, output, timeout_secs);
    if let Err(err) = &result {
        // Early failures must fail the job row too — otherwise `running`
        // leaks forever and the jobs table misstates what happened.
        let _ = case_db::fail_job(case_dir, &job.job_id, err);
    }
    result?;
    case_db::complete_job(case_dir, &job.job_id, 1, "export-hik completed")?;
    Ok(())
}

fn export_hik_inner(
    case_dir: &Path,
    hik_file: &Path,
    output: Option<PathBuf>,
    timeout_secs: Option<u64>,
) -> Result<(), String> {
    let stem = hik_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("hik");
    let requested_raw = output.unwrap_or_else(|| {
        crate::util::unique_available_path(
            &case_dir.join("artifacts/clips").join(format!("{stem}.mp4")),
        )
    });
    require_case_output_path(case_dir, &requested_raw, "Hikvision export")?;
    if requested_raw.exists() {
        return Err(format!(
            "output already exists: {} (choose a new --output path)",
            requested_raw.display()
        ));
    }

    let stripped = crate::util::unique_path(
        &case_dir
            .join("artifacts/carved")
            .join(format!("{stem}.imkh-stripped.bin")),
    );
    let method =
        crate::hikvision::remux_imkh_to_mp4(hik_file, &requested_raw, &stripped, timeout_secs)
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&requested_raw);
            })?;
    let output_sha256 = audit::digest_file(&requested_raw)?;
    let line = format!(
        "{{\"schema_version\":1,\"event\":\"export-hik\",\"selector\":\"{}\",\"source_path\":\"{}\",\"format\":\"mp4\",\"output_path\":\"{}\",\"output_sha256\":\"{}\",\"remux_method\":\"{}\",\"stripped_path\":\"{}\",\"container_validation\":\"imkh-header-stripped-candidate\"}}",
        json_escape(stem),
        json_escape(&hik_file.to_string_lossy()),
        json_escape(&requested_raw.to_string_lossy()),
        json_escape(&output_sha256),
        json_escape(method),
        json_escape(&stripped.to_string_lossy()),
    );
    audit::append_chained_jsonl(&case_dir.join("artifacts/clips/export-log.jsonl"), &line)?;

    println!("hikvision remux complete");
    println!("method: {method}");
    println!("output: {}", requested_raw.display());
    println!("output sha256: {output_sha256}");
    println!("note: keep the original export; HDD FS recovery is out of scope without a corpus.");
    Ok(())
}

pub fn export_video(case_dir: &Path, selector: &str, options: ExportOptions) -> Result<(), String> {
    ensure_case(case_dir)?;
    let result = video_export::export_video(case_dir, selector, &options)?;
    println!("video exported");
    println!("source: {}", result.source_path.display());
    println!("output: {}", result.output_path.display());
    println!("format: {}", result.format.extension());
    Ok(())
}

pub fn transcode_queue(case_dir: &Path, only: Option<&str>, force: bool) -> Result<(), String> {
    ensure_case(case_dir)?;
    let ids: Option<Vec<String>> = only.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    });
    let summary = transcode::run_queue(case_dir, ids.as_deref(), force)?;
    println!(
        "queue done: {} candidates · {} proxied · {} cached · {} failed",
        summary.total, summary.proxied, summary.skipped_existing, summary.failed
    );
    Ok(())
}

pub fn extract_telemetry(case_dir: &Path, selector: &str) -> Result<(), String> {
    ensure_case(case_dir)?;
    let (artifact, report) = telemetry::extract_telemetry(case_dir, selector)?;
    println!("telemetry extracted");
    println!("source: {}", report.source_path);
    println!("points: {}", report.point_count);
    println!("lane: {}", report.source);
    if let (Some(a), Some(b)) = (report.first_ts_unix, report.last_ts_unix) {
        println!("range: {} .. {}", a as i64, b as i64);
    }
    if let Some(s) = report.max_speed_kmh {
        println!("max speed: {:.1} km/h", s);
    }
    for s in &report.unparsed_streams {
        println!("unparsed: {s}");
    }
    println!("artifact: {}", artifact.display());
    Ok(())
}

pub fn make_proxy(case_dir: &Path, selector: &str, options: ProxyOptions) -> Result<(), String> {
    ensure_case(case_dir)?;
    let result = artifacts::generate_proxy(case_dir, selector, &options)?;
    println!("proxy generated");
    println!("source: {}", result.source_path.display());
    println!("output: {}", result.output_path.display());
    Ok(())
}

pub fn make_thumbnail(
    case_dir: &Path,
    selector: &str,
    options: ThumbnailOptions,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let result = artifacts::generate_thumbnail(case_dir, selector, &options)?;
    println!("thumbnail generated");
    println!("source: {}", result.source_path.display());
    println!("output: {}", result.output_path.display());
    Ok(())
}

pub fn validate_artifact(
    case_dir: &Path,
    selector: &str,
    options: ValidationOptions,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let job = case_db::start_job(
        case_dir,
        "validate-artifact",
        Path::new(selector),
        Some(1),
        &validation_options_json(&options),
    )?;
    let result = match validation::validate_artifact(case_dir, selector, &options) {
        Ok(result) => result,
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            return Err(err);
        }
    };
    case_db::complete_job(case_dir, &job.job_id, 1, "validate-artifact completed")?;
    println!("artifact validated");
    println!("job: {} ({})", job.job_id, job.job_type);
    println!("selector: {}", result.selector);
    println!("target: {}", result.target_path.display());
    println!("sha256: {}", result.target_sha256);
    println!("status: {}", result.validation_status);
    println!("note: {}", result.validation_note);
    if let Some(codec) = result.probe.video_codec {
        println!("video codec: {codec}");
    }
    if let Some(duration) = result.probe.duration_seconds {
        println!("duration seconds: {duration:.3}");
    }
    Ok(())
}
