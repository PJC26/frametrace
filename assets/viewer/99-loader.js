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
 * This file sits outside the viewer IIFE and runs before i18n is set
 * up, so its few strings are bilingual inline. */
//
// Only function declarations live at this file's top level: the viewer
// IIFE (10-boot.js) calls loadViewerData() while the bundle is still being
// evaluated, before any top-level const/class further down would leave
// its temporal dead zone.
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

function loaderOverlay() {
  const note = document.createElement("div");
  note.id = "ft-load-note";
  note.setAttribute("role", "status");
  note.setAttribute("aria-live", "polite");
  note.style.cssText = "position:fixed;inset:0;display:grid;place-items:center;background:rgba(14,21,19,.92);color:#c9d6d1;font:14px/1.5 system-ui,sans-serif;z-index:9999";
  const box = document.createElement("div");
  box.style.cssText = "max-width:560px;padding:20px 24px;text-align:center";
  const msg = document.createElement("div");
  msg.textContent = "증거 목록을 불러오는 중… / Loading evidence list…";
  const bar = document.createElement("progress");
  bar.style.cssText = "width:320px;max-width:100%;margin-top:10px";
  bar.removeAttribute("value"); // indeterminate until the total is known
  const detail = document.createElement("div");
  detail.style.cssText = "margin-top:8px;font-size:12px;color:#8fa79e;white-space:pre-wrap;word-break:break-word";
  const actions = document.createElement("div");
  actions.style.cssText = "margin-top:14px;display:flex;gap:8px;justify-content:center;flex-wrap:wrap";
  box.append(msg, bar, detail, actions);
  note.appendChild(box);
  (document.body || document.documentElement).appendChild(note);
  const button = (label, primary) => {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = label;
    b.style.cssText = "padding:6px 14px;border-radius:6px;font:inherit;cursor:pointer;" +
      (primary ? "background:#0f7c71;color:#fff;border:1px solid #0f7c71" : "background:transparent;color:#c9d6d1;border:1px solid #5d746c");
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
      msg.style.color = "#f2b8ad";
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
async function loadAllRecordPages(first, ui) {
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
      const page = await loaderFetchJson(`/api/records?offset=${offset}&limit=${loaderLimits().pageSize}`);
      if (!page || !page.ok || !Array.isArray(page.videos)) {
        throw ViewerLoadError(
          `증거 목록 일부를 불러오지 못했습니다 (${offset}/${total}). / Failed to load part of the evidence list.`,
          (page && page.error) || `/api/records offset=${offset}`
        );
      }
      const pageVersion = versionOf(page);
      if (version !== null && pageVersion !== null && pageVersion !== version) {
        if (restarts >= loaderLimits().maxRestarts) {
          throw ViewerLoadError(
            "불러오는 동안 케이스 색인이 계속 바뀌고 있습니다. 분석이 끝난 뒤 다시 시도하십시오. / The case index kept changing while loading — retry after the analysis finishes.",
            `version ${version} → ${pageVersion}`
          );
        }
        restarts += 1;
        ui.status("케이스 색인이 바뀌어 처음부터 다시 불러옵니다… / Case index changed — reloading from the start…");
        head = await loaderFetchJson(`/api/records?offset=0&limit=${loaderLimits().pageSize}`);
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
    return { videos: [...byId.values()], total, received: offset };
  }
}

async function loadViewerDataFromApi(first, ui) {
  const paged = await loadAllRecordPages(first, ui);
  ui.status("케이스 정보를 불러오는 중… / Loading case metadata…");
  const meta = await loaderFetchJson("/api/records-meta");
  if (!meta || meta.ok === false || typeof meta !== "object") {
    throw ViewerLoadError(
      "케이스 정보(/api/records-meta)를 불러오지 못했습니다 — 기존 판독 마크가 표시되지 않을 수 있어 중단합니다. / Failed to load case metadata; stopping so existing review marks are not hidden.",
      (meta && meta.error) || "/api/records-meta"
    );
  }
  const scan = (meta && meta.scan) || {};
  scan.videos = paged.videos;
  const data = {
    manifest: meta.manifest || {},
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
  };
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
      first = await loaderFetchJson(`/api/records?offset=0&limit=${loaderLimits().pageSize}`);
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
  const loaded = await loadDataBundle();
  const data = window.__FRAMETRACE_DATA__;
  if (loaded && data && typeof data === "object") return data;
  if (apiError) throw apiError;
  throw ViewerLoadError(
    "증거 데이터(data-bundle.js)를 찾을 수 없습니다. / Evidence data (data-bundle.js) not found.",
    "뷰어 HTML과 같은 폴더에 data-bundle.js가 있어야 합니다. 리뷰 페이지를 다시 생성하거나 워크스테이션에서 여십시오. / data-bundle.js must sit next to the viewer HTML; regenerate the review page or open it from the workstation."
  );
}

async function loadViewerData() {
  if (window.__FRAMETRACE_DATA__ && window.__FRAMETRACE_DATA__.manifest) {
    return window.__FRAMETRACE_DATA__;
  }
  const ui = loaderOverlay();
  for (;;) {
    try {
      const data = await loadViewerDataOnce(ui);
      ui.remove();
      return data;
    } catch (err) {
      const error = err && err.ftViewerLoad
        ? err
        : ViewerLoadError("증거 목록을 불러오지 못했습니다. / Failed to load the evidence list.", String(err && err.message || err));
      const action = await ui.fail(error);
      if (action === "continue" && error.partial) {
        ui.remove();
        return error.partial;
      }
      ui.reset();
    }
  }
}
