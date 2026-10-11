// --- IN/OUT range sync (FOR-01) ------------------------------------------
// Ranges ride in the marks payload as `range: {in, out}` (null = unset).
// Ids whose server and browser ranges disagree at boot stay in
// rangeConflictIds until the examiner picks a side; while there, the
// payload omits their range so a routine save can never overwrite the
// other side silently.
var rangeConflictIds = new Set();

function rangeValid(range) {
  return !!range && (Number.isFinite(range.in) || Number.isFinite(range.out));
}
function normRange(range) {
  return rangeValid(range)
    ? { in: Number.isFinite(range.in) ? range.in : null, out: Number.isFinite(range.out) ? range.out : null }
    : null;
}
function sameRange(a, b) {
  return JSON.stringify(normRange(a)) === JSON.stringify(normRange(b));
}

function marksPayload() {
  // Marks and free-text notes travel together: a record with only a note
  // is exported with status "noted" so the case DB keeps the memo.
  const ids = new Set(Object.keys(state.marks));
  Object.keys(state.notes).forEach(id => { if ((state.notes[id] || "").trim()) ids.add(id); });
  // Self-contained on purpose (the review-batch VM harness evaluates this
  // function alone): ranges are normalised and conflict ids read locally.
  const conflicts = typeof rangeConflictIds === "undefined" ? new Set() : rangeConflictIds;
  const rangeOfId = id => {
    const r = state.ranges && state.ranges[id];
    return r && (Number.isFinite(r.in) || Number.isFinite(r.out))
      ? { in: Number.isFinite(r.in) ? r.in : null, out: Number.isFinite(r.out) ? r.out : null }
      : null;
  };
  Object.keys(state.ranges || {}).forEach(id => { if (rangeOfId(id) && !conflicts.has(id)) ids.add(id); });
  const marks = [...ids].map(id => {
    const mark = state.marks[id];
    const note = (state.notes[id] || "").trim();
    const entry = {
      id,
      status: mark ? mark.status : "noted",
      marked_unix: mark ? mark.marked_unix : Math.floor(Date.now() / 1000),
      note,
      examiner: state.examiners[id] ?? ((state.examiner || "").trim() || null)
    };
    if (!conflicts.has(id)) entry.range = rangeOfId(id) || { in: null, out: null };
    return entry;
  });
  const tagEntries = Object.entries(state.tags).map(([id, tags]) => ({ id, tags }));
  return {
    schema_version: 2,
    protocol: "patch-v1",
    deleted_ids: state.deletedIds.filter(id => !ids.has(id)),
    case_id: manifest.case_id || null,
    examiner: (state.examiner || "").trim() || null,
    exported_unix: Math.floor(Date.now() / 1000),
    marks,
    tags: tagEntries
  };
}

document.getElementById("btnDownloadMarks").addEventListener("click", () => {
  const payload = marksPayload();
  if (!payload.marks.length && !payload.tags.length && !payload.deleted_ids.length) { toast(t("toast.noChanges")); return; }
  downloadJSON(`frametrace-marks-${manifest.case_id || "case"}.json`, payload);
});

// --- Review-change protection + save state (FOR-01) ----------------------
// touchAnnotation() funnels every mark/note/tag mutation, and deletedIds
// tracks records changed since the last server import. A served viewer
// pushes those changes to the case DB on its own: debounced after edits,
// retried with exponential backoff after a failure (busy lock, server
// restart, network), and never dropped — the drafts stay in localStorage
// until the server confirms them. The #saveState indicator always shows
// which of saved / saving / N pending / FAILED applies, so an examiner
// can never believe unsaved work reached the case.
// `var` (not let): render() calls renderSaveState(), and these must read
// as undefined rather than throw if a render ever runs before this file.
var pendingReviewSync = state.deletedIds.length > 0;
var autosaveTimer = null;
var saveInFlight = false;   // one import at a time
var saveQueued = false;     // a save was requested while one was in flight
var manualQueued = false;   // "Apply to case" was pressed while one was in flight
var saveRevision = 0;       // bumps on every change; detects edits mid-flight
var saveFailures = 0;
var saveError = "";
var queuedOnServer = 0;     // marks the server holds in its phase-2 queue (durable, not yet in the DB)
var saveRetryAt = 0;
var viewerIsServed = location.protocol === "http:" || location.protocol === "https:";
var saveStateEl = document.getElementById("saveState");
const AUTOSAVE_DEBOUNCE_MS = 2500;
const AUTOSAVE_RETRY_BASE_MS = 2000;
const AUTOSAVE_RETRY_MAX_MS = 60000;

function autosaveRetryDelay(failures) {
  return Math.min(AUTOSAVE_RETRY_MAX_MS, AUTOSAVE_RETRY_BASE_MS * Math.pow(2, Math.max(0, failures - 1)));
}

function pendingSaveCount() {
  return state.deletedIds.length || (pendingReviewSync ? 1 : 0);
}

function renderSaveState() {
  if (!saveStateEl) return;
  const n = pendingSaveCount();
  let cls, label, title, kind;
  if (!viewerIsServed) {
    kind = "local";
    cls = "candidate";
    label = n ? tf("save.local", { n }) : t("save.localClean");
    title = t("save.title.local");
  } else if (saveInFlight) {
    kind = "saving";
    cls = "candidate";
    label = t("save.saving");
    title = t("save.title.saving");
  } else if (saveFailures > 0 && pendingReviewSync) {
    kind = "failed";
    cls = "failed";
    label = tf("save.failed", { n });
    const sec = Math.max(0, Math.ceil((saveRetryAt - Date.now()) / 1000));
    title = tf("save.title.failed", { err: saveError || "?", sec });
  } else if (pendingReviewSync) {
    kind = "pending";
    cls = "candidate";
    label = tf("save.pending", { n });
    title = tf("save.title.pending", { n });
  } else if (queuedOnServer > 0) {
    kind = "queued";
    cls = "candidate";
    label = t("save.queuedServer");
    title = tf("save.title.queuedServer", { n: queuedOnServer });
  } else {
    kind = "saved";
    cls = "ok";
    label = t("save.saved");
    title = t("save.title.saved");
  }
  saveStateEl.className = `badge save-state ${cls}`;
  saveStateEl.style.fontWeight = kind === "failed" || kind === "pending" ? "700" : "";
  saveStateEl.dataset.state = kind;
  saveStateEl.textContent = label;
  saveStateEl.title = title;
  saveStateEl.setAttribute("aria-label", `${label} — ${title}`);
}

// While the server holds queued marks, poll until its queue is empty so the
// indicator turns "saved" only once they really reached the case DB.
var queueWatchTimer = null;
function watchServerQueue() {
  if (queueWatchTimer || !viewerIsServed) return;
  queueWatchTimer = setInterval(async () => {
    try {
      const res = await fetch("/api/status", { cache: "no-store" });
      const st = res.ok ? await res.json() : null;
      if (st && Number(st.pending_marks) === 0) {
        queuedOnServer = 0;
        clearInterval(queueWatchTimer);
        queueWatchTimer = null;
        renderSaveState();
      }
    } catch (e) { /* keep waiting; the indicator stays "queued" */ }
  }, 3000);
}

function scheduleAutosave(delay) {
  clearTimeout(autosaveTimer);
  autosaveTimer = setTimeout(() => { autosaveTimer = null; syncMarksNow(); }, delay);
}

function markReviewDirty() {
  pendingReviewSync = true;
  saveRevision += 1;
  if (viewerIsServed) {
    // While backing off after a failure keep the backoff timer: an edit
    // must not turn a busy server into a request-per-keystroke loop.
    if (!(saveFailures > 0 && autosaveTimer)) scheduleAutosave(AUTOSAVE_DEBOUNCE_MS);
  }
  renderSaveState();
}

function clearReviewDirty() {
  pendingReviewSync = false;
  state.deletedIds = [];
  state.drafts = {};
  storageSet(ANNOTATIONS_KEY, state.drafts);
}

async function pushMarksToCase(payload) {
  const res = await fetch("/api/import-marks", {
    method: "POST",
    headers: { "Content-Type": "text/plain" },
    body: JSON.stringify({ marks_json: JSON.stringify(payload) })
  });
  let data = null;
  try { data = await res.json(); } catch { data = null; }
  if (!data || typeof data !== "object") {
    return { ok: false, error: `HTTP ${res.status}` };
  }
  return data;
}

// One save attempt. Resolves { ok, error, offline, empty, queued }.
// A call made while a save is in flight is queued behind it, not dropped.
async function syncMarksNow() {
  if (!viewerIsServed) return { ok: false, offline: true, error: t("save.offlineErr") };
  if (saveInFlight) {
    saveQueued = true;
    return { ok: false, queued: true };
  }
  const payload = marksPayload();
  if (!payload.marks.length && !payload.tags.length && !payload.deleted_ids.length) {
    clearReviewDirty();
    saveFailures = 0;
    saveError = "";
    renderSaveState();
    return { ok: true, empty: true };
  }
  const sentRevision = saveRevision;
  saveInFlight = true;
  clearTimeout(autosaveTimer);
  autosaveTimer = null;
  renderSaveState();
  let result;
  try {
    result = await pushMarksToCase(payload);
  } catch {
    result = { ok: false, offline: true, error: t("save.offlineErr") };
  }
  saveInFlight = false;
  if (result.ok) {
    // queued: durably on the server but not in the case DB until the
    // enrichment step ends — shown as "저장 대기(보강 중)", never a failure.
    queuedOnServer = result.deferred ? Math.max(1, Number(result.pending) || 1) : 0;
    if (queuedOnServer) watchServerQueue();
    saveFailures = 0;
    saveError = "";
    saveRetryAt = 0;
    // Edits made while the request was in flight are not in this payload:
    // keep them pending and push again instead of clearing them.
    if (saveRevision === sentRevision) clearReviewDirty();
    else scheduleAutosave(AUTOSAVE_DEBOUNCE_MS);
  } else {
    const firstFailure = saveFailures === 0;
    saveFailures += 1;
    saveError = String(result.error || "?");
    const delay = autosaveRetryDelay(saveFailures);
    saveRetryAt = Date.now() + delay;
    scheduleAutosave(delay);
    if (firstFailure) toast(tf("toast.saveFailed", { err: saveError }));
  }
  if (manualQueued) {
    // A queued manual apply always pushes once more and reports, even if
    // this save already cleared pendingReviewSync.
    manualQueued = false;
    saveQueued = false;
    pendingReviewSync = true;
    renderSaveState();
    runManualApply();
    return result;
  }
  if (saveQueued) {
    saveQueued = false;
    if (pendingReviewSync && result.ok) scheduleAutosave(0);
  }
  renderSaveState();
  return result;
}

// The debounced auto-save entry point.
function autoApplyMarks() {
  if (!pendingReviewSync || !viewerIsServed) return Promise.resolve({ ok: true, empty: true });
  return syncMarksNow();
}

window.addEventListener("beforeunload", (event) => {
  if (!pendingReviewSync && !state.deletedIds.length && !saveInFlight) return;
  event.preventDefault();
  event.returnValue = "";
});

if (saveStateEl) {
  // Clicking the indicator saves (or retries) right away.
  saveStateEl.addEventListener("click", () => {
    if (!viewerIsServed) { toast(t("save.title.local")); return; }
    if (!pendingReviewSync && !saveInFlight) { toast(t("save.title.saved")); return; }
    syncMarksNow();
  });
}
// Back online / tab visible again: retry a failed save now instead of
// waiting out the backoff.
window.addEventListener("online", () => { if (viewerIsServed && pendingReviewSync) scheduleAutosave(0); });
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && viewerIsServed && pendingReviewSync && saveFailures > 0) scheduleAutosave(0);
});

// Server-served viewers can push marks straight into the case DB instead of
// the download → file-pick → import dance.
document.getElementById("btnApplyMarks").addEventListener("click", async () => {
  if (!viewerIsServed) { toast(t("offline.applyTitle")); return; }
  const payload = marksPayload();
  if (!payload.marks.length && !payload.tags.length && !payload.deleted_ids.length) { toast(t("toast.noChanges")); return; }
  // A manual apply always pushes, even if the auto-save already did, so the
  // examiner gets an explicit confirmation and a refreshed report.
  if (saveInFlight) {
    manualQueued = true;
    saveQueued = true;
    toast(t("save.queued"));
    return;
  }
  await runManualApply();
});

async function runManualApply() {
  pendingReviewSync = true;
  const result = await syncMarksNow();
  if (result.queued) { manualQueued = true; toast(t("save.queued")); }
  else if (result.ok && result.deferred) toast(t("save.queuedServer"));
  else if (result.ok) toast(t("toast.applyDone"));
  else if (result.offline) toast(t("toast.applyOffline"));
  else toast(tf("toast.applyFail", { err: result.error || "?" }));
  return result;
}

// --- 선별 결과 내보내기: 사람이 받는 형태 (CSV / 요약 리포트 / 자료 묶음) ---

document.getElementById("btnDownloadCsv").addEventListener("click", () => {
  const selected = selectedRecords();
  if (!selected.length) { toast(t("toast.selectFirst")); return; }
  const header = ["id", "kind", "name", "status", "mark", "examiner_note", "tags", "sha256", "size_bytes",
    "recorded_time", "channel", "rec_type", "inode", "original_path", "source_path",
    "warnings", "note", "anomalies"];
  const rows = selected.map(record => [
    record.id,
    KIND_LABELS[record.kind] || record.kind,
    record.name,
    record.status,
    state.marks[record.id]?.status || ((state.notes[record.id] || "").trim() ? "noted" : ""),
    (state.notes[record.id] || "").trim(),
    (state.tags[record.id] || []).join("; "),
    record.sha256,
    record.size ?? "",
    isoTime(record.recTime),
    record.channel || "",
    record.recType || "",
    record.inode ?? "",
    record.originalPath || "",
    record.path || "",
    (record.warnings || []).join("; "),
    record.note && record.note !== "-" ? record.note : "",
    (record.anomalies || []).map(item => item.kind).join("; ")
  ].map(csvCell).join(","));
  downloadText(
    `frametrace-selection-${manifest.case_id || "case"}.csv`,
    String.fromCharCode(0xFEFF) + header.join(",") + "\n" + rows.join("\n") + "\n",
    "text/csv;charset=utf-8"
  );
});

// A self-contained, printable handoff report: everything a requester
// needs to understand what was selected and why, without the viewer.
document.getElementById("btnSummary").addEventListener("click", () => {
  let items = selectedRecords();
  if (!items.length) {
    items = records.filter(record => state.marks[record.id] || (state.tags[record.id] || []).length || (state.notes[record.id] || "").trim());
  }
  if (!items.length) { toast(t("toast.noItems")); return; }
  const e = escapeHtml;
  const fmtSize = value => Number.isFinite(value) ? `${(value / 1048576).toFixed(1)} MB` : "-";
  const rowHtml = items.map(record => {
    const mark = state.marks[record.id]?.status;
    const memo = (state.notes[record.id] || "").trim();
    const warn = [...(record.warnings || []), ...(record.anomalies || []).map(item => item.kind)].join("; ");
    return `<tr><td>${e(record.id)}</td><td>${e(record.name)}</td><td>${e(KIND_LABELS[record.kind] || record.kind)}</td>` +
      `<td>${e(record.status)}</td><td>${e(mark ? markLabel(mark) : (memo ? t("mark.noted") : ""))}</td>` +
      `<td>${e((state.tags[record.id] || []).join(", "))}</td>` +
      `<td class="mono">${e(record.sha256 && record.sha256 !== "-" ? record.sha256 : "")}</td>` +
      `<td>${fmtSize(record.size)}</td><td>${e(isoTime(record.recTime))}</td>` +
      `<td>${e(warn)}${record.note && record.note !== "-" ? `<br>${e(record.note)}` : ""}${memo ? `<br><b>${t("report.memo")}</b> ${e(memo)}` : ""}</td>` +
      `<td class="mono">${e(record.originalPath || record.path || "")}</td></tr>`;
  }).join("\n");
  const counts = { verified: 0, candidate: 0, failed: 0 };
  items.forEach(record => {
    if (record.status === "validation-failed") counts.failed += 1;
    else if (record.status === "ffprobe-video-stream-confirmed") counts.verified += 1;
    else counts.candidate += 1;
  });
  const html = `<!doctype html><html lang="${state.locale}"><head><meta charset="utf-8">
<title>${t("report.title")} — ${e(manifest.case_id || "case")}</title>
<style>
body{font-family:ui-sans-serif,system-ui,"Segoe UI",sans-serif;margin:32px;color:#1f2724}
h1{font-size:20px} .meta{color:#68736f;font-size:13px;margin-bottom:4px}
.note{background:#fdf6e8;border:1px solid #e3cf9e;border-radius:6px;padding:10px 12px;font-size:13px;margin:14px 0}
table{border-collapse:collapse;width:100%;font-size:12px}
th,td{border:1px solid #d8dedb;padding:6px 8px;text-align:left;vertical-align:top}
th{background:#f2f5f4} .mono{font-family:Consolas,monospace;font-size:11px;word-break:break-all}
</style></head><body>
<h1>${t("report.title")}</h1>
<div class="meta">${tf("report.meta", { case: manifest.case_id || "-", when: new Date().toLocaleString(), n: items.length })}</div>
<div class="meta">${tf("report.counts", { v: counts.verified, c: counts.candidate, f: counts.failed })}</div>
<div class="note">${t("report.note")}</div>
<table><thead><tr><th>ID</th><th>${t("report.th.name")}</th><th>${t("report.th.kind")}</th><th>${t("report.th.status")}</th><th>${t("report.th.mark")}</th><th>${t("report.th.tags")}</th>
<th>SHA-256</th><th>${t("report.th.size")}</th><th>${t("report.th.time")}</th><th>${t("report.th.warn")}</th><th>${t("report.th.orig")}</th></tr></thead>
<tbody>
${rowHtml}
</tbody></table>
</body></html>`;
  downloadText(`frametrace-summary-${manifest.case_id || "case"}.html`, html, "text/html;charset=utf-8");
});

// 서버 측 자료 묶음 — 선택 파일을 exports/selection-*/ 에 해시 매니페스트와 함께 복사.
document.getElementById("btnExportSelected").addEventListener("click", async () => {
  const selected = selectedRecords();
  if (!selected.length) { toast(t("toast.selectFirst")); return; }
  const btn = document.getElementById("btnExportSelected");
  btn.disabled = true;
  try {
    const res = await fetch("/api/export-selected", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ items: selected.map(exportItem) })
    }).then(reply => reply.json());
    if (!res.ok) {
      toast(tf("toast.exportFail", { err: res.error || "" }));
      return;
    }
    toast(tf("toast.exportDone", { copied: res.copied, skipped: res.skipped, dir: res.export_dir }));
    fetch("/api/open-folder", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path: res.export_dir })
    }).catch(() => {});
  } catch (err) {
    toast(t("toast.offline"));
  } finally {
    btn.disabled = false;
  }
});

// --- 원본 파일 다운로드: /media?path&download=1이 Content-Disposition으로
// 저장을 유도. file:// 스탠드얼론 번들에서는 서버가 없으므로 원본/프록시의
// file:// 경로로 폴백한다 — 브라우저가 렌더할 수 없는 형식(AVI/.bin/DAV)은
// 네비게이션 대신 그대로 저장되고, 재생 가능한 파일은 새 탭에서 열린다. ---
function downloadHref(record) {
  if (location.protocol === "http:" || location.protocol === "https:") {
    if (!record.path) return "";
    return "/media?path=" + encodeURIComponent(record.path) + "&download=1";
  }
  if (record.fileUrl) return record.fileUrl;
  if (record.proxyPath) return fileUrl(record.proxyPath);
  return "";
}
function triggerDownload(record) {
  const href = downloadHref(record);
  if (!href) return false;
  const a = document.createElement("a");
  a.href = href;
  a.download = record.originalName || record.name || record.id;
  // Standalone file:// fallback navigates instead of downloading for
  // renderable types — keep the viewer alive by opening a new tab.
  if (location.protocol !== "http:" && location.protocol !== "https:") a.target = "_blank";
  document.body.appendChild(a);
  a.click();
  a.remove();
  return true;
}
// Folder-pick saves need a fetchable source: /media over http(s). file://
// pages cannot fetch file:// URLs (CORS), so standalone falls back to
// browser downloads below.
function canFolderSave() {
  return typeof window.showDirectoryPicker === "function" &&
    (location.protocol === "http:" || location.protocol === "https:");
}
function safeFileName(name) {
  return String(name || "evidence").replace(/[\\/:*?"<>|]/g, "_") || "evidence";
}
async function writeRecordTo(dir, record) {
  const href = downloadHref(record);
  if (!href) return false;
  const res = await fetch(href);
  if (!res.ok || !res.body) return false;
  const handle = await dir.getFileHandle(safeFileName(record.originalName || record.name || record.id), { create: true });
  const writable = await handle.createWritable();
  await res.body.pipeTo(writable);
  return true;
}
document.getElementById("btnDownloadFile").addEventListener("click", async () => {
  const record = selectedRecord();
  if (!record) { toast(t("toast.selectFirst")); return; }
  if (canFolderSave() && typeof window.showSaveFilePicker === "function") {
    try {
      const handle = await showSaveFilePicker({ suggestedName: safeFileName(record.originalName || record.name || record.id) });
      const href = downloadHref(record);
      if (!href) { toast(t("toast.fileOnly")); return; }
      const res = await fetch(href);
      if (!res.ok || !res.body) { toast(t("toast.fileOnly")); return; }
      const writable = await handle.createWritable();
      await res.body.pipeTo(writable);
      toast(tf("toast.fileSaved", { name: handle.name }));
    } catch (error) {
      if (error.name !== "AbortError") toast(tf("toast.folderFail", { err: error.message }));
    }
    return;
  }
  if (!triggerDownload(record)) {
    toast(t("toast.fileOnly"));
  }
});
document.getElementById("btnDownloadFiles").addEventListener("click", async () => {
  const selected = selectedRecords();
  if (!selected.length) { toast(t("toast.selectFirst")); return; }
  if (canFolderSave()) {
    try {
      const dir = await showDirectoryPicker({ mode: "readwrite" });
      let saved = 0, failed = 0;
      for (const record of selected) {
        try {
          if (await writeRecordTo(dir, record)) saved += 1;
          else failed += 1;
        } catch {
          failed += 1;
        }
      }
      toast(failed ? tf("toast.folderPartial", { n: saved, f: failed }) : tf("toast.folderSaved", { n: saved }));
    } catch (error) {
      if (error.name !== "AbortError") toast(tf("toast.folderFail", { err: error.message }));
    }
    return;
  }
  let started = 0;
  selected.forEach((record, index) => {
    if (!downloadHref(record)) return;
    started += 1;
    setTimeout(() => triggerDownload(record), index * 450);
  });
  if (!started) {
    toast(t("toast.fileOnly"));
  } else {
    toast(tf("toast.downloads", { n: started }));
  }
});
document.getElementById("btnTranscodeQueue").addEventListener("click", async (ev) => {
  const btn = ev.currentTarget;
  const ids = selectedRecords().map(r => r.id).join(",");
  btn.disabled = true;
  toast(t("toast.queueRunning"));
  try {
    const res = await fetch("/api/transcode-queue", {
      method: "POST",
      headers: { "Content-Type": "text/plain" },
      body: JSON.stringify({ ids })
    });
    const data = await res.json();
    if (data.ok && data.summary) {
      toast(tf("toast.queueDone", { t: data.summary.total ?? 0, p: data.summary.proxied ?? 0, c: data.summary.skipped_existing ?? 0, f: data.summary.failed ?? 0 }));
    } else {
      toast(tf("toast.queueFail", { err: data.error || "" }));
    }
  } catch {
    toast(t("toast.queueOffline"));
  } finally {
    btn.disabled = false;
  }
});

// --- 케이스 타임라인 패널: db/timeline.jsonl을 읽어 시간순 이벤트를 표시 ---
async function loadTimeline(regenerate) {
  const list = document.getElementById("timelineList");
  const meta = document.getElementById("timelineMeta");
  if (location.protocol === "file:") {
    list.innerHTML = `<div class="timeline-row"><span class="desc">${t("timeline.offline")}</span></div>`;
    return;
  }
  if (regenerate) {
    meta.textContent = t("timeline.building");
    try {
      const res = await fetch("/api/advanced", {
        method: "POST", headers: { "Content-Type": "text/plain" },
        body: JSON.stringify({ tool: "timeline" })
      });
      const data = await res.json();
      if (!data.ok) { meta.textContent = tf("timeline.fail", { err: data.error || "" }); return; }
    } catch { meta.textContent = t("timeline.failOffline"); return; }
  }
  try {
    const res = await fetch("/case/db/timeline.jsonl");
    if (!res.ok) {
      meta.textContent = t("timeline.none");
      list.innerHTML = "";
      return;
    }
    const text = await res.text();
    const events = text.split("\n").filter(l => l.trim()).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
    const byPath = new Map(records.map(r => [r.path, r.id]));
    meta.textContent = tf("timeline.count", { n: events.length });
    const cap = 400;
    list.innerHTML = events.slice(0, cap).map(ev => {
      const recId = byPath.get(ev.path);
      return `<div class="timeline-row" data-id="${escapeHtml(recId || "")}">
        <span class="ts">${escapeHtml(isoTime(ev.ts_unix))}</span>
        <span class="src">${escapeHtml(ev.source || "")}</span>
        <span class="desc" title="${escapeHtml(ev.path || "")} ${escapeHtml(ev.detail || "")}">${escapeHtml(ev.kind || "")} — ${escapeHtml((ev.path || "").split(/[\\/]/).pop() || ev.path || "")}</span>
      </div>`;
    }).join("") + (events.length > cap ? `<div class="timeline-row"><span class="desc muted">${tf("timeline.ellipsis", { n: events.length - cap })}</span></div>` : "");
    list.querySelectorAll(".timeline-row[data-id]").forEach(row => {
      row.addEventListener("click", () => {
        if (!row.dataset.id) return;
        state.selectedIds.clear();
        state.selectedIds.add(row.dataset.id);
        state.activeId = row.dataset.id;
        render();
      });
    });
  } catch {
    meta.textContent = t("timeline.unreadable");
  }
}
document.getElementById("btnTimeline").addEventListener("click", () => {
  const panel = document.getElementById("timelinePanel");
  panel.hidden = !panel.hidden;
  if (!panel.hidden) loadTimeline(false);
});
document.getElementById("btnTimelineClose").addEventListener("click", () => {
  document.getElementById("timelinePanel").hidden = true;
});
document.getElementById("btnTimelineGen").addEventListener("click", () => loadTimeline(true));

// The viewer also runs standalone via file:// — server-backed features need
// the workstation server, and marks/tags live in a different localStorage
// origin than the served viewer, so make those limits visible up front
// (UX-13). Every server-only control is disabled with an explanatory
// tooltip, so nothing prompts for input (e.g. the burn-in exhibit label)
// only to fail afterwards. The tooltip goes through data-i18n-title so
// applyChromeI18n() keeps it — and localizes it — on every render.
const SERVER_ONLY_CONTROLS = [
  ["btnExportSelected", "offline.openServer"],
  ["btnApplyMarks", "offline.applyTitle"],
  ["btnCaptureFrame", "offline.serverOnly"],
  ["btnExportClip", "offline.serverOnly"],
  ["btnExportClipBurn", "offline.serverOnly"],
  ["btnProxy", "offline.serverOnly"],
  ["btnTelemetry", "offline.serverOnly"],
  ["btnTranscodeQueue", "offline.serverOnly"],
  ["btnTimeline", "offline.serverOnly"],
  ["btnTimelineGen", "offline.serverOnly"],
];
function disableServerOnlyControls() {
  for (const [id, key] of SERVER_ONLY_CONTROLS) {
    const btn = document.getElementById(id);
    if (!btn) continue;
    btn.disabled = true;
    btn.setAttribute("aria-disabled", "true");
    btn.classList.add("server-only");
    btn.dataset.i18nTitle = key;
    btn.title = t(key);
  }
}
if (!viewerIsServed) {
  disableServerOnlyControls();
  toast(t("toast.standalone"));
} else if (pendingReviewSync) {
  // Drafts replayed from localStorage (a reload or crash before the last
  // save landed) are pushed without waiting for the next edit.
  scheduleAutosave(1500);
}
renderSaveState();

state.pageSize = Number(els.pageSize.value) || 100;


// --- Range divergence check on boot ---------------------------------------
function rangeText(range) {
  const r = normRange(range);
  const p = v => Number.isFinite(v) ? `${v.toFixed(1)}s` : "—";
  return r ? `${p(r.in)} ~ ${p(r.out)}` : t("range.none");
}

function serverRanges() {
  const map = {};
  for (const mark of (DATA.annotations && DATA.annotations.marks) || []) {
    if (rangeValid(mark.range)) map[mark.id] = normRange(mark.range);
  }
  return map;
}

function detectRangeDivergence() {
  if (!viewerIsServed) return [];
  const server = serverRanges();
  const pending = new Set(state.deletedIds);
  const ids = new Set([...Object.keys(server), ...Object.keys(state.ranges)]);
  const diverged = [];
  for (const id of ids) {
    if (pending.has(id)) continue; // unsynced local edit: will be pushed, not a conflict
    if (!sameRange(server[id], state.ranges[id])) diverged.push(id);
  }
  return diverged;
}

function resolveRangeDivergence(ids, side) {
  const server = serverRanges();
  for (const id of ids) {
    if (side === "server") {
      if (server[id]) state.ranges[id] = { ...server[id] }; else delete state.ranges[id];
    }
  }
  storageSet(RANGES_KEY, state.ranges);
  ids.forEach(id => rangeConflictIds.delete(id));
  if (side === "browser") {
    // Push the browser's version (also clears server ranges the browser lacks).
    ids.forEach(id => touchAnnotation(id));
  }
  if (typeof updateRangeLabel === "function") updateRangeLabel();
}

function showRangeDivergenceBanner(ids) {
  const server = serverRanges();
  const bar = document.createElement("div");
  bar.id = "rangeDivergenceBanner";
  bar.setAttribute("role", "alert");
  bar.style.cssText = "position:sticky;top:0;z-index:50;display:flex;flex-wrap:wrap;gap:8px;align-items:center;padding:8px 12px;background:#fff3cd;color:#4a3500;border-bottom:2px solid #d9a400;font-size:13px";
  const msg = document.createElement("span");
  msg.style.fontWeight = "700";
  msg.textContent = tf("range.divergeMsg", { n: ids.length });
  msg.title = ids.slice(0, 20).map(id => `${id}: ${t("range.server")} ${rangeText(server[id])} / ${t("range.browser")} ${rangeText(state.ranges[id])}`).join("\n");
  bar.appendChild(msg);
  const mk = (id, label, side) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "mini";
    b.id = id;
    b.textContent = label;
    b.addEventListener("click", () => { resolveRangeDivergence(ids, side); bar.remove(); });
    bar.appendChild(b);
  };
  mk("rangeUseServer", t("range.useServer"), "server");
  mk("rangeUseBrowser", t("range.useBrowser"), "browser");
  document.body.insertBefore(bar, document.body.firstChild);
}

(function initRangeDivergence() {
  const ids = detectRangeDivergence();
  if (!ids.length) return;
  ids.forEach(id => rangeConflictIds.add(id));
  showRangeDivergenceBanner(ids);
})();
