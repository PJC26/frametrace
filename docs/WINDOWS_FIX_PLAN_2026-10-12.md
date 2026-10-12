# FrameTrace Windows 리뷰(2026-10-12) 수정 계획

기준 커밋 4a5a609. 리뷰 문서: docs/WINDOWS_REVIEW_2026-10-12.md. ★ = 이번 계획 작성 시 코드로 재확인함.

## 0. 불변 조건 (모든 트랙 공통)

- 마크·메모·태그·IN/OUT 보존, 조용한 실패 금지, 보고서는 검증한 것만 (PLAN-v100 §2).
- 단축키·키맵·UI ID(docs/ui-rebuild-contract.md) 변경 없음.
- `src/html_report.rs`가 내보내는 evidence_viewer.html 바이트 변경 없음 → 트랙 B의 JS 수정은 워크스테이션 전용 파일에만, 머지 전 `verify-embed` 동등성 통과 필수.
- 테스트는 사용자 실제 설정을 건드리지 않음(`startIsolatedServer`, `FRAMETRACE_INSTANCE_FILE`, 임시 HOME).
- 고정 sleep으로 통과하는 테스트 금지(동기화 신호 사용).

## 1. 항목별 판정

| # | 판정 | 코드 확인 | 근거/비고 |
|---|---|---|---|
| 1 체크포인트 강제 잠금 | **고침** | ★ `checkpoint.rs:96` 실행 내내 `lock_exclusive`, 서버는 `util::read_to_string`로 별도 핸들 읽기(`records.rs:281`) → Windows ERROR_LOCK_VIOLATION | 사이드카 `.lock` |
| 2 CREATE_NO_WINDOW 부재 | **고침** | ★ `util.rs:36`, `proc.rs:162`(`creation_flags(CREATE_SUSPENDED)` 덮어씀), `system.rs:92` 등 | 공용 헬퍼 |
| 3 WM_CLOSE 무조건 파괴 | **고침** | ★ `shell.rs:412` DestroyWindow → `run_hosted` `request_shutdown` → 작업 중이면 거부(`system.rs:541`) → `watcher.join()` 영구 대기 = 보이지 않는 프로세스 | 확인창 + 취소→종료 + 상한 후 강제 종료 |
| 4 case_dir 비교 불일치 | **고침** | ★ `candidates.rs:121` `canonicalize()`(\\?\) vs `scan/mod.rs:47` `canonicalize_display` → `starts_with` 항상 거짓 | 같은 함수로 통일 |
| 5 msedge가 서버 job 안 | **고침** | ★ `proc.rs init_process_guard` 서버 자신을 KILL_ON_JOB_CLOSE job에 넣음, `mod.rs open_in_browser`가 그 안에서 spawn | BREAKAWAY_OK + CREATE_BREAKAWAY_FROM_JOB |
| 6 discovery 토큰 선전송·인스턴스 파일 미삭제 | **고침(부분)** | ★ `find_running_server`는 인스턴스 파일 포트에 토큰 먼저 전송; 폴백 스캔은 `token_for_port`가 포트 일치 때만 토큰 반환; 종료 시 파일 미삭제 | 종료 시 삭제(pid 일치) + 폴백 스캔 무토큰. 1차 경로 토큰 전송은 유지(우리 프로세스가 쓴 사용자 전용 파일의 포트에만, 토큰은 프로세스별 난수) — 잔여 위험 문서화 |
| 7 Origin 포트 미검사 | **고침** | ★ `http.rs request_is_localhost_trusted` 호스트만 비교 | Origin 있으면 Host와 authority 완전 일치 요구 |
| 8 UNC 경로 fs 선접근 | **고침** | ★ `http.rs serve_media` canonicalize 선행, `system.rs browse_json` metadata 선행 | `\\`,`//` 접두 거부(승인 루트가 UNC면 그 접두만 허용) |
| 9 SO_EXCLUSIVEADDRUSE | **고침** | `mod.rs:228,341` std `TcpListener::bind` (Windows에서 옵션 미설정) | WinSock 직접 bind(cfg(windows)) |
| 10 explorer/msedge 이름 실행 | **고침** | ★ `mod.rs:360-381` `Command::new("msedge.exe")`, `"explorer.exe"` | `%SystemRoot%\explorer.exe`, Edge는 절대 경로 목록→App Paths 레지스트리→ShellExecuteW 폴백 |
| 11 토큰 URL 노출 | **고침** | 미확인(워커가 `replaceState`·`Referrer-Policy` 존재 여부 먼저 grep) | 헤더 + 쿠키 심은 뒤 `history.replaceState` |
| 12 지연 트랜잭션 | **고침** | ★ `marks.rs:108`, `scan.rs:16,40` `conn.transaction()` | `TransactionBehavior::Immediate` 공용 헬퍼 |
| 13 HMAC 제거 무경고 | **고침(리뷰 정정)** | ★ `audit.rs:420` "mixed keyed/unkeyed" 경고 **이미 있음**. 빈틈은 (a) 전부 unkeyed로 재체인하면 경고 0, (b) keyed→unkeyed 전환이 "mixed"로만 뭉뚱그려짐 | 키 설정됨+keyed 0 → 경고; keyed→unkeyed 전환 → 키 설정 시 오류/미설정 시 줄번호 경고 |
| 14 잘림/빈 로그 탐지 | **비목표** | — | 매니페스트에 앵커(엔트리 수·마지막 해시) 기록하는 스키마 변경 필요. 별도 설계(PLAN-v100 추가 항목)로 분리 |
| 15 e01/tsk-audit 검증 누락 | **고침** | ★ `custody.rs:356` `ends_with("log.jsonl")`만 수집 | `-audit.jsonl` 포함 |
| 16 DAV 파싱 실패 조용히 제외 | **고침** | ★ `anomaly.rs:518` `let Ok(frames) = … else { continue }`, `dav.rs:123` 첫 불량 프레임에서 Err | `dav-parse-limitation` finding (기존 `dav-date-coverage-limitation` 패턴) |
| 17 키링 truncate 쓰기 | **고침** | ★ `audit_key.rs:439` `truncate(true)` 직접 쓰기 | temp+rename 원자 쓰기 |
| 18 mp4 u64 오버플로 | **고침** | ★ `mp4.rs:348-350` `region_off + payload_start`, `rel + size - payload_start` 무검사 | checked_add/sub, 실패 시 후보 건너뛰고 warning |
| 19 재개 시 로그 중복 | **보류→테스트로 판정** | ★ `carve/mod.rs:511`은 재개 1회당 `carve-resume` 이벤트 1줄 — 의도된 동작으로 보임 | 2회 재개 테스트로 아티팩트 이벤트 중복 여부 확인, 중복 시에만 수정 |
| 20 build-libewf-tools.ps1 | **고침** | 미확인(워커 재확인) | PS5.1 `2>&1`+Stop 회피, exit code 검사, DLL 수집, 해시 검증 |
| 21 make-portable tools/bin 경로 | **고침** | ★ `make-portable.ps1:20` `tools/bin`만 복사 | `tools/bin` 우선, `target/release/tools/bin` 폴백, 문서와 일치 |
| 22 PS 스크립트 BOM | **고침** | ★ make-portable.ps1에는 BOM 있음; 나머지 워커 확인 | UTF-8 BOM 통일 + CI에서 BOM 검사 |
| 23 `-ExecutionPolicy Bypass` 누락 | **고침(문서)** | 미확인 | 문서 일괄 수정 |
| 24 windows-ci.yml 하네스 미검증 | **고침** | ★ fmt/clippy/test/e2e만 있음, `ci-harnesses.sh`·install·make-portable 없음 | 하네스 step + make-portable/install 스모크 추가 |
| 25 FRAMETRACE_BIN .exe 보정 | **고침** | ★ `isolated-server.mjs:51` env 값 그대로 사용 | win32에서 `.exe` 없고 파일 없으면 보정 |
| 26 rmSync EBUSY | **고침** | ★ `verify-marks-roundtrip.mjs:91` `srv.stop()` 직후 `rmSync` | `stop()`이 exit 이벤트 대기 보장 + `rmSync({maxRetries, retryDelay})` |
| 27 SHA256SUMS CRLF / .gitattributes | **고침** | ★ `Out-File -Encoding ascii`(PS5.1 CRLF), `.gitattributes`는 `contracts/** -text`만 | `[IO.File]::WriteAllText` LF, `assets/** text eol=lf` 추가 후 verify-embed·contract-pin 무변경 확인 |
| 28 문서 오류 | **고침(문서)** | 미확인 | — |
| Low: DPI E_ACCESSDENIED | **고침** | ★ `shell.rs:222` `?`로 셸 전체 포기 | 실패 무시(로그만) |
| Low: MessageBoxA 한글 | **고침** | ★ `app.rs:48` | `MessageBoxW` + UTF-16 |
| Low: 사본 읽기 전용 속성 | **고침** | 미확인(`media.rs:1170`) | copy 후 readonly 해제 |
| Low: junction 무경고 | **고침** | `candidates.rs:95` | warning 1줄 |
| Low: HOME 우선 | **고침** | ★ `discovery.rs:14` HOME→USERPROFILE 순 | Windows는 USERPROFILE 우선(격리 하네스는 둘 다 설정하므로 영향 없음) |
| Low: proc.rs 주석 | **고침(잡일)** | — | A 트랙이 파일 소유 |
| Low: deepfake py 런처 | **비목표** | — | 선택 플러그인, Windows Python 검증 환경 없음. 후속 |

## 2. 트랙 구성 (파일 소유권 겹침 없음)

### 트랙 A — 프로세스·셸·잠금 (난이도: 어려움 = **Fable**)
항목: 1, 2, 3, 4, 5, 9, 10, Low(DPI, MessageBoxW, junction, proc.rs 주석)

소유 파일:
```
src/checkpoint.rs
src/util.rs
src/serve/proc.rs
src/serve/mod.rs
src/shell.rs
src/app.rs
src/scan/candidates.rs
src/scan/mod.rs
src/tool_policy.rs
src/tsk.rs
src/video_export.rs
src/cli/handlers/case.rs
src/serve/jobs.rs
src/serve/system.rs (tool_available 1곳만, 92행 — 트랙 B와 조정: B가 system.rs 본 소유, A는 `tool_available`의 Command 생성 1줄만 PR 설명에 명시하고 B 머지 후 리베이스)
Cargo.toml (windows features: Win32_Networking_WinSock 추가)
```

수정 방법:
- **1**: `RunCheckpoint::begin`이 `<path>.lock` 사이드카를 열어 `lock_exclusive`(런 동안 보유), 데이터 파일은 잠그지 않음. `finish()`/Drop에서 unlock 후 사이드카 삭제 시도(실패 무시). 패키지/커스터디가 `db/`를 열거하면 `.lock` 제외 확인.
  - 테스트(`checkpoint.rs`): begin 후 같은 프로세스에서 `std::fs::read_to_string(path)` 성공(Windows에서 현재 실패); 같은 경로 두 번째 `begin` 실패; finish 후 다시 begin 성공.
- **2**: `util::hide_console(&mut Command)` (cfg(windows): `creation_flags |= CREATE_NO_WINDOW`), `proc::prepare`는 `CREATE_SUSPENDED | CREATE_NO_WINDOW`로. 모든 `Command::new` 호출처 적용.
  - 테스트: `src/`를 스캔해 허용 목록 밖의 `Command::new(` 가 `hide_console`/`spawn_tracked` 없이 쓰이면 실패하는 소스 스캔 테스트(기존 `check-*` 스타일). 창 깜빡임 자체는 WINDOWS_VALIDATION 체크리스트 항목으로.
- **3**: `WM_CLOSE`에서 `/api/status` 조회 → 작업 중이면 `MessageBoxW`(예: "작업 중입니다. 취소하고 종료할까요?" 예/아니오). 예 → `/api/cancel` 후 `has_job=false`까지 상한(10s) 대기 → DestroyWindow. 아니오 → `LRESULT(0)` 유지. `run_hosted` 종료 경로: `request_shutdown`이 `running:true`면 cancel→재시도 상한(15s) → 그래도 거부면 `kill_all_tracked` + `process::exit(1)`(job이 자식 정리) — 영구 대기 금지. 셸의 다른 메시지·키 처리는 변경 없음.
  - 테스트: 단위 테스트 가능 범위 — 종료 결정 함수(`decide_close(status_json, user_choice) -> CloseAction`)를 순수 함수로 분리해 테이블 테스트. 실제 창은 Windows 검증 체크리스트.
- **4**: `excluded_case_dirs`에서 case_dir도 `canonicalize_display`로. `path_is_or_is_under_excluded_dir`는 그대로.
  - 테스트: tempdir에 `case/` 중첩 생성 → `excluded_case_dirs` 비어 있지 않음; `collect_video_candidates`가 `case/artifacts/*.mp4` 제외하고 warning 1줄 기록(Linux·Windows 모두 실행).
- **5·10**: `open_in_browser`/`open_in_explorer`: 서버 job에 `JOB_OBJECT_LIMIT_BREAKAWAY_OK` 추가, 브라우저/탐색기 spawn에 `CREATE_BREAKAWAY_FROM_JOB`. 경로: `explorer.exe` → `%SystemRoot%\explorer.exe`; Edge → `%ProgramFiles(x86)%`·`%ProgramFiles%`·`%LOCALAPPDATA%` 알려진 경로 → `HKLM\...\App Paths\msedge.exe` → `ShellExecuteW(open, url)`. 현재 디렉터리·PATH 검색 없음.
  - 테스트: 경로 해석 함수(`resolve_edge_path(env, lookup)`) 순수 함수 테스트; "상대 이름 반환 안 함" 어서션.
- **9**: `serve/mod.rs`에 `bind_loopback(port) -> io::Result<TcpListener>`; cfg(windows)는 WinSock `socket`→`setsockopt(SO_EXCLUSIVEADDRUSE)`→`bind`→`listen`→`from_raw_socket`, 그 외 std. `first_free_port`·명시 포트 둘 다 사용.
  - 테스트(cfg(windows)): bind 후 SO_REUSEADDR 소켓으로 같은 주소 bind 시도 → 실패.
- Low: `SetProcessDpiAwareness` 실패는 `eprintln`만; `MessageBoxW`; junction은 `file_type.is_symlink()`(Windows junction은 symlink_dir로 보고) 시 warning; proc.rs 76·299 주석 정정.

### 트랙 B — HTTP 보안·디스커버리 (난이도: 보통 = **Opus**)
항목: 6, 7, 8, 11, Low(HOME 우선)

소유 파일:
```
src/serve/http.rs
src/serve/discovery.rs
src/serve/system.rs (tool_available 1줄 제외)
src/serve/media.rs (경로 검사 부분; Low 읽기 전용 속성은 트랙 C가 아니라 B가 함께 처리 — 파일 소유권 단일화)
assets/<워크스테이션 전용 JS 1개> (replaceState; evidence_viewer에 포함되는 파일 금지)
tests/workstation_e2e.rs (CSRF·UNC 라우트 테스트 추가)
```

수정 방법:
- **7**: `request_is_localhost_trusted`: Origin이 있으면 Host도 있어야 하고 `origin_authority(host, port)` == `host_authority(host, port)`(포트 기본값 80 정규화, 호스트 소문자). Origin 없음(셸 raw 소켓·curl)은 기존대로.
  - 테스트(http.rs): `Origin: http://127.0.0.1:9999` + `Host: 127.0.0.1:8477` → false; 같은 포트 → true; Origin 있고 Host 없음 → false; `[::1]:8477` 양쪽 → true.
- **8**: `http.rs`에 `reject_remote_path(raw: &str, approved_roots: &[PathBuf]) -> Result<(), &'static str>`: `\\`·`//`·`\\?\`·`\\.\` 접두 거부, 단 승인 루트가 UNC면 그 루트 접두(대소문자 무시)만 허용. `serve_media`, `browse_json`, `api_open_case`, `api_case_info`, `resolve_open_folder`, 그 외 `PathBuf::from(사용자 입력)` 전부 — canonicalize/metadata **이전**에 호출.
  - 테스트: 헬퍼 단위 테스트(UNC 루트 허용 케이스 포함); e2e: `GET /api/media?path=\\\\evil\\share\\a.mp4` → 400, `GET /api/browse?path=//evil/share` → 400.
- **6**: `discovery::remove_instance_file_if_owned()`(파일의 pid == 현재 pid일 때만 삭제); `find_running_server` 폴백 스캔은 토큰 없이 프로빙하고 200+전체 형태만 인정. 1차 경로 유지 사유를 주석으로.
  - `serve/mod.rs:209` `serve_on` 뒤 호출 1줄은 **통합 단계에서 추가**(A 소유 파일).
  - 테스트: 임시 인스턴스 파일 pid 불일치 → 미삭제, 일치 → 삭제.
- **11**: 모든 응답에 `Referrer-Policy: no-referrer`; `GET /?token=` 성공 시 쿠키 심은 뒤 UI JS가 `history.replaceState`로 `token` 쿼리 제거(이미 있으면 유지). 뷰어 JS 미접촉.
  - 테스트: http 단위 테스트에서 헤더 존재; e2e에서 로드 후 `location.search`에 `token` 없음 + 이후 POST 성공(쿠키 인증 유지).
- Low: `instance_file_path` Windows는 USERPROFILE 우선; `media.rs` 사본 생성 후 `set_readonly(false)`.

### 트랙 C — 증거 무결성 (난이도: 보통 = **Opus**)
항목: 12, 13, 15, 16, 17, 18, 19(판정)

소유 파일:
```
src/case_db/mod.rs (쓰기 트랜잭션 헬퍼)
src/case_db/marks.rs
src/case_db/scan.rs
src/case_db/metrics.rs
src/audit.rs
src/custody.rs
src/dav.rs
src/anomaly.rs
src/audit_key.rs
src/carve/mp4.rs
src/carve/mod.rs
tests/<carve 재개 테스트 파일>
```

수정 방법:
- **12**: `case_db::begin_write(conn) -> Transaction`이 `transaction_with_behavior(Immediate)`; 세 호출처 교체. busy_timeout 설정은 `open_case_db`에 있는지 확인(없으면 5s 추가).
  - 테스트(결정적): 헬퍼로 tx 연 상태에서 두 번째 커넥션(busy_timeout 0)이 `BEGIN IMMEDIATE` → 즉시 SQLITE_BUSY (Deferred였다면 성공했을 것). 마크 왕복 기존 테스트 유지.
- **13**: `verify_chained_jsonl_text`: (a) `!keys.is_empty() && entries>0 && keyed_entries==0` → 경고 "키가 설정돼 있으나 keyed 엔트리 0 — 전면 재서명 가능성"; (b) 첫 keyed 줄 이후 unkeyed 줄 등장 → `keys` 비어 있으면 줄번호 포함 경고 + StructuralOnly, 키 있으면 **오류**. unkeyed→keyed(키 도입)는 기존 "mixed" 경고 유지.
  - 테스트: 기존 helper로 keyed 로그 생성 → 전부 HMAC 제거하고 재체인 → 키 설정 검증 시 경고 포함; 중간부터 제거 → 키 설정 시 Err, 미설정 시 경고에 줄번호.
- **15**: `collect_log_files` 매칭 `ends_with("log.jsonl") || ends_with("-audit.jsonl")`.
  - 테스트: tempdir에 `e01-audit.jsonl` 두고 수집 결과 포함.
- **16**: `dav_frame_gap_findings`에서 `Err(err)` → `Finding{kind:"dav-parse-limitation", note: err}` push(제외 아님). `dav.rs`는 변경 없음(Err 메시지 그대로 사용).
  - 테스트: 깨진 DHAV 픽스처 → finding 1건, 다른 DAV의 gap finding은 그대로.
- **17**: `write_keyring`: `<path>.tmp` 쓰기+sync → `rename`(Windows std rename은 덮어씀) → 디렉터리 sync(unix).
  - 테스트: 기존 키링 있을 때 재쓰기 후 내용 일치, `.tmp` 잔존 없음.
- **18**: `emit_mp4_gapfills`·리뷰 지목 행들 `checked_add/checked_sub`; None이면 해당 후보 skip + warning "offset overflow".
  - 테스트: `region_off=u64::MAX-1`, `need=100` 입력 → panic 없이 join 0건, warning 1건.
- **19**: 재개 2회 테스트 작성: 아티팩트 이벤트 줄 id 집합이 중복 없음·`carve-resume` 줄은 재개 횟수만큼. 통과하면 "비목표(의도된 동작)"로 리뷰 문서에 기재, 실패하면 `write_carve_outputs`에서 재개된 아티팩트 재기록 방지.

### 트랙 D — 스크립트·CI (난이도: 보통 = **Opus**)
항목: 20, 21, 22, 24(yml), 25, 26, 27

소유 파일:
```
scripts/build-libewf-tools.ps1
scripts/make-portable.ps1
scripts/install.ps1
scripts/sign-release.ps1
scripts/lib/isolated-server.mjs
scripts/verify-marks-roundtrip.mjs
scripts/check-ps-bom.mjs (신규)
.gitattributes
.github/workflows/windows-ci.yml
```

수정 방법:
- **20**: 워커가 먼저 PS5.1로 실행해 각 주장 재현. `2>&1`은 `$ErrorActionPreference='Continue'` 블록 또는 `cmd /c ... 2>&1`로, `$LASTEXITCODE` 검사 후 `exit`, 산출 DLL(`libewf.dll`, `zlib1.dll`, `tsk` 의존) 목록 복사, 소스 아카이브 SHA256 고정값 비교.
- **21**: `make-portable.ps1`: `tools/bin` → 없으면 `target/release/tools/bin`; 둘 다 없으면 경고 출력.
- **22**: 모든 `scripts/*.ps1` UTF-8 BOM; `check-ps-bom.mjs`로 검사, CI step 추가.
- **27**: SHA256SUMS `[IO.File]::WriteAllText($p, ($lines -join "`n") + "`n")`; `.gitattributes`에 `assets/** text eol=lf` 추가 → **로컬·CI에서 `verify-contract-pin`·`verify-embed` 무변경 확인 후 머지**.
- **25**: win32에서 `FRAMETRACE_BIN`이 `.exe` 없고 미존재면 `.exe` 붙여 재확인.
- **26**: `stop()`이 `child.on('exit')`를 await하도록(이미 그렇다면 확인만), `rmSync(work, {recursive:true, force:true, maxRetries:10, retryDelay:200})`.
- **24**: windows-ci.yml에 (a) `cargo build --locked` 후 Playwright 일괄 설치(CLAUDE.md 명령 그대로) → `bash scripts/ci-harnesses.sh`(Git Bash, `FRAMETRACE_BIN` 설정), (b) `make-portable.ps1` 실행 + zip 존재·SHA256SUMS LF 검사, (c) `install.ps1` 스모크(APPDATA·USERPROFILE을 러너 temp로 격리, `-Uninstall`까지), (d) BOM 검사. libewf/TSK 빌드는 시간 때문에 `workflow_dispatch` 별도 job.
- 테스트: 스크립트 트랙은 CI 자체가 테스트. 로컬은 PS5.1(`powershell`)과 PS7(`pwsh`) 둘 다 1회 실행.

### 트랙 E — 문서 (난이도: 잡일 = **Haiku**)
항목: 23, 28, 24(HANDOFF 표현), 25(CLAUDE.md 게이트 문구), 6·14·19 결정 기록

소유 파일:
```
docs/WINDOWS_USAGE.md
docs/WINDOWS_VALIDATION.md
docs/HANDOFF-2026-10-11.md
docs/WINDOWS_REVIEW_2026-10-12.md (판정·결과 열 추가)
docs/PLAN-v100.md (§6/§7: 14 후속, 19 결론)
CLAUDE.md (게이트의 FRAMETRACE_BIN 예시에 .exe)
README.md
```

수정 방법: `powershell -ExecutionPolicy Bypass -File ...` 통일; register-source 인자 실제 CLI `--help` 출력과 대조; winget 표기 정정; HANDOFF의 "CI가 검증함" 표현을 D 트랙 결과에 맞게; WINDOWS_VALIDATION에 수동 체크 항목 추가(콘솔 깜빡임 없음, 작업 중 창 닫기 확인창, 분석 중 그리드 표시, Edge 생존). E는 D 머지 후 시작(최종 상태 반영).

## 3. 머지 순서와 게이트

```
C (무결성, 순수 Rust)  →  B (HTTP)  →  A (프로세스·셸)  →  D (스크립트·CI)  →  E (문서)
```

사유: C·B는 플랫폼 독립 테스트로 빨리 닫힘. A가 가장 크고 Windows unsafe 코드라 먼저 머지된 C·B 위에서 전체 하네스를 돌림. D의 CI 하네스 step은 A의 바이너리 동작(잠금·종료)을 전제로 하므로 A 뒤. E는 최종 상태 기록.

각 트랙 머지 전 (트랙 워커 책임):
```
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node scripts/check-js.mjs && node scripts/check-css.mjs
node scripts/verify-contract-pin.mjs            # B·D 필수 (뷰어 바이트)
verify-embed 동등성                                # B·D 필수
```

A·D 머지 후 (통합자 책임, Windows 실기):
```
cargo build --locked && FRAMETRACE_BIN=<repo>/target/debug/frametrace-app.exe bash scripts/ci-harnesses.sh   # 22종
FRAMETRACE_IT=1 cargo test --locked --test workstation_e2e -- --ignored
수동: frametrace-app.exe 실행 → 스캔 중 그리드 표시(1) / 콘솔 창 없음(2) / 작업 중 창 닫기 확인창(3) / Edge 열어둔 채 종료 후 Edge 생존(5)
```

통합 단계 추가 작업: `serve/mod.rs` `serve_on` 뒤 `discovery::remove_instance_file_if_owned()` 1줄(B↔A 교차 의존), `system.rs tool_available` 1줄 리베이스.

전체 머지 후: **적대적 리뷰(Fable)** 1회 — 범위: (a) 잠금 변경이 체크포인트 재개·재진입 방지를 깨지 않았는지, (b) CSRF 규칙이 셸 raw 소켓·Playwright·curl 경로를 막지 않았는지, (c) 13번 규칙이 정상 키 도입 로그를 오류로 만들지 않는지, (d) .gitattributes 변경 후 Windows 체크아웃의 임베드 바이트 동일성. 차단 문제 → 해당 트랙 워커가 수정 → 게이트 재실행.

## 4. 리뷰 주장 중 이번에 확인 못 한 것 (워커가 착수 전 재확인)

- 20(build-libewf 스크립트 4가지 주장), 21의 "빌드 산출 위치가 target/release/tools/bin"이라는 전제, 23·28 문서 오류 위치, 11의 현재 UI 토큰 처리, Low 읽기 전용 속성(`media.rs:1170`).
- 13·19는 리뷰 서술과 코드가 다르므로 위 표의 수정된 판정을 따른다.
