# Hikvision IMKH sample intake harness.
# Walks exports under -Samples, remuxes via frametrace export-hik, then validates.
#
# Usage:
#   powershell -File scripts/validate-hik-samples.ps1 -Samples C:\Evidence\hik-corpus

param(
	[Parameter(Mandatory = $true)]
	[string]$Samples,
	[string]$Exe = "",
	[string]$WorkRoot = "$env:TEMP\frametrace-hik-validation",
	[int]$TimeoutSecs = 120
)

$ErrorActionPreference = "Stop"

function Resolve-FrametraceExe {
	param([string]$Preferred)
	if ($Preferred -and (Test-Path -LiteralPath $Preferred)) {
		return (Resolve-Path -LiteralPath $Preferred).Path
	}
	foreach ($candidate in @(".\target\release\frametrace.exe", ".\target\debug\frametrace.exe")) {
		if (Test-Path -LiteralPath $candidate) {
			return (Resolve-Path -LiteralPath $candidate).Path
		}
	}
	throw "frametrace.exe not found. Build with 'cargo build --release' or pass -Exe."
}

if (-not (Test-Path -LiteralPath $Samples)) {
	throw "Samples folder not found: $Samples"
}

$files = @(Get-ChildItem -LiteralPath $Samples -Recurse -File |
	Where-Object {
		$_.Extension -match '\.(mp4|mpg|mpeg|avi|ps)$' -or
		((Get-Content -LiteralPath $_.FullName -Encoding Byte -TotalCount 4 -ErrorAction SilentlyContinue) -join ',') -eq '73,77,75,72'
	} | Sort-Object FullName)

# 73,77,75,72 = IMKH ASCII bytes
if ($files.Count -eq 0) {
	Write-Host "BLOCKED: no IMKH/Hikvision export candidates under $Samples"
	Write-Host "Place recorder/iVMS exports (IMKH-prefixed) and re-run."
	exit 2
}

$exePath = Resolve-FrametraceExe -Preferred $Exe
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$caseDir = Join-Path $WorkRoot "case-$stamp"
$reportPath = Join-Path $WorkRoot "hik-validation-$stamp.jsonl"
New-Item -ItemType Directory -Force -Path $caseDir | Out-Null
& $exePath init-case $caseDir --title "Hikvision sample validation $stamp" | Out-Host

$passed = 0
$failed = 0
foreach ($file in $files) {
	Write-Host "----"
	Write-Host "sample: $($file.FullName)"
	$entry = [ordered]@{ sample = $file.FullName; size_bytes = $file.Length; status = "fail"; error = $null }
	try {
		$header = Get-Content -LiteralPath $file.FullName -Encoding Byte -TotalCount 4
		$headerText = [System.Text.Encoding]::ASCII.GetString($header)
		if ($headerText -ne "IMKH") {
			throw "not IMKH (header='$headerText'); skip non-IMKH media or convert with vendor tools first"
		}
		& $exePath export-hik $caseDir $file.FullName --timeout $TimeoutSecs | Out-Host
		if ($LASTEXITCODE -ne 0) { throw "export-hik exit $LASTEXITCODE" }
		$stem = [System.IO.Path]::GetFileNameWithoutExtension($file.Name)
		$mp4 = Get-ChildItem -LiteralPath (Join-Path $caseDir "artifacts\clips") -Filter "$stem*.mp4" |
			Sort-Object LastWriteTime -Descending | Select-Object -First 1
		if (-not $mp4) { throw "remuxed mp4 missing" }
		& $exePath validate-artifact $caseDir $mp4.FullName | Out-Host
		if ($LASTEXITCODE -ne 0) { throw "validate-artifact exit $LASTEXITCODE" }
		$validationLog = Get-Content -LiteralPath (Join-Path $caseDir "evidence\logs\validation-log.jsonl") -Raw
		if ($validationLog -notmatch "ffprobe-video-stream-confirmed") {
			throw "validation log missing ffprobe-video-stream-confirmed"
		}
		$entry.status = "pass"
		$passed++
	}
	catch {
		$entry.error = "$_"
		$failed++
		Write-Host "FAIL: $_"
	}
	($entry | ConvertTo-Json -Compress) | Add-Content -LiteralPath $reportPath -Encoding utf8
}

Write-Host "===="
Write-Host "samples: $($files.Count)  pass: $passed  fail: $failed"
Write-Host "report: $reportPath"
if ($failed -gt 0) { exit 1 }
if ($passed -lt 1) { exit 2 }
exit 0
