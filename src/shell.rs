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
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::mpsc;
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

/// UI thread id shared with the server-watcher thread: if the embedded
/// server dies while the window is still open (external shutdown, crash),
/// the watcher posts a thread message that ends the message pump. A
/// thread message avoids HWND reuse hazards — a posted WM_CLOSE could
/// otherwise land on an unrelated window that recycled our handle.
static FRAME_THREAD: AtomicU32 = AtomicU32::new(0);

/// Port of the server this process owns (0 when attached to a foreign
/// workstation). The WM_CLOSE handler consults it: only an owned server is
/// shut down on close, so only then does a running job need the
/// cancel-and-quit question.
static OWNED_PORT: AtomicU16 = AtomicU16::new(0);

/// Set while a cancel-then-close is in flight so a second WM_CLOSE (the
/// examiner clicking × again) neither re-asks nor destroys the frame early.
static CLOSING: AtomicBool = AtomicBool::new(false);

/// Thread message (hwnd == null) posted by the server watcher: the server
/// is gone, end the pump.
const MSG_SERVER_GONE: u32 = WM_APP;
/// Thread message posted by the cancel-then-close worker: the job stopped
/// (or the wait ran out), destroy the frame now.
const MSG_CLOSE_NOW: u32 = WM_APP + 1;

/// How long WM_CLOSE waits for a cancelled job to stop before the frame
/// closes anyway (the shutdown path below has its own bound after that).
const CLOSE_CANCEL_WAIT: Duration = Duration::from_secs(10);
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
        let server = std::thread::Builder::new()
            .name("frametrace-serve".to_string())
            .spawn(move || {
                serve::run(ServeOptions {
                    case_dir: None,
                    port: Some(port),
                    open_browser: false,
                })
            })
            .map_err(|err| format!("failed to spawn workstation server thread: {err}"))?;
        wait_for_server(port, Duration::from_secs(15))?;
        OWNED_PORT.store(port, Ordering::SeqCst);
        // Server-exit watcher: if the server ends while the WebView2
        // window is still up (UI 종료 button, /api/shutdown, a fatal
        // error), close the frame rather than leaving a dead page open.
        // The channel lets the shutdown path wait for the server with a
        // bound instead of an unbounded `join`.
        let (server_done_tx, server_done_rx) = mpsc::channel::<()>();
        let watcher = std::thread::spawn(move || {
            let _ = server.join();
            let _ = server_done_tx.send(());
            post_thread_message(MSG_SERVER_GONE);
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
        Err(err) => {
            eprintln!("webview2 shell unavailable, falling back to browser: {err}");
            if let Ok(url) = &target {
                serve::open_in_browser(url);
            }
            false
        }
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
                    std::process::exit(1);
                }
            }
        }
        // Whether we hosted or fell back to a browser, the process keeps
        // running until the server exits (UI 종료 button, /api/shutdown,
        // or the shutdown request above). In the browser fallback the
        // examiner ends the session from the UI, so this wait is open.
        let _ = watcher.join();
    }
    Ok(())
}

fn post_thread_message(message: u32) {
    let thread_id = FRAME_THREAD.load(Ordering::SeqCst);
    if thread_id != 0 {
        // SAFETY: a thread message carrying no pointers.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                thread_id,
                message,
                WPARAM::default(),
                LPARAM::default(),
            );
        }
    }
}

fn reserve_port() -> Result<u16, String> {
    // Bind-release-bind is a tiny TOCTOU window, but serve::run returns a
    // bind error rather than grabbing a random port, and the caller turns
    // that into the browser fallback — acceptable and self-healing.
    let listener = serve::bind_loopback(0)
        .map_err(|err| format!("failed to reserve a loopback port: {err}"))?;
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    if port == 0 {
        return Err("reserved port resolved to 0".to_string());
    }
    Ok(port)
}

fn wait_for_server(port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "workstation server did not bind 127.0.0.1:{port} in time"
    ))
}

/// One request to our own server over a raw socket (no HTTP client
/// dependency). Returns the response body, `None` when the server could
/// not be reached or answered nothing. HTTP/1.0 so the server closes the
/// connection after the response; a read timeout bounds a stalled peer
/// and whatever arrived before it is still returned.
fn shell_request(port: u16, method: &str, path: &str) -> Option<String> {
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
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let request = format!(
        "{method} {path}{query} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n{nonce_header}Content-Length: 0\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut buffer = Vec::new();
    // A timeout after the response arrived is not a failure: keep what
    // was read.
    let _ = stream.read_to_end(&mut buffer);
    if buffer.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&buffer);
    Some(
        text.split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default(),
    )
}

/// POST /api/shutdown. `Ok(true)` accepted, `Ok(false)` refused because
/// a job runs, `Err` when the server did not answer.
fn request_shutdown(port: u16) -> Result<bool, ()> {
    let body = shell_request(port, "POST", "/api/shutdown").ok_or(())?;
    Ok(shutdown_accepted(&body))
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
/// already gone, and the caller's server-exit wait is bounded too).
fn shutdown_server(port: u16, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    let mut cancelled = false;
    loop {
        match request_shutdown(port) {
            Ok(true) | Err(()) => return true,
            Ok(false) => {}
        }
        if !cancelled {
            let _ = shell_request(port, "POST", "/api/cancel");
            cancelled = true;
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

/// Whether `/api/status` reports work the shutdown path would refuse to
/// interrupt: the busy pipeline/enrichment/finalize job or a running clip
/// export. Unparseable or missing status counts as idle.
pub(crate) fn status_has_running_work(status_json: Option<&str>) -> bool {
    let Some(value) =
        status_json.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
    else {
        return false;
    };
    let busy = value
        .get("busy")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let clips = value
        .get("clips_running")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    busy || clips > 0
}

/// Pure close decision: `status_json` is the `/api/status` body of the
/// owned server (`None` when it could not be fetched) and `confirm_cancel`
/// asks the examiner — it is only called when work is running.
pub(crate) fn decide_close(
    status_json: Option<&str>,
    confirm_cancel: &dyn Fn() -> bool,
) -> CloseAction {
    if !status_has_running_work(status_json) {
        return CloseAction::Close;
    }
    if confirm_cancel() {
        CloseAction::CancelThenClose
    } else {
        CloseAction::KeepOpen
    }
}

/// Modal yes/no in the examiner's language; UTF-16 so Korean renders.
fn confirm_cancel_dialog(hwnd: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_YESNO, MessageBoxW,
    };
    let text = HSTRING::from(
        "분석 작업이 진행 중입니다.\n작업을 중단하고 FrameTrace를 종료할까요?\n\n예: 중단 후 종료    아니요: 계속 작업",
    );
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
    std::thread::spawn(move || {
        let _ = shell_request(port, "POST", "/api/cancel");
        let deadline = Instant::now() + CLOSE_CANCEL_WAIT;
        while Instant::now() < deadline {
            let status = shell_request(port, "GET", "/api/status");
            if !status_has_running_work(status.as_deref()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        post_thread_message(MSG_CLOSE_NOW);
    });
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
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|err| format!("CoInitializeEx failed: {err}"))?;
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

        // The watcher needs our thread id so it can end the pump with a
        // thread message if the server dies while the window is open.
        FRAME_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
        let result = host_webview(hwnd, target);
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

        CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        let mut msg = MSG::default();
        loop {
            let result = GetMessageW(&mut msg, None, 0, 0).0;
            match result {
                -1 => break,
                0 => break,
                // Thread message from the server watcher: the server is
                // gone, so close the frame instead of serving a dead page.
                _ if msg.hwnd.0.is_null() && msg.message == MSG_SERVER_GONE => break,
                // The cancel-then-close worker finished its bounded wait:
                // destroy the frame (WM_DESTROY then ends the pump).
                _ if msg.hwnd.0.is_null() && msg.message == MSG_CLOSE_NOW => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }

        CONTROLLER.with(|slot| {
            if let Some(controller) = slot.borrow_mut().take() {
                let _ = controller.Close();
            }
        });
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
                CONTROLLER.with(|slot| {
                    if let Some(controller) = slot.borrow().as_ref() {
                        let _ = controller.SetBounds(rect);
                    }
                });
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            if CLOSING.load(Ordering::SeqCst) {
                // A cancel-then-close is already in flight.
                return LRESULT(0);
            }
            let port = OWNED_PORT.load(Ordering::SeqCst);
            // A foreign (attached) server is not shut down on close, so
            // its job is not interrupted either: no question to ask.
            let action = if port == 0 {
                CloseAction::Close
            } else {
                let status = shell_request(port, "GET", "/api/status");
                decide_close(status.as_deref(), &|| confirm_cancel_dialog(hwnd))
            };
            match action {
                CloseAction::KeepOpen => {}
                CloseAction::Close => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                CloseAction::CancelThenClose => {
                    CLOSING.store(true, Ordering::SeqCst);
                    cancel_then_close(port);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) },
    }
}

#[cfg(test)]
mod tests {
    use super::{CloseAction, decide_close, nonce_from_response, shutdown_accepted};

    /// Table: status body × examiner answer → action. The question is
    /// only asked while work runs (`asked` tracks that).
    #[test]
    fn close_decision_table() {
        let idle = r#"{"ok":true,"busy":false,"clips_running":0,"has_job":false}"#;
        let busy = r#"{"ok":true,"busy":true,"clips_running":0,"has_job":true}"#;
        let clips = r#"{"ok":true,"busy":false,"clips_running":2}"#;
        let garbage = "<html>not json</html>";
        let cases: &[(Option<&str>, bool, CloseAction, bool)] = &[
            // status, answer, expected action, question asked?
            (Some(idle), true, CloseAction::Close, false),
            (Some(idle), false, CloseAction::Close, false),
            (None, false, CloseAction::Close, false),
            (Some(garbage), false, CloseAction::Close, false),
            (Some(busy), true, CloseAction::CancelThenClose, true),
            (Some(busy), false, CloseAction::KeepOpen, true),
            (Some(clips), true, CloseAction::CancelThenClose, true),
            (Some(clips), false, CloseAction::KeepOpen, true),
        ];
        for (status, answer, expected, expect_asked) in cases {
            let asked = std::cell::Cell::new(false);
            let action = decide_close(*status, &|| {
                asked.set(true);
                *answer
            });
            assert_eq!(action, *expected, "status={status:?} answer={answer}");
            assert_eq!(asked.get(), *expect_asked, "status={status:?}");
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
