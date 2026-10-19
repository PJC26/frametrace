# Builds the Windows distribution with FFmpeg bundled (docs/PLAN_TOOLS_2026-10-20.md S5).
#
# FFmpeg is taken ONLY from tools/manifest.json: the pinned URL is fetched with
# Windows' own curl.exe (HTTPS only; redirects are followed here one hop at a
# time, each hop checked against the manifest's host allowlist BEFORE it is
# requested — curl never gets --location), its size and SHA-256 must equal the
# manifest, every archive entry must be a safe relative name and no entry may
# be a link — otherwise the script fails and nothing is packaged. Only the
# manifest's files are copied: ffmpeg.exe / ffprobe.exe into tools/bin, the
# licenses into tools/bin/licenses (the zip's LICENSE.txt, plus FrameTrace's
# own hash-pinned GPLv3 text and third-party license bundle). The package zip
# is written with [IO.Compression.ZipFile]::CreateFromDirectory and its entry
# names are checked to use forward slashes.
#
# curl honours the user's proxy environment (HTTPS_PROXY / ALL_PROXY /
# NO_PROXY) so this works on corporate networks; integrity never rests on the
# network path — the pinned size and SHA-256 decide.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package-windows.ps1 [-SkipBuild] [-CacheDir <dir>]
#   -SkipBuild  use the existing target/release binaries (CI builds them first)
#   -CacheDir   keep/reuse the downloaded archive there (re-verified every run)
param(
    [switch]$SkipBuild,
    [string]$CacheDir = "",
    # Alternate manifest (offline tests of this script only).
    [string]$ManifestPath = "tools/manifest.json",
    # Package version (default: Cargo.toml via cargo metadata).
    [string]$Version = "",
    # Offline tests only (scripts/test-package-windows.mjs): also admit plain
    # http:// to 127.0.0.1 — no other host, and never https downgrades.
    [switch]$TestLoopbackHttp
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
# Windows PowerShell 5.1 (.NET Framework) writes `\` into ZipFile entry names
# unless this switch is off; it is read once, so set it before any ZipFile
# call. The entry names are checked after the zip is written regardless.
[System.AppContext]::SetSwitch('Switch.System.IO.Compression.ZipFile.UseBackslash', $false)

# SHA-256 through .NET directly: `Get-FileHash` lives in a module that
# Windows PowerShell 5.1 fails to auto-load when started from pwsh 7 with
# its PSModulePath (GitHub runners) — CommandNotFoundException.
function Get-Sha256Hex([string]$Path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $stream = [System.IO.File]::OpenRead((Resolve-Path -LiteralPath $Path).ProviderPath)
    try { ([System.BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $sha.Dispose() }
}

$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

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

# ---- manifest ----------------------------------------------------------------
$manifest = Get-Content -Raw -Encoding UTF8 $ManifestPath | ConvertFrom-Json
$ffmpeg = $manifest.tools | Where-Object { $_.name -eq "ffmpeg" }
if (-not $ffmpeg) { throw "tools/manifest.json has no ffmpeg entry" }
if (-not $ffmpeg.sha256 -or $ffmpeg.sha256 -notmatch '^[0-9a-f]{64}$') { throw "ffmpeg sha256 is not pinned in tools/manifest.json" }
if (-not $ffmpeg.size -or [int64]$ffmpeg.size -le 0) { throw "ffmpeg size is not pinned in tools/manifest.json" }
if ($ffmpeg.url -match '(?i)/latest(/|$)') { throw "ffmpeg url is a 'latest' url (not pinned): $($ffmpeg.url)" }
$allowed = @($manifest.allowed_hosts | ForEach-Object { $_.ToLowerInvariant() })
$maxHops = 5

function Test-SafeEntry([string]$name) {
    $trimmed = $name.TrimEnd('/')
    if ($trimmed.Length -eq 0 -or $trimmed.StartsWith('/')) { return $false }
    if ($name -match '[\\:\x00-\x1f]') { return $false }
    foreach ($part in $trimmed.Split('/')) {
        if ($part -eq '' -or $part -eq '.' -or $part -eq '..') { return $false }
    }
    return $true
}

# T2 gate for one hop (the manifest URL and every redirect): https (or the
# test loopback), default port, no credentials, host on the allowlist.
function Assert-AllowedUrl([string]$url) {
    # -cmatch: a case-insensitive class over \x7f-￿ would also match
    # ASCII letters (Kelvin sign folds to 'k').
    if ($url -cmatch '[^\x21-\x7e]' -or $url.Contains('\') -or $url.Contains('"')) { throw "refused URL: control, space, backslash or non-ASCII characters" }
    $uri = $null
    if (-not [System.Uri]::TryCreate($url, [System.UriKind]::Absolute, [ref]$uri)) { throw "refused URL (not absolute): $url" }
    $hostName = $uri.Host.ToLowerInvariant()
    if ($uri.UserInfo) { throw "refused URL: credentials in the host" }
    if ($uri.Scheme -eq 'https') {
        if (-not $uri.IsDefaultPort) { throw "refused URL: non-default HTTPS port" }
    } elseif (-not ($TestLoopbackHttp -and $uri.Scheme -eq 'http' -and $hostName -eq '127.0.0.1')) {
        throw "refused URL (HTTPS only): $($uri.GetLeftPart([System.UriPartial]::Path))"
    }
    if ($allowed -notcontains $hostName) { throw "refused URL: host $hostName is not on the tool source allowlist ($($allowed -join ', '))" }
    return $uri
}
$null = Assert-AllowedUrl $ffmpeg.url

# Downloads $url to $out with at most $maxHops redirects, each checked by
# Assert-AllowedUrl before curl connects to it (curl itself never redirects).
function Invoke-PinnedDownload([string]$curl, [string]$url, [string]$out, [int64]$size) {
    $current = $url
    for ($hop = 0; $hop -le $maxHops; $hop++) {
        $uri = Assert-AllowedUrl $current
        $proto = if ($uri.Scheme -eq 'https') { '=https' } else { '=http' }
        Remove-Item -Force $out -ErrorAction SilentlyContinue
        # --disable first (no .curlrc), --globoff ([] and {} are literal).
        $written = & $curl --disable --silent --show-error --fail --globoff `
            --proto $proto --tlsv1.2 --max-redirs 0 --connect-timeout 30 `
            --speed-limit 1024 --speed-time 60 --max-time 3600 `
            --max-filesize $size --output $out --write-out "%{http_code} %{redirect_url}" --url $current
        if ($LASTEXITCODE -ne 0) { throw "download failed (curl exit $LASTEXITCODE)" }
        $parts = "$written".Trim().Split(' ', 2)
        $code = [int]$parts[0]
        $location = if ($parts.Count -gt 1) { $parts[1].Trim() } else { "" }
        if ($code -eq 200) { return }
        if (@(301, 302, 303, 307, 308) -contains $code) {
            if (-not $location) { throw "download failed: HTTP $code without a Location" }
            Write-Host "redirect $hop -> $(([System.Uri]$location).GetLeftPart([System.UriPartial]::Path))"
            $current = $location
            continue
        }
        throw "download failed: HTTP $code"
    }
    throw "download failed: more than $maxHops redirects"
}

# One of FrameTrace's own license files, checked against its manifest pin.
function Get-PinnedRepoFile($file, $sha) {
    if (-not (Test-Path -LiteralPath $file)) { throw "missing $file" }
    $actual = (Get-Sha256Hex $file)
    if ($actual -ne $sha) { throw "$file does not match its pinned sha256 (got $actual, manifest $sha)" }
    return (Resolve-Path -LiteralPath $file).Path
}

# ---- download (or reuse the cache) ---------------------------------------------
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("ft-package-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $work | Out-Null
$staged = $false
$stage = "dist/FrameTrace"
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
        Invoke-PinnedDownload $curl $ffmpeg.url $archive ([int64]$ffmpeg.size)
    }

    # ---- verify size + SHA-256 before opening the archive ----------------------
    $len = (Get-Item $archive).Length
    if ($len -ne [int64]$ffmpeg.size) { throw "size mismatch: $len bytes, the manifest pins $($ffmpeg.size)" }
    $sha = (Get-Sha256Hex $archive)
    if ($sha -ne $ffmpeg.sha256) { throw "SHA-256 mismatch: got $sha, the manifest pins $($ffmpeg.sha256)" }
    Write-Host "verified $($ffmpeg.file_name) ($len bytes, sha256 $sha)"
    if ($CacheDir -and $archive -ne (Join-Path $CacheDir $ffmpeg.file_name)) {
        Copy-Item $archive (Join-Path $CacheDir $ffmpeg.file_name) -Force
    }

    # ---- stage --------------------------------------------------------------------
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

    # ---- FrameTrace's own pinned license texts (GPLv3, third-party bundle) --------
    $own = @()
    if ($ffmpeg.PSObject.Properties['bundled_licenses']) { $own += @($ffmpeg.bundled_licenses) }
    if ($ffmpeg.PSObject.Properties['package_licenses']) { $own += @($ffmpeg.package_licenses) }
    foreach ($l in $own) {
        if ($l.to -notmatch '^[A-Za-z0-9._-]+$') { throw "unsafe license name '$($l.to)'" }
        $src = Get-PinnedRepoFile $l.file $l.sha256
        Copy-Item -LiteralPath $src (Join-Path $licenses $l.to)
    }

    # FFmpeg's identity and the written source offer next to its license (T5).
    @"
FFmpeg $($ffmpeg.version) ($($ffmpeg.license))
binary:   $($ffmpeg.url)
sha256:   $($ffmpeg.sha256)
checksum: $($ffmpeg.checksum_source)
source:   $($ffmpeg.source_url)
build:    $($ffmpeg.build_url)

The statically linked libraries, their licenses and pinned upstream
commits, and the written offer for the complete corresponding source are
in THIRD_PARTY_NOTICES.md (package root); their license texts are in
ffmpeg-THIRD-PARTY-LICENSES.txt next to this file.
"@ | Out-File -Encoding utf8 (Join-Path $licenses "ffmpeg-SOURCE.txt")
    $staged = $true
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    # A refused archive leaves no half-built package behind.
    if (-not $staged) { Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue }
}

# ---- zip (forward-slash entry names, checked) ------------------------------------
$zipPath = "dist/FrameTrace-$Version-win64-bundled.zip"
Remove-Item -Force $zipPath -ErrorAction SilentlyContinue
$fullStage = (Resolve-Path $stage).Path
$fullZip = Join-Path (Resolve-Path "dist").Path ([System.IO.Path]::GetFileName($zipPath))
[System.IO.Compression.ZipFile]::CreateFromDirectory($fullStage, $fullZip, [System.IO.Compression.CompressionLevel]::Optimal, $true)
$check = [System.IO.Compression.ZipFile]::OpenRead($fullZip)
try {
    foreach ($entry in $check.Entries) {
        if ($entry.FullName.Contains('\')) {
            $check.Dispose(); $check = $null
            Remove-Item -Force $fullZip
            throw "package zip entry uses a backslash: '$($entry.FullName)' (other unzip tools would not see folders)"
        }
        if (-not (Test-SafeEntry $entry.FullName)) { throw "package zip has an unsafe entry '$($entry.FullName)'" }
    }
} finally {
    if ($check) { $check.Dispose() }
}
$dist = (Resolve-Path "dist").Path
$sumLines = @(Get-ChildItem $dist -Filter *.zip | ForEach-Object {
    "{0}  {1}" -f (Get-Sha256Hex $_.FullName), $_.Name
})
[System.IO.File]::WriteAllText((Join-Path $dist "SHA256SUMS"), (($sumLines -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "package: $zipPath"
