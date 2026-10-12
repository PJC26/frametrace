# FrameTrace portable installer.
# Copies this package to %LOCALAPPDATA%\Programs\FrameTrace, creates a
# Start Menu shortcut (and optional desktop shortcut), then unblocks the
# binaries so Windows does not re-prompt on every launch.
#
# Usage (from the extracted package folder):
#   powershell -ExecutionPolicy Bypass -File install.ps1
#   powershell -ExecutionPolicy Bypass -File install.ps1 -DesktopShortcut
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Uninstall

param(
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\FrameTrace",
    [switch]$DesktopShortcut,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$src = Split-Path -Parent $MyInvocation.MyCommand.Path
$appName = "FrameTrace"
$exeName = "frametrace-app.exe"
$linkName = "$appName.lnk"

function Remove-Install {
    param([string]$Dir)
    foreach ($lnk in @(
        (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\$linkName"),
        (Join-Path $env:USERPROFILE "Desktop\$linkName")
    )) {
        if (Test-Path -LiteralPath $lnk) { Remove-Item -LiteralPath $lnk -Force }
    }
    if (Test-Path -LiteralPath $Dir) {
        # Refuse to delete a directory that still holds user case folders.
        $caseDirs = Get-ChildItem -LiteralPath $Dir -Recurse -Filter case.json -ErrorAction SilentlyContinue
        if ($caseDirs) {
            throw "케이스 데이터가 남아 있습니다 ($($caseDirs.Count)개 case.json). 먼저 케이스 폴더를 다른 곳으로 옮기십시오."
        }
        Remove-Item -LiteralPath $Dir -Recurse -Force
    }
    Write-Host "FrameTrace 제거 완료."
}

if ($Uninstall) {
    Remove-Install -Dir $InstallDir
    return
}

if (-not (Test-Path -LiteralPath (Join-Path $src $exeName))) {
    throw "install.ps1은 배포 패키지 폴더에서 실행하십시오 ($exeName 없음: $src)"
}

# Warn when SmartScreen reputation may still prompt — unsigned builds are
# expected to show "알 수 없는 게시자" once; unblocking after copy prevents
# repeat prompts.
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
# -Path (not -LiteralPath) so the * wildcard expands.
Copy-Item -Path (Join-Path $src "*") -Destination $InstallDir -Recurse -Force
if (-not (Test-Path -LiteralPath (Join-Path $InstallDir $exeName))) {
    throw "copy failed — $exeName not found under $InstallDir"
}

# Unblock every copied file so Zone.Identifier MOTW does not re-trigger
# SmartScreen on each tool binary.
Get-ChildItem -LiteralPath $InstallDir -Recurse -File -ErrorAction SilentlyContinue |
    Unblock-File -ErrorAction SilentlyContinue

# Start Menu shortcut → windowed launcher.
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
New-Item -ItemType Directory -Path $startMenu -Force | Out-Null
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut((Join-Path $startMenu $linkName))
$shortcut.TargetPath = (Join-Path $InstallDir $exeName)
$shortcut.WorkingDirectory = $InstallDir
$shortcut.Description = "FrameTrace 영상 증거 검수 워크스테이션"
$shortcut.Save()

if ($DesktopShortcut) {
    $desktop = $shell.CreateShortcut((Join-Path $env:USERPROFILE "Desktop\$linkName"))
    $desktop.TargetPath = (Join-Path $InstallDir $exeName)
    $desktop.WorkingDirectory = $InstallDir
    $desktop.Description = "FrameTrace 영상 증거 검수 워크스테이션"
    $desktop.Save()
}

Write-Host ""
Write-Host "설치 완료: $InstallDir"
Write-Host "시작 메뉴: $appName"
Write-Host "첫 실행 시 SmartScreen이 뜨면 '추가 정보' → '실행'을 선택하십시오."
Write-Host "도구(ffmpeg/libewf/Sleuth Kit)는 $InstallDir\tools\bin 에 넣으면 자동 인식됩니다."
