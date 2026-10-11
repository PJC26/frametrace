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

`btnExitFs caseLine linkCarveReport btnTheme btnLang btnRail query kind status btnMoreFilters filtersExtra btnDensityComfortable btnDensityCompact sortBy groupBy btnGroupKind tagFilter pageSize dateFrom dateTo btnClearDates pillVerified metricVerified pillFailed metricFailed pillCandidate metricCarved pillAnomaly metricAnomaly btnViewMenu viewMenuList btnTheater btnFullscreen btnPip btnTimeline btnShortcuts leftRail railTitle presetChips facetTree calTitle dayHistogram periodLabel mainArea caseWarnings timelinePanel timelineMeta btnTimelineGen btnTimelineClose timelineList activeFilters recordGrid dflSummary triageStatus resultCount prevPage pageStatus nextPage gridLive vsplitter mediaTitle mediaStatus mediaStage mediaToolbar btnSkipBack btnSkipFwd playRate btnStepBack btnStepFwd btnSetIn btnSetOut rangeLabel tbMore btnMoreTools moreToolsList toolsRow btnPopPlayer btnDual btnDownloadFile btnCaptureFrame btnExportClip btnExportClipBurn btnProxy videoSizeLabel videoMode videoZoom metricVideos hsplitter detailsTitle detailBadges detailTabs tabSummary tabInspector tabValidation tabTelemetry tpSummary summaryList tpInspector metaList tpValidation validationList tpTelemetry telemetryTitle btnTelemetry telemetryPane telemetryCanvas telemetryNote telemetryEmpty selectionBar scopeToggle btnScopeActive btnScopeSelected scopeSelLabel btnSelectFiltered btnClearSelection selectionCount btnMarkReviewed btnMarkImportant btnMarkVerify btnMarkClear btnTagMenu tagMenuList examinerName saveState btnApplyMarks btnExportMenu exportMenuList btnDownloadFiles btnExportSelected btnTranscodeQueue btnDownloadCsv btnSummary btnDownloadMarks btnDownloadSelection btnCopyIds btnCopyPaths toastHost shortcutsModal shortcutsTitle btnShortcutsClose` 및 JS 생성 id `ft-live-chip ft-load-note calPrev calNext calYear chipMore customTagInput btnAddCustomTag btnSaveCustomTag evidenceNote tagPresetInput cmdPalette cmdList reviewProgress rangeDivergenceBanner rangeUseServer rangeUseBrowser` + 팝업 플레이어 문서의 `stage vv novid title meta note list play rate prev next cap wrap left b1…b10 f1…f10`.

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
  init({ data = null, url = "", embed = false, host = document, theme = null, locale = null } = {}) → Promise<void>,
  destroy() → void,                 // AbortController.abort() + root.innerHTML 유지(마크업은 남고 상태·리스너만 제거)
  reload({ keepUnsaved = true, reason = "" } = {}) → Promise<"reloaded"|"refused">,
  setLocale("ko"|"en"), getLocale() → "ko"|"en",
  setTheme("light"|"dark"), getTheme(),
  focus() → boolean,                // 활성 카드(없으면 #recordGrid)에 포커스
  summary() → { total, byKind, byStatus, warned, marked, noted, pending },   // = __frametraceSummary() + pending(미저장 건수)
  saveState() → "saved"|"pending"|"saving"|"failed"|"queued"|"local"|null,
  save() → Promise<boolean>,         // syncMarksNow()
  setExaminer(name), getExaminer() → string|null,   // 검토자(마크 귀속). 호스트 선택 바에는 입력란이 없고 41-finalize `#finalExaminer`가 호출
  liveState() → { live, generation, total, canReload }|null,   // = 마지막 `live-state` detail
  popout() → Window|null,            // 70-player.js openPopupPlayer(): 영상+판독 창
  closePopout(), isPoppedOut() → boolean,
  toast(message, kind = "info") → void,     // §6.5 단일 토스트 호스트
  on(event, fn, opts), off(event, fn)       // EventTarget; fn(CustomEvent) — detail는 아래 표
};
```

### 3.1 라이프사이클

| 상황 | 동작 |
|---|---|
| **standalone** (`/review/evidence-viewer.html`, `/viewer/`, 패키지 파일, 레거시 iframe `?embed=1`) | `99-loader.js` 끝: `if (!document.documentElement.hasAttribute("data-ft-host")) FTViewer.init({ embed: new URLSearchParams(location.search).has("embed") || window.self !== window.top })`. 데이터 소스 판별은 현 `loadViewerData()` 순서 그대로(인라인 → `/api/records` → `data-bundle.js`). |
| **hosted** (`GET /`) | `examiner_app.html`은 `<html data-ft-host="shell">`. 자동 init 없음. `22-viewer-host.js`가 프로젝트 열림(`FTStore.state.phase ∈ {review-partial, review-ready, done, finalized}`)에 `FTViewer.init({ embed: true, host: $("hViewerHost"), theme, locale })`를 호출하고 `hViewerHost.hidden=false`, `inputCard.hidden=true`. 프로젝트 전환: `await FTViewer.save()` 성공(또는 저장 없음) 확인 → `FTViewer.destroy()` → `/api/open-case` → `init()`. `init()` 재호출은 `destroy()` 없이는 거부(`throw Error("FTViewer already mounted")`). |
| 번들 평가 시점 | `10-boot.js`의 `(async function () {` → `async function ftViewerBoot(opts) {`, `95-init.js`의 `})();` → `}`. `00-host.js`는 `FTViewer.init = opts => ftViewerBoot(opts)` 호이스팅 함수에 의존(TDZ 없음). |
| 리스너 | 뷰어가 `document`/`window`/`matchMedia`에 거는 모든 리스너는 `{ signal: FTViewer._ac.signal }`로 등록(T5가 grep 기준: `addEventListener(` 호출 전부). `destroy()`는 abort 후 새 AbortController를 만든다. |
| 데이터 재적재 | `reload()`는 현 `ft-reload` 핸들러 로직(95-init.js:28-63)을 함수화: 미저장이면 `"refused"`(저장 시작 + `toast(t("reload.refused"))`), 아니면 **페이지 리로드 대신** `destroy()`→`init()`(호스트)·`location.reload()`(standalone). 호스트에서 `keepUnsaved=false`면 미저장 무시하고 재적재(사용 금지: 호출자는 T3가 아니라 사용자 확인 대화상자를 거친 T2만). |
| postMessage 호환 | 레거시 iframe(§9.3)을 위해 `ft-theme/ft-locale/ft-reload/ft-embed` 메시지 핸들러는 standalone 모드에서 유지한다. 호스트 모드에서는 등록하지 않는다. |

### 3.2 이벤트 (CustomEvent 이름 / detail / 리스너)

| 이벤트 | detail | 발생 | 듣는 쪽 |
|---|---|---|---|
| `ready` | `{ total, live, generation }` | init 완료(렌더 1회 후) | 22-viewer-host(T5), 01-store(T2) |
| `records` | `{ total, byKind, byStatus, generation }` | 데이터 적재/재적재 후 | 31-progress(T3: 칩 "n/m") |
| `save-state` | `{ state, pending, queued }` | `renderSaveState()` 마다 | 01-store, 40-finalize(T4), 23-popout(T2) |
| `dirty` | `{ count }` | `markReviewDirty()/clearReviewDirty()` | 01-store, 40-finalize |
| `live-state` | `{ live, generation, total, canReload }` | 마운트 데이터의 색인 세대(`live`=부분 색인으로 적재) · 지금 `reload()`가 받아들여질지(미저장 없음). 값이 바뀔 때만 | 22-viewer-host → `viewer.live/liveGeneration/canReload`; 31-progress·32-jobs가 `/api/status`의 `index_generation`과 비교해 칩 "보강 완료"·`btnJobReload` "새 데이터 반영 (n건)" 표시. **호스트에서 색인·보강 상태의 단일 출처는 셸의 `/api/status` 폴링**이며 뷰어는 `#ft-live-chip`을 만들지 않고 상태를 폴링하지 않는다 |
| `selection` | `{ ids: string[], count }` | 선택 변경 | 01-store |
| `active-record` | `{ id, kind, status, mark }` | 활성 카드 변경 | 01-store, 23-popout |
| `locale` | `{ locale }` | 언어 변경(버튼·API) | 15-theme(T2: 라벨 동기) |
| `theme` | `{ theme }` | 테마 변경 | 15-theme |
| `request-popout` | `{}` | 뷰어 `btnPopPlayer` 클릭 | 23-popout(T2: 셸이 `popout()`를 대신 호출하고 본 창 영상을 접음) |
| `popout` | `{ open: boolean }` | 팝업 창 열림/닫힘 감지 | 23-popout, 01-store |
| `toast` | `{ message, kind }` | 뷰어 `toast()` 호출 | 20-net(T2) — 호스트 모드에서만 셸이 표시 |
| `error` | `{ message, detail, stage: "load"|"save"|"export" }` | 로더 실패·저장 실패 | 01-store(배너), 31-progress |
| `destroyed` | `{}` | destroy 완료 | 22-viewer-host |

### 3.3 BroadcastChannel

- `ft-review` (T2 23-popout.js, 세션 단위, 이름 고정 `"ft-review"`): 메시지 `{ type: "popout-open"|"popout-closed"|"theme"|"locale"|"project-changed", caseDir, value, from: instanceId }`. 수신자: 같은 origin의 다른 셸 탭·팝아웃 창. 쓰기는 **본 창(셸)만**: 팝아웃은 마크를 직접 저장하지 않고 `ft-player:<case_id>`(기존 70-player.js 브리지, 불변)로 본 창에 위임한다 → "단일 writer"는 구조적으로 보장된다.
- `ft-player:<case_id>` (T5, 불변): 팝업 플레이어 ↔ 본 창 브리지(`focus/navigate/mark/toggleTag/list/undo/redo`).

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
    server:   { instance: "", lost: false, failures: 0 },
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
| `#selectionBar` | 44px 전폭(실측 49 = 36px primary + 패딩, ≤52), **1행**, sticky bottom | 동일 | `btnApplyMarks` = 영역 유일 primary. **판독 블록을 우측 열로 옮기지 않는다**(§10 Q1). 1366에서 1행 유지: `#reviewProgress`는 <1600px에서 숨김(페이저 `#triageStatus`가 같은 수치), 호스트에서는 `#examinerName` 숨김(검토자는 결과물 시트 `#finalExaminer` → `FTViewer.setExaminer`; 마운트 시 셸 prefs의 이름으로 시드) — DOM과 동작은 유지 |
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
| `Esc` | 순서: ① 셸 대화상자(자체) ② 뷰어 열린 메뉴(window capture, 80-review.js:1077) ③ 뷰어 모달/팔레트 ④ 결과물 시트 ⑤ 작업 시트 ⑥ 셸 팝오버 ⑦ 최신 토스트 ⑧ 뷰어 모드(시어터/전체화면) | 셸은 `document` bubble 단계에서 `e.defaultPrevented`면 중단 | 각 단계는 처리 시 `preventDefault()` |
| `Tab` | 순서: skiplink → 상단 띠 → (빈 상태 열 | 뷰어 topbar → 레일 → 그리드 → 미디어 → 선택 바) → 열린 시트 | 시트(결과물)는 트랩 | |
| `F6` | 영역 순환: 상단 띠 → 레일 → 그리드 → 미디어 → 선택 바 → (시트) | 셸 97-shell.js(신규, 선택) | 미구현 시 생략 가능 — 계약상 "권장" |

충돌 규칙:
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
