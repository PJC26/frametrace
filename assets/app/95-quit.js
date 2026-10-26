// ---- 워크스테이션 종료 ----
// frametrace-app.exe has no console window, so this button is the only
// clean way to stop the server. Busy jobs must be cancelled first — the
// server refuses shutdown while busy to protect audit/carve output.
function showQuitPage() {
  shuttingDown = true;
  clearTimeout(pollTimer);
  setServerLost(false);
  hClosePopovers(false);
  hCloseSheet("jobs", false);
  hCloseSheet("finalize", false);
  $("main").hidden = true;
  $("hTopStrip").inert = true;
  $("hEnv").innerHTML = "";
  $("hBtnQuit").hidden = true;
  $("quitPage").hidden = false;
}
// Unsynced viewer changes (same signal as the finalize checklist).
async function hPendingChanges() {
  const sm = hViewer.summary();
  if (sm && Number.isFinite(sm.pending)) return sm.pending;
  if (sm && Number.isFinite(sm.unsynced)) return sm.unsynced;
  if (!FTStore.hasProject()) return 0;
  try {
    const meta = await jget("/api/records-meta");
    // R8 U1: the viewer's own storage key of the case (case.json's case_id;
    // an unreadable case.json: a hash of the case folder — never "case").
    const caseId = meta && meta.ok !== false && typeof FTViewer === "object" && FTViewer && FTViewer.caseStorageKey
      ? FTViewer.caseStorageKey(meta.manifest, meta.caseDir)
      : (meta && meta.manifest && meta.manifest.case_id ? String(meta.manifest.case_id) : null);
    // Per-id draft keys (ft.draft.<case>.<writer>.<id>) plus any legacy
    // single-key drafts not yet migrated; null = storage unreadable.
    return caseId ? (unsavedDraftCount(caseId) || 0) : 0;
  } catch (e) { return 0; }
}
// ---- A15 / C15: flush the viewer before the window closes ----
// The desktop shell (src/shell.rs FLUSH_SCRIPT, C15) runs FTViewer.save()
// in the page before its window goes (capped at 5 s) and names the unsaved
// edits in its confirmation. hBeforeClose is the same flush for the page
// itself (and the harness): FTViewer.save() awaits a save already in flight
// and pushes what is left; ok:false comes with "저장 안 된 편집 N건".
// R6 U4 (S ↔ U contract): the desktop shell's FLUSH_SCRIPT prefers
// window.hBeforeClose() over FTViewer.save() — so the answer must come
// within the shell's 5 s wait: default 4 s, and the viewer's own busy
// budget is capped below it (FTViewer.save({ timeoutMs })). A read-only
// close with nothing held here answers ok + `warning` at once; the
// warning is shown here too (toast) and carried for the shell's dialog.
// R7 U1 (S7 ↔ U): the shell calls hBeforeClose(4000) and `timeoutMs` is a
// HARD TOTAL budget measured from the call: the wait for a save already
// on the wire, the conflict lookup, the send and any busy retry all fit in
// it (FTViewer.save({timeoutMs}) is itself a total deadline now, given a
// little less), and a backstop timer answers at the budget whatever the
// viewer does. Everything after the race is synchronous.
const H_CLOSE_TIMEOUT_MS = 4000;
const H_CLOSE_MARGIN_MS = 250;
async function hBeforeClose(timeoutMs = H_CLOSE_TIMEOUT_MS) {
  const started = Date.now();
  const asked = Number(timeoutMs);
  const ms = Math.max(0, Number.isFinite(asked) ? asked : H_CLOSE_TIMEOUT_MS);
  const api = hViewerApi();
  const mounted = !!(api && FTStore.state.viewer && FTStore.state.viewer.mounted);
  if (!mounted) return { ok: true, pending: 0, state: null, message: "", warning: "" };
  let saved = false;
  let timedOut = false;
  let timer = 0;
  const opts = { timeoutMs: Math.max(0, ms - H_CLOSE_MARGIN_MS) };
  try {
    saved = await Promise.race([
      Promise.resolve(api.save(opts)).then(v => v === true),
      new Promise(resolve => { timer = setTimeout(() => { timedOut = true; resolve(false); }, Math.max(0, ms - (Date.now() - started) - 100)); }),
    ]);
  } catch (e) { saved = false; }
  clearTimeout(timer);
  if (opts.timedOut) timedOut = true;
  let pending = 0;
  let state = null;
  let cause = "";
  try {
    const sm = api.summary() || {};
    pending = Number(sm.pending) || 0;
    cause = typeof sm.lockCause === "string" ? sm.lockCause : "";
    state = api.saveState();
  } catch (e) { pending = 0; }
  // A durable server queue counts as saved (it reaches the case DB when the job ends).
  // R5 U15: read-only with nothing pending here (FTViewer.save() true: no
  // draft, no edit) loses nothing by closing — no confirmation, a warning only.
  const readonlyClean = saved && state === "readonly" && pending === 0;
  const ok = readonlyClean || (saved && !hViewerStateUnsaved(state === "queued" ? "saved" : state, () => pending));
  const late = timedOut ? " (저장 응답 대기 시간 초과)" : "";
  const message = ok ? ""
    : pending > 0 ? `저장 안 된 편집 ${pending}건` + late
    : state === "readonly" ? (cause === "case" ? "케이스 정보(case.json)를 읽지 못해 저장이 막혀 있습니다" : "판독 기록을 읽지 못해 저장이 막혀 있습니다")
    : "모든 편집이 저장되었는지 확인하지 못했습니다" + late;
  // R9 U2: the read-only cause in the words of the viewer (case.json vs the marks).
  const warning = !readonlyClean ? ""
    : cause === "case" ? "케이스 정보(case.json)를 읽지 못한 상태였습니다 — 이 창에는 저장 안 된 편집이 없습니다."
    : "판독 기록을 읽지 못한 상태였습니다 — 이 창에는 저장 안 된 편집이 없습니다.";
  if (warning && typeof toast === "function") toast(warning, "warn");
  return { ok, pending, state, message, warning };
}
window.hBeforeClose = hBeforeClose;
$("hBtnQuit").addEventListener("click", async () => {
  const btn = $("hBtnQuit");
  btn.disabled = true;
  btn.setAttribute("aria-busy", "true");
  try {
    let st = null;
    try { st = await jget("/api/status"); } catch (e) { st = null; }
    if (!st) {
      await alertDialog("서버 종료", "서버에 연결할 수 없습니다 — 이미 종료되었을 수 있습니다.");
      return;
    }
    const pending = await hPendingChanges();
    const pendingHtml = pending > 0
      ? `<p><b>케이스에 아직 반영되지 않은 마크·메모 변경 ${pending}건</b>이 있습니다. 서버를 종료하면 이 브라우저의 임시 저장본에만 남습니다 — 취소하고 몇 초 기다려 자동 반영된 뒤 종료하는 것을 권장합니다.</p>`
      : "";
    const go = await askDialog({
      title: "워크스테이션 서버 종료",
      html: (st.busy ? `<p><b>작업이 진행 중입니다</b> (${esc(st.phase || "")}). 작업을 중단한 뒤 서버를 종료합니다 — 중단된 분석은 다음 실행 때 체크포인트에서 이어서 진행할 수 있습니다.</p>` : "") +
        pendingHtml +
        `<p>서버를 종료하면 이 화면과 뷰어가 더 이상 동작하지 않습니다. 케이스 데이터와 감사 로그는 보존됩니다.<br>다시 시작하려면 frametrace-app을 실행하십시오.</p>`,
      okLabel: st.busy ? "작업 중단 후 서버 종료" : "서버 종료",
      cancelLabel: "계속 작업",
      danger: true,
      focus: "dlgCancel",
      onOk: async () => {
        // Status captured at dialog open may be stale: re-check before cancel.
        let cur = null;
        try { cur = await jget("/api/status"); } catch (e) { cur = null; }
        if (cur && cur.busy) {
          $("dlgNote").textContent = "작업 중단 요청 중…";
          const cr = await jpost("/api/cancel", {});
          if (!cr.ok) {
            // "no job running" means it finished meanwhile — fine, unless still busy.
            let again = null;
            try { again = await jget("/api/status"); } catch (e) { again = null; }
            if (!again || again.busy) { $("dlgNote").textContent = "중단 요청 실패: " + (cr.error || "알 수 없는 오류"); return false; }
          }
          let still = !!cr.ok;
          for (let i = 0; i < 120 && still; i++) {
            await new Promise(r => setTimeout(r, 500));
            let s2 = null;
            try { s2 = await jget("/api/status"); } catch (e) { s2 = null; }
            still = !!(s2 && s2.busy);
            $("dlgNote").textContent = `작업이 멈추기를 기다리는 중… ${Math.round((i + 1) / 2)}초 / 최대 60초`;
          }
          if (still) { $("dlgNote").textContent = "작업이 아직 멈추지 않았습니다 — 잠시 후 다시 시도하십시오."; return false; }
        }
        $("dlgNote").textContent = "서버 종료 요청 중…";
        const res = await jpost("/api/shutdown", {});
        if (res && res.running) { $("dlgNote").textContent = "작업이 아직 진행 중입니다 — 잠시 후 다시 시도하십시오."; return false; }
        // A network error here usually means the server already went away.
        if (!res.ok && !res.network) { $("dlgNote").textContent = "종료 실패: " + (res.error || "알 수 없는 오류"); return false; }
        return true;
      },
    });
    if (go) showQuitPage();
  } finally {
    btn.removeAttribute("aria-busy");
    btn.disabled = false;
  }
});

