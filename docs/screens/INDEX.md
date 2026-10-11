# UI screenshot index

Screens of the integrated workstation (one screen, no stages —
[../ui-rebuild-contract.md](../ui-rebuild-contract.md) §6, §9.4). Generate them with
`node scripts/capture-screens.mjs`; it writes `current/<WxH>-<theme>/<name>.png`
for 1366x768 and 1920x1080 in light and dark, plus `current/INDEX.md` with every
image and the visible-grid-row note. See [../SCREENSHOTS.md](../SCREENSHOTS.md).
Older baselines (`before/`, `before-ux/`) use the pre-rebuild names.

| File | What it shows |
|---|---|
| `00-empty.png` | 빈 상태 — 증거 넣기 · 이전 프로젝트 두 블록 (Empty state: two blocks) |
| `01-empty-options.png` | 옵션(케이스 폴더 · 분석 모드) 펼침 (Options expanded) |
| `01b-empty-filled.png` | 경로 입력 — 자동 판별 칩과 사전 점검 한 줄 (Path filled: detected kind + preflight) |
| `02-running.png` | 분석 시작 직후 — 상단 진행 칩 + 작업 시트 자동 열림 (Chip + jobs sheet) |
| `02b-running-chip.png` | 작업 시트를 닫은 상태 — 진행은 칩에만 (Chip only) |
| `03-project.png` | 프로젝트 화면 — 호스팅된 검토 뷰어 (Viewer hosted in the shell) |
| `03a-project-menu.png` | 프로젝트 전환 팝오버 (Project switcher) |
| `03b-jobs-sheet.png` | 작업 시트 — 삭제 영상 복구 실패 상태 (Jobs sheet, recover failed) |
| `04-viewer-grid.png` … `06b-toast-actionbar.png` | standalone 증거 뷰어 (grid, detail, inspector, shortcuts, clip dialog, multi-select, toast) |
| `06c-toast-hosted.png` | 알림 3개 + 작업 시트 열림 (Three toasts with a sheet open) |
| `07-finalize-sheet.png` | 결과물 시트 — 1차 버튼 1개 "보고서 생성·패키징" (Deliverables sheet) |
| `07b-finalize-blocked.png` | 저장되지 않은 마크로 차단 (Blocked by unsaved marks) |
| `08-finalized.png` | 보고서 링크와 패키지 경로 (Report links + package path) |
| `09-alt-theme.png` | 테마 전환 후 (Alternate theme) |
| `10-popout.png` | 보조 모니터 분리 중인 본 창 (Main window while popped out) |
