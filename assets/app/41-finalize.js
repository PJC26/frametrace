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
  result: null, incomplete: null, reportStale: null, verify: null, audit: null, examinerTouched: false,
  bound: false, timer: 0, caseDir: null, sig: "", attributing: false,
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
// "saved" | "pending" | "saving" | "failed" | "queued" | "readonly" | "unknown" | "local" | null.
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
  if (v && typeof v.save === "function") { Promise.resolve(v.save({ patient: true })).catch(() => {}); return true; }
  return false;
}
// Browser-only mark drafts of a case (any window): the viewer's per-id keys
// ft.draft.<case>.<writer>.<id> (§10.3) plus a not-yet-migrated pre-§10.3
// single-key store. Shared with the quit guard.
function unsavedDraftCount(caseId) {
  let n = 0;
  try {
    const prefix = `ft.draft.${caseId}.`;
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.startsWith(prefix)) n += 1;
    }
  } catch (e) { return null; }
  const legacy = readLs(`ft.viewer.${caseId}.annotations.v1`);
  if (legacy && typeof legacy === "object") n += Object.keys(legacy).length;
  return n;
}
// R9 U2: the viewer's own lock cause (summary().lockCause); without it,
// records-meta saying case.json was not read.
function finLockCause(summary, meta) {
  const own = summary && typeof summary.lockCause === "string" ? summary.lockCause : "";
  if (own) return own;
  if (meta && meta.ok !== false && (meta.manifest == null || meta.manifestUnreadable)) return "case";
  return "";
}
function finViewerSummary() {
  const v = finViewer();
  try { return v && typeof v.summary === "function" ? v.summary() : null; } catch (e) { return null; }
}

// Pure: finalize checklist rows for unsaved marks. `retry: true` rows get
// the sheet's "저장 다시 시도" button.
//   markConflicts  /api/status.mark_conflicts.count — saves the server's
//                  queue could not apply (a newer revision); the examiner
//                  resolves them in the viewer's conflict list.
//   lockCause      R9 U2: why a "readonly" viewer is locked — "case"
//                  (case.json was not read) or "annotations" (the case's
//                  marks were not read); the text names that cause.
function unsavedMarksChecks({ pendingMarks, saveState, drafts, markConflicts = 0, lockCause = "" }) {
  const out = [];
  const add = (level, text, sub, retry) => out.push({ level, text, sub: sub || "", retry: !!retry });
  if (pendingMarks === null) add("block", "서버의 마크 저장 대기열을 확인할 수 없습니다", "연결이 복구된 뒤 '다시 확인'을 누르십시오.");
  else if (pendingMarks > 0) add("block", `케이스 DB에 아직 반영되지 않은 저장 대기 마크 ${pendingMarks}건`,
    "보강 작업이 끝나면 자동 반영됩니다 — 잠시 뒤 '다시 확인'을 누르십시오.");
  if (Number(markConflicts) > 0) add("block", `해결하지 않은 저장 충돌 ${Number(markConflicts)}건`,
    "뷰어 위쪽 충돌 배너의 '충돌 목록'에서 건별로 서버 값 또는 내 값을 고르십시오.");
  if (saveState === "failed") add("block", "뷰어의 마크·메모 저장이 실패했습니다",
    "최신 판독 내용 없이 보고서를 봉인할 수 없습니다 — '저장 다시 시도'를 누르십시오.", true);
  else if (saveState === "pending" || saveState === "saving") add("block", "뷰어에 아직 저장되지 않은 마크·메모 변경이 있습니다",
    "'저장 다시 시도'로 지금 저장한 뒤 '다시 확인'을 누르십시오.", true);
  else if (saveState === "queued") add("block", "마크가 서버 대기열에 있습니다 (보강 작업 중)", "보강 작업이 끝난 뒤 다시 확인하십시오.");
  else if (saveState === "readonly" && lockCause === "case") add("block", "케이스 정보(case.json)를 읽지 못했습니다 — 편집 저장이 막혀 있습니다",
    "case.json을 고친 뒤 뷰어를 새로 고치고 다시 확인하십시오.");
  else if (saveState === "readonly") add("block", "판독 기록을 읽지 못했습니다 — 편집 저장이 막혀 있습니다",
    "케이스의 마크·메모를 읽을 수 있게 된 뒤 뷰어를 새로 고치고 다시 확인하십시오.");
  else if (saveState === "unknown") add("block", "저장 충돌 확인에 실패했습니다 — 모두 저장되었는지 알 수 없습니다",
    "연결이 복구되면 자동으로 다시 확인합니다 — 잠시 뒤 '다시 확인'을 누르십시오.", true);
  if (drafts > 0 && !out.some(c => c.retry)) add("block", `브라우저에만 남아 있는 마크·메모 변경 ${drafts}건`,
    "'저장 다시 시도'로 저장을 완료하십시오.", true);
  if (drafts === null && saveState === null) add("warn", "뷰어의 미저장 변경을 확인할 수 없습니다", "뷰어가 아직 열리지 않았거나 케이스 정보를 읽지 못했습니다.");
  if (!out.length) add("ok", "마크·메모가 모두 케이스에 반영되었습니다");
  return out;
}

// SHOULD-FIX 4: "all match" only when something was actually compared and
// nothing was left uncompared — 0 matched / N not comparable is a warning,
// never a green pass.
// R11 U4 (track I2, P9): the run's own verdict — `is_result: false` = no
// current result (counts are 0), `verified: false` = 확인 불가 (reason in
// `not_verified_reason`, `unauthenticated` lines). Green only when
// verified && is_result && nothing mismatched / uncompared && matched > 0.
// Fields absent (older server) = the counts alone, as before.
function verifySourcesVerdict(res) {
  // R14 (track I3): `head_status` of the re-verification lane anchor —
  // every verdict names it in the tooltip.
  const head = res.head_status == null ? "" : String(res.head_status);
  const v = (() => {
    const matched = Number(res.matched) || 0;
    const mismatched = Number(res.mismatched) || 0;
    const notComparable = Number(res.not_comparable) || 0;
    const unauth = Number(res.unauthenticated) > 0 ? Number(res.unauthenticated) : 0;
    const reason = res.not_verified_reason == null ? "" : String(res.not_verified_reason).trim();
    const counts = `일치 ${matched} · 불일치 ${mismatched} · 비교 불가 ${notComparable}` + (unauth ? ` · 인증 안 됨 ${unauth}` : "");
    if (mismatched > 0) {
      return { level: "err", toast: `원본 재검증: 불일치 ${mismatched}건`,
        hint: `원본 재검증 완료 — ${counts} (불일치 항목은 보관 이력서 5절에서 기록값/관측값을 비교하십시오)` };
    }
    // R13 U2 (track I1, P16): a newer usable run without a completion marker
    // withholds the result — its mismatches are shown, never hidden.
    const newerMis = Number(res.newer_mismatched) > 0 ? Number(res.newer_mismatched) : 0;
    const withheld = res.withheld_reason == null ? "" : String(res.withheld_reason).trim();
    // Several reasons may be joined with "; " (track I 32e88a2); the reason
    // text repeats in not_verified_reason — each part once.
    const parts = [];
    for (const p of [withheld, reason].join("; ").split(/;\s*/)) {
      const x = p.trim();
      if (x && !parts.some(q => q.includes(x) || x.includes(q))) parts.push(x);
    }
    const why = parts.join(" · ");
    const NEWER = "표식 없는 더 새 실행 있음";
    const mis = Array.isArray(res.newer_mismatches) ? res.newer_mismatches.filter(m => m && typeof m === "object") : [];
    if (newerMis > 0 || mis.length) {
      const n = Math.max(newerMis, mis.length);
      const other = parts.filter(x => !x.includes(NEWER)).join(" · ");
      const runs = Number(res.newer_runs) > 1 ? ` · 실행 ${Number(res.newer_runs)}개` : "";
      // Each mismatch: id + source path; hashes and run in its tooltip.
      const items = mis.slice(0, 10).map(m => ({ id: String(m.id == null ? "" : m.id), path: String(m.source_path == null ? "" : m.source_path),
        tip: [`기록 SHA-256 ${m.recorded_sha256 || "-"}`, `관측 SHA-256 ${m.observed_sha256 || "-"}`, `실행 ${m.run_id || "-"}` + (Number(m.run_unix) > 0 ? ` (${new Date(Number(m.run_unix) * 1000).toLocaleString()})` : "")].join("\n") }));
      return { level: "err", kind: "newermismatch", label: `원본 재검증: 현재 결과 없음 — 불일치 ${n}건`,
        toast: `원본 재검증: ${NEWER} — 불일치 ${n}건`,
        hint: `${NEWER} — 불일치 ${n}건${runs}` + (other ? ` (${other})` : "") + " — 다시 실행하십시오 (보관 이력서 5절).",
        items, more: Math.max(0, n - items.length),
        tip: res.expected_run_id ? `이번 실행 ${res.expected_run_id} · 결과 실행 ${res.run_id || "없음"}` : "" };
    }
    // R14 (track I3): the lane is shorter than its anchored head — a cut
    // tail, never a plain "no result".
    // R15 U4 (track I5): a head this PC cannot use may be another PC's —
    // warn, never err (a cut tail named on its own stays err).
    const OTHER_HEAD = "다른 PC 머리일 수 있음";
    const otherHead = parts.some(x => x.includes(OTHER_HEAD)) || (head === "unknown" && res.head_reason === "other-pc");
    if (parts.some(x => x.includes("꼬리 잘림") && !x.includes(OTHER_HEAD)) || head === "truncated" || (head === "rewritten" && !otherHead)) {
      return { level: "err", kind: "truncated", label: "원본 재검증: 기록 꼬리 잘림 — 현재 결과 없음", toast: "원본 재검증: 기록 꼬리 잘림",
        hint: `원본 재검증: 기록 꼬리 잘림 — ${why} (보관 이력서 5절 참조).`,
        tip: res.expected_run_id ? `이번 실행 ${res.expected_run_id} · 결과 실행 ${res.run_id || "없음"}` : "" };
    }
    // R14 (track I3): the re-verification head could not be checked — no result (warn).
    if (otherHead) {
      const label = `원본 재검증: ${OTHER_HEAD} — 현재 결과 없음`;
      return { level: "warn", kind: "headotherpc", label, toast: label, hint: label + (why ? ` — ${why}` : "") + " (보관 이력서 5절 참조).",
        tip: res.expected_run_id ? `이번 실행 ${res.expected_run_id} · 결과 실행 ${res.run_id || "없음"}` : "" };
    }
    if (head === "unknown" || parts.some(x => x.includes("머리 확인 불가"))) {
      return { level: "warn", kind: "headunknown", label: "원본 재검증: 머리 확인 불가 — 현재 결과 없음", toast: "원본 재검증: 머리 확인 불가 — 현재 결과 없음",
        hint: "원본 재검증: 머리 확인 불가 — 현재 결과 없음" + (why ? ` — ${why}` : "") + " (보관 이력서 5절 참조).",
        tip: res.expected_run_id ? `이번 실행 ${res.expected_run_id} · 결과 실행 ${res.run_id || "없음"}` : "" };
    }
    // R15 (track I3): two completed runs written into each other — no result.
    if (parts.some(x => x.includes("완료 실행 구간 겹침"))) {
      const label = "원본 재검증: 완료 실행 겹침 — 현재 결과 없음";
      return { level: "warn", kind: "overlap", label, toast: label, hint: `${label} — ${why} (보관 이력서 5절 참조).`,
        tip: res.expected_run_id ? `이번 실행 ${res.expected_run_id} · 결과 실행 ${res.run_id || "없음"}` : "" };
    }
    if (res.is_result === false) {
      return { level: "warn", kind: "noresult", toast: "원본 재검증: 현재 결과 없음(결과 아님)",
        hint: "원본 재검증: 현재 결과 없음(결과 아님)" + (why ? ` — ${why}` : "") + (unauth ? ` · 인증 안 된 줄 ${unauth}개` : "") + " (보관 이력서 5절 참조).",
        // R12 (track I4): the run this request started vs the lane's result run.
        tip: res.expected_run_id ? `이번 실행 ${res.expected_run_id} · 결과 실행 ${res.run_id || "없음"}` : "" };
    }
    if (res.verified === false || unauth > 0) {
      return { level: "warn", kind: "unverified", toast: "원본 재검증: 확인 불가" + (unauth ? ` — 인증 안 된 줄 ${unauth}개` : ""),
        hint: `원본 재검증: 확인 불가 — ${counts}` + (reason ? `. ${reason}` : "") + ` — "모두 일치"로 쓸 수 없습니다 (보관 이력서 5절 참조).` };
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
    // R12 U1: no audit key on this PC — the lane is self-consistent only, so
    // the match is not authenticated: info, never green.
    if (res.lane_status === "structural") {
      return { level: "info", kind: "structural", toast: `원본 재검증: ${matched}건 일치 (자기 일관성만 — 인증 아님)`,
        hint: `원본 재검증 완료 — ${counts}. 자기 일관성만 — 인증 아님 (이 PC에 감사 키 없음).` };
    }
    return { level: "ok", toast: `원본 재검증: ${matched}건 모두 일치`, hint: `원본 재검증 완료 — ${counts}` };
  })();
  // R15 (track I5): why the head is unknown (`head_reason`), short.
  const hr = res.head_reason == null ? "" : String(res.head_reason);
  const hrWord = { "other-pc": "다른 PC 머리일 수 있음", "unusable-line": "쓸 수 없는 줄", "chain-failed": "체인 실패", "repaired": "레인 수리", "malformed": "형식 오류", "own-head-missing": "결과 실행 머리 없음", "unreadable": "읽지 못함", "mixed-version": "이전 버전이 스캔함 — 이 PC에서 다시 스캔하면 해소", "scan-mismatch": "다른 스캔의 머리" }[hr] || hr;
  if (head) v.tip = [v.tip || "", `재검증 머리 ${head}` + (hrWord ? ` (${hrWord})` : "")].filter(Boolean).join(" · ");
  // R15 (track I4): "시계 이상" — its own warning, never withholds the result.
  const clock = typeof res.clock_note === "string" ? res.clock_note.trim() : "";
  if (clock) v.clock = clock;
  return v;
}

// Package dir is a server-side path, not a browsable URL — this control
// POSTs to /api/open-folder so the OS shell opens it only on request.
function pkgExplorerLink(dir) {
  if (!dir) return "";
  return `<button type="button" class="quiet" data-pkgdir="${esc(dir)}">탐색기에서 폴더 열기</button>`;
}

// R10 U4 (track S2): a finalize answer whose package was published but not
// finalized -> { package_dir } ("" when only the server's words say so),
// else null. Fields a server may send are preferred over its error text.
function finIncompleteOf(res) {
  if (!res || typeof res !== "object") return null;
  // R11 (track S 2a51c94): the server's own verdict when it sends one — a
  // failed promotion creates no package, so there is no path to name.
  // R12 (track S1 608884d): packages this finalize published or kept, now
  // `…_INCOMPLETE` or marked beside — each one listed.
  const dirs = Array.isArray(res.incomplete_package_dirs) ? res.incomplete_package_dirs.filter(d => typeof d === "string" && d) : [];
  if (dirs.length) return { package_dir: dirs[0], dirs };
  if (typeof res.finalize_incomplete === "boolean") {
    if (!res.finalize_incomplete) return null;
    const dir = typeof res.incomplete_package_dir === "string" ? res.incomplete_package_dir : typeof res.package_dir === "string" ? res.package_dir : "";
    return { package_dir: dir };
  }
  for (const k of ["incomplete_package_dir", "package_dir"]) {
    const v = typeof res[k] === "string" ? res[k] : "";
    if (v && (k === "incomplete_package_dir" || packageIncomplete(v, res.finalize_incomplete))) return { package_dir: v };
  }
  if (res.finalize_incomplete === true) return { package_dir: "" };
  if (!res.ok && /최종화 미완료|_INCOMPLETE\b/i.test(String(res.error || ""))) return { package_dir: "" };
  return null;
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
  if (typeof hExaminerRender === "function") hExaminerRender();
}

// "현재 이름으로 귀속": records the examiner name on the marks the case DB
// holds without one. POST /api/import-marks upserts by record id and keeps
// what is not sent: examiner = COALESCE(new, stored), note likewise, range
// untouched when absent — so only the examiner changes (status and
// marked_unix are re-sent as stored), and review_mark_history records the
// update with this name as actor. Done only once the viewer has nothing
// unsaved, so a pending viewer edit cannot be overtaken by stale values.
// R5 U7: the direct attribution patch carries a random client id of THIS
// tab (made once per page). A fixed id ("shell-attribute") let one shell
// run inherit the server's same-window exception for a revision another
// run — another tab, another day — had written, so a stale base passed as
// "this window's own consecutive edit" instead of coming back as a conflict.
const FIN_CLIENT_ID = (() => {
  try {
    const a = new Uint32Array(3);
    crypto.getRandomValues(a);
    return "shell-" + Array.from(a, n => n.toString(36)).join("");
  } catch (e) { return "shell-" + String(Math.random()).slice(2) + Date.now().toString(36); }
})();
async function finAttribute() {
  if (FIN.attributing || FIN.busy) return;
  const name = (finEl("finalExaminer").value || "").trim();
  if (!name) { finHint("먼저 검토자 이름을 입력하십시오."); try { finEl("finalExaminer").focus(); } catch (e) {} return; }
  FIN.attributing = true;
  finPaint();
  finHint("검토자 이름을 기록하는 중…");
  let msg = "", ok = false;
  try {
    const v = finViewer();
    if (v && typeof v.save === "function" && FIN_UNSAVED.includes(finViewerSaveState())) {
      let saved = false;
      const so = { patient: true };
      try { saved = await v.save(so); } catch (e) { saved = false; }
      if (!saved) { msg = hSaveStillSaving(so, v) ? H_STILL_SAVING : "뷰어의 미저장 변경을 먼저 저장하지 못했습니다 — ‘저장 다시 시도’ 후 다시 누르십시오."; return; }
    }
    propagateExaminerName(name, FIN.caseId);
    // Preferred: the viewer owns the marks it shows — it stamps the name on
    // its unattributed marks/notes and saves them (contract §3). The
    // re-check below counts what the case DB really holds afterwards.
    if (v && typeof v.attributeUnassignedMarks === "function") {
      let n;
      try { n = await v.attributeUnassignedMarks(name); } catch (e) { n = e; }
      if (typeof n !== "number" || !Number.isFinite(n)) {
        msg = "검토자 이름을 기록하지 못했습니다: " + ((n && n.message) || "뷰어 저장 실패") + " — ‘다시 확인’ 후 다시 누르십시오.";
        return;
      }
      if (n > 0) { ok = true; msg = `마크 ${n}건에 검토자 ‘${name}’을(를) 기록했습니다.`; return; }
      // 0 from the viewer while the DB still lists some (marks the viewer
      // does not hold): fall through to the direct, DB-driven patch.
    }
    let meta = null;
    try { meta = await jget("/api/records-meta"); } catch (e) { meta = null; }
    const marks = meta && meta.annotations && Array.isArray(meta.annotations.marks) ? meta.annotations.marks : null;
    if (!marks) { msg = "케이스의 마크 정보를 읽지 못했습니다 — 연결을 확인한 뒤 다시 누르십시오."; return; }
    const todo = marks.filter(m => m && !String(m.examiner || "").trim() && (m.record_id || m.id) && m.status);
    if (!todo.length) { ok = true; msg = "검토자 이름이 없는 마크가 없습니다."; return; }
    // A9: save protocol v2 with the revision each row was read at — a row
    // another window changed since comes back as a conflict, never silently
    // overwritten. Note, range and tags are not sent (kept as stored).
    const revisions = (meta.annotations && meta.annotations.revisions) || {};
    const ids = todo.map(m => String(m.record_id || m.id));
    const payload = {
      schema_version: 2, protocol: "patch-v2",
      case_id: (meta.manifest && meta.manifest.case_id) || null,
      examiner: name,
      client_id: FIN_CLIENT_ID,
      exported_unix: Math.floor(Date.now() / 1000),
      base_revisions: Object.fromEntries(ids.map(id => [id, Number(revisions[id]) || 0])),
      marks: todo.map(m => {
        const e = { id: String(m.record_id || m.id), status: m.status, examiner: name };
        if (Number.isFinite(Number(m.marked_unix))) e.marked_unix = Number(m.marked_unix);
        return e;
      }),
      tags: [], deleted_ids: [],
    };
    const res = await jpost("/api/import-marks", { marks_json: JSON.stringify(payload) });
    if (!res || !res.ok) { msg = "검토자 이름을 기록하지 못했습니다: " + ((res && res.error) || "알 수 없는 오류"); return; }
    const conflicts = Array.isArray(res.conflicts) ? res.conflicts.length : 0;
    if (conflicts) {
      msg = `마크 ${conflicts}건은 그 사이 다른 창이 바꿔 검토자를 기록하지 않았습니다 — ‘다시 확인’ 후 다시 누르십시오.`;
      return;
    }
    ok = true;
    msg = res.deferred
      ? `마크 ${todo.length}건의 검토자 기록을 저장 대기열에 넣었습니다 — 보강 작업이 끝나면 반영됩니다.`
      : `마크 ${todo.length}건에 검토자 ‘${name}’을(를) 기록했습니다.`;
  } finally {
    FIN.attributing = false;
    finHint(ok ? "" : msg);
    if (msg) toast(msg, ok ? "ok" : "err");
    finRefresh();
  }
}

// R8 (track I P0/P1/P2 ↔ U): one reading of POST /api/verify-audit for the
// env badge and the finalize sheet. Green ("ok") ONLY when every lane is
// authenticated here (integrity-keyed); a keyless PC (structural-only) is
// "자기 일관성만 확인(인증 아님)", lines this PC cannot authenticate are
// "이 PC에서 인증 불가" naming the PCs' key ids to check on — both amber
// ("warn"), never green; a failed chain is "bad".
//   -> { level: "ok"|"warn"|"bad", text, title }
function auditVerdict(res) {
  // R10 U2: an unreadable key configuration comes first, in its own words —
  // nothing could be authenticated, which is not a tampering verdict (P8).
  const keyErr = auditKeyConfigError(res);
  if (keyErr) return { level: "warn", text: "감사 키 설정을 읽지 못함 — 인증 불가(변조 판정 아님)", title: keyErr };
  if (!res || !res.ok) return { level: "bad", text: "감사 로그 검증 실패", title: (res && (res.error || "")) || "" };
  const logs = Array.isArray(res.logs) ? res.logs : [];
  const count = `(${res.verified}개 로그, ${res.entries}항목)`;
  const others = [...new Set(logs.flatMap(l => Array.isArray(l.other_pc_key_ids) ? l.other_pc_key_ids : []).map(String))];
  const title = logs.map(l => `${l.name}: ${l.integrity}, ${l.entries}항목${l.trust ? " — " + l.trust : ""}`).join("\n");
  // R19 (track I3): only an id matching ^[0-9a-z-]{1,64}$ (not "unknown")
  // is quoted; any other reads "알 수 없는 키 id".
  const quoted = others.filter(id => id !== "unknown" && /^[0-9a-z-]{1,64}$/.test(id));
  const unknownKey = quoted.length < others.length ? "알 수 없는 키 id" : "";
  const where = quoted.length
    ? ` — 키 ${quoted.slice(0, 3).join(", ")}${quoted.length > 3 ? " 외" : ""} 보유 PC에서 확인${unknownKey ? " · " + unknownKey : ""}`
    : unknownKey ? ` — ${unknownKey}` : "";
  if (res.integrity === "integrity-keyed") return { level: "ok", text: `감사 로그 인증됨 ${count}`, title };
  if (res.integrity === "integrity-not-verified") {
    // R9 U4: lines without a valid HMAC here come first ("인증 안 된 줄 N개");
    // "check on the PC holding key <id>" only when other PCs' keys are the
    // ONLY reason (R9 U3: never a failure, not a tampering verdict).
    const c = auditNotHereCounts(logs);
    if (c.unauth > 0) return { level: "warn", text: `감사 로그 인증 안 된 줄 ${c.unauth}개 ${count}`, title };
    if (c.otherPc > 0 && !c.otherCause) return { level: "warn", text: `감사 로그 이 PC에서 인증 불가(변조 판정 아님)${where} ${count}`, title };
    return { level: "warn", text: `감사 로그 이 PC에서 인증되지 않음 ${count}`, title };
  }
  if (res.integrity === "integrity-structural-only") return { level: "warn", text: `감사 로그 자기 일관성만 확인(인증 아님) ${count}`, title };
  return { level: "warn", text: `감사 로그 상태 확인 불가${where} ${count}`, title: title || String(res.integrity || "") };
}
// R10 U2: the tooltip of a key-configuration failure ("" when none):
// `key_config_error: true` on a lane (or on the answer), with the server's
// reason (`trust`, else `error`).
function auditKeyConfigError(res) {
  if (!res || typeof res !== "object") return "";
  const logs = Array.isArray(res.logs) ? res.logs : [];
  const bad = logs.filter(l => l && l.key_config_error === true);
  if (!bad.length && res.key_config_error !== true) return "";
  const lines = bad.map(l => `${l.name || l.path || "-"}: ${l.trust || "키 설정을 읽지 못함"}`);
  if (!lines.length) lines.push(String(res.trust || res.error || "키 설정을 읽지 못함"));
  return lines.join("\n");
}
// R9 U4: per not-verified lane, how many lines are simply not authenticated
// here (no HMAC / retired key only) and how many carry another PC's key.
// Numeric fields when the server sends them (`unauthenticated_lines`,
// `other_pc_lines`); otherwise read from the lane's reason text (the server's
// own words, src/audit.rs lane_trust). A lane not-verified for any other
// reason (rewrite not excluded, nothing counted) sets otherCause.
function auditNotHereCounts(logs) {
  const out = { unauth: 0, otherPc: 0, otherCause: false };
  for (const l of Array.isArray(logs) ? logs : []) {
    if (!l || (l.integrity !== "not-verified" && l.integrity !== "integrity-not-verified")) continue;
    const why = String(l.trust || "");
    const num = (k) => (l[k] != null && Number.isFinite(Number(l[k])) ? Number(l[k]) : null);
    const grab = (re) => { const m = re.exec(why); return m ? Number(m[1]) : 0; };
    let unauth = num("unauthenticated_lines");
    if (unauth == null) unauth = grab(/HMAC 없는 줄 (\d+)개/) + grab(/현재 키 항목이 없는 줄 (\d+)개/);
    let otherPc = num("other_pc_lines");
    if (otherPc == null) otherPc = grab(/다른 PC 키로 서명된 줄 (\d+)개/);
    if (l.rewrite_not_excluded || (!unauth && !otherPc)) out.otherCause = true;
    out.unauth += unauth;
    out.otherPc += otherPc;
  }
  return out;
}
async function finRunAudit() {
  try {
    // R8: green only when authenticated here (auditVerdict above).
    const v = auditVerdict(await jpost("/api/verify-audit", {}));
    FIN.audit = { level: v.level === "ok" ? "ok" : "warn", text: v.level === "bad" ? "감사 로그 검증에 실패했습니다" : v.text, title: v.title };
  } catch (e) { FIN.audit = { level: "warn", text: "감사 로그 검증 불가", title: String((e && e.message) || e) }; }
}

async function gatherFinalizeChecks() {
  const checks = [];
  const add = (level, text, sub, tip, extra) => checks.push(Object.assign({ level, text, sub: sub || "", tip: tip || "" }, extra || {}));
  let st = null;
  try { st = await jget("/api/status"); } catch (e) { st = null; }
  const phase = st ? String(st.phase || "") : "";
  // R13 U1 (track S, P17): startup recovery holds the workstation; another
  // PC's unfinished finalize blocks this one (never recovered from here).
  const journal = finJournalState(st);
  if (!st) add("block", "서버 상태를 확인할 수 없습니다", "연결이 복구된 뒤 '다시 확인'을 누르십시오.");
  else if (journal && journal.kind === "recovering" && finJournalBlocks(journal)) add("block", "이전 최종화 복구 중", "끝나면 '다시 확인'을 누르십시오.");
  else if (phase === "review-partial") {
    const e = st.enrichment;
    add("block", "분석 보강 중 — 해시·검증이 끝나야 보고서를 낼 수 있습니다",
      st.busy ? (e ? `${e.label} ${e.stage}/${e.stages} 진행 중` : "끝난 뒤 '다시 확인'을 누르십시오.")
        : "보강이 끝나지 않았습니다 — 같은 소스로 다시 분석하면 남은 파일만 처리합니다.");
  } else if (phase === "running") add("block", "분석이 진행 중입니다", "끝난 뒤 '다시 확인'을 누르십시오.");
  else if (st.busy) add("block", "다른 작업이 진행 중입니다", `현재 단계: ${phase || "-"} — 끝난 뒤 다시 확인하십시오.`);
  else if (!["review-ready", "done", "finalized"].includes(phase)) add("block", "완료된 분석 결과가 없습니다", "먼저 분석을 끝내십시오.");
  // R13 (track S c76a4df): `blocks_finalize` decides, not the state.
  if (journal && journal.kind !== "recovering" && finJournalBlocks(journal)) {
    const jj = journal.journal || {};
    const cmds = finJournalDismissLines(st, journal.kind).map(l => `${l.label}: ${l.cmd}`).join(" · ");
    const sub = journal.kind === "otherhost" ? [finJournalOtherHost(jj), "그 PC에서 끝난 뒤 다시 확인하십시오.", cmds].filter(Boolean).join(" — ")
      : journal.kind === "otheraccount" || journal.kind === "unverifiedwriter" ? [finJournalOtherHost(jj), journal.detail ? finShortDetail(journal.detail) : ""].filter(Boolean).join(" — ")
      : journal.kind === "unreadable" || journal.kind === "unpaired" ? cmds || (journal.detail ? finShortDetail(journal.detail) : "")
      : journal.detail ? finShortDetail(journal.detail) : "결과 아래 안내를 확인하십시오.";
    const force = finJournalCmdText(finJournalWriterGoneLines(st).concat(finJournalForceLines(st, journal.kind)));
    const verdict = finJournalVerdict(jj);
    add("block", "최종화 차단: " + finJournalHead(journal), [sub, verdict ? finShortDetail(verdict) : "", force, finJournalNoCommand(jj)].filter(Boolean).join(" — "));
  } else if (journal && finJournalMaybeBlocks(journal)) {
    // R18 U4 (P31): the status could not tell — possibly blocking.
    const jj = journal.journal || {};
    const verdict = finJournalVerdict(jj);
    add("warn", finJournalHead(journal), [FIN_MAYBE_BLOCKS, verdict ? finShortDetail(verdict) : journal.detail ? finShortDetail(journal.detail) : ""].filter(Boolean).join(" — "), verdict || journal.detail || "");
  }
  let meta = null;
  try { meta = await jget("/api/records-meta"); } catch (e) { meta = null; }
  // R8 U1: the viewer's own storage key of the case (case.json's case_id;
  // an unreadable case.json: a hash of the case folder — never "case").
  const caseId = meta && meta.ok !== false && typeof FTViewer === "object" && FTViewer && FTViewer.caseStorageKey
    ? FTViewer.caseStorageKey(meta.manifest, meta.caseDir)
    : (meta && meta.manifest && meta.manifest.case_id ? String(meta.manifest.case_id) : null);
  const summary = finViewerSummary();

  // Unsaved / queued marks block the finalize: sealing a package without
  // the examiner's latest marks is a silent loss.
  const drafts = caseId ? unsavedDraftCount(caseId) : null;
  unsavedMarksChecks({
    pendingMarks: st ? Number(st.pending_marks) || 0 : null,
    saveState: finViewerSaveState(),
    drafts,
    markConflicts: st && st.mark_conflicts ? Number(st.mark_conflicts.count) || 0 : 0,
    lockCause: finLockCause(summary, meta),
  }).forEach(c => checks.push(c));

  const marks = meta && meta.annotations && Array.isArray(meta.annotations.marks) ? meta.annotations.marks : null;
  if (marks) {
    const real = marks.filter(m => m && m.status && m.status !== "noted");
    const waiting = real.filter(m => m.status === "needs_verification").length;
    if (waiting) add("warn", `검증 대기 ${waiting}건`, "보고서에 '검증 대기'로 표시되며 확정 소견이 아닙니다.");
    const impNoNote = real.filter(m => m.status === "important" && !String(m.note || "").trim()).length;
    if (impNoNote) add("warn", `메모 없는 '중요' 마크 ${impNoNote}건`, "중요 판단의 근거를 메모로 남기는 것을 권장합니다.");
    // Counted from the case DB (records-meta), never guessed. A report must
    // not go out with marks nobody is answerable for: block, with a
    // one-click fix that records the name typed above on exactly these.
    const noExaminer = marks.filter(m => m && !String(m.examiner || "").trim());
    if (noExaminer.length) {
      checks.push({ level: "block", text: `검토자 이름 없이 기록된 마크 ${noExaminer.length}건`,
        sub: "‘현재 이름으로 귀속’을 누르면 위의 검토자 이름을 이 마크들에 기록합니다 (변경 이력에 남습니다).",
        attribute: noExaminer.length });
    }
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
    // R12 U1: lane_status "structural" → info "자기 일관성만 — 인증 아님".
    if (FIN.verify.kind === "structural") add("info", "원본 재검증: 자기 일관성만 — 인증 아님", FIN.verify.hint, FIN.verify.tip);
    // R14 U3: a newer run's MISMATCH withholds the result — it blocks.
    else if (FIN.verify.kind === "newermismatch") add("block", FIN.verify.label, FIN.verify.hint, FIN.verify.tip, { items: FIN.verify.items || [], more: FIN.verify.more || 0 });
    // R13 U2: the verdict's own label (newer run's mismatches) comes first.
    else add(lv === "ok" ? "ok" : "warn", FIN.verify.label ? FIN.verify.label : lv === "ok" ? "원본 재검증: 모두 일치" : lv === "err" ? "원본 재검증: 불일치 있음" : FIN.verify.kind === "noresult" ? "원본 재검증: 현재 결과 없음" : FIN.verify.kind === "unverified" ? "원본 재검증: 확인 불가" : "원본 재검증: 일부만 확인됨", FIN.verify.hint, FIN.verify.tip,
      { items: FIN.verify.items || [], more: FIN.verify.more || 0 });
    // R15 (track I4): `clock_note` — its own warning row, the result stands.
    if (FIN.verify.clock) add("warn", "원본 재검증: 시계 이상", finShortDetail(FIN.verify.clock), FIN.verify.clock);
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
  const fix = c.attribute
    ? `<button type="button" id="btnFinalAttribute"${FIN.attributing ? ' disabled aria-busy="true"' : ""}>현재 이름으로 귀속</button>`
    : "";
  return `<li class="h-check h-check-${esc(c.level)}" data-level="${esc(c.level)}"${c.tip ? ` title="${esc(c.tip)}"` : ""}>${finIcon(lv.icon)}` +
    `<span class="h-check-text"><span class="sr-only">${lv.word}: </span>${esc(c.text)}` +
    (c.sub ? `<small>${esc(c.sub)}</small>` : "") + finCheckItemsHtml(c) + fix + `</span></li>`;
}
// R13 (track I1): the rows behind a count (newer run's mismatches): id +
// source path, the hashes in each row's tooltip.
function finCheckItemsHtml(c) {
  const items = Array.isArray(c.items) ? c.items : [];
  if (!items.length) return "";
  return `<ul class="h-check-items">` + items.map(m => `<li${m.tip ? ` title="${esc(m.tip)}"` : ""}><code class="mono">${esc(m.id || "-")}</code> <span>${esc(m.path)}</span></li>`).join("") +
    (c.more > 0 ? `<li class="muted">외 ${Number(c.more)}건 — 보관 이력서 5절</li>` : "") + `</ul>`;
}
// Writes markup only when it changed: a background re-check that finds the
// same result must not re-announce the verdict (role=status) or rebuild a
// list under the examiner's focus.
// (Compared with what was last written, not with innerHTML: the browser
// re-serializes markup, so a read-back never matches the source string.)
const finWritten = new Map();
function finSetHtml(el, html) {
  if (!el) return false;
  const key = el.id || el;
  if (finWritten.get(key) === html && el.innerHTML !== "") return false;
  finWritten.set(key, html);
  el.innerHTML = html;
  return true;
}
// R12 (track S): a finalize the server refused before starting.
function finBlockedError(err) { return /^\s*최종화 차단:/.test(String(err || "")); }
// R12 U2: a server detail line, cut short for the row.
function finShortDetail(text, max) {
  const lim = max || 90;
  const t = String(text == null ? "" : text).replace(/\s+/g, " ").trim();
  return t.length > lim ? t.slice(0, lim - 1).trimEnd() + "…" : t;
}
// R12 U3 (track S2, P13 — 608884d): /api/status `finalize_journal` is null
// while this workstation's own finalize runs, else {path, state, examiner,
// started_unix, signoff_sha256, error}; state "recovered" → 복구됨,
// "interrupted" / "in-progress" → 확인 필요, "unreadable" (reason in `error`;
// blocks the next finalize) → 기록을 읽지 못함. `finalize_recovered` +
// `finalize_recovered_detail` (what the recovery did; "확인 필요: …" when
// something is left) without a journal → 복구됨. Absent fields = no row.
function finJournalState(st) {
  if (!st || typeof st !== "object") return null;
  const j = st.finalize_journal && typeof st.finalize_journal === "object" ? st.finalize_journal : null;
  const jState = j && j.state != null ? String(j.state) : "";
  const detail = st.finalize_recovered_detail == null ? "" : String(st.finalize_recovered_detail);
  // R13 (track S c76a4df): `reason` says why (unpaired; the live process of
  // in-progress) — the row body.
  const reason = j && j.reason != null ? String(j.reason).trim() : "";
  if (j && jState === "unreadable") return { kind: "unreadable", journal: j, detail: j.error == null ? "" : String(j.error) };
  // R13 U1 (track S, P15/P17): no matching lane record — nothing was changed;
  // another PC's finalize — blocked, never recovered here; startup recovery
  // running in the background (busy); the lane says the finalize completed
  // or was resolved — only the journal is left (cleared on the next open).
  let kind = { "unpaired": "unpaired", "other-host": "otherhost", "recovering": "recovering", "completed": "completed", "in-progress": "inprogress", "completed-unrecorded": "unrecorded" }[jState];
  // R14 U2 (track S9): a closed journal says how it ended (`outcome`) —
  // failed / dismissed are warnings, never "완료".
  if (kind === "completed") kind = finJournalClosedKind(j.outcome);
  // R15 U4 (track S6): the complete record could not be written again — it
  // blocks (absent / false = info as before).
  if (kind === "unrecorded" && j.blocks_finalize === true) kind = "unrecordedblocked";
  // R17 (track S9): the manifest was too large to hash on the status path
  // (`manifest_check: "withheld"`) — "확인 보류", never shown as complete.
  if ((kind === "unrecorded" || kind === "completed") && j.manifest_check === "withheld") kind = "unrecordedwithheld";
  // R15 (track S10): maybe another account's FrameTrace — blocks, no dismiss hint.
  if (kind === "inprogress" && j.in_progress_code === "other-account") kind = "otheraccount";
  // R16 U4 (track S2, P25): a live process that may be this case's writer — never rolled back automatically.
  if (kind === "inprogress" && j.in_progress_code === "unverified-writer") kind = "unverifiedwriter";
  if (j && kind) return { kind, journal: j, detail: reason || detail, code: kind === "unpaired" ? finJournalUnpairedCode(j, reason) : "" };
  if (j && jState !== "recovered") return { kind: "check", journal: j, detail: reason || detail };
  if (st.finalize_recovered === true || jState === "recovered") return { kind: "recovered", journal: j, detail };
  return null;
}
// Does the journal block the next finalize? The server's `blocks_finalize`
// when it sends one; else (older server) recovering / other-host only.
function finJournalBlocks(s) {
  if (!s || !s.journal) return false;
  if (typeof s.journal.blocks_finalize === "boolean") return s.journal.blocks_finalize;
  return s.kind === "recovering" || s.kind === "otherhost" || s.kind === "otheraccount" || s.kind === "unverifiedwriter";
}
// The body shows what is left first: from "확인 필요:" on when the detail has it.
function finJournalBody(detail) {
  const d = String(detail || "");
  const at = d.indexOf("확인 필요:");
  return finShortDetail(at > 0 ? d.slice(at) : d);
}
const FIN_JOURNAL_HEAD = {
  unreadable: "이전 최종화 기록을 읽지 못함 — 확인 필요",
  unpaired: "이전 최종화 기록 짝 없음 — 확인 필요",
  otherhost: "다른 PC의 최종화 — 차단 중",
  recovering: "이전 최종화 복구 중",
  completed: "이전 최종화 완료 — 기록만 남음",
  closedrecovered: "이전 최종화 복구됨 — 기록만 남음",
  closedfailed: "이전 최종화 실패 — 기록만 남음",
  closeddismissed: "이전 최종화 치워짐 — 기록만 남음",
  closedrefused: "이전 최종화 거부됨 — 기록만 남음",
  closedsuperseded: "이전 최종화 대체됨 — 기록만 남음",
  unrecorded: "이전 최종화 완료 — 완료 기록만 빠짐",
  unrecordedblocked: "이전 최종화 완료 — 완료 기록 못 씀",
  unrecordedwithheld: "이전 최종화 확인 보류 — 완료 확정 아님",
  closed: "이전 최종화 끝남 — 확인 필요",
  inprogress: "다른 FrameTrace에서 최종화 중",
  otheraccount: "다른 계정의 최종화일 수 있음 — 차단 중",
  unverifiedwriter: "확인 불가 작성자 — 자동 복구 안 함",
  check: "이전 최종화 중단됨 — 확인 필요",
  recovered: "이전 최종화 중단됨 — 복구됨",
};
const FIN_JOURNAL_INFO = new Set(["recovering", "completed", "closedrecovered", "unrecorded"]);
// R14 (track S): `unpaired_code` → the short cause in the head ("로컬 기록
// 없음" in the reason without a code = no-local-record).
const FIN_UNPAIRED_SHORT = { "no-local-record": "로컬 기록 없음", "local-mismatch": "로컬 기록 불일치", "local-unreadable": "로컬 기록 읽지 못함", "lane": "서명 레인 확인 불가", "journal-mismatch": "저널 불일치", "legacy-journal": "이전 형식 기록", "host-unknown": "이 PC 식별 불가", "package-changed": "패키지 변경됨", "local-state": "로컬 기록 상태 불일치" };
function finJournalUnpairedCode(j, reason) {
  const c = j && j.unpaired_code != null ? String(j.unpaired_code) : "";
  return c || (String(reason || "").includes("로컬 기록 없음") ? "no-local-record" : "");
}
// R18 U3 (track S1, P29): the server sends no command (all null) when the
// case path holds a character a shell could read; `command_unavailable_reason`
// says so — shown instead of any command line (a code → the fixed words).
const FIN_NO_COMMAND = "경로에 특수 문자 — 명령을 직접 입력하십시오";
function finJournalNoCommand(j) {
  const v = j && typeof j === "object" ? j.command_unavailable_reason : null;
  if (v == null || v === false) return "";
  const r = String(v).trim();
  return /[^\x00-\x7f]/.test(r) ? r : FIN_NO_COMMAND;
}
// R18 U4 (track S2, P31): `blocks_finalize: null` = the status could not
// tell — possibly blocking: a warning, never info.
const FIN_MAYBE_BLOCKS = "확인 필요 — 최종화가 거부될 수 있음";
function finJournalMaybeBlocks(s) {
  return !!(s && s.journal && Object.prototype.hasOwnProperty.call(s.journal, "blocks_finalize") && s.journal.blocks_finalize === null && !finJournalBlocks(s));
}
// R18 U4: the remembered recovery verdict the server sends
// (`recovery_verdict`: text, or {text | reason}); "" when none.
// Track S (a9d26a3): `manifest_check: "recovery-verdict"` — the remembered
// verdict is the `reason` (shown as the body): labelled "기억된 판정".
function finJournalVerdict(j) {
  const v = j && typeof j === "object" ? j.recovery_verdict : null;
  if (typeof v === "string" && v.trim()) return v.trim();
  if (v && typeof v === "object") { const x = String(v.text != null ? v.text : v.reason != null ? v.reason : "").trim(); if (x) return x; }
  return j && j.manifest_check === "recovery-verdict" ? FIN_VERDICT_LABEL : "";
}
const FIN_VERDICT_LABEL = "기억된 판정(복구가 전체 해시로 확인)";
// The dismiss command lines (unreadable / unpaired / other-host): the
// server's PowerShell (single-quoted) and cmd.exe forms when both come, one
// line when one does. R18 U5 (P29): never built here — no server string, no
// line (R15's built-in form is gone: it could not quote every path safely).
function finJournalDismissLines(st, kind) {
  if (!["unreadable", "unpaired", "otherhost"].includes(kind)) return [];
  const j = st && st.finalize_journal && typeof st.finalize_journal === "object" ? st.finalize_journal : {};
  if (finJournalNoCommand(j)) return [];
  const ps = typeof j.dismiss_command === "string" ? j.dismiss_command.trim() : "";
  const cmd = typeof j.dismiss_command_cmd === "string" ? j.dismiss_command_cmd.trim() : "";
  if (ps && cmd) return [{ label: "치우기(PowerShell)", cmd: ps }, { label: "치우기(명령 프롬프트)", cmd }];
  return ps || cmd ? [{ label: "치우기", cmd: ps || cmd }] : [];
}
// R16 U4 (track S 2e3a4e4): the `--force` dismiss hint — ONLY the server's
// `force_dismiss_command` (PowerShell) / `force_dismiss_command_cmd`
// (cmd.exe) strings; never built here. R17 U3 (track S8): never for a
// verified live writer (`in_progress_code: "live-writer"` — the server
// sends null; an older server's string is not shown either); the last line
// carries the fixed note FIN_FORCE_NOTE.
const FIN_FORCE_NOTE = "저널만 옮김 — 아무것도 되돌리거나 기록하지 않음(서명·패키지 그대로)";
function finJournalForceLines(st, kind) {
  const j = st && st.finalize_journal && typeof st.finalize_journal === "object" ? st.finalize_journal : {};
  if (j.in_progress_code === "live-writer" || finJournalNoCommand(j)) return [];
  const ps = typeof j.force_dismiss_command === "string" ? j.force_dismiss_command.trim() : "";
  const cmd = typeof j.force_dismiss_command_cmd === "string" ? j.force_dismiss_command_cmd.trim() : "";
  const out = ps && cmd ? [{ label: "확인 뒤 강제 치우기(PowerShell)", cmd: ps }, { label: "확인 뒤 강제 치우기(명령 프롬프트)", cmd }]
    : ps || cmd ? [{ label: "확인 뒤 강제 치우기", cmd: ps || cmd }] : [];
  if (out.length) out[out.length - 1].note = FIN_FORCE_NOTE;
  return out;
}
// R17 U5 (track S2): "작성자 없음 확인 시 복구" — ONLY the server's
// `writer_gone_command` (PowerShell) / `writer_gone_command_cmd` (cmd.exe).
// R18 U5 (track S7): `--i-am-sure` only ever comes inside these strings.
function finJournalWriterGoneLines(st) {
  const j = st && st.finalize_journal && typeof st.finalize_journal === "object" ? st.finalize_journal : {};
  if (finJournalNoCommand(j)) return [];
  const ps = typeof j.writer_gone_command === "string" ? j.writer_gone_command.trim() : "";
  const cmd = typeof j.writer_gone_command_cmd === "string" ? j.writer_gone_command_cmd.trim() : "";
  const out = ps && cmd ? [{ label: "작성자 없음 확인 시 복구(PowerShell)", cmd: ps }, { label: "작성자 없음 확인 시 복구(명령 프롬프트)", cmd }]
    : ps || cmd ? [{ label: "작성자 없음 확인 시 복구", cmd: ps || cmd }] : [];
  // R18 (track S7): `writer_gone_i_am_sure: true` — the server's command already
  // carries --i-am-sure (a live process matches the writer): a warning next to it.
  if (out.length && j.writer_gone_i_am_sure === true) out[out.length - 1].note = FIN_SURE_NOTE;
  return out;
}
const FIN_SURE_NOTE = "이 케이스의 진행 중 최종화가 아닌지 확인";
// One checklist text of command lines; a line's note follows it in brackets.
function finJournalCmdText(lines) {
  return lines.map(l => `${l.label}: ${l.cmd}` + (l.note ? ` (${l.note})` : "")).join(" · ");
}
// Tooltip words for `local_record` / `unpaired_code`.
function finJournalTipExtra(j) {
  const out = [];
  if (j && j.local_record === true) out.push("이 PC의 로컬 기록 있음");
  else if (j && j.local_record === false) out.push("이 PC의 로컬 기록 없음");
  if (j && j.unpaired_code) out.push("짝 없음 코드 " + String(j.unpaired_code));
  if (j && j.local_record_state) out.push("로컬 기록 상태 " + String(j.local_record_state));
  if (j && j.in_progress_code) out.push("진행 중 코드 " + String(j.in_progress_code));
  return out;
}
// R14 U2: `outcome` of a closed journal → row kind (absent = completed).
function finJournalClosedKind(outcome) {
  const o = outcome == null ? "completed" : String(outcome);
  return { completed: "completed", recovered: "closedrecovered", failed: "closedfailed", dismissed: "closeddismissed", refused: "closedrefused", superseded: "closedsuperseded" }[o] || "closed";
}
// The row / checklist head; P18: an unpaired journal without the local
// open record says so.
function finJournalHead(s) {
  if (s && s.kind === "unpaired" && FIN_UNPAIRED_SHORT[s.code]) return `이전 최종화 짝 없음(${FIN_UNPAIRED_SHORT[s.code]}) — 확인 필요`;
  return FIN_JOURNAL_HEAD[s && s.kind] || FIN_JOURNAL_HEAD.check;
}
// other-host: "PC <host> · <time>까지 차단".
function finJournalOtherHost(j) {
  const until = Number(j && j.blocked_until_unix);
  return [j && j.host ? "PC " + String(j.host) : "", until > 0 ? new Date(until * 1000).toLocaleString() + "까지 차단" : ""].filter(Boolean).join(" · ");
}
function finJournalRow(st) {
  const s = finJournalState(st);
  if (!s) return "";
  const j = s.journal || {};
  const tip = [];
  if (j.host) tip.push("호스트 " + String(j.host));
  if (j.examiner) tip.push("검토자 " + String(j.examiner));
  if (j.signoff_sha256) tip.push("검토자 기록 SHA-256 " + String(j.signoff_sha256));
  if (j.package_name) tip.push("패키지 " + String(j.package_name));
  if (j.finalize_id) tip.push("최종화 id " + String(j.finalize_id));
  const started = Number(j.started_unix);
  if (started > 0) tip.push("시작 " + new Date(started * 1000).toLocaleString());
  if (j.state != null && String(j.state)) tip.push("상태 " + String(j.state));
  if (j.path) tip.push(String(j.path));
  tip.push(...finJournalTipExtra(j));
  if (s.detail) tip.push(s.detail);
  const head = s.kind === "unpaired" ? finJournalHead(s) : FIN_JOURNAL_HEAD[s.kind] || FIN_JOURNAL_HEAD.recovered;
  const lines = [];
  if (s.kind === "otherhost" || s.kind === "otheraccount" || s.kind === "unverifiedwriter") { const o = finJournalOtherHost(j); if (o) lines.push(o); }
  if (s.detail) lines.push(finJournalBody(s.detail));
  else if (s.kind === "unpaired") lines.push("아무것도 바뀌지 않았습니다");
  // R18 U4: unknown blocking → "확인 필요 — 최종화가 거부될 수 있음"; the remembered verdict.
  const maybe = finJournalMaybeBlocks(s);
  if (maybe) lines.push(FIN_MAYBE_BLOCKS);
  const verdict = finJournalVerdict(j);
  if (verdict) lines.push(finShortDetail(verdict));
  // R18 U3: no command from the server → why, in place of any command line.
  const nocmd = finJournalNoCommand(j);
  if (nocmd) lines.push(nocmd);
  // R13 U3 (track S6): an unreadable or unpaired journal (R12 journals read
  // unpaired) is set aside with the CLI.
  const cmds = finJournalDismissLines(st, s.kind).concat(finJournalWriterGoneLines(st), finJournalForceLines(st, s.kind));
  return `<li class="h-fin-journal-row" data-journal="${s.kind}"${maybe ? ` data-maybe-blocks="true"` : ""}${tip.length ? ` title="${esc(tip.join("\n"))}"` : ""}>` +
    `<span class="${FIN_JOURNAL_INFO.has(s.kind) && !maybe && !(s.kind !== "recovering" && finJournalBlocks(s)) ? "h-fin-info" : "h-fin-incomplete"}">${esc(head)}</span>` +
    lines.map(b => `<small class="h-fin-detail muted">${esc(b)}</small>`).join("") +
    cmds.map(l => `<small class="h-fin-detail h-fin-cmd">${esc(l.label)}: <code class="mono">${esc(l.cmd)}</code>` +
      (l.note ? ` <span class="muted h-fin-note">${esc(l.note)}</span>` : "") + `</small>`).join("") + `</li>`;
}
function finResultHtml() {
  const r = FIN.result;
  const st = FIN.status;
  const pkgDir = (r && r.package_dir) || (st && ["done", "finalized"].includes(st.phase) && st.package_dir) || "";
  // R10 U4: the last finalize published a package but did not complete
  // (its own row unless it is the package listed below).
  // R11 U4 (S6): the status flag speaks for the status package_dir only.
  const pkgInc = !!pkgDir && packageIncomplete(pkgDir, (r && r.package_dir) ? r.finalize_incomplete : st ? st.package_incomplete : undefined);
  // R11 (track S 2a51c94): after a reload /api/status says what the last
  // finalize left (`finalize_incomplete` + detail, `report_stale`; memory
  // only) — used when this sheet has no answer of its own.
  const own = !!(FIN.incomplete || FIN.reportStale || r);
  const inc = FIN.incomplete || (!own && st && st.finalize_incomplete === true
    ? { package_dir: "", detail: st.finalize_incomplete_detail == null ? "" : String(st.finalize_incomplete_detail) } : null);
  const stale = FIN.reportStale || (!own && st && st.report_stale === true ? { error: "" } : null);
  // R11 U4 (track S5): the report could not be re-rendered after a failed
  // step — reports/case-report.html is not current.
  const staleRow = stale ? `<li class="h-fin-stale-row"><span class="h-fin-incomplete">보고서 최신 아님</span>` +
    ` <span class="muted"${stale.error ? ` title="${esc(stale.error)}"` : ""}>다시 최종화하십시오</span></li>` : "";
  // R12 U2: the server's detail (what was left) shows in the row, shortened;
  // the full text stays in the tooltip.
  const incDetail = inc && inc.detail ? finShortDetail(inc.detail) : "";
  const incRow = finJournalRow(st) + staleRow + (inc && !(pkgInc && (!inc.package_dir || inc.package_dir === pkgDir)) ? `<li class="h-fin-incomplete-row"${inc.detail ? ` title="${esc(inc.detail)}"` : ""}><span class="h-fin-incomplete">${esc(H_PKG_INCOMPLETE)}</span>` +
    (inc.dirs && inc.dirs.length ? inc.dirs.map(d => ` <code class="mono">${esc(d)}</code> ${pkgExplorerLink(d)}`).join("")
      : inc.package_dir ? ` <code class="mono">${esc(inc.package_dir)}</code> ${pkgExplorerLink(inc.package_dir)}` : "") +
    ` <span class="muted">최종 결과로 쓰지 마십시오</span>` +
    (incDetail ? `<small class="h-fin-detail muted">${esc(incDetail)}</small>` : "") + `</li>` : "");
  if (!r && !pkgDir) return incRow ? `<h3 class="h-fin-h">결과</h3><ul class="h-fin-links">${incRow}</ul>` : "";
  const pkgUrl = r && r.package_url ? `<a href="/case/${esc(r.package_url)}" target="_blank" rel="noopener">패키지 폴더 보기${finIcon("i-external")}</a>` : "";
  return `<h3 class="h-fin-h">결과</h3><ul class="h-fin-links">` + incRow +
    `<li><a href="/case/reports/case-report.html" target="_blank" rel="noopener"><b>보고서 열기</b>${finIcon("i-external")}</a> <span class="muted">새 탭에서 열립니다</span></li>` +
    (pkgDir ? `<li><span class="muted">패키지</span>${pkgInc ? ` <span class="h-fin-incomplete">${esc(H_PKG_INCOMPLETE)}</span>` : ""} <code class="mono">${esc(pkgDir)}</code> ${pkgUrl} ${pkgExplorerLink(pkgDir)}${pkgInc ? ` <span class="muted">최종 결과로 쓰지 마십시오</span>` : ""}</li>` : "") +
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
    `<div class="h-sheet-head h-fin-head"><h2 id="hFinalizeTitle">결과물</h2>` +
    `<button type="button" class="quiet icon" id="hFinalizeClose" aria-label="결과물 닫기" title="닫기 (Esc)">${finIcon("i-close")}</button></div>` +
    `<div class="h-sheet-body h-fin-body">` +
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
    `<button type="button" id="btnVerifyAudit" title="체인된 감사 로그의 무결성을 다시 확인합니다">감사 로그 검증</button>` +
    `<button type="button" id="btnAudioMeasure" title="${esc(finAudioTip())}">소리 측정 (미측정 항목)</button></div>` +
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
    `<div class="h-sheet-foot h-fin-foot"><span class="h-fin-hint muted" id="finalHint" role="status"></span>` +
    `<button type="button" class="primary lg" id="btnFinalize">보고서 생성·패키징</button></div>` +
    `</div>`;
  FIN.built = true;
  mount.addEventListener("click", finOnClick);
  mount.addEventListener("change", e => {
    if (e.target && e.target.id === "marksFile") finImportMarks();
    // The hosted selection bar has no 검토자 field: a committed name here is
    // the examiner for new marks too (FTViewer.setExaminer), not only for
    // the report.
    if (e.target && e.target.id === "finalExaminer" && e.target.value.trim()) propagateExaminerName(e.target.value.trim(), FIN.caseId);
  });
  mount.addEventListener("input", e => {
    if (e.target && e.target.id === "finalExaminer") { FIN.examinerTouched = true; finPaint(); }
  });
  finApplyEnv();
  return true;
}
// Sound measurement run later (§2.2): a case job like recover/carve
// (32-jobs.js CASE_JOBS.audio → POST /api/audio-measure); it needs ffmpeg
// to decode and ffprobe facts to pick the audio tracks.
function finAudioTip() { return "오디오 트랙이 있는데 아직 소리를 재지 않은 영상만 끝까지 디코드해 최대·평균 신호 레벨을 잽니다 — 진행은 작업 시트에 표시되고, 끝나면 뷰어에 반영됩니다"; }
function finAudioMeasure() {
  if (typeof runCaseJob !== "function") return;
  runCaseJob("audio");
}
// Disabled (with the reason) while a tool is missing or another job runs;
// "소리 측정 중…" + aria-busy while its own job runs (32-jobs.js calls this
// from setCaseJobButtons on every status).
function finApplyAudioEnv() {
  const btn = finEl("btnAudioMeasure");
  if (!btn) return;
  const tools = (FIN.env && FIN.env.tools) || {};
  const missing = tools.ffmpeg === false ? "ffmpeg" : tools.ffprobe === false ? "ffprobe" : "";
  const busy = typeof caseJobsBusy !== "undefined" && !!caseJobsBusy;
  const own = busy && typeof caseJob !== "undefined" && !!caseJob && !caseJob.outcome && caseJob.kind === "audio";
  btn.disabled = !!missing || busy;
  if (own) btn.setAttribute("aria-busy", "true"); else btn.removeAttribute("aria-busy");
  const label = own ? "소리 측정 중…" : "소리 측정 (미측정 항목)";
  if (btn.textContent !== label) btn.textContent = label;
  btn.title = missing ? `${missing}가 없어 사용할 수 없습니다 — 도구 상태를 확인하십시오`
    : own ? "소리 측정이 진행 중입니다 — 작업 시트(Alt+J)에서 진행 상황을 보고 중단할 수 있습니다"
      : busy ? "다른 작업이 진행 중입니다 — 끝난 뒤 실행할 수 있습니다" : finAudioTip();
}
function finApplyEnv() {
  finApplyAudioEnv();
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
  const cls = "h-fin-verdict " + (done ? "done" : sum.state);
  if (verdict.className !== cls) verdict.className = cls;
  finSetHtml(verdict, finIcon(sum.state === "blocked" ? "i-warning" : sum.state === "loading" ? "i-spinner" : "i-check") +
    `<span>${esc(done ? (FIN.incomplete ? `보고서·패키지를 만들었지만 ${H_PKG_INCOMPLETE}` : FIN.reportStale ? "보고서·패키지를 만들었지만 보고서 최신 아님" : "보고서·패키지를 만들었습니다") : sum.text)}</span>`);
  const list = finEl("finalChecklist");
  const hadFocus = typeof list.contains === "function" && list.contains(document.activeElement) && document.activeElement && document.activeElement.id;
  if (finSetHtml(list, rows.map(finRowHtml).join("")) && hadFocus) {
    const back = finEl(hadFocus);
    try { if (back && !back.disabled) back.focus(); else finEl("finalExaminer").focus(); } catch (e) {}
  }
  const attr = finEl("btnFinalAttribute");
  if (attr && !FIN.attributing) {
    const named = !!name.trim();
    attr.disabled = !named;
    attr.title = named ? `검토자 이름 없는 마크에 ‘${name.trim()}’을(를) 기록합니다` : "먼저 위에 검토자 이름을 입력하십시오";
  }
  finEl("btnFinalRetrySave").hidden = !FIN.checks.some(c => c.retry);
  const btn = finEl("btnFinalize");
  btn.disabled = FIN.busy || sum.disabled;
  btn.title = FIN.busy ? "보고서 생성 중입니다" : sum.reason || "검토자 이름과 점검 결과를 기록해 보고서·패키지를 생성합니다";
  btn.setAttribute("aria-describedby", "finalVerdict");
  if (FIN.busy) btn.setAttribute("aria-busy", "true"); else btn.removeAttribute("aria-busy");
  finEl("btnFinalRecheck").disabled = FIN.busy || FIN.loading;
  finSetHtml(finEl("finalLinks"), finResultHtml());
  const mount = finMount();
  if (FIN.busy) mount.setAttribute("aria-busy", "true"); else mount.removeAttribute("aria-busy");
}
function finHint(text) { const h = finEl("finalHint"); if (h) h.textContent = text || ""; }

// opts.background: a re-check the examiner did not ask for (something that
// matters changed). It shows no "점검 중…" and repaints only what changed.
async function finRefresh(opts) {
  if (!finBuild()) return;
  const background = !!(opts && opts.background);
  const gen = ++FIN.gen;
  FIN.sig = finSignature();
  if (!background) { FIN.loading = true; finPaint(); }
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
// What the checklist depends on. Anything else the store or the viewer
// reports (a status poll with the same content, selection, active card,
// counts) leaves the checklist alone — re-checking costs three requests
// and must not flicker the verdict.
function finSignature() {
  const S = typeof FTStore === "object" && FTStore && FTStore.state ? FTStore.state : null;
  const j = S ? S.jobs || {} : {};
  const v = finViewer();
  let examiner = "";
  try { examiner = v && typeof v.getExaminer === "function" ? String(v.getExaminer() || "") : ""; } catch (e) {}
  return JSON.stringify([
    S ? S.phase : "", S && typeof FTStore.isBusy === "function" ? FTStore.isBusy() : false,
    Number(j.pendingMarks) || 0, !!(j.pipeline && j.pipeline.busy), j.jobKind || null,
    S ? S.project.caseDir : "", S ? !!S.viewer.mounted : true, S ? Number(S.viewer.dirty) || 0 : 0,
    finViewerSaveState(), examiner,
  ]);
}
function finScheduleRefresh() {
  if (!FIN.open || FIN.busy) return;
  if (finSignature() === FIN.sig) return;
  clearTimeout(FIN.timer);
  FIN.timer = setTimeout(() => finRefresh({ background: true }), 300);
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
  FIN.incomplete = null; FIN.reportStale = null;
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
// "local" (edits only in this browser) counts as unsaved once anything is
// pending — the dirty count below, the same rule as hViewer.hasUnsaved().
const FIN_UNSAVED = ["pending", "saving", "failed", "queued", "readonly", "unknown"];
// "" = nothing new stands in the way; otherwise why the seal must wait.
async function finLateBlocker() {
  const viewerSays = () => {
    const ss = finViewerSaveState();
    if (FIN_UNSAVED.includes(ss)) return "뷰어에 아직 케이스에 반영되지 않은 마크·메모 변경이 있습니다.";
    const S = typeof FTStore === "object" && FTStore && FTStore.state ? FTStore.state : null;
    if (S && S.viewer && S.viewer.mounted && Number(S.viewer.dirty) > 0) return "뷰어에 아직 저장되지 않은 변경이 있습니다.";
    return "";
  };
  const first = viewerSays();
  if (first) return first;
  const st = await jgetSoft("/api/status");
  if (!st || st.ok === false || st.malformed) return "서버 상태를 확인할 수 없습니다.";
  if (Number(st.pending_marks) > 0) return `저장 대기 마크 ${Number(st.pending_marks)}건이 아직 케이스에 반영되지 않았습니다.`;
  if (st.mark_conflicts && Number(st.mark_conflicts.count) > 0) return `해결하지 않은 저장 충돌 ${Number(st.mark_conflicts.count)}건이 있습니다.`;
  if (st.busy) return "다른 작업이 진행 중입니다.";
  return viewerSays();
}
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
  // The checklist took a few requests; a mark made meanwhile (or a save
  // that failed) must still stop the seal. Last look, right before POST.
  const late = await finLateBlocker();
  if (late) {
    finSetFinalizing(false);
    finRefresh();
    finHint("보고서를 만들지 않았습니다 — " + late);
    toast("보고서를 만들지 않았습니다 — " + late, "warn");
    return;
  }
  finHint("보고서 생성 중… (원본 해시 재계산 포함 — 소스 크기에 따라 오래 걸릴 수 있습니다)");
  finPaint();
  let res;
  try { res = await jpost("/api/finalize", { examiner: name }); }
  finally { finSetFinalizing(false); }
  FIN.incomplete = finIncompleteOf(res);
  FIN.reportStale = res && res.report_stale === true ? { error: String(res.error || "") } : null;
  if (!res.ok) {
    // R12 (track S): "최종화 차단: …" = refused before anything changed —
    // a warning to resolve, not a failure.
    const err = String(res.error || "알 수 없는 오류");
    if (finBlockedError(err)) { finHint(err); toast(err, "warn"); }
    else finHint("실패: " + err);
    finPaint(); return;
  }
  FIN.result = { examiner: res.examiner || name, manifest_sha256: res.manifest_sha256 || "", package_dir: res.package_dir || "", package_url: res.package_url || "", finalize_incomplete: typeof res.finalize_incomplete === "boolean" ? res.finalize_incomplete : undefined };
  finHint("");
  finPaint();
  if (FIN.incomplete) toast(`보고서·패키지를 만들었지만 ${H_PKG_INCOMPLETE} — 최종 결과로 쓰지 마십시오`, "warn");
  else if (FIN.reportStale) toast("보고서·패키지를 만들었지만 보고서 최신 아님 — 다시 최종화하십시오", "warn");
  else toast("보고서·패키지를 만들었습니다", "ok");
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
    FIN.verify = { level: verdict.level, hint: verdict.hint, kind: verdict.kind || "", tip: verdict.tip || "", label: verdict.label || "",
      items: verdict.items || [], more: verdict.more || 0, clock: verdict.clock || "" };
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
  const so = { patient: true };
  try { ok = await v.save(so); } catch (e) { ok = false; }
  btn.removeAttribute("aria-busy"); btn.disabled = false;
  finHint(ok ? "" : hSaveStillSaving(so, v) ? H_STILL_SAVING : "저장하지 못했습니다 — 뷰어의 저장 상태를 확인하십시오.");
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
    case "btnFinalAttribute": return finAttribute();
    case "btnVerifySources": return finVerifySources();
    case "btnVerifyAudit": return finVerifyAudit();
    case "btnAudioMeasure": return finAudioMeasure();
    case "btnImportMarks": return finEl("marksFile").click();
    // The shell closes the sheet (delegated click, 97-shell.js).
    case "hFinalizeClose": if (typeof hCloseSheet !== "function") finClose(); return;
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
