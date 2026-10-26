use crate::util::json_for_script;

pub struct ReportInputs<'a> {
    pub manifest_json: &'a str,
    pub index_json: &'a str,
    pub export_log_jsonl: &'a str,
    pub proxy_log_jsonl: &'a str,
    pub thumbnail_log_jsonl: &'a str,
    pub carve_log_jsonl: &'a str,
    pub filesystem_log_jsonl: &'a str,
    pub validation_log_jsonl: &'a str,
    pub anomaly_log_jsonl: &'a str,
    pub batch_log_jsonl: &'a str,
    /// `artifacts/joined/join-log.jsonl` (plan §1.5): one entry per lossless
    /// join. Empty when the case has none.
    pub join_log_jsonl: &'a str,
    /// Compact measured-sound map `{id: {r, max, mean, sec, t}}` (plan §2.4),
    /// already bound to the current bytes by the caller. Empty or `{}` when
    /// nothing was measured. Display only — never digested.
    pub audio_levels_json: &'a str,
    pub scan_runs_json: &'a str,
    /// Review annotations shown in the "판독" column: the
    /// `report_annotations_json` object (`{"marks":[...],"tags":{...}}`,
    /// marks with IN-OUT ranges). A bare legacy marks array is still
    /// accepted (no ranges/tags then). Display only — never digested.
    pub marks_json: &'a str,
    /// True when the caller already redacted absolute paths in every input;
    /// the report then shows the redaction notice instead of silently
    /// presenting rewritten paths as real ones.
    pub redaction_applied: bool,
    /// D9: review-state warnings shown at the top of the report (marks
    /// still queued, unresolved save conflicts), as a JSON array of
    /// strings. Empty or `[]` when there are none.
    pub review_warnings_json: &'a str,
    /// D11: inputs that could not be read, `{name: error}` (names as in the
    /// chain map plus `scan_runs` / `audio`). A section whose input failed
    /// says so instead of "없음". Empty or `{}` when everything was read.
    pub read_errors_json: &'a str,
    /// Telemetry extracts as `{name: {selector, source, point_count,
    /// total_points, truncated, first_ts_unix, last_ts_unix, note}}` (points
    /// stripped). E9: a track cut at the point cap says so. Empty or `{}`
    /// when the case has none.
    pub telemetry_json: &'a str,
}

/// Renders the report without audit-chain verification results: every log
/// column then reads "체인 미검증" (I9). `make-report` uses
/// [`render_case_report_with_chains`].
pub fn render_case_report(inputs: &ReportInputs<'_>) -> String {
    render_case_report_with_chains(inputs, "{}")
}

/// [`render_case_report`] with the chain verification of each audit log the
/// report quotes (I9): `{"export"|"proxy"|"thumbnail"|"carve"|"filesystem"|
/// "validation"|"anomaly"|"batch"|"join": {"path", "status":
/// "keyed"|"structural"|"not-verified"|"failed"|"absent", "entries",
/// "detail"}}` (R7-I3: "not-verified" = `rewrite_not_excluded`). A log
/// missing from the map is shown as not verified — the report never calls a
/// chain hash tamper-evident unless that log passed keyed verification.
pub fn render_case_report_with_chains(
    inputs: &ReportInputs<'_>,
    audit_chains_json: &str,
) -> String {
    let audit_chains = json_for_script(&audio_levels_object(audit_chains_json));
    let manifest = json_for_script(inputs.manifest_json);
    let index = json_for_script(inputs.index_json);
    let export_lines = json_for_script(&jsonl_to_array(inputs.export_log_jsonl));
    let proxy_lines = json_for_script(&jsonl_to_array(inputs.proxy_log_jsonl));
    let thumbnail_lines = json_for_script(&jsonl_to_array(inputs.thumbnail_log_jsonl));
    let carve_lines = json_for_script(&jsonl_to_array(inputs.carve_log_jsonl));
    let filesystem_lines = json_for_script(&jsonl_to_array(inputs.filesystem_log_jsonl));
    let validation_lines = json_for_script(&jsonl_to_array(inputs.validation_log_jsonl));
    let anomaly_lines = json_for_script(&jsonl_to_array(inputs.anomaly_log_jsonl));
    let batch_lines = json_for_script(&jsonl_to_array(inputs.batch_log_jsonl));
    let join_lines = json_for_script(&jsonl_to_array(inputs.join_log_jsonl));
    let audio_levels = json_for_script(&audio_levels_object(inputs.audio_levels_json));
    let scan_runs = json_for_script(inputs.scan_runs_json);
    let marks = json_for_script(inputs.marks_json);
    let review_warnings = json_for_script(&json_array_or_empty(inputs.review_warnings_json));
    let read_errors = json_for_script(&audio_levels_object(inputs.read_errors_json));
    let telemetry = json_for_script(&audio_levels_object(inputs.telemetry_json));
    let redaction_note = if inputs.redaction_applied {
        format!(
            "  {}\n  <div class=\"note\">{}</div>\n",
            crate::redact::REDACTION_MARKER,
            crate::util::html_escape(crate::redact::REDACTION_NOTE)
        )
    } else {
        String::new()
    };
    format!(
        r#"<!doctype html>
<html lang="ko">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>FrameTrace 영상 포렌식 보고서</title>
  <style>
    body {{
      margin: 0;
      font-family: "Segoe UI", Arial, sans-serif;
      color: #202936;
      background: #ffffff;
      line-height: 1.45;
    }}
    main {{
      max-width: 1120px;
      margin: 0 auto;
      padding: 32px 28px 48px;
    }}
    h1 {{
      font-size: 28px;
      margin: 0 0 8px;
      letter-spacing: 0;
    }}
    h2 {{
      font-size: 18px;
      margin: 30px 0 10px;
      padding-bottom: 6px;
      border-bottom: 1px solid #d7dde6;
      letter-spacing: 0;
    }}
    .muted {{
      color: #657286;
      font-size: 13px;
    }}
    .summary {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
      gap: 10px;
      margin: 22px 0;
    }}
    .box {{
      border: 1px solid #d7dde6;
      border-radius: 8px;
      padding: 12px;
      background: #f8fafc;
    }}
    .box strong {{
      display: block;
      font-size: 20px;
      margin-top: 4px;
    }}
    table {{
      width: 100%;
      border-collapse: collapse;
      border: 1px solid #d7dde6;
      margin-top: 10px;
    }}
    th, td {{
      border-bottom: 1px solid #e8edf3;
      padding: 8px 10px;
      text-align: left;
      vertical-align: top;
      font-size: 12px;
    }}
    th {{
      background: #eef2f7;
      font-size: 12px;
    }}
    code {{
      font-family: Consolas, "SFMono-Regular", monospace;
      word-break: break-all;
      font-size: 11px;
    }}
    .note {{
      border-left: 4px solid #3b82f6;
      background: #f5f8ff;
      padding: 10px 12px;
      margin-top: 12px;
      font-size: 13px;
    }}
    .print-bar {{
      position: sticky;
      top: 0;
      z-index: 10;
      display: flex;
      justify-content: flex-end;
      gap: 8px;
      background: #ffffffee;
      padding: 8px 0;
    }}
    .print-bar button {{
      font: inherit;
      border: 1px solid #b8c2d0;
      background: #f4f7fb;
      border-radius: 6px;
      padding: 6px 14px;
      cursor: pointer;
    }}
    @media print {{
      main {{ padding: 0; }}
      .print-bar {{ display: none; }}
      .box {{ break-inside: avoid; }}
      table {{ break-inside: auto; }}
      tr {{ break-inside: avoid; }}
      thead {{ display: table-header-group; }}
      h2 {{ break-after: avoid; }}
    }}
  </style>
</head>
<body>
<div class="print-bar">
  <button id="print-button" type="button">인쇄 / PDF로 저장</button>
</div>
<main>
  <h1 id="title">FrameTrace 영상 포렌식 보고서</h1>
  <div class="muted" id="case-line"></div>
  <div class="muted" id="time-basis">시각 표기: 이 보고서의 모든 시각은 UTC 기준입니다(보관 이력서 custody-statement와 같은 기준). 괄호 안은 이 보고서를 연 PC의 현지 시각과 그 UTC 오프셋입니다. 파일 수정 시각 등 장치·파일시스템이 기록한 시각은 그 장치 시계를 따르며 정확성은 검증하지 않았습니다.</div>
{redaction_note}
  <div id="review-warnings"></div>
  <div id="read-errors"></div>
  <section class="summary">
    <div class="box">색인 영상<strong id="count">0</strong></div>
    <div class="box">증거 총 용량<strong id="bytes">0</strong></div>
    <div class="box">ffprobe 확인<strong id="confirmed">0</strong></div>
    <div class="box">추정 소스<strong id="sources">0</strong></div>
    <div class="box">스캔 경고<strong id="warnings-count">0</strong></div>
    <div class="box">내보낸 클립<strong id="exports">0</strong></div>
    <div class="box">이어붙임<strong id="joins">0</strong></div>
    <div class="box">리뷰 산출물<strong id="derived">0</strong></div>
    <div class="box">복구 후보<strong id="carved">0</strong></div>
    <div class="box">파일시스템 작업<strong id="filesystem-actions">0</strong></div>
    <div class="box">검증 기록<strong id="validations">0</strong></div>
  </section>

  <h2>처리 요약</h2>
  <div id="processing"></div>
  <div class="note">이 보고서는 원본 증거 참조와 파생 리뷰/내보내기 산출물을 분리합니다. carving 또는 파일시스템 복구 결과는 별도 재생/컨테이너 검증 전까지 복구 후보로 취급합니다.</div>

  <h2>소스 / 파서 평가</h2>
  <div id="source-assessment"></div>

  <h2>발견 및 분석 기법 (증거별 명세)</h2>
  <div id="techniques"></div>
  <div class="note">각 증거가 어떤 기법으로 발견되었고 어떤 검증을 통과했는지 정리합니다. 이상 징후 후보는 아래 전용 섹션과 <code>qa anomalies</code> 로그를 참고하십시오. 재생성 검증을 통과한 증거도 최종 보고 전 판독자 재생 확인이 필요합니다. 판독 마크는 뷰어에서 내려받아 <code>import-marks</code>로 반영한 뒤 보고서를 재생성하면 함께 정리됩니다.</div>
  <div class="note" id="mark-legend">판독 마크 범례: 판독완료 = 회색 체크 · 중요 = 호박색 깃발 · 검증대기 = 보라 물음표</div>

  <h2>영상 색인</h2>
  <div id="videos"></div>
  <div class="note" id="audio-caveat">소리 측정은 오디오 트랙 전체의 신호 레벨(최대·평균, dBFS)만 잰 값입니다. 말소리나 대화가 있는지, 소리가 어디서 났는지는 판단하지 않습니다.</div>

  <h2>MP4/AVI 내보내기 산출물</h2>
  <div id="clip-exports"></div>

  <h2>이어붙인 영상</h2>
  <div id="joined-videos"></div>
  <div class="note" id="join-caveat" hidden>원본 영상 프레임을 재인코딩 없이 순서대로 연결(스트림 복사)했습니다. 겹친 구간은 잘라내지 않아 같은 장면이 두 번 나올 수 있고, 끊긴 구간은 채우지 않았습니다. <span id="join-caveat-time">순서와 끊김·겹침 판단은 장치가 기록한 시각을 근거로 하며, 장치 시계의 정확성은 검증하지 않았습니다. </span><span id="join-caveat-order" hidden>순서 근거가 파일명 순(filename-order)인 항목은 녹화 시각 없이 파일 이름 순서로만 이었으므로 끊김·겹침을 확인하지 못했습니다. </span>위치·데이터 스트림(GPS 등)은 포함되지 않았습니다.</div>

  <h2>위치·텔레메트리 추출</h2>
  <div id="telemetry"></div>

  <h2>리뷰 산출물</h2>
  <div id="derived-artifacts"></div>

  <h2>복구 / Carving 후보</h2>
  <div id="carved-artifacts"></div>

  <h2>재생 / 컨테이너 검증</h2>
  <div id="validation-results"></div>
  <div id="validation-history"></div>

  <h2>이상 징후 후보 (candidate-finding)</h2>
  <div id="anomaly-findings"></div>
  <div id="anomaly-history"></div>
  <div class="note">자동 플래그는 수동 확인이 필요한 후보 소견입니다. 조작·위변조·법적 증명으로 해석하지 마십시오. <code>qa anomalies</code>로 재생성합니다.</div>

  <h2>파일시스템 조사 / Inode 복구</h2>
  <div id="filesystem-recovery"></div>

  <h2>처리 체인 (분석 타임라인)</h2>
  <div id="chain"></div>
  <div class="note">스캔·검증·내보내기·복구 이벤트를 시간순으로 정렬한 처리 이력입니다. 체인 해시는 감사 로그 항목을 앞 항목과 잇는 해시 값이며, 각 로그를 보고서 생성 시 다시 검증한 결과는 아래 표에 있습니다.</div>

  <h2>감사 로그 체인 검증</h2>
  <div id="unauthenticated-entries"></div>
  <div id="audit-chains"></div>
  <div class="note" id="audit-chain-note">integrity-keyed = 모든 항목이 설정된 키(HMAC)로 확인됨 — 키 없이 고친 항목은 드러납니다. 구조만 확인 = 항목 연결은 맞지만 서명이 없어, 파일에 접근할 수 있는 사람이 로그 전체를 다시 쓰면 탐지할 수 없습니다. 검증 실패 = 연결이 끊김 — 해당 로그의 내용을 그대로 믿으면 안 됩니다. 체인 검증만으로는 로그 끝부분을 항목 단위로 잘라낸 것(잘림)을 탐지하지 못합니다 — 잘림은 최종화 서명(v4)에 기록된 레인별 항목 수·마지막 해시(앵커)와 비교해야 드러나며, 그 결과는 위 검토자 줄에 표시됩니다.</div>
<script>
const auditChains = {audit_chains};
// R8-S4: an unreadable case.json / db/video_index.json is embedded as
// null (R6-S1); `manifest.title` then threw and the report stayed blank.
const objectOrNull = value => value && typeof value === "object" && !Array.isArray(value) ? value : null;
const manifestInput = objectOrNull({manifest});
const scanInput = objectOrNull({index});
const unreadableInputs = [[manifestInput, "케이스 파일(case.json)"], [scanInput, "색인 파일(db/video_index.json)"]].filter(([value]) => !value).map(([, name]) => name);
const manifest = manifestInput || {{}};
const scan = scanInput || {{}};
// R9 P3: every log entry carries `trust` ("auth" | "sc" | "unauth" |
// "pc:<key id>", from audit::read_lane_rows). Only "auth"/"sc" entries are
// current results; the rest are listed separately as "인증 안 됨" and never
// counted, confirmed or derived from. (An entry without `trust` comes from
// a caller that did not classify it and is shown as before.)
const usableEntry = item => !item || typeof item !== "object" || item.trust === undefined || item.trust === "auth" || item.trust === "sc";
// R13 I2: a usable `lane-repair` marker (frametrace repair-lane) is not a
// record of the lane's own kind — the chain shows it as "수리됨". Every
// "latest record" reader treats what sits BEFORE it as unknown (the cut
// tail may have held a newer record).
const isLaneRepair = item => !!item && typeof item === "object" && item.event === "lane-repair" && usableEntry(item);
const unauthenticatedEntries = [];
const usableLog = (name, rows) => (Array.isArray(rows) ? rows : []).filter(item => {{
  if (isLaneRepair(item)) return false;
  if (usableEntry(item)) return true;
  unauthenticatedEntries.push({{ name, item }});
  return false;
}});
const exportsLog = usableLog("export", {export_lines});
const proxyLog = usableLog("proxy", {proxy_lines});
const thumbnailLog = usableLog("thumbnail", {thumbnail_lines});
const carveLog = usableLog("carve", {carve_lines});
const filesystemLog = usableLog("filesystem", {filesystem_lines});
const validationAll = (rows => Array.isArray(rows) ? rows : [])({validation_lines});
const validationLog = usableLog("validation", validationAll);
// R13 I2: lane position of the last usable repair marker (-1: none).
const validationRepairAt = validationAll.map(isLaneRepair).lastIndexOf(true);
const validationPosition = new Map(validationAll.map((item, at) => [item, at]));
const anomalyAll = (rows => Array.isArray(rows) ? rows : [])({anomaly_lines});
const anomalyLog = usableLog("anomaly", anomalyAll);
// R14 P20: lane position of every entry — runs are judged by span.
const anomalyPosition = new Map(anomalyAll.map((item, at) => [item, at]));
// R11-I2 (P9): an anomaly entry this PC cannot use AFTER the newest usable
// run marker (e.g. the newest run's marker HMAC stripped) means the newest
// run's outcome is unknown — no older run stands in as "current".
const usableAnomalyMarkers = anomalyAll.map((item, index) => item?.event === "anomaly-scan-run" && usableEntry(item) ? index : -1).filter(index => index >= 0);
const lastUsableAnomalyMarker = usableAnomalyMarkers.length ? usableAnomalyMarkers[usableAnomalyMarkers.length - 1] : -1;
const anomalyUnusableAfter = anomalyAll.some((item, index) => index > lastUsableAnomalyMarker && !usableEntry(item));
const previousUsableAnomalyMarker = usableAnomalyMarkers.length > 1 ? usableAnomalyMarkers[usableAnomalyMarkers.length - 2] : -1;
// R13 I2: a repair marker after it — the cut tail may have held a newer run.
const anomalyRepairedAfter = anomalyAll.some((item, index) => index > lastUsableAnomalyMarker && isLaneRepair(item));
const batchLog = usableLog("batch", {batch_lines});
const joinLog = usableLog("join", {join_lines});
const audioLevels = {audio_levels};
const scanRuns = {scan_runs};
const annotations = {marks};
const reviewWarnings = {review_warnings};
const readErrors = {read_errors};
const telemetry = {telemetry};
const redaction = {{"applied": {}}};
const videos = Array.isArray(scan.videos) ? scan.videos : [];
const warnings = Array.isArray(scan.warnings) ? scan.warnings : [];
const derivedLog = [...proxyLog, ...thumbnailLog];

// B5: no inline handlers — the page qualifies for a strict script-src.
document.getElementById("print-button").addEventListener?.("click", () => window.print());
const escapeHtml = value => String(value ?? "")
  .replaceAll("&", "&amp;")
  .replaceAll("<", "&lt;")
  .replaceAll(">", "&gt;")
  .replaceAll('"', "&quot;")
  .replaceAll("'", "&#39;");

const fmtBytes = value => {{
  if (!Number.isFinite(value)) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let current = value;
  let unit = 0;
  while (current >= 1024 && unit < units.length - 1) {{
    current /= 1024;
    unit += 1;
  }}
  return `${{current.toFixed(unit === 0 ? 0 : 1)}} ${{units[unit]}}`;
}};

const fmtDuration = value => {{
  if (!Number.isFinite(value)) return "-";
  const seconds = Math.round(value);
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  return h ? `${{h}}:${{String(m).padStart(2, "0")}}:${{String(s).padStart(2, "0")}}` : `${{m}}:${{String(s).padStart(2, "0")}}`;
}};

// I6: times are shown in UTC (the custody statement's basis) with the
// viewing PC's local time and its UTC offset spelled out, never as a bare
// browser-local string whose time zone the reader cannot see.
const pad2 = value => String(value).padStart(2, "0");
const fmtOffset = date => {{
  const minutes = -date.getTimezoneOffset();
  const abs = Math.abs(minutes);
  return `UTC${{minutes < 0 ? "-" : "+"}}${{pad2(Math.floor(abs / 60))}}:${{pad2(abs % 60)}}`;
}};
const fmtUnix = value => {{
  if (typeof value !== "number" || !Number.isFinite(value)) return "-";
  const date = new Date(value * 1000);
  if (Number.isNaN(date.getTime())) return "-";
  const utc = `${{date.getUTCFullYear()}}-${{pad2(date.getUTCMonth() + 1)}}-${{pad2(date.getUTCDate())}} ${{pad2(date.getUTCHours())}}:${{pad2(date.getUTCMinutes())}}:${{pad2(date.getUTCSeconds())}} UTC`;
  const local = `${{date.getFullYear()}}-${{pad2(date.getMonth() + 1)}}-${{pad2(date.getDate())}} ${{pad2(date.getHours())}}:${{pad2(date.getMinutes())}}:${{pad2(date.getSeconds())}}`;
  return `${{utc}} <span class="muted">(현지 ${{local}} ${{fmtOffset(date)}})</span>`;
}};

// I9: what re-verifying each quoted audit log at report time found.
// R9-I7: the reason is the one the reader gave (`detail` — chain break,
// unreadable key configuration (not tampering), unauthenticated entries,
// another PC's key), never one fixed sentence for every cause.
const chainLabel = name => {{
  const chain = auditChains[name];
  const why = chain?.detail ? ` — ${{chain.detail}}` : "";
  switch (chain?.status) {{
    case "keyed": return "체인 검증: integrity-keyed (HMAC 키로 확인)";
    case "structural": return `체인 검증: 구조만 확인 — 자기 일관성만 확인(인증 아님)${{why}}`;
    case "repaired": return `체인 검증: 수리됨 — frametrace repair-lane으로 검증되는 앞부분까지 잘라 냄 (${{chain.base_status === "keyed" ? "integrity-keyed" : "구조만 확인 — 자기 일관성만 확인(인증 아님)"}}; 잘린 부분의 기록은 읽지 않음)${{why}}`;
    case "failed": return `체인 검증 실패 — 이 로그를 그대로 믿으면 안 됨${{why}}`;
    case "not-verified": return `체인 확인 불가 (NOT VERIFIED)${{why}}`;
    case "absent": return "로그 없음";
    default: return "체인 미검증";
  }}
}};
const chainCell = (name, hash) => `<code>${{escapeHtml(hash || "-")}}</code><br><span class="muted">${{escapeHtml(chainLabel(name))}}</span>`;

// D11: an input that could not be read is said to be unreadable — never
// rendered as an empty "없음" section.
const readFailed = name => readErrors[name] ? `<p class="note"><strong>읽기 실패 — 이 항목은 확인되지 않았습니다:</strong> ${{escapeHtml(readErrors[name])}}</p>` : "";
const emptyOr = (name, text) => readErrors[name] ? readFailed(name) : text;
document.getElementById("read-errors").innerHTML = (unreadableInputs.length ? `<div class="note" role="alert"><strong>${{unreadableInputs.map(escapeHtml).join(", ")}}을(를) 읽지 못했습니다</strong> — 이 보고서의 케이스 정보·영상 목록은 비어 있는 것이 아니라 확인되지 않은 것입니다. 파일을 확인한 뒤 보고서를 다시 만드십시오.</div>` : "") + (Object.keys(readErrors).length ? `<div class="note"><strong>보고서 생성 중 읽지 못한 입력 ${{Object.keys(readErrors).length}}건</strong> — 해당 표는 비어 있는 것이 아니라 확인되지 않은 것입니다.<br>${{Object.entries(readErrors).map(([name, err]) => `${{escapeHtml(name)}}: ${{escapeHtml(err)}}`).join("<br>")}}</div>` : "");
// D9: marks still queued or save conflicts still open when the report was
// made — the report may not show the latest review.
document.getElementById("review-warnings").innerHTML = Array.isArray(reviewWarnings) && reviewWarnings.length ? `<div class="note"><strong>주의: 판독 기록이 아직 확정되지 않았습니다</strong><br>${{reviewWarnings.map(escapeHtml).join("<br>")}}</div>` : "";

const sourceCounts = new Map();
videos.forEach(video => {{
  const profile = video.source_profile || {{}};
  const key = `${{profile.vendor || "Unknown"}}\t${{profile.parser || "unknown"}}\t${{profile.confidence || "-"}}\t${{profile.lane || "-"}}`;
  sourceCounts.set(key, (sourceCounts.get(key) || 0) + 1);
}});

document.getElementById("title").textContent = manifest.title || "FrameTrace 영상 포렌식 보고서";
document.getElementById("case-line").textContent = `${{manifest.case_id || "case"}} · 로컬 케이스 데이터 기반 생성 · ${{manifest.tool_name || "frametrace"}} ${{manifest.tool_version || ""}}`;
document.getElementById("count").textContent = scan.video_count ?? videos.length;
document.getElementById("bytes").textContent = fmtBytes(scan.total_bytes ?? 0);
document.getElementById("confirmed").textContent = videos.filter(video => video.ffprobe_ok).length;
document.getElementById("sources").textContent = sourceCounts.size;
document.getElementById("warnings-count").textContent = warnings.length;
document.getElementById("exports").textContent = exportsLog.length;
document.getElementById("joins").textContent = joinLog.length;
document.getElementById("derived").textContent = derivedLog.length;
document.getElementById("carved").textContent = carveLog.length;
document.getElementById("filesystem-actions").textContent = filesystemLog.length;
document.getElementById("validations").textContent = validationLog.length;

document.getElementById("processing").innerHTML = `<table>
  <tbody>
    <tr><th>케이스 ID</th><td>${{escapeHtml(manifest.case_id || "-")}}</td></tr>
    <tr><th>작업자 / 호스트</th><td>${{escapeHtml(manifest.operator || "-")}} / ${{escapeHtml(manifest.host || "-")}}</td></tr>
    <tr><th>플랫폼 / 도구</th><td>${{escapeHtml(manifest.platform || "-")}} / ${{escapeHtml(manifest.tool_name || "-")}} ${{escapeHtml(manifest.tool_version || "")}}</td></tr>
    <tr><th>장치</th><td>${{escapeHtml(manifest.device_id || "-")}} · serial: ${{escapeHtml(manifest.device_serial || "-")}} · write-protect: ${{escapeHtml(manifest.write_protect || "-")}}</td></tr>
    <tr><th>취득</th><td>${{escapeHtml(manifest.acquisition_tool || "-")}} · evidence hash: <code>${{escapeHtml(manifest.evidence_hash || "-")}}</code></td></tr>
    <tr><th>비고</th><td>${{escapeHtml(manifest.notes || "-")}}</td></tr>
    <tr><th>소스 경로</th><td><code>${{escapeHtml(scan.source_path || "-")}}</code></td></tr>
    <tr><th>스캔 시각</th><td>${{fmtUnix(scan.scanned_unix)}} <span class="muted">(${{escapeHtml(scan.scanned_unix || "-")}})</span></td></tr>
    <tr><th>해시 모드</th><td>${{scan.options?.hash_files ? "파일별 SHA-256 계산" : "파일별 SHA-256 생략"}}</td></tr>
    <tr><th>메타데이터 모드</th><td>${{scan.options?.use_ffprobe ? "ffprobe 사용" : "ffprobe 생략"}}</td></tr>
    <tr><th>경고</th><td>${{warnings.length ? warnings.map(escapeHtml).join("<br>") : "없음"}}</td></tr>
  </tbody>
</table>`;

document.getElementById("source-assessment").innerHTML = sourceCounts.size ? `<table>
  <thead>
    <tr><th>추정 소스</th><th>파서 레인</th><th>신뢰도</th><th>파일 수</th><th>처리 메모</th></tr>
  </thead>
  <tbody>
    ${{[...sourceCounts.entries()].map(([key, count]) => {{
      const [vendor, parser, confidence, lane] = key.split("\t");
      const sample = videos.find(video => (video.source_profile?.vendor || "Unknown") === vendor && (video.source_profile?.parser || "unknown") === parser);
      return `<tr>
        <td>${{escapeHtml(vendor)}}</td>
        <td>${{escapeHtml(lane)}}<br><code>${{escapeHtml(parser)}}</code></td>
        <td>${{escapeHtml(confidence)}}</td>
        <td>${{count}}</td>
        <td>${{escapeHtml(sample?.source_profile?.recommended_action || "-")}}</td>
      </tr>`;
    }}).join("")}}
  </tbody>
</table>` : "<p>소스 평가 정보가 없습니다.</p>";

// Measured-sound wording (plan §2.7). Only what was measured is claimed:
// a signal level over the whole audio track, never speech. `audioLevels`
// arrives already bound to the current bytes (size/mtime/hash, latest wins).
const fmtDb = value => (value === null || value === undefined ? "-inf" : (Number.isFinite(value) ? value.toFixed(1) : "-")).replace("-", "−");
// R8 P1/I8: `sc` = the measurement is self-consistency only (not
// authenticated by an audit key); P2: `pc` = signed under another PC's key.
const selfConsistent = level => level && level.sc === true ? " (자기 일관성만 확인 — 인증 아님)" : "";
function audioText(video) {{
  const level = audioLevels[video.id];
  if (level && level.r === "sound") return `소리 있음 — 측정: 최대 ${{fmtDb(level.max)}} dB, 평균 ${{fmtDb(level.mean)}} dB (dBFS, ffmpeg volumedetect)${{selfConsistent(level)}}`;
  if (level && level.r === "silent") return `무음 트랙 — 측정: 최대 ${{fmtDb(level.max)}} dB (기준: −70 dB 이하)${{selfConsistent(level)}}`;
  if (level && level.r === "stale") return "측정 후 파일 변경 — 미확인";
  if (level && level.r === "unverified" && typeof level.pc === "string") return `이 PC에서 인증 불가 — 키 "${{level.pc}}" 보유 PC에서 확인 (다른 PC에서 측정됨) — 표시 안 함`;
  if (level && level.r === "unverified") return "측정 기록 검증 실패 — 표시 안 함";
  if (level && level.r === "replayed") return "재개 시 재기록된 측정 — 다시 측정 전 미확인";
  if (level && level.r === "error") return "오디오 트랙 있음 — 소리 미측정 (측정 실패)";
  if (video.ffprobe_ok && video.audio_codec) return "오디오 트랙 있음 — 소리 미측정";
  if (video.ffprobe_ok) return "오디오 트랙 없음 (ffprobe)";
  return "미확인";
}}

document.getElementById("videos").innerHTML = videos.length ? `<table>
  <thead>
    <tr>
      <th>ID</th><th>경로</th><th>소스</th><th>포맷</th><th>길이</th><th>크기</th><th>해시</th><th>소리(측정값)</th>
    </tr>
  </thead>
  <tbody>
    ${{videos.map(video => `<tr>
      <td>${{escapeHtml(video.id)}}</td>
      <td><code>${{escapeHtml(video.relative_path || video.source_path)}}</code></td>
      <td>${{escapeHtml(video.source_profile?.vendor || "-")}}<br><code>${{escapeHtml(video.source_profile?.parser || "-")}}</code></td>
      <td>${{escapeHtml(video.video_codec || video.extension || "-")}} ${{video.width && video.height ? `(${{escapeHtml(video.width)}}x${{escapeHtml(video.height)}})` : ""}}</td>
      <td>${{fmtDuration(video.duration_seconds)}}</td>
      <td>${{fmtBytes(video.size_bytes)}}</td>
      <td><code>${{escapeHtml(video.sha256 || video.hash_status || "-")}}</code></td>
      <td>${{escapeHtml(audioText(video))}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : emptyOr("index", "<p>색인된 영상이 없습니다.</p>");

document.getElementById("clip-exports").innerHTML = exportsLog.length ? `<table>
  <thead>
    <tr><th>포맷</th><th>원본</th><th>산출물</th><th>산출물 SHA-256</th><th>범위</th><th>감사 체인</th></tr>
  </thead>
  <tbody>
    ${{exportsLog.map(item => `<tr>
      <td>${{escapeHtml(item.format || "-")}}</td>
      <td><code>${{escapeHtml(item.source_path || item.selector || "-")}}</code></td>
      <td><code>${{escapeHtml(item.output_path || "-")}}</code></td>
      <td><code>${{escapeHtml(item.output_sha256 || "-")}}</code></td>
      <td>${{escapeHtml(item.start_seconds ?? "-")}}, ${{escapeHtml(item.duration_seconds ?? "-")}}</td>
      <td>${{chainCell("export", item.entry_sha256)}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : emptyOr("export", "<p>내보낸 클립이 없습니다.</p>");


document.getElementById("derived-artifacts").innerHTML = derivedLog.length ? `<table>
  <thead>
    <tr><th>종류</th><th>원본</th><th>산출물</th><th>산출물 SHA-256</th><th>감사 체인</th></tr>
  </thead>
  <tbody>
    ${{derivedLog.map(item => `<tr>
      <td>${{escapeHtml(item.kind || "-")}}</td>
      <td><code>${{escapeHtml(item.source_path || "-")}}</code></td>
      <td><code>${{escapeHtml(item.output_path || "-")}}</code></td>
      <td><code>${{escapeHtml(item.output_sha256 || "-")}}</code></td>
      <td>${{chainCell(item.kind === "proxy" ? "proxy" : "thumbnail", item.entry_sha256)}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : (readErrors.proxy || readErrors.thumbnail ? readFailed(readErrors.proxy ? "proxy" : "thumbnail") : "<p>프록시 또는 썸네일 산출물이 없습니다.</p>");

document.getElementById("carved-artifacts").innerHTML = carveLog.length ? `<table>
  <thead>
    <tr><th>ID</th><th>상태</th><th>시그니처</th><th>오프셋</th><th>크기</th><th>산출물</th><th>SHA-256</th><th>감사 체인</th></tr>
  </thead>
  <tbody>
    ${{carveLog.map(item => `<tr>
      <td>${{escapeHtml(item.id || "-")}}</td>
      <td>${{escapeHtml(item.validation_status || "candidate-unvalidated")}}</td>
      <td>${{escapeHtml(item.signature || item.extension || "-")}}</td>
      <td>${{escapeHtml(item.offset ?? "-")}}</td>
      <td>${{fmtBytes(item.size_bytes)}}</td>
      <td><code>${{escapeHtml(item.output_path || "-")}}</code></td>
      <td><code>${{escapeHtml(item.sha256 || "-")}}</code></td>
      <td>${{chainCell("carve", item.entry_sha256)}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : emptyOr("carve", "<p>Carving 후보가 없습니다.</p>");

// I5: the validation log is append-only across runs. Only the newest
// validation of each target is "current"; superseded ones are history,
// shown folded with their run time so an old result never reads as today's.
const validationTarget = item => item.target_path || item.selector || "-";
const currentValidations = [];
const supersededValidations = [];
{{
  const latest = new Map();
  validationLog.forEach((item, order) => {{
    const key = validationTarget(item);
    const prior = latest.get(key);
    const ts = Number.isFinite(item.validated_unix) ? item.validated_unix : -Infinity;
    if (!prior || ts >= prior.ts) latest.set(key, {{item, ts, order}});
  }});
  const keep = new Set([...latest.values()].map(entry => entry.order));
  validationLog.forEach((item, order) => (keep.has(order) ? currentValidations : supersededValidations).push(item));
}}
// R13 I2: a target's newest validation positioned BEFORE a lane-repair
// marker is not known to be the latest (the repair cut a tail that may
// have held a newer one) — shown as "알 수 없음", never as current.
const unknownValidations = currentValidations.filter(item => (validationPosition.get(item) ?? -1) < validationRepairAt);
// R19 I4: no `splice(..., ...arr)` (RangeError at ~130k arguments).
{{
  const unknownSet = new Set(unknownValidations);
  const known = currentValidations.filter(item => !unknownSet.has(item));
  currentValidations.length = 0;
  for (const item of known) currentValidations.push(item);
}}
const validationRow = (item, withTime) => `<tr>
      ${{withTime ? `<td>${{fmtUnix(item.validated_unix)}}</td>` : ""}}
      <td>${{escapeHtml(item.validation_status || "-")}}</td>
      <td><code>${{escapeHtml(validationTarget(item))}}</code></td>
      <td>${{escapeHtml(item.format_name || "-")}}</td>
      <td>${{escapeHtml(item.video_codec || "-")}} / ${{escapeHtml(item.audio_codec || "-")}}</td>
      <td>${{fmtDuration(item.duration_seconds)}}</td>
      <td><code>${{escapeHtml(item.target_sha256 || "-")}}</code></td>
      <td>${{escapeHtml(item.validation_note || item.ffprobe_error || "-")}}</td>
      <td>${{chainCell("validation", item.entry_sha256)}}</td>
    </tr>`;
const validationHead = withTime => `<thead>
    <tr>${{withTime ? "<th>실행 시각</th>" : ""}}<th>상태</th><th>대상</th><th>포맷</th><th>코덱</th><th>길이</th><th>SHA-256</th><th>검증 메모</th><th>감사 체인</th></tr>
  </thead>`;
document.getElementById("validations").textContent = currentValidations.length;
const unknownValidationsHtml = unknownValidations.length ? `<div class="note"><strong>최신 여부 알 수 없음 ${{unknownValidations.length}}건</strong> — 검증 기록 레인이 이 검증들 뒤에서 수리됨(lane-repair): 잘린 꼬리에 같은 대상의 더 최근 검증이 있었을 수 있어 현재 결과로 보지 않습니다 (다시 검증하십시오).</div><table>
  ${{validationHead(true)}}
  <tbody>
    ${{unknownValidations.map(item => validationRow(item, true)).join("")}}
  </tbody>
</table>` : "";
document.getElementById("validation-results").innerHTML = currentValidations.length ? `<p class="muted">현재 결과: 대상별 가장 최근 검증 ${{currentValidations.length}}건${{supersededValidations.length ? ` (이전 검증 ${{supersededValidations.length}}건은 아래 이력에 접어 둠)` : ""}}</p><table>
  ${{validationHead(true)}}
  <tbody>
    ${{currentValidations.map(item => validationRow(item, true)).join("")}}
  </tbody>
</table>${{unknownValidationsHtml}}` : unknownValidationsHtml || emptyOr("validation", "<p>검증 기록이 없습니다.</p>");
document.getElementById("validation-history").innerHTML = supersededValidations.length ? `<details><summary>이전 검증 이력 ${{supersededValidations.length}}건 — 같은 대상의 더 최근 검증으로 대체됨 (현재 결과 아님)</summary><table>
  ${{validationHead(true)}}
  <tbody>
    ${{[...supersededValidations].reverse().map(item => validationRow(item, true)).join("")}}
  </tbody>
</table></details>` : "";

// I5: the anomaly log is append-only too. Entries belong to the run whose
// `anomaly-scan-run` marker follows them; only the newest completed run is
// "current". Logs written before run markers existed group by scanned_unix.
const anomalyRuns = [];
{{
  let pending = [];
  const flushUnmarked = items => {{
    // R20 I2: by scan_id when the row has one ("id:" keys never equal a
    // number key), else by scanned_unix (older scanners' rows).
    const byTime = new Map();
    items.forEach(item => {{
      const key = typeof item.scan_id === "string" ? "id:" + item.scan_id : Number.isFinite(item.scanned_unix) ? item.scanned_unix : null;
      if (!byTime.has(key)) byTime.set(key, []);
      byTime.get(key).push(item);
    }});
    byTime.forEach((entries, key) => {{
      const id = typeof entries[0].scan_id === "string" ? entries[0].scan_id : null;
      const ts = id === null ? key : Number.isFinite(entries[0].scanned_unix) ? entries[0].scanned_unix : null;
      anomalyRuns.push({{run: null, ts, id, entries}});
    }});
  }};
  // R20 I2: a row with a scan_id belongs to the marker with that scan_id
  // only; a row without one (older scanners) to the marker with its
  // scanned_unix. Same rule as `report::anomaly_lane_withheld`.
  const sameAnomalyRun = (entry, marker) => typeof entry.scan_id === "string" ? entry.scan_id === marker.scan_id : entry.scanned_unix === marker.scanned_unix;
  anomalyLog.forEach(item => {{
    if (item.event === "anomaly-scan-run") {{
      // A run writes its findings and then its marker with one scanned_unix;
      // anything else still pending is an older unmarked (legacy or cut
      // short) run.
      flushUnmarked(pending.filter(entry => !sameAnomalyRun(entry, item)));
      anomalyRuns.push({{run: item, ts: item.scanned_unix, id: typeof item.scan_id === "string" ? item.scan_id : null, entries: pending.filter(entry => sameAnomalyRun(entry, item))}});
      pending = [];
    }} else {{
      pending.push(item);
    }}
  }});
  flushUnmarked(pending);
  // R19 I1: interleaved runs (A's finding @100, B's finding @200, A's
  // marker @100, B's marker @200) flushed B's finding as an unmarked run
  // before A's run — B's completed run read "후보 0건". An unmarked run
  // whose scanned_unix equals a LATER completed run's belongs to it (the
  // nearest one; its entries go ahead of the run's own, lane order).
  // Same rule as `report::anomaly_lane_withheld`.
  const nearest = new Map();
  const chunks = new Map();
  const merged = new Set();
  // R20 I2: an unmarked scan_id group joins only the completed run whose
  // marker has that scan_id; one without keeps the time rule.
  for (let index = anomalyRuns.length - 1; index >= 0; index--) {{
    const run = anomalyRuns[index];
    if (run.run !== null) {{
      if (Number.isFinite(run.ts)) nearest.set(run.ts, run);
      if (run.id !== null) nearest.set("id:" + run.id, run);
      continue;
    }}
    const key = run.id !== null ? "id:" + run.id : Number.isFinite(run.ts) ? run.ts : undefined;
    if (key !== undefined && nearest.has(key)) {{
      const target = nearest.get(key);
      if (!chunks.has(target)) chunks.set(target, []);
      chunks.get(target).push(run.entries);
      merged.add(run);
    }}
  }}
  chunks.forEach((parts, target) => {{
    const entries = [];
    for (let at = parts.length - 1; at >= 0; at--) for (const item of parts[at]) entries.push(item);
    for (const item of target.entries) entries.push(item);
    target.entries = entries;
  }});
  if (merged.size) {{
    const kept = anomalyRuns.filter(run => !merged.has(run));
    anomalyRuns.length = 0;
    for (const run of kept) anomalyRuns.push(run);
  }}
}}
const latestCompleted = anomalyRuns.map(run => run.run !== null).lastIndexOf(true);
// R13 P16: a newer run without its completion marker after the last
// completed run — cut short, OR its marker was deleted (no key needed) —
// WITHHOLDS the result (R8 showed the older run as "current" with a
// notice); its findings are shown with the "no result" notice.
// R14 P20: by SPAN, not position — `flushUnmarked` puts an interleaved
// unmarked run BEFORE the completed run whose marker ended it, so an
// unmarked run is also newer when its first entry is after the completed
// run's first entry or its scanned_unix is greater.
// R19 I4: loops, not `Math.min(...arr)` — a spread of ~130k arguments
// throws RangeError and blanked the report.
const anomalyRunFirst = run => {{
  let first = run.run ? (anomalyPosition.get(run.run) ?? Infinity) : Infinity;
  for (const item of run.entries) {{
    const at = anomalyPosition.get(item) ?? Infinity;
    if (at < first) first = at;
  }}
  return first;
}};
// R15 I4: the run's last lane position — a run stamped later but placed
// wholly before the completed run's first entry is an older crash with a
// clock set in the future, not a newer run.
const anomalyRunLast = run => {{
  let last = run.run ? (anomalyPosition.get(run.run) ?? -Infinity) : -Infinity;
  for (const item of run.entries) {{
    const at = anomalyPosition.get(item) ?? -Infinity;
    if (at > last) last = at;
  }}
  return last;
}};
// R15 I4: "시계 이상" — a run stamped more than 600 s after this viewer's
// clock.
const anomalyClockAhead = run => Number.isFinite(run.ts) && run.ts > Date.now() / 1000 + 600;
// R15 P22 / I2: the sign-off lane's hash-verify-head against the anomaly
// lane (`auditChains.anomaly.head`): a cut tail, a rewritten lane or a
// head that cannot be relied on withholds the current result.
const anomalyHead = auditChains && auditChains.anomaly && typeof auditChains.anomaly.head === "object" ? auditChains.anomaly.head : null;
const anomalyHeadReason = anomalyHead && typeof anomalyHead.withhold_reason === "string" ? anomalyHead.withhold_reason : "";
const latestAnomalyRun = anomalyRuns[latestCompleted] || null;
// R18 I5: an entry this PC cannot use INSIDE the newest completed run —
// between the previous usable marker and its own, and after the run's
// first entry or stamped with its scanned_unix (a stripped HMAC keeps the
// content) — that run's candidates may be missing, head or no head.
const anomalyUnusableInRun = latestAnomalyRun ? anomalyAll.filter((item, index) => index > previousUsableAnomalyMarker && index < lastUsableAnomalyMarker && !usableEntry(item)
  && (index > anomalyRunFirst(latestAnomalyRun) || item.scanned_unix === latestAnomalyRun.run.scanned_unix)).length : 0;
// R14 I4: no completed run at all in a lane written by a marker-writing
// scanner (its entries carry schema_version — every scanner since run
// markers exist): every marker may have been deleted, so the last unmarked
// run is not "current" either. Legacy logs without markers keep grouping
// by time.
const anomalyMarkedLane = anomalyLog.some(item => item?.schema_version !== undefined);
const newerAnomalyRuns = latestAnomalyRun
  ? anomalyRuns.filter((run, index) => run.run === null && (index > latestCompleted
      || anomalyRunFirst(run) > anomalyRunFirst(latestAnomalyRun)
      || (Number.isFinite(run.ts) && Number.isFinite(latestAnomalyRun.ts) && run.ts > latestAnomalyRun.ts
        && anomalyRunLast(run) > anomalyRunFirst(latestAnomalyRun))))
  : anomalyMarkedLane ? anomalyRuns.slice() : [];
// R18 I2: the same lane rules computed by the server
// (`report::anomaly_lane_withheld`, also `/api/records-meta` and the
// bundle's `logChains.anomaly.withheld`) — withheld when either says so.
const anomalyLaneVerdict = auditChains && auditChains.anomaly && auditChains.anomaly.withheld && typeof auditChains.anomaly.withheld === "object" && typeof auditChains.anomaly.withheld.reason === "string" ? auditChains.anomaly.withheld : null;
const anomalyLaneReasons = [
  anomalyUnusableAfter ? "가장 최근 완료 실행의 완료 표식 뒤에 이 PC에서 인증할 수 없는 항목이 있어(더 새 실행의 완료 표식일 수 있음) 그 결과를 알 수 없습니다 (아래 \"인증 안 된 감사 항목\" 참조)" : "",
  anomalyUnusableInRun ? `가장 최근 완료 실행의 기록 범위 안에 이 PC에서 인증할 수 없는 항목 ${{anomalyUnusableInRun}}개 — 그 실행의 후보가 빠졌을 수 있어 그 결과를 알 수 없습니다 (아래 "인증 안 된 감사 항목" 참조)` : "",
  anomalyRepairedAfter ? "마지막 완료 실행 뒤에 레인 수리(lane-repair) 표식이 있어 — 잘린 꼬리에 더 새 실행이 있었을 수 있어 — 최신 실행을 알 수 없습니다 (이상 징후 검사를 다시 실행하십시오)" : "",
  newerAnomalyRuns.length && latestAnomalyRun ? `표식 없는 더 새 실행 있음 — 마지막 완료 실행 뒤에 있거나 그 뒤에 시작한 완료 표식 없는 실행 ${{newerAnomalyRuns.length}}회: 완료 표식이 지워졌거나 실행이 중단됐을 수 있습니다 (이상 징후 검사를 다시 실행하십시오)` : "",
  newerAnomalyRuns.length && !latestAnomalyRun ? `표식 없는 더 새 실행 있음 — 완료된 실행이 없고 완료 표식 없는 실행 ${{newerAnomalyRuns.length}}회만 있습니다: 완료 표식이 지워졌거나 실행이 중단됐을 수 있어 결과가 아닙니다 (이상 징후 검사를 다시 실행하십시오)` : "",
].filter(Boolean);
const anomalyWithheldReasons = [
  ...(anomalyLaneReasons.length ? anomalyLaneReasons : anomalyLaneVerdict ? [anomalyLaneVerdict.reason] : []),
  anomalyHeadReason,
].filter(Boolean);
const currentAnomalyIndex = anomalyWithheldReasons.length ? -1 : latestCompleted >= 0 ? latestCompleted : anomalyRuns.length - 1;
const currentAnomalyRun = anomalyRuns[currentAnomalyIndex] || null;
const findingRows = run => run.entries.filter(item => item.kind && item.kind !== "none");
const anomalyTable = rows => `<table>
  <thead>
    <tr><th>라벨</th><th>종류</th><th>대상</th><th>상세</th></tr>
  </thead>
  <tbody>
    ${{rows.map(item => `<tr>
      <td>${{escapeHtml(item.label || "candidate-finding")}}</td>
      <td>${{escapeHtml(item.kind || "-")}}</td>
      <td><code>${{escapeHtml(item.selector || item.source_path || "-")}}</code></td>
      <td>${{escapeHtml(item.detail || "-")}}</td>
    </tr>`).join("")}}
  </tbody>
</table>`;
// E8: index lines/records the scan did not examine, disclosed per run.
const indexCoverageText = marker => {{
  if (!marker) return "";
  const parts = [];
  if (marker.index_unparseable_lines) parts.push(`색인 손상 줄 ${{marker.index_unparseable_lines}}개 미검사`);
  if (marker.index_rows_without_id) parts.push(`id 없는 색인 기록 ${{marker.index_rows_without_id}}개 미검사`);
  if (marker.index_stale_rows) parts.push(`최근 스캔 때 없던 파일(stale) ${{marker.index_stale_rows}}개 제외`);
  return parts.length ? " · " + parts.join(" · ") : "";
}};
const anomalyRunLine = run => {{
  const mode = run.run ? (run.run.hash_mode === "rehash" ? "해시 모드: 실시간 재해시" : "해시 모드: 색인에 저장된 해시") : "실행 기록 없음(완료 표시 없는 실행)";
  return `실행 시각 ${{fmtUnix(run.ts)}}${{anomalyClockAhead(run) ? " (시계 이상: 이 PC의 현재 시각보다 뒤)" : ""}} · ${{mode}} · 후보 ${{findingRows(run).length}}건${{run.run?.coverage_limited && run.run?.hash_mode === "rehash" && (run.run.skipped_no_hash || run.run.skipped_missing || run.run.skipped_error) ? " · 일부 파일 재해시 못 함" : ""}}${{indexCoverageText(run.run)}}`;
}};
const anomalyRows = currentAnomalyRun ? findingRows(currentAnomalyRun) : [];
document.getElementById("anomaly-findings").innerHTML = currentAnomalyRun
  ? `<p class="muted">현재 결과 — ${{anomalyRunLine(currentAnomalyRun)}} · ${{escapeHtml(chainLabel("anomaly"))}}</p>${{anomalyRows.length ? anomalyTable(anomalyRows) : "<p>이 실행에서 이상 징후 후보가 없습니다.</p>"}}`
  : anomalyWithheldReasons.length
    ? `<div class="note"><strong>현재 결과 없음(결과 아님)</strong> — ${{anomalyWithheldReasons.map(escapeHtml).join("; ")}}. 이전 실행을 현재 결과로 보이지 않습니다.</div>${{newerAnomalyRuns.map(run => `<p class="muted"><strong>표식 없는 더 새 실행 (결과 아님, 숨기지 않고 표시)</strong> — ${{anomalyRunLine(run)}}</p>${{findingRows(run).length ? anomalyTable(findingRows(run)) : "<p>후보 없음</p>"}}`).join("")}}`
    : emptyOr("anomaly", "<p>이상 징후 후보가 없습니다.</p>");
const pastAnomalyRuns = anomalyRuns.filter((run, index) => index !== currentAnomalyIndex && !newerAnomalyRuns.includes(run)).reverse();
document.getElementById("anomaly-history").innerHTML = pastAnomalyRuns.length ? `<details><summary>이전 실행 ${{pastAnomalyRuns.length}}회 — 현재 결과 아님</summary>${{pastAnomalyRuns.map(run => `<p class="muted">${{anomalyRunLine(run)}}</p>${{findingRows(run).length ? anomalyTable(findingRows(run)) : "<p>후보 없음</p>"}}`).join("")}}</details>` : "";

document.getElementById("filesystem-recovery").innerHTML = filesystemLog.length ? `<table>
  <thead>
    <tr><th>이벤트</th><th>이미지</th><th>오프셋</th><th>Inode</th><th>결과</th><th>경고</th><th>SHA-256 / 로그</th><th>감사 체인</th></tr>
  </thead>
  <tbody>
    ${{filesystemLog.map(item => `<tr>
      <td>${{escapeHtml(item.event || "-")}}</td>
      <td><code>${{escapeHtml(item.image_path || "-")}}</code></td>
      <td>${{escapeHtml(item.partition_offset ?? "-")}}</td>
      <td><code>${{escapeHtml(item.inode || "-")}}</code></td>
      <td>${{escapeHtml(item.validation_status || `${{item.entry_count ?? "-"}} entries`)}}<br><code>${{escapeHtml(item.output_path || item.summary_path || "-")}}</code></td>
      <td>${{Array.isArray(item.warnings) && item.warnings.length ? item.warnings.map(escapeHtml).join("<br>") : "-"}}</td>
      <td><code>${{escapeHtml(item.sha256 || item.entries_jsonl_path || "-")}}</code></td>
      <td>${{chainCell("filesystem", item.entry_sha256)}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : emptyOr("filesystem", "<p>파일시스템 조사 또는 inode 복구 기록이 없습니다.</p>");

// `annotations` is {{marks:[...], tags:{{id:[...]}}}}; a bare marks array
// (pre-I3 callers) is still accepted.
const annotationMarks = Array.isArray(annotations) ? annotations : (Array.isArray(annotations?.marks) ? annotations.marks : []);
const annotationTags = !Array.isArray(annotations) && annotations?.tags && typeof annotations.tags === "object" ? annotations.tags : {{}};
const marksById = new Map(annotationMarks.map(mark => [mark.id, mark]));
const tagsById = new Map(Object.entries(annotationTags).filter(([, tags]) => Array.isArray(tags) && tags.length));
const fmtSeconds = value => typeof value === "number" && Number.isFinite(value) ? `${{Number.isInteger(value) ? value.toFixed(1) : value}}s` : "—";
const multiline = value => escapeHtml(value).replaceAll("\n", "<br>");
function markCell(id) {{
  const mark = marksById.get(id);
  const tags = tagsById.get(id) || [];
  const parts = [];
  if (mark) {{
    parts.push(escapeHtml(markLabel(mark.status)));
    if (mark.note) parts.push(`<span class="muted">${{multiline(mark.note)}}</span>`);
    if (mark.examiner) parts.push(`<span class="muted">검토자: ${{escapeHtml(mark.examiner)}}</span>`);
    const range = mark.range;
    if (range && (Number.isFinite(range.in) || Number.isFinite(range.out))) {{
      parts.push(`<span class="muted">구간: ${{escapeHtml(fmtSeconds(range.in))}} ~ ${{escapeHtml(fmtSeconds(range.out))}}</span>`);
    }}
  }}
  if (tags.length) parts.push(`<span class="muted">태그: ${{tags.map(escapeHtml).join(", ")}}</span>`);
  return parts.length ? parts.join("<br>") : "-";
}}
function validationForRow(row) {{
  const byExactPath = validationLog.filter(item => item.target_path === row.where);
  const eligible = byExactPath.filter(item => /^[a-fA-F0-9]{{64}}$/.test(row.hash) &&
    typeof item.target_sha256 === "string" && item.target_sha256.toLowerCase() === row.hash.toLowerCase());
  if (eligible.length) {{
    const latest = eligible.sort((a, b) => (a.validated_unix ?? 0) - (b.validated_unix ?? 0)).at(-1);
    // R13 I2: before a lane-repair marker the latest is unknown.
    if ((validationPosition.get(latest) ?? -1) < validationRepairAt) {{
      return {{validation_status: "validation-unknown-after-repair", validation_note: `최신 여부 알 수 없음 — 검증 기록 레인이 이 검증(${{latest.validation_status || "-"}}) 뒤에서 수리됨(lane-repair): 잘린 꼬리에 더 최근 검증이 있었을 수 있음`}};
    }}
    return latest;
  }}
  if (byExactPath.length) return {{validation_status: "validation-unbound", validation_note: "No validation is bound to the recorded evidence SHA-256; missing or different hashes cannot confirm these bytes."}};
  return null;
}}
const markLabel = status => ({{
  reviewed: "판독 완료",
  important: "중요",
  needs_verification: "검증 대기",
  noted: "메모"
}})[status] || status || "-";

function techniqueRows() {{
  const rows = [];
  videos.forEach(video => rows.push({{
    id: video.id,
    kind: "원본 (논리 파일)",
    how: "논리 파일 스캔 — 확장자·매직바이트 판별 후 색인",
    parser: `${{video.source_profile?.vendor || "-"}} / ${{video.source_profile?.parser || "-"}} (${{video.source_profile?.confidence || "-"}})`,
    where: video.source_path || video.relative_path || "-",
    when: video.modified_unix,
    meta: [video.video_codec || video.format_name || "-", video.width && video.height ? `${{video.width}}x${{video.height}}` : "", fmtDuration(video.duration_seconds), fmtBytes(video.size_bytes), `소리(측정값): ${{audioText(video)}}`].filter(Boolean).join(" · "),
    hash: video.sha256 || video.hash_status || "-"
  }}));
  carveLog.forEach(item => rows.push({{
    id: item.id || item.output_path,
    kind: "카빙 후보",
    how: `시그니처 카빙 — ${{item.signature || item.extension || "?"}} @ 오프셋 ${{item.offset ?? "-"}}`,
    parser: "carve",
    where: item.output_path || "-",
    when: null,
    meta: fmtBytes(item.size_bytes),
    hash: item.sha256 || "-"
  }}));
  filesystemLog.forEach(item => {{
    if (item.event !== "recover-inode") return;
    rows.push({{
      id: `inode:${{item.partition_offset ?? 0}}:${{item.inode || item.output_path || ""}}`,
      kind: "파일시스템 복구",
      how: `Sleuth Kit icat inode 복구 — inode ${{item.inode ?? "-"}} @ 파티션 오프셋 ${{item.partition_offset ?? "-"}}`,
      parser: "tsk/icat",
      where: item.output_path || "-",
      when: null,
      meta: fmtBytes(item.size_bytes) + (Array.isArray(item.warnings) && item.warnings.length ? ` · 경고: ${{item.warnings.join("; ")}}` : ""),
      hash: item.sha256 || "-"
    }});
  }});
  return rows;
}}

document.getElementById("techniques").innerHTML = techniqueRows().length ? `<table>
  <thead>
    <tr><th>ID</th><th>구분</th><th>발견 기법</th><th>파서 평가</th><th>위치 / 근원</th><th>시각</th><th>메타데이터</th><th>SHA-256</th><th>검증 상태 / 근거</th><th>판독 (상태·메모·구간·태그)</th></tr>
  </thead>
  <tbody>
    ${{techniqueRows().map(row => {{
      const validation = validationForRow(row);
      const status = validation?.validation_status || (row.kind === "원본 (논리 파일)" ? "색인됨 (재생성 검증 전)" : "candidate-unvalidated");
      const reason = validation ? (validation.validation_note || validation.ffprobe_error || "-") : "재생성 검증 대기 — 판독자 재생 확인 필요";
      return `<tr>
        <td>${{escapeHtml(row.id)}}</td>
        <td>${{escapeHtml(row.kind)}}</td>
        <td>${{escapeHtml(row.how)}}</td>
        <td><code>${{escapeHtml(row.parser)}}</code></td>
        <td><code>${{escapeHtml(row.where)}}</code></td>
        <td>${{row.when ? fmtUnix(row.when) : "-"}}</td>
        <td>${{escapeHtml(row.meta)}}</td>
        <td><code>${{escapeHtml(row.hash)}}</code></td>
        <td>${{escapeHtml(status)}}<br><span class="muted">${{escapeHtml(reason)}}</span></td>
        <td>${{markCell(row.id)}}</td>
      </tr>`;
    }}).join("")}}
  </tbody>
</table>` : "<p>정리할 증거가 없습니다.</p>";

// Joined videos (plan §1.9). Each row carries the joined record's 판독
// annotations (U2), filed under `record_id` (`join:<first 16 hex of the
// output SHA-256>`); legacy `join:<file name>` ids are still read. A
// filename-order join never claims "끊김 0 / 겹침 0" (B2).
const joinRecordId = item => item.record_id || (typeof item.output_sha256 === "string" && /^[0-9a-fA-F]{{16}}/.test(item.output_sha256) ? `join:${{item.output_sha256.slice(0, 16).toLowerCase()}}` : null);
const joinLegacyId = item => `join:${{String(item.output_path || "").split(/[\\/]/).pop()}}`;
// D18: the `join:<file name>` fallback is only for entries logged before
// record ids existed (no record_id in the log line itself). A file name is
// reused by later joins, so a newer entry must never pick up the old
// join's annotations through it.
const joinIsLegacy = item => item.record_id_derived === true || !item.record_id;
function joinMarkCell(item) {{
  const id = joinRecordId(item);
  const cell = id ? markCell(id) : "-";
  return cell === "-" && joinIsLegacy(item) ? markCell(joinLegacyId(item)) : cell;
}}
// D3: the joined length as measured on the output against the planned
// total, and the per-segment boundary check, when the log recorded them.
function joinLengthText(item) {{
  const check = item.duration_check && typeof item.duration_check === "object" ? item.duration_check : null;
  const observed = check && Number.isFinite(check.observed) ? check.observed : (Number.isFinite(item.observed_duration_seconds) ? item.observed_duration_seconds : null);
  const expected = check && Number.isFinite(check.expected) ? check.expected : item.duration_seconds;
  const ok = check && typeof check.ok === "boolean" ? check.ok : (typeof item.duration_check_ok === "boolean" ? item.duration_check_ok : null);
  const parts = [];
  parts.push(observed === null ? "관측 길이 미기록" : `관측 ${{fmtDuration(observed)}} / 예상 ${{fmtDuration(expected)}}`);
  parts.push(ok === null ? "길이 검사 결과 미기록" : (ok ? "길이 검사 통과" : "길이 검사 불일치"));
  const boundary = item.boundary_check && typeof item.boundary_check === "object" ? item.boundary_check : null;
  parts.push(boundary && typeof boundary.ok === "boolean" ? (boundary.ok ? `세그먼트 경계 검증 통과${{boundary.detail ? ` — ${{boundary.detail}}` : ""}}` : `세그먼트 경계 검증 실패${{boundary.detail ? ` — ${{boundary.detail}}` : ""}}`) : "세그먼트 경계 검증 미기록");
  return parts.map(escapeHtml).join("<br>");
}}
function joinJunctionText(item) {{
  if (item.order_basis === "filename-order") return "끊김·겹침 확인 불가 — 파일명 순";
  const unknown = Number(item.unknown_junctions) > 0 ? ` / 확인 불가 ${{escapeHtml(item.unknown_junctions)}}` : "";
  return `끊김 ${{escapeHtml(item.gaps ?? "-")}} / 겹침 ${{escapeHtml(item.overlaps ?? "-")}}${{unknown}}`;
}}
document.getElementById("joined-videos").innerHTML = joinLog.length ? `<table>
  <thead>
    <tr><th>산출물</th><th>SHA-256</th><th>원본 수</th><th>채널</th><th>순서 근거</th><th>총 길이</th><th>길이·경계 검사</th><th>끊김·겹침</th><th>매니페스트 SHA-256</th><th>감사 체인</th><th>판독 (상태·메모·구간·태그)</th></tr>
  </thead>
  <tbody>
    ${{joinLog.map(item => `<tr>
      <td><code>${{escapeHtml(item.output_path || "-")}}</code></td>
      <td><code>${{escapeHtml(item.output_sha256 || "-")}}</code></td>
      <td>${{escapeHtml(item.segment_count ?? "-")}}${{item.hash_mismatches ? `<br><span class="muted">색인 해시와 불일치 ${{escapeHtml(item.hash_mismatches)}}개</span>` : ""}}</td>
      <td>${{escapeHtml(item.channel || "미확인")}}</td>
      <td>${{escapeHtml(item.order_basis || "-")}}</td>
      <td>${{fmtDuration(item.duration_seconds)}}</td>
      <td>${{joinLengthText(item)}}</td>
      <td>${{joinJunctionText(item)}}</td>
      <td><code>${{escapeHtml(item.manifest_sha256 || "-")}}</code></td>
      <td>${{chainCell("join", item.entry_sha256)}}</td>
      <td>${{joinMarkCell(item)}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : emptyOr("join", "<p>이어붙인 영상이 없습니다.</p>");
document.getElementById("join-caveat").hidden = joinLog.length === 0;
document.getElementById("join-caveat-time").hidden = !joinLog.some(item => item.order_basis !== "filename-order");
document.getElementById("join-caveat-order").hidden = !joinLog.some(item => item.order_basis === "filename-order");

// E9: a telemetry track cut at the point cap is never shown as complete.
{{
  const items = Object.entries(telemetry).filter(([, t]) => t && typeof t === "object");
  const pointsText = t => {{
    const kept = Number.isFinite(t.point_count) ? t.point_count : null;
    const total = Number.isFinite(t.total_points) ? t.total_points : null;
    if (t.truncated === true) return `전체 ${{total ?? "?"}}점 중 ${{kept ?? "?"}}점만 기록 — 잘림 (시작·끝 시각과 최고 속도는 기록된 점 기준)`;
    return `${{kept ?? total ?? "-"}}점`;
  }};
  document.getElementById("telemetry").innerHTML = items.length ? `<table>
  <thead><tr><th>추출</th><th>원본</th><th>출처</th><th>점 수</th><th>시작</th><th>끝</th><th>메모</th></tr></thead>
  <tbody>
    ${{items.map(([name, t]) => `<tr>
      <td><code>${{escapeHtml(name)}}</code></td>
      <td><code>${{escapeHtml(t.selector || t.source_path || "-")}}</code></td>
      <td>${{escapeHtml(t.source || "-")}}</td>
      <td>${{escapeHtml(pointsText(t))}}</td>
      <td>${{fmtUnix(t.first_ts_unix)}}</td>
      <td>${{fmtUnix(t.last_ts_unix)}}</td>
      <td>${{escapeHtml(t.note || "-")}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : emptyOr("telemetry", "<p>위치·텔레메트리 추출 기록이 없습니다.</p>");
}}

const chainEvents = [];
(Array.isArray(scanRuns) ? scanRuns : []).forEach(run => chainEvents.push({{
  ts: run.scanned_unix,
  name: "scan-folder (스캔·색인)",
  detail: `${{run.video_count ?? "-"}}건 색인 · ${{run.source_path || "-"}}${{(run.warnings || []).length ? ` · 경고 ${{run.warnings.length}}건` : ""}}`,
  chain: "-"
}}));
validationLog.forEach(item => chainEvents.push({{
  ts: item.validated_unix,
  name: item.event || "validate",
  detail: `${{item.selector || "-"}} → ${{item.validation_status || "-"}}`,
  chain: item.entry_sha256 || "-",
  log: "validation"
}}));
batchLog.forEach(item => chainEvents.push({{
  ts: null,
  name: item.event || "batch",
  detail: `요청 ${{item.requested ?? "-"}} · 성공 ${{item.ok ?? "-"}} · 실패 ${{item.failed ?? "-"}}`,
  chain: item.entry_sha256 || "-",
  log: "batch"
}}));
exportsLog.forEach(item => chainEvents.push({{
  ts: item.exported_unix ?? item.created_unix,
  name: "export-video (클립 내보내기)",
  detail: `${{item.selector || item.source_path || "-"}} → ${{item.output_path || "-"}} (${{item.format || "-"}})`,
  chain: item.entry_sha256 || "-",
  log: "export"
}}));
derivedLog.forEach(item => chainEvents.push({{
  ts: item.created_unix,
  name: item.kind === "proxy" ? "make-proxy (검토용 프록시)" : "make-thumbnail (썸네일)",
  detail: `${{item.source_path || "-"}} → ${{item.output_path || "-"}}`,
  chain: item.entry_sha256 || "-",
  log: item.kind === "proxy" ? "proxy" : "thumbnail"
}}));
joinLog.forEach(item => chainEvents.push({{
  ts: item.joined_unix,
  name: "join-video (이어붙이기)",
  detail: `원본 ${{item.segment_count ?? "-"}}개 → ${{item.output_path || "-"}}`,
  chain: item.entry_sha256 || "-",
  log: "join"
}}));
filesystemLog.forEach(item => chainEvents.push({{
  ts: item.recovered_unix ?? item.inspected_unix,
  name: item.event || "filesystem",
  detail: `inode ${{item.inode ?? "-"}} @ ${{item.partition_offset ?? "-"}} → ${{item.output_path || item.summary_path || `${{item.entry_count ?? "-"}} entries`}}`,
  chain: item.entry_sha256 || "-",
  log: "filesystem"
}}));
chainEvents.sort((a, b) => (a.ts ?? Infinity) - (b.ts ?? Infinity));

document.getElementById("chain").innerHTML = chainEvents.length ? `<table>
  <thead>
    <tr><th>시각</th><th>처리</th><th>내용</th><th>체인 해시</th></tr>
  </thead>
  <tbody>
    ${{chainEvents.map(event => `<tr>
      <td>${{event.ts ? fmtUnix(event.ts) : "-"}}</td>
      <td>${{escapeHtml(event.name)}}</td>
      <td><code>${{escapeHtml(event.detail)}}</code></td>
      <td>${{event.log ? chainCell(event.log, event.chain) : `<code>${{escapeHtml(event.chain)}}</code>`}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : "<p>처리 이력이 없습니다.</p>";

const chainLogNames = [
  ["export", "클립 내보내기"], ["proxy", "검토용 프록시"], ["thumbnail", "썸네일"],
  ["carve", "carving"], ["filesystem", "파일시스템 조사·복구"], ["validation", "재생/컨테이너 검증"],
  ["anomaly", "이상 징후 스캔"], ["batch", "일괄 처리"], ["join", "이어붙이기"],
  ["audio", "소리 측정"], ["signoff", "최종화 서명 기록"]
];
const chainStatusText = chain => {{
  switch (chain?.status) {{
    case "keyed": return "integrity-keyed — 모든 항목이 설정된 키(HMAC)로 확인됨";
    case "structural": return "구조만 확인 (integrity-structural-only) — 자기 일관성만 확인(인증 아님): 항목 연결은 맞으나 감사 키로 인증되지 않아 로그 전체 재작성은 탐지 못함";
    case "repaired": return `수리됨 (repair-lane) — 검증되는 앞부분까지 잘라 내고 lane-repair 표식으로 기록함; 남은 체인: ${{chain.base_status === "keyed" ? "integrity-keyed" : "구조만 확인(자기 일관성만, 인증 아님)"}}; 잘린 부분의 기록은 읽지 않음`;
    case "failed": return "검증 실패";
    case "not-verified": return "확인 불가 (NOT VERIFIED) — 이 PC에서 인증되지 않은 항목이 있음 (다른 PC 키면 그 키 보유 PC에서 확인, 상세 참조)";
    case "absent": return "로그 없음";
    default: return "미검증 (이 보고서 생성 시 검증하지 않음)";
  }}
}};
// R9 P3: entries this PC could not authenticate — listed, never results.
// R19 I3: a key id is forgeable (outside entry_sha256) — quoted only when
// it matches ^[0-9a-z-]{{1,64}}$ (and is not "unknown").
const quotableKeyPhrase = id => /^[0-9a-z-]{{1,64}}$/.test(id) && id !== "unknown" ? `키 "${{id}}"` : "알 수 없는 키 id";
const unauthTrustText = trust => typeof trust === "string" && trust.startsWith("pc:") ? `이 PC에서 인증 불가 — ${{quotableKeyPhrase(trust.slice(3))}} 보유 PC에서 확인 (변조 판정 아님)` : "인증 안 됨";
const unauthSubject = item => item?.output_path || item?.target_path || item?.source_path || item?.selector || item?.record_id || item?.id || "-";
document.getElementById("unauthenticated-entries").innerHTML = unauthenticatedEntries.length ? `<div class="note"><strong>인증 안 된 감사 항목 ${{unauthenticatedEntries.length}}건</strong> — 이 PC에서 인증되지 않아 위 표·집계·검증 결과에 쓰지 않았습니다 (결과·근거로 보지 마십시오).</div><table>
  <thead><tr><th>로그</th><th>항목</th><th>대상</th><th>신뢰</th><th>사유</th></tr></thead>
  <tbody>
    ${{unauthenticatedEntries.map(({{ name, item }}) => `<tr>
      <td>${{escapeHtml(name)}}</td>
      <td>${{escapeHtml(item?.event || item?.kind || "-")}}</td>
      <td><code>${{escapeHtml(unauthSubject(item))}}</code></td>
      <td>${{escapeHtml(unauthTrustText(item?.trust))}}</td>
      <td>${{escapeHtml(item?.trust_note || "-")}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : "";

document.getElementById("audit-chains").innerHTML = `<table>
  <thead>
    <tr><th>로그</th><th>경로</th><th>항목 수</th><th>검증 결과</th><th>상세</th></tr>
  </thead>
  <tbody>
    ${{chainLogNames.map(([key, label]) => {{
      const chain = auditChains[key];
      return `<tr>
      <td>${{escapeHtml(label)}}</td>
      <td><code>${{escapeHtml(chain?.path || "-")}}</code></td>
      <td>${{escapeHtml(chain?.entries ?? "-")}}</td>
      <td>${{escapeHtml(chainStatusText(chain))}}</td>
      <td>${{escapeHtml(chain?.detail || "-")}}</td>
    </tr>`;
    }}).join("")}}
  </tbody>
</table>`;
</script>
</main>
</body>
</html>
"#,
        inputs.redaction_applied
    )
}

/// Relative path of the finalize sign-off record inside a case.
pub const SIGNOFF_REL_PATH: &str = "review/signoff.json";

/// Parses `review/signoff.json` into `(examiner, signed_unix)`.
pub fn parse_signoff(text: &str) -> Option<(String, i64)> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let examiner = value.get("examiner")?.as_str()?.trim().to_string();
    if examiner.is_empty() {
        return None;
    }
    let signed = value
        .get("signed_unix")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Some((examiner, signed))
}

/// Digest of the content a finalize sign-off covers: since sign-off
/// schema v3 the review marks with their IN-OUT ranges plus the tag map
/// (`signoff_content_json`); v2 records digested the report's marks JSON
/// (`review_marks_json`: status / time / examiner / note only).
pub fn signoff_content_sha256(content_json: &str) -> String {
    crate::sha256::digest_bytes(content_json.as_bytes())
}

/// Sign-off schema version whose `content_sha256` covers ranges and tags.
pub const SIGNOFF_SCHEMA_VERSION: u64 = 3;

/// The current content digests a sign-off record is compared with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignoffDigests {
    /// v3 binding: marks (with IN-OUT ranges) and tags.
    pub content: String,
    /// v2 binding: the report's marks JSON (status / time / examiner / note).
    pub legacy: String,
}

/// Whether the content a sign-off was given for is still what the report
/// shows (SHOULD-FIX 6): an old sign-off must never read as current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignoffCurrency {
    /// Marks (incl. ranges) and tags are unchanged since the sign-off.
    Current,
    /// The review marks changed after the sign-off.
    Changed,
    /// An older (schema < 3) record whose status/note binding still
    /// matches, but which never covered ranges or tags: whether those
    /// changed cannot be checked.
    LegacyBinding,
    /// The record predates content binding; currency cannot be checked.
    Unbound,
    /// R6-I1: a v4 record whose every CHECKED binding matches, but with at
    /// least one item that could not be checked — never "current".
    PartiallyVerified,
}

impl SignoffCurrency {
    pub fn label(self) -> &'static str {
        match self {
            Self::Current => "sign-off 이후 판독 마크(구간·태그 포함) 변경 없음",
            Self::Changed => "sign-off 이후 판독 마크가 변경됨 — 현재 내용에 대한 sign-off가 아님",
            Self::LegacyBinding => {
                "이전 형식 sign-off — 마크 상태·메모는 일치하나 IN-OUT 구간·태그 변경 여부는 확인 불가"
            }
            Self::Unbound => "sign-off 대상 내용 해시 없음 — 이후 변경 여부 확인 불가",
            Self::PartiallyVerified => {
                "확인한 항목은 일치하나 일부 항목 확인 불가 — 서명 일치를 단정할 수 없음"
            }
        }
    }
}

/// Compares the sign-off's recorded `content_sha256` with the current
/// content. A record below [`SIGNOFF_SCHEMA_VERSION`] is compared with the
/// legacy digest: a mismatch is a real change, a match only says status and
/// notes are unchanged (ranges/tags were never bound).
pub fn signoff_currency(signoff_text: &str, current: &SignoffDigests) -> SignoffCurrency {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(signoff_text) else {
        return SignoffCurrency::Unbound;
    };
    let Some(recorded) = value.get("content_sha256").and_then(|v| v.as_str()) else {
        return SignoffCurrency::Unbound;
    };
    let schema = value
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if schema > SIGNOFF_SCHEMA_V4 {
        // R7-I2: a schema this build never writes is never "current".
        return SignoffCurrency::Unbound;
    }
    if schema == SIGNOFF_SCHEMA_V4 {
        // D13: a v4 record's content_sha256 also covers the evidence and
        // lane anchors, which only `check_signoff` can recompute; the
        // marks/tags/ranges part is its `annotations_sha256` (the v3
        // digest). Callers holding only the digests get that part.
        return match value.get("annotations_sha256").and_then(|v| v.as_str()) {
            Some(annotations) if annotations == current.content => SignoffCurrency::Current,
            Some(_) => SignoffCurrency::Changed,
            None => SignoffCurrency::Unbound,
        };
    }
    if schema >= SIGNOFF_SCHEMA_VERSION {
        if recorded == current.content {
            SignoffCurrency::Current
        } else {
            SignoffCurrency::Changed
        }
    } else if recorded == current.legacy {
        SignoffCurrency::LegacyBinding
    } else {
        SignoffCurrency::Changed
    }
}

/// Banner naming the reviewer recorded at finalize time. The wording is
/// deliberately "recorded as entered": the workstation has no login, so the
/// name is the examiner's own typed statement, not an authenticated identity.
/// It also states whether the review marks changed after that sign-off.
pub fn signoff_banner_html(examiner: &str, signed_unix: i64, currency: SignoffCurrency) -> String {
    let (background, border, tail) = match currency {
        SignoffCurrency::Current => (
            "#eef4f8",
            "#b9d0e0",
            " — 이 기록 이후 판독 마크는 변경되지 않았습니다 (상태·메모·IN-OUT 구간·태그 모두 일치). (아래 '판독' 열에 구간·태그 표시)".to_string(),
        ),
        SignoffCurrency::Changed => (
            "#fce8e6",
            "#e6a49c",
            " — <strong>주의: 이 기록 이후 판독 마크가 변경되었습니다. 아래 내용은 검토자가 최종화한 시점과 다르며, 이 기록은 현재 내용에 대한 sign-off가 아닙니다. 다시 최종화하십시오.</strong>".to_string(),
        ),
        SignoffCurrency::LegacyBinding => (
            "#fff4e0",
            "#f4c790",
            " — 이 기록은 이전 형식이라 판독 마크의 상태·메모만 대상으로 합니다(이후 변경 없음). IN-OUT 구간·태그가 이후 바뀌었는지는 확인할 수 없습니다 — 다시 최종화하면 함께 기록됩니다.".to_string(),
        ),
        SignoffCurrency::Unbound => (
            "#fff4e0",
            "#f4c790",
            " — 이 기록에는 대상 내용의 해시가 없어 이후 판독 마크 변경 여부를 확인할 수 없습니다.".to_string(),
        ),
        // R6-I1: amber, never the "unchanged" wording.
        SignoffCurrency::PartiallyVerified => (
            "#fff4e0",
            "#f4c790",
            " — <strong>일부 항목 확인 불가 — 서명 일치를 단정할 수 없습니다.</strong> 확인한 항목은 서명 시와 일치합니다(아래 목록).".to_string(),
        ),
    };
    format!(
        "<div style=\"padding:8px 16px;background:{background};border-bottom:1px solid {border};font-size:13px\">검토자: <strong>{}</strong> (최종화 시 입력·기록된 이름이며 신원 인증은 아닙니다; 기록 시각 {} UTC, unix {}){tail}</div>",
        crate::util::html_escape(examiner),
        crate::custody::utc_timestamp(signed_unix),
        signed_unix
    )
}

/// HTML comment naming the exact sign-off record a report was rendered
/// with (SHA-256 of the record file's bytes), so `package-case` packages
/// that record and not a leftover one (I10).
pub fn signoff_source_marker(signoff_text: &str) -> String {
    format!(
        "<!-- frametrace:signoff-sha256={} -->",
        crate::sha256::digest_bytes(signoff_text.as_bytes())
    )
}

/// The sign-off record digest a rendered report names (see
/// [`signoff_source_marker`]); `None` for a report without a sign-off or
/// one rendered before the marker existed.
pub fn report_signoff_sha256(html: &str) -> Option<&str> {
    let start =
        html.find("<!-- frametrace:signoff-sha256=")? + "<!-- frametrace:signoff-sha256=".len();
    let hex = html.get(start..start + 64)?;
    (hex.bytes().all(|b| b.is_ascii_hexdigit()) && html[start + 64..].starts_with(" -->"))
        .then_some(hex)
}

/// D10 helper for `verify-package` check 5: whether a rendered report was
/// rendered from exactly this sign-off record — the report's
/// `frametrace:signoff-sha256` marker must equal the SHA-256 of the
/// record's bytes (an examiner-name comparison alone accepts a swapped
/// record). `Err` names what differs.
pub fn report_matches_signoff(report_html: &str, signoff_text: &str) -> Result<(), String> {
    let actual = crate::sha256::digest_bytes(signoff_text.as_bytes());
    match report_signoff_sha256(report_html) {
        Some(marker) if marker == actual => Ok(()),
        Some(marker) => Err(format!(
            "the report was rendered from sign-off record {marker}, not from this record ({actual})"
        )),
        None => Err(
            "the report carries no sign-off record marker (rendered without a sign-off, or before markers existed)"
                .to_string(),
        ),
    }
}

/// Sign-off content schema v4 (D13, contract §8 / plan "§D4 as
/// implemented"): the v3 binding (marks with IN-OUT ranges, tags) plus the
/// SHA-256 of each annotated record's target file and the head of every
/// audit lane at sign-off time (truncation anchors).
pub const SIGNOFF_SCHEMA_V4: u64 = 4;

/// The chained audit lane every sign-off is recorded in (D13).
pub const SIGNOFF_LOG_REL_PATH: &str = "evidence/logs/signoff-log.jsonl";

/// What a v4 sign-off binds besides the marks: record id → target file
/// SHA-256 (`None` when the case had no hash for it), and lane → head.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignoffBindings {
    pub evidence: std::collections::BTreeMap<String, Option<String>>,
    pub lanes: std::collections::BTreeMap<String, crate::audit::LaneHead>,
}

/// Every record id → SHA-256 the case (or package) at `root` knows:
/// indexed videos (`db/videos.jsonl`), carve candidates, recovered inodes
/// (`inode:<offset>:<inode>`, the report's id) and joined outputs
/// (`join:<hash16>`). Later log entries win. A missing source is empty; an
/// unreadable one is an error.
/// The parseable rows of the case index `db/videos.jsonl` (plain data, not
/// a chained lane). Missing = empty; unreadable = error.
fn index_rows(root: &std::path::Path) -> Result<Vec<serde_json::Value>, String> {
    match std::fs::read_to_string(root.join("db/videos.jsonl")) {
        Ok(text) => Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(format!("db/videos.jsonl을(를) 읽지 못했습니다: {err}")),
    }
}

pub fn evidence_hashes(
    root: &std::path::Path,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    // R9 P3/I3: a hash a sign-off binds or is compared with comes only from
    // a lane entry usable here — the LAST such entry per id. An id whose
    // only entries are not authenticated (keys held) has no hash here
    // ("확인 불가"), never the unauthenticated value.
    let lane_rows = |rel: &str| -> Result<Vec<serde_json::Value>, String> {
        crate::audit::read_lane_rows(&root.join(rel))
            .map(|lane| lane.usable_values())
            .map_err(|err| format!("{rel}을(를) 읽지 못했습니다: {err}"))
    };
    let field = |value: &serde_json::Value, key: &str| -> Option<String> {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let mut out = std::collections::BTreeMap::new();
    for row in index_rows(root)? {
        if let (Some(id), Some(sha)) = (field(&row, "id"), field(&row, "sha256")) {
            out.insert(id, sha.to_ascii_lowercase());
        }
    }
    for row in lane_rows("artifacts/carved/carve-log.jsonl")? {
        if let (Some(id), Some(sha)) = (field(&row, "id"), field(&row, "sha256")) {
            out.insert(id, sha.to_ascii_lowercase());
        }
    }
    for row in lane_rows("evidence/logs/tsk-audit.jsonl")? {
        if field(&row, "event").as_deref() != Some("recover-inode") {
            continue;
        }
        let offset = row
            .get("partition_offset")
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_else(|| "0".to_string());
        let inode = row
            .get("inode")
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .or_else(|| field(&row, "output_path"))
            .unwrap_or_default();
        if let Some(sha) = field(&row, "sha256") {
            out.insert(format!("inode:{offset}:{inode}"), sha.to_ascii_lowercase());
        }
    }
    for row in lane_rows("artifacts/joined/join-log.jsonl")? {
        let row = crate::join::with_record_id(row);
        if let (Some(id), Some(sha)) = (field(&row, "record_id"), field(&row, "output_sha256")) {
            out.insert(id, sha.to_ascii_lowercase());
        }
    }
    Ok(out)
}

/// The bindings of `ids` at `root` now: each id's target SHA-256 and the
/// head of every chained lane present ([`crate::audit::collect_chain_lanes`]).
pub fn signoff_bindings<'a>(
    root: &std::path::Path,
    ids: impl IntoIterator<Item = &'a str>,
) -> Result<SignoffBindings, String> {
    let hashes = evidence_hashes(root)?;
    let evidence = ids
        .into_iter()
        .map(|id| (id.to_string(), hashes.get(id).cloned()))
        .collect();
    let mut lanes = std::collections::BTreeMap::new();
    for rel in crate::audit::collect_chain_lanes(root) {
        if let Some(head) = crate::audit::lane_head(&root.join(&rel))
            .map_err(|err| format!("감사 레인 {rel}의 앵커를 읽지 못했습니다: {err}"))?
        {
            lanes.insert(rel, head);
        }
    }
    Ok(SignoffBindings { evidence, lanes })
}

fn json_str(text: &str) -> String {
    format!("\"{}\"", crate::util::json_escape(text))
}

fn bindings_json(bindings: &SignoffBindings) -> (String, String) {
    let evidence = bindings
        .evidence
        .iter()
        .map(|(id, sha)| {
            format!(
                "{}:{}",
                json_str(id),
                sha.as_deref()
                    .map(json_str)
                    .unwrap_or_else(|| "null".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let lanes = bindings
        .lanes
        .iter()
        .map(|(rel, head)| {
            format!(
                "{}:{{\"entries\":{},\"last_entry_sha256\":{}}}",
                json_str(rel),
                head.entries,
                json_str(&head.last_entry_sha256)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    (format!("{{{evidence}}}"), format!("{{{lanes}}}"))
}

/// The canonical v4 content a sign-off's `content_sha256` covers:
/// `{"schema":4,"annotations_sha256":"<v3 digest>","evidence":{…},"lanes":{…}}`
/// with ids and lanes in byte order — the same inputs always give the same
/// bytes.
pub fn signoff_v4_content_json(annotations_sha256: &str, bindings: &SignoffBindings) -> String {
    let (evidence, lanes) = bindings_json(bindings);
    format!(
        "{{\"schema\":{SIGNOFF_SCHEMA_V4},\"annotations_sha256\":{},\"evidence\":{evidence},\"lanes\":{lanes}}}",
        json_str(annotations_sha256)
    )
}

/// A finalize sign-off record (`review/signoff.json`), as written and as
/// recorded in the sign-off audit lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signoff {
    pub schema_version: u64,
    pub examiner: String,
    pub signed_unix: i64,
    pub content_sha256: Option<String>,
    /// The record's exact bytes (newline-terminated JSON object).
    pub text: String,
}

impl Signoff {
    /// Parses a record (any schema). `None` without an examiner.
    pub fn parse(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let (examiner, signed_unix) = parse_signoff(text)?;
        Some(Self {
            schema_version: value
                .get("schema_version")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            examiner,
            signed_unix,
            content_sha256: value
                .get("content_sha256")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            text: text.to_string(),
        })
    }

    /// Builds a schema-v4 record for the case's current marks, tags,
    /// evidence hashes and lane heads. Deterministic: the same case state,
    /// examiner and time give the same bytes. A mark/tag read failure is
    /// an error (finding 8: never sign an empty list by accident).
    pub fn build_v4(
        case_dir: &std::path::Path,
        examiner: &str,
        signed_unix: i64,
    ) -> Result<Self, String> {
        let marks = crate::case_db::load_review_marks(case_dir)
            .map_err(|err| format!("판독 마크를 읽지 못했습니다: {err}"))?;
        let tags = crate::case_db::load_review_tags(case_dir)
            .map_err(|err| format!("판독 태그를 읽지 못했습니다: {err}"))?;
        let annotations_sha256 = signoff_content_sha256(
            &crate::cli::handlers::signoff_content_json_from(&marks, &tags),
        );
        let mut ids: std::collections::BTreeSet<&str> =
            marks.iter().map(|mark| mark.record_id.as_str()).collect();
        ids.extend(
            tags.iter()
                .filter(|entry| !entry.tags.is_empty())
                .map(|entry| entry.id.as_str()),
        );
        let bindings = signoff_bindings(case_dir, ids)?;
        let content_sha256 =
            signoff_content_sha256(&signoff_v4_content_json(&annotations_sha256, &bindings));
        let (evidence, lanes) = bindings_json(&bindings);
        // R7-I7: the sign-off lane's entry count now — this record's own
        // entry will be the next one (checked by `signoff_audit_record`).
        let signoff_log_entries = crate::audit::lane_head(&case_dir.join(SIGNOFF_LOG_REL_PATH))
            .map_err(|err| format!("서명 감사 레인을 읽지 못했습니다: {err}"))?
            .map_or(0, |head| head.entries);
        let text = format!(
            "{{\"schema_version\":{SIGNOFF_SCHEMA_V4},\"examiner\":{},\"signed_unix\":{signed_unix},\"content\":\"review marks (status, note, IN-OUT range), tags, the SHA-256 of each annotated record's target file, and the head of every audit lane\",\"annotations_sha256\":{},\"evidence\":{evidence},\"lanes\":{lanes},\"content_sha256\":{},\"signoff_log_entries\":{signoff_log_entries},\"note\":\"examiner name recorded as typed at finalize; not an authenticated identity\"}}\n",
            json_str(examiner),
            json_str(&annotations_sha256),
            json_str(&content_sha256),
        );
        Ok(Self {
            schema_version: SIGNOFF_SCHEMA_V4,
            examiner: examiner.to_string(),
            signed_unix,
            content_sha256: Some(content_sha256),
            text,
        })
    }

    /// SHA-256 of the record's bytes — the identity the report marker and
    /// the sign-off audit entry name.
    pub fn record_sha256(&self) -> String {
        crate::sha256::digest_bytes(self.text.as_bytes())
    }
}

/// D13 (contract §8, called by finalize — B18): appends the sign-off to the
/// chained audit lane [`SIGNOFF_LOG_REL_PATH`] (HMAC-keyed when an audit
/// key is configured), naming the record by the SHA-256 of its bytes. A
/// failure is an error: a sign-off that is not in the chain must not be
/// presented as recorded.
pub fn record_signoff_audit(case_dir: &std::path::Path, signoff: &Signoff) -> Result<(), String> {
    crate::audit::append_chained_jsonl(
        &case_dir.join(SIGNOFF_LOG_REL_PATH),
        &signoff_audit_body(signoff),
    )
    .map_err(|err| format!("최종화 서명을 감사 체인에 기록하지 못했습니다: {err}"))
}

/// The sign-off lane entry body [`record_signoff_audit`] appends.
fn signoff_audit_body(signoff: &Signoff) -> String {
    format!(
        "{{\"schema_version\":1,\"event\":\"signoff\",\"signed_unix\":{},\"examiner\":{},\"signoff_schema_version\":{},\"content_sha256\":{},\"signoff_record_sha256\":{}}}",
        signoff.signed_unix,
        json_str(&signoff.examiner),
        signoff.schema_version,
        signoff
            .content_sha256
            .as_deref()
            .map(json_str)
            .unwrap_or_else(|| "null".to_string()),
        json_str(&signoff.record_sha256()),
    )
}

/// R5-P1: what the sign-off audit lane says about one sign-off record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignoffAuditRecord {
    /// The lane verifies and holds an entry naming exactly this record
    /// (record SHA-256 and `content_sha256`) as its LATEST sign-off (R6-I7),
    /// authenticated by HMAC when an audit key is configured. `integrity`
    /// is the lane's grade label. R6-I6: `authenticated` is false when the
    /// entry's HMAC was not checked here (no key on this PC, or no HMAC in
    /// a keyless case) — then the match proves self-consistency only.
    Verified {
        integrity: &'static str,
        authenticated: bool,
        /// R7-I5: the entry IS HMAC-signed (under this key id) but this PC
        /// holds no audit key to check it — the remedy is checking it on a
        /// PC that holds that key, not re-finalizing.
        unchecked_key_id: Option<String>,
    },
    /// Nothing to check against (no sign-off lane, or a package that does
    /// not carry it, or a keyed check that cannot be made): the record
    /// cannot be confirmed — never presented as current.
    NotVerifiable(String),
    /// The lane contradicts the record: its chain does not verify, or it
    /// records sign-offs but none naming this record (a swapped or
    /// re-computed record), or the entry for it names another content
    /// digest.
    Contradicted(String),
}

impl SignoffAuditRecord {
    /// The one line the report banner and `verify-package` show.
    pub fn note(&self) -> String {
        match self {
            Self::Verified {
                integrity,
                authenticated: true,
                ..
            } => {
                format!("서명 기록이 감사 체인(signoff-log, {integrity})에 있음 — HMAC으로 인증됨")
            }
            Self::Verified {
                integrity,
                authenticated: false,
                unchecked_key_id: Some(key_id),
            } => {
                // R19 I3: a forgeable id is quoted only when quotable.
                let phrase = crate::audit::key_id_phrase(key_id);
                format!(
                    "서명 기록이 감사 체인(signoff-log, {integrity})에 있음 — {SELF_CONSISTENT_ONLY} 이 항목은 감사 {phrase}로 HMAC 서명되어 있으나 이 PC에는 감사 키가 없습니다 — {phrase}를 가진 PC에서 확인하십시오"
                )
            }
            Self::Verified {
                integrity,
                authenticated: false,
                unchecked_key_id: None,
            } => format!(
                "서명 기록이 감사 체인(signoff-log, {integrity})에 있음 — {SELF_CONSISTENT_ONLY} {REFINALIZE_TO_AUTHENTICATE}"
            ),
            Self::NotVerifiable(why) => format!("서명 확인 불가 — {why}"),
            Self::Contradicted(why) => format!("서명 기록 불일치 — {why}"),
        }
    }
}

/// R6-I6: what a match in a keyless (or key-less-here) sign-off lane
/// proves — anyone with the case folder can recompute such an entry.
pub const SELF_CONSISTENT_ONLY: &str = "자기 일관성만 확인(인증 아님): 감사 키 HMAC으로 확인되지 않은 항목이라 케이스 폴더를 가진 누구나 다시 계산할 수 있습니다.";

/// R7-I5: the remedy for a keyless sign-off — only when the entry carries
/// no HMAC at all. An HMAC-signed entry on a PC without the key is checked
/// on the key's PC instead (see [`SignoffAuditRecord::note`]).
pub const REFINALIZE_TO_AUTHENTICATE: &str = "인증하려면 감사 키를 설정한 뒤 다시 최종화하십시오";

/// R6-I8: the remedy for a sign-off finalized without a key in a case
/// that is now keyed.
pub const REFINALIZE_WITH_KEY: &str = "키 없이 최종화한 뒤 감사 키를 설정했다면, 키 설정 후 다시 최종화해야 서명을 확인할 수 있습니다";

/// R5-P1: checks `signoff_text` against the sign-off audit lane under
/// `root`. `lane_listed` is `false` when a package does not carry the lane.
pub fn signoff_audit_record(
    root: &std::path::Path,
    signoff_text: &str,
    lane_listed: bool,
) -> SignoffAuditRecord {
    let keys = match crate::audit_key::verification_keys() {
        Ok(keys) => keys,
        Err(err) => {
            return SignoffAuditRecord::NotVerifiable(format!(
                "감사 키를 읽지 못해 서명 감사 기록을 확인할 수 없습니다: {err}"
            ));
        }
    };
    signoff_audit_record_with_keys(root, signoff_text, lane_listed, &keys)
}

/// [`signoff_audit_record`] with an explicit verification key set.
pub fn signoff_audit_record_with_keys(
    root: &std::path::Path,
    signoff_text: &str,
    lane_listed: bool,
    keys: &[crate::audit_key::AuditKey],
) -> SignoffAuditRecord {
    let path = root.join(SIGNOFF_LOG_REL_PATH);
    if !lane_listed {
        return SignoffAuditRecord::NotVerifiable(
            "패키지에 서명 감사 레인(signoff-log)이 없어 이 서명이 기록된 것인지 확인할 수 없습니다"
                .to_string(),
        );
    }
    // R9 P3: the lane's rows through the one lane reader.
    let lane = match crate::audit::read_lane_rows_with_keys(&path, keys) {
        Ok(lane) if !lane.present => {
            return SignoffAuditRecord::NotVerifiable(
                "감사 체인(signoff-log)에 서명 기록이 없습니다 — 이 서명이 최종화로 기록된 것인지 확인할 수 없습니다"
                    .to_string(),
            );
        }
        Ok(lane) => lane,
        Err(err) => {
            return SignoffAuditRecord::NotVerifiable(format!(
                "서명 감사 레인을 읽지 못했습니다: {err}"
            ));
        }
    };
    if let Some(err) = &lane.failure {
        return SignoffAuditRecord::Contradicted(format!("서명 감사 레인 검증 실패: {err}"));
    }
    let Some(verification) = lane.verification.as_ref() else {
        return SignoffAuditRecord::NotVerifiable(
            "서명 감사 레인을 확인하지 못했습니다".to_string(),
        );
    };
    let record = crate::sha256::digest_bytes(signoff_text.as_bytes());
    let parsed = Signoff::parse(signoff_text);
    let schema = parsed.as_ref().map_or(0, |signoff| signoff.schema_version);
    if schema > SIGNOFF_SCHEMA_V4 {
        // R7-I2: a schema this build never writes is never confirmed.
        return SignoffAuditRecord::NotVerifiable(format!(
            "지원하지 않는 서명 형식(schema_version {schema})이라 감사 체인으로 확인할 수 없습니다"
        ));
    }
    // R7-I7: how many entries the sign-off lane held when this record was
    // signed (v4 records written from R7 on).
    let signed_lane_entries = serde_json::from_str::<serde_json::Value>(signoff_text)
        .ok()
        .and_then(|value| value.get("signoff_log_entries").and_then(|v| v.as_u64()));
    let content = parsed.and_then(|signoff| signoff.content_sha256);
    // R8-I9: sign-off entries of finalize attempts that FAILED after
    // recording (a trusted `signoff-abandoned` entry names them) are shown,
    // never counted as newer sign-offs.
    let abandoned = abandoned_signoff_records(&lane.rows);
    let mut signoff_entries = 0usize;
    let mut abandoned_entries = 0usize;
    // Line index of the LAST entry naming this record, and the live
    // sign-off entries recorded after it.
    let mut matching: Option<usize> = None;
    let mut later_signoffs = 0usize;
    // R8-I9: line index of the first live sign-off entry at or after the
    // position the record names (other events — rotation markers,
    // abandonment notes — are ignored for the position check).
    let mut first_signoff_from_expected: Option<usize> = None;
    // Row indices are the verifier's (`LaneRow::index`).
    for row in &lane.rows {
        let (index, value) = (row.index, &row.value);
        if value.get("event").and_then(|v| v.as_str()) != Some("signoff") {
            continue;
        }
        let entry_record = value.get("signoff_record_sha256").and_then(|v| v.as_str());
        // R10-I12 (P8): only an entry usable here can CONTRADICT the
        // record (content mismatch = "changed"). An unusable entry naming
        // this record with other content (e.g. an unkeyed line someone
        // appended where keys are held) is not this record's entry — it
        // counts as another sign-off entry ("not the latest": NOT
        // VERIFIED), never as a refutation.
        let entry_content = value.get("content_sha256").and_then(|v| v.as_str());
        let is_this = entry_record == Some(record.as_str())
            && (row.trust.is_usable() || entry_content == content.as_deref());
        if !is_this && entry_record.is_some_and(|sha| abandoned.contains(sha)) {
            abandoned_entries += 1;
            continue;
        }
        signoff_entries += 1;
        if signed_lane_entries.is_some_and(|expected| index as u64 >= expected) {
            first_signoff_from_expected.get_or_insert(index);
        }
        if is_this {
            if entry_content != content.as_deref() {
                return SignoffAuditRecord::Contradicted(format!(
                    "감사 체인의 이 서명 항목은 다른 결합 내용(content_sha256 {})을 기록합니다",
                    entry_content.unwrap_or("null")
                ));
            }
            matching = Some(index);
            later_signoffs = 0;
        } else if matching.is_some() {
            later_signoffs += 1;
        }
    }
    let abandoned_note = if abandoned_entries > 0 {
        format!(
            " (완료되지 않은 최종화 시도의 서명 항목 {abandoned_entries}건은 표시만 하고 판정에 쓰지 않음)"
        )
    } else {
        String::new()
    };
    let Some(index) = matching else {
        return if signoff_entries == 0 {
            SignoffAuditRecord::NotVerifiable(format!(
                "감사 체인(signoff-log)에 서명 항목이 없습니다 — 이 서명이 최종화로 기록된 것인지 확인할 수 없습니다{abandoned_note}"
            ))
        } else if schema < SIGNOFF_SCHEMA_V4 {
            // R6-I5: only a v4 finalize records its sign-off in this lane.
            // An older record is never in it — the lane's entries are later
            // finalize attempts (e.g. one that failed after recording), not
            // evidence against this record.
            SignoffAuditRecord::NotVerifiable(format!(
                "이전 형식(v{schema}) 서명 기록은 감사 체인(signoff-log)에 기록되지 않는 형식이라 확인할 수 없습니다 (체인의 서명 {signoff_entries}건은 이후의 최종화 시도){abandoned_note}"
            ))
        } else {
            SignoffAuditRecord::Contradicted(format!(
                "감사 체인(signoff-log)에는 서명 {signoff_entries}건이 기록되어 있으나 이 서명 기록(sha256 {record})은 없습니다 — 서명 기록이 교체되었거나 다시 계산되었습니다"
            ))
        };
    };
    // R9-I6: a record a trusted `signoff-abandoned` entry names belongs to a
    // finalize that FAILED after recording it. Restoring that record (e.g.
    // from a leftover pending file) must never make it Current.
    if abandoned.contains(&record) {
        return SignoffAuditRecord::NotVerifiable(
            "이 서명 기록은 완료되지 않은(포기된) 최종화 시도의 기록입니다 — 감사 체인의 포기 이벤트가 이 기록을 가리키므로 현재 서명으로 보지 않습니다 (다시 최종화하십시오)"
                .to_string(),
        );
    }
    // R6-I7: a genuine but OLDER sign-off restored over a newer one (or one
    // whose re-finalize never completed) is not the latest sign-off — when
    // the newer entries are still in the lane. R7-I7: removing them by
    // truncating the lane is NOT detected here (truncation of the lane's
    // own tail is a documented non-goal; see docs/audit-hmac-design.md).
    if later_signoffs > 0 {
        return SignoffAuditRecord::NotVerifiable(format!(
            "더 최신 서명 존재 — 이 기록은 최신이 아님: 감사 체인(signoff-log)에 이 서명 뒤로 서명 {later_signoffs}건이 더 기록되어 있습니다 (이전 서명 기록으로 되돌려졌거나, 이후 최종화가 완료되지 않음 — 다시 최종화하십시오)"
        ));
    }
    // R13 I2 (P16): a `lane-repair` marker after this record's entry — the
    // repair cut a tail that may have held a newer sign-off or this
    // record's abandonment: whether it is the latest is UNKNOWN.
    if lane.repaired_after(index) {
        return SignoffAuditRecord::NotVerifiable(format!(
            "{SIGNOFF_REPAIRED_AFTER_NOTE} — 잘린 꼬리에 더 최신 서명이나 이 서명의 포기 기록이 있었을 수 있어 이 서명이 최신인지 알 수 없습니다 (다시 최종화하십시오)"
        ));
    }
    // R7-I7 / R8-I9: the record names how many entries the lane held when
    // it was signed; its own entry must be the FIRST sign-off entry at or
    // after that position. Other events appended in between (a rotation
    // marker, an abandonment note — a race with the finalize) do not move
    // it; an entry before that position, or another sign-off in between,
    // means the lane before it was cut or re-written.
    if let Some(expected) = signed_lane_entries
        && first_signoff_from_expected != Some(index)
    {
        return SignoffAuditRecord::Contradicted(format!(
            "서명 시 서명 감사 레인(signoff-log)에는 항목 {expected}개가 있었으나 이 서명의 항목은 {}번째에 있습니다(그 위치 이후 첫 서명 항목이 아님) — 레인이 잘렸거나 재작성됨",
            index + 1
        ));
    }
    // R8 P0/P1/P2: what the entry may be relied on for is decided by THIS
    // PC holding keys (`verification.requirement`), never by whether the
    // entry carries an HMAC.
    match verification.line_trust(index) {
        crate::audit::LineTrust::SelfConsistentOnly { key_id } => {
            return SignoffAuditRecord::Verified {
                integrity: if key_id.is_some() {
                    "HMAC 항목이나 이 PC에 키가 없어 구조만 확인"
                } else {
                    verification.integrity.label()
                },
                authenticated: false,
                unchecked_key_id: key_id,
            };
        }
        crate::audit::LineTrust::Authenticated => {}
        trust => {
            return SignoffAuditRecord::NotVerifiable(signoff_entry_not_authenticated(&trust));
        }
    }
    // R7-I3: this PC authenticated part of the lane and a later part is
    // signed under a key id it does not hold — a re-written tail (e.g. the
    // newer sign-offs removed) cannot be ruled out.
    if let Some(reason) = &verification.rewrite_not_excluded {
        return SignoffAuditRecord::NotVerifiable(format!("서명 감사 레인(signoff-log): {reason}"));
    }
    SignoffAuditRecord::Verified {
        integrity: verification.integrity.label(),
        authenticated: true,
        unchecked_key_id: None,
    }
}

/// R8: why a sign-off entry that is not authenticated on a PC holding
/// audit keys does not confirm the record (P0) — and, for another PC's key,
/// where to check it (P2: not a FAIL).
fn signoff_entry_not_authenticated(trust: &crate::audit::LineTrust) -> String {
    match trust {
        crate::audit::LineTrust::OtherPc { key_id } => format!(
            "이 서명의 감사 항목은 이 PC에서 인증 불가 — {} 보유 PC에서 확인하십시오 (다른 PC의 감사 키로 서명됨, 변조 판정 아님)",
            crate::audit::key_id_phrase(key_id)
        ),
        crate::audit::LineTrust::NotAuthenticated {
            retired_unproven: true,
        } => "이 서명의 감사 항목은 은퇴한 감사 키로만 서명되었고 그 뒤에 현재 키의 항목이 없어, 은퇴 전에 기록된 것인지 보일 수 없습니다 — 서명 확인 불가 (현재 키로 다시 최종화하십시오)"
            .to_string(),
        crate::audit::LineTrust::PreKey => format!(
            "이 서명의 감사 항목은 감사 키 도입 이전에 기록되어 {} — 이 PC에 감사 키가 있어 인증되지 않은 서명은 확인된 것으로 표시하지 않습니다. 다시 최종화하십시오",
            crate::audit::SELF_CONSISTENT_NOTE
        ),
        crate::audit::LineTrust::RewriteNotExcluded { .. }
        | crate::audit::LineTrust::LocationUnverified
        | crate::audit::LineTrust::ChainBroken => format!(
            "이 서명의 감사 항목은 확인할 수 없습니다 — {} {REFINALIZE_WITH_KEY}",
            trust.note()
        ),
        _ => format!(
            "감사 키가 설정되어 있으나 이 서명의 감사 항목에 HMAC 서명이 없습니다 — 키 없이 추가됐거나 HMAC이 제거된 항목일 수 있어 서명을 확인할 수 없습니다. {REFINALIZE_WITH_KEY}"
        ),
    }
}

/// R8-I9: the event a finalize that recorded its sign-off entry but then
/// FAILED (report or package) appends to the sign-off lane, naming the
/// record it abandons. That sign-off entry is then shown only — it neither
/// makes the previous sign-off "not the latest" nor caps an older record.
pub const SIGNOFF_ABANDONED_EVENT: &str = "signoff-abandoned";

/// R8-I9: appends the [`SIGNOFF_ABANDONED_EVENT`] entry for `signoff_text`
/// (HMAC-keyed when a key is configured). For the finalize flow to call
/// when the report or package fails after [`record_signoff_audit`].
pub fn record_signoff_abandoned(
    case_dir: &std::path::Path,
    signoff_text: &str,
    reason: &str,
) -> Result<(), String> {
    record_signoff_abandoned_sha(
        case_dir,
        &crate::sha256::digest_bytes(signoff_text.as_bytes()),
        reason,
    )
}

/// R13-S1: [`record_signoff_abandoned`] for a record known by its SHA-256
/// (the finalize journal no longer carries the record text).
pub fn record_signoff_abandoned_sha(
    case_dir: &std::path::Path,
    signoff_sha256: &str,
    reason: &str,
) -> Result<(), String> {
    crate::audit::append_chained_jsonl(
        &case_dir.join(SIGNOFF_LOG_REL_PATH),
        &serde_json::json!({
            "schema_version": 1,
            "event": SIGNOFF_ABANDONED_EVENT,
            "signoff_record_sha256": signoff_sha256,
            "reason": reason,
        })
        .to_string(),
    )
    .map_err(|err| format!("완료되지 않은 최종화를 감사 체인에 기록하지 못했습니다: {err}"))
}

/// R13 I2: the sign-off lane was repaired after this record's entry.
pub const SIGNOFF_REPAIRED_AFTER_NOTE: &str =
    "서명 감사 레인(signoff-log)이 이 서명 항목 뒤에서 수리됨(lane-repair)";

/// R13-S1 (P15): the sign-off lane entry a finalize appends BEFORE its
/// journal and its sign-off entry — what a later recovery may do for that
/// finalize, keyed (HMAC) when an audit key is configured. The journal on
/// disk is only a pointer to it: a recovery acts only on a journal that
/// matches the lane's LATEST trusted begin entry with no
/// [`FINALIZE_COMPLETE_EVENT`] / [`FINALIZE_RESOLVED_EVENT`] after it.
pub const FINALIZE_BEGIN_EVENT: &str = "finalize-begin";
/// R13-S3 (P15): appended once a finalize completed (package published,
/// sign-off promoted) — before its journal is removed, so a journal whose
/// removal failed is only deleted by the next recovery.
pub const FINALIZE_COMPLETE_EVENT: &str = "finalize-complete";
/// R13 (P15): appended when an unfinished finalize was cleaned up (its
/// failure path or a recovery left nothing unresolved, or an examiner
/// dismissed its journal) — the begin entry is closed.
pub const FINALIZE_RESOLVED_EVENT: &str = "finalize-resolved";

/// R13-S1/S2/S4 (P15/P17): the fields of a [`FINALIZE_BEGIN_EVENT`] entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FinalizeBegin {
    /// Random id naming this finalize in the journal and the lane.
    pub finalize_id: String,
    /// SHA-256 of the pending sign-off record bytes.
    pub signoff_record_sha256: String,
    /// SHA-256 of what `review/signoff.json` held before (`None`: nothing)
    /// — a recovery writes back only bytes that hash to it.
    pub previous_signoff_sha256: Option<String>,
    /// The ONE `reports/` package name this finalize's package-case may
    /// publish (`package-case --package-name`); a recovery marks only it.
    pub package_name: String,
    /// [`crate::util::host_id`] of the PC that ran it (P17).
    pub host_id: String,
    pub pid: u32,
    /// The writer process's start time (unix ms) and executable name.
    pub writer_started_ms: Option<u64>,
    pub writer_image: Option<String>,
    pub started_unix: u64,
}

impl FinalizeBegin {
    fn body(&self) -> String {
        serde_json::json!({
            "schema_version": 1,
            "event": FINALIZE_BEGIN_EVENT,
            "finalize_id": self.finalize_id,
            "signoff_record_sha256": self.signoff_record_sha256,
            "previous_signoff_sha256": self.previous_signoff_sha256,
            "package_name": self.package_name,
            "host_id": self.host_id,
            "pid": self.pid,
            "writer_started_ms": self.writer_started_ms,
            "writer_image": self.writer_image,
            "started_unix": self.started_unix,
        })
        .to_string()
    }

    fn from_value(value: &serde_json::Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
        Some(Self {
            finalize_id: text("finalize_id")?,
            signoff_record_sha256: text("signoff_record_sha256")?,
            previous_signoff_sha256: text("previous_signoff_sha256"),
            package_name: text("package_name")?,
            host_id: text("host_id")?,
            pid: value.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            writer_started_ms: value.get("writer_started_ms").and_then(|v| v.as_u64()),
            writer_image: text("writer_image"),
            started_unix: value
                .get("started_unix")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        })
    }
}

/// R13 (P15): appends `begin` to the sign-off lane. A failure refuses the
/// finalize (nothing durable has happened yet).
pub fn record_finalize_begin(
    case_dir: &std::path::Path,
    begin: &FinalizeBegin,
) -> Result<(), String> {
    crate::audit::append_chained_jsonl(&case_dir.join(SIGNOFF_LOG_REL_PATH), &begin.body())
        .map_err(|err| format!("최종화 시작을 감사 체인에 기록하지 못했습니다: {err}"))
}

/// R13-S3 (P15): appends the [`FINALIZE_COMPLETE_EVENT`] of `finalize_id`.
pub fn record_finalize_complete(
    case_dir: &std::path::Path,
    finalize_id: &str,
    signoff_sha256: &str,
    package_name: &str,
    manifest_sha256: Option<&str>,
) -> Result<(), String> {
    crate::audit::append_chained_jsonl(
        &case_dir.join(SIGNOFF_LOG_REL_PATH),
        &serde_json::json!({
            "schema_version": 1,
            "event": FINALIZE_COMPLETE_EVENT,
            "finalize_id": finalize_id,
            "signoff_record_sha256": signoff_sha256,
            "package_name": package_name,
            "package_manifest_sha256": manifest_sha256,
        })
        .to_string(),
    )
    .map_err(|err| format!("최종화 완료를 감사 체인에 기록하지 못했습니다: {err}"))
}

/// R13 (P15): appends the [`FINALIZE_RESOLVED_EVENT`] of `finalize_id`
/// (`outcome`: `failed`, `recovered`, `dismissed`, `refused`).
pub fn record_finalize_resolved(
    case_dir: &std::path::Path,
    finalize_id: &str,
    signoff_sha256: Option<&str>,
    outcome: &str,
) -> Result<(), String> {
    crate::audit::append_chained_jsonl(
        &case_dir.join(SIGNOFF_LOG_REL_PATH),
        &serde_json::json!({
            "schema_version": 1,
            "event": FINALIZE_RESOLVED_EVENT,
            "finalize_id": finalize_id,
            "signoff_record_sha256": signoff_sha256,
            "outcome": outcome,
        })
        .to_string(),
    )
    .map_err(|err| format!("최종화 정리를 감사 체인에 기록하지 못했습니다: {err}"))
}

/// R13 (P15): what the sign-off lane says about the finalize `finalize_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalizeLanePairing {
    /// The lane's latest trusted begin entry is this finalize's and nothing
    /// closes it: an interrupted finalize a recovery may clean up, with
    /// exactly what that entry records.
    Open(FinalizeBegin),
    /// A trusted complete entry names it: the finalize completed.
    Completed { manifest_sha256: Option<String> },
    /// A trusted resolved entry names it: already cleaned up.
    Resolved { outcome: String },
    /// No trusted begin entry pairs with it (none, not the latest, not
    /// authenticated where keys are held, the lane does not verify, keys
    /// unreadable) — why. Nothing may be changed on its account.
    Unpaired(String),
}

/// R13 (P15): [`FinalizeLanePairing`] of `finalize_id` under this PC's
/// verification keys. Trusted = authenticated here, or self-consistent
/// only on a PC that holds no audit key (P1 — anyone with the folder can
/// write such entries there, as any other keyless record).
pub fn finalize_lane_pairing(case_dir: &std::path::Path, finalize_id: &str) -> FinalizeLanePairing {
    let keys = match crate::audit_key::verification_keys() {
        Ok(keys) => keys,
        Err(err) => return FinalizeLanePairing::Unpaired(err),
    };
    let lane =
        match crate::audit::read_lane_rows_with_keys(&case_dir.join(SIGNOFF_LOG_REL_PATH), &keys) {
            Ok(lane) if !lane.present => {
                return FinalizeLanePairing::Unpaired(
                    "서명 감사 레인(signoff-log)이 없습니다".to_string(),
                );
            }
            Ok(lane) => lane,
            Err(err) => {
                return FinalizeLanePairing::Unpaired(format!(
                    "서명 감사 레인을 읽지 못했습니다: {err}"
                ));
            }
        };
    // R14 (P19): an entry before a chain break is not usable either.
    let trusted = |row: &crate::audit::LaneRow| {
        row.lane_broken_at.is_none()
            && matches!(
                row.trust,
                crate::audit::LineTrust::Authenticated
                    | crate::audit::LineTrust::SelfConsistentOnly { .. }
            )
    };
    let event = |row: &crate::audit::LaneRow| {
        row.value
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let names_it = |row: &crate::audit::LaneRow| {
        row.value.get("finalize_id").and_then(|v| v.as_str()) == Some(finalize_id)
    };
    for row in lane.rows.iter().rev().filter(|row| trusted(row)) {
        match event(row).as_str() {
            FINALIZE_COMPLETE_EVENT if names_it(row) => {
                return FinalizeLanePairing::Completed {
                    manifest_sha256: row
                        .value
                        .get("package_manifest_sha256")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                };
            }
            FINALIZE_RESOLVED_EVENT if names_it(row) => {
                return FinalizeLanePairing::Resolved {
                    outcome: row
                        .value
                        .get("outcome")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                };
            }
            _ => {}
        }
    }
    let latest_begin = lane
        .rows
        .iter()
        .rev()
        .filter(|row| trusted(row) && event(row) == FINALIZE_BEGIN_EVENT)
        .find_map(|row| FinalizeBegin::from_value(&row.value).map(|begin| (row.index, begin)));
    let named = |only_trusted: bool| {
        lane.rows.iter().any(|row| {
            event(row) == FINALIZE_BEGIN_EVENT && names_it(row) && (!only_trusted || trusted(row))
        })
    };
    match latest_begin {
        // R14 (P19): what follows the begin entry must be known — an entry
        // this PC cannot vouch for (stripped HMAC, another key, a torn or
        // edited line) may be the cut-off complete / resolved entry, and
        // so may whatever a lane-repair removed after it.
        Some((index, begin)) if begin.finalize_id == finalize_id => {
            if lane.repaired_after(index) {
                FinalizeLanePairing::Unpaired(
                    "서명 감사 레인이 이 최종화의 시작 기록 뒤에서 수리되어(lane-repair) 완료 여부를 알 수 없습니다"
                        .to_string(),
                )
            } else if lane.rows[index + 1..].iter().any(|row| !trusted(row)) {
                FinalizeLanePairing::Unpaired(
                    "서명 감사 레인의 이 최종화 시작 기록 뒤에 이 PC에서 확인할 수 없는 항목이 있어 완료 여부를 알 수 없습니다"
                        .to_string(),
                )
            } else {
                FinalizeLanePairing::Open(begin)
            }
        }
        Some(_) if named(true) => FinalizeLanePairing::Unpaired(
            "이 최종화의 시작 기록 뒤에 다른 최종화가 시작되었습니다 (가장 최근 최종화가 아님)"
                .to_string(),
        ),
        _ if named(false) => FinalizeLanePairing::Unpaired(format!(
            "서명 감사 레인의 시작 기록이 이 PC에서 인증되지 않습니다{}",
            lane.failure
                .as_deref()
                .map(|err| format!(" (레인 검증 실패: {err})"))
                .unwrap_or_default()
        )),
        _ => FinalizeLanePairing::Unpaired(
            "서명 감사 레인(signoff-log)에 이 최종화의 시작 기록이 없습니다".to_string(),
        ),
    }
}

/// R14-S8: the lane holds a begin entry (ANY trust — another PC's keyed
/// entries do not authenticate here) of `finalize_id` naming
/// `package_name`, and no complete / resolved entry (any trust) of it.
/// Only for marking another PC's unfinished package beside it.
pub fn finalize_lane_mentions(
    case_dir: &std::path::Path,
    finalize_id: &str,
    package_name: &str,
) -> bool {
    // Every row counts whatever its trust (an empty key set: no key work).
    let Ok(lane) =
        crate::audit::read_lane_rows_with_keys(&case_dir.join(SIGNOFF_LOG_REL_PATH), &[])
    else {
        return false;
    };
    let mut begun = false;
    for value in lane.rows.iter().map(|row| &row.value) {
        if value.get("finalize_id").and_then(|v| v.as_str()) != Some(finalize_id) {
            continue;
        }
        match value.get("event").and_then(|v| v.as_str()) {
            Some(FINALIZE_BEGIN_EVENT) => {
                begun |= value.get("package_name").and_then(|v| v.as_str()) == Some(package_name);
            }
            Some(FINALIZE_COMPLETE_EVENT) | Some(FINALIZE_RESOLVED_EVENT) => return false,
            _ => {}
        }
    }
    begun
}

/// R12-S2/S6: where the sign-off record `signoff_text` stands in the
/// sign-off lane under `case_dir`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignoffLaneState {
    /// The lane holds a `signoff` entry naming this record (any trust).
    pub recorded: bool,
    /// A trusted [`SIGNOFF_ABANDONED_EVENT`] entry names this record.
    pub abandoned: bool,
    /// R13 I2 (P16): a usable `lane-repair` marker follows this record's
    /// last `signoff` entry ([`crate::audit::LaneRows::repaired_after`]):
    /// the cut tail may have held its abandonment — `abandoned: false` is
    /// then UNKNOWN, not "not abandoned" (check before acting on it).
    pub repaired_after: bool,
}

/// R12-S2/S6: [`SignoffLaneState`] of `signoff_text` with this PC's
/// verification keys. A finalize journal's recovery uses it to abandon a
/// recorded sign-off once (never twice), and a finalize to tell whether
/// the "previous" sign-off it would restore was itself abandoned.
pub fn signoff_lane_state(
    case_dir: &std::path::Path,
    signoff_text: &str,
) -> Result<SignoffLaneState, String> {
    signoff_lane_state_sha(
        case_dir,
        &crate::sha256::digest_bytes(signoff_text.as_bytes()),
    )
}

/// R13-S1: [`signoff_lane_state`] for a record known by its SHA-256.
pub fn signoff_lane_state_sha(
    case_dir: &std::path::Path,
    sha: &str,
) -> Result<SignoffLaneState, String> {
    let keys = crate::audit_key::verification_keys()?;
    let lane = crate::audit::read_lane_rows_with_keys(&case_dir.join(SIGNOFF_LOG_REL_PATH), &keys)?;
    if !lane.present {
        return Ok(SignoffLaneState::default());
    }
    let sha = sha.to_string();
    let entry_at = lane.rows.iter().rposition(|row| {
        row.value.get("event").and_then(|v| v.as_str()) == Some("signoff")
            && row
                .value
                .get("signoff_record_sha256")
                .and_then(|v| v.as_str())
                == Some(sha.as_str())
    });
    Ok(SignoffLaneState {
        recorded: entry_at.is_some(),
        abandoned: abandoned_signoff_records(&lane.rows).contains(&sha),
        repaired_after: entry_at.is_some_and(|at| lane.repaired_after(at)),
    })
}

/// R8-I9: the record SHA-256s trusted `signoff-abandoned` entries name.
/// P0: with audit keys on this PC only an AUTHENTICATED entry counts (an
/// unauthenticated one would let anyone hide a newer sign-off); without
/// keys everything is self-consistency only anyway (P1).
fn abandoned_signoff_records(rows: &[crate::audit::LaneRow]) -> std::collections::BTreeSet<String> {
    rows.iter()
        .filter_map(|row| {
            let value = &row.value;
            let trusted = matches!(
                row.trust,
                crate::audit::LineTrust::Authenticated
                    | crate::audit::LineTrust::SelfConsistentOnly { .. }
            );
            (value.get("event").and_then(|v| v.as_str()) == Some(SIGNOFF_ABANDONED_EVENT)
                && trusted)
                .then(|| {
                    value
                        .get("signoff_record_sha256")?
                        .as_str()
                        .map(str::to_string)
                })
                .flatten()
        })
        .collect()
}

/// Whether the sign-off record `signoff_text` is in the sign-off audit
/// lane under `root`: one line for the report banner / verify-package.
/// R5-P1: `true` only for a verified entry naming exactly this record.
pub fn signoff_audit_status(root: &std::path::Path, signoff_text: &str) -> (bool, String) {
    let record = signoff_audit_record(root, signoff_text, true);
    (
        matches!(record, SignoffAuditRecord::Verified { .. }),
        record.note(),
    )
}

/// R5-P2: a signed target file hashed again for this check, or why it
/// could not be (missing, locked, read error) — the latter is "not
/// verified", never "changed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveHash {
    Hashed(String),
    Unavailable(String),
}

/// R5-P2: the file each annotated record id names under `root`: indexed
/// videos (lossless path, R5-P3), carve outputs, recovered inodes and
/// joined outputs (later log entries win). Unreadable logs contribute
/// nothing (the id then reads "not verified").
pub fn evidence_target_paths(
    root: &std::path::Path,
) -> std::collections::BTreeMap<String, std::path::PathBuf> {
    // `db/videos.jsonl` is the index (not a chained lane); every other
    // source is a lane read through `audit::read_lane_rows` — R9 P3/I3: a
    // path re-hashed for a sign-off check comes only from the last entry
    // usable here, never from one this PC cannot authenticate (then the
    // id has no target here: "확인 불가").
    let rows = |rel: &str| -> Vec<serde_json::Value> {
        crate::audit::read_lane_rows(&root.join(rel))
            .map(|lane| lane.usable_values())
            .unwrap_or_default()
    };
    let text = |value: &serde_json::Value, key: &str| -> Option<String> {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let mut out = std::collections::BTreeMap::new();
    for row in index_rows(root).unwrap_or_default() {
        if let (Some(id), Some(path)) = (text(&row, "id"), crate::model::record_os_path(&row)) {
            out.insert(id, path);
        }
    }
    for row in rows("artifacts/carved/carve-log.jsonl") {
        if let (Some(id), Some(path)) = (text(&row, "id"), text(&row, "output_path")) {
            out.insert(id, crate::util::resolve_case_artifact_path(root, &path));
        }
    }
    for row in rows("evidence/logs/tsk-audit.jsonl") {
        if text(&row, "event").as_deref() != Some("recover-inode") {
            continue;
        }
        let offset = row
            .get("partition_offset")
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_else(|| "0".to_string());
        let inode = row
            .get("inode")
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string())
            })
            .or_else(|| text(&row, "output_path"))
            .unwrap_or_default();
        if let Some(path) = text(&row, "output_path") {
            out.insert(
                format!("inode:{offset}:{inode}"),
                crate::util::resolve_case_artifact_path(root, &path),
            );
        }
    }
    for row in rows("artifacts/joined/join-log.jsonl") {
        let row = crate::join::with_record_id(row);
        if let (Some(id), Some(path)) = (text(&row, "record_id"), text(&row, "output_path")) {
            out.insert(id, crate::util::resolve_case_artifact_path(root, &path));
        }
    }
    out
}

/// R5-P2: hashes again every target file a sign-off record bound with a
/// recorded SHA-256 (ids with `null` were never bound). `reuse` holds hashes
/// this run already computed (finalize's rehash of the indexed originals)
/// and is used instead of reading those files twice.
pub fn rehash_signed_targets(
    root: &std::path::Path,
    signoff_text: &str,
    reuse: &std::collections::BTreeMap<String, LiveHash>,
) -> std::collections::BTreeMap<String, LiveHash> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(signoff_text) else {
        return out;
    };
    let Some(evidence) = value.get("evidence").and_then(|v| v.as_object()) else {
        return out;
    };
    let targets = evidence_target_paths(root);
    for (id, sha) in evidence {
        if !sha.is_string() {
            continue;
        }
        if let Some(known) = reuse.get(id) {
            out.insert(id.clone(), known.clone());
            continue;
        }
        let live = match targets.get(id) {
            None => LiveHash::Unavailable("대상 파일 위치가 케이스 기록에 없음".to_string()),
            Some(path) => match crate::audit::digest_file(path) {
                Ok(sha) => LiveHash::Hashed(sha),
                Err(err) => LiveHash::Unavailable(err),
            },
        };
        out.insert(id.clone(), live);
    }
    out
}

/// Where a v4 sign-off is checked: the case itself, or a package whose
/// manifest lists the files it holds (a lane the package never contained
/// cannot be checked there and is reported as not verified, not changed).
pub enum SignoffScope<'a> {
    Case,
    Package(&'a dyn Fn(&str) -> bool),
}

/// The full currency check of a sign-off record (D13): what the report
/// banner and `verify-package` print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignoffCheck {
    pub currency: SignoffCurrency,
    /// Every change found since the sign-off (empty when current).
    pub changes: Vec<String>,
    /// What could not be checked here (evidence without a recorded hash,
    /// lanes a package does not hold).
    pub not_verified: Vec<String>,
    /// R5-P2: how many signed target files were hashed again for this
    /// check (0 = the bindings were compared with the case's records only).
    pub rehashed: usize,
    /// R6-I6: the sign-off audit entry for this record was authenticated by
    /// HMAC here. `false` with a current record = self-consistency only.
    pub authenticated: bool,
}

/// R7-I2: `Some(description)` when the sign-off lane under `root` holds at
/// least one entry (or cannot be read — then it is assumed to), `None`
/// when it is absent or empty.
///
/// R8-I9: only LIVE sign-off entries count — the sign-off entry of a
/// finalize attempt a trusted `signoff-abandoned` entry names (it failed
/// after recording) no longer caps an older record, and neither do other
/// events (rotation markers). A lane that does not verify counts as having
/// entries.
fn signoff_lane_has_entries(
    root: &std::path::Path,
    keys: &[crate::audit_key::AuditKey],
) -> Option<String> {
    // R9 P3: through the one lane reader. Every sign-off entry counts here,
    // trusted or not — this only ever ADDS a restriction (caps an older
    // record); only a trusted abandonment note lifts one.
    let lane = match crate::audit::read_lane_rows_with_keys(&root.join(SIGNOFF_LOG_REL_PATH), keys)
    {
        Ok(lane) if !lane.present => return None,
        Ok(lane) => lane,
        Err(err) => return Some(format!("레인을 읽지 못함: {err}")),
    };
    if let Some(err) = &lane.failure {
        return Some(format!("레인 검증 실패: {err}"));
    }
    let abandoned = abandoned_signoff_records(&lane.rows);
    let live = lane
        .rows
        .iter()
        .map(|row| &row.value)
        .filter(|value| value.get("event").and_then(|v| v.as_str()) == Some("signoff"))
        .filter(|value| {
            !value
                .get("signoff_record_sha256")
                .and_then(|v| v.as_str())
                .is_some_and(|sha| abandoned.contains(sha))
        })
        .count();
    (live > 0).then(|| format!("서명 {live}건"))
}

/// Checks a sign-off record against the case/package at `root`: for a
/// schema-v4 record the marks (`digests.content`), every annotated
/// record's target SHA-256 and every anchored lane; older records keep
/// their [`signoff_currency`] meaning (legacy binding).
pub fn check_signoff(
    root: &std::path::Path,
    signoff_text: &str,
    digests: &SignoffDigests,
    scope: SignoffScope<'_>,
) -> SignoffCheck {
    check_signoff_with(root, signoff_text, digests, scope, None)
}

/// [`check_signoff`] plus R5-P2's live re-hash of the signed target files
/// (`live`, from [`rehash_signed_targets`]): a re-hash that differs from
/// the signed SHA-256 is a change; a file that could not be read is "not
/// verified".
///
/// R5-P1: a v4 record is `Current` only when, besides every binding
/// matching, the sign-off audit lane holds a verified entry naming exactly
/// this record ([`signoff_audit_record`]); without one it is `Unbound`
/// (not verifiable), and a lane that contradicts it is a change.
pub fn check_signoff_with(
    root: &std::path::Path,
    signoff_text: &str,
    digests: &SignoffDigests,
    scope: SignoffScope<'_>,
    live: Option<&std::collections::BTreeMap<String, LiveHash>>,
) -> SignoffCheck {
    // R7-I2: a record below v4 is never recorded in the sign-off lane, so
    // its legacy binding is all that can be compared — but once the lane
    // holds entries, this case was finalized as v4 and an older-schema
    // record is exactly what a forger would produce by re-labelling a v4
    // record (`schema_version` 3 + `content_sha256` = the marks digest) to
    // skip the lane. Such a record is at most Unbound (NOT VERIFIED).
    //
    // R8-I2 (P0): and on a PC holding audit keys an older-schema record is
    // NEVER current — whatever the case files say: deleting the sign-off
    // lane (R7 only capped it while the lane held entries) must not turn a
    // re-labelled record into "current". It is at most amber (Unbound, "no
    // authentication"); a key-source error is treated as keys held (fail
    // closed).
    let legacy = || {
        let schema = serde_json::from_str::<serde_json::Value>(signoff_text)
            .ok()
            .and_then(|value| value.get("schema_version").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let mut check = SignoffCheck {
            currency: signoff_currency(signoff_text, digests),
            changes: Vec::new(),
            not_verified: Vec::new(),
            rehashed: 0,
            authenticated: false,
        };
        if check.currency == SignoffCurrency::Changed {
            return check;
        }
        let keys = crate::audit_key::verification_keys();
        let keys_held = keys
            .as_ref()
            .map_or(true, |keys| crate::audit::trust_policy(keys).keys_held());
        if keys_held {
            check.currency = SignoffCurrency::Unbound;
            check.not_verified.push(format!(
                "이전 형식(v{schema}) 서명 기록은 감사 키로 인증되지 않습니다(레거시 바인딩, 인증 없음) — 이 PC에는 감사 키가 있어 인증되지 않은 서명을 현재 내용에 대한 서명으로 표시하지 않습니다. 감사 키가 있는 PC에서 다시 최종화하십시오"
            ));
        } else if let Some(entries) =
            signoff_lane_has_entries(root, keys.as_deref().unwrap_or_default())
        {
            check.currency = SignoffCurrency::Unbound;
            check.not_verified.push(format!(
                "이전 형식(v{schema}) 서명 기록이지만 서명 감사 레인(signoff-log)에 항목이 있습니다({entries}) — 이 케이스는 v4로 최종화된 적이 있어 이전 형식 기록은 감사 체인으로 확인할 수 없습니다(v4 기록을 이전 형식으로 바꾼 것일 수 있음). 다시 최종화하십시오"
            ));
        }
        check
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(signoff_text) else {
        return legacy();
    };
    match value.get("schema_version").and_then(|v| v.as_u64()) {
        Some(SIGNOFF_SCHEMA_V4) => {}
        // R7-I2: a schema this build never writes is rejected — never read
        // through the v4 marks-only shortcut of `signoff_currency`.
        Some(schema) if schema > SIGNOFF_SCHEMA_V4 => {
            return SignoffCheck {
                currency: SignoffCurrency::Unbound,
                changes: Vec::new(),
                not_verified: vec![format!(
                    "지원하지 않는 서명 형식(schema_version {schema}) — 이 버전이 쓰는 형식은 v{SIGNOFF_SCHEMA_V4}까지라 서명 기록을 확인할 수 없습니다 (기록이 바뀌었거나 더 새 버전에서 만든 기록)"
                )],
                rehashed: 0,
                authenticated: false,
            };
        }
        _ => return legacy(),
    }
    let mut changes = Vec::new();
    let mut not_verified = Vec::new();
    let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let (Some(annotations), Some(content)) = (text("annotations_sha256"), text("content_sha256"))
    else {
        return SignoffCheck {
            currency: SignoffCurrency::Changed,
            changes: vec![
                "v4 서명 기록에 annotations_sha256/content_sha256이 없습니다 — 기록이 손상됨"
                    .to_string(),
            ],
            not_verified,
            rehashed: 0,
            authenticated: false,
        };
    };
    // Recorded bindings, parsed back from the record.
    let mut recorded = SignoffBindings::default();
    let mut malformed = false;
    match value.get("evidence").and_then(|v| v.as_object()) {
        Some(map) => {
            for (id, sha) in map {
                match sha {
                    serde_json::Value::Null => {
                        recorded.evidence.insert(id.clone(), None);
                    }
                    serde_json::Value::String(sha) => {
                        recorded.evidence.insert(id.clone(), Some(sha.clone()));
                    }
                    _ => malformed = true,
                }
            }
        }
        None => malformed = true,
    }
    match value.get("lanes").and_then(|v| v.as_object()) {
        Some(map) => {
            for (rel, head) in map {
                match (
                    head.get("entries").and_then(|v| v.as_u64()),
                    head.get("last_entry_sha256").and_then(|v| v.as_str()),
                ) {
                    (Some(entries), Some(last)) => {
                        recorded.lanes.insert(
                            rel.clone(),
                            crate::audit::LaneHead {
                                entries,
                                last_entry_sha256: last.to_string(),
                            },
                        );
                    }
                    _ => malformed = true,
                }
            }
        }
        None => malformed = true,
    }
    if malformed
        || signoff_content_sha256(&signoff_v4_content_json(&annotations, &recorded)) != content
    {
        changes.push(
            "서명 기록의 결합 내용(content_sha256)이 기록된 증거 해시·감사 앵커와 맞지 않습니다 — 서명 기록이 변경됨"
                .to_string(),
        );
    }
    if annotations != digests.content {
        changes.push("sign-off 이후 판독 마크·메모·IN-OUT 구간·태그가 변경됨".to_string());
    }
    // R5-P2: these compare the hash the CASE RECORDS for each target now
    // with the one signed — not the file's bytes (see `live` below).
    // R5-P5: a record without a hash, or a log that cannot be read, is
    // "not verified" — never "changed".
    match evidence_hashes(root) {
        Ok(current) => {
            for (id, sha) in &recorded.evidence {
                match (sha, current.get(id)) {
                    (Some(sha), Some(now)) if now.eq_ignore_ascii_case(sha) => {}
                    (Some(sha), Some(now)) => changes.push(format!(
                        "{id}: 케이스 기록의 대상 파일 해시가 서명 후 바뀜 (서명 시 {sha}, 현재 기록 {now})"
                    )),
                    (Some(sha), None) => not_verified.push(format!(
                        "{id}: 서명 시 대상 파일 SHA-256 {sha}이 기록됐으나 현재 케이스 기록에서 해시를 찾을 수 없어 확인 불가"
                    )),
                    (None, _) => not_verified.push(format!(
                        "{id}: 서명 시 대상 파일 해시가 없어 파일 교체 여부를 확인할 수 없음"
                    )),
                }
            }
        }
        Err(err) => not_verified.push(format!(
            "케이스 기록의 대상 파일 해시를 읽지 못해 확인 불가: {err}"
        )),
    }
    let mut rehashed = 0usize;
    if let Some(live) = live {
        for (id, sha) in &recorded.evidence {
            let Some(sha) = sha else {
                continue;
            };
            match live.get(id) {
                Some(LiveHash::Hashed(now)) if now.eq_ignore_ascii_case(sha) => rehashed += 1,
                Some(LiveHash::Hashed(now)) => {
                    rehashed += 1;
                    changes.push(format!(
                        "{id}: 대상 파일을 다시 해시한 값이 서명 시 해시와 다름 (서명 시 {sha}, 재해시 {now}) — 서명 후 파일이 바뀜"
                    ));
                }
                Some(LiveHash::Unavailable(why)) => not_verified.push(format!(
                    "{id}: 대상 파일을 다시 해시하지 못해 파일 자체는 확인 불가 — {why}"
                )),
                None => not_verified.push(format!(
                    "{id}: 대상 파일을 다시 해시하지 않음 — 파일 자체는 확인 불가"
                )),
            }
        }
    }
    for (rel, head) in &recorded.lanes {
        if let SignoffScope::Package(listed) = &scope
            && !listed(rel)
        {
            not_verified.push(format!(
                "{rel}: 패키지에 포함되지 않은 감사 레인 — 앵커(항목 {}개) 확인 불가",
                head.entries
            ));
            continue;
        }
        match crate::audit::check_lane_anchor(&root.join(rel), head) {
            crate::audit::AnchorStatus::Intact => {}
            crate::audit::AnchorStatus::Missing => changes.push(format!(
                "{rel}: 서명 시 항목 {}개였던 감사 레인이 없어짐",
                head.entries
            )),
            crate::audit::AnchorStatus::Truncated { now } => changes.push(format!(
                "{rel}: 감사 레인 끝부분이 삭제됨 (서명 시 항목 {}개, 현재 {now}개)",
                head.entries
            )),
            crate::audit::AnchorStatus::Rewritten => changes.push(format!(
                "{rel}: 서명 시 {}번째 항목이 다른 항목으로 바뀜 — 레인이 재작성됨",
                head.entries
            )),
            // R5-P5: unreadable (locked, I/O error) is not verified.
            crate::audit::AnchorStatus::Unreadable(err) => not_verified.push(format!(
                "{rel}: 감사 레인을 읽지 못해 앵커 확인 불가 — {err}"
            )),
            // R10-I7/I8: the anchored entry is there, but the lane up to it
            // is not usable here — not "changed", not current either.
            crate::audit::AnchorStatus::NotAuthenticated(why) => not_verified.push(format!(
                "{rel}: 앵커 지점까지 이 PC에서 인증되지 않은 항목이 있어 앵커 확인 불가 — {why}"
            )),
        }
    }
    // R5-P1: the record must be the one the sign-off lane recorded.
    let lane_listed = match &scope {
        SignoffScope::Case => true,
        SignoffScope::Package(listed) => listed(SIGNOFF_LOG_REL_PATH),
    };
    let mut unrecorded = false;
    let mut authenticated = false;
    match signoff_audit_record(root, signoff_text, lane_listed) {
        SignoffAuditRecord::Verified {
            authenticated: auth,
            ..
        } => authenticated = auth,
        SignoffAuditRecord::NotVerifiable(why) => {
            unrecorded = true;
            not_verified.push(format!("서명 감사 기록 확인 불가 — 서명 확인 불가: {why}"));
        }
        SignoffAuditRecord::Contradicted(why) => changes.push(why),
    }
    SignoffCheck {
        currency: if !changes.is_empty() {
            SignoffCurrency::Changed
        } else if unrecorded {
            SignoffCurrency::Unbound
        } else if !not_verified.is_empty() {
            // R6-I1: one item that could not be checked is enough to stop
            // the record reading as current.
            SignoffCurrency::PartiallyVerified
        } else {
            SignoffCurrency::Current
        },
        changes,
        not_verified,
        rehashed,
        authenticated,
    }
}

/// The banner for a checked sign-off: [`signoff_banner_html`] plus, for a
/// v4 record, what was compared, every change found, what could not be
/// checked, and whether the record is in the sign-off audit lane.
pub fn signoff_banner_checked_html(
    examiner: &str,
    signed_unix: i64,
    check: &SignoffCheck,
    v4: bool,
    audit_note: &str,
) -> String {
    let mut banner = if (v4 || !check.not_verified.is_empty())
        && check.currency == SignoffCurrency::Unbound
    {
        // R5-P1: a v4 record the sign-off lane does not confirm (R7-I2: or an
        // older / unsupported-schema record capped at Unbound, with reasons).
        signoff_banner_html(examiner, signed_unix, check.currency).replace(
            " — 이 기록에는 대상 내용의 해시가 없어 이후 판독 마크 변경 여부를 확인할 수 없습니다.",
            " — <strong>서명 확인 불가: 감사 체인(signoff-log)으로 이 서명 기록을 확인할 수 없어(아래 사유), 최종화 때 기록된 최신 서명인지 보증할 수 없습니다.</strong>",
        )
    } else if check.currency == SignoffCurrency::PartiallyVerified {
        // R6-I1: say how many items could not be checked.
        signoff_banner_html(examiner, signed_unix, check.currency).replace(
            "<strong>일부 항목 확인 불가 — ",
            &format!("<strong>{}개 항목 확인 불가 — ", check.not_verified.len()),
        )
    } else if check.currency == SignoffCurrency::Current && !check.authenticated {
        // R6-I6: a keyless match must not read like an authentication.
        // R8 P1: nor may a legacy (v<4) record's — only possible on a PC
        // without audit keys now (I2), and never authenticated.
        let base = signoff_banner_html(examiner, signed_unix, check.currency);
        match base.rfind("</div>") {
            Some(close) => format!(
                "{} <strong>(자기 일관성만 확인 — 인증 아님)</strong>{}",
                &base[..close],
                &base[close..]
            ),
            None => base,
        }
    } else {
        signoff_banner_html(examiner, signed_unix, check.currency)
    };
    let mut details = Vec::new();
    if v4
        && matches!(
            check.currency,
            SignoffCurrency::Current | SignoffCurrency::PartiallyVerified
        )
    {
        // R5-P2: say exactly what was compared.
        details.push(format!(
            "서명 형식 v4: {}판독 내용, 케이스 기록의 대상 파일 해시, 감사 레인 앵커(항목 수·마지막 해시)가 서명 시와 일치함 — 서명 후 로그 끝부분 삭제 없음. {}",
            if check.not_verified.is_empty() {
                ""
            } else {
                "확인한 범위에서 "
            },
            if check.rehashed > 0 {
                format!("대상 파일 {}개는 이번에 다시 해시해 서명 시 값과 일치함을 확인함", check.rehashed)
            } else {
                "대상 파일 자체는 이번에 다시 해시하지 않음(케이스 기록의 해시와 비교)".to_string()
            }
        ));
        if !check.not_verified.is_empty() {
            details.push(format!(
                "다만 아래 {}개 항목은 확인 불가 — 서명 일치를 단정할 수 없음",
                check.not_verified.len()
            ));
        }
        if !check.authenticated {
            details.push(SELF_CONSISTENT_ONLY.to_string());
        }
    } else if check.currency == SignoffCurrency::Current {
        // R8 P1: a legacy record's match is self-consistency only.
        details.push(SELF_CONSISTENT_ONLY.to_string());
    }
    details.extend(check.changes.iter().map(|change| format!("변경: {change}")));
    details.extend(
        check
            .not_verified
            .iter()
            .map(|item| format!("확인 불가: {item}")),
    );
    details.push(audit_note.to_string());
    let list = details
        .iter()
        .map(|item| crate::util::html_escape(item))
        .collect::<Vec<_>>()
        .join("<br>");
    if let Some(close) = banner.rfind("</div>") {
        banner.insert_str(
            close,
            &format!("<br><span style=\"font-size:12px\">{list}</span>"),
        );
    }
    banner
}

/// [`inject_signoff_banner`] with a full [`check_signoff`] result.
pub fn inject_signoff_banner_checked(
    html: String,
    signoff: &Signoff,
    check: &SignoffCheck,
    audit_note: &str,
) -> String {
    let banner = format!(
        "{}{}",
        signoff_source_marker(&signoff.text),
        signoff_banner_checked_html(
            &signoff.examiner,
            signoff.signed_unix,
            check,
            signoff.schema_version == SIGNOFF_SCHEMA_V4,
            audit_note
        )
    );
    match html.find("<body>") {
        Some(index) => format!(
            "{}{}{}",
            &html[..index + "<body>".len()],
            banner,
            &html[index + "<body>".len()..]
        ),
        None => format!("{banner}{html}"),
    }
}

/// Inserts the sign-off banner right after `<body>`, with the marker of the
/// record (`signoff_text`) it was rendered from.
pub fn inject_signoff_banner(
    html: String,
    examiner: &str,
    signed_unix: i64,
    currency: SignoffCurrency,
    signoff_text: &str,
) -> String {
    let banner = format!(
        "{}{}",
        signoff_source_marker(signoff_text),
        signoff_banner_html(examiner, signed_unix, currency)
    );
    match html.find("<body>") {
        Some(index) => format!(
            "{}{}{}",
            &html[..index + "<body>".len()],
            banner,
            &html[index + "<body>".len()..]
        ),
        None => format!("{banner}{html}"),
    }
}

/// The audio-level map as an object literal; anything that is not a JSON
/// object (empty input, garbage) degrades to `{}` so the embedded script
/// block can never be broken by it.
pub(crate) fn audio_levels_object(json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(json.trim()) {
        Ok(value) if value.is_object() => value.to_string(),
        _ => "{}".to_string(),
    }
}

// R14 I3 / R15 P22 (track I): the `hash-verify-head` writer/reader, apart
// from the finalize pairing above.
#[path = "report_hash_verify_head.rs"]
mod hash_verify_head;
pub use hash_verify_head::{
    ANOMALY_LOG_REL_PATH, HASH_VERIFY_HEAD_EVENT, HASH_VERIFY_HEAD_TRUNCATED,
    HASH_VERIFY_MARKER_HEAD_FIELD, HEAD_LANE_CHANGED_NOTE, HEAD_MARKER_UNUSABLE_NOTE,
    HEAD_MIXED_VERSION_NOTE, HEAD_OTHER_EVENT_NOTE, HEAD_OTHER_PC_NOTE, HEAD_RESCAN_NOTE,
    HEAD_SCAN_MISMATCH_NOTE, HEAD_SCHEMA_VERSION, HashVerifyHeadCheck, HeadLane,
    KNOWN_SIGNOFF_EVENTS, anomaly_head_check, hash_verify_head_check, lane_head_check,
    read_anomaly_head_check, record_scan_head,
};

// R18 I2 / I5 / I6 (track I): the anomaly lane's own withholding verdict,
// shared by the report, `/api/records-meta` and the review bundle.
#[path = "report_anomaly_withheld.rs"]
mod anomaly_withheld;
pub use anomaly_withheld::{
    ANOMALY_REPAIRED_AFTER_REASON, ANOMALY_UNUSABLE_AFTER_REASON, ANOMALY_UNUSABLE_IN_RUN_NOTE,
    AnomalyWithheld, anomaly_lane_read_withheld_json, anomaly_lane_withheld,
    anomaly_lane_withheld_json, anomaly_unreadable_withheld_json, bundle_log_chains_json,
};

/// A JSON array literal; anything else (empty input, garbage) is `[]`.
fn json_array_or_empty(json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(json.trim()) {
        Ok(value) if value.is_array() => value.to_string(),
        _ => "[]".to_string(),
    }
}

fn jsonl_to_array(jsonl: &str) -> String {
    // A torn final line (the documented crash survivability mode) is not
    // valid JSON; feeding it into the embedded array literal would be a
    // syntax error that blanks the whole script block and the report
    // would render as an empty-but-valid-looking page. Skip broken lines
    // instead — the audit verify command is the authoritative integrity
    // surface for detecting them.
    let items: Vec<&str> = jsonl
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
        .collect();
    format!("[{}]", items.join(","))
}

#[cfg(test)]
mod tests {
    use super::render_case_report;

    /// Owner decision (docs/ui-rebuild-contract.md 10.1-3): the court report
    /// carries a one-line textual legend of the viewer's mark shapes/colours,
    /// placed right after the header note that mentions the marks.
    #[test]
    fn report_carries_mark_legend_line_after_marks_note() {
        let html = render(&[
            ("manifest", "{}"),
            ("index", "[]"),
            ("scan_runs", "[]"),
            ("marks", "[]"),
        ]);
        let legend =
            "판독 마크 범례: 판독완료 = 회색 체크 · 중요 = 호박색 깃발 · 검증대기 = 보라 물음표";
        assert_eq!(html.matches(legend).count(), 1);
        assert!(
            html.find("import-marks").unwrap() < html.find(legend).unwrap(),
            "legend sits next to the marks note"
        );
    }

    #[test]
    fn signoff_banner_escapes_and_is_injected_after_body() {
        let html = super::inject_signoff_banner(
            "<html><body><p>x</p></body></html>".to_string(),
            "홍길동 <b>",
            1700000000,
            super::SignoffCurrency::Current,
            "{\"examiner\":\"홍길동 <b>\"}",
        );
        assert!(html.contains("검토자: <strong>홍길동 &lt;b&gt;</strong>"));
        // I6: the signed time is spelled out in UTC, not as a bare number.
        assert!(
            html.contains("기록 시각 2023-11-14 22:13:20 UTC, unix 1700000000"),
            "{html}"
        );
        // I10: the report names the exact record it was rendered from.
        assert_eq!(
            super::report_signoff_sha256(&html),
            Some(crate::sha256::digest_bytes("{\"examiner\":\"홍길동 <b>\"}".as_bytes()).as_str())
        );
        assert_eq!(
            super::report_signoff_sha256("<html><body></body></html>"),
            None
        );
        assert!(html.contains("신원 인증은 아닙니다"));
        assert!(html.find("검토자").unwrap() < html.find("<p>x</p>").unwrap());
        assert_eq!(
            super::parse_signoff(r#"{"examiner":" A ","signed_unix":5}"#),
            Some(("A".to_string(), 5))
        );
        assert_eq!(super::parse_signoff(r#"{"examiner":"  "}"#), None);
    }

    /// SHOULD-FIX 6: a sign-off given for other marks never reads as current.
    #[test]
    fn signoff_currency_detects_marks_changed_after_signoff() {
        use super::{SignoffCurrency, SignoffDigests, signoff_banner_html, signoff_currency};
        let digests = |content: &str, legacy: &str| SignoffDigests {
            content: super::signoff_content_sha256(content),
            legacy: super::signoff_content_sha256(legacy),
        };
        let now = digests(r#"{"marks":[]}"#, "[]");
        let record = format!(
            r#"{{"schema_version":3,"examiner":"A","signed_unix":5,"content_sha256":"{}"}}"#,
            now.content
        );
        assert_eq!(signoff_currency(&record, &now), SignoffCurrency::Current);
        // A range/tag edit changes only the v3 digest: still detected.
        let range_moved = digests(r#"{"marks":[],"tags":{"v":["x"]}}"#, "[]");
        assert_eq!(
            signoff_currency(&record, &range_moved),
            SignoffCurrency::Changed
        );
        assert_eq!(
            signoff_currency(r#"{"examiner":"A","signed_unix":5}"#, &now),
            SignoffCurrency::Unbound
        );
        // A v2 record bound the legacy marks JSON: a match there is NOT
        // "unchanged" (ranges/tags were never covered), a mismatch is.
        let legacy = format!(
            r#"{{"schema_version":2,"examiner":"A","signed_unix":5,"content_sha256":"{}"}}"#,
            now.legacy
        );
        assert_eq!(
            signoff_currency(&legacy, &range_moved),
            SignoffCurrency::LegacyBinding
        );
        let status_changed = digests(r#"{"marks":[1]}"#, r#"[{"id":"v"}]"#);
        assert_eq!(
            signoff_currency(&legacy, &status_changed),
            SignoffCurrency::Changed
        );
        let banner = signoff_banner_html("A", 5, SignoffCurrency::LegacyBinding);
        assert!(banner.contains("확인할 수 없습니다"), "{banner}");
        assert!(!banner.contains("변경되지 않았습니다"), "{banner}");
        let banner = signoff_banner_html("A", 5, SignoffCurrency::Changed);
        assert!(banner.contains("변경되었습니다"), "{banner}");
        assert!(banner.contains("현재 내용에 대한 sign-off가 아닙니다"));
        assert!(!signoff_banner_html("A", 5, SignoffCurrency::Current).contains("주의"));
    }

    fn render(inputs: &[(&str, &str)]) -> String {
        let lookup = |key: &str| -> String {
            inputs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
                .unwrap_or_default()
        };
        let manifest = lookup("manifest");
        let index = lookup("index");
        let export = lookup("export");
        let proxy = lookup("proxy");
        let thumbnail = lookup("thumbnail");
        let carve = lookup("carve");
        let filesystem = lookup("filesystem");
        let validation = lookup("validation");
        let anomaly = lookup("anomaly");
        let batch = lookup("batch");
        let join = lookup("join");
        let audio_levels = lookup("audio_levels");
        let scan_runs = lookup("scan_runs");
        let marks = lookup("marks");
        let review_warnings = lookup("review_warnings");
        let read_errors = lookup("read_errors");
        let telemetry = lookup("telemetry");
        render_case_report(&super::ReportInputs {
            manifest_json: &manifest,
            index_json: &index,
            export_log_jsonl: &export,
            proxy_log_jsonl: &proxy,
            thumbnail_log_jsonl: &thumbnail,
            carve_log_jsonl: &carve,
            filesystem_log_jsonl: &filesystem,
            validation_log_jsonl: &validation,
            anomaly_log_jsonl: &anomaly,
            batch_log_jsonl: &batch,
            join_log_jsonl: &join,
            audio_levels_json: &audio_levels,
            scan_runs_json: &scan_runs,
            marks_json: &marks,
            redaction_applied: inputs
                .iter()
                .any(|(name, value)| *name == "redacted" && *value == "1"),
            review_warnings_json: &review_warnings,
            read_errors_json: &read_errors,
            telemetry_json: &telemetry,
        })
    }

    fn rendered_techniques(index: &str, validation: &str, carve: &str, filesystem: &str) -> String {
        rendered_techniques_with(index, validation, carve, filesystem, "[]")
    }

    fn rendered_techniques_with(
        index: &str,
        validation: &str,
        carve: &str,
        filesystem: &str,
        annotations: &str,
    ) -> String {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let html = render(&[
            ("manifest", "{}"),
            ("index", index),
            ("validation", validation),
            ("carve", carve),
            ("filesystem", filesystem),
            ("scan_runs", "[]"),
            ("marks", annotations),
        ]);
        let script = html
            .split_once("<script>")
            .unwrap()
            .1
            .split_once("</script>")
            .unwrap()
            .0;
        let mut child = Command::new("node")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("node is required for report rendering tests");
        let mut stdin = child.stdin.take().unwrap();
        write!(stdin, "const elements = new Map(); const document = {{getElementById(id) {{ if (!elements.has(id)) elements.set(id, {{}}); return elements.get(id); }}}};\n{script}\nprocess.stdout.write(elements.get('techniques').innerHTML);").unwrap();
        drop(stdin);
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn old_validation_for_different_bytes_is_not_confirmed() {
        let index = serde_json::json!({"videos": [{"id": "vid_1", "source_path": "/evidence/a.mp4", "sha256": "b".repeat(64)}]}).to_string();
        let validation = serde_json::json!({"selector": "vid_1", "target_path": "/evidence/a.mp4", "target_sha256": "a".repeat(64), "validation_status": "ffprobe-video-stream-confirmed"}).to_string();
        let rows = rendered_techniques(&index, &validation, "", "");
        assert!(!rows.contains("ffprobe-video-stream-confirmed"));
        assert!(rows.contains("validation-unbound"));
    }

    #[test]
    fn validation_join_uses_latest_eligible_event_and_exact_paths() {
        let sha = "a".repeat(64);
        let index = serde_json::json!({"videos": [{"id": "vid_1", "source_path": "/evidence/A.mp4", "sha256": sha}]}).to_string();
        let event = |path: &str, hash: Option<&str>, timestamp: u64, status: &str| {
            serde_json::json!({"selector": "vid_1", "target_path": path, "target_sha256": hash, "validated_unix": timestamp, "validation_status": status}).to_string()
        };
        let log = [
            event("/evidence/A.mp4", Some(&sha), 20, "validation-failed"),
            event(
                "/evidence/A.mp4",
                Some(&sha),
                10,
                "ffprobe-video-stream-confirmed",
            ),
            event(
                "/evidence/A.mp4",
                Some(&"b".repeat(64)),
                30,
                "ffprobe-video-stream-confirmed",
            ),
            event(
                "/evidence/A.mp4",
                None,
                40,
                "ffprobe-video-stream-confirmed",
            ),
            event(
                "/evidence/a.mp4",
                Some(&sha),
                50,
                "ffprobe-video-stream-confirmed",
            ),
        ]
        .join("\n");
        let rows = rendered_techniques(&index, &log, "", "");
        assert!(rows.contains("validation-failed"));
        assert!(!rows.contains("ffprobe-video-stream-confirmed"));
        let rows = rendered_techniques(
            &index,
            &event(
                "/evidence/a.mp4",
                Some(&sha),
                50,
                "ffprobe-video-stream-confirmed",
            ),
            "",
            "",
        );
        assert!(!rows.contains("ffprobe-video-stream-confirmed"));
        for hash in [None, Some(""), Some("not-a-hash")] {
            let rows = rendered_techniques(
                &index,
                &event("/evidence/A.mp4", hash, 1, "ffprobe-video-stream-confirmed"),
                "",
                "",
            );
            assert!(rows.contains("validation-unbound"));
        }
        let unhashed_index = r#"{"videos":[{"id":"vid_1","source_path":"/evidence/A.mp4"}]}"#;
        let rows = rendered_techniques(
            unhashed_index,
            &event(
                "/evidence/A.mp4",
                Some(&sha),
                1,
                "ffprobe-video-stream-confirmed",
            ),
            "",
            "",
        );
        assert!(rows.contains("validation-unbound"));
        let tie = [
            event(
                "/evidence/A.mp4",
                Some(&sha.to_uppercase()),
                20,
                "ffprobe-video-stream-confirmed",
            ),
            event("/evidence/A.mp4", Some(&sha), 20, "validation-failed"),
        ]
        .join("\n");
        assert!(rendered_techniques(&index, &tie, "", "").contains("validation-failed"));
    }

    #[test]
    fn artifact_validation_requires_recorded_matching_hash() {
        for hash in [
            serde_json::Value::Null,
            serde_json::json!("b".repeat(64)),
            serde_json::json!("a".repeat(64)),
        ] {
            let carve =
                serde_json::json!({"id":"carve_1", "output_path":"/case/carve.mp4", "sha256":hash})
                    .to_string();
            let recovered = serde_json::json!({"event":"recover-inode", "inode":"1", "output_path":"/case/recovered.mp4", "sha256":hash}).to_string();
            let log = ["/case/carve.mp4", "/case/recovered.mp4"].map(|path| serde_json::json!({"target_path":path, "target_sha256":"a".repeat(64), "validation_status":"ffprobe-video-stream-confirmed"}).to_string()).join("\n");
            let rows = rendered_techniques("{}", &log, &carve, &recovered);
            assert_eq!(
                rows.contains("ffprobe-video-stream-confirmed"),
                hash == serde_json::json!("a".repeat(64))
            );
        }
    }

    #[test]
    fn empty_report_renders_section_markers() {
        let html = render(&[]);
        assert!(html.contains("발견 및 분석 기법"));
        assert!(html.contains("처리 체인"));
        assert!(html.contains("이상 징후 후보"));
        assert!(html.contains("정리할 증거가 없습니다."));
        assert!(html.contains("처리 이력이 없습니다."));
    }

    #[test]
    fn populated_report_lists_technique_and_chain_rows() {
        let html = render(&[
            ("manifest", r#"{"case_id":"FT-1","title":"T"}"#),
            (
                "index",
                r#"{"videos":[{"id":"vid_000001","source_path":"C:/src/a.mp4","relative_path":"a.mp4","size_bytes":10,"sha256":"ab","hash_status":"hashed","source_profile":{"lane":"generic_media","vendor":"Generic media","parser":"generic_media","confidence":"medium","recommended_action":"r","evidence":[]},"video_codec":"h264","width":640,"height":360,"duration_seconds":5.0,"modified_unix":100}]}"#,
            ),
            (
                "validation",
                r#"{"event":"validate-artifact","validated_unix":2,"selector":"vid_000001","target_path":"C:/src/a.mp4","validation_status":"ffprobe-video-stream-confirmed","validation_note":"ok","entry_sha256":"e1"}"#,
            ),
            (
                "scan_runs",
                r#"[{"scanned_unix":1,"video_count":1,"source_path":"C:/src","warnings":[]}]"#,
            ),
            (
                "marks",
                r#"[{"id":"vid_000001","status":"important","marked_unix":9}]"#,
            ),
        ]);
        assert!(html.contains("vid_000001"));
        assert!(html.contains("ffprobe-video-stream-confirmed"));
        assert!(html.contains("논리 파일 스캔"));
        assert!(html.contains("중요"));
        assert!(html.contains("scan-folder (스캔·색인)"));
    }

    fn three_videos() -> String {
        serde_json::json!({"videos": [
            {"id": "vid_1", "source_path": "/evidence/a.mp4"},
            {"id": "vid_2", "source_path": "/evidence/b.mp4"},
            {"id": "vid_3", "source_path": "/evidence/c.mp4"},
        ]})
        .to_string()
    }

    /// The row of `id` in the rendered techniques table.
    fn row_of<'a>(rows: &'a str, id: &str) -> &'a str {
        let start = rows
            .find(&format!("<td>{id}</td>"))
            .unwrap_or_else(|| panic!("no row for {id}: {rows}"));
        let end = rows[start..]
            .find("</tr>")
            .map_or(rows.len(), |i| start + i);
        &rows[start..end]
    }

    /// The last ("판독") cell of a rendered row.
    fn mark_cell(row: &str) -> &str {
        let start = row.rfind("<td>").unwrap() + "<td>".len();
        let end = start + row[start..].find("</td>").unwrap();
        &row[start..end]
    }

    /// I3: the "판독" column shows status, note, examiner, IN-OUT range and
    /// tags, all escaped (a `<` in a tag or note never becomes markup).
    #[test]
    fn report_mark_column_shows_range_and_tags() {
        let annotations = serde_json::json!({
            "marks": [
                {"id": "vid_1", "status": "important", "marked_unix": 9, "examiner": "kim<i>",
                 "note": "첫 줄\n<b>둘째</b>", "range": {"in": 1.5, "out": 4}},
                {"id": "vid_2", "status": "reviewed", "marked_unix": 9, "examiner": null,
                 "note": null, "range": {"in": 2, "out": null}},
            ],
            "tags": {"vid_1": ["사고", "<script>x</script>"]},
        })
        .to_string();
        let rows = rendered_techniques_with(&three_videos(), "", "", "", &annotations);
        let first = row_of(&rows, "vid_1");
        assert!(first.contains("중요"), "{first}");
        assert!(first.contains("구간: 1.5s ~ 4.0s"), "{first}");
        assert!(
            first.contains("태그: 사고, &lt;script&gt;x&lt;/script&gt;"),
            "{first}"
        );
        assert!(
            first.contains("첫 줄<br>&lt;b&gt;둘째&lt;/b&gt;"),
            "{first}"
        );
        assert!(first.contains("검토자: kim&lt;i&gt;"), "{first}");
        assert!(!rows.contains("<script>"), "{rows}");
        assert!(!rows.contains("<b>"), "{rows}");
        // Only IN set: the open side reads as a dash.
        let second = row_of(&rows, "vid_2");
        assert!(second.contains("구간: 2.0s ~ —"), "{second}");
        assert!(!second.contains("태그:"), "{second}");
        assert_eq!(mark_cell(row_of(&rows, "vid_3")), "-");
        let html = render(&[("marks", annotations.as_str())]);
        assert!(html.contains("판독 (상태·메모·구간·태그)"));
    }

    /// I3: a record with tags but no mark shows its tags instead of "-".
    #[test]
    fn report_tag_only_record_shows_tags() {
        let annotations =
            serde_json::json!({"marks": [], "tags": {"vid_3": ["보류", "a&b"]}}).to_string();
        let rows = rendered_techniques_with(&three_videos(), "", "", "", &annotations);
        let third = row_of(&rows, "vid_3");
        assert!(third.contains("태그: 보류, a&amp;b"), "{third}");
        assert_ne!(mark_cell(third), "-", "{third}");
        // A pre-I3 bare marks array still renders (no ranges/tags then).
        let legacy = r#"[{"id":"vid_2","status":"important","marked_unix":9}]"#;
        let rows = rendered_techniques_with(&three_videos(), "", "", "", legacy);
        assert!(row_of(&rows, "vid_2").contains("중요"));
        assert!(!rows.contains("태그:"));
    }

    #[test]
    fn current_banner_points_to_the_mark_column_and_legacy_wording_is_unchanged() {
        use super::{SignoffCurrency, signoff_banner_html};
        let current = signoff_banner_html("A", 5, SignoffCurrency::Current);
        assert!(
            current.contains(
                "(상태·메모·IN-OUT 구간·태그 모두 일치). (아래 '판독' 열에 구간·태그 표시)"
            ),
            "{current}"
        );
        assert!(signoff_banner_html("A", 5, SignoffCurrency::LegacyBinding).contains(
            " — 이 기록은 이전 형식이라 판독 마크의 상태·메모만 대상으로 합니다(이후 변경 없음). IN-OUT 구간·태그가 이후 바뀌었는지는 확인할 수 없습니다 — 다시 최종화하면 함께 기록됩니다.</div>"
        ));
        assert!(signoff_banner_html("A", 5, SignoffCurrency::Unbound).contains(
            " — 이 기록에는 대상 내용의 해시가 없어 이후 판독 마크 변경 여부를 확인할 수 없습니다.</div>"
        ));
    }

    /// Runs the report script under node with a stub `document` and returns
    /// every element it touched as `{id: {innerHTML, textContent, hidden}}`.
    fn rendered_elements(inputs: &[(&str, &str)]) -> serde_json::Value {
        let mut full: Vec<(&str, &str)> = inputs.to_vec();
        for (key, default) in [
            ("manifest", "{}"),
            ("index", "{}"),
            ("scan_runs", "[]"),
            ("marks", "[]"),
        ] {
            if !full.iter().any(|(name, _)| *name == key) {
                full.push((key, default));
            }
        }
        run_report_script(&render(&full))
    }

    fn run_report_script(html: &str) -> serde_json::Value {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let script = html
            .split_once("<script>")
            .unwrap()
            .1
            .split_once("</script>")
            .unwrap()
            .0;
        let mut child = Command::new("node")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("node is required for report rendering tests");
        let mut stdin = child.stdin.take().unwrap();
        write!(stdin, "const elements = new Map(); const document = {{getElementById(id) {{ if (!elements.has(id)) elements.set(id, {{}}); return elements.get(id); }}}};\n{script}\nprocess.stdout.write(JSON.stringify(Object.fromEntries(elements)));").unwrap();
        drop(stdin);
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// A join-log entry per plan §1.5 (plus the chain fields the audit adds).
    fn join_entry() -> String {
        serde_json::json!({
            "schema_version": 1, "event": "join-video", "joined_unix": 1_788_000_000,
            "output_path": "artifacts/joined/join_F_20260910-120000_n10.mp4",
            "output_sha256": "a".repeat(64), "output_size_bytes": 1234,
            "manifest_path": "artifacts/joined/join_F_20260910-120000_n10.manifest.json",
            "manifest_sha256": "b".repeat(64), "plan_id": "c".repeat(64),
            "segment_count": 10, "source_ids": ["vid_1", "vid_2"],
            "source_sha256": ["d", "e"], "hash_mismatches": 1,
            "order_basis": "filename-time", "channel": "F", "tolerance_seconds": 1.0,
            "gaps": 0, "overlaps": 2, "mode": "stream-copy", "duration_check_ok": true,
            "ffmpeg_version": "7.1", "command": "ffmpeg", "command_args": ["-y"],
            "previous_entry_sha256": "0".repeat(64), "entry_sha256": "f".repeat(64)
        })
        .to_string()
    }

    /// Plan §1.9: the join section, its fixed wording and the "이어붙임 N"
    /// counter; the section caveat only shows when joins exist.
    #[test]
    fn report_join_section_counter_and_fixed_wording() {
        let html = render(&[]);
        assert!(html.contains("<h2>이어붙인 영상</h2>"));
        assert!(html.contains("이어붙임<strong id=\"joins\">"));
        for fixed in [
            "원본 영상 프레임을 재인코딩 없이 순서대로 연결(스트림 복사)했습니다. 겹친 구간은 잘라내지 않아 같은 장면이 두 번 나올 수 있고, 끊긴 구간은 채우지 않았습니다. ",
            "순서와 끊김·겹침 판단은 장치가 기록한 시각을 근거로 하며, 장치 시계의 정확성은 검증하지 않았습니다. ",
            "위치·데이터 스트림(GPS 등)은 포함되지 않았습니다.",
        ] {
            assert!(html.contains(fixed), "missing {fixed}");
        }

        let empty = rendered_elements(&[("manifest", "{}")]);
        assert_eq!(empty["joins"]["textContent"], 0);
        assert!(
            empty["joined-videos"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("이어붙인 영상이 없습니다.")
        );
        assert_eq!(empty["join-caveat"]["hidden"], true);

        let entry = join_entry();
        // A torn trailing line must not blank the section.
        let log = format!("{entry}\n{{\"torn\":");
        let page = rendered_elements(&[("manifest", "{}"), ("join", &log)]);
        assert_eq!(page["joins"]["textContent"], 1);
        assert_eq!(page["join-caveat"]["hidden"], false);
        let table = page["joined-videos"]["innerHTML"].as_str().unwrap();
        for needle in [
            "artifacts/joined/join_F_20260910-120000_n10.mp4",
            &"a".repeat(64),
            &"b".repeat(64),
            &"f".repeat(64),
            "<td>10<br>",
            "색인 해시와 불일치 1개",
            "filename-time",
            "<td>F</td>",
            "끊김 0 / 겹침 2",
            "<th>매니페스트 SHA-256</th>",
            "<th>감사 체인</th>",
        ] {
            assert!(table.contains(needle), "missing {needle:?}: {table}");
        }
        assert!(
            page["chain"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("join-video")
        );
        // Time-based join only: the time caveat shows, the order one not.
        assert_eq!(page["join-caveat-time"]["hidden"], false);
        assert_eq!(page["join-caveat-order"]["hidden"], true);
    }

    /// B2 + U2: a filename-order join never reads "끊김 0 / 겹침 0", the
    /// caveat switches to the filename-order wording, and annotations filed
    /// under the joined record's `record_id` (or a legacy `join:<file>` id)
    /// appear in the join table's 판독 column.
    #[test]
    fn report_join_rows_vary_by_basis_and_show_annotations() {
        let mut ordered: serde_json::Value = serde_json::from_str(&join_entry()).unwrap();
        ordered["order_basis"] = "filename-order".into();
        ordered["gaps"] = serde_json::Value::Null;
        ordered["overlaps"] = serde_json::Value::Null;
        ordered["unknown_junctions"] = 9.into();
        ordered["record_id"] = "join:1111111111111111".into();
        ordered["output_path"] = "artifacts/joined/join_U_order_n10.mp4".into();
        let mut legacy: serde_json::Value = serde_json::from_str(&join_entry()).unwrap();
        legacy["output_path"] = "artifacts/joined/join_F_old_n2.mp4".into();
        legacy["output_sha256"] = "2".repeat(64).into();
        let log = format!("{}\n{ordered}\n{legacy}\n", join_entry());
        let annotations = serde_json::json!({
            "marks": [
                {"id": "join:1111111111111111", "status": "important", "note": "<b>사고 장면</b>",
                 "examiner": "kim", "range": {"in": 3, "out": 9}},
                {"id": "join:aaaaaaaaaaaaaaaa", "status": "reviewed"},
                {"id": "join:join_F_old_n2.mp4", "status": "needs_verification"},
            ],
            "tags": {"join:1111111111111111": ["충돌"]},
        })
        .to_string();
        let page =
            rendered_elements(&[("manifest", "{}"), ("join", &log), ("marks", &annotations)]);
        let table = page["joined-videos"]["innerHTML"].as_str().unwrap();
        assert!(
            table.contains("<th>판독 (상태·메모·구간·태그)</th>"),
            "{table}"
        );
        let rows: Vec<&str> = table.split("<tr>").skip(2).collect();
        assert_eq!(rows.len(), 3, "{table}");
        // Time-based entry: counts; its derived id join:aaaa… (from the
        // output SHA-256, no record_id field) carries the reviewed mark.
        assert!(rows[0].contains("끊김 0 / 겹침 2"), "{}", rows[0]);
        assert!(rows[0].contains("판독 완료"), "{}", rows[0]);
        // Filename-order entry: no zero claim, its annotations are shown
        // escaped.
        assert!(
            rows[1].contains("끊김·겹침 확인 불가 — 파일명 순"),
            "{}",
            rows[1]
        );
        assert!(!rows[1].contains("끊김 0"), "{}", rows[1]);
        assert!(rows[1].contains("중요"), "{}", rows[1]);
        assert!(
            rows[1].contains("&lt;b&gt;사고 장면&lt;/b&gt;"),
            "{}",
            rows[1]
        );
        assert!(rows[1].contains("구간: 3.0s ~ 9.0s"), "{}", rows[1]);
        assert!(rows[1].contains("태그: 충돌"), "{}", rows[1]);
        // Legacy id by file name.
        assert!(rows[2].contains("검증 대기"), "{}", rows[2]);
        assert_eq!(page["join-caveat-order"]["hidden"], false);
        assert_eq!(page["join-caveat-time"]["hidden"], false);
        let only_order =
            rendered_elements(&[("manifest", "{}"), ("join", &format!("{ordered}\n"))]);
        assert_eq!(only_order["join-caveat-time"]["hidden"], true);
        assert_eq!(only_order["join-caveat-order"]["hidden"], false);
    }

    fn sound_index() -> String {
        let video = |id: &str, ok: bool, codec: Option<&str>| serde_json::json!({"id": id, "source_path": format!("/e/{id}.mp4"), "ffprobe_ok": ok, "audio_codec": codec});
        serde_json::json!({"videos": [
            video("v_sound", true, Some("aac")),
            video("v_silent", true, Some("aac")),
            video("v_track", true, Some("aac")),
            video("v_none", true, None),
            video("v_stale", true, Some("aac")),
            video("v_error", true, Some("aac")),
            video("v_unknown", false, None),
        ]})
        .to_string()
    }

    /// Plan §2.7: measured-sound wording in the video table and the
    /// techniques metadata cell; only measured facts, never speech.
    #[test]
    fn report_sound_wording_variants_and_caveat() {
        let levels = r#"{
            "v_sound": {"r":"sound","max":-12.3,"mean":-38.0,"sec":60.0,"t":1},
            "v_silent": {"r":"silent","max":-91.04,"mean":-91.04,"sec":60.0,"t":1},
            "v_stale": {"r":"stale","max":null,"mean":null,"sec":null,"t":1},
            "v_error": {"r":"error","max":null,"mean":null,"sec":null,"t":1}
        }"#;
        let page = rendered_elements(&[
            ("manifest", "{}"),
            ("index", &sound_index()),
            ("audio_levels", levels),
        ]);
        let videos = page["videos"]["innerHTML"].as_str().unwrap();
        let techniques = page["techniques"]["innerHTML"].as_str().unwrap();
        assert!(videos.contains("<th>소리(측정값)</th>"));
        for wording in [
            "소리 있음 — 측정: 최대 \u{2212}12.3 dB, 평균 \u{2212}38.0 dB (dBFS, ffmpeg volumedetect)",
            "무음 트랙 — 측정: 최대 \u{2212}91.0 dB (기준: \u{2212}70 dB 이하)",
            "오디오 트랙 있음 — 소리 미측정",
            "오디오 트랙 없음 (ffprobe)",
            "측정 후 파일 변경 — 미확인",
            "미확인",
        ] {
            assert!(videos.contains(wording), "video table lacks {wording:?}");
            assert!(
                techniques.contains(&format!("소리(측정값): {wording}")),
                "techniques meta lacks {wording:?}"
            );
        }
        // Row-level: the right wording lands on the right record.
        let row = |id: &str| {
            let start = videos.find(&format!("<td>{id}</td>")).unwrap();
            let end = start + videos[start..].find("</tr>").unwrap();
            videos[start..end].to_string()
        };
        assert!(row("v_track").contains("오디오 트랙 있음 — 소리 미측정"));
        assert!(row("v_none").contains("오디오 트랙 없음 (ffprobe)"));
        assert!(row("v_stale").contains("측정 후 파일 변경"));
        assert!(row("v_unknown").ends_with("<td>미확인</td>\n    "));
        assert!(row("v_error").contains("소리 미측정 (측정 실패)"));
        assert!(!row("v_sound").contains("무음"));

        let html = render(&[("index", &sound_index()), ("audio_levels", levels)]);
        assert!(html.contains("소리 측정은 오디오 트랙 전체의 신호 레벨(최대·평균, dBFS)만 잰 값입니다. 말소리나 대화가 있는지, 소리가 어디서 났는지는 판단하지 않습니다."));
        // The report never asserts speech.
        for forbidden in ["대화 있음", "음성 녹음", "음성 있음", "말소리 있음"] {
            assert!(!html.contains(forbidden), "{forbidden}");
        }
    }

    /// Without any measurement the report still tells the truth: coded track
    /// -> "소리 미측정"; hostile or malformed map input degrades to `{}`.
    #[test]
    fn report_sound_map_garbage_degrades_to_unmeasured() {
        assert_eq!(super::audio_levels_object(""), "{}");
        assert_eq!(super::audio_levels_object("[1,2]"), "{}");
        assert_eq!(super::audio_levels_object("</script>"), "{}");
        let page = rendered_elements(&[
            ("index", &sound_index()),
            ("audio_levels", "</script><script>alert(1)"),
        ]);
        let videos = page["videos"]["innerHTML"].as_str().unwrap();
        assert!(!videos.contains("소리 있음"));
        assert!(videos.contains("오디오 트랙 있음 — 소리 미측정"));
    }

    #[test]
    fn redaction_note_renders_only_when_applied() {
        let note = crate::util::html_escape(crate::redact::REDACTION_NOTE);
        let plain = render(&[]);
        assert!(!plain.contains(&note));
        assert!(!crate::redact::html_is_redacted(&plain));
        assert!(plain.contains("\"applied\": false"));
        let redacted = render(&[("redacted", "1")]);
        assert!(redacted.contains(&note));
        assert!(crate::redact::html_is_redacted(&redacted));
        // The note says what was NOT redacted (I4).
        assert!(note.contains("not rewritten"));
        assert!(redacted.contains("\"applied\": true"));
    }

    /// I5: only the newest anomaly run and the newest validation of each
    /// target are "current"; older ones are folded history with run times.
    #[test]
    fn anomaly_and_validation_sections_show_only_the_latest_run_as_current() {
        let anomaly = [
            r#"{"event":"anomaly-scan","scanned_unix":100,"label":"candidate-finding","kind":"timestamp-regression","selector":"vid_old","detail":"old"}"#,
            r#"{"event":"anomaly-scan-run","scanned_unix":100,"hash_mode":"stored","finding_count":1}"#,
            r#"{"event":"anomaly-scan","scanned_unix":200,"label":"none","kind":"none","selector":"","detail":"no candidate findings"}"#,
            r#"{"event":"anomaly-scan-run","scanned_unix":200,"hash_mode":"rehash","finding_count":0}"#,
        ]
        .join("\n");
        let validation = [
            r#"{"selector":"vid_1","target_path":"/e/a.mp4","validated_unix":10,"validation_status":"validation-failed"}"#,
            r#"{"selector":"vid_1","target_path":"/e/a.mp4","validated_unix":20,"validation_status":"ffprobe-video-stream-confirmed"}"#,
            r#"{"selector":"vid_2","target_path":"/e/b.mp4","validated_unix":5,"validation_status":"ffprobe-video-stream-confirmed"}"#,
        ]
        .join("\n");
        let elements = rendered_elements(&[("anomaly", &anomaly), ("validation", &validation)]);
        let html = |id: &str| {
            elements[id]["innerHTML"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        let current = html("anomaly-findings");
        assert!(
            current.contains("이 실행에서 이상 징후 후보가 없습니다."),
            "{current}"
        );
        assert!(current.contains("실시간 재해시"), "{current}");
        assert!(!current.contains("vid_old"), "{current}");
        let history = html("anomaly-history");
        assert!(
            history.contains("<details>") && history.contains("vid_old"),
            "{history}"
        );
        assert!(history.contains("현재 결과 아님"), "{history}");
        let current = html("validation-results");
        assert!(!current.contains("validation-failed"), "{current}");
        assert_eq!(current.matches("ffprobe-video-stream-confirmed").count(), 2);
        let history = html("validation-history");
        assert!(history.contains("<details>") && history.contains("validation-failed"));
        assert_eq!(elements["validations"]["textContent"], 2);
    }

    /// R8: when the newest run never wrote its completion marker, the
    /// report says the shown "current" result is the previous completed run.
    #[test]
    fn interrupted_newest_anomaly_run_is_called_out() {
        let anomaly = [
            r#"{"event":"anomaly-scan","scanned_unix":100,"kind":"gap","selector":"vid_done","detail":"a"}"#,
            r#"{"event":"anomaly-scan-run","scanned_unix":100,"hash_mode":"stored","finding_count":1}"#,
            r#"{"event":"anomaly-scan","scanned_unix":300,"kind":"gap","selector":"vid_cut","detail":"b"}"#,
        ]
        .join("\n");
        let elements = rendered_elements(&[("anomaly", &anomaly)]);
        let current = elements["anomaly-findings"]["innerHTML"].as_str().unwrap();
        // R13 P16: the newer unmarked run (its marker may have been
        // deleted) withholds the result; its findings are shown with the
        // notice, the older run is history only.
        assert!(current.contains("현재 결과 없음(결과 아님)"), "{current}");
        assert!(current.contains("표식 없는 더 새 실행 있음"), "{current}");
        assert!(
            current.contains("vid_cut") && !current.contains("vid_done"),
            "{current}"
        );
        assert!(!current.contains("현재 결과 —"), "{current}");
        let history = elements["anomaly-history"]["innerHTML"].as_str().unwrap();
        assert!(
            history.contains("vid_done") && !history.contains("vid_cut"),
            "{history}"
        );
        // A complete newest run carries no such notice.
        let complete = [
            r#"{"event":"anomaly-scan","scanned_unix":100,"kind":"gap","selector":"vid_done","detail":"a"}"#,
            r#"{"event":"anomaly-scan-run","scanned_unix":100,"hash_mode":"stored","finding_count":1}"#,
        ]
        .join("\n");
        let elements = rendered_elements(&[("anomaly", &complete)]);
        let current = elements["anomaly-findings"]["innerHTML"].as_str().unwrap();
        assert!(!current.contains("현재 결과 없음"), "{current}");
        assert!(current.contains("현재 결과 —") && current.contains("vid_done"));
    }

    /// R13 I2: a usable `lane-repair` marker after the last completed
    /// anomaly run / after a target's newest validation makes that value
    /// UNKNOWN (the cut tail may have held a newer one); the marker itself
    /// is never shown as a run or a validation row. Values recorded after
    /// the marker are current again.
    #[test]
    fn r13_i2_repaired_lanes_make_the_latest_values_unknown() {
        let repair = r#"{"event":"lane-repair","kept_entries":2,"removed_lines":1,"removed_bytes":10,"removed_sha256":"aa"}"#;
        let anomaly = [
            r#"{"event":"anomaly-scan","scanned_unix":100,"kind":"gap","selector":"vid_done","detail":"a"}"#,
            r#"{"event":"anomaly-scan-run","scanned_unix":100,"hash_mode":"stored","finding_count":1}"#,
            repair,
        ]
        .join("\n");
        let validation = [
            r#"{"selector":"vid_1","target_path":"/e/a.mp4","validated_unix":20,"validation_status":"ffprobe-video-stream-confirmed"}"#,
            repair,
            r#"{"selector":"vid_2","target_path":"/e/b.mp4","validated_unix":30,"validation_status":"validation-failed"}"#,
        ]
        .join("\n");
        let elements = rendered_elements(&[("anomaly", &anomaly), ("validation", &validation)]);
        let text = |id: &str| {
            elements[id]["innerHTML"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        let current = text("anomaly-findings");
        assert!(current.contains("현재 결과 없음(결과 아님)"), "{current}");
        assert!(current.contains("lane-repair"), "{current}");
        assert!(!current.contains("현재 결과 —"), "{current}");
        assert!(!text("anomaly-history").contains("lane-repair"));
        let results = text("validation-results");
        assert!(results.contains("최신 여부 알 수 없음 1건"), "{results}");
        // vid_2 (after the marker) is current; vid_1 (before) is unknown.
        let (known, unknown) = results.split_once("최신 여부 알 수 없음").unwrap();
        assert!(
            known.contains("/e/b.mp4") && !known.contains("/e/a.mp4"),
            "{results}"
        );
        assert!(unknown.contains("/e/a.mp4"), "{results}");
        assert_eq!(elements["validations"]["textContent"], 1);
        // The marker is never a validation row ("-" target).
        assert_eq!(
            results.matches("</tr>").count() - results.matches("</thead>").count(),
            2,
            "two rows (one per target), no marker row: {results}"
        );
        // A completed run after the marker is current again.
        let anomaly = format!(
            "{anomaly}\n{}\n{}",
            r#"{"event":"anomaly-scan","scanned_unix":300,"kind":"gap","selector":"vid_new","detail":"c"}"#,
            r#"{"event":"anomaly-scan-run","scanned_unix":300,"hash_mode":"stored","finding_count":1}"#,
        );
        let elements = rendered_elements(&[("anomaly", &anomaly)]);
        let current = elements["anomaly-findings"]["innerHTML"].as_str().unwrap();
        assert!(
            current.contains("현재 결과 —") && current.contains("vid_new"),
            "{current}"
        );
    }

    /// R13 I2: the sign-off lane repaired AFTER this record's entry — the
    /// cut tail may have held a newer sign-off or this record's
    /// abandonment: not verified (R12 verified it), and the lane state
    /// says the abandonment is unknown.
    #[test]
    fn r13_i2_signoff_repaired_after_its_entry_is_not_the_latest() {
        let mine = crate::audit_key::AuditKey::new("mine", &[0x93; 32]);
        let keys = std::slice::from_ref(&mine);
        let (root, digests) = signoff_tree("r13-i2");
        let record = v4_record_for(&root, &digests, &["vid_000001"], 5);
        record_with_key(&root, &record, Some(&mine));
        assert!(matches!(
            super::signoff_audit_record_with_keys(&root, &record, true, keys),
            super::SignoffAuditRecord::Verified {
                authenticated: true,
                ..
            }
        ));
        crate::audit::append_chained_jsonl_keyed(
            &root.join(super::SIGNOFF_LOG_REL_PATH),
            r#"{"event":"lane-repair","kept_entries":1,"removed_lines":1,"removed_bytes":10,"removed_sha256":"aa"}"#,
            Some(&mine),
        )
        .unwrap();
        let verdict = super::signoff_audit_record_with_keys(&root, &record, true, keys);
        assert!(
            matches!(&verdict, super::SignoffAuditRecord::NotVerifiable(why) if why.contains("lane-repair") && why.contains("최신인지 알 수 없습니다")),
            "{verdict:?}"
        );
        let state = crate::audit_key::with_test_keys(keys, || {
            super::signoff_lane_state(&root, &record).unwrap()
        });
        assert!(
            state.recorded && !state.abandoned && state.repaired_after,
            "{state:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// I5: a legacy log without run markers still separates runs by time.
    #[test]
    fn legacy_anomaly_log_without_run_markers_groups_by_scan_time() {
        let anomaly = [
            r#"{"event":"anomaly-scan","scanned_unix":100,"kind":"gap","selector":"vid_a","detail":"a"}"#,
            r#"{"event":"anomaly-scan","scanned_unix":300,"kind":"gap","selector":"vid_b","detail":"b"}"#,
        ]
        .join("\n");
        let elements = rendered_elements(&[("anomaly", &anomaly)]);
        let current = elements["anomaly-findings"]["innerHTML"].as_str().unwrap();
        assert!(
            current.contains("vid_b") && !current.contains("vid_a"),
            "{current}"
        );
        assert!(current.contains("완료 표시 없는 실행"), "{current}");
        assert!(
            elements["anomaly-history"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("vid_a")
        );
    }

    /// I6: every time is UTC with the local offset spelled out, and the
    /// report states the basis.
    #[test]
    fn report_times_are_utc_with_explicit_offset() {
        let elements = rendered_elements(&[(
            "scan_runs",
            r#"[{"scanned_unix":1700000000,"video_count":1,"source_path":"x","warnings":[]}]"#,
        )]);
        let chain = elements["chain"]["innerHTML"].as_str().unwrap();
        assert!(chain.contains("2023-11-14 22:13:20 UTC"), "{chain}");
        assert!(
            regex_like_offset(chain),
            "local time must carry an explicit UTC offset: {chain}"
        );
        let html = render(&[]);
        assert!(html.contains("모든 시각은 UTC 기준"));
        assert!(html.contains("custody-statement와 같은 기준"));
        assert!(!html.contains("toLocaleString"));
    }

    fn regex_like_offset(text: &str) -> bool {
        text.match_indices("(현지 ").any(|(at, _)| {
            let tail = &text[at..];
            tail.find(" UTC+")
                .or_else(|| tail.find(" UTC-"))
                .is_some_and(|i| tail[i + 5..].len() >= 5 && tail.as_bytes()[i + 7] == b':')
        })
    }

    /// I9: chain cells say what verification found; nothing is called
    /// tamper-proof unless the log passed keyed verification.
    #[test]
    fn audit_chain_cells_show_verification_and_never_overclaim() {
        let export = r#"{"format":"mp4","output_path":"o.mp4","entry_sha256":"e1"}"#;
        let html = render(&[("export", export)]);
        assert!(
            !html.contains("변조 방지"),
            "no unverified tamper-proof claim"
        );
        let script_for = |chains: &str| {
            let html = super::render_case_report_with_chains(
                &super::ReportInputs {
                    manifest_json: "{}",
                    index_json: "{}",
                    export_log_jsonl: export,
                    proxy_log_jsonl: "",
                    thumbnail_log_jsonl: "",
                    carve_log_jsonl: "",
                    filesystem_log_jsonl: "",
                    validation_log_jsonl: "",
                    anomaly_log_jsonl: "",
                    batch_log_jsonl: "",
                    join_log_jsonl: "",
                    audio_levels_json: "",
                    scan_runs_json: "[]",
                    marks_json: "[]",
                    redaction_applied: false,
                    review_warnings_json: "",
                    read_errors_json: "",
                    telemetry_json: "",
                },
                chains,
            );
            run_report_script(&html)
        };
        let unverified = script_for("{}");
        assert!(
            unverified["clip-exports"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("체인 미검증")
        );
        let structural = script_for(
            r#"{"export":{"path":"artifacts/clips/export-log.jsonl","status":"structural","entries":1,"detail":"x"}}"#,
        );
        let cell = structural["clip-exports"]["innerHTML"].as_str().unwrap();
        assert!(cell.contains("구조만 확인"), "{cell}");
        assert!(!cell.contains("integrity-keyed"), "{cell}");
        let table = structural["audit-chains"]["innerHTML"].as_str().unwrap();
        assert!(
            table.contains("artifacts/clips/export-log.jsonl"),
            "{table}"
        );
        let keyed = script_for(r#"{"export":{"status":"keyed","entries":1}}"#);
        assert!(
            keyed["clip-exports"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("integrity-keyed")
        );
        let failed = script_for(r#"{"export":{"status":"failed","detail":"broken at line 2"}}"#);
        assert!(
            failed["clip-exports"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("체인 검증 실패")
        );
        assert!(
            failed["audit-chains"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("broken at line 2")
        );
    }

    /// I12: number-shaped fields are escaped like every other value.
    #[test]
    fn number_shaped_fields_are_escaped() {
        let export = r#"{"format":"mp4","start_seconds":"<img src=x onerror=alert(1)>","duration_seconds":"<b>d</b>"}"#;
        let carve = r#"{"id":"c1","offset":"<i>o</i>","size_bytes":3}"#;
        let index = r#"{"videos":[{"id":"v1","source_path":"/e/a.mp4","width":"<u>w</u>","height":"<s>h</s>"}]}"#;
        let elements = rendered_elements(&[("export", export), ("carve", carve), ("index", index)]);
        for id in ["clip-exports", "carved-artifacts", "videos"] {
            let html = elements[id]["innerHTML"].as_str().unwrap();
            for tag in ["<img", "<b>", "<i>", "<u>", "<s>"] {
                assert!(!html.contains(tag), "{id} leaks {tag}: {html}");
            }
        }
        assert!(
            elements["clip-exports"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("&lt;img src=x")
        );
    }

    /// D1/D2: a measurement the report cannot vouch for is never shown as
    /// sound or silence.
    #[test]
    fn unverified_and_replayed_sound_states_claim_no_level() {
        let index = r#"{"videos":[{"id":"v1","source_path":"/e/a.mp4","ffprobe_ok":true,"audio_codec":"aac"},{"id":"v2","source_path":"/e/b.mp4","ffprobe_ok":true,"audio_codec":"aac"}]}"#;
        let levels = r#"{"v1":{"r":"unverified","max":null,"mean":null,"sec":null,"t":1},"v2":{"r":"replayed","max":null,"mean":null,"sec":null,"t":1}}"#;
        let elements = rendered_elements(&[("index", index), ("audio_levels", levels)]);
        let videos = elements["videos"]["innerHTML"].as_str().unwrap();
        assert!(
            videos.contains("측정 기록 검증 실패 — 표시 안 함"),
            "{videos}"
        );
        assert!(videos.contains("재개 시 재기록된 측정"), "{videos}");
        assert!(
            !videos.contains("무음") && !videos.contains("소리 있음"),
            "{videos}"
        );
    }

    /// D18: a join whose log line carries its own record id never shows the
    /// annotations filed under `join:<file name>` (a reused name); a legacy
    /// entry (derived id) still does. D3: the measured length and the
    /// checks are shown, or said to be unrecorded.
    #[test]
    fn join_rows_use_legacy_name_lookup_only_for_legacy_entries() {
        let entry = |record_id: Option<&str>, derived: bool, check: Option<serde_json::Value>| {
            let mut value: serde_json::Value = serde_json::from_str(&join_entry()).unwrap();
            match record_id {
                Some(id) => value["record_id"] = id.into(),
                None => {
                    value.as_object_mut().unwrap().remove("record_id");
                }
            }
            if derived {
                value["record_id_derived"] = true.into();
            }
            if let Some(check) = check {
                value["duration_check"] = check;
            }
            value.to_string()
        };
        let marks = serde_json::json!({"marks": [
            {"id": "join:join_F_20260910-120000_n10.mp4", "status": "important", "marked_unix": 1}
        ], "tags": {}})
        .to_string();
        let own = entry(
            Some("join:aaaaaaaaaaaaaaaa"),
            false,
            Some(serde_json::json!({"expected": 600.0, "observed": 598.0, "ok": true})),
        );
        let html =
            rendered_elements(&[("join", &own), ("marks", &marks)])["joined-videos"]["innerHTML"]
                .as_str()
                .unwrap()
                .to_string();
        assert!(
            !html.contains("중요"),
            "own record id: no file-name fallback: {html}"
        );
        assert!(html.contains("관측 9:58 / 예상 10:00"), "{html}");
        assert!(html.contains("길이 검사 통과"), "{html}");
        assert!(html.contains("세그먼트 경계 검증 미기록"), "{html}");

        let legacy = entry(Some("join:aaaaaaaaaaaaaaaa"), true, None);
        let html = rendered_elements(&[("join", &legacy), ("marks", &marks)])["joined-videos"]
            ["innerHTML"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            html.contains("중요"),
            "legacy entry keeps its old annotations: {html}"
        );
        assert!(html.contains("관측 길이 미기록"), "{html}");
        assert!(
            html.contains("길이 검사 통과"),
            "falls back to duration_check_ok: {html}"
        );
    }

    /// D11: an input that could not be read is named — the section says
    /// "읽기 실패", not "없습니다". D9: open review state is shown on top.
    #[test]
    fn unreadable_inputs_and_open_review_state_are_stated() {
        let elements = rendered_elements(&[
            (
                "read_errors",
                r#"{"carve":"artifacts/carved/carve-log.jsonl: Access is denied"}"#,
            ),
            ("review_warnings", r#"["저장 대기열 1건"]"#),
        ]);
        let carved = elements["carved-artifacts"]["innerHTML"].as_str().unwrap();
        assert!(carved.contains("읽기 실패"), "{carved}");
        assert!(!carved.contains("Carving 후보가 없습니다"), "{carved}");
        assert!(
            elements["read-errors"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("Access is denied")
        );
        assert!(
            elements["review-warnings"]["innerHTML"]
                .as_str()
                .unwrap()
                .contains("저장 대기열 1건")
        );
        let clean = rendered_elements(&[]);
        assert_eq!(clean["read-errors"]["innerHTML"], "");
        assert_eq!(clean["review-warnings"]["innerHTML"], "");
    }

    /// D14: the report never implies that the chain alone detects a removed
    /// tail; it points at the sign-off anchors.
    #[test]
    fn audit_note_states_truncation_needs_anchors() {
        let html = render(&[]);
        assert!(
            html.contains("(잘림)을 탐지하지 못합니다"),
            "chain-only limit stated"
        );
        assert!(html.contains("최종화 서명(v4)에 기록된 레인별 항목 수·마지막 해시"));
        assert!(html.contains("[\"audio\", \"소리 측정\"]"));
        assert!(html.contains("[\"signoff\", \"최종화 서명 기록\"]"));
    }

    /// Determinism: the same inputs render the same report bytes.
    #[test]
    fn same_inputs_render_identical_report_bytes() {
        let inputs = [
            ("manifest", r#"{"case_id":"FT-1"}"#),
            (
                "index",
                r#"{"videos":[{"id":"v1","source_path":"/e/a.mp4"}]}"#,
            ),
            ("join", "{\"output_path\":\"x.mp4\"}"),
            ("read_errors", r#"{"batch":"denied"}"#),
        ];
        assert_eq!(render(&inputs), render(&inputs));
    }

    /// D10 helper: the report must have been rendered from these exact
    /// record bytes.
    #[test]
    fn report_matches_only_its_own_signoff_record() {
        let record = "{\"schema_version\":4,\"examiner\":\"A\",\"signed_unix\":5}\n";
        let html = super::inject_signoff_banner(
            "<html><body></body></html>".into(),
            "A",
            5,
            super::SignoffCurrency::Current,
            record,
        );
        assert!(super::report_matches_signoff(&html, record).is_ok());
        let swapped = record.replace("\"signed_unix\":5", "\"signed_unix\":6");
        let err = super::report_matches_signoff(&html, &swapped).unwrap_err();
        assert!(err.contains("not from this record"), "{err}");
        assert!(super::report_matches_signoff("<html></html>", record).is_err());
    }

    /// A minimal case/package tree for sign-off checks: one indexed video
    /// with a hash, a two-entry chained lane, and the marks digests.
    fn signoff_tree(name: &str) -> (std::path::PathBuf, super::SignoffDigests) {
        let root = std::env::temp_dir().join(format!(
            "frametrace-signoff-v4-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("db")).unwrap();
        std::fs::write(
            root.join("db/videos.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({"id": "vid_000001", "sha256": "A".repeat(64)})
            ),
        )
        .unwrap();
        let lane = root.join("evidence/logs/validation-log.jsonl");
        for n in 0..2 {
            crate::audit::append_chained_jsonl_keyed(&lane, &format!(r#"{{"n":{n}}}"#), None)
                .unwrap();
        }
        let digests = super::SignoffDigests {
            content: super::signoff_content_sha256("{\"schema\":3,\"marks\":[1]}"),
            legacy: super::signoff_content_sha256("[]"),
        };
        (root, digests)
    }

    /// Builds a v4 record over `root` for `vid_000001` (what
    /// `Signoff::build_v4` writes, minus the database read) WITHOUT
    /// recording it in the sign-off lane.
    fn v4_record_unrecorded(root: &std::path::Path, digests: &super::SignoffDigests) -> String {
        let bindings = super::signoff_bindings(root, ["vid_000001", "join:nothing"]).unwrap();
        let content = super::signoff_content_sha256(&super::signoff_v4_content_json(
            &digests.content,
            &bindings,
        ));
        let (evidence, lanes) = super::bindings_json(&bindings);
        format!(
            "{{\"schema_version\":4,\"examiner\":\"A\",\"signed_unix\":5,\"annotations_sha256\":\"{}\",\"evidence\":{evidence},\"lanes\":{lanes},\"content_sha256\":\"{content}\"}}\n",
            digests.content
        )
    }

    /// [`v4_record_unrecorded`] recorded in the sign-off audit lane, as a
    /// finalize does (R5-P1: only such a record can read current).
    fn v4_record(root: &std::path::Path, digests: &super::SignoffDigests) -> String {
        let record = v4_record_unrecorded(root, digests);
        super::record_signoff_audit(root, &super::Signoff::parse(&record).unwrap()).unwrap();
        record
    }

    /// D13: a v4 sign-off reads "changed" when a marked record's target
    /// hash changes, when an anchored lane loses its tail, or when the
    /// record's own bindings are edited — and stays current across appends.
    #[test]
    fn v4_signoff_detects_target_hash_change_and_lane_truncation() {
        use super::{SignoffCurrency, SignoffScope, check_signoff};
        let (root, digests) = signoff_tree("detect");
        let record = v4_record(&root, &digests);
        assert!(
            record.contains("\"vid_000001\":\"aaaa"),
            "lowercased: {record}"
        );
        assert!(record.contains("\"join:nothing\":null"), "{record}");
        assert!(
            record.contains("\"evidence/logs/validation-log.jsonl\":{\"entries\":2"),
            "{record}"
        );
        let check = check_signoff(&root, &record, &digests, SignoffScope::Case);
        assert_eq!(
            check.currency,
            SignoffCurrency::PartiallyVerified,
            "{check:?}"
        );
        assert!(
            check
                .not_verified
                .iter()
                .any(|item| item.contains("join:nothing"))
        );
        // The digests-only path (package.rs today) compares the marks part.
        assert_eq!(
            super::signoff_currency(&record, &digests),
            SignoffCurrency::Current
        );

        // Appends after the sign-off are fine.
        let lane = root.join("evidence/logs/validation-log.jsonl");
        crate::audit::append_chained_jsonl_keyed(&lane, r#"{"n":9}"#, None).unwrap();
        assert_eq!(
            check_signoff(&root, &record, &digests, SignoffScope::Case).currency,
            SignoffCurrency::PartiallyVerified
        );

        // Lane tail removed (still a valid chain): changed.
        let text = std::fs::read_to_string(&lane).unwrap();
        std::fs::write(&lane, format!("{}\n", text.lines().next().unwrap())).unwrap();
        let check = check_signoff(&root, &record, &digests, SignoffScope::Case);
        assert_eq!(check.currency, SignoffCurrency::Changed);
        assert!(
            check.changes.iter().any(|c| c.contains("끝부분이 삭제됨")),
            "{check:?}"
        );
        // In a package that never held the lane it is "not verified".
        let unlisted = |_: &str| false;
        let check = check_signoff(&root, &record, &digests, SignoffScope::Package(&unlisted));
        assert!(
            check
                .not_verified
                .iter()
                .any(|c| c.contains("패키지에 포함되지 않은"))
        );

        // Target file hash changed (re-scanned with other bytes).
        let (root2, digests2) = signoff_tree("target");
        let record2 = v4_record(&root2, &digests2);
        std::fs::write(
            root2.join("db/videos.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({"id": "vid_000001", "sha256": "b".repeat(64)})
            ),
        )
        .unwrap();
        let check = check_signoff(&root2, &record2, &digests2, SignoffScope::Case);
        assert_eq!(check.currency, SignoffCurrency::Changed);
        assert!(
            check
                .changes
                .iter()
                .any(|c| c.contains("vid_000001: 케이스 기록의 대상 파일 해시가 서명 후 바뀜")),
            "{check:?}"
        );

        // Editing the record's anchors to match a truncated lane is caught
        // by content_sha256.
        let forged = record2.replace("\"entries\":2", "\"entries\":1");
        let check = check_signoff(&root2, &forged, &digests2, SignoffScope::Case);
        assert!(
            check
                .changes
                .iter()
                .any(|c| c.contains("서명 기록이 변경됨")),
            "{check:?}"
        );
        // Marks changed.
        let other = super::SignoffDigests {
            content: super::signoff_content_sha256("other"),
            legacy: digests2.legacy.clone(),
        };
        let check = check_signoff(&root2, &record2, &other, SignoffScope::Case);
        assert!(
            check.changes.iter().any(|c| c.contains("판독 마크")),
            "{check:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root2);
    }

    /// R5-P1: a v4 record forged consistently (marks changed, every hash
    /// recomputed) is not "current": the sign-off lane does not name it.
    /// Without any sign-off lane the record is "not verifiable"; a lane
    /// that records other sign-offs contradicts it.
    #[test]
    fn v4_signoff_is_current_only_with_its_audit_entry() {
        use super::{SignoffCurrency, SignoffScope, check_signoff};
        let (root, digests) = signoff_tree("p1-forged");
        // Not finalized through the workstation: no lane at all.
        let unrecorded = v4_record_unrecorded(&root, &digests);
        let check = check_signoff(&root, &unrecorded, &digests, SignoffScope::Case);
        assert_eq!(check.currency, SignoffCurrency::Unbound, "{check:?}");
        assert!(
            check
                .not_verified
                .iter()
                .any(|c| c.contains("서명 확인 불가")),
            "{check:?}"
        );
        let (recorded, note) = super::signoff_audit_status(&root, &unrecorded);
        assert!(!recorded && note.contains("서명 확인 불가"), "{note}");
        // The genuine finalize records it: current.
        let genuine = v4_record(&root, &digests);
        assert_eq!(genuine, unrecorded);
        let check = check_signoff(&root, &genuine, &digests, SignoffScope::Case);
        assert_eq!(
            check.currency,
            SignoffCurrency::PartiallyVerified,
            "{check:?}"
        );
        // A forger changes the marks and recomputes a self-consistent v4
        // record (annotations_sha256 and content_sha256 both match).
        let forged_digests = super::SignoffDigests {
            content: super::signoff_content_sha256("{\"schema\":3,\"marks\":[2]}"),
            legacy: digests.legacy.clone(),
        };
        let forged = v4_record_unrecorded(&root, &forged_digests);
        let check = check_signoff(&root, &forged, &forged_digests, SignoffScope::Case);
        assert_eq!(check.currency, SignoffCurrency::Changed, "{check:?}");
        assert!(
            check.changes.iter().any(|c| c.contains("교체")),
            "{check:?}"
        );
        // Package without the lane: not verifiable, never a pass.
        let only_record = |rel: &str| rel != super::SIGNOFF_LOG_REL_PATH;
        let check = check_signoff(
            &root,
            &genuine,
            &digests,
            SignoffScope::Package(&only_record),
        );
        assert_eq!(check.currency, SignoffCurrency::Unbound, "{check:?}");
        // The banner says so in words.
        let banner = super::signoff_banner_checked_html("A", 5, &check, true, "x");
        assert!(banner.contains("서명 확인 불가"), "{banner}");
        // With an audit key configured, an unkeyed entry proves nothing.
        let key = crate::audit_key::AuditKey::new("k", &[7; 32]);
        let record = super::signoff_audit_record_with_keys(&root, &genuine, true, &[key]);
        assert!(
            matches!(record, super::SignoffAuditRecord::NotVerifiable(_)),
            "{record:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R5-P2: the bindings say what they compare ("케이스 기록의 해시"), and
    /// a live re-hash catches a target whose bytes changed while the case
    /// records still carry the signed hash; a file that cannot be read is
    /// "not verified", not changed.
    #[test]
    fn v4_signoff_rehash_compares_the_files_themselves() {
        use super::{LiveHash, SignoffCurrency, SignoffScope, check_signoff, check_signoff_with};
        let (root, digests) = signoff_tree("p2-rehash");
        let media = root.join("clip.mp4");
        std::fs::write(&media, b"signed bytes").unwrap();
        let sha = crate::sha256::digest_bytes(b"signed bytes");
        std::fs::write(
            root.join("db/videos.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({"id": "vid_000001", "sha256": sha, "source_path": media.to_string_lossy()})
            ),
        )
        .unwrap();
        let record = v4_record(&root, &digests);
        let none = std::collections::BTreeMap::new();
        let live = super::rehash_signed_targets(&root, &record, &none);
        assert_eq!(live.get("vid_000001"), Some(&LiveHash::Hashed(sha.clone())));
        let check = check_signoff_with(&root, &record, &digests, SignoffScope::Case, Some(&live));
        assert_eq!(
            check.currency,
            SignoffCurrency::PartiallyVerified,
            "{check:?}"
        );
        assert_eq!(check.rehashed, 1);
        let banner = super::signoff_banner_checked_html("A", 5, &check, true, "x");
        assert!(banner.contains("케이스 기록의 대상 파일 해시"), "{banner}");
        assert!(banner.contains("다시 해시"), "{banner}");
        // The file is replaced; the index still records the signed hash.
        std::fs::write(&media, b"other bytes!").unwrap();
        assert_eq!(
            check_signoff(&root, &record, &digests, SignoffScope::Case).currency,
            SignoffCurrency::PartiallyVerified,
            "records alone cannot see it"
        );
        let live = super::rehash_signed_targets(&root, &record, &none);
        let check = check_signoff_with(&root, &record, &digests, SignoffScope::Case, Some(&live));
        assert_eq!(check.currency, SignoffCurrency::Changed, "{check:?}");
        assert!(
            check.changes.iter().any(|c| c.contains("다시 해시한 값")),
            "{check:?}"
        );
        // Reused results (finalize's rehash of the originals) are used.
        let reuse = std::collections::BTreeMap::from([(
            "vid_000001".to_string(),
            LiveHash::Unavailable("잠김".into()),
        )]);
        let live = super::rehash_signed_targets(&root, &record, &reuse);
        let check = check_signoff_with(&root, &record, &digests, SignoffScope::Case, Some(&live));
        assert_eq!(
            check.currency,
            SignoffCurrency::PartiallyVerified,
            "{check:?}"
        );
        assert!(
            check
                .not_verified
                .iter()
                .any(|c| c.contains("vid_000001") && c.contains("잠김")),
            "{check:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R5-P5: a hash the case no longer records and a lane that cannot be
    /// read are "not verified" — only real differences are "changed".
    #[test]
    fn v4_signoff_unreadable_inputs_are_not_verified_not_changed() {
        use super::{SignoffCurrency, SignoffScope, check_signoff};
        let (root, digests) = signoff_tree("p5");
        let record = v4_record(&root, &digests);
        // The index lost the hash (re-scan without hashing).
        std::fs::write(
            root.join("db/videos.jsonl"),
            format!("{}\n", serde_json::json!({"id": "vid_000001"})),
        )
        .unwrap();
        // The anchored lane cannot be read (a directory in its place).
        let lane = root.join("evidence/logs/validation-log.jsonl");
        std::fs::remove_file(&lane).unwrap();
        std::fs::create_dir_all(&lane).unwrap();
        let check = check_signoff(&root, &record, &digests, SignoffScope::Case);
        assert!(check.changes.is_empty(), "{check:?}");
        assert_eq!(
            check.currency,
            SignoffCurrency::PartiallyVerified,
            "{check:?}"
        );
        assert!(
            check
                .not_verified
                .iter()
                .any(|c| c.contains("vid_000001") && c.contains("확인 불가")),
            "{check:?}"
        );
        assert!(
            check
                .not_verified
                .iter()
                .any(|c| c.contains("validation-log") && c.contains("확인 불가")),
            "{check:?}"
        );
        let banner = super::signoff_banner_checked_html("A", 5, &check, true, "x");
        assert!(banner.contains("확인 불가"), "{banner}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// D13: records of older schemas keep their legacy meaning through the
    /// full check (no new failure modes for existing sign-offs).
    #[test]
    fn legacy_signoff_schemas_keep_their_binding_meaning() {
        use super::{SignoffCurrency, SignoffScope, check_signoff};
        let (root, digests) = signoff_tree("legacy");
        let v3 = format!(
            r#"{{"schema_version":3,"examiner":"A","signed_unix":5,"content_sha256":"{}"}}"#,
            digests.content
        );
        let check = check_signoff(&root, &v3, &digests, SignoffScope::Case);
        assert_eq!(check.currency, SignoffCurrency::Current);
        assert!(check.changes.is_empty());
        let v2 = format!(
            r#"{{"schema_version":2,"examiner":"A","signed_unix":5,"content_sha256":"{}"}}"#,
            digests.legacy
        );
        assert_eq!(
            check_signoff(&root, &v2, &digests, SignoffScope::Case).currency,
            SignoffCurrency::LegacyBinding
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A v4 record over `ids` (see [`v4_record_unrecorded`]).
    fn v4_record_for(
        root: &std::path::Path,
        digests: &super::SignoffDigests,
        ids: &[&str],
        signed_unix: i64,
    ) -> String {
        let bindings = super::signoff_bindings(root, ids.iter().copied()).unwrap();
        let content = super::signoff_content_sha256(&super::signoff_v4_content_json(
            &digests.content,
            &bindings,
        ));
        let (evidence, lanes) = super::bindings_json(&bindings);
        format!(
            "{{\"schema_version\":4,\"examiner\":\"A\",\"signed_unix\":{signed_unix},\"annotations_sha256\":\"{}\",\"evidence\":{evidence},\"lanes\":{lanes},\"content_sha256\":\"{content}\"}}\n",
            digests.content
        )
    }

    /// Appends `record`'s sign-off lane entry under `key` (None = unkeyed).
    fn record_with_key(
        root: &std::path::Path,
        record: &str,
        key: Option<&crate::audit_key::AuditKey>,
    ) {
        crate::audit::append_chained_jsonl_keyed(
            &root.join(super::SIGNOFF_LOG_REL_PATH),
            &super::signoff_audit_body(&super::Signoff::parse(record).unwrap()),
            key,
        )
        .unwrap();
    }

    /// R6-I1: one binding that cannot be checked (a marked record without a
    /// hash) stops a v4 record from reading current: PartiallyVerified,
    /// amber, with the count — never the "unchanged" wording.
    #[test]
    fn v4_signoff_with_an_unverifiable_item_is_partially_verified() {
        use super::{SignoffCurrency, SignoffScope, check_signoff};
        let (root, digests) = signoff_tree("i1");
        let full = v4_record_for(&root, &digests, &["vid_000001"], 5);
        super::record_signoff_audit(&root, &super::Signoff::parse(&full).unwrap()).unwrap();
        let check = check_signoff(&root, &full, &digests, SignoffScope::Case);
        assert_eq!(check.currency, SignoffCurrency::Current, "{check:?}");
        assert!(check.not_verified.is_empty(), "{check:?}");
        let partial = v4_record_for(&root, &digests, &["vid_000001", "join:nothing"], 6);
        super::record_signoff_audit(&root, &super::Signoff::parse(&partial).unwrap()).unwrap();
        let check = check_signoff(&root, &partial, &digests, SignoffScope::Case);
        assert_eq!(
            check.currency,
            SignoffCurrency::PartiallyVerified,
            "{check:?}"
        );
        let banner = super::signoff_banner_checked_html("A", 6, &check, true, "x");
        assert!(banner.contains("1개 항목 확인 불가"), "{banner}");
        assert!(banner.contains("서명 일치를 단정할 수 없"), "{banner}");
        assert!(!banner.contains("변경되지 않았습니다"), "{banner}");
        assert!(banner.contains("#fff4e0"), "amber: {banner}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R6-I2: a sign-off entry signed only by a RETIRED key, with no entry
    /// of a current key after it, does not authenticate the record; once a
    /// current key's entry follows (proving its age) it does.
    #[test]
    fn retired_key_signoff_entry_is_not_verifiable_until_proven() {
        use super::SignoffAuditRecord;
        let (root, digests) = signoff_tree("i2");
        let record = v4_record_for(&root, &digests, &["vid_000001"], 5);
        let retired = crate::audit_key::AuditKey::new("old", &[3; 32]).retired_at(100);
        let active = crate::audit_key::AuditKey::new("new", &[4; 32]);
        let keys = [active.clone(), retired.clone()];
        record_with_key(&root, &record, Some(&retired));
        let audit = super::signoff_audit_record_with_keys(&root, &record, true, &keys);
        assert!(
            matches!(&audit, SignoffAuditRecord::NotVerifiable(why) if why.contains("은퇴")),
            "{audit:?}"
        );
        crate::audit::append_chained_jsonl_keyed(
            &root.join(super::SIGNOFF_LOG_REL_PATH),
            r#"{"event":"note"}"#,
            Some(&active),
        )
        .unwrap();
        let audit = super::signoff_audit_record_with_keys(&root, &record, true, &keys);
        assert!(
            matches!(
                audit,
                SignoffAuditRecord::Verified {
                    authenticated: true,
                    ..
                }
            ),
            "{audit:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R6-I4/I5/I7/I8: unknown key id → not verifiable (not a FAIL); an
    /// older-schema record next to later finalize attempts → not
    /// verifiable (not a FAIL); a genuine record with a newer sign-off
    /// after it → "not the latest"; an unkeyed entry in a now-keyed case
    /// names the re-finalize remedy.
    #[test]
    fn signoff_lane_edge_cases_are_not_verifiable_with_reasons() {
        use super::SignoffAuditRecord;
        let (root, digests) = signoff_tree("i4-i8");
        let mine = crate::audit_key::AuditKey::new("mine", &[5; 32]);
        let other = crate::audit_key::AuditKey::new("other-pc", &[6; 32]);
        // I4: signed on another PC.
        let a = v4_record_for(&root, &digests, &["vid_000001"], 5);
        record_with_key(&root, &a, Some(&other));
        let audit =
            super::signoff_audit_record_with_keys(&root, &a, true, std::slice::from_ref(&mine));
        // R8 P2: "not authenticable here — check on the PC holding it".
        assert!(
            matches!(&audit, SignoffAuditRecord::NotVerifiable(why)
                if why.contains("이 PC에서 인증 불가") && why.contains("\"other-pc\" 보유 PC에서 확인")),
            "{audit:?}"
        );
        // I7: a newer sign-off B follows A in the lane.
        let b = v4_record_for(&root, &digests, &["vid_000001"], 6);
        record_with_key(&root, &b, Some(&mine));
        let audit =
            super::signoff_audit_record_with_keys(&root, &b, true, std::slice::from_ref(&mine));
        assert!(
            matches!(
                audit,
                SignoffAuditRecord::Verified {
                    authenticated: true,
                    ..
                }
            ),
            "{audit:?}"
        );
        let audit = super::signoff_audit_record_with_keys(&root, &a, true, &[]);
        assert!(
            matches!(&audit, SignoffAuditRecord::NotVerifiable(why) if why.contains("더 최신 서명 존재")),
            "{audit:?}"
        );
        // I5: a v3 record (never recorded in the lane) after a failed v4
        // re-finalize recorded one: not verifiable, not contradicted.
        let v3 = format!(
            r#"{{"schema_version":3,"examiner":"A","signed_unix":5,"content_sha256":"{}"}}"#,
            digests.content
        );
        let audit = super::signoff_audit_record_with_keys(&root, &v3, true, &[]);
        assert!(
            matches!(&audit, SignoffAuditRecord::NotVerifiable(why) if why.contains("이전 형식")),
            "{audit:?}"
        );
        // I8: finalized without a key, key configured afterwards.
        let c = v4_record_for(&root, &digests, &["vid_000001"], 7);
        record_with_key(&root, &c, None);
        let audit =
            super::signoff_audit_record_with_keys(&root, &c, true, std::slice::from_ref(&mine));
        assert!(
            matches!(&audit, SignoffAuditRecord::NotVerifiable(why) if why.contains("키 설정 후 다시 최종화")),
            "{audit:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R6-I6: a keyless match reads "self-consistency only", never like an
    /// authentication, in the audit note and the current banner.
    #[test]
    fn keyless_current_signoff_says_self_consistency_only() {
        use super::{SignoffAuditRecord, SignoffCheck, SignoffCurrency};
        let (root, digests) = signoff_tree("i6");
        let record = v4_record_for(&root, &digests, &["vid_000001"], 5);
        record_with_key(&root, &record, None);
        let audit = super::signoff_audit_record_with_keys(&root, &record, true, &[]);
        assert!(
            matches!(
                audit,
                SignoffAuditRecord::Verified {
                    authenticated: false,
                    ..
                }
            ),
            "{audit:?}"
        );
        assert!(audit.note().contains("인증 아님"), "{}", audit.note());
        let check = SignoffCheck {
            currency: SignoffCurrency::Current,
            changes: Vec::new(),
            not_verified: Vec::new(),
            rehashed: 0,
            authenticated: false,
        };
        let banner = super::signoff_banner_checked_html("A", 5, &check, true, &audit.note());
        assert!(
            banner.contains("(자기 일관성만 확인 — 인증 아님)"),
            "{banner}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// E9: a telemetry track cut at the point cap is shown as truncated
    /// with its total, never as a complete track.
    #[test]
    fn truncated_telemetry_track_is_labelled() {
        let telemetry = r#"{"t1":{"selector":"vid_1","source":"nmea-file-scan","point_count":20000,"total_points":51234,"truncated":true},"t2":{"selector":"vid_2","point_count":12,"total_points":12,"truncated":false}}"#;
        let html = rendered_elements(&[("telemetry", telemetry)])["telemetry"]["innerHTML"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            html.contains("전체 51234점 중 20000점만 기록 — 잘림"),
            "{html}"
        );
        assert!(html.contains("<td>12점</td>"), "{html}");
        let empty = rendered_elements(&[])["telemetry"]["innerHTML"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(empty.contains("추출 기록이 없습니다"), "{empty}");
    }

    /// R7-I2: a genuine v4 sign-off re-labelled `schema_version` 3 (with
    /// `content_sha256` = the marks digest anyone can compute) or 5 used to
    /// read Current without the sign-off lane; now a v<4 record in a case
    /// whose lane has entries is at most Unbound, and v>4 is rejected.
    #[test]
    fn downgraded_or_future_schema_signoff_is_never_current() {
        use super::{SignoffCurrency, SignoffScope, check_signoff, signoff_currency};
        let (root, digests) = signoff_tree("r7-i2");
        let genuine = v4_record_for(&root, &digests, &["vid_000001"], 5);
        super::record_signoff_audit(&root, &super::Signoff::parse(&genuine).unwrap()).unwrap();
        assert_eq!(
            check_signoff(&root, &genuine, &digests, SignoffScope::Case).currency,
            SignoffCurrency::Current
        );
        let v3 = format!(
            r#"{{"schema_version":3,"examiner":"A","signed_unix":5,"content_sha256":"{}"}}"#,
            digests.content
        );
        let check = check_signoff(&root, &v3, &digests, SignoffScope::Case);
        assert_eq!(check.currency, SignoffCurrency::Unbound, "{check:?}");
        assert!(
            check
                .not_verified
                .iter()
                .any(|item| item.contains("signoff-log")),
            "{check:?}"
        );
        let banner = super::signoff_banner_checked_html("A", 5, &check, false, "x");
        assert!(banner.contains("서명 확인 불가"), "{banner}");
        assert!(!banner.contains("대상 내용의 해시가 없어"), "{banner}");
        // A changed legacy record still reads changed.
        let v3_changed = v3.replace(&digests.content, &"0".repeat(64));
        assert_eq!(
            check_signoff(&root, &v3_changed, &digests, SignoffScope::Case).currency,
            SignoffCurrency::Changed
        );
        for schema in [5, 99] {
            let future = genuine.replace(
                "\"schema_version\":4",
                &format!("\"schema_version\":{schema}"),
            );
            let check = check_signoff(&root, &future, &digests, SignoffScope::Case);
            assert_eq!(check.currency, SignoffCurrency::Unbound, "{check:?}");
            assert!(
                check.not_verified[0].contains(&format!("schema_version {schema}")),
                "{check:?}"
            );
            assert_eq!(
                signoff_currency(&future, &digests),
                SignoffCurrency::Unbound
            );
            assert!(matches!(
                super::signoff_audit_record_with_keys(&root, &future, true, &[]),
                super::SignoffAuditRecord::NotVerifiable(_)
            ));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R7-I5: a sign-off entry HMAC-signed under a key this (reviewer) PC
    /// does not have is checked on the PC holding that key — the note no
    /// longer tells the reviewer to configure a key and re-finalize.
    #[test]
    fn keyed_entry_on_a_keyless_pc_points_to_the_key_holder() {
        use super::SignoffAuditRecord;
        let (root, digests) = signoff_tree("r7-i5");
        let record = v4_record_for(&root, &digests, &["vid_000001"], 5);
        let key = crate::audit_key::AuditKey::new("kid-0123abcd", &[7; 32]);
        record_with_key(&root, &record, Some(&key));
        let audit = super::signoff_audit_record_with_keys(&root, &record, true, &[]);
        assert!(
            matches!(
                &audit,
                SignoffAuditRecord::Verified {
                    authenticated: false,
                    unchecked_key_id: Some(id),
                    ..
                } if id == "kid-0123abcd"
            ),
            "{audit:?}"
        );
        let note = audit.note();
        assert!(
            note.contains("\"kid-0123abcd\"를 가진 PC에서 확인"),
            "{note}"
        );
        assert!(!note.contains("다시 최종화"), "{note}");
        // A keyless (unkeyed) entry keeps the re-finalize remedy.
        let (root2, digests2) = signoff_tree("r7-i5-unkeyed");
        let record2 = v4_record_for(&root2, &digests2, &["vid_000001"], 5);
        record_with_key(&root2, &record2, None);
        let note = super::signoff_audit_record_with_keys(&root2, &record2, true, &[]).note();
        assert!(note.contains("다시 최종화"), "{note}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root2);
    }

    /// R7-I3/I7: a sign-off lane whose tail was re-written under an
    /// invented key id after lines this PC authenticated is not verifiable;
    /// a v4 record names the lane's entry count at signing and its entry
    /// must sit right after them.
    #[test]
    fn signoff_lane_rewrite_and_position_are_checked() {
        use super::SignoffAuditRecord;
        let (root, digests) = signoff_tree("r7-i3");
        let mine = crate::audit_key::AuditKey::new("mine", &[5; 32]);
        let bogus = crate::audit_key::AuditKey::new("bogus-id", &[6; 32]);
        let record = v4_record_for(&root, &digests, &["vid_000001"], 5);
        record_with_key(&root, &record, Some(&mine));
        crate::audit::append_chained_jsonl_keyed(
            &root.join(super::SIGNOFF_LOG_REL_PATH),
            r#"{"event":"note"}"#,
            Some(&bogus),
        )
        .unwrap();
        let audit = super::signoff_audit_record_with_keys(
            &root,
            &record,
            true,
            std::slice::from_ref(&mine),
        );
        assert!(
            matches!(&audit, SignoffAuditRecord::NotVerifiable(why) if why.contains("재작성 배제 불가")),
            "{audit:?}"
        );
        let _ = std::fs::remove_dir_all(&root);

        // I7: the record says the lane held 1 entry at signing, but its
        // entry is the first line.
        let (root, digests) = signoff_tree("r7-i7");
        let record = v4_record_for(&root, &digests, &["vid_000001"], 5).replace(
            "\"content_sha256\"",
            "\"signoff_log_entries\":1,\"content_sha256\"",
        );
        record_with_key(&root, &record, None);
        let audit = super::signoff_audit_record_with_keys(&root, &record, true, &[]);
        assert!(
            matches!(&audit, SignoffAuditRecord::Contradicted(why) if why.contains("잘렸거나")),
            "{audit:?}"
        );
        let at_zero = record.replace("\"signoff_log_entries\":1", "\"signoff_log_entries\":0");
        record_with_key(&root, &at_zero, None);
        // `at_zero` is the 2nd line: still contradicted (expected index 0).
        assert!(matches!(
            super::signoff_audit_record_with_keys(&root, &at_zero, true, &[]),
            SignoffAuditRecord::Contradicted(_)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let (root, digests) = signoff_tree("r7-i7b");
        let record = v4_record_for(&root, &digests, &["vid_000001"], 5).replace(
            "\"content_sha256\"",
            "\"signoff_log_entries\":0,\"content_sha256\"",
        );
        record_with_key(&root, &record, None);
        assert!(matches!(
            super::signoff_audit_record_with_keys(&root, &record, true, &[]),
            SignoffAuditRecord::Verified { .. }
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R8-I2 (P0): a genuine v4 sign-off re-labelled v3 AND its sign-off
    /// lane deleted read Current in R7 (the v<4 cap looked at the lane). On
    /// a PC holding audit keys a legacy record is now never Current — at
    /// most amber, "legacy binding, no authentication". P1: on a PC without
    /// keys the legacy meaning is unchanged.
    #[test]
    fn r8_i2_legacy_signoff_with_its_lane_deleted_is_never_current_where_keys_are_held() {
        use super::{SignoffCurrency, SignoffScope, check_signoff};
        let (root, digests) = signoff_tree("r8-i2");
        let mine = crate::audit_key::AuditKey::new("mine", &[0x91; 32]);
        let genuine = v4_record_for(&root, &digests, &["vid_000001"], 5);
        record_with_key(&root, &genuine, Some(&mine));
        let v3 = format!(
            r#"{{"schema_version":3,"examiner":"A","signed_unix":5,"content_sha256":"{}"}}"#,
            digests.content
        );
        std::fs::remove_file(root.join(super::SIGNOFF_LOG_REL_PATH)).unwrap();
        crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
            let check = check_signoff(&root, &v3, &digests, SignoffScope::Case);
            assert_eq!(check.currency, SignoffCurrency::Unbound, "{check:?}");
            assert!(
                check
                    .not_verified
                    .iter()
                    .any(|item| item.contains("레거시 바인딩")),
                "{check:?}"
            );
            let banner = super::signoff_banner_checked_html("A", 5, &check, false, "x");
            assert!(banner.contains("#fff4e0"), "amber: {banner}");
            assert!(!banner.contains("변경되지 않았습니다"), "{banner}");
            // A changed legacy record still reads changed.
            let changed = v3.replace(&digests.content, &"0".repeat(64));
            assert_eq!(
                check_signoff(&root, &changed, &digests, SignoffScope::Case).currency,
                SignoffCurrency::Changed
            );
        });
        crate::audit_key::with_test_keys(&[], || {
            assert_eq!(
                check_signoff(&root, &v3, &digests, SignoffScope::Case).currency,
                SignoffCurrency::Current
            );
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R8 P0/P2 for sign-offs: a v4 sign-off whose lane entry was stripped
    /// of its HMAC is not confirmed on a PC holding keys; one signed on
    /// another PC is "not authenticable here — check on the PC holding key
    /// <id>": never Current, and never a FAIL (no change reported).
    #[test]
    fn r8_signoff_entry_stripped_or_from_another_pc_is_never_current() {
        use super::{SignoffCurrency, SignoffScope, check_signoff};
        let mine = crate::audit_key::AuditKey::new("mine", &[0x92; 32]);
        let other = crate::audit_key::AuditKey::new("pc-b", &[0x93; 32]);
        for (name, key) in [("r8-so-strip", None), ("r8-so-other", Some(&other))] {
            let (root, digests) = signoff_tree(name);
            let record = v4_record_for(&root, &digests, &["vid_000001"], 5);
            record_with_key(&root, &record, key);
            crate::audit_key::with_test_keys(std::slice::from_ref(&mine), || {
                let check = check_signoff(&root, &record, &digests, SignoffScope::Case);
                assert_eq!(check.currency, SignoffCurrency::Unbound, "{check:?}");
                assert!(check.changes.is_empty(), "never a FAIL: {check:?}");
                if key.is_some() {
                    assert!(
                        check
                            .not_verified
                            .iter()
                            .any(|item| item.contains("\"pc-b\" 보유 PC에서 확인")),
                        "{check:?}"
                    );
                }
            });
            // The PC holding the key that signed it: Current.
            if let Some(key) = key {
                // R10-I8: its sign-off entry is authenticated there; the
                // fixture's other lanes were written WITHOUT a key, so their
                // anchors are not authenticated on a PC holding keys —
                // partially verified, never "changed".
                crate::audit_key::with_test_keys(std::slice::from_ref(key), || {
                    let check = check_signoff(&root, &record, &digests, SignoffScope::Case);
                    assert!(check.authenticated, "{check:?}");
                    assert!(check.changes.is_empty(), "{check:?}");
                    assert!(
                        check
                            .not_verified
                            .iter()
                            .all(|item| item.contains("앵커 확인 불가")),
                        "{check:?}"
                    );
                });
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// R8-I9: the position check is "the first sign-off entry at or after
    /// the recorded lane length" — another event appended in between (a
    /// rotation marker racing the finalize) no longer contradicts the
    /// record — and the sign-off entry of a finalize that FAILED after
    /// recording (a trusted `signoff-abandoned` entry names it) is shown
    /// only: the previous sign-off stays the latest, and an older-schema
    /// record is not capped by it.
    #[test]
    fn r8_i9_position_race_and_abandoned_refinalize() {
        use super::{SignoffAuditRecord, SignoffCurrency, SignoffScope, check_signoff};
        let mine = crate::audit_key::AuditKey::new("mine", &[0x94; 32]);
        let keys = std::slice::from_ref(&mine);
        // (a) A rotation marker lands between the record and its entry.
        let (root, digests) = signoff_tree("r8-i9a");
        let record = v4_record_for(&root, &digests, &["vid_000001"], 5).replace(
            "\"content_sha256\"",
            "\"signoff_log_entries\":0,\"content_sha256\"",
        );
        crate::audit::append_chained_jsonl_keyed(
            &root.join(super::SIGNOFF_LOG_REL_PATH),
            r#"{"kind":"audit-key-rotate","from":null,"to":"mine"}"#,
            Some(&mine),
        )
        .unwrap();
        record_with_key(&root, &record, Some(&mine));
        assert!(
            matches!(
                super::signoff_audit_record_with_keys(&root, &record, true, keys),
                SignoffAuditRecord::Verified {
                    authenticated: true,
                    ..
                }
            ),
            "a non-signoff event before the entry is not a cut lane"
        );
        // ...but another sign-off entry in between still is.
        let (root2, digests2) = signoff_tree("r8-i9a2");
        let other = v4_record_for(&root2, &digests2, &["vid_000001"], 4);
        let record2 = v4_record_for(&root2, &digests2, &["vid_000001"], 5).replace(
            "\"content_sha256\"",
            "\"signoff_log_entries\":0,\"content_sha256\"",
        );
        record_with_key(&root2, &other, Some(&mine));
        record_with_key(&root2, &record2, Some(&mine));
        assert!(matches!(
            super::signoff_audit_record_with_keys(&root2, &record2, true, keys),
            SignoffAuditRecord::Contradicted(_)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root2);

        // (b) A re-finalize recorded B, then failed and abandoned it.
        let (root, digests) = signoff_tree("r8-i9b");
        let a = v4_record_for(&root, &digests, &["vid_000001"], 5);
        record_with_key(&root, &a, Some(&mine));
        let b = v4_record_for(&root, &digests, &["vid_000001"], 6);
        record_with_key(&root, &b, Some(&mine));
        let lane = root.join(super::SIGNOFF_LOG_REL_PATH);
        let abandon = |key: Option<&crate::audit_key::AuditKey>| {
            crate::audit::append_chained_jsonl_keyed(
                &lane,
                &serde_json::json!({"event": super::SIGNOFF_ABANDONED_EVENT,
                    "signoff_record_sha256": crate::sha256::digest_bytes(b.as_bytes())})
                .to_string(),
                key,
            )
            .unwrap();
        };
        // P0: an UNAUTHENTICATED abandonment note does not count here.
        abandon(None);
        assert!(matches!(
            super::signoff_audit_record_with_keys(&root, &a, true, keys),
            SignoffAuditRecord::NotVerifiable(why) if why.contains("더 최신 서명")
        ));
        abandon(Some(&mine));
        assert!(
            matches!(
                super::signoff_audit_record_with_keys(&root, &a, true, keys),
                SignoffAuditRecord::Verified {
                    authenticated: true,
                    ..
                }
            ),
            "the abandoned attempt is shown only"
        );
        let _ = std::fs::remove_dir_all(&root);

        // (c) Keyless: a v3 record next to only an abandoned v4 attempt is
        // not capped (R7 downgraded it to Unbound).
        let (root, digests) = signoff_tree("r8-i9c");
        let attempt = v4_record_for(&root, &digests, &["vid_000001"], 6);
        record_with_key(&root, &attempt, None);
        crate::audit_key::with_test_keys(&[], || {
            super::record_signoff_abandoned(&root, &attempt, "report failed").unwrap();
        });
        let v3 = format!(
            r#"{{"schema_version":3,"examiner":"A","signed_unix":5,"content_sha256":"{}"}}"#,
            digests.content
        );
        crate::audit_key::with_test_keys(&[], || {
            assert_eq!(
                check_signoff(&root, &v3, &digests, SignoffScope::Case).currency,
                SignoffCurrency::Current
            );
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R9-I6: the record of an ABANDONED finalize (a trusted
    /// `signoff-abandoned` entry names it) restored as `review/signoff.json`
    /// is never Current — R8 skipped the abandonment only for OTHER records,
    /// so the abandoned record itself still verified.
    #[test]
    fn r9_i6_abandoned_record_restored_is_never_current() {
        use super::SignoffAuditRecord;
        let mine = crate::audit_key::AuditKey::new("mine", &[0x9b; 32]);
        let keys = std::slice::from_ref(&mine);
        let (root, digests) = signoff_tree("r9-i6");
        let a = v4_record_for(&root, &digests, &["vid_000001"], 5);
        record_with_key(&root, &a, Some(&mine));
        let b = v4_record_for(&root, &digests, &["vid_000001"], 6);
        record_with_key(&root, &b, Some(&mine));
        crate::audit::append_chained_jsonl_keyed(
            &root.join(super::SIGNOFF_LOG_REL_PATH),
            &serde_json::json!({"event": super::SIGNOFF_ABANDONED_EVENT,
                "signoff_record_sha256": crate::sha256::digest_bytes(b.as_bytes())})
            .to_string(),
            Some(&mine),
        )
        .unwrap();
        match super::signoff_audit_record_with_keys(&root, &b, true, keys) {
            SignoffAuditRecord::NotVerifiable(why) => assert!(why.contains("포기"), "{why}"),
            other => panic!("an abandoned record must not verify: {other:?}"),
        }
        // The previous (completed) sign-off is still the current one.
        assert!(matches!(
            super::signoff_audit_record_with_keys(&root, &a, true, keys),
            SignoffAuditRecord::Verified {
                authenticated: true,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R14 I2 (Medium, the R13 reviewer's PoC): two anomaly runs
    /// interleaved — A's finding, B's finding, A's marker, B's marker —
    /// and B's marker deleted (the last line; no key). `flushUnmarked`
    /// pushed B BEFORE A, so B was not "newer" and A stood as the current
    /// result with B's finding in the history. P20: an unmarked run whose
    /// first row is after the result run's first row, or whose
    /// `scanned_unix` is greater, is newer.
    #[test]
    fn r14_i2_interleaved_anomaly_run_without_its_marker_withholds() {
        let anomaly = [
            r#"{"schema_version":1,"event":"anomaly-scan","scanned_unix":100,"kind":"gap","selector":"vid_a","detail":"a"}"#,
            r#"{"schema_version":1,"event":"anomaly-scan","scanned_unix":200,"kind":"gap","selector":"vid_b","detail":"b"}"#,
            r#"{"schema_version":1,"event":"anomaly-scan-run","scanned_unix":100,"hash_mode":"stored","finding_count":1}"#,
        ]
        .join("\n");
        let elements = rendered_elements(&[("anomaly", &anomaly)]);
        let current = elements["anomaly-findings"]["innerHTML"].as_str().unwrap();
        assert!(current.contains("현재 결과 없음(결과 아님)"), "{current}");
        assert!(current.contains("표식 없는 더 새 실행 있음"), "{current}");
        assert!(current.contains("vid_b"), "{current}");
        assert!(!current.contains("현재 결과 —"), "{current}");
        // An older crash (first entry and scan time before the completed
        // run) still does not withhold.
        let anomaly = [
            r#"{"schema_version":1,"event":"anomaly-scan","scanned_unix":50,"kind":"gap","selector":"vid_old","detail":"o"}"#,
            r#"{"schema_version":1,"event":"anomaly-scan","scanned_unix":100,"kind":"gap","selector":"vid_a","detail":"a"}"#,
            r#"{"schema_version":1,"event":"anomaly-scan-run","scanned_unix":100,"hash_mode":"stored","finding_count":1}"#,
        ]
        .join("\n");
        let elements = rendered_elements(&[("anomaly", &anomaly)]);
        let current = elements["anomaly-findings"]["innerHTML"].as_str().unwrap();
        assert!(
            current.contains("현재 결과 —") && current.contains("vid_a"),
            "{current}"
        );
    }

    /// R14 I4: a lane written by a marker-writing scanner (entries carry
    /// `schema_version`) with NO completed run — every marker may have been
    /// deleted — has no current result; R13 showed the last unmarked run as
    /// "현재 결과". A legacy log without markers or schema still groups by
    /// time (`legacy_anomaly_log_without_run_markers_groups_by_scan_time`).
    #[test]
    fn r14_i4_marked_lane_without_a_completed_run_is_no_result() {
        let anomaly = [
            r#"{"schema_version":1,"event":"anomaly-scan","scanned_unix":100,"kind":"gap","selector":"vid_a","detail":"a"}"#,
            r#"{"schema_version":1,"event":"anomaly-scan","scanned_unix":300,"kind":"gap","selector":"vid_b","detail":"b"}"#,
        ]
        .join("\n");
        let elements = rendered_elements(&[("anomaly", &anomaly)]);
        let current = elements["anomaly-findings"]["innerHTML"].as_str().unwrap();
        assert!(current.contains("현재 결과 없음(결과 아님)"), "{current}");
        assert!(current.contains("완료된 실행이 없고"), "{current}");
        assert!(
            current.contains("vid_a") && current.contains("vid_b"),
            "{current}"
        );
        assert!(!current.contains("현재 결과 —"), "{current}");
    }
}
