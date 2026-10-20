//! Round-4 track C: process / job lifetime over the workstation API.
//!
//! Every test here fails on the code before round 4 (see each doc
//! comment). External tools are replaced by generated scripts (`/bin/sh`
//! on unix, `.cmd` on Windows) so the job machinery runs for real.

use super::*;

const PLAN_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn temp_case(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r4c-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::create_dir_all(dir.join(crate::join::JOINED_DIR)).unwrap();
    std::fs::create_dir_all(dir.join("evidence/images")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-r4c"}"#).unwrap();
    dir
}

fn bound_state(case_dir: &Path) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case_dir.to_path_buf());
    state.media_roots = vec![case_dir.to_path_buf()];
    state.phase = "review-ready";
    Arc::new(Mutex::new(state))
}

fn request(method: &str, path: &str, body: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: String::new(),
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

fn wait_for(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
    while !check() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn job_end(state: &SharedState, id: &str) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    wait_for(&format!("job {id} to end"), Duration::from_secs(60), || {
        last = parse(&api_export_clip_status(
            &Request {
                query: format!("job={id}"),
                ..request("GET", "/api/export-clip-status", "")
            },
            state,
        ));
        last["job"]["state"] != "running"
    });
    last
}

/// A fake `frametrace` whose `join-video` runs `body` (shell / batch
/// lines); every other subcommand prints ok.
fn fake_cli(dir: &Path, body: &[String]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut text = String::from(
            "#!/bin/sh\n[ \"$1\" = --warm ] && exit 0\nif [ \"$1\" != \"join-video\" ]; then echo ok; exit 0; fi\n",
        );
        for line in body {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str("exit 0\n");
        let path = dir.join("fake-r4c.sh");
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::test_support::warm_unix_script(&path);
        path
    }
    #[cfg(windows)]
    {
        let mut text = String::from(
            "@echo off\r\nif not \"%1\"==\"join-video\" (\r\n  echo ok\r\n  exit /b 0\r\n)\r\n",
        );
        for line in body {
            text.push_str(line);
            text.push_str("\r\n");
        }
        text.push_str("exit /b 0\r\n");
        let path = dir.join("fake-r4c.cmd");
        std::fs::write(&path, text).unwrap();
        path
    }
}

fn copy_line(src: &Path, dst: &Path) -> String {
    if cfg!(windows) {
        format!("copy /y \"{}\" \"{}\" >nul", src.display(), dst.display())
    } else {
        format!("cp \"{}\" \"{}\"", src.display(), dst.display())
    }
}

fn append_line(src: &Path, dst: &Path) -> String {
    if cfg!(windows) {
        format!("type \"{}\" >> \"{}\"", src.display(), dst.display())
    } else {
        format!("cat \"{}\" >> \"{}\"", src.display(), dst.display())
    }
}

fn exit_line(code: i32) -> String {
    if cfg!(windows) {
        format!("exit /b {code}")
    } else {
        format!("exit {code}")
    }
}

fn start_join(state: &SharedState, exe: &Path) -> serde_json::Value {
    let body =
        serde_json::json!({"ids": ["vid_000001", "vid_000002"], "plan_id": PLAN_ID, "tolerance": 1.0})
            .to_string();
    let response = parse(&join_with_exe(
        &request("POST", "/api/join", &body),
        state,
        exe,
    ));
    assert_eq!(response["ok"], true, "{response}");
    response
}

// ---------------------------------------------------------------------------
// C12: `vid_*` ids
// ---------------------------------------------------------------------------

/// C12: a `vid_`-prefixed id that is not the scanner's exact form
/// (`vid_x\..\..\f`) used to skip the approved-roots check and reach the
/// CLI's path resolver; it is now refused before any job exists.
#[test]
fn c12_export_clip_refuses_malformed_vid_ids() {
    let case_dir = temp_case("c12-clip");
    let state = bound_state(&case_dir);
    let exe = case_dir.join("frametrace-not-run");
    for id in [
        "vid_x\\..\\..\\Windows\\win.ini",
        "vid_x/../../etc/passwd",
        "vid_0001",
        "vid_000001/../x",
    ] {
        let body = serde_json::json!({"id": id, "start": "0", "duration": "1"}).to_string();
        let response = parse(&export_clip_with_exe(
            &request("POST", "/api/export-clip", &body),
            &state,
            &exe,
        ));
        assert_eq!(response["ok"], false, "{id}: {response}");
        assert!(
            response["error"]
                .as_str()
                .unwrap_or_default()
                .contains("형식"),
            "{id}: {response}"
        );
    }
    assert!(
        state_lock(&state).clip_jobs.is_empty(),
        "no job may be registered for a malformed id"
    );
    let _ = std::fs::remove_dir_all(case_dir);
}

/// C12: the same rule on the telemetry and proxy routes (which resolved
/// `vid_*` selectors without any roots check at all).
#[test]
fn c12_telemetry_and_proxy_refuse_malformed_vid_ids() {
    let case_dir = temp_case("c12-tel");
    let state = bound_state(&case_dir);
    let body = serde_json::json!({"id": "vid_x/../../../outside.mp4"}).to_string();
    let telemetry = parse(&api_telemetry(
        &request("POST", "/api/telemetry", &body),
        &state,
    ));
    assert_eq!(telemetry["ok"], false, "{telemetry}");
    assert!(
        telemetry["error"]
            .as_str()
            .unwrap_or_default()
            .contains("형식"),
        "{telemetry}"
    );
    let proxy = parse(&api_proxy(&request("POST", "/api/proxy", &body), &state));
    assert_eq!(proxy["ok"], false, "{proxy}");
    assert!(
        proxy["error"].as_str().unwrap_or_default().contains("형식"),
        "{proxy}"
    );
    assert!(!state_lock(&state).busy, "busy released after refusal");
    let _ = std::fs::remove_dir_all(case_dir);
}

// ---------------------------------------------------------------------------
// C13 / C3: join job attribution and crash cleanup
// ---------------------------------------------------------------------------

/// What another process's same-plan join leaves: output, manifest and a
/// log line carrying ITS job tag.
struct Foreign {
    output: PathBuf,
    manifest: PathBuf,
    output_src: PathBuf,
    manifest_src: PathBuf,
    log_src: PathBuf,
}

fn stage_foreign_join(case_dir: &Path, tag: &str) -> Foreign {
    let staging = case_dir.join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let joined = case_dir.join(crate::join::JOINED_DIR);
    let output = joined.join("join_F_20260910-120000_n2.mp4");
    let manifest = joined.join("join_F_20260910-120000_n2.manifest.json");
    let output_src = staging.join("out.mp4");
    std::fs::write(&output_src, b"another-process-joined-bytes").unwrap();
    let sha = crate::audit::digest_file(&output_src).unwrap();
    let manifest_text = format!(
        "{}\n",
        serde_json::json!({"plan_id": PLAN_ID, "output": {"sha256": sha}})
    );
    let manifest_src = staging.join("out.manifest.json");
    std::fs::write(&manifest_src, &manifest_text).unwrap();
    let log_line = serde_json::json!({
        "schema_version": 1,
        "event": "join-video",
        "job_tag": tag,
        "joined_unix": crate::util::now_unix().unwrap() + 3600,
        "output_path": output.to_string_lossy(),
        "output_sha256": sha,
        "manifest_path": manifest.to_string_lossy(),
        "manifest_sha256": crate::sha256::digest_bytes(manifest_text.as_bytes()),
        "plan_id": PLAN_ID,
        "duration_check_ok": true,
    });
    let log_src = staging.join("log.jsonl");
    std::fs::write(&log_src, format!("{log_line}\n")).unwrap();
    Foreign {
        output,
        manifest,
        output_src,
        manifest_src,
        log_src,
    }
}

/// C13: while this job's CLI ran, another process joined the same plan
/// and logged it. The old matcher (same plan, joined after start) adopted
/// that entry and reported the other process's output as this job's
/// ("done"); only an entry carrying this job's tag counts now, and the
/// other output is left alone.
#[test]
fn c13_join_never_adopts_another_processes_log_entry() {
    let case_dir = temp_case("c13");
    let state = bound_state(&case_dir);
    let foreign = stage_foreign_join(&case_dir, "cli-1788000000-4242");
    let log = case_dir
        .join(crate::join::JOINED_DIR)
        .join(crate::join::LOG_FILE);
    let exe = fake_cli(
        &case_dir,
        &[
            copy_line(&foreign.output_src, &foreign.output),
            copy_line(&foreign.manifest_src, &foreign.manifest),
            append_line(&foreign.log_src, &log),
            exit_line(0),
        ],
    );
    let response = start_join(&state, &exe);
    let id = response["job"]["id"].as_str().unwrap().to_string();
    let end = job_end(&state, &id);
    assert_eq!(end["job"]["state"], "failed", "{end}");
    assert!(
        end["job"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("로그에 기록이 없습니다"),
        "{end}"
    );
    assert!(end["path"].is_null(), "no output attributed: {end}");
    assert!(
        foreign.output.exists() && foreign.manifest.exists(),
        "the other process's logged output must survive"
    );
    let _ = std::fs::remove_dir_all(case_dir);
}

/// C3: a join process killed while publishing left a claim naming the
/// output + manifest it published but never logged. The next join's
/// stale sweep now removes them through the claim; the old sweep removed
/// only the `.join-*` temp files and left both behind for good.
#[test]
fn c3_next_join_sweeps_a_dead_jobs_claimed_outputs() {
    let case_dir = temp_case("c3");
    let state = bound_state(&case_dir);
    let joined = case_dir.join(crate::join::JOINED_DIR);
    let output = joined.join("join_F_20260910-120000_n2.mp4");
    let manifest = joined.join("join_F_20260910-120000_n2.manifest.json");
    std::fs::write(&output, b"torn-or-unlogged").unwrap();
    std::fs::write(&manifest, b"").unwrap();
    std::fs::write(joined.join(".join-dead1.lock"), b"").unwrap();
    std::fs::write(
        joined.join(".join-dead1.claim.json"),
        serde_json::json!({
            "output": output.to_string_lossy(),
            "manifest": manifest.to_string_lossy(),
            "job_tag": "dead1",
            "published": true,
            "part_size": 16,
        })
        .to_string(),
    )
    .unwrap();
    // A logged join must never be touched by the sweep.
    let logged = joined.join("join_R_20260101-000000_n3.mp4");
    std::fs::write(&logged, b"logged").unwrap();
    std::fs::write(
        joined.join(crate::join::LOG_FILE),
        format!(
            "{}\n",
            serde_json::json!({"event": "join-video", "output_path": logged.to_string_lossy()})
        ),
    )
    .unwrap();
    let exe = fake_cli(&case_dir, &[exit_line(3)]);
    let response = start_join(&state, &exe);
    let id = response["job"]["id"].as_str().unwrap().to_string();
    let end = job_end(&state, &id);
    assert_eq!(end["job"]["state"], "failed", "{end}");
    assert!(!output.exists(), "dead job's unlogged output must be swept");
    assert!(
        !manifest.exists(),
        "dead job's unlogged manifest must be swept"
    );
    assert!(!joined.join(".join-dead1.claim.json").exists());
    assert!(logged.exists(), "a logged output was touched");
    let logs = state_lock(&state).logs.join("\n");
    assert!(logs.contains("join_F_20260910-120000_n2.mp4"), "{logs}");
    let _ = std::fs::remove_dir_all(case_dir);
}

// ---------------------------------------------------------------------------
// C10: bounded reader joins
// ---------------------------------------------------------------------------

/// R6-CI3: argument prefix carrying the straggler pid file to the C10
/// helper (a libtest filter that matches no test under `--exact`; the
/// helper reads it back from its own argv — `run_step_cancellable` takes
/// no per-child environment).
const C10_PID_FILE_ARG: &str = "ft-c10-pid-file=";

/// R6-CI3 helper, run as a re-executed copy of this test binary (the
/// "step"): starts `sleep 40` in its OWN process group with the step's
/// stdout/stderr inherited, records its pid and returns — the step exits,
/// the straggler keeps the pipes open. Does nothing in a normal run.
///
/// The old fixture was `/bin/sh -c "set -m; sleep 40 & …"`. On Linux CI
/// `/bin/sh` is dash, which turns job control off when it has no
/// terminal ("can't access tty"), so the `sleep` stayed in the step's own
/// group, the leader-exit sweep killed it, the pipe closed, and the test
/// saw no warning (`[]`). `process_group(0)` does not depend on a tty.
#[cfg(unix)]
#[test]
#[allow(clippy::zombie_processes)] // the straggler outlives this process on purpose
fn c10_straggler_helper() {
    use std::os::unix::process::CommandExt;
    let Some(pid_file) = std::env::args().find_map(|arg| {
        arg.strip_prefix(C10_PID_FILE_ARG)
            .map(std::path::PathBuf::from)
    }) else {
        return;
    };
    let straggler = Command::new("sleep")
        .arg("40")
        .process_group(0)
        .spawn()
        .expect("spawn straggler");
    let temp = pid_file.with_extension("tmp");
    std::fs::write(&temp, straggler.id().to_string()).expect("write straggler pid");
    std::fs::rename(&temp, &pid_file).expect("publish straggler pid");
    println!("started");
}

/// Process group of `pid` as `ps` reports it (diagnostics, unix).
#[cfg(unix)]
fn process_group_of(pid: &str) -> String {
    Command::new("ps")
        .args(["-o", "pgid=", "-p", pid.trim()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|err| format!("<ps failed: {err}>"))
}

/// C10: a step whose grandchild escaped and keeps the step's stdout pipe
/// open used to hang `run_step_cancellable` until the grandchild exited
/// (40 s here, forever in the field). Unix: the straggler sits in its own
/// process group, so only the bounded join saves the caller — with a
/// visible warning. Windows: the leader-exit job sweep kills it.
#[test]
fn c10_a_straggler_holding_the_pipe_cannot_hang_the_step() {
    let _serial = super::proc::tree_test_serial();
    let case_dir = temp_case("c10");
    let state = bound_state(&case_dir);
    let pid_file = case_dir.join("straggler.pid");
    let (exe, args): (PathBuf, Vec<String>) = if cfg!(windows) {
        (
            PathBuf::from("cmd"),
            vec![
                "/C".into(),
                "start /b ping -n 40 127.0.0.1 & echo started".into(),
            ],
        )
    } else {
        let module = module_path!();
        let helper = format!(
            "{}::c10_straggler_helper",
            module.split_once("::").map_or(module, |(_, rest)| rest)
        );
        (
            std::env::current_exe().expect("test binary"),
            vec![
                helper,
                "--exact".into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
                format!("{C10_PID_FILE_ARG}{}", pid_file.display()),
            ],
        )
    };
    let started = std::time::Instant::now();
    let result = run_step_cancellable(&exe, &args, &|| false, &state);
    let elapsed = started.elapsed();
    assert!(result.is_ok(), "{result:?}");
    assert!(
        elapsed < Duration::from_secs(30),
        "the step waited {elapsed:?} for the straggler"
    );
    #[cfg(unix)]
    {
        let straggler = std::fs::read_to_string(&pid_file).unwrap_or_default();
        let warnings = state_lock(&state).warnings.clone();
        let facts = || {
            format!(
                "straggler pid {:?} (pid file {}), its process group {:?}, step output {:?}, elapsed {elapsed:?}, warnings {warnings:?}",
                straggler.trim(),
                pid_file.display(),
                process_group_of(&straggler),
                result.as_deref().unwrap_or_default(),
            )
        };
        assert!(
            !straggler.trim().is_empty(),
            "the helper never started its straggler: {}",
            facts()
        );
        assert!(
            warnings
                .iter()
                .any(|(_, message)| message.contains("출력 파이프")),
            "no abandoned-pipe warning: {}",
            facts()
        );
        let _ = Command::new("kill").args(["-9", straggler.trim()]).status();
    }
    let _ = std::fs::remove_dir_all(case_dir);
}

// ---------------------------------------------------------------------------
// C1 / C2 / C4: the server sweeps a killed step's `.part` files
// ---------------------------------------------------------------------------

/// A fake step that drops `.evidence.raw.ft<own pid>.part` into
/// `<case>/evidence/images` (like a killed ewfexport) and then hangs.
fn part_writing_step(dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-step.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\n[ \"$1\" = --warm ] && exit 0\necho torn > \"$2/evidence/images/.evidence.raw.ft$$.part.raw\"\nsleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::test_support::warm_unix_script(&path);
        path
    }
    #[cfg(windows)]
    {
        // cmd has no `$$`: a PowerShell helper names the part after its
        // parent (this cmd.exe — the tracked step process).
        let helper = dir.join("fake-step.ps1");
        std::fs::write(
            &helper,
            "$me = (Get-CimInstance Win32_Process -Filter \"ProcessId=$PID\").ParentProcessId\r\n\
             Set-Content -Path (Join-Path $args[0] \"evidence\\images\\.evidence.raw.ft$me.part.raw\") -Value torn\r\n",
        )
        .unwrap();
        let path = dir.join("fake-step.cmd");
        std::fs::write(
            &path,
            format!(
                "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" \"%~2\"\r\n:spin\r\nping -n 2 127.0.0.1 >nul 2>&1\r\ngoto spin\r\n",
                helper.display()
            ),
        )
        .unwrap();
        path
    }
}

/// C1/C4: cancelling a step that was writing a `.part` (an E01 export, an
/// icat recovery, a clip) removes exactly that process's in-progress
/// files. Before, the torn `.part` (and, before C1, the torn image under
/// its final name) stayed in the case.
#[test]
fn c4_cancelled_step_parts_are_swept() {
    let _serial = super::proc::tree_test_serial();
    let case_dir = temp_case("c4-sweep");
    let state = bound_state(&case_dir);
    let exe = part_writing_step(&case_dir);
    let images = case_dir.join("evidence/images");
    // Another process's in-progress file must survive.
    let foreign = images.join(".other.raw.ft1.part");
    std::fs::write(&foreign, b"live").unwrap();
    let args: Vec<String> = vec![
        "import-e01".into(),
        case_dir.to_string_lossy().to_string(),
        "x.E01".into(),
    ];
    let has_part = || {
        std::fs::read_dir(&images).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with(".evidence.raw.ft") && name.contains(".part")
            })
        })
    };
    let result = run_step_cancellable(&exe, &args, &has_part, &state);
    assert!(result.is_err(), "{result:?}");
    assert!(!has_part(), "the killed step's .part must be swept");
    assert!(foreign.exists(), "another process's part was removed");
    let logs = state_lock(&state).logs.join("\n");
    assert!(logs.contains("임시 파일을 지웠습니다"), "{logs}");
    let _ = std::fs::remove_dir_all(case_dir);
}

// ---------------------------------------------------------------------------
// C4 / C5: selection export
// ---------------------------------------------------------------------------

/// C4: from its first byte the bundle says "in progress" (note file and
/// `manifest-audit.json` state) and no copy is ever visible under its
/// final name before it is complete; the finished bundle drops the note.
#[test]
fn c4_export_bundle_reads_in_progress_until_finished() {
    let case_dir = temp_case("c4-export");
    let state = bound_state(&case_dir);
    let source = case_dir.join("clip.mp4");
    std::fs::write(&source, vec![7u8; 64 * 1024]).unwrap();
    let body = serde_json::json!({"items": [
        {"id": "vid_000001", "path": source.to_string_lossy()},
        {"id": "vid_000002", "path": source.to_string_lossy()},
    ]})
    .to_string();
    let seen = Mutex::new(Vec::<String>::new());
    let exports = case_dir.join("exports");
    let response = parse(&super::media::api_export_selected_with(
        &request("POST", "/api/export-selected", &body),
        &state,
        &|_| {
            let dir = std::fs::read_dir(&exports)
                .unwrap()
                .flatten()
                .next()
                .unwrap()
                .path();
            let marker = dir.join(super::media::EXPORT_IN_PROGRESS_FILE).is_file();
            let audit =
                std::fs::read_to_string(dir.join("manifest-audit.json")).unwrap_or_default();
            seen.lock()
                .unwrap()
                .push(format!("{marker}|{}", audit.contains("in-progress")));
        },
    ));
    assert_eq!(response["ok"], true, "{response}");
    let seen = seen.into_inner().unwrap();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|entry| entry == "true|true"),
        "in-progress state missing mid-export: {seen:?}"
    );
    let dir = PathBuf::from(response["export_dir"].as_str().unwrap());
    assert!(!dir.join(super::media::EXPORT_IN_PROGRESS_FILE).exists());
    let audit = std::fs::read_to_string(dir.join("manifest-audit.json")).unwrap();
    assert!(audit.contains("\"state\":\"complete\""), "{audit}");
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        !names.iter().any(|n| n.contains(".part")),
        "leftover parts: {names:?}"
    );
    let manifest = std::fs::read_to_string(dir.join("manifest.jsonl")).unwrap();
    for line in manifest.lines() {
        let row = parse(line);
        assert_eq!(row["skipped"], false, "{row}");
        assert_eq!(
            row["sha256_copy"].as_str().unwrap().len(),
            64,
            "C5: a copied row always carries its hash: {row}"
        );
    }
    let _ = std::fs::remove_dir_all(case_dir);
}

/// C4: the shell's forced-exit path stamps a bundle that is still being
/// written as "중단됨" (note + aborted manifest-audit); a finished bundle
/// is no longer registered and is never stamped.
#[test]
fn c4_abandoned_exports_are_marked_incomplete() {
    // R13-S14: serialized with the S14 test (both stamp every export).
    let _serial = super::media::ABANDONED_EXPORTS_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let case_dir = temp_case("c4-abandon");
    let dir = case_dir.join("exports/selection-1");
    std::fs::create_dir_all(&dir).unwrap();
    let registration = super::media::begin_export_bundle(&dir, 1, 3).unwrap();
    assert!(dir.join(super::media::EXPORT_IN_PROGRESS_FILE).is_file());
    assert!(super::mark_abandoned_exports() >= 1);
    let note = std::fs::read_to_string(dir.join("EXPORT-INCOMPLETE.txt")).unwrap();
    assert!(note.contains("중단됨"), "{note}");
    let audit = std::fs::read_to_string(dir.join("manifest-audit.json")).unwrap();
    assert!(audit.contains("aborted"), "{audit}");
    drop(registration);
    std::fs::remove_file(dir.join("EXPORT-INCOMPLETE.txt")).unwrap();
    super::mark_abandoned_exports();
    assert!(
        !dir.join("EXPORT-INCOMPLETE.txt").exists(),
        "a deregistered bundle must not be stamped"
    );
    let _ = std::fs::remove_dir_all(case_dir);
}

/// C5: a copy that fails mid-way (reading a directory fails after open on
/// unix) is a failed item with nothing left in the bundle — the old code
/// had already created the final name and left an empty "copy" behind.
#[cfg(unix)]
#[test]
fn c5_failed_copy_leaves_nothing_under_any_name() {
    let case_dir = temp_case("c5");
    let out = case_dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let source_dir = case_dir.join("not-a-file");
    std::fs::create_dir_all(&source_dir).unwrap();
    let err = super::media::copy_hashed_unique(&source_dir, &out, "vid_000001__x.mp4")
        .err()
        .expect("copying a directory must fail");
    assert!(err.starts_with("copy-failed"), "{err}");
    let left: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
    assert!(
        left.is_empty(),
        "{:?}",
        left.iter().map(|e| e.path()).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(case_dir);
}

// ---------------------------------------------------------------------------
// C2: package-case
// ---------------------------------------------------------------------------

/// C2: a package-case that fails leaves neither a half-written
/// `reports/package_*` folder (the old code had already created and
/// partly filled it) nor its in-progress directory.
#[test]
fn c2_failed_package_leaves_no_package_folder() {
    let case_dir = temp_case("c2");
    std::fs::create_dir_all(case_dir.join("reports")).unwrap();
    let result = crate::package::package_case(&case_dir, None);
    assert!(result.is_err(), "a case without its required files fails");
    let left: Vec<String> = std::fs::read_dir(case_dir.join("reports"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(left.is_empty(), "leftovers: {left:?}");
    let _ = std::fs::remove_dir_all(case_dir);
}
