# FrameTrace Windows Usage Notes

Target environment: Windows 10/11 x64, local examiner PC, attached HDD/SSD/SD card/USB media, no processing server.

## Install Prerequisites

1. Install Node.js 20+ (`https://nodejs.org/`) — required by tests/CI for
   generated-page syntax checks and the CSS gate (`node scripts/check-css.mjs`).
2. Install Rust for Windows with the MSVC toolchain (`https://rustup.rs/`;
   `rust-toolchain.toml` pins the exact toolchain via rustup):
   - Visual Studio Build Tools may be required for the MSVC linker.
3. Install FFmpeg for Windows (`winget install Gyan.FFmpeg` or
   `choco install ffmpeg`) and make sure both tools are in `PATH`:
   - `ffmpeg.exe`
   - `ffprobe.exe`
4. Install libewf tools if E01/Ex01 evidence containers will be processed.
   Upstream ships source only for Windows, so use the automated source build:
   - `powershell -ExecutionPolicy Bypass -File scripts/build-libewf-tools.ps1` (installs a per-user
     MSYS2, builds libewf, drops `ewf*.exe` + `libewf-3.dll` into
     `target/release/tools/bin`; no admin required)
   - or place a trusted community build into `tools/bin/`.
5. Install The Sleuth Kit tools if raw-image file-system inspection or deleted inode recovery will be processed
   (`winget install Sleuthkit.Sleuthkit` or `https://sleuthkit.org`):
   - `mmls.exe`
   - `fls.exe`
   - `icat.exe`
   Portable alternative: drop any of the above binaries into `tools/bin/`
   next to `frametrace-app.exe` — they are discovered automatically, no PATH
   editing required. Note: libewf has no official Windows build upstream;
   build from source or use a trusted community build.
6. Use PowerShell or Windows Terminal.

Check the tools:

```powershell
rustc --version
cargo --version
ffmpeg -version
ffprobe -version
ewfinfo -V
ewfverify -V
ewfexport -V
mmls -V
fls -V
icat -V
```

## Build

From the project folder:

```powershell
cargo build --release
```

The executable is:

```text
.\target\release\frametrace.exe
```

## Distribution (Portable / Install / Signing)

Build the portable package:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/make-portable.ps1
```

This produces `dist/FrameTrace-<version>-win64/` + `.zip` + `SHA256SUMS`.
The package contains both executables, docs, `install.ps1`, and `tools/bin`.
The packaged libewf / Sleuth Kit tools come from `tools/bin` if it exists,
otherwise from `target/release/tools/bin` (where `build-libewf-tools.ps1`
installs them); FFmpeg files found there are skipped, never packaged.
The package never contains ffmpeg/ffprobe (they are not redistributable by us): run `frametrace.exe tools install ffmpeg` after unzipping (or use the home screen's install button), or keep them on PATH.

**Two ways to run it:**

- *No install*: unzip and double-click `frametrace-app.exe`.
- *Install*: `powershell -ExecutionPolicy Bypass -File install.ps1` from the
  package folder — copies to `%LOCALAPPDATA%\Programs\FrameTrace`, creates a
  Start Menu shortcut (`-DesktopShortcut` for desktop), and unblocks the
  binaries. `-Uninstall` removes everything (it refuses if case data still
  lives under the install dir).

**SmartScreen / signing:** unsigned builds trigger SmartScreen once —
"추가 정보 → 실행" is safe. `scripts/sign-release.ps1` signs the package
binaries two ways:

```powershell
# Real CA certificate (removes SmartScreen reputation warnings for users):
powershell -ExecutionPolicy Bypass -File scripts/sign-release.ps1 -Thumbprint <cert-thumbprint>

# Local self-signed dev cert (signs but does NOT suppress SmartScreen on
# other machines — development/internal use only):
powershell -ExecutionPolicy Bypass -File scripts/sign-release.ps1 -SelfSigned
```

Suppressing SmartScreen reputation prompts for end users requires a real
OV/EV code-signing certificate from a CA — that is a purchase decision and
cannot be scripted.

## Examiner Workstation Lifecycle

`frametrace-app.exe` (windowed, no console) or a bare `frametrace.exe` starts
the local review workstation on `127.0.0.1` and opens it in the browser.

- **Stopping**: use the **종료** button in the page header — there is no
  console window to close, and closing the browser tab alone leaves the
  server running. Shutdown is refused while a job is in progress; cancel
  the job first (the UI offers this).
- **Relaunching**: starting the app again while it is already running
  reuses the live server and just opens a browser tab — no duplicate
  instances stack up on successive ports.
- The server never stops on its own — it only exits via the 종료 button /
  `POST /api/shutdown` or by killing the process.
- **Auth** (optional): `FRAMETRACE_TOKEN` adds a shared-secret gate — see
  `docs/security-review.md`.
- **Previous UI** (rollback, one release only): start with
  `FRAMETRACE_UI=legacy` (PowerShell: `$env:FRAMETRACE_UI="legacy"`) to get
  the pre-rebuild 4-step workstation screen with its embedded viewer frame.

## Recommended Field Workflow

Use a case folder on a fast local SSD when possible:

```powershell
.\target\release\frametrace.exe init-case C:\Cases\case-001 --title "Client CCTV review"
```

When the receiving form has these details, record them in the case manifest immediately:

```powershell
.\target\release\frametrace.exe init-case C:\Cases\case-001 --title "Client CCTV review" --operator "Examiner" --device-id "SD-001" --device-serial "SN123" --write-protect "hardware write blocker" --acquisition-tool "FTK Imager" --evidence-hash "<device-or-image-sha256>"
```

Run a fast inventory pass first. This avoids spending hours hashing or probing every file on a terabyte-scale source.

```powershell
.\target\release\frametrace.exe register-source C:\Cases\case-001 E:\ --kind mounted-volume --write-protect "hardware write blocker"
.\target\release\frametrace.exe scan-folder C:\Cases\case-001 E:\ --no-ffprobe
```

The scan still records likely source/parser lanes from extensions and folder names, even when `ffprobe` is skipped. It also records SQLite evidence-source and job rows.

If the blackbox SD card was acquired as E01, inspect and import it first:

```powershell
.\target\release\frametrace.exe inspect-e01 C:\Cases\case-001 D:\Images\blackbox.E01
.\target\release\frametrace.exe import-e01 C:\Cases\case-001 D:\Images\blackbox.E01 --output C:\Cases\case-001\evidence\images\blackbox.raw
```

`import-e01` uses `ewfinfo`, `ewfverify`, and `ewfexport`. It writes E01 provenance logs under `evidence\logs`, hashes the exported raw image, and appends `evidence\logs\e01-audit.jsonl`.

For normal video-file review, mount the E01 or exported raw image read-only with a forensic mounter and run `scan-folder` on the mounted drive letter. For direct signature recovery from the image, run `carve-file` on the exported raw file.

For file-system-aware triage without mounting, inspect the exported raw image with Sleuth Kit:

```powershell
.\target\release\frametrace.exe inspect-image C:\Cases\case-001 C:\Cases\case-001\evidence\images\blackbox.raw
```

If the correct file-system offset is known or must be selected manually:

```powershell
.\target\release\frametrace.exe inspect-image C:\Cases\case-001 C:\Cases\case-001\evidence\images\blackbox.raw --partition-offset 2048
```

Recover one inode/metadata address from the `fls` listing:

```powershell
.\target\release\frametrace.exe recover-inode C:\Cases\case-001 C:\Cases\case-001\evidence\images\blackbox.raw 1304-128-1 --partition-offset 2048 --recover-deleted
```

`inspect-image` writes `db\filesystem\tsk-files-*.jsonl` and raw tool logs under `evidence\logs`. `recover-inode` writes derived candidates under `artifacts\recovered\filesystem` and appends `evidence\logs\tsk-audit.jsonl`.

After the first inventory, run deeper metadata collection on a narrowed source folder when needed:

```powershell
.\target\release\frametrace.exe scan-folder C:\Cases\case-001 E:\BLACKBOX --hash --max-depth 2
```

Repeated scans are cumulative. `C:\Cases\case-001\db\case.db` is the primary SQLite index, the JSON/JSONL/TSV files remain compatibility artifacts, and every individual scan run is preserved under `C:\Cases\case-001\db\scan_runs`.

Generate the local review page:

```powershell
.\target\release\frametrace.exe make-review C:\Cases\case-001
```

Open:

```text
C:\Cases\case-001\review\index.html
C:\Cases\case-001\review\evidence-viewer.html
```

`evidence-viewer.html` is the viewer-first real case page. It uses the current video index, carving log, and validation log; no server is required.

Export client deliverables:

```powershell
.\target\release\frametrace.exe export-video C:\Cases\case-001 vid_000001 --format mp4 --start 10 --duration 30
.\target\release\frametrace.exe export-video C:\Cases\case-001 vid_000001 --format avi
```

Generate review artifacts:

```powershell
.\target\release\frametrace.exe make-thumbnail C:\Cases\case-001 vid_000001 --time 5
.\target\release\frametrace.exe make-proxy C:\Cases\case-001 vid_000001 --max-width 1280
```

Default export, proxy, and thumbnail names are made unique when a file already exists. When `--output` is provided, FrameTrace refuses to overwrite an existing file.
The derived artifact logs include output SHA-256 values, FFmpeg version text, command arguments, and hash-chain fields so later report review can detect missing or reordered log entries.

Carve contiguous video candidates from an acquired image file or raw export file:

```powershell
.\target\release\frametrace.exe carve-file C:\Cases\case-001 D:\Images\client-sdcard.img --max-bytes 536870912 --max-candidates 128
```

For an imported E01:

```powershell
.\target\release\frametrace.exe carve-file C:\Cases\case-001 C:\Cases\case-001\evidence\images\blackbox.raw --max-bytes 536870912 --max-candidates 128
```

`carve-file` is a candidate-recovery pass, not a full proprietary DVR file-system parser. It preserves offsets, hashes the carved outputs, marks duplicate carved hashes, and writes logs under `artifacts\carved`.
Carved artifacts remain `candidate-unvalidated` or `duplicate-candidate` until playback/container validation is done.

Validate an indexed video, carved candidate ID, recovered inode output path, or direct file path with `ffprobe`:

```powershell
.\target\release\frametrace.exe validate-artifact C:\Cases\case-001 vid_000001
.\target\release\frametrace.exe validate-artifact C:\Cases\case-001 carve_000001
.\target\release\frametrace.exe validate-artifact C:\Cases\case-001 C:\Cases\case-001\artifacts\recovered\filesystem\inode_1304-128-1.bin
```

Validation results are appended to `evidence\logs\validation-log.jsonl` and shown in the HTML report and evidence viewer. `ffprobe-video-stream-confirmed` means `ffprobe` found a video stream; the examiner should still perform playback review before final reporting.

Generate the current HTML report:

```powershell
.\target\release\frametrace.exe make-report C:\Cases\case-001
```

By default `make-report` trusts the SHA-256 hashes stored in the index and only flags index records whose files went missing or whose size/mtime drifted (`index-record-stale` candidate findings). Pass `--rehash` to re-hash every indexed file for full revalidation:

```powershell
.\target\release\frametrace.exe make-report C:\Cases\case-001 --rehash
```

The anomaly log entry records which mode was used (`"hash_mode":"stored"` or `"rehash"`).

Open:

```text
C:\Cases\case-001\reports\case-report.html
```

Build a checksummed transfer/review package:

```powershell
.\target\release\frametrace.exe package-case C:\Cases\case-001
```

Open `reports\case-report.html` from the package and print to PDF when a PDF deliverable is required.

## Evidence Handling Rules

- Treat source media as read-only.
- Prefer write blockers or forensic images when the workflow requires strict preservation.
- Do not run vendor self-player `.exe` files automatically.
- Keep derived files, logs, reports, and exports inside the case folder.
- Start with mounted volumes and copied evidence folders. Raw `\\.\PhysicalDriveN` access should be added only after imaging and recovery behavior is validated.
- On unstable disks, image first with a dedicated acquisition tool, then scan the mounted image or extracted folder.
- For E01/Ex01 images, keep the original E01 set unchanged. Use `import-e01` only to create a derived raw working image inside the case folder.
- For Sleuth Kit recovery, preserve the selected partition offset and inode value in notes/report review. Inode-recovered files remain candidates until validated.

## FFmpeg Notes

The prototype calls `ffmpeg` and `ffprobe` by name, so Windows must be able to find them through `PATH`.

FrameTrace does not bundle FFmpeg. Every proxy/export audit line records the resolved binary's encoders (`video_encoder`, `audio_encoder`) and the exact arguments; `frametrace doctor` shows which ffmpeg is used and whether another copy on PATH is ignored.
