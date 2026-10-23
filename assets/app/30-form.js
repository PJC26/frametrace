async function loadEnv(refresh) {
  try {
    const env = await jget(refresh ? "/api/env?refresh=1" : "/api/env");
    const t = env.tools || {};
    const wf = env.workflows || {};
    const hints = env.hints || {};
    // Rows of the '도구 N/5' popover; a missing tool also shows its hint
    // inline (a tooltip alone is unreachable by keyboard).
    const badge = (label, ok, hint, id) =>
      `<span class="badge ${ok ? "ok" : "bad"}"${id ? ` id="${id}"` : ""} title="${esc(hint || "")}">${esc(label)} ${ok ? "준비됨" : "없음"}` +
      (!ok && hint ? ` — ${esc(hint)}` : "") + `</span>`;
    const prevAudit = $("auditBadge");
    const auditHtml = prevAudit && prevAudit.className && prevAudit.className !== "badge"
      ? `<span class="${esc(prevAudit.className)}" id="auditBadge" title="${esc(prevAudit.title || "")}">${esc(prevAudit.textContent)}</span>`
      : `<span class="badge" id="auditBadge" title="감사 로그 무결성은 분석 완료 후 자동 검증됩니다">감사 로그 대기</span>`;
    $("hEnv").innerHTML =
      badge("ffmpeg", t.ffmpeg, hints.media) +
      badge("ffprobe", t.ffprobe, hints.media) +
      badge("libewf(E01)", wf.e01, hints.e01, "ewfBadge") +
      badge("TSK(파일시스템)", wf.filesystem, hints.filesystem) +
      auditHtml;
    envState = { ffmpeg: !!t.ffmpeg, ffprobe: !!t.ffprobe, e01: !!wf.e01, filesystem: !!wf.filesystem };
    syncToolsBtn();
    ffprobeMissing = !t.ffprobe;
    if (!t.ffprobe) { $("withFfprobe").checked = false; syncScanMode(); }
    // The sound option depends on ffprobe (and ffmpeg to decode): re-sync it
    // now that the tools are known — it may have been enabled before /api/env
    // answered, and unchecking ffprobe programmatically fires no change event.
    if (typeof syncAudioOpt === "function") syncAudioOpt();
    // E01 is auto-detected from the path; a missing libewf is explained
    // next to the detected-kind chip instead of greying a radio.
    syncE01Need();
    // deepfake-lens is optional — grey the controls rather than letting a
    // first-time user click into a per-file "tool not found" storm.
    const dfOk = env.deepfake === true;
    setEnvOff($("withDeepfake"), !dfOk);
    $("withDeepfake").closest("label")?.setAttribute("title",
      dfOk ? "" : "deepfake-lens 미설치 — pip install deepfake-lens 또는 FRAMETRACE_DEEPFAKE_LENS 경로 설정");
    const dfBtn = document.querySelector("button[data-adv='deepfake']");
    if (dfBtn) {
      setEnvOff(dfBtn, !dfOk);
      dfBtn.setAttribute("title",
        dfOk ? "케이스의 모든 파일을 deepfake-lens로 스크리닝 — 결과는 '합성의심' 배지로 표시" : "deepfake-lens 미설치 — pip install deepfake-lens 또는 FRAMETRACE_DEEPFAKE_LENS 경로 설정");
    }
  } catch (e) { console.error("loadEnv failed:", e); }
}

// '도구 N/5' in the top strip: ffmpeg, ffprobe, libewf, TSK and the
// audit log. A missing tool (or a failed audit check) turns it amber with
// the count; a pending audit check is not a failure.
let envState = null;
function syncToolsBtn() {
  const btn = $("hBtnTools"), count = $("hToolsCount");
  if (!btn || !count) return;
  if (!envState) { count.textContent = "확인 중"; return; }
  const audit = $("auditBadge");
  const auditBad = !!(audit && /\bbad\b/.test(audit.className || ""));
  const missing = ["ffmpeg", "ffprobe", "e01", "filesystem"].filter(k => !envState[k]).length + (auditBad ? 1 : 0);
  count.textContent = `${5 - missing}/5`;
  btn.classList.toggle("warn", missing > 0);
  btn.setAttribute("aria-label", `도구 ${5 - missing}/5 — ` + (missing > 0 ? `${missing}개 준비 안 됨` : "모두 준비됨"));
  btn.title = missing > 0 ? `외부 도구 · 감사 로그: ${missing}개 준비 안 됨 — 눌러서 확인` : "외부 도구 · 감사 로그 상태 — 모두 준비됨";
}

// ---- UX-11: analysis mode presets + size/placement hints ----
let ffprobeMissing = false;
// Auto-detected evidence kind of the source path (55-home.js): "" (unknown),
// folder | e01 | other | missing.
let detectedKind = "";
const SCAN_MODES = {
  quick: { hash: false, ffprobe: false },
  standard: { hash: false, ffprobe: true },
  full: { hash: true, ffprobe: true },
};
function scanMode() {
  return document.querySelector("input[name='scanMode']:checked")?.value || "custom";
}
function setScanModeRadio(mode) {
  const radio = document.querySelector(`input[name='scanMode'][value='${mode}']`);
  if (radio) radio.checked = true;
}
function applyScanMode(mode) {
  const m = SCAN_MODES[mode];
  if (m) {
    $("withHash").checked = m.hash;
    // ffprobe stays off when the tool is missing, whatever the preset says.
    $("withFfprobe").checked = m.ffprobe && !ffprobeMissing;
  }
  syncScanMode();
}
// Checkbox state is the source of truth; the radio reflects it.
function syncScanMode() {
  const h = $("withHash").checked, f = $("withFfprobe").checked;
  let mode = "custom";
  for (const [k, m] of Object.entries(SCAN_MODES)) if (m.hash === h && m.ffprobe === f) mode = k;
  setScanModeRadio(mode);
  updateModeInfo();
}
// Whole drives, device roots and forensic images are routinely hundreds of
// GB to several TB. The server reports no source size, so this is a
// path-shape heuristic, worded as such.
function looksLargeSource(src) {
  const v = String(src || "").trim();
  if (!v) return false;
  if (inputKind() === "e01") return true;
  return /^[A-Za-z]:[\\/]?$/.test(v) || /^\\\\[?.]\\/.test(v) || v === "/" ||
    /^\/(dev|media|mnt|Volumes|run\/media\/[^/]+)\/[^/]+\/?$/.test(v);
}
// R19 (track S1): the server writes a drive root as `E:\.` — read as `E:`.
function normPath(p) {
  return String(p || "").trim().replace(/\\/g, "/").replace(/\/+$/, "").replace(/^([a-z]:)\/\.$/i, "$1").toLowerCase();
}
// True when `child` is `parent` or lies beneath it (case-insensitive,
// separator-agnostic — the workstation is primarily Windows).
function pathInside(child, parent) {
  const c = normPath(child), p = normPath(parent);
  // Trailing-slash stripping turns the root "/" into "": treat it as the
  // filesystem root, which contains every absolute path (including itself).
  if (!p && /^\s*[\\/]+\s*$/.test(String(parent || ""))) {
    return /^\s*[\\/]/.test(String(child || ""));
  }
  if (!c || !p) return false;
  if (c === p) return true;
  return c.startsWith(p + "/");
}
function updateModeInfo() {
  const el = $("modeInfo");
  if (!el) return;
  const mode = scanMode();
  const hash = $("withHash").checked;
  const big = looksLargeSource($("sourcePath").value);
  const lines = {
    quick: "빠른 선별: 해시·ffprobe를 생략하고 파일 목록과 기본 메타데이터만 색인합니다 — 대용량 소스의 1차 선별용. 증거 대조용 SHA-256은 색인에 기록되지 않습니다.",
    standard: "표준: ffprobe로 코덱·길이를 검증하고 SHA-256 해시는 생략합니다.",
    full: "전체 (증거용): 모든 파일의 SHA-256을 계산하므로 소스의 모든 바이트를 읽습니다 — 소요 시간은 소스 크기에 비례합니다 (1 TB 기준 대략 1.5–3시간, 디스크 속도에 따라 다름).",
    custom: "사용자 지정: 아래 체크박스 설정 그대로 실행합니다.",
  };
  let text = lines[mode] || lines.custom;
  let warn = false;
  if (big && hash) {
    warn = true;
    text = "⚠ 드라이브 전체 또는 디스크 이미지로 보입니다 — 수백 GB~수 TB일 수 있고, SHA-256 해시 계산이 켜져 있어 수 시간이 걸릴 수 있습니다. " +
      "소스 크기는 서버가 제공하지 않아 정확한 예상 시간은 표시할 수 없습니다. 먼저 '빠른 선별'로 훑어본 뒤 필요한 범위만 '전체'로 분석하는 것을 권장합니다. · " + text;
  }
  el.className = "modeinfo" + (warn ? " warn" : "");
  el.textContent = text;
  updateOptSummary();
  // The preflight's hash-cost line depends on the SHA-256 checkbox.
  renderPreflightBox();
}

function inputKind() {
  return document.querySelector("input[name='inputKind']:checked")?.value || "folder";
}

// Sets the (hidden) evidence-kind radios from auto-detection.
function setInputKind(kind) {
  const radio = document.querySelector(`input[name='inputKind'][value='${kind === "e01" ? "e01" : "folder"}']`);
  if (radio && !radio.checked) { radio.checked = true; syncInputKindView(); }
}
function syncInputKindView() {
  const e01 = inputKind() === "e01";
  $("verifyRow").style.display = e01 ? "flex" : "none";
  syncE01Need();
  updateModeInfo();
}
function syncInputKind() {
  syncInputKindView();
  validatePaths();
}
// "libewf 미설치" shows only when the detected input is an E01 image.
function syncE01Need() {
  const el = $("e01Need");
  if (!el) return;
  el.hidden = !(envState && !envState.e01 && detectedKind === "e01");
}

function syncDirectE01() {
  const direct = $("directE01").checked;
  $("skipE01Verify").disabled = direct;
  if (direct) $("skipE01Verify").checked = false;
}

