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
pub(crate) use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
pub(crate) use std::sync::{Arc, Mutex, MutexGuard};
pub(crate) use std::thread;
pub(crate) use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod discovery;
mod finalize_journal;
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
mod tests_join;
#[cfg(test)]
mod tests_live;
#[cfg(test)]
mod tests_marks_live;
#[cfg(test)]
mod tests_phase3;
#[cfg(test)]
mod tests_post_review;
#[cfg(test)]
mod tests_r12_server;
#[cfg(test)]
mod tests_r13_server;
#[cfg(test)]
mod tests_r14_server;
#[cfg(test)]
mod tests_r16_server;
#[cfg(test)]
mod tests_r17_server;
#[cfg(test)]
mod tests_r18_server;
#[cfg(test)]
mod tests_r19_server;
#[cfg(test)]
mod tests_r20_server;
#[cfg(test)]
mod tests_r21_server;
#[cfg(test)]
mod tests_r4_integration;
#[cfg(test)]
mod tests_r4_jobs;
#[cfg(test)]
mod tests_r4_server;
#[cfg(test)]
mod tests_r5_server;
#[cfg(test)]
mod tests_r6_server;
#[cfg(test)]
mod tests_r7_server;
#[cfg(test)]
mod tests_r8_server;
#[cfg(test)]
mod tests_r9_server;
#[cfg(test)]
mod tests_recent;
#[cfg(test)]
mod tests_review3;
#[cfg(test)]
mod tests_tools;
mod tools;

pub(crate) use discovery::{
    find_running_server, publish_local_server, token_for_port, write_instance_file,
};
pub(crate) use http::{
    MAX_CAPTURE_FRAME_BODY, Request, body_value, instance_fingerprint, is_viewer_page_path, json,
    json_string, page, path_is_under, plain, query_value, request_is_localhost_trusted,
    valid_token, viewer_page, write_simple,
};
#[cfg(test)]
pub(crate) use jobs::api_mark_conflicts;
pub(crate) use jobs::{
    api_audio_measure, api_carve, api_finalize, api_import_marks, api_marks_pending,
    api_recover_deleted, api_start, api_verify_sources, case_job_json, default_case_dir_checked,
    pending_marks_count, run_step, run_step_cancellable,
};
#[cfg(test)]
pub(crate) use media::newest_package_dir;
pub(crate) use media::{
    api_advanced, api_capture_frame, api_export_clip, api_export_clip_cancel,
    api_export_clip_status, api_export_selected, api_join, api_join_check, api_proxy,
    api_telemetry, api_transcode_queue, cancel_all_clip_jobs, clip_job_json,
    running_clip_job_count,
};
pub(crate) use recent::{api_recent_cases, api_recent_cases_forget, record_recent_case};
pub(crate) use records::{
    api_records, api_records_meta, cached_index, index_generation, newest_tsk_inspection,
    scan_progress_path,
};
#[cfg(test)]
pub(crate) use state::try_lease_busy;
pub(crate) use state::{
    BusyLease, CaseJob, ClipJob, Enrichment, IndexCache, IndexFingerprint, InputKind, JobState,
    JoinJobInfo, LiveCache, PipelineJob, SharedState, StepStatus, api_err, cancel_write_ops,
    lease_busy_locked, restore_session, save_session, shutdown_refusal, side_work_refusal,
    state_lock, try_acquire_busy, try_begin_seal, try_begin_write_op,
};
pub(crate) use system::{
    api_browse, api_cancel, api_case_info, api_env, api_open_case, api_preflight, api_shutdown,
    api_status, api_verify_audit,
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
pub(crate) use jobs::{audio_measure_with_exe, enrichment_plan};
#[cfg(test)]
pub(crate) use media::{
    copy_unique, export_clip_with_exe, export_safe_name, join_check_with_tools, join_with_exe,
};
// The fake-CLI tests use these (unix scripts and Windows `.cmd` fakes).
#[cfg(test)]
pub(crate) use jobs::SIGNOFF_PENDING_REL_PATH;
// Unused by the current Windows test set; kept for the fake-CLI tests.
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use media::exhibit_arg;
#[cfg(test)]
pub(crate) use recent::{
    RECENT_CAP, RecentCase, canonical_case_dir, forget_recent_in, load_recent,
    override_recent_cases_file, recent_case_json, recent_cases_file, recent_cases_json_from,
    record_recent_in, upsert_recent,
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
/// connections get an immediate 503 instead of a thread. R5-S7: the
/// budget is the header-phase pool plus the authenticated pool, each
/// capped on its own (`http::MAX_PREAUTH`, `http::MAX_AUTHED`).
pub(crate) const MAX_CONNECTIONS: usize = http::MAX_PREAUTH + http::MAX_AUTHED;

/// How long the nonblocking accept loop sleeps when no connection is
/// pending (also bounds how fast /api/shutdown ends the loop).
const ACCEPT_POLL: Duration = Duration::from_millis(10);

pub struct ServeOptions {
    pub case_dir: Option<PathBuf>,
    pub port: Option<u16>,
    /// Whether to open the workstation URL in a browser once bound. The
    /// WebView2 shell passes `false` — it hosts the UI itself; the
    /// `FRAMETRACE_NO_BROWSER=1` env override still wins for tests.
    pub open_browser: bool,
}

pub fn run(options: ServeOptions) -> Result<(), String> {
    run_on(options, None)
}

/// [`run`] on a listener the caller already bound (R5: the WebView2 shell
/// binds its port itself and hands the very socket over, so no other
/// process can claim the port between "probe says free" and the bind).
/// The "reuse an already-running workstation" step is skipped — the
/// caller decided to own this server.
pub fn run_with_listener(options: ServeOptions, listener: TcpListener) -> Result<(), String> {
    run_on(options, Some(listener))
}

/// R20-S3: the [`ServeOptions::case_dir`] folder as bound —
/// [`state::bind_path`] under the `/api/start` policy (a typed UNC share
/// admitted; device, reserved and verbatim-reassembled paths refused with
/// an error, never silently). R21-S4: no production entry point sets it
/// (`main.rs`, `app.rs` and `shell.rs` pass `None`; there is no
/// `serve --case` flag) — tests and library callers only.
pub(crate) fn bind_cli_case(typed: &Path) -> Result<PathBuf, String> {
    state::bind_path(typed, http::reject_device_path)
        .map_err(|reason| format!("--case {}: {reason}", typed.display()))
}

fn run_on(options: ServeOptions, bound: Option<TcpListener>) -> Result<(), String> {
    proc::install_process_guard();
    // Leftover `tools/.staging-*` of a killed install (skipped while
    // another process holds the install lock).
    crate::tool_install::clean_stale_staging_at_start();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    if let Some(typed) = &options.case_dir {
        // R20-S3 (P34): `case_dir` is bound like a typed `/api/start` case
        // folder — normalized and checked again on the normalized path —
        // so it compares equal to the same folder bound any other way.
        let case_dir = bind_cli_case(typed)?;
        // An explicit --case binding still honors extra media roots the
        // session recorded for that case (linked carving workspaces).
        // Read before the state lock.
        let roots = restore_session()
            .and_then(|(dir, roots)| (dir == case_dir).then_some(roots))
            .unwrap_or_else(|| vec![case_dir.clone()]);
        let mut guard = state_lock(&state);
        guard.case_dir = Some(case_dir);
        guard.media_roots = roots;
    } else if let Some((case_dir, roots)) = restore_session() {
        // Upgrading from a build without the recent list: the restored
        // case is at least the first entry.
        record_recent_case(&case_dir, None);
        let has_review = case_dir.join("review/index.html").is_file();
        let mut guard = state_lock(&state);
        guard.case_dir = Some(case_dir.clone());
        guard.media_roots = roots;
        if has_review {
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
    // successful ?token= request plants it as the ft_token_<port> cookie.
    let token = resolve_token(
        std::env::var("FRAMETRACE_TOKEN").ok().as_deref(),
        std::env::var("FRAMETRACE_NO_TOKEN").ok().as_deref(),
    )?;
    if let Some(token) = &token {
        state_lock(&state).token = Some(token.clone());
    }
    // R4-B15: the audit key source this workstation runs with is handed to
    // every CLI child, and installed here too for the in-process appends.
    let key_source = workstation_key_source();
    if let Some(spec) = &key_source
        && let Err(err) = crate::audit_key::install_source_override(spec)
        && !err.contains("already installed")
    {
        return Err(err);
    }
    let key_tag = audit_key_tag(crate::audit_key::configured());
    {
        let mut guard = state_lock(&state);
        guard.key_source = key_source;
        guard.audit_key_tag = Some(key_tag);
    }
    // Relaunching the app (e.g. after closing the browser tab) must reuse
    // the already-running workstation instead of stacking zombie servers on
    // successive ports. Skipped when a port or case is explicitly given —
    // that is the E2E harness / CLI asking for a fresh dedicated instance.
    // R4-B1: only a server whose identity the user-private instance file
    // proves is reused — never one that merely answers like a workstation.
    if bound.is_none()
        && options.port.is_none()
        && options.case_dir.is_none()
        && let Some(existing_port) = find_running_server()
    {
        // R4-B2: the running instance mints a one-time launch code; the
        // token (instance file or FRAMETRACE_TOKEN) is only sent to it in
        // a header, never put on a command line or printed.
        let code = token_for_port(existing_port)
            .or_else(|| token.clone())
            .and_then(|token| discovery::request_launch_code(existing_port, &token));
        let url = launch_url(existing_port, code.as_deref());
        println!("FrameTrace workstation is already running — reusing it.");
        println!("  {url}");
        if code.is_some() {
            println!("{LAUNCH_URL_NOTE}");
        }
        let _ = std::io::stdout().flush();
        if should_open_browser(&options)
            && let Err(err) = try_open_in_browser(&url)
        {
            eprintln!("frametrace: {err}");
        }
        return Ok(());
    }
    let listener = match (bound, options.port) {
        (Some(listener), _) => listener,
        (None, Some(port)) => {
            bind_loopback(port).map_err(|err| format!("failed to bind 127.0.0.1:{port}: {err}"))?
        }
        // The probe listener is passed straight through instead of being
        // dropped and re-bound: between "probe says free" and "bind for
        // real" another process could claim the port (classic TOCTOU).
        (None, None) => {
            let (listener, occupied) =
                first_free_port_noting().ok_or("사용 가능한 로컬 포트를 찾지 못했습니다")?;
            // R4-B1: a preferred port held by something we could not
            // identify as our workstation is skipped — say so.
            if let Some(warning) = occupied_ports_warning(&occupied) {
                eprintln!("{warning}");
            }
            listener
        }
    };
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    // In-process publish first: the shell must never depend on the file.
    publish_local_server(port, token.as_deref());
    write_instance_file(port, &state_lock(&state).instance, token.as_deref());
    // R4-B2: printed and opened with a one-time launch code, not the token.
    // R5-S8: only when a browser (or the examiner) is meant to open it —
    // the WebView2 shell (`open_browser: false`) hosts the UI itself and
    // authenticates through the instance file, so a code minted for it
    // would only sit there, valid, for its whole life.
    let code = launch_code_wanted(&options, token.is_some())
        .then(|| mint_launch_code(&state))
        .flatten();
    let url = launch_url(port, code.as_deref());
    println!("FrameTrace examiner workstation is running.");
    println!("  {url}");
    if code.is_some() {
        println!("{LAUNCH_URL_NOTE}");
    }
    println!("Close this window to stop the workstation.");
    let _ = std::io::stdout().flush();
    // Test harnesses set FRAMETRACE_NO_BROWSER=1 so spawning the server
    // never steals focus with a real browser window; the WebView2 shell
    // suppresses it via `open_browser: false` because it hosts the UI.
    if should_open_browser(&options)
        && let Err(err) = try_open_in_browser(&url)
    {
        eprintln!("frametrace: {err}");
    }
    // R12-S2 (P13) / R13-S7 (P17): a finalize the last run left unfinished
    // (crash, kill, power loss) is cleaned up in the background once the
    // listener is bound and the URL handed out — holding `busy`, status
    // "recovering" — so a file another program holds can no longer keep
    // the app from starting. The audit key is installed by now.
    let _recovery = finalize_journal::spawn_bound_case_recovery(&state);
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

/// R13-S6: `frametrace finalize-journal <case> [--dismiss]` (the CLI's
/// view of a case's finalize journal; `--dismiss` moves it aside). R17-S2:
/// `--writer-gone` runs the normal recovery as if the writer were gone.
/// R18-S7: `i_am_sure` confirms that while a live process still matches
/// the writer's pid and start time.
pub fn finalize_journal_command(
    case_dir: &Path,
    dismiss: bool,
    force: bool,
    writer_gone: bool,
    i_am_sure: bool,
) -> Result<String, String> {
    if writer_gone {
        return finalize_journal::cli_writer_gone(case_dir, i_am_sure);
    }
    finalize_journal::cli_command(case_dir, dismiss, force)
}

/// C4: stamps every selection export this process is still writing as
/// "중단됨" (the shell's forced-exit path, after its bounded wait).
#[cfg(any(test, target_os = "windows"))]
pub(crate) fn mark_abandoned_exports() -> usize {
    media::mark_abandoned_exports()
}

/// C8: a step child started by the workstation server watches the server
/// and takes its process group down when the server dies (unix; no-op
/// elsewhere). Called first thing by every binary's `main`.
pub fn watch_parent_from_env() {
    proc::watch_parent_from_env();
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
        SOCKADDR_IN, SOCKET, SOL_SOCKET, WSA_FLAG_NO_HANDLE_INHERIT, WSA_FLAG_OVERLAPPED, WSADATA,
        WSAGetLastError, WSASocketW, WSAStartup, bind, closesocket, listen, setsockopt,
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

    /// Listen backlog: WinSock 2's `SOMAXCONN` (0x7fffffff = "reasonable
    /// system maximum"). NOT the `windows` crate's `SOMAXCONN`, which is the
    /// WinSock 1.1 value 5 from winsock.h: with a backlog of 5, a burst of
    /// connects arriving between two polls of the nonblocking accept loop
    /// (the viewer reloading records + thumbnails while the UI polls
    /// /api/status) overflowed the queue and Windows answered the excess
    /// with RST, i.e. ECONNREFUSED on a live server.
    pub(super) const LISTEN_BACKLOG: i32 = 0x7fff_ffff;

    pub(super) fn bind_loopback_exclusive(port: u16) -> std::io::Result<TcpListener> {
        let sock = bind_with_option(port, SO_EXCLUSIVEADDRUSE)?;
        // SAFETY: `sock` is a bound socket we own; ownership moves into the
        // TcpListener on success.
        unsafe {
            if listen(sock, LISTEN_BACKLOG) != 0 {
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

    /// The listen backlog must absorb a burst of connects that arrive
    /// before the accept loop gets to them. With the `windows` crate's
    /// `SOMAXCONN` (WinSock 1.1 value 5) every connect past the fifth was
    /// answered with RST — ECONNREFUSED against a live server.
    #[test]
    fn listener_backlog_absorbs_a_burst_of_pending_connects() {
        use std::net::TcpStream;
        use std::time::Duration;
        let listener = bind_loopback(0).expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let burst = 48;
        let clients: Vec<TcpStream> = (0..burst)
            .map(|i| {
                TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap_or_else(|err| {
                    panic!("connect {i} refused while nothing was accepted yet: {err}")
                })
            })
            .collect();
        for _ in 0..burst {
            listener.accept().expect("accept a queued connection");
        }
        drop(clients);
    }

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

// ---- R4-B2: one-time launch codes ----

/// How long a launch code stays valid.
pub(crate) const LAUNCH_CODE_TTL: Duration = Duration::from_secs(30);
/// Outstanding codes kept at most (oldest dropped first).
const LAUNCH_CODES_CAP: usize = 16;

/// Printed under a launch URL.
pub(crate) const LAUNCH_URL_NOTE: &str = "  (이 주소는 30초 동안 한 번만 열 수 있습니다 — 다시 열려면 프로그램을 다시 실행하십시오. / one-time link, valid for 30 s)";

/// The URL printed / handed to the browser: `/?launch=<code>` when a code
/// was minted, else the bare page (no-token mode, or the code could not be
/// obtained). Never carries the token.
pub(crate) fn launch_url(port: u16, code: Option<&str>) -> String {
    match code {
        Some(code) => format!("http://127.0.0.1:{port}/?launch={code}"),
        None => format!("http://127.0.0.1:{port}/"),
    }
}

/// R5-S8 (pure): whether startup mints a launch code — only with a token
/// and when the URL is for a browser (not the shell, which never uses it).
pub(crate) fn launch_code_wanted(options: &ServeOptions, has_token: bool) -> bool {
    has_token && options.open_browser
}

/// Mints a one-time launch code (only meaningful while a token is
/// configured; `None` otherwise or when entropy fails).
pub(crate) fn mint_launch_code(state: &SharedState) -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    let code: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let now = std::time::Instant::now();
    let mut guard = state_lock(state);
    guard.token.as_ref()?;
    guard.launch_codes.retain(|(_, expires)| *expires > now);
    if guard.launch_codes.len() >= LAUNCH_CODES_CAP {
        guard.launch_codes.remove(0);
    }
    guard
        .launch_codes
        .push((code.clone(), now + LAUNCH_CODE_TTL));
    Some(code)
}

/// Consumes `code` if it is outstanding and unexpired at `now` (single
/// use: a consumed or expired code never validates again).
pub(crate) fn consume_launch_code(
    guard: &mut JobState,
    code: &str,
    now: std::time::Instant,
) -> bool {
    guard.launch_codes.retain(|(_, expires)| *expires > now);
    match guard
        .launch_codes
        .iter()
        .position(|(known, _)| http::secret_eq(code, known))
    {
        Some(position) => {
            guard.launch_codes.remove(position);
            true
        }
        None => false,
    }
}

/// `POST /api/launch-code` (token-authenticated): a fresh one-time code,
/// for a relaunch that reuses this running workstation.
pub(crate) fn api_launch_code(state: &SharedState) -> String {
    match mint_launch_code(state) {
        Some(code) => serde_json::json!({
            "ok": true,
            "code": code,
            "url": format!("/?launch={code}"),
            "expires_in_secs": LAUNCH_CODE_TTL.as_secs(),
        })
        .to_string(),
        None => api_err("이 워크스테이션은 접근 토큰 없이 실행 중이라 실행 코드가 필요 없습니다."),
    }
}

// ---- R4-B15: audit key source handed to CLI children ----

/// The key source spec this workstation process runs with.
///
/// R5-S9: the workstation takes its audit key source from the
/// `FRAMETRACE_KEY_SOURCE` environment variable only (same spec syntax as
/// the CLI's global `--key-source`). The never-called
/// `serve::set_key_source` hook is gone: neither launcher (`frametrace`
/// without arguments, `frametrace-app`) parses CLI flags, so the variable
/// is the one documented way. The spec is then handed to every CLI child
/// as `--key-source`, and a child that still resolves another key says so
/// on stderr ([`KEY_MISMATCH_MARKER`]) — the server turns that into a job
/// warning.
fn workstation_key_source() -> Option<String> {
    std::env::var("FRAMETRACE_KEY_SOURCE")
        .ok()
        .map(|spec| spec.trim().to_string())
        .filter(|spec| !spec.is_empty())
}

/// R5-S9: stderr line prefix of a child's audit-key mismatch warning
/// (machine-readable; the server promotes such lines to job warnings).
pub const KEY_MISMATCH_MARKER: &str = "FRAMETRACE-KEY-MISMATCH: ";

/// `<key id>:<12-hex fingerprint>` of a resolved audit key, `none` without
/// one, `error` when the configured source is unusable. The fingerprint
/// is a domain-separated hash prefix — it identifies, never reveals, the
/// key.
pub(crate) fn audit_key_tag(key: Result<Option<crate::audit_key::AuditKey>, String>) -> String {
    use sha2::{Digest, Sha256};
    match key {
        Ok(Some(key)) => {
            let mut hasher = Sha256::new();
            hasher.update(b"frametrace-key-tag:");
            hasher.update(&key.bytes);
            let digest = hasher.finalize();
            let fp: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
            format!("{}:{fp}", key.id)
        }
        Ok(None) => "none".to_string(),
        Err(_) => "error".to_string(),
    }
}

/// Pure comparison behind [`key_source_mismatch_warning`].
pub(crate) fn key_tag_mismatch(expected: &str, actual: &str) -> Option<String> {
    (expected.trim() != actual.trim()).then(|| {
        format!(
            "경고: 이 단계의 감사 키({actual})가 워크스테이션의 감사 키({expected})와 다릅니다 — 감사 기록의 키가 섞일 수 있습니다. --key-source 설정을 확인하십시오. / audit key differs from the workstation's"
        )
    })
}

/// R4-B15, child side: a CLI process started by the workstation compares
/// its own resolved audit key with the one the workstation announced
/// (`FRAMETRACE_EXPECTED_AUDIT_KEY`) and returns a warning on mismatch.
/// `None` outside the workstation or when they agree.
///
/// `cli::run` calls this right after installing `--key-source` and prints
/// the warning to stderr.
pub fn key_source_mismatch_warning() -> Option<String> {
    let expected = std::env::var(jobs::EXPECTED_AUDIT_KEY_ENV).ok()?;
    key_tag_mismatch(&expected, &audit_key_tag(crate::audit_key::configured()))
}

/// One counted in-flight connection (P11): decrements on drop, including
/// while a panicking handler thread unwinds.
pub(crate) struct InFlightSlot(pub(crate) Arc<AtomicUsize>);

impl Drop for InFlightSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accept loop shared by `run` and the integration tests (no browser side
/// effects here). Nonblocking so the /api/shutdown flag can end the loop
/// without a wake-up connection. The server only exits on an explicit
/// shutdown request — never on idle timeout.
///
/// The loop never takes the state lock: the shutdown flag is an atomic
/// cloned once up front, so a handler holding `JobState` cannot stall
/// accepting (a stalled accept loop fills the listen backlog and Windows
/// then refuses new connections outright).
pub(crate) fn serve_on(listener: TcpListener, state: SharedState) {
    let in_flight = Arc::new(AtomicUsize::new(0));
    let (shutdown, limits) = {
        let guard = state_lock(&state);
        (Arc::clone(&guard.shutdown), Arc::clone(&guard.conn_limits))
    };
    let _ = listener.set_nonblocking(true);
    loop {
        if shutdown.load(Ordering::SeqCst) {
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
                // R5-S7: the header-phase slot is taken here, before a
                // thread exists; a full pre-auth pool costs no thread.
                let Some(preauth) = http::take_conn_slot(&limits, false) else {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    let _ = write_simple(
                        &mut stream,
                        503,
                        b"too many unauthenticated connections - retry shortly",
                    );
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                };
                // P11: the slot is released by a drop guard, so a handler
                // that panics still frees it (a leak would eventually turn
                // every connection into a 503 and stall shutdown's drain).
                // A failed spawn drops the closure, and with it the guard.
                let slot = InFlightSlot(Arc::clone(&in_flight));
                let limits = Arc::clone(&limits);
                let _ = thread::Builder::new()
                    .name("ft-http".into())
                    .spawn(move || {
                        let _slot = slot;
                        let _ = http::handle_connection_with(
                            stream,
                            state,
                            http::HEADER_READ_TIMEOUT,
                            &limits,
                            Some(preauth),
                        );
                    });
            }
            // Only an empty queue sleeps; every pending connection is
            // accepted back to back first.
            Err(_) => thread::sleep(ACCEPT_POLL),
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

/// The well-known workstation ports, tried in order before a random one
/// (the viewer keeps unsaved drafts per origin, so a stable port keeps a
/// crashed session's drafts reachable).
pub(crate) const PREFERRED_PORTS: std::ops::RangeInclusive<u16> = 8477..=8486;

// Only the Windows WebView2 shell claims a preferred port this way.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn first_free_port() -> Option<TcpListener> {
    first_free_port_noting().map(|(listener, _)| listener)
}

/// [`first_free_port`] plus the preferred ports it had to skip because
/// something else holds them (R4-B1 warning).
pub(crate) fn first_free_port_noting() -> Option<(TcpListener, Vec<u16>)> {
    let mut occupied = Vec::new();
    for port in PREFERRED_PORTS {
        match bind_loopback(port) {
            Ok(listener) => return Some((listener, occupied)),
            Err(_) => occupied.push(port),
        }
    }
    bind_loopback(0).ok().map(|listener| (listener, occupied))
}

/// R4-B1: warning for preferred ports held by processes that are not a
/// workstation this user's instance file vouches for (they are never
/// reused, whatever they answer).
pub(crate) fn occupied_ports_warning(occupied: &[u16]) -> Option<String> {
    (!occupied.is_empty()).then(|| {
        let list = occupied
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "경고: 포트 {list}을(를) 신원이 확인되지 않은 다른 프로세스가 쓰고 있어 건너뜁니다 — 이 워크스테이션은 다른 포트에서 시작합니다. / ports in use by an unverified process were skipped"
        )
    })
}

pub(crate) fn should_open_browser(options: &ServeOptions) -> bool {
    options.open_browser && std::env::var("FRAMETRACE_NO_BROWSER").as_deref() != Ok("1")
}

/// R4-B14: the system "open this" program off Windows — `open` on macOS
/// (there is no xdg-open there), `xdg-open` elsewhere.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn system_opener(target_os: &str) -> &'static str {
    if target_os == "macos" {
        "open"
    } else {
        "xdg-open"
    }
}

/// How long the opener may take before the open counts as failed (it
/// normally hands off to the desktop and exits within a second).
#[cfg_attr(windows, allow(dead_code))]
const OPENER_TIMEOUT_SECS: u64 = 15;

/// R4-B14: runs `program target`, waits for it (no zombie), and reports a
/// spawn failure, a timeout or a non-zero exit as an error — the old code
/// spawned and forgot, so "no browser opened" looked like success.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn run_opener(program: &str, target: &std::ffi::OsStr) -> Result<(), String> {
    let mut command = Command::new(program);
    command.arg(target);
    match crate::util::run_with_timeout_detailed(&mut command, Some(OPENER_TIMEOUT_SECS)) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!(
            "{program} 실패 (exit {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(err) => Err(format!("{program} 실행 실패: {}", err.message())),
    }
}

/// [`try_open_in_browser`] for callers without an error channel (the
/// WebView2 shell's fallback): a failure is printed, never swallowed.
/// R4-B2: a tokenized workstation URL is exchanged for a one-time launch
/// URL first — the token never reaches a browser command line.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn open_in_browser(url: &str) {
    let url = browser_safe_url(url, &discovery::request_launch_code);
    if let Err(err) = try_open_in_browser(&url) {
        eprintln!("frametrace: {err}");
    }
}

/// `http://127.0.0.1:<port>/?token=<t>` → `/?launch=<code>` (code minted
/// by that server through `mint`), or the bare page when no code could be
/// obtained. Any other URL is returned unchanged.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn browser_safe_url(url: &str, mint: &dyn Fn(u16, &str) -> Option<String>) -> String {
    let Some(rest) = url.strip_prefix("http://127.0.0.1:") else {
        return url.to_string();
    };
    let Some((port, query)) = rest.split_once("/?") else {
        return url.to_string();
    };
    let (Ok(port), Some(token)) = (port.parse::<u16>(), query_value(query, "token")) else {
        return url.to_string();
    };
    launch_url(port, mint(port, &token).as_deref())
}

/// Opens the workstation URL in a browser. `Err` when no launch strategy
/// worked (R4-B14) — the caller tells the examiner to open it by hand.
pub(crate) fn try_open_in_browser(url: &str) -> Result<(), String> {
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
            return Ok(());
        }
        if let Some(explorer) = launch::resolve_explorer_path(&env)
            && launch::spawn_detached(&explorer, &[std::ffi::OsString::from(url)])
        {
            return Ok(());
        }
        if launch::shell_open(url) {
            Ok(())
        } else {
            Err(format!(
                "could not open a browser for {url}; open it manually"
            ))
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // R5-S8: a one-time launch URL never goes on the opener's command
        // line (`/proc/<pid>/cmdline` and `ps` show it to every local
        // user for as long as xdg-open / open runs, well inside the code's
        // 30 s life). The opener gets the path of a user-private (0600, in
        // a 0700 folder) redirect page instead; the page is deleted once
        // the code has expired.
        let target = if url.contains("?launch=") {
            // R6-S3: without a home folder there is no user-private place
            // a sandboxed browser can read (the temp folder is shared):
            // no launch page — the examiner opens the printed URL.
            let Some(dir) = launcher_dir() else {
                return Err(format!(
                    "no home folder (HOME) for a private launch page; open {url} manually"
                ));
            };
            match write_launcher_file(&dir, url) {
                Ok(path) => {
                    schedule_launcher_removal(path.clone());
                    path.into_os_string()
                }
                Err(err) => {
                    return Err(format!(
                        "could not prepare a private launch page ({err}); open {url} manually"
                    ));
                }
            }
        } else {
            std::ffi::OsString::from(url)
        };
        run_opener(system_opener(std::env::consts::OS), &target)
            .map_err(|err| format!("could not open a browser for {url} ({err}); open it manually"))
    }
}

/// R5-S8: the redirect page handed to the system opener instead of a
/// launch URL (pure). `url` is our own `http://127.0.0.1:<port>/?launch=`
/// URL (hex code); it is HTML-escaped anyway.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn launcher_page_html(url: &str) -> String {
    let safe = crate::util::html_escape(url);
    let script_url = serde_json::to_string(url).unwrap_or_else(|_| "\"/\"".to_string());
    format!(
        "<!doctype html>\n<html lang=\"ko\"><head><meta charset=\"utf-8\">\n<meta name=\"referrer\" content=\"no-referrer\">\n<meta http-equiv=\"refresh\" content=\"0;url={safe}\">\n<title>FrameTrace</title></head>\n<body><p>FrameTrace 워크스테이션을 여는 중… / Opening the FrameTrace workstation…</p>\n<p><a href=\"{safe}\">열리지 않으면 여기를 누르십시오 / click here if nothing happens</a></p>\n<script>location.replace({script_url});</script>\n</body></html>\n"
    )
}

/// R5-S8: where launch pages go — `<Documents or home>/FrameTrace/launch`
/// (a NON-hidden folder in the examiner's home: sandboxed browsers such
/// as snap/flatpak Firefox can read it, and neither `/tmp` nor a dot
/// folder is reliably readable to them). R6-S3: `None` without a home
/// folder (the fallback would be the shared temp folder).
#[cfg(not(target_os = "windows"))]
fn launcher_dir() -> Option<PathBuf> {
    launcher_dir_from(&|name| std::env::var_os(name), &|path| path.is_dir())
}

/// [`launcher_dir`] with the environment injected (pure, tests).
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn launcher_dir_from(
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    is_dir: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let (root, temp_fallback) = jobs::default_case_root(false, env, is_dir, Path::new(""));
    (!temp_fallback).then(|| root.join("FrameTrace").join("launch"))
}

/// Launch pages older than this are removed by the next launch (a
/// process that exited before its own removal timer ran).
#[cfg_attr(windows, allow(dead_code))]
const LAUNCHER_PAGE_MAX_AGE: Duration = Duration::from_secs(120);

/// R5-S8: writes a fresh user-private launch page into `dir` (created
/// 0700; the file is created 0600 with `create_new`, so it never follows
/// or reuses an existing name) and sweeps old ones. Returns its path.
/// R6-S3: only `dir` itself is made 0700 — its parents (the examiner's
/// `~/Documents/FrameTrace`, which also holds cases) are created with the
/// default mode and never re-permissioned.
#[cfg(unix)]
pub(crate) fn write_launcher_file(dir: &Path, url: &str) -> Result<PathBuf, String> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(format!("{}: {err}", dir.display())),
    }
    // An existing launch folder keeps whatever mode it had; tighten it.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("{}: {err}", dir.display()))?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        let now = std::time::SystemTime::now();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let old = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| now.duration_since(time).ok())
                .is_some_and(|age| age >= LAUNCHER_PAGE_MAX_AGE);
            if name.starts_with("open-") && name.ends_with(".html") && old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|err| format!("entropy: {err}"))?;
    let name: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("open-{name}.html"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    file.write_all(launcher_page_html(url).as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(path)
}

/// R5-S8: removes the launch page once its code can no longer be used
/// (the code lives [`LAUNCH_CODE_TTL`] and opens once). Runs on its own
/// thread; if this process exits first, the next launch sweeps the page.
#[cfg(not(target_os = "windows"))]
fn schedule_launcher_removal(path: PathBuf) {
    let _ = thread::Builder::new()
        .name("ft-launch-page".into())
        .spawn(move || {
            thread::sleep(LAUNCH_CODE_TTL + Duration::from_secs(5));
            let _ = std::fs::remove_file(&path);
        });
}

/// Opens a folder in the system file manager; `Err` when it could not be
/// opened (R4-B14 — reported to the UI instead of a silent success).
pub(crate) fn open_in_explorer(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let env = |name: &str| std::env::var_os(name);
        if let Some(explorer) = launch::resolve_explorer_path(&env)
            && launch::spawn_detached(&explorer, &[path.as_os_str().to_owned()])
        {
            return Ok(());
        }
        if launch::shell_open(&path.to_string_lossy()) {
            Ok(())
        } else {
            Err(format!("탐색기에서 열지 못했습니다: {}", path.display()))
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        run_opener(system_opener(std::env::consts::OS), path.as_os_str())
            .map_err(|err| format!("폴더를 열지 못했습니다 ({}): {err}", path.display()))
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
        use super::{is_absolute_windows_path, resolve_edge_path, resolve_explorer_path};
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
            // Windows-style absolute on every host OS (the resolver's contract).
            assert!(is_absolute_windows_path(&found));
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
    match resolve_open_folder(state, &raw).and_then(|path| open_in_explorer(&path)) {
        Ok(()) => "{\"ok\":true}".to_string(),
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
    // R4-B19: the UNC/device gate also guards every direct `route` caller
    // (handle_connection runs it first too) — path fields such as
    // `/api/advanced`'s `extra` never reach a handler unchecked.
    if !http::path_fields_for(&request.method, &request.path).is_empty()
        && let Some(reason) =
            http::remote_path_violation(request, &http::approved_path_roots(state))
    {
        return http::remote_path_refusal(reason);
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => page(examiner_page(legacy_ui()).as_bytes().to_vec()),
        ("GET", "/api/env") => json(api_env(request)),
        ("GET", "/api/browse") => json(api_browse(request)),
        ("GET", "/api/records") => json(api_records(request, state)),
        ("GET", "/api/records-meta") => json(api_records_meta(state)),
        ("GET", "/api/status") => json(api_status(state)),
        ("GET", "/api/mark-conflicts") => json(jobs::api_mark_conflicts_page(request, state)),
        ("GET", "/api/marks-pending") => json(api_marks_pending(request, state)),
        ("POST", "/api/launch-code") => json(api_launch_code(state)),
        ("POST", "/api/start") => json(api_start(request, state)),
        ("POST", "/api/reimport-raw") => json(jobs::api_reimport_raw(request, state)),
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
        ("POST", "/api/join-check") => json(api_join_check(request, state)),
        ("POST", "/api/join") => json(api_join(request, state)),
        ("POST", "/api/audio-measure") => json(api_audio_measure(request, state)),
        ("GET", "/api/recent-cases") => json(api_recent_cases()),
        ("POST", "/api/recent-cases/forget") => json(api_recent_cases_forget(request)),
        ("GET", "/api/tools") => json(tools::api_tools(request, state)),
        ("GET", "/api/tools/install-status") => json(tools::api_tools_install_status(state)),
        ("POST", "/api/tools/install") => json(tools::api_tools_install(request, state)),
        ("POST", "/api/tools/install-cancel") => {
            json(tools::api_tools_install_cancel(request, state))
        }
        _ => plain(404, b"not found".to_vec()),
    }
}
