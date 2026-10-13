# Windows Validation

FrameTrace targets Windows 10/11 x64 with the MSVC Rust toolchain.

## Local Windows Commands

Run these from the repository root on Windows:

```powershell
rustup component add clippy rustfmt
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
.\target\release\frametrace.exe benchmark-db C:\Temp\frametrace-db-bench --rows 1000000
```

Then run a smoke case with a small sample video:

```powershell
.\target\release\frametrace.exe init-case C:\Temp\frametrace-case --title "Windows smoke"
.\target\release\frametrace.exe register-source C:\Temp\frametrace-case C:\Temp\sample-media --kind folder --write-protect "copied evidence"
.\target\release\frametrace.exe scan-folder C:\Temp\frametrace-case C:\Temp\sample-media --no-ffprobe
.\target\release\frametrace.exe make-review C:\Temp\frametrace-case
.\target\release\frametrace.exe make-report C:\Temp\frametrace-case
.\target\release\frametrace.exe package-case C:\Temp\frametrace-case
.\target\release\frametrace.exe inspect C:\Temp\frametrace-case
```

## CI

The repository includes `.github/workflows/windows-ci.yml`, which runs format, clippy, tests, and a release build on `windows-latest`.

The current macOS validation can prove Rust behavior and command contracts, but the Windows MSVC release build is only fully closed when the workflow or a local Windows machine passes.

## 2026-10-12 수정 후 수동 확인

Windows 실기에서 아래 항목을 직접 확인하고 결과를 `docs/HANDOFF-2026-10-11.md` 검증 기록에 남긴다.

- [ ] 분석·도구 실행 중 콘솔(cmd) 창이 뜨지 않음
- [ ] 작업 중 창 닫기 → 확인창 표시 (예 = 취소 후 종료, 아니오 = 계속). 종료 후 작업 관리자에 `frametrace`/`ffmpeg` 프로세스 잔존 없음
- [ ] 스캔 중에도 그리드가 채워짐
- [ ] Edge를 미리 열어 둔 채 워크스테이션을 종료해도 Edge 창이 유지됨
- [ ] UNC 경로(`\\server\share`)는 미디어/찾아보기에서 거부됨
