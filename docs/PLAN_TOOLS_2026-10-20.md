# 외부 도구 번들 + 안내 설치 계획 (2026-10-20, 기준 54cf3c5)

사용자 결정: **섞는다** — 필수인 FFmpeg는 배포본에 번들, 선택 도구(libewf·Sleuth Kit·deepfake-lens)는 앱 안에서 확인된 다운로드로 안내.

## 원칙
- **T1. 고정 버전·고정 해시.** 번들이든 다운로드든 도구마다 버전·URL·SHA-256을 저장소의 매니페스트(`tools/manifest.json`)에 고정한다. 해시가 다르면 설치하지 않는다(부분 파일도 남기지 않음). "latest" URL 금지.
- **T2. 허용된 출처만, HTTPS만.** 매니페스트의 호스트 허용 목록 밖으로는 내려받지 않는다. 리다이렉트도 허용 목록 안이어야 한다(FrameTrace·패키징 스크립트가 홉마다 직접 확인, curl에 `--location` 없음; `--globoff`로 URL의 `[]{}`는 글자 그대로). 인증서 오류는 그대로 실패. **프록시**: 사용자/시스템 프록시 환경 변수(`HTTPS_PROXY`·`ALL_PROXY`·`NO_PROXY`)는 회사망을 위해 **그대로 따른다** — 프록시는 허용 호스트로 가는 TLS를 중계할 뿐이고(인증서는 그 호스트의 것으로 확인), 무결성은 언제나 고정 크기·SHA-256이 결정한다(§Fix round 1).
- **T3. 사용자가 누를 때만.** 앱은 스스로 인터넷에 나가지 않는다. 설치는 사용자가 버튼/명령으로 시작하고, 받을 파일 이름·출처·크기를 먼저 보여 준다.
- **T4. 새 의존성 없이.** HTTP는 Windows 내장 `curl.exe`(10 1803+), 압축 해제는 내장 `tar.exe`(bsdtar). 둘 다 `tool_policy`로 절대 경로 해석. 압축 해제는 임시 폴더에서 하고 경로 탈출(zip-slip·절대 경로·`..`·링크) 항목이 있으면 전체 거부, 매니페스트에 적힌 파일만 `tools/bin`으로 옮긴다.
- **T5. 라이선스 고지 — FrameTrace는 외부 도구를 배포하지 않는다.** 배포 zip(`scripts/make-portable.ps1`)에는 FrameTrace 자신의 실행 파일만 들어가고 `ffmpeg.exe`·`ffprobe.exe`는 절대 넣지 않는다. 도구 설치는 사용자가 매니페스트에 고정된 **공식 출처에서 직접** 내려받는 것이며, 앱은 내려받기 **전에** 파일·출처 호스트·크기·라이선스 구성·“FrameTrace가 배포하지 않음”(`notice`)을 보여 준다. 설치된 도구의 라이선스 원문은 `tools/bin/licenses/`에 복사한다(LGPLv3 `LICENSE.txt` + 저장소에 해시 고정한 GPLv3 원문). `THIRD_PARTY_NOTICES.md`는 FrameTrace가 **실제로 배포하는 것**에 대한 고지만 담고, 사용자가 설치하는 도구는 “배포하지 않음” 절로 안내한다. 자체 빌드 LGPL FFmpeg 번들은 옵션 A(별도 PR) 전까지 없다.
- **T6. 오프라인에서도 동작.** FFmpeg가 설치돼 있으면(tools/bin 또는 PATH) 기본 기능은 인터넷 없이 동작한다. 없으면 doctor·첫 화면이 설치 안내만 하고 앱을 막지 않는다. 다운로드 실패도 기능 안내만 한다.

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

### LGPL FFmpeg와 인코더 (통합 수정, 2026-10-19)
- 번들 FFmpeg는 LGPL 빌드라 **libx264(GPL)가 없다**. 프록시·MP4 내보내기가 `-c:v libx264`로 고정돼 있어 번들만 있으면 실패했다.
- `src/ffmpeg_encoders.rs`: 해석된 ffmpeg에 `-hide_banner -encoders`를 한 번 묻고(`run_with_timeout_detailed`·콘솔 숨김·버전 확인 시간 제한, ffmpeg 경로+mtime+크기별 캐시, state_lock 밖) 순서 libx264 → h264_mf → libopenh264 → mpeg4(§Fix round 1에서 h264_mf를 openh264 앞으로)로 고른다. 인코더별 옵션: libx264 `-preset veryfast -crf 28/20`(종전 그대로), libopenh264 `-b:v 2M/8M`, h264_mf `-rate_control quality -quality 55/75`, mpeg4 `-q:v 5/3`; 모두 `-pix_fmt yuv420p`. 오디오는 네이티브 `aac`(없고 libfdk_aac만 있으면 그것), AVI는 libmp3lame 있으면 그것, 없으면 네이티브 `mp2`. 확인 자체가 실패하면 종전 가정(libx264)으로 두어 ffmpeg가 직접 오류를 낸다.
- 감사 줄(`proxy-log.jsonl`의 `make-proxy`, `export-log.jsonl`의 `export-video`)에 `video_encoder`·`audio_encoder`·`encoder_source`를 추가(`command_args`도 실제 인자). 출력 이름·컨테이너(mp4/avi)는 그대로.
- `frametrace doctor`의 ffmpeg 줄, `frametrace tools list`, `GET /api/tools`의 ffmpeg 항목 `encoders{probed,libx264,h264,aac,avi_audio,note}`이 libx264 없는 빌드를 알린다(예: "LGPL 빌드 — H.264는 h264_mf 사용"; H.264가 하나도 없으면 doctor는 WARN).
- deepfake-lens는 **PyPI에 없다** → README·`docs/DEEPFAKE_LENS.md`·`src/deepfake.rs`의 `pip install deepfake-lens` 안내를 "받은 체크아웃/휠에서 `pip install <경로>`, 또는 `FRAMETRACE_DEEPFAKE_LENS`/`FRAMETRACE_DEEPFAKE_LENS_HOME`"으로 고쳤다.
- (§Fix round 1에서 정정) 고정한 BtbN 태그의 `scripts.d/50-openh264.sh`는 lgpl 변형에서도 켜져 있어(`ffbuild_enabled`가 항상 0) **libopenh264가 소스에서 빌드되어 정적으로 들어간다** — 처음 순서(openh264 우선)라면 번들은 h264_mf가 아니라 libopenh264를 골랐다. 소스 빌드 openh264는 Cisco 특허 라이선스(Cisco가 배포한 바이너리에만 적용)를 받지 못하므로 순서를 libx264 → h264_mf → libopenh264 → mpeg4로 바꿨고, windows-ci가 번들 ffmpeg의 `-encoders`로 실제 선택을 기록하며 libopenh264면 실패한다. h264_mf 인자는 이 PC의 full 빌드(7.1)로 실제 인코딩해 확인했다.

### 남은 일
- 실제 FFmpeg zip으로의 설치·패키징은 이 세션에서 내려받지 않았다 → windows-ci의 package 단계와 Windows 실기 `frametrace tools install ffmpeg`로 첫 확인 필요.
- libewf·Sleuth Kit·deepfake-lens는 공식 체크섬(또는 PyPI 배포)이 생기면 매니페스트·`REQUIREMENTS` 표에 추가.
- ~~PATH가 tools/bin보다 먼저 검색된다~~ → §Fix round 2에서 매니페스트 도구는 tools/bin 우선으로 바꿈.

## §Fix round 1 (2026-10-19, 공급망 리뷰 반영, 기준 ff96d9b)

리뷰어가 확인한 7개 항목을 모두 고쳤다. 테스트는 모두 가짜 출처(127.0.0.1)·가짜 zip만 쓰고 실제 인터넷·실제 바이너리를 받지 않는다.

| # | 문제 | 수정 | 테스트 |
|---|---|---|---|
| 1 (중) | 고정한 BtbN 태그의 `scripts.d/50-openh264.sh`가 lgpl에서도 켜져 있어 번들 ffmpeg에 **소스 빌드 libopenh264**가 정적으로 들어 있다 → 종전 순서(openh264 우선)면 번들은 h264_mf가 아니라 openh264를 골랐다. 소스 빌드 openh264는 Cisco 특허 라이선스 대상이 아니다. | 순서를 **libx264 → h264_mf → libopenh264 → mpeg4**로(`VideoEncoder::PREFERENCE`; h264_mf는 Windows에만 있으므로 다른 OS에서는 사실상 libx264 → libopenh264 → mpeg4). README·THIRD_PARTY_NOTICES·위 "LGPL FFmpeg와 인코더" 절 정정. windows-ci에 "Bundled FFmpeg encoder pick" 단계: 번들 `ffmpeg -hide_banner -encoders`의 H.264 목록과 `-buildconf`(openh264·chromaprint·gpl·version3) 기록, PATH를 System32로 줄여 `dist/FrameTrace/frametrace.exe tools list --json`이 번들 ffmpeg를 해석하는지 확인하고 그 `encoders.h264`를 `dist/ffmpeg-encoder-pick.txt`로 남김 — **libopenh264면 실패**. | `ffmpeg_encoders::tests::picks_in_preference_order`(둘 다 있으면 h264_mf, openh264만 있으면 openh264, x264 최우선, 순서 배열), `btbn_lgpl_bundle_never_picks_libopenh264`(BtbN lgpl 형태 목록). CI 단계는 이 PC의 full 빌드로 같은 스크립트를 돌려 확인(해석·probe·기록 동작). |
| 2 (중) | 번들에 `LICENSE.txt`(COPYING.LGPLv3)만 있었다 — LGPLv3는 GPLv3를 참조로 포함하므로 GPLv3 원문도 필요. | `tools/licenses/COPYING.GPLv3` = FFmpeg 저장소의 그 커밋(`29e619e767cde9045a75c29bc9a8278ae7b3a98b`) 원본(35 147 B, SHA-256 `8ceb4b9e…5b903`; gnu.org 현재본과는 fsf.org 주소의 http/https만 다름). 매니페스트 `bundled_licenses`에 SHA-256 고정, `include_str!`로 컴파일, `validate_manifest`가 해시 불일치·미포함을 거부. 가이드 설치와 패키징 모두 `tools/bin/licenses/ffmpeg-COPYING.GPLv3.txt`로 설치. `.gitattributes`에 `tools/licenses/** -text`(autocrlf가 바이트를 바꾸지 않게). | `shipped_ffmpeg_entry_installs_the_pinned_gplv3_text`, `bundled_license_must_match_its_pin_and_be_compiled_in`, Windows `installs_the_pinned_gplv3_text_next_to_the_archive_license`, `scripts/test-package-windows.mjs`(zip에 들어가고 바이트가 고정값). |
| 3 (중) | 정적 링크된 서드파티 라이브러리·라이선스·고지·소스 제공이 문서화되지 않았다. | BtbN 태그 `autobuild-2026-09-30-13-08`(커밋 `6c9aec5f…`)의 `scripts.d`를 받아 `generate.sh`의 선택 규칙(win64 · lgpl · 8.1 addin, `ffbuild_enabled`, 디렉터리 단계는 마지막 스크립트가 켜져야 빌드)대로 켜진 단계를 뽑고, 각 `SCRIPT_REPO`·`SCRIPT_COMMIT`(또는 태그/SVN rev)을 THIRD_PARTY_NOTICES.md 표에 라이선스와 함께 적었다. 각 업스트림의 라이선스 파일(그 커밋)을 모은 `tools/licenses/ffmpeg-THIRD-PARTY-LICENSES.txt`(매니페스트 `package_licenses`, SHA-256 고정; 패키징만)를 `tools/bin/licenses/`에 넣는다. 3년 서면 소스 제공 문구 추가, 포함/불포함 범위 명시. | `package_only_license_files_exist_with_their_pinned_hash`, `test-package-windows.mjs`. |
| 4a (하) | curl URL 글로빙. | `curl --globoff`(`archive::curl_args`로 분리). | `curl_never_globs_follows_redirects_or_overrides_the_proxy`, `bracketed_urls_are_fetched_literally_not_as_a_curl_glob`(`/pkg[1].zip`을 글자 그대로 요청, 1회). |
| 4b (하) | 프록시 동작 미정. | **사용자/시스템 프록시 환경 변수는 그대로 따른다**(회사망). `--proxy`/`--noproxy`를 주지 않는다. 프록시는 TLS를 중계할 뿐 인증서는 허용 호스트의 것이고, 무결성은 고정 크기·SHA-256이 결정한다. T2와 README에 기록. | `curl_never_globs_…`(`--proxy`/`--noproxy`/`--location`/`--insecure` 없음). |
| 4c (하) | `package-windows.ps1`이 `--location`으로 임의 호스트까지 따라갔고 `Compress-Archive`(역슬래시 항목)를 썼다. | 홉마다 `Assert-AllowedUrl`(https·기본 포트·자격 증명 없음·허용 호스트)로 확인한 뒤 `--max-redirs 0`으로 한 홉씩(최대 5), `--globoff`. zip은 `[IO.Compression.ZipFile]::CreateFromDirectory` — Windows PowerShell 5.1(.NET Framework)은 기본값으로 **역슬래시를 써서**(실제로 확인) `Switch.System.IO.Compression.ZipFile.UseBackslash`를 끄고, 쓴 뒤 모든 항목 이름을 다시 열어 `\`가 있으면 실패. `-Version`, `-TestLoopbackHttp`(127.0.0.1의 http만, 오프라인 테스트용) 매개변수 추가. | `node scripts/test-package-windows.mjs`(가짜 zip·가짜 저장소 루트): 허용 리다이렉트 1홉 성공, 항목 이름 전부 `/`, 필요한 파일·라이선스 포함·목록 외 파일 제외, SHA256SUMS 일치, 허용 밖 호스트로의 리다이렉트는 연결 0회로 거부, https 다른 호스트 리다이렉트 거부, 루프 5회 제한, 해시 불일치 시 아무것도 안 남음, 허용 밖 매니페스트 URL 거부. windows-ci에 단계 추가. |
| 4d (하) | 취소가 다운로드 중에만 동작하고 다른 단계에서는 조용히 무시됐다. | `Progress`의 취소를 3상태 래치(none/requested/locked, compare-exchange 하나)로. 단계 경계(잠금 직후, 홉마다, 검증 전, 압축 해제 전)에서 확인, 다운로드 중에는 curl 종료. 파일 이동(`Install`)·venv·pip에 들어갈 때 `enter_non_cancellable` — 그 뒤 취소는 **`{"ok":false,"code":"not_cancellable","error":…,"install":J}`**. 경쟁에서 취소가 먼저면 그 단계에 들어가지 않고 `install cancelled`로 끝난다. | `cancel_is_honoured_until_a_non_cancellable_stage_then_refused`, `cancel_during_a_non_cancellable_stage_is_refused_with_its_own_code`(API), Windows `a_cancel_before_the_first_hop_contacts_nothing_and_cleans_up`, `a_cancel_mid_download_stops_the_install_and_cleans_up`(API, 진행 중 취소 → failed·busy 해제·스테이징 없음). |
| 4e (하) | 강제 종료된 설치의 `tools/.staging-*`가 남았다. | `clean_stale_staging`: 설치 잠금(`tools/.install.lock`)을 잡은 뒤 이번 실행 시작 전에 수정된 `.staging-*` 디렉터리만 지운다(링크·파일은 건드리지 않음, 지우지 못한 것은 다음에). 설치 직전과 **서버 시작 시**(`serve::run_on`; tools 폴더가 있고 잠금이 비어 있을 때만, 폴더를 만들지 않음). | `stale_staging_dirs_are_swept_but_not_live_or_foreign_entries`, `server_start_sweep_skips_while_another_install_holds_the_lock`, Windows `stale_staging_of_a_killed_install_is_swept_before_installing`. |
| 4f (하) | 시작 전 거부가 감사 로그에 없었고, 로그가 체인이 아니었다. | API의 모든 거부(`unknown_tool`·`not_installable`·`unsupported_platform`·`python_required`·`install_running`·`shutdown`·`busy`)를 한 줄씩 기록(**정정**: CLI의 unknown/not installable은 `install()` 전에 반환해 실제로는 기록되지 않았다 — §Fix round 2 B-4에서 고침) — 상태 잠금을 놓은 뒤. 쓰기 실패는 거부 응답의 `audit_warning`. 로그 쓰기는 다른 감사 로그와 같은 `audit::append_chained_jsonl`. | `refusals_are_chained_onto_existing_install_audit_lines`(체인 검증·편집 시 검증 실패·이름 64자 제한·줄 주입 없음), `install_refusals_carry_stable_codes_and_start_nothing`·`busy_workstation_refuses_…`·`null_sha256_…`·`installs_report_unsupported_platform_off_windows`(거부 줄만 생기고 다른 파일 없음, 체인 검증). |
| 4g (하) | `tool_policy` PATH 검색이 상대 항목(`.`, `bin`)도 현재 폴더 기준으로 찾았다. | 절대 경로 항목만 검색(Windows의 `\bin`처럼 드라이브 상대인 것도 제외). | `tool_policy::tests::relative_path_entries_are_never_searched`. |

### `tools/install-audit.jsonl` 형식 (변경)
한 줄 = JSON 객체 하나, 다른 감사 로그와 같은 체인: 본문 필드 뒤에 `previous_entry_sha256`(직전 줄 바이트의 SHA-256, 첫 줄은 `GENESIS`)과 `entry_sha256`(그 필드까지의 줄의 SHA-256), 감사 키가 설정돼 있으면 `entry_hmac_sha256`·`entry_hmac_key_id`. 본문:
- 설치 시도: `ts_unix, event:"tool-install", tool, kind, version, url, host, size, sha256, checksum_source, requirements_sha256, host_id, result:"ok"|"failed", files[] | error`
- 시작 전 거부: `ts_unix, event:"tool-install", tool(최대 64자), result:"refused", code, error, host_id`
종전(체인 없는) 줄이 있는 파일에도 이어 쓴다(첫 체인 줄의 `previous_entry_sha256`이 마지막 옛 줄의 해시).

### API 변경 (트랙 U)
- `POST /api/tools/install-cancel`: 새 거부 코드 **`not_cancellable`** — `{"ok":false,"code":"not_cancellable","error":"지금 단계(파일 이동·Python 환경 구성)는 취소할 수 없습니다 — 끝날 때까지 기다리십시오.","install":J}`. UI는 이때 취소 버튼을 비활성/안내로 바꾸면 된다(J.stage가 `install|venv|pip`).
- 취소된 설치는 `state:"failed"`, `error:"install cancelled"`.
- `POST /api/tools/install` 거부 응답에 `audit_warning`(감사 줄을 못 쓴 경우만)이 붙을 수 있다.
- `GET /api/tools`의 ffmpeg `encoders.h264`는 번들에서 `h264_mf`가 된다(종전 `libopenh264`).

### 확인 못 한 것 / 남은 일
- 정적 라이브러리 목록은 BtbN 스크립트에서 **생성 규칙대로 도출**한 것이다(바이너리를 받지 않음). windows-ci의 새 단계가 실제 `-buildconf`를 기록하므로 첫 CI 실행에서 목록과 대조할 것.
- **FFTW3(GPL-2.0-or-later)** 가 chromaprint(`-DFFT_LIB=fftw3`, `--enable-chromaprint`)를 통해 lgpl 변형에도 정적으로 들어간다(libzvbi도 COPYING.md상 대부분 GPL-2.0-or-later, `src/packet-830.*`·`src/pdc.*`는 GPL-2.0) — 이름과 달리 번들 `ffmpeg.exe` 전체는 GPL 조건으로 재배포되는 것으로 다뤄야 한다(FrameTrace 자체는 별도 프로그램으로 실행하므로 MIT 그대로). THIRD_PARTY_NOTICES에 적었다. 원치 않으면 다음 라운드에서 다른 빌드(예: chromaprint 없는 빌드)로 바꾸는 결정이 필요.
- 서면 소스 제공(3년)은 문구만 넣었다 — 실제로 소스를 낼 수 있도록 소유자가 BtbN 태그의 전체 소스(각 `SCRIPT_COMMIT` + FFmpeg `29e619e767`)를 보관해야 한다. 연락 창구는 저장소 이슈로 적었다(소유자 확인 필요).
- h264_mf 인코딩 품질/호환은 이 PC의 full 빌드로 확인한 것이고, 번들 바이너리로의 실제 인코딩은 Windows CI/실기 확인이 필요.

## §Fix round 2 (server) (2026-10-19, 2차 리뷰 반영, 기준 3c076a0)

2차 Fable 리뷰가 확인한 서버 쪽 항목 중 **번들·라이선스 결정과 무관한 것만** 고쳤다. THIRD_PARTY_NOTICES 문구·매니페스트 `license` 필드·CI buildconf 게이트는 소유자 결정을 기다리므로 손대지 않았다. 테스트는 모두 가짜 출처·가짜 ffmpeg만 쓴다.

| # | 문제 | 수정 | 테스트 |
|---|---|---|---|
| B-4 (중) | `frametrace tools install`의 unknown / not installable 거부가 `install()` 전에 반환돼 감사 줄이 없었다(§Fix round 1 4f와 보안 리뷰는 "모든 거부 기록"이라고 적었음). | 새 `tool_install::preflight(manifest, name, dirs)`: 도구 찾기·설치 가능 확인, 거부면 체인 감사 줄(`audit_refusal`, `result:"refused"`, `code: unknown_tool / not_installable`(Windows 밖은 `unsupported_platform`))을 남기고 오류(+기록 실패 시 경고)를 돌려준다. CLI는 확인 프롬프트 **전에** 이것을 부른다. `install_entry`도 같은 코드 함수(`not_installable_code`)를 쓴다. 4f 문장 정정. 사용자가 프롬프트에서 취소하거나 비대화형에서 `--yes`가 없는 경우는 설치 시도가 아니라서 기록하지 않는다. | `cli::handlers::tools::tests::cli_refusals_are_audited_before_the_prompt`(없는 도구·libewf → 거부 줄 2개만, 체인 검증, 다른 파일 없음) |
| B-5 (중) | 인코더 선택이 `-encoders` 목록만 봤다. `h264_mf`는 Windows 빌드마다 목록에 있지만 Windows N/KN(Media Feature Pack 없음)·Server Core에서 `MFStartup`에서 실패 → libx264 없는 번들은 모든 프록시·내보내기가 실패하고 대안이 없었다. | `ffmpeg_encoders::for_binary`: 목록에 있는 후보를 선호 순서대로 **1프레임 시험 인코딩**(`-hide_banner -nostdin -loglevel error -f lavfi -i color=c=black:s=64x64:d=0.04 -frames:v 1 <프록시와 같은 영상 인자> -an -f null -`, `run_with_timeout_detailed`(콘솔 숨김·버전 확인 시간 제한), state_lock 밖) — 성공한 첫 후보에서 멈추고 실패한 것은 건너뛴다. 결과는 `-encoders` 결과와 함께 캐시(경로+mtime+크기). 시험 자체를 못 돌리면(실행 실패·시간 초과) 종전처럼 그 후보를 쓰고 캐시하지 않는다. doctor·`tools list`·`/api/tools`의 note에 "h264_mf 사용 불가 — Media Feature Pack 필요 (Windows N/KN 에디션·Server Core)" 추가, `encoders.trial_failed`(실패한 인코더 이름 목록). h264_mf 인자는 이 PC의 full 7.1 빌드로 64×64 시험이 성공함을 확인(229 ms). **주의(결정 대기와 관련)**: 번들에서 h264_mf가 실패하면 순서상 다음은 소스 빌드 libopenh264다 — windows-ci의 "libopenh264면 실패" 게이트는 정상 Windows에서의 선택만 본다. | `ffmpeg_encoders::tests::a_listed_encoder_that_fails_its_trial_is_skipped`(가짜 ffmpeg: h264_mf 시험 실패 → libopenh264, note·`trial_failed`, 호출 3회, 캐시 후 재호출 0회; 모두 실패 → mpeg4; 정상이면 h264_mf에서 멈춤), `trial_uses_the_proxy_options_and_writes_nothing` |
| B-7 (하) | PATH가 tools/bin보다 먼저였고, 기본 사용자 PATH에는 사용자가 쓸 수 있는 폴더(WindowsApps, Python Scripts, npm, `.cargo\bin`)가 있어 심어 둔 ffmpeg가 해시 고정 설치본을 가렸다(감사는 "ok"). | `tool_policy`: 매니페스트 `provides`에 있는 이름(ffmpeg, ffprobe, ewfinfo/ewfverify/ewfexport, mmls/fls/icat, deepfake-lens)은 **`<exe dir>/tools/bin` 먼저**, 없으면 PATH. 그 밖의 도구(python 등)는 종전대로 PATH 먼저. 새 `tool_policy::shadowed_by(name)`: tools/bin 것이 쓰이고 PATH에 **다른** 사본이 있으면 그 경로(쓰지 않음 — 확인용). doctor 줄, `tools list`(주의 줄)·`--json`, `GET /api/tools`에 노출. CLI 설치 안내 문구도 새 순서로. | `tool_policy::tests::manifest_tools_resolve_from_tools_bin_before_path_and_report_the_shadow` |
| B-8 (하) | 기존 `tools/bin/ffmpeg.exe`가 실행 중이면 이름 바꾸기는 되지만 `StagingDir` Drop의 `remove_dir_all`이 실패 → `.staging-*/backup/0.bak`이 남는데 설치는 "ok". | `StagingDir::close()`가 성공 경로에서 직접 지우고 못 지우면 경로를 돌려준다. `InstallOutcome.staging_left`(pip은 못 지운 옛 venv 또는 스테이징) → 감사 줄 `staging_left`, CLI "참고: 이전 파일이 아직 실행 중이라 임시 폴더를 지우지 못했습니다 — <경로> (다음 설치·서버 시작 때 정리)", API 작업 J의 `staging_left`. 남은 `.staging-*`는 프로세스가 끝난 뒤 다음 설치·서버 시작의 정리에서 지워진다. | Windows `a_running_old_binary_is_reported_as_staging_left_not_silently_ok`(복사한 cmd.exe를 tools/bin/fake.exe로 실행해 둔 채 설치 → 새 파일 설치됨, `staging_left`에 `backup/0.bak`, 감사 줄 "ok"+`staging_left`, 종료 후 정리 1개), `audit_line_records_source_hash_and_result_without_signed_query` 확장 |
| B-9 (하) | `package-windows.ps1`이 리다이렉트 `Location`을 `Assert-AllowedUrl` 전에 `[Uri]`로 캐스트 → 절대 URI가 아닌 Location이면 "refused URL" 대신 캐스트 오류. | 다음 홉을 `Assert-AllowedUrl $location`으로 먼저 검사하고 그 결과로 로그를 찍는다. (curl의 `%{redirect_url}`은 보통의 상대 Location을 현재 URL 기준 절대 URL로 풀어 주고, `http:x`처럼 못 푸는 것은 그대로 넘긴다 — 직접 확인.) | `scripts/test-package-windows.mjs`: `Location: http:x` → "refused URL (not absolute)", 캐스트 오류 없음, 그 뒤 연결 0회, 스테이징 없음; `Location: real/ffmpeg.zip`(curl이 풀어 줌) → 정상 패키징 |

### API 변경 (트랙 U)
- `GET /api/tools`의 각 도구 T에 **`shadowed_by`**: `{"<binary>": "<PATH의 다른 사본 절대 경로>" | null, …}` (`paths`와 같은 키). 값이 있으면 그 바이너리는 tools/bin 것이 쓰이고, PATH에 FrameTrace가 **쓰지 않는** 다른 사본이 있다는 뜻 — UI는 "PATH에 다른 ffmpeg가 있습니다(사용 안 함): <경로>. 직접 둔 것이 아니면 확인하십시오." 정도의 주의를 보이면 된다. `frametrace tools list --json`도 같은 필드.
- ffmpeg 항목 `encoders`에 **`trial_failed`**: 목록에는 있지만 시험 인코딩이 실패해 건너뛴 인코더 이름 배열(보통 `[]`, N/KN에서는 `["h264_mf"]`). `note`는 그때 "… · h264_mf 사용 불가 — Media Feature Pack 필요 (Windows N/KN 에디션·Server Core)"를 포함한다. `h264`는 실제로 쓸 인코더.
- 설치 작업 J에 **`staging_left`**: 완료(`done`)됐지만 지우지 못한 임시 폴더의 절대 경로 또는 `null`. 값이 있으면 설치는 성공이고, "이전 파일이 아직 실행 중이라 임시 폴더가 남았습니다(다음 시작 때 정리)" 정도의 정보 표시.
- 설치 작업 J에 **`cancelled`**: `true`면 사용자의 취소로 끝난 `failed`(오류가 아님) — UI가 `error` 문자열(`install cancelled`)을 비교할 필요가 없다. 그 밖에는 `false`.
- 감사 줄(`tools/install-audit.jsonl`): 성공 줄에 선택 필드 `staging_left`. CLI의 unknown/not installable 거부도 이제 `result:"refused"` 줄을 남긴다.

### 확인 못 한 것
- Windows N/KN·Server Core 실기에서의 h264_mf 실패는 재현하지 못했다(이 PC는 일반 에디션) — 가짜 ffmpeg로 분기만 확인. 실제 MF 실패의 종료 코드가 0이 아닌지는 실기 확인 필요(ffmpeg는 인코더 열기 실패 시 0이 아닌 코드로 끝난다).

## §Fix round 3 (2026-10-19, 2차 리뷰 확인 반영, 기준 f321d0b)

리뷰어가 확인한 항목을 모두 고쳤다. 번들·라이선스 결정(고지 문구, 매니페스트 `license`, CI 인코더 게이트, 인코더 선호 순서)은 손대지 않았다. 각 수정의 테스트는 f321d0b에서 실패함을 확인했다(아래 "확인 방법").

| # | 문제 | 수정 | 테스트 |
|---|---|---|---|
| 1 (중) | 시험 인코딩이 **멈추면**(h264_mf가 `MFStartup`에서 대기) `Trial::Unknown`이 되어 캐시되지 않았고, 시험 시간 제한이 버전 확인과 같은 60 s라 `for_binary`를 부르는 모든 곳(GET /api/tools, doctor, tools list, 모든 프록시·내보내기)이 매번 60 s를 기다렸다. 동시에 부르면 각자 따로 실행했다. | `ffmpeg_encoders`: 시험 전용 제한 **`TRIAL_TIMEOUT_SECS` = 10 s**, 시간 초과는 **`Fails`**(64×64 한 프레임을 10 s 안에 못 하면 쓸 수 없음). 실행 자체를 못 하면(`RunError::Failed`) `Unknown`. 결론 없는 결과(`-encoders` 실패·빈 목록, `Unknown`, 아래 하네스 결함)는 **`INCONCLUSIVE_TTL` = 60 s 동안 음성 캐시**. 캐시 항목이 `Ready{encoders, expires}` / `Pending(flight)`이 되어 같은 바이너리를 **동시에 부르면 한 번만 실행**(single flight; 나머지는 Condvar로 기다림, 실행 쪽이 패닉해도 guard가 가정값을 알리고 Pending을 지움). 어떤 후보든 실패한 뒤에는 **네이티브 mpeg4를 대조군으로 마지막에 반드시 시험**(목록에 없어도) — mpeg4마저 실패하면 인코더가 아니라 시험 하네스 문제로 보고 실패 기록을 모두 지운 채 목록 기반 선택을 유지(→ "Media Feature Pack 필요" 오진 없음), 결론 없음으로 TTL 캐시. | `a_hung_trial_encode_fails_within_the_trial_bound_and_is_cached`(가짜 ffmpeg: h264_mf 시험이 25 s 잠 → ~10 s에 `Fails`, libopenh264 선택, 호출 3회, 재호출 즉시·실행 0회), `concurrent_callers_share_one_probe`(4 스레드 동시 → `-encoders` 1회 + 시험 1회), `a_failing_mpeg4_control_is_a_harness_defect_and_is_negative_cached`(모두 실패 → `trial_failed: []`, h264_mf 유지, MF 문구 없음, TTL 안 재실행 0회·만료 뒤 재실행; mpeg4가 목록에 없어도 대조군 시험; 대조군 성공이면 h264_mf 실패는 진짜; 빈 목록도 음성 캐시) |
| 2a (하) | `install-cancel`에 작업 id가 없어, 탭 A의 늦은 [중단]이 탭 B가 새로 시작한 설치를 멈췄다. | 본문 `{"id": <작업 id>}`(선택; 숫자·문자열). 현재 작업 id와 다르면 **`{"ok":false,"code":"stale_job","error":…,"install":J}`**, 아무것도 멈추지 않음. `id`가 없으면(옛 UI) 종전 동작. UI는 `HT.job`을 보내고 `stale_job`을 `no_install`처럼 다룬다(오류 문구 없음). | `serve::tests_tools::a_cancel_naming_another_job_is_refused_as_stale`, unit "Tools [중단] names the job", 흉내 서버 `stale_job` |
| 2b (하) | `install_job_json`의 `json!`에 `"cancelled"` 키가 두 번. | 하나 삭제. | `tool_replies_carry_the_server_clock_and_the_job_age`(함수 본문에 `"cancelled":` 1회) |
| 2c (하) | pip이 지우지 못한 옛 venv `tools/python/.<name>.old-*`는 아무도 치우지 않는데 CLI는 "(다음 설치·서버 시작 때 정리)"라고 했다. | `clean_stale_staging`이 `tools/.staging-*`와 함께 **`tools/python/.<이름>.old-*`**(디렉터리, 링크 아님, 이번 실행 전 수정)도 지운다(설치 잠금 아래, 설치 직전·서버 시작 때). CLI 문구: "… (그 프로그램을 닫으면 다음 설치·서버 시작 때 정리됩니다)" — 실행 중인 동안은 지울 수 없으므로. | `stale_staging_dirs_are_swept_but_not_live_or_foreign_entries`(옛 venv 삭제, 현재 venv·파일·이번 실행 것 유지), `cli::handlers::tools::tests::staging_left_note_says_when_it_is_cleaned` |
| 2d (하) | `shadowed_by("deepfake-lens")`가 실제 순서(개인 venv 먼저)와 맞지 않았고, `status()`가 바이너리마다 PATH를 두 번씩 훑었다. | `tool_policy::shadowed_by`는 deepfake-lens에 항상 `None`. 새 `PathDirs`(PATH의 절대·존재 디렉터리를 한 번 정규화)와 `lookup_bare(name, &PathDirs) → {used, shadowed_by}`(해석과 그림자를 같은 탐색에서). `tool_install::status`는 도구당 **PATH 탐색 1회**, deepfake-lens는 `resolved_lens_binary()` + 그림자 `None`. | `tool_policy::tests::manifest_tools_resolve_from_tools_bin_before_path_and_report_the_shadow`(확장: deepfake-lens는 tools/bin·PATH 둘 다 있어도 `None`), `tool_install::tests::status_walks_path_once_and_never_shadows_deepfake_lens`(테스트용 스레드 카운터) |
| 2e (하·UI) | 확인 대화상자가 떠 있는 동안 다른 설치를 따라가게 되면 OK가 조용히 아무것도 안 했다. | `htAdopt`가 대화상자 중이면 "다른 설치가 진행 중입니다"를 알리고, OK 뒤에도 POST 없이 같은 문구를 다시 알린다(`htAnnounceBusy`, 같은 문구 연속도 알림). | unit "Tools confirm dialog open while another install is adopted" |
| 2f (하·UI) | `htRecent`가 브라우저 시계로 "최근 5분"을 판단했다. | 서버가 **`now_unix`**(GET /api/tools·install-status)와 J의 **`age_s`**(끝난 뒤 초, 서버 시계; 실행 중 null)를 보낸다. `htParseInstall(i, now_unix)`가 `age_s`(없으면 `now_unix - finished_unix`)를 만들고 `htRecent`는 그것만 쓴다 — 브라우저 시계는 두 값이 다 없는 옛 서버에서만. | `tool_replies_carry_the_server_clock_and_the_job_age`, unit "Tools \"recent\" is judged by the server clock"(서버 시계가 한 시간 늦음/빠름, `age_s` 우선) |
| 2g (하·UI) | POST 응답이 사라지면(네트워크·status 0) 서버가 설치를 시작했어도 "설치 요청 실패"로 보였다. | 응답 없음·`network`·`malformed`·status 0이면 `htLoad()`로 묻고 그 도구의 실행 중 작업을 따라간다; 방금 끝났으면 그 결과(`htFinish`), 아무것도 없으면 "설치 상태를 알 수 없습니다 — 다시 시도하십시오". HTTP 오류 응답(5xx 등)은 종전대로 거부 문구. | unit "Tools a lost POST reply" |
| 2h (하·UI·문서) | 계약 §28·흉내 서버에 `shadowed_by`·`encoders.trial_failed`·`staging_left`·`now_unix`·`stale_job`이 없었고, UI가 그림자를 보이지 않았다. | `docs/ui-rebuild-contract.md` §28에 모두 추가. `tools-mock.mjs`: 픽스처 T에 `shadowed_by`(paths와 같은 키), 설치 뒤 ffmpeg `encoders{…, trial_failed: []}`, J에 `staging_left`·`age_s`, 두 GET에 `now_unix`, 다른 id의 취소에 `stale_job`. UI: `shadowed_by`에 값이 있으면 그 줄에 `<small class="h-tool-shadow">` **"PATH의 다른 사본 무시됨"**(title: 경로 + "직접 둔 것이 아니면 확인하십시오"); 설치된 도구도 이때만 정보 줄로 보인다. | unit "Tools shadow warning", "Tools mock + fixture carry the round-3 fields" |

### API 변경 (트랙 U)
- `GET /api/tools`·`GET /api/tools/install-status`: 최상위 **`now_unix`**(서버 시계).
- 설치 작업 J: **`age_s`**(끝난 지 몇 초, 서버 시계; 실행 중 `null`). `cancelled` 키는 하나.
- `POST /api/tools/install-cancel`: 본문 **`{"id": <J.id>}`**(선택). 다르면 `{"ok":false,"code":"stale_job","error":"그 설치는 이미 끝났습니다 — 지금 진행 중인 설치는 중단하지 않았습니다.","install":J}`.
- deepfake-lens의 `shadowed_by`는 항상 `null`.
- ffmpeg `encoders.trial_failed`: 시간 초과(10 s)도 실패로 들어간다. mpeg4 대조군까지 실패하면 `[]`(하네스 결함, 목록 기반 선택).

### 확인 방법
- f321d0b를 `C:/s/r3old`에 풀고(git archive) 새 Rust 테스트를 옛 API에 맞춰 옮겨(`api_tools_install_cancel(&state)`, `shadow_in(…)`, 캐시 만료 생략) 별도 target(`C:/s/t3`)에서 실행 → 아래 "f321d0b에서" 열 참고. 새 API에만 있는 것(`staging_left_note`, `path_walks`)은 옛 코드에서 컴파일되지 않는다(= 실패).
- JS: `assets/app/56-tools.js`·`scripts/lib/tools-mock.mjs`만 f321d0b 것으로 바꾸고 `verify-review-batch --unit`을 돌려 새 행 7개(기존 [중단] 행의 `{id}` 확인 포함)가 모두 FAIL, 되돌리면 모두 PASS.

### 확인 못 한 것
- 실제 h264_mf가 `MFStartup`에서 멈추는 상황(Windows N/KN·Server Core)은 재현하지 못했다 — 가짜 ffmpeg의 잠자는 시험으로만 확인.
- Linux/macOS clippy·테스트는 이 PC(Windows)에서 돌리지 못했다 — 바꾼 `cfg(not(windows))` 코드는 테스트용 가짜 ffmpeg의 `sleep` 분기뿐이다.

## §옵션 C (2026-10-20)

소유자 결정(2026-10-20): **옵션 C.**
- 고정한 BtbN `win64-lgpl` 빌드는 이름과 달리 **GPL-2.0-only(zvbi `src/packet-830.*`·`src/pdc.*`) + GPL-2.0-or-later(FFTW3) + LGPLv3 + Apache-2.0(openssl 등)** 이 섞여 있어 FrameTrace가 재배포할 수 없다(2차 Fable 공급망 리뷰 **B-1 Blocker**). 따라서 **배포 zip에 FFmpeg를 넣지 않는다.**
- 앱 안의 해시 고정 설치(`frametrace tools install ffmpeg`, 첫 화면 [설치] 버튼)는 **그대로 둔다** — 사용자가 BtbN GitHub 릴리스에서 **직접** 내려받으므로 FrameTrace는 배포자가 아니다.
- 자체 빌드한 진짜 LGPL FFmpeg 번들(옵션 A)은 **별도 PR**(부록 A).

구현 계획·행 목록은 `docs/PLAN_TOOLS_C_2026-10-20.md`.

§S5·§Fix round 1 #2·#3의 패키징·소스 제공 부분은 이 결정으로 폐기(이력).
## §Fix round 4 (2026-10-19, 3차 리뷰 확인 반영, 기준 e90257d)

3차 확인 리뷰의 항목을 모두 고쳤다. 번들·라이선스 결정(고지 문구, 매니페스트 `license`, CI 인코더 게이트, 인코더 선호 순서)은 손대지 않았다. 새 테스트는 e90257d에서 실패함을 확인했다(아래 "확인 방법").

| # | 문제 | 수정 | 테스트 |
|---|---|---|---|
| 1 (중) | 시험 인코딩 **시간 초과가 영구 `Fails`로 캐시**됐다(`(encoders, true)`). 한 번 느린 순간(첫 MF 초기화, 백신이 MFT DLL 검사)이면 정상 h264_mf가 프로세스가 끝날 때까지 빠지고, 조용히 libopenh264(라이선스상 피해야 할 빌드)가 선택되며 "Media Feature Pack 필요" 오진이 고정됐다. libopenh264가 성공하면 mpeg4 대조군도 돌지 않았다. | `ffmpeg_encoders`: 시간 초과는 새 `Trial::TimedOut`. 그 후보를 **2배 제한(20 s)으로 한 번 더** 시험하고, 성공하면 정상 선택(영구 캐시). 또 시간 초과면 이번에는 건너뛰되(`trial_failed`에 넣고 새 **`trial_timed_out`**에도 넣음) 결과는 **`TIMED_OUT_TTL` = 10분**만 재사용 — 만료 뒤 다시 시험한다(정말 멈춘 h264_mf가 매분 30 s씩 붙잡지 않도록 60 s보다 길게). 영구 캐시는 0이 아닌 종료 코드로 실패한 경우뿐. note는 시간 초과면 "h264_mf 시험 인코딩 시간 초과 — 잠시 뒤 다시 확인"(MF 팩 문구 아님). `FlightGuard::finish`가 `bool` 대신 TTL(`Option<Duration>`)을 받는다. | `a_timed_out_trial_is_retried_once_and_not_cached_for_good`(가짜 ffmpeg: h264_mf 35 s 잠 → 10 s + 20 s 재시도 뒤 건너뜀, 호출 4회, `trial_timed_out`, MF 문구 없음, 만료 시각 ≈ 10분(영구 아님), TTL 안 재실행 0회; `awake` 표시 파일을 만들고 만료 → 다시 시험해 h264_mf 선택, 이번엔 영구), `a_slow_first_trial_succeeds_on_the_retry`(13 s → 재시도에서 성공, libopenh264 시험 없음, 영구), `a_listed_encoder_that_fails_its_trial_is_skipped`(확장: 진짜 0이 아닌 종료 → 영구, 만료시켜도 재실행 없음). 기존 "no second probe" 테스트(`a_hung_trial_encode_fails_within_the_trial_bound_and_is_cached`)는 위 첫 테스트로 대체. |
| 2 (하) | mpeg4 대조군: 목록에 없어도 시험했고, 시간 초과도 하네스 결함으로 봤다. `Unknown`은 앞선 실패 기록을 남긴 채 반환했다. `-encoders`가 버전 확인용 60 s 제한을 썼다. | 하네스 결함은 **mpeg4가 목록에 있고 0이 아닌 코드로 끝날 때만**(실패 기록 모두 지우고 `INCONCLUSIVE_TTL`). 목록에 없으면 대조군을 돌리지 않고 앞선 실패는 그대로(영구), mpeg4 시간 초과는 다른 후보와 같은 시간 초과 처리(앞선 진짜 실패 유지, TTL). `Unknown`(실행 불가)도 실패 기록을 지운 뒤 반환(결론 없음 → 목록 기반 선택). `-encoders`는 자체 제한 **`ENCODERS_TIMEOUT_SECS` = 10 s**. | `a_failing_mpeg4_control_is_a_harness_defect_and_is_negative_cached`(바뀜: 목록에 없는 mpeg4 → 대조군 없음, 호출 2회, h264_mf 실패 영구; 목록에 있는 mpeg4 성공 → h264_mf 실패 영구), `a_timed_out_mpeg4_is_not_a_harness_defect`(h264_mf 실패 + mpeg4 35 s 잠 → h264_mf 실패 유지, MF 문구, mpeg4 `trial_timed_out`, TTL) |
| 3 (하) | 옛 venv(`tools/python/.<이름>.old-*`)·`.staging-*` 정리와 pip의 옛 venv 삭제가 `remove_dir_all`이라, 서버가 쓰는 중인 venv(CLI는 서버의 busy lease를 모름)를 **반쯤 지울** 수 있었다. | 새 `tool_install::remove_dir_unless_in_use(dir)`: (Windows) 먼저 그 아래 파일마다 **쓰기용으로(공유 허용, 아무것도 쓰지 않음) 열어 보아** 공유·잠금 위반이면(실행 중인 프로그램·로드된 DLL) 통째로 건너뛴다 — **실행 중인 exe는 디렉터리 이름 바꾸기를 막지 못한다**(이 PC에서 확인: 이름 바꾸기만 쓴 첫 구현은 실행 중인 `python.exe` 옆의 `lens.py`를 지웠고 새 테스트가 잡았다). 그다음 `<dir>.rm`으로 이름을 바꾸고(Windows는 그 아래 파일 하나라도 열린 핸들이 있으면 거부 — FILE_SHARE_DELETE로 연 핸들도; 직접 확인) 실패하면 통째로 건너뛴다. 바꾼 사본만 지우고, 그것도 실패하면 `<dir>.rm`이 남는다(이름이 정리 패턴과 맞아 다음 정리에서 다시 시도). 정리(`sweep_dir`: 목록을 먼저 모은 뒤 처리)와 pip의 옛 venv 삭제(`staging_left` 보고)가 이것을 쓴다. 설치 자신의 `StagingDir::close()`는 그대로(남이 쓰지 않는 자기 폴더). | Windows `the_sweep_skips_a_dir_in_use_whole_instead_of_half_deleting_it`(옛 venv의 `Scripts/python.exe`(복사한 cmd.exe)를 실행해 두고 `.staging-held`의 파일을 std로 열어 둔 채 정리 → 0개, 모든 파일 그대로, `.rm` 없음, `remove_dir_unless_in_use`가 venv 경로를 돌려줌; 놓은 뒤 정리 → 2개, `.rm` 없음), `stale_staging_dirs_are_swept_but_not_live_or_foreign_entries`(확장: 남은 `.staging-*.rm`·`.<이름>.old-*.rm`도 정리) |
| 4a (하·UI) | POST 응답이 5xx이고 본문이 JSON이 아니면(`malformed`) "상태를 알 수 없음"(lost) 분기로 갔다. | 순수 함수 `htPostLost(res)`: HTTP 상태 ≥ 400이면 JSON이든 아니든 **서버의 답 = 거부**; lost는 응답 없음·`network`·status 0·해석 못 한 2xx뿐. | unit "Tools round 4: a 5xx POST reply with a non-JSON body is a refusal"(502+`malformed`, 500, 503+`network` → "설치 요청 실패: …", 추가 GET 없음) |
| 4b (하·UI) | lost 뒤 `htLoad()`가 **다른 도구**의 설치를 따라가게 되면 `HT.busy`가 차 있어 `name`의 설치 성공(`true`)으로 처리됐다. | `HT.busy === name`일 때만 성공. 다른 도구를 따라가게 됐으면 그 설치는 계속 따라가되 `name` 줄에는 "설치 상태를 알 수 없습니다 — 다시 시도하십시오"(알림 assertive), `false` 반환. | unit "Tools round 4: a lost POST whose htLoad() adopts ANOTHER tool's install …" |
| 4c (하·UI·a11y) | 그림자 경로가 `title`에만 있어 키보드·스크린리더로는 알 수 없었다. | `<small class="h-tool-shadow">` 안에 `<span class="sr-only"> — <경로 포함 전체 문장></span>`(title은 마우스용으로 유지). | unit "Tools round 4: the shadow path is in the row text … (sr-only)" |
| 5 (하·서버) | `install-cancel`이 id가 맞는 작업이 **이미 끝났는데**(작업자가 단계를 done/failed로 바꾸고 감사 줄을 쓰는 동안, `state`는 아직 `running`) 취소를 받아들여 UI가 끝난 설치에 "중단하는 중…"을 보였다. | 실행 중 판정 = `state == "running"` **그리고** 단계가 `Done`/`Failed`가 아님. 아니면 `no_install`. | `serve::tests_tools::a_cancel_of_a_finished_job_is_no_install`(단계 Done/Failed × id 있음/없음, 게시된 done/failed → `no_install`, 취소 플래그 안 섬; 실행 중이면 종전대로 수락) |

### API 변경 (트랙 U)
- ffmpeg `encoders`에 **`trial_timed_out`**: `trial_failed` 중 시간 초과(10 s + 재시도 20 s)로만 건너뛴 이름(보통 `[]`). 이때 `note`는 "… · h264_mf 시험 인코딩 시간 초과 — 잠시 뒤 다시 확인"이고 결과는 10분 뒤 다시 시험한다. `trial_failed`의 다른 이름은 0이 아닌 종료 코드(진짜 실패)다.
- `POST /api/tools/install-cancel`: 작업자가 이미 끝낸 작업(단계 done/failed)이면 `{"ok":false,"code":"no_install"}`.
- UI: HTTP 오류 응답(≥ 400)은 본문과 관계없이 거부 문구. 그림자 경로는 sr-only 텍스트로도 읽힌다.

### 확인 방법
- e90257d를 `C:/s/r4old`에 풀고(git archive) 옛 API로 컴파일되는 새 테스트를 옮겨(`r4_slow_first_trial_succeeds_on_the_retry`, `r4_timeout_is_not_permanent`, `r4_unlisted_mpeg4_runs_no_control`, `a_cancel_of_a_finished_job_is_no_install`, `the_sweep_skips_…`(`remove_dir_unless_in_use` 줄 제외)) 별도 target(`C:/s/t4`)에서 실행 → 모두 FAIL. `trial_timed_out`·`expiry_of`·`TIMED_OUT_TTL`을 쓰는 원래 테스트는 옛 코드에서 컴파일되지 않는다(= 실패).
- JS: `assets/app/56-tools.js`만 e90257d 것으로 바꾸고 `verify-review-batch --unit` → 새 행 3개 모두 FAIL, 되돌리면 PASS.

### 확인 못 한 것
- 실제 h264_mf가 느리거나 멈추는 상황(첫 MF 초기화, 백신 검사, N/KN)은 재현하지 못했다 — 가짜 ffmpeg의 잠자는 시험으로만 확인.
- Linux/macOS clippy·테스트는 이 PC(Windows)에서 돌리지 못했다 — 바꾼 `cfg(not(windows))` 코드는 테스트용 가짜 ffmpeg 스크립트의 `[ -e awake ] || sleep` 한 줄뿐이고, 새 `remove_dir_unless_in_use`는 플랫폼 공통 코드(유닉스에서는 이름 바꾸기가 항상 되므로 종전처럼 지운다 — 열린 파일도 지울 수 있는 OS).

## §옵션 C as implemented (2026-10-20)

계획: `docs/PLAN_TOOLS_C_2026-10-20.md`. 문서 행(THIRD_PARTY_NOTICES·README·WINDOWS_USAGE·repro-build·security-review·HANDOFF·이 문서의 T5/T6·§옵션 C·구성 요소 표)은 a8b7f69. 코드·스크립트·CI·테스트는 아래 세 커밋.

### 패키징·CI (P1–P6)
- `scripts/make-portable.ps1`이 유일한 패키지 스크립트: .NET SHA-256(`Get-Sha256Hex`), `ZipFile.CreateFromDirectory`(UseBackslash off) + 쓴 뒤 항목 검사(`\`·안전하지 않은 이름), `-SkipBuild`·`-Version`(경로 같은 값 거부), BOM 유지. 스테이지에 `LICENSE`·`THIRD_PARTY_NOTICES.md` 추가. `README-tools.txt` 3번째 줄은 계획 문구 그대로.
- **FFmpeg 배제**: `tools/bin`/`target/release/tools/bin`을 파일 단위로 복사하며 `ffmpeg*.exe`·`ffprobe*.exe`·`ffplay*.exe`·FFmpeg 공유 DLL(`avcodec-N.dll` 등)·`licenses/ffmpeg-*`는 건너뛰고 `WARNING: ffmpeg/ffprobe are never packaged (…) — skipped: <경로>`. zip을 쓴 뒤 같은 규칙에 걸리는 항목이 하나라도 있으면 zip을 지우고 실패. 우회 스위치 없음. (계획의 ffmpeg/ffprobe에 ffplay·공유 DLL을 더한 것은 공유 빌드를 tools/bin에 둔 경우까지 막기 위해서다.)
- `scripts/package-windows.ps1`·`scripts/test-package-windows.mjs` 삭제(옵션 A에서 e90257d로 복원).
- 새 오프라인 테스트 `scripts/test-make-portable.mjs`: 가짜 저장소 두 벌(도구가 `tools/bin`에 / `target/release/tools/bin`에)에 `ffmpeg.exe`·`ffprobe.exe`·`ffplay.exe`·`avcodec-61.dll`·`licenses/ffmpeg-LICENSE.txt`를 심고, zip에 없음·`mmls.exe`·`libewf.dll`·`THIRD_PARTY_NOTICES.md`·`README-tools.txt` 있음·모든 항목 `/`·`SHA256SUMS` 한 줄(LF, BOM 없음)·"never packaged" 경고·`-Version ../x` 거부를 확인.
- `windows-ci.yml`: 번들 4단계 삭제. Release build 뒤 "Portable package offline test" → "Portable package smoke"(`-SkipBuild`; zip에 ffmpeg/ffprobe 없음, 항목 `/`, `FrameTrace-<ver>-win64/THIRD_PARTY_NOTICES.md`, SHA256SUMS CR 없음) → "Upload portable package"(아티팩트 `FrameTrace-win64`: zip + SHA256SUMS) → **"Install-path smoke"**(20분, pwsh): `dist/FrameTrace-<ver>-win64`를 `RUNNER_TEMP/ft-install-smoke`로 복사, PATH = System32만, `frametrace.exe tools install ffmpeg --yes`; 파일 4개, 설치된 GPLv3 = 매니페스트 핀, `.staging-*` 없음, 감사 마지막 줄 `result: ok`·`sha256` = 핀, `tools list --json`의 `paths.ffmpeg` = 사본, `encoders.probed`, h264 ≠ libopenh264, `trial_failed` = []; `-buildconf`에 `--enable-version3` 있음·`--enable-gpl`/`--enable-nonfree` 없음, version3/gpl/nonfree/libopenh264/chromaprint/libzvbi 줄과 `(codec h264)` 인코더 줄을 `ffmpeg-install-smoke.txt`에 기록 → 아티팩트 `ffmpeg-install-smoke`(텍스트만, `if: always()`). 바이너리는 어떤 아티팩트에도 올라가지 않는다.

### 매니페스트·서버·CLI·UI (L4, M1–M4, M6)
- 매니페스트 ffmpeg: `title` "FFmpeg", `license` 혼합 명시, `notice`/`notice_en`(계획 M1 값 그대로), `manual_hint`(BtbN 릴리스 + ffmpeg.org), `note`의 계획 포인터; `package_licenses`·`bundled_licenses_note`의 package-windows 문장·`tools/licenses/ffmpeg-THIRD-PARTY-LICENSES.txt` 삭제. url·sha256·size·files·license_files·bundled_licenses·허용 호스트는 그대로.
- `ToolEntry.notice`/`notice_en`(serde default). `validate_manifest`: sha256이 있는(설치 가능) 도구는 공백 아닌 `notice` 필수(`installable tools must carry a distribution notice (T5)`), `notice_en`은 있으면 공백 금지. `tool_json` 최상위에 `notice`·`notice_en`(`download` 안에는 없음). `ToolEntry.package_licenses`와 그 검증 삭제.
- CLI: 프롬프트 줄을 `install_prompt_lines(tool, dirs, status) -> Vec<String>`로 뽑고 `라이선스:` 다음에 `고지: <notice>`.
- UI `htConfirmHtml`: `</dl>` 뒤 `<p class="h-tool-notice">`(영어 화면은 `notice_en`, 없으면 `notice`; 둘 다 없으면 문단 없음; `esc`). CSS `.dlg-body .h-tool-notice`는 기존 토큰(`--muted`·`--fs-xs`·`--lh-ctl`)만 — 계획의 `--fs-small`/`--fg-muted`는 없는 이름이라 쓰지 않았다. §28·`tools-mock.mjs` 갱신(round 4의 `trial_timed_out`과 합침).

### 인코더·문구 (E1, E2, D2)
- 순서 그대로. `build_note` "libx264 없음 — H.264는 {} 사용". 모듈·artifacts·doctor·tool_json 주석에서 "bundled LGPL" 제거. 테스트 이름 `btbn_lgpl_build_never_picks_libopenh264`. doctor 코드 변경 없음.

### 테스트
- 추가: `tool_install::tests::installable_tools_must_carry_a_distribution_notice`, `tool_install::tests::tool_json_carries_the_distribution_notice`, `cli::handlers::tools::tests::install_prompt_shows_the_distribution_notice`, `scripts/test-make-portable.mjs`, tools-unit "Tools confirm dialog carries the distribution notice (ko/en)"(+ U2 행의 영어 대화상자), verify-layout tools "the confirm dialog shows the distribution notice … readable"(두 테마, 문단이 목록 뒤·대화상자 안·대비 ≥ 4.5:1), CI 설치 경로 스모크.
- 갱신: `shipped_manifest_is_valid_and_pins_only_published_checksums`(메시지, title·license·notice·notice_en), `serve::tests_tools` 목록 테스트(license 접두·title/label·notice), `ffmpeg_encoders` note 3곳, tools-unit·verify-layout의 "FFmpeg (LGPL build)" 전부, tools-mock.
- 삭제: `package_only_license_files_exist_with_their_pinned_hash`, `scripts/test-package-windows.mjs`, CI 번들 4단계.

### 게이트 (이 PC, Windows 11)
- 기준: tools-bundle 5599b4b(fix round 4 + 문서 행 a8b7f69) 위에 다시 쌓음.
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok(Windows만 — Linux/macOS 타깃이 이 PC에 없어 돌리지 못함; 이번 변경에 `cfg` 분기 코드는 없다) · `cargo test --locked` 1234 passed / 0 failed / 30 ignored · `cargo build --locked` ok.
- `node scripts/check-ps-bom.mjs` ok · `check-js` ok · `check-css` ok · `node scripts/test-make-portable.mjs` all checks passed · `verify-review-batch --unit` 147 PASS(Tools 33).
- `FRAMETRACE_R11_STRICT=1`: verify-layout 171/171(새 고지 행: 대비 light 6.76 · dark 7.32) · check-a11y(`FRAMETRACE_REQUIRE_BROWSER=1`) axe 0 · verify-buttons 173 · verify-i18n 770/770 · verify-start-screen 85/85 · 셸 스모크(`FRAMETRACE_SHELL_SMOKE=1`, APPDATA/LOCALAPPDATA 격리) exit 0.

### 첫 CI 설치 경로 스모크 결과 (dd9ef61, windows-msvc run 39228894226, 2026-10-19)
- 실제 BtbN 자산 다운로드·검증·설치 성공: `audit: result=ok sha256=4a7642b2…ffc0a size=170611883 host=github.com`(리다이렉트 모두 허용 목록 안 — 허용 목록 확장 불필요).
- h264 pick: **h264_mf**(probed true, trial_failed `[]`, note "libx264 없음 — H.264는 h264_mf 사용") — Windows Server 러너에서도 Media Foundation 시험 인코딩이 통과했으므로 게이트(≠ libopenh264) 유지.
- buildconf: `--enable-version3`, `--enable-chromaprint`, `--enable-libopenh264`, `--enable-libzvbi` 출력, `--enable-gpl`·`--enable-nonfree` 없음. `(codec h264)` 줄: libopenh264, h264_amf, h264_d3d12va, h264_mf, h264_nvenc, h264_qsv, h264_vaapi, h264_vulkan.
- 같은 푸시의 ubuntu 잡은 `ffmpeg_encoders::tests::a_listed_encoder_that_fails_its_trial_is_skipped`가 한 번 실패(Linux에서 병렬 테스트의 fork가 방금 쓴 가짜 ffmpeg 스크립트를 쥐고 있어 exec가 ETXTBSY → 프로브가 `assumed_full`로 떨어져 libx264) — e50f8ea에서 헬퍼가 `--warm` 실행으로 exec 가능해질 때까지 재시도하도록 고침.

### 남은 일
- L1·M1 고지 문구의 "(Owner to confirm)" 확인.

## §Fix round 5 (2026-10-20, 기준 8e21db1)

8e21db1 적대적 리뷰의 항목을 모두 고쳤다(소유자 요청으로 Low 포함). M2(“Owner to confirm”)는 소유자 확인 전까지 문구를 그대로 두고 릴리스 가드만 더했다.

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| M1 | `notice`/`notice_en`이 “내 PC에서 쓰는 데는 제한이 없다”고 하면서 같은 문장에서 특허 사용권은 없다고 함(소스 빌드 libopenh264는 Cisco 특허 라이선스 밖). | 주장을 라이선스로 한정: ko “이 라이선스들은 내 PC에서의 사용을 제한하지 않습니다(특허는 별개).”, en “These licenses do not restrict use on your own PC (patents are a separate matter)”. `tools/manifest.json`·`scripts/lib/tools-mock.mjs`, `THIRD_PARTY_NOTICES.md`의 같은 문장(“patents are a separate matter: no patent licence comes with it”, Owner-to-confirm 표시는 그대로). | tools-unit·verify-layout은 mock의 `FF.notice`를 그대로 비교하므로 문구 고정 없음 — `verify-review-batch --unit` 147 PASS, verify-layout 고지 행 통과(대비 light 6.76 · dark 7.32), verify-i18n 770/770. |
| M3 | `-encoders`가 10 s 안에 안 끝나면(백신이 새 170 MB exe 검사) `assumed_full`(libx264) → BtbN 빌드에서 프록시·내보내기가 “Unknown encoder libx264”로 60 s 동안 실패, `probed:false`라 note 없음. | `ffmpeg_encoders::probe`: 시간 초과면 **2배(20 s)로 한 번 더**. 그래도 목록이 없고 바이너리가 `<exe>/tools/bin` 바로 아래(안내 설치 = 매니페스트 고정 빌드)면 새 **`Encoders::assumed_pinned`**(h264_mf·libopenh264·mpeg4·aac, libx264·libmp3lame 없음)를 가정하고 **시험 인코딩은 그대로** 한다. note: “ffmpeg -encoders 확인 시간 초과 — 설치한 고정 빌드(libx264 없음) 기준으로 H.264는 h264_mf 사용 · 잠시 뒤 다시 확인”(시간 초과가 아니면 “확인 실패”). 다른 위치의 ffmpeg는 종전대로 `assumed_full`이되 시간 초과면 note “ffmpeg -encoders 확인 시간 초과 — libx264로 가정 · 잠시 뒤 다시 확인”. 가정한 결과는 영구 캐시하지 않는다(시간 초과 있음 → `TIMED_OUT_TTL`, 아니면 `INCONCLUSIVE_TTL`). | 가짜 ffmpeg(`fake_ffmpeg_full`: `-encoders`도 잠들 수 있게; Windows `.cmd`·유닉스 `sh` 두 분기): `a_timed_out_listing_is_retried_once`(13 s → 재시도 성공, `-encoders` 2회, 영구), `a_timed_out_listing_of_the_pinned_build_assumes_its_candidates`(35 s, 테스트용 고정 디렉터리 등록 → h264_mf, `probed` false, `assumed_pinned_build`/`listing_timed_out` true, libx264 없음, note, 호출 3회, TTL ≈ 10분, 45 s+8 s 안), `a_timed_out_listing_elsewhere_assumes_a_full_build_with_a_note`(libx264 가정 + note, 시험 없음, TTL 10분; 없는 바이너리는 note 없음). |
| L1 | 최악의 동기 프로브 ≈ 100 s(후보마다 10 + 20 s), 10분마다 반복 — GET /api/tools·doctor·프록시가 모두 기다림. 앞 후보 시간 초과 뒤 mpeg4가 0이 아닌 코드/`Unknown`이면 TTL이 60 s로 줄어듦. | **`PROBE_BUDGET` = 45 s**를 `-encoders`(재시도 포함)와 모든 시험에 공통으로: 각 시험 제한은 남은 예산으로 자르고, 다 쓰면 시험을 더 돌리지 않는다 — 남은 mpeg4는 **시험 없이** 사용, 그 밖의 남은 후보는 `trial_timed_out`(새 `Trial::OverBudget`). 무엇이든 시간 초과가 있었으면 결과는 **`TIMED_OUT_TTL`(10분)** — 하네스 결함·`Unknown`으로 결론이 없어도(`inconclusive_ttl(timed_out)`). | `the_probe_budget_stops_trialling`(libx264·h264_mf 60 s 잠: libx264 2회, h264_mf ≥ 1회, libopenh264·mpeg4 시험 0회, 셋 다 `trial_timed_out`, mpeg4 선택, 예산+8 s 안, TTL 10분), `a_harness_defect_after_a_timeout_keeps_the_longer_ttl`(h264_mf 35 s 잠 + mpeg4 실패 → 실패 기록 지움, 목록 기반 h264_mf, TTL 10분). |
| L2 | pip 재설치가 살아 있는 venv를 사용 중 확인 없이 이름 바꿈(실행 중인 exe는 Windows에서 디렉터리 이름 바꾸기를 막지 못함). | `pip::install_pip`: 아무것도 만들기 전에 `image_in_use_under(&target)` → 사용 중이면 `Err("busy: <venv> is in use (a program runs from it) — close it and try again; nothing was changed")`. 새 `tool_install::BUSY_PREFIX`; 실패 감사 줄에 `"code":"busy"`. | Windows `a_pip_reinstall_refuses_while_a_program_runs_from_the_live_venv`(venv의 `Scripts/held.exe`(복사한 cmd.exe) 실행 중 → busy 오류, 가짜 python 호출 0, venv 파일 그대로, `.lens.old-*`·`.staging-*` 없음, 감사 줄 `failed`+`busy`; 종료 뒤 재설치 성공). |
| L3 | `Test-FfmpegFile`이 이름 패턴만: `ffmpeg.exe.bak`, 끝 공백 `ffmpeg.EXE `, `avcodec.dll`, `libavcodec-61.dll`, 이름 바꾼 사본을 놓침. | 잎 이름을 공백·끝 점 제거·소문자로 보고 `ffmpeg|ffprobe|ffplay` 또는 `av(codec|format|util|device|filter)|swscale|swresample|postproc`를 **포함**하면 건너뜀. 스테이지의 모든 .exe/.dll은 `FileVersionInfo`(ProductName·FileDescription·CompanyName·InternalName·OriginalFilename)에 FFmpeg가 있으면 실패. CI 스모크의 zip 검사도 같은 이름 규칙. | `test-make-portable.mjs`: `ffmpeg.exe.bak`·`FFProbe-Old.EXE`·`avcodec.dll`·`libavcodec-61.dll`·`swscale-8.dll`·`my-ffmpeg-copy.exe`·`\\?\`로 만든 진짜 끝 공백 `ffmpeg.EXE ` → zip에 없음·경고 있음·어떤 항목 이름에도 FFmpeg 없음; `Add-Type`으로 만든 .NET DLL(AssemblyProduct “FFmpeg”)을 `renamed-tool.dll`로 → 실패·zip 없음. |
| L4 | SHA256SUMS가 dist/의 모든 zip을 나열 → 옛 패키저의 `*-bundled.zip`이 함께 실림. | 이번 실행이 쓴 zip 한 줄만. 시작할 때 dist/의 다른 zip 중 FFmpeg 항목(같은 규칙)이 있거나 읽을 수 없으면 실패. `docs/repro-build.md` 갱신. | 깨끗한 `old-notes.zip`이 있어도 SHA256SUMS는 우리 zip 한 줄; `…-bundled.zip`(안에 ffmpeg.exe) → “contains FFmpeg”로 실패, SHA256SUMS 없음. |
| L5 | 지우기가 계속 실패하는 `.rm` 디렉터리가 정리 때마다 `.rm.rm.rm…`로 바뀜(MAX_PATH까지). | `remove_dir_unless_in_use`: 이름이 이미 `.rm`으로 끝나면 이름을 바꾸지 않고 `remove_dir_all`만 다시(사용 중 확인은 그대로). | `a_leftover_rm_dir_is_retried_not_renamed_again`(플랫폼 공통; 테스트 전용 스레드 로컬 `FAIL_DOOMED_REMOVAL`로 삭제 실패를 흉내: `.staging-stuck` → `.staging-stuck.rm`, 정리 3번 뒤에도 그 이름 하나뿐, `.lens.old-00ff.rm`도 `.rm.rm` 없음; 풀면 정리 2개). |
| L6 | `src/video_export.rs` “The bundled LGPL ffmpeg”, `docs/WINDOWS_USAGE.md` “The bundled tools”가 옵션 C와 모순. | 둘 다 고침. 저장소 grep으로 `docs/TECH_STACK.md`(“Bundle or validate”), `docs/WINUI_BLOCKER.md`, `docs/WINDOWS_IMPLEMENTATION_HANDOFF.md`(날짜 붙인 주석)도. `docs/security-review.md`의 round 1 행(“Residual … the bundle is gone”)과 계획 문서의 이력 절은 그대로. 매니페스트 필드명 `bundled_licenses`(코드 식별자)는 그대로. | grep `bundl|번들` × `ffmpeg|lgpl` — 남은 것은 이력 행·식별자뿐. |
| M2 | `THIRD_PARTY_NOTICES.md:36` “(Owner to confirm this paragraph.)”. | **문구는 그대로**(소유자 항목). `make-portable.ps1`이 이 표시가 있으면 실패하고, `-AllowUnconfirmedNotices`일 때만 경고 후 진행. `windows-ci.yml` 패키지 단계에 스위치 + 소유자 항목을 가리키는 주석. | `test-make-portable.mjs`: 표시 있음 → 실패(메시지에 `Owner to confirm`·`AllowUnconfirmedNotices`), zip 없음; 스위치 → 0, “test package only” 경고, zip 있음. |

### API 변경 (트랙 U)
- ffmpeg `encoders`에 **`assumed_pinned_build`**(bool: `-encoders`가 목록을 주지 않아 고정 빌드 후보를 가정; 이때 `probed` false)와 **`listing_timed_out`**(bool). `note`는 위 M3 문구. UI는 `note`를 그대로 보여 주므로 변경 없음.
- 설치 실패 오류가 `busy: `로 시작할 수 있다(pip venv 사용 중). 감사 줄 `result: failed`, `code: busy`.
- `make-portable.ps1 -AllowUnconfirmedNotices`(CI 테스트 패키지 전용).

### 확인 방법
- 새 Rust 테스트는 새 API(`fake_ffmpeg_full`, `PINNED_TEST_DIRS`, `FAIL_DOOMED_REMOVAL`, `BUSY_PREFIX`)를 써서 8e21db1에서는 컴파일되지 않는다(= 실패). 동작 근거: 8e21db1의 `probe`는 시간 초과 뒤 mpeg4 실패 시 `INCONCLUSIVE_TTL`, 예산 없이 모든 후보를 시험, `-encoders` 재시도 없음 — 각 테스트의 단언과 정면으로 어긋난다.
- `test-make-portable.mjs`(새 판)를 8e21db1의 `make-portable.ps1`로 돌림 → 9 실패(끝 공백 파일로 패키지 실패, stale zip·버전 정보·Owner 가드 미검출), 새 판으로 되돌리면 all checks passed.

### 게이트 (이 PC, Windows 11, 기준 8e21db1 + 이 라운드 커밋)
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok(Windows만) · `cargo test --locked` 1241 passed / 0 failed / 30 ignored.
- `node scripts/check-ps-bom.mjs` ok · `check-css` ok · `check-js` ok · `verify-review-batch --unit` 147 PASS · `node scripts/test-make-portable.mjs` all checks passed.
- `FRAMETRACE_R11_STRICT=1`: verify-i18n 770/770 · verify-layout 171/171(고지 행 light 6.76 · dark 7.32).

### 확인 못 한 것
- Linux/macOS clippy·테스트는 돌리지 못했다. 바뀐 `cfg(not(windows))` 코드는 가짜 ffmpeg 스크립트의 `-encoders` 분기 앞 `[ -e awake ] || sleep N; ` 한 조각뿐이고(같은 `sleep` 방식은 기존 시험 테스트가 CI에서 씀), `is_pinned_install`·예산·L5는 플랫폼 공통, L2 테스트는 Windows 전용(유닉스의 `image_in_use_under`는 늘 false).
- 실제 백신이 `-encoders`를 늦추는 상황, 실제 BtbN `ffmpeg.exe`의 버전 정보 문자열은 재현·확인하지 못했다(L3 검사는 .NET 픽스처로만). BtbN DLL/EXE에 버전 정보가 없더라도 이름 규칙이 먼저 걸러 낸다.

### 메모 — BtbN 보존 기간 (2026-10-20, 리뷰어 추정)
- 리뷰어 추정: BtbN FFmpeg-Builds는 autobuild 릴리스를 정리하면서 **매달 마지막 빌드만 약 2년 보존**한다. 고정한 `autobuild-2026-09-30-13-08`은 9월의 마지막 빌드라 남겠지만 2028-09 무렵 지워질 수 있다 → **~2028-09 전에 매니페스트를 새 빌드로 다시 고정**(url·sha256·size·checksum_source·`docs/ffmpeg-btbn-components-*.md`·CI 설치 경로 스모크). 확인되지 않은 추정이다 — 재고정 시 BtbN README의 보존 정책을 다시 읽을 것.

## §Fix round 6 (2026-10-20, 기준 224d52a)

224d52a 확인 리뷰의 항목(M·L3b·L-CI, 그리고 그 안의 Low)을 모두 고쳤다. 게이트에서 224d52a 자체의 테스트 실패 하나를 더 찾아 고쳤다(아래 T).

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| M | `<exe>/tools/bin` 밖의 ffmpeg(PATH 설치)는 `-encoders`가 두 번 시간 초과되면 **시험 없이** libx264를 가정했고, 이를 `inconclusive_ttl(true)` = 10분 동안 캐시(8e21db1에서는 60 s). libx264 없는 빌드면 프록시(`artifacts.rs` ~95)·MP4 내보내기(`video_export.rs` ~146)가 대체 경로 없이 “Unknown encoder libx264”로 10분 동안 실패. **Low**: 위치 규칙 때문에 사용자가 tools/bin에 복사한 gyan(libx264 있음) 빌드를 고정 빌드로 보고 “설치한 고정 빌드(libx264 없음)”라는 틀린 note. | `ffmpeg_encoders::probe`: 목록이 없으면(시간 초과·실패) **위치와 무관하게** 새 `Encoders::assumed_candidates`(libx264·h264_mf·libopenh264·mpeg4 + aac; libmp3lame 가정 안 함 → AVI는 mp2)를 가정하고 **남은 예산 안에서 시험 인코딩**(없는 libx264는 1 s 안에 실패 → 다음 후보). `is_pinned_install`·`PINNED_TEST_DIRS`·`assumed_pinned`·JSON `assumed_pinned_build` 삭제, JSON에 `assumed_candidates`(bool) — `listing_timed_out`은 그대로. 시험으로 고르면 `chosen_by_trial`. note: “ffmpeg -encoders 확인 시간 초과 — 시험 인코딩으로 h264_mf 선택 · 잠시 뒤 다시 확인”(목록 실패면 “확인 실패”; 시험이 결론을 못 내면 “시험 인코딩 결론 없음, libx264 가정”; 시험 시간 초과가 있으면 “· 일부 시험 인코딩 시간 초과”). 가정 후보에서는 Media Feature Pack 힌트를 붙이지 않는다(가정한 h264_mf 실패는 빌드에 없는 것일 수 있음). 없는 바이너리(목록 실패 + 시험 spawn 불가)는 종전처럼 note 없음·`INCONCLUSIVE_TTL`. TTL 규칙은 그대로(시간 초과 있음 → `TIMED_OUT_TTL`, 목록 없음 → 최대 `INCONCLUSIVE_TTL`). `docs/ui-rebuild-contract.md` §28에 Fix round 6 줄. `serve/tools.rs`·tools-mock·tools-unit은 이 필드를 참조하지 않아 변경 없음. | `a_timed_out_listing_trials_the_candidates_and_skips_a_missing_libx264`(PATH 위치, `-encoders` 35 s 잠 + libx264 시험 exit 1: `-encoders` 2회, libx264 시험 1회 실패, h264_mf 1회 → h264_mf, `probed` false, `assumed_candidates`/`listing_timed_out` true, `trial_failed` = [libx264], note 전문 일치, “고정 빌드”·MF 힌트 없음, `assumed_pinned_build` 없음, 예산+8 s 안, TTL 10분). `a_timed_out_listing_chooses_a_working_libx264_by_trial`(FULL 목록, 같은 시간 초과 → 시험 1회로 libx264, note “…시험 인코딩으로 libx264 선택…”; 없는 바이너리 → note 없음, TTL ≤ 60 s). `a_failing_mpeg4_control_…`의 빈 목록 부분: 이제 libx264 시험 1회(호출 2), note “확인 실패 — 시험 인코딩으로 libx264 선택”, TTL ≤ 60 s. 옛 `…pinned_build_assumes_its_candidates`·`…elsewhere_assumes_a_full_build_with_a_note`는 위 둘로 대체. `cargo test --lib ffmpeg_encoders` 19/19. |
| L3b | `make-portable.ps1`의 PE 버전 정보 검사가 무력: 실제 정적 ffmpeg.exe(이 PC의 gyan 7.1)는 ProductName·FileDescription·CompanyName·InternalName·OriginalFilename이 **비어 있어** 이름 바꾼 `my-tool.exe`가 통과; .exe/.dll 밖의 확장자(.bin 등)는 검사도 안 함. | 이름 규칙·버전 정보 검사는 그대로 두고, 스테이지의 **모든 파일(확장자 무관, 문서 포함 — 화이트리스트 없음)**을 바이트 검사(`Get-FfmpegSignature`): 4 MiB 청크 + 앞 청크 끝 64바이트 겹침, Latin-1 디코드(1바이트=1문자), PowerShell 5.1 호환. 서명(대소문자 무시): `the FFmpeg developers`, `ffmpeg version (n\|N-)?<숫자>`, `libavcodec`, `Lavf<2자리>.<숫자>`(libavformat ident, 예 Lavf61.7.100). 리뷰가 제안한 그대로의 `ffmpeg version`·`Lavf`는 우리 파일과 겹친다 — `docs/WINDOWS_USAGE.md:215` “FFmpeg version text”, frametrace.exe의 `-f lavfi`(대소문자 무시 `lavf`) — 그래서 뒤에 숫자를 요구했다. 매니페스트는 **zip(아카이브) SHA-256만** 고정하고 ffmpeg.exe/ffprobe.exe 파일별 해시가 없어 **파일별 해시 비교는 하지 않았다**. | gyan 7.1 `ffmpeg.exe`/`ffprobe.exe`에 네 서명 모두 있음(Python 확인: `the FFmpeg developers` 2–3, `FFmpeg version 7.1-full_build` 8, `libavcodec` 434, `Lavf61.7.100` 11). 우리 파일에는 없음: README.md·LICENSE·THIRD_PARTY_NOTICES.md·docs/WINDOWS_USAGE.md·docs/WINDOWS_VALIDATION.md·scripts/install.ps1, 이 라운드의 debug `frametrace.exe`(19.1 MB)·`frametrace-app.exe`(19.7 MB). 실제 debug exe 둘 + 실제 문서로 만든 가짜 저장소 → 패키지 성공(압축 포함 12 s); 거기에 gyan `ffprobe.exe`를 `tools/bin/my-tool.exe`, `ffmpeg.exe`를 `codec.bin`으로 → “contains the FFmpeg signature 'the FFmpeg developers'”로 실패. `test-make-portable.mjs`: `my-tool.exe`(the FFmpeg developers)·`payload.bin`(4 MiB 경계에 걸친 `Lavf61.7.100`)·`blob`(확장자 없음, 대문자 `FFMPEG VERSION 7.1`)·`helper.dat`(libavcodec) → 각각 실패·zip 없음·1–1.4 s; 진짜 README.md·WINDOWS_USAGE.md·THIRD_PARTY_NOTICES.md 사본과 `tools/bin/NOTES.txt`(“ffmpeg and ffprobe are not included … FFmpeg version text … -f lavfi”) → 통과·zip에 있음. 겹침을 0으로 바꾸면 `payload.bin` 두 검사만 실패(경계 처리 확인). |
| L-CI | `-AllowUnconfirmedNotices`로 만든 CI 아티팩트 `FrameTrace-win64`(zip + SHA256SUMS)가 “(Owner to confirm this paragraph.)”를 표시 없이 실어 감. 표시 검사 `'Owner to confirm'`은 줄바꿈으로 다시 흐른 문장을 놓침(추정). | `make-portable.ps1`: 표시(`(?i)Owner\s+to\s+confirm`)가 있고 스위치가 있으면 zip 이름 `FrameTrace-<ver>-win64-UNCONFIRMED.zip`, `SHA256SUMS` 대신 `SHA256SUMS.unconfirmed`(남아 있던 `SHA256SUMS`는 지움), 끝에 큰 경고 “UNCONFIRMED TEST PACKAGE — NOT FOR RELEASE”. 같은 버전의 다른 종류 zip도 지움. `windows-ci.yml`: 스모크 단계가 `FrameTrace-<ver>-win64*.zip` 하나를 glob으로 찾고, 고지에 표시가 있으면 이름이 `-UNCONFIRMED.zip`·`SHA256SUMS` 없음을 확인; 업로드는 `hashFiles(...UNCONFIRMED.zip)`로 갈라 **`FrameTrace-win64-unconfirmed`**(UNCONFIRMED zip + SHA256SUMS.unconfirmed) 또는 확인된 경우 종전 `FrameTrace-win64`. 설치 경로 스모크는 zip이 아니라 스테이지 디렉터리 `dist/FrameTrace-<ver>-win64/`를 복사하므로 이름 변경 영향 없음(그대로). `docs/repro-build.md` 갱신. | `test-make-portable.mjs`: 스위치 → 0, “test package only”·“UNCONFIRMED TEST PACKAGE”·“NOT FOR RELEASE”, `…-UNCONFIRMED.zip` 있음, 일반 zip 없음, 미리 둔 낡은 `SHA256SUMS` 지워짐, `SHA256SUMS.unconfirmed` = UNCONFIRMED zip 해시 한 줄; `(Owner to\r\n   confirm …)` → 실패·zip 없음; 확인된 패키지에는 `*.unconfirmed`·`-UNCONFIRMED.zip` 없음. 새 테스트를 224d52a의 `make-portable.ps1`로 돌리면 16 실패, 새 판으로 all checks passed. |
| T | (게이트에서 발견) 224d52a의 병합(f1c4bf5, 유닉스 가짜 스크립트 warm 헬퍼)이 더한 `src/test_support.rs`는 lib.rs에서 통째로 `#[cfg(test)]`라 안쪽 표시가 없어, `util::tests::every_command_spawn_site_hides_the_console`이 그 `Command::new`를 운영 코드로 보고 **모든 플랫폼에서 실패**(lib 테스트가 실패하면 cargo가 거기서 멈춰 통합 테스트 바이너리는 돌지 않음; 224d52a 자체로는 돌리지 않았지만 파일이 그대로라 같은 결과). | 검사가 `test_support.rs`를 테스트 전용 파일로 건너뜀(`tests.rs`·`tests_*`와 같은 취급). | 그 테스트 통과, 전체 `cargo test` 아래. |

### API 변경 (트랙 U)
- ffmpeg `encoders`: `assumed_pinned_build` **삭제**, **`assumed_candidates`**(bool) 추가, `listing_timed_out` 유지. `note`는 위 M 문구. UI는 `note`를 그대로 보여 주므로 변경 없음.
- `make-portable.ps1 -AllowUnconfirmedNotices`의 산출물 이름: `…-win64-UNCONFIRMED.zip` + `SHA256SUMS.unconfirmed`; CI 아티팩트 `FrameTrace-win64-unconfirmed`.

### 게이트 (이 PC, Windows 11, 기준 224d52a + 이 라운드 커밋)
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok(Windows만) · `cargo test --locked` **1241 passed / 0 failed / 30 ignored**(lib 1218 + 통합 14·5·4).
- T를 고치기 전 첫 전체 실행(M·L3b·L-CI 커밋 위): lib `1217 passed; 1 failed; 3 ignored` — 실패는 `every_command_spawn_site_hides_the_console`(`test_support.rs`, 이 라운드에서 바꾸지 않은 파일 → 224d52a에서도 같음).
- `node scripts/check-ps-bom.mjs` ok · `check-css` ok · `check-js` ok · `verify-review-batch --unit` 147 PASS · `node scripts/test-make-portable.mjs` all checks passed(106 ok) · `verify-contract-pin` ok.
- `FRAMETRACE_R11_STRICT=1` verify-i18n 770/770(바뀐 문구는 서버 note뿐 — UI 키 변경 없음). verify-layout은 UI를 바꾸지 않아 돌리지 않았다.

### 확인 못 한 것
- Linux/macOS clippy·테스트는 돌리지 못했다. 바뀐 Rust는 플랫폼 공통(가정 후보·note·테스트)이고 `cfg(not(windows))` 코드는 바꾸지 않았다. 비 Windows에서 가정 후보의 h264_mf 시험은 바로 실패(“Unknown encoder”)해 다음 후보로 넘어간다 — 가짜 스크립트 테스트는 플랫폼 공통으로 이 경로를 지난다.
- 실제 PATH ffmpeg의 `-encoders`가 백신 때문에 늦는 상황은 재현하지 않았다(가짜 스크립트로만).
- BtbN `ffmpeg.exe`의 바이트에 네 서명이 있는지는 이 PC에 BtbN 바이너리가 없어 확인하지 못했다(gyan 7.1로만). 같은 FFmpeg 소스의 배너(`the FFmpeg developers`)·`LIBAVFORMAT_IDENT`(`Lavf…`)·라이브러리 이름(`libavcodec`)이라 빌드와 무관하게 들어 있을 것으로 본다.
- release 프로필 `frametrace.exe`로는 서명 검사를 돌리지 않았다(debug exe로만; 같은 소스 문자열).
- CI 워크플로의 새 업로드 분기(`hashFiles` 조건)는 GitHub에서 실행해 보지 못했다. `scripts/build-release.sh`(Linux/macOS)의 SHA256SUMS는 여전히 dist/의 모든 zip을 나열한다 — 같은 dist/에서 Windows UNCONFIRMED zip과 섞일 일은 없지만 고치지 않았다.
- audit 줄의 `encoder_source`는 목록이 없을 때 시험으로 골라도 종전대로 “assumed (encoder probe failed)”(감사 줄 형식은 바꾸지 않는다는 계획 규칙).

## §Fix round 7 (2026-10-20, 기준 7db65c7)

7db65c7 확인 리뷰의 Low 넷(L1–L4)과 서명 목록 추정 하나를 처리했다.

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| L1 | 목록이 **실패**(시간 초과 아님)한 뒤 시험이 시간 초과되거나 예산이 끝나면 mpeg4를 시험 없이 10분 동안 쓰는데 note가 None(`assumed_note`가 `!listing_timed_out && !chosen_by_trial`만 봄). | `assumed_note`의 None 조건에 `!trial_ran && timed_out.is_empty()`를 더함(새 필드 `trial_ran`: 시험이 종료 코드나 시간 초과까지 간 적 있음). 이제 “ffmpeg -encoders 확인 실패 — 시험 인코딩 결론 없음, mpeg4(MPEG-4 Part 2) 가정 · 일부 시험 인코딩 시간 초과 · 잠시 뒤 다시 확인”. 시험이 하나도 못 돈 없는 ffmpeg는 종전처럼 note 없음. | `a_failed_listing_with_timed_out_trials_has_a_note`(빈 목록 + libx264·h264_mf 60 s 잠: mpeg4, `trial_timed_out` libx264, note 전문 일치, mpeg4 시험 0회, 예산+8 s 안, TTL 10분), `the_assumed_note_names_timed_out_trials_after_a_failed_listing`(실행 없이: 아무것도 안 돌면 None, 모두 시간 초과면 접미사). |
| L2 | 가정 후보 모드(목록 없음)에서 mpeg4 대조 시험이 실패(lavfi 없는 최소 빌드)하거나 시험 spawn이 `Unknown`이면 `clear_failures` → 시험 안 한 libx264를 10분(또는 60 s) 가정 — round 5 Medium의 좁은 잔재. | 새 `give_up`: 목록이 있으면 종전대로 `clear_failures`(목록 기반 선택), **가정 후보면 `assume_mpeg4_without_evidence`** — 실패 기록은 지우되(아무도 탓하지 않음) 시간 초과 항목은 남기고 새 필드 `no_evidence` → `video()`는 mpeg4. note “ffmpeg -encoders 확인 실패 — 시험 인코딩 증거 없음, mpeg4(MPEG-4 Part 2) 가정 · 잠시 뒤 다시 확인”(시험이 하나라도 돌았을 때; 시간 초과가 있었으면 “· 일부 시험 인코딩 시간 초과”). TTL 규칙은 그대로(`inconclusive_ttl`). **경우 정리**: 목록 없음 + 증거 없음 → mpeg4(모든 ffmpeg에 내장, 실패해도 ffmpeg가 크게 알림); 없는 ffmpeg(목록·시험 모두 spawn 불가)도 이제 h264 “mpeg4”·note 없음(종전 libx264 — 어차피 실행되지 않음). JSON 키 변경 없음. | `a_failed_listing_and_a_failing_mpeg4_control_assume_mpeg4`(빈 목록 + 네 후보 모두 exit 1: 호출 5회, mpeg4, `trial_failed` [], note 전문, TTL ≤ 60 s). `a_failed_listing_and_an_unknown_trial_assume_mpeg4`(가짜가 libx264 시험에서 자기 자신을 지움 → 다음 시험 spawn 실패 = `Unknown`: 호출 2회, mpeg4, `trial_failed` [], note 전문; Windows `.cmd`·유닉스 `sh` 두 분기, 유닉스는 `warm_unix_script`; **Fix round 8 정정**: Windows 분기는 지운 `.cmd`가 `cmd.exe /c`로 여전히 spawn되어 exit 1 → `Fails`(mpeg4 대조 경로)였다 — `Unknown`은 유닉스 분기뿐이었다. §Fix round 8 L2). 기존 `a_timed_out_listing_chooses_a_working_libx264_by_trial`의 없는 바이너리 단언을 libx264 → mpeg4로 바꿈. 8e21db1·7db65c7 코드로는 셋 다 `video()` = libx264(실패 기록을 지우면 가정 목록의 첫 후보)·note None이라 실패한다(논리 확인; 옛 코드로 다시 돌리지는 않음). |
| L3 | 감사 줄 `encoder_source:"assumed (encoder probe failed)"`는 시험이 인코더를 골라도 그대로. | **문자열은 그대로**(감사 줄 형식 고정). 뜻: “probe failed” = `-encoders` **목록**이 실패/시간 초과했다는 것; 시험 인코딩 결과는 doctor/`tools list`/`/api/tools`의 `note`에 있다. 감사 줄(`artifacts.rs` make-*, `video_export.rs` export-video)에는 기존 `note`나 자유 텍스트 필드가 없어 — 키를 더하지 않고는 “trial chose h264_mf”를 넣을 곳이 없으므로 — **감사 줄은 바꾸지 않았다**. `docs/ui-rebuild-contract.md` §28 Fix round 7 줄에도 같은 설명. | 문서만. |
| L4 | `scripts/build-release.sh`의 `shasum ./*.zip`가 dist/의 모든 zip을 나열 → `--sign`이 `*-UNCONFIRMED.zip`까지 cosign할 수 있음. | `refuse_unconfirmed`: dist/에 `*-UNCONFIRMED.zip`(대소문자 무시, `find -iname`)이 있으면 **빌드 전과 SHA256SUMS 쓰기 직전 두 번** 큰 오류로 exit 1(“UNCONFIRMED test package — NOT FOR RELEASE”, 파일 목록). 그와 별개로 SHA256SUMS glob에서 `*-unconfirmed.zip`(소문자 비교)을 뺀다. 도움말 범위 `2,16p`. `docs/repro-build.md` 갱신. | 이 스크립트용 테스트 하네스는 없어(scripts/·tests/에 build-release 테스트 없음) Git Bash로 픽스처 실행: 임시 디렉터리에 `scripts/build-release.sh` 사본, `Cargo.toml`(version 9.9.9), `README.md`, `target/release/frametrace`, PATH 앞에 가짜 `cargo`(호출 기록; `PLANT=1`이면 빌드 중 `dist/FrameTrace-9.9.9-win64-UNCONFIRMED.zip`을 만듦)·`rustc`(`host: x86_64-unknown-linux-gnu`)·`zip`(인자 `$2`에 파일 하나), 그리고 `bash <fixture>/repo/scripts/build-release.sh`. (a) 시작 때 UNCONFIRMED zip + `other-1.0-aarch64.zip` → exit 1, cargo 호출 0, SHA256SUMS 없음; (b) 소문자 `…-unconfirmed.zip` → exit 1; (c) `PLANT=1` → 빌드 뒤 두 번째 검사로 exit 1, SHA256SUMS 없음; (d) 깨끗한 dist/ → exit 0, SHA256SUMS = 우리 zip + other zip; (e) 두 번째 검사 줄을 뺀 사본 + `PLANT=1` → exit 0, SHA256SUMS에 UNCONFIRMED zip 없음(glob 제외 단독 확인). |
| H | (추정) `make-portable.ps1` 서명 목록에 `libavutil`·`libswscale`. | 둘 다 우리 파일에 없어 **추가함**: debug `frametrace.exe`(19.2 MB)·`frametrace-app.exe`(19.7 MB, target-fix7, 이 라운드 코드)에서 `grep -aoi` 0건, 패키저가 스테이지하는 README.md·LICENSE·THIRD_PARTY_NOTICES.md·docs/WINDOWS_USAGE.md·docs/WINDOWS_VALIDATION.md·scripts/install.ps1·`README-tools.txt` here-string, `tools/licenses`·`src`에도 0건. 실제 gyan 7.1 `ffmpeg.exe`에는 libavutil 33·libswscale 20건. | `test-make-portable.mjs`: `util.so`(대문자 `LIBAVUTIL_59`)·`scale.part`(`libswscale`) → 각각 실패·zip 없음·1 s 안; 진짜 문서 사본 통과 검사는 그대로 통과. |

### API 변경 (트랙 U)
- 없음(JSON 키 그대로). ffmpeg `encoders`: 목록 없음 + 시험 증거 없음이면 `h264` "mpeg4"(종전 "libx264"), `note`는 위 L1·L2 문구.

### 게이트 (이 PC, Windows 11, 기준 7db65c7 + 이 라운드 커밋, CARGO_TARGET_DIR=target-fix7)
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok(Windows만) · `cargo test --locked` **1245 passed / 0 failed / 30 ignored**(lib 1222 + 통합 14·5·4); `cargo test --lib ffmpeg_encoders` 23/23.
- `node scripts/check-ps-bom.mjs` ok · `check-js` ok · `check-css` ok · `verify-review-batch --unit` 147 PASS · `node scripts/test-make-portable.mjs` all checks passed(112 ok).

### 확인 못 한 것
- Linux/macOS clippy·테스트는 돌리지 못했다. 바뀐 Rust는 플랫폼 공통이고, 새 `cfg(not(windows))` 코드는 L2 Unknown 테스트의 자기 삭제 `sh` 가짜 하나(`--warm` + `warm_unix_script`)뿐이다.
- `build-release.sh`는 Git Bash(bash 5.2)의 가짜 cargo/rustc/zip으로만 돌렸다 — 실제 Linux/macOS 릴리스 빌드·cosign은 돌리지 않았다. macOS의 bash 3.2에서 `set -u` + 빈 배열은 오류지만 배열에는 늘 이번 실행의 zip이 들어 있다.
- BtbN `ffmpeg.exe`/DLL에 `libavutil`·`libswscale`가 있는지는 BtbN 바이너리가 없어 확인하지 못했다(gyan 7.1로만). release 프로필 exe로는 서명 검사를 돌리지 않았다(debug로만).

## §Fix round 8 (2026-10-20, 기준 47f9625)

47f9625 확인 리뷰의 Low 셋(L1–L3)과 추정 하나(H)를 처리했다.

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| L1 | `build-release.sh`의 두 가드(`find -iname '*-UNCONFIRMED.zip'`, SHA256SUMS glob `*-unconfirmed.zip`)가 이름 **끝**만 봐서 브라우저/탐색기 사본 `FrameTrace-9.9.9-win64-UNCONFIRMED (1).zip`·`… - 복사본.zip`이 통과 → SHA256SUMS에 실리고 `--sign`으로 서명됨. round 7의 픽스처는 계획 문서에 설명만 있고 스크립트가 없었다. | 이름: dist/의 zip 이름을 소문자로 바꿔 `*unconfirmed*.zip`(가드·glob 둘 다). 내용: dist/의 **모든 zip**에서 `THIRD_PARTY_NOTICES.md`(폴더·대소문자 무관, basename 비교)를 꺼내 공백·줄바꿈을 접고 소문자로 `owner\s+to\s+confirm`(bash `[[ =~ ]]` — `grep -q`+pipefail SIGPIPE 회피)이면 거부. 읽기 도구: `unzip`(`-Z1`로 목록 → `-p`; Git Bash의 unzip은 `'*THIRD_PARTY_NOTICES.md'` 와일드카드가 `/`를 넘지 못해 목록 방식), 없으면 `python3`/`python -I`의 `zipfile`, 없으면 bsdtar, 모두 없으면 경고 “NOT checked for the 'Owner to confirm' marker” 후 내용 검사 생략. `FRAMETRACE_ZIP_READER`(unzip\|python\|tar\|none)로 강제(테스트용). 빌드 전·SHA256SUMS 직전 두 번. 도움말 범위 `2,17p`. 새 **`scripts/test-build-release.sh`**(Git Bash/Linux/macOS, 가짜 `cargo`·`rustc`·`zip`을 PATH 앞에; 마커 zip은 Python `zipfile`로 만들고 없으면 그 경우 skip). `docs/repro-build.md`, `docs/PLAN_TOOLS_C_2026-10-20.md` §7 게이트 목록에 추가. CI에는 Linux 셸 테스트 단계가 없어(harnesses.yml은 node 하네스만) 건드리지 않음. | `bash scripts/test-build-release.sh` **58 ok, 0 failed, 0 skipped**: 이름 5종(`-UNCONFIRMED.zip`·`-unconfirmed.zip`·`UNCONFIRMED (1).zip`·`UNCONFIRMED - 복사본.zip`·`복사본 - …-Unconfirmed.ZIP`) → exit 1·cargo 0회·SHA256SUMS 없음·이름 표시·NOT FOR RELEASE; 빌드 중 사본 이름 심기 → 두 번째 검사로 exit 1; 깨끗한 dist/ → exit 0·두 zip 나열; 두 번째 검사를 뺀 사본 → glob만으로 사본 이름 제외; 마커(`(Owner to\r\n   confirm …)`, `FrameTrace-9.9.9-win64/THIRD_PARTY_NOTICES.md`) → 기본·python·unzip 읽기 각각 exit 1·cargo 0회, `none` → exit 0 + 경고; 루트의 소문자 `third_party_notices.md` → exit 1; 빌드 중 마커 zip 심기 → exit 1; 확인된 고지 zip·깨진 zip → 통과·나열; `--help`. 같은 하네스를 47f9625의 `build-release.sh`로 돌리면 **29 실패**(사본 이름·마커 전부). |
| L2 | `a_failed_listing_and_an_unknown_trial_assume_mpeg4`의 Windows 분기: std는 `.cmd`를 `cmd.exe /c`로 띄우므로(절대 경로 + `.cmd` 확장자 → `resolve_exe`가 존재 확인 없이 넘기고 배치 파일은 cmd.exe로 실행) 지운 `.cmd`도 spawn되어 exit 1 = `Fails` → mpeg4 대조 경로로 같은 결과. 계획 문서의 “두 분기” 주장이 틀렸다. | 테스트 전용 훅 `tests::trial_forced_unknown`: 바이너리 옆에 `force-unknown` 파일이 있으면 `trial_encode`가 `Trial::Unknown`(`#[cfg(test)]`만). Windows 가짜는 libx264 시험에서 자신을 지우는 대신 `force-unknown`을 만들고 exit 1. 유닉스 분기는 그대로(자기 삭제 → 실제 ENOENT). 새 비공개 필드 `gave_up_unknown`(접근자 `gave_up_on_unknown_trial`) — `Trial::Unknown`으로 포기했을 때만 true, JSON에는 없음. round 7 표의 문구를 정정. | Unknown 테스트: 호출 2회, Windows는 `force-unknown` 있음/유닉스는 파일 없음, **`gave_up_on_unknown_trial()` true**, `spawn_failed()` false, mpeg4·note 전문. mpeg4 대조 실패 테스트에 `gave_up_on_unknown_trial()` **false** 단언 추가 — 두 경로가 서로를 대신해 통과할 수 없다. 옛 Windows 가짜(지운 `.cmd`)로는 이후 시험이 cmd.exe로 돌아 `gave_up_on_unknown_trial` false → 실패(논리 확인). |
| L3 | 파일은 있는데 spawn할 수 없는 ffmpeg(백신 차단·다른 아키텍처): 목록 실패, 첫 시험 `Unknown` → `no_evidence`·`trial_ran` false·시간 초과 없음 → note None인데 h264는 "mpeg4"; doctor는 경고 없이 ok, 프록시·내보내기는 60 s 동안 조용히 mpeg4. | `probe(binary, exists)`(`for_binary`의 `metadata`가 Some인지). `Trial::Unknown`으로 포기할 때 `spawn_failed = exists && assumed_candidates && !trial_ran && !listing_timed_out`; `assumed_note`가 맨 먼저 이것이면 “ffmpeg 실행 실패 — mpeg4(MPEG-4 Part 2) 가정 · 잠시 뒤 다시 확인”. note None은 이제 **파일이 없는** ffmpeg뿐. TTL 규칙 그대로(`INCONCLUSIVE_TTL`). doctor: ffmpeg 줄을 `ffmpeg_check` 헬퍼로 빼고 WARN 조건에 `spawn_failed()`를 명시(mpeg4라 원래도 WARN 조건에 걸리지만 의도를 고정). `docs/ui-rebuild-contract.md` §28: Fix round 7 줄의 “시험이 하나도 못 돈 없는 ffmpeg는 note 없음”을 고치고 Fix round 8 줄 추가. JSON 키 변경 없음. | `an_existing_ffmpeg_that_cannot_run_has_a_note`(Windows 0바이트 `ffmpeg.exe`, 유닉스 0644 `#!/bin/sh`): `spawn_failed`·`no_trial_evidence`·`gave_up_on_unknown_trial` true, h264 "mpeg4", `trial_failed` [], note 전문, TTL ≤ 60 s; 같은 폴더의 없는 경로 → `spawn_failed` false·note None. doctor `an_unrunnable_ffmpeg_is_a_warning`: WARN, detail 끝이 “— 클립 · ffmpeg 실행 실패 — mpeg4(MPEG-4 Part 2) 가정 · 잠시 뒤 다시 확인”; FULL 목록은 OK·note 없음. |
| H | (추정) `fake_ffmpeg_slow` 유닉스 분기의 `sleep N` 자식이 죽은 sh보다 오래 살아 stdout 파이프를 쥐면, 시간 초과 실행기가 EOF까지 읽을 경우 `PROBE_BUDGET + 8s` 단언이 늘어날 수 있음. | **문제 없음 — 코드 변경 없음(주석만)**: `util::run_with_timeout_detailed`는 시간 초과 때 `kill`·`wait` 후 바로 `Err(TimedOut)`을 돌려주고 읽기 스레드를 **join하지 않는다**(스레드는 고아 sleep이 끝나 EOF가 되면 스스로 끝남). join은 자식이 제때 끝난 `Some(status)` 경로에서만 — 그때 sh는 sleep을 기다린 뒤 끝나므로 고아가 없다. Windows의 `ping`도 같다. 가짜의 sleep 분기에 이 설명 주석. | 코드 읽기(`src/util.rs` 335–382). 시간 측정 테스트(`a_failed_listing_with_timed_out_trials_has_a_note` 등)는 이 PC에서 통과. |

### API 변경 (트랙 U)
- 없음(JSON 키 그대로). ffmpeg `encoders.note`: 파일은 있으나 실행할 수 없는 ffmpeg에 “ffmpeg 실행 실패 — mpeg4(MPEG-4 Part 2) 가정 · 잠시 뒤 다시 확인”(종전 null). UI는 `note`를 그대로 보여 준다.

### 게이트 (이 PC, Windows 11, 기준 47f9625 + 이 라운드 커밋, CARGO_TARGET_DIR=target-fix8)
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok(Windows만) · `cargo test --locked` **1247 passed / 0 failed / 30 ignored**(lib 1224 + 통합 14·5·4); `cargo test --lib -- ffmpeg_encoders doctor` 31/31(ffmpeg_encoders 24 + doctor 7).
- `node scripts/check-ps-bom.mjs` ok · `check-js` ok · `check-css` ok · `verify-review-batch --unit` 147 PASS · `node scripts/test-make-portable.mjs` all checks passed(112 ok) · **`bash scripts/test-build-release.sh` 58 ok, 0 failed, 0 skipped**.
- `FRAMETRACE_R11_STRICT=1` verify-i18n 770/770(바뀐 문구는 서버 note뿐 — UI 키 변경 없음).

### 확인 못 한 것
- Linux/macOS clippy·테스트는 돌리지 못했다. 새 `cfg(not(windows))` 코드는 `unrunnable_ffmpeg`의 0644 파일 하나(루트로 돌아도 실행 비트가 없으면 EACCES).
- `test-build-release.sh`는 Git Bash(bash 5.2, Info-ZIP unzip, Windows Python 3.12)에서만 돌렸다. bsdtar 읽기 경로는 이 PC에 bsdtar가 없어 돌리지 못했다(GNU tar는 zip을 못 읽어 자동 선택에서 제외). macOS bash 3.2에서는 하네스의 `readers+=()`·`$'..'`는 동작하지만 실행해 보지 않았다.
- 실제 백신 차단·다른 아키텍처 exe로는 L3를 재현하지 않았다(0바이트 `.exe` = ERROR_BAD_EXE_FORMAT로만).

## §Fix round 9 (2026-10-20, 기준 d54f002)

round 8 확인 리뷰의 Low 넷(L1–L4)을 처리했다.

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| L1 | `build-release.sh`의 내용 검사가 읽기 도구의 실패를 `2>/dev/null \|\| true`로 삼켜, 도구는 있는데 실패하면 “깨끗함”으로 통과: 항목 경로의 `[x]`(Info-ZIP `unzip -p`가 이름을 와일드카드로 읽어 아무것도 못 읽음, exit 11), `-Z1` 없는 BusyBox식 unzip, Microsoft Store 별칭 `python3`(exit 9009), 암호화·지원 안 되는 압축. | 읽기 도구 순서를 **Python 먼저**: `find_python`이 `python3`, 다음 `python`을 `-c 'import zipfile'`로 확인한 뒤 사용(Store 별칭은 건너뜀) → `unzip`(`-Z1` 목록, `-p` 읽기) → bsdtar(`tar --version`을 `case`로 — `grep -q`+pipefail 회피). `read_notices_with <reader> <zip>`: 0 = 성공, 1 = 이 zip에서 실패, 3 = 없음. Python 읽기는 **모든 예외**(`BaseException`; `BadZipFile` 포함)에 exit 1. `unzip -Z1`·`-p`가 0이 아니면 실패; 항목 이름에 `[ ] * ? \`가 있으면 `unzip -p`로 정확히 가리킬 수 없으므로 실패(다음 도구로). `notices_of_zip`은 각 도구의 출력을 따로 받아(실패한 도구의 일부 출력은 버림) 첫 성공을 쓰고, 있는 도구가 모두 실패하면 4 → 호출부가 “`<zip> (THIRD_PARTY_NOTICES.md: cannot verify — every zip reader failed on it)`”로 **거부(exit 1)**. `FRAMETRACE_ZIP_READER`는 이제 도구 하나만 강제(대체 없음). 머리 주석 6행 “… or cannot be read”(`--help` 범위 `2,17p` 그대로). **동작 변경**: 깨진 zip(`not a zip`)은 종전엔 통과·SHA256SUMS에 실렸으나 이제 거부. | `bash scripts/test-build-release.sh` **95 ok, 0 failed, 0 skipped**(종전 58). 새 경우: `FrameTrace-9.9.9-win64[x]/THIRD_PARTY_NOTICES.md` — 기본 읽기는 마커 발견(exit 1)·확인된 고지는 exit 0, `unzip`만 강제하면 마커/확인 모두 exit 1 “cannot verify”(깨끗함 아님); 실패하는 가짜 `unzip`(exit 1) 강제 → 마커/확인 모두 exit 1·cargo 0회; Store 가짜 `python3`·`python`(9009) + 실패 `unzip` → exit 1 “cannot verify”(bsdtar 있으면 skip); Store 가짜 `python3` + 진짜 `python` + 실패 `unzip` → 마커 발견(=python이 읽음)·확인은 exit 0; 두 Python 모두 Store 가짜 → 진짜 `unzip`이 마커 발견·확인은 exit 0; 확인된 고지를 기본·python·unzip 각각으로 exit 0; 깨진 zip → exit 1·cargo 0회·“every zip reader failed”. 가짜 `zip`은 이제 진짜 zip(README 항목 하나, Python 없으면 빈 zip)을 쓴다 — 두 번째 검사가 읽을 수 있어야 하므로; `other-1.0-aarch64.zip`도 빈 zip(`empty_zip`). 같은 하네스를 d54f002의 `build-release.sh`로 돌리면 **28 실패**(67 ok). |
| L2 | 읽기 도구가 하나도 없으면 경고만 하고 SHA256SUMS·`--sign`까지 진행. | 기본은 **실패(exit 1)**: 각 zip을 “`cannot verify — no zip reader`”로 나열하고 “install one, or set FRAMETRACE_SKIP_NOTICES_CHECK=1 …” 안내. `FRAMETRACE_SKIP_NOTICES_CHECK=1`일 때만 종전 경고(“NOT checked for the 'Owner to confirm' marker”) 후 진행(이름 검사는 그대로). `docs/repro-build.md` 갱신(순서·실패 시 거부·환경 변수). | 하네스 `none`: 기본 → exit 1·cargo 0회·SHA256SUMS 없음·“cannot verify — no zip reader”·`FRAMETRACE_SKIP_NOTICES_CHECK=1` 안내; `FRAMETRACE_SKIP_NOTICES_CHECK=1` → exit 0·경고·SHA256SUMS 있음. 하네스는 Python이 없으면 나머지 경우를 `FRAMETRACE_SKIP_NOTICES_CHECK=1`로 돌린다(마커 경우는 skip). |
| L3 | note “ffmpeg 실행 실패 — mpeg4(MPEG-4 Part 2) 가정 …”이 프록시가 mpeg4로 만들어진다는 뜻으로 읽히지만, 실행할 수 없는 ffmpeg는 spawn에서 실패한다. §28은 조건을 “둘 다 spawn 실패”로만 적었다. | note → “**ffmpeg 실행 실패 — 클립·프록시·내보내기가 실패할 수 있음 · 잠시 뒤 다시 확인**”(`src/ffmpeg_encoders.rs` `assumed_note`; 영어 쪽 없음 — 서버 note는 한국어만, verify-i18n 픽스처에 이 문구 없음). `spawn_failed` 필드 문서에 정확한 조건. `docs/ui-rebuild-contract.md` §28 Fix round 8 줄: 문구 교체 + 조건 = 파일 있음 **그리고** `-encoders` 목록 실패(0이 아닌 종료 **또는** 빈 목록 **또는** spawn 실패, 시간 초과 아님) **그리고** 첫 시험 spawn 실패(`Trial::Unknown`, 그 전에 돈 시험 없음). | `an_existing_ffmpeg_that_cannot_run_has_a_note`·doctor `an_unrunnable_ffmpeg_is_a_warning`의 기대 문구를 새 문구로. |
| L4 | doctor `ffmpeg_check`의 `\|\| encoders.spawn_failed()`는 중복(spawn_failed ⇒ no_evidence ⇒ mpeg4 ⇒ !is_h264). | 그대로 두고 한 줄 주석 “redundant on purpose: pins the intent even if the mpeg4 fallback changes”. | 코드 읽기. |

### API 변경 (트랙 U)
- 없음(JSON 키 그대로). `encoders.note`(실행할 수 없는 ffmpeg) 문구만: “ffmpeg 실행 실패 — 클립·프록시·내보내기가 실패할 수 있음 · 잠시 뒤 다시 확인”. UI는 `note`를 그대로 보여 준다.
- 릴리스 절차: `build-release.sh`가 zip 읽기 도구 없음/읽기 실패에 exit 1. 우회는 `FRAMETRACE_SKIP_NOTICES_CHECK=1`(읽기 도구 없음에만).

### 게이트 (이 PC, Windows 11, 기준 d54f002 + 이 라운드 커밋, CARGO_TARGET_DIR=target-fix9)
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok(Windows만) · `cargo test --locked` **1247 passed / 0 failed / 30 ignored**(lib 1224 + 통합 14·5·4).
- `node scripts/check-ps-bom.mjs` ok · `check-js` ok(54) · `check-css` ok · `verify-review-batch --unit` 147 PASS · `node scripts/test-make-portable.mjs` all checks passed(112 ok) · **`bash scripts/test-build-release.sh` 95 ok, 0 failed, 0 skipped**.
- `FRAMETRACE_R11_STRICT=1` verify-i18n 770 ko / 770 en(UI 키 변경 없음).

### 확인 못 한 것
- bsdtar 읽기 경로(이 PC는 GNU tar)와 BusyBox unzip 실물은 돌리지 못했다 — BusyBox는 “`unzip -Z1` 실패 → 다음 도구”와 같은 경로라 가짜 실패 `unzip`으로 대신했다. 암호화 zip 픽스처는 만들지 않았다(Python `zipfile`은 쓰기 암호화를 못 함; 읽기 예외 → exit 1 경로는 깨진 zip으로 확인).
- Linux/macOS에서 하네스·clippy·테스트는 돌리지 못했다.

## §Fix round 10 (2026-10-20, 기준 233825f)

round 9 확인 리뷰의 Medium 하나·Low 하나와 값싼 짐작 둘을 처리했다. Rust 변경 없음(cargo 안 돌림).

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| M | `build-release.sh`의 bsdtar 읽기가 `tar -xOf zip --include='*THIRD_PARTY_NOTICES.md'` — 맞는 항목이 없으면 bsdtar가 exit 1(“Not found in archive”), 게다가 `--include`는 대소문자 구분. 이 스크립트가 만드는 zip(바이너리 + README, 고지 없음)을 bsdtar만 쓸 수 있는 호스트(Python 없음, BusyBox/실패하는 unzip: Alpine + libarchive-tools, BSD)에서는 두 번째 검사가 “every zip reader failed”로 **자기 zip을 거부**. | tar 읽기도 unzip 읽기와 같게: `tar -tf zip`으로 목록(실패 → 이 읽기 실패), 각 줄의 `\r` 제거(Windows bsdtar는 CRLF로 나열), basename을 소문자로 비교해 `third_party_notices.md`만 고르고, 항목 이름에 `[ ] * ? \`가 있으면 실패(bsdtar도 이름 인자를 패턴으로 읽음), `tar -xOf zip -- "$entry"`로 그 항목만 읽음. 고지 항목이 없으면 rc 0(깨끗함). | `FRAMETRACE_TEST_BSDTAR=C:/Windows/System32/tar.exe bash scripts/test-build-release.sh` **111 ok, 0 failed, 0 skipped**. 새 tar 경우(`FRAMETRACE_ZIP_READER=tar`): 고지 없는 zip + 스크립트 자신의 zip → exit 0·SHA256SUMS에 우리 zip; 마커 zip → exit 1·cargo 0회·마커 표시; 빌드 중 심은 마커 zip → exit 1·cargo 1회; 확인된 고지 → exit 0; 대문자 확인 고지 + 소문자 마커 고지(한 zip) → exit 1·마커 표시; `[x]` 항목 → exit 1 “cannot verify”; 깨진 zip → exit 1 “cannot verify”. 같은 하네스를 233825f의 `build-release.sh`로 돌리면 **4 실패**(106 ok): 고지 없는 zip exit 0·SHA256SUMS, 확인된 고지 exit 0, 대문자+소문자의 마커 표시. |
| L | 하네스가 `python3`만 골라 import가 실패하면 `PY=""` — `python3`가 Store 별칭이고 `python`이 진짜인 PC에서 마커 경우가 모두 조용히 skip(나머지는 `FRAMETRACE_SKIP_NOTICES_CHECK=1`)되는데도 “all checks passed”. | `for c in python3 python`에 `-I -c 'import zipfile'` 탐침(스크립트의 `find_python`과 같음). 요약에 skip이 있으면 “`SKIPPED n (see the 'skip' lines above)`”, 마지막 줄 “`all run checks passed (n SKIPPED)`”(exit는 0 그대로). bsdtar 경우는 `tar --version`에 “bsdtar”가 있거나 `FRAMETRACE_TEST_BSDTAR=<path>`(그 경로를 exec하는 가짜 `tar`를 PATH 앞에)일 때만 돈다. | exit 9009 가짜 `python3`를 PATH 맨 앞에 두고 하네스 → 95 ok, 0 failed, 1 skipped(bsdtar만 skip; 마커 경우는 `python`으로 돌았다). |
| 짐작 | `unzip -p`가 암호화 항목에서 tty에 암호를 물을 수 있음; `find_python` 탐침에 `-I` 없음. | `unzip -p -P '' "$zip" "$entry"`(암호화 항목은 바로 실패 → 다음 도구); `"$cand" -I -c 'import zipfile'`. | 보통 zip에서 `unzip -p -P ''` 그대로 읽힘(손으로 확인); 하네스의 unzip 경우 통과. |

### 게이트 (이 PC, Windows 11, 기준 233825f + 이 라운드 커밋)
- `bash scripts/test-build-release.sh`: **95 ok, 0 failed, 1 skipped** + “SKIPPED 1”(이 PC의 `tar`는 GNU tar → bsdtar 경우 skip); `FRAMETRACE_TEST_BSDTAR=C:/Windows/System32/tar.exe`(bsdtar 3.8.9)로 **111 ok, 0 failed, 0 skipped**.
- `node scripts/check-ps-bom.mjs` ok · `bash -n` 두 스크립트 ok.
- `docs/repro-build.md`: 읽기 도구가 목록을 보고 고지 항목만 읽는다는 것·고지 없는 zip은 깨끗함·하네스의 bsdtar 경우/`SKIPPED n`.

### 확인 못 한 것
- 진짜 Alpine/BSD bsdtar(Windows 동봉 bsdtar 3.8.9로 대신)·BusyBox unzip 실물. 암호화 zip 픽스처는 여전히 없음(`-P ''`는 코드 읽기와 보통 zip 회귀만).

### Fix round 10 추가 (2026-10-20, 10차 확인 리뷰 반영, 기준 f6c1781)
- Medium(PR 전체): Python 읽기가 `namelist()` + `z.read(name)`이라 같은 이름 항목이 둘이면 **마지막 복사본만** 읽어, 앞의 마커 복사본이 뒤의 깨끗한 복사본에 가려 통과했다(기본 순서가 Python 먼저라 서명까지 갈 수 있었음). `infolist()` + `z.read(info)`로 모든 복사본을 읽는다. 하네스에 "중복 이름(마커가 먼저)" 경우를 default·python·unzip 읽기마다 추가 — 옛 스크립트로 돌리면 default·python 4건이 실패(113 ok / 4 failed), 새 스크립트는 117 ok / 0 failed(`FRAMETRACE_TEST_BSDTAR` 지정, bsdtar 3.8.9).
- Low: unzip 목록 루프에도 `\r` 제거를 넣었다(CRLF로 목록을 내는 네이티브 Win32 unzip에서 고지 항목이 마지막 줄이 아니면 건너뛰어 열린 쪽 실패). 실물 CRLF unzip으로는 확인하지 못했고, Python이 있으면 이 경로에 오지 않는다.
- 확인 못 한 것: Windows bsdtar는 비ASCII 항목 이름을 CP949로 출력해 그 이름으로 꺼내면 "Not found in archive"(rc 1, 닫힌 쪽 실패 → 잘못 거부). Git Bash 기본 tar는 GNU라 하네스 경로에서만 생긴다. crypt 없이 빌드된 Info-ZIP에서 `-P ''`가 사용법 오류가 되면 unzip 읽기가 실패해 다음 도구로 넘어간다(닫힌 쪽).
- 범위 밖 메모(10차 리뷰 짐작): `build-release.sh`의 서명 단계가 `python3`를 탐침 없이 부르고(Store 별칭이면 `COSIGN_KEY` 경로가 깨짐), `mktemp -t … ` 뒤 `.json`을 붙여 확장자 없는 빈 임시 파일이 남는다. 옵션 A PR에서 다룬다.
- Low(최종 확인 리뷰, 1d707d5): unzip 목록에 백슬래시 구분자가 그대로 나오면(`a\THIRD_PARTY_NOTICES.md`) basename 비교가 어긋나 고지 항목을 건너뛰고 "깨끗함"이 될 수 있었다. `/`로 정규화한 사본으로 basename을 비교하고, 원래 이름은 기존 와일드카드 거부 분기(`*'\'*`)에 걸려 읽기 실패 → 다음 도구 또는 거부가 되게 했다. 하네스에 "backslash entry" 경우를 추가(기본 읽기: 마커 거부; unzip 전용: 목록을 정규화하는 unzip이면 읽고 판정, 그대로 내는 unzip이면 cannot verify — 어느 쪽이든 "깨끗함"은 아님). 이 PC의 Info-ZIP 6.00은 목록에서 이미 `/`로 바꿔 주므로 옛 스크립트도 통과해, 그대로 내는 unzip 빌드에서의 실패는 실물로 재현하지 못했다(123 ok / 0 failed, bsdtar 3.8.9 포함).

## §Fix round 11 (2026-10-21, Fable 최종 리뷰 반영, 기준 f02d196)

Fable 최종 리뷰의 Medium 둘(M1·M2)·Low 넷(L1–L4)과 값싼 짐작 셋(H2·H4·H5)을 처리했다. H1(실제 4K/VFR h264_mf 인코딩)·H3(OneDrive 폴더)는 하지 않았다 — 아래 "소유자·실기 확인"에 남긴다.

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| M1 | 첫 화면에서 FFmpeg 설치(busy 임대 보유) 중 [종료]가 "작업을 중단한 뒤 종료"라며 `/api/cancel`을 불렀는데, `api_cancel`은 `guard.busy`만 보고 `cancel_requested = true`·로그 "중단 요청을 받았습니다. 현재 단계를 멈추는 중입니다."·`ok:true cancelled:"job"`. 설치 작업자는 자기 `Progress` 래치만 보므로 멈추지 않았고, UI는 60 s 기다린 뒤 "아직 멈추지 않았습니다", `/api/shutdown`은 계속 `running:true`, 로그에는 거짓 줄. | `src/serve/system.rs` `api_cancel`: busy이고 `tool_install`이 running이면 job 경로 대신 **그 설치의 `progress.request_cancel()`** → `{ok:true, cancelled:"tool-install", job_kind:"tool-install", install:J}`(로그 "도구 설치(<이름>) 중단 요청을 받았습니다."), 파일 이동/venv/pip 단계면 `{ok:false, code:"not_cancellable", error, job_kind:"tool-install", install:J}`(로그 없음), 작업자가 이미 끝남(stage done/failed, 게시 직전)이면 `cancelled:"tool-install"`만(요청·로그 없음). `cancel_requested`는 건드리지 않는다. `assets/app/95-quit.js`: `not_cancellable`이면 "파일 이동 단계라 중단할 수 없습니다 — 끝난 뒤 종료합니다"를 보이고 busy가 풀릴 때까지(최대 60 s) 기다린 뒤 종료, `cancelled:"tool-install"`이면 "도구 설치를 중단하는 중…". 잘못 붙어 있던 `api_cancel`의 doc 주석(verify-audit 설명)을 `api_verify_audit`로 옮기고 `api_cancel`에 맞는 주석. `assets/legacy/app/95-quit.js`(롤백 화면, 설치 UI 없음)는 그대로 — 거기서는 `not_cancellable`이 "중단 요청 실패"로 보이고 다시 누르면 된다. | `serve::tests_tools::the_global_cancel_stops_a_tool_install_through_its_own_latch`(래치 설정·`cancel_requested` false·"현재 단계를 멈추는 중" 없음; Install 단계 → `not_cancellable`·로그 0줄; Done 단계 → 조용히 확인), `windows::the_global_cancel_ends_a_running_install_and_shutdown_proceeds`(진짜 작업자, 게이트 걸린 가짜 소스: shutdown `running:true` → `/api/cancel` → 작업자 `INSTALL_CANCELLED`·`cancelled:true`·busy 해제·거짓 줄 없음·`fake.exe` 없음 → shutdown `ok:true`). `scripts/lib/tools-mock.mjs`에 `apiCancelReply(m)`(서버 모양; `routeTools(page,{routeCancel:true})`로 `/api/cancel` 라우트), `tools-unit.mjs`에 VM 두 줄: 다운로드 중 [종료] → posts `['/api/cancel','/api/shutdown']`·설치 cancelled·"도구 설치를 중단하는 중"; 이동 단계 → 안내 문구·설치 완료까지 대기·"중단 요청 실패" 없음·종료. |
| M2 | `tools/manifest.json` `notice`/`notice_en`(바이너리에 컴파일, 대화 상자·CLI 프롬프트에 표시)와 README가 "파일 전체는 GPL 조건을 따릅니다"·"복사해 주면 GPL 의무는 복사하는 사람에게" — GPL로 재배포할 수 있다는 법적 결론으로 읽힌다. GPL-2.0-only(zvbi packet-830/pdc)는 같은 바이너리에 정적으로 링크된 LGPL-3.0(`--enable-version3`)·Apache-2.0(OpenSSL 3)과 양립하지 않아 어느 GPL 버전으로도 재배포가 허용되지 않는다(이 문서·security-review의 "재배포 불가"). | 결론을 빼고 사실만(ko/en 같은 내용): ko "… 빌드 이름은 lgpl이지만 GPL-2.0-only(zvbi)·LGPL-3.0·Apache-2.0 코드가 섞여 있어, FrameTrace는 이 파일을 다른 사람에게 넘겨도 되는지 보증하지 않습니다. 다른 PC에는 복사하지 말고 각자 BtbN에서 받으십시오. 이 라이선스들은 내 PC에서의 사용을 제한하지 않습니다(특허는 별개)." / en "… it mixes GPL-2.0-only (zvbi), LGPL-3.0 and Apache-2.0 code, so FrameTrace makes no assurance that the file may be passed on to others. Do not copy it to other PCs; let each user download it from BtbN. …". README 설치 절·`THIRD_PARTY_NOTICES.md`의 해당 문장도 같은 사실로(마커 "(Owner to confirm this paragraph.)"는 그대로). **가드**: 매니페스트 ffmpeg에 `"notice_confirmed": false`; `scripts/make-portable.ps1`은 sha256이 고정된(설치 가능한) 도구 중 `notice_confirmed`가 불리언 `true`가 아닌 것이 있으면 거부(`-AllowUnconfirmedNotices`면 같은 UNCONFIRMED 이름·`SHA256SUMS.unconfirmed`·큰 경고에 이유 줄), `tools/manifest.json`이 없으면 실패. CI의 패키지 확인 단계도 같은 조건으로 `$unconfirmed`를 계산(CI는 이미 이 스위치를 넘김). Rust는 모르는 필드를 무시하므로 코드 변경 없음. | `tool_install::tests::the_ffmpeg_notice_states_facts_not_a_gpl_redistribution_conclusion`(ko/en 모두 GPL-2.0-only·LGPL-3.0·Apache-2.0·zvbi, "보증하지 않습니다"/"makes no assurance", "복사하지 말고"/"Do not copy", 옛 결론 문구 없음). `scripts/test-make-portable.mjs` **123 ok**(종전 112): `notice_confirmed` false·없음·문자열 `"true"` → 거부·zip 없음; false + 스위치 → UNCONFIRMED zip·`SHA256SUMS.unconfirmed`만·"notice_confirmed is not true for: ffmpeg"; 매니페스트 없음 → 실패; 실제 `tools/manifest.json` → 거부(unconfirmed: ffmpeg). 가짜 저장소는 기본으로 확인된 매니페스트를 쓴다. verify-layout의 알림 대화 상자 검사(“배포하지 않”·대비 ≥ 4.5·목록 뒤)는 새 문구로 통과(길이 비슷, 높이 문제 없음); tools-mock 픽스처 문구 갱신. verify-i18n은 이 문구를 다루지 않음(770/770 그대로). |
| L1 | `docs/ffmpeg-btbn-components-autobuild-2026-09-30.md`의 zvbi 행 "LGPL-2.0-or-later for src/* (library)"가 THIRD_PARTY_NOTICES(`src/packet-830.*`·`src/pdc.*`는 GPL-2.0-only)와 모순. | "src/* is LGPL-2.0+, except packet-830.* and pdc.* which are GPL-2.0-only (headers checked)". | 문서. |
| L2 | Windows `tar.exe`(bsdtar)는 argv를 시스템 ANSI 코드 페이지로 받는다(**§Fix round 12 L3 정정**: 처음에 `curl.exe`도 그렇다고 적었으나 틀렸다 — 이 PC의 curl 8.21.0은 이모지 `--output` 경로를 그대로 썼고, bsdtar 3.8.9만 망가뜨렸다. curl에도 같은 짧은 경로를 넘기는 것은 그대로 둔다(해 없음)) — exe 폴더 경로에 그 밖의 문자(이모지, en-US의 한글)가 있으면 `?`로 바뀌어 "archive listing failed"로 실패. | `src/tool_install/archive.rs`: 스테이징을 만든 직후 `ansi_safe_dir`: 경로가 ANSI 코드 페이지를 그대로 왕복하면(`WideCharToMultiByte(CP_ACP, WC_NO_BEST_FIT_CHARS)` + 기본 문자 사용 없음 + `MultiByteToWideChar` 결과 일치; ACP가 UTF-8이면 플래그 없이) 그대로, 아니면 `GetShortPathNameW`(8.3) 형태가 왕복하면 그것을, 둘 다 아니면 다운로드 **전에** "FrameTrace 폴더 경로에 이 PC의 코드 페이지로 쓸 수 없는 문자가 있어 설치할 수 없습니다 — 영문 경로의 폴더로 옮긴 뒤 다시 시도하십시오 (<경로>)". curl `--output`, tar의 압축 파일·`-C`가 그 경로 아래를 쓴다(이동·정리는 원래 경로). kernel32 직접 선언(새 의존성·windows 기능 없음), cfg(windows)만(archive 모듈 자체가 Windows 전용). | `windows_installs::an_emoji_folder_installs_via_the_short_path_or_is_refused_clearly`(임시 폴더 `ft-😀-dir` 아래 tools: 성공이면 `fake.exe` 설치, 거부면 그 문구·소스 접촉 0회; 어느 쪽이든 "archive listing failed" 아님·스테이징 없음; 이모지는 ACP가 UTF-8일 때만 왕복). 이 PC(ko-KR, C: 8.3 켜짐)에서는 **짧은 경로로 설치 성공**. 거부 분기(8.3 꺼진 볼륨)는 실기로 돌리지 못했다. |
| L3 | `ENV_CACHE`(TTL 없음, 서버 안 설치만 무효화): 워크스테이션이 도는 동안 콘솔에서 `frametrace tools install ffmpeg`를 하면 `/api/tools`는 설치됨인데 `/api/env`는 재시작 전까지 "ffmpeg 없음". | 캐시 항목에 `<exe>/tools/bin` 지문(바로 아래 파일 이름·크기·mtime, 정렬; 디렉터리 읽기 한 번)을 함께 저장. `/api/env`는 지문이 다르면 항목을 버리고 다시 탐침(지문은 탐침 전에 계산 — 탐침 중 변경은 다음 호출에서 다시 탐침), `/api/tools`도 매번 같은 검사로 낡은 항목을 버린다. PATH 쪽 변경은 여전히 `?refresh=1`. | `serve::tests_tools::the_env_cache_misses_when_tools_bin_changes`(없는 폴더 = 빈 지문, 안정, 파일 추가·크기 변경·같은 크기 mtime 변경 각각 miss·항목 삭제). |
| L4 | `ffmpeg_encoders` note·README "Media Feature Pack 필요 (Windows N/KN 에디션·Server Core)" — Windows Server는 Desktop Experience여도 Server-Media-Foundation 기능이 없으면 MF가 없다. | note " · h264_mf 사용 불가 — Windows N/KN(Media Feature Pack)·Windows Server(Media Foundation 기능 미설치)", 모듈·테스트 주석, README 인코더 절에 같은 설명. | `a_listed_encoder_that_fails_its_trial_is_skipped`가 새 문구 전체를 확인. |
| H2 | CI 설치 스모크에 zvbi GPL-2.0-only 객체가 실제로 링크됐는지 증거가 없었다. | `.github/workflows/windows-ci.yml` 설치 스모크: `ffmpeg.exe` 바이트(Latin-1)에서 zvbi `src/pdc.c`의 서식 문자열 둘(`pcs=%s pty=%02x tape_delayed=%u`, `ch=%u cni=%04x (%s) pil=` — `_vbi_program_id_dump`/`_vbi_pil_dump`, zvbi main에서 읽음)을 찾아 `ffmpeg-install-smoke.txt`에 "zvbi packet-830/pdc linked: yes/no (pdc.c strings found n/2 …; packet-830.c has no strings to probe)"를 남긴다. `packet-830.c`에는 문자열 리터럴이 없고 정적 exe라 심볼도 없다. 정보용 — 실패시키지 않으며 탐침 예외도 "unknown"으로 기록. "no"는 없다는 증거가 아니다(링커가 안 쓰는 함수를 버릴 수 있음). | CI 다음 실행에서 확인(이 PC에서 돌리지 않음). |
| H4 | `move_all_or_nothing`: 막 풀린 `ffmpeg.exe`를 백신이 검사하는 동안의 공유 위반이 설치 전체를 실패시키고 170 MB를 다시 받게 했다. | `rename_with_retry`: `ERROR_SHARING_VIOLATION`(32)·`ERROR_ACCESS_DENIED`(5)일 때만 400 ms 쉬며 최대 5번(약 1.6 s), 이름 바꾸기(대상→백업, 원본→대상)에만. 다른 오류·마지막 시도의 오류는 그대로, 실패 시 되돌리기(all-or-nothing)는 종전과 같다. | `a_briefly_locked_file_is_moved_after_a_retry_and_a_held_one_rolls_back`(공유 없이 연 원본: 500 ms 잡음 → 재시도 뒤 성공, ≥ 400 ms; 4 s 잡음 → 1.5–4 s 안에 실패·첫 이동 되돌림·`c.exe` 없음; 없는 원본은 재시도 없이 즉시 실패). |
| H5 | `video_export` 내보내기 로그의 `ffmpeg_version`을 인코딩 **뒤에** 맨 이름 `ffmpeg`로 다시 물어, 그 사이 설치가 끝나면 옛 바이너리로 만든 클립에 새 버전이 기록될 수 있었다. | 인코딩 **전에** 같은 해석 경로(`resolve_tool_binary` 결과)로 `command_version`을 물어 `write_export_log`에 넘긴다. | `export_log_records_the_version_asked_before_encoding`(넘긴 버전 문자열이 그대로 기록). 동시 설치 경쟁 자체는 재현하지 않았다. |

### API 변경 (트랙 U)
- `POST /api/cancel`: 도구 설치가 busy를 잡고 있으면 `{ok:true, cancelled:"tool-install", job_kind:"tool-install", install:J, clips_cancelled:0, clips_running:0}` 또는 `{ok:false, code:"not_cancellable", error, job_kind:"tool-install", install:J}`. 다른 작업의 `cancelled:"job"`·`"clips"`·`"writes"`는 그대로.
- `tools/manifest.json` 도구에 `notice_confirmed`(bool, 패키징 가드 전용; `/api/tools` 출력에는 없음). `notice`/`notice_en` 문구 변경.
- `encoders.note`의 h264_mf 실패 문구 변경(위 L4).
- `/api/env`: tools/bin이 바뀌면 캐시가 저절로 무효화(키 변경 없음).

### 게이트 (이 PC, Windows 11, 기준 f02d196 + 이 라운드 커밋, CARGO_TARGET_DIR=target-fix11)
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok · `cargo test --locked` **1254 passed / 0 failed / 30 ignored**(lib 1231 + 통합 14·5·4).
- `node scripts/check-ps-bom.mjs` ok · `check-js` ok(54) · `check-css` ok · `verify-review-batch --unit` **149 PASS**(종전 147, +[종료] 두 줄) · `node scripts/test-make-portable.mjs` all checks passed(**123 ok**) · `bash scripts/test-build-release.sh` **107 ok, 0 failed, 1 skipped**(이 PC의 tar는 GNU → bsdtar 경우 skip).
- `FRAMETRACE_R11_STRICT=1 node scripts/verify-i18n.mjs` 770 ko / 770 en · `FRAMETRACE_R11_STRICT=1 node scripts/verify-layout.mjs` **171/171 passed**(debug `frametrace-app`를 `CARGO_TARGET_DIR`로 찾게 함, playwright는 `NODE_PATH`로). 첫 실행은 `cargo test`와 동시에 돌려 홈 화면 대기 시간 초과로 154/155였고, 혼자 다시 돌리면 171/171.

### 확인 못 한 것 / 소유자·실기 확인
- **H1(소유자·실기)**: 실제 4K·VFR 원본을 h264_mf로 프록시·내보내기(속도·품질·VFR 타임스탬프) — 이 라운드에서 하지 않았다.
- **H3(소유자·실기)**: FrameTrace 폴더가 OneDrive(파일 주문형) 아래일 때의 설치·실행(자리 표시자 파일, 동기화 잠금) — 하지 않았다.
- **M2 문구(소유자)**: 새 `notice`/`notice_en`과 THIRD_PARTY_NOTICES 문단은 소유자 확인 전이다. 확인하면 매니페스트 `notice_confirmed: true`와 THIRD_PARTY_NOTICES의 마커 제거가 함께 있어야 `make-portable.ps1`이 릴리스 zip을 만든다.
- L2의 거부 분기(8.3 이름이 꺼진 볼륨, en-US의 한글 경로)와 Windows Server·N/KN 실기의 MF 실패는 재현하지 못했다. H2의 CI 결과는 다음 실행에서 본다.
- legacy 롤백 화면의 [종료]는 M1 UI 처리를 받지 않았다(위 M1 행).

## §Fix round 12 (2026-10-21, Fable 확인 리뷰 반영, 기준 849a5bf)

849a5bf에 대한 Fable 확인 리뷰의 Medium 하나(M)·Low 다섯(L1–L5)·짐작 하나(H-a)를 처리했다.

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| M | `tools/manifest.json`(알림 포함)은 `include_str!`로 **모든 플랫폼** 바이너리에 들어가고 `frametrace tools`는 Linux/macOS에서도 알림을 보이는데, `scripts/build-release.sh`는 `notice_confirmed`를 몰랐고 zip에 `THIRD_PARTY_NOTICES.md`가 없어 기존 내용 검사가 읽을 것이 없었다 → `notice_confirmed: false`인 채로 비Windows 릴리스 zip이 나갈 수 있었다. | `build-release.sh`: cargo 전에 작업 트리 검사 `source_problems`(첫 `refuse_unconfirmed --source` 안): 설치 가능한 도구(`sha256`이 있고 null·`""` 아님 — make-portable.ps1과 같은 규칙)마다 `notice_confirmed`가 JSON 불리언 `true`(문자열 `"true"`·`1`·키 없음은 거부), `THIRD_PARTY_NOTICES.md`가 있고 `owner\s+to\s+confirm`(줄바꿈·대소문자 무시)이 없어야 한다. 매니페스트는 `python3`/`python -I`의 `json`으로 읽고(`utf-8-sig`), Python이 없으면 "cannot verify"로 실패 — `FRAMETRACE_SKIP_NOTICES_CHECK=1`일 때만 경고 후 진행(Python이 내린 판정·THIRD_PARTY_NOTICES 검사는 건너뛰지 않음). 실패면 "the working tree is not releasable — NOT FOR RELEASE"와 문제마다 한 줄, exit 1, cargo 0회. zip에 `THIRD_PARTY_NOTICES.md`·`LICENSE` 추가(두 번째 내용 검사가 실제로 읽는다). `docs/repro-build.md` 갱신. make-portable과 달리 테스트용 스위치는 없다(유닉스 zip은 릴리스용뿐). | `scripts/test-build-release.sh` **201 ok, 0 failed, 1 skipped**(bsdtar 없음) / `FRAMETRACE_TEST_BSDTAR=C:/Windows/System32/tar.exe` **217 ok, 0 failed, 0 skipped**(종전 107). 가짜 저장소에 확인된 매니페스트·깨끗한 알림·LICENSE, 가짜 `zip`은 디렉터리 전체를 담는다. 새 경우: false·키 없음·`"true"`·`1`·둘째 설치 도구 미확인 → 거부(exit 1·cargo 0·zip 없음·SHA256SUMS 없음·도구 이름); sha256 `""` 도구 → 통과; 매니페스트 없음·JSON 아님 → 거부; 실제 `tools/manifest.json` → 거부(ffmpeg); Python 없음 → 거부, +SKIP=1 → 경고 후 통과; false+SKIP=1 → 여전히 거부; 작업 트리 마커(CRLF·줄바꿈)·+SKIP=1·알림 없음 → 거부; 둘 다 → 둘 다 이름; 확인+깨끗 → exit 0·zip에 README.md·THIRD_PARTY_NOTICES.md·LICENSE·frametrace. Store-python 경우 하나는 이제 매니페스트에 Python이 필요해 SKIP=1로 돌리고 경고를 확인. |
| L1 | `ENV_CACHE` 지문이 `tools/bin`만 봐서, 콘솔에서 설치한 deepfake-lens(`tools/python/<이름>/Scripts`, `/api/env`는 `private_venv_executable`로 보고)가 재시작 전까지 `deepfake:false`. | `src/serve/system.rs` `tools_python_fingerprint`: `tools/python` 바로 아래 항목마다 이름·mtime·`Scripts`(유닉스 `bin`) 디렉터리 mtime, 정렬. `tools_fingerprint(bin, python)` = 둘을 합친 것; `current_tools_fingerprint`가 이것을 쓴다. `InstallDirs::python_root()` 추가. | `serve::tests_tools::the_env_cache_misses_when_a_private_venv_appears_or_changes`(venv 생김 → miss·항목 삭제; Scripts에 스크립트 추가 + 디렉터리 mtime 변경 → miss; venv 삭제 → miss; tools/bin은 그대로). |
| L2 | 프록시·썸네일 감사 줄의 `ffmpeg_version`을 결과물을 만든 **뒤에** 맨 이름 `ffmpeg`로 물었다. | `src/artifacts.rs`: H5처럼 실행 **전에** 해석된 경로로 `command_version`, `append_artifact_log`에 넘긴다(프록시·썸네일 둘 다). | `artifacts::tests::artifact_log_records_the_version_asked_before_encoding`. |
| L3 | archive.rs 주석·round 11 L2 행의 "curl.exe도 argv를 ANSI로 받는다"가 틀렸다. | 주석·문서를 tar.exe(bsdtar)로 좁히고 curl의 동작을 적음. 짧은 경로는 둘 다에 그대로(해 없음). | 이 PC에서 다시 확인: 이모지 폴더 아래로 `curl.exe -o`(file://) 성공, `tar.exe -cf`는 `ft-??-l3\a.tar`를 열지 못해 실패. |
| L4 | CI 패키지 확인이 `$_.notice_confirmed -ne $true`(문자열 `"true"` 통과)·`$_.sha256 -and`(참/거짓 판정), make-portable은 `-isnot [bool]` → 불일치 시 알 수 없는 CI 실패. | `.github/workflows/windows-ci.yml`: make-portable.ps1의 반복문을 그대로(속성 존재·null·`''` 검사, 불리언 `true`만). | 같은 PowerShell 조각을 이 PC에서 실제 매니페스트에 돌림 → `pending: ffmpeg`. CI는 다음 실행에서. |
| L5 | 도구 설치가 busy일 때 [종료] 대화 상자가 "중단된 분석은 체크포인트에서 이어서"라 했고, `not_cancellable`에서 60 s 뒤 "작업이 아직 멈추지 않았습니다"(venv/pip는 몇 분 걸릴 수 있음). | `assets/app/95-quit.js`: busy면 `GET /api/tools/install-status`로 실행 중 설치를 확인 → "**도구 설치가 진행 중입니다** (<tool>). 도구 설치를 중단한 뒤 서버를 종료합니다 — 파일을 옮기는 중이면 그 단계가 끝난 뒤 종료합니다.", 버튼 "설치 중단 후 서버 종료". `/api/cancel`이 `job_kind:"tool-install"`이면 대기 상한 **10분**(다른 작업 60초 그대로), `not_cancellable` 문구 "파일을 옮기는 중이라 중단할 수 없습니다 — 끝나면 종료합니다 N초 / 최대 10분", 상한을 넘기면 "도구 설치가 아직 끝나지 않았습니다 — 끝난 뒤 다시 [서버 종료]를 누르십시오." 긴 대기 중 [계속 작업]으로 대화 상자를 닫으면(또는 다른 대화 상자로 바뀌면 — `dlgResolve` 토큰) 기다림을 멈추고 **서버를 종료하지 않는다**(종전에는 닫은 뒤에도 최대 60 s 뒤 종료될 수 있었다). | `tools-unit` VM: 다운로드 중 행에 대화 상자 html("도구 설치를 중단한 뒤 서버를 종료합니다"·"(ffmpeg)"·"체크포인트" 없음)·버튼 문구 단언 추가; 이동 단계 행 새 문구; 새 행 둘 — pip 단계가 200 폴링(100 s) 걸려도 기다린 뒤 종료(“최대 10분”, "아직 멈추지 않았습니다"·"최대 60초" 없음), 5 폴링 뒤 대화 상자 닫힘 → `/api/shutdown` 없음. `verify-review-batch --unit` **151 PASS**(종전 149). verify-i18n은 뷰어 표만 다루고 `95-quit.js`는 한국어 전용(en 표 없음)이라 en 문구는 추가하지 않았다 — 770/770 그대로. |
| H-a | `move_all_or_nothing`의 되돌리기(`remove_file(dest)`, `rename(backup, dest)`)에 재시도가 없어, 뒤 이동이 실패한 순간 막 옮긴 새 ffmpeg.exe를 백신이 잡고 있으면 새 파일이 남고 백업은 복원되지 않은 채 조용히 끝났다(그리고 스테이징과 함께 백업도 지워짐). | `src/tool_install/mod.rs`: `remove_file_with_retry`(같은 32/5 재시도, 없음 = 성공), 되돌리기의 삭제·이름 바꾸기 모두 재시도. 그래도 못 되돌리면 오류가 `rollback_incomplete: `로 시작하고 남은 파일마다 "<경로> is the NEW file (cannot remove it: …); the previous one is kept at <백업>" 또는 "… is missing (cannot restore it: …)"; 감사 줄 `"code":"rollback_incomplete"`. `archive.rs`: 그 경우 `preserve_rollback_backup`이 `staging/backup`을 `tools/rollback-backup-<hex>`(오래된 스테이징 청소 대상 아님)로 옮기고 오류의 경로를 바꿔 "(restore the previous files from … by hand)"; 옮기지 못하면 스테이징 디렉터리를 남기고(`StagingDir::keep`) 그렇게 적는다. | Windows `a_rollback_blocked_by_a_held_new_file_is_retried_or_reported_loudly`(c 원본 4 s 잡음 + 첫 이동으로 들어간 새 a.exe를 공유 없이 잡음: 2.2 s → 되돌리기 재시도로 완료·옛 a.exe 복원; 6 s → `rollback_incomplete`·경로·"NEW file"·백업 내용 그대로, `preserve_rollback_backup` 뒤 스테이징 없음·`rollback-backup-*`에 옛 파일·오류에 새 경로·감사 줄 code). `preserve_rollback_backup_leaves_other_errors_alone`(보통 오류는 그대로, 스테이징 삭제, code 없음). |

### API 변경 (트랙 U)
- 도구 설치 실패 감사 줄·오류: 새 `code: "rollback_incomplete"`(오류 앞머리 `rollback_incomplete: `). 그 경우 `tools/rollback-backup-<hex>/`가 남는다.
- `/api/env`: `tools/python` 변경도 캐시를 무효화(키 변경 없음).

### 게이트 (이 PC, Windows 11, 기준 849a5bf + 이 라운드 커밋, CARGO_TARGET_DIR=target-fix12)
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok · `cargo test --locked` **1258 passed / 0 failed / 30 ignored**(lib 1235 + 통합 14·5·4).
- `node scripts/check-ps-bom.mjs` ok · `check-js` ok(54) · `check-css` ok · `verify-review-batch --unit` **151 PASS**(rc 0; 첫 실행은 `cargo test`와 동시라 시간 단언 `R21 U2 case names … < 50 ms`가 92.8 ms로 FAIL, 혼자 다시 돌려 통과) · `node scripts/test-make-portable.mjs` all checks passed(123 ok) · `bash scripts/test-build-release.sh` **201 ok, 0 failed, 1 skipped** · `FRAMETRACE_TEST_BSDTAR=C:/Windows/System32/tar.exe bash scripts/test-build-release.sh` **217 ok, 0 failed, 0 skipped**.
- `FRAMETRACE_R11_STRICT=1 node scripts/verify-i18n.mjs` 770 ko / 770 en · `FRAMETRACE_R11_STRICT=1 node scripts/verify-layout.mjs` **171/171 passed**(혼자 실행, debug `frametrace-app`는 `CARGO_TARGET_DIR`, playwright는 `NODE_PATH`).

### 확인 못 한 것 / 남은 것
- `notice_confirmed: true`·THIRD_PARTY_NOTICES 마커 제거는 **소유자** 몫(그 전에는 make-portable·build-release 모두 릴리스를 거부한다 — 의도).
- `src/tool_install/pip.rs`의 venv 되돌리기(`remove_dir_all(target)` + `rename(backup, target)`)는 H-a의 범위 밖이라 그대로 — 재시도·`rollback_incomplete` 보고가 없다. 다음 라운드 후보.
- H-a의 `preserve_rollback_backup`이 백업 디렉터리를 옮기지 못하는 분기(백업 안의 파일이 잡힌 경우)는 실기로 돌리지 않았다.
- Linux/macOS에서 `build-release.sh`·새 Rust 코드(`tools_python_fingerprint`의 `bin` 분기, 테스트의 `File::open(dir).set_modified`)는 돌리지 못했다. L4는 CI 다음 실행에서 본다.
- 커밋 메모: archive.rs의 L3 주석 변경은 실수로 L3 커밋(문서만)이 아니라 H-a 커밋에 함께 들어갔다.
- 이전 라운드의 H1(4K/VFR h264_mf 실기)·H3(OneDrive) 그대로.

## §Fix round 13 (2026-10-21, Fable 확인 리뷰 반영, 기준 f207a02)

f207a02에 대한 Fable 확인 리뷰의 Medium 하나(M)·Low 넷(L1–L4)·짐작 둘(H1·H2)을 처리했다.

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| M | deepfake-lens(UI·CLI에서 설치하는 on-demand pip 도구)를 다시 설치하다 pip가 실패하면 되돌리기가 `remove_dir_all(target)` + `rename(backup, target)`(재시도 없음, 결과 버림)였다. 새 부분 venv의 파일 하나를 백신·인덱서가 잡으면 `target`이 남아 rename이 조용히 실패 → 이전의 좋은 venv가 `.deepfake-lens.old-<hex>`로 남고, 이 이름은 청소 패턴(`.`로 시작 + `.old-`)과 맞아 **다음 서버 시작·설치 때 삭제**됐다. 오류는 pip 실패만 말했다. | 새 `src/tool_install/rollback.rs` `undo_venv`(pip.rs가 호출): 부분 venv 삭제는 새 `remove_dir_all_with_retry`(H-a와 같은 5회/400 ms, 32·5만 재시도, 없음 = 성공), 되돌리는 이름 바꾸기는 `rename_with_retry`. 그래도 못 끝내면 이전 venv를 통째로 `tools/python/rollback-backup-<hex>/<도구>`(청소 대상 아님)로 옮기고 옆에 `RESTORE.txt`, 오류는 `rollback_incomplete: <pip 오류>; the rollback could NOT restore the previous venv — <target> is the NEW, partial venv (cannot remove it: …) / is missing (cannot restore it: …); the previous venv is kept at <경로> (… RESTORE.txt says how)` → 감사 줄 `code: "rollback_incomplete"`. 그 이동마저 실패하면 `.<도구>.old-<hex>`에 두고 그렇게 적는다. **청소 강화**(`clean_stale_staging`): `.<도구>.old-*`는 `tools/python/<도구>/`에 동작하는 venv 실행기(인터프리터 + 콘솔 스크립트 `<도구>`, `venv_intact`)가 있을 때만 지운다(`….rm`은 그대로 재시도). 이전 venv가 없던 경우 부분 venv를 못 지우면 오류에 그렇게 덧붙인다. | Windows `a_pip_undo_blocked_by_a_held_file_is_retried_or_kept_safe`(가짜 pip가 새 venv의 `Scripts/held.pyd`를 공유 없이 잡고 실패: 1 s → 재시도로 되돌림 완료·이전 venv(표식 파일) 복원·`python/`에 `lens`만; 6 s → `rollback_incomplete`·"NEW, partial venv"·경로, `.old-` 이름 없음, `rollback-backup-*/lens`에 이전 venv 통째(표식·실행기), RESTORE.txt `tool: lens` + `<target> <- lens`, 뒤이은 청소가 지우지 않음, `rollback_backups`에 나옴, 감사 code). `the_sweep_keeps_an_old_venv_while_the_live_one_is_broken`(실행기 없는 live → 0개 삭제, 실행기 생기면 1개; live 없는 `.gone.old-*`는 계속 남음). 기존 청소 테스트 둘은 live venv에 실행기를 둔다. |
| L1 | `tools/rollback-backup-<hex>`의 보존 파일은 `0.bak`, `1.bak`…이고 원래 경로와의 대응은 오류 문자열에만 있었다. | `move_all_or_nothing`이 되돌리지 못한 경우 `backup/RESTORE.txt`를 쓴다: 복원 방법 두 줄 + 파일마다 `<원래 경로> <- <이 폴더 안 이름>`(이전 사본이 없는 새 파일은 `<- (none: NEW file …)`). `preserve_rollback_backup(…, tool)`이 첫 줄에 `tool: <이름>`을 넣고 폴더째 옮기며, 오류에 RESTORE.txt 경로를 덧붙인다. 옮기지 못해 남긴 `.staging-*`(안에 `backup/RESTORE.txt`)는 이제 청소가 건너뛴다(라운드 12에서는 다음 청소가 지울 수 있었다). `rollback_backups(tools_dir)`: `tools/rollback-backup-*`, 그런 `.staging-*`, `tools/python/rollback-backup-*`(도구 = RESTORE.txt 첫 줄, 없으면 하위 폴더 이름)와 크기. `frametrace doctor`: 하나마다 WARN `tool rollback backup (<도구>)` — 경로·크기(MB)·"RESTORE.txt의 안내대로 직접 복원하거나 도구를 다시 설치한 뒤 이 폴더를 지우십시오". | H-a 테스트에 RESTORE.txt(`tool: fake`, `<bin/a.exe> <- 0.bak`, 안내)·오류의 RESTORE.txt 경로·`rollback_backups` 단언 추가. `kept_rollback_backups_are_listed_and_never_swept`(보존 staging·archive·venv 백업 셋 다 청소에서 살아남고 목록 순서·도구; RESTORE.txt 없는 라운드 12 잔여물은 도구 미상). `doctor::tests::doctor_lists_kept_rollback_backups_as_warnings`. |
| L2 | rollback_incomplete 뒤 `tools/bin`에 새 ffmpeg.exe + 옛 ffprobe.exe가 섞여도 상태는 `installed`. | `ToolStatus.rollback_backups`(그 도구 이름의 보존 백업, `status()`가 채움). `/api/tools` 도구마다 `rollback_incomplete`(bool)·`rollback_note`(첫 백업의 안내, 없으면 null)·`rollback_backups`(경로 목록). `frametrace tools list`: `  경고: <안내>` 줄. UI 도구 패널(`assets/app/56-tools.js`): 플래그가 있으면 설치됨이어도 warn 행, `<small class="h-tool-rollback">` "이전 설치를 되돌리지 못했습니다 — 도구 파일이 섞였을 수 있습니다"(en "A previous install could not be undone — the tool's files may be mixed"), 서버 안내는 title·sr-only. 버전 불일치 탐지는 하지 않는다(지시대로). | `tool_json_flags_a_kept_rollback_backup`. tools-unit 새 행: 설치됨 + 플래그 → 패널 보임·warn·경고 줄·title·버튼 없음, 플래그 없는 도구엔 없음, 미설치 + 플래그 → 경고 + [설치], en 문구. mock 고정 데이터 4개에 `rollback_incomplete:false`·`rollback_note:null`·`rollback_backups:[]`. |
| L3 | `build-release.sh`: Python이 없고 `FRAMETRACE_SKIP_NOTICES_CHECK=1`이면 매니페스트를 보지 않고 진행; Python의 stderr는 `2>/dev/null`로 버렸다. | 두 번째 매니페스트 판독기 `node -e`(`find_node`: `node -e 0`로 확인; BOM 제거 후 `JSON.parse`, 같은 규칙 — `sha256`이 undefined·null·`""`면 건너뜀, 아니면 `notice_confirmed === true`만 통과, `tools` 배열 아님·항목이 객체 아님은 오류). 둘 다 없으면 "cannot verify — no python3/python or node to parse it (install one; FRAMETRACE_SKIP_NOTICES_CHECK does not skip this check)"로 **우회 없이** 거부. `FRAMETRACE_SKIP_NOTICES_CHECK`는 이제 zip 내용 판독기 부재에만 적용(스크립트 머리말·`source_problems` 주석·`docs/repro-build.md`에 명시). 판독기 stderr는 임시 파일로 받아 거부 줄에 `(python: JSONDecodeError(…))` / `(node: …)`로 붙인다. | `test-build-release.sh` 새 경우: python 오류 원인 표시, BOM 매니페스트(python·node) 통과; python 없이 node로 — 확인됨 통과(경고 없음)·false·키 없음·`"true"`·`1`·둘째 도구 미확인 거부·sha256 `""` 통과·JSON 아님(원인 "(node: ")·`tools` 없음(원인)·false+SKIP=1 거부·실제 매니페스트는 node 판정과 같음; python·node 둘 다 없음 → 거부, +SKIP=1도 거부·"NOT checked" 경고 없음. 기존 "no python + SKIP=1 → 통과·경고" 두 경우는 이것으로 바뀌었고, Store-python + unzip 경우는 node가 매니페스트를 읽어 통과(경고 없음). |
| L4 | 다운로드·검증 중 [설치 중단 후 서버 종료]는 이미 `/api/cancel`로 설치를 취소한다. 기다리는 동안 [계속 작업]을 누르면 서버는 그대로지만 설치는 이미 취소됐는데 대화 상자는 그 말을 하지 않았다. | `assets/app/95-quit.js` `hQuitInstallLeftNote`: 기다림 중 대화 상자가 닫히면(두 지점) `/api/cancel` 답이 `ok && cancelled:"tool-install"`이면 toast(warn) "설치는 이미 중단됐습니다 — 필요하면 다시 설치하십시오.", `not_cancellable` + `job_kind:"tool-install"`이면 "도구 설치는 중단되지 않았고 계속 진행됩니다 (파일을 옮기는 중) — 서버는 종료하지 않았습니다.", 그 밖(분석 작업)은 아무것도. | tools-unit `quitRun`에 `toast`·`opts.cancelAfterPolls`(취소가 끝나기까지 폴링 수). 새 행: download 단계·취소 수락·3폴링 뒤 닫힘 → `/api/cancel`만·toast 정확히 그 문구; 닫지 않으면 종료·toast 없음. 라운드 12 L5 행(pip, not_cancellable)에 "계속 진행됩니다" toast 단언. |
| H1 | `build-release.sh`의 `cp target/release/frametrace`가 `CARGO_TARGET_DIR`을 무시 → 변수가 설정되면 cp 실패 또는 **오래된** 바이너리 포장. | `RELEASE_DIR="${CARGO_TARGET_DIR:-target}/release"`(상대 경로는 저장소 루트 기준 — 스크립트가 루트로 `cd`, cargo와 같은 해석), `frametrace`·`frametrace-app` 둘 다. | 하네스: 가짜 cargo가 `CARGO_TARGET_DIR`이 있으면 거기에 `fresh-from-cargo`를 쓴다(`FT_CARGO_NOWRITE=1`이면 안 씀); `target/release`에는 오래된 `bin`. 절대·상대(`out/ct`, 루트 아래 생성) → 새 것 포장; 빈 디렉터리 → 실패·zip·SHA256SUMS 없음; 변수 없음 → `target/release`. `run_script`는 `env -u CARGO_TARGET_DIR`로 바깥 환경을 차단. |
| H2 | `tools_python_fingerprint`가 `.<이름>.old-*`·`*.rm`까지 넣어 청소·재설치 중의 임시 이름이 `/api/env` 캐시를 놓치게 했다. | `.`로 시작하거나 `.rm`으로 끝나는 항목 제외. | `the_env_cache_misses_when_a_private_venv_appears_or_changes`에 `.deepfake-lens.old-00ff/Scripts`·`.x.old-01.rm`·`leftover.rm`을 만들어도 지문 그대로 단언 추가. |

### API 변경 (트랙 U)
- `/api/tools` 도구마다 `rollback_incomplete`(bool), `rollback_note`(string|null, 한국어), `rollback_backups`(string[]). `frametrace tools list --json`도 같다.
- 실패한 pip 재설치도 `code: "rollback_incomplete"`를 낼 수 있다(오류 앞머리 동일). 그 경우 `tools/python/rollback-backup-<hex>/<도구>/` + `RESTORE.txt`가 남는다. archive 도구의 `tools/rollback-backup-<hex>/`에도 `RESTORE.txt`가 생긴다.
- `frametrace doctor`: 보존 백업마다 WARN `tool rollback backup (<도구>)`.

### 게이트 (이 PC, Windows 11, 기준 f207a02 + 이 라운드 커밋, CARGO_TARGET_DIR=target-fix13)
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok · `cargo test --locked` **1263 passed / 0 failed / 30 ignored**(lib 1240 + 통합 14·5·4).
- `node scripts/check-ps-bom.mjs` ok · `check-js` ok(54) · `check-css` ok · `verify-review-batch --unit` **153 PASS**(rc 0; 종전 151 — 첫 실행은 다른 작업과 겹쳐 시간 단언 `R21 U2 case names … < 50 ms`가 78.9 ms로 FAIL, 혼자 다시 돌려 통과) · `node scripts/test-make-portable.mjs` all checks passed(123 ok) · `bash scripts/test-build-release.sh` **272 ok, 0 failed, 1 skipped**(bsdtar 없음; 종전 201) · `FRAMETRACE_TEST_BSDTAR=C:/Windows/System32/tar.exe bash scripts/test-build-release.sh` **288 ok, 0 failed, 0 skipped**(종전 217).
- `FRAMETRACE_R11_STRICT=1 node scripts/verify-i18n.mjs` 770 ko / 770 en · `FRAMETRACE_R11_STRICT=1 node scripts/verify-layout.mjs` **171/171 passed**(혼자 실행, debug `frametrace-app`는 `CARGO_TARGET_DIR`, playwright는 `NODE_PATH`=frametrace/node_modules, `playwright install` 안 함).

### 확인 못 한 것 / 남은 것
- `undo_venv`가 이전 venv를 `rollback-backup-*`로 옮기지 못하는 분기(이전 venv 안의 파일도 잡힌 경우)와, 부분 venv 삭제가 "missing" 쪽으로 실패하는 분기(삭제는 됐는데 되돌리는 rename만 실패)는 실기로 돌리지 않았다(H-a의 같은 분기와 마찬가지).
- 청소 강화는 실행기 파일의 존재만 본다(실행 가능 여부·버전은 보지 않음). 부분 venv에 실행기가 남은 채 되돌리기가 실패하면 `.old-*`는 지워질 수 있지만, 이제 그 경우 이전 venv는 `.old-*`가 아니라 `rollback-backup-*`에 있다(이동 실패 분기만 예외).
- UI 경고 줄의 서버 안내(`rollback_note`)는 한국어 전용이다(en 화면에서는 en 짧은 문구 + 한국어 안내가 title).
- `95-quit.js`의 새 toast는 한국어 전용(종전처럼 en 표 없음).
- Linux/macOS에서 `build-release.sh`(node 분기 포함)·새 Rust 코드는 돌리지 못했다.
- 이전 라운드의 H1(4K/VFR h264_mf 실기)·H3(OneDrive)·소유자의 `notice_confirmed` 확인 그대로.

## §Fix round 14 (2026-10-21, Fable 확인 리뷰 반영, 기준 90873bc)

90873bc에 대한 Fable 확인 리뷰의 Low 다섯(L1–L5)과 짐작 하나(H)를 처리했다.

| # | 문제 | 수정 | 확인 |
|---|---|---|---|
| L1 | `status(tool)`이 도구마다 `rollback_backups_for_current_exe()`를 불러 보존 백업 전부를 재귀로 크기 쟀다(보존 venv는 파일 수천 개·수백 MB) → `/api/tools` 한 번에 4번. `tools install`의 프롬프트는 받은 `dirs`가 아니라 실행 파일 옆 `tools`를 봤다. | `status(tool, backups: &[RollbackBackup])` — 호출자가 한 번 구한 목록에서 그 도구 것만 고른다. `/api/tools`·`tools list`는 요청마다 `rollback_backups_for_current_exe()` 한 번, `tools install`은 설치할 `dirs.tools_dir`의 `rollback_backups()`. 프롬프트(`install_prompt_lines`)는 그 도구의 보존 백업을 `  경고: …` 줄로 보인다(설치 뒤에도 폴더는 남으므로). | `cli::handlers::tools::tests::status_and_the_prompt_use_the_given_tools_dir_backups`(고정 tools 폴더에 ffmpeg·libewf 백업 → ffmpeg status에 1개, 프롬프트에 경고 1줄·경로; 빈 목록 → 경고 없음). 기존 `status_walks_path_once…`는 `&[]`. |
| L2 | `95-quit.js` 기다림 루프 **뒤**의 닫힘 확인은 작업이 이미 끝난 뒤에만 닿는데, not_cancellable 설치에도 "계속 진행됩니다"라고 했다. | `hQuitInstallLeftNote(cr, ended)`: 루프 뒤(ended)에 not_cancellable 설치면 "도구 설치가 끝났습니다 — 도구 패널에서 결과를 확인하십시오.", 루프 안이면 종전 문구; "설치는 이미 중단됐습니다"는 `cr.ok && cancelled === "tool-install"`일 때만. 짐작(중복 toast): 도구 패널이 같은 설치를 따라가는 중(`HT.busy === cr.install.tool`)이면 종료 대화 상자의 toast는 내지 않는다 — 패널의 `htFinish`가 끝을 알린다. | tools-unit 새 행: install 단계·끝나는 폴링에 닫힘 → `/api/cancel`만·toast "도구 설치가 끝났습니다…"; 아직 도는 pip → "계속 진행됩니다"; 취소 수락 → "이미 중단됐습니다"; `HT.busy:'ffmpeg'` → 세 경우 모두 toast 없음, `HT.busy:'x-opt'` → 1개. |
| L3 | `docs/ui-rebuild-contract.md` §28 T 필드 목록에 라운드 13 필드가 없었다. | T에 `rollback_incomplete`·`rollback_note`·`rollback_backups[]`, "Fix round 13"·"Fix round 14" 줄. | 문서. |
| L4 | `RESTORE.txt` 쓰기는 best effort인데 오류 문구는 그 파일이 있다고 했다. archive 쪽 쓰기가 실패하면 `name_tool_in_restore_file`이 아무것도 안 해 백업에 도구 이름이 없고 `status()`가 붙이지 못했다(UI·CLI 경고 없음). | `name_tool_in_restore_file`은 파일이 없으면 만든다(`tool:` 줄 + 복원 안내 + "파일 목록은 설치 오류(install-audit.jsonl에도 있음)에 있다"는 줄) → `Result`. 그래도 못 쓰면 `preserve_rollback_backup`은 "… — this message lists each one (RESTORE.txt could not be written: <원인>)"(파일을 가리키지 않음). `undo_venv`도 쓰기 실패면 "move it back to <target> (RESTORE.txt could not be written: …)"(venv 백업은 하위 폴더 이름으로 도구가 붙는다). 청소·목록은 `backup/RESTORE.txt`라는 **항목이 있으면**(파일이 아니어도) 남겨 둔 staging으로 본다. | `a_missing_or_unwritable_restore_file_is_created_or_reported`: 없음 → 만들어지고 `tool: fake`·안내·끝 줄, 목록에 도구 fake, 오류가 그 경로를 가리킴; `RESTORE.txt` 디렉터리 → 오류에 "(RESTORE.txt could not be written: ", 파일을 가리키지 않음, 목록엔 도구 미상으로 나옴; 그런 staging은 청소되지 않음; `undo_venv`(target이 파일이라 지울 수 없음 + 테스트 훅 `FAIL_RESTORE_WRITE`) → 같은 문구·"move it back to"·"says how" 없음·백업은 `lens`로 목록. |
| L5 | 청소의 `venv_intact(&python.join(base), base)`는 **도구 이름**을 콘솔 스크립트로 봤지만 설치는 `provides[0]`을 확인한다 → 이름 ≠ `provides[0]`인 pip 도구는 `.old-*`가 영원히 남는다. | `clean_stale_staging(tools_dir, started, tools: &[&ToolEntry])`: `venv_script_name(base, tools)` = 그 이름의 **pip** 도구의 `provides[0]`, 목록에 없으면 base(종전). 설치(`run_platform_install`)는 설치 중인 도구를 먼저, 그다음 매니페스트 도구; 서버 시작(`clean_stale_staging_at_start` → `clean_stale_staging_if_unlocked(…, tools)`)은 매니페스트 도구. | `the_sweep_checks_the_provided_console_script`: 이름 `lens-kit`·`provides ["lens-cli"]` 고정 도구 — 목록 없이(종전) 0개, 같은 이름의 archive 도구로도 0개, 도구를 주면 1개 삭제; live venv에 `lens-kit` 스크립트만 있으면 남김. 기존 청소 테스트는 `&[]`. |
| H | `.gitattributes`에 `scripts/**` 규칙이 없어 autocrlf=true 체크아웃에서 `*.sh`·`*.mjs`가 CRLF. | `*.sh text eol=lf`, `scripts/**/*.mjs text eol=lf`(`assets/**`와 같은 방식). 인덱스는 48개 모두 이미 LF — `git add --renormalize .`의 변경은 `.gitattributes`뿐. | `git ls-files --eol`: 48개 `i/lf w/lf`(다시 체크아웃 뒤). |

### 커밋 (detached HEAD, 기준 90873bc)
- b9bb6d3 L1 · b4c8841 L4·L5 · b5b5474 L2 · dba1314 L3 · 78f0546 H · 이 문서.

### 게이트 (이 PC, Windows 11, 기준 90873bc + 이 라운드 커밋, CARGO_TARGET_DIR=target-fix14)
- `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok · `cargo test --locked` **1266 passed / 0 failed / 30 ignored**(lib 1243 + 통합 14·5·4).
- `node scripts/check-ps-bom.mjs` ok · `check-js` ok(54) · `check-css` ok · `verify-review-batch --unit` **154 PASS**(rc 0, 혼자 실행; 종전 153) · `node scripts/test-make-portable.mjs` all checks passed(123 ok) · `bash scripts/test-build-release.sh` **272 ok, 0 failed, 1 skipped**(bsdtar 없음).
- `FRAMETRACE_R11_STRICT=1 node scripts/verify-i18n.mjs` 770 ko / 770 en · `FRAMETRACE_R11_STRICT=1 node scripts/verify-layout.mjs` **171/171 passed**(혼자 실행, debug `frametrace-app`는 `CARGO_TARGET_DIR`, playwright는 frametrace/node_modules로의 junction — 커밋하지 않음, `playwright install` 안 함).

### 확인 못 한 것 / 남은 것
- RESTORE.txt 자체를 만들 수 없는데(디스크 가득 등) staging 백업을 밖으로 옮기지도 못한 경우: `backup/RESTORE.txt` 항목이 없으니 다음 청소가 그 staging을 지울 수 있다(오류 문구에는 남음). 디렉터리 이름 충돌 같은 경우는 이제 남긴다.
- 중복 toast 억제는 `HT.busy`만 본다: 패널이 이미 끝을 알리고 `HT.busy`를 비운 뒤에 종료 대화 상자가 닫힘을 알아채면 두 toast가 다 날 수 있다(드묾).
- 새 toast는 한국어 전용(종전처럼). Linux/macOS에서 새 Rust 코드는 돌리지 못했다. 이전 라운드의 남은 항목 그대로.

## §Fix round 15 (2026-10-21, Opus 확인 리뷰 반영, 기준 041bf68)
Fable 크레딧 소진으로 라운드 14 확인 리뷰는 Opus가 했다. 판정은 머지 가능(Blocker·High·Medium 없음), Low 2건.

| 발견 | 변경 | 확인 |
|---|---|---|
| L-a: RESTORE.txt 생성이 NotFound 외 오류로 실패하고 `rollback-backup-*` 이동도 실패하면, 남은 `.staging-*`에 `backup/RESTORE.txt`가 없어 다음 서버 시작·설치의 정리가 이전 파일의 유일한 사본을 지운다. 오류 문구는 "다시 설치하기 전에"만 말했다. | 정리 규칙은 그대로 둔다. 성공한 설치도 옛 바이너리가 쓰이는 중이면 `backup/`이 든 staging을 일부러 남기고, 그것은 다음 정리에서 지워져야 하므로 "`backup/`이 비어 있지 않으면 보존"으로 바꾸면 그 폴더가 영구히 남고 되돌리기 경고가 거짓으로 뜬다. 대신 RESTORE.txt를 쓰지 못한 경우의 오류 문구를 "FrameTrace를 다시 시작하거나 다른 설치를 하기 전에(둘 다 이 폴더를 정리함)"로 바꿨다. | 코드 읽기. 디스크 가득 + 이동 실패의 겹침은 실기 재현하지 않음. |
| L-b: 설치가 이미 Done/Failed인 순간의 `/api/cancel`은 아무것도 요청하지 않으면서 `ok:true, cancelled:"tool-install"`로 답해, 대기 중 [계속 작업]이면 "설치는 이미 중단됐습니다"가 떴다(실제로는 성공했을 수 있음). | 응답에 `requested`(실제로 중단을 요청했는지)를 추가. `95-quit.js`는 `requested === false`이면 "도구 설치는 이미 끝나 있었습니다 — 도구 패널에서 결과를 확인하십시오."를 낸다. mock(`apiCancelReply`)도 같은 규칙으로 맞췄다. | `serve::tests_tools::the_global_cancel_stops_a_tool_install_through_its_own_latch`에 `requested` true/false 단언 추가. tools-unit 새 행 "Fix round 15 …" PASS. |

게이트(이 PC): `cargo fmt --check` ok · `cargo clippy --locked --all-targets -- -D warnings` ok · `cargo test --lib -- tool_install serve::tests_tools` 65 passed · `verify-review-batch --unit` 155 PASS / 0 FAIL. 전체 게이트는 깨끗한 워크트리(ft-gate)에서 따로 돈다.
