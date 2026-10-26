/* Data loading order:
 * 1. window.__FRAMETRACE_DATA__ already set — legacy single-file bundle
 *    (older make-review output) — use it directly.
 * 2. Served by the frametrace workstation — page the case index through
 *    /api/records + /api/records-meta so the HTML never carries a
 *    multi-hundred-MB inline payload for large cases.
 * 3. Opened as a plain file (or a server without the API) — the
 *    generated sibling data-bundle.js, which a plain <script> tag can
 *    load even under file:// where fetch() is blocked.
 *
 * FOR-09: a failure is never silent. The overlay shows determinate
 * progress while paging, and any error — unreachable server, a failed
 * page, a short or duplicated page sequence (the index changed under the
 * pager), missing meta, a missing bundle — stops on a visible error with
 * a retry button instead of opening an empty or partial viewer. A count
 * mismatch can be accepted explicitly; the viewer then flags the list as
 * incomplete for the whole session (DATA.loadWarning).
 *
 * UX-02 live index: while the workstation is still scanning or enriching
 * (phase running / review-partial) the first page answers `live:true`.
 * The list then legitimately changes under the pager (checkpoint records
 * replace hash-less ones), so a version change or count drift is NOT an
 * error: the partial list opens, DATA.liveIndex records what was loaded,
 * and a chip polls /api/status — "색인 중 N건 / 보강 중" — offering to
 * reload once index_generation moves. The reload is refused while review
 * marks are unsaved, so annotations are never dropped by a refresh.
 *
 * This file sits outside ftViewerBoot() and runs before i18n is set up,
 * so its few strings are bilingual inline.
 *
 * The overlay and the live chip are created INSIDE the fragment root
 * (#ftViewer): the viewer stylesheet is scoped to .ft-viewer, a hosted page
 * keeps them out of the shell's layout, and FTViewer.destroy() removes them
 * with the rest of the mount. Their colours are the shared tokens.
 */
//
// Only function declarations live at this file's top level (plus the
// standalone auto-init call at the very end): the shell bundle shares the
// global scope when the viewer is hosted (docs/ui-rebuild-contract.md §2.1).
function loaderLimits() {
  return { pageSize: 500, maxRestarts: 3 };
}

function ViewerLoadError(message, detail, partial) {
  const error = new Error(message);
  error.ftViewerLoad = true;
  error.detail = detail || "";
  error.partial = partial || null;
  return error;
}

// Record API base (FTViewer.init({ url })): "" = this origin.
function loaderApiBase() {
  return (window.FTViewer && window.FTViewer._apiBase) || "";
}

function loaderOverlay(root) {
  const note = document.createElement("div");
  note.id = "ft-load-note";
  note.setAttribute("role", "status");
  note.setAttribute("aria-live", "polite");
  note.style.cssText = "position:absolute;inset:0;display:grid;place-items:center;background:var(--media-bg);color:var(--media-muted);font:var(--fs-sm)/1.5 var(--font-sans);z-index:90";
  const box = document.createElement("div");
  box.style.cssText = "max-width:560px;padding:20px 24px;text-align:center";
  const msg = document.createElement("div");
  msg.textContent = "증거 목록을 불러오는 중… / Loading evidence list…";
  const bar = document.createElement("progress");
  bar.style.cssText = "width:320px;max-width:100%;margin-top:10px";
  bar.removeAttribute("value"); // indeterminate until the total is known
  const detail = document.createElement("div");
  detail.style.cssText = "margin-top:8px;font-size:var(--fs-xs);color:var(--media-muted);white-space:pre-wrap;word-break:break-word";
  const actions = document.createElement("div");
  actions.style.cssText = "margin-top:14px;display:flex;gap:8px;justify-content:center;flex-wrap:wrap";
  box.append(msg, bar, detail, actions);
  note.appendChild(box);
  (root || document.getElementById("ftViewer") || document.body).appendChild(note);
  const button = (label, primary) => {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = label;
    b.style.cssText = "min-height:32px;padding:6px 14px;border-radius:var(--radius);font:inherit;cursor:pointer;" +
      (primary ? "background:var(--accent);color:var(--on-accent);border:1px solid var(--accent)" : "background:transparent;color:var(--media-muted);border:1px solid var(--media-muted)");
    return b;
  };
  return {
    progress(loaded, total) {
      msg.textContent = `증거 목록을 불러오는 중… / Loading evidence list… ${loaded}/${total}`;
      bar.hidden = false;
      bar.max = Math.max(1, total);
      bar.value = Math.min(loaded, total);
    },
    status(text) {
      msg.textContent = text;
    },
    // Shows the error and resolves with "retry" or "continue" (the latter
    // only offered when a partial list is available).
    fail(error) {
      note.setAttribute("role", "alert");
      bar.hidden = true;
      msg.style.color = "var(--failed-fg)";
      msg.style.background = "var(--failed-bg)";
      msg.style.padding = "4px 8px";
      msg.style.borderRadius = "var(--radius)";
      msg.style.fontWeight = "700";
      msg.textContent = error.message || "증거 목록을 불러오지 못했습니다. / Failed to load the evidence list.";
      detail.textContent = error.detail || "";
      actions.replaceChildren();
      return new Promise(resolve => {
        const retry = button("다시 시도 / Retry", true);
        retry.addEventListener("click", () => resolve("retry"));
        actions.appendChild(retry);
        if (error.partial) {
          const partial = error.partial;
          const cont = button(`부분 목록으로 계속 (${partial.loadWarning.loaded}/${partial.loadWarning.total}) / Continue with partial list`, false);
          cont.addEventListener("click", () => resolve("continue"));
          actions.appendChild(cont);
        }
        retry.focus();
      });
    },
    reset() {
      note.setAttribute("role", "status");
      msg.style.color = "";
      msg.style.background = "";
      msg.style.padding = "";
      msg.style.fontWeight = "";
      msg.textContent = "증거 목록을 불러오는 중… / Loading evidence list…";
      detail.textContent = "";
      actions.replaceChildren();
      bar.hidden = false;
      bar.removeAttribute("value");
    },
    remove() { note.remove(); }
  };
}

async function loaderFetchJson(url) {
  let res;
  try {
    res = await fetch(url, { cache: "no-store" });
  } catch (err) {
    throw ViewerLoadError(
      "워크스테이션 서버에 연결할 수 없습니다. / Cannot reach the workstation server.",
      `${url}: ${err && err.message ? err.message : err}`
    );
  }
  if (res.status === 401) {
    // The server is up but refused this page's proof (U8): no "server
    // error" wording — the way back is reopening from the workstation.
    const error = ViewerLoadError(
      "인증 오류로 요청이 거부되었습니다 — FrameTrace 앱 창에서 다시 여십시오(같은 컴퓨터의 다른 FrameTrace 창은 닫으십시오). / Request refused (authorization error) — reopen from the FrameTrace app window (close any other FrameTrace window on this computer).",
      `${url}: HTTP 401`
    );
    error.status = 401;
    throw error;
  }
  if (!res.ok) {
    const error = ViewerLoadError(
      `서버 응답 오류 (HTTP ${res.status}) / Server error (HTTP ${res.status})`,
      url
    );
    error.status = res.status;
    throw error;
  }
  try {
    return await res.json();
  } catch (err) {
    throw ViewerLoadError("서버 응답을 해석할 수 없습니다. / Unreadable server response.", url);
  }
}

// Pages the whole index. Restarts from offset 0 when the server reports a
// different index version (etag/generation, when provided) mid-way; any
// short page, failed page or count mismatch is an error, never a silent
// truncation. Duplicate ids (an unstable order across pages) are dropped
// and surface as a count mismatch.
async function loadAllRecordPages(first, ui, live) {
  const liveParam = live ? "&live=1" : "";
  const versionOf = page => page && (page.etag ?? page.generation ?? null);
  let restarts = 0;
  let head = first;
  for (;;) {
    const version = versionOf(head);
    const total = Number(head.total);
    if (!Number.isFinite(total) || total < 0) {
      throw ViewerLoadError("서버가 전체 건수를 알려주지 않았습니다. / Server did not report a total.", "/api/records");
    }
    const byId = new Map();
    const keep = list => {
      for (const video of list) {
        const key = video && video.id != null ? String(video.id) : JSON.stringify(video);
        if (!byId.has(key)) byId.set(key, video);
      }
    };
    let offset = Array.isArray(head.videos) ? head.videos.length : 0;
    keep(Array.isArray(head.videos) ? head.videos : []);
    let restarted = false;
    while (offset < total) {
      ui.progress(offset, total);
      const page = await loaderFetchJson(`${loaderApiBase()}/api/records?offset=${offset}&limit=${loaderLimits().pageSize}${liveParam}`);
      if (!page || !page.ok || !Array.isArray(page.videos)) {
        throw ViewerLoadError(
          `증거 목록 일부를 불러오지 못했습니다 (${offset}/${total}). / Failed to load part of the evidence list.`,
          (page && page.error) || `/api/records offset=${offset}`
        );
      }
      const pageVersion = versionOf(page);
      if (!live && version !== null && pageVersion !== null && pageVersion !== version) {
        if (restarts >= loaderLimits().maxRestarts) {
          throw ViewerLoadError(
            "불러오는 동안 케이스 색인이 계속 바뀌고 있습니다. 분석이 끝난 뒤 다시 시도하십시오. / The case index kept changing while loading — retry after the analysis finishes.",
            `version ${version} → ${pageVersion}`
          );
        }
        restarts += 1;
        ui.status("케이스 색인이 바뀌어 처음부터 다시 불러옵니다… / Case index changed — reloading from the start…");
        head = await loaderFetchJson(`${loaderApiBase()}/api/records?offset=0&limit=${loaderLimits().pageSize}`);
        if (!head || !head.ok) {
          throw ViewerLoadError("증거 목록을 불러오지 못했습니다. / Failed to load the evidence list.", (head && head.error) || "/api/records");
        }
        restarted = true;
        break;
      }
      if (!page.videos.length) break; // short sequence — reported below
      keep(page.videos);
      offset += page.videos.length;
    }
    if (restarted) continue;
    ui.progress(Math.min(offset, total), total);
    return { videos: [...byId.values()], total, received: offset, generation: head.generation ?? null };
  }
}

// R10 U5: the data objects built from this server's API (10-boot
// trustFromBundle) — a bundle cannot claim it by a field of its own.
const LIVE_TRUST_DATA = new WeakSet();
async function loadViewerDataFromApi(first, ui) {
  const live = !!(first && first.live);
  const paged = await loadAllRecordPages(first, ui, live);
  ui.status("케이스 정보를 불러오는 중… / Loading case metadata…");
  const meta = await loaderFetchJson(`${loaderApiBase()}/api/records-meta`);
  if (!meta || meta.ok === false || typeof meta !== "object") {
    throw ViewerLoadError(
      "케이스 정보(/api/records-meta)를 불러오지 못했습니다 — 기존 판독 마크가 표시되지 않을 수 있어 중단합니다. / Failed to load case metadata; stopping so existing review marks are not hidden.",
      (meta && meta.error) || "/api/records-meta"
    );
  }
  const scan = (meta && meta.scan && typeof meta.scan === "object") ? meta.scan : {};
  scan.videos = paged.videos;
  const data = {
    manifest: meta.manifest || {},
    // R7: case.json / the scan summary came back null (not valid JSON on
    // disk) — the viewer says so instead of an unexplained empty case.
    manifestUnreadable: !meta.manifest || typeof meta.manifest !== "object",
    scanUnreadable: Object.prototype.hasOwnProperty.call(meta, "scan") && (!meta.scan || typeof meta.scan !== "object"),
    caseDir: meta.caseDir || "",
    scan,
    carveLog: meta.carveLog,
    filesystemLog: meta.filesystemLog,
    validationLog: meta.validationLog,
    anomalyLog: meta.anomalyLog,
    flsEntries: meta.flsEntries,
    thumbs: meta.thumbs,
    annotations: meta.annotations,
    deepfake: meta.deepfake,
    telemetry: meta.telemetry,
    proxies: meta.proxies,
    // R5 U16 (S15): per-record proxy paths {id: path} when the server names
    // them; the viewer then uses only those (30-records resolveProxyPaths).
    proxyPaths: meta.proxy_paths || meta.proxyPaths || null,
    joinLog: meta.joinLog,
    audioLevels: meta.audioLevels,
    // A failed read of the measurement log (records-meta reports it beside
    // an empty map): kept so the viewer never shows it as "not measured".
    audioLevelsError: meta.audioLevelsError || null,
    // R10 U3: per-lane chain state ({carveLog|…: {status, entries,
    // unauthenticated, detail}}); absent from older servers and bundles.
    logChains: meta.logChains && typeof meta.logChains === "object" ? meta.logChains : null,
  };
  // R10 U5: trust codes computed by this server just now (not a bundle's).
  LIVE_TRUST_DATA.add(data);
  if (live) {
    // The index is still growing/enriching: whatever arrived is the
    // current snapshot, not a truncation. The chip tells the examiner.
    data.liveIndex = {
      generation: paged.generation,
      loaded: paged.videos.length,
      total: paged.total,
      phase: first.phase || "",
    };
    return data;
  }
  if (paged.videos.length !== paged.total) {
    data.loadWarning = { loaded: paged.videos.length, total: paged.total };
    throw ViewerLoadError(
      `증거 목록이 완전하지 않습니다: ${paged.total}건 중 ${paged.videos.length}건만 받았습니다. / Incomplete evidence list: received ${paged.videos.length} of ${paged.total}.`,
      paged.received !== paged.videos.length
        ? `중복 ${paged.received - paged.videos.length}건 — 불러오는 동안 색인이 바뀌었을 수 있습니다. / ${paged.received - paged.videos.length} duplicate rows — the index may have changed while loading.`
        : "서버가 마지막 페이지 전에 빈 페이지를 돌려주었습니다. / The server returned an empty page before the end.",
      data
    );
  }
  return data;
}

function loadDataBundle() {
  return new Promise(resolve => {
    document.querySelectorAll("script[data-ft-bundle]").forEach(el => el.remove());
    const s = document.createElement("script");
    s.src = "data-bundle.js";
    s.dataset.ftBundle = "1";
    s.onload = () => resolve(true);
    s.onerror = () => resolve(false);
    document.head.appendChild(s);
  });
}

async function loadViewerDataOnce(ui) {
  const served = location.protocol === "http:" || location.protocol === "https:";
  let apiError = null;
  if (served) {
    let first = null;
    try {
      first = await loaderFetchJson(`${loaderApiBase()}/api/records?offset=0&limit=${loaderLimits().pageSize}&live=1`);
    } catch (err) {
      // 404 = a plain static server without the API: the bundle is the
      // expected source. Anything else is a real failure to report if the
      // bundle is missing too.
      if (err.status !== 404) apiError = err;
    }
    if (first && first.ok) return loadViewerDataFromApi(first, ui);
    if (first && !first.ok) {
      apiError = ViewerLoadError(
        "서버가 증거 목록을 제공하지 못했습니다. / The server could not provide the evidence list.",
        first.error || "/api/records"
      );
    }
  }
  // Hosted in the workstation page (GET /), the API is the only source:
  // there is no data-bundle.js next to "/" (host-integration hook).
  const hostedPage = typeof FTViewer === "object" && FTViewer && FTViewer._hosted;
  if (!hostedPage) {
    const loaded = await loadDataBundle();
    const data = window.__FRAMETRACE_DATA__;
    if (loaded && data && typeof data === "object") return data;
  }
  if (apiError) throw apiError;
  throw ViewerLoadError(
    "증거 데이터(data-bundle.js)를 찾을 수 없습니다. / Evidence data (data-bundle.js) not found.",
    "뷰어 HTML과 같은 폴더에 data-bundle.js가 있어야 합니다. 리뷰 페이지를 다시 생성하거나 워크스테이션에서 여십시오. / data-bundle.js must sit next to the viewer HTML; regenerate the review page or open it from the workstation."
  );
}

async function loadViewerData(mount) {
  const { root = null, signal = null } = mount || {};
  // R7: an inline bundle whose case.json / index was unreadable carries
  // `manifest: null` / `scan: null` — it is still THE data (the viewer says
  // the case files were not read), not a reason to look for another source.
  const inline = window.__FRAMETRACE_DATA__;
  if (inline && typeof inline === "object" && (inline.manifest || "manifest" in inline || "scan" in inline)) {
    return inline;
  }
  const ui = loaderOverlay(root);
  for (;;) {
    try {
      const data = await loadViewerDataOnce(ui);
      ui.remove();
      // Hosted: the shell's top-strip chip and jobs sheet show the live
      // index (FTViewer "live-state"); only standalone pages get the chip.
      const hostedMount = !!(window.FTViewer && window.FTViewer._hosted);
      if (data && data.liveIndex && !hostedMount) setTimeout(() => { if (!signal || !signal.aborted) startLiveIndexChip(data.liveIndex, root, signal); }, 0);
      return data;
    } catch (err) {
      const error = err && err.ftViewerLoad
        ? err
        : ViewerLoadError("증거 목록을 불러오지 못했습니다. / Failed to load the evidence list.", String(err && err.message || err));
      if (window.FTViewer) window.FTViewer._emit("error", { message: error.message, detail: error.detail || "", stage: "load" });
      const action = await ui.fail(error);
      if (action === "continue" && error.partial) {
        ui.remove();
        return error.partial;
      }
      ui.reset();
    }
  }
}

// ---- UX-02 live index chip ----------------------------------------------
// Polls /api/status while the case index is still being built or enriched
// and shows where it stands. New data is applied by reloading the page
// (the viewer derives its record model once at boot), and only when the
// save indicator reports nothing pending — a refresh must never be the
// thing that loses review marks.
function liveChipText(st, live, newTotal) {
  const n = newTotal != null ? newTotal : live.total;
  const phase = st ? st.phase : live.phase;
  if (phase === "running") return `색인 중 ${n}건 / Indexing ${n}`;
  if (phase === "review-partial") {
    const e = st && st.enrichment;
    if (st && st.busy && e) {
      const prog = e.total > 0 ? ` ${e.done}/${e.total}` : "";
      return `색인 ${n}건 · 보강 중: ${e.label}${prog} / Enriching`;
    }
    if (st && st.busy) return `색인 ${n}건 · 보강 중 / Enriching`;
    return `색인 ${n}건 · 보강 중단됨 — 해시·검증 미완료 / Enrichment stopped`;
  }
  if (phase === "review-ready" || phase === "done" || phase === "finalized") {
    return `보강 완료 (${n}건) / Enrichment complete`;
  }
  return `색인 ${n}건 / Indexed ${n}`;
}

function liveSaveIsClean() {
  const el = document.getElementById("saveState");
  const kind = el && el.dataset ? el.dataset.state : "";
  return !kind || kind === "saved";
}

function startLiveIndexChip(live, root, signal) {
  if (document.getElementById("ft-live-chip")) return;
  const chip = document.createElement("div");
  chip.id = "ft-live-chip";
  chip.setAttribute("role", "status");
  chip.setAttribute("aria-live", "polite");
  chip.style.cssText = "position:fixed;right:12px;bottom:60px;z-index:55;display:flex;gap:8px;align-items:center;max-width:min(560px,calc(100vw - 24px));padding:6px 10px;border-radius:999px;background:var(--surface);color:var(--text);border:1px solid var(--border-strong);font:var(--fs-xs)/1.4 var(--font-sans)";
  const text = document.createElement("span");
  const button = document.createElement("button");
  button.type = "button";
  button.hidden = true;
  button.style.cssText = "padding:2px 10px;border-radius:999px;border:1px solid var(--accent);background:var(--accent);color:var(--on-accent);font:inherit;cursor:pointer";
  const note = document.createElement("span");
  note.style.cssText = "color:var(--warn)";
  chip.append(text, button, note);
  (root || document.getElementById("ftViewer") || document.body).appendChild(chip);

  let latest = null;
  let newTotal = null;
  let failures = 0;
  let timer = 0;
  const render = () => {
    text.textContent = liveChipText(latest, live, newTotal);
    const changed = latest && latest.index_generation != null && live.generation != null &&
      latest.index_generation !== live.generation;
    button.hidden = !changed;
    if (changed) {
      button.textContent = newTotal != null
        ? `새 데이터 반영 (${newTotal}건) / Reload`
        : "새 데이터 반영 / Reload";
    }
    const stopped = latest && latest.phase === "review-partial" && !latest.busy;
    chip.style.borderColor = stopped ? "var(--warn)" : "var(--border-strong)";
  };
  button.addEventListener("click", () => {
    if (!liveSaveIsClean()) {
      note.textContent = "저장되지 않은 판독 내용이 있어 새로 고치지 않았습니다 — 저장 상태가 '저장됨'이 된 뒤 다시 누르십시오. / Unsaved review marks — reload refused.";
      const save = document.getElementById("saveState");
      if (save) save.click(); // ask the viewer to save (or retry) now
      return;
    }
    note.textContent = "";
    // Standalone: a page reload; hosted: destroy() + init() in place.
    if (window.FTViewer) window.FTViewer.reload({ reason: "live-index" });
    else location.reload();
  });
  const tick = async () => {
    timer = 0;
    if (signal && signal.aborted) return; // the mount is gone
    try {
      const res = await fetch("/api/status", { cache: "no-store" });
      if (res.status === 401) {
        note.textContent = "인증 오류로 상태 확인이 거부되었습니다 — FrameTrace 앱 창에서 다시 여십시오. / Status refused (authorization error) — reopen from the FrameTrace app window.";
        timer = setTimeout(tick, 15000);
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const st = await res.json();
      failures = 0;
      const moved = latest && st.index_generation !== latest.index_generation;
      latest = st;
      if (moved || (newTotal == null && st.index_generation !== live.generation)) {
        try {
          const head = await fetch(`${loaderApiBase()}/api/records?offset=0&limit=1&live=1`, { cache: "no-store" });
          const page = head.ok ? await head.json() : null;
          if (page && page.ok && Number.isFinite(Number(page.total))) newTotal = Number(page.total);
        } catch (e) { /* the count is a hint; the status line still updates */ }
      }
      if (note.textContent.startsWith("상태 확인 실패") || note.textContent.startsWith("인증 오류")) note.textContent = "";
    } catch (err) {
      failures += 1;
      note.textContent = `상태 확인 실패 (${failures}회) — 재시도 중 / Status check failed, retrying`;
    }
    if (signal && signal.aborted) return;
    render();
    // review-partial is polled fast even when not busy: "stopped" is only
    // shown for as long as the server really reports it.
    const active = !latest || latest.phase === "running" || latest.phase === "review-partial";
    // Keep polling while work continues; afterwards slow down but keep the
    // chip honest (a later re-analysis can move the index again).
    timer = setTimeout(tick, active ? 3000 : 15000);
  };
  render();
  tick();
  return () => { if (timer) clearTimeout(timer); };
}

// A served standalone viewer (/viewer/?token=…, or the one-time launch
// code /?launch=… of R4 B2) drops those secrets from the address bar and
// history once the page has them (the server already planted its cookie on
// this response). Every other query parameter and the hash stay.
function loaderStripLaunchSecrets() {
  try {
    if (location.protocol !== "http:" && location.protocol !== "https:") return;
    const url = new URL(location.href);
    if (!url.searchParams.has("token") && !url.searchParams.has("launch")) return;
    url.searchParams.delete("token");
    url.searchParams.delete("launch");
    history.replaceState(history.state, "", url.pathname + url.search + url.hash);
  } catch (err) {
    console.warn("토큰 URL 정리 실패 / could not strip the launch secret from the URL", err);
  }
}

// ---- standalone auto-init (docs/ui-rebuild-contract.md §3.1) --------------
// /review/evidence-viewer.html, /viewer/, the packaged file and the legacy
// iframe mount themselves; a hosting page (<html data-ft-host>) calls
// FTViewer.init() when a project opens.
if (!document.documentElement.hasAttribute("data-ft-host")) {
  loaderStripLaunchSecrets();
  FTViewer.init({ embed: new URLSearchParams(location.search).has("embed") || window.self !== window.top })
    .catch(err => { console.error("FrameTrace viewer failed to start", err); });
}
