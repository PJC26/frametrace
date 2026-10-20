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
# fixture zips and as the script's first zip reader; without it the marker
# cases are skipped and the rest run with FRAMETRACE_SKIP_NOTICES_CHECK=1),
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
# zip -qr <out.zip> <dir>: a real (readable) zip with one README entry, so
# the content check before SHA256SUMS can read it; an empty zip without
# python.
if [ -n "${FT_PY:-}" ]; then
    "$FT_PY" -I -c '
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w") as z:
    z.writestr(sys.argv[2] + "/README.md", "stub\n")
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
# XPATH (optional, colon-separated) goes before the shared stubs on PATH.
SKIP_DEFAULT=""
[ -n "$PY" ] || SKIP_DEFAULT=1
run_script() {
    OUT="$(env PATH="${XPATH:+$XPATH:}$STUBS:$PATH" FT_LOG="$WORK/log$n" FT_MARKER_ZIP="$MARKER_ZIP" \
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
        XPATH="$STORE_PY3:$STORE_PY" run_script
        check "Store python3 + python, confirmed: exit 0 via unzip" [ "$RC" -eq 0 ]
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
    # The zips this script builds hold only binaries + README: no notices
    # entry is clean, in the first check and the second.
    fresh_repo
    make_zip "$REPO/dist/other-1.0-aarch64.zip" "other-1.0/README.md" "readme"
    XPATH="$BSDTAR_DIR" run_script FRAMETRACE_ZIP_READER=tar
    check "tar reader, zips without notices: exit 0" [ "$RC" -eq 0 ]
    check "tar reader, zips without notices: SHA256SUMS written" has_sums
    if has_sums; then
        check "tar reader, zips without notices: sums list our zip" sums_has "frametrace-9.9.9-x86_64-unknown-linux-gnu.zip"
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
