// ---- UX-06: Recover / Carve as background case jobs ----
// Server contract (src/serve/jobs.rs): POST /api/recover-deleted and
// POST /api/carve answer {ok:true, job:{kind, started_unix}} at once (or
// {ok:false, error} when the job cannot start). /api/status then carries
// `job_kind` (what holds busy) and `job` = the current/last case job:
// {kind: "recover-deleted"|"carve", state: running|done|failed|cancelled,
//  stage, stages, stage_label, started_unix, finished_unix, elapsed_secs,
//  error}. The outcome is read from job.state, never guessed from words in
// an error string. /api/cancel kills the job's process tree; the job then
// ends "cancelled". A shell reloaded mid-job adopts the running job from
// /api/status so progress and cancel stay reachable.
// Older servers (no `job` in /api/status) still work: a synchronous
// response means finished; an async one falls back to busy/error.
// Completion refreshes the viewer via refreshViewerData() (postMessage /
// guarded reload), never `src = src`.
const CASE_JOBS = {
  recover: {
    serverKind: "recover-deleted",
    title: "삭제 영상 후보 복구",
    url: () => "/api/recover-deleted",
    confirm: null,
  },
  carve: {
    serverKind: "carve",
    title: "이미지 카빙",
    url: () => ($("chkReassemble") && $("chkReassemble").checked ? "/api/carve?reassemble=1" : "/api/carve"),
    confirm: {
      title: "이미지 카빙",
      html: "<p>export된 raw 이미지에서 영상 시그니처 카빙을 실행합니다.</p><p>이미지 크기에 따라 시간이 걸릴 수 있습니다 — 진행 상황과 경과 시간이 표시되며 언제든 중단할 수 있습니다. 계속하시겠습니까?</p>",
      okLabel: "카빙 실행",
    },
  },
};
// A server that went async but whose first status still reads idle gets
// this long before "not busy" is taken as "finished".
const CASE_JOB_GRACE_MS = 4000;
let caseJob = null;
let caseJobTicker = 0;

function setCaseToolsVisible(on) {
  for (const id of ["btnRecoverDeleted", "btnCarve", "reassembleWrap"]) {
    const el = $(id);
    if (el) el.hidden = !on;
  }
}
const CASE_JOB_TITLES = {
  btnRecoverDeleted: "조사된 이미지에서 삭제된 영상 후보를 복구한 뒤 뷰어를 갱신합니다",
  btnCarve: "export된 raw 이미지에서 영상 시그니처를 카빙합니다 (E01 전체 export 분석 필요)",
};
function setCaseJobButtons(busy) {
  for (const id of ["btnRecoverDeleted", "btnCarve"]) {
    const el = $(id);
    if (!el) continue;
    el.disabled = busy;
    // A disabled control says why.
    el.title = busy ? "다른 작업이 진행 중입니다 — 상단 작업 표시에서 진행 상황을 확인하거나 중단할 수 있습니다" : CASE_JOB_TITLES[id];
  }
}
function fmtClock(secs) {
  const s = Math.max(0, Math.floor(Number(secs) || 0));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
  return (h ? `${h}:${String(m).padStart(2, "0")}` : String(m)) + ":" + String(r).padStart(2, "0");
}
function caseJobElapsed(job) {
  if (!job) return 0;
  // Server-reported elapsed wins (it survives a page reload); the client
  // clock fills in between polls and on older servers.
  if (Number.isFinite(job.serverElapsed) && job.serverElapsedAt) {
    return job.serverElapsed + Math.max(0, ((job.endedAt || Date.now()) - job.serverElapsedAt) / 1000);
  }
  return Math.max(0, ((job.endedAt || Date.now()) - job.startedAt) / 1000);
}
// The server's record of THIS job in /api/status (null on older servers
// or when st.job describes a different/earlier job).
function serverCaseJob(st, job) {
  const sj = st && st.job;
  if (!sj || typeof sj !== "object" || !sj.state) return null;
  const info = job.jobInfo && typeof job.jobInfo === "object" ? job.jobInfo : null;
  if (sj.kind !== (info && info.kind ? info.kind : job.spec.serverKind)) return null;
  if (info && info.started_unix != null) return Number(sj.started_unix) === Number(info.started_unix) ? sj : null;
  // No start stamp from the POST: accept a job that started with ours.
  return Number(sj.started_unix) >= Math.floor(job.startedAt / 1000) - 5 ? sj : null;
}
// One line describing where the job stands, from /api/status.
function caseJobDetail(st, sj) {
  if (!st) return "";
  const logs = Array.isArray(st.logs) ? st.logs : [];
  const lastLog = logs.length ? String(logs[logs.length - 1]) : "";
  if (sj && sj.stages > 0) {
    const stage = `단계 ${Number(sj.stage) || 1}/${sj.stages}: ${sj.stage_label || ""}`;
    const tail = lastLog && lastLog !== sj.stage_label ? " · " + lastLog : "";
    const line = stage + tail;
    return line.length > 180 ? line.slice(0, 179) + "…" : line;
  }
  const p = st.progress;
  if (p && p.total > 0) {
    const pct = Math.min(100, Math.round(p.done * 100 / p.total));
    return `${Number(p.done).toLocaleString()}/${Number(p.total).toLocaleString()} (${pct}%)` +
      (p.eta_secs > 0 ? ` · 남은 시간 ${fmtEta(p.eta_secs)}` : "");
  }
  if (p && p.done > 0) return `${fmtBytes(p.done)} 처리됨`;
  return lastLog.length > 140 ? lastLog.slice(0, 139) + "…" : lastLog;
}
function renderCaseJob() {
  const job = caseJob;
  const panel = $("jobPanel");
  if (!panel) return;
  if (!job) {
    // The job controls live in the jobs sheet now, not inside the chip.
    for (const id of ["btnJobCancel", "btnJobReload", "btnJobClose"]) { const b = $(id); if (b) b.hidden = true; }
    renderPipelineChip();
    return;
  }
  panel.hidden = false;
  panel.className = "h-chip " + (job.outcome || "running");
  const title = job.spec.title;
  const label = {
    ok: `${title} 완료`,
    failed: `${title} 실패`,
    cancelled: `${title} 중단됨`,
    deferred: `${title} 완료 — 뷰어 새로 고침 대기`,
  }[job.outcome] || (job.cancel ? `${title} 중단 요청 중…` : `${title} 진행 중…`);
  $("jobTitle").textContent = label;
  $("jobDetail").textContent = job.message || job.detail || (job.async ? "" : "서버 응답 대기 중");
  // The pill truncates; the full line is in its tooltip. Only state
  // changes are announced (elapsed ticks every second and must not be).
  panel.title = label + ($("jobDetail").textContent ? " — " + $("jobDetail").textContent : "");
  if (job.announced !== label) {
    job.announced = label;
    announce(label + (job.outcome && job.message ? " — " + job.message : ""), job.outcome === "failed");
  }
  $("jobElapsed").textContent = "경과 " + fmtClock(caseJobElapsed(job));
  const cancel = $("btnJobCancel");
  cancel.hidden = !!job.outcome;
  cancel.disabled = !!job.cancel;
  cancel.textContent = job.cancel ? "중단 요청 중…" : "작업 중단";
  if (job.cancel) cancel.setAttribute("aria-busy", "true"); else cancel.removeAttribute("aria-busy");
  $("btnJobReload").hidden = job.outcome !== "deferred";
  $("btnJobClose").hidden = !job.outcome || job.outcome === "deferred";
}
function stopCaseJobTicker() {
  if (caseJobTicker) clearInterval(caseJobTicker);
  caseJobTicker = 0;
}

async function runCaseJob(kind) {
  if (caseJob && !caseJob.outcome) {
    toast("다른 작업이 진행 중입니다 — 끝난 뒤 다시 시도하십시오.", "warn");
    return null;
  }
  const spec = CASE_JOBS[kind];
  if (!spec) return null;
  if (spec.confirm) {
    const go = await askDialog(spec.confirm);
    if (!go) return null;
  }
  const job = { kind, spec, startedAt: Date.now(), endedAt: 0, async: false, posting: true, sawBusy: false,
    cancel: false, outcome: "", detail: "", message: "", serverElapsed: NaN, serverElapsedAt: 0 };
  caseJob = job;
  setCaseJobButtons(true);
  $("jobsNote").textContent = `${spec.title} 실행 중… 진행 상황은 상단 작업 표시에 나타납니다.`;
  renderCaseJob();
  stopCaseJobTicker();
  caseJobTicker = setInterval(() => { if (caseJob === job && !job.outcome) renderCaseJob(); }, 1000);
  // Faster status polling while the request is out (the poll loop is
  // otherwise idle-paced).
  schedulePoll(400);
  const res = await jpost(spec.url(), {});
  if (caseJob !== job || job.outcome) return job;
  job.posting = false;
  if (!res.ok) {
    await finishCaseJob(job, job.cancel ? "cancelled" : "failed", res.error || "알 수 없는 오류");
    return job;
  }
  if (res.job !== undefined && res.job !== null && res.job !== false) {
    // Async server: completion arrives through /api/status.
    job.async = true;
    job.jobInfo = res.job;
    renderCaseJob();
    schedulePoll(400);
    return job;
  }
  // Older synchronous server: the work is already finished.
  await finishCaseJob(job, "ok");
  return job;
}

// Called for every /api/status reply (poll loop).
function caseJobOnStatus(st) {
  if (!st) return;
  if (!caseJob || (caseJob.outcome && caseJob.outcome !== "deferred")) {
    adoptRunningCaseJob(st);
    return;
  }
  const job = caseJob;
  if (job.outcome) return;
  const sj = job.async && !job.posting ? serverCaseJob(st, job) : null;
  if (sj) {
    job.sawServerJob = true;
    const el = Number(sj.elapsed_secs);
    if (Number.isFinite(el) && el >= 0) { job.serverElapsed = el; job.serverElapsedAt = Date.now(); }
    if (sj.state === "running") {
      job.detail = caseJobDetail(st, sj);
      renderCaseJob();
      return;
    }
    if (sj.state === "done") finishCaseJob(job, "ok");
    else if (sj.state === "cancelled") finishCaseJob(job, "cancelled", sj.error);
    else finishCaseJob(job, "failed", sj.error || st.error || "알 수 없는 오류");
    return;
  }
  // Older server (no `job` in /api/status): busy / error only.
  if (st.busy) job.sawBusy = true;
  const el = Number(st.elapsed_secs);
  if (Number.isFinite(el) && el >= 0 && st.busy) { job.serverElapsed = el; job.serverElapsedAt = Date.now(); }
  if (st.busy) job.detail = caseJobDetail(st, null);
  renderCaseJob();
  if (!job.async || job.posting || job.sawServerJob) return;
  const graceOver = Date.now() - job.startedAt > CASE_JOB_GRACE_MS;
  if (!st.busy && (job.sawBusy || graceOver)) {
    if (st.error) finishCaseJob(job, job.cancel ? "cancelled" : "failed", st.error);
    else finishCaseJob(job, "ok");
  }
}

// A shell (re)loaded while the server runs a case job (or a job started
// from another window): show it, with elapsed time and cancel, instead of
// hiding running work.
function adoptRunningCaseJob(st) {
  const sj = st.job;
  if (!sj || sj.state !== "running") return;
  const kind = Object.keys(CASE_JOBS).find(k => CASE_JOBS[k].serverKind === sj.kind);
  if (!kind) return;
  const prev = caseJob;
  if (prev && prev.jobInfo && Number(prev.jobInfo.started_unix) === Number(sj.started_unix) && prev.jobInfo.kind === sj.kind) return;
  const spec = CASE_JOBS[kind];
  const el = Number(sj.elapsed_secs) || 0;
  const job = { kind, spec, startedAt: Date.now() - el * 1000, endedAt: 0, async: true, posting: false, sawBusy: true,
    cancel: false, outcome: "", detail: caseJobDetail(st, sj), message: "", serverElapsed: el, serverElapsedAt: Date.now(),
    jobInfo: { kind: sj.kind, started_unix: sj.started_unix }, adopted: true, sawServerJob: true };
  caseJob = job;
  setCaseJobButtons(true);
  $("jobsNote").textContent = `${spec.title} 실행 중… 진행 상황은 상단 작업 표시에 나타납니다.`;
  renderCaseJob();
  stopCaseJobTicker();
  caseJobTicker = setInterval(() => { if (caseJob === job && !job.outcome) renderCaseJob(); }, 1000);
}

async function finishCaseJob(job, outcome, error) {
  if (job.outcome) return;
  job.endedAt = Date.now();
  stopCaseJobTicker();
  setCaseJobButtons(false);
  const title = job.spec.title;
  if (outcome === "failed") {
    job.outcome = "failed";
    job.message = String(error || "알 수 없는 오류").split(/\r?\n/)[0].slice(0, 240);
    $("jobsNote").textContent = `${title} 실패: ${job.message} — 원인을 확인한 뒤 다시 시도할 수 있습니다.`;
    renderCaseJob();
    toast(`${title} 실패: ${job.message}`, "err");
    return;
  }
  if (outcome === "cancelled") {
    job.outcome = "cancelled";
    const why = String(error || "").split(/\r?\n/)[0].slice(0, 240);
    job.message = (why || "작업이 중단되었습니다.") + " 다시 실행할 수 있습니다.";
    $("jobsNote").textContent = `${title}이(가) 중단되었습니다.`;
    renderCaseJob();
    return;
  }
  job.message = "완료 — 뷰어를 갱신하는 중…";
  job.outcome = "ok";
  renderCaseJob();
  const how = await refreshViewerData(job.kind);
  job.refresh = how;
  if (how === "deferred") {
    job.outcome = "deferred";
    job.message = "저장되지 않은 판독 내용이 있어 뷰어를 새로 고치지 않았습니다 — 저장이 끝난 뒤 '뷰어 새로 고침'을 누르십시오.";
    toast(`${title} 완료 — ${job.message}`, "warn");
  } else {
    job.message = how === "none" ? "완료 — 검토 화면을 열면 결과가 표시됩니다." : "완료 — 뷰어에 결과를 반영했습니다.";
  }
  $("jobsNote").textContent = `${title} ${job.message}`;
  renderCaseJob();
}

$("btnRecoverDeleted").addEventListener("click", () => { runCaseJob("recover"); });
$("btnCarve").addEventListener("click", () => { runCaseJob("carve"); });
$("btnJobCancel").addEventListener("click", async () => {
  const job = caseJob;
  if (!job || job.outcome || job.cancel) return;
  const go = await askDialog({
    title: `${job.spec.title} 중단`,
    html: "<p>진행 중인 작업을 중단하시겠습니까?</p><p>이미 만들어진 산출물은 케이스 폴더에 남고, 언제든 다시 실행할 수 있습니다.</p>",
    okLabel: "작업 중단", cancelLabel: "계속", danger: true, focus: "dlgCancel",
  });
  if (!go || caseJob !== job || job.outcome) return;
  job.cancel = true;
  renderCaseJob();
  const res = await jpost("/api/cancel", {});
  if (!res.ok) {
    job.cancel = false;
    renderCaseJob();
    toast("중단 요청 실패: " + (res.error || "알 수 없는 오류"), "err");
  }
});
$("btnJobReload").addEventListener("click", async () => {
  const job = caseJob;
  if (!job || job.outcome !== "deferred") return;
  const how = guardedFrameReload();
  if (how === "deferred") {
    toast("아직 저장되지 않은 판독 내용이 있습니다 — 저장 상태가 '저장됨'이 된 뒤 다시 누르십시오.", "warn");
    return;
  }
  job.outcome = "ok";
  job.message = "완료 — 뷰어에 결과를 반영했습니다.";
  renderCaseJob();
});
$("btnJobClose").addEventListener("click", () => {
  if (caseJob && caseJob.outcome && caseJob.outcome !== "deferred") { caseJob = null; renderCaseJob(); }
});

