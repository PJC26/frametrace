// STR-01: the workstation shell is authored as assets/examiner_app.html
// (markup with <!-- @bundle:app.css --> / <!-- @bundle:app.js --> markers)
// plus assets/app/*.css|*.js. build.rs inlines them into one page; these
// helpers reproduce every build.rs page exactly for node harnesses:
//   appPageHtml()        GET /            (shell; hosts the viewer once the
//                                          template carries @include:viewer.body)
//   legacyAppPageHtml()  GET / with FRAMETRACE_UI=legacy
//   viewerTemplateHtml() OUT_DIR/evidence_viewer.html (slots still open)
//   viewerPageHtml()     a filled standalone viewer page (make-review output)
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

const sortedParts = (dir, ext) => readdirSync(dir).filter((f) => f.endsWith('.' + ext)).sort().map((f) => join(dir, f));
export function appParts(ext, root = repoRoot) {
  return sortedParts(join(root, 'assets/app'), ext);
}
export const viewerParts = (root = repoRoot) => sortedParts(join(root, 'assets/viewer'), 'js');

// Same concatenation as build.rs `concat`: per-part banner, newline-
// terminated, sorted, and (js only) a trailing sourceURL.
export function concatParts(files, bundleName, root = repoRoot) {
  let bundle = '';
  for (const f of files) {
    bundle += `/* ==== ${relative(root, f).split(sep).join('/')} ==== */\n`;
    bundle += readFileSync(f, 'utf8');
    if (!bundle.endsWith('\n')) bundle += '\n';
  }
  if (bundleName.endsWith('.js')) bundle += `//# sourceURL=${bundleName}\n`;
  return bundle;
}

export const appBundleJs = (root = repoRoot) => concatParts(appParts('js', root), 'assets/app/bundle.js', root);
// The viewer bundle (00-host.js = window.FTViewer first), as build.rs viewer_js.
export const viewerBundleJs = (root = repoRoot) => concatParts(viewerParts(root), 'assets/viewer/bundle.js', root);
// Shared tokens (assets/tokens.css) are the first CSS part of both pages,
// the shared UI kit (assets/kit.css) the second (docs/ui-rebuild-contract.md §5.4).
export const tokensPath = (root = repoRoot) => join(root, 'assets/tokens.css');
export const kitPath = (root = repoRoot) => join(root, 'assets/kit.css');
// The viewer's own sheets (scoped under .ft-viewer), as build.rs viewer_own_css.
export const viewerOwnCssPaths = (root = repoRoot) => [join(root, 'assets/evidence_viewer.css'), join(root, 'assets/viewer/controls.css')];
export const appMarkup = (root = repoRoot) => readFileSync(join(root, 'assets/examiner_app.html'), 'utf8');

// ---- shared viewer fragment (build.rs viewer_fragment / include_viewer_body)
export const VIEWER_INCLUDE = '<!-- @include:viewer.body -->';
export const viewerFragment = (root = repoRoot) => readFileSync(join(root, 'assets/viewer/viewer.body.html'), 'utf8').trimEnd();
export function includeViewerBody(page, { required = false, root = repoRoot } = {}) {
  const n = page.split(VIEWER_INCLUDE).length - 1;
  if (n === 0 && !required) return { page, hosted: false };
  if (n !== 1) throw new Error(`template must contain ${required ? 'exactly one' : 'at most one'} ${VIEWER_INCLUDE} (found ${n})`);
  return { page: page.replace(VIEWER_INCLUDE, () => viewerFragment(root)), hosted: true };
}

// GET / CSS: tokens + kit + app/*.css (+ the viewer's own sheets once the
// shell hosts the viewer).
export const appBundleCss = (root = repoRoot) => {
  const hosted = appMarkup(root).includes(VIEWER_INCLUDE);
  return concatParts([tokensPath(root), kitPath(root), ...appParts('css', root), ...(hosted ? viewerOwnCssPaths(root) : [])], 'assets/app/bundle.css', root);
};
// The standalone/exported viewer stylesheet: tokens + kit + evidence_viewer.css +
// viewer/controls.css (action bar, player toolbar, menus, toasts).
export const viewerBundleCss = (root = repoRoot) => concatParts([tokensPath(root), kitPath(root), ...viewerOwnCssPaths(root)], 'assets/viewer/bundle.css', root);
// Offline icon sprite, inlined where a template carries the marker (build.rs inline_icons).
export const iconsSprite = (root = repoRoot) => readFileSync(join(root, 'assets/icons.svg'), 'utf8').trimEnd();
export function inlineIcons(page, root = repoRoot) {
  const marker = '<!-- @bundle:icons.svg -->';
  const n = page.split(marker).length - 1;
  if (n > 1) throw new Error(`template must contain at most one ${marker} (found ${n})`);
  return n ? page.replace(marker, () => iconsSprite(root)) : page;
}

function shellPage({ markup, css, js, viewerJs }) {
  const script = viewerJs ? `<script>\n${viewerJs}</script>\n<script>\n${js}</script>` : `<script>\n${js}</script>`;
  return markup
    .replace('<!-- @bundle:app.css -->', () => `<style>\n${css}</style>`)
    .replace('<!-- @bundle:app.js -->', () => script);
}

// The page exactly as GET / serves it.
export function appPageHtml(root = repoRoot) {
  const { page, hosted } = includeViewerBody(appMarkup(root), { root });
  return inlineIcons(shellPage({ markup: page, css: appBundleCss(root), js: appBundleJs(root), viewerJs: hosted ? viewerBundleJs(root) : '' }), root);
}

// GET / under FRAMETRACE_UI=legacy (build.rs legacy_bundle): the 465e7db
// shell snapshot, no kit, its own tokens.
export function legacyAppPageHtml(root = repoRoot) {
  const dir = join(root, 'assets/legacy');
  const markup = readFileSync(join(dir, 'examiner_app.html'), 'utf8');
  const css = concatParts([join(dir, 'tokens.css'), ...sortedParts(join(dir, 'app'), 'css')], 'assets/legacy/app/bundle.css', root);
  const js = concatParts(sortedParts(join(dir, 'app'), 'js'), 'assets/legacy/app/bundle.js', root);
  const { page, hosted } = includeViewerBody(markup, { root });
  return inlineIcons(shellPage({ markup: page, css, js, viewerJs: hosted ? viewerBundleJs(root) : '' }), root);
}

// OUT_DIR/evidence_viewer.html: the standalone template with the fragment
// and the icon sprite inlined; __CSS__ / __DATA__ / __JS__ still open.
export function viewerTemplateHtml(root = repoRoot) {
  const template = readFileSync(join(root, 'assets/evidence_viewer.html'), 'utf8');
  return inlineIcons(includeViewerBody(template, { required: true, root }).page, root);
}

// A filled standalone viewer page (html_report::fill_viewer_template: one
// left-to-right pass, substituted text is never rescanned).
//   data: the __DATA__ script body (default: the slim page's null bundle)
//   js:   the viewer bundle (default: viewerBundleJs())
export function viewerPageHtml({ data = 'window.__FRAMETRACE_DATA__ = null;', js, css, root = repoRoot } = {}) {
  const slots = { __CSS__: css ?? viewerBundleCss(root), __DATA__: data, __JS__: js ?? viewerBundleJs(root) };
  return viewerTemplateHtml(root).replace(/__CSS__|__DATA__|__JS__/g, (token) => slots[token]);
}

// ---- lightweight JS scanner (check-js.mjs, verify-embed.mjs) ---------------
// Walks a classic script skipping comments, strings, template literals and
// regex literals, and calls visit(i, depth) for every code character with
// the current {}/()/[] nesting depth. Good enough for this codebase's style;
// not a general JS parser.
export function scanCode(src, visit) {
  let depth = 0;
  const tmpl = []; // brace depth at which each open template `${` resumes
  let prevSig = ''; // last significant code char (regex vs divide heuristic)
  let prevWord = '';
  const regexAfter = new Set(['', '(', ',', '=', ':', '[', '!', '&', '|', '?', '{', '}', ';', '+', '-', '*', '%', '<', '>', '~', '^']);
  const regexWords = new Set(['return', 'typeof', 'case', 'do', 'else', 'in', 'of', 'new', 'delete', 'void', 'throw', 'instanceof', 'yield', 'await']);
  for (let i = 0; i < src.length; i++) {
    const c = src[i];
    const n = src[i + 1];
    if (c === '/' && n === '/') { while (i < src.length && src[i] !== '\n') i++; continue; }
    if (c === '/' && n === '*') { i = src.indexOf('*/', i + 2) + 1; if (i <= 0) break; continue; }
    if (c === '"' || c === "'") {
      for (i++; i < src.length && src[i] !== c; i++) if (src[i] === '\\') i++;
      prevSig = c; prevWord = ''; continue;
    }
    if (c === '`' || (c === '}' && tmpl.length && tmpl[tmpl.length - 1] === depth)) {
      if (c === '}') tmpl.pop();
      for (i++; i < src.length; i++) {
        if (src[i] === '\\') { i++; continue; }
        if (src[i] === '`') break;
        if (src[i] === '$' && src[i + 1] === '{') { i++; tmpl.push(depth); break; }
      }
      prevSig = '`'; prevWord = ''; continue;
    }
    if (c === '/' && (regexAfter.has(prevSig) || regexWords.has(prevWord))) {
      let cls = false;
      for (i++; i < src.length; i++) {
        if (src[i] === '\\') { i++; continue; }
        if (src[i] === '[') cls = true;
        else if (src[i] === ']') cls = false;
        else if (src[i] === '/' && !cls) break;
        else if (src[i] === '\n') break;
      }
      while (/[a-z]/i.test(src[i + 1] || '')) i++;
      prevSig = ')'; prevWord = ''; continue;
    }
    if (c === '{' || c === '(' || c === '[') { visit(i, depth); depth++; }
    else if (c === '}' || c === ')' || c === ']') { depth--; visit(i, depth); }
    else visit(i, depth);
    if (/\s/.test(c)) continue;
    if (/[\w$]/.test(c)) {
      prevWord = /[\w$]/.test(src[i - 1] || '') ? prevWord + c : c;
      prevSig = 'w';
    } else { prevSig = c; prevWord = ''; }
  }
  return depth;
}

// Names a classic script declares at its top level (function / class /
// const / let / var, first declarator only). Two bundles sharing one
// document share these (docs/ui-rebuild-contract.md §2.1).
export function topLevelBindings(src) {
  const names = new Set();
  scanCode(src, (i, depth) => {
    if (depth !== 0) return;
    if (/[\w$.]/.test(src[i - 1] || '')) return;
    const m = /^(?:async\s+)?(?:function\s*\*?|class|const|let|var)\s+([A-Za-z_$][\w$]*)/.exec(src.slice(i, i + 80));
    if (m) names.add(m[1]);
  });
  return names;
}

// The full argument text of the call whose "(" sits at `open`.
export function callArgs(src, open) {
  let end = -1;
  let base = null;
  scanCode(src.slice(open), (i, depth) => {
    if (end >= 0) return;
    if (base === null) { base = depth; return; }
    if (depth === base && src[open + i] === ')') end = open + i;
  });
  return end < 0 ? null : src.slice(open + 1, end);
}
