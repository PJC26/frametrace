# FrameTrace fuzz targets

Coverage-guided fuzzing (cargo-fuzz / libFuzzer) for the hand-rolled parsers
that touch untrusted bytes: recorded evidence metadata, audit logs, and raw
carve input.

## Targets

| Target | Surface |
| --- | --- |
| `audit_jsonl_verify` | `audit::verify_chained_jsonl_text` — hash-chain JSONL verification (`entry_sha256` placement, `previous_entry_sha256` chaining, torn-tail handling) |
| `index_jsonl_records` | `scan::json_record_lines` record splitter + `serde` `VideoRecord` parse + `scan::set_json_field` byte-stable field editor |
| `carve_signature_scan` | `carve::find_video_signatures_in` — chunked MP4/AVI/DHAV/IMKH signature scan with overlap bookkeeping |
| `creation_time_unix` | `timeline::parse_creation_time_unix` — ISO-8601 subset parser for ffprobe `creation_time` tags |
| `dav_walk_frames` | `dav::walk_frames` — DHAV frame-record walker (20-byte prefixes, attacker-controlled lengths); input is written to a per-process scratch file |
| `rectime_parse` | `rectime::parse_name_time` / `parse_record_time` — recording-time pattern matcher over filenames and tags |
| `dfxml_export` | `dfxml::export_dfxml` — XML rendering of index rows (file names/paths from evidence); input is the case's `db/videos.jsonl` |

## Running

Requires a nightly toolchain (libFuzzer needs `-Z` flags) and cargo-fuzz:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --version 0.13.2 --locked   # CI pins the same version
cargo +nightly fuzz run audit_jsonl_verify
```

Run from the repository root. Use `-j` for parallel workers, e.g.
`cargo +nightly fuzz run index_jsonl_records -- -jobs=8`. Crash artifacts land
in `fuzz/artifacts/<target>/`; copy any crash into a regression test before
fixing.

The fuzz crate is its own workspace (`fuzz/Cargo.toml` carries an explicit
`[workspace]`), so the main `cargo build`/`cargo test --locked` never
compiles libfuzzer-sys. The `#[doc(hidden)]` functions the targets call are
fuzz seams, not supported API.

## Not yet covered (needs a `#[doc(hidden)] pub` fuzz seam in `src/`)

- MP4 box walk (`carve/mp4.rs`: `mp4_top_boxes`, `mp4_sample_payload_bytes`, `find_moov_in_region`) is `pub(super)`; `carve_signature_scan` only reaches the signature scan. A seam taking a byte slice or path is needed before a target can be added.
- TSK/E01 output parsers (`tsk.rs`, `e01.rs`) parse external-tool stdout; a `&str`-taking seam per parser would allow fuzzing them.

CI pins the nightly toolchain by date (see `.github/workflows/fuzz.yml`); bump it deliberately.
