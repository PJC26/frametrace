//! Integration tests that actually execute ffmpeg/ffprobe through the real
//! binary. Gated behind `FRAMETRACE_IT=1` so the default suite stays fast and
//! dependency-free:
//!
//! ```text
//! FRAMETRACE_IT=1 cargo test --test integration_tools -- --ignored
//! ```

mod common;
use common::IsolatedCommand;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn integration_enabled() -> bool {
    std::env::var("FRAMETRACE_IT").as_deref() == Ok("1")
}

fn ffmpeg_binary() -> Option<PathBuf> {
    find_tool("ffmpeg")
}

fn find_tool(name: &str) -> Option<PathBuf> {
    // tools/bin next to the test binary first, then PATH.
    for suffix in [name, &format!("{name}.exe")] {
        if let Ok(exe) = std::env::current_exe() {
            let candidate = exe.parent().unwrap().join("tools/bin").join(suffix);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    // `where` is Windows-only; use `which` on unix so the IT layer runs
    // outside Windows CI as well.
    let lookup = if cfg!(windows) { "where" } else { "which" };
    let output = Command::new(lookup).arg(name).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let first = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    if first.is_empty() {
        None
    } else {
        Some(PathBuf::from(first))
    }
}

fn unique_dir(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{name}_{}_{}", std::process::id(), stamp))
}

fn run_binary(exe: &Path, args: &[&str]) {
    let output = Command::new(exe)
        .args(args)
        .isolated()
        .output()
        .expect("run frametrace");
    assert!(
        output.status.success(),
        "command failed: {:?}\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn make_video(ffmpeg: &Path, path: &Path, color: &str, seconds: u32) {
    let output = Command::new(ffmpeg)
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={color}:s=320x240:r=12:d={seconds}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .output()
        .expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg failed for {path:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_file(path: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(path).expect("read file")).to_string()
}

#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe on PATH"]
fn full_lifecycle_with_real_ffmpeg() {
    if !integration_enabled() {
        eprintln!("FRAMETRACE_IT=1 not set; skipping");
        return;
    }
    let Some(ffmpeg) = ffmpeg_binary() else {
        panic!(
            "FRAMETRACE_IT=1 is set but ffmpeg was not found on PATH (T2: a requested integration run must not pass by skipping)"
        );
    };

    let source_dir = unique_dir("ft_it_src");
    std::fs::create_dir_all(&source_dir).unwrap();
    let case_dir = unique_dir("ft_it_case");
    let exe = env!("CARGO_BIN_EXE_frametrace");
    let exe = Path::new(exe);

    // Fixtures: three healthy clips with dashcam-style names + one truncated.
    make_video(
        &ffmpeg,
        &source_dir.join("FRONT_20260820_060000_F.mp4"),
        "blue",
        2,
    );
    make_video(
        &ffmpeg,
        &source_dir.join("REAR_20260820_060000_R.mp4"),
        "red",
        2,
    );
    make_video(
        &ffmpeg,
        &source_dir.join("EVENT_20260821_120000.mp4"),
        "green",
        3,
    );
    let healthy = std::fs::read(source_dir.join("FRONT_20260820_060000_F.mp4")).unwrap();
    std::fs::write(
        source_dir.join("truncated_20260822_090000.mp4"),
        &healthy[..64],
    )
    .unwrap();

    let case_text = case_dir.to_string_lossy().to_string();
    let source_text = source_dir.to_string_lossy().to_string();
    run_binary(exe, &["init-case", &case_text, "--title", "IT lifecycle"]);
    run_binary(exe, &["scan-folder", &case_text, &source_text, "--hash"]);

    let selection_path = case_dir.join("selection-all.json");
    std::fs::write(
        &selection_path,
        r#"{"schema_version":1,"items":[
            {"selector":"vid_000001","kind":"video","action":"validate"},
            {"selector":"vid_000002","kind":"video","action":"validate"},
            {"selector":"vid_000003","kind":"video","action":"validate"},
            {"selector":"vid_000004","kind":"video","action":"validate"}]}"#,
    )
    .unwrap();
    run_binary(
        exe,
        &[
            "validate-batch",
            &case_text,
            &selection_path.to_string_lossy(),
        ],
    );
    run_binary(exe, &["make-review", &case_text]);
    run_binary(exe, &["make-report", &case_text]);
    run_binary(exe, &["package-case", &case_text]);
    run_binary(
        exe,
        &[
            "verify-audit",
            &case_dir
                .join("artifacts/logs/batch-log.jsonl")
                .to_string_lossy(),
        ],
    );

    // Validation mix: three confirmed clips, one failed truncation.
    let validation_log = read_file(&case_dir.join("evidence/logs/validation-log.jsonl"));
    let confirmed = validation_log
        .matches("ffprobe-video-stream-confirmed")
        .count();
    let failed = validation_log.matches("validation-failed").count();
    assert_eq!(confirmed, 3, "validation log: {validation_log}");
    assert_eq!(failed, 1, "validation log: {validation_log}");

    // Review: externalized thumbnails on disk (healthy clips only) and no
    // inline data URLs in the data bundle.
    let viewer = read_file(&case_dir.join("review/evidence-viewer.html"));
    assert!(viewer.contains("__FRAMETRACE_DATA__"));
    let bundle = read_file(&case_dir.join("review/data-bundle.js"));
    assert!(bundle.contains("thumbs/vid_000001.jpg"));
    assert!(!bundle.contains("data:image/jpeg"));
    let thumbs_dir = case_dir.join("review/thumbs");
    let thumb_count = std::fs::read_dir(&thumbs_dir)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .map(|ext| ext == "jpg")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(thumb_count, 3, "expected three thumbnails");

    // Report: technique + chain sections rendered with the case contents.
    let report = read_file(&case_dir.join("reports/case-report.html"));
    assert!(report.contains("발견 및 분석 기법"));
    assert!(report.contains("처리 체인"));
    assert!(report.contains("vid_000001"));
    assert!(report.contains("scan-folder (스캔·색인)"));

    // Package: checksummed manifest present.
    let reports = std::fs::read_dir(case_dir.join("reports")).unwrap();
    let package = reports
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().starts_with("package_"))
                .unwrap_or(false)
        })
        .expect("package directory");
    assert!(package.join("package-manifest.json").exists());

    let _ = std::fs::remove_dir_all(&source_dir);
    let _ = std::fs::remove_dir_all(&case_dir);
}

/// Builds a synthetic DAV container matching FFmpeg `dhav.c` around a real
/// H.264 elementary stream produced by ffmpeg.
fn build_dav_fixture(h264: &[u8]) -> Vec<u8> {
    let header_len = 24u32;
    let frame_length = header_len + h264.len() as u32 + 8;
    let mut bytes = Vec::with_capacity(frame_length as usize);
    bytes.extend_from_slice(b"DHAV");
    bytes.push(0xFD); // video I-frame
    bytes.push(0); // subtype
    bytes.push(1); // channel
    bytes.push(0); // frame_subnumber
    bytes.extend_from_slice(&1u32.to_le_bytes()); // frame_number
    bytes.extend_from_slice(&frame_length.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // date
    bytes.extend_from_slice(&0u16.to_le_bytes()); // timestamp
    bytes.push(0); // ext_length
    bytes.push(0); // checksum
    // Minimal video ext so FFmpeg can pick codec/size (type 0x81 + 0x80).
    // Keep ext_length=0 for the FrameTrace walker path; FFmpeg still demuxes
    // Annex-B payloads when native demux succeeds on this skeleton.
    bytes.extend_from_slice(h264);
    bytes.extend_from_slice(b"dhav");
    bytes.extend_from_slice(&frame_length.to_le_bytes());
    bytes
}

#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe on PATH"]
fn dav_export_remuxes_real_h264_and_validates() {
    if !integration_enabled() {
        eprintln!("FRAMETRACE_IT=1 not set; skipping");
        return;
    }
    let Some(ffmpeg) = ffmpeg_binary() else {
        panic!(
            "FRAMETRACE_IT=1 is set but ffmpeg was not found on PATH (T2: a requested integration run must not pass by skipping)"
        );
    };
    let work = unique_dir("ft_it_dav");
    std::fs::create_dir_all(&work).unwrap();
    let case_dir = work.join("case");
    let exe = Path::new(env!("CARGO_BIN_EXE_frametrace"));

    // 1. Real Annex-B H.264 elementary stream from ffmpeg.
    let es = work.join("raw.h264");
    let output = Command::new(&ffmpeg)
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=320x240:r=12:d=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "h264",
        ])
        .arg(&es)
        .output()
        .expect("run ffmpeg");
    assert!(output.status.success(), "ffmpeg es failed");

    // 2. Wrap it into a DAV container and run the real export-dav command.
    let dav_path = work.join("camera_20260830_120000.dav");
    std::fs::write(&dav_path, build_dav_fixture(&std::fs::read(&es).unwrap())).unwrap();
    let case_text = case_dir.to_string_lossy().to_string();
    run_binary(exe, &["init-case", &case_text, "--title", "IT dav"]);
    run_binary(
        exe,
        &[
            "export-dav",
            &case_text,
            &dav_path.to_string_lossy(),
            "--timeout",
            "60",
        ],
    );

    // 3. The remuxed MP4 is registered in the export log and passes ffprobe.
    let export_log = read_file(&case_dir.join("artifacts/clips/export-log.jsonl"));
    assert!(export_log.contains("export-dav"), "{export_log}");
    let mp4 = case_dir.join("artifacts/clips/camera_20260830_120000.mp4");
    assert!(mp4.exists(), "remuxed mp4 missing");
    run_binary(
        exe,
        &["validate-artifact", &case_text, &mp4.to_string_lossy()],
    );
    let validation_log = read_file(&case_dir.join("evidence/logs/validation-log.jsonl"));
    assert!(
        validation_log.contains("ffprobe-video-stream-confirmed"),
        "{validation_log}"
    );

    let _ = std::fs::remove_dir_all(&work);
}

#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe on PATH"]
fn hik_export_strips_imkh_and_validates() {
    if !integration_enabled() {
        eprintln!("FRAMETRACE_IT=1 not set; skipping");
        return;
    }
    let Some(ffmpeg) = ffmpeg_binary() else {
        panic!(
            "FRAMETRACE_IT=1 is set but ffmpeg was not found on PATH (T2: a requested integration run must not pass by skipping)"
        );
    };
    let work = unique_dir("ft_it_hik");
    std::fs::create_dir_all(&work).unwrap();
    let case_dir = work.join("case");
    let exe = Path::new(env!("CARGO_BIN_EXE_frametrace"));

    // MPEG program stream payload that FFmpeg can remux after the IMKH header.
    let mpg = work.join("raw.mpg");
    let output = Command::new(&ffmpeg)
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=160x90:r=5:d=1",
            "-c:v",
            "mpeg2video",
            "-f",
            "mpeg",
        ])
        .arg(&mpg)
        .output()
        .expect("run ffmpeg mpeg");
    assert!(output.status.success(), "ffmpeg mpeg failed");

    let mut imkh = vec![0u8; 40];
    imkh[0..4].copy_from_slice(b"IMKH");
    imkh.extend_from_slice(&std::fs::read(&mpg).unwrap());
    let hik_path = work.join("nvr_export_cam01.mpg");
    std::fs::write(&hik_path, &imkh).unwrap();

    let case_text = case_dir.to_string_lossy().to_string();
    run_binary(exe, &["init-case", &case_text, "--title", "IT hik"]);
    run_binary(
        exe,
        &[
            "export-hik",
            &case_text,
            &hik_path.to_string_lossy(),
            "--timeout",
            "60",
        ],
    );
    let export_log = read_file(&case_dir.join("artifacts/clips/export-log.jsonl"));
    assert!(export_log.contains("export-hik"), "{export_log}");
    let mp4 = case_dir.join("artifacts/clips/nvr_export_cam01.mp4");
    assert!(mp4.exists(), "remuxed mp4 missing");
    run_binary(
        exe,
        &["validate-artifact", &case_text, &mp4.to_string_lossy()],
    );
    let validation_log = read_file(&case_dir.join("evidence/logs/validation-log.jsonl"));
    assert!(
        validation_log.contains("ffprobe-video-stream-confirmed"),
        "{validation_log}"
    );

    let _ = std::fs::remove_dir_all(&work);
}

fn ewf_tool(name: &str) -> Option<PathBuf> {
    find_tool(name)
}

/// Real libewf round trip: acquire a raw image to E01 with ewfacquire, then
/// run the real import-e01 (ewfverify -> ewfexport -> raw -> SHA-256 chain).
/// With FRAMETRACE_IT=1 it FAILS when the tools are missing (it only skips when
/// FRAMETRACE_IT is unset); exclude it explicitly with `--skip e01_import` on
/// machines without libewf.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and libewf (ewfacquire/ewfinfo/ewfverify/ewfexport) on PATH"]
fn e01_import_roundtrip_with_real_libewf() {
    if !integration_enabled() {
        eprintln!("FRAMETRACE_IT=1 not set; skipping");
        return;
    }
    let (Some(ewfacquire), Some(_ewfinfo), Some(_ewfverify), Some(_ewfexport)) = (
        ewf_tool("ewfacquire"),
        ewf_tool("ewfinfo"),
        ewf_tool("ewfverify"),
        ewf_tool("ewfexport"),
    ) else {
        panic!(
            "FRAMETRACE_IT=1 is set but libewf tools (ewfacquire/ewfinfo/ewfverify/ewfexport) were not found on PATH (T2: a requested integration run must not pass by skipping)"
        );
    };

    let work = unique_dir("ft_it_e01");
    std::fs::create_dir_all(&work).unwrap();
    let case_dir = work.join("case");
    let exe = Path::new(env!("CARGO_BIN_EXE_frametrace"));

    // Raw "image" whose payload embeds a known marker; content does not need
    // a filesystem for the import path under test.
    let raw = work.join("source.raw");
    let mut payload = vec![0u8; 512 * 1024];
    payload[0..4].copy_from_slice(b"FTST");
    std::fs::write(&raw, &payload).unwrap();

    let e01 = work.join("acquired.E01");
    let acquire = Command::new(&ewfacquire)
        .args([
            "-u",
            "-C",
            "FT-CASE",
            "-D",
            "integration test image",
            "-e",
            "EV-1",
            "-E",
            "examiner",
            "-m",
            "fixed",
            "-S",
            "8MiB",
            "-t",
        ])
        .arg(e01.with_extension(""))
        .arg(&raw)
        .output()
        .expect("run ewfacquire");
    assert!(
        acquire.status.success(),
        "ewfacquire failed: {}",
        String::from_utf8_lossy(&acquire.stderr)
    );
    let e01_file = std::fs::read_dir(&work)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().map(|ext| ext == "E01").unwrap_or(false))
        .expect("ewfacquire produced no E01");

    let case_text = case_dir.to_string_lossy().to_string();
    run_binary(exe, &["init-case", &case_text, "--title", "IT e01"]);
    run_binary(
        exe,
        &[
            "import-e01",
            &case_text,
            &e01_file.to_string_lossy(),
            "--hash-e01",
        ],
    );

    // Import artifacts: verified E01 log, exported raw, matching hashes.
    let tsk_audit = read_file(&case_dir.join("evidence/logs/e01-audit.jsonl"));
    assert!(tsk_audit.contains("import-e01"), "{tsk_audit}");
    assert!(tsk_audit.contains("\"verified\":true"), "{tsk_audit}");
    let images = case_dir.join("evidence/images");
    let raw_out = std::fs::read_dir(&images)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().map(|ext| ext == "raw").unwrap_or(false))
        .expect("no exported raw image");
    let imported = std::fs::read(&raw_out).unwrap();
    assert_eq!(&imported[0..4], b"FTST", "exported raw payload mismatch");
    assert_eq!(imported.len(), payload.len());

    let _ = std::fs::remove_dir_all(&work);
}
