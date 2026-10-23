// Picker extras (80-paths.js owns the dialog itself): quick-access places,
// clickable breadcrumbs, an in-folder name filter, icons + image sizes,
// a remembered start folder per mode, and one-click "새 폴더" so a case
// folder can be created without leaving the picker (POST /api/mkdir).
const PICKER_PLUS = { places: null, wsl: false, newOnOpen: false };

// "C:\a\b" → "C:\a", "C:\a" → "C:\", "/mnt/c" → "/mnt", "/mnt" → "/".
function parentPath(p) {
  const s = String(p || "").trim().replace(/[\\/]+$/, "");
  if (!s || /^[A-Za-z]:$/.test(s)) return "";
  const i = Math.max(s.lastIndexOf("\\"), s.lastIndexOf("/"));
  if (i < 0) return "";
  const head = s.slice(0, i);
  if (!head) return "/";
  return /^[A-Za-z]:$/.test(head) ? head + "\\" : head;
}

function pickerEntryHtml(e) {
  const icon = e.is_case ? "i-project" : e.dir ? "i-folder" : "i-file-image";
  const meta = e.is_case ? `<span class="pk-badge">케이스</span>`
    : !e.dir ? `<span class="pk-meta">${e.size != null ? esc(fmtBytes(e.size)) + " · " : ""}E01 이미지</span>` : "";
  return `<svg class="ic pk-ic${e.dir ? "" : " img"}" aria-hidden="true"><use href="#${icon}"/></svg>` +
    `<span class="pk-name${e.dir ? " dir" : ""}">${esc(e.name)}</span>${meta}`;
}

function pickerSeed(targetId, mode) {
  const typed = $(targetId).value.trim();
  if (typed) return PICKER_PLUS.newOnOpen ? (parentPath(typed) || typed) : typed;
  // Nothing typed or remembered: start somewhere useful (Windows home /
  // home) rather than the bare drive list.
  return (loadPrefs().pickerLast || {})[mode] || ((PICKER_PLUS.places || [])[0] || {}).path || "";
}
function pickerRemember(mode, path) {
  const last = loadPrefs().pickerLast || {};
  savePrefs({ pickerLast: Object.assign({}, last, { [mode]: path }) });
}

function pickerPlusOpen(mode) {
  $("pickerFilter").value = "";
  $("pickerNewRow").hidden = true;
  $("pickerNewFolder").hidden = mode !== "dir";
  loadPlaces();
}

async function loadPlaces() {
  if (!PICKER_PLUS.places) {
    const res = await jgetSoft("/api/places");
    PICKER_PLUS.places = res.ok ? res.places || [] : [];
    PICKER_PLUS.wsl = !!res.wsl;
    if (PICKER_PLUS.wsl) $("sourcePath").placeholder = "/mnt/e/BLACKBOX 또는 C:\\증거\\disk.E01";
  }
  renderPlaces();
}

// Folders of recently used evidence / cases, most recent first.
function recentFolders() {
  const prefs = loadPrefs();
  const seen = new Set();
  const out = [];
  for (const p of [...(prefs.recentSources || []), ...(prefs.recentCases || [])]) {
    const dir = /\.(e|ex|l|s)\d+$/i.test(p) ? parentPath(p) : p;
    if (dir && !seen.has(dir.toLowerCase())) { seen.add(dir.toLowerCase()); out.push(dir); }
    if (out.length >= 4) break;
  }
  return out;
}

function renderPlaces() {
  const nav = $("pickerPlaces");
  const icon = k => k === "drive" ? "i-save" : "i-folder";
  const btn = (label, path, kind) =>
    `<button type="button" class="quiet pk-place" data-path="${esc(path)}" title="${esc(path)}">` +
    `<svg class="ic" aria-hidden="true"><use href="#${icon(kind)}"/></svg><span>${esc(label)}</span></button>`;
  const recent = recentFolders();
  const places = PICKER_PLUS.places || [];
  nav.innerHTML =
    (recent.length ? `<div class="pk-group">최근 위치</div>` +
      recent.map(p => btn(p.split(/[\\/]/).filter(Boolean).pop() || p, p, "folder")).join("") : "") +
    (places.length ? `<div class="pk-group">바로 가기</div>` + places.map(p => btn(p.label, p.path, p.kind)).join("") : "");
  markActivePlace();
}
function markActivePlace() {
  const cur = normPath(pickerCurrent);
  $("pickerPlaces").querySelectorAll(".pk-place").forEach(b => {
    b.classList.toggle("active", !!pickerCurrent && normPath(b.dataset.path) === cur);
  });
}
$("pickerPlaces").addEventListener("click", e => {
  const b = e.target.closest(".pk-place");
  if (b) browseTo(b.dataset.path, true);
});

function renderCrumbs(path) {
  const nav = $("pickerCrumbs");
  if (!path) { nav.innerHTML = `<span class="pk-crumb cur" aria-current="location">드라이브 목록</span>`; return; }
  const win = /^[A-Za-z]:/.test(path);
  const parts = path.split(/[\\/]+/).filter(Boolean);
  const crumbs = [];
  let acc = win ? "" : "/";
  if (!win) crumbs.push({ label: "/", path: "/" });
  parts.forEach((part, i) => {
    acc = win ? (i === 0 ? part + "\\" : acc.replace(/\\?$/, "\\") + part) : acc.replace(/\/?$/, "/") + part;
    crumbs.push({ label: part, path: acc });
  });
  nav.innerHTML = crumbs.map((c, i) => i === crumbs.length - 1
    ? `<span class="pk-crumb cur" aria-current="location">${esc(c.label)}</span>`
    : `<button type="button" class="quiet pk-crumb" data-path="${esc(c.path)}">${esc(c.label)}</button>` +
      `<svg class="ic pk-sep" aria-hidden="true"><use href="#i-chevron-right"/></svg>`).join("");
  nav.scrollLeft = nav.scrollWidth;
}
$("pickerCrumbs").addEventListener("click", e => {
  const b = e.target.closest("button.pk-crumb");
  if (b) browseTo(b.dataset.path, true);
});

function pickerPlusRendered() {
  renderCrumbs(pickerCurrent);
  markActivePlace();
  $("pickerNewFolder").disabled = !pickerCurrent;
  applyPickerFilter();
  const sel = $("pickerList").querySelector('[aria-selected="true"]');
  if (sel && sel.hidden) {
    const first = pickerOptions().find(o => o.dataset.name !== undefined) || pickerOptions()[0];
    pickerFocus(first, document.activeElement === sel);
  }
  if (PICKER_PLUS.newOnOpen) {
    PICKER_PLUS.newOnOpen = false;
    if (pickerCurrent) showNewFolderRow();
  }
}

function applyPickerFilter() {
  const q = $("pickerFilter").value.trim().toLowerCase();
  let shown = 0;
  $("pickerList").querySelectorAll('[role="option"]').forEach(li => {
    if (li.dataset.name === undefined) return; // the ".." row always stays
    // Dot folders (.cache, .git …) are noise when hunting evidence; typing
    // a leading "." shows them.
    const dot = li.dataset.name.startsWith(".") && !q.startsWith(".");
    li.hidden = dot || (!!q && !li.dataset.name.includes(q));
    if (!li.hidden) shown++;
  });
  if (q) $("pickerStatus").textContent = shown ? `‘${q}’ — ${shown}개 일치` : `‘${q}’와 일치하는 항목이 없습니다`;
}
$("pickerFilter").addEventListener("input", applyPickerFilter);
$("pickerFilter").addEventListener("keydown", e => {
  if (e.isComposing) return;
  const first = pickerOptions().find(o => o.dataset.name !== undefined);
  if (e.key === "ArrowDown" && first) { e.preventDefault(); pickerFocus(first); }
  else if (e.key === "Enter" && first) { e.preventDefault(); pickerFocus(first); first._act(); }
});

function suggestedFolderName() {
  if (pickerTarget !== "caseDir") return "새 폴더";
  const src = $("sourcePath").value.trim().replace(/[\\/]+$/, "");
  const base = (src.split(/[\\/]/).pop() || "").replace(/\.(e|ex|l|s)\d+$/i, "").replace(/[:*?"<>|]/g, "");
  const d = new Date(), p2 = n => String(n).padStart(2, "0");
  return `ftcase-${base ? base + "-" : ""}${d.getFullYear()}${p2(d.getMonth() + 1)}${p2(d.getDate())}-${p2(d.getHours())}${p2(d.getMinutes())}`;
}
function showNewFolderRow() {
  if (!pickerCurrent) { $("pickerStatus").textContent = "드라이브를 먼저 연 뒤 새 폴더를 만들 수 있습니다"; return; }
  $("pickerNewRow").hidden = false;
  $("pickerNewErr").textContent = "";
  $("pickerNewName").value = suggestedFolderName();
  $("pickerNewName").focus();
  $("pickerNewName").select();
}
$("pickerNewFolder").addEventListener("click", showNewFolderRow);
$("pickerNewCancel").addEventListener("click", () => { $("pickerNewRow").hidden = true; $("pickerNewFolder").focus(); });
$("pickerNewName").addEventListener("keydown", e => {
  if (e.key !== "Escape") return;
  e.preventDefault();
  e.stopPropagation(); // Esc cancels the form, not the whole picker
  $("pickerNewRow").hidden = true;
  $("pickerNewFolder").focus();
});
$("pickerNewRow").addEventListener("submit", async e => {
  e.preventDefault();
  const ok = $("pickerNewOk");
  ok.disabled = true;
  const res = await jpost("/api/mkdir", { parent: pickerCurrent, name: $("pickerNewName").value });
  ok.disabled = false;
  if (!res.ok) { $("pickerNewErr").textContent = res.error || "폴더를 만들지 못했습니다"; $("pickerNewName").focus(); return; }
  $("pickerNewRow").hidden = true;
  await browseTo(res.path, false);
  $("pickerStatus").textContent = `새 폴더를 만들었습니다 — ‘이 폴더 선택’을 누르면 사용합니다`;
  announce("새 폴더를 만들었습니다: " + res.path);
  $("pickerSelect").focus();
});
$("btnNewCase").addEventListener("click", () => {
  PICKER_PLUS.newOnOpen = true;
  openPicker("caseDir", "dir");
});
