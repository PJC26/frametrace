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
// R11 U3 (P11): a second incomplete package ("_INCOMPLETE_2") and an
// Explorer copy (" - 복사본", " (2)") count too; trailing dots / spaces
// (dropped by Windows) are ignored.
const H_PKG_INCOMPLETE = "최종화 미완료";
const H_PKG_INCOMPLETE_RE = /_INCOMPLETE(_\d+)?( - 복사본| \(\d+\))?$/i;
function packageIncomplete(dir, flag) {
  if (flag === true) return true;
  const base = String(dir || "").replace(/[\\/. ]+$/, "").split(/[\\/]/).pop() || "";
  return H_PKG_INCOMPLETE_RE.test(base) || /\.finalize-incomplete\.txt$/i.test(base);
}

// A6: an Enter that only commits an IME composition (Korean input) is
// not a command — every Enter handler of the shell checks this first.
const hImeComposing = e => !!e && (e.isComposing === true || e.keyCode === 229);
