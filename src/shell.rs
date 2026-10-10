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
//! cleanly. When the shell attaches to an already-running workstation it
//! does NOT shut it down on close — that mirrors the browser semantics
//! where closing a tab leaves the server alive.

use crate::serve::{self, ServeOptions};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
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

/// Runs the workstation in a native WebView2 window. Returns when the
/// window (or the fallback browser session) ends.
pub fn run_hosted() -> Result<(), String> {
    // An already-running workstation wins: attach a window to it instead
    // of stacking a second server. We don't own it, so closing the window
    // leaves it running — same contract as the browser flow.
    let (url, owned_port) = if let Some(port) = serve::find_running_server() {
        (workstation_url(port), None)
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
        // Server-exit watcher: if the server ends while the WebView2
        // window is still up (UI 종료 button, /api/shutdown, a fatal
        // error), close the frame rather than leaving a dead page open.
        let watcher = std::thread::spawn(move || {
            let _ = server.join();
            let thread_id = FRAME_THREAD.load(Ordering::SeqCst);
            if thread_id != 0 {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                        thread_id,
                        windows::Win32::UI::WindowsAndMessaging::WM_APP,
                        WPARAM::default(),
                        LPARAM::default(),
                    );
                }
            }
        });
        (workstation_url(port), Some((port, watcher)))
    };

    let hosted = match host_window(&url) {
        Ok(()) => true,
        Err(err) => {
            eprintln!("webview2 shell unavailable, falling back to browser: {err}");
            serve::open_in_browser(&url);
            false
        }
    };

    if let Some((port, watcher)) = owned_port {
        if hosted {
            // Window closed by the examiner: stop our server cleanly.
            request_shutdown(port);
        }
        // Whether we hosted or fell back to a browser, the process keeps
        // running until the server exits (UI 종료 button, /api/shutdown,
        // or the shutdown request above). The watcher also returns once
        // the server thread has joined.
        let _ = watcher.join();
    }
    Ok(())
}

fn reserve_port() -> Result<u16, String> {
    // Bind-release-bind is a tiny TOCTOU window, but serve::run returns a
    // bind error rather than grabbing a random port, and the caller turns
    // that into the browser fallback — acceptable and self-healing.
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|err| format!("failed to reserve a loopback port: {err}"))?;
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    if port == 0 {
        return Err("reserved port resolved to 0".to_string());
    }
    Ok(port)
}

fn workstation_url(port: u16) -> String {
    match std::env::var("FRAMETRACE_TOKEN") {
        Ok(token) if !token.trim().is_empty() => {
            format!("http://127.0.0.1:{port}/?token={token}")
        }
        _ => format!("http://127.0.0.1:{port}/"),
    }
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

/// POST /api/shutdown over a raw socket — no HTTP client dependency.
/// Reads briefly so the server thread can flush, then returns; the
/// accept loop exits on its own poll even if the read races.
fn request_shutdown(port: u16) {
    let query = match std::env::var("FRAMETRACE_TOKEN") {
        Ok(token) if !token.trim().is_empty() => format!("?token={token}"),
        _ => String::new(),
    };
    // No configured token means the mutating-request gate wants the
    // instance nonce — the shell shares the user's trust domain, so the
    // user-private instance file (or the ft_nonce cookie GET / plants)
    // supplies it.
    let nonce_header = instance_nonce(port)
        .map(|nonce| format!("X-FrameTrace-Nonce: {nonce}\r\n"))
        .unwrap_or_default();
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request =
        format!("POST /api/shutdown{query} HTTP/1.0\r\n{nonce_header}Content-Length: 0\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return;
    }
    let mut buf = [0u8; 512];
    let _ = stream.read(&mut buf);
}

/// Per-process instance nonce for the nonce gate: the instance file is
/// authoritative (user-private). The fallback (servers started before
/// instance files existed, or an unreadable file) is the `ft_nonce`
/// cookie that `GET /` plants — `/api/status`'s "instance" field is only
/// a SHA-256 fingerprint of the nonce and would be refused with 401.
fn instance_nonce(port: u16) -> Option<String> {
    if let Some((instance_port, instance)) = crate::serve::read_instance_file()
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

fn host_window(url: &str) -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|err| format!("CoInitializeEx failed: {err}"))?;
        SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE)
            .map_err(|err| format!("SetProcessDpiAwareness failed: {err}"))?;

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
        let result = host_webview(hwnd, url);
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

unsafe fn host_webview(hwnd: HWND, url: &str) -> Result<(), String> {
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

        let url_wide = HSTRING::from(url);
        webview
            .Navigate(PCWSTR::from_raw(url_wide.as_ptr()))
            .map_err(|err| format!("WebView2 navigate to {url} failed: {err}"))?;

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
                _ if msg.hwnd.0.is_null() && msg.message == WM_APP => break,
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
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
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
    use super::nonce_from_response;

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
