//! Shared by the integration-test crates (`mod common;`).
//!
//! R4 F9: every `frametrace` / `frametrace-app` process an integration test
//! spawns gets a throwaway HOME / USERPROFILE / XDG_CONFIG_HOME / APPDATA and
//! instance file, so a test can never read or write the examiner's real
//! `~/.config/frametrace`, `%APPDATA%\frametrace` or `~/.frametrace`.
//!
//! ```ignore
//! Command::new(exe).args(args).envs(common::isolated_env())
//! ```
#![allow(dead_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

/// One scratch config root per test process (removed at OS temp cleanup).
fn config_root() -> &'static PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("ft-it-config-{}", std::process::id()));
        std::fs::create_dir_all(root.join("home")).expect("create isolated home");
        std::fs::create_dir_all(root.join("cfg")).expect("create isolated config dir");
        root
    })
}

/// Environment overrides that isolate user configuration. Later explicit
/// `.env(..)` calls on the same `Command` still win.
pub fn isolated_env() -> Vec<(&'static str, OsString)> {
    let root = config_root();
    let home = root.join("home");
    let cfg = root.join("cfg");
    vec![
        ("HOME", home.clone().into_os_string()),
        ("USERPROFILE", home.into_os_string()),
        ("XDG_CONFIG_HOME", cfg.clone().into_os_string()),
        ("APPDATA", cfg.clone().into_os_string()),
        ("LOCALAPPDATA", cfg.clone().into_os_string()),
        (
            "FRAMETRACE_INSTANCE_FILE",
            cfg.join("server.json").into_os_string(),
        ),
    ]
}
