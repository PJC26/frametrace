const STEP_NAMES = ["케이스 준비", "소스 등록", "스캔 · 색인", "재생성 검증", "리뷰 생성"];

// U8: a 401 means the server is up but refused this page's proof (the
// HttpOnly ft_token / ft_nonce cookie is missing or belongs to another
// FrameTrace instance on this computer) — never "server down", and not
// "expired" either: the likely cause is a second window/instance.
const AUTH_EXPIRED_MSG = "요청이 인증 오류로 거부되었습니다 — 같은 컴퓨터에서 다른 FrameTrace 창이 열려 있으면 닫고 이 창을 새로 고치십시오";
// Reads a fetch Response as JSON without ever throwing: HTTP errors,
// non-JSON bodies (a proxy error page, a truncated reply) and network
// failures all collapse to { ok:false, error } so callers have exactly one
// failure shape to render. `kind` ("read" = GET, "write" = POST) feeds the
// auth banner (40-status.js): a 401 raises it for that kind, a success of
// the same kind clears it — a public GET (no-token mode: /api/status needs
// no proof) never clears a refused write.
async function readJson(r, kind) {
  let data = null;
  try { data = await r.json(); } catch (e) { data = null; }
  if (!r || !r.ok) {
    const status = r && r.status ? `HTTP ${r.status}` : "응답 없음";
    if (r && r.status === 401) {
      if (typeof noteAuthExpired === "function") noteAuthExpired(kind || "read");
      return Object.assign({}, data && typeof data === "object" ? data : {}, { ok: false, status: 401, auth: true, error: AUTH_EXPIRED_MSG });
    }
    return Object.assign({}, data && typeof data === "object" ? data : {}, {
      ok: false,
      // HTTP status kept so callers can tell "endpoint not in this server
      // build" (404/405 — degrade quietly) from a real failure.
      status: (r && r.status) || 0,
      error: (data && data.error) || `서버 오류 (${status})`,
    });
  }
  if (typeof noteAuthOk === "function") noteAuthOk(kind || "read");
  if (!data || typeof data !== "object") return { ok: false, malformed: true, error: "서버 응답을 해석할 수 없습니다 (JSON 아님)" };
  return data;
}
function netError(err) {
  return { ok: false, network: true, status: 0, error: "서버에 연결할 수 없습니다: " + ((err && err.message) || err || "network") };
}
// jget throws on failure (existing callers wrap it and treat a throw as
// "unknown"); jpost never throws — it always resolves to { ok, error }.
async function jget(url) {
  const r = await fetch(url, { cache: "no-store" });
  const data = await readJson(r, "read");
  if (!(r && r.ok) || data.malformed) {
    const err = new Error(data.error);
    err.status = data.status || 0;
    throw err;
  }
  return data;
}
// Like jpost for GET: never throws, keeps the HTTP status on failure.
async function jgetSoft(url) {
  try {
    const r = await fetch(url, { cache: "no-store" });
    return await readJson(r, "read");
  } catch (err) {
    return netError(err);
  }
}
// True when a soft call failed because the server does not have the
// endpoint (an older build) — callers then hide the feature, not error.
function endpointMissing(res) {
  return !!(res && !res.ok && (res.status === 404 || res.status === 405 || res.status === 501));
}
async function jpost(url, body) {
  try {
    const r = await fetch(url, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
    return await readJson(r, "write");
  } catch (err) {
    return netError(err);
  }
}

// ---- screen-reader announcements: one polite + one assertive region ----
// Toasts and job transitions are announced here (the toast stack itself
// is not a live region, so nothing is read twice). Assertive is reserved
// for errors; the server-lost banner is its own role=alert.
let announceTimer = 0;
function announce(msg, assertive) {
  const el = $(assertive ? "srAlert" : "srStatus");
  if (!el || !msg) return;
  // Clearing first makes a repeated identical message announce again.
  el.textContent = "";
  clearTimeout(announceTimer);
  announceTimer = setTimeout(() => { el.textContent = String(msg); }, 40);
}

// ---- toast: transient, non-blocking feedback ----
// One toast host at a time (docs/ui-rebuild-contract.md §6.5): while the
// hosted viewer is mounted its own #toastHost shows everything (shell
// toasts are handed to FTViewer.toast); otherwise #hToastBox under the top
// strip, left of an open sheet. At most TOAST_MAX stacked; info/warn leave
// after TOAST_MS (hover or keyboard focus pauses the clock), errors stay
// until dismissed (close button or Esc). Esc dismisses the newest toast.
const TOAST_MAX = 3, TOAST_MS = 6000;
let toasts = [];
function toast(msg, kind) {
  announce(msg, kind === "err");
  // The deliverables sheet is modal: #main (and the viewer's toast host in
  // it) is inert, so a toast there could not be dismissed or clicked. While
  // it is open, toasts go to the shell's body-level box instead.
  const sheetModal = typeof FTStore === "object" && FTStore && FTStore.state && FTStore.state.panels && FTStore.state.panels.finalize;
  if (!sheetModal && typeof hViewer === "object" && hViewer.toast(msg, kind)) return null;
  const box = $("hToastBox");
  if (!box || typeof document.createElement !== "function") return null;
  const el = document.createElement("div");
  el.className = "h-toast" + (kind === "err" ? " err" : kind === "warn" ? " warn" : kind === "ok" ? " ok" : "");
  const text = document.createElement("span");
  text.className = "h-toast-msg";
  text.textContent = msg;
  const close = document.createElement("button");
  close.type = "button";
  close.className = "quiet icon h-toast-x";
  close.innerHTML = '<svg class="ic" aria-hidden="true"><use href="#i-close"/></svg>';
  close.title = "알림 닫기 (Esc)";
  close.setAttribute("aria-label", "알림 닫기");
  if (typeof el.append === "function") el.append(text, close); else { el.appendChild(text); el.appendChild(close); }
  const sticky = kind === "err";
  const rec = { el, sticky, timer: 0, left: TOAST_MS, since: 0, hover: false, focus: false };
  const arm = () => {
    if (sticky || rec.timer || rec.hover || rec.focus) return;
    rec.since = Date.now();
    rec.timer = setTimeout(() => dismissToast(rec), Math.max(800, rec.left));
  };
  const pause = () => {
    if (!rec.timer) return;
    clearTimeout(rec.timer);
    rec.timer = 0;
    rec.left -= Date.now() - rec.since;
  };
  rec.dismiss = () => dismissToast(rec);
  el.addEventListener("mouseenter", () => { rec.hover = true; pause(); });
  el.addEventListener("mouseleave", () => { rec.hover = false; arm(); });
  el.addEventListener("focusin", () => { rec.focus = true; pause(); });
  el.addEventListener("focusout", () => { rec.focus = false; arm(); });
  close.addEventListener("click", () => dismissToast(rec));
  box.appendChild(el);
  toasts.push(rec);
  // Over the cap: drop the oldest non-error first, errors last.
  while (toasts.length > TOAST_MAX) dismissToast(toasts.find(t => !t.sticky) || toasts[0]);
  arm();
  return rec;
}
function dismissToast(rec) {
  if (!rec) return;
  clearTimeout(rec.timer);
  rec.timer = 0;
  toasts = toasts.filter(t => t !== rec);
  try { rec.el.remove(); } catch (e) {}
}
// Esc with focus inside a toast (97-shell.js): that toast goes, and focus
// lands somewhere useful instead of on <body> — the newest remaining
// toast's close button, else the open deliverables sheet, else the hosted
// viewer (its active card), else #main.
function dismissToastEl(el) {
  const rec = toasts.find(t => t.el === el);
  if (!rec) return false;
  dismissToast(rec);
  const next = toasts[toasts.length - 1];
  const nextBtn = next && next.el && typeof next.el.querySelector === "function" ? next.el.querySelector("button") : null;
  if (nextBtn) { nextBtn.focus(); return true; }
  const sheetOpen = typeof FTStore === "object" && FTStore && FTStore.state && FTStore.state.panels && FTStore.state.panels.finalize;
  if (sheetOpen && typeof hFocusables === "function") {
    const f = hFocusables($("hFinalizeSheet"));
    if (f.length) { f[0].focus(); return true; }
  }
  if (window.FTViewer && typeof window.FTViewer.focus === "function" && window.FTViewer.focus()) return true;
  const main = $("main");
  if (main && typeof main.focus === "function") main.focus();
  return true;
}
function dismissNewestToast() {
  const rec = toasts[toasts.length - 1];
  if (!rec) return false;
  dismissToast(rec);
  return true;
}

// Busy state for a button: spinner + label, aria-busy, disabled.
function setBusy(btn, busy, label) {
  if (!btn) return;
  if (busy) {
    if (btn.dataset && btn.dataset.idleLabel === undefined) btn.dataset.idleLabel = btn.textContent;
    if (label) btn.textContent = label;
    btn.setAttribute("aria-busy", "true");
    btn.disabled = true;
  } else {
    btn.removeAttribute("aria-busy");
    if (btn.dataset && btn.dataset.idleLabel !== undefined) {
      if (label !== false) btn.textContent = btn.dataset.idleLabel;
      delete btn.dataset.idleLabel;
    }
  }
}

// ---- in-app dialog (replaces blocking confirm/alert) ----
// askDialog resolves true on OK, false on cancel/Escape. `onOk` may return
// false (or a Promise of false) to keep the dialog open, e.g. when a
// required field is still empty.
let dlgResolve = null, dlgOnOk = null, dlgLastFocus = null;
function askDialog({ title, html, okLabel, cancelLabel, danger, onOk, focus }) {
  if (dlgResolve) closeDialog(false);
  dlgLastFocus = document.activeElement;
  $("dlgTitle").textContent = title || "";
  $("dlgBody").innerHTML = html || "";
  $("dlgNote").textContent = "";
  $("dlgOk").textContent = okLabel || "확인";
  $("dlgOk").disabled = false;
  $("dlgOk").classList.toggle("destructive", !!danger);
  $("dlgCancel").textContent = cancelLabel || "취소";
  $("dlgCancel").hidden = cancelLabel === null;
  $("dlgBackdrop").hidden = false;
  dlgOnOk = onOk || null;
  setTimeout(() => {
    const target = (focus && $(focus)) || $("dlgOk");
    if (target && target.focus) target.focus();
  }, 0);
  return new Promise(resolve => { dlgResolve = resolve; });
}
function closeDialog(result) {
  $("dlgBackdrop").hidden = true;
  const resolve = dlgResolve;
  dlgResolve = null; dlgOnOk = null;
  if (dlgLastFocus && dlgLastFocus.focus) { try { dlgLastFocus.focus(); } catch (e) {} }
  if (resolve) resolve(result);
}
function alertDialog(title, msg) {
  return askDialog({ title, html: `<p>${esc(msg)}</p>`, okLabel: "확인", cancelLabel: null });
}
$("dlgOk").addEventListener("click", async () => {
  if (dlgOnOk) {
    $("dlgOk").disabled = true;
    $("dlgOk").setAttribute("aria-busy", "true");
    let keep = false;
    try { keep = (await dlgOnOk()) === false; } catch (e) { keep = true; $("dlgNote").textContent = "오류: " + (e.message || e); }
    $("dlgOk").removeAttribute("aria-busy");
    $("dlgOk").disabled = false;
    if (keep) return;
  }
  closeDialog(true);
});
$("dlgCancel").addEventListener("click", () => closeDialog(false));
// Document-level so Escape / Tab still work after a click on the backdrop
// or any non-focusable part of the dialog moved focus to <body>.
document.addEventListener("keydown", e => {
  if ($("dlgBackdrop").hidden) return;
  if (e.key === "Escape") { e.preventDefault(); closeDialog(false); return; }
  if (e.key !== "Tab") return;
  const focusables = [...$("dlgBackdrop").querySelectorAll("button, input, a[href], [tabindex='0']")]
    .filter(el => !el.disabled && !el.hidden && el.offsetParent !== null);
  if (!focusables.length) return;
  const first = focusables[0], last = focusables[focusables.length - 1];
  const inside = $("dlgBackdrop").contains(document.activeElement);
  if (!inside) { e.preventDefault(); (e.shiftKey ? last : first).focus(); }
  else if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
});

