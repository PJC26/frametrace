# UI 재구축 계획 (omo · Fable, 2026-10-11)

> omo(OmO Native, `anthropic-subscription/claude-fable-5-1`, 읽기 전용)로 작성한 원본 계획서다. 소유자가 승인했고, 구속력 있는 세부 사항은 [ui-rebuild-contract.md](ui-rebuild-contract.md)에 있다. 계획 작성 시 omo는 스크린샷 폴더 권한이 없어 코드 기준으로 진단했다.

# FrameTrace 통합 화면 재구축 계획

## 1. 진단 — 지금 UI가 어색한 이유

(스크린샷 디렉터리는 외부 경로 권한 거부로 열지 못했고, `examiner_app.html`·`evidence_viewer.html`·`tokens.css`·하네스 코드 기준으로 진단함.)

- **위저드 구조가 작업 흐름과 안 맞음.** `#stageNav`(s1~s4, Alt+1-4)가 "입력→진행→검토→결과물"을 강제. 실제 검수는 "클립 보며 마킹"이 95%인데, 02(분석 진행)와 07/08(결과물)이 각각 전체 화면을 차지해 전환이 최소 4번 생김.
- **분석 진행 화면이 '대기 화면'임.** `#progCard`는 steptrack + `<pre class="log">`뿐. 인덱싱된 레코드가 `/api/records?live`로 이미 흘러나오는데(verify-live-index 하네스가 보장) 사용자는 로그만 보고 기다림.
- **결과물 화면이 버튼 창고.** `#finalCard`에 1차 버튼 1개 + ghost 버튼 8개 + 자유 입력창 + 150자 안내문 + 보고서 iframe. 체크리스트도, "지금 보고서를 내도 되는 상태인지"도 안 보임.
- **헤더 과밀.** 40px `.stagebar`에 브랜드·h1·4단계 레일·케이스명·부제·잡패널(버튼 3개)·메뉴 2개·테마·언어·서버종료 = 14개 요소. 03 검토 화면은 셸 헤더 + 뷰어 `.topbar` + 뷰어 `.selection-bar` 3단 바.
- **버튼 체계가 9종.** `primary/primary big/ghost/hbtn/mini/btn-sm/chip/pill/linkbtn/mi/danger`. 아이콘은 유니코드 글리프(◐ ⏻ ⤢ ⧉ ▾ ↗)라 Malgun Gothic에서 굵기·베이스라인이 제각각.
- **타입·밀도.** 본문 13px(`--fs-sm`)에 1366 해상도 + 장시간 작업은 피로. 모서리 8/12px + 그림자 + 틸 그린 악센트(#0d7569)는 일반 SaaS 느낌.
- **의미 색 충돌.** `--mark-important-bg`(중요)가 `--failed-fg`(검증 실패)와 같은 빨강, `--mark-reviewed`가 `--info`와 같은 파랑 → 그리드에서 "중요"와 "실패"가 같은 색.
- **설명문 과다.** `recoverHint`, `finalCard .hint`, `onboard`, 각 블록 `.sbdesc` 등 — 시작 화면 원칙("딱 필요한 설명과 버튼만")이 다른 화면엔 적용 안 됨.
- **iframe 비용.** 테마/언어/ft-reload를 postMessage로 동기화, 포커스·단축키·스크롤이 두 문서로 갈라짐, 프레임 로드 실패 UI(`frameFail`)까지 유지 중.

## 2. 목표 화면 — 단일 통합 화면

```
┌ 상단 띠 44px: [프로젝트 ▾] [분석 ●1,240/4,000 ▸] ····· [결과물] [도구] [◐] ┐
│ 좌 240/280 │ 중앙 (가변)                      │ 우 480/640                │
│ 검색       │ 활성 필터 칩                     │ 영상 (16:9, 270/360px)    │
│ 상태 카운트│ 클립 그리드 (가상 스크롤)        │ 재생 툴바 1줄 + 더보기    │
│ 파셋 트리  │                                  │ 요약/인스펙터/검증 탭     │
│ 달력       │                                  │ ───────────────────────   │
│ 프로젝트   │ 페이저                           │ 판독: 1 2 3 0 · 태그 · 반영│
└────────────┴──────────────────────────────────┴───────────────────────────┘
```

- **1366×768**: 좌 240(Ctrl+B 접힘) / 중앙 ≥600 / 우 480. 영상 270px, 판독 블록 88px 고정, 나머지 탭 영역 스크롤. 1차 버튼은 우측 하단 "케이스에 반영"(`#btnApplyMarks`) 하나.
- **1920×1080**: 좌 280 / 중앙 ≥960 / 우 640. 영상 360px. 그리드 열 수만 늘고 구조 동일.
- **분석 진행(인라인, 비차단)**: 상단 띠의 분석 칩이 단계/건수/경과를 표시하고, 그리드는 레코드가 들어오는 대로 채움(기존 live 색인). 칩 클릭 → 우측 480px 시트에 steptrack + 로그 + 중단 버튼(`#jobPanel`, `#track`, `#log` id 유지). 복구/카빙/클립 내보내기 잡도 같은 칩에 합류.
- **결과물**: 상단 "결과물" 버튼 → 우측 시트. 구성: 체크리스트(미검토 n, 검증 대기 n, 검토자 이름, 미저장 변경, 원본 재검증 여부 — `/api/preflight`·records-meta로 계산) → 1차 버튼 "보고서 생성·패키징" 1개 → 결과 경로·링크 → 접힌 "고급" 섹션(DFXML·타임라인·QA·비교·병합·기지원 해시). 보고서 미리보기는 새 탭.
- **빈 상태(프로젝트 없음)**: 중앙 영역에 현재 시작 블록 3개(`#blkEvidence`, `#blkRecent`, `#blkViewer`)를 760px 중앙 열로 그대로 배치. 좌·우 패널은 비활성 회색. 분석 시작 즉시 그리드 모드로 전환.
- **프로젝트 전환**: 상단 좌측 프로젝트 버튼 → 팝오버(최근 목록 `/api/recent-cases`, 다른 폴더 열기, 새 증거 넣기). 전환은 `/api/open-case` 후 뷰어 데이터 재적재(페이지 리로드 없음). 미저장 마크가 있으면 확인.
- **보조 모니터**: 상단 "새 창" → 기존 standalone 뷰어 페이지(`/review/index.html`)를 새 창으로. 본 창은 그리드+판독만 남기고 영상을 접음. 통신은 BroadcastChannel `ft-review`(현 postMessage ft-reload 대체, 팝업 플레이어와 동일 채널).

## 3. 디자인 언어

**원칙**: 화면은 종이 서류와 영상 스테이지. 색은 상태에만 쓰고 장식에는 쓰지 않는다. 장시간 응시를 전제로 대비는 높게, 채도는 낮게.

- **팔레트(tokens.css 교체)**: 라이트 `--bg #ecebe7`(따뜻한 회색 종이) / `--surface #fbfaf7` / `--text #1a1c1e` / `--muted #55595e`(≥7:1). 다크 `--bg #121416` / `--surface #1b1e21` / `--text #e6e4df`. 악센트는 틸 → **잉크 블루 `#234e8c`**(다크 `#8db4ea`, on-accent 검정). 그린 계열 표면 제거.
- **상태 의미 재정렬**: 검증 상태(verified/failed/candidate)는 색, **판독 마크는 모양+색**으로 분리 — 판독완료 = 회색 체크, 중요 = 호박색 깃발(`#8a5a00`/`#e0a94a`), 검증대기 = 보라 물음표. 빨강은 "실패/삭제후보/파괴적 동작"에만.
- **타입**: 본문 `--fs-sm 14px`(현 13), 최소 13px, 메타·해시·타임코드는 `--font-mono` + `font-variant-numeric: tabular-nums`. 제목은 굵기만(15/16px), 크기 단계 4개로 축소.
- **형태·밀도**: `--radius 3px`, 그림자는 팝오버·시트에만, 1px 보더. 컨트롤 높이 32px, 1차 36px, 툴바 아이콘 28px(간격으로 24px 확보). 뷰어 '촘촘' 밀도를 기본값으로.
- **버튼 4종으로 통일**: `primary`(채움, 영역당 1개) / `secondary`(보더) / `quiet`(툴바·메뉴, 호버 시 배경) / `destructive`(빨강 보더). 상태 5종(기본·호버·활성·포커스·비활성) 토큰화. 아이콘은 유니코드 대신 **인라인 SVG 스프라이트**(오프라인, 16px, currentColor). `<kbd>` 힌트는 유지.
- **텍스트 절감**: 설명문은 툴팁·"?" 팝오버로 이동. 화면에 남는 문장은 빈 상태·오류·체크리스트뿐.

## 4. 아키텍처 결정

- **iframe 제거, 단일 문서.** 셸이 뷰어 마크업을 같은 DOM에 호스트한다.
  - `assets/viewer/viewer.body.html`(현 `evidence_viewer.html` `<body>` 중 header/toastHost/shortcutsModal 포함 부분)로 분리. build.rs에 `<!-- @include:viewer.body -->` 마커 치환 추가 → standalone 템플릿과 호스트 페이지가 **같은 조각**을 포함. `html_report.rs`는 경로 불변(include_str 그대로).
  - 뷰어 JS는 `window.FTViewer = { init({data|url, embed}), reload(), setLocale(), on(evt, fn) }`를 노출하는 `assets/viewer/00-host.js` 한 파일 추가. standalone은 `__DATA__`로 init, 호스트는 `/api/records`로 init. 나머지 뷰어 파일은 이벤트 디스패치 1줄씩만 추가.
  - 호스트 페이지는 `app.css + viewer bundle.css`, `viewer bundle.js + app bundle.js` 순으로 인라인. 셸 CSS는 전부 `.ft-host` 하위로 스코프하고 셸 전용 클래스를 `h-` 접두(`.rail/.menu/.modal-backdrop/.popover` 충돌 제거). 셸 id 중 뷰어와 겹치는 것(`btnTheme`) 은 `hostTheme`로 개명.
- **상태**: 셸 스토어 1개(`assets/app/01-store.js`: project, phase, jobs, counts, panels) + EventTarget 구독; 뷰어 스토어(마크·선택·undo)는 그대로 두고 셸은 `FTViewer.on('save-state'|'selection')`만 구독. `/api/status` 1초 폴링은 잡이 있을 때만.
- **API 추가**: 없음. 선택 1건 — `/api/status`에 `indexed/total` 카운트가 없으면 추가(칩 표시용).
- **standalone 보존**: `/review/index.html` 라우트와 패키지 내 `evidence_viewer.html`은 동일 조각+동일 번들. 하네스 `verify-viewer-*`는 standalone에서 그대로 통과해야 한다.

## 5. 실행 계획

**Phase 0 (직렬, 1명, ½일)** — 계약 문서 `docs/ui-rebuild-contract.md`: 파일 소유권, id 매핑표, `FTViewer` API, 토큰 이름, 하네스 변경 목록. 여기서 `verify-start-screen`이 쓰는 id(`inputCard`, `blkEvidence/Recent/Viewer`, `btnStart`, `btnViewerOpen`, `sourcePath`, `kindChip`, `preflight`, `recentCaseRow`, `recentEmpty`)와 뷰어 id 전부는 **불변**으로 고정.

**Phase 1 (병렬, 5트랙)**

| 트랙 | 소유 파일 | 산출 | 수용 테스트 |
|---|---|---|---|
| T1 토큰·컨트롤 | `assets/tokens.css`, `assets/app/10-controls.css`, `assets/viewer/controls.css`, `assets/icons.svg`, `scripts/check-css.mjs` | 팔레트·타입·버튼 4종·SVG 스프라이트 | `check-css`(양 테마 대비), 신규 `verify-buttons.mjs`(클래스 4종 외 금지, 상태 5종 스타일 존재, 높이 ≥24/36) |
| T2 호스트 레이아웃 | `assets/examiner_app.html`, `assets/app/00-layout.css`, `01-store.js`, `20-start.js`, `21-project.js` | 3열 셸, 빈 상태, 프로젝트 팝오버, 보조 모니터 | `verify-start-screen`(수정 최소: 레일 부재 체크 제거), 신규 `verify-layout.mjs`(4구성에서 열 폭·버튼 가시성·1차 버튼 영역당 1개) |
| T3 인라인 진행·잡 | `assets/app/30-jobs.js`, `31-progress.js`, `30-jobs.css` | 분석 칩, 잡 시트, 복구/카빙 합류 | `verify-shell-jobs`(`reviewFrame`·`s1~s4` 의존 제거, `jobPanel` 계열 id 유지), `verify-live-index` 무수정 |
| T4 결과물 시트 | `assets/app/40-finalize.js`, `40-finalize.css` | 체크리스트, 1차 버튼, 고급 접힘 | 신규 `verify-finalize-sheet.mjs`(VM: 체크리스트 카운트, precheck 실패 시 버튼 비활성+사유, `/api/finalize` 호출 1회) |
| T5 뷰어 호스팅 | `build.rs`, `assets/evidence_viewer.html`, `assets/viewer/viewer.body.html`, `assets/viewer/00-host.js`, `src/serve/` 라우트 1곳, `scripts/lib/app-bundle.mjs` | 조각 분리, `FTViewer` API, iframe 제거 | 신규 `verify-embed.mjs`(호스트 DOM에 뷰어 id 전부 1개씩, 단축키 j/k/1/Ctrl+Z가 셸 입력창 포커스 시 미발동, standalone 렌더 바이트 동일성), Rust 테스트 `render_review_html` 스냅샷 |

T2는 `<!-- @include:viewer.body -->` 자리만 두고 작업하므로 T5와 독립. T1 토큰 이름은 Phase 0에서 확정되어 T2~T4가 참조만 한다.

**Phase 2 (직렬, 통합 1명)** — 머지 후 `check-a11y`(로드 대상: 빈 상태/프로젝트 열림/standalone으로 변경), `verify-jobs-e2e`(iframe 셀렉터 → 같은 문서 셀렉터), `ci-harnesses.sh`에 신규 3종 추가, `cargo test`(441) + `workstation_e2e`.

**Phase 3 (1명)** — 4구성(1366/1920 × 라이트/다크) 스크린샷 비교, 키보드 단축키 전체 표 수동 점검, 이전 케이스 폴더 열기 회귀.

**리스크·롤백**
- CSS 전역 충돌(뷰어 `.rail`·`.menu` vs 셸): T2 스코프 규칙 + `verify-embed`의 계산 스타일 검사로 차단.
- 단축키 이중 바인딩: 뷰어 키 핸들러는 `document`, 셸 입력창은 `.ft-host [data-keys=off]`로 표시해 뷰어가 무시.
- 롤백: 머지 전까지 기존 `examiner_app.html`을 `assets/legacy/`에 보존, `FRAMETRACE_UI=legacy` 환경변수로 1릴리스 동안 구 페이지 서빙. Phase 3 통과 후 삭제.

## 6. 결정이 필요한 질문

1. **마크 색 변경**(중요 = 빨강 → 호박색 깃발)이 기존 보고서·법원 제출물의 색 범례와 맞물려 있나? 범례 변경이 불가하면 모양만 바꾸고 색은 유지한다.
2. 빈 상태의 **"뷰어 보기" 블록**: 통합 화면에서는 "마지막 프로젝트 열기"와 같은 의미가 된다. 블록 3개 원칙을 지켜 이름만 바꿀지, 2개로 줄일지.
3. **보조 모니터** 범위: 영상+판독 패널만 분리(권장, 저장 창 1개 원칙 유지)인지, 전체 화면 복제인지.
