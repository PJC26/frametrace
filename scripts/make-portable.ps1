# Builds release binaries and the portable Windows zip for examiners.
#
# The package contains FrameTrace only. FFmpeg (ffmpeg / ffprobe / ffplay and
# its shared libraries) is NEVER packaged: the build the guided installer
# fetches is not redistributable by us (docs/PLAN_TOOLS_C_2026-10-20.md T5).
# The examiner runs `frametrace.exe tools install ffmpeg` (or the home
# screen's install button) after unzipping, or keeps ffmpeg on PATH. Any
# such file found in the tools folder is skipped with a warning, and the
# written zip is checked again — one FFmpeg entry deletes it and fails.
#
# The zip is written with [IO.Compression.ZipFile]::CreateFromDirectory and
# every entry name is checked to use forward slashes. SHA-256 is computed
# through .NET (`Get-FileHash` fails to auto-load in Windows PowerShell 5.1
# started from pwsh 7 — GitHub runners).
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/make-portable.ps1 [-SkipBuild] [-Version <ver>]
#   -SkipBuild  use the existing target/release binaries (CI builds them first)
#   -Version    package version (default: Cargo.toml via cargo metadata)
param(
    [switch]$SkipBuild,
    [string]$Version = ""
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

# FFmpeg files that must never be packaged (relative, `/`-separated path).
function Test-FfmpegFile([string]$rel) {
    $leaf = ($rel.TrimEnd('/') -split '/')[-1]
    if ($leaf -match '^(ffmpeg|ffprobe|ffplay)[^/]*\.exe$') { return $true }
    if ($leaf -match '^(avcodec|avformat|avutil|avfilter|avdevice|swscale|swresample|postproc)-\d+\.dll$') { return $true }
    if ($rel -match '(^|/)licenses/ffmpeg-') { return $true }
    return $false
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

# ---- zip (forward-slash entry names, no FFmpeg — both checked) ----------------
$dist = (Resolve-Path "dist").ProviderPath
$fullStage = (Resolve-Path -LiteralPath $stage).ProviderPath
$fullZip = Join-Path $dist "FrameTrace-$Version-win64.zip"
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

# SHA256SUMS covering every zip in dist/ — same manifest convention as
# scripts/build-release.sh; consumers verify with `sha256sum -c` (see
# docs/repro-build.md). LF endings, no BOM (`sha256sum -c` on Linux/macOS).
$sumLines = @(Get-ChildItem $dist -Filter *.zip | Sort-Object Name | ForEach-Object {
    "{0}  {1}" -f (Get-Sha256Hex $_.FullName), $_.Name
})
[System.IO.File]::WriteAllText((Join-Path $dist "SHA256SUMS"), (($sumLines -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "package: $fullZip"
Write-Host "sums: $(Join-Path $dist 'SHA256SUMS')"
