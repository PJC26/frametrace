//! Shared fake-CLI helpers for the server tests (R4 F3).
//!
//! Included into each test file with `#[path = "tests_fake_tools.rs"]`, so
//! no `mod` line is needed in `mod.rs` (the module is compiled once per
//! includer; hence the blanket `dead_code` allowance).
//!
//! A fake tool is written twice: a `/bin/sh` body for unix and a batch body
//! for Windows (`.cmd`). Windows rules baked into the batch bodies:
//! - an infinite wait never busy-spins (R5 U10: a bare `goto spin` loop
//!   pinned a CPU core per fake tool and starved the runner): it sleeps in
//!   `ping -n 2` steps ([`CMD_SPIN`], [`cmd_wait_for_file`]). Each `ping`
//!   lives about one second, so one left behind by a killed `cmd.exe`
//!   holds the inherited stdout/stderr pipes for a second at most;
//! - cmd splits arguments on `=` and `,`, so values are located by flag with
//!   a `shift` loop (see [`CMD_FIND_OUTPUT`]) and the raw line is `%*`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Writes `fake-frametrace.sh` (0755) on unix or `fake-frametrace.cmd` on
/// Windows into `dir` and returns its path. `unix_body` runs under
/// `/bin/sh`; `cmd_body` runs under `cmd.exe` after `@echo off`.
pub fn write_fake(dir: &Path, unix_body: &str, cmd_body: &str) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = cmd_body;
        let path = dir.join("fake-frametrace.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{unix_body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    #[cfg(not(unix))]
    {
        let _ = unix_body;
        let path = dir.join("fake-frametrace.cmd");
        let text = format!("@echo off\n{cmd_body}\n")
            .replace("\r\n", "\n")
            .replace('\n', "\r\n");
        std::fs::write(&path, text).unwrap();
        path
    }
}

/// Batch prologue: sets `out` to the value after `--output` (arguments are
/// consumed; use `%~dp0` for the script dir afterwards).
pub const CMD_FIND_OUTPUT: &str = "set out=\n:findout\nif \"%~1\"==\"\" goto gotout\nif \"%~1\"==\"--output\" set out=%~2\nshift\ngoto findout\n:gotout";

/// `/bin/sh` equivalent of [`CMD_FIND_OUTPUT`].
pub const SH_FIND_OUTPUT: &str = "out=\"\"; prev=\"\"\nfor a in \"$@\"; do if [ \"$prev\" = \"--output\" ]; then out=\"$a\"; fi; prev=\"$a\"; done";

/// Batch snippet: wait forever, one ~1 s `ping` step at a time (never a
/// busy spin, R5 U10).
pub const CMD_SPIN: &str = ":spin\nping -n 2 127.0.0.1 >nul 2>&1\ngoto spin";

/// Batch snippet that waits until `gate` exists, polling every ~1 s with a
/// `ping` step (never a busy spin, R5 U10).
pub fn cmd_wait_for_file(gate: &Path) -> String {
    format!(
        ":waitgate\nif exist \"{}\" goto gateopen\nping -n 2 127.0.0.1 >nul 2>&1\ngoto waitgate\n:gateopen",
        gate.display()
    )
}

/// Batch snippet: sleep roughly `seconds` using `ping` (finite, so a stray
/// grandchild is harmless).
pub fn cmd_sleep(seconds: u32) -> String {
    format!("ping -n {} 127.0.0.1 >nul", seconds + 1)
}

/// Batch snippet: start a long-lived grandchild (`ping`), record its pid in
/// `pidfile`, then wait on it — the process tree a cancel must tear down.
pub fn cmd_grandchild_sleeper(pidfile: &Path) -> String {
    format!(
        "powershell -NoProfile -Command \"$p = Start-Process -FilePath ping -ArgumentList '-n','300','127.0.0.1' -PassThru -WindowStyle Hidden; Set-Content -Path '{}' -Value $p.Id -Encoding ascii; Wait-Process -Id $p.Id\"",
        pidfile.display()
    )
}

/// Whether process `pid` still exists.
pub fn pid_alive(pid: &str) -> bool {
    #[cfg(unix)]
    {
        // `kill -0` succeeds for a zombie; `ps` marks it `Z`.
        let out = Command::new("ps")
            .args(["-p", pid, "-o", "stat="])
            .stderr(Stdio::null())
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
        !stat.is_empty() && !stat.starts_with('Z')
    }
    #[cfg(not(unix))]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .stderr(Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    }
}

/// Force-kills `pid` (cleanup when an assertion is about to fail).
pub fn kill_pid(pid: &str) {
    #[cfg(unix)]
    let _ = Command::new("kill").args(["-9", pid]).status();
    #[cfg(not(unix))]
    let _ = Command::new("taskkill").args(["/F", "/PID", pid]).output();
}
