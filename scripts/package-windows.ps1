# Builds the Windows distribution with FFmpeg bundled (docs/PLAN_TOOLS_2026-10-20.md S5).
#
# FFmpeg is taken ONLY from tools/manifest.json: the pinned URL is fetched with
# Windows' own curl.exe (HTTPS only, redirects limited to the manifest's host
# allowlist), its size and SHA-256 must equal the manifest, every archive entry
# must be a safe relative name and no entry may be a link — otherwise the
# script fails and nothing is packaged. Only the manifest's files are copied:
# ffmpeg.exe / ffprobe.exe into tools/bin, the license into tools/bin/licenses.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package-windows.ps1 [-SkipBuild] [-CacheDir <dir>]
#   -SkipBuild  use the existing target/release binaries (CI builds them first)
#   -CacheDir   keep/reuse the downloaded archive there (re-verified every run)
param(
    [switch]$SkipBuild,
    [string]$CacheDir = "",
    # Alternate manifest (offline tests of this script only).
    [string]$ManifestPath = "tools/manifest.json"
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

if (-not $SkipBuild) {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed (exit code $LASTEXITCODE)" }
}
foreach ($exe in @("target/release/frametrace.exe", "target/release/frametrace-app.exe")) {
    if (-not (Test-Path $exe)) { throw "missing $exe (build first or drop -SkipBuild)" }
}
$version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages[0].version

# ---- manifest ----------------------------------------------------------------
$manifest = Get-Content -Raw -Encoding UTF8 $ManifestPath | ConvertFrom-Json
$ffmpeg = $manifest.tools | Where-Object { $_.name -eq "ffmpeg" }
if (-not $ffmpeg) { throw "tools/manifest.json has no ffmpeg entry" }
if (-not $ffmpeg.sha256 -or $ffmpeg.sha256 -notmatch '^[0-9a-f]{64}$') { throw "ffmpeg sha256 is not pinned in tools/manifest.json" }
if (-not $ffmpeg.size -or [int64]$ffmpeg.size -le 0) { throw "ffmpeg size is not pinned in tools/manifest.json" }
if ($ffmpeg.url -notmatch '^https://') { throw "ffmpeg url must be https: $($ffmpeg.url)" }
if ($ffmpeg.url -match '(?i)/latest(/|$)') { throw "ffmpeg url is a 'latest' url (not pinned): $($ffmpeg.url)" }
$allowed = @($manifest.allowed_hosts)

function Test-SafeEntry([string]$name) {
    $trimmed = $name.TrimEnd('/')
    if ($trimmed.Length -eq 0 -or $trimmed.StartsWith('/')) { return $false }
    if ($name -match '[\\:\x00-\x1f]') { return $false }
    foreach ($part in $trimmed.Split('/')) {
        if ($part -eq '' -or $part -eq '.' -or $part -eq '..') { return $false }
    }
    return $true
}

# ---- download (or reuse the cache) ---------------------------------------------
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("ft-package-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $work | Out-Null
$staged = $false
try {
    $archive = $null
    if ($CacheDir) {
        New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null
        $cached = Join-Path $CacheDir $ffmpeg.file_name
        if (Test-Path $cached) { $archive = $cached }
    }
    if (-not $archive) {
        $curl = Join-Path $env:SystemRoot "System32\curl.exe"
        if (-not (Test-Path $curl)) { throw "curl.exe not found at $curl" }
        $archive = Join-Path $work "download.part"
        Write-Host "downloading $($ffmpeg.url)"
        $effective = & $curl --disable --silent --show-error --fail --location --max-redirs 5 `
            --proto "=https" --proto-redir "=https" --tlsv1.2 --connect-timeout 30 `
            --max-filesize $ffmpeg.size --output $archive --write-out "%{url_effective}" --url $ffmpeg.url
        if ($LASTEXITCODE -ne 0) { throw "download failed (curl exit $LASTEXITCODE)" }
        $finalHost = ([System.Uri]$effective).Host.ToLowerInvariant()
        if ($allowed -notcontains $finalHost) { throw "download ended on host $finalHost, outside the manifest allowlist ($($allowed -join ', '))" }
    }

    # ---- verify size + SHA-256 before opening the archive ----------------------
    $len = (Get-Item $archive).Length
    if ($len -ne [int64]$ffmpeg.size) { throw "size mismatch: $len bytes, the manifest pins $($ffmpeg.size)" }
    $sha = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($sha -ne $ffmpeg.sha256) { throw "SHA-256 mismatch: got $sha, the manifest pins $($ffmpeg.sha256)" }
    Write-Host "verified $($ffmpeg.file_name) ($len bytes, sha256 $sha)"
    if ($CacheDir -and $archive -ne (Join-Path $CacheDir $ffmpeg.file_name)) {
        Copy-Item $archive (Join-Path $CacheDir $ffmpeg.file_name) -Force
    }

    # ---- stage --------------------------------------------------------------------
    $stage = "dist/FrameTrace"
    $staged = $false
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    $bin = Join-Path $stage "tools/bin"
    $licenses = Join-Path $bin "licenses"
    New-Item -ItemType Directory -Force -Path $licenses | Out-Null
    Copy-Item target/release/frametrace.exe $stage/
    Copy-Item target/release/frametrace-app.exe $stage/
    Copy-Item README.md, LICENSE, THIRD_PARTY_NOTICES.md $stage/
    New-Item -ItemType Directory -Path "$stage/docs" -Force | Out-Null
    Copy-Item docs/WINDOWS_USAGE.md $stage/docs/
    Copy-Item scripts/install.ps1 $stage/

    # ---- extract only the manifest's files (whole-archive safety check first) ----
    $zip = [System.IO.Compression.ZipFile]::OpenRead((Resolve-Path $archive).Path)
    try {
        foreach ($entry in $zip.Entries) {
            if (-not (Test-SafeEntry $entry.FullName)) { throw "archive refused: unsafe entry '$($entry.FullName)'" }
            $type = ($entry.ExternalAttributes -shr 16) -band 0xF000
            if ($type -eq 0xA000) { throw "archive refused: link entry '$($entry.FullName)'" }
        }
        $maps = @()
        foreach ($m in @($ffmpeg.files)) { $maps += , @($m.from, (Join-Path $bin $m.to)) }
        foreach ($m in @($ffmpeg.license_files)) { $maps += , @($m.from, (Join-Path $licenses $m.to)) }
        foreach ($pair in $maps) {
            $found = @($zip.Entries | Where-Object { $_.FullName -eq $pair[0] })
            if ($found.Count -ne 1) { throw "archive refused: '$($pair[0])' found $($found.Count) times" }
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($found[0], $pair[1], $false)
        }
    } finally {
        $zip.Dispose()
    }
    $staged = $true
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    # A refused archive leaves no half-built package behind.
    if (-not $staged) { Remove-Item -Recurse -Force "dist/FrameTrace" -ErrorAction SilentlyContinue }
}

# FFmpeg's identity next to its license (T5).
@"
FFmpeg $($ffmpeg.version) ($($ffmpeg.license))
binary:   $($ffmpeg.url)
sha256:   $($ffmpeg.sha256)
checksum: $($ffmpeg.checksum_source)
source:   $($ffmpeg.source_url)
build:    $($ffmpeg.build_url)
"@ | Out-File -Encoding utf8 (Join-Path $licenses "ffmpeg-SOURCE.txt")

$zipPath = "dist/FrameTrace-$version-win64-bundled.zip"
Remove-Item -Force $zipPath -ErrorAction SilentlyContinue
Compress-Archive -Path $stage -DestinationPath $zipPath -Force
$dist = (Resolve-Path "dist").Path
$sumLines = @(Get-ChildItem $dist -Filter *.zip | ForEach-Object {
    "{0}  {1}" -f ((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()), $_.Name
})
[System.IO.File]::WriteAllText((Join-Path $dist "SHA256SUMS"), (($sumLines -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "package: $zipPath"
