#!/usr/bin/env bash
# Runs the viewer/shell/e2e harnesses sequentially and prints a summary.
# Expects: a built target/debug/frametrace-app (or FRAMETRACE_BIN), Node with
# playwright resolvable (NODE_PATH or local node_modules), ffmpeg on PATH.
set -u
cd "$(dirname "$0")/.."

harnesses=(
  "verify-viewer-undo"
  "verify-player-keys"
  "verify-range-sync"
  "verify-virtual-grid"
  "verify-live-index"
  "verify-shell-jobs"
  "verify-facets"
  "verify-i18n"
  "verify-player-popup-escape"
  "verify-viewer-dialogs"
  "verify-jobs-e2e --no-build"
  "verify-start-screen"
  "verify-review-batch --unit"
  "bench-viewer"
  "check-a11y"
  # UI rebuild (docs/ui-rebuild-contract.md §8.2): each track replaces ONLY
  # its own anchor line below with the real entry — never reorder or append.
  "verify-buttons"
  "verify-embed"
  "verify-layout"
  "verify-progress"
  "verify-finalize-sheet"
  "verify-marks-roundtrip"
  # Save protocol v2 (docs/FIX_PLAN_R1_2026-10-13.md §1.7): two windows, conflicts, chunked bulk saves.
  "verify-marks-sync"
  # Adversarial-review regressions of the rebuilt shell (B1/B2/S1/S2/S4/S5).
  "verify-rebuild-regressions"
  # Phase 2 (docs/PLAN_PHASE2_JOIN_AUDIO_2026-10-13.md §3): lossless join UI — mock API (quick unit:
  # race/fallback shapes) and the real server (ffmpeg fixtures, cancel, hosted-shell reload + marks) —
  # + measured-sound facet.
  "verify-join"
  "verify-join --live"
  "verify-audio-facet"
  # Rust/JS recording-time parser parity (tests/fixtures/rectime-cases.json, from T1).
  "check-rectime-fixture"
)

all_start=$(date +%s)
failed=()
# R6 U11: per-harness durations, printed at the end (the CI time limit in
# .github/workflows/harnesses.yml is set from these measurements).
durations=()
for entry in "${harnesses[@]}"; do
  # shellcheck disable=SC2086
  set -- $entry
  name="$1"; shift
  echo "::group::$name"
  start=$(date +%s)
  if node "scripts/$name.mjs" "$@"; then
    status=ok
  else
    status=FAIL
    failed+=("$name")
  fi
  echo "::endgroup::"
  took=$(( $(date +%s) - start ))
  echo "[$status] $name (${took}s)"
  durations+=("$(printf '%5d s  %s %s' "$took" "$entry" "$status")")
done

echo
echo "durations (slowest first):"
printf '%s\n' "${durations[@]}" | sort -rn
echo "total: $(( $(date +%s) - all_start )) s"
echo
if [ "${#failed[@]}" -gt 0 ]; then
  echo "FAILED harnesses: ${failed[*]}"
  exit 1
fi
echo "all ${#harnesses[@]} harnesses passed"
