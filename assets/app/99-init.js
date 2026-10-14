// Windows review item 11: the launch URL carries ?token= once — or, since
// R4 B2, a one-time launch code ?launch= (30 s, single use). The server has
// already planted the HttpOnly cookie on this very response, so drop both
// from the address bar / history (bookmarks, screenshots, session restore)
// while keeping every other query parameter and the hash. Reloads and
// follow-up requests authenticate with the cookie.
(function stripTokenFromUrl() {
  try {
    const url = new URL(window.location.href);
    if (!url.searchParams.has('token') && !url.searchParams.has('launch')) return;
    url.searchParams.delete('token');
    url.searchParams.delete('launch');
    window.history.replaceState(window.history.state, '', url.pathname + url.search + url.hash);
  } catch (err) {
    // Not fatal for the session (the cookie still authenticates), but never silent.
    console.warn('토큰 URL 정리 실패 / could not strip ?token= from the URL', err);
  }
})();

// Boot: sync the form, then the first status poll renders the screen
// (97-shell.js keeps it blank until that reply, so a bound case never
// flashes the empty state first).
syncInputKind();
poll();
