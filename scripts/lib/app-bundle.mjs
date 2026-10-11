// STR-01: the workstation shell is authored as assets/examiner_app.html
// (markup with <!-- @bundle:app.css --> / <!-- @bundle:app.js --> markers)
// plus assets/app/*.css|*.js. build.rs inlines them into one page; these
// helpers reproduce that bundle exactly for node harnesses.
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

export function appParts(ext, root = repoRoot) {
  const dir = join(root, 'assets/app');
  return readdirSync(dir).filter((f) => f.endsWith('.' + ext)).sort().map((f) => join(dir, f));
}

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
// Shared tokens (assets/tokens.css) are the first CSS part of both pages,
// the shared UI kit (assets/kit.css) the second (docs/ui-rebuild-contract.md §5.4).
export const tokensPath = (root = repoRoot) => join(root, 'assets/tokens.css');
export const kitPath = (root = repoRoot) => join(root, 'assets/kit.css');
export const appBundleCss = (root = repoRoot) => concatParts([tokensPath(root), kitPath(root), ...appParts('css', root)], 'assets/app/bundle.css', root);
// The standalone/exported viewer stylesheet: tokens + kit + evidence_viewer.css +
// viewer/controls.css (action bar, player toolbar, menus, toasts).
export const viewerBundleCss = (root = repoRoot) => concatParts([tokensPath(root), kitPath(root), join(root, 'assets/evidence_viewer.css'), join(root, 'assets/viewer/controls.css')], 'assets/viewer/bundle.css', root);
export const appMarkup = (root = repoRoot) => readFileSync(join(root, 'assets/examiner_app.html'), 'utf8');
// Offline icon sprite, inlined where a template carries the marker (build.rs inline_icons).
export const iconsSprite = (root = repoRoot) => readFileSync(join(root, 'assets/icons.svg'), 'utf8').trimEnd();
export function inlineIcons(page, root = repoRoot) {
  const marker = '<!-- @bundle:icons.svg -->';
  const n = page.split(marker).length - 1;
  if (n > 1) throw new Error(`template must contain at most one ${marker} (found ${n})`);
  return n ? page.replace(marker, () => iconsSprite(root)) : page;
}

// The page exactly as GET / serves it.
export function appPageHtml(root = repoRoot) {
  return inlineIcons(appMarkup(root)
    .replace('<!-- @bundle:app.css -->', () => `<style>\n${appBundleCss(root)}</style>`)
    .replace('<!-- @bundle:app.js -->', () => `<script>\n${appBundleJs(root)}</script>`), root);
}
