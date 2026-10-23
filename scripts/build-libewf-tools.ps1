# Builds libewf tools (ewfacquire/ewfinfo/ewfverify/ewfexport) from source
# inside a per-user MSYS2 environment (no admin required) and installs them
# into target/release/tools/bin (copy to tools/bin to bundle them with
# scripts/make-portable.ps1, or put them next to the release binary).
#
# Usage: powershell -ExecutionPolicy Bypass -File scripts/build-libewf-tools.ps1
# Notes:
#   - libewf upstream ships source only (GitHub/SourceForge), so building is
#     the supported way to get Windows binaries.
#   - Takes ~10-20 minutes; downloads ~350 MB (MSYS2 + gcc).
#   - The produced tools link libewf-3.dll dynamically; every non-system DLL
#     they need (zlib1.dll, libwinpthread-1.dll, libgcc_s_seh-1.dll, ...) is
#     discovered with objdump and copied next to them.
#   - Downloads are verified against pinned SHA256 values. The pins were
#     computed from the downloaded files (trust-on-first-use), not taken from
#     a vendor-published digest; re-check them when bumping versions.
param(
    [string]$Msys2Dir = "$env:USERPROFILE\msys64",
    [string]$LibewfVersion = "20240506",
    [string]$LibewfSha256 = "",
    # Dated MSYS2 base image (NOT the moving "latest" URL) so the hash is stable.
    [string]$Msys2Release = "20260927",
    [string]$Msys2Sha256 = "",
    # Install folder for the built tools. Empty keeps the developer layout
    # (target/release/tools/bin + target/debug/deps/tools/bin); the
    # workstation's in-app installer passes its own staging folder.
    [string]$Destination = ""
)
$ErrorActionPreference = "Stop"

# Known-good pins. Add a row when bumping a version.
$libewfPins = @{ "20240506" = "247d8ee9572392a2404be514d1137f099970f41f240c1134ddc3f04322281c67" }
$msys2Pins  = @{ "20260927" = "ad336cccfda47758b5e15cda993fbba421115cb0b126697daef1ee4dfe37209f" }
if (-not $LibewfSha256) { $LibewfSha256 = $libewfPins[$LibewfVersion] }
if (-not $Msys2Sha256)  { $Msys2Sha256  = $msys2Pins[$Msys2Release] }
if (-not $LibewfSha256) { throw "no pinned SHA256 for libewf $LibewfVersion; pass -LibewfSha256 after checking the tarball" }
if (-not $Msys2Sha256)  { throw "no pinned SHA256 for MSYS2 $Msys2Release; pass -Msys2Sha256 after checking the installer" }

$repoRoot = Split-Path -Parent $PSScriptRoot

# Windows PowerShell 5.1 turns any native stderr output into a terminating
# error under $ErrorActionPreference = "Stop" when stderr is redirected with
# 2>&1. Run native commands with "Continue", flatten ErrorRecords to text and
# judge success by the real exit code only.
function Invoke-Native {
    param([scriptblock]$Command, [string]$What, [switch]$Quiet)
    $saved = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $global:LASTEXITCODE = 0
    try {
        $lines = & $Command 2>&1 | ForEach-Object { "$_" }
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $saved
    }
    if ($code -ne 0) {
        $lines | Select-Object -Last 20 | ForEach-Object { Write-Host $_ }
        throw "$What failed (exit code $code)"
    }
    if (-not $Quiet) { $lines | Select-Object -Last 2 | ForEach-Object { Write-Host $_ } }
    return $lines
}

function Assert-Sha256 {
    param([string]$Path, [string]$Expected, [string]$What)
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
    if ($actual -ne $Expected.ToLowerInvariant()) {
        throw "$What SHA256 mismatch: expected $Expected, got $actual ($Path)"
    }
    Write-Host "$What SHA256 verified"
}

# 1. MSYS2 bootstrap
if (-not (Test-Path "$Msys2Dir/usr/bin/bash.exe")) {
    Write-Host "downloading MSYS2 $Msys2Release bootstrap..."
    New-Item -ItemType Directory -Force -Path (Split-Path $Msys2Dir) | Out-Null
    $sfx = Join-Path $env:TEMP "msys2-base-x86_64-$Msys2Release.sfx.exe"
    Invoke-WebRequest -Uri "https://repo.msys2.org/distrib/x86_64/msys2-base-x86_64-$Msys2Release.sfx.exe" -OutFile $sfx
    try {
        Assert-Sha256 $sfx $Msys2Sha256 "MSYS2 installer"
    } catch {
        Remove-Item -Force $sfx -ErrorAction SilentlyContinue
        throw
    }
    Push-Location (Split-Path $Msys2Dir)
    try {
        $null = Invoke-Native { & $sfx -y } "MSYS2 extraction" -Quiet
    } finally {
        Pop-Location
    }
    if (-not (Test-Path "$Msys2Dir/usr/bin/bash.exe")) { throw "MSYS2 extraction failed" }
}

$bash = "$Msys2Dir/usr/bin/bash.exe"
$env:MSYSTEM = "MINGW64"

# 2. First-run init + keyring
$null = Invoke-Native { & $bash -lc "pacman-key --init && pacman-key --populate msys2" } "pacman keyring init"

# 3. mingw64 toolchain
$null = Invoke-Native { & $bash -lc "pacman -S --noconfirm --needed mingw-w64-x86_64-gcc make" } "pacman toolchain install"
# pacman-key/pacman start a gpg-agent daemon that outlives this script and
# keeps the caller's stdout/stderr handles open; stop it (best effort).
$null = Invoke-Native { & $bash -lc "gpgconf --kill all || true" } "gpg-agent shutdown" -Quiet

# 4. libewf source (pinned SHA256)
$srcRoot = "$Msys2Dir/build"
New-Item -ItemType Directory -Force -Path $srcRoot | Out-Null
$tarball = "$srcRoot/libewf-experimental-$LibewfVersion.tar.gz"
if (-not (Test-Path $tarball)) {
    Invoke-WebRequest -Uri "https://github.com/libyal/libewf/releases/download/$LibewfVersion/libewf-experimental-$LibewfVersion.tar.gz" -OutFile $tarball
}
try {
    Assert-Sha256 $tarball $LibewfSha256 "libewf source tarball"
} catch {
    Remove-Item -Force $tarball -ErrorAction SilentlyContinue
    throw
}
if (-not (Test-Path "$srcRoot/libewf-$LibewfVersion")) {
    $null = Invoke-Native { tar -xzf $tarball -C $srcRoot } "libewf source extraction" -Quiet
}

# 5. build + install. bash exits non-zero if configure, make or make install
# fails (set -e); the exit code is checked by Invoke-Native.
$cygpathOut = Invoke-Native { & $bash -lc ("cygpath -u '" + $srcRoot + "'") } "cygpath" -Quiet | Select-Object -Last 1
if ($null -eq $cygpathOut -or [string]::IsNullOrWhiteSpace([string]$cygpathOut)) {
    throw "cygpath -u '$srcRoot' printed nothing - cannot map the source folder to an MSYS2 path (is MSYS2's bash/cygpath working?)"
}
$srcUnix = ([string]$cygpathOut).Trim()
$buildCmd = "set -e; export MSYSTEM=MINGW64; cd '$srcUnix/libewf-$LibewfVersion'; " +
    "./configure --prefix='$srcUnix/out' --disable-nls > configure.log 2>&1; " +
    "make -j`$(nproc) > make.log 2>&1; " +
    "make install > install.log 2>&1"
$null = Invoke-Native { & $bash -lc $buildCmd } "libewf build (see $srcRoot/libewf-$LibewfVersion/*.log)" -Quiet

# 6. collect binaries + the runtime DLLs they need
$outBin = "$srcRoot/out/bin"
$exes = @("ewfacquire.exe", "ewfinfo.exe", "ewfverify.exe", "ewfexport.exe")
$files = New-Object System.Collections.Generic.List[string]
foreach ($e in $exes) {
    if (-not (Test-Path "$outBin/$e")) { throw "build did not produce $outBin/$e" }
    $files.Add("$outBin/$e")
}
if (-not (Test-Path "$outBin/libewf-3.dll")) { throw "build did not produce $outBin/libewf-3.dll" }
$files.Add("$outBin/libewf-3.dll")

# Transitive DLL closure: any imported DLL that exists in the build's bin or
# MSYS2's mingw64/bin is bundled; Windows system DLLs (KERNEL32, msvcrt, ...)
# are not found there and are left to the OS.
$mingwBin = "$Msys2Dir/mingw64/bin"
$objdump = "$mingwBin/objdump.exe"
$dllSearch = @($outBin, $mingwBin)
$seen = @{}
if (Test-Path $objdump) {
    $queue = New-Object System.Collections.Generic.Queue[string]
    foreach ($f in $files) { $queue.Enqueue($f) }
    while ($queue.Count -gt 0) {
        $f = $queue.Dequeue()
        $imports = Invoke-Native { & $objdump -p $f } "objdump $f" -Quiet |
            ForEach-Object { if ($_ -match 'DLL Name:\s*(\S+)') { $Matches[1] } }
        foreach ($name in $imports) {
            if ($seen.ContainsKey($name.ToLowerInvariant())) { continue }
            $seen[$name.ToLowerInvariant()] = $true
            foreach ($d in $dllSearch) {
                $cand = Join-Path $d $name
                if (Test-Path $cand) {
                    if (-not ($files -contains $cand) -and -not ($files | Where-Object { (Split-Path $_ -Leaf) -ieq $name })) {
                        $files.Add($cand); $queue.Enqueue($cand)
                    }
                    break
                }
            }
        }
    }
} else {
    Write-Warning "objdump not found at $objdump; falling back to the usual MinGW runtime DLLs"
    foreach ($n in @("zlib1.dll", "libwinpthread-1.dll", "libgcc_s_seh-1.dll")) {
        $cand = Join-Path $mingwBin $n
        if (Test-Path $cand) { $files.Add($cand) } else { Write-Warning "optional runtime DLL $n not found" }
    }
}

# 7. deploy to tools/bin layout (or the caller-chosen -Destination)
if ($Destination) {
    $dest = $Destination
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    foreach ($f in $files) { Copy-Item -LiteralPath $f -Destination $dest -Force }
} else {
    $dest = Join-Path $repoRoot "target/release/tools/bin"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    foreach ($f in $files) { Copy-Item -LiteralPath $f -Destination $dest -Force }
    # integration tests discover tools/bin next to the test binary too
    $depsDest = Join-Path $repoRoot "target/debug/deps/tools/bin"
    New-Item -ItemType Directory -Force -Path $depsDest | Out-Null
    Copy-Item "$dest/*" $depsDest -Force
}
Write-Host ("bundled: " + (($files | ForEach-Object { Split-Path $_ -Leaf }) -join ", "))
Write-Host "libewf tools installed: $dest"
Write-Host "verify: $dest/ewfinfo.exe -V"
