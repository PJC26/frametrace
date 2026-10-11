// ---- stage bar: menus, stage shortcuts, Esc for toasts ----
// Menus are disclosure popovers (button aria-expanded + aria-controls):
// Enter/Space/click toggles, ArrowDown opens and focuses the first item,
// ArrowUp/ArrowDown/Home/End move between items, Esc or Tab-out closes —
// Esc returns focus to the opener. Clicking an action item closes the
// menu first (capture phase) so a confirm dialog it opens restores focus
// to the opener, not to a now-hidden item.
const POPOVERS = [["btnCaseMenu", "caseMenu"], ["btnTools", "toolsPop"]];
function popoverItems(pop) {
  return [...pop.querySelectorAll("button.mi, a.mi, label.mi input")]
    .filter(el => !el.disabled && !el.hidden && !(el.closest && el.closest("[hidden]")));
}
function closeAllPopovers(refocus) {
  for (const [btnId, popId] of POPOVERS) {
    const btn = $(btnId), pop = $(popId);
    if (!btn || !pop || pop.hidden) continue;
    pop.hidden = true;
    btn.setAttribute("aria-expanded", "false");
    if (refocus) { try { btn.focus(); } catch (e) {} }
  }
}
function openPopover(btnId, popId, focusFirst) {
  closeAllPopovers(false);
  const btn = $(btnId), pop = $(popId);
  if (!btn || !pop) return;
  pop.hidden = false;
  btn.setAttribute("aria-expanded", "true");
  if (focusFirst) {
    const items = popoverItems(pop);
    if (items[0]) items[0].focus();
  }
}
for (const [btnId, popId] of POPOVERS) {
  const btn = $(btnId), pop = $(popId);
  if (!btn || !pop) continue;
  btn.addEventListener("click", () => {
    if (pop.hidden) openPopover(btnId, popId, false);
    else closeAllPopovers(false);
  });
  btn.addEventListener("keydown", e => {
    if (e.key === "ArrowDown") { e.preventDefault(); openPopover(btnId, popId, true); }
  });
  pop.addEventListener("keydown", e => {
    const items = popoverItems(pop);
    const i = items.indexOf(document.activeElement);
    let next = -1;
    if (e.key === "ArrowDown") next = i < 0 ? 0 : (i + 1) % items.length;
    else if (e.key === "ArrowUp") next = i <= 0 ? items.length - 1 : i - 1;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = items.length - 1;
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeAllPopovers(true); return; }
    if (next >= 0 && items[next]) { e.preventDefault(); items[next].focus(); }
  });
  // Action items (buttons/links) close the menu; the checkbox row does not.
  pop.addEventListener("click", e => {
    const item = e.target && e.target.closest ? e.target.closest("button.mi, a.mi") : null;
    if (item && !item.disabled) closeAllPopovers(true);
  }, true);
  // Focus leaving both the opener and the popover closes it.
  pop.addEventListener("focusout", e => {
    const to = e.relatedTarget;
    if (to && (pop.contains(to) || to === btn)) return;
    if (to) closeAllPopovers(false);
  });
}
// Click outside any open menu closes it.
document.addEventListener("mousedown", e => {
  const t = e.target;
  for (const [btnId, popId] of POPOVERS) {
    const btn = $(btnId), pop = $(popId);
    if (!btn || !pop || pop.hidden) continue;
    if (t && (pop.contains(t) || btn.contains(t))) continue;
    pop.hidden = true;
    btn.setAttribute("aria-expanded", "false");
  }
});

// ---- Alt+1…Alt+4: switch stage ----
// Uses event.code (macOS Alt turns "1" into "¡"); skips AltGr (Windows
// reports ctrl+alt for it) and any text field, where Alt+digit may type a
// character. The viewer ignores plain-Alt keys (80-review.js / 70-player.js
// return on altKey without AltGraph), so the bridged frame keys never
// collide with its shortcuts.
function isTextField(el) {
  if (!el) return false;
  const tag = (el.tagName || "").toUpperCase();
  if (tag === "TEXTAREA" || tag === "SELECT" || el.isContentEditable) return true;
  if (tag !== "INPUT") return false;
  const type = (el.type || "text").toLowerCase();
  return !["checkbox", "radio", "button", "submit", "reset", "range", "color", "file"].includes(type);
}
function stageShortcut(e, activeEl) {
  if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return false;
  if (typeof e.getModifierState === "function" && e.getModifierState("AltGraph")) return false;
  const m = /^Digit([1-4])$/.exec(e.code || "") || /^Numpad([1-4])$/.exec(e.code || "");
  if (!m) return false;
  if (isTextField(activeEl)) return false;
  if (!$("dlgBackdrop").hidden || !$("pickerBackdrop").hidden) return false;
  const btn = $("s" + m[1]);
  if (!btn) return false;
  e.preventDefault();
  if (btn.disabled) { toast(`${CRUMB_NAMES[Number(m[1]) - 1]} 단계는 아직 열 수 없습니다 — ${STEP_WHY[m[1]] || ""}`, "warn"); return true; }
  btn.click();
  return true;
}
document.addEventListener("keydown", e => {
  if (stageShortcut(e, document.activeElement)) return;
  // Esc: an open menu first, then the newest toast (dialogs handle their own).
  if (e.key === "Escape" && !e.defaultPrevented && $("dlgBackdrop").hidden && $("pickerBackdrop").hidden) {
    const open = POPOVERS.some(([, popId]) => $(popId) && !$(popId).hidden);
    if (open) { closeAllPopovers(true); return; }
    if (dismissNewestToast()) e.preventDefault();
  }
});
// Same-origin review frame: forward Alt+digit from inside the viewer.
let bridgedDoc = null;
function bridgeFrameKeys() {
  let doc = null;
  try { doc = reviewFrame.contentDocument; } catch (e) { doc = null; }
  if (!doc || doc === bridgedDoc || typeof doc.addEventListener !== "function") return;
  bridgedDoc = doc;
  doc.addEventListener("keydown", e => { stageShortcut(e, doc.activeElement); });
  // A click inside the viewer closes any open shell menu.
  doc.addEventListener("mousedown", () => closeAllPopovers(false));
}
