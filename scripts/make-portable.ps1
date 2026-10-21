# Builds release binaries and the portable Windows zip for examiners.
#
# The package contains FrameTrace only. FFmpeg (ffmpeg / ffprobe / ffplay and
# its shared libraries) is NEVER packaged: the build the guided installer
# fetches is not redistributable by us (docs/PLAN_TOOLS_C_2026-10-20.md T5).
# The examiner runs `frametrace.exe tools install ffmpeg` (or the home
# screen's install button) after unzipping, or keeps ffmpeg on PATH. Any
# such file found in the tools folder is skipped with a warning (by name:
# any name containing ffmpeg/ffprobe/ffplay or an FFmpeg library name, any
# case, trailing dots/spaces ignored), every staged .exe/.dll whose version
# information names FFmpeg fails the run, EVERY staged file (any extension,
# docs included) containing an FFmpeg signature string fails the run (a
# renamed copy: real static ffmpeg.exe builds have empty version info), and
# the written zip is checked again — one FFmpeg entry deletes it and fails.
# A zip from an earlier run left in dist/ that contains FFmpeg also fails the
# run; SHA256SUMS lists only the zip written by this run.
#
# Signature scan (Fix round 6): case-insensitive `the FFmpeg developers`,
# `ffmpeg version <digit|n|N->`, `libavcodec`, `libavutil`, `libswscale`
# (Fix round 7; absent from our exes and staged docs), `Lavf<NN>.<N>` (the
# libavformat ident, e.g. Lavf61.7.100). They are chosen not to occur in our
# own staged files: "FFmpeg version text" (docs/WINDOWS_USAGE.md) and
# "lavfi" (frametrace.exe's trial encode) do not match. The manifest pins only
# the archive's SHA-256, not per-file ffmpeg.exe/ffprobe.exe hashes, so no
# per-file hash comparison is made.
#
# Release guard: THIRD_PARTY_NOTICES.md still containing "Owner to confirm"
# (any whitespace between the words — a reflowed line still counts; a
# paragraph the owner has not confirmed yet) fails the run unless
# -AllowUnconfirmedNotices is passed (CI test packages only). With the switch
# the zip is named FrameTrace-<ver>-win64-UNCONFIRMED.zip, no SHA256SUMS is
# written (SHA256SUMS.unconfirmed instead; a stale SHA256SUMS is removed) and
# a warning says it is not a release. The same applies (same switch) while
# any installable tool in tools/manifest.json (sha256 pinned) lacks
# "notice_confirmed": true — its distribution notice is compiled into
# frametrace.exe and the owner has not confirmed it (§Fix round 11 M2).
#
# The zip is written with [IO.Compression.ZipFile]::CreateFromDirectory and
# every entry name is checked to use forward slashes. SHA-256 is computed
# through .NET (`Get-FileHash` fails to auto-load in Windows PowerShell 5.1
# started from pwsh 7 — GitHub runners).
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/make-portable.ps1 [-SkipBuild] [-Version <ver>] [-AllowUnconfirmedNotices]
#   -SkipBuild  use the existing target/release binaries (CI builds them first)
#   -Version    package version (default: Cargo.toml via cargo metadata)
#   -AllowUnconfirmedNotices  package although THIRD_PARTY_NOTICES.md still
#               says "Owner to confirm" (not for a release)
param(
    [switch]$SkipBuild,
    [string]$Version = "",
    [switch]$AllowUnconfirmedNotices
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
# Windows PowerShell 5.1 (.NET Framework) writes `\` into ZipFile entry names
# unless this switch is off; it is read once, so set it before any ZipFile
# call. The entry names are checked after the zip is written regardless.
[System.AppContext]::SetSwitch('Switch.System.IO.Compression.ZipFile.UseBackslash', $false)

function Get-Sha256Hex([string]$Path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $stream = [System.IO.File]::OpenRead((Resolve-Path -LiteralPath $Path).ProviderPath)
    try { ([System.BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $sha.Dispose() }
}

function Test-SafeEntry([string]$name) {
    $trimmed = $name.TrimEnd('/')
    if ($trimmed.Length -eq 0 -or $trimmed.StartsWith('/')) { return $false }
    if ($name -match '[\\:\x00-\x1f]') { return $false }
    foreach ($part in $trimmed.Split('/')) {
        if ($part -eq '' -or $part -eq '.' -or $part -eq '..') { return $false }
    }
    return $true
}

# FFmpeg files that must never be packaged (relative, `/`-separated path):
# any leaf whose name — trimmed of spaces and trailing dots, any case —
# contains ffmpeg/ffprobe/ffplay or an FFmpeg library name (renamed copies,
# `ffmpeg.exe.bak`, `libavcodec-61.dll`, licenses/ffmpeg-*).
function Test-FfmpegFile([string]$rel) {
    $leaf = (($rel.TrimEnd('/') -split '/')[-1]).Trim().TrimEnd('.', ' ').ToLowerInvariant()
    if ($leaf -match 'ffmpeg|ffprobe|ffplay') { return $true }
    if ($leaf -match 'av(codec|format|util|device|filter)|swscale|swresample|postproc') { return $true }
    return $false
}

# Whether a PE file's version information names FFmpeg (a renamed binary).
function Test-FfmpegVersionInfo([string]$path) {
    $info = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($path)
    $text = @($info.ProductName, $info.FileDescription, $info.CompanyName, $info.InternalName, $info.OriginalFilename) -join ' '
    return ($text -match '(?i)ffmpeg|ffprobe|ffplay')
}

# FFmpeg signature strings (see the header): case-insensitive, ASCII.
$script:FfmpegSignature = New-Object System.Text.RegularExpressions.Regex(
    'the FFmpeg developers|ffmpeg version (?:n|N-)?[0-9]|libavcodec|libavutil|libswscale|Lavf[0-9]{2}\.[0-9]',
    ([System.Text.RegularExpressions.RegexOptions]::IgnoreCase -bor [System.Text.RegularExpressions.RegexOptions]::CultureInvariant))
# Longer than any signature: a match across a chunk boundary is not missed.
$script:SignatureOverlap = 64

# The first FFmpeg signature string in a file (any extension), or $null. Read
# in 4 MiB chunks; each chunk is decoded as Latin-1 (one char per byte) and
# keeps the last $SignatureOverlap bytes of the previous chunk in front.
function Get-FfmpegSignature([string]$path) {
    $latin1 = [System.Text.Encoding]::GetEncoding(28591)
    $chunk = 4MB
    $buffer = New-Object byte[] ($chunk + $script:SignatureOverlap)
    $stream = [System.IO.File]::OpenRead($path)
    try {
        $kept = 0
        while ($true) {
            $read = $stream.Read($buffer, $kept, $chunk)
            if ($read -le 0) { break }
            $total = $kept + $read
            $match = $script:FfmpegSignature.Match($latin1.GetString($buffer, 0, $total))
            if ($match.Success) { return $match.Value }
            $kept = [Math]::Min($script:SignatureOverlap, $total)
            [System.Array]::Copy($buffer, $total - $kept, $buffer, 0, $kept)
        }
    } finally {
        $stream.Dispose()
    }
    return $null
}

# The first FFmpeg entry of a zip, or $null.
function Get-ZipFfmpegEntry([string]$path) {
    $zip = [System.IO.Compression.ZipFile]::OpenRead($path)
    try {
        foreach ($entry in $zip.Entries) {
            if (Test-FfmpegFile $entry.FullName) { return $entry.FullName }
        }
    } finally {
        $zip.Dispose()
    }
    return $null
}

$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# Release guard (docs/PLAN_TOOLS_2026-10-20.md §Fix round 5 M2, §Fix round 6
# L-CI): any whitespace (a reflowed line break) between the words counts.
$unconfirmed = $false
$unconfirmedWhy = @()
if ((Get-Content -Raw -Encoding UTF8 -LiteralPath THIRD_PARTY_NOTICES.md) -match '(?i)Owner\s+to\s+confirm') {
    if (-not $AllowUnconfirmedNotices) {
        throw "THIRD_PARTY_NOTICES.md still contains 'Owner to confirm' — the owner has not confirmed that paragraph; no release package until then (pass -AllowUnconfirmedNotices for a test package)"
    }
    $unconfirmed = $true
    $unconfirmedWhy += "THIRD_PARTY_NOTICES.md still says 'Owner to confirm'."
    Write-Warning "THIRD_PARTY_NOTICES.md still contains 'Owner to confirm' — test package only (-AllowUnconfirmedNotices), not for release: the zip is named *-UNCONFIRMED.zip and no SHA256SUMS is written"
}

# Release guard (§Fix round 11 M2): the distribution notice of every
# installable tool (sha256 pinned) in tools/manifest.json is compiled into
# this package's frametrace.exe and shown before the download; it needs
# "notice_confirmed": true from the owner. Same switch, same UNCONFIRMED
# naming as the THIRD_PARTY_NOTICES.md guard.
if (-not (Test-Path -LiteralPath tools/manifest.json)) {
    throw "tools/manifest.json not found — cannot check the tools' distribution notices"
}
$toolManifest = Get-Content -Raw -Encoding UTF8 -LiteralPath tools/manifest.json | ConvertFrom-Json
$noticePending = @()
foreach ($tool in @($toolManifest.tools)) {
    $pinned = $tool.PSObject.Properties['sha256']
    if (-not $pinned -or $null -eq $pinned.Value -or "$($pinned.Value)" -eq '') { continue }
    $confirmed = $tool.PSObject.Properties['notice_confirmed']
    if (-not $confirmed -or $confirmed.Value -isnot [bool] -or -not $confirmed.Value) { $noticePending += "$($tool.name)" }
}
if ($noticePending.Count -gt 0) {
    $names = $noticePending -join ', '
    if (-not $AllowUnconfirmedNotices) {
        throw "tools/manifest.json: the distribution notice of $names is not confirmed (notice_confirmed is not true) — the owner has not confirmed it; no release package until then (pass -AllowUnconfirmedNotices for a test package)"
    }
    $unconfirmed = $true
    $unconfirmedWhy += "tools/manifest.json notice_confirmed is not true for: $names."
    Write-Warning "tools/manifest.json: the distribution notice of $names is not confirmed — test package only (-AllowUnconfirmedNotices), not for release: the zip is named *-UNCONFIRMED.zip and no SHA256SUMS is written"
}

# A zip left in dist/ by an earlier packager (e.g. the old *-bundled.zip) must
# not sit next to this package with FFmpeg in it.
if (Test-Path dist) {
    foreach ($old in @(Get-ChildItem -LiteralPath (Resolve-Path dist).ProviderPath -Filter *.zip -File)) {
        $hit = $null
        try { $hit = Get-ZipFfmpegEntry $old.FullName } catch { throw "cannot read $($old.FullName): $($_.Exception.Message) — remove it from dist/" }
        if ($hit) { throw "dist/$($old.Name) contains FFmpeg ('$hit') — remove it; ffmpeg/ffprobe are never packaged (docs/PLAN_TOOLS_C_2026-10-20.md T5)" }
    }
}

if (-not $SkipBuild) {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed (exit code $LASTEXITCODE)" }
}
foreach ($exe in @("target/release/frametrace.exe", "target/release/frametrace-app.exe")) {
    if (-not (Test-Path $exe)) { throw "missing $exe (build first or drop -SkipBuild)" }
}
if (-not $Version) {
    $Version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages[0].version
}
if ($Version -notmatch '^[0-9A-Za-z][0-9A-Za-z.+-]*$') { throw "bad -Version '$Version'" }

$stage = "dist/FrameTrace-$Version-win64"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path "$stage/tools/bin" | Out-Null
Copy-Item target/release/frametrace.exe $stage/
Copy-Item target/release/frametrace-app.exe $stage/
Copy-Item README.md, LICENSE, THIRD_PARTY_NOTICES.md $stage/
New-Item -ItemType Directory -Path "$stage/docs" -Force | Out-Null
Copy-Item docs/WINDOWS_USAGE.md $stage/docs/
Copy-Item docs/WINDOWS_VALIDATION.md $stage/docs/ -ErrorAction SilentlyContinue
Copy-Item scripts/install.ps1 $stage/
# Vendored tool binaries (libewf / Sleuth Kit + their DLLs) when the repo has
# them, so E01 and deleted-file workflows work out of the box. tools/bin
# (hand-assembled) wins; scripts/build-libewf-tools.ps1 installs to
# target/release/tools/bin, so fall back to that. FFmpeg files in either
# folder are skipped (see the header).
$toolsSrc = $null
foreach ($cand in @("tools/bin", "target/release/tools/bin")) {
    if (Test-Path $cand) { $toolsSrc = $cand; break }
}
if ($toolsSrc) {
    Write-Host "bundling tools from $toolsSrc"
    $srcFull = (Resolve-Path -LiteralPath $toolsSrc).ProviderPath.TrimEnd('\', '/')
    $binFull = (Resolve-Path -LiteralPath "$stage/tools/bin").ProviderPath
    foreach ($file in @(Get-ChildItem -LiteralPath $srcFull -Recurse -File)) {
        $rel = $file.FullName.Substring($srcFull.Length).TrimStart('\', '/') -replace '\\', '/'
        if (Test-FfmpegFile $rel) {
            Write-Warning "ffmpeg/ffprobe are never packaged (docs/PLAN_TOOLS_C_2026-10-20.md T5) — skipped: $($file.FullName)"
            continue
        }
        $dest = Join-Path $binFull ($rel -replace '/', '\')
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dest) | Out-Null
        Copy-Item -LiteralPath $file.FullName -Destination $dest -Force
    }
} else {
    Write-Warning "no tools/bin or target/release/tools/bin found - the package ships WITHOUT libewf/Sleuth Kit tools (E01 and deleted-file workflows need them). Run scripts/build-libewf-tools.ps1 first or copy the tools into tools/bin."
}
@'
FrameTrace portable package
===========================
1. tools/bin 아래의 도구 실행 파일은 PATH 설정 없이 자동 인식됩니다.
   - ewfinfo.exe / ewfverify.exe / ewfexport.exe (E01) — 패키지에 포함된 경우 그대로 동작
   - mmls.exe / fls.exe / icat.exe (Sleuth Kit) — 동일
   - ffmpeg.exe / ffprobe.exe — 패키지에 포함되지 않음. frametrace.exe tools install ffmpeg (또는 첫 화면의 [설치])로 BtbN에서 직접 내려받거나 PATH에 두십시오.
   설치/빌드 안내는 docs/WINDOWS_USAGE.md 참고.
2. frametrace-app.exe를 실행하면 브라우저 검수 워크스테이션이 열립니다 (콘솔 없음).
   종료는 페이지 우측 상단 '종료' 버튼을 사용하십시오. 브라우저 탭만 닫으면
   서버가 계속 실행됩니다. 다시 실행하면 실행 중인 서버에 재접속됩니다.
3. frametrace.exe는 CLI입니다 (인자 없이 실행해도 워크스테이션이 시작됩니다).
4. 설치(시작 메뉴 등록): powershell -ExecutionPolicy Bypass -File install.ps1
   제거: powershell -ExecutionPolicy Bypass -File install.ps1 -Uninstall
   그냥 압축 풀고 frametrace-app.exe를 바로 실행해도 됩니다.
5. 실 Dahua DAV 샘플 검증: scripts/validate-dav-samples.ps1 -Samples <폴더>
'@ | Out-File -Encoding utf8 "$stage/tools/bin/README-tools.txt"

# ---- no renamed FFmpeg binary (PE version information) ----------------------
$fullStage = (Resolve-Path -LiteralPath $stage).ProviderPath
foreach ($pe in @(Get-ChildItem -LiteralPath $fullStage -Recurse -File | Where-Object { $_.Extension -match '^\.(exe|dll)$' })) {
    if (Test-FfmpegVersionInfo $pe.FullName) {
        throw "$($pe.FullName) is an FFmpeg binary by its version information — ffmpeg/ffprobe are never packaged (docs/PLAN_TOOLS_C_2026-10-20.md T5)"
    }
}

# ---- no renamed FFmpeg file (signature strings, every staged file) ----------
foreach ($file in @(Get-ChildItem -LiteralPath $fullStage -Recurse -File -Force)) {
    $sig = Get-FfmpegSignature $file.FullName
    if ($sig) {
        throw "$($file.FullName) contains the FFmpeg signature '$sig' — a renamed FFmpeg file? ffmpeg/ffprobe are never packaged (docs/PLAN_TOOLS_C_2026-10-20.md T5)"
    }
}

# ---- zip (forward-slash entry names, no FFmpeg — both checked) ----------------
$dist = (Resolve-Path "dist").ProviderPath
$zipName = if ($unconfirmed) { "FrameTrace-$Version-win64-UNCONFIRMED.zip" } else { "FrameTrace-$Version-win64.zip" }
$fullZip = Join-Path $dist $zipName
# Never leave the other kind of zip of this version next to this one.
foreach ($other in @("FrameTrace-$Version-win64.zip", "FrameTrace-$Version-win64-UNCONFIRMED.zip")) {
    Remove-Item -Force -LiteralPath (Join-Path $dist $other) -ErrorAction SilentlyContinue
}
Remove-Item -Force -LiteralPath $fullZip -ErrorAction SilentlyContinue
[System.IO.Compression.ZipFile]::CreateFromDirectory($fullStage, $fullZip, [System.IO.Compression.CompressionLevel]::Optimal, $true)
$problem = $null
$check = [System.IO.Compression.ZipFile]::OpenRead($fullZip)
try {
    foreach ($entry in $check.Entries) {
        $name = $entry.FullName
        if ($name.Contains('\')) { $problem = "package zip entry uses a backslash: '$name' (other unzip tools would not see folders)"; break }
        if (-not (Test-SafeEntry $name)) { $problem = "package zip has an unsafe entry '$name'"; break }
        if (Test-FfmpegFile $name) { $problem = "package zip contains FFmpeg ('$name') — ffmpeg/ffprobe are never packaged (docs/PLAN_TOOLS_C_2026-10-20.md T5)"; break }
    }
} finally {
    $check.Dispose()
}
if ($problem) {
    Remove-Item -Force -LiteralPath $fullZip
    throw $problem
}

# SHA256SUMS for the zip written by this run only (a stale zip in dist/ is
# never vouched for) — same manifest convention as scripts/build-release.sh;
# consumers verify with `sha256sum -c` (see docs/repro-build.md). LF endings,
# no BOM (`sha256sum -c` on Linux/macOS).
# An unconfirmed (test) package gets SHA256SUMS.unconfirmed instead, and a
# SHA256SUMS left by an earlier run is removed: nothing vouches for it as a
# release.
$sumLine = "{0}  {1}" -f (Get-Sha256Hex $fullZip), (Split-Path -Leaf $fullZip)
$sumsName = if ($unconfirmed) { "SHA256SUMS.unconfirmed" } else { "SHA256SUMS" }
foreach ($old in @("SHA256SUMS", "SHA256SUMS.unconfirmed")) {
    Remove-Item -Force -LiteralPath (Join-Path $dist $old) -ErrorAction SilentlyContinue
}
[System.IO.File]::WriteAllText((Join-Path $dist $sumsName), ($sumLine + "`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "package: $fullZip"
Write-Host "sums: $(Join-Path $dist $sumsName)"
if ($unconfirmed) {
    Write-Warning "=================================================================="
    Write-Warning " UNCONFIRMED TEST PACKAGE — NOT FOR RELEASE"
    foreach ($why in $unconfirmedWhy) { Write-Warning " $why" }
    Write-Warning " $zipName / $sumsName (no SHA256SUMS written)"
    Write-Warning "=================================================================="
}
