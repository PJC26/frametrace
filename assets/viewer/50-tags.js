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

function addTag(id, tag) {
  const list = state.tags[id] || [];
  if (!list.includes(tag)) list.push(tag);
  state.tags[id] = list;
  touchAnnotation(id);
  storageSet(TAGS_KEY, state.tags);
}

function removeTag(id, tag) {
  const list = state.tags[id] || [];
  const idx = list.indexOf(tag);
  if (idx >= 0) list.splice(idx, 1);
  state.tags[id] = list;
  touchAnnotation(id);
  storageSet(TAGS_KEY, state.tags);
}

function toggleTag(id, tag) {
  const list = state.tags[id] || [];
  if (list.includes(tag)) removeTag(id, tag);
  else addTag(id, tag);
}

function markLabel(status) {
  if (status === "reviewed") return t("mark.reviewed");
  if (status === "important") return t("mark.important");
  if (status === "needs_verification") return t("mark.verify");
  if (status === "noted") return t("mark.noted");
  return status;
}

function indexOfFiltered(id) {
  return filteredRecords().findIndex(record => record.id === id);
}

function selectRange(fromId, toId, checked) {
  const filtered = filteredRecords();
  const from = filtered.findIndex(record => record.id === fromId);
  const to = filtered.findIndex(record => record.id === toId);
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
  storageSet(MARKS_KEY, state.marks);
  requestRender();
  toast(tf("toast.markApplied", { n: ids.length, status: status === null ? t("mark.cleared") : markLabel(status) }));
}
