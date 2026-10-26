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
    WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DESTROY, WM_SIZE, WM_TIMER, WNDCLASSW,
    WS_OVERLAPPEDWINDOW,
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
/// Teardown goes through [`release_slot`], which also reports a busy slot.
#[cfg(test)]
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

/// R9: takes the value out of `cell` and hands it to `close`, then drops
/// it. Returns whether the slot is verifiably empty afterwards — `false`
/// when the cell was still borrowed (a re-entrant path holds it), in which
/// case the value is left where it is and the caller must not tear down
/// what it depends on (COM) underneath it.
pub(crate) fn release_slot<T>(cell: &RefCell<Option<T>>, close: impl FnOnce(&T)) -> bool {
    match cell.try_borrow_mut() {
        Ok(mut slot) => {
            let taken = slot.take();
            // The borrow ends before `close` runs: Close() may pump
            // messages that look the slot up again (P10).
            drop(slot);
            if let Some(value) = taken {
                close(&value);
            }
            true
        }
        Err(_) => false,
    }
}

/// P13: balances a successful `CoInitializeEx` (S_OK *and* S_FALSE both
/// need a matching `CoUninitialize`) once every WebView2 object of the
/// window is released. R9: when a WebView2 object could not be released
/// (its slot was busy), uninitialization is skipped — releasing a COM
/// pointer after `CoUninitialize` (at thread-local teardown) is undefined
/// behaviour, while a still-initialized apartment at process exit is not.
struct ComApartment {
    uninit: bool,
}

impl ComApartment {
    fn init() -> Result<Self, String> {
        // SAFETY: initializes COM for this (UI) thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if com_init_needs_uninit(hr.0) {
            Ok(ComApartment { uninit: true })
        } else {
            Err(format!(
                "CoInitializeEx failed: {}",
                windows::core::Error::from(hr)
            ))
        }
    }
}

impl ComApartment {
    /// R9: leave COM initialized (a WebView2 object is still alive).
    fn keep_initialized(&mut self) {
        self.uninit = false;
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: pairs the successful CoInitializeEx in `init`; every
            // WebView2 object of this thread was released before.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
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

/// C9: `WM_QUERYENDSESSION` / `WM_ENDSESSION`.
const WM_QUERYENDSESSION_ID: u32 = 0x0011;
const WM_ENDSESSION_ID: u32 = 0x0016;

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
    let (target, owned_port) = match claim_port()? {
        PortClaim::Attach(port) => {
            // Attaching to a separately launched server: the instance
            // file is the only token source.
            (serve::shell_target(port, false, Duration::ZERO), None)
        }
        PortClaim::Own(listener, port) => {
            // P4: the server thread reports how `serve::run` ended, so a
            // startup failure (token format, bind) is shown as itself instead
            // of a generic "did not bind in time" after the full wait.
            let (result_tx, result_rx) = mpsc::channel::<Result<(), String>>();
            let server = std::thread::Builder::new()
                .name("frametrace-serve".to_string())
                .spawn(move || {
                    // R5: the listener bound by `claim_port` is served as is —
                    // never released and re-bound, so a second shell (or any
                    // other process) cannot take the port in between.
                    let result = serve::run_with_listener(
                        ServeOptions {
                            case_dir: None,
                            port: Some(port),
                            open_browser: false,
                        },
                        listener,
                    );
                    let _ = result_tx.send(result);
                })
                .map_err(|err| format!("failed to spawn workstation server thread: {err}"))?;
            wait_for_server(port, Duration::from_secs(15), &result_rx)?;
            // Only now, with this process's own listener serving, is the port
            // "ours" (R5): WM_CLOSE shuts down exactly what OWNED_PORT names.
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
        }
    };

    let smoke = smoke_mode();
    if smoke {
        smoke_watchdog();
    }
    let hosted = match host_window(&target) {
        Ok(()) => true,
        // The smoke run proves the WebView2 shell itself: no browser.
        Err(err) if smoke => {
            smoke_fail(format!("WebView2 shell could not be hosted: {err}"));
            if let Some((port, _, _)) = owned_port.as_ref() {
                let _ = shutdown_server(*port, SHUTDOWN_RETRY_WAIT);
            }
            return Err(smoke_failure().unwrap_or(err));
        }
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
                // C4: running writes get their cancel flag and up to 30 s
                // to finish; what is still being written is marked 중단됨.
                settle_writes_before_forced_exit(port);
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
                    serve::mark_abandoned_exports();
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
    if smoke && let Some(reason) = smoke_failure() {
        return Err(format!("FRAMETRACE_SHELL_SMOKE failed: {reason}"));
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
/// whatever else answers on the port); `connected`: `/api/status`
/// answered 200 to our token (C11: [`status_ready`]); `timed_out`: the
/// wait budget is spent.
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

/// Where the shell's workstation comes from (R5).
enum PortClaim {
    /// Another FrameTrace instance already serves (or just started on)
    /// this port: attach a window to it.
    Attach(u16),
    /// This process bound the port; the listener goes straight to the
    /// server thread (never released and re-bound).
    Own(std::net::TcpListener, u16),
}

/// One step of the R5 port decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortDecision {
    /// Serve on the port this process bound.
    Own,
    /// Drop our listener and attach to the instance on this port.
    Attach(u16),
    /// Another instance announced itself but does not answer yet: look
    /// again shortly.
    Recheck,
}

/// How long a shell that lost the first preferred port waits for the
/// instance that announced itself there to start answering (a double
/// launch: both shells start at once, the other one won 8477).
const ATTACH_GRACE: Duration = Duration::from_secs(3);

/// Pure R5 decision. `bound`: the port this process holds; `first`: the
/// first preferred port; `running`: a FrameTrace instance discovery
/// verified (nonce match / full status shape); `announced`: the port the
/// instance file names (a server writes it right after its bind, before it
/// serves); `grace_left`: the [`ATTACH_GRACE`] budget is not spent.
pub(crate) fn decide_port(
    bound: u16,
    first: u16,
    running: Option<u16>,
    announced: Option<u16>,
    grace_left: bool,
) -> PortDecision {
    if let Some(port) = running.filter(|port| *port != bound) {
        return PortDecision::Attach(port);
    }
    if bound == first {
        // Nobody held the first preferred port: no other instance can be
        // starting ahead of us.
        return PortDecision::Own;
    }
    if grace_left && announced.is_some_and(|port| port != bound) {
        return PortDecision::Recheck;
    }
    PortDecision::Own
}

/// Picks the shell's server (R5): an already-running instance wins;
/// otherwise this process binds a preferred port and KEEPS the listener.
/// When the first preferred port was taken, the taker may be a second
/// FrameTrace shell starting at the same moment — attach to it once
/// discovery verifies it instead of stacking a second workstation.
fn claim_port() -> Result<PortClaim, String> {
    if let Some(port) = serve::find_running_server() {
        return Ok(PortClaim::Attach(port));
    }
    let listener =
        serve::first_free_port().ok_or_else(|| "failed to reserve a loopback port".to_string())?;
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    if port == 0 {
        return Err("reserved port resolved to 0".to_string());
    }
    let first = *serve::PREFERRED_PORTS.start();
    let deadline = Instant::now() + ATTACH_GRACE;
    loop {
        let (running, announced) = if port == first {
            (None, None)
        } else {
            (
                serve::find_running_server(),
                serve::read_instance_record().map(|(announced, _, _)| announced),
            )
        };
        match decide_port(port, first, running, announced, Instant::now() < deadline) {
            PortDecision::Own => return Ok(PortClaim::Own(listener, port)),
            PortDecision::Attach(other) => {
                drop(listener);
                return Ok(PortClaim::Attach(other));
            }
            PortDecision::Recheck => std::thread::sleep(Duration::from_millis(100)),
        }
    }
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
        // C11: ready = an authorized 200 from /api/status, not a connect.
        let ready = ended.is_none()
            && status_ready(&shell_request_full(
                port,
                "GET",
                "/api/status",
                UI_REQUEST_TIMEOUT,
            ));
        match startup_poll(port, ended.as_ref(), ready, Instant::now() >= deadline) {
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
    shell_request_full(port, method, path, io_timeout).map(|(_, body)| body)
}

/// [`shell_request_timed`] that also returns the HTTP status code (0 when
/// the status line is unreadable).
fn shell_request_full(
    port: u16,
    method: &str,
    path: &str,
    io_timeout: Duration,
) -> Result<(u16, String), RequestFailure> {
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
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    Ok((
        status,
        text.split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default(),
    ))
}

/// C11: the server is ready only when `/api/status` answers 200 with a
/// well-formed `ok:true` body to THIS process's token — a bare TCP
/// connect (the old check) also succeeds against a listener that cannot
/// serve the UI yet (token not published: 401) or a foreign responder.
pub(crate) fn status_ready(reply: &Result<(u16, String), RequestFailure>) -> bool {
    match reply {
        Ok((200, body)) => serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|value| value.get("ok").and_then(serde_json::Value::as_bool))
            .unwrap_or(false),
        _ => false,
    }
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

// ---------------------------------------------------------------------------
// C15: viewer flush before close.
// ---------------------------------------------------------------------------

/// How long WM_CLOSE waits for `FTViewer.save()` (A15 contract).
const CLOSE_FLUSH_WAIT: Duration = Duration::from_secs(5);
/// Timer id of the flush wait.
const FLUSH_TIMER_ID: usize = 0xF7_15;

/// Runs in the page and posts the result back. R6 (contract S↔U4): the
/// page's own close flush `hBeforeClose()` is preferred — it knows the
/// read-only and timed-out cases and returns `{ok, pending, message,
/// warning}` (the read-only close warning used to be lost because the
/// shell called `FTViewer.save()` directly); `FTViewer.save()` (resolves
/// once every pending edit is saved or durably queued — A15) remains the
/// fallback for a page without it. Never throws; a page without a viewer
/// reports a clean save.
const FLUSH_SCRIPT: &str = r#"(async () => {
  const r = { ft_close_save: 1, ok: true, pending: 0, message: "", warning: "" };
  try {
    if (typeof window.hBeforeClose === "function") {
      const h = (await window.hBeforeClose(4000)) || {};
      r.ok = h.ok !== false;
      r.pending = Number(h.pending) || 0;
      r.message = String(h.message || "");
      r.warning = String(h.warning || "");
    } else {
      const v = window.FTViewer;
      if (v && typeof v.save === "function") {
        r.ok = (await v.save()) !== false;
        const s = typeof v.summary === "function" ? v.summary() : null;
        r.pending = (s && Number(s.pending)) || 0;
      }
    }
  } catch (e) {
    r.ok = false;
    r.error = String((e && e.message) || e);
  }
  try { window.chrome.webview.postMessage(JSON.stringify(r)); } catch (e) {}
})();"#;

/// R5-S14 (pure): whether the page at `url` can host the evidence viewer
/// (and so may hold unsaved edits): a loopback workstation page at `/`,
/// the embedded `/viewer/`, or a generated `/review/*.html` page.
/// Everything else — sandboxed case files and evidence (`/case/…`,
/// `/media`), `about:`/error pages, other hosts — cannot run the save
/// script and is not asked.
pub(crate) fn page_may_host_viewer(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| url.strip_prefix("http://localhost:"))
        .or_else(|| url.strip_prefix("http://[::1]:"))
    else {
        return false;
    };
    let Some(slash) = rest.find('/') else {
        // `http://127.0.0.1:8477` — the root page.
        return rest.bytes().all(|b| b.is_ascii_digit());
    };
    if !rest[..slash].bytes().all(|b| b.is_ascii_digit()) || slash == 0 {
        return false;
    }
    let path = rest[slash..]
        .split(['?', '#'])
        .next()
        .unwrap_or("/")
        .to_ascii_lowercase();
    matches!(
        path.as_str(),
        "/" | "/index.html" | "/viewer/" | "/viewer/index.html"
    ) || (path.starts_with("/review/")
        && !path["/review/".len()..].contains('/')
        && (path.ends_with(".html") || path.ends_with(".htm")))
}

/// What the page reported after its close flush.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SaveReport {
    pub(crate) ok: bool,
    pub(crate) pending: u64,
    /// R6: the page's own reason when not ok (`hBeforeClose().message`).
    pub(crate) message: String,
    /// R6: a note for a clean close (`hBeforeClose().warning`, e.g. the
    /// viewer was read-only).
    pub(crate) warning: String,
}

/// Longest page-supplied text the shell shows (characters).
const PAGE_TEXT_MAX_CHARS: usize = 300;

fn page_text(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .map(|text| {
            text.chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(PAGE_TEXT_MAX_CHARS)
                .collect::<String>()
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// R6 (pure): the close question for `verdict`, quoting the page's own
/// reason when it gave one.
pub(crate) fn close_question_text(verdict: &FlushVerdict, outcome: &FlushOutcome) -> String {
    let reason = match outcome {
        FlushOutcome::Report(report) if !report.message.is_empty() => Some(report.message.clone()),
        _ => None,
    };
    match (verdict, reason) {
        (_, Some(reason)) => format!(
            "{}\n지금 종료하면 저장되지 않은 편집을 잃을 수 있습니다. 그래도 종료할까요?\n\n예: 종료    아니요: 돌아가서 확인",
            as_sentence(&reason)
        ),
        (FlushVerdict::Unsaved(count), None) => format!(
            "저장 안 된 편집 {count}건이 있습니다.\n지금 종료하면 이 편집은 저장되지 않습니다. 그래도 종료할까요?\n\n예: 종료    아니요: 돌아가서 저장"
        ),
        _ => "편집 저장을 확인하지 못했습니다 (5초 안에 응답 없음).\n저장 안 된 편집이 있을 수 있습니다. 그래도 종료할까요?\n\n예: 종료    아니요: 돌아가서 확인".to_string(),
    }
}

/// R6 (pure): the note shown before a CLEAN close, when the page gave one
/// (the read-only viewer's warning).
pub(crate) fn close_notice_text(outcome: &FlushOutcome) -> Option<String> {
    match outcome {
        FlushOutcome::Report(report) if !report.warning.is_empty() => {
            Some(as_sentence(&report.warning))
        }
        _ => None,
    }
}

/// R7-S7 (pure): `text` ending in exactly one sentence mark. The page's
/// reason often already ends in `.` (or `…`, `?`, `!`, `。`); R6 appended
/// another `.` and showed `….`/`..`. Trailing whitespace and repeated
/// marks are trimmed; a missing mark becomes `.`.
///
/// R8-S8: R7 kept only the LAST mark of the trailing run, so `저장할까요?.`
/// or `정말?...` lost the question (and `기다리는 중….` its ellipsis).
/// Now a run with `?`/`!` keeps those marks (dots dropped), else a run
/// with an ellipsis keeps one ellipsis (`…` or `...`), else its last
/// `.`/`。`.
///
/// R9-S6: full-width `？`/`！`/`．` count as marks too (R8 kept `정말？.`
/// as is), and spaces inside the trailing run are part of it (`정말? .`
/// kept its dot and the space before it).
pub(crate) fn as_sentence(text: &str) -> String {
    const ASKED: [char; 4] = ['?', '!', '？', '！'];
    const ENDS: [char; 4] = ['.', '…', '。', '．'];
    let body = text
        .trim_end_matches(|c: char| c.is_whitespace() || ASKED.contains(&c) || ENDS.contains(&c));
    let run: String = text[body.len()..]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let asked: String = run.chars().filter(|c| ASKED.contains(c)).collect();
    if !asked.is_empty() {
        format!("{body}{asked}")
    } else if run.contains('…') {
        format!("{body}…")
    } else if run.contains("...") {
        format!("{body}...")
    } else if let Some(mark) = run.chars().next_back() {
        format!("{body}{mark}")
    } else {
        format!("{body}.")
    }
}

/// Parses a web message posted by [`FLUSH_SCRIPT`] (None for any other
/// message).
pub(crate) fn parse_save_report(message: &str) -> Option<SaveReport> {
    let value: serde_json::Value = serde_json::from_str(message).ok()?;
    if value
        .get("ft_close_save")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return None;
    }
    Some(SaveReport {
        ok: value
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        pending: value
            .get("pending")
            .and_then(serde_json::Value::as_f64)
            .filter(|n| n.is_finite() && *n > 0.0)
            .map(|n| n as u64)
            .unwrap_or(0),
        message: page_text(value.get("message")),
        warning: page_text(value.get("warning")),
    })
}

/// How the pre-close flush ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FlushOutcome {
    /// No web view (error page / not created): nothing to save.
    NoViewer,
    Report(SaveReport),
    /// No answer within [`CLOSE_FLUSH_WAIT`].
    TimedOut,
    /// `ExecuteScript` itself failed.
    ScriptFailed,
}

/// What the close does with a flush outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlushVerdict {
    /// Everything saved: continue closing.
    Clean,
    /// The viewer still holds this many unsaved edits: ask first.
    Unsaved(u64),
    /// Could not confirm (timeout, script failure, save refused with no
    /// count): ask first.
    Unknown,
}

/// Pure C15 decision.
pub(crate) fn flush_verdict(outcome: &FlushOutcome) -> FlushVerdict {
    match outcome {
        FlushOutcome::NoViewer => FlushVerdict::Clean,
        FlushOutcome::Report(report) if report.ok && report.pending == 0 => FlushVerdict::Clean,
        FlushOutcome::Report(report) if report.pending > 0 => FlushVerdict::Unsaved(report.pending),
        FlushOutcome::Report(_) | FlushOutcome::TimedOut | FlushOutcome::ScriptFailed => {
            FlushVerdict::Unknown
        }
    }
}

/// Close-flush progress (UI thread only; a Mutex for simplicity).
#[derive(Debug, Clone, PartialEq)]
enum FlushPhase {
    Idle,
    Flushing(Instant),
    Done(FlushOutcome),
}

static FLUSH: Mutex<FlushPhase> = Mutex::new(FlushPhase::Idle);

fn flush_phase() -> FlushPhase {
    FLUSH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn set_flush_phase(phase: FlushPhase) {
    *FLUSH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = phase;
}

thread_local! {
    static WEBVIEW: RefCell<Option<ICoreWebView2>> = const { RefCell::new(None) };
}

/// Ends a running flush with `outcome` and re-sends WM_CLOSE so the close
/// continues with it. Ignored when no flush is running (a late answer).
fn finish_flush(hwnd: HWND, outcome: FlushOutcome) {
    use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW};
    if !matches!(flush_phase(), FlushPhase::Flushing(_)) {
        return;
    }
    set_flush_phase(FlushPhase::Done(outcome));
    // SAFETY: our own timer on our own window; a private re-post.
    unsafe {
        let _ = KillTimer(Some(hwnd), FLUSH_TIMER_ID);
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM::default(), LPARAM::default());
    }
}

/// Starts the flush: runs [`FLUSH_SCRIPT`] and arms the bounded wait.
/// Returns the outcome right away when no flush can run.
fn start_flush(hwnd: HWND) -> Option<FlushOutcome> {
    use windows::Win32::UI::WindowsAndMessaging::SetTimer;
    let Some(webview) = WEBVIEW.with(cell_clone) else {
        return Some(FlushOutcome::NoViewer);
    };
    // R5-S14: only a page that can host the viewer is asked to save. A
    // sandboxed evidence page (`/case/…`, `/media`), an error page or a
    // foreign URL never runs the script, so asking it only cost a 5 s wait
    // on every close.
    let mut source = windows::core::PWSTR::null();
    // SAFETY: COM getter on the UI thread; take_pwstr frees the string.
    if unsafe { webview.Source(&mut source) }.is_ok() {
        let url = webview2_com::take_pwstr(source);
        if !page_may_host_viewer(&url) {
            return Some(FlushOutcome::NoViewer);
        }
    }
    set_flush_phase(FlushPhase::Flushing(Instant::now()));
    let script = HSTRING::from(FLUSH_SCRIPT);
    // The script's completion value is a pending Promise; the real answer
    // arrives as a web message (see `install_web_message_handler`).
    let handler =
        webview2_com::ExecuteScriptCompletedHandler::create(Box::new(|_error, _result| Ok(())));
    // SAFETY: live null-terminated UTF-16; COM call on the UI thread.
    let started = unsafe { webview.ExecuteScript(PCWSTR::from_raw(script.as_ptr()), &handler) };
    if started.is_err() {
        set_flush_phase(FlushPhase::Idle);
        return Some(FlushOutcome::ScriptFailed);
    }
    // SAFETY: a timer on our own window.
    unsafe {
        SetTimer(
            Some(hwnd),
            FLUSH_TIMER_ID,
            CLOSE_FLUSH_WAIT.as_millis() as u32,
            None,
        );
    }
    None
}

/// Registers the web-message handler that receives the flush report.
unsafe fn install_web_message_handler(webview: &ICoreWebView2, hwnd: HWND) -> Result<(), String> {
    let hwnd_raw = hwnd.0 as usize;
    let handler =
        webview2_com::WebMessageReceivedEventHandler::create(Box::new(move |_sender, args| {
            let Some(args) = args else {
                return Ok(());
            };
            let mut message = windows::core::PWSTR::null();
            // SAFETY: WebView2 allocates the string; take_pwstr frees it.
            if unsafe { args.TryGetWebMessageAsString(&mut message) }.is_ok() {
                let text = webview2_com::take_pwstr(message);
                if let Some(report) = parse_save_report(&text) {
                    finish_flush(
                        HWND(hwnd_raw as *mut core::ffi::c_void),
                        FlushOutcome::Report(report),
                    );
                }
            }
            Ok(())
        }));
    let mut token = 0i64;
    // SAFETY: COM registration on the UI thread.
    unsafe { webview.add_WebMessageReceived(&handler, &mut token) }
        .map_err(|err| format!("WebView2 add_WebMessageReceived failed: {err}"))
}

/// Asked when the viewer still holds unsaved edits (C15 / A15).
fn confirm_unsaved_dialog(hwnd: HWND, verdict: FlushVerdict, outcome: &FlushOutcome) -> bool {
    confirm_dialog(hwnd, &close_question_text(&verdict, outcome))
}

/// R6: an information box (OK only) before a clean close. Smoke mode
/// has nobody to read it: printed instead.
fn notice_dialog(hwnd: HWND, text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
    if smoke_mode() {
        eprintln!(
            "frametrace-shell: close notice: {}",
            text.replace('\n', " ")
        );
        return;
    }
    let text = HSTRING::from(text);
    let caption = HSTRING::from("FrameTrace");
    // SAFETY: live null-terminated UTF-16 strings; modal to our own window.
    unsafe {
        MessageBoxW(
            Some(hwnd),
            PCWSTR::from_raw(text.as_ptr()),
            PCWSTR::from_raw(caption.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

// ---------------------------------------------------------------------------
// C9: Windows shutdown / logoff.
// ---------------------------------------------------------------------------

/// What WM_QUERYENDSESSION answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndSessionDecision {
    /// Nothing runs: let Windows end the session.
    Allow,
    /// Work runs (or cannot be ruled out): block with a visible reason.
    Block,
}

/// Pure C9 decision: a running (or unknown) job blocks the session end
/// with a reason Windows shows; an idle or gone server lets it proceed.
pub(crate) fn decide_end_session(work: WorkState) -> EndSessionDecision {
    match work {
        WorkState::Idle | WorkState::Gone => EndSessionDecision::Allow,
        WorkState::Running | WorkState::Unknown => EndSessionDecision::Block,
    }
}

/// What WM_ENDSESSION does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndSessionAction {
    /// The session end was cancelled: drop the block reason, keep going.
    Resume,
    /// The session ends: cancel work, wait briefly, mark unfinished
    /// exports, stop process trees, drop the instance file.
    CancelAndCleanUp,
}

/// Pure C9 decision on WM_ENDSESSION's `wParam` (TRUE = ending).
pub(crate) fn end_session_action(ending: bool) -> EndSessionAction {
    if ending {
        EndSessionAction::CancelAndCleanUp
    } else {
        EndSessionAction::Resume
    }
}

/// Reason text Windows shows on the shutdown screen.
const SHUTDOWN_BLOCK_REASON: &str =
    "FrameTrace 작업 진행 중 — 작업을 마치거나 중단한 뒤 종료하십시오";
/// How long WM_ENDSESSION waits for cancelled work (Windows allows ~5 s).
const END_SESSION_WAIT: Duration = Duration::from_secs(3);

#[link(name = "user32")]
unsafe extern "system" {
    fn ShutdownBlockReasonCreate(hwnd: *mut core::ffi::c_void, reason: *const u16) -> i32;
    fn ShutdownBlockReasonDestroy(hwnd: *mut core::ffi::c_void) -> i32;
}

/// WM_ENDSESSION(TRUE): best effort within the session-end budget.
fn end_session_cleanup(port: u16) {
    if port != 0 {
        let _ = shell_request_timed(port, "POST", "/api/cancel", UI_REQUEST_TIMEOUT);
        let deadline = Instant::now() + END_SESSION_WAIT;
        while Instant::now() < deadline {
            let status = shell_request_timed(port, "GET", "/api/status", UI_REQUEST_TIMEOUT);
            if close_wait_done(classify_status(&status)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    serve::mark_abandoned_exports();
    serve::kill_all_tracked();
    serve::remove_instance_file_if_owned();
}

// ---------------------------------------------------------------------------
// C4: forced exit.
// ---------------------------------------------------------------------------

/// How long the forced-exit path waits for running case writes (export
/// copies, hashes, audit appends) after asking them to stop.
const FORCED_EXIT_WRITE_WAIT: Duration = Duration::from_secs(30);

/// `writes_running` from a status reply (`None` = unreadable).
pub(crate) fn writes_running(reply: &Result<String, RequestFailure>) -> Option<u64> {
    match reply {
        Err(RequestFailure::Unreachable) => Some(0),
        Err(RequestFailure::NoAnswer) => None,
        Ok(text) => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .filter(|value| value.get("ok").and_then(serde_json::Value::as_bool) == Some(true))
            .map(|value| {
                value
                    .get("writes_running")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            }),
    }
}

/// Pure C4 decision after the bounded write wait: exports still being
/// written (or an unreadable status) get stamped "중단됨" before exit.
pub(crate) fn must_mark_abandoned(writes: Option<u64>) -> bool {
    writes != Some(0)
}

/// C4: the forced-exit path's write lease handling — the cancel flags were
/// set by `/api/cancel` (shutdown_server); wait up to 30 s for every write
/// to finish (a copy in progress is completed, never torn), then stamp
/// whatever is still being written as "중단됨".
fn settle_writes_before_forced_exit(port: u16) {
    let _ = shell_request(port, "POST", "/api/cancel");
    let deadline = Instant::now() + FORCED_EXIT_WRITE_WAIT;
    let mut writes = None;
    while Instant::now() < deadline {
        writes = writes_running(&shell_request_timed(
            port,
            "GET",
            "/api/status",
            UI_REQUEST_TIMEOUT,
        ));
        if writes == Some(0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if must_mark_abandoned(writes) {
        let marked = serve::mark_abandoned_exports();
        eprintln!("frametrace: forced exit — {marked} unfinished export(s) marked 중단됨");
    }
}

// ---------------------------------------------------------------------------
// FRAMETRACE_SHELL_SMOKE=1 (CI): load the UI, wait for an authorized
// /api/status 200, then close cleanly through the normal path; exit 0.
// ---------------------------------------------------------------------------

/// Whether the smoke mode is on.
pub(crate) fn smoke_mode() -> bool {
    smoke_requested(std::env::var("FRAMETRACE_SHELL_SMOKE").ok().as_deref())
}

pub(crate) fn smoke_requested(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "1")
}

/// Upper bound for the whole smoke run before it is failed and closed.
const SMOKE_DEADLINE: Duration = Duration::from_secs(120);

/// First smoke failure (None = passing so far).
static SMOKE_FAILURE: Mutex<Option<String>> = Mutex::new(None);

fn smoke_fail(reason: String) {
    eprintln!("frametrace smoke: {reason}");
    let mut slot = SMOKE_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_none() {
        *slot = Some(reason);
    }
}

fn smoke_failure() -> Option<String> {
    SMOKE_FAILURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// After the page loaded: wait for an authorized /api/status 200, then
/// ask the frame to close (the normal WM_CLOSE path, flush included).
fn smoke_after_load(port: u16) {
    let _ = std::thread::Builder::new()
        .name("frametrace-smoke".into())
        .spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            // R5-S14 / U13: no server port means no workstation answered —
            // a failed smoke run, never a pass.
            if port == 0 {
                smoke_fail("no workstation port: the UI was not served by a server".into());
                post_frame_message(WM_CLOSE);
                return;
            }
            loop {
                if status_ready(&shell_request_full(
                    port,
                    "GET",
                    "/api/status",
                    UI_REQUEST_TIMEOUT,
                )) {
                    break;
                }
                if Instant::now() >= deadline {
                    smoke_fail("/api/status did not answer 200 within 60 s".into());
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            post_frame_message(WM_CLOSE);
        });
}

/// Smoke watchdog: a run that is still up after [`SMOKE_DEADLINE`] fails
/// and its frame is destroyed.
fn smoke_watchdog() {
    let _ = std::thread::Builder::new()
        .name("frametrace-smoke-watchdog".into())
        .spawn(|| {
            std::thread::sleep(SMOKE_DEADLINE);
            if !FRAME_HWND.load(Ordering::SeqCst).is_null() {
                smoke_fail(format!(
                    "the window did not close within {}s",
                    SMOKE_DEADLINE.as_secs()
                ));
                post_frame_message(MSG_CLOSE_NOW);
            }
        });
}

/// Registers the navigation-completed handler of the smoke mode.
unsafe fn install_smoke_handler(webview: &ICoreWebView2) -> Result<(), String> {
    let handler =
        webview2_com::NavigationCompletedEventHandler::create(Box::new(move |_sender, args| {
            let mut success = windows::core::BOOL(0);
            if let Some(args) = args {
                // SAFETY: COM getter on the UI thread.
                let _ = unsafe { args.IsSuccess(&mut success) };
            }
            if success.as_bool() {
                smoke_after_load(OWNED_PORT.load(Ordering::SeqCst));
            } else {
                smoke_fail("the workstation page failed to load".into());
                post_frame_message(MSG_CLOSE_NOW);
            }
            Ok(())
        }));
    let mut token = 0i64;
    // SAFETY: COM registration on the UI thread.
    unsafe { webview.add_NavigationCompleted(&handler, &mut token) }
        .map_err(|err| format!("WebView2 add_NavigationCompleted failed: {err}"))
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
    if smoke_mode() {
        // CI has nobody to answer: a question here is a smoke failure.
        smoke_fail(format!(
            "unexpected close question: {}",
            text.replace('\n', " ")
        ));
        return true;
    }
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

/// C2: `ft_nonce_<port>` (current servers) or the legacy `ft_nonce`.
fn is_nonce_cookie_name(name: &str) -> bool {
    name == "ft_nonce"
        || name
            .strip_prefix("ft_nonce_")
            .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
}

/// Extracts the `ft_nonce[_<port>]` cookie value from a raw HTTP response's
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
        (is_nonce_cookie_name(cookie.trim()) && !nonce.is_empty()).then(|| nonce.to_string())
    })
}

fn host_window(target: &Result<String, String>) -> Result<(), String> {
    unsafe {
        // Dropped last (end of this function): after host_webview has
        // released the webview and the environment, and after the
        // controller slot was emptied below (R9).
        let mut com = ComApartment::init()?;
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
        // R9: the web view (C15 flush handle) goes before the controller
        // and the apartment.
        let webview_released = WEBVIEW.with(|slot| release_slot(slot, |_| {}));
        // R9: the controller is released here, on every path, BEFORE the
        // apartment is torn down. Moved out first: Close() may pump
        // messages (WM_SIZE) that look the controller up again (P10).
        let released = webview_released
            && CONTROLLER.with(|slot| {
                release_slot(slot, |controller| {
                    let _ = controller.Close();
                })
            });
        if !released {
            eprintln!(
                "frametrace: WebView2 controller still borrowed at teardown; leaving COM initialized"
            );
            com.keep_initialized();
        }
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
        // C15: the close flush's answer channel.
        install_web_message_handler(&webview, hwnd)?;
        if smoke_mode() {
            install_smoke_handler(&webview)?;
        }
        if WEBVIEW
            .with(|slot| cell_store(slot, webview.clone()))
            .is_err()
        {
            return Err("WebView2 web view slot was busy (re-entrant call)".to_string());
        }

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
        // The controller is closed and released by host_window (R9).
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
        WM_TIMER if w_param.0 == FLUSH_TIMER_ID => {
            // C15: the viewer did not answer within the bound.
            finish_flush(hwnd, FlushOutcome::TimedOut);
            LRESULT(0)
        }
        WM_QUERYENDSESSION_ID => {
            // C9: Windows asks whether the session may end (restart,
            // shutdown, logoff). Running work blocks it with a reason the
            // examiner sees on the shutdown screen.
            let port = OWNED_PORT.load(Ordering::SeqCst);
            let work = if port == 0 {
                WorkState::Idle
            } else {
                classify_status(&shell_request_timed(
                    port,
                    "GET",
                    "/api/status",
                    UI_REQUEST_TIMEOUT,
                ))
            };
            match decide_end_session(work) {
                EndSessionDecision::Allow => LRESULT(1),
                EndSessionDecision::Block => {
                    let reason = HSTRING::from(SHUTDOWN_BLOCK_REASON);
                    // SAFETY: our own window; live UTF-16 string.
                    unsafe {
                        ShutdownBlockReasonCreate(hwnd.0, reason.as_ptr());
                    }
                    LRESULT(0)
                }
            }
        }
        WM_ENDSESSION_ID => {
            // SAFETY: our own window (no reason registered is harmless).
            unsafe {
                ShutdownBlockReasonDestroy(hwnd.0);
            }
            if end_session_action(w_param.0 != 0) == EndSessionAction::CancelAndCleanUp {
                // The session ends: the process may be terminated as soon
                // as this returns — cancel, settle, mark, reap now.
                end_session_cleanup(OWNED_PORT.load(Ordering::SeqCst));
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
            // C15: first let the viewer save (bounded), then decide.
            let outcome = match flush_phase() {
                FlushPhase::Idle => match start_flush(hwnd) {
                    // Flushing: WM_CLOSE comes back with the answer.
                    None => return LRESULT(0),
                    Some(outcome) => outcome,
                },
                FlushPhase::Flushing(started) => {
                    if started.elapsed() < CLOSE_FLUSH_WAIT + Duration::from_secs(2) {
                        // A repeated × while the save runs.
                        return LRESULT(0);
                    }
                    FlushOutcome::TimedOut
                }
                FlushPhase::Done(outcome) => outcome,
            };
            // The next close starts a fresh flush (e.g. after "keep open").
            set_flush_phase(FlushPhase::Idle);
            let verdict = flush_verdict(&outcome);
            if verdict != FlushVerdict::Clean {
                if smoke_mode() {
                    smoke_fail(format!("viewer flush before close: {outcome:?}"));
                } else if !confirm_unsaved_dialog(hwnd, verdict, &outcome) {
                    return LRESULT(0);
                }
            } else if let Some(notice) = close_notice_text(&outcome) {
                notice_dialog(hwnd, &notice);
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

    /// C11: only an authorized 200 with `ok:true` means "ready"; a bare
    /// accepted connection (the old check) does not.
    #[test]
    fn startup_readiness_table() {
        use super::status_ready;
        let reply = |code: u16, body: &str| Ok::<_, RequestFailure>((code, body.to_string()));
        assert!(status_ready(&reply(200, r#"{"ok":true,"busy":false}"#)));
        assert!(!status_ready(&reply(401, "unauthorized")));
        assert!(!status_ready(&reply(200, "<html>someone else</html>")));
        assert!(!status_ready(&reply(200, r#"{"ok":false}"#)));
        assert!(!status_ready(&reply(503, r#"{"ok":true}"#)));
        assert!(!status_ready(&reply(0, "")));
        assert!(!status_ready(&Err(RequestFailure::NoAnswer)));
        assert!(!status_ready(&Err(RequestFailure::Unreachable)));
    }

    /// R5-S14: only pages that can host the viewer are asked to save
    /// before close; sandboxed evidence pages, error pages and other hosts
    /// cannot run the script, and asking them only cost a 5 s wait.
    #[test]
    fn close_flush_asks_only_viewer_pages() {
        use super::page_may_host_viewer;
        for url in [
            "http://127.0.0.1:8477/",
            "http://127.0.0.1:8477",
            "http://127.0.0.1:8477/?launch=abc#x",
            "http://localhost:8480/viewer/",
            "http://127.0.0.1:8477/viewer/index.html",
            "http://127.0.0.1:8477/review/evidence-viewer.html?x=1",
            "http://[::1]:8477/review/index.html",
        ] {
            assert!(page_may_host_viewer(url), "{url}");
        }
        for url in [
            "http://127.0.0.1:8477/case/reports/case-report.html",
            "http://127.0.0.1:8477/media?path=C:/e/x.html",
            "http://127.0.0.1:8477/review/thumbs/a.jpg",
            "http://127.0.0.1:8477/review/sub/x.html",
            "about:blank",
            "file:///C:/x/review/index.html",
            "https://example.com/",
            "http://127.0.0.1.evil.com:8477/",
            "",
        ] {
            assert!(!page_may_host_viewer(url), "{url}");
        }
    }

    /// C15: the viewer's save report → close verdict. Anything but a
    /// confirmed clean save asks before closing.
    #[test]
    fn close_flush_verdict_table() {
        use super::{FlushOutcome, FlushVerdict, SaveReport, flush_verdict, parse_save_report};
        assert_eq!(
            parse_save_report(r#"{"ft_close_save":1,"ok":true,"pending":0}"#),
            Some(SaveReport {
                ok: true,
                pending: 0,
                ..SaveReport::default()
            })
        );
        assert_eq!(
            parse_save_report(r#"{"ft_close_save":1,"ok":false,"pending":3}"#),
            Some(SaveReport {
                ok: false,
                pending: 3,
                ..SaveReport::default()
            })
        );
        assert_eq!(parse_save_report(r#"{"ok":true}"#), None, "foreign message");
        assert_eq!(parse_save_report("not json"), None);
        let report = |ok, pending| {
            FlushOutcome::Report(SaveReport {
                ok,
                pending,
                ..SaveReport::default()
            })
        };
        let cases = [
            (FlushOutcome::NoViewer, FlushVerdict::Clean),
            (report(true, 0), FlushVerdict::Clean),
            (report(true, 2), FlushVerdict::Unsaved(2)),
            (report(false, 5), FlushVerdict::Unsaved(5)),
            (report(false, 0), FlushVerdict::Unknown),
            (FlushOutcome::TimedOut, FlushVerdict::Unknown),
            (FlushOutcome::ScriptFailed, FlushVerdict::Unknown),
        ];
        for (outcome, verdict) in cases {
            assert_eq!(flush_verdict(&outcome), verdict, "{outcome:?}");
        }
        assert_eq!(super::CLOSE_FLUSH_WAIT, Duration::from_secs(5));
        assert!(super::FLUSH_SCRIPT.contains("FTViewer"));
        assert!(super::FLUSH_SCRIPT.contains("save()"));
    }

    /// R7-S7: the page's close flush gets 4 s (`hBeforeClose(4000)`) of the
    /// shell's 5 s wait — R6 asked for 4.5 s, leaving 0.5 s for the page
    /// to post its answer — and a reason that already ends in a sentence
    /// mark is not given a second one (`…다..`).
    #[test]
    fn close_flush_budget_and_sentence_marks() {
        use super::{
            CLOSE_FLUSH_WAIT, FLUSH_SCRIPT, FlushOutcome, FlushVerdict, as_sentence,
            close_notice_text, close_question_text, parse_save_report,
        };
        assert!(
            FLUSH_SCRIPT.contains("window.hBeforeClose(4000)"),
            "{FLUSH_SCRIPT}"
        );
        assert!(Duration::from_millis(4000) + Duration::from_secs(1) <= CLOSE_FLUSH_WAIT);
        assert_eq!(as_sentence("저장하지 못했습니다."), "저장하지 못했습니다.");
        assert_eq!(as_sentence("저장하지 못했습니다 "), "저장하지 못했습니다.");
        assert_eq!(as_sentence("저장하지 못했습니다.."), "저장하지 못했습니다.");
        assert_eq!(as_sentence("기다리는 중..."), "기다리는 중...");
        assert_eq!(as_sentence("기다리는 중…"), "기다리는 중…");
        assert_eq!(as_sentence("저장할까요?"), "저장할까요?");
        assert_eq!(as_sentence(""), ".");
        // R8-S8: a question or exclamation is never lost to trailing dots.
        assert_eq!(as_sentence("저장할까요?."), "저장할까요?");
        assert_eq!(as_sentence("정말?... "), "정말?");
        assert_eq!(as_sentence("왜?!"), "왜?!");
        assert_eq!(as_sentence("기다리는 중…."), "기다리는 중…");
        assert_eq!(as_sentence("기다리는 중...."), "기다리는 중...");
        assert_eq!(as_sentence("끝。."), "끝.");
        // R9-S6: full-width marks and spaces inside the trailing run.
        assert_eq!(as_sentence("저장할까요？."), "저장할까요？");
        assert_eq!(as_sentence("정말！ ."), "정말！");
        assert_eq!(as_sentence("정말? . "), "정말?");
        assert_eq!(as_sentence("끝 ."), "끝.");
        assert_eq!(as_sentence("끝．"), "끝．");
        assert_eq!(as_sentence("기다리는 중 … ."), "기다리는 중…");
        let report = parse_save_report(
            r#"{"ft_close_save":1,"ok":false,"pending":1,"message":"Save timed out.","warning":"읽기 전용이었습니다."}"#,
        )
        .unwrap();
        let outcome = FlushOutcome::Report(report);
        let text = close_question_text(&FlushVerdict::Unknown, &outcome);
        assert!(text.starts_with("Save timed out.\n"), "{text}");
        assert!(!text.contains(".."), "{text}");
        assert_eq!(
            close_notice_text(&outcome).as_deref(),
            Some("읽기 전용이었습니다.")
        );
    }

    /// R6 (contract S↔U4): the shell's close flush asks the page's own
    /// `hBeforeClose()` first and carries its reason and its read-only
    /// warning to the close dialogs. Before, it called `FTViewer.save()`
    /// directly: a read-only viewer's "nothing unsaved, but read-only"
    /// warning never reached the examiner, and the question was generic.
    #[test]
    fn close_flush_prefers_hbeforeclose_and_keeps_its_texts() {
        use super::{
            FLUSH_SCRIPT, FlushOutcome, FlushVerdict, close_notice_text, close_question_text,
            flush_verdict, parse_save_report,
        };
        let prefer = FLUSH_SCRIPT
            .find("hBeforeClose")
            .expect("asks hBeforeClose");
        let fallback = FLUSH_SCRIPT
            .find("v.save()")
            .expect("FTViewer.save fallback");
        assert!(prefer < fallback, "hBeforeClose comes first");
        let readonly = parse_save_report(
            r#"{"ft_close_save":1,"ok":true,"pending":0,"message":"","warning":"판독 기록을 읽지 못한 상태였습니다"}"#,
        )
        .unwrap();
        let outcome = FlushOutcome::Report(readonly);
        assert_eq!(flush_verdict(&outcome), FlushVerdict::Clean);
        assert_eq!(
            close_notice_text(&outcome).as_deref(),
            Some("판독 기록을 읽지 못한 상태였습니다.")
        );
        let blocked = parse_save_report(
            r#"{"ft_close_save":1,"ok":false,"pending":0,"message":"판독 기록을 읽지 못해 저장이 막혀 있습니다\u0007"}"#,
        )
        .unwrap();
        assert_eq!(
            blocked.message,
            "판독 기록을 읽지 못해 저장이 막혀 있습니다"
        );
        let outcome = FlushOutcome::Report(blocked);
        let verdict = flush_verdict(&outcome);
        assert_eq!(verdict, FlushVerdict::Unknown);
        let text = close_question_text(&verdict, &outcome);
        assert!(
            text.starts_with("판독 기록을 읽지 못해 저장이 막혀 있습니다."),
            "{text}"
        );
        assert!(close_notice_text(&outcome).is_none());
        // Without a page reason the old texts stay.
        assert!(
            close_question_text(&FlushVerdict::Unsaved(2), &FlushOutcome::TimedOut)
                .contains("저장 안 된 편집 2건")
        );
    }

    /// C9: session end (restart / shutdown / logoff) is blocked while work
    /// runs or cannot be ruled out; an ending session cancels and cleans.
    #[test]
    fn end_session_decision_table() {
        use super::{EndSessionAction, EndSessionDecision, decide_end_session, end_session_action};
        assert_eq!(
            decide_end_session(WorkState::Idle),
            EndSessionDecision::Allow
        );
        assert_eq!(
            decide_end_session(WorkState::Gone),
            EndSessionDecision::Allow
        );
        assert_eq!(
            decide_end_session(WorkState::Running),
            EndSessionDecision::Block
        );
        assert_eq!(
            decide_end_session(WorkState::Unknown),
            EndSessionDecision::Block
        );
        assert_eq!(end_session_action(true), EndSessionAction::CancelAndCleanUp);
        assert_eq!(end_session_action(false), EndSessionAction::Resume);
        assert!(super::END_SESSION_WAIT < Duration::from_secs(5));
    }

    /// C4: the forced exit marks exports that are still being written (or
    /// whose state cannot be read) as 중단됨.
    #[test]
    fn forced_exit_write_table() {
        use super::{must_mark_abandoned, writes_running};
        let ok = |s: &str| Ok::<String, RequestFailure>(s.to_string());
        assert_eq!(
            writes_running(&ok(r#"{"ok":true,"writes_running":0}"#)),
            Some(0)
        );
        assert_eq!(
            writes_running(&ok(r#"{"ok":true,"writes_running":2}"#)),
            Some(2)
        );
        assert_eq!(writes_running(&ok(r#"{"ok":true}"#)), Some(0));
        assert_eq!(writes_running(&ok("garbage")), None);
        assert_eq!(writes_running(&ok(r#"{"ok":false}"#)), None);
        assert_eq!(writes_running(&Err(RequestFailure::NoAnswer)), None);
        assert_eq!(writes_running(&Err(RequestFailure::Unreachable)), Some(0));
        assert!(!must_mark_abandoned(Some(0)));
        assert!(must_mark_abandoned(Some(1)));
        assert!(must_mark_abandoned(None));
        assert_eq!(super::FORCED_EXIT_WRITE_WAIT, Duration::from_secs(30));
    }

    /// F2 contract: `FRAMETRACE_SHELL_SMOKE=1` exactly.
    #[test]
    fn smoke_switch_table() {
        use super::smoke_requested;
        assert!(smoke_requested(Some("1")));
        assert!(smoke_requested(Some(" 1 ")));
        assert!(!smoke_requested(Some("0")));
        assert!(!smoke_requested(Some("true")));
        assert!(!smoke_requested(None));
    }

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

    /// R9: the controller slot is emptied (and the value closed) before
    /// COM teardown; a busy slot reports failure so uninit is deferred.
    #[test]
    fn release_slot_closes_and_empties_or_reports_a_busy_slot() {
        use super::release_slot;
        let closed = std::cell::Cell::new(0);
        let cell = RefCell::new(Some(7));
        assert!(release_slot(&cell, |value| {
            assert_eq!(*value, 7);
            closed.set(closed.get() + 1);
        }));
        assert_eq!(closed.get(), 1);
        assert!(cell.borrow().is_none());
        // Empty slot: nothing to close, still verifiably empty.
        assert!(release_slot(&cell, |_| closed.set(closed.get() + 1)));
        assert_eq!(closed.get(), 1);
        // Busy slot (a re-entrant borrow): not released, value untouched.
        let busy = RefCell::new(Some(9));
        let held = busy.borrow();
        assert!(!release_slot(&busy, |_| closed.set(closed.get() + 1)));
        drop(held);
        assert_eq!(closed.get(), 1);
        assert_eq!(*busy.borrow(), Some(9));
        // The close callback may look the slot up again (P10).
        let reentrant = RefCell::new(Some(1));
        assert!(release_slot(&reentrant, |_| {
            assert!(super::cell_clone(&reentrant).is_none());
        }));
    }

    /// R5: a verified instance is attached to; a shell that won the first
    /// preferred port owns it; one that lost it waits (bounded) for the
    /// instance that announced itself, then owns what it bound.
    #[test]
    fn port_decision_table() {
        use super::{PortDecision, decide_port};
        let first = 8477;
        // Discovery verified another instance: attach, whatever we bound.
        assert_eq!(
            decide_port(8478, first, Some(8477), Some(8477), true),
            PortDecision::Attach(8477)
        );
        assert_eq!(
            decide_port(8477, first, Some(8479), None, false),
            PortDecision::Attach(8479)
        );
        // Our own port never counts as "another instance".
        assert_eq!(
            decide_port(8478, first, Some(8478), Some(8478), true),
            PortDecision::Own
        );
        // Won the first preferred port: nobody can be ahead of us.
        assert_eq!(
            decide_port(8477, first, None, Some(8477), true),
            PortDecision::Own
        );
        assert_eq!(
            decide_port(8477, first, None, Some(8480), true),
            PortDecision::Own
        );
        // Lost it to an instance that announced itself but does not answer
        // yet (double launch): look again while the grace lasts...
        assert_eq!(
            decide_port(8478, first, None, Some(8477), true),
            PortDecision::Recheck
        );
        // ...then own what we bound (a stale file / a foreign occupant).
        assert_eq!(
            decide_port(8478, first, None, Some(8477), false),
            PortDecision::Own
        );
        // Lost it to something that never announced itself: own at once.
        assert_eq!(
            decide_port(8478, first, None, None, true),
            PortDecision::Own
        );
    }

    /// R5: the listener `claim_port` binds is the one served — a port held
    /// by a listener cannot be taken by a second shell's bind.
    #[test]
    fn a_held_listener_keeps_its_port_from_a_second_bind() {
        let held = crate::serve::bind_loopback(0).expect("bind");
        let port = held.local_addr().unwrap().port();
        assert!(crate::serve::bind_loopback(port).is_err());
        drop(held);
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
    fn parses_per_port_ft_nonce_from_set_cookie() {
        let response = "HTTP/1.1 200 OK\r\nSet-Cookie: ft_nonce_8478=n8478; HttpOnly; SameSite=Strict; Path=/\r\n\r\n";
        assert_eq!(nonce_from_response(response).as_deref(), Some("n8478"));
        assert_eq!(
            nonce_from_response("HTTP/1.1 200 OK\r\nSet-Cookie: ft_nonce_x=bad\r\n\r\n"),
            None
        );
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
