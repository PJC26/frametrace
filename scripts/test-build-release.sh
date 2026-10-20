#!/usr/bin/env bash
# Fixture harness for scripts/build-release.sh (no real build, no network):
# copies the script into a throwaway repo, puts stub `cargo`, `rustc` and
# `zip` first on PATH, and checks that a Windows UNCONFIRMED test package in
# dist/ — by name (any case, browser/Explorer copy names) or by the
# "Owner to confirm" marker inside its THIRD_PARTY_NOTICES.md — stops the
# run before SHA256SUMS is written, while a clean dist/ still gets sums.
#
# Runs from Git Bash on Windows and from bash on Linux/macOS:
#   bash scripts/test-build-release.sh
# Needs: shasum (the script under test), python3 or python (to make the
# marker fixture zips; those cases are skipped without it), unzip
# (optional: the unzip-reader case is skipped without it).
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d 2>/dev/null || mktemp -d -t ft-build-release)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
skip=0
ok()   { pass=$((pass + 1)); echo "ok   $*"; }
bad()  { fail=$((fail + 1)); echo "FAIL $*"; }
skp()  { skip=$((skip + 1)); echo "skip $*"; }
check() { # check <description> <command...>
    local d="$1"; shift
    if "$@"; then ok "$d"; else bad "$d"; fi
}

PY="$(command -v python3 2>/dev/null || command -v python 2>/dev/null || true)"
if [ -n "$PY" ] && ! "$PY" -c 'import zipfile' >/dev/null 2>&1; then PY=""; fi
HAVE_UNZIP=0
command -v unzip >/dev/null 2>&1 && HAVE_UNZIP=1

# --- stubs ------------------------------------------------------------------
STUBS="$WORK/stubs"
mkdir -p "$STUBS"
cat > "$STUBS/cargo" <<'EOF'
#!/usr/bin/env bash
echo "cargo $*" >> "$FT_LOG"
case "${PLANT:-}" in
    name) : > "dist/FrameTrace-9.9.9-win64-UNCONFIRMED (1).zip" ;;
    marker) cp "$FT_MARKER_ZIP" "dist/FrameTrace-9.9.9-win64.zip" ;;
esac
exit 0
EOF
cat > "$STUBS/rustc" <<'EOF'
#!/usr/bin/env bash
echo "rustc 1.0.0 (stub)"
echo "host: x86_64-unknown-linux-gnu"
EOF
cat > "$STUBS/zip" <<'EOF'
#!/usr/bin/env bash
# zip -qr <out.zip> <dir>: a placeholder file is enough for SHA256SUMS.
echo "stub zip of $3" > "$2"
EOF
chmod +x "$STUBS/cargo" "$STUBS/rustc" "$STUBS/zip"

# make_zip <out.zip> <entry name> <content>: a real zip with one entry.
make_zip() {
    "$PY" -I -c '
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w") as z:
    z.writestr(sys.argv[2], sys.argv[3].encode("utf-8"))
' "$1" "$2" "$3"
}
MARKER_TEXT=$'# Third-party notices\r\n\r\nFFmpeg is not bundled. (Owner to\r\n   confirm this paragraph.)\r\n'
CLEAN_TEXT=$'# Third-party notices\r\n\r\nFFmpeg is not bundled; the owner confirmed this paragraph.\r\n'
MARKER_ZIP="$WORK/marker.zip"
if [ -n "$PY" ]; then
    make_zip "$MARKER_ZIP" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$MARKER_TEXT"
fi

# fresh_repo: a new fixture repo at $REPO with an empty dist/.
n=0
fresh_repo() {
    n=$((n + 1))
    REPO="$WORK/repo$n"
    mkdir -p "$REPO/scripts" "$REPO/target/release" "$REPO/dist"
    cp "$ROOT/scripts/build-release.sh" "$REPO/scripts/"
    printf '[package]\nname = "frametrace"\nversion = "9.9.9"\n' > "$REPO/Cargo.toml"
    echo readme > "$REPO/README.md"
    echo bin > "$REPO/target/release/frametrace"
    : > "$WORK/log$n"
}
# run_script [env...]: runs the copy; sets RC, OUT, CARGO_CALLS.
run_script() {
    OUT="$(env PATH="$STUBS:$PATH" FT_LOG="$WORK/log$n" FT_MARKER_ZIP="$MARKER_ZIP" "$@" \
        bash "$REPO/scripts/build-release.sh" 2>&1)"
    RC=$?
    CARGO_CALLS="$(grep -c '^cargo ' "$WORK/log$n" || true)"
}
has_sums() { [ -f "$REPO/dist/SHA256SUMS" ]; }
no_sums() { [ ! -f "$REPO/dist/SHA256SUMS" ]; }
out_has() { printf '%s' "$OUT" | grep -qF -- "$1"; }
sums_has() { grep -qF -- "$1" "$REPO/dist/SHA256SUMS"; }
sums_lacks() { ! tr '[:upper:]' '[:lower:]' < "$REPO/dist/SHA256SUMS" | grep -qF -- "$1"; }

# --- refused by name, before the build ----------------------------------
for name in \
    "FrameTrace-9.9.9-win64-UNCONFIRMED.zip" \
    "FrameTrace-9.9.9-win64-unconfirmed.zip" \
    "FrameTrace-9.9.9-win64-UNCONFIRMED (1).zip" \
    "FrameTrace-9.9.9-win64-UNCONFIRMED - 복사본.zip" \
    "복사본 - FrameTrace-9.9.9-win64-Unconfirmed.ZIP"; do
    fresh_repo
    : > "$REPO/dist/$name"
    : > "$REPO/dist/other-1.0-aarch64.zip"
    run_script
    check "name '$name': exit 1" [ "$RC" -eq 1 ]
    check "name '$name': cargo not run" [ "$CARGO_CALLS" -eq 0 ]
    check "name '$name': no SHA256SUMS" no_sums
    check "name '$name': message names it" out_has "$name"
    check "name '$name': NOT FOR RELEASE" out_has "NOT FOR RELEASE"
done

# --- planted during the build: the second check -------------------------
fresh_repo
run_script PLANT=name
check "planted copy name: exit 1" [ "$RC" -eq 1 ]
check "planted copy name: cargo ran once" [ "$CARGO_CALLS" -eq 1 ]
check "planted copy name: no SHA256SUMS" no_sums

# --- clean dist/ -----------------------------------------------------------
fresh_repo
: > "$REPO/dist/other-1.0-aarch64.zip"
run_script
check "clean: exit 0" [ "$RC" -eq 0 ]
check "clean: SHA256SUMS written" has_sums
if has_sums; then
    check "clean: sums list our zip" sums_has "frametrace-9.9.9-x86_64-unknown-linux-gnu.zip"
    check "clean: sums list the other zip" sums_has "other-1.0-aarch64.zip"
fi

# --- the SHA256SUMS glob excludes copy names on its own ---------------------
fresh_repo
# Drop the second refuse_unconfirmed call (the last bare one) from the copy.
"${PY:-python3}" -I -c '
import sys
p = sys.argv[1]
lines = open(p, encoding="utf-8").read().split("\n")
i = max(k for k, l in enumerate(lines) if l == "refuse_unconfirmed")
del lines[i]
open(p, "w", encoding="utf-8", newline="\n").write("\n".join(lines))
' "$REPO/scripts/build-release.sh" 2>/dev/null || {
    # no python: same edit with awk (last bare line)
    awk '{a[NR]=$0} $0=="refuse_unconfirmed"{last=NR} END{for(i=1;i<=NR;i++) if(i!=last) print a[i]}' \
        "$REPO/scripts/build-release.sh" > "$REPO/scripts/b.sh" && mv "$REPO/scripts/b.sh" "$REPO/scripts/build-release.sh"
}
run_script PLANT=name
check "glob only (second check removed): exit 0" [ "$RC" -eq 0 ]
if has_sums; then
    check "glob only: copy name not in SHA256SUMS" sums_lacks "unconfirmed"
    check "glob only: our zip in SHA256SUMS" sums_has "frametrace-9.9.9-x86_64-unknown-linux-gnu.zip"
else
    bad "glob only: SHA256SUMS missing"
fi

# --- refused by the THIRD_PARTY_NOTICES.md marker ---------------------------
if [ -z "$PY" ]; then
    skp "marker cases (no python3/python to build fixture zips)"
else
    readers=("" python none)
    if [ "$HAVE_UNZIP" -eq 1 ]; then readers+=(unzip); else skp "marker with the unzip reader (no unzip)"; fi
    for reader in "${readers[@]}"; do
        label="${reader:-default}"
        fresh_repo
        cp "$MARKER_ZIP" "$REPO/dist/FrameTrace-9.9.9-win64.zip"
        run_script FRAMETRACE_ZIP_READER="$reader"
        if [ "$reader" = none ]; then
            check "marker, reader none: exit 0 (cannot look inside)" [ "$RC" -eq 0 ]
            check "marker, reader none: warns that it did not check" out_has "NOT checked for the 'Owner to confirm' marker"
        else
            check "marker, reader $label: exit 1" [ "$RC" -eq 1 ]
            check "marker, reader $label: cargo not run" [ "$CARGO_CALLS" -eq 0 ]
            check "marker, reader $label: no SHA256SUMS" no_sums
            check "marker, reader $label: names the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"
        fi
    done

    # lower-case entry name at the zip root, odd zip name
    fresh_repo
    make_zip "$REPO/dist/renamed.zip" "third_party_notices.md" $'x\n(owner TO\n\tconfirm)\n'
    run_script
    check "marker, root lower-case entry: exit 1" [ "$RC" -eq 1 ]

    # planted during the build: the second check, by content
    fresh_repo
    run_script PLANT=marker
    check "planted marker zip: exit 1" [ "$RC" -eq 1 ]
    check "planted marker zip: cargo ran once" [ "$CARGO_CALLS" -eq 1 ]
    check "planted marker zip: no SHA256SUMS" no_sums

    # confirmed notices and an unreadable zip pass and are listed
    fresh_repo
    make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$CLEAN_TEXT"
    echo "not a zip" > "$REPO/dist/broken.zip"
    run_script
    check "confirmed notices: exit 0" [ "$RC" -eq 0 ]
    if has_sums; then
        check "confirmed notices: listed in SHA256SUMS" sums_has "FrameTrace-9.9.9-win64.zip"
        check "unreadable zip: listed in SHA256SUMS" sums_has "broken.zip"
    else
        bad "confirmed notices: SHA256SUMS missing"
    fi
fi

# --- --help prints the header only -------------------------------------------
fresh_repo
run_script
OUT="$(bash "$REPO/scripts/build-release.sh" --help 2>&1)"
check "--help: names make-portable.ps1" out_has "scripts/make-portable.ps1, which writes the same SHA256SUMS."
check "--help: stops before code" bash -c '! printf "%s" "$1" | grep -q "set -euo"' _ "$OUT"

echo
echo "build-release fixture: $pass ok, $fail failed, $skip skipped"
[ "$fail" -eq 0 ] || exit 1
echo "all checks passed"
