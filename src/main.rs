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
    0
}

fn main() {
    // C8: a step child of the workstation dies with it (unix watch pipe).
    frametrace::serve::watch_parent_from_env();
    // P7: no modal loader/critical-error dialogs for this process or the
    // tools it spawns (inherited) — they would block unseen.
    frametrace::util::suppress_os_error_dialogs();
    std::process::exit(frametrace::run_with_large_stack(cli_main));
}
