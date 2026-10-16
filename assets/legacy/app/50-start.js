let starting = false;
$("btnStart").addEventListener("click", async () => {
  if (starting) return;
  const source = $("sourcePath").value.trim();
  if (!source) {
    showStartErr("증거 경로를 입력하십시오 — E01 이미지 파일이나 폴더·드라이브 경로입니다.");
    try { $("sourcePath").focus(); } catch (e) {}
    return;
  }
  $("startErr").classList.remove("show");
  // Preflight: never write case output inside the evidence source.
  const caseDir = $("caseDir").value.trim();
  // UX-11: the server-side preflight (when this server has it) also sees
  // symlinks/junctions the string check cannot; give it a bounded moment.
  starting = true;
  let pf = null, det = null;
  try {
    // The evidence kind is auto-detected (folder vs E01 image); a missing
    // or unsupported path stops here with an inline error.
    det = await detectSourceKind(source);
    if (det.kind === "missing" || det.kind === "other") {
      applyDetection(source, det);
      showStartErr(det.message);
      try { $("sourcePath").focus(); } catch (e) {}
      return;
    }
    applyDetection(source, det);
    pf = await ensurePreflight(source, caseDir);
  } finally { starting = false; }
  if ((caseDir && pathInside(caseDir, source)) || (pf && pf.case_inside_source === true)) {
    const go = await askDialog({
      title: "케이스 폴더가 증거 소스 안에 있습니다",
      html: `<p>케이스 폴더 <code>${esc(caseDir)}</code>가 증거 소스 <code>${esc(source)}</code> 안에 있습니다.</p>` +
        `<p>원본 증거 매체에 산출물을 기록하게 되며, 다음 스캔에서 산출물이 증거로 다시 색인될 수 있습니다. 다른 드라이브의 폴더를 지정하는 것을 강력히 권장합니다.</p>`,
      okLabel: "그래도 진행", cancelLabel: "케이스 폴더 바꾸기", danger: true,
    });
    if (!go) { $("caseDir").focus(); return; }
  }
  starting = true;
  $("btnStart").disabled = true;
  const hint = $("startHint");
  const prevHint = hint.textContent;
  hint.textContent = "분석 시작 요청 중…";
  $("btnStart").setAttribute("aria-busy", "true");
  cancelRequested = false;
  $("btnCancel").disabled = false;
  $("btnCancel").removeAttribute("aria-busy");
  $("btnCancel").textContent = "분석 중단";
  let started = false;
  try {
    const res = await jpost("/api/start", {
      input_kind: inputKind(),
      source_path: source,
      case_dir: caseDir,
      with_hash: $("withHash").checked,
      with_ffprobe: $("withFfprobe").checked,
      with_deepfake: $("withDeepfake").checked,
      skip_e01_verify: $("skipE01Verify").checked,
      e01_direct: inputKind() === "e01" && $("directE01").checked
    });
    if (!res.ok) {
      $("startErr").classList.add("show");
      $("startErr").innerHTML = errorHtml(res.error || "시작 실패");
      return;
    }
    savePrefs({
      inputKind: inputKind(),
      scanMode: scanMode(),
      withHash: $("withHash").checked,
      withFfprobe: $("withFfprobe").checked,
      withDeepfake: $("withDeepfake").checked,
      skipE01Verify: $("skipE01Verify").checked,
      directE01: $("directE01").checked,
      lastSource: source,
      lastCase: caseDir,
    });
    pushRecent("recentSources", source);
    pushRecent("recentCases", caseDir);
    started = true;
    resetLogView();
    jobSeen = true; autoProgress = true; autoReviewed = false;
    $("s2").disabled = false;
    showView(2);
    setStep(3, 2);
  } finally {
    starting = false;
    hint.textContent = prevHint;
    $("btnStart").removeAttribute("aria-busy");
    // The poll owns the steady-state enable/disable; this only undoes the
    // optimistic disable so a failed request never strands the button.
    $("btnStart").disabled = started || finalizing || !!(lastStatus && lastStatus.phase === "running");
  }
});

// Switching cases must not leak the previous case's state into the UI —
// badges, links and package paths describe whatever the server last
// reported, so clear them before the poll repopulates from the new case.
function resetCaseUi() {
  jobSeen = false; reviewReady = false; autoReviewed = false; autoProgress = false;
  auditVerified = false;
  lastVerifyVerdict = null;
  $("s2").disabled = true; $("s3").disabled = true; $("s4").disabled = true;
  setStep(1, 0);
  $("btnFinalize").disabled = true;
  $("btnFinalize").textContent = "결과 보고서 생성 및 패키징";
  $("finalLinks").innerHTML = "";
  $("finalHint").textContent = "";
  $("recoverHint").textContent = "조사된 이미지에서 삭제된 영상 후보를 자동 복구한 뒤 뷰어를 갱신합니다.";
  $("pkgPath").classList.remove("show");
  $("pkgPath").textContent = "";
  $("advOut").hidden = true;
  $("advOut").textContent = "";
  $("advLink").innerHTML = "";
  $("track").innerHTML = "";
  setCaseToolsVisible(false);
  resetLogView();
  shownStatusError = null;
  const audit = $("auditBadge");
  if (audit) { audit.className = "badge"; audit.textContent = "감사 로그 대기"; audit.removeAttribute("title"); }
  syncToolsBtn();
}

$("btnOpenCase").addEventListener("click", () => openCase($("caseDir").value.trim()));
async function openCase(dir) {
  if (!dir) {
    $("startErr").classList.add("show");
    $("startErr").textContent = "열 기존 케이스 폴더 경로를 입력하십시오.";
    return false;
  }
  $("startErr").classList.remove("show");
  let res;
  try {
    res = await jpost("/api/open-case", { case_dir: dir });
  } catch (err) {
    res = { ok: false, error: "서버에 연결할 수 없습니다: " + err.message };
  }
  resetCaseUi();
  if (!res.ok) {
    $("startErr").classList.add("show");
    $("startErr").textContent = "오류: " + (res.error || "케이스 열기 실패");
    return false;
  }
  if (!res.has_review && !res.has_index) {
    $("startErr").classList.add("show");
    $("startErr").textContent = "케이스를 열었으나 검토 결과가 아직 없습니다 — 이전 분석이 중단되었을 수 있습니다. 같은 증거 소스를 지정하고 '분석 시작'을 누르면 체크포인트에서 이어서 진행합니다.";
    renderRecentCases();
    return false;
  }
  // UX-02: an index without the review bundle (enrichment cancelled) is
  // still reviewable — the server opened it as review-partial.
  if (!res.has_review) toast("색인만 있는 케이스입니다 — 해시·검증 보강 전 상태로 엽니다.", "warn");
  pushRecent("recentCases", dir);
  savePrefs({ lastCase: dir });
  // Flags were already reset above — the next poll re-enables the rail
  // and jumps to the review screen when the new case reports its state.
  return true;
}

let cancelRequested = false;
$("btnCancel").addEventListener("click", async () => {
  const go = await askDialog({
    title: "분석 중단",
    html: "<p>분석을 중단하시겠습니까?</p><p>중단 후 같은 소스로 '분석 시작'을 다시 누르면 같은 케이스에서 이어서 분석합니다 — 이미 계산된 해시·메타데이터는 다시 계산하지 않고 남은 파일만 처리합니다.</p>",
    okLabel: "분석 중단", cancelLabel: "계속 분석", danger: true, focus: "dlgCancel",
  });
  if (!go) return;
  cancelRequested = true;
  $("btnCancel").disabled = true;
  $("btnCancel").setAttribute("aria-busy", "true");
  $("btnCancel").textContent = "중단 요청 중…";
  const res = await jpost("/api/cancel", {});
  if (!res.ok) {
    cancelRequested = false;
    $("btnCancel").disabled = false;
    $("btnCancel").removeAttribute("aria-busy");
    $("btnCancel").textContent = "분석 중단";
    toast("중단 요청 실패: " + (res.error || "알 수 없는 오류"), "err");
  }
});
// Reload alone lands back on the review stage (the poll auto-jumps on a
// completed case); the flag keeps stage 1 on this load only.
// UX-18: a page reload also reloads the embedded viewer, so it is guarded
// exactly like the viewer's own refresh: never while marks/memos are not
// yet in the case DB — offer to save instead.
async function viewerUnsavedCount() {
  const ss = viewerSaveState();
  let pending = 0;
  try { pending = await pendingViewerChanges(); } catch (e) { pending = 0; }
  const unsaved = ss === "pending" || ss === "saving" || ss === "failed" || ss === "queued";
  return { pending: Number(pending) || 0, unsaved, saveState: ss };
}
async function guardedPageReload(reason) {
  const u = await viewerUnsavedCount();
  if (u.pending > 0 || u.unsaved) {
    const go = await askDialog({
      title: reason || "화면 새로 고침",
      html: `<p><b>케이스에 아직 반영되지 않은 마크·메모 변경${u.pending ? ` ${u.pending}건` : ""}</b>이 있어 화면을 새로 고치지 않았습니다.</p>` +
        `<p>'저장 다시 시도'로 저장한 뒤 다시 누르십시오 — 저장 상태가 '저장됨'이 되면 진행할 수 있습니다.</p>`,
      okLabel: "저장 다시 시도", cancelLabel: "닫기",
    });
    if (go && !retryViewerSave()) toast("뷰어가 열려 있지 않아 저장을 다시 시도하지 못했습니다 — 검토 화면에서 저장 상태를 확인하십시오.", "warn");
    return false;
  }
  location.reload();
  return true;
}
$("btnNew").addEventListener("click", async () => {
  const btn = $("btnNew");
  btn.disabled = true;
  try {
    try { sessionStorage.setItem("ft_want_new", "1"); } catch (e) { /* storage blocked: stage 1 may auto-advance */ }
    const reloaded = await guardedPageReload("새 케이스 시작");
    if (!reloaded) { try { sessionStorage.removeItem("ft_want_new"); } catch (e) {} }
  } finally {
    btn.disabled = false;
  }
});

$("directE01").addEventListener("change", syncDirectE01);
document.querySelectorAll("input[name='scanMode']").forEach(r => r.addEventListener("change", () => applyScanMode(r.value)));
$("withHash").addEventListener("change", syncScanMode);
$("withFfprobe").addEventListener("change", syncScanMode);
