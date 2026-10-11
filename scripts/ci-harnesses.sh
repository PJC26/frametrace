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
  # @T1 verify-buttons
  "verify-embed"
  # @T2 verify-layout
  # @T3 verify-progress
  # @T4 verify-finalize-sheet
)

failed=()
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
  echo "[$status] $name ($(( $(date +%s) - start ))s)"
done

echo
if [ "${#failed[@]}" -gt 0 ]; then
  echo "FAILED harnesses: ${failed[*]}"
  exit 1
fi
echo "all ${#harnesses[@]} harnesses passed"
