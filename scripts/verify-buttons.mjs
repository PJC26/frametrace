// Button-system gate (docs/ui-rebuild-contract.md section 5.4 + 8.2).
//
//   cargo build --locked
//   NODE_PATH=<repo>/node_modules node scripts/verify-buttons.mjs [--no-live]
//
// Two layers, both in light AND dark:
//
//  A. KIT FIXTURE (always enforced). A synthetic page built from the exact
//     bundled assets/tokens.css + assets/kit.css + assets/icons.svg holds one
//     of every button kind (secondary, primary, quiet, destructive,
//     primary.destructive, quiet.destructive, lg, icon, toggle, busy) on both
//     page paper (--bg) and card (--surface). For each: height >= 24, primary
//     and .lg >= 36, .icon 28x28; five states (default / hover / active /
//     focus-visible / disabled) each differ from default in background,
//     outline or text colour; text contrast >= 4.5 (disabled >= 3); the focus
//     ring is >= 2px and >= 3:1. Every icon in the sprite renders inside its
//     16x16 box.
//  B. LIVE PAGES (shell empty state, shell with a case, standalone viewer) at
//     1366x768 and 1920x1080 against an isolated server: class vocabulary,
//     sizes, one visible .primary per [data-region], no unicode glyphs in
//     button text, icon-only buttons carry aria-label, per-state checks on a
//     sample of buttons, top strip / selection bar / sheet buttons inside the
//     viewport and not covered. These findings FAIL the run on the rebuilt UI
//     (detected by <html data-ft-host>, or
//     FT_BUTTONS_STRICT=1). On the pre-rebuild UI they are printed as
//     warnings so the transition does not break CI.
//  C. BUNDLES. assets/kit.css is the second CSS part of both bundles and the
//     served pages carry it exactly once.
import { mkdtempSync, rmSync, readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { startIsolatedServer } from './lib/isolated-server.mjs';
import { appBundleCss, viewerBundleCss } from './lib/app-bundle.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const argv = process.argv.slice(2);
const noLive = argv.includes('--no-live');
const forceStrict = process.env.FT_BUTTONS_STRICT === '1';
const require = createRequire(import.meta.url);
let chromium;
try { ({ chromium } = require('playwright')); } catch {
  console.error('playwright not resolvable — set NODE_PATH to a node_modules with playwright');
  process.exit(2);
}

const failures = [];
const warnings = [];
const fail = (where, msg) => failures.push(`${where}: ${msg}`);
const warn = (where, msg) => warnings.push(`${where}: ${msg}`);
let checks = 0;

// ------------------------------------------------------------------ helpers
const KIT_CLASSES = new Set(['primary', 'quiet', 'destructive', 'lg', 'icon', 'toggle']);
// Old vocabulary and the component classes the contract forbids on buttons.
const FORBIDDEN_CLASSES = new Set(['ghost', 'hbtn', 'mini', 'btn-sm', 'mi', 'linkbtn', 'icon-btn', 'cal-btn', 'thumb-retry', 'toast-x', 'danger', 'danger-fill', 'big', 'chip', 'pill', 'navcell', 'cal-cell', 'tag-del', 'save-state']);
const GLYPHS = /[◐☀⏻⤢⧉▾↗✓✕☰⚠⤓▸]/;
const REQUIRED_ICONS = ['check', 'flag', 'question', 'x', 'warning', 'info', 'menu', 'chevron-down', 'chevron-right', 'chevron-up', 'external', 'theme', 'power', 'popout', 'fullscreen', 'download', 'camera', 'clip', 'court', 'proxy', 'dual', 'pip', 'theater', 'skip-back', 'skip-fwd', 'step-back', 'step-fwd', 'in', 'out', 'play', 'pause', 'search', 'filter', 'folder', 'file-image', 'project', 'activity', 'report', 'tools', 'help', 'close', 'undo', 'redo', 'tag', 'save', 'refresh', 'copy', 'calendar', 'lock', 'spinner'];

// In-page: composited colours + metrics for one element (self-contained).
function snapshotFn(el) {
  const parse = (s) => {
    let m = /^rgba?\(([^)]+)\)$/.exec(s);
    if (m) { const p = m[1].split(/[ ,\/]+/).filter(Boolean).map(Number); return { r: p[0], g: p[1], b: p[2], a: p.length > 3 ? p[3] : 1 }; }
    m = /^color\(srgb ([^)]+)\)$/.exec(s);
    if (m) { const p = m[1].split(/[ \/]+/).filter(Boolean).map(Number); return { r: p[0] * 255, g: p[1] * 255, b: p[2] * 255, a: p.length > 3 ? p[3] : 1 }; }
    return { r: 0, g: 0, b: 0, a: 0 };
  };
  const over = (t, b) => {
    const a = t.a + b.a * (1 - t.a);
    if (a === 0) return { r: 0, g: 0, b: 0, a: 0 };
    const k = (x, y) => (x * t.a + y * b.a * (1 - t.a)) / a;
    return { r: k(t.r, b.r), g: k(t.g, b.g), b: k(t.b, b.b), a };
  };
  const lum = (c) => { const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }; return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b); };
  const ratio = (a, b) => { const x = lum(a), y = lum(b); return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05); };
  const hex = (c) => '#' + [c.r, c.g, c.b].map((v) => Math.round(v).toString(16).padStart(2, '0')).join('');
  const chain = [];
  for (let e = el.parentElement; e && e.nodeType === 1; e = e.parentElement) chain.push(e);
  let under = { r: 255, g: 255, b: 255, a: 1 };
  for (let i = chain.length - 1; i >= 0; i--) { const c = parse(getComputedStyle(chain[i]).backgroundColor); if (c.a > 0) under = over(c, under); }
  const cs = getComputedStyle(el);
  const bg = over(parse(cs.backgroundColor), under);
  const fg = over(parse(cs.color), bg);
  const border = over(parse(cs.borderTopColor), under);
  const outline = over(parse(cs.outlineColor), under);
  const r = el.getBoundingClientRect();
  return {
    bg: hex(bg), fg: hex(fg), border: hex(border),
    borderW: parseFloat(cs.borderTopWidth), outlineStyle: cs.outlineStyle, outlineW: parseFloat(cs.outlineWidth), outline: hex(outline),
    transform: cs.transform, textRatio: ratio(fg, bg), outlineRatio: ratio(outline, under),
    focusVisible: el.matches(':focus-visible'), w: r.width, h: r.height,
  };
}

const sig = (s) => `${s.bg}|${s.border}|${s.fg}`;

// Drives one element through the five states. `ctrl` = { page, handle, label, allowDisabledToggle }.
async function probeStates(page, handle, label, where, report, { toggleDisabled }) {
  const snap = () => handle.evaluate(snapshotFn);
  await handle.scrollIntoViewIfNeeded().catch(() => {});
  await handle.evaluate((e) => e.blur && e.blur());
  await page.mouse.move(0, 0);
  await page.waitForTimeout(30);
  const def = await snap();
  await handle.hover();
  await page.waitForTimeout(40);
  const hov = await snap();
  await page.mouse.down();
  await page.waitForTimeout(40);
  const act = await snap();
  await page.mouse.move(0, 0); // leave before release: no click fires on live pages
  await page.mouse.up();
  await handle.evaluate((e) => e.blur && e.blur());
  await page.keyboard.press('Shift');
  await handle.focus();
  await page.waitForTimeout(40);
  const foc = await snap();
  await handle.evaluate((e) => e.blur && e.blur());
  const states = { default: def, hover: hov, active: act, focus: foc };
  let dis = null;
  if (toggleDisabled) {
    await handle.evaluate((e) => { e.disabled = true; });
    await page.waitForTimeout(40);
    dis = await snap();
    await handle.evaluate((e) => { e.disabled = false; });
    states.disabled = dis;
  }
  checks++;
  for (const k of ['hover', 'active']) {
    if (sig(states[k]) === sig(def) && states[k].transform === def.transform) report(`${label} ${k} is indistinguishable from default (${sig(def)})`);
  }
  if (dis && sig(dis) === sig(def)) report(`${label} disabled is indistinguishable from default (${sig(def)})`);
  if (!(foc.focusVisible && foc.outlineStyle !== 'none' && foc.outlineW >= 2)) report(`${label} focus-visible ring missing (focusVisible=${foc.focusVisible} outline=${foc.outlineStyle} ${foc.outlineW}px)`);
  else if (foc.outlineRatio < 3) report(`${label} focus ring ${foc.outline} only ${foc.outlineRatio.toFixed(2)}:1 vs surroundings`);
  for (const [k, s] of Object.entries(states)) {
    const need = k === 'disabled' ? 3 : 4.5;
    if (s.textRatio + 1e-9 < need) report(`${label} ${k} text ${s.fg} on ${s.bg} = ${s.textRatio.toFixed(2)}:1 < ${need}:1`);
  }
  return def;
}

// ------------------------------------------------------------------ C. bundles
{
  const banner = (css, n) => css.split('\n').filter((l) => l.startsWith('/* ==== '))[n] ?? '';
  for (const [name, css] of [['shell', appBundleCss(root)], ['viewer', viewerBundleCss(root)]]) {
    checks++;
    if (!banner(css, 0).includes('assets/tokens.css') || !banner(css, 1).includes('assets/kit.css')) fail('bundle', `${name} bundle must be tokens.css then kit.css`);
    if ((css.match(/\/\* ==== assets\/kit\.css ==== \*\//g) ?? []).length !== 1) fail('bundle', `${name} bundle must include kit.css exactly once`);
  }
}

// ------------------------------------------------------------------ browser
let browser;
try { browser = await chromium.launch(); } catch { browser = await chromium.launch({ channel: 'chrome' }); }

// ------------------------------------------------------------------ A. fixture
const tokensCss = readFileSync(join(root, 'assets/tokens.css'), 'utf8');
const kitCss = readFileSync(join(root, 'assets/kit.css'), 'utf8');
const sprite = readFileSync(join(root, 'assets/icons.svg'), 'utf8');

{
  const ids = [...sprite.matchAll(/<symbol id="i-([a-z0-9-]+)"([^>]*)>/g)];
  const have = new Set(ids.map((m) => m[1]));
  checks++;
  for (const id of REQUIRED_ICONS) if (!have.has(id)) fail('icons', `missing symbol i-${id}`);
  for (const m of ids) if (!/viewBox="0 0 16 16"/.test(m[2])) fail('icons', `i-${m[1]} must use viewBox="0 0 16 16"`);
  if (/https?:\/\//.test(sprite.replace(/xmlns="[^"]*"/, ''))) fail('icons', 'sprite must not reference remote resources');
  if ((sprite.match(/<svg\b/g) ?? []).length !== 1) fail('icons', 'sprite must be a single <svg> element');
}

const KINDS = [
  ['secondary', '<button>Secondary</button>'],
  ['primary', '<button class="primary">Primary</button>'],
  ['quiet', '<button class="quiet">Quiet</button>'],
  ['destructive', '<button class="destructive">Destructive</button>'],
  ['primary.destructive', '<button class="primary destructive">Delete</button>'],
  ['quiet.destructive', '<button class="quiet destructive">Remove</button>'],
  ['lg', '<button class="lg">Large</button>'],
  ['icon', '<button class="icon" aria-label="Close"><svg class="ic" aria-hidden="true"><use href="#i-close"/></svg></button>'],
  ['quiet.icon', '<button class="quiet icon" aria-label="Menu"><svg class="ic" aria-hidden="true"><use href="#i-menu"/></svg></button>'],
  ['text+icon', '<button><svg class="ic" aria-hidden="true"><use href="#i-download"/></svg>Download</button>'],
  ['toggle(pressed)', '<button class="toggle" aria-pressed="true">Pressed</button>'],
  ['toggle(off)', '<button class="toggle" aria-pressed="false">Off</button>'],
];

for (const theme of ['light', 'dark']) {
  const ctx = await browser.newContext({ viewport: { width: 900, height: 600 }, colorScheme: theme });
  const page = await ctx.newPage();
  const html = `<!doctype html><html lang="en" data-theme="${theme}"><head><meta charset="utf-8"><style>${tokensCss}\n${kitCss}\n*{transition:none!important}body{margin:0;padding:12px;background:var(--bg);color:var(--text)}.card{background:var(--surface);padding:12px;margin:8px 0;display:flex;flex-wrap:wrap;align-items:flex-start;gap:8px}</style></head><body>${sprite}
<div id="onbg" class="card" style="background:transparent" data-region="onbg">${KINDS.map(([, h]) => h).join('')}</div>
<div id="oncard" class="card" data-region="oncard">${KINDS.map(([, h]) => h).join('')}</div>
<div class="card"><button id="busy" class="primary" aria-busy="true">Working</button><button id="busy2" aria-busy="true">Working</button>
<label>Name <input id="t" type="text" value="abc"></label><select id="s"><option>one</option></select><input id="d" type="date"><kbd id="k">J</kbd></div>
<div id="icons" class="card">${REQUIRED_ICONS.map((i) => `<svg class="ic" aria-hidden="true" data-i="${i}"><use href="#i-${i}"/></svg>`).join('')}</div>
</body></html>`;
  await page.setContent(html);
  const where = `fixture/${theme}`;

  // sizes
  for (const region of ['onbg', 'oncard']) {
    const hs = await page.$$(`#${region} > button`);
    for (let i = 0; i < hs.length; i++) {
      const [kind] = KINDS[i];
      const s = await hs[i].evaluate(snapshotFn);
      checks++;
      const label = `${region} ${kind}`;
      if (s.h < 24 - 0.01) fail(where, `${label} height ${s.h}px < 24`);
      if (/^(primary|lg)/.test(kind) && s.h < 36 - 0.01) fail(where, `${label} height ${s.h}px < 36`);
      if (/(^|\.)icon$/.test(kind) && (Math.abs(s.w - 28) > 0.01 || Math.abs(s.h - 28) > 0.01)) fail(where, `${label} must be 28x28 (got ${s.w}x${s.h})`);
      if (kind === 'secondary' && Math.abs(s.h - 32) > 0.01) fail(where, `${label} default must be 32px (got ${s.h})`);
    }
  }
  // states on both backgrounds
  for (const region of ['onbg', 'oncard']) {
    const hs = await page.$$(`#${region} > button`);
    for (let i = 0; i < hs.length; i++) {
      await probeStates(page, hs[i], `${region} ${KINDS[i][0]}`, where, (m) => fail(where, m), { toggleDisabled: true });
    }
  }
  // pressed toggle differs from the unpressed one
  {
    const on = await (await page.$('#oncard > button[aria-pressed="true"]')).evaluate(snapshotFn);
    const off = await (await page.$('#oncard > button[aria-pressed="false"]')).evaluate(snapshotFn);
    checks++;
    if (sig(on) === sig(off)) fail(where, 'toggle[aria-pressed=true] is indistinguishable from unpressed');
  }
  // busy: spinner pseudo-element + progress cursor + readable
  for (const id of ['busy', 'busy2']) {
    const r = await page.$eval('#' + id, (e) => {
      const b = getComputedStyle(e, '::before');
      return { cursor: getComputedStyle(e).cursor, content: b.content, w: b.width };
    });
    checks++;
    if (r.cursor !== 'progress' || r.content === 'none' || r.content === 'normal') fail(where, `#${id} aria-busy must show a spinner and cursor:progress (${JSON.stringify(r)})`);
    const s = await (await page.$('#' + id)).evaluate(snapshotFn);
    if (s.textRatio < 4.5) fail(where, `#${id} busy text ${s.textRatio.toFixed(2)}:1 < 4.5`);
  }
  // inputs: >= 24 high (32), focus ring, contrast, border >= 3:1
  for (const id of ['t', 's', 'd']) {
    const h = await page.$('#' + id);
    const s = await h.evaluate(snapshotFn);
    checks++;
    if (Math.abs(s.h - 32) > 0.01) fail(where, `#${id} input height ${s.h} != 32`);
    if (s.textRatio < 4.5) fail(where, `#${id} text ${s.textRatio.toFixed(2)}:1 < 4.5`);
    await page.keyboard.press('Shift');
    await h.focus();
    const f = await h.evaluate(snapshotFn);
    if (!(f.outlineStyle !== 'none' && f.outlineW >= 2)) fail(where, `#${id} focus-visible ring missing`);
  }
  // kbd 18px, readable
  {
    const s = await (await page.$('#k')).evaluate(snapshotFn);
    checks++;
    if (Math.abs(s.h - 18) > 0.01) fail(where, `kbd height ${s.h} != 18`);
    if (s.textRatio < 4.5) fail(where, `kbd text ${s.textRatio.toFixed(2)}:1 < 4.5`);
  }
  // every icon renders within its 16x16 box and inherits currentColor
  {
    const bad = await page.$$eval('#icons svg.ic', (svgs) => svgs.map((s) => {
      const u = s.querySelector('use');
      const bb = u.getBBox();
      const st = getComputedStyle(s);
      return { id: s.dataset.i, w: bb.width, h: bb.height, x: bb.x, y: bb.y, size: s.getBoundingClientRect().width, stroke: st.stroke, fill: st.fill };
    }).filter((r) => !(r.w > 1 && r.h > 1 && r.x >= -0.5 && r.y >= -0.5 && r.x + r.w <= 16.5 && r.y + r.h <= 16.5) || r.size !== 16 || r.stroke === 'none'));
    checks++;
    for (const b of bad) fail(where, `icon i-${b.id} renders wrongly (${JSON.stringify(b)})`);
  }
  await ctx.close();
}

// ------------------------------------------------------------------ B. live pages
if (!noLive) {
  const work = mkdtempSync(join(tmpdir(), 'ft-buttons-'));
  let server = null;
  const cleanup = () => { try { server && server.stop(); } catch {} try { rmSync(work, { recursive: true, force: true }); } catch {} };
  const die = (msg) => { console.error('FAIL:', msg); cleanup(); process.exit(1); };
  const src = join(work, 'src');
  const demo = spawnSync('node', [join(root, 'scripts/make-demo-case.mjs'), src], { encoding: 'utf8' });
  if (demo.status !== 0) die('demo source: ' + (demo.stderr || demo.stdout));
  try { server = await startIsolatedServer({ root, work }); } catch (e) { die(e.message); }
  const { base, token } = server;

  // Page-side scan of one frame. Returns raw findings; the driver decides severity.
  const scanFrame = (opts) => {
    const out = { classes: [], sizes: [], regions: [], glyphs: [], names: [], viewport: [], strict: false };
    const visible = (el) => {
      for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
        const cs = getComputedStyle(e);
        if (cs.display === 'none' || cs.visibility === 'hidden' || e.hidden) return false;
      }
      const r = el.getBoundingClientRect();
      return r.width > 1 && r.height > 1;
    };
    const one = (e) => {
      let s = e.tagName.toLowerCase();
      if (e.id) return s + '#' + e.id;
      const cls = [...e.classList].slice(0, 3);
      return cls.length ? s + '.' + cls.join('.') : s;
    };
    const name = (e) => (e.parentElement ? one(e.parentElement) + ' > ' : '') + one(e);
    // Strict once the rebuilt integrated page (T2) is live. The viewer
    // fragment alone (T5, data-ft-viewer="2") appears before its glyph →
    // icon migration lands, so it must not flip the whole page to strict.
    out.strict = document.documentElement.hasAttribute('data-ft-host');
    for (const b of document.querySelectorAll('button')) {
      if (!visible(b)) continue;
      const cls = [...b.classList];
      const r = b.getBoundingClientRect();
      for (const c of cls) if (opts.forbidden.includes(c)) out.classes.push(`${name(b)} uses forbidden class "${c}"`);
      if (r.height < 24 - 0.01 || r.width < 24 - 0.01) out.sizes.push(`${name(b)} is ${Math.round(r.width)}x${Math.round(r.height)} (< 24)`);
      if (b.classList.contains('primary') && r.height < 36 - 0.01) out.sizes.push(`${name(b)} .primary is ${Math.round(r.height)}px (< 36)`);
      if (b.classList.contains('icon') && (Math.abs(r.width - 28) > 0.5 || Math.abs(r.height - 28) > 0.5)) out.sizes.push(`${name(b)} .icon is ${Math.round(r.width)}x${Math.round(r.height)} (!= 28)`);
      const text = (b.textContent || '').trim();
      if (opts.glyphs.test(text)) out.glyphs.push(`${name(b)} text "${text.slice(0, 20)}" contains a unicode glyph (use an i-* icon)`);
      if (!text && !b.getAttribute('aria-label') && !b.getAttribute('aria-labelledby')) out.names.push(`${name(b)} is icon-only without aria-label`);
    }
    for (const reg of document.querySelectorAll('[data-region]')) {
      if (!visible(reg)) continue;
      const prim = [...reg.querySelectorAll('button.primary')].filter(visible);
      const id = reg.getAttribute('data-region');
      if (prim.length > 1) out.regions.push(`[data-region=${id}] has ${prim.length} visible .primary buttons (max 1)`);
      if (['selectionBar', 'hJobsSheet', 'hFinalizeSheet'].includes(id) && prim.length !== 1) out.regions.push(`[data-region=${id}] must have exactly 1 .primary (has ${prim.length})`);
    }
    const vw = document.documentElement.clientWidth, vh = document.documentElement.clientHeight;
    const guarded = opts.zones.flatMap((s) => [...document.querySelectorAll(s)]);
    for (const b of new Set(guarded.flatMap((z) => (z.tagName === 'BUTTON' ? [z] : [...z.querySelectorAll('button')])))) {
      if (!visible(b)) continue;
      const r = b.getBoundingClientRect();
      if (r.left < -0.5 || r.top < -0.5 || r.right > vw + 0.5 || r.bottom > vh + 0.5) { out.viewport.push(`${name(b)} extends outside the viewport (${Math.round(r.left)},${Math.round(r.top)} ${Math.round(r.right)},${Math.round(r.bottom)} in ${vw}x${vh})`); continue; }
      const t = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      if (!t || !(t === b || b.contains(t))) out.viewport.push(`${name(b)} is covered by ${t ? name(t) : 'nothing'}`);
    }
    return out;
  };
  const scanOpts = { forbidden: [...FORBIDDEN_CLASSES], glyphs: GLYPHS, zones: ['#hTopStrip', '#stageBar', '#selectionBar', '.h-sheet:not([hidden])'] };

  async function sampleHandles(frame) {
    // One visible, enabled, non-destructive button per class signature.
    return frame.evaluateHandle(() => {
      const seen = new Set();
      const pick = [];
      const vis = (el) => {
        for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
          const cs = getComputedStyle(e);
          if (cs.display === 'none' || cs.visibility === 'hidden' || e.hidden) return false;
        }
        const r = el.getBoundingClientRect();
        return r.width > 1 && r.height > 1 && r.top >= 0 && r.bottom <= innerHeight && r.left >= 0 && r.right <= innerWidth;
      };
      for (const b of document.querySelectorAll('button:not(:disabled)')) {
        if (!vis(b) || /quit|exit|delete|remove|power/i.test(b.id + ' ' + (b.getAttribute('aria-label') || ''))) continue;
        const key = [...b.classList].sort().join('.') + '|' + (b.closest('[data-region]')?.getAttribute('data-region') ?? '');
        if (seen.has(key)) continue;
        seen.add(key);
        pick.push(b);
        if (pick.length >= 8) break;
      }
      return pick;
    });
  }

  const PAGES = [
    { id: 'shell-empty', path: '/', needsCase: false, prep: async (page) => { await page.waitForSelector('#inputCard, .navstep[data-view="1"]', { timeout: 15000 }).catch(() => {}); await page.waitForTimeout(400); } },
    { id: 'shell-project', path: '/', needsCase: true, prep: async (page) => {
      const step = await page.$('.navstep[data-view="3"]');
      if (step) { await page.waitForSelector('.navstep[data-view="3"]:not([disabled])', { timeout: 20000 }).catch(() => {}); await step.click().catch(() => {}); }
      await page.waitForFunction(() => [document, ...[...document.querySelectorAll('iframe')].map((f) => f.contentDocument)].some((d) => d && d.querySelectorAll('#recordGrid .card').length > 0), null, { timeout: 30000 }).catch(() => {});
      await page.waitForTimeout(800);
    } },
    { id: 'viewer-standalone', path: '/viewer/', needsCase: true, prep: async (page) => { await page.waitForSelector('#recordGrid .card', { timeout: 30000 }).catch(() => {}); await page.waitForTimeout(800); } },
  ];

  let caseStarted = false;
  async function startCase() {
    if (caseStarted) return;
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
    caseStarted = true;
  }

  // served pages carry kit.css exactly once (bundle check on what users get)
  for (const p of ['/', '/viewer/']) {
    const text = await (await fetch(`${base}${p}?token=${token}`, { headers: server.headers })).text();
    checks++;
    const n = (text.match(/\/\* ==== assets\/kit\.css ==== \*\//g) ?? []).length;
    if (n !== 1) fail('bundle', `GET ${p} carries assets/kit.css ${n}x (expected exactly 1)`);
  }

  const seenStrict = new Set();
  for (const pg of PAGES) {
    if (pg.needsCase) await startCase();
    for (const theme of ['light', 'dark']) {
      for (const vp of [{ width: 1366, height: 768 }, { width: 1920, height: 1080 }]) {
        const context = await browser.newContext({ viewport: vp, colorScheme: theme });
        await context.addInitScript((t) => { try { localStorage.setItem('ft-theme', t); } catch {} }, theme);
        const page = await context.newPage();
        const where = `${pg.id}/${theme}/${vp.width}x${vp.height}`;
        try {
          await page.goto(`${base}${pg.path}?token=${token}`, { waitUntil: 'load' });
          await pg.prep(page);
        } catch (e) { fail(where, 'could not reach state: ' + e.message); await context.close(); continue; }
        let strictPage = forceStrict;
        const results = [];
        for (const frame of page.frames()) {
          if (frame !== page.mainFrame() && frame.url() === 'about:blank') continue;
          let res;
          try { res = await frame.evaluate(scanFrame, scanOpts); } catch { continue; }
          if (res.strict) strictPage = true;
          results.push({ frame, res });
        }
        if (strictPage) seenStrict.add(pg.id);
        const report = (m) => (strictPage ? fail(where, m) : warn(where, m));
        checks++;
        for (const { res } of results) for (const k of ['classes', 'sizes', 'regions', 'glyphs', 'names', 'viewport']) for (const m of res[k]) report(m);
        // sampled state checks (hover/active/focus) on one button per class signature
        if (vp.width === 1366) {
          for (const { frame } of results) {
            const arr = await sampleHandles(frame);
            const props = await arr.getProperties();
            for (const h of props.values()) {
              const el = h.asElement();
              if (!el) continue;
              const label = await el.evaluate((e) => e.tagName.toLowerCase() + (e.id ? '#' + e.id : e.className ? '.' + String(e.className).split(' ').join('.') : '')).catch(() => 'button');
              await probeStates(page, el, label, where, report, { toggleDisabled: false }).catch((e) => warn(where, `state probe skipped for ${label}: ${e.message.split('\n')[0]}`));
            }
          }
        }
        await context.close();
      }
    }
  }
  console.log(`live pages enforced (rebuilt UI detected): ${[...seenStrict].join(', ') || 'none (pre-rebuild UI: findings are warnings)'}`);
  cleanup();
}
await browser.close();

// ------------------------------------------------------------------ report
const uniq = (a) => [...new Set(a)];
if (warnings.length) {
  const w = uniq(warnings.map((m) => m.replace(/^[^:]+: /, '')));
  console.log(`warnings (pre-rebuild UI, not enforced yet): ${warnings.length} findings, ${w.length} distinct`);
  for (const m of w.slice(0, 12)) console.log('  warn ' + m);
  if (w.length > 12) console.log(`  ... ${w.length - 12} more`);
}
if (failures.length) {
  const f = uniq(failures);
  console.error(`verify-buttons FAIL: ${f.length} violation(s)`);
  for (const m of f.slice(0, 60)) console.error('  FAIL ' + m);
  if (f.length > 60) console.error(`  ... ${f.length - 60} more`);
  process.exit(1);
}
console.log(`verify-buttons ok: ${checks} checks (kit fixture x2 themes enforced${noLive ? '' : '; live pages x2 themes x2 viewports'})`);
