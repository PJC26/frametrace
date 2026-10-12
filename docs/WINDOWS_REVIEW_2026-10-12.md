# Windows 적대적 리뷰 — 2026-10-12 (기준 커밋 4a5a609)

정적 리뷰 5트랙(프로세스·셸 / 경로·파일 / HTTP 보안 / 스크립트·CI / 파서·감사). ★ = 코드 재확인.

## A. Windows 차단급
1. ★ `src/checkpoint.rs:96` 스캔 체크포인트를 실행 내내 `lock_exclusive` — Windows 강제 잠금이라 서버(`src/serve/records.rs:281` read_live_checkpoint, `src/serve/recent.rs:192`)의 읽기가 ERROR_LOCK_VIOLATION → 분석 중 그리드 빈 화면. (`src/audit.rs:188-197`이 같은 문제를 이미 피함.) 수정: 실행 배타성은 사이드카 `.lock` 파일로, 데이터 파일은 잠그지 않음.
2. ★ `src/app.rs:3` windows_subsystem=windows인데 모든 외부 도구 spawn에 CREATE_NO_WINDOW 없음 → 콘솔 창 깜빡임. 사용처: `src/util.rs:36` run_with_timeout, `src/serve/system.rs:92` tool_available, `src/tool_policy.rs:62`, `src/tsk.rs:369,600`, `src/video_export.rs:181`, `src/cli/handlers/case.rs:1047`, `src/serve/jobs.rs:759` 단계 프로세스, `src/serve/proc.rs:162`(creation_flags는 OR가 아니라 덮어씀). 수정: 공용 헬퍼로 CREATE_NO_WINDOW 부여.
3. ★ `src/shell.rs:412` WM_CLOSE가 무조건 창 파괴, `/api/shutdown`은 작업 중 거부(`src/serve/system.rs:541`) → 보이지 않는 프로세스 잔존. 수정: 작업 중이면 확인(취소 후 종료 / 계속) 또는 취소+종료.
4. ★ `src/scan/candidates.rs:121-130` case_dir는 `canonicalize()`(\\?\ 유지), source는 `canonicalize_display`(`src/scan/mod.rs:47`) → 비교 항상 거짓, 케이스 산출물이 증거로 재색인. 수정: 양쪽 같은 방식.
5. `src/serve/mod.rs:165,360` 서버 KILL_ON_JOB_CLOSE job 안에서 msedge 실행 → 서버 종료 시 Edge 전체 종료 가능. 수정: BREAKAWAY_OK + CREATE_BREAKAWAY_FROM_JOB 또는 ShellExecuteW.

## B. 보안
6. `src/serve/discovery.rs:72-144` 신원 확인 전에 토큰 전송, 구조적 폴백, 인스턴스 파일 종료 시 미삭제(`mod.rs:209`).
7. `src/serve/http.rs:469-495` Origin 포트 미검사 + 쿠키만으로 인증 → 다른 localhost 포트발 CSRF. 수정: 비-GET은 Origin 포트=바인딩 포트 또는 Sec-Fetch-Site same-origin 요구.
8. `src/serve/http.rs:1009`, `src/serve/system.rs:125,132,145,736` 등 사용자 경로를 범위 검사 전에 fs 호출 → UNC로 SMB/NTLM 유출. 수정: `\\`,`//` 시작 경로(UNC/디바이스) 거부(승인 루트가 UNC인 경우 제외).
9. `src/serve/mod.rs:228,341` SO_EXCLUSIVEADDRUSE 없음.
10. `src/serve/mod.rs:360,370,381` explorer.exe/msedge.exe 이름 실행 → 현재 폴더 하이재킹. 수정: %SystemRoot% 절대 경로 등.
11. 토큰 URL 노출: Referrer-Policy: no-referrer, 쿠키 설정 후 history.replaceState로 ?token= 제거.

## C. 증거 무결성
12. `src/case_db/marks.rs:108`, `scan.rs:16,40`, `metrics.rs:16` 지연 트랜잭션 → busy_timeout 미적용, 마크 저장 즉시 locked. 수정: TransactionBehavior::Immediate.
13. `src/audit.rs:356,412-429` HMAC 필드 제거 시 경고 없이 structural-only 통과. 수정: 앞에 keyed 줄이 있었는데 이후 unkeyed면 오류, 키가 설정돼 있으면 경고.
14. `src/audit.rs:265`, `src/package.rs:672` 잘림/빈 로그 탐지 불가 — (설계 변경 큼, 비목표 후보)
15. `src/custody.rs:367` e01-audit.jsonl / tsk-audit.jsonl 검증 누락.
16. `src/dav.rs:123`, `src/anomaly.rs:518` DAV 프레임 하나 깨지면 조용히 제외. 수정: 파싱 한계 finding/경고.
17. `src/audit_key.rs:439` 키링 truncate 쓰기 → 원자적 쓰기로.
18. `src/carve/mp4.rs:293,304,318,321,348,370,491` u64 오버플로 → checked_add.
19. `src/carve/mod.rs:511` 재개 시 로그 중복.

## D. 스크립트·CI·문서
20. `scripts/build-libewf-tools.ps1` PS5.1 `2>&1`+Stop 중단, BUILD_EXIT 무시, DLL 누락, 해시 미검증.
21. `scripts/make-portable.ps1:20` tools/bin 경로 불일치(빌드는 target/release/tools/bin).
22. `scripts/install.ps1`, `scripts/sign-release.ps1` BOM 없는 UTF-8 → CP949 PS5.1 한글 깨짐.
23. 문서 `powershell -File` 예시에 `-ExecutionPolicy Bypass` 없음.
24. windows-ci.yml이 하네스·libewf/TSK·설치 스크립트·셸 미검증. HANDOFF 표현 정정.
25. `scripts/lib/isolated-server.mjs:51` FRAMETRACE_BIN에 .exe 자동 보정 없음 / CLAUDE.md 게이트 문구.
26. `scripts/verify-marks-roundtrip.mjs:91` rmSync EBUSY.
27. `scripts/make-portable.ps1:44` SHA256SUMS CRLF; `.gitattributes`에 `assets/** text eol=lf` 없음.
28. 문서 오류(WINDOWS_VALIDATION register-source 인자, WINDOWS_USAGE winget 표기).

## Low
`src/shell.rs:222` SetProcessDpiAwareness E_ACCESSDENIED 시 셸 포기 / `src/app.rs:40` MessageBoxA 한글 깨짐 / `src/deepfake.rs:99` py 런처 미탐색 / `src/serve/media.rs:1170` 사본에 읽기 전용 속성 복사 / `src/scan/candidates.rs:95` junction 무경고 스킵 / `src/serve/discovery.rs:14` HOME 우선 / `src/serve/proc.rs:76,299` 잘못된 주석.
