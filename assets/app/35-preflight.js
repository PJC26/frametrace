// ---- UX-11: preflight panel (server-side size / space / placement) ----
// POST /api/preflight {source_path, case_dir} →
//   {sampled_bytes, est_files, free_bytes, case_inside_source, warnings[]}
// Shown under the analysis-mode line before Start. A server without the
// endpoint (404/405) hides the panel for the session — the path-shape
// heuristic in updateModeInfo() remains the fallback.
const PREFLIGHT_DEBOUNCE_MS = 600;
const PREFLIGHT_START_WAIT_MS = 5000;
// SHA-256 over local disks is I/O bound; quoted as a range, not a promise.
const HASH_MBPS_FAST = 300, HASH_MBPS_SLOW = 100;
let preflightTimer = 0, preflightSeq = 0, preflightUnsupported = false;
let preflightData = null; // { key, res }
let preflightInflight = null; // { key, promise }

function preflightKey(src, dir) { return String(src || "").trim() + "\u0000" + String(dir || "").trim(); }

// Pure: preflight result → panel HTML. `opts.hash` = SHA-256 enabled.
function renderPreflight(res, opts) {
  const o = opts || {};
  const rows = [];
  const num = v => (Number.isFinite(Number(v)) && v !== null && v !== "" ? Number(v) : null);
  const bytes = num(res.sampled_bytes), files = num(res.est_files), free = num(res.free_bytes);
  if (res.case_inside_source === true) {
    rows.push({ cls: "err", text: "케이스 폴더가 증거 소스 안에 있습니다 — 원본 매체에 산출물이 기록되고 다음 스캔에서 다시 색인될 수 있습니다. 다른 드라이브를 지정하십시오." });
  }
  if (bytes !== null) {
    rows.push({ cls: "", text: `확인된 소스 크기: ${fmtBytes(bytes)}`, title: "서버가 표본 조사한 값 — 정확한 총량이 아닐 수 있습니다" });
  }
  if (files !== null) {
    rows.push({ cls: "", text: `파일 약 ${files.toLocaleString()}개`, title: "표본 조사로 추정한 파일 수" });
  }
  if (bytes !== null && bytes > 0) {
    if (o.hash) {
      const fast = Math.max(1, Math.round(bytes / (HASH_MBPS_FAST * 1024 * 1024)));
      const slow = Math.max(1, Math.round(bytes / (HASH_MBPS_SLOW * 1024 * 1024)));
      rows.push({ cls: slow >= 3600 ? "warn" : "", text: `SHA-256 해시 예상 소요: ${fmtEta(fast)} – ${fmtEta(slow)}`,
        title: `디스크 ${HASH_MBPS_SLOW}–${HASH_MBPS_FAST} MB/s 가정, 추정치` });
    } else {
      rows.push({ cls: "", text: "SHA-256 해시 생략", title: "원본 바이트 전체를 읽지 않습니다 (증거 대조용 해시는 기록되지 않음)" });
    }
  }
  if (free !== null) {
    const low = free < 2 * 1024 * 1024 * 1024;
    rows.push({ cls: low ? "warn" : "", text: `케이스 드라이브 여유 ${fmtBytes(free)}` + (low ? " — 부족할 수 있습니다" : "") });
  }
  for (const w of Array.isArray(res.warnings) ? res.warnings : []) {
    if (w !== null && w !== undefined && String(w).trim()) rows.push({ cls: "warn", text: String(w) });
  }
  if (!rows.length) rows.push({ cls: "", text: "서버가 사전 점검 정보를 제공하지 않았습니다." });
  // One compact line: "사전 점검  a · b · c" (wraps only when it must).
  return `<span class="pf-k">사전 점검</span>` +
    rows.map(r => `<span class="pf-i${r.cls ? " " + r.cls : ""}"${r.title ? ` title="${esc(r.title)}"` : ""}>${esc(r.text)}</span>`)
      .join(`<span class="pf-sep" aria-hidden="true">·</span>`);
}
function renderPreflightBox() {
  const box = $("preflight");
  if (!box || !preflightData) return;
  if (preflightData.key !== preflightKey($("sourcePath").value, $("caseDir").value)) return;
  box.hidden = false;
  box.innerHTML = renderPreflight(preflightData.res, { hash: $("withHash").checked });
}
function schedulePreflight() {
  clearTimeout(preflightTimer);
  if (preflightUnsupported) return;
  preflightTimer = setTimeout(() => { runPreflight(); }, PREFLIGHT_DEBOUNCE_MS);
}
// Resolves to the preflight result for the current paths, or null
// (unsupported / failed / no source).
function runPreflight() {
  const src = $("sourcePath").value.trim(), dir = $("caseDir").value.trim();
  const box = $("preflight");
  const key = preflightKey(src, dir);
  // A path already known to be missing / unsupported gets the inline
  // error instead of a preflight line.
  if (preflightUnsupported || !src || detectedKind === "missing" || detectedKind === "other") {
    if (box) box.hidden = true;
    preflightData = null;
    return Promise.resolve(null);
  }
  if (preflightData && preflightData.key === key) { renderPreflightBox(); return Promise.resolve(preflightData.res); }
  if (preflightInflight && preflightInflight.key === key) return preflightInflight.promise;
  const seq = ++preflightSeq;
  if (box) { box.hidden = false; box.textContent = "사전 점검 중…"; }
  const promise = (async () => {
    const res = await jpost("/api/preflight", { source_path: src, case_dir: dir });
    if (seq !== preflightSeq) return res && res.ok ? res : null;
    preflightInflight = null;
    if (endpointMissing(res)) {
      preflightUnsupported = true;
      if (box) box.hidden = true;
      return null;
    }
    if (!res.ok) {
      preflightData = null;
      if (box) { box.hidden = false; box.textContent = "사전 점검 실패: " + (res.error || "알 수 없는 오류") + " — 분석은 시작할 수 있습니다."; }
      return null;
    }
    preflightData = { key, res };
    renderPreflightBox();
    return res;
  })();
  preflightInflight = { key, promise };
  return promise;
}
// Start path: bounded wait so a slow preflight never blocks Start.
function ensurePreflight(src, dir) {
  if (preflightUnsupported || !String(src || "").trim()) return Promise.resolve(null);
  if (preflightData && preflightData.key === preflightKey(src, dir)) return Promise.resolve(preflightData.res);
  clearTimeout(preflightTimer);
  return Promise.race([
    runPreflight(),
    new Promise(resolve => setTimeout(() => resolve(null), PREFLIGHT_START_WAIT_MS)),
  ]);
}

// ---- UX-10: checkpoint resume choice ----
// GET /api/case-info?case_dir=… → {checkpoint:{done,total|null}} when the case
// holds an interrupted scan. 404 → caseResumeInfo() falls back to probing
// db/scan-progress.jsonl through /api/browse (no N/M then).
let caseInfoUnsupported = false;
async function fetchCaseInfo(dir) {
  if (caseInfoUnsupported) return null;
  const res = await jgetSoft("/api/case-info?case_dir=" + encodeURIComponent(dir));
  if (endpointMissing(res)) { caseInfoUnsupported = true; return null; }
  if (!res.ok) return null;
  return res;
}
// Pure: resume info → sentence for the resume box ("" = hide).
function resumeText(info) {
  if (!info || !info.checkpoint) return "";
  const d = Number(info.done), t = Number(info.total);
  if (Number.isFinite(d) && Number.isFinite(t) && t > 0) {
    return `이전 분석 ${d.toLocaleString()}/${t.toLocaleString()} 진행됨 (${Math.min(100, Math.round(d * 100 / t))}%) — 이어서 하거나 새 케이스 폴더에서 처음부터 시작할 수 있습니다.`;
  }
  // checkpoint.total is null until a completed index exists: show the
  // count alone, never a made-up total.
  if (Number.isFinite(d) && d > 0) {
    return `이전 분석 ${d.toLocaleString()}건 진행됨 (전체 건수는 아직 모름) — 이어서 하거나 새 케이스 폴더에서 처음부터 시작할 수 있습니다.`;
  }
  return "중단된 이전 분석의 체크포인트가 있습니다 — 이어서 하거나 새 케이스 폴더에서 처음부터 시작할 수 있습니다.";
}
function renderResumeBox(info) {
  const box = $("resumeBox");
  if (!box) return;
  const text = resumeText(info);
  box.hidden = !text;
  $("resumeText").textContent = text;
}
$("btnResumeContinue").addEventListener("click", () => {
  if (!$("sourcePath").value.trim()) {
    toast("이어서 분석하려면 이전과 같은 증거 소스 경로를 입력하십시오.", "warn");
    $("sourcePath").focus();
    return;
  }
  if (!$("btnStart").disabled) $("btnStart").click();
});
$("btnResumeFresh").addEventListener("click", () => {
  $("btnSuggestCase").click();
  renderResumeBox(null);
  toast("새 케이스 폴더를 제안했습니다 — 기존 케이스와 체크포인트는 그대로 보존됩니다.");
});

