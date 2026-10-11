// Lint: every title= in assets/evidence_viewer.html needs data-i18n-title, and
// every data-i18n* key used by the page must exist in both locales (ko, en).
// Also reports keys present in only one locale.
//
//   node scripts/verify-i18n.mjs
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const html = readFileSync(join(root, 'assets/evidence_viewer.html'), 'utf8');
const src = readFileSync(join(root, 'assets/viewer/20-i18n.js'), 'utf8');

const start = src.indexOf('const I18N = {');
const end = src.indexOf('\n};', start);
if (start < 0 || end < 0) { console.error('cannot locate the I18N table in 20-i18n.js'); process.exit(1); }
const I18N = vm.runInNewContext('(' + src.slice(start + 'const I18N = '.length, end + 2) + ')');
// Layout strings live in 21-i18n-layout.js and are merged into I18N at load.
try {
  const lsrc = readFileSync(join(root, 'assets/viewer/21-i18n-layout.js'), 'utf8');
  const ls = lsrc.indexOf('const I18N_LAYOUT = {');
  const le = lsrc.indexOf('\n};', ls);
  if (ls >= 0 && le >= 0) {
    const extra = vm.runInNewContext('(' + lsrc.slice(ls + 'const I18N_LAYOUT = '.length, le + 2) + ')');
    for (const loc of ['ko', 'en']) Object.assign(I18N[loc], extra[loc] || {});
  }
} catch { /* optional part */ }

const problems = [];
const tags = html.match(/<[a-zA-Z][^<>]*>/g) || [];
for (const tag of tags) {
  const hasTitle = /\stitle="/.test(tag);
  const hasKey = /\sdata-i18n-title="/.test(tag);
  const id = (/\sid="([^"]+)"/.exec(tag) || [])[1] || tag.slice(0, 60);
  if (hasTitle && !hasKey) problems.push(`title= without data-i18n-title: ${id}`);
}
const attrs = ['data-i18n', 'data-i18n-title', 'data-i18n-placeholder', 'data-i18n-aria-label'];
for (const attr of attrs) {
  const re = new RegExp(`\\s${attr}="([^"]+)"`, 'g');
  let m;
  while ((m = re.exec(html))) {
    for (const loc of ['ko', 'en']) {
      if (!(m[1] in I18N[loc])) problems.push(`${attr}="${m[1]}" missing in ${loc}`);
    }
  }
}
for (const k of Object.keys(I18N.ko)) if (!(k in I18N.en)) problems.push(`key only in ko: ${k}`);
for (const k of Object.keys(I18N.en)) if (!(k in I18N.ko)) problems.push(`key only in en: ${k}`);

if (problems.length) {
  console.error(problems.join('\n'));
  console.error(`\n${problems.length} i18n problem(s)`);
  process.exit(1);
}
console.log(`ok   i18n: ${Object.keys(I18N.ko).length} ko / ${Object.keys(I18N.en).length} en keys, all titles localised`);
