function tagListFor(record) { return state.tags[record.id] || []; }

function saveTagPresets() { storageSet(TAG_PRESETS_KEY, state.tagPresets); }
// renderDetails/openPlayerWindow call tagPresets() for the preset list —
// the array itself lives on state (initialized with DEFAULT_TAG_PRESETS).
function tagPresets() { return state.tagPresets || DEFAULT_TAG_PRESETS; }
function addTagPreset(tag) {
  const name = String(tag || "").trim();
  if (!name || state.tagPresets.includes(name)) return;
  state.tagPresets.push(name);
  saveTagPresets();
}
function removeTagPreset(tag) {
  const idx = state.tagPresets.indexOf(tag);
  if (idx < 0) return;
  state.tagPresets.splice(idx, 1);
  saveTagPresets();
}

// A14: tag lists are replaced, never edited in place — a list may still be
// shared with a snapshot (a draft, a conflict's server value, the loaded
// case data) that must keep its own value.
function addTag(id, tag) {
  if (refuseLockedEdit()) return; // R5 U1
  const list = [...(state.tags[id] || [])];
  if (!list.includes(tag)) list.push(tag);
  state.tags[id] = list;
  touchAnnotation(id);
}

function removeTag(id, tag) {
  if (refuseLockedEdit()) return; // R5 U1
  const list = [...(state.tags[id] || [])];
  const idx = list.indexOf(tag);
  if (idx >= 0) list.splice(idx, 1);
  state.tags[id] = list;
  touchAnnotation(id);
}

function toggleTag(id, tag) {
  const list = state.tags[id] || [];
  if (list.includes(tag)) removeTag(id, tag);
  else addTag(id, tag);
}

const MARK_ICON = { reviewed: "check", important: "flag", needs_verification: "question" };
// Mark chip: shape + colour + text (reviewed = grey check, important = amber
// flag, needs verification = purple question mark).
function markChipHtml(status) {
  return `<span class="mark-chip ${escapeHtml(status)}">${ftIcon(MARK_ICON[status] || "check")}${escapeHtml(markLabel(status))}</span>`;
}
function markLabel(status) {
  if (status === "reviewed") return t("mark.reviewed");
  if (status === "important") return t("mark.important");
  if (status === "needs_verification") return t("mark.verify");
  if (status === "noted") return t("mark.noted");
  return status;
}

function indexOfFiltered(id) {
  return indexInList(filteredRecords(), id);
}

function selectRange(fromId, toId, checked) {
  const filtered = filteredRecords();
  const from = indexInList(filtered, fromId);
  const to = indexInList(filtered, toId);
  if (from < 0 || to < 0) return;
  const [low, high] = from <= to ? [from, to] : [to, from];
  for (let index = low; index <= high; index += 1) {
    if (checked) state.selectedIds.add(filtered[index].id);
    else state.selectedIds.delete(filtered[index].id);
  }
}

function selectAllFiltered() {
  filteredRecords().forEach(record => state.selectedIds.add(record.id));
  refreshSelectionUi();
  toast(tf("toast.selectedAll", { n: state.selectedIds.size }));
}

function clearSelection() {
  state.selectedIds.clear();
  refreshSelectionUi();
}

function targetIds() {
  if (state.selectedIds.size) return [...state.selectedIds];
  return state.activeId ? [state.activeId] : [];
}

function applyMark(status) {
  if (refuseLockedEdit()) return; // R5 U1
  const ids = targetIds();
  if (!ids.length) { toast(t("toast.selectTarget")); return; }
  const stamped = Math.floor(Date.now() / 1000);
  ids.forEach(id => {
    if (status === null) delete state.marks[id];
    else state.marks[id] = { status, marked_unix: stamped };
    // touchAnnotation must run after the mutation — it snapshots the
    // resulting state into the draft that hydration replays on reload.
    touchAnnotation(id);
  });
  requestRender();
  toast(tf("toast.markApplied", { n: ids.length, status: status === null ? t("mark.cleared") : markLabel(status) }));
}
