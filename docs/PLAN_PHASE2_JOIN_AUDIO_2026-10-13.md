# FrameTrace 계획: 연속 영상 이어붙이기(join), 실제 소리 감지와 소리 필터

소유자 결정(2026-10-13, 통합자 기본값 채택): Q1 재인코딩 join 범위 밖 · Q2 소리 측정 기본 꺼짐, 오디오 빼고 이어붙이기 없음 · Q3 facet·상세 라벨 '소리' · Q4 허용 오차 1.0초 · Q5 채널 인식 확장은 별도 작업

기준: 브랜치 `review-fixes-2026-10-13`, HEAD `19419c3`. 읽기만 했고 빌드나 실행은 하지 않았습니다.

---

## §0 코드에서 확인한 사실 (file:line)

### 클립 내보내기와 잡 인프라

| # | 사실 | 위치 |
|---|---|---|
| F1 | 클립 내보내기는 **무손실이 아닙니다**. 항상 `libx264 crf20`/`aac`로 재인코딩하고, `-map 0:v:0 -map 0:a?`로 데이터·자막 스트림은 버립니다. | `src/video_export.rs:218-298` |
| F2 | Windows ffmpeg은 필터 옵션 안의 비ASCII argv를 제대로 못 받습니다. 그래서 burn-in은 UTF-8 사이드카 파일을 쓰고, `cwd`를 clips 폴더로 고정해 상대 이름만 넘깁니다. | `video_export.rs:155-189` |
| F3 | ffmpeg가 실패하거나 출력이 0바이트면 출력 파일을 지웁니다. 감사 로그는 `artifacts/clips/export-log.jsonl`에 `append_chained_jsonl`로 남기고, ffmpeg 버전과 argv를 함께 기록합니다. | `video_export.rs:191-215`, `:453-494` |
| F4 | `POST /api/export-clip`는 백그라운드 잡입니다. `busy`를 잡지 않으며, 같은 요청은 dedupe하고, 동시 실행은 `MAX_RUNNING_CLIP_JOBS=4`개까지입니다. 등록은 state lock 안에서 다시 확인합니다. | `src/serve/media.rs:111-327` (`:119`, `:216-282`) |
| F5 | `run_clip_job`은 CLI 자식 프로세스를 `run_step_cancellable`로 실행하고, 끝나면 출력의 SHA-256을 계산합니다. 실패하거나 취소되면 출력을 지웁니다. | `media.rs:379-443` (`:398-401`) |
| F6 | `clip_job_json`은 `"kind":"export-clip"`이 하드코딩돼 있고, 진행률은 출력 파일의 현재 크기로 보여 줍니다. 상태 조회와 취소는 job id로만 찾습니다. | `media.rs:469-535` (`:483`) |
| F7 | `ClipJob`과 `clip_jobs`, `clip_seq`가 있는 곳입니다. | `src/serve/state.rs:144-146`, `:167-183` |
| F8 | 실행 중인 clip_jobs가 있으면 다음이 자동으로 따라옵니다. `/api/status`에 노출되고, 전역 취소가 clip을 취소하고, 종료를 거부하고, open-case를 거부하며, 케이스가 바뀌면 목록을 비웁니다. | `src/serve/system.rs:406-407`, `:532`, `:555`, `:592-603` |
| F9 | recover/carve 같은 case job은 clip이 실행 중이면 거부됩니다. finalize도 clip을 기다립니다. | `src/serve/jobs.rs:1811-1820` |
| F10 | 라우트 등록 위치입니다. | `src/serve/mod.rs:955`, `:966-967` |
| F11 | `run_step_cancellable`은 프로세스 트리(Job 객체 또는 프로세스 그룹)를 kill하고, stdout/stderr는 꼬리만 남깁니다. | `jobs.rs:761-823` |
| F12 | 보강 단계 구성(`enrichment_plan`)과 실행(`run_folder_enrichment`)입니다. `hash`/`validate`/`anomalies`/`make-review` 단계가 있고, 시작 옵션은 `with_hash/with_ffprobe/with_deepfake`입니다. | `jobs.rs:512-524`, `:532-693`, `:43-45` |
| F13 | 시작 옵션 체크박스가 있는 곳입니다. | `assets/examiner_app.html:150-152`, `assets/app/50-start.js:52-53` |

### 뷰어

| # | 사실 | 위치 |
|---|---|---|
| F14 | 뷰어의 클립 다이얼로그는 **`80-review.js`** 에 있습니다(브리프에 적힌 65-actions.js가 아님). 진행 행은 `clipProgressUi`이고 1초마다 폴링합니다. `askDialog`와 `withBusy`는 65-actions.js에 있습니다. | `assets/viewer/80-review.js:790-920`, `65-actions.js:255`, `:295` |
| F15 | 셸 작업 시트는 clip_jobs를 이름·상태 목록으로 렌더링하고, 실패는 토스트 한 번으로 알립니다. 보강이 끝나도 뷰어를 몰래 다시 불러오지 않고, "새 데이터 반영"을 기다립니다. | `assets/app/32-jobs.js:565-580`, `31-progress.js:200-206`, `:228-240` |
| F16 | 녹화 시각 해석은 **클라이언트 JS에만** 있습니다. `TIME_PATTERNS` 4종이 있고 `new Date(...)`로 브라우저 로컬 시간대 기준입니다. 결과는 파일명 시각 → 파일명 날짜만(`name-date`) → mtime 순입니다. **Rust 쪽에는 파일명 시각 파서가 없습니다.** | `assets/viewer/30-records.js:9-48` |
| F17 | ffprobe `creation_time` 파서는 Rust에 있습니다. 시간대가 없으면 "zone assumed" 플래그를 붙입니다. | `src/timeline.rs:376-430` |
| F18 | 채널 판별(`channelCodeOf`)은 JS 정규식입니다(`_F/_R/_I/_B` 토큰, `front/rear/interior`). 경로 전체를 대상으로 하므로 `DCIM/FRONT/...`도 F가 됩니다. BlackVue `_NF/_NR`, 한글 폴더 `전방/후방`은 인식하지 **못합니다**. `pairKeyOf`가 이 결과를 쓰는 쌍 재생에도 영향이 있습니다. | `30-records.js:59-90`, `:562-564` |
| F19 | `ProbeSummary`에는 `audio_codec`(첫 오디오 스트림)과 전체 ffprobe JSON(`raw_json`)이 있습니다. ffprobe는 `-show_format -show_streams`만 쓰고 `-show_data_hash`는 쓰지 않습니다. | `src/ffprobe.rs:8-90`, `src/model.rs:75-90` |
| F20 | `VideoRecord::to_json`은 손으로 쓴 고정 포맷입니다. fps도 "알 때만" 덧붙여 예전 인덱스와 바이트가 같게 유지합니다. 스캔할 때 ffprobe 결과는 `carried_facts`로 이어받습니다. | `model.rs:173-226`, `src/scan/mod.rs:168-198` |
| F21 | case.db는 스키마 v7입니다. `videos` 테이블에 `audio_codec` 컬럼과 `record_json`이 있습니다. 마이그레이션은 백업 후 단계별로 체인합니다. | `src/case_db/core.rs:6`, `:158-183`, `:285-312` |
| F22 | 그리드의 오디오 표시는 3상태입니다. 코덱이 있으면 `i-audio`, 프로브됐는데 코덱이 없으면 `i-mute`, 그 외는 아무것도 표시하지 않습니다. 상세 패널도 같은 규칙입니다. 아이콘 `i-audio`/`i-mute`가 있습니다. | `assets/viewer/40-grid.js:740-751`, `60-details.js:42-47`, `:163`, `assets/icons.svg:53-54` |
| F23 | facet 계산(`facetCounts`)은 각 차원을 자기 필터만 빼고 셉니다. 메모 키는 `filterMemoKey().split().slice(0,7)`이라 **필드 위치에 의존합니다**. 트리는 `renderTree`(`60-details.js:256-302`), 칩은 `PRESET_CHIPS`(`40-grid.js:44-56`)입니다. | `40-grid.js:80-146` |
| F24 | 뷰어 데이터의 경로는 두 개입니다. 서버는 `/api/records-meta`의 carveLog/validationLog 등을 쓰고, 법원 제출용 독립 뷰어는 make-review가 데이터를 직접 넣습니다. | `src/serve/records.rs:515-622`, `src/html_report.rs:412` |
| F25 | **`append_chained_jsonl`은 append할 때마다 로그 전체를 읽고 fsync합니다.** 그래서 항목이 N개 쌓이면 비용이 O(N²)입니다. | `src/audit.rs:60-104`, `:112-115` |
| F26 | QA 감사 레인은 `evidence/`와 `artifacts/` 아래의 모든 `*log.jsonl`를 자동으로 레인으로 잡습니다. 비어 있으면 실패하고, 앵커로 잘림을 탐지합니다. | `src/qa.rs:474-600`, `:609-639` |
| F27 | 패키지는 정해진 폴더를 재귀로 넣는데 **`artifacts/joined`는 목록에 없습니다**. `.`으로 시작하는 이름은 제외됩니다. | `src/package.rs:254-268`, `:270-292` |
| F28 | 보고서 입력은 `ReportInputs` 고정 필드입니다. 클립 섹션이 따로 있고, 영상 표에는 오디오 열이 없습니다. 읽는 쪽은 `case.rs`입니다. | `src/report.rs:3-24`, `:325-356`, `src/cli/handlers/case.rs:659-700` |
| F29 | case 폴더는 항상 media root입니다. 그래서 `artifacts/joined/*.mp4`는 `/media`로 재생할 수 있고, 비-`vid_` 레코드의 export-clip 경로 확인도 통과합니다. | `jobs.rs:111-119`, `media.rs:164-185` |
| F30 | 선택 바의 "보내기·복사" 메뉴 그룹 위치입니다. | `assets/viewer/viewer.body.html:253-268` |

---

## §1 기능 1: 연속 영상 이어붙이기 (join)

### 1.1 핵심 결정과 그 근거

| 결정 | 내용 | 근거 |
|---|---|---|
| 방식 | ffmpeg concat demuxer + `-c copy`만 씁니다. **재인코딩은 v1 범위 밖**이고, 맞지 않으면 거부합니다. | 스트림 복사는 원본의 인코딩된 패킷을 그대로 옮기므로 화소가 바뀌지 않습니다. 재인코딩하면 원본과 다른 화소가 되어 별도 표시와 검증이 필요한데, 그건 이미 클립 내보내기(F1)가 하는 일입니다. |
| 겹침 | **자르지 않습니다.** 세그먼트를 전부 넣고, 겹친 곳은 매니페스트·로그·보고서에 위치와 길이로 남깁니다. | 정확히 잘라내려면 재인코딩이 필요하고, 키프레임 단위로 자르면 오차가 수 초 생겨 내용이 바뀝니다. 같은 장면이 두 번 나오는 것을 밝히는 편이, 몰래 줄인 영상보다 법정에서 방어하기 쉽습니다. |
| 끊김 | 빈 화면이나 프레임으로 **채우지 않습니다**. 출력 시간축은 이어지지만, 매니페스트의 `output_offset` 대 `recorded_start` 표가 실제 시각을 알려 줍니다. | 원본에 없는 프레임을 만들지 않기 위해서입니다. |
| 순서 근거 | ① 모든 세그먼트에 파일명 시각(시분초까지)이 있으면 그것, ② 아니면 모든 세그먼트에 ffprobe `creation_time`이 있으면 그것, ③ 아니면 파일명 자연 정렬. ③일 때는 "끊김·겹침 확인 불가"라고 표시합니다. **근거를 섞어 쓰지 않습니다.** mtime은 쓰지 않습니다. | 복사하면 mtime이 바뀌어 믿을 수 없습니다. 근거가 섞이면 시각 차이 계산이 의미 없어집니다. |
| 허용 오차 | 기본 **1.0초**. 요청으로 0.5~5초 사이에서 바꿀 수 있습니다. | 파일명 시각은 초 단위로 내림된 값이라 오차가 ±1초 있습니다. 0.5초로 두면 정상적인 연속도 끊김·겹침으로 잘못 보고됩니다. |
| 채널 | Rust로 `channelCodeOf`(F18)를 **그대로 옮긴** `channel_code`를 씁니다. 판정: 모든 세그먼트가 같은 코드면 허용, 모두 미확인이면 경고만 하고 허용, 일부만 알거나 서로 다르면 거부합니다. | JS와 Rust가 다르게 판정하면 그리드의 "채널"과 이어붙이기 대화상자가 서로 다른 말을 하게 됩니다. 인식 범위를 넓히는 문제는 §7-Q5. |
| 대상 | 색인된 영상(`vid_*`)만 받습니다. 카빙·복구 후보와 이미 이어붙인 결과물은 거부합니다. | 후보는 검증되지 않은 파일입니다(RECOVERY_BOUNDARIES). 결과물을 다시 이어붙이면 출처 관계가 중첩되어 복잡해집니다. |
| 출력 컨테이너 | 영상이 h264/hevc이고 오디오가 aac/mp3/없음이면 `.mp4`(`+faststart`), 그 외에는 `.mkv`입니다. | mp4 muxer는 pcm 같은 코덱을 copy로 받지 못합니다. |
| 함께 옮기는 스트림 | `-map 0:v:0 -map 0:a?`만 옮기고, 데이터·자막(GPS 등)은 **옮기지 않습니다**. 매니페스트의 `not_carried`에 적어 둡니다. | mp4 muxer는 모르는 데이터 스트림이 있으면 실패합니다. 빠진 것은 숨기지 않고 명시합니다. |

### 1.2 새 모듈과 CLI

새 파일 `src/rectime.rs`(T1):
- `parse_name_time(&str) -> Option<NaiveRecTime{secs_naive, text, has_time}>`. JS `TIME_PATTERNS`(`30-records.js:9-14`)를 같은 순서·같은 정규식으로 옮깁니다. 시간대 없는 naive 값이고, 차이 계산에만 씁니다.
- 동등성 fixture `tests/fixtures/rectime-cases.json`를 Rust 단위 테스트와 Node 검사가 같이 읽습니다.

새 파일 `src/join.rs`(T1):
- `channel_code(path) -> Option<char>`: JS 포팅.
- `plan(case_dir, ids, tolerance) -> JoinPlan`: 인덱스에서 경로를 찾고, 라이브 ffprobe를 `-show_streams -show_format -show_data_hash sha256`로 최대 4개씩 병렬 실행한 뒤 정렬·판정합니다.
- `JoinPlan::plan_id`: 정규화한 계획(ids 순서, 경로, size, mtime, 스트림 파라미터, tolerance)의 SHA-256입니다.
- `concat_list(&plan) -> String`(아래 형식 참고), `run(case_dir, plan, opts, progress)`, `write_manifest`, `append_log`.

CLI `frametrace join-video <case> --ids <id,...> [--tolerance 1.0] [--plan-id <hex>] --job-tag <tag>`:
1. 다시 계획을 세우고, `--plan-id`가 다르면 종료 코드 3과 "검사 이후 파일이 바뀌었습니다"로 끝냅니다.
2. 남은 디스크 공간이 원본 크기 합계의 1.05배 이상인지 확인합니다(`src/diskspace.rs`).
3. 원본마다 **라이브 SHA-256**을 계산합니다. 인덱스 해시와 다르면 `hash_mismatch`로 기록합니다(clip의 `--hash-source`와 같은 방식).
4. ffmpeg를 실행합니다. `cwd = artifacts/joined/`이고 argv에는 ASCII만 넣습니다:
   `-y -hide_banner -nostdin -f concat -safe 0 -i .join-<tag>.ffconcat -map 0:v:0 -map 0:a? -c copy -avoid_negative_ts make_zero [-movflags +faststart] -f mp4|matroska .join-<tag>.part`
5. `.part`를 ffprobe로 확인합니다(영상 스트림 존재, 길이). 길이 차이가 1초와 합계의 1% 중 큰 값을 넘으면 실패가 아니라 **경고**로 `duration_check.ok=false`를 남기고 요약에 표시합니다. 바이트는 원본을 그대로 복사한 것이라서입니다.
6. 원본을 다시 stat해 size와 mtime이 그대로인지 확인합니다. 바뀌었으면 실패합니다.
7. `.part`의 SHA-256 계산 → 최종 이름으로 rename(`unique_available_path`) → 매니페스트 원자적 쓰기 → 로그 체인 append. 6~7 단계 중 하나라도 실패하면 출력과 매니페스트를 지우고 종료 코드를 0이 아니게 합니다(F3과 같은 원칙).
8. 진행 상황은 `.join-<tag>.progress.json`에 `{stage:"hash"|"concat"|"verify", done, total}`로 원자적으로 씁니다.

concat 목록(`.join-<tag>.ffconcat`)은 **UTF-8, BOM 없음**(BOM이 있으면 첫 줄 인식에 실패)입니다:

```
ffconcat version 1.0
file 'C:/증거/전방 영상/20260910_120000_F.mp4'
```

- 경로는 `canonicalize_display` 형태입니다. `\\?\` 접두사를 떼고 구분자는 `/`로 바꿉니다.
- `'`는 `'\''`로 바꿉니다.
- 경로에 개행이나 제어문자가 있으면 거부합니다.
- 경로 바이트는 이 파일 안에만 있고 argv로 넘어가지 않습니다(F2).

### 1.3 파일 배치

```
artifacts/joined/
  join_F_20260910-120000_n10.mp4            # 출력 (ASCII 이름, 필요하면 __2 같은 접미사)
  join_F_20260910-120000_n10.manifest.json  # 사이드카 매니페스트
  join-log.jsonl                            # 체인 감사 레인 (F26으로 QA가 자동 인식)
  .join-<tag>.ffconcat / .part / .progress.json   # 임시 파일. '.'로 시작해 패키지에서 제외(F27). 성공·실패·취소 때 삭제
```

다음 join을 시작할 때, 실행 중인 잡의 것이 아닌 `.join-*` 잔여 파일은 지우고 서버 로그에 남깁니다(크래시 복구용).

### 1.4 매니페스트 스키마 (`schema_version: 1`)

```json
{
  "schema_version": 1, "kind": "frametrace-join-manifest", "plan_id": "…",
  "case_id": "…", "created_unix": 0, "tool": {"name": "frametrace", "version": "…"},
  "output": {"path_rel": "artifacts/joined/….mp4", "sha256": "…", "size_bytes": 0,
             "container": "mp4", "duration_seconds_probed": 602.1,
             "video": {"codec":"h264","width":1920,"height":1080,"r_frame_rate":"30/1"},
             "audio": {"codec":"aac","sample_rate":48000,"channels":1}},
  "method": {"mode": "stream-copy", "reencode": false, "demuxer": "concat",
             "ffmpeg_version": "…", "ffmpeg_argv": ["…"],
             "concat_list_sha256": "…", "concat_list": ["file '…'"]},
  "order": {"basis": "filename-time|creation-time|filename-order", "tolerance_seconds": 1.0,
            "time_note": "파일명 시각은 시간대 없는 장치 기록값(naive local)"},
  "channel": {"code": "F", "label": "전방", "basis": "filename-or-path-token|unknown"},
  "segments": [{"index": 1, "record_id": "vid_000123", "source_path": "…",
                "size_bytes": 0, "modified_unix": 0,
                "sha256_at_join": "…", "sha256_indexed": "…|null", "hash_mismatch": false,
                "recorded_start": {"text": "2026-09-10 12:00:00", "source": "filename|creation_time|null"},
                "duration_seconds": 60.03,
                "output_offset_seconds": 0.0, "output_end_seconds": 60.03}],
  "junctions": [{"after_index": 1, "delta_seconds": -1.02, "kind": "continuous|gap|overlap|unknown"}],
  "totals": {"segments": 10, "duration_seconds": 602.1, "gaps": 0, "gap_seconds": 0,
             "overlaps": 2, "overlap_seconds": 2.04},
  "duration_check": {"expected": 602.1, "observed": 602.0, "ok": true},
  "not_carried": ["data/subtitle streams (e.g. GPS telemetry)"],
  "caveats": ["시각은 장치가 기록한 값이며 장치 시계의 정확성은 검증하지 않았습니다.",
              "겹침 구간은 잘라내지 않았습니다 — 같은 장면이 두 번 나올 수 있습니다.",
              "출력 내 위치는 ffprobe가 잰 각 원본 길이의 누적합으로 계산했습니다."]
}
```

### 1.5 감사 항목 (`artifacts/joined/join-log.jsonl`, `append_chained_jsonl`)

```json
{"schema_version":1,"event":"join-video","joined_unix":0,"output_path":"…","output_sha256":"…",
 "output_size_bytes":0,"manifest_path":"…","manifest_sha256":"…","plan_id":"…",
 "segment_count":10,"source_ids":["…"],"source_sha256":["…"],"hash_mismatches":0,
 "order_basis":"filename-time","channel":"F","tolerance_seconds":1.0,"gaps":0,"overlaps":2,
 "mode":"stream-copy","duration_check_ok":true,"ffmpeg_version":"…",
 "command":"ffmpeg","command_args":["…"]}
```

`manifest_sha256`가 사이드카를 체인에 묶어 둡니다.

### 1.6 서버 API (T2)

`POST /api/join-check` 요청: `{"ids":[...], "tolerance":1.0}`. JSON 배열이므로 `serde_json`으로 파싱합니다(`api_export_selected` 방식, `media.rs:877`).

거부 조건:
- 열린 케이스 없음
- id가 2개 미만이거나 500개 초과
- 제어문자 포함, 512바이트 초과, `vid_*`가 아닌 id, 인덱스에 없는 id
- `busy`일 때(`CLIP_BUSY_ERROR`)

응답:

```json
{"ok":true,"plan":{
  "plan_id":"…","order_basis":"filename-time","tolerance_seconds":1.0,
  "channel":{"code":"F","label":"전방","basis":"…"},
  "container":"mp4",
  "segments":[{"id","name","recorded_start_text","time_source","duration_seconds","size_bytes",
               "video":{"codec","profile","width","height","pix_fmt","r_frame_rate","time_base","extradata_hash"},
               "audio":{"codec","sample_rate","channels"}|null}],
  "junctions":[{"after_index","delta_seconds","kind"}],
  "totals":{"segments","duration_seconds","gaps","gap_seconds","overlaps","overlap_seconds","overlap_max_seconds"},
  "blocking":[{"code":"channel-mixed|stream-mismatch|audio-presence|unknown-duration|duplicate-file|same-start|not-indexed","message":"…","ids":["…"]}],
  "warnings":[{"code":"channel-unknown|order-fallback|hash-unavailable|vendor-differs|folders-differ","message":"…"}],
  "summary_text":"전방 · 10개 · 10분 02초 · 끊김 0곳 / 겹침 2곳(각 1초)"}}
```

`summary_text`는 서버가 만듭니다. 형식 규칙은 이렇습니다:
- 시간: `M분 SS초`, 1시간 이상이면 `H시간 MM분 SS초`.
- 겹침 길이가 모두 같은 정수 초로 반올림되면 `(각 N초)`, 아니면 `(합계 x.x초, 최대 y.y초)`.
- 채널을 모르면 `채널 미확인`.
- 근거가 `filename-order`이면 `끊김·겹침 확인 불가`.

스트림 비교 항목: `codec_name`, `profile`, `width`, `height`, `pix_fmt`, `r_frame_rate`, `time_base`, `extradata_hash`(SPS/PPS), 오디오 존재 여부, 오디오 `codec_name/sample_rate/channels`. `avg_frame_rate`가 다르면 경고만 합니다(VFR).

`POST /api/join` 요청: `{"ids":[...], "plan_id":"…", "tolerance":1.0}`. 응답은 `export-clip`과 같은 모양입니다: `{"ok":true,"job":{…,"kind":"join"},"deduped":false}`.
- dedupe 키는 `case + 정렬된 ids + tolerance + plan_id`입니다.
- 등록, `busy` 재확인, 동시 실행 상한 4는 **clip과 같은 lock과 같은 상한**을 씁니다.

상태 조회와 취소는 **기존** `GET /api/export-clip-status?job=`, `POST /api/export-clip-cancel`을 그대로 씁니다(id로만 찾음, F6). `/api/status`의 `clip_jobs`에 `kind:"join"`으로 나옵니다.

일반화 작업:
- `ClipJob`에 `kind: &'static str`와 `progress_path: Option<PathBuf>`, `expected_bytes: Option<u64>`를 추가합니다.
- `clip_job_json`은 `kind`, `stage`(progress.json에서 읽음), `bytes_written`, `expected_bytes`를 내보냅니다.
- join일 때 `path`는 최종 출력 경로이고, 실행 중 `bytes_written`은 `.part` 크기입니다.
- F8과 F9의 보호 장치가 자동으로 적용됩니다: finalize 대기, open-case·shutdown 거부, 전역 취소.

`run_clip_job`의 정리 단계 일반화:
- 실패하거나 취소되면 `.part`, `.ffconcat`, `.progress.json`, (생겼다면) 최종 출력과 매니페스트를 삭제합니다.
- **Windows에서는 삭제를 200ms 간격으로 5번 재시도**합니다(백신 등이 파일을 잡고 있을 수 있음).
- 그래도 못 지우면 잡을 `failed`로 하고 "임시 파일을 지우지 못했습니다: <경로>"를 표시합니다. 조용한 실패는 없습니다.
- 성공했을 때 SHA-256은 CLI가 계산해 매니페스트에 쓴 값을 쓰고, 서버는 출력 파일을 다시 해시해 서로 같은지 확인합니다.

### 1.7 잡 수명 주기

```
check(동기, 프로브) → [대화상자] → join(등록) → running{stage: hash i/N → concat (진행률 = .part 바이트 / 원본 합계) → verify}
  → done{path, sha256, manifest}           → 로그 1줄
  → failed{error}  (임시 파일·출력 삭제, 로그 없음)
  → cancelled      (프로세스 트리 kill → 임시 파일 삭제, 로그 없음)
```

### 1.8 UI 흐름 (T4)

**진입점.** "보내기·복사" 메뉴의 "원본 파일" 그룹(F30)에 menuitem **"선택 영상 이어붙이기…"**(`btnJoinSelected`)를 추가합니다. 새 버튼이나 단축키는 추가하지 않습니다. 선택이 2개 미만이면 `disabled`이고 `title`은 "영상을 2개 이상 선택하십시오"입니다. 서버 없는 독립 뷰어에서는 숨깁니다.

**검사.** `withBusy`로 "검사 중…"을 표시하면서 `/api/join-check`를 호출합니다.

**대화상자.** `askDialog`에 `content`(DOM 노드)와 `okDisabled`/`okTitle` 옵션을 추가해 씁니다(65-actions.js). 제목은 "영상 이어붙이기"입니다.
1. 굵은 한 줄: `전방 · 10개 · 10분 02초 · 끊김 0곳 / 겹침 2곳(각 1초)`
2. 보조 한 줄: `순서: 파일명 시각 기준 · 허용 오차 1초 · 원본 프레임 그대로(재인코딩 없음)`
3. 겹침이 있을 때만: "겹친 구간은 잘라내지 않습니다 — 같은 장면이 두 번 나올 수 있고, 위치는 매니페스트에 기록됩니다."
4. 끊김이 있을 때만: "끊긴 구간은 채우지 않습니다 — 결과 영상에서는 바로 이어져 보입니다."
5. 근거가 `filename-order`일 때: "녹화 시각이 없어 파일명 순으로 이어붙입니다 — 끊김·겹침은 확인할 수 없습니다."
6. 차단이 있을 때: `i-warn` 아이콘과 "이어붙일 수 없습니다:" 아래에 목록. 예: "채널이 섞여 있습니다 (전방 6 · 후방 4)", "해상도가 다릅니다: 1920×1080 (8개) / 1280×720 (2개)", "오디오 트랙이 일부에만 있습니다 (3개 없음)", "길이를 알 수 없는 파일 1개 — 손상 의심". 색은 `--danger` 글자색을 씁니다(실패 의미).
7. `<details>` "세그먼트 목록 (10)"은 접힌 상태로 둡니다. 열면 `# · 파일 · 녹화 시각 · 길이 · 다음과의 간격` 표가 나오고, 최대 높이를 정해 스크롤합니다.
8. 버튼은 `[취소]` `[이어붙이기]`(primary) 두 개입니다. 차단이 있으면 primary를 disabled로 하고 `title`에 첫 번째 사유를 넣습니다.

**진행.** `clipProgressUi`/`pollExportJob`을 재사용해 일반화합니다. 문구는 단계에 따라 "이어붙이는 중 · 해시 3/10", "이어붙이는 중 · 45% · 12초", 그리고 `[중단]` 버튼입니다.

**완료.** 단일 버튼 대화상자 "이어붙이기 완료"에 다음을 보여 줍니다: 산출물 경로, 크기, SHA-256, 매니페스트 경로, `길이: 10분 02초 (원본 합계 10분 02초)`, 끊김·겹침 수. `duration_check`가 실패했으면 "출력 길이가 원본 합계와 x초 다릅니다 — 매니페스트를 확인하십시오" 경고 줄을 추가합니다.

**취소·실패.**
- 취소 토스트: "이어붙이기를 중단했습니다 — 만들던 파일은 지웠습니다."
- 실패 토스트는 직접 닫을 때까지 남습니다.
- 셸 작업 시트(`32-jobs.js`)는 kind별 상태 문구를 씁니다(`join`: "이어붙이는 중/완료/실패/중단됨").

**뷰어 반영.**
- 셸: join이 완료되면 보강 완료와 같은 "새 데이터 반영" 대기 경로(F15)를 탑니다. 몰래 다시 불러오지 않습니다.
- 독립 `/viewer/`: 완료 대화상자에 "목록을 새로 고치면 표시됩니다" 한 줄을 넣습니다.

**파생 레코드.** `records-meta`와 `html_report`에 `joinLog`(join-log 항목 배열)를 추가합니다. `30-records.js`가 이를 레코드로 만듭니다:
- `kind:"joined"`(라벨 "이어붙인 영상"), `id:"join:<출력파일명>"`, `status:"derived"`(라벨 "파생물", 아이콘 + 텍스트).
- `recTime`은 첫 세그먼트 시각, `channel`은 계획의 채널, `sha256`은 출력 해시, `note`는 "원본 10개 이어붙임 · 끊김 0 / 겹침 2 · 매니페스트 …"입니다.
- 상세 패널 "검증" 탭에 원본 목록을 보여 줍니다.
- 마크, 태그, IN/OUT, 클립 내보내기는 그대로 동작합니다(F29).
- join 메뉴는 이 레코드를 선택해도 서버에서 거부합니다.

### 1.9 보고서와 패키지 (T3)

- `package.rs:254`에 `"artifacts/joined"`를 추가합니다. `.join-*` 임시 파일은 자동으로 제외됩니다.
- `ReportInputs`에 `join_log_jsonl`을 추가하고, `case.rs`가 읽고 redact하게 합니다.
- 보고서에 새 섹션 **"이어붙인 영상"** 을 둡니다. 열: 산출물, SHA-256, 원본 수, 채널, 순서 근거, 총 길이, 끊김·겹침, 매니페스트 SHA-256, 감사 체인.
- 섹션 아래 고정 문구: "원본 영상 프레임을 재인코딩 없이 순서대로 연결(스트림 복사)했습니다. 겹친 구간은 잘라내지 않아 같은 장면이 두 번 나올 수 있고, 끊긴 구간은 채우지 않았습니다. 순서와 끊김·겹침 판단은 장치가 기록한 시각을 근거로 하며, 장치 시계의 정확성은 검증하지 않았습니다. 위치·데이터 스트림(GPS 등)은 포함되지 않았습니다."
- 요약 카운터에 "이어붙임 N"을 추가합니다.

### 1.10 엣지 케이스

| 상황 | 처리 |
|---|---|
| 같은 파일이 두 번 선택됨(경로 또는 해시가 같음) | 차단 `duplicate-file` |
| 시작 시각이 같은 세그먼트 2개(다른 폴더의 복사본, 또는 전방·후방) | 차단 `same-start` |
| 일부 세그먼트만 시각이 있음 | 근거를 `filename-order`로 하고 경고. 끊김·겹침은 `unknown` |
| ffprobe 실패 또는 길이 없음(moov 손상) | 차단 `unknown-duration` |
| mdat가 잘렸지만 moov는 있음 | ffmpeg는 성공할 수 있습니다. `duration_check` 경고로 드러냅니다 |
| SPS/PPS(`extradata_hash`)가 다름 | 차단합니다. mp4 copy는 첫 파일의 avcC만 쓰기 때문에 이후 세그먼트가 깨질 수 있습니다 |
| 일부에만 오디오 트랙이 있음 | 차단합니다(오디오 빼기 옵션은 §7-Q2) |
| 입력이 .ts/.avi | 컨테이너 규칙에 따라 mp4 또는 mkv. `-avoid_negative_ts make_zero` |
| 경로에 한글, 공백, `'` | 목록 파일 안에서 이스케이프. argv는 ASCII만 |
| 경로에 개행 | 차단 |
| `\\?\` 경로, 260자 넘는 긴 경로 | 접두사 제거. 긴 경로는 Windows IT에서 확인합니다(§6) |
| 남은 공간 부족 | 시작 전에 거부하고 필요한 용량을 표시 |
| 진행 중 케이스 전환이나 종료 | F8로 거부 |
| 서버 크래시 | 다음 join이 `.join-*` 잔여 파일을 정리. 잡 기록은 사라지지만 로그가 없으니 산출물로 기록된 것도 없습니다 |
| 같은 요청을 다시 보냄 | 출력이 남아 있으면 기존 잡을 돌려주고, 없으면 새 파일을 만듭니다 |
| 검사 이후 원본 변경 | plan_id가 달라져 거부되고 "다시 검사하십시오"를 표시 |
| 원본 500개 초과 | 거부 |

---

## §2 기능 2: 실제 소리 감지와 소리 필터

### 2.1 결정

| 항목 | 결정 | 근거 |
|---|---|---|
| 측정 방법 | `ffmpeg -nostdin -hide_banner -v info -i <src> -map 0:a:<k> -vn -sn -dn -af volumedetect -f null -`. 오디오 스트림마다 최대 4개까지 측정합니다. stderr에서 `n_samples`, `mean_volume`, `max_volume`을 읽고, `-inf`도 처리합니다. | volumedetect는 트랙 **전체**의 최대(peak)와 평균(RMS)을 dBFS로 줍니다. astats는 정보가 많지만 파싱과 버전 차이 위험이 큽니다. silencedetect는 구간 정보라 저장량이 많습니다. |
| 무음 판정 | `SILENT_MAX_DBFS = -70.0`. 최대값이 −70 이하이면 "무음 트랙", 넘으면 "소리 있음". **`n_samples == 0`이거나 디코드 오류이면 무음이 아니라 "측정 실패(미확인)"** 입니다. | 디지털 무음(마이크를 끈 블랙박스가 흔히 0을 기록)은 volumedetect에서 −91 dB 또는 −inf로 나옵니다. 0을 AAC로 인코딩했다 복원하면 −80~−90 정도 잡음이 생길 수 있습니다. 실제 마이크 경로(프리앰프·ADC 잡음, 실내·노면 소음)는 대부분 −60 dBFS를 넘습니다. 그래서 양쪽에 약 20 dB 여유가 있는 −70을 기준으로 합니다. 기준값은 로그에 같이 남겨 나중에 다시 계산할 수 있게 합니다. **출시 전 실제 블랙박스 표본(docs/validation-corpus.md)으로 확인해야 합니다.** |
| 측정 범위 | 트랙 전체를 잽니다. 잘라서 재지 않습니다. `measured_seconds = n_samples / sample_rate`를 기록합니다. | 일부만 재고 "무음"이라고 하면 사실과 다른 주장이 됩니다. |
| 대상 | ffprobe에서 `audio_codec`이 있는 색인 영상만 잽니다. 트랙이 없으면 ffprobe 사실만으로 "없음"이고, 프로브되지 않은 영상은 "미확인"입니다. | 쓸데없는 디코드를 피합니다. |
| 저장 | **case.db와 인덱스 스키마는 바꾸지 않습니다.** 새 체인 레인 `evidence/logs/audio-level-log.jsonl`에 저장합니다. | 인덱스는 다시 스캔할 때 새로 쓰이는데, `carried_facts`(F20) 밖의 사실은 사라질 위험이 있습니다. 레인으로 두면 마이그레이션이 없고(손실 0), QA·패키지가 자동으로 포함하며(F26·F27), 검증 로그와 같은 "현재 바이트에 대한 측정" 결합 규칙을 쓸 수 있습니다. |
| 로그 쓰기 단위 | **파일 200개당 1항목**(`results` 배열)입니다. | F25 때문입니다. 파일마다 체인 append를 하면 10만 개에서 O(N²) 읽기와 10만 번의 fsync가 됩니다. |
| 무효화 | 측정 결과는 해당 레코드의 현재 `size_bytes`와 `modified_unix`가 같고, 해시가 양쪽에 있으면 해시도 같을 때만 적용합니다. 다르면 "측정 후 파일 변경 — 미확인"입니다. 같은 id에 여러 항목이 있으면 가장 최근 것을 씁니다. | 바이트가 바뀐 파일에 이전 측정을 붙이지 않기 위해서입니다(보고서의 `old_validation_for_different_bytes_is_not_confirmed`와 같은 원칙). |
| 이어서 하기와 증분 | 새 체크포인트 `db/audio-level-progress.jsonl`을 둡니다(`validate-batch` 패턴, `batch.rs:281-284`). 이미 유효한 측정(같은 방법과 기준값)이 있는 파일은 건너뜁니다. | 다시 분석해도 같은 측정을 반복하지 않습니다. |

### 2.2 어느 단계에서 재고 비용은 얼마인가

- CLI `frametrace measure-audio <case> [selection.json] [--all]`을 새로 둡니다(T1, `src/audio_level.rs`).
- 보강 단계 `("audio","소리 측정")`을 **`validate` 다음**에 둡니다. ffprobe가 있어야 대상을 고를 수 있으므로 `with_audio && with_ffprobe`일 때만 들어갑니다(`jobs.rs:512`).
- 시작 옵션 체크박스 `withAudio`를 추가합니다. 문구: "소리 측정 — 오디오 트랙을 끝까지 디코드해 무음 여부를 잽니다(대용량은 해시만큼 오래 걸릴 수 있음)". ffprobe가 꺼져 있으면 disabled이고 사유를 `title`에 넣습니다. **기본값은 꺼짐**입니다(§7-Q2).
- **나중에 따로 실행**하는 경로도 둡니다. 결과물 시트 "고급" 안에 "소리 측정 (미측정 항목)"을 추가합니다. `start_case_job(kind:"audio-measure")`를 쓰며, 실행 중 마크는 큐에 쌓이고 끝나면 make-review가 돕니다(`jobs.rs:1811` 재사용).
- **비용(추정, 측정 필요).**
  - 오디오 디코드 자체는 1분 AAC당 0.1초 미만으로 가볍습니다.
  - mov demuxer는 버린 스트림의 샘플을 건너뛰므로 I/O는 오디오 청크와 seek 위주가 됩니다.
  - 인터리브 간격이 짧은 파일은 사실상 파일 전체를 읽는 것과 비슷할 수 있습니다.
  - 최악의 경우 해시를 한 번 더 도는 것과 비슷합니다.
  - 파일별 타임아웃은 `max(60s, 2×duration)`입니다.
  - IT 벤치(§3)에서 10k 파일 기준 수치를 얻은 뒤 기본값을 확정합니다.

### 2.3 로그 항목 (`schema_version 1`)

```json
{"schema_version":1,"event":"audio-level","measured_unix":0,"method":"ffmpeg-volumedetect",
 "threshold_silent_max_dbfs":-70.0,"ffmpeg_version":"…","command_template":["-nostdin","…","-af","volumedetect","-f","null","-"],
 "results":[{"selector":"vid_000123","source_path":"…","size_bytes":0,"modified_unix":0,"sha256_indexed":"…|null",
   "result":"sound|silent|no-audio-stream|error",
   "streams":[{"index":1,"codec":"aac","n_samples":2880000,"measured_seconds":60.0,
               "max_volume_db":-12.3,"mean_volume_db":-38.0,"max_is_neg_inf":false}],
   "error":null}]}
```

레코드 수준 판정:
- 스트림 중 하나라도 `sound`이면 `sound`.
- 모든 스트림이 `silent`이면 `silent`.
- 오류가 하나라도 있고 `sound`가 없으면 `error`(UI에서는 미확인).

### 2.4 데이터 전달

- `records-meta`와 `html_report`에 압축한 맵 **`audioLevels: {id: {r:"sound|silent|error|stale", max, mean, sec, t}}`** 을 넣습니다. 서버가 무효화까지 끝낸 값만 넘기므로, 10만 건이어도 약 6MB입니다.
- 원본 로그 배열은 넘기지 않습니다.
- `html_report`(법원 제출용 독립 뷰어)를 바꾸는 것이므로 `verify-embed`로 의도한 변경인지 확인합니다.

### 2.5 표시 (그리드, 상세)

`30-records.js`가 `record.audioState`를 정합니다:

| 상태 | 조건 | 그리드 아이콘(모양으로 구분) | tooltip / sr 텍스트 | 상세 "소리" | facet 값 |
|---|---|---|---|---|---|
| `sound` | 유효 측정 sound | `i-audio`(기존, 음파 있음) | "소리 있음 · 측정 최대 −12 dB" | "소리 있음 — 측정: 최대 −12.3 dB · 평균 −38.0 dB (aac)" | 소리 있음 |
| `silent` | 유효 측정 silent | **새 `i-audio-silent`**(스피커 + 수평선) | "무음 트랙 · 측정 최대 −91 dB" | "무음 트랙 — 측정: 최대 −91.0 dB (기준 −70 dB 이하)" | 무음 트랙 |
| `none` | 프로브 성공, 오디오 코덱 없음 | `i-mute`(기존) | "오디오 트랙 없음" | "오디오 트랙 없음 (ffprobe)" | 트랙 없음 |
| `track` | 코덱은 있는데 측정이 없거나, 오류, 또는 측정 뒤 파일 변경 | **새 `i-audio-track`**(스피커, 음파 없음) | "오디오 트랙 있음 · 소리 미측정" (오류·변경이면 사유) | "오디오 트랙 있음 (aac) — 소리 미측정" 또는 사유 | 미확인 |
| `unknown` | 프로브되지 않음 | 표시 없음(기존과 같음) | — | "미확인" | 미확인 |

- 지금 "코덱 있음 = `i-audio`"로 보이는 레코드는 측정 전까지 `i-audio-track`으로 바뀝니다. 의도한 변경이며, 검증 안 된 소리를 주장하지 않기 위해서입니다.
- 아이콘은 `currentColor`만 쓰므로 두 테마 AA 대비는 기존과 같습니다. 모든 아이콘에 `aria-label`을 붙입니다.

### 2.6 facet과 필터

- `state.audio`(`""|sound|silent|none|unknown`)를 추가합니다. `passesAudio`를 만들고 `computeFilteredRecords`와 `facetCounts`에 넣습니다. `audioCounts`는 자기 필터를 뺀 나머지 기준으로 셉니다.
- `filterMemoKey` 끝에 `state.audio`를 넣고, **`facetCounts`의 `slice(0,7)` 키 계산을 바꿔** 정렬 필드만 빠지게 합니다(F23 위험).
- `renderTree`에 섹션 **"소리"** 를 추가합니다(이름은 §7-Q3). 값 순서는 소리 있음 / 무음 트랙 / 트랙 없음 / 미확인으로 고정합니다(알파벳 정렬 아님).
- 칩, 단축키, 선택 바 버튼은 추가하지 않습니다. 기존 단축키 매핑은 건드리지 않습니다.
- 그룹 기준(`groupBy`)에 "소리"를 넣는 것은 선택 사항입니다. 넣는다면 `groupKeyFor`에 한 줄입니다.
- CSV `exportItem`에 `audio_state`, `audio_max_db`, `audio_mean_db` 열을 추가합니다. 값은 측정값 그대로입니다.

### 2.7 보고서 문구 (측정한 것만 주장)

`techniques` 표의 "메타데이터" 칸(`report.rs:522-535`)과 영상 표(`:325`)에 "소리(측정값)"를 넣습니다:
- `소리 있음 — 측정: 최대 −12.3 dB, 평균 −38.0 dB (dBFS, ffmpeg volumedetect)`
- `무음 트랙 — 측정: 최대 −91.0 dB (기준: −70 dB 이하)`
- `오디오 트랙 없음 (ffprobe)` / `오디오 트랙 있음 — 소리 미측정` / `측정 후 파일 변경 — 미확인` / `미확인`

표 아래 고정 문구: "소리 측정은 오디오 트랙 전체의 신호 레벨(최대·평균, dBFS)만 잰 값입니다. 말소리나 대화가 있는지, 소리가 어디서 났는지는 판단하지 않습니다."

**"대화 있음", "음성 녹음됨" 같은 표현은 쓰지 않습니다.**

---

## §3 테스트

### Rust 단위 테스트 (`cargo test`)

**`rectime.rs`**
- fixture 동등성: 대시캠 `h/m/s` 형식, `_`/`-`/`T` 구분자, 날짜만 있는 경우, 범위 밖 값.
- 날짜만 있는 경우(`has_time=false`)는 정렬 근거로 쓰지 않는지.

**`join.rs`**
- 채널(F18 포팅): FRONT 폴더, `_R` 토큰, 미확인, 혼합.
- 정렬 근거 3종과 혼합 근거일 때 `filename-order`로 떨어지는지.
- 접합부 분류의 경계(정확히 ±1.0초 → continuous).
- `same-start`와 `duplicate-file` 차단.
- 스트림 불일치 매트릭스(각 필드 하나씩만 바꿔 보기).
- 컨테이너 선택.
- concat 목록: `'` 이스케이프, `\`를 `/`로, 한글, 개행 거부, `\\?\` 제거, BOM 없음.
- `ffmpeg_argv`가 모두 ASCII인지.
- 출력 내 위치(오프셋) 누적.
- plan_id가 같은 입력에 대해 안정적인지, size나 mtime이 바뀌면 달라지는지.
- `summary_text` 형식: 각 N초, 합계·최대, 1시간 이상, 채널 미확인.
- 로그 줄이 올바른 JSON이고 체인 검증을 통과하는지.

**`audio_level.rs`**
- stderr 파싱: 숫자, `-inf`, `n_samples: 0`, 행 누락, 다중 스트림.
- 기준값 경계: −70.0이면 silent, −69.9이면 sound.
- 디코드 오류는 silent가 아니라 error인지.
- 무효화: size, mtime, sha가 바뀌면 적용하지 않는지.
- 가장 최근 항목이 이기는지.
- 200개 단위 청크.

**`serve/tests_join.rs`(새 파일, 가짜 exe 주입)**
- check 입력 검증: 케이스 없음, id 2개 미만, 500개 초과, `vid_*` 아님, 모르는 id, busy.
- 등록 dedupe.
- clip과 공유하는 동시 실행 상한 4.
- plan_id 불일치 거부.
- 취소하면 `.part`, `.ffconcat`, `.progress.json`이 지워지고 상태가 `cancelled`인지.
- 실패하면 출력이 없는지.
- 완료되면 `clip_jobs[].kind=="join"`, `stage`가 노출되는지.
- join 실행 중 open-case, shutdown, case job(recover/carve)이 거부되는지.
- 잔여 `.join-*` 정리.
- `records-meta`의 `joinLog`/`audioLevels` 모양.
- `with_audio` 옵션이 `enrichment_plan`에 반영되는지(ffprobe가 꺼져 있으면 빠지는지).

**`package.rs`, `report.rs`, `qa.rs`**
- `artifacts/joined/*`는 포함하고 `.join-*`는 빼는지.
- `verify-package`가 통과하는지.
- 보고서에 join 섹션과 고정 문구, 소리 열 문구 5종이 나오는지.
- QA가 `join-log.jsonl`와 `audio-level-log.jsonl`을 레인으로 잡는지.

### 통합 테스트 (`FRAMETRACE_IT=1`, 실제 ffmpeg) — 새 파일 `tests/join_audio_it.rs`, `#[ignore]`

- **join 성공.** lavfi로 h264+aac 2초짜리 세그먼트 5개를 만듭니다. 폴더 이름은 `증거 '따옴표' 폴더/FRONT/`이고 파일명은 `20260910_120000_F.mp4`부터 시작합니다. 시각을 조정해 겹침 1곳(1.5초)과 끊김 1곳(3초)을 넣습니다. 확인할 것:
  - `join-video` 성공, 출력 SHA가 매니페스트·로그와 같음
  - ffprobe 길이가 합계와 같음(오차 범위 안)
  - 매니페스트에 `overlaps=1`, `gaps=1`
  - `verify-audit` 체인 통과
- **join 거부.** 640×360과 1280×720을 섞으면 `stream-mismatch`. 오디오가 있는 파일과 없는 파일을 섞으면 `audio-presence`.
- **취소.** 서버를 띄우고, 큰 세그먼트(각 30초, 고비트레이트)로 join을 시작한 뒤 바로 취소합니다. 출력, `.part`, 로그 항목이 모두 없어야 합니다.
- **소리.** `sine`(소리), `anullsrc`(무음 트랙), 오디오 없음, `sine,volume=-60dB`(소리, 기준값 근처)를 측정해 각 결과를 확인합니다.
- **소리 벤치(정보용).** 1분짜리 1080p 파일 100개의 측정 시간을 출력만 하고 실패시키지 않습니다. §2.2의 기본값 결정에 씁니다.
- **기존 e2e 확장.** `workstation_e2e`에 `with_audio=true`로 시작하는 경우를 추가하고, `audio-level-log.jsonl` 레인과 `records-meta.audioLevels`를 확인합니다.

### Playwright 하네스 (새 파일 2개, `ci-harnesses.sh` 맨 끝에 주석과 함께 추가)

**`scripts/verify-join.mjs`**: 격리 서버(`startIsolatedServer`)와 ffmpeg로 만든 fixture를 씁니다.
1. 선택이 1개일 때 메뉴 항목이 disabled이고 사유 `title`이 있는지.
2. 3개를 선택하면 대화상자 요약이 `/^전방 · 3개 · 0분 0\d초 · 끊김 0곳 \/ 겹침 1곳/`에 맞는지.
3. 실행하면 진행 행과 [중단]이 나오고, 완료 대화상자에 SHA-256이 64자 16진수로 나오는지. "새 데이터 반영" 후 `kind=joined` 카드가 나오는지.
4. 전방과 후방을 섞어 선택하면 primary가 disabled이고 차단 사유가 보이는지.
5. 취소 경로에서 토스트가 나오고 서버의 `.part`가 없는지.
6. 대화상자 안에서 `j`/`k`/`1`/`Space` 키가 그리드와 마크를 바꾸지 않는지.
7. 1366×768과 1920×1080에서 대화상자 버튼이 화면 안에 있고 다른 요소에 덮이지 않는지(`elementFromPoint`).
8. 두 테마에서 axe AA 검사를 통과하는지.

**`scripts/verify-audio-facet.mjs`**: 독립 `viewerPageHtml`에 합성 DATA(5개 상태 + `audioLevels`)를 넣어 씁니다.
1. 카드 아이콘의 `use href`와 `aria-label`이 상태별로 맞는지.
2. 상세 문구가 맞는지.
3. "소리" facet 개수가 자기 차원만 빼고 세어지는지(다른 facet을 고르면 바뀌고, 자기를 고르면 그대로).
4. 고르면 필터링되는지.
5. 날짜와 종류 facet이 그대로인지.
6. `j`/`k`/`x`/`1`/`0`이 그대로 동작하는지.
7. CSV에 소리 열이 있는지.
8. 메모 키 회귀: 정렬만 바꿨을 때 facet을 다시 계산하지 않는지.

**기존 하네스 갱신**
- `verify-i18n`: 새 키의 ko/en 쌍.
- `verify-facets`: 섹션 수.
- `verify-buttons`: 대화상자 안의 primary가 1개인지.
- `verify-embed`: 법원 뷰어 데이터 키 추가를 의도한 변경으로 확인.
- `check-a11y`.
- `capture-screens`: 새 화면 `05e-join-dialog`, `05f-join-blocked`, `04b-grid-audio-states`.

---

## §4 병렬 트랙 (worktree 분리, 파일 소유권이 겹치지 않음)

| 트랙 | 난이도 → 모델 | 담당 파일 (이 트랙만 수정) | 내용 |
|---|---|---|---|
| **T1 코어 (join·소리 측정·CLI)** | 어려움 → **Fable 선호**, 지금은 Opus | 새 파일: `src/rectime.rs`, `src/join.rs`, `src/audio_level.rs`, `src/cli/handlers/join_audio.rs`, `tests/fixtures/rectime-cases.json`, `tests/join_audio_it.rs`. 수정: `src/lib.rs`(mod 3줄), `src/cli/mod.rs`(서브커맨드 2개), `src/cli/handlers/mod.rs`(re-export) | §1.2~1.5, §2.1~2.3. ffmpeg concat 의미, 판정 매트릭스, 매니페스트, 체인 로그, 청크 로그, 체크포인트 |
| **T2 서버** | 보통 → Opus | `src/serve/media.rs`, `state.rs`, `mod.rs`, `jobs.rs`, `records.rs`, `http.rs`(필요하면 라우트 정책), 새 파일 `src/serve/tests_join.rs` | §1.6~1.7, `ClipJob.kind`로 일반화, 정리 재시도, 보강 단계 "audio"와 `with_audio`, case job `audio-measure`, `records-meta`의 `joinLog`/`audioLevels`(무효화 판단 포함) |
| **T3 보고서·패키지·법원 뷰어 데이터** | 단순~보통 → Sonnet | `src/report.rs`, `src/package.rs`, `src/html_report.rs`, `src/cli/handlers/case.rs`, `src/qa.rs`(테스트만) | §1.9, §2.7, 법원 뷰어 데이터에 `joinLog`/`audioLevels` 추가. 로그 fixture는 §1.5와 §2.3 스키마로 직접 만들어 T1 머지 전에도 테스트할 수 있게 합니다 |
| **T4 뷰어·셸 UI와 하네스** | 보통 → Opus | `assets/viewer/{20-i18n,30-records,40-grid,60-details,65-actions,80-review}.js`, `viewer.body.html`, `controls.css`, `assets/icons.svg`(새 아이콘 2개), `assets/app/{31-progress,32-jobs,50-start}.js`, `assets/examiner_app.html`(체크박스 한 줄, 고급 항목 한 줄), `assets/app/41-finalize.js`(고급 "소리 측정"), 새 파일 `scripts/verify-join.mjs`, `scripts/verify-audio-facet.mjs`, `scripts/ci-harnesses.sh`(끝에 2줄), `scripts/capture-screens.mjs` | §1.8, §2.5~2.6. 개발 중에는 API를 `page.route` 목업으로 대신합니다(JSON 모양은 §1.6과 §2.4) |
| 잡일 | Haiku | `docs/PLAN-v100.md`(§8 결과 섹션), `CHANGELOG.md`, `docs/SCREENSHOTS.md`, `docs/RECOVERY_BOUNDARIES.md`(join 경계 한 줄) | 머지 뒤 |

계약 고정점: §1.4 매니페스트, §1.5 로그, §1.6 API JSON, §2.3 로그, §2.4 `audioLevels` 맵. 바꾸려면 오케스트레이터 승인을 받습니다.

---

## §5 머지 순서와 게이트

1. **T1** → 2. **T2** → 3. **T3** → 4. **T4** → 5. 잡일.
   - T3은 fixture 기반이라 T1 바로 다음에 머지할 수 있습니다.
   - T4는 T2 뒤에 실제 서버로 하네스를 돌립니다.
2. **머지할 때마다**: `cargo fmt --check` · `cargo clippy --locked --all-targets -- -D warnings` · `cargo test --locked` · `node scripts/check-js.mjs && node scripts/check-css.mjs`.
3. **T4 머지 후 전체 게이트**:
   - `cargo build --locked && FRAMETRACE_BIN=… bash scripts/ci-harnesses.sh`(기존 항목 + 새 2개)
   - `FRAMETRACE_IT=1 cargo test --locked --test workstation_e2e -- --ignored`
   - `FRAMETRACE_IT=1 cargo test --locked --test join_audio_it -- --ignored`
   - `node scripts/verify-embed.mjs`(법원 뷰어 데이터 변경이 의도한 것인지 확인)
   - `capture-screens`와 `visual-diff`.
4. **Windows** (Git Bash, `frametrace-app.exe`):
   - 같은 하네스 전체와 두 IT 테스트.
   - 직접 확인: 한글·공백·`'`·260자 넘는 원본 경로로 join.
   - 취소할 때 ffmpeg 프로세스 트리가 Job 객체로 정리되고 `.part`가 지워지는지(재시도 경로 포함).
   - `\\?\` 경로가 들어오는 경우.
   - `cfg(windows)` 컴파일(Mac에서 바꿨다면 draft PR로 Windows CI를 먼저 통과).
5. **적대적 리뷰**(Fable 선호, 지금은 Opus) → 수정 → 3·4번 재실행.

---

## §6 적대적 리뷰 관점

1. **실패·취소 후 잔여물.** ffmpeg가 죽은 뒤 rename, 매니페스트, 로그 사이 어디서든 실패하면 출력·매니페스트·로그가 서로 안 맞는 상태가 남지 않는가? 로그 append가 실패했는데 출력이 남아 있지는 않은가?
2. **TOCTOU.** 검사 → join → 해시 → ffmpeg 읽기 사이에 원본이 바뀌면? 다시 stat하는 것으로 충분한가? 해시는 ffmpeg 전에 계산하므로 같은 바이트를 읽었다는 보증은 size와 mtime뿐입니다. 이 한계를 매니페스트 caveat에 적어야 하는가?
3. **concat 정확성.** concat demuxer가 실제로 쓰는 다음 파일 시작 위치(포맷 길이 vs 스트림 끝)와 매니페스트 오프셋의 차이. A/V 길이가 다른 세그먼트에서 생기는 누적 어긋남. `extradata_hash`가 없는 ffprobe 버전에서의 동작.
4. **argv 비ASCII 금지가 정말 지켜지는가**(F2). `cwd`를 바꿨을 때 `-safe 0`과 절대 경로의 상호작용.
5. **시간 해석의 주장 수준.** naive local 시각을 UTC처럼 보이게 출력하지 않는가? JS와 Rust의 파서 동등성(특히 `name-date`를 정렬 근거로 쓰지 않는 점).
6. **채널 판정의 과신.** 모두 미확인이면 허용하는데, 이걸로 다른 카메라 영상이 섞일 수 있습니다. 스트림 파라미터가 같은 전방·후방(같은 해상도)이면?
7. **잡 공유의 부작용.** `clip_jobs`를 같이 쓰면서 dedupe 키 충돌, 전역 취소가 join도 죽이는 것이 의도한 동작인가, 작업 시트·verify-progress의 문구, `KEPT_FINISHED_CLIP_JOBS` 정리에서 join이 밀려나는지.
8. **소리 판정에서 디코드 실패를 무음으로 보는 오분류**(`n_samples==0`, 타임아웃, 특정 코덱 디코더 없음). 다중 오디오 스트림. 측정 후 변경된 파일 처리(해시가 없는 fast 모드에서는 size와 mtime만 봄).
9. **facet 메모 키**(F23 `slice(0,7)`). 새 차원이 메모 무효화에서 빠지거나, 정렬 변경이 facet을 다시 계산하게 만드는지.
10. **로그 성능**(F25). 청크 단위가 정말 지켜지는가? join 로그는 건수가 적어 문제없음. QA 레인의 빈 로그 규칙(측정을 하나도 안 했을 때 파일을 만들지 않는지).
11. **패키지 범위.** `.join-*` 제외가 확실한지. 매니페스트의 절대 경로가 redact 모드에서 처리되는지. 지금 `case.rs`는 로그만 redact하는데, 사이드카 매니페스트 원문이 그대로 패키지에 들어갑니다.
12. **UI 규칙.** 대화상자 영역의 primary는 1개, 차단 상태에서 disabled와 `title`, 빨강은 실패 의미로만, 1366×768에서 세그먼트 목록이 넘치지 않는지, 단축키 누수.
13. **법원 뷰어 데이터 크기**(`audioLevels` 10만 건)와 `verify-embed` 동등성.

---

## §7 소유자 확인 질문 (필수 항목만)

1. **재인코딩 join은 v1 범위 밖**으로 둘까요? (기본: 범위 밖. 파라미터가 다르면 거부하고, 필요하면 개별 클립 내보내기를 씁니다.)
2. **소리 측정 기본값**: 시작 옵션에서 꺼둘까요, 켜둘까요? (기본: **꺼짐** + 고급에서 나중에 실행. IT 벤치 뒤에 다시 판단.) 그리고 일부 세그먼트에만 오디오가 있을 때 **"오디오 빼고 이어붙이기"(`-an`, 영상은 무손실, 모든 기록에 표시)** 옵션을 넣을까요? (기본: 넣지 않음.)
3. **facet 이름**: "음성"은 말소리로 읽힐 수 있어 측정 범위를 넘는 주장이 됩니다. **"소리"** 로 해도 될까요? (기본: "소리". 기존 상세 라벨 "음성"도 "소리"로 맞춥니다.)
4. **허용 오차 기본값 1.0초**(요청은 0.5초)로 해도 될까요? 파일명 시각은 초 단위라 0.5초면 정상 연속을 잘못 판정합니다.
5. **채널 인식 확장**(BlackVue `_NF/_NR`, `전방/후방/실내` 폴더 이름)을 JS와 Rust에 같이 넣을까요? 그리드 채널 표시와 쌍 재생(`pairKeyOf`, verify-dual)에도 영향이 있습니다. (기본: v1은 기존 규칙을 그대로 옮기고, 확장은 별도 작업.)

---

## §8 post-review changes (백엔드, 2026-10-13 적대적 리뷰 반영)

계약 변경만 적습니다. 기존 필드는 그대로이고, 아래는 추가되거나 의미가 바뀐 것입니다.

**join-check 응답 (`plan`)**
- `totals.unknown_junctions`(정수): 판정하지 못한 접합부 수. `filename-order`이면 모든 접합부입니다(B2).
- 새 차단 코드 `path-not-utf8`: 경로를 UTF-8로 나타낼 수 없는 원본(B7). concat 목록의 `\` → `/` 변환은 Windows에서만 합니다.
- 새 경고 코드:
  - `extra-video-streams`: "추가 영상 스트림 N개는 포함되지 않습니다"(B4)
  - `data-streams-dropped`: "데이터·자막 스트림 N개(GPS 등)는 포함되지 않습니다"(B4)
  - `extradata-unavailable`: 모든 세그먼트에 SPS/PPS 해시가 없을 때. 이때 `level`·`refs`·`has_b_frames`·`field_order`·색 정보·`is_avc`·`nal_length_size`를 비교하고, 다르면 `stream-mismatch`로 **차단**합니다(B6). 예전에는 이 상황도 `hash-unavailable`이었습니다.
  - `hash-unavailable`은 이제 "색인 해시가 없는 원본"이라는 한 가지 뜻만 가집니다(B15, 뷰어 `JOIN_CODE_TEXT`와 일치).
- `stream-mismatch` 비교 항목 추가: 오디오 `profile`, 오디오 `extradata_hash`(AAC ASC 등, 어느 하나라도 값이 있을 때)(B5).

**매니페스트 (`schema_version` 1 유지)**
- 최상위에 `record_id`, `job_tag`를 추가했습니다.
- `totals.unknown_junctions`를 추가했습니다. `filename-order`이면 `totals.gaps`/`overlaps`/`gap_seconds`/`overlap_seconds`는 **null**입니다(0이 아님).

**join-log 항목**
- `record_id`: `"join:" + output_sha256 앞 16자(소문자 hex)`입니다. 파일 이름과 무관하고 바이트에서 나옵니다(U2).
- `unknown_junctions`를 추가했습니다. `filename-order`이면 `gaps`/`overlaps`는 null입니다.
- `records-meta.joinLog`와 법원 뷰어 데이터(`data-bundle.js`의 `joinLog`), 보고서 입력의 모든 항목에 `record_id`가 들어갑니다. 예전 항목은 `output_sha256`으로 계산해 채웁니다.
- **뷰어(30-records.js)는 이어붙인 레코드 id로 `entry.record_id`를 써야 합니다.** 보고서는 `join:<출력파일명>` 예전 id에 붙은 주석도 읽어 줍니다.

**임시 파일**
- `artifacts/joined/` 안에 `.join-<tag>.claim.json`과 `.join-<tag>.lock`이 추가됐습니다.
  - `claim.json`에는 게시된 `{output, manifest, job_tag}`가 들어갑니다.
  - `lock`은 실행하는 동안 배타 잠금을 잡습니다. 같은 태그가 이미 실행 중이면 CLI가 거부합니다.
- 실패나 취소 뒤 정리는 claim에 적힌 파일만 지웁니다. 같은 계획의 다른 잡이나 CLI join이 만든 결과물은 건드리지 않습니다(B8).
- 남은 파일 정리는 잠금이 풀린 lock 파일이 있는 태그만 바로 지웁니다. lock 파일이 없으면 모든 파일이 1시간 넘게 지났을 때만 지웁니다. 다른 프로세스가 잠금을 잡고 있으면 지우지 않습니다(B13).
- FAT/exFAT처럼 하드 링크가 없으면 `create_new`로 복사하고, 이름이 이미 있으면 다음 `__N` 이름을 씁니다(B14).

**소리 측정**
- ffmpeg 인자에 `-xerror`를 추가했습니다. 아래 경우는 `error`이고, 소리 있음·무음으로 판정하지 않습니다(B1).
  - stderr에 디코드 오류가 있을 때. 예: `partial file`, `Error while decoding`
  - 디코드된 길이(`measured_seconds`)가 트랙 길이보다 짧을 때
    - 스트림 길이를 알면: 1초 넘게, 또는 0.25초 넘게 짧으면서 95% 미만일 때
    - 컨테이너 길이만 알면: max(2초, 5%) 넘게 짧을 때
- 측정 전후로 파일을 다시 stat합니다. 크기나 mtime이 색인과 다르면 `error`이고 오류 문구는 `changed:`로 시작합니다(B3).
- 색인에 스트림 목록 없이 `audio_codec`만 있으면 먼저 실제 ffprobe로 오디오 트랙 수와 파라미터를 확인합니다. 확인하지 못하면 `error`입니다(B12).
- 체크포인트 지문에 ffmpeg 버전이 들어갑니다. 체크포인트에서 재생한 결과는 별도 로그 항목으로 남깁니다(B11).
  - 항목 수준: `replayed_from_checkpoint: true`, `logged_unix`, `measured_unix`(원래 측정 시각 중 가장 이른 값)
  - 결과마다 `measured_unix`를 남깁니다. 읽는 쪽은 결과 수준 값을 우선합니다.
- 로그 append는 이제 로그 끝부분만 읽습니다. 모든 체인 레인에 적용되고, 체인 검증은 그대로입니다(B10).
- **`measure-audio` 종료 코드(B15)**: 오류가 없으면 0입니다.
  - 일부(또는 전부) 측정하지 못했으면 **4**입니다. 이때도 모든 결과는 로그에 남고, stdout 요약 JSON은 그대로이며, stderr 마지막 줄은 `소리 측정 오류 N건 (측정 대상 M건)`입니다.
  - 그 밖의 0이 아닌 코드는 실행 실패입니다.
  - 서버는 종료 코드 4를 실패로 보지 않습니다. 보강 단계에서는 경고 "소리 측정 완료(오류 N건) — …"를 남기고, `audio-measure` 잡에서는 같은 로그를 남긴 뒤 리뷰를 다시 만듭니다. 성공하면 "소리 측정 완료: 소리 있음 · 무음 · 오류 · …" 집계 줄을 남깁니다.
- `records-meta.audioLevels`: 공유 인덱스 캐시에서 계산하고, (레인 stamp, 인덱스 세대, videos.jsonl stamp)를 키로 캐시합니다(B9). 모양은 같습니다.

**보고서**
- "이어붙인 영상" 표에 `판독 (상태·메모·구간·태그)` 열을 추가했습니다. `record_id` 기준이고, 예전 `join:<파일명>`도 읽습니다(U2).
- `filename-order` 행은 "끊김·겹침 확인 불가 — 파일명 순"으로 표시합니다.
- 주의 문구는 순서 근거에 따라 바뀝니다(`join-caveat-time` / `join-caveat-order`).

**서버 문구**
- 최종화와 case job 거부: "내보내기·이어붙이기 작업이 진행 중입니다 — 끝난 뒤 다시 시도하십시오."
- open-case 거부와 전역 취소 로그도 "내보내기·이어붙이기"로 바꿨습니다.
- 끝난 join을 취소하려 하면 "이미 끝난 이어붙이기입니다."를 표시합니다.
