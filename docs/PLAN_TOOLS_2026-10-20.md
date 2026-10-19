# 외부 도구 번들 + 안내 설치 계획 (2026-10-20, 기준 54cf3c5)

사용자 결정: **섞는다** — 필수인 FFmpeg는 배포본에 번들, 선택 도구(libewf·Sleuth Kit·deepfake-lens)는 앱 안에서 확인된 다운로드로 안내.

## 원칙
- **T1. 고정 버전·고정 해시.** 번들이든 다운로드든 도구마다 버전·URL·SHA-256을 저장소의 매니페스트(`tools/manifest.json`)에 고정한다. 해시가 다르면 설치하지 않는다(부분 파일도 남기지 않음). "latest" URL 금지.
- **T2. 허용된 출처만, HTTPS만.** 매니페스트의 호스트 허용 목록 밖으로는 내려받지 않는다. 리다이렉트도 허용 목록 안이어야 한다(FrameTrace·패키징 스크립트가 홉마다 직접 확인, curl에 `--location` 없음; `--globoff`로 URL의 `[]{}`는 글자 그대로). 인증서 오류는 그대로 실패. **프록시**: 사용자/시스템 프록시 환경 변수(`HTTPS_PROXY`·`ALL_PROXY`·`NO_PROXY`)는 회사망을 위해 **그대로 따른다** — 프록시는 허용 호스트로 가는 TLS를 중계할 뿐이고(인증서는 그 호스트의 것으로 확인), 무결성은 언제나 고정 크기·SHA-256이 결정한다(§Fix round 1).
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
