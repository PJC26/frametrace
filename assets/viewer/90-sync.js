// --- IN/OUT range sync (FOR-01, R4 A1/A2) ---------------------------------
// Ranges are first-class annotation fields of save protocol v2: every
// marks entry carries `range: {in, out}` ({null, null} = cleared), they
// hydrate from the server like marks, edits ride in the per-id drafts, and
// a disagreement with the server is an ordinary conflict (the conflict
// list: server value / mine) — never a separate banner, never an omitted
// range that the server would read as "keep" while the indicator says
// "saved".
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
// ids whose revision moved on instead of overwriting them. `deleted_ids`
// means "remove the mark row" and is used only for an id with no mark, no
// note AND no range (A1): a record that keeps its range always travels as a
// marks entry with that range, so a tag edit or a cleared mark can never
// delete a stored range. Tags (possibly empty) always ride explicitly —
// inside the marks entry, or in `tags` beside a deletion — so "all tags
// removed" reaches the server too.
// Self-contained on purpose (the review-batch VM harness evaluates this
// function alone): ranges are normalised locally.
//   opts.all   every annotated id (the marks download), not just dirty ones
function marksPayload(opts) {
  const all = !!(opts && opts.all);
  const markRows = typeof markRowMaybe === "undefined" ? null : markRowMaybe;
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
    Object.keys(state.ranges || {}).forEach(id => { if (rangeOfId(id)) ids.add(id); });
    Object.keys(state.tags || {}).forEach(id => { if ((state.tags[id] || []).length) ids.add(id); });
  }
  const marks = [];
  const tags = [];
  const deleted = [];
  const baseRevisions = {};
  for (const id of ids) {
    const mark = state.marks[id];
    const note = (state.notes[id] || "").trim();
    const range = rangeOfId(id);
    const tagList = Array.isArray(state.tags[id]) ? [...state.tags[id]] : [];
    // The base is the revision the unsaved edit was made on (the draft's,
    // also for a draft adopted from a dead window), else the last one seen.
    const draft = state.drafts && state.drafts[id];
    // R5 U1: a draft made while the annotations were unreadable goes out
    // with base -1 — the server always answers it with a conflict (no
    // same-window exception), so it can never overwrite what it never saw.
    baseRevisions[id] = draft && draft.unsafe_base === true ? -1
      : draft && Number.isFinite(draft.base) ? draft.base
      : (Number.isFinite(state.revisions && state.revisions[id]) ? state.revisions[id] : 0);
    if (!mark && !note && !range) {
      // Nothing left of the mark row: delete it — but only when the server
      // may hold one (markRowMaybe: hydrated, or a marks entry was sent
      // since). A record that only ever had tags sends its tags alone.
      if (!markRows || markRows.has(id)) deleted.push(id);
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
      tags: tagList,
      // Always explicit: {null, null} clears, a value sets — never omitted.
      range: range || { in: null, out: null }
    };
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

// Every id a body touches: marks entries, deletions and tag-only entries.
function payloadIds(payload) {
  return [...new Set([...payload.marks.map(m => m.id), ...payload.deleted_ids, ...(payload.tags || []).map(t => t.id)])];
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
        // R5 U1: a body that must stay revision-checked (base -1 drafts) is
        // never downgraded to last-write-wins — the caller holds it instead.
        if (opts && opts.noLegacy) {
          return Object.assign(out, { ok: false, error: String(res.error), status: Number(res.status) || 0, legacyRefused: true });
        }
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
      // R4 B4: the server was briefly busy and queued nothing — transient,
      // retried after `retry_after_ms`; the ids stay dirty.
      if (res.busy && res.retry) {
        out.busy = true;
        out.retryAfterMs = Math.max(250, Number(res.retry_after_ms) || 1000);
      }
      return out;
    }
    // R4 B7: the marks are in, only the report refresh failed.
    if (res.report_error) out.reportError = String(res.report_error);
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
var saveBusyRetries = 0;          // consecutive "busy, retry" replies (R4 B4)
const SAVE_BUSY_RETRIES = 10;
const SAVE_BUSY_WAIT_MS = 20000;  // FTViewer.save(): longest total wait for "busy, retry" (R5 U6)
// R7 U5: FTViewer.save() default answer budget per caller — an explicit
// save (finalize retry, case switch, attribution) keeps the full 20 s; only
// the close flush (hBeforeClose) asks for less (4 s, a hard total).
const SAVE_CALLER_WAIT_MS = SAVE_BUSY_WAIT_MS;
// R8 U2: an explicit save ({ patient: true }: finalize retry, case switch,
// attribution) does not give up while this window's save request is still
// progressing — past its 20 s it keeps waiting as long as a request was
// sent or answered within SAVE_STALL_MS; only a request left without a
// reply that long (a genuine stall) times out. A call that names no budget
// and is not patient — the desktop shell's fallback `FTViewer.save()` when
// a page has no hBeforeClose — gets the close budget, a hard SAVE_DEFAULT_MS.
const SAVE_STALL_MS = 30000;
const SAVE_DEFAULT_MS = 4000;
var saveProgressAt = 0; // last request sent / reply received (pushMarksToCase)
var lastReportError = "";         // R4 B7 notice shown once per text
var saveError = "";
var saveAuthExpired = false; // the last attempt was refused with 401 (U8)
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
// A5: the server holds queue conflicts (or a queue) this window could not
// read — "saved" is never claimed then; the lookup retries with backoff.
var conflictLookupFailed = false;
var conflictLookupFailures = 0;
var conflictLookupTimer = null;
// A3: annotations could not be read at load — nothing is ever sent.
var annotationsReadOnly = !!annotationsError;
// A4: ids the server holds in its marks queue (from /api/marks-pending at
// boot, or this window's own deferred saves): shown with the queued value.
var serverQueuedIds = new Set();
// R5 U4: ids whose queued value is SHOWN over the server's (an overlay).
// When the queue drains, every one of them goes back to what the case DB
// then holds — a queued body the drain did not apply (a conflict, a
// refusal) must not keep showing as if it had landed.
var queueOverlayIds = new Set();
// "" | "unreadable" | "truncated": the queued values could not all be shown.
var queueValuesState = "";
var conflictLookupRetryAt = 0;
const AUTOSAVE_DEBOUNCE_MS = 2500;
const AUTOSAVE_RETRY_BASE_MS = 2000;
const AUTOSAVE_RETRY_MAX_MS = 60000;

function autosaveRetryDelay(failures) {
  return Math.min(AUTOSAVE_RETRY_MAX_MS, AUTOSAVE_RETRY_BASE_MS * Math.pow(2, Math.max(0, failures - 1)));
}

function pendingSaveCount() {
  return state.dirtyIds.size + conflictIds.size || (pendingReviewSync ? 1 : 0);
}
// Nothing unsaved anywhere: no local edit, no conflict, no conflict lookup
// running or failed, and nothing waiting in the server's queue (A4/A5).
function reviewIdle() {
  return localReviewIdle() && queuedOnServer === 0;
}
// This window holds nothing the server has not durably accepted (marks in
// the server's queue are durable there: FTViewer.save() resolves true).
function localReviewIdle() {
  return !pendingReviewSync && !state.dirtyIds.size && !conflictIds.size && conflictChecks === 0 && !conflictLookupFailed && !annotationsReadOnly;
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

// FTViewer.save(opts) (95-init.js). Resolves true once nothing is pending
// here (marks durably queued on the server count as saved). A save already
// in flight is awaited first (A15); a "busy, retry" reply is waited out
// (R5 U6, up to SAVE_BUSY_RETRIES replies). Read-only with NOTHING held
// here: true (R5 U15).
// Budgets (R7 U1/U5, R8 U2):
//   save(ms) / save({ timeoutMs })  a HARD TOTAL from the call (the close
//       flush, hBeforeClose): the wait for the request on the wire, the
//       conflict lookup, every send and busy retry count against it.
//   save({ patient: true[, timeoutMs] })  an explicit save: timeoutMs
//       (default SAVE_CALLER_WAIT_MS) and then on while this window's save
//       request keeps progressing (sent / answered within `stallMs`,
//       default SAVE_STALL_MS) — a slow but live save is never abandoned;
//       a request without any reply for stallMs is a stall: false.
//   save()  (no budget, not patient — the desktop shell's fallback when a
//       page has no hBeforeClose): the hard SAVE_DEFAULT_MS (4 s).
// Past the deadline: false and `opts.timedOut = true` (when an options
// object was passed); the requests go on and their replies still settle
// the drafts.
async function viewerSave(opts) {
  if (!viewerIsServed) return false;
  if (annotationsReadOnly) return !Object.keys(state.drafts).length && !state.dirtyIds.size && !conflictIds.size;
  const o = opts && typeof opts === "object" ? opts : null;
  const patient = !!(o && o.patient);
  const asked = typeof opts === "number" ? opts : o && o.timeoutMs != null ? Number(o.timeoutMs) : NaN;
  const budget = Number.isFinite(asked) && asked >= 0 ? asked : (patient ? SAVE_CALLER_WAIT_MS : SAVE_DEFAULT_MS);
  const stallAsked = Number(o && o.stallMs);
  const stallMs = Number.isFinite(stallAsked) && stallAsked > 0 ? stallAsked : SAVE_STALL_MS;
  const started = Date.now();
  // The deadline NOW: the budget; patient, also as long as this window's
  // save showed life (a request sent / answered) within stallMs.
  const deadline = () => {
    const base = started + budget;
    if (!patient || !(saveInFlight || saveProgressAt >= started)) return base;
    return Math.max(base, saveProgressAt + stallMs);
  };
  const idle = () => localReviewIdle();
  let busyWaits = 0;
  const TIMED_OUT = {};
  let timer = 0;
  const expired = new Promise(resolve => {
    const tick = () => {
      const left = deadline() - Date.now();
      if (left <= 0) { resolve(TIMED_OUT); return; }
      timer = setTimeout(tick, left);
    };
    tick();
  });
  // Every wait of this call races the one deadline.
  const within = (work) => Promise.race([Promise.resolve(work), expired]);
  const timedOut = () => {
    if (o) o.timedOut = true;
    return false;
  };
  try {
    for (let attempt = 0; attempt < 4; attempt++) {
      if ((await within(whenSaveSettled())) === TIMED_OUT) return timedOut();
      if ((await within(whenConflictsChecked())) === TIMED_OUT) return timedOut();
      if (idle()) return true;
      if (conflictLookupFailed) {
        const checked = await within(checkServerConflicts());
        if (checked === TIMED_OUT) return timedOut();
        if (!checked) return false;
      }
      if (!state.dirtyIds.size && !pendingReviewSync && conflictIds.size) return false;
      if (!state.dirtyIds.size && !pendingReviewSync && !conflictIds.size) continue;
      const result = await within(syncMarksNow());
      if (result === TIMED_OUT) return timedOut();
      if (!result.ok && result.busy && !ftSignal.aborted) {
        const wait = Math.max(250, Number(result.retryAfterMs) || 1000);
        if (busyWaits < SAVE_BUSY_RETRIES && Date.now() + wait <= deadline()) {
          busyWaits += 1;
          await new Promise(resolve => setTimeout(resolve, wait));
          attempt -= 1; // a busy reply is not one of the attempts
          continue;
        }
        if (Date.now() + wait > deadline()) return timedOut();
      }
      if (!result.ok && !result.queued) return false;
    }
    if ((await within(whenSaveSettled())) === TIMED_OUT) return timedOut();
    if ((await within(whenConflictsChecked())) === TIMED_OUT) return timedOut();
    return idle();
  } finally {
    clearTimeout(timer);
  }
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
  } else if (annotationsReadOnly) {
    // A3: the case's annotations could not be read — saving would write
    // over records this window never saw. Edits stay in this browser.
    kind = "readonly";
    cls = "failed";
    label = t("save.readonly");
    title = tf(lockKey("save.title.readonly"), { err: annotationsError || "?", n: state.dirtyIds.size });
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
    label = saveBusyRetries > 0 ? tf("save.busyRetry", { n: saveBusyRetries, max: SAVE_BUSY_RETRIES }) : tf("save.pending", { n });
    title = saveBusyRetries > 0 ? tf("save.title.busy", { n: saveBusyRetries, max: SAVE_BUSY_RETRIES })
      : conflictIds.size && !state.dirtyIds.size
      ? tf("save.title.conflict", { n: conflictIds.size })
      : tf("save.title.pending", { n });
    if (conflictIds.size && state.dirtyIds.size) title += " " + tf("save.title.conflict", { n: conflictIds.size });
  } else if (conflictLookupFailed) {
    // A5: the server reported conflicts (or queued saves) that could not be
    // read: unknown, never "saved". Retried with backoff.
    kind = "unknown";
    cls = "failed";
    label = t("save.unknown");
    const sec = Math.max(0, Math.ceil((conflictLookupRetryAt - Date.now()) / 1000));
    title = tf("save.title.unknown", { sec });
  } else if (queuedOnServer > 0) {
    kind = "queued";
    cls = "candidate";
    label = t("save.queuedServer");
    title = tf("save.title.queuedServer", { n: queuedOnServer });
    // R5 U4: the queued values could not (all) be read — say so.
    if (queueValuesState === "unreadable") title += " " + t("save.title.queueUnreadable");
    else if (queueValuesState === "truncated") title += " " + t("save.title.queueTruncated");
    if (queueUnreadableFiles) title += " " + tf("save.title.queueUnreadableFiles", { n: queueUnreadableFiles });
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
  saveStateEl.style.fontWeight = kind === "failed" || kind === "pending" || kind === "readonly" || kind === "unknown" ? "700" : "";
  saveStateEl.dataset.state = kind;
  if (queueValuesState && queuedOnServer > 0) saveStateEl.dataset.queueValues = queueValuesState;
  else delete saveStateEl.dataset.queueValues;
  if (queueUnreadableFiles && queuedOnServer > 0) saveStateEl.dataset.queueUnreadable = String(queueUnreadableFiles);
  else delete saveStateEl.dataset.queueUnreadable;
  if (kind === "pending" && saveBusyRetries > 0) saveStateEl.dataset.busyRetry = String(saveBusyRetries);
  else delete saveStateEl.dataset.busyRetry;
  saveStateEl.innerHTML = `${ftIcon(kind === "failed" || kind === "readonly" || kind === "unknown" ? "warning" : kind === "saved" ? "check" : kind === "saving" ? "spinner" : "save")}<span class="save-label"></span>`;
  saveStateEl.lastElementChild.textContent = label;
  saveStateEl.title = title;
  saveStateEl.setAttribute("aria-label", `${label} — ${title}`);
  // Host notification (FTViewer "save-state"): only on an actual change.
  // `auth`: the last save attempt was refused with 401 (U8) — the host keeps
  // its auth banner up until this goes false (a save got through).
  const signature = `${kind}|${n}|${queuedOnServer}|${conflictIds.size}|${saveAuthExpired}`;
  if (signature !== lastSaveSignature) {
    lastSaveSignature = signature;
    FTViewer._emit("save-state", { state: kind, pending: n, queued: queuedOnServer, conflicts: conflictIds.size, auth: !!saveAuthExpired });
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
  if (typeof bumpAnnotations === "function") bumpAnnotations(id);
  if (a && (a.status || (a.note || "").trim() || rangeValid(a.range))) markRowMaybe.add(id);
  if (a && a.status && a.status !== "noted") state.marks[id] = { status: a.status, marked_unix: Number.isFinite(a.marked_unix) ? a.marked_unix : Math.floor(Date.now() / 1000) };
  else delete state.marks[id];
  state.notes[id] = (a && a.note) || "";
  state.examiners[id] = (a && a.examiner) || null;
  if (a && rangeValid(a.range)) state.ranges[id] = normRange(a.range); else delete state.ranges[id];
  if (a && Array.isArray(a.tags) && a.tags.length) state.tags[id] = [...a.tags]; else delete state.tags[id];
}
// The conflict rides in the id's draft, so a reload restores the banner.
function persistConflict(id, entry) {
  const prev = state.drafts[id];
  const known = Number.isFinite(state.revisions[id]) ? state.revisions[id] : 0;
  const draft = prev ? { ...prev } : { ...draftValuesOf(id), seq: nextDraftSeq(), base: known };
  draft.conflict = { server: entry.server, server_revision: entry.server_revision, queued: !!entry.queued };
  // A queued save's value, or a closed window's (A7), is not this window's
  // state: it is kept so a reload shows (and [내 값] sends) that value.
  if (entry.queued || entry.orphan) draft.conflict.mine = entry.mine;
  if (entry.orphan) draft.conflict.orphan = true;
  if (entry.legacy) draft.conflict.legacy = true;
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
      queued: queued || !!(prev && prev.queued),
      orphan: !!(c.orphan || (prev && prev.orphan)),
      legacy: !!(c.legacy || (prev && prev.legacy))
    };
    state.dirtyIds.delete(id);
    if (entry.server) markRowMaybe.add(id);
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
      mine: (c.queued || c.orphan) && c.mine ? c.mine : localAnnotationOf(id),
      queued: !!c.queued,
      orphan: !!c.orphan,
      legacy: !!c.legacy
    });
  }
  const plain = list.filter(c => !c.queued);
  const fromQueue = list.filter(c => c.queued);
  return registerConflicts(plain) + registerConflicts(fromQueue, { queued: true });
}
// An edit to a conflicted id keeps the conflict; "mine" follows the edit
// (it is now this window's value — no longer a queued / closed-window one).
function conflictEdited(id) {
  const c = conflictIds.get(id);
  if (!c) return;
  c.mine = localAnnotationOf(id);
  c.orphan = false;
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
      if ((c.queued || c.orphan) && c.mine) applyAnnotationToState(id, c.mine);
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
  // R7 U9: the row has a maximum height and scrolls (controls.css): a
  // labelled region. R8 U7: a Tab stop ONLY while it actually overflows
  // (a keyboard must be able to scroll it then) — one short notice is not
  // an extra stop before the grid. Re-checked whenever its content or size
  // changes (MutationObserver + ResizeObserver, tied to this mount).
  if (!host.hasAttribute("role")) {
    host.setAttribute("role", "region");
    host.dataset.i18nAriaLabel = "notices.label";
    host.setAttribute("aria-label", t("notices.label"));
    watchNoticesOverflow(host);
  }
  return host;
}
// R9 U10: the conflict banner stays the LAST banner of the row and its
// list toggle the last button in it, so in Tab order the toggle is
// directly followed by the conflict list (#markConflictPanel, the row's
// next sibling). Every other banner is added before it.
function noticeAppend(bar) {
  const host = noticesHost();
  const conflict = host.querySelector(":scope > #markConflictBanner");
  if (conflict && conflict !== bar) host.insertBefore(bar, conflict);
  else host.appendChild(bar);
  return bar;
}
function syncNoticesTabStop(host) {
  if (!host || !host.isConnected) return;
  const overflows = host.scrollHeight > host.clientHeight + 1;
  if (overflows) {
    if (host.getAttribute("tabindex") !== "0") host.tabIndex = 0;
  } else if (host.hasAttribute("tabindex")) {
    if (document.activeElement === host && typeof viewerFocus === "function") viewerFocus();
    host.removeAttribute("tabindex");
  }
}
function watchNoticesOverflow(host) {
  let queued = false;
  const later = () => {
    if (queued) return;
    queued = true;
    // After layout of the change (one frame), never synchronously inside it.
    requestAnimationFrame(() => { queued = false; if (!ftSignal.aborted) syncNoticesTabStop(host); });
  };
  syncNoticesTabStop(host);
  later();
  try {
    const mo = new MutationObserver(later);
    mo.observe(host, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ["hidden"] });
    ftSignal.addEventListener("abort", () => mo.disconnect(), { once: true });
  } catch (e) { /* no MutationObserver: the resize path still checks */ }
  try {
    const ro = new ResizeObserver(later);
    ro.observe(host);
    ftSignal.addEventListener("abort", () => ro.disconnect(), { once: true });
  } catch (e) { window.addEventListener("resize", later, { signal: ftSignal }); }
}
// R8 U6: notice texts carry their i18n key (data-i18n / data-i18n-args), so
// a language switch re-renders them (applyChromeI18n). `key` is an i18n key.
function noticeButton(id, key, onClick) {
  const b = document.createElement("button");
  b.type = "button";
  b.id = id;
  i18nText(b, key);
  b.addEventListener("click", onClick);
  return b;
}
function noticeMessage(key, subs) {
  const msg = document.createElement("span");
  msg.className = "ft-banner-msg";
  return i18nText(msg, key, subs);
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
    const toggle = noticeButton("markConflictList", "conflict.list", () => {
      conflictPanelOpen = !conflictPanelOpen;
      renderConflictBanner();
      if (conflictPanelOpen) document.getElementById("markConflictPanelTitle")?.focus();
    });
    toggle.className = "toggle";
    toggle.setAttribute("aria-controls", "markConflictPanel");
    bar.appendChild(noticeButton("markConflictUseServer", "conflict.useServerAll", () => resolveConflictsUseServer([...conflictIds.keys()])));
    bar.appendChild(noticeButton("markConflictUseMine", "conflict.useMineAll", () => resolveConflictsUseMine([...conflictIds.keys()])));
    // R9 U10: the toggle last — Tab goes from it straight into the list.
    bar.appendChild(toggle);
    noticesHost().appendChild(bar);
  }
  const ids = [...conflictIds.keys()];
  const msg = bar.querySelector(".conflict-msg");
  i18nText(msg, "conflict.msg", { n: ids.length });
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
    // R8 U3: the list is NOT inside the capped, scrolling notices row (a
    // scroller inside a scroller): it is the row's sibling in the viewer
    // grid (grid-area "conflicts", controls.css) with its own one scroll.
    noticesHost().after(panel);
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
        `<div class="conflict-rec"><span class="conflict-name">${e(label)}</span>${label !== id ? ` <span class="conflict-id">${e(id)}</span>` : ""}${c.queued ? ` <span class="conflict-tag">${e(t("conflict.queued"))}</span>` : ""}${c.orphan ? ` <span class="conflict-tag">${e(t("conflict.orphan"))}</span>` : ""}${c.legacy ? ` <span class="conflict-tag">${e(t("conflict.legacyRange"))}</span>` : ""}</div>` +
        `<dl class="conflict-vals"><div><dt>${e(t("conflict.server"))}</dt><dd>${e(annotationText(c.server))}</dd></div>` +
        `<div><dt>${e(t("conflict.mine"))}</dt><dd>${e(annotationText(c.mine))}</dd></div></dl>` +
        `<div class="conflict-acts">` +
        `<button type="button" data-side="server" aria-label="${e(label)} — ${e(t("conflict.useServer"))}">${e(t("conflict.itemServer"))}</button>` +
        `<button type="button" data-side="mine" aria-label="${e(label)} — ${e(t("conflict.useMine"))}">${e(t("conflict.itemMine"))}</button>` +
        `</div></li>`;
    }).join("")}</ul>` +
    (ids.length > shown.length ? `<p class="conflict-more">${e(tf("conflict.more", { n: ids.length - shown.length }))}</p>` : "");
}

// --- Server queue conflicts (GET /api/mark-conflicts, §10.2, R4 B3) -------
// A save the server queued (deferred) is applied when its job ends; a
// revision that moved meanwhile is journaled, not applied. The journal's
// `queued` value — what this (or another) window submitted — is "mine".
// The route is paged (`?offset&limit`, R4 B3) and every page is read; a
// server without paging ignores the query and answers the whole list once
// (no `total`): the loop stops there. Throws when the conflicts the server
// reported could be read by neither route (A5: the caller shows "충돌 확인
// 실패" and retries — never "saved").
var queueConflictsSeen = new Set();
const CONFLICT_PAGE = 500;
const CONFLICT_PAGES_MAX = 2000;
async function fetchJsonOrNull(url) {
  try {
    const res = await fetch(url, { cache: "no-store" });
    if (!res.ok) return null;
    return await res.json();
  } catch (e) { return null; }
}
// Reads every page of a paged list route. `pick(data)` returns the page's
// items (null = not this route's shape). Resolves null when the FIRST page
// is unreadable (route missing / down); throws when a later one is (a
// partial list must not pass for the whole).
// R5 (S11): a server with stable cursors answers `next_after` (the seq to
// continue after; null on the last page) and is then read with
// `?after=<seq>&limit` — an entry added or removed meanwhile can no longer
// shift a page under the reader. Older servers page by offset
// (`next_offset`, or `total`/`count`). The returned array carries
// `truncated: true` when the server said so (`truncated`), when fewer
// entries came back than its count, or when the page cap was hit.
// R6 U1 (S2): the cursor is opaque — "<file seq>:<index>" (a string, so a
// file holding more ids than one page is continued INSIDE that file), or
// a plain number from an older server; it goes back verbatim (URL-encoded).
// R6 U2: `unreadable` (queue files the server could not read or parse:
// [{file, error}]) is collected over every page (once per file name) and
// carried as `all.unreadable` — never dropped.
async function readPagedList(get, path, pick) {
  const all = [];
  const unreadable = new Map();
  let offset = 0;
  let after = null;
  let total = NaN;
  let truncated = false;
  let finished = false;
  for (let page = 0; page < CONFLICT_PAGES_MAX; page++) {
    const query = after != null ? `after=${encodeURIComponent(after)}&limit=${CONFLICT_PAGE}` : `offset=${offset}&limit=${CONFLICT_PAGE}`;
    const data = await get(`${path}?${query}`);
    const items = data && data.ok !== false ? pick(data) : null;
    if (!items) {
      if (page === 0) return null;
      throw new Error(`${path}: page at ${after != null ? "after " + after : offset} unreadable`);
    }
    all.push(...items);
    if (data.truncated === true) truncated = true;
    if (Array.isArray(data.unreadable)) {
      // R7 U9: a named file counts once over every page; entries WITHOUT a
      // name are not one file — the k-th nameless entry of a page is keyed
      // "#k" (the same list repeated on each page still counts once, two
      // nameless files count two).
      let nameless = 0;
      for (const u of data.unreadable) {
        const name = u && typeof u === "object" ? String(u.file || "") : String(u || "");
        const key = name ? "f:" + name : "#" + nameless++;
        if (!unreadable.has(key)) unreadable.set(key, u);
      }
    } else if (Number(data.unreadable) > 0 && !unreadable.size) {
      for (let i = 0; i < Number(data.unreadable); i++) unreadable.set("#" + i, { file: "", error: "" });
    }
    const count = Number(data.total ?? data.count);
    if (Number.isFinite(count)) total = count;
    if (Object.prototype.hasOwnProperty.call(data, "next_after")) {
      const next = data.next_after;
      if (next == null || next === "" || String(next) === String(after)) { finished = true; break; }
      after = String(next);
      continue;
    }
    // R4 B3/B6 as implemented: `next_offset` (null on the last page).
    if (Object.prototype.hasOwnProperty.call(data, "next_offset")) {
      const next = data.next_offset;
      if (next == null || !Number.isFinite(Number(next)) || Number(next) <= offset) { finished = true; break; }
      offset = Number(next);
      continue;
    }
    // Otherwise a `total` / `count`, or a server without paging (whole list).
    offset += items.length;
    if (!items.length || !Number.isFinite(count) || offset >= count || items.length > CONFLICT_PAGE) { finished = true; break; }
  }
  if (!finished || (Number.isFinite(total) && all.length < total)) truncated = true;
  if (truncated) all.truncated = true;
  if (unreadable.size) all.unreadable = [...unreadable.values()];
  return all;
}
async function loadServerConflicts(st, get = fetchJsonOrNull) {
  const mc = st && st.mark_conflicts;
  if (!mc || !(Number(mc.count) > 0)) {
    if (conflictsTruncated) noteConflictsTruncated(null); // none left on the server
    return 0;
  }
  const conflicts = await readPagedList(get, "/api/mark-conflicts", data => (Array.isArray(data.conflicts) ? data.conflicts : null));
  if (ftSignal.aborted) return 0;
  if (!conflicts) {
    // A workstation without the route: ids from /api/status, values from records-meta.
    const ids = Array.isArray(mc.ids) ? mc.ids.filter(id => typeof id === "string") : [];
    // R5 U5: an old server that has neither the route nor the ids cannot
    // ever list them — retrying forever ("unknown" for good) helps nobody.
    // Say so once (notices row) and let the state go; a route that merely
    // failed (5xx, network) still counts as "unknown" and is retried.
    if (!ids.length && await conflictRouteMissing()) {
      if (ftSignal.aborted) return 0;
      noteConflictsUnlisted(Number(mc.count) || 0);
      return 0;
    }
    return loadQueueConflicts(ids, get);
  }
  // R6 U10: a list the server cut short (or that ended below its count)
  // is said so — the conflicts not listed are real and stay on the server.
  noteConflictsTruncated(conflicts.truncated ? { listed: conflicts.length, total: Math.max(conflicts.length, Number(mc.count) || 0) } : null);
  const list = [];
  for (const c of conflicts) {
    if (!c || typeof c.id !== "string") continue;
    queueConflictsSeen.add(c.id);
    const server = normServerAnnotation(c.server);
    list.push({ id: c.id, server_revision: c.server_revision, server, mine: queuedAnnotation(c.queued, server) || localAnnotationOf(c.id) });
  }
  const n = registerConflicts(list, { queued: true });
  if (n) { renderSaveState(); emitDirty(); }
  return n;
}
// The server does not have GET /api/mark-conflicts at all (404/405/501, or
// an answer that is not that route's JSON) — not a failure of it.
async function conflictRouteMissing() {
  try {
    const res = await fetch(`/api/mark-conflicts?offset=0&limit=1`, { cache: "no-store" });
    if (res.status === 404 || res.status === 405 || res.status === 501) return true;
    if (!res.ok) return false;
    let data = null;
    try { data = await res.json(); } catch (e) { return true; }
    return !data || typeof data !== "object" || !Array.isArray(data.conflicts);
  } catch (e) { return false; }
}
// R6 U10: { listed, total } while the last conflict list read was partial.
var conflictsTruncated = null;
function noteConflictsTruncated(info) {
  conflictsTruncated = info && info.total > 0 ? info : null;
  const bar = statusNotice("conflictsTruncatedNotice", conflictsTruncated ? "conflict.truncated" : "", conflictsTruncated ? { m: conflictsTruncated.listed, n: conflictsTruncated.total } : null);
  if (bar) { bar.dataset.listed = String(conflictsTruncated.listed); bar.dataset.total = String(conflictsTruncated.total); }
}
// R6 U2: queue files the server could not read (GET /api/marks-pending
// `unreadable`): their values cannot be shown — a visible notice, never a
// silent gap (the save indicator's title says it too).
var queueUnreadableFiles = 0;
function noteQueueUnreadable(n) {
  queueUnreadableFiles = Number(n) > 0 ? Number(n) : 0;
  const bar = statusNotice("queueUnreadableNotice", queueUnreadableFiles ? "queue.unreadableFiles" : "", { n: queueUnreadableFiles });
  if (bar) bar.dataset.files = String(queueUnreadableFiles);
}
// One role=status line in the notices row (no button: it clears itself
// once the condition is gone). An empty key removes it. R8 U6: the text is
// the i18n `key` with `subs` (follows the language switch).
function statusNotice(id, key, subs) {
  const old = document.getElementById(id);
  if (!key) {
    if (old) old.remove();
    return null;
  }
  const bar = old || document.createElement("div");
  if (!old) {
    bar.id = id;
    bar.className = "ft-banner";
    bar.setAttribute("role", "status");
    bar.appendChild(noticeMessage(key, subs));
    noticeAppend(bar);
  }
  i18nText(bar.firstChild, key, subs);
  return bar;
}
var conflictsUnlistedShown = false;
function noteConflictsUnlisted(n) {
  if (conflictsUnlistedShown || document.getElementById("conflictsUnlistedNotice")) return;
  conflictsUnlistedShown = true;
  const bar = document.createElement("div");
  bar.id = "conflictsUnlistedNotice";
  bar.className = "ft-banner";
  bar.setAttribute("role", "status");
  bar.appendChild(noticeMessage("conflict.unlisted", { n }));
  bar.appendChild(noticeButton("conflictsUnlistedDismiss", "origin.dismiss", () => {
    const hadFocus = bar.contains(document.activeElement);
    bar.remove();
    if (hadFocus && typeof viewerFocus === "function") viewerFocus();
  }));
  noticeAppend(bar);
}
async function loadQueueConflicts(ids, get = fetchJsonOrNull) {
  // The status reports conflicts but names none: nothing to read them by.
  if (!(ids || []).length) throw new Error("mark conflicts unreadable");
  const wanted = ids.filter(id => !queueConflictsSeen.has(id) && !conflictIds.has(id));
  if (!wanted.length) return 0;
  const meta = await get("/api/records-meta");
  const ann = meta && meta.annotations;
  if (ftSignal.aborted) return 0;
  if (!ann || ann.error) throw new Error("records-meta unreadable");
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
// A5: one conflict lookup — the status, its queue conflicts and, after a
// queue drain (`reconcileDue`), the reconcile with records-meta. Any failure
// leaves the indicator at "unknown" ("충돌 확인 실패") and retries with
// backoff (2 s … 60 s) until it gets through.
var reconcileDue = false;
// /api/status for the conflict lookup: null when it failed (network, HTTP
// error — "unknown"), {} when this server has no such route (404, or a page
// that is not JSON — a plain file server: nothing to look up).
async function fetchLookupStatus() {
  try {
    const res = await fetch("/api/status", { cache: "no-store" });
    if (res.status === 404) return {};
    if (!res.ok) return null;
    try { return await res.json(); } catch (e) { return {}; }
  } catch (e) { return null; }
}
async function checkServerConflicts(knownStatus) {
  if (!viewerIsServed || ftSignal.aborted) return false;
  clearTimeout(conflictLookupTimer);
  conflictLookupTimer = null;
  let ok = false;
  await withConflictCheck(async () => {
    const st = knownStatus || await fetchLookupStatus();
    if (ftSignal.aborted) return;
    if (!st) throw new Error("status unreadable");
    await loadServerConflicts(st);
    if (reconcileDue) {
      if (!(await reconcileWithServer())) throw new Error("records-meta unreadable");
      reconcileDue = false;
    }
    ok = true;
  });
  if (ftSignal.aborted) return false;
  if (ok) {
    conflictLookupFailed = false;
    conflictLookupFailures = 0;
    conflictLookupRetryAt = 0;
  } else {
    conflictLookupFailed = true;
    conflictLookupFailures += 1;
    const delay = autosaveRetryDelay(conflictLookupFailures);
    conflictLookupRetryAt = Date.now() + delay;
    conflictLookupTimer = setTimeout(() => { conflictLookupTimer = null; checkServerConflicts(); }, delay);
  }
  renderSaveState();
  emitDirty();
  return ok;
}

// --- A4: the server's marks queue across a reload ------------------------
// Saves the server queued (deferred, durable) are not in records-meta until
// the job ends: a reload in between would show the old values, as if the
// edits were gone. At boot, when /api/status reports pending_marks, the
// queued values come from GET /api/marks-pending (R4 B6; paged like
// mark-conflicts: { ok, total, items: [{ id, mark|null, tags|null, deleted,
// base_revision, client_id }] }) and are shown over the server's for ids
// this window holds no draft of. The indicator says "저장 대기", the queue
// is watched until it drains (the normal reconcile then decides), leaving
// the page warns. A server without the route still gets the "저장 대기"
// state, the watch and the warning — only the values cannot be shown.
// What the id will hold once its queued body applies (the server's queue
// semantics): a deletion clears mark / note / range, a mark entry sets
// them (an omitted range or note keeps the stored one), tags replace only
// when the body carried them.
function pendingAnnotation(q, server) {
  const out = server
    ? { ...server, tags: [...(server.tags || [])] }
    : { status: null, marked_unix: null, examiner: null, note: "", range: null, tags: [] };
  if (!q || typeof q !== "object") return out;
  const mark = q.mark && typeof q.mark === "object" ? q.mark : (Object.prototype.hasOwnProperty.call(q, "status") ? q : null);
  if (q.deleted) Object.assign(out, { status: null, marked_unix: null, examiner: null, note: "", range: null });
  if (mark) {
    out.status = mark.status && mark.status !== "noted" ? mark.status : null;
    if (Number.isFinite(mark.marked_unix)) out.marked_unix = mark.marked_unix;
    if (mark.examiner) out.examiner = mark.examiner;
    if (typeof mark.note === "string") out.note = mark.note;
    if (Object.prototype.hasOwnProperty.call(mark, "range") && mark.range) out.range = normRange(mark.range);
  }
  if (Array.isArray(q.tags)) out.tags = [...q.tags];
  else if (mark && Array.isArray(mark.tags)) out.tags = [...mark.tags];
  return out;
}
async function loadServerQueue(st, get = fetchJsonOrNull) {
  const pending = Number(st && st.pending_marks) || 0;
  if (!(pending > 0)) return 0;
  queuedOnServer = Math.max(queuedOnServer, pending);
  watchServerQueue();
  let items = null;
  try {
    items = await readPagedList(get, "/api/marks-pending", data =>
      Array.isArray(data.entries) ? data.entries : Array.isArray(data.items) ? data.items : Array.isArray(data.pending) ? data.pending : null);
  } catch (e) { items = null; }
  if (ftSignal.aborted) return 0;
  // R5 U4: say when the queued values are not (all) shown — the route is
  // missing or failed ("unreadable"), or the list came back short
  // ("truncated"). The indicator's title names it; never a silent gap.
  queueValuesState = !items ? "unreadable" : items.truncated ? "truncated" : "";
  noteQueueUnreadable(items && items.unreadable ? items.unreadable.length : 0);
  let shown = 0;
  if (items) {
    const serverById = serverAnnotationMap(DATA.annotations || {});
    for (const item of items) {
      const id = item && typeof item.id === "string" ? item.id : null;
      if (!id) continue;
      serverQueuedIds.add(id);
      // This window's own unsaved edit (or open conflict) of the id is newer
      // than what was queued and goes out with its base: it stays.
      if (state.drafts[id] || conflictIds.has(id)) continue;
      const value = pendingAnnotation(item.value && typeof item.value === "object" ? item.value : item, serverById.get(id) || null);
      applyAnnotationToState(id, value);
      queueOverlayIds.add(id);
      shown += 1;
    }
    if (shown) {
      if (typeof requestRender === "function") requestRender();
      if (typeof updateRangeLabel === "function") updateRangeLabel();
    }
  }
  renderSaveState();
  emitDirty();
  return shown;
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
  // R5 U4: queued values shown over the server's go back to the case DB's
  // value now that the queue drained — also when the revision did not move
  // (the drain did not apply that body). Ids conflicted or edited here meanwhile
  // keep what they have (the conflict list / the draft decides).
  const reconciled = new Set(conflicts.map(c => c.id));
  for (const id of queueOverlayIds) {
    if (reconciled.has(id) || conflictIds.has(id) || state.drafts[id] || state.dirtyIds.has(id)) continue;
    applyAnnotationToState(id, serverById.get(id) || null);
    const rev = Number(ann.revisions[id]);
    if (Number.isFinite(rev)) state.revisions[id] = rev;
    changed = true;
  }
  queueOverlayIds.clear();
  if (conflicts.length) registerConflicts(conflicts);
  if (draftFlushKeys.size) scheduleDraftFlush();
  if (changed) {
    if (typeof requestRender === "function") requestRender();
    if (typeof updateRangeLabel === "function") updateRangeLabel();
  }
  return true;
}
async function afterQueueDrained(st) {
  await whenSaveSettled();
  reconcileDue = true;
  // Holds "saved" back (conflictChecks) until the lookup is through; a
  // failure turns the indicator "unknown" and retries (A5).
  const pending = checkServerConflicts(st);
  queuedOnServer = 0;
  queuedIds.clear();
  serverQueuedIds.clear();
  queueValuesState = "";
  // R7 U2: the queue is empty — so are its unreadable files: the notice
  // (and the indicator's note) go now, not at the next page load.
  noteQueueUnreadable(0);
  await pending;
  if (typeof requestRender === "function") requestRender();
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
  const settled = [];
  for (const [id, rev] of Object.entries(applied || {})) {
    if (Number.isFinite(rev)) state.revisions[id] = rev;
    const draft = state.drafts[id];
    if (!draft) state.dirtyIds.delete(id);
    else if (draft.seq === sentSeq.get(id)) { dropDraft(id); settled.push(id); }
    else if (Number.isFinite(rev) && !draft.conflict) { draft.base = rev; draftFlushKeys.add(id); scheduleDraftFlush(); }
  }
  if (settled.length && typeof orphanDrafts !== "undefined" && orphanDrafts.size) settleOrphanDrafts(settled);
}
// A7: our draft of these ids was just confirmed. A closed window's draft of
// the same id (held back at hydration) now either holds exactly what was
// saved — it goes — or it is a conflict right away: server = what was just
// saved, mine = the closed window's value (tagged "닫힌 창의 편집"). Its
// storage key is removed once ours (carrying the conflict) is stored.
function settleOrphanDrafts(ids) {
  const list = [];
  for (const id of ids) {
    const orphan = orphanDrafts.get(id);
    if (!orphan) continue;
    orphanDrafts.delete(id);
    if (draftSameValues(orphan.draft, draftValuesOf(id))) {
      try { localStorage.removeItem(orphan.key); } catch (error) { /* retried next boot */ }
      continue;
    }
    const d = orphan.draft;
    list.push({
      id,
      server_revision: Number.isFinite(state.revisions[id]) ? state.revisions[id] : 0,
      server: localAnnotationOf(id),
      mine: {
        status: d.mark && d.mark.status ? d.mark.status : null,
        marked_unix: d.mark && Number.isFinite(d.mark.marked_unix) ? d.mark.marked_unix : null,
        examiner: d.examiner ?? null, note: d.note || "", range: normRange(d.range),
        tags: Array.isArray(d.tags) ? [...d.tags] : []
      },
      orphan: true
    });
    draftAdoptedFrom.set(id, orphan.key);
  }
  if (list.length) registerConflicts(list);
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

// Folds one send outcome into the save's running result (first one as is).
function mergeSaveResult(result, r) {
  if (!result) return r;
  Object.assign(result.applied, r.applied || {});
  result.conflicts.push(...(r.conflicts || []));
  result.queuedIds.push(...(r.queuedIds || []));
  result.posts += r.posts || 0;
  result.deferred = result.deferred || !!r.deferred;
  result.pending = Math.max(result.pending || 0, r.pending || 0);
  if (r.maxIds) result.maxIds = r.maxIds;
  if (r.legacy) result.legacy = r.legacy;
  if (r.reportError) result.reportError = r.reportError;
  if (!r.ok) Object.assign(result, { ok: false, error: r.error, status: r.status, offline: r.offline, busy: r.busy, retryAfterMs: r.retryAfterMs });
  return result;
}

// R5 U1: the ids whose draft is `unsafe_base` (base_revisions -1) travel in
// a body of their own, so a server that refuses that body (no support for
// -1 yet: track P) never takes the ordinary edits down with it.
//   -> [{ body, unsafe }]   (the unsafe group first, then the rest if any)
function splitUnsafeBase(payload) {
  const unsafe = new Set(Object.keys(payload.base_revisions || {}).filter(id => payload.base_revisions[id] === -1));
  if (!unsafe.size) return [{ body: payload, unsafe: false }];
  const pick = keep => ({
    ...payload,
    marks: payload.marks.filter(m => unsafe.has(m.id) === keep),
    tags: (payload.tags || []).filter(e => unsafe.has(e.id) === keep),
    deleted_ids: (payload.deleted_ids || []).filter(id => unsafe.has(id) === keep),
    base_revisions: Object.fromEntries(Object.entries(payload.base_revisions).filter(([id]) => unsafe.has(id) === keep))
  });
  const out = [{ body: pick(true), unsafe: true }];
  const rest = pick(false);
  if (!payloadEmpty(rest)) out.push({ body: rest, unsafe: false });
  return out;
}
// A server that predates base -1 refuses the body while parsing it
// ("base_revisions[…] must be a non-negative integer"; nothing applied).
function unsafeBaseRefused(r) {
  return !!r && !r.ok && (!!r.legacyRefused || /base_revisions/.test(String(r.error || "")));
}
// Sends the unsafe group; where the server cannot take base -1 (an older
// build, or a patch-v1 workstation) the same outcome is produced here: each
// id becomes a conflict against the case's current value (records-meta) —
// the examiner picks a side, nothing is written meanwhile.
async function sendUnsafeGroup(body) {
  const ids = payloadIds(body);
  if (saveLegacyProtocol) return holdUnsafeLocally(ids);
  let result = null;
  for (const part of splitByExaminer(body)) {
    const r = await sendPayloadChunks(part, pushMarksToCase, { maxIds: saveChunkIds, noLegacy: true });
    if (unsafeBaseRefused(r)) {
      const done = new Set([...Object.keys((result && result.applied) || {}), ...((result && result.conflicts) || []).map(c => c.id)]);
      return mergeSaveResult(result, await holdUnsafeLocally(ids.filter(id => !done.has(id))));
    }
    result = mergeSaveResult(result, r);
    if (r.maxIds) saveChunkIds = r.maxIds;
    if (!r.ok) break;
  }
  return result;
}
async function holdUnsafeLocally(ids) {
  const out = { ok: true, error: "", status: 0, applied: {}, conflicts: [], queuedIds: [], deferred: false, pending: 0, posts: 0 };
  if (!ids.length) return out;
  const meta = await fetchJsonOrNull("/api/records-meta");
  const ann = meta && meta.annotations;
  if (!ann || ann.error) return { ...out, ok: false, error: t("save.unsafeHoldFail") };
  const serverById = serverAnnotationMap(ann);
  for (const id of ids) out.conflicts.push({ id, server_revision: Number((ann.revisions || {})[id]) || 0, server: serverById.get(id) || null });
  return out;
}

async function pushMarksToCase(payload) {
  // R8 U2: every request sent and every reply received is a sign of life
  // of this window's save (viewerSave's patient deadline follows it).
  saveProgressAt = Date.now();
  let res;
  try {
    res = await fetch("/api/import-marks", {
      method: "POST",
      headers: { "Content-Type": "text/plain" },
      body: JSON.stringify({ marks_json: JSON.stringify(payload) })
    });
  } finally {
    saveProgressAt = Date.now();
  }
  let data = null;
  try { data = await res.json(); } catch { data = null; }
  saveProgressAt = Date.now();
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
  // A3: the case's annotations were not readable — never write over them.
  if (annotationsReadOnly) { renderSaveState(); return { ok: false, readonly: true, error: t("save.readonly") }; }
  if (saveInFlight) {
    saveQueued = true;
    return { ok: false, queued: true };
  }
  const payload = marksPayload();
  if (payloadEmpty(payload)) {
    pendingReviewSync = false;
    saveFailures = 0;
    saveError = "";
    saveAuthExpired = false; // nothing left to send: nothing is being refused
    renderSaveState();
    emitDirty();
    return { ok: true, empty: true };
  }
  const sentSeq = new Map(payloadIds(payload).map(id => [id, state.drafts[id] ? state.drafts[id].seq : undefined]));
  // A marks entry may create the row (whatever the reply says, it may land).
  for (const m of payload.marks) markRowMaybe.add(m.id);
  saveInFlight = true;
  clearTimeout(autosaveTimer);
  autosaveTimer = null;
  renderSaveState();
  let result;
  try {
    for (const group of splitUnsafeBase(payload)) {
      if (group.unsafe) {
        // R5 U1: base -1 drafts in a body of their own, never downgraded.
        // R6 U9: its failure stays its own — the ordinary edits are still
        // sent (independently); the unsafe ids stay dirty and are retried.
        result = mergeSaveResult(result, await sendUnsafeGroup(group.body));
        continue;
      }
      for (const part of splitByExaminer(group.body)) {
        const r = await sendPayloadChunks(part, pushMarksToCase, { maxIds: saveChunkIds, legacy: saveLegacyProtocol });
        result = mergeSaveResult(result, r);
        if (r.maxIds) saveChunkIds = r.maxIds;
        if (r.legacy) saveLegacyProtocol = true;
        if (!r.ok) break;
      }
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
  // A deletion the server applied leaves no row behind.
  for (const id of payload.deleted_ids) if (result.applied && Object.prototype.hasOwnProperty.call(result.applied, id) && !state.marks[id]) markRowMaybe.delete(id);
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
    saveAuthExpired = false;
    saveRetryAt = 0;
    // Edits made while the request was in flight (or ids the server
    // skipped) are still dirty: push again instead of forgetting them.
    saveBusyRetries = 0;
    pendingReviewSync = state.dirtyIds.size > 0;
    if (pendingReviewSync) scheduleAutosave(AUTOSAVE_DEBOUNCE_MS);
    // R6 U10: the listed conflicts are all decided — read the rest.
    if (conflictsTruncated && !conflictIds.size && typeof checkServerConflicts === "function") checkServerConflicts();
    // R4 B7: saved, but the case report was not refreshed — say so once per text.
    if (result.reportError && result.reportError !== lastReportError) {
      lastReportError = result.reportError;
      const firstLine = result.reportError.split(String.fromCharCode(10))[0].trim();
      toast(tf("save.reportError", { err: firstLine.slice(0, 200) }), { error: true });
    }
  } else if (result.busy && saveBusyRetries < SAVE_BUSY_RETRIES) {
    // R4 B4: "briefly busy, nothing queued" — transient: the edits stay
    // dirty ("미저장", never "saved"), retried after retry_after_ms without
    // counting as a failure (a long busy spell becomes one after a few tries).
    saveBusyRetries += 1;
    pendingReviewSync = true;
    scheduleAutosave(result.retryAfterMs || 1000);
  } else {
    saveBusyRetries = 0;
    const firstFailure = saveFailures === 0;
    saveFailures += 1;
    // 401: the server is up but refused this browser's proof (ft_token /
    // ft_nonce cookie missing or stale, U8) — not "server down". The edits
    // stay in the drafts and the retry keeps running (a page reload that
    // re-plants the cookie lets the next attempt through).
    const auth = Number(result.status) === 401;
    const authChanged = auth !== saveAuthExpired;
    saveAuthExpired = auth;
    saveError = auth ? t("save.authExpired") : String(result.error || "?");
    const delay = autosaveRetryDelay(saveFailures);
    saveRetryAt = Date.now() + delay;
    scheduleAutosave(delay);
    if (auth) {
      // Toast once; the host hears every refused retry (repeat: true), so
      // its banner stays up for as long as saves keep failing even if it
      // was cleared meanwhile (U8).
      if (authChanged || firstFailure) toastError(saveError);
      FTViewer._emit("error", { message: saveError, detail: String(result.error || "HTTP 401"), stage: "save", auth: true, repeat: !(authChanged || firstFailure) });
    } else {
      if (firstFailure) toast(tf("toast.saveFailed", { err: saveError }));
      if (firstFailure) FTViewer._emit("error", { message: tf("toast.saveFailed", { err: saveError }), detail: saveError, stage: "save" });
    }
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
  // A4: marks still in the server's queue (not yet in the case DB) and an
  // unread conflict lookup also warn before the page goes.
  if (!pendingReviewSync && !state.dirtyIds.size && !conflictIds.size && !saveInFlight && !draftsUnpersisted &&
    !(queuedOnServer > 0) && !conflictLookupFailed && !(annotationsReadOnly && Object.keys(state.drafts).length)) return;
  event.preventDefault();
  event.returnValue = "";
}, { signal: ftSignal });

if (saveStateEl) {
  // Clicking the indicator saves (or retries) right away.
  saveStateEl.addEventListener("click", () => {
    if (!viewerIsServed) { toast(t("save.title.local")); return; }
    if (annotationsReadOnly) { toast(t(lockKey("save.readonlyMsg"))); return; }
    if (conflictLookupFailed && !saveInFlight) { checkServerConflicts(); return; }
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
  noteOriginDrafts(); // U9: keep this port's marker while drafts exist
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
    "warnings", "note", "anomalies", "audio_state", "audio_max_db", "audio_mean_db", "audio_trust", "status_trust"];
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
    recTimeText(record, { iso: true }),
    record.channel || "",
    record.recType || "",
    record.inode ?? "",
    record.originalPath || "",
    record.path || "",
    (record.warnings || []).join("; "),
    record.note && record.note !== "-" ? record.note : "",
    (record.anomalies || []).map(item => item.kind).join("; "),
    ...Object.values(audioExportFields(record)),
    record.statusTrust || ""
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
      `<td>${fmtSize(record.size)}</td><td>${e(recTimeText(record))}</td>` +
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
<div class="meta">${e(tf("report.meta", { case: manifest.case_id || "-", when: (p => `${p.date} ${p.time} UTC${p.offset}`)(recTimeParts(Date.now() / 1000)), n: items.length }))}</div>
<div class="meta">${tf("report.counts", { v: counts.verified, c: counts.candidate, f: counts.failed })}</div>
<div class="note">${t("report.note")}</div>
<table><thead><tr><th>ID</th><th>${t("report.th.name")}</th><th>${t("report.th.kind")}</th><th>${t("report.th.status")}</th><th>${t("report.th.mark")}</th><th>${t("report.th.tags")}</th>
<th>SHA-256</th><th>${t("report.th.size")}</th><th>${t("report.th.time")}</th><th>${t("report.th.warn")}</th><th>${t("report.th.orig")}</th></tr></thead>
<tbody>
${rowHtml}
</tbody></table>
</body></html>`;
  downloadText(`frametrace-summary-${safeFileName(manifest.case_id || "case")}.html`, html, "text/html;charset=utf-8");
});

// 서버 측 자료 묶음 — 선택 파일을 exports/selection-*/ 에 해시 매니페스트와 함께 복사.
document.getElementById("btnExportSelected").addEventListener("click", async () => {
  const selected = selectedRecords();
  if (!selected.length) { toast(t("toast.selectFirst")); return; }
  const btn = document.getElementById("btnExportSelected");
  await withBusy(btn, async () => {
    const res = await viewerPost("/api/export-selected", { items: selected.map(exportItem) }, { contentType: "application/json" });
    if (!res.ok) {
      toast(tf("toast.exportFail", { err: res.error || "" }));
      return;
    }
    toast(tf("toast.exportDone", { copied: res.copied, skipped: res.skipped, dir: res.export_dir }));
    viewerPost("/api/open-folder", { path: res.export_dir }, { contentType: "application/json" }).catch(() => {});
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
    const data = await viewerPost("/api/transcode-queue", { ids });
    if (data.ok && data.summary) {
      toast(tf("toast.queueDone", { t: data.summary.total ?? 0, p: data.summary.proxied ?? 0, c: data.summary.skipped_existing ?? 0, f: data.summary.failed ?? 0 }));
    } else {
      toast(tf("toast.queueFail", { err: data.error || "" }));
    }
  }, { start: "toast.queueRunning", fail: () => t("toast.queueOffline") });
});

// R12 (track I2): a carve event taken only from db/carve_results.json
// (`carved-candidate-unaudited`) is not a candidate — warn badge
// "후보(감사 안 됨)", never the plain kind word. Other kinds as recorded.
// R14 U5 (track I8): an event whose time was not audited (`time_unaudited:
// true`, or the kind `…-time-unaudited`) gets a warn badge "시각 감사 안 됨".
// `group` (R15, track I8 — index / ffprobe events are all unaudited): the
// badge on the first row of a run of such rows only — n > 1 says how many,
// 0 = a later row of the run (no badge; the row tooltip says it).
// Absent = one badge per row.
function timelineKindHtml(kind, ev, group) {
  let k = String(kind || "");
  if (k === "carved-candidate-unaudited") {
    return `<span class="badge warn tl-unaudited" data-kind="${escapeHtml(k)}" title="${escapeHtml(t("timeline.tip.carvedUnaudited"))}">${escapeHtml(t("timeline.kind.carvedUnaudited"))}</span>`;
  }
  const timeUnaudited = timelineTimeUnaudited(ev, k);
  if (/-time-unaudited$/.test(k)) k = k.slice(0, -"-time-unaudited".length);
  if (!timeUnaudited || group === 0) return escapeHtml(k);
  const label = Number(group) > 1 ? tf("timeline.badge.timeUnauditedN", { n: Number(group) }) : t("timeline.badge.timeUnaudited");
  return escapeHtml(k) + ` <span class="badge warn tl-time-unaudited" title="${escapeHtml(t("timeline.tip.timeUnaudited"))}">${escapeHtml(label)}</span>`;
}
// Track I (3e002d8): every event carries `time_unaudited: boolean` — the
// field decides; the kind suffix only when the field is absent.
function timelineTimeUnaudited(ev, kind) {
  const k = String(kind == null ? (ev && ev.kind) || "" : kind);
  return ev && typeof ev.time_unaudited === "boolean" ? ev.time_unaudited : /-time-unaudited$/.test(k);
}
// Per event: n (> 0) = first of n consecutive unaudited events of the same
// source and kind, 0 = a later one of that run, undefined = audited.
function timelineUnauditedGroups(events) {
  const out = new Array(events.length);
  const key = ev => String(ev && ev.source || "") + "\u0000" + String(ev && ev.kind || "").replace(/-time-unaudited$/, "");
  let head = -1;
  events.forEach((ev, i) => {
    if (!timelineTimeUnaudited(ev)) { head = -1; return; }
    if (head >= 0 && key(events[head]) === key(ev)) { out[i] = 0; out[head] += 1; return; }
    head = i;
    out[i] = 1;
  });
  return out;
}
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
      const data = await viewerPost("/api/advanced", { tool: "timeline" });
      if (!data.ok) { meta.textContent = tf("timeline.fail", { err: data.error || "" }); return; }
    } catch (err) { meta.textContent = err && err.ftAuth ? err.message : t("timeline.failOffline"); return; }
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
    const shown = events.slice(0, cap);
    const groups = timelineUnauditedGroups(shown);
    list.innerHTML = shown.map((ev, i) => {
      const recId = byPath.get(ev.path);
      const tip = `${ev.path || ""} ${ev.detail || ""}` + (groups[i] === 0 ? "\n" + t("timeline.tip.timeUnaudited") : "");
      return `<div class="timeline-row" data-id="${escapeHtml(recId || "")}">
        <span class="ts">${escapeHtml(isoTime(ev.ts_unix))}</span>
        <span class="src">${escapeHtml(ev.source || "")}</span>
        <span class="desc" title="${escapeHtml(tip)}">${timelineKindHtml(ev.kind, ev, groups[i])} —${escapeHtml((ev.path || "").split(/[\\/]/).pop() || ev.path || "")}</span>
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
if (viewerIsServed && !annotationsReadOnly) {
  // Conflicts the server recorded while draining its marks queue (possibly
  // before a restart) are shown on boot, and "saved" waits for that answer
  // (A5: an unreadable answer is "unknown" and retried, never "saved").
  // Marks still in the server's queue are shown with their queued values (A4).
  // withConflictCheck from the first moment: "saved" is withheld until then.
  withConflictCheck(async () => {
    const st = await fetchLookupStatus();
    if (ftSignal.aborted) return;
    if (st) await loadServerQueue(st);
    await checkServerConflicts(st || null);
  });
}
renderSaveState();

state.pageSize = Number(els.pageSize.value) || 100;


// --- Range text + boot notices (A2/A3) -------------------------------------
function rangeText(range) {
  const r = normRange(range);
  const p = v => Number.isFinite(v) ? `${v.toFixed(1)}s` : "—";
  return r ? `${p(r.in)} ~ ${p(r.out)}` : t("range.none");
}

// A2 (standalone, file://): ranges an older page kept in this browser's
// whole-map store (RANGES_KEY) are never applied over this package's data —
// the bundle is the reference. They are only reported here, with a way to
// keep them (download as JSON) and to drop them; nothing is removed unasked.
(function showLegacyRangesNotice() {
  if (!legacyRangesLeft || !legacyRangesLeft.count) return;
  const left = legacyRangesLeft;
  const bar = document.createElement("div");
  bar.id = "legacyRangesNotice";
  bar.className = "ft-banner";
  bar.setAttribute("role", "status");
  const msg = noticeMessage("range.legacyMsg", { n: left.count });
  msg.title = Object.entries(left.ranges).slice(0, 20).map(([id, r]) => `${recordLabelOf(id)}: ${rangeText(r)}`).join("\n");
  bar.appendChild(msg);
  const done = () => {
    const hadFocus = bar.contains(document.activeElement);
    bar.remove();
    if (hadFocus && typeof viewerFocus === "function") viewerFocus();
  };
  bar.appendChild(noticeButton("legacyRangesSave", "range.legacySave", () => {
    downloadJSON(`frametrace-browser-ranges-${safeFileName(manifest.case_id || "case")}.json`, { case_id: manifest.case_id || null, ranges: left.ranges });
  }));
  bar.appendChild(noticeButton("legacyRangesDrop", "range.legacyDrop", () => {
    try { localStorage.removeItem(RANGES_KEY); } catch (e) { /* storage blocked */ }
    legacyRangesLeft = null;
    done();
  }));
  noticeAppend(bar);
})();

// A3: records-meta could not read the case's annotations. The marks, notes,
// tags and ranges shown are NOT the case's: saving is blocked (a save would
// write over records this window never saw), the summary report and CSV
// are off (they would claim "no marks"), and the shell's finalize blocks on
// the "readonly" save state. Edits made meanwhile stay in this browser's
// drafts and go out with their base once the annotations read again.
const ANNOTATION_GATED_CONTROLS = ["btnSummary", "btnDownloadCsv", "btnDownloadMarks", "btnApplyMarks"];
// R5 U1: the edit controls too — an edit made now would be built on values
// this window never saw (refuseLockedEdit also stops the keys 1/2/3/0/i/o,
// Ctrl+Z/Y, the tag menu, the note field and the pop-out player).
const ANNOTATION_EDIT_CONTROLS = ["btnMarkReviewed", "btnMarkImportant", "btnMarkVerify", "btnMarkClear", "btnTagMenu", "btnSetIn", "btnSetOut"];
(function showAnnotationsReadOnly() {
  if (!annotationsReadOnly) return;
  const gate = (id, key) => {
    const btn = document.getElementById(id);
    if (!btn) return;
    btn.disabled = true;
    btn.setAttribute("aria-disabled", "true");
    btn.dataset.i18nTitle = key;
    btn.title = t(key);
  };
  for (const id of ANNOTATION_GATED_CONTROLS) gate(id, lockKey("save.readonlyMsg"));
  for (const id of ANNOTATION_EDIT_CONTROLS) gate(id, lockKey("save.readonlyEdit"));
  // R8 U1: locked because case.json was not read — the case-input banner
  // (below, role=alert) says so and that editing is blocked; one alert.
  if (annotationsLockCause === "case") return;
  const bar = document.createElement("div");
  bar.id = "annotationsErrorBanner";
  bar.className = "ft-banner";
  bar.setAttribute("role", "alert");
  const msg = noticeMessage("save.readonlyMsg");
  msg.title = annotationsError;
  bar.appendChild(msg);
  const host = noticesHost();
  host.insertBefore(bar, host.firstChild);
  FTViewer._emit("error", { message: t("save.readonlyMsg"), detail: annotationsError, stage: "load" });
})();

// R7 (S ↔ U): case.json or the evidence index was not valid JSON — the data
// bundle / records-meta carry null for it (10-boot.js caseInputProblems).
// Never an unexplained empty case: one role=alert banner naming what was
// not read, the same words as the review / carve pages.
(function showCaseInputProblems() {
  if (!caseInputProblems.length) return;
  // R8 U6: each piece carries its i18n key (the language switch re-renders
  // it); R8 U1: a case.json that was not read also locks editing — said here.
  const keys = caseInputProblems.map(what => what === "scan" ? "caseInput.scan" : "caseInput.manifest");
  if (annotationsLockCause === "case") keys.push("caseInput.readonly");
  const bar = document.createElement("div");
  bar.id = "caseInputErrorBanner";
  bar.className = "ft-banner";
  bar.setAttribute("role", "alert");
  bar.dataset.problems = caseInputProblems.join(" ");
  const msg = document.createElement("span");
  msg.className = "ft-banner-msg";
  const piece = (key) => { const s = document.createElement("span"); i18nText(s, key); msg.appendChild(s); };
  piece("caseInput.title");
  keys.forEach((key, i) => { msg.appendChild(document.createTextNode(i ? " · " : " — ")); piece(key); });
  const text = msg.textContent;
  bar.appendChild(msg);
  const host = noticesHost();
  host.insertBefore(bar, host.firstChild);
  FTViewer._emit("error", { message: text, detail: caseInputProblems.join(","), stage: "load" });
})();

// --- U9: drafts left under another origin ---------------------------------
// localStorage is per origin (scheme + host + PORT). When the workstation
// comes up on another port (browser mode falls back from 8477 to 8478…;
// the WebView2 window takes a fresh port per launch), drafts written by an
// earlier window are invisible here — and no browser API can read another
// origin's storage. Cookies, unlike storage, are shared by every port of a
// host, so each window keeps one small marker per (port, case) in a cookie
// while this origin holds drafts of the case: `ft_draft_ports=<port>.<case
// hash>-…` (no case id or path, only an FNV-1a hash; SameSite=Strict, not
// HttpOnly, never read by the server). A boot that finds a marker of the
// same case under ANOTHER port says so once in the notices row: the drafts
// may still be there, reachable only by reopening that address. Nothing is
// claimed beyond "may": the marker cannot see whether that storage was
// cleared since. Limitation (docs/ui-rebuild-contract.md §11.4): only
// windows of this build write markers, and only on the same host name
// (127.0.0.1 and localhost are different hosts).
// `var`, not const: flushDraftsNow (30-records) may call noteOriginDrafts
// during hydration, before this part of the bundle has run.
var ORIGIN_COOKIE = "ft_draft_ports";
var ORIGIN_COOKIE_MAX = 24;
var ORIGIN_COOKIE_AGE = 60 * 60 * 24 * 60; // 60 days
function originCaseHash() {
  const text = String(CASE_KEY) + "|" + String(DATA.caseDir || "");
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) { h ^= text.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; }
  return h.toString(16).padStart(8, "0");
}
function originPort() {
  return String(location.port || (location.protocol === "https:" ? "443" : "80"));
}
function readOriginMarks() {
  try {
    const pair = String(document.cookie || "").split(";").map(p => p.trim()).find(p => p.startsWith(ORIGIN_COOKIE + "="));
    if (!pair) return [];
    return pair.slice(ORIGIN_COOKIE.length + 1).split("-").filter(m => /^\d{1,5}\.[0-9a-f]{8}$/.test(m));
  } catch (e) { return []; }
}
function writeOriginMarks(marks) {
  try {
    const list = [...new Set(marks)].slice(-ORIGIN_COOKIE_MAX);
    document.cookie = list.length
      ? `${ORIGIN_COOKIE}=${list.join("-")}; Path=/; Max-Age=${ORIGIN_COOKIE_AGE}; SameSite=Strict`
      : `${ORIGIN_COOKIE}=; Path=/; Max-Age=0; SameSite=Strict`;
  } catch (e) { /* cookies blocked: no cross-port hint */ }
}
// Any stored draft of this case under THIS origin (any writer window).
function originHasStoredDrafts() {
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.startsWith(DRAFT_PREFIX)) return true;
    }
  } catch (e) { /* storage blocked */ }
  return false;
}
var originMarked = null; // last marker state this window wrote (null = unknown)
// Called after every draft flush (30-records flushDraftsNow) and on the
// heartbeat tick. While drafts exist the cookie is re-read every time and
// the marker rewritten if it went missing — another port's window may have
// dismissed its notice (dropping our marker) or rewritten the list
// concurrently. Only "no drafts, already unmarked" is skipped.
// The scan uses the drafts namespace of this case (DRAFT_PREFIX, keyed by
// case id — the same keys a reopened window on that port would restore);
// the marker's hash adds the case folder so two cases sharing an id on
// different folders never point at each other's port.
function noteOriginDrafts() {
  if (!viewerIsServed || typeof ORIGIN_COOKIE !== "string") return; // not initialised yet
  let mine = false;
  for (const id in state.drafts) { mine = true; break; }
  const has = mine || originHasStoredDrafts();
  if (!has && originMarked === false) return;
  originMarked = has;
  const me = originPort() + "." + originCaseHash();
  const marks = readOriginMarks();
  const present = marks.includes(me);
  if (has && !present) writeOriginMarks([...marks, me]);
  else if (!has && present) writeOriginMarks(marks.filter(m => m !== me));
}
(function checkOtherOriginDrafts() {
  if (!viewerIsServed) return;
  const hash = originCaseHash();
  const me = originPort() + "." + hash;
  noteOriginDrafts(); // also drops a stale marker of this origin
  const others = readOriginMarks().filter(m => m.endsWith("." + hash) && m !== me);
  if (!others.length) return;
  const where = others.map(m => `${location.hostname}:${m.split(".")[0]}`).join(", ");
  const bar = document.createElement("div");
  bar.id = "originDraftsNotice";
  bar.className = "ft-banner";
  bar.setAttribute("role", "status");
  bar.appendChild(noticeMessage("origin.draftsMsg", { where }));
  bar.appendChild(noticeButton("originDraftsDismiss", "origin.dismiss", () => {
    const hadFocus = bar.contains(document.activeElement);
    writeOriginMarks(readOriginMarks().filter(m => !others.includes(m)));
    bar.remove();
    if (hadFocus && typeof viewerFocus === "function") viewerFocus();
  }));
  noticeAppend(bar);
})();

// R9 U7: drafts an earlier build kept under the literal case key "case"
// (a case.json without case_id, or one it could not read) may belong to
// ANY such case: never adopted (R8 U1). Said once per browser (and again
// only when their number changes): how many, and that they were not
// applied. Nothing is deleted.
var LEGACY_CASE_DRAFTS_SEEN = "ft.viewer.legacyCaseDrafts.noticed";
function legacyCaseDraftCount() {
  let n = 0;
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.startsWith("ft.draft.case.")) n += 1;
    }
    const raw = localStorage.getItem("ft.viewer.case.annotations.v1");
    const old = raw ? JSON.parse(raw) : null;
    if (old && typeof old === "object") n += Object.keys(old).length;
  } catch (e) { /* storage blocked or unreadable: nothing to say */ }
  return n;
}
(function showLegacyCaseDrafts() {
  if (CASE_KEY === "case") return; // a case whose case_id really is "case": they are its own
  const n = legacyCaseDraftCount();
  if (!n) return;
  try {
    if (localStorage.getItem(LEGACY_CASE_DRAFTS_SEEN) === String(n)) return;
    localStorage.setItem(LEGACY_CASE_DRAFTS_SEEN, String(n));
  } catch (e) { /* storage blocked: say it on every mount rather than never */ }
  const bar = document.createElement("div");
  bar.id = "legacyCaseDraftsNotice";
  bar.className = "ft-banner";
  bar.setAttribute("role", "status");
  bar.appendChild(noticeMessage("legacyDrafts.msg", { n }));
  bar.appendChild(noticeButton("legacyCaseDraftsDismiss", "legacyDrafts.dismiss", () => {
    const hadFocus = bar.contains(document.activeElement);
    bar.remove();
    if (hadFocus && typeof viewerFocus === "function") viewerFocus();
  }));
  noticeAppend(bar);
})();
