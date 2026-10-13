# 전체 적대적 리뷰 — 2026-10-13 (기준 커밋 8b3084b)

5개 영역 병렬 리뷰(Fable): 서버·보안 / 프로세스·OS / 증거 무결성 / 프런트엔드 / 테스트·CI·문서. 2026-10-12 리뷰에서 고친 항목은 재보고 제외. ★ = 통합자가 코드로 재확인.

## 차단·높음

| ID | 영역 | 문제 | 위치 |
|---|---|---|---|
| U1 ★ | UI | 저장할 때마다 **전체 마크**를 보냄 → 약 3만 건 넘으면 본문 상한 4 MiB 초과(413), 이후 모든 편집 저장 불가·무한 재시도 | `assets/viewer/90-sync.js:24-48,228-240`, `src/serve/http.rs:7` |
| I1 ★ | 무결성 | 패키지에 `review/data-bundle.js` 누락 → 제출 패키지의 독립 뷰어가 데이터 없이 열리고, 검토 데이터가 manifest 해시에도 없음 | `src/package.rs:214-223` |
| I2 | 무결성 | `merge-cases`가 원본 케이스의 마크·태그·메모·구간·이력을 전부 버림(재번호 후 import도 불가) | `src/case_merge.rs:48-240` |
| I3 | 무결성 | 법원 보고서에 IN-OUT 구간·태그가 표시되지 않는데 서명 배너는 "구간·태그 모두 일치"라고 씀 | `src/cli/handlers/case.rs:565-587,637`, `src/report.rs:437,513,683` |
| U2 / I7 | UI·무결성 | 같은 케이스를 두 창에서 열면 오래된 창의 전체 저장이 다른 창의 마크를 조용히 되돌림, 공유 드래프트도 지움 | `90-sync.js:24-46,220-226`, `case_db/marks.rs:151-158` |
| U3 | UI | 대량 마크/태그가 O(n²) + 매번 드래프트 전체 직렬화 → 1만~10만 건에서 멈춤, 쿼터 초과 토스트 폭주 | `30-records.js:647-659`, `80-review.js:293-309`, `65-actions.js:95-107` |
| P1 | OS | Linux/macOS에서 케이스 폴더 기본값이 `/tmp`(USERPROFILE만 확인) → 재부팅·자동 정리로 케이스 소실 | `src/serve/jobs.rs:128-137`, `system.rs:913` |
| T1 | 테스트 | Windows Job object(하위 프로세스 정리) 테스트가 어느 OS에도 없음, 취소·kill 경로 테스트 대부분 unix 전용 | `src/serve/proc.rs:338`, `tests_jobs.rs`, `tests_review3.rs` |
| T2 | 테스트 | `FRAMETRACE_IT=1` 통합 테스트가 도구 없으면 skip하고 통과 → Windows CI에서 E01 테스트는 한 번도 안 돌았음 | `tests/integration_tools.rs:104-110,265-271,338-344,417-428` |

## 중간

| ID | 문제 | 위치 |
|---|---|---|
| S1 | export-selected·capture-frame이 busy 밖에서 실행 → 종료·최종화와 경합, 찢어진 내보내기 묶음 | `src/serve/media.rs:869-1089,9-97`, `system.rs:545` |
| S2 | `C:\x\COM1` 같은 예약 장치명이 경로 검사를 통과 → browse가 장치 read에서 영구 블록, 64개면 서버 503 | `http.rs:608-634`, `system.rs:132-139,272-278` |
| I4 | `--redact-paths`가 HTML만 가림; 패키지 DB·로그·custody는 절대 경로 그대로, 문자열 중간 경로 미처리 | `case.rs:335-343,642-660`, `redact.rs:81,109-118`, `custody.rs:441-449` |
| I5 | 보고서 이상/검증 섹션이 과거 실행 전부 누적(재생성마다 중복, 오래된 stale 결과가 현재처럼 표시) | `report.rs:386-417`, `anomaly.rs:80-99` |
| I6 | 보고서·뷰어 시각이 열람자 브라우저 시간대로, 표시 없음(custody는 UTC) | `report.rs:258-261`, `html_report.rs:759`, `30-records.js:188`, `90-sync.js:441` |
| I8 | merge-cases가 4개 저장소에 비원자적으로 씀 | `case_merge.rs:213-229` |
| U4 | 작업 완료 리로드가 선택·필터·Undo를 확인 없이 버림 | `00-host.js:145-166`, `32-jobs.js:345-349` |
| U5 | 서버 대기열(deferred) 마크는 클라이언트 드래프트를 즉시 지움, 드레인 실패 시 재시도 수단 없음·"저장됨" 오해 토스트 | `90-sync.js:272-282,326-330`, `jobs.rs:923-996` |
| U6 | 한글 IME 상태에서 뷰어 단축키(j/k/1/2/3/0/i/o/x/n)가 조용히 무시(`event.key`만 비교) | `80-review.js:506-541`, `70-player.js:393` |
| U7 | 마크 필터에서 j/k마다 10만 건 필터+정렬 3~4회 | `40-grid.js:82-86,603-612`, `80-review.js:1-23,326-343` |
| P2 | 메인 파이프라인 busy가 수동 설정 → 워커 패닉 시 영구 busy | `jobs.rs:60-106,139-148` |
| P3 | 창 닫기 상태 조회 1초 타임아웃·shutdown 무응답을 "작업 없음/서버 죽음"으로 간주 → 질문 없이 강제 종료 | `shell.rs:349-366,745-752` |
| P4 | 셸 서버 기동 실패 원인(토큰 형식, bind)이 버려지고 "did not bind in time"만 표시 | `shell.rs:114-130` |
| P5 | WebView2 생성 중 서버가 죽으면 죽은 URL로 브라우저 폴백, 오류 없이 종료 | `shell.rs:151-160,193-198` |
| P6 | Job 생성 실패가 eprintln만(창 앱에선 안 보임) → 취소 후 손자 프로세스 잔존 | `proc.rs:181-214` |
| P7 | 도구 존재 확인이 타임아웃 없이 HTTP 핸들러에서 실행, SetErrorMode 없음 → DLL 누락 시 모달 대화상자로 블록 | `system.rs:81-105`, `tool_policy.rs:64` |
| T3 | `docs/security-review.md`가 "기본 토큰 없음"이라고 서술(실제는 기본 랜덤 토큰) | `docs/security-review.md:37-39` |
| T4 | perf_budgets가 어떤 CI에서도 실행 안 됨, 참조 문서 경로 틀림 | `tests/perf_budgets.rs` |
| T5 | `qa reproducibility`가 같은 케이스끼리 비교 — 재현성 증명 아님 | `src/qa.rs:135-142`, `tests/cli_smoke.rs:271-300` |
| T6 | cli_smoke의 qa performance 50k rows/min 벽시계 기준 → 불안정 | `src/qa.rs:334-337` |
| T7 | verify-live-index 등 브라우저 미해결 시 skip 후 통과 | `scripts/verify-live-index.mjs:102-104` |
| T8 | 셸 VM 하네스(verify-shell-jobs, verify-progress)는 가짜 DOM·모의 FTViewer — 독립 증거로 세면 안 됨 | `scripts/lib/shell-vm.mjs` |

## 낮음 (요약)
- S3 본문 전체를 읽은 뒤 인증 검사(16 MiB) / S4 클립 내보내기 중 /api/start 허용 / S5 recent-cases/forget UNC 검사 누락 / S6 case-info가 외부 체크포인트의 source_path를 stat(UNC) / S7 shutdown 플래그를 작업 시작 쪽이 안 봄 / S8 `//mnt/x`를 UNC로 오인
- I9 보고서가 감사 로그를 체인 검증 없이 "변조 방지"로 표시 / I10 오래된 signoff.pending.json 우선 / I11 가벼운 재스캔이 해시 실패 기록을 지움 / I12 보고서 HTML 일부 미이스케이프(offset, start_seconds) / I13 마이그레이션 백업 이름 오표기 / I14 보고서·manifest 비원자 쓰기
- U8 쿠키 없는 401을 "서버 끊김"으로 오진 / U9 포트 폴백 시 localStorage 원점 변경 / U10 요약 리포트 case_id 미이스케이프, playbackRate 리터럴 삽입 / U11 저장 시 미귀속 마크 전부를 현재 검토자로 재귀속 / U12 뷰어 모달 위에서 Alt+J/P/R 통과 / U13 Esc가 한 번에 전부 닫음 / U14 가상 그리드 ARIA 체인 / U15 레거시 UI 미검증
- P8 unix pgid 재사용 창 / P9 unix 종료 시 인스턴스 파일 잔존 / P10 shell RefCell 재진입 가능성 / P11 핸들러 패닉 시 in_flight 누수 / P12 marks-imported.json 비원자 쓰기 / P13 CoUninitialize 없음
- T9 verify-review-batch 실서버 모드 CI 미실행 / T10 퍼징이 dav·dfxml·tsk·e01·mp4 파서 미포함, nightly 미고정 / T11 cargo audit 경고 무시·액션 버전 미고정 / T12 HANDOFF 숫자 불일치(테스트 수, "5만 건"은 실제 1만 건 벤치) / T13 CI 밖의 죽은 하네스(verify-dual 하드코딩 경로, 항상 exit 0) / T14 병렬 테스트의 전역 env 변경 / T15 PIN.json 원본 재검증 불가
