// Boot: sync the form, then the first status poll renders the screen
// (97-shell.js keeps it blank until that reply, so a bound case never
// flashes the empty state first).
syncInputKind();
poll();
