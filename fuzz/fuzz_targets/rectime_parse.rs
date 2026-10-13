#![no_main]

use libfuzzer_sys::fuzz_target;

// The recording-time parser is a hand-rolled pattern matcher over `char`
// indices with fixed-width numeric captures (a Rust port of the viewer's
// parseTimeFromName). Arbitrary filenames/tags - including non-UTF-8 input
// and multi-byte characters - must return Some/None, never panic or index
// out of range.
fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = frametrace::rectime::parse_name_time(&text);
    let parts: Vec<&str> = text.split('\n').collect();
    let _ = frametrace::rectime::parse_record_time(&parts);
});
