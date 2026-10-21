// ---- 도구: guided installs (docs/PLAN_TOOLS_2026-10-20.md track U,
// docs/ui-rebuild-contract.md §28) ----
// GET /api/tools (track S) → a compact "도구" panel on the empty state
// (#hToolsPanel: a missing required tool is a warning, a missing optional
// one is info), one finalize-checklist line per missing required tool, and
// an in-context prompt when a run needs a tool that is not installed
// (deepfake-lens: `tool_missing` from the server, or known missing here).
// Nothing is downloaded until the examiner confirms a dialog that lists
// the file, source host, size and license (T3); progress is polled from
// the light GET /api/tools/install-status. An older server without the
// endpoint shows nothing. On-demand tools (deepfake-lens) never appear in
// the panel — only in the prompt. Announcements go through the shell's
// global announce() (#srStatus / #srAlert), never a region inside the
// panel (the panel is hidden while a project is open).
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
    stop: "중단",
    stopAria: "{label} 설치 중단",
    stopping: "중단하는 중…",
    stopped: "{label} 설치를 중단했습니다",
    stopFail: "중단 요청 실패: {error}",
    notCancellable: "이 단계는 중단할 수 없습니다",
    lost: "설치 상태를 알 수 없습니다 — 다시 시도하십시오",
    shadow: "PATH의 다른 사본 무시됨",
    shadowTitle: "FrameTrace는 tools/bin의 것을 씁니다. PATH의 다른 사본(사용 안 함): {paths} — 직접 둔 것이 아니면 확인하십시오.",
    rollback: "이전 설치를 되돌리지 못했습니다 — 도구 파일이 섞였을 수 있습니다",
    "code.not_installable": "여기서 자동 설치할 수 없습니다",
    "code.unsupported_platform": "이 운영체제에서는 자동 설치를 지원하지 않습니다",
    "code.python_required": "Python이 필요합니다",
    "code.install_running": "다른 설치가 진행 중입니다",
    "code.busy": "분석·내보내기가 끝난 뒤 설치하십시오",
    "code.shutdown": "서버가 종료 중입니다",
    "code.unknown_tool": "이 서버가 모르는 도구입니다",
    "code.spawn_failed": "설치를 시작하지 못했습니다",
    "code.manifest_invalid": "도구 목록이 손상되어 설치할 수 없습니다",
    finReq: "{label} 없음 — 영상 검증·프록시를 만들 수 없음",
    finReqSub: "첫 화면 ‘도구’에서 설치하거나 tools/bin에 넣으십시오.",
    dfMissing: "합성 영상 검사에 필요한 도구가 없습니다",
    toolMissing: "{label}이(가) 없습니다",
    noApi: "tools/bin 또는 PATH에 설치하십시오.",
    retry: "다시 검사",
    retryStart: "다시 시작",
    dfAskStart: "deepfake-lens 미설치 — 시작하면 설치를 안내합니다",
    dfAskBtn: "deepfake-lens 미설치 — 누르면 설치를 안내합니다",
    dfManual: "deepfake-lens 미설치 — PyPI에 없음: 받은 패키지를 pip install <경로>로 설치하거나 FRAMETRACE_DEEPFAKE_LENS 경로 설정",
    dfReady: "케이스의 모든 파일을 deepfake-lens로 스크리닝 — 결과는 '합성의심' 배지로 표시",
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
    stop: "Stop",
    stopAria: "Stop installing {label}",
    stopping: "Stopping…",
    stopped: "{label} install stopped",
    stopFail: "Stop request failed: {error}",
    notCancellable: "This step cannot be stopped",
    lost: "Install state unknown — try again",
    shadow: "Other copy on PATH ignored",
    shadowTitle: "FrameTrace uses the one in tools/bin. Other copy on PATH (not used): {paths} — check it if you did not put it there.",
    rollback: "A previous install could not be undone — the tool's files may be mixed",
    "code.not_installable": "Cannot be installed automatically here",
    "code.unsupported_platform": "Automatic install is not supported on this OS",
    "code.python_required": "Python is required",
    "code.install_running": "Another install is running",
    "code.busy": "Install after the running analysis or export ends",
    "code.shutdown": "The server is shutting down",
    "code.unknown_tool": "This server does not know the tool",
    "code.spawn_failed": "The install could not be started",
    "code.manifest_invalid": "The tool list is damaged — cannot install",
    finReq: "{label} missing — videos cannot be verified or proxied",
    finReqSub: "Install it from ‘Tools’ on the start screen or put it in tools/bin.",
    dfMissing: "The deepfake screening tool is not installed",
    toolMissing: "{label} is not installed",
    noApi: "Install it into tools/bin or PATH.",
    retry: "Run again",
    retryStart: "Start again",
    dfAskStart: "deepfake-lens is not installed — starting offers to install it",
    dfAskBtn: "deepfake-lens is not installed — click to install it",
    dfManual: "deepfake-lens is not installed — not on PyPI: install the package you received with pip install <path> or set FRAMETRACE_DEEPFAKE_LENS",
    dfReady: "Screen every file in the case with deepfake-lens — results show as a 'suspected synthetic' badge",
  },
};
const HT_ACTIVE = ["downloading", "verifying", "extracting"];
const HT_POLL_MS = 700;
// The worker's error after an honoured cancel (src/tool_install/mod.rs
// INSTALL_CANCELLED); newer servers also send `cancelled: true`.
const HT_CANCELLED = "install cancelled";
// Stages the server refuses to cancel (moving files into place, venv, pip).
const HT_LOCKED_STAGES = ["install", "venv", "pip"];
// Consecutive failed status polls before the tab gives up following.
const HT_MAX_POLL_FAILS = 20;
// A `failed` job from before this tab looked is shown only this long
// (judged by the server's clock: J.age_s, or now_unix - finished_unix).
const HT_RECENT_S = 300;
// busy: the tool whose install this tab follows (clicked here, or adopted
// from the server after a refresh / in a second tab); job: its install id.
const HT = { res: null, seq: 0, timer: 0, polling: false, busy: "", job: null, confirming: false, stopping: "", stopAsked: false, note: null, result: null, prompt: null, written: new Map(), live: "", fails: 0 };

function htLocale() {
  const l = typeof hLocalePref === "function" ? hLocalePref() : "";
  return l === "en" ? "en" : "ko";
}
function htT(key, subs, locale) {
  const table = HT_STR[locale || htLocale()] || HT_STR.ko;
  const s = key in table ? table[key] : (HT_STR.ko[key] || key);
  return Object.entries(subs || {}).reduce((acc, [k, v]) => acc.split("{" + k + "}").join(String(v)), s);
}
const htNum = (v) => (v === null || v === undefined || v === "" ? NaN : Number(v));
// Pure: an install record (J) → the fields the UI reads, or null.
// `serverNow`: the reply's `now_unix` (the server's clock). `age_s` is how
// long ago the job finished by that clock (J.age_s, else now_unix -
// finished_unix; null when the server sent neither — an older server).
function htParseInstall(i, serverNow) {
  if (!i || typeof i !== "object") return null;
  const fin = htNum(i.finished_unix), now = htNum(serverNow), age = htNum(i.age_s);
  const ageS = Number.isFinite(age) ? age : Number.isFinite(fin) && Number.isFinite(now) ? Math.max(0, now - fin) : null;
  return { id: i.id ?? null, state: String(i.state || "idle"), stage: i.stage || null, tool: i.tool || null, bytes: i.bytes ?? null, total: i.total ?? null, error: i.error || null,
    finished_unix: Number.isFinite(fin) ? fin : null, age_s: ageS, cancelled: typeof i.cancelled === "boolean" ? i.cancelled : null };
}
// Pure: a `failed` job that ended on a cancel (any tab, the CLI). An older
// server has no `cancelled` field: its INSTALL_CANCELLED error, or a stop
// this tab asked for, counts.
function htCancelledJob(install, asked) {
  if (!install || install.state !== "failed") return false;
  if (install.cancelled === true || install.error === HT_CANCELLED) return true;
  return install.cancelled !== false && !!asked;
}
// Pure: a finished job recent enough to report (an old `failed` job is
// not re-shown on every refresh / in every tab). Judged by the server's
// clock (`age_s`); only an older server without `now_unix` / `age_s` falls
// back to this browser's clock.
function htRecent(install, nowS) {
  const f = install && install.finished_unix;
  if (f === null || f === undefined || !Number.isFinite(f)) return false;
  if (install.age_s !== null && install.age_s !== undefined && Number.isFinite(install.age_s)) return install.age_s <= HT_RECENT_S;
  const now = nowS === undefined || nowS === null ? Date.now() / 1000 : nowS;
  return now - f <= HT_RECENT_S;
}
// Pure: the running stage cannot be cancelled ([중단] disabled).
function htStopLocked(install) { return !!(install && HT_LOCKED_STAGES.includes(install.stage)); }
// Pure: a GET /api/tools reply → { supported, tools, install }.
// supported:false = an older server (no endpoint) or no answer — the
// panel stays hidden, never an error.
function htParse(res) {
  if (!res || res.ok === false || !Array.isArray(res.tools)) return { supported: false, tools: [], install: null };
  const tools = res.tools.filter(t => t && typeof t === "object" && typeof t.name === "string" && t.name);
  return { supported: true, tools, install: htParseInstall(res.install, res.now_unix) };
}
// Pure: the other copies on PATH that tools/bin takes precedence over
// (T.shadowed_by values; FrameTrace does not run them).
function htShadows(t) {
  const s = t && t.shadowed_by && typeof t.shadowed_by === "object" ? t.shadowed_by : {};
  return Object.values(s).filter(v => typeof v === "string" && v.trim()).map(v => v.trim());
}
// The panel lists binary tools only; on-demand / pip tools (deepfake-lens)
// are offered only in the in-context prompt.
function htPanelTools(tools) {
  return (tools || []).filter(t => t.name !== "deepfake-lens" && t.kind !== "pip" && t.on_demand !== true);
}
function htActive(install) { return !!(install && HT_ACTIVE.includes(install.state)); }
function htLabel(t) { return String((t && (t.label || t.title || t.name)) || ""); }
// The short Korean manual-install hint (shown) and the manifest's long
// English reason (title only).
function htHint(t) { return String((t && t.manual_hint) || "").trim(); }
function htReason(t) { return String((t && (t.unavailable_reason || t.not_installable_reason)) || "").trim(); }
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
  if (st === "failed" && htCancelledJob(install)) return htT("stopped", { label }, locale);
  if (st === "failed") return htT("failed", { error: install.error || htT("unknown", null, locale) }, locale);
  return "";
}
// Pure: only https homepages become links (the manual-install hint).
function htHomepage(url) {
  const s = String(url || "").trim();
  return /^https:\/\/[^\s"'<>]+$/i.test(s) ? s : "";
}
// Pure: escaped hint text whose https URLs are links; a hint without a
// URL gets "수동 설치: <homepage>" (https only) after it.
function htHintHtml(text, homepage, locale) {
  const s = String(text || "");
  const link = (u) => `<a href="${esc(u)}" target="_blank" rel="noopener noreferrer">${esc(u)}</a>`;
  let out = "", last = 0, found = false;
  const re = /https:\/\/[^\s"'<>]+/g;
  let m;
  while ((m = re.exec(s))) {
    const url = m[0].replace(/[).,;:·」』]+$/, "");
    out += esc(s.slice(last, m.index)) + link(url);
    last = m.index + url.length;
    found = true;
  }
  out += esc(s.slice(last));
  const home = htHomepage(homepage);
  if (!found && home) out += (s ? " — " : "") + esc(htT("manual", null, locale)) + " " + link(home);
  return out;
}
// Pure: a POST reply that never arrived (network, status 0, an unreadable
// 2xx body) — the server may have acted on it. Any HTTP error status
// (a 5xx included, JSON body or not) is the server's answer: a refusal.
function htPostLost(res) {
  if (!res) return true;
  if (Number(res.status) >= 400) return false;
  return !!(res.network || res.malformed || (!res.ok && res.status === 0));
}
// Pure: a refused POST /api/tools/install → { text (short, by code),
// hint (manual_hint), homepage, title (the server's full message) }.
function htRefusal(res, t, locale) {
  const r = res || {};
  const code = typeof r.code === "string" ? r.code : "";
  const key = "code." + code;
  const known = code && key in HT_STR.ko;
  const text = known ? htT(key, null, locale) : htT("reqFail", { error: r.error || htT("unknown", null, locale) }, locale);
  const showHint = code === "not_installable" || code === "unsupported_platform" || code === "python_required";
  return {
    text,
    hint: showHint ? (String(r.manual_hint || "").trim() || htHint(t)) : "",
    homepage: showHint ? htHomepage(r.homepage || (t && t.homepage)) : "",
    title: known ? String(r.error || "") : "",
  };
}
// Pure: one panel row, or null when nothing needs saying.
// action: "install" ([설치], or [중단] while it runs), "manual"
// (manual_hint + homepage), "" (none).
function htRowModel(t, install, ctx, locale) {
  const c = ctx || {};
  const label = htLabel(t);
  const mine = !!(install && install.tool === t.name);
  const running = mine && htActive(install);
  const sending = c.busy === t.name && !running;
  const res = c.result && c.result.name === t.name ? c.result : null;
  const shadows = htShadows(t);
  const shadow = shadows.length ? { text: htT("shadow", null, locale), title: htT("shadowTitle", { paths: shadows.join(", ") }, locale) } : null;
  // Fix round 13 L2: an incomplete rollback left previous files behind
  // (T.rollback_note says where and how to restore; Korean, from the server).
  const rollback = t.rollback_incomplete === true ? { text: htT("rollback", null, locale), title: String(t.rollback_note || "") } : null;
  if (t.installed) {
    if (rollback) return { name: t.name, label, level: "warn", text: label, why: "", action: "", progress: "", busy: false, shadow, rollback };
    if (res && res.ok) return { name: t.name, label, level: "ok", text: htT("done", { label }, locale), why: "", action: "", progress: "", busy: false, shadow };
    // Installed from tools/bin, but another copy sits on PATH: one short
    // line so the examiner can check it is not planted.
    if (shadow) return { name: t.name, label, level: "info", text: label, why: "", action: "", progress: "", busy: false, shadow };
    return null;
  }
  const level = t.required ? "warn" : "info";
  const model = { name: t.name, label, level, text: htT(t.required ? "req" : "opt", { label }, locale), why: htWhy(t, locale), action: "", progress: "", busy: false, stop: false, stopBusy: false, note: "", hint: "", reason: "", homepage: "", shadow, rollback };
  if (t.installable === true) {
    model.action = "install";
    model.busy = running || sending || !!c.busy || htActive(install);
    model.stop = running && c.busy === t.name;
    model.stopBusy = model.stop && c.stopping === t.name;
    model.stopLocked = model.stop && !model.stopBusy && htStopLocked(install);
    model.progress = running ? htProgressText(install, label, locale) + (model.stopBusy ? " — " + htT("stopping", null, locale) : "")
      : sending ? htT("sending", null, locale)
        : res && !res.ok ? res.text
          : mine && install.state === "failed" && htRecent(install, c.now) ? htProgressText(install, label, locale) : "";
    if (!running && !sending && res && !res.ok) { model.hint = res.hint || ""; model.homepage = res.homepage || ""; model.reason = res.title || ""; }
    if (c.note && c.note.name === t.name && running) model.note = c.note.text;
  } else {
    model.action = "manual";
    model.hint = htHint(t) || htT("unavailable", null, locale);
    model.reason = htReason(t);
    model.homepage = htHomepage(t.homepage);
  }
  return model;
}
const HT_ICON = { warn: "i-warning", info: "i-info", ok: "i-check" };
const HT_WORD = { ko: { warn: "주의", info: "정보", ok: "완료" }, en: { warn: "Warning", info: "Info", ok: "Done" } };
function htHintSmall(m, loc) {
  return m.hint || m.homepage ? `<small class="h-tool-hint"${m.reason ? ` title="${esc(m.reason)}"` : ""}>${htHintHtml(m.hint, m.homepage, loc)}</small>` : "";
}
function htRowHtml(m, locale) {
  const loc = locale || htLocale();
  const ic = `<svg class="ic" aria-hidden="true"><use href="#${HT_ICON[m.level] || "i-info"}"/></svg>`;
  const btn = m.action !== "install" ? ""
    : m.stop ? `<button type="button" data-tool-cancel="${esc(m.name)}" aria-label="${esc(htT("stopAria", { label: m.label }, loc))}"${m.stopBusy ? ' disabled aria-busy="true"' : m.stopLocked ? ` disabled title="${esc(htT("notCancellable", null, loc))}"` : ""}>${esc(htT("stop", null, loc))}</button>`
      : `<button type="button" data-tool-install="${esc(m.name)}" aria-label="${esc(htT("installAria", { label: m.label }, loc))}"${m.busy ? ' disabled aria-busy="true"' : ""}${m.busy ? ` title="${esc(htT("busy", null, loc))}"` : ""}>${esc(m.busy && m.progress ? htT("installing", null, loc) : htT("install", null, loc))}</button>`;
  return `<li class="h-tool h-tool-${esc(m.level)}" data-tool="${esc(m.name)}">${ic}<span class="h-tool-text" tabindex="-1" data-ht-focus>` +
    `<span class="sr-only">${esc((HT_WORD[loc] || HT_WORD.ko)[m.level] || "")}: </span><b>${esc(m.text)}</b>` +
    (m.why ? `<small>${esc(m.why)}</small>` : "") + (m.progress ? `<small class="h-tool-prog">${esc(m.progress)}</small>` : "") +
    (m.note ? `<small>${esc(m.note)}</small>` : "") + htHintSmall(m, loc) +
    // The paths are in the text too (sr-only), not only in a hover title.
    (m.shadow ? `<small class="h-tool-shadow" title="${esc(m.shadow.title)}">${esc(m.shadow.text)}<span class="sr-only"> — ${esc(m.shadow.title)}</span></small>` : "") +
    (m.rollback ? `<small class="h-tool-rollback"${m.rollback.title ? ` title="${esc(m.rollback.title)}"` : ""}>${esc(m.rollback.text)}${m.rollback.title ? `<span class="sr-only"> — ${esc(m.rollback.title)}</span>` : ""}</small>` : "") +
    `</span>${btn}</li>`;
}
// Pure: the confirm dialog body — file (download.file_name), source host,
// size, license (T3), then the distribution notice (T5, option C: the
// examiner downloads the file from the publisher, FrameTrace does not
// distribute it; `notice_en` for locale en, else `notice`; omitted when the
// tool has none).
function htConfirmHtml(t, locale) {
  const d = t.download && typeof t.download === "object" ? t.download : {};
  const file = String(d.file_name || "") || [htLabel(t), t.pinned_version || t.version || ""].filter(Boolean).join(" ");
  const row = (k, v) => `<dt>${esc(htT(k, null, locale))}</dt><dd>${esc(v)}</dd>`;
  const loc = locale || htLocale();
  const notice = String((loc === "en" && t.notice_en) || t.notice || "").trim();
  return `<p>${esc(htT("confirmNote", null, locale))}</p><dl class="h-tool-dl">` +
    row("dFile", file) + row("dHost", d.host || t.source_host || htT("unknown", null, locale)) +
    row("dSize", htSize(d.size ?? t.size_bytes, locale)) + row("dLicense", t.license || htT("unknown", null, locale)) + `</dl>` +
    (notice ? `<p class="h-tool-notice">${esc(notice)}</p>` : "");
}
// Pure: the in-context prompt for a run that needs `name`.
// action: "install" | "stop" | "retry" | "" (hint only).
function htPromptModel(name, parsed, ctx, locale) {
  const c = ctx || {};
  const p = parsed || { supported: false, tools: [] };
  const t = p.supported ? p.tools.find(x => x.name === name) : null;
  const label = t ? htLabel(t) : name;
  const text = name === "deepfake-lens" ? htT("dfMissing", null, locale) : htT("toolMissing", { label }, locale);
  const res = c.result && c.result.name === name ? c.result : null;
  if (!t) return { name, label, text, action: "", detail: htT("noApi", null, locale), busy: false };
  if (t.installed || (res && res.ok)) return { name, label, text: htT("done", { label }, locale), action: "retry", detail: "", busy: false, ok: true };
  if (t.installable !== true) return { name, label, text, action: "", detail: "", hint: htHint(t) || htT("unavailable", null, locale), reason: htReason(t), homepage: htHomepage(t.homepage), busy: false };
  const install = p.install;
  const running = !!(install && install.tool === name && htActive(install));
  const sending = c.busy === name && !running;
  const stopBusy = running && c.stopping === name;
  const detail = running ? htProgressText(install, label, locale) + (stopBusy ? " — " + htT("stopping", null, locale) : "")
    : sending ? htT("sending", null, locale)
      : res && !res.ok ? res.text
        : install && install.tool === name && install.state === "failed" && htRecent(install, c.now) ? htProgressText(install, label, locale) : "";
  const stopLocked = running && c.busy === name && !stopBusy && htStopLocked(install);
  const m = { name, label, text, action: running && c.busy === name ? "stop" : "install", detail, busy: running || sending || !!c.busy || htActive(install), stopBusy, stopLocked };
  if (!running && !sending && res && !res.ok) { m.hint = res.hint || ""; m.homepage = res.homepage || ""; m.reason = res.title || ""; }
  if (running && c.note && c.note.name === name) m.note = c.note.text;
  return m;
}
function htPromptHtml(m, retryLabel, locale) {
  const loc = locale || htLocale();
  const btn = m.action === "install"
    ? `<button type="button" data-ht-prompt="install" aria-label="${esc(htT("installAria", { label: m.label }, loc))}"${m.busy ? ' disabled aria-busy="true"' : ""}>${esc(m.busy && m.detail ? htT("installing", null, loc) : htT("install", null, loc))}</button>`
    : m.action === "stop" ? `<button type="button" data-ht-prompt="stop" aria-label="${esc(htT("stopAria", { label: m.label }, loc))}"${m.stopBusy ? ' disabled aria-busy="true"' : m.stopLocked ? ` disabled title="${esc(htT("notCancellable", null, loc))}"` : ""}>${esc(htT("stop", null, loc))}</button>`
      : m.action === "retry" ? `<button type="button" data-ht-prompt="retry">${esc(retryLabel || htT("retry", null, loc))}</button>` : "";
  return `<svg class="ic" aria-hidden="true"><use href="#${m.ok ? "i-check" : "i-warning"}"/></svg>` +
    `<span class="h-tool-text" tabindex="-1" data-ht-focus><b>${esc(m.text)}</b>${m.detail ? `<small>${esc(m.detail)}</small>` : ""}` +
    (m.note ? `<small>${esc(m.note)}</small>` : "") + htHintSmall(m, loc) + `</span>${btn}`;
}

// ---- state + rendering ----
function htParsed() { return HT.res || { supported: false, tools: [], install: null }; }
function htFind(name) { return htParsed().tools.find(t => t.name === name) || null; }
// True when this server lists `name` as missing AND can install it here:
// the run then asks to install instead of starting. A tool that cannot be
// installed here keeps its control disabled (manual_hint as the tooltip).
function htNeedsTool(name) { const t = htFind(name); return !!(t && !t.installed && t.installable === true); }
function htToolHint(name) { return htHint(htFind(name)); }
function htCtx() { return { busy: HT.busy, result: HT.result, stopping: HT.stopping, note: HT.note }; }
// Writes markup only when it changed and keeps keyboard focus: the same
// button again, or — when it is gone or disabled — that row's text
// (tabindex=-1; also when focus was on a row's text), never <body>.
function htWrite(el, html) {
  if (!el || HT.written.get(el) === html) return;
  const a = document.activeElement;
  const had = a && typeof el.contains === "function" && el.contains(a);
  const ds = had && a.dataset ? a.dataset : {};
  const kind = ds.toolInstall ? "toolInstall" : ds.toolCancel ? "toolCancel" : ds.htPrompt ? "htPrompt" : "";
  const key = kind ? ds[kind] : "";
  const li = !kind && had && typeof a.closest === "function" ? a.closest("li[data-tool]") : null;
  const tool = ds.toolInstall || ds.toolCancel || (li && li.dataset ? li.dataset.tool || "" : "");
  HT.written.set(el, html);
  el.innerHTML = html;
  if (!had || typeof el.querySelector !== "function") return;
  const all = (sel) => typeof el.querySelectorAll === "function" ? [...el.querySelectorAll(sel)] : [];
  const again = kind ? all("button").find(b => b.dataset && b.dataset[kind] === key) : null;
  const row = tool ? all("li").find(li => li.dataset && li.dataset.tool === tool) : null;
  const target = again && !again.disabled ? again
    : (row && row.querySelector("[data-ht-focus]")) || el.querySelector("[data-ht-focus]") || el.querySelector("button:not([disabled])");
  if (target && typeof target.focus === "function") { try { target.focus(); } catch (e) {} }
}
// One message per change, through the shell's global live regions.
function htAnnounce(text, assertive) {
  if (!text || (text === HT.live && !assertive)) return;
  HT.live = text;
  if (typeof announce === "function") announce(text, !!assertive);
}
function htRenderPanel() {
  const panel = $("hToolsPanel"), list = $("hToolsList");
  if (!panel || !list) return;
  const p = htParsed();
  const loc = htLocale();
  const ctx = htCtx();
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
  const m = htPromptModel(pr.name, htParsed(), htCtx(), loc);
  pr.box.hidden = false;
  if (pr.box.classList) pr.box.classList.toggle("ok", !!m.ok);
  htWrite(pr.box, htPromptHtml(m, pr.retryLabel ? htT(pr.retryLabel, null, loc) : "", loc));
}
function htRenderAll() {
  htRenderPanel();
  htRenderPrompt();
  htSyncDeepfake();
}
// The deepfake controls (start option + finalize [딥페이크 스크리닝]).
// /api/env (loadEnv) and /api/tools (htLoad) answer in either order, so
// both call this: an env-missing control is re-enabled only when this
// server can install deepfake-lens (the click then shows the prompt);
// otherwise it stays disabled with the manual_hint as its tooltip.
function htSyncDeepfake() {
  const box = $("withDeepfake");
  if (box && box.dataset && box.dataset.envOff === "1") {
    const lab = typeof box.closest === "function" ? box.closest("label") : null;
    if (htNeedsTool("deepfake-lens")) {
      if (typeof setEnvOff === "function") setEnvOff(box, false); else { box.disabled = false; delete box.dataset.envOff; }
      if (lab && typeof lab.setAttribute === "function") lab.setAttribute("title", htT("dfAskStart"));
    } else {
      box.disabled = true;
      const hint = htToolHint("deepfake-lens");
      if (hint && lab && typeof lab.setAttribute === "function") lab.setAttribute("title", hint);
    }
  }
  if (typeof finApplyEnv === "function" && typeof FIN === "object" && FIN && FIN.built) { try { finApplyEnv(); } catch (e) {} }
}
// Follows an install that is already running on the server (a refresh,
// a second tab): polling starts without a click here.
function htAdopt(p) {
  const i = p && p.install;
  if (HT.busy || HT.timer || HT.polling || !htActive(i) || !i.tool) return;
  HT.busy = i.tool; HT.job = i.id; HT.result = null; HT.note = null; HT.stopping = ""; HT.stopAsked = false; HT.fails = 0;
  HT.timer = setTimeout(htPoll, HT_POLL_MS);
  // A confirm dialog is open here: its OK will not start a second install.
  if (HT.confirming) htAnnounceBusy();
}
// "다른 설치가 진행 중입니다" — announced again even right after itself.
function htAnnounceBusy() { HT.live = ""; htAnnounce(htT("busy"), false); }
async function htLoad() {
  const seq = ++HT.seq;
  const res = typeof jgetSoft === "function" ? await jgetSoft("/api/tools") : null;
  if (seq !== HT.seq) return htParsed();
  const parsed = htParse(res);
  // A transient failure (no answer) keeps what is on screen; only an
  // older server (no endpoint) hides the feature.
  const gone = res && res.ok === false && (typeof endpointMissing === "function" ? endpointMissing(res) : res.status === 404);
  if (parsed.supported || gone || !(HT.res && HT.res.supported)) HT.res = parsed;
  // Only a fresh answer starts following a job (never the kept, stale one).
  if (parsed.supported) htAdopt(parsed);
  htRenderAll();
  return HT.res;
}
function htStopPoll() { clearTimeout(HT.timer); HT.timer = 0; }
function htSetInstall(install) {
  if (HT.res && HT.res.supported) HT.res = Object.assign({}, HT.res, { install });
}
// Polls GET /api/tools/install-status. A failed poll keeps the last state
// (no flicker) and tries again; only the server's `failed` is a failure.
// A 401 or HT_MAX_POLL_FAILS failures in a row stop following ("lost").
// A different running job (another tab, the CLI, a restarted server) is
// adopted and followed.
async function htPoll() {
  htStopPoll();
  const name = HT.busy;
  if (!name) return;
  HT.polling = true;
  let res = null;
  try { res = typeof jgetSoft === "function" ? await jgetSoft("/api/tools/install-status") : null; } finally { HT.polling = false; }
  if (HT.busy !== name) return;
  if (res && res.ok === false && typeof endpointMissing === "function" && endpointMissing(res)) {
    // No light endpoint: the full list carries the same record.
    const p = await htLoad();
    res = p.supported ? { ok: true, install: p.install } : null;
    if (HT.busy !== name) return;
  }
  if (res && res.ok === false && res.status === 401) { await htFinish(name, null); return; }
  if (!res || res.ok !== true || !("install" in res)) {
    HT.fails += 1;
    if (HT.fails >= HT_MAX_POLL_FAILS) { await htFinish(name, null); return; }
    HT.timer = setTimeout(htPoll, HT_POLL_MS);
    return;
  }
  HT.fails = 0;
  const install = htParseInstall(res.install, res.now_unix);
  const ours = !!(install && install.tool === name && (HT.job === null || HT.job === undefined || install.id === null || install.id === HT.job));
  if (!ours && htActive(install) && install.tool) {
    HT.busy = install.tool; HT.job = install.id; HT.stopAsked = false; HT.stopping = ""; HT.note = null; HT.result = null;
    htSetInstall(install);
    htRenderAll();
    htAnnounce(htT(install.state) + " — " + htLabel(htFind(install.tool) || { name: install.tool }));
    HT.timer = setTimeout(htPoll, HT_POLL_MS);
    return;
  }
  if (ours && htActive(install)) {
    const prev = htParsed().install;
    htSetInstall(install);
    htRenderAll();
    if (!prev || prev.state !== install.state || prev.tool !== install.tool) htAnnounce(htT(install.state) + " — " + htLabel(htFind(name) || { name }));
    HT.timer = setTimeout(htPoll, HT_POLL_MS);
    return;
  }
  if (ours) htSetInstall(install);
  await htFinish(name, ours ? install : null);
}
async function htFinish(name, install) {
  // Idle before reloading, so htLoad can adopt an install that is running
  // again (another tab) instead of leaving every button disabled.
  const asked = HT.stopAsked || HT.stopping === name;
  HT.busy = ""; HT.job = null; HT.stopping = ""; HT.stopAsked = false; HT.note = null; HT.fails = 0;
  htStopPoll();
  // Lost: the last running record is no longer true.
  if (!install) htSetInstall(null);
  await htLoad();
  const t = htFind(name);
  const label = t ? htLabel(t) : name;
  const done = !!(install && install.state === "done");
  const failed = !!(install && install.state === "failed");
  const ok = done || (!failed && !!(t && t.installed));
  const stopped = failed && htCancelledJob(install, asked);
  const text = ok ? htT("done", { label }) : stopped ? htT("stopped", { label }) : failed ? htProgressText(install, label) : htT("lost");
  HT.result = { name, ok, text };
  htRenderAll();
  const kind = ok ? "ok" : failed && !stopped ? "err" : "warn";
  if (typeof toast === "function") toast(text, kind); else htAnnounce(text, kind === "err");
  // The top-strip '도구 N/5', E01 hints and run buttons follow.
  if (ok && typeof loadEnv === "function") { try { await loadEnv(); } catch (e) {} htRenderAll(); }
}
// Confirm (file · source · size · license), then POST; never sends
// anything before the dialog's OK.
async function htInstall(name) {
  const t = htFind(name);
  if (!t || t.installable !== true || t.installed) return false;
  if (HT.busy || HT.confirming || htActive(htParsed().install)) { htAnnounceBusy(); return false; }
  const label = htLabel(t);
  // A double click must not open (and so cancel) a second dialog.
  HT.confirming = true;
  let go = false;
  try {
    go = await askDialog({ title: htT("confirmTitle", { label }), html: htConfirmHtml(t), okLabel: htT("confirmOk"), cancelLabel: htT("cancel"), focus: "dlgCancel" });
  } finally { HT.confirming = false; }
  if (!go) return false;
  // An install was adopted while the dialog was open (another tab, the
  // CLI): OK does not start a second one — and says so.
  if (HT.busy || htActive(htParsed().install)) { htAnnounceBusy(); return false; }
  HT.busy = name; HT.job = null; HT.result = null; HT.note = null; HT.stopping = ""; HT.stopAsked = false; HT.fails = 0;
  htRenderAll();
  // Not announced: the POST reply's state follows within milliseconds.
  const res = await jpost("/api/tools/install", { name });
  if (htPostLost(res)) {
    // The reply was lost, not refused: the server may well have started
    // the install. Ask it, and follow what is running (or report it).
    HT.busy = "";
    const p = await htLoad();
    // Following this tool's install: started after all.
    if (HT.busy === name) return true;
    // Another tool's install was adopted (another tab, the CLI): it is
    // followed, but this tool's request is still unknown — say so.
    const i = HT.busy ? null : p.install;
    if (i && i.tool === name && !htActive(i) && htRecent(i)) { await htFinish(name, i); return false; }
    HT.result = { name, ok: false, text: htT("lost") };
    htRenderAll();
    htAnnounce(HT.result.text, true);
    return false;
  }
  if (!res.ok) {
    HT.busy = "";
    const r = htRefusal(res, t);
    HT.result = Object.assign({ name, ok: false }, r);
    htRenderAll();
    htAnnounce(r.text, true);
    // Someone else's install is running: follow it.
    if (res && res.code === "install_running") await htLoad();
    return false;
  }
  const install = htParseInstall(res.install, res.now_unix);
  if (install && install.tool === name) { HT.job = install.id; htSetInstall(install); htRenderAll(); if (htActive(install)) htAnnounce(htT(install.state) + " — " + label); }
  HT.timer = setTimeout(htPoll, HT_POLL_MS);
  return true;
}
// [중단]: POST /api/tools/install-cancel. Some stages (moving files) cannot
// be stopped — the server answers `not_cancellable` and the install goes on.
async function htCancel(name) {
  if (!name || HT.busy !== name || HT.stopping) return false;
  HT.stopping = name; HT.note = null;
  htRenderAll();
  // The job id: a late click never stops another tab's newer install
  // (the server answers `stale_job`).
  const res = await jpost("/api/tools/install-cancel", HT.job === null || HT.job === undefined ? {} : { id: HT.job });
  if (HT.busy !== name) { HT.stopping = ""; return false; }
  if (res && res.ok) {
    HT.stopAsked = true;
    const install = htParseInstall(res.install, res.now_unix);
    if (install && install.tool === name) htSetInstall(install);
    htRenderAll();
    htAnnounce(htT("stopping"));
    return true;
  }
  HT.stopping = "";
  // Already ended (or replaced by another job): the next poll shows it.
  if (res && (res.code === "no_install" || res.code === "stale_job")) { htRenderAll(); return false; }
  HT.note = { name, text: res && res.code === "not_cancellable" ? htT("notCancellable") : htT("stopFail", { error: (res && res.error) || htT("unknown") }) };
  htRenderAll();
  htAnnounce(HT.note.text, false);
  return false;
}
// In-context prompt: `box` (a hidden container next to the run's button)
// says what is missing and offers install → progress → [다시 검사]. Focus
// moves to its text when it appears (the start / run button stays put).
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
      else if (b.dataset.htPrompt === "stop") htCancel(pr.name);
      else if (b.dataset.htPrompt === "retry") { htHidePrompt(box); if (typeof pr.retry === "function") pr.retry(); }
    });
  }
  htRenderPrompt();
  const f = typeof box.querySelector === "function" ? box.querySelector("[data-ht-focus]") : null;
  if (f && typeof f.focus === "function") { try { f.focus(); } catch (e) {} }
  await htLoad();
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
      const b = e.target && e.target.closest ? e.target.closest("button[data-tool-install], button[data-tool-cancel]") : null;
      if (!b || b.disabled) return;
      if (b.dataset.toolCancel) htCancel(b.dataset.toolCancel); else htInstall(b.dataset.toolInstall);
    });
  }
  if (typeof FTStore === "object" && FTStore && typeof FTStore.on === "function") {
    FTStore.on("change", ev => { if (ev && ev.keys && ev.keys.has("viewer")) { htRenderPanel(); htRenderPrompt(); } });
  }
  htLoad();
})();
