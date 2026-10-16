// ---- empty state: 증거 넣기 · 이전 프로젝트 ----
// One centred column, exactly two blocks. The evidence kind is
// auto-detected from the path (folder → folder pipeline, E01-family image
// → E01 pipeline); previous projects come from GET /api/recent-cases and
// the newest openable one carries the block's single primary,
// [이어서 검토].

function showStartErr(msg) {
  const el = $("startErr");
  el.classList.add("show");
  el.textContent = msg;
}

// ---- evidence kind auto-detection ----
const E01_PATH_RE = /\.(e01|ex01|l01|s01|lx01)$/i;
const KIND_MSG = {
  missing: "경로를 찾을 수 없습니다 — 드라이브 문자와 경로를 확인하십시오.",
  other: "지원하지 않는 입력입니다 — E01 이미지 파일(.E01 · .Ex01)이나 블랙박스 폴더·드라이브를 넣으십시오.",
};
let kindCache = { src: "", det: null };
// Resolves {kind, message}: kind = folder | e01 | missing | other |
// unknown (server unreachable — the extension decides, the server
// validates on start).
async function detectSourceKind(src) {
  const path = String(src || "").trim();
  if (!path) return { kind: "" };
  // Reused briefly (validate → Start), never across a re-typed path.
  if (kindCache.src === path && kindCache.det && Date.now() - kindCache.at < 5000) return kindCache.det;
  let det;
  try {
    const res = await jget("/api/browse?path=" + encodeURIComponent(path));
    let kind = res.kind;
    if (!kind) {
      // Older server without "kind": derive it from the listing shape.
      if (res.exists === false) kind = "missing";
      else if (res.is_file) kind = E01_PATH_RE.test(path) ? "e01" : "other";
      else kind = "folder";
    }
    det = { kind, message: KIND_MSG[kind] || "" };
    kindCache = { src: path, det, at: Date.now() };
  } catch (e) {
    det = { kind: "unknown", guess: E01_PATH_RE.test(path) ? "e01" : "folder", message: "" };
  }
  return det;
}
// Chip + inline status + the hidden kind radios from one detection.
function applyDetection(src, det) {
  const kind = det && det.kind || "";
  detectedKind = kind === "unknown" ? "" : kind;
  const chip = $("kindChip");
  const label = kind === "folder" ? "폴더" : kind === "e01" ? "E01 이미지" : "";
  if (chip) {
    chip.hidden = !label;
    chip.textContent = label;
    chip.className = "kindchip" + (kind === "e01" ? " e01" : "");
    chip.title = label ? `자동 판별: ${label} — ${kind === "e01" ? "E01 이미지 분석 절차로 진행합니다" : "폴더·드라이브 분석 절차로 진행합니다"}` : "";
  }
  const status = $("sourceStatus");
  if (status) {
    if (kind === "missing" || kind === "other") setStatus(status, "err", det.message);
    else setStatus(status, "", "");
  }
  if (kind === "e01" || kind === "folder") setInputKind(kind);
  else if (kind === "unknown") setInputKind(det.guess);
  syncE01Need();
  if (kind === "missing" || kind === "other") {
    const box = $("preflight");
    if (box) box.hidden = true;
  }
}

// ---- '옵션' summary: what will happen without opening the disclosure ----
function updateOptSummary() {
  const el = $("optSummary");
  if (!el) return;
  const dir = ($("caseDir").value || "").trim();
  const base = dir ? hBaseName(dir) : "";
  const modeNames = { quick: "빠른 선별", standard: "표준", full: "전체 (증거용)", custom: "사용자 지정" };
  const extras = [];
  if ($("withDeepfake").checked) extras.push("딥페이크");
  el.textContent = `케이스 폴더 ${base ? base : "자동"} · ${modeNames[scanMode()] || "사용자 지정"}` + (extras.length ? " · " + extras.join(" · ") : "");
}

// ---- 이전 프로젝트 ----
const RECENT_SHOWN = 8;
let recentItems = [];
let recentSeq = 0;
function fmtWhen(unix) {
  const t = Number(unix);
  if (!Number.isFinite(t) || t <= 0) return "";
  const d = new Date(t * 1000), now = new Date();
  const p2 = n => String(n).padStart(2, "0");
  const hm = `${p2(d.getHours())}:${p2(d.getMinutes())}`;
  const day = x => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diffDays = Math.round((day(now) - day(d)) / 86400000);
  const mins = Math.floor((now.getTime() - d.getTime()) / 60000);
  if (diffDays === 0 && mins >= 0 && mins < 1) return "방금";
  if (diffDays === 0 && mins >= 0 && mins < 60) return `${mins}분 전`;
  if (diffDays === 0) return `오늘 ${hm}`;
  if (diffDays === 1) return `어제 ${hm}`;
  if (d.getFullYear() === now.getFullYear()) return `${d.getMonth() + 1}월 ${d.getDate()}일 ${hm}`;
  return `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}`;
}
// The status reports the case dir as typed; the list stores it canonical
// (macOS /var → /private/var). Equal, or one is a path-suffix of the other.
function samePathish(a, b) {
  const x = normPath(a), y = normPath(b);
  if (!x || !y) return false;
  return x === y || x.endsWith("/" + y.replace(/^\/+/, "")) || y.endsWith("/" + x.replace(/^\/+/, ""));
}
// Pure: one entry → { state, action: open|resume|progress|forget, label }.
function recentState(it, st) {
  const live = st && st.case_dir && samePathish(st.case_dir, it.case_dir);
  if (live && (st.phase === "running" || (st.phase === "review-partial" && st.busy))) {
    return { state: "분석 진행 중", action: "progress", label: "진행 보기" };
  }
  if (it.exists === false) return { state: "폴더를 찾을 수 없음", action: "forget", label: "목록에서 지우기" };
  const n = v => (v === null || v === undefined || v === "" ? null : Number(v));
  const reviewed = n(it.reviewed), total = n(it.total), done = n(it.checkpoint_done);
  const counts = total !== null && total > 0 ? ` · 검토 ${(reviewed || 0).toLocaleString()}/${total.toLocaleString()}` : "";
  switch (it.phase_hint) {
    case "interrupted": {
      const pct = total && done !== null ? Math.min(99, Math.round(done * 100 / total)) : null;
      const what = pct !== null ? `분석 중단 ${pct}%` : done ? `분석 중단 (${done.toLocaleString()}건 처리)` : "분석 중단";
      return { state: what, action: it.source_path ? "resume" : "open", label: it.source_path ? "이어서" : "열기" };
    }
    // R11 U3: /api/recent-cases sends no package hint (recent.rs: phase_hint
    // only) — the R10 "finalize-incomplete" branches were dead and are gone;
    // the opened case says 최종화 미완료 from /api/status.
    case "finalized": return { state: "결과물 생성됨" + counts, action: "open", label: "열기" };
    case "review": return { state: (counts ? counts.slice(3) : "검토 가능"), action: "open", label: "열기" };
    case "partial": return { state: "색인 완료 · 보강 전" + counts, action: "open", label: "열기" };
    case "empty": return { state: "분석 결과 없음", action: "open", label: "열기" };
    default: return { state: "", action: "open", label: "열기" };
  }
}
async function renderRecentCases() {
  const seq = ++recentSeq;
  const res = await jgetSoft("/api/recent-cases");
  let items = Array.isArray(res) ? res.slice() : [];
  // Cases remembered only by this browser (older builds kept them in
  // localStorage) are merged in when they are still real case folders.
  const known = new Set(items.map(it => normPath(it.case_dir)));
  const legacy = (loadPrefs().recentCases || []).filter(p => p && !known.has(normPath(p))).slice(0, RECENT_SHOWN);
  if (legacy.length && items.length < RECENT_SHOWN) {
    const checks = await Promise.all(legacy.map(async p => {
      try {
        const r = await jget("/api/browse?path=" + encodeURIComponent(p));
        // The server's canonical form dedupes symlinked / differently
        // spelled paths against the (canonical) server list.
        return r.is_case ? { p, key: normPath(r.canonical || p) } : null;
      } catch (e) { return null; }
    }));
    for (const c of checks.filter(Boolean)) {
      if (known.has(c.key)) continue;
      known.add(c.key);
      items.push({ case_dir: c.p, name: hBaseName(c.p) || c.p, exists: true, phase_hint: "", legacy: true });
    }
  }
  if (seq !== recentSeq) return;
  recentItems = items.slice(0, RECENT_SHOWN);
  drawRecentList();
  if (FTStore.state.panels.project) hProjectPopRender();
}
function drawRecentList() {
  const list = $("recentCaseRow"), empty = $("recentEmpty");
  if (!list) return;
  const st = lastStatus;
  const running = !!(st && (st.phase === "running" || (st.phase === "review-partial" && st.busy)));
  list.hidden = !recentItems.length;
  if (empty) empty.hidden = !!recentItems.length;
  if (typeof document.createElement !== "function" || typeof list.appendChild !== "function") return;
  list.innerHTML = "";
  // Exactly one primary in the block: the first openable row (labelled
  // "이어서 검토", or "이어서" for an interrupted analysis), else
  // '다른 케이스 폴더 열기…'. Every other row action is quiet.
  let primaryGiven = false;
  recentItems.forEach((it, i) => {
    const s = recentState(it, st);
    const li = document.createElement("li");
    li.className = "rc-row" + (it.exists === false ? " missing" : "");
    const name = it.name || hBaseName(it.case_dir) || it.case_dir;
    const when = fmtWhen(it.last_opened_unix);
    li.innerHTML =
      `<div class="rc-main"><span class="rc-name" title="${esc(it.case_dir)}">${esc(name)}</span>` +
      `<span class="rc-meta">${[when, s.state].filter(Boolean).map(esc).join(" · ")}</span>` +
      `<span class="rc-path" title="${esc(it.case_dir)}">${esc(it.case_dir)}</span></div>`;
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "rc-act";
    btn.dataset.i = String(i);
    let label = s.label;
    if (s.action === "forget") {
      btn.classList.add("quiet");
      btn.title = "목록에서만 지웁니다 — 폴더나 파일은 건드리지 않습니다";
    } else if (!primaryGiven && s.action !== "progress" && !running) {
      if (s.action === "open") label = "이어서 검토";
      btn.classList.add("primary", "lg");
      li.classList.add("rc-top");
      primaryGiven = true;
    } else {
      btn.classList.add("quiet");
    }
    btn.textContent = label;
    btn.setAttribute("aria-label", `${label} — ${name}`);
    if (running && s.action !== "progress") {
      btn.disabled = true;
      btn.title = "분석이 진행 중입니다 — 끝나거나 중단한 뒤 열 수 있습니다";
    }
    btn.addEventListener("click", () => recentAction(i, btn));
    li.appendChild(btn);
    list.appendChild(li);
  });
  const other = $("btnOpenOther");
  if (other && other.classList) other.classList.toggle("primary", !primaryGiven);
}
async function recentAction(i, btn) {
  const it = recentItems[i];
  if (!it) return;
  const s = recentState(it, lastStatus);
  if (s.action === "progress") { FTStore.set({ ui: { home: false } }); hOpenSheet("jobs", btn); return; }
  if (s.action === "forget") {
    setBusy(btn, true, "지우는 중…");
    const res = await jpost("/api/recent-cases/forget", { case_dir: it.case_dir });
    setBusy(btn, false);
    if (!res.ok) toast("목록에서 지우지 못했습니다: " + (res.error || "알 수 없는 오류"), "err");
    else toast("목록에서 지웠습니다 — 폴더는 그대로입니다.");
    if (it.legacy) {
      const prefs = loadPrefs();
      prefs.recentCases = (prefs.recentCases || []).filter(p => normPath(p) !== normPath(it.case_dir));
      try { localStorage.setItem(LS_KEY, JSON.stringify(prefs)); } catch (e) {}
    }
    renderRecentCases();
    try { $("btnOpenOther").focus(); } catch (e) {}
    return;
  }
  if (s.action === "resume") {
    // Same source + same case folder → the server resumes from the
    // checkpoint (completed files are skipped).
    $("sourcePath").value = it.source_path;
    $("caseDir").value = it.case_dir;
    kindCache = { src: "", det: null };
    updateOptSummary();
    $("btnStart").click();
    return;
  }
  setBusy(btn, true, "여는 중…");
  try { await hSwitchProject(it.case_dir); } finally { setBusy(btn, false); }
}

$("btnOpenOther").addEventListener("click", () => {
  openPicker("caseDir", "case", dir => { hSwitchProject(dir); });
});
$("caseDir").addEventListener("input", updateOptSummary);
$("withDeepfake").addEventListener("change", updateOptSummary);

// Called whenever the empty state comes back on screen.
function onHomeShown() {
  renderRecentCases();
}
