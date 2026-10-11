// --- screen-reader announcer ------------------------------------------------
// The viewer used to carry ~14 independent aria-live="polite" regions
// (selection count, save-state button, toast host, index chip, ...) that all
// fired on every j/k. Now there are exactly two, created here and never
// removed:
//   #ftStatusLine  role=status (polite)  — debounced 300 ms; messages that
//                  arrive inside one window are merged into a single utterance
//   #ftAlert       role=alert (assertive) — save failure / server loss only
// Everything else announces through window.ftAnnounce(text, { assertive }).
function makeAnnouncerRegion(id, role, live) {
  const el = document.createElement("div");
  el.id = id;
  el.className = "sr-only";
  el.setAttribute("role", role);
  el.setAttribute("aria-live", live);
  el.setAttribute("aria-atomic", "true");
  ftRoot.appendChild(el); // inside #ftViewer: a destroy() removes it
  return el;
}
const ftPoliteRegion = makeAnnouncerRegion("ftStatusLine", "status", "polite");
const ftAssertiveRegion = makeAnnouncerRegion("ftAlert", "alert", "assertive");
let ftPoliteQueue = [];
let ftPoliteTimer = 0;
let ftPoliteFlip = false;

function ftFlushPolite() {
  ftPoliteTimer = 0;
  const messages = [...new Set(ftPoliteQueue)].slice(-3);
  ftPoliteQueue = [];
  if (!messages.length) return;
  // A trailing NBSP on alternate flushes makes a repeated sentence a new
  // text node value, so it is read again.
  ftPoliteFlip = !ftPoliteFlip;
  ftPoliteRegion.textContent = messages.join(". ") + (ftPoliteFlip ? "" : " ");
}

function ftAnnounce(text, opts) {
  const message = String(text == null ? "" : text).replace(/\s+/g, " ").trim();
  if (!message) return;
  if (opts && opts.assertive) {
    ftAssertiveRegion.textContent = "";
    requestAnimationFrame(() => { ftAssertiveRegion.textContent = message; });
    return;
  }
  ftPoliteQueue.push(message);
  if (!ftPoliteTimer) ftPoliteTimer = setTimeout(ftFlushPolite, 300);
}
window.ftAnnounce = ftAnnounce;
