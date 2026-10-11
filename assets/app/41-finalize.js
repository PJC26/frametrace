// ---- 결과물 시트 (T4, docs/ui-rebuild-contract.md 2.2 / 6.3 / 8.2) ----
// Rendered from JS into the empty mount <div id="hFinalizeSheet"> that the
// host layout (T2) provides; T2 owns open/close/focus-trap/inert, this file
// owns everything inside. One question is answered above all: "may a report
// be produced right now?" — a verdict line, a checklist whose rows are
// block / warn / info / ok, and exactly ONE primary button (#btnFinalize),
// disabled with a reason while any block row exists. Everything else
// (DFXML, timeline, QA, compare/merge, known-hash, verify-sources,
// verify-audit, external marks import) lives in the collapsed #finalAdvanced.
//
// Data sources (nothing new on the server): /api/status, /api/records-meta,
// /api/verify-audit, /api/env, FTViewer (save state / summary / save) and the
// viewer's per-case localStorage drafts. The shell never touches viewer DOM.
//
// Top-level names here start with fin/FIN/unsaved/verifySources/pkgExplorer
// (the shell must not use ft*/loader*/viewer*/live* — contract 2.1).
const FIN = {
  built: false, open: false, loading: false, busy: false, gen: 0,
  checks: [], caseId: null, status: null, env: null,
  result: null, verify: null, audit: null, examinerTouched: false,
  bound: false, timer: 0, caseDir: null,
};
const FIN_LEVEL = { block: { icon: "i-x", word: "차단" }, warn: { icon: "i-warning", word: "주의" }, info: { icon: "i-info", word: "정보" }, ok: { icon: "i-check", word: "완료" } };
const FIN_ADV = [
  ["dfxml", "DFXML 내보내기", ""],
  ["timeline", "타임라인 JSONL", ""],
  ["qa-consistency", "색인 일관성 QA", ""],
  ["qa-defense", "보고 방어 체크", ""],
  ["deepfake", "딥페이크 스크리닝", "케이스의 모든 파일(색인·카빙·복구)을 deepfake-lens로 스크리닝 — 결과는 뷰어의 '합성의심' 배지로 표시. 판정이 아니라 검토 우선순위입니다"],
];
const FIN_ADV_CASE = [["compare", "케이스 비교"], ["merge", "케이스 병합"], ["known-hash", "기지원 해시 필터"]];

function finEl(id) { return document.getElementById(id); }
function finMount() { return finEl("hFinalizeSheet"); }
function finViewer() { return typeof window !== "undefined" && window.FTViewer ? window.FTViewer : null; }
function readLs(key) {
  try { const raw = localStorage.getItem(key); return raw ? JSON.parse(raw) : null; } catch (e) { return null; }
}
// "saved" | "pending" | "saving" | "failed" | "queued" | "local" | null.
function finViewerSaveState() {
  const v = finViewer();
  try { return v && typeof v.saveState === "function" ? (v.saveState() || null) : null; } catch (e) { return null; }
}
// Bridge helpers other shell files call (project-switch guard, quit guard).
// Hosted viewer only — the shell never reads viewer DOM (contract 3).
function viewerWindow() { return null; }
function viewerSaveState() { return finViewerSaveState(); }
// Starts a save (or retry) now; true when a viewer accepted it.
function retryViewerSave() {
  const v = finViewer();
  if (v && typeof v.save === "function") { Promise.resolve(v.save()).catch(() => {}); return true; }
  return false;
}
function finViewerSummary() {
  const v = finViewer();
  try { return v && typeof v.summary === "function" ? v.summary() : null; } catch (e) { return null; }
}

// Pure: finalize checklist rows for unsaved marks. `retry: true` rows get
// the sheet's "저장 다시 시도" button.
function unsavedMarksChecks({ pendingMarks, saveState, drafts }) {
  const out = [];
  const add = (level, text, sub, retry) => out.push({ level, text, sub: sub || "", retry: !!retry });
  if (pendingMarks === null) add("block", "서버의 마크 저장 대기열을 확인할 수 없습니다", "연결이 복구된 뒤 '다시 확인'을 누르십시오.");
  else if (pendingMarks > 0) add("block", `케이스 DB에 아직 반영되지 않은 저장 대기 마크 ${pendingMarks}건`,
    "보강 작업이 끝나면 자동 반영됩니다 — 잠시 뒤 '다시 확인'을 누르십시오.");
  if (saveState === "failed") add("block", "뷰어의 마크·메모 저장이 실패했습니다",
    "최신 판독 내용 없이 보고서를 봉인할 수 없습니다 — '저장 다시 시도'를 누르십시오.", true);
  else if (saveState === "pending" || saveState === "saving") add("block", "뷰어에 아직 저장되지 않은 마크·메모 변경이 있습니다",
    "'저장 다시 시도'로 지금 저장한 뒤 '다시 확인'을 누르십시오.", true);
  else if (saveState === "queued") add("block", "마크가 서버 대기열에 있습니다 (보강 작업 중)", "보강 작업이 끝난 뒤 다시 확인하십시오.");
  if (drafts > 0 && !out.some(c => c.retry)) add("block", `브라우저에만 남아 있는 마크·메모 변경 ${drafts}건`,
    "'저장 다시 시도'로 저장을 완료하십시오.", true);
  if (drafts === null && saveState === null) add("warn", "뷰어의 미저장 변경을 확인할 수 없습니다", "뷰어가 아직 열리지 않았거나 케이스 정보를 읽지 못했습니다.");
  if (!out.length) add("ok", "마크·메모가 모두 케이스에 반영되었습니다");
  return out;
}

// SHOULD-FIX 4: "all match" only when something was actually compared and
// nothing was left uncompared — 0 matched / N not comparable is a warning,
// never a green pass.
function verifySourcesVerdict(res) {
  const matched = Number(res.matched) || 0;
  const mismatched = Number(res.mismatched) || 0;
  const notComparable = Number(res.not_comparable) || 0;
  const counts = `일치 ${matched} · 불일치 ${mismatched} · 비교 불가 ${notComparable}`;
  if (mismatched > 0) {
    return { level: "err", toast: `원본 재검증: 불일치 ${mismatched}건`,
      hint: `원본 재검증 완료 — ${counts} (불일치 항목은 보관 이력서 5절에서 기록값/관측값을 비교하십시오)` };
  }
  if (matched === 0) {
    return { level: "warn", toast: "원본 재검증: 비교된 파일 없음 — 재검증되지 않았습니다",
      hint: `원본 재검증 완료 — ${counts}. 기록된 해시와 비교할 수 있는 파일이 없어 아무것도 재검증되지 않았습니다` +
        (notComparable ? " (원본에 접근할 수 없거나, 읽을 수 없거나, 해시 없이 색인됨 — 해시 포함으로 다시 분석하십시오)." : ".") };
  }
  if (notComparable > 0) {
    return { level: "warn", toast: `원본 재검증: 일치 ${matched}건, 비교 불가 ${notComparable}건`,
      hint: `원본 재검증 완료 — ${counts}. 비교 불가 ${notComparable}건은 재검증되지 않았습니다 (접근 불가·읽기 실패·기록된 해시 없음 — 보관 이력서 5절 참조).` };
  }
  return { level: "ok", toast: `원본 재검증: ${matched}건 모두 일치`, hint: `원본 재검증 완료 — ${counts}` };
}

// Package dir is a server-side path, not a browsable URL — this control
// POSTs to /api/open-folder so the OS shell opens it only on request.
function pkgExplorerLink(dir) {
  if (!dir) return "";
  return `<button type="button" class="quiet" data-pkgdir="${esc(dir)}">탐색기에서 폴더 열기</button>`;
}

// Pure: what the verdict line, the button state and its tooltip say.
function finSummarize(rows, { loading = false, name = "" } = {}) {
  const blocks = rows.filter(r => r.level === "block");
  const warns = rows.filter(r => r.level === "warn").length;
  let reason = "";
  if (loading) reason = "점검 중입니다";
  else if (blocks.length) reason = blocks[0].text + (blocks[0].sub ? " — " + blocks[0].sub : "");
  else if (!String(name || "").trim()) reason = "검토자 이름을 입력하십시오";
  const state = loading ? "loading" : blocks.length ? "blocked" : "ready";
  const text = loading ? "점검 중…"
    : blocks.length ? `보고서를 낼 수 없습니다 — 해결할 항목 ${blocks.length}건`
    : warns ? `보고서를 낼 수 있습니다 — 확인할 항목 ${warns}건` : "보고서를 낼 수 있습니다";
  return { state, text, reason, blocks: blocks.length, warns, disabled: !!reason };
}

function currentExaminerName(caseId) {
  if (caseId) { const v = readLs(`ft.viewer.${caseId}.examiner`); if (typeof v === "string" && v.trim()) return v.trim(); }
  return (loadPrefs().examinerName || "").trim();
}
function propagateExaminerName(name, caseId) {
  savePrefs({ examinerName: name });
  if (caseId) { try { localStorage.setItem(`ft.viewer.${caseId}.examiner`, JSON.stringify(name)); } catch (e) {} }
  const v = finViewer();
  try { if (v && typeof v.setExaminer === "function") v.setExaminer(name); } catch (e) {}
}

async function finRunAudit() {
  try {
    const res = await jpost("/api/verify-audit", {});
    if (!res.ok) { FIN.audit = { level: "warn", text: "감사 로그 검증에 실패했습니다", title: res.error || "" }; return; }
    const keyed = res.integrity === "integrity-keyed";
    FIN.audit = {
      level: "ok", text: `감사 로그 ${keyed ? "키 서명 검증됨" : "구조 검증됨"} (${res.verified}개 로그, ${res.entries}항목)`,
      title: (res.logs || []).map(l => `${l.name}: ${l.integrity}, ${l.entries}항목`).join("\n"),
    };
  } catch (e) { FIN.audit = { level: "warn", text: "감사 로그 검증 불가", title: String((e && e.message) || e) }; }
}

async function gatherFinalizeChecks() {
  const checks = [];
  const add = (level, text, sub) => checks.push({ level, text, sub: sub || "" });
  let st = null;
  try { st = await jget("/api/status"); } catch (e) { st = null; }
  const phase = st ? String(st.phase || "") : "";
  if (!st) add("block", "서버 상태를 확인할 수 없습니다", "연결이 복구된 뒤 '다시 확인'을 누르십시오.");
  else if (phase === "review-partial") {
    const e = st.enrichment;
    add("block", "분석 보강 중 — 해시·검증이 끝나야 보고서를 낼 수 있습니다",
      st.busy ? (e ? `${e.label} ${e.stage}/${e.stages} 진행 중` : "끝난 뒤 '다시 확인'을 누르십시오.")
        : "보강이 끝나지 않았습니다 — 같은 소스로 다시 분석하면 남은 파일만 처리합니다.");
  } else if (phase === "running") add("block", "분석이 진행 중입니다", "끝난 뒤 '다시 확인'을 누르십시오.");
  else if (st.busy) add("block", "다른 작업이 진행 중입니다", `현재 단계: ${phase || "-"} — 끝난 뒤 다시 확인하십시오.`);
  else if (!["review-ready", "done", "finalized"].includes(phase)) add("block", "완료된 분석 결과가 없습니다", "먼저 분석을 끝내십시오.");
  let meta = null;
  try { meta = await jget("/api/records-meta"); } catch (e) { meta = null; }
  const caseId = meta && meta.manifest ? (meta.manifest.case_id || "case") : null;
  const summary = finViewerSummary();

  // Unsaved / queued marks block the finalize: sealing a package without
  // the examiner's latest marks is a silent loss.
  let drafts = null;
  if (caseId) {
    const stored = readLs(`ft.viewer.${caseId}.annotations.v1`);
    drafts = stored && typeof stored === "object" ? Object.keys(stored).length : 0;
  }
  unsavedMarksChecks({
    pendingMarks: st ? Number(st.pending_marks) || 0 : null,
    saveState: finViewerSaveState(),
    drafts,
  }).forEach(c => checks.push(c));

  const marks = meta && meta.annotations && Array.isArray(meta.annotations.marks) ? meta.annotations.marks : null;
  if (marks) {
    const real = marks.filter(m => m && m.status && m.status !== "noted");
    const waiting = real.filter(m => m.status === "needs_verification").length;
    if (waiting) add("warn", `검증 대기 ${waiting}건`, "보고서에 '검증 대기'로 표시되며 확정 소견이 아닙니다.");
    const impNoNote = real.filter(m => m.status === "important" && !String(m.note || "").trim()).length;
    if (impNoNote) add("warn", `메모 없는 '중요' 마크 ${impNoNote}건`, "중요 판단의 근거를 메모로 남기는 것을 권장합니다.");
    const noExaminer = marks.filter(m => !String(m.examiner || "").trim()).length;
    if (noExaminer) add("warn", `검토자 이름 없이 기록된 마크 ${noExaminer}건`, "이름 입력 이후 수정된 마크부터 검토자가 기록됩니다.");
  }
  if (summary) {
    const total = summary.total || 0;
    const failed = (summary.byStatus && summary.byStatus["validation-failed"]) || 0;
    const cand = (summary.byKind && summary.byKind.candidate) || 0;
    if (failed) add("warn", `검증 실패 항목 ${failed}건`, "보고서에 검증 실패로 표시됩니다.");
    if (cand) add("warn", `복구 전 후보 ${cand}건`, "검증되지 않은 후보는 보고서에서 확정 복구로 주장되지 않습니다.");
    const unmarked = Math.max(0, total - (summary.marked || 0));
    add(unmarked ? "warn" : "ok", unmarked ? `미검토 ${unmarked}건 (전체 ${total}건)` : `전체 ${total}건 모두 검토됨`,
      unmarked ? "마크가 없는 항목은 보고서에 '검토 안 됨'으로 남습니다." : "");
  }
  // Source re-verification: only what actually ran is claimed.
  if (FIN.verify) {
    const lv = FIN.verify.level;
    add(lv === "ok" ? "ok" : "warn", lv === "ok" ? "원본 재검증: 모두 일치" : lv === "err" ? "원본 재검증: 불일치 있음" : "원본 재검증: 일부만 확인됨", FIN.verify.hint);
  } else {
    add("info", "원본 재검증: 아직 실행하지 않음", "패키징할 때 원본 SHA-256을 다시 계산해 일치/불일치를 그대로 기록합니다. 지금 확인하려면 고급 ▸ 원본 재검증.");
  }
  if (!FIN.audit && st && ["review-ready", "done", "finalized"].includes(phase)) await finRunAudit();
  if (FIN.audit) add(FIN.audit.level === "ok" ? "ok" : "warn", FIN.audit.text, "");
  return { checks, caseId, status: st };
}

// ---- rendering ----
function finIcon(id) { return `<svg class="ic" aria-hidden="true"><use href="#${id}"/></svg>`; }
function finRows() {
  const rows = FIN.checks.slice();
  const nameEl = finEl("finalExaminer");
  if (nameEl && !nameEl.value.trim()) rows.unshift({ level: "block", text: "검토자 이름이 필요합니다", sub: "입력한 그대로 보고서와 이후 판독 마크에 기록됩니다." });
  return rows;
}
function finRowHtml(c) {
  const lv = FIN_LEVEL[c.level] || FIN_LEVEL.info;
  return `<li class="h-check h-check-${esc(c.level)}" data-level="${esc(c.level)}">${finIcon(lv.icon)}` +
    `<span class="h-check-text"><span class="sr-only">${lv.word}: </span>${esc(c.text)}` +
    (c.sub ? `<small>${esc(c.sub)}</small>` : "") + `</span></li>`;
}
function finResultHtml() {
  const r = FIN.result;
  const st = FIN.status;
  const pkgDir = (r && r.package_dir) || (st && ["done", "finalized"].includes(st.phase) && st.package_dir) || "";
  if (!r && !pkgDir) return "";
  const pkgUrl = r && r.package_url ? `<a href="/case/${esc(r.package_url)}" target="_blank" rel="noopener">패키지 폴더 보기${finIcon("i-external")}</a>` : "";
  return `<h3 class="h-fin-h">결과</h3><ul class="h-fin-links">` +
    `<li><a href="/case/reports/case-report.html" target="_blank" rel="noopener"><b>보고서 열기</b>${finIcon("i-external")}</a> <span class="muted">새 탭에서 열립니다</span></li>` +
    (pkgDir ? `<li><span class="muted">패키지</span> <code class="mono">${esc(pkgDir)}</code> ${pkgUrl} ${pkgExplorerLink(pkgDir)}</li>` : "") +
    (r && r.manifest_sha256 ? `<li><span class="muted">manifest.sha256의 SHA-256</span> <code class="mono num">${esc(r.manifest_sha256)}</code></li>` : "") +
    (r ? `<li><span class="muted">검토자(입력값 기록)</span> ${esc(r.examiner || "-")}</li>` : "") + `</ul>`;
}
function finBuild() {
  const mount = finMount();
  if (!mount || FIN.built) return !!mount;
  const advBtns = FIN_ADV.map(([k, label, tip]) => `<button type="button" data-adv="${k}"${tip ? ` title="${esc(tip)}"` : ""}>${esc(label)}</button>`).join("");
  const caseBtns = FIN_ADV_CASE.map(([k, label]) => `<button type="button" data-adv="${k}">${esc(label)}</button>`).join("");
  mount.innerHTML =
    `<div class="h-fin">` +
    `<div class="h-fin-head"><h2 id="hFinalizeTitle">결과물</h2>` +
    `<button type="button" class="quiet icon" id="hFinalizeClose" aria-label="결과물 닫기" title="닫기 (Esc)">${finIcon("i-close")}</button></div>` +
    `<div class="h-fin-body">` +
    `<p class="h-fin-verdict" id="finalVerdict" role="status"></p>` +
    `<div class="h-fin-field"><label for="finalExaminer">검토자 이름</label>` +
    `<input type="text" id="finalExaminer" autocomplete="name" placeholder="예: 홍길동 (소속)" aria-describedby="finalNameNote">` +
    `<small id="finalNameNote" class="muted">입력한 그대로 보고서·sign-off·이후 판독 마크에 기록됩니다. 신원 인증은 아닙니다.</small></div>` +
    `<ul class="h-checklist" id="finalChecklist" aria-label="보고서 생성 전 점검"></ul>` +
    `<div class="h-fin-actions"><button type="button" id="btnFinalRetrySave" hidden>저장 다시 시도</button>` +
    `<button type="button" class="quiet" id="btnFinalRecheck">${finIcon("i-refresh")}다시 확인</button></div>` +
    `<div id="finalLinks" class="h-fin-result"></div>` +
    `<details id="finalAdvanced" class="h-fin-adv"><summary>고급</summary><div class="h-fin-adv-body">` +
    `<h3 class="h-fin-h">검증</h3><div class="h-fin-grid">` +
    `<button type="button" id="btnVerifySources" title="색인된 모든 원본을 지금 다시 해시해 기록된 SHA-256과 비교합니다 (evidence/logs/hash-verify.jsonl, 보관 이력서 5절)">원본 재검증 (SHA-256)</button>` +
    `<button type="button" id="btnVerifyAudit" title="체인된 감사 로그의 무결성을 다시 확인합니다">감사 로그 검증</button></div>` +
    `<h3 class="h-fin-h">내보내기 · QA</h3><div class="h-fin-grid">${advBtns}</div>` +
    `<h3 class="h-fin-h">다른 케이스 · 해시 목록</h3>` +
    `<input type="text" id="advPath" placeholder="다른 케이스 폴더 또는 sha256 목록 파일 경로" aria-label="다른 케이스 폴더 또는 해시 목록 경로">` +
    `<div class="h-fin-grid">${caseBtns}</div>` +
    `<h3 class="h-fin-h">외부 마크</h3>` +
    `<input type="file" id="marksFile" accept=".json" hidden>` +
    `<button type="button" id="btnImportMarks" title="다른 PC·오프라인 뷰어에서 내보낸 마크 JSON을 가져올 때만 사용합니다 — 이 화면의 뷰어 마크는 자동으로 케이스에 반영됩니다">외부 마크 파일 가져오기 (보고서 갱신)</button>` +
    `<pre class="h-fin-out mono" id="advOut" hidden></pre><div id="advLink"></div>` +
    `</div></details>` +
    `</div>` +
    `<div class="h-fin-foot"><span class="h-fin-hint muted" id="finalHint" role="status"></span>` +
    `<button type="button" class="primary lg" id="btnFinalize">보고서 생성·패키징</button></div>` +
    `</div>`;
  FIN.built = true;
  mount.addEventListener("click", finOnClick);
  mount.addEventListener("change", e => { if (e.target && e.target.id === "marksFile") finImportMarks(); });
  mount.addEventListener("input", e => {
    if (e.target && e.target.id === "finalExaminer") { FIN.examinerTouched = true; finPaint(); }
  });
  finApplyEnv();
  return true;
}
function finApplyEnv() {
  const mount = finMount();
  const btn = mount && mount.querySelector("button[data-adv='deepfake']");
  if (!btn || !FIN.env) return;
  const ok = FIN.env.deepfake === true;
  btn.dataset.envOff = ok ? "" : "1";
  if (!ok) { btn.disabled = true; btn.title = "deepfake-lens 미설치 — pip install deepfake-lens 또는 FRAMETRACE_DEEPFAKE_LENS 경로 설정"; }
  else if (!FIN.advBusy) btn.disabled = false;
}
// Repaints everything that depends on state; never touches the typed
// examiner name or the <details> open state.
function finPaint() {
  if (!FIN.built) return;
  const rows = finRows();
  const name = finEl("finalExaminer").value;
  const sum = finSummarize(rows, { loading: FIN.loading, name });
  const verdict = finEl("finalVerdict");
  const done = !!FIN.result && sum.state === "ready";
  verdict.className = "h-fin-verdict " + (done ? "done" : sum.state);
  verdict.innerHTML = finIcon(sum.state === "blocked" ? "i-warning" : sum.state === "loading" ? "i-spinner" : "i-check") +
    `<span>${esc(done ? "보고서·패키지를 만들었습니다" : sum.text)}</span>`;
  finEl("finalChecklist").innerHTML = rows.map(finRowHtml).join("");
  finEl("btnFinalRetrySave").hidden = !FIN.checks.some(c => c.retry);
  const btn = finEl("btnFinalize");
  btn.disabled = FIN.busy || sum.disabled;
  btn.title = FIN.busy ? "보고서 생성 중입니다" : sum.reason || "검토자 이름과 점검 결과를 기록해 보고서·패키지를 생성합니다";
  btn.setAttribute("aria-describedby", "finalVerdict");
  if (FIN.busy) btn.setAttribute("aria-busy", "true"); else btn.removeAttribute("aria-busy");
  finEl("btnFinalRecheck").disabled = FIN.busy || FIN.loading;
  finEl("finalLinks").innerHTML = finResultHtml();
  const mount = finMount();
  if (FIN.busy) mount.setAttribute("aria-busy", "true"); else mount.removeAttribute("aria-busy");
}
function finHint(text) { const h = finEl("finalHint"); if (h) h.textContent = text || ""; }

async function finRefresh() {
  if (!finBuild()) return;
  const gen = ++FIN.gen;
  FIN.loading = true;
  finPaint();
  try {
    const state = await gatherFinalizeChecks();
    if (gen !== FIN.gen) return;
    FIN.checks = state.checks; FIN.caseId = state.caseId; FIN.status = state.status;
    const nameEl = finEl("finalExaminer");
    if (!FIN.examinerTouched && !nameEl.value.trim()) nameEl.value = currentExaminerName(state.caseId);
  } catch (err) {
    if (gen !== FIN.gen) return;
    FIN.checks = [{ level: "block", text: "점검 정보를 불러오지 못했습니다", sub: String((err && err.message) || err) }];
  }
  if (gen === FIN.gen) { FIN.loading = false; finPaint(); }
  if (gen === FIN.gen) { try { const e = await jgetSoft("/api/env"); if (e && e.ok !== false) { FIN.env = e; finApplyEnv(); } } catch (x) {} }
}
function finScheduleRefresh() {
  if (!FIN.open || FIN.busy) return;
  clearTimeout(FIN.timer);
  FIN.timer = setTimeout(finRefresh, 300);
}
function finBindViewer() {
  const v = finViewer();
  if (FIN.bound || !v || typeof v.on !== "function") return;
  FIN.bound = true;
  try { v.on("save-state", finScheduleRefresh); v.on("dirty", finScheduleRefresh); } catch (e) { FIN.bound = false; }
}
function finOnOpen() {
  if (FIN.open) return;
  FIN.open = true;
  finBindViewer();
  finRefresh();
}
function finOnClose() { FIN.open = false; clearTimeout(FIN.timer); }
function finIsOpen() { return FIN.open; }
function finIsBusy() { return FIN.busy; }
function finClose() {
  const mount = finMount();
  if (typeof FTStore !== "undefined" && FTStore && FTStore.state && FTStore.state.panels) {
    FTStore.set({ panels: Object.assign({}, FTStore.state.panels, { finalize: false }) });
  } else if (mount) mount.hidden = true;
  finOnClose();
}
// A different case was opened: nothing about the old one may stay visible.
function finResetSheet() {
  FIN.result = null; FIN.verify = null; FIN.audit = null; FIN.checks = []; FIN.status = null;
  FIN.caseId = null; FIN.examinerTouched = false; FIN.gen++;
  if (FIN.built) { finEl("finalExaminer").value = ""; finHint(""); finEl("advOut").hidden = true; finEl("advOut").textContent = ""; finEl("advLink").innerHTML = ""; finPaint(); }
  if (FIN.open) finRefresh();
}
function finSetFinalizing(on) {
  FIN.busy = on;
  try { if (typeof finalizing !== "undefined") finalizing = on; } catch (e) {}
}

// ---- actions ----
async function finFinalize() {
  if (FIN.busy || !FIN.built) return;
  const nameEl = finEl("finalExaminer");
  const name = nameEl.value.trim();
  // Re-check right before sealing: a stale "ready" must never seal a report.
  finSetFinalizing(true);
  FIN.loading = true; finPaint(); finHint("점검 중…");
  let state;
  try { state = await gatherFinalizeChecks(); }
  catch (err) { state = { checks: [{ level: "block", text: "점검 정보를 불러오지 못했습니다", sub: String((err && err.message) || err) }], caseId: FIN.caseId, status: FIN.status }; }
  FIN.checks = state.checks; FIN.caseId = state.caseId; FIN.status = state.status; FIN.loading = false;
  if (state.checks.some(c => c.level === "block") || !name) {
    finSetFinalizing(false); finPaint();
    finHint("해결할 항목이 있어 보고서를 만들지 않았습니다.");
    return;
  }
  propagateExaminerName(name, state.caseId);
  finHint("보고서 생성 중… (원본 해시 재계산 포함 — 소스 크기에 따라 오래 걸릴 수 있습니다)");
  finPaint();
  let res;
  try { res = await jpost("/api/finalize", { examiner: name }); }
  finally { finSetFinalizing(false); }
  if (!res.ok) { finHint("실패: " + (res.error || "알 수 없는 오류")); finPaint(); return; }
  FIN.result = { examiner: res.examiner || name, manifest_sha256: res.manifest_sha256 || "", package_dir: res.package_dir || "", package_url: res.package_url || "" };
  finHint("");
  finPaint();
  toast("보고서·패키지를 만들었습니다", "ok");
}
async function finVerifySources() {
  const btn = finEl("btnVerifySources");
  if (!btn || btn.disabled) return;
  btn.disabled = true; btn.setAttribute("aria-busy", "true");
  finHint("원본 재검증 중… (색인된 모든 원본을 다시 해시합니다 — 오래 걸릴 수 있습니다)");
  try {
    const res = await jpost("/api/verify-sources", {});
    if (!res.ok) { finHint("원본 재검증 실패: " + (res.error || "알 수 없는 오류")); return; }
    const verdict = verifySourcesVerdict(res);
    FIN.verify = { level: verdict.level, hint: verdict.hint };
    finHint("");
    toast(verdict.toast, verdict.level);
    finRefresh();
  } catch (err) {
    finHint("원본 재검증 실패: " + ((err && err.message) || err));
  } finally {
    btn.removeAttribute("aria-busy"); btn.disabled = false;
  }
}
async function finVerifyAudit() {
  const btn = finEl("btnVerifyAudit");
  if (!btn || btn.disabled) return;
  btn.disabled = true; btn.setAttribute("aria-busy", "true");
  try { await finRunAudit(); toast(FIN.audit.text, FIN.audit.level === "ok" ? "ok" : "warn"); finRefresh(); }
  finally { btn.removeAttribute("aria-busy"); btn.disabled = false; }
}
async function finRetrySave() {
  const btn = finEl("btnFinalRetrySave");
  const v = finViewer();
  if (!v || typeof v.save !== "function") { toast("뷰어가 열려 있지 않습니다 — 검토 화면을 연 뒤 저장하십시오.", "warn"); return; }
  btn.disabled = true; btn.setAttribute("aria-busy", "true"); finHint("저장 중…");
  let ok = false;
  try { ok = await v.save(); } catch (e) { ok = false; }
  btn.removeAttribute("aria-busy"); btn.disabled = false; finHint(ok ? "" : "저장하지 못했습니다 — 뷰어의 저장 상태를 확인하십시오.");
  finRefresh();
}
async function finImportMarks() {
  const input = finEl("marksFile");
  const file = input && input.files && input.files[0];
  if (!file) return;
  const btn = finEl("btnImportMarks");
  btn.disabled = true;
  finHint("마크 반영 및 보고서 갱신 중…");
  try {
    let marksJson;
    try { marksJson = await file.text(); }
    catch (err) { finHint("실패: 파일을 읽을 수 없습니다 — " + ((err && err.message) || err)); return; }
    const res = await jpost("/api/import-marks", { marks_json: marksJson });
    finHint(res.ok ? "" : "실패: " + (res.error || "알 수 없는 오류"));
    if (res.ok) {
      finEl("advOut").hidden = false; finEl("advOut").textContent = "보고서에 판독 마크가 반영되었습니다.";
      finEl("advLink").innerHTML = `<a href="/case/${esc(res.report_url || "reports/case-report.html")}" target="_blank" rel="noopener">갱신된 보고서 열기${finIcon("i-external")}</a>`;
    }
  } finally {
    btn.disabled = false;
    input.value = ""; // the same file twice must fire "change" again
  }
}
async function finAdvanced(btn) {
  const tool = btn.dataset.adv;
  const needsPath = tool === "compare" || tool === "merge" || tool === "known-hash";
  const extra = needsPath ? finEl("advPath").value.trim() : "";
  const out = finEl("advOut");
  if (needsPath && !extra) {
    out.hidden = false;
    out.textContent = "경로를 입력하십시오 — " + (tool === "known-hash" ? "sha256 목록 파일" : "비교/병합할 케이스 폴더");
    return;
  }
  if (tool === "merge") {
    const go = await askDialog({
      title: "케이스 병합",
      html: "<p>다른 케이스의 색인을 이 케이스에 병합합니다 (중복은 duplicate로 표시). 계속하시겠습니까?</p>",
      okLabel: "병합",
    });
    if (!go) return;
  }
  btn.disabled = true; FIN.advBusy = true;
  out.hidden = false; out.textContent = "실행 중…";
  finEl("advLink").innerHTML = "";
  try {
    const res = await jpost("/api/advanced", { tool, extra });
    out.textContent = res.ok ? (res.output || "완료") : "실패: " + (res.error || "알 수 없는 오류");
    if (res.ok && res.url) finEl("advLink").innerHTML = `<a href="/case/${esc(res.url)}" target="_blank" rel="noopener">산출물 열기: ${esc(res.url)}${finIcon("i-external")}</a>`;
  } finally {
    FIN.advBusy = false;
    btn.disabled = btn.dataset.envOff === "1"; // a missing tool stays disabled
    if (out.textContent === "실행 중…") out.textContent = "실패: 응답 없음 — 다시 시도할 수 있습니다.";
  }
}
async function finOnClick(e) {
  const t = e.target && e.target.closest ? e.target.closest("button, a[data-pkgdir]") : null;
  if (!t) return;
  if (t.dataset && t.dataset.pkgdir) {
    e.preventDefault();
    const res = await jpost("/api/open-folder", { path: t.dataset.pkgdir });
    if (!res.ok) toast("폴더를 열지 못했습니다: " + (res.error || "알 수 없는 오류") + " — 경로를 직접 여십시오: " + t.dataset.pkgdir, "err");
    return;
  }
  if (t.disabled) return;
  switch (t.id) {
    case "btnFinalize": return finFinalize();
    case "btnFinalRecheck": return finRefresh();
    case "btnFinalRetrySave": return finRetrySave();
    case "btnVerifySources": return finVerifySources();
    case "btnVerifyAudit": return finVerifyAudit();
    case "btnImportMarks": return finEl("marksFile").click();
    case "hFinalizeClose": return finClose();
  }
  if (t.dataset && t.dataset.adv) return finAdvanced(t);
}

// ---- wiring: T2 opens the sheet by un-hiding the mount (and by setting
// FTStore.panels.finalize); both are honoured. ----
(function finWire() {
  const mount = finMount();
  if (mount && typeof MutationObserver === "function") {
    new MutationObserver(() => { if (mount.hidden) finOnClose(); else finOnOpen(); })
      .observe(mount, { attributes: true, attributeFilter: ["hidden"] });
  }
  if (typeof FTStore !== "undefined" && FTStore && typeof FTStore.on === "function") {
    FTStore.on("change", ev => {
      const keys = ev && ev.keys ? ev.keys : new Set();
      const s = FTStore.state;
      if (keys.has("panels")) { if (s.panels && s.panels.finalize) finOnOpen(); else finOnClose(); }
      if (keys.has("project")) {
        const dir = (s.project && s.project.caseDir) || "";
        if (FIN.caseDir !== null && dir !== FIN.caseDir) finResetSheet();
        FIN.caseDir = dir;
      }
      if (keys.has("viewer") || keys.has("phase") || keys.has("jobs")) finScheduleRefresh();
    });
  }
})();
