#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use std::sync::OnceLock;

// `dav::walk_frames` streams DHAV frame records with 20-byte prefixes and
// attacker-controlled length fields. It only accepts a path, so each input is
// written to one scratch file per process. Arbitrary bytes must return
// Ok/Err, never panic, overflow, or loop forever.
fn scratch() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        std::env::temp_dir().join(format!("ft-fuzz-dav-{}.dav", std::process::id()))
    })
}

fuzz_target!(|data: &[u8]| {
    let path = scratch();
    if std::fs::write(path, data).is_ok() {
        let _ = frametrace::dav::walk_frames(path);
    }
});
