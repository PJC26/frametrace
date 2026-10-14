# FrameTrace — Claude Code 작업 지침

한국어 블랙박스·CCTV·SD·HDD 영상 포렌식 검수 워크스테이션(Rust CLI + 로컬 HTTP 서버 + 단일 페이지 UI)이다.
**처음 들어왔다면 [docs/HANDOFF-2026-10-11.md](docs/HANDOFF-2026-10-11.md)부터 읽는다** — 현재 상태, 남은 일, Windows 검증 체크리스트가 있다.

## 소유자(사용자) 선호

- 답변은 한국어로, 짧고 쉽게. 결과는 표·목록으로 요약하고, 확인 못 한 것은 숨기지 않고 따로 적는다.
- UI 원칙: **"딱 필요한 설명과 버튼만"**. 버튼은 항상 최적 조건(누르는 영역 ≥24px, 주 버튼 ≥36px, 영역마다 주 버튼 1개, 기본·호버·누름·포커스·비활성·진행 중 상태 구분, 두 테마 AA 대비, 1366×768~1920×1080에서 잘리거나 가려지지 않음).
- 모델 분담(서브에이전트 위임 시 `model` 지정): 계획·막히는 어려운 작업 = **Fable**, 기본 작업 = **Opus**, 워커 = **Sonnet**, 잡일 = **Haiku**.
- **omo**(OmO Native, `omo-ai`)를 쓴다. 계획을 omo로 세워 달라고 하면: `omo -p --no-session --no-ask-user --permission-preset read-only --exclude-tools edit,write --model anthropic-subscription/claude-fable-5-1 --thinking high "<프롬프트>"` (Claude 구독 로그인 필요, `omo` 실행 후 `/login`).
- 큰 작업은 병렬 워커(별도 worktree, 파일 소유권 분리) → 하나씩 머지 → 전체 게이트 → **적대적 리뷰**(Fable) → 수정 순서로 진행한다. 큰 변경 뒤 적대적 리뷰에서 매번 차단 문제가 나왔다.
- 사용자가 "진행해", "/ulw"라고 하면 끝까지 자율 진행. 되돌리기 어려운 외부 동작(force push, 삭제 등)만 확인받는다. main 브랜치에 직접 커밋·push 하는 흐름을 써 왔다.

## 반드시 지킬 것

- **주석(마크·메모·태그·IN/OUT 구간)은 절대 잃지 않는다. 조용한 실패 금지. 보고서는 검증한 것만 주장한다.** (docs/PLAN-v100.md §2)
- 테스트·하네스는 사용자의 실제 설정(`~/.config/frametrace`, `%APPDATA%\frametrace`, `~/.frametrace`)을 절대 건드리지 않는다 → 서버를 띄울 땐 `scripts/lib/isolated-server.mjs`의 `startIsolatedServer({root, work, env})`.
- 보고서에 들어가는 독립 뷰어(`src/html_report.rs`가 내보내는 evidence_viewer.html)는 법원 제출물 — 바꾸면 `verify-embed` 동등성 검사로 의도적 변경인지 확인.
- 단축키 보존: j/k/n/x/1/2/3/0/i/o/Space/,/./Shift+J·K·L/Ctrl+Z·Y/Ctrl+B/Ctrl+K, 셸은 Alt+P(프로젝트)·Alt+J(작업)·Alt+R(결과물).
- `cfg(windows)` 코드는 macOS에서 교차 컴파일이 안 된다(libsqlite3-sys). Mac에서 바꿨다면 draft PR로 Windows CI를 먼저 통과시켰다.

## 게이트 (머지 전 전부 통과)

```
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node scripts/check-js.mjs && node scripts/check-css.mjs && node scripts/check-ps-bom.mjs && node scripts/verify-contract-pin.mjs && node scripts/check-rectime-fixture.mjs
cargo build --locked && FRAMETRACE_BIN=<repo>/target/debug/frametrace-app bash scripts/ci-harnesses.sh   # 27종 (Windows는 Git Bash; 바이너리 이름은 Linux/macOS `frametrace-app`, Windows `frametrace-app.exe` — isolated-server.mjs가 .exe를 자동으로 붙임)
FRAMETRACE_IT=1 cargo test --locked --test workstation_e2e -- --ignored
FRAMETRACE_IT=1 cargo test --locked --test integration_tools -- --ignored   # ffmpeg 필요; libewf 없는 PC는 `--skip e01_import_roundtrip_with_real_libewf`
FRAMETRACE_IT=1 cargo test --locked --test join_audio_it -- --ignored       # ffmpeg·ffprobe 필요
```

Playwright 설치는 **한 번에 함께**: `npm install --no-save --no-package-lock playwright@1.56.1 axe-core@4.10.2 pngjs pixelmatch` 후 `npx playwright install chromium`. (`npm i --no-save X`를 따로 하면 다른 패키지가 지워진다.)

## 어디에 무엇이

- 현재 상태·인수인계: `docs/HANDOFF-2026-10-11.md`
- 이상 상태·완료/비목표 기록: `docs/PLAN-v100.md` (§2, §6, §7)
- UI 구조 계약(파일 소유권, ID 맵, FTViewer API, 토큰, 레이아웃 px, 키맵): `docs/ui-rebuild-contract.md` / 원본 계획: `docs/ui-rebuild-plan-omo.md`
- 백로그 원본: `docs/review-backlog-2026-10.json`
- Windows 설치·사용: `docs/WINDOWS_USAGE.md`, `docs/WINDOWS_VALIDATION.md`
- 스크린샷 재생성: `docs/SCREENSHOTS.md` (`node scripts/capture-screens.mjs`, `node scripts/visual-diff.mjs`)
