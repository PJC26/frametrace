//! Lossless join over the workstation API (plan §1.6–1.7, §3 server tests)
//! plus the sound-measurement wiring (enrichment step, `audio-measure` case
//! job, `records-meta` `joinLog` / `audioLevels`).
//!
//! The CLI is replaced by a generated fake script (`/bin/sh` on unix, a
//! `.cmd` batch file on Windows) so the job machinery — registration,
//! dedupe, the shared running cap, cancel, cleanup and verification — runs
//! for real on both platforms.

use super::media::MAX_RUNNING_CLIP_JOBS;
use super::*;

const PLAN_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OUTPUT_NAME: &str = "join_F_20260910-120000_n2.mp4";
const MANIFEST_NAME: &str = "join_F_20260910-120000_n2.manifest.json";

fn temp_case(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-join-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::create_dir_all(dir.join(crate::join::JOINED_DIR)).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-join"}"#).unwrap();
    dir
}

/// Writes `db/videos.jsonl` with one row per `(id, file name)`; each source
/// file is created under `<case>/src/` with `bytes` bytes.
fn write_videos(case_dir: &Path, rows: &[(&str, &str)], bytes: usize) {
    let src = case_dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut lines = String::new();
    for (id, name) in rows {
        let path = src.join(name);
        std::fs::write(&path, vec![0u8; bytes]).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let modified = meta
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        lines.push_str(
            &serde_json::json!({
                "id": id,
                "source_path": path.to_string_lossy(),
                "relative_path": name,
                "size_bytes": meta.len(),
                "modified_unix": modified,
                "audio_codec": "aac",
                "probe_status": "ok",
            })
            .to_string(),
        );
        lines.push('\n');
    }
    std::fs::write(case_dir.join("db/videos.jsonl"), lines).unwrap();
}

fn bound_state(case_dir: &Path) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case_dir.to_path_buf());
    state.media_roots = vec![case_dir.to_path_buf()];
    state.phase = "review-ready";
    Arc::new(Mutex::new(state))
}

fn request(method: &str, path: &str, query: &str, body: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        body: body.into(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    }
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("invalid JSON {err}: {text}"))
}

fn status(state: &SharedState) -> serde_json::Value {
    parse(&api_status(state))
}

fn wait_for(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
    while !check() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn job_status(state: &SharedState, id: &str) -> serde_json::Value {
    parse(&api_export_clip_status(
        &request("GET", "/api/export-clip-status", &format!("job={id}"), ""),
        state,
    ))
}

fn wait_job_end(state: &SharedState, id: &str) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    // A generous upper bound (polled, not slept): under full-suite load a
    // cmd.exe fake can take seconds just to start.
    wait_for(
        &format!("job {id} to end"),
        Duration::from_secs(120),
        || {
            last = job_status(state, id);
            last["job"]["state"] != "running"
        },
    );
    last
}

fn tools_without_ffprobe() -> crate::join::Tools {
    // A path that does not exist: every probe fails, so the plan is built
    // (and blocked with unknown-duration) without any external tool.
    let missing = std::env::temp_dir().join("ft-join-no-such-dir/ffprobe");
    crate::join::Tools {
        ffmpeg: "ffmpeg".into(),
        ffprobe: missing.to_string_lossy().to_string(),
    }
}

fn check(state: &SharedState, body: &str) -> serde_json::Value {
    parse(&join_check_with_tools(
        &request("POST", "/api/join-check", "", body),
        state,
        &tools_without_ffprobe(),
    ))
}

fn join_body(ids: &[&str], plan_id: &str) -> String {
    serde_json::json!({"ids": ids, "plan_id": plan_id, "tolerance": 1.0}).to_string()
}

fn start_join(state: &SharedState, exe: &Path, body: &str) -> serde_json::Value {
    parse(&join_with_exe(
        &request("POST", "/api/join", "", body),
        state,
        exe,
    ))
}

/// One step of the fake CLI's `join-video` branch.
enum Step {
    Copy(PathBuf, PathBuf),
    Append(PathBuf, PathBuf),
    Stderr(&'static str),
    /// Writes `.join-<tag>.{part,ffconcat,progress.json}` in the joined
    /// dir (tag = the `--job-tag` argument; progress copied from the file).
    TaggedTemps(PathBuf, PathBuf),
    /// Copies a claim file to `.join-<tag>.claim.json` in the joined dir.
    /// Must follow a `TaggedTemps` step (which found the tag).
    TaggedClaim(PathBuf, PathBuf),
    Spin,
    Exit(i32),
}

/// Writes a fake frametrace CLI: `join-video` runs `steps`, every other
/// subcommand (make-review of a case job, measure-audio) succeeds.
fn fake_cli(dir: &Path, steps: &[Step]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // C13: the job tag (`--job-tag`) is found once; `Append` writes
        // it into staged log lines (`@TAG@`) like the real CLI does.
        let mut body = String::from(
            "#!/bin/sh\nif [ \"$1\" != \"join-video\" ]; then echo ok; exit 0; fi\n\
             tag=\"\"; prev=\"\"; for a in \"$@\"; do if [ \"$prev\" = \"--job-tag\" ]; then tag=\"$a\"; fi; prev=\"$a\"; done\n",
        );
        for step in steps {
            body.push_str(&match step {
                Step::Copy(src, dst) => format!("cp \"{}\" \"{}\"\n", src.display(), dst.display()),
                // R9 P3: appended as a real CHAINED entry (the server
                // reads the join log through `audit::read_lane_rows`).
                Step::Append(src, dst) => format!(
                    "body=$(sed \"s/@TAG@/$tag/g\" \"{src}\")\n\
                     h() {{ if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi; }}\n\
                     prev=GENESIS\n\
                     if [ -s \"{dst}\" ]; then prev=$(printf '%s' \"$(tail -n 1 \"{dst}\")\" | h); fi\n\
                     chained=\"${{body%\\}}}},\\\"previous_entry_sha256\\\":\\\"$prev\\\"}}\"\n\
                     entry=$(printf '%s' \"$chained\" | h)\n\
                     printf '%s\\n' \"${{chained%\\}}}},\\\"entry_sha256\\\":\\\"$entry\\\"}}\" >> \"{dst}\"\n",
                    src = src.display(),
                    dst = dst.display()
                ),
                Step::Stderr(text) => format!("echo \"{text}\" >&2\n"),
                Step::TaggedTemps(dir, progress) => {
                    let dir = dir.display();
                    format!(
                        "echo partial > \"{dir}/.join-$tag.part\"\n\
                         echo list > \"{dir}/.join-$tag.ffconcat\"\n\
                         cp \"{}\" \"{dir}/.join-$tag.progress.json\"\n",
                        progress.display()
                    )
                }
                Step::TaggedClaim(dir, claim) => format!(
                    "cp \"{}\" \"{}/.join-$tag.claim.json\"\n",
                    claim.display(),
                    dir.display()
                ),
                Step::Spin => "sleep 30\n".to_string(),
                Step::Exit(code) => format!("exit {code}\n"),
            });
        }
        body.push_str("exit 0\n");
        let path = dir.join("fake-frametrace.sh");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    #[cfg(windows)]
    {
        // cmd also splits arguments on commas: find the tag by its flag
        // instead of by position (once, up front — `shift` consumes them).
        let mut body = String::from(
            "@echo off\r\nif not \"%1\"==\"join-video\" (\r\n  echo ok\r\n  exit /b 0\r\n)\r\n\
             set tag=\r\n:findtag\r\nif \"%~1\"==\"\" goto gottag\r\n\
             if \"%~1\"==\"--job-tag\" set tag=%~2\r\nshift\r\ngoto findtag\r\n:gottag\r\n",
        );
        for step in steps {
            body.push_str(&match step {
                Step::Copy(src, dst) => format!(
                    "copy /y \"{}\" \"{}\" >nul\r\n",
                    src.display(),
                    dst.display()
                ),
                // C13: staged log lines carry `@TAG@` for the job tag.
                // R9 P3: appended as a real CHAINED entry (the server
                // reads the join log through `audit::read_lane_rows`).
                Step::Append(src, dst) => format!(
                    "powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" \"{}\" \"{}\" \"%tag%\"\r\n",
                    append_helper_ps1(dir).display(),
                    src.display(),
                    dst.display()
                ),
                Step::Stderr(text) => format!("echo {text} 1>&2\r\n"),
                Step::TaggedTemps(dir, progress) => {
                    let dir = dir.display();
                    format!(
                        "echo partial> \"{dir}\\.join-%tag%.part\"\r\n\
                         echo list> \"{dir}\\.join-%tag%.ffconcat\"\r\n\
                         copy /y \"{}\" \"{dir}\\.join-%tag%.progress.json\" >nul\r\n",
                        progress.display()
                    )
                }
                Step::TaggedClaim(dir, claim) => format!(
                    "copy /y \"{}\" \"{}\\.join-%tag%.claim.json\" >nul\r\n",
                    claim.display(),
                    dir.display()
                ),
                // Item 8: an idle wait, not a busy `goto` loop — several
                // spinning cmd.exe (the running-cap test starts four) pinned
                // the CPU under full-suite load and starved the other join
                // tests' fakes past their deadlines. `ping` gets `nul` for
                // both streams, so it never holds the step's pipes open
                // (and dies with the job object on cancel).
                Step::Spin => ":spin\r\nping -n 2 127.0.0.1 >nul 2>&1\r\ngoto spin\r\n".to_string(),
                Step::Exit(code) => format!("exit /b {code}\r\n"),
            });
        }
        body.push_str("exit /b 0\r\n");
        let path = dir.join("fake-frametrace.cmd");
        std::fs::write(&path, body).unwrap();
        path
    }
}

/// Windows: a PowerShell helper the fake CLI's `Append` step runs — reads
/// the staged JSON body, substitutes the job tag, and appends it as a
/// chained audit entry (`previous_entry_sha256` = SHA-256 of the last
/// line, `entry_sha256` over the line through it), like
/// `audit::append_chained_jsonl` (unkeyed).
#[cfg(windows)]
fn append_helper_ps1(dir: &Path) -> PathBuf {
    let path = dir.join("fake-append.ps1");
    let script = r#"param($src, $dst, $tag)
function Get-LineSha($s) { $sha = [Security.Cryptography.SHA256]::Create(); ([BitConverter]::ToString($sha.ComputeHash([Text.UTF8Encoding]::new($false).GetBytes($s)))).Replace('-', '').ToLower() }
$body = [IO.File]::ReadAllText($src).Replace('@TAG@', $tag).TrimEnd("`r", "`n")
$prev = 'GENESIS'
if ((Test-Path $dst) -and ((Get-Item $dst).Length -gt 0)) { $prev = Get-LineSha (([IO.File]::ReadAllText($dst).TrimEnd("`n")).Split("`n")[-1]) }
$chained = $body.Substring(0, $body.Length - 1) + ',"previous_entry_sha256":"' + $prev + '"}'
$entry = Get-LineSha $chained
[IO.File]::AppendAllText($dst, $chained.Substring(0, $chained.Length - 1) + ',"entry_sha256":"' + $entry + '"}' + "`n", [Text.UTF8Encoding]::new($false))
"#;
    std::fs::write(&path, script).unwrap();
    path
}

fn now() -> u64 {
    crate::util::now_unix().unwrap()
}

/// Stages what a successful `join-video` publishes: output, sidecar
/// manifest and one join-log line, bound together by SHA-256 like the
/// real CLI. `manifest_sha_override` corrupts the manifest's output hash.
struct Staged {
    output_src: PathBuf,
    manifest_src: PathBuf,
    log_src: PathBuf,
    output: PathBuf,
    manifest: PathBuf,
    log: PathBuf,
}

fn stage_success(case_dir: &Path, plan_id: &str, manifest_sha_override: Option<&str>) -> Staged {
    let staging = case_dir.join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let joined = case_dir.join(crate::join::JOINED_DIR);
    let output = joined.join(OUTPUT_NAME);
    let manifest = joined.join(MANIFEST_NAME);
    let log = joined.join(crate::join::LOG_FILE);
    let output_src = staging.join("out.mp4");
    std::fs::write(&output_src, b"joined-bytes-joined-bytes").unwrap();
    let output_sha = crate::audit::digest_file(&output_src).unwrap();
    let created = now() + 3600;
    let manifest_text = format!(
        "{}\n",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "kind": "frametrace-join-manifest",
            "plan_id": plan_id,
            "created_unix": created,
            "output": {
                "path_rel": format!("{}/{OUTPUT_NAME}", crate::join::JOINED_DIR),
                "sha256": manifest_sha_override.unwrap_or(&output_sha),
                "size_bytes": 25,
            },
            "duration_check": {"expected": 4.0, "observed": 3.98, "ok": true},
        }))
        .unwrap()
    );
    let manifest_src = staging.join("out.manifest.json");
    std::fs::write(&manifest_src, &manifest_text).unwrap();
    let log_line = serde_json::json!({
        "schema_version": 1,
        "event": "join-video",
        "joined_unix": created,
        "output_path": output.to_string_lossy(),
        "output_sha256": output_sha,
        "output_size_bytes": 25,
        "manifest_path": manifest.to_string_lossy(),
        "manifest_sha256": crate::sha256::digest_bytes(manifest_text.as_bytes()),
        "plan_id": plan_id,
        // C13: substituted with the job's `--job-tag` by the fake CLI.
        "job_tag": "@TAG@",
        "segment_count": 2,
        "source_ids": ["vid_1", "vid_2"],
        "duration_check_ok": true,
    });
    let log_src = staging.join("log.jsonl");
    std::fs::write(&log_src, format!("{log_line}\n")).unwrap();
    Staged {
        output_src,
        manifest_src,
        log_src,
        output,
        manifest,
        log,
    }
}

fn success_steps(staged: &Staged) -> Vec<Step> {
    vec![
        Step::Copy(staged.output_src.clone(), staged.output.clone()),
        Step::Copy(staged.manifest_src.clone(), staged.manifest.clone()),
        Step::Append(staged.log_src.clone(), staged.log.clone()),
    ]
}

// ---------------------------------------------------------------------------
// /api/join-check
// ---------------------------------------------------------------------------

#[test]
fn join_check_refuses_bad_requests() {
    // No case bound.
    let empty: SharedState = Arc::new(Mutex::new(JobState::new()));
    let none = check(&empty, r#"{"ids":["vid_1","vid_2"]}"#);
    assert_eq!(none["ok"], false, "{none}");

    let case_dir = temp_case("check-refuse");
    write_videos(
        &case_dir,
        &[
            ("vid_1", "20260910_120000_F.mp4"),
            ("vid_2", "20260910_120100_F.mp4"),
        ],
        64,
    );
    let state = bound_state(&case_dir);
    let many: Vec<String> = (0..501).map(|i| format!("vid_{i}")).collect();
    let control = serde_json::json!({"ids": ["vid_1", "vid_\u{1}x"]}).to_string();
    let long =
        serde_json::json!({"ids": ["vid_1", format!("vid_{}", "x".repeat(600))]}).to_string();
    for (body, needle) in [
        ("not json".to_string(), "요청 형식"),
        (r#"{"ids":"vid_1"}"#.to_string(), "2개 이상"),
        (r#"{"ids":["vid_1"]}"#.to_string(), "2개 이상"),
        (serde_json::json!({ "ids": many }).to_string(), "500개까지"),
        (r#"{"ids":["vid_1",2]}"#.to_string(), "문자열"),
        (control, "잘못된 id"),
        (long, "잘못된 id"),
        (r#"{"ids":["vid_1","carve_9"]}"#.to_string(), "vid_*"),
        (r#"{"ids":["vid_1","join:x.mp4"]}"#.to_string(), "vid_*"),
        (
            r#"{"ids":["vid_1","vid_2"],"tolerance":9}"#.to_string(),
            "허용 오차",
        ),
        (
            r#"{"ids":["vid_1","vid_2"],"tolerance":"x"}"#.to_string(),
            "허용 오차",
        ),
        (r#"{"ids":["vid_1","vid_404"]}"#.to_string(), "색인에 없는"),
    ] {
        let response = check(&state, &body);
        assert_eq!(response["ok"], false, "{body}: {response}");
        let error = response["error"].as_str().unwrap_or_default();
        assert!(error.contains(needle), "{body}: {error}");
    }
    // An analysis holding `busy` refuses the check (CLIP_BUSY_ERROR).
    state_lock(&state).busy = true;
    let busy = check(&state, r#"{"ids":["vid_1","vid_2"]}"#);
    assert_eq!(busy["ok"], false, "{busy}");
    assert!(
        busy["error"].as_str().unwrap().contains("분석 작업"),
        "{busy}"
    );
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn join_check_returns_the_plan_and_remembers_its_size() {
    let case_dir = temp_case("check-plan");
    write_videos(
        &case_dir,
        &[
            ("vid_1", "20260910_120000_F.mp4"),
            ("vid_2", "20260910_120100_F.mp4"),
        ],
        100,
    );
    let state = bound_state(&case_dir);
    let response = check(&state, r#"{"ids":["vid_1","vid_2"],"tolerance":"1.5"}"#);
    assert_eq!(response["ok"], true, "{response}");
    let plan = &response["plan"];
    let plan_id = plan["plan_id"].as_str().unwrap();
    assert_eq!(plan_id.len(), 64, "{plan}");
    assert_eq!(plan["tolerance_seconds"], 1.5);
    assert_eq!(plan["segments"].as_array().unwrap().len(), 2);
    for key in [
        "order_basis",
        "channel",
        "container",
        "junctions",
        "totals",
        "warnings",
        "summary_text",
    ] {
        assert!(plan.get(key).is_some(), "missing {key}: {plan}");
    }
    // ffprobe unavailable: the plan is returned, blocked — never refused.
    let codes: Vec<&str> = plan["blocking"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|issue| issue["code"].as_str())
        .collect();
    assert!(codes.contains(&"unknown-duration"), "{plan}");
    // The check remembers the summed source size for the job's progress.
    let remembered = state_lock(&state).join_plan_bytes.clone();
    assert_eq!(remembered, vec![(plan_id.to_string(), 200)]);
    let _ = std::fs::remove_dir_all(case_dir);
}

// ---------------------------------------------------------------------------
// /api/join
// ---------------------------------------------------------------------------

#[test]
fn join_refuses_missing_or_malformed_plan_id_and_busy() {
    let case_dir = temp_case("join-refuse");
    let state = bound_state(&case_dir);
    let exe = fake_cli(&case_dir, &[]);
    let missing = start_join(&state, &exe, r#"{"ids":["vid_1","vid_2"]}"#);
    assert_eq!(missing["ok"], false, "{missing}");
    assert!(missing["error"].as_str().unwrap().contains("plan_id"));
    let bad = start_join(&state, &exe, &join_body(&["vid_1", "vid_2"], "zz"));
    assert_eq!(bad["ok"], false, "{bad}");
    let carve = start_join(&state, &exe, &join_body(&["vid_1", "carve_2"], PLAN_ID));
    assert_eq!(carve["ok"], false, "{carve}");
    state_lock(&state).busy = true;
    let busy = start_join(&state, &exe, &join_body(&["vid_1", "vid_2"], PLAN_ID));
    assert_eq!(busy["ok"], false, "{busy}");
    assert!(state_lock(&state).clip_jobs.is_empty());
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn join_success_is_verified_reported_and_deduped() {
    let case_dir = temp_case("join-ok");
    let state = bound_state(&case_dir);
    state_lock(&state)
        .join_plan_bytes
        .push((PLAN_ID.to_string(), 4242));
    // A crashed run's leftovers: removed (and logged) at the next start.
    let joined = case_dir.join(crate::join::JOINED_DIR);
    let stale_part = joined.join(".join-srv-1-1.part");
    let stale_list = joined.join(".join-srv-1-1.ffconcat");
    let stale_lock = joined.join(".join-srv-1-1.lock");
    std::fs::write(&stale_part, b"torn").unwrap();
    std::fs::write(&stale_list, b"ffconcat version 1.0\n").unwrap();
    // Its run lock is free: the job that wrote these is dead (B13).
    std::fs::write(&stale_lock, b"").unwrap();
    // A fresh lock-less leftover may belong to a join running elsewhere.
    let fresh = joined.join(".join-cli-9-9.part");
    std::fs::write(&fresh, b"busy").unwrap();
    let staged = stage_success(&case_dir, PLAN_ID, None);
    let exe = fake_cli(&case_dir, &success_steps(&staged));

    let body = join_body(&["vid_2", "vid_1"], PLAN_ID);
    let response = start_join(&state, &exe, &body);
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["deduped"], false);
    let job = &response["job"];
    assert_eq!(job["kind"], "join", "{job}");
    assert_eq!(job["plan_id"], PLAN_ID);
    assert_eq!(job["segments"], 2);
    assert_eq!(job["expected_bytes"], 4242);
    assert!(job["path"].is_null(), "final name unknown while running");
    let id = job["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("join-"), "{id}");

    let done = wait_job_end(&state, &id);
    assert_eq!(done["job"]["state"], "done", "{done}");
    let rel = format!("{}/{OUTPUT_NAME}", crate::join::JOINED_DIR);
    assert_eq!(done["job"]["path"], rel.as_str());
    assert_eq!(done["path"], rel.as_str());
    let digest = crate::audit::digest_file(&staged.output).unwrap();
    assert_eq!(done["sha256"], digest.as_str());
    assert_eq!(done["job"]["size_bytes"], 25);
    assert_eq!(done["job"]["bytes_written"], 25);
    assert_eq!(
        done["manifest_path"],
        format!("{}/{MANIFEST_NAME}", crate::join::JOINED_DIR).as_str()
    );
    assert_eq!(done["job"]["duration_check_ok"], true);
    assert_eq!(
        done["job"]["duration_check"],
        serde_json::json!({"expected": 4.0, "observed": 3.98, "ok": true})
    );
    assert_eq!(done["duration_check"]["ok"], true);
    assert!(!stale_part.exists() && !stale_list.exists() && !stale_lock.exists());
    assert!(fresh.exists(), "a fresh lock-less temp file was removed");

    // /api/status lists it as a join beside clip exports.
    let listed = status(&state);
    assert_eq!(listed["clip_jobs"][0]["kind"], "join", "{listed}");
    assert_eq!(listed["clips_running"], 0);
    let logs = listed["logs"].to_string();
    assert!(logs.contains("잔여 임시 파일"), "{logs}");
    assert!(logs.contains("영상 이어붙이기 완료"), "{logs}");

    // The same request (any id order) returns the finished job while its
    // output is still on disk.
    let again = start_join(&state, &exe, &join_body(&["vid_1", "vid_2"], PLAN_ID));
    assert_eq!(again["deduped"], true, "{again}");
    assert_eq!(again["job"]["id"], id.as_str());
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn join_output_that_fails_rehash_is_kept_and_reported() {
    let case_dir = temp_case("join-mismatch");
    let state = bound_state(&case_dir);
    let staged = stage_success(&case_dir, PLAN_ID, Some(&"f".repeat(64)));
    let exe = fake_cli(&case_dir, &success_steps(&staged));
    let response = start_join(&state, &exe, &join_body(&["vid_1", "vid_2"], PLAN_ID));
    let id = response["job"]["id"].as_str().unwrap().to_string();
    let end = wait_job_end(&state, &id);
    assert_eq!(end["job"]["state"], "failed", "{end}");
    let error = end["job"]["error"].as_str().unwrap();
    assert!(error.contains("SHA-256"), "{error}");
    // Logged artifacts are never deleted out from under their record.
    assert!(staged.output.exists() && staged.manifest.exists());
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn join_plan_changed_since_check_fails_with_recheck_message() {
    let case_dir = temp_case("join-plan-changed");
    let state = bound_state(&case_dir);
    let exe = fake_cli(
        &case_dir,
        &[Step::Stderr("error: plan changed"), Step::Exit(3)],
    );
    let response = start_join(&state, &exe, &join_body(&["vid_1", "vid_2"], PLAN_ID));
    let id = response["job"]["id"].as_str().unwrap().to_string();
    let end = wait_job_end(&state, &id);
    assert_eq!(end["job"]["state"], "failed", "{end}");
    assert_eq!(end["job"]["error"], crate::join::PLAN_CHANGED_MESSAGE);
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn join_failure_removes_temp_files_and_unlogged_outputs() {
    let case_dir = temp_case("join-fail");
    let state = bound_state(&case_dir);
    let staged = stage_success(&case_dir, PLAN_ID, None);
    let joined = case_dir.join(crate::join::JOINED_DIR);
    // Another, older logged join must survive the cleanup untouched.
    let kept = joined.join("join_R_20260101-000000_n3.mp4");
    std::fs::write(&kept, b"older").unwrap();
    std::fs::write(
        joined.join(crate::join::LOG_FILE),
        format!(
            "{}\n",
            serde_json::json!({"event": "join-video", "plan_id": "aa", "joined_unix": 1,
                               "output_path": kept.to_string_lossy()})
        ),
    )
    .unwrap();
    // B8: a concurrent same-plan job's unlogged output + manifest (fresh,
    // same plan_id) and a manifest-less output — none of them claimed by
    // this job — must survive this job's cleanup.
    let lone = joined.join("join_F_20260910-120000_n2__2.mp4");
    let other = joined.join("join_F_20260910-120000_n2__3.mp4");
    let other_manifest = joined.join("join_F_20260910-120000_n2__3.manifest.json");
    // The CLI dies after publishing output + manifest (recorded in its
    // claim) but before the log append.
    let progress_src = case_dir.join("staging/progress.json");
    std::fs::write(&progress_src, br#"{"stage":"verify","done":0,"total":1}"#).unwrap();
    let claim_src = case_dir.join("staging").join("claim.json");
    std::fs::write(
        &claim_src,
        serde_json::json!({
            "output": staged.output.to_string_lossy(),
            "manifest": staged.manifest.to_string_lossy(),
        })
        .to_string(),
    )
    .unwrap();
    let exe = fake_cli(
        &case_dir,
        &[
            Step::TaggedTemps(joined.clone(), progress_src),
            Step::Copy(staged.output_src.clone(), staged.output.clone()),
            Step::TaggedClaim(joined.clone(), claim_src),
            Step::Copy(staged.manifest_src.clone(), staged.manifest.clone()),
            Step::Copy(staged.output_src.clone(), lone.clone()),
            Step::Copy(staged.output_src.clone(), other.clone()),
            Step::Copy(staged.manifest_src.clone(), other_manifest.clone()),
            Step::Exit(1),
        ],
    );
    let response = start_join(&state, &exe, &join_body(&["vid_1", "vid_2"], PLAN_ID));
    let id = response["job"]["id"].as_str().unwrap().to_string();
    let end = wait_job_end(&state, &id);
    assert_eq!(end["job"]["state"], "failed", "{end}");
    assert!(
        end["job"]["error"].as_str().unwrap().contains("join-video"),
        "{end}"
    );
    assert!(!staged.output.exists(), "unlogged output left behind");
    assert!(!staged.manifest.exists(), "unlogged manifest left behind");
    assert!(
        lone.exists(),
        "an output this job never claimed was deleted"
    );
    assert!(
        other.exists() && other_manifest.exists(),
        "another same-plan job's output was deleted"
    );
    let leftovers: Vec<String> = std::fs::read_dir(&joined)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".join-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert!(kept.exists(), "a logged join from before was touched");
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn join_cancel_kills_the_cli_and_removes_its_temp_files() {
    let case_dir = temp_case("join-cancel");
    let state = bound_state(&case_dir);
    let joined = case_dir.join(crate::join::JOINED_DIR);
    // The job tag embeds the registration time and sequence number: the
    // fake reads it from argv, so it writes the real temp names.
    let progress_src = case_dir.join("progress.json");
    std::fs::write(&progress_src, br#"{"stage":"hash","done":1,"total":2}"#).unwrap();
    let exe = fake_cli(
        &case_dir,
        &[
            Step::TaggedTemps(joined.clone(), progress_src.clone()),
            Step::Spin,
        ],
    );
    let response = start_join(&state, &exe, &join_body(&["vid_1", "vid_2"], PLAN_ID));
    assert_eq!(response["ok"], true, "{response}");
    let id = response["job"]["id"].as_str().unwrap().to_string();
    wait_for("progress visible", Duration::from_secs(120), || {
        job_status(&state, &id)["job"]["stage"] == "hash"
    });
    let running = job_status(&state, &id);
    assert_eq!(running["job"]["stage_done"], 1, "{running}");
    assert_eq!(running["job"]["stage_total"], 2, "{running}");
    assert!(
        running["job"]["bytes_written"].as_u64().unwrap() > 0,
        "{running}"
    );

    // F8/F9 protections apply to a running join.
    let shutdown = parse(&api_shutdown(&state));
    assert_eq!(shutdown["ok"], false, "{shutdown}");
    let open = parse(&api_open_case(
        &request(
            "POST",
            "/api/open-case",
            "",
            &serde_json::json!({"case_dir": case_dir.to_string_lossy()}).to_string(),
        ),
        &state,
    ));
    assert_eq!(open["ok"], false, "{open}");
    let measure = parse(&audio_measure_with_exe(
        &request("POST", "/api/audio-measure", "", "{}"),
        &state,
        &exe,
    ));
    assert_eq!(measure["ok"], false, "case jobs wait for joins: {measure}");
    assert!(
        !state_lock(&state).busy,
        "refused case job must release busy"
    );

    let cancel = parse(&api_export_clip_cancel(
        &request(
            "POST",
            "/api/export-clip-cancel",
            "",
            &serde_json::json!({ "job": id }).to_string(),
        ),
        &state,
    ));
    assert_eq!(cancel["ok"], true, "{cancel}");
    let end = wait_job_end(&state, &id);
    assert_eq!(end["job"]["state"], "cancelled", "{end}");
    let leftovers: Vec<String> = std::fs::read_dir(&joined)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn joins_share_the_clip_running_cap_and_global_cancel() {
    let case_dir = temp_case("join-cap");
    let state = bound_state(&case_dir);
    let exe = fake_cli(&case_dir, &[Step::Spin]);
    let mut ids = Vec::new();
    for n in 0..MAX_RUNNING_CLIP_JOBS {
        let body = serde_json::json!({
            "ids": ["vid_1", format!("vid_{}", n + 2)],
            "plan_id": PLAN_ID,
        })
        .to_string();
        let response = start_join(&state, &exe, &body);
        assert_eq!(response["ok"], true, "{response}");
        ids.push(response["job"]["id"].as_str().unwrap().to_string());
    }
    // Identical running request: deduped, no fifth job.
    let dup = start_join(
        &state,
        &exe,
        &serde_json::json!({"ids": ["vid_2", "vid_1"], "plan_id": PLAN_ID}).to_string(),
    );
    assert_eq!(dup["deduped"], true, "{dup}");
    assert_eq!(dup["job"]["id"], ids[0].as_str());
    // The cap is shared: a fifth join and a clip export are both refused.
    let fifth = start_join(&state, &exe, &join_body(&["vid_1", "vid_99"], PLAN_ID));
    assert_eq!(fifth["ok"], false, "{fifth}");
    let clip = parse(&export_clip_with_exe(
        &request(
            "POST",
            "/api/export-clip",
            "",
            r#"{"id":"vid_1","start":"0","duration":"1"}"#,
        ),
        &state,
        &exe,
    ));
    assert_eq!(clip["ok"], false, "{clip}");
    assert_eq!(status(&state)["clips_running"], MAX_RUNNING_CLIP_JOBS);
    // The global cancel (nothing holds busy) stops every join.
    let cancel = parse(&api_cancel(&state));
    assert_eq!(cancel["clips_cancelled"], MAX_RUNNING_CLIP_JOBS, "{cancel}");
    for id in &ids {
        assert_eq!(wait_job_end(&state, id)["job"]["state"], "cancelled");
    }
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn join_routes_carry_no_path_fields() {
    for route in ["/api/join-check", "/api/join", "/api/audio-measure"] {
        assert!(
            super::http::path_fields_for("POST", route).is_empty(),
            "{route}"
        );
    }
}

// ---------------------------------------------------------------------------
// Sound measurement wiring
// ---------------------------------------------------------------------------

fn pipeline_job(case_dir: &Path, with_ffprobe: bool, with_audio: bool) -> PipelineJob {
    PipelineJob {
        kind: InputKind::Folder,
        case_dir: case_dir.to_path_buf(),
        source_path: PathBuf::from("/ev"),
        with_hash: false,
        with_ffprobe,
        with_deepfake: false,
        with_audio,
        skip_e01_verify: false,
        e01_direct: false,
    }
}

#[test]
fn with_audio_adds_the_sound_step_after_validate_only_with_ffprobe() {
    let case_dir = temp_case("enrich-plan");
    let keys = |job: &PipelineJob| {
        enrichment_plan(job)
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(&pipeline_job(&case_dir, true, true)),
        ["hash", "validate", "audio", "anomalies", "make-review"]
    );
    assert!(
        enrichment_plan(&pipeline_job(&case_dir, true, true)).contains(&("audio", "소리 측정"))
    );
    assert_eq!(
        keys(&pipeline_job(&case_dir, true, false)),
        ["hash", "validate", "anomalies", "make-review"]
    );
    assert_eq!(
        keys(&pipeline_job(&case_dir, false, true)),
        ["anomalies", "make-review"]
    );

    // The runner sees measure-audio right after validate-batch.
    std::fs::write(
        case_dir.join("db/video_index.json"),
        r#"{"videos":[{"id":"vid_1"}]}"#,
    )
    .unwrap();
    let state = bound_state(&case_dir);
    {
        let mut guard = state_lock(&state);
        guard.phase = "review-partial";
        guard.busy = true;
    }
    let seen: Mutex<Vec<String>> = Mutex::new(Vec::new());
    run_folder_enrichment(&state, &pipeline_job(&case_dir, true, true), |args| {
        seen.lock().unwrap().push(args.join(" "));
        Ok("ok".into())
    });
    let seen = seen.into_inner().unwrap();
    let position = |prefix: &str| seen.iter().position(|cmd| cmd.starts_with(prefix));
    let audio = position("measure-audio").expect("measure-audio ran");
    assert_eq!(
        seen[audio],
        format!("measure-audio {}", case_dir.to_string_lossy())
    );
    assert!(
        position("validate-batch").is_some_and(|v| v < audio),
        "{seen:?}"
    );
    assert!(
        position("qa anomalies").is_some_and(|a| a > audio),
        "{seen:?}"
    );
    assert_eq!(state_lock(&state).phase, "review-ready");

    // with_audio without ffprobe: skipped with a visible warning.
    let state = bound_state(&case_dir);
    {
        let mut guard = state_lock(&state);
        guard.phase = "review-partial";
        guard.busy = true;
    }
    let ran_audio = Mutex::new(false);
    run_folder_enrichment(&state, &pipeline_job(&case_dir, false, true), |args| {
        if args.first().map(String::as_str) == Some("measure-audio") {
            *ran_audio.lock().unwrap() = true;
        }
        Ok("ok".into())
    });
    assert!(!*ran_audio.lock().unwrap());
    let warnings = status(&state)["warnings"].to_string();
    assert!(warnings.contains("소리 측정"), "{warnings}");
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn measure_audio_runs_as_a_case_job() {
    let case_dir = temp_case("audio-job");
    let state = bound_state(&case_dir);
    let exe = fake_cli(&case_dir, &[]);
    let empty: SharedState = Arc::new(Mutex::new(JobState::new()));
    let refused = parse(&audio_measure_with_exe(
        &request("POST", "/api/audio-measure", "", "{}"),
        &empty,
        &exe,
    ));
    assert_eq!(refused["ok"], false, "{refused}");
    let response = parse(&audio_measure_with_exe(
        &request("POST", "/api/audio-measure", "", r#"{"all":true}"#),
        &state,
        &exe,
    ));
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["job"]["kind"], "audio-measure");
    wait_for("audio-measure done", Duration::from_secs(30), || {
        status(&state)["job"]["state"] == "done"
    });
    let done = status(&state);
    assert_eq!(done["job"]["kind"], "audio-measure");
    assert_eq!(done["busy"], false);
    assert!(
        done["logs"].to_string().contains("전체 다시 측정"),
        "{done}"
    );
    let _ = std::fs::remove_dir_all(case_dir);
}

#[test]
fn records_meta_carries_join_log_and_bound_audio_levels() {
    let case_dir = temp_case("records-meta");
    write_videos(&case_dir, &[("vid_1", "a.mp4"), ("vid_2", "b.mp4")], 10);
    // The scan writes the same rows into video_index.json; records-meta
    // binds levels through that (cached) index (B9).
    let index_rows: Vec<serde_json::Value> =
        std::fs::read_to_string(case_dir.join("db/videos.jsonl"))
            .unwrap()
            .lines()
            .map(parse)
            .collect();
    std::fs::write(
        case_dir.join("db/video_index.json"),
        serde_json::json!({ "videos": index_rows }).to_string(),
    )
    .unwrap();
    let state = bound_state(&case_dir);
    // Nothing joined or measured yet: stable empty shapes.
    let empty = parse(&api_records_meta(&state));
    assert_eq!(empty["joinLog"], serde_json::json!([]), "{empty}");
    assert_eq!(empty["audioLevels"], serde_json::json!({}));
    assert!(empty["audioLevelsError"].is_null());

    let joined = case_dir.join(crate::join::JOINED_DIR);
    // R9 P3: a real chained entry, then a torn line.
    let join_lane = joined.join(crate::join::LOG_FILE);
    crate::audit::test_write_lane(
        &join_lane,
        &format!(
            "{{\"event\":\"join-video\",\"plan_id\":\"p\",\"segment_count\":2,\"output_sha256\":\"{}\"}}\n",
            "AB".repeat(32)
        ),
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&join_lane)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"{\"torn\":\n"))
        .unwrap();
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(case_dir.join("db/videos.jsonl"))
        .unwrap()
        .lines()
        .map(parse)
        .collect();
    let result = |row: &serde_json::Value, size: u64| {
        serde_json::json!({
            "selector": row["id"], "source_path": row["source_path"],
            "size_bytes": size, "modified_unix": row["modified_unix"],
            "sha256_indexed": null, "result": "sound",
            "streams": [{"index": 1, "codec": "aac", "n_samples": 480000,
                         "measured_seconds": 10.0, "max_volume_db": -12.5,
                         "mean_volume_db": -30.0, "max_is_neg_inf": false}],
            "error": null,
        })
    };
    let entry = serde_json::json!({
        "schema_version": 1, "event": "audio-level", "measured_unix": 77,
        "method": crate::audio_level::METHOD,
        "threshold_silent_max_dbfs": crate::audio_level::SILENT_MAX_DBFS,
        "ffmpeg_version": "test", "command_template": [],
        // vid_2 was measured on other bytes (size differs) → stale.
        "results": [result(&rows[0], 10), result(&rows[1], 11)],
    });
    let log = case_dir.join(crate::audio_level::LOG_REL);
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    // D1: the lane is chain-verified before any level is shown, so the
    // fixture is a real chained lane.
    crate::audit::append_chained_jsonl_keyed(&log, &entry.to_string(), None).unwrap();

    let computed = || super::records::AUDIO_LEVELS_COMPUTED.with(|c| c.get());
    let before = computed();
    let meta = parse(&api_records_meta(&state));
    assert_eq!(meta["ok"], true);
    let join_log = meta["joinLog"].as_array().unwrap();
    assert_eq!(join_log.len(), 1, "torn line skipped: {meta}");
    assert_eq!(join_log[0]["event"], "join-video");
    // U2: the derived record id, from the output bytes (not the name).
    assert_eq!(join_log[0]["record_id"], "join:abababababababab");
    // R9 P3 (contract with track U): every log entry carries `trust`.
    // R11-I4: a lane whose chain fails (here a torn final line) is not
    // usable before the break either — shown as "unauth".
    assert_eq!(join_log[0]["trust"], "unauth", "{meta}");
    assert!(
        join_log[0]["trust_note"]
            .as_str()
            .is_some_and(|note| note.starts_with(crate::audit::LANE_BROKEN_NOTE)),
        "{meta}"
    );
    assert_eq!(meta["logChains"]["joinLog"]["status"], "failed", "{meta}");
    assert_eq!(computed(), before + 1);
    // B9: unchanged lane + index → the cached map, not a recomputation.
    let again = parse(&api_records_meta(&state));
    assert_eq!(again["audioLevels"], meta["audioLevels"]);
    assert_eq!(computed(), before + 1, "second call recomputed the map");
    let levels = &meta["audioLevels"];
    assert_eq!(
        levels["vid_1"],
        serde_json::json!({"r": "sound", "max": -12.5, "mean": -30.0, "sec": 10.0, "t": 77, "sc": true, "trust": "sc"})
    );
    assert_eq!(levels["vid_2"]["r"], "stale", "{levels}");
    assert!(levels["vid_2"]["max"].is_null());
    // A new lane entry invalidates the cache.
    let newer = serde_json::json!({
        "schema_version": 1, "event": "audio-level", "measured_unix": 99,
        "method": crate::audio_level::METHOD,
        "threshold_silent_max_dbfs": crate::audio_level::SILENT_MAX_DBFS,
        "ffmpeg_version": "test", "command_template": [],
        "results": [result(&rows[1], 10)],
    });
    crate::audit::append_chained_jsonl_keyed(&log, &newer.to_string(), None).unwrap();
    let updated = parse(&api_records_meta(&state));
    assert_eq!(updated["audioLevels"]["vid_2"]["r"], "sound", "{updated}");
    assert_eq!(computed(), before + 2);
    let _ = std::fs::remove_dir_all(case_dir);
}
