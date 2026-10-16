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

// A14: matches are found in the RAW text and every piece is escaped on its
// own — matching inside the escaped string split entities ("amp" would
// match the "&amp;" of "&" and emit "&<mark>amp</mark>;").
function highlightEscape(value, query) {
  const text = String(value ?? "");
  const needle = String(query ?? "");
  if (!needle) return escapeHtml(text);
  const hay = text.toLowerCase();
  const low = needle.toLowerCase();
  // toLowerCase may change the length (e.g. "İ"): then no highlight, never a wrong one.
  if (hay.length !== text.length || low.length !== needle.length) return escapeHtml(text);
  let out = "";
  let at = 0;
  for (let hit = hay.indexOf(low); hit >= 0; hit = hay.indexOf(low, hit + low.length)) {
    out += escapeHtml(text.slice(at, hit)) + "<mark>" + escapeHtml(text.slice(hit, hit + low.length)) + "</mark>";
    at = hit + low.length;
  }
  return out + escapeHtml(text.slice(at));
}
// A6: an Enter (or any key) that only commits an IME composition (Korean,
// Japanese, Chinese input) is not a command.
function imeComposing(event) {
  return !!event && (event.isComposing === true || event.keyCode === 229);
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

// R9 U1 (track I P3 ↔ U): every entry of the audit-lane logs the bundle /
// records-meta carry (validationLog, carveLog, filesystemLog, joinLog,
// anomalyLog, …) has `trust`: "auth" (authenticated here), "sc" (no audit
// keys on this PC: self-consistency only), "unauth" (keys held, line not
// authenticated) or "pc:<key id>" (signed by another PC's key). Absent =
// an older server: shown as before, never called authenticated. Anything
// else is read as "unauth" (fail closed).
//   -> "" (legacy) | "auth" | "sc" | "unauth" | "pc:<id>"
function logTrustOf(item) {
  if (!item || typeof item !== "object" || item.trust == null) return "";
  const v = String(item.trust).trim();
  if (v === "auth" || v === "sc") return v;
  if (/^pc:\S/.test(v)) return v;
  return "unauth";
}
// May an entry of this trust make a record confirmed / derived / verified?
function logTrustUsable(trust) {
  return trust === "" || trust === "auth" || trust === "sc";
}
function logTrustPcKey(trust) {
  return typeof trust === "string" && trust.startsWith("pc:") ? trust.slice(3) : "";
}
// The pseudo status of a record whose result comes only from a line this
// PC could not authenticate: never confirmed / derived / verified.
function untrustedStatusOf(trust) {
  return logTrustPcKey(trust) ? "auth-other-pc" : "unauthenticated";
}
// R11 U1: the server's P7 mark on an entry a newer unusable entry of the
// same record supersedes (`trust_superseded_by`: that entry's line number).
function logTrustSuperseded(item) {
  return !!item && typeof item === "object" && Number(item.trust_superseded_by) > 0;
}
// Per key: the latest usable entry, the latest one that is not, and the
// latest of all (`last`).
// R10 U1 (P7, recency): when the latest entry of a key is not usable and an
// earlier one is, the key is NOT rolled back to that earlier line — the
// unusable entry is current and carries the non-enumerable flag
// `ftNewerUnauth` ("더 최근 인증 안 된 항목 있음"; a copy, so the log list
// itself is untouched and exports never see the flag).
// R13 (track I2, P16): a usable `lane-repair` marker after a key's latest
// entry — the repair cut a tail that may have held a newer entry — makes
// that entry "최신 여부 알 수 없음" (a copy flagged `ftRepairedAfter`, never
// confirmed). Markers: `chain.repairs[].line` (records-meta `logChains`)
// and usable `event: "lane-repair"` entries of `source` (the whole lane as
// sent, line = position + 1; defaults to `list` — pass it when `list` is a
// filtered copy).
// R14 U4: each repair's line and time (`logged_unix`, 0 when unknown).
function logRepairs(chain, source) {
  const out = [];
  if (chain && typeof chain === "object" && Array.isArray(chain.repairs)) {
    for (const r of chain.repairs) if (r && Number(r.line) > 0) out.push({ line: Number(r.line), unix: Number(r.logged_unix) > 0 ? Number(r.logged_unix) : 0 });
  }
  (Array.isArray(source) ? source : []).forEach((item, i) => {
    if (item && typeof item === "object" && item.event === "lane-repair" && logTrustUsable(logTrustOf(item))) out.push({ line: i + 1, unix: Number(item.logged_unix) > 0 ? Number(item.logged_unix) : 0 });
  });
  return out;
}
// R15 U2: a usable entry's lane (the whole lane array) and 1-based line,
// kept beside it (never on the object — exports never see it).
function logPosOf(item, pos) {
  const map = logPosOf.map || (logPosOf.map = new WeakMap());
  if (!item || typeof item !== "object") return null;
  if (pos) { map.set(item, pos); return pos; }
  return map.get(item) || null;
}
function logSlots(list, keyOf, chain, source) {
  const slots = new Map();
  const all = Array.isArray(source) ? source : Array.isArray(list) ? list : [];
  const repairs = logRepairs(chain, all);
  const lastRepair = Math.max(0, ...repairs.map(r => r.line));
  // The newest repair's time (R14 U4: a validation logged after it
  // re-verifies an entry written before it); 0 = unknown.
  const lastRepairUnix = Math.max(0, ...repairs.filter(r => r.line === lastRepair).map(r => r.unix));
  // R15 U2: every usable entry's line in the whole lane (logPosOf).
  const items = Array.isArray(list) ? list : [];
  const lineOf = items === all ? null : new Map(all.map((item, i) => [item, i + 1]));
  let idx = 0;
  for (const item of items) {
    idx += 1;
    if (!item || typeof item !== "object") continue;
    const key = keyOf(item);
    if (!key) continue;
    let slot = slots.get(key);
    if (!slot) { slot = { usable: null, untrusted: null, last: null, superseded: false }; slots.set(key, slot); }
    if (logTrustUsable(logTrustOf(item))) {
      let entry = item;
      const line = lineOf ? lineOf.get(item) || 0 : idx;
      if (lastRepair && !(line > lastRepair)) {
        entry = Object.assign({}, item);
        Object.defineProperty(entry, "ftRepairedAfter", { value: true, enumerable: false });
        Object.defineProperty(entry, "ftRepairedUnix", { value: lastRepairUnix, enumerable: false });
        Object.defineProperty(entry, "ftRepairedLine", { value: lastRepair, enumerable: false });
      }
      if (line > 0) logPosOf(entry, { lane: all, line });
      slot.usable = entry;
      slot.last = entry;
      continue;
    }
    let entry = item;
    // R11 U1: the server already marks an older entry a newer unusable one
    // supersedes (`trust_superseded_by`, 1-based line of the newer entry;
    // that older entry then reads `unauth`) — a later unusable entry after
    // it, or the marked entry itself, is "더 최근 인증 안 된 항목 있음".
    if (slot.usable || slot.superseded || logTrustSuperseded(item)) {
      entry = Object.assign({}, item);
      Object.defineProperty(entry, "ftNewerUnauth", { value: true, enumerable: false });
    }
    if (logTrustSuperseded(item)) slot.superseded = true;
    slot.untrusted = entry;
    slot.last = entry;
  }
  return slots;
}
// The current entry per key (first-seen key order): the latest one. R10 U1
// (P7): an unusable latest entry is current even when an earlier one was
// usable — the record is then not confirmed (its own status says so).
function currentLogEntries(list, keyOf, chain, source) {
  return [...logSlots(list, keyOf, chain, source).values()].map(slot => slot.last);
}
// The server's reason for an entry this PC could not use (`trust_note`),
// "" otherwise. R10 U5: in a bundle the reason was written on the PC that
// generated it — "이 PC" there is not the PC reading it now.
// R11 U2: `any` also returns the note of a usable entry (a legacy
// authenticated entry: `auth` + "레거시" note) — tooltips only.
function logTrustNote(item, any) {
  if (!item || typeof item !== "object" || (!any && logTrustUsable(logTrustOf(item)))) return "";
  const note = item.trust_note == null ? "" : String(item.trust_note).trim();
  return viewerTrustFromBundle() ? note.replace(/이 PC/g, "생성 PC").replace(/\bthis PC\b/gi, "the generating PC") : note;
}
// The status a record shows and the trust it rests on.
//   own        the record's own log entry (carve / recovery / join; null
//              for an indexed video)
//   valid      the usable validation entry of its path (or null) — only
//              when it is the path's latest validation (P7)
//   unvalid    the path's latest validation when this PC could not
//              authenticate it
//   fallback   the status without any validation
// -> { status, trust[, newer: true][, note] }  (`newer`: a newer entry this
//    PC could not use superseded a usable one, R10 U1; `note`: that
//    entry's trust_note, R10 U3)
function recordStatusOf(own, valid, unvalid, fallback, recheck) {
  const ownTrust = own ? logTrustOf(own) : "";
  // R14 U4: an own entry before its lane's last repair is confirmed again
  // by a usable latest validation of its file logged after that repair
  // (`recheck`: the validation of a record that otherwise shows none).
  // R15 U1: only a confirmed state of the same bytes (`target_sha256` ===
  // the join's `output_sha256` / the carve's or recovery's `sha256`) confirms
  // again; another validation after the repair shows its own status.
  // R15 U2: in the repaired lane itself the line decides (a clock may be
  // off); across lanes there is no common line — the time, strictly after.
  const after = v => {
    if (!v || v.ftRepairedAfter || !logTrustUsable(logTrustOf(v))) return false;
    const vp = logPosOf(v), op = logPosOf(own);
    if (vp && op && vp.lane === op.lane && Number(own.ftRepairedLine) > 0) return vp.line > Number(own.ftRepairedLine);
    return Number(own.ftRepairedUnix) > 0 && Number(v.validated_unix) > Number(own.ftRepairedUnix);
  };
  const sameBytes = v => {
    const mine = String(own.output_sha256 || own.sha256 || "").trim().toLowerCase();
    return !!mine && String(v.target_sha256 || "").trim().toLowerCase() === mine;
  };
  const confirmedState = s => s === "ffprobe-confirmed" || s === "ffprobe-video-stream-confirmed";
  const recent = !!own && !!own.ftRepairedAfter && logTrustUsable(ownTrust) && after(valid || recheck) ? (valid || recheck) : null;
  const reverified = !!recent && confirmedState(recent.validation_status) && sameBytes(recent);
  if (recent && !reverified && recent.validation_status && !confirmedState(recent.validation_status)) {
    const note = [...new Set([own, recent].map(i => logTrustNote(i, true)).filter(Boolean))].join("\n");
    const out = { status: String(recent.validation_status), trust: logTrustOf(recent) || ownTrust };
    if (note) out.note = note;
    return out;
  }
  const untrusted = (item, trust) => {
    // R13: usable, but a lane repair follows it — the latest is unknown.
    if (item.ftRepairedAfter && logTrustUsable(trust)) return { status: "unknown-after-repair", trust, repairedAfter: true };
    const out = { status: untrustedStatusOf(trust), trust };
    if (item.ftNewerUnauth) out.newer = true;
    const note = logTrustNote(item);
    if (note) out.note = note;
    return out;
  };
  if (own && (!logTrustUsable(ownTrust) || (own.ftRepairedAfter && !reverified))) return untrusted(own, ownTrust);
  // R11 U2: a usable entry's note (legacy, unbound) stays in the tooltip.
  const usable = (out, ...items) => {
    const note = [...new Set(items.map(i => logTrustNote(i, true)).filter(Boolean))].join("\n");
    if (note) out.note = note;
    return out;
  };
  if (valid && valid.validation_status) return usable({ status: valid.validation_status, trust: logTrustOf(valid) || ownTrust }, own, valid);
  if (unvalid) return untrusted(unvalid, logTrustOf(unvalid));
  return usable({ status: fallback, trust: ownTrust }, own);
}
// Tooltip of a record's status badge: the raw status plus what its trust means.
function statusTitle(record) {
  const status = String(record && record.status || "");
  const trust = String(record && record.statusTrust || "");
  // R10 U3: the server's reason; R10 U1: a newer entry this PC could not
  // use superseded a usable one.
  const note = record && record.statusNote ? "\n" + record.statusNote : "";
  const newer = !!(record && record.statusNewer);
  const head = newer ? t("status.tip.newer") + " · " : "";
  if (status === "auth-other-pc") return head + tf("trust.otherPc", { key: logTrustPcKey(trust) || "?" }) + note;
  if (status === "unauthenticated") return head + t(newer ? "status.unauth" : "status.tip.unauth") + note;
  if (status === "unknown-after-repair") return head + t("status.tip.repairedAfter") + note;
  if (trust === "sc") return status + " · " + t("trust.sc") + note;
  return status + note;
}

// R10 U3: what records-meta's `logChains[lane]` says when it is not plainly
// fine: "" for an absent lane or a keyed one with every line authenticated;
// otherwise a short state ("체인 검증 실패", "인증 안 된 줄 N개", …). An
// unknown status reads "인증 불가" (fail closed, never a failure verdict).
function logChainSummary(chain) {
  if (!chain || typeof chain !== "object") return "";
  const st = String(chain.status || "");
  const n = Number(chain.unauthenticated) > 0 ? Number(chain.unauthenticated) : 0;
  if (st === "absent" || (st === "keyed" && !n)) return "";
  // R12 U4 (track I3): `repair-lane` cut a torn tail off — say so (detail =
  // what was cut, the tooltip) with the chain's own state (`base_status`,
  // keyed = nothing more), plus any lines still not authenticated.
  // R13 U4 (track I5): the cut bytes' archive (`.removed`) checked by
  // verify-audit — 일치 / 없음 / 다름, next to the repair.
  const arc = logChainArchiveText(chain);
  if (st === "repaired") {
    const base = String(chain.base_status || "");
    const baseText = base === "structural" ? t("chain.structural") : base && base !== "keyed" && base !== "repaired" ? logChainSummary({ status: base }) : "";
    return [t("chain.repaired"), arc, baseText, n ? tf("chain.unauthN", { n }) : ""].filter(Boolean).join(" · ");
  }
  // A worse state wins over the repair, which is still mentioned.
  const fix = chain.repaired === true ? " · " + t("chain.repairedShort") + (arc ? " · " + arc : "") : "";
  if (st === "failed") return t("chain.failed") + fix;
  if (st === "unreadable") return t("chain.unreadable") + fix;
  if (st === "structural") return t("chain.structural") + fix;
  if (n) return tf("chain.unauthN", { n }) + fix;
  return t("chain.notVerified") + fix;
}
// `removed_archive` on the chain (or, failing that, the worst one among its
// `repairs[]`): "match" | "missing" | "differs"; another value reads 확인 안 됨.
function logChainArchiveText(chain) {
  let v = chain && chain.removed_archive != null ? String(chain.removed_archive) : "";
  if (!v && chain && Array.isArray(chain.repairs)) {
    const rank = (x) => ({ differs: 4, missing: 2, match: 1 })[x] || (x ? 3 : 0);
    for (const r of chain.repairs) {
      const x = r && r.removed_archive != null ? String(r.removed_archive) : "";
      if (rank(x) > rank(v)) v = x;
    }
  }
  if (!v) return "";
  if (v === "match") return t("chain.archive.match");
  if (v === "missing") return t("chain.archive.missing");
  if (v === "differs") return t("chain.archive.differs");
  return t("chain.archive.unknown");
}
// The lanes a record's log entries come from (head of its log list).
function logChainLanesOf(record) {
  const lanes = ["validationLog"];
  if (record && record.kind === "carved") lanes.push("carveLog");
  if (record && record.kind === "filesystem") lanes.push("filesystemLog");
  if (record && record.kind === "joined") lanes.push("joinLog");
  if (record && Array.isArray(record.anomalies) && record.anomalies.length) lanes.push("anomalyLog");
  return lanes;
}
// One warn row per lane whose chain is not plainly fine; the server's
// `detail` is the tooltip. [] without logChains (older server, bundle).
function logChainItems(chains, lanes) {
  if (!chains || typeof chains !== "object") return [];
  return lanes.map(lane => {
    const chain = chains[lane];
    const text = logChainSummary(chain);
    if (!text) return "";
    let detail = chain && chain.detail != null ? String(chain.detail) : "";
    if (viewerTrustFromBundle()) detail = detail.replace(/이 PC/g, "생성 PC");
    return `<div class="validation-item warn log-chain" data-lane="${escapeHtml(lane)}"${detail ? ` title="${escapeHtml(detail)}"` : ""}>` +
      `<strong>${escapeHtml(t("chain.lane." + lane))}</strong> ${escapeHtml(text)}</div>`;
  }).filter(Boolean);
}

function statusLabel(status) {
  if (status === "unauthenticated") return t("status.unauth");
  if (status === "auth-other-pc") return t("status.otherPc");
  if (status === "unknown-after-repair") return t("status.repairedAfter");
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

// R9 U1: a validation this PC could not authenticate never confirms a
// record: `validationsByPath` holds the usable entry per path,
// `untrustedValidationsByPath` the one that is not.
const validationSlots = logSlots(validationLog, item => normalizePath(item.target_path), logChains && logChains.validationLog);
// R10 U1 (P7): only the path's LATEST validation counts — a usable one
// confirms; an unusable one (even after a usable one) makes the record
// unauthenticated. An older usable line is never rolled back to.
function splitValidationSlots(slots) {
  const usable = new Map(), untrusted = new Map();
  for (const [k, s] of slots) {
    if (s.last && s.last === s.usable && !s.usable.ftRepairedAfter) usable.set(k, s.usable);
    else if (s.last) untrusted.set(k, s.last);
  }
  return { usable, untrusted };
}
const validationSplit = splitValidationSlots(validationSlots);
const validationsByPath = validationSplit.usable;
const untrustedValidationsByPath = validationSplit.untrusted;
const recoveredFilesystemLog = currentLogEntries(
  filesystemLog.filter(item => item && item.event === "recover-inode" && item.output_path),
  item => `${item.partition_offset ?? 0}:${item.inode || normalizePath(item.output_path)}`,
  logChains && logChains.filesystemLog, filesystemLog);
// Case-level warnings recorded on inspection events (e.g. a truncated
// fls listing means the deleted-file list is incomplete) — the reviewer
// must see them, not just the CLI.
const inspectionWarnings = filesystemLog
  .filter(item => item.event === "inspect-image-filesystem" && Array.isArray(item.warnings) && item.warnings.length)
  .flatMap(item => item.warnings);

// Lossless joins (artifacts/joined/join-log.jsonl, docs/PLAN_PHASE2_JOIN_AUDIO
// §1.5/§1.8): one derived record per output file; a later entry for the same
// output path supersedes an earlier one.
// R10 U1 (P7): a later entry this PC could not use makes the record
// unauthenticated ("더 최근 인증 안 된 항목 있음"), never the older usable one.
const joinLog = currentLogEntries(Array.isArray(DATA.joinLog) ? DATA.joinLog : [], item => item.output_path ? normalizePath(item.output_path) : "", logChains && logChains.joinLog);

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
  // R8 (track I P1/P2): `sc: true` = measured on a PC without audit keys —
  // the level is shown, labelled "자기 일관성만(인증 아님)"; `pc: "<key id>"`
  // on an unverified line = authenticated by another PC's key — 미확인 here,
  // "키 <id> 보유 PC에서 확인".
  const trust = level && typeof level === "object" && level.sc === true ? "sc" : "";
  const pcKey = level && typeof level === "object" && level.pc != null && String(level.pc) ? String(level.pc) : "";
  if (r === "sound" || r === "silent") return { state: r, level, reason: "", trust, pcKey: "" };
  // R4 D1/D2: "unverified" (the measurement log failed its chain check) and
  // "replayed" (a checkpoint result recorded without re-measuring) are never
  // sound/silent — the track reads 미확인 with the reason.
  // R14 (track I5): the server's reason for an unverified / replayed state
  // (e.g. AUDIO_REPAIRED_AFTER_NOTE "측정 레인이 이 측정 뒤에서 수리됨 …")
  // when the entry carries one (`note`, else `error`) — tooltip / detail.
  const why = level && typeof level === "object" && (r === "unverified" || r === "replayed")
    ? String(level.note != null ? level.note : level.error != null ? level.error : "").trim() : "";
  if (hasTrack) return { state: "track", level: null, reason: ["error", "stale", "unverified", "replayed"].includes(r) ? r : audioLevelsError ? "unread" : "", trust: "", pcKey: r === "unverified" ? pcKey : "", note: why };
  if (r === "unverified" || r === "replayed") return { state: "unknown", level: null, reason: r, trust: "", pcKey: r === "unverified" ? pcKey : "", note: why };
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
// R8: the trust note of a sound shown (sc) / not shown (another PC's key).
// R9 U3: a line signed by another PC's key is "이 PC에서 인증 불가(변조
// 판정 아님) — 키 <id> 보유 PC에서 확인", never "검증 실패", and is said for
// a record without a probed track (state unknown) too.
function audioTrustNote(record) {
  if (record.audioTrust === "sc" && (record.audioState === "sound" || record.audioState === "silent")) return " · " + t("audio.selfConsistent");
  if (record.audioPcKey) return " · " + tf("trust.otherPc", { key: record.audioPcKey });
  return "";
}
function audioTipFor(record) {
  const text = audioTipBase(record) || (record.audioPcKey ? t("audio.unknown") : "");
  return text ? text + audioTrustNote(record) + audioReasonNote(record) : text;
}
// R14: the server's reason (tooltip: its own line; detail: " · ").
function audioReasonNote(record, sep) {
  return record && record.audioNote ? (sep || "\n") + record.audioNote : "";
}
function audioTipBase(record) {
  const level = record.audioLevel || {};
  switch (record.audioState) {
    case "sound": return tf("audio.tip.sound", { max: fmtDb(level.max, 0) });
    case "silent": return tf("audio.tip.silent", { max: fmtDb(level.max, 0) });
    case "none": return t("audio.tip.none");
    case "track": if (record.audioPcKey) return t("audio.tip.trackOtherPc");
      return t(record.audioReason === "error" ? "audio.tip.trackError" : record.audioReason === "stale" ? "audio.tip.trackStale" : record.audioReason === "unread" ? "audio.tip.trackUnread" : record.audioReason === "unverified" ? "audio.tip.trackUnverified" : record.audioReason === "replayed" ? "audio.tip.trackReplayed" : "audio.tip.track");
    default: return "";
  }
}
function audioDetailFor(record) {
  return audioDetailBase(record) + audioTrustNote(record) + audioReasonNote(record, " · ");
}
function audioDetailBase(record) {
  const level = record.audioLevel || {};
  const codec = record.audioCodec ? ` (${record.audioCodec})` : "";
  switch (record.audioState) {
    case "sound": return tf("audio.detail.sound", { max: fmtDb(level.max, 1), mean: fmtDb(level.mean, 1), codec });
    case "silent": return tf("audio.detail.silent", { max: fmtDb(level.max, 1) });
    case "none": return t("audio.detail.none");
    case "track": if (record.audioPcKey) return tf("audio.detail.trackOtherPc", { codec });
      return tf(record.audioReason === "error" ? "audio.detail.trackError" : record.audioReason === "stale" ? "audio.detail.trackStale" : record.audioReason === "unread" ? "audio.detail.trackUnread" : record.audioReason === "unverified" ? "audio.detail.trackUnverified" : record.audioReason === "replayed" ? "audio.detail.trackReplayed" : "audio.detail.track", { codec });
    default: return t("audio.unknown");
  }
}

// R5 U16: the server's per-record proxy (`proxy_path` on the row; null or
// "" = it has none). undefined when the row does not carry the field.
function serverProxyOf(row) {
  if (!row || !Object.prototype.hasOwnProperty.call(row, "proxy_path")) return undefined;
  return typeof row.proxy_path === "string" ? row.proxy_path : "";
}
const records = [
  ...videos.map(video => {
    const validation = validationsByPath.get(normalizePath(video.source_path));
    const st = recordStatusOf(null, validation, untrustedValidationsByPath.get(normalizePath(video.source_path)), video.ffprobe_ok ? "ffprobe-confirmed" : "candidate-unvalidated");
    return {
      id: video.id,
      kind: "video",
      name: video.relative_path || video.id,
      path: video.source_path,
      fileUrl: video.file_url,
      parser: video.source_profile?.parser || "-",
      vendor: video.source_profile?.vendor || "-",
      status: st.status,
      statusTrust: st.trust,
      statusNewer: !!st.newer,
      statusNote: st.note || "",
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
      serverProxy: serverProxyOf(video),
      validation
    };
  }),
  ...carveLog.map(item => {
    const validation = validationsByPath.get(normalizePath(item.output_path));
    const st = recordStatusOf(item, validation, untrustedValidationsByPath.get(normalizePath(item.output_path)), item.validation_status || "candidate-unvalidated");
    return {
      id: item.id || item.output_path,
      kind: "carved",
      name: item.output_path ? item.output_path.split(/[\\\/]/).pop() : item.id,
      path: absolutizeArtifactPath(item.output_path),
      fileUrl: fileUrl(absolutizeArtifactPath(item.output_path)),
      parser: item.signature || "carve",
      vendor: "Recovered candidate",
      status: st.status,
      statusTrust: st.trust,
      statusNewer: !!st.newer,
      statusNote: st.note || "",
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
      serverProxy: serverProxyOf(item),
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
    const st = recordStatusOf(item, validation, untrustedValidationsByPath.get(normalizePath(item.output_path)), item.validation_status || "candidate-unvalidated");
    return {
      id: `inode:${item.partition_offset ?? 0}:${item.inode || item.output_path}`,
      kind: "filesystem",
      name: item.output_path ? item.output_path.split(/[\\\/]/).pop() : item.inode,
      path: absolutizeArtifactPath(item.output_path),
      fileUrl: fileUrl(absolutizeArtifactPath(item.output_path)),
      parser: "tsk/icat",
      vendor: "Filesystem recovery",
      status: st.status,
      statusTrust: st.trust,
      statusNewer: !!st.newer,
      statusNote: st.note || "",
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
      serverProxy: serverProxyOf(item),
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
    // R9 U1: "derived" only from a usable join-log line.
    const st = recordStatusOf(item, null, null, "derived", validationsByPath.get(normalizePath(item.output_path)));
    return {
      id: serverId || `join:${name}`,
      kind: "joined",
      name,
      path,
      fileUrl: fileUrl(path),
      parser: "ffmpeg concat (stream copy)",
      vendor: t("joined.vendor"),
      status: st.status,
      statusTrust: st.trust,
      statusNewer: !!st.newer,
      statusNote: st.note || "",
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
  record.audioTrust = audio.trust || "";
  record.audioPcKey = audio.pcKey || "";
  record.audioNote = audio.note || "";
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
// R5 U16 (P9/S15): a name match cannot tell two records apart whose
// sanitized keys collide (a legacy carve id and an indexed id, an id that
// is a prefix of another). When the server says per record which proxy is
// whose — records-meta `proxy_paths` {id: path} (also `proxyPaths`), proxy
// entries given as objects {record_id|id, proxy_path|path}, or a
// `proxy_path` on the record rows themselves — that answer is the ONLY one
// used (an id it does not name has no proxy). A server without it (older
// builds) keeps the file-name match below.
function resolveProxyPaths(list, data) {
  const files = Array.isArray(data.proxies) ? data.proxies : [];
  let byId = null;
  const own = (map, id, path) => {
    if (typeof id !== "string" || !id) return;
    if (!map.has(id)) map.set(id, typeof path === "string" ? path : "");
  };
  const given = data.proxyPaths || data.proxy_paths;
  const mapObj = given && typeof given === "object" && !Array.isArray(given) ? given : null;
  if (mapObj) {
    byId = new Map();
    for (const [id, path] of Object.entries(mapObj)) own(byId, id, path);
  }
  if (files.some(item => item && typeof item === "object")) {
    byId = byId || new Map();
    for (const item of files) if (item && typeof item === "object") own(byId, item.record_id || item.id, item.proxy_path ?? item.path);
  }
  if (list.some(record => record.serverProxy !== undefined)) {
    byId = byId || new Map();
    for (const record of list) if (record.serverProxy !== undefined) own(byId, record.id, record.serverProxy || "");
  }
  if (byId) {
    for (const record of list) {
      const path = byId.get(record.id) || "";
      // Absolute paths as given (like the `proxies` listing); a relative one
      // is the case's artifacts path.
      record.proxyPath = !path ? "" : /^(?:[A-Za-z]:[\\\/]|[\\\/])/.test(path) ? path : absolutizeArtifactPath(path);
    }
    return "server";
  }
  const proxyIndexByPrefix = new Map();
  files.forEach((path, index) => {
    const name = String(path).split(/[\\\/]/).pop() || "";
    if (!name.endsWith(".mp4")) return;
    // R4 D5: current proxies are path-free — proxy_<sanitized id>_<hash8>(_NNN).mp4.
    const fresh = /^proxy_(.+)_[0-9a-f]{8}(?:_\d{3})?\.mp4$/.exec(name);
    if (fresh && !proxyIndexByPrefix.has(fresh[1])) proxyIndexByPrefix.set(fresh[1], index);
    // Older "<sanitized id or path>_proxy_…" names keep working.
    for (let at = name.indexOf("_proxy_"); at >= 0; at = name.indexOf("_proxy_", at + 1)) {
      const prefix = name.slice(0, at);
      if (!proxyIndexByPrefix.has(prefix)) proxyIndexByPrefix.set(prefix, index);
    }
  });
  for (const record of list) {
    let best = -1;
    for (const key of [record.id, record.path].filter(Boolean).map(sanitizeSelector)) {
      const index = proxyIndexByPrefix.get(key);
      if (index != null && (best < 0 || index < best)) best = index;
    }
    record.proxyPath = best >= 0 ? files[best] : "";
  }
  return "names";
}
resolveProxyPaths(records, DATA);

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
  // IN/OUT ranges are case annotations like marks (A1/A2): the server's (or
  // the bundle's) value + this window's per-id drafts, nothing else. The old
  // whole-map RANGES_KEY store is migrated once (migrateLegacyRanges).
  ranges: {},
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
var draftsUnpersisted = false;    // the last flush could not be stored (quota) — warn before unload
var restoredConflictIds = [];     // adopted drafts that carry a conflict (90-sync registers them)
// A7: a closed window's draft for an id this window also has a draft of.
// It is neither adopted over ours nor left in storage to be re-adopted some
// day: once our draft is confirmed it becomes a conflict (90-sync
// settleOrphanDrafts) — or goes, when it holds exactly what was saved.
var orphanDrafts = new Map();     // id -> { key, writer, draft }
// A2: RANGES_KEY leftovers of a standalone (file://) page: never applied
// over the bundle, only reported (90-sync legacy-ranges notice).
var legacyRangesLeft = null;      // { count, ranges } | null
// A3: records-meta could not read the case's annotations (annotations.error):
// the viewer shows none of the server's marks, so it must never save over
// them (90-sync: read-only banner, saves blocked).
var annotationsError = "";
// R8 U1: why the annotations are locked — "annotations" (records-meta could
// not read them) or "case" (case.json was not read: which case this is is
// unknown, so nothing may be written or kept under its keys). The texts
// differ (lockKey), the read-only path is the same.
var annotationsLockCause = "";
// Ids whose mark row the server may hold (hydrated rows, rows this window
// sent since, server values taken over). A record left with no mark, note
// or range sends `deleted_ids` only when its id is here — a record that
// only ever carried tags never asks the server to delete anything (A1).
var markRowMaybe = new Set();
// Bumped on every write of state.marks / notes / tags (touchAnnotation, a
// replayed draft, hydration, a server value applied by 90-sync): the
// filter memo of the mark/tag chips keys on it (40-grid filterStateKey, U7).
var annotationGen = 0;
// R7 U4: `id` = the one record whose annotations changed (touchAnnotation,
// a replayed draft, a server value); no id = everything may have changed
// (hydration). The mark tally and the filtered-list memo use it to update
// by the changed ids instead of walking every mark / record again.
function bumpAnnotations(id) {
  annotationGen += 1;
  markTallyNote(id);
  if (typeof noteFilterTouched === "function") noteFilterTouched(id);
}

// R7 U4: the mark counts shown on every render (검토 n/m, the triage line,
// the selection line, summary().marked) kept INCREMENTALLY: a full walk of
// the mark map only after hydration or a swapped map, otherwise O(changed
// ids) — j/k and a click cost nothing here, a mark costs one id.
//   all       — keys in state.marks (summary().marked, the selection line)
//   n         — of those, records of this case (review progress)
//   byStatus  — reviewed / important / needs_verification among n
var markTally = null;
var markTallyTouched = new Set();
var markTallyAll = true;
var markTallyFullCount = 0; // full walks of the mark map (FTViewer._debugStats().tallyFull, harness)
function markTallyNote(id) {
  if (!markTallyTouched) return; // before this file ran: the first tally is a full count anyway
  if (id == null || markTallyTouched.size >= 4096) { markTallyAll = true; markTallyTouched.clear(); }
  else if (!markTallyAll) markTallyTouched.add(id);
}
// R8 U5: the bucket of a mark is its status; a mark of an id this case does
// not list gets its own marker (MARK_TALLY_OFFCASE) — never "", which is
// also what a mark with an empty / missing status yields (that mark is a
// mark of this case and counts in `n`, as the pre-R7 count did).
var MARK_TALLY_OFFCASE = "\u0000offcase";
function markTallyBucket(id) {
  const mark = state.marks[id];
  if (!mark) return null;
  return recordById.has(id) ? String(mark.status || "") : MARK_TALLY_OFFCASE;
}
function markTallyMove(tally, from, to) {
  if (from !== null) {
    tally.all -= 1;
    if (from !== MARK_TALLY_OFFCASE) { tally.n -= 1; tally.byStatus[from] = (tally.byStatus[from] || 0) - 1; }
  }
  if (to !== null) {
    tally.all += 1;
    if (to !== MARK_TALLY_OFFCASE) { tally.n += 1; tally.byStatus[to] = (tally.byStatus[to] || 0) + 1; }
  }
}
function markTallyNow() {
  if (markTallyAll || !markTally || markTally.ref !== state.marks || markTally.records !== recordById.size) {
    markTallyFullCount += 1;
    const tally = { ref: state.marks, records: recordById.size, all: 0, n: 0, byStatus: {}, seen: new Map() };
    for (const id in state.marks) {
      if (!Object.prototype.hasOwnProperty.call(state.marks, id)) continue;
      const bucket = markTallyBucket(id);
      if (bucket === null) continue;
      tally.seen.set(id, bucket);
      markTallyMove(tally, null, bucket);
    }
    markTally = tally;
    markTallyAll = false;
    markTallyTouched.clear();
    return markTally;
  }
  for (const id of markTallyTouched) {
    const was = markTally.seen.has(id) ? markTally.seen.get(id) : null;
    const now = markTallyBucket(id);
    if (was === now) continue;
    markTallyMove(markTally, was, now);
    if (now === null) markTally.seen.delete(id); else markTally.seen.set(id, now);
  }
  markTallyTouched.clear();
  return markTally;
}
const DRAFT_FLUSH_MS = 250;
const DRAFT_PREFIX = "ft.draft." + CASE_KEY + ".";
const ALIVE_PREFIX = "ft.alive:" + CASE_KEY + ":";
// Liveness of the other windows on this case (§10.3): each refreshes its
// heartbeat every 5 s; pagehide (a real unload, not the back/forward cache)
// replaces it with ALIVE_CLOSED. Another writer's drafts are adopted only
// when its page is closed, or when its heartbeat is ALIVE_STALE_MS old AND
// it does not answer a same-origin BroadcastChannel ping. A throttled
// background tab still answers and keeps its drafts; a FROZEN tab (page
// lifecycle "frozen", discarded) runs no script, cannot answer, and IS
// adopted — safely: the adopted draft goes out with ITS base revision, and
// when the frozen tab resumes, its in-memory copy goes out with the same
// old base; whichever lands second meets a moved revision and is a
// conflict (A11), never an overwrite.
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
function draftNormRange(r) {
  return r && typeof r === "object" && (Number.isFinite(r.in) || Number.isFinite(r.out))
    ? { in: Number.isFinite(r.in) ? r.in : null, out: Number.isFinite(r.out) ? r.out : null }
    : null;
}
function draftRangeOf(id) {
  return draftNormRange(state.ranges && state.ranges[id]);
}
// The annotation values a draft stores for `id` (post-mutation state).
// Copies: a later in-place edit of state must not rewrite the draft (A14).
function draftValuesOf(id) {
  return {
    mark: state.marks[id] ? { ...state.marks[id] } : null,
    note: state.notes[id] || "",
    tags: [...(state.tags[id] || [])],
    examiner: state.examiners[id] ?? null,
    range: draftRangeOf(id)
  };
}
// Two drafts (or a draft and an annotation in draft shape) hold the same
// annotation: mark status, note, tags, range (A7).
function draftSameValues(a, b) {
  if (!a || !b) return false;
  const norm = d => JSON.stringify([
    d.mark && d.mark.status ? d.mark.status : null,
    String(d.note || "").trim(),
    [...(Array.isArray(d.tags) ? d.tags : [])].sort(),
    draftNormRange(d.range)
  ]);
  return norm(a) === norm(b);
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
  if (typeof noteOriginDrafts === "function") noteOriginDrafts();
  if (!quiet && typeof renderSaveState === "function") renderSaveState();
}

// R5 U1: while the case's annotations could not be read (A3, read-only),
// every annotation edit is refused at its entry point (marks, tags, notes,
// IN/OUT, undo/redo — the controls are disabled with the reason): an edit
// made on top of values this window never saw would later overwrite (or
// delete) the case's real marks. `annotationsReadOnly` (90-sync) is the
// switch once that part ran; before it, the load error itself.
function annotationsLocked() {
  if (typeof annotationsReadOnly !== "undefined") return !!annotationsReadOnly;
  return typeof annotationsError !== "undefined" && !!annotationsError;
}
// True (and says why, once per attempt) when an edit must not happen now.
// R8 U1: the i18n key of a read-only text for the current cause ("…Case"
// variant when case.json was not read).
function lockKey(key) {
  return annotationsLockCause === "case" && typeof I18N !== "undefined" && I18N.ko[key + "Case"] ? key + "Case" : key;
}
function refuseLockedEdit() {
  if (!annotationsLocked()) return false;
  if (typeof toast === "function") toast(t(lockKey("save.readonlyEdit")));
  return true;
}

// Records a mutation of `id` (call AFTER the mutation: the draft snapshots
// the resulting state). The base revision stays the one the first unsaved
// edit was made on; opts.base overrides it (a conflict resolution moves it
// to the server's revision). An id with an open conflict stays in conflict
// (its "mine" follows the edit) until the examiner resolves it.
// R5 U1: a draft written while the annotations were unreadable (it cannot
// happen through the UI any more, but a stored one from an older build or
// a path that slipped through can) is marked `unsafe_base`: it was made on
// values this window never saw, so it is sent with base_revision -1 — the
// server treats that as a conflict ALWAYS, even when this same window
// wrote the stored revision (the same-window exception does not apply).
// Only a conflict resolution (opts.base) clears the mark.
function touchAnnotation(id, opts) {
  if (typeof bumpAnnotations === "function") bumpAnnotations(id);
  state.examiners[id] = (state.examiner || "").trim() || state.examiners[id] || null;
  const prev = state.drafts[id];
  const known = state.revisions && Number.isFinite(state.revisions[id]) ? state.revisions[id] : 0;
  const resolved = !!(opts && Number.isFinite(opts.base));
  const base = resolved ? opts.base : (prev && Number.isFinite(prev.base) ? prev.base : known);
  const draft = { ...draftValuesOf(id), seq: nextDraftSeq(), base };
  if (annotationsLocked() || (!resolved && prev && prev.unsafe_base === true)) draft.unsafe_base = true;
  const conflicted = typeof conflictIds !== "undefined" && conflictIds.has(id);
  if (conflicted && prev && prev.conflict) draft.conflict = prev.conflict;
  state.drafts[id] = draft;
  if (conflicted) {
    if (typeof conflictEdited === "function") conflictEdited(id);
  } else {
    state.dirtyIds.add(id);
  }
  draftFlushKeys.add(id);
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
  if (typeof bumpAnnotations === "function") bumpAnnotations(id);
  if (draft.mark) state.marks[id] = draft.mark;
  else delete state.marks[id];
  state.notes[id] = draft.note || "";
  // A copy: the tag editors change state.tags lists (A14).
  state.tags[id] = Array.isArray(draft.tags) ? [...draft.tags] : [];
  state.examiners[id] = draft.examiner ?? null;
  if (Object.prototype.hasOwnProperty.call(draft, "range")) {
    const range = draftNormRange(draft.range);
    if (range) state.ranges[id] = range;
    else delete state.ranges[id];
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
  // R5 U1: made while the annotations were unreadable — always a conflict.
  if (draft.unsafe_base === true) adopted.unsafe_base = true;
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

// The server's value of `id` in draft shape (hydrated maps, before drafts).
function serverDraftValues(annotations, id) {
  const mark = (annotations.marks || []).find(m => annotationMarkId(m) === id) || null;
  const tags = (annotations.tags || []).find(entry => entry && entry.id === id);
  return {
    mark: mark && mark.status && mark.status !== "noted" ? { status: mark.status } : null,
    note: mark ? mark.note || "" : "",
    tags: tags && Array.isArray(tags.tags) ? tags.tags : [],
    range: mark ? draftNormRange(mark.range) : null
  };
}

// A closed (or dead) writer's draft for an id this window already holds a
// draft of (A7). Identical → the orphan goes now. Otherwise it waits in
// orphanDrafts and becomes a conflict once ours is confirmed (90-sync
// settleOrphanDrafts) — it is never silently dropped and never re-adopted
// at some later boot.
function noteOrphanDraft(entry) {
  const mine = state.drafts[entry.id];
  if (mine && draftSameValues(mine, entry.draft)) {
    try { localStorage.removeItem(entry.key); } catch (error) { /* retried next boot */ }
    return;
  }
  orphanDrafts.set(entry.id, { key: entry.key, writer: entry.writer, draft: entry.draft });
}

// A2: the pre-R4 whole-map range store (RANGES_KEY) is retired. Served:
// each stored range that differs from the server's (and has no draft of
// its own) is moved into a per-id draft carrying a conflict — the examiner
// picks the server's or this browser's range in the conflict list; a
// deleted range never comes back on its own. The key goes once every
// entry is stored. Standalone (file://): the bundle is the reference — the
// leftovers are never applied, only reported (legacyRangesLeft).
// R5 U2: nothing is migrated while the annotations could not be read (the
//   "server" side of the comparison would be the empty read-only view and
//   a migrated draft would carry values this window never saw); the key
//   stays for a boot that can read them.
// R5 U3: an id this case does not list (yet: a live index still growing)
//   is moved to RANGES_HELD_KEY — never dropped with the retired key — and
//   every later boot re-checks the held entries.
function migrateLegacyRanges(annotations) {
  if (typeof annotationsError !== "undefined" && annotationsError) return 0;
  let raw = null;
  let heldRaw = null;
  try {
    raw = localStorage.getItem(RANGES_KEY);
    heldRaw = typeof RANGES_HELD_KEY === "string" ? localStorage.getItem(RANGES_HELD_KEY) : null;
  } catch (error) { return 0; }
  if (raw == null && heldRaw == null) return 0;
  const parseMap = text => {
    if (text == null) return {};
    try { const v = JSON.parse(text); return v && typeof v === "object" ? v : {}; } catch (error) { return {}; }
  };
  const entries = Object.entries(parseMap(raw)).map(([id, r]) => [id, draftNormRange(r)]).filter(([, r]) => r);
  const served = location.protocol === "http:" || location.protocol === "https:";
  if (!served) {
    if (raw == null) return 0;
    if (entries.length) legacyRangesLeft = { count: entries.length, ranges: Object.fromEntries(entries) };
    else { try { localStorage.removeItem(RANGES_KEY); } catch (error) { /* harmless */ } }
    return 0;
  }
  // Held entries first; a value of the same id still in RANGES_KEY wins.
  const all = new Map(Object.entries(parseMap(heldRaw)).map(([id, r]) => [id, draftNormRange(r)]).filter(([, r]) => r));
  for (const [id, range] of entries) all.set(id, range);
  const stillHeld = {};
  let moved = 0;
  let complete = true;
  for (const [id, range] of all) {
    if (!recordById.has(id)) { stillHeld[id] = range; continue; } // U3: held, re-checked next boot
    if (state.drafts[id]) continue; // a draft is newer
    const server = serverDraftValues(annotations, id);
    if (JSON.stringify(server.range) === JSON.stringify(range)) continue;
    const known = Number.isFinite(state.revisions[id]) ? state.revisions[id] : 0;
    const draft = {
      mark: state.marks[id] ? { ...state.marks[id] } : null, note: state.notes[id] || "", tags: [...(state.tags[id] || [])],
      examiner: state.examiners[id] ?? null, range, seq: nextDraftSeq(), base: known,
      conflict: {
        server: {
          status: server.mark ? server.mark.status : null, marked_unix: null, examiner: state.examiners[id] ?? null,
          note: server.note, range: server.range, tags: [...server.tags]
        },
        server_revision: known, queued: false, legacy: true
      }
    };
    try { localStorage.setItem(draftKey(writerId(), id), JSON.stringify(draft)); } catch (error) {
      reportStorageError(error);
      complete = false;
      break;
    }
    state.drafts[id] = draft;
    state.ranges[id] = range;
    restoredConflictIds.push(id);
    moved += 1;
  }
  if (!complete) return moved; // both keys stay: retried next boot
  try {
    if (Object.keys(stillHeld).length) localStorage.setItem(RANGES_HELD_KEY, JSON.stringify(stillHeld));
    else if (heldRaw != null) localStorage.removeItem(RANGES_HELD_KEY);
  } catch (error) {
    reportStorageError(error);
    return moved; // the unknown ids could not be held: RANGES_KEY stays
  }
  if (raw != null) { try { localStorage.removeItem(RANGES_KEY); } catch (error) { /* retried next boot */ } }
  return moved;
}

function hydrateAnnotations(annotations) {
  if (typeof bumpAnnotations === "function") bumpAnnotations();
  state.marks = {}; state.notes = {}; state.tags = {}; state.examiners = {}; state.ranges = {};
  state.revisions = {};
  annotationsError = annotations && annotations.error ? String(annotations.error) : "";
  annotationsLockCause = annotationsError ? "annotations" : "";
  // R8 U1: case.json unreadable — the same read-only path (no save, no
  // edit, no migration of browser leftovers), its own texts.
  if (!annotationsError && typeof caseFileUnreadable !== "undefined" && caseFileUnreadable) {
    annotationsError = "case.json";
    annotationsLockCause = "case";
  }
  for (const [id, rev] of Object.entries(annotations.revisions || {})) {
    if (Number.isFinite(Number(rev))) state.revisions[id] = Number(rev);
  }
  markRowMaybe = new Set();
  for (const mark of annotations.marks || []) {
    const id = annotationMarkId(mark);
    if (!id) continue;
    markRowMaybe.add(id);
    if (mark.status !== "noted") state.marks[id] = { status: mark.status, marked_unix: mark.marked_unix };
    state.notes[id] = mark.note || "";
    state.examiners[id] = mark.examiner || null;
    const range = draftNormRange(mark.range);
    if (range) state.ranges[id] = range;
  }
  // Copies: the server's lists stay the server's (A14 — a tag edit must
  // never rewrite DATA.annotations, the conflict list reads it).
  for (const entry of annotations.tags || []) if (entry && entry.id) state.tags[entry.id] = Array.isArray(entry.tags) ? [...entry.tags] : [];
  state.dirtyIds = new Set();
  state.drafts = {};
  skippedDraftWriters = new Set();
  restoredConflictIds = [];
  orphanDrafts = new Map();
  legacyRangesLeft = null;
  migrateLegacyDrafts();
  // This window's own drafts first (a hosted destroy()+init() keeps the
  // writer token); a closed window's draft for the same id is an orphan (A7).
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
    else if (verdict === "closed") {
      if (!state.drafts[entry.id]) adoptDraft(entry.id, entry.draft, entry.writer, entry.key);
      else noteOrphanDraft(entry);
    } else skippedDraftWriters.add(entry.writer);
  }
  migrateLegacyRanges(annotations);
  if (draftFlushKeys.size) scheduleDraftFlush();
}

// --- Dead-window detection (§10.3) -----------------------------------------
// Same-origin windows on this case answer {type:"ping"} for their writer
// token on BroadcastChannel ft-alive:<case>. A heartbeat can go stale in a
// live tab (timer throttling, storage full); the ping tells. A frozen tab
// cannot answer (no script runs) — see ALIVE_* above for why that is safe.
var aliveChannel = null;
var alivePings = new Map(); // nonce -> Set of writers that answered
function openAliveChannel() {
  if (aliveChannel || typeof BroadcastChannel !== "function" || ftSignal.aborted) return aliveChannel;
  try { aliveChannel = new BroadcastChannel("ft-alive:" + CASE_KEY); } catch (error) { aliveChannel = null; return null; }
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
    if (!gone.has(entry.writer)) continue;
    if (state.drafts[entry.id]) { noteOrphanDraft(entry); continue; }
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
    marked: markTallyNow().all,
    noted: records.filter(r => (state.notes[r.id] || "").trim()).length
  };
};
