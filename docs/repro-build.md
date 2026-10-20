# Reproducible Build & Release Procedure

This document is the release checklist for FrameTrace binaries: how the
released artifact is built, how its integrity is anchored (SHA256SUMS +
optional signature), how to emit an SBOM, and how to build from a vendored
source tree. It is procedure, not new infrastructure — the only code added
is `scripts/build-release.sh` (Unix) and the SHA256SUMS step in
`scripts/make-portable.ps1` (Windows).

## Inputs that decide the build

| Input | Where pinned |
| --- | --- |
| Rust toolchain | `rust-toolchain.toml` — channel `1.94.0`, components `clippy`, `rustfmt` |
| Dependency graph | `Cargo.lock` — every build command below passes `--locked` |
| Compiler/target | `rustc -vV` host triple, baked into the artifact name |

Rebuilding a release means: same pinned toolchain, same lockfile, same
target. FrameTrace does **not** currently assert bit-for-bit
reproducibility across machines or toolchain builds — the published
SHA-256 manifest plus a signature is the integrity anchor, not a
rebuild-everywhere guarantee. If byte-identical rebuilds become a
requirement, the first gaps to close are documented below under
"Caveats".

## Building a release

```sh
scripts/build-release.sh            # zip + SHA256SUMS
scripts/build-release.sh --sbom     # + CycloneDX SBOM (needs cargo-sbom)
scripts/build-release.sh --sign     # + cosign sign-blob bundles
```

produces:

```
dist/frametrace-<version>-<target>.zip
dist/SHA256SUMS                     # covers every zip in dist/
```

`build-release.sh` refuses to run (exit 1, before building and again
before writing SHA256SUMS) while `dist/` holds an UNCONFIRMED test package
from `make-portable.ps1 -AllowUnconfirmedNotices`: any zip whose name
contains `unconfirmed` in any case (so a browser/Explorer copy such as
`…-UNCONFIRMED (1).zip` or `…-UNCONFIRMED - 복사본.zip` counts), or any zip
whose `THIRD_PARTY_NOTICES.md` still matches `Owner\s+to\s+confirm`
(read with Python's `zipfile` — `python3`, else `python`, each probed with
`import zipfile` so the Microsoft Store alias stub is skipped — else
`unzip -Z1`/`-p`, else bsdtar). The content check fails closed: a reader
that exits non-zero on a zip hands it to the next one, and a zip that no
available reader can read (broken, encrypted, unsupported compression) is
refused as "cannot verify", never passed as clean. With no reader at all
the run also stops (exit 1); `FRAMETRACE_SKIP_NOTICES_CHECK=1` lets it
proceed with a warning and the name check only. It never lists such a name in
SHA256SUMS — so `--sign` can never cosign it. Fixture test (stub
cargo/rustc/zip, no build): `bash scripts/test-build-release.sh`.

On Windows, `scripts/make-portable.ps1` already stages the portable
layout (`tools/bin` for external tools); it now also writes
`dist/SHA256SUMS` with the same `sha256sum -c`-compatible format — listing
only the zip that run wrote (it fails when another zip in `dist/` contains
FFmpeg). It also refuses to package while `THIRD_PARTY_NOTICES.md` still
contains "Owner to confirm" (a paragraph the owner has not confirmed);
`-AllowUnconfirmedNotices` overrides that for CI test packages only, never
for a release: such a package is named `FrameTrace-<ver>-win64-UNCONFIRMED.zip`,
gets `dist/SHA256SUMS.unconfirmed` instead of `dist/SHA256SUMS` (a stale
`SHA256SUMS` is removed), and the CI artifact is `FrameTrace-win64-unconfirmed`.
Every staged file is also scanned for FFmpeg signature strings (a renamed
copy is refused whatever its name or extension).

## Verifying a release

```sh
cd dist
sha256sum -c SHA256SUMS             # or: shasum -a 256 -c SHA256SUMS
```

With a cosign bundle produced by `--sign`:

```sh
cosign verify-blob --bundle frametrace-<ver>-<target>.zip.sigstore.json \
    --certificate-identity <release-identity> \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    frametrace-<ver>-<target>.zip
```

For releases cut on GitHub Actions, prefer artifact attestation over raw
cosign: add `actions/attest-build-provenance@v2` to the release workflow
and consumers verify with `gh attestation verify frametrace-<ver>-<target>.zip
--repo <org>/<repo>`. Both options are documented alternatives; neither is
required to produce the zip + SHA256SUMS baseline.

## SBOM

`cargo-sbom` is the reasonable choice: maintained, emits CycloneDX JSON,
walks `Cargo.lock` so it documents exactly what `--locked` built:

```sh
cargo install cargo-sbom --locked
cargo sbom > dist/frametrace-<ver>-<target>.sbom.cyclonedx.json
```

The `--sbom` flag of `build-release.sh` does exactly this. A lighter
fallback when cargo-sbom cannot be installed: `cargo tree --locked
--format "{p}"` enumerates the resolved dependency graph — less
structured than CycloneDX, but sufficient for an examiner to diff two
builds' inputs.

## Vendored (offline) build

For air-gapped or archive-preservation builds:

```sh
cargo vendor --locked vendor/ > vendor-config.toml
# append vendor-config.toml to .cargo/config.toml, then:
cargo build --release --locked --offline
```

Commit or archive `vendor/` + `Cargo.lock` + `rust-toolchain.toml`
alongside the release record; that triplet is the complete build input.

## Publication practice

1. Cut the release from a clean `main` checkout (`git status` empty).
2. Run `scripts/build-release.sh --sbom --sign` per platform (or the
   `.ps1` equivalent on Windows).
3. Publish the zips, `SHA256SUMS`, the `.sigstore.json` bundles (or the
   GitHub attestation), and the SBOMs together — the manifest is only
   trustworthy when its signature travels with it.
4. Record the expected binary SHA-256 in the release notes themselves so
   a consumer who never fetches dist/ can still compare a hash quoted in
   release text against a downloaded binary.

## Caveats

- **Not bit-for-bit reproducible yet.** `rustc` embeds path-dependent
  and occasionally nondeterministic metadata; same toolchain + lockfile
  on the same target *should* converge, but nobody has run the two-machine
  comparison. `qa consistency` (in-repo) is the pragmatic check today:
  same inputs → same outputs at the *product* level.
- **SBOM covers the Rust crate graph only.** Runtime external tools
  (ffmpeg, libewf, Sleuth Kit) are out-of-process and belong in the
  examiner's environment manifest, not the crate SBOM.
- **SHA256SUMS is self-asserted.** It anchors integrity only when the
  signature/attestation over it is verified; an unsigned manifest proves
  nothing about who produced it.

The Windows zip contains no FFmpeg; it is installed by the user from the pinned BtbN release (tools/manifest.json).
