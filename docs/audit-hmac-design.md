# Audit Chain HMAC Design

**Status: implemented (opt-in).** Keyed appends and verification ship behind
key configuration — with no key configured, behavior is byte-identical to the
unkeyed chain. Legal/practice review still governs how much weight a keyed log
carries in testimony; the code deliberately reports `integrity-structural-only`
so unkeyed logs are never oversold. This document records the chain format, the
residual threat, and the keyed format as shipped.

## Trust policy (R8) — the governing rule

Every "can this be relied on?" decision over audit entries follows ONE rule,
implemented once in `src/audit.rs` (`trust_policy`, `AuthRequirement`,
`AuditChainVerification::line_trust` / `lane_trust`) and used by every
consumer — sound levels, sign-offs (any schema), E01 raw-image trust, lane
verification (`verify-audit`, `verify-package`, the report's chain table,
custody statement, `doctor`, the workstation `/api/verify-audit`):

- **P0 — keys held here ⇒ only what is authenticated HERE is trusted.**
  Whether authentication is required is decided by whether **this PC holds
  audit keys** (the verification key set is non-empty; a key source that is
  present but broken counts as keys held — fail closed). It is **never**
  decided by signals inside the case files — a lane's presence, `entry_hmac_*`
  fields, "this case is keyed" — because an attacker can delete or strip them
  (R7's High findings were exactly that: strip every HMAC, delete the
  sign-off lane, delete the E01 lane). With keys held, nothing that is not
  HMAC-authenticated here is ever shown as trusted / current / measured /
  reusable: an unkeyed (possibly stripped) line, a line under a retired key
  nothing later proves, and a line under a key this PC does not hold are all
  "not authenticated here".
- **P1 — no keys here ⇒ self-consistency only, and every trust display says
  so** ("자기 일관성만 확인(인증 아님)"): lanes grade
  `integrity-structural-only` with that caveat, sound levels carry `"sc":
  true`, legacy and keyless sign-offs say it in the banner and in
  `verify-package`.
- **P2 — legitimate multi-PC work ends as "not authenticable here — check on
  the PC holding key <id>", never FAIL.** A FAIL needs a key **this PC
  holds** that the entry names to refute it (a `kid-<sha8>` fingerprint id
  this PC holds, or a held keyring id): that is a verification error. An
  entry under any other key — including the legacy shared id `"default"`
  that no held key verifies (R8-I7) — is `LineTrust::OtherPc`; a lane holding
  one is NOT VERIFIED (not PASS, not FAIL) and lists the key ids.

| Lane / entry state | Keys held here (P0) | No keys here (P1) |
| --- | --- | --- |
| Every entry HMAC-authenticated here | `Authenticated` → PASS, `integrity-keyed` | — |
| Some entry unkeyed (incl. stripped), or retired-key-unproven | **NOT VERIFIED** (`integrity-not-verified`) | PASS, structural, self-consistency only |
| Some entry under a key this PC does not hold | **NOT VERIFIED**, "키 <id> 보유 PC에서 확인" (P2) | PASS, structural, "키 <id> 보유 PC에서 확인" |
| Unkeyed entries before an authenticated `audit-key-introduced` marker that is the lane's FIRST keyed entry (R8-I8), everything after authenticated | PASS, structural, "키 도입 이전 항목 N개는 자기 일관성만" | same as above |
| Chain does not verify (structure, or a held key refutes an entry) | FAIL | FAIL (structure only) |

Per consumer (R8 track I):

- **Sound levels:** a line is a measurement only when `line_trust` allows it;
  with keys held an unauthenticated line is `unverified` (another PC's line
  carries `"pc": "<key id>"`), never a level, and never replaces a verified
  one; a self-consistent level never replaces an authenticated one. A
  continuation lane's marker that this PC cannot authenticate is **not
  damage** (no new lane — R7's rule made two PCs open lanes for each other
  forever); its lines are judged per line.
- **Sign-offs:** a v4 record is Current only through a lane entry
  authenticated here. With keys held a legacy (v<4) record is **never**
  Current — at most amber "legacy binding, no authentication" — whether or
  not the sign-off lane exists.
- **E01 raw images:** with keys held a completion marker is Complete only
  through an authenticated `import-e01` entry it names — the newest import of
  that raw file from the marker's E01; a marker naming none, or a deleted E01
  lane, means "re-verify". Before reuse the raw image is re-hashed once
  (progress, cancel) against that entry's SHA-256.
- **Lanes everywhere:** `lane_trust()` gives the status word
  (`keyed`/`structural`/`not-verified`), label and reason every consumer
  prints.

### Row-level trust, full-case scope, fail-closed keys (R9)

R8 applied P0 per LANE; consumers that read a lane's ROWS (report
sections, viewer bundle, `/api/records-meta`, sign-off targets, timeline,
QA, custody re-hash table, artifact lookups) still used every row. R9:

- **P3 — one reader.** Every non-test reader of a chained lane's rows goes
  through `audit::read_lane_rows(path)` (or `read_lane_rows_with_keys`):
  `LaneRows { present, rows: [LaneRow { index, raw, value, trust:
  LineTrust }], lane: LaneTrust, failure, key_error,
  unbound_authenticated, verification }`. `LaneRow::usable()` = the entry
  may feed a "current / confirmed / passed" claim or a path / hash used
  for re-verification: authenticated here, the R8-I8 pre-key prefix, or
  (no keys here, P1) self-consistency only. Entries past a chain break are
  `LineTrust::ChainBroken` (unusable on any PC). Unusable entries are
  shown, if at all, distinguished as "인증 안 됨" — the report lists them in
  their own table above the chain table and never counts them; the
  timeline emits them as `audit-entry-unauthenticated`; custody counts
  them beside the re-hash table; artifact / validation / proxy / screening
  lookups ignore them; a sign-off's re-hash target path and bound hash are
  the LAST usable entry's (none → "확인 불가"). Restriction-only readers
  (the join log protecting files from deletion, "the carve log has
  entries", sign-off-lane caps) count every entry — the safe direction.
  A source-scan test (`audit::tests_r9::r9_p3_every_lane_row_reader_
  goes_through_read_lane_rows`) fails when a non-test function names a
  lane and reads files raw without the reader. Its limits: it is
  name-based (a lane path built far from where it is read, in a function
  that also calls the reader, is not caught), and the sound-measurement
  lanes keep their own reader (`audio_level::read_lanes`, allow-listed:
  the continuation proof hashes the raw lane bytes; every line is judged
  by `line_trust`, R8-I1).
- **Per-entry `trust` (viewer contract).** Each log entry the viewer bundle
  (`data-bundle.js`) and `/api/records-meta` (`carveLog`, `filesystemLog`,
  `validationLog`, `anomalyLog`, `joinLog`) carry has `"trust"`: `"auth"`
  (authenticated here), `"sc"` (self-consistency only — no keys here, or
  the pre-key prefix), `"unauth"` (keys held and not authenticated, or past
  a chain break; with `"trust_note"`), `"pc:<key id>"` (another PC's key —
  check it there; not a tamper verdict). `/api/records-meta` adds
  `logChains: {<array name>: {path, status, entries, unauthenticated,
  detail}}`; `audioLevels` entries carry the same `trust`.
- **P4 — full-case scope.** `verify-audit <case>`, `doctor` and the
  workstation `/api/verify-audit` (badge) check `audit::case_lane_paths`:
  every known lane under `artifacts/**` and `evidence/logs/**`
  (`collect_chain_lanes`) plus any other `*.jsonl` in `evidence/logs`.
  `/api/verify-audit` per-log entries add `path` (case-relative),
  `unauthenticated_lines` and `other_pc_lines`.
- **P5 — fail closed on unreadable key configuration.** A key path that is
  configured explicitly (`FRAMETRACE_AUDIT_KEY_FILE`,
  `FRAMETRACE_AUDIT_KEYRING_FILE`) but missing, or any key path (default
  or explicit) that is a directory, a broken link or not a regular file,
  is an error starting `키 설정을 읽지 못함` (`audit_key::
  KEY_CONFIG_UNREADABLE`) — R8 read these as "no key", silently relaxing
  P0 to P1. Every `verification_keys()` error carries that prefix.
  Readers treat it as keys held with nothing authenticated
  (`LaneRows::key_error`, lane NOT VERIFIED); `verify-audit`, `doctor`,
  `verify-package`, custody, QA and `/api/verify-audit` show it as NOT
  VERIFIED with that wording (I9) — never "chain broken" / FAIL.
  Rotation still creates a keyring at an explicit path that does not
  exist yet.
- **I10.** Unkeyed entries before ANOTHER PC's key-introduction marker
  (the lane's first keyed entry, signed under a key this PC does not hold)
  are `OtherPc { key_id }` — "키 <id> 보유 PC에서 확인" — not "HMAC 없는 줄".
- **QA (I5).** `qa` grades lanes with `lane_trust`; a lane this PC cannot
  authenticate is NOT VERIFIED (the run fails, not a tamper verdict) and
  its anchor keeps the last state it passed with.
- **Sign-offs (I6).** A record that a trusted `signoff-abandoned` entry
  names is never Current, even restored as `review/signoff.json`.

### Lane and case binding (R9-I11)

A keyed append to a known lane adds, inside the bytes `entry_sha256` and
the HMAC cover, `"audit_lane":"<case-relative lane path>"` and — when the
case root's `case.json` names one — `"audit_case_id":"<case_id>"`
(`audit::ENTRY_LANE_FIELD` / `ENTRY_CASE_FIELD`). Unkeyed appends are
unchanged (byte-identical, no binding). Path-aware verification
(`verify_chained_jsonl`, `read_lane_rows`) checks every authenticated
entry that carries a binding against where it was found: another lane, or
another `case_id`, is a verification FAIL ("다른 레인 / 다른 케이스의 항목을
옮긴 것"). Authenticated entries without a binding (written before R9)
still verify and are reported as legacy ("레인·케이스 결합이 없는 이전 형식"
/ a `legacy` warning). Remaining limits:

- The case binding is only as strong as `case.json`'s `case_id`, which is
  not keyed: copying a lane into another case AND editing that case's
  `case.json` to the source's id passes. The lane binding has no such gap.
- Legacy (pre-R9) entries cannot be checked for moves at all.
- A lane moved to a path that is not a known lane is never read as one;
  if read as a lane file anyway, a bound entry there fails.
- Truncation and rollback limits ("Non-goals") are unchanged.

Key introduction (R8-I8): `rotate-audit-key --log <case>` on a PC that had no
key before stamps, into each lane holding only unkeyed entries, an
`{"kind":"audit-key-introduced"}` marker signed by the new key instead of
the rotation marker. Its HMAC covers the chain hash of everything before it,
so that prefix cannot be extended later; those entries are self-consistency
only (never authenticated) instead of "not authenticated". A PC that already
held a key never introduces keying, so a lane stripped of its HMACs stays
NOT VERIFIED there. Rotating without `--log <case>` prints a warning.

### Lane identity, recency, location (R10)

R9's one reader was kept, but consumers still (a) rendered rows and
dropped their `trust` (the carve page), (b) disagreed on WHICH file is a
lane under Windows path spellings, and (c) fell back to an older
authenticated row when the newest one was not usable. R10:

- **P6 — one lane identity.** A root-relative path is compared through
  `audit::lane_rel_key` (`/` separators, no `./`, ASCII-lowercased on
  Windows / macOS, whose file systems ignore case by default) and spelled
  canonically by `canonical_lane_rel`. `collect_chain_lanes` looks every
  `KNOWN_CHAIN_LANES` path up DIRECTLY under the root (R9 walked the tree
  and compared names exactly, missing a lane reached under another letter
  case or through a link the walk did not follow, while the consumers read
  it through the constant path); `case_lane_paths`, the timeline and the
  sound-lane listing use the same identity. `audit::lane_location` makes a
  path lexically absolute (`std::path::absolute`, `.`/`..` removed; then
  `canonicalize` as a fallback, e.g. 8.3 names) before finding its root and
  canonical lane. A **reparse point** (junction or symbolic link — Rust
  reports Windows junctions as `is_symlink()`, verified) on the lane path
  below the case root makes the lane's location unconfirmable:
  `AuditChainVerification::location_unverified`, every entry
  `LineTrust::LocationUnverified` (`unauth`), lane NOT VERIFIED.
- **P7 — recency.** `read_lane_rows` marks a usable entry as superseded
  (`LaneRow::superseded_by`, no longer `usable()`, code `unauth`,
  `trust_note` starting `더 최근 인증 안 된 항목 있음`,
  `trust_superseded_by: <line>`) when a LATER entry about the same record
  (`audit::recency_keys`: `id`, `record_id`, `selector`, `target_path`,
  `output_path`, a recovered inode, the transcode-queue summary) is not
  usable here. Stripping the HMAC of a record's newest entry therefore no
  longer rolls the record back to its previous authenticated entry —
  in sign-off target hashes / paths, validation target resolution, the
  report, the viewer bundle, `/api/records-meta` and the timeline alike.
  The sign-off lane (latest-sign-off rule) and the E01 lane (newest import
  of any trust) keep their own rules; sound lanes have their own reader.
  P1 (no keys here): nothing is unusable except past a chain break, so
  nothing is superseded.
- **P8 — location unknown ≠ tampering.** FAIL needs a key this PC holds to
  refute an entry: an authenticated entry bound to ANOTHER lane / case is
  still FAIL; a bound entry found where no known lane sits, or where the
  root has no `case_id`, and a lane behind a reparse point, are NOT
  VERIFIED (R9: FAIL for the first two).
- **Anchors (I8).** `check_lane_anchor` reads through the lane reader: a
  chain break at or before the anchored entry is "rewritten" (changed); an
  entry up to the anchor that is not usable here (a stripped HMAC, another
  PC's key, a key-configuration error, an unconfirmed location) is the new
  `AnchorStatus::NotAuthenticated` — the sign-off is at most partially
  verified. R9 compared only the recorded `entry_sha256` text, which
  survives stripping the HMAC fields.
- **I6.** An entry under a key this PC does not hold AFTER one this PC
  authenticated is `LineTrust::RewriteNotExcluded` (`unauth`,
  "재작성 배제 불가") — no longer the other-PC (`pc:`) wording.
- **I9.** Key introduction is decided per CASE: a lane without any HMAC in
  a case where another lane holds HMAC entries never gets the
  introduction marker. A keyring path set explicitly
  (`FRAMETRACE_AUDIT_KEYRING_FILE`) that does not exist is created only
  with `rotate-audit-key --init` (a briefly disconnected drive no longer
  reads as "no key before").
- **I10.** The binding is added by the appender, decided from the body's
  top-level JSON keys; a body that names `audit_lane` / `audit_case_id`
  itself is refused.
- **I11.** An authenticated entry without binding keeps `auth` with
  `trust_note` "레인·케이스 결합 없음(레거시)".
- **I12.** Only a usable sign-off-lane entry can contradict a record
  (content mismatch); an unusable one counts as another sign-off entry
  (NOT VERIFIED, "not the latest").
- **I14.** `audit_key::adopt_key_env` (second statement of both `main`s,
  right after the step token) keeps `FRAMETRACE_AUDIT_KEY`,
  `FRAMETRACE_AUDIT_KEY_FILE` and `FRAMETRACE_AUDIT_KEYRING_FILE` and
  removes them from the process environment, so ffmpeg / ffprobe / Sleuth
  Kit / python children never inherit the raw key; the workstation hands
  them back only to its own CLI step children
  (`forward_adopted_key_env`).
- **I15.** Verified on Windows 11: `std` reports an unreachable UNC host
  (os error 53), a missing share (67) and a path under a junction whose
  target is gone (3) all as `ErrorKind::NotFound`. A missing DEFAULT key
  path is therefore "no key" only when its nearest existing ancestor is an
  ordinary reachable folder; a dangling link / junction there, a network
  error or a missing volume root is a P5 key-configuration error. With no
  config directory at all, a warning says the default locations were not
  looked at (stderr once, and in `doctor`).

Remaining limits: P7 keys are field-name based (a new lane or record field
needs its key added to `recency_keys`); case folding assumes the default
case-insensitive volumes on Windows / macOS.

**Implementation notes (what shipped):**

- `src/hmac.rs` — HMAC-SHA-256 hand-assembled over the existing `sha2`
  dependency (RFC 2104; tested against the RFC 4231 vectors). No new crates.
- `src/audit_key.rs` — key resolution in the design's preference order: a
  `--key-source` override, then the rotation keyring's active key
  (`audit-keys.json`, `FRAMETRACE_AUDIT_KEYRING_FILE` overrides the path),
  then a `0600` key file (`$XDG_CONFIG_HOME/frametrace/audit-key` or
  `~/.config/frametrace/audit-key` on Unix, `%APPDATA%\frametrace\audit-key`
  on Windows; `FRAMETRACE_AUDIT_KEY_FILE` overrides the path), then
  `FRAMETRACE_AUDIT_KEY` (hex- or base64-encoded 32-byte material). A present
  but unusable source — world/group-readable file on Unix, malformed or
  wrong-length material — is an error, never a silent downgrade.
- `FRAMETRACE_AUDIT_KEY_ID` names the loaded key and is stamped as
  `entry_hmac_key_id`. Its default is the key's **fingerprint id**
  `kid-<first 8 hex of SHA-256(material)>` (R7-I4). It used to default to
  the shared literal `"default"`, so two PCs' different keys claimed the
  same id and one PC's genuine entries read as forged on the other. See
  "Key ids" below for how old `"default"` entries are still verified.
- `verify-audit` prints the log-level mark (`integrity-keyed` /
  `integrity-structural-only`), keyed-entry counts, and per-log warnings
  (mixed chain, missing key).
- `rotate-audit-key` generates a fresh 32-byte key from OS entropy
  (`getrandom`), registers it as the **active** key in a keyring file
  (`audit-keys.json` next to `audit-key`, `0600` on Unix,
  `FRAMETRACE_AUDIT_KEYRING_FILE` overrides the path), and records the
  previously configured key — whichever source supplied it — as a retired
  keyring entry so its audit entries stay verifiable. `--key-id` names the
  new key (default: its fingerprint id `kid-<sha8>`; keyrings written with
  the older `key-<sha8>` default keep their ids); `--log PATH`
  (repeatable) appends the signed `audit-key-rotate` marker entry keyed
  under the NEW key. R7-I6: a `--log` that is a **case directory** stamps
  the marker into every known chain lane of that case that verifies now
  (the sign-off lane included; for sound measurements only the active
  lane); lanes that do not verify are listed and left untouched.
- `--key-source env:VAR_NAME` / `--key-source file:PATH` (global flag)
  overrides every ambient key source for one invocation. `env:` is the
  documented hand-off from an external secret store — the key is injected
  into the process environment and never touches disk.
- **Still not wired:** OS-keystore integration (macOS Keychain / DPAPI). No
  new heavyweight dependency was taken for it; the `0600` key file and
  keyring remain the portable store, and `--key-source env:` covers
  secret-manager injection. If a keyring crate ever lands in the tree,
  `audit_key` is the single place it plugs in.

## Current chain structure

`audit::append_chained_jsonl` (`src/audit.rs`) appends one JSON object per line
to each `*.jsonl` audit log. Every entry carries two chain fields, appended in
fixed order at the end of the object:

```json
{"kind":"scan-folder", "...": "...", "previous_entry_sha256":"<hex>", "entry_sha256":"<hex>"}
```

- `previous_entry_sha256` — SHA-256 of the **raw previous line bytes**
  (including its own `entry_sha256` field and trailing content, excluding the
  newline). The first entry uses the literal `GENESIS`.
- `entry_sha256` — SHA-256 of the entry as serialized up to and including
  `previous_entry_sha256`, i.e. the line prefix `{...,"previous_entry_sha256":"X"}`
  before the `entry_sha256` field itself is appended.

`verify-audit` re-walks the chain: it recomputes each `entry_sha256` from the
recorded bytes and each `previous_entry_sha256` from the preceding raw line,
and reports torn final lines (a crash mid-append) distinctly from mid-chain
mismatches.

## What the unkeyed chain does and does not prove

The chain is **tamper-evident against accidental damage**: bit rot, torn
writes, a copy cut off mid-line, and naive single-line edits all break
verification. That is already valuable — it proves the log was not corrupted
in storage or transit.

It does **not** detect truncation at an entry boundary: a log whose last N
entries were removed whole is still a valid chain (the remaining prefix
verifies). Truncation is detected only by comparing with an anchor recorded
elsewhere — the schema-v4 finalize sign-off records every lane's entry count
and last `entry_sha256` (see "Truncation anchors" below), and `qa`'s
`audit-chain-anchors.json` does the same between QA runs.

It is **not tamper-evident against a rewriting adversary.** Anyone with write
access to the case directory can modify or drop entries and recompute both
hash fields from scratch, producing a chain that verifies cleanly. The chain
commits to nothing the attacker cannot recompute, because it is keyed by
nothing.

## Threat a keyed chain mitigates

| Threat | Unkeyed chain | Keyed (HMAC) chain |
| --- | --- | --- |
| Torn write / corruption | Detected | Detected |
| Single-line edit | Detected | Detected |
| Full rewrite with recomputed hashes by anyone holding the case directory | **Undetectable** | Detected (attacker lacks the key) |
| Rewrite by an attacker who also holds the key | Undetectable | Undetectable — see non-goals |

The practical scenario: evidence copied to media, case directory shared with a
second workstation, or a package inspected by opposing counsel. Today a
recipient who distrusts the sender gains nothing from `verify-audit`; with a
keyed chain, a log produced under a key the sender never possessed (or that
lives only in the producing machine's OS keystore) cannot be silently
re-written.

## Proposed format

Opt-in, versioned by field presence — no schema flag file needed:

```json
{"kind":"scan-folder", "...": "...", "previous_entry_sha256":"<hex>", "entry_sha256":"<hex>", "entry_hmac_sha256":"<hex>", "entry_hmac_key_id":"<id>"}
```

- `entry_hmac_sha256` — HMAC-SHA-256 over **exactly the same bytes** that
  `entry_sha256` covers (the line through `previous_entry_sha256` + closing
  `}`). Hashing the same signed region means the HMAC is a pure additive
  field; the unkeyed verification rules are untouched.
- `entry_hmac_key_id` — short opaque identifier (`kid-<sha8>` fingerprint
  id by default, `"2026-Q1"`, or the legacy `"default"`). It names the key,
  never contains key material, and enables rotation lookups.

### Key ids (R7-I4)

Whether a mismatching HMAC is a **forgery** (FAIL) or an entry from
**another PC's key** (not verifiable here) depends on whether this PC holds
the key the entry names:

- `kid-<sha8>` names exactly the material with that fingerprint, whatever
  id this PC loaded it under. A held key with that fingerprint that does
  not verify the entry → FAIL.
- The legacy `"default"` names no particular key (every unnamed key used
  it). A `"default"` entry that a key this PC holds verifies is
  authenticated (every configured key is tried as a fallback, so this PC's
  own old `"default"` logs stay verifiable); one that no held key verifies
  is another PC's legacy `"default"` key — not authenticable here, never a
  FAIL (R8-I7: R7 made it a FAIL when another `"default"` entry of the same
  log verified here, which broke logs two PCs continued). Existing keyrings
  (any ids, including `"default"`) load unchanged.
- Any other id (`2026-Q1`, `key-<sha8>`) is compared as text.

Field order stays append-only at the tail of the object so
`entry_without_recorded_hash` style byte-surgery on the signed region keeps
working.

## Key storage

In order of preference; the first available source wins for **appends**, and
**verification** tries the union of all configured sources:

1. **`--key-source` override** (per-invocation): `env:VAR_NAME` reads
   hex/base64 key material from that environment variable — the documented
   hand-off for an external secret store (Vault, sops, a CI secret), since
   the key material never touches disk — and `file:PATH` reads a key file at
   an explicit path. `FRAMETRACE_AUDIT_KEY_ID` still names the loaded key.
2. **Rotation keyring.** `audit-keys.json` beside the single-key file
   (`FRAMETRACE_AUDIT_KEYRING_FILE` overrides the path), `0600` on Unix:

   ```json
   {"version":1,"active":"2026-Q2","keys":{"2026-Q1":"<hex>","2026-Q2":"<hex>"}}
   ```

   `active` names the id that signs new appends; every other id is a retired
   key kept so its entries stay verifiable. Written by `rotate-audit-key`;
   hand-editable, but a present-but-broken keyring (bad JSON, unknown
   `active`, malformed material, loose permissions) is a hard error, never a
   silent downgrade.
3. **OS-protected store → `0600` file fallback.** The design's preference
   was DPAPI / macOS Keychain; that wiring is not shipped (no heavyweight
   keystore dependency was taken). The portable store is the `0600` key file
   under the user's config dir (`~/.config/frametrace/audit-key` or platform
   equivalent) and the keyring above. The key never lives inside the case
   directory — storing it next to the log would defeat the control entirely.
4. **Environment variable.** `FRAMETRACE_AUDIT_KEY` (hex- or base64-encoded
   32-byte key) for CI, portable installs, and multi-machine cases where the
   examiner deliberately manages the key. Documented as the weakest option:
   any process running as the examiner can read it, matching the existing
   loopback threat model (`docs/security-review.md`).

When no key is configured, append behavior is unchanged: entries are written
without the HMAC fields and no warning is emitted (keyed mode is opt-in).

## Backward-compatible verification

`verify-audit` must handle three log shapes:

| Log contents | Verification result |
| --- | --- |
| Entries without `entry_hmac_sha256` (all logs written before this feature, or written with no key configured) | Structural chain verified; report marks the log **"integrity-structural-only"** — the current documented limitation, made explicit in output. |
| Entries with `entry_hmac_sha256`, key available | Structural + HMAC verified; report marks **"integrity-keyed"**. |
| Entries with `entry_hmac_sha256`, no key on this PC | Structural chain verified; report warns **"keyed entries present but key unavailable — authenticity unverified"** (still a pass with a loud caveat — P1 self-consistency only — so a log moved off its producing machine remains inspectable). |
| Mixed keyed/unkeyed entries | Per-entry result; the log-level mark degrades to structural-only with a mixed-chain warning. |
| Entries under a key id this PC does not hold (R6-I4), none after an entry this PC authenticated | Structural chain verified; warning "signed under audit key id(s) … that this PC does not hold — not a tampering verdict"; never graded keyed. R8 P0/P2: on a PC holding keys the lane is **NOT VERIFIED** ("이 PC에서 인증 불가 — 키 <id> 보유 PC에서 확인"), never FAIL. |
| Unkeyed entries, keys held here (R8 P0) | Structural chain verified; **NOT VERIFIED** — an unkeyed entry may be a stripped one, so it is never trusted where keys are held (whether or not the lane holds keyed entries). |
| An entry under a key id this PC does not hold AFTER an entry this PC authenticated (R7-I3) | **NOT VERIFIED** — "이 PC에 없는 키 — 재작성 배제 불가": this PC held the log's key, so the tail may have been re-chained under an invented key id. `verify-audit` exits non-zero, `verify-package` reports NOT VERIFIED, the report's chain status is `not-verified`, and a sign-off in such a lane is not verifiable. No in-log rotation marker excuses it — a marker naming the new id would be signed by that same unknown key. |

**Per-entry use (R7-I1, R7-S10, R8 P0).** Where a single entry is relied on
— a sound measurement, an E01 import record or the completion marker bound
to it, a sign-off entry — and this PC holds audit keys (R8: whatever the
case files say — "in a keyed case" is no longer a condition), only an entry
whose HMAC is **authenticated here** counts. An unkeyed entry, an HMAC
under a key this PC does not hold (P2: "check on the PC holding it"), or
under a retired key nothing later proves, is not. Without keys on this PC
these are self-consistency only, and say so (P1). A sound-lane continuation
marker is judged for STRUCTURE only (R8-I6); its lane's lines are judged
per line.

The unkeyed fields are never removed: keyed logs still verify structurally on
any machine, and the HMAC layer adds rather than replaces a check.

## Key rotation

`frametrace rotate-audit-key [--key-id <id>] [--log <path>]...` implements
the rotation rules:

- It generates a new 32-byte key from OS entropy (`getrandom`), writes it to
  the keyring as `active`, and records the previously configured key —
  keyring, file, env, or `--key-source`, whichever supplied it — as a retired
  keyring entry. Rotating with nothing configured simply enables keying
  (`from: null`).
- New appends always use the **current** (active) key and stamp its
  `entry_hmac_key_id`.
- Verification accepts every known key: the verifier's key set is the union
  of the keyring (active + retired), the single-key file, the env var, and
  any `--key-source` override. Each entry's declared `key_id` is tried
  first, then every known key as fallback, so a renamed id cannot brick old
  entries.
- Rotation is itself audited: each `--log PATH` gets a signed marker entry
  (`{"kind":"audit-key-rotate","from":"<old id>","to":"<new id>"}`) keyed
  under the **new** key, so a gap between last-old-key and first-new-key
  entries is explainable in-log. Audit logs are per-case, so the flag is
  repeatable — pass every log that grows across the boundary, or the case
  directory itself (R7-I6: every verifying lane of the case). The marker
  also matters for older entries: an entry signed by a now-retired key is
  only authenticated when a newer key's entry follows it in the same log,
  so without the marker every earlier sign-off reads "cannot be verified"
  after a rotation. R8-I10: a lane named twice (a case folder and one of its
  lanes, another spelling of the path) gets one marker; a lane that cannot
  be appended to does not stop the others — every failure is listed and the
  command exits non-zero saying the key WAS rotated. R8-I8: without any
  `--log <case folder>` the command warns; on first keying, lanes holding
  only unkeyed entries get the key-introduction marker (see "Trust policy").
- Old keys are retained in the keyring for as long as the case lives;
  deleting a retired key downgrades verification of its entries to
  structural-only, never to failure.

## Non-goals

- **Not a signature.** HMAC is symmetric: anyone holding the key can forge
  entries. This raises the bar from "anyone with the case directory" to
  "anyone with the key", nothing more. Asymmetric signing is a larger legal
  and operational commitment and out of scope for M5.
- **Not a write-once guarantee.** Truncating the log back to an earlier
  prefix remains possible regardless of keying, and the chain alone does not
  reveal it. The sign-off anchors below reveal truncation of anything
  recorded before the sign-off; package-level manifests (`manifest.sha256`)
  and external timestamping remain the controls for the rest.
- **Not rollback detection (R7-I7).** An older genuine sign-off record
  restored over a newer one is reported "not the latest" only while the
  newer sign-off entries are still in `signoff-log.jsonl`. Cutting the
  lane's tail back to the older entry is truncation, and truncation of the
  sign-off lane after the restored record is NOT detected; the wording
  ("newest sign-off entry in the lane as found") claims no more.

## Truncation anchors (sign-off schema v4)

A finalize sign-off (`review/signoff.json`, schema 4) records, for every
chained lane under `artifacts/` and `evidence/logs/`, the entry count and the
`entry_sha256` of the last entry at sign-off time. `make-report` and
`verify-package` check each anchor: the lane must still hold that entry at
that position (later appends are fine). A removed tail, a rewritten prefix
or a deleted lane reads "changed since the sign-off". The sign-off itself is
appended to `evidence/logs/signoff-log.jsonl` (HMAC-keyed when a key is
configured), naming the record by the SHA-256 of its bytes.

R7-I7: a v4 record also states `signoff_log_entries` — how many entries the
sign-off lane held when it was signed. Its own lane entry must sit right
after them (line `signoff_log_entries + 1`); anything else (the lane before
it cut or re-written) is a contradiction → "changed". Records written
before R7 lack the field and are not position-checked.

R7-I2: only `schema_version` 4 is checked as v4. A higher schema is
rejected (Unbound / NOT VERIFIED, never read through a marks-only
shortcut), and once the sign-off lane holds entries an older-schema record
is at most Unbound / NOT VERIFIED — re-labelling a v4 record as v3 (whose
`content_sha256` anyone can recompute from the marks) no longer skips the
lane. R8-I2 (P0): on a PC holding audit keys an older-schema record is
never Current at all — deleting the sign-off lane no longer restores it.

R8-I9: the position check is "this record's entry is the FIRST sign-off
entry at or after line `signoff_log_entries + 1`" — other events appended
in between (a rotation marker racing the finalize) no longer contradict it.
A finalize that recorded its sign-off entry but then failed appends a
`{"event":"signoff-abandoned","signoff_record_sha256":…}` entry
(`report::record_signoff_abandoned`); a TRUSTED one (authenticated here when
keys are held) makes that sign-off entry display-only: it does not make the
previous sign-off "not the latest", nor cap an older-schema record.

## Retired keys

Each key the keyring retires gets a retirement time (`retired` map in
`audit-keys.json`, written by `rotate-audit-key`). A retired key only
authenticates entries that predate its retirement: an entry it signed is
refused when the entry is dated later (its event-time field), when the same
log recorded the key's `audit-key-rotate` marker earlier, or when a newer key
generation already signed an earlier entry of that log. Keyrings written
before retirement times were recorded load their non-active keys as retired
at an unknown time (only the positional rules apply).
- **Not a replacement for chain-of-custody documentation.** The legal review
  that gates this feature decides how much weight a keyed log carries in
  testimony; the design deliberately exposes the "integrity-structural-only"
  wording so unkeyed logs are never oversold.
