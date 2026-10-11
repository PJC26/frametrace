// @interim(T2): the stage rail, breadcrumb and per-stage views are gone —
// one integrated screen renders from FTStore (97-shell.js). T3 rewrites
// this file (status polling -> FTStore.set); until then the poll below
// maps each /api/status reply with hStoreFromStatus() (01-store.js).
function setStep() { /* compat no-op: no step rail any more */ }
function caseBaseName(caseDir) {
  return String(caseDir || "").replace(/[\\/]+$/, "").split(/[\\/]/).filter(Boolean).pop() || "";
}
// One-shot latches so an automatic action happens once per state.
let jobSeen = false, reviewReady = false, autoProgress = false, autoReviewed = false;
// True while the last status was review-partial (UX-02) — detects the
// enrichment -> review-ready transition under an open viewer.
let reviewPartialSeen = false;
// Last phase reported by /api/status. While it is review-partial the
// review/ folder (evidence-viewer.html) is not written yet, so the legacy
// viewer frame opens the server's built-in /viewer/ instead.
let lastPhase = "";
function standaloneViewerUrl() {
  return lastPhase === "review-partial" ? "/viewer/" : "/review/evidence-viewer.html";
}

// Controls that loadEnv() disabled for a missing tool carry
// data-env-off and must stay disabled when a finished run re-enables the
// form (otherwise the E01 radio comes back without libewf installed).
function setInputsDisabled(disabled) {
  // The evidence form and the recent-project buttons lock while a job
  // runs.
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

function renderTrack(steps, names, progress, warnings) {
  const labels = (names && names.length === 5) ? names : STEP_NAMES;
  $("track").innerHTML = labels.map((name, i) => {
    const s = steps[i] || "pending";
    const cls = s === "done" ? "done" : s === "running" ? "running" : s === "failed" ? "failed" : "";
    let extra = s === "running" ? " …" : "";
    const ws = (warnings || []).filter(w => w && w.step === i && w.message);
    if (s === "running" && progress && progress.total > 0) {
      const pct = Math.min(100, Math.round(progress.done * 100 / progress.total));
      const eta = progress.eta_secs > 0 ? ` · 남은 시간 ${fmtEta(progress.eta_secs)}` : ` · 계산 중`;
      extra = `<span class="prog"><span class="bar"><i style="width:${pct}%"></i></span><span class="ptext">${progress.done.toLocaleString()}/${progress.total.toLocaleString()} (${pct}%)${eta}</span></span>`;
    } else if (s === "running" && progress) {
      const bytes = progress.done > 0 ? `${fmtBytes(progress.done)} 기록됨 · ` : "";
      extra = `<span class="prog"><span class="bar indet"><i></i></span><span class="ptext">${bytes}${esc(progress.job_type)} · ${Math.floor((progress.elapsed_secs||0)/60)}:${String((progress.elapsed_secs||0)%60).padStart(2,"0")}</span></span>`;
    }
    if (s === "done" && ws.length) {
      extra = ` <span class="badge warn" title="경고와 함께 완료">경고 ${ws.length}건과 함께 완료</span>` +
        `<span class="wlist">${ws.map(w => `<span>• ${esc(w.message)}</span>`).join("")}</span>`;
    }
    return `<li class="${cls}${s === "done" && ws.length ? " warned" : ""}"><span class="dot"></span><span class="lbl">${esc(name)}</span>${extra}</li>`;
  }).join("");
}

// @interim(T2): the analysis shows in the same top-strip chip as the
// background case jobs (65-jobs.js renderCaseJob falls back here when no
// case job is on display). T3's progress chip replaces both.
function renderPipelineChip() {
  const panel = $("jobPanel");
  if (!panel) return false;
  const st = lastStatus;
  const partial = !!st && st.phase === "review-partial";
  const running = !!st && (st.phase === "running" || (partial && !!st.busy));
  const failed = !!st && st.phase === "error";
  // With a project open the chip stays as the jobs sheet opener (복구 ·
  // 카빙 start there); the empty state hides it (00-layout.css).
  const idle = !running && !failed && !!st && !!st.case_dir && H_REVIEW_PHASES.includes(st.phase === "finalizing" ? "review-ready" : st.phase);
  if (!running && !failed && !idle) { panel.hidden = true; return false; }
  panel.hidden = false;
  panel.className = "h-chip " + (failed ? "failed" : running ? "running" : "ok");
  if (idle) {
    $("jobTitle").textContent = partial ? "보강 미완료" : "분석 완료";
    $("jobDetail").textContent = "";
    $("jobElapsed").textContent = "";
    panel.title = "작업 — 진행 기록 · 삭제 영상 복구 · 카빙 (Alt+J)";
    return true;
  }
  const steps = st.steps || [];
  const names = (st.step_names && st.step_names.length === 5) ? st.step_names : STEP_NAMES;
  const idx = steps.findIndex(x => x === "running");
  let title = "분석 중", detail = "";
  if (failed) { title = "분석 실패"; detail = String(st.error || "").split(/\r?\n/)[0].slice(0, 160); }
  else if (partial) { const e = st.enrichment; title = "보강 중"; detail = e ? `${e.label} ${e.stage}/${e.stages}` : ""; }
  else if (idx >= 0) detail = `단계 ${idx + 1}/${names.length} · ${names[idx]}`;
  const p = st.progress;
  if (!failed && p && p.total > 0) detail += `${detail ? " · " : ""}${Number(p.done).toLocaleString()}/${Number(p.total).toLocaleString()}`;
  $("jobTitle").textContent = title;
  $("jobDetail").textContent = detail;
  const el = p && Number.isFinite(Number(p.elapsed_secs)) ? Number(p.elapsed_secs) : null;
  $("jobElapsed").textContent = el !== null && !failed ? "경과 " + fmtClock(el) : "";
  panel.title = title + (detail ? " — " + detail : "");
  return true;
}

let finalizing = false;
let auditVerified = false;
async function verifyAudit() {
  const badge = $("auditBadge");
  if (!badge) return;
  try {
    const res = await jpost("/api/verify-audit", {});
    if (!res.ok) { badge.className = "badge bad"; badge.textContent = "감사 로그 검증 실패"; syncToolsBtn(); return; }
    const keyed = res.integrity === "integrity-keyed";
    badge.className = "badge ok";
    badge.textContent = `감사 로그 ${keyed ? "키 서명 검증됨" : "구조 검증됨"} (${res.verified}개 로그, ${res.entries}항목)`;
    badge.title = (res.logs || []).map(l => `${l.name}: ${l.integrity}, ${l.entries}항목`).join("\n");
  } catch (e) { badge.className = "badge bad"; badge.textContent = "감사 로그 검증 불가"; }
  syncToolsBtn();
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
  if (!Array.isArray(lines) || !lines.length) return;
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
  $("log").textContent = "";
}

// ---- server-lost banner (UX-05) ----
let pollFailures = 0;
let lastInstance = null;
function setServerLost(lost, detail) {
  const banner = $("serverBanner");
  banner.hidden = !lost;
  FTStore.set({ server: { lost: !!lost, failures: pollFailures } });
  if (lost && detail) $("serverBannerMsg").textContent = detail;
}
function pollFailed(err) {
  pollFailures += 1;
  if (pollFailures >= 3) {
    setServerLost(true,
      `frametrace-app이 실행 중인지 확인하십시오 — ${pollFailures}회 연속 응답 없음, 자동으로 다시 연결을 시도합니다.` +
      (err && err.message ? ` (${err.message})` : ""));
  }
}

let shuttingDown = false;
let pollTimer = 0;
function schedulePoll(ms) {
  clearTimeout(pollTimer);
  // Back off while the server is unreachable so a dead server is not
  // hammered, but never longer than 10 s so reconnects stay snappy.
  const delay = pollFailures > 0 ? Math.min(10000, 1200 * Math.pow(1.6, Math.min(pollFailures, 6))) : ms;
  pollTimer = setTimeout(poll, delay);
}
let lastStatus = null;
let lastHomeKey = null;
async function poll() {
  if (shuttingDown) return;
  let st;
  try {
    st = await jget("/api/status");
  } catch (err) {
    pollFailed(err);
    hOnStatus(null);
    schedulePoll(1200);
    return;
  }
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
  try {
    FTStore.set(hStoreFromStatus(st));
    hOnStatus(st);
  } catch (err) {
    console.error("shell render failed:", err);
  }
  try {
    // A phase change (analysis finished, case opened) refreshes the
    // 이전 프로젝트 list so it is current before the examiner returns.
    const homeKey = `${st.case_dir || ""}|${st.phase || ""}|${!!st.busy}`;
    if (homeKey !== lastHomeKey) {
      if (lastHomeKey !== null) renderRecentCases();
      lastHomeKey = homeKey;
    }
  } catch (err) {
    console.error("project list update failed:", err);
  }
  schedulePoll(st.has_job ? 1200 : 1500);
}
$("btnServerRetry").addEventListener("click", () => { pollFailures = Math.min(pollFailures, 2); schedulePoll(0); poll(); });

function applyStatus(st) {
    if (!st.has_job) return;
    lastPhase = st.phase;
    jobSeen = true;
    renderTrack(st.steps || [], st.step_names, st.progress, st.warnings);
    appendLogLines(st.logs || []);
    // UX-02: phase 2 (enrichment) runs under review-partial with busy set —
    // the job is still active (cancel visible, inputs locked) although the
    // review stage is already open.
    const partial = st.phase === "review-partial";
    const enriching = partial && !!st.busy;
    const running = st.phase === "running" || enriching;
    setInputsDisabled(running);
    $("btnStart").disabled = running || finalizing;
    $("btnStart").title = running ? "분석이 진행 중입니다 — 상단 작업 표시에서 상황을 보거나 중단할 수 있습니다"
      : finalizing ? "보고서 생성이 진행 중입니다" : "";
    $("btnCancel").style.display = running ? "" : "none";
    if (!running && !cancelRequested) { $("btnCancel").disabled = false; $("btnCancel").removeAttribute("aria-busy"); $("btnCancel").textContent = "분석 중단"; }
    $("btnNew").style.display = (!running && (st.error || st.phase !== "running")) ? "" : "none";
    if (running) autoProgress = true;
    if (partial) {
      reviewReady = true;
      if (!autoReviewed && enriching) {
        autoReviewed = true;
        toast("1단계 색인 완료 — 해시·검증이 끝나기 전에도 검토를 시작할 수 있습니다.");
      }
      setStep(3, enriching ? 1 : 2);
      const fh = $("finalHint");
      if (!finalizing) {
        const e = st.enrichment;
        fh.textContent = enriching
          ? `보강 진행 중${e ? ` (${e.label} ${e.stage}/${e.stages})` : ""} — 끝나면 결과 보고서를 생성할 수 있습니다.`
          : "보강이 완료되지 않았습니다 (해시·검증·리뷰 생성 미완료) — 같은 소스로 다시 분석하면 이미 계산된 해시는 재사용하고 남은 파일만 처리합니다.";
      }
    } else if (st.phase === "review-ready" || st.phase === "done" || st.phase === "finalized") {
      if (!auditVerified) { auditVerified = true; verifyAudit(); }
      if (reviewPartialSeen) {
        // Enrichment finished under an open review: keep the viewer (and
        // any unsaved marks in it); its chip offers the reload.
        reviewPartialSeen = false;
        toast("보강 완료 — 뷰어 오른쪽 아래 '새 데이터 반영'으로 해시·검증 결과를 불러오십시오.");
      }
      reviewReady = true;
      autoReviewed = true;
      setStep(st.phase === "running" ? 4 : 4, 3);
      // Only fill an idle hint — never overwrite a progress/failure message.
      const fh = $("finalHint");
      if (!finalizing && (!fh.textContent || fh.textContent.startsWith("케이스: ") || fh.textContent.startsWith("보강"))) {
        fh.textContent = st.case_dir ? ("케이스: " + st.case_dir) : "";
      }
      if ((st.phase === "done" || st.phase === "finalized") && st.package_dir) {
        $("btnFinalize").disabled = false;
        $("btnFinalize").textContent = "보고서 재생성";
        $("pkgPath").classList.add("show");
        $("pkgPath").textContent = "패키지: " + st.package_dir;
        if (!$("finalLinks").innerHTML) {
          $("finalLinks").innerHTML =
            `<a href="/case/reports/case-report.html" target="_blank"><b>최종 결과 보고서 열기</b><span>발견 원본 영상 · 메타데이터 · SHA-256 · 발견/분석 기법 · 처리 체인</span></a>` +
            pkgExplorerLink(st.package_dir);
        }
      }
    } else if (running) {
      setStep(3, 2);
    }
    if (partial) reviewPartialSeen = true;
    // UX-12: Recover/Carve only make sense once a case is open for review.
    setCaseToolsVisible(reviewReady && !!st.case_dir);
    renderCaseJob();
    const errEl = $("startErr");
    if (st.error) {
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
let shownStatusError = null, shownStatusHtml = "";
// Action hint first (what to do now), raw CLI output behind a disclosure.
function errorHtml(raw) {
  const text = String(raw || "");
  const first = text.split(/\r?\n/)[0].slice(0, 240);
  return `<div>오류: ${esc(first)}</div><div class="hintline">${esc(errorHint(text))}</div>` +
    (text.length > first.length ? `<details><summary>자세한 오류 내용</summary><pre>${esc(text)}</pre></details>` : "");
}

