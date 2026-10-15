//! Shared by the integration-test crates (`mod common;`).
//!
//! R4 F9: every `frametrace` / `frametrace-app` process an integration test
//! spawns gets a throwaway HOME / USERPROFILE / XDG_CONFIG_HOME / APPDATA and
//! instance file, so a test can never read or write the examiner's real
//! `~/.config/frametrace`, `%APPDATA%\frametrace` or `~/.frametrace`.
//!
//! R5 U11: on top of that, every `FRAMETRACE_*` variable of the test
//! runner's own environment is removed from the child first (an examiner's
//! `FRAMETRACE_AUDIT_KEY`, `FRAMETRACE_TOKEN`, `FRAMETRACE_UI`, … used to
//! leak into the IT processes); a test sets the ones it needs explicitly,
//! after `.isolated()`.
//!
//! ```ignore
//! use common::IsolatedCommand;
//! Command::new(exe).args(args).isolated()
//! ```
#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// The inherited variables a test child must not see: every `FRAMETRACE_*`
/// name among `names` (the runner's environment).
pub fn inherited_frametrace_vars<I, S>(names: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    names
        .into_iter()
        .filter(|name| {
            name.as_ref()
                .to_str()
                .is_some_and(|text| text.to_ascii_uppercase().starts_with("FRAMETRACE_"))
        })
        .map(|name| name.as_ref().to_os_string())
        .collect()
}

/// `Command` extension: scrub the inherited `FRAMETRACE_*` variables, then
/// apply [`isolated_env`]. Later `.env(..)` calls still win.
pub trait IsolatedCommand {
    fn isolated(&mut self) -> &mut Self;
}

impl IsolatedCommand for Command {
    fn isolated(&mut self) -> &mut Self {
        for name in inherited_frametrace_vars(std::env::vars_os().map(|(name, _)| name)) {
            self.env_remove(name);
        }
        self.envs(isolated_env())
    }
}

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
