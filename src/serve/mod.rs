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
mod recent;
mod records;
mod state;
mod system;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_hardening;
#[cfg(test)]
mod tests_http;
#[cfg(test)]
mod tests_jobs;
#[cfg(test)]
mod tests_live;
#[cfg(test)]
mod tests_marks_live;
#[cfg(test)]
mod tests_recent;
#[cfg(test)]
mod tests_review3;

pub(crate) use discovery::{
    find_running_server, publish_local_server, token_for_port, write_instance_file,
};
pub(crate) use http::{
    MAX_CAPTURE_FRAME_BODY, Request, body_value, handle_connection, instance_fingerprint,
    is_viewer_page_path, json, json_string, page, path_is_under, plain, query_value,
    request_is_localhost_trusted, valid_token, viewer_page, write_simple,
};
pub(crate) use jobs::{
    api_carve, api_finalize, api_import_marks, api_recover_deleted, api_start, api_verify_sources,
    case_job_json, default_case_dir, pending_marks_count, run_step, run_step_cancellable,
};
pub(crate) use media::{
    api_advanced, api_capture_frame, api_export_clip, api_export_clip_cancel,
    api_export_clip_status, api_export_selected, api_proxy, api_telemetry, api_transcode_queue,
    cancel_all_clip_jobs, clip_job_json, newest_package_dir, opt_path, running_clip_job_count,
};
pub(crate) use recent::{api_recent_cases, api_recent_cases_forget, record_recent_case};
pub(crate) use records::{
    api_records, api_records_meta, cached_index, index_generation, newest_tsk_inspection,
    scan_progress_path,
};
pub(crate) use state::{
    BusyLease, CaseJob, ClipJob, Enrichment, IndexCache, IndexFingerprint, InputKind, JobState,
    LiveCache, PipelineJob, SharedState, StepStatus, api_err, restore_session, save_session,
    state_lock, try_acquire_busy, try_lease_busy,
};
pub(crate) use system::{
    api_browse, api_cancel, api_case_info, api_env, api_open_case, api_preflight, api_shutdown,
    api_status, api_verify_audit, tool_available,
};

// ---- test-only re-exports (names only the unit tests reach through `super::*`)
#[cfg(test)]
pub(crate) use discovery::probe_status_body;
// The WebView2 shell (windows only, src/shell.rs) reaches these.
#[cfg(any(test, target_os = "windows"))]
pub(crate) use discovery::read_instance_record;
#[cfg(target_os = "windows")]
pub(crate) use discovery::{remove_instance_file_if_owned, resolve_shell_token, shell_target};
#[cfg(test)]
pub(crate) use http::{
    AuthProof, HttpAuthClass, MAX_BODY, authorize, etag_matches, handle_connection_timed,
    http_auth_class, http_body_limit_for, is_generated_page, mime_for, parse_range,
    parse_request_head, percent_decode_component, percent_encode_header_value,
    relative_path_is_safe, request_authorized, secret_eq,
};
#[cfg(test)]
pub(crate) use jobs::{
    STEP_OUTPUT_TAIL_BYTES, build_selection_file, carve_with_exe, drain_capped,
    enter_review_partial, finalize_with_exe, phase1_scan_args, recover_deleted_with_exe,
    run_folder_enrichment, source_is_volume_root, start_media_roots, warn_step,
};
#[cfg(test)]
pub(crate) use media::{copy_unique, export_clip_with_exe, export_safe_name};
// Only the fake-CLI tests (unix-only) use these.
#[cfg(all(test, unix))]
pub(crate) use jobs::SIGNOFF_PENDING_REL_PATH;
#[cfg(all(test, unix))]
pub(crate) use media::exhibit_arg;
#[cfg(test)]
pub(crate) use recent::{
    RECENT_CAP, RecentCase, canonical_case_dir, forget_recent_in, load_recent, recent_case_json,
    recent_cases_file, recent_cases_json_from, record_recent_in, upsert_recent,
};
#[cfg(test)]
pub(crate) use records::{
    FTS_CACHE_CAPACITY, cached_fts_search, cached_live_checkpoint, cached_thumb_stems,
    records_fts_cache_len,
};
#[cfg(test)]
pub(crate) use system::{
    browse_json, byte_progress_json, has_ewf_signature, preflight_json, progress_json, source_kind,
};

const EXAMINER_PAGE: &str = include_str!(concat!(env!("OUT_DIR"), "/examiner_app.html"));
/// `FRAMETRACE_UI=legacy` rollback page (docs/ui-rebuild-contract.md §9.3):
/// the pre-rebuild shell, which embeds the viewer through its iframe.
/// Kept for one release after the UI rebuild, then removed with
/// assets/legacy/.
const EXAMINER_PAGE_LEGACY: &str =
    include_str!(concat!(env!("OUT_DIR"), "/examiner_app_legacy.html"));

/// Whether `FRAMETRACE_UI=legacy` asked for the pre-rebuild shell. Read
/// once per process: the served UI never flips mid-session.
fn legacy_ui() -> bool {
    static LEGACY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LEGACY.get_or_init(|| legacy_ui_requested(std::env::var("FRAMETRACE_UI").ok().as_deref()))
}

fn legacy_ui_requested(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim().eq_ignore_ascii_case("legacy"))
}

/// The page `GET /` serves.
fn examiner_page(legacy: bool) -> &'static str {
    if legacy {
        EXAMINER_PAGE_LEGACY
    } else {
        EXAMINER_PAGE
    }
}

/// In-flight connection bound: the accept loop spawns one thread per
/// connection, so an unbounded accept would let any local process exhaust
/// memory/handles by opening thousands of idle sockets. Overflow
/// connections get an immediate 503 instead of a thread.
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
    proc::install_process_guard();
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
        // Upgrading from a build without the recent list: the restored
        // case is at least the first entry.
        record_recent_case(&case_dir, None);
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
        Some(port) => {
            bind_loopback(port).map_err(|err| format!("failed to bind 127.0.0.1:{port}: {err}"))?
        }
        // The probe listener is passed straight through instead of being
        // dropped and re-bound: between "probe says free" and "bind for
        // real" another process could claim the port (classic TOCTOU).
        None => first_free_port().ok_or("사용 가능한 로컬 포트를 찾지 못했습니다")?,
    };
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    // In-process publish first: the shell must never depend on the file.
    publish_local_server(port, token.as_deref());
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
    // Drop the instance file while it still names this process, so later
    // launches never send a token to whatever reuses the port.
    discovery::remove_instance_file_if_owned();
    // Shutdown path (/api/shutdown ends the accept loop; it is refused while a
    // step runs, so this only reaps stragglers): never leave pipeline process
    // trees (ffmpeg grandchildren) running after the workstation exits. Kept
    // out of serve_on so in-process test servers cannot kill each other's steps.
    proc::kill_all_tracked();
    Ok(())
}

/// Kills every tracked pipeline process tree. The WebView2 shell calls
/// this on its forced-exit path (a server that keeps refusing shutdown
/// after the bounded cancel wait) so no ffmpeg outlives the window.
#[cfg(target_os = "windows")]
pub(crate) fn kill_all_tracked() {
    proc::kill_all_tracked();
}

/// Binds a loopback listener. On Windows the socket is created through
/// WinSock with `SO_EXCLUSIVEADDRUSE` so another local process cannot
/// bind the same port on top of the workstation with `SO_REUSEADDR`
/// (port hijack); elsewhere `std` already refuses that by default.
/// `port == 0` picks a free port.
pub(crate) fn bind_loopback(port: u16) -> std::io::Result<TcpListener> {
    #[cfg(windows)]
    {
        winsock::bind_loopback_exclusive(port)
    }
    #[cfg(not(windows))]
    {
        TcpListener::bind(("127.0.0.1", port))
    }
}

#[cfg(windows)]
mod winsock {
    use std::net::TcpListener;
    use std::os::windows::io::FromRawSocket;
    use windows::Win32::Networking::WinSock::{
        AF_INET, INVALID_SOCKET, IPPROTO_TCP, SO_EXCLUSIVEADDRUSE, SOCK_STREAM, SOCKADDR,
        SOCKADDR_IN, SOCKET, SOL_SOCKET, SOMAXCONN, WSA_FLAG_NO_HANDLE_INHERIT,
        WSA_FLAG_OVERLAPPED, WSADATA, WSAGetLastError, WSASocketW, WSAStartup, bind, closesocket,
        listen, setsockopt,
    };

    fn last_error() -> std::io::Error {
        // SAFETY: plain thread-local error read.
        std::io::Error::from_raw_os_error(unsafe { WSAGetLastError() }.0)
    }

    /// std initializes WinSock lazily on its first socket call; a raw
    /// `socket()` before that fails with WSANOTINITIALISED. WSAStartup is
    /// reference counted and never cleaned up here (process lifetime).
    fn ensure_winsock() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let mut data = WSADATA::default();
            // SAFETY: out-pointer to a live WSADATA.
            let _ = unsafe { WSAStartup(0x0202, &mut data) };
        });
    }

    fn loopback_addr(port: u16) -> SOCKADDR_IN {
        let mut addr = SOCKADDR_IN {
            sin_family: AF_INET,
            sin_port: port.to_be(),
            ..Default::default()
        };
        addr.sin_addr.S_un.S_addr = u32::from_ne_bytes([127, 0, 0, 1]);
        addr
    }

    /// socket -> setsockopt(option=1) -> bind(127.0.0.1:port). Returns the
    /// bound socket; the caller listens on it or closes it.
    ///
    /// The socket is created with `WSA_FLAG_NO_HANDLE_INHERIT` (and
    /// `WSA_FLAG_OVERLAPPED`, matching std's own sockets): a plain
    /// `socket()` handle is inheritable, so every child spawned through
    /// `Command` (Edge with CREATE_BREAKAWAY_FROM_JOB, ffmpeg) would inherit
    /// the LISTEN socket and keep the port alive after the server exits.
    fn bind_with_option(port: u16, option: i32) -> std::io::Result<SOCKET> {
        ensure_winsock();
        // SAFETY: WinSock calls on a socket this function owns until it is
        // either returned or closed on the error paths.
        unsafe {
            let sock = WSASocketW(
                i32::from(AF_INET.0),
                SOCK_STREAM.0,
                IPPROTO_TCP.0,
                None,
                0,
                WSA_FLAG_OVERLAPPED | WSA_FLAG_NO_HANDLE_INHERIT,
            )
            .map_err(|_| last_error())?;
            if sock == INVALID_SOCKET {
                return Err(last_error());
            }
            let one: i32 = 1;
            if setsockopt(sock, SOL_SOCKET, option, Some(&one.to_ne_bytes())) != 0 {
                let err = last_error();
                let _ = closesocket(sock);
                return Err(err);
            }
            let addr = loopback_addr(port);
            if bind(
                sock,
                &addr as *const SOCKADDR_IN as *const SOCKADDR,
                std::mem::size_of::<SOCKADDR_IN>() as i32,
            ) != 0
            {
                let err = last_error();
                let _ = closesocket(sock);
                return Err(err);
            }
            Ok(sock)
        }
    }

    pub(super) fn bind_loopback_exclusive(port: u16) -> std::io::Result<TcpListener> {
        let sock = bind_with_option(port, SO_EXCLUSIVEADDRUSE)?;
        // SAFETY: `sock` is a bound socket we own; ownership moves into the
        // TcpListener on success.
        unsafe {
            if listen(sock, SOMAXCONN as i32) != 0 {
                let err = last_error();
                let _ = closesocket(sock);
                return Err(err);
            }
            Ok(TcpListener::from_raw_socket(sock.0 as _))
        }
    }

    /// Test probe: tries to bind `port` with `SO_REUSEADDR`, the way a
    /// hijacking process would. Closes the socket again either way.
    #[cfg(test)]
    pub(super) fn reuseaddr_bind_succeeds(port: u16) -> bool {
        use windows::Win32::Networking::WinSock::SO_REUSEADDR;
        match bind_with_option(port, SO_REUSEADDR) {
            Ok(sock) => {
                // SAFETY: closing a socket we own.
                let _ = unsafe { closesocket(sock) };
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(all(test, windows))]
mod bind_tests {
    use super::{bind_loopback, winsock};

    /// Without SO_EXCLUSIVEADDRUSE a second socket with SO_REUSEADDR can
    /// bind on top of a listening port and steal new connections; with it
    /// that bind is refused.
    #[test]
    fn reuseaddr_cannot_bind_over_the_workstation_port() {
        let listener = bind_loopback(0).expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        assert!(
            !winsock::reuseaddr_bind_succeeds(port),
            "SO_REUSEADDR bind succeeded over the exclusive listener on port {port}"
        );
        drop(listener);
        assert!(
            winsock::reuseaddr_bind_succeeds(port),
            "port {port} should be free again once the listener is closed"
        );
    }

    /// The listener is a real std listener: it accepts, and after a
    /// connection has been served and everything is closed the same port
    /// can be bound again immediately (restart of the workstation).
    #[test]
    fn exclusive_listener_accepts_and_can_be_rebound_after_restart() {
        use std::io::{Read, Write};
        let listener = bind_loopback(0).expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.write_all(b"ping").unwrap();
            let mut reply = Vec::new();
            stream.read_to_end(&mut reply).unwrap();
            reply
        });
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).unwrap();
        stream.write_all(b"pong").unwrap();
        drop(stream);
        assert_eq!(client.join().unwrap(), b"pong");
        drop(listener);
        let again = bind_loopback(port).expect("rebind same port after restart");
        assert_eq!(again.local_addr().unwrap().port(), port);
    }

    /// The LISTEN socket must not be inheritable: children spawned with
    /// `Command` (Edge, ffmpeg) would otherwise hold the port open after
    /// the workstation exits.
    #[test]
    fn listener_handle_is_not_inheritable() {
        use std::os::windows::io::AsRawSocket;
        use windows::Win32::Foundation::{GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
        let listener = bind_loopback(0).expect("bind loopback");
        let mut flags = 0u32;
        // SAFETY: querying flags of a live socket handle we own.
        unsafe {
            GetHandleInformation(HANDLE(listener.as_raw_socket() as _), &mut flags)
                .expect("GetHandleInformation on listener");
        }
        assert_eq!(
            flags & HANDLE_FLAG_INHERIT.0,
            0,
            "listener socket is inheritable (flags {flags:#x})"
        );
    }
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
                let worker_count = Arc::clone(&in_flight);
                let spawned = thread::Builder::new()
                    .name("ft-http".into())
                    .spawn(move || {
                        let _ = handle_connection(stream, state);
                        worker_count.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                }
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
        if let Ok(listener) = bind_loopback(port) {
            return Some(listener);
        }
    }
    bind_loopback(0).ok()
}

pub(crate) fn should_open_browser(options: &ServeOptions) -> bool {
    options.open_browser && std::env::var("FRAMETRACE_NO_BROWSER").as_deref() != Ok("1")
}

pub(crate) fn open_in_browser(url: &str) {
    // Windows-first: prefer Edge app mode so the workstation opens as a
    // dedicated chromeless window instead of a tab inside the examiner's
    // browsing session. Edge and Explorer are resolved to ABSOLUTE paths
    // (known install dirs, then the App Paths registry key) — a bare
    // `msedge.exe` would let `Command` search the current directory first
    // (planted-binary hijack). Both are launched outside the server's
    // KILL_ON_JOB_CLOSE job so closing the workstation never tears down
    // the examiner's whole browser. Last resort: the shell's own URL
    // handler (ShellExecuteW).
    #[cfg(target_os = "windows")]
    {
        let env = |name: &str| std::env::var_os(name);
        if let Some(edge) = launch::resolve_edge_path(&env, &|p| p.is_file(), &launch::app_path)
            && launch::spawn_detached(&edge, &[std::ffi::OsString::from(format!("--app={url}"))])
        {
            return;
        }
        if let Some(explorer) = launch::resolve_explorer_path(&env)
            && launch::spawn_detached(&explorer, &[std::ffi::OsString::from(url)])
        {
            return;
        }
        if !launch::shell_open(url) {
            eprintln!("frametrace: could not open a browser for {url}; open it manually");
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = crate::util::hide_console(&mut Command::new("xdg-open"))
            .arg(url)
            .spawn();
    }
}

pub(crate) fn open_in_explorer(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let env = |name: &str| std::env::var_os(name);
        if let Some(explorer) = launch::resolve_explorer_path(&env)
            && launch::spawn_detached(&explorer, &[path.as_os_str().to_owned()])
        {
            return;
        }
        if !launch::shell_open(&path.to_string_lossy()) {
            eprintln!("frametrace: could not open {} in Explorer", path.display());
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = crate::util::hide_console(&mut Command::new("xdg-open"))
            .arg(path)
            .spawn();
    }
}

/// Absolute-path resolution for the shell programs the workstation
/// launches. The resolvers are pure (environment and filesystem are
/// injected) so the "never a relative name" contract is unit-tested on
/// every platform; the spawn/registry/ShellExecute parts are Windows-only.
pub(crate) mod launch {
    // Only the unit tests use the pure resolvers off Windows.
    #![cfg_attr(not(windows), allow(dead_code))]

    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    const EDGE_REL: &str = r"Microsoft\Edge\Application\msedge.exe";

    /// Windows-style absoluteness (`X:\...` or `\\server\...`) decided
    /// textually, so the resolvers behave identically in the unix test
    /// runs — `Path::is_absolute` would call `C:\...` relative there.
    fn is_absolute_windows_path(path: &Path) -> bool {
        let text = path.to_string_lossy();
        let bytes = text.as_bytes();
        let drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
        drive || text.starts_with(r"\\")
    }

    /// `<base>\<rel>` built textually (see `is_absolute_windows_path`).
    fn join_windows(base: &std::ffi::OsStr, rel: &str) -> PathBuf {
        let base = base.to_string_lossy();
        PathBuf::from(format!("{}\\{rel}", base.trim_end_matches(['\\', '/'])))
    }

    /// Known Edge install locations, in order: per-machine x86, per-machine
    /// x64, per-user; then the App Paths registry entry. Only an absolute
    /// path that exists is returned — never a bare or relative name.
    pub(crate) fn resolve_edge_path(
        env: &dyn Fn(&str) -> Option<OsString>,
        exists: &dyn Fn(&Path) -> bool,
        app_path: &dyn Fn(&str) -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        let from_env = ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
            .iter()
            .filter_map(|name| env(name))
            .map(|base| join_windows(&base, EDGE_REL))
            .find(|candidate| is_absolute_windows_path(candidate) && exists(candidate));
        from_env.or_else(|| {
            app_path("msedge.exe")
                .filter(|candidate| is_absolute_windows_path(candidate) && exists(candidate))
        })
    }

    /// `%SystemRoot%\explorer.exe` (`%windir%` as the alias); `None` when
    /// neither variable is set rather than a bare `explorer.exe`.
    pub(crate) fn resolve_explorer_path(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
        ["SystemRoot", "windir"]
            .iter()
            .filter_map(|name| env(name))
            .map(|base| join_windows(&base, "explorer.exe"))
            .find(|candidate| is_absolute_windows_path(candidate))
    }

    /// `HKLM\...\App Paths\<exe>` default value.
    #[cfg(target_os = "windows")]
    pub(crate) fn app_path(exe: &str) -> Option<PathBuf> {
        use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
        use windows::core::{HSTRING, PCWSTR};
        let subkey = HSTRING::from(format!(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}"
        ));
        let mut size: u32 = 0;
        // SAFETY: size query, then a read into a buffer of that size; the
        // registry API writes at most `size` bytes.
        unsafe {
            let status = RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR::from_raw(subkey.as_ptr()),
                PCWSTR::null(),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut size),
            );
            if status.is_err() || size < 2 {
                return None;
            }
            let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
            let status = RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR::from_raw(subkey.as_ptr()),
                PCWSTR::null(),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            );
            if status.is_err() {
                return None;
            }
            let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            let text = String::from_utf16_lossy(&buffer[..len]);
            let trimmed = text.trim().trim_matches('"');
            (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
        }
    }

    /// Spawns `program` (absolute path) with the arguments, detached from
    /// the server job (`CREATE_BREAKAWAY_FROM_JOB`, allowed by the server
    /// job's BREAKAWAY_OK) and without inheriting our stdio. If the
    /// process is in an outer job that forbids breakaway the spawn fails
    /// with ERROR_ACCESS_DENIED and the caller falls through to the next
    /// strategy — we never retry inside the job silently.
    #[cfg(target_os = "windows")]
    pub(crate) fn spawn_detached(program: &Path, args: &[OsString]) -> bool {
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        debug_assert!(program.is_absolute());
        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        crate::util::hide_console_with_flags(&mut command, CREATE_BREAKAWAY_FROM_JOB);
        match command.spawn() {
            Ok(_) => true,
            Err(err) => {
                eprintln!("frametrace: launching {} failed: {err}", program.display());
                false
            }
        }
    }

    /// `ShellExecuteW(open, target)`: the shell's own handler for a URL or
    /// folder. Used only when the direct launches failed.
    #[cfg(target_os = "windows")]
    pub(crate) fn shell_open(target: &str) -> bool {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, PCWSTR, w};
        let target = HSTRING::from(target);
        // SAFETY: all pointers are to live null-terminated UTF-16 strings.
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR::from_raw(target.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        // Documented: values above 32 mean success.
        (result.0 as usize) > 32
    }

    #[cfg(test)]
    mod tests {
        use super::{resolve_edge_path, resolve_explorer_path};
        use std::collections::HashMap;
        use std::ffi::OsString;
        use std::path::{Path, PathBuf};

        fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, OsString> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), OsString::from(v)))
                .collect()
        }

        #[test]
        fn edge_resolves_to_the_first_existing_known_location() {
            let env = env_of(&[
                ("ProgramFiles(x86)", r"C:\Program Files (x86)"),
                ("ProgramFiles", r"C:\Program Files"),
                ("LOCALAPPDATA", r"C:\Users\x\AppData\Local"),
            ]);
            let present = Path::new(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe");
            let found =
                resolve_edge_path(&|name| env.get(name).cloned(), &|p| p == present, &|_| None)
                    .unwrap();
            assert_eq!(found, present);
            assert!(found.is_absolute());
        }

        #[test]
        fn edge_falls_back_to_app_paths_and_never_returns_a_bare_name() {
            let env = env_of(&[("ProgramFiles", r"C:\Program Files")]);
            let registry = PathBuf::from(r"D:\Apps\Edge\msedge.exe");
            let found =
                resolve_edge_path(&|name| env.get(name).cloned(), &|p| p == registry, &|exe| {
                    assert_eq!(exe, "msedge.exe");
                    Some(registry.clone())
                })
                .unwrap();
            assert_eq!(found, registry);
            // A relative App Paths value (or a planted bare name) is refused
            // even when a file of that name exists in the cwd.
            let relative = resolve_edge_path(
                &|name| env.get(name).cloned(),
                &|p| p == Path::new("msedge.exe"),
                &|_| Some(PathBuf::from("msedge.exe")),
            );
            assert_eq!(relative, None);
        }

        #[test]
        fn edge_is_none_when_nothing_exists() {
            let env = env_of(&[("ProgramFiles", r"C:\Program Files")]);
            assert_eq!(
                resolve_edge_path(&|name| env.get(name).cloned(), &|_| false, &|_| None),
                None
            );
            // No environment at all: nothing is guessed from the cwd/PATH.
            assert_eq!(resolve_edge_path(&|_| None, &|_| true, &|_| None), None);
        }

        #[test]
        fn explorer_is_system_root_absolute_or_none() {
            let env = env_of(&[("SystemRoot", r"C:\WINDOWS")]);
            assert_eq!(
                resolve_explorer_path(&|name| env.get(name).cloned()),
                Some(PathBuf::from(r"C:\WINDOWS\explorer.exe"))
            );
            let alias = env_of(&[("windir", r"C:\WINDOWS")]);
            assert_eq!(
                resolve_explorer_path(&|name| alias.get(name).cloned()),
                Some(PathBuf::from(r"C:\WINDOWS\explorer.exe"))
            );
            assert_eq!(resolve_explorer_path(&|_| None), None);
            let relative = env_of(&[("SystemRoot", "WINDOWS")]);
            assert_eq!(
                resolve_explorer_path(&|name| relative.get(name).cloned()),
                None
            );
        }

        /// On the real machine the resolvers must produce absolute,
        /// existing programs (Explorer always; Edge when installed).
        #[cfg(windows)]
        #[test]
        fn real_environment_yields_absolute_existing_paths() {
            let env = |name: &str| std::env::var_os(name);
            let explorer = resolve_explorer_path(&env).expect("SystemRoot is set on Windows");
            assert!(explorer.is_absolute() && explorer.is_file(), "{explorer:?}");
            if let Some(edge) = resolve_edge_path(&env, &|p| p.is_file(), &super::app_path) {
                assert!(edge.is_absolute() && edge.is_file(), "{edge:?}");
            }
        }
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
        ("GET", "/") => page(examiner_page(legacy_ui()).as_bytes().to_vec()),
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
        ("GET", "/api/case-info") => json(api_case_info(request, state)),
        ("POST", "/api/preflight") => json(api_preflight(request)),
        ("GET", "/api/export-clip-status") => json(api_export_clip_status(request, state)),
        ("POST", "/api/export-clip-cancel") => json(api_export_clip_cancel(request, state)),
        ("GET", "/api/recent-cases") => json(api_recent_cases()),
        ("POST", "/api/recent-cases/forget") => json(api_recent_cases_forget(request)),
        _ => plain(404, b"not found".to_vec()),
    }
}
