const STEP_NAMES = ["케이스 준비", "소스 등록", "스캔 · 색인", "재생성 검증", "리뷰 생성"];

// Reads a fetch Response as JSON without ever throwing: HTTP errors,
// non-JSON bodies (a proxy error page, a truncated reply) and network
// failures all collapse to { ok:false, error } so callers have exactly one
// failure shape to render.
async function readJson(r) {
  let data = null;
  try { data = await r.json(); } catch (e) { data = null; }
  if (!r || !r.ok) {
    const status = r && r.status ? `HTTP ${r.status}` : "응답 없음";
    return Object.assign({}, data && typeof data === "object" ? data : {}, {
      ok: false,
      // HTTP status kept so callers can tell "endpoint not in this server
      // build" (404/405 — degrade quietly) from a real failure.
      status: (r && r.status) || 0,
      error: (data && data.error) || `서버 오류 (${status})`,
    });
  }
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
  const data = await readJson(r);
  if (!(r && r.ok) || data.malformed) throw new Error(data.error);
  return data;
}
// Like jpost for GET: never throws, keeps the HTTP status on failure.
async function jgetSoft(url) {
  try {
    const r = await fetch(url, { cache: "no-store" });
    return await readJson(r);
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
    return await readJson(r);
  } catch (err) {
    return netError(err);
  }
}

// ---- toast: transient, non-blocking feedback ----
function toast(msg, kind) {
  const box = $("toastBox");
  if (!box || typeof document.createElement !== "function") return;
  const el = document.createElement("div");
  el.className = "toast" + (kind === "err" ? " err" : kind === "warn" ? " warn" : "");
  el.textContent = msg;
  box.appendChild(el);
  setTimeout(() => el.remove(), kind === "err" ? 7000 : 3500);
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
  $("dlgOk").style.background = danger ? "var(--danger)" : "";
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
    let keep = false;
    try { keep = (await dlgOnOk()) === false; } catch (e) { keep = true; $("dlgNote").textContent = "오류: " + (e.message || e); }
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

