//! Windowed launcher binary. `frametrace.exe` stays a console CLI; this
//! variant hides the console and starts the examiner workstation directly.
#![windows_subsystem = "windows"]

fn app_main() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        // A single numeric argument selects the workstation port (used by
        // the E2E harness); any other arguments are CLI subcommands — the
        // workstation pipeline spawns `current_exe()` with subcommands
        // like init-case/scan-folder, and without this delegation every
        // pipeline step would launch ANOTHER workstation.
        let first = args[1].as_str();
        if let Ok(port) = first.parse::<u16>() {
            if let Err(error) = frametrace::serve::run(frametrace::serve::ServeOptions {
                case_dir: None,
                port: Some(port),
                open_browser: true,
            }) {
                eprintln!("error: {error}");
                return 1;
            }
            return 0;
        }
        if let Err(error) = frametrace::cli::run(args) {
            eprintln!("error: {error}");
            return 1;
        }
        return 0;
    }
    if let Err(error) = run_workstation() {
        // No console is attached in windowed mode; surface fatal startup
        // errors through a message box so the examiner is never left with a
        // silently missing window.
        use std::io::Write;
        let log_path = std::env::temp_dir().join("frametrace-app-error.log");
        if let Ok(mut log) = std::fs::File::create(&log_path) {
            let _ = writeln!(log, "{error}");
        }
        let message = format!(
            "FrameTrace failed to start: {error}\n(log: {})",
            log_path.display()
        );
        // FRAMETRACE_SHELL_SMOKE=1 (CI) has nobody to dismiss a dialog:
        // report on stderr and exit non-zero instead.
        if smoke_requested(std::env::var("FRAMETRACE_SHELL_SMOKE").ok().as_deref()) {
            eprintln!("{message}");
            return 1;
        }
        // MessageBoxW + UTF-16: the ANSI variant garbles Korean text
        // (paths, server errors) on a non-Korean code page.
        #[cfg(target_os = "windows")]
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                utf16z(&message).as_ptr(),
                utf16z("FrameTrace").as_ptr(),
                0x10, // MB_ICONERROR
            );
        }
        #[cfg(not(target_os = "windows"))]
        {
            eprintln!("{message}");
        }
        return 1;
    }
    0
}

/// `FRAMETRACE_SHELL_SMOKE=1` (exact contract shared with
/// `frametrace::shell`): no modal error box in the CI smoke run.
fn smoke_requested(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "1")
}

/// Null-terminated UTF-16 for Win32 `W` APIs. Interior NULs cannot occur
/// in a Rust `String` passed from our own error path, and a stray one
/// would only truncate the dialog text, never panic.
#[cfg(target_os = "windows")]
fn utf16z(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// No-argument launch. On Windows the native WebView2 shell hosts the
/// workstation window itself; everywhere else (and as the shell's own
/// fallback) the server runs and the URL opens in a browser.
fn run_workstation() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        frametrace::shell::run_hosted()
    }
    #[cfg(not(target_os = "windows"))]
    {
        frametrace::serve::run(frametrace::serve::ServeOptions {
            case_dir: None,
            port: None,
            open_browser: true,
        })
    }
}

fn main() {
    // R9-S7: keep the workstation's per-step token and drop it from the
    // environment before any thread or tool exists (see `frametrace` main).
    // SAFETY: first statement of `main`; no other thread exists yet.
    unsafe { frametrace::util::adopt_step_warning_token() };
    // R10-I14: the audit key variables too (see `frametrace` main).
    // SAFETY: still single-threaded process start.
    unsafe { frametrace::audit_key::adopt_key_env() };
    // C8: a step child of the workstation dies with it (unix watch pipe).
    frametrace::serve::watch_parent_from_env();
    // P7: no modal loader/critical-error dialogs for this process or the
    // tools it spawns (inherited) — in the windowed app nobody would see
    // them, and the probe or pipeline step would wait forever.
    frametrace::util::suppress_os_error_dialogs();
    std::process::exit(frametrace::run_with_large_stack(app_main));
}

#[cfg(target_os = "windows")]
#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(
        hwnd: *mut core::ffi::c_void,
        text: *const u16,
        caption: *const u16,
        utype: u32,
    ) -> i32;
}
