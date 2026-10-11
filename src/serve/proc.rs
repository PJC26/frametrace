//! Process-tree-aware child spawning for pipeline steps.
//!
//! `frametrace <step>` children in turn spawn ffmpeg/ffprobe/icat. Killing
//! only the direct child on cancel orphans those grandchildren, which keep
//! burning CPU/IO on the examiner's machine and keep evidence files open.
//! `spawn_tracked` puts the child in its own process group (unix) or Job
//! object with KILL_ON_JOB_CLOSE (windows) so one call tears down the tree.

use std::process::{Child, Command};
use std::sync::Mutex;

/// Group/job handles of live tracked children, so server shutdown can reap
/// stragglers even when no `run_step` loop is watching them any more.
static LIVE: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn live() -> std::sync::MutexGuard<'static, Vec<usize>> {
    LIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) struct TrackedChild {
    pub(crate) child: Child,
    /// unix: process-group id (== child pid); windows: Job object HANDLE.
    key: usize,
}

#[cfg(unix)]
mod sys {
    unsafe extern "C" {
        fn killpg(pgrp: i32, sig: i32) -> i32;
    }
    const SIGKILL: i32 = 9;

    pub(super) fn prepare(command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    pub(super) fn attach(child: &std::process::Child) -> Option<usize> {
        // process_group(0) makes the child its own group leader: pgid == pid.
        Some(child.id() as usize)
    }

    pub(super) fn kill(key: usize) {
        // SAFETY: killpg is a plain syscall wrapper taking integers.
        unsafe {
            killpg(key as i32, SIGKILL);
        }
    }

    pub(super) fn release(_key: usize) {}

    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
        fn pipe(fds: *mut i32) -> i32;
        fn write(fd: i32, buf: *const u8, count: usize) -> isize;
        fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    }
    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    static PIPE_WRITE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

    extern "C" fn on_signal(signum: i32) {
        // Async-signal-safe: a single write(2) to the self-pipe; the
        // watcher thread does the real work.
        let fd = PIPE_WRITE.load(std::sync::atomic::Ordering::Relaxed);
        if fd >= 0 {
            let byte = signum as u8;
            // SAFETY: write(2) on a valid pipe fd with a 1-byte buffer.
            unsafe {
                write(fd, &byte, 1);
            }
        }
    }

    /// SIGINT/SIGTERM/SIGHUP -> kill every tracked process group, exit with
    /// the conventional 128+signal status. Step processes live in their own
    /// groups, so the terminal's Ctrl-C no longer reaches them directly.
    pub(super) fn install_signal_handlers() -> Result<(), String> {
        let mut fds = [0i32; 2];
        // SAFETY: pipe(2) fills two ints.
        if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!("pipe: {}", std::io::Error::last_os_error()));
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        PIPE_WRITE.store(write_fd, std::sync::atomic::Ordering::Relaxed);
        std::thread::Builder::new()
            .name("frametrace-signal".into())
            .spawn(move || {
                let mut byte = 0u8;
                loop {
                    // SAFETY: read(2) into a 1-byte buffer on our pipe.
                    let n = unsafe { read(read_fd, &mut byte, 1) };
                    if n == 1 {
                        super::kill_all_tracked();
                        std::process::exit(128 + i32::from(byte));
                    }
                    if n < 0
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                        continue;
                    }
                    return;
                }
            })
            .map_err(|err| format!("signal thread: {err}"))?;
        for signum in [SIGINT, SIGTERM, SIGHUP] {
            // SAFETY: installing an async-signal-safe handler.
            unsafe {
                signal(signum, on_signal as extern "C" fn(i32) as usize);
            }
        }
        Ok(())
    }

    pub(super) fn init_process_guard() {}
}

#[cfg(windows)]
mod sys {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows::core::PCWSTR;

    const CREATE_SUSPENDED: u32 = 0x0000_0004;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtResumeProcess(process: *mut std::ffi::c_void) -> i32;
    }

    /// Creates a KILL_ON_JOB_CLOSE job object.
    unsafe fn new_kill_job() -> Option<HANDLE> {
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .is_ok();
            if !configured {
                let _ = CloseHandle(job);
                return None;
            }
            Some(job)
        }
    }

    /// Spawned suspended so the child is in its job before it can run (and
    /// spawn grandchildren that would escape the job).
    pub(super) fn prepare(command: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_SUSPENDED);
    }

    pub(super) fn attach(child: &std::process::Child) -> Option<usize> {
        // SAFETY: plain Win32 calls on handles we own; the job handle is
        // closed in `release` (which, with KILL_ON_JOB_CLOSE, also reaps any
        // survivors of the tree).
        let result = unsafe {
            match new_kill_job() {
                Some(job) => {
                    let process = HANDLE(child.as_raw_handle());
                    match AssignProcessToJobObject(job, process) {
                        Ok(()) => Some(job.0 as usize),
                        Err(err) => {
                            eprintln!(
                                "frametrace: job assignment failed ({err}); cancel will only kill the direct child"
                            );
                            let _ = CloseHandle(job);
                            None
                        }
                    }
                }
                None => {
                    eprintln!(
                        "frametrace: could not create job object; cancel will only kill the direct child"
                    );
                    None
                }
            }
        };
        // Always resume, even when tracking failed: a suspended child hangs.
        // SAFETY: the std Child process handle has full access rights.
        unsafe {
            NtResumeProcess(child.as_raw_handle());
        }
        result
    }

    static SERVER_JOB: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Puts the server process itself in a KILL_ON_JOB_CLOSE job. Children
    /// inherit it, so every descendant dies when the server exits for any
    /// reason (the handle is intentionally never closed; process exit closes
    /// it). Nested jobs (Windows 8+) keep the per-step jobs working.
    pub(super) fn init_process_guard() {
        // SAFETY: Win32 calls on a handle we own and leak on purpose.
        unsafe {
            let Some(job) = new_kill_job() else {
                eprintln!("frametrace: could not create server job object");
                return;
            };
            let current = windows::Win32::System::Threading::GetCurrentProcess();
            match AssignProcessToJobObject(job, current) {
                Ok(()) => SERVER_JOB.store(job.0 as usize, std::sync::atomic::Ordering::SeqCst),
                Err(err) => {
                    eprintln!("frametrace: server job assignment failed: {err}");
                    let _ = CloseHandle(job);
                }
            }
        }
    }

    unsafe extern "system" fn on_ctrl(_ctrl_type: u32) -> windows::core::BOOL {
        super::kill_all_tracked();
        // Not handled: the default handler still terminates the process.
        windows::core::BOOL(0)
    }

    pub(super) fn install_signal_handlers() -> Result<(), String> {
        // SAFETY: registers a process-lifetime handler routine.
        unsafe {
            windows::Win32::System::Console::SetConsoleCtrlHandler(Some(on_ctrl), true)
                .map_err(|err| err.to_string())
        }
    }

    pub(super) fn kill(key: usize) {
        // SAFETY: `key` is a live job handle owned by a TrackedChild.
        unsafe {
            let _ = TerminateJobObject(HANDLE(key as *mut std::ffi::c_void), 1);
        }
    }

    pub(super) fn release(key: usize) {
        // SAFETY: closes the job handle created in `attach`.
        unsafe {
            let _ = CloseHandle(HANDLE(key as *mut std::ffi::c_void));
        }
    }
}

/// Spawns `command` so that [`TrackedChild::kill_tree`] reaches every
/// descendant. If tree tracking cannot be set up (Windows job assignment
/// failure) the child is still returned and `kill_tree` falls back to
/// killing the direct child only.
pub(crate) fn spawn_tracked(command: &mut Command) -> std::io::Result<TrackedChild> {
    sys::prepare(command);
    let child = command.spawn()?;
    let key = sys::attach(&child).unwrap_or(0);
    if key != 0 {
        live().push(key);
    }
    Ok(TrackedChild { child, key })
}

impl TrackedChild {
    /// Kills the child and all its descendants. Call only while the child
    /// has not yet been reaped (a reaped leader's pid could be recycled).
    pub(crate) fn kill_tree(&mut self) {
        if self.key != 0 {
            sys::kill(self.key);
        }
        let _ = self.child.kill();
    }
}

impl Drop for TrackedChild {
    fn drop(&mut self) {
        if self.key != 0 {
            live().retain(|entry| *entry != self.key);
            sys::release(self.key);
        }
    }
}

/// Kills every still-tracked process tree (server shutdown path).
pub(crate) fn kill_all_tracked() {
    let keys: Vec<usize> = live().clone();
    kill_keys(&keys);
}

fn kill_keys(keys: &[usize]) {
    for key in keys {
        sys::kill(*key);
    }
}

/// Process-wide setup for the server: signal / console-control handlers
/// that reap tracked trees (step processes are in their own groups/jobs so
/// the terminal's Ctrl-C does not reach them), and on Windows a server-wide
/// job so no descendant outlives the server. Idempotent; failures are
/// logged, not fatal.
pub(crate) fn install_process_guard() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        sys::init_process_guard();
        if let Err(err) = sys::install_signal_handlers() {
            eprintln!("frametrace: could not install shutdown signal handler: {err}");
        }
    });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    fn alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn kill_keys_reaps_grandchild() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(Stdio::piped());
        let mut tracked = spawn_tracked(&mut command).expect("spawn");
        let mut line = String::new();
        BufReader::new(tracked.child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let pid = line.trim().to_string();
        assert!(alive(&pid), "grandchild should be running");
        kill_keys(&[tracked.key]);
        let _ = tracked.child.wait();
        for _ in 0..50 {
            if !alive(&pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("grandchild {pid} survived kill_keys");
    }
}
