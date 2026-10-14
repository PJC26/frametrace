// R4 F7: manual harnesses that attach to an ALREADY RUNNING workstation must
// never silently drive the examiner's real server. Port 8477 is the default
// workstation port (the first of 8477..8486), so refuse it unless the caller
// passes --i-know. Preferred: start `startIsolatedServer` (lib/isolated-server.mjs)
// and give the harness that server's URL via FT_BASE.
//
//   import { guardBase } from './lib/real-server-guard.mjs';
//   const BASE = guardBase('verify-foo', process.env.FT_BASE || 'http://127.0.0.1:8477');
export function guardBase(name, base, argv = process.argv) {
  let port = '';
  try { port = new URL(base).port; } catch { /* not a URL we can judge */ }
  if (port === '8477' && !argv.includes('--i-know')) {
    console.error(
      `${name}: refusing to attach to ${base} — that is the real workstation port.\n` +
      `  Start an isolated server (scripts/lib/isolated-server.mjs) and set FT_BASE to its URL,\n` +
      `  or re-run with --i-know if you really mean to drive the live server (it may modify the open case).`,
    );
    process.exit(2);
  }
  return base;
}
