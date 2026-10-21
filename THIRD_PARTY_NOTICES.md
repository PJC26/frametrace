# Third-party notices

FrameTrace itself is MIT-licensed (see `LICENSE`). Neither this repository nor
the Windows package built by `scripts/make-portable.ps1` contains any
third-party executable: no FFmpeg, no libewf, no Sleuth Kit, no deepfake-lens.
FrameTrace runs those tools as separate programs when they are present in
`tools/bin` or on `PATH`; it does not link them, so none of their licenses
applies to FrameTrace's own code.

## FFmpeg — not distributed by FrameTrace; installed by you from BtbN

`frametrace tools install ffmpeg` (and the home screen's install button)
downloads one pinned file **directly from the BtbN FFmpeg-Builds GitHub
release to your machine** and verifies it against the SHA-256 published with
that release (`tools/manifest.json`: version `n8.1.3-9-g29e619e767`, release
`autobuild-2026-09-30-13-08`, variant `win64-lgpl-8.1`, size 170 611 883 bytes,
SHA-256 `4a7642b2264c03e8a0ce8a3825b933ee5580656f45695a086fe7e294045ffc0a`).
Nothing is downloaded until you confirm the file, source, size and the notice
below. FrameTrace is not the distributor of that file: the FrameTrace
maintainers make no offer of source for it and grant no patent licence of any
kind.

What the file is: FFmpeg's own code under the GNU LGPL v3 or later
(`--enable-version3`), statically linked with many libraries. **Despite the
variant name "lgpl", the binary as a whole is not LGPL-only**: it contains
libzvbi parts under GPL-2.0-only (`src/packet-830.*`, `src/pdc.*`), FFTW3 under
GPL-2.0-or-later (via chromaprint), OpenSSL and others under Apache-2.0, and a
libopenh264 compiled from source (Cisco's H.264 patent licence covers only the
binaries Cisco distributes). The component list derived from BtbN's build
scripts is in `docs/ffmpeg-btbn-components-autobuild-2026-09-30.md`
(informational). These licenses do not restrict using the file on your own
computer (patents are a separate matter: no patent licence comes with it).
Because GPL-2.0-only, LGPL-3.0 and Apache-2.0 code are mixed in that one
binary, FrameTrace makes no assurance that the file may be passed on to anyone
else: do not copy it to other computers (for example inside a FrameTrace
folder that contains `tools/bin/ffmpeg.exe`) — let each user download it from
BtbN. The sources are at the upstream locations named in the manifest and in
that document. (Owner to confirm this paragraph.)

FrameTrace copies two license texts next to the installed binaries:
`tools/bin/licenses/ffmpeg-LICENSE.txt` (the `COPYING.LGPLv3` shipped in the
BtbN zip) and `tools/bin/licenses/ffmpeg-COPYING.GPLv3.txt` (the GNU GPL v3
the LGPLv3 incorporates by reference; FFmpeg's own `COPYING.GPLv3` at commit
`29e619e767cde9045a75c29bc9a8278ae7b3a98b`, SHA-256
`8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903`, pinned in
the manifest). Each install attempt is recorded in `tools/install-audit.jsonl`
(URL, size, SHA-256, result).

Encoders: that build has no libx264. FrameTrace encodes H.264 with the best
encoder the resolved ffmpeg really has, in the order `libx264` → `h264_mf`
(Windows Media Foundation, licensed with Windows) → `libopenh264` → `mpeg4`,
and records the pick in every proxy / export audit line. You may use any other
FFmpeg build instead: put `ffmpeg.exe` / `ffprobe.exe` in `tools/bin` or on
`PATH`.

## Optional tools (not distributed; installed by the examiner)

| Tool | License | Where to get it | Installer |
| --- | --- | --- | --- |
| libewf (`ewfinfo`, `ewfverify`, `ewfexport`) | LGPL-3.0-or-later | https://github.com/libyal/libewf | Manual — no official Windows binaries (`scripts/build-libewf-tools.ps1` builds from source). |
| The Sleuth Kit (`mmls`, `fls`, `icat`) | IPL-1.0 / CPL-1.0 core, per-file licenses (https://www.sleuthkit.org/sleuthkit/licenses.php) | https://www.sleuthkit.org/sleuthkit/ | Manual — the project publishes PGP signatures, not checksums. |
| deepfake-lens | per its distribution | not on PyPI — the checkout or wheel you were given | Manual (`FRAMETRACE_DEEPFAKE_LENS`); the on-demand pip installer stays disabled until every wheel can be hash-pinned. |

## Future bundle

If a self-built LGPL FFmpeg is ever shipped in the package (option A,
`docs/PLAN_TOOLS_C_2026-10-20.md` appendix A), its notices and source offer
will be added here in that change.
