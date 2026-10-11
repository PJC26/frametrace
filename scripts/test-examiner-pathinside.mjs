// Regression: pathInside treats "/" as a root containing every absolute path.
import vm from 'node:vm';
import { appBundleJs } from './lib/app-bundle.mjs';
const html = appBundleJs();
const m = html.match(/function normPath[\s\S]*?\nfunction pathInside[\s\S]*?\n}\n/);
if (!m) { console.error('FAIL cannot extract pathInside'); process.exit(1); }
const ctx = vm.createContext({});
vm.runInContext(m[0] + ';this.pathInside=pathInside;', ctx);
const t = (c, p, want) => { if (ctx.pathInside(c, p) !== want) { console.error(`FAIL pathInside(${c}, ${p}) != ${want}`); process.exitCode = 1; } };
t('/a/b', '/', true); t('/', '/', true); t('a/b', '/', false); t('', '/', false);
t('C:\\x\\y', 'C:\\x', true); t('C:\\xy', 'C:\\x', false); t('/a', '', false);
if (!process.exitCode) console.log('ok   pathInside');
