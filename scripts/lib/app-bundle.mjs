// STR-01: the workstation shell is authored as assets/examiner_app.html
// (markup with <!-- @bundle:app.css --> / <!-- @bundle:app.js --> markers)
// plus assets/app/*.css|*.js. build.rs inlines them into one page; these
// helpers reproduce that bundle exactly for node harnesses.
import { readFileSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

export const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

export function appParts(ext, root = repoRoot) {
  const dir = join(root, 'assets/app');
  return readdirSync(dir).filter((f) => f.endsWith('.' + ext)).sort().map((f) => join(dir, f));
}

// Same concatenation as build.rs `concat`: newline-terminated, sorted.
export function concatParts(files) {
  let bundle = '';
  for (const f of files) {
    bundle += readFileSync(f, 'utf8');
    if (!bundle.endsWith('\n')) bundle += '\n';
  }
  return bundle;
}

export const appBundleJs = (root = repoRoot) => concatParts(appParts('js', root));
export const appBundleCss = (root = repoRoot) => concatParts(appParts('css', root));
export const appMarkup = (root = repoRoot) => readFileSync(join(root, 'assets/examiner_app.html'), 'utf8');

// The page exactly as GET / serves it.
export function appPageHtml(root = repoRoot) {
  return appMarkup(root)
    .replace('<!-- @bundle:app.css -->', () => `<style>\n${appBundleCss(root)}</style>`)
    .replace('<!-- @bundle:app.js -->', () => `<script>\n${appBundleJs(root)}</script>`);
}
