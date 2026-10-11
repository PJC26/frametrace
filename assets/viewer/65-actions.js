function setupGridDelegation() {
  if (els.recordGrid._delegated) return;
  els.recordGrid._delegated = true;
  let shiftRange = false;
  // PERF-05: a thumbnail that fails to load is retried once (cache-busted),
  // then replaced by the "no thumbnail" placeholder — never a broken-image
  // icon. error events don't bubble, so listen in the capture phase.
  els.recordGrid.addEventListener("error", event => {
    const img = event.target;
    if (!img || img.tagName !== "IMG" || !img.closest(".thumb")) return;
    const src = img.getAttribute("src") || "";
    // thumbRetried/thumbDead outlive grid re-renders (the virtual grid
    // recreates <img> nodes), so a bad URL costs one retry in total.
    const base = src.replace(/[?&]r=1$/, "");
    if (!thumbRetried.has(base) && !/^data:/i.test(src)) {
      thumbRetried.add(base);
      setTimeout(() => { if (img.isConnected) img.src = base + (base.includes("?") ? "&" : "?") + "r=1"; }, 400);
      return;
    }
    thumbDead.add(base);
    const ph = document.createElement("div");
    ph.className = "ph";
    ph.textContent = t("thumb.missing");
    img.replaceWith(ph);
  }, true);
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

// --- UX-14: busy buttons + in-page dialogs -------------------------------

// Runs fn() while `button` shows a busy state (disabled, aria-busy, spinner
// via .busy). Optional toasts: opts.start (i18n key), opts.done(result)
// -> string|null, opts.fail(err) -> string. Errors never stay silent: an
// exception always ends in an error toast and the button is re-enabled.
// A second click while busy is ignored.
async function withBusy(button, fn, opts = {}) {
  let hadFocus = false;
  if (button) {
    if (button.dataset.busy === "1") return undefined;
    hadFocus = document.activeElement === button;
    button.dataset.busy = "1";
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    button.classList.add("busy");
  }
  if (opts.start) toast(t(opts.start));
  try {
    const result = await fn();
    const done = typeof opts.done === "function" ? opts.done(result) : opts.done;
    if (done) toast(done);
    return result;
  } catch (err) {
    const msg = typeof opts.fail === "function" ? opts.fail(err) : tf("toast.busyFail", { err: err?.message || String(err) });
    if (msg) toastError(msg); // failures persist until dismissed
    return undefined;
  } finally {
    if (button) {
      delete button.dataset.busy;
      button.disabled = false;
      button.removeAttribute("aria-busy");
      button.classList.remove("busy");
      // A disabled button drops keyboard focus; hand it back so the
      // examiner is not thrown to <body> after a long export.
      if (hadFocus && (!document.activeElement || document.activeElement === document.body)) {
        try { button.focus({ preventScroll: true }); } catch { /* detached */ }
      }
    }
  }
}

// Accessible in-page dialog replacing window.confirm / window.prompt.
// spec: { title?, message, fields?: [{ name, label, value }], ok?, cancel? }
// Resolves to null on cancel/Esc/backdrop click; otherwise to a values
// object ({} when there are no fields). Enter confirms from an input,
// Tab cycles inside the dialog, focus returns to the opener.
function askDialog(spec) {
  return new Promise(resolve => {
    const opener = document.activeElement;
    const back = document.createElement("div");
    back.className = "modal-backdrop";
    const box = document.createElement("div");
    box.className = "modal dlg";
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    const uid = "dlg" + Math.random().toString(36).slice(2, 8);
    const msg = document.createElement("p");
    msg.id = uid + "m";
    msg.className = "dlg-msg";
    msg.textContent = spec.message || "";
    box.setAttribute("aria-labelledby", spec.title ? uid + "t" : msg.id);
    if (spec.title) {
      const h = document.createElement("div");
      h.id = uid + "t";
      h.className = "panel-title";
      h.textContent = spec.title;
      box.appendChild(h);
    }
    box.appendChild(msg);
    const inputs = {};
    (spec.fields || []).forEach((f, i) => {
      const label = document.createElement("label");
      label.className = "dlg-field";
      label.textContent = f.label || "";
      const input = document.createElement("input");
      input.type = "text";
      input.value = f.value || "";
      input.id = uid + "f" + i;
      label.appendChild(input);
      box.appendChild(label);
      inputs[f.name] = input;
    });
    const row = document.createElement("div");
    row.className = "dlg-actions";
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.textContent = spec.cancel || t("dlg.cancel");
    const ok = document.createElement("button");
    ok.type = "button";
    ok.className = "primary";
    ok.textContent = spec.ok || t("dlg.ok");
    if (spec.single) row.append(ok); else row.append(cancel, ok);
    box.appendChild(row);
    back.appendChild(box);
    let done = false;
    const finish = value => {
      if (done) return;
      done = true;
      back.removeEventListener("keydown", onKey, true);
      back.remove();
      try { if (opener && opener.focus) opener.focus(); } catch { /* opener gone */ }
      resolve(value);
    };
    const accept = () => {
      const out = {};
      for (const k of Object.keys(inputs)) out[k] = inputs[k].value;
      finish(out);
    };
    function onKey(e) {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); finish(null); return; }
      if (e.key === "Enter" && e.target.tagName === "INPUT") { e.preventDefault(); e.stopPropagation(); accept(); return; }
      if (e.key === "Tab") {
        const f = [...box.querySelectorAll("input,button")];
        const first = f[0], last = f[f.length - 1];
        if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
        else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
        return;
      }
      // Keep app shortcuts (j/k/1/2/space ...) from firing under the dialog.
      e.stopPropagation();
    }
    back.addEventListener("keydown", onKey, true);
    back.addEventListener("click", e => { if (e.target === back) finish(null); });
    cancel.addEventListener("click", () => finish(null));
    ok.addEventListener("click", accept);
    ftRoot.appendChild(back);
    const firstInput = Object.values(inputs)[0];
    (firstInput || ok).focus();
    if (firstInput) firstInput.select();
  });
}
