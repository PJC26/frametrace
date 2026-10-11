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
  return String(caseDir || "").replace(/[\\/]+$/, "").split(/[\\/]/).filter(Boolean).pop() || "";
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
async function verifyAudit() {
  const badge = $("auditBadge");
  if (!badge) return;
  try {
    const res = await jpost("/api/verify-audit", {});
    if (!res.ok) { badge.className = "badge bad"; badge.textContent = "감사 로그 검증 실패"; if (typeof syncToolsBtn === "function") syncToolsBtn(); return; }
    const keyed = res.integrity === "integrity-keyed";
    badge.className = "badge ok";
    badge.textContent = `감사 로그 ${keyed ? "키 서명 검증됨" : "구조 검증됨"} (${res.verified}개 로그, ${res.entries}항목)`;
    badge.title = (res.logs || []).map(l => `${l.name}: ${l.integrity}, ${l.entries}항목`).join("\n");
  } catch (e) { badge.className = "badge bad"; badge.textContent = "감사 로그 검증 불가"; }
  if (typeof syncToolsBtn === "function") syncToolsBtn();
}

// Maps raw pipeline errors to a first-run action hint — the log keeps the
// exact CLI output, this answers "what do I do now?".
function errorHint(msg) {
  const m = msg.toLowerCase();
  if (/ewfinfo|ewfverify|ewfexport|libewf/.test(m))
    return "E01 처리에는 libewf 도구가 필요합니다 — tools/bin에 ewf*.exe를 넣거나 PATH에 설치하십시오.";
  if (/mmls|fls|icat|sleuth/.test(m))
    return "파일시스템 조사에는 Sleuth Kit이 필요합니다 — tools/bin에 mmls/fls/icat.exe를 넣거나 PATH에 설치하십시오.";
  if (/ffmpeg|ffprobe/.test(m))
    return "ffmpeg/ffprobe가 필요합니다 — tools/bin에 복사하거나 PATH에 설치하십시오.";
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
// Action hint first (what to do now), raw CLI output behind a disclosure.
function errorHtml(raw) {
  const text = String(raw || "");
  const first = text.split(/\r?\n/)[0].slice(0, 240);
  return `<div>오류: ${esc(first)}</div><div class="hintline">${esc(errorHint(text))}</div>` +
    (text.length > first.length ? `<details><summary>자세한 오류 내용</summary><pre>${esc(text)}</pre></details>` : "");
}

// ---- server-lost banner (UX-05) ----
let pollFailures = 0;
let lastInstance = null;
function setServerLost(lost, detail) {
  const banner = $("serverBanner");
  banner.hidden = !lost;
  if (lost && detail) $("serverBannerMsg").textContent = detail;
  const S = storeState();
  if (S && (!!S.server.lost !== !!lost || S.server.failures !== pollFailures)) {
    patchStore({ server: Object.assign({}, S.server, { lost: !!lost, failures: pollFailures }) });
  }
}
function pollFailed(err) {
  pollFailures += 1;
  if (pollFailures >= 3) {
    setServerLost(true,
      `frametrace-app이 실행 중인지 확인하십시오 — ${pollFailures}회 연속 응답 없음, 자동으로 다시 연결을 시도합니다.` +
      (err && err.message ? ` (${err.message})` : ""));
    const S = storeState();
    if (S && S.phase !== "server-lost") patchStore({ phase: "server-lost" });
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
$("btnServerRetry").addEventListener("click", () => { pollFailures = Math.min(pollFailures, 2); schedulePoll(0); poll(); });
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
function storePhase(st) {
  if (!st || !st.has_job) return "empty";
  const p = String(st.phase || "");
  if (p === "finalizing") return "review-ready";
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
    statusAt: Date.now(),
  });
  if (S) {
    patchStore({
      phase,
      project: Object.assign({}, S.project, {
        caseDir,
        name: caseBaseName(caseDir),
        packageDir: st.package_dir || "",
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

  const errEl = $("startErr");
  if (errEl) {
    if (st.error && st.phase !== "review-partial" && st.phase !== "review-ready" && st.phase !== "done" && st.phase !== "finalized") {
      // Re-render only when something else replaced the box, so an
      // expanded details block (and any selection in it) survives polls.
      if (shownStatusError !== st.error || errEl.innerHTML !== shownStatusHtml || !/\bshow\b/.test(errEl.className)) {
        errEl.classList.add("show");
        errEl.innerHTML = errorHtml(st.error);
        shownStatusError = st.error;
        shownStatusHtml = errEl.innerHTML;
      }
    } else {
      shownStatusError = null;
      errEl.classList.remove("show");
    }
  }
}
let shownStatusError = null, shownStatusHtml = "";
