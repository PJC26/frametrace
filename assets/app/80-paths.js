loadEnv();
document.querySelectorAll("input[name='inputKind']").forEach(radio => radio.addEventListener("change", syncInputKind));

// Auto-detect forensic image extensions in the source path: pasting an
// E01/Ex01/L01/S01 path while "증거 폴더" is selected would otherwise send the
// file to the folder pipeline and fail at the scan step.
// The server's /api/browse "kind" (folder / e01 by extension or EWF
// header / other / missing) settles it in validatePaths; the extension
// check here only makes the chip react instantly while typing.
$("sourcePath").addEventListener("input", () => {
  const v = $("sourcePath").value.trim().toLowerCase();
  const looksImage = /\.(e01|ex01|l01|s01|e02|ex02|l02|s02)$/.test(v);
  const e01Radio = document.querySelector("input[name='inputKind'][value='e01']");
  if (looksImage && e01Radio && inputKind() !== "e01") {
    e01Radio.checked = true;
    syncInputKindView();
  }
  detectedKind = "";
  $("startErr").classList.remove("show");
  updateModeInfo();
  scheduleValidate();
});
$("caseDir").addEventListener("input", scheduleValidate);

// Enter in a path field starts analysis, matching the single-action form.
for (const id of ["sourcePath", "caseDir"]) {
  $(id).addEventListener("keydown", e => {
    if (e.key === "Enter" && !hImeComposing(e) && !$("btnStart").disabled) $("btnStart").click();
  });
}

// ---- 편의 기능: 경로 선택기 · 경로 검증 · 최근 경로 · 옵션 기억 ----

const LS_KEY = "ft.examiner.prefs";
function loadPrefs() {
  try { return JSON.parse(localStorage.getItem(LS_KEY) || "{}") || {}; } catch { return {}; }
}
function savePrefs(patch) {
  const prefs = loadPrefs();
  Object.assign(prefs, patch);
  try { localStorage.setItem(LS_KEY, JSON.stringify(prefs)); } catch { /* storage may be full/blocked */ }
}
function pushRecent(key, value) {
  if (!value) return;
  const prefs = loadPrefs();
  prefs[key] = [value, ...(prefs[key] || []).filter(v => v !== value)].slice(0, 8);
  try { localStorage.setItem(LS_KEY, JSON.stringify(prefs)); } catch { /* ignore */ }
  renderRecent();
}
function renderRecent() {
  const prefs = loadPrefs();
  const fill = (id, items) => {
    $(id).innerHTML = (items || []).map(v => `<option value="${esc(v)}"></option>`).join("");
  };
  fill("recentSources", prefs.recentSources);
  fill("recentCases", prefs.recentCases);
  renderRecentCases();
  renderEvRecent();
}

function setStatus(el, cls, msg) {
  el.className = "pathstatus" + (cls ? " " + cls : "");
  el.textContent = msg;
}

let validateTimer = 0;
function scheduleValidate() {
  clearTimeout(validateTimer);
  validateTimer = setTimeout(validatePaths, 250);
}

// Typed paths are checked against the server filesystem so a mistyped
// drive letter fails before the analysis does, not three steps in.
async function validatePaths() {
  const src = $("sourcePath").value.trim();
  // Case folder may have been filled programmatically (자동 제안, 처음부터).
  updateOptSummary();
  if (!src) {
    applyDetection("", { kind: "" });
    schedulePreflight();
  } else {
    // Auto-detect the evidence kind (chip + inline error), then preflight
    // only a usable path.
    const det = await detectSourceKind(src);
    if ($("sourcePath").value.trim() !== src) return; // stale reply
    applyDetection(src, det);
    schedulePreflight();
  }
  const dir = $("caseDir").value.trim();
  const caseEl = $("caseStatus");
  if (!dir) {
    setStatus(caseEl, "", "");
    renderResumeBox(null);
  } else {
    try {
      const res = await jget("/api/browse?path=" + encodeURIComponent(dir));
      if ($("caseDir").value.trim() !== dir) return;
      if (!res.is_case) renderResumeBox(null);
      const underSource = src && pathInside(dir, src);
      if (res.is_file) setStatus(caseEl, "err", "파일 경로입니다 — 폴더를 지정하십시오.");
      else if (underSource) setStatus(caseEl, "err", "케이스 폴더가 증거 소스 안에 있습니다 — 원본 매체에 산출물이 기록됩니다. 다른 드라이브의 폴더를 지정하십시오.");
      else if (res.exists === false) setStatus(caseEl, "", "새 케이스 폴더로 생성됩니다. (모드: 새 분석)");
      else if (res.is_case) {
        // UX-10: surface what the server will do with an existing case.
        const info = await caseResumeInfo(dir);
        if ($("caseDir").value.trim() !== dir) return;
        renderResumeBox(info);
        if (info.checkpoint) setStatus(caseEl, "warn",
          "중단된 스캔 체크포인트가 있습니다 — 같은 소스·같은 옵션으로 '분석 시작'하면 완료된 파일은 건너뛰고 이어서 진행합니다 (모드: 이어서 분석). " +
          "옵션이나 소스가 다르면 처음부터 다시 스캔합니다. 완전히 새로 시작하려면 다른(빈) 케이스 폴더를 지정하십시오.");
        else if (info.review) setStatus(caseEl, "ok", "기존 케이스입니다 (검토 결과 있음) — '기존 케이스 열기'로 바로 검토하거나, '분석 시작'으로 같은 케이스에 다시 분석합니다.");
        else setStatus(caseEl, "warn", "기존 케이스이지만 검토 결과가 없습니다 — 이전 분석이 끝나지 않았을 수 있습니다. '분석 시작'으로 이어서 진행하십시오.");
      }
      else setStatus(caseEl, "ok", "기존 폴더입니다 — 이 폴더 안에 케이스가 새로 만들어집니다.");
    } catch { setStatus(caseEl, "", ""); renderResumeBox(null); }
  }
}

// Probes the case folder through /api/browse (the only path API the
// server has): db/scan-progress.jsonl exists only while a scan is
// interrupted (finish() deletes it); review/index.html marks a completed
// review build. UX-10: a server with GET /api/case-info also reports the
// checkpoint's N-of-M ({checkpoint:{done,total}}); older ones fall back
// to the probes (no count).
async function caseResumeInfo(dir) {
  const ci = await fetchCaseInfo(dir);
  if (ci) {
    const cp = ci.checkpoint && typeof ci.checkpoint === "object" ? ci.checkpoint : null;
    const review = typeof ci.has_review === "boolean" ? ci.has_review : (typeof ci.review === "boolean" ? ci.review : null);
    if (cp || review !== null) {
      return {
        checkpoint: !!cp,
        // Number(null) is 0: a null total (no index yet) must stay unknown.
        done: cp && cp.done != null ? Number(cp.done) : NaN,
        total: cp && cp.total != null ? Number(cp.total) : NaN,
        review: review !== null ? review : (await caseResumeProbe(dir)).review,
      };
    }
  }
  return caseResumeProbe(dir);
}
async function caseResumeProbe(dir) {
  const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  const base = dir.replace(/[\\/]+$/, "");
  const probe = async rel => {
    try { const r = await jget("/api/browse?path=" + encodeURIComponent(base + sep + rel.split("/").join(sep))); return !!r.is_file; }
    catch { return false; }
  };
  const [checkpoint, review] = await Promise.all([probe("db/scan-progress.jsonl"), probe("review/index.html")]);
  return { checkpoint, review };
}

// ---- 경로 선택 대화상자 (서버 파일시스템 탐색) ----
let pickerTarget = null;
let pickerMode = "dir";
let pickerCurrent = "";
let pickerParent = "";
let pickerLastFocus = null;

// Modes: "dir" (any folder), "source" (evidence: a folder OR an
// E01-family image file), "case" (a case folder to open; picking one
// runs `onChoose`), "e01" (image files only).
const PICKER_MODES = {
  dir: { title: "폴더 선택", select: "이 폴더 선택", files: false },
  source: { title: "증거 선택 — 폴더 또는 E01 이미지", select: "이 폴더 선택", files: true },
  case: { title: "케이스 폴더 열기", select: "이 케이스 열기", files: false },
  e01: { title: "E01 이미지 선택", select: "", files: true },
};
let pickerOnChoose = null;
function openPicker(targetId, mode, onChoose) {
  const m = PICKER_MODES[mode] || PICKER_MODES.dir;
  pickerTarget = targetId;
  pickerMode = mode;
  pickerOnChoose = onChoose || null;
  pickerLastFocus = document.activeElement;
  $("pickerTitle").textContent = m.title;
  $("pickerSelect").textContent = m.select || "선택";
  $("pickerSelect").style.display = m.select ? "" : "none";
  $("pickerBackdrop").hidden = false;
  pickerPlusOpen(mode);
  // Focus lands in the list (first entry) once it loads, not the path box.
  browseTo(pickerSeed(targetId, mode), true);
}
function closePicker() {
  $("pickerBackdrop").hidden = true;
  pickerTarget = null;
  pickerOnChoose = null;
  if (pickerLastFocus) pickerLastFocus.focus();
}
function choose(path) {
  const input = $(pickerTarget);
  const after = pickerOnChoose;
  pickerRemember(pickerMode, path);
  input.value = path;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  closePicker();
  if (after) after(path);
}
// Picker rows are listbox options with a roving tabindex: one Tab stop
// for the list, Up/Down/Home/End move, Enter/Space activate (open a
// folder or choose an image), Backspace goes up.
function pickerOptions() {
  return [...$("pickerList").querySelectorAll('[role="option"]')].filter(o => !o.hidden);
}
function pickerFocus(el, focus) {
  pickerOptions().forEach(o => { o.tabIndex = -1; o.setAttribute("aria-selected", "false"); });
  if (!el) return;
  el.tabIndex = 0;
  el.setAttribute("aria-selected", "true");
  if (focus !== false) el.focus();
}
function pickerRow(html, act, title) {
  const li = document.createElement("li");
  li.className = "picker-item";
  li.setAttribute("role", "option");
  li.setAttribute("aria-selected", "false");
  li.tabIndex = -1;
  li.innerHTML = html;
  if (title) li.title = title;
  li._act = act;
  li.addEventListener("click", () => { pickerFocus(li, false); act(); });
  return li;
}
$("pickerList").addEventListener("keydown", e => {
  const opts = pickerOptions();
  if (!opts.length) return;
  const idx = opts.indexOf(document.activeElement);
  let next = null;
  if (e.key === "ArrowDown") next = opts[Math.min(opts.length - 1, idx + 1)];
  else if (e.key === "ArrowUp") next = opts[Math.max(0, idx - 1)];
  else if (e.key === "Home") next = opts[0];
  else if (e.key === "End") next = opts[opts.length - 1];
  else if ((e.key === "Enter" || e.key === " ") && idx >= 0 && !hImeComposing(e)) {
    e.preventDefault();
    if (opts[idx]._act) opts[idx]._act();
    return;
  } else if (e.key === "Backspace") {
    e.preventDefault();
    browseTo(pickerParent, true);
    return;
  }
  if (next) { e.preventDefault(); pickerFocus(next); }
});
let browseSeq = 0;
async function browseTo(path, focusList) {
  const seq = ++browseSeq;
  const list = $("pickerList");
  const keepFocus = focusList !== false && (focusList === true || list.contains(document.activeElement));
  $("pickerStatus").textContent = "읽는 중…";
  const files = (PICKER_MODES[pickerMode] || {}).files ? "&files=e01" : "";
  let res;
  try {
    res = await jget("/api/browse?path=" + encodeURIComponent(path) + files);
  } catch (err) {
    if (seq !== browseSeq) return;
    list.innerHTML = "";
    const li = document.createElement("li");
    li.className = "picker-item errrow";
    li.innerHTML = `<span>목록을 읽지 못했습니다: ${esc(err.message || err)}</span>`;
    const retry = document.createElement("button");
    retry.type = "button";
    retry.textContent = "다시 시도";
    retry.addEventListener("click", () => browseTo(path, true));
    li.appendChild(retry);
    list.appendChild(li);
    $("pickerStatus").textContent = "목록을 읽지 못했습니다 — 다시 시도하거나 경로를 직접 입력하십시오.";
    if (keepFocus) retry.focus();
    return;
  }
  if (seq !== browseSeq) return; // a newer navigation won
  if (res.is_file) {
    // Seeded with a file path — show its containing folder instead.
    const parent = (res.path || "").replace(/[\\/][^\\/]+$/, "");
    if (parent && parent !== res.path) { browseTo(parent, focusList); return; }
  }
  pickerCurrent = res.path || "";
  pickerParent = res.parent || "";
  $("pickerPath").value = pickerCurrent;
  $("pickerSelect").disabled = !pickerCurrent;
  list.innerHTML = "";
  // A typed or remembered folder that no longer exists opens its nearest
  // existing ancestor instead of a dead end.
  const up = res.ok && res.exists === false ? parentPath(path) : "";
  if (up && up !== path) { browseTo(up, focusList); return; }
  if (!res.ok || res.exists === false) {
    const li = document.createElement("li");
    li.className = "picker-item errrow";
    li.innerHTML = `<span>${esc(res.error || "경로를 찾을 수 없습니다")}</span>`;
    const up = document.createElement("button");
    up.type = "button";
    up.textContent = "상위 폴더로";
    up.addEventListener("click", () => browseTo(pickerParent, true));
    const retry = document.createElement("button");
    retry.type = "button";
    retry.textContent = "다시 시도";
    retry.addEventListener("click", () => browseTo(path, true));
    li.append(up, retry);
    list.appendChild(li);
    $("pickerStatus").textContent = res.error || "경로를 찾을 수 없습니다";
    if (keepFocus) retry.focus();
    return;
  }
  list.appendChild(pickerRow(`<svg class="ic pk-ic" aria-hidden="true"><use href="#i-chevron-up"/></svg><span class="pk-name dir">.. (상위 / 드라이브 목록)</span>`, () => browseTo(pickerParent, true)));
  for (const e of res.entries || []) {
    // In "case" mode a case folder is the answer itself (one Enter opens it).
    const pickCase = pickerMode === "case" && e.is_case;
    const row = pickerRow(pickerEntryHtml(e), () => { if (e.dir && !pickCase) browseTo(e.path, true); else choose(e.path); },
      pickCase ? "Enter: 이 케이스 열기" : e.dir ? "Enter: 폴더 열기" : "Enter: 이 이미지 선택");
    row.dataset.name = String(e.name || "").toLowerCase();
    list.appendChild(row);
  }
  if (!(res.entries || []).length) {
    const li = document.createElement("li");
    li.className = "picker-item muted";
    li.textContent = (PICKER_MODES[pickerMode] || {}).files ? "(하위 폴더나 E01 이미지가 없습니다)" : "(하위 폴더가 없습니다)";
    list.appendChild(li);
  }
  const opts = pickerOptions();
  // First real entry (skip "..") when there is one.
  pickerFocus(opts[1] || opts[0], keepFocus);
  const count = (res.entries || []).length;
  $("pickerStatus").textContent = res.truncated
    ? "항목이 많아 일부만 표시합니다 (500개 제한) — 경로를 직접 입력해도 됩니다"
    : `${pickerCurrent || "드라이브 목록"} — ${count}개 항목`;
  pickerPlusRendered(res);
}

$("btnBrowseSource").addEventListener("click", () => openPicker("sourcePath", "source"));
$("btnBrowseCase").addEventListener("click", () => openPicker("caseDir", "dir"));
$("pickerCancel").addEventListener("click", closePicker);
$("pickerClose").addEventListener("click", closePicker);
$("pickerBackdrop").addEventListener("click", e => { if (e.target === $("pickerBackdrop")) closePicker(); });
document.addEventListener("keydown", e => { if (e.key === "Escape" && !$("pickerBackdrop").hidden) { e.preventDefault(); closePicker(); } });
$("pickerBackdrop").addEventListener("keydown", e => {
  if (e.key !== "Tab") return;
  const focusables = [...$("pickerBackdrop").querySelectorAll("button, input, .picker-item[tabindex='0']")]
    .filter(el => !el.disabled && el.style.display !== "none" && el.offsetParent !== null);
  if (!focusables.length) return;
  const first = focusables[0], last = focusables[focusables.length - 1];
  if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
});
$("pickerSelect").addEventListener("click", () => { if (pickerCurrent) choose(pickerCurrent); });
$("pickerUp").addEventListener("click", () => browseTo(pickerParent, false));
$("pickerPath").addEventListener("keydown", e => { if (e.key === "Enter" && !hImeComposing(e)) browseTo($("pickerPath").value.trim(), true); });

$("btnSuggestCase").addEventListener("click", () => {
  const src = $("sourcePath").value.trim().replace(/[\\/]+$/, "");
  const now = new Date();
  const p2 = n => String(n).padStart(2, "0");
  const stamp = `${now.getFullYear()}${p2(now.getMonth() + 1)}${p2(now.getDate())}-${p2(now.getHours())}${p2(now.getMinutes())}`;
  if (/^[A-Za-z]:$/.test(src)) {
    $("caseDir").value = `${src}\\ftcase-${src[0]}-${stamp}`;
  } else {
    const m = src.match(/^(.*)[\\/]([^\\/]+)$/);
    const parent = m ? m[1] : "";
    let base = m ? m[2] : (src || "case");
    base = base.replace(/\.(e\d+|ex\d+|l\d+|s\d+)$/i, "") || "case";
    const name = `ftcase-${base}-${stamp}`;
    $("caseDir").value = parent ? `${parent.replace(/[\\/]+$/, "")}\\${name}` : `cases\\${name}`;
  }
  validatePaths();
});

