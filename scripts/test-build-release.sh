#!/usr/bin/env bash
# Fixture harness for scripts/build-release.sh (no real build, no network):
# copies the script into a throwaway repo, puts stub `cargo`, `rustc` and
# `zip` first on PATH, and checks that a Windows UNCONFIRMED test package in
# dist/ — by name (any case, browser/Explorer copy names) or by the
# "Owner to confirm" marker inside its THIRD_PARTY_NOTICES.md — stops the
# run before SHA256SUMS is written, while a clean dist/ still gets sums; and
# that an unconfirmed working tree (tools/manifest.json notice_confirmed not
# the boolean true for an installable tool, or THIRD_PARTY_NOTICES.md still
# saying "Owner to confirm") stops it before cargo runs.
#
# Runs from Git Bash on Windows and from bash on Linux/macOS:
#   bash scripts/test-build-release.sh
# Needs: shasum (the script under test), python3 or python (to make the
# fixture zips and as the script's first zip reader; without it the marker
# cases are skipped and the rest run with FRAMETRACE_SKIP_NOTICES_CHECK=1,
# which no longer skips the manifest check — that then needs node, the
# script's second manifest reader; node also runs its own cases),
# unzip (optional: the unzip-reader cases are skipped without it), bsdtar
# (optional: `tar` when it is bsdtar, or FRAMETRACE_TEST_BSDTAR=<path>, e.g.
# C:/Windows/System32/tar.exe; the tar-reader cases are skipped without it).
# Skipped cases are counted and shown as "SKIPPED n" in the summary.
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

# Same probe as the script's find_python: the first of python3/python that
# can `import zipfile` (a Microsoft Store alias python3 is skipped).
PY=""
for c in python3 python; do
    command -v "$c" >/dev/null 2>&1 || continue
    if "$c" -I -c 'import zipfile' >/dev/null 2>&1; then PY="$(command -v "$c")"; break; fi
done
HAVE_UNZIP=0
command -v unzip >/dev/null 2>&1 && HAVE_UNZIP=1
# Same probe as the script's find_node.
NODE=""
if command -v node >/dev/null 2>&1 && node -e 0 >/dev/null 2>&1; then NODE=node; fi

# --- stubs ------------------------------------------------------------------
STUBS="$WORK/stubs"
mkdir -p "$STUBS"
cat > "$STUBS/cargo" <<'EOF'
#!/usr/bin/env bash
echo "cargo $*" >> "$FT_LOG"
# Fix round 13 H1: like cargo, build into $CARGO_TARGET_DIR when it is set
# (relative = to the cwd, the repo root); FT_CARGO_NOWRITE=1 writes nothing.
if [ -n "${CARGO_TARGET_DIR:-}" ] && [ "${FT_CARGO_NOWRITE:-}" != 1 ]; then
    mkdir -p "$CARGO_TARGET_DIR/release"
    echo fresh-from-cargo > "$CARGO_TARGET_DIR/release/frametrace"
fi
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
# zip -qr <out.zip> <dir>: a real (readable) zip of every file under <dir>,
# so the content check before SHA256SUMS reads the staged
# THIRD_PARTY_NOTICES.md; an empty zip without python.
if [ -n "${FT_PY:-}" ]; then
    "$FT_PY" -I -c '
import os, sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w") as z:
    for root, _dirs, files in os.walk(sys.argv[2]):
        for f in sorted(files):
            p = os.path.join(root, f)
            z.write(p, p.replace(os.sep, "/"))
' "$2" "$3"
else
    { printf 'PK\005\006'; head -c 18 /dev/zero; } > "$2"
fi
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
# make_zip_n <out.zip> <name> <content> [<name> <content>...]: several entries.
make_zip_n() {
    "$PY" -I -c '
import sys, zipfile
a = sys.argv[2:]
with zipfile.ZipFile(sys.argv[1], "w") as z:
    for i in range(0, len(a), 2):
        z.writestr(a[i], a[i + 1].encode("utf-8"))
' "$@"
}
# empty_zip <out.zip>: a valid zip with no entries (end-of-central-directory only).
empty_zip() { { printf 'PK\005\006'; head -c 18 /dev/zero; } > "$1"; }
# stub_dir <name> <script body>: a new directory with one executable stub
# <name>, for XPATH (put before the shared stubs on PATH).
stub_dir() {
    local d="$WORK/x-$1-$RANDOM$RANDOM"
    mkdir -p "$d"
    printf '#!/usr/bin/env bash\n%s\n' "$2" > "$d/$1"
    chmod +x "$d/$1"
    printf '%s' "$d"
}
MARKER_TEXT=$'# Third-party notices\r\n\r\nFFmpeg is not bundled. (Owner to\r\n   confirm this paragraph.)\r\n'
CLEAN_TEXT=$'# Third-party notices\r\n\r\nFFmpeg is not bundled; the owner confirmed this paragraph.\r\n'
MARKER_ZIP="$WORK/marker.zip"
if [ -n "$PY" ]; then
    make_zip "$MARKER_ZIP" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$MARKER_TEXT"
fi

# write_manifest <ffmpeg notice_confirmed JSON value | -> [extra tool JSON]:
# one installable tool (sha256 pinned) and one manual-only tool (sha256
# null, no notice_confirmed); "-" leaves ffmpeg's key out.
SHA64="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
write_manifest() {
    local conf="" extra=""
    [ "$1" = - ] || conf=", \"notice_confirmed\": $1"
    [ -n "${2:-}" ] && extra=", $2"
    mkdir -p "$REPO/tools"
    printf '{"schema": 1, "tools": [{"name": "ffmpeg", "sha256": "%s"%s}, {"name": "libewf", "sha256": null}%s]}\n' \
        "$SHA64" "$conf" "$extra" > "$REPO/tools/manifest.json"
}
# fresh_repo: a new fixture repo at $REPO with an empty dist/, a confirmed
# tools/manifest.json, a clean THIRD_PARTY_NOTICES.md and a LICENSE.
n=0
fresh_repo() {
    n=$((n + 1))
    REPO="$WORK/repo$n"
    mkdir -p "$REPO/scripts" "$REPO/target/release" "$REPO/dist"
    cp "$ROOT/scripts/build-release.sh" "$REPO/scripts/"
    printf '[package]\nname = "frametrace"\nversion = "9.9.9"\n' > "$REPO/Cargo.toml"
    echo readme > "$REPO/README.md"
    printf '%s' "$CLEAN_TEXT" > "$REPO/THIRD_PARTY_NOTICES.md"
    echo "MIT License (fixture)" > "$REPO/LICENSE"
    write_manifest true
    echo bin > "$REPO/target/release/frametrace"
    : > "$WORK/log$n"
}
# run_script [env...]: runs the copy; sets RC, OUT, CARGO_CALLS.
# XPATH (optional, colon-separated) goes before the shared stubs on PATH.
SKIP_DEFAULT=""
[ -n "$PY" ] || SKIP_DEFAULT=1
run_script() {
    OUT="$(env -u CARGO_TARGET_DIR PATH="${XPATH:+$XPATH:}$STUBS:$PATH" FT_LOG="$WORK/log$n" FT_MARKER_ZIP="$MARKER_ZIP" \
        FT_PY="$PY" FRAMETRACE_SKIP_NOTICES_CHECK="$SKIP_DEFAULT" "$@" \
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
    empty_zip "$REPO/dist/other-1.0-aarch64.zip"
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
empty_zip "$REPO/dist/other-1.0-aarch64.zip"
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
            # No reader: fail closed by default ...
            check "marker, reader none: exit 1 (cannot look inside)" [ "$RC" -eq 1 ]
            check "marker, reader none: cargo not run" [ "$CARGO_CALLS" -eq 0 ]
            check "marker, reader none: no SHA256SUMS" no_sums
            check "marker, reader none: says cannot verify" out_has "cannot verify — no zip reader"
            check "marker, reader none: names the override" out_has "FRAMETRACE_SKIP_NOTICES_CHECK=1"
            # ... and only warns with FRAMETRACE_SKIP_NOTICES_CHECK=1.
            fresh_repo
            cp "$MARKER_ZIP" "$REPO/dist/FrameTrace-9.9.9-win64.zip"
            run_script FRAMETRACE_ZIP_READER=none FRAMETRACE_SKIP_NOTICES_CHECK=1
            check "marker, reader none + SKIP=1: exit 0" [ "$RC" -eq 0 ]
            check "marker, reader none + SKIP=1: warns that it did not check" out_has "NOT checked for the 'Owner to confirm' marker"
            check "marker, reader none + SKIP=1: SHA256SUMS written" has_sums
        else
            check "marker, reader $label: exit 1" [ "$RC" -eq 1 ]
            check "marker, reader $label: cargo not run" [ "$CARGO_CALLS" -eq 0 ]
            check "marker, reader $label: no SHA256SUMS" no_sums
            check "marker, reader $label: names the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"
        fi
    done

    # duplicated entry name: a marked first copy behind a clean last copy.
    # zipfile's read(name) returns only the LAST copy, so a reader that
    # iterates namelist() would pass this zip as clean (round-10 review).
    dup_readers=("" python)
    if [ "$HAVE_UNZIP" -eq 1 ]; then dup_readers+=(unzip); fi
    for reader in "${dup_readers[@]}"; do
        label="${reader:-default}"
        fresh_repo
        make_zip_n "$REPO/dist/FrameTrace-9.9.9-win64.zip" \
            "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$MARKER_TEXT" \
            "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$CLEAN_TEXT" 2>/dev/null
        run_script FRAMETRACE_ZIP_READER="$reader"
        check "duplicate name (marker first), reader $label: exit 1" [ "$RC" -eq 1 ]
        check "duplicate name (marker first), reader $label: names the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"
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

    # confirmed notices pass and are listed (default reader and each one)
    for reader in "" python unzip; do
        [ "$reader" = unzip ] && [ "$HAVE_UNZIP" -eq 0 ] && continue
        label="${reader:-default}"
        fresh_repo
        make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$CLEAN_TEXT"
        run_script FRAMETRACE_ZIP_READER="$reader"
        check "confirmed notices, reader $label: exit 0" [ "$RC" -eq 0 ]
        if has_sums; then
            check "confirmed notices, reader $label: listed in SHA256SUMS" sums_has "FrameTrace-9.9.9-win64.zip"
        else
            bad "confirmed notices, reader $label: SHA256SUMS missing"
        fi
    done

    # an unreadable zip cannot be verified: refused, not passed as clean
    fresh_repo
    echo "not a zip" > "$REPO/dist/broken.zip"
    run_script
    check "unreadable zip: exit 1" [ "$RC" -eq 1 ]
    check "unreadable zip: cargo not run" [ "$CARGO_CALLS" -eq 0 ]
    check "unreadable zip: says every reader failed" out_has "broken.zip (THIRD_PARTY_NOTICES.md: cannot verify — every zip reader failed on it)"

    # an entry path with '[x]': `unzip -p` would read the name as a wildcard
    BRACKET_ENTRY="FrameTrace-9.9.9-win64[x]/THIRD_PARTY_NOTICES.md"
    fresh_repo
    make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "$BRACKET_ENTRY" "$MARKER_TEXT"
    run_script
    check "'[x]' entry, default reader: exit 1" [ "$RC" -eq 1 ]
    check "'[x]' entry, default reader: names the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"
    fresh_repo
    make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "$BRACKET_ENTRY" "$CLEAN_TEXT"
    run_script
    check "'[x]' entry, confirmed, default reader: exit 0" [ "$RC" -eq 0 ]
    if [ "$HAVE_UNZIP" -eq 1 ]; then
        for what in marker confirmed; do
            text="$MARKER_TEXT"; [ "$what" = confirmed ] && text="$CLEAN_TEXT"
            fresh_repo
            make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "$BRACKET_ENTRY" "$text"
            run_script FRAMETRACE_ZIP_READER=unzip
            check "'[x]' entry ($what), unzip only: exit 1 (cannot verify, not clean)" [ "$RC" -eq 1 ]
            check "'[x]' entry ($what), unzip only: no SHA256SUMS" no_sums
            check "'[x]' entry ($what), unzip only: says cannot verify" out_has "cannot verify — every zip reader failed on it"
        done
    else
        skp "'[x]' entry with the unzip reader (no unzip)"
    fi

    # a backslash separator (hand-made PS 5.1 Compress-Archive zips): Info-ZIP
    # lists it verbatim, so a basename compare on the raw name would skip the
    # notices and pass the zip as clean. The unzip reader must refuse it
    # (wildcard-unsafe name → cannot verify), python/bsdtar normalise and read.
    BS_ENTRY='FrameTrace-9.9.9-win64\THIRD_PARTY_NOTICES.md'
    fresh_repo
    make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "$BS_ENTRY" "$MARKER_TEXT"
    run_script
    check "backslash entry, default reader: exit 1" [ "$RC" -eq 1 ]
    check "backslash entry, default reader: names the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"
    if [ "$HAVE_UNZIP" -eq 1 ]; then
        # Two correct outcomes, depending on the unzip build: one that lists
        # the backslash verbatim must refuse (cannot verify); one that
        # normalises it to '/' reads the notices and judges them. The wrong
        # outcome — skipping the entry and calling the zip clean — is what
        # the marker run rules out.
        fresh_repo
        make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "$BS_ENTRY" "$MARKER_TEXT"
        run_script FRAMETRACE_ZIP_READER=unzip
        check "backslash entry (marker), unzip only: exit 1" [ "$RC" -eq 1 ]
        check "backslash entry (marker), unzip only: no SHA256SUMS" no_sums
        if out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"; then
            UNZIP_READS_BACKSLASH=1
        else
            UNZIP_READS_BACKSLASH=0
            check "backslash entry (marker), unzip only: says cannot verify" out_has "cannot verify — every zip reader failed on it"
        fi
        fresh_repo
        make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "$BS_ENTRY" "$CLEAN_TEXT"
        run_script FRAMETRACE_ZIP_READER=unzip
        if [ "$UNZIP_READS_BACKSLASH" -eq 1 ]; then
            check "backslash entry (confirmed), unzip normalises: exit 0" [ "$RC" -eq 0 ]
            check "backslash entry (confirmed), unzip normalises: SHA256SUMS written" has_sums
        else
            check "backslash entry (confirmed), unzip verbatim: exit 1 (cannot verify, not clean)" [ "$RC" -eq 1 ]
            check "backslash entry (confirmed), unzip verbatim: no SHA256SUMS" no_sums
        fi
    else
        skp "backslash entry with the unzip reader (no unzip)"
    fi

    # a failing reader: stub unzip that exits 1; Store-alias python stubs
    FAILING_UNZIP="$(stub_dir unzip 'echo "unzip: stub failure" >&2; exit 1')"
    STORE_MSG='echo "Python was not found; run without arguments to install from the Microsoft Store" >&2; exit 9009'
    STORE_PY3="$(stub_dir python3 "$STORE_MSG")"
    STORE_PY="$(stub_dir python "$STORE_MSG")"
    REAL_PY="$(stub_dir python "exec \"$PY\" \"\$@\"")"
    HAVE_BSDTAR=0
    case "$(tar --version 2>/dev/null)" in *[Bb][Ss][Dd][Tt][Aa][Rr]*) HAVE_BSDTAR=1 ;; esac
    for what in marker confirmed; do
        text="$MARKER_TEXT"; [ "$what" = confirmed ] && text="$CLEAN_TEXT"
        fresh_repo
        make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$text"
        XPATH="$FAILING_UNZIP" run_script FRAMETRACE_ZIP_READER=unzip
        check "failing unzip ($what), unzip only: exit 1 (cannot verify, not clean)" [ "$RC" -eq 1 ]
        check "failing unzip ($what), unzip only: cargo not run" [ "$CARGO_CALLS" -eq 0 ]
        check "failing unzip ($what), unzip only: says cannot verify" out_has "cannot verify — every zip reader failed on it"
        # no working python either (both Store stubs) + failing unzip: no
        # reader left that can read it (bsdtar exists only on macOS/BSD).
        if [ "$HAVE_BSDTAR" -eq 0 ]; then
            fresh_repo
            make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$text"
            XPATH="$STORE_PY3:$STORE_PY:$FAILING_UNZIP" run_script
            check "Store pythons + failing unzip ($what), default: exit 1" [ "$RC" -eq 1 ]
            check "Store pythons + failing unzip ($what), default: says cannot verify" out_has "cannot verify — every zip reader failed on it"
        else
            skp "Store pythons + failing unzip ($what) (bsdtar present)"
        fi
    done

    # python3 is the Microsoft Store alias stub (exit 9009): probed and
    # skipped, `python` reads the zip (unzip is failing, so a found marker
    # proves python did the reading).
    fresh_repo
    make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$MARKER_TEXT"
    XPATH="$STORE_PY3:$REAL_PY:$FAILING_UNZIP" run_script
    check "Store python3: exit 1" [ "$RC" -eq 1 ]
    check "Store python3: python found the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"
    fresh_repo
    make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$CLEAN_TEXT"
    XPATH="$STORE_PY3:$REAL_PY:$FAILING_UNZIP" run_script
    check "Store python3, confirmed: exit 0 via python" [ "$RC" -eq 0 ]
    # both pythons are Store stubs: falls to the next reader (real unzip)
    if [ "$HAVE_UNZIP" -eq 1 ]; then
        fresh_repo
        make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$MARKER_TEXT"
        XPATH="$STORE_PY3:$STORE_PY" run_script
        check "Store python3 + python: exit 1 via unzip" [ "$RC" -eq 1 ]
        check "Store python3 + python: unzip found the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"
        fresh_repo
        make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$CLEAN_TEXT"
        # (no python: node reads the manifest — Fix round 13 L3, the
        # override no longer skips it; unzip reads the zips)
        XPATH="$STORE_PY3:$STORE_PY" run_script FRAMETRACE_SKIP_NOTICES_CHECK=1
        if [ -n "$NODE" ]; then
            check "Store python3 + python, confirmed: exit 0 via unzip (manifest via node)" [ "$RC" -eq 0 ]
        else
            check "Store python3 + python, no node: exit 1 (manifest cannot be verified)" [ "$RC" -eq 1 ]
        fi
        check "Store python3 + python: no 'NOT checked' manifest warning any more" bash -c '! printf "%s" "$1" | grep -qF "notice_confirmed NOT checked"' _ "$OUT"
    else
        skp "Store python3 + python falling to unzip (no unzip)"
    fi
fi

# --- the bsdtar reader (FRAMETRACE_ZIP_READER=tar) ---------------------------
# Uses `tar` when it is bsdtar, else FRAMETRACE_TEST_BSDTAR (a stub `tar`
# that execs it goes first on PATH).
BSDTAR_DIR=""
if [ -n "${FRAMETRACE_TEST_BSDTAR:-}" ]; then
    case "$("$FRAMETRACE_TEST_BSDTAR" --version 2>/dev/null)" in
        *[Bb][Ss][Dd][Tt][Aa][Rr]*) BSDTAR_DIR="$(stub_dir tar "exec \"$FRAMETRACE_TEST_BSDTAR\" \"\$@\"")" ;;
        *) bad "FRAMETRACE_TEST_BSDTAR=$FRAMETRACE_TEST_BSDTAR is not bsdtar" ;;
    esac
fi
HAVE_TAR_READER=0
if [ -n "$BSDTAR_DIR" ]; then
    HAVE_TAR_READER=1
else
    case "$(tar --version 2>/dev/null)" in *[Bb][Ss][Dd][Tt][Aa][Rr]*) HAVE_TAR_READER=1 ;; esac
fi
if [ "$HAVE_TAR_READER" -eq 0 ]; then
    skp "bsdtar reader cases (no bsdtar; set FRAMETRACE_TEST_BSDTAR=<path>)"
elif [ -z "$PY" ]; then
    skp "bsdtar reader cases (no python3/python to build fixture zips)"
else
    # A zip without a notices entry is clean; the zip this script builds
    # carries the (clean) working-tree notices, read in the second check.
    fresh_repo
    make_zip "$REPO/dist/other-1.0-aarch64.zip" "other-1.0/README.md" "readme"
    XPATH="$BSDTAR_DIR" run_script FRAMETRACE_ZIP_READER=tar
    check "tar reader, our zip + a zip without notices: exit 0" [ "$RC" -eq 0 ]
    check "tar reader, our zip + a zip without notices: SHA256SUMS written" has_sums
    if has_sums; then
        check "tar reader, our zip + a zip without notices: sums list our zip" sums_has "frametrace-9.9.9-x86_64-unknown-linux-gnu.zip"
    fi

    fresh_repo
    cp "$MARKER_ZIP" "$REPO/dist/FrameTrace-9.9.9-win64.zip"
    XPATH="$BSDTAR_DIR" run_script FRAMETRACE_ZIP_READER=tar
    check "tar reader, marker: exit 1" [ "$RC" -eq 1 ]
    check "tar reader, marker: cargo not run" [ "$CARGO_CALLS" -eq 0 ]
    check "tar reader, marker: names the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"

    # planted during the build: the second check reads it with tar too
    fresh_repo
    XPATH="$BSDTAR_DIR" run_script FRAMETRACE_ZIP_READER=tar PLANT=marker
    check "tar reader, planted marker zip: exit 1" [ "$RC" -eq 1 ]
    check "tar reader, planted marker zip: cargo ran once" [ "$CARGO_CALLS" -eq 1 ]
    check "tar reader, planted marker zip: names the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"

    fresh_repo
    make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$CLEAN_TEXT"
    XPATH="$BSDTAR_DIR" run_script FRAMETRACE_ZIP_READER=tar
    check "tar reader, confirmed notices: exit 0" [ "$RC" -eq 0 ]

    # upper-case clean entry + lower-case marker entry: both are read
    fresh_repo
    make_zip_n "$REPO/dist/FrameTrace-9.9.9-win64.zip" \
        "FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md" "$CLEAN_TEXT" \
        "FrameTrace-9.9.9-win64/docs/third_party_notices.md" "$MARKER_TEXT"
    XPATH="$BSDTAR_DIR" run_script FRAMETRACE_ZIP_READER=tar
    check "tar reader, upper clean + lower marker: exit 1" [ "$RC" -eq 1 ]
    check "tar reader, upper clean + lower marker: names the marker" out_has "THIRD_PARTY_NOTICES.md: Owner to confirm"

    # '[x]' in the entry path: bsdtar reads name arguments as patterns too
    fresh_repo
    make_zip "$REPO/dist/FrameTrace-9.9.9-win64.zip" "FrameTrace-9.9.9-win64[x]/THIRD_PARTY_NOTICES.md" "$CLEAN_TEXT"
    XPATH="$BSDTAR_DIR" run_script FRAMETRACE_ZIP_READER=tar
    check "tar reader, '[x]' entry: exit 1 (cannot verify, not clean)" [ "$RC" -eq 1 ]
    check "tar reader, '[x]' entry: says cannot verify" out_has "cannot verify — every zip reader failed on it"

    fresh_repo
    echo "not a zip" > "$REPO/dist/broken.zip"
    XPATH="$BSDTAR_DIR" run_script FRAMETRACE_ZIP_READER=tar
    check "tar reader, unreadable zip: exit 1" [ "$RC" -eq 1 ]
    check "tar reader, unreadable zip: says cannot verify" out_has "cannot verify — every zip reader failed on it"
fi

# --- the working tree: tools/manifest.json + THIRD_PARTY_NOTICES.md ---------
OUR_ZIP_NAME="frametrace-9.9.9-x86_64-unknown-linux-gnu"
# refused_src <label>: exit 1, cargo not run, no zip, no SHA256SUMS, message.
refused_src() {
    check "$1: exit 1" [ "$RC" -eq 1 ]
    check "$1: cargo not run" [ "$CARGO_CALLS" -eq 0 ]
    check "$1: no SHA256SUMS" no_sums
    check "$1: no zip built" [ ! -f "$REPO/dist/$OUR_ZIP_NAME.zip" ]
    check "$1: says not releasable" out_has "the working tree is not releasable — NOT FOR RELEASE"
}
if [ -z "$PY" ]; then
    skp "manifest notice_confirmed cases (no python3/python)"
else
    fresh_repo; write_manifest false; run_script
    refused_src "manifest notice_confirmed false"
    check "manifest notice_confirmed false: names ffmpeg" out_has "tools/manifest.json: notice_confirmed is not true for: ffmpeg"

    fresh_repo; write_manifest -; run_script
    refused_src "manifest notice_confirmed missing"
    check "manifest notice_confirmed missing: names ffmpeg" out_has "notice_confirmed is not true for: ffmpeg"

    fresh_repo; write_manifest '"true"'; run_script
    refused_src 'manifest notice_confirmed "true" (string)'
    check 'manifest notice_confirmed "true" (string): names ffmpeg' out_has "notice_confirmed is not true for: ffmpeg"

    fresh_repo; write_manifest 1; run_script
    refused_src "manifest notice_confirmed 1 (number)"

    # a second installable tool, unconfirmed: the rule is per tool
    fresh_repo; write_manifest true '{"name": "other", "sha256": "bb", "notice_confirmed": false}'; run_script
    refused_src "manifest, second installable tool unconfirmed"
    check "manifest, second installable tool unconfirmed: names only it" out_has "notice_confirmed is not true for: other"

    # sha256 "" is not installable (make-portable.ps1's rule): no notice needed
    fresh_repo; write_manifest true '{"name": "manual", "sha256": ""}'; run_script
    check 'manifest, sha256 "" tool without notice_confirmed: exit 0' [ "$RC" -eq 0 ]

    fresh_repo; rm -f "$REPO/tools/manifest.json"; run_script
    refused_src "manifest missing"
    check "manifest missing: says not found" out_has "tools/manifest.json: not found"

    fresh_repo; echo '{"tools": [' > "$REPO/tools/manifest.json"; run_script
    refused_src "manifest not JSON"
    check "manifest not JSON: says cannot verify" out_has "tools/manifest.json: cannot verify"

    # the repository's own manifest
    fresh_repo; cp "$ROOT/tools/manifest.json" "$REPO/tools/manifest.json"; run_script
    if "$PY" -I -c 'import json,sys; m=json.load(open(sys.argv[1],encoding="utf-8-sig")); sys.exit(0 if all(t.get("notice_confirmed") is True for t in m["tools"] if t.get("sha256") not in (None, "")) else 1)' "$ROOT/tools/manifest.json"; then
        check "real tools/manifest.json (confirmed): exit 0" [ "$RC" -eq 0 ]
    else
        refused_src "real tools/manifest.json (unconfirmed)"
        check "real tools/manifest.json (unconfirmed): names ffmpeg" out_has "notice_confirmed is not true for: ffmpeg"
    fi

    # Fix round 13 L3: python's own error is printed with the refusal
    fresh_repo; echo '{"tools": [' > "$REPO/tools/manifest.json"; run_script
    check "manifest not JSON (python): prints the cause" out_has "python: JSONDecodeError"
    fresh_repo; printf '\357\273\277' > "$REPO/tools/manifest.json"; printf '{"tools": [{"name": "ffmpeg", "sha256": "%s", "notice_confirmed": true}]}\n' "$SHA64" >> "$REPO/tools/manifest.json"; run_script
    check "manifest with a UTF-8 BOM (python): exit 0" [ "$RC" -eq 0 ]
    # ... and the override never skips a verdict python could give
    fresh_repo; write_manifest false; run_script FRAMETRACE_SKIP_NOTICES_CHECK=1
    refused_src "manifest notice_confirmed false + SKIP=1"
fi

# --- the manifest without python: node, else refused with no override --------
NOPY3="$(stub_dir python3 'exit 9009')"
NOPY="$(stub_dir python 'exit 9009')"
NONODE="$(stub_dir node 'exit 127')"
if [ -n "$NODE" ]; then
    NOPYS="$NOPY3:$NOPY"
    fresh_repo; XPATH="$NOPYS" run_script
    check "manifest via node (no python), confirmed: exit 0" [ "$RC" -eq 0 ]
    check "manifest via node (no python), confirmed: cargo ran once" [ "$CARGO_CALLS" -eq 1 ]
    check "manifest via node (no python), confirmed: no 'NOT checked' warning" bash -c '! printf "%s" "$1" | grep -qF "notice_confirmed NOT checked"' _ "$OUT"
    fresh_repo; write_manifest false; XPATH="$NOPYS" run_script
    refused_src "manifest via node, notice_confirmed false"
    check "manifest via node, notice_confirmed false: names ffmpeg" out_has "tools/manifest.json: notice_confirmed is not true for: ffmpeg"
    fresh_repo; write_manifest -; XPATH="$NOPYS" run_script
    refused_src "manifest via node, notice_confirmed missing"
    fresh_repo; write_manifest '"true"'; XPATH="$NOPYS" run_script
    refused_src 'manifest via node, notice_confirmed "true" (string)'
    fresh_repo; write_manifest 1; XPATH="$NOPYS" run_script
    refused_src "manifest via node, notice_confirmed 1 (number)"
    fresh_repo; write_manifest true '{"name": "other", "sha256": "bb", "notice_confirmed": false}'; XPATH="$NOPYS" run_script
    refused_src "manifest via node, second installable tool unconfirmed"
    check "manifest via node, second installable tool unconfirmed: names only it" out_has "notice_confirmed is not true for: other"
    fresh_repo; write_manifest true '{"name": "manual", "sha256": ""}'; XPATH="$NOPYS" run_script
    check 'manifest via node, sha256 "" tool without notice_confirmed: exit 0' [ "$RC" -eq 0 ]
    fresh_repo; printf '\357\273\277' > "$REPO/tools/manifest.json"; printf '{"tools": [{"name": "ffmpeg", "sha256": "%s", "notice_confirmed": true}]}\n' "$SHA64" >> "$REPO/tools/manifest.json"; XPATH="$NOPYS" run_script
    check "manifest via node with a UTF-8 BOM: exit 0" [ "$RC" -eq 0 ]
    fresh_repo; echo '{"tools": [' > "$REPO/tools/manifest.json"; XPATH="$NOPYS" run_script
    refused_src "manifest via node, not JSON"
    check "manifest via node, not JSON: says cannot verify" out_has "tools/manifest.json: cannot verify — not readable as a tool manifest"
    check "manifest via node, not JSON: prints node's cause" out_has "(node: "
    fresh_repo; echo '{"schema": 1}' > "$REPO/tools/manifest.json"; XPATH="$NOPYS" run_script
    refused_src "manifest via node, no tools array"
    check "manifest via node, no tools array: prints the cause" out_has 'no "tools" array'
    fresh_repo; write_manifest false; XPATH="$NOPYS" run_script FRAMETRACE_SKIP_NOTICES_CHECK=1
    refused_src "manifest via node, false + SKIP=1"
    # the repository's own manifest: the same verdict as python's
    fresh_repo; cp "$ROOT/tools/manifest.json" "$REPO/tools/manifest.json"; XPATH="$NOPYS" run_script
    if node -e 'const m=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8").replace(/^\uFEFF/,""));process.exit(m.tools.every(t=>t.sha256==null||t.sha256===""||t.notice_confirmed===true)?0:1)' "$ROOT/tools/manifest.json"; then
        check "real tools/manifest.json via node (confirmed): exit 0" [ "$RC" -eq 0 ]
    else
        refused_src "real tools/manifest.json via node (unconfirmed)"
        check "real tools/manifest.json via node (unconfirmed): names ffmpeg" out_has "notice_confirmed is not true for: ffmpeg"
    fi
else
    skp "manifest via node cases (no node)"
fi
# neither python nor node: fails closed, and FRAMETRACE_SKIP_NOTICES_CHECK=1
# does not change that (Fix round 13 L3)
fresh_repo; XPATH="$NOPY3:$NOPY:$NONODE" run_script
refused_src "manifest, no python and no node"
check "manifest, no python and no node: says cannot verify" out_has "tools/manifest.json: cannot verify — no python3/python or node"
fresh_repo; XPATH="$NOPY3:$NOPY:$NONODE" run_script FRAMETRACE_SKIP_NOTICES_CHECK=1
refused_src "manifest, no python and no node + SKIP=1 (no override)"
check "manifest, no python and no node + SKIP=1: no 'NOT checked' warning" bash -c '! printf "%s" "$1" | grep -qF "notice_confirmed NOT checked"' _ "$OUT"

# THIRD_PARTY_NOTICES.md in the working tree (no python needed)
fresh_repo; printf '%s' "$MARKER_TEXT" > "$REPO/THIRD_PARTY_NOTICES.md"; run_script
refused_src "working-tree notices marker (re-wrapped, CRLF)"
check "working-tree notices marker: names it" out_has "THIRD_PARTY_NOTICES.md: still says 'Owner to confirm'"
fresh_repo; printf '%s' "$MARKER_TEXT" > "$REPO/THIRD_PARTY_NOTICES.md"; run_script FRAMETRACE_SKIP_NOTICES_CHECK=1
refused_src "working-tree notices marker + SKIP=1"
fresh_repo; rm -f "$REPO/THIRD_PARTY_NOTICES.md"; run_script
refused_src "working-tree notices missing"
if [ -n "$PY" ]; then
    fresh_repo; write_manifest false; printf '%s' "$MARKER_TEXT" > "$REPO/THIRD_PARTY_NOTICES.md"; run_script
    refused_src "manifest + notices both unconfirmed"
    check "manifest + notices both unconfirmed: names the manifest" out_has "notice_confirmed is not true for: ffmpeg"
    check "manifest + notices both unconfirmed: names the notices" out_has "still says 'Owner to confirm'"
fi

# confirmed + clean: the zip carries THIRD_PARTY_NOTICES.md and LICENSE
fresh_repo; run_script
check "confirmed + clean notices: exit 0" [ "$RC" -eq 0 ]
check "confirmed + clean notices: cargo ran once" [ "$CARGO_CALLS" -eq 1 ]
check "confirmed + clean notices: SHA256SUMS written" has_sums
check "confirmed + clean notices: staged THIRD_PARTY_NOTICES.md" [ -f "$REPO/dist/$OUR_ZIP_NAME/THIRD_PARTY_NOTICES.md" ]
check "confirmed + clean notices: staged LICENSE" [ -f "$REPO/dist/$OUR_ZIP_NAME/LICENSE" ]
if [ -n "$PY" ]; then
    ENTRIES="$("$PY" -I -c 'import sys, zipfile; print("\n".join(zipfile.ZipFile(sys.argv[1]).namelist()))' "$REPO/dist/$OUR_ZIP_NAME.zip" 2>&1 | tr -d '\r')"
    for e in README.md THIRD_PARTY_NOTICES.md LICENSE frametrace; do
        check "confirmed + clean notices: zip has $e" bash -c 'printf "%s\n" "$1" | grep -qxF -- "$2"' _ "$ENTRIES" "$OUR_ZIP_NAME/$e"
    done
else
    skp "zip entry listing (no python3/python)"
fi

# --- CARGO_TARGET_DIR (Fix round 13 H1) ----------------------------------------
# fresh_repo leaves a STALE target/release/frametrace ("bin"); with the
# variable set, the binary cargo built there is packaged, never the stale one.
fresh_repo; run_script CARGO_TARGET_DIR="$WORK/ctd$n"
check "CARGO_TARGET_DIR absolute: exit 0" [ "$RC" -eq 0 ]
check "CARGO_TARGET_DIR absolute: packages the fresh binary" grep -qx fresh-from-cargo "$REPO/dist/$OUR_ZIP_NAME/frametrace"
fresh_repo; run_script CARGO_TARGET_DIR=out/ct
check "CARGO_TARGET_DIR relative: exit 0" [ "$RC" -eq 0 ]
check "CARGO_TARGET_DIR relative: cargo built under the repo root" [ -f "$REPO/out/ct/release/frametrace" ]
check "CARGO_TARGET_DIR relative: packages the fresh binary" grep -qx fresh-from-cargo "$REPO/dist/$OUR_ZIP_NAME/frametrace"
# cargo wrote nothing there: the stale target/release one is not taken
fresh_repo; run_script CARGO_TARGET_DIR="$WORK/ctd-empty$n" FT_CARGO_NOWRITE=1
check "CARGO_TARGET_DIR without a binary: fails" [ "$RC" -ne 0 ]
check "CARGO_TARGET_DIR without a binary: no zip" [ ! -f "$REPO/dist/$OUR_ZIP_NAME.zip" ]
check "CARGO_TARGET_DIR without a binary: no SHA256SUMS" no_sums
# unset: target/release as before
fresh_repo; run_script
check "no CARGO_TARGET_DIR: packages target/release/frametrace" grep -qx bin "$REPO/dist/$OUR_ZIP_NAME/frametrace"

# --- --help prints the header only -------------------------------------------
fresh_repo
run_script
OUT="$(bash "$REPO/scripts/build-release.sh" --help 2>&1)"
check "--help: names make-portable.ps1" out_has "scripts/make-portable.ps1, which writes the same SHA256SUMS."
check "--help: stops before code" bash -c '! printf "%s" "$1" | grep -q "set -euo"' _ "$OUT"

echo
echo "build-release fixture: $pass ok, $fail failed, $skip skipped"
[ "$skip" -eq 0 ] || echo "SKIPPED $skip (see the 'skip' lines above)"
[ "$fail" -eq 0 ] || exit 1
if [ "$skip" -eq 0 ]; then echo "all checks passed"; else echo "all run checks passed ($skip SKIPPED)"; fi
