//! Workstation end-to-end integration test: boots the real
//! `frametrace-app` server binary, drives the full 5-step examiner
//! pipeline (case init -> source registration -> scan/index ->
//! regeneration validation -> review generation) over raw HTTP against
//! a real ffmpeg-made fixture, and asserts the final package artifacts.
//!
//! Gated behind `FRAMETRACE_IT=1` like the tool IT layer:
//!
//! ```text
//! FRAMETRACE_IT=1 cargo test --test workstation_e2e -- --ignored
//! ```

mod common;
use common::IsolatedCommand;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

fn unique_dir(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{name}_{}_{}", std::process::id(), stamp))
}

/// Minimal HTTP/1.1 client for the workstation API. The server always
/// closes connections after one response (write half-close), so request
/// and response live on one connection.
fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    http_with(port, method, path, body, None)
}

/// Mutating requests need the instance nonce under the no-token gate
/// (see `fetch_nonce`).
fn http_with(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
    nonce: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect workstation");
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    let body = body.unwrap_or("");
    let nonce_header = nonce
        .map(|nonce| format!("X-FrameTrace-Nonce: {nonce}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{nonce_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buffer).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let body_at = text.find("\r\n\r\n").map(|at| at + 4).unwrap_or(0);
    (status, text[body_at..].to_string())
}

/// Reads the mutation nonce from the `ft_nonce_<port>` cookie (C2: named
/// per port) set on a page GET.
fn fetch_nonce(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect workstation");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    let text = String::from_utf8_lossy(&buffer);
    let name = format!("ft_nonce_{port}=");
    let at = text
        .find(&name)
        .map(|at| at + name.len())
        .expect("GET / plants the per-port ft_nonce cookie");
    text[at..]
        .split(|ch: char| ch == ';' || ch.is_whitespace())
        .next()
        .unwrap()
        .to_string()
}

/// Polls `/api/status` until `phase == {want}` or the deadline passes.
fn wait_phase(port: u16, want: &str, deadline: Duration) -> (bool, String) {
    let started = Instant::now();
    loop {
        let (code, body) = http(port, "GET", "/api/status", None);
        if code == 200 && body.contains(&format!("\"phase\":\"{want}\"")) {
            return (true, body);
        }
        if started.elapsed() > deadline {
            return (false, body);
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[test]
#[ignore = "requires FRAMETRACE_IT=1, ffmpeg, and a free loopback port"]
fn workstation_full_pipeline_over_http() {
    if !integration_enabled() {
        eprintln!("FRAMETRACE_IT=1 not set; skipping");
        return;
    }
    assert!(
        find_tool("ffmpeg").is_some(),
        "ffmpeg must be on PATH for the workstation E2E"
    );

    let work = unique_dir("ft_ws_e2e");
    std::fs::create_dir_all(work.join("source")).unwrap();
    let case_dir = work.join("case");

    // Fixture: one healthy clip the pipeline can validate.
    let ffmpeg = find_tool("ffmpeg").unwrap();
    let clip = work.join("source/FRONT_20260908_010000.mp4");
    let make = Command::new(&ffmpeg)
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=10:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&clip)
        .output()
        .expect("run ffmpeg");
    assert!(make.status.success(), "fixture creation failed");

    // Boot the workstation on a dedicated port; never open a browser here.
    let port: u16 = 18577;
    let app = env!("CARGO_BIN_EXE_frametrace-app");
    let mut child: Child = Command::new(app)
        .arg(port.to_string())
        .isolated()
        .env("FRAMETRACE_NO_BROWSER", "1")
        // Never read or overwrite the user's ~/.frametrace/server.json.
        .env("FRAMETRACE_INSTANCE_FILE", work.join("server.json"))
        // ...nor the user's workstation-session.json / audit key config dir.
        .env("XDG_CONFIG_HOME", work.join("config"))
        .env("APPDATA", work.join("config"))
        // SEC-04: the e2e exercises the nonce gate, so opt out of the default token.
        .env("FRAMETRACE_NO_TOKEN", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn frametrace-app");
    let cleanup = |child: &mut Child| {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&work);
    };

    // Wait for the server to accept connections.
    let started = Instant::now();
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        if started.elapsed() > Duration::from_secs(10) {
            cleanup(&mut child);
            panic!("workstation did not start listening");
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Sanity: idle status and the environment report.
    let (code, body) = http(port, "GET", "/api/status", None);
    if code != 200 || !body.contains("\"phase\":\"idle\"") {
        cleanup(&mut child);
        panic!("idle status failed: {code} {body}");
    }

    // /api/status only publishes a fingerprint; the real nonce is planted
    // as the ft_nonce cookie on GET responses.
    let nonce = fetch_nonce(port);
    // A nonce-less mutating request must be refused outright.
    let (code, _) = http(port, "POST", "/api/start", Some("{}"));
    assert_eq!(code, 401, "POST without the instance nonce must be 401");

    // Start the folder pipeline with hashing and ffprobe.
    // serde_json quoting keeps Windows paths valid: C:\...\source contains
    // backslashes that would corrupt raw-{} interpolation as JSON escapes.
    let start_body = format!(
        "{{\"input_kind\":\"folder\",\"source_path\":{},\"case_dir\":{},\"with_hash\":\"true\",\"with_ffprobe\":\"true\"}}",
        serde_json::to_string(&work.join("source").display().to_string()).unwrap(),
        serde_json::to_string(&case_dir.display().to_string()).unwrap()
    );
    let (code, body) = http_with(port, "POST", "/api/start", Some(&start_body), Some(&nonce));
    if code != 200 || !body.contains("\"ok\":true") {
        cleanup(&mut child);
        panic!("api/start failed: {code} {body}");
    }

    // The pipeline runs the real CLI chain; allow generous time.
    let (ok, final_body) = wait_phase(port, "review-ready", Duration::from_secs(180));
    if !ok {
        cleanup(&mut child);
        panic!("pipeline did not reach review-ready: {final_body}");
    }

    // Finalize -> make-report + package-case.
    let (code, body) = http_with(
        port,
        "POST",
        "/api/finalize",
        Some(r#"{"examiner":"E2E Examiner"}"#),
        Some(&nonce),
    );
    if code != 200 || !body.contains("\"ok\":true") {
        cleanup(&mut child);
        panic!("api/finalize failed: {code} {body}");
    }
    let (ok, done_body) = wait_phase(port, "done", Duration::from_secs(120));
    if !ok {
        cleanup(&mut child);
        panic!("pipeline did not finish: {done_body}");
    }

    // Assertions on real artifacts the examiner would open.
    let review = case_dir.join("review/evidence-viewer.html");
    assert!(review.is_file(), "viewer missing: {}", review.display());
    let report = case_dir.join("reports/case-report.html");
    assert!(report.is_file(), "report missing: {}", report.display());
    assert!(
        done_body.contains("\"package_dir\""),
        "status must expose the package dir: {done_body}"
    );
    let jsonl = case_dir.join("db/videos.jsonl");
    let indexed = std::fs::read_to_string(&jsonl).unwrap_or_default();
    assert!(
        indexed.contains("FRONT_20260908_010000.mp4"),
        "fixture must be indexed"
    );
    // Validation audit trail must confirm the video stream.
    let validation = std::fs::read_to_string(case_dir.join("evidence/logs/validation-log.jsonl"))
        .unwrap_or_default();
    assert!(
        validation.contains("ffprobe-video-stream-confirmed"),
        "validation log must confirm the stream"
    );

    cleanup(&mut child);
}

/// One raw request with arbitrary extra headers; returns the full response
/// (head + body).
fn raw_request(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect workstation");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).to_string()
}

fn status_of(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

/// Windows review items 7, 8 and 11 against the real binary with the
/// default token model: the `?token=` page load plants the cookie and sends
/// `Referrer-Policy: no-referrer`; cookie-authenticated POSTs from the same
/// origin work, from another localhost port are 403; UNC/device paths are
/// 400 before any filesystem access. (The browser-side `?token=` removal via
/// `history.replaceState` is covered by the Playwright harnesses.)
#[test]
#[ignore = "requires FRAMETRACE_IT=1 and a free loopback port"]
fn workstation_csrf_and_unc_hardening_over_http() {
    if !integration_enabled() {
        eprintln!("FRAMETRACE_IT=1 not set; skipping");
        return;
    }
    let work = unique_dir("ft_ws_harden");
    std::fs::create_dir_all(&work).unwrap();
    let port: u16 = 18578;
    let token = "e2e-token-0123456789";
    let app = env!("CARGO_BIN_EXE_frametrace-app");
    let mut child: Child = Command::new(app)
        .arg(port.to_string())
        .isolated()
        .env("FRAMETRACE_NO_BROWSER", "1")
        .env("FRAMETRACE_INSTANCE_FILE", work.join("server.json"))
        .env("XDG_CONFIG_HOME", work.join("config"))
        .env("APPDATA", work.join("config"))
        .env("FRAMETRACE_TOKEN", token)
        .env_remove("FRAMETRACE_NO_TOKEN")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn frametrace-app");
    let started = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("workstation did not start listening");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let result = std::panic::catch_unwind(|| {
        let host = format!("127.0.0.1:{port}");
        // ?token= page load: cookie planted, referrer suppressed.
        let page = raw_request(
            port,
            &format!("GET /?token={token} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert_eq!(status_of(&page), 200, "{}", &page[..page.len().min(400)]);
        let head = &page[..page.find("\r\n\r\n").unwrap()];
        assert!(
            head.contains(&format!("Set-Cookie: ft_token_{port}={token}")),
            "{head}"
        );
        assert!(head.contains("Referrer-Policy: no-referrer"), "{head}");
        let cookie = format!("Cookie: ft_token_{port}={token}\r\n");
        let post_cancel = |origin: &str| {
            raw_request(
                port,
                &format!(
                    "POST /api/cancel HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\n{cookie}Content-Length: 0\r\n\r\n"
                ),
            )
        };
        // Same origin with only the cookie (what the page does after the
        // token left the URL): accepted.
        let same = post_cancel(&format!("http://127.0.0.1:{port}"));
        assert_eq!(status_of(&same), 200, "{same}");
        // A page on another local port carrying the same cookie: refused.
        let foreign = post_cancel("http://127.0.0.1:9");
        assert_eq!(status_of(&foreign), 403, "{foreign}");
        // UNC / device paths: 400 before any filesystem call.
        for target in [
            r"/media?path=\\evil\share\a.mp4",
            "/media?path=%5C%5Cevil%5Cshare%5Ca.mp4",
            // `//x` is UNC only on Windows (POSIX treats it as a local path).
            "/api/browse?path=%5C%5Cevil%5Cshare",
            r"/api/browse?path=\\?\UNC\evil\share",
            r"/api/case-info?case_dir=\\evil\share\case",
        ] {
            let response = raw_request(
                port,
                &format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n{cookie}\r\n"),
            );
            assert_eq!(status_of(&response), 400, "{target}: {response}");
        }
        let body = r#"{"case_dir":"\\\\evil\\share\\case"}"#;
        let open_case = raw_request(
            port,
            &format!(
                "POST /api/open-case HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        );
        assert_eq!(status_of(&open_case), 400, "{open_case}");
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&work);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// Boots `frametrace-app` on `port` with an isolated config under `work`
/// (no-token nonce gate); returns the child once it accepts connections.
fn boot_isolated(port: u16, work: &std::path::Path) -> Child {
    let app = env!("CARGO_BIN_EXE_frametrace-app");
    let mut child: Child = Command::new(app)
        .arg(port.to_string())
        .isolated()
        .env("FRAMETRACE_NO_BROWSER", "1")
        .env("FRAMETRACE_INSTANCE_FILE", work.join("server.json"))
        .env("XDG_CONFIG_HOME", work.join("config"))
        .env("APPDATA", work.join("config"))
        .env("FRAMETRACE_NO_TOKEN", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn frametrace-app");
    let started = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("workstation did not start listening");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    child
}

/// lavfi fixture: `video` spec, optional `audio` spec, extra encoder args.
fn make_fixture(path: &std::path::Path, video: &str, audio: Option<&str>, extra: &[&str]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut command = Command::new(find_tool("ffmpeg").expect("ffmpeg on PATH"));
    command.args(["-y", "-v", "error", "-f", "lavfi", "-i", video]);
    if let Some(audio) = audio {
        command.args(["-f", "lavfi", "-i", audio, "-shortest", "-c:a", "aac"]);
    }
    command.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    command.args(extra);
    command.arg(path);
    let output = command.output().expect("run ffmpeg");
    assert!(
        output.status.success(),
        "fixture creation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn start_folder(
    port: u16,
    nonce: &str,
    source: &std::path::Path,
    case_dir: &std::path::Path,
    extra: &str,
) {
    let body = format!(
        "{{\"input_kind\":\"folder\",\"source_path\":{},\"case_dir\":{}{extra}}}",
        serde_json::to_string(&source.display().to_string()).unwrap(),
        serde_json::to_string(&case_dir.display().to_string()).unwrap()
    );
    let (code, body) = http_with(port, "POST", "/api/start", Some(&body), Some(nonce));
    assert!(
        code == 200 && body.contains("\"ok\":true"),
        "api/start: {code} {body}"
    );
}

/// file name → indexed id.
fn ids_by_name(case_dir: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(case_dir.join("db/videos.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|row| {
            let path = row.get("relative_path")?.as_str()?.replace('\\', "/");
            let name = path.rsplit('/').next()?.to_string();
            Some((name, row.get("id")?.as_str()?.to_string()))
        })
        .collect()
}

/// Plan §3 (기존 e2e 확장): a run started with `with_audio=true` measures
/// sound in phase 2 — the chained `audio-level-log.jsonl` lane exists and
/// verifies, and `/api/records-meta` carries the bound `audioLevels` map.
#[test]
#[ignore = "requires FRAMETRACE_IT=1, ffmpeg, and a free loopback port"]
fn workstation_with_audio_measures_sound_over_http() {
    if !integration_enabled() {
        eprintln!("FRAMETRACE_IT=1 not set; skipping");
        return;
    }
    assert!(find_tool("ffmpeg").is_some(), "ffmpeg must be on PATH");
    let work = unique_dir("ft_ws_audio");
    let source = work.join("source");
    let video = "testsrc2=size=320x240:rate=10:duration=2";
    make_fixture(
        &source.join("20260908_010000_F.mp4"),
        video,
        Some("sine=frequency=440:sample_rate=48000:duration=2"),
        &[],
    );
    make_fixture(
        &source.join("20260908_010100_F.mp4"),
        video,
        Some("anullsrc=channel_layout=mono:sample_rate=48000"),
        &["-t", "2"],
    );
    let case_dir = work.join("case");
    let port: u16 = 18579;
    let mut child = boot_isolated(port, &work);
    let result = std::panic::catch_unwind(|| {
        let nonce = fetch_nonce(port);
        start_folder(
            port,
            &nonce,
            &source,
            &case_dir,
            ",\"with_hash\":\"true\",\"with_ffprobe\":\"true\",\"with_audio\":\"true\"",
        );
        let (ok, body) = wait_phase(port, "review-ready", Duration::from_secs(180));
        assert!(ok, "pipeline did not reach review-ready: {body}");
        assert!(
            !body.contains("소리 측정 실패"),
            "the sound step must not fail: {body}"
        );
        // The chained lane exists and verifies.
        let lane = case_dir.join("evidence/logs/audio-level-log.jsonl");
        let text = std::fs::read_to_string(&lane).expect("audio-level-log.jsonl lane");
        assert!(text.contains("\"event\":\"audio-level\""), "{text}");
        let (code, verify) = http_with(port, "POST", "/api/verify-audit", Some("{}"), Some(&nonce));
        assert_eq!(code, 200, "{verify}");
        let verify: serde_json::Value = serde_json::from_str(&verify).unwrap();
        let entry = verify["logs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|log| log["name"] == "audio-level-log.jsonl")
            .unwrap_or_else(|| panic!("verify-audit must cover the lane: {verify}"));
        assert_ne!(entry["integrity"], "failed", "{verify}");
        assert!(entry["entries"].as_u64().unwrap() >= 1, "{verify}");
        // records-meta: the bound map, per id.
        let ids = ids_by_name(&case_dir);
        let (code, meta) = http(port, "GET", "/api/records-meta", None);
        assert_eq!(code, 200);
        let meta: serde_json::Value = serde_json::from_str(&meta).unwrap();
        let levels = &meta["audioLevels"];
        let sound = &levels[&ids["20260908_010000_F.mp4"]];
        assert_eq!(sound["r"], "sound", "{levels}");
        assert!(sound["max"].as_f64().unwrap() > -70.0, "{levels}");
        assert!(sound["sec"].as_f64().unwrap() > 1.0, "{levels}");
        assert_eq!(
            levels[&ids["20260908_010100_F.mp4"]]["r"], "silent",
            "{levels}"
        );
        assert_eq!(meta["joinLog"], serde_json::json!([]), "{meta}");
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&work);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// Plan §3 (취소): a join over large segments, cancelled while ffmpeg
/// copies, leaves no output, no `.part`/temp file and no log line.
#[test]
#[ignore = "requires FRAMETRACE_IT=1, ffmpeg/ffprobe, and a free loopback port"]
fn workstation_join_cancel_leaves_nothing_behind() {
    if !integration_enabled() {
        eprintln!("FRAMETRACE_IT=1 not set; skipping");
        return;
    }
    assert!(find_tool("ffmpeg").is_some() && find_tool("ffprobe").is_some());
    let work = unique_dir("ft_ws_join_cancel");
    let source = work.join("source").join("FRONT");
    // ~3 × 150 MB: large enough that the copy outlives a 50 ms poll.
    let names = [
        "20260910_120000_F.mp4",
        "20260910_120030_F.mp4",
        "20260910_120100_F.mp4",
    ];
    for name in names {
        make_fixture(
            &source.join(name),
            "testsrc2=size=1280x720:rate=30:duration=30",
            None,
            &[
                "-preset",
                "ultrafast",
                "-b:v",
                "40M",
                "-maxrate",
                "40M",
                "-bufsize",
                "40M",
            ],
        );
    }
    let case_dir = work.join("case");
    let port: u16 = 18580;
    let mut child = boot_isolated(port, &work);
    let result = std::panic::catch_unwind(|| {
        let nonce = fetch_nonce(port);
        start_folder(port, &nonce, &work.join("source"), &case_dir, "");
        let (ok, body) = wait_phase(port, "review-ready", Duration::from_secs(180));
        assert!(ok, "pipeline did not reach review-ready: {body}");
        let by_name = ids_by_name(&case_dir);
        let ids: Vec<&str> = names.iter().map(|n| by_name[*n].as_str()).collect();
        let check_body = serde_json::json!({"ids": ids, "tolerance": 1.0}).to_string();
        let (code, checked) = http_with(
            port,
            "POST",
            "/api/join-check",
            Some(&check_body),
            Some(&nonce),
        );
        assert_eq!(code, 200, "{checked}");
        let checked: serde_json::Value = serde_json::from_str(&checked).unwrap();
        assert_eq!(checked["ok"], true, "{checked}");
        assert_eq!(
            checked["plan"]["blocking"],
            serde_json::json!([]),
            "{checked}"
        );
        let plan_id = checked["plan"]["plan_id"].as_str().unwrap();
        let join_body = serde_json::json!({"ids": ids, "plan_id": plan_id, "tolerance": 1.0});
        let (_, started) = http_with(
            port,
            "POST",
            "/api/join",
            Some(&join_body.to_string()),
            Some(&nonce),
        );
        let started: serde_json::Value = serde_json::from_str(&started).unwrap();
        assert_eq!(started["job"]["kind"], "join", "{started}");
        let job = started["job"]["id"].as_str().unwrap().to_string();
        let status_of_job = || -> serde_json::Value {
            let (_, body) = http(
                port,
                "GET",
                &format!("/api/export-clip-status?job={job}"),
                None,
            );
            serde_json::from_str(&body).unwrap()
        };
        // Cancel as soon as ffmpeg is copying (or at the latest stage seen).
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let now = status_of_job();
            assert_eq!(
                now["job"]["state"], "running",
                "join ended before cancel: {now}"
            );
            if now["job"]["stage"] == "concat" || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let cancel = serde_json::json!({ "job": job }).to_string();
        let (code, body) = http_with(
            port,
            "POST",
            "/api/export-clip-cancel",
            Some(&cancel),
            Some(&nonce),
        );
        assert!(code == 200 && body.contains("\"ok\":true"), "{code} {body}");
        let deadline = Instant::now() + Duration::from_secs(60);
        let end = loop {
            let now = status_of_job();
            if now["job"]["state"] != "running" {
                break now;
            }
            assert!(Instant::now() < deadline, "cancel did not finish: {now}");
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(end["job"]["state"], "cancelled", "{end}");
        let joined = case_dir.join("artifacts/joined");
        let left: Vec<String> = std::fs::read_dir(&joined)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            left.iter()
                .all(|name| !name.starts_with(".join-") && !name.starts_with("join_")),
            "leftovers: {left:?}"
        );
        let log = std::fs::read_to_string(joined.join("join-log.jsonl")).unwrap_or_default();
        assert!(
            log.trim().is_empty(),
            "a cancelled join must not log: {log}"
        );

        // The same plan, left to finish: the server re-verifies the real
        // CLI's output against its log entry and manifest.
        let (_, again) = http_with(
            port,
            "POST",
            "/api/join",
            Some(&join_body.to_string()),
            Some(&nonce),
        );
        let again: serde_json::Value = serde_json::from_str(&again).unwrap();
        let job2 = again["job"]["id"].as_str().unwrap().to_string();
        assert_ne!(job2, job, "a cancelled job is never reused: {again}");
        let deadline = Instant::now() + Duration::from_secs(120);
        let done = loop {
            let (_, body) = http(
                port,
                "GET",
                &format!("/api/export-clip-status?job={job2}"),
                None,
            );
            let now: serde_json::Value = serde_json::from_str(&body).unwrap();
            if now["job"]["state"] != "running" {
                break now;
            }
            assert!(Instant::now() < deadline, "join did not finish: {now}");
            std::thread::sleep(Duration::from_millis(200));
        };
        assert_eq!(done["job"]["state"], "done", "{done}");
        let rel = done["path"].as_str().unwrap();
        assert!(rel.starts_with("artifacts/joined/join_F_"), "{done}");
        assert!(
            done["manifest_path"]
                .as_str()
                .unwrap()
                .ends_with(".manifest.json")
        );
        assert_eq!(done["job"]["duration_check"]["ok"], true, "{done}");
        let log = std::fs::read_to_string(joined.join("join-log.jsonl")).unwrap();
        let entry: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(entry["output_sha256"], done["sha256"], "{done}");
        let (_, meta) = http(port, "GET", "/api/records-meta", None);
        let meta: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(meta["joinLog"].as_array().map(Vec::len), Some(1), "{meta}");
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&work);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
