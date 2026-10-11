// Mechanical CSS sanity gate for the generated viewer stylesheet.
// CI runs this so a stray token (e.g. a leading quote that once silently
// killed the whole :root token block) can never ship again. It also
// validates the shared design tokens (assets/tokens.css): they must parse,
// be bundled first into BOTH pages, and meet the contrast contract in both
// themes.
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { appBundleCss, viewerBundleCss } from "./lib/app-bundle.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const files = ["assets/evidence_viewer.css", "assets/app/app.css"];
// controls.css has no token block of its own (it is bundled after tokens.css +
// evidence_viewer.css): only brace balance is checked here.
for (const extra of ["assets/viewer/controls.css"]) {
  const css = readFileSync(join(root, extra), "utf8");
  const open = (css.match(/{/g) ?? []).length;
  const close = (css.match(/}/g) ?? []).length;
  if (open !== close) {
    console.error(`${extra}: unbalanced braces (${open} open / ${close} close)`);
    process.exitCode = 1;
  } else console.log(`${extra}: ok`);
}

for (const file of files) {
  const css = readFileSync(join(root, file), "utf8");
  const open = (css.match(/{/g) ?? []).length;
  const close = (css.match(/}/g) ?? []).length;
  if (open !== close) {
    console.error(`${file}: unbalanced braces (${open} open / ${close} close)`);
    process.exitCode = 1;
    continue;
  }
  if (!css.trimStart().startsWith(":root")) {
    console.error(`${file}: must start with the :root token block`);
    process.exitCode = 1;
    continue;
  }
  if (!css.includes("color-scheme: light")) {
    console.error(`${file}: color-scheme guard missing`);
    process.exitCode = 1;
    continue;
  }
  console.log(`${file}: ok`);
}

// ---- shared design tokens (assets/tokens.css) ----
let tokenErrors = 0;
const bad = (msg) => {
  console.error(`tokens.css: ${msg}`);
  tokenErrors++;
};
const tokensCss = readFileSync(join(root, "assets/tokens.css"), "utf8");
{
  const open = (tokensCss.match(/{/g) ?? []).length;
  const close = (tokensCss.match(/}/g) ?? []).length;
  if (open !== close) bad(`unbalanced braces (${open} open / ${close} close)`);
  if (!tokensCss.includes('html[data-theme="dark"]')) bad("dark theme block missing");
  if (/@import|url\(\s*["']?https?:/i.test(tokensCss)) bad("must not load remote resources (offline tool)");
}
const firstBanner = (css) => css.split("\n").find((l) => l.startsWith("/* ==== ")) ?? "";
for (const [name, css] of [
  ["shell", appBundleCss(root)],
  ["viewer", viewerBundleCss(root)],
]) {
  if (!firstBanner(css).includes("assets/tokens.css")) {
    bad(`${name} CSS bundle does not start with assets/tokens.css (node mirror)`);
  }
}
{
  const build = readFileSync(join(root, "build.rs"), "utf8");
  if ((build.match(/tokens_path\(\)/g) ?? []).length < 3) {
    bad("build.rs must bundle tokens_path() into both the shell and the viewer");
  }
  const report = readFileSync(join(root, "src/html_report.rs"), "utf8");
  if (!report.includes('"/evidence_viewer.css"')) {
    bad("src/html_report.rs must embed the bundled viewer css from OUT_DIR");
  }
}

function tokenMap(block) {
  const m = {};
  for (const [, k, v] of block.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) m[k] = v.trim();
  return m;
}
const darkStart = tokensCss.indexOf('html[data-theme="dark"]');
const light = tokenMap(tokensCss.slice(tokensCss.indexOf(":root {"), darkStart));
const dark = { ...light, ...tokenMap(tokensCss.slice(darkStart, tokensCss.indexOf("}", darkStart))) };

const lum = (hex) => {
  const c = [1, 3, 5]
    .map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
    .map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
};
const ratio = (a, b) => {
  const x = lum(a);
  const y = lum(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
};
const isHex = (v) => /^#[0-9a-f]{6}$/i.test(v ?? "");
const surfaces = ["--bg", "--surface", "--surface-2"];
const statuses = ["verified", "failed", "candidate", "original", "carved", "fs-recovered", "deleted-candidate", "anomaly", "warn", "info"];
const marks = ["reviewed", "important", "needs-verification"];
const pairs = [];
for (const s of surfaces) {
  for (const t of ["--text", "--muted", "--accent", "--accent-ink", "--ok", "--danger", "--warn", "--info"]) {
    pairs.push([t, s, 4.5]);
  }
}
pairs.push(["--on-accent", "--accent", 4.5], ["--text", "--accent-soft", 4.5], ["--accent-ink", "--accent-soft", 4.5], ["--muted", "--accent-soft", 4.5]);
pairs.push(["--disabled-fg", "--disabled-bg", 3]);
for (const st of statuses) {
  pairs.push([`--${st}-fg`, `--${st}-bg`, 4.5]);
  for (const s of surfaces) pairs.push([`--${st}-fg`, s, 4.5]);
}
for (const m of marks) pairs.push([`--mark-${m}-fg`, `--mark-${m}-bg`, 4.5]);
for (const [theme, map] of [
  ["light", light],
  ["dark", dark],
]) {
  for (const [fg, bg, min] of pairs) {
    if (!isHex(map[fg]) || !isHex(map[bg])) {
      bad(`${theme}: ${fg} or ${bg} missing / not #rrggbb`);
      continue;
    }
    const r = ratio(map[fg], map[bg]);
    if (r + 1e-9 < min) bad(`${theme}: ${fg} ${map[fg]} on ${bg} ${map[bg]} = ${r.toFixed(2)}:1 < ${min}:1`);
  }
}
if (!/--fs-xs:\s*12px/.test(tokensCss)) bad("--fs-xs must be 12px (minimum text size)");
if (tokenErrors) process.exitCode = 1;
else console.log(`assets/tokens.css: ok (${pairs.length} contrast pairs x 2 themes, bundled first into shell + viewer)`);
