# 외부 도구 번들 + 안내 설치 계획 (2026-10-20, 기준 54cf3c5)

사용자 결정: **섞는다** — 필수인 FFmpeg는 배포본에 번들, 선택 도구(libewf·Sleuth Kit·deepfake-lens)는 앱 안에서 확인된 다운로드로 안내.

## 원칙
- **T1. 고정 버전·고정 해시.** 번들이든 다운로드든 도구마다 버전·URL·SHA-256을 저장소의 매니페스트(`tools/manifest.json`)에 고정한다. 해시가 다르면 설치하지 않는다(부분 파일도 남기지 않음). "latest" URL 금지.
- **T2. 허용된 출처만, HTTPS만.** 매니페스트의 호스트 허용 목록 밖으로는 내려받지 않는다. 리다이렉트도 허용 목록 안이어야 한다. 프록시·인증서 오류는 그대로 실패.
- **T3. 사용자가 누를 때만.** 앱은 스스로 인터넷에 나가지 않는다. 설치는 사용자가 버튼/명령으로 시작하고, 받을 파일 이름·출처·크기를 먼저 보여 준다.
- **T4. 새 의존성 없이.** HTTP는 Windows 내장 `curl.exe`(10 1803+), 압축 해제는 내장 `tar.exe`(bsdtar). 둘 다 `tool_policy`로 절대 경로 해석. 압축 해제는 임시 폴더에서 하고 경로 탈출(zip-slip·절대 경로·`..`·링크) 항목이 있으면 전체 거부, 매니페스트에 적힌 파일만 `tools/bin`으로 옮긴다.
- **T5. 라이선스 고지.** 번들·설치하는 도구마다 라이선스와 소스 위치를 `THIRD_PARTY_NOTICES.md`와 `tools/bin/licenses/`에 둔다. FFmpeg는 **LGPL 빌드**만(FrameTrace는 MIT).
- **T6. 오프라인에서도 동작.** 번들된 FFmpeg로 기본 기능은 인터넷 없이 동작. 다운로드 실패는 기능 안내만 하고 앱을 막지 않는다.

## 트랙 S — 서버·CLI·패키징 (Opus)
| ID | 내용 |
|---|---|
| S1 | `tools/manifest.json`(도구별 name, version, license, source_url, homepage, archive url, sha256, size, files[](`ffmpeg.exe` 등 상대 경로→`tools/bin/` 이름), required(bool), platform). 실제 URL·SHA-256은 **공식 배포처가 공개한 체크섬**에서 가져오고 출처를 적는다(추측 금지; 못 찾으면 그 도구는 `sha256: null`이고 설치 버튼 비활성 + "수동 설치 안내"만). |
| S2 | 새 모듈 `src/tool_install.rs`: 매니페스트 로드·검증, 상태(`installed`/`missing`/`path`/`version`), `install(name)` = curl로 임시 파일 → 크기·SHA-256 확인 → 임시 폴더에 tar 해제 → 항목 검사(T4) → `tools/bin`에 원자적 이동 → 라이선스 복사 → 감사 로그 한 줄. 실패 시 모두 정리. |
| S3 | CLI `frametrace tools list` / `frametrace tools install <name>` (확인 프롬프트, `--yes`), `doctor`가 미설치 도구에 "frametrace tools install <name>" 안내. |
| S4 | API `GET /api/tools`(상태+매니페스트 요약), `POST /api/tools/install`(nonce 필요, busy 처리, 진행 상태 polling) — state_lock 아래 디스크·네트워크 금지. |
| S5 | 패키징: `scripts/package-windows.ps1`이 매니페스트의 FFmpeg를 받아 해시 확인 후 `dist/FrameTrace/tools/bin`에 넣고 `THIRD_PARTY_NOTICES.md`·라이선스를 포함한 zip을 만든다. windows-ci에 이 스크립트를 돌려 산출물로 올리는 단계 추가(해시 불일치면 실패). |
| S6 | 테스트: 로컬 HTTPS가 어려우므로 다운로드 단계는 테스트 훅(파일 URL/로컬 서버)으로; 해시 불일치·크기 불일치·zip-slip·링크·허용 목록 밖 호스트·리다이렉트·중단 시 정리·state_lock 밖 실행을 검사. 실제 인터넷 다운로드는 테스트하지 않는다. |

## 트랙 U — 화면 (Opus)
| ID | 내용 |
|---|---|
| U1 | 첫 화면·최종화 점검의 "도구" 영역: `/api/tools` 상태 표시(필수 FFmpeg 없음은 경고, 선택 도구는 정보), 미설치 선택 도구마다 "설치" 버튼 → 확인 대화상자(파일 이름·출처·크기·라이선스) → 진행 표시 → 결과. 해시가 없는 도구는 버튼 대신 "수동 설치: <homepage>" 안내. |
| U2 | 키보드·스크린리더 접근성, 두 테마, 짧은 문구(CLAUDE.md UI 규칙). legacy 셸은 안내 문구만. |

## 머지
새 브랜치 `tools-bundle` (54cf3c5 기준) → S → U → 게이트 → push → 새 PR → 적대적 리뷰(공급망·다운로드·압축 해제 집중) → 수정.

## §S as implemented (2026-10-19)

사용자 결정(구현 중 변경): deepfake-lens는 일반 도구 목록·패널에 넣지 않고 **필요할 때만(on demand)** — 합성 영상 검사를 시작했는데 없으면 그 자리에서 설치를 제안한다. 매니페스트에는 `kind: "pip"`, `on_demand: true`로 들어가고, `GET /api/tools`는 `on_demand: true` 표시와 함께 내보내며 UI(트랙 U)가 패널에서 숨긴다. `frametrace tools list`는 기본으로 숨긴다(`--all`). 기존 doctor의 deepfake-lens 줄은 그대로다.

### 매니페스트 (`tools/manifest.json`, `include_str!`로 바이너리에 고정)
| 도구 | 버전 | 설치 | 체크섬 출처 |
|---|---|---|---|
| ffmpeg (필수, LGPL win64) | `n8.1.3-9-g29e619e767`, BtbN `autobuild-2026-09-30-13-08`(월말 빌드 — BtbN은 월말 빌드를 장기 보관) | 가능: 170 611 883 B, SHA-256 `4a7642b2…ffc0a`, `bin/ffmpeg.exe`·`bin/ffprobe.exe` + `LICENSE.txt`(COPYING.LGPLv3) | 그 릴리스의 `checksums.sha256`; GitHub 자산 digest·크기와 교차 확인. 압축 안 구조는 BtbN `build.sh`·`variants/windows-install-static.sh`에서 확인 |
| libewf | 20260924 | 불가(`sha256: null`) — 공식 Windows 바이너리 없음 | — (수동 안내: `scripts/build-libewf-tools.ps1`) |
| sleuthkit | 4.15.0 | 불가 — 공식은 PGP 서명(.asc)만, 압축 안 파일 목록 미확인. GitHub digest는 `reference`에 참고용으로만 기록 | — |
| deepfake-lens (on demand, pip) | — | 불가 — PyPI에 없음(`pypi.org/pypi/deepfake-lens/json` 404, 2026-10-19). 해시 고정 requirements를 만들 수 없음 | — |

허용 호스트: `github.com`, `release-assets.githubusercontent.com`, `objects.githubusercontent.com`.

### 행별 결과
- **S1** 위 매니페스트 + 검증(`validate_manifest`): 64자리 소문자 hex, 크기·체크섬 출처 필수, https·허용 호스트, `latest` 경로 금지, `file_name` = URL 마지막 조각, 파일 매핑은 안전한 상대 경로→순수 파일 이름.
- **S2** `src/tool_install/`(mod·archive·pip). curl.exe/tar.exe는 `%SystemRoot%\System32` 절대 경로. curl은 `--disable --proto =https --tlsv1.2 --max-redirs 0 --max-filesize <size>`; 리다이렉트는 FrameTrace가 직접 따라가며(최대 5) 매 홉을 허용 목록으로 검사 → 크기·SHA-256 → `tar -tf`/`-tvf` 목록 검사(절대·`..`·드라이브/스트림 콜론·역슬래시·링크/특수 항목·중복(대소문자 무시) 하나라도 있으면 전체 거부) → 매니페스트 항목만 추출 → 추출 트리 재검사(reparse point 금지, 목록 외 파일 금지) → `tools/bin`(·`tools/bin/licenses`)으로 전부-아니면-전무 이동(기존 파일은 백업 후 실패 시 복구) → `tools/install-audit.jsonl` 한 줄(성공·실패 모두). 스테이징은 `tools/.staging-*`(같은 볼륨)이고 모든 경로에서 지움. 프로세스 간 잠금 `tools/.install.lock`. Windows 밖에서는 `unsupported on this platform: …`.
- pip(on demand): Python은 `tool_policy`로만 해석(없으면 "Python 필요" 안내), 비공개 venv `<exe dir>/tools/python/<name>`, `python -I -m pip install --isolated --no-input --require-hashes --no-deps --only-binary :all: --index-url https://pypi.org/simple -r <컴파일된 requirements>`; requirements는 `name==ver --hash=sha256:…`만 허용. venv는 옮길 수 없어 제자리에 만들고 기존 venv는 옆으로 옮겼다가 실패 시 복구. 해석 순서: `FRAMETRACE_DEEPFAKE_LENS` → 비공개 venv → PATH/tools/bin. 지금은 requirements가 없어 비활성.
- **S3** `frametrace tools list [--json] [--all]`, `frametrace tools install <name> [--yes]`(파일·출처·크기·SHA-256·라이선스·설치 위치 표시 후 확인; 비대화형이면 `--yes` 필요). doctor는 미설치 도구에 `설치: frametrace tools install <name>` 또는 수동 안내를 붙인다.
- **S4** API(아래 계약). 상태 잠금은 작업 기록/확인에만 쓰고, 해석·네트워크·디스크는 잠금 밖, 진행률은 lock-free 원자값.
- **S5** `scripts/package-windows.ps1`(BOM): 매니페스트의 FFmpeg를 받아(또는 `-CacheDir`) 크기·SHA-256·항목 안전성 검사 후 `dist/FrameTrace/tools/bin`에 넣고 `THIRD_PARTY_NOTICES.md`·라이선스·`ffmpeg-SOURCE.txt`를 포함해 `dist/FrameTrace-<ver>-win64-bundled.zip` + `SHA256SUMS`. 불일치면 실패하고 반쯤 만든 폴더를 지운다. windows-ci에 실행·ffmpeg 실행 확인·artifact 업로드 단계 추가.
- **S6** 로컬 가짜 출처(테스트 전용 127.0.0.1 HTTP — 이 예외는 `cfg(test)`에만 존재)와 직접 만든 zip으로: 해시 불일치, 크기 불일치(양방향), zip-slip(`../`, 깊은 `..`), 절대 경로, 드라이브, 역슬래시, 심볼릭 링크(일반 항목·매니페스트 항목), 누락 항목, 허용 목록 밖 호스트(연결 0회), 다른 호스트로의 리다이렉트(대상 연결 0회), 리다이렉트 루프, 중간에 끊긴 다운로드·404 정리, 잠금 충돌, `sha256: null` 거부(디스크 무변경), 설치 중 `state_lock` 비어 있음 + busy 유지 + 진행률, 실패 시 busy 해제, deepfake `tool_missing`, pip 요구사항·인자·venv 복구(가짜 인터프리터). 실제 인터넷 다운로드는 테스트하지 않는다.

### API 계약 (트랙 U)
- `GET /api/tools[?exclude_on_demand=1]` →
  `{"ok":true,"platform":"windows-x86_64","platform_supported":bool,"tools":[T…],"install":J|null}`
  - T: `name, title, label, kind("archive"|"pip"), required, on_demand, version, pinned_version, license, homepage, source_url, provides[], status("installed"|"partial"|"missing"), installed, paths{binary: 절대경로|null}, installable, unavailable_reason(=not_installable_reason; pip은 Python 없음 포함), manual_hint, source_host("github.com"/"pypi.org"/null), size_bytes|null, download{file_name,url,host,size,sha256,checksum_source}|null`
- `POST /api/tools/install` `{"name":"ffmpeg"}` (nonce 필요) → `{"ok":true,"install":J}` (J.state는 즉시 `downloading` — `idle` 아님) 또는 `{"ok":false,"code":C,"error":"…"[,"manual_hint","homepage"]}`
  - C: `unknown_tool`, `not_installable`, `unsupported_platform`, `python_required`, `install_running`, `busy`(분석·내보내기 진행 중 또는 busy 보유), `shutdown`, `spawn_failed`, `manifest_invalid`
- `GET /api/tools/install-status` → `{"ok":true,"install":J|null}` (GET /api/tools의 `install`과 같음)
- `POST /api/tools/install-cancel` → `{"ok":true,"install":J}` / `{"ok":false,"code":"no_install"}`
- J: `{"id","tool","state","stage","bytes","total","error","started_unix","finished_unix","installed_files":[],"audit_warning"}`
  - `state`: `downloading` | `verifying` | `extracting` (실행 중; 파일 이동·venv·pip도 `extracting`) | `done` | `failed`
  - `stage`(정밀): `queued|download|verify|extract|install|venv|pip|done|failed`
  - 설치 중에는 busy를 잡으므로 분석 시작 등은 기존 busy 오류로 거부된다. 성공하면 `/api/env` 캐시가 비워져 다음 호출이 다시 확인한다.
- 합성 영상 검사(`POST /api/start` + `with_deepfake:true`, `POST /api/advanced` + `tool:"deepfake"`)에서 deepfake-lens가 없으면 → `{"ok":false,"code":"tool_missing","tool_missing":"deepfake-lens","error":"…","installable":bool,"not_installable_reason":…,"manual_hint":…,"install":{"method":"POST","path":"/api/tools/install","body":{"name":"deepfake-lens"}}}` (busy를 잡기 전에 반환).

### 남은 일
- 실제 FFmpeg zip으로의 설치·패키징은 이 세션에서 내려받지 않았다 → windows-ci의 package 단계와 Windows 실기 `frametrace tools install ffmpeg`로 첫 확인 필요.
- libewf·Sleuth Kit·deepfake-lens는 공식 체크섬(또는 PyPI 배포)이 생기면 매니페스트·`REQUIREMENTS` 표에 추가.
- PATH가 tools/bin보다 먼저 검색된다(기존 `tool_policy` 동작) — 설치해도 PATH의 다른 ffmpeg가 쓰일 수 있음(CLI는 안내 출력).
