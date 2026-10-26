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

    #[cfg(target_os = "linux")]
    unsafe extern "C" {
        fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
        fn getppid() -> i32;
    }
    #[cfg(target_os = "linux")]
    const PR_SET_PDEATHSIG: i32 = 1;

    pub(super) fn prepare(command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        // C8: the parent-watch pipe (both unix flavours): the child's
        // watcher thread kills the child's whole process group (itself and
        // its ffmpeg/icat grandchildren) once the server's write end
        // closes — i.e. when the server dies for ANY reason, SIGKILL
        // included, when no handler of ours can run.
        //
        // R5-S4: BOTH ends of the pipe are close-on-exec in this process,
        // so no other child (a browser started by xdg-open, an ffprobe
        // probe) ever inherits them. Only a step child gets the read end:
        // the `pre_exec` hook below clears FD_CLOEXEC on it in the forked
        // child, between fork and exec, so the flag change never touches
        // the parent's descriptor table (fd flags are per-process after
        // fork; only the open file description is shared).
        let watch_fd = parent_watch_read_fd();
        if let Some(fd) = watch_fd {
            command.env(super::PARENT_WATCH_ENV, fd.to_string());
        }
        #[cfg(target_os = "linux")]
        let parent = std::process::id();
        // SAFETY: only async-signal-safe syscalls run between fork and
        // exec (fcntl, prctl, getppid); no allocation (raw OS errors only).
        unsafe {
            command.pre_exec(move || {
                if let Some(fd) = watch_fd
                    && fcntl(fd, F_SETFD, 0) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                // C8 / R5-S3 (Linux): the kernel delivers SIGTERM — not
                // SIGKILL — to the direct child as soon as the spawning
                // thread exits (the step's runner waits for the child, so
                // that is the server dying). SIGKILL used to kill the step
                // child before its own watcher thread could reach the
                // grandchildren, which then lived on (they are in the
                // child's process group but do not hold the watch fd).
                // SIGTERM runs the step child's handler instead
                // (`install_step_child_term_handler`), which SIGKILLs the
                // child's whole process group — itself included. A child
                // that is not a FrameTrace binary (no handler) simply
                // takes SIGTERM's default action and terminates.
                #[cfg(target_os = "linux")]
                {
                    if prctl(PR_SET_PDEATHSIG, SIGTERM as u64, 0, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    // The parent may have died between fork and prctl (the
                    // death signal would then never come).
                    if getppid() as u32 != parent {
                        return Err(std::io::Error::from_raw_os_error(3));
                    }
                }
                Ok(())
            });
        }
    }

    unsafe extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        fn getpgrp() -> i32;
        fn getpid() -> i32;
        #[link_name = "kill"]
        fn kill_pid(pid: i32, sig: i32) -> i32;
    }
    #[cfg(target_os = "linux")]
    unsafe extern "C" {
        fn pipe2(fds: *mut i32, flags: i32) -> i32;
    }
    const F_SETFD: i32 = 2;
    const FD_CLOEXEC: i32 = 1;
    /// `O_CLOEXEC` on the Linux architectures FrameTrace ships for
    /// (x86-64, aarch64: the generic value 02000000).
    #[cfg(target_os = "linux")]
    const O_CLOEXEC: i32 = 0o2_000_000;

    /// R5-S4: serializes "create a pipe and mark it close-on-exec" against
    /// [`super::spawn_tracked`]'s fork where `pipe2` does not exist
    /// (macOS): spawns take it shared, pipe creation takes it exclusively,
    /// so no tracked fork can copy the descriptors in the gap between
    /// `pipe()` and `fcntl(FD_CLOEXEC)`. Spawns outside `spawn_tracked`
    /// do not take it; the pipes are therefore created once, early
    /// (`install_process_guard`, before the server starts any worker), and
    /// never again.
    pub(super) static SPAWN_LOCK: std::sync::RwLock<()> = std::sync::RwLock::new(());

    /// A pipe with both ends close-on-exec: atomically with `pipe2` on
    /// Linux, under [`SPAWN_LOCK`] elsewhere.
    pub(super) fn cloexec_pipe() -> Option<[i32; 2]> {
        let mut fds = [0i32; 2];
        #[cfg(target_os = "linux")]
        {
            // SAFETY: pipe2(2) fills two ints.
            if unsafe { pipe2(fds.as_mut_ptr(), O_CLOEXEC) } != 0 {
                return None;
            }
            Some(fds)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _exclusive = SPAWN_LOCK.write().unwrap_or_else(|p| p.into_inner());
            // SAFETY: pipe(2) fills two ints; fcntl on our own fds.
            unsafe {
                if pipe(fds.as_mut_ptr()) != 0 {
                    return None;
                }
                if fcntl(fds[0], F_SETFD, FD_CLOEXEC) != 0
                    || fcntl(fds[1], F_SETFD, FD_CLOEXEC) != 0
                {
                    return None;
                }
            }
            Some(fds)
        }
    }

    /// Both ends of this process's parent-watch pipe `(read, write)`,
    /// created once. Both are close-on-exec (R5-S4); the read end is
    /// opened up for step children only, in their `pre_exec`. The write
    /// end is never closed: it disappears only when this process exits.
    pub(super) fn parent_watch_fds() -> Option<(i32, i32)> {
        static FDS: std::sync::OnceLock<Option<(i32, i32)>> = std::sync::OnceLock::new();
        *FDS.get_or_init(|| cloexec_pipe().map(|fds| (fds[0], fds[1])))
    }

    fn parent_watch_read_fd() -> Option<i32> {
        parent_watch_fds().map(|(read, _)| read)
    }

    /// R5-S3: SIGTERM handler of a step child that leads its own process
    /// group: SIGKILL to the whole group (`kill(0, …)` = the caller's
    /// group, the child itself included, so the process ends here).
    /// `kill(2)` is async-signal-safe; nothing else runs.
    extern "C" fn on_step_child_term(_signum: i32) {
        // SAFETY: kill(2) with integer arguments.
        unsafe {
            kill_pid(0, SIGKILL);
        }
    }

    /// R5-S3: installs [`on_step_child_term`] for SIGTERM. Only called by
    /// a process that verified it leads its own group (never signals a
    /// foreign group). Before this runs (between exec and `main`), SIGTERM
    /// keeps its default action — the child dies, and it has no
    /// grandchildren yet.
    fn install_step_child_term_handler() {
        // SAFETY: installing an async-signal-safe handler.
        unsafe {
            signal(SIGTERM, on_step_child_term as extern "C" fn(i32) as usize);
        }
    }

    /// Child side of the parent-watch pipe (see [`prepare`]): when the
    /// inherited fd hits EOF the parent is gone, and this process — a
    /// step child leading its own process group — kills its group.
    pub(super) fn watch_parent(fd: i32) {
        // SAFETY: plain syscalls on integers.
        let leader = unsafe { getpgrp() == getpid() };
        if !leader {
            // Not a group of our own: never signal someone else's group.
            return;
        }
        // R5-S3: the Linux parent-death signal (SIGTERM) and any other
        // SIGTERM take the whole step tree down, not just this process.
        install_step_child_term_handler();
        // Our own children need not inherit the fd.
        // SAFETY: fcntl on an inherited fd number.
        unsafe {
            fcntl(fd, F_SETFD, FD_CLOEXEC);
        }
        let _ = std::thread::Builder::new()
            .name("frametrace-parent-watch".into())
            .spawn(move || {
                let mut byte = 0u8;
                loop {
                    // SAFETY: read(2) into a 1-byte buffer.
                    let n = unsafe { read(fd, &mut byte, 1) };
                    if n < 0
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                        continue;
                    }
                    if n > 0 {
                        // Nobody writes to the pipe; ignore stray bytes.
                        continue;
                    }
                    // EOF (or a broken fd): the parent is gone.
                    // SAFETY: killpg(0, …) signals the caller's own group.
                    unsafe {
                        killpg(0, SIGKILL);
                    }
                    return;
                }
            });
    }

    /// Unix leader-exit sweep: kills what is left of the group while the
    /// exited leader is still an unreaped zombie (its pid — the group id —
    /// cannot be recycled until then).
    pub(super) fn kill_group_of_exited_leader(key: usize) {
        kill(key);
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
        #[cfg(not(target_os = "linux"))]
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
        // R5-S4: close-on-exec from birth, so no child inherits the
        // self-pipe either.
        let Some(fds) = cloexec_pipe() else {
            return Err(format!("pipe: {}", std::io::Error::last_os_error()));
        };
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

    /// R5-S4: the parent-watch pipe is created here, at server start and
    /// before any worker thread spawns a child, so on macOS (no `pipe2`)
    /// even spawns that bypass [`SPAWN_LOCK`] cannot race its creation.
    pub(super) fn init_process_guard() {
        let _ = parent_watch_fds();
    }

    /// Shared spawn guard (see [`SPAWN_LOCK`]).
    pub(super) fn spawn_guard() -> std::sync::RwLockReadGuard<'static, ()> {
        SPAWN_LOCK.read().unwrap_or_else(|p| p.into_inner())
    }
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

    /// Leader-exit sweep: terminates what is left in the step's job (a
    /// grandchild still holding the step's pipes) — the job handle stays
    /// valid until `release`, so this can never hit another tree.
    pub(super) fn kill_group_of_exited_leader(key: usize) {
        kill(key);
    }
}

/// C8: environment variable carrying the parent-watch pipe's read fd to
/// a step child (unix).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const PARENT_WATCH_ENV: &str = "FRAMETRACE_PARENT_WATCH_FD";

/// C8: child side of the parent-watch pipe. A `frametrace` step child
/// started by the workstation server (env [`PARENT_WATCH_ENV`]) starts a
/// watcher thread that kills the child's whole process group — the child
/// and its ffmpeg/icat/ewfexport grandchildren — the moment the server's
/// end of the pipe closes, i.e. when the server dies for any reason
/// (SIGKILL included). Linux additionally sets `PR_SET_PDEATHSIG` on the
/// direct child at spawn. No-op elsewhere and for processes not started
/// that way. Call once, early, from every binary's `main`.
pub fn watch_parent_from_env() {
    #[cfg(unix)]
    if let Some(fd) = std::env::var(PARENT_WATCH_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .filter(|fd| *fd > 2)
    {
        sys::watch_parent(fd);
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
    // R5-S4 (unix): no tracked fork while a close-on-exec pipe is being
    // set up on a platform without `pipe2`.
    #[cfg(unix)]
    let spawn_guard = sys::spawn_guard();
    let child = command.spawn();
    #[cfg(unix)]
    drop(spawn_guard);
    let child = child?;
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
            // C8/C10: nothing of the step outlives its leader — a straggler
            // (an orphaned ffmpeg still holding the step's pipes, which
            // would hang the reader joins) is killed while the exited
            // leader's zombie still reserves the group id.
            sys::kill_group_of_exited_leader(self.key);
            self.unlist();
            // The group id is only safe to signal while the zombie exists.
            self.key = 0;
        }
        let status = self.child.try_wait()?;
        if status.is_some() {
            #[cfg(windows)]
            if self.key != 0 {
                // Same leader-exit sweep on Windows: the job handle stays
                // valid (and ours) until `release`.
                sys::kill_group_of_exited_leader(self.key);
            }
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

    /// Whether `pid` still runs. R6-S9: on Linux a zombie (`Z`, killed
    /// but not yet reaped by its parent) or a dying task (`X`) is dead —
    /// `kill -0` succeeds on a zombie, so a killed grandchild whose parent
    /// had not reaped it yet looked alive.
    fn alive(pid: &str) -> bool {
        #[cfg(target_os = "linux")]
        {
            !matches!(proc_state(pid).as_deref(), None | Some("Z") | Some("X"))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    }

    /// Linux `/proc/<pid>/stat` state letter (`None`: no such process).
    #[cfg(target_os = "linux")]
    fn proc_state(pid: &str) -> Option<String> {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).ok()?;
        let (_, rest) = stat.rsplit_once(')')?;
        rest.split_whitespace().next().map(str::to_string)
    }

    /// Process facts (state, process group) for assertion messages.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn describe_pid(pid: &str) -> String {
        let out = Command::new("ps")
            .args(["-o", "pid=,ppid=,pgid=,stat=,comm=", "-p", pid.trim()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|err| format!("<ps failed: {err}>"));
        if out.is_empty() {
            format!("pid {pid}: not listed by ps")
        } else {
            format!("pid {pid}: [{out}]")
        }
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

    /// C8/C10: a straggler left in the step's process group when the
    /// leader exits is killed at once (while the leader's zombie still
    /// reserves the group id) — before, it outlived the step and kept the
    /// step's pipes open.
    #[test]
    fn leader_exit_kills_group_stragglers() {
        let _serial = tree_test_serial();
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & echo $!"])
            .stdout(Stdio::piped());
        let mut tracked = spawn_tracked(&mut command).expect("spawn");
        let mut line = String::new();
        BufReader::new(tracked.child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let pid = line.trim().to_string();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while tracked.try_wait().expect("try_wait").is_none() {
            assert!(std::time::Instant::now() < deadline, "leader never exited");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for _ in 0..100 {
            if !alive(&pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = Command::new("kill").args(["-9", &pid]).status();
        panic!("straggler {pid} outlived its exited leader");
    }

    /// C8 (Linux): `PR_SET_PDEATHSIG` — the step child receives SIGKILL
    /// as soon as the thread that spawned it dies (in the server: the
    /// step runner, i.e. the server itself). Before, the child kept
    /// running for its full 30 s.
    #[cfg(target_os = "linux")]
    #[test]
    fn step_child_dies_with_its_spawning_thread() {
        let _serial = tree_test_serial();
        let pid = std::thread::spawn(|| {
            let mut command = Command::new("sleep");
            command.arg("30");
            let tracked = spawn_tracked(&mut command).expect("spawn");
            let pid = tracked.child.id();
            // Dropping the handle neither kills nor waits on unix.
            drop(tracked);
            pid
        })
        .join()
        .unwrap();
        let state = |pid: u32| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit_once(')')
                        .and_then(|(_, rest)| rest.split_whitespace().next().map(str::to_string))
                })
        };
        for _ in 0..100 {
            match state(pid).as_deref() {
                None | Some("Z") | Some("X") => {
                    // Reap the zombie (we are its parent).
                    let mut status = 0i32;
                    unsafe extern "C" {
                        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
                    }
                    // SAFETY: plain syscall on our own child.
                    unsafe {
                        waitpid(pid as i32, &mut status, 1);
                    }
                    return;
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        panic!("step child {pid} survived its spawning thread");
    }

    /// Env var naming the file the R5-S3 helper writes its grandchild's
    /// pid to (its presence is what turns the helper on).
    const PDEATH_PID_FILE_ENV: &str = "FT_R5_PDEATH_PID_FILE";

    /// R5-S3 helper, run as a re-executed copy of this test binary (a
    /// FrameTrace-like step child): installs the step-child side exactly
    /// like `main` does (`watch_parent_from_env`), starts a grandchild in
    /// its own process group, reports the grandchild's pid and waits.
    /// Without [`PDEATH_PID_FILE_ENV`] (a normal test run) it does nothing.
    ///
    /// R6-CI2: the pid goes to a FILE (written, then renamed into place).
    /// It used to be a `GRANDCHILD <pid>` line on stdout, but libtest with
    /// `--test-threads=1` prints `test <name> ... ` WITHOUT a newline before
    /// it runs the test, so the helper's line read `test serve::proc::tests::
    /// r5_pdeath_helper_child ... GRANDCHILD <pid>` and never matched the
    /// `GRANDCHILD ` prefix; the parent then read on until the helper's own
    /// 300 s sleep ended (EOF) and reported "never reported".
    #[test]
    // The grandchild is deliberately never reaped here: the test that
    // re-executes this helper asserts the whole group gets killed.
    #[allow(clippy::zombie_processes)]
    fn r5_pdeath_helper_child() {
        let Some(pid_file) = std::env::var_os(PDEATH_PID_FILE_ENV) else {
            return;
        };
        let pid_file = std::path::PathBuf::from(pid_file);
        watch_parent_from_env();
        let grandchild = Command::new("sleep")
            .arg("300")
            .spawn()
            .expect("spawn grandchild");
        let temp = pid_file.with_extension("tmp");
        std::fs::write(&temp, grandchild.id().to_string()).expect("write grandchild pid");
        std::fs::rename(&temp, &pid_file).expect("publish grandchild pid");
        std::thread::sleep(std::time::Duration::from_secs(300));
    }

    /// R6-CI2 (pure): the helper's libtest argument list. The filter is
    /// the helper's full path without the crate name (`module_path!()` is
    /// `<crate>::serve::proc::tests`), matched with `--exact`.
    fn pdeath_helper_args(module: &str) -> Vec<String> {
        let name = format!(
            "{}::r5_pdeath_helper_child",
            module.split_once("::").map_or(module, |(_, rest)| rest)
        );
        vec![
            name,
            "--exact".to_string(),
            "--nocapture".to_string(),
            "--test-threads=1".to_string(),
        ]
    }

    #[test]
    fn pdeath_helper_filter_names_the_helper() {
        let args = pdeath_helper_args(module_path!());
        assert_eq!(args[0], "serve::proc::tests::r5_pdeath_helper_child");
        assert!(args.contains(&"--exact".to_string()));
    }

    /// R5-S3 (Linux): when the thread that spawned a step child exits
    /// (i.e. the server dies), the parent-death signal now reaches the
    /// GRANDCHILDREN too. Before, `PR_SET_PDEATHSIG(SIGKILL)` killed the
    /// step child outright, its watcher thread never ran, and the
    /// grandchild (`sleep 300`, same process group, no watch fd) survived.
    /// Now the signal is SIGTERM, whose handler SIGKILLs the whole group.
    #[cfg(target_os = "linux")]
    #[test]
    fn pdeathsig_takes_the_grandchildren_down() {
        let _serial = tree_test_serial();
        let exe = std::env::current_exe().expect("test binary");
        let scratch = std::env::temp_dir().join(format!(
            "ft-r6-pdeath-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let pid_file = scratch.join("grandchild.pid");
        let log_file = scratch.join("helper.log");
        let args = pdeath_helper_args(module_path!());
        let (child_pid, grandchild, waited, helper_exit) = {
            let (pid_file, log_file, args) = (pid_file.clone(), log_file.clone(), args.clone());
            std::thread::spawn(move || {
                // The helper's output goes to a file: nothing here can
                // block on a pipe, and a failure can quote it.
                let log = std::fs::File::create(&log_file).expect("helper log");
                let mut command = Command::new(exe);
                command
                    .args(&args)
                    .env(PDEATH_PID_FILE_ENV, &pid_file)
                    .stdin(Stdio::null())
                    .stdout(Stdio::from(log.try_clone().expect("helper log handle")))
                    .stderr(Stdio::from(log));
                let mut tracked = spawn_tracked(&mut command).expect("spawn helper");
                let pid = tracked.child.id();
                // Wait for the grandchild before the spawning thread exits
                // (that exit is what fires the parent-death signal).
                let started = std::time::Instant::now();
                let mut grandchild = String::new();
                let mut exit = None;
                while started.elapsed() < std::time::Duration::from_secs(60) {
                    if let Ok(text) = std::fs::read_to_string(&pid_file) {
                        grandchild = text.trim().to_string();
                        break;
                    }
                    if let Ok(Some(status)) = tracked.child.try_wait() {
                        exit = Some(status);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                if grandchild.is_empty() && exit.is_none() {
                    tracked.kill_tree();
                    exit = tracked.child.wait().ok();
                }
                let waited = started.elapsed();
                // Dropping the handle neither kills nor waits on unix.
                drop(tracked);
                (pid, grandchild, waited, exit)
            })
            .join()
            .unwrap()
        };
        let helper_log = || {
            std::fs::read_to_string(&log_file)
                .unwrap_or_else(|err| format!("<helper log unreadable: {err}>"))
        };
        assert!(
            !grandchild.is_empty(),
            "helper never reported its grandchild (waited {waited:?}; helper pid {child_pid}, exit {helper_exit:?}; args {args:?}; pid file {}). Helper output:\n{}",
            pid_file.display(),
            helper_log()
        );
        for _ in 0..200 {
            if !alive(&grandchild) {
                // Reap the helper (we are its parent).
                unsafe extern "C" {
                    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
                }
                let mut status = 0i32;
                // SAFETY: plain syscall on our own child.
                unsafe {
                    waitpid(child_pid as i32, &mut status, 0);
                }
                let _ = std::fs::remove_dir_all(&scratch);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let facts = format!(
            "{}; helper {}",
            describe_pid(&grandchild),
            describe_pid(&child_pid.to_string())
        );
        let _ = Command::new("kill").args(["-9", &grandchild]).status();
        let _ = Command::new("kill")
            .args(["-9", &child_pid.to_string()])
            .status();
        panic!(
            "grandchild {grandchild} survived the step child's parent-death signal ({facts}). Helper output:\n{}",
            helper_log()
        );
    }

    /// Open file descriptors of a fresh `sh` started by `command`
    /// (`/dev/fd` is per-process on Linux and macOS).
    fn child_has_fd(command: &mut Command, fd: i32) -> bool {
        let output = command
            .args([
                "-c",
                &format!("[ -e /dev/fd/{fd} ] && echo OPEN || echo CLOSED"),
            ])
            .stdout(Stdio::piped())
            .output()
            .expect("run sh");
        String::from_utf8_lossy(&output.stdout).contains("OPEN")
    }

    /// R5-S4: the parent-watch pipe is close-on-exec on both ends, so an
    /// ordinary child (a browser, a probe) inherits neither — before, the
    /// read end leaked into every child, and the write end leaked into any
    /// child forked between `pipe()` and `fcntl()`. A step child gets the
    /// read end only (cleared in its `pre_exec`), never the write end
    /// (whose leak would keep the step children from ever seeing EOF).
    #[test]
    fn watch_pipe_reaches_step_children_only() {
        let _serial = tree_test_serial();
        let (read, write) = sys::parent_watch_fds().expect("watch pipe");
        let mut plain = Command::new("sh");
        assert!(!child_has_fd(&mut plain, read), "read end leaked");
        let mut plain = Command::new("sh");
        assert!(!child_has_fd(&mut plain, write), "write end leaked");
        for (fd, expected) in [(read, true), (write, false)] {
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    &format!("[ -e /dev/fd/{fd} ] && echo OPEN || echo CLOSED"),
                ])
                .stdout(Stdio::piped());
            let mut tracked = spawn_tracked(&mut command).expect("spawn");
            let mut line = String::new();
            BufReader::new(tracked.child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            let _ = tracked.child.wait();
            assert_eq!(line.trim() == "OPEN", expected, "fd {fd}: {line}");
        }
    }

    /// C8: a step child is handed the parent-watch pipe (unix) through
    /// the environment.
    #[test]
    fn step_child_receives_the_parent_watch_fd() {
        let _serial = tree_test_serial();
        let mut command = Command::new("sh");
        command
            .args(["-c", &format!("echo ${}", PARENT_WATCH_ENV)])
            .stdout(Stdio::piped());
        let mut tracked = spawn_tracked(&mut command).expect("spawn");
        let mut line = String::new();
        BufReader::new(tracked.child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let _ = tracked.child.wait();
        let fd: i32 = line.trim().parse().expect("fd number in the env");
        assert!(fd > 2, "{fd}");
    }
}
