// Starts a dedicated, isolated FrameTrace workstation for a harness.
//
// Isolation (earlier flakiness came from harnesses attaching to another
// worker's server): an explicit port — which skips the "reuse the running
// workstation" discovery (instance file + 8477..8486 port scan) entirely —
// plus a throwaway HOME / USERPROFILE / XDG_CONFIG_HOME / APPDATA and
// FRAMETRACE_INSTANCE_FILE, so the real ~/.frametrace/server.json is never
// read or overwritten, and a pinned FRAMETRACE_TOKEN.
//
//   const srv = await startIsolatedServer({ root, work });
//   startIsolatedServer({ root, work, env: { FRAMETRACE_UI: 'legacy' } })  // extra env
//   srv.base, srv.token, srv.headers, srv.getJson(path), srv.postJson(path, body), srv.stop()
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { mkdirSync, existsSync } from 'node:fs';
import { join } from 'node:path';

export function freePort() {
  return new Promise((resolve, reject) => {
    const s = createServer();
    s.on('error', reject);
    s.listen(0, '127.0.0.1', () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
}

// Environment for any frametrace process a harness spawns.
export function isolatedEnv(work, extra = {}) {
  const home = join(work, 'home');
  const cfg = join(work, 'cfg');
  mkdirSync(home, { recursive: true });
  mkdirSync(cfg, { recursive: true });
  return {
    ...process.env,
    HOME: home,
    USERPROFILE: home,
    XDG_CONFIG_HOME: cfg,
    APPDATA: cfg,
    FRAMETRACE_INSTANCE_FILE: join(cfg, 'server.json'),
    FRAMETRACE_NO_BROWSER: '1',
    ...extra,
  };
}

export async function startIsolatedServer({ root, work, token, port, release = false, env = {} } = {}) {
  // FRAMETRACE_BIN pins an exact binary (CI builds once); otherwise
  // $CARGO_TARGET_DIR (or ./target) / debug|release / frametrace-app.
  const dir = join(process.env.CARGO_TARGET_DIR || join(root, 'target'), release ? 'release' : 'debug');
  let exe = process.env.FRAMETRACE_BIN || join(dir, 'frametrace-app' + (process.platform === 'win32' ? '.exe' : ''));
  // On Windows a FRAMETRACE_BIN given without the extension (e.g. from Git Bash)
  // still resolves: spawn() would not append .exe itself.
  if (process.platform === 'win32' && !existsSync(exe) && !/\.exe$/i.test(exe) && existsSync(exe + '.exe')) exe += '.exe';
  if (!existsSync(exe)) throw new Error(`build first (cargo build --locked) or set FRAMETRACE_BIN: ${exe} missing`);
  const pinned = token || 'iso-' + Math.random().toString(36).slice(2, 12);
  const usePort = port || await freePort();
  const base = `http://127.0.0.1:${usePort}`;
  const child = spawn(exe, [String(usePort)], {
    env: isolatedEnv(work, { ...env, FRAMETRACE_TOKEN: pinned }),
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let log = '';
  child.stdout.on('data', (d) => { log += d; });
  child.stderr.on('data', (d) => { log += d; });
  let exited = null;
  // Resolves once the process is gone (also for spawn errors), so callers can
  // delete the work dir without hitting EBUSY on Windows.
  const exitPromise = new Promise((resolve) => {
    child.on('exit', (code) => { exited = code ?? -1; resolve(); });
    child.on('error', () => { if (exited === null) exited = -1; resolve(); });
  });
  const headers = { 'X-FrameTrace-Token': pinned };
  for (let i = 0; ; i++) {
    if (exited !== null) throw new Error(`server exited (${exited}):\n${log}`);
    try {
      const res = await fetch(base + '/api/status', { headers });
      if (res.ok) {
        const st = await res.json();
        if (st.app === 'frametrace') break;
      }
    } catch { /* not listening yet */ }
    if (i > 150) { child.kill(); throw new Error('server did not start:\n' + log); }
    await new Promise((r) => setTimeout(r, 100));
  }
  const getJson = async (path) => (await fetch(base + path, { cache: 'no-store', headers })).json();
  const postJson = async (path, body, contentType = 'application/json') => (await fetch(base + path, {
    method: 'POST',
    headers: { ...headers, 'Content-Type': contentType },
    body: typeof body === 'string' ? body : JSON.stringify(body),
  })).json();
  return {
    base, token: pinned, headers, exe, port: usePort, getJson, postJson,
    log: () => log,
    // Returns a promise that resolves when the server process has exited
    // (bounded wait; force-kills if it ignores the first signal).
    stop() {
      if (exited === null) {
        try { child.kill(); } catch { /* gone */ }
      }
      // The fallback timer is unref'd so a server that exits promptly does
      // not keep the harness's event loop alive for the full 5 s.
      return Promise.race([
        exitPromise,
        new Promise((r) => {
          setTimeout(() => {
            if (exited === null) { try { child.kill('SIGKILL'); } catch { /* gone */ } }
            r();
          }, 5000).unref();
        }),
      ]);
    },
  };
}
