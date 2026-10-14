// Workstation shell script. assets/app/*.js are concatenated in filename
// order into ONE classic <script> by build.rs (see app_bundle), so
// top-level declarations are shared across files and order is
// load-bearing — keep the numeric prefixes.
const $ = id => document.getElementById(id);
// Escapes for both text and quoted attribute contexts (", ' included).
const esc = v => String(v ?? "").replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;").replaceAll("'","&#39;");

// A6: an Enter that only commits an IME composition (Korean input) is
// not a command — every Enter handler of the shell checks this first.
const hImeComposing = e => !!e && (e.isComposing === true || e.keyCode === 229);
