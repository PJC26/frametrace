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

/// R2: every test in a crate that spawns tracked trees or touches the
/// global kill list holds this guard, so `kill_all_tracked` in one test can
/// never reap another test's children and the list assertions see only
/// their own tree. This file is also compiled straight into
/// `tests/windows_job_tree.rs`; that crate gets its own copy of the guard,
/// shared by its tests and by the tests below.
#[cfg(test)]
pub(crate) fn tree_test_serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) struct TrackedChild {
    pub(crate) child: Child,
    /// unix: process-group id (== child pid); windows: Job object HANDLE.
    /// 0 = not tracked (or, on unix, no longer safe to signal).
    key: usize,
    /// Whether `key` is still in [`LIVE`] (the shutdown kill list).
    listed: bool,
    /// Why tree tracking could not be set up (P6), `None` when tracked.
    pub(crate) tracking_error: Option<String>,
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

    pub(super) fn attach(child: &std::process::Child) -> Result<usize, String> {
        // process_group(0) makes the child its own group leader: pgid == pid.
        Ok(child.id() as usize)
    }

    unsafe extern "C" {
        fn waitid(idtype: i32, id: u32, infop: *mut u8, options: i32) -> i32;
    }

    #[cfg(target_os = "linux")]
    const WAITID_FLAGS: Option<(i32, i32)> = Some((1, 0x4 | 0x1 | 0x0100_0000));
    #[cfg(target_os = "macos")]
    const WAITID_FLAGS: Option<(i32, i32)> = Some((1, 0x04 | 0x01 | 0x20));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    const WAITID_FLAGS: Option<(i32, i32)> = None;

    /// P8: `true` once `pid` (our child) has exited but is NOT yet reaped
    /// — `waitid(P_PID, WEXITED | WNOHANG | WNOWAIT)` leaves the zombie in
    /// place, so its pid (== the group id) cannot be recycled until the
    /// following `try_wait` reaps it. The group is untracked in between,
    /// which closes the window where a shutdown `killpg` could hit a
    /// recycled group. `false` while it runs (or where unsupported).
    pub(super) fn exited_unreaped(pid: u32) -> bool {
        let Some((p_pid, options)) = WAITID_FLAGS else {
            return false;
        };
        // siginfo_t is 128 bytes on Linux and macOS; its first word
        // (`si_signo`, `si_errno`) stays 0 when WNOHANG found no change.
        let mut info = [0u64; 32];
        // SAFETY: waitid writes at most one siginfo_t into the buffer.
        let rc = unsafe { waitid(p_pid, pid, info.as_mut_ptr().cast(), options) };
        rc == 0 && info[0] != 0
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

    /// SIGINT/SIGTERM/SIGHUP -> the watcher thread kills every tracked
    /// process group and exits with the conventional 128+signal status.
    /// Step children run in their own process groups (`process_group(0)`),
    /// so a terminal Ctrl-C (delivered to the foreground group only) does
    /// not reach them by itself — `kill_all_tracked` is what stops them.
    /// This is the unix path; Windows uses a console-control handler plus
    /// job objects (see the `windows` module).
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
                        super::on_fatal_signal();
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
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };
    use windows::core::PCWSTR;

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    /// Lets a child spawned with `CREATE_BREAKAWAY_FROM_JOB` (the browser /
    /// explorer launch) leave the server job; pipeline steps never set
    /// that flag, so they still die with the server.
    const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0000_0800;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtResumeProcess(process: *mut std::ffi::c_void) -> i32;
    }

    /// Creates a KILL_ON_JOB_CLOSE job object. `allow_breakaway` adds
    /// BREAKAWAY_OK so a child spawned with CREATE_BREAKAWAY_FROM_JOB can
    /// opt out (the server job only — a browser launched from the
    /// workstation must survive the workstation closing).
    unsafe fn new_kill_job(allow_breakaway: bool) -> Option<HANDLE> {
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if allow_breakaway {
                flags |= JOB_OBJECT_LIMIT(JOB_OBJECT_LIMIT_BREAKAWAY_OK);
            }
            info.BasicLimitInformation.LimitFlags = flags;
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
    /// spawn grandchildren that would escape the job). `creation_flags`
    /// replaces the whole flag word, so CREATE_NO_WINDOW is set in the
    /// same call (through the shared helper) rather than lost.
    pub(super) fn prepare(command: &mut std::process::Command) {
        crate::util::hide_console_with_flags(command, CREATE_SUSPENDED);
    }

    pub(super) fn attach(child: &std::process::Child) -> Result<usize, String> {
        // SAFETY: plain Win32 calls on handles we own; the job handle is
        // closed in `release` (which, with KILL_ON_JOB_CLOSE, also reaps any
        // survivors of the tree).
        let result = unsafe {
            match new_kill_job(false) {
                Some(job) => {
                    let process = HANDLE(child.as_raw_handle());
                    match AssignProcessToJobObject(job, process) {
                        Ok(()) => Ok(job.0 as usize),
                        Err(err) => {
                            let _ = CloseHandle(job);
                            Err(format!("job assignment failed ({err})"))
                        }
                    }
                }
                None => Err(format!(
                    "could not create job object ({})",
                    std::io::Error::last_os_error()
                )),
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
    /// it). Nested jobs (Windows 8+) keep the per-step jobs working. The
    /// job allows breakaway so `open_in_browser`/`open_in_explorer` can
    /// launch Edge/Explorer outside it (CREATE_BREAKAWAY_FROM_JOB);
    /// pipeline steps never request breakaway and stay inside.
    pub(super) fn init_process_guard() {
        // SAFETY: Win32 calls on a handle we own and leak on purpose.
        unsafe {
            let Some(job) = new_kill_job(true) else {
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
        super::on_fatal_signal();
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
/// failure) the child is still returned, `kill_tree` falls back to
/// killing the direct child only, and [`TrackedChild::tracking_error`]
/// says why — the caller surfaces it as a job warning (P6) instead of a
/// console line the windowed app never shows.
pub(crate) fn spawn_tracked(command: &mut Command) -> std::io::Result<TrackedChild> {
    sys::prepare(command);
    let child = command.spawn()?;
    let (key, tracking_error) = match sys::attach(&child) {
        Ok(key) => (key, None),
        Err(err) => (0, Some(err)),
    };
    if key != 0 {
        live().push(key);
    }
    Ok(TrackedChild {
        child,
        key,
        listed: key != 0,
        tracking_error,
    })
}

/// Job-warning text for a child whose process tree could not be tracked.
pub(crate) fn tracking_warning(program: &str, err: &str) -> String {
    format!(
        "하위 프로세스 묶음(Job object)을 만들지 못했습니다 ({program}: {err}) — 중단·종료 시 이 단계가 띄운 ffmpeg 등 하위 프로세스가 남을 수 있습니다. 작업 관리자에서 확인하십시오."
    )
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

    /// `Child::try_wait` that first drops the tree from the shutdown list
    /// once the child has exited (P8). On unix the exit is detected
    /// WITHOUT reaping (the zombie keeps its pid, i.e. the group id,
    /// reserved), the group is untracked, and only then is it reaped — so
    /// `kill_all_tracked` can no longer `killpg` a recycled group id.
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        #[cfg(unix)]
        if self.key != 0 && sys::exited_unreaped(self.child.id()) {
            self.unlist();
            // The group id is only safe to signal while the zombie exists.
            self.key = 0;
        }
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.unlist();
            #[cfg(unix)]
            {
                self.key = 0;
            }
        }
        Ok(status)
    }

    fn unlist(&mut self) {
        if self.listed {
            let key = self.key;
            live().retain(|entry| *entry != key);
            self.listed = false;
        }
    }
}

impl Drop for TrackedChild {
    fn drop(&mut self) {
        self.unlist();
        if self.key != 0 {
            sys::release(self.key);
        }
    }
}

/// Kills every still-tracked process tree (server shutdown path).
pub(crate) fn kill_all_tracked() {
    let keys: Vec<usize> = live().clone();
    kill_keys(&keys);
}

/// Whether `key` is in the shutdown list (tests).
#[cfg(test)]
pub(crate) fn is_listed(key: usize) -> bool {
    live().contains(&key)
}

/// Snapshot of the shutdown list (tests, under [`tree_test_serial`]; used
/// by `tests/windows_job_tree.rs`).
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn listed_keys() -> Vec<usize> {
    live().clone()
}

/// Fatal-signal / console-control path: reap every tracked tree, then
/// drop the instance file while it still names this process (P9) — the
/// process exits right after, skipping `serve::run`'s own cleanup, and a
/// stale file would point the next launch (and its token) at a dead port.
pub(crate) fn on_fatal_signal() {
    fatal_signal_cleanup(&kill_all_tracked, &|| {
        super::discovery::remove_instance_file_if_owned()
    });
}

/// Order of [`on_fatal_signal`], with the effects injected (tests).
pub(crate) fn fatal_signal_cleanup(kill: &dyn Fn(), remove_instance: &dyn Fn() -> bool) -> bool {
    kill();
    remove_instance()
}

fn kill_keys(keys: &[usize]) {
    for key in keys {
        sys::kill(*key);
    }
}

/// Process-wide setup for the server: signal (unix) / console-control
/// (Windows) handlers that reap tracked trees, and on Windows a
/// server-wide KILL_ON_JOB_CLOSE job so no descendant outlives the server.
/// Unix step children sit in their own process groups, so the terminal's
/// Ctrl-C does not reach them directly. On Windows a job object does NOT
/// shield children from console control events; step children are instead
/// created with CREATE_NO_WINDOW (no console attached, see
/// `util::hide_console`), so Ctrl-C never reaches them either — the handler
/// and the jobs are what stop them. Idempotent; failures are logged, not
/// fatal.
pub(crate) fn install_process_guard() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        sys::init_process_guard();
        if let Err(err) = sys::install_signal_handlers() {
            eprintln!("frametrace: could not install shutdown signal handler: {err}");
        }
    });
}

#[cfg(test)]
mod tests_try_wait {
    use super::*;

    /// P8: once `try_wait` sees the child exit, its tree is off the
    /// shutdown kill list — on unix before the zombie is reaped, so a
    /// recycled group id can never be signalled.
    #[test]
    fn exited_child_leaves_the_shutdown_list() {
        let _serial = tree_test_serial();
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/C", "exit 0"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            command
        };
        let mut tracked = spawn_tracked(&mut command).expect("spawn");
        assert!(
            tracked.tracking_error.is_none(),
            "{:?}",
            tracked.tracking_error
        );
        let key = tracked.key;
        assert_ne!(key, 0);
        assert!(is_listed(key), "a running tree is on the kill list");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let status = loop {
            if let Some(status) = tracked.try_wait().expect("try_wait") {
                break status;
            }
            assert!(std::time::Instant::now() < deadline, "child never exited");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        // Exit status is not asserted: an in-process test server shutting
        // down elsewhere in this crate may reap the tree first — what this
        // test pins is the list membership. (Never call kill_all_tracked
        // here.) Shutdown reaping can no longer reach this key.
        let _ = status;
        assert!(!is_listed(key), "an exited tree must leave the kill list");
        drop(tracked);
        assert!(!is_listed(key));
    }
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
        let _serial = tree_test_serial();
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
