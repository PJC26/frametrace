//! Native WebView2 workstation shell (Windows only).
//!
//! `frametrace-app.exe` with no arguments hosts the examiner workstation
//! inside a real desktop window instead of opening a browser tab: the
//! loopback server runs on a background thread and an embedded WebView2
//! navigates to it. If the WebView2 runtime is unavailable the shell
//! falls back to the ordinary browser path, so the workstation always
//! opens *something* the examiner can use.
//!
//! Closing the window posts `/api/shutdown` so the embedded server exits
//! cleanly. While a job runs the server refuses that, so the frame first
//! asks the examiner ("cancel the job and quit?"), cancels, waits a
//! bounded time for the job to stop, and only then closes; the shutdown
//! path itself is bounded too (cancel → retry → forced exit) so a closed
//! window can never leave an invisible process behind. When the shell
//! attaches to an already-running workstation it does NOT shut it down on
//! close — that mirrors the browser semantics where closing a tab leaves
//! the server alive.

use crate::serve::{self, ServeOptions};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicPtr, AtomicU16, AtomicU32, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
};
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::Foundation::{E_POINTER, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{PROCESS_PER_MONITOR_DPI_AWARE, SetProcessDpiAwareness};
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetMessageW, MSG, PostQuitMessage, RegisterClassW, SW_SHOW, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DESTROY, WM_SIZE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;
use windows::core::{HSTRING, PCWSTR};

thread_local! {
    static CONTROLLER: RefCell<Option<ICoreWebView2Controller>> = const { RefCell::new(None) };
}

/// P10: the controller is only ever touched through these helpers, which
/// hold the `RefCell` borrow just long enough to clone/move the COM
/// pointer out — never across a WebView2 call. `SetBounds` / `Close` can
/// pump messages and re-enter `frame_window_proc` (WM_SIZE); a borrow held
/// across them made the re-entrant borrow panic inside an `extern
/// "system"` callback, which aborts the process. A borrow that is still
/// busy reads as "no controller" instead of panicking.
pub(crate) fn cell_clone<T: Clone>(cell: &RefCell<Option<T>>) -> Option<T> {
    cell.try_borrow().ok().and_then(|slot| slot.clone())
}

/// Moves the value out (see [`cell_clone`]); `None` when empty or busy.
pub(crate) fn cell_take<T>(cell: &RefCell<Option<T>>) -> Option<T> {
    cell.try_borrow_mut().ok().and_then(|mut slot| slot.take())
}

/// Stores `value`; hands it back when the cell is busy (never panics).
pub(crate) fn cell_store<T>(cell: &RefCell<Option<T>>, value: T) -> Result<(), T> {
    match cell.try_borrow_mut() {
        Ok(mut slot) => {
            *slot = Some(value);
            Ok(())
        }
        Err(_) => Err(value),
    }
}

/// P13: balances a successful `CoInitializeEx` (S_OK *and* S_FALSE both
/// need a matching `CoUninitialize`) once every WebView2 object of the
/// window is released.
struct ComApartment;

impl ComApartment {
    fn init() -> Result<Self, String> {
        // SAFETY: initializes COM for this (UI) thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if com_init_needs_uninit(hr.0) {
            Ok(ComApartment)
        } else {
            Err(format!(
                "CoInitializeEx failed: {}",
                windows::core::Error::from(hr)
            ))
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: pairs the successful CoInitializeEx in `init`.
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
}

/// Whether a `CoInitializeEx` HRESULT must be paired with
/// `CoUninitialize`: every success code (S_OK, S_FALSE), never a failure
/// such as RPC_E_CHANGED_MODE.
pub(crate) fn com_init_needs_uninit(hresult: i32) -> bool {
    hresult >= 0
}

/// The frame window, shared with the server-watcher and cancel-then-close
/// threads. They post private `WM_APP + n` window messages to it: unlike
/// thread messages (`PostThreadMessageW`, hwnd == null), window messages
/// are dispatched by modal loops too (window drag/resize, `MessageBoxW`),
/// so a close request can no longer be dropped while the examiner moves
/// the window. Cleared in WM_DESTROY, so nothing is posted to a handle the
/// system may recycle afterwards; the private message ids (not WM_CLOSE)
/// make a stray post to a recycled handle harmless anyway.
static FRAME_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// UI thread id: fallback target while no frame window is published (the
/// pump still handles the thread-message form).
static FRAME_THREAD: AtomicU32 = AtomicU32::new(0);

/// Port of the server this process owns (0 when attached to a foreign
/// workstation). The WM_CLOSE handler consults it: only an owned server is
/// shut down on close, so only then does a running job need the
/// cancel-and-quit question.
static OWNED_PORT: AtomicU16 = AtomicU16::new(0);

/// When the in-flight cancel-then-close started (`None` when none runs).
/// A second WM_CLOSE (the examiner clicking × again) neither re-asks nor
/// destroys the frame early — but only until [`CLOSE_FORCE_AFTER`] has
/// passed (see [`reclose_forces_close`]), so a lost or failed worker can
/// never latch the window open for good. The worker also clears it when
/// it cannot deliver its message.
static CLOSING: Mutex<Option<Instant>> = Mutex::new(None);

/// Window message posted by the server watcher: the server is gone,
/// destroy the frame (WM_DESTROY then ends the pump).
const MSG_SERVER_GONE: u32 = WM_APP;
/// Window message posted by the cancel-then-close worker: the job stopped
/// (or the wait ran out), destroy the frame now.
const MSG_CLOSE_NOW: u32 = WM_APP + 1;

/// How long WM_CLOSE waits for a cancelled job to stop before the frame
/// closes anyway (the shutdown path below has its own bound after that).
const CLOSE_CANCEL_WAIT: Duration = Duration::from_secs(10);
/// Timeout for each connect/write/read of a request the UI thread makes
/// itself (the WM_CLOSE status probe) and of the cancel worker's status
/// polls: loopback answers in milliseconds, and the UI thread must not
/// freeze for long on a stalled server.
const UI_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
/// Timeout for each connect/write/read of the off-UI-thread shutdown and
/// cancel requests.
const WORKER_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// After this long a repeated WM_CLOSE during cancel-then-close destroys
/// the frame even if the worker never reported back: the worker's own
/// bound (initial cancel request, CLOSE_CANCEL_WAIT, one in-flight status
/// poll) plus slack.
const CLOSE_FORCE_AFTER: Duration = Duration::from_secs(10 + 1 + 3 + 2);
/// How long the shutdown path keeps retrying `/api/shutdown` (cancelling
/// first) before giving up on a clean exit.
const SHUTDOWN_RETRY_WAIT: Duration = Duration::from_secs(15);
/// After an accepted shutdown, how long to wait for the server thread to
/// end before forcing the process out.
const SERVER_EXIT_WAIT: Duration = Duration::from_secs(30);

/// Runs the workstation in a native WebView2 window. Returns when the
/// window (or the fallback browser session) ends.
pub fn run_hosted() -> Result<(), String> {
    // An already-running workstation wins: attach a window to it instead
    // of stacking a second server. We don't own it, so closing the window
    // leaves it running — same contract as the browser flow.
    let (target, owned_port) = if let Some(port) = serve::find_running_server() {
        // Attaching to a separately launched server: the instance file is
        // the only token source.
        (serve::shell_target(port, false, Duration::ZERO), None)
    } else {
        let port = reserve_port()?;
        // P4: the server thread reports how `serve::run` ended, so a
        // startup failure (token format, bind) is shown as itself instead
        // of a generic "did not bind in time" after the full wait.
        let (result_tx, result_rx) = mpsc::channel::<Result<(), String>>();
        let server = std::thread::Builder::new()
            .name("frametrace-serve".to_string())
            .spawn(move || {
                let result = serve::run(ServeOptions {
                    case_dir: None,
                    port: Some(port),
                    open_browser: false,
                });
                let _ = result_tx.send(result);
            })
            .map_err(|err| format!("failed to spawn workstation server thread: {err}"))?;
        wait_for_server(port, Duration::from_secs(15), &result_rx)?;
        OWNED_PORT.store(port, Ordering::SeqCst);
        // Server-exit watcher: if the server ends while the WebView2
        // window is still up (UI 종료 button, /api/shutdown, a fatal
        // error), close the frame rather than leaving a dead page open.
        // The channel lets the shutdown path wait for the server with a
        // bound instead of an unbounded `join`.
        let (server_done_tx, server_done_rx) = mpsc::channel::<()>();
        let watcher = std::thread::spawn(move || {
            let result = result_rx
                .recv()
                .unwrap_or_else(|_| Err(SERVER_THREAD_PANIC.into()));
            let _ = server.join();
            // P5: published before the frame hears about it, so a host
            // failure caused by the dying server is never answered with a
            // browser pointed at the dead URL.
            *SERVER_EXIT
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
            let _ = server_done_tx.send(());
            post_frame_message(MSG_SERVER_GONE);
        });
        // The server publishes its token in-process right after bind; wait
        // for that rather than re-reading the instance file.
        (
            serve::shell_target(port, true, Duration::from_secs(10)),
            Some((port, watcher, server_done_rx)),
        )
    };

    let hosted = match host_window(&target) {
        Ok(()) => true,
        Err(err) => match fallback_plan(owned_port.is_some(), server_exit(), &target, &err) {
            Fallback::OpenBrowser(url) => {
                eprintln!("webview2 shell unavailable, falling back to browser: {err}");
                serve::open_in_browser(&url);
                false
            }
            // Shown by the caller (message box); nothing to open.
            Fallback::Fail(message) => return Err(message),
        },
    };

    if let Some((port, watcher, server_done_rx)) = owned_port {
        if hosted {
            // Window closed by the examiner: stop our server cleanly. The
            // server refuses while a job runs; cancel and retry for a
            // bounded time, then force the exit — a closed window must
            // never leave an invisible workstation process behind.
            if !shutdown_server(port, SHUTDOWN_RETRY_WAIT) {
                eprintln!(
                    "frametrace: server still refused shutdown after {}s; forcing exit",
                    SHUTDOWN_RETRY_WAIT.as_secs()
                );
                serve::kill_all_tracked();
                // process::exit skips serve::run's own cleanup: drop the
                // instance file (only while it still names this process)
                // so discovery does not point at a dead server.
                serve::remove_instance_file_if_owned();
                std::process::exit(1);
            }
            match server_done_rx.recv_timeout(SERVER_EXIT_WAIT) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    eprintln!(
                        "frametrace: server thread did not exit within {}s; forcing exit",
                        SERVER_EXIT_WAIT.as_secs()
                    );
                    serve::kill_all_tracked();
                    serve::remove_instance_file_if_owned();
                    std::process::exit(1);
                }
            }
        }
        // Whether we hosted or fell back to a browser, the process keeps
        // running until the server exits (UI 종료 button, /api/shutdown,
        // or the shutdown request above). In the browser fallback the
        // examiner ends the session from the UI, so this wait is open.
        let _ = watcher.join();
        // A server that died with an error is reported, never a silent exit.
        if let Some(Err(err)) = server_exit() {
            return Err(format!(
                "워크스테이션 서버가 오류로 종료되었습니다: {err} / The workstation server stopped with an error."
            ));
        }
    }
    Ok(())
}

/// Reported when the server thread unwound instead of returning.
const SERVER_THREAD_PANIC: &str = "server thread panicked";

/// How the owned server thread ended (`None` while it runs), published by
/// the server watcher (P5).
static SERVER_EXIT: Mutex<Option<Result<(), String>>> = Mutex::new(None);

fn server_exit() -> Option<Result<(), String>> {
    SERVER_EXIT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// What the shell does when the WebView2 window could not be hosted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fallback {
    /// Open the workstation in the default browser.
    OpenBrowser(String),
    /// Nothing useful can be opened: show this error.
    Fail(String),
}

/// Pure P5 decision. `owned`: the server runs in this process;
/// `server_exit`: how it ended, if it already did; `target`: the URL (or
/// the reason there is none); `host_err`: why hosting failed. A browser is
/// only pointed at a server that is still alive — a dead URL would just
/// open a connection error with the real cause lost.
pub(crate) fn fallback_plan(
    owned: bool,
    server_exit: Option<Result<(), String>>,
    target: &Result<String, String>,
    host_err: &str,
) -> Fallback {
    if owned && let Some(exit) = server_exit {
        let cause = match exit {
            Ok(()) => "서버가 종료되었습니다 / the server exited".to_string(),
            Err(err) => err,
        };
        return Fallback::Fail(format!(
            "창을 여는 동안 워크스테이션 서버가 멈췄습니다: {cause} (WebView2: {host_err}) / The workstation server stopped while the window was opening."
        ));
    }
    match target {
        Ok(url) => Fallback::OpenBrowser(url.clone()),
        Err(message) => Fallback::Fail(format!("{message} (WebView2: {host_err})")),
    }
}

/// One poll of the server startup wait (P4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartupPoll {
    Ready,
    Wait,
    Failed(String),
}

/// Pure startup decision. `finished`: what the server thread reported, if
/// it already ended (a server that ended before accepting is a failure,
/// whatever else answers on the port); `connected`: the port accepts;
/// `timed_out`: the wait budget is spent.
pub(crate) fn startup_poll(
    port: u16,
    finished: Option<&Result<(), String>>,
    connected: bool,
    timed_out: bool,
) -> StartupPoll {
    match finished {
        Some(Err(err)) => StartupPoll::Failed(format!(
            "워크스테이션 서버를 시작하지 못했습니다: {err} / The workstation server failed to start: {err}"
        )),
        Some(Ok(())) => StartupPoll::Failed(format!(
            "워크스테이션 서버가 시작 직후 종료되었습니다 (127.0.0.1:{port}). / The workstation server exited during startup."
        )),
        None if connected => StartupPoll::Ready,
        None if timed_out => StartupPoll::Failed(format!(
            "workstation server did not bind 127.0.0.1:{port} in time"
        )),
        None => StartupPoll::Wait,
    }
}

/// Posts one of the private `MSG_*` messages to the frame: as a window
/// message while the frame exists (survives modal loops), otherwise as a
/// thread message to the UI thread. Returns whether it was queued.
fn post_frame_message(message: u32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, PostThreadMessageW};
    let hwnd = FRAME_HWND.load(Ordering::SeqCst);
    if !hwnd.is_null() {
        // SAFETY: a private message carrying no pointers.
        return unsafe {
            PostMessageW(
                Some(HWND(hwnd)),
                message,
                WPARAM::default(),
                LPARAM::default(),
            )
        }
        .is_ok();
    }
    let thread_id = FRAME_THREAD.load(Ordering::SeqCst);
    if thread_id == 0 {
        return false;
    }
    // SAFETY: a thread message carrying no pointers.
    unsafe { PostThreadMessageW(thread_id, message, WPARAM::default(), LPARAM::default()) }.is_ok()
}

fn set_closing(started: Option<Instant>) {
    *CLOSING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = started;
}

fn closing_since() -> Option<Instant> {
    *CLOSING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What a WM_CLOSE does while a cancel-then-close is in flight (started at
/// `started`): `true` = destroy the frame now (the worker had more than
/// enough time and evidently never reported back), `false` = ignore the
/// repeated click while the worker still runs.
pub(crate) fn reclose_forces_close(started: Instant, now: Instant) -> bool {
    now.saturating_duration_since(started) >= CLOSE_FORCE_AFTER
}

fn reserve_port() -> Result<u16, String> {
    // Bind-release-bind is a tiny TOCTOU window, but serve::run returns a
    // bind error rather than grabbing a random port, and the caller turns
    // that into the browser fallback — acceptable and self-healing.
    // Prefer the well-known ports (8477..8486) like the CLI server does:
    // the viewer keeps unsaved drafts in per-origin localStorage, so a
    // random port on every launch would strand a crashed session's drafts
    // under an address the next launch never opens.
    let listener =
        serve::first_free_port().ok_or_else(|| "failed to reserve a loopback port".to_string())?;
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    if port == 0 {
        return Err("reserved port resolved to 0".to_string());
    }
    Ok(port)
}

fn wait_for_server(
    port: u16,
    timeout: Duration,
    finished: &mpsc::Receiver<Result<(), String>>,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let ended = match finished.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(SERVER_THREAD_PANIC.to_string())),
        };
        let connected = ended.is_none() && TcpStream::connect(("127.0.0.1", port)).is_ok();
        match startup_poll(port, ended.as_ref(), connected, Instant::now() >= deadline) {
            StartupPoll::Ready => return Ok(()),
            StartupPoll::Failed(message) => return Err(message),
            StartupPoll::Wait => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Why a shell request produced no response (P3): the two are different
/// facts — nothing listening vs. a server that did not answer in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestFailure {
    /// The connect failed: nothing accepts on the port.
    Unreachable,
    /// Connected, but no response arrived (stalled / timed out).
    NoAnswer,
}

/// One request to our own server over a raw socket (no HTTP client
/// dependency). Returns the response body, or why there is none.
/// HTTP/1.0 so the server closes the connection after the response; a
/// read timeout bounds a stalled peer and whatever arrived before it is
/// still returned.
fn shell_request(port: u16, method: &str, path: &str) -> Result<String, RequestFailure> {
    shell_request_timed(port, method, path, WORKER_REQUEST_TIMEOUT)
}

/// [`shell_request`] with an explicit per-operation timeout (connect,
/// write and read are each bounded by `io_timeout`).
fn shell_request_timed(
    port: u16,
    method: &str,
    path: &str,
    io_timeout: Duration,
) -> Result<String, RequestFailure> {
    let query = match crate::serve::resolve_shell_token(port, crate::serve::token_for_port) {
        Some(token) => format!("?token={token}"),
        None => String::new(),
    };
    // No configured token means the mutating-request gate wants the
    // instance nonce — the shell shares the user's trust domain, so the
    // user-private instance file (or the ft_nonce cookie GET / plants)
    // supplies it. GETs do not need it.
    let nonce_header = if method == "POST" {
        instance_nonce(port)
            .map(|nonce| format!("X-FrameTrace-Nonce: {nonce}\r\n"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        TcpStream::connect_timeout(&addr, io_timeout).map_err(|err| match err.kind() {
            // A connect that merely ran out of time reached no verdict.
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                RequestFailure::NoAnswer
            }
            _ => RequestFailure::Unreachable,
        })?;
    let _ = stream.set_read_timeout(Some(io_timeout));
    let _ = stream.set_write_timeout(Some(io_timeout));
    let request = format!(
        "{method} {path}{query} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n{nonce_header}Content-Length: 0\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|_| RequestFailure::NoAnswer)?;
    let mut buffer = Vec::new();
    // A timeout after the response arrived is not a failure: keep what
    // was read.
    let _ = stream.read_to_end(&mut buffer);
    if buffer.is_empty() {
        return Err(RequestFailure::NoAnswer);
    }
    let text = String::from_utf8_lossy(&buffer);
    Ok(text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default())
}

/// What one `/api/shutdown` attempt established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShutdownReply {
    Accepted,
    /// Refused (a job runs) or an unexpected reply.
    Refused,
    /// Nothing listens any more: the server is gone.
    Unreachable,
    /// The server did not answer — NOT proof that it died (P3).
    NoAnswer,
}

/// POST /api/shutdown.
fn request_shutdown(port: u16) -> ShutdownReply {
    classify_shutdown_reply(shell_request(port, "POST", "/api/shutdown"))
}

/// Pure classification of a shutdown request's outcome.
pub(crate) fn classify_shutdown_reply(reply: Result<String, RequestFailure>) -> ShutdownReply {
    match reply {
        Ok(body) if shutdown_accepted(&body) => ShutdownReply::Accepted,
        Ok(_) => ShutdownReply::Refused,
        Err(RequestFailure::Unreachable) => ShutdownReply::Unreachable,
        Err(RequestFailure::NoAnswer) => ShutdownReply::NoAnswer,
    }
}

/// Next move of the bounded shutdown loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShutdownStep {
    /// Accepted, or the server is already gone.
    Done,
    /// Refused because work runs: cancel it (once), then retry.
    CancelAndRetry,
    /// No answer: retry without assuming anything (until the deadline).
    Retry,
}

/// Pure P3 decision: an unanswered shutdown is retried, never taken as
/// "the server is dead" (which skipped the forced cleanup of a live,
/// stalled server and its tool processes).
pub(crate) fn shutdown_step(reply: ShutdownReply) -> ShutdownStep {
    match reply {
        ShutdownReply::Accepted | ShutdownReply::Unreachable => ShutdownStep::Done,
        ShutdownReply::Refused => ShutdownStep::CancelAndRetry,
        ShutdownReply::NoAnswer => ShutdownStep::Retry,
    }
}

/// `/api/shutdown` reply → accepted? (`{"ok":true}`; a refusal carries
/// `"running":true`).
fn shutdown_accepted(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("ok").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

/// Shutdown with a bound: when the server refuses because a job runs,
/// cancel it and keep retrying until `wait` elapses. Returns whether the
/// shutdown was accepted (an unreachable server counts as accepted — it is
/// already gone, and the caller's server-exit wait is bounded too). A
/// server that does not answer is retried; when the bound runs out the
/// caller forces the exit (P3).
fn shutdown_server(port: u16, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    let mut cancelled = false;
    loop {
        match shutdown_step(request_shutdown(port)) {
            ShutdownStep::Done => return true,
            ShutdownStep::CancelAndRetry if !cancelled => {
                let _ = shell_request(port, "POST", "/api/cancel");
                cancelled = true;
            }
            ShutdownStep::CancelAndRetry | ShutdownStep::Retry => {}
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// What the frame does with a WM_CLOSE (see [`decide_close`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseAction {
    /// Nothing runs (or the server is unreachable): close now.
    Close,
    /// A job runs and the examiner chose to cancel it: cancel, wait for
    /// it to stop (bounded), then close.
    CancelThenClose,
    /// A job runs and the examiner chose to keep working.
    KeepOpen,
}

/// What the owned server's `/api/status` says about running work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkState {
    /// Nothing the shutdown path would refuse to interrupt.
    Idle,
    /// The busy pipeline/enrichment/finalize job, a clip export / join,
    /// or an export-selected / capture-frame write runs.
    Running,
    /// The status could not be read (timeout, stall, garbage, 401) — work
    /// MAY be running (P3: never read as "idle").
    Unknown,
    /// Nothing listens on the port any more: the server is gone.
    Gone,
}

/// Pure classification of a `/api/status` request (P3).
pub(crate) fn classify_status(reply: &Result<String, RequestFailure>) -> WorkState {
    let text = match reply {
        Ok(text) => text,
        Err(RequestFailure::Unreachable) => return WorkState::Gone,
        Err(RequestFailure::NoAnswer) => return WorkState::Unknown,
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return WorkState::Unknown;
    };
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return WorkState::Unknown;
    }
    let count = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let busy = value
        .get("busy")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if busy || count("clips_running") > 0 || count("writes_running") > 0 {
        WorkState::Running
    } else {
        WorkState::Idle
    }
}

/// Pure close decision: `work` is the owned server's state; the examiner
/// is asked (`confirm_cancel` while work runs, `confirm_unknown` when the
/// status could not be read or the server stopped answering) — closing
/// never silently interrupts work it could not rule out.
pub(crate) fn decide_close(
    work: WorkState,
    confirm_cancel: &dyn Fn() -> bool,
    confirm_unknown: &dyn Fn() -> bool,
) -> CloseAction {
    let confirmed = match work {
        WorkState::Idle => return CloseAction::Close,
        WorkState::Running => confirm_cancel(),
        WorkState::Unknown | WorkState::Gone => confirm_unknown(),
    };
    if confirmed {
        CloseAction::CancelThenClose
    } else {
        CloseAction::KeepOpen
    }
}

/// Whether the cancel-then-close wait may stop polling: the work stopped
/// or the server is gone. `Unknown` keeps waiting (bounded by the caller).
pub(crate) fn close_wait_done(work: WorkState) -> bool {
    matches!(work, WorkState::Idle | WorkState::Gone)
}

/// Modal yes/no in the examiner's language; UTF-16 so Korean renders.
fn confirm_cancel_dialog(hwnd: HWND) -> bool {
    confirm_dialog(
        hwnd,
        "분석 작업이 진행 중입니다.\n작업을 중단하고 FrameTrace를 종료할까요?\n\n예: 중단 후 종료    아니요: 계속 작업",
    )
}

/// Asked when the status could not be read (P3).
fn confirm_unknown_dialog(hwnd: HWND) -> bool {
    confirm_dialog(
        hwnd,
        "워크스테이션의 작업 상태를 확인하지 못했습니다 (서버 응답 없음).\n진행 중인 작업이 있다면 중단됩니다. FrameTrace를 종료할까요?\n\n예: 중단 후 종료    아니요: 계속 작업",
    )
}

fn confirm_dialog(hwnd: HWND, text: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_YESNO, MessageBoxW,
    };
    let text = HSTRING::from(text);
    let caption = HSTRING::from("FrameTrace");
    // SAFETY: live null-terminated UTF-16 strings; modal to our own window.
    let answer = unsafe {
        MessageBoxW(
            Some(hwnd),
            PCWSTR::from_raw(text.as_ptr()),
            PCWSTR::from_raw(caption.as_ptr()),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        )
    };
    answer == IDYES
}

/// Cancel the running job and wait (bounded) until the server reports no
/// running work; then tell the frame to close. Runs off the UI thread so
/// the window stays responsive (and shows the UI's own "cancelling"
/// state) meanwhile.
fn cancel_then_close(port: u16) {
    let spawned = std::thread::Builder::new()
        .name("frametrace-close".to_string())
        .spawn(move || {
            let _ = shell_request(port, "POST", "/api/cancel");
            let deadline = Instant::now() + CLOSE_CANCEL_WAIT;
            while Instant::now() < deadline {
                let status = shell_request_timed(port, "GET", "/api/status", UI_REQUEST_TIMEOUT);
                if close_wait_done(classify_status(&status)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if !post_frame_message(MSG_CLOSE_NOW) {
                // Undeliverable: unlatch so the examiner's next × starts
                // over instead of being ignored.
                eprintln!("frametrace: could not post the close message to the frame window");
                set_closing(None);
            }
        });
    if let Err(err) = spawned {
        eprintln!("frametrace: could not start the cancel-then-close worker: {err}");
        set_closing(None);
    }
}

/// Per-process instance nonce for the nonce gate: the instance file is
/// authoritative (user-private). The fallback (servers started before
/// instance files existed, or an unreadable file) is the `ft_nonce`
/// cookie that `GET /` plants — `/api/status`'s "instance" field is only
/// a SHA-256 fingerprint of the nonce and would be refused with 401.
fn instance_nonce(port: u16) -> Option<String> {
    if let Some((instance_port, instance, _)) = crate::serve::read_instance_record()
        && instance_port == port
        && !instance.is_empty()
    {
        return Some(instance);
    }
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .ok()?;
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    nonce_from_response(&String::from_utf8_lossy(&buffer))
}

/// Extracts the `ft_nonce` cookie value from a raw HTTP response's
/// `Set-Cookie` headers (header block only; the body is ignored).
fn nonce_from_response(response: &str) -> Option<String> {
    let head = response.split("\r\n\r\n").next().unwrap_or(response);
    head.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.trim().eq_ignore_ascii_case("set-cookie") {
            return None;
        }
        let pair = value.trim().split(';').next()?;
        let (cookie, nonce) = pair.split_once('=')?;
        let nonce = nonce.trim();
        (cookie.trim() == "ft_nonce" && !nonce.is_empty()).then(|| nonce.to_string())
    })
}

fn host_window(target: &Result<String, String>) -> Result<(), String> {
    unsafe {
        // Dropped last (end of this function): after host_webview has
        // released the controller, the webview and the environment.
        let _com = ComApartment::init()?;
        // DPI awareness may already be fixed by a manifest or a parent
        // process (E_ACCESSDENIED); a slightly blurry window is far better
        // than abandoning the shell for the browser fallback over it.
        if let Err(err) = SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE) {
            eprintln!("frametrace: SetProcessDpiAwareness failed ({err}); continuing without it");
        }

        let class = WNDCLASSW {
            lpfnWndProc: Some(frame_window_proc),
            lpszClassName: w!("FrameTraceWebView"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("FrameTraceWebView"),
            w!("FrameTrace Examiner Workstation"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1440,
            900,
            None,
            None,
            GetModuleHandleW(None).ok().map(|h| HINSTANCE(h.0)),
            None,
        )
        .map_err(|err| format!("CreateWindowExW failed: {err}"))?;
        if hwnd.0.is_null() {
            return Err("CreateWindowExW returned a null window handle".to_string());
        }

        // The watcher and the close worker post to the frame window (or,
        // while no window is published, to this thread).
        FRAME_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
        FRAME_HWND.store(hwnd.0, Ordering::SeqCst);
        let result = host_webview(hwnd, target);
        FRAME_HWND.store(std::ptr::null_mut(), Ordering::SeqCst);
        FRAME_THREAD.store(0, Ordering::SeqCst);
        if let Err(err) = result {
            // Browser fallback happens at the caller — the dead frame must
            // not linger on screen looking like a broken app.
            let _ = DestroyWindow(hwnd);
            return Err(err);
        }
        Ok(())
    }
}

unsafe fn host_webview(hwnd: HWND, target: &Result<String, String>) -> Result<(), String> {
    unsafe {
        // WebView2 user data lives under LOCALAPPDATA — the default
        // "<exe>.WebView2" beside the binary is not writable when the app
        // is installed under Program Files.
        let user_data = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("FrameTrace")
            .join("WebView2");
        let _ = std::fs::create_dir_all(&user_data);
        let user_data_wide = HSTRING::from(user_data.as_os_str());
        let no_options: Option<&ICoreWebView2EnvironmentOptions> = None;

        let environment = {
            let (tx, rx) = mpsc::channel();
            CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
                Box::new(move |handler| {
                    CreateCoreWebView2EnvironmentWithOptions(
                        PCWSTR::null(),
                        PCWSTR::from_raw(user_data_wide.as_ptr()),
                        no_options,
                        &handler,
                    )
                    .map_err(webview2_com::Error::WindowsError)
                }),
                Box::new(move |error_code, environment| {
                    error_code?;
                    tx.send(environment.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                        .map_err(|_| {
                            windows::core::Error::from(windows::Win32::Foundation::E_FAIL)
                        })?;
                    Ok(())
                }),
            )
            .map_err(|err| format!("WebView2 environment creation failed: {err}"))?;
            rx.recv()
                .map_err(|_| "WebView2 environment callback never fired".to_string())?
                .map_err(|err| format!("WebView2 environment failed: {err}"))?
        };

        let controller = {
            let (tx, rx) = mpsc::channel();
            CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
                Box::new(move |handler| {
                    environment
                        .CreateCoreWebView2Controller(hwnd, &handler)
                        .map_err(webview2_com::Error::WindowsError)
                }),
                Box::new(move |error_code, controller| {
                    error_code?;
                    tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                        .map_err(|_| {
                            windows::core::Error::from(windows::Win32::Foundation::E_FAIL)
                        })?;
                    Ok(())
                }),
            )
            .map_err(|err| format!("WebView2 controller creation failed: {err}"))?;
            rx.recv()
                .map_err(|_| "WebView2 controller callback never fired".to_string())?
                .map_err(|err| format!("WebView2 controller failed: {err}"))?
        };

        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        controller
            .SetBounds(rect)
            .map_err(|err| format!("WebView2 SetBounds failed: {err}"))?;
        controller
            .SetIsVisible(true)
            .map_err(|err| format!("WebView2 SetIsVisible failed: {err}"))?;
        let webview = controller
            .CoreWebView2()
            .map_err(|err| format!("WebView2 CoreWebView2 failed: {err}"))?;

        match target {
            Ok(url) => {
                let url_wide = HSTRING::from(url.as_str());
                webview
                    .Navigate(PCWSTR::from_raw(url_wide.as_ptr()))
                    .map_err(|err| format!("WebView2 navigate to {url} failed: {err}"))?;
            }
            Err(message) => {
                // Explicit error page instead of a blank 401 / dead frame.
                let escaped = message
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                let html = HSTRING::from(format!(
                    "<!doctype html><meta charset=\"utf-8\"><title>FrameTrace</title>\
                     <body style=\"font:16px sans-serif;margin:3em\"><h2>FrameTrace</h2><p>{escaped}</p></body>"
                ));
                webview
                    .NavigateToString(PCWSTR::from_raw(html.as_ptr()))
                    .map_err(|err| format!("WebView2 NavigateToString failed: {err}"))?;
            }
        }

        if CONTROLLER
            .with(|slot| cell_store(slot, controller))
            .is_err()
        {
            return Err("WebView2 controller slot was busy (re-entrant call)".to_string());
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        let mut msg = MSG::default();
        loop {
            let result = GetMessageW(&mut msg, None, 0, 0).0;
            match result {
                -1 => break,
                0 => break,
                // Thread-message form (posted while no frame hwnd was
                // published; the window form is handled in
                // frame_window_proc). Server gone → close the frame instead
                // of serving a dead page; close-now → destroy the frame
                // (WM_DESTROY then ends the pump).
                _ if msg.hwnd.0.is_null() && msg.message == MSG_SERVER_GONE => {
                    let _ = DestroyWindow(hwnd);
                    break;
                }
                _ if msg.hwnd.0.is_null() && msg.message == MSG_CLOSE_NOW => {
                    set_closing(None);
                    let _ = DestroyWindow(hwnd);
                }
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }

        // Moved out first: Close() may pump messages (WM_SIZE) that look
        // the controller up again (P10).
        if let Some(controller) = CONTROLLER.with(cell_take) {
            let _ = controller.Close();
        }
    }
    Ok(())
}

unsafe extern "system" fn frame_window_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
        WM_SIZE => {
            unsafe {
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                // Cloned out (an AddRef) so no borrow is held while
                // SetBounds runs and possibly re-enters this proc (P10).
                if let Some(controller) = CONTROLLER.with(cell_clone) {
                    let _ = controller.SetBounds(rect);
                }
            }
            LRESULT(0)
        }
        // Posted by the server watcher (server gone) or the cancel worker
        // (job stopped / wait over): destroy the frame.
        MSG_SERVER_GONE | MSG_CLOSE_NOW => {
            set_closing(None);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            if let Some(started) = closing_since() {
                // A cancel-then-close is already in flight: ignore the
                // repeated ×, unless the worker is long overdue.
                if reclose_forces_close(started, Instant::now()) {
                    eprintln!(
                        "frametrace: cancel-then-close did not report back in {}s; closing",
                        CLOSE_FORCE_AFTER.as_secs()
                    );
                    set_closing(None);
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                return LRESULT(0);
            }
            let port = OWNED_PORT.load(Ordering::SeqCst);
            // A foreign (attached) server is not shut down on close, so
            // its job is not interrupted either: no question to ask.
            let action = if port == 0 {
                CloseAction::Close
            } else {
                // On the UI thread: short timeouts keep a stalled server
                // from freezing the window (<= 1 s per connect/write/read).
                let status = shell_request_timed(port, "GET", "/api/status", UI_REQUEST_TIMEOUT);
                decide_close(
                    classify_status(&status),
                    &|| confirm_cancel_dialog(hwnd),
                    &|| confirm_unknown_dialog(hwnd),
                )
            };
            match action {
                CloseAction::KeepOpen => {}
                CloseAction::Close => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                CloseAction::CancelThenClose => {
                    set_closing(Some(Instant::now()));
                    cancel_then_close(port);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // No more posts to this handle (it may be recycled).
            FRAME_HWND.store(std::ptr::null_mut(), Ordering::SeqCst);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CLOSE_CANCEL_WAIT, CLOSE_FORCE_AFTER, CloseAction, Fallback, RequestFailure, ShutdownReply,
        ShutdownStep, StartupPoll, UI_REQUEST_TIMEOUT, WORKER_REQUEST_TIMEOUT, WorkState,
        cell_clone, cell_store, cell_take, classify_shutdown_reply, classify_status,
        close_wait_done, com_init_needs_uninit, decide_close, fallback_plan, nonce_from_response,
        reclose_forces_close, shutdown_accepted, shutdown_step, startup_poll,
    };
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    /// P3: status reply → work state. Only a well-formed `ok` status can
    /// say "idle"; timeouts, garbage and refusals are `Unknown`.
    #[test]
    fn status_classification_table() {
        let ok = |s: &str| Ok::<String, RequestFailure>(s.to_string());
        let cases: &[(Result<String, RequestFailure>, WorkState)] = &[
            (
                ok(r#"{"ok":true,"busy":false,"clips_running":0,"writes_running":0}"#),
                WorkState::Idle,
            ),
            (ok(r#"{"ok":true,"busy":false}"#), WorkState::Idle),
            (ok(r#"{"ok":true,"busy":true}"#), WorkState::Running),
            (
                ok(r#"{"ok":true,"busy":false,"clips_running":2}"#),
                WorkState::Running,
            ),
            (
                ok(r#"{"ok":true,"busy":false,"writes_running":1}"#),
                WorkState::Running,
            ),
            (ok("<html>not json</html>"), WorkState::Unknown),
            (ok("unauthorized"), WorkState::Unknown),
            (ok(r#"{"ok":false}"#), WorkState::Unknown),
            (ok(""), WorkState::Unknown),
            (Err(RequestFailure::NoAnswer), WorkState::Unknown),
            (Err(RequestFailure::Unreachable), WorkState::Gone),
        ];
        for (reply, expected) in cases {
            assert_eq!(classify_status(reply), *expected, "{reply:?}");
        }
    }

    /// P3: the cancel wait stops on idle or a gone server, never on an
    /// unreadable status.
    #[test]
    fn close_wait_stops_only_on_idle_or_gone() {
        assert!(close_wait_done(WorkState::Idle));
        assert!(close_wait_done(WorkState::Gone));
        assert!(!close_wait_done(WorkState::Running));
        assert!(!close_wait_done(WorkState::Unknown));
    }

    /// P3: an unanswered shutdown is retried, never read as "server dead".
    #[test]
    fn shutdown_reply_and_step_table() {
        let cases: &[(Result<String, RequestFailure>, ShutdownReply, ShutdownStep)] = &[
            (
                Ok(r#"{"ok":true}"#.into()),
                ShutdownReply::Accepted,
                ShutdownStep::Done,
            ),
            (
                Ok(r#"{"ok":false,"running":true}"#.into()),
                ShutdownReply::Refused,
                ShutdownStep::CancelAndRetry,
            ),
            (
                Ok("unauthorized".into()),
                ShutdownReply::Refused,
                ShutdownStep::CancelAndRetry,
            ),
            (
                Err(RequestFailure::NoAnswer),
                ShutdownReply::NoAnswer,
                ShutdownStep::Retry,
            ),
            (
                Err(RequestFailure::Unreachable),
                ShutdownReply::Unreachable,
                ShutdownStep::Done,
            ),
        ];
        for (reply, kind, step) in cases {
            let classified = classify_shutdown_reply(reply.clone());
            assert_eq!(classified, *kind, "{reply:?}");
            assert_eq!(shutdown_step(classified), *step, "{reply:?}");
        }
    }

    /// P4: the startup wait reports the server's own error at once.
    #[test]
    fn startup_poll_table() {
        let err: Result<(), String> = Err("FRAMETRACE_TOKEN must be 8+ chars".into());
        let ok: Result<(), String> = Ok(());
        assert_eq!(startup_poll(1, None, true, false), StartupPoll::Ready);
        assert_eq!(startup_poll(1, None, false, false), StartupPoll::Wait);
        match startup_poll(1, Some(&err), true, false) {
            StartupPoll::Failed(message) => {
                assert!(
                    message.contains("FRAMETRACE_TOKEN must be 8+ chars"),
                    "{message}"
                )
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            startup_poll(1, Some(&ok), false, false),
            StartupPoll::Failed(_)
        ));
        match startup_poll(7, None, false, true) {
            StartupPoll::Failed(message) => assert!(message.contains("127.0.0.1:7"), "{message}"),
            other => panic!("{other:?}"),
        }
    }

    /// P5: never a browser on a dead server; a missing URL is an error.
    #[test]
    fn fallback_plan_table() {
        let url: Result<String, String> = Ok("http://127.0.0.1:9/?token=x".into());
        let no_url: Result<String, String> = Err("token missing".into());
        assert_eq!(
            fallback_plan(true, None, &url, "no runtime"),
            Fallback::OpenBrowser("http://127.0.0.1:9/?token=x".into())
        );
        assert_eq!(
            fallback_plan(false, None, &url, "no runtime"),
            Fallback::OpenBrowser("http://127.0.0.1:9/?token=x".into())
        );
        for exit in [Ok(()), Err("bind failed".to_string())] {
            match fallback_plan(true, Some(exit.clone()), &url, "no runtime") {
                Fallback::Fail(message) => {
                    assert!(message.contains("no runtime"), "{message}");
                    if let Err(cause) = exit {
                        assert!(message.contains(&cause), "{message}");
                    }
                }
                other => panic!("dead server must not open a browser: {other:?}"),
            }
        }
        // An attached (not owned) server's exit state is not ours to know.
        assert!(matches!(
            fallback_plan(false, Some(Ok(())), &url, "x"),
            Fallback::OpenBrowser(_)
        ));
        match fallback_plan(false, None, &no_url, "no runtime") {
            Fallback::Fail(message) => assert!(message.contains("token missing"), "{message}"),
            other => panic!("{other:?}"),
        }
    }

    /// P10: the controller helpers never panic on a busy cell.
    #[test]
    fn controller_cell_helpers_survive_reentrancy() {
        let cell: RefCell<Option<u32>> = RefCell::new(None);
        assert_eq!(cell_store(&cell, 7), Ok(()));
        assert_eq!(cell_clone(&cell), Some(7));
        {
            // A borrow held further up the stack (the re-entrant case).
            let _outer = cell.borrow();
            assert_eq!(cell_clone(&cell), Some(7), "shared borrows coexist");
            assert_eq!(cell_take(&cell), None, "busy: no panic, no take");
            assert_eq!(cell_store(&cell, 8), Err(8), "busy: value handed back");
        }
        {
            let _outer = cell.borrow_mut();
            assert_eq!(cell_clone(&cell), None, "busy: no panic");
        }
        assert_eq!(cell_take(&cell), Some(7));
        assert_eq!(cell_take(&cell), None);
    }

    /// P13: S_OK and S_FALSE are paired with CoUninitialize, failures not.
    #[test]
    fn com_uninit_pairing_table() {
        let rpc_e_changed_mode = 0x8001_0106_u32 as i32;
        let e_outofmemory = 0x8007_000E_u32 as i32;
        for (hr, expected) in [
            (0, true),
            (1, true),
            (rpc_e_changed_mode, false),
            (e_outofmemory, false),
        ] {
            assert_eq!(com_init_needs_uninit(hr), expected, "{hr:#x}");
        }
    }

    /// A repeated × while cancel-then-close runs is ignored until the
    /// worker is overdue, then it force-closes (never latched forever).
    #[test]
    fn repeated_close_forces_only_after_the_worker_bound() {
        let started = Instant::now();
        assert!(!reclose_forces_close(started, started));
        assert!(!reclose_forces_close(started, started + CLOSE_CANCEL_WAIT));
        assert!(!reclose_forces_close(
            started,
            started + CLOSE_FORCE_AFTER - Duration::from_millis(1)
        ));
        assert!(reclose_forces_close(started, started + CLOSE_FORCE_AFTER));
        assert!(reclose_forces_close(
            started,
            started + CLOSE_FORCE_AFTER + Duration::from_secs(60)
        ));
        // A clock reading before `started` never forces.
        assert!(!reclose_forces_close(
            started + Duration::from_secs(1),
            started
        ));
        // The force bound covers the worker's own worst case: the initial
        // cancel request, the cancel wait, and one in-flight status poll.
        assert!(
            CLOSE_FORCE_AFTER > CLOSE_CANCEL_WAIT + UI_REQUEST_TIMEOUT + WORKER_REQUEST_TIMEOUT
        );
        assert!(UI_REQUEST_TIMEOUT <= Duration::from_secs(1));
    }

    /// Table: work state × examiner answer → action, and which question
    /// was asked (P3: an unreadable status asks instead of closing).
    #[test]
    fn close_decision_table() {
        #[derive(Debug, PartialEq, Clone, Copy)]
        enum Asked {
            None,
            Running,
            Unknown,
        }
        let cases: &[(WorkState, bool, CloseAction, Asked)] = &[
            (WorkState::Idle, true, CloseAction::Close, Asked::None),
            (WorkState::Idle, false, CloseAction::Close, Asked::None),
            (
                WorkState::Running,
                true,
                CloseAction::CancelThenClose,
                Asked::Running,
            ),
            (
                WorkState::Running,
                false,
                CloseAction::KeepOpen,
                Asked::Running,
            ),
            (
                WorkState::Unknown,
                true,
                CloseAction::CancelThenClose,
                Asked::Unknown,
            ),
            (
                WorkState::Unknown,
                false,
                CloseAction::KeepOpen,
                Asked::Unknown,
            ),
            (
                WorkState::Gone,
                true,
                CloseAction::CancelThenClose,
                Asked::Unknown,
            ),
            (
                WorkState::Gone,
                false,
                CloseAction::KeepOpen,
                Asked::Unknown,
            ),
        ];
        for (work, answer, expected, expect_asked) in cases {
            let asked = std::cell::Cell::new(Asked::None);
            let action = decide_close(
                *work,
                &|| {
                    asked.set(Asked::Running);
                    *answer
                },
                &|| {
                    asked.set(Asked::Unknown);
                    *answer
                },
            );
            assert_eq!(action, *expected, "work={work:?} answer={answer}");
            assert_eq!(asked.get(), *expect_asked, "work={work:?}");
        }
    }

    #[test]
    fn shutdown_reply_classification() {
        assert!(shutdown_accepted(r#"{"ok":true}"#));
        assert!(!shutdown_accepted(
            r#"{"ok":false,"running":true,"error":"작업이 진행 중입니다 — 중단 후 종료하십시오."}"#
        ));
        assert!(!shutdown_accepted(""));
        assert!(!shutdown_accepted("401 unauthorized"));
    }

    #[test]
    fn parses_ft_nonce_from_set_cookie() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: ft_nonce=abc123; HttpOnly; SameSite=Strict; Path=/\r\n\r\n<html>ft_nonce=bogus</html>";
        assert_eq!(nonce_from_response(response).as_deref(), Some("abc123"));
    }

    #[test]
    fn missing_or_empty_cookie_yields_none() {
        assert_eq!(
            nonce_from_response("HTTP/1.1 200 OK\r\n\r\nft_nonce=x"),
            None
        );
        assert_eq!(
            nonce_from_response(
                "HTTP/1.1 200 OK\r\nset-cookie: other=1\r\nSet-Cookie: ft_nonce=; Path=/\r\n\r\n"
            ),
            None
        );
    }
}
