use frametrace::{cli, serve};

fn cli_main() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    if args.len() <= 1 {
        // Running the bare executable (e.g. double-click) launches the local
        // examiner workstation instead of printing CLI usage.
        if let Err(error) = serve::run(serve::ServeOptions {
            case_dir: None,
            port: None,
            open_browser: true,
        }) {
            eprintln!("error: {error}");
            return 1;
        }
        return 0;
    }
    if let Err(error) = cli::run(args) {
        eprintln!("error: {error}");
        return 1;
    }
    // R11-S8: `verify-package --strict` exits 3 for an incomplete finalize.
    cli::requested_exit_code()
}

fn main() {
    // R9-S7: keep the workstation's per-step token and drop it from the
    // environment before any thread or tool exists, so ffmpeg/ffprobe/
    // Sleuth Kit children never inherit it.
    // SAFETY: first statement of `main`; no other thread exists yet.
    unsafe { frametrace::util::adopt_step_warning_token() };
    // R10-I14: the same for the audit key variables (raw key, key file and
    // keyring paths) — kept for this process, gone from the environment.
    // SAFETY: still single-threaded process start.
    unsafe { frametrace::audit_key::adopt_key_env() };
    // C8: a step child of the workstation dies with it (unix watch pipe).
    frametrace::serve::watch_parent_from_env();
    // P7: no modal loader/critical-error dialogs for this process or the
    // tools it spawns (inherited) — they would block unseen.
    frametrace::util::suppress_os_error_dialogs();
    std::process::exit(frametrace::run_with_large_stack(cli_main));
}
