//! Track S4/S6: `/api/tools*` — listing, refusals, busy handling, the
//! install worker running entirely outside the state lock (against the
//! unit tests' local fake source, never the internet), and the
//! deepfake `tool_missing` refusal.

use super::*;
use crate::tool_install::{self, InstallDirs, NetPolicy};

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

fn new_state() -> SharedState {
    Arc::new(Mutex::new(JobState::new()))
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("{err}: {text}"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-tools-api-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn production_env(tools_dir: PathBuf) -> tools::InstallEnv {
    let manifest = tool_install::manifest().unwrap().clone();
    let net = NetPolicy::for_manifest(&manifest);
    tools::InstallEnv {
        manifest,
        dirs: InstallDirs { tools_dir },
        net,
    }
}

#[test]
fn api_tools_lists_pinned_tools_and_flags_on_demand_ones() {
    let state = new_state();
    let body = parse(&tools::api_tools(
        &request("GET", "/api/tools", "", ""),
        &state,
    ));
    assert_eq!(body["ok"], true);
    assert!(body["install"].is_null());
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["ffmpeg", "libewf", "sleuthkit", "deepfake-lens"]);
    let ffmpeg = &body["tools"][0];
    assert_eq!(ffmpeg["required"], true);
    assert_eq!(ffmpeg["download"]["host"], "github.com");
    assert_eq!(ffmpeg["download"]["size"], 170_611_883u64);
    assert!(ffmpeg["paths"].get("ffprobe").is_some());
    // Track U names.
    assert_eq!(ffmpeg["source_host"], "github.com");
    assert_eq!(ffmpeg["size_bytes"], 170_611_883u64);
    assert_eq!(ffmpeg["pinned_version"], "n8.1.3-9-g29e619e767");
    assert!(
        ffmpeg["license"]
            .as_str()
            .is_some_and(|l| l.starts_with("LGPL-3.0-or-later (FFmpeg)")),
        "{}",
        ffmpeg["license"]
    );
    assert_eq!(ffmpeg["title"], "FFmpeg");
    assert_eq!(ffmpeg["label"], "FFmpeg");
    // Option C (T5): the distribution notice the dialog shows.
    assert!(ffmpeg["notice"].as_str().is_some_and(|n| !n.is_empty()));
    assert!(ffmpeg["notice_en"].as_str().is_some_and(|n| !n.is_empty()));
    assert_eq!(ffmpeg["on_demand"], false);
    let libewf = &body["tools"][1];
    assert_eq!(libewf["installable"], false);
    assert!(libewf["download"].is_null());
    assert!(libewf["unavailable_reason"].as_str().is_some());
    assert!(
        libewf["manual_hint"]
            .as_str()
            .unwrap()
            .contains("tools/bin")
    );

    let all = parse(&tools::api_tools(
        &request("GET", "/api/tools", "exclude_on_demand=1", ""),
        &state,
    ));
    assert_eq!(all["tools"].as_array().unwrap().len(), 3);
    let lens = &body["tools"][3];
    assert_eq!(lens["on_demand"], true);
    assert_eq!(lens["installable"], false);
    assert!(lens["unavailable_reason"].as_str().is_some());
    assert_eq!(lens["source_host"], "pypi.org");
    assert!(lens["size_bytes"].is_null());
}

#[test]
fn install_refusals_carry_stable_codes_and_start_nothing() {
    let base = scratch("refusals");
    let state = new_state();
    let refused = |name: &str| {
        parse(&tools::start_install(
            &state,
            name,
            production_env(base.join("tools")),
        ))
    };
    assert_eq!(refused("no-such-tool")["code"], "unknown_tool");
    let libewf = refused("libewf");
    let expected = if cfg!(all(windows, target_arch = "x86_64")) {
        "not_installable"
    } else {
        "unsupported_platform"
    };
    assert_eq!(libewf["code"], expected);
    assert!(libewf["manual_hint"].as_str().is_some());
    let mut codes = vec!["unknown_tool", expected];
    if !cfg!(windows) {
        assert_eq!(refused("ffmpeg")["code"], "unsupported_platform");
        codes.push("unsupported_platform");
    }
    assert!(state_lock(&state).tool_install.is_none());
    assert!(!state_lock(&state).busy);
    // Each refusal is one chained audit line; nothing else is written.
    crate::tool_install::tests::assert_only_refusals(
        &InstallDirs {
            tools_dir: base.join("tools"),
        },
        &codes,
    );
    assert_eq!(
        parse(&tools::api_tools_install_cancel(
            &request("POST", "/api/tools/install-cancel", "", "{}"),
            &state
        ))["code"],
        "no_install"
    );
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn cancel_during_a_non_cancellable_stage_is_refused_with_its_own_code() {
    let state = new_state();
    let progress = Arc::new(crate::tool_install::Progress::default());
    {
        let mut guard = state_lock(&state);
        guard.tool_install = Some(tools::ToolInstallJob {
            id: 1,
            name: "ffmpeg".into(),
            state: "running",
            error: None,
            progress: Arc::clone(&progress),
            started_unix: 0,
            finished_unix: None,
            installed_files: Vec::new(),
            audit_warning: None,
            staging_left: None,
        });
    }
    // Downloading: honoured.
    progress.set_stage(crate::tool_install::Stage::Download);
    let ok = parse(&tools::api_tools_install_cancel(
        &request("POST", "/api/tools/install-cancel", "", "{}"),
        &state,
    ));
    assert_eq!(ok["ok"], true, "{ok}");
    assert!(progress.cancelled());

    // Moving files into place: refused, nothing changes.
    let progress = Arc::new(crate::tool_install::Progress::default());
    progress
        .enter_non_cancellable(crate::tool_install::Stage::Install)
        .unwrap();
    state_lock(&state).tool_install.as_mut().unwrap().progress = Arc::clone(&progress);
    let refused = parse(&tools::api_tools_install_cancel(
        &request("POST", "/api/tools/install-cancel", "", "{}"),
        &state,
    ));
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["code"], "not_cancellable", "{refused}");
    assert_eq!(refused["install"]["stage"], "install");
    assert_eq!(refused["install"]["state"], "extracting");
    assert!(!progress.cancelled());
}

fn running_job(state: &SharedState, id: u64) -> Arc<crate::tool_install::Progress> {
    let progress = Arc::new(crate::tool_install::Progress::default());
    progress.set_stage(crate::tool_install::Stage::Download);
    state_lock(state).tool_install = Some(tools::ToolInstallJob {
        id,
        name: "ffmpeg".into(),
        state: "running",
        error: None,
        progress: Arc::clone(&progress),
        started_unix: 0,
        finished_unix: None,
        installed_files: Vec::new(),
        audit_warning: None,
        staging_left: None,
    });
    progress
}

/// Fix round 11 L3: the env cache is keyed by a tools/bin fingerprint — a
/// file added, resized or touched there (a CLI install while the
/// workstation runs) misses the cache and drops the stale entry.
#[test]
fn the_env_cache_misses_when_tools_bin_changes() {
    use super::system::{EnvCacheEntry, env_cache_hit, env_cache_store, tools_bin_fingerprint};
    let base = scratch("env-fp");
    let bin = base.join("tools/bin");
    assert_eq!(tools_bin_fingerprint(&bin), "", "a missing dir");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("ffprobe.exe"), b"old").unwrap();
    let cache: Mutex<Option<EnvCacheEntry>> = Mutex::new(None);
    let before = tools_bin_fingerprint(&bin);
    assert_eq!(tools_bin_fingerprint(&bin), before, "stable");
    env_cache_store(&cache, before.clone(), "{\"ffmpeg\":false}".into());
    assert_eq!(
        env_cache_hit(&cache, &before).as_deref(),
        Some("{\"ffmpeg\":false}")
    );
    // A new binary (the CLI install).
    std::fs::write(bin.join("ffmpeg.exe"), b"MZ").unwrap();
    let added = tools_bin_fingerprint(&bin);
    assert_ne!(added, before);
    assert!(env_cache_hit(&cache, &added).is_none(), "miss");
    assert!(
        cache.lock().unwrap().is_none(),
        "the stale entry is dropped"
    );
    // A replaced binary of another size.
    env_cache_store(&cache, added.clone(), "{}".into());
    std::fs::write(bin.join("ffmpeg.exe"), b"MZ new build").unwrap();
    assert!(env_cache_hit(&cache, &tools_bin_fingerprint(&bin)).is_none());
    // Same size, newer mtime.
    let same = tools_bin_fingerprint(&bin);
    env_cache_store(&cache, same.clone(), "{}".into());
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(bin.join("ffmpeg.exe"))
        .unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    drop(file);
    assert!(env_cache_hit(&cache, &tools_bin_fingerprint(&bin)).is_none());
    let _ = std::fs::remove_dir_all(base);
}

/// Fix round 12 L1: a pip tool (deepfake-lens) installed from a console
/// lands in `tools/python/<name>/Scripts` — the cache fingerprint covers
/// that too, so `/api/env` re-probes instead of `deepfake: false`.
#[test]
fn the_env_cache_misses_when_a_private_venv_appears_or_changes() {
    use super::system::{EnvCacheEntry, env_cache_hit, env_cache_store, tools_fingerprint};
    let base = scratch("env-fp-py");
    let bin = base.join("tools/bin");
    let python = base.join("tools/python");
    let scripts = if cfg!(windows) { "Scripts" } else { "bin" };
    std::fs::create_dir_all(&bin).unwrap();
    let cache: Mutex<Option<EnvCacheEntry>> = Mutex::new(None);
    let empty = tools_fingerprint(&bin, &python);
    assert_eq!(tools_fingerprint(&bin, &python), empty, "stable");
    env_cache_store(&cache, empty.clone(), "{\"deepfake\":false}".into());
    assert!(env_cache_hit(&cache, &empty).is_some());
    // The venv appears (tools/bin untouched).
    let venv = python.join("deepfake-lens");
    std::fs::create_dir_all(venv.join(scripts)).unwrap();
    let with_venv = tools_fingerprint(&bin, &python);
    assert_ne!(with_venv, empty);
    assert!(env_cache_hit(&cache, &with_venv).is_none(), "miss");
    assert!(
        cache.lock().unwrap().is_none(),
        "the stale entry is dropped"
    );
    // pip adds the console script: the Scripts dir changes (its mtime is
    // pushed forward explicitly — directory mtime granularity is coarse
    // on some file systems).
    env_cache_store(&cache, with_venv.clone(), "{}".into());
    std::fs::write(venv.join(scripts).join("deepfake-lens.exe"), b"MZ").unwrap();
    #[cfg(windows)]
    let dir = {
        // A directory handle needs FILE_FLAG_BACKUP_SEMANTICS on Windows.
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(0x0200_0000)
            .open(venv.join(scripts))
            .unwrap()
    };
    #[cfg(not(windows))]
    let dir = std::fs::File::open(venv.join(scripts)).unwrap();
    dir.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    drop(dir);
    let with_script = tools_fingerprint(&bin, &python);
    assert_ne!(
        with_script, with_venv,
        "the Scripts dir mtime is part of it"
    );
    assert!(env_cache_hit(&cache, &with_script).is_none(), "miss");
    // Fix round 13 H2: transient sweep names do not change it.
    std::fs::create_dir_all(python.join(".deepfake-lens.old-00ff").join(scripts)).unwrap();
    std::fs::create_dir_all(python.join(".x.old-01.rm")).unwrap();
    std::fs::create_dir_all(python.join("leftover.rm")).unwrap();
    assert_eq!(
        tools_fingerprint(&bin, &python),
        with_script,
        "transient names ignored"
    );
    // Removing the venv misses as well.
    env_cache_store(&cache, with_script.clone(), "{}".into());
    std::fs::remove_dir_all(&venv).unwrap();
    assert!(env_cache_hit(&cache, &tools_fingerprint(&bin, &python)).is_none());
    let _ = std::fs::remove_dir_all(base);
}

/// Fix round 11 M1: the global `/api/cancel` ([종료] on the home screen)
/// while a tool install holds `busy` asks the install's own latch — never
/// the job path (`cancel_requested` + "현재 단계를 멈추는 중" in the log,
/// which the install worker never reads). During the move stage it is
/// `not_cancellable`; a worker that already ended is just acknowledged.
#[test]
fn the_global_cancel_stops_a_tool_install_through_its_own_latch() {
    const JOB_LINE: &str = "현재 단계를 멈추는 중";
    let state = new_state();
    let _lease = try_lease_busy(&state).unwrap();
    let progress = running_job(&state, 3);
    let reply = parse(&api_cancel(&state));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["cancelled"], "tool-install", "{reply}");
    assert_eq!(reply["job_kind"], "tool-install");
    assert_eq!(reply["requested"], true, "{reply}");
    assert_eq!(reply["install"]["id"], 3);
    assert!(progress.cancelled(), "the install's latch is set");
    {
        let guard = state_lock(&state);
        assert!(!guard.cancel_requested, "the job path is untouched");
        assert!(
            !guard.logs.iter().any(|line| line.contains(JOB_LINE)),
            "no false job-cancel line: {:?}",
            guard.logs
        );
    }

    // Moving files into place: refused with its own code, nothing set.
    let progress = Arc::new(crate::tool_install::Progress::default());
    progress
        .enter_non_cancellable(crate::tool_install::Stage::Install)
        .unwrap();
    state_lock(&state).tool_install.as_mut().unwrap().progress = Arc::clone(&progress);
    let logs_before = state_lock(&state).logs.len();
    let refused = parse(&api_cancel(&state));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["code"], "not_cancellable", "{refused}");
    assert_eq!(refused["job_kind"], "tool-install");
    assert!(!progress.cancelled());
    let guard = state_lock(&state);
    assert!(!guard.cancel_requested);
    assert_eq!(guard.logs.len(), logs_before, "nothing logged");
    drop(guard);

    // The worker already finished (still publishing): acknowledged, quiet.
    let progress = running_job(&state, 4);
    progress.set_stage(crate::tool_install::Stage::Done);
    let reply = parse(&api_cancel(&state));
    assert_eq!(reply["cancelled"], "tool-install", "{reply}");
    // Fix round 15: nothing was asked — the quit dialog must not say
    // the install was cancelled.
    assert_eq!(reply["requested"], false, "{reply}");
    assert!(!progress.cancelled());
    assert!(!state_lock(&state).cancel_requested);
}

/// Fix round 4: a cancel naming the current job after it already ended —
/// published (`done` / `failed`), or finished by the worker (stage done /
/// failed) while it still writes the audit line — is `no_install`, never
/// accepted ("중단하는 중…" for a finished install).
#[test]
fn a_cancel_of_a_finished_job_is_no_install() {
    let state = new_state();
    let cancel = |body: &str| {
        parse(&tools::api_tools_install_cancel(
            &request("POST", "/api/tools/install-cancel", "", body),
            &state,
        ))
    };
    for stage in [
        crate::tool_install::Stage::Done,
        crate::tool_install::Stage::Failed,
    ] {
        for body in [r#"{"id":5}"#, "{}"] {
            let progress = running_job(&state, 5);
            progress.set_stage(stage);
            let reply = cancel(body);
            assert_eq!(reply["ok"], false, "{stage:?} {body}: {reply}");
            assert_eq!(reply["code"], "no_install", "{stage:?} {body}: {reply}");
            assert!(!progress.cancelled(), "{stage:?} {body}");
        }
    }
    for published in ["done", "failed"] {
        let progress = running_job(&state, 6);
        progress.set_stage(crate::tool_install::Stage::Download);
        state_lock(&state).tool_install.as_mut().unwrap().state = published;
        let reply = cancel(r#"{"id":6}"#);
        assert_eq!(reply["code"], "no_install", "{published}: {reply}");
        assert!(!progress.cancelled());
    }
    // Still running: accepted as before.
    let progress = running_job(&state, 7);
    assert_eq!(cancel(r#"{"id":7}"#)["ok"], true);
    assert!(progress.cancelled());
}

/// Fix round 3: a late [중단] from tab A (still showing job 1) never stops
/// job 2 that tab B started meanwhile — the body names the job.
#[test]
fn a_cancel_naming_another_job_is_refused_as_stale() {
    let state = new_state();
    let cancel = |body: &str| {
        parse(&tools::api_tools_install_cancel(
            &request("POST", "/api/tools/install-cancel", "", body),
            &state,
        ))
    };
    let progress = running_job(&state, 2);
    let stale = cancel(r#"{"id":1}"#);
    assert_eq!(stale["ok"], false, "{stale}");
    assert_eq!(stale["code"], "stale_job", "{stale}");
    assert_eq!(stale["install"]["id"], 2);
    assert!(!progress.cancelled(), "the new job keeps running");
    // The current id (number or string) is honoured.
    let ok = cancel(r#"{"id":2}"#);
    assert_eq!(ok["ok"], true, "{ok}");
    assert!(progress.cancelled());
    let progress = running_job(&state, 3);
    assert_eq!(cancel(r#"{"id":"3"}"#)["ok"], true);
    assert!(progress.cancelled());
    // No id (an older client): the running job, as before.
    let progress = running_job(&state, 4);
    assert_eq!(cancel("{}")["ok"], true);
    assert!(progress.cancelled());
    // Nothing installed at all: no_install, whatever the id.
    let empty = new_state();
    let none = parse(&tools::api_tools_install_cancel(
        &request("POST", "/api/tools/install-cancel", "", r#"{"id":9}"#),
        &empty,
    ));
    assert_eq!(none["code"], "no_install");
}

/// Fix round 3: the server's clock travels with the job (`now_unix` on
/// both GETs, `age_s` on a finished job) so a tab never judges "recent"
/// by its own clock; the job JSON has one `cancelled` key.
#[test]
fn tool_replies_carry_the_server_clock_and_the_job_age() {
    let state = new_state();
    let now = crate::util::now_unix().unwrap();
    for body in [
        tools::api_tools(&request("GET", "/api/tools", "", ""), &state),
        tools::api_tools_install_status(&state),
    ] {
        let value = parse(&body);
        let server_now = value["now_unix"].as_u64().expect("now_unix");
        assert!(server_now.abs_diff(now) <= 5, "{value}");
    }
    let progress = running_job(&state, 1);
    let running = parse(&tools::api_tools_install_status(&state));
    assert!(running["install"]["age_s"].is_null(), "{running}");
    {
        let mut guard = state_lock(&state);
        let job = guard.tool_install.as_mut().unwrap();
        job.state = "failed";
        job.error = Some(crate::tool_install::INSTALL_CANCELLED.into());
        job.finished_unix = Some(now - 400);
    }
    progress.set_stage(crate::tool_install::Stage::Failed);
    let done = parse(&tools::api_tools_install_status(&state));
    let age = done["install"]["age_s"].as_u64().expect("age_s");
    assert!((400..=410).contains(&age), "{done}");
    assert_eq!(done["install"]["cancelled"], true);
    // One `cancelled` key in the job JSON (a duplicate would silently win).
    let src = include_str!("tools.rs");
    let body = &src[src.find("fn install_job_json").unwrap()..];
    let body = &body[..body.find("\n}").unwrap()];
    assert_eq!(body.matches("\"cancelled\":").count(), 1, "{body}");
}

#[test]
fn deepfake_start_without_the_tool_is_a_distinct_tool_missing_refusal() {
    if crate::deepfake::lens_available() {
        eprintln!("SKIPPED: deepfake-lens resolves on this machine");
        return;
    }
    let refusal = parse(&tools::deepfake_missing_refusal().unwrap());
    assert_eq!(refusal["ok"], false);
    assert_eq!(refusal["code"], "tool_missing");
    assert_eq!(refusal["tool_missing"], "deepfake-lens");
    assert_eq!(refusal["installable"], false);
    assert_eq!(refusal["install"]["path"], "/api/tools/install");

    let base = scratch("deepfake");
    let source = base.join("source");
    std::fs::create_dir_all(&source).unwrap();
    let body = serde_json::json!({
        "input_kind": "folder",
        "source_path": source.display().to_string(),
        "case_dir": base.join("case").display().to_string(),
        "with_deepfake": true,
    })
    .to_string();
    let state = new_state();
    let started = parse(&api_start(
        &request("POST", "/api/start", "", &body),
        &state,
    ));
    assert_eq!(started["code"], "tool_missing", "{started}");
    assert!(!state_lock(&state).busy, "nothing may start");
    let _ = std::fs::remove_dir_all(base);
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::tool_install::tests::fake::{self, Reply};
    use std::collections::HashMap;
    use std::time::Instant;

    fn fake_env(tools_dir: PathBuf, tool: crate::tool_install::ToolEntry) -> tools::InstallEnv {
        tools::InstallEnv {
            manifest: crate::tool_install::Manifest {
                schema: 1,
                allowed_hosts: vec!["127.0.0.1".into()],
                tools: vec![tool],
            },
            dirs: InstallDirs { tools_dir },
            net: NetPolicy::test_loopback(&["127.0.0.1"]),
        }
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "timed out: {what}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn busy_workstation_refuses_an_install_before_any_download() {
        let base = scratch("busy");
        let state = new_state();
        let _lease = try_lease_busy(&state).unwrap();
        let refused = parse(&tools::start_install(
            &state,
            "ffmpeg",
            production_env(base.join("tools")),
        ));
        assert_eq!(refused["code"], "busy", "{refused}");
        assert!(state_lock(&state).tool_install.is_none());
        crate::tool_install::tests::assert_only_refusals(
            &InstallDirs {
                tools_dir: base.join("tools"),
            },
            &["busy"],
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn install_runs_outside_the_state_lock_and_reports_progress() {
        let bytes = fake::big_zip();
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let source = fake::serve(HashMap::new());
        source.route("/fake.zip", Reply::Gated(bytes.clone(), Arc::clone(&gate)));
        let tool = crate::tool_install::tests::test_archive_entry(&source.url("/fake.zip"), &bytes);
        let base = scratch("lockfree");
        let state = new_state();
        let started = parse(&tools::start_install(
            &state,
            "faketool",
            fake_env(base.join("tools"), tool.clone()),
        ));
        assert_eq!(started["ok"], true, "{started}");
        assert_eq!(started["install"]["state"], "downloading", "never idle");
        assert_eq!(started["install"]["tool"], "faketool");

        // Mid-download (half the body sent, the source is holding the
        // rest): the state lock is free, `busy` is held, progress moves.
        wait_until("download progress", || {
            let status = parse(&tools::api_tools_install_status(&state));
            status["install"]["stage"] == "download"
                && status["install"]["bytes"].as_u64().unwrap_or(0) > 0
        });
        for _ in 0..20 {
            assert!(
                state.try_lock().is_ok(),
                "state lock held during the download"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(state_lock(&state).busy, "install must hold the busy lease");
        let status = parse(&tools::api_tools_install_status(&state));
        assert_eq!(status["install"]["total"], bytes.len() as u64);
        // A second install while one runs is refused with its own code.
        let second = parse(&tools::start_install(
            &state,
            "faketool",
            fake_env(base.join("tools"), tool),
        ));
        assert_eq!(second["code"], "install_running");

        fake::open_gate(&gate);
        wait_until("install finished", || {
            state_lock(&state)
                .tool_install
                .as_ref()
                .is_some_and(|job| job.state != "running")
        });
        let status = parse(&tools::api_tools_install_status(&state));
        assert_eq!(status["install"]["state"], "done", "{status}");
        assert_eq!(status["install"]["stage"], "done");
        assert_eq!(status["install"]["cancelled"], false);
        assert!(status["install"]["staging_left"].is_null(), "{status}");
        assert_eq!(
            status["install"]["installed_files"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(!state_lock(&state).busy, "busy released");
        assert!(base.join("tools/bin/fake.exe").is_file());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_cancel_mid_download_stops_the_install_and_cleans_up() {
        let bytes = fake::big_zip();
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let source = fake::serve(HashMap::new());
        source.route("/fake.zip", Reply::Gated(bytes.clone(), Arc::clone(&gate)));
        let tool = crate::tool_install::tests::test_archive_entry(&source.url("/fake.zip"), &bytes);
        let base = scratch("cancel");
        let state = new_state();
        let started = parse(&tools::start_install(
            &state,
            "faketool",
            fake_env(base.join("tools"), tool),
        ));
        assert_eq!(started["ok"], true, "{started}");
        wait_until("download progress", || {
            let status = parse(&tools::api_tools_install_status(&state));
            status["install"]["stage"] == "download"
                && status["install"]["bytes"].as_u64().unwrap_or(0) > 0
        });
        let cancel = parse(&tools::api_tools_install_cancel(
            &request("POST", "/api/tools/install-cancel", "", "{}"),
            &state,
        ));
        assert_eq!(cancel["ok"], true, "{cancel}");
        wait_until("install stopped", || {
            state_lock(&state)
                .tool_install
                .as_ref()
                .is_some_and(|job| job.state != "running")
        });
        fake::open_gate(&gate);
        let status = parse(&tools::api_tools_install_status(&state));
        assert_eq!(status["install"]["state"], "failed", "{status}");
        assert_eq!(
            status["install"]["error"],
            crate::tool_install::INSTALL_CANCELLED
        );
        assert_eq!(status["install"]["cancelled"], true, "{status}");
        assert!(!state_lock(&state).busy);
        assert!(!base.join("tools/bin/fake.exe").exists());
        let staged = std::fs::read_dir(base.join("tools"))
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with(".staging-"));
        assert!(!staged, "staging left behind");
        // A finished job has nothing to cancel.
        assert_eq!(
            parse(&tools::api_tools_install_cancel(
                &request("POST", "/api/tools/install-cancel", "", "{}"),
                &state
            ))["code"],
            "no_install"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    /// Fix round 11 M1, end to end: [종료] → `/api/cancel` mid-download
    /// stops the real worker (INSTALL_CANCELLED, `busy` released, so
    /// `/api/shutdown` is accepted) and logs no false job-cancel line.
    #[test]
    fn the_global_cancel_ends_a_running_install_and_shutdown_proceeds() {
        let bytes = fake::big_zip();
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let source = fake::serve(HashMap::new());
        source.route("/fake.zip", Reply::Gated(bytes.clone(), Arc::clone(&gate)));
        let tool = crate::tool_install::tests::test_archive_entry(&source.url("/fake.zip"), &bytes);
        let base = scratch("global-cancel");
        let state = new_state();
        let started = parse(&tools::start_install(
            &state,
            "faketool",
            fake_env(base.join("tools"), tool),
        ));
        assert_eq!(started["ok"], true, "{started}");
        wait_until("download progress", || {
            let status = parse(&tools::api_tools_install_status(&state));
            status["install"]["stage"] == "download"
                && status["install"]["bytes"].as_u64().unwrap_or(0) > 0
        });
        assert_eq!(parse(&api_shutdown(&state))["running"], true);
        let cancel = parse(&api_cancel(&state));
        assert_eq!(cancel["ok"], true, "{cancel}");
        assert_eq!(cancel["cancelled"], "tool-install", "{cancel}");
        wait_until("install stopped", || !state_lock(&state).busy);
        fake::open_gate(&gate);
        let status = parse(&tools::api_tools_install_status(&state));
        assert_eq!(status["install"]["state"], "failed", "{status}");
        assert_eq!(
            status["install"]["error"],
            crate::tool_install::INSTALL_CANCELLED
        );
        assert_eq!(status["install"]["cancelled"], true);
        {
            let guard = state_lock(&state);
            assert!(!guard.cancel_requested);
            assert!(
                !guard
                    .logs
                    .iter()
                    .any(|line| line.contains("현재 단계를 멈추는 중")),
                "{:?}",
                guard.logs
            );
        }
        assert!(!base.join("tools/bin/fake.exe").exists());
        assert_eq!(parse(&api_shutdown(&state))["ok"], true);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_failed_install_is_reported_and_releases_busy() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::new());
        source.route("/fake.zip", Reply::Body(bytes.clone()));
        let mut tool =
            crate::tool_install::tests::test_archive_entry(&source.url("/fake.zip"), &bytes);
        tool.sha256 = Some("f".repeat(64));
        let base = scratch("failed");
        let state = new_state();
        let started = parse(&tools::start_install(
            &state,
            "faketool",
            fake_env(base.join("tools"), tool),
        ));
        assert_eq!(started["ok"], true);
        wait_until("install finished", || {
            state_lock(&state)
                .tool_install
                .as_ref()
                .is_some_and(|job| job.state != "running")
        });
        let status = parse(&tools::api_tools_install_status(&state));
        assert_eq!(status["install"]["state"], "failed");
        assert_eq!(status["install"]["stage"], "failed");
        assert_eq!(status["install"]["cancelled"], false);
        assert!(
            status["install"]["error"]
                .as_str()
                .unwrap()
                .contains("SHA-256 mismatch")
        );
        assert!(!state_lock(&state).busy);
        assert!(!base.join("tools/bin/fake.exe").exists());
        let _ = std::fs::remove_dir_all(base);
    }
}
