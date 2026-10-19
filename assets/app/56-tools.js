// ---- 도구: guided installs (docs/PLAN_TOOLS_2026-10-20.md track U,
// docs/ui-rebuild-contract.md §28) ----
// GET /api/tools (track S) → a compact "도구" panel on the empty state
// (#hToolsPanel: a missing required tool is a warning, a missing optional
// one is info), one finalize-checklist line per missing required tool, and
// an in-context prompt when a run needs a tool that is not installed
// (deepfake-lens: `tool_missing` from the server, or known missing here).
// Nothing is downloaded until the examiner confirms a dialog that lists
// the file, source host, size and license (T3); progress is polled from
// GET /api/tools. An older server without the endpoint shows nothing.
// deepfake-lens never appears in the panel — only in the prompt.
const HT_STR = {
  ko: {
    title: "도구",
    req: "{label} 없음 — 필수",
    opt: "{label} 없음",
    "why.ffmpeg": "영상 검증·재생 준비에 필요",
    "why.ewf": "E01 이미지에 필요",
    "why.tsk": "삭제 파일 조사에 필요",
    install: "설치",
    installAria: "{label} 설치",
    installing: "설치 중…",
    manual: "수동 설치:",
    unavailable: "여기서 설치할 수 없음",
    confirmTitle: "{label} 설치",
    confirmNote: "확인을 누르면 내려받습니다. 해시가 다르면 설치하지 않습니다.",
    confirmOk: "내려받아 설치",
    cancel: "취소",
    dFile: "파일",
    dHost: "출처",
    dSize: "크기",
    dLicense: "라이선스",
    unknown: "알 수 없음",
    sending: "설치 요청 중…",
    downloading: "내려받는 중",
    verifying: "해시 확인 중",
    extracting: "압축 푸는 중",
    done: "{label} 설치됨",
    failed: "설치 실패: {error}",
    reqFail: "설치 요청 실패: {error}",
    busy: "다른 설치가 진행 중입니다",
    finReq: "{label} 없음 — 영상 검증·프록시를 만들 수 없음",
    finReqSub: "첫 화면 ‘도구’에서 설치하거나 tools/bin에 넣으십시오.",
    dfMissing: "합성 영상 검사에 필요한 도구가 없습니다",
    toolMissing: "{label}이(가) 없습니다",
    noApi: "tools/bin 또는 PATH에 설치하십시오.",
    retry: "다시 검사",
    retryStart: "다시 시작",
  },
  en: {
    title: "Tools",
    req: "{label} missing — required",
    opt: "{label} missing",
    "why.ffmpeg": "Needed to verify and prepare videos",
    "why.ewf": "Needed for E01 images",
    "why.tsk": "Needed for deleted-file analysis",
    install: "Install",
    installAria: "Install {label}",
    installing: "Installing…",
    manual: "Manual install:",
    unavailable: "Cannot be installed here",
    confirmTitle: "Install {label}",
    confirmNote: "Downloads only after you confirm. A wrong hash is never installed.",
    confirmOk: "Download and install",
    cancel: "Cancel",
    dFile: "File",
    dHost: "Source",
    dSize: "Size",
    dLicense: "License",
    unknown: "unknown",
    sending: "Requesting install…",
    downloading: "Downloading",
    verifying: "Checking hash",
    extracting: "Extracting",
    done: "{label} installed",
    failed: "Install failed: {error}",
    reqFail: "Install request failed: {error}",
    busy: "Another install is running",
    finReq: "{label} missing — videos cannot be verified or proxied",
    finReqSub: "Install it from ‘Tools’ on the start screen or put it in tools/bin.",
    dfMissing: "The deepfake screening tool is not installed",
    toolMissing: "{label} is not installed",
    noApi: "Install it into tools/bin or PATH.",
    retry: "Run again",
    retryStart: "Start again",
  },
};
const HT_ACTIVE = ["downloading", "verifying", "extracting"];
const HT_POLL_MS = 700;
const HT = { res: null, seq: 0, timer: 0, busy: "", confirming: false, idlePolls: 0, result: null, prompt: null, written: new Map(), live: "" };

function htLocale() {
  const l = typeof hLocalePref === "function" ? hLocalePref() : "";
  return l === "en" ? "en" : "ko";
}
function htT(key, subs, locale) {
  const table = HT_STR[locale || htLocale()] || HT_STR.ko;
  const s = key in table ? table[key] : (HT_STR.ko[key] || key);
  return Object.entries(subs || {}).reduce((acc, [k, v]) => acc.split("{" + k + "}").join(String(v)), s);
}
// Pure: a GET /api/tools reply → { supported, tools, install }.
// supported:false = an older server (no endpoint) or no answer — the
// panel stays hidden, never an error.
function htParse(res) {
  if (!res || res.ok === false || !Array.isArray(res.tools)) return { supported: false, tools: [], install: null };
  const tools = res.tools.filter(t => t && typeof t === "object" && typeof t.name === "string" && t.name);
  const i = res.install && typeof res.install === "object" ? res.install : null;
  return { supported: true, tools, install: i ? { state: String(i.state || "idle"), tool: i.tool || null, bytes: i.bytes ?? null, total: i.total ?? null, error: i.error || null } : null };
}
// The panel lists binary tools only (deepfake-lens: in-context prompt).
function htPanelTools(tools) {
  return (tools || []).filter(t => t.name !== "deepfake-lens" && t.kind !== "pip");
}
function htActive(install) { return !!(install && HT_ACTIVE.includes(install.state)); }
function htLabel(t) { return String((t && (t.label || t.name)) || ""); }
function htWhy(t, locale) {
  const n = String((t && t.name) || "").toLowerCase();
  if (/ffmpeg/.test(n)) return htT("why.ffmpeg", null, locale);
  if (/ewf/.test(n)) return htT("why.ewf", null, locale);
  if (/sleuth|tsk/.test(n)) return htT("why.tsk", null, locale);
  return "";
}
function htSize(n, locale) {
  const v = Number(n);
  return n === null || n === undefined || n === "" || !Number.isFinite(v) || v < 0 ? htT("unknown", null, locale) : fmtBytes(v);
}
// Pure: the install state line ("내려받는 중 1.0 MB / 4.0 MB (25%)").
function htProgressText(install, label, locale) {
  if (!install) return "";
  const st = install.state;
  if (st === "downloading") {
    const b = Number(install.bytes), tot = Number(install.total);
    const word = htT("downloading", null, locale);
    if (install.bytes === null || !Number.isFinite(b)) return word + "…";
    if (install.total === null || !Number.isFinite(tot) || tot <= 0) return `${word} ${fmtBytes(b)}`;
    return `${word} ${fmtBytes(b)} / ${fmtBytes(tot)} (${Math.min(100, Math.floor(b * 100 / tot))}%)`;
  }
  if (st === "verifying" || st === "extracting") return htT(st, null, locale) + "…";
  if (st === "done") return htT("done", { label }, locale);
  if (st === "failed") return htT("failed", { error: install.error || htT("unknown", null, locale) }, locale);
  return "";
}
// Pure: only https homepages become links (the manual-install hint).
function htHomepage(url) {
  const s = String(url || "").trim();
  return /^https:\/\/[^\s"'<>]+$/i.test(s) ? s : "";
}
// Pure: one panel row, or null when nothing needs saying.
// action: "install" (button), "manual" (reason + homepage), "" (none).
function htRowModel(t, install, ctx, locale) {
  const c = ctx || {};
  const label = htLabel(t);
  const mine = !!(install && install.tool === t.name);
  const running = mine && htActive(install);
  const sending = c.busy === t.name && !running;
  if (t.installed) {
    if (c.result && c.result.name === t.name && c.result.ok) return { name: t.name, label, level: "ok", text: htT("done", { label }, locale), why: "", action: "", progress: "", busy: false };
    return null;
  }
  const level = t.required ? "warn" : "info";
  const model = { name: t.name, label, level, text: htT(t.required ? "req" : "opt", { label }, locale), why: htWhy(t, locale), action: "", progress: "", busy: false, reason: "", homepage: "" };
  if (t.installable === true) {
    model.action = "install";
    model.busy = running || sending || !!c.busy || htActive(install);
    model.progress = running ? htProgressText(install, label, locale)
      : sending ? htT("sending", null, locale)
        : c.result && c.result.name === t.name && !c.result.ok ? c.result.text
          : mine && install.state === "failed" ? htProgressText(install, label, locale) : "";
  } else {
    model.action = "manual";
    model.reason = String(t.unavailable_reason || "") || htT("unavailable", null, locale);
    model.homepage = htHomepage(t.homepage);
  }
  return model;
}
const HT_ICON = { warn: "i-warning", info: "i-info", ok: "i-check" };
const HT_WORD = { ko: { warn: "주의", info: "정보", ok: "완료" }, en: { warn: "Warning", info: "Info", ok: "Done" } };
function htRowHtml(m, locale) {
  const loc = locale || htLocale();
  const ic = `<svg class="ic" aria-hidden="true"><use href="#${HT_ICON[m.level] || "i-info"}"/></svg>`;
  const manual = m.action === "manual"
    ? `<small>${esc(m.reason)}${m.homepage ? ` — ${esc(htT("manual", null, loc))} <a href="${esc(m.homepage)}" target="_blank" rel="noopener noreferrer">${esc(m.homepage)}</a>` : ""}</small>` : "";
  const btn = m.action === "install"
    ? `<button type="button" data-tool-install="${esc(m.name)}" aria-label="${esc(htT("installAria", { label: m.label }, loc))}"${m.busy ? ' disabled aria-busy="true"' : ""}${m.busy ? ` title="${esc(htT("busy", null, loc))}"` : ""}>${esc(m.busy && m.progress ? htT("installing", null, loc) : htT("install", null, loc))}</button>` : "";
  return `<li class="h-tool h-tool-${esc(m.level)}" data-tool="${esc(m.name)}">${ic}<span class="h-tool-text" tabindex="-1" data-ht-focus>` +
    `<span class="sr-only">${esc((HT_WORD[loc] || HT_WORD.ko)[m.level] || "")}: </span><b>${esc(m.text)}</b>` +
    (m.why ? `<small>${esc(m.why)}</small>` : "") + (m.progress ? `<small class="h-tool-prog">${esc(m.progress)}</small>` : "") + manual +
    `</span>${btn}</li>`;
}
// Pure: the confirm dialog body — file, source host, size, license (T3).
function htConfirmHtml(t, locale) {
  const file = [htLabel(t), t.pinned_version || t.version || ""].filter(Boolean).join(" ");
  const row = (k, v) => `<dt>${esc(htT(k, null, locale))}</dt><dd>${esc(v)}</dd>`;
  return `<p>${esc(htT("confirmNote", null, locale))}</p><dl class="h-tool-dl">` +
    row("dFile", file) + row("dHost", t.source_host || htT("unknown", null, locale)) +
    row("dSize", htSize(t.size_bytes, locale)) + row("dLicense", t.license || htT("unknown", null, locale)) + `</dl>`;
}
// Pure: the in-context prompt for a run that needs `name`.
// action: "install" | "retry" | "" (reason only).
function htPromptModel(name, parsed, ctx, locale) {
  const c = ctx || {};
  const p = parsed || { supported: false, tools: [] };
  const t = p.supported ? p.tools.find(x => x.name === name) : null;
  const label = t ? htLabel(t) : name;
  const text = name === "deepfake-lens" ? htT("dfMissing", null, locale) : htT("toolMissing", { label }, locale);
  if (!t) return { name, label, text, action: "", detail: htT("noApi", null, locale), busy: false };
  if (t.installed || (c.result && c.result.name === name && c.result.ok)) return { name, label, text: htT("done", { label }, locale), action: "retry", detail: "", busy: false, ok: true };
  if (t.installable !== true) return { name, label, text, action: "", detail: String(t.unavailable_reason || "") || htT("unavailable", null, locale), busy: false };
  const install = p.install;
  const running = !!(install && install.tool === name && htActive(install));
  const sending = c.busy === name && !running;
  const detail = running ? htProgressText(install, label, locale) : sending ? htT("sending", null, locale)
    : c.result && c.result.name === name && !c.result.ok ? c.result.text
      : install && install.tool === name && install.state === "failed" ? htProgressText(install, label, locale) : "";
  return { name, label, text, action: "install", detail, busy: running || sending || !!c.busy || htActive(install) };
}
function htPromptHtml(m, retryLabel, locale) {
  const loc = locale || htLocale();
  const btn = m.action === "install"
    ? `<button type="button" data-ht-prompt="install" aria-label="${esc(htT("installAria", { label: m.label }, loc))}"${m.busy ? ' disabled aria-busy="true"' : ""}>${esc(m.busy && m.detail ? htT("installing", null, loc) : htT("install", null, loc))}</button>`
    : m.action === "retry" ? `<button type="button" data-ht-prompt="retry">${esc(retryLabel || htT("retry", null, loc))}</button>` : "";
  return `<svg class="ic" aria-hidden="true"><use href="#${m.ok ? "i-check" : "i-warning"}"/></svg>` +
    `<span class="h-tool-text" tabindex="-1" data-ht-focus><b>${esc(m.text)}</b>${m.detail ? `<small>${esc(m.detail)}</small>` : ""}</span>${btn}`;
}

// ---- state + rendering ----
function htParsed() { return HT.res || { supported: false, tools: [], install: null }; }
function htFind(name) { return htParsed().tools.find(t => t.name === name) || null; }
// True when this server lists `name` as not installed: the run asks to
// install it instead of starting (no per-file "tool not found" storm).
function htNeedsTool(name) { const t = htFind(name); return !!(t && !t.installed); }
// Writes markup only when it changed and keeps keyboard focus: the same
// button again, or — when it is now disabled — the row's text (tabindex=-1).
function htWrite(el, html) {
  if (!el || HT.written.get(el) === html) return;
  const a = document.activeElement;
  const had = a && typeof el.contains === "function" && el.contains(a);
  const key = had && a.dataset ? (a.dataset.toolInstall || a.dataset.htPrompt || "") : "";
  HT.written.set(el, html);
  el.innerHTML = html;
  if (!had || typeof el.querySelector !== "function") return;
  const again = key && typeof el.querySelectorAll === "function"
    ? [...el.querySelectorAll("button")].find(b => b.dataset && (b.dataset.toolInstall === key || b.dataset.htPrompt === key)) : null;
  const row = again && typeof again.closest === "function" ? again.closest("li") : null;
  const target = again && !again.disabled ? again
    : (row && row.querySelector("[data-ht-focus]")) || el.querySelector("[data-ht-focus]") || el.querySelector("button:not([disabled])");
  if (target && typeof target.focus === "function") { try { target.focus(); } catch (e) {} }
}
function htAnnounce(text, assertive) {
  if (!text || text === HT.live) return;
  HT.live = text;
  const live = $("hToolsLive");
  if (live) live.textContent = text;
  if (assertive && typeof announce === "function") announce(text, true);
}
function htRenderPanel() {
  const panel = $("hToolsPanel"), list = $("hToolsList");
  if (!panel || !list) return;
  const p = htParsed();
  const loc = htLocale();
  const ctx = { busy: HT.busy, result: HT.result };
  const rows = p.supported ? htPanelTools(p.tools).map(t => htRowModel(t, p.install, ctx, loc)).filter(Boolean) : [];
  panel.hidden = !rows.length;
  const title = $("hToolsPanelTitle");
  if (title && title.textContent !== htT("title", null, loc)) title.textContent = htT("title", null, loc);
  htWrite(list, rows.map(m => htRowHtml(m, loc)).join(""));
}
function htRenderPrompt() {
  const pr = HT.prompt;
  if (!pr || !pr.box) return;
  const loc = htLocale();
  const m = htPromptModel(pr.name, htParsed(), { busy: HT.busy, result: HT.result }, loc);
  pr.box.hidden = false;
  if (pr.box.classList) pr.box.classList.toggle("ok", !!m.ok);
  htWrite(pr.box, htPromptHtml(m, pr.retryLabel ? htT(pr.retryLabel, null, loc) : "", loc));
}
function htRenderAll() {
  htRenderPanel();
  htRenderPrompt();
  if (typeof finApplyEnv === "function" && typeof FIN === "object" && FIN && FIN.built) { try { finApplyEnv(); } catch (e) {} }
  htSyncStartDeepfake();
}
// The start screen's deepfake option: with a server that can install
// deepfake-lens it stays tickable — Start then shows the prompt.
function htSyncStartDeepfake() {
  const box = $("withDeepfake");
  if (!box || !box.dataset || box.dataset.envOff !== "1" || !htNeedsTool("deepfake-lens")) return;
  if (typeof setEnvOff === "function") setEnvOff(box, false); else box.disabled = false;
  const lab = typeof box.closest === "function" ? box.closest("label") : null;
  if (lab && typeof lab.setAttribute === "function") lab.setAttribute("title", "deepfake-lens 미설치 — 시작하면 설치를 안내합니다");
}
async function htLoad() {
  const seq = ++HT.seq;
  const res = typeof jgetSoft === "function" ? await jgetSoft("/api/tools") : null;
  if (seq !== HT.seq) return htParsed();
  HT.res = htParse(res);
  htRenderAll();
  return HT.res;
}
function htStopPoll() { clearTimeout(HT.timer); HT.timer = 0; }
async function htPoll() {
  htStopPoll();
  const p = await htLoad();
  const install = p.install;
  const name = HT.busy;
  if (!name) return;
  const t = htFind(name);
  const label = t ? htLabel(t) : name;
  if (htActive(install) && install.tool === name) {
    HT.idlePolls = 0;
    htAnnounce(htT(install.state, null) + " — " + label);
    HT.timer = setTimeout(htPoll, HT_POLL_MS);
    return;
  }
  const doneHere = install && install.tool === name && install.state === "done";
  const failedHere = install && install.tool === name && install.state === "failed";
  if (!doneHere && !failedHere && !(t && t.installed)) {
    // The server may not have started yet (or the poll failed): a few more tries.
    if (++HT.idlePolls <= 8) { HT.timer = setTimeout(htPoll, HT_POLL_MS); return; }
  }
  HT.busy = ""; HT.idlePolls = 0;
  const ok = !!(doneHere || (t && t.installed && !failedHere));
  HT.result = { name, ok, text: ok ? htT("done", { label }) : htProgressText(failedHere ? install : { state: "failed", error: htT("unknown") }, label) };
  htRenderAll();
  htAnnounce(HT.result.text, !ok);
  if (typeof toast === "function") toast(HT.result.text, ok ? "ok" : "err");
  // The top-strip '도구 N/5', E01 hints and run buttons follow.
  if (ok && typeof loadEnv === "function") { try { await loadEnv(); } catch (e) {} htRenderAll(); }
}
// Confirm (file · source · size · license), then POST; never sends
// anything before the dialog's OK.
async function htInstall(name) {
  const t = htFind(name);
  if (!t || t.installable !== true || t.installed) return false;
  if (HT.busy || HT.confirming || htActive(htParsed().install)) { htAnnounce(htT("busy"), false); return false; }
  const label = htLabel(t);
  // A double click must not open (and so cancel) a second dialog.
  HT.confirming = true;
  let go = false;
  try {
    go = await askDialog({ title: htT("confirmTitle", { label }), html: htConfirmHtml(t), okLabel: htT("confirmOk"), cancelLabel: htT("cancel"), focus: "dlgCancel" });
  } finally { HT.confirming = false; }
  if (!go || HT.busy) return false;
  HT.busy = name; HT.result = null; HT.idlePolls = 0;
  htRenderAll();
  htAnnounce(htT("sending"));
  const res = await jpost("/api/tools/install", { name });
  if (!res || !res.ok) {
    HT.busy = "";
    HT.result = { name, ok: false, text: htT("reqFail", { error: (res && res.error) || htT("unknown") }) };
    htRenderAll();
    htAnnounce(HT.result.text, true);
    return false;
  }
  htPoll();
  return true;
}
// In-context prompt: `box` (a hidden container next to the run's button)
// says what is missing and offers install → progress → [다시 검사].
async function htShowToolPrompt(box, name, retry, retryLabel) {
  if (!box) return;
  if (HT.prompt && HT.prompt.box !== box && HT.prompt.box) { HT.prompt.box.hidden = true; HT.written.delete(HT.prompt.box); }
  HT.prompt = { box, name: String(name), retry, retryLabel: retryLabel || "" };
  if (HT.result && HT.result.name === name && !htNeedsTool(name)) HT.result = null;
  if (!box.dataset.htWired) {
    box.dataset.htWired = "1";
    box.addEventListener("click", e => {
      const b = e.target && e.target.closest ? e.target.closest("button[data-ht-prompt]") : null;
      const pr = HT.prompt;
      if (!b || b.disabled || !pr || pr.box !== box) return;
      if (b.dataset.htPrompt === "install") htInstall(pr.name);
      else if (b.dataset.htPrompt === "retry") { htHidePrompt(box); if (typeof pr.retry === "function") pr.retry(); }
    });
  }
  htRenderPrompt();
  await htLoad();
  const m = htPromptModel(String(name), htParsed(), { busy: HT.busy, result: HT.result });
  htAnnounce(m.text + (m.detail ? " — " + m.detail : ""));
}
function htHidePrompt(box) {
  const b = box || (HT.prompt && HT.prompt.box);
  if (!b) return;
  b.hidden = true;
  HT.written.delete(b);
  b.innerHTML = "";
  if (HT.prompt && HT.prompt.box === b) HT.prompt = null;
}
// Finalize checklist: one warning per missing required tool.
async function htFinalizeChecks() {
  const p = await htLoad();
  if (!p.supported) return [];
  return htPanelTools(p.tools).filter(t => t.required && !t.installed)
    .map(t => ({ level: "warn", text: htT("finReq", { label: htLabel(t) }), sub: htT("finReqSub") }));
}

(function htWire() {
  const list = $("hToolsList");
  if (list && typeof list.addEventListener === "function") {
    list.addEventListener("click", e => {
      const b = e.target && e.target.closest ? e.target.closest("button[data-tool-install]") : null;
      if (b && !b.disabled) htInstall(b.dataset.toolInstall);
    });
  }
  if (typeof FTStore === "object" && FTStore && typeof FTStore.on === "function") {
    FTStore.on("change", ev => { if (ev && ev.keys && ev.keys.has("viewer")) { htRenderPanel(); htRenderPrompt(); } });
  }
  htLoad();
})();
