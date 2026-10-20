#!/usr/bin/env bash
# Builds the FrameTrace release binaries and packages them as
#   dist/frametrace-<version>-<target-triple>.zip
# plus a dist/SHA256SUMS manifest covering every zip in dist/ (refuses to
# run while dist/ holds a Windows *-UNCONFIRMED.zip test package).
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
            sed -n '2,16p' "$0"
            exit 0
            ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

# A Windows test package (make-portable.ps1 -AllowUnconfirmedNotices:
# dist/*-UNCONFIRMED.zip, THIRD_PARTY_NOTICES not confirmed by the owner)
# must never reach a release run: it would be listed in SHA256SUMS and
# signed with it. Checked before the build and again before the sums.
refuse_unconfirmed() {
    local found
    found="$(find dist -maxdepth 1 -type f -iname '*-UNCONFIRMED.zip' 2>/dev/null || true)"
    if [ -n "$found" ]; then
        {
            echo "error: dist/ holds an UNCONFIRMED test package — NOT FOR RELEASE:"
            echo "$found" | sed 's/^/  /'
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
            *-unconfirmed.zip) continue ;;
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
