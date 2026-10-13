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
      // wall: the device clock exactly as written. `ts` is a browser-local
      // instant used only to sort; a wall time the local zone skips (DST
      // spring-forward, e.g. 02:30 in Europe/Berlin on 2026-03-29) would
      // come back shifted from it, so labels are built from `wall`.
      return { ts, date: `${year}-${month}-${day}`, wall: `${year}-${month}-${day}T${hh}:${mm}:${ss}`, source: pattern.hasTime ? "name" : "name-date" };
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

// Recording-time text with an explicit time basis (same rule as the case
// report's "UTC (현지 … UTC±HH:MM)" for real instants):
//   name       the device's clock as written in the file name — a wall
//              time with NO known zone (parseTimeFromName builds it as a
//              browser-local date only to sort/bucket it): shown as is,
//              labelled "기기 기록 시각, 시간대 미상", never with an offset.
//   name-date  the same, date only.
//   mtime      a real instant (file modification time): this computer's
//              local time with its UTC offset, marked as an estimate.
// opts.iso: machine form for exports — naive "YYYY-MM-DDTHH:mm:ss" (no Z),
// an instant as ISO 8601 with its offset.
function recTimeParts(unix) {
  const d = new Date(unix * 1000);
  const p = n => String(n).padStart(2, "0");
  const off = -d.getTimezoneOffset();
  const sign = off >= 0 ? "+" : "-";
  return {
    date: `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`,
    time: `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`,
    offset: `${sign}${p(Math.floor(Math.abs(off) / 60))}:${p(Math.abs(off) % 60)}`
  };
}
// A naive (file-name) time is formatted from its parsed fields
// (record.recWall), never from a local Date: a wall time the local zone
// skips or repeats (DST) must read exactly as the device wrote it.
function recWallParts(record) {
  const m = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2}:\d{2})$/.exec(String(record.recWall || ""));
  if (m) return { date: m[1], time: m[2] };
  if (record.recDay && record.recSource === "name-date") return { date: record.recDay, time: "00:00:00" };
  return null;
}
function recTimeText(record, opts) {
  if (!record || !Number.isFinite(record.recTime)) return "";
  const iso = !!(opts && opts.iso);
  if (record.recSource === "name" || record.recSource === "name-date") {
    const wall = recWallParts(record) || recTimeParts(record.recTime);
    if (record.recSource === "name-date") return iso ? wall.date : `${wall.date} ${t("time.naiveDate")}`;
    return iso ? `${wall.date}T${wall.time}` : `${wall.date} ${wall.time} ${t("time.naive")}`;
  }
  const { date, time, offset } = recTimeParts(record.recTime);
  return iso ? `${date}T${time}${offset}` : `${date} ${time} UTC${offset} ${t("detail.est")}`;
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
  if (status === "derived") return t("status.derived");
  return status;
}

function statusClass(status) {
  if (status === "ffprobe-video-stream-confirmed" || status === "ffprobe-confirmed") return "ok";
  if (status === "validation-failed") return "failed";
  if (status === "derived") return "derived";
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

// Quota exhaustion (tens of thousands of drafts in one case) is reported
// once per session; the live condition is tracked by draftsUnpersisted
// (save indicator title + beforeunload warning), never by a toast storm.
var storageQuotaToasted = false;
function isQuotaError(error) {
  return !!error && (error.name === "QuotaExceededError" || error.name === "NS_ERROR_DOM_QUOTA_REACHED" || error.code === 22 || error.code === 1014);
}
var storageFailToasted = false;
function reportStorageError(error) {
  if (isQuotaError(error)) {
    if (!storageQuotaToasted) {
      storageQuotaToasted = true;
      toast(t("toast.storageQuota"));
    }
  } else if (!storageFailToasted) {
    storageFailToasted = true;
    toast(tf("toast.storageFail", { err: error && error.message }));
  }
}
function storageSet(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
    return true;
  } catch (error) {
    reportStorageError(error);
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

// Lossless joins (artifacts/joined/join-log.jsonl, docs/PLAN_PHASE2_JOIN_AUDIO
// §1.5/§1.8): one derived record per output file; a later entry for the same
// output path supersedes an earlier one.
const joinLog = [...new Map(
  (Array.isArray(DATA.joinLog) ? DATA.joinLog : [])
    .filter(item => item && item.output_path)
    .map(item => [normalizePath(item.output_path), item])
).values()];

// Gaps/overlaps of a join (log entry, plan or manifest totals) are not
// determinable when it was ordered by file name only: the server counts
// such junctions as `unknown_junctions` (older logs: order_basis).
function joinJunctionsUnknown(item) {
  if (!item || typeof item !== "object") return false;
  const totals = item.totals && typeof item.totals === "object" ? item.totals : {};
  return Number(item.unknown_junctions ?? totals.unknown_junctions) > 0 ||
    String(item.order_basis || "") === "filename-order";
}

// Measured sound (§2.4): the server already dropped measurements that no
// longer match the file's bytes ("stale") — the viewer only maps them.
const audioLevels = DATA.audioLevels && typeof DATA.audioLevels === "object" ? DATA.audioLevels : {};
// The measurement log could not be read (records-meta audioLevelsError):
// the map is empty for that reason, so a track reads "측정 결과를 읽지
// 못함", never "미측정".
const audioLevelsError = DATA.audioLevelsError ? String(DATA.audioLevelsError) : "";

// §2.5 five states: sound | silent (valid measurement) · none (probed, no
// audio stream) · track (stream present, sound not measured / failed /
// changed since) · unknown (never probed). Never claims a sound that was
// not measured.
function audioInfoFor(record) {
  const codec = String(record.audioCodec || "").toLowerCase();
  const hasTrack = !!codec && codec !== "none" && codec !== "-";
  const level = Object.prototype.hasOwnProperty.call(audioLevels, record.id) ? audioLevels[record.id] : null;
  const r = level && typeof level === "object" ? String(level.r || "") : "";
  if (r === "sound" || r === "silent") return { state: r, level, reason: "" };
  if (hasTrack) return { state: "track", level: null, reason: r === "error" || r === "stale" ? r : audioLevelsError ? "unread" : "" };
  if (record.probeOk === true) return { state: "none", level: null, reason: "" };
  return { state: "unknown", level: null, reason: "" };
}
// Facet value of a record: track and unknown both read "미확인".
function audioFacetOf(state) {
  return state === "sound" || state === "silent" || state === "none" ? state : "unknown";
}
const AUDIO_FACETS = ["sound", "silent", "none", "unknown"];
// dBFS with a real minus sign; -inf (digital zero) arrives as null/"-inf".
function fmtDb(value, digits) {
  const v = typeof value === "string" && /^-inf/i.test(value) ? -Infinity : Number(value);
  if (value == null || v === -Infinity) return "−∞";
  if (!Number.isFinite(v)) return "?";
  const text = Math.abs(v).toFixed(digits);
  return (v < 0 && Number(text) !== 0 ? "−" : "") + text;
}
function audioTipFor(record) {
  const level = record.audioLevel || {};
  switch (record.audioState) {
    case "sound": return tf("audio.tip.sound", { max: fmtDb(level.max, 0) });
    case "silent": return tf("audio.tip.silent", { max: fmtDb(level.max, 0) });
    case "none": return t("audio.tip.none");
    case "track": return t(record.audioReason === "error" ? "audio.tip.trackError" : record.audioReason === "stale" ? "audio.tip.trackStale" : record.audioReason === "unread" ? "audio.tip.trackUnread" : "audio.tip.track");
    default: return "";
  }
}
function audioDetailFor(record) {
  const level = record.audioLevel || {};
  const codec = record.audioCodec ? ` (${record.audioCodec})` : "";
  switch (record.audioState) {
    case "sound": return tf("audio.detail.sound", { max: fmtDb(level.max, 1), mean: fmtDb(level.mean, 1), codec });
    case "silent": return tf("audio.detail.silent", { max: fmtDb(level.max, 1) });
    case "none": return t("audio.detail.none");
    case "track": return tf(record.audioReason === "error" ? "audio.detail.trackError" : record.audioReason === "stale" ? "audio.detail.trackStale" : record.audioReason === "unread" ? "audio.detail.trackUnread" : "audio.detail.track", { codec });
    default: return t("audio.unknown");
  }
}

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
  }),
  ...joinLog.map(item => {
    const path = absolutizeArtifactPath(item.output_path);
    const name = path.split(/[\\\/]/).pop() || String(item.output_path);
    const sources = Array.isArray(item.source_ids) ? item.source_ids.map(String) : [];
    // The server names the record ("join:<16 hex of the output sha>"), so
    // marks saved on it survive a rename of the output; logs written before
    // record_id existed fall back to the output file name.
    const serverId = typeof item.record_id === "string" ? item.record_id.trim() : "";
    return {
      id: serverId || `join:${name}`,
      kind: "joined",
      name,
      path,
      fileUrl: fileUrl(path),
      parser: "ffmpeg concat (stream copy)",
      vendor: t("joined.vendor"),
      status: "derived",
      sha256: item.output_sha256 || "-",
      duration: Number.isFinite(item.duration_seconds) ? item.duration_seconds : null,
      codec: "-",
      ext: extOfPath(path),
      container: "",
      probeOk: undefined,
      size: item.output_size_bytes,
      // A filename-order join cannot tell gaps from overlaps: never "0".
      note: joinJunctionsUnknown(item)
        ? tf("joined.noteUnknown", {
          n: item.segment_count ?? sources.length,
          manifest: String(item.manifest_path || "-").split(/[\\\/]/).pop()
        })
        : tf("joined.note", {
          n: item.segment_count ?? sources.length,
          gaps: item.gaps ?? 0,
          overlaps: item.overlaps ?? 0,
          manifest: String(item.manifest_path || "-").split(/[\\\/]/).pop()
        }),
      indexStatus: "active",
      joinSources: sources,
      joinInfo: item,
      validation: null
    };
  })
];

records.forEach(record => {
  record.originalPath = record.originalPath || originalPathFor(record);
  const rec = recordingTimeFor(record);
  record.recTime = rec ? rec.ts : null;
  record.recDay = rec ? rec.date : null;
  record.recWall = rec && rec.wall ? rec.wall : null;
  record.recSource = rec ? rec.source : null;
  if (record.kind === "joined") {
    // The plan's channel (§1.8), not a guess from the output file name; a
    // joined file never pairs for dual playback.
    const code = String(record.joinInfo.channel || "").toUpperCase();
    record.channelCode = /^[FRIB]$/.test(code) ? code : null;
    record.pairKey = "";
  } else {
    record.channelCode = channelCodeOf(record);
    record.pairKey = pairKeyOf(record);
  }
  record.channel = record.channelCode ? channelLabel(record.channelCode) : null;
  const audio = audioInfoFor(record);
  record.audioState = audio.state;
  record.audioLevel = audio.level;
  record.audioReason = audio.reason;
  record.audioFacet = audioFacetOf(audio.state);
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
// A joined record's recording time is its first segment's (§1.8).
{
  const videoById = new Map(records.filter(record => record.kind === "video").map(record => [record.id, record]));
  records.forEach(record => {
    if (record.kind !== "joined") return;
    const first = videoById.get(record.joinSources[0]);
    if (!first || first.recTime == null) return;
    record.recTime = first.recTime;
    record.recDay = first.recDay;
    record.recWall = first.recWall;
    record.recSource = first.recSource;
  });
}
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
  // Per-id drafts this window replays or wrote (hydrateAnnotations fills it).
  drafts: {},
  // Record ids edited since the server last confirmed them (save protocol
  // v2, docs/FIX_PLAN_R1_2026-10-13.md §1.6): the save payload is built
  // from this set alone.
  dirtyIds: new Set(),
  // Per-record annotation revision the server reported last (records-meta /
  // the exported bundle seed it; every applied save bumps it). Sent back as
  // base_revisions so a stale window can never silently overwrite another.
  revisions: {},
  examiners: {},
  // marks / notes / tags come from the case data + the per-id drafts
  // (hydrateAnnotations); the old whole-map keys are no longer written.
  marks: {},
  notes: {},
  ranges: storageGet(RANGES_KEY, {}),
  proxies: storageGet(PROXIES_KEY, {}),
  examiner: storageGet(EXAMINER_KEY, ""),
  tags: {},
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
  audio: "",
  collapsedGroups: new Set()
};
// The stored layout is browser data: a playback rate is only ever a finite
// number in the player's range (U10), never a string carried into
// video.playbackRate or the pop-out player's inline script.
{
  const r = Number(state.layout.rate);
  state.layout.rate = Number.isFinite(r) && r >= 0.25 && r <= 4 ? r : 1;
}

// --- Drafts (docs/FIX_PLAN_R1_2026-10-13.md §1.6, §10.3) --------------------
// A draft is the full annotation of one record as this window last edited
// it, stored under its OWN key ft.draft.<case>.<writer>.<id> =
//   { mark, note, tags, examiner, range, seq, base, conflict? }
//   seq       per-window counter: a save forgets a draft only while the seq
//             it sent is still the current one (an edit made mid-flight
//             survives the save)
//   base      the server revision the edit was made on. It travels as the
//             id's base_revision, so a draft adopted from a dead window is
//             refused (conflict) when anyone saved that id since — an
//             adopted draft can never overwrite silently.
//   conflict  { server, server_revision, queued, mine? } while the server
//             refused the id: a reload restores the conflict banner from it.
// One key per id keeps every write O(1) and a window only ever writes its
// own keys. Writes are batched into one debounced flush (a 30k-record bulk
// mark is one flush, not 30k). The pre-§10.3 single-key store
// (ANNOTATIONS_KEY) is migrated once at boot (migrateLegacyDrafts).
var draftSeq = 0;
var draftFlushTimer = null;
var draftFlushKeys = new Set();   // ids whose storage entry must be (re)written or removed
var draftAdoptedFrom = new Map(); // id -> the dead writer's key, removed once ours is stored
var draftRangesDirty = false;     // RANGES_KEY joins the flush (the server seeds it on boot)
var draftsUnpersisted = false;    // the last flush could not be stored (quota) — warn before unload
var restoredConflictIds = [];     // adopted drafts that carry a conflict (90-sync registers them)
// Bumped on every write of state.marks / notes / tags (touchAnnotation, a
// replayed draft, hydration, a server value applied by 90-sync): the
// filter memo of the mark/tag chips keys on it (40-grid filterStateKey, U7).
var annotationGen = 0;
function bumpAnnotations() { annotationGen += 1; }
const DRAFT_FLUSH_MS = 250;
const DRAFT_PREFIX = "ft.draft." + (manifest.case_id || "case") + ".";
const ALIVE_PREFIX = "ft.alive:" + (manifest.case_id || "case") + ":";
// Liveness of the other windows on this case (§10.3): each refreshes its
// heartbeat every 5 s; pagehide (a real unload, not the back/forward cache)
// replaces it with ALIVE_CLOSED. Another writer's drafts are adopted only
// when its page is closed, or when its heartbeat is ALIVE_STALE_MS old AND
// it does not answer a same-origin BroadcastChannel ping (a throttled or
// frozen live tab keeps its drafts).
const ALIVE_CLOSED = "closed";
const ALIVE_RECENT_MS = 30000;
const ALIVE_STALE_MS = 5 * 60 * 1000;
const ALIVE_PING_MS = 1500;

function writerId() {
  return (typeof FTViewer !== "undefined" && FTViewer.writerToken) || "";
}
function aliveKey(writer) {
  return ALIVE_PREFIX + writer;
}
function draftKey(writer, id) {
  return DRAFT_PREFIX + writer + "." + id;
}
// Writer tokens never contain a dot; record ids may.
function parseDraftKey(key) {
  if (!key || !key.startsWith(DRAFT_PREFIX)) return null;
  const rest = key.slice(DRAFT_PREFIX.length);
  const dot = rest.indexOf(".");
  return dot > 0 && dot < rest.length - 1 ? { writer: rest.slice(0, dot), id: rest.slice(dot + 1) } : null;
}
// "mine" | "alive" | "closed" | "stale" — what this window may do with
// another writer's drafts. "stale" (no heartbeat for ALIVE_STALE_MS, or
// none at all) still needs an unanswered ping before adoption.
function writerVerdict(writer) {
  if (!writer || writer === "legacy") return "closed";
  if (writer === writerId()) return "mine";
  let raw = null;
  try { raw = localStorage.getItem(aliveKey(writer)); } catch (error) { raw = null; }
  if (raw === ALIVE_CLOSED) return "closed";
  const ts = Number(raw);
  if (raw != null && Number.isFinite(ts) && ts > 0 && Date.now() - ts < ALIVE_STALE_MS) return "alive";
  return "stale";
}
// Heart-beating right now (the "also open in another window" toast).
function writerRecentlyAlive(writer) {
  let ts = NaN;
  try { ts = Number(localStorage.getItem(aliveKey(writer))); } catch (error) { ts = NaN; }
  return Number.isFinite(ts) && ts > 0 && Date.now() - ts < ALIVE_RECENT_MS;
}
function nextDraftSeq() {
  draftSeq += 1;
  return draftSeq;
}
function draftRangeOf(id) {
  const r = state.ranges && state.ranges[id];
  return r && (Number.isFinite(r.in) || Number.isFinite(r.out))
    ? { in: Number.isFinite(r.in) ? r.in : null, out: Number.isFinite(r.out) ? r.out : null }
    : null;
}
// The annotation values a draft stores for `id` (post-mutation state).
function draftValuesOf(id) {
  return {
    mark: state.marks[id] || null,
    note: state.notes[id] || "",
    tags: state.tags[id] || [],
    examiner: state.examiners[id] ?? null,
    range: draftRangeOf(id)
  };
}

// Every stored draft of this case: [{ key, writer, id, draft }].
function storedDraftEntries() {
  const out = [];
  try {
    const keys = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.startsWith(DRAFT_PREFIX)) keys.push(key);
    }
    for (const key of keys) {
      const at = parseDraftKey(key);
      if (!at) continue;
      let draft = null;
      try { draft = JSON.parse(localStorage.getItem(key)); } catch (error) { draft = null; }
      if (draft && typeof draft === "object") out.push({ key, writer: at.writer, id: at.id, draft });
    }
  } catch (error) { /* storage blocked: nothing stored */ }
  return out;
}

// One-time move of the pre-§10.3 single-key drafts ({id: draft}) into
// per-id keys. Those drafts carry no base revision: they are migrated with
// base 0, so a value the server already holds comes back as a conflict
// (or a no-op when identical) instead of being overwritten. The old key
// goes only once every entry is stored under its new key.
function migrateLegacyDrafts() {
  let legacy = null;
  try {
    const raw = localStorage.getItem(ANNOTATIONS_KEY);
    if (raw == null) return 0;
    legacy = JSON.parse(raw);
  } catch (error) { legacy = null; }
  let moved = 0;
  let complete = true;
  for (const [id, draft] of Object.entries(legacy && typeof legacy === "object" ? legacy : {})) {
    if (!draft || typeof draft !== "object") continue;
    const writer = typeof draft.writer === "string" && /^[A-Za-z0-9_-]+$/.test(draft.writer) ? draft.writer : "legacy";
    const record = {
      mark: draft.mark || null, note: draft.note || "", tags: Array.isArray(draft.tags) ? draft.tags : [],
      examiner: draft.examiner ?? null, seq: Number.isFinite(draft.seq) ? draft.seq : 0,
      base: Number.isFinite(draft.base) ? draft.base : 0
    };
    if (Object.prototype.hasOwnProperty.call(draft, "range")) record.range = draft.range || null;
    try {
      if (localStorage.getItem(draftKey(writer, id)) == null) localStorage.setItem(draftKey(writer, id), JSON.stringify(record));
      moved += 1;
    } catch (error) {
      reportStorageError(error);
      complete = false;
      break;
    }
  }
  if (complete) { try { localStorage.removeItem(ANNOTATIONS_KEY); } catch (error) { /* retried next boot */ } }
  return moved;
}

function scheduleDraftFlush() {
  if (draftFlushTimer) return;
  const run = () => { draftFlushTimer = null; if (!ftSignal.aborted) flushDraftsNow(); };
  draftFlushTimer = setTimeout(() => {
    if (typeof requestIdleCallback === "function") requestIdleCallback(run, { timeout: DRAFT_FLUSH_MS * 2 });
    else run();
  }, DRAFT_FLUSH_MS);
}

// Writes (or removes) only this window's keys for the ids changed since the
// last flush. Removals go first (they free space); the first storage
// failure (quota) stops the pass and keeps the rest for the next flush —
// one toast per session, never one per key.
function flushDraftsNow(quiet) {
  clearTimeout(draftFlushTimer);
  draftFlushTimer = null;
  if (draftFlushKeys.size) {
    const me = writerId();
    const order = [...draftFlushKeys].sort((a, b) => (state.drafts[a] ? 1 : 0) - (state.drafts[b] ? 1 : 0));
    let blocked = false;
    for (const id of order) {
      const draft = state.drafts[id];
      try {
        if (draft) localStorage.setItem(draftKey(me, id), JSON.stringify(draft));
        else localStorage.removeItem(draftKey(me, id));
      } catch (error) {
        reportStorageError(error);
        blocked = true;
        break;
      }
      draftFlushKeys.delete(id);
      const foreign = draftAdoptedFrom.get(id);
      if (foreign) {
        draftAdoptedFrom.delete(id);
        try { localStorage.removeItem(foreign); } catch (error) { /* a duplicate resends idempotently */ }
      }
    }
    draftsUnpersisted = blocked;
  } else {
    draftsUnpersisted = false;
  }
  if (draftRangesDirty && storageSet(RANGES_KEY, state.ranges)) draftRangesDirty = false;
  if (typeof noteOriginDrafts === "function") noteOriginDrafts();
  if (!quiet && typeof renderSaveState === "function") renderSaveState();
}

// Records a mutation of `id` (call AFTER the mutation: the draft snapshots
// the resulting state). The base revision stays the one the first unsaved
// edit was made on; opts.base overrides it (a conflict resolution moves it
// to the server's revision). An id with an open conflict stays in conflict
// (its "mine" follows the edit) until the examiner resolves it.
function touchAnnotation(id, opts) {
  if (typeof bumpAnnotations === "function") bumpAnnotations();
  state.examiners[id] = (state.examiner || "").trim() || state.examiners[id] || null;
  const prev = state.drafts[id];
  const known = state.revisions && Number.isFinite(state.revisions[id]) ? state.revisions[id] : 0;
  const base = opts && Number.isFinite(opts.base) ? opts.base : (prev && Number.isFinite(prev.base) ? prev.base : known);
  const draft = { ...draftValuesOf(id), seq: nextDraftSeq(), base };
  const conflicted = typeof conflictIds !== "undefined" && conflictIds.has(id);
  if (conflicted && prev && prev.conflict) draft.conflict = prev.conflict;
  state.drafts[id] = draft;
  if (conflicted) {
    if (typeof conflictEdited === "function") conflictEdited(id);
  } else {
    state.dirtyIds.add(id);
  }
  draftFlushKeys.add(id);
  draftRangesDirty = true;
  scheduleDraftFlush();
  markReviewDirty();
}

// Forgets a draft this window owns (the server confirmed it, or the
// examiner took the server's value). Storage follows at the next flush.
function dropDraft(id) {
  delete state.drafts[id];
  state.dirtyIds.delete(id);
  draftFlushKeys.add(id);
  scheduleDraftFlush();
}

// Replays one stored draft's values into the in-memory annotation maps.
function applyDraftValues(id, draft) {
  if (typeof bumpAnnotations === "function") bumpAnnotations();
  if (draft.mark) state.marks[id] = draft.mark;
  else delete state.marks[id];
  state.notes[id] = draft.note || "";
  state.tags[id] = Array.isArray(draft.tags) ? draft.tags : [];
  state.examiners[id] = draft.examiner ?? null;
  if (Object.prototype.hasOwnProperty.call(draft, "range")) {
    if (draft.range) state.ranges[id] = { ...draft.range };
    else delete state.ranges[id];
    draftRangesDirty = true;
  }
}

// Claims one stored draft for this window. Another (dead) writer's draft is
// re-keyed under this window's writer — ours is written first, theirs
// removed after, so a crash in between leaves a duplicate, never a loss —
// and keeps ITS base revision.
function adoptDraft(id, draft, writer, key) {
  applyDraftValues(id, draft);
  if (Number.isFinite(draft.seq)) draftSeq = Math.max(draftSeq, draft.seq);
  const mine = writer === writerId();
  const adopted = {
    mark: draft.mark || null, note: draft.note || "", tags: Array.isArray(draft.tags) ? draft.tags : [],
    examiner: draft.examiner ?? null, range: Object.prototype.hasOwnProperty.call(draft, "range") ? (draft.range || null) : draftRangeOf(id),
    seq: mine && Number.isFinite(draft.seq) ? draft.seq : nextDraftSeq(),
    base: Number.isFinite(draft.base) ? draft.base : 0
  };
  if (draft.conflict && typeof draft.conflict === "object") adopted.conflict = draft.conflict;
  state.drafts[id] = adopted;
  if (!mine && key) {
    let stored = false;
    try { localStorage.setItem(draftKey(writerId(), id), JSON.stringify(adopted)); stored = true; } catch (error) { reportStorageError(error); }
    if (stored) { try { localStorage.removeItem(key); } catch (error) { /* duplicate, resent idempotently */ } }
    else { draftAdoptedFrom.set(id, key); draftFlushKeys.add(id); }
  }
  if (adopted.conflict) restoredConflictIds.push(id);
  else state.dirtyIds.add(id);
}

// Case-DB rows (records-meta, the exported bundle) key marks by `record_id`;
// marks files and older bundles use `id`. Accept both, or server-saved
// marks/notes/examiners/ranges never appear in a fresh browser.
function annotationMarkId(mark) {
  return mark ? (mark.id || mark.record_id || "") : "";
}

// Writers whose stored drafts were left alone because the window looked
// alive (or stale but not yet pinged); the heartbeat tick re-checks them.
var skippedDraftWriters = new Set();

function hydrateAnnotations(annotations) {
  if (typeof bumpAnnotations === "function") bumpAnnotations();
  state.marks = {}; state.notes = {}; state.tags = {}; state.examiners = {};
  state.revisions = {};
  for (const [id, rev] of Object.entries(annotations.revisions || {})) {
    if (Number.isFinite(Number(rev))) state.revisions[id] = Number(rev);
  }
  for (const mark of annotations.marks || []) {
    const id = annotationMarkId(mark);
    if (!id) continue;
    if (mark.status !== "noted") state.marks[id] = { status: mark.status, marked_unix: mark.marked_unix };
    state.notes[id] = mark.note || "";
    state.examiners[id] = mark.examiner || null;
  }
  for (const entry of annotations.tags || []) state.tags[entry.id] = entry.tags;
  state.dirtyIds = new Set();
  state.drafts = {};
  skippedDraftWriters = new Set();
  restoredConflictIds = [];
  migrateLegacyDrafts();
  // This window's own drafts first (a hosted destroy()+init() keeps the
  // writer token): a closed window's draft for the same id stays stored.
  const entries = storedDraftEntries();
  const verdicts = new Map();
  const verdictOf = writer => {
    if (!verdicts.has(writer)) verdicts.set(writer, writerVerdict(writer));
    return verdicts.get(writer);
  };
  entries.sort((a, b) => (verdictOf(a.writer) === "mine" ? 0 : 1) - (verdictOf(b.writer) === "mine" ? 0 : 1));
  for (const entry of entries) {
    const verdict = verdictOf(entry.writer);
    if (verdict === "mine") adoptDraft(entry.id, entry.draft, entry.writer, entry.key);
    else if (verdict === "closed") { if (!state.drafts[entry.id]) adoptDraft(entry.id, entry.draft, entry.writer, entry.key); }
    else skippedDraftWriters.add(entry.writer);
  }
  if (draftFlushKeys.size) scheduleDraftFlush();
}

// --- Dead-window detection (§10.3) -----------------------------------------
// Same-origin windows on this case answer {type:"ping"} for their writer
// token on BroadcastChannel ft-alive:<case>. A heartbeat can go stale in a
// live tab (timer throttling, frozen tab, storage full); the ping tells.
var aliveChannel = null;
var alivePings = new Map(); // nonce -> Set of writers that answered
function openAliveChannel() {
  if (aliveChannel || typeof BroadcastChannel !== "function" || ftSignal.aborted) return aliveChannel;
  try { aliveChannel = new BroadcastChannel("ft-alive:" + (manifest.case_id || "case")); } catch (error) { aliveChannel = null; return null; }
  const channel = aliveChannel;
  channel.onmessage = event => {
    const msg = event.data || {};
    if (msg.type === "ping" && Array.isArray(msg.writers) && msg.writers.includes(writerId())) {
      try { channel.postMessage({ type: "pong", nonce: msg.nonce, writer: writerId() }); } catch (error) { /* closed */ }
    } else if (msg.type === "pong" && alivePings.has(msg.nonce)) {
      alivePings.get(msg.nonce).add(msg.writer);
    }
  };
  ftSignal.addEventListener("abort", () => { try { channel.close(); } catch (error) { /* closed */ } if (aliveChannel === channel) aliveChannel = null; });
  return channel;
}
// Resolves the set of `writers` that answered within ALIVE_PING_MS.
function pingWriters(writers) {
  const channel = openAliveChannel();
  if (!channel || !writers.length) return Promise.resolve(new Set());
  const nonce = Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
  const answered = new Set();
  alivePings.set(nonce, answered);
  try { channel.postMessage({ type: "ping", nonce, writers, from: writerId() }); } catch (error) { /* closed */ }
  return new Promise(resolve => setTimeout(() => { alivePings.delete(nonce); resolve(answered); }, ALIVE_PING_MS));
}

// Heartbeat tick: drafts of a writer whose page closed, or whose heartbeat
// is ALIVE_STALE_MS old and who does not answer the ping, are adopted
// here — with their base revision, so a value saved since by someone else
// comes back as a conflict, never an overwrite.
var orphanCheck = null;
function adoptOrphanDrafts() {
  if (orphanCheck) return orphanCheck;
  if (!skippedDraftWriters.size) return Promise.resolve(0);
  orphanCheck = (async () => {
    const closed = [];
    const stale = [];
    for (const writer of skippedDraftWriters) {
      const verdict = writerVerdict(writer);
      if (verdict === "closed") closed.push(writer);
      else if (verdict === "stale") stale.push(writer);
    }
    let dead = closed;
    if (stale.length) {
      const alive = await pingWriters(stale);
      dead = closed.concat(stale.filter(writer => !alive.has(writer)));
    }
    if (ftSignal.aborted || !dead.length) return 0;
    return adoptDeadWriters(dead);
  })().finally(() => { orphanCheck = null; });
  return orphanCheck;
}
function adoptDeadWriters(dead) {
  const gone = new Set(dead);
  gone.forEach(writer => skippedDraftWriters.delete(writer));
  let adopted = 0;
  const adoptedIds = [];
  for (const entry of storedDraftEntries()) {
    if (!gone.has(entry.writer) || state.drafts[entry.id]) continue;
    adoptDraft(entry.id, entry.draft, entry.writer, entry.key);
    adoptedIds.push(entry.id);
    adopted += 1;
  }
  // An adopted draft replaced those records' values after the undo journal
  // (or a journal restored across a reload, U4) was checked against them:
  // their entries would now undo/redo over the dead window's edit — drop them.
  if (adoptedIds.length && typeof dropJournalIds === "function") dropJournalIds(adoptedIds);
  gone.forEach(writer => { try { localStorage.removeItem(aliveKey(writer)); } catch (error) { /* storage blocked */ } });
  if (adopted) {
    if (typeof registerRestoredConflicts === "function") registerRestoredConflicts();
    scheduleDraftFlush();
    markReviewDirty();
    if (typeof requestRender === "function") requestRender();
  }
  return adopted;
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
