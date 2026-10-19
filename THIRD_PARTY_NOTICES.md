# Third-party notices

FrameTrace itself is MIT-licensed (see `LICENSE`). The Windows distribution
built by `scripts/package-windows.ps1` bundles FFmpeg, and
`frametrace tools install` can install further tools into `tools/bin`. Every
such tool is pinned in `tools/manifest.json` (version, URL, size, SHA-256 and
the published checksum it was copied from). Their licenses are copied next to
the binaries in `tools/bin/licenses/`.

## FFmpeg (bundled)

- Version: `n8.1.3-9-g29e619e767` (release branch 8.1), BtbN FFmpeg-Builds
  `win64-lgpl` build, release `autobuild-2026-09-30-13-08`.
- License: **GNU LGPL v3 or later** (built with `--enable-version3`, no GPL or
  non-free components). The license text ships as
  `tools/bin/licenses/ffmpeg-LICENSE.txt`.
- Corresponding source: https://github.com/FFmpeg/FFmpeg/tree/29e619e767
- Build scripts: https://github.com/BtbN/FFmpeg-Builds (tag
  `autobuild-2026-09-30-13-08`).
- Binary: https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-8.1.zip
  (SHA-256 `4a7642b2264c03e8a0ce8a3825b933ee5580656f45695a086fe7e294045ffc0a`,
  from that release's `checksums.sha256`).
- Being an LGPL build, it contains **no GPL encoders such as libx264**. FrameTrace
  detects the encoders of whichever ffmpeg it resolves and uses libx264 only
  when that build has it; with this bundle H.264 comes from `libopenh264`
  (BSD-2-Clause, Cisco) or `h264_mf` (Windows Media Foundation, part of
  Windows), and audio from FFmpeg's native `aac`.
- FrameTrace runs `ffmpeg.exe` / `ffprobe.exe` as separate programs; it does
  not link FFmpeg's libraries. You may replace them with another build in
  `tools/bin` (or on `PATH`).

## Optional tools (not bundled; installed by the examiner)

| Tool | License | Where to get it | Installer |
| --- | --- | --- | --- |
| libewf (`ewfinfo`, `ewfverify`, `ewfexport`) | LGPL-3.0-or-later | https://github.com/libyal/libewf | Manual — no official Windows binaries (`scripts/build-libewf-tools.ps1` builds from source). |
| The Sleuth Kit (`mmls`, `fls`, `icat`) | IPL-1.0 / CPL-1.0 core, per-file licenses (https://www.sleuthkit.org/sleuthkit/licenses.php) | https://www.sleuthkit.org/sleuthkit/ | Manual — the project publishes PGP signatures, not checksums. |
| deepfake-lens | per its distribution | not on PyPI — the checkout or wheel you were given | Manual (`FRAMETRACE_DEEPFAKE_LENS`); the on-demand pip installer stays disabled until every wheel can be hash-pinned. |
