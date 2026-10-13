#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use std::sync::OnceLock;

// `dfxml::export_dfxml` renders attacker-influenced index rows (file names,
// paths, hashes taken from evidence media) into XML. Arbitrary bytes as the
// case's videos.jsonl must produce Ok/Err, never panic. One scratch case per
// process; the chained audit log is reset each iteration so it cannot grow.
fn case_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("ft-fuzz-dfxml-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db")).expect("scratch case db dir");
        std::fs::write(dir.join("case.json"), "{}").expect("scratch case.json");
        dir
    })
}

fuzz_target!(|data: &[u8]| {
    let dir = case_dir();
    if std::fs::write(dir.join("db/videos.jsonl"), data).is_err() {
        return;
    }
    let _ = std::fs::remove_dir_all(dir.join("evidence"));
    let _ = frametrace::dfxml::export_dfxml(dir, &dir.join("reports/case-index.dfxml"));
});
