//! T1: Windows Job-object process-tree teardown (src/serve/proc.rs).
//!
//! `proc` is `pub(crate)` inside the library, so it is not reachable from an
//! integration test through the public API. The file is compiled straight
//! into this test crate with `#[path]`; its only crate-level dependency is
//! `crate::util::hide_console_with_flags`, re-provided below with the same
//! contract. These tests therefore exercise the real production code.
//!
//! Each test spawns a child (PowerShell) that spawns a grandchild (`ping`)
//! and reports its pid; teardown must reach the grandchild, which a plain
//! `Child::kill` would orphan.
//!
//! R2: the tests share one process-wide kill list (`kill_all_tracked`
//! reaps every listed tree), so every test here — and the `proc.rs` unit
//! tests compiled into this crate with it — holds `proc::tree_test_serial()`
//! for its whole run. `kill_all_tracked_reaps_grandchild_for_shutdown`
//! therefore only ever sees its own tree on the list.
#![cfg(windows)]
#![allow(dead_code)]

mod util {
    use std::process::Command;

    /// Same contract as `frametrace::util::hide_console_with_flags`.
    pub fn hide_console_with_flags(command: &mut Command, extra_flags: u32) -> &mut Command {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW | extra_flags);
        command
    }
}

/// `proc.rs` reaches `super::discovery` on its fatal-signal path (P9);
/// the test crate has no instance file, so the stub reports "not ours".
mod discovery {
    pub fn remove_instance_file_if_owned() -> bool {
        false
    }
}

#[path = "../src/serve/proc.rs"]
mod proc;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SCRIPT: &str = "$i = New-Object System.Diagnostics.ProcessStartInfo; \
    $i.FileName = 'ping'; $i.Arguments = '-n 120 127.0.0.1'; \
    $i.UseShellExecute = $false; $i.CreateNoWindow = $true; \
    $p = [System.Diagnostics.Process]::Start($i); \
    [Console]::Out.WriteLine($p.Id); [Console]::Out.Flush(); \
    Start-Sleep -Seconds 120";

fn alive(pid: &str) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .any(|word| word == pid)
}

fn spawn_tree() -> (proc::TrackedChild, String) {
    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut tracked = proc::spawn_tracked(&mut command).expect("spawn tracked child");
    let mut line = String::new();
    BufReader::new(tracked.child.stdout.take().unwrap())
        .read_line(&mut line)
        .expect("read grandchild pid");
    let pid = line.trim().to_string();
    assert!(
        !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()),
        "grandchild pid not reported: {line:?}"
    );
    assert!(alive(&pid), "grandchild {pid} should be running");
    (tracked, pid)
}

fn wait_dead(pid: &str, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if !alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Do not leak the 120s ping on failure.
    let _ = Command::new("taskkill").args(["/F", "/PID", pid]).status();
    panic!("grandchild {pid} survived {what}");
}

#[test]
fn kill_tree_reaps_grandchild() {
    let _serial = proc::tree_test_serial();
    let (mut tracked, pid) = spawn_tree();
    tracked.kill_tree();
    let _ = tracked.child.wait();
    wait_dead(&pid, "kill_tree");
}

#[test]
fn dropping_tracked_child_closes_job_and_reaps_grandchild() {
    // KILL_ON_JOB_CLOSE: releasing the handle must reap survivors even when
    // nobody calls kill_tree (e.g. the run_step loop panicked).
    let _serial = proc::tree_test_serial();
    let (tracked, pid) = spawn_tree();
    drop(tracked);
    wait_dead(&pid, "TrackedChild drop");
}

#[test]
fn kill_all_tracked_reaps_grandchild_for_shutdown() {
    let _serial = proc::tree_test_serial();
    let (mut tracked, pid) = spawn_tree();
    let key_listed = proc::listed_keys();
    assert_eq!(
        key_listed.len(),
        1,
        "only this test's tree may be on the kill list: {key_listed:?}"
    );
    proc::kill_all_tracked();
    let _ = tracked.child.wait();
    wait_dead(&pid, "kill_all_tracked");
}
