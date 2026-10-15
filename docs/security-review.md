# Security Review

Phase 2 review focused on local file handling, external command boundaries, report/viewer serialization, privacy leakage, and packaging.

## Findings

| Severity | Finding | Status | Owner |
| --- | --- | --- | --- |
| High | User-configurable external binary names can execute arbitrary binaries through `Command::new`. | Fixed for ffprobe, ffmpeg, libewf, and Sleuth Kit user-configurable binaries. | Security Owner |
| High | Bare tool names resolved by `Command::new` let the Windows loader search the current directory (binary planting from evidence media). | Fixed: bare names are resolved manually against PATH only, returning canonical paths or failing. | Security Owner |
| High | Output paths can be directed outside the case workspace for some export/proxy/package/recovery operations. | Fixed for E01 raw export, video export, proxy, thumbnail, inode recovery, marks export, and recursive package traversal. | Security Owner |
| High | Reports and viewer payloads expose full source paths by default. | Mitigated: `make-report`/`make-review --redact-paths` rewrites absolute paths as `<case>/`-relative or `<redacted:hash>` tokens and marks the output; default output is still unredacted. | Security Owner |
| High | Generated pages embedded `case.json` / `db/video_index.json` into scripts as raw text (only `<>&` escaped): a crafted case ran script in the workstation origin from a page the generated-pages list vouches for (R6-S1). | Fixed: every embedded JSON input is parsed and re-serialized (`util::json_for_script`); text that is not one JSON value is embedded as `null` with a warning. | Security Owner |
| Medium | Selector-to-path resolution may trust poisoned logs or free-form paths. | Pending | Security Owner |
| Medium | Recursive packaging could follow symlinked inputs outside the intended tree. | Fixed for package inputs. | Engineering Lead |
| Medium | Manual JSON-like parsing increases malformed input risk. | Partially mitigated for `ffprobe`; broader migration pending. | Engineering Lead |
| Medium | Media-path containment compared canonical path strings case-insensitively, which could pass a case-only-different sibling on case-sensitive filesystems. | Fixed: `Path::starts_with` component-wise comparison on canonicalized paths. | Engineering Lead |
| Medium | The workstation pipeline piped child stdout/stderr but only drained them after exit, so a chatty child (>64 KiB output) could deadlock the run. | Fixed: both pipes are drained on separate threads while the child runs; only a bounded tail is retained. | Engineering Lead |
| Low | `first_free_port` probed a port by binding then dropping the listener, racing any process that claimed it before the real bind. | Fixed: the successfully bound listener is reused directly. | Engineering Lead |
| Low | `fs2` 0.4.3 (audit-chain file locking, disk-space checks) is unmaintained upstream; successor `fs4` is an API-incompatible migration. | Tracked, not exempted: `.cargo/audit.toml` lists no ignore for it, so the `cargo audit --deny warnings` job is the arbiter (see "Dependency audit policy"). If audit starts reporting an advisory for `fs2`, the job fails until the id is added to `[advisories] ignore` with a reason and a re-check date, or `fs2` is replaced. Re-evaluate if a lock-semantics defect appears. | Engineering Lead |

## Implemented Security Fixes

1. `ffmpeg` invocations (export, proxy, thumbnail, version logging) now go through the same tool-policy allowlist as ffprobe/libewf/Sleuth Kit, and the unvalidated `audit::command_version` helper was removed.
2. Bare tool names are resolved manually against PATH (current directory excluded), removing the Windows binary-planting window for planted executables.
3. Recursive package inputs now reject symlinks instead of following them.
4. Required package files are validated before package generation.
5. Invalid `ffprobe` JSON output now fails closed instead of corrupting JSON index output.
6. Scan now rejects using the case directory as the source and skips nested case output directories.
7. `src/tool_policy.rs` allowlists external tool binaries and rejects unapproved bare names.
8. Explicit derived-output paths must resolve under the case directory for recovery/export artifacts — now including `export-marks --output`.
9. The workstation Origin gate now parses the `http://` authority host exactly (`127.0.0.1`, `localhost`, `[::1]`, optional `:port`), so suffix/userinfo spoofs such as `http://127.0.0.1.evil.com` no longer pass a prefix check.
10. Parallel batch workers (validate-batch, thumbnail generation) recover poisoned mutexes via `into_inner()` instead of cascading one worker panic into a whole-batch abort.

## Workstation Loopback Threat Model

The examiner workstation (running `frametrace` with no arguments, `src/serve/`) binds `127.0.0.1` only. **A shared-secret token is ON by default (SEC-04):** at startup the server generates a random 128-bit access token (`resolve_token` in `src/serve/mod.rs`), and every request must prove it via the `X-FrameTrace-Token` header, a `?token=` query parameter, or the `ft_token_<port>` cookie (named per port so two instances on one host never overwrite each other; the legacy `ft_token` name is still accepted for one release when it validates) planted by a successful query-token request (`HttpOnly; SameSite=Strict`). The workstation prints and opens the tokenized URL; token-less requests get `401`. Local relaunches and the shell recover the token from the user-private instance file. `FRAMETRACE_TOKEN` (8+ chars of `A-Z a-z 0-9 . _ - ~`) pins a specific token instead of a random one, and `FRAMETRACE_NO_TOKEN=1` opts out entirely (E2E harnesses only; do not use on a machine with other local users or untrusted processes). Any process already running as the examiner and able to read the instance file still holds equivalent privileges over the case directory - the token protects against other local users, web content and port-guessing, not against examiner-level malware.

Consequences and the controls that exist:

- **Mutating requests require the per-instance nonce (CSRF gate, independent of the token).** The server generates a 128-bit instance nonce at startup (written to the user-private instance file and reported as `instance` by `/api/status`), plants it as a `ft_nonce_<port>` cookie (`HttpOnly; SameSite=Strict`) on GET responses, and requires it on every non-GET/HEAD request via the `X-FrameTrace-Nonce` header, the cookie, or a `?nonce=` query value. Same-origin page fetches carry the cookie automatically; cross-site form POSTs cannot (SameSite=Strict strips it) and non-browser clients that never learned the nonce are refused with `401`. Scripts read the nonce from the instance file or `/api/status`. **A local process that can read the user's instance file or loopback responses can still invoke any endpoint** — the nonce raises the bar for web-content attackers (CSRF, malicious pages, rebound origins), not for local malware, which already holds examiner-equivalent privileges; the token is the additional gate against other local users.
- **Connection concurrency is capped** in two separate pools (R5-S7): at most 48 connections server-wide may sit in the unauthenticated header phase (a slow-drip client holds one for at most 10 s), and at most 64 authenticated requests run at once; overflow gets an immediate `503` so idle sockets cannot exhaust threads, and unauthenticated connections can never use up the authenticated pool. (The R4 per-peer cap was effectively global — every local client is 127.0.0.1.)
- **Generated pages and the scripts they load are pinned by content (R4-B5, R5-S1/S2).** `review/.generated.json` lists the SHA-256 of exactly the files each generator wrote (make-review: the review pages and `review/data-bundle.js`; make-report; make-custody), under an HMAC with a per-user key kept outside the case. The list records the bytes each generator wrote, not a later read of the file (R6-S7). A `.js` file under the case is served only when its bytes are listed (else `403`), and both scripts and generated pages are read once, verified, and served from those same bytes (no re-open after the check); `/media` never serves evidence with a script MIME type. Sockets carry a 30 s read timeout and a 60 s write timeout, request headers are capped at 64 KiB, and bodies at 4 MiB.
- **`/review/` and `/case/` responses are streamed** in 64 KiB chunks instead of being buffered whole — large artifacts no longer multiply memory use by request count. `/media` was already Range-streamed and restricted to the approved evidence roots.
- **Browser-originated requests are gated.** Every route (including `/media`) requires a loopback `Host` header (the DNS-rebinding gate) and, when present, an `Origin` whose authority parses to an exact loopback host (the cross-site form-POST gate). Requests with **no** `Origin` — curl, local scripts, the page the workstation itself opened — are trusted, because a local caller needs no browser to reach the port.
- **UNC / device paths are refused by default (SEC item 8, `REMOTE_PATH_ERROR` in `src/serve/http.rs`).** `\\server\share`, `//server/share`, `\\?\UNC\...` and the device namespace are rejected before any filesystem call (a bare `canonicalize`/`metadata` on a UNC path can already trigger an SMB connection and leak NTLM credentials). The rules differ by route: every **GET** route (the cross-origin vector: `<img>`/`<video>` send no `Origin`) refuses UNC unless the path lies under an approved root that is itself UNC (exact share prefix, component boundary, no `..`); the **POST** routes where the examiner types a source/case path (`/api/preflight`, `/api/start`) admit a well-formed UNC share path (no `.`/`..` components), because they are gated by the Origin port check and the instance nonce; the device namespace (`\\.\`, `\\?\GLOBALROOT`, `\??\`) and reserved device names are refused on every route. The verbatim local-drive form `\\?\C:\...` stays allowed (it never leaves the machine). The check is textual and platform-independent.
- **Recommended mitigations for examiners:** run the workstation on a single-user analysis machine, avoid browsing untrusted sites in the same browser session while a case is loaded, and close the window (which stops the server) when review is done. Multi-user or remote access would require a changed bind policy and a reviewed auth design (the default token is a loopback measure, not a remote-access credential) — loopback trust does not extend to other interfaces.

## Remaining Security Work

1. Report privacy/redaction mode exists as an opt-in flag (`--redact-paths`); decide whether redaction becomes the default before distributable report release.
2. Harden selector-to-path resolution against poisoned logs.
3. Replace manual JSON extraction with typed parsing where feasible.
4. Add release-time privacy leakage QA once redaction policy is approved.

## Validation

- Symlink package regression test added.
- Missing required package regression test added.
- Scan exclusion regression test added.
- `ffprobe` JSON structural helper test added.
- Tool binary allowlist regression tests added.
- Case-contained output path regression tests added (including the `export-marks` sibling-prefix rejection).
- Origin authority exact-match regression test added.
- Mutex poison recovery regression test added.
- CLI smoke test keeps missing libewf guidance while enforcing the allowlist.

## Dependency audit policy

`.github/workflows/audit.yml` runs `cargo audit --deny warnings` on every push/PR and weekly: vulnerabilities **and** warnings (unmaintained, unsound, yanked) fail the job. An advisory may only be accepted by listing its id under `[advisories] ignore` in `.cargo/audit.toml` together with a reason and a re-check date. Currently nothing is accepted. All third-party GitHub Actions are pinned to full commit SHAs (version in the trailing comment); bump them deliberately.
