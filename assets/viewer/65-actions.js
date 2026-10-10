function setupGridDelegation() {
  if (els.recordGrid._delegated) return;
  els.recordGrid._delegated = true;
  let shiftRange = false;
  els.recordGrid.addEventListener("click", event => {
    const header = event.target.closest(".group-header");
    if (header) {
      const key = header.dataset.group;
      if (state.collapsedGroups.has(key)) state.collapsedGroups.delete(key);
      else state.collapsedGroups.add(key);
      renderGrid(filteredRecords());
      return;
    }
    if (event.target.closest("input[type='checkbox']")) return;
    const card = event.target.closest(".card");
    if (!card) return;
    // PERF-03: activating a visible card only swaps the active class and
    // refreshes the detail pane — no filter/sort or grid rebuild.
    setActiveCard(card.dataset.id);
  });
  els.recordGrid.addEventListener("keydown", event => {
    if (event.target.closest("input")) return;
    const header = event.target.closest(".group-header");
    if (header && (event.key === "Enter" || event.key === " ")) {
      event.preventDefault();
      event.stopPropagation();
      const key = header.dataset.group;
      if (state.collapsedGroups.has(key)) state.collapsedGroups.delete(key);
      else state.collapsedGroups.add(key);
      renderGrid(filteredRecords());
      return;
    }
    const card = event.target.closest(".card");
    if (!card) return;
    if (event.key === "Enter") {
      event.stopPropagation();
      setActiveCard(card.dataset.id);
      return;
    }
    // Space is not handled here: it falls through to the global handler
    // (play/pause), matching the shortcut sheet; x toggles selection.
    const step = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 }[event.key];
    if (step) {
      event.preventDefault();
      event.stopPropagation();
      state.activeId = card.dataset.id;
      moveActive(step);
      els.recordGrid.querySelector(`.card[data-id="${CSS.escape(state.activeId)}"]`)?.focus();
    }
  });
  els.recordGrid.addEventListener("click", event => {
    const box = event.target.closest("input[type='checkbox']");
    if (!box) return;
    event.stopPropagation();
    shiftRange = event.shiftKey && !!state.lastCheckedKey;
  }, true);
  els.recordGrid.addEventListener("change", event => {
    const box = event.target.closest("input[type='checkbox']");
    if (!box) return;
    const id = box.dataset.check;
    if (shiftRange) {
      selectRange(state.lastCheckedKey, id, box.checked);
    } else if (box.checked) {
      state.selectedIds.add(id);
    } else {
      state.selectedIds.delete(id);
    }
    state.lastCheckedKey = id;
    shiftRange = false;
    refreshSelectionUi();
  });
}

function applyTag(tag) {
  const ids = targetIds();
  if (!ids.length) { toast(t("toast.selectTarget")); return; }
  ids.forEach(id => {
    const list = state.tags[id] || [];
    if (!list.includes(tag)) list.push(tag);
    state.tags[id] = list;
    touchAnnotation(id);
  });
  storageSet(TAGS_KEY, state.tags);
  requestRender();
  toast(tf("toast.tagApplied", { n: ids.length, tag }));
}

function clearTags() {
  const ids = targetIds();
  if (!ids.length) { toast(t("toast.selectTarget")); return; }
  ids.forEach(id => { delete state.tags[id]; touchAnnotation(id); });
  storageSet(TAGS_KEY, state.tags);
  requestRender();
  toast(tf("toast.tagCleared", { n: ids.length }));
}

function downloadJSON(filename, payload) {
  const blob = new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" });
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(link.href), 5000);
  toast(tf("toast.downloadStart", { name: filename }));
}

function downloadText(filename, text, mime) {
  const blob = new Blob([text], { type: mime || "text/plain;charset=utf-8" });
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(link.href), 5000);
  toast(tf("toast.downloadStart", { name: filename }));
}

function csvCell(value) {
  const text = String(value ?? "");
  return /[",\n\r]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

function isoTime(unix) {
  return unix ? new Date(unix * 1000).toISOString() : "";
}

const KIND_LABELS = {
  video: t("filter.kind.video"),
  carved: t("filter.kind.carved"),
  filesystem: t("filter.kind.filesystem"),
  candidate: t("filter.kind.candidate")
};
const KIND_SHORT = {
  video: t("kind.short.video"),
  carved: t("kind.short.carved"),
  filesystem: t("kind.short.filesystem"),
  candidate: t("kind.short.candidate")
};

function exportItem(record) {
  return {
    id: record.id,
    name: record.name || "",
    path: record.path || "",
    kind: record.kind || "",
    status: record.status || "",
    mark: state.marks[record.id]?.status || "",
    tags: state.tags[record.id] || [],
    warnings: record.warnings || [],
    sha256: record.sha256 && record.sha256 !== "-" ? record.sha256 : "",
    rec_time: isoTime(record.recTime),
    original_path: record.originalPath || "",
    note: record.note && record.note !== "-" ? record.note : ""
  };
}

// Selection is usually far smaller than the case: resolve ids through the
// index map and keep case order, instead of scanning every record.
function selectedRecords() {
  return [...state.selectedIds]
    .map(id => recordIndexById.get(id))
    .filter(index => index != null)
    .sort((a, b) => a - b)
    .map(index => records[index]);
}

function copyText(text, label) {
  const done = () => toast(tf("toast.copied", { label }));
  const fallback = () => {
    const area = document.createElement("textarea");
    area.value = text;
    document.body.appendChild(area);
    area.select();
    try {
      document.execCommand("copy");
      done();
    } catch (error) {
      toast(tf("toast.copyFail", { err: error.message }));
    }
    area.remove();
  };
  if (navigator.clipboard?.writeText) {
    navigator.clipboard.writeText(text).then(done, fallback);
  } else {
    fallback();
  }
}

function toggleTheater() {
  state.layout.theater = !state.layout.theater;
  saveLayout();
  applyLayout();
}

function toggleFullscreen() {
  const stage = document.getElementById("mediaStage");
  if (document.fullscreenElement) {
    document.exitFullscreen().catch(error => toast(tf("toast.fsExitFail", { err: error.message })));
    return;
  }
  if (stage.requestFullscreen) {
    stage.requestFullscreen().catch(error => toast(tf("toast.fsEnterFail", { err: error.message })));
  } else {
    toast(t("toast.fsUnsupported"));
  }
}

async function togglePip() {
  const video = els.mediaStage.querySelector("video");
  if (!video) { toast(t("toast.noVideo")); return; }
  try {
    if (document.pictureInPictureElement) {
      await document.exitPictureInPicture();
    } else if (video.requestPictureInPicture) {
      await video.requestPictureInPicture();
    } else {
      toast(t("toast.pipUnsupported"));
    }
  } catch (error) {
    toast(tf("toast.pipFail", { err: error.message }));
  }
}
