# Builds release binaries and a portable zip for examiners.
# Usage: powershell -ExecutionPolicy Bypass -File scripts/make-portable.ps1
$ErrorActionPreference = "Stop"
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed (exit code $LASTEXITCODE)" }
$version = (cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages[0].version
$stage = "dist/FrameTrace-$version-win64"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path "$stage/tools/bin" | Out-Null
Copy-Item target/release/frametrace.exe $stage/
Copy-Item target/release/frametrace-app.exe $stage/
Copy-Item README.md $stage/
New-Item -ItemType Directory -Path "$stage/docs" -Force | Out-Null
Copy-Item docs/WINDOWS_USAGE.md $stage/docs/
Copy-Item docs/WINDOWS_VALIDATION.md $stage/docs/ -ErrorAction SilentlyContinue
Copy-Item scripts/install.ps1 $stage/
# Bundle vendored tool binaries (libewf / Sleuth Kit + their DLLs) when the
# repo has them, so E01 and deleted-file workflows work out of the box.
# tools/bin (checked in or hand-assembled) wins; scripts/build-libewf-tools.ps1
# installs to target/release/tools/bin, so fall back to that.
# ffmpeg/ffprobe are still expected on PATH or dropped into tools/bin by the
# examiner (they are large and commonly already installed).
$toolsSrc = $null
foreach ($cand in @("tools/bin", "target/release/tools/bin")) {
    if (Test-Path $cand) { $toolsSrc = $cand; break }
}
if ($toolsSrc) {
    Write-Host "bundling tools from $toolsSrc"
    Copy-Item "$toolsSrc/*" "$stage/tools/bin/" -Recurse -Force
} else {
    Write-Warning "no tools/bin or target/release/tools/bin found - the package ships WITHOUT libewf/Sleuth Kit tools (E01 and deleted-file workflows need them). Run scripts/build-libewf-tools.ps1 first or copy the tools into tools/bin."
}
@'
FrameTrace portable package
===========================
1. tools/bin 아래의 도구 실행 파일은 PATH 설정 없이 자동 인식됩니다.
   - ewfinfo.exe / ewfverify.exe / ewfexport.exe (E01) — 패키지에 포함된 경우 그대로 동작
   - mmls.exe / fls.exe / icat.exe (Sleuth Kit) — 동일
   - ffmpeg.exe / ffprobe.exe — PATH에 없으면 이 폴더에 넣으십시오
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
Compress-Archive -Path $stage -DestinationPath "$stage.zip" -Force
# SHA256SUMS covering every zip in dist/ — same manifest convention as
# scripts/build-release.sh; consumers verify with `Get-FileHash` or
# `sha256sum -c` (see docs/repro-build.md).
# Written with LF endings and no BOM: Out-File on PS 5.1 emits CRLF, which
# breaks `sha256sum -c` on Linux/macOS.
$dist = Split-Path $stage
$sumLines = @(Get-ChildItem $dist -Filter *.zip | ForEach-Object {
    "{0}  {1}" -f ((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()), $_.Name
})
$sumsPath = Join-Path (Resolve-Path $dist).Path "SHA256SUMS"
[System.IO.File]::WriteAllText($sumsPath, (($sumLines -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "package: $stage.zip"
Write-Host "sums: $dist/SHA256SUMS"
