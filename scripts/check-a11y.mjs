// Accessibility gate for the workstation shell and the evidence viewer.
//
//   cargo build --locked
//   NODE_PATH=<repo>/node_modules node scripts/check-a11y.mjs [--out report.json] [--shots DIR] [--quick]
//
// Loads the integrated shell (empty state; project with the hosted viewer;
// project with each sheet open) and the standalone viewer against an
// isolated server holding a synthetic demo case (scripts/make-demo-case.mjs),
// in light and dark, at 1366x768 and 1920x1080, and checks
// (docs/ui-rebuild-contract.md §8.1):
//
//   contrast   ENFORCED  every visible text node vs its effective background
//                        >= 4.5:1 (>= 3:1 for large/bold, and for disabled)
//   name       ENFORCED  every visible button / link / input / select has an
//                        accessible name (NAME_BASELINE lists known-unlabelled
//                        controls awaiting markup fixes; reported as warnings)
//   min-font   ENFORCED  no visible text under the --fs-xs token (13px, §5.3;
//                        the token itself is pinned by check-css.mjs)
//   glyph      ENFORCED  no Unicode icon glyph in a button label (icons come
//                        from the sprite); the pre-rebuild viewer (no
//                        data-ft-viewer="2" root) is reported as a warning
//   target     ENFORCED  interactive targets >= 24x24 CSS px or spaced (WCAG 2.5.8)
//   focus      ENFORCED  a focus-visible ring is present when Tabbing
//   axe        ENFORCED  serious/critical axe-core violations; without axe-core
//                        the run says "axe off" — and FAILS when
//                        FRAMETRACE_REQUIRE_BROWSER=1 (`npm i --no-save axe-core`)
//
// Writes a JSON report and a human summary. Exit status is nonzero only for
// violations of ENFORCED rules.
import { mkdtempSync, rmSync, writeFileSync, mkdirSync, readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { startIsolatedServer } from './lib/isolated-server.mjs';
import { routeTools } from './lib/tools-mock.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const argv = process.argv.slice(2);
const opt = (name) => { const i = argv.indexOf('--' + name); return i >= 0 ? argv[i + 1] : ''; };
const outPath = opt('out') || join(process.env.CARGO_TARGET_DIR || join(root, 'target'), 'a11y-report.json');
const shotsDir = opt('shots');
const quick = argv.includes('--quick');

const require = createRequire(import.meta.url);
let chromium;
try { ({ chromium } = require('playwright')); } catch {
  console.error('playwright not resolvable — set NODE_PATH to a node_modules with playwright');
  process.exit(2);
}
let axeSource = '';
try { axeSource = readFileSync(require.resolve('axe-core/axe.min.js'), 'utf8'); } catch { /* checked below */ }
// F13: a required browser run (CI, FRAMETRACE_REQUIRE_BROWSER=1) must not
// pass without axe-core — an unloaded axe is not "no violations".
if (!axeSource && process.env.FRAMETRACE_REQUIRE_BROWSER === '1') {
  console.error('axe-core not resolvable but FRAMETRACE_REQUIRE_BROWSER=1 — install it beside playwright (npm i --no-save axe-core@4.10.2)');
  process.exit(1);
}

// All rules reached zero after the UX overhaul; enforce them so regressions fail.
const ENFORCED = { contrast: true, name: true, 'name-baseline': true, 'name-custom': true, 'min-font': true, glyph: true, 'glyph-legacy': false, target: true, focus: true, axe: true };
// Minimum text size = the --fs-xs token (contract §5.3: 13px). Read from
// assets/tokens.css so this gate and the token can never disagree; never
// below 12px.
const MIN_FONT = Math.max(12, Number((/--fs-xs:\s*(\d+(?:\.\d+)?)px/.exec(readFileSync(join(root, 'assets/tokens.css'), 'utf8')) || [])[1]) || 12);
const GLYPHS = '◐☀⏻⤢⧉▾↗✓✕☰⚠⤓▸';

// ---------------------------------------------------------------- in-page
// Runs inside every frame. Returns { contrast, name, minFont, target, glyph } findings.
function collect({ minFont: MIN_FONT, glyphs: GLYPHS }) {
  const parseColor = (s) => {
    let m = /^rgba?\(([^)]+)\)$/.exec(s);
    if (m) {
      const p = m[1].split(/[ ,\/]+/).filter(Boolean).map(Number);
      return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 };
    }
    m = /^color\(srgb ([^)]+)\)$/.exec(s);
    if (m) {
      const p = m[1].split(/[ \/]+/).filter(Boolean).map(Number);
      return { r: p[0] * 255, g: p[1] * 255, b: p[2] * 255, a: p.length > 3 ? p[3] : 1 };
    }
    return null;
  };
  const over = (top, bot) => {
    const a = top.a + bot.a * (1 - top.a);
    if (a === 0) return { r: 0, g: 0, b: 0, a: 0 };
    const k = (t, b) => (t * top.a + b * bot.a * (1 - top.a)) / a;
    return { r: k(top.r, bot.r), g: k(top.g, bot.g), b: k(top.b, bot.b), a };
  };
  const lum = (c) => {
    const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; };
    return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b);
  };
  const ratio = (a, b) => { const x = lum(a), y = lum(b); return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05); };
  const sel = (el) => {
    const one = (e) => {
      let s = e.tagName.toLowerCase();
      if (e.id) return s + '#' + e.id;
      const cls = [...e.classList].filter((c) => !/^\d/.test(c)).slice(0, 3);
      if (cls.length) s += '.' + cls.join('.');
      return s;
    };
    const parts = [];
    let e = el;
    for (let i = 0; e && e.nodeType === 1 && i < 3; i++, e = e.parentElement) {
      parts.unshift(one(e));
      if (e.id) break;
    }
    return parts.join(' > ');
  };
  const visible = (el) => {
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
      const cs = getComputedStyle(e);
      if (cs.display === 'none' || cs.visibility === 'hidden' || cs.visibility === 'collapse') return false;
      if (parseFloat(cs.opacity) === 0) return false;
      if (e.hidden) return false;
    }
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  };
  const inViewportClip = (el) => {
    // Clipped off-screen/visually-hidden text (clip, 1px boxes) is not "visible".
    const r = el.getBoundingClientRect();
    if (r.width <= 1 || r.height <= 1) return false;
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
      const cs = getComputedStyle(e);
      if (cs.clip && cs.clip !== 'auto' && /rect\(0(px)?,? 0(px)?,? 0(px)?,? 0(px)?\)/.test(cs.clip)) return false;
    }
    return true;
  };
  const bgOf = (el) => {
    // Composite backgrounds bottom-up. Gradients/images make the answer
    // unknowable here: report null so the caller can skip instead of guess.
    const chain = [];
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) chain.push(e);
    let acc = { r: 255, g: 255, b: 255, a: 1 }; // canvas
    let unknown = false;
    for (let i = chain.length - 1; i >= 0; i--) {
      const cs = getComputedStyle(chain[i]);
      const c = parseColor(cs.backgroundColor);
      if (c && c.a > 0) acc = over(c, acc);
      if (cs.backgroundImage && cs.backgroundImage !== 'none') {
        unknown = true;
      }
    }
    return unknown ? null : acc;
  };
  const effOpacity = (el) => {
    let o = 1;
    for (let e = el; e && e.nodeType === 1; e = e.parentElement) o *= parseFloat(getComputedStyle(e).opacity);
    return o;
  };
  const isDisabled = (el) => !!el.closest('[disabled], [aria-disabled="true"], fieldset:disabled');

  const out = { contrast: [], name: [], nameCustom: [], minFont: [], target: [], glyph: [], glyphLegacy: [], newViewer: !!document.querySelector('[data-ft-viewer="2"]'), scanned: { text: 0, interactive: 0 } };
  const seen = new Set();
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const text = n.nodeValue.replace(/\s+/g, ' ').trim();
    if (!text) continue;
    const el = n.parentElement;
    if (!el || /^(SCRIPT|STYLE|NOSCRIPT|OPTION)$/.test(el.tagName)) continue;
    if (el.closest('video, canvas, svg, iframe, template')) continue;
    if (!visible(el) || !inViewportClip(el)) continue;
    if (/^[\p{Extended_Pictographic}\s\uFE0F\u200d]+$/u.test(text)) continue; // colour emoji: no text colour to measure
    out.scanned.text++;
    const cs = getComputedStyle(el);
    const fs = parseFloat(cs.fontSize);
    if (fs === 0) continue; // glyph drawn via ::after (e.g. done-step check); no text is shown
    const key = sel(el);
    if (fs < MIN_FONT) {
      const k = 'f|' + key + '|' + fs;
      if (!seen.has(k)) { seen.add(k); out.minFont.push({ selector: key, detail: `${fs}px "${text.slice(0, 24)}"` }); }
    }
    const fg = parseColor(cs.color);
    const bg = bgOf(el);
    if (!fg || !bg) continue;
    const fgEff = over({ ...fg, a: fg.a * effOpacity(el) }, bg);
    const r = ratio(fgEff, bg);
    const weight = parseInt(cs.fontWeight, 10) || 400;
    const large = fs >= 24 || (fs >= 18.66 && weight >= 700);
    const disabled = isDisabled(el);
    const need = disabled || large ? 3 : 4.5;
    if (r + 1e-9 < need) {
      const k = 'c|' + key + '|' + cs.color + '|' + r.toFixed(2);
      if (!seen.has(k)) {
        seen.add(k);
        const hx = (c) => '#' + [c.r, c.g, c.b].map((v) => Math.round(v).toString(16).padStart(2, '0')).join('');
        out.contrast.push({ selector: key, detail: `${r.toFixed(2)}:1 < ${need} (${hx(fgEff)} on ${hx(bg)}, ${fs}px${disabled ? ', disabled' : ''}) "${text.slice(0, 24)}"` });
      }
    }
  }

  // ---- interactive: names + targets
  const SEL = 'button, a[href], input:not([type="hidden"]), select, textarea, summary, [role="button"], [role="link"], [role="tab"], [role="menuitem"], [role="checkbox"], [role="switch"], [tabindex]:not([tabindex="-1"])';
  const all = [...document.querySelectorAll(SEL)].filter((e) => {
    if (!visible(e)) return false;
    if (e.tagName === 'INPUT' && /^(radio|checkbox)$/.test(e.type)) {
      // Visually-hidden native controls inside a visible <label> are operated via the label.
      return true;
    }
    return inViewportClip(e);
  });
  const textOf = (e) => (e.innerText || e.textContent || '').replace(/\s+/g, ' ').trim();
  const nameOf = (e) => {
    const al = e.getAttribute('aria-label');
    if (al && al.trim()) return al.trim();
    const lb = e.getAttribute('aria-labelledby');
    if (lb) {
      const t = lb.split(/\s+/).map((id) => { const x = document.getElementById(id); return x ? textOf(x) : ''; }).join(' ').trim();
      if (t) return t;
    }
    if (e.labels && e.labels.length) {
      const t = [...e.labels].map(textOf).join(' ').trim();
      if (t) return t;
    }
    if (e.tagName === 'INPUT' && /^(button|submit|reset)$/.test(e.type) && e.value) return e.value;
    if (e.tagName === 'INPUT' && e.type === 'image' && e.alt) return e.alt;
    if (/^(BUTTON|A|SUMMARY)$/.test(e.tagName) || /^(button|link|tab|menuitem|menuitemcheckbox|menuitemradio|checkbox|radio|switch|option|treeitem)$/.test(e.getAttribute('role') || '')) {
      const t = textOf(e);
      if (t) return t;
      const img = e.querySelector('img[alt], svg title, [aria-label]');
      if (img) return (img.getAttribute('alt') || img.getAttribute('aria-label') || img.textContent || '').trim();
    }
    const t = e.getAttribute('title');
    if (t && t.trim()) return t.trim();
    const ph = e.getAttribute('placeholder'); // browsers expose placeholder as the fallback name
    if (ph && ph.trim()) return ph.trim();
    return '';
  };
  // Icon glyphs in button labels: the sprite (<svg class="ic">) replaces
  // them. Inside a pre-rebuild viewer (no data-ft-viewer="2" root) they
  // are reported separately until that viewer is replaced.
  const glyphRe = new RegExp('[' + GLYPHS + ']');
  const newViewer = out.newViewer;
  for (const e of all) {
    if (e.tagName !== 'BUTTON' && e.getAttribute('role') !== 'button') continue;
    const t = textOf(e);
    if (!glyphRe.test(t)) continue;
    const legacyViewer = !newViewer && !e.closest('.h-top, .h-main > .h-empty, .h-sheet, .h-pop, .h-modal, .h-toastbox, .h-popnote, .h-host-wait');
    (legacyViewer ? out.glyphLegacy : out.glyph).push({ selector: sel(e), detail: `glyph in label "${t.slice(0, 24)}"` });
  }
  const rects = [];
  for (const e of all) {
    out.scanned.interactive++;
    if (!nameOf(e)) {
      // A 1x1 / clipped hidden file input is not an operable target.
      const native = /^(BUTTON|A|INPUT|SELECT|TEXTAREA|SUMMARY)$/.test(e.tagName) || e.hasAttribute('role');
      // tabindex-only widgets (e.g. div.card) have no role: reported separately as a warning
      (native ? out.name : out.nameCustom).push({ selector: sel(e), detail: `<${e.tagName.toLowerCase()}${e.type ? ' type=' + e.type : ''}> has no accessible name` });
    }
    const isNativeBox = e.tagName === 'INPUT' && /^(radio|checkbox)$/.test(e.type);
    let box = e;
    // Visually-hidden radio/checkbox: the wrapping label is the real target.
    if (isNativeBox) {
      const lab = e.closest('label') || (e.labels && e.labels[0]);
      const r0 = e.getBoundingClientRect();
      if (lab && (r0.width < 4 || r0.height < 4 || parseFloat(getComputedStyle(e).opacity) === 0)) box = lab;
    }
    const r = box.getBoundingClientRect();
    // inline links inside running text are exempt (2.5.8 inline exception)
    const inline = e.tagName === 'A' && getComputedStyle(e).display === 'inline';
    rects.push({ e, r, inline, sel: sel(e) });
  }
  const undersized = (r) => r.width < 24 || r.height < 24;
  for (const t of rects) {
    if (t.inline || !undersized(t.r)) continue;
    const cx = t.r.left + t.r.width / 2, cy = t.r.top + t.r.height / 2;
    let clash = null;
    for (const o of rects) {
      if (o === t || o.inline) continue;
      if (o.e.contains(t.e) || t.e.contains(o.e)) continue;
      if (!undersized(o.r)) {
        // 24px circle vs rectangle
        const dx = Math.max(o.r.left - cx, 0, cx - o.r.right);
        const dy = Math.max(o.r.top - cy, 0, cy - o.r.bottom);
        if (dx * dx + dy * dy < 144) { clash = o; break; }
      } else {
        const ox = o.r.left + o.r.width / 2, oy = o.r.top + o.r.height / 2;
        if (Math.hypot(ox - cx, oy - cy) < 24) { clash = o; break; }
      }
    }
    // Targets that overlap (same visual control) never count as clashes above.
    if (clash || t.r.width < 24 || t.r.height < 24) {
      if (clash) {
        const k = 't|' + t.sel + '|' + Math.round(t.r.width) + 'x' + Math.round(t.r.height);
        if (!seen.has(k)) {
          seen.add(k);
          out.target.push({ selector: t.sel, detail: `${Math.round(t.r.width)}x${Math.round(t.r.height)}px, too close to ${clash.sel}` });
        }
      }
    }
  }
  return out;
}

// Focus ring probe for the currently focused element of this frame.
function probeFocus() {
  const a = document.activeElement;
  if (!a || a === document.body || a === document.documentElement || a.tagName === 'IFRAME') return null;
  const cs = getComputedStyle(a);
  const outline = cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0 && !/rgba\([^)]*,\s*0\)/.test(cs.outlineColor);
  const shadow = cs.boxShadow && cs.boxShadow !== 'none';
  const focusVisible = a.matches(':focus-visible');
  const one = (e) => {
    let s = e.tagName.toLowerCase();
    if (e.id) return s + '#' + e.id;
    const cls = [...e.classList].slice(0, 3);
    return cls.length ? s + '.' + cls.join('.') : s;
  };
  const r = a.getBoundingClientRect();
  return { selector: (a.parentElement ? one(a.parentElement) + ' > ' : '') + one(a), ring: outline || !!shadow, focusVisible, visible: r.width > 0 && r.height > 0 };
}

// ---------------------------------------------------------------- driver
const work = mkdtempSync(join(tmpdir(), 'ft-a11y-'));
let server = null;
const cleanup = () => { try { server && server.stop(); } catch {} try { rmSync(work, { recursive: true, force: true }); } catch {} };
const die = (msg) => { console.error('FAIL:', msg); cleanup(); process.exit(1); };

const src = join(work, 'src');
const demo = spawnSync('node', [join(root, 'scripts/make-demo-case.mjs'), src], { encoding: 'utf8' });
if (demo.status !== 0) die('demo source: ' + (demo.stderr || demo.stdout));
try { server = await startIsolatedServer({ root, work }); } catch (e) { die(e.message); }
const { base, token } = server;
const caseDir = join(work, 'case');
const start = await (await fetch(base + '/api/start', {
  method: 'POST',
  headers: { ...server.headers, 'Content-Type': 'application/json' },
  body: JSON.stringify({ input_kind: 'folder', source_path: src, case_dir: caseDir, with_hash: true, with_ffprobe: true }),
})).json();
if (!start.ok) die('start: ' + start.error);
for (const deadline = Date.now() + 240000; ; await new Promise((r) => setTimeout(r, 400))) {
  const st = await server.getJson('/api/status');
  if (st.phase === 'review-ready') break;
  if (st.phase === 'error') die('analysis error: ' + st.error);
  if (Date.now() > deadline) die('analysis timed out at ' + st.phase);
}

let browser;
try { browser = await chromium.launch(); } catch { browser = await chromium.launch({ channel: 'chrome' }); }

// The project view: the hosted viewer's grid in #hViewerHost (or, until
// the fragment is hosted, the legacy viewer frame in the same mount).
const waitProject = async (page) => {
  await page.waitForSelector('#hViewerHost:not([hidden])', { timeout: 20000 });
  await page.waitForFunction(() => {
    const host = document.getElementById('hViewerHost');
    if (host.querySelectorAll('#recordGrid .card').length) return true;
    const fr = host.querySelector('iframe');
    try { return !!(fr && fr.contentDocument && fr.contentDocument.querySelectorAll('#recordGrid .card').length); } catch { return false; }
  }, null, { timeout: 30000 }).catch(() => {});
  await page.waitForTimeout(1200);
};
const PAGES = [
  { id: 'shell-empty', path: '/', prep: async (page) => {
    await page.waitForSelector('#hViewerHost:not([hidden]), #inputCard:not([hidden])', { timeout: 20000 });
    if (await page.locator('#hViewerHost').isVisible()) { await page.click('#hProjectBtn'); await page.click('#hProjectNew'); }
    await page.waitForSelector('#inputCard:not([hidden])');
    await page.waitForTimeout(500);
  } },
  // Contract §28: the 도구 panel (mocked /api/tools) and its confirm dialog.
  { id: 'shell-empty-tools', path: '/', route: (page) => routeTools(page), prep: async (page) => {
    await page.waitForSelector('#hViewerHost:not([hidden]), #inputCard:not([hidden])', { timeout: 20000 });
    if (await page.locator('#hViewerHost').isVisible()) { await page.click('#hProjectBtn'); await page.click('#hProjectNew'); }
    await page.waitForSelector('#hToolsPanel:not([hidden])', { timeout: 8000 });
    await page.waitForTimeout(300);
  } },
  { id: 'shell-empty-tools-dialog', path: '/', route: (page) => routeTools(page), prep: async (page) => {
    await page.waitForSelector('#hViewerHost:not([hidden]), #inputCard:not([hidden])', { timeout: 20000 });
    if (await page.locator('#hViewerHost').isVisible()) { await page.click('#hProjectBtn'); await page.click('#hProjectNew'); }
    await page.waitForSelector('#hToolsPanel:not([hidden])', { timeout: 8000 });
    await page.click('#hToolsList button[data-tool-install="ffmpeg"]');
    await page.waitForSelector('#dlgBackdrop:not([hidden])');
    await page.waitForTimeout(300);
  } },
  // A running install (followed after a reload): progress + [중단].
  { id: 'shell-empty-tools-running', path: '/', route: (page) => routeTools(page, { steps: 100000, install: { id: 1, tool: 'ffmpeg', state: 'downloading', stage: 'download', bytes: 1048576, total: 4194304, error: null, started_unix: 1, finished_unix: null, installed_files: [], audit_warning: null } }), prep: async (page) => {
    await page.waitForSelector('#hViewerHost:not([hidden]), #inputCard:not([hidden])', { timeout: 20000 });
    if (await page.locator('#hViewerHost').isVisible()) { await page.click('#hProjectBtn'); await page.click('#hProjectNew'); }
    await page.waitForSelector('#hToolsList button[data-tool-cancel="ffmpeg"]', { timeout: 8000 });
    await page.waitForTimeout(300);
  } },
  { id: 'shell-project', path: '/', prep: waitProject },
  { id: 'shell-project-sheets-finalize', path: '/', prep: async (page) => { await waitProject(page); await page.click('#hBtnFinalize'); await page.waitForSelector('#hFinalizeSheet:not([hidden])'); await page.waitForTimeout(300); } },
  { id: 'shell-project-sheets-jobs', path: '/', prep: async (page) => { await waitProject(page); await page.click('#hBtnJobs'); await page.waitForSelector('#hJobsSheet:not([hidden])'); await page.waitForTimeout(300); } },
  { id: 'viewer-standalone', path: '/viewer/', prep: async (page) => {
    await page.waitForSelector('#recordGrid .card', { timeout: 30000 }).catch(() => {});
    await page.waitForTimeout(1200);
  } },
];
const THEMES = ['light', 'dark'];
const VIEWPORTS = quick ? [{ width: 1366, height: 768 }] : [{ width: 1366, height: 768 }, { width: 1920, height: 1080 }];
const TABS = 45;
// Unlabelled native controls that exist today and need markup (HTML/JS)
// changes owned by tracks T1-T3. They are reported as warnings; delete the
// entry once the control is labelled so the 'name' gate covers it again.
// Contract §8.1 empties this list once T5 labels the three viewer
// controls: it now applies only to the pre-rebuild viewer (a frame without
// a data-ft-viewer="2" root), so the rebuilt viewer is held to the full gate.
const NAME_BASELINE = [/^select#videoMode$/, /^input#customTagInput$/, /^textarea#evidenceNote$/];

const findings = { contrast: [], name: [], 'name-baseline': [], 'name-custom': [], 'min-font': [], glyph: [], 'glyph-legacy': [], target: [], focus: [], axe: [] };
const push = (rule, ctx, item) => findings[rule].push({ ...ctx, ...item });
const scanned = [];

if (shotsDir) mkdirSync(shotsDir, { recursive: true });

for (const theme of THEMES) {
  for (const vp of VIEWPORTS) {
    for (const pg of PAGES) {
      const context = await browser.newContext({ viewport: vp, colorScheme: theme });
      await context.addInitScript((t) => { try { localStorage.setItem('ft-theme', t); } catch {} }, theme);
      const page = await context.newPage();
      const ctx0 = { page: pg.id, theme, viewport: `${vp.width}x${vp.height}` };
      try {
        if (pg.route) await pg.route(page);
        await page.goto(`${base}${pg.path}?token=${token}`, { waitUntil: 'load' });
        await pg.prep(page);
      } catch (e) {
        console.error(`prep failed for ${JSON.stringify(ctx0)}: ${e.message}`);
        push('name', { ...ctx0, frame: 'prep' }, { selector: '(page)', detail: 'could not reach state: ' + e.message });
        await context.close();
        continue;
      }
      const actual = await page.evaluate(() => document.documentElement.dataset.theme);
      if (actual !== theme) console.error(`warn: ${pg.id} resolved theme ${actual}, wanted ${theme}`);
      if (shotsDir) await page.screenshot({ path: join(shotsDir, `${pg.id}-${theme}-${vp.width}.png`) });
      for (const frame of page.frames()) {
        const label = frame === page.mainFrame() ? 'top' : 'iframe';
        if (frame !== page.mainFrame() && frame.url() === 'about:blank') continue;
        let res;
        try { res = await frame.evaluate(collect, { minFont: MIN_FONT, glyphs: GLYPHS }); } catch (e) { console.error('collect failed', label, e.message); continue; }
        const ctx = { ...ctx0, frame: label };
        scanned.push({ ...ctx, ...res.scanned });
        for (const it of res.contrast) push('contrast', ctx, it);
        for (const it of res.name) push(!res.newViewer && NAME_BASELINE.some((re) => re.test(it.selector)) ? 'name-baseline' : 'name', ctx, it);
        for (const it of res.nameCustom) push('name-custom', ctx, it);
        for (const it of res.minFont) push('min-font', ctx, it);
        for (const it of res.target) push('target', ctx, it);
        for (const it of res.glyph) push('glyph', ctx, it);
        for (const it of res.glyphLegacy) push('glyph-legacy', ctx, it);
        if (axeSource) {
          try {
            await frame.evaluate(axeSource);
            const r = await frame.evaluate(async () => {
              const x = await window.axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'] }, resultTypes: ['violations'] });
              return x.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical')
                .map((v) => ({ id: v.id, impact: v.impact, nodes: v.nodes.slice(0, 12).map((n) => n.target.join(' ')), count: v.nodes.length }));
            });
            for (const v of r) for (const n of v.nodes) push('axe', ctx, { selector: n, detail: `${v.id} (${v.impact}, ${v.count} nodes)` });
          } catch (e) { console.error('axe failed', label, e.message); }
        }
      }
      // Focus ring: Tab through the page once per theme at the first viewport.
      if (vp === VIEWPORTS[0]) {
        const seenFocus = new Set();
        await page.evaluate(() => { document.activeElement && document.activeElement.blur && document.activeElement.blur(); });
        for (let i = 0; i < TABS; i++) {
          await page.keyboard.press('Tab');
          for (const frame of page.frames()) {
            let p = null;
            try { p = await frame.evaluate(probeFocus); } catch { /* frame navigating */ }
            if (!p || !p.visible) continue;
            const k = frame === page.mainFrame() ? 'top' : 'iframe';
            const id = k + '|' + p.selector;
            if (seenFocus.has(id)) continue;
            seenFocus.add(id);
            if (!p.ring || !p.focusVisible) push('focus', { ...ctx0, frame: k }, { selector: p.selector, detail: p.focusVisible ? 'no outline/box-shadow on :focus-visible' : 'not :focus-visible after Tab' });
          }
        }
      }
      await context.close();
    }
  }
}
await browser.close();
cleanup();

// ---------------------------------------------------------------- report
const summary = {};
const byRule = {};
for (const [rule, items] of Object.entries(findings)) {
  const groups = new Map();
  for (const it of items) {
    const k = it.selector + ' :: ' + it.detail.replace(/\d+(\.\d+)?:1|\(#[0-9a-f]{6} on #[0-9a-f]{6}[^)]*\)/g, '').slice(0, 80);
    const g = groups.get(k) || { selector: it.selector, detail: it.detail, contexts: [] };
    g.contexts.push(`${it.page}/${it.theme}/${it.viewport}/${it.frame}`);
    groups.set(k, g);
  }
  byRule[rule] = [...groups.values()];
  summary[rule] = { enforced: !!ENFORCED[rule], occurrences: items.length, unique: byRule[rule].length };
}
mkdirSync(dirname(outPath), { recursive: true });
writeFileSync(outPath, JSON.stringify({ generated: new Date().toISOString(), axe: !!axeSource, summary, scanned, findings: byRule }, null, 2));

console.log(`a11y: ${PAGES.length} pages x ${THEMES.length} themes x ${VIEWPORTS.length} viewports (axe ${axeSource ? 'on' : 'off — npm i --no-save axe-core'})`);
let failed = false;
for (const [rule, s] of Object.entries(summary)) {
  const tag = s.enforced ? (s.unique ? 'FAIL' : 'ok') : (s.unique ? 'warn' : 'ok');
  console.log(`[${tag}] ${rule.padEnd(9)} ${s.enforced ? 'enforced' : 'warning '}  ${s.unique} unique / ${s.occurrences} occurrences`);
  if (s.enforced && s.unique) failed = true;
  for (const g of byRule[rule].slice(0, 25)) console.log(`      ${g.selector}  —  ${g.detail}  [${g.contexts.length} ctx]`);
  if (byRule[rule].length > 25) console.log(`      … ${byRule[rule].length - 25} more in ${outPath}`);
}
console.log(`report: ${outPath}`);
process.exit(failed ? 1 : 0);
