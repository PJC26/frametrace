# Windows 구현 인수인계

이 문서는 FrameTrace를 Windows 환경에서 이어서 구현/검증하기 위한 작업표다.

현재 Rust 코어는 macOS에서 케이스 생성, 폴더 스캔, E01 import, Sleuth Kit 이미지 조사, inode 복구, carving, `ffprobe` 검증, 보고서, 패키징, 실제 케이스 기반 Evidence Viewer 생성까지 검증된 상태다. Windows에서는 프로젝트를 새로 만들지 말고, 현재 엔진을 Windows에서 검증하고 보강한 뒤 최종 GUI shell을 붙이면 된다.

GUI 또는 Windows 배포 구현을 시작하기 전에 `docs/WINDOWS_RISK_REVIEW.md`도 함께 읽는다. 진행률/ETA, 중단 재개, 디스크 공간, dependency checker, audit chain 검증, 대량 리스트 성능은 production GUI 요구사항으로 취급한다.

## 0. Git에서 시작

Windows 작업 PC에서:

```powershell
git clone https://github.com/daeryundf2-prog/frametrace.git
cd frametrace
git status
git log -1 --oneline
```

최소 기준 커밋:

이 문서가 작성된 이후 저장소 이력이 재구성되어 당시 기준 커밋(f5c95f5)은 현재
히스토리에 존재하지 않는다. **현재 `main` HEAD를 기준으로 진행**하되, 먼저 이 문서를
끝까지 읽고 작업 순서를 맞춘다. 진행 전 `docs/ROADMAP-v3.md`도 함께 확인한다.

## 1. Windows 필수 도구 설치

PowerShell에서 아래 실행 파일들이 모두 잡혀야 한다.

필수:

- Rust stable MSVC toolchain
- Visual Studio Build Tools
- FFmpeg for Windows
  - `ffmpeg.exe`
  - `ffprobe.exe`

E01 / raw image / 삭제 파일 복구 작업에 필요:

- libewf command-line tools
  - `ewfinfo.exe`
  - `ewfverify.exe`
  - `ewfexport.exe`
- The Sleuth Kit
  - `mmls.exe`
  - `fls.exe`
  - `icat.exe`
- 읽기 전용 forensic image mounter

확인 명령:

```powershell
rustc --version
cargo --version
ffmpeg -version
ffprobe -version
ewfinfo -V
ewfverify -V
ewfexport -V
mmls -V
fls -V
icat -V
```

## 2. Phase 1 - Windows 빌드와 CLI 기본 검증

목표: 현재 엔진이 Windows MSVC 환경에서 깨지지 않는지 먼저 확인한다.

```powershell
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
.\target\release\frametrace.exe --help
.\target\release\frametrace.exe list-parsers
```

통과 기준:

- 빌드, 테스트, clippy 실패가 없어야 한다.
- `frametrace.exe --help`에 현재 명령들이 보여야 한다.
- `list-parsers`가 제조사/소스 파서 카탈로그 JSON을 출력해야 한다.

여기서 실패하면 GUI 작업을 시작하지 말고 Windows portability부터 고친다.

## 3. Phase 2 - 가짜 영상으로 전체 워크플로우 검증

목표: 실제 의뢰인 증거 없이 Windows 로컬 워크플로우가 끝까지 도는지 확인한다.

테스트 MP4 생성:

```powershell
New-Item -ItemType Directory -Force C:\Temp\frametrace-source | Out-Null
ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc=size=160x90:rate=1 -t 1 -pix_fmt yuv420p C:\Temp\frametrace-source\sample.mp4
```

전체 흐름 실행:

```powershell
.\target\release\frametrace.exe init-case C:\Temp\frametrace-case --title "Windows smoke"
.\target\release\frametrace.exe scan-folder C:\Temp\frametrace-case C:\Temp\frametrace-source --hash
.\target\release\frametrace.exe validate-artifact C:\Temp\frametrace-case vid_000001
.\target\release\frametrace.exe make-review C:\Temp\frametrace-case
.\target\release\frametrace.exe make-report C:\Temp\frametrace-case
.\target\release\frametrace.exe package-case C:\Temp\frametrace-case
```

생성 확인:

```text
C:\Temp\frametrace-case\db\case.db
C:\Temp\frametrace-case\db\video_index.json
C:\Temp\frametrace-case\evidence\logs\validation-log.jsonl
C:\Temp\frametrace-case\review\index.html
C:\Temp\frametrace-case\review\evidence-viewer.html
C:\Temp\frametrace-case\reports\case-report.html
```

통과 기준:

- `validation-log.jsonl`에 `verified-playable`이 있어야 한다.
- `review\evidence-viewer.html`이 서버 없이 바로 열려야 한다.
- 브라우저에서 영상 metadata 또는 재생이 확인되어야 한다.
- 보고서에 검증 섹션이 보여야 한다.
- 패키지에 report, viewer, DB, logs가 포함되어야 한다.

## 4. Phase 3 - Windows 경로와 대량 파일 보강

목표: 실제 사건에서 자주 터지는 경로/대량 파일 문제를 먼저 잡는다.

반드시 테스트할 것:

- 한글 경로
  - `C:\Cases\한글 사건\증거 원본\`
- 공백 포함 경로
  - `C:\Cases\Client CCTV 001\`
- 긴 경로
- 1,000개 이상 파일
- 영상/사진/기타 파일 혼합 폴더
- 같은 케이스에 반복 스캔

명령 예시:

```powershell
.\target\release\frametrace.exe init-case "C:\Cases\한글 사건" --title "한글 경로 테스트"
.\target\release\frametrace.exe scan-folder "C:\Cases\한글 사건" "E:\BLACKBOX" --no-ffprobe
.\target\release\frametrace.exe scan-folder "C:\Cases\한글 사건" "E:\BLACKBOX" --hash --max-depth 2
.\target\release\frametrace.exe make-review "C:\Cases\한글 사건"
.\target\release\frametrace.exe make-report "C:\Cases\한글 사건"
```

문제가 나오면 구현할 것:

- JSON/TSV/HTML/PowerShell 출력의 Windows path escaping 수정
- `file:///C:/...` URL이 viewer 재생에 맞게 생성되는지 확인
- SQLite에 Unicode 경로가 깨지지 않는지 확인
- 반복 스캔에서 row 중복/누락이 없는지 확인
- 1,000개 이상 evidence list에서 viewer pagination/filter 성능 확인

## 5. Phase 4 - E01과 raw image 검증

목표: Windows에서 E01 import와 raw image 후속 처리가 실제로 되는지 확인한다.

처음에는 의뢰인 자료가 아닌 테스트 E01을 사용한다.

```powershell
.\target\release\frametrace.exe init-case C:\Cases\e01-test --title "E01 Windows test"
.\target\release\frametrace.exe inspect-e01 C:\Cases\e01-test D:\Images\sample.E01
.\target\release\frametrace.exe import-e01 C:\Cases\e01-test D:\Images\sample.E01 --output C:\Cases\e01-test\evidence\images\sample.raw
```

raw workflow:

```powershell
.\target\release\frametrace.exe inspect-image C:\Cases\e01-test C:\Cases\e01-test\evidence\images\sample.raw
.\target\release\frametrace.exe carve-file C:\Cases\e01-test C:\Cases\e01-test\evidence\images\sample.raw --max-bytes 536870912 --max-candidates 128
.\target\release\frametrace.exe make-review C:\Cases\e01-test
.\target\release\frametrace.exe make-report C:\Cases\e01-test
```

문제가 나오면 구현할 것:

- `PATH` 탐지가 불안정하면 libewf/Sleuth Kit binary path 옵션 추가
- segmented E01 세트 `.E01`, `.E02` 처리 안내와 오류 메시지 개선
- 대용량 raw export 후 SHA-256 계산 안정성 확인
- `mmls` partition offset 단위가 examiner 안내와 로그에 정확히 남는지 확인
- libewf/Sleuth Kit Windows 배포 방식 문서화

## 6. Phase 5 - 실제 증거 dry run

목표: 의뢰인 데이터 투입 전에, 복제본/이미지/읽기 전용 마운트 기준으로 실무 흐름을 검증한다.

원칙:

- 원본 SD/HDD/SSD/USB/E01에는 쓰지 않는다.
- forensic image, copied folder, read-only mounted volume만 사용한다.
- 모든 derived output은 case folder 안에 둔다.
- write-protection, acquisition context를 `init-case` 또는 `register-source`에 남긴다.

1차 빠른 스캔:

```powershell
.\target\release\frametrace.exe init-case C:\Cases\case-001 --title "Client CCTV review" --operator "Examiner" --device-id "SD-001" --write-protect "hardware write blocker"
.\target\release\frametrace.exe register-source C:\Cases\case-001 E:\ --kind mounted-volume --write-protect "hardware write blocker"
.\target\release\frametrace.exe scan-folder C:\Cases\case-001 E:\ --no-ffprobe
.\target\release\frametrace.exe make-review C:\Cases\case-001
```

필요 구간만 심화:

```powershell
.\target\release\frametrace.exe scan-folder C:\Cases\case-001 E:\BLACKBOX --hash --max-depth 2
.\target\release\frametrace.exe validate-artifact C:\Cases\case-001 vid_000001
.\target\release\frametrace.exe make-report C:\Cases\case-001
.\target\release\frametrace.exe package-case C:\Cases\case-001
```

문제가 나오면 구현할 것:

- 실제 폴더 구조 기반 제조사별 event/folder parser 추가
- 안정적으로 입증 가능한 timestamp metadata 추출
- GPS/speed metadata는 출처가 명확할 때만 추가
- 깨진 파일 repair는 자동 변경이 아니라 명시적 candidate operation으로 추가
- 장시간 scan/probe/hash 작업에 progress output 추가

## 7. Phase 6 - 최종 Windows GUI shell 구현

목표: 안정화된 Rust engine을 Windows GUI로 감싼다.

이 단계는 Phase 1-5 통과 후 시작한다.

권장 기술:

- C# / WinUI 3
- Rust CLI는 evidence-processing source of truth로 유지
- GUI는 `frametrace.exe` 명령을 실행하고 JSON/JSONL/SQLite 산출물을 읽는다
- Tauri는 WinUI가 막힐 때의 fallback으로만 둔다

최소 화면:

- Case home
  - 케이스 생성/열기
  - 증거 source 목록
  - job history
  - examiner action이 필요한 warning
- Evidence source intake
  - folder/drive 등록
  - E01 import
  - write-protection/acquisition note 입력
- Large evidence browser
  - 1,000개 이상 virtualized list
  - parser/vendor/status/codec/date/hash filter
  - unvalidated/verified 상태 구분
- Viewer
  - video playback
  - image preview
  - timeline/range selection
  - proxy/thumbnail/export action
  - validation status와 SHA-256 상시 표시
- Recovery workspace
  - image inspection 결과
  - deleted entry 목록
  - inode recovery 실행
  - carving candidate 목록
- Report/package
  - report 생성
  - generated viewer/report 열기
  - package-case 실행

GUI 규칙:

- candidate 상태를 숨기지 않는다.
- vendor player `.exe`는 자동 실행하지 않는다.
- 모든 output은 engine command에서 생성하고 로그에 남긴다.
- command failure stderr를 GUI에 명확히 보여준다.
- 긴 작업은 progress/status와 cancel-safe 상태가 있어야 한다.
- 한국어 UI가 기본값이어야 한다.

## 8. Phase 7 - Windows 배포 패키지

목표: examiner workstation에 설치/복사 가능한 Windows deliverable을 만든다.

포함 대상:

- `frametrace.exe`
- GUI executable
- startup diagnostic 또는 dependency check 화면
  - 최종 사용자는 Rust가 필요 없어야 한다.
  - FFmpeg/ffprobe는 bundle 또는 discovery가 필요하다. (2026-10-20 옵션 C: 번들하지 않음 — `tools install ffmpeg` 안내 설치 또는 PATH 탐색, `docs/PLAN_TOOLS_C_2026-10-20.md`.)
  - libewf/Sleuth Kit은 bundle하거나 missing 상태를 명확히 알려야 한다.
- 예제 workflow 문서
- versioned release notes

릴리즈 확인:

```powershell
.\target\release\frametrace.exe --help
.\target\release\frametrace.exe list-parsers
.\target\release\frametrace.exe benchmark-db C:\Temp\frametrace-db-bench --rows 100000
```

패키징 원칙:

- 외부 binary는 license 검토 전까지 함부로 bundle하지 않는다.
- FFmpeg, libewf, Sleuth Kit의 정확한 binary/version을 로그에 남긴다.
- case package는 app 설치 경로와 독립적으로 열려야 한다.

## 9. Windows 완료 기준

아래가 모두 통과해야 Windows 작업 완료로 본다.

- Windows MSVC build 통과
- Windows unit test / clippy 통과
- synthetic MP4 workflow 통과
- 한글 경로 workflow 통과
- 1,000개 이상 evidence viewer 사용성 통과
- segmented E01 sample inspect/import 통과
- Sleuth Kit raw image `inspect-image` 통과
- 삭제 inode recovery 최소 1건 검증
- carving candidate offset/SHA-256/status log 확인
- `validate-artifact`가 indexed video, carved ID, recovered path, direct path 모두 처리
- report에 scan/export/carving/filesystem/validation 섹션 표시
- package에 review, evidence viewer, report, DB, logs 포함
- GUI가 있다면 evidence logic을 재구현하지 않고 engine command를 호출
- source evidence에 쓰기 작업이 없어야 함

## 10. 알려진 Windows 전용 리스크

- 외부 binary 설치/`PATH` 차이
- libewf/Sleuth Kit Windows 배포 차이
- segmented E01 경로 동작
- Unicode/긴 경로 처리
- 브라우저의 local `file:///` 영상 재생 제한
- 대용량 디렉터리 traversal 지연
- 독자 CCTV/DVR 포맷의 제조사별 parser 필요
- 법정 제출용 표현/절차는 포렌식 전문가 검토 필요

## 11. 수정 후 커밋 전 확인

Windows에서 기능을 고친 뒤 최소한 아래를 실행한다.

```powershell
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

커밋 메시지는 왜 바꿨는지, 무엇을 검증했는지 남긴다. 가능하면 이 repo의 Lore-style trailer를 유지한다.
