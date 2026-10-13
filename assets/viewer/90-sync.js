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

// Save protocol v2 (docs/FIX_PLAN_R1_2026-10-13.md §1.4/§1.6). The payload
// carries ONLY the dirty record ids — each with its whole annotation
// (mark, note, examiner, range, tags) and the revision this window last
// saw (`base_revisions`), so the server applies per id and reports the
// ids whose revision moved on instead of overwriting them. An id with no
// mark, note or range goes to `deleted_ids`; its tags (possibly empty)
// ride in `tags` so "all tags removed" reaches the server explicitly.
// Self-contained on purpose (the review-batch VM harness evaluates this
// function alone): ranges are normalised and conflict ids read locally.
//   opts.all   every annotated id (the marks download), not just dirty ones
function marksPayload(opts) {
  const all = !!(opts && opts.all);
  const rangeConflicts = typeof rangeConflictIds === "undefined" ? new Set() : rangeConflictIds;
  const rangeOfId = id => {
    const r = state.ranges && state.ranges[id];
    return r && (Number.isFinite(r.in) || Number.isFinite(r.out))
      ? { in: Number.isFinite(r.in) ? r.in : null, out: Number.isFinite(r.out) ? r.out : null }
      : null;
  };
  const ids = new Set(state.dirtyIds || []);
  if (all) {
    Object.keys(state.marks).forEach(id => ids.add(id));
    Object.keys(state.notes).forEach(id => { if ((state.notes[id] || "").trim()) ids.add(id); });
    Object.keys(state.ranges || {}).forEach(id => { if (rangeOfId(id) && !rangeConflicts.has(id)) ids.add(id); });
    Object.keys(state.tags || {}).forEach(id => { if ((state.tags[id] || []).length) ids.add(id); });
  }
  const marks = [];
  const tags = [];
  const deleted = [];
  const baseRevisions = {};
  for (const id of ids) {
    const mark = state.marks[id];
    const note = (state.notes[id] || "").trim();
    const range = rangeConflicts.has(id) ? null : rangeOfId(id);
    const tagList = Array.isArray(state.tags[id]) ? state.tags[id] : [];
    // The base is the revision the unsaved edit was made on (the draft's,
    // also for a draft adopted from a dead window), else the last one seen.
    const draft = state.drafts && state.drafts[id];
    baseRevisions[id] = draft && Number.isFinite(draft.base) ? draft.base
      : (Number.isFinite(state.revisions && state.revisions[id]) ? state.revisions[id] : 0);
    if (!mark && !note && !range) {
      deleted.push(id);
      tags.push({ id, tags: tagList });
      continue;
    }
    // Marks and free-text notes travel together: a record with only a note
    // (or only a range) is exported with status "noted" so the case DB
    // keeps the memo.
    const entry = {
      id,
      status: mark ? mark.status : "noted",
      marked_unix: mark ? mark.marked_unix : Math.floor(Date.now() / 1000),
      note,
      // An id this window hydrated or touched carries its own examiner
      // (null included — "[서버 값 사용]" must re-send the server's null, and
      // the server keeps its examiner on null); only an unknown id falls
      // back to the current examiner name.
      examiner: Object.prototype.hasOwnProperty.call(state.examiners, id) ? state.examiners[id] : ((state.examiner || "").trim() || null),
      tags: tagList
    };
    if (!rangeConflicts.has(id)) entry.range = range || { in: null, out: null };
    marks.push(entry);
  }
  return {
    schema_version: 2,
    protocol: "patch-v2",
    case_id: manifest.case_id || null,
    examiner: (state.examiner || "").trim() || null,
    client_id: (typeof FTViewer !== "undefined" && FTViewer.writerToken) || null,
    exported_unix: Math.floor(Date.now() / 1000),
    base_revisions: baseRevisions,
    marks,
    tags,
    deleted_ids: deleted
  };
}

// The server stamps a mark sent with `examiner: null` with the body's
// top-level examiner. Marks that must keep a null examiner (an id taken
// over with "[서버 값 사용]" whose server row has none) therefore travel in
// a body of their own with a null top-level examiner; everything else
// keeps the current examiner as the acting one. Returns 1 or 2 payloads.
function splitByExaminer(payload) {
  if (!payload.examiner) return [payload];
  const bare = new Set(payload.marks.filter(m => m.examiner === null).map(m => m.id));
  if (!bare.size) return [payload];
  const pick = keep => {
    const base = {};
    const marks = payload.marks.filter(m => bare.has(m.id) === keep);
    for (const m of marks) if (Object.prototype.hasOwnProperty.call(payload.base_revisions, m.id)) base[m.id] = payload.base_revisions[m.id];
    return { marks, base };
  };
  const named = pick(false);
  const nameless = pick(true);
  const ids = new Set(named.marks.map(m => m.id));
  for (const t of payload.tags) if (!bare.has(t.id)) ids.add(t.id);
  for (const id of payload.deleted_ids) ids.add(id);
  for (const id of ids) if (Object.prototype.hasOwnProperty.call(payload.base_revisions, id)) named.base[id] = payload.base_revisions[id];
  const out = [];
  const first = { ...payload, base_revisions: named.base, marks: named.marks, tags: payload.tags.filter(t => !bare.has(t.id)) };
  if (!payloadEmpty(first)) out.push(first);
  out.push({ ...payload, examiner: null, base_revisions: nameless.base, marks: nameless.marks, tags: payload.tags.filter(t => bare.has(t.id)), deleted_ids: [] });
  return out;
}

function payloadIds(payload) {
  return [...new Set([...payload.marks.map(m => m.id), ...payload.deleted_ids])];
}
function payloadEmpty(payload) {
  return !payload.marks.length && !payload.tags.length && !payload.deleted_ids.length;
}

// Splits a payload into sequential chunks of at most `maxIds` records and
// roughly `maxBytes` of JSON each (the server caps a body at 4 MiB). Every
// chunk is a complete v2 body of its own and carries batch {id, index,
// total}; the server runs its post-save work once, on the last one.
const PAYLOAD_CHUNK_IDS = 2000;
const PAYLOAD_CHUNK_BYTES = 1024 * 1024;
const PAYLOAD_CHUNK_MIN_IDS = 50;
function splitPayloadChunks(payload, opts) {
  const maxIds = Math.max(1, (opts && opts.maxIds) || PAYLOAD_CHUNK_IDS);
  const maxBytes = Math.max(1024, (opts && opts.maxBytes) || PAYLOAD_CHUNK_BYTES);
  const batchId = (opts && opts.batchId) || ("b-" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 8));
  const marksById = new Map(payload.marks.map(m => [m.id, m]));
  const tagsById = new Map((payload.tags || []).map(t => [t.id, t]));
  const deleted = new Set(payload.deleted_ids || []);
  const ids = payloadIds(payload);
  const header = { ...payload, marks: [], tags: [], deleted_ids: [], base_revisions: {} };
  const headerBytes = JSON.stringify(header).length + 64;
  const groups = [];
  let current = null;
  let bytes = 0;
  const sizeOf = id => {
    let n = 0;
    if (marksById.has(id)) n += JSON.stringify(marksById.get(id)).length + 1;
    if (tagsById.has(id)) n += JSON.stringify(tagsById.get(id)).length + 1;
    if (deleted.has(id)) n += id.length + 3;
    return n + id.length + 16; // base_revisions entry
  };
  for (const id of ids) {
    const size = sizeOf(id);
    if (!current || current.length >= maxIds || (current.length && bytes + size > maxBytes - headerBytes)) {
      current = [];
      groups.push(current);
      bytes = 0;
    }
    current.push(id);
    bytes += size;
  }
  return groups.map((group, index) => {
    const body = { ...header, batch: { id: batchId, index, total: groups.length }, base_revisions: {}, marks: [], tags: [], deleted_ids: [] };
    for (const id of group) {
      if (payload.base_revisions && Object.prototype.hasOwnProperty.call(payload.base_revisions, id)) body.base_revisions[id] = payload.base_revisions[id];
      if (marksById.has(id)) body.marks.push(marksById.get(id));
      if (tagsById.has(id)) body.tags.push(tagsById.get(id));
      if (deleted.has(id)) body.deleted_ids.push(id);
    }
    return body;
  });
}

// Sends every chunk in order through `post(body) -> response`. A 413
// halves the chunk size (down to PAYLOAD_CHUNK_MIN_IDS) and restarts a
// fresh batch from the failed chunk's ids; any other failure stops the
// run. Resolves the merged outcome:
//   { ok, error, status, applied: {id: revision}, conflicts: [...],
//     queuedIds: [id...], deferred, pending, posts, maxIds }
// A legacy reply (no `applied` map, no v2 protocol) counts every id of that
// chunk as applied at its sent revision; a deferred reply lists them in
// `queuedIds` (the server applies them when its job ends — the drain, not this
// reply, decides their revision).
// The same chunk as a patch-v1 body for a workstation that predates v2
// ("unsupported marks protocol"): dirty ids only, tags listed in the v1
// `tags` array (an empty list removes them), no revision check (that
// server has none — last write wins, as before).
function legacyPayloadBody(chunk) {
  return {
    schema_version: 2,
    protocol: "patch-v1",
    case_id: chunk.case_id,
    examiner: chunk.examiner,
    exported_unix: chunk.exported_unix,
    marks: chunk.marks.map(m => { const { tags, ...rest } = m; void tags; return rest; }),
    tags: [...chunk.tags, ...chunk.marks.map(m => ({ id: m.id, tags: m.tags || [] }))],
    deleted_ids: chunk.deleted_ids
  };
}

async function sendPayloadChunks(payload, post, opts) {
  const out = { ok: true, error: "", status: 0, applied: {}, conflicts: [], queuedIds: [], deferred: false, pending: 0, posts: 0, maxIds: (opts && opts.maxIds) || PAYLOAD_CHUNK_IDS, legacy: !!(opts && opts.legacy) };
  const minIds = (opts && opts.minIds) || PAYLOAD_CHUNK_MIN_IDS;
  let chunks = splitPayloadChunks(payload, { maxIds: out.maxIds, maxBytes: opts && opts.maxBytes });
  let at = 0;
  while (at < chunks.length) {
    const chunk = chunks[at];
    let res;
    try { res = await post(out.legacy ? legacyPayloadBody(chunk) : chunk); } catch (error) { res = { ok: false, offline: true, error: (error && error.message) || String(error) }; }
    out.posts += 1;
    if (!res || typeof res !== "object") res = { ok: false, error: "bad response" };
    if (!res.ok) {
      // Exactly the pre-v2 parser's refusal: any other error that mentions
      // a protocol must not downgrade the session to last-write-wins.
      if (!out.legacy && String(res.error || "") === "unsupported marks protocol") {
        out.legacy = true; // pre-v2 workstation: resend this chunk as patch-v1
        continue;
      }
      if (Number(res.status) === 413 && out.maxIds > minIds) {
        out.maxIds = Math.max(minIds, Math.floor(out.maxIds / 2));
        const rest = { ...payload, marks: [], tags: [], deleted_ids: [], base_revisions: {} };
        const remaining = new Set(chunks.slice(at).flatMap(payloadIds));
        rest.marks = payload.marks.filter(m => remaining.has(m.id));
        rest.tags = (payload.tags || []).filter(t => remaining.has(t.id));
        rest.deleted_ids = (payload.deleted_ids || []).filter(id => remaining.has(id));
        for (const id of remaining) if (payload.base_revisions && id in payload.base_revisions) rest.base_revisions[id] = payload.base_revisions[id];
        chunks = splitPayloadChunks(rest, { maxIds: out.maxIds, maxBytes: opts && opts.maxBytes });
        at = 0;
        continue;
      }
      out.ok = false;
      out.error = String(res.error || (res.status ? `HTTP ${res.status}` : "?"));
      out.status = Number(res.status) || 0;
      out.offline = !!res.offline;
      return out;
    }
    const ids = payloadIds(chunk);
    if (res.deferred) {
      out.deferred = true;
      out.pending = Math.max(out.pending, Number(res.pending) || 1);
      out.queuedIds.push(...ids);
    } else if (res.applied && typeof res.applied === "object" && !Array.isArray(res.applied)) {
      for (const [id, rev] of Object.entries(res.applied)) out.applied[id] = Number(rev);
      for (const c of Array.isArray(res.conflicts) ? res.conflicts : []) if (c && c.id) out.conflicts.push(c);
    } else {
      // Legacy server (patch-v1 semantics): everything it accepted is in.
      for (const id of ids) out.applied[id] = chunk.base_revisions[id] ?? 0;
    }
    at += 1;
  }
  return out;
}

document.getElementById("btnDownloadMarks").addEventListener("click", () => {
  const payload = marksPayload({ all: true });
  if (payloadEmpty(payload)) { toast(t("toast.noChanges")); return; }
  downloadJSON(`frametrace-marks-${manifest.case_id || "case"}.json`, payload);
});

// --- Review-change protection + save state (FOR-01) ----------------------
// touchAnnotation() funnels every mark/note/tag/range mutation into
// state.dirtyIds — the records changed since the server last confirmed
// them. A served viewer pushes those changes to the case DB on its own:
// debounced after edits, in id-bounded chunks, retried with exponential
// backoff after a failure (busy lock, server restart, network), and never
// dropped — the drafts stay in localStorage until the server confirms each
// id. Ids the server refused because another window saved them first are
// held in conflictIds and shown in #markConflictBanner (+ the per-record
// #markConflictPanel) until the examiner picks a side. The #saveState
// indicator always shows which of saved / saving / N pending / FAILED
// applies, so an examiner can never believe unsaved work reached the case.
// `var` (not let): render() calls renderSaveState(), and these must read
// as undefined rather than throw if a render ever runs before this file.
var pendingReviewSync = state.dirtyIds.size > 0;
var autosaveTimer = null;
var saveInFlight = false;   // one import at a time
var saveQueued = false;     // a save was requested while one was in flight
var manualQueued = false;   // "Apply to case" was pressed while one was in flight
var saveRevision = 0;       // bumps on every change; detects edits mid-flight
var saveFailures = 0;
var saveError = "";
var queuedOnServer = 0;     // marks the server holds in its phase-2 queue (durable, not yet in the DB)
var queuedIds = new Set();  // ids this window sent into that queue (reconciled after the drain)
var conflictChecks = 0;     // server conflict lookups in flight: "saved" waits for them
var saveRetryAt = 0;
var saveChunkIds = PAYLOAD_CHUNK_IDS; // shrinks after a 413, for the rest of the session
var saveLegacyProtocol = false;       // the workstation refused patch-v2 once: keep sending patch-v1
var viewerIsServed = location.protocol === "http:" || location.protocol === "https:";
var saveStateEl = document.getElementById("saveState");
// id -> { server, server_revision, mine, queued } for ids the server
// refused (another window saved a newer revision, or a queued save met a
// newer one at drain time). Not dirty, not saved: pending until resolved.
var conflictIds = new Map();
var saveSettleWaiters = [];
const AUTOSAVE_DEBOUNCE_MS = 2500;
const AUTOSAVE_RETRY_BASE_MS = 2000;
const AUTOSAVE_RETRY_MAX_MS = 60000;

function autosaveRetryDelay(failures) {
  return Math.min(AUTOSAVE_RETRY_MAX_MS, AUTOSAVE_RETRY_BASE_MS * Math.pow(2, Math.max(0, failures - 1)));
}

function pendingSaveCount() {
  return state.dirtyIds.size + conflictIds.size || (pendingReviewSync ? 1 : 0);
}
function reviewIdle() {
  return !pendingReviewSync && !state.dirtyIds.size && !conflictIds.size && conflictChecks === 0;
}
// Resolves once no save request is in flight (host unmount, FTViewer.save).
function whenSaveSettled() {
  if (!saveInFlight) return Promise.resolve(true);
  return new Promise(resolve => saveSettleWaiters.push(resolve)).then(() => whenSaveSettled());
}
function notifySaveSettled() {
  const waiters = saveSettleWaiters;
  saveSettleWaiters = [];
  waiters.forEach(resolve => resolve());
}
// Resolves once the server-conflict lookups (boot, queue drain) finished.
async function whenConflictsChecked() {
  for (let i = 0; conflictChecks > 0 && i < 600; i++) await new Promise(resolve => setTimeout(resolve, 50));
  return conflictChecks === 0;
}

function renderSaveState() {
  if (!saveStateEl) return;
  const n = pendingSaveCount();
  const unsaved = pendingReviewSync || state.dirtyIds.size > 0 || conflictIds.size > 0;
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
  } else if (saveFailures > 0 && unsaved) {
    kind = "failed";
    cls = "failed";
    label = tf("save.failed", { n });
    const sec = Math.max(0, Math.ceil((saveRetryAt - Date.now()) / 1000));
    title = tf("save.title.failed", { err: saveError || "?", sec });
  } else if (unsaved) {
    kind = "pending";
    cls = "candidate";
    label = tf("save.pending", { n });
    title = conflictIds.size && !state.dirtyIds.size
      ? tf("save.title.conflict", { n: conflictIds.size })
      : tf("save.title.pending", { n });
    if (conflictIds.size && state.dirtyIds.size) title += " " + tf("save.title.conflict", { n: conflictIds.size });
  } else if (queuedOnServer > 0) {
    kind = "queued";
    cls = "candidate";
    label = t("save.queuedServer");
    title = tf("save.title.queuedServer", { n: queuedOnServer });
  } else if (conflictChecks > 0) {
    // "saved" is never claimed before the server's conflict journal was
    // read (a queued save may have met a newer revision at drain time).
    kind = "saving";
    cls = "candidate";
    label = t("save.saving");
    title = t("save.title.checking");
  } else {
    kind = "saved";
    cls = "ok";
    label = t("save.saved");
    title = t("save.title.saved");
  }
  if (draftsUnpersisted) title += " " + t("save.title.quota");
  saveStateEl.className = `quiet save-ind ${cls}`;
  saveStateEl.style.fontWeight = kind === "failed" || kind === "pending" ? "700" : "";
  saveStateEl.dataset.state = kind;
  saveStateEl.innerHTML = `${ftIcon(kind === "failed" ? "warning" : kind === "saved" ? "check" : kind === "saving" ? "spinner" : "save")}<span class="save-label"></span>`;
  saveStateEl.lastElementChild.textContent = label;
  saveStateEl.title = title;
  saveStateEl.setAttribute("aria-label", `${label} — ${title}`);
  // Host notification (FTViewer "save-state"): only on an actual change.
  const signature = `${kind}|${n}|${queuedOnServer}|${conflictIds.size}`;
  if (signature !== lastSaveSignature) {
    lastSaveSignature = signature;
    FTViewer._emit("save-state", { state: kind, pending: n, queued: queuedOnServer, conflicts: conflictIds.size });
  }
  emitLiveState();
}
var lastSaveSignature = "";

// Host notification (FTViewer "live-state", contract §3.2): which index
// generation this mount loaded and whether a reload would be accepted now.
// In the workstation the shell owns the live-index / enrichment status (its
// /api/status poll is the single source of truth) and offers '새 데이터
// 반영' in its jobs sheet; the viewer never polls or shows a chip there.
var lastLiveSignature = "";
function liveStateDetail() {
  const live = DATA.liveIndex || null;
  return {
    live: !!live,
    generation: live && live.generation != null ? live.generation : null,
    total: records.length,
    canReload: !(pendingReviewSync || saveInFlight || state.dirtyIds.size || conflictIds.size || queuedOnServer > 0),
  };
}
function emitLiveState() {
  const detail = liveStateDetail();
  const signature = JSON.stringify(detail);
  if (signature === lastLiveSignature) return;
  lastLiveSignature = signature;
  FTViewer._emit("live-state", detail);
}
function emitDirty() {
  FTViewer._emit("dirty", { count: pendingSaveCount() });
}

// --- Conflicts (another window saved a newer revision) --------------------
// Annotation value shape (contract §1.4): { status, marked_unix, examiner,
// note, range, tags }, or null when the server holds nothing for the id.
function localAnnotationOf(id) {
  return {
    status: state.marks[id] ? state.marks[id].status : null,
    marked_unix: state.marks[id] ? state.marks[id].marked_unix : null,
    examiner: state.examiners[id] ?? null,
    note: state.notes[id] || "",
    range: normRange(state.ranges[id]),
    tags: [...(state.tags[id] || [])]
  };
}
function normServerAnnotation(s) {
  if (!s || typeof s !== "object") return null;
  return {
    status: s.status || null,
    marked_unix: Number.isFinite(s.marked_unix) ? s.marked_unix : null,
    examiner: s.examiner || null,
    note: s.note || "",
    range: normRange(s.range),
    tags: Array.isArray(s.tags) ? [...s.tags] : []
  };
}
// The value a queued save submitted (GET /api/mark-conflicts `queued`):
// the drain journal's { mark, tags, deleted, base_revision } or a bare
// marks entry. Tags the body did not carry (null) were left as they were.
function queuedAnnotation(q, server) {
  if (!q || typeof q !== "object") return null;
  const mark = q.mark && typeof q.mark === "object" ? q.mark : (Object.prototype.hasOwnProperty.call(q, "status") ? q : null);
  const tags = Array.isArray(q.tags) ? q.tags : (mark && Array.isArray(mark.tags) ? mark.tags : ((server && server.tags) || []));
  if (q.deleted || !mark) return { status: null, marked_unix: null, examiner: null, note: "", range: null, tags: [...tags] };
  return {
    status: mark.status || null,
    marked_unix: Number.isFinite(mark.marked_unix) ? mark.marked_unix : null,
    examiner: mark.examiner || null,
    note: mark.note || "",
    range: normRange(mark.range),
    tags: [...tags]
  };
}
// id -> server annotation value, from an `annotations` object
// (records-meta / the bundle): marks rows + tag lists.
function serverAnnotationMap(ann) {
  const byId = new Map();
  for (const mark of (ann && ann.marks) || []) {
    const id = annotationMarkId(mark);
    if (id) byId.set(id, { status: mark.status || null, marked_unix: Number.isFinite(mark.marked_unix) ? mark.marked_unix : null, examiner: mark.examiner || null, note: mark.note || "", range: normRange(mark.range), tags: [] });
  }
  for (const entry of (ann && ann.tags) || []) {
    if (!entry || !entry.id) continue;
    const cur = byId.get(entry.id) || { status: null, marked_unix: null, examiner: null, note: "", range: null, tags: [] };
    cur.tags = Array.isArray(entry.tags) ? [...entry.tags] : [];
    byId.set(entry.id, cur);
  }
  return byId;
}
function annotationText(a) {
  if (!a) return t("conflict.none");
  const parts = [];
  if (a.status) parts.push(typeof markLabel === "function" ? markLabel(a.status) : a.status);
  if ((a.note || "").trim()) parts.push(`"${String(a.note).trim().slice(0, 60)}"`);
  if (rangeValid(a.range)) parts.push(rangeText(a.range));
  if ((a.tags || []).length) parts.push((a.tags || []).join(", "));
  return parts.length ? parts.join(" · ") : t("conflict.none");
}
// Writes an annotation value into the in-memory maps (no draft, no save).
function applyAnnotationToState(id, a) {
  if (a && a.status && a.status !== "noted") state.marks[id] = { status: a.status, marked_unix: Number.isFinite(a.marked_unix) ? a.marked_unix : Math.floor(Date.now() / 1000) };
  else delete state.marks[id];
  state.notes[id] = (a && a.note) || "";
  state.examiners[id] = (a && a.examiner) || null;
  if (a && rangeValid(a.range)) state.ranges[id] = normRange(a.range); else delete state.ranges[id];
  draftRangesDirty = true;
  if (a && Array.isArray(a.tags) && a.tags.length) state.tags[id] = [...a.tags]; else delete state.tags[id];
}
// The conflict rides in the id's draft, so a reload restores the banner.
function persistConflict(id, entry) {
  const prev = state.drafts[id];
  const known = Number.isFinite(state.revisions[id]) ? state.revisions[id] : 0;
  const draft = prev ? { ...prev } : { ...draftValuesOf(id), seq: nextDraftSeq(), base: known };
  draft.conflict = { server: entry.server, server_revision: entry.server_revision, queued: !!entry.queued };
  if (entry.queued) draft.conflict.mine = entry.mine;
  state.drafts[id] = draft;
  draftFlushKeys.add(id);
  scheduleDraftFlush();
}
//   list   [{ id, server_revision, server, mine? }]
//   opts.queued   the conflicts come from the server's queue journal
function registerConflicts(list, opts) {
  const queued = !!(opts && opts.queued);
  let added = 0;
  for (const c of list || []) {
    if (!c || !c.id) continue;
    const id = c.id;
    const prev = conflictIds.get(id);
    const entry = {
      server: normServerAnnotation(c.server),
      server_revision: Number(c.server_revision) || 0,
      mine: c.mine || (prev ? prev.mine : localAnnotationOf(id)),
      queued: queued || !!(prev && prev.queued)
    };
    state.dirtyIds.delete(id);
    conflictIds.set(id, entry);
    persistConflict(id, entry);
    added += 1;
  }
  if (added) renderConflictBanner();
  return added;
}
// Drafts adopted with a stored conflict (reload, dead-window adoption):
// back into conflictIds, with the server side refreshed from the data
// this mount loaded when it knows the id's revision.
function registerRestoredConflicts() {
  const ids = restoredConflictIds.splice(0);
  if (!ids.length) return 0;
  const fresh = serverAnnotationMap(DATA.annotations || {});
  const revs = (DATA.annotations && DATA.annotations.revisions) || {};
  const list = [];
  for (const id of ids) {
    const draft = state.drafts[id];
    const c = draft && draft.conflict;
    if (!c) continue;
    const known = Number(revs[id]);
    const useFresh = Number.isFinite(known) && known > 0;
    list.push({
      id,
      server_revision: useFresh ? known : c.server_revision,
      server: useFresh ? (fresh.get(id) || null) : c.server,
      mine: c.queued && c.mine ? c.mine : localAnnotationOf(id),
      queued: !!c.queued
    });
  }
  const plain = list.filter(c => !c.queued);
  const fromQueue = list.filter(c => c.queued);
  return registerConflicts(plain) + registerConflicts(fromQueue, { queued: true });
}
// An edit to a conflicted id keeps the conflict; "mine" follows the edit.
function conflictEdited(id) {
  const c = conflictIds.get(id);
  if (!c) return;
  c.mine = localAnnotationOf(id);
  persistConflict(id, c);
  renderConflictBanner();
}
// Resolves conflicted ids. Either side is re-saved with base_revision =
// the server's revision: "server" sends the server's own value back (a
// no-op apply that also clears a queue-journal conflict), "mine" pushes
// this window's value (for a queue conflict: the value that was queued).
// Undo/redo entries of a resolved id are dropped — replaying one would
// silently undo the examiner's choice.
function resolveConflicts(ids, side) {
  const done = [];
  for (const id of ids) {
    const c = conflictIds.get(id);
    if (!c) continue;
    conflictIds.delete(id);
    state.revisions[id] = c.server_revision;
    if (side === "server") {
      applyAnnotationToState(id, c.server);
      touchAnnotation(id, { base: c.server_revision });
      // touchAnnotation stamps this window's examiner; the server's goes back.
      state.examiners[id] = (c.server && c.server.examiner) || null;
      if (state.drafts[id]) state.drafts[id].examiner = state.examiners[id];
    } else {
      if (c.queued && c.mine) applyAnnotationToState(id, c.mine);
      touchAnnotation(id, { base: c.server_revision });
    }
    done.push(id);
  }
  if (!done.length) return 0;
  if (typeof dropJournalIds === "function") dropJournalIds(done);
  renderConflictBanner();
  if (typeof render === "function") render();
  if (typeof updateRangeLabel === "function") updateRangeLabel();
  renderSaveState();
  if (viewerIsServed) scheduleAutosave(0);
  emitDirty();
  return done.length;
}
function resolveConflictsUseServer(ids) { return resolveConflicts(ids, "server"); }
function resolveConflictsUseMine(ids) { return resolveConflicts(ids, "mine"); }

// --- Notices row: range-divergence + conflict banners --------------------
// Both banners live in one row of the viewer grid (controls.css
// .ft-notices, grid-area "notice"); colours come from the theme tokens
// (--warn-bg / --warn / --text), so both themes hold AA contrast.
function noticesHost() {
  let host = ftRoot.querySelector(":scope > .ft-notices");
  if (!host) {
    host = document.createElement("div");
    host.className = "ft-notices";
    ftRoot.insertBefore(host, ftRoot.firstChild);
  }
  return host;
}
function noticeButton(id, label, onClick) {
  const b = document.createElement("button");
  b.type = "button";
  b.id = id;
  b.textContent = label;
  b.addEventListener("click", onClick);
  return b;
}
const CONFLICT_LIST_MAX = 300;
var conflictPanelOpen = false;
function recordLabelOf(id) {
  const record = recordById.get(id);
  return record ? (record.originalName || record.name || id) : id;
}
function renderConflictBanner() {
  let bar = document.getElementById("markConflictBanner");
  let panel = document.getElementById("markConflictPanel");
  if (!conflictIds.size) {
    const hadFocus = (bar && bar.contains(document.activeElement)) || (panel && panel.contains(document.activeElement));
    if (bar) bar.remove();
    if (panel) panel.remove();
    conflictPanelOpen = false;
    if (hadFocus && typeof viewerFocus === "function") viewerFocus();
    return;
  }
  if (!bar) {
    bar = document.createElement("div");
    bar.id = "markConflictBanner";
    bar.className = "ft-banner";
    bar.setAttribute("role", "alert");
    const msg = document.createElement("span");
    msg.className = "ft-banner-msg conflict-msg";
    bar.appendChild(msg);
    const toggle = noticeButton("markConflictList", t("conflict.list"), () => {
      conflictPanelOpen = !conflictPanelOpen;
      renderConflictBanner();
      if (conflictPanelOpen) document.getElementById("markConflictPanelTitle")?.focus();
    });
    toggle.className = "toggle";
    toggle.setAttribute("aria-controls", "markConflictPanel");
    bar.appendChild(toggle);
    bar.appendChild(noticeButton("markConflictUseServer", t("conflict.useServerAll"), () => resolveConflictsUseServer([...conflictIds.keys()])));
    bar.appendChild(noticeButton("markConflictUseMine", t("conflict.useMineAll"), () => resolveConflictsUseMine([...conflictIds.keys()])));
    noticesHost().appendChild(bar);
  }
  const ids = [...conflictIds.keys()];
  const msg = bar.querySelector(".conflict-msg");
  msg.textContent = tf("conflict.msg", { n: ids.length });
  const toggle = document.getElementById("markConflictList");
  toggle.setAttribute("aria-expanded", conflictPanelOpen ? "true" : "false");
  if (!panel) {
    panel = document.createElement("section");
    panel.id = "markConflictPanel";
    panel.className = "conflict-panel";
    panel.setAttribute("aria-labelledby", "markConflictPanelTitle");
    panel.addEventListener("click", event => {
      const btn = event.target.closest("button[data-side]");
      if (!btn || !panel.contains(btn)) return;
      const item = btn.closest("[data-id]");
      const id = item && item.dataset.id;
      if (!id) return;
      const next = item.nextElementSibling && item.nextElementSibling.dataset.id;
      const prev = item.previousElementSibling && item.previousElementSibling.dataset.id;
      resolveConflicts([id], btn.dataset.side);
      // Keyboard flow: focus moves to the same choice on the next record.
      const target = [next, prev].map(other => other && panel.querySelector(`[data-id="${CSS.escape(other)}"] button[data-side="${btn.dataset.side}"]`)).find(Boolean);
      if (target) target.focus();
      else if (conflictIds.size) document.getElementById("markConflictPanelTitle")?.focus();
    });
    panel.addEventListener("keydown", event => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      conflictPanelOpen = false;
      renderConflictBanner();
      document.getElementById("markConflictList")?.focus();
    });
    bar.after(panel);
  }
  panel.hidden = !conflictPanelOpen;
  if (conflictPanelOpen) renderConflictList(panel, ids);
}
function renderConflictList(panel, ids) {
  const shown = ids.slice(0, CONFLICT_LIST_MAX);
  const e = escapeHtml;
  panel.innerHTML = `<h2 id="markConflictPanelTitle" class="conflict-title" tabindex="-1">${e(tf("conflict.listTitle", { n: ids.length }))}</h2>` +
    `<ul class="conflict-list">${shown.map(id => {
      const c = conflictIds.get(id);
      const label = recordLabelOf(id);
      return `<li class="conflict-item" data-id="${e(id)}">` +
        `<div class="conflict-rec"><span class="conflict-name">${e(label)}</span>${label !== id ? ` <span class="conflict-id">${e(id)}</span>` : ""}${c.queued ? ` <span class="conflict-tag">${e(t("conflict.queued"))}</span>` : ""}</div>` +
        `<dl class="conflict-vals"><div><dt>${e(t("conflict.server"))}</dt><dd>${e(annotationText(c.server))}</dd></div>` +
        `<div><dt>${e(t("conflict.mine"))}</dt><dd>${e(annotationText(c.mine))}</dd></div></dl>` +
        `<div class="conflict-acts">` +
        `<button type="button" data-side="server" aria-label="${e(label)} — ${e(t("conflict.useServer"))}">${e(t("conflict.itemServer"))}</button>` +
        `<button type="button" data-side="mine" aria-label="${e(label)} — ${e(t("conflict.useMine"))}">${e(t("conflict.itemMine"))}</button>` +
        `</div></li>`;
    }).join("")}</ul>` +
    (ids.length > shown.length ? `<p class="conflict-more">${e(tf("conflict.more", { n: ids.length - shown.length }))}</p>` : "");
}

// --- Server queue conflicts (GET /api/mark-conflicts, §10.2) -------------
// A save the server queued (deferred) is applied when its job ends; a
// revision that moved meanwhile is journaled, not applied. The journal's
// `queued` value — what this (or another) window submitted — is "mine".
var queueConflictsSeen = new Set();
async function loadServerConflicts(st) {
  const mc = st && st.mark_conflicts;
  if (!mc || !(Number(mc.count) > 0)) return 0;
  let data = null;
  try {
    const res = await fetch("/api/mark-conflicts", { cache: "no-store" });
    data = res.ok ? await res.json() : null;
  } catch (e) { data = null; }
  if (ftSignal.aborted) return 0;
  if (!data || data.ok === false || !Array.isArray(data.conflicts)) {
    // A workstation without the route: ids from /api/status, values from records-meta.
    const ids = Array.isArray(mc.ids) ? mc.ids.filter(id => typeof id === "string") : [];
    return loadQueueConflicts(ids);
  }
  const list = [];
  for (const c of data.conflicts) {
    if (!c || typeof c.id !== "string") continue;
    queueConflictsSeen.add(c.id);
    const server = normServerAnnotation(c.server);
    list.push({ id: c.id, server_revision: c.server_revision, server, mine: queuedAnnotation(c.queued, server) || localAnnotationOf(c.id) });
  }
  const n = registerConflicts(list, { queued: true });
  if (n) { renderSaveState(); emitDirty(); }
  return n;
}
async function loadQueueConflicts(ids) {
  const wanted = (ids || []).filter(id => !queueConflictsSeen.has(id) && !conflictIds.has(id));
  if (!wanted.length) return 0;
  let meta = null;
  try {
    const res = await fetch("/api/records-meta", { cache: "no-store" });
    meta = res.ok ? await res.json() : null;
  } catch (e) { meta = null; }
  const ann = meta && meta.annotations;
  if (!ann || ftSignal.aborted) return 0;
  const serverById = serverAnnotationMap(ann);
  const list = [];
  for (const id of wanted) {
    queueConflictsSeen.add(id);
    list.push({ id, server_revision: Number((ann.revisions || {})[id]) || 0, server: serverById.get(id) || null });
  }
  const n = registerConflicts(list, { queued: true });
  if (n) { renderSaveState(); emitDirty(); }
  return n;
}
// Runs `fn` while "saved" is withheld (conflictChecks > 0).
async function withConflictCheck(fn) {
  conflictChecks += 1;
  renderSaveState();
  try { return await fn(); } catch (e) { return null; } finally {
    conflictChecks -= 1;
    if (!ftSignal.aborted) { renderSaveState(); emitDirty(); }
  }
}

// After the server queue drained: per id whose server revision moved past
// the one this window knows (records-meta revisions + writers, §10.3):
//   last writer is this window      → take the revision only
//   else, nothing unsaved here      → take value AND revision
//   else (an unsaved local edit)    → conflict
// Never a revision without its value: that would turn the next save of a
// stale value into an overwrite.
async function reconcileWithServer() {
  let meta = null;
  try {
    const res = await fetch("/api/records-meta", { cache: "no-store" });
    meta = res.ok ? await res.json() : null;
  } catch (e) { meta = null; }
  const ann = meta && meta.annotations;
  if (!ann || !ann.revisions || typeof ann.revisions !== "object" || ftSignal.aborted) return false;
  const writers = ann.writers && typeof ann.writers === "object" ? ann.writers : null;
  const serverById = serverAnnotationMap(ann);
  const me = writerId();
  const conflicts = [];
  let changed = false;
  for (const [id, raw] of Object.entries(ann.revisions)) {
    const rev = Number(raw);
    const known = state.revisions[id];
    if (!Number.isFinite(rev) || (Number.isFinite(known) && rev <= known) || conflictIds.has(id)) continue;
    const draft = state.drafts[id];
    if (writers && writers[id] === me) {
      state.revisions[id] = rev;
      if (draft && !draft.conflict) { draft.base = rev; draftFlushKeys.add(id); }
      continue;
    }
    if (!draft && !state.dirtyIds.has(id)) {
      applyAnnotationToState(id, serverById.get(id) || null);
      state.revisions[id] = rev;
      changed = true;
      continue;
    }
    conflicts.push({ id, server_revision: rev, server: serverById.get(id) || null });
  }
  if (conflicts.length) registerConflicts(conflicts);
  if (draftFlushKeys.size) scheduleDraftFlush();
  if (changed) {
    if (typeof requestRender === "function") requestRender();
    if (typeof updateRangeLabel === "function") updateRangeLabel();
  }
  return true;
}
async function afterQueueDrained(st) {
  await withConflictCheck(async () => {
    await whenSaveSettled();
    await loadServerConflicts(st);
    await reconcileWithServer();
  });
  queuedOnServer = 0;
  queuedIds.clear();
  renderSaveState();
}

// While the server holds queued marks, poll until its queue is empty so the
// indicator turns "saved" only once they really reached the case DB and
// the drain's conflicts were read.
var queueWatchTimer = null;
function watchServerQueue() {
  if (queueWatchTimer || !viewerIsServed) return;
  queueWatchTimer = setInterval(async () => {
    if (ftSignal.aborted) { clearInterval(queueWatchTimer); queueWatchTimer = null; return; }
    try {
      const res = await fetch("/api/status", { cache: "no-store" });
      const st = res.ok ? await res.json() : null;
      if (st && Number(st.pending_marks) === 0 && queueWatchTimer) {
        clearInterval(queueWatchTimer);
        queueWatchTimer = null;
        await afterQueueDrained(st);
      }
    } catch (e) { /* keep waiting; the indicator stays "queued" */ }
  }, 3000);
}

function scheduleAutosave(delay) {
  clearTimeout(autosaveTimer);
  if (ftSignal.aborted) return; // this mount was destroyed
  autosaveTimer = setTimeout(() => { autosaveTimer = null; syncMarksNow(); }, delay);
}

// Called per touched id: the timer restarts right away (cheap), the
// indicator and the host notification are coalesced into one microtask,
// so a 30k-record bulk action renders the save state once, not 30k times.
var dirtyRenderQueued = false;
function markReviewDirty() {
  pendingReviewSync = true;
  saveRevision += 1;
  if (viewerIsServed) {
    // While backing off after a failure keep the backoff timer: an edit
    // must not turn a busy server into a request-per-keystroke loop.
    if (!(saveFailures > 0 && autosaveTimer)) scheduleAutosave(AUTOSAVE_DEBOUNCE_MS);
  }
  if (dirtyRenderQueued) return;
  dirtyRenderQueued = true;
  queueMicrotask(() => {
    dirtyRenderQueued = false;
    if (ftSignal.aborted) return;
    renderSaveState();
    emitDirty();
  });
}

// Per-id success: the server confirmed these ids at these revisions. A
// draft is forgotten only when it is still the one that was sent (`seq`
// unchanged); an edit made while the request was in flight stays dirty,
// now based on the revision just confirmed.
function applyAppliedIds(applied, sentSeq) {
  for (const [id, rev] of Object.entries(applied || {})) {
    if (Number.isFinite(rev)) state.revisions[id] = rev;
    const draft = state.drafts[id];
    if (!draft) state.dirtyIds.delete(id);
    else if (draft.seq === sentSeq.get(id)) dropDraft(id);
    else if (Number.isFinite(rev) && !draft.conflict) { draft.base = rev; draftFlushKeys.add(id); scheduleDraftFlush(); }
  }
}
// Deferred (queued on the server, durable there): the draft goes like an
// applied one, the revision stays — the drain decides it (afterQueueDrained).
function applyQueuedIds(ids, sentSeq) {
  for (const id of ids || []) {
    queuedIds.add(id);
    const draft = state.drafts[id];
    if (!draft) state.dirtyIds.delete(id);
    else if (draft.seq === sentSeq.get(id)) dropDraft(id);
  }
}

// A save whose mount was destroyed before the reply (hosted unmount,
// FTViewer.reload): the reply still settles this writer's stored drafts —
// a confirmed draft must not be replayed as unsaved — and the next mount
// of the same writer (same token) is told through a window event.
function settleLateSave(result, sentSeq) {
  const me = writerId();
  const stored = id => {
    try { return JSON.parse(localStorage.getItem(draftKey(me, id)) || "null"); } catch (e) { return null; }
  };
  const forget = id => {
    const d = stored(id);
    if (d && d.seq === sentSeq.get(id)) { try { localStorage.removeItem(draftKey(me, id)); } catch (e) { /* storage blocked */ } }
  };
  Object.keys(result.applied || {}).forEach(forget);
  (result.queuedIds || []).forEach(forget);
  for (const c of result.conflicts || []) {
    const d = stored(c.id);
    if (!d || d.seq !== sentSeq.get(c.id)) continue;
    d.conflict = { server: normServerAnnotation(c.server), server_revision: Number(c.server_revision) || 0, queued: false };
    try { localStorage.setItem(draftKey(me, c.id), JSON.stringify(d)); } catch (e) { /* the draft stays dirty: resent */ }
  }
  try {
    window.dispatchEvent(new CustomEvent("ft-viewer-late-save", { detail: {
      writer: me, applied: result.applied || {}, queued: result.queuedIds || [], conflicts: result.conflicts || [],
      pending: result.pending || 0, seqs: Object.fromEntries(sentSeq)
    } }));
  } catch (e) { /* no listener */ }
}
window.addEventListener("ft-viewer-late-save", event => {
  const d = event.detail || {};
  if (d.writer !== writerId()) return;
  const seqs = new Map(Object.entries(d.seqs || {}));
  applyAppliedIds(d.applied, seqs);
  applyQueuedIds(d.queued, seqs);
  registerConflicts((d.conflicts || []).filter(c => c && state.drafts[c.id] && state.drafts[c.id].seq === seqs.get(c.id)));
  if ((d.queued || []).length) { queuedOnServer = Math.max(queuedOnServer, Number(d.pending) || 1); watchServerQueue(); }
  pendingReviewSync = state.dirtyIds.size > 0;
  if (typeof requestRender === "function") requestRender();
  renderSaveState();
  emitDirty();
}, { signal: ftSignal });

async function pushMarksToCase(payload) {
  const res = await fetch("/api/import-marks", {
    method: "POST",
    headers: { "Content-Type": "text/plain" },
    body: JSON.stringify({ marks_json: JSON.stringify(payload) })
  });
  let data = null;
  try { data = await res.json(); } catch { data = null; }
  if (!data || typeof data !== "object") {
    return { ok: false, error: `HTTP ${res.status}`, status: res.status };
  }
  if (!data.ok && !data.status) data.status = res.status;
  return data;
}

// One save attempt. Resolves { ok, error, offline, empty, queued }.
// A call made while a save is in flight is queued behind it, not dropped.
async function syncMarksNow() {
  if (!viewerIsServed) return { ok: false, offline: true, error: t("save.offlineErr") };
  // A destroyed mount never writes: the next mount owns the drafts.
  if (ftSignal.aborted) return { ok: false, error: "viewer destroyed" };
  if (saveInFlight) {
    saveQueued = true;
    return { ok: false, queued: true };
  }
  const payload = marksPayload();
  if (payloadEmpty(payload)) {
    pendingReviewSync = false;
    saveFailures = 0;
    saveError = "";
    renderSaveState();
    emitDirty();
    return { ok: true, empty: true };
  }
  const sentSeq = new Map(payloadIds(payload).map(id => [id, state.drafts[id] ? state.drafts[id].seq : undefined]));
  saveInFlight = true;
  clearTimeout(autosaveTimer);
  autosaveTimer = null;
  renderSaveState();
  let result;
  try {
    for (const part of splitByExaminer(payload)) {
      const r = await sendPayloadChunks(part, pushMarksToCase, { maxIds: saveChunkIds, legacy: saveLegacyProtocol });
      if (!result) result = r;
      else {
        Object.assign(result.applied, r.applied);
        result.conflicts.push(...r.conflicts);
        result.queuedIds.push(...r.queuedIds);
        result.posts += r.posts;
        result.deferred = result.deferred || r.deferred;
        result.pending = Math.max(result.pending, r.pending);
        result.maxIds = r.maxIds;
        result.legacy = r.legacy;
        if (!r.ok) Object.assign(result, { ok: false, error: r.error, status: r.status, offline: r.offline });
      }
      if (r.maxIds) saveChunkIds = r.maxIds;
      if (r.legacy) saveLegacyProtocol = true;
      if (!r.ok) break;
    }
  } catch (error) {
    result = { ok: false, offline: true, error: t("save.offlineErr"), applied: {}, conflicts: [], queuedIds: [] };
  }
  saveInFlight = false;
  if (ftSignal.aborted) {
    settleLateSave(result, sentSeq);
    notifySaveSettled();
    return result;
  }
  if (result.maxIds) saveChunkIds = result.maxIds;
  if (result.legacy) saveLegacyProtocol = true;
  // Chunks that landed before a later failure are confirmed either way.
  applyAppliedIds(result.applied, sentSeq);
  applyQueuedIds(result.queuedIds, sentSeq);
  registerConflicts(result.conflicts);
  if (result.ok) {
    // queued: durably on the server but not in the case DB until the
    // enrichment step ends — shown as "저장 대기(보강 중)", never a failure.
    if (result.deferred) {
      queuedOnServer = Math.max(queuedOnServer, Number(result.pending) || 1);
      watchServerQueue();
    }
    saveFailures = 0;
    saveError = "";
    saveRetryAt = 0;
    // Edits made while the request was in flight (or ids the server
    // skipped) are still dirty: push again instead of forgetting them.
    pendingReviewSync = state.dirtyIds.size > 0;
    if (pendingReviewSync) scheduleAutosave(AUTOSAVE_DEBOUNCE_MS);
  } else {
    const firstFailure = saveFailures === 0;
    saveFailures += 1;
    saveError = String(result.error || "?");
    const delay = autosaveRetryDelay(saveFailures);
    saveRetryAt = Date.now() + delay;
    scheduleAutosave(delay);
    if (firstFailure) toast(tf("toast.saveFailed", { err: saveError }));
    if (firstFailure) FTViewer._emit("error", { message: tf("toast.saveFailed", { err: saveError }), detail: saveError, stage: "save" });
  }
  scheduleDraftFlush();
  notifySaveSettled();
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
  emitDirty();
  return result;
}

// The debounced auto-save entry point.
function autoApplyMarks() {
  if (!pendingReviewSync || !viewerIsServed) return Promise.resolve({ ok: true, empty: true });
  return syncMarksNow();
}

window.addEventListener("beforeunload", (event) => {
  flushDraftsNow();
  if (!pendingReviewSync && !state.dirtyIds.size && !conflictIds.size && !saveInFlight && !draftsUnpersisted) return;
  event.preventDefault();
  event.returnValue = "";
}, { signal: ftSignal });

if (saveStateEl) {
  // Clicking the indicator saves (or retries) right away.
  saveStateEl.addEventListener("click", () => {
    if (!viewerIsServed) { toast(t("save.title.local")); return; }
    if (reviewIdle() && !saveInFlight) { toast(t("save.title.saved")); return; }
    if (!state.dirtyIds.size && conflictIds.size && !saveInFlight) { toast(tf("conflict.msg", { n: conflictIds.size })); return; }
    syncMarksNow();
  });
}
// Back online / tab visible again: retry a failed save now instead of
// waiting out the backoff.
window.addEventListener("online", () => { if (viewerIsServed && pendingReviewSync) scheduleAutosave(0); }, { signal: ftSignal });
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") return;
  writerHeartbeat();
  if (viewerIsServed && pendingReviewSync && saveFailures > 0) scheduleAutosave(0);
}, { signal: ftSignal });

// --- Two windows on one case (U2, §10.3) ----------------------------------
// Each window refreshes ft.alive:<case>:<writer> every 5 s and answers
// pings on ft-alive:<case> (30-records openAliveChannel). A real unload
// (pagehide, not the back/forward cache) marks the writer "closed", so a
// reload hands its drafts over at once; a crash is recognised by a 5-minute
// stale heartbeat plus an unanswered ping (adoptOrphanDrafts).
const ALIVE_TICK_MS = 5000;
function writerHeartbeat() {
  try { localStorage.setItem(aliveKey(writerId()), String(Date.now())); } catch (e) { /* storage blocked/full: the ping still answers */ }
}
function otherLiveWriters() {
  const others = [];
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key || !key.startsWith(ALIVE_PREFIX)) continue;
      const writer = key.slice(ALIVE_PREFIX.length);
      if (writer !== writerId() && writerRecentlyAlive(writer)) others.push(writer);
    }
  } catch (e) { /* storage blocked */ }
  return others;
}
// Heartbeat keys of writers that left nothing behind (closed, or stale
// for ALIVE_STALE_MS, with no stored draft) are swept at boot.
function sweepStaleHeartbeats() {
  try {
    const withDrafts = new Set(storedDraftEntries().map(entry => entry.writer));
    const stale = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key || !key.startsWith(ALIVE_PREFIX)) continue;
      const writer = key.slice(ALIVE_PREFIX.length);
      if (writer === writerId() || withDrafts.has(writer)) continue;
      const verdict = writerVerdict(writer);
      if (verdict === "closed" || verdict === "stale") stale.push(key);
    }
    stale.forEach(key => localStorage.removeItem(key));
  } catch (e) { /* storage blocked */ }
}
openAliveChannel();
writerHeartbeat();
const aliveTimer = setInterval(() => {
  if (ftSignal.aborted) { clearInterval(aliveTimer); return; }
  writerHeartbeat();
  adoptOrphanDrafts();
}, ALIVE_TICK_MS);
// Another window started writing drafts after this one booted: watch it.
window.addEventListener("storage", event => {
  const at = event.key && event.newValue != null ? parseDraftKey(event.key) : null;
  if (at && at.writer !== writerId()) skippedDraftWriters.add(at.writer);
}, { signal: ftSignal });
// destroy() (hosted reload, FTViewer.reload) aborts this mount: its pending
// draft changes reach storage before the next mount reads them, or a
// draft the server already confirmed would be replayed as unsaved.
ftSignal.addEventListener("abort", () => { clearInterval(aliveTimer); flushDraftsNow(true); notifySaveSettled(); });
window.addEventListener("pagehide", (event) => {
  flushDraftsNow();
  if (event.persisted) return; // back/forward cache: the page may come back
  try {
    localStorage.removeItem(aliveKey(writerId()));
    localStorage.setItem(aliveKey(writerId()), ALIVE_CLOSED);
  } catch (e) { /* no marker: the ping decides */ }
}, { signal: ftSignal });
window.addEventListener("pageshow", (event) => { if (event.persisted) writerHeartbeat(); }, { signal: ftSignal });
if (otherLiveWriters().length) toast(t("conflict.otherWindow"));
sweepStaleHeartbeats();
registerRestoredConflicts();
// A stale writer's drafts are checked (ping) right away, not after 5 s.
if (skippedDraftWriters.size) adoptOrphanDrafts();

// Server-served viewers can push marks straight into the case DB instead of
// the download → file-pick → import dance.
document.getElementById("btnApplyMarks").addEventListener("click", async () => {
  if (!viewerIsServed) { toast(t("offline.applyTitle")); return; }
  const payload = marksPayload();
  if (payloadEmpty(payload)) {
    if (conflictIds.size) { toast(tf("conflict.msg", { n: conflictIds.size })); return; }
    toast(t("toast.noChanges"));
    return;
  }
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
  else if (result.ok && result.conflicts && result.conflicts.length) toast(tf("conflict.msg", { n: conflictIds.size }));
  // v2 sends dirty ids only: with nothing left to push the honest answer is
  // "everything is already in the case", not a fake "applied".
  else if (result.ok && result.empty) toast(conflictIds.size ? tf("conflict.msg", { n: conflictIds.size }) : t("save.title.saved"));
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
    "warnings", "note", "anomalies", "audio_state", "audio_max_db", "audio_mean_db"];
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
    (record.anomalies || []).map(item => item.kind).join("; "),
    ...Object.values(audioExportFields(record))
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
  await withBusy(btn, async () => {
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
  }, { fail: () => t("toast.offline") });
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
document.getElementById("btnDownloadFiles").addEventListener("click", async (ev) => {
  const selected = selectedRecords();
  if (!selected.length) { toast(t("toast.selectFirst")); return; }
  if (canFolderSave()) {
    await withBusy(ev.currentTarget, async () => {
      if (typeof window.showDirectoryPicker !== "function") throw new Error(t("toast.noDirPicker"));
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
    }, { fail: err => err?.name === "AbortError" ? "" : tf("toast.folderFail", { err: err?.message || "" }) });
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
  const ids = selectedRecords().map(r => r.id).join(",");
  await withBusy(ev.currentTarget, async () => {
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
  }, { start: "toast.queueRunning", fail: () => t("toast.queueOffline") });
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
if (viewerIsServed) {
  // Conflicts the server recorded while draining its marks queue (possibly
  // before a restart) are shown on boot, and "saved" waits for that answer.
  withConflictCheck(async () => {
    const res = await fetch("/api/status", { cache: "no-store" });
    const st = res.ok ? await res.json() : null;
    if (st && !ftSignal.aborted) await loadServerConflicts(st);
  });
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
    const id = annotationMarkId(mark);
    if (id && rangeValid(mark.range)) map[id] = normRange(mark.range);
  }
  return map;
}

function detectRangeDivergence() {
  if (!viewerIsServed) return [];
  const server = serverRanges();
  const pending = state.dirtyIds;
  const ids = new Set([...Object.keys(server), ...Object.keys(state.ranges)]);
  const diverged = [];
  for (const id of ids) {
    if (pending.has(id) || conflictIds.has(id)) continue; // unsynced local edit (pushed), or already in the conflict list
    // Only a conflict when BOTH sides hold a range and they differ; a
    // one-sided range is a seed (server only) or an unsynced edit (browser only).
    if (!rangeValid(server[id]) || !rangeValid(state.ranges[id])) continue;
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
  bar.className = "ft-banner";
  bar.setAttribute("role", "alert");
  const msg = document.createElement("span");
  msg.className = "ft-banner-msg";
  msg.textContent = tf("range.divergeMsg", { n: ids.length });
  msg.title = ids.slice(0, 20).map(id => `${id}: ${t("range.server")} ${rangeText(server[id])} / ${t("range.browser")} ${rangeText(state.ranges[id])}`).join("\n");
  bar.appendChild(msg);
  const pick = side => () => {
    const hadFocus = bar.contains(document.activeElement);
    resolveRangeDivergence(ids, side);
    bar.remove();
    if (hadFocus && typeof viewerFocus === "function") viewerFocus();
  };
  bar.appendChild(noticeButton("rangeUseServer", t("range.useServer"), pick("server")));
  bar.appendChild(noticeButton("rangeUseBrowser", t("range.useBrowser"), pick("browser")));
  const host = noticesHost();
  host.insertBefore(bar, host.firstChild);
}

// A fresh browser (new origin/port, cleared storage) has no local ranges:
// adopt the server's so they are visible and a later save can never send a
// null range for an id the browser merely has no value for.
function seedServerRanges() {
  const server = serverRanges();
  let changed = false;
  for (const id of Object.keys(server)) {
    if (rangeValid(state.ranges[id])) continue;
    state.ranges[id] = { ...server[id] };
    changed = true;
  }
  if (changed) storageSet(RANGES_KEY, state.ranges);
  return changed;
}

(function initRangeDivergence() {
  seedServerRanges();
  const ids = detectRangeDivergence();
  if (!ids.length) return;
  ids.forEach(id => rangeConflictIds.add(id));
  showRangeDivergenceBanner(ids);
})();
