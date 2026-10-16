use crate::util::json_for_script;

// The standalone template with the shared viewer fragment
// (assets/viewer/viewer.body.html) and the icon sprite already inlined by
// build.rs (docs/ui-rebuild-contract.md §1.2).
const VIEWER_TEMPLATE: &str = include_str!(concat!(env!("OUT_DIR"), "/evidence_viewer.html"));
const VIEWER_CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/evidence_viewer.css"));
// build.rs concatenates assets/viewer/*.js into this bundle — the
// deliverable stays a single serverless file while the source is split.
const VIEWER_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/evidence_viewer.js"));

pub fn render_review_html(manifest_json: &str, index_json: &str) -> String {
    let manifest = json_for_script(manifest_json);
    let index = json_for_script(index_json);
    format!(
        r#"<!doctype html>
<html lang="ko">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="icon" href="data:,">
  <script>
    try {{ document.documentElement.dataset.theme = localStorage.getItem("ft-theme") || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"); }} catch (e) {{}}
  </script>
  <title>FrameTrace Review</title>
  <style>
    :root {{
      color-scheme: light;
      font-family: "Segoe UI", Arial, sans-serif;
      background: #f6f7f9;
      color: #1f2933;
    }}
    body {{
      margin: 0;
      background: #f6f7f9;
    }}
    header {{
      background: #ffffff;
      border-bottom: 1px solid #d9dee7;
      padding: 12px 24px;
      position: sticky;
      top: 0;
      z-index: 10;
      display: flex;
      align-items: baseline;
      gap: 14px;
      flex-wrap: wrap;
    }}
    h1 {{
      font-size: 18px;
      margin: 0 0 2px;
      letter-spacing: 0;
    }}
    .subtle {{
      color: #667085;
      font-size: 13px;
    }}
    main {{
      padding: 20px 24px 32px;
    }}
    .metrics {{
      display: grid;
      gap: 12px;
      grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
      margin-bottom: 18px;
    }}
    .metric {{
      background: #ffffff;
      border: 1px solid #d9dee7;
      border-radius: 8px;
      padding: 14px;
    }}
    .metric strong {{
      display: block;
      font-size: 22px;
      margin-top: 4px;
    }}
    .toolbar {{
      display: flex;
      gap: 10px;
      align-items: center;
      margin: 16px 0;
      flex-wrap: wrap;
    }}
    input, select {{
      border: 1px solid #c7ceda;
      border-radius: 6px;
      padding: 9px 10px;
      background: #fff;
      font-size: 14px;
    }}
    input {{
      min-width: min(460px, 100%);
      flex: 1;
    }}
    table {{
      width: 100%;
      border-collapse: collapse;
      background: #fff;
      border: 1px solid #d9dee7;
      border-radius: 8px;
      overflow: hidden;
    }}
    #table-wrap {{
      max-height: calc(100vh - 170px);
      overflow: auto;
    }}
    th, td {{
      padding: 10px 12px;
      border-bottom: 1px solid #ecf0f4;
      text-align: left;
      vertical-align: top;
      font-size: 13px;
    }}
    th {{
      background: #f1f4f8;
      font-weight: 600;
      color: #344054;
      position: sticky;
      top: 0;
    }}
    tr:hover td {{
      background: #fafcff;
    }}
    code {{
      font-family: Consolas, "SFMono-Regular", monospace;
      font-size: 12px;
      word-break: break-all;
    }}
    .badge {{
      border-radius: 999px;
      padding: 3px 8px;
      background: #e9eff6;
      color: #344054;
      white-space: nowrap;
      font-size: 12px;
    }}
    .actions a {{
      color: #155eef;
      text-decoration: none;
      margin-right: 8px;
    }}
    .empty {{
      background: #fff;
      border: 1px solid #d9dee7;
      border-radius: 8px;
      padding: 24px;
    }}
    .warnings {{
      background: #fff7ed;
      border: 1px solid #fed7aa;
      border-radius: 8px;
      color: #7c2d12;
      margin: 0 0 16px;
      padding: 12px 14px;
      font-size: 13px;
    }}
    .table-status {{
      color: #475467;
      font-size: 13px;
      margin: 0 0 8px;
    }}
    .pager {{
      display: flex;
      gap: 8px;
      align-items: center;
      justify-content: flex-end;
      margin-top: 10px;
    }}
    button {{
      border: 1px solid #c7ceda;
      border-radius: 6px;
      background: #ffffff;
      color: #1f2933;
      padding: 7px 10px;
      font-size: 13px;
    }}
    button:disabled {{
      color: #98a2b3;
      background: #f2f4f7;
    }}
    #btnTheme {{ margin-left: auto; }}
    html[data-theme="dark"] {{
      color-scheme: dark; background: #101514; color: #dfe8e4;
    }}
    html[data-theme="dark"] :root, html[data-theme="dark"] body {{ background: #101514; }}
    html[data-theme="dark"] header {{ background: #1a211e; border-bottom-color: #2c3a34; }}
    html[data-theme="dark"] .subtle {{ color: #8fa79e; }}
    html[data-theme="dark"] .metric {{ background: #1a211e; border-color: #2c3a34; }}
    html[data-theme="dark"] input, html[data-theme="dark"] select {{ background: #161d1a; border-color: #3a4f48; color: #dfe8e4; }}
    html[data-theme="dark"] table {{ background: #1a211e; border-color: #2c3a34; }}
    html[data-theme="dark"] th, html[data-theme="dark"] td {{ border-bottom-color: #26302c; }}
    html[data-theme="dark"] th {{ background: #232c28; color: #b8ccc4; }}
    html[data-theme="dark"] tr:hover td {{ background: #1d2926; }}
    html[data-theme="dark"] .badge {{ background: #26344a; color: #a8c4e0; }}
    html[data-theme="dark"] .actions a {{ color: #7ab8f5; }}
    html[data-theme="dark"] .empty {{ background: #1a211e; border-color: #2c3a34; }}
    html[data-theme="dark"] .warnings {{ background: #3a2e1a; border-color: #5c4a28; color: #d9b45b; }}
    html[data-theme="dark"] .table-status {{ color: #8fa79e; }}
    html[data-theme="dark"] button {{ background: #232c28; border-color: #3a4f48; color: #dfe8e4; }}
    html[data-theme="dark"] button:disabled {{ color: #5a6b64; background: #1a211e; }}
  </style>
</head>
<body>
  <header>
    <h1 id="title">FrameTrace Review</h1>
    <div class="subtle" id="subtitle"></div>
    <button id="btnTheme" type="button" title="테마 전환">◐ 테마</button>
  </header>
  <main>
    <section class="metrics" aria-label="scan metrics">
      <div class="metric">Indexed videos<strong id="metric-count">0</strong></div>
      <div class="metric">Total bytes<strong id="metric-bytes">0</strong></div>
      <div class="metric">Likely sources<strong id="metric-sources">0</strong></div>
      <div class="metric">Scan warnings<strong id="metric-warnings">0</strong></div>
      <div class="metric">Hash mode<strong id="metric-hash">-</strong></div>
      <div class="metric">ffprobe<strong id="metric-probe">-</strong></div>
    </section>
    <section id="warnings"></section>
    <section class="toolbar" aria-label="filters">
      <input id="query" type="search" placeholder="Search path, codec, extension, source, parser, confidence">
      <select id="source">
        <option value="">All sources</option>
      </select>
      <select id="confidence">
        <option value="">All confidence</option>
      </select>
    </section>
    <div class="table-status" id="result-status"></div>
    <section id="table-wrap"></section>
    <section class="pager" aria-label="pagination">
      <button id="prev-page" type="button">Previous</button>
      <span class="subtle" id="page-status"></span>
      <button id="next-page" type="button">Next</button>
    </section>
  </main>
  <script>
    // R7-S6: an input that was not one JSON value is embedded as null —
    // say so instead of failing on `null.title` (a blank page).
    const manifestInput = {manifest};
    const scanInput = {index};
    const manifest = manifestInput && typeof manifestInput === "object" ? manifestInput : {{}};
    const scan = scanInput && typeof scanInput === "object" ? scanInput : {{}};
    const inputProblems = [];
    if (manifest !== manifestInput) inputProblems.push("케이스 파일(case.json)을 읽지 못했습니다 — 올바른 JSON이 아닙니다. / The case file could not be read.");
    if (scan !== scanInput) inputProblems.push("색인 파일(db/video_index.json)을 읽지 못했습니다 — 올바른 JSON이 아닙니다. / The case index could not be read.");
    const videos = Array.isArray(scan.videos) ? scan.videos : [];
    const warnings = Array.isArray(scan.warnings) ? scan.warnings : [];

    document.getElementById("btnTheme").addEventListener("click", () => {{
      const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
      document.documentElement.dataset.theme = next;
      try {{ localStorage.setItem("ft-theme", next); }} catch (e) {{}}
    }});

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

    const escapeHtml = value => String(value ?? "")
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#39;");

    const confidenceSelect = document.getElementById("confidence");
    [...new Set(videos.map(v => v.confidence).filter(Boolean))].sort().forEach(value => {{
      const option = document.createElement("option");
      option.value = value;
      option.textContent = value;
      confidenceSelect.appendChild(option);
    }});

    const sourceSelect = document.getElementById("source");
    const sourceNames = [...new Set(videos.map(v => v.source_profile?.vendor).filter(Boolean))].sort();
    sourceNames.forEach(value => {{
      const option = document.createElement("option");
      option.value = value;
      option.textContent = value;
      sourceSelect.appendChild(option);
    }});

    document.getElementById("title").textContent = manifest.title || "FrameTrace Review";
    document.getElementById("subtitle").textContent = `${{manifest.case_id || "case"}} · source: ${{scan.source_path || "-"}}`;
    document.getElementById("metric-count").textContent = scan.video_count ?? videos.length;
    document.getElementById("metric-bytes").textContent = fmtBytes(scan.total_bytes ?? 0);
    document.getElementById("metric-sources").textContent = sourceNames.length;
    document.getElementById("metric-warnings").textContent = warnings.length;
    document.getElementById("metric-hash").textContent = scan.options?.hash_files ? "SHA-256" : "Skipped";
    document.getElementById("metric-probe").textContent = scan.options?.use_ffprobe ? "Enabled" : "Skipped";
    const warningSeverity = warning => /failed|unreadable|skipped/i.test(warning) ? "주의" : "정보";
    document.getElementById("warnings").innerHTML = warnings.length
      ? `<div class="warnings"><strong>Scan warnings</strong><table><thead><tr><th>Severity</th><th>Message</th><th>Status</th></tr></thead><tbody>${{warnings.map(warning => `<tr><td>${{warningSeverity(warning)}}</td><td>${{escapeHtml(warning)}}</td><td>Review required</td></tr>`).join("")}}</tbody></table></div>`
      : "";
    if (inputProblems.length) {{
      document.getElementById("warnings").insertAdjacentHTML(
        "afterbegin",
        `<div class="warnings" role="alert" id="input-problems"><strong>케이스 파일을 읽지 못했습니다</strong>${{inputProblems.map(problem => `<p>${{escapeHtml(problem)}}</p>`).join("")}}</div>`
      );
    }}

    let currentPage = 1;
    const pageSize = 100;

    const render = () => {{
      const query = document.getElementById("query").value.trim().toLowerCase();
      const confidence = confidenceSelect.value;
      const source = sourceSelect.value;
      const filtered = videos.filter(video => {{
        if (confidence && video.confidence !== confidence) return false;
        if (source && video.source_profile?.vendor !== source) return false;
        if (!query) return true;
        return [
          video.relative_path,
          video.source_path,
          video.extension,
          video.video_codec,
          video.audio_codec,
          video.confidence,
          video.source_profile?.lane,
          video.source_profile?.vendor,
          video.source_profile?.parser,
          video.source_profile?.confidence,
          video.sha256
        ].some(value => String(value ?? "").toLowerCase().includes(query));
      }});

      const pageCount = Math.max(1, Math.ceil(filtered.length / pageSize));
      currentPage = Math.min(currentPage, pageCount);
      const start = (currentPage - 1) * pageSize;
      const pageRows = filtered.slice(start, start + pageSize);
      document.getElementById("result-status").textContent = `${{filtered.length}} matching videos · showing ${{filtered.length ? start + 1 : 0}}-${{Math.min(start + pageSize, filtered.length)}}`;
      document.getElementById("page-status").textContent = `Page ${{currentPage}} / ${{pageCount}}`;
      document.getElementById("prev-page").disabled = currentPage <= 1;
      document.getElementById("next-page").disabled = currentPage >= pageCount;

      const wrap = document.getElementById("table-wrap");
      if (!filtered.length) {{
        wrap.innerHTML = '<div class="empty">No matching videos.</div>';
        return;
      }}

      wrap.innerHTML = `<table>
        <thead>
          <tr>
            <th>ID</th>
            <th>Path</th>
            <th>Source</th>
            <th>Media</th>
            <th>Size</th>
            <th>Hash</th>
            <th>Review</th>
          </tr>
        </thead>
        <tbody>
          ${{pageRows.map(video => `
            <tr>
              <td><span class="badge">${{escapeHtml(video.id)}}</span></td>
              <td><code>${{escapeHtml(video.relative_path || video.source_path)}}</code><br><span class="subtle">${{escapeHtml(video.confidence)}}</span></td>
              <td>${{escapeHtml(video.source_profile?.vendor || "-")}}<br><span class="subtle">${{escapeHtml(video.source_profile?.parser || "-")}} · ${{escapeHtml(video.source_profile?.confidence || "-")}}</span></td>
              <td>${{escapeHtml(video.video_codec || "-")}} / ${{escapeHtml(video.audio_codec || "-")}}<br><span class="subtle">${{escapeHtml(video.width || "-")}}x${{escapeHtml(video.height || "-")}} · ${{fmtDuration(video.duration_seconds)}}</span></td>
              <td>${{fmtBytes(video.size_bytes)}}</td>
              <td><code>${{escapeHtml(video.sha256 || video.hash_status || "-")}}</code></td>
              <td class="actions"><a href="${{escapeHtml(video.file_url)}}" target="_blank" rel="noreferrer">Source</a></td>
            </tr>
          `).join("")}}
        </tbody>
      </table>`;
    }};

    document.getElementById("query").addEventListener("input", () => {{ currentPage = 1; render(); }});
    sourceSelect.addEventListener("change", () => {{ currentPage = 1; render(); }});
    confidenceSelect.addEventListener("change", () => {{ currentPage = 1; render(); }});
    document.getElementById("prev-page").addEventListener("click", () => {{ currentPage -= 1; render(); }});
    document.getElementById("next-page").addEventListener("click", () => {{ currentPage += 1; render(); }});
    render();
  </script>
</body>
</html>
"#
    )
}

#[allow(clippy::too_many_arguments)] // log feeds arrive as one field per pipeline lane
/// The `window.__FRAMETRACE_DATA__` assignment shared by the inline and
/// external-bundle renderers.
fn evidence_viewer_data(
    manifest_json: &str,
    index_json: &str,
    carve_log_jsonl: &str,
    filesystem_log_jsonl: &str,
    validation_log_jsonl: &str,
    anomaly_log_jsonl: &str,
    fls_entries_jsonl: &str,
    thumbs_json: &str,
    annotations_json: &str,
    deepfake_json: &str,
    telemetry_json: &str,
    proxies_json: &str,
    join_log_jsonl: &str,
    audio_levels_json: &str,
    case_dir: &std::path::Path,
) -> String {
    // Carve/recovery logs may carry relative artifact paths; the viewer
    // absolutizes them against this directory.
    let case_dir_json =
        serde_json::to_string(&crate::audit::path_string(case_dir)).unwrap_or_default();
    format!(
        "window.__FRAMETRACE_DATA__ = {{manifest:{manifest},caseDir:{case_dir},scan:{index},carveLog:{carve_lines},filesystemLog:{filesystem_lines},validationLog:{validation_lines},anomalyLog:{anomaly_lines},flsEntries:{fls_lines},thumbs:{thumbs_lines},annotations:{annotations_lines},deepfake:{deepfake_map},telemetry:{telemetry_map},proxies:{proxies_map},joinLog:{join_lines},audioLevels:{audio_levels}}};",
        case_dir = json_for_script(&case_dir_json),
        manifest = json_for_script(manifest_json),
        index = json_for_script(index_json),
        carve_lines = json_for_script(&jsonl_to_array(carve_log_jsonl)),
        filesystem_lines = json_for_script(&jsonl_to_array(filesystem_log_jsonl)),
        validation_lines = json_for_script(&jsonl_to_array(validation_log_jsonl)),
        anomaly_lines = json_for_script(&jsonl_to_array(anomaly_log_jsonl)),
        fls_lines = json_for_script(&jsonl_to_array(fls_entries_jsonl)),
        // Same script-injection guard as every other embedded JSON: a
        // </script> sequence inside any thumb value must not break out of
        // the data block.
        thumbs_lines = json_for_script(thumbs_json),
        annotations_lines = json_for_script(annotations_json),
        deepfake_map = json_for_script(deepfake_json),
        telemetry_map = json_for_script(telemetry_json),
        proxies_map = json_for_script(proxies_json),
        join_lines = json_for_script(&jsonl_to_array(join_log_jsonl)),
        audio_levels = json_for_script(&crate::report::audio_levels_object(audio_levels_json)),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_evidence_viewer_html(
    manifest_json: &str,
    index_json: &str,
    carve_log_jsonl: &str,
    filesystem_log_jsonl: &str,
    validation_log_jsonl: &str,
    anomaly_log_jsonl: &str,
    fls_entries_jsonl: &str,
    thumbs_json: &str,
    annotations_json: &str,
    deepfake_json: &str,
    telemetry_json: &str,
    proxies_json: &str,
    join_log_jsonl: &str,
    audio_levels_json: &str,
    case_dir: &std::path::Path,
) -> String {
    // The layout/markup lives in assets/evidence_viewer.* and is embedded at
    // compile time, keeping the generated page a single serverless file.
    let data = evidence_viewer_data(
        manifest_json,
        index_json,
        carve_log_jsonl,
        filesystem_log_jsonl,
        validation_log_jsonl,
        anomaly_log_jsonl,
        fls_entries_jsonl,
        thumbs_json,
        annotations_json,
        deepfake_json,
        telemetry_json,
        proxies_json,
        join_log_jsonl,
        audio_levels_json,
        case_dir,
    );
    fill_viewer_template(&data)
}

/// Fills the viewer template's `__CSS__` / `__DATA__` / `__JS__` slots in a
/// single left-to-right pass over the template, so substituted content is
/// never rescanned: case data (or the bundled JS/CSS) that happens to contain
/// one of the tokens is emitted verbatim instead of being rewritten.
fn fill_viewer_template(data: &str) -> String {
    fill_template(
        VIEWER_TEMPLATE,
        &[
            ("__CSS__", VIEWER_CSS),
            ("__DATA__", data),
            ("__JS__", VIEWER_JS),
        ],
    )
}

fn fill_template(template: &str, slots: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(
        template.len() + slots.iter().map(|(_, value)| value.len()).sum::<usize>(),
    );
    let mut rest = template;
    while !rest.is_empty() {
        let next = slots
            .iter()
            .filter_map(|(token, value)| rest.find(token).map(|at| (at, *token, *value)))
            .min_by_key(|(at, _, _)| *at);
        match next {
            Some((at, token, value)) => {
                out.push_str(&rest[..at]);
                out.push_str(value);
                rest = &rest[at + token.len()..];
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// External data bundle written next to the slim viewer page
/// (`review/data-bundle.js`). The viewer loads it through a dynamically
/// inserted <script> tag, which also works under file:// where fetch()
/// is blocked — keeping the standalone deliverable self-sufficient.
#[allow(clippy::too_many_arguments)]
pub fn render_data_bundle_js(
    manifest_json: &str,
    index_json: &str,
    carve_log_jsonl: &str,
    filesystem_log_jsonl: &str,
    validation_log_jsonl: &str,
    anomaly_log_jsonl: &str,
    fls_entries_jsonl: &str,
    thumbs_json: &str,
    annotations_json: &str,
    deepfake_json: &str,
    telemetry_json: &str,
    proxies_json: &str,
    join_log_jsonl: &str,
    audio_levels_json: &str,
    case_dir: &std::path::Path,
) -> String {
    evidence_viewer_data(
        manifest_json,
        index_json,
        carve_log_jsonl,
        filesystem_log_jsonl,
        validation_log_jsonl,
        anomaly_log_jsonl,
        fls_entries_jsonl,
        thumbs_json,
        annotations_json,
        deepfake_json,
        telemetry_json,
        proxies_json,
        join_log_jsonl,
        audio_levels_json,
        case_dir,
    )
}

/// Slim viewer page: `__FRAMETRACE_DATA__` starts null and the viewer
/// boot resolves records via /api/records (workstation mode) or the
/// sibling data-bundle.js (standalone), so a huge index never lands
/// inside the HTML itself.
pub fn render_evidence_viewer_html_slim() -> String {
    fill_viewer_template("window.__FRAMETRACE_DATA__ = null;")
}

/// Standalone carve-results page (`review/carve-report.html`): a raw
/// carve can recover hundreds of artifacts that would drown the evidence
/// grid, so carving gets its own view — sortable/filterable table plus a
/// detail pane that prefers generated proxies for browser-unplayable
/// containers. Works identically under http (workstation /media) and
/// file:// (file:/// URLs).
pub fn render_carve_report_html(
    manifest_json: &str,
    carve_log_jsonl: &str,
    carve_results_json: &str,
    proxies_json: &str,
    case_dir: &std::path::Path,
    index_json: &str,
) -> String {
    let manifest = json_for_script(manifest_json);
    let artifacts = json_for_script(&carve_artifacts_json(case_dir, carve_log_jsonl));
    // Indexed (live-filesystem) sha256 set — a carved artifact whose hash
    // matches is a re-carve of an allocated file; everything else is
    // content that exists only as carved data (deleted / overwritten).
    let index_shas: Vec<String> = serde_json::from_str::<serde_json::Value>(index_json)
        .ok()
        .and_then(|v| v.get("videos").cloned())
        .and_then(|v| serde_json::from_value::<Vec<serde_json::Value>>(v).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.get("sha256").and_then(|s| s.as_str()))
        .map(str::to_string)
        .collect();
    let index_shas_json = serde_json::to_string(&index_shas).unwrap_or_else(|_| "[]".to_string());
    let index_shas_lit = json_for_script(&index_shas_json);
    let results = json_for_script(carve_results_json);
    let proxies = json_for_script(proxies_json);
    // Embedded so detail links stay honest if every absolutization fails.
    let root_json = serde_json::to_string(&crate::audit::path_string(case_dir))
        .unwrap_or_else(|_| "\"\"".to_string());
    let root = json_for_script(&root_json);
    format!(
        r#"<!doctype html>
<html lang="ko">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="icon" href="data:,">
  <script>
    try {{ document.documentElement.dataset.theme = localStorage.getItem("ft-theme") || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"); }} catch (e) {{}}
  </script>
  <title>FrameTrace — 카빙 결과</title>
  <style>
    :root {{ font-family: "Segoe UI", Arial, sans-serif; color: #1f2933; background: #f6f7f9; }}
    body {{ margin: 0; }}
    header {{ background: #fff; border-bottom: 1px solid #d9dee7; padding: 12px 22px; position: sticky; top: 0; z-index: 5; display: flex; flex-wrap: wrap; align-items: center; gap: 6px 16px; }}
    header .metrics {{ margin-top: 0; margin-left: auto; }}
    #btnTheme {{ border: 1px solid #d9dee7; border-radius: 6px; background: #fff; padding: 4px 10px; font-size: 12px; cursor: pointer; }}
    html[data-theme="dark"] :root, html[data-theme="dark"] body {{ background: #101514; color: #dfe8e4; color-scheme: dark; }}
    html[data-theme="dark"] header {{ background: #1a211e; border-bottom-color: #2c3a34; }}
    html[data-theme="dark"] .subtle {{ color: #8fa79e; }}
    html[data-theme="dark"] .metric {{ background: #1a211e; border-color: #2c3a34; }}
    html[data-theme="dark"] .metric.warn {{ border-color: #5c4a28; }}
    html[data-theme="dark"] .metric.fail {{ border-color: #5c3733; }}
    html[data-theme="dark"] .metric.ok {{ border-color: #2a5245; }}
    html[data-theme="dark"] .chip {{ background: #232c28; border-color: #3a4f48; color: #dfe8e4; }}
    html[data-theme="dark"] .chip.on {{ background: #3fa08e; border-color: #3fa08e; color: #0b1411; }}
    html[data-theme="dark"] .search, html[data-theme="dark"] select {{ background: #161d1a; border-color: #3a4f48; color: #dfe8e4; }}
    html[data-theme="dark"] table {{ background: #1a211e; }}
    html[data-theme="dark"] th, html[data-theme="dark"] td {{ border-bottom-color: #26302c; }}
    html[data-theme="dark"] th {{ background: #232c28; }}
    html[data-theme="dark"] tr[data-id]:hover {{ background: #1d2926; }}
    html[data-theme="dark"] tr.sel {{ background: #20333a; }}
    html[data-theme="dark"] .badge {{ background: #26302c; }}
    html[data-theme="dark"] .badge.failed {{ background: #3d2422; }}
    html[data-theme="dark"] .badge.ok {{ background: #16382c; }}
    html[data-theme="dark"] .detail {{ background: #1a211e; border-color: #2c3a34; }}
    html[data-theme="dark"] .kv b {{ color: #8fa79e; }}
    html[data-theme="dark"] a {{ color: #7ab8f5; }}
    html[data-theme="dark"] #btnTheme {{ background: #232c28; border-color: #3a4f48; color: #dfe8e4; }}
    h1 {{ font-size: 18px; margin: 0 0 4px; }}
    .subtle {{ color: #667085; font-size: 12px; }}
    .metrics {{ display: flex; flex-wrap: wrap; gap: 10px; margin-top: 8px; }}
    .metric {{ background: #fff; border: 1px solid #d9dee7; border-radius: 8px; padding: 6px 12px; font-size: 12px; }}
    .metric b {{ font-size: 15px; display: block; }}
    .warn {{ border-color: #f0b429; }}
    .fail {{ border-color: #d64545; }}
    .ok {{ border-color: #2f9e44; }}
    main {{ padding: 14px 22px 40px; }}
    .filters {{ display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 10px; align-items: center; }}
    .chip {{ border: 1px solid #d9dee7; border-radius: 999px; background: #fff; padding: 4px 12px; font-size: 12px; cursor: pointer; }}
    .chip.on {{ background: #1f6feb; color: #fff; border-color: #1f6feb; }}
    .search {{ padding: 4px 10px; border: 1px solid #d9dee7; border-radius: 6px; font-size: 13px; min-width: 220px; }}
    table {{ border-collapse: collapse; width: 100%; background: #fff; font-size: 12px; }}
    th, td {{ border-bottom: 1px solid #e4e7ee; padding: 6px 8px; text-align: left; white-space: nowrap; }}
    th {{ position: sticky; top: 0; background: #eef1f6; cursor: pointer; user-select: none; }}
    tr[data-id] {{ cursor: pointer; }}
    tr[data-id]:hover {{ background: #f0f5ff; }}
    tr.sel {{ background: #dbe7ff; }}
    .badge {{ display: inline-block; padding: 1px 8px; border-radius: 999px; font-size: 11px; background: #e4e7ee; }}
    .badge.failed {{ background: #ffd8d8; }}
    .badge.ok {{ background: #d3f9d8; }}
    .detail {{ margin-top: 14px; background: #fff; border: 1px solid #d9dee7; border-radius: 8px; padding: 14px; }}
    .detail video {{ max-width: 640px; width: 100%; background: #000; }}
    .kv {{ font-size: 12px; margin: 3px 0; word-break: break-all; }}
    .kv b {{ display: inline-block; min-width: 110px; color: #667085; }}
    .muted {{ color: #98a2b3; }}
    .unaudited {{ color: #b54708; font-weight: 600; }}
    html[data-theme="dark"] .unaudited {{ color: #f0b429; }}
    a {{ color: #1f6feb; }}
  </style>
</head>
<body>
<header>
  <h1>카빙 결과 <span class="subtle" id="caseTitle"></span></h1>
  <div class="subtle" id="sourceInfo"></div>
  <div class="metrics" id="metrics"></div>
  <button id="btnTheme" type="button" title="테마 전환 (라이트/다크)">◐ 테마</button>
</header>
<main>
  <div class="filters" id="filters"></div>
  <div class="subtle" id="countLine"></div>
  <table>
    <thead><tr>
      <th data-k="run_order">실행</th><th data-k="id">ID</th><th data-k="extension">확장자</th><th data-k="size_bytes">크기</th>
      <th data-k="offset">오프셋</th><th>라이브FS</th><th data-k="validation_status">검증 상태</th>
      <th data-k="trust_ok">신뢰</th><th>SHA-256</th><th>비고</th><th></th>
    </tr></thead>
    <tbody id="rows"></tbody>
  </table>
  <div class="detail" id="detail" hidden></div>
</main>
<script>
// R7-S6: null when case.json was not one JSON value (shown in the title).
const MANIFEST_INPUT = {manifest};
const MANIFEST = MANIFEST_INPUT && typeof MANIFEST_INPUT === "object" ? MANIFEST_INPUT : {{}};
const ARTIFACTS = {artifacts};
const RESULTS = {results};
const PROXIES = {proxies};
const CASE_ROOT = {root};
const INDEXED_SHAS = new Set({index_shas});

const EXT_PREFIX = "\\\\?\\";
const EXT_UNC = "\\\\?\\UNC\\";
const BS = "\\";
const BROWSER_EXTS = new Set(["mp4","m4v","webm","mov","mpg","mpeg","m2v"]);
const UNPLAYABLE_EXTS = new Set(["avi","dav","hik","h264","h265","heic","bin","dat","ps","ts","m2ts"]);

function fileUrl(path) {{
  if (!path) return "";
  let value = String(path);
  if (value.startsWith("file:")) return value;
  if (value.slice(0, 8).toLowerCase() === EXT_UNC.toLowerCase()) value = BS + BS + value.slice(8);
  else if (value.slice(0, 4).toLowerCase() === EXT_PREFIX.toLowerCase()) value = value.slice(4);
  const normalized = value.split(BS).join("/");
  const enc = s => (/^[A-Za-z]:$/.test(s) ? s : encodeURIComponent(s));
  const encoded = normalized.split("/").map(enc).join("/");
  if (normalized.length > 2 && normalized[1] === ":" && normalized[2] === "/") return "file:///" + encoded;
  return "file://" + encoded;
}}
function absPath(path) {{
  if (!path) return "";
  const p = String(path);
  if (/^([A-Za-z]:[\\/]|\\\\|\/|file:)/.test(p)) return p;
  return CASE_ROOT ? CASE_ROOT.replace(/[\\/]+$/, "") + "/" + p.replace(/^[\\/]+/, "") : p;
}}
function mediaHref(path) {{
  if (!path) return "";
  if (location.protocol === "http:" || location.protocol === "https:") return "/media?path=" + encodeURIComponent(absPath(path));
  return fileUrl(absPath(path));
}}
function downloadHref(path) {{
  if (!path) return "";
  if (location.protocol === "http:" || location.protocol === "https:")
    return "/media?path=" + encodeURIComponent(absPath(path)) + "&download=1";
  return fileUrl(absPath(path));
}}
// Mirrors util::sanitize_filename — every non-alphanumeric char
// (including '.') becomes '_' one-for-one, so ft-951sd\...\carve_1.avi maps to
// ft-951sd_..._carve_1_avi_proxy_*; Windows device names get a leading '_'.
const sanitizeSelector = s => {{
  const out = String(s || "").replace(/[^A-Za-z0-9_-]/g, "_") || "clip";
  return /^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i.test(out) ? "_" + out : out;
}};
function proxyFor(a) {{
  // Proxy filenames embed sanitize(selector) where the selector was the
  // carve-log path at build time (often case-relative), while output_path
  // here is absolutized — so match on id, output_path, AND basename.
  const base = String(a.output_path || "").split(/[\\/]/).pop() || "";
  const keys = [a.id, a.output_path, base].filter(Boolean).map(sanitizeSelector);
  return PROXIES.find(p => {{
    const name = String(p).split(/[\\/]/).pop() || "";
    // D5: path-free names are proxy_<sanitized record id>_<8 hex>[_NNN].mp4;
    // names written before D5 embed the sanitized selector.
    return name.endsWith(".mp4") && keys.some(k =>
      new RegExp("^proxy_" + k.replace(/[-]/g, "\\-") + "_[0-9a-f]{{8}}(_[0-9]{{3}})?\\.mp4$").test(name) ||
      name.startsWith(k + "_proxy_") || name.includes(k + "_proxy_"));
  }}) || "";
}}
function unplayable(a) {{
  const ext = String(a.extension || "").toLowerCase();
  if (BROWSER_EXTS.has(ext)) return false;
  if (UNPLAYABLE_EXTS.has(ext)) return true;
  return !BROWSER_EXTS.has(ext) && !!ext;
}}
const fmtSize = n => n >= 1e9 ? (n/1e9).toFixed(2)+" GB" : n >= 1e6 ? (n/1e6).toFixed(1)+" MB" : (n||0)+" B";
const hex = n => "0x" + (n||0).toString(16);
const esc = s => String(s ?? "").replace(/[&<>"']/g, c => ({{"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}})[c]);

// header
document.getElementById("btnTheme").addEventListener("click", () => {{
  const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next;
  try {{ localStorage.setItem("ft-theme", next); }} catch (e) {{}}
}});
document.getElementById("caseTitle").textContent = MANIFEST !== MANIFEST_INPUT
  ? "— 케이스 파일(case.json)을 읽지 못했습니다"
  : (MANIFEST.case_id ? "— " + MANIFEST.case_id : "");
const R = RESULTS || {{}};
// I6: carve time in UTC (the custody statement's basis) plus the viewing
// PC's local time with its UTC offset, never a bare browser-local string.
const fmtCarveTime = unix => {{
  const d = new Date(unix * 1000);
  if (!Number.isFinite(unix) || Number.isNaN(d.getTime())) return "";
  const p = n => String(n).padStart(2, "0");
  const off = -d.getTimezoneOffset();
  const utc = `${{d.getUTCFullYear()}}-${{p(d.getUTCMonth() + 1)}}-${{p(d.getUTCDate())}} ${{p(d.getUTCHours())}}:${{p(d.getUTCMinutes())}}:${{p(d.getUTCSeconds())}} UTC`;
  const local = `${{d.getFullYear()}}-${{p(d.getMonth() + 1)}}-${{p(d.getDate())}} ${{p(d.getHours())}}:${{p(d.getMinutes())}}:${{p(d.getSeconds())}}`;
  return `${{utc}} (현지 ${{local}} UTC${{off < 0 ? "-" : "+"}}${{p(Math.floor(Math.abs(off) / 60))}}:${{p(Math.abs(off) % 60)}})`;
}};
document.getElementById("sourceInfo").textContent =
  (R.source_path || "") + (R.carved_unix ? "  ·  " + fmtCarveTime(R.carved_unix) : "") +
  (R.source_path || R.carved_unix ? "  ·  실행 요약 감사 안 됨 (db/carve_results.json)" : "");
const exts = [...new Set(ARTIFACTS.map(a => a.extension || "?"))].sort();
const warnN = (R.warnings || []).length;
// R10-I1: only entries usable here (trust_ok) are carve results; the rest
// are listed with "인증 안 됨" in the 신뢰 column and never counted.
const trusted = a => a.trust_ok !== false;
const RESULT_ARTIFACTS = ARTIFACTS.filter(trusted);
const unauthN = ARTIFACTS.length - RESULT_ARTIFACTS.length;
const dupN = RESULT_ARTIFACTS.filter(a => INDEXED_SHAS.has(a.sha256)).length;
const newN = RESULT_ARTIFACTS.length - dupN;
// R11-I5: the run summary (scan complete, candidate limit, source size,
// warnings) comes from db/carve_results.json, which is NOT an audit lane:
// nothing authenticates it, so those metrics carry "감사 안 됨" and are
// never shown in the ok colour (a bad value still warns — the safe
// direction).
const UNAUDITED_LABEL = "감사 안 됨";
const unaudited = k => `${{k}} <span class="unaudited" title="db/carve_results.json — 감사 체인 밖 (인증되지 않음)">· ${{UNAUDITED_LABEL}}</span>`;
document.getElementById("metrics").innerHTML = [
  ["아티팩트", RESULT_ARTIFACTS.length, ""],
  ...(unauthN ? [["인증 안 됨(결과 아님)", unauthN, "fail"]] : []),
  ["라이브FS 미존재", newN, newN ? "warn" : ""],
  ["인덱스 동일", dupN, ""],
  [unaudited("스캔 완료"), R.scan_complete ? "전체 완료" : "부분 스캔", R.scan_complete ? "" : "fail"],
  [unaudited("상한 도달"), R.candidate_limit_reached ? "예" : "아니오", R.candidate_limit_reached ? "fail" : ""],
  [unaudited("원본 크기"), fmtSize(R.source_size_bytes), ""],
  [unaudited("경고"), warnN, warnN ? "warn" : ""],
].map(([k,v,c]) => `<div class="metric ${{c}}"><b>${{v}}</b>${{k}}</div>`).join("");

// filters
const state = {{ ext: "", status: "", big: false, newOnly: false, q: "", sort: "offset", asc: true }};
const filtersEl = document.getElementById("filters");
filtersEl.innerHTML =
  `<button class="chip on" data-ext="">전체</button>` +
  exts.map(e => `<button class="chip" data-ext="${{esc(e)}}">${{esc(e.toUpperCase())}}</button>`).join("") +
  `<button class="chip" data-big="1">> 50MB</button>` +
  `<button class="chip" data-new="1">라이브FS 미존재만</button>` +
  `<select id="fStatus"><option value="">상태 전체</option>` +
  [...new Set(ARTIFACTS.map(a => a.validation_status || "?"))].sort()
    .map(s => `<option>${{esc(s)}}</option>`).join("") + `</select>` +
  `<input class="search" id="fQ" placeholder="ID / 경로 / 비고 검색">`;
filtersEl.addEventListener("click", e => {{
  const b = e.target.closest("button"); if (!b) return;
  if (b.dataset.ext !== undefined) {{ state.ext = b.dataset.ext; }}
  if (b.dataset.new) {{ state.newOnly = !state.newOnly; b.classList.toggle("on", state.newOnly); render(); return; }}
  if (b.dataset.big) {{ state.big = !state.big; b.classList.toggle("on", state.big); render(); return; }}
  filtersEl.querySelectorAll("[data-ext]").forEach(x => x.classList.toggle("on", x.dataset.ext === state.ext));
  render();
}});
filtersEl.addEventListener("input", e => {{
  if (e.target.id === "fQ") state.q = e.target.value.toLowerCase();
  if (e.target.id === "fStatus") state.status = e.target.value;
  render();
}});
document.querySelector("thead").addEventListener("click", e => {{
  const k = e.target.dataset?.k; if (!k) return;
  state.asc = state.sort === k ? !state.asc : true; state.sort = k; render();
}});

const tbody = document.getElementById("rows");
function rows() {{
  let list = ARTIFACTS.filter(a =>
    (!state.ext || (a.extension || "?") === state.ext) &&
    (!state.status || (a.validation_status || "?") === state.status) &&
    (!state.big || (a.size_bytes || 0) > 50_000_000) &&
    (!state.newOnly || !INDEXED_SHAS.has(a.sha256)) &&
    (!state.q || [a.id, a.output_path, a.validation_note].join(" ").toLowerCase().includes(state.q)));
  // Runs stay grouped (latest run first); the chosen column sorts within a run.
  list.sort((a, b) => {{
    const run = (a.run_order ?? 0) - (b.run_order ?? 0);
    if (state.sort !== "run_order" && run) return run;
    const x = a[state.sort], y = b[state.sort];
    const c = typeof x === "number" && typeof y === "number" ? x - y : String(x ?? "").localeCompare(String(y ?? ""));
    return state.asc ? c : -c;
  }});
  return list;
}}
function render() {{
  const list = rows();
  document.getElementById("countLine").textContent = `${{list.length}} / ${{ARTIFACTS.length}} 표시`;
  tbody.innerHTML = list.map(a => `<tr data-id="${{esc(a.id)}}">
    <td class="muted">${{esc(a.run_label || "-")}}</td><td>${{esc(a.id)}}</td><td>${{esc(a.extension || "-")}}</td><td>${{fmtSize(a.size_bytes)}}</td>
    <td title="${{a.offset}}">${{hex(a.offset)}}</td>
    <td>${{!trusted(a) ? '<span class="badge">-</span>' : INDEXED_SHAS.has(a.sha256) ? '<span class="badge">중복</span>' : '<span class="badge ok">신규</span>'}}</td>
    <td><span class="badge ${{!trusted(a) ? "" : (a.validation_status||"").includes("failed") ? "failed" : (a.validation_status||"").includes("confirmed") ? "ok" : ""}}">${{esc(a.validation_status || "-")}}</span></td>
    <td>${{trusted(a) ? `<span class="badge ok" title="${{esc(a.trust_label)}}">${{esc(a.trust === "sc" ? "자기 일관성만" : "확인됨")}}</span>` : `<span class="badge failed" title="${{esc(a.trust_label)}}">인증 안 됨</span>`}}</td>
    <td class="muted">${{esc((a.sha256 || "").slice(0, 12))}}</td>
    <td class="muted" title="${{esc(a.validation_note)}}">${{esc((a.validation_note || "").slice(0, 60))}}</td>
    <td><a href="${{esc(downloadHref(a.output_path))}}">저장</a></td>
  </tr>`).join("");
}}
tbody.addEventListener("click", e => {{
  // The 저장 link downloads; it does not select the row.
  if (e.target.closest("a")) return;
  const tr = e.target.closest("tr[data-id]"); if (!tr) return;
  tbody.querySelectorAll("tr.sel").forEach(x => x.classList.remove("sel"));
  tr.classList.add("sel");
  showDetail(ARTIFACTS.find(a => a.id === tr.dataset.id));
}});
function showDetail(a) {{
  const el = document.getElementById("detail");
  if (!a) {{ el.hidden = true; return; }}
  const proxy = proxyFor(a);
  const needs = unplayable(a);
  const src = proxy ? mediaHref(proxy) : needs ? "" : mediaHref(a.output_path);
  el.innerHTML = `
    <div class="kv"><b>ID</b>${{esc(a.id)}}</div>
    <div class="kv"><b>출력 경로</b>${{esc(a.output_path)}}</div>
    <div class="kv"><b>오프셋 / 크기</b>${{hex(a.offset)}} (${{a.offset}}) / ${{fmtSize(a.size_bytes)}}</div>
    <div class="kv"><b>SHA-256</b>${{esc(a.sha256)}}</div>
    <div class="kv"><b>검증 상태</b>${{esc(a.validation_status)}}</div>
    <div class="kv"><b>신뢰</b>${{esc(a.trust_label)}}${{trusted(a) ? "" : " — 이 항목은 카빙 결과로 보지 마십시오"}}</div>
    <div class="kv"><b>검증 비고</b>${{esc(a.validation_note)}}</div>
    ${{proxy ? `<div class="kv"><b>프록시</b>${{esc(proxy)}}</div>` : ""}}
    ${{src
      ? `<video controls preload="metadata" src="${{esc(src)}}"></video>` + (proxy ? `<div class="subtle">프록시 재생 (원본 컨테이너 재생불가)</div>` : "")
      : `<div class="subtle">${{needs ? "브라우저 재생불가 — 프록시가 없습니다. make-review --build-proxies 또는 워크스테이션의 프록시 요청을 사용하십시오." : "재생할 소스가 없습니다."}}</div>`}}
    <div class="kv"><a href="${{esc(downloadHref(a.output_path))}}">원본 저장</a>${{proxy ? ` · <a href="${{esc(downloadHref(proxy))}}">프록시 저장</a>` : ""}}</div>`;
  el.hidden = false;
  el.scrollIntoView({{ block: "nearest" }});
}}
render();
</script>
</body>
</html>"#,
        manifest = manifest,
        artifacts = artifacts,
        results = results,
        proxies = proxies,
        root = root,
        index_shas = index_shas_lit,
    )
}

/// Carve-log JSONL → JSON array literal with `output_path` absolutized.
/// Carve entries may record paths relative to the invocation cwd rather
/// than the case dir, so each candidate root is existence-checked before
/// falling back to `case_dir`-relative resolution.
fn carve_artifacts_json(case_dir: &std::path::Path, jsonl: &str) -> String {
    let mut items: Vec<serde_json::Value> = jsonl
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();
    // Carve ids are run-unique since E4 (`carve_<runid>_<n>`, entries carry
    // `run_id` / `carved_unix`), so every run is shown, grouped by run with
    // the latest run first. Within one run a repeated id (a resumed run
    // re-logging an artifact) keeps its last entry. Entries logged before
    // run ids existed share one "earlier runs" group where the old id space
    // repeated across runs, so there the last entry per id still wins.
    // Lines without an id are run-level events (e.g. carve-resume).
    {
        use std::collections::HashMap;
        let run_of = |item: &serde_json::Value| {
            item.get("run_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let mut last: HashMap<(Option<String>, String), usize> = HashMap::new();
        for (i, item) in items.iter().enumerate() {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                last.insert((run_of(item), id.to_string()), i);
            }
        }
        // R10 P7: the LAST entry per (run, id) decides — whatever its
        // trust, so an unauthenticated newest entry is never rolled back
        // to an older authenticated one (it is shown as "인증 안 됨").
        let keep: std::collections::HashSet<usize> = last.into_values().collect();
        let mut i = 0usize;
        items.retain(|item| {
            let keep_it = item.get("id").is_some() && keep.contains(&i);
            i += 1;
            keep_it
        });
        // R10-I1: every entry carries whether it may be shown as a result
        // (`trust_ok`, from the reader's per-entry `trust`) and the
        // "신뢰" column text; the page counts and lists results from
        // `trust_ok` entries only.
        for item in &mut items {
            let (ok, label) = carve_entry_trust(item);
            item["trust_ok"] = serde_json::Value::Bool(ok);
            item["trust_label"] = serde_json::Value::String(label);
        }
        // Run order: newest carved_unix first (then latest in the log);
        // the legacy group (no run id) last.
        let mut runs: Vec<(Option<String>, u64, usize)> = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let run = run_of(item);
            let at = item
                .get("carved_unix")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            match runs.iter_mut().find(|(known, _, _)| *known == run) {
                Some(entry) => {
                    entry.1 = entry.1.max(at);
                    entry.2 = index;
                }
                None => runs.push((run, at, index)),
            }
        }
        runs.sort_by(|a, b| {
            a.0.is_none()
                .cmp(&b.0.is_none())
                .then(b.1.cmp(&a.1))
                .then(b.2.cmp(&a.2))
        });
        for item in &mut items {
            let run = run_of(item);
            let order = runs
                .iter()
                .position(|(known, _, _)| *known == run)
                .unwrap_or(0);
            let label = match &run {
                Some(run_id) => run_id.clone(),
                None => "이전 실행(실행 id 없음)".to_string(),
            };
            item["run_order"] = serde_json::json!(order);
            item["run_label"] = serde_json::Value::String(label);
        }
        items.sort_by_key(|item| item["run_order"].as_u64().unwrap_or(0));
    }
    for item in &mut items {
        let Some(raw) = item.get("output_path").and_then(|v| v.as_str()) else {
            continue;
        };
        if std::path::Path::new(raw).is_absolute() || raw.starts_with("file:") {
            continue;
        }
        let resolved = crate::util::resolve_case_artifact_path(case_dir, raw);
        item["output_path"] = serde_json::Value::String(crate::audit::path_string(&resolved));
    }
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
}

/// R10-I1: whether a carve-log entry (as `audit::LaneRows::
/// jsonl_with_trust` hands it over, field `trust`) may be shown as a
/// carve RESULT, and its "신뢰" text. `auth` / `sc` are results (`sc` says
/// "자기 일관성만"); `unauth` and `pc:<id>` are listed as "인증 안 됨"
/// (another PC's key named); an entry without `trust` comes from a caller
/// that did not classify it and is shown as before.
fn carve_entry_trust(item: &serde_json::Value) -> (bool, String) {
    let note = item
        .get("trust_note")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match item.get("trust").and_then(serde_json::Value::as_str) {
        None | Some("auth") => (true, "확인됨".to_string()),
        Some("sc") => (true, crate::audit::SELF_CONSISTENT_NOTE.to_string()),
        Some(code) => {
            let base = match code.strip_prefix("pc:") {
                Some(key_id) => format!(
                    "인증 안 됨 — 이 PC에서 인증 불가, 키 \"{key_id}\" 보유 PC에서 확인 (변조 판정 아님)"
                ),
                None => "인증 안 됨".to_string(),
            };
            if note.is_empty() || code.starts_with("pc:") {
                (false, base)
            } else {
                (false, format!("{base} — {note}"))
            }
        }
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
    use super::{
        VIEWER_CSS, VIEWER_JS, VIEWER_TEMPLATE, fill_template, render_carve_report_html,
        render_evidence_viewer_html, render_evidence_viewer_html_slim,
    };
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[test]
    fn template_fill_does_not_rewrite_tokens_inside_substituted_data() {
        let out = fill_template(
            "a __CSS__ b __DATA__ c __JS__ d",
            &[
                ("__CSS__", "css __JS__ __DATA__"),
                ("__DATA__", "{\"x\":\"__JS__ __CSS__ __DATA__\"}"),
                ("__JS__", "js __DATA__"),
            ],
        );
        assert_eq!(
            out,
            "a css __JS__ __DATA__ b {\"x\":\"__JS__ __CSS__ __DATA__\"} c js __DATA__ d"
        );
        assert_eq!(fill_template("no slots", &[("__JS__", "x")]), "no slots");
    }

    #[test]
    fn carve_report_renders_summary_and_absolutizes_relative_paths() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir =
            std::env::temp_dir().join(format!("ft-carve-report-{}-{}", std::process::id(), unique));
        let carved = dir.join("artifacts/carved");
        std::fs::create_dir_all(&carved).unwrap();
        std::fs::write(carved.join("carve_1_00001000.avi"), b"x").unwrap();
        // Relative-to-cwd-style path: <case-name>/artifacts/carved/file,
        // resolved via the case-parent candidate. Windows logs mix `\` and
        // `/`; on Unix `\` is not a separator, so the fixture uses the
        // platform's own separator to exercise the same resolution branch.
        let case_name = dir.file_name().unwrap().to_str().unwrap();
        let rel = if cfg!(windows) {
            format!("{case_name}\\artifacts/carved\\carve_1_00001000.avi")
        } else {
            format!("{case_name}/artifacts/carved/carve_1_00001000.avi")
        };
        let carve_log = format!(
            "{{\"id\":\"carve_1\",\"extension\":\"avi\",\"offset\":4096,\"size_bytes\":1,\"output_path\":{},\"sha256\":\"aa\",\"validation_status\":\"candidate-unvalidated\"}}\n",
            serde_json::to_string(&rel).unwrap()
        );
        let html = render_carve_report_html(
            r#"{"case_id":"FT-C"}"#,
            &carve_log,
            r#"{"scan_complete":true,"artifact_count":1}"#,
            "[]",
            &dir,
            r#"{"videos":[{"sha256":"aa"}]}"#,
        );
        assert!(html.contains("carve_1"));
        assert!(html.contains("카빙 결과"));
        assert!(html.contains("CASE_ROOT"));
        // The relative path was resolved to the absolute case-parent
        // candidate; compare against the platform-resolved PathBuf.
        let expected = dir.parent().unwrap().join(&rel);
        assert!(expected.is_absolute());
        let expected_json = serde_json::to_string(&crate::audit::path_string(&expected)).unwrap();
        assert!(
            html.contains(&format!("\"output_path\":{expected_json}")),
            "expected resolved output_path {expected_json}"
        );
        #[cfg(windows)]
        {
            // Windows: the serialized path carries a drive-qualified prefix.
            let drive = dir
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .unwrap_or("C:")
                .to_string();
            assert!(html.contains(&format!("\"output_path\":\"{}\\\\", drive)));
        }
        assert!(html.contains("carve_1_00001000.avi"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evidence_viewer_includes_filesystem_recovery_records() {
        let manifest = r#"{"case_id":"FT-1","title":"Test"}"#;
        let index = r#"{"videos":[]}"#;
        let filesystem = r#"{"event":"recover-inode","partition_offset":2048,"inode":"1304","output_path":"/case/artifacts/recovered/filesystem/inode_1304.bin","size_bytes":10,"sha256":"abc","validation_status":"candidate-unvalidated"}"#;
        let html = render_evidence_viewer_html(
            manifest,
            index,
            "",
            filesystem,
            "",
            "",
            "",
            "{}",
            "{}",
            "{}",
            "{}",
            "[]",
            "",
            "{}",
            Path::new("C:\\case"),
        );
        assert!(html.contains("recoveredFilesystemLog"));
        assert!(html.contains("tsk/icat"));
        assert!(html.contains("inode_1304.bin"));
        assert!(html.contains("anomalyLog"));
    }

    #[test]
    fn evidence_viewer_embeds_deepfake_map_and_escapes_script_close() {
        let manifest = r#"{"case_id":"FT-2","title":"Dfl"}"#;
        let index = r#"{"videos":[]}"#;
        // The deepfake map is keyed by sanitized artifact ids; a hostile
        // </script> inside a field must not break the data block —
        // json_for_script escapes it to <.
        let deepfake =
            r#"{"vid_1":{"band":"high","score":88,"note":"</script><script>alert(1)</script>"}}"#;
        let html = render_evidence_viewer_html(
            manifest,
            index,
            "",
            "",
            "",
            "",
            "",
            "{}",
            "{}",
            deepfake,
            "{}",
            "[]",
            "",
            "{}",
            Path::new("C:\\case"),
        );
        assert!(html.contains("deepfake:{\"vid_1\""));
        assert!(html.contains("vid_1"));
        assert!(!html.contains("</script><script>alert(1)"));
        assert!(html.contains("\\u003c/script\\u003e"));
        assert_script_blocks_parse_with_node("evidence-viewer-dfl", &html);
    }

    /// Plan §1.9/§2.4: the court viewer data carries `joinLog` (array) and the
    /// compact `audioLevels` map; script-close sequences stay escaped.
    #[test]
    fn evidence_viewer_embeds_join_log_and_audio_levels() {
        let join = r#"{"schema_version":1,"event":"join-video","output_path":"</script>x.mp4","segment_count":2,"entry_sha256":"e"}"#;
        let levels = r#"{"vid_1":{"r":"silent","max":-91.0,"mean":-91.0,"sec":60.0,"t":5}}"#;
        let html = render_evidence_viewer_html(
            r#"{"case_id":"FT-J","title":"j"}"#,
            r#"{"videos":[]}"#,
            "",
            "",
            "",
            "",
            "",
            "{}",
            "{}",
            "{}",
            "{}",
            "[]",
            join,
            levels,
            Path::new("C:\\case"),
        );
        assert!(html.contains("joinLog:[{\"schema_version\":1,\"event\":\"join-video\""));
        assert!(html.contains("audioLevels:{\"vid_1\":{\"r\":\"silent\""));
        assert!(!html.contains("</script>x.mp4"));
        assert_script_blocks_parse_with_node("evidence-viewer-join", &html);
        // Defaults keep a stable shape: empty array / object, never missing.
        let plain = full_viewer_page();
        assert!(plain.contains("joinLog:[],audioLevels:{}"));
        // The slim data bundle uses the same data builder.
        let bundle = super::render_data_bundle_js(
            "{}",
            "{}",
            "",
            "",
            "",
            "",
            "",
            "{}",
            "{}",
            "{}",
            "{}",
            "[]",
            join,
            levels,
            Path::new("C:\\case"),
        );
        assert!(bundle.contains("joinLog:[{") && bundle.contains("audioLevels:{\"vid_1\""));
    }

    /// R7-S6: a `case.json` / index that is not one JSON value is
    /// embedded as `null` (R6-S1); the review page then threw on
    /// `manifest.title` and stayed blank. It now runs and says which file
    /// could not be read. (Runs the page script under node with a stub
    /// DOM; skipped without node, like the syntax checks.)
    #[test]
    fn review_page_says_when_its_inputs_were_unreadable() {
        let html = crate::html_report::render_review_html("{} x", "not json");
        assert!(html.contains("const manifestInput = null;"));
        assert!(html.contains("const scanInput = null;"));
        let Ok(node) = Command::new("node").arg("--version").output() else {
            return;
        };
        if !node.status.success() {
            return;
        }
        let blocks = extract_script_blocks(&html);
        let main = blocks
            .iter()
            .find(|block| block.contains("manifestInput"))
            .expect("main script");
        let stub = r#"
const shown = [];
const el = () => new Proxy(function () {}, {
  get: (_, key) => {
    if (key === "insertAdjacentHTML") return (_where, html) => shown.push(String(html));
    if (key === "value") return "";
    if (key === Symbol.toPrimitive) return () => "";
    if (key === Symbol.iterator) return [][Symbol.iterator];
    return el();
  },
  set: () => true,
  apply: () => el(),
});
globalThis.document = el();
globalThis.localStorage = el();
globalThis.matchMedia = () => ({ matches: false });
process.on("exit", code => { if (code === 0) console.log("SHOWN:" + JSON.stringify(shown)); });
"#;
        let path =
            std::env::temp_dir().join(format!("frametrace-r7-s6-review-{}.js", std::process::id()));
        std::fs::write(&path, format!("{stub}\n{main}")).unwrap();
        let output = Command::new("node").arg(&path).output().unwrap();
        let _ = std::fs::remove_file(&path);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "review page script failed on null inputs:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("case.json"), "{stdout}");
        assert!(stdout.contains("video_index.json"), "{stdout}");
    }

    fn extract_script_blocks(html: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut rest = html;
        while let Some(start) = rest.find("<script>") {
            let after = &rest[start + "<script>".len()..];
            let Some(end) = after.find("</script>") else {
                break;
            };
            blocks.push(after[..end].to_string());
            rest = &after[end..];
        }
        blocks
    }

    fn assert_script_blocks_parse_with_node(page_name: &str, html: &str) {
        let Ok(node_version) = Command::new("node").arg("--version").output() else {
            return; // node is optional locally; CI checks explicitly.
        };
        if !node_version.status.success() {
            return;
        }
        for (index, script) in extract_script_blocks(html).into_iter().enumerate() {
            let path: PathBuf = std::env::temp_dir().join(format!(
                "frametrace-script-check-{}-{}-{}.js",
                page_name,
                index,
                std::process::id()
            ));
            std::fs::write(&path, &script).expect("script temp file should write");
            let output = Command::new("node")
                .arg("--check")
                .arg(&path)
                .output()
                .expect("node should run after a successful --version");
            let _ = std::fs::remove_file(&path);
            assert!(
                output.status.success(),
                "{page_name} script block {index} has a syntax error:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn generated_page_scripts_are_syntactically_valid() {
        let manifest = r#"{"case_id":"FT-1","title":"테스트 케이스"}"#;
        let index = r#"{"videos":[{"id":"vid_000001","source_path":"C:\\case\\a.mp4","file_url":"file:///C:/case/a.mp4","relative_path":"a.mp4","size_bytes":1,"source_profile":{"vendor":"v","parser":"p"},"ffprobe_ok":true}]}"#;
        let carve = r#"{"id":"carve_000001","output_path":"\\\\?\\C:\\case\\carved\\a.mp4","signature":"mp4-ftyp","size_bytes":2,"sha256":"d","validation_status":"candidate-unvalidated"}"#;
        let filesystem = r#"{"event":"recover-inode","partition_offset":2048,"inode":"1304","output_path":"\\\\?\\C:\\case\\inode.bin","size_bytes":10,"sha256":"a","validation_status":"candidate-unvalidated"}"#;
        let validation = r#"{"selector":"vid_000001","target_path":"C:\\case\\a.mp4","validation_status":"ffprobe-video-stream-confirmed"}"#;

        assert_script_blocks_parse_with_node(
            "review",
            &crate::html_report::render_review_html(manifest, index),
        );
        assert_script_blocks_parse_with_node(
            "viewer",
            &render_evidence_viewer_html(
                manifest,
                index,
                carve,
                filesystem,
                validation,
                r#"{"event":"anomaly-scan","kind":"timestamp-gap","selector":"vid_000001","label":"candidate-finding","detail":"gap"}"#,
                "",
                "{}",
                r#"{"marks":[{"id":"vid_000001","status":"reviewed","marked_unix":100,"note":"db memo","examiner":"Alice"}],"tags":[{"id":"vid_000001","tags":["DB태그"]}]}"#,
                "{}",
                "{}",
                "[]",
                "",
                "{}",
                Path::new("C:\\case"),
            ),
        );
        assert_script_blocks_parse_with_node(
            "report",
            &crate::report::render_case_report(&crate::report::ReportInputs {
                manifest_json: manifest,
                index_json: index,
                export_log_jsonl: "",
                proxy_log_jsonl: "",
                thumbnail_log_jsonl: "",
                carve_log_jsonl: carve,
                filesystem_log_jsonl: filesystem,
                validation_log_jsonl: validation,
                anomaly_log_jsonl: "",
                batch_log_jsonl: "",
                join_log_jsonl: "",
                audio_levels_json: "{}",
                scan_runs_json: "[]",
                marks_json: "[]",
                redaction_applied: false,
                review_warnings_json: "",
                read_errors_json: "",
                telemetry_json: "",
            }),
        );
    }

    // ---- shared viewer fragment / standalone parity (docs/ui-rebuild-contract.md §1.2, §8.1)

    fn asset(rel: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn full_viewer_page() -> String {
        render_evidence_viewer_html(
            r#"{"case_id":"FT-PARITY","title":"parity"}"#,
            r#"{"videos":[{"id":"vid_000001","relative_path":"a.mp4","size_bytes":1}]}"#,
            "",
            "",
            "",
            "",
            "",
            "{}",
            "{}",
            "{}",
            "{}",
            "[]",
            "",
            "{}",
            Path::new("/case"),
        )
    }

    /// Every `id="…"` the fragment declares.
    fn fragment_ids(fragment: &str) -> Vec<String> {
        fragment
            .match_indices(" id=\"")
            .map(|(at, m)| {
                let rest = &fragment[at + m.len()..];
                rest[..rest.find('"').unwrap()].to_string()
            })
            .collect()
    }

    #[test]
    fn viewer_fragment_present_in_slim_and_full_pages() {
        let fragment = asset("assets/viewer/viewer.body.html");
        let fragment = fragment.trim_end();
        for (name, page) in [
            ("slim", render_evidence_viewer_html_slim()),
            ("full", full_viewer_page()),
        ] {
            assert_eq!(page.matches(r#"id="ftViewer""#).count(), 1, "{name}");
            assert_eq!(page.matches(r#"data-ft-viewer="2""#).count(), 1, "{name}");
            assert_eq!(page.matches(r#"id="recordGrid""#).count(), 1, "{name}");
            assert!(
                page.contains(fragment),
                "{name}: fragment not inlined verbatim"
            );
            // (the icon sprite's own header comment names its marker, so only
            // the include and the CSS/JS bundle markers are checked here)
            assert!(
                !page.contains("<!-- @include:") && !page.contains("<!-- @bundle:app"),
                "{name}: unreplaced build marker"
            );
            for slot in ["__CSS__", "__DATA__", "__JS__"] {
                assert!(
                    !VIEWER_TEMPLATE.replacen(slot, "", 1).contains(slot),
                    "template must carry {slot} once"
                );
            }
            // Every viewer id stays unique on the page (harnesses and the
            // shell's legacy iframe bridge address them by id).
            let ids = fragment_ids(fragment);
            assert!(ids.len() > 120, "fragment ids: {}", ids.len());
            for id in ids {
                assert_eq!(
                    page.matches(&format!(" id=\"{id}\"")).count(),
                    1,
                    "{name}: id {id} must appear exactly once"
                );
            }
        }
    }

    #[test]
    fn standalone_viewer_template_is_assembled_from_the_shared_sources() {
        // OUT_DIR template == assets/evidence_viewer.html with the fragment
        // and the icon sprite substituted — nothing else may differ, so the
        // exported court-evidence page and the hosted viewer share markup.
        let expected = asset("assets/evidence_viewer.html")
            .replacen(
                "<!-- @include:viewer.body -->",
                asset("assets/viewer/viewer.body.html").trim_end(),
                1,
            )
            .replacen(
                "<!-- @bundle:icons.svg -->",
                asset("assets/icons.svg").trim_end(),
                1,
            );
        assert_eq!(VIEWER_TEMPLATE, expected);
        // The slim page is exactly the template with the three slots filled.
        assert_eq!(
            render_evidence_viewer_html_slim(),
            VIEWER_TEMPLATE
                .replacen("__CSS__", VIEWER_CSS, 1)
                .replacen("__DATA__", "window.__FRAMETRACE_DATA__ = null;", 1)
                .replacen("__JS__", VIEWER_JS, 1)
        );
    }

    #[test]
    fn standalone_viewer_bundles_tokens_kit_and_host_api_once() {
        let page = render_evidence_viewer_html_slim();
        for banner in [
            "/* ==== assets/tokens.css ==== */",
            "/* ==== assets/kit.css ==== */",
            "/* ==== assets/evidence_viewer.css ==== */",
            "/* ==== assets/viewer/controls.css ==== */",
            "/* ==== assets/viewer/00-host.js ==== */",
            "/* ==== assets/viewer/99-loader.js ==== */",
        ] {
            assert_eq!(page.matches(banner).count(), 1, "{banner}");
        }
        // Mark colours follow the shared tokens (contract §5.6): whatever
        // assets/tokens.css defines for --mark-important-bg is in the page.
        let tokens = asset("assets/tokens.css");
        let line = tokens
            .lines()
            .find(|l| l.contains("--mark-important-bg:"))
            .expect("tokens.css defines --mark-important-bg");
        assert!(page.contains(line), "{line}");
        assert!(tokens.len() > 100 && page.contains(tokens.trim_end()));
        // The bundle exposes window.FTViewer and still self-mounts when no
        // host page claims it (packaged file, /viewer/, legacy iframe).
        assert!(VIEWER_JS.contains("window.FTViewer = FTViewer;"));
        assert!(VIEWER_JS.contains(r#"hasAttribute("data-ft-host")"#));
        assert!(VIEWER_JS.contains("async function ftViewerBoot(opts)"));
    }

    #[test]
    fn standalone_viewer_keeps_the_legacy_iframe_contract() {
        // FRAMETRACE_UI=legacy embeds this page in an iframe (?embed=1) and
        // drives it by postMessage + same-origin DOM reads: keep all of it
        // until the legacy shell is deleted (contract §9.3).
        let page = render_evidence_viewer_html_slim();
        for needle in [
            "\"ft-reload\"",
            "\"ft-reload-ack\"",
            "\"ft-embed\"",
            "\"ft-theme\"",
            "\"ft-locale\"",
            "window.__frametraceSummary",
            "window.__ftBridge",
            r#"id="saveState""#,
            r#"id="examinerName""#,
            r#"id="btnExitFs""#,
            r#"has("embed")"#,
        ] {
            assert!(page.contains(needle), "{needle}");
        }
    }

    /// I6: the carve report's run time names its basis (UTC plus the local
    /// offset) instead of a bare browser-local string.
    #[test]
    fn carve_report_time_label_is_utc_with_offset() {
        let html = render_carve_report_html(
            r#"{"case_id":"FT-T"}"#,
            "",
            r#"{"carved_unix":1700000000}"#,
            "[]",
            Path::new("case"),
            "{}",
        );
        assert!(!html.contains("toLocaleString"), "bare local time label");
        let script = html.rsplit_once("<script>").unwrap().1;
        let start = script.find("const fmtCarveTime").unwrap();
        let end = script[start..].find("};").unwrap() + start + 2;
        let output = Command::new("node")
            .arg("-e")
            .arg(format!(
                "{}\nprocess.stdout.write(fmtCarveTime(1700000000));",
                &script[start..end]
            ))
            .output()
            .expect("node is required for report rendering tests");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.starts_with("2023-11-14 22:13:20 UTC (현지 "), "{text}");
        assert!(text.contains(" UTC+") || text.contains(" UTC-"), "{text}");
    }

    /// E4 follow-up: carve ids are run-unique, so every run is shown,
    /// grouped by run with the latest run first; only legacy entries
    /// (no run id, repeating id space) keep the last entry per id.
    #[test]
    fn carve_page_shows_every_run_grouped_latest_first() {
        let log = [
            r#"{"id":"carve_000001","output_path":"/c/a.mp4"}"#,
            r#"{"id":"carve_000001","output_path":"/c/a2.mp4"}"#,
            r#"{"id":"carve_r1_1","run_id":"r1","carved_unix":100,"output_path":"/c/r1.mp4"}"#,
            r#"{"event":"carve-resume","run_id":"r1"}"#,
            r#"{"id":"carve_r2_1","run_id":"r2","carved_unix":200,"output_path":"/c/r2a.mp4"}"#,
            r#"{"id":"carve_r2_2","run_id":"r2","carved_unix":200,"output_path":"/c/r2b.mp4"}"#,
        ]
        .join("\n");
        let items: Vec<serde_json::Value> = serde_json::from_str(&super::carve_artifacts_json(
            std::path::Path::new("/c"),
            &log,
        ))
        .unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            ["carve_r2_1", "carve_r2_2", "carve_r1_1", "carve_000001"]
        );
        assert_eq!(items[0]["run_label"], "r2");
        assert_eq!(items[2]["run_order"], 1);
        assert!(
            items[3]["output_path"]
                .as_str()
                .unwrap()
                .ends_with("a2.mp4"),
            "legacy: last entry per id"
        );
        assert_eq!(items[3]["run_label"], "이전 실행(실행 id 없음)");
    }
}
