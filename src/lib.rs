pub mod anomaly;
pub mod artifacts;
pub mod audio_level;
pub mod audit;
pub mod audit_key;
pub mod carve;
pub mod case_compare;
pub mod case_db;
pub mod case_merge;
pub mod checkpoint;
pub mod cli;
pub mod custody;
pub mod dav;
pub mod deepfake;
pub mod detector;
pub mod dfxml;
pub mod diskspace;
pub mod e01;
pub mod ffprobe;
pub mod hikvision;
pub mod hmac;
pub mod html_report;
pub mod join;
pub mod known_hash;
pub mod model;
pub mod package;
pub mod qa;
pub mod rectime;
pub mod redact;
pub mod report;
pub mod scan;
pub mod selection;
pub mod serve;
pub mod sha256;
#[cfg(target_os = "windows")]
pub mod shell;
pub mod telemetry;
pub mod timeline;
pub mod tool_policy;
pub mod transcode;
pub mod tsk;
pub mod util;
pub mod validation;
pub mod video_export;

/// Runs `f` on a dedicated worker thread with a large stack and returns its
/// exit code.
///
/// Windows reserves only 1 MiB for the main thread's stack (POSIX systems
/// typically allow 8 MiB). The clap command tree plus the top-level dispatch
/// match overflow 1 MiB, so `frametrace --help` crashed with a stack
/// overflow on Windows before any work started. Both binaries route their
/// real entry point through this so behaviour is identical on every OS.
pub fn run_with_large_stack(f: impl FnOnce() -> i32 + Send + 'static) -> i32 {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(f)
        .expect("failed to spawn frametrace worker thread")
        .join()
        .unwrap_or(1)
}
