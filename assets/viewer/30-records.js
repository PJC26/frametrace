const videos = Array.isArray(scan.videos) ? scan.videos : [];

const flsEntryByInode = new Map();
(DATA.flsEntries || []).forEach(entry => {
  if (entry.inode != null) flsEntryByInode.set(String(entry.inode), entry);
});

const TIME_PATTERNS = [
  // Dashcam style: 20260916-13h41m15s / 2026_09_16 13h41m15s
  { re: /(20\d{2})[_.\-]?(0[1-9]|1[0-2])[_.\-]?(0[1-9]|[12]\d|3[01])[ T_\-]+([01]\d|2[0-3])h([0-5]\d)m([0-5]\d)s/i, hasTime: true },
  { re: /(20\d{2})[_.\-]?(0[1-9]|1[0-2])[_.\-]?(0[1-9]|[12]\d|3[01])[ T_\-]+([01]\d|2[0-3])[:_.\-]?([0-5]\d)[:_.\-]?([0-5]\d)/, hasTime: true },
  { re: /(20\d{2})(0[1-9]|1[0-2])(0[1-9]|[12]\d|3[01])[ T_\-]+([01]\d|2[0-3])([0-5]\d)([0-5]\d)/, hasTime: true },
  { re: /(20\d{2})[_.\-]?(0[1-9]|1[0-2])[_.\-]?(0[1-9]|[12]\d|3[01])(?!\d)/, hasTime: false }
];

function parseTimeFromName(name) {
  const text = String(name || "");
  for (const pattern of TIME_PATTERNS) {
    const match = text.match(pattern.re);
    if (!match) continue;
    const [year, month, day] = [match[1], match[2], match[3]];
    const hh = match[4] ?? "00";
    const mm = match[5] ?? "00";
    const ss = match[6] ?? "00";
    const ts = new Date(+year, +month - 1, +day, +hh, +mm, +ss).getTime() / 1000;
    if (Number.isFinite(ts)) {
      // hasTime:false means only the date part matched — the 00:00:00
      // time is synthetic and must not be presented as a recording time.
      return { ts, date: `${year}-${month}-${day}`, source: pattern.hasTime ? "name" : "name-date" };
    }
  }
  return null;
}

function recordingTimeFor(record) {
  for (const candidate of [record.originalPath, record.name, record.path]) {
    const parsed = parseTimeFromName(candidate);
    if (parsed) return parsed;
  }
  if (record.kind === "video" && record.modifiedUnix) {
    const day = new Date(record.modifiedUnix * 1000).toLocaleDateString("sv-SE");
    return { ts: record.modifiedUnix, date: day, source: "mtime" };
  }
  return null;
}

function recTypeFor(record) {
  const text = `${record.originalPath || ""} ${record.originalPath ? "" : record.name || ""}`.toLowerCase();
  if (/(event|충격|사고|impact)/.test(text)) return "event";
  if (/(parking|주차)/.test(text)) return "parking";
  if (/(driving|주행|상시|continuous|normal)/.test(text)) return "driving";
  return "";
}

function recTypeLabel(recType) {
  return ({ driving: t("rectype.driving"), event: t("rectype.event"), parking: t("rectype.parking"), unclassified: t("rectype.unclassified") })[recType] || t("rectype.unclassified");
}

function channelCodeOf(record) {
  const name = `${record.originalPath || ""} ${record.name || ""}`;
  let match = name.match(/[_\-. ]([FRIB])(?:[_.\- ]|[a-z0-9]*$)/i);
  if (match) return match[1].toUpperCase();
  if (/front/i.test(name)) return "F";
  if (/rear/i.test(name)) return "R";
  if (/interior|inside/i.test(name)) return "I";
  return null;
}

function channelLabel(code) {
  return ({ F: t("chan.front"), R: t("chan.rear"), I: t("chan.interior"), B: t("chan.rear2") })[code] || code;
}

function channelFor(record) {
  const code = channelCodeOf(record);
  return code ? channelLabel(code) : null;
}

// Channel pairing for dual sync playback: two records mate when their
// filenames differ only by a channel token (_F/_R/_I/_B or front/rear/
// interior) inside the same folder. The channel token is stripped to form
// a shared key — `20240101_1200_F.mp4` and `20240101_1200_R.mp4` pair.
function pairKeyOf(record) {
  const p = String(record.originalPath || record.name || "").replace(/\\/g, "/");
  const dir = p.split("/").slice(0, -1).join("/").toLowerCase();
  let base = (p.split("/").pop() || "").replace(/\.[^.]*$/, "").toLowerCase();
  base = base
    .replace(/front|rear|interior|inside/g, "")
    .replace(/[_\-. ][frib](?=$|[_\-. ])/g, "")
    .replace(/^[ _\-.]+|[ _\-.]+$/g, "");
  return base ? `${dir}|${base}` : "";
}

// PERF-06: pair key and channel code are derived from immutable record
// fields, so they are computed once at load (record.pairKey /
// record.channelCode) and indexed in recordsByPairKey — a dual-mode detail
// render is an O(1) bucket lookup instead of a full-record scan that
// re-ran both regex derivations per record per click. mediaSrcFor stays
// per-call because it depends on the session proxy choice.
function matesOf(record) {
  const code = record.channelCode;
  const key = record.pairKey;
  if (!code || !key) return [];
  const bucket = recordsByPairKey.get(key);
  if (!bucket) return [];
  return bucket.filter(r =>
    r.id !== record.id &&
    r.channelCode && r.channelCode !== code &&
    mediaSrcFor(r)
  );
}

// Dual-channel panes: any pane can drive — user events are mirrored to the
// others. A suppress flag stops echo loops, and a drift poll re-locks
// panes that wander (>0.3s) without a user event (decoder jitter,
// background tab throttling).
function wireDualSync(videos) {
  const list = Array.from(videos);
  if (list.length < 2) return;
  let suppress = false;
  const release = () => setTimeout(() => { suppress = false; }, 250);
  list.forEach(src => {
    src.addEventListener("play", () => {
      if (suppress) return;
      suppress = true;
      list.forEach(v => { if (v !== src) { v.currentTime = src.currentTime; v.play().catch(() => {}); } });
      release();
    });
    src.addEventListener("pause", () => {
      if (suppress) return;
      suppress = true;
      list.forEach(v => { if (v !== src) v.pause(); });
      release();
    });
    src.addEventListener("seeked", () => {
      if (suppress) return;
      suppress = true;
      list.forEach(v => { if (v !== src) v.currentTime = src.currentTime; });
      release();
    });
    src.addEventListener("ratechange", () => {
      if (suppress) return;
      suppress = true;
      list.forEach(v => { if (v !== src) v.playbackRate = src.playbackRate; });
      release();
    });
  });
  const drift = setInterval(() => {
    if (!list[0].isConnected) { clearInterval(drift); return; }
    const driver = list.find(v => !v.paused) || list[0];
    list.forEach(v => {
      if (v !== driver && Math.abs(v.currentTime - driver.currentTime) > 0.3) {
        v.currentTime = driver.currentTime;
      }
    });
  }, 500);
}

function prefixFor(record) {
  const name = String(record.originalPath || record.name || "");
  const match = name.match(/[A-Za-z가-힣_\-]+/);
  return match ? match[0].replace(/[_\-]+$/, "") || t("misc.other") : t("misc.other");
}

function originalNameFor(record) {
  const path = originalPathFor(record);
  if (path) {
    const base = path.replace(/\\/g, "/").split("/").pop();
    if (base && base.includes(".")) return base;
  }
  return record.name || "";
}

function originalPathFor(record) {
  if (record.inode && flsEntryByInode.has(String(record.inode))) {
    return flsEntryByInode.get(String(record.inode)).path || "";
  }
  // Recovered outputs are named inode_<num>.bin; map that back to the
  // original path recorded by inspect-image.
  const match = String(record.name || "").match(/inode_(\d+)/);
  if (match && flsEntryByInode.has(match[1])) {
    return flsEntryByInode.get(match[1]).path || "";
  }
  return "";
}

function fmtUnix(value) {
  if (!Number.isFinite(value)) return "-";
  return new Date(value * 1000).toLocaleString();
}

function normalizePath(value) {
  let text = String(value || "");
  if (text.slice(0, 4).toLowerCase() === EXT_PREFIX) text = text.slice(4);
  return text.split(BS).join("/").toLowerCase();
}

// Carve/recovery logs can carry paths relative to the case dir or its
// parent ("<case-name>\artifacts\..."). Absolutize against the case dir so
// downloads, /media playback, and export resolve identically under file://
// and served modes — a relative path would otherwise produce a dead
// file:/// URL or a 404 against the server's cwd.
const caseDirPath = String(DATA.caseDir || "").replace(/[\\\/]+$/, "");
const caseDirName = caseDirPath.split(/[\\\/]/).filter(Boolean).pop() || "";
function absolutizeArtifactPath(path) {
  const norm = String(path || "").split("/").join(BS);
  if (!norm) return norm;
  // Already absolute: "C:\…", "\\?\C:\…", or UNC "\\host\share".
  if (/^[A-Za-z]:[\\\/]/.test(norm) || norm.startsWith(BS + BS)) return norm;
  if (!caseDirPath) return norm;
  // "<case-name>\…" was logged relative to the case directory's parent.
  if (caseDirName && (norm === caseDirName || norm.startsWith(caseDirName + BS))) {
    return caseDirPath.slice(0, caseDirPath.length - caseDirName.length - 1) + BS + norm;
  }
  return caseDirPath + BS + norm;
}

function fileUrl(path) {
  if (!path) return "";
  let value = String(path);
  if (value.startsWith("file:")) return value;
  if (value.slice(0, 8).toLowerCase() === EXT_UNC) value = BS + BS + value.slice(8);
  else if (value.slice(0, 4).toLowerCase() === EXT_PREFIX) value = value.slice(4);
  const normalized = value.split(BS).join("/");
  // encodeURI leaves #, ?, and & unescaped, which truncates file URLs whose
  // evidence paths contain them; encode per segment instead, keeping the
  // drive-letter colon literal.
  const encodeSegment = segment => (/^[A-Za-z]:$/.test(segment) ? segment : encodeURIComponent(segment));
  const encoded = normalized.split("/").map(encodeSegment).join("/");
  if (normalized.length > 2 && normalized[1] === ":" && normalized[2] === "/") return "file:///" + encoded;
  if (normalized.startsWith("//")) return "file:" + encoded;
  if (normalized.startsWith("/")) return "file://" + encoded;
  return encoded;
}

// Offline sprite icon (assets/icons.svg, inlined into every viewer page).
function ftIcon(name) {
  return `<svg class="ic" aria-hidden="true"><use href="#i-${name}"/></svg>`;
}

function escapeHtml(value) {
  return String(value ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;");
}

// --- Browser playability gate: mirrors transcode::unplayable_reason so the
// viewer never hands a container Chromium cannot decode (AVI, DAV, .264 …)
// to <video> and silently shows a dead player. ---
const BROWSER_CODECS = ["h264", "vp8", "vp9", "av1", "theora"];
const BROWSER_CONTAINERS = ["mp4", "mov", "webm", "matroska", "ogg"];
const PROPRIETARY_EXTS = ["dav", "nov", "ave", "h264", "264", "h265", "sec", "ts"];
// Extension allowlist for records with no probe data (carved/recovered
// outputs): anything else (bin, avi, dat, tmp …) cannot be decoded.
const PLAYABLE_EXTS = ["mp4", "m4v", "mov", "webm", "mkv", "ogv", "ogg", "mp3", "m4a", "wav"];

function extOfPath(path) {
  const name = String(path || "").split(/[\\\/]/).pop() || "";
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

function needsProxy(record) {
  const ext = record.ext || "";
  if (PROPRIETARY_EXTS.includes(ext)) return "proprietary extension ." + ext;
  if (record.probeOk === false) return "ffprobe parse failed — proprietary or corrupt container";
  const format = String(record.container || "").toLowerCase();
  if (format && !BROWSER_CONTAINERS.some(c => format.includes(c))) return "container '" + format + "' not browser-playable";
  const codec = String(record.codec || "").toLowerCase();
  if (codec && codec !== "-" && !BROWSER_CODECS.includes(codec)) return "codec '" + codec + "' not browser-playable";
  // Extension allowlist is the last line for records with no positive
  // probe evidence (carved/recovered outputs, unprobed files): a playable
  // container inside an odd extension (e.g. mp4 bytes in a .dat) still
  // plays, so only flag when the probe didn't already prove playability.
  const probedPlayable = record.probeOk === true
    && (!format || BROWSER_CONTAINERS.some(c => format.includes(c)))
    && (!codec || codec === "-" || BROWSER_CODECS.includes(codec));
  if (ext && !PLAYABLE_EXTS.includes(ext) && !probedPlayable) return "extension '." + ext + "' not browser-playable";
  return "";
}

// Artifact filenames sanitize selectors (video_export::sanitize_filename) —
// mirror that mapping so a stored proxy can be matched back to its record.
function sanitizeSelector(value) {
  return String(value || "").replace(/[^A-Za-z0-9_-]/g, "_") || "clip";
}

function proxyPathFor(record) {
  // Session-stored choice wins; otherwise fall back to a proxy that already
  // existed on disk when the review bundle was generated.
  return state.proxies?.[record.id] || record.proxyPath || "";
}

// When the viewer is served by the local examiner workstation (http://127.0.0.1),
// file:// video sources are blocked by the browser; route playback through the
// server's Range-enabled /media endpoint instead. Opening the page directly
// from disk keeps the original file:// URL. An unplayable container prefers
// its generated proxy in both modes — the original would never decode.
function mediaSrcFor(record) {
  const proxy = proxyPathFor(record);
  const unplayable = needsProxy(record);
  const preferProxy = !!proxy && (!!state.proxies?.[record.id] || !!unplayable);
  // An unplayable record without a proxy returns "" so the detail pane
  // renders the auto-build/offline-guidance fallback instead of a player
  // that can never decode the source.
  if (unplayable && !proxy) return "";
  if (location.protocol === "http:" || location.protocol === "https:") {
    if (preferProxy) return "/media?path=" + encodeURIComponent(proxy);
    if (record.path) return "/media?path=" + encodeURIComponent(record.path);
    return "";
  }
  if (preferProxy) return fileUrl(proxy);
  if (!record.fileUrl) return "";
  return record.fileUrl;
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function highlightEscape(value, query) {
  const safe = escapeHtml(value);
  if (!query) return safe;
  try {
    return safe.replaceAll(new RegExp("(" + escapeRegExp(escapeHtml(query)) + ")", "gi"), "<mark>$1</mark>");
  } catch (error) {
    return safe;
  }
}

function fmtBytes(value) {
  if (!Number.isFinite(value)) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let current = value;
  let unit = 0;
  while (current >= 1024 && unit < units.length - 1) { current /= 1024; unit += 1; }
  return `${current.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function fmtDuration(value) {
  if (!Number.isFinite(value)) return "-";
  const seconds = Math.round(value);
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
}

function statusLabel(status) {
  if (status === "ffprobe-video-stream-confirmed") return t("status.verified");
  if (status === "ffprobe-confirmed") return t("status.confirmed");
  if (status === "validation-failed") return t("status.failed");
  if (status === "candidate-unvalidated") return t("status.candidate");
  if (status === "duplicate-candidate") return t("status.duplicate");
  return status;
}

function statusClass(status) {
  if (status === "ffprobe-video-stream-confirmed" || status === "ffprobe-confirmed") return "ok";
  if (status === "validation-failed") return "failed";
  return "candidate";
}

function toast(message) {
  const host = document.getElementById("toastHost");
  const item = document.createElement("div");
  item.className = "toast";
  item.textContent = message;
  host.appendChild(item);
  setTimeout(() => item.remove(), 3200);
}

function storageGet(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    return raw ? JSON.parse(raw) : fallback;
  } catch (error) {
    return fallback;
  }
}

function storageSet(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
    return true;
  } catch (error) {
    toast(tf("toast.storageFail", { err: error.message }));
    return false;
  }
}

// Frame rate for frame-stepping: the server's `fps` field when present,
// else avg_frame_rate / r_frame_rate ("30000/1001") of the first video
// stream in the raw ffprobe JSON; undefined when unknown (the player then
// falls back to 1/30 s).
function parseRate(value) {
  if (typeof value === "number") return Number.isFinite(value) && value > 0 ? value : undefined;
  const m = /^\s*(\d+(?:\.\d+)?)\s*(?:\/\s*(\d+(?:\.\d+)?))?\s*$/.exec(String(value ?? ""));
  if (!m) return undefined;
  const den = m[2] === undefined ? 1 : Number(m[2]);
  if (!den) return undefined;
  const fps = Number(m[1]) / den;
  return Number.isFinite(fps) && fps > 0 ? fps : undefined;
}
function videoFps(video) {
  const direct = parseRate(video.fps);
  if (direct) return direct;
  let probe = video.ffprobe;
  if (typeof probe === "string") { try { probe = JSON.parse(probe); } catch { probe = null; } }
  const stream = (probe && Array.isArray(probe.streams) ? probe.streams : []).find(st => st && st.codec_type === "video");
  if (!stream) return undefined;
  return parseRate(stream.avg_frame_rate) || parseRate(stream.r_frame_rate);
}

const validationsByPath = new Map(validationLog.map(item => [normalizePath(item.target_path), item]));
const recoveredFilesystemLog = filesystemLog.filter(item => item.event === "recover-inode" && item.output_path);
// Case-level warnings recorded on inspection events (e.g. a truncated
// fls listing means the deleted-file list is incomplete) — the reviewer
// must see them, not just the CLI.
const inspectionWarnings = filesystemLog
  .filter(item => item.event === "inspect-image-filesystem" && Array.isArray(item.warnings) && item.warnings.length)
  .flatMap(item => item.warnings);

const records = [
  ...videos.map(video => {
    const validation = validationsByPath.get(normalizePath(video.source_path));
    return {
      id: video.id,
      kind: "video",
      name: video.relative_path || video.id,
      path: video.source_path,
      fileUrl: video.file_url,
      parser: video.source_profile?.parser || "-",
      vendor: video.source_profile?.vendor || "-",
      status: validation?.validation_status || (video.ffprobe_ok ? "ffprobe-confirmed" : "candidate-unvalidated"),
      sha256: validation?.target_sha256 || video.sha256 || video.hash_status || "-",
      duration: validation?.duration_seconds ?? video.duration_seconds,
      codec: validation?.video_codec || video.video_codec || "-",
      audioCodec: video.audio_codec || "",
      ext: video.extension || extOfPath(video.relative_path || video.source_path),
      container: video.format_name || "",
      probeOk: video.ffprobe_ok ?? validation?.ffprobe_ok,
      size: video.size_bytes,
      note: validation?.validation_note || video.source_profile?.recommended_action || "-",
      indexStatus: video.index_status || "active",
      modifiedUnix: video.modified_unix,
      inode: video.inode,
      fps: videoFps(video),
      validation
    };
  }),
  ...carveLog.map(item => {
    const validation = validationsByPath.get(normalizePath(item.output_path));
    return {
      id: item.id || item.output_path,
      kind: "carved",
      name: item.output_path ? item.output_path.split(/[\\\/]/).pop() : item.id,
      path: absolutizeArtifactPath(item.output_path),
      fileUrl: fileUrl(absolutizeArtifactPath(item.output_path)),
      parser: item.signature || "carve",
      vendor: "Recovered candidate",
      status: validation?.validation_status || item.validation_status || "candidate-unvalidated",
      sha256: validation?.target_sha256 || item.sha256 || "-",
      duration: validation?.duration_seconds,
      // An extension is not a codec — falling back to it marks carved
      // mp4/webm artifacts "codec not browser-playable" and kills inline
      // playback of files Chromium could decode. Unprobed records rely on
      // the PLAYABLE_EXTS allowlist in needsProxy instead.
      codec: validation?.video_codec || "-",
      ext: item.extension || extOfPath(item.output_path),
      container: "",
      probeOk: undefined,
      size: item.size_bytes,
      note: validation?.validation_note || item.validation_note || "-",
      offset: item.offset,
      indexStatus: "active",
      validation
    };
  }),
  ...(DATA.flsEntries || [])
    // Deleted entries are forensic evidence regardless of extension — a
    // dashcam's in-progress recording temp files (20260917112038816) have
    // no extension at all. Non-video candidates stay flagged honestly.
    .filter(entry => entry.deleted)
    .map(entry => {
      const inode = entry.inode != null ? String(entry.inode) : "";
      return {
        id: `fls:${inode || entry.raw_line || "unknown"}`,
        kind: "candidate",
        name: entry.path || entry.raw_line || `inode ${inode}`,
        path: "",
        fileUrl: "",
        parser: "fls listing",
        vendor: t("cand.vendor"),
        ext: extOfPath(entry.path),
        videoCandidate: !!entry.video_candidate,
        status: "candidate-unvalidated",
        sha256: "-",
        duration: null,
        codec: "-",
        size: null,
        note: entry.video_candidate ? t("cand.note") : t("cand.noteNonVideo"),
        indexStatus: "recovery-pending",
        modifiedUnix: null,
        inode: entry.inode,
        originalPath: entry.path || "",
        validation: null
      };
    }),
  ...recoveredFilesystemLog.map(item => {
    const original = item.inode && flsEntryByInode.get(String(item.inode));
    const validation = validationsByPath.get(normalizePath(item.output_path));
    return {
      id: `inode:${item.partition_offset ?? 0}:${item.inode || item.output_path}`,
      kind: "filesystem",
      name: item.output_path ? item.output_path.split(/[\\\/]/).pop() : item.inode,
      path: absolutizeArtifactPath(item.output_path),
      fileUrl: fileUrl(absolutizeArtifactPath(item.output_path)),
      parser: "tsk/icat",
      vendor: "Filesystem recovery",
      status: validation?.validation_status || item.validation_status || "candidate-unvalidated",
      sha256: validation?.target_sha256 || item.sha256 || "-",
      duration: validation?.duration_seconds,
      codec: validation?.video_codec || "-",
      ext: extOfPath(item.output_path),
      container: "",
      probeOk: undefined,
      size: item.size_bytes,
      note: validation?.validation_note || "Recovered inode output; validate before final reporting.",
      offset: item.partition_offset,
      inode: item.inode,
      originalPath: original?.path || "",
      warnings: Array.isArray(item.warnings) ? item.warnings : [],
      indexStatus: "active",
      validation
    };
  })
];

records.forEach(record => {
  record.originalPath = record.originalPath || originalPathFor(record);
  const rec = recordingTimeFor(record);
  record.recTime = rec ? rec.ts : null;
  record.recDay = rec ? rec.date : null;
  record.recSource = rec ? rec.source : null;
  record.channelCode = channelCodeOf(record);
  record.channel = record.channelCode ? channelLabel(record.channelCode) : null;
  record.pairKey = pairKeyOf(record);
  record.prefix = prefixFor(record);
  record.recType = recTypeFor(record);
  record.originalName = originalNameFor(record);
  record.thumb = DATA.thumbs?.[record.id] || null;
  // Artifact filenames sanitize record ids (inode:<off>:<ino> →
  // inode_<off>_<ino>) — mirror the same mapping for the lookup.
  record.dfl = DATA.deepfake?.[String(record.id).replace(/[:\\\/]/g, "_")] || null;
  record.telemetry = DATA.telemetry?.[String(record.id).replace(/[:\\\/]/g, "_")] || null;
  const fromId = anomaliesBySelector.get(record.id) || [];
  const fromValidation = Array.isArray(record.validation?.anomaly_flags)
    ? record.validation.anomaly_flags.map(kind => ({ kind, selector: record.id, detail: "validation anomaly_flags" }))
    : [];
  const merged = [...fromId];
  for (const item of fromValidation) {
    if (!merged.some(existing => existing.kind === item.kind)) merged.push(item);
  }
  record.anomalies = merged;
  record.hasAnomaly = record.anomalies.length > 0;
});
// Carve-log and inode records duplicate indexed files under different ids;
// reuse the indexed video's thumbnail through the shared file name.
const thumbByName = new Map(records.filter(record => record.thumb).map(record => [record.name, record.thumb]));
records.forEach(record => {
  if (!record.thumb && record.name) record.thumb = thumbByName.get(record.name) || null;
});
// Pre-built proxies embedded in the bundle (artifacts/proxies listing):
// match a record by the sanitized filename prefix its proxy was written
// under — sanitize(id) for indexed videos, sanitize(path) for the rest.
// PERF-06: index proxy files by every "<key>_proxy_" prefix their name
// carries (a sanitized key may itself contain "_proxy_"), keeping the
// first listing position per prefix. Each record then resolves with Map
// lookups — the earliest-listed match wins, exactly as the former
// per-record linear find did, without the O(records x proxies) scan.
const proxyFiles = Array.isArray(DATA.proxies) ? DATA.proxies : [];
const proxyIndexByPrefix = new Map();
proxyFiles.forEach((path, index) => {
  const name = String(path).split(/[\\\/]/).pop() || "";
  if (!name.endsWith(".mp4")) return;
  for (let at = name.indexOf("_proxy_"); at >= 0; at = name.indexOf("_proxy_", at + 1)) {
    const prefix = name.slice(0, at);
    if (!proxyIndexByPrefix.has(prefix)) proxyIndexByPrefix.set(prefix, index);
  }
});
records.forEach(record => {
  let best = -1;
  for (const key of [record.id, record.path].filter(Boolean).map(sanitizeSelector)) {
    const index = proxyIndexByPrefix.get(key);
    if (index != null && (best < 0 || index < best)) best = index;
  }
  record.proxyPath = best >= 0 ? proxyFiles[best] : "";
});

// O(1) lookups for the hot paths (selectedRecord, selectedRecords,
// dual-channel pairing) — records never change after load.
const recordById = new Map();
const recordIndexById = new Map();
const recordsByPairKey = new Map();
records.forEach((record, index) => {
  if (!recordById.has(record.id)) {
    recordById.set(record.id, record);
    recordIndexById.set(record.id, index);
  }
  if (record.pairKey && record.channelCode) {
    const bucket = recordsByPairKey.get(record.pairKey);
    if (bucket) bucket.push(record); else recordsByPairKey.set(record.pairKey, [record]);
  }
});

const state = {
  activeId: records[0]?.id || null,
  selectedIds: new Set(),
  lastCheckedKey: null,
  drafts: storageGet(ANNOTATIONS_KEY, {}),
  deletedIds: [],
  examiners: {},
  marks: storageGet(MARKS_KEY, {}),
  notes: storageGet(NOTES_KEY, {}),
  ranges: storageGet(RANGES_KEY, {}),
  proxies: storageGet(PROXIES_KEY, {}),
  examiner: storageGet(EXAMINER_KEY, ""),
  tags: storageGet(TAGS_KEY, {}),
  tagPresets: storageGet(TAG_PRESETS_KEY, null),
  // A host-supplied locale (FTViewer.init({ locale })) wins and is remembered.
  locale: opts.locale || (storageGet(LOCALE_KEY, "ko") === "en" ? "en" : "ko"),
  layout: Object.assign({ videoMode: "fit", videoZoom: 100, theater: false, dual: false, stageH: 0, colSplit: 0, rate: 1 }, storageGet(LAYOUT_KEY, {})),
  currentPage: 1,
  pageSize: 100,
  query: "",
  kind: "",
  status: "",
  chip: "",
  chipsExpanded: false,
  calMonth: "",
  sortBy: "id",
  groupBy: "none",
  dateFrom: "",
  dateTo: "",
  recType: "",
  collapsedGroups: new Set()
};

function touchAnnotation(id) {
  state.examiners[id] = (state.examiner || "").trim() || state.examiners[id] || null;
  const draft = {
    mark: state.marks[id] || null,
    note: state.notes[id] || "",
    tags: state.tags[id] || [],
    examiner: state.examiners[id]
  };
  state.drafts[id] = draft;
  state.deletedIds = [...new Set([...state.deletedIds, id])];
  storageSet(ANNOTATIONS_KEY, state.drafts);
  markReviewDirty();
}

function hydrateAnnotations(annotations) {
  state.marks = {}; state.notes = {}; state.tags = {}; state.examiners = {};
  for (const mark of annotations.marks || []) {
    if (mark.status !== "noted") state.marks[mark.id] = { status: mark.status, marked_unix: mark.marked_unix };
    state.notes[mark.id] = mark.note || "";
    state.examiners[mark.id] = mark.examiner || null;
  }
  for (const entry of annotations.tags || []) state.tags[entry.id] = entry.tags;
  state.deletedIds = [];
  for (const [id, draft] of Object.entries(state.drafts)) {
    if (draft.mark) state.marks[id] = draft.mark;
    else delete state.marks[id];
    state.notes[id] = draft.note;
    state.tags[id] = draft.tags;
    state.examiners[id] = draft.examiner;
    state.deletedIds.push(id);
  }
}

hydrateAnnotations(DATA.annotations || { marks: [], tags: [] });

// null = never customized → seed from defaults; an empty array is a
// deliberate "no presets" choice and must not reseed.
if (!Array.isArray(state.tagPresets)) state.tagPresets = [...DEFAULT_TAG_PRESETS];
if (opts.locale) storageSet(LOCALE_KEY, state.locale);

// Same-origin hosts (e.g. the examiner workstation iframe) read live case
// stats through this getter instead of re-parsing the embedded data.
window.__frametraceSummary = () => {
  const byKind = {};
  const byStatus = {};
  let warned = 0;
  records.forEach(r => {
    byKind[r.kind || "video"] = (byKind[r.kind || "video"] || 0) + 1;
    byStatus[r.status] = (byStatus[r.status] || 0) + 1;
    if ((r.warnings || []).length) warned += 1;
  });
  return {
    total: records.length,
    byKind,
    byStatus,
    warned,
    marked: Object.keys(state.marks).length,
    noted: records.filter(r => (state.notes[r.id] || "").trim()).length
  };
};
