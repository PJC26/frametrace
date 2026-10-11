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

/// Reads the mutation nonce from the `ft_nonce` cookie set on a page GET.
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
    let at = text
        .find("ft_nonce=")
        .map(|at| at + "ft_nonce=".len())
        .expect("GET / plants the ft_nonce cookie");
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
