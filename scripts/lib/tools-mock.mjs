// A mocked GET /api/tools + POST /api/tools/install (track S contract,
// docs/ui-rebuild-contract.md §28) for browser harnesses (verify-layout,
// check-a11y). Nothing is downloaded: the install walks downloading →
// verifying → done over the next GET polls.
export const TOOLS_FIXTURE = () => ([
  { name: 'ffmpeg', label: 'FFmpeg', required: true, installed: false, path: null, version: null, pinned_version: '7.1', license: 'LGPL-2.1-or-later', homepage: 'https://ffmpeg.org/', source_host: 'github.com', size_bytes: 31457280, installable: true, unavailable_reason: null, kind: 'binary', pip_command: null },
  { name: 'libewf', label: 'libewf', required: false, installed: false, path: null, version: null, pinned_version: '20231119', license: 'LGPL-3.0-or-later', homepage: 'https://github.com/libyal/libewf', source_host: 'github.com', size_bytes: 4194304, installable: true, unavailable_reason: null, kind: 'binary', pip_command: null },
  { name: 'sleuthkit', label: 'Sleuth Kit', required: false, installed: false, path: null, version: null, pinned_version: '4.12.1', license: 'IPL-1.0 / CPL-1.0', homepage: 'https://www.sleuthkit.org/sleuthkit/download.php', source_host: 'github.com', size_bytes: null, installable: false, unavailable_reason: '공식 체크섬을 찾지 못해 여기서 설치할 수 없습니다', kind: 'binary', pip_command: null },
]);

// opts.steps: GET polls per install stage (default 2). Returns the live
// mock state { posts: [{name}], tools, install }.
export async function routeTools(page, opts = {}) {
  const steps = opts.steps || 2;
  const m = { posts: [], tools: opts.tools || TOOLS_FIXTURE(), install: { state: 'idle', tool: null, bytes: null, total: null, error: null }, polls: 0 };
  await page.route('**/api/tools/install', async (route) => {
    const req = route.request();
    if (req.method() !== 'POST') return route.fulfill({ status: 405, body: '{"ok":false}', contentType: 'application/json' });
    let body = {};
    try { body = JSON.parse(req.postData() || '{}'); } catch { /* bad body */ }
    m.posts.push(body);
    const t = m.tools.find((x) => x.name === body.name);
    if (!t || !t.installable) return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ ok: false, error: 'not installable' }) });
    m.install = { state: 'downloading', tool: t.name, bytes: 0, total: t.size_bytes, error: null };
    m.polls = 0;
    return route.fulfill({ status: 200, contentType: 'application/json', body: '{"ok":true}' });
  });
  await page.route('**/api/tools', async (route) => {
    const i = m.install;
    if (i.tool && i.state !== 'done' && i.state !== 'failed') {
      m.polls += 1;
      const t = m.tools.find((x) => x.name === i.tool);
      if (i.state === 'downloading') {
        i.bytes = Math.min(i.total || 0, Math.round((i.total || 0) * m.polls / (steps + 1)));
        if (m.polls > steps) { i.state = 'verifying'; m.polls = 0; }
      } else if (i.state === 'verifying') {
        if (m.polls > steps) { i.state = opts.fail ? 'failed' : 'done'; if (opts.fail) i.error = opts.fail; else t.installed = true; }
      }
    }
    return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ ok: true, tools: m.tools, install: m.install }) });
  });
  return m;
}
