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

    pub(super) fn prepare(_command: &mut std::process::Command) {}

    pub(super) fn attach(child: &std::process::Child) -> Option<usize> {
        // SAFETY: plain Win32 calls on handles we own; the job handle is
        // closed in `release` (which, with KILL_ON_JOB_CLOSE, also reaps any
        // survivors of the tree).
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
            let process = HANDLE(child.as_raw_handle());
            if !configured || AssignProcessToJobObject(job, process).is_err() {
                let _ = CloseHandle(job);
                return None;
            }
            Some(job.0 as usize)
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
    for key in keys {
        sys::kill(key);
    }
}
