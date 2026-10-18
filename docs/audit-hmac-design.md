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

### Missing rows, secret adoption, canonical lane files (R11)

R10's P7 only covered "a newer entry of the SAME record". Run-level
records (a re-hash run and its completion marker) and chain breaks have no
such key, and some consumers got BETTER results when an unusable row
dropped out (R10 review: one stripped row turned the custody statement
green; one stripped completion marker rolled the re-hash result back to an
older run). R11:

- **P9 — a missing row is not good news.** If a run holds an entry that is
  not usable here, or the lane's chain is broken, no "all match / fully
  complete / current value" claim comes out of that lane — the custody
  banner, the API summary and the report alike.
  - `anomaly::hash_verify_assessment` is THE decision for
    `hash-verify.jsonl` (custody section 5, `/api/verify-sources`): the
    result is the newest completed run, but there is **no result** when a
    run newer than it holds an unusable entry (I2: a stripped completion
    marker; R10 then showed the previous run as "latest") or the chain is
    broken; the result is **not verified** when it or a newer run holds an
    unusable entry, its marker's `records` count differs from its usable
    records (I1), or the lane is NOT VERIFIED here. Only a verified run
    with every file compared and equal gets the green banner.
  - `recency_keys` adds `run_id=<id>` to `hash-verify` / `hash-verify-run`
    entries (a stripped marker supersedes its run's records).
  - The report's anomaly section: an unusable anomaly entry after the
    newest usable `anomaly-scan-run` marker → "현재 결과 없음(결과 아님)",
    no older run as current.
  - **I4:** in a lane whose chain does not verify, the entries BEFORE the
    break are not usable either (`LaneRow::lane_broken_at`, code `unauth`,
    `trust_note` starting `체인 파손 레인`, `trust_lane_broken_at: <line>`).
    R10 used the verified prefix, so editing a record's newest entry rolled
    it back to the previous value (P7 defeated). A sign-off anchored before
    the break is `AnchorStatus::NotAuthenticated` (R10: Intact). A torn
    final line counts as a break too — repair it with `frametrace
    repair-lane` (R12-I3, below) to use the lane again (fail closed; R10
    kept the prefix).
  - **Correction (R12):** R11's rule for `hash-verify.jsonl` attributed an
    unusable line to a run by that line's own `run_id` and skipped it when
    its `event` was not a re-hash event. Both are attacker-controlled on an
    unsigned line, so replacing the newest run's completion marker (the
    last line — the chain stays intact when `entry_sha256` is recomputed)
    with an unsigned line claiming `event:"x-note"` or an OLDER run's id
    brought the older all-match run back as the result (`is_result:true`,
    `unauthenticated:0`). The R11 claim "a newer run holding an unusable
    entry withholds the result" was only true for unusable lines that
    told the truth about themselves. Superseded by P12.

### Position, not content (R12)

- **P12 — an unusable line's content is never used for a decision; only
  its position.** `anomaly::runs_from_lane` groups USABLE entries only;
  the result candidate is the run whose usable completion marker sits
  LAST in the lane, and **any** unusable line after that marker (whatever
  its `event`, `run_id`, or whether it parses) withholds the result
  (`is_result:false`) and is counted in `unauthenticated`. With no usable
  completed run, any unusable line withholds. Unusable lines from the
  candidate run's first entry on make it not `verified`. Unusable lines
  BEFORE the candidate run (an older run under another PC's key) do not
  withhold it (보류하지 않는다) — *corrected in R13 (I6): this said "do not
  affect it", but they still appear in the lane's `unauthenticated` total
  and `detail`; they only neither withhold the result nor count against
  the candidate run.* Remaining content use of unusable lines: P7 `recency_keys`
  still reads them, but only to make OLDER usable entries unusable — it
  can only withhold, never restore.
- **I3 — `frametrace repair-lane <lane> [--dry-run]`.** A lane whose chain
  does not verify is cut back to the prefix the lane reader accepts (the
  first `ChainBroken` row), under the lane's exclusive lock and only if
  the file is byte-identical to what was analysed. The cut bytes are
  saved beside the lane as `<lane>.repair-<unix>.removed` (create-new,
  fsync'd; not a `.jsonl`, so never read as a lane), then a
  `lane-repair` marker is appended (HMAC-signed when this PC has a key)
  with `kept_entries`, `kept_bytes`, `kept_sha256`, `removed_lines`,
  `removed_bytes`, `removed_sha256`, `removed_saved_as`,
  `lane_sha256_before`, `failure`. A usable marker makes the lane
  "수리됨" (`LaneRows::repairs`, `chain_json` `repaired`/`repairs`,
  `detail`, `verify-audit` "repaired:" lines). Repair is no more than a
  tail deletion an attacker could do by hand (I9) — except that it is
  disclosed; consumers that rely on "the newest run" treat a usable
  `lane-repair` marker after the result run's marker like an unusable
  line (the re-hash result is withheld until a new run). Refused: a lane
  on a link / junction, an unreadable key configuration, and a sound
  lane that a continuation lane already follows (its continuation marker
  checks it by hash — `audio_level::DO_NOT_REPAIR_LANE`). Every
  "repair the lane" message names the command
  (`audit::REPAIR_LANE_HINT`).
- **I4.** `qa anomalies` prints `hash-verify-run-id: <id>` (first line,
  before any finding text); `/api/verify-sources` compares it with the
  result run's id — a different id, or none, is `is_result:false`.
- **I5.** `--key-source env:VAR` / `FRAMETRACE_KEY_SOURCE=env:VAR` naming a
  system variable (`audit_key::SYSTEM_ENV_VARS`: `PATH`, `SystemRoot`,
  `TEMP`, `APPDATA`, `HOME`, …, plus `LC_*` / `XDG_*`; ASCII case ignored)
  is refused and never adopted — R11's adoption removed it from the
  environment of every tool a step ran.
- **I2.** The timeline emits a `db/carve_results.json` snapshot artifact as
  `carved-candidate` only when a usable carve-lane entry names the same
  `(id, output_path)` (it is then the lane's event); otherwise
  `carved-candidate-unaudited`. A carve lane whose chain does not verify
  makes the snapshot unused.
- Tail deletion remains detectable only through the sign-off anchors (I9).
- **P10 — a secret leaves the environment as soon as it is read.**
  `audit_key::adopt_key_env` also adopts the variable an `env:VAR` key
  source names — from the process's `--key-source` argument (either
  spelling, before `--`) and from the workstation's
  `FRAMETRACE_KEY_SOURCE` (`audit_key::key_source_env_vars`). The value is
  kept, removed from the process environment, read by
  `install_source_override`, and handed back only to FrameTrace's own step
  children (`forward_adopted_key_env`; they get `--key-source env:VAR` and
  adopt it again). R10 left that variable in the environment (I3).
- **P11 (lane part, I6/I7).** The timeline's `.jsonl` test uses
  `lane_rel_key` (so `X.JSONL` is the lane on Windows / macOS). A file
  spelled unlike its canonical lane name that is NOT the file the
  canonical spelling opens (a copy beside the real lane in a
  case-sensitive folder) is not the lane: `lane_location` reports it
  location-unverified (`정규 표기와 다른 이름의 레인 파일`, NOT VERIFIED),
  `case_lane_paths` lists it as itself, and the timeline always reads the
  real lane (R10 read whichever spelling sorted first and skipped the
  other). `audit::is_canonical_lane_file` decides.
- **I8.** Clip-family lanes (`artifacts/clips/export-log`,
  `artifacts/proxies/proxy-log`, `artifacts/thumbnails/thumbnail-log`,
  `transcode-queue-item` entries) key P7 on `output_path` only — their
  `selector` names the source that many clips share.
- **I5.** The carve page's run summary (scan complete, candidate limit,
  source size, warnings) comes from `db/carve_results.json`, which no
  audit lane covers: those metrics read "감사 안 됨" and are never
  ok-coloured.
- **I9 — the limit that remains: deleting rows.** Every rule above acts on
  rows that are still in the lane. Removing the TAIL of a lane (the newest
  run's records and marker, the newest entry of a record) leaves a valid,
  shorter chain — the older run or value then really is the newest the
  lane shows, on any PC, keyed or not. Only the sign-off truncation
  anchors (below) reveal it, and only for what was recorded before the
  sign-off; after the last sign-off, a cut tail is undetectable from the
  lane alone (package manifests and external timestamping are the
  controls). Deleting a row in the MIDDLE breaks the chain (I4: nothing
  from the lane is used).

### Unmarked newer runs, repairs for every reader, allow-listed key variables (R13)

- **P16 — a usable run without a completion marker after the result run
  withholds the result too.** P12 withheld only for UNUSABLE lines after
  the last usable completion marker. Deleting that marker itself (the
  last line — no key, no forging; the shorter chain verifies) left the
  newest run's usable records looking "interrupted", and R12 let the
  previous run stand: R1 match + marker, R2 mismatch + marker, delete
  R2's marker → `run_unix:100, mismatched:0, verified:true,
  is_result:true` and the green "All … equal" custody banner, with R2's
  authenticated MISMATCH row still in the lane (reviewer PoC). Now
  `anomaly::assess_runs`: every run without a usable completion marker
  whose last usable entry sits after the last usable marker (every such
  run when none completed) is in `HashVerifyAssessment::interrupted` and
  WITHHOLDS the result — reason prefix "표식 없는 더 새 실행 있음"
  (`anomaly::NEWER_UNMARKED_RUN_REASON`). Its usable `mismatch` rows are
  surfaced, never hidden: `HashVerifySummary.{newer_runs, newer_records,
  newer_mismatched, newer_mismatches}` (and `/api/verify-sources`), the
  custody statement lists them in a red table under the "No current
  re-verification result" banner. An interrupted run positioned entirely
  BEFORE the result's marker (an older crash) still does not withhold.
  Withhold reasons now accumulate (unusable tail; lane-repair after the
  result; unmarked newer run) in `withheld` / `withheld_reason`. The
  case report's anomaly section applies the same rule: a newer run
  without its `anomaly-scan-run` marker is no longer "최근 실행이 중단되어
  이전 완료 실행을 현재로 표시" — there is no current result and that run's
  findings are shown with the notice. A cut-short scan therefore also
  reads "no current result" until the next completed run (the cost of not
  being able to tell a deleted marker from a crash).
- **I2 — "repaired after" for every newest-record reader.**
  `audit::LaneRows::repaired_after(index)`: a usable `lane-repair` marker
  sits after lane index `index`, so a value at or before it is UNKNOWN.
  Applied to: the re-hash result (as R12), the sign-off verifier
  (`signoff_audit_record_with_keys`: NOT VERIFIABLE
  "…수리됨(lane-repair) … 이 서명이 최신인지 알 수 없습니다";
  `SignoffLaneState.repaired_after` for the finalize recovery), the
  report's validation section (a target's newest validation before the
  marker → "최신 여부 알 수 없음", not counted as current; also the
  evidence table's `validationForRow` → `validation-unknown-after-repair`)
  and the report's current anomaly run. Usable `lane-repair` markers are
  no longer shown as rows of the lane's own kind in the report
  (`usableLog` drops them; the chain shows "수리됨").
- **I3 — key-source variables are an allow-list.** R12's deny-list
  (`SYSTEM_ENV_VARS`) let `DYLD_INSERT_LIBRARIES` through (measured).
  `audit_key::key_source_var_refusal`: the name must be ASCII letters,
  digits and `_`, start with `FRAMETRACE_` or end with `_SECRET` / `_KEY`
  (ASCII case ignored), and is refused anyway when it starts with
  `DYLD_` / `LD_` or is a `SYSTEM_ENV_VARS` name (`FRAMETRACE_KEY_SOURCE`,
  `FRAMETRACE_AUDIT_KEY_ID`). A refused name is never adopted (removed
  from the environment). CLI `--key-source env:X` exits 1 ("… — refused");
  the workstation's `FRAMETRACE_KEY_SOURCE=env:X` fails to start.
- **I4.** A carve-lane entry without `carved_unix` (old format) that the
  timeline times from `db/carve_results.json` says so in its detail
  ("시각 감사 안 됨 …", `timeline::CARVE_TIME_UNAUDITED_NOTE`); the
  candidate itself is still the lane's.
- **I5 — the `.removed` archive is checked.** Every lane read hashes the
  archive a usable `lane-repair` marker names (a plain file name in the
  lane's folder only; never a path elsewhere, never a link) against the
  marker's `removed_bytes` / `removed_sha256`: `audit::RemovedArchive`
  `match` / `missing` / `differs`. In `chain_json`:
  `repairs[].removed_archive` and lane-level `removed_archive` (worst;
  `null` without a repair); in `detail` and `verify-audit`
  (`removed-archive: …` line, plus a `warning:` line when not `match`;
  the exit code is still the chain's).
- Tail deletion past the last sign-off remains undetectable from the lane
  alone (I9); P16 only stops a deleted MARKER from promoting an older run.
  (R14: see below — P16 was itself positional, and the two-line deletion
  is now caught by the `hash-verify-head` anchor.)

### Run spans, a scan lock, the re-verification lane head (R14)

- **P20 — runs are judged by span, not position.** R13's P16 asked
  whether an unfinished run's LAST usable row sits after the result
  run's marker. Interleave two runs — A (match), B (mismatch), A's marker,
  B's marker — and delete B's marker (the last line, no key): B's last row
  is before A's marker, so A came back green with B's MISMATCH hidden
  (reviewer PoC). Now an unfinished run is "newer" also when its FIRST
  usable row is after the result run's first row, or its `run_unix` is
  greater (`anomaly::assess_runs`). An unfinished run whose first row and
  run time are both before the result run (an older crash) still does
  not withhold. The case report's anomaly grouping applies the same rule
  (`flushUnmarked` puts an interleaved unmarked run BEFORE the completed
  run whose marker ended it; it is newer when its first entry is after
  that run's first entry or its `scanned_unix` is greater).
- **One scan at a time per case.** `anomaly::scan_case` (the only writer
  of the anomaly and re-verification lanes) holds an fs2 exclusive lock
  on `db/anomaly-scan.lock` for the whole scan; a second scan of the same
  case is refused ("다른 이상 징후 검사·원본 재검증이 실행 중"), writing
  nothing. Legitimate interleaving therefore no longer happens, and an
  interleaved lane is either a pre-R14 race or an edit.
- **I3 — the `hash-verify-head` anchor.** Deleting the newest run's
  record AND its marker (two lines; the shorter lane still verifies) made
  the previous run green again. Each completed live re-hash run now
  writes its marker with `"head_in":"evidence/logs/signoff-log.jsonl"` and
  then appends to the SIGN-OFF lane (keyed when a key is configured):

  | `event` | fields |
  |---|---|
  | `hash-verify-head` | `lane` (`evidence/logs/hash-verify.jsonl`), `run_id`, `run_unix`, `entries` (the re-verification lane's entry count), `last_entry_sha256` |

  `report::hash_verify_head_check(root, result_run_id, marker_wants_head)`
  takes the newest USABLE head and checks it with the D13 anchor check
  (`audit::check_lane_anchor`): `intact` (later appends fine), `truncated`
  (fewer entries — **"꼬리 잘림 — 결과 없음"**, also a missing lane),
  `rewritten` (another entry at that position), `not-authenticated`
  (the lane up to the head is not usable here: NOT VERIFIED, not
  withheld), `unknown` (an unusable sign-off-lane line after the newest
  head — it may be a newer head, P12: its content is never read; a
  `lane-repair` of the sign-off lane after it; a malformed head; or the
  result run's marker says it anchored itself but no usable head names
  it), `absent` (no head ever — cases from before R14: no check, the
  custody statement says the tail cut cannot be detected). `truncated`,
  `rewritten` and `unknown` WITHHOLD the result
  (`HashVerifyAssessment.head`, `HashVerifySummary.head_status`); every
  reader of the assessment follows — the custody statement (a red
  "TAIL CUT" banner) and `/api/verify-sources` (`head_status`,
  `withheld_reason`). `verify-package` checks the packaged lanes the same
  way (subject `evidence/logs/hash-verify.jsonl`: PASS intact, FAIL
  truncated/rewritten, NOT VERIFIED unknown/not-authenticated; nothing for
  a package without a head). The next completed run anchors the lane
  again. Head entries are not `signoff` entries: the sign-off position
  check, "not the latest" and finalize pairing ignore them.
  **Limit:** cutting BOTH lanes' tails (the re-verification lane's last
  run and the sign-off lane's last head) still restores the previous
  run — the same "tail past the last anchor" limit as I9; a later
  sign-off's v4 anchors or a finalize package's copy are the evidence
  then.
- **I4.** The report's anomaly section: a lane written by a
  marker-writing scanner (its entries carry `schema_version`) with NO
  completed run has no current result (every marker may have been
  deleted) — R13 showed the last unmarked run as "현재 결과". Legacy logs
  without markers or schema still group by time. (The re-verification
  lane already withheld in this case, R13.)
- **I5 — "repaired after" and P7 for the remaining newest-record
  readers.** The E01 lane's newest full import of an image positioned
  before a usable `lane-repair` marker is refused
  (`e01::E01_REPAIRED_AFTER_NOTE`, re-import) for reuse and for the
  completion-marker check. Sound measurements: a measurement before a
  usable repair marker of its lane is `chain_unverified`
  (`audio_level::AUDIO_REPAIRED_AFTER_NOTE`); and P7 now applies — a
  NEWER result that is not usable here (stripped HMAC, another PC's or a
  forged key id, a line written without a key in a keyed case) replaces
  the older usable one within its lane and from a proven continuation
  lane, so the record reads "unverified" instead of rolling back to the
  older level (R6-I3/R7-I1 kept the older one). An unproven (planted)
  continuation lane still never replaces a verified measurement. The
  transcode queue summary is the lane's NEWEST line only (an unusable
  newer line or a repair marker → no summary).
- **I6.** `audit_key::APP_ENV_VARS`: the `FRAMETRACE_*` variables the app
  itself reads (token, instance file, key/keyring file paths, UI and test
  switches, …) are refused as key sources even though the prefix is
  allowed — adopting one removed it from the process and its children.
  `FRAMETRACE_AUDIT_KEY` (key material itself) stays allowed.
- **I7.** The `.removed` archive is hashed through ONE handle opened
  without following a link (Windows `FILE_FLAG_OPEN_REPARSE_POINT` +
  reparse attribute check; Unix the handle's inode must be the `lstat`'s),
  and a verdict is cached per (path, size, mtime ns, recorded sha256) once
  the mtime is ≥ 2 s old (racy timestamps are always re-hashed). Limit: a
  same-size rewrite that restores the mtime exactly is not re-hashed
  within the process (the cache lives for one process).
- **I8.** Timeline events carry `"time_unaudited": true|false`
  (`TimelineEvent.time_unaudited`) — true for a carve event timed from
  `db/carve_results.json` (its lane entry has no `carved_unix`); the
  detail text keeps `CARVE_TIME_UNAUDITED_NOTE`.

### The head is the scan's last line and covers every result lane (R15)

- **P22 — written last, both lanes.** R14 wrote the head from inside the
  re-verification writer, BEFORE the anomaly rows, and anchored only
  `hash-verify.jsonl`: cutting the newest anomaly run's tail (its rows and
  marker, no key) left the anomaly lane `keyed` and the report showed the
  previous run as "현재 결과" (reviewer PoC 5). Now `anomaly::scan_case`
  draws ONE id per scan (`<scanned_unix>-<4 hex>`: the re-verification
  `run_id` when it re-hashes, and the anomaly marker's new `scan_id`; the
  marker also carries `"head_in":"evidence/logs/signoff-log.jsonl"`) and
  calls `report::record_scan_head` as its LAST step — for every scan,
  stored-hash scans included:

  | `event` | fields |
  |---|---|
  | `hash-verify-head` (`schema_version: 2`) | `scan_id`, `run_id` (null without re-hash), `run_unix`, `lanes` = `{ "evidence/logs/hash-verify.jsonl": {entries, last_entry_sha256}, "evidence/logs/anomaly-log.jsonl": {…} }` (`entries: 0`, `last_entry_sha256: null` for an empty lane); plus the R14 top-level `lane`/`entries`/`last_entry_sha256` for the re-verification lane when it is not empty |

  `report::lane_head_check(root, HeadLane, result_id, marker_wants_head)`
  is the one reader (`hash_verify_head_check` and `anomaly_head_check`
  wrap it; the anomaly result run is the lane's newest usable
  `anomaly-scan-run` marker, its `scan_id`/`head_in` are the result id /
  "wants head"). An R14 head (no `lanes`) anchors the re-verification lane
  only — for the anomaly lane it is `absent`. The report's anomaly section
  reads the check from `auditChains.anomaly.head` (`{status, reason_code,
  withhold_reason, detail}`, filled by `make-report`'s
  `report_audit_chains_json`) and withholds "현재 결과" on `truncated` /
  `rewritten` / `unknown`.
- **P22 — no usable head is not "absent" when something could hide one
  (I1).** Reviewer PoC 1: pre-R14 run A (match, no `head_in`), R14 run B
  (mismatch, head H_B); strip H_B's `entry_hmac_*` fields (it is the
  sign-off lane's last line, so the chain still passes — or a keyed line
  follows and the chain breaks there) and cut B's two rows from the
  re-verification lane: no usable head was left, R14 said `absent` and A
  was green. Now, with no usable head, a sign-off lane with a chain
  failure (`chain-failed`), ANY line this PC cannot use (`unusable-line`,
  or `other-pc`, below) or a `lane-repair` marker (`repaired`) is
  `unknown` and withholds — the unusable line may be the stripped head,
  and its content is never read (P12). `absent` remains only for a
  sign-off lane that is missing or wholly usable with no head.
  Consequence: a pre-R14 keyed case whose sign-off lane holds an
  unusable line has no re-verification result until the next scan writes
  a head (lines before a usable head do not matter).
- **I5 — "다른 PC 머리일 수 있음".** When every such unusable sign-off line
  (after the newest usable head, or anywhere with no usable head) is
  signed under a key this PC does not hold (`LineTrust::OtherPc`, not
  superseded, not in a broken lane), the reason is `other-pc`: it may be
  that PC's newer head, not a stripped one — still no result here (a
  newer head is exactly what such a line could be), check on the PC
  holding the key or re-run the scan here. `HashVerifyHeadCheck::Unknown
  { code, why }`; `reason_code()` = `other-pc` | `unusable-line` |
  `chain-failed` | `repaired` | `malformed` | `own-head-missing` |
  `unreadable`.
- **I3 — overlapping completed runs.** Two COMPLETED runs written into
  each other (A row, B row, B marker, A marker — a pre-lock lane): the
  result run's span (first usable row … marker) overlapping another
  completed run's span withholds the result
  (`anomaly::OVERLAPPING_RUNS_REASON` "완료 실행 구간 겹침"). The report's
  anomaly grouping cannot produce overlapping completed runs (a run is the
  rows between two markers), so it has no such rule.
- **I4 — clocks.** R14 rule (c) (an unfinished run with a greater
  `run_unix` is newer) now applies only when the run is not WHOLLY before
  the result run's first row: a crashed run stamped by a clock set in the
  future, positioned entirely before the result, withheld every later
  result forever. Runs stamped more than `CLOCK_SKEW_MARGIN_SECS` (600 s)
  after this PC's clock are "시계 이상" (`HashVerifySummary.clock_note`,
  `clock_anomaly_runs`; a withholding run with such a stamp adds a
  "시계 이상" sentence to the reason). The report marks such an anomaly run
  "(시계 이상: …)" against the viewer's clock and applies the same (c)
  restriction.
- **I6.** The scan lock reports "다른 … 실행 중" only for contention
  (`WouldBlock` or fs2's `lock_contended_error`, Windows
  `ERROR_LOCK_VIOLATION`); any other lock failure is "잠금을 걸지
  못했습니다 …" with the real error.
- **I7.** `verify-package` checks BOTH packaged result lanes against the
  package's own sign-off lane with the package's result run
  (`hash_verify_assessment(package)` / `anomaly_head_check(package)`): a
  package whose result run's own head was cut is NOT VERIFIED
  (`own-head-missing`) — R14 passed `(None, false)` and the older head
  still matched. Subjects `evidence/logs/hash-verify.jsonl` and
  `evidence/logs/anomaly-log.jsonl`, detail "… lane anchor: …".
- **I8.** Index timeline events (file mtime, stale mark, ffprobe
  `creation_time`, all from `db/videos.jsonl`, which no chain covers)
  are `time_unaudited: true`.
- **I9.** A source-scan test checks that every whole `"FRAMETRACE_…"`
  string literal in `src/`, `tests/` is in `APP_ENV_VARS` or a short list
  of key-material / key-source-test names.
- **Limit (unchanged):** cutting the result lane's tail AND the sign-off
  lane's last head still restores the previous run (its marker's
  `head_in` names a head that is still there).

### Heads paired with their own scan; anchors first (R16)

- **P26 — the reference head is the result run's own (I1).** Reviewer
  PoC-A: an R15 scan (v2 head H1), then an R14 build's scan with the same
  key — its v1 head H2 (no `lanes`) is written BEFORE its anomaly rows.
  Cutting the R14 scan's anomaly rows + marker (no key) left H2 as the
  newest head; it does not anchor the anomaly lane → `absent`, and the
  report showed the R15 run as "현재 결과 — 후보 0건". Now
  `report::lane_head_check` pairs: the reference is the newest usable head
  naming the result id — for the anomaly lane a **v2** head whose
  `scan_id` is the result marker's; for the re-verification lane the head
  naming the result `run_id` (v1 or v2). After that head:
  - a v1 head → `unknown` / `mixed-version` (`HEAD_MIXED_VERSION_NOTE`
    "이전 버전 머리 섞임");
  - a head naming another scan → `unknown` / `scan-mismatch`
    (`HEAD_SCAN_MISMATCH_NOTE` "다른 스캔의 머리"); for the
    re-verification lane only a head with a non-null `run_id` counts (a
    scan without re-hash names no run).
  - An anomaly result marker WITHOUT `scan_id` (an older build's run, or
    no marker) while any usable v2 head exists → `mixed-version`.
  - Result marker with `head_in` and no head naming it → `own-head-missing`
    (R15, unchanged).
  Consequence: running an R14 build on a case an R15+ build scanned
  withholds the anomaly result until the next scan with this build (the
  reason says "이 PC에서 다시 스캔하면 새 머리가 기록돼 해소",
  `HEAD_RESCAN_NOTE`). The re-verification result of such an R14 re-hash
  run stays anchored by its own v1 head.
- **P26 — anchors before trailing lines (I3).** Reviewer PoC-B: the
  re-verification lane was shorter than its newest head (truncation
  certain), but one unkeyed line appended to the sign-off lane after the
  head returned `unknown` / `unusable-line` first — the error "꼬리 잘림"
  became a warning. Now step 1 compares the newest usable head that
  anchors the lane (it recorded the most entries — a lane shorter than an
  older head is shorter than it too): `truncated` / `rewritten` is
  returned before the no-head, trailing-line, repair, malformed and
  pairing rules. `intact`, `not-authenticated` and `unreadable` anchors
  are only returned after those rules.
- **I2.** `/api/records-meta` carries the anomaly head as
  `logChains.anomaly.head` — the same `{status, reason_code,
  withhold_reason, detail}` object as the report's
  `auditChains.anomaly.head` (`anomaly_head_check(case).to_json_in(Anomaly)`;
  R20 I1: `anomaly_head_check(case, &lane)` over the same read as the rows),
  for the viewer's anomaly badge. (`logChains.anomalyLog` is unchanged.)
- **I4 — wording, not verdict.** In a shared case another examiner's
  sign-off written without this PC's key (or under a forged id) is still
  an unusable line after the head → still withheld (its content is not
  relied on). The `unusable-line` reason now says which: every such line
  is, by its own shape, another event (`HEAD_OTHER_EVENT_NOTE`
  "구조적으로 다른 이벤트 … (event: …) … 변조 판정 아님") vs some line is
  head-shaped or unreadable ("… HMAC이 제거된 머리 포함"). Both end with
  `HEAD_RESCAN_NOTE`: a scan on this PC appends a new usable head after
  them and clears it. `reason_code` stays `unusable-line`.
- **I5.** `anomaly::scan_case` checks, under its scan lock and before
  writing anything, that each lane it appends to (sign-off, anomaly, and
  re-verification with `--rehash`) takes an append
  (`audit::check_appendable`: a torn last line or an unopenable lane is
  refused with the append's own words). R15 wrote both result lanes and
  then failed on the head in a torn sign-off lane.
- **I6.** `verify-package` (documented in `package.rs`): since R15 the
  anomaly lane has its own anchor line and a re-verification line can be
  NOT VERIFIED where R14 said PASS; since R16 FAIL (TAIL CUT /
  REWRITTEN) wins over an unusable sign-off line after the head, and a
  package whose result run is followed by an older build's or another
  scan's head is NOT VERIFIED (`mixed-version` / `scan-mismatch`) —
  re-scan and re-package.
- **I7.** Timeline `time_unaudited: true` also for an unchained log's
  entry (`unchained-log-entry`), an audit entry not authenticated here
  (`audit-entry-unauthenticated`, carve lane included) and a carve
  artifact only in `db/carve_results.json`.
- **Limit (unchanged):** cutting the result lane's tail AND every newer
  head (the sign-off lane's tail) still restores the previous run.

### Certain is certain, unknown is neutral (R17)

**P27.** An anchor comparison (tail cut / rewritten) is made against the
newest head whose own trust is usable, also before a sign-off chain break
and whatever P7 recency says; nothing unauthenticated is a reason to name
a cause ("not tampering", "older build").

- **I1 — `marker-unusable`.** `anomaly_head_check` now tells the head
  check whether the anomaly lane holds a USABLE `anomaly-scan-run` marker
  at all. `mixed-version` needs one without `scan_id` (an older build's
  run; "이 PC에서 다시 스캔하면 해소"). No usable marker while a v2 head
  anchors the lane — the marker's HMAC stripped (no key needed) or the
  marker deleted — is `unknown` / **`marker-unusable`**
  (`HEAD_MARKER_UNUSABLE_NOTE` "완료 표식을 쓸 수 없음 … 변조 가능성 — 버전
  혼재로 보지 않음"; no rescan note). R16 showed that tamper as a version
  mix.
- **I2 — anchors across a chain break.** Step (1) picks its candidate
  from sign-off head rows with `trust.is_usable()` (R11-I4
  `lane_broken_at` ignored — the rule `check_lane_anchor` already uses);
  rows from the break on are `ChainBroken` and never candidates. A result
  lane shorter than such a head is `truncated` / `rewritten`. R16 used
  `usable()` only, so one chain-breaking line after the heads turned a
  certain tail cut into `unknown` / `own-head-missing`. Steps (2)/(3) still
  use fully usable heads (an `intact` anchor from a broken prefix never
  escapes: with a break there is no usable head, so (2) returns
  `chain-failed` / `own-head-missing`).
- **I3 — neutral wording.** `HEAD_OTHER_EVENT_NOTE` is now
  "모양상 다른 이벤트이나 내용은 인증 안 됨"; the sentence no longer says
  "변조 판정 아님" or names a likely writer — the event name it quotes is
  unauthenticated. Code and verdict unchanged (`unusable-line`, withheld,
  `HEAD_RESCAN_NOTE`). (`other-pc` keeps its P2 wording.)
- **I4.** `audit::check_appendable` opens the lane for append (read +
  append, never create) — a read-only lane is refused up front — and
  `scan_case` runs `preflight_scan_lanes` again right before the first
  result-lane write (re-hashing can take long after the first check).
- **I5.** Re-verification lane, result run without a head of its own (no
  `head_in`, unpaired): a v2 head naming another non-null `run_id` →
  `unknown` / `scan-mismatch` (neutral: "지워졌거나 인증되지 않았거나 이전
  버전 빌드가 그 뒤에 스캔함"). R16 checked pairing only after a paired head.
- **I6.** `make-review`'s `review/data-bundle.js` carries
  `logChains: {anomaly: {head}}` — the same object as `/api/records-meta`
  `logChains.anomaly.head` and the report's `auditChains.anomaly.head`, as
  of that make-review (redacted with the bundle). No other `logChains`
  lane is in the bundle; the inline full page has none.
- **I7 — decision.** `time_unaudited` stays `false` for an audit-lane
  entry usable by self-consistency only (`sc`: no keys on this PC, or the
  pre-key prefix): its time IS the lane's, and the viewer's badge says
  "감사 기록에 없는 시각". New field **`time_self_consistent`** (always a
  boolean) is `true` for exactly those events (audit events and carve
  candidates with their own `carved_unix`): anyone can rewrite such a lane
  with any time. `false` whenever `time_unaudited` is true.

### One withholding verdict for every reader; a version mix needs position (R18)

- **I1 — `mixed-version` needs the marker AFTER the head (P27).**
  Reviewer PoC-A: an older build's usable marker without `scan_id`, then
  an R17 scan (marker with `scan_id` + v2 head); stripping only the newest
  marker's HMAC made the marker selection fall back to the old marker and
  R17 said `mixed-version` ("… 다시 스캔하면 해소"). Now `anomaly_head_check`
  also passes the marker's lane index and whether a line this PC cannot
  use follows it. A usable marker without `scan_id` is `mixed-version` only
  when it lies past the newest usable v2 head's anchored range
  (`index >= entries`) and no unusable anomaly-lane line follows it;
  otherwise `unknown` / `marker-unusable` (`HEAD_MARKER_UNUSABLE_NOTE`;
  the sentence says which: "그 머리가 기록한 범위 안 — 그 머리의 스캔보다
  오래된 실행" or "뒤에 이 PC에서 쓸 수 없는 줄이 있음"; never the rescan
  note). A malformed newest v2 anchor counts as "inside" (fail closed).
- **I2 / I5 — the lane's own P9 verdict, one function.**
  `report::anomaly_lane_withheld(values)` takes the anomaly lane rows
  exactly as every reader receives them (`LaneRows::values_with_trust`,
  `trust` codes) and applies the report's rules: `unusable-after` (an
  entry not `auth`/`sc` after the newest usable `anomaly-scan-run`
  marker, anywhere without one), **`unusable-in-run`** (R18 I5: such an
  entry between the previous usable marker and the newest one, after the
  newest run's first entry or stamped with its `scanned_unix` — a
  legacy lane without a head showed the run as "현재 결과" with the
  finding missing; an older line appended before the run started is not
  the run's, so a rescan clears it), `repaired-after`, `newer-unmarked-run`
  (R13 P16 / R14 P20 / R15 I4 / R14 I4 — position, span, time; no completed
  run in a marker-written lane). It returns `{reason_code, codes, reason}`
  (reason = the report's sentences joined by "; ") or `null`.
  Reviewer PoC-B (server side): a keyed re-hash finding, then ONE
  unkeyed `{"event":"anomaly-scan","kind":"none","selector":"vid_a"}`
  line — P7 makes the real finding `unauth`, the head stays `intact`, and
  the viewer counted nothing. Now `/api/records-meta` and
  `review/data-bundle.js` carry `logChains.anomaly.withheld` (computed
  from the very lane read the `anomalyLog` rows come from — R19 I2: R18
  code read the lane a second time for the bundle, and again for an
  unreadable lane in records-meta; now one read, and a lane that cannot be
  read is `reason_code: "unreadable"` with that read's error, next to an
  empty `anomalyLog`), and the report's
  `auditChains.anomaly.withheld` carries it to the report script, which
  also computes the same rules itself: withheld when either says so. A
  test renders the report with no server verdict and checks the script's
  own verdict and sentences equal the function's on eight lane shapes.
- **I3.** `audit::check_appendable` takes `lock_exclusive` on its handle
  before reading the last line (the append's lock, same handle — Windows
  locks are mandatory), so an append in progress is waited for, not
  called a torn lane.
- **I4.** The `unusable-line` sentence quotes only event names this build
  writes to the sign-off lane (`KNOWN_SIGNOFF_EVENTS`: `signoff`,
  `signoff-abandoned`, `finalize-begin`, `finalize-complete`,
  `finalize-resolved`, `lane-repair`); any other (unauthenticated) name is
  only counted ("알려지지 않은 이름 N개(표시하지 않음)").
- **I6.** `report::bundle_log_chains_json`: the bundle's redacted
  `logChains` must still parse as an object with an `anomaly.head` object;
  otherwise it becomes `head: {status: "unknown", reason_code:
  "bundle-unreadable", …}` and `withheld: {reason_code:
  "bundle-unreadable", …}` (R17 dropped the field and the banner with it).

### Interleaved runs, one read, unquotable key ids (R19)

- **I1 — a flushed row joins the later run with its `scanned_unix`.**
  Interleaved scans (A's finding @100, B's finding @200, A's marker @100,
  B's marker @200): the grouping flushed B's finding as an unmarked run
  placed before A's run, so B's completed run — the newest — read
  "후보 0건" and nothing withheld. Now, after grouping, an unmarked run
  whose finite `scanned_unix` equals that of a LATER completed run
  belongs to it (the nearest such run; its entries go ahead of the run's
  own, lane order). Same rule in `report::anomaly_lane_withheld` and the
  report script (`anomalyRuns`); the agreement test covers five
  interleaved shapes. Merging only moves rows INTO a completed run, which
  moves that run's first position earlier — the span checks
  (`newer-unmarked-run`, `unusable-in-run`) then catch more, never less
  (a crashed run C @300 interleaved before both markers now withholds).
- **I2 — one lane read per reader.** `/api/records-meta`, the review
  bundle and the report's `auditChains.anomaly` build `withheld` from the
  same `audit::read_lane_rows` result as the rows / chain status they
  send (`report::anomaly_lane_read_withheld_json`); the shared module no
  longer reads the lane. (The report's own anomaly rows are still a
  separate read in `make-report`; the report script recomputes the rules
  from those rows and withholds on either verdict.)
- **I3 — a key id is quoted only when it matches `^[0-9a-z-]{1,64}$`.**
  `entry_hmac_key_id` lies outside `entry_sha256` and needs no key to
  write. `AuditChainVerification::line_trust` carries a quotable id
  (fingerprint ids `kid-<hex>` are) or `"unknown"`; trust code
  `pc:unknown`; every sentence (entry note, lane reason, rewrite warning,
  head `other-pc`, sign-off notes, carve trust, report script) says
  "알 수 없는 키 id" instead. A user-chosen id with capitals or spaces
  (`FRAMETRACE_AUDIT_KEY_ID`) is also not quoted — the line still is what
  it is (another PC's key), only the name is not repeated.
- **I4.** The report script finds a run's first / last lane position with
  loops (`Math.min(...arr)` and `splice(..., ...arr)` threw RangeError at
  ~130k entries and blanked the report).

### Head from the same read, findings carry `scan_id` (R20)

- **I1 — the anomaly head comes from the same read as the rows and
  `withheld`.** `report::anomaly_head_check(root, &lane)` takes the
  caller's `audit::read_lane_rows` result: the result-run marker search
  and the head's anchor comparison (`audit::check_lane_anchor_in`) use
  those rows; R19 read the anomaly lane twice more (marker search, then
  `check_lane_anchor`), so a scan in between could make rows, `withheld`
  and `head` describe different lanes. `/api/records-meta` (in
  `lane_values`, next to `anomalyLog` / `withheld`), the review bundle
  (`review_anomaly_lane`) and the report's `auditChains.anomaly` (with
  the chain status) all pass their one read; `verify-package` and tests
  use `read_anomaly_head_check(root)` (one read, then the check). Only the
  sign-off lane is read inside. A race remains between the two lanes (the
  scan writes its anomaly rows, then the head LAST): rows read before a
  scan finished are shorter than the newest head — so that one verdict
  (`Truncated` / `Missing` on the caller's rows) is confirmed against the
  lane as it is now; still short → "꼬리 잘림" (append-only lanes never
  shrink), rewritten → `rewritten`, otherwise `unknown` with
  `reason_code: "lane-changed"` ("읽는 사이에 레인이 바뀜 …", withheld,
  never a tamper verdict, never "intact" for rows not seen). An unreadable
  read is `unreadable` with that read's error. The structural test now
  checks the WHOLE reader files (R19 cut them at the first
  `#[cfg(test)]` — line 87 of records.rs) and that every
  `anomaly_head_check(` call passes `&lane`.
- **I2 — finding rows carry their scan's `scan_id`.** `scanned_unix` has
  second resolution, so two runs in one second (interleaved) were
  grouped by time into whichever marker came first (B, the newest, read
  "후보 0건"; a crashed same-second run's finding showed as the current
  run's). `scan_case` writes `"scan_id"` (the marker's, `<unix>-<hex4>`)
  on every `anomaly-scan` row, the `none` row included — inside the
  HMAC'd bytes like every field. A row with a string `scan_id` belongs to
  the marker with the same `scan_id` only; unmarked rows group by it
  (time = its first row's `scanned_unix`), and an unmarked group joins
  only the later completed run with that `scan_id` (R19 I1's merge).
  Rows without `scan_id` (older scanners) keep the `scanned_unix` rules,
  also next to markers that have one. Same rule in
  `report_anomaly_withheld.rs` (`same_run` / `run_key`) and the report
  script (`sameAnomalyRun`); the agreement test covers six same-second /
  legacy shapes. Older readers: every reader parses lane rows as JSON
  objects and ignores fields it does not know (the R19 build rendered the
  new rows, verified the chain, and agreed with itself — it only grouped
  same-second runs by time).

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
  into the process environment and never touches disk. R11-I3: the
  process removes `VAR` from its environment at start (P10) and passes it
  on only to its own step children.
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

### Finalize lifecycle entries (R13-S1, P15)

R12's finalize journal (`review/finalize.inprogress.json`) was unkeyed: anyone
able to write the case folder could plant one naming the CURRENT, completed
sign-off and make the workstation itself append a keyed `signoff-abandoned`
for it, write arbitrary bytes back as `review/signoff.json` and rename every
package `_INCOMPLETE`. From R13 the sign-off lane carries three more keyed
(HMAC when a key is configured) event kinds, and the journal is only a
pointer to them:

| `event` | written | fields |
|---|---|---|
| `finalize-begin` | before the journal and before the `signoff` entry | `finalize_id` (random 128-bit hex), `signoff_record_sha256` (pending record), `previous_signoff_sha256` (what `review/signoff.json` held, or null), `package_name` (the ONE `reports/` name `package-case --package-name` may publish), `host_id` (`util::host_id`), `pid`, `writer_started_ms`, `writer_image`, `started_unix` |
| `finalize-complete` | after a finalize completed, before its journal is removed | `finalize_id`, `signoff_record_sha256`, `package_name`, `package_manifest_sha256` |
| `finalize-resolved` | when an unfinished finalize was cleaned up | `finalize_id`, `signoff_record_sha256`, `outcome`: `failed` (its failure path left nothing), `recovered` (a recovery finished), `refused` (stopped before its sign-off was chained), `superseded` (a journal kept only for a stale report, replaced by the next finalize), `dismissed` (`frametrace finalize-journal <case> --dismiss`) |

A recovery acts only when the journal's `finalize_id` names the lane's
**latest trusted** `finalize-begin` (authenticated here; self-consistent only
on a PC holding no audit key — P1) with no trusted `finalize-complete` /
`finalize-resolved` for it, and the journal's SHAs, package name and host id
equal the entry's. It then uses the entry's values: bytes are written back
only when they hash to `previous_signoff_sha256`, only `package_name` (and a
kept work folder reserved for it) is marked, only `signoff_record_sha256` is
abandoned. A complete/resolved entry → the journal is only deleted. Anything
else (`unpaired`: no entry, not the latest, not authenticated, lane broken,
keys unreadable, R12-format journal) changes nothing. A journal whose
`host_id` is another PC's is never recovered (P17). These events are not
`signoff` entries: the sign-off position check, "not the latest" and the
legacy-record cap ignore them, and the lane has no recency keys.

**Keyless limit:** on a PC without an audit key every entry is
self-consistency only, so someone who can write the case folder can also
append a chained `finalize-begin` — the same limit as every other keyless
record (and `--strict` exits 3 for such a sign-off, R13-S12).

#### R14 (P18/P19): the lane's tail is not proof of an interruption

A chained lane's END can be cut (I9): deleting the last line of a good
finalize (`finalize-complete`) and rebuilding the journal from the plaintext
`finalize-begin` made the begin entry "the latest one without a complete
entry", and R13 abandoned the good keyed sign-off, restored the older bytes
and renamed its package (two R13 reviewers reproduced it with the real CLI).
So the lane alone never starts a recovery any more:

- **P18 — host-local open-finalize record.** Before its begin entry a
  finalize writes `<local state dir>/finalize-open/<finalize_id>.json`
  (Windows `%LOCALAPPDATA%\frametrace\finalize-open\`, falling back to
  `%APPDATA%\frametrace\`; elsewhere `$XDG_CONFIG_HOME/frametrace/` or
  `~/.config/frametrace/`) with the finalize id, the canonical case path,
  the sign-off / previous sign-off SHA-256, the reserved package name, the
  host id and `state` (`open`, or `completed-unrecorded` when the complete
  entry could not be appended). It is removed once the lane records the
  finalize complete or resolved. A recovery (abandon, restore, mark) runs
  only when this record, the lane's begin entry and the journal agree on
  every field; otherwise the journal is `unpaired` with `unpaired_code:
  "no-local-record"` ("로컬 기록 없음") / `local-mismatch` /
  `local-unreadable` and nothing changes. The record lives outside the
  case folder, so a copy or share of the case never carries it, and
  writing it needs the examiner's own profile (examiner-level access is
  out of scope, as for the key file). Records older than 30 days that
  stand for nothing left to recover are swept at server start.
- **P19 — external evidence first.** When `review/signoff.json` is the
  begin entry's sign-off and the reserved package is published with a
  complete `manifest.sha256` (newline-terminated, every listed file present,
  `review/signoff.json` listed with that SHA-256 and the packaged copy
  hashing to it), the finalize FINISHED: only the missing
  `finalize-complete` entry is appended — nothing is rolled back. A begin
  entry followed by any entry this PC cannot vouch for (stripped HMAC,
  another key, a broken chain — the cut entries may have closed it) or by a
  usable `lane-repair` is `unpaired`.
- **Host id (R14-S7).** `util::host_id` uses the Windows `MachineGuid` and
  the physical DNS host name from `GetComputerNameExW` (R13 read the
  `COMPUTERNAME` variable, which a parent process sets). When the
  MachineGuid cannot be read the id is `unknown` and the PC recovers
  nothing. Limits: a cloned VM / disk image shares the MachineGuid (and,
  unless renamed, the host name) with its source; Linux without
  `/etc/machine-id` and macOS use the host name alone. The P18 record is
  what really ties a recovery to the PC that ran the finalize.
- **Writer liveness (R14-S3).** A journal's writer counts as running only
  on a positive match: known start time within 2 s of the recorded one, and
  a FrameTrace executable (`frametrace…`) of the recorded name. A platform
  that cannot read process start times never sees a live writer (the
  exclusive journal creation, R14-S4, and the busy lease keep finalizes
  apart). `frametrace finalize-journal <case> --dismiss --force` moves a
  journal whose writer seems to run.

#### R15 (P21/P23): the local record is a state machine

R14 removed the local record as the finalize's LAST step, with no retry. When
that removal failed the record stayed `open`, and the R14-S1 attack worked
again (R14 reviewer PoC A, reproduced): cut the lane's `finalize-complete`,
rebuild the journal from the plaintext begin entry, put the older sign-off in
`finalize.previous-signoff.bin` and delete one listed file of the good
package (defeating P19) — recovery abandoned the good sign-off, restored the
older bytes and renamed the package `_INCOMPLETE`. P19 itself trusted the
case folder (PoC B: one forged manifest line plus a copy of the promoted
sign-off made an interrupted finalize "finished").

- **States.** `open` (before the begin entry) → `packaged` (right after
  package-case succeeded, carrying `manifest_sha256`, the SHA-256 of the
  reserved package's `manifest.sha256`) → `completed` (atomically rewritten
  right after the `finalize-complete` entry, BEFORE the journal is removed) →
  removed (retried within the rename budget). A failure path writes
  `resolved` after its `finalize-resolved` entry; `completed-unrecorded`
  (R14-S2) stays. A record left behind past `open` is safe.
- **Only `open` is recovered.** `completed` / `completed-unrecorded` →
  finished: only the missing complete entry is (re)written. `packaged` →
  finished when the reserved package's manifest still hashes to the recorded
  SHA-256, otherwise `unpaired` (`package-changed`) — never rolled back,
  never recorded complete. `resolved` → closed (only the journal goes).
  P19's case-folder evidence alone no longer makes anything "finished".
- **Residual window.** A crash between the complete entry and the
  `completed` rewrite leaves `packaged` (finished if the manifest is
  intact); a crash between package-case's success and the `packaged` write
  leaves `open` (rolled back, the safe side).
- **Sweep (P23).** At server start an `open` record whose case lane holds a
  trusted complete / resolved entry of it is closed at once; other `open`
  records go after 30 days only when the case folder is reachable and holds
  no journal of theirs. An unreachable case or an unparseable journal is
  "unknown" — never swept (PoC C).
- **Writer liveness (R15-S4/S10).** A live writer also needs its executable
  in this executable's folder and hashing to the journal's
  `writer_image_sha256` (PoC E: a renamed program counted). A live writer
  without this PC's local record of that finalize is only a note. A writer
  pid this account may not inspect whose Toolhelp / `/proc` name is a
  FrameTrace one may be another account's finalize: blocked for 24 h from the
  journal's start, no dismiss offered.
- **Keyed children (R15-S5).** With the workstation's `key_source=file:` the
  CLI steps get `--key-source` and key the entries they append after the
  begin entry (`hash-verify-head`), so the pairing holds; an unkeyed line
  planted after the begin entry still unpairs it.

#### R16 (P24/P25): states are sibling files; a maybe-live writer is never rolled back

R15 advanced the record by atomically rewriting `<id>.json` (MoveFileEx
over it, then `fs::rename`) and removed it with retries. A program that
holds the file without delete sharing (an indexer, antivirus, backup) makes
the rewrites and the removal fail alike: the record stayed `open` and the
R14-S1 attack (cut the complete line, rebuild the journal, older bytes in
`finalize.previous-signoff.bin`) rolled the good finalize back again (R15
reviewer PoC, reproduced with the real CLI and a real locked handle).

- **P24 — one file per state, never rewritten.** `<id>.json` (`open`),
  `<id>.resolved.json`, `<id>.packaged.json`,
  `<id>.completed-unrecorded.json`, `<id>.completed.json`, each created with
  `create_new` (fsync'd, directory flushed; a retried advance finding its
  own file is success). The state is the highest present, in that order. A
  lock on one file does not stop the next one being created. Every file
  present must parse, name its id and its own state and describe the same
  finalize — otherwise the record is unreadable (`local-unreadable`, never a
  lower state). Removal goes lowest first and stops at the first failure
  (retry ≤ 3 s per file), so it never lowers the state.
- **The journal goes only once the record is past `open`.** After the
  complete entry, `completed` is created; when it cannot be, the journal is
  kept as `completed-unrecorded` (the record is then `packaged`, which a
  cut lane tail finds "finished", not "open"). A recovery that closes a
  journal by the lane (complete / resolved) first creates `completed` /
  `resolved` for a lower record; when that fails the journal stays and the
  finalize is blocked (`blocks_finalize: true`, same reason).
- **S3.** The complete entry a recovery writes again carries the manifest
  SHA-256 the record holds; a `completed` / `completed-unrecorded` record
  whose manifest is no longer the reserved package's is `package-changed`.
- **S5/S7.** Manifests are hashed streaming (up to 1 GiB) when a record past
  `open` is judged; the status poll re-hashes manifests up to 16 MiB on every
  poll (the cache key is size + mtime, which a same-size rewrite can keep).
  The sweep keeps a record in ANY state whose case is unreachable or still
  holds its journal (P23 extended).
- **P25 — maybe-live writer.** A live process matching the journal's pid,
  start time and FrameTrace name, whose executable is not in this
  executable's folder or does not hash to `writer_image_sha256` (another
  FrameTrace install or build), with this PC's local record of the
  finalize: `in-progress` / `unverified-writer`, nothing recovered, the
  finalize blocked, `--dismiss` refused; `--dismiss --force` only moves the
  journal. R15 took it as gone, so opening the case (or `--dismiss`) with
  another build rolled a running finalize back.
- **S4.** "Another account's FrameTrace writer" needs the lane's latest
  trusted begin entry of that finalize to record the same pid (a planted
  journal or a reused pid no longer blocks 24 h); the block offers
  `--dismiss --force`.
- **S8.** `--dismiss --force` of a journal whose writer may still run
  (verified, unverified, other account) records no `finalize-resolved` and
  keeps the local record — the writer may still write `finalize-complete`.
  An `open` journal (writer gone) is still cleaned up first and closed with
  `resolved (dismissed)`.
- **Residual.** If the local record folder accepts the `open` file but then
  refuses both `packaged` and `completed` (disk full, ACL change mid-run) the
  record stays `open` while the journal stays `completed-unrecorded`; the
  journal's own state is not trusted, so a cut lane tail then rolls back as
  in R15. A crash while a state file is half written leaves it unparseable:
  `local-unreadable` (blocks, dismiss), never a rollback. Forging the local
  record folder stays out of scope (examiner profile write access).
  (R17 closes the first half of this residual — next section.)

#### R17 (P28): a record that cannot advance loses its `open` file

R16's residual: a local folder that took the `open` file but then refused
both `packaged` and `completed` (disk full, ACL change) left the record
`open` beside a `completed-unrecorded` journal, and a cut lane tail plus a
rebuilt journal rolled the good finalize back (reproduced for R17 with
directories squatting the two state-file names: `r17_s1_record_that_cannot_advance_is_dropped`).

- **P28.** Wherever the record must move past `open` (after the complete
  entry → `completed`; after a resolved entry → `resolved`; the failed
  complete entry → `completed-unrecorded`) and the state file cannot be
  created while the record still reads `open` (or cannot be read), the
  `<id>.json` file is deleted on the spot — deleting needs no free space.
  No record means any journal naming that finalize is `unpaired`
  (`no-local-record`): never rolled back, never recorded complete. The
  journal then goes as usual (lane closed). Only when the deletion fails
  too is the journal kept (`completed-unrecorded`, or the lane-closed
  journal, `blocks_finalize: true`) — the one residual: creation AND
  deletion both refused, then a cut tail before the next recovery. A
  record already past `open` keeps the R16 behavior (journal kept until
  `completed` can be created; a cut tail finds it "finished").
- **S4.** The `resolved` advance after a `finalize-resolved` entry (failure
  path, superseded journal, recovery's close, `--dismiss`) follows P28;
  R16 ignored its failure and removed the journal with the record `open`.
- **S7.** A finalize the lane closed (complete / resolved) or whose record
  reads `completed` / `resolved` has no writer to wait for: the liveness
  and other-account checks are skipped (a journal a completed finalize
  failed to remove no longer blocks `--dismiss` while its workstation
  runs).
- **S2 / S8.** `finalize-journal <case> --writer-gone`: the examiner
  confirmed the journal's writer is gone — the journal is judged as if it
  were and the NORMAL recovery runs (refused for a verified live writer).
  `--dismiss --force` still only moves the journal and now names the
  sign-off and package it leaves in place. Every block on a maybe-live
  writer says "wait" first (it lifts when the process ends; another
  account's block 24 h after the journal's start). A verified live writer
  is offered no command; unverified / other-account get both.
- **S3.** "Another account's FrameTrace writer" also needs the live
  process's start time, when it can be read (Windows: the system process
  list, `NtQuerySystemInformation(SystemProcessInformation)`, readable
  without opening the process; Linux `/proc/<pid>/stat`), to be the
  journal's `writer_started_ms` (± 2 s): a pid reused since no longer
  blocks for 24 h.
- **S5.** The torn-scan-lane preflight runs before the finalize's journal
  check and recovery (nothing at all is written for a case it refuses).
- **S6.** An unreadable record is swept (30 days) only when one of its
  files parses and every such file's case is reachable and no longer holds
  its journal (R16 swept it by age alone).
- **S9.** The status path hashes a reserved package's manifest only up to
  16 MiB; a larger one is "확인 보류" (`manifest_check: "withheld"`, the
  record's word shown, `blocks_finalize: false` — R18-S2: `null`, below)
  — the 1 GiB streaming hash runs in recoveries only (R16 ran it on every
  status cache miss).
- **S10.** The `cmd.exe` form of a command is not offered when the case
  path holds `%`: `cmd.exe` expands `%VAR%` inside double quotes and has no
  escape for it at its interactive prompt (`%%` is a batch-file escape and
  stays `%%` there), so the pasted command could name another folder. The
  PowerShell form is single-quoted (no expansion).

#### R18 (P29 / P31): a pasted command is paste-safe or absent; unknown is not "no block"

- **S1 (High, R17 reviewer PoC, confirmed with PowerShell's own parser).**
  The commands offered for pasting doubled only the ASCII `'`, but
  PowerShell also reads U+2018/U+2019/U+201A/U+201B as single quotes: a
  case folder named `x’; calc; ’` yielded a `--dismiss` / `--dismiss
  --force` / `--writer-gone` command that parsed as four statements and ran
  `calc` when pasted. **P29:** a command is built only when every character
  of the case path is on a whitelist (letters and digits of any script,
  ASCII space, `- _ . , / \ : ( ) [ ] + = @ ~ #`; not starting with `-`);
  nothing is ever escaped. Otherwise every command field of
  `/api/status.finalize_journal` (PowerShell and `cmd.exe` forms) is null,
  `command_unavailable_reason` is "경로에 특수 문자 — 명령을 직접
  입력하십시오", and the CLI / recovery texts show only
  `frametrace finalize-journal <케이스 폴더> <flags>` with that reason. The
  whitelist also covers `cmd.exe`'s `%` (R17-S10) and `!` (delayed
  expansion, S8). Regression parses each offered command with
  `System.Management.Automation.Language.Parser::ParseInput` (Windows):
  one statement, one command, the path a single-quoted constant equal to
  the case folder.
- **S2 (P31).** The status path does not hash a manifest over 16 MiB, and
  R17 then showed `completed-unrecorded` / `blocks_finalize: false` even
  after a recovery (full hash) had judged the package CHANGED and kept the
  journal. Now each full-hash assessment of a record past `open` remembers
  its verdict (finished / package changed) in memory per (finalize id,
  manifest size + mtime, equal before and after the hash); the status path
  shows a remembered verdict (`manifest_check: "recovery-verdict"`) and
  otherwise says `blocks_finalize: null` (unknown — the finalize hashes in
  full first). `finalize-journal <case>` (CLI) shows the full-hash
  assessment. Residual: a same-size rewrite with its mtime restored after
  the recovery keeps a remembered verdict on the status screen (the
  finalize's own full hash still decides).
- **S3.** `--writer-gone` looks at the journal twice (as it is, then as if
  the writer were gone): the two must name the same finalize id, writer pid
  and start time and sign-off SHA, else nothing is done; a writer verified
  as this program is refused in the second look as well.
- **S7.** While a live process holds the writer's pid AND started when the
  journal says its writer did (both known, ± 2 s — always so for an
  unverified writer), `--writer-gone` also needs `--i-am-sure`; the offered
  command then carries it (`writer_gone_i_am_sure: true`).
- **S4 / S5 / S6.** `--writer-gone` prints the recovery's log lines;
  another account's block says recovery happens in THAT account (here,
  without its local record, the journal can only be moved); the
  finalize's torn-lane refusal reads `{rel}에 이어 쓸 수 없음: {err}`
  (not the anomaly scan's wording).

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
