# FrameTrace UI 재구축 — Phase 0 계약 (ui-rebuild-contract)

기준 커밋: `465e7db` (main). 이 문서는 5개 병렬 트랙(T1~T5)이 **서로 다른 worktree에서 구현하고 충돌 없이 머지**하기 위한 계약이다. 산문보다 식별자가 우선한다. 본문의 모든 식별자(파일명·id·class·토큰·이벤트명)는 **그대로** 써야 하며, 바꾸려면 이 문서를 먼저 고친다(변경은 통합 담당 1명만).

소유자 결정(구속력): 단일 통합 화면(단계 1~4 위저드 폐지) · iframe 제거(같은 문서에서 뷰어 호스팅, standalone 뷰어는 동일 조각+동일 번들로 유지) · 인라인 비차단 분석 진행(상단 띠 칩 + 우측 시트) · 결과물은 우측 시트(체크리스트 + 1차 버튼 1개 "보고서 생성·패키징" + 접힌 고급) · 빈 상태는 블록 2개(증거 넣기 · 이전 프로젝트, "뷰어 보기" 블록 삭제) · "종이 서류 + 영상 스테이지" 디자인 · 마크 의미(판독완료=회색 체크, 중요=호박색 깃발, 검증대기=보라 물음표, 빨강은 실패/삭제후보/파괴적 동작만) · 보조 모니터는 영상+판독만 분리, 본 창은 그리드 유지, 저장은 한 곳, `BroadcastChannel('ft-review')` · 버튼/AA/단축키/테스트 기준.

차례: §1 파일 배치·소유권 · §2 ID 맵 · §3 `window.FTViewer` · §4 셸 스토어 · §5 토큰·타입·버튼·아이콘 · §6 레이아웃 · §7 키보드 · §8 하네스 · §9 머지 순서·통합·롤백·스크린샷 · §10 미결 질문(3개)

---

## 1. 최종 파일 배치와 소유권

규칙: 파일 하나에 소유 트랙 하나. 다른 트랙의 파일은 읽기만 한다. 공유 파일은 "앵커 라인"(이 커밋이 넣음) 안에서만 **추가**한다. 소유자가 아닌 트랙이 어떤 파일에 손대야 하면 Phase 2(통합)로 넘긴다.

### 1.1 트랙 범위

| 트랙 | 범위 |
|---|---|
| **T1** | 토큰, 공용 컨트롤(버튼 4종·입력·kbd), 아이콘 스프라이트, CSS 게이트 |
| **T2** | 호스트 레이아웃(상단 띠·3열 호스트·시트 프레임·팝오버·대화상자·토스트), 빈 상태 2블록, 프로젝트 전환 팝오버, 보조 모니터(팝아웃), 셸 스토어, 셸 단축키, a11y/스크린샷 하네스 |
| **T3** | 분석 진행 칩 + 작업 시트(파이프라인·복구·카빙·클립 잡 합류), 상태 폴링 → 스토어, 로그 뷰 |
| **T4** | 결과물 시트(체크리스트·1차 버튼·고급), 원본 재검증·마크 가져오기·고급 도구, 보고서 범례 확인 |
| **T5** | 뷰어 호스팅: 조각 분리(`viewer.body.html`), `build.rs` include, `FTViewer` API, iframe 제거, CSS 스코핑, standalone 동등성, 레거시 롤백 스위치 |

### 1.2 최종 파일 배치 (재구축 후)

```
assets/
  tokens.css                   T1  rewrite   (§5 토큰 전체)
  kit.css                      T1  NEW       공용 컨트롤: 버튼 4종·입력·select·kbd·.ic 아이콘 — 양쪽 번들 2번째 (Phase 0가 빈 파일+번들 슬롯을 넣음)
  icons.svg                    T1  NEW       인라인 SVG 스프라이트 (<symbol id="i-*">) — Phase 0가 빈 스프라이트+마커 치환을 넣음
  examiner_app.html            T2  rewrite   셸 마크업. 마커 4개: @bundle:app.css · @bundle:app.js · @bundle:icons.svg · @include:viewer.body
  evidence_viewer.html         T5  rewrite   standalone 템플릿. 마커: __CSS__ · __DATA__ · __JS__ · @include:viewer.body · @bundle:icons.svg
  evidence_viewer.css          T5  rewrite   .ft-viewer 하위로 전부 스코프, body/element 셀렉터 제거, 마크 칩 모양
  app/
    00-layout.css              T2  NEW (git mv app.css → 00-layout.css 후 버튼 규칙 삭제)
    01-store.js                T2  NEW       FTStore (§4)
    10-core.js                 T2  keep
    15-theme.js                T2  rewrite   테마/언어 → FTViewer.setTheme/setLocale
    20-net.js                  T2  edit      toast() 단일 호스트 위임(§6.5), askDialog 유지
    21-project.js              T2  NEW       프로젝트 팝오버·전환(/api/open-case → FTViewer.destroy/init)
    22-viewer-host.js          T5  NEW       호스트 마운트: FTViewer.init/destroy, 잡 완료 후 reload, 저장상태 브리지
    23-popout.js               T2  NEW       보조 모니터: FTViewer.popout(), ft-review 채널, 본 창 영상 접기
    30-form.js                 T2  keep (id 개명만)
    30-jobs.css                T3  NEW
    30-jobs.js                 T3  NEW (git mv 65-jobs.js → 30-jobs.js 후 rewrite; 파이프라인 중단·새 분석·로그 도구 포함)
    31-progress.js             T3  NEW       상단 띠 칩(#jobPanel) 렌더
    35-preflight.js            T2  keep
    40-finalize.css            T4  NEW
    40-finalize.js             T4  NEW (git mv 70-finalize.js → 40-finalize.js 후 rewrite; 75-advanced.js 내용 흡수)
    40-status.js               T3  rewrite   /api/status 폴링 → FTStore.set; showView/setStep/crumbs 삭제
    50-start.js                T2  rewrite   분석 시작·케이스 열기 (btnCancel/btnNew 핸들러는 30-jobs.js로)
    55-home.js                 T2  rewrite   빈 상태: 이전 프로젝트 목록 (뷰어 보기 블록 삭제, [이어서 검토] 1차)
    80-paths.js                T2  keep      (looksImage 정규식·kindstore 라디오 유지 — src/serve/tests.rs:895 가드)
    90-tools.js                T2  edit      도구 팝오버·? 도움 팝오버·flashLabel/legacyCopy (로그 도구는 T3로)
    95-quit.js                 T2  edit
    97-shell.js                T2  rewrite   팝오버·Esc·Alt+P/J/R
    99-init.js                 T2  edit
    (삭제) app.css 60-review.js 65-jobs.js 70-finalize.js 75-advanced.js
  viewer/
    viewer.body.html           T5  NEW       조각: <div id="ftViewer" class="ft-viewer" data-ft-viewer="2">…</div> (현 evidence_viewer.html body 23~322행 전부)
    00-host.js                 T5  NEW       window.FTViewer (§3)
    10-boot.js … 95-init.js    T5  edit      IIFE → async function ftViewerBoot(opts); body.* 상태 클래스 → root; AbortSignal 리스너; 이벤트 dispatch
    99-loader.js               T5  edit      standalone 자동 init; 오버레이를 root 내부에 absolute; live chip은 standalone 전용(호스트는 `live-state` 이벤트)
    controls.css               T1  rewrite   kit.css 클래스로 통일, 중복 버튼 규칙 삭제 (embed 규칙 추가 금지 — T5)
    20-i18n.js, 21-i18n-layout.js  T5 edit   글리프(▾ ◐ ⧉ ⤓ ✓ ✕ ☰ ⚠) 제거, 신규 키 추가
  legacy/                      T5  NEW       465e7db 스냅샷: examiner_app.html, tokens.css, app/*.css, app/*.js (롤백 §9.3)
build.rs                       T5  edit      viewer.body include(양 템플릿), 뷰어 템플릿 OUT_DIR 생성, legacy 번들 — Phase 0가 kit.css 슬롯·icons 마커(셸)를 넣음
src/html_report.rs             T5  edit      VIEWER_TEMPLATE = include_str!(OUT_DIR/evidence_viewer.html); 동등성 테스트
src/serve/mod.rs               T5  edit      GET / → FRAMETRACE_UI=legacy 분기 (라우트 표 불변)
src/serve/tests.rs             T5  edit      examiner page 가드 테스트 보강
src/serve/tests_http.rs        —   불변      EXPECTED_ROUTES 변경 없음 (새 라우트 금지)
src/report.rs                  —   불변      (마크 범례 색 없음 — §5.6)
scripts/lib/app-bundle.mjs     T5  edit      build.rs 미러 — Phase 0가 kit/icons 반영
scripts/check-css.mjs          T1  edit      파일 목록(00-layout.css, kit.css), --fs-xs 13px, 신규 대비 쌍
scripts/check-js.mjs           T5  edit      id 검사에 viewer.body.html 포함, 두 번들 간 최상위 바인딩 중복 검사
scripts/check-a11y.mjs         T2  edit      PAGES 교체(§8.1), min-font 13
scripts/capture-screens.mjs    T2  rewrite   §9.4
scripts/ci-harnesses.sh        공유  앵커 라인(# @T1 …)만 교체 (Phase 0가 넣음)
scripts/verify-start-screen.mjs  T2 edit
scripts/verify-shell-jobs.mjs    T3 rewrite
scripts/verify-jobs-e2e.mjs      T3 edit
scripts/verify-live-index.mjs    T3 (무수정 통과 확인)
scripts/verify-i18n.mjs          T5 edit (viewer.body.html도 읽음)
scripts/verify-buttons.mjs       T1 NEW
scripts/verify-layout.mjs        T2 NEW
scripts/verify-progress.mjs      T3 NEW
scripts/verify-finalize-sheet.mjs T4 NEW
scripts/verify-embed.mjs         T5 NEW
scripts/audit-buttons.mjs        T1 삭제 (verify-buttons가 대체)
scripts/verify-stage3-embed.mjs, verify-fullscreen.mjs, shot-stages.mjs  T5 삭제 (iframe 전용 CDP 하네스)
scripts/user-walkthrough.mjs, real-user-test.mjs   T2 삭제 (단계 레일 전용 CDP 하네스; CI 미포함)
docs/SCREENSHOTS.md, docs/screens/INDEX.md         T2
docs/ui-rebuild-contract.md                         Phase 0 / 통합 담당
```

### 1.3 공유 파일 규칙

| 파일 | 규칙 |
|---|---|
| `assets/examiner_app.html` | **T2 단독**. T3/T4는 마크업을 넣지 않는다. T2가 마운트 노드 `<div id="hJobsSheet" class="h-sheet" hidden>`, `<div id="hFinalizeSheet" class="h-sheet" hidden>`, `<div id="jobPanel" class="h-chip" hidden>`를 비워 둔 채 제공하고, T3/T4는 JS 템플릿 문자열로 내부를 렌더한다(`check-js.mjs`의 id 검사는 JS 안의 `id="…"` 문자열을 인정한다 — scripts/check-js.mjs:83-85). |
| `build.rs`, `scripts/lib/app-bundle.mjs` | T5 단독. Phase 0가 `kit.css`·`icons.svg` 슬롯을 미리 넣었으므로 T1은 손대지 않는다. |
| `src/serve/mod.rs` | T5 단독(`GET /` 분기 1곳). 새 API 라우트는 어느 트랙도 추가하지 않는다 → `tests_http.rs` 불변. |
| `scripts/ci-harnesses.sh` | 배열 안의 앵커 라인 `# @T1 verify-buttons` 등을 **자기 라인만** 실제 항목으로 바꾼다(라인 추가 위치 고정 → 3-way 머지 무충돌). |
| i18n (`assets/viewer/20-i18n.js`, `21-i18n-layout.js`) | T5 단독. 셸은 한국어 전용(사전 없음). T1이 글리프를 빼야 하면 이 문서 §5.5의 "글리프→아이콘" 표를 근거로 T5가 수행한다. |
| `assets/tokens.css` | T1 단독. 다른 트랙은 토큰 **이름**만 참조(§5 확정). |

---

## 2. ID 맵

grep 대상: `scripts/*.mjs`, `scripts/lib/*.mjs`, `assets/app/*.js`, `assets/viewer/*.js` (276개 id; 마크업 교차: 셸∩뷰어 id 충돌은 `btnTheme` 1개). 뷰어 id는 **전부 불변**(§2.3). 셸 id는 아래 표.

### 2.1 접두 규칙 (충돌 해소)

- 셸 **class**는 전부 `h-` 접두(`.h-shell .h-top .h-sheet .h-pop .h-chip .h-modal-backdrop .h-modal .h-toast .h-badge .h-seg .h-sep .h-card .h-on …`). 예외 = `kit.css` 공용 클래스(§5.4: `primary quiet destructive lg icon ic sr-only muted mono`). 현재 충돌 18개(`badge card css failed js modal modal-backdrop muted ok on primary rail seg sep sr-only sub toast warn`)는 이 규칙으로 소멸한다.
- 셸 **id** 중 신규·개명은 `h` 접두 camelCase(`hTopStrip`). 하네스가 쓰는 기존 셸 id는 요소가 살아남는 한 유지한다.
- 뷰어 class/id는 변경하지 않되, 뷰어 CSS는 전부 `.ft-viewer` 하위로 스코프한다(T5). `body.embed/.theater/.rail-collapsed/.vgrid/.density-compact` 상태 클래스는 `#ftViewer` 루트로 옮긴다(`.ft-viewer.theater` …; JS는 `document.body.classList` → `els.root.classList`).
- 전역 JS 바인딩: 두 번들이 한 문서의 전역 렉시컬 스코프를 공유한다. 뷰어는 `99-loader.js`의 함수 7개(`ViewerLoadError liveChipText liveSaveIsClean loadDataBundle loaderLimits loaderOverlay startLiveIndexChip` + loader 내부 함수)와 `00-host.js`의 `FTViewer`만 최상위에 둔다. 셸은 `ft`/`FT`/`loader`/`viewer`/`live` 로 시작하는 최상위 이름을 쓰지 않는다. `check-js.mjs`가 두 번들의 최상위 `function|const|let|var|class` 이름 교집합이 비었는지 검사한다(T5).

### 2.2 셸 id (markup S) — 결정표

K=유지, R=개명(구→신), X=삭제. "하네스" = 바꿔야 하는 스크립트.

**상단 띠(옛 stagebar)** — 소유 T2

| id | 결정 | 비고 / 하네스 |
|---|---|---|
| `stageBar` | R → `hTopStrip` | `<header class="h-top">` 44px |
| `stageNav`, `s1`,`s2`,`s3`,`s4`, `s1State`…`s4State` | X | 단계 폐지. verify-start-screen(`#s1`,`#stageNav`), capture-screens(`#s3`,`#s4`), check-a11y(`.navstep`), shot-stages |
| `barCase` | R → `hProjectBtn` | 프로젝트 전환 버튼(케이스 이름 + 캐럿). aria-controls=`hProjectPop` |
| `jobPanel`, `jobDot`, `jobTitle`, `jobDetail`, `jobElapsed` | K | 칩 = `<div id="jobPanel" class="h-chip {running|ok|failed|cancelled|deferred}">` 안에 `<button id="hBtnJobs" aria-controls="hJobsSheet">`. 클래스 상태명 유지(verify-jobs-e2e `body.cancelled` 정규식). 소유 T3(내부), T2(마운트) |
| `btnJobCancel`, `btnJobReload`, `btnJobClose` | K | 작업 시트 안으로 이동(칩에는 `hBtnJobs`만). verify-jobs-e2e/shell-jobs는 시트 열림 상태에서 클릭 (T3) |
| `caseMenuWrap`, `btnCaseMenu`, `caseMenu` | X | 케이스 작업 메뉴 폐지. 복구/카빙은 작업 시트 "작업 시작" 구역. verify-jobs-e2e·capture-screens(`#btnCaseMenu`) → `#hBtnJobs` (T3/T2) |
| `btnViewerFull` | X | 뷰어 자체 `f`/`btnFullscreen` 사용. verify-fullscreen 삭제 |
| `btnViewerWindow` | R → `hBtnPopout` | 상단 띠 "새 창" (T2 23-popout.js) |
| `lnkDashboard` | R → `hLnkDashboard` | 프로젝트 팝오버로 이동 |
| `btnRecoverDeleted`, `btnCarve`, `reassembleWrap`, `chkReassemble` | K | 작업 시트 "작업 시작" 구역(T3 렌더). hidden 규칙 유지(케이스 열림 시 표시) |
| `recoverHint` | R → `jobsNote` | 작업 시트 상태 한 줄. verify-shell-jobs(1곳), user-walkthrough(삭제) |
| `btnTools`, `toolsPop`, `toolsCount`, `env` | R → `hBtnTools`, `hToolsPop`, `hToolsCount`, `hEnv` | real-user-test(삭제) |
| `btnTheme` | R → `hBtnTheme` | **충돌 해소**(뷰어 `btnTheme` 유지). capture-screens, verify-shell-jobs |
| `btnViewerLang` | R → `hBtnLang` | verify-shell-jobs |
| `btnQuit` | R → `hBtnQuit` | `class="quiet destructive icon"` |
| (신규) | `hBrand`, `hBtnFinalize`(결과물 시트 오프너, secondary), `hBtnHelp`+`hHelpPop`(? 도움 팝오버, 옛 onboard 대체), `hProjectPop`, `hProjectRecent`, `hProjectOpenOther`, `hProjectNew` | T2 |

**배너·토스트·SR·종료** — T2

| id | 결정 |
|---|---|
| `serverBanner`, `serverBannerMsg`, `btnServerRetry` | K (`class="h-srvbanner"`, 위치 §6.2) |
| `toastBox` | R → `hToastBox` (§6.5 단일 호스트) |
| `srStatus`, `srAlert` | K |
| `quitPage` | K |
| `main` | K (`<main id="main" class="h-main">`) |

**빈 상태(옛 stage 1)** — T2. `verify-start-screen` 고정 id:

| id | 결정 | 비고 |
|---|---|---|
| `inputCard`, `inputTitle` | K | `<section id="inputCard" class="h-empty" data-keys="off">` — 프로젝트 없을 때만 표시 |
| `onboard`, `btnOnboardOk` | X | 안내 → `hHelpPop` |
| `btnOnboardShow` | R → `hBtnHelp` | verify-start-screen "사용 안내 still reachable" → `#hBtnHelp` |
| `blkEvidence`, `blkEvidenceTitle`, `blkEvidenceDesc`, `sourceLabel`, `sourcePath`, `recentSources`, `btnBrowseSource`, `btnStart`, `kindChip`, `sourceStatus`, `startHint`, `e01Need`, `preflight`, `resumeBox`, `resumeText`, `btnResumeContinue`, `btnResumeFresh`, `startErr`, `startOptions`, `optSummary`, `caseDir`, `recentCases`, `btnBrowseCase`, `btnSuggestCase`, `caseStatus`, `btnOpenCase`, `scanModeLabel`, `modeInfo`, `withHash`, `withFfprobe`, `withDeepfake`, `verifyRow`, `skipE01Verify`, `directE01` | K | 옵션 disclosure 구조 그대로. `kindstore` 라디오(`input[name='inputKind']`) 유지 — Rust 가드 src/serve/tests.rs:895-902 |
| `blkRecent`, `blkRecentTitle`, `recentCaseRow`, `recentEmpty`, `btnOpenOther` | K | 최상단 행 버튼 = `[이어서 검토]` `class="primary lg"`(`.rc-act`), 나머지 행 `quiet`. `btnOpenOther`는 목록이 비었을 때만 `primary` |
| `blkViewer`, `blkViewerTitle`, `btnViewerOpen`, `viewerReason` | X | 블록 삭제. verify-start-screen(a/c/g 항목) 수정 |

**분석 진행(옛 stage 2)** — T3 (작업 시트 내부 렌더)

| id | 결정 |
|---|---|
| `progCard`, `progTitle` | X (capture-screens `#progCard.on` → `#hJobsSheet:not([hidden])`) |
| `track`, `log`, `btnCancel`, `btnNew`, `btnCopyLog`, `btnSaveLog`, `btnLogBottom` | K (시트 안) — verify-start-screen `#track`, capture-screens `#track`·`#log` |
| (신규) | `hJobsSheet`(T2 마운트), `hJobsTitle`, `hJobsClose`, `jobsNote`, `jobsStart`(작업 시작 구역), `jobsClips`(클립 내보내기 잡 목록), `jobsCounts`(색인 n/총 m) |

**검토(옛 stage 3)** — T5/T2

| id | 결정 |
|---|---|
| `resultCard`, `resultTitle`, `caseStats`, `reviewFrame`, `frameFail`, `frameFailTitle`, `frameFailMsg`, `btnFrameRetry`, `btnFrameWindow`, `framePopout`, `btnFrameReturn`, `btnFrameFocusPopup` | X — iframe 폐지. verify-start-screen(`#resultCard.on`,`#reviewFrame`), verify-jobs-e2e(`#reviewFrame`, `frames()`), verify-shell-jobs(frame 테스트 6개), capture-screens, check-a11y(`iframe`) |
| (신규) | `hViewerHost`(T2 마운트: `<div id="hViewerHost" class="h-host" hidden>` — 조각 `<!-- @include:viewer.body -->`가 이 안에 들어감), `hPopoutNote`(보조 창 열림 배너), `hBtnPopoutReturn`, `hBtnPopoutFocus` (T2) |

**결과물(옛 stage 4)** — T4 (시트 내부 렌더)

| id | 결정 |
|---|---|
| `finalCard`, `finalTitle`, `reportWrap`, `reportFrame` | X (보고서 미리보기는 새 탭 링크) |
| `btnFinalize` | K — 시트의 유일한 `primary lg`, 라벨 "보고서 생성·패키징" (capture-screens, real-user-test) |
| `btnVerifySources`, `marksFile`, `btnImportMarks`, `finalHint`, `finalLinks`, `pkgPath`, `advPath`, `advOut`, `advLink`, `button[data-adv=…]` | K — `btnVerifySources`는 체크리스트 행의 보조 버튼, 나머지는 `<details id="finalAdvanced">` 안 |
| `dlgExaminer`, `dlgRecheck`, `dlgRetrySave` | R → `finalExaminer`, `btnFinalRecheck`, `btnFinalRetrySave` (체크리스트가 시트 본문이 되므로 askDialog 사용 안 함). capture-screens(`#dlgExaminer`) |
| `auditBadge` | K (JS 생성, 시트 체크리스트 행으로 이동) |
| (신규) | `hFinalizeSheet`(T2 마운트), `finalChecklist`, `finalAdvanced`, `hFinalizeClose` |

**대화상자·피커** — T2: `pickerBackdrop pickerTitle pickerClose pickerPath pickerUp pickerList pickerStatus pickerSelect pickerCancel dlgBackdrop dlgTitle dlgBody dlgNote dlgOk dlgCancel` 전부 K (class만 `h-modal-backdrop`/`h-modal`). `crumbs` X (verify-start-screen·verify-shell-jobs).

### 2.3 뷰어 id — 전부 K (T5)

`btnExitFs caseLine linkCarveReport btnTheme btnLang btnRail query kind status btnMoreFilters filtersExtra btnDensityComfortable btnDensityCompact sortBy groupBy btnGroupKind tagFilter pageSize dateFrom dateTo btnClearDates pillVerified metricVerified pillFailed metricFailed pillCandidate metricCarved pillAnomaly metricAnomaly btnViewMenu viewMenuList btnTheater btnFullscreen btnPip btnTimeline btnShortcuts leftRail railTitle presetChips facetTree calTitle dayHistogram periodLabel mainArea caseWarnings timelinePanel timelineMeta btnTimelineGen btnTimelineClose timelineList activeFilters recordGrid dflSummary triageStatus resultCount prevPage pageStatus nextPage gridLive vsplitter mediaTitle mediaStatus mediaStage mediaToolbar btnSkipBack btnSkipFwd playRate btnStepBack btnStepFwd btnSetIn btnSetOut rangeLabel tbMore btnMoreTools moreToolsList toolsRow btnPopPlayer btnDual btnDownloadFile btnCaptureFrame btnExportClip btnExportClipBurn btnProxy videoSizeLabel videoMode videoZoom metricVideos hsplitter detailsTitle detailBadges detailTabs tabSummary tabInspector tabValidation tabTelemetry tpSummary summaryList tpInspector metaList tpValidation validationList tpTelemetry telemetryTitle btnTelemetry telemetryPane telemetryCanvas telemetryNote telemetryEmpty selectionBar scopeToggle btnScopeActive btnScopeSelected scopeSelLabel btnSelectFiltered btnClearSelection selectionCount btnMarkReviewed btnMarkImportant btnMarkVerify btnMarkClear btnTagMenu tagMenuList examinerName saveState btnApplyMarks btnExportMenu exportMenuList btnDownloadFiles btnExportSelected btnTranscodeQueue btnDownloadCsv btnSummary btnDownloadMarks btnDownloadSelection btnCopyIds btnCopyPaths toastHost shortcutsModal shortcutsTitle btnShortcutsClose` 및 JS 생성 id `ft-live-chip ft-load-note calPrev calNext calYear chipMore customTagInput btnAddCustomTag btnSaveCustomTag evidenceNote tagPresetInput cmdPalette cmdList reviewProgress markConflictBanner markConflictPanel legacyRangesNotice legacyRangesSave legacyRangesDrop annotationsErrorBanner` (R4: `rangeDivergenceBanner rangeUseServer rangeUseBrowser` 삭제 — 구간 불일치는 일반 충돌 목록, §11.7) + 팝업 플레이어 문서의 `stage vv novid title meta note list play rate prev next cap wrap left b1…b10 f1…f10`.

신규 뷰어 id(T5): `ftViewer`(루트). 조각 안의 `btnTheme`/`btnLang`/`.app-header`는 standalone 전용이며 호스트에서는 `.ft-viewer.hosted .app-header{display:none}`.

### 2.4 하네스 전용 문자열

`shortcutModal`(audit-buttons 오타, 파일 삭제), `vid_1`/`vid_d`(데이터 id, 불변), `b2560f`/`c0392b`/`f2b8ad`(색상 hex 오탐 — 99-loader/60-details의 인라인 색은 T5가 토큰으로 교체).

---

## 3. `window.FTViewer` API (T5)

파일 `assets/viewer/00-host.js`(번들 첫 파일). 뷰어 번들은 `window.FTViewer` **하나만** 노출하고(`__frametraceSummary`, `ftAnnounce`, `__ftBridge`는 호환용으로 유지), 셸은 이 객체로만 뷰어를 조작한다. DOM 접근(`document.getElementById("saveState")` 등)은 셸 코드에서 금지(`verify-embed`가 셸 번들 소스에서 뷰어 id 문자열을 grep 해 실패시킨다).

```js
window.FTViewer = {
  version: 2,
  // 마운트. host: 조각 루트(#ftViewer)를 포함하는 요소(기본 document). data: 인라인 번들(옛 __FRAMETRACE_DATA__). url: 레코드 API base(기본 "" = 같은 origin /api/records). embed: 헤더 숨김.
  init({ data = null, url = "", embed = false, host = document, theme = null, locale = null, restore = null } = {}) → Promise<void>,   // restore: 내부용 — reload()가 넘기는 보기 상태(§11.1)
  destroy() → void,                 // AbortController.abort() + root.innerHTML 유지(마크업은 남고 상태·리스너만 제거)
  reload({ keepUnsaved = true, reason = "" } = {}) → Promise<"reloaded"|"refused">,
  setLocale("ko"|"en"), getLocale() → "ko"|"en",
  setTheme("light"|"dark"), getTheme(),
  focus() → boolean,                // 활성 카드(없으면 #recordGrid)에 포커스
  summary() → { total, byKind, byStatus, warned, marked, noted, pending, lockCause },   // = __frametraceSummary() + pending(미저장 건수) + lockCause(R9 U2: 읽기 전용 원인 "case" | "annotations" | "")
  saveState() → "saved"|"pending"|"saving"|"failed"|"queued"|"readonly"|"unknown"|"local"|null,   // readonly = 판독 기록 읽기 실패로 저장 차단(R4 A3), unknown = 저장 충돌 확인 실패(R4 A5) — 둘 다 "저장됨"이 아니다
  save({ timeoutMs } = {}) → Promise<boolean>,   // 진행 중 저장의 응답을 먼저 기다리고 남은 것을 보낸다(R4 A15). true = 이 창에 미저장 없음(서버 대기열은 내구 저장이라 true), false = 실패·충돌·확인 실패·읽기 전용·시간 초과. timeoutMs = 이 호출의 응답 **전체 상한**(진행 중 저장 대기·충돌 조회·전송·busy 재시도 모두 포함, R7 §14.1) — 넘으면 false + `opts.timedOut = true`(요청은 계속 진행). **R8 §15.2**: `{ patient: true }` = 명시적 저장(기본 20 s, 요청이 진행 중이면 연장), 인자 없는 `save()` = 4 s 고정
  caseStorageKey(manifest, caseDir) → string,   // R8 §15.1: 저장소 키·채널의 케이스 부분(case_id, 없으면 "nocase-<케이스 폴더 해시>", 절대 "case" 아님)
  setExaminer(name), getExaminer() → string|null,   // 검토자(마크 귀속). 호스트 선택 바에는 입력란이 없고 41-finalize `#finalExaminer`가 호출
  attributeUnassignedMarks(name) → Promise<number>,   // R4 A9: 그 마크들의 저장 응답까지 기다린 뒤 건수로 resolve, 저장 실패·충돌이면 reject(성공 통보 금지).   // setExaminer(name) 후, 검토자가 비어 있는 마크·메모를 같은 드래프트→저장 경로로 재전송하고 건수를 돌려줌(0이면 무동작). 서버 upsert는 `examiner = COALESCE(excluded.examiner, 기존)`이라 비어 있지 않은 새 값이 기존 값을 덮고 `review_mark_history`에 update로 남는다(src/case_db/marks.rs). setExaminer만으로는 이미 저장된 마크가 바뀌지 않는다
  writerToken,                       // 이 창의 고유 토큰: 이 창이 연 팝아웃만 `ft-player:<case>` 메시지에 담아 보내고, 토큰이 다른 창(같은 케이스의 다른 셸 탭)은 응답하지 않는다(단일 writer)
  liveState() → { live, generation, total, canReload }|null,   // = 마지막 `live-state` detail
  popout() → Window|null,            // 70-player.js openPopupPlayer(): 영상+판독 창
  closePopout(), isPoppedOut() → boolean,
  toast(message, kind = "info") → void,     // §6.5 단일 토스트 호스트
  _debugStats() → { filterComputes, activeId } | null,       // 내부용(하네스): 필터+정렬 전체 재계산 횟수(메모 미스)와 활성 id — verify-virtual-grid U4/U7
  _whenSaveSettled(timeoutMs = 15000) → Promise<boolean>,   // 내부용(22-viewer-host 언마운트): 진행 중인 마크 저장 요청이 끝나면 true, 시간 초과면 false. 응답이 이 마운트의 드래프트를 정리한 뒤 destroy() 하기 위함
  on(event, fn, opts), off(event, fn)       // EventTarget; fn(CustomEvent) — detail는 아래 표
};
```

### 3.1 라이프사이클

| 상황 | 동작 |
|---|---|
| **standalone** (`/review/evidence-viewer.html`, `/viewer/`, 패키지 파일, 레거시 iframe `?embed=1`) | `99-loader.js` 끝: `if (!document.documentElement.hasAttribute("data-ft-host")) FTViewer.init({ embed: new URLSearchParams(location.search).has("embed") || window.self !== window.top })`. 데이터 소스 판별은 현 `loadViewerData()` 순서 그대로(인라인 → `/api/records` → `data-bundle.js`). |
| **hosted** (`GET /`) | `examiner_app.html`은 `<html data-ft-host="shell">`. 자동 init 없음. `22-viewer-host.js`가 프로젝트 열림(`FTStore.state.phase ∈ {review-partial, review-ready, done, finalized}`)에 `FTViewer.init({ embed: true, host: $("hViewerHost"), theme, locale })`를 호출하고 `hViewerHost.hidden=false`, `inputCard.hidden=true`. 프로젝트 전환: `await FTViewer.save()` 성공(또는 저장 없음) 확인 → `FTViewer.destroy()` → `/api/open-case` → `init()`. `init()` 재호출은 `destroy()` 없이는 거부(`throw Error("FTViewer already mounted")`). |
| 번들 평가 시점 | `10-boot.js`의 `(async function () {` → `async function ftViewerBoot(opts) {`, `95-init.js`의 `})();` → `}`. `00-host.js`는 `FTViewer.init = opts => ftViewerBoot(opts)` 호이스팅 함수에 의존(TDZ 없음). |
| 랜드마크 | 조각의 `#mainArea`는 `role="main"`인 `<div>`(standalone: 페이지의 유일한 main). hosted `init()`은 이를 `role="region"` + `aria-label`("증거 검토", i18n `layout.review`)로 바꾼다 — 셸의 `<main id="main">`이 페이지의 유일한 main. `verify-embed` 9번이 양쪽 모두 main 정확히 1개를 확인(axe `landmark-*` 포함). |
| 호스트 클래스 | `reload()`(destroy→init)는 호스트가 `#ftViewer`에 건 `.popped`를 보존한다(destroy 단독은 원본 className으로 복원). |
| 리스너 | 뷰어가 `document`/`window`/`matchMedia`에 거는 모든 리스너는 `{ signal: FTViewer._ac.signal }`로 등록(T5가 grep 기준: `addEventListener(` 호출 전부). `destroy()`는 abort 후 새 AbortController를 만든다. |
| 데이터 재적재 | `reload()`는 현 `ft-reload` 핸들러 로직(95-init.js:28-63)을 함수화: 미저장이면 `"refused"`(저장 시작 + `toast(t("reload.refused"))`), 아니면 **페이지 리로드 대신** `destroy()`→`init()`(호스트)·`location.reload()`(standalone). 호스트에서 `keepUnsaved=false`면 미저장 무시하고 재적재(사용 금지: 호출자는 T3가 아니라 사용자 확인 대화상자를 거친 T2만). 어느 경로든 **보기 상태(필터·검색·정렬·묶기·선택·활성 카드·그리드 스크롤·Undo/Redo 저널)를 보존**한다(§11.1, U4). |
| postMessage 호환 | 레거시 iframe(§9.3)을 위해 `ft-theme/ft-locale/ft-reload/ft-embed` 메시지 핸들러는 standalone 모드에서 유지한다. 호스트 모드에서는 등록하지 않는다. |

### 3.2 이벤트 (CustomEvent 이름 / detail / 리스너)

| 이벤트 | detail | 발생 | 듣는 쪽 |
|---|---|---|---|
| `ready` | `{ total, live, generation }` | init 완료(렌더 1회 후) | 22-viewer-host(T5), 01-store(T2) |
| `records` | `{ total, byKind, byStatus, generation }` | 데이터 적재/재적재 후 | 31-progress(T3: 칩 "n/m") |
| `save-state` | `{ state, pending, queued, conflicts, auth }` (`auth`: 마지막 저장 시도가 401로 거부됨 — 저장이 통과하면 false) | `renderSaveState()` 마다 | 01-store, 40-finalize(T4), 23-popout(T2) |
| `dirty` | `{ count }` | `markReviewDirty()/clearReviewDirty()` | 01-store, 40-finalize |
| `live-state` | `{ live, generation, total, canReload }` | 마운트 데이터의 색인 세대(`live`=부분 색인으로 적재) · 지금 `reload()`가 받아들여질지(미저장 없음). 값이 바뀔 때만 | 22-viewer-host → `viewer.live/liveGeneration/canReload`; 31-progress·32-jobs가 `/api/status`의 `index_generation`과 비교해 칩 "보강 완료"·`btnJobReload` "새 데이터 반영 (n건)" 표시. **호스트에서 색인·보강 상태의 단일 출처는 셸의 `/api/status` 폴링**이며 뷰어는 `#ft-live-chip`을 만들지 않고 상태를 폴링하지 않는다 |
| `selection` | `{ ids: string[], count }` | 선택 변경 | 01-store |
| `active-record` | `{ id, kind, status, mark }` | 활성 카드 변경 | 01-store, 23-popout |
| `locale` | `{ locale }` | 언어 변경(버튼·API) | 15-theme(T2: 라벨 동기) |
| `theme` | `{ theme }` | 테마 변경 | 15-theme |
| `request-popout` | `{}` | 뷰어 `btnPopPlayer` 클릭 | 23-popout(T2: 셸이 `popout()`를 대신 호출하고 본 창 영상을 접음) |
| `popout` | `{ open: boolean }` | 팝업 창 열림/닫힘 감지 | 23-popout, 01-store |
| `toast` | `{ message, kind }` | 뷰어 `toast()` 호출 | 20-net(T2) — 호스트 모드에서만 셸이 표시 |
| `error` | `{ message, detail, stage: "load"|"save"|"export", auth? }` | 로더 실패·저장 실패. `auth: true` = 서버가 401로 거부(쿠키 없음·다른 인스턴스의 쿠키, U8). 401 재시도마다 다시 보냄(`repeat: true`, 토스트는 처음 한 번) | 01-store(배너), 31-progress, 22-viewer-host(`auth`면 `noteAuthExpired()`) |
| `derived-job` | `{ kind: "join", id, state: "started"|"running"|"done"|"failed"|"cancelled", started_unix }` | 뷰어 안에서 시작한 파생 작업(이어붙이기)의 시작·상태 변화(80-review.js `emitDerivedJob`) | 22-viewer-host → 31-progress `noteViewerDerivedJob`: 실행 중엔 빠르게 폴링, `done`이면 5 s 유휴 폴링이 실행을 못 봤어도 '새 데이터 반영'을 제안 |
| `destroyed` | `{}` | destroy 완료 | 22-viewer-host |

### 3.3 BroadcastChannel

- `ft-review` (T2 23-popout.js, 세션 단위, 이름 고정 `"ft-review"`): 메시지 `{ type: "popout-open"|"popout-closed"|"theme"|"locale"|"project-changed", caseDir, value, from: instanceId }`. 수신자: 같은 origin의 다른 셸 탭·팝아웃 창. 쓰기는 **본 창(셸)만**: 팝아웃은 마크를 직접 저장하지 않고 `ft-player:<case_id>`(기존 70-player.js 브리지, 불변)로 본 창에 위임한다 → "단일 writer"는 구조적으로 보장된다.
- `ft-alive:<case_id>` (save protocol v2, 30-records.js `openAliveChannel`): 같은 origin의 창끼리 `{type:"ping", nonce, writers[], from}` / `{type:"pong", nonce, writer}`. 다른 writer의 드래프트를 넘겨받기 전에(하트비트 `ft.alive:<case>:<writer>`가 5분 넘게 멈췄을 때) 그 창이 살아 있는지 묻는다 — 응답하면 드래프트를 건드리지 않는다(§11.2).
- `ft-player:<case_id>` (T5, 불변): 팝업 플레이어 ↔ 본 창 브리지(`focus/navigate/mark/toggleTag/list/undo/redo`). 모든 요청은 `token`(= 팝업을 연 창의 `FTViewer.writerToken`)을 담아야 하며, 토큰이 일치하는 창만 응답한다.

---

## 4. 셸 스토어 (T2 `assets/app/01-store.js`)

```js
const FTStore = {
  state: {
    project:  { caseDir: "", name: "", packageDir: "", restored: false, sourcePath: "" },
    phase:    "empty",      // "empty"|"running"|"review-partial"|"review-ready"|"done"|"finalized"|"error"|"server-lost"
    jobs: {
      pipeline: { steps: [], stepNames: [], current: "", progress: null, enrichment: null, warnings: [], logs: [], busy: false, error: "" },
      caseJob:  null,       // /api/status.job 그대로 + { outcome, message } (옛 caseJob 객체, T3)
      clipJobs: [], clipsRunning: 0,
      pendingMarks: 0, jobKind: null,
    },
    counts:   { indexed: 0, total: 0, generation: 0 },   // indexed = FTViewer records.total, total = /api/records live total
    panels:   { jobs: false, finalize: false, project: false, tools: false, help: false },
    popout:   { open: false, writer: "main" },           // writer는 항상 "main"(팝아웃은 writer가 아님)
    viewer:   { mounted: false, saveState: null, dirty: 0, selection: 0, activeId: null, locale: "ko", theme: "light", live: false, liveGeneration: null, canReload: true },
    server:   { instance: "", lost: false, auth: false, failures: 0 },   // lost = 배너가 보임, auth = 배너가 인증 오류 모습(401, U8) — §11.5
    env:      null,                                      // /api/env 응답
  },
  get(path) , set(patch /* 얕은 병합, 키별 */) , on("change", fn /* ({keys:Set<string>}) */), off(),
};
```

이벤트: `change` 하나(변경 키 집합 전달). 파생값은 함수로: `FTStore.isBusy()`, `FTStore.hasProject()`, `FTStore.canFinalize()`(phase ∈ {review-ready, done, finalized} ∧ !busy). 폴링: `40-status.js`(T3)가 `/api/status`를 **잡이 있을 때 1 s, 없을 때 5 s**로 읽어 `set({phase, jobs, project, server})`. 뷰어 이벤트 → `viewer.*`는 `22-viewer-host.js`(T5)가 set. 패널 열림/닫힘은 `97-shell.js`(T2)만 set.

---

## 5. 토큰 · 타입 · 간격 · z-index · 버튼 · 아이콘 (T1)

### 5.1 색 토큰 (전체, light / dark) — 대비는 §5.2에 계산

| 토큰 | light | dark | 용도 |
|---|---|---|---|
| `--bg` | `#ecebe7` | `#121416` | 페이지(종이) |
| `--surface` | `#fbfaf7` | `#1b1e21` | 카드·시트·팝오버 |
| `--surface-2` | `#f3f2ee` | `#24282c` | 호버·보조면 |
| `--text` | `#1a1c1e` | `#e6e4df` | 본문 |
| `--muted` | `#55595e` | `#a7acb2` | 보조 텍스트 |
| `--border` | `#d6d3cc` | `#33383d` | 구분선 |
| `--border-strong` | `#858177` | `#767d84` | 컨트롤 외곽선(≥3:1) |
| `--accent` | `#234e8c` | `#8db4ea` | 잉크 블루 |
| `--on-accent` | `#ffffff` | `#0f1a2b` | 악센트 위 글자 |
| `--accent-ink` | `#1c3f73` | `#a9c7f2` | 악센트 텍스트/호버 |
| `--accent-soft` | `#e4ebf5` | `#22304a` | 선택·활성 배경 |
| `--disabled-fg` / `--disabled-bg` | `#6f7276` / `#e6e4df` | `#848a90` / `#1b1e21` | 비활성 |
| `--media-bg` / `--media-muted` | `#141618` / `#b4b9be` | `#0e1012` / `#b4b9be` | 영상 스테이지(양 테마 어둡게) |
| `--ok` `--danger` `--warn` `--info` | `#2a6b3a` `#b3261e` `#7a5200` `#234e8c` | `#7fcb95` `#ff8a80` `#e8b45a` `#8db4ea` | 의미색(상태에만) |
| `--verified-fg/-bg` | `#2a6b3a` / `#e4f1e7` | `#7fcb95` / `#17352a` | |
| `--failed-fg/-bg` | `#b3261e` / `#fbeae8` | `#ff8a80` / `#3d2422` | 빨강 = 실패 |
| `--candidate-fg/-bg` | `#5e4f91` / `#eeecf6` | `#b8a9ec` / `#2a2640` | |
| `--original-fg/-bg` | `#1c3f73` / `#e4ebf5` | `#a9c7f2` / `#22304a` | |
| `--carved-fg/-bg` | `#96470a` / `#fbeadd` | `#f0a060` / `#3a2616` | |
| `--fs-recovered-fg/-bg` | `#1f4fb0` / `#e5ecfa` | `#8fb0f5` / `#1c2a4a` | |
| `--deleted-candidate-fg/-bg` | `#9b1c1c` / `#fbe6e6` | `#ff8f8f` / `#3d2222` | 빨강 = 삭제 후보 |
| `--anomaly-fg/-bg` | `#7a5200` / `#f8efd9` | `#e8b45a` / `#3a2e1a` | |
| `--warn-fg/-bg` | `#7a5200` / `#fbf1dc` | `#e8b45a` / `#3a2e1a` | |
| `--info-fg/-bg` | `#234e8c` / `#e4ebf5` | `#8db4ea` / `#22304a` | |
| `--mark-reviewed-fg/-bg/-ink` | `#3d4146` / `#e2e1dc` / `#55595e` | `#d6d9dd` / `#3a3f45` / `#a7acb2` | 판독완료 = **회색 체크** |
| `--mark-important-fg/-bg/-ink` | `#5c3a00` / `#f6dfb3` / `#8a5a00` | `#1f1500` / `#e0a94a` / `#e0a94a` | 중요 = **호박색 깃발** |
| `--mark-needs-verification-fg/-bg/-ink` | `#4a3b7f` / `#e7e2f5` / `#5e4f91` | `#15102a` / `#b8a9ec` / `#b8a9ec` | 검증대기 = **보라 물음표** |
| `--focus-ring` | `2px solid var(--accent)` | 동일 | offset 2px |
| `--shadow-pop` | `0 4px 16px rgba(0,0,0,.14)` | `0 6px 20px rgba(0,0,0,.45)` | 팝오버·메뉴 |
| `--shadow-sheet` | `-8px 0 24px rgba(0,0,0,.12)` | `-8px 0 28px rgba(0,0,0,.5)` | 시트 |

삭제되는 토큰: `--shadow`(카드 그림자 금지), `--radius-sm/--radius-lg`(이행기 동안 `--radius` 별칭으로 1릴리스 유지), `--fs-xl`. `*-ink` 마크 토큰은 **아이콘 단색**(체크/깃발/물음표를 surface 위에 그릴 때)용 — 텍스트가 아니므로 ≥3:1.

### 5.2 대비 계산 (WCAG 2.2 AA; `scripts/check-css.mjs` 공식과 동일, light / dark)

| 쌍 | bg | surface | surface-2 |
|---|---|---|---|
| `--text` | 14.3 / 14.5 | 16.4 / 13.2 | 15.3 / 11.7 |
| `--muted` | 5.9 / 8.1 | 6.8 / 7.3 | 6.3 / 6.5 |
| `--accent-ink` | 8.8 / 10.7 | 10.0 / 9.7 | 9.3 / 8.6 |
| `--ok` | 5.4 / 9.6 | 6.2 / 8.7 | 5.7 / 7.7 |
| `--danger` | 5.5 / 8.1 | 6.3 / 7.3 | 5.8 / 6.5 |
| `--warn` | 5.8 / 9.8 | 6.6 / 8.9 | 6.2 / 7.9 |
| `--info`/`--accent`(텍스트) | 6.9 / 8.7 | 7.9 / 7.9 | 7.4 / 7.0 |

`--on-accent` on `--accent` 8.3 / 8.2 · `--disabled-fg` on `--disabled-bg` 3.8 / 4.8 (≥3) · `--media-muted` on `--media-bg` 9.2 / 9.6 · `--border-strong` on {bg,surface,surface-2} ≥3.0 양 테마(비텍스트 1.4.11) · 상태 fg/bg: verified 5.5/6.9, failed 5.6/6.2, candidate 6.0/6.8, original 8.7/7.6, carved 5.6/6.7, fs-recovered 6.3/6.6, deleted-candidate 6.8/6.6, anomaly 6.0/7.0, warn 6.2/7.0, info 6.9/6.2 (각 fg는 세 surface 위에서도 ≥5.4) · 마크 fg/bg: reviewed 7.8/7.5, important 7.8/8.5, needs-verification 7.5/8.7; `*-ink` on surface 6.8/7.3, 5.7/7.9, 6.7/7.9. **89쌍 × 2테마 전부 통과**(계산 스크립트는 check-css.mjs에 T1이 동일 쌍으로 추가한다: 기존 쌍 + `--media-muted/--media-bg` 4.5 + `--border-strong`·`--accent` on 3 surface ≥3 + `--mark-*-ink` on 3 surface ≥3).

### 5.3 타입 · 간격 · 모양 · 모션

| 토큰 | 값 | 비고 |
|---|---|---|
| `--font-sans` / `--font-mono` | 현행 유지 | 오프라인 시스템 폰트 |
| `--fs-xs` | **13px** | 최소 글자(캡션·kbd·메타). check-css `--fs-xs: 13px`, check-a11y min-font 13 |
| `--fs-sm` | **14px** | 본문·컨트롤 |
| `--fs-md` | 16px | 제목(굵기 600) |
| `--fs-lg` | 20px | 빈 상태 제목·대화상자 제목 |
| `--lh-body` 1.5 / `--lh-ctl` 1.35 | 유지 | |
| `--num` | `font-variant-numeric: tabular-nums` 적용 클래스 `.num`; 해시·타임코드·경과·건수는 `.mono.num` | kit.css |
| `--sp-1…6` | 4/8/12/16/24/32 | 유지 |
| `--radius` | **3px** | `--radius-pill: 999px`(칩·필) |
| `--ctl-h` 32px · `--ctl-h-lg` 36px · `--ctl-h-icon` 28px · `--hit-min` 24px | 버튼 높이 | |
| `--top-h` 44px · `--rail-w` 240/280 · `--media-w` 480/640 · `--sheet-w` 480/560 | 레이아웃(§6) | 1600px 분기 |
| `--dur-fast/--dur/--dur-slow` 120/150/180ms, reduced-motion 0 | 유지 | |

### 5.4 버튼 (kit.css, 양 번들 공통)

종류 4(클래스): 기본 `<button>` = **secondary**(1px `--border-strong` 외곽, `--surface`), `.primary`(채움 `--accent`/`--on-accent`, **영역당 1개** — 영역 = `[data-region]` 속성: `hTopStrip`, `inputCard>blkEvidence`, `blkRecent`, `hJobsSheet`, `hFinalizeSheet`, `selectionBar`, `mediaToolbar`, `h-modal`), `.quiet`(투명, 호버 시 `--surface-2`; 툴바·메뉴·칩 보조), `.destructive`(`--danger` 외곽+글자, `.primary.destructive`는 채움 — 확인 대화상자 뒤에서만). 크기 수식: `.lg`(36px, `.primary` 기본 36), `.icon`(28×28, 아이콘만, `aria-label` 필수, 인접 간격으로 24px 히트 확보 — 툴바 `gap ≥ 4px`), 기본 32px, **24px 미만 금지**.
상태 5: default · `:hover` · `:active` · `:focus-visible`(`--focus-ring`, offset 2px; 메뉴 항목은 -2px) · `:disabled`(`--disabled-*`, `cursor:not-allowed`, `title`에 사유). 추가 수식 `[aria-busy=true]`(스피너 `::before`, `cursor:progress`). 각 상태는 양 테마에서 배경 **또는** 외곽선 **또는** 글자색이 서로 달라야 한다(verify-buttons가 computed style 비교).
구 클래스 → 신 클래스: `ghost|hbtn|mini|btn-sm|mi|linkbtn|icon-btn|cal-btn|thumb-retry|toast-x` → `quiet`(+`icon` 필요 시); `danger` → `destructive`; `big` → `lg`; `primary` 유지; `chip`/`pill`(필터 토글)은 kit가 아니라 뷰어 CSS 컴포넌트(T5)로 남되 높이 28·`aria-pressed` 시각화는 kit의 `.toggle[aria-pressed=true]` 규칙을 공유한다.
입력: `input[type=text|search|date]`, `select` 32px, `--border-strong`, `:focus-visible` 링, `:disabled`. `kbd` 18px `--surface-2`.

### 5.5 아이콘 스프라이트 (`assets/icons.svg`, `<symbol id="i-*" viewBox="0 0 16 16">`, `fill="none" stroke="currentColor" stroke-width="1.5"`)

사용법: `<svg class="ic" aria-hidden="true"><use href="#i-check"/></svg>` (16px, `vertical-align:-3px`). 아이콘만 있는 버튼은 `aria-label`. `check-a11y`·`verify-buttons`가 버튼 텍스트의 유니코드 글리프를 금지한다.

| id | 대체하는 글리프/용도 | id | 대체 |
|---|---|---|---|
| `i-check` | ✓ (판독완료 마크, 검증됨 필, 완료 단계) | `i-flag` | 중요 마크 |
| `i-question` | ? (검증대기 마크, 후보 필) | `i-x` | ✕ (실패 필, 닫기) |
| `i-warning` | ⚠ (이상, 경고) | `i-info` | i |
| `i-menu` | ☰ (레일 토글) | `i-chevron-down` | ▾ (메뉴 캐럿) |
| `i-chevron-right` / `i-chevron-up` | ▸ (details), 드롭업 | `i-external` | ↗ (새 탭 링크) |
| `i-theme` | ◐ / ☀ | `i-power` | ⏻ (서버 종료) |
| `i-popout` | ⧉ (새 창/팝아웃) | `i-fullscreen` | ⤢ |
| `i-download` | ⤓ (저장) | `i-camera` | 프레임 캡처 |
| `i-clip` | 구간 내보내기 | `i-court` | 법원용 내보내기 |
| `i-proxy` | 프록시 재생 | `i-dual` | 듀얼 재생 |
| `i-pip` | PiP | `i-theater` | 시어터 |
| `i-skip-back` / `i-skip-fwd` | −10초 / +10초 | `i-step-back` / `i-step-fwd` | −1f / +1f |
| `i-in` / `i-out` | IN / OUT | `i-play` / `i-pause` | ▶ (활성 카드 표식 포함) |
| `i-search` | 검색 | `i-filter` | 필터 |
| `i-folder` | 폴더/찾아보기 | `i-file-image` | E01 이미지 칩 |
| `i-project` | 프로젝트 전환 | `i-activity` | 분석 칩(진행) |
| `i-report` | 결과물 | `i-tools` | 도구 |
| `i-help` | ? 도움 팝오버 | `i-close` | 시트 닫기 |
| `i-undo` / `i-redo` | 실행 취소/다시 | `i-tag` | 태그 |
| `i-save` | 저장 상태 | `i-refresh` | 새 데이터 반영 |
| `i-copy` | ID/경로 복사 | `i-calendar` | 녹화 달력 |
| `i-lock` | 읽기 전용 | `i-spinner` | 진행(회전은 CSS) |

### 5.6 마크 의미와 보고서 범례

- 뷰어 `.mark-chip`(T5): `<span class="mark-chip reviewed|important|needs_verification"><svg class="ic"><use href="#i-check|#i-flag|#i-question"/></svg>라벨</span>` — 모양+색. 색은 §5.1 `--mark-*` 토큰(T1). 그리드 카드·상세 배지·선택 바 버튼 `btnMarkReviewed/Important/Verify`의 아이콘도 동일.
- 패키지 보고서 `review/evidence-viewer.html`은 같은 번들/토큰이므로 자동으로 따라간다(T5가 `html_report.rs` 동등성 테스트로 `--mark-important-bg` 값이 들어갔는지 확인).
- `reports/case-report.html`(`src/report.rs`)에는 **색 범례가 없다**(마크는 텍스트 라벨만: src/report.rs:447-451 `markLabel`). 따라서 Rust 변경 없음. 범례 문장 추가 여부는 §10 Q3. 카빙 보고서(`render_carve_report_html`)는 마크를 쓰지 않는다.
- 빨강(`--danger`/`--failed-*`/`--deleted-candidate-*`)은 검증 실패·삭제 후보·`.destructive`에만 쓴다. `verify-buttons`가 `.mark-chip.important`의 배경이 `--failed-bg`와 다른지 확인.

### 5.7 z-index 층 (양 번들 공통 표)

| 층 | z | 요소 |
|---|---|---|
| 그리드 내부 | 1–3 | 카드 체크박스·그룹 헤더·스플리터 |
| 뷰어 상단 바 | 35 | `.ft-viewer .topbar` |
| 뷰어 메뉴/필터 패널 | 40 | `.menu-list`, `.filters-extra` |
| 뷰어 모달(단축키·클립·팔레트) | 50 | `#shortcutsModal`, `.modal.dlg` |
| live 칩(standalone 전용: 헤더 인라인, embed는 페이저 행 인라인 — 층 없음) | — | `#ft-live-chip` |
| 토스트 | 60 | `#toastHost`(단일) |
| 상단 띠 | 65 | `#hTopStrip` |
| 셸 팝오버·뷰어 고정 메뉴 | 70 | `.h-pop`, `.menu-fixed` |
| 시트 | 75 | `.h-sheet` |
| 셸 대화상자 | 80 | `.h-modal-backdrop` |
| 로더 오버레이(루트 내부) | 90 | `#ft-load-note` (position:absolute inset:0 — standalone에서는 루트가 100vh이므로 동일 효과) |
| skiplink | 100 | `.h-skiplink` |
| 종료 화면 | 110 | `#quitPage` |
| standalone 전체화면 닫기 | 3000 | `#btnExitFs` (호스트 모드 미사용) |

---

## 6. 레이아웃 (T2 호스트 / T5 뷰어 내부)

### 6.1 영역과 px

```
┌ #hTopStrip  44px ─────────────────────────────────────────────────────────────┐
│ [FT][프로젝트명 ▾ hProjectBtn][● 분석 1,240/4,000 ▸ jobPanel] … [새 창][결과물][도구][◐][언어][⏻]│
├ #main (100vh−44) ─────────────────────────────────────────────────────────────┤
│  (빈 상태) #inputCard 760px 중앙 열 · 2블록            |  (프로젝트) #hViewerHost      │
│                                                       │   .ft-viewer.hosted          │
│                                                       │   .topbar 40 │ .workarea │ .selection-bar 44 │
└───────────────────────────────────────────────────────────────────────────────┘
```

| 영역 | 1366×768 | 1920×1080 | 규칙 |
|---|---|---|---|
| `#hTopStrip` | 44px, padding 0 12px, gap 8 | 동일 | sticky top 0; 항목은 `flex:none`, `hProjectBtn` max-width 280/360 ellipsis; `jobPanel` `flex:0 1 420px`/`560px`; 1366에서 라벨 생략 우선순위: 언어 라벨→"새 창" 라벨→"결과물" 라벨(아이콘만, `aria-label` 유지). 1차 버튼 없음 |
| `#main` | 724px | 1036px | `display:grid; grid-template-rows: minmax(0,1fr)` |
| 빈 상태 `#inputCard` | 열 760px 중앙, 블록 간 12px, 수직 중앙(`min-height:100%`, `padding 20px 0`) | 동일 | 블록 2개 `#blkEvidence`(입력 40px·`btnStart` primary lg), `#blkRecent`(목록 `max-height: max(110px, 100vh − 420px)`, 상단 행 `[이어서 검토]` primary lg 36px) |
| 뷰어 `.topbar` | 40px | 40px | 불변 |
| 뷰어 `.workarea` | 724−40−44 = **640px** | 1036−40−44 = **952px** | `grid-template-columns: var(--rail-w) minmax(0,1fr)` |
| 좌 레일 `#leftRail` | **240px** | **280px** (≥1600) | 기본 열림(`--rail-w` 토큰), Ctrl+B 접힘 → 0; ≥1000/＜1000 두 폭별로 열림 상태 기억(`ft.viewer.rail.v2`) |
| 중앙 그리드 열 | 1366−240−480−6 = **640px** (≥600) | 1920−280−640−6 = **994px** (≥960) | `vsplitter` 6px; 드래그 범위 유지(`aria-valuemin 28`…) — 기본값은 px 토큰에서 계산 |
| 우 미디어 열 `section.media` | **480px** | **640px** (≥1600) | `--media-w` |
| 영상 스테이지 `#mediaStage` | **270px**(16:9) | **360px** | 최소값이 아니라 미디어 열 폭의 16:9(`aspect-ratio`) — 더 키우면 레터박스만 늘어난다. `hsplitter` 드래그(120…100vh−320, `#mediaStage` 인라인 height, 저장 키 `layout.stageH`), 더블클릭/Enter 기본값 |
| 툴바 `#mediaToolbar` | 1행 44px(transport: −10초/+10초·프레임 이동은 아이콘 버튼 — 라벨은 접근 이름, 키는 툴팁) + `tbMore` 팝오버 | 1행 44px (미디어 열 <1000px이면 팝오버 — 640px에서도 팝오버) | 현행 로직 |
| 상세 탭 `.mini-info` | 640−270−44−6 = **320px** (탭 32 + 288 스크롤) | 952−360−88−6 = **498px** | 스크롤 영역 |
| `#selectionBar` | 44px 전폭(36px primary + 3px 패딩 + 1px 경계, ≤52), **1행**, sticky bottom | 동일 | `btnApplyMarks` = 영역 유일 primary. **판독 블록을 우측 열로 옮기지 않는다**(§10 Q1). 1366에서 1행 유지: `#reviewProgress`는 <1600px에서 숨김(페이저 `#triageStatus`가 같은 수치), 호스트에서는 `#examinerName` 숨김(검토자는 결과물 시트 `#finalExaminer` → `FTViewer.setExaminer`; 마운트 시 셸 prefs의 이름으로 시드) — DOM과 동작은 유지 |
| 시트 `.h-sheet` | 폭 **480px**, top 44, height 724, right 0 | 폭 **560px**, height 1036 | §6.3 |
| 팝오버 `.h-pop` | min 240 / max min(380, 100vw−32) | 동일 | opener 아래 6px, 우측 정렬 |
| 대화상자 `.h-modal` | min(560px, 92vw), max-height 80vh | 동일 | |

접힘 규칙: `<1366` 폭에서는 상단 띠 라벨 전부 아이콘화; `<1180`에서 `hProjectBtn` 이름을 20자 ellipsis; `<1000` 뷰어는 레일 자동 접힘(`rail-collapsed`) — 1366 미만은 지원 범위 밖(스크롤 없이 깨지지 않으면 됨).

### 6.2 상단 띠 아래 배너

`#serverBanner`(서버 연결 끊김)는 `#hTopStrip` 바로 아래 sticky(top 44, z 39, 36px). 배너가 보이면 `#main`은 `calc(100vh − 44px − 36px)`(그리드 리플로우 1회 허용 — 오류 상황). 팝아웃 중 `#hPopoutNote`는 미디어 열 자리(`section.media` 대신 표시, 폭 동일)에 들어가므로 레이아웃 시프트 없음.

### 6.3 시트 동작

- 위치 `position:fixed; top:44px; right:0; bottom:0; width:var(--sheet-w)`; `--shadow-sheet`; 열려도 **그리드는 리플로우되지 않는다**(overlay). 그리드 오른쪽 480px가 가려지는 대신 토스트·live 칩이 왼쪽으로 비킨다(§6.5).
- `#hJobsSheet`(작업): **비모달**. `role="complementary" aria-labelledby="hJobsTitle"`. 열면 `hJobsClose`로 포커스, Tab은 자연 순서(트랩 없음), Esc(포커스가 시트 안일 때)로 닫고 opener(`hBtnJobs`)로 포커스 복귀. 바깥 클릭으로 닫히지 않는다. 분석 중에는 자동으로 열리지 않는다(칩만 갱신; 최초 분석 시작 직후 1회만 자동 열림 → `FTStore.panels.jobs=true`, 사용자가 닫으면 그 세션에서 다시 자동 열지 않음).
- `#hFinalizeSheet`(결과물): **모달**. `role="dialog" aria-modal="true"`, 열면 `#main`·`#hTopStrip`에 `inert`, 포커스 트랩(첫 포커스 = `finalExaminer`), Esc 닫기(진행 중 `aria-busy` 상태면 Esc 무시), 닫힘 시 opener `hBtnFinalize`로 복귀. 뒤 배경 어둡게 하지 않음(종이 위 시트).
- 두 시트는 동시에 열리지 않는다(`panels.jobs`와 `panels.finalize` 상호 배타; 열려 있던 쪽은 닫힘).
- 시트 머리: 제목(16px 600) + `quiet icon` 닫기 버튼(`i-close`, 28px). 본문 스크롤 `overflow:auto`. 바닥 고정 행(결과물: `btnFinalize` primary lg 우측 정렬; 작업: `btnCancel` destructive · `btnNew` secondary).

### 6.4 빈 상태 (프로젝트 없음)

`#inputCard`만 보임(`#hViewerHost[hidden]`), 상단 띠는 `hProjectBtn`="프로젝트 없음"(비활성), `jobPanel[hidden]`, `hBtnFinalize`/`hBtnPopout` disabled(사유 title). 블록 2개 순서 고정: `#blkEvidence`(제목 "증거 넣기", `.sbdesc` 1줄, 입력 행 `sourcePath`+`btnBrowseSource`+`btnStart`, `srcline`, `preflight`, `resumeBox`, `startErr`, `details#startOptions` 접힘) → `#blkRecent`(제목 "이전 프로젝트", `.sbdesc` 1줄, `#recentCaseRow` 최신순 ≤8, 상단 행 버튼 라벨 **"이어서 검토"** `primary lg`(열 수 있는 첫 행; 분석 중단 상태면 "이어서" 라벨로 분석 재개), 나머지 행 `quiet` "열기", 사라진 폴더 행 `quiet` "목록에서 지우기", `#btnOpenOther`는 목록이 비면 `primary` 아니면 secondary). 바닥 `.h-startfoot`: "로컬 전용 · 원본 읽기 전용" + `hBtnHelp`(quiet). 분석 시작 즉시(`phase=running`) 호스트로 전환하고(`inputCard.hidden=true`) 뷰어는 `review-partial` 진입 시 마운트(`/api/records?live=1`); 그 사이(`running`, 색인 전)는 `#hViewerHost` 안에 `.h-host-wait`(한 문장 + 작업 시트 열기 버튼) 표시.

### 6.5 토스트 (절대 컨트롤을 덮지 않음)

- **단일 호스트 규칙**: 뷰어가 마운트되면 `#toastHost`(뷰어)만 쓰고 셸 `toast()`는 `FTViewer.toast()`로 위임; 뷰어가 없으면(빈 상태·레거시) 셸 `#hToastBox`를 쓴다. 두 호스트가 동시에 보이지 않는다.
- 위치(`position:fixed`): 빈 상태 `top: 52px; right: 16px`; 호스트 모드 `top: 44+40+8 = 92px; right: 16px`; 시트 열림 시 `right: calc(var(--sheet-w) + 16px)`(`html[data-sheet]` 속성으로 CSS 전환); 폭 `min(400px, 100vw − 32px)`; 최대 3개; `pointer-events:none`(버튼만 auto); 6 s, 오류는 수동 닫기; Esc는 최신 토스트.
- standalone 뷰어: `top: 100px`(헤더 36+탭 40+8)·우측 14px — 현행과 동일.
- `#ft-live-chip`: **standalone 전용**. 헤더가 보이면 헤더 안 인라인, embed(레거시 iframe)면 그리드 아래 페이저 행 인라인 — 어떤 컨트롤도 덮지 않는다. 호스트에서는 만들지 않는다: 색인·보강 상태는 상단 띠 칩(`#jobPanel`), "새 데이터 반영"은 작업 시트 `btnJobReload`(§3.2 `live-state`).

### 6.6 프로젝트 팝오버 `#hProjectPop` (T2)

항목: 현재 프로젝트(이름·경로 mono·phase 배지) · "이전 프로젝트" 목록(≤8, `/api/recent-cases`) · `hProjectOpenOther`("다른 케이스 폴더 열기…" → 피커) · `hProjectNew`("새 증거 넣기" → 미저장 확인 후 빈 상태로) · `hLnkDashboard`. 전환 절차: `FTViewer.saveState()`가 saved/null이 아니면 `askDialog`(저장 다시 시도/취소) → `FTViewer.destroy()` → `POST /api/open-case` → `FTStore.set(project)` → 폴링 1회 → `FTViewer.init()`. 페이지 리로드 없음.

### 6.7 보조 모니터 (T2 23-popout.js)

`hBtnPopout` 또는 뷰어 `request-popout` → `FTViewer.popout()`(기존 팝업 플레이어: 영상·전송·마크 1/2/3/0·태그·j/k·실행취소, `popup=yes,width=1100,height=880`). 본 창: `.ft-viewer.popped` → `section.media`와 `#hsplitter` 숨김, `#hPopoutNote`(같은 자리·같은 폭: "보조 창에서 영상 검토 중" + `hBtnPopoutFocus` secondary + `hBtnPopoutReturn` primary) 표시, 그리드·레일·선택 바 유지. 저장은 본 창만(팝업은 `ft-player:<case>`로 위임). 팝업 닫힘 감지(1 s 폴링, 현행) → `popout:false` → 미디어 열 복원. `ft-review` 채널로 다른 셸 탭에 `popout-open`을 알리고, 다른 탭은 자신의 팝아웃 버튼을 disabled("다른 창에서 열림")로 바꾼다.

---

## 7. 키보드 맵 (최종)

| 키 | 동작 | 처리 | 비고 |
|---|---|---|---|
| `j`/`k`, `n`/`N`, `x`, `Enter`, `Space`, `1`/`2`/`3`/`0`, `←`/`→`, `[`/`]`, `,`/`.`, `Shift+J/K/L`, `i`/`o`, `f`, `t`, `p`, `?`, `Esc`(모드 종료) | 뷰어 불변 | 뷰어 `document` keydown (80-review.js:474, 70-player.js:377 capture) | **전부 보존** |
| `Ctrl+Z`/`Ctrl+Y`(`Ctrl+Shift+Z`), `Ctrl+A`, `Ctrl+D`, `Ctrl+B`, `Ctrl+K` | 뷰어 불변 | 동일 | |
| `Alt+1`…`Alt+4` | **폐기** | 97-shell.js `stageShortcut` 삭제 | 단계가 없음. 문서(단축키 모달)에 표기 없음 |
| `Alt+P` | 프로젝트 팝오버 토글 | 셸 97-shell.js, `event.code === "KeyP"` + altKey, AltGraph 제외, 텍스트 필드 제외 | 브라우저 예약 아님(Chrome/Edge) |
| `Alt+J` | 작업 시트 토글 | 동일 | |
| `Alt+R` | 결과물 시트 토글(프로젝트 없으면 토스트) | 동일 | |
| `Esc` | 순서: ① 셸 대화상자(자체) ② 뷰어 열린 메뉴(window capture, 80-review.js:1077) ③ 뷰어 모달/팔레트 ④ 결과물 시트 ⑤ 작업 시트 ⑥ 셸 팝오버 ⑦ 최신 토스트 ⑧ 뷰어 모드(시어터/전체화면) | 셸은 `document` bubble 단계에서 `e.defaultPrevented`면 중단 | 각 단계는 처리 시 `preventDefault()`. **한 번 누르면 한 층만**(U13): 뷰어 전역 핸들러도 필터 컨트롤 blur → 단축키 시트 → 최신 토스트 1개 → 시어터 중 하나만 처리 |
| `Tab` | 순서: skiplink → 상단 띠 → (빈 상태 열 | 뷰어 topbar → 레일 → 그리드 → 미디어 → 선택 바) → 열린 시트 | 시트(결과물)는 트랩 | |
| `F6` | 영역 순환: 상단 띠 → 레일 → 그리드 → 미디어 → 선택 바 → (시트) | 셸 97-shell.js(신규, 선택) | 미구현 시 생략 가능 — 계약상 "권장" |

충돌 규칙:
0. **IME·CapsLock (U6)**: 뷰어 전역 핸들러(80-review.js)와 팝아웃 플레이어는 `shortcutKey(event)`로 키를 읽는다. `event.key`가 **비라틴 문자**(유니코드 Script = Hangul·Cyrillic·Greek·Armenian·Georgian·Hebrew·Arabic·Thai·Devanagari·Bengali·Tamil·Han·Hiragana·Katakana·Bopomofo 한 글자)이거나 `"Process"`·`"Unidentified"`일 때만 `event.code`(`KeyJ`→`j`, `Digit1`→`1`, `Comma`/`Period`/`BracketLeft`/`BracketRight`, Shift+`Slash`→`?`)로 정한다. 그 밖(라틴 문자 — 악센트 포함 `é à ü ě š ı å`, AZERTY `&` 등 기호)은 `event.key` 그대로라 Digit2 위의 `é`는 `2`가 아니고 아무 동작도 하지 않는다. 글자의 대소문자는 **Shift만** 따른다(CapsLock+j = j, Shift+j = J 셔틀) — 예전 70-player.js의 capture 정규화 리스너는 이것으로 대체됐다. Ctrl+Z/Y/A/D·Ctrl+B(40-grid, 레일)·Ctrl+K(명령 팔레트)도 같은 함수로 읽는다(러시아어 배열 Ctrl+`л` = Ctrl+K). Ctrl+B도 뷰어 모달이 열려 있으면 동작하지 않는다. 입력칸 포커스·AltGr·Alt 조합 무시 규칙은 그대로다. 뷰어 모달(`.modal-backdrop:not([hidden])`)이 열려 있으면 Esc 외의 뷰어 단축키는 동작하지 않는다. 하네스: verify-player-keys(`key:"ㅓ"`/`"Process"`/`"о"` + `code:"KeyJ"` 등, 음성 검사 `{key:"é",code:"Digit2"}`·`{key:"&",code:"Digit1"}` 무동작, Ctrl+`л`/`и`).
0. 셸 `hDialogOpen()`은 셸 대화상자·경로 선택기 외에 **뷰어 모달**(`#ftViewer .modal-backdrop:not([hidden])`: 단축키 시트, 뷰어 대화상자, 명령 팔레트)도 포함한다(U12) — 그 위에서 Alt+P/J/R·Esc는 셸이 처리하지 않는다(verify-layout 9번).
1. 뷰어 전역 핸들러는 `isTypingTarget(activeElement)` 외에 **`activeElement.closest('[data-keys="off"]')`가 있으면 return**(T5가 80-review.js:474·70-player.js:377·40-grid.js:983 세 곳에 추가). `data-keys="off"`는 `#inputCard`, `.h-sheet`, `.h-pop`, `.h-modal-backdrop`, `#hPopoutNote`에 T2가 붙인다. 상단 띠 버튼은 keys-off가 아니다(시트를 닫고 opener로 돌아와도 `j/k`가 즉시 동작).
2. 셸 핸들러는 Alt+문자·Esc만 가진다. 뷰어는 Alt 조합을 무시(80-review.js:490)하므로 이중 발동이 없다.
3. 셸 텍스트 입력(`sourcePath`, `caseDir`, `finalExaminer`, `advPath`, `pickerPath`)은 `isTypingTarget`로 자동 보호된다.
4. 단축키 모달(`#shortcutsModal`)은 뷰어 것 하나만 쓴다; 셸 단축키 3개(Alt+P/J/R)는 T5가 모달 표에 행 3개를 추가한다(i18n 키 `sc.altProject`, `sc.altJobs`, `sc.altFinalize`; 호스트 모드에서만 표시 `.ft-viewer.hosted .sc-host`).

---

## 8. 하네스

### 8.1 기존 하네스 이행 (CI 15개 + 기타)

| 하네스 | 변경 | 트랙 |
|---|---|---|
| `check-a11y` | PAGES → `shell-empty`(`/`, `#inputCard`), `shell-project`(`/`, `#hViewerHost .card` 대기, iframe 없음 → `page.frames()` 루프는 메인 프레임만), `viewer-standalone`(`/viewer/`) + `shell-project-sheets`(결과물 시트 열림, 작업 시트 열림 상태 각 1회 스캔). `min-font` 13. `NAME_BASELINE` 비움(세 컨트롤 라벨 완료는 T5). 글리프 금지 규칙 추가: 버튼 `textContent`에 `[◐☀⏻⤢⧉▾↗✓✕☰⚠⤓▸]` 있으면 ENFORCED 실패 | T2 |
| `verify-start-screen` | a: 블록 2개(`['증거 넣기','이전 프로젝트']`), `.sbdesc` 2; "뷰어 보기" 검사 전부 삭제; `#stageNav/#crumbs/#onboard` hidden 검사 → **요소 부재** 검사; `#btnOnboardShow`→`#hBtnHelp`; b: `#resultCard.on`→`#hViewerHost:not([hidden]) #recordGrid .card`; c: `#s1` 클릭 → `#hProjectBtn`→`hProjectNew`로 빈 상태 복귀(또는 새 탭에서 `/`); "뷰어 보기 opens" → 상단 행 `[이어서 검토]` 클릭 후 `#hViewerHost` 표시; 레이아웃 g: `primaries = [btnStart, 첫 행 primary]`, blocks 2, `#inputCard` 컨트롤 ≥24 | T2 |
| `verify-shell-jobs` | VM 환경 재작성: 번들은 `appBundleJs()` 그대로, `FTViewer` 목(`init/destroy/reload/saveState/save/on/off/summary/toast`) 주입, `reviewFrame` 가짜 제거; 프레임 6개 테스트 삭제 → `reload` 거부/승인 테스트로 대체; `btnTheme`→`hBtnTheme`, `btnViewerLang`→`hBtnLang`, `recoverHint`→`jobsNote`, `crumbs` 테스트 → `hProjectBtn` 텍스트; 잡 칩/시트 상태는 `#jobPanel` 클래스와 `#jobTitle/#jobDetail/#jobElapsed` 그대로 | T3 |
| `verify-jobs-e2e` | `frameOf()`/`frames()` → `page`(같은 문서); `#reviewFrame src` 비교 → `FTViewer` `records` 이벤트 generation 비교; `#btnCaseMenu` → `#hBtnJobs`(시트 열기) 후 `#btnCarve`; `.view[data-view="3"].on` → `#hViewerHost:not([hidden])`; 나머지 id 불변 | T3 |
| `verify-live-index` | **무수정**(`/viewer/`·`#ft-live-chip`·`#recordGrid`) — T3가 통과 확인 | T3 |
| `verify-viewer-undo`, `verify-player-keys`, `verify-range-sync`, `verify-virtual-grid`, `verify-facets`, `verify-player-popup-escape`, `verify-viewer-dialogs` | standalone 페이지 조립(`app-bundle.mjs viewerBundleCss` + `assets/evidence_viewer.html`)을 쓰는 것들은 T5가 `viewerPageHtml()`(조각 포함, build.rs 미러) 헬퍼를 `app-bundle.mjs`에 추가하고 호출만 바꾼다. `document.body.classList`를 검사하는 곳(`verify-virtual-grid`의 `vgrid`, `verify-player-keys`) → `#ftViewer.classList` | T5 |
| `verify-i18n` | `assets/viewer/viewer.body.html`도 읽는다(evidence_viewer.html + 조각) | T5 |
| `verify-review-batch --unit` | 단위 모드만 CI; CDP 경로 미사용 → 변경 없음(확인 T5) | T5 |
| `bench-viewer` | `/viewer/` 그대로 | — |
| `check-css` | 파일 목록 `["assets/evidence_viewer.css","assets/app/00-layout.css"]` + `kit.css`·`30-jobs.css`·`40-finalize.css` 괄호 균형; `:root` 시작 규칙은 `00-layout.css`·`evidence_viewer.css`에만; 대비 쌍 §5.2; `--fs-xs: 13px` | T1 |
| `check-js` | id 검사: 셸 번들이 참조하는 id ∈ examiner_app.html ∪ viewer.body.html(조각) ∪ JS 문자열; **셸 번들에서 뷰어 id 직접 참조 금지 목록**(§2.3 전체)은 `verify-embed`에서; 두 번들 최상위 바인딩 교집합 검사 | T5 |
| `capture-screens` | §9.4 | T2 |
| `verify-stage3-embed`, `verify-fullscreen`, `shot-stages` | 삭제 | T5 |
| `user-walkthrough`, `real-user-test`, `audit-buttons` | 삭제 | T2 / T2 / T1 |
| Rust `cargo test --locked`(441) | `examiner_page_auto_detects_image_extension`(src/serve/tests.rs:895) 유지 — 80-paths.js 문자열 불변; `html_report.rs` 테스트는 `VIEWER_TEMPLATE` 출처 변경 후에도 동일(조각 포함 결과가 같아야 함); 신규: `viewer_fragment_present_in_slim_and_full_pages`(`id="ftViewer"`, `id="recordGrid"`, `data-ft-viewer="2"`), `legacy_examiner_page_served_when_env_set` | T5 |
| `workstation_e2e` | 페이지 구조 비의존 — 변경 없음 | — |

### 8.2 신규 하네스 (각 트랙이 `ci-harnesses.sh`의 자기 앵커를 교체)

| 하네스 | 방식 | 단언 |
|---|---|---|
| `verify-buttons.mjs` (T1) | Playwright + isolated server + demo case; 페이지 3종(빈 상태·프로젝트·standalone) × 2테마 | ① 모든 `button`의 클래스 ⊂ {`primary`,`quiet`,`destructive`,`lg`,`icon`,`toggle`} ∪ 컴포넌트 클래스 허용 목록(`chip pill navcell cal-cell tag-del save-state mi`은 금지 → 실패) ② 높이 ≥24, `.primary`≥36, `.icon` 28 ③ `[data-region]`마다 보이는 `.primary` ≤1(`selectionBar`·시트는 =1) ④ 5상태 computed style 상이(hover/active는 `page.hover`/`mousedown`, focus-visible은 Tab, disabled는 속성) ⑤ 각 상태 글자/배경 ≥4.5(비활성 ≥3) ⑥ 텍스트에 글리프 없음, 아이콘 버튼은 `aria-label` ⑦ `kit.css`가 양 번들에 1회씩만 포함(배너 count) ⑧ 1366×768·1920×1080에서 상단 띠·선택 바·시트 바닥의 버튼이 뷰포트 안이고 다른 요소에 덮이지 않음(`elementFromPoint` 중심점 = 자기 자신) |
| `verify-layout.mjs` (T2) | Playwright, 4조합 | ① 영역 rect: `#hTopStrip` h=44, `#leftRail` w=240/280, `section.media` w=480/640, `#mediaStage` h=270/360, `#selectionBar` h≤52, 그리드 열 ≥600/960 ② 가로 스크롤 없음 ③ 빈 상태: 블록 2, `[이어서 검토]` primary 36px, 열 ≤760 중앙 ④ 시트 열기 전후 `#recordGrid` rect 동일(리플로우 없음), 시트 폭 480/560, 포커스가 시트 안, Esc 닫힘 + opener 포커스, 결과물 시트는 `#main[inert]` ⑤ 토스트 rect가 상단 띠·topbar·선택 바·시트와 교차하지 않음(3개 띄운 상태) ⑥ 팝아웃: `window.open` 스텁 → `.ft-viewer.popped`, `#hPopoutNote` 표시, 그리드 폭 증가, `ft-review`에 `popout-open` 수신(두 번째 탭) ⑦ 프로젝트 전환: 두 케이스 사이 전환 시 `location.reload` 없음(`page.on('load')` 0회), `#recordGrid .card` 수가 바뀜 ⑧ Alt+P/J/R 토글, Alt+1 무동작 |
| `verify-progress.mjs` (T3) | VM(verify-shell-jobs 환경 재사용) | ① `/api/status` 시퀀스(running→review-partial busy→review-ready)에 따라 `#jobPanel` 클래스·텍스트("색인 n/m · 단계 2/5 · 경과 m:ss") ② 칩 클릭 → `panels.jobs` true, 시트에 `#track li` 5개·`#log` 누적(append-only 유지) ③ `btnCancel` → 확인 → `/api/cancel` 1회 ④ 복구/카빙/클립 잡이 같은 칩에 합류(우선순위 pipeline > caseJob > clip) ⑤ 완료 시 `FTViewer.reload()` 호출 1회, 거부되면 `deferred` 상태 + `btnJobReload` ⑥ 폴링 간격 1 s(busy)/5 s(idle) |
| `verify-finalize-sheet.mjs` (T4) | VM | ① 체크리스트 행 생성(unsavedMarksChecks/gatherFinalizeChecks 순수 함수 유지) ② block 행 있으면 `btnFinalize.disabled` + `title` 사유 ③ `finalExaminer` 비면 disabled ④ `/api/finalize` POST 정확히 1회, body `{examiner}` ⑤ `#finalAdvanced` 기본 접힘, 안에 `data-adv` 8개·`btnImportMarks`·`btnVerifySources` ⑥ 완료 후 `#finalLinks`에 보고서 링크(`target=_blank`)와 패키지 경로, `reportFrame` 없음 ⑦ 결과물 시트 열림 중 `#main.inert` ⑧ 패키지 뷰어 동등성: `render_evidence_viewer_html` 출력에 `--mark-important-bg` 값이 §5.1과 같음(Rust 테스트로 위임, 하네스는 `/review/evidence-viewer.html` 응답 문자열 검사) |
| `verify-embed.mjs` (T5) | Playwright + 정적 검사 | ① `GET /`에 §2.3 뷰어 id가 **정확히 1개씩**(`querySelectorAll('#'+id).length===1`), `#ftViewer[data-ft-viewer="2"]` ② `/review/evidence-viewer.html`·`/viewer/`·`GET /`에서 `<!-- @include:viewer.body -->` 치환 결과(루트 outerHTML을 데이터 적재 전 기준으로 정규화)가 바이트 동일 ③ 셸 번들 소스에 뷰어 id 문자열 0건(`$("saveState")` 등), `postToViewer`/`contentWindow` 0건 ④ 단축키: `#sourcePath`·`#finalExaminer`·시트 안 버튼 포커스에서 `j/k/1/Ctrl+Z` 무동작(활성 카드·마크 불변); 상단 띠 버튼 포커스에서 `j` 동작 ⑤ 두 번들 최상위 바인딩 교집합 0 ⑥ computed style 스팟 체크 6개: `.ft-viewer .card`·`.topbar`·`#selectionBar button.primary`·`.menu-list`·`.mark-chip.important`·`#toastHost .toast`가 standalone과 호스트에서 동일(색·높이·radius) ⑦ `FTViewer.destroy()` 후 `document` keydown으로 `j`를 보내도 활성 카드 불변, `init()` 재마운트 성공 ⑧ 레거시: `FRAMETRACE_UI=legacy`로 띄운 서버의 `GET /`에 `id="stageNav"`와 `<iframe id="reviewFrame"`, 그 iframe이 `/review/evidence-viewer.html?embed=1`을 로드해 `.ft-viewer.embed`가 됨 |

`ci-harnesses.sh` 최종 순서(앵커 교체 후): 기존 15개 뒤에 `verify-buttons`, `verify-embed`, `verify-layout`, `verify-progress`, `verify-finalize-sheet`.

---

## 9. 머지 순서 · 통합 체크리스트 · 롤백 · 스크린샷

### 9.1 머지 순서

`T1(tokens/kit/icons)` → `T5(fragment/FTViewer/build.rs/legacy)` → `T2(host/store/empty/popout)` → `T3(progress/jobs)` → `T4(finalize)`.
각 트랙은 Phase 0 커밋에서 분기하고, 머지 전 최신 `main`에 **rebase**한다. T2는 T5의 `<!-- @include:viewer.body -->`·`FTViewer`를 전제로 개발하되 머지 전까지는 자기 worktree에서 `assets/viewer/00-host.js` 스텁(§3 시그니처, no-op)을 두고 **커밋하지 않는다**. T3/T4는 T2의 마운트 id(§2.2)와 `FTStore`(§4)를 전제로 하며, 자기 worktree에서는 `01-store.js` 최소 구현을 `scripts/lib/`가 아닌 테스트 전용 목으로 둔다(커밋 금지). 트랙 내 CI: `cargo build --locked && cargo test --locked`, `node scripts/check-js.mjs`, `node scripts/check-css.mjs`, 자기 하네스.

### 9.2 통합 체크리스트 (Phase 2, 1명)

1. `git log` 5개 머지가 §9.1 순서. `assets/app/app.css`, `60-review.js`, `65-jobs.js`, `70-finalize.js`, `75-advanced.js`, 삭제 하네스 5개가 트리에 없음.
2. `cargo build --locked && cargo test --locked` (441 + 신규 ≥2) · `cargo test --locked --test workstation_e2e -- --ignored`.
3. `node scripts/check-js.mjs`, `node scripts/check-css.mjs`.
4. `bash scripts/ci-harnesses.sh` → 20개 전부 ok(앵커 주석 0개 잔존: `grep -c '# @T' scripts/ci-harnesses.sh` = 0).
5. `node scripts/check-a11y.mjs` 0 FAIL(양 테마·2해상도·4페이지).
6. `node scripts/capture-screens.mjs` → `docs/screens/current/INDEX.md` 갱신, 빈 PNG 없음, page error 0.
7. 수동: §7 키 표 전부(standalone·호스트), 이전 케이스 폴더 열기, 팝아웃 열고 마크 → 본 창 `saveState` saved, `FRAMETRACE_UI=legacy` 기동 → 옛 화면 + iframe 동작.
8. README/`docs/SCREENSHOTS.md`/`CHANGELOG.md` "Unreleased"에 단축키 변경(Alt+1-4 폐기, Alt+P/J/R)과 레거시 스위치 기록.

### 9.3 롤백 스위치 `FRAMETRACE_UI=legacy` (T5, 1릴리스 유지 후 삭제)

- 스냅샷: `assets/legacy/examiner_app.html`, `assets/legacy/tokens.css`, `assets/legacy/app/*.css|*.js` = `git show 465e7db:assets/…` 그대로(수정 금지; T1의 새 토큰과 무관하게 옛 팔레트 유지).
- `build.rs`: `legacy_bundle(out_dir)` = 현 `app_bundle`와 같은 로직을 `assets/legacy/`에 적용 → `OUT_DIR/examiner_app_legacy.html`. `rerun-if-changed` 등록.
- `src/serve/mod.rs`: `const EXAMINER_PAGE_LEGACY: &str = include_str!(concat!(env!("OUT_DIR"), "/examiner_app_legacy.html"));` `fn legacy_ui() -> bool { static V: OnceLock<bool>; std::env::var("FRAMETRACE_UI").as_deref() == Ok("legacy") }` `("GET", "/") => page(if legacy_ui() { EXAMINER_PAGE_LEGACY } else { EXAMINER_PAGE }.as_bytes().to_vec())`. 라우트 표 불변 → `tests_http.rs` 불변.
- 레거시 페이지의 iframe은 `/review/evidence-viewer.html?embed=1`·`/viewer/?embed=1`을 로드한다 → 새 뷰어는 standalone 모드에서 `?embed=1`과 `ft-theme/ft-locale/ft-reload/ft-embed` postMessage, `btnExitFs`, `__frametraceSummary`, `#saveState` DOM을 **유지**한다(§3.1). 레거시 삭제 시 이들도 함께 제거.
- `scripts/lib/isolated-server.mjs`는 `startIsolatedServer({ env: { FRAMETRACE_UI: "legacy" } })` 옵션을 받는다(T5; `verify-embed` ⑧에서 사용).
- 문서: `docs/WINDOWS_USAGE.md`에 한 줄(T5). 삭제 시점: 다음 릴리스 태그 후 첫 커밋에서 `assets/legacy/`·분기·`?embed=1` 경로 제거.

### 9.4 스크린샷 캡처 변경 (`scripts/capture-screens.mjs`, T2)

단계 내비·iframe 의존 전부 제거. 새 샷 목록(파일명 고정, 4조합 유지):
`00-empty`(빈 상태) · `01-empty-options`(옵션 펼침) · `01b-empty-filled`(경로 입력+preflight) · `02-running`(분석 시작 직후: 칩 running + 작업 시트 자동 열림) · `02b-running-chip`(시트 닫고 칩만) · `03-project`(호스트 뷰어, 그리드 가시 행 수 메모) · `03a-project-menu`(프로젝트 팝오버) · `03b-jobs-sheet`(복구 실패 상태의 작업 시트) · `04-viewer-grid` · `05-viewer-detail` · `05b-viewer-inspector` · `05c-shortcuts` · `05d-clip-dialog` · `06-viewer-multiselect` · `06b-toast-actionbar`(standalone 토스트) · `06c-toast-hosted`(호스트에서 토스트 3개 + 시트 열림) · `07-finalize-sheet`(체크리스트, 차단 항목 없음) · `07b-finalize-blocked`(미저장 마크로 차단) · `08-finalized`(링크·패키지 경로) · `09-alt-theme` · `10-popout`(팝아웃 중 본 창).
구현 규칙: `page.frames()`/`frameLocator` 사용 금지; 가시 행 계산은 메인 프레임; `#dlgExaminer`→`#finalExaminer`; `#btnTheme`→`#hBtnTheme`; `#s3/#s4` 클릭 → `#hBtnFinalize`; 팝아웃 샷은 `window.open`을 `page.addInitScript`로 스텁해 실제 창 없이 `.popped` 상태만. `docs/SCREENSHOTS.md`·`docs/screens/INDEX.md` 설명 갱신. `visual-diff.mjs`는 이름 매칭이 바뀌므로 "before-ux" 비교는 Phase 3에서 수동 대응표로만 쓴다.

---

## 10. 미결 질문 (3개 — 통합 전 소유자 결정)

1. **판독(선택) 바 위치**: 본 계약은 `#selectionBar`를 **전폭 하단 44px**로 유지한다(뷰어 id·하네스·standalone 동등성 유지, 레이아웃 시프트 0). 승인된 계획의 그림은 우측 열 하단 "판독 블록 88px"을 그렸다. 우측 열로 옮기려면 T5가 `#selectionBar`를 `section.media` 안으로 이동(1366에서 상세 탭 영역 320→232px)해야 하며 `verify-layout`·`capture-screens` 수치가 바뀐다. **기본: 유지.**
2. **`hBtnFinalize`(결과물) 활성 조건**: `review-partial`(보강 미완)에서 시트는 열리되 체크리스트가 "보강 미완료"를 block 행으로 보여 `btnFinalize`를 비활성화한다(현 `s4` disabled 동작의 이식). 대안은 보강 전에는 버튼 자체를 숨김. **기본: 열리되 차단 표시.**
3. **보고서 범례 문장**: `reports/case-report.html`(src/report.rs)에는 색 범례가 없어 마크 색 변경의 영향이 없다. 법원 제출물용으로 "판독 마크 범례(판독완료=회색 체크 · 중요=호박색 깃발 · 검증대기=보라 물음표)" 문장을 보고서 머리말에 **추가할지**. 추가하면 T4가 `src/report.rs` 1곳(184행 note 옆)과 테스트 1개를 소유한다. **기본: 추가하지 않음.**

### 10.1 결정 (2026-10-11, 오케스트레이터 — 소유자 원칙 "필요한 설명과 버튼만 · 버튼 최적 조건" 기준)

1. **판독 바: 전폭 하단 44px 유지(기본안 채택).** 뷰어 id·standalone 동등성·레이아웃 시프트 0을 지킨다. 다중 선택 시에도 같은 바를 쓴다.
2. **결과물: 열리되 차단 표시(기본안 채택).** `review-partial`에서는 체크리스트 첫 행에 "분석 보강 중"을 block으로 보이고 `btnFinalize`는 사유 툴팁과 함께 비활성.
3. **보고서 범례: 추가한다.** T4가 `src/report.rs`(머리말 note 옆)에 "판독 마크 범례: 판독완료 = 회색 체크 · 중요 = 호박색 깃발 · 검증대기 = 보라 물음표" 한 줄과 테스트 1개를 추가한다.


## 11. 3단계 트랙 C 추가 계약 (2026-10-13)

### 11.1 리로드가 보기를 보존 (U4)
- 보존 대상: `query`(입력칸 원문 포함), `kind`, `status`, `chip`, `calMonth`, `sortBy`, `groupBy`, `dateFrom/To`, `recType`, `audio`, `chipsExpanded`, `pageSize`, `currentPage`, `collapsedGroups`, `selectedIds`, `activeId`, `#recordGrid.scrollTop`, Undo/Redo 저널(95-init.js `viewerViewState` / `viewerApplyViewState`).
- 전달: 호스트는 `reload()`가 destroy 전에 `impl.viewState()`를 받아 `init({ restore })`로 넘긴다. standalone(페이지 리로드·레거시 `ft-reload`)은 `sessionStorage["ft.viewer.<case>.reloadView"]`에 한 번 저장하고 다음 부팅이 **항상** 읽고 지운다(호스트 모드 부팅도 지움, 용량 초과면 저널 없이 재시도). 이 사본에는 메모 원문이 없다 — 저널의 메모 필드 변경은 빼고(standalone 리로드 뒤 메모 Ctrl+Z는 없음) 마크·태그·구간 값과 id만 남긴다.
- 걸러내기: 사라진 레코드 id는 선택·활성에서 빠진다. 저널은 맨 위(다음 Ctrl+Z/Y)부터 내려가며 가상 현재값에 재생해, **그 레코드가 여전히 그 항목이 남긴 값**(undo: `after`, redo: `before`; 마크는 상태만, 메모·태그·구간은 정규화 비교)일 때만 남긴다. 다른 창·서버가 바꾼 필드는 그 필드의 더 깊은 항목까지 버린다 — 오래된 Ctrl+Z가 남의 변경을 덮지 않는다. 부팅 뒤 죽은 창의 드래프트를 넘겨받으면(`adoptDeadWriters`) 그 id의 저널 항목은 버린다. 세션 안에서도 Undo/Redo는 필드마다 현재값이 항목이 남긴 값(undo: `after`, redo: `before`)과 같을 때만 되돌리고, 다르면(다른 곳에서 바뀜) 그 필드를 건드리지 않고 항목에서 빼며 토스트 "다른 곳에서 바뀐 항목은 되돌리지 않았습니다"(redo: "…다시 적용하지 않았습니다")를 보인다.
- 저장 상태와 무관: 리로드는 미저장이 없을 때만 일어나고(`viewerReloadBlocker`), 드래프트·dirty·충돌은 평소 부팅처럼 저장소에서 수화된다. 보존 코드는 `state.drafts`·`dirtyIds`·`conflictIds`를 건드리지 않으며, 되살린 저널의 Undo/Redo는 일반 경로(`touchAnnotation` → 드래프트 → 저장)를 탄다. 하네스: verify-viewer-undo(standalone), verify-virtual-grid(스크롤·검색), verify-layout 10번(호스트).

### 11.2 드래프트 키·생존 확인·저장 대기 (save protocol v2 정리)
- 드래프트는 **레코드 id마다 한 키**: `ft.draft.<case>.<writer>.<id>` = `{ mark, note, tags, examiner, range, seq, base, conflict? }`(30-records.js). 창(writer = `FTViewer.writerToken`)은 자기 키만 쓴다. 쓰기는 250 ms 디바운스 1회 flush. 예전 단일 키 `ft.viewer.<case>.annotations.v1`은 부팅 때 한 번 이관(base 0 → 서버 값과 다르면 충돌로 표시).
- 생존: 각 창이 5 s마다 `ft.alive:<case>:<writer>`에 시각을 쓰고, 정상 종료(pagehide)는 `"closed"`. 닫힌 창의 드래프트는 즉시, 5분 넘게 멈춘 창의 드래프트는 `ft-alive:<case>` ping(1.5 s)에 응답이 없을 때만 넘겨받는다(§3.3). **정정(R4 A11)**: 예전 문구의 "얼어붙은 탭은 드래프트를 지킨다"는 틀렸다 — 타이머만 느려진 백그라운드 탭은 ping에 답해 드래프트를 지키지만, 브라우저가 **얼린(frozen·discarded) 탭은 스크립트가 돌지 않아 답하지 못하고 그 드래프트는 넘겨받힌다**. 그래도 덮어쓰기는 없다: 넘겨받은 드래프트는 **그 드래프트의 base 리비전**으로 전송되고, 얼었던 탭이 깨어나 같은 값을 자기 base로 다시 보내면 둘 중 나중 것은 바뀐 리비전을 만나 **충돌**이 된다(서버 `base_revisions` 검사, 같은 client_id 예외는 같은 창일 때만).
- 닫힌 창 드래프트와 내 드래프트가 같은 id(R4 A7): 값이 같으면 닫힌 창 키를 즉시 지우고, 다르면 내 드래프트가 저장된 직후 **충돌**로 등록한다(서버 = 방금 저장된 값, 내 값 = 닫힌 창의 값, 표식 "닫힌 창의 편집") — 저장소에 남아 나중 부팅에 다시 채택되는 일이 없다.
- `FTViewer._whenSaveSettled(ms)`(§3): 22-viewer-host 언마운트가 진행 중 저장의 응답을 기다린 뒤 destroy — 응답이 다음 마운트와 경합하지 않는다.

### 11.3 알림 행 (notices grid row)
- `#ftViewer > .ft-notices`(grid-area `notice`, controls.css): 구간 불일치 배너 `#rangeDivergenceBanner`, 저장 충돌 배너 `#markConflictBanner`(목록 `#markConflictPanel`은 R8 §15.3부터 이 행의 형제), 다른 주소 드래프트 알림 `#originDraftsNotice`(§11.4)가 이 한 행에 쌓인다. 비면 `display:none`. 색은 `--warn-bg`/`--warn`/`--text`(두 테마 AA).

### 11.4 포트(원점)가 바뀐 경우의 드래프트 (U9) — 한계 포함
- 브라우저 저장소(localStorage)는 원점(scheme+host+**port**)마다 따로다. 브라우저 모드는 8477이 막히면 8478…으로, WebView2 창은 **실행마다 새 포트**(src/shell.rs `reserve_port`, 포트 0)를 쓴다 → 이전 실행의 저장 안 된 드래프트는 새 원점에서 보이지 않고, 어떤 브라우저 API로도 다른 원점의 저장소를 읽을 수 없다.
- 클라이언트 최선책: 쿠키는 포트와 무관하게 호스트 단위로 공유되므로, 이 원점에 그 케이스의 드래프트가 있는 동안 쿠키 `ft_draft_ports=<port>.<case hash>-…`(FNV-1a 8자리, 케이스 id·경로 원문 없음, `SameSite=Strict`, 60일, 최대 24개; 서버는 읽지 않음)에 표식을 둔다(90-sync.js `noteOriginDrafts`, flush마다). 그 원점에서 케이스 드래프트가 모두 사라지면 표식을 지운다. 드래프트가 있는 동안에는 flush와 5 s 하트비트마다 쿠키를 다시 읽어 표식이 없으면 다시 쓴다(다른 포트 창의 [알림 닫기]·동시 쓰기로 지워져도 복구 — "이미 썼음"을 캐시하지 않음). 저장소 검사는 이 케이스의 드래프트 이름공간(`ft.draft.<case id>.`, 다른 포트에서 다시 열면 복원될 바로 그 키)을, 표식 해시는 case id + 케이스 폴더를 쓴다. 부팅 때 **같은 케이스·다른 포트** 표식이 있으면 알림 행에 "다른 주소(127.0.0.1:PORT)의 브라우저 저장소에 남아 있을 수 있음 — 같은 주소로 다시 열면 그 창이 복원"을 `role=status`로 보이고, [알림 닫기]가 그 표식을 지운다.
- 한계: "있을 수 있음"까지만 말한다 — 그 저장소가 이후 지워졌는지는 알 수 없다. 이 빌드 이후의 창만 표식을 쓴다. `127.0.0.1`과 `localhost`는 다른 호스트라 서로 보이지 않는다. 쿠키가 막힌 브라우저에서는 알림이 없다. 드래프트를 실제로 되살리려면 그 포트로 서버를 다시 띄워야 한다(WebView2 창은 포트를 고를 수 없음 — 실행 간 고정 선호 포트는 트랙 A(Rust) 몫, 미해결).

### 11.5 401 = 인증 오류로 거부 (U8)
- 원인: 서버는 살아 있으나 이 창의 HttpOnly 쿠키(`ft_nonce`/`ft_token`, 트랙 A가 포트별 이름 `ft_nonce_<port>`/`ft_token_<port>`로 바꾸는 중 — JS는 쿠키를 읽지 않으므로 무관)가 없거나 같은 컴퓨터의 다른 FrameTrace 인스턴스 것으로 바뀜. 그래서 "만료"·"다시 열기" 대신 "다른 FrameTrace 창이 열려 있으면 닫고 이 창을 새로 고치십시오"라고 말한다.
- 셸 `readJson(r, kind)`(20-net.js): 401이면 `{ok:false, status:401, auth:true, error}`와 `noteAuthExpired(kind)`, 성공하면 `noteAuthOk(kind)`. kind: GET(jget·jgetSoft) = `"read"`, POST(jpost) = `"write"`. 뷰어 저장(22-viewer-host)은 `"save"`: `error`(auth:true, 재시도마다)·`save-state`(auth:true)면 실패, `save-state` auth:false(저장 통과)·`destroyed`면 해제.
- 배너(40-status.js)는 실패 중인 kind 집합으로 그린다 — 무토큰 모드에선 GET /api/status가 증명 없이 200이므로 **정상 폴링이 저장 인증 실패를 지우지 않는다**. 제목: save/write 실패면 "저장이 인증 오류로 거부되었습니다", read만이면 "요청이 인증 오류로 거부되었습니다". 연결 끊김이 우선이고, 다시 연결되면 인증 모습이 돌아온다. 스토어 `server.lost`(배너 보임)·`server.auth`(인증 모습).
- [이 창 새로 고침]: 먼저 GET `/`을 확인 — 200이면 `location.assign("/")`(무토큰: nonce 재발급, 토큰 모드: `?token=`은 지워졌지만 쿠키로 열림), 401/403이면(토큰 쿠키 자체가 없음) 새로 고치지 않고 "FrameTrace 앱 창에서 다시 여십시오" 안내 + 버튼 숨김.
- 뷰어 저장의 401: 저장 상태·토스트 i18n `save.authExpired` = "저장이 인증 오류로 거부되었습니다 — 같은 컴퓨터에서 다른 FrameTrace 창이 열려 있으면 닫고 이 창을 새로 고치십시오", 드래프트는 그대로, 자동 재시도는 계속. 로더(99-loader.js)의 401·라이브 칩은 "인증 오류로 … 거부 — FrameTrace 앱 창에서 다시 여십시오".
- R4 A13: 뷰어의 저장 외 POST(자료 묶음·시간표·텔레메트리·캡처·클립·프록시·이어붙이기)는 `viewerPost`(65-actions.js) 하나로 보내고 401이면 i18n `net.authRefused`와 `error`(`auth:true, stage:"request"`) — 셸은 `noteAuthExpired("write")`. "오프라인"이라고 말하지 않는다.
- 하네스: verify-layout 11번(실서버 모델: 저장 POST 401 + /api/status 200 → 배너가 폴링·재시도 동안 유지, 저장 통과 시 해제; status 401 + `/` 401 → 안내, `/` 200 → 다시 로드), verify-viewer-undo(토스트 문구).

### 11.5a Esc와 포커스 (F5·F7)
- 셸의 문서 Esc 핸들러(97-shell.js)는 대상이 호스트 뷰어 안의 텍스트 필드(input·textarea·select·contenteditable)면 아무것도 하지 않는다 — 뷰어가 필드를 blur한다. 포커스가 토스트 안이면 그 토스트를 닫는다(셸 `.h-toast`는 셸, 뷰어 `.toast`는 뷰어). 닫은 뒤 포커스: 남은 가장 새 토스트의 버튼 → (셸) 열린 결과물 시트 → 뷰어 활성 카드(`FTViewer.focus()`/`viewerFocus`) → `#main`.

### 11.6 기타
- U7(하네스: verify-virtual-grid가 `_debugStats().filterComputes`로 j/k 300회 동안 재계산 0회, 마크 1회 = 재계산 증가를 확인): 마크·태그 칩 필터의 메모 키에 `annotationGen`(30-records.js `bumpAnnotations`: `touchAnnotation`, 드래프트 재생, 수화, 서버 값 적용 때 증가)을 넣어, 그 필터에서도 j/k는 필터·정렬을 다시 계산하지 않는다.
- U10: 요약 보고서는 `case_id`를 제목·머리줄 모두 이스케이프하고 파일 이름은 `safeFileName`. 재생 속도는 저장된 레이아웃 값을 0.25–4 범위의 수로 정규화하고, 팝아웃 인라인 스크립트에는 `RATES`에 있는 값만 `jsLit`으로 넣는다.
- U14: 가상 그리드는 `#recordGrid[role=grid][aria-rowcount] > .vspacer[role=rowgroup] > .vrow|.group-header[role=row][aria-rowindex] > [role=gridcell]`(axe `aria-required-children`/`-parent` 통과, verify-virtual-grid).
- 촬영 시각 표기(트랙 B 보고서와 일치): 파일 이름의 시각은 **기기 시계의 시각(시간대 미상)** — 표기는 파싱한 필드(`record.recWall`)에서 만들고 이 컴퓨터의 Date로 만들지 않는다(DST로 없는 시각 02:30이 03:30으로 밀리지 않음; 그리드 카드도 같음, verify-viewer-dialogs가 `timezoneId: Europe/Berlin` 2026-03-29 02:30:00으로 확인) — 상세·요약 보고서에 "(기기 기록 시각, 시간대 미상)", 날짜만이면 "(파일 이름의 날짜만 · 기기 기록, 시간대 미상)"; 내보내기(JSON·CSV)는 오프셋 없는 `YYYY-MM-DDTHH:mm:ss`(Z 없음)와 JSON `rec_time_basis`(`device-clock-naive`|`device-clock-date`|`file-mtime`). 파일 수정 시각처럼 실제 순간인 값만 `UTC±HH:MM`(이 컴퓨터 기준)과 "(추정)"을 붙인다. 요약 보고서 생성 시각도 `UTC±HH:MM`. 그리드 카드의 짧은 시각(공간 부족)은 표식 없이 그대로다.

## 12. R4 트랙 A 추가 계약 (2026-10-14, docs/PLAN_R4_2026-10-14.md)

### 12.1 구간(IN/OUT)은 저장 프로토콜 v2의 일급 필드 (A1/A2)
- 구간은 마크처럼 서버(또는 번들) 값으로 수화되고(`annotations.marks[].range`), 편집은 id별 드래프트(`range`)에만 남는다. 모든 marks 항목은 `range`를 **항상 명시**(`{in:null,out:null}` = 지움)한다 — 생략으로 "유지"시키고 "저장됨"을 보이는 경로는 없다.
- `deleted_ids`는 마크 행 삭제에만 쓴다: 마크·메모·구간이 모두 없고, 서버에 그 행이 있을 수 있을 때(`markRowMaybe`: 수화된 행, 이 창이 보낸 항목, 받아들인 서버 값). 태그는 언제나 명시 전송(태그만 있는 레코드는 태그 항목만).
- 서버와 다른 구간은 일반 충돌 목록(`#markConflictPanel`, 서버 값/내 값)으로만 다룬다. 충돌 중인 id의 편집(i/o·태그·마크 해제)은 전송되지 않고 "미저장"으로 남는다. `#rangeDivergenceBanner`는 없다.
- 예전 전체 맵 키 `ft.viewer.<case>.ranges`(RANGES_KEY) 폐지: 서비스 모드는 부팅 때 1회 이관 — 서버와 같으면 버리고, 다르면 그 id의 드래프트(충돌 표식 "이 브라우저의 이전 구간", base = 서버 리비전)로 옮긴 뒤 키를 지운다. 독립 뷰어(file://)는 번들이 기준이고 잔여는 적용하지 않고 `#legacyRangesNotice`(`role=status`, [이전 편집 내려받기] [이전 편집 지우기])로만 알린다.

### 12.2 저장 상태 `readonly`·`unknown`, 서버 대기열 (A3/A4/A5)
- `annotations.error`(records-meta가 판독 기록을 못 읽음): `#annotationsErrorBanner`(`role=alert`, "판독 기록을 읽지 못했습니다 — 편집 저장이 막혀 있습니다"), 저장 상태 `readonly`, 저장 요청 없음, 요약 보고서·CSV·마크 내려받기·케이스 반영 비활성, 셸 최종화 차단. 그동안의 편집은 이 브라우저 드래프트에 남고 다시 읽힌 뒤 자기 base로 전송된다.
- 서버 대기열(A4): 부팅 때 `/api/status.pending_marks > 0`이면 `GET /api/marks-pending?offset&limit`(트랙 B6) — 응답 `{ ok, total, offset, items: [{ id, mark|null, tags|null, deleted, base_revision, client_id }] }`(`items` 대신 `pending`도 허용, 페이지는 `total`까지 순회) — 의 값을 내 드래프트가 없는 id 위에 보이고, 상태 `queued`, 대기열 감시, 떠날 때 경고. 라우트가 없는 서버도 `queued`·감시·경고는 같다(값만 못 보임).
- 충돌 조회(A5): `GET /api/mark-conflicts?offset&limit`(트랙 B3)을 `total`까지 순회(페이지 상한 500; `total` 없는 옛 서버는 한 번에 전체). 두 경로(이 라우트, records-meta 폴백)가 모두 실패하면 상태 `unknown`("충돌 확인 실패"), 2 s~60 s 백오프로 재시도, "저장됨" 금지.

- 트랙 B 구현에 맞춘 세부(PLAN_R4 "§B4 as implemented"): 두 라우트 모두 `next_offset`(마지막 페이지 `null`)으로 순회하고 `count`를 전체 수로 본다. 대기열 항목 키는 `entries`(`items`·`pending`도 허용).
- 저장 응답 `{"ok":false,"busy":true,"retry":true,"retry_after_ms":n}`(보강 밖의 짧은 busy, 대기열에 넣지 않음)은 일시 상태: dirty 유지(상태 `pending`, "저장됨"·"실패" 아님), `retry_after_ms` 뒤 재시도(연속 10회까지, 넘으면 일반 실패·백오프). `ok:true`와 함께 온 `report_error`는 저장 성공이고 오류 토스트 "마크는 케이스에 저장되었지만 보고서 갱신에 실패했습니다"를 문구마다 한 번 보인다.
- 작업 시트 단계 상태 `"partial"`(종료 코드 5, B16)은 경고 모습(`li.done.warned[data-step=partial]`, "부분 결과로 끝났습니다")이며 완료 단계로 센다.

### 12.3 셸 닫기 전 저장 (A15 ↔ C15)
- 셸 JS `window.hBeforeClose(timeoutMs = 5000) → Promise<{ ok, pending, state, message }>`: `FTViewer.save()`(진행 중 저장의 응답 대기 포함)를 상한까지 기다린다. `ok:false`면 `message`가 확인창 문구("저장 안 된 편집 N건"). **(R7 §14.1이 대체: 기본 4 s, 전체 상한, `warning` 추가, 독립 뷰어 페이지에도 있음)**
- WebView2 경로(트랙 C 구현, src/shell.rs `FLUSH_SCRIPT`): WM_CLOSE 때 셸이 페이지에서 `FTViewer.save()`를 실행하고 결과 `{ft_close_save:1, ok, pending}`를 `chrome.webview.postMessage`로 받는다(대기 상한 5 s). `save()`는 진행 중 저장의 응답을 먼저 기다리며, 미저장·충돌·확인 실패·읽기 전용이면 false, 서버 대기열만 남았으면(내구 저장) true다.

### 12.4 기타
- A6: 모든 Enter 처리(태그 입력·팔레트·대화상자·셸 입력)는 IME 조합 중(`isComposing` 또는 `keyCode 229`)이면 무시한다. 뷰어 전역 단축키 처리기는 예외(비라틴 배열 단축키가 `Process`/229로 오므로 U6 `shortcutKey`가 처리).
- A8: Undo 저널 항목은 압축 저장 `{ label, activeBefore, n, groups: [{ ids, fields, before, after }] }`(같은 전후 값끼리 묶음, 바뀐 필드만), 두 스택 합계 20만 id 초과 시 오래된 항목부터 폐기(가장 최근 항목은 유지).
- A12: 명령 팔레트(Ctrl+K)는 뷰어 모달 또는 셸 모달(`[aria-modal=true]`, 보이는 것)이 열려 있으면 열리지 않는다.
- A14: 검색 강조는 원문에서 찾고 조각별로 이스케이프한다. 태그 목록은 제자리 수정 없이 교체한다.
- B2 계약: 셸(99-init.js)과 서비스 독립 뷰어(99-loader.js)는 `?token=`과 1회용 `?launch=`를 주소창·기록에서 지운다.


## 13. R5 트랙 U 추가 계약 (2026-10-14, docs/PLAN_R5_2026-10-14.md)

### 13.1 읽기 전용(판독 기록 읽기 실패) 동안의 편집 (U1/U2/U15)
- `annotations.error`로 읽기 전용이면 주석 편집은 **입구에서 거부**된다(`refuseLockedEdit`, 30-records.js): 마크 버튼·1/2/3/0, 태그 메뉴·태그 편집기, 메모 칸(readonly), IN/OUT(i/o), Ctrl+Z/Y, 팝아웃 플레이어. 버튼은 비활성 + 툴팁 `save.readonlyEdit`, 키 입력은 같은 문구 토스트.
- 그래도 읽기 전용 중에 만들어진 드래프트(이전 빌드·우회 경로)는 `unsafe_base: true`를 달고, 저장 때 `base_revisions[id] = -1`로 **자기만의 본문**에 실려 간다(`splitUnsafeBase`). 서버(트랙 P)는 -1을 같은 창 예외 없이 **항상 충돌**로 답한다. -1을 모르는 서버(파싱 거부 `base_revisions… non-negative`, 또는 patch-v1)는 그 본문을 적용하지 않으므로, 뷰어가 records-meta의 현재 값으로 같은 충돌을 직접 만든다 — patch-v1로 내려가는 일은 없다. 충돌 해결(`touchAnnotation(id, {base})`)만 표식을 지운다.
- 읽기 전용 부팅은 옛 구간 키(`RANGES_KEY`) 이관을 하지 않고 키를 그대로 둔다(U2).
- `FTViewer.save()`: 읽기 전용이어도 이 창에 드래프트·미저장·충돌이 하나도 없으면 `true`(닫기 확인 없음). `hBeforeClose`는 그때 `{ok:true, warning}`(U15). 최종화 차단은 그대로(`readonly`는 FIN_UNSAVED).

### 13.2 구간 키 보류 (U3)
- 이관 때 케이스가 모르는 id의 구간은 `ft.viewer.<case>.ranges.held`(`RANGES_HELD_KEY`)로 옮기고 지우지 않는다. 매 부팅 다시 확인해 그 id가 생기면 일반 이관(충돌 표식 "이 브라우저의 이전 구간").

### 13.3 서버 대기열 표시 (U4) · 옛 서버 충돌 (U5) · busy (U6)
- 부팅 때 대기열 값으로 덮어 보인 id(`queueOverlayIds`)는 대기열이 비면 **모두** 케이스 DB 값으로 되돌린다(리비전이 안 바뀌었어도 — 적용되지 않은 본문). 그 사이 편집·충돌된 id는 제외.
- 대기 값을 못 읽음/일부만 읽음: `#saveState[data-queue-values="unreadable"|"truncated"]` + 툴팁 문구. `truncated` = 서버 `truncated:true`, 받은 수 < `count`, 또는 페이지 상한.
- `/api/status.mark_conflicts`에 `ids`가 없고 `GET /api/mark-conflicts`가 없는(404/405/501, 그 모양이 아닌 JSON) 옛 서버: 알림 `#conflictsUnlistedNotice`(role=status, "구버전 서버 — 충돌 목록 불가") 후 상태를 풀어 준다. 라우트가 5xx·네트워크로 실패한 것은 여전히 `unknown`·재시도.
- `FTViewer.save()`는 `busy/retry` 응답이면 `retry_after_ms`를 기다려 다시 보낸다(최대 10회·합계 20 s), 바로 false가 아니다.

### 13.4 페이지 커서 (S11 ↔ U)
- `readPagedList`: 응답에 `next_after`가 있으면 다음 페이지는 `?after=<값>&limit=500`, `null`이면 끝. 없으면 예전 `next_offset`/`count` 방식.

### 13.5 기타
- U7: 셸의 직접 귀속 패치 `client_id`는 탭(페이지)마다 난수 `shell-<rand>`(`FIN_CLIENT_ID`).
- U8: 팝아웃의 프레임 캡처는 브리지 `capture`(→ `viewerPost`)로 보낸다. 401이면 `net.authRefused`와 호스트 `error`(auth). 브리지가 없을 때만 직접 POST(401도 인증 문구).
- U16(S15): records-meta `proxy_paths` {id: path}(또는 `proxyPaths`, 객체형 `proxies` 항목, 레코드 행의 `proxy_path`)가 있으면 **그 값만** 쓴다(없는 id = 프록시 없음). 없으면 예전 파일 이름 매칭.
- S6 ↔ U: 오류 문구가 raw 불일치("raw 불일치 — 재임포트 필요", "재임포트", "reimport-raw", "raw mismatch")이거나 `/api/status.raw_reimport = {raw_path, reason, endpoint}`가 있으면 오류 상자에 `[원본 다시 가져오기]`(`button[data-reimport-raw]`). 누르면 확인 대화상자("raw 파일을 삭제하고 E01에서 다시 만든 뒤 같은 분석 재실행", 위험 버튼) 후 `POST /api/reimport-raw {raw_path}`; 404/405면 "이 서버 버전은 지원하지 않습니다".
- S13: 조인 차단 코드 `audio-only` 문구(80-review.js `JOIN_CODE_TEXT`; 서버 `message`가 있으면 그것이 우선).
- S ↔ U14: launch 303은 `launch`만 빼고 나머지 쿼리·해시 유지. verify-layout 14번은 서버가 그렇게 답할 때 `?keep=1`까지 확인하고, 아니면 SKIP("awaits track S").
- U17: 목록 안 위치는 `indexInList(list, id)`(목록마다 id→index 맵, 40-grid.js), 검토 진행 수는 `annotationGen`이 바뀔 때만 다시 센다. verify-virtual-grid가 5만 건에서 클릭·마크 지연 중앙값 < 200 ms를 확인.

## 14. R7 트랙 U 추가 계약 (2026-10-15, docs/PLAN_R7_2026-10-15.md)

### 14.1 저장·닫기 예산 (U1/U5/U10, S7 ↔ U)
- `FTViewer.save({ timeoutMs })`(또는 `save(ms)`): `timeoutMs`는 **이 호출이 답하기까지의 전체 상한**이다 — 이미 전송 중인 저장의 응답 대기(`whenSaveSettled`), 충돌 조회 대기, 새 전송, busy 재시도(`retry_after_ms`) 모두 같은 마감 하나에 들어간다(R6은 busy 재시도만 묶고 진행 중 저장 대기는 무제한이었다). 마감이 지나면 `false`, 넘긴 옵션 객체에 `timedOut: true`. 요청 자체는 취소하지 않는다 — 늦게 온 응답도 드래프트를 정리한다.
- 호출자별 예산: 명시적 저장(최종화 재시도 41-finalize, 케이스 전환 22-viewer-host·21-project, 귀속 `attributeUnassignedMarks`, 그 밖의 `save()` 무인자 호출)은 기본 **20 s**(`SAVE_CALLER_WAIT_MS` = `SAVE_BUSY_WAIT_MS`). 닫기(`hBeforeClose`)만 짧게 — 아래.
- `window.hBeforeClose(timeoutMs = 4000) → Promise<{ ok, pending, state, message, warning }>`: 셸(src/shell.rs FLUSH_SCRIPT)은 `hBeforeClose(4000)`을 부르고 5 s까지 기다린다. `timeoutMs`는 **호출 시점부터의 전체 상한**: 뷰어 `save({ timeoutMs: timeoutMs − 250 })` + 백스톱 타이머(상한 − 100 ms, Windows 타이머 지연 여유)가 먼저 끝나는 쪽으로 답하고, 그 뒤는 동기 코드뿐이다. 시간 초과(백스톱 또는 `opts.timedOut`)면 `message` 끝에 " (저장 응답 대기 시간 초과)".
  - 워크스테이션 셸 페이지: `assets/app/95-quit.js`(한국어 문구 고정).
  - **독립 뷰어 페이지**(`/viewer/`, `/review/*.html`, `data-ft-host` 없음): `assets/viewer/00-host.js`가 같은 모양·같은 예산의 `window.hBeforeClose`를 정의한다. 문구는 뷰어 i18n `close.*`(`close.unsaved` "저장 안 된 편집 {n}건", `close.timeout`, `close.readonlyBlocked`, `close.notConfirmed`, `close.readonlyWarn`) — 기본 한국어, 영어 조각("unsaved N")은 없다. 마운트 전에는 한국어 고정 문구(`FTViewer._closeText`).
  - `message`: `ok:false`일 때만(확인창 사유, 끝 마침표 없음). `warning`: 읽기 전용인데 이 창에 잃을 것이 없는 깨끗한 종료(`ok:true`)일 때 "판독 기록을 읽지 못한 상태였습니다 — …" — 페이지에도 **경고 토스트**(`warn`, 호박색 `toast-warn`; 오류 모습이 아님, U8)로 보인다.
- 뷰어 토스트 종류: `FTViewer.toast(message, "info"|"warn"|"error")`, `toast` 이벤트 detail `kind`도 `"warn"`을 낸다(셸 01-store는 `"warn"`으로 옮김).

### 14.2 알림 행 (U2/U9)
- `#queueUnreadableNotice`(대기 파일 N개 읽기 실패)는 대기열이 비면(`afterQueueDrained`) **같은 페이지에서** 사라진다(저장 상태 툴팁·`data-queue-unreadable`도). 문구는 한 번만: "대기 파일 {n}개 읽기 실패 — 그 파일의 값은 화면에 보이지 않습니다. …".
- `readPagedList`의 `unreadable` 집계: 이름(`file`) 있는 항목은 이름으로 한 번, 이름 없는 항목은 페이지 안 순번(`#k`)으로 — 같은 목록이 페이지마다 반복돼도 한 번, 이름 없는 두 파일은 두 개(하나로 합치지 않음).
- `.ft-viewer > .ft-notices`는 최대 높이 `min(28vh, 216px)`, 넘치면 세로 스크롤(`overscroll-behavior: contain`). 스크롤되는 영역이라 `role=region`(R8 §15.5: `tabindex=0`은 넘칠 때만), `aria-label` i18n `notices.label`("알림"). verify-layout: 1366×768(두 테마)에서 긴 알림 5개 → 행 ≤ 상한·스크롤·마지막 알림 도달 가능, 그리드 높이 ≥ 200 px, 선택 바 화면 안, 가로 스크롤 없음.

### 14.3 팝아웃 캡처 (R6 U7 기록 + R7 U6)
- 팝아웃의 모든 브리지 호출 `call(op, arg, cb, ms)`은 **두 경로 모두 시간 제한**: opener 직접 호출(`window.opener.__ftBridge`)이 Promise를 돌려주면 `ms`(기본 3 s, 캡처 `CAP_MS` 30 s) 뒤 `undefined`로 끝낸다(늦은 응답은 무시). BroadcastChannel 경로(`ft-player:<case>`, `token` 일치 창만 응답)도 같다.
- 캡처: 먼저 `ping`(1.5 s). 답하면 `capture`를 요청 id `rid`(`<writerToken>-<시각>-<n>`)와 함께 브리지로 보내고, 답이 늦으면 다시 ping → 살아 있으면 **같은 `rid`로** 재요청(최대 5회; 뷰어는 창 단위 `window.__ftCaptureRids`(최근 32개)로 첫 요청의 결과를 그대로 돌려줌 — 프레임 하나에 파일 하나). ping이 처음부터 답이 없으면 그때만 직접 `POST /api/capture-frame`.
- 문구 구분: 재요청이 끝나도 결과가 없을 때 — 뷰어가 ping에 **답함** → `player.capSlow`("캡처 응답이 늦습니다 — 메인 뷰어는 열려 있지만 …"), **답 없음** → `player.capUnsure`("… 메인 뷰어가 닫혔습니다 …"). 어느 쪽도 두 번째 POST를 보내지 않는다.

### 14.4 원본 다시 가져오기 재확인 (U7)
- `[원본 다시 가져오기]` 확인 대화상자에서 [삭제 후 다시 가져오기]를 누른 뒤 `GET /api/status`를 다시 읽는다: `busy`면, `raw_reimport`가 없거나 `raw_path`가 대화상자에 보인 것과 다르면, 상태를 읽지 못하면 POST 없이 취소하고 경고 토스트로 이유를 말한다. 대화상자가 열린 동안 두 번째 대화상자는 열리지 않는다.

### 14.5 마크 수·필터 목록 (U4)
- 검토 진행(`검토 n/m`), 판독 줄(`판독 x/m 완료 (중요 · 검증 대기)`), 선택 줄의 마크 수, `summary().marked`는 **증분 집계**(30-records.js `markTallyNow`): `bumpAnnotations(id)`가 바뀐 id를 모으고 다음 읽기에서 그 id만 반영. 수화(id 없는 bump)·`state.marks` 교체·4096건 초과 때만 전체 1회. j/k·클릭은 마크 맵을 돌지 않는다.
- 마크·태그 칩 필터 목록(`filteredRecords`)은 주석만 바뀐 경우 바뀐 id(최대 256)만 필터에 다시 대 본다: 빠진 레코드는 옛 목록에서 잘라 내고(정렬은 주석을 읽지 않아 순서 유지), 옛 목록의 id→위치 맵을 잘린 위치 목록과 함께 재사용(`indexInListDerive`, 누적 512개까지) — 새 Map을 만들지 않는다. 새로 들어오는 레코드(정렬 위치 모름)·필터·정렬 변경·수화는 전체 계산. `_debugStats().filterComputes`는 두 경우 모두 1 증가(메모 무효화 계수).
- verify-virtual-grid R7: 5만 건 중 4만 건 마크, `mark:important` 필터에서 j/k 1만 번 — 마크 맵 순회(`Object.keys/values/entries` ≥ 1만 키) 0, 1만 항목 이상 Map 생성 0, 1만 이상 배열 전체 순회 0, 시간 상한; 필터 안 마크 8번 — 다음 레코드로 이동, 마크 맵 순회·Map 재생성 0, 집계 일치.

### 14.6 항진 검사 정리 (U3)
- 소스 정규식으로 동작을 대신하던 검사(verify-review-batch: 대기 파일 표시, 독립 `hBeforeClose` 정의, 저장 예산 상수, 옵션 전달, base −1 독립 전송, 읽기 전용 `save()`)를 지우고 실제 페이지 검사(verify-range-sync ⑨·⑱·⑳·㉑·㉒, VM의 `hBeforeClose` 시간 검사)로 대신한다. verify-marks-sync ⑯은 S2 문자열 커서를 **요구**한다(숫자 커서 대체 분기 삭제).

### 14.7 읽지 못한 케이스 파일 (S ↔ U, R7 S6)
- make-review 데이터 묶음·records-meta는 올바른 JSON이 아닌 `case.json`·증거 색인을 `null`로 싣는다(`util::json_for_script`, records.rs). 뷰어(10-boot.js `caseInputProblems`)는 `manifest`/`scan`이 객체가 아니면(API 경로는 99-loader가 `manifestUnreadable`/`scanUnreadable`로 전달) 알림 행 맨 위에 `#caseInputErrorBanner`(`role=alert`, `data-problems="manifest scan"`)로 "케이스 파일을 읽지 못했습니다 — …"를 보이고 `error`(stage `load`)를 낸다 — 빈 목록이 "증거 없음"으로 읽히지 않게. 인라인 묶음은 `manifest: null`이어도 그 묶음을 쓴다(다른 데이터 출처를 찾다가 "data-bundle.js 없음"으로 빠지지 않음). 하네스: verify-player-keys "R7 null case.json / index".
- 하네스는 HEAD 요청에 기대지 않는다(서버는 스크립트를 GET으로 한 번 검증하기 전 HEAD에 403).

## 15. R8 트랙 U 추가 계약 (2026-10-15, docs/PLAN_R8_2026-10-15.md)

### 15.1 case.json을 읽지 못한 케이스 (U1)
- 저장소 키·채널의 케이스 부분은 `FTViewer.caseStorageKey(manifest, caseDir)`(00-host.js) 하나로 만든다: `case.json`의 `case_id`, 읽지 못했거나 없으면 `"nocase-"` + 케이스 폴더(`caseDir`, 번들·records-meta가 싣는 값; 경로 구분자 `/`로 정규화) FNV-1a 64비트(정방향·역방향 32비트 두 개) 16진수. 가린 번들(`"<case>"`)·폴더 없음이면 이 페이지 주소의 해시. 문자 그대로의 `"case"`는 어디에도 쓰지 않는다 — `ft.viewer.<key>.*`, `ft.draft.<key>.*`, `ft.alive:<key>:*`, `ft-alive:<key>`, `ft-player:<key>`, 리로드 보기 사본, 다른 포트 쿠키 해시 모두. 셸(41-finalize 점검, 95-quit 종료 확인)의 드래프트 수도 같은 함수로 센다.
- `case.json`을 읽지 못하면(10-boot `caseFileUnreadable`) 뷰어는 **읽기 전용**이다 — R5 §13.1 경로 그대로(`annotationsError` = `"case.json"`, 저장 상태 `readonly`, 편집 입구 거부, 요약·CSV·마크 내려받기·최종화 차단, 옛 구간 키 이관 없음). 문구는 원인별(`annotationsLockCause = "case"`, `lockKey(key)` → i18n `…Case` 변형: "케이스 정보(case.json)를 읽지 못해 편집할 수 없습니다 — …"). 알림은 하나: `#caseInputErrorBanner`(role=alert)에 "편집·저장이 막혀 있습니다"가 붙고 `#annotationsErrorBanner`는 만들지 않는다. 하네스: verify-player-keys "R8 U1".
- 이미 읽힌 `case.json`에 `case_id`가 없던 케이스는 이전 빌드의 `"case"` 키 드래프트를 이어받지 않는다(그 키는 다른 케이스와 섞일 수 있어 의도적으로 버림).

### 15.2 명시적 저장은 진행 중이면 기다림 (U2)
- `FTViewer.save(opts)`(90-sync.js `viewerSave`) 예산 세 가지:
  - `save(ms)` / `save({ timeoutMs })`: **고정 전체 상한**(닫기 `hBeforeClose`, R7 §14.1 그대로).
  - `save({ patient: true[, timeoutMs] })`: 명시적 저장(최종화 재시도·저장 버튼 41-finalize, 케이스 전환 22-viewer-host·01-store, 작업 시트 32-jobs, 귀속 `attributeUnassignedMarks`). 기본 20 s(`SAVE_CALLER_WAIT_MS`)가 지나도 이 창의 저장 요청이 **진행 중**이면 기다린다 — 진행 = 요청 전송 또는 응답 수신(`saveProgressAt`, `pushMarksToCase`)이 `SAVE_STALL_MS`(30 s, 하네스는 `stallMs`로 줄임) 안에 있었음. 요청이 그동안 아무 응답도 없으면 정체 → false + `timedOut`.
  - `save()`(예산·patient 없음): 셸 대체 경로(src/shell.rs FLUSH_SCRIPT가 `hBeforeClose` 없는 페이지에서 `FTViewer.save()`)의 상한 — 고정 `SAVE_DEFAULT_MS` 4 s.
- 하네스: verify-review-batch --unit "R8 U2"(VM: 진행 중 연장, 고정 상한 유지, 정체 시간 초과, 무인자 ≤ 4 s), verify-range-sync ㉓(실제 페이지: 2.5 s 늦은 응답을 patient가 기다림, 고정 1 s는 false, 응답 없는 요청 정체, 무인자 ≤ 4 s), ⑳은 `{ patient: true }`.

### 15.3 충돌 목록은 알림 행 밖 (U3)
- `#markConflictPanel`은 `.ft-notices`의 **형제**(`#ftViewer` 직속, 그리드 영역 `conflicts` — 행 순서 head·notice·conflicts·top·work·bar). 상한 `min(30vh, 260px)` 하나의 스크롤. 열려 있는 동안 알림 행 상한은 `min(14vh, 108px)`(controls.css `:has`). verify-layout R8 U3: 1366×768 두 테마에서 실제 저장 충돌 → 목록 열기 → Tab으로 항목 버튼(서버 값/내 값) 도달, 각 버튼 화면 안·가려지지 않음·≥24 px·알림 행 스크롤 밖, 형제 관계, 선택 바 화면 안, 가로 스크롤 없음; 긴 알림 5개를 더해도 같고 그리드 ≥120 px.

### 15.4 하네스 시간 검사 (U4) · 마크 집계 (U5/U8)
- verify-virtual-grid R7 j/k 1만 번: 시간 **비율** 비교(기준 페이지) 삭제. 계수기(마크 맵 순회·큰 Map 재생성·전체 배열 순회·재집계·재필터 = 0)만 판정하고, 벽 한도는 키당 평균 40 ms(측정값 약 8 ms의 5배). 크기 변경 검사는 그리드의 실제 너비·`--card-min-w`·간격으로 기대 열 수를 계산해 그 수가 바뀌는 너비까지 줄이고, 다시 그려질 때까지 제한 시간 안에서 확인한다(Linux 글꼴·스크롤바 차이에 무관).
- 마크 집계(30-records `markTallyBucket`): 케이스에 없는 id는 별도 표식 `MARK_TALLY_OFFCASE`, 상태가 비거나 없는 마크는 `""` 버킷으로 이 케이스의 마크(`n`)에 센다(R7은 둘을 같은 `""`로 묶어 빈 상태 마크가 검토 수에서 빠졌다). verify-review-batch --unit "R8 U5/U8": 씨앗 5개 무작위 실행(빈 상태·케이스 밖 id·지움·undo/redo·충돌 해결·수화·4096 초과 대량·레코드 증가)마다 증분 집계 = 전체 재계산.

### 15.5 알림 언어·Tab (U6/U7)
- 알림 행의 모든 문구(충돌 배너·버튼, 대기 파일·목록 일부·구버전 서버·이전 구간·다른 주소 드래프트·읽기 전용·케이스 파일 알림)는 `i18nText(el, key, subs)`(20-i18n.js: `data-i18n` + `data-i18n-args` JSON)로 넣고 `applyChromeI18n()`이 언어 전환 때 다시 그린다. 열린 충돌 목록은 언어 전환 때 다시 만든다. verify-player-keys R8 U6.
- `.ft-notices`는 `role=region` + `aria-label`을 늘 갖지만 `tabindex=0`은 **넘칠 때만**(MutationObserver·ResizeObserver, 마운트 신호로 해제). 넘치지 않게 되면 tabindex를 지우고, 그때 포커스가 행에 있었으면 뷰어 활성 카드로. verify-player-keys R8 U7.

### 15.6 신뢰 표시 (트랙 I P0/P1/P2 ↔ U)
- 소리 수준(`audioLevels`): `"sc": true`(감사 키 없는 PC의 측정) — 수준은 보이되 툴팁·상세에 "자기 일관성만(인증 아님)"(i18n `audio.selfConsistent`). `r: "unverified"`에 `"pc": "<key id>"` — 미확인 그대로, "키 <id> 보유 PC에서 확인"(`audio.otherPc`). 30-records `audioInfoFor`/`audioTrustNote`. verify-review-batch --unit "R8 (I <-> U)".
- `/api/verify-audit`(41-finalize `auditVerdict`, 환경 배지 `#auditBadge`·결과물 시트 감사 로그 줄): 초록(`ok`)은 `integrity-keyed`일 때만. `integrity-structural-only` = "감사 로그 자기 일관성만 확인(인증 아님)", `integrity-not-verified` = "감사 로그 이 PC에서 인증 불가 — 키 <other_pc_key_ids> 보유 PC에서 확인" — 둘 다 호박색(`warn`). 로그별 `trust` 사유는 툴팁. 검증 실패 = `bad`. verify-finalize-sheet 1·2. (R9 U4: §16.2가 `integrity-not-verified` 문구를 대신한다.)

## 16. R9 트랙 U 추가 계약 (2026-10-15, docs/PLAN_R9_2026-10-15.md)

### 16.1 감사 레인 줄의 신뢰 (U1, 트랙 I P3 ↔ U)
- 번들·`/api/records-meta`의 모든 로그 항목(`validationLog`, `carveLog`, `filesystemLog`, `joinLog`, `anomalyLog` …)은 `trust`를 싣는다: `"auth"`(이 PC에서 인증) · `"sc"`(키 없는 PC, 자기 일관성만) · `"unauth"`(키가 있는데 인증 안 됨) · `"pc:<key id>"`(다른 PC 키). 없으면 이전 서버(레거시) — 지금처럼 보이되 "인증됨"이라고 하지 않는다. 그 밖의 값은 `"unauth"`로 읽는다(30-records `logTrustOf`).
- 쓸 수 있는 줄 = `auth` · `sc` · 레거시. 키별 현재 항목(`currentLogEntries`/`logSlots`)은 **마지막 쓸 수 있는 줄**이고, 인증 안 된 줄은 쓸 수 있는 줄이 하나도 없을 때만 그 기록으로 보인다(카빙 id, 복구 inode, 조인 출력 경로, 검증 대상 경로 모두).
- 기록 상태(`recordStatusOf`): 자기 줄(카빙·복구·조인)이 인증 안 됨이면, 또는 경로의 검증이 인증 안 된 줄뿐이면 상태는 `unauthenticated`("인증 안 됨") / `auth-other-pc`("이 PC에서 인증 불가", 툴팁 "이 PC에서 인증 불가(변조 판정 아님) — 키 <id> 보유 PC에서 확인") — 확인됨·검증됨·파생물이 되지 않고 `검증됨` 지표·요약 보고서 수에서 빠진다. 아이콘 `lock`. `sc`는 값을 그대로 보이고 배지 툴팁에 "자기 일관성만(인증 아님)". 상세의 검증·이상 목록은 줄마다 신뢰 표시(`logTrustTagHtml`).
- 내보내기(U5): CSV·JSON에 `audio_trust`(`sc` / `pc:<id>` / `unauth` / 수준의 `trust` / `""`)와 `status_trust`(상태가 기댄 줄의 `trust`, 레거시 `""`).
- 하네스: verify-audio-facet `/trust`(실제 페이지: 배지·지표·상세 목록·최신 인증 줄 우선·조인·CSV), verify-review-batch --unit "R9 U1".

### 16.2 다른 PC 줄 · 감사 배지 (U3/U4)
- 다른 PC 키로 서명된 소리 측정(`pc`)은 "검증 실패"가 아니라 "이 PC에서 인증 불가(변조 판정 아님) — 키 <id> 보유 PC에서 확인". 트랙 없는(unknown) 기록도 카드에 `lock` 표시와 같은 문구.
- `auditVerdict`(41-finalize): `integrity-not-verified`면 HMAC 없는 줄·은퇴 키 줄(`unauthenticated_lines`, 없으면 서버 사유 문구에서 셈)을 먼저 "감사 로그 인증 안 된 줄 N개", 다른 PC 키 줄(`other_pc_lines`)만이 원인일 때만 "이 PC에서 인증 불가(변조 판정 아님) — 키 … 보유 PC에서 확인", 그 밖의 원인은 "이 PC에서 인증되지 않음". 알 수 없는 상태도 다른 PC 키를 적는다. verify-finalize-sheet 1.

### 16.3 원인별 읽기 전용 문구 (U2) · 아직 저장 중 (U6)
- `summary().lockCause`가 읽기 전용 원인을 준다. 셸 닫기(95-quit `hBeforeClose`)·최종화 점검(41-finalize `unsavedMarksChecks({ lockCause })`; 뷰어 값이 없으면 records-meta의 `manifest` 없음 = `"case"`)은 `"case"`면 "케이스 정보(case.json)를 읽지 못해…", 아니면 "판독 기록을 읽지 못해…".
- `save({ patient: true })`가 false + `timedOut`이고 저장 상태가 `saving`(요청이 아직 진행 중)이면 호출자는 실패가 아니라 "아직 저장 중"(01-store `hSaveStillSaving`): 최종화 '저장 다시 시도'·검토자 기록, 프로젝트 전환(`hostViewer.stillSaving`), 뷰어 `attributeUnassignedMarks`(`attr.stillSaving`). verify-review-batch --unit "R9 U2/U6".

### 16.4 "case" 키 (U7/U8) · 하네스 저장 (U9) · 충돌 목록 Tab (U10)
- 이전 빌드가 문자 그대로의 `"case"` 키로 남긴 드래프트(`ft.draft.case.*`, `ft.viewer.case.annotations.v1`)는 채택하지 않고, 알림 행에 한 번(`#legacyCaseDraftsNotice`; 건수가 바뀔 때만 다시, `ft.viewer.legacyCaseDrafts.noticed`) "케이스를 알 수 없는 이전 버전의 임시 편집 N건 — 반영하지 않았습니다". 지우지 않는다. `case_id`가 실제로 `"case"`인 케이스에서는 알리지 않는다. verify-audio-facet `/trust`.
- `FTViewer.channels.player(key)`는 키가 없으면 `caseStorageKey(null, "")`(이 페이지 주소의 해시) — `"case"` 없음.
- 하네스에서 실제 저장을 기다리는 `FTViewer.save`는 `{ patient: true }`. 4 s 무예산 경로를 시험하는 곳만 `FTViewer.save(/* no-budget: … */)`로 표시(verify-review-batch --unit "R9 U9"가 scripts/*.mjs 검사).
- 충돌 배너는 알림 행의 **마지막** 배너(다른 알림은 `noticeAppend`로 그 앞), 목록 토글은 배너의 마지막 버튼 — Tab 순서가 토글 → 목록 첫 버튼, Shift+Tab은 토글로. verify-layout "R9 U10".

## 17. R10 트랙 U 추가 계약 (2026-10-15, docs/PLAN_R10_2026-10-15.md)

### 17.1 최신성 규칙 P7 (U1)
- 같은 키(카빙 id, 복구 inode, 조인 출력 경로, 검증 대상 경로)의 현재 항목 = **마지막 줄**. 마지막 줄이 쓸 수 없는 줄(`unauth`·`pc:`)이면 그 앞의 인증 줄로 되돌아가지 않는다(R9 §16.1의 "마지막 쓸 수 있는 줄"을 대신함). 30-records `logSlots`(`last`) · `currentLogEntries` · `splitValidationSlots`.
- 앞에 쓸 수 있는 줄이 있었으면 그 항목은 복사본에 열거 안 되는 `ftNewerUnauth` 표시(내보내기에 안 나감). 기록은 `statusNewer: true` — 상태는 `unauthenticated`/`auth-other-pc` 그대로, 배지 툴팁 맨 앞과 상세 로그 목록에 "더 최근 인증 안 된 항목 있음"(i18n `status.tip.newer`, 행 `.status-newer`). 검증 값(SHA-256·길이·코덱)도 그 경로의 쓸 수 있는 최신 검증이 아니면 쓰지 않는다.

### 17.2 key_config_error (U2)
- `auditVerdict`(41-finalize): `logs[].key_config_error === true`(또는 응답 자체)면 다른 모든 상태보다 먼저 "감사 키 설정을 읽지 못함 — 인증 불가(변조 판정 아님)", 호박색(`warn`, P8 — 실패 아님), 툴팁은 레인별 서버 사유(`auditKeyConfigError`).

### 17.3 trust_note · logChains (U3)
- 쓸 수 없는 항목의 `trust_note`: 상태 배지 툴팁(`statusNote`)·상세 목록 신뢰 배지 툴팁·항목 아래 `.trust-note` 줄(30-records `logTrustNote`, 40-grid `logTrustNoteHtml`).
- `/api/records-meta` `logChains`는 로더가 그대로 넘기고(`data.logChains`), 상세 로그 목록(`#validationList`) 머리에 그 기록의 레인(검증 + 카빙/복구/조인 + 이상 있으면 이상 탐지)마다 문제가 있을 때만 한 줄 `.log-chain[data-lane]`: "검증 로그 체인 검증 실패 / 자기 일관성만(인증 아님) / 인증 안 된 줄 N개 / 인증 불가(변조 판정 아님) / 읽지 못함", 툴팁 = 서버 `detail`. `absent`·인증 줄만 있는 `keyed`는 표시 없음, 모르는 상태는 "인증 불가". `logChains` 없음(구 서버) = 표시 없음. 독립 번들의 `logChains`는 `{anomaly:{head}}`만(R17 트랙 I6, §24.4) — 레인 줄은 없음.

### 17.4 최종화 미완료 패키지 (U4, 트랙 S2)
- `packageIncomplete(dir, flag)`(10-core): 폴더 이름이 `…_INCOMPLETE`(또는 `*.finalize-incomplete.txt`)이거나 서버가 명시 필드(`package_incomplete`·`finalize_incomplete`)를 보내면 참. 표시는 "최종화 미완료"(`H_PKG_INCOMPLETE`).
- 상태: `done`/`finalized` + 미완료 패키지 → 상점 phase `done`(결과물 생성됨 아님), `project.packageIncomplete`; 프로젝트 전환 팝업 배지 `warn` "최종화 미완료". (R11 U3: 최근 케이스 분기는 서버 힌트가 없어 제거 — §18.3.)
- 결과물 시트: `finIncompleteOf(res)` — `incomplete_package_dir`/미완료 `package_dir`/`finalize_incomplete`/실패 문구의 "최종화 미완료" → 결과 목록 첫 줄 `.h-fin-incomplete` "최종화 미완료"(+경로가 필드로 오면 경로·탐색기) "최종 결과로 쓰지 마십시오", 판정 줄 "보고서·패키지를 만들었지만 최종화 미완료". 오류 문구에서 경로를 잘라내지 않는다(공백 있는 경로). 완전한 패키지로 다시 최종화하면 사라진다. 필드가 없어도 동작.

### 17.5 번들의 신뢰 문구 (U5)
- 신뢰 코드가 이 서버 API에서 온 데이터(99-loader `LIVE_TRUST_DATA` WeakSet — 번들 필드로는 주장 못 함)가 아니면 `trustFromBundle`(10-boot). 그때 "이 PC"가 들어간 신뢰 문구는 `<key>@bundle` 쌍둥이("생성 PC 기준 …": `trust.otherPc`, `status.otherPc`, `status.tip.unauth`, `trust.sc`, `audio.selfConsistent`; 20-i18n `trustI18nKey`), 서버 사유(`trust_note`, `logChains[].detail`)의 "이 PC"는 "생성 PC"로.
- 하네스: verify-review-batch --unit "R10 U1–U5", verify-audio-facet(`/trust` 번들 페이지 = 생성 PC 문구·P7·trust_note·logChains, `/live/` API 페이지 = 이 PC 문구·logChains 전달), verify-finalize-sheet 1(U2)·4c(U4, 두 테마 AA).

## 18. R11 트랙 U 추가 계약 (2026-10-16, docs/PLAN_R11_2026-10-16.md)

### 18.1 서버의 P7 표시 `trust_superseded_by` (U1)
- 서버는 더 새 쓸 수 없는 줄에 밀린 이전 줄을 `trust: "unauth"` + `trust_superseded_by`(더 새 줄의 1-기준 줄 번호) + `trust_note`("더 최근 인증 안 된 항목 있음 — …")로 보낸다. 30-records `logSlots`는 앞 줄이 쓸 수 있었을 때뿐 아니라 앞 줄(또는 그 항목 자신)에 `trust_superseded_by`가 있어도 현재 항목에 `ftNewerUnauth` → 기록 `statusNewer`("더 최근 인증 안 된 항목 있음", §17.1과 같은 표시). `logTrustSuperseded(item)`.

### 18.2 쓸 수 있는 항목의 note (U2)
- 쓸 수 있는 항목도 `trust_note`가 있으면(레거시 `auth` + "레인·케이스 결합 없음(레거시) …") 툴팁에만 보인다: `logTrustNote(item, true)`, 기록 상태 `statusNote`(배지 툴팁 `상태\nnote`), 상세 목록 `.trust-info`(`info` 아이콘, title·aria-label = note). 항목 아래 `.trust-note` 줄은 여전히 쓸 수 없는 항목만.

### 18.3 `_INCOMPLETE` 판정 (U3, P11)
- `packageIncomplete(dir, flag)`(10-core): 서버가 boolean `flag`를 보내면 그것이 판정. 없으면 `packageIncompleteName` — 서버 규칙(트랙 S 2a51c94)과 같게 폴더 이름 끝의 점·공백, ` - 복사본`, ` - Copy`(대소문자 무관), ` (n)`을 되풀이해 떼고 `/_INCOMPLETE(_\d+)?$/i`(`H_PKG_INCOMPLETE_RE`), 또는 `*.finalize-incomplete.txt`.
- `/api/recent-cases`는 패키지 힌트를 보내지 않는다(recent.rs `phase_hint`만) → 55-home `recentState`의 `finalize-incomplete`·`package_dir` 분기 제거. 연 케이스는 `/api/status`로 "최종화 미완료".

### 18.4 미인증 재검증 줄 · report_stale · package_incomplete (U4, 트랙 I2/S5/S6)
- `/api/verify-sources`(트랙 I dfa2f8a: `run_id`, `unauthenticated`, `verified`, `is_result`, `not_verified_reason`, `lane_status`): `verifySourcesVerdict`(41-finalize, legacy 70-finalize 동일) — 불일치 `err`가 먼저; `is_result: false` → `warn` "원본 재검증: 현재 결과 없음(결과 아님)"(+ 사유), 점검 목록 "현재 결과 없음"; `verified: false` 또는 `unauthenticated` > 0 → `warn` "확인 불가"(+ 사유·인증 안 된 줄 수). 초록 "모두 일치"는 `verified && is_result && mismatched == 0 && not_comparable == 0 && matched > 0`일 때만. 필드 없음(구 서버) = 수치만, 이전과 같음.
- 최종화 응답 `report_stale: true`: 결과 목록 첫 줄 `.h-fin-stale-row` "보고서 최신 아님 · 다시 최종화하십시오"(title = 서버 `error`); 성공 응답이면 판정 줄 "보고서·패키지를 만들었지만 보고서 최신 아님"·`warn` 토스트. 다른 케이스를 열면 지워진다.
- `/api/status` `package_incomplete`(boolean)는 `package_dir` 폴더에 대한 서버 판정 — 이름보다 우선, 없으면 이름(40-status `stPackageIncomplete`): 상점 phase `done`, `project.packageIncomplete`, 결과물 시트 패키지 줄(상태의 `package_dir`일 때만). 이전 버전이 남긴 폴더(`incomplete_packages`)용.
- 트랙 S(2a51c94) 최종화: 실패 응답은 `error`·`report_stale`·`finalize_incomplete`(boolean, 승격 실패는 패키지를 만들지 않으므로 경로 없음 — `incomplete_package_dir`/`url` 없음), 성공 응답도 `report_stale: false`. `finIncompleteOf`는 boolean `finalize_incomplete`가 있으면 그것만 믿는다(오류 문구 추측 안 함). `/api/status`의 `finalize_incomplete`·`finalize_incomplete_detail`·`report_stale`(메모리, 성공·케이스 전환 시 지워짐)은 시트에 이번 응답이 없을 때(새로 고침 뒤) "최종화 미완료"(title = detail)·"보고서 최신 아님" 줄로 보인다. 실패 시 `package_dir`은 그대로이므로 "결과물 생성됨"은 `phase`(`done`/`finalized`)로만. verify-finalize-sheet 4e.

### 18.5 카빙 페이지 지표 (U5, 트랙 I5)
- 카빙 페이지(`review/carve-report.html`)는 src/html_report.rs(트랙 I). verify-jobs-e2e 3b: 실제 카빙 뒤 `db/carve_results.json`을 위조(원본 크기·경고·스캔 완료)하고 `make-review`를 다시 돌려, 지표가 "감사 안 됨"(초록 `ok` 없음)이거나 위조 값이 아닌지 본다. I5(dfa2f8a: 네 지표 뒤 `<span class="unaudited">· 감사 안 됨</span>`) 없는 바이너리에서는 `PEND`, `FRAMETRACE_R11_STRICT=1`이면 실패.

### 18.6 그리드 재구성과 포커스 (PR #4 Windows CI)
- 그리드 재구성(`render()`·그룹 접기 → 40-grid `renderGridKeepFocus`)은 카드 노드를 바꾼다: 재구성 전에 포커스가 카드·그룹 머리·그리드에 있었고 뒤에 `<body>`로 떨어졌으면 같은 카드(없으면 그리드)로 되돌린다(`preventScroll`). 자동 저장 재시도·크기 변경 재렌더가 아무 때나 와도 키보드 포커스를 잃지 않는다. verify-viewer-dialogs "R11: a grid rebuild keeps keyboard focus".
- 하네스: verify-review-batch --unit "R11 U1–U4", verify-finalize-sheet 4d(U4).

## 19. R12 트랙 U 추가 계약 (2026-10-16, docs/PLAN_R12_2026-10-16.md)

### 19.1 `lane_status: "structural"` (U1)
- `/api/verify-sources`의 `lane_status === "structural"`(이 PC에 감사 키 없음)이면 `verifySourcesVerdict`(41-finalize, legacy 70-finalize 동일)는 초록이 아니라 `info`(`kind: "structural"`): 토스트 "원본 재검증: N건 일치 (자기 일관성만 — 인증 아님)", 점검 목록 "원본 재검증: 자기 일관성만 — 인증 아님"(`data-level="info"`). 불일치(`err`)·현재 결과 없음·확인 불가·비교 0건·비교 불가(`warn`)가 먼저. `keyed`이고 §18.4 조건을 만족할 때만 "모두 일치".

### 19.2 `finalize_incomplete_detail` (U2)
- `/api/status`의 "최종화 미완료" 줄(`.h-fin-incomplete-row`)은 detail을 툴팁뿐 아니라 줄 본문 `<small class="h-fin-detail">`에 짧게(공백 정리, 90자 넘으면 `…`; 41-finalize `finShortDetail`) 보인다. 전체 문구는 title.

### 19.3 최종화 저널 (U3, 트랙 S2 P13)
- `/api/status` `finalize_journal`(트랙 S 608884d): 이 PC의 최종화가 도는 동안 `null`, 그 밖에는 `{path, state, examiner, started_unix, signoff_sha256, error}`. 결과 목록 첫 줄 `.h-fin-journal-row`(41-finalize `finJournalState`/`finJournalRow`):
  - `state: "unreadable"`(다음 최종화를 막음) → `[data-journal="unreadable"]` "이전 최종화 기록을 읽지 못함 — 확인 필요", 본문 = `error`(짧게).
  - `"interrupted"`·`"in-progress"`(다른 프로세스가 쓰는 중이거나 아직 복구 안 됨) → `[data-journal="check"]` "이전 최종화 중단됨 — 확인 필요".
  - `"recovered"`, 또는 저널 없이 `finalize_recovered === true` → `[data-journal="recovered"]` "이전 최종화 중단됨 — 복구됨".
  - `finalize_recovered_detail`이 있으면 줄 본문 `.h-fin-detail`에 짧게(U2와 같음). "확인 필요:"가 들어 있으면 그 부분부터 보인다. title = 검토자 · SHA-256 · 시작 · 상태 · 경로 · 전체 detail. 필드 없음(구 서버) = 줄 없음. 복구가 세운 `report_stale`은 §18.4의 "보고서 최신 아님" 줄.
- `POST /api/finalize` 실패의 `incomplete_package_dirs: string[]`(이번 최종화가 게시·보관해 `…_INCOMPLETE`/옆 표식이 된 패키지): `finIncompleteOf` → `{package_dir: 첫째, dirs}`, "최종화 미완료" 줄에 폴더마다 경로·탐색기 버튼. 오류가 "최종화 차단:"으로 시작하면(아무것도 바꾸기 전 거부) 실패가 아니라 경고: 힌트에 문구 그대로("실패:" 없음) + `warn` 토스트(`finBlockedError`; legacy 70-finalize 같음).

### 19.4 수리된 레인 (U4, 트랙 I3)
- `logChains[lane].status === "repaired"`(`repair-lane`으로 찢긴 끝부분을 잘라냄) → `.log-chain` 줄 "수리됨(손상된 끝부분 잘라냄)"(i18n `chain.repaired`), 인증 안 된 줄이 있으면 " · 인증 안 된 줄 N개". title = 서버 `detail`. (이전에는 모르는 상태라 "인증 불가".)
- 트랙 I(cc4688c) 체인 객체의 `base_status`(수리를 빼고 본 상태)·`repaired: bool`·`repairs[]`: `repaired`면 기반 상태 문구를 덧붙인다(`structural` → "수리됨(…) · 자기 일관성만(인증 아님)", `keyed` → 수리됨만). 더 나쁜 상태(`failed`·`not-verified`·`unreadable`·인증 안 된 줄)가 이기고 `repaired: true`면 그 문구 뒤에 " · 수리됨"(i18n `chain.repairedShort`).

### 19.5 감사 안 된 카빙 후보 · 실행 id (트랙 I2/I4)
- 타임라인 패널(90-sync `timelineKindHtml`): kind `carved-candidate-unaudited`(db/carve_results.json에만 있는 카빙 결과)는 kind 단어 대신 `badge warn tl-unaudited` "후보(감사 안 됨)"(`timeline.kind.carvedUnaudited`, title `timeline.tip.carvedUnaudited`). 다른 kind는 기록된 그대로.
- `/api/verify-sources` `expected_run_id`(이번 요청이 시작한 실행): `is_result: false`면 `verifySourcesVerdict(res).tip` = "이번 실행 <expected_run_id> · 결과 실행 <run_id|없음>" — 앱 셸 점검 목록 줄의 title, legacy `#finalHint` title.
- 하네스: verify-review-batch --unit "R12 U1–U4", "R12 I2/I3/I4", "R12 S", verify-finalize-sheet 4f(U1–U3·I4·S 차단·미완료 폴더들, 두 테마 AA).

## 20. R13 트랙 U 추가 계약 (2026-10-16, docs/PLAN_R13_2026-10-16.md)

### 20.1 저널 상태 재설계 (U1, 트랙 S P15/P17)
- `/api/status` `finalize_journal.state` 추가값(41-finalize `finJournalState`, 머리말 `FIN_JOURNAL_HEAD`):
  - `"unpaired"`(서명 레인에 짝 맞는 begin 항목 없음 — 아무것도 바꾸지 않음) → `[data-journal="unpaired"]` "이전 최종화 기록 짝 없음 — 확인 필요", detail 없으면 본문 "아무것도 바뀌지 않았습니다". 점검 목록은 막지 않음(서버가 막으면 "최종화 차단:" 경고 — §19.3).
  - `"other-host"`(다른 PC의 작업, `host` 필드) → `[data-journal="otherhost"]` "다른 PC의 최종화 — 차단 중", 본문 = `host`(detail 없을 때), title에 "호스트 …". 점검 목록 `block` "다른 PC의 최종화 — 차단 중"(sub "<host>에서 끝난 뒤 다시 확인하십시오.") → 최종화 버튼 비활성.
  - `"recovering"`(시작 시 백그라운드 복구, busy) → `[data-journal="recovering"]` "이전 최종화 복구 중", 라벨은 경고가 아닌 `.h-fin-info`(`--info-bg`/`--info-fg`, 두 테마 AA). 점검 목록 첫 `block` "이전 최종화 복구 중"("다른 작업이 진행 중" 대신).
  - 그 밖의 recovered 아닌 값은 §19.3대로 "확인 필요".

### 20.2 더 새 미완료 실행의 불일치 (U2, 트랙 I1 P16)
- `/api/verify-sources` `withheld_reason: string|null`, `newer_mismatched: number`(`verifySourcesVerdict`, 41-finalize·legacy 70-finalize 같음): `mismatched` > 0(`err`)이 먼저, 그다음 `newer_mismatched` > 0이면 `is_result`와 무관하게 `err`, `kind: "newermismatch"`, 토스트 "원본 재검증: 표식 없는 더 새 실행 있음 — 불일치 N건", 점검 목록 줄 `label` "원본 재검증: 현재 결과 없음 — 불일치 N건"(sub에 "표식 없는 더 새 실행 있음 — 불일치 N건"). 숨기지 않는다. `is_result: false`의 사유는 `withheld_reason` · `not_verified_reason`(다를 때) 순서.
- 보관 이력서 요약(src/custody.rs)은 트랙 I 소유 — 뷰어는 "보관 이력서 5절"로 안내만.

### 20.3 읽을 수 없는 저널 치우기 (U3, 트랙 S6)
- `state: "unreadable"` 줄에 `<small class="h-fin-detail h-fin-cmd">치우기: <code>…</code></small>`: 서버가 `finalize_journal.dismiss_command`(문자열)를 보내면 그것, 없으면 ~~`frametrace finalize-journal --dismiss "<status.case_dir>"`(`finJournalDismissCmd`)~~ **(§25.5로 폐기 — 내장 명령 없음, 서버 필드가 null이면 줄 없음)**. 다른 상태엔 없음.

### 20.4 보관본 상태 (U4, 트랙 I5)
- `logChains[lane].removed_archive`(수리된 레인; 없으면 `repairs[].removed_archive` 중 가장 나쁜 것: differs > 알 수 없는 값 > missing > match) → 30-records `logChainArchiveText`: "보관본 일치"/"보관본 없음"/"보관본 다름"/"보관본 확인 안 됨"(i18n `chain.archive.*`, ko·en). "수리됨(…)" 바로 뒤, 더 나쁜 상태면 "… · 수리됨 · 보관본 …". 필드 없음(구 서버) = 덧붙이지 않음.
### 20.5 트랙 I(32e88a2) 후속
- `/api/verify-sources` `newer_runs`·`newer_records`·`newer_mismatches: [{run_id, run_unix, id, source_path, recorded_sha256, observed_sha256}]`: `newer_mismatched` 또는 목록이 있으면 위 `err` 판정에 `items`(최대 10개: id·source_path, title = 기록/관측 SHA-256·실행 id·시각)와 `more`(나머지 수). 앱 셸 점검 줄 아래 `<ul class="h-check-items">`(41-finalize `finCheckItemsHtml`, "외 N건 — 보관 이력서 5절"), legacy는 `#finalHint`에 줄마다(`white-space: pre-line`), 해시는 title. `withheld_reason`은 `; `로 여러 사유를 이을 수 있음 → 포함 검사, `not_verified_reason`과 겹치는 부분은 한 번만.
- 수리 뒤 규칙(트랙 I2)을 뷰어 최신값에도: 30-records `logSlots(list, keyOf, chain, source)` — `chain.repairs[].line` 또는 `source`(전체 레인, 줄 = 위치 + 1)의 사용 가능 `event:"lane-repair"` 항목 중 마지막 줄이 키의 최신 사용 가능 항목 줄보다 뒤면 그 항목 복사본에 비열거 `ftRepairedAfter` → `recordStatusOf`가 `status: "unknown-after-repair"`(`repairedAfter: true`) "최신 여부 알 수 없음"(i18n `status.repairedAfter`, 툴팁 `status.tip.repairedAfter`), 확인됨·파생으로 쓰지 않음. 검증(validationLog)·카빙(carveLog, 10-boot)·복구(filesystemLog, 걸러낸 목록이라 전체 레인을 `source`로)·이어붙임(joinLog) 레인 모두 자기 `logChains[lane]`을 넘긴다. 줄 위치를 모르는 항목은 수리 뒤로 보지 않음(닫힌 쪽).
- 하네스: verify-review-batch --unit "R13 I1", "R13 I2", verify-finalize-sheet 4g(불일치 행: 시트 안, 해시 title, 두 테마 AA).

### 20.6 트랙 S(c76a4df) 후속 — 저널 객체 전체
- 필드: `state`(`recovering`·`in-progress`·`interrupted`·`recovered`·`unpaired`·`other-host`·`completed`·`unreadable`), `examiner`, `started_unix`, `signoff_sha256`, `finalize_id`, `package_name`, `host_id`(32 hex), `host`(12 hex — 표시), `this_host`, `reason`, `error`, `blocks_finalize`, `blocked_until_unix`, `dismiss_command`(항상).
- 줄(`finJournalRow`): `completed` → `[data-journal="completed"]` "이전 최종화 완료 — 기록만 남음"(`.h-fin-info`, 차단 없음, 명령 없음); `in-progress` → `[data-journal="inprogress"]` "다른 FrameTrace에서 최종화 중"(§19.3의 "확인 필요"를 대신). 본문 = `reason`(있으면, 없으면 `finalize_recovered_detail`), unreadable은 `error`; `other-host`는 첫 줄 "PC <host> · <blocked_until_unix 시각>까지 차단". 치우기 명령(`dismiss_command`)은 `unreadable`·`unpaired`(R12가 남긴 저널도 unpaired) 줄에. title에 패키지 이름·최종화 id 추가.
- 점검 목록 차단 줄은 상태가 아니라 `blocks_finalize`(`finJournalBlocks`; 필드 없는 구 서버만 recovering·other-host로 대체): `recovering` → "이전 최종화 복구 중", 그 밖 → `block` "최종화 차단: <머리말>"(sub: other-host = 호스트·시각, unreadable/unpaired = "치우기: <명령>", 그 외 reason/detail 짧게).
- `finalize_recovered`는 실제로 정리했을 때만 true(서버) — 뷰어 규칙 그대로.
- 하네스: verify-review-batch --unit "R13 S journal …", verify-finalize-sheet 4g(completed info AA·차단 없음, 차단 끝난 other-host 차단 없음, interrupted + blocks_finalize 차단).
- 하네스: verify-review-batch --unit "R13 U1–U4", verify-finalize-sheet 4g(U1–U3, 두 테마 AA, 시트 안에 보임).

## 21. R14 트랙 U 추가 계약 (2026-10-16, docs/PLAN_R14_2026-10-16.md)

### 21.1 legacy 셸의 최종화 저널 (U1)
- legacy 70-finalize `gatherFinalizeChecks`가 `/api/status.finalize_journal`을 앱 셸과 같은 규칙(`finJournalState`·`finJournalBlocks`·`finJournalHead`, 머리말 `FIN_JOURNAL_HEAD` 동일)으로 읽는다(`finJournalChecks`): 막으면 `block` "최종화 차단: <머리말>"(recovering은 "이전 최종화 복구 중" — 이때 "다른 작업이 진행 중"은 빠짐), sub = other-host "PC <host> · <시각>까지 차단" · 사유(짧게) · unreadable/unpaired는 "치우기: <dismiss_command ~~또는 `frametrace finalize-journal --dismiss "<case_dir>"`~~>" **(내장 형식은 §25.5로 폐기 — dismiss_command가 null이면 이 sub 없음)**. 막지 않고 정보(완료·복구됨·복구 중)가 아니면 `warn` 줄(같은 머리말). 결과 목록은 legacy에 없음.

### 21.2 닫힌 저널의 `outcome` · 로컬 기록 없음 (U2, 트랙 S9/P18)
- `state: "completed"` + `outcome`: `completed`(또는 없음) → `[data-journal="completed"]` "이전 최종화 완료 — 기록만 남음"(info) · `recovered` → `closedrecovered` "이전 최종화 복구됨 — 기록만 남음"(info) · `failed` → `closedfailed` "이전 최종화 실패 — 기록만 남음"(경고 `.h-fin-incomplete`) · `dismissed` → `closeddismissed` "이전 최종화 치워짐 — 기록만 남음"(경고) · 그 밖 → `closed` "이전 최종화 끝남 — 확인 필요"(경고). 실패·치움은 "완료"라 쓰지 않는다. 차단 여부는 여전히 `blocks_finalize`.
- `state: "unpaired"`이고 `reason`에 "로컬 기록 없음"이 있으면 머리말 "이전 최종화 짝 없음(로컬 기록 없음) — 확인 필요"(결과 줄·점검 목록 둘 다). 본문 = `reason`, 치우기 명령 그대로.

### 21.3 더 새 실행 불일치는 차단 · 꼬리 잘림 (U3, 트랙 I3)
- `verifySourcesVerdict` `kind: "newermismatch"`(§20.2)는 점검 목록에서 `block`(라벨·sub·items 그대로) → 최종화 버튼 비활성. legacy는 이 페이지의 마지막 재검증 판정(`lastVerifyVerdict`, 케이스 바꾸면 비움)으로 같은 `block` 줄.
- `withheld_reason`/`not_verified_reason`에 "꼬리 잘림"(레인이 앵커된 head보다 짧음) → `err`, `kind: "truncated"`, 토스트 "원본 재검증: 기록 꼬리 잘림", 라벨 "원본 재검증: 기록 꼬리 잘림 — 현재 결과 없음", hint에 사유. 더 새 실행 불일치가 있으면 그쪽이 먼저(사유는 hint에). 점검 목록은 `warn`.

### 21.4 수리 뒤 다시 검증 (U4)
- `logSlots`: 레인의 마지막 수리 줄보다 뒤에 있는 항목은 그대로 현재값(확인됨). 그 앞 항목은 §20.5대로 "최신 여부 알 수 없음"이고 마지막 수리 시각(`repairs[].logged_unix` 또는 표식 항목의 `logged_unix`)을 비열거 `ftRepairedUnix`로 갖는다.
- `recordStatusOf(own, valid, unvalid, fallback, recheck)`: own이 수리 앞이어도 그 파일의 사용 가능 최신 검증(`valid`, 이어붙임은 `recheck` = 출력 경로의 검증)이 수리 뒤(`validated_unix` > 수리 시각, 같은 초는 아님)에 기록됐으면 다시 확인된 것 — 카빙·복구는 검증 상태, 이어붙임은 "derived". 수리 시각을 모르거나 검증이 수리 전·인증 안 됨이면 "최신 여부 알 수 없음" 그대로.

### 21.5 시각 감사 안 된 타임라인 항목 (U5, 트랙 I8)
- 타임라인 항목 `time_unaudited: true` 또는 kind가 `…-time-unaudited`로 끝나면 kind 단어(접미 뺀 것) 뒤 `badge warn tl-time-unaudited` "시각 감사 안 됨"(`timeline.badge.timeUnaudited`, title `timeline.tip.timeUnaudited`, ko·en).
- 하네스: verify-review-batch --unit "R14 U1"–"R14 U5"(40126c3 자산에서 각각 실패 확인).

### 21.6 트랙 I(3e002d8) 후속
- `/api/verify-sources` `head_status`(absent|intact|truncated|rewritten|unknown|not-authenticated): `verifySourcesVerdict`(두 셸)가 모든 판정의 tip에 " · 재검증 머리 <head_status>"를 덧붙인다(필드 없음 = 안 붙임; 앱 셸 점검 줄 title, legacy `#finalHint` title). `truncated`/`rewritten` 또는 사유 "꼬리 잘림" → §21.3 `err`(`kind: "truncated"`). `unknown` 또는 사유 "머리 확인 불가" → `warn`, `kind: "headunknown"`, 라벨·토스트 "원본 재검증: 머리 확인 불가 — 현재 결과 없음", hint에 사유. 순서: 불일치 > 더 새 실행 불일치 > 꼬리 잘림 > 머리 확인 불가 > 현재 결과 없음.
- 타임라인: `time_unaudited`가 boolean이면 그것만으로 배지(§21.5의 kind 접미는 필드가 없을 때만).
- 소리: `audioLevels[id]`가 `r: "unverified"`/`"replayed"`이고 `note`(없으면 `error`) 문자열이 있으면 `record.audioNote` → 그리드 소리 툴팁 다음 줄, 상세 " · <사유>". (서버의 압축 JSON `AudioLevelState::to_json`은 아직 이유를 보내지 않음 — 보내면 바로 표시.)
- 뷰어는 이상 징후·재검증의 "현재 실행"을 스스로 판정하지 않는다: 이상 징후는 selector별 기록된 소견을 모두 나열(현재 결과라고 주장하지 않음), 재검증 판정은 서버 요약(`is_result`·`withheld_reason`·`newer_*`)만 따른다 — P20 구간 규칙은 서버 몫.
- 하네스: verify-review-batch --unit "R14 track I follow-up"(5b77c07 자산에서 항목별 실패 확인).

### 21.7 트랙 S(0608d98) 후속 — 저널 추가 필드
- `state: "completed-unrecorded"` → `[data-journal="unrecorded"]` "이전 최종화 완료 — 완료 기록만 빠짐"(info `.h-fin-info`), 본문 = `reason`. 차단은 `blocks_finalize`(false). legacy는 info라 줄 없음.
- `outcome` 추가값: `refused` → `closedrefused` "이전 최종화 거부됨 — 기록만 남음", `superseded` → `closedsuperseded` "이전 최종화 대체됨 — 기록만 남음"(둘 다 경고). 나머지는 §21.2.
- `unpaired_code` → 머리말 "이전 최종화 짝 없음(<짧은 원인>) — 확인 필요": no-local-record "로컬 기록 없음" · local-mismatch "로컬 기록 불일치" · local-unreadable "로컬 기록 읽지 못함" · lane "서명 레인 확인 불가" · journal-mismatch "저널 불일치" · legacy-journal "이전 형식 기록" · host-unknown "이 PC 식별 불가"(`FIN_UNPAIRED_SHORT`). 코드 없고 사유에 "로컬 기록 없음"이면 no-local-record, 그 밖엔 "이전 최종화 기록 짝 없음 — 확인 필요".
- `local_record`(true/false) → title "이 PC의 로컬 기록 있음/없음"; `unpaired_code` → title "짝 없음 코드 <code>".
- 치우기 명령(`finJournalDismissLines`, unreadable·unpaired·other-host만): `dismiss_command`(PowerShell, 작은따옴표)와 `dismiss_command_cmd`(cmd.exe)가 둘 다 있으면 두 줄 "치우기(PowerShell): …" / "치우기(명령 프롬프트): …", 하나면 "치우기: …", 둘 다 null이면 unreadable·unpaired만 내장 형식(§20.3). 점검 목록 sub는 같은 줄들을 " · "로. legacy 같음.
- 하네스: verify-review-batch --unit "R14 track S follow-up"(이전 커밋 자산에서 실패 확인).

## 22. R15 트랙 U 추가 계약 (2026-10-16, docs/PLAN_R15_2026-10-16.md)

### 22.1 수리 뒤 다시 확인은 같은 바이트의 확인 상태만 (U1)
- §21.4를 좁힌다. 수리 앞 own 항목(카빙·복구·이어붙임)은 그 파일의 사용 가능 최신 검증이 수리 뒤이고 **`validation_status`가 `ffprobe-confirmed`/`ffprobe-video-stream-confirmed`이며 `target_sha256`이 own의 SHA-256**(이어붙임 `output_sha256`, 카빙·복구 `sha256`; 대소문자 무시, 둘 다 있어야 함)일 때만 다시 확인된다 — 카빙·복구는 검증 상태, 이어붙임은 "derived".
- 수리 뒤 검증이 확인 상태가 아니면(`validation-failed`, `candidate-unvalidated` 등) 그 검증 상태를 그대로 보인다(이어붙임도 "derived"로 떨어지지 않음). 확인 상태지만 SHA가 다르거나 없으면, 또는 own SHA를 모르면 "최신 여부 알 수 없음" 그대로.

### 22.2 위치가 시각보다 먼저 (U2)
- `logSlots`는 사용 가능 항목마다 (레인 배열, 1부터 줄 번호)를 옆에(`logPosOf`, WeakMap — 객체에 쓰지 않음) 두고, 수리 앞 항목 사본에 비열거 `ftRepairedLine`(마지막 수리 줄)을 단다.
- "수리 뒤" 판정: 검증과 수리가 **같은 레인**이면 줄 번호(`줄 > 수리 줄`)만 본다(시각 무시). 다른 레인(보통: own은 carve/filesystem/join 레인, 검증은 validation 레인)은 공통 위치가 없어 §21.4의 시각 비교(`validated_unix` > 수리 시각, 같은 초 아님)가 남는다.

### 22.3 내장 치우기 명령은 작은따옴표 (U3)
- **(§25.5로 폐기 — 아래 내장 형식은 더 이상 없음; 서버 명령이 null이면 줄 없음)** ~~서버 명령이 둘 다 null일 때의 내장 형식(두 셸 `finJournalDismissCmd`):~~ `frametrace finalize-journal '<case_dir>' --dismiss` — Windows 경로(드라이브 문자 또는 `\`)는 `'`→`''`(PowerShell), 그 밖은 `'\''`(POSIX). case_dir 없으면 `frametrace finalize-journal --dismiss`. (§20.3·§21.1의 큰따옴표 형식을 대신한다.)

### 22.4 문구 (U4, 트랙 S6·I4·I5)
- `state: "completed-unrecorded"` + `blocks_finalize: true` → `[data-journal="unrecordedblocked"]` "이전 최종화 완료 — 완료 기록 못 씀"(경고 `.h-fin-incomplete`, 본문 = `reason`), 점검 목록 `block` "최종화 차단: 이전 최종화 완료 — 완료 기록 못 씀"(legacy 같음). `blocks_finalize`가 없거나 false면 §21.7 그대로(info).
- `/api/verify-sources` 사유(`withheld_reason`/`not_verified_reason`, 포함 검사)에 **"다른 PC 머리일 수 있음"** → `warn`, `kind: "headotherpc"`, 라벨·토스트 "원본 재검증: 다른 PC 머리일 수 있음 — 현재 결과 없음", hint에 사유 — `head_status`가 unknown·rewritten·not-authenticated여도 err 아님. 그 문구가 없는 사유 조각에 "꼬리 잘림"이 있거나 `head_status: "truncated"`면 §21.3 err 그대로.
- 사유에 **"시계 이상"** → `warn`, `kind: "clock"`, 라벨 "원본 재검증: 시계 이상 — 확인 필요"(`is_result: false`면 "— 현재 결과 없음"), hint에 사유. 초록·자기 일관성(info)보다 먼저. 순서: 불일치 > 더 새 실행 불일치 > 꼬리 잘림 > 다른 PC 머리 > 머리 확인 불가 > 시계 이상 > 현재 결과 없음.
- 하네스: verify-review-batch --unit "R15 U1"–"R15 U4"(7c3955e 자산에서 각각 실패 확인).

### 22.5 트랙 I(db2bcb3) 후속
- `/api/verify-sources` `head_reason`(head_status unknown일 때): tip "재검증 머리 unknown (<짧은 말>)" — other-pc "다른 PC 머리일 수 있음" · unusable-line "쓸 수 없는 줄" · chain-failed "체인 실패" · repaired "레인 수리" · malformed "형식 오류" · own-head-missing "결과 실행 머리 없음" · unreadable "읽지 못함"(그 밖 코드는 그대로). `head_reason: "other-pc"`면 사유 문구가 없어도 §22.4 `headotherpc`(warn).
- `clock_note`("시계 이상…")는 보류가 아니다: 판정(초록 포함)은 그대로, 판정에 `clock` = 문장. 앱 셸 점검 목록에 따로 `warn` "원본 재검증: 시계 이상"(sub = 짧게, title = 전체), legacy 점검 목록에 같은 `warn` 줄 + `#finalHint` 다음 줄. §22.4의 "사유에 시계 이상 → clock" 규칙은 없앤다(보류 사유 속 "시계 이상" 문장은 hint에만).
- 사유에 "완료 실행 구간 겹침" → `warn`, `kind: "overlap"`, 라벨·토스트 "원본 재검증: 완료 실행 겹침 — 현재 결과 없음", hint에 사유. 순서: … > 머리 확인 불가 > 완료 실행 겹침 > 현재 결과 없음.
- 타임라인 패널: `time_unaudited`인 연속 행(같은 source·같은 kind)은 첫 행에만 배지 — 2행 이상이면 "시각 감사 안 됨 (N건 연속)"(`timeline.badge.timeUnauditedN`, ko·en), 뒤 행은 배지 없이 행 title에 `timeline.tip.timeUnaudited`. `timelineKindHtml(kind, ev, group)`의 group 없으면 행마다 배지(이전 그대로).
- 하네스: verify-review-batch --unit "R15 track I follow-up", "R15 I8"(972eb41 자산에서 실패 확인).

### 22.6 트랙 S(8b523ba) 후속 — 저널 필드
- 치우기 명령: `dismiss_command`가 **있고 null**이면 명령 없음(일부러 안내하지 않음 — other-account 등). 내장 작은따옴표 형식(§22.3)은 필드 자체가 **없을 때**(옛 서버)만. 점검 목록 sub는 명령이 없으면 사유(짧게). 두 셸 같음.
- `state: "in-progress"` + `in_progress_code: "other-account"` → `[data-journal="otheraccount"]` "다른 계정의 최종화일 수 있음 — 차단 중"(경고), 본문 = `blocked_until_unix` "…까지 차단" · `reason`, 치우기 줄 없음. 점검 목록 `block` "최종화 차단: 다른 계정의 최종화일 수 있음 — 차단 중"(legacy 같음). `blocks_finalize`가 없어도 막음. `live-writer`(또는 코드 없음)는 §20 `inprogress` 그대로.
- `unpaired_code` 추가: package-changed "패키지 변경됨" · local-state "로컬 기록 상태 불일치"(`FIN_UNPAIRED_SHORT`).
- title에 `local_record_state` "로컬 기록 상태 <값>", `in_progress_code` "진행 중 코드 <값>".
- 하네스: verify-review-batch --unit "R15 track S follow-up"(이전 커밋 자산에서 실패 확인).

## 23. R16 트랙 U 추가 계약 (2026-10-18, docs/PLAN_R16_2026-10-18.md)

### 23.1 이상 징후 기록 머리 (U1, 트랙 I 4c95f30)
- records-meta `logChains.anomaly.head`(없으면 `logChains.anomalyLog.head`) = {status, reason_code, withhold_reason, detail}(보고서 `auditChains.anomaly.head`와 같음). `truncated`/`rewritten` → 알림 줄 `#anomalyHeadBanner`(role=alert) "이상 징후 기록 꼬리 잘림 — 현재 결과 없음". unknown·그 밖의 값, 또는 `withhold_reason`이 문자열(상태와 무관) → role=status "이상 징후 기록 머리 확인 불가 — 현재 결과 없음"(reason_code mixed-version "이상 징후 기록: 이전 버전이 스캔함 — 이 PC에서 다시 스캔하면 해소" — 변조로 쓰지 않음 · scan-mismatch "이상 징후 기록: 다른 스캔의 머리 — 현재 결과 없음"). 이 두 경우 모든 이상 후보 배지에 " · 확인 불가"(`.anomaly-unverified`, 그리드·상세). `not-authenticated`(보류 없음) → role=status 참고 줄 "이상 징후 기록 머리 인증 안 됨 — 참고"(배지 그대로). 없음·`absent`·`intact`(보류 없음) → 아무것도 없음. title = 사유 문장·status·reason_code·withhold_reason·detail (`anomalyHead.*`, ko·en).

### 23.2 레인 간 위치 비교 (U2)
- 줄 번호는 같은 레인 안에서만 순서다. 서버가 레인 간 순서를 주지 않는 한 다른 레인 사이의 "수리 뒤" 판정은 §22.2의 시각 비교로 남는다(뷰어가 레인 간 위치를 만들지 않음).

### 23.3 문구 (U3·U4, 트랙 S 2e3a4e4·I)
- 타임라인 행: kind와 파일 이름 사이 " — "(양쪽 공백).
- `in_progress_code: "unverified-writer"` → `[data-journal="unverifiedwriter"]` "확인 불가 작성자 — 자동 복구 안 함"(경고), 본문 = 차단 시각 · `reason`(문장). 점검 목록 `block` "최종화 차단: 확인 불가 작성자 — 자동 복구 안 함". `blocks_finalize`가 없어도 막음. 치우기 줄 없음(서버 null). legacy 같음.
- `--force` 안내 줄(`finJournalForceLines`)은 서버 `force_dismiss_command`(PowerShell)·`force_dismiss_command_cmd`(cmd.exe) 문자열에서만: 둘 다 → "확인 뒤 강제 치우기(PowerShell): …" / "(명령 프롬프트): …", 하나 → "확인 뒤 강제 치우기: …", 없거나 null → 줄 없음(뷰어가 만들지 않음). 결과 줄과 점검 목록 sub(앱 셸·legacy) 모두. 일반 "치우기:" 줄은 §22.6 그대로.
- `state: "completed"`도 `blocks_finalize: true`면 막는다(필드가 결정): 점검 목록 `block`, 결과 줄은 info가 아니라 경고 표시.
- `head_reason` 추가: mixed-version "이전 버전이 스캔함 — 이 PC에서 다시 스캔하면 해소"(변조 아님) · scan-mismatch "다른 스캔의 머리"(§22.5 tip 형식).

### 23.4 하네스 (U5)
- verify-range-sync ㉓: 편집(keydown)과 `FTViewer.save` 호출을 한 페이지 작업에서 한다. 실제 키 입력 뒤 하네스가 2.5 s 이상 멈추면 자동 저장(디바운스)이 먼저 보내 save가 그 요청을 기다렸다(시험 시각 문제, 저장 경로는 정상). 호출 시점 상태 `pending`도 확인.
- 하네스: verify-review-batch --unit "R16 U1"·"R16 U3"·"R16 U4"(82a6cb1 자산, 후속은 첫 R16 U 커밋 자산에서 각각 실패 확인).

## 24. R17 트랙 U 추가 계약 (2026-10-19, docs/PLAN_R17_2026-10-18.md)

### 24.1 표식 사용 불가 (U1, 트랙 I1)
- 이상 징후 머리 `reason_code: "marker-unusable"` → 경고(`warn`, role=status) "이상 징후 기록: 표식 확인 불가 — 변조 가능성, 현재 결과 없음"(`anomalyHead.reason.markerUnusable`, ko·en). 상태가 `not-authenticated`·`intact`여도 참고(note)·없음으로 떨어지지 않고 경고 + 배지 "확인 불가". "다시 스캔하면 해소"는 `mixed-version`만.
- `marker-unusable`은 이상 징후 머리에만 온다(records-meta·보고서·번들; `/api/verify-sources`에는 없음). `withhold_reason`은 "이상 징후 머리 확인 불가 — 결과 없음: 완료 표식을 쓸 수 없음 — …"(tooltip).

### 24.2 이상 후보 개수·칩 (U2)
- 이상 후보(`record.hasAnomaly`)는 `trust`가 auth·sc인 이상 징후 행(필드 없음 = 옛 서버·번들, 이전 그대로)이 하나라도 있는 기록만. 행은 있으나 모두 그 밖(unauth·`pc:<키>`)이면 `hasUnauthAnomaly` — "인증 안 된 후보"로 따로 센다(배지 `.badge.candidate.anomaly-unauth` "인증 안 된 후보", 개수 알약 tooltip "인증 안 된 후보 N건(따로 셈)", 더보기 칩 `anomaly:unauth` "인증 안 된 후보 N" — N>0일 때만). 검증 기록의 `anomaly_flags`는 그 검증 줄의 `trust`를 따르고, 같은 kind 두 행 중 쓸 수 있는 쪽이 남는다(`recordAnomalies`).
- 머리가 보류(§23.1 err·warn)면 알약 "이상 N · 확인 불가"(`.anomaly-unverified`, tooltip에 사유), 칩 "이상 징후 후보 · 확인 불가"(활성 필터 줄 같음).

### 24.3 강제 치우기·작성자 없음 복구 (U3·U5, 트랙 S8·S2)
- `in_progress_code: "live-writer"`(검증된 작성자)는 `--force` 줄 없음(서버 null; 옛 서버 문자열도 보이지 않음; 그 `reason`은 기다리라는 안내뿐). 그 밖에 `force_dismiss_command(_cmd)`가 있으면 마지막 줄 옆에 고정 문구 "저널만 옮김 — 아무것도 되돌리거나 기록하지 않음(서명·패키지 그대로)"(`FIN_FORCE_NOTE`; 결과 줄 `.h-fin-note`, 점검 목록 sub는 괄호).
- `*_cmd`(cmd.exe 형식)는 `%`가 든 경로면 null — PowerShell 줄 하나만("…: …", 라벨 괄호 없음).
- `manifest_check: "withheld"`(`completed-unrecorded`·`completed`) → `[data-journal="unrecordedwithheld"]` "이전 최종화 확인 보류 — 완료 확정 아님"(경고, info 아님; 차단은 `blocks_finalize` 그대로 — 서버 false). legacy 점검 목록 `warn` 같은 문구.
- `writer_gone_command`(PowerShell)·`writer_gone_command_cmd`(cmd.exe) → "작성자 없음 확인 시 복구(PowerShell): …"/"(명령 프롬프트): …", 하나면 "작성자 없음 확인 시 복구: …". 서버 필드에서만(뷰어가 만들지 않음; 서버는 unverified-writer·other-account에만 보냄). 순서: 치우기 → 작성자 없음 복구 → 강제 치우기. 결과 줄·점검 목록(앱 셸·legacy) 모두.

### 24.4 번들 머리 (U4, 트랙 I6)
- 독립 번들의 `logChains.anomaly.head`도 §23.1과 같은 배너·배지. 번들에서는 사유·detail의 "이 PC"를 "생성 PC"로(`anomalyHead.reason.mixedVersion@bundle` "… 생성 PC에서 다시 스캔하면 해소").

### 24.5 트랙 I(7095fce) 후속
- 타임라인 행 `time_self_consistent: true`(없으면 false) → kind 옆 약한 표시 "자기 일관성만"(`.badge.candidate.tl-time-sc`, tooltip `timeline.tip.timeSc`, ko·en) — "시각 감사 안 됨"과 구별, 같이 붙지 않음(`time_unaudited` 우선).
- `head_reason: "unusable-line"`은 코드 그대로 "쓸 수 없는 줄"(서버 문장 "모양상 다른 이벤트이나 내용은 인증 안 됨 — …"은 사유·hint에 그대로).
- 하네스: verify-review-batch --unit "R17 U1"–"R17 U5"(9bac14c 자산), "R17 track I follow-up"(7095fce 자산에서 실패 확인); verify-layout "R17 U1/U2"(1366×768, records-meta 주입).

## 25. R18 트랙 U 추가 계약 (2026-10-19, docs/PLAN_R18_2026-10-19.md)

### 25.1 이상 후보는 안전한 쪽으로 센다 (U1, P30 — §24.2를 대신함)
- 이상 징후 발견 행(`kind` ≠ none)이 하나라도 있는 기록은 `trust`와 무관하게 이상 후보(`record.hasAnomaly`). 인증 안 된 줄·대체된 줄이 기록을 깨끗하게 보이게 하지 않는다.
- 검증 `anomaly_flags`(R19 U1, P33 — §24.2 "그 검증 줄의 trust"를 대신함): 경로의 현재 쓸 수 있는 검증 줄 플래그는 그 줄의 trust. 그 밖의 모든 검증 줄(인증 안 된 최신 줄, 대체된 이전 줄 — 인증됐던 줄 포함, 레인 수리 전 줄)의 플래그도 남아 "인증 안 됨"으로 센다(`pc:<키>` 줄은 "다른 PC 키"). 같은 kind는 한 번만, 쓸 수 있는 행이 있으면 그쪽(`validationFlagRows` → `record.validationFlags` → `recordAnomalies`). 하네스: verify-review-batch --unit "R19 U1"(16f4c67 자산에서 실패 확인).
- 쓸 수 있는 행(auth·sc·필드 없음)이 하나도 없는 기록만 표시를 구분: 그중 `pc:<키>`가 아닌 행이 있으면 `hasUnauthAnomaly`("인증 안 됨"), 모두 `pc:`면 `hasPcAnomaly`("다른 PC 키", §25.2).
- 알약: "이상 N (인증 안 됨 M · 다른 PC 키 K)" — 괄호 안 각 부분은 0보다 클 때만; 보류 중이면 뒤에 " · 확인 불가". tooltip "인증 안 됨 M건 포함(표시만 구분)".
- 배지: `.badge.anomaly` "이상 후보" + " · 인증 안 됨"(`.anomaly-unauth`) 또는 " · 다른 PC 키"(`.anomaly-pc`) + " · 확인 불가"(`.anomaly-unverified`). 상세 이상 징후 줄도 같은 표시.
- 칩: `anomaly`(이상 징후 후보) 필터는 모든 이상 후보. `anomaly:unauth` "인증 안 된 후보 M"은 M>0이면 레일에 늘 보임(더보기 뒤가 아님). `anomaly:pc` "다른 PC 키 후보 K"는 K>0일 때만(더보기).
- 서버(트랙 I2) records-meta `logChains.anomaly.withheld`(또는 `anomalyLog.withheld`) = `{reason}`: null·false·없음이면 무시, 그 밖(빈 객체 포함)은 보류 — 머리가 intact여도 경고(`warn`): 알약·칩·배지 "확인 불가", 배너 `#anomalyHeadBanner`(role=status) "이상 징후 확인 불가 — {reason}"(`anomalyHead.withheld`; 사유 없으면 `anomalyHead.unknown`). 순서: 머리 err(꼬리 잘림) > withheld > reason_code 문구. tooltip 첫 줄 = reason.
- 트랙 I(5a75e30) 모양 `{reason_code, codes[], reason}`: 배너 {reason}은 `codes`가 모두 아는 코드면 짧은 말(`anomalyHead.held.*`, " · "로 이음) — unusable-after "완료 표식 뒤 인증 안 된 줄" · unusable-in-run "최근 실행 안 인증 안 된 줄" · repaired-after "레인 수리 뒤 — 다시 검사" · newer-unmarked-run "표식 없는 더 새 실행" · unreadable "레인 읽지 못함" · bundle-unreadable "번들 머리 읽지 못함 — make-review를 다시 실행"; 모르는 코드가 있으면 서버 문장. 전체 문장은 배너 title. `head.withhold_reason`만 있어도 보류(§23.1 그대로) — 둘 중 하나면 "확인 불가".
- 머리 `reason_code: "bundle-unreadable"`(번들만) → "이상 징후 기록: 번들 머리 읽지 못함 — make-review를 다시 실행"(`anomalyHead.reason.bundleUnreadable`; 다시 스캔 안내 아님). `marker-unusable`은 §24.1 그대로. unusable-line 문장의 "(적힌 event: …)"에 기대지 않는다.

### 25.2 다른 PC 키 (U2)
- `pc:` trust만 있는 이상 후보는 "인증 안 됨"으로 묶지 않는다: "다른 PC 키 — 키 보유 PC에서 확인"(`anomaly.pcTip`, ko·en).

### 25.3 명령 없음 (U3, 트랙 S1 P29)
- `finalize_journal.command_unavailable_reason`이 있으면(null·false 아님) 치우기·작성자 없음 복구·강제 치우기 줄을 하나도 보이지 않고 사유를 보인다: 비ASCII 문장이면 그대로, 코드면 고정 문구 "경로에 특수 문자 — 명령을 직접 입력하십시오"(`FIN_NO_COMMAND`). 결과 줄 `.h-fin-detail`, 점검 목록 sub(앱 셸·legacy).

### 25.4 차단 여부 모름 (U4, 트랙 S2 P31)
- `blocks_finalize: null`(필드가 있고 null)이면서 다른 규칙으로 막지 않는 저널 → 차단 가능: 결과 줄은 경고(info 아님, `data-maybe-blocks="true"`) + "확인 필요 — 최종화가 거부될 수 있음"(`FIN_MAYBE_BLOCKS`), 점검 목록 `warn`(두 셸). 버튼은 막지 않는다(서버가 거부한다).
- 기억된 복구 판정: `finalize_journal.recovery_verdict`(문자열 또는 `{text}`/`{reason}`)를 결과 줄·점검 목록 sub에 짧게. 트랙 S(a9d26a3)는 따로 필드를 보내지 않고 `manifest_check: "recovery-verdict"` + `reason`(판정 문장)으로 보낸다 → 본문 = `reason`, 덧줄 "기억된 판정(복구가 전체 해시로 확인)"(`FIN_VERDICT_LABEL`); "확인 보류"(§24.3 `unrecordedwithheld`)는 `"withheld"`일 때만. completed-unrecorded(+false)는 info 그대로, unpaired/package-changed(+true)는 차단.
- `null`은 `finJournalBlocks`/`finJournalMaybeBlocks`로만 읽는다(두 셸 어디에도 `if (blocks_finalize)` 같은 참거짓 검사 없음).

### 25.5 명령은 서버 문자열만 (U5, 트랙 S7)
- 뷰어·셸은 명령을 만들지 않는다. §22.3의 내장 작은따옴표 형식(`finJournalDismissCmd`/`finJournalDismissQuote`)은 없앴다 — `’` 같은 따옴표 문자를 막지 못했다. 서버 필드가 없거나 null이면 줄 없음. `--i-am-sure`는 서버가 보낸 명령 문자열 안에서만 나온다(그대로 표시). `writer_gone_i_am_sure: true`면 작성자 없음 복구 마지막 줄 옆에 경고 "이 케이스의 진행 중 최종화가 아닌지 확인"(`FIN_SURE_NOTE`; 결과 줄 `.h-fin-note`, 점검 목록 sub는 괄호). other-account·최종화 거부 문장은 키로 쓰지 않는다.
- 하네스: verify-review-batch --unit "R18 U1"–"R18 U5"(ff898f5 자산에서 각각 실패 확인), verify-layout "R18 U1/U2"(1366×768, records-meta 주입: auth·unauth·pc 3행 + withheld).

## 26. R20 트랙 U 추가 계약 (2026-10-19, docs/PLAN_R20_2026-10-19.md)

- **U1** (§25.1 검증 `anomaly_flags` 보충): 현재 아닌 검증 줄의 플래그 행은 그 줄의 trust를 그대로 둔다(`auth`도). 경로의 마지막 줄이 아니거나 `trust_superseded_by`가 있으면 `superseded: true` → 상세 표시 "대체됨"(tooltip "대체된 이전 검증 — 현재 아님", `anomaly.superseded`/`anomaly.tip.superseded`) — "이 PC의 키로 인증되지 않아" 문구가 아니다. 마지막 줄인데 레인 수리 전이면 `repairedAfter: true` → "최신 여부 알 수 없음"(`status.repairedAfter`). 두 표식이 있는 행은 trust와 무관하게 쓸 수 없는 행으로 센다(fail closed, `hasUnauthAnomaly` — `pc:` 줄은 `hasPcAnomaly`).
- **U2**: 플래그 행 설명은 i18n — `anomaly.validationFlag`(현재 줄)·`anomaly.validationFlagOld` "검증 anomaly_flags (현재 아님)"(ko·en).
- **U3**: CSV(`csvCell`) 셀이 `= + - @ 탭 CR`로 시작하면 앞에 `'`를 붙인 뒤 따옴표 규칙을 적용한다(수식 주입). 수(`-12.3`, `+1`)와 자리 표시 `-`는 그대로.
- **U4**: legacy 셸 `normPath`도 `E:\.` → `e:`(앱 셸 30-form과 같은 치환).
- **U5**: 케이스 이름(`hBaseName`·`caseBaseName`)은 끝의 `\.`·구분자를 떼고 고른다 — `E:\.` → "E:", 절대 "."이 아님. 최근 목록은 `hCaseName(name, dir)`: 서버 이름이 "."·".."이면 폴더에서 다시 고른다.
- **U6**: case_id 없는 케이스의 초안은 시작 시(`hydrateAnnotations`, 초안 읽기 전) 옛 키 후보(`FTViewer.caseStorageKeyAliases` — 끝 `\.`, 드라이브 문자 대소문자, 겹친 구분자)의 `ft.draft.<옛 키>.*`를 새 키로 옮긴다(`migrateAliasDrafts`). 새 키에 쓰고 다시 읽어 같을 때만 옛 항목을 지운다; 새 키에 다른 값이 있거나 쓰기가 실패하면 둘 다 남는다(손실 없음).
- **트랙 I(b9067ba)**: 머리 `reason_code: "lane-changed"`(status unknown, withhold_reason 있음) → 경고(확인 불가) "이상 징후 기록이 읽는 중 바뀜 — 다시 확인"(`anomalyHead.reason.laneChanged`; 변조 판정 아님). 보류 코드로 오면 "읽는 중 바뀜 — 다시 확인"(`anomalyHead.held.laneChanged`). `anomalyLog`의 `scan_id`는 뷰어가 쓰지 않는다.
- 하네스: verify-review-batch --unit "R20 U1"–"R20 U6", "R20 track I lane-changed"(1593a61 자산에서 각각 실패 확인).

## 27. R21 트랙 U 추가 계약 (2026-10-20, docs/PLAN_R21_2026-10-20.md)

- **U1** (§26 U1 보충): 현재 아닌 검증 줄의 플래그 행은 경로의 마지막 줄이 쓸 수 있는 줄(auth·sc)일 때만 `superseded`("대체됨"). 마지막 줄이 쓸 수 없으면(키 없는 `anomaly_flags: []` 줄 포함, `trust_superseded_by`가 있어도) `newerUnauth: true` → 배지 "인증 안 됨"(행 trust가 `pc:`면 "다른 PC 키"), tooltip "더 최근 인증 안 된 항목 있음"(`status.tip.newer`). 쓸 수 없는 행으로 센다.
- **U2**: 케이스 이름의 끝 구분자·`\.` 제거는 중첩 없는 정규식 `/[\\/](?:\.?[\\/])*\.?$/`(`hBaseName`·`caseBaseName`). 결과는 이전과 같다.
- **U3**: `csvCell`은 `;`가 든 셀도 따옴표로 감싼다(`;` 구분 로캘).
- **U4**: 상세 요약 이상 징후 줄의 괄호 말(`anomalySummaryNote`): 모든 행이 `superseded`/`repairedAfter`면 "현재 아님"(`anomaly.notCurrent`) 또는(하나라도 repairedAfter) "최신 여부 알 수 없음". 그 밖은 "인증 안 됨"/"다른 PC 키 — 키 보유 PC에서 확인" 그대로. 카운트·배지·칩은 그대로.
- **U5**: 별칭 이전(`migrateAliasDrafts`)이 옛 키의 `ft.viewer.<옛 키>.annotations.v1`(`migrateLegacyDrafts(key)`), `.examiner`(새 키에 없을 때만; 다른 이름이면 둘 다 남음), `.ranges`(`migrateAliasRanges`, 초안 채택 뒤)도 옮긴다. 새 키에 다른 값이 있어 옮기지 못한 초안·annotations.v1 항목은 예비 키 `ft.draft.<키>.legacy-<n>.<id>`(닫힌 창, `writerVerdict` "closed")에 써서 닫힌 창의 편집(A7)으로 보인다. 옛 `.ranges` 값이 다른 범위의 초안 아래 있으면 예비 키 + `orphanDrafts`. 옛 항목은 새 값을 쓰고 다시 읽어 같을 때만 지운다; 읽지 못한 값은 남긴다.
- **U6**: 옛 키의 `ft.alive:<옛 키>:<writer>`가 최근(`ALIVE_STALE_MS` 안, "closed" 아님)이면 그 writer의 초안은 옮기지 않는다(`aliasWriterAlive`).
- 하네스: verify-review-batch --unit "R21 U1"–"R21 U6"(74a7b82 자산에서 각각 실패 확인).

## 28. 도구 설치 안내 트랙 U (2026-10-20, docs/PLAN_TOOLS_2026-10-20.md U1·U2)

- **파일**: `assets/app/56-tools.js`·`56-tools.css`(새, 이 트랙 소유). 마크업: 빈 상태 `.h-col` 안 이전 프로젝트 블록 뒤 `<section class="h-tools" id="hToolsPanel" hidden>`(`h3#hToolsPanelTitle`, `ul#hToolsList`) — `.sblock`이 아니다(빈 상태는 블록 2개 그대로). 패널 안에는 live region이 없다(프로젝트가 열리면 패널이 숨어 알림이 사라지므로) — 알림은 모두 셸의 전역 `announce()`(`#srStatus` polite / `#srAlert` assertive). 증거 넣기 블록 `#startErr` 뒤 `#startToolPrompt`, 결과물 시트 고급의 `#advOut` 뒤 `#advToolPrompt`(둘 다 `.h-toolprompt`, role=group, hidden).
- **서버 계약(트랙 S, `docs/PLAN_TOOLS_2026-10-20.md` "§S as implemented"가 원본)**: `GET /api/tools` → `{ok, platform, platform_supported, tools:[T], install: J|null, now_unix}`. T: `name, title, label, kind("archive"|"pip"), required, on_demand, version, pinned_version, license, notice(한국어 배포 고지 — 사용자가 출처에서 직접 내려받고 FrameTrace는 배포하지 않음; 설치 가능 도구는 항상 있음, 아니면 null), notice_en(영어, 없을 수 있음), homepage, source_url, provides[], status, installed, paths{<binary>: 절대 경로|null}, shadowed_by{<binary>: PATH의 다른 사본 절대 경로|null}, encoders{probed, libx264, h264, aac, avi_audio, note, trial_failed[], trial_timed_out[]}|null(ffmpeg만), installable, unavailable_reason(=not_installable_reason, 영어 긴 문장), manual_hint(한국어 수동 설치 안내), source_host, size_bytes, download{file_name,url,host,size,sha256,checksum_source}|null, rollback_incomplete(bool), rollback_note(string|null, 한국어), rollback_backups[](절대 경로)`. J: `{id, tool, state: downloading|verifying|extracting|done|failed, stage, bytes, total, error, cancelled, started_unix, finished_unix, age_s, installed_files[], audit_warning, staging_left}` (`cancelled: true` = 받아들여진 중단으로 끝난 `failed`, `error`는 `INSTALL_CANCELLED` "install cancelled"; 옛 서버는 필드 없음 → 그 error 문자열로 판단). `stage`가 `install|venv|pip`이면 중단할 수 없다. `POST /api/tools/install {name}`(jpost — 기존 쿠키·nonce 인증) → `{ok:true, install:J}`(state는 바로 `downloading`) / `{ok:false, code, error[, manual_hint, homepage]}`. `GET /api/tools/install-status` → `{ok:true, install:J|null, now_unix}`. `POST /api/tools/install-cancel {id}` → `{ok:true, install:J}` / `{ok:false, code:"no_install"|"not_cancellable"|"stale_job"}`. 엔드포인트 없음(404·405·501)·`tools` 없음 → 패널·프롬프트 없음(오류 아님), deepfake 버튼은 이전 그대로 비활성. 일시적 실패(응답 없음)는 화면의 이전 상태를 유지한다.
- **패널**: 미설치 도구만 줄로(설치된 도구는 안 보임, 이 화면에서 방금 설치한 것만 "{label} 설치됨"). `label`은 서버 값("FFmpeg"). 필수 미설치 = 경고(`.h-tool-warn`, "{label} 없음 — 필수"), 선택 = 정보(`.h-tool-info`). `installable: true`면 [설치](보조 버튼; 주 버튼 아님), 아니면 버튼 없이 **`manual_hint`**(한국어, 안의 https URL은 링크; 없으면 "여기서 설치할 수 없음", URL이 없으면 "— 수동 설치: <homepage>"(https만 링크)) — 영어 `unavailable_reason`은 그 줄의 `title`로만. 모두 설치되면 패널 숨김. `on_demand: true`·`kind: "pip"`·`name: "deepfake-lens"`은 패널에 절대 없다. pip 명령은 어디에도 표시·실행하지 않는다.
- **확인 대화상자**(`askDialog`, 초점 가둠·Esc 취소·처음 초점 [취소]): 제목 "{label} 설치", 파일(`download.file_name`; 없으면 `label pinned_version`)·출처(`download.host`/`source_host`)·크기(`download.size`/`size_bytes`, 없으면 "알 수 없음")·라이선스 + "확인을 누르면 내려받습니다. 해시가 다르면 설치하지 않습니다." + 고지 문단 `<p class="h-tool-notice">`(`notice`; 영어 화면은 `notice_en`, 없으면 `notice`; 둘 다 없으면 문단 없음 — 옵션 C, docs/PLAN_TOOLS_C_2026-10-20.md T5). OK 전에는 아무것도 보내지 않는다. 대화상자가 떠 있는 동안·설치 중에는 두 번째 설치를 열지 않는다(`HT.confirming`/`HT.busy`, 다른 [설치] disabled+aria-busy). 버튼이 사라지거나 비활성으로 바뀌면 초점은 **그 줄**의 글(`[data-ht-focus]`, tabindex=-1)로 — `<body>`로 떨어지지 않음.
- **진행·결과**: POST 응답의 `install`을 바로 그린 뒤 `GET /api/tools/install-status`를 700 ms마다 폴링(가벼운 끝점; 없는 서버는 `GET /api/tools`). 폴링 실패는 이전 상태 그대로(깜빡임 없음) 다시 시도하고, 실패로 알리는 것은 서버의 `state:"failed"`뿐. 줄에 "내려받는 중 1.0 MB / 4.0 MB (25%)" · "해시 확인 중…" · "압축 푸는 중…"; 단계가 바뀔 때만 polite 알림(바이트마다 아님). 실행 중인 도구 줄은 [설치] 대신 **[중단]**(→ `POST /api/tools/install-cancel`; 받아들여지면 "중단하는 중…"·버튼 disabled, 끝나면 "{label} 설치를 중단했습니다"(토스트 warn); `not_cancellable` → "이 단계는 중단할 수 없습니다"(설치는 계속); `no_install` → 다음 폴링이 결과를 보여 줌). 끝: "{label} 설치됨"(토스트 ok, `loadEnv()`로 상단 '도구 N/5'·E01 안내 갱신) 또는 "설치 실패: {error}"(토스트 err = assertive, [설치]로 재시도). 작업 기록이 사라짐(서버 재시작) → "설치 상태를 알 수 없습니다 — 다시 시도하십시오". **새로 고침·다른 탭**: `htLoad`가 실행 중(`downloading|verifying|extracting`)인 `install`을 보면 클릭 없이 그 설치를 따라 폴링하고 같은 결과·`loadEnv()`로 끝낸다(새 응답으로만 — 실패해서 남겨 둔 옛 상태로는 따라가지 않음). 폴링 중 작업 id가 바뀌어 다른 실행 중 작업이 보이면(다른 탭·CLI·서버 재시작 뒤 새 설치) 그 작업을 따라간다("상태를 알 수 없음" 아님); 끝낼 때는 `HT.busy`를 먼저 비운 뒤 다시 읽어 이미 시작된 다음 설치도 따라간다. **중단 판정**: `failed`이면서 `cancelled: true`·error "install cancelled"·이 탭의 중단 요청(진행 중 포함, 단 `cancelled: false`면 실패)이면 "{label} 설치를 중단했습니다"(warn) — "설치 실패: install cancelled"는 없다. 실행 중 단계가 `install|venv|pip`이면 [중단] disabled, title "이 단계는 중단할 수 없습니다". 폴링이 401이거나 20번 연속 실패하면 따라가기를 멈추고 "설치 상태를 알 수 없습니다"(무한 폴링 없음). 이 탭이 보지 않은 오래된 `failed`(끝난 지 5분 넘음 또는 `finished_unix` 없음)는 새로 고침·다른 탭에서 다시 "설치 실패"로 보이지 않는다. "설치 요청 중…"은 줄에만 쓰고 알리지 않는다(POST 응답의 상태를 바로 알림). 다시 그릴 때 초점이 어떤 줄의 글(`[data-ht-focus]`)에 있었으면 **같은 줄**로 돌려놓는다.
- **Fix round 3 (서버 시계·작업 id·그림자)**: `now_unix`(서버 시계, 두 GET 모두)와 J의 `age_s`(끝난 지 몇 초인지, 서버 시계 기준; 실행 중이면 null). "최근에 끝난 `failed`"(5분)는 `age_s`(없으면 `now_unix - finished_unix`)로만 판단하고 브라우저 시계는 두 값이 다 없는 옛 서버에서만 쓴다. [중단]은 따라가는 작업의 `{id}`(`HT.job`)를 보낸다 — 서버의 현재 작업과 다르면 `{"ok":false,"code":"stale_job","install":J}`이고 아무것도 멈추지 않는다(다른 탭이 시작한 새 설치를 늦은 클릭이 멈추지 않음); UI는 `no_install`처럼 다룬다(오류 문구 없음, 다음 폴링이 보여 줌). `id` 없는 요청(옛 UI)은 종전대로 실행 중인 작업. 확인 대화상자가 떠 있는 동안 다른 설치를 따라가게 되면 "다른 설치가 진행 중입니다"를 알리고, OK를 눌러도 POST 없이 같은 문구를 다시 알린다(조용히 아무 일도 안 하지 않음). POST 응답이 사라지면(네트워크·status 0·해석 불가) "설치 요청 실패" 대신 `htLoad()`로 서버에 묻는다 — 그 도구 설치가 실행 중이면 따라가고, 방금 끝났으면 그 결과, 없으면 "설치 상태를 알 수 없습니다 — 다시 시도하십시오"(HTTP 오류 응답은 종전대로 거부 문구). `shadowed_by`에 값이 있는 도구(tools/bin 것이 쓰이고 PATH에 FrameTrace가 쓰지 않는 다른 사본이 있음)는 그 줄에 짧은 주의 `<small class="h-tool-shadow">` "PATH의 다른 사본 무시됨"(title에 경로와 "직접 둔 것이 아니면 확인하십시오") — 설치된 도구도 이때만 정보 줄 하나로 보인다. deepfake-lens는 개인 venv가 먼저라 `shadowed_by`가 항상 null. `encoders.trial_failed`(시험 인코딩에 실패해 건너뛴 인코더)·J의 `staging_left`(지우지 못한 이전 파일 경로, 설치는 성공)는 UI가 지금 표시하지 않는 정보 필드다.
- **Fix round 4**: POST 응답의 HTTP 상태가 400 이상이면(5xx 포함, 본문이 JSON이 아니어도) 서버의 답 = 거부 문구이고, "상태를 알 수 없음"으로 서버에 다시 묻는 것은 응답 없음·`network`·status 0·해석 못 한 2xx뿐이다(`htPostLost`). 그렇게 다시 물었을 때 **다른 도구**의 설치를 따라가게 되면 그 설치는 계속 따라가되 요청한 도구 줄에는 "설치 상태를 알 수 없습니다 — 다시 시도하십시오"(성공으로 치지 않음). 그림자 주의 줄의 경로는 title만이 아니라 `<span class="sr-only">`로도 들어간다. 서버는 작업자가 이미 끝낸 작업(단계 done/failed)의 취소를 `no_install`로 거부한다. ffmpeg `encoders.trial_timed_out[]`: `trial_failed` 중 시간 초과로만 건너뛴 이름(10분 뒤 다시 시험; 표시하지 않는 정보 필드).
- **Fix round 6 (encoders, 기준 224d52a)**: ffmpeg `encoders.listing_timed_out`(bool: `-encoders`가 재시도 뒤에도 시간 초과)·`encoders.assumed_candidates`(bool: `-encoders`가 목록을 주지 않아 — 위치와 무관하게 — 후보 전체 libx264·h264_mf·libopenh264·mpeg4·aac를 가정하고 시험 인코딩으로 고름; 이때 `probed` false, `libx264`는 가정값이라 true일 수 있고 실패하면 `trial_failed`에 들어감). round 5의 `assumed_pinned_build`는 없어졌다. `note` 예: "ffmpeg -encoders 확인 시간 초과 — 시험 인코딩으로 h264_mf 선택 · 잠시 뒤 다시 확인"(시험이 결론을 못 내면 "시험 인코딩 결론 없음, … 가정"). UI는 `note`를 그대로 보여 주므로 변경 없음; 두 bool은 표시하지 않는 정보 필드다.
- **Fix round 7 (encoders, 기준 7db65c7)**: `-encoders` 목록이 없을 때 시험 인코딩이 아무것도 증명하지 못하면(mpeg4 대조 시험도 0이 아닌 종료 — lavfi 없는 최소 빌드 — 또는 시험 실행 불가) 시험하지 않은 libx264가 아니라 **mpeg4**를 쓴다: `encoders.h264` "mpeg4", `trial_failed` 비어 있음, 시험이 하나라도 돌았으면 `note` "ffmpeg -encoders 확인 실패 — 시험 인코딩 증거 없음, mpeg4(MPEG-4 Part 2) 가정 · 잠시 뒤 다시 확인"(시험이 하나도 못 돈 경우는 Fix round 8 참고). 목록이 실패(시간 초과 아님)한 뒤 시험이 시간 초과되거나 예산이 끝나도 이제 note가 있다("… 시험 인코딩 결론 없음, mpeg4(MPEG-4 Part 2) 가정 · 일부 시험 인코딩 시간 초과 · 잠시 뒤 다시 확인"). JSON 키 변경 없음. 감사 줄의 `encoder_source` "assumed (encoder probe failed)"에서 “probe failed”는 **`-encoders` 목록이 실패했다**는 뜻이다 — 시험 인코딩으로 고른 경우도 같은 문자열이며(감사 줄 형식은 고정), 시험 결과는 doctor/`tools list`/`/api/tools`의 `note`에 있다.
- **Fix round 8 (encoders, 기준 47f9625; Fix round 9에서 조건·문구 정정)**: 파일은 있는데 실행할 수 없는 ffmpeg(백신 차단·다른 아키텍처·실행 권한 없음)는 `encoders.h264` "mpeg4"와 `note` "ffmpeg 실행 실패 — 클립·프록시·내보내기가 실패할 수 있음 · 잠시 뒤 다시 확인"(종전 note 없음 — doctor가 경고 없이 ok였다; Fix round 8의 “mpeg4(MPEG-4 Part 2) 가정” 문구는 프록시가 mpeg4로 만들어진다는 뜻으로 읽혔지만, 실행할 수 없는 ffmpeg는 spawn에서 실패한다). 정확한 조건(`spawn_failed`): 파일이 있고, `-encoders` 목록이 실패했고(0이 아닌 종료 **또는** 빈 목록 **또는** spawn 실패 — 시간 초과는 아님), **첫 시험 인코딩의 spawn이 실패**(`Trial::Unknown`, 그 전에 돈 시험 없음)한 경우 — “둘 다 spawn 실패”만이 아니다; doctor는 이 줄을 WARN으로 낸다. note가 null인 것은 이제 **파일이 없는** ffmpeg뿐이다(없는 도구는 `installed`/`paths`가 알린다). JSON 키 변경 없음; UI는 `note`를 그대로 보여 주므로 변경 없음.
- **Fix round 13 (되돌리기 보존 백업, 기준 f207a02)**: T의 `rollback_incomplete`는 실패한 설치를 되돌리지 못해 그 도구의 이전 파일이 `tools/rollback-backup-<hex>/`(archive)·`tools/python/rollback-backup-<hex>/<도구>/`(pip venv)·남겨 둔 `tools/.staging-<hex>/backup/`에 보존돼 있다는 뜻이다(각각 `RESTORE.txt`, 첫 줄 `tool: <이름>`; 청소 대상 아님). `rollback_note`는 첫 백업의 한국어 안내(경로·크기·RESTORE.txt대로 복원하거나 다시 설치한 뒤 폴더를 지우라는 말), `rollback_backups`는 그 폴더 경로들(플래그가 false면 `rollback_note` null·`rollback_backups` `[]`; 도구를 알 수 없는 백업은 어느 도구에도 붙지 않고 doctor에만 나온다). `installed`여도 새것·옛것이 섞였을 수 있어 패널은 warn 줄 `<small class="h-tool-rollback">` "이전 설치를 되돌리지 못했습니다 — 도구 파일이 섞였을 수 있습니다"(en "A previous install could not be undone — the tool's files may be mixed"), `rollback_note`는 title·sr-only. J의 `error`가 `rollback_incomplete:`로 시작하면 감사 줄 `code: "rollback_incomplete"`. `frametrace tools list --json`도 같은 필드, 텍스트 출력은 `  경고: <안내>`, `frametrace doctor`는 백업마다 WARN.
- **Fix round 14 (기준 90873bc)**: `/api/tools`·`tools list`는 보존 백업을 요청마다 한 번만 찾고 크기를 잰다(종전 도구마다). `frametrace tools install`의 확인 프롬프트도 설치할 `tools` 폴더의 보존 백업을 `  경고:` 줄로 보인다. `RESTORE.txt`를 쓰지 못한 경우 설치 오류는 그 파일을 가리키지 않고 "(RESTORE.txt could not be written: <원인>)"을 붙인다(그 백업은 도구 미상으로 나올 수 있다 — archive; venv 백업은 하위 폴더 이름이 도구). [서버 종료] 대화 상자를 기다리는 중에 닫았는데 그때 이미 설치가 끝났으면(중단할 수 없던 설치) toast는 "도구 설치가 끝났습니다 — 도구 패널에서 결과를 확인하십시오"(종전 "계속 진행됩니다"), 받아들여진 중단은 "설치는 이미 중단됐습니다"; 도구 패널이 같은 설치를 따라가는 중(`HT.busy`)이면 그 toast는 내지 않는다(패널이 끝을 알린다). JSON 키 변경 없음.
- **요청 거부**(`code`별 짧은 문구, assertive): `not_installable` "여기서 자동 설치할 수 없습니다" · `unsupported_platform` "이 운영체제에서는 자동 설치를 지원하지 않습니다" · `python_required` "Python이 필요합니다"(이 셋은 `manual_hint`+homepage를 함께) · `install_running` "다른 설치가 진행 중입니다"(그 설치를 따라 폴링) · `busy` "분석·내보내기가 끝난 뒤 설치하십시오" · `shutdown` "서버가 종료 중입니다" · `unknown_tool` · `spawn_failed` · `manifest_invalid`; 모르는 코드는 "설치 요청 실패: {error}". 서버의 전체 메시지는 `title`로만.
- **결과물 점검 목록**: 필수 도구 미설치마다 `warn` "{label} 없음 — 영상 검증·프록시를 만들 수 없음" / "첫 화면 ‘도구’에서 설치하거나 tools/bin에 넣으십시오."(차단 아님, `htFinalizeChecks`).
- **deepfake-lens 프롬프트**: 고급 [딥페이크 스크리닝] 또는 '딥페이크 스크리닝' 옵션을 켠 [분석 시작]에서 — 서버 목록에 미설치이고 `installable: true`면 실행 전에, 서버가 `tool_missing: "<name>"`으로 답하면 그 뒤에 — 그 자리의 `.h-toolprompt`에 "합성 영상 검사에 필요한 도구가 없습니다"(다른 도구는 "{label}이(가) 없습니다"), 나타날 때 초점은 그 글(`[data-ht-focus]`)로. [설치] → 같은 확인 대화상자 → `POST {name:"deepfake-lens"}` → 같은 진행·[중단] → "{label} 설치됨" + [다시 검사]/[다시 시작](같은 동작을 다시 실행, 프롬프트 닫힘). `installable: false`(지금 매니페스트: PyPI에 없음)면 `manual_hint`만(사유는 title, 버튼 없음); 목록에 없음(옛 서버) → "tools/bin 또는 PATH에 설치하십시오."만.
- **deepfake 옵션·버튼의 활성 상태**: `/api/env`(`loadEnv`)와 `/api/tools`(`htLoad`)는 어느 쪽이 먼저 답해도 같은 결과 — 둘 다 끝에 `htSyncDeepfake()`를 부른다. env에서 없음이면, `htNeedsTool("deepfake-lens")`(= 목록에 있고 미설치이고 `installable === true`)일 때만 다시 켠다(누르면 안내). 그 밖에는 비활성 그대로, 툴팁은 `manual_hint`(`htToolHint`). 결과물 시트 `finApplyEnv`도 같은 규칙. 이 컨트롤들의 고정 툴팁(`dfAskStart`·`dfAskBtn`·`dfManual`·`dfReady`)은 `HT_STR` 키라 영어 화면에서는 영어다(30-form.js·41-finalize.js에 한국어 글자 그대로 없음).
- **언어**: 문구는 `HT_STR.ko`/`HT_STR.en`(같은 키). 셸의 뷰어 언어 선택(`hLocalePref`)을 따르고 기본은 한국어; 언어가 바뀌면 다시 그린다. 서버가 주는 `manual_hint`는 한국어 그대로.
- **legacy 셸**: 도구 팝오버에 한 줄 "도구 설치: 명령 프롬프트에서 `frametrace tools list`"만(API 호출 없음).
- 하네스: verify-review-batch --unit "Tools U1/U2", "Tools deepfake option", "Tools deepfake-lens prompt", "Tools job id replaced mid-poll", "Tools second tab cancels", "Tools stale failed job not re-shown", "Tools focus stays on the same row", "Tools [중단] disabled", "Tools polling gives up", "Tools announcements", "Tools deepfake tooltips", "Tools mock", "Tools confirm dialog open while another install is adopted", "Tools \"recent\" is judged by the server clock", "Tools a lost POST reply", "Tools [중단] names the job", "Tools shadow warning", "Tools mock + fixture carry the round-3 fields", "Tools confirm dialog carries the distribution notice (ko/en)"(`scripts/lib/tools-unit.mjs`, 실제 모양은 `scripts/lib/tools-mock.mjs`의 `TOOLS_FIXTURE`; 흉내 서버도 진짜처럼 `install|venv|pip`에서만 `not_cancellable`, 받아들인 중단은 다음 폴링에 `failed`+`cancelled: true`); verify-layout "tools light/dark"(1366×768, `routeTools`로 네 끝점 흉내: 줄·manual_hint·크기·두 블록·키보드 대화상자(파일 이름·고지 문단 표시·대비 4.5:1)·Esc 무전송·POST 1회·진행·`#srStatus` 알림·[중단]·결과·새로 고침 뒤 이어 폴링·중단); check-a11y 페이지 `shell-empty-tools`·`shell-empty-tools-dialog`·`shell-empty-tools-running`(두 테마·두 해상도).
