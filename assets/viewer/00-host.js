/* window.FTViewer — the viewer's single host-facing surface
 * (docs/ui-rebuild-contract.md §3). First part of the viewer bundle.
 *
 * The same bundle serves two pages:
 *   standalone  /review/evidence-viewer.html, /viewer/, the packaged file,
 *               the legacy iframe (?embed=1) — 99-loader.js calls
 *               FTViewer.init() itself when <html> has no data-ft-host.
 *   hosted      the workstation shell (<html data-ft-host="shell">) mounts
 *               the fragment (#ftViewer) in its own document through
 *               FTViewer.init({ embed: true, host }) — no iframe.
 *
 * Everything the viewer registers on document / window / channels is tied
 * to one AbortController; destroy() aborts it, restores the fragment's
 * pristine markup (dropping every element listener and rendered state)
 * and leaves the root in place for the next init(). Unsaved marks survive
 * a destroy: they live in the per-case localStorage drafts that the next
 * init() rehydrates, and the shell is expected to save() first anyway.
 *
 * Only `FTViewer` (and window.FTViewer) is a top-level binding here; the
 * shell bundle must not declare top-level names starting with ft, FT,
 * viewer, loader or live (scripts/check-js.mjs enforces an empty
 * intersection of the two bundles' top-level bindings). */
const FTViewer = (() => {
  const target = new EventTarget();
  const counts = new Map(); // event -> listener count (request-popout fallback)
  let ac = new AbortController();
  let impl = null; // set by ftViewerBoot via _attach()
  let state = "idle"; // idle | mounting | mounted
  let root = null;
  let pristine = null; // { html, className } captured on the first init
  let hosted = false;
  let embed = false;
  let generation = 0; // bumps per mount; stale boots check it
  let apiBase = "";
  let popup = null;
  let popupTimer = 0;
  // Per-window writer identity (contract §3.3): the pop-out player opened
  // by THIS window carries it in every ft-player channel message; windows
  // holding another token (other shell tabs on the same case) stay silent.
  const writerToken = (() => {
    try {
      const a = new Uint32Array(4);
      crypto.getRandomValues(a);
      return Array.from(a, n => n.toString(36)).join("");
    } catch (e) { return String(Math.random()).slice(2) + Date.now().toString(36); }
  })();
  // Classes the HOST toggles on #ftViewer (not the viewer): they must
  // survive reload()'s destroy()+init().
  const HOST_CLASSES = ["popped"];

  const emit = (type, detail, cancelable = false) => {
    const ev = new CustomEvent(type, { detail: detail || {}, cancelable });
    try { target.dispatchEvent(ev); } catch (e) { /* a host listener threw */ }
    return ev;
  };
  const isHostDoc = () => document.documentElement.hasAttribute("data-ft-host");
  const findRoot = host => {
    const scope = host || document;
    if (scope.id === "ftViewer") return scope;
    return scope.querySelector ? scope.querySelector("#ftViewer") : document.getElementById("ftViewer");
  };
  const validTheme = v => (v === "dark" || v === "light" ? v : null);
  const validLocale = v => (v === "ko" || v === "en" ? v : null);

  function applyTheme(theme) {
    const next = validTheme(theme);
    if (!next) return false;
    const changed = document.documentElement.dataset.theme !== next;
    document.documentElement.dataset.theme = next;
    try { localStorage.setItem("ft-theme", next); } catch (e) { /* storage blocked */ }
    if (impl && impl.syncTheme) impl.syncTheme();
    if (changed) emit("theme", { theme: next });
    return true;
  }

  function stopPopupWatch() {
    if (popupTimer) clearInterval(popupTimer);
    popupTimer = 0;
  }

  const api = {
    version: 2,
    /** Channel names (contract §3.3). ft-review is the shell's (T2);
     *  ft-player:<case_id> is the popup-player bridge (70-player.js). */
    writerToken,
    channels: Object.freeze({ review: "ft-review", player: caseId => `ft-player:${caseId || "case"}` }),

    async init({ data = null, url = "", embed: wantEmbed = false, host = document, theme = null, locale = null, restore = null } = {}) {
      if (state !== "idle") throw new Error("FTViewer already mounted");
      const el = findRoot(host);
      if (!el) throw new Error("FTViewer: #ftViewer fragment not found");
      root = el;
      apiBase = String(url || "").replace(/\/+$/, "");
      if (!pristine) pristine = { html: el.innerHTML, className: el.className };
      hosted = isHostDoc();
      embed = !!wantEmbed || hosted;
      el.classList.toggle("hosted", hosted);
      el.classList.toggle("embed", embed);
      // The fragment carries role="main" (standalone: its one main
      // landmark). Inside the shell, whose <main id="main"> is the page's
      // main landmark, it becomes a labelled region instead.
      const area = hosted ? el.querySelector("#mainArea") : null;
      if (area) {
        area.setAttribute("role", "region");
        area.setAttribute("aria-label", "증거 검토");
        area.setAttribute("data-i18n-aria-label", "layout.review");
      }
      if (theme) applyTheme(theme);
      state = "mounting";
      const mine = ++generation;
      try {
        await ftViewerBoot({
          data, url, embed, hosted, root: el, restore,
          locale: validLocale(locale),
          signal: ac.signal,
          isCurrent: () => mine === generation && !ac.signal.aborted,
        });
      } catch (err) {
        if (mine === generation) {
          state = "idle";
          emit("error", { message: String(err && err.message || err), detail: String(err && err.stack || ""), stage: "load" });
        }
        throw err;
      }
      if (mine !== generation) return; // destroyed while loading
      state = "mounted";
    },

    destroy() {
      if (state === "idle" && !impl) return;
      generation += 1;
      try { impl && impl.beforeDestroy && impl.beforeDestroy(); } catch (e) { /* best effort */ }
      ac.abort();
      ac = new AbortController();
      impl = null;
      state = "idle";
      if (root && pristine) {
        root.innerHTML = pristine.html;
        root.className = pristine.className;
        root.removeAttribute("style");
      }
      emit("destroyed", {});
    },

    async reload({ keepUnsaved = true, reason = "" } = {}) {
      if (!impl) return "refused";
      const blocker = keepUnsaved ? await impl.reloadBlocker() : "";
      if (blocker) {
        impl.refuseReload(blocker);
        return "refused";
      }
      // U4: the view (filters, search, selection, active card, scroll, undo
      // journal) is carried over; the save state is not — a reload only
      // happens with nothing unsaved, and drafts rehydrate from storage.
      if (!hosted) {
        try { if (impl.stashViewState) impl.stashViewState(); } catch (e) { /* plain reload */ }
        location.reload();
        return "reloaded";
      }
      let restore = null;
      try { restore = impl.viewState ? impl.viewState() : null; } catch (e) { restore = null; }
      const opts = { embed, host: root, theme: api.getTheme(), locale: api.getLocale(), restore };
      const kept = root ? HOST_CLASSES.filter(c => root.classList.contains(c)) : [];
      const el = root;
      api.destroy();
      const restoreHostClasses = () => { if (el) for (const c of kept) el.classList.add(c); };
      restoreHostClasses(); // right away: the media column must not flash back
      await api.init(opts);
      restoreHostClasses();
      void reason;
      return "reloaded";
    },

    setLocale(locale) {
      const next = validLocale(locale);
      if (next && impl) impl.setLocale(next);
    },
    getLocale() { return impl ? impl.getLocale() : null; },
    setTheme(theme) { applyTheme(theme); },
    getTheme() { return validTheme(document.documentElement.dataset.theme) || "light"; },
    focus() { return impl ? impl.focus() : false; },
    summary() { return impl ? impl.summary() : null; },
    saveState() { return impl ? impl.saveState() : null; },
    async save() { return impl ? impl.save() : true; },
    /** Internal (22-viewer-host.js unmount): resolves true once no marks
     *  save request is in flight, false after `timeoutMs` (default 15 s). */
    _whenSaveSettled(timeoutMs = 15000) {
      if (!impl || !impl.whenSaveSettled) return Promise.resolve(true);
      return Promise.race([
        impl.whenSaveSettled().then(() => true),
        new Promise(resolve => setTimeout(() => resolve(false), timeoutMs)),
      ]);
    },
    /** Sets the viewer's examiner (검토자) used in marks payloads — the same
     *  flow as typing into #examinerName. No-op while unmounted. */
    setExaminer(name) { if (impl) impl.setExaminer(name); },
    /** Sets `name` on marks/notes whose examiner is empty and saves them;
     *  returns how many were re-pushed (0 when unmounted or name empty). */
    attributeUnassignedMarks(name) { return impl && impl.attributeUnassignedMarks ? impl.attributeUnassignedMarks(name) : 0; },
    getExaminer() { return impl ? impl.getExaminer() : null; },
    /** { live, generation, total, canReload } of the mounted data (same
     *  detail as the "live-state" event); null while unmounted. */
    liveState() { return impl && impl.liveState ? impl.liveState() : null; },
    popout() { return impl ? impl.popout() : null; },
    closePopout() {
      stopPopupWatch();
      const win = popup;
      popup = null;
      try { if (win && !win.closed) win.close(); } catch (e) { /* gone */ }
      if (win) emit("popout", { open: false });
    },
    isPoppedOut() { return !!(popup && !popup.closed); },
    toast(message, kind = "info") {
      if (impl) impl.toast(message, kind);
    },
    on(event, fn, opts) {
      target.addEventListener(event, fn, opts);
      counts.set(event, (counts.get(event) || 0) + 1);
    },
    off(event, fn) {
      target.removeEventListener(event, fn);
      counts.set(event, Math.max(0, (counts.get(event) || 0) - 1));
    },

    // ---- internal (viewer bundle only) ----
    get _ac() { return ac; },
    get _hosted() { return hosted; },
    get _embed() { return embed; },
    get _root() { return root; },
    get _apiBase() { return apiBase; },
    _attach(methods) { impl = methods; },
    _emit: emit,
    _applyTheme: applyTheme,
    _listens(event) { return (counts.get(event) || 0) > 0; },
    // The popup player window outlives a mount (its bridge calls always go
    // to the current window.__ftBridge), so its tracking lives here.
    _trackPopup(win) {
      if (!win) return;
      const was = api.isPoppedOut();
      popup = win;
      stopPopupWatch();
      popupTimer = setInterval(() => {
        if (popup && !popup.closed) return;
        stopPopupWatch();
        popup = null;
        emit("popout", { open: false });
      }, 1000);
      if (!was) emit("popout", { open: true });
    },
  };
  return api;
})();
window.FTViewer = FTViewer;
