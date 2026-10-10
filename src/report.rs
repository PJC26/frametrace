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
    pub scan_runs_json: &'a str,
    pub marks_json: &'a str,
    /// True when the caller already redacted absolute paths in every input;
    /// the report then shows the redaction notice instead of silently
    /// presenting rewritten paths as real ones.
    pub redaction_applied: bool,
}

pub fn render_case_report(inputs: &ReportInputs<'_>) -> String {
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
    let scan_runs = json_for_script(inputs.scan_runs_json);
    let marks = json_for_script(inputs.marks_json);
    let redaction_note = if inputs.redaction_applied {
        format!(
            "  <div class=\"note\">{}</div>\n",
            crate::redact::REDACTION_NOTE
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
  <button onclick="window.print()" type="button">인쇄 / PDF로 저장</button>
</div>
<main>
  <h1 id="title">FrameTrace 영상 포렌식 보고서</h1>
  <div class="muted" id="case-line"></div>
{redaction_note}
  <section class="summary">
    <div class="box">색인 영상<strong id="count">0</strong></div>
    <div class="box">증거 총 용량<strong id="bytes">0</strong></div>
    <div class="box">ffprobe 확인<strong id="confirmed">0</strong></div>
    <div class="box">추정 소스<strong id="sources">0</strong></div>
    <div class="box">스캔 경고<strong id="warnings-count">0</strong></div>
    <div class="box">내보낸 클립<strong id="exports">0</strong></div>
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

  <h2>영상 색인</h2>
  <div id="videos"></div>

  <h2>MP4/AVI 내보내기 산출물</h2>
  <div id="clip-exports"></div>

  <h2>리뷰 산출물</h2>
  <div id="derived-artifacts"></div>

  <h2>복구 / Carving 후보</h2>
  <div id="carved-artifacts"></div>

  <h2>재생 / 컨테이너 검증</h2>
  <div id="validation-results"></div>

  <h2>이상 징후 후보 (candidate-finding)</h2>
  <div id="anomaly-findings"></div>
  <div class="note">자동 플래그는 수동 확인이 필요한 후보 소견입니다. 조작·위변조·법적 증명으로 해석하지 마십시오. <code>qa anomalies</code>로 재생성합니다.</div>

  <h2>파일시스템 조사 / Inode 복구</h2>
  <div id="filesystem-recovery"></div>

  <h2>처리 체인 (분석 타임라인)</h2>
  <div id="chain"></div>
  <div class="note">스캔·검증·내보내기·복구 이벤트를 시간순으로 정렬한 처리 이력입니다. 체인 해시는 감사 로그의 변조 방지 연결 값을 나타냅니다.</div>
<script>
const manifest = {manifest};
const scan = {index};
const exportsLog = {export_lines};
const proxyLog = {proxy_lines};
const thumbnailLog = {thumbnail_lines};
const carveLog = {carve_lines};
const filesystemLog = {filesystem_lines};
const validationLog = {validation_lines};
const anomalyLog = {anomaly_lines};
const batchLog = {batch_lines};
const scanRuns = {scan_runs};
const marks = {marks};
const redaction = {{"applied": {}}};
const videos = Array.isArray(scan.videos) ? scan.videos : [];
const warnings = Array.isArray(scan.warnings) ? scan.warnings : [];
const derivedLog = [...proxyLog, ...thumbnailLog];

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

const fmtUnix = value => {{
  if (!Number.isFinite(value)) return "-";
  return new Date(value * 1000).toLocaleString();
}};

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

document.getElementById("videos").innerHTML = videos.length ? `<table>
  <thead>
    <tr>
      <th>ID</th><th>경로</th><th>소스</th><th>포맷</th><th>길이</th><th>크기</th><th>해시</th>
    </tr>
  </thead>
  <tbody>
    ${{videos.map(video => `<tr>
      <td>${{escapeHtml(video.id)}}</td>
      <td><code>${{escapeHtml(video.relative_path || video.source_path)}}</code></td>
      <td>${{escapeHtml(video.source_profile?.vendor || "-")}}<br><code>${{escapeHtml(video.source_profile?.parser || "-")}}</code></td>
      <td>${{escapeHtml(video.video_codec || video.extension || "-")}} ${{video.width && video.height ? `(${{video.width}}x${{video.height}})` : ""}}</td>
      <td>${{fmtDuration(video.duration_seconds)}}</td>
      <td>${{fmtBytes(video.size_bytes)}}</td>
      <td><code>${{escapeHtml(video.sha256 || video.hash_status || "-")}}</code></td>
    </tr>`).join("")}}
  </tbody>
</table>` : "<p>색인된 영상이 없습니다.</p>";

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
      <td>${{item.start_seconds ?? "-"}}, ${{item.duration_seconds ?? "-"}}</td>
      <td><code>${{escapeHtml(item.entry_sha256 || "-")}}</code></td>
    </tr>`).join("")}}
  </tbody>
</table>` : "<p>내보낸 클립이 없습니다.</p>";

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
      <td><code>${{escapeHtml(item.entry_sha256 || "-")}}</code></td>
    </tr>`).join("")}}
  </tbody>
</table>` : "<p>프록시 또는 썸네일 산출물이 없습니다.</p>";

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
      <td><code>${{escapeHtml(item.entry_sha256 || "-")}}</code></td>
    </tr>`).join("")}}
  </tbody>
</table>` : "<p>Carving 후보가 없습니다.</p>";

document.getElementById("validation-results").innerHTML = validationLog.length ? `<table>
  <thead>
    <tr><th>상태</th><th>대상</th><th>포맷</th><th>코덱</th><th>길이</th><th>SHA-256</th><th>검증 메모</th><th>감사 체인</th></tr>
  </thead>
  <tbody>
    ${{validationLog.map(item => `<tr>
      <td>${{escapeHtml(item.validation_status || "-")}}</td>
      <td><code>${{escapeHtml(item.target_path || item.selector || "-")}}</code></td>
      <td>${{escapeHtml(item.format_name || "-")}}</td>
      <td>${{escapeHtml(item.video_codec || "-")}} / ${{escapeHtml(item.audio_codec || "-")}}</td>
      <td>${{fmtDuration(item.duration_seconds)}}</td>
      <td><code>${{escapeHtml(item.target_sha256 || "-")}}</code></td>
      <td>${{escapeHtml(item.validation_note || item.ffprobe_error || "-")}}</td>
      <td><code>${{escapeHtml(item.entry_sha256 || "-")}}</code></td>
    </tr>`).join("")}}
  </tbody>
</table>` : "<p>검증 기록이 없습니다.</p>";

const anomalyRows = anomalyLog.filter(item => item.kind && item.kind !== "none");
document.getElementById("anomaly-findings").innerHTML = anomalyRows.length ? `<table>
  <thead>
    <tr><th>라벨</th><th>종류</th><th>대상</th><th>상세</th></tr>
  </thead>
  <tbody>
    ${{anomalyRows.map(item => `<tr>
      <td>${{escapeHtml(item.label || "candidate-finding")}}</td>
      <td>${{escapeHtml(item.kind || "-")}}</td>
      <td><code>${{escapeHtml(item.selector || item.source_path || "-")}}</code></td>
      <td>${{escapeHtml(item.detail || "-")}}</td>
    </tr>`).join("")}}
  </tbody>
</table>` : "<p>이상 징후 후보가 없습니다.</p>";

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
      <td><code>${{escapeHtml(item.entry_sha256 || "-")}}</code></td>
    </tr>`).join("")}}
  </tbody>
</table>` : "<p>파일시스템 조사 또는 inode 복구 기록이 없습니다.</p>";

const marksById = new Map((Array.isArray(marks) ? marks : []).map(mark => [mark.id, mark]));
function validationForRow(row) {{
  const byExactPath = validationLog.filter(item => item.target_path === row.where);
  const eligible = byExactPath.filter(item => /^[a-fA-F0-9]{{64}}$/.test(row.hash) &&
    typeof item.target_sha256 === "string" && item.target_sha256.toLowerCase() === row.hash.toLowerCase());
  if (eligible.length) {{
    return eligible.sort((a, b) => (a.validated_unix ?? 0) - (b.validated_unix ?? 0)).at(-1);
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
    meta: [video.video_codec || video.format_name || "-", video.width && video.height ? `${{video.width}}x${{video.height}}` : "", fmtDuration(video.duration_seconds), fmtBytes(video.size_bytes)].filter(Boolean).join(" · "),
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
    <tr><th>ID</th><th>구분</th><th>발견 기법</th><th>파서 평가</th><th>위치 / 근원</th><th>시각</th><th>메타데이터</th><th>SHA-256</th><th>검증 상태 / 근거</th><th>판독</th></tr>
  </thead>
  <tbody>
    ${{techniqueRows().map(row => {{
      const validation = validationForRow(row);
      const status = validation?.validation_status || (row.kind === "원본 (논리 파일)" ? "색인됨 (재생성 검증 전)" : "candidate-unvalidated");
      const reason = validation ? (validation.validation_note || validation.ffprobe_error || "-") : "재생성 검증 대기 — 판독자 재생 확인 필요";
      const mark = marksById.get(row.id);
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
        <td>${{mark ? escapeHtml(markLabel(mark.status)) + (mark.note ? `<br><span class="muted">${{escapeHtml(mark.note)}}</span>` : "") + (mark.examiner ? `<br><span class="muted">검토자: ${{escapeHtml(mark.examiner)}}</span>` : "") : "-"}}</td>
      </tr>`;
    }}).join("")}}
  </tbody>
</table>` : "<p>정리할 증거가 없습니다.</p>";

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
  chain: item.entry_sha256 || "-"
}}));
batchLog.forEach(item => chainEvents.push({{
  ts: null,
  name: item.event || "batch",
  detail: `요청 ${{item.requested ?? "-"}} · 성공 ${{item.ok ?? "-"}} · 실패 ${{item.failed ?? "-"}}`,
  chain: item.entry_sha256 || "-"
}}));
exportsLog.forEach(item => chainEvents.push({{
  ts: item.exported_unix ?? item.created_unix,
  name: "export-video (클립 내보내기)",
  detail: `${{item.selector || item.source_path || "-"}} → ${{item.output_path || "-"}} (${{item.format || "-"}})`,
  chain: item.entry_sha256 || "-"
}}));
derivedLog.forEach(item => chainEvents.push({{
  ts: item.created_unix,
  name: item.kind === "proxy" ? "make-proxy (검토용 프록시)" : "make-thumbnail (썸네일)",
  detail: `${{item.source_path || "-"}} → ${{item.output_path || "-"}}`,
  chain: item.entry_sha256 || "-"
}}));
filesystemLog.forEach(item => chainEvents.push({{
  ts: item.recovered_unix ?? item.inspected_unix,
  name: item.event || "filesystem",
  detail: `inode ${{item.inode ?? "-"}} @ ${{item.partition_offset ?? "-"}} → ${{item.output_path || item.summary_path || `${{item.entry_count ?? "-"}} entries`}}`,
  chain: item.entry_sha256 || "-"
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
      <td><code>${{escapeHtml(event.chain)}}</code></td>
    </tr>`).join("")}}
  </tbody>
</table>` : "<p>처리 이력이 없습니다.</p>";
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

/// Banner naming the reviewer recorded at finalize time. The wording is
/// deliberately "recorded as entered": the workstation has no login, so the
/// name is the examiner's own typed statement, not an authenticated identity.
pub fn signoff_banner_html(examiner: &str, signed_unix: i64) -> String {
    format!(
        "<div style=\"padding:8px 16px;background:#eef4f8;border-bottom:1px solid #b9d0e0;font-size:13px\">검토자: <strong>{}</strong> (최종화 시 입력·기록된 이름이며 신원 인증은 아닙니다; 기록 시각 unix {})</div>",
        crate::util::html_escape(examiner),
        signed_unix
    )
}

/// Inserts the sign-off banner right after `<body>`.
pub fn inject_signoff_banner(html: String, examiner: &str, signed_unix: i64) -> String {
    let banner = signoff_banner_html(examiner, signed_unix);
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

    #[test]
    fn signoff_banner_escapes_and_is_injected_after_body() {
        let html = super::inject_signoff_banner(
            "<html><body><p>x</p></body></html>".to_string(),
            "홍길동 <b>",
            1700000000,
        );
        assert!(html.contains("검토자: <strong>홍길동 &lt;b&gt;</strong>"));
        assert!(html.contains("신원 인증은 아닙니다"));
        assert!(html.find("검토자").unwrap() < html.find("<p>x</p>").unwrap());
        assert_eq!(
            super::parse_signoff(r#"{"examiner":" A ","signed_unix":5}"#),
            Some(("A".to_string(), 5))
        );
        assert_eq!(super::parse_signoff(r#"{"examiner":"  "}"#), None);
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
        let scan_runs = lookup("scan_runs");
        let marks = lookup("marks");
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
            scan_runs_json: &scan_runs,
            marks_json: &marks,
            redaction_applied: inputs
                .iter()
                .any(|(name, value)| *name == "redacted" && *value == "1"),
        })
    }

    fn rendered_techniques(index: &str, validation: &str, carve: &str, filesystem: &str) -> String {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let html = render(&[
            ("manifest", "{}"),
            ("index", index),
            ("validation", validation),
            ("carve", carve),
            ("filesystem", filesystem),
            ("scan_runs", "[]"),
            ("marks", "[]"),
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

    #[test]
    fn redaction_note_renders_only_when_applied() {
        let plain = render(&[]);
        assert!(!plain.contains(crate::redact::REDACTION_NOTE));
        assert!(plain.contains("\"applied\": false"));
        let redacted = render(&[("redacted", "1")]);
        assert!(redacted.contains(crate::redact::REDACTION_NOTE));
        assert!(redacted.contains("\"applied\": true"));
    }
}
