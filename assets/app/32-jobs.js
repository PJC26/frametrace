// ---- work sheet (#hJobsSheet) + case jobs + log (T3, contract §1/§2.2/§6.3) ----
// The sheet is a right-hand, non-modal panel. T2 owns the empty mount node
// (<div id="hJobsSheet" class="h-sheet" hidden>) and the open/close
// plumbing (panels.jobs, Alt+J, Esc, focus); this file renders everything
// inside it and owns the job controls:
//   step track + counts + status line + failure box   (pipeline, from FTStore)
//   '작업 시작'  btnRecoverDeleted / btnCarve / chkReassemble
//   '클립 내보내기' list (clip_jobs)
//   log (#log) with copy / save / jump-to-latest
//   foot: btnJobReload · btnJobClose · btnNew · btnCancel · btnJobCancel
// The analysis chip in the top strip is 31-progress.js.
//
// Server contract for Recover/Carve (src/serve/jobs.rs): POST
// /api/recover-deleted and POST /api/carve answer {ok:true, job:{kind,
// started_unix}} at once (or {ok:false, error}). /api/status then carries
// `job_kind` and `job` = the current/last case job {kind, state: running|
// done|failed|cancelled, stage, stages, stage_label, started_unix,
// finished_unix, elapsed_secs, error}; the outcome is read from job.state,
// never guessed from words in an error string. /api/cancel stops it. A shell
// reloaded mid-job adopts the running job from /api/status. Older servers
// (no `job`) fall back to busy/error. Completion refreshes the hosted viewer
// with FTViewer.reload() (refused while marks are unsaved -> 'deferred').

// T2-owned nodes are looked up by constant so this file does not depend on
// the shell markup having them (check-js id gate); they are optional.
const JOBS_SHEET_ID = "hJobsSheet";
const PROJECT_NEW_ID = "hProjectNew";

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
  // Measured sound for the items not measured yet (docs/PLAN_PHASE2_JOIN_
  // AUDIO_2026-10-13.md §2.2): started from the deliverables sheet's
  // Advanced section (41-finalize.js); same case-job contract as recover.
  audio: {
    serverKind: "audio-measure",
    title: "소리 측정",
    url: () => "/api/audio-measure",
    confirm: {
      title: "소리 측정",
      html: "<p>오디오 트랙이 있는데 아직 소리를 재지 않은 영상을 <b>처음부터 끝까지 디코드</b>해 최대·평균 신호 레벨을 잽니다.</p>" +
        "<p>영상 수와 길이에 따라 오래 걸릴 수 있습니다 — 그동안 다른 작업(복구·카빙·보고서 생성)은 기다리며, 진행 상황은 작업 시트에 표시되고 언제든 중단할 수 있습니다. 계속하시겠습니까?</p>",
      okLabel: "소리 측정 실행",
    },
  },
};
// A server that went async but whose first status still reads idle gets
// this long before "not busy" is taken as "finished".
const CASE_JOB_GRACE_MS = 4000;
let caseJob = null;
let caseJobTicker = 0;

// ---- sheet markup ----
const IC = name => `<svg class="ic" aria-hidden="true"><use href="#i-${name}"/></svg>`;
function jobsSheetHtml() {
  return `
<div class="h-sheet-head h-job-head">
  <h2 id="hJobsTitle" class="h-job-title">작업</h2>
  <button type="button" id="hJobsClose" class="quiet icon" aria-label="작업 시트 닫기" title="닫기 (Esc)">${IC("close")}</button>
</div>
<div class="h-sheet-body h-job-body">
  <p id="jobsNote" class="h-job-note"></p>
  <div id="jobsErr" class="h-job-err" role="alert" hidden></div>
  <section id="jobsPipe" class="h-job-sec" aria-label="분석 단계" hidden>
    <div class="h-job-secrow"><h3>분석 단계</h3><span id="jobsCounts" class="mono num"></span></div>
    <ol class="h-track" id="track"></ol>
  </section>
  <section id="jobsStart" class="h-job-sec" aria-label="작업 시작" hidden>
    <h3>작업 시작</h3>
    <div class="h-job-actions">
      <button type="button" id="btnRecoverDeleted" hidden title="조사된 이미지에서 삭제된 영상 후보를 복구한 뒤 뷰어를 갱신합니다">삭제 영상 후보 복구</button>
      <button type="button" id="btnCarve" hidden title="export된 raw 이미지에서 영상 시그니처를 카빙합니다 (E01 전체 export 분석 필요)">이미지 카빙</button>
    </div>
    <label class="h-job-chk" id="reassembleWrap" hidden title="조각난 영상을 이어붙인 후보를 추가로 만듭니다. 시간이 더 걸리며, 결과는 검증 전 '후보'로만 표시됩니다 — 확정된 복구가 아닙니다"><input type="checkbox" id="chkReassemble"> 카빙 시 조각 재조립 시도</label>
  </section>
  <section id="jobsClips" class="h-job-sec" aria-label="클립 내보내기" hidden>
    <h3>클립 내보내기</h3>
    <ul class="h-clips" id="jobsClipList"></ul>
  </section>
  <section class="h-job-sec h-job-logsec" aria-label="로그">
    <div class="h-job-secrow">
      <h3>로그</h3>
      <span class="h-job-logtools">
        <button type="button" id="btnLogBottom" class="quiet" hidden title="로그 맨 아래로 이동하고 자동 스크롤을 다시 켭니다">${IC("chevron-down")}최신으로</button>
        <button type="button" id="btnCopyLog" class="quiet" title="현재까지의 로그를 클립보드로 복사합니다">${IC("copy")}복사</button>
        <button type="button" id="btnSaveLog" class="quiet" title="현재까지의 로그를 텍스트 파일로 저장합니다">${IC("download")}저장</button>
      </span>
    </div>
    <pre class="h-job-log" id="log" tabindex="0" aria-label="분석 로그 (스크롤 가능)"></pre>
  </section>
</div>
<div class="h-sheet-foot h-job-foot" id="jobsFoot">
  <button type="button" id="btnJobReload" class="primary" hidden>뷰어 새로 고침</button>
  <button type="button" id="btnJobClose" class="quiet" hidden>알림 지우기</button>
  <span class="h-job-spacer"></span>
  <button type="button" id="btnNew" hidden>새 케이스 시작</button>
  <button type="button" id="btnCancel" class="destructive" hidden>분석 중단</button>
  <button type="button" id="btnJobCancel" class="destructive" hidden>작업 중단</button>
</div>`;
}
function mountJobsSheet() {
  const host = document.getElementById(JOBS_SHEET_ID);
  if (!host) return false;
  const has = k => typeof host.getAttribute === "function" && host.getAttribute(k);
  if (!has("role")) host.setAttribute("role", "complementary");
  if (!has("aria-labelledby")) host.setAttribute("aria-labelledby", "hJobsTitle");
  host.setAttribute("data-region", "hJobsSheet");
  host.setAttribute("data-keys", "off");
  host.innerHTML = jobsSheetHtml();
  return true;
}
const jobsSheetMounted = mountJobsSheet();

// ---- tool buttons (Recover / Carve) ----
function setCaseToolsVisible(on) {
  for (const id of ["btnRecoverDeleted", "btnCarve", "reassembleWrap"]) {
    const el = $(id);
    if (el) el.hidden = !on;
  }
  const sec = $("jobsStart");
  if (sec) sec.hidden = !on;
}
const CASE_JOB_TITLES = {
  btnRecoverDeleted: "조사된 이미지에서 삭제된 영상 후보를 복구한 뒤 뷰어를 갱신합니다",
  btnCarve: "export된 raw 이미지에서 영상 시그니처를 카빙합니다 (E01 전체 export 분석 필요)",
};
// Whether a case job (or anything the server refuses one beside) runs:
// read by the deliverables sheet's 소리 측정 button (41-finalize.js), which
// also depends on ffmpeg/ffprobe being present.
let caseJobsBusy = false;
function setCaseJobButtons(busy) {
  caseJobsBusy = !!busy;
  for (const id of ["btnRecoverDeleted", "btnCarve"]) {
    const el = $(id);
    if (!el) continue;
    el.disabled = busy;
    // A disabled control says why.
    el.title = busy ? "다른 작업이 진행 중입니다 — 위 칩이나 이 시트에서 진행 상황을 보고 중단할 수 있습니다" : CASE_JOB_TITLES[id];
  }
  try { if (typeof finApplyAudioEnv === "function") finApplyAudioEnv(); } catch (e) { /* sheet not built yet */ }
}
setCaseToolsVisible(false);

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
// Publishes the job object to the store (chip + sheet render from there)
// and keeps the 1 s elapsed ticker alive only while a job runs.
function renderCaseJob() {
  const job = caseJob;
  if (job && !job.outcome && !caseJobTicker) {
    caseJobTicker = setInterval(() => { if (caseJob && !caseJob.outcome) publishCaseJob(); }, 1000);
  }
  publishCaseJob();
}
// The store detects changes by value, and caseJob is mutated in place, so
// it is published as a snapshot; the chip and sheet render right away even
// when nothing the store compares has moved (dismiss, reload done).
function caseJobSnapshot() { return caseJob ? Object.assign({}, caseJob) : null; }
function publishCaseJob() {
  const S = storeState();
  if (S) patchStore({ jobs: Object.assign({}, S.jobs, { caseJob: caseJobSnapshot() }) });
  rerenderJobs();
}
function rerenderJobs() {
  if (typeof renderChip === "function") renderChip();
  renderJobsSheet();
}
function stopCaseJobTicker() {
  if (caseJobTicker) clearInterval(caseJobTicker);
  caseJobTicker = 0;
}
function setJobsNote(text) {
  const el = $("jobsNote");
  if (el) el.textContent = text || "";
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
  const job = { kind, spec, title: spec.title, startedAt: Date.now(), endedAt: 0, async: false, posting: true, sawBusy: false,
    cancel: false, outcome: "", detail: "", message: "", serverElapsed: NaN, serverElapsedAt: 0 };
  caseJob = job;
  setCaseJobButtons(true);
  setJobsNote(`${spec.title} 실행 중…`);
  renderCaseJob();
  // Faster status polling while the request is out.
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
  const job = { kind, spec, title: spec.title, startedAt: Date.now() - el * 1000, endedAt: 0, async: true, posting: false, sawBusy: true,
    cancel: false, outcome: "", detail: caseJobDetail(st, sj), message: "", serverElapsed: el, serverElapsedAt: Date.now(),
    jobInfo: { kind: sj.kind, started_unix: sj.started_unix }, adopted: true, sawServerJob: true };
  caseJob = job;
  setCaseJobButtons(true);
  setJobsNote(`${spec.title} 실행 중…`);
  renderCaseJob();
}

// Hosted viewer refresh after a job (T5's FTViewer.reload): "none" (no
// viewer mounted — the next mount loads fresh data), "reloaded", or
// "deferred" (refused because marks are unsaved, or the reload failed).
async function refreshHostedViewer(reason) {
  const S = storeState();
  const mounted = S ? !!(S.viewer && S.viewer.mounted) : false;
  if (!mounted || typeof FTViewer !== "object" || !FTViewer || typeof FTViewer.reload !== "function") return "none";
  try {
    const r = await FTViewer.reload({ keepUnsaved: true, reason: reason || "" });
    return r === "refused" ? "deferred" : "reloaded";
  } catch (e) {
    return "deferred";
  }
}
const DEFERRED_MSG = "저장되지 않은 판독 내용이 있어 뷰어를 새로 고치지 않았습니다 — 저장이 끝난 뒤 '뷰어 새로 고침'을 누르십시오.";

async function finishCaseJob(job, outcome, error) {
  if (job.outcome) return;
  job.endedAt = Date.now();
  stopCaseJobTicker();
  setCaseJobButtons(false);
  const title = job.spec.title;
  if (outcome === "failed") {
    job.outcome = "failed";
    job.message = String(error || "알 수 없는 오류").split(/\r?\n/)[0].slice(0, 240);
    setJobsNote(`${title} 실패: ${job.message} — 원인을 확인한 뒤 다시 시도할 수 있습니다.`);
    renderCaseJob();
    toast(`${title} 실패: ${job.message}`, "err");
    return;
  }
  if (outcome === "cancelled") {
    job.outcome = "cancelled";
    const why = String(error || "").split(/\r?\n/)[0].slice(0, 240);
    job.message = (why || "작업이 중단되었습니다.") + " 다시 실행할 수 있습니다.";
    setJobsNote(`${title}이(가) 중단되었습니다.`);
    renderCaseJob();
    return;
  }
  job.message = "완료 — 뷰어를 갱신하는 중…";
  job.outcome = "ok";
  renderCaseJob();
  const how = await refreshHostedViewer(job.kind);
  job.refresh = how;
  if (how === "deferred") {
    job.outcome = "deferred";
    job.message = DEFERRED_MSG;
    toast(`${title} 완료 — ${job.message}`, "warn");
  } else {
    job.message = how === "none" ? "완료 — 검토 화면을 열면 결과가 표시됩니다." : "완료 — 뷰어에 결과를 반영했습니다.";
  }
  // Sound measurement may finish with per-file errors (server log line
  // "소리 측정 완료(오류 N건)"): never reported as a clean success.
  if (job.kind === "audio") {
    const logs = typeof lastStatus !== "undefined" && lastStatus && Array.isArray(lastStatus.logs) ? lastStatus.logs : [];
    const hit = logs.map(String).reverse().map(l => /소리 측정 완료\s*\(\s*오류\s*(\d+)\s*건\s*\)/.exec(l)).find(Boolean);
    if (hit && Number(hit[1]) > 0) {
      job.message = `완료 — 오류 ${hit[1]}건: 그 영상은 '측정 실패'(미확인)로 표시됩니다. 로그를 확인하십시오. ` + job.message.replace(/^완료 — /, "");
      toast(`${title} 완료 — 오류 ${hit[1]}건 (로그 확인)`, "warn");
    }
  }
  setJobsNote(`${title} ${job.message}`);
  renderCaseJob();
}

// ---- cancel (always reachable: sheet foot, and the sheet opens with Alt+J) ----
let pipelineCancelAsked = false;
let clipCancelAsked = false;
async function cancelPipelineOrClips(kind) {
  const clips = kind === "clips";
  const S0 = storeState();
  const running = clips && !!S0 ? clipView(S0).running : [];
  const joinsOnly = clips && clipJobsTitle(running) === "영상 이어붙이기";
  const joinsAmong = clips && !joinsOnly && running.some(isJoinJob);
  const go = await askDialog(joinsOnly ? {
    title: "이어붙이기 중단",
    html: "<p>진행 중인 영상 이어붙이기를 중단하시겠습니까?</p><p>이어붙이던 파일은 지웁니다. 원본은 그대로이고, 언제든 다시 실행할 수 있습니다.</p>",
    okLabel: "이어붙이기 중단", cancelLabel: "계속", danger: true, focus: "dlgCancel",
  } : joinsAmong ? {
    title: "내보내기·이어붙이기 중단",
    html: "<p>진행 중인 클립 내보내기와 영상 이어붙이기를 모두 중단하시겠습니까?</p><p>이미 만들어진 클립은 케이스 폴더에 남고, 이어붙이던 파일은 지웁니다. 원본은 그대로입니다.</p>",
    okLabel: "모두 중단", cancelLabel: "계속", danger: true, focus: "dlgCancel",
  } : clips ? {
    title: "클립 내보내기 중단",
    html: "<p>진행 중인 클립 내보내기를 중단하시겠습니까?</p><p>이미 만들어진 파일은 케이스 폴더에 남습니다.</p>",
    okLabel: "내보내기 중단", cancelLabel: "계속", danger: true, focus: "dlgCancel",
  } : {
    title: "분석 중단",
    html: "<p>분석을 중단하시겠습니까?</p><p>중단 후 같은 소스로 '분석 시작'을 다시 누르면 같은 케이스에서 이어서 분석합니다 — 이미 계산된 해시·메타데이터는 다시 계산하지 않고 남은 파일만 처리합니다.</p>",
    okLabel: "분석 중단", cancelLabel: "계속 분석", danger: true, focus: "dlgCancel",
  });
  if (!go) return;
  if (clips) clipCancelAsked = true; else pipelineCancelAsked = true;
  renderJobsSheet();
  const res = await jpost("/api/cancel", {});
  if (!res.ok) {
    if (clips) clipCancelAsked = false; else pipelineCancelAsked = false;
    renderJobsSheet();
    toast("중단 요청 실패: " + (res.error || "알 수 없는 오류"), "err");
    return;
  }
  schedulePoll(400);
}
$("btnCancel").addEventListener("click", () => {
  const S = storeState();
  const clipsOnly = S && !pipelineActive(S) && Number(S.jobs.clipsRunning) > 0;
  cancelPipelineOrClips(clipsOnly ? "clips" : "pipeline");
});
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
    return;
  }
  schedulePoll(400);
});
$("btnRecoverDeleted").addEventListener("click", () => { runCaseJob("recover"); });
$("btnCarve").addEventListener("click", () => { runCaseJob("carve"); });

// ---- reload / dismiss / new case ----
$("btnJobReload").addEventListener("click", async () => {
  const btn = $("btnJobReload");
  const job = caseJob;
  btn.disabled = true;
  btn.setAttribute("aria-busy", "true");
  try {
    const how = await refreshHostedViewer(job && job.kind || "pipeline");
    if (how === "deferred") {
      toast("아직 저장되지 않은 판독 내용이 있습니다 — 저장 상태가 '저장됨'이 된 뒤 다시 누르십시오.", "warn");
      return;
    }
    if (job && job.outcome === "deferred") {
      job.outcome = "ok";
      job.message = "완료 — 뷰어에 결과를 반영했습니다.";
    }
    pipelineReloadDone();
    renderCaseJob();
  } finally {
    btn.disabled = false;
    btn.removeAttribute("aria-busy");
  }
});
$("btnJobClose").addEventListener("click", () => {
  if (caseJob && caseJob.outcome && caseJob.outcome !== "deferred") caseJob = null;
  pipelineDismiss();
  renderCaseJob();
});
$("btnNew").addEventListener("click", async () => {
  // The project popover action owns the unsaved-marks guard and the way
  // back to the empty state (T2); fall back to a guarded reload without it.
  const go = document.getElementById(PROJECT_NEW_ID);
  if (go && typeof go.click === "function") { go.click(); return; }
  const ss = typeof FTViewer === "object" && FTViewer && typeof FTViewer.saveState === "function" ? FTViewer.saveState() : null;
  if (hViewerStateUnsaved(ss, () => { try { return Number((FTViewer.summary() || {}).pending) || 0; } catch (e) { return 1; } })) {
    if (typeof FTViewer.save === "function") { try { FTViewer.save({ patient: true }); } catch (e) { /* surfaced by the viewer */ } }
    toast("아직 저장되지 않은 마크·메모가 있습니다 — 저장 상태가 '저장됨'이 된 뒤 다시 누르십시오.", "warn");
    return;
  }
  try { sessionStorage.setItem("ft_want_new", "1"); } catch (e) { /* storage blocked */ }
  location.reload();
});

// ---- log view: append-only (UX-19) ----
// /api/status returns the last 60 lines. Resetting the <pre> every poll
// wiped the examiner's selection and yanked the scroll position, so only
// the lines past the overlap with what is already shown are appended, and
// auto-scroll happens only while the view is already at the bottom.
let logShown = [];
const LOG_DOM_CAP = 5000;
function logOverlap(prev, next) {
  // Largest k with prev's last k lines == next's first k lines.
  const max = Math.min(prev.length, next.length);
  for (let k = max; k > 0; k--) {
    let same = true;
    for (let i = 0; i < k; i++) {
      if (prev[prev.length - k + i] !== next[i]) { same = false; break; }
    }
    if (same) return k;
  }
  return 0;
}
function logAtBottom(el) {
  return (el.scrollHeight || 0) - (el.scrollTop || 0) - (el.clientHeight || 0) < 24;
}
function appendLogLines(lines) {
  const el = $("log");
  if (!el || !Array.isArray(lines) || !lines.length) return;
  const prevTail = logShown.slice(-lines.length);
  if (prevTail.length === lines.length && prevTail.every((l, i) => l === lines[i])) return;
  const k = logOverlap(prevTail, lines);
  const fresh = lines.slice(k);
  if (!fresh.length) return;
  const stick = logAtBottom(el);
  const text = (logShown.length ? "\n" : "") + fresh.join("\n");
  if (typeof el.appendChild === "function" && typeof document.createTextNode === "function") {
    el.appendChild(document.createTextNode(text));
  } else {
    el.textContent += text;
  }
  logShown = logShown.concat(fresh);
  if (logShown.length > LOG_DOM_CAP) {
    // Rare: trim from the top once, keeping the newest lines.
    logShown = logShown.slice(-Math.floor(LOG_DOM_CAP * 0.8));
    el.textContent = logShown.join("\n");
  }
  if (stick) el.scrollTop = el.scrollHeight;
  else if ($("btnLogBottom")) $("btnLogBottom").hidden = false;
}
function resetLogView() {
  logShown = [];
  const el = $("log");
  if (el) el.textContent = "";
}
$("btnCopyLog").addEventListener("click", async () => {
  const text = $("log").textContent;
  let ok = false;
  try { await navigator.clipboard.writeText(text); ok = true; }
  catch (e) { ok = legacyCopy(text); }
  if (ok) flashLabel($("btnCopyLog"), "복사됨", "복사");
  else toast("클립보드에 복사하지 못했습니다 — '저장'으로 파일로 저장하십시오.", "err");
});
$("btnSaveLog").addEventListener("click", () => {
  const p2 = n => String(n).padStart(2, "0");
  const d = new Date();
  const name = `frametrace-log-${d.getFullYear()}${p2(d.getMonth() + 1)}${p2(d.getDate())}-${p2(d.getHours())}${p2(d.getMinutes())}.txt`;
  const blob = new Blob([$("log").textContent], { type: "text/plain;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url; a.download = name;
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
});
$("log").addEventListener("scroll", () => {
  if (logAtBottom($("log"))) $("btnLogBottom").hidden = true;
});
$("btnLogBottom").addEventListener("click", () => {
  const el = $("log");
  el.scrollTop = el.scrollHeight;
  $("btnLogBottom").hidden = true;
});

// ---- step track ----
function renderTrack(steps, names, progress, warnings) {
  const el = $("track");
  if (!el) return;
  const labels = (names && names.length === 5) ? names : STEP_NAMES;
  el.innerHTML = labels.map((name, i) => {
    const s = steps[i] || "pending";
    // R4 B16: "partial" = the step finished with a partial result (child
    // exit code 5) — a warning state, neither done nor failed.
    const partial = s === "partial";
    const cls = s === "done" || partial ? "done" : s === "running" ? "running" : s === "failed" ? "failed" : "";
    let extra = "";
    const ws = (warnings || []).filter(w => w && w.step === i && w.message);
    if (s === "running" && progress && progress.job_type === "raw-rehash") {
      // R6 (S4): raw re-verification — bytes, not files.
      const total = Number(progress.total) || 0;
      const pct = total > 0 ? Math.min(100, Math.floor((Number(progress.done) || 0) * 100 / total)) : 0;
      extra = `<span class="h-prog"><span class="h-bar${total > 0 ? "" : " indet"}" role="progressbar" aria-valuemin="0" aria-valuemax="100"${total > 0 ? ` aria-valuenow="${pct}"` : ""} aria-label="원본 재검증 진행률"><i${total > 0 ? ` style="width:${pct}%"` : ""}></i></span><span class="h-ptext mono num">${esc(rawRehashText(progress))}</span></span>`;
    } else if (s === "running" && progress && progress.total > 0) {
      const pct = Math.min(100, Math.round(progress.done * 100 / progress.total));
      const eta = progress.eta_secs > 0 ? ` · 남은 시간 ${fmtEta(progress.eta_secs)}` : ` · 계산 중`;
      extra = `<span class="h-prog"><span class="h-bar" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${pct}" aria-label="${esc(name)} 진행률"><i style="width:${pct}%"></i></span><span class="h-ptext mono num">${progress.done.toLocaleString()}/${progress.total.toLocaleString()} (${pct}%)${eta}</span></span>`;
    } else if (s === "running" && progress) {
      const bytes = progress.done > 0 ? `${fmtBytes(progress.done)} 기록됨 · ` : "";
      extra = `<span class="h-prog"><span class="h-bar indet" role="progressbar" aria-label="${esc(name)} 진행 중"><i></i></span><span class="h-ptext mono num">${bytes}${esc(progress.job_type)} · ${fmtClock(progress.elapsed_secs || 0)}</span></span>`;
    }
    if (partial) {
      extra = `<span class="h-trk-warn">부분 결과로 끝났습니다 — 일부만 조사되었습니다</span>` +
        (ws.length ? `<span class="h-wlist">${ws.map(w => `<span>• ${esc(w.message)}</span>`).join("")}</span>` : "");
    } else if (s === "done" && ws.length) {
      extra = `<span class="h-trk-warn">경고 ${ws.length}건과 함께 완료</span>` +
        `<span class="h-wlist">${ws.map(w => `<span>• ${esc(w.message)}</span>`).join("")}</span>`;
    }
    const state = partial ? "부분 결과(경고)" : s === "done" ? (ws.length ? "완료(경고)" : "완료") : s === "running" ? "진행 중" : s === "failed" ? "실패" : "대기";
    return `<li class="${cls}${(s === "done" && ws.length) || partial ? " warned" : ""}"${partial ? ' data-step="partial"' : ""}><span class="h-trk-dot" aria-hidden="true"></span><span class="h-trk-lbl">${esc(name)}</span><span class="sr-only">${state}</span>${extra}</li>`;
  }).join("");
}

// ---- sheet render (store -> DOM) ----
// The output file name once known; before that "이어붙이기 #3" / "클립 #2"
// (server ids are "join-3" / "clip-2" — the prefix is not shown twice).
function clipName(job) {
  const p = String(job && job.path || "").split(/[\\/]/).filter(Boolean).pop();
  if (p) return p;
  const id = String(job && job.id != null ? job.id : "").replace(/^(join|clip)-/, "");
  return `${isJoinJob(job) ? "이어붙이기" : "클립"} #${id}`;
}
// State words per job kind (clip_jobs also carries lossless joins).
const CLIP_STATE = { running: "내보내는 중", done: "완료", failed: "실패", cancelled: "중단됨" };
const JOIN_STATE = { running: "이어붙이는 중", done: "완료", failed: "실패", cancelled: "중단됨" };
// Running join: hash i/N → concat (.part bytes / sources total) → verify.
function joinStageText(c) {
  const raw = c.stage && typeof c.stage === "object" ? c.stage : { stage: c.stage, done: c.stage_done ?? c.done, total: c.stage_total ?? c.total };
  const stage = String(raw.stage || "");
  const clock = fmtClock(c.elapsed_secs);
  if (stage === "hash" && Number(raw.total) > 0) return `해시 ${Number(raw.done) || 0}/${Number(raw.total)} · ${clock}`;
  if (stage === "concat" && Number(c.expected_bytes) > 0) return `${Math.min(100, Math.floor((Number(c.bytes_written) || 0) * 100 / Number(c.expected_bytes)))}% · ${clock}`;
  if (stage === "verify") return `결과 확인 · ${clock}`;
  return clock;
}
function renderClips(S) {
  const sec = $("jobsClips"), list = $("jobsClipList");
  if (!sec || !list) return;
  const clips = (S.jobs.clipJobs || []).slice(0, 8);
  sec.hidden = clips.length === 0;
  const head = typeof sec.querySelector === "function" ? sec.querySelector("h3") : null;
  const title = clipJobsTitle(clips);
  if (head && head.textContent !== title) head.textContent = title;
  if (typeof sec.setAttribute === "function") sec.setAttribute("aria-label", title);
  list.innerHTML = clips.map(c => {
    const st = String(c.state || "");
    const join = isJoinJob(c);
    const tail = st === "running" ? (join ? joinStageText(c) : `${fmtBytes(Number(c.bytes_written) || 0)} 기록됨 · ${fmtClock(c.elapsed_secs)}`)
      : st === "done" ? `${fmtBytes(Number(c.size_bytes) || 0)} · ${fmtClock(c.elapsed_secs)}`
      : st === "failed" || st === "cancelled" ? esc(String(c.error || "").split(/\r?\n/)[0].slice(0, 160)) : "";
    return `<li class="${esc(st)}"><span class="h-clip-name" title="${esc(c.path || "")}">${esc(clipName(c))}</span>` +
      `<span class="h-clip-state">${esc((join ? JOIN_STATE : CLIP_STATE)[st] || st)}</span>` +
      (tail ? `<span class="h-clip-tail mono num">${tail}</span>` : "") + `</li>`;
  }).join("");
}
function jobsNoteText(S, pv) {
  const cj = S.jobs.caseJob;
  if (pv.state === "running") return "분석 중 — 색인된 영상은 바로 검토할 수 있습니다.";
  if (pv.state === "enriching") return "해시·검증 보강 중 — 검토는 계속할 수 있습니다.";
  if (cj && !cj.outcome) return cj.cancel ? `${cj.title} 중단 요청 중…` : `${cj.title} 실행 중…`;
  if (pv.state === "stopped") return "보강이 중단되었습니다 — 같은 소스로 다시 분석하면 이미 계산한 해시는 재사용하고 이어서 진행합니다.";
  if (pv.state === "cancelled") return "분석이 중단되었습니다 — 같은 소스로 다시 분석하면 이어서 진행합니다.";
  if (cj && cj.outcome === "failed") return `${cj.title} 실패 — 원인을 확인한 뒤 다시 시도할 수 있습니다.`;
  if (cj && cj.outcome === "cancelled") return `${cj.title}이(가) 중단되었습니다.`;
  if (cj && (cj.outcome === "ok" || cj.outcome === "deferred")) return `${cj.title} ${cj.message}`;
  if (Number(S.jobs.clipsRunning) > 0) return `${clipJobsTitle(clipView(S).running)} 진행 중…`;
  if (joinReloadPending()) return "이어붙이기 완료 — '새 데이터 반영'을 누르면 목록에 표시됩니다.";
  return "";
}
// A10: the failure box is rewritten only when its markup string changes —
// compared with what was last WRITTEN, never with innerHTML (the browser
// re-serializes it: an escaped quote reads back as a plain one, so the
// read-back never matched and every poll rebuilt the box, folding the
// examiner's opened "자세한 오류 내용"). Same rule as 40-status.js.
let jobsErrWritten = null;
function jobsErrWrite(el, html) {
  if (!el) return false;
  if (jobsErrWritten === html && (el.innerHTML !== "" || html === "")) return false;
  jobsErrWritten = html;
  el.innerHTML = html;
  return true;
}
function renderJobsSheet() {
  const S = storeState();
  if (!S || !$("track")) return;
  const pv = pipelineView(S);
  const p = S.jobs.pipeline || {};
  const cj = S.jobs.caseJob;
  const caseRunning = !!cj && !cj.outcome;

  const pipeSec = $("jobsPipe");
  // The five-step track is for work in flight or needing attention; a
  // clean finished run is just noise (the log keeps the record).
  const steps5 = p.steps || [];
  const hasTrack = steps5.length > 0 && S.phase !== "empty" &&
    (pv.active || steps5.some(x => x !== "done") || (p.warnings || []).length > 0);
  if (pipeSec) pipeSec.hidden = !hasTrack;
  if (hasTrack) renderTrack(p.steps || [], p.stepNames || [], p.progress, p.warnings);
  const counts = $("jobsCounts");
  if (counts) counts.textContent = indexCountText(S);
  setJobsNote(jobsNoteText(S, pv));

  // Failure box: never silent. Pipeline failure shows the hint + raw
  // error; a failed case job shows its message.
  const err = $("jobsErr");
  if (err) {
    let html = "";
    if (pv.state === "failed" || (pv.state === "stopped" && p.error)) html = errorHtml(p.error);
    else if (cj && cj.outcome === "failed") html = rawMismatchError(cj.message) ? errorHtml(cj.message) : `<div>오류: ${esc(cj.message)}</div>`;
    jobsErrWrite(err, html);
    err.hidden = !html;
  }

  renderClips(S);

  // Foot buttons: one cancel per kind of work, a reload when fresh data
  // waits behind unsaved marks, dismiss for finished notices.
  const clipsRunning = Number(S.jobs.clipsRunning) > 0;
  const cancelPipe = $("btnCancel");
  if (cancelPipe) {
    const show = pv.active || (clipsRunning && !caseRunning);
    cancelPipe.hidden = !show;
    const asked = pv.active ? pipelineCancelAsked : clipCancelAsked;
    cancelPipe.disabled = !!asked;
    if (asked) cancelPipe.setAttribute("aria-busy", "true"); else cancelPipe.removeAttribute("aria-busy");
    const clipTitle = clipJobsTitle(clipView(S).running);
    const clipWord = clipTitle === "영상 이어붙이기" ? "이어붙이기 중단" : clipTitle === "내보내기·이어붙이기" ? "내보내기·이어붙이기 중단" : "내보내기 중단";
    cancelPipe.textContent = asked ? "중단 요청 중…" : pv.active ? (pv.state === "enriching" ? "보강 중단" : "분석 중단") : clipWord;
    if (!pv.active && !clipsRunning) clipCancelAsked = false;
  }
  if (!pv.active) pipelineCancelAsked = false;
  const cancelCase = $("btnJobCancel");
  if (cancelCase) {
    cancelCase.hidden = !caseRunning;
    cancelCase.disabled = !!(cj && cj.cancel);
    if (cj && cj.cancel) cancelCase.setAttribute("aria-busy", "true"); else cancelCase.removeAttribute("aria-busy");
    cancelCase.textContent = cj && cj.cancel ? "중단 요청 중…" : "작업 중단";
  }
  const mounted = !!(S.viewer && S.viewer.mounted);
  const reloadBtn = $("btnJobReload");
  if (reloadBtn) {
    const offer = liveReloadOffer(S);
    reloadBtn.hidden = !((cj && cj.outcome === "deferred") || ((pipelineReloadPending() || offer) && mounted));
    reloadBtn.textContent = cj && cj.outcome === "deferred" ? "뷰어 새로 고침"
      : offer && offer.total ? `새 데이터 반영 (${fmtCount(offer.total)}건)` : "새 데이터 반영";
    reloadBtn.title = offer && !offer.canReload ? "저장되지 않은 판독 내용이 있습니다 — 저장된 뒤 반영됩니다" : "";
  }
  const closeBtn = $("btnJobClose");
  if (closeBtn) {
    closeBtn.hidden = !((cj && cj.outcome && cj.outcome !== "deferred") || pv.dismissible || joinReloadPending());
  }
  const newBtn = $("btnNew");
  if (newBtn) newBtn.hidden = !(S.project && S.project.caseDir) || pv.active || caseRunning;
  // An empty foot is not drawn (only necessary buttons).
  const foot = $("jobsFoot");
  if (foot) foot.hidden = ![reloadBtn, closeBtn, newBtn, cancelPipe, cancelCase].some(b => b && !b.hidden);
}
