// ---- /api/status polling -> FTStore (T3, docs/ui-rebuild-contract.md §4) ----
// There are no stages any more: this file only reads /api/status, keeps the
// shell store current and fills the idle-time pieces that depend on it
// (start-form lock, start error box, server-lost banner). Rendering of the
// analysis chip lives in 31-progress.js, the work sheet / case jobs / log in
// 32-jobs.js; both subscribe to FTStore and never poll themselves.
//
// Cadence (contract §4): 1 s while any job is active (pipeline, enrichment,
// recover/carve, clip export), 5 s otherwise; backs off (<= 10 s) while the
// server is unreachable.
const POLL_BUSY_MS = 1000, POLL_IDLE_MS = 5000;

// Phases the viewer can be mounted in: the index exists (docs §3.1).
const REVIEW_PHASES = ["review-partial", "review-ready", "done", "finalized"];
function caseBaseName(caseDir) {
  // R20 U5: `E:\.` → "E:" (a trailing `\.` is the folder itself).
  // R21 U2: linear loop, same result as 01-store hTrimTail.
  const s = String(caseDir || "");
  const sep = (c) => c === "\\" || c === "/";
  let end = s.length;
  for (;;) {
    if (end > 0 && sep(s[end - 1])) { end--; continue; }
    if (end > 1 && s[end - 1] === "." && sep(s[end - 2])) { end--; continue; }
    break;
  }
  return s.slice(0, end).split(/[\\/]/).filter(Boolean).pop() || "";
}

// Store access is optional so a page without the store (legacy bundle,
// tests) still polls and fills the plain-DOM pieces below.
function patchStore(patch) {
  if (typeof FTStore === "object" && FTStore && typeof FTStore.set === "function") FTStore.set(patch);
}
function storeState() {
  return typeof FTStore === "object" && FTStore && FTStore.state ? FTStore.state : null;
}

// Controls that loadEnv() disabled for a missing tool carry
// data-env-off and must stay disabled when a finished run re-enables the
// form (otherwise the E01 radio comes back without libewf installed).
function setInputsDisabled(disabled) {
  // The evidence form and the recent-project buttons lock while a job
  // runs; the viewer stays usable during enrichment.
  document.querySelectorAll("#blkEvidence input, #blkEvidence select, #blkEvidence button, #blkRecent button").forEach(el => {
    el.disabled = disabled || (el.dataset && el.dataset.envOff === "1");
  });
  if (!disabled && $("directE01").checked) $("skipE01Verify").disabled = true;
}
function setEnvOff(el, off) {
  if (!el) return;
  el.disabled = off;
  if (el.dataset) { if (off) el.dataset.envOff = "1"; else delete el.dataset.envOff; }
}

function fmtBytes(value) {
  if (!Number.isFinite(value)) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let current = value;
  let unit = 0;
  while (current >= 1024 && unit < units.length - 1) { current /= 1024; unit += 1; }
  return `${current.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function fmtEta(secs) {
  if (secs >= 3600) return `약 ${Math.round(secs / 3600)}시간 ${Math.round((secs % 3600) / 60)}분`;
  if (secs >= 60) return `약 ${Math.round(secs / 60)}분`;
  return `약 ${secs}초`;
}

// m:ss (h:mm:ss from one hour).
function fmtClock(secs) {
  const s = Math.max(0, Math.floor(Number(secs) || 0));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
  return (h ? `${h}:${String(m).padStart(2, "0")}` : String(m)) + ":" + String(r).padStart(2, "0");
}

// finalizing / auditVerified belong to the finalize sheet (T4): it assigns
// them; they stay declared here because 50-start.js and the status
// handler below read them before 40-finalize.js loads.
let finalizing = false;
let auditVerified = false;
// auditVerdict (R8, the one reading of /api/verify-audit) lives in
// 41-finalize.js — called at run time, after every part loaded.
async function verifyAudit() {
  const badge = $("auditBadge");
  if (!badge) return;
  try {
    const v = auditVerdict(await jpost("/api/verify-audit", {}));
    badge.className = "badge " + v.level;
    badge.textContent = v.text;
    badge.title = v.title;
  } catch (e) { badge.className = "badge bad"; badge.textContent = "감사 로그 검증 불가"; }
  if (typeof syncToolsBtn === "function") syncToolsBtn();
}

// Maps raw pipeline errors to a first-run action hint — the log keeps the
// exact CLI output, this answers "what do I do now?".
function errorHint(msg) {
  const m = msg.toLowerCase();
  if (/ewfinfo|ewfverify|ewfexport|libewf/.test(m))
    return "E01 처리에는 libewf 도구가 필요합니다 — 상단 ‘도구’에서 libewf [설치]를 누르십시오.";
  if (/mmls|fls|icat|sleuth/.test(m))
    return "파일시스템 조사에는 Sleuth Kit이 필요합니다 — 상단 ‘도구’에서 Sleuth Kit [설치]를 누르십시오.";
  if (/ffmpeg|ffprobe/.test(m))
    return "ffmpeg/ffprobe가 필요합니다 — 상단 ‘도구’에서 FFmpeg [설치]를 누르십시오.";
  if (/tool not found|not found|cannot find|실행 파일/.test(m))
    return "필요한 외부 도구를 찾지 못했습니다 — 상단 도구 배지에서 누락 항목을 확인하십시오.";
  if (/space|disk|공간|저장소/.test(m))
    return "디스크 공간이 부족합니다 — 케이스 폴더 드라이브의 여유 공간을 확보하십시오.";
  if (/찾을 수 없|no such|not exist|존재하지 않/.test(m))
    return "입력 경로를 확인하십시오 — 파일이나 폴더가 이동·삭제되었을 수 있습니다.";
  if (/corrupt|damaged|손상|invalid/.test(m))
    return "입력 이미지가 손상되었거나 형식이 다릅니다 — 원본을 다시 확인하거나 다른 소스로 시도하십시오.";
  return "로그에서 실패한 단계를 확인하십시오 — 입력을 바로잡은 뒤 '분석 시작'으로 재시도할 수 있습니다.";
}
// R5 (S6 ↔ U): a raw image whose completion marker no longer matches (an
// old case, a moved case) is NOT deleted any more — the server stops with
// "raw 불일치 — 재임포트 필요" and the examiner decides. The error box then
// carries the one action that resolves it: POST /api/reimport-raw.
// /api/status.raw_reimport = {raw_path, reason, endpoint} while one waits (else null).
let hRawReimport = null;
// R6 U3: the pipeline is busy (/api/status.busy) — the button is disabled.
let hStatusBusy = false;
function rawMismatchError(text) {
  return /raw\s*불일치|재임포트|reimport[-_ ]?raw|raw[-_ ]mismatch/i.test(String(text || ""));
}
// Action hint first (what to do now), raw CLI output behind a disclosure.
// R6 U3: the [원본 다시 가져오기] button only while the server names a raw
// image waiting for that decision (hRawReimport) — an error text alone gets
// the hint, not an action the server would refuse. Its state is part of the
// markup, so a re-render (poll) keeps "다시 가져오는 중…" and a busy
// pipeline keeps it disabled.
function errorHtml(raw) {
  const text = String(raw || "");
  const first = text.split(/\r?\n/)[0].slice(0, 240);
  const action = !!hRawReimport && (rawMismatchError(text) || /raw/i.test(text));
  const rawWait = action || rawMismatchError(text);
  const hint = action
    ? "보관된 원본 이미지(raw)가 기록과 다릅니다 — 지우지 않았습니다. ‘원본 다시 가져오기’를 누르면 증거 이미지에서 다시 만듭니다."
    : rawWait ? "보관된 원본 이미지(raw)가 기록과 다릅니다 — 지우지 않았습니다. 분석을 다시 시작하면 서버가 다시 확인합니다."
    : errorHint(text);
  const busy = reimportRawBusy;
  const disabled = busy || hStatusBusy;
  const btn = `<button type="button" data-reimport-raw${disabled ? " disabled" : ""}${busy ? ' aria-busy="true"' : ""}${hStatusBusy && !busy ? ' title="작업이 진행 중입니다 — 끝난 뒤 누를 수 있습니다"' : ""}>${busy ? "다시 가져오는 중…" : "원본 다시 가져오기"}</button>`;
  return `<div>오류: ${esc(first)}</div><div class="hintline">${esc(hint)}</div>` +
    (action ? `<div class="err-acts">${btn}</div>` : "") +
    (text.length > first.length ? `<details><summary>자세한 오류 내용</summary><pre>${esc(text)}</pre></details>` : "");
}
// One delegated handler for every error box that shows the button. A
// server without the route (older build) says so instead of failing silently.
var reimportRawBusy = false;
var reimportRawAsking = false;
// R7 U7: the confirmation can stay open for a while — the pipeline may
// have started a job, or the server may no longer wait for (or now wait
// for a different) raw decision. Re-read /api/status after the examiner
// confirms; anything changed cancels (said so), nothing is deleted on a
// stale decision.
async function hReimportRawStillValid(pending) {
  let st = null;
  try { st = await jget("/api/status"); } catch (e) { st = null; }
  if (!st || typeof st !== "object" || st.ok === false || st.network) return "서버 상태를 다시 확인하지 못해 원본 다시 가져오기를 취소했습니다 — 잠시 후 다시 시도하십시오.";
  hStatusBusy = !!st.busy;
  hRawReimport = st.raw_reimport && typeof st.raw_reimport === "object" ? st.raw_reimport : null;
  if (st.busy) return "그 사이 작업이 시작되어 원본 다시 가져오기를 취소했습니다 — 작업이 끝난 뒤 다시 누르십시오.";
  const now = hRawReimport;
  if (!now || String(now.raw_path || "") !== String((pending && pending.raw_path) || "")) {
    return "그 사이 서버의 원본 결정 대기 상태가 바뀌어 원본 다시 가져오기를 취소했습니다 — 화면의 현재 상태를 확인하십시오.";
  }
  return "";
}
async function hReimportRaw(btn) {
  if (reimportRawBusy || reimportRawAsking || hStatusBusy || !hRawReimport) return;
  const pending = hRawReimport;
  // It deletes the kept raw image and makes it again from the evidence: ask.
  reimportRawAsking = true;
  let go = false;
  try {
    go = await askDialog({
      title: "원본 이미지 다시 가져오기",
      html: `<p>보관된 원본 이미지(raw)가 기록과 다릅니다${pending && pending.reason ? ` (${esc(String(pending.reason))})` : ""}.</p>` +
        (pending && pending.raw_path ? `<p><code>${esc(String(pending.raw_path))}</code></p>` : "") +
        `<p>계속하면 이 raw 파일을 <b>삭제</b>하고(감사 로그에 기록) 증거 이미지(E01)에서 다시 만든 뒤 같은 분석을 다시 실행합니다.</p>`,
      okLabel: "삭제 후 다시 가져오기",
      cancelLabel: "취소",
      danger: true,
      focus: "dlgCancel",
    });
    if (go) {
      const stale = await hReimportRawStillValid(pending);
      if (stale) {
        go = false;
        toast(stale, "warn");
        if (typeof poll === "function") { clearTimeout(pollTimer); poll(); }
      }
    }
  } finally {
    reimportRawAsking = false;
  }
  if (!go) return;
  reimportRawBusy = true;
  const live = () => (btn && btn.isConnected ? btn : document.querySelector("button[data-reimport-raw]"));
  const b0 = live();
  if (b0) { b0.disabled = true; b0.setAttribute("aria-busy", "true"); b0.textContent = "다시 가져오는 중…"; }
  let res = null;
  const body = pending && pending.raw_path ? { raw_path: String(pending.raw_path) } : {};
  const endpoint = pending && pending.endpoint === "/api/reimport-raw" ? pending.endpoint : "/api/reimport-raw";
  try { res = await jpost(endpoint, body); } catch (e) { res = { ok: false, error: String((e && e.message) || e) }; }
  reimportRawBusy = false;
  const b1 = live();
  if (b1) { b1.disabled = hStatusBusy; b1.removeAttribute("aria-busy"); b1.textContent = "원본 다시 가져오기"; }
  if (res && res.ok) {
    toast("원본 이미지를 다시 가져오는 작업을 시작했습니다 — 작업 시트(Alt+J)에서 진행을 볼 수 있습니다.", "ok");
    if (typeof poll === "function") { clearTimeout(pollTimer); poll(); }
  } else if (res && (Number(res.status) === 404 || Number(res.status) === 405)) {
    toast("이 서버 버전은 원본 다시 가져오기를 지원하지 않습니다 — FrameTrace를 업데이트한 뒤 다시 시도하십시오.", "err");
  } else {
    toast("원본 다시 가져오기 실패: " + ((res && res.error) || "알 수 없는 오류"), "err");
  }
}
document.addEventListener("click", ev => {
  const btn = ev.target && ev.target.closest ? ev.target.closest("button[data-reimport-raw]") : null;
  if (btn) hReimportRaw(btn);
});

// ---- server-lost banner (UX-05) ----
// A lost server is a connection state, not a phase: the store keeps the
// last good phase / project, so the review screen, an open sheet and the
// mounted viewer stay exactly as they were, and only server.lost goes up.
// The banner text is set once when the connection drops (not per failed
// poll, so the role=alert region is not re-announced every few seconds);
// the next good poll takes it down again.
let pollFailures = 0;
let lastInstance = null;
// When the last good /api/status arrived (the clip chip's elapsed clock).
// Deliberately NOT in the store: a per-poll timestamp would make "jobs"
// change on every poll and wake every jobs listener for nothing. `var` so
// 31-progress.js may read it before this line has run.
var pollStatusAt = 0;
// The banner has two faces: "server lost" (connection refused / timed out)
// and "authorization refused" (U8: the server answered 401 — it is up, but
// this page's ft_token / ft_nonce cookie is missing, or another FrameTrace
// instance on this computer replaced it). The auth face is driven by WHAT
// is failing, not by /api/status: in no-token mode a GET needs no proof, so
// a good status poll says nothing about saves. Kinds (authFailing):
//   "read"  a GET was refused (token mode) — cleared by a good GET
//   "write" a shell POST was refused — cleared by a good shell POST
//   "save"  the hosted viewer's save was refused — cleared only when a
//           viewer save gets through (FTViewer "save-state" auth:false)
// A lost connection takes precedence; the auth face returns after it.
let bannerAuth = false;     // the banner currently shows the auth face
let connLost = false;
let connDetail = "";
const authFailing = new Set();
const BANNER_LOST = { title: "", button: "" };
const AUTH_DETAIL = "같은 컴퓨터에서 다른 FrameTrace 창이 열려 있으면 닫고 이 창을 새로 고치십시오. 서버는 응답하지만 이 창의 인증 쿠키가 없거나 다른 창의 것으로 바뀌어 요청이 거부되었습니다 (HTTP 401). 판독 내용은 이 브라우저에 보존됩니다.";
const AUTH_GONE_DETAIL = "이 창에는 인증 정보가 남아 있지 않아 새로 고쳐도 열리지 않습니다 — FrameTrace 앱 창(또는 앱이 연 주소)에서 다시 여십시오. 판독 내용은 이 브라우저에 보존됩니다.";
function authTitle() {
  return authFailing.has("save") || authFailing.has("write") ? "저장이 인증 오류로 거부되었습니다" : "요청이 인증 오류로 거부되었습니다";
}
function bannerFace(auth) {
  const banner = $("serverBanner");
  const title = banner && typeof banner.querySelector === "function" ? banner.querySelector("b") : null;
  const button = $("btnServerRetry");
  if (!BANNER_LOST.title && title) { BANNER_LOST.title = title.textContent; BANNER_LOST.button = button ? button.textContent : ""; }
  const nextTitle = auth ? authTitle() : BANNER_LOST.title;
  // Only real changes: rewriting the role=alert text re-announces it.
  if (title && BANNER_LOST.title && title.textContent !== nextTitle) title.textContent = nextTitle;
  if (button && BANNER_LOST.button && bannerAuth !== auth) {
    button.textContent = auth ? "이 창 새로 고침" : BANNER_LOST.button;
    button.hidden = false;
    button.disabled = false;
  }
  bannerAuth = auth;
}
function setBannerMsg(text) {
  if (!text) return;
  $("serverBannerMsg").textContent = text;
  $("serverBannerMsg").title = text;
}
function renderServerBanner() {
  const banner = $("serverBanner");
  const was = !banner.hidden;
  const auth = !connLost && authFailing.size > 0;
  const show = connLost || auth;
  banner.hidden = !show;
  if (show && (auth !== bannerAuth || !was)) { bannerFace(auth); setBannerMsg(auth ? AUTH_DETAIL : connDetail); }
  else if (show && auth) bannerFace(true); // the title follows the failing kinds
  if (!show && bannerAuth) bannerFace(false);
  const S = storeState();
  if (S && (!!S.server.lost !== show || !!S.server.auth !== auth)) {
    patchStore({ server: Object.assign({}, S.server, { lost: show, auth, failures: connLost ? pollFailures : 0 }) });
  }
}
function setServerLost(lost, detail) {
  // The detail is set once when the connection drops (not per failed poll).
  if (lost && detail && !connLost) connDetail = detail;
  connLost = !!lost;
  if (!lost) connDetail = "";
  renderServerBanner();
}
// readJson (20-net.js) calls this on any 401 (kind "read" / "write"), the
// viewer bridge on a refused save (22-viewer-host, kind "save"). A kind
// that is already failing changes nothing (no re-announce).
function noteAuthExpired(kind) {
  const k = kind || "read";
  if (authFailing.has(k)) return;
  const first = authFailing.size === 0;
  authFailing.add(k);
  renderServerBanner();
  if (first) announce(authTitle() + " — 다른 FrameTrace 창이 열려 있으면 닫고 이 창을 새로 고치십시오", true);
}
// A request of that kind got through: its proof works again.
function noteAuthOk(kind) {
  if (!authFailing.delete(kind || "read")) return;
  renderServerBanner();
}
// [이 창 새로 고침] (auth face). Loading "/" re-plants the nonce in no-token
// mode and still carries the ft_token cookie in token mode (the launch
// ?token= was stripped from the address, 99-init.js). If the token cookie
// itself is gone, "/" answers 401 — reloading would leave a dead page, so
// probe first and then say where to reopen instead.
async function reloadForAuth() {
  const button = $("btnServerRetry");
  if (button) button.disabled = true;
  let status = 0;
  try {
    const r = await fetch("/", { cache: "no-store", credentials: "same-origin" });
    status = r.status;
  } catch (e) { status = 0; }
  if (status === 401 || status === 403) {
    setBannerMsg(AUTH_GONE_DETAIL);
    if (button) button.hidden = true;
    announce("이 창에는 인증 정보가 없습니다 — FrameTrace 앱 창에서 다시 여십시오", true);
    return;
  }
  if (button) button.disabled = false;
  window.location.assign("/");
}
function pollFailed(err) {
  if (err && err.status === 401) { noteAuthExpired("read"); return; }
  pollFailures += 1;
  if (pollFailures >= 3) {
    setServerLost(true,
      "frametrace-app이 실행 중인지 확인하십시오. 화면과 판독 내용은 그대로 유지되고, 연결되면 자동으로 이어집니다." +
      (err && err.message ? ` (${err.message})` : ""));
  }
}

let shuttingDown = false;
let pollTimer = 0;
let pollInFlight = false;
function schedulePoll(ms) {
  clearTimeout(pollTimer);
  // Back off while the server is unreachable so a dead server is not
  // hammered, but never longer than 10 s so reconnects stay snappy.
  const delay = pollFailures > 0 ? Math.min(10000, 1200 * Math.pow(1.6, Math.min(pollFailures, 6))) : ms;
  pollTimer = setTimeout(poll, delay);
}
// A job is "active" when the next status should arrive within a second:
// the pipeline / enrichment, a recover/carve job (or its POST still out),
// or a clip export.
function pollActive(st) {
  if (!st) return false;
  if (st.busy || Number(st.clips_running) > 0) return true;
  return typeof caseJob !== "undefined" && !!caseJob && !caseJob.outcome;
}
let lastStatus = null;
let lastHomeKey = null;
async function poll() {
  if (shuttingDown || pollInFlight) return;
  pollInFlight = true;
  let st;
  try {
    st = await jget("/api/status");
  } catch (err) {
    pollInFlight = false;
    pollFailed(err);
    shellOnStatus(null);
    schedulePoll(POLL_BUSY_MS);
    return;
  }
  pollInFlight = false;
  if (pollFailures >= 3) toast("서버에 다시 연결되었습니다.");
  pollFailures = 0;
  setServerLost(false);
  if (lastInstance && st.instance && st.instance !== lastInstance) {
    toast("서버가 다시 시작되었습니다 — 화면 상태를 새로 고칩니다.");
  }
  if (st.instance) lastInstance = st.instance;
  lastStatus = st;
  try {
    caseJobOnStatus(st);
  } catch (err) {
    console.error("case job update failed:", err);
  }
  try {
    applyStatus(st);
  } catch (err) {
    console.error("status render failed:", err);
  }
  shellOnStatus(st);
  try {
    // A phase change (analysis finished, case opened) refreshes the
    // recent-project list so it is current before the examiner returns.
    const homeKey = `${st.case_dir || ""}|${st.phase || ""}|${!!st.busy}`;
    if (homeKey !== lastHomeKey) {
      if (lastHomeKey !== null && typeof renderRecentCases === "function") renderRecentCases();
      lastHomeKey = homeKey;
    }
  } catch (err) {
    console.error("recent list update failed:", err);
  }
  schedulePoll(pollActive(st) ? POLL_BUSY_MS : POLL_IDLE_MS);
}
$("btnServerRetry").addEventListener("click", () => {
  if (bannerAuth) { reloadForAuth(); return; }
  pollFailures = Math.min(pollFailures, 2); schedulePoll(0); poll();
});
// The idle cadence is 5 s: starting or opening something from the empty
// state must not wait that long for the first status, so a click inside the
// input card re-polls shortly after (the poll then switches to 1 s itself).
document.addEventListener("click", e => {
  const t = e && e.target;
  if (!t || typeof t.closest !== "function" || !t.closest("#inputCard")) return;
  if (lastStatus && pollActive(lastStatus)) return;
  schedulePoll(700);
});

// The integrated screen (97-shell.js, T2) renders its mode from the store
// once a status (or a failed poll, null) has arrived.
function shellOnStatus(st) {
  if (typeof hOnStatus !== "function") return;
  try { hOnStatus(st); } catch (err) { console.error("shell render failed:", err); }
}

// Server phase -> store phase (contract §4). "idle" (no job, no case) is
// "empty"; "finalizing" is the review phase with busy set; a sealed case
// (done + package) is "finalized".
// R11 U4 (track S6): the server's `package_incomplete` is its verdict on
// `package_dir` (true counts even without a renamed folder); absent (older
// server) = the folder name.
function stPackageIncomplete(st) {
  if (!st) return false;
  if (typeof st.package_incomplete === "boolean") return st.package_incomplete;
  return !!st.package_dir && packageIncompleteName(st.package_dir);
}
function storePhase(st) {
  if (!st || !st.has_job) return "empty";
  const p = String(st.phase || "");
  if (p === "finalizing") return "review-ready";
  // R10 U4: a package whose finalize did not complete is not "finalized".
  const incomplete = stPackageIncomplete(st);
  if ((p === "done" || p === "finalized") && incomplete) return "done";
  if (p === "done" && st.package_dir) return "finalized";
  return ["running", "review-partial", "review-ready", "done", "finalized", "error"].includes(p) ? p : "empty";
}

// index_generation only moves when the live index changed; the live total
// (what "색인 n/m" shows as m) is one tiny paged request, so it is read
// when the generation moved, at most every 3 s, and only while an index is
// being built.
let totalProbe = { gen: null, at: 0, busy: false };
async function refreshTotalCount(st) {
  const building = st.phase === "running" || (st.phase === "review-partial" && st.busy);
  if (!building || !st.case_dir || totalProbe.busy) return;
  const gen = st.index_generation == null ? "" : String(st.index_generation);
  if (totalProbe.gen === gen && Date.now() - totalProbe.at < 30000) return;
  if (Date.now() - totalProbe.at < 3000) return;
  totalProbe.busy = true;
  totalProbe.at = Date.now();
  try {
    const res = await jgetSoft("/api/records?offset=0&limit=1&live=1");
    const total = res && res.ok ? Number(res.total) : NaN;
    if (Number.isFinite(total)) {
      totalProbe.gen = gen;
      const S = storeState();
      if (S && S.counts.total !== total) patchStore({ counts: Object.assign({}, S.counts, { total }) });
    }
  } catch (e) { /* the count is a hint */ } finally { totalProbe.busy = false; }
}

// The viewer announces how many records it holds (`records`); the chip's
// "n" is that number. FTViewer lives in the other bundle, so hook lazily.
let hostHooked = false;
function hookHostEvents() {
  if (hostHooked || typeof FTViewer !== "object" || !FTViewer || typeof FTViewer.on !== "function") return;
  hostHooked = true;
  FTViewer.on("records", ev => {
    const S = storeState();
    const d = (ev && ev.detail) || {};
    if (S && Number.isFinite(Number(d.total)) && S.counts.indexed !== Number(d.total)) {
      patchStore({ counts: Object.assign({}, S.counts, { indexed: Number(d.total) }) });
    }
  });
}

let prevCaseDir = null, prevPhaseKey = "";
function applyStatus(st) {
  hookHostEvents();
  const S = storeState();
  const phase = storePhase(st);
  // A different case, or a fresh run, starts a fresh log.
  const caseDir = st.case_dir || "";
  if (prevCaseDir !== null && prevCaseDir !== caseDir) resetLogView();
  if (prevPhaseKey && prevPhaseKey !== "running" && st.phase === "running") resetLogView();
  prevCaseDir = caseDir;
  prevPhaseKey = String(st.phase || "");
  appendLogLines(st.logs || []);

  const pipeline = {
    phase: String(st.phase || ""),
    steps: st.steps || [],
    stepNames: st.step_names || [],
    current: st.current || "",
    progress: st.progress || null,
    enrichment: st.enrichment || null,
    warnings: st.warnings || [],
    logs: st.logs || [],
    busy: !!st.busy,
    error: st.error || "",
  };
  const prev = S ? S.jobs : {};
  const jobs = Object.assign({}, prev, {
    pipeline,
    caseJob: typeof caseJobSnapshot === "function" ? caseJobSnapshot() : null,
    clipJobs: Array.isArray(st.clip_jobs) ? st.clip_jobs : [],
    clipsRunning: Number(st.clips_running) || 0,
    pendingMarks: Number(st.pending_marks) || 0,
    jobKind: st.job_kind || null,
  });
  pollStatusAt = Date.now();
  if (S) {
    patchStore({
      phase,
      project: Object.assign({}, S.project, {
        caseDir,
        name: caseBaseName(caseDir),
        packageDir: st.package_dir || "",
        packageIncomplete: stPackageIncomplete(st),
        restored: !!st.restored,
      }),
      jobs,
      counts: Object.assign({}, S.counts, { generation: Number(st.index_generation) || 0 }),
      server: Object.assign({}, S.server, { instance: st.instance || S.server.instance || "" }),
    });
  }
  refreshTotalCount(st);

  // ---- plain-DOM pieces that follow the status ----
  const partial = st.phase === "review-partial";
  const enriching = partial && !!st.busy;
  const running = st.phase === "running" || enriching;
  setInputsDisabled(running);
  $("btnStart").disabled = running || finalizing;
  $("btnStart").title = running ? "분석이 진행 중입니다 — 작업 시트(Alt+J)에서 상황을 보거나 중단할 수 있습니다"
    : finalizing ? "보고서 생성이 진행 중입니다" : "";
  if (REVIEW_PHASES.includes(st.phase) && !auditVerified && st.phase !== "review-partial") { auditVerified = true; verifyAudit(); }
  // Recover/Carve only make sense once a case is open for review.
  setCaseToolsVisible(!!caseDir && (REVIEW_PHASES.includes(st.phase) || st.phase === "finalizing"));
  setCaseJobButtons(!!st.busy || (typeof caseJob !== "undefined" && !!caseJob && !caseJob.outcome));

  // R5 (S6): the server names a raw image waiting for the examiner's decision.
  hRawReimport = st.raw_reimport && typeof st.raw_reimport === "object" ? st.raw_reimport : null;
  hStatusBusy = !!st.busy;
  const errEl = $("startErr");
  if (errEl) {
    if (st.error && st.phase !== "review-partial" && st.phase !== "review-ready" && st.phase !== "done" && st.phase !== "finalized") {
      // Re-render only when the markup would change or something else
      // replaced the box, so an expanded details block (and any selection
      // in it) survives polls. R6 U3: the markup includes the reimport
      // button's state (shown / busy / disabled), so a change re-renders.
      const html = errorHtml(st.error);
      if (shownStatusSrc !== html || errEl.innerHTML !== shownStatusHtml || !/\bshow\b/.test(errEl.className)) {
        errEl.classList.add("show");
        errEl.innerHTML = html;
        shownStatusError = st.error;
        shownStatusSrc = html;
        shownStatusHtml = errEl.innerHTML;
      }
    } else {
      shownStatusError = null;
      errEl.classList.remove("show");
    }
  }
}
let shownStatusError = null, shownStatusHtml = "", shownStatusSrc = "";
