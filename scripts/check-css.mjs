// Mechanical CSS sanity gate for the generated viewer stylesheet.
// CI runs this so a stray token (e.g. a leading quote that once silently
// killed the whole :root token block) can never ship again. It also
// validates the shared design tokens (assets/tokens.css): they must parse,
// be bundled first into BOTH pages, and meet the contrast contract in both
// themes.
import { readFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { appBundleCss, viewerBundleCss } from "./lib/app-bundle.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
// The shell layout stylesheet is assets/app/00-layout.css after T2 renames
// app.css (docs/ui-rebuild-contract.md section 1.2); accept either so the gate
// works on both sides of that rename.
const layoutCss = existsSync(join(root, "assets/app/00-layout.css")) ? "assets/app/00-layout.css" : "assets/app/app.css";
const files = ["assets/evidence_viewer.css", layoutCss];
// These have no token block of their own (kit.css is bundled right after
// tokens.css, the rest after the page stylesheet): only brace balance is
// checked. Optional ones are skipped until their track lands.
for (const extra of ["assets/kit.css", "assets/viewer/controls.css", "assets/app/30-jobs.css", "assets/app/40-finalize.css", "assets/app/56-tools.css"]) {
  if (!existsSync(join(root, extra))) {
    if (extra === "assets/kit.css" || extra === "assets/viewer/controls.css") {
      console.error(`${extra}: missing`);
      process.exitCode = 1;
    }
    continue;
  }
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
const nthBanner = (css, n) => css.split("\n").filter((l) => l.startsWith("/* ==== "))[n] ?? "";
const firstBanner = (css) => css.split("\n").find((l) => l.startsWith("/* ==== ")) ?? "";
for (const [name, css] of [
  ["shell", appBundleCss(root)],
  ["viewer", viewerBundleCss(root)],
]) {
  if (!firstBanner(css).includes("assets/tokens.css")) {
    bad(`${name} CSS bundle does not start with assets/tokens.css (node mirror)`);
  }
  if (!nthBanner(css, 1).includes("assets/kit.css")) {
    bad(`${name} CSS bundle must carry assets/kit.css second (node mirror)`);
  }
  if ((css.match(/\/\* ==== assets\/kit\.css/g) ?? []).length !== 1) {
    bad(`${name} CSS bundle must include assets/kit.css exactly once`);
  }
}
{
  const build = readFileSync(join(root, "build.rs"), "utf8");
  if ((build.match(/kit_path\(\)/g) ?? []).length < 3) {
    bad("build.rs must bundle kit_path() into both the shell and the viewer");
  }
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
// Section 5.2 additions: video stage text, non-text contrast (WCAG 1.4.11) for
// control outlines / accent, and single-colour mark glyphs.
pairs.push(["--media-muted", "--media-bg", 4.5]);
for (const s of surfaces) {
  pairs.push(["--border-strong", s, 3], ["--accent", s, 3]);
  for (const m of marks) pairs.push([`--mark-${m}-ink`, s, 3]);
}
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
if (!/--fs-xs:\s*13px/.test(tokensCss)) bad("--fs-xs must be 13px (minimum text size)");
if (!/--fs-sm:\s*14px/.test(tokensCss)) bad("--fs-sm must be 14px (body size)");
if (!/--radius:\s*3px/.test(tokensCss)) bad("--radius must be 3px");
for (const [theme, map] of [["light", light], ["dark", dark]]) {
  for (const [k, v] of Object.entries({ "--ctl-h": "32px", "--ctl-h-lg": "36px", "--ctl-h-icon": "28px", "--hit-min": "24px" })) {
    if (map[k] !== v) bad(`${theme}: ${k} must be ${v}`);
  }
}
if (tokenErrors) process.exitCode = 1;
else console.log(`assets/tokens.css: ok (${pairs.length} contrast pairs x 2 themes, bundled first into shell + viewer)`);
