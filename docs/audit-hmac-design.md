# Audit Chain HMAC Design

**Status: implemented (opt-in).** Keyed appends and verification ship behind
key configuration — with no key configured, behavior is byte-identical to the
unkeyed chain. Legal/practice review still governs how much weight a keyed log
carries in testimony; the code deliberately reports `integrity-structural-only`
so unkeyed logs are never oversold. This document records the chain format, the
residual threat, and the keyed format as shipped.

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
- `FRAMETRACE_AUDIT_KEY_ID` names the loaded key (default `"default"`) and is
  stamped as `entry_hmac_key_id`.
- `verify-audit` prints the log-level mark (`integrity-keyed` /
  `integrity-structural-only`), keyed-entry counts, and per-log warnings
  (mixed chain, missing key).
- `rotate-audit-key` generates a fresh 32-byte key from OS entropy
  (`getrandom`), registers it as the **active** key in a keyring file
  (`audit-keys.json` next to `audit-key`, `0600` on Unix,
  `FRAMETRACE_AUDIT_KEYRING_FILE` overrides the path), and records the
  previously configured key — whichever source supplied it — as a retired
  keyring entry so its audit entries stay verifiable. `--key-id` names the
  new key (default `key-<sha256 fingerprint prefix>`); `--log PATH`
  (repeatable) appends the signed `audit-key-rotate` marker entry keyed
  under the NEW key.
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
- `entry_hmac_key_id` — short opaque identifier (`"default"`, `"2026-Q1"`,
  a key fingerprint prefix). It names the key, never contains key material,
  and enables rotation lookups.

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
| Entries with `entry_hmac_sha256`, key missing | Structural chain verified; report warns **"keyed entries present but key unavailable — authenticity unverified"** (still a pass with a loud caveat, so a log moved off its producing machine remains inspectable). |
| Mixed keyed/unkeyed entries | Per-entry result; the log-level mark degrades to structural-only with a mixed-chain warning. |

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
  repeatable — pass every log that grows across the boundary.
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

## Truncation anchors (sign-off schema v4)

A finalize sign-off (`review/signoff.json`, schema 4) records, for every
chained lane under `artifacts/` and `evidence/logs/`, the entry count and the
`entry_sha256` of the last entry at sign-off time. `make-report` and
`verify-package` check each anchor: the lane must still hold that entry at
that position (later appends are fine). A removed tail, a rewritten prefix
or a deleted lane reads "changed since the sign-off". The sign-off itself is
appended to `evidence/logs/signoff-log.jsonl` (HMAC-keyed when a key is
configured), naming the record by the SHA-256 of its bytes.

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
