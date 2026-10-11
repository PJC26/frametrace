//! Local examiner workstation launcher.
//!
//! Running the bare `frametrace.exe` binary starts a 127.0.0.1-only HTTP
//! server that hosts the INPUT wizard, drives the existing CLI pipeline as
//! subprocesses (so audit logging and job tracking stay identical), serves the
//! generated review/report pages, and streams evidence media with Range
//! support so the browser can play it without file:// restrictions.

pub(crate) use crate::audit;
pub(crate) use crate::util::json_escape;
pub(crate) use std::io::{Read, Seek, SeekFrom, Write};
pub(crate) use std::net::{TcpListener, TcpStream};
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::{Command, Stdio};
pub(crate) use std::sync::atomic::{AtomicUsize, Ordering};
pub(crate) use std::sync::{Arc, Mutex, MutexGuard};
pub(crate) use std::thread;
pub(crate) use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod discovery;
mod http;
mod jobs;
mod media;
mod proc;
mod records;
mod state;
mod system;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_hardening;
#[cfg(test)]
mod tests_live;
#[cfg(test)]
mod tests_marks_live;

pub(crate) use discovery::*;
pub(crate) use http::*;
pub(crate) use jobs::*;
pub(crate) use media::*;
pub(crate) use records::*;
pub(crate) use state::*;
pub(crate) use system::*;

/// In-flight connection bound: the accept loop spawns one thread per
/// connection, so an unbounded accept would let any local process exhaust
/// memory/handles by opening thousands of idle sockets. Overflow
/// connections get an immediate 503 instead of a thread.
const EXAMINER_PAGE: &str = include_str!("../../assets/examiner_app.html");

pub(crate) const MAX_CONNECTIONS: usize = 64;

pub struct ServeOptions {
    pub case_dir: Option<PathBuf>,
    pub port: Option<u16>,
    /// Whether to open the workstation URL in a browser once bound. The
    /// WebView2 shell passes `false` — it hosts the UI itself; the
    /// `FRAMETRACE_NO_BROWSER=1` env override still wins for tests.
    pub open_browser: bool,
}

pub fn run(options: ServeOptions) -> Result<(), String> {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    if let Some(case_dir) = &options.case_dir {
        let mut guard = state_lock(&state);
        guard.case_dir = Some(case_dir.clone());
        // An explicit --case binding still honors extra media roots the
        // session recorded for that case (linked carving workspaces).
        guard.media_roots = restore_session()
            .and_then(|(dir, roots)| (dir == *case_dir).then_some(roots))
            .unwrap_or_else(|| vec![case_dir.clone()]);
    } else if let Some((case_dir, roots)) = restore_session() {
        let mut guard = state_lock(&state);
        guard.case_dir = Some(case_dir.clone());
        guard.media_roots = roots;
        if case_dir.join("review/index.html").is_file() {
            guard.phase = "review-ready";
            guard.steps = [StepStatus::Done; 5];
            guard.restored = true;
            guard.logs.push(format!(
                "이전 세션의 케이스를 복원했습니다: {}",
                case_dir.display()
            ));
        }
    }
    // SEC-04: shared-secret auth is ON by default on top of the loopback
    // trust model - any local process (or a browser tab on another origin
    // that guessed the port) can otherwise read evidence over GET.
    // FRAMETRACE_TOKEN pins a specific token; FRAMETRACE_NO_TOKEN=1 opts
    // out (E2E harnesses). The token must be cookie-safe because a
    // successful ?token= request plants it as the ft_token cookie.
    let token = resolve_token(
        std::env::var("FRAMETRACE_TOKEN").ok().as_deref(),
        std::env::var("FRAMETRACE_NO_TOKEN").ok().as_deref(),
    )?;
    if let Some(token) = &token {
        state_lock(&state).token = Some(token.clone());
    }
    // Relaunching the app (e.g. after closing the browser tab) must reuse
    // the already-running workstation instead of stacking zombie servers on
    // successive ports. Skipped when a port or case is explicitly given —
    // that is the E2E harness / CLI asking for a fresh dedicated instance.
    if options.port.is_none()
        && options.case_dir.is_none()
        && let Some(existing_port) = find_running_server()
    {
        // The running instance's token lives in the user-private instance
        // file (or FRAMETRACE_TOKEN pinned it for both processes).
        let url = match token_for_port(existing_port).or_else(|| token.clone()) {
            Some(token) => format!("http://127.0.0.1:{existing_port}/?token={token}"),
            None => format!("http://127.0.0.1:{existing_port}/"),
        };
        println!("FrameTrace workstation is already running — reusing it.");
        println!("  {url}");
        let _ = std::io::stdout().flush();
        if should_open_browser(&options) {
            open_in_browser(&url);
        }
        return Ok(());
    }
    let listener = match options.port {
        Some(port) => TcpListener::bind(("127.0.0.1", port))
            .map_err(|err| format!("failed to bind 127.0.0.1:{port}: {err}"))?,
        // The probe listener is passed straight through instead of being
        // dropped and re-bound: between "probe says free" and "bind for
        // real" another process could claim the port (classic TOCTOU).
        None => first_free_port().ok_or("사용 가능한 로컬 포트를 찾지 못했습니다")?,
    };
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    write_instance_file(port, &state_lock(&state).instance, token.as_deref());
    let url = match &token {
        Some(token) => format!("http://127.0.0.1:{port}/?token={token}"),
        None => format!("http://127.0.0.1:{port}/"),
    };
    println!("FrameTrace examiner workstation is running.");
    println!("  {url}");
    println!("Close this window to stop the workstation.");
    let _ = std::io::stdout().flush();
    // Test harnesses set FRAMETRACE_NO_BROWSER=1 so spawning the server
    // never steals focus with a real browser window; the WebView2 shell
    // suppresses it via `open_browser: false` because it hosts the UI.
    if should_open_browser(&options) {
        open_in_browser(&url);
    }
    serve_on(listener, state);
    // Shutdown path (/api/shutdown ends the accept loop; it is refused while a
    // step runs, so this only reaps stragglers): never leave pipeline process
    // trees (ffmpeg grandchildren) running after the workstation exits. Kept
    // out of serve_on so in-process test servers cannot kill each other's steps.
    proc::kill_all_tracked();
    Ok(())
}

/// Decides the auth token for a server process. Pure so it can be tested
/// without touching the environment. Explicit `FRAMETRACE_TOKEN` wins, then
/// `FRAMETRACE_NO_TOKEN=1` disables, otherwise a random 128-bit token.
pub(crate) fn resolve_token(
    env_token: Option<&str>,
    no_token: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(value) = env_token.map(str::trim).filter(|v| !v.is_empty()) {
        if !valid_token(value) {
            return Err(
                "FRAMETRACE_TOKEN must be 8+ chars of A-Z a-z 0-9 . _ - ~ (cookie-safe)".into(),
            );
        }
        return Ok(Some(value.to_string()));
    }
    if no_token.map(str::trim) == Some("1") {
        return Ok(None);
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|err| format!("could not generate an access token (entropy): {err}"))?;
    Ok(Some(bytes.iter().map(|b| format!("{b:02x}")).collect()))
}

/// Accept loop shared by `run` and the integration tests (no browser side
/// effects here). Nonblocking so the /api/shutdown flag can end the loop
/// without a wake-up connection. The server only exits on an explicit
/// shutdown request — never on idle timeout.
pub(crate) fn serve_on(listener: TcpListener, state: SharedState) {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let _ = listener.set_nonblocking(true);
    loop {
        if state_lock(&state).shutdown_requested {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                // On Windows an accepted socket inherits the listener's
                // nonblocking mode; a nonblocking write then fails with
                // WouldBlock once the kernel send buffer fills, truncating
                // large responses mid-stream. Force blocking explicitly.
                let _ = stream.set_nonblocking(false);
                let state = Arc::clone(&state);
                let in_flight = Arc::clone(&in_flight);
                if in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    let _ = write_simple(&mut stream, 503, b"server busy - too many connections");
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let _ = thread::Builder::new()
                    .name("ft-http".into())
                    .spawn(move || {
                        let _ = handle_connection(stream, state);
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                    });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
    // Drain in-flight handlers so a shutdown response (and any in-progress
    // write) is fully flushed before the process exits.
    for _ in 0..200 {
        if in_flight.load(Ordering::SeqCst) == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn first_free_port() -> Option<TcpListener> {
    for port in 8477..=8486 {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            return Some(listener);
        }
    }
    TcpListener::bind(("127.0.0.1", 0)).ok()
}

pub(crate) fn should_open_browser(options: &ServeOptions) -> bool {
    options.open_browser && std::env::var("FRAMETRACE_NO_BROWSER").as_deref() != Ok("1")
}

pub(crate) fn open_in_browser(url: &str) {
    // Windows-first: prefer Edge app mode so the workstation opens as a
    // dedicated chromeless window instead of a tab inside the examiner's
    // browsing session. Falls back to the default browser when Edge is
    // unavailable (PATH, then the standard install location).
    #[cfg(target_os = "windows")]
    {
        let app_arg = format!("--app={url}");
        let launched = Command::new("msedge.exe").arg(&app_arg).spawn().is_ok()
            || Command::new(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe")
                .arg(&app_arg)
                .spawn()
                .is_ok()
            || Command::new(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe")
                .arg(&app_arg)
                .spawn()
                .is_ok();
        if !launched {
            let _ = Command::new("explorer.exe").arg(url).spawn();
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

pub(crate) fn open_in_explorer(path: &Path) {
    #[cfg(target_os = "windows")]
    let _ = Command::new("explorer.exe").arg(path).spawn();
    #[cfg(not(target_os = "windows"))]
    {
        let _ = Command::new("xdg-open").arg(path).spawn();
    }
}

/// SEC-01: resolves an `/api/open-folder` path to a folder the shell may
/// open. The path must exist, be a directory (never a file — on Windows
/// `explorer.exe <file>` launches it), and sit inside the open case, the
/// last package, or an approved media root (case/source roots).
pub(crate) fn resolve_open_folder(state: &SharedState, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("폴더 경로가 비어 있습니다.".into());
    }
    // Only the canonical (absolute) path reaches the shell, so a value like
    // `-switch` can never be parsed as an explorer/xdg-open option.
    let canonical = Path::new(raw)
        .canonicalize()
        .map_err(|_| format!("폴더가 존재하지 않습니다: {raw}"))?;
    if !canonical.is_dir() {
        return Err("폴더만 열 수 있습니다 (파일 경로 거부).".into());
    }
    let roots: Vec<PathBuf> = {
        let guard = state_lock(state);
        guard
            .case_dir
            .iter()
            .chain(guard.package_dir.iter())
            .chain(guard.media_roots.iter())
            .cloned()
            .collect()
    };
    if roots.iter().any(|root| path_is_under(root, &canonical)) {
        Ok(shell_path(canonical))
    } else {
        Err("케이스 또는 승인된 소스 폴더 밖의 경로는 열 수 없습니다.".into())
    }
}

/// explorer.exe does not understand the `\\?\` verbatim prefix that
/// `canonicalize` produces on Windows; convert back to the plain form.
pub(crate) fn shell_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

pub(crate) fn api_open_folder(request: &Request, state: &SharedState) -> String {
    let raw = body_value(&request.body, "path").unwrap_or_default();
    match resolve_open_folder(state, &raw) {
        Ok(path) => {
            open_in_explorer(&path);
            "{\"ok\":true}".to_string()
        }
        Err(err) => api_err(&err),
    }
}

pub(crate) fn route(request: &Request, state: &SharedState) -> Vec<u8> {
    // GET responses are safe to read from any origin only when the Host
    // header is loopback; a rebinding domain pointing at this port is the
    // documented DNS-rebinding attack against local tools.
    if !request_is_localhost_trusted(request) {
        return plain(403, b"cross-origin request rejected".to_vec());
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => page(EXAMINER_PAGE.as_bytes().to_vec()),
        ("GET", "/api/env") => json(api_env(request)),
        ("GET", "/api/browse") => json(api_browse(request)),
        ("GET", "/api/records") => json(api_records(request, state)),
        ("GET", "/api/records-meta") => json(api_records_meta(state)),
        ("GET", "/api/status") => json(api_status(state)),
        ("POST", "/api/start") => json(api_start(request, state)),
        ("POST", "/api/finalize") => json(api_finalize(request, state)),
        ("POST", "/api/recover-deleted") => json(api_recover_deleted(state)),
        ("POST", "/api/export-selected") => json(api_export_selected(request, state)),
        ("POST", "/api/verify-audit") => json(api_verify_audit(state)),
        ("POST", "/api/cancel") => json(api_cancel(state)),
        ("POST", "/api/shutdown") => json(api_shutdown(state)),
        ("POST", "/api/open-case") => json(api_open_case(request, state)),
        ("POST", "/api/import-marks") => json(api_import_marks(request, state)),
        ("POST", "/api/capture-frame") => json(api_capture_frame(request, state)),
        ("POST", "/api/export-clip") => json(api_export_clip(request, state)),
        ("POST", "/api/telemetry") => json(api_telemetry(request, state)),
        ("POST", "/api/transcode-queue") => json(api_transcode_queue(request, state)),
        ("POST", "/api/proxy") => json(api_proxy(request, state)),
        ("POST", "/api/advanced") => json(api_advanced(request, state)),
        ("POST", "/api/carve") => json(api_carve(request, state)),
        ("POST", "/api/open-folder") => json(api_open_folder(request, state)),
        ("GET", path) if is_viewer_page_path(path) => viewer_page(),
        ("POST", "/api/verify-sources") => json(api_verify_sources(state)),
        _ => plain(404, b"not found".to_vec()),
    }
}
