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
