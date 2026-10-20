# 외부 도구 — 옵션 C 계획: FFmpeg 번들 중단, 안내 설치만 유지 (2026-10-20, 기준 tools-bundle e90257d, PR #5)

소유자 결정(2026-10-20): **옵션 C.**
- 고정한 BtbN `win64-lgpl` 빌드는 이름과 달리 **GPL-2.0-only(zvbi `src/packet-830.*`·`src/pdc.*`) + GPL-2.0-or-later(FFTW3) + LGPLv3 + Apache-2.0(openssl 등)** 이 섞여 있어 FrameTrace가 재배포할 수 없다(2차 Fable 공급망 리뷰 **B-1 Blocker**). 따라서 **배포 zip에 FFmpeg를 넣지 않는다.**
- 앱 안의 해시 고정 설치(`frametrace tools install ffmpeg`, 첫 화면 [설치] 버튼)는 **그대로 둔다** — 사용자가 BtbN GitHub 릴리스에서 **직접** 내려받으므로 FrameTrace는 배포자가 아니다.
- 자체 빌드한 진짜 LGPL FFmpeg 번들(옵션 A)은 **별도 PR**(부록 A).

리뷰 항목 대응: B-1(재배포 불가) → 배포하지 않음으로 해소. B-2(3년 서면 소스 제공) → 배포가 없으므로 **삭제**. B-3(“LGPL 빌드” 문구) → 매니페스트·UI·doctor·README 전부 고침. B-6(CI buildconf 게이트) → 번들이 없으므로 **설치 경로 스모크**(CI가 실제 BtbN 자산으로 `tools install ffmpeg`를 돌려 검증만, 산출물로 올리지 않음)로 옮긴다.

이 문서는 Opus 구현자용이다. 각 행에 **[Opus]**(판단·코드 구조·문구 결정) 또는 **[Sonnet]**(기계적: 문자열·삭제·이동·테스트 문자열 갱신, 본문에 적힌 텍스트를 그대로 적용)을 표시했다. Sonnet 행도 Opus가 diff를 읽고 게이트를 돌린 뒤 커밋한다.

---

## 0. 원칙 재작성 (`docs/PLAN_TOOLS_2026-10-20.md` 원칙 절에 반영, 행 M5)

T1–T4는 그대로. T5·T6을 아래로 바꾼다.

- **T5. 라이선스 고지 — FrameTrace는 외부 도구를 배포하지 않는다.** 배포 zip(`scripts/make-portable.ps1`)에는 FrameTrace 자신의 실행 파일만 들어가고 `ffmpeg.exe`·`ffprobe.exe`는 절대 넣지 않는다. 도구 설치는 사용자가 매니페스트에 고정된 **공식 출처에서 직접** 내려받는 것이며, 앱은 내려받기 **전에** 파일·출처 호스트·크기·라이선스 구성·“FrameTrace가 배포하지 않음”(`notice`)을 보여 준다. 설치된 도구의 라이선스 원문은 `tools/bin/licenses/`에 복사한다(LGPLv3 `LICENSE.txt` + 저장소에 해시 고정한 GPLv3 원문). `THIRD_PARTY_NOTICES.md`는 FrameTrace가 **실제로 배포하는 것**에 대한 고지만 담고, 사용자가 설치하는 도구는 “배포하지 않음” 절로 안내한다. 자체 빌드 LGPL FFmpeg 번들은 옵션 A(별도 PR) 전까지 없다.
- **T6. 오프라인에서도 동작.** FFmpeg가 설치돼 있으면(tools/bin 또는 PATH) 기본 기능은 인터넷 없이 동작한다. 없으면 doctor·첫 화면이 설치 안내만 하고 앱을 막지 않는다. 다운로드 실패도 기능 안내만 한다.

---

## 1. 트랙 P — 패키징·CI

| ID | 담당 | 내용 | 확인 |
|---|---|---|---|
| P1 | [Opus] | `scripts/make-portable.ps1`을 **유일한 패키지 스크립트**로 유지하되 `scripts/package-windows.ps1`에서 검증된 부분을 **먼저 옮긴다**(P2 삭제 전에): (a) `Get-Sha256Hex`(.NET SHA-256; `Get-FileHash`는 pwsh 7 부모 아래 5.1에서 자동 로드 실패 — 3c076a0) (b) zip 쓰기를 `Compress-Archive` → `[IO.Compression.ZipFile]::CreateFromDirectory` + `UseBackslash` 스위치 off + 쓴 뒤 항목 이름 `\` 검사·`Test-SafeEntry` (c) `-SkipBuild`·`-Version` 매개변수(CI는 Release build 단계 뒤 `-SkipBuild`). 파일 머리의 BOM 유지(`scripts/check-ps-bom.mjs`). | P5 오프라인 테스트, CI “PowerShell scripts have UTF-8 BOM” |
| P2 | [Opus] | `make-portable.ps1`의 **FFmpeg 배제 가드**: `tools/bin`(또는 `target/release/tools/bin`)을 복사할 때 `ffmpeg*.exe`·`ffprobe*.exe`·`licenses/ffmpeg-*`는 **복사하지 않고** `Write-Warning "ffmpeg/ffprobe are never packaged (docs/PLAN_TOOLS_C_2026-10-20.md T5) — skipped: <경로>"`. zip을 쓴 뒤 항목 이름에 `ffmpeg.exe`/`ffprobe.exe`가 하나라도 있으면 zip을 지우고 `throw`. `README-tools.txt` 3번째 줄을 `- ffmpeg.exe / ffprobe.exe — 패키지에 포함되지 않음. frametrace.exe tools install ffmpeg (또는 첫 화면의 [설치])로 BtbN에서 직접 내려받거나 PATH에 두십시오.`로. 스테이지에 `LICENSE`·`THIRD_PARTY_NOTICES.md`도 복사(지금은 README만). | P5 |
| P3 | [Sonnet] | 삭제: `scripts/package-windows.ps1`, `scripts/test-package-windows.mjs`(둘 다 BtbN 자산을 받아 zip에 넣는 전제). git 이력에 남으므로 옵션 A에서 되살린다(부록 A4). 저장소 전체에서 두 파일 이름 참조 제거: `.github/workflows/windows-ci.yml`(P4), `README.md`(D1), `tools/manifest.json` `bundled_licenses_note`(M1), `src/tool_install/mod.rs` `package_licenses` 주석(L4), `docs/security-review.md`(D6). `grep -rn "package-windows\|test-package-windows" --exclude-dir=node_modules --exclude-dir=target* .`가 `docs/PLAN_TOOLS_2026-10-20.md`(이력)와 이 문서만 남겨야 한다. | grep 결과 |
| P4 | [Opus] | `windows-ci.yml`: (a) 삭제 — “Packaging script offline test”, “Bundled package (package-windows.ps1 …)”, “Bundled FFmpeg encoder pick”, “Upload bundled package” 4단계와 그 주석. (b) “Portable package smoke” 단계를 `-SkipBuild`로 바꾸고 검사 추가: zip 항목 목록에 `ffmpeg.exe`·`ffprobe.exe` 없음, 모든 항목 `/`, `FrameTrace-<ver>-win64/THIRD_PARTY_NOTICES.md` 있음. (c) 새 “Portable package offline test” 단계 = `node scripts/test-make-portable.mjs`(P5). (d) 새 “Upload portable package” 아티팩트 `FrameTrace-win64`: `dist/FrameTrace-*-win64.zip` + `dist/SHA256SUMS`(지금은 번들만 올렸고 포터블은 안 올렸다). (e) 설치 경로 스모크(P6)는 **업로드 뒤**, 별도 임시 폴더에서. | CI 통과 |
| P5 | [Sonnet] | 새 `scripts/test-make-portable.mjs`(Windows 전용, 아니면 SKIPPED 0 종료): 삭제되는 `test-package-windows.mjs`의 `zipNames`·`fakeRepo` 뼈대를 재사용. 가짜 저장소 루트에 `target/release/{frametrace,frametrace-app}.exe`(가짜 MZ), `README.md`·`LICENSE`·`THIRD_PARTY_NOTICES.md`·`docs/WINDOWS_USAGE.md`·`scripts/install.ps1`, 그리고 **일부러 심은** `tools/bin/ffmpeg.exe`·`tools/bin/ffprobe.exe`·`tools/bin/licenses/ffmpeg-LICENSE.txt`·`tools/bin/mmls.exe`를 두고 `powershell -NoProfile -ExecutionPolicy Bypass -File <root>/scripts/make-portable.ps1 -SkipBuild -Version 0.0.0-test`를 돌린다(환경: 프록시 변수 제거, `PSModulePath` 제거 — 종전 테스트와 같음). 검사: 종료 0; `dist/FrameTrace-0.0.0-test-win64.zip` 존재; 항목에 `ffmpeg.exe`·`ffprobe.exe`·`licenses/ffmpeg-LICENSE.txt` **없음**, `tools/bin/mmls.exe`·`THIRD_PARTY_NOTICES.md`·`tools/bin/README-tools.txt` **있음**; 모든 항목 `/`; `SHA256SUMS` = zip 해시 한 줄(LF, BOM 없음); 출력에 “never packaged” 경고. | `node scripts/test-make-portable.mjs` |
| P6 | [Opus] | CI **설치 경로 스모크**(검증만, 배포 아님): 새 단계 “Install-path smoke: tools install ffmpeg (real BtbN asset, verification only)”, `timeout-minutes: 20`, `shell: pwsh`. `dist/FrameTrace-<ver>-win64/`를 `$env:RUNNER_TEMP/ft-install-smoke/`로 **복사**(dist는 건드리지 않음 — 아티팩트에 ffmpeg가 섞이지 않게), `$env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"`(choco ffmpeg 차단), `& <copy>/frametrace.exe tools install ffmpeg --yes` → 종료 0. 검사: `<copy>/tools/bin/ffmpeg.exe`·`ffprobe.exe`·`licenses/ffmpeg-LICENSE.txt`·`licenses/ffmpeg-COPYING.GPLv3.txt` 존재, `COPYING.GPLv3.txt` SHA-256 = 매니페스트 핀, `<copy>/tools/install-audit.jsonl` 마지막 줄 `"result":"ok"` + `"sha256":"4a7642b2…"`, `<copy>/tools/.staging-*` 없음. 그 다음 `tools list --json`: `paths.ffmpeg`가 `<copy>/tools/bin/ffmpeg.exe`, `encoders.probed` true, `encoders.h264` ≠ `libopenh264`(종전 게이트 유지), `encoders.trial_failed`가 `[]`. **B-6 buildconf 기록**: `ffmpeg -hide_banner -buildconf`에서 `--enable-version3` 있음, `--enable-gpl`·`--enable-nonfree` **없음**(둘 중 하나라도 있으면 실패 — 고정 해시가 바뀌는 PR에서 혼합이 더 나빠지는 걸 막는 가드), `libopenh264`·`chromaprint`·`libzvbi` 줄 출력. `-encoders`의 `(codec h264)` 줄 출력. 결과를 `$RUNNER_TEMP/ffmpeg-install-smoke.txt`(h264 pick·buildconf 요약·audit 줄)로 써서 아티팩트 `ffmpeg-install-smoke`로 **텍스트만** 올린다. 이것이 계획 “남은 일”의 **첫 실기 확인**이다(실제 zip으로의 설치는 이 세션까지 한 번도 돌지 않았다). | CI 통과, 아티팩트 내용 |
| P7 | [Sonnet] | 문서: `docs/WINDOWS_USAGE.md` “Distribution” 절의 `Drop ffmpeg.exe/ffprobe.exe into tools/bin too for a fully turnkey bundle.` → `The package never contains ffmpeg/ffprobe (they are not redistributable by us): run `frametrace.exe tools install ffmpeg` after unzipping (or use the home screen's install button), or keep them on PATH.`; “FFmpeg Notes” 마지막 문단(`If a future GUI bundles FFmpeg…`) → `FrameTrace does not bundle FFmpeg. Every proxy/export audit line records the resolved binary's encoders (`video_encoder`, `audio_encoder`) and the exact arguments; `frametrace doctor` shows which ffmpeg is used and whether another copy on PATH is ignored.` `docs/repro-build.md`에 한 줄: `The Windows zip contains no FFmpeg; it is installed by the user from the pinned BtbN release (tools/manifest.json).` | 읽기 |

---

## 2. 트랙 L — 고지·라이선스 파일

결정 요약: **배포하는 것이 없으니 의무 고지도 없다.** 남기는 것은 (1) 사용자가 설치하는 FFmpeg의 성격을 설명하는 정보 절, (2) 설치 시 옆에 복사하는 라이선스 원문(LGPLv3 + GPLv3 — 유지), (3) 선택 도구 표. 삭제하는 것은 서면 소스 제공, “포함/불포함 범위”, 정적 라이브러리 표(정보 문서로 이동), `ffmpeg-THIRD-PARTY-LICENSES.txt`(1.4 MB, 패키징 전용이었음).

| ID | 담당 | 내용 | 확인 |
|---|---|---|---|
| L1 | [Opus] | `THIRD_PARTY_NOTICES.md` 전면 교체. 아래 초안을 기준으로 쓰되 법적 단정은 피하고(“we believe”류 없이 사실만), 소유자 확인 표시를 남긴다. 구조: ① 머리(FrameTrace MIT; **이 저장소와 Windows zip은 제3자 바이너리를 하나도 배포하지 않는다**; 도구는 사용자가 설치) ② “FFmpeg — not distributed by FrameTrace; installed by you from BtbN” ③ “Optional tools” 표(그대로) ④ 부록 포인터(옵션 A 때 번들 고지가 다시 생긴다). | 읽기, D6 |
| L2 | [Sonnet] | L1의 일부: “Written offer for the complete corresponding source” 절, “What is and is not covered” 절, “Statically linked libraries” 표·“Notices” 절, `ffmpeg-SOURCE.txt`·`ffmpeg-THIRD-PARTY-LICENSES.txt` 언급을 모두 제거. 저장소 어디에도 “source offer / written offer / three years / 3년 서면” 문구가 남지 않게(`grep -rni "written offer\|source offer\|three years\|서면 소스"` → 이력 문서(`PLAN_TOOLS_2026-10-20.md`, `security-review.md` 과거 행)만). | grep |
| L3 | [Sonnet] | 정적 라이브러리 표(THIRD_PARTY_NOTICES.md 68–160행)를 **그대로** 새 파일 `docs/ffmpeg-btbn-components-autobuild-2026-09-30.md`로 옮긴다. 머리말(영문): `Informational only. FrameTrace does not distribute this binary. This table lists what the BtbN FFmpeg-Builds tag autobuild-2026-09-30-13-08 (win64, variant lgpl, addin 8.1) statically links, derived from its scripts.d selection rules — not read out of the binary. It exists so an examiner who installs that build with `frametrace tools install ffmpeg` can see which licences the file carries. The Windows CI install-path smoke records the real `-buildconf` for comparison. Kept for the option-A (self-built LGPL FFmpeg) plan, docs/PLAN_TOOLS_C_2026-10-20.md appendix A.` THIRD_PARTY_NOTICES의 FFmpeg 절에서 이 파일로 링크. | 읽기 |
| L4 | [Opus] | `package_licenses` 제거: `tools/licenses/ffmpeg-THIRD-PARTY-LICENSES.txt` 삭제, `tools/manifest.json`의 `package_licenses` 키 삭제, `ToolEntry.package_licenses` 필드·`validate_manifest`의 해당 블록(mod.rs 320–331)·테스트 `package_only_license_files_exist_with_their_pinned_hash`(tests.rs 441–469) 삭제, `tool_install/tests.rs` 36행의 `package_licenses: Vec::new()` 제거. `bundled_licenses`(GPLv3)는 **유지**(아래 L5). `.gitattributes`의 `tools/licenses/** -text`는 유지(GPLv3 핀). | `cargo test tool_install` |
| L5 | [결정, 코드 변경 없음] | 설치 시 `tools/bin/licenses/`에 `ffmpeg-LICENSE.txt`(zip의 COPYING.LGPLv3)와 `ffmpeg-COPYING.GPLv3.txt`(저장소 핀)를 복사하는 동작은 **그대로 둔다**. 이유: 바이너리를 배포하는 것이 아니라 사용자가 받은 파일 옆에 그 파일의 라이선스 원문을 놓아 주는 것이고(LGPLv3는 GPLv3를 참조로 포함), 사용자가 나중에 그 폴더를 누군가에게 넘길 때도 도움이 된다. Windows 테스트 `installs_the_pinned_gplv3_text_next_to_the_archive_license`·`shipped_ffmpeg_entry_installs_the_pinned_gplv3_text`·`bundled_license_must_match_its_pin_and_be_compiled_in`은 그대로 통과해야 한다. | 기존 테스트 |

### L1 초안 (영문, `THIRD_PARTY_NOTICES.md`)

```
# Third-party notices

FrameTrace itself is MIT-licensed (see `LICENSE`). Neither this repository nor
the Windows package built by `scripts/make-portable.ps1` contains any
third-party executable: no FFmpeg, no libewf, no Sleuth Kit, no deepfake-lens.
FrameTrace runs those tools as separate programs when they are present in
`tools/bin` or on `PATH`; it does not link them, so none of their licenses
applies to FrameTrace's own code.

## FFmpeg — not distributed by FrameTrace; installed by you from BtbN

`frametrace tools install ffmpeg` (and the home screen's install button)
downloads one pinned file **directly from the BtbN FFmpeg-Builds GitHub
release to your machine** and verifies it against the SHA-256 published with
that release (`tools/manifest.json`: version `n8.1.3-9-g29e619e767`, release
`autobuild-2026-09-30-13-08`, variant `win64-lgpl-8.1`, size 170 611 883 bytes,
SHA-256 `4a7642b2264c03e8a0ce8a3825b933ee5580656f45695a086fe7e294045ffc0a`).
Nothing is downloaded until you confirm the file, source, size and the notice
below. FrameTrace is not the distributor of that file: the FrameTrace
maintainers make no offer of source for it and grant no patent licence of any
kind.

What the file is: FFmpeg's own code under the GNU LGPL v3 or later
(`--enable-version3`), statically linked with many libraries. **Despite the
variant name "lgpl", the binary as a whole is not LGPL-only**: it contains
libzvbi parts under GPL-2.0-only (`src/packet-830.*`, `src/pdc.*`), FFTW3 under
GPL-2.0-or-later (via chromaprint), OpenSSL and others under Apache-2.0, and a
libopenh264 compiled from source (Cisco's H.264 patent licence covers only the
binaries Cisco distributes). The component list derived from BtbN's build
scripts is in `docs/ffmpeg-btbn-components-autobuild-2026-09-30.md`
(informational). Using the file on your own computer is not restricted by any
of these licenses. If **you** pass the file on to someone else (for example by
copying a FrameTrace folder that contains `tools/bin/ffmpeg.exe`), the GPL's
conditions — including making the complete corresponding source available —
fall on you; the sources are at the upstream locations named in the manifest
and in that document. (Owner to confirm this paragraph.)

FrameTrace copies two license texts next to the installed binaries:
`tools/bin/licenses/ffmpeg-LICENSE.txt` (the `COPYING.LGPLv3` shipped in the
BtbN zip) and `tools/bin/licenses/ffmpeg-COPYING.GPLv3.txt` (the GNU GPL v3
the LGPLv3 incorporates by reference; FFmpeg's own `COPYING.GPLv3` at commit
`29e619e767cde9045a75c29bc9a8278ae7b3a98b`, SHA-256
`8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903`, pinned in
the manifest). Each install attempt is recorded in `tools/install-audit.jsonl`
(URL, size, SHA-256, result).

Encoders: that build has no libx264. FrameTrace encodes H.264 with the best
encoder the resolved ffmpeg really has, in the order `libx264` → `h264_mf`
(Windows Media Foundation, licensed with Windows) → `libopenh264` → `mpeg4`,
and records the pick in every proxy / export audit line. You may use any other
FFmpeg build instead: put `ffmpeg.exe` / `ffprobe.exe` in `tools/bin` or on
`PATH`.

## Optional tools (not distributed; installed by the examiner)

(표 그대로)
```

---

## 3. 트랙 M — 매니페스트·서버·CLI·UI 고지

| ID | 담당 | 내용 | 확인 |
|---|---|---|---|
| M1 | [Sonnet] | `tools/manifest.json` ffmpeg 항목 문자열 교체(아래 M1 값). `note`(최상위)의 계획 포인터를 `docs/PLAN_TOOLS_2026-10-20.md (T1-T4), docs/PLAN_TOOLS_C_2026-10-20.md (T5-T6: nothing is bundled; tools are installed by the user from the pinned source)`로. `package_licenses` 키 삭제(L4). `bundled_licenses_note` 끝 문장 `ffmpeg-THIRD-PARTY-LICENSES.txt … package-windows.ps1 only.` 삭제. JSON은 `cargo test tool_install::tests::shipped_manifest_is_valid…`가 파싱·검증한다. | T1 |
| M2 | [Opus] | `src/tool_install/mod.rs`: `ToolEntry`에 `#[serde(default)] pub notice: Option<String>`, `#[serde(default)] pub notice_en: Option<String>`. `validate_manifest`: **`sha256`가 Some(설치 가능)인 archive 도구는 `notice`가 비어 있지 않아야 한다**(없으면 `"<name>: installable tools must carry a distribution notice (T5)"`) — pip 도구는 요구하지 않음(지금 설치 불가). `tool_json`에 `"notice": tool.notice, "notice_en": tool.notice_en` 추가(최상위 T, `download` 안이 아님 — 설치 전 줄·대화상자 둘 다 쓰므로). 모듈 머리 T5 줄을 `T5 — nothing is bundled; an installable tool carries a distribution notice (shown before download) and its license files land in tools/bin/licenses.`로. | T2·T3 |
| M3 | [Sonnet] | `src/cli/handlers/tools.rs` `tools_install_in`: `라이선스:` 줄 뒤에 `if let Some(n) = &tool.notice { println!("고지: {n}"); }`. 프롬프트 출력을 테스트할 수 있게 **문자열 조립을 `fn install_prompt_lines(tool, dirs, status) -> Vec<String>`로 뽑고** `println!`은 그 줄을 찍기만 하도록 바꾼다(Opus가 서명 확정; `status`는 PATH 참고 줄용). | T4 |
| M4 | [Opus] | UI `assets/app/56-tools.js`(트랙 U 파일, `docs/ui-rebuild-contract.md` §28 규칙 준수 — 짧은 문구·두 테마·스크린리더): `htConfirmHtml`의 `<dl>` 뒤에 `<p class="h-tool-notice">{notice}</p>`(locale `en`이면 `notice_en || notice`, 없으면 문단 생략). 기존 `confirmNote`("확인을 누르면 내려받습니다. 해시가 다르면 설치하지 않습니다.")는 유지. CSS `56-tools.css`에 `.h-tool-notice { font-size: var(--fs-small); color: var(--fg-muted); margin-top: ... }`(기존 토큰만, 새 토큰 금지; `scripts/check-css.mjs` 통과). 대화상자 OK 라벨은 그대로 “내려받아 설치”. | T6, `verify-layout` tools 구간, `check-css` |
| M5 | [Sonnet] | `docs/PLAN_TOOLS_2026-10-20.md`: 원칙 T5·T6을 §0의 문장으로 교체하고 문서 끝에 `## §옵션 C (2026-10-20)` 절을 추가: 결정 3줄 + “구현 계획·행 목록은 docs/PLAN_TOOLS_C_2026-10-20.md” + “§S5·§Fix round 1 #2·#3의 패키징·소스 제공 부분은 이 결정으로 폐기(이력)”. 다른 절은 손대지 않는다(이력). | 읽기 |
| M6 | [Sonnet] | `docs/ui-rebuild-contract.md` §28: 1070행 T 필드 목록에 `notice(한국어 배포 고지; 설치 가능 도구는 항상 있음), notice_en` 추가; 1071행 `label`은 서버 값("FFmpeg (LGPL build)")` → `("FFmpeg")`; 대화상자 설명(“확인 대화상자 … 파일·출처·크기·라이선스”가 있는 줄)에 “+ 고지 문단(`notice`/영문은 `notice_en`)” 추가. `scripts/lib/tools-mock.mjs` 픽스처: ffmpeg `title`/`label` `'FFmpeg'`, `license` 새 값, `manual_hint` 새 값, `notice`/`notice_en` 추가(다른 세 도구는 `notice: null, notice_en: null`), `LGPL_ENCODERS.note` 새 값(E2), 상수 이름은 그대로 둬도 됨. | T6·T7 |

### M1 값

```json
"title": "FFmpeg",
"license": "LGPL-3.0-or-later (FFmpeg); the BtbN win64-lgpl binary also contains GPL-2.0-only (zvbi), GPL-2.0-or-later (FFTW3), Apache-2.0 and BSD/MIT components",
"notice": "이 파일은 BtbN FFmpeg-Builds(github.com)에서 사용자가 직접 내려받습니다. FrameTrace는 이 바이너리를 배포하지 않으며 소스 제공·특허 사용권도 제공하지 않습니다. 빌드 이름은 lgpl이지만 GPL-2.0-only 구성 요소(zvbi)가 들어 있어 파일 전체는 GPL 조건을 따릅니다 — 내 PC에서 쓰는 데는 제한이 없고, 이 파일을 다른 사람에게 복사해 주면 GPL 의무(소스 제공 등)는 복사하는 사람에게 있습니다.",
"notice_en": "You download this file yourself from BtbN FFmpeg-Builds (github.com). FrameTrace does not distribute it, offers no source for it and grants no patent licence. Although the build is named lgpl, it contains GPL-2.0-only components (zvbi), so the file as a whole is under GPL terms: using it on your own PC is unrestricted; if you pass the file on to others, the GPL obligations (source availability etc.) are yours.",
"manual_hint": "https://github.com/BtbN/FFmpeg-Builds/releases 또는 https://ffmpeg.org/download.html 에서 Windows 빌드를 받아 ffmpeg.exe·ffprobe.exe를 tools/bin에 복사하십시오."
```

`url`·`sha256`·`size`·`files`·`license_files`·`bundled_licenses`·`checksum_*`·`layout_source`는 **바꾸지 않는다**(사용자가 받는 자산은 같다).

---

## 4. 트랙 E — 인코더 선호 순서

**결정: `libx264 → h264_mf → libopenh264 → mpeg4` 유지. 코드 변경 없음, 주석·문구만.**

이유(문서에 그대로 적을 것):
- 순서는 라이선스 문제가 아니라 “사용자가 가진 빌드에서 가장 좋은 H.264”를 고르는 규칙이다. 사용자가 full 빌드(choco 등)를 쓰면 libx264가 가장 좋고 GPL 빌드를 **실행**하는 것은 어떤 조건도 붙지 않는다.
- BtbN 빌드에서 h264_mf를 libopenh264 앞에 두는 이유도 그대로다: 소스 빌드 openh264는 Cisco 특허 사용권 밖이라 **사용자의** 사용에도 특허 질문이 남고, Media Foundation 인코더는 Windows와 함께 사용권이 있다. FrameTrace가 배포자가 아니어도 사용자를 위해 안전한 쪽을 먼저 고르는 것이 맞다.
- libopenh264는 h264_mf가 없는 PC(N/KN·Server Core)의 대안으로 남긴다(B-5 시험 인코딩 유지). mpeg4는 최후 수단.

| ID | 담당 | 내용 | 확인 |
|---|---|---|---|
| E1 | [Sonnet] | `src/ffmpeg_encoders.rs` 모듈 주석 3–14행: “The bundled FFmpeg is an LGPL build (FrameTrace is MIT)” → `FrameTrace bundles no FFmpeg. The build the guided installer fetches (BtbN win64-lgpl) has no libx264 (GPL), and the examiner may use any other build, so proxies and MP4 exports never hard-code an encoder: …`; “the pinned BtbN LGPL build links openh264 built from source” → `the BtbN build the installer fetches links openh264 built from source`. 536행 fixture 주석 `(libopenh264 built in` 그대로. 테스트 이름 `btbn_lgpl_bundle_never_picks_libopenh264` → `btbn_lgpl_build_never_picks_libopenh264`(`docs/security-review.md` 78행의 이름도). `src/artifacts.rs` 92행·`src/cli/handlers/doctor.rs` 596행 주석 “The bundled LGPL …” → `A build without libx264 (e.g. the BtbN build the installer fetches): …`. | clippy |
| E2 | [Sonnet] | `build_note`(ffmpeg_encoders.rs 219행) `"LGPL 빌드 — H.264는 {} 사용"` → `"libx264 없음 — H.264는 {} 사용"`(B-3: 빌드의 라이선스가 아니라 능력을 말한다). 같은 문자열을 쓰는 곳 모두: ffmpeg_encoders.rs 테스트 660·720·777행, `scripts/lib/tools-mock.mjs` 68행 `LGPL_ENCODERS.note`, `README.md` 107행 예시, `docs/PLAN_TOOLS_2026-10-20.md`는 이력이므로 **그대로**. `grep -rn "LGPL 빌드" src scripts assets README.md` → 0. | `cargo test ffmpeg_encoders`, grep |

---

## 5. 트랙 D — README·doctor·보안 리뷰 문구

| ID | 담당 | 내용 | 확인 |
|---|---|---|---|
| D1 | [Sonnet] | `README.md` 95–107행 절을 아래 D1 초안으로 교체. 93행 `Install Rust and FFmpeg first. …` 뒤에 ` (or run `frametrace tools install ffmpeg` after building — see below)` 추가. 105행의 틀린 문장 `PATH is still searched before `tools\bin`.`은 초안에서 이미 고쳤다(B-7 이후 매니페스트 도구는 tools\bin 우선). | 읽기, grep “bundled” in README → 0 |
| D2 | [Opus] | `frametrace doctor`: 코드 변경 없음 — ffmpeg 줄은 `build_note`(E2)로 바뀐 문구를 쓴다. 미설치 줄의 `설치: frametrace tools install ffmpeg`는 유지. doctor 테스트가 “LGPL 빌드”를 기대하면 E2에서 같이 고친다(`grep -rn "LGPL" src/cli` → 0 확인). | `cargo test doctor` |
| D3 | [Sonnet] | `docs/security-review.md`: 30행 “Residual: …” 문장을 `Residual (resolved 2026-10-20, option C — docs/PLAN_TOOLS_C_2026-10-20.md): the bundle is gone; see the next row.`로; 새 행(Medium) 추가: `Tools option C (owner decision after the round-2 licensing findings B-1/B-2/B-3/B-6): the pinned BtbN win64-lgpl binary mixes GPL-2.0-only (zvbi), GPL-2.0-or-later (FFTW3), LGPLv3 and Apache-2.0 and is not redistributable by us. | Fixed: FrameTrace ships no FFmpeg (make-portable.ps1 excludes ffmpeg/ffprobe and the CI checks the zip; package-windows.ps1 deleted); the guided install downloads the pinned asset directly from BtbN and shows a distribution notice (manifest `notice`, required by validate_manifest for installable tools) in the CLI prompt and the UI dialog before anything is fetched; the written source offer and the packaged third-party licence bundle are removed; the CI now runs the real install path once (`tools install ffmpeg --yes` in a temp copy, PATH reduced) and records `-buildconf` / the H.264 pick as a text artifact — no binary is uploaded. A self-built LGPL FFmpeg bundle is a separate PR (plan appendix A). | Security Owner`. 78행 테스트 이름 갱신(E1), “packaging script offline test” → “make-portable offline test (ffmpeg never packaged)”. | 읽기 |
| D4 | [Sonnet] | `docs/HANDOFF-2026-10-11.md` 남은 일/체크리스트에 한 줄: `- [ ] 첫 실기: 포터블 zip을 풀고 `frametrace.exe tools install ffmpeg` → doctor의 ffmpeg 줄이 "libx264 없음 — H.264는 h264_mf 사용"인지, 프록시·MP4 내보내기가 h264_mf로 실제 인코딩되는지(CI 스모크는 설치·probe까지만 본다).` | 읽기 |

### D1 초안 (영문, README “External tools” 절)

```
### External tools (guided installs; nothing is bundled)

The Windows zip (`scripts/make-portable.ps1`, also a Windows CI artifact)
contains FrameTrace only — **no FFmpeg**. FrameTrace needs `ffmpeg.exe` /
`ffprobe.exe` in `tools\bin` next to the executable or on `PATH`, and it can
fetch a pinned build for you when you ask:

```powershell
.\frametrace.exe tools list                # what is installed, what can be installed here
.\frametrace.exe tools install ffmpeg      # shows file, source, size, SHA-256, licence and notice, then asks
.\frametrace.exe tools install ffmpeg --yes
```

Every tool is pinned in `tools/manifest.json` (version, URL, size and the
publisher's SHA-256, compiled into the binary). The download goes **directly
from the publisher to your machine** — for FFmpeg that is the BtbN
FFmpeg-Builds GitHub release — and FrameTrace is not its distributor; the
confirm step shows a notice saying so, together with the build's licence mix
(the `win64-lgpl` variant still contains GPL-2.0-only components; see
`THIRD_PARTY_NOTICES.md`). Downloads use Windows' own `curl.exe` (HTTPS only,
every redirect checked against the manifest's host allowlist) and `tar.exe`; a
size or hash mismatch, or an archive entry that is absolute, climbs with `..`
or is a link, refuses the install and leaves nothing behind. Proxies:
`curl.exe` honours `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`; integrity never
depends on the network path — the file must match the pinned size and SHA-256.
Each attempt (including a refused one) appends one hash-chained line to
`tools\install-audit.jsonl`; a cancel is honoured while downloading, verifying
or extracting and refused (`not_cancellable`) once files are being moved into
place. Tools without a published checksum (libewf, The Sleuth Kit,
deepfake-lens today) show a manual-install hint instead. Binaries the manifest
provides (ffmpeg, ffprobe, the libewf and Sleuth Kit tools) resolve from
`tools\bin` first, then `PATH`; a different copy on `PATH` is reported as
ignored by `doctor` and the home screen. Licence texts of an installed tool are
copied to `tools\bin\licenses\`.

The BtbN build has no libx264 (GPL). FrameTrace asks the resolved
`ffmpeg -encoders` once, trial-encodes one frame, and encodes proxies and MP4
clips with the best H.264 encoder that build really has — `libx264` →
`h264_mf` (Windows Media Foundation) → `libopenh264` → `mpeg4` as a last resort
(Media Foundation comes before openh264 because BtbN's openh264 is compiled
from source, which Cisco's patent licence does not cover) — with `aac` audio
(AVI clips: `libmp3lame`, else native `mp2`). The chosen encoders are written
to each proxy/export audit line (`video_encoder`, `audio_encoder`), and
`frametrace doctor` / `GET /api/tools` say when the build lacks libx264
("libx264 없음 — H.264는 h264_mf 사용"). Output names and containers (`.mp4`,
`.avi`) do not change.
```

---

## 6. 트랙 T — 테스트

### 폐기(삭제)
| 무엇 | 이유 | 행 |
|---|---|---|
| `scripts/test-package-windows.mjs` 전체 | 패키징 스크립트 삭제 | P3 |
| `tool_install::tests::package_only_license_files_exist_with_their_pinned_hash` | `package_licenses` 삭제 | L4 |
| windows-ci “Packaging script offline test”·“Bundled package”·“Bundled FFmpeg encoder pick”·“Upload bundled package” | 번들 없음 | P4 |

### 갱신(문자열·기대값)
| 테스트 | 바꿀 것 | 담당 |
|---|---|---|
| `tool_install::tests::shipped_manifest_is_valid_and_pins_only_published_checksums`(tests.rs 63–100) | `contains("-lgpl-")` 단언은 유지하되 메시지 `"LGPL build only (T5)"` → `"the pinned BtbN asset is the win64-lgpl variant (same file the examiner downloads)"`; 추가 단언 `assert!(ffmpeg.notice.as_deref().is_some_and(|n| n.contains("배포하지 않")))`, `ffmpeg.notice_en` Some, `assert_eq!(ffmpeg.title, "FFmpeg")`, `assert!(ffmpeg.package_licenses…)` 류 제거 | [Sonnet] |
| `serve::tests_tools` 74행 `assert_eq!(ffmpeg["license"], "LGPL-3.0-or-later")` | 새 license 문자열의 접두 `starts_with("LGPL-3.0-or-later (FFmpeg)")` + `ffmpeg["notice"]`가 문자열이고 비어 있지 않음, `ffmpeg["notice_en"]` 문자열 | [Sonnet] |
| `ffmpeg_encoders::tests` 660·720·777행 | 새 note 문자열(E2) | [Sonnet] |
| `scripts/lib/tools-unit.mjs` 79·103·189·259·306·318행, `scripts/verify-layout.mjs` 797·820·823·840행 | `FFmpeg (LGPL build)` → `FFmpeg`; 103행·797행 대화상자 본문에 `d.html.includes('배포하지 않')`(ko) 추가, 영어 로케일 행(306행 근처)이 대화상자를 열면 `notice_en` 문구 확인 | [Sonnet] |
| `scripts/lib/tools-mock.mjs` | M6 | [Sonnet] |
| `docs/security-review.md` 78행 테스트 이름 | E1 | [Sonnet] |

### 추가
| ID | 테스트 | 내용 | 담당 |
|---|---|---|---|
| T1 | `tool_install::tests::installable_tools_must_carry_a_distribution_notice` | `archive_entry(...)`(sha256 Some)에서 `notice: None` → `validate_manifest` Err에 `"distribution notice"` 포함; `notice: Some("")`도 Err; `sha256: None`(libewf류)은 notice 없어도 Ok | [Opus] |
| T2 | `tool_install::tests::tool_json_carries_the_distribution_notice` | `tool_json`이 `notice`/`notice_en`을 최상위에 내보내고 `download` 안에는 없음; notice 없는 도구는 `null` | [Sonnet] |
| T3 | `shipped_manifest_…` 확장(위) | — | [Sonnet] |
| T4 | `cli::handlers::tools::tests::install_prompt_shows_the_distribution_notice` | `install_prompt_lines(ffmpeg, dirs, status)`에 `"고지: "`로 시작하는 줄이 정확히 1개이고 매니페스트 `notice`와 같음; `라이선스:` 줄 뒤에 옴; libewf(설치 불가)는 preflight에서 거부되므로 이 함수에 오지 않음(기존 `cli_refusals_are_audited_before_the_prompt` 유지) | [Opus] |
| T5 | `scripts/test-make-portable.mjs` | P5 | [Sonnet] |
| T6 | `tools-unit.mjs` 새 행 “Tools confirm dialog carries the distribution notice (ko/en)” | `htConfirmHtml(fixture ffmpeg, 'ko')`에 `<p class="h-tool-notice">` + `notice`; `'en'`이면 `notice_en`; `notice`가 둘 다 없는 도구(픽스처 libewf를 installable로 바꾼 사본)는 문단 없음; HTML 이스케이프(`<`가 들어간 notice 사본) | [Opus] |
| T7 | `verify-layout.mjs` tools 구간 | 대화상자 스크린샷/텍스트에 고지 문단이 보이고 두 테마에서 대비 검사(`check-a11y`가 이미 대화상자를 보면 자동) | [Opus] |
| T8 | CI P6 스모크 | 실기 확인(테스트 코드 아님) | [Opus] |

### 그대로 두는 테스트(옵션 C와 무관, 통과해야 함)
`installs_the_pinned_gplv3_text_next_to_the_archive_license`, `shipped_ffmpeg_entry_installs_the_pinned_gplv3_text`, `bundled_license_must_match_its_pin_and_be_compiled_in`, `picks_in_preference_order`, `btbn_lgpl_build_never_picks_libopenh264`(이름만), 시험 인코딩·취소·스테이징·감사 체인·그림자 테스트 전부, `artifacts.rs`·`video_export.rs`의 `lgpl` 픽스처 테스트(픽스처 이름은 그대로).

---

## 7. 순서·게이트·머지

1. P1 → P2 → P5 → P3(옮긴 뒤 삭제) → P4 → P6.
2. L4 → M2 → M1 → M3 → M4 → M6 → T1–T7.
3. E1 → E2 → L3 → L1/L2 → D1 → D3 → D4 → P7 → M5.
4. 게이트(각 커밋): `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, `node scripts/check-ps-bom.mjs`, `node scripts/check-css.mjs`, `node scripts/check-js.mjs`, `node scripts/verify-review-batch.mjs --unit`(tools-unit 포함), `node scripts/test-make-portable.mjs`, `bash scripts/test-build-release.sh`(Git Bash/Linux/macOS, 가짜 cargo·rustc·zip — Fix round 8), `node scripts/verify-layout.mjs`(tools 구간), 그리고 **push 후 windows-ci 전체**(P6의 실제 다운로드 포함 — 첫 실행 결과를 `docs/PLAN_TOOLS_2026-10-20.md` §옵션 C 절에 한 줄로 적는다: h264 pick, buildconf 요약).
5. 커밋 분할 권장: ① 패키징·CI(P*) ② 고지·매니페스트·notice(L*·M*·T1–T4) ③ 문구(E*·D*·M5·M6·T5–T7). PR #5에 이어서 push; 설명에 “옵션 C: 번들 중단” 요약과 이 문서 링크.
6. 마지막에 `grep -rn "bundled\|번들" README.md THIRD_PARTY_NOTICES.md tools/manifest.json src/tool_install src/ffmpeg_encoders.rs src/cli/handlers/doctor.rs assets/app/56-tools.js scripts/make-portable.ps1 .github/workflows/windows-ci.yml` → FFmpeg를 번들한다는 뜻의 문장 0개(`bundled_licenses` 식별자·rusqlite `bundled`·CSS/JS “bundled” 빌드 용어는 무관).

---

## 8. 하지 말 것

- **FFmpeg 바이너리를 어떤 경로로도 저장소·zip·CI 아티팩트에 넣지 않는다.** P6 스모크의 산출물은 텍스트 한 파일뿐이고, 설치는 `RUNNER_TEMP` 사본에서만 한다. `dist/`에서 `tools install`을 돌리지 않는다.
- 매니페스트의 `url`·`sha256`·`size`·`files`·`license_files`·`bundled_licenses`·허용 호스트를 바꾸지 않는다(사용자가 받는 자산은 동일). “latest” URL 금지는 그대로.
- `VideoEncoder::PREFERENCE`, 시험 인코딩(B-5·round 3), 설치 파이프라인(`archive.rs`·리다이렉트 홉 검사·zip-slip 검사·원자 이동·스테이징 정리), 감사 줄 형식, API 코드·상태값을 바꾸지 않는다. 새 API 필드는 `notice`·`notice_en` 둘뿐(추가만, 제거 없음).
- 서면 소스 제공, “FrameTrace가 소스를 제공한다”, “LGPL 빌드/LGPL-only” 류 문구를 어디에도 다시 쓰지 않는다. 반대로 “FrameTrace가 GPL 의무를 진다”고도 쓰지 않는다 — 배포하지 않는다는 사실만.
- 법적 판단 문장(“합법이다/안전하다”)을 추가하지 않는다. L1의 “(Owner to confirm)” 표시는 소유자가 지울 때까지 남긴다.
- GPLv3 원문 설치(`bundled_licenses`)와 `.gitattributes`의 `tools/licenses/** -text`를 없애지 않는다.
- Rust 테스트·node 테스트에서 실제 인터넷에 나가지 않는다(가짜 127.0.0.1·가짜 zip만). 실제 다운로드는 CI P6 한 단계뿐.
- deepfake-lens·libewf·sleuthkit 항목·문구·`manual_hint`를 건드리지 않는다(이 결정과 무관).
- 옵션 A(자체 빌드)를 이 PR에서 시작하지 않는다 — 부록 A는 다음 PR의 입력이다. BtbN 빌드 스크립트·FFmpeg 소스를 저장소에 넣지 않는다.
- `docs/PLAN_TOOLS_2026-10-20.md`의 기존 절을 지우거나 고쳐 쓰지 않는다(T5·T6 교체와 §옵션 C 추가만). `docs/security-review.md`의 과거 행도 삭제하지 않고 “resolved” 표시만.
- `make-portable.ps1`에 `target/release/tools/bin`의 ffmpeg를 집어 오는 우회를 만들지 않는다. `-AllowFfmpeg` 같은 스위치도 만들지 않는다.
- 새 crate·새 npm 의존성 없음. PowerShell 파일을 새로 만들거나 고칠 때 BOM 유지.
- UI 문구는 짧게, 새 CSS 토큰 금지, 대화상자에 링크를 넣더라도 `https`만(§28 규칙).

---

## 부록 A — 옵션 A PR 개요: 자체 빌드 진짜-LGPL FFmpeg 번들 (별도 PR, 착수 전 소유자 승인)

목표: FrameTrace가 **스스로 빌드한** FFmpeg(`ffmpeg.exe`·`ffprobe.exe`)를 GitHub 릴리스 자산으로 공개하고, 그 자산을 (1) 매니페스트의 설치 출처로, (2) 배포 zip의 번들로 쓴다. LGPLv3 **§6(d)** — 네트워크로 오브젝트 코드를 제공할 때 같은 장소에서 같은 방식으로 완전한 대응 소스를 제공 — 를 **소스 tarball을 같은 릴리스 자산으로** 올려 충족한다(3년 서면 제공 불필요).

| ID | 내용 |
|---|---|
| A1 | 빌드 구성(진짜 LGPL): FFmpeg 고정 커밋(예: `29e619e767` 또는 n8.1.x 태그) + `--enable-version3 --disable-gpl --disable-nonfree --disable-doc --disable-ffplay --disable-network --disable-autodetect --enable-mediafoundation --enable-zlib`(zlib은 Zlib 라이선스). **외부 라이브러리 없음**: libx264·x265·xvid(GPL), chromaprint/FFTW3·zvbi(GPL), libopenh264(특허) 제외. 디코딩은 FFmpeg 네이티브(h264·hevc·mpeg4·aac·mp3 등 LGPL), H.264 인코딩은 `h264_mf`만(N/KN·Server Core는 mpeg4 폴백 — 지금 코드가 이미 처리). 필요하면 `--enable-openssl`(Apache-2.0, LGPLv3 호환) 대신 `--disable-network`로 통째로 뺀다(FrameTrace는 로컬 파일만 쓴다). 툴체인: MSYS2 mingw-w64(정적, 런타임 고지 `COPYING.MinGW-w64-runtime.txt` + GCC 런타임 예외) — 패키지 버전 고정. |
| A2 | 워크플로 `ffmpeg-lgpl-build.yml`(`workflow_dispatch` + 태그 `ffmpeg-v*`): 소스 체크아웃(고정 커밋) → 빌드 → `ffmpeg -buildconf`·`-encoders`·`-version` 기록 → 자산 `ffmpeg-<commit>-win64-lgpl-frametrace.zip`(bin/ffmpeg.exe·ffprobe.exe·COPYING.LGPLv3·COPYING.GPLv3·BUILDCONF.txt·mingw 런타임 고지) + **`ffmpeg-<commit>-source.tar.xz`**(빌드한 소스 트리 + configure 줄 + 빌드 스크립트 + MSYS2 패키지 목록) + `SHA256SUMS` → GitHub 릴리스(`gh release create`, 액션 SHA 고정). 재현성: `SOURCE_DATE_EPOCH`, 두 번 빌드해 해시 비교(가능하면). |
| A3 | 게이트(워크플로 안): `-buildconf`에 `--enable-gpl`·`--enable-nonfree`·`libx264`·`libopenh264`·`chromaprint`·`libzvbi` **없음**, `--enable-version3` 있음; `-encoders`에 `h264_mf`·`mpeg4`·`aac` 있음, `libopenh264`·`libx264` 없음; 64×64 시험 인코딩(h264_mf·mpeg4) 성공. |
| A4 | 매니페스트: ffmpeg 항목의 `url`·`sha256`·`size`·`checksum_source`를 **우리 릴리스**(host `github.com`, 이미 허용)로, `version`을 커밋/태그로, `license: "LGPL-3.0-or-later"`(이번엔 사실), `notice`를 “FrameTrace가 빌드·배포; 소스는 같은 릴리스의 tarball”로, `files`/`license_files` 경로를 새 zip 구조로. `bundled_licenses` GPLv3 유지. |
| A5 | 패키징 복원: `scripts/package-windows.ps1`·`scripts/test-package-windows.mjs`를 e90257d에서 되살려 출처만 우리 릴리스로(허용 호스트·홉 검사·ZipFile 쓰기 그대로), `FrameTrace-<ver>-win64-bundled.zip` + 포터블 zip 둘 다 CI 아티팩트. CI 인코더 게이트: pick은 `h264_mf`여야 하고 `-encoders`에 libopenh264가 **없어야** 한다(지금의 “libopenh264면 실패”보다 강함). |
| A6 | `THIRD_PARTY_NOTICES.md`에 번들 절 복귀: FFmpeg LGPLv3+, zlib, mingw-w64 런타임·winpthreads, GCC 런타임 예외, (openssl이면 Apache-2.0); **소스 위치 = 같은 릴리스의 `ffmpeg-<commit>-source.tar.xz`(§6(d))**, 빌드 재현 절차 링크. 서면 제공 문구는 넣지 않아도 되지만 넣어도 무방(소유자 결정). 사용자가 받은 BtbN 사본과 구별되도록 설치 경로의 `notice`도 분기. |
| A7 | 테스트: `shipped_manifest_…`가 새 핀·우리 호스트를 확인, 패키징 오프라인 테스트 복원, 인코더 fixture에 “frametrace-lgpl” 목록(h264_mf·mpeg4·aac만) 추가 → pick `h264_mf`, note `"libx264 없음 — H.264는 h264_mf 사용"`. |
| A8 | 위험·미확인: MSYS2 정적 빌드 시간(20–40분), Media Foundation 전용 H.264가 N/KN에서 mpeg4로 떨어짐(문서화), 재현 빌드 해시 일치 여부, 릴리스 자산 보관 기간(릴리스를 지우면 §6(d)가 깨진다 — 지우지 않는 규칙 필요). |

---

## 확인 못 한 것 / 소유자 결정 필요
- L1의 “복사해 넘기면 GPL 의무는 넘기는 사람에게” 문단과 M1 `notice`의 같은 문장은 법률 자문이 아니다 — 소유자가 문구를 확정할 것(표시 남김).
- P6의 실제 다운로드(170 MB, GitHub → GitHub 러너)는 처음 도는 것이라 첫 CI에서 깨질 수 있다(리다이렉트 호스트가 허용 목록 세 개 밖이면 설치가 거부된다 — 그 경우 허용 목록 추가는 **별도 결정**, 이 PR에서 임의로 넓히지 않는다).
- h264_mf 실제 인코딩(프록시·MP4)은 CI 스모크 범위 밖(probe까지) — D4의 실기 체크리스트.
- 이 문서는 저장소 밖 파일을 만들지 않았고, 계획 외 파일은 수정하지 않았다.
