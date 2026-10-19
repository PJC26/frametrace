# deepfake-lens integration

FrameTrace screens evidence for synthetic-media signals through the
[deepfake-lens](https://github.com/daeryundf2-prog/deepfake-lens) sidecar
(Python, local-first). It is invoked as a subprocess — the same boundary
as ffprobe / Sleuth Kit / libewf — via `src/deepfake.rs`.

## Contract

```
deepfake-lens forensic <file> --format json   →  JSON on stdout
```

```json
{
  "score": 0,                    // 0-100 review priority, NOT a verdict
  "band": "low",                 // low | medium | high | unknown
  "band_label": "낮음",
  "verdict": "…",
  "signals": [{"title": "…", "detail": "…", "weight": 20}],
  "limitations": ["…"],
  "provenance_records": [],
  "has_c2pa": false,
  "has_synthid": false,
  "has_watermark": false
}
```

**Framing rule:** a `high` score means "review this first", never
"confirmed synthetic". Surfaces must keep that wording — the tool has
measured false-positive/false-negative behaviour (see
`experiments/TEXT_DETECTION_EVAL.md` in the deepfake-lens repo).

## Resolution order

1. `FRAMETRACE_DEEPFAKE_LENS` env var — full path or bare tool name
2. the private venv of an on-demand install (`<exe dir>/tools/python/deepfake-lens`;
   disabled until deepfake-lens has hash-pinnable published wheels)
3. `deepfake-lens` on PATH — deepfake-lens is **not on PyPI**, so install it
   from the checkout or wheel you were given (`pip install <path>`)
4. Fallback: `python -m deepfake_lens.cli` — for source checkouts set
   `FRAMETRACE_DEEPFAKE_LENS_HOME=<repo root>` so PYTHONPATH resolves.

## CLI

```
frametrace deepfake-screen <file> [--json-out out.json]
```

## Wiring into the case pipeline

1. **Artifact file — implemented.** `scan-folder --deepfake` screens
   each indexed file and writes `artifacts/deepfake/<id>.json`
   (full forensic report, or `{"ok":false,"error":…}` on failure).
   Screening failures become scan warnings, never aborts; incremental
   rescans and checkpoint replay skip already-indexed files.

   For E01/carve/recover pipelines that never pass through
   `scan-folder`, `frametrace deepfake-scan <case> [--force]` screens
   every record family — indexed videos, carved candidates
   (`carve-log.jsonl`), filesystem-recovered files (`recover-inode`
   events) — and writes the same per-id artifacts. Records that already
   have an artifact are skipped unless `--force`. The workstation UI
   exposes it as 고급 도구 → "딥페이크 스크리닝". Record ids are
   sanitized into filenames (`inode:0:1304` → `inode_0_1304`); the
   viewer applies the same mapping when looking up reports.
2. **`VideoRecord` field**: add `pub deepfake: Option<DeepfakeSummary>`
   serialized as `deepfake_*` flattened keys (same pattern as
   `probe_summary_flat`). Touches the published JSONL contract — bump
   deliberately.
3. **Viewer badge — implemented.** `make-review` embeds the artifact
   map as `DATA.deepfake`; `evidence_viewer.js` renders a band-colored
   `badge dfl` chip on each card and a "합성의심" row (score, top
   signal titles, or the screening error) in the detail panel. The
   badge title keeps the "검토 우선순위, 판정 아님" framing.

Only non-video files (audio, images, documents, archives) currently sit
outside `VideoRecord`; screen them through the same lane when the index
grows a generic file record.

## Notes

- Timeout: 900 s (neural members are slow on CPU-only boxes).
- Optional weights (SyncNet, AIDE, AASIST, ECAPA) live under the
  deepfake-lens `models/` dir; absent weights degrade to heuristics.
- Uploaded evidence bytes never leave the workstation; the sidecar does
  no network I/O.
