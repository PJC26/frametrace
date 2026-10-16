//! Real-ffmpeg integration tests for `join-video` and `measure-audio`.
//!
//! ```text
//! FRAMETRACE_IT=1 cargo test --locked --test join_audio_it -- --ignored
//! ```
//!
//! With `FRAMETRACE_IT=1` a missing ffmpeg/ffprobe FAILS the test (it never
//! silently skips); without it the ignored tests return early.

mod common;
use common::IsolatedCommand;

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn integration_enabled() -> bool {
    std::env::var("FRAMETRACE_IT").as_deref() == Ok("1")
}

fn find_tool(name: &str) -> Option<PathBuf> {
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
    (!first.is_empty()).then(|| PathBuf::from(first))
}

/// Returns false (skip) when IT is off; panics when IT is on but the
/// tools are missing.
fn require_tools() -> bool {
    if !integration_enabled() {
        eprintln!("FRAMETRACE_IT!=1: skipping real-ffmpeg join/audio test");
        return false;
    }
    for tool in ["ffmpeg", "ffprobe"] {
        assert!(
            find_tool(tool).is_some(),
            "FRAMETRACE_IT=1 requires {tool} on PATH"
        );
    }
    true
}

fn unique_dir(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("{name}_{}_{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn frametrace(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_frametrace"))
        .args(args)
        .isolated()
        .output()
        .expect("run frametrace")
}

fn ok(output: &Output, what: &str) -> String {
    assert!(
        output.status.success(),
        "{what} failed ({:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn last_json(stdout: &str) -> Value {
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON line in: {stdout}"));
    serde_json::from_str(line).unwrap()
}

/// lavfi clip: `audio` is a lavfi audio source spec or None for no track.
fn make_clip(path: &Path, seconds: f64, size: &str, audio: Option<&str>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut command = Command::new("ffmpeg");
    command.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
    ]);
    command.arg(format!("testsrc=size={size}:rate=30"));
    if let Some(audio) = audio {
        command.args(["-f", "lavfi", "-i", audio]);
    }
    command.args(["-t", &format!("{seconds}")]);
    command.args([
        "-c:v", "libx264", "-preset", "veryfast", "-g", "30", "-bf", "0", "-pix_fmt", "yuv420p",
    ]);
    if audio.is_some() {
        command.args(["-c:a", "aac", "-b:a", "96k"]);
    }
    command.arg(path);
    let output = command.output().expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const SINE: &str = "sine=frequency=440:sample_rate=48000";

fn new_case(root: &Path, source: &Path, hash: bool) -> PathBuf {
    let case = root.join("case");
    ok(
        &frametrace(&["init-case", case.to_str().unwrap(), "--title", "join-it"]),
        "init-case",
    );
    let mut args = vec![
        "scan-folder",
        case.to_str().unwrap(),
        source.to_str().unwrap(),
    ];
    if hash {
        args.push("--hash");
    }
    ok(&frametrace(&args), "scan-folder");
    case
}

/// relative_path file name → id.
fn ids_by_name(case: &Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(case.join("db/videos.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .map(|row| {
            let rel = row["relative_path"].as_str().unwrap().replace('\\', "/");
            let name = rel.rsplit('/').next().unwrap().to_string();
            (name, row["id"].as_str().unwrap().to_string())
        })
        .collect()
}

fn sha256_file(path: &Path) -> String {
    frametrace::audit::digest_file(path).unwrap()
}

fn probe_duration(path: &Path) -> f64 {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn join_video_success_with_overlap_and_gap() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_it");
    let source = root.join("src");
    let front = source.join("증거 '따옴표' 폴더").join("FRONT");
    // start offsets (s) / durations: 0/2, 2/3.5 (continuous), 4/2 (overlap
    // 1.5 s), 9/2 (gap 3 s), 11/2 (continuous).
    let layout = [
        ("20260910_120000_F.mp4", 2.0),
        ("20260910_120002_F.mp4", 3.5),
        ("20260910_120004_F.mp4", 2.0),
        ("20260910_120009_F.mp4", 2.0),
        ("20260910_120011_F.mp4", 2.0),
    ];
    // Distinct tones keep the files byte-distinct (identical bytes would
    // rightly be blocked as duplicate-file).
    for (i, (name, seconds)) in layout.iter().enumerate() {
        let tone = format!("sine=frequency={}:sample_rate=48000", 300 + 100 * i);
        make_clip(&front.join(name), *seconds, "320x240", Some(&tone));
    }
    let case = new_case(&root, &source, true);
    let ids_map = ids_by_name(&case);
    // Pass them out of order: the planner orders by file-name time.
    let ids: Vec<String> = layout
        .iter()
        .rev()
        .map(|(name, _)| ids_map[*name].clone())
        .collect();
    let ids_arg = ids.join(",");
    let case_str = case.to_str().unwrap();

    let check = last_json(&ok(
        &frametrace(&["join-video", case_str, "--ids", &ids_arg, "--check"]),
        "join-video --check",
    ));
    let plan = &check["plan"];
    assert_eq!(plan["order_basis"], "filename-time", "{plan:#}");
    assert_eq!(plan["blocking"].as_array().unwrap().len(), 0, "{plan:#}");
    assert_eq!(plan["totals"]["overlaps"], 1, "{plan:#}");
    assert_eq!(plan["totals"]["gaps"], 1, "{plan:#}");
    assert_eq!(plan["channel"]["code"], "F");
    assert_eq!(plan["container"], "mp4");
    assert_eq!(
        plan["segments"][0]["name"]
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .rsplit('/')
            .next(),
        Some("20260910_120000_F.mp4")
    );
    assert!(
        plan["segments"][0]["video"]["extradata_hash"]
            .as_str()
            .is_some_and(|h| h.starts_with("SHA256:")),
        "{plan:#}"
    );
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();

    // Wrong plan id → exit code 3, nothing written.
    let stale = frametrace(&[
        "join-video",
        case_str,
        "--ids",
        &ids_arg,
        "--plan-id",
        &"0".repeat(64),
        "--job-tag",
        "stale",
    ]);
    assert_eq!(
        stale.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );

    let result = last_json(&ok(
        &frametrace(&[
            "join-video",
            case_str,
            "--ids",
            &ids_arg,
            "--plan-id",
            &plan_id,
            "--job-tag",
            "it1",
        ]),
        "join-video",
    ));
    let output = PathBuf::from(result["output_path"].as_str().unwrap());
    let manifest_path = PathBuf::from(result["manifest_path"].as_str().unwrap());
    assert_eq!(
        output.file_name().unwrap().to_str().unwrap(),
        "join_F_20260910-120000_n5.mp4"
    );
    let sha = sha256_file(&output);
    assert_eq!(result["sha256"], sha.as_str());
    let manifest_text = std::fs::read_to_string(&manifest_path).unwrap();
    let manifest: Value = serde_json::from_str(&manifest_text).unwrap();
    assert_eq!(manifest["output"]["sha256"], sha.as_str());
    assert_eq!(manifest["totals"]["overlaps"], 1);
    assert_eq!(manifest["totals"]["gaps"], 1);
    assert_eq!(manifest["method"]["reencode"], false);
    assert_eq!(manifest["segments"].as_array().unwrap().len(), 5);
    assert_eq!(manifest["segments"][0]["hash_mismatch"], false);
    assert!(
        manifest["method"]["ffmpeg_argv"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a.as_str().unwrap().is_ascii())
    );
    let expected: f64 = manifest["duration_check"]["expected"].as_f64().unwrap();
    let observed = probe_duration(&output);
    assert!(
        (observed - expected).abs() <= (expected * 0.01).max(1.0),
        "probed {observed} vs expected {expected}"
    );
    assert_eq!(manifest["duration_check"]["ok"], true);

    let log = case.join("artifacts/joined/join-log.jsonl");
    let log_text = std::fs::read_to_string(&log).unwrap();
    let entry: Value = serde_json::from_str(log_text.lines().last().unwrap()).unwrap();
    assert_eq!(entry["event"], "join-video");
    assert_eq!(entry["output_sha256"], sha.as_str());
    assert_eq!(
        entry["manifest_sha256"].as_str().unwrap(),
        frametrace::sha256::digest_bytes(manifest_text.as_bytes())
    );
    assert_eq!(entry["segment_count"], 5);
    assert!(entry["duration_seconds"].as_f64().unwrap() > 10.0);
    // U2: the derived record id comes from the output bytes.
    let record_id = format!("join:{}", &sha[..16]);
    assert_eq!(entry["record_id"], record_id.as_str());
    assert_eq!(manifest["record_id"], record_id.as_str());
    // B2: every junction was judged on the file-name time basis.
    assert_eq!(entry["unknown_junctions"], 0);
    assert_eq!(entry["gaps"], 1);
    assert_eq!(manifest["totals"]["unknown_junctions"], 0);
    assert_eq!(
        log_text.lines().count(),
        1,
        "the stale attempt logged nothing"
    );
    ok(
        &frametrace(&["verify-audit", log.to_str().unwrap()]),
        "verify-audit",
    );

    // No temp leftovers.
    let leftovers: Vec<String> = std::fs::read_dir(case.join("artifacts/joined"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(".join-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn join_video_refuses_mismatched_streams() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_refuse_it");
    let source = root.join("src");
    make_clip(
        &source.join("20260910_130000_F.mp4"),
        2.0,
        "640x360",
        Some(SINE),
    );
    make_clip(
        &source.join("20260910_130002_F.mp4"),
        2.0,
        "1280x720",
        Some(SINE),
    );
    make_clip(&source.join("20260910_130004_F.mp4"), 2.0, "640x360", None);
    let case = new_case(&root, &source, false);
    let ids = ids_by_name(&case);
    let case_str = case.to_str().unwrap();
    let check = |a: &str, b: &str| -> Value {
        let arg = format!("{},{}", ids[a], ids[b]);
        last_json(&ok(
            &frametrace(&["join-video", case_str, "--ids", &arg, "--check"]),
            "join-video --check",
        ))["plan"]
            .clone()
    };
    let codes = |plan: &Value| -> Vec<String> {
        plan["blocking"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["code"].as_str().unwrap().to_string())
            .collect()
    };
    let resolution = check("20260910_130000_F.mp4", "20260910_130002_F.mp4");
    assert!(
        codes(&resolution).contains(&"stream-mismatch".to_string()),
        "{resolution:#}"
    );
    let audio = check("20260910_130000_F.mp4", "20260910_130004_F.mp4");
    assert!(
        codes(&audio).contains(&"audio-presence".to_string()),
        "{audio:#}"
    );

    // Running a blocked join fails and writes nothing.
    let arg = format!(
        "{},{}",
        ids["20260910_130000_F.mp4"], ids["20260910_130002_F.mp4"]
    );
    let run = frametrace(&[
        "join-video",
        case_str,
        "--ids",
        &arg,
        "--job-tag",
        "blocked",
    ]);
    assert!(!run.status.success());
    let joined = case.join("artifacts/joined");
    let written = std::fs::read_dir(&joined)
        .map(|d| d.flatten().count())
        .unwrap_or(0);
    assert_eq!(
        written,
        0,
        "blocked join must not write into {}",
        joined.display()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn measure_audio_classifies_sound_silence_and_absence() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_audio_it");
    let source = root.join("src");
    make_clip(&source.join("sound.mp4"), 2.0, "320x240", Some(SINE));
    make_clip(
        &source.join("silent.mp4"),
        2.0,
        "320x240",
        Some("anullsrc=r=48000:cl=mono"),
    );
    make_clip(&source.join("none.mp4"), 2.0, "320x240", None);
    // lavfi's sine peaks at -18 dBFS, so -50 dB lands near -67 dBFS: just
    // above the -70 dBFS silence threshold.
    make_clip(
        &source.join("quiet.mp4"),
        2.0,
        "320x240",
        Some("sine=frequency=440:sample_rate=48000,volume=-50dB"),
    );
    let case = new_case(&root, &source, true);
    let ids = ids_by_name(&case);
    let case_str = case.to_str().unwrap();

    let summary = last_json(&ok(
        &frametrace(&["measure-audio", case_str]),
        "measure-audio",
    ));
    assert_eq!(summary["measured"], 3, "{summary:#}");
    assert_eq!(summary["skipped_no_audio"], 1, "{summary:#}");
    assert_eq!(summary["log_entries"], 1, "{summary:#}");

    let levels = frametrace::audio_level::audio_levels_for_case(&case).unwrap();
    assert_eq!(levels[&ids["sound.mp4"]].r, "sound");
    assert_eq!(levels[&ids["silent.mp4"]].r, "silent");
    assert_eq!(
        levels[&ids["quiet.mp4"]].r, "sound",
        "{:?}",
        levels[&ids["quiet.mp4"]]
    );
    let quiet_max = levels[&ids["quiet.mp4"]].max.unwrap();
    assert!(quiet_max < -60.0 && quiet_max > -70.0, "{quiet_max}");
    assert!(!levels.contains_key(&ids["none.mp4"]));
    let sec = levels[&ids["sound.mp4"]].sec.unwrap();
    assert!((sec - 2.0).abs() < 0.2, "measured seconds {sec}");

    let log = case.join("evidence/logs/audio-level-log.jsonl");
    ok(
        &frametrace(&["verify-audit", log.to_str().unwrap()]),
        "verify-audit",
    );
    assert!(!case.join("db/audio-level-progress.jsonl").exists());

    // Second run skips the valid measurements; --all re-measures.
    let again = last_json(&ok(
        &frametrace(&["measure-audio", case_str]),
        "measure-audio again",
    ));
    assert_eq!(again["measured"], 0, "{again:#}");
    assert_eq!(again["skipped_valid"], 3, "{again:#}");
    let forced = last_json(&ok(
        &frametrace(&["measure-audio", case_str, "--all"]),
        "measure-audio --all",
    ));
    assert_eq!(forced["measured"], 3, "{forced:#}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Informational only: prints the measurement throughput, never fails on time.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn measure_audio_bench_info() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_audio_bench");
    let source = root.join("src");
    let count = 8;
    for i in 0..count {
        make_clip(
            &source.join(format!("b{i:02}.mp4")),
            10.0,
            "1280x720",
            Some(SINE),
        );
    }
    let case = new_case(&root, &source, false);
    let started = Instant::now();
    ok(
        &frametrace(&["measure-audio", case.to_str().unwrap()]),
        "measure-audio bench",
    );
    let elapsed = started.elapsed().as_secs_f64();
    eprintln!(
        "measure-audio bench: {count} x 10 s 720p files in {elapsed:.2}s ({:.3}s/file)",
        elapsed / count as f64
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// B2: a join ordered by file name only records its gaps/overlaps as
/// unknown (null + `unknown_junctions`), never as zero.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn join_video_filename_order_records_unknown_junctions() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_order_it");
    let source = root.join("src");
    for (i, name) in ["clip_a1.mp4", "clip_a2.mp4", "clip_a3.mp4"]
        .iter()
        .enumerate()
    {
        let tone = format!("sine=frequency={}:sample_rate=48000", 500 + 100 * i);
        make_clip(&source.join(name), 2.0, "320x240", Some(&tone));
    }
    let case = new_case(&root, &source, false);
    let ids_map = ids_by_name(&case);
    let ids_arg = ["clip_a1.mp4", "clip_a2.mp4", "clip_a3.mp4"]
        .iter()
        .map(|n| ids_map[*n].clone())
        .collect::<Vec<_>>()
        .join(",");
    let case_str = case.to_str().unwrap();
    let result = last_json(&ok(
        &frametrace(&[
            "join-video",
            case_str,
            "--ids",
            &ids_arg,
            "--job-tag",
            "order",
        ]),
        "join-video filename-order",
    ));
    assert!(
        result["summary_text"]
            .as_str()
            .unwrap()
            .contains("끊김·겹침 확인 불가"),
        "{result:#}"
    );
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(result["manifest_path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["order"]["basis"], "filename-order");
    assert!(manifest["totals"]["gaps"].is_null(), "{manifest:#}");
    assert!(manifest["totals"]["overlaps"].is_null(), "{manifest:#}");
    assert_eq!(manifest["totals"]["unknown_junctions"], 2);
    let log = std::fs::read_to_string(case.join("artifacts/joined/join-log.jsonl")).unwrap();
    let entry: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert!(
        entry["gaps"].is_null() && entry["overlaps"].is_null(),
        "{entry:#}"
    );
    assert_eq!(entry["unknown_junctions"], 2);
    let _ = std::fs::remove_dir_all(&root);
}

/// B1: a silent track whose file was truncated (moov intact, mdat cut) is
/// an ERROR, never "silent" — the decoded length falls short of the probed
/// stream duration. `measure-audio` then exits 4 (B15) and says how many
/// files errored; an intact silent file in the same run stays silent.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn measure_audio_truncated_track_is_an_error_not_silent() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_audio_trunc_it");
    let source = root.join("src");
    std::fs::create_dir_all(&source).unwrap();
    let faststart_silent = |path: &Path| {
        let output = Command::new("ffmpeg")
            .args(["-y", "-hide_banner", "-loglevel", "error"])
            .args(["-f", "lavfi", "-i", "testsrc=size=320x240:rate=30"])
            .args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono"])
            .args([
                "-t", "10", "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p",
            ])
            .args(["-c:a", "aac", "-b:a", "96k", "-movflags", "+faststart"])
            .arg(path)
            .output()
            .expect("run ffmpeg");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let truncated = source.join("truncated.mp4");
    let intact = source.join("intact.mp4");
    faststart_silent(&truncated);
    faststart_silent(&intact);
    // Cut the media data in half; the moov (at the front) still describes
    // the full 10 s.
    let len = std::fs::metadata(&truncated).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&truncated)
        .unwrap()
        .set_len(len / 2)
        .unwrap();
    let case = new_case(&root, &source, false);
    let ids = ids_by_name(&case);
    let index = std::fs::read_to_string(case.join("db/videos.jsonl")).unwrap();
    assert!(
        index
            .lines()
            .any(|l| l.contains("truncated.mp4") && l.contains("\"audio_codec\":\"aac\"")),
        "truncated file must still probe with its audio track: {index}"
    );

    let run = frametrace(&["measure-audio", case.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_eq!(run.status.code(), Some(4), "stderr: {stderr}");
    assert!(stderr.contains("소리 측정 오류 1건"), "{stderr}");
    let summary = last_json(&String::from_utf8_lossy(&run.stdout));
    assert_eq!(summary["error"], 1, "{summary:#}");
    assert_eq!(summary["silent"], 1, "{summary:#}");

    let levels = frametrace::audio_level::audio_levels_for_case(&case).unwrap();
    assert_eq!(
        levels[&ids["truncated.mp4"]].r, "error",
        "{:?}",
        levels[&ids["truncated.mp4"]]
    );
    assert_eq!(levels[&ids["intact.mp4"]].r, "silent");
    let log = case.join("evidence/logs/audio-level-log.jsonl");
    ok(
        &frametrace(&["verify-audit", log.to_str().unwrap()]),
        "verify-audit",
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Real ffmpeg concat (stream copy) of `inputs` into `output`.
fn concat_copy(inputs: &[&Path], output: &Path) {
    let dir = output.parent().unwrap();
    let list = dir.join(format!(
        "{}.ffconcat",
        output.file_name().unwrap().to_string_lossy()
    ));
    let mut text = String::from("ffconcat version 1.0\n");
    for input in inputs {
        text.push_str(&format!(
            "file '{}'\n",
            input.to_string_lossy().replace('\\', "/")
        ));
    }
    std::fs::write(&list, text).unwrap();
    let status = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "concat"])
        .args(["-safe", "0", "-i"])
        .arg(&list)
        .args(["-map", "0:v:0", "-map", "0:a?", "-c", "copy"])
        .args(["-avoid_negative_ts", "make_zero", "-f", "mp4"])
        .arg(output)
        .status()
        .unwrap();
    assert!(status.success());
}

/// C14: the packet-level segment check passes a complete stream-copy join
/// and FAILS one whose middle segment is missing — a 2.2 s hole in a
/// 6.6 s join that the old duration tolerance (max(1 s, 1 %)) let through
/// on long joins, and that the new max(1 s, 0.2 %) bound plus the
/// per-segment packet / boundary check refuse.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn join_segment_check_catches_a_missing_segment() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_c14");
    let source = root.join("src");
    let names = [
        ("20260910_120000_F.mp4", 2.1),
        ("20260910_120002_F.mp4", 2.2),
        ("20260910_120004_F.mp4", 2.3),
    ];
    for (i, (name, seconds)) in names.iter().enumerate() {
        let tone = format!("sine=frequency={}:sample_rate=48000", 400 + 100 * i);
        make_clip(&source.join(name), *seconds, "160x120", Some(&tone));
    }
    let case = new_case(&root, &source, false);
    let ids_map = ids_by_name(&case);
    let ids: Vec<String> = names.iter().map(|(n, _)| ids_map[*n].clone()).collect();
    let plan = frametrace::join::plan(&case, &ids, 1.0).unwrap();
    assert!(plan.blocking.is_empty(), "{:?}", plan.blocking);
    let paths: Vec<PathBuf> = plan
        .segments
        .iter()
        .map(|s| s.source_path.clone())
        .collect();
    let full = root.join("full.mp4");
    concat_copy(&[&paths[0], &paths[1], &paths[2]], &full);
    let check = frametrace::join::verify_segments("ffprobe", "ffmpeg", &full, &plan, None).unwrap();
    assert!(check.ok, "{:?}", check.problems);
    assert_eq!(check.video_packets_expected, check.video_packets_observed);

    let missing = root.join("missing.mp4");
    concat_copy(&[&paths[0], &paths[2]], &missing);
    let check =
        frametrace::join::verify_segments("ffprobe", "ffmpeg", &missing, &plan, None).unwrap();
    assert!(!check.ok, "a missing segment must fail the check");
    assert!(
        check.video_packets_observed < check.video_packets_expected,
        "{check:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// C13: every join-log entry carries the job tag that wrote it (the
/// server attributes entries by this tag only).
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn join_log_entry_carries_the_job_tag() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_c13");
    let source = root.join("src");
    for (i, name) in ["20260910_120000_F.mp4", "20260910_120002_F.mp4"]
        .iter()
        .enumerate()
    {
        let tone = format!("sine=frequency={}:sample_rate=48000", 500 + 100 * i);
        make_clip(&source.join(name), 2.0, "160x120", Some(&tone));
    }
    let case = new_case(&root, &source, false);
    let ids: Vec<String> = ids_by_name(&case).values().cloned().collect();
    let ids_arg = ids.join(",");
    let case_str = case.to_str().unwrap();
    let result = last_json(&ok(
        &frametrace(&[
            "join-video",
            case_str,
            "--ids",
            &ids_arg,
            "--job-tag",
            "srv-c13-1",
        ]),
        "join-video",
    ));
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(result["manifest_path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["segment_check"]["ok"], true, "{manifest:#}");
    assert!(
        manifest["duration_check"]["tolerance_seconds"]
            .as_f64()
            .is_some()
    );
    let log = std::fs::read_to_string(case.join("artifacts/joined/join-log.jsonl")).unwrap();
    let entry: Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(entry["job_tag"], "srv-c13-1");
    assert_eq!(entry["segment_check_ok"], true);
    let _ = std::fs::remove_dir_all(&root);
}

/// Runs ffmpeg with `args` (fixture generation); panics with its stderr.
fn ffmpeg_fixture(args: &[&str]) {
    let output = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .args(args)
        .output()
        .expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// An H.264 test clip: `seconds` long, GOP `gop`, `bframes` B-frames,
/// `audio` codec with tone `tone` Hz.
fn bframe_clip(path: &Path, seconds: &str, gop: &str, bframes: &str, audio: &str, tone: u32) {
    ffmpeg_fixture(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=160x120:rate=30",
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency={tone}:sample_rate=48000"),
        "-t",
        seconds,
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-g",
        gop,
        "-bf",
        bframes,
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        audio,
        path.to_str().unwrap(),
    ]);
}

/// Video packet field values of `path` as ffprobe lists them.
fn packet_fields(path: &Path, field: &str) -> String {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args([
            "-show_entries",
            &format!("packet={field}"),
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Joins every indexed file of `case` with the real CLI; returns the
/// result JSON and the manifest.
fn join_all(case: &Path, tag: &str) -> (Value, Value) {
    let ids: Vec<String> = ids_by_name(case).values().cloned().collect();
    let ids_arg = ids.join(",");
    let result = last_json(&ok(
        &frametrace(&[
            "join-video",
            case.to_str().unwrap(),
            "--ids",
            &ids_arg,
            "--job-tag",
            tag,
        ]),
        "join-video",
    ));
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(result["manifest_path"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    (result, manifest)
}

/// R5-S13: Matroska sources with B-frames (and PCM audio, so the output is
/// Matroska too). ffprobe reports no dts (`N/A`) for the first packets of
/// such a stream and decode order is not presentation order; the segment
/// check now locates each segment by its earliest presentation time.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn join_bframe_mkv_sources_pass_the_segment_check() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_s13_mkv");
    let source = root.join("src");
    std::fs::create_dir_all(&source).unwrap();
    let names = [
        ("20260910_120000_F.mkv", "2", 400),
        ("20260910_120002_F.mkv", "2.5", 500),
        ("20260910_120004_F.mkv", "2", 600),
    ];
    for (name, seconds, tone) in names {
        bframe_clip(&source.join(name), seconds, "30", "3", "pcm_s16le", tone);
    }
    // The fixture really has what the check must cope with.
    assert!(
        packet_fields(&source.join(names[0].0), "dts_time").contains("N/A"),
        "fixture without N/A dts"
    );
    let case = new_case(&root, &source, false);
    let (result, manifest) = join_all(&case, "s13-mkv");
    assert!(
        result["output_path"].as_str().unwrap().ends_with(".mkv"),
        "{result}"
    );
    let check = &manifest["segment_check"];
    assert_eq!(check["ok"], true, "{check:#}");
    let boundaries = check["boundaries"].as_array().unwrap();
    assert_eq!(boundaries.len(), 3);
    for boundary in boundaries {
        let expected = boundary["expected_start_seconds"].as_f64().unwrap();
        let observed = boundary["observed_start_seconds"].as_f64().unwrap();
        assert!(
            (observed - expected).abs() < 0.2,
            "boundary off: {boundary}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// R5-S13: an MP4 cut by stream copy at a non-key frame carries an edit
/// list — its first 1.5 s of packets are flagged discard. The concat
/// output keeps them as visible frames, so the output is 1.5 s longer and
/// every later segment starts 1.5 s later than the plan's sum of
/// durations. The old check refused this complete join ("출력 길이 …
/// 다릅니다", "2번째 원본이 … 시작하지 않습니다"); now the pre-roll is
/// measured, accounted for and stated in the manifest.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn join_accepts_an_edit_list_mp4_source() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_s13_elst");
    let source = root.join("src");
    std::fs::create_dir_all(&source).unwrap();
    let long = root.join("long.mp4");
    bframe_clip(&long, "4", "120", "2", "aac", 300);
    let cut = source.join("20260910_120000_F.mp4");
    ffmpeg_fixture(&[
        "-ss",
        "1.5",
        "-i",
        long.to_str().unwrap(),
        "-c",
        "copy",
        cut.to_str().unwrap(),
    ]);
    // The fixture really has discarded (edit-list) packets.
    assert!(
        packet_fields(&cut, "flags").contains('D'),
        "fixture without an edit list"
    );
    for (i, name) in ["20260910_120003_F.mp4", "20260910_120005_F.mp4"]
        .iter()
        .enumerate()
    {
        bframe_clip(
            &source.join(name),
            "2",
            "30",
            "2",
            "aac",
            700 + 100 * i as u32,
        );
    }
    let case = new_case(&root, &source, false);
    let (_result, manifest) = join_all(&case, "s13-elst");
    let check = &manifest["segment_check"];
    assert_eq!(check["ok"], true, "{check:#}");
    assert_eq!(manifest["duration_check"]["ok"], true, "{manifest:#}");
    let presented: f64 = check["boundaries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["presented_edit_list_preroll_seconds"].as_f64().unwrap())
        .sum();
    match check["mode"].as_str() {
        // The output kept the pre-roll frames: measured and stated.
        Some("all-packets") if presented > 0.5 => {
            assert!((presented - 1.5).abs() < 0.2, "{check:#}");
            let caveats = manifest["caveats"].to_string();
            assert!(caveats.contains("편집 목록"), "{caveats}");
        }
        // An ffmpeg that applies the edit list: nothing presented.
        Some("all-packets") | Some("edit-list-applied") => {}
        other => panic!("unexpected mode {other:?}: {check:#}"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Real ffmpeg concat (stream copy, as `join-video` runs it) of `inputs`
/// into `output` with muxer `format`.
fn concat_to(inputs: &[&Path], output: &Path, format: &str) {
    let list = output.with_extension("ffconcat");
    let mut text = String::from("ffconcat version 1.0\n");
    for input in inputs {
        text.push_str(&format!(
            "file '{}'\n",
            input.to_string_lossy().replace('\\', "/")
        ));
    }
    std::fs::write(&list, text).unwrap();
    let list_arg = list.to_str().unwrap().to_string();
    let output_arg = output.to_str().unwrap().to_string();
    ffmpeg_fixture(&[
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        &list_arg,
        "-map",
        "0:v:0",
        "-c",
        "copy",
        "-avoid_negative_ts",
        "make_zero",
        "-f",
        format,
        &output_arg,
    ]);
}

/// Rewrites the first (version 0) `elst` entry of an MP4 so its media time
/// starts half a frame after frame 3 of a 10 fps stream: the demuxer then
/// flags frame 3 discard (pre-roll) and drops frames 0-2.
fn patch_edit_list(path: &Path) {
    let mut bytes = std::fs::read(path).unwrap();
    let find = |bytes: &[u8], tag: &[u8]| bytes.windows(4).position(|w| w == tag).unwrap();
    let mdhd = find(&bytes, b"mdhd");
    assert_eq!(bytes[mdhd + 4], 0, "mdhd version 0 expected");
    let timescale = u32::from_be_bytes(bytes[mdhd + 16..mdhd + 20].try_into().unwrap());
    let elst = find(&bytes, b"elst");
    assert_eq!(bytes[elst + 4], 0, "elst version 0 expected");
    let duration = u32::from_be_bytes(bytes[elst + 12..elst + 16].try_into().unwrap());
    let frame = timescale / 10;
    bytes[elst + 12..elst + 16].copy_from_slice(&(duration - 350).to_be_bytes());
    bytes[elst + 16..elst + 20].copy_from_slice(&(3 * frame + frame / 2).to_be_bytes());
    std::fs::write(path, bytes).unwrap();
}

/// R9-S2 (real ffmpeg): an all-intra H.264 source with an edit list.
/// Stream copy rewrites every IDR frame - the concat demuxer converts to
/// Annex B and parameter sets are re-inserted; an MPEG-TS output is Annex
/// B with access unit delimiters - so NO output packet has a source
/// payload. R8 took exactly that for "every key frame rewritten" and
/// accepted on count and timing alone, so an output whose segment was
/// shifted by one frame (pre-roll frame kept, last real frame lost: same
/// count) passed. With normalized payload hashes (parameter sets and AUDs
/// removed, Annex B framing) the correct output matches position by
/// position in MP4, Matroska and MPEG-TS, and the shifted one is refused.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn all_intra_edit_list_output_is_compared_frame_by_frame() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_r9_s2");
    let plain = root.join("plain.mp4");
    ffmpeg_fixture(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=160x120:rate=10",
        "-t",
        "2",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-g",
        "1",
        "-pix_fmt",
        "yuv420p",
        plain.to_str().unwrap(),
    ]);
    let edited = root.join("edited.mp4");
    ffmpeg_fixture(&[
        "-i",
        plain.to_str().unwrap(),
        "-c",
        "copy",
        "-use_editlist",
        "1",
        edited.to_str().unwrap(),
    ]);
    patch_edit_list(&edited);
    let flags = packet_fields(&edited, "flags");
    assert!(
        flags.lines().next().is_some_and(|f| f.contains('D')),
        "fixture without edit-list pre-roll: {flags}"
    );
    assert!(
        flags.lines().all(|f| f.contains('K')),
        "not all-intra: {flags}"
    );
    // What an ffmpeg that applies the edit list produces: frames 4..19.
    let applied_part = root.join("applied.mp4");
    ffmpeg_fixture(&[
        "-i",
        plain.to_str().unwrap(),
        "-ss",
        "0.4",
        "-c",
        "copy",
        applied_part.to_str().unwrap(),
    ]);
    // The pre-roll frame kept and the last real frame lost: frames 3..18.
    let shifted_part = root.join("shifted.mp4");
    ffmpeg_fixture(&[
        "-i",
        plain.to_str().unwrap(),
        "-ss",
        "0.3",
        "-frames:v",
        "16",
        "-c",
        "copy",
        shifted_part.to_str().unwrap(),
    ]);
    let sources = [edited.clone(), plain.clone()];
    let offsets = [0.0, 1.6];
    let check = |output: &Path| {
        frametrace::join::verify_segments_with(
            &sources,
            &offsets,
            output,
            &mut |path, hashes| {
                if hashes {
                    frametrace::join::normalized_video_packets("ffprobe", "ffmpeg", path, None)
                } else {
                    frametrace::join::video_packets("ffprobe", path, None, false)
                }
            },
            &mut |path| frametrace::join::stream_packet_counts("ffprobe", path, None),
        )
        .unwrap()
    };
    let raw = |path: &Path| -> Vec<u64> {
        frametrace::join::video_packets("ffprobe", path, None, true)
            .unwrap()
            .iter()
            .map(|p| p.hash.unwrap())
            .collect()
    };
    let source_raw: std::collections::HashSet<u64> =
        raw(&edited).into_iter().chain(raw(&plain)).collect();
    for (format, ext) in [("mp4", "mp4"), ("matroska", "mkv"), ("mpegts", "ts")] {
        let good = root.join(format!("out-good.{ext}"));
        concat_to(&[&applied_part, &plain], &good, format);
        // Premise: stream copy rewrote every packet (no raw payload of
        // any source survives) - R8's "rewritten" escape.
        assert!(
            raw(&good).iter().all(|h| !source_raw.contains(h)),
            "{format}: some packets were copied byte for byte"
        );
        let verdict = check(&good);
        assert!(verdict.ok, "{format}: {:?}", verdict.problems);
        assert_eq!(verdict.mode, "edit-list-applied", "{format}");
        let bad = root.join(format!("out-shifted.{ext}"));
        concat_to(&[&shifted_part, &plain], &bad, format);
        assert!(raw(&bad).iter().all(|h| !source_raw.contains(h)));
        let verdict = check(&bad);
        assert!(!verdict.ok, "{format}: a one-frame shift passed");
        assert!(
            verdict
                .problems
                .iter()
                .any(|p| p.contains("유지 프레임") && p.contains("1번째 프레임부터")),
            "{format}: {:?}",
            verdict.problems
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// [`concat_to`] with `-copyinkf`, as `join-video` runs it since R10-S8.
fn concat_keep_leading(inputs: &[&Path], output: &Path, format: &str) {
    let list = output.with_extension("ffconcat");
    let mut text = String::from("ffconcat version 1.0\n");
    for input in inputs {
        text.push_str(&format!(
            "file '{}'\n",
            input.to_string_lossy().replace('\\', "/")
        ));
    }
    std::fs::write(&list, text).unwrap();
    let list_arg = list.to_str().unwrap().to_string();
    let output_arg = output.to_str().unwrap().to_string();
    ffmpeg_fixture(&[
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        &list_arg,
        "-map",
        "0:v:0",
        "-c",
        "copy",
        "-copyinkf",
        "-avoid_negative_ts",
        "make_zero",
        "-f",
        format,
        &output_arg,
    ]);
}

/// A 10 fps long-GOP (key frame every 10) H.264 clip without B-frames.
fn long_gop_clip(path: &Path, seconds: &str) {
    ffmpeg_fixture(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=160x120:rate=10",
        "-t",
        seconds,
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-g",
        "10",
        "-bf",
        "0",
        "-pix_fmt",
        "yuv420p",
        path.to_str().unwrap(),
    ]);
}

/// Stream copy of `input` from `start` seconds that keeps the non-key
/// frames before the next key frame (`-copyinkf`): a clip that starts
/// mid-GOP, as a CCTV export or a cut file does. `frames` limits the
/// video frame count.
fn cut_mid_gop(input: &Path, start: &str, frames: Option<&str>, output: &Path) {
    let mut args = vec!["-i", input.to_str().unwrap(), "-ss", start];
    if let Some(frames) = frames {
        args.extend(["-frames:v", frames]);
    }
    args.extend(["-c", "copy", "-copyinkf", output.to_str().unwrap()]);
    ffmpeg_fixture(&args);
}

/// R10-S8 (real ffmpeg, real `join-video`): a first source that starts on
/// a non-key frame. Stream copy dropped every frame before its first key
/// frame (no `-copyinkf`), so the output was short by those frames and
/// the segment check refused a join that had lost material.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn join_video_keeps_a_non_keyframe_start_source() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_r10_s8_join");
    let source = root.join("src");
    std::fs::create_dir_all(&source).unwrap();
    let long = root.join("long.mp4");
    long_gop_clip(&long, "3");
    let first = source.join("20260910_120000_F.mp4");
    cut_mid_gop(&long, "0.3", None, &first);
    let flags = packet_fields(&first, "flags");
    assert!(
        flags.lines().next().is_some_and(|f| !f.contains('K')),
        "fixture does not start on a non-key frame: {flags}"
    );
    assert!(!flags.contains('D'), "fixture has an edit list: {flags}");
    let first_packets = flags.lines().count() as u64;
    long_gop_clip(&source.join("20260910_120003_F.mp4"), "2");
    let case = new_case(&root, &source, false);
    let (_result, manifest) = join_all(&case, "r10-s8");
    let check = &manifest["segment_check"];
    assert_eq!(check["ok"], true, "{check:#}");
    assert_eq!(
        check["video_packets_observed"].as_u64(),
        Some(first_packets + 20),
        "{check:#}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// R10-S8 (real ffmpeg): a source that starts on a non-key frame AND has
/// an edit list (its first packet is pre-roll). Its kept frames are
/// compared by normalized payload hash, which R9 computed with stream
/// copy WITHOUT `-copyinkf`: ffmpeg hashed fewer packets than ffprobe
/// listed and the check could not run at all ("hashed N but lists M"),
/// so every such join was refused. Now the correct output passes in MP4,
/// Matroska and MPEG-TS and a one-frame shift (same count) is refused.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn non_keyframe_start_edit_list_output_is_compared_frame_by_frame() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_r10_s8");
    let plain = root.join("plain.mp4");
    long_gop_clip(&plain, "3");
    // Frames 2..29 of `plain`: starts mid-GOP (key frame at frame 10).
    let midgop = root.join("midgop.mp4");
    cut_mid_gop(&plain, "0.2", None, &midgop);
    let edited = root.join("edited.mp4");
    ffmpeg_fixture(&[
        "-i",
        midgop.to_str().unwrap(),
        "-c",
        "copy",
        "-copyinkf",
        "-use_editlist",
        "1",
        edited.to_str().unwrap(),
    ]);
    patch_edit_list(&edited);
    // The edit list drops frames 0-2 of `midgop` and flags frame 3 (a
    // non-key frame) pre-roll: the source starts on a non-key frame.
    let flags = packet_fields(&edited, "flags");
    let first = flags.lines().next().unwrap_or_default();
    assert!(
        first.contains('D') && !first.contains('K'),
        "fixture without non-key pre-roll: {flags}"
    );
    let kept = flags.lines().filter(|f| !f.contains('D')).count();
    assert!(
        flags
            .lines()
            .find(|f| !f.contains('D'))
            .is_some_and(|f| !f.contains('K')),
        "the first kept frame is a key frame: {flags}"
    );
    // What an ffmpeg that applies the edit list produces: the kept frames
    // (frames 6.. of `plain`), and a one-frame shift of them (frames 5..,
    // the last kept frame lost: same count).
    let applied_part = root.join("applied.mp4");
    cut_mid_gop(&plain, "0.6", Some(&kept.to_string()), &applied_part);
    let shifted_part = root.join("shifted.mp4");
    cut_mid_gop(&plain, "0.5", Some(&kept.to_string()), &shifted_part);
    let sources = [edited.clone(), plain.clone()];
    let offsets = [0.0, kept as f64 / 10.0];
    let check = |output: &Path| {
        frametrace::join::verify_segments_with(
            &sources,
            &offsets,
            output,
            &mut |path, hashes| {
                if hashes {
                    frametrace::join::normalized_video_packets("ffprobe", "ffmpeg", path, None)
                } else {
                    frametrace::join::video_packets("ffprobe", path, None, false)
                }
            },
            &mut |path| frametrace::join::stream_packet_counts("ffprobe", path, None),
        )
        .expect("the check runs on a non-key-frame start")
    };
    for (format, ext) in [("mp4", "mp4"), ("matroska", "mkv"), ("mpegts", "ts")] {
        let good = root.join(format!("out-good.{ext}"));
        concat_keep_leading(&[&applied_part, &plain], &good, format);
        let verdict = check(&good);
        assert!(verdict.ok, "{format}: {:?}", verdict.problems);
        assert_eq!(verdict.mode, "edit-list-applied", "{format}");
        let bad = root.join(format!("out-shifted.{ext}"));
        concat_keep_leading(&[&shifted_part, &plain], &bad, format);
        let verdict = check(&bad);
        assert!(!verdict.ok, "{format}: a one-frame shift passed");
        assert!(
            verdict.problems.iter().any(|p| p.contains("유지 프레임")),
            "{format}: {:?}",
            verdict.problems
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// R10-S8 (real ffmpeg): packets that carry ONLY parameter sets (SPS/PPS
/// as packets of their own, as some DVRs write them). The normalizing
/// filter removes their whole payload and drops them, so R9 hashed fewer
/// packets than ffprobe listed and refused the file. They are now proven
/// parameter-set-only and hashed as the empty payload; every other
/// packet keeps the same normalized hash it has in the plain file.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn parameter_set_only_packets_are_accounted_for() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_r10_s8_ps");
    let plain_mp4 = root.join("plain.mp4");
    long_gop_clip(&plain_mp4, "3");
    let plain = root.join("plain.mkv");
    ffmpeg_fixture(&[
        "-i",
        plain_mp4.to_str().unwrap(),
        "-c",
        "copy",
        plain.to_str().unwrap(),
    ]);
    // Packets of only SPS/PPS (one per key frame of `plain`) ...
    let ps = root.join("ps.mkv");
    ffmpeg_fixture(&[
        "-i",
        plain_mp4.to_str().unwrap(),
        "-c",
        "copy",
        "-bsf:v",
        "h264_mp4toannexb,filter_units=pass_types=7|8",
        "-f",
        "matroska",
        ps.to_str().unwrap(),
    ]);
    // ... followed by the real stream, in one file.
    let mixed = root.join("mixed.mkv");
    concat_keep_leading(&[&ps, &plain], &mixed, "matroska");
    let ps_count = packet_fields(&ps, "size").lines().count();
    assert!(ps_count >= 2, "no parameter-set packets made");
    let listed = packet_fields(&mixed, "size").lines().count();
    assert_eq!(listed, ps_count + 30, "fixture packet count");
    let mixed_packets =
        frametrace::join::normalized_video_packets("ffprobe", "ffmpeg", &mixed, None)
            .expect("parameter-set-only packets are accounted for");
    let plain_packets =
        frametrace::join::normalized_video_packets("ffprobe", "ffmpeg", &plain, None).unwrap();
    assert_eq!(mixed_packets.len(), listed);
    assert!(
        mixed_packets[..ps_count]
            .iter()
            .all(|p| p.hash == Some(frametrace::join::EMPTY_PAYLOAD_HASH))
    );
    let rest: Vec<Option<u64>> = mixed_packets[ps_count..].iter().map(|p| p.hash).collect();
    let expected: Vec<Option<u64>> = plain_packets.iter().map(|p| p.hash).collect();
    assert_eq!(rest, expected, "picture packets hash as in the plain file");
    let _ = std::fs::remove_dir_all(&root);
}

/// R11-S1: a 10 fps H.264 clip with `bframes` B-frames (key frame every
/// 10): decode order is not presentation order, so pts ≠ dts.
fn bframe_gop_clip(path: &Path, seconds: &str, bframes: &str) {
    ffmpeg_fixture(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=160x120:rate=10",
        "-t",
        seconds,
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-g",
        "10",
        "-bf",
        bframes,
        "-pix_fmt",
        "yuv420p",
        path.to_str().unwrap(),
    ]);
}

/// R11-S1: stream copy of `input` through the `noise` filter, dropping
/// the packets for which `drop` (an ffmpeg expression over `pts`, `n`…)
/// is non-zero — the exact packet set an edit-list-applying muxer keeps.
/// `extra`: muxer options before the output.
fn drop_packets(input: &Path, drop: &str, extra: &[&str], output: &Path) {
    let filter = format!("noise=drop={}", drop.replace(',', "\\,"));
    let mut args = vec![
        "-copyts",
        "-i",
        input.to_str().unwrap(),
        "-map",
        "0:v:0",
        "-c",
        "copy",
        "-copyinkf",
        "-bsf:v",
        &filter,
        // A kept pre-roll frame presents before the first kept frame:
        // without the shift Matroska cannot store its (negative) pts.
        "-avoid_negative_ts",
        "make_zero",
    ];
    args.extend_from_slice(extra);
    args.push(output.to_str().unwrap());
    ffmpeg_fixture(&args);
}

/// `pts` per video packet of `path` (`None` for `N/A`), file order.
fn packet_pts(path: &Path) -> Vec<Option<i64>> {
    packet_fields(path, "pts")
        .lines()
        .map(|l| l.trim().trim_end_matches(',').parse().ok())
        .collect()
}

/// R11-S1 (real ffmpeg): B-frame sources whose normalized payload hashes
/// need the timestamp alignment of R10-S8 (packets of only parameter
/// sets). ffprobe prints `pts,dts` in that order whatever order
/// `-show_entries` names them in, and R10 read the pair as `(dts, pts)`:
/// with B-frames the two differ, every packet "differed from ffmpeg's
/// timestamps" and every such join was refused as unverifiable. In
/// Matroska ffprobe also reports no dts (`N/A`) for the first packets
/// while ffmpeg's framehash computes one. Now the correct join passes in
/// MP4 and Matroska output (`-bf 2` and `-bf 3`) and a one-frame shift
/// with the same packet count is refused frame by frame. (MPEG-TS cannot
/// carry a parameter-set-only packet — its demuxer folds it into the next
/// access unit — so TS is covered by the timestamp test below.)
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn bframe_parameter_set_packets_join_is_compared_frame_by_frame() {
    if !require_tools() {
        return;
    }
    let mut passed: Vec<String> = Vec::new();
    let mut unverifiable: Vec<String> = Vec::new();
    for bframes in ["2", "3"] {
        let root = unique_dir(&format!("ft_join_r11_s1_bf{bframes}"));
        let plain = root.join("plain.mp4");
        bframe_gop_clip(&plain, "3", bframes);
        let plain_mkv = root.join("plain.mkv");
        ffmpeg_fixture(&[
            "-i",
            plain.to_str().unwrap(),
            "-c",
            "copy",
            plain_mkv.to_str().unwrap(),
        ]);
        let ps = root.join("ps.mkv");
        ffmpeg_fixture(&[
            "-i",
            plain.to_str().unwrap(),
            "-c",
            "copy",
            "-bsf:v",
            "h264_mp4toannexb,filter_units=pass_types=7|8",
            "-f",
            "matroska",
            ps.to_str().unwrap(),
        ]);
        // Source 1: the B-frame stream, parameter-set-only packets, the
        // stream again. `joined`: the same followed by source 2 (`plain`)
        // — the material of the output, laid out by clean key-frame joins.
        let mixed = root.join("mixed.mp4");
        concat_keep_leading(&[&plain_mkv, &ps, &plain_mkv], &mixed, "mp4");
        let joined = root.join("joined.mp4");
        concat_keep_leading(&[&plain_mkv, &ps, &plain_mkv, &plain_mkv], &joined, "mp4");
        // Cut mid-GOP by stream copy: an edit list whose pre-roll
        // (discard) packets are interleaved with kept ones in decode
        // order, as B-frames put them.
        let cut = |input: &Path, output: &Path| {
            ffmpeg_fixture(&[
                "-ss",
                "1.5",
                "-i",
                input.to_str().unwrap(),
                "-c",
                "copy",
                output.to_str().unwrap(),
            ])
        };
        let edited = root.join("edited.mp4");
        cut(&mixed, &edited);
        let edited_joined = root.join("edited-joined.mp4");
        cut(&joined, &edited_joined);
        let flags: Vec<String> = packet_fields(&edited, "flags")
            .lines()
            .map(str::to_string)
            .collect();
        let pts = packet_pts(&edited);
        assert_eq!(flags.len(), pts.len());
        let discard: Vec<i64> = flags
            .iter()
            .zip(&pts)
            .filter(|(f, _)| f.contains('D'))
            .map(|(_, p)| p.expect("pts"))
            .collect();
        assert!(discard.len() >= 2, "fixture without pre-roll: {flags:?}");
        // The premise: B-frames (a packet presented before an earlier
        // one) and pts ≠ dts as ffprobe lists them.
        assert!(
            pts.windows(2).any(|w| w[1] < w[0]),
            "bf {bframes}: not reordered"
        );
        let listed = packet_fields(&edited, "pts,dts");
        assert!(
            listed.lines().any(|l| {
                let mut f = l.split(',');
                f.next() != f.next()
            }),
            "bf {bframes}: pts == dts everywhere: {listed}"
        );
        let edited_packets =
            frametrace::join::normalized_video_packets("ffprobe", "ffmpeg", &edited, None)
                .unwrap_or_else(|e| panic!("bf {bframes}: B-frame source refused: {e}"));
        let empty = edited_packets
            .iter()
            .filter(|p| p.hash == Some(frametrace::join::EMPTY_PAYLOAD_HASH))
            .count();
        assert_eq!(empty, 3, "bf {bframes}: parameter-set-only packets");
        // The output material starts with exactly source 1's packets.
        let joined_pts = packet_pts(&edited_joined);
        assert_eq!(joined_pts[..pts.len()], pts[..], "bf {bframes}: prefix");
        let plain_count = packet_pts(&plain).len();
        assert_eq!(joined_pts.len(), pts.len() + plain_count);
        let kept = flags.iter().filter(|f| !f.contains('D')).count();
        let last_preroll = *discard.iter().max().unwrap();
        let sources = [edited.clone(), plain.clone()];
        let offsets = [0.0, probe_duration(&edited)];
        let check = |output: &Path| {
            frametrace::join::verify_segments_with(
                &sources,
                &offsets,
                output,
                &mut |path, hashes| {
                    if hashes {
                        frametrace::join::normalized_video_packets("ffprobe", "ffmpeg", path, None)
                    } else {
                        frametrace::join::video_packets("ffprobe", path, None, false)
                    }
                },
                &mut |path| frametrace::join::stream_packet_counts("ffprobe", path, None),
            )
        };
        // The output an edit-list-applying muxer writes (every packet the
        // edit list does not discard, then source 2), and a one-frame
        // shift of source 1's segment (the last pre-roll frame kept, its
        // last packet lost — same count). Built from `edited_joined` by
        // dropping packets, not by a second concat: a concat of a part
        // that starts mid-GOP with B-frames makes ffmpeg collide
        // timestamps at the junction (Matroska stores no dts), which the
        // check rightly refuses as unverifiable.
        for (format, ext, extra) in [
            ("mp4", "mp4", &["-use_editlist", "0"][..]),
            ("matroska", "mkv", &[][..]),
        ] {
            let good = root.join(format!("out-good.{ext}"));
            drop_packets(
                &edited_joined,
                &format!("lt(pts,{})", last_preroll + 1),
                extra,
                &good,
            );
            assert_eq!(packet_pts(&good).len(), kept + plain_count, "{format}");
            // Matroska stores no decode times; for a segment that starts
            // mid-GOP with a deep B-frame pyramid the derived dts can
            // repeat (ffprobe lists them non-increasing) and ffmpeg then
            // rewrites a pts to keep them ordered. That packet's hash
            // cannot be located — the check must refuse to run (never
            // pass on a guess), and only for such a file.
            let verdict = match check(&good) {
                Ok(verdict) => verdict,
                Err(err) => {
                    let dts: Vec<i64> = packet_fields(&good, "dts")
                        .lines()
                        .filter_map(|l| l.trim().trim_end_matches(',').parse().ok())
                        .collect();
                    assert!(
                        ext == "mkv"
                            && dts.windows(2).any(|w| w[1] <= w[0])
                            && err.contains("differ from ffprobe's"),
                        "bf {bframes} {format}: the check could not run: {err}"
                    );
                    unverifiable.push(format!("bf {bframes} {format}"));
                    continue;
                }
            };
            assert!(verdict.ok, "bf {bframes} {format}: {:?}", verdict.problems);
            passed.push(format!("bf {bframes} {format}"));
            assert_eq!(verdict.mode, "edit-list-applied", "bf {bframes} {format}");
            let bad = root.join(format!("out-shifted.{ext}"));
            drop_packets(
                &edited_joined,
                &format!("lt(pts,{})+eq(n,{})", last_preroll, pts.len() - 1),
                extra,
                &bad,
            );
            assert_eq!(packet_pts(&bad).len(), kept + plain_count, "{format}");
            let verdict = check(&bad)
                .unwrap_or_else(|e| panic!("bf {bframes} {format}: the check could not run: {e}"));
            assert!(
                !verdict.ok,
                "bf {bframes} {format}: a one-frame shift passed"
            );
            assert!(
                verdict.problems.iter().any(|p| p.contains("유지 프레임")),
                "bf {bframes} {format}: {:?}",
                verdict.problems
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    eprintln!(
        "R11-S1 compared frame by frame: {passed:?}; refused as unverifiable: {unverifiable:?}"
    );
    // Both containers really ran the frame-by-frame comparison.
    for format in ["mp4", "matroska"] {
        assert!(
            passed.iter().any(|p| p.ends_with(format)),
            "{format} never compared: {passed:?} / {unverifiable:?}"
        );
    }
    assert!(passed.iter().filter(|p| p.ends_with("mp4")).count() == 2);
}

/// R11-S1 (real ffprobe/ffmpeg): `packet_stamps` reads every B-frame
/// packet's pts and dts as ffmpeg's framehash prints them, in MP4,
/// Matroska and MPEG-TS (`-bf 2` and `-bf 3`). R10 read ffprobe's
/// `pts,dts` line as `dts,pts`. Matroska lists no dts (`N/A`) for the
/// first packets; framehash computes one — that dts is the only one not
/// compared.
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and ffmpeg/ffprobe"]
fn bframe_packet_timestamps_are_read_by_name() {
    if !require_tools() {
        return;
    }
    let root = unique_dir("ft_join_r11_s1_stamps");
    for bframes in ["2", "3"] {
        let plain = root.join(format!("bf{bframes}.mp4"));
        bframe_gop_clip(&plain, "2", bframes);
        for ext in ["mp4", "mkv", "ts"] {
            let file = root.join(format!("bf{bframes}-copy.{ext}"));
            ffmpeg_fixture(&[
                "-i",
                plain.to_str().unwrap(),
                "-c",
                "copy",
                file.to_str().unwrap(),
            ]);
            let stamps = frametrace::join::packet_stamps("ffprobe", &file, None).unwrap();
            let output = Command::new("ffmpeg")
                .args(["-v", "error", "-nostdin", "-copyts", "-i"])
                .arg(&file)
                .args(["-map", "0:v:0", "-c", "copy", "-copyinkf"])
                .args(["-f", "framehash", "-hash", "sha256", "-"])
                .output()
                .unwrap();
            let lines =
                frametrace::join::parse_framehash_lines(&String::from_utf8_lossy(&output.stdout))
                    .unwrap();
            assert_eq!(stamps.len(), lines.len(), "bf {bframes} {ext}");
            assert!(
                stamps.iter().any(|s| s.pts != s.dts),
                "bf {bframes} {ext}: pts == dts everywhere"
            );
            for (i, (stamp, line)) in stamps.iter().zip(&lines).enumerate() {
                assert_eq!(stamp.pts, line.pts, "bf {bframes} {ext} packet {i}");
                if stamp.dts.is_some() {
                    assert_eq!(stamp.dts, line.dts, "bf {bframes} {ext} packet {i}");
                }
            }
            if ext == "mkv" {
                assert!(
                    stamps.iter().any(|s| s.dts.is_none()),
                    "bf {bframes}: Matroska fixture lists every dts"
                );
            } else {
                assert!(stamps.iter().all(|s| s.dts.is_some()), "bf {bframes} {ext}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
