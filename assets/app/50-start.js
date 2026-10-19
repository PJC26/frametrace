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
  // §28: deepfake screening ticked but deepfake-lens is not installed —
  // offer the install here instead of starting.
  if (typeof htHidePrompt === "function") htHidePrompt($("startToolPrompt"));
  if ($("withDeepfake").checked && typeof htNeedsTool === "function" && htNeedsTool("deepfake-lens")) {
    htShowToolPrompt($("startToolPrompt"), "deepfake-lens", () => $("btnStart").click(), "retryStart");
    return;
  }
  starting = true;
  $("btnStart").disabled = true;
  const hint = $("startHint");
  const prevHint = hint.textContent;
  hint.textContent = "분석 시작 요청 중…";
  $("btnStart").setAttribute("aria-busy", "true");
  let started = false;
  try {
    const res = await jpost("/api/start", {
      input_kind: inputKind(),
      source_path: source,
      case_dir: caseDir,
      with_hash: $("withHash").checked,
      with_ffprobe: $("withFfprobe").checked,
      with_deepfake: $("withDeepfake").checked,
      with_audio: $("withAudio").checked && !$("withAudio").disabled,
      skip_e01_verify: $("skipE01Verify").checked,
      e01_direct: inputKind() === "e01" && $("directE01").checked
    });
    if (!res.ok && res.tool_missing && typeof htShowToolPrompt === "function") {
      htShowToolPrompt($("startToolPrompt"), String(res.tool_missing), () => $("btnStart").click(), "retryStart");
      return;
    }
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
    // Straight into the host: the chip shows progress, the jobs sheet
    // opens once on the first start, the viewer mounts at review-partial.
    FTStore.set({ ui: { home: false } });
    hAutoOpenJobs();
  } finally {
    starting = false;
    hint.textContent = prevHint;
    $("btnStart").removeAttribute("aria-busy");
    // The poll owns the steady-state enable/disable; this only undoes the
    // optimistic disable so a failed request never strands the button.
    $("btnStart").disabled = started || finalizing || !!(lastStatus && lastStatus.phase === "running");
  }
});

// Switching cases must not leak the previous case's state into the UI.
// The deliverables sheet (41-finalize.js) and the jobs sheet (32-jobs.js)
// reset themselves on a project change; this clears what the shell owns.
function resetCaseUi() {
  auditVerified = false;
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
  // Flags were already reset above — the next poll mounts the viewer once
  // the new case reports a reviewable phase.
  return true;
}

// btnCancel / btnNew (work sheet) are wired in 32-jobs.js.
// UX-18: a page reload also reloads the viewer, so it is guarded exactly
// like the viewer's own refresh: never while marks/memos are not yet in
// the case DB — offer to save instead.
async function hUnsavedCount() {
  const ss = hViewer.saveState();
  let pending = 0;
  try { pending = await hPendingChanges(); } catch (e) { pending = 0; }
  const unsaved = hViewerStateUnsaved(ss, () => Number(pending) || 0);
  return { pending: Number(pending) || 0, unsaved, saveState: ss };
}
async function guardedPageReload(reason) {
  const u = await hUnsavedCount();
  if (u.pending > 0 || u.unsaved) {
    const go = await askDialog({
      title: reason || "화면 새로 고침",
      html: `<p><b>케이스에 아직 반영되지 않은 마크·메모 변경${u.pending ? ` ${u.pending}건` : ""}</b>이 있어 화면을 새로 고치지 않았습니다.</p>` +
        `<p>'저장 다시 시도'로 저장한 뒤 다시 누르십시오 — 저장 상태가 '저장됨'이 되면 진행할 수 있습니다.</p>`,
      okLabel: "저장 다시 시도", cancelLabel: "닫기",
    });
    if (go && !hViewer.save()) toast("뷰어가 열려 있지 않아 저장을 다시 시도하지 못했습니다 — 검토 화면에서 저장 상태를 확인하십시오.", "warn");
    return false;
  }
  location.reload();
  return true;
}
$("directE01").addEventListener("change", syncDirectE01);
document.querySelectorAll("input[name='scanMode']").forEach(r => r.addEventListener("change", () => applyScanMode(r.value)));
$("withHash").addEventListener("change", syncScanMode);
$("withFfprobe").addEventListener("change", syncScanMode);

// Sound measurement (docs/PLAN_PHASE2_JOIN_AUDIO_2026-10-13.md §2.2): off by
// default; it picks its targets from ffprobe, so it is disabled — with the
// reason in the tooltip — while ffprobe is off or missing. Re-synced
// whenever the options open or ffprobe / the scan mode change.
const WITH_AUDIO_TIP = "오디오 트랙을 끝까지 디코드해 최대·평균 신호 레벨(dBFS)로 무음 여부를 잽니다 — 말소리 여부는 판단하지 않습니다";
function syncAudioOpt() {
  const box = $("withAudio");
  if (!box) return;
  const ffmpegMissing = typeof envState !== "undefined" && !!envState && envState.ffmpeg === false;
  const ff = $("withFfprobe").checked && !ffprobeMissing && !ffmpegMissing;
  box.disabled = !ff;
  if (!ff) box.checked = false;
  const tip = ff ? WITH_AUDIO_TIP
    : ffmpegMissing ? "ffmpeg가 없어 사용할 수 없습니다 — 소리 측정은 ffmpeg로 오디오를 디코드합니다"
    : ffprobeMissing ? "ffprobe가 없어 사용할 수 없습니다 — 소리 측정은 ffprobe가 찾은 오디오 트랙만 잽니다"
      : "ffprobe 재생성 검증을 켜야 사용할 수 있습니다 — 소리 측정은 ffprobe가 찾은 오디오 트랙만 잽니다";
  const wrap = $("withAudioWrap");
  if (wrap) wrap.title = tip;
}
$("withFfprobe").addEventListener("change", syncAudioOpt);
document.querySelectorAll("input[name='scanMode']").forEach(r => r.addEventListener("change", syncAudioOpt));
$("startOptions").addEventListener("toggle", syncAudioOpt);
syncAudioOpt();
