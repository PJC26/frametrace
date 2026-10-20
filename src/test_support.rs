//! Shared helpers for unit tests that write and exec fake shell scripts.

/// Makes sure a just-written, chmod'ed fake script can be exec'd.
///
/// On Linux a parallel test's `fork` may still hold the freshly written file
/// open until its own `exec`, so exec'ing it fails with ETXTBSY (os error 26).
/// The script must start with `[ "$1" = --warm ] && exit 0` (right after the
/// shebang, before any logging); this runs it with `--warm`, retrying
/// ETXTBSY for up to 10 s and panicking on any other error.
#[cfg(unix)]
pub(crate) fn warm_unix_script(path: &std::path::Path) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::process::Command::new(path).arg("--warm").output() {
            Ok(out) if out.status.success() => break,
            Ok(out) => panic!("fake script --warm failed: {out:?}"),
            Err(e) if e.raw_os_error() == Some(26) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("fake script --warm: {e}"),
        }
    }
}
