//! Real-ffmpeg integration tests for `join-video` and `measure-audio`.
//!
//! ```text
//! FRAMETRACE_IT=1 cargo test --locked --test join_audio_it -- --ignored
//! ```
//!
//! With `FRAMETRACE_IT=1` a missing ffmpeg/ffprobe FAILS the test (it never
//! silently skips); without it the ignored tests return early.

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
