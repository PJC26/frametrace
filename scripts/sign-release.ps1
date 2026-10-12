# Signs FrameTrace release binaries.
#
# Real distribution needs an OV/EV code-signing certificate bought from a CA —
# that is a purchase decision, not something a script can fabricate. This
# script covers both cases:
#
#   powershell -File scripts/sign-release.ps1 -Thumbprint <cert-thumbprint>
#       Sign with an existing certificate (real CA cert or prior self-signed).
#
#   powershell -File scripts/sign-release.ps1 -SelfSigned
#       Create a local self-signed "FrameTrace Dev" cert (CurrentUser) and
#       sign with it. Removes "unknown publisher" on THIS machine after the
#       cert is trusted once — it does NOT remove SmartScreen on other PCs.
#
# Signs: dist/FrameTrace-*-win64/*.exe (or -TargetDir override).

param(
    [string]$Thumbprint = "",
    [switch]$SelfSigned,
    [switch]$TrustSelfSigned,
    [string]$TargetDir = ""
)

$ErrorActionPreference = "Stop"

$signtool = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match "x64" } | Select-Object -First 1 -ExpandProperty FullName
if (-not $signtool) { throw "signtool.exe not found — install the Windows SDK" }

if ($SelfSigned) {
    $existing = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -eq "CN=FrameTrace Dev" } | Select-Object -First 1
    if ($existing) {
        $Thumbprint = $existing.Thumbprint
        Write-Host "reusing existing self-signed cert: $Thumbprint"
    } else {
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=FrameTrace Dev" `
            -CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy NonExportable `
            -NotAfter (Get-Date).AddYears(3)
        $Thumbprint = $cert.Thumbprint
        Write-Host "created self-signed cert: $Thumbprint"
        Write-Host "NOTE: 이 인증서는 이 PC 전용입니다. 다른 PC의 SmartScreen을 없애려면 CA 인증서가 필요합니다."
        if ($TrustSelfSigned) {
            # Adding to Root can pop a Windows security confirmation dialog —
            # only do it when explicitly requested.
            $store = New-Object System.Security.Cryptography.X509Certificates.X509Store("Root", "CurrentUser")
            $store.Open("ReadWrite"); $store.Add($cert); $store.Close()
            Write-Host "cert trusted in CurrentUser\Root (this machine only)"
        }
    }
}

if (-not $Thumbprint) {
    throw "cert thumbprint required — pass -Thumbprint <hash> or -SelfSigned"
}

if (-not $TargetDir) {
    $candidate = Get-ChildItem "dist" -Directory -Filter "FrameTrace-*-win64" -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1
    if ($candidate) { $TargetDir = $candidate.FullName }
}
if (-not $TargetDir -or -not (Test-Path -LiteralPath $TargetDir)) {
    throw "no dist package found — run scripts/make-portable.ps1 first, or pass -TargetDir"
}

$exes = Get-ChildItem -LiteralPath $TargetDir -Filter "*.exe" -Recurse
if (-not $exes) { throw "no .exe under $TargetDir" }

foreach ($exe in $exes) {
    & $signtool sign /fd SHA256 /sha1 $Thumbprint /tr http://timestamp.digicert.com /td SHA256 $exe.FullName
    if ($LASTEXITCODE -ne 0) {
        Write-Host "timestamped signing failed on $($exe.Name) — retrying without timestamp"
        & $signtool sign /fd SHA256 /sha1 $Thumbprint $exe.FullName
        if ($LASTEXITCODE -ne 0) { throw "signtool failed on $($exe.Name) (exit $LASTEXITCODE)" }
        Write-Host "signed (no timestamp): $($exe.Name)"
    } else {
        Write-Host "signed: $($exe.Name)"
    }
}
Write-Host "done: $($exes.Count) file(s) signed in $TargetDir"
