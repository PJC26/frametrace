#!/usr/bin/env bash
# Builds the FrameTrace release binaries and packages them as
#   dist/frametrace-<version>-<target-triple>.zip
# plus a dist/SHA256SUMS manifest covering every zip in dist/ (refuses to
# run while dist/ holds a Windows *UNCONFIRMED*.zip test package or any
# zip whose THIRD_PARTY_NOTICES.md says "Owner to confirm").
#
# Options:
#   --sign   also runs `cosign sign-blob` on the zip and SHA256SUMS
#            (requires cosign on PATH; keyless OIDC or -key, see
#            docs/repro-build.md)
#   --sbom   also emits dist/frametrace-<ver>-<target>.sbom.cyclonedx.json
#            via `cargo sbom` (requires cargo-sbom; see docs/repro-build.md)
#
# The script deliberately adds no build infrastructure: it is the
# documented release procedure made repeatable. Windows packaging lives
# in scripts/make-portable.ps1, which writes the same SHA256SUMS.
set -euo pipefail

cd "$(dirname "$0")/.."

SIGN=0
SBOM=0
for arg in "$@"; do
    case "$arg" in
        --sign) SIGN=1 ;;
        --sbom) SBOM=1 ;;
        -h|--help)
            sed -n '2,17p' "$0"
            exit 0
            ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

# A Windows test package (make-portable.ps1 -AllowUnconfirmedNotices:
# dist/*-UNCONFIRMED.zip, THIRD_PARTY_NOTICES not confirmed by the owner)
# must never reach a release run: it would be listed in SHA256SUMS and
# signed with it. Checked before the build and again before the sums, by
# name (case-insensitive *unconfirmed*.zip, so browser/Explorer copies
# such as "...-UNCONFIRMED (1).zip" or "...-UNCONFIRMED - 복사본.zip" are
# caught too) and by content: any dist/*.zip whose THIRD_PARTY_NOTICES.md
# still carries the "(Owner to confirm ...)" marker, whatever it is called.
#
# FRAMETRACE_ZIP_READER (unzip|python|tar|none) forces the zip reader;
# it exists for scripts/test-build-release.sh. Default: the first that works.

# Prints the THIRD_PARTY_NOTICES.md entries of zip $1 to stdout (nothing
# if the zip has none). Returns 3 when no zip reader is available.
notices_of_zip() {
    local zip="$1" reader="${FRAMETRACE_ZIP_READER:-}" py
    if [ -z "$reader" ]; then
        if command -v unzip >/dev/null 2>&1; then
            reader=unzip
        elif command -v python3 >/dev/null 2>&1 || command -v python >/dev/null 2>&1; then
            reader=python
        elif tar --version 2>/dev/null | grep -qi bsdtar; then
            reader=tar
        else
            reader=none
        fi
    fi
    case "$reader" in
        unzip)
            # List the entries and pick the notices by basename (any case,
            # any folder) — unzip's own wildcards do not reliably cross '/'.
            local entry base
            while IFS= read -r entry; do
                base="$(printf '%s' "${entry##*/}" | tr '[:upper:]' '[:lower:]')"
                [ "$base" = "third_party_notices.md" ] || continue
                unzip -p "$zip" "$entry" 2>/dev/null || true
                echo
            done < <(unzip -Z1 "$zip" 2>/dev/null || true)
            ;;
        python)
            py="$(command -v python3 2>/dev/null || command -v python 2>/dev/null || true)"
            [ -n "$py" ] || return 3
            "$py" -I -c '
import sys, zipfile
try:
    with zipfile.ZipFile(sys.argv[1]) as z:
        for n in z.namelist():
            if n.replace("\\", "/").rsplit("/", 1)[-1].lower() == "third_party_notices.md":
                sys.stdout.buffer.write(z.read(n) + b"\n")
except zipfile.BadZipFile:
    pass
' "$zip" 2>/dev/null || true
            ;;
        tar)
            # bsdtar (macOS/BSD) reads zip archives; GNU tar does not.
            tar -xOf "$zip" --include='*THIRD_PARTY_NOTICES.md' 2>/dev/null \
                || tar -xOf "$zip" '*THIRD_PARTY_NOTICES.md' 2>/dev/null || true
            ;;
        *)
            return 3
            ;;
    esac
}

refuse_unconfirmed() {
    local found="" zip lower notices warned=0 rc
    if [ -d dist ]; then
        for zip in dist/*; do
            [ -f "$zip" ] || continue
            lower="$(printf '%s' "${zip##*/}" | tr '[:upper:]' '[:lower:]')"
            case "$lower" in
                *.zip) ;;
                *) continue ;;
            esac
            case "$lower" in
                *unconfirmed*.zip)
                    found="${found}${zip} (name)"$'\n'
                    continue
                    ;;
            esac
            rc=0
            notices="$(notices_of_zip "$zip")" || rc=$?
            if [ "$rc" -eq 3 ]; then
                if [ "$warned" -eq 0 ]; then
                    echo "warning: no zip reader (unzip, python3, bsdtar) — THIRD_PARTY_NOTICES.md inside dist/*.zip NOT checked for the 'Owner to confirm' marker" >&2
                    warned=1
                fi
                continue
            fi
            # The marker may be re-wrapped across lines: fold whitespace and
            # case first, then match in bash (no `grep -q` + pipefail SIGPIPE).
            notices="$(printf '%s' "$notices" | tr '\r\n\t' '   ' | tr '[:upper:]' '[:lower:]')"
            if [[ "$notices" =~ owner[[:space:]]+to[[:space:]]+confirm ]]; then
                found="${found}${zip} (THIRD_PARTY_NOTICES.md: Owner to confirm)"$'\n'
            fi
        done
    fi
    if [ -n "$found" ]; then
        {
            echo "error: dist/ holds an UNCONFIRMED test package — NOT FOR RELEASE:"
            printf '%s' "$found" | sed 's/^/  /'
            echo "remove it (and SHA256SUMS.unconfirmed) and run again; no SHA256SUMS was written and nothing was signed."
        } >&2
        exit 1
    fi
}
refuse_unconfirmed

# Pinned toolchain + locked dependencies: the two inputs that decide the
# build (see docs/repro-build.md).
cargo build --release --locked

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TARGET="$(rustc -vV | sed -n 's/^host: //p')"
ARTIFACT="frametrace-${VERSION}-${TARGET}"
STAGE="dist/${ARTIFACT}"

rm -rf "$STAGE"
mkdir -p "$STAGE"
cp target/release/frametrace "$STAGE/"
cp target/release/frametrace-app "$STAGE/" 2>/dev/null || true
cp README.md "$STAGE/"

rm -f "dist/${ARTIFACT}.zip"
(cd dist && zip -qr "${ARTIFACT}.zip" "$ARTIFACT")

# SHA256SUMS covers every zip present in dist/, so a multi-target release
# directory accumulates one manifest consumers can `sha256sum -c` — never
# an UNCONFIRMED test package (refused above; excluded here regardless).
refuse_unconfirmed
(
    cd dist
    zips=()
    for zip in ./*.zip; do
        [ -f "$zip" ] || continue
        case "$(printf '%s' "$zip" | tr '[:upper:]' '[:lower:]')" in
            *unconfirmed*.zip) continue ;;
        esac
        zips+=("$zip")
    done
    shasum -a 256 -- "${zips[@]}" | sed 's|\./||' > SHA256SUMS
)

if [ "$SBOM" -eq 1 ]; then
    command -v cargo-sbom >/dev/null 2>&1 || {
        echo "cargo-sbom not found; install with: cargo install cargo-sbom --locked" >&2
        exit 1
    }
    cargo sbom > "dist/${ARTIFACT}.sbom.cyclonedx.json"
fi

if [ "$SIGN" -eq 1 ]; then
    command -v cosign >/dev/null 2>&1 || {
        echo "cosign not found; see docs/repro-build.md#signing" >&2
        exit 1
    }
    if [ -n "${COSIGN_KEY:-}" ]; then
        # Local-key mode (offline): COSIGN_KEY points at a cosign private key,
        # COSIGN_PASSWORD unlocks it. Uses a signing config without a
        # transparency-log service so no Rekor upload is attempted; the bundle
        # still verifies offline via `cosign verify-blob --insecure-ignore-tlog`.
        CONFIG="${COSIGN_SIGNING_CONFIG:-}"
        if [ -z "$CONFIG" ]; then
            CONFIG="$(mktemp -t ft-signing-config).json"
            curl -fsSL https://raw.githubusercontent.com/sigstore/root-signing/refs/heads/main/targets/signing_config.v0.2.json \
                | python3 -c 'import json,sys; c=json.load(sys.stdin); c.pop("rekorTlogUrls",None); c.pop("rekorTlogConfig",None); json.dump(c,sys.stdout)' \
                > "$CONFIG"
        fi
        cosign sign-blob --yes --key "$COSIGN_KEY" --signing-config "$CONFIG" \
            --bundle "dist/${ARTIFACT}.zip.sigstore.json" \
            "dist/${ARTIFACT}.zip"
        cosign sign-blob --yes --key "$COSIGN_KEY" --signing-config "$CONFIG" \
            --bundle "dist/SHA256SUMS.sigstore.json" \
            "dist/SHA256SUMS"
    else
        # Keyless mode: OIDC identity + Rekor transparency log (needs network).
        cosign sign-blob --yes \
            --bundle "dist/${ARTIFACT}.zip.sigstore.json" \
            "dist/${ARTIFACT}.zip"
        cosign sign-blob --yes \
            --bundle "dist/SHA256SUMS.sigstore.json" \
            "dist/SHA256SUMS"
    fi
fi

echo "package: dist/${ARTIFACT}.zip"
echo "sums:    dist/SHA256SUMS"
[ "$SBOM" -eq 1 ] && echo "sbom:    dist/${ARTIFACT}.sbom.cyclonedx.json"
[ "$SIGN" -eq 1 ] && echo "sigs:    dist/${ARTIFACT}.zip.sigstore.json, dist/SHA256SUMS.sigstore.json"
exit 0
