// ---- analysis chip (#jobPanel, top strip) — T3, contract §2.2/§6 ----
// One inline, non-blocking chip for everything that runs: the analysis
// pipeline (and its enrichment), Recover/Carve and clip exports. It never
// covers or blocks the review grid. T2 provides the empty mount node
// <div id="jobPanel" class="h-chip" hidden>; this file renders the single
// button inside it and keeps its class in {running ok failed cancelled
// deferred idle}. Clicking it opens the work sheet (32-jobs.js) — the
// open/close wiring (panels.jobs, Alt+J, focus) is the shell's (T2).
//
// Priority when several things run: pipeline > case job > clip export
// (running first, then finished notices: case job > pipeline).
//
// Pure derivation: pipelineView(S) and chipModel(S, now) only read the
// FTStore state (docs §4), so the harness exercises them without a DOM.

// ---- pipeline view (store -> one small object) ----
let pipeStartedAt = 0;          // client clock when the current run was first seen active
let pipePrevState = null;       // last pipelineView().state seen (null = nothing seen yet)
let pipeReloadWaits = false;    // enrichment finished under an open viewer: fresh data waits
let pipeDismissed = "";         // notice key the examiner cleared
// A lossless join (clip job kind "join") finished under an open viewer: the
// derived record waits for the same '새 데이터 반영' (never a silent reload).
let joinReloadWaits = false;
function pipelineReloadPending() { return pipeReloadWaits || joinReloadWaits; }
function pipelineReloadDone() { pipeReloadWaits = false; joinReloadWaits = false; }
function joinReloadPending() { return joinReloadWaits; }
function pipelineDismiss() {
  const S = storeState();
  if (S) pipeDismissed = pipelineView(S).noticeKey;
  pipeReloadWaits = false;
  joinReloadWaits = false;
}
function pipelineActive(S) { return pipelineView(S).active; }

// Fresh index data the mounted viewer does not show yet. The viewer only
// reports which generation it loaded (FTViewer "live-state"); the current
// generation comes from the /api/status poll — the one source of truth for
// indexing / enrichment state (the viewer shows no live chip when hosted).
// -> { total, canReload } | null
function liveReloadOffer(S) {
  const v = (S && S.viewer) || {};
  if (!v.mounted || !v.live || v.liveGeneration == null) return null;
  const gen = Number(S.counts && S.counts.generation) || 0;
  if (!gen || gen === Number(v.liveGeneration)) return null;
  return { total: Number(S.counts.total) || 0, canReload: v.canReload !== false };
}

// state: idle | running | enriching | failed | cancelled | stopped | ready
function pipelineView(S) {
  const p = (S.jobs && S.jobs.pipeline) || {};
  const raw = String(p.phase || "");
  const steps = p.steps || [];
  const labels = p.stepNames && p.stepNames.length === 5 ? p.stepNames : STEP_NAMES;
  const runIdx = steps.indexOf("running");
  const doneN = steps.filter(s => s === "done" || s === "partial").length; // R4 B16: "partial" finished (with a warning)
  const stepNo = Math.min(labels.length, (runIdx >= 0 ? runIdx : doneN) + 1);
  const err = String(p.error || "");
  let state = "idle";
  if (raw === "running") state = "running";
  else if (raw === "review-partial") state = p.busy ? "enriching" : "stopped";
  else if (raw === "error") state = /중단|cancel/i.test(err) ? "cancelled" : "failed";
  else if ((pipeReloadWaits || liveReloadOffer(S)) && ["review-ready", "done", "finalized"].includes(raw)) state = "ready";
  const active = state === "running" || state === "enriching";
  const noticeKey = state === "idle" || active ? "" : `${state}|${err}`;
  const notice = !!noticeKey && noticeKey !== pipeDismissed;
  return {
    state, active, stepNo, stepCount: labels.length, stepName: labels[stepNo - 1] || "",
    notice, noticeKey, dismissible: notice,
    errorLine: err.split(/\r?\n/)[0].slice(0, 160),
  };
}

function fmtCount(n) { return Number(n).toLocaleString("en-US"); }
// "색인 1,240/4,000" while the viewer shows fewer than the index holds,
// "색인 4,000건" otherwise; empty until a count is known.
function indexCountText(S) {
  const c = S.counts || {};
  const total = Number(c.total) || 0, indexed = Number(c.indexed) || 0;
  if (total > 0 && indexed > 0) return `색인 ${fmtCount(indexed)}/${fmtCount(total)}`;
  if (total > 0) return `색인 ${fmtCount(total)}건`;
  if (indexed > 0) return `색인 ${fmtCount(indexed)}건`;
  return "";
}

const JOIN = " · ";
function clipView(S) {
  const clips = (S.jobs && S.jobs.clipJobs) || [];
  return { running: clips.filter(c => c && c.state === "running"), all: clips };
}
// clip_jobs carry both clip exports and lossless joins (kind "join").
function isJoinJob(c) { return !!c && c.kind === "join"; }
function clipJobsTitle(list) {
  const joins = list.filter(isJoinJob).length;
  if (!joins) return "클립 내보내기";
  return joins === list.length ? "영상 이어붙이기" : "내보내기·이어붙이기";
}
// R6 (S4): /api/status.progress job_type "raw-rehash" = the kept raw image
// is re-verified (hashed) before reuse — bytes done / total, cancellable
// with the job's [중단]. -> "원본 재검증 12.3 GB / 40 GB (31%)" | "".
function rawRehashText(progress) {
  if (!progress || progress.job_type !== "raw-rehash") return "";
  const total = Number(progress.total) || 0;
  const done = Math.min(total || Infinity, Number(progress.done) || 0);
  if (!(total > 0)) return "원본 재검증 중";
  return `원본 재검증 ${fmtBytes(done)} / ${fmtBytes(total)} (${Math.min(100, Math.floor(done * 100 / total))}%)`;
}
function hasProject(S) {
  return !!(S.project && S.project.caseDir) && S.phase !== "empty";
}

// -> { mode, cls, title, detail, elapsed (secs|null) } | null
function chipModel(S, now) {
  const J = S.jobs || {};
  const pv = pipelineView(S);
  const cj = J.caseJob || null;
  const clips = clipView(S);
  const p = J.pipeline || {};
  if (pv.active) {
    const idx = indexCountText(S);
    let title, detail;
    if (pv.state === "enriching") {
      const e = p.enrichment;
      title = "보강 중";
      detail = [e ? `${e.label} ${e.stage}/${e.stages}` + (e.unit === "units" && e.total > 0 ? ` ${fmtCount(e.done)}/${fmtCount(e.total)}` : "") : "해시·검증", idx].filter(Boolean).join(JOIN);
    } else {
      title = "분석 중";
      detail = [idx, rawRehashText(p.progress) || `단계 ${pv.stepNo}/${pv.stepCount} ${pv.stepName}`].filter(Boolean).join(JOIN);
    }
    const server = p.progress && Number(p.progress.elapsed_secs) || 0;
    const client = pipeStartedAt ? Math.max(0, (now - pipeStartedAt) / 1000) : 0;
    return { mode: "pipeline", cls: "running", title, detail, elapsed: Math.max(server, client), live: true };
  }
  if (cj && !cj.outcome) return caseChip(cj, now);
  if (clips.running.length) {
    const bytes = clips.running.reduce((n, c) => n + (Number(c.bytes_written) || 0), 0);
    const base = Math.max(...clips.running.map(c => Number(c.elapsed_secs) || 0));
    const at = typeof pollStatusAt === "number" ? pollStatusAt : 0;
    const since = at ? Math.max(0, (now - at) / 1000) : 0;
    return { mode: "clips", cls: "running", title: clipJobsTitle(clips.running),
      detail: `${clips.running.length}건 진행 중` + (bytes > 0 ? JOIN + fmtBytes(bytes) : ""), elapsed: base + since, live: true };
  }
  if (cj && cj.outcome) return caseChip(cj, now);
  if (joinReloadWaits) return { mode: "join", cls: "deferred", title: "이어붙이기 완료", detail: "새 데이터 반영 대기", elapsed: null };
  if (pv.notice) {
    if (pv.state === "failed") return { mode: "pipeline", cls: "failed", title: "분석 실패", detail: pv.errorLine, elapsed: null };
    if (pv.state === "cancelled") return { mode: "pipeline", cls: "cancelled", title: "분석 중단됨", detail: "같은 소스로 다시 분석하면 이어서 진행합니다", elapsed: null };
    if (pv.state === "stopped") return { mode: "pipeline", cls: "cancelled", title: "보강 중단됨", detail: "해시·검증 미완료 — 검토는 계속할 수 있습니다", elapsed: null };
    if (pv.state === "ready") return { mode: "pipeline", cls: "deferred", title: "보강 완료", detail: "새 데이터 반영 대기", elapsed: null };
  }
  if (hasProject(S)) return { mode: "idle", cls: "idle", title: "작업", detail: "", elapsed: null };
  return null;
}
function caseChip(job, now) {
  const title = job.title || job.spec.title;
  const label = {
    ok: `${title} 완료`,
    failed: `${title} 실패`,
    cancelled: `${title} 중단됨`,
    deferred: `${title} 완료 — 뷰어 새로 고침 대기`,
  }[job.outcome] || (job.cancel ? `${title} 중단 요청 중…` : `${title} 진행 중…`);
  const detail = job.message || job.detail || (job.async ? "" : "서버 응답 대기 중");
  return { mode: "case", cls: job.outcome || "running", title: label, detail, elapsed: caseJobElapsed(job), live: !job.outcome };
}

// ---- chip DOM ----
function chipHtml() {
  return `<button type="button" id="hBtnJobs" class="quiet h-chip-btn" aria-controls="hJobsSheet" aria-expanded="false" title="작업 시트 열기 (Alt+J)">` +
    `<span id="jobDot" class="h-dot" aria-hidden="true"></span>` +
    `<span class="h-chip-text"><b id="jobTitle"></b> <span id="jobDetail"></span></span>` +
    `<span id="jobElapsed" class="h-chip-elapsed mono num"></span></button>`;
}
function mountChip() {
  const host = $("jobPanel");
  if (!host) return false;
  host.innerHTML = chipHtml();
  host.hidden = true;
  return true;
}
const chipMounted = mountChip();
let chipAnnounced = "";
let chipLast = null;
function renderChip() {
  const panel = $("jobPanel");
  const S = storeState();
  if (!panel || !S) return;
  const m = chipModel(S, Date.now());
  chipLast = m;
  if (!m) { panel.hidden = true; return; }
  panel.hidden = false;
  panel.className = "h-chip " + m.cls;
  $("jobTitle").textContent = m.title;
  $("jobDetail").textContent = m.detail;
  $("jobElapsed").textContent = m.elapsed == null ? "" : "경과 " + fmtClock(m.elapsed);
  // The chip truncates; the full line is in its tooltip. Only state
  // changes are announced (elapsed ticks every second and must not be).
  const full = m.title + (m.detail ? " — " + m.detail : "");
  $("hBtnJobs").title = m.mode === "idle" ? "작업 시트 열기 (Alt+J)" : full + " · 작업 시트 열기 (Alt+J)";
  $("hBtnJobs").setAttribute("aria-label", m.mode === "idle" ? "작업" : full + (m.elapsed == null ? "" : ", 경과 " + fmtClock(m.elapsed)) + " — 작업 시트 열기");
  const key = m.cls + "|" + m.title;
  if (m.mode !== "idle" && chipAnnounced !== key) {
    chipAnnounced = key;
    announce(full, m.cls === "failed");
  }
}

// ---- transitions: auto-open once, failure toasts, reload waits ----
let jobsAutoOpened = false;     // the sheet auto-opens once per session, on the first analysis start
let jobsUserClosed = false;     // ...and never again after the examiner closed it
let jobsPanelWas = false;
let pipeToastKey = "";
function onJobsStoreChange() {
  const S = storeState();
  if (!S) return;
  // panels.jobs true -> false = the examiner closed it.
  const open = !!(S.panels && S.panels.jobs);
  if (jobsPanelWas && !open) jobsUserClosed = true;
  jobsPanelWas = open;

  const pv = pipelineView(S);
  const was = pipePrevState;
  if (pv.active && !pipeStartedAt) pipeStartedAt = Date.now();
  if (!pv.active) pipeStartedAt = 0;
  // enrichment ended while a viewer is mounted: fresh hashes/validation
  // wait for the examiner's '새 데이터 반영' (never reloaded behind their back).
  if (was === "enriching" && !pv.active && ["review-ready", "done", "finalized"].includes((S.jobs.pipeline || {}).phase) && S.viewer && S.viewer.mounted) {
    pipeReloadWaits = true;
    announce("분석 보강 완료 — 새 데이터를 반영할 수 있습니다.");
  }
  if (!["review-ready", "done", "finalized"].includes((S.jobs.pipeline || {}).phase)) pipeReloadWaits = false;
  // First analysis start of the session: show the sheet once (never when
  // the page merely reloaded into a running job).
  if (pv.state === "running" && was !== null && was !== "running" && was !== "enriching" && !jobsAutoOpened && !jobsUserClosed && !open) {
    jobsAutoOpened = true;
    // Opening goes through the shell (97-shell.js owns panels.* and focus).
    if (typeof hAutoOpenJobs === "function") hAutoOpenJobs();
    else patchStore({ panels: Object.assign({}, S.panels, { jobs: true }) });
    jobsPanelWas = true;
  }
  // A failure is never silent: sticky toast once per distinct error.
  if (was !== null && pv.notice && pv.state === "failed" && pipeToastKey !== pv.noticeKey) {
    pipeToastKey = pv.noticeKey;
    toast(`분석 실패: ${pv.errorLine}`, "err");
  }
  pipePrevState = pv.state;
  noteClipFailures(S);
}
// Clip exports and joins end on their own: a failed one is reported once,
// and a join seen running that finished waits for '새 데이터 반영' (F15).
const clipSeen = new Set();
const joinsSeenRunning = new Set();
let clipsBaselined = false;
function noteClipFailures(S) {
  const clips = (S.jobs && S.jobs.clipJobs) || [];
  for (const c of clips) {
    if (!c) continue;
    const key = `${c.id}|${c.started_unix}`;
    if (isJoinJob(c) && c.state === "running") { joinsSeenRunning.add(key); continue; }
    if (isJoinJob(c) && c.state === "done") {
      if (joinsSeenRunning.delete(key) && S.viewer && S.viewer.mounted) {
        joinReloadWaits = true;
        announce("이어붙이기 완료 — 새 데이터를 반영하면 목록에 표시됩니다.");
      }
      continue;
    }
    if (c.state !== "failed") { joinsSeenRunning.delete(key); continue; }
    joinsSeenRunning.delete(key);
    if (clipSeen.has(key)) continue;
    clipSeen.add(key);
    const what = isJoinJob(c) ? "이어붙이기" : "클립 내보내기";
    if (clipsBaselined) toast(`${what} 실패: ${clipName(c)} — ${String(c.error || "알 수 없는 오류").split(/\r?\n/)[0].slice(0, 160)}`, "err");
  }
  clipsBaselined = true;
}
// The hosted viewer reports joins it started (FTViewer "derived-job",
// 22-viewer-host.js). "started": poll fast now (the idle poll is 5 s and a
// short join can begin and end between two polls) and remember the job as
// seen running. "done": the derived record waits for '새 데이터 반영'
// whether or not a poll ever saw the job running.
function noteViewerDerivedJob(d) {
  if (!d || d.kind !== "join") return;
  const key = `${d.id}|${d.started_unix}`;
  if (d.state === "started" || d.state === "running") {
    joinsSeenRunning.add(key);
    if (typeof schedulePoll === "function") schedulePoll(300);
    return;
  }
  joinsSeenRunning.delete(key);
  if (d.state !== "done") { if (typeof schedulePoll === "function") schedulePoll(300); return; }
  const S = storeState();
  if (S && S.viewer && S.viewer.mounted && !joinReloadWaits) {
    joinReloadWaits = true;
    announce("이어붙이기 완료 — 새 데이터를 반영하면 목록에 표시됩니다.");
  }
  if (typeof schedulePoll === "function") schedulePoll(300);
  renderChip();
  renderJobsSheet();
}
let jobsMountedSeen = null;
if (typeof FTStore === "object" && FTStore && typeof FTStore.on === "function") {
  FTStore.on("change", ev => {
    const keys = ev && ev.keys;
    if (keys && typeof keys.has === "function") {
      if (!(keys.has("phase") || keys.has("jobs") || keys.has("counts") || keys.has("project") || keys.has("panels") || keys.has("viewer"))) return;
      // Selection / save-state traffic only touches `viewer`: re-render
      // only when the mounted flag itself moved.
      if (keys.size === 1 && keys.has("viewer")) {
        const S = storeState();
        const m = !!(S && S.viewer && S.viewer.mounted);
        if (m === jobsMountedSeen) return;
        jobsMountedSeen = m;
      }
    }
    onJobsStoreChange();
    renderChip();
    renderJobsSheet();
  });
}
// Elapsed ticks once a second while something runs (text only).
setInterval(() => { if (chipLast && chipLast.live) renderChip(); }, 1000);
