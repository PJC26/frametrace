# Seed synthetic DAV + IMKH fixtures outside git, then run intake harnesses.
# These are NOT recorder exports — they prove the intake path end-to-end.
# Usage: powershell -File scripts/seed-and-validate-synth-samples.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$ffmpegCmd = Get-Command ffmpeg -ErrorAction SilentlyContinue
if (-not $ffmpegCmd) { throw "ffmpeg not on PATH" }
$ffmpeg = $ffmpegCmd.Source

$exe = Join-Path $root "target\release\frametrace.exe"
if (-not (Test-Path $exe)) {
	cargo build --release --locked
}

$davDir = "C:\Temp\frametrace-dav-samples"
$hikDir = "C:\Temp\frametrace-hik-samples"
New-Item -ItemType Directory -Force -Path $davDir, $hikDir | Out-Null

# --- DAV: DHAV frame wrapping real H.264 (FFmpeg-aligned skeleton) ---
$es = Join-Path $env:TEMP "ft-seed.h264"
& ffmpeg -y -hide_banner -loglevel error -f lavfi -i testsrc=s=160x90:r=6:d=1 -c:v libx264 -pix_fmt yuv420p -f h264 $es
$h264 = [System.IO.File]::ReadAllBytes($es)
function New-DavBytes([byte[]]$payload) {
	$headerLen = 24
	$frameLen = $headerLen + $payload.Length + 8
	$buf = New-Object byte[] $frameLen
	[Text.Encoding]::ASCII.GetBytes("DHAV").CopyTo($buf, 0)
	$buf[4] = 0xFD
	$buf[6] = 1
	[BitConverter]::GetBytes([uint32]1).CopyTo($buf, 8)
	[BitConverter]::GetBytes([uint32]$frameLen).CopyTo($buf, 12)
	$payload.CopyTo($buf, 24)
	[Text.Encoding]::ASCII.GetBytes("dhav").CopyTo($buf, 24 + $payload.Length)
	[BitConverter]::GetBytes([uint32]$frameLen).CopyTo($buf, 24 + $payload.Length + 4)
	return $buf
}
@("continuous", "event", "parking") | ForEach-Object {
	$path = Join-Path $davDir "synth_$_.dav"
	[System.IO.File]::WriteAllBytes($path, (New-DavBytes $h264))
}

# --- Hikvision IMKH + MPEG-PS ---
$mpg = Join-Path $env:TEMP "ft-seed.mpg"
& ffmpeg -y -hide_banner -loglevel error -f lavfi -i testsrc=s=160x90:r=5:d=1 -c:v mpeg2video -f mpeg $mpg
$mpgBytes = [System.IO.File]::ReadAllBytes($mpg)
1..3 | ForEach-Object {
	$header = New-Object byte[] 40
	[Text.Encoding]::ASCII.GetBytes("IMKH").CopyTo($header, 0)
	$out = New-Object byte[] ($header.Length + $mpgBytes.Length)
	$header.CopyTo($out, 0)
	$mpgBytes.CopyTo($out, 40)
	[System.IO.File]::WriteAllBytes((Join-Path $hikDir ("synth_cam{0:D2}.mpg" -f $_)), $out)
}

Write-Host "seeded DAV -> $davDir"
Write-Host "seeded HIK -> $hikDir"

powershell -File (Join-Path $root "scripts\validate-dav-samples.ps1") -Samples $davDir -Exe $exe
$davCode = $LASTEXITCODE
powershell -File (Join-Path $root "scripts\validate-hik-samples.ps1") -Samples $hikDir -Exe $exe
$hikCode = $LASTEXITCODE

Write-Host "dav harness exit=$davCode  hik harness exit=$hikCode"
if ($davCode -ne 0 -or $hikCode -ne 0) { exit 1 }
exit 0
