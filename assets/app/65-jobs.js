// ---- UX-06: Recover / Carve as background case jobs ----
// Contract (server side lands separately): POST /api/recover-deleted and
// POST /api/carve answer {ok:true, job:...} immediately; progress and
// cancel go through the existing /api/status (busy/phase/logs/progress,
// optional elapsed_secs) and /api/cancel. An older server answers only
// when the work is finished ({ok:true} / {ok:false,error}) — then the
// regular status poll still drives the panel (elapsed, cancel) while the
// request is outstanding. Either way completion refreshes the viewer via
// refreshViewerData() (postMessage / guarded reload), never `src = src`.
const CASE_JOBS = {
  recover: {
    title: "삭제 영상 후보 복구",
    url: () => "/api/recover-deleted",
    confirm: null,
  },
  carve: {
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
function setCaseJobButtons(busy) {
  for (const id of ["btnRecoverDeleted", "btnCarve"]) {
    const el = $(id);
    if (el) el.disabled = busy;
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
    return job.serverElapsed + Math.max(0, (Date.now() - job.serverElapsedAt) / 1000);
  }
  return Math.max(0, ((job.endedAt || Date.now()) - job.startedAt) / 1000);
}
// One line describing where the job stands, from /api/status.
function caseJobDetail(st) {
  if (!st) return "";
  const p = st.progress;
  if (p && p.total > 0) {
    const pct = Math.min(100, Math.round(p.done * 100 / p.total));
    return `${Number(p.done).toLocaleString()}/${Number(p.total).toLocaleString()} (${pct}%)` +
      (p.eta_secs > 0 ? ` · 남은 시간 ${fmtEta(p.eta_secs)}` : "");
  }
  if (p && p.done > 0) return `${fmtBytes(p.done)} 처리됨`;
  const logs = Array.isArray(st.logs) ? st.logs : [];
  const last = logs.length ? String(logs[logs.length - 1]) : "";
  return last.length > 140 ? last.slice(0, 139) + "…" : last;
}
function renderCaseJob() {
  const job = caseJob;
  const panel = $("jobPanel");
  if (!panel) return;
  if (!job) { panel.hidden = true; return; }
  panel.hidden = false;
  panel.className = "jobpanel " + (job.outcome || "running");
  const title = job.spec.title;
  const label = {
    ok: `${title} 완료`,
    failed: `${title} 실패`,
    cancelled: `${title} 중단됨`,
    deferred: `${title} 완료 — 뷰어 새로 고침 대기`,
  }[job.outcome] || (job.cancel ? `${title} 중단 요청 중…` : `${title} 진행 중…`);
  $("jobTitle").textContent = label;
  $("jobDetail").textContent = job.message || job.detail || (job.async ? "" : "서버 응답 대기 중");
  $("jobElapsed").textContent = "경과 " + fmtClock(caseJobElapsed(job));
  const cancel = $("btnJobCancel");
  cancel.hidden = !!job.outcome;
  cancel.disabled = !!job.cancel;
  cancel.textContent = job.cancel ? "중단 요청 중…" : "작업 중단";
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
  $("recoverHint").textContent = `${spec.title} 실행 중… 진행 상황은 위 작업 표시줄에 나타납니다.`;
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
  const job = caseJob;
  if (!job || job.outcome || !st) return;
  if (st.busy) job.sawBusy = true;
  const el = Number(st.elapsed_secs);
  if (Number.isFinite(el) && el >= 0 && st.busy) { job.serverElapsed = el; job.serverElapsedAt = Date.now(); }
  if (st.busy) job.detail = caseJobDetail(st);
  renderCaseJob();
  if (!job.async || job.posting) return;
  const graceOver = Date.now() - job.startedAt > CASE_JOB_GRACE_MS;
  if (!st.busy && (job.sawBusy || graceOver)) {
    if (st.error) {
      const cancelled = job.cancel || /중단|cancel/i.test(String(st.error));
      finishCaseJob(job, cancelled ? "cancelled" : "failed", st.error);
    } else {
      finishCaseJob(job, "ok");
    }
  }
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
    $("recoverHint").textContent = `${title} 실패: ${job.message} — 원인을 확인한 뒤 다시 시도할 수 있습니다.`;
    renderCaseJob();
    return;
  }
  if (outcome === "cancelled") {
    job.outcome = "cancelled";
    job.message = "작업이 중단되었습니다 — 다시 실행할 수 있습니다.";
    $("recoverHint").textContent = `${title}이(가) 중단되었습니다.`;
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
  } else {
    job.message = how === "none" ? "완료 — 검토 화면을 열면 결과가 표시됩니다." : "완료 — 뷰어에 결과를 반영했습니다.";
  }
  $("recoverHint").textContent = `${title} ${job.message}`;
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

