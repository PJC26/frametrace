// Workstation shell script. assets/app/*.js are concatenated in filename
// order into ONE classic <script> by build.rs (see app_bundle), so
// top-level declarations are shared across files and order is
// load-bearing — keep the numeric prefixes.
const $ = id => document.getElementById(id);
// Escapes for both text and quoted attribute contexts (", ' included).
const esc = v => String(v ?? "").replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;").replaceAll("'","&#39;");

// R10 U4 (track S2): a package whose finalize did not complete is renamed
// "<name>_INCOMPLETE" (marker "<package>.finalize-incomplete.txt" beside it)
// — shown as "최종화 미완료" wherever a package or the finalized state is.
// `flag`: an explicit server field, when one is sent (absent is fine).
// R11 U3 (P11, the server's rule in track S 2a51c94): strip trailing dots /
// spaces and copy tails (" - 복사본", " - Copy", " (2)") repeatedly, then
// "_INCOMPLETE" or "_INCOMPLETE_<n>" (any case). A boolean `flag` from the
// server is its verdict and wins over the name.
const H_PKG_INCOMPLETE = "최종화 미완료";
const H_PKG_INCOMPLETE_RE = /_INCOMPLETE(_\d+)?$/i;
const H_PKG_COPY_TAIL_RE = /(?:[. ]+| - 복사본| - copy| \(\d+\))$/i;
function packageIncompleteName(dir) {
  let base = String(dir || "").replace(/[\\/]+$/, "").split(/[\\/]/).pop() || "";
  if (/\.finalize-incomplete\.txt$/i.test(base)) return true;
  for (let i = 0; i < 64 && H_PKG_COPY_TAIL_RE.test(base); i++) base = base.replace(H_PKG_COPY_TAIL_RE, "");
  return H_PKG_INCOMPLETE_RE.test(base);
}
function packageIncomplete(dir, flag) {
  if (typeof flag === "boolean") return flag;
  return packageIncompleteName(dir);
}

// A6: an Enter that only commits an IME composition (Korean input) is
// not a command — every Enter handler of the shell checks this first.
const hImeComposing = e => !!e && (e.isComposing === true || e.keyCode === 229);
