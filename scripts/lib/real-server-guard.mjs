// R4 F7: manual harnesses that attach to an ALREADY RUNNING workstation must
// never silently drive the examiner's real server. The workstation listens
// on the first free port of 8477..8486 (browser mode falls back from 8477
// to 8478…), so every port of that range is refused unless the caller
// passes --i-know (R5 U12: the guard used to check 8477 only, and a server
// that had fallen back to 8478 was driven without a word). Preferred: start
// `startIsolatedServer` (lib/isolated-server.mjs) and give the harness that
// server's URL via FT_BASE.
//
//   import { guardBase } from './lib/real-server-guard.mjs';
//   const BASE = guardBase('verify-foo', process.env.FT_BASE || 'http://127.0.0.1:8477');
export const WORKSTATION_PORT_FIRST = 8477;
export const WORKSTATION_PORT_LAST = 8486;

// Whether `base` points at a port the real workstation may be using.
export function isWorkstationPort(base) {
  let port = NaN;
  try { port = Number(new URL(base).port); } catch { return false; /* not a URL we can judge */ }
  return Number.isInteger(port) && port >= WORKSTATION_PORT_FIRST && port <= WORKSTATION_PORT_LAST;
}

export function guardBase(name, base, argv = process.argv) {
  if (isWorkstationPort(base) && !argv.includes('--i-know')) {
    console.error(
      `${name}: refusing to attach to ${base} — ${WORKSTATION_PORT_FIRST}..${WORKSTATION_PORT_LAST} are the real workstation's ports.\n` +
      `  Start an isolated server (scripts/lib/isolated-server.mjs) and set FT_BASE to its URL,\n` +
      `  or re-run with --i-know if you really mean to drive the live server (it may modify the open case).`,
    );
    process.exit(2);
  }
  return base;
}
