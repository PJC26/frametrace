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
    assert_eq!(ffmpeg["license"], "LGPL-3.0-or-later");
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
        parse(&tools::api_tools_install_cancel(&state))["code"],
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
    let ok = parse(&tools::api_tools_install_cancel(&state));
    assert_eq!(ok["ok"], true, "{ok}");
    assert!(progress.cancelled());

    // Moving files into place: refused, nothing changes.
    let progress = Arc::new(crate::tool_install::Progress::default());
    progress
        .enter_non_cancellable(crate::tool_install::Stage::Install)
        .unwrap();
    state_lock(&state).tool_install.as_mut().unwrap().progress = Arc::clone(&progress);
    let refused = parse(&tools::api_tools_install_cancel(&state));
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["code"], "not_cancellable", "{refused}");
    assert_eq!(refused["install"]["stage"], "install");
    assert_eq!(refused["install"]["state"], "extracting");
    assert!(!progress.cancelled());
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
        let cancel = parse(&tools::api_tools_install_cancel(&state));
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
            parse(&tools::api_tools_install_cancel(&state))["code"],
            "no_install"
        );
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
