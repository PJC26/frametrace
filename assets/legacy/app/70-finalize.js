// ---- FOR-02: pre-finalize checklist ----
// Everything here comes from data that already exists: /api/status
// (busy/phase), /api/records-meta (case manifest + marks the server has
// applied), the embedded viewer's __frametraceSummary() and the viewer's
// per-case localStorage drafts (= mark/note changes not yet synced to the
// case DB). Blocking items disable "proceed"; warnings only inform.
function viewerWindow() {
  try { return reviewFrameLoaded ? reviewFrame.contentWindow : null; } catch (e) { return null; }
}
function viewerExaminerInput() {
  try { return reviewFrameLoaded && reviewFrame.contentDocument ? reviewFrame.contentDocument.getElementById("examinerName") : null; } catch (e) { return null; }
}
function readLs(key) {
  try { const raw = localStorage.getItem(key); return raw ? JSON.parse(raw) : null; } catch (e) { return null; }
}
// Save-state badge of the embedded viewer ("saved", "pending", "saving",
// "failed", "queued", "local"), or null when the viewer is not loaded.
function viewerSaveState() {
  try {
    const doc = reviewFrameLoaded && reviewFrame.contentDocument;
    const el = doc ? doc.getElementById("saveState") : null;
    return el && el.dataset ? (el.dataset.state || null) : null;
  } catch (e) { return null; }
}
// Clicking the viewer's save badge saves (or retries) immediately.
function retryViewerSave() {
  try {
    const doc = reviewFrameLoaded && reviewFrame.contentDocument;
    const el = doc ? doc.getElementById("saveState") : null;
    if (el) { el.click(); return true; }
  } catch (e) {}
  return false;
}
// Pure: finalize checklist rows for unsaved marks. `retry: true` rows get
// the dialog's "저장 다시 시도" button.
function unsavedMarksChecks({ pendingMarks, saveState, drafts }) {
  const out = [];
  const add = (level, text, sub, retry) => out.push({ level, text, sub: sub || "", retry: !!retry });
  if (pendingMarks === null) add("block", "서버의 마크 저장 대기열을 확인할 수 없습니다", "연결이 복구된 뒤 '다시 확인'을 누르십시오.");
  else if (pendingMarks > 0) add("block", `케이스 DB에 아직 반영되지 않은 저장 대기 마크 ${pendingMarks}건`,
    "보강 작업이 끝나면 자동 반영됩니다 — 잠시 뒤 '다시 확인'을 누르십시오.");
  if (saveState === "failed") add("block", "뷰어의 마크·메모 저장이 실패했습니다",
    "최신 판독 내용 없이 보고서를 봉인할 수 없습니다 — '저장 다시 시도'를 누르거나 뷰어의 저장 상태 표시를 확인하십시오.", true);
  else if (saveState === "pending" || saveState === "saving") add("block", "뷰어에 아직 저장되지 않은 마크·메모 변경이 있습니다",
    "'저장 다시 시도'로 지금 저장한 뒤 '다시 확인'을 누르십시오.", true);
  else if (saveState === "queued") add("block", "마크가 서버 대기열에 있습니다 (보강 작업 중)", "보강 작업이 끝난 뒤 다시 확인하십시오.");
  if (drafts > 0 && !out.some(c => c.retry)) add("block", `브라우저에만 남아 있는 마크·메모 변경 ${drafts}건`,
    "뷰어를 열어 저장을 완료하십시오 ('저장 다시 시도').", true);
  if (drafts === null && saveState === null) add("warn", "뷰어의 미저장 변경을 확인할 수 없습니다", "뷰어가 아직 열리지 않았거나 케이스 정보를 읽지 못했습니다.");
  if (!out.length) add("ok", "뷰어의 마크·메모가 모두 케이스에 반영되었습니다");
  return out;
}
// R14 U1: /api/status `finalize_journal` (same reading as the app shell,
// assets/app/41-finalize.js finJournalState / finJournalBlocks): a journal
// that blocks the next finalize is a block row, with the CLI command that
// sets an unreadable / unpaired one aside.
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
  closed: "이전 최종화 끝남 — 확인 필요",
  inprogress: "다른 FrameTrace에서 최종화 중",
  otheraccount: "다른 계정의 최종화일 수 있음 — 차단 중",
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
// The dismiss command lines (unreadable / unpaired / other-host): the
// server's PowerShell (single-quoted) and cmd.exe forms when both come, one
// line when one does, the built-in form only when neither does
// (unreadable / unpaired).
function finJournalDismissLines(st, kind) {
  if (!["unreadable", "unpaired", "otherhost"].includes(kind)) return [];
  const j = st && st.finalize_journal && typeof st.finalize_journal === "object" ? st.finalize_journal : {};
  const ps = typeof j.dismiss_command === "string" ? j.dismiss_command.trim() : "";
  const cmd = typeof j.dismiss_command_cmd === "string" ? j.dismiss_command_cmd.trim() : "";
  if (ps && cmd) return [{ label: "치우기(PowerShell)", cmd: ps }, { label: "치우기(명령 프롬프트)", cmd }];
  if (ps || cmd) return [{ label: "치우기", cmd: ps || cmd }];
  // R15 (track S): null = no dismiss hint on purpose; the built-in form only
  // for an older server that does not send the field.
  if (kind === "otherhost" || Object.prototype.hasOwnProperty.call(j, "dismiss_command")) return [];
  return [{ label: "치우기", cmd: finJournalDismissCmd(st) }];
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
function finJournalState(st) {
  if (!st || typeof st !== "object") return null;
  const j = st.finalize_journal && typeof st.finalize_journal === "object" ? st.finalize_journal : null;
  const jState = j && j.state != null ? String(j.state) : "";
  const detail = st.finalize_recovered_detail == null ? "" : String(st.finalize_recovered_detail);
  const reason = j && j.reason != null ? String(j.reason).trim() : "";
  if (j && jState === "unreadable") return { kind: "unreadable", journal: j, detail: j.error == null ? "" : String(j.error) };
  let kind = { "unpaired": "unpaired", "other-host": "otherhost", "recovering": "recovering", "completed": "completed", "in-progress": "inprogress", "completed-unrecorded": "unrecorded" }[jState];
  if (kind === "completed") {
    const o = j.outcome == null ? "completed" : String(j.outcome);
    kind = { completed: "completed", recovered: "closedrecovered", failed: "closedfailed", dismissed: "closeddismissed", refused: "closedrefused", superseded: "closedsuperseded" }[o] || "closed";
  }
  // R15 U4 (track S6): the complete record could not be written again — it blocks.
  if (kind === "unrecorded" && j.blocks_finalize === true) kind = "unrecordedblocked";
  // R15 (track S10): maybe another account's FrameTrace — blocks, no dismiss hint.
  if (kind === "inprogress" && j.in_progress_code === "other-account") kind = "otheraccount";
  if (j && kind) return { kind, journal: j, detail: reason || detail, code: kind === "unpaired" ? finJournalUnpairedCode(j, reason) : "" };
  if (j && jState !== "recovered") return { kind: "check", journal: j, detail: reason || detail };
  if (st.finalize_recovered === true || jState === "recovered") return { kind: "recovered", journal: j, detail };
  return null;
}
function finJournalBlocks(s) {
  if (!s || !s.journal) return false;
  if (typeof s.journal.blocks_finalize === "boolean") return s.journal.blocks_finalize;
  return s.kind === "recovering" || s.kind === "otherhost" || s.kind === "otheraccount";
}
function finJournalHead(s) {
  if (s && s.kind === "unpaired" && FIN_UNPAIRED_SHORT[s.code]) return `이전 최종화 짝 없음(${FIN_UNPAIRED_SHORT[s.code]}) — 확인 필요`;
  return FIN_JOURNAL_HEAD[s && s.kind] || FIN_JOURNAL_HEAD.check;
}
function finJournalDismissCmd(st) {
  const j = st && st.finalize_journal;
  if (j && typeof j.dismiss_command === "string" && j.dismiss_command.trim()) return j.dismiss_command.trim();
  const dir = st && typeof st.case_dir === "string" ? st.case_dir : "";
  return dir ? `frametrace finalize-journal ${finJournalDismissQuote(dir)} --dismiss` : "frametrace finalize-journal --dismiss";
}
// R15 U3: single quotes like the server's form (assets/app/41-finalize.js).
function finJournalDismissQuote(dir) {
  const d = String(dir);
  return "'" + (/^[A-Za-z]:|\\/.test(d) ? d.replace(/'/g, "''") : d.replace(/'/g, "'\\''")) + "'";
}
function finShortDetail(text, max) {
  const lim = max || 90;
  const t = String(text == null ? "" : text).replace(/\s+/g, " ").trim();
  return t.length > lim ? t.slice(0, lim - 1).trimEnd() + "…" : t;
}
// Checklist rows of the journal: a block row when it blocks (recovering:
// "이전 최종화 복구 중"), a warning when it does not and is not plain info.
function finJournalChecks(st) {
  const s = finJournalState(st);
  if (!s) return [];
  const j = s.journal || {};
  const until = Number(j.blocked_until_unix);
  const host = s.kind === "otherhost" || s.kind === "otheraccount" ? [j.host ? "PC " + String(j.host) : "", until > 0 ? new Date(until * 1000).toLocaleString() + "까지 차단" : ""].filter(Boolean).join(" · ") : "";
  const cmd = finJournalDismissLines(st, s.kind).map(l => `${l.label}: ${l.cmd}`).join(" · ");
  const sub = [host, s.detail ? finShortDetail(s.detail) : "", cmd].filter(Boolean).join(" — ");
  if (finJournalBlocks(s)) {
    if (s.kind === "recovering") return [{ level: "block", text: "이전 최종화 복구 중", sub: "끝나면 '다시 확인'을 누르십시오." }];
    return [{ level: "block", text: "최종화 차단: " + finJournalHead(s), sub: sub || "결과 안내를 확인하십시오.", cmd: !!cmd }];
  }
  if (FIN_JOURNAL_INFO.has(s.kind)) return [];
  return [{ level: "warn", text: finJournalHead(s), sub, cmd: !!cmd }];
}
// R14 U3: the last source re-verification of this page (a newer run's
// MISMATCH blocks the finalize).
let lastVerifyVerdict = null;
function verifyChecks(v) {
  if (!v) return [];
  const out = [];
  if (v.kind === "newermismatch") out.push({ level: "block", text: v.label, sub: v.hint });
  else if (v.kind === "truncated") out.push({ level: "warn", text: v.label, sub: v.hint });
  // R15 (track I4): `clock_note` — its own warning row, the result stands.
  if (v.clock) out.push({ level: "warn", text: "원본 재검증: 시계 이상", sub: v.clock });
  return out;
}
async function gatherFinalizeChecks() {
  const checks = [];
  const add = (level, text, sub) => checks.push({ level, text, sub: sub || "" });
  let st = null;
  try { st = await jget("/api/status"); } catch (e) { st = null; }
  const journalRows = finJournalChecks(st);
  if (!st) add("block", "서버 상태를 확인할 수 없습니다", "연결이 복구된 뒤 다시 시도하십시오.");
  else if (st.busy && !journalRows.some(c => c.text === "이전 최종화 복구 중")) add("block", "다른 작업이 진행 중입니다", `현재 단계: ${st.phase || "-"} — 끝난 뒤 보고서를 생성하십시오.`);
  journalRows.forEach(c => checks.push(c));
  verifyChecks(lastVerifyVerdict).forEach(c => checks.push(c));
  let meta = null;
  try { meta = await jget("/api/records-meta"); } catch (e) { meta = null; }
  const caseId = meta && meta.manifest ? (meta.manifest.case_id || "case") : null;
  let summary = null;
  const w = viewerWindow();
  try { if (w && typeof w.__frametraceSummary === "function") summary = w.__frametraceSummary(); } catch (e) { summary = null; }

  // Unsaved / queued marks block the finalize (SHOULD-FIX 5): sealing a
  // package without the examiner's latest marks is a silent loss.
  let drafts = null;
  if (caseId) {
    const stored = readLs(`ft.viewer.${caseId}.annotations.v1`);
    drafts = stored && typeof stored === "object" ? Object.keys(stored).length : 0;
  }
  unsavedMarksChecks({
    pendingMarks: st ? Number(st.pending_marks) || 0 : null,
    saveState: viewerSaveState(),
    drafts,
  }).forEach(c => checks.push(c));

  const marks = meta && meta.annotations && Array.isArray(meta.annotations.marks) ? meta.annotations.marks : null;
  if (marks) {
    const real = marks.filter(m => m && m.status && m.status !== "noted");
    if (!real.length) add("warn", "판독 마크가 하나도 없습니다", "보고서에 검토 결과(중요/검토완료)가 표시되지 않습니다.");
    const impNoNote = real.filter(m => m.status === "important" && !String(m.note || "").trim()).length;
    if (impNoNote) add("warn", `메모 없는 '중요' 마크 ${impNoNote}건`, "중요 판단의 근거를 메모로 남기는 것을 권장합니다.");
    const noExaminer = marks.filter(m => !String(m.examiner || "").trim()).length;
    if (noExaminer) add("warn", `검토자 이름 없이 기록된 마크 ${noExaminer}건`, "이름 입력 이후 수정된 마크부터 검토자가 기록됩니다.");
  }
  if (summary) {
    const total = summary.total || 0;
    const failed = (summary.byStatus && summary.byStatus["validation-failed"]) || 0;
    const cand = (summary.byKind && summary.byKind.candidate) || 0;
    if (failed) add("warn", `검증 실패 항목 ${failed}건`, "보고서에 검증 실패로 표시됩니다 — 뷰어에서 확인하십시오.");
    if (cand) add("warn", `복구 전 후보 ${cand}건`, "검증되지 않은 후보는 보고서에서 확정 복구로 주장되지 않습니다.");
    const unmarked = Math.max(0, total - (summary.marked || 0));
    add(unmarked ? "warn" : "ok", unmarked ? `마크하지 않은 항목 ${unmarked} / ${total}건` : `전체 ${total}건 모두 마크됨`);
  }
  const audit = $("auditBadge");
  if (audit && /\bbad\b/.test(audit.className)) add("warn", "감사 로그 검증에 실패했습니다", audit.textContent);
  else if (audit && /\bok\b/.test(audit.className)) add("ok", audit.textContent);
  return { checks, caseId };
}
function renderChecklist(checks) {
  const icon = { block: "✕", warn: "!", ok: "✓" };
  return `<ul class="checklist">` + checks.map(c =>
    `<li class="${c.level}"${c.cmd ? ' data-journal-cmd="1"' : ""}><span class="ic" aria-hidden="true">${icon[c.level]}</span><span>${esc(c.text)}` +
    (c.sub ? `<small>${esc(c.sub)}</small>` : "") + `</span></li>`).join("") + `</ul>`;
}
function currentExaminerName(caseId) {
  const vi = viewerExaminerInput();
  if (vi && vi.value.trim()) return vi.value.trim();
  if (caseId) { const v = readLs(`ft.viewer.${caseId}.examiner`); if (typeof v === "string" && v.trim()) return v.trim(); }
  return loadPrefs().examinerName || "";
}
function propagateExaminerName(name, caseId) {
  savePrefs({ examinerName: name });
  const vi = viewerExaminerInput();
  if (vi) {
    vi.value = name;
    try { vi.dispatchEvent(new Event("input", { bubbles: true })); } catch (e) {}
  } else if (caseId) {
    try { localStorage.setItem(`ft.viewer.${caseId}.examiner`, JSON.stringify(name)); } catch (e) {}
  }
}
let finalizeExaminer = "";
async function preFinalizeDialog() {
  let state = await gatherFinalizeChecks();
  const body = () => {
    const blocking = state.checks.filter(c => c.level === "block").length;
    return `<p>보고서·패키지를 생성하기 전에 아래 항목을 확인하십시오.${blocking ? " <b>차단 항목을 해결해야 진행할 수 있습니다.</b>" : ""}</p>` +
      `<div class="field"><label class="fieldlabel" for="dlgExaminer">검토자 이름 (필수 — 입력한 그대로 sign-off·보고서·이후 판독 마크에 기록되며, 신원 인증은 아닙니다)</label>` +
      `<input type="text" id="dlgExaminer" autocomplete="name" value="${esc(currentExaminerName(state.caseId))}" placeholder="예: 홍길동 (소속)"></div>` +
      renderChecklist(state.checks) +
      (state.checks.some(c => c.retry) ? `<button type="button" class="ghost" id="dlgRetrySave">저장 다시 시도</button> ` : "") +
      `<button type="button" class="ghost" id="dlgRecheck">다시 확인</button>`;
  };
  const wire = () => {
    const rc = $("dlgRecheck");
    const rs = $("dlgRetrySave");
    if (rs) rs.addEventListener("click", async () => {
      rs.disabled = true;
      if (!retryViewerSave()) {
        $("dlgNote").textContent = "뷰어가 열려 있지 않습니다 — 검토 화면에서 뷰어를 연 뒤 저장하십시오.";
        rs.disabled = false;
        return;
      }
      $("dlgNote").textContent = "저장 중…";
      await new Promise(resolve => setTimeout(resolve, 1500));
      if (rc) rc.click();
    });
    if (rc) rc.addEventListener("click", async () => {
      const typed = $("dlgExaminer") ? $("dlgExaminer").value : "";
      rc.disabled = true;
      $("dlgNote").textContent = "확인 중…";
      state = await gatherFinalizeChecks();
      $("dlgBody").innerHTML = body();
      if ($("dlgExaminer")) $("dlgExaminer").value = typed;
      $("dlgNote").textContent = "";
      $("dlgOk").disabled = state.checks.some(c => c.level === "block");
      wire();
    });
  };
  const opened = askDialog({
    title: "최종화 전 점검",
    html: body(),
    okLabel: "보고서 생성 진행",
    cancelLabel: "취소",
    focus: "dlgExaminer",
    onOk: () => {
      const name = ($("dlgExaminer") && $("dlgExaminer").value.trim()) || "";
      if (state.checks.some(c => c.level === "block")) { $("dlgNote").textContent = "차단 항목을 먼저 해결하십시오."; return false; }
      if (!name) { $("dlgNote").textContent = "검토자 이름을 입력하십시오 (보고서에 기록됩니다)."; return false; }
      finalizeExaminer = name;
      propagateExaminerName(name, state.caseId);
      return true;
    },
  });
  $("dlgOk").disabled = state.checks.some(c => c.level === "block");
  wire();
  return opened;
}

$("btnFinalize").addEventListener("click", async () => {
  if (finalizing) return;
  const btn = $("btnFinalize");
  btn.disabled = true;
  let go = false;
  try { go = await preFinalizeDialog(); }
  catch (err) { toast("점검 정보를 불러오지 못했습니다: " + (err.message || err), "err"); }
  if (!go) { btn.disabled = false; return; }
  finalizing = true;
  btn.setAttribute("aria-busy", "true");
  $("finalHint").textContent = "보고서 생성 중… (원본 해시 재계산 포함 — 소스 크기에 따라 오래 걸릴 수 있습니다)";
  try {
    const res = await jpost("/api/finalize", { examiner: finalizeExaminer });
    if (!res.ok) {
      // R12 (track S): "최종화 차단: …" is a refusal (warning), not a failure.
      const err = String(res.error || "알 수 없는 오류");
      if (/^\s*최종화 차단:/.test(err)) { $("finalHint").textContent = err; toast(err, "warn"); }
      else $("finalHint").textContent = "실패: " + err;
      return;
    }
    $("finalHint").textContent = "보고서·패키지 생성 완료. 검토자(입력값 기록): " + (res.examiner || "-") +
      (res.manifest_sha256 ? " · manifest.sha256의 SHA-256: " + res.manifest_sha256 : "");
    $("finalLinks").innerHTML =
      `<a href="/case/reports/case-report.html" target="_blank"><b>최종 결과 보고서 열기</b><span>발견 원본 영상 · 메타데이터 · SHA-256 · 발견/분석 기법 · 처리 체인</span></a>` +
      `<a href="/case/${esc(res.package_url || "")}" target="_blank"><b>패키지 폴더 보기</b><span>체크섬 검증된 증거 패키지</span></a>` +
      pkgExplorerLink(res.package_dir);
    if (res.package_dir) {
      $("pkgPath").classList.add("show");
      $("pkgPath").textContent = "패키지: " + res.package_dir;
    }
    setStep(4, 4);
    ensureReportFrame();
  } finally {
    finalizing = false;
    btn.removeAttribute("aria-busy");
    btn.disabled = false;
    if (/중…/.test($("finalHint").textContent)) $("finalHint").textContent = "보고서 생성이 완료되지 않았습니다 — 다시 시도할 수 있습니다.";
  }
});

// SHOULD-FIX 4: "all match" only when something was actually compared and
// nothing was left uncompared — 0 matched / N not comparable is a warning,
// never a green pass.
function verifySourcesVerdict(res) {
  // R14 (track I3): `head_status` of the re-verification lane anchor —
  // every verdict names it in the tooltip.
  const head = res.head_status == null ? "" : String(res.head_status);
  const v = (() => {
    const matched = Number(res.matched) || 0;
    const mismatched = Number(res.mismatched) || 0;
    const notComparable = Number(res.not_comparable) || 0;
    // R11 U4 (track I2): is_result / verified / unauthenticated → never "모두 일치"
    // (same rule as assets/app/41-finalize.js; absent fields = counts only).
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
    // R14 (track I3): the lane is shorter than its anchored head.
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
      return { level: "warn", toast: "원본 재검증: 현재 결과 없음(결과 아님)",
        hint: "원본 재검증: 현재 결과 없음(결과 아님)" + (why ? ` — ${why}` : "") + (unauth ? ` · 인증 안 된 줄 ${unauth}개` : "") + " (보관 이력서 5절 참조).",
        // R12 (track I4): the run this request started vs the lane's result run.
        tip: res.expected_run_id ? `이번 실행 ${res.expected_run_id} · 결과 실행 ${res.run_id || "없음"}` : "" };
    }
    if (res.verified === false || unauth > 0) {
      return { level: "warn", toast: "원본 재검증: 확인 불가" + (unauth ? ` — 인증 안 된 줄 ${unauth}개` : ""),
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
  const hrWord = { "other-pc": "다른 PC 머리일 수 있음", "unusable-line": "쓸 수 없는 줄", "chain-failed": "체인 실패", "repaired": "레인 수리", "malformed": "형식 오류", "own-head-missing": "결과 실행 머리 없음", "unreadable": "읽지 못함" }[hr] || hr;
  if (head) v.tip = [v.tip || "", `재검증 머리 ${head}` + (hrWord ? ` (${hrWord})` : "")].filter(Boolean).join(" · ");
  // R15 (track I4): "시계 이상" — its own warning, never withholds the result.
  const clock = typeof res.clock_note === "string" ? res.clock_note.trim() : "";
  if (clock) v.clock = clock;
  return v;
}

$("btnVerifySources").addEventListener("click", async () => {
  const btn = $("btnVerifySources");
  if (btn.disabled) return;
  btn.disabled = true;
  btn.setAttribute("aria-busy", "true");
  $("finalHint").textContent = "원본 재검증 중… (색인된 모든 원본을 다시 해시합니다 — 소스 크기에 따라 오래 걸릴 수 있습니다)";
  $("finalHint").title = "";
  try {
    const res = await jpost("/api/verify-sources", {});
    if (!res.ok) { $("finalHint").textContent = "원본 재검증 실패: " + (res.error || "알 수 없는 오류"); return; }
    const verdict = verifySourcesVerdict(res);
    lastVerifyVerdict = verdict;
    // R13 (track I1): a newer run's mismatches — id + source path listed,
    // hashes in the tooltip.
    const rows = Array.isArray(verdict.items) ? verdict.items : [];
    $("finalHint").textContent = verdict.hint + rows.map(m => `\n· ${m.id || "-"} ${m.path}`).join("") + (verdict.more > 0 ? `\n· 외 ${verdict.more}건` : "") + (verdict.clock ? "\n" + verdict.clock : "");
    $("finalHint").title = [verdict.tip || "", ...rows.map(m => `${m.id || "-"}: ${m.tip.replace(/\n/g, " · ")}`)].filter(Boolean).join("\n");
    toast(verdict.toast, verdict.level);
  } catch (err) {
    $("finalHint").textContent = "원본 재검증 실패: " + (err.message || err);
  } finally {
    btn.removeAttribute("aria-busy");
    btn.disabled = false;
  }
});

// Package dir is a server-side path, not a browsable URL — this link POSTs
// to /api/open-folder so the OS shell opens it instead of surprise-popping
// Explorer the moment the report finishes.
function pkgExplorerLink(dir) {
  if (!dir) return "";
  return `<a href="#" data-pkgdir="${esc(dir)}"><b>탐색기에서 패키지 폴더 열기</b><span>${esc(dir)}</span></a>`;
}
document.addEventListener("click", async e => {
  const a = e.target.closest && e.target.closest("a[data-pkgdir]");
  if (!a) return;
  e.preventDefault();
  const res = await jpost("/api/open-folder", { path: a.dataset.pkgdir });
  if (!res.ok) toast("폴더를 열지 못했습니다: " + (res.error || "알 수 없는 오류") + " — 경로를 직접 여십시오: " + a.dataset.pkgdir, "err");
});

$("btnImportMarks").addEventListener("click", () => $("marksFile").click());
$("marksFile").addEventListener("change", async () => {
  const input = $("marksFile");
  const file = input.files[0];
  if (!file) return;
  const btn = $("btnImportMarks");
  btn.disabled = true;
  $("finalHint").textContent = "마크 반영 및 보고서 갱신 중…";
  try {
    let marksJson;
    try { marksJson = await file.text(); }
    catch (err) { $("finalHint").textContent = "실패: 파일을 읽을 수 없습니다 — " + (err.message || err); return; }
    const res = await jpost("/api/import-marks", { marks_json: marksJson });
    $("finalHint").textContent = res.ok ? "보고서에 판독 마크가 반영되었습니다." : "실패: " + (res.error || "알 수 없는 오류");
    if (res.ok && !$("finalLinks").innerHTML) {
      $("finalLinks").innerHTML =
        `<a href="/case/${esc(res.report_url || "")}" target="_blank"><b>갱신된 최종 결과 보고서 열기</b><span>판독 마크 포함</span></a>`;
    }
  } finally {
    btn.disabled = false;
    // Same file picked twice must fire "change" again.
    input.value = "";
    if (/중…$/.test($("finalHint").textContent)) $("finalHint").textContent = "";
  }
});

