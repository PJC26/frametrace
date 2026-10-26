# Performance Validation

FrameTrace is designed for terabyte-scale evidence by avoiding full hashing/probing unless requested and by keeping the primary index in SQLite.

## SQLite Scale Check

Use the synthetic benchmark to validate the local machine and build:

```powershell
.\target\release\frametrace.exe benchmark-db C:\Temp\frametrace-db-bench --rows 1000000
```

The command creates `C:\Temp\frametrace-db-bench\db\case.db` and inserts synthetic video-index rows in a single transaction using the production `videos` schema. It does not claim end-to-end media scan speed; it validates SQLite write-path behavior for large indexes.

## Current macOS Baseline

Latest local validation on macOS (Apple Silicon host, release build):

- Command: `target/release/frametrace benchmark-db /tmp/frametrace-bench-1m --rows 1000000`
- Result: 1,000,000 synthetic `videos` rows inserted in 15.3 seconds.
- Database size: 670 MiB.
- SQLite integrity check: `ok`.

This baseline validates the large SQLite index write path on macOS. It does not replace Windows MSVC validation or terabyte-scale media scan testing against real evidence.

For routine development, use a smaller run:

```powershell
cargo run -- benchmark-db ./target/frametrace-db-bench --rows 10000
```

## In-tree benchmark harness

`tests/perf_budgets.rs` is an opt-in (all `#[ignore]`d, `std::time` only, no
extra crates) harness over synthetic workloads:

```text
cargo test --locked --test perf_budgets -- --ignored --nocapture
```

| Measurement | What it covers | Reference budget |
| --- | --- | --- |
| `index_write_read_10k_rows` | `case_db::benchmark_case_db` single-transaction writes + `load_video_ids` / `summarize_case_db` reads | 10k-row write ≈ 591 ms release (`docs/archive/ROADMAP-v2.md` §1); sanity ceiling 30 s write / 10 s read in debug |
| `audit_append_verify_1k_entries` | `audit::append_chained_jsonl` lock+chain+fsync per entry, then `verify_chained_jsonl` | sanity ceiling 60 s append / 10 s verify |
| `scan_synthetic_tree_1k_files` | `scan_folder` walk + SQLite/JSON/JSONL/TSV index outputs, `--no-ffprobe`, no hashing | sanity ceiling 30 s |

The in-test ceilings are regression tripwires for debug builds, not the field
budgets — the product-level budgets (10k viewer load ≤1.5 s, 1k thumbnails
≤180 s, 1k validations ≤240 s, inspect-image 10k entries ≤120 s) live in
`docs/archive/ROADMAP-v2.md` §3 (performance budgets) and are exercised against real media. CI runs this harness weekly and on manual dispatch (`.github/workflows/perf.yml`, release-less debug build, `--ignored`). CI also runs a
smaller `qa performance --rows 1000` gate (≥50k rows/minute) on both the
Windows and macOS lanes.

## Field Performance Rules

- Start with `scan-folder --no-ffprobe` and no `--hash`.
- Limit depth with `--max-depth` when the media layout is known.
- Hash only selected folders or final evidence subsets.
- Generate thumbnails/proxies lazily.
- Keep the case folder on fast local SSD storage when processing external media.
