//! R12-S2/S3 (P13) and R13 (P15/P17): the finalize journal
//! `review/finalize.inprogress.json`.
//!
//! A finalize first appends a keyed `finalize-begin` entry to the sign-off
//! lane (`report::FinalizeBegin`: its random id, the pending sign-off's
//! SHA-256, the SHA-256 of the sign-off it replaces, the ONE `reports/`
//! package name it reserved, this PC's host id and the writer process),
//! then writes this journal (fsync'd) and only then chains, renders and
//! promotes its sign-off. The bytes of the sign-off it replaces go to
//! `review/finalize.previous-signoff.bin` beside it (R13-S11: the journal
//! stays small — at most [`JOURNAL_MAX_BYTES`]). A finalize that completes
//! appends `finalize-complete` and removes the journal; one whose failure
//! path cleaned up appends `finalize-resolved`.
//!
//! R13-S1 (P15): the journal is only a pointer. R12 recovered whatever
//! journal it could parse — anyone able to write the case folder could
//! make the workstation itself abandon a completed sign-off in the keyed
//! lane, write arbitrary bytes back as `review/signoff.json` and rename
//! every package `_INCOMPLETE`. A recovery now acts only when the
//! journal matches the lane's LATEST trusted begin entry (authenticated
//! here; self-consistent only on a PC without audit keys) and nothing
//! closes it; then it uses the values that ENTRY records: it writes back
//! only bytes hashing to the recorded previous SHA-256, marks only the
//! recorded package name, abandons only the recorded sign-off. A journal
//! with a complete / resolved entry is only deleted. Any other journal
//! (`unpaired`) changes nothing: it is shown, blocks the next finalize
//! (never overwritten unseen) and is moved aside with `frametrace
//! finalize-journal <case> --dismiss`.
//!
//! R13-S5 (P17): a journal written on another PC (host id) is never
//! recovered here; it blocks the finalize for [`OTHER_HOST_BLOCK`] (24 h),
//! after which a finalize moves it aside. R13-S4: a journal whose writer
//! still runs (same pid AND process start time AND executable name) is
//! left alone; pid 0 or a reused pid no longer block forever. R13-S7: the
//! start-up recovery runs in the background after the server serves,
//! holding `busy` (status `recovering`).
//!
//! R14-S1 (P18): the lane's tail can be cut (an append-only file is not
//! tamper-proof at its end), so "the latest begin entry has no complete
//! entry" is not proof that a finalize was interrupted: cutting the
//! `finalize-complete` line of a good finalize and rebuilding the journal
//! from the plaintext begin entry made R13 abandon the good keyed
//! sign-off, write the older one back and rename its package. A finalize
//! now first writes a HOST-LOCAL open-finalize record
//! ([`local_open_dir`]`/<finalize_id>.json`, outside the case folder — a
//! copy or share of the case never carries it) and removes it once the
//! finalize completed or was resolved. A recovery acts only when that local
//! record, the lane's begin entry and the journal all agree; without it the
//! journal is `unpaired` ("로컬 기록 없음") and nothing changes.
//!
//! R14 (P19): external evidence beats the lane's tail. When
//! `review/signoff.json` is the begin entry's sign-off and the reserved
//! package is published with a complete manifest carrying that sign-off,
//! the finalize finished: only its missing `finalize-complete` entry is
//! written. A begin entry followed by entries this PC cannot vouch for, or
//! by a lane-repair, is `unpaired` (the cut entries may have closed it).
//!
//! R15 (P21): the local record is a STATE MACHINE that ends in a
//! transition, not a delete — R14 removed it as the last step, so a
//! removal that failed left it `open` and the R14-S1 attack worked again
//! (cut the complete line, rebuild the journal, delete a packaged file to
//! defeat P19). `open` → `packaged{manifest_sha256}` (right after
//! package-case succeeded) → `completed` (atomically rewritten right after
//! the complete entry, BEFORE the journal goes) → removed with retries
//! (`resolved` on the failure paths). Only `open` is ever rolled back;
//! "finished" (P19) is now only a `packaged` record whose manifest SHA-256
//! is the reserved package's (the case folder's own evidence can be
//! forged). P23: where the sweep cannot read a case or its journal it
//! keeps the record ("unknown").
//!
//! R16 (P24): the state machine never REWRITES a file. R15 advanced it by
//! an atomic rewrite of `<id>.json`; a program holding that file without
//! delete sharing (an indexer, antivirus, backup) made the rewrite and the
//! removal fail alike, the record stayed `open` and the R14-S1 attack
//! rolled the good finalize back again (R15 reviewer PoC). Each state is
//! now its own sibling file, created with `create_new` beside the others —
//! `<id>.json` (open), `<id>.resolved.json`, `<id>.packaged.json`,
//! `<id>.completed-unrecorded.json`, `<id>.completed.json` — and the
//! record's state is the highest one present ([`LOCAL_STATE_ORDER`]). A
//! lock on one file never stops the next one being created, and a removal
//! goes lowest first and stops at the first failure, so neither can lower
//! the state. The journal goes only once the record is past `open` (a
//! `completed` that cannot be created keeps the journal as
//! `completed-unrecorded`). P25: a live process that matches the writer's
//! pid, start time and FrameTrace name but not its folder / SHA-256 is a
//! "maybe-live" writer — never recovered, `--force` to move its journal.
//!
//! R17 (P28): when the record cannot ADVANCE (no state file can be created
//! — a full disk, an ACL change), its `open` file, the only thing a
//! recovery ever rolls back on, is deleted on the spot
//! ([`advance_or_drop`]; deleting needs no free space): no record means a
//! rebuilt journal is `unpaired`, never rolled back. Only when that
//! deletion fails too is the journal kept. `finalize-journal --writer-gone`
//! runs the normal recovery once the examiner confirmed that a maybe-live
//! writer is gone ([`cli_writer_gone`]).
//!
//! Disk work here never runs under the state lock.

use super::*;
use crate::report::{FinalizeBegin, FinalizeLanePairing};

/// R12-S2: where the journal of a finalize in progress lives.
pub(crate) const FINALIZE_JOURNAL_REL_PATH: &str = "review/finalize.inprogress.json";

/// R13-S11: the bytes of the sign-off a finalize replaces (the journal
/// records only their SHA-256, as does the lane's begin entry).
pub(crate) const PREVIOUS_SIGNOFF_REL_PATH: &str = "review/finalize.previous-signoff.bin";

const JOURNAL_KIND: &str = "frametrace-finalize-journal";

/// R13-S11: a journal larger than this is not parsed ("unreadable") — the
/// status poll reads it, and a planted multi-gigabyte file must not stall
/// it. A journal this build writes is well under 2 KiB.
pub(crate) const JOURNAL_MAX_BYTES: u64 = 64 * 1024;

/// The previous sign-off kept beside the journal is read up to this size.
const PREVIOUS_SIGNOFF_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// R13-S5 (P17): how long another PC's journal blocks a finalize here.
pub(crate) const OTHER_HOST_BLOCK: Duration = Duration::from_secs(24 * 3600);

/// R13-S7: the retry budget of the start-up recovery's renames (it runs in
/// the background, but a file another program holds should not keep the
/// workstation `busy` for the full 12 s per step).
pub(crate) const STARTUP_RETRY_BUDGET: Duration = Duration::from_secs(3);

/// R12-S2: the abandon reason recorded for a finalize found interrupted.
const INTERRUPTED_REASON: &str = "finalize interrupted (workstation stopped before it completed); recovered from review/finalize.inprogress.json and its finalize-begin lane entry";

/// R12-S2: `state` of a journal a finalize wrote (running, or interrupted
/// and not yet recovered).
pub(crate) const STATE_IN_PROGRESS: &str = "in-progress";
/// R12-S2: recovered; only the stale report remains for a finalize.
pub(crate) const STATE_RECOVERED: &str = "recovered";
/// R12-S2: recovery could not finish (a sign-off not taken back, no abandon
/// record, a package not marked) — "확인 필요".
pub(crate) const STATE_INTERRUPTED: &str = "interrupted";
/// R13 status-only states (never written into a journal).
pub(crate) const STATE_UNREADABLE: &str = "unreadable";
pub(crate) const STATE_UNPAIRED: &str = "unpaired";
pub(crate) const STATE_OTHER_HOST: &str = "other-host";
pub(crate) const STATE_RECOVERING: &str = "recovering";
pub(crate) const STATE_COMPLETED: &str = "completed";
/// R14-S2: the finalize completed but its `finalize-complete` lane entry
/// could not be written — the journal (and the local record) are kept in
/// this state and the next recovery writes the entry (P19 / the local
/// record vouch for it; the journal's own word is never enough).
pub(crate) const STATE_COMPLETED_UNRECORDED: &str = "completed-unrecorded";

/// R14 (P18): the reason text of a journal without this PC's local
/// open-finalize record (track U shows "짝 없음(로컬 기록 없음)").
pub(crate) const NO_LOCAL_RECORD: &str = "로컬 기록 없음";

/// R14 (P18): the local record directory's name under the local state dir.
const LOCAL_OPEN_DIR_NAME: &str = "finalize-open";
const LOCAL_RECORD_KIND: &str = "frametrace-finalize-open";
/// R14 (P18) / R15 (P21): local record states — a state machine that ends
/// in a transition, not in a delete: `open` (written before the begin
/// entry) → `packaged` (package-case succeeded; carries the SHA-256 of the
/// package's manifest) → `completed` (atomically rewritten right after the
/// `finalize-complete` entry, BEFORE the journal goes) → removed (retried;
/// a record left behind is safe). A failure path ends in `resolved`
/// (after its `finalize-resolved` entry). Only `open` is ever recovered
/// (rolled back); `packaged` / `completed` / `completed-unrecorded` mean
/// the finalize finished, `resolved` that it was cleaned up.
pub(crate) const LOCAL_OPEN: &str = "open";
/// R15 (P21): package-case succeeded (`manifest_sha256` recorded).
pub(crate) const LOCAL_PACKAGED: &str = "packaged";
/// R15 (P21): the `finalize-complete` entry was written.
pub(crate) const LOCAL_COMPLETED: &str = "completed";
/// R15 (P21): the `finalize-resolved` entry was written.
pub(crate) const LOCAL_RESOLVED: &str = "resolved";
/// R14-S13: a local record older than this is swept at server start once
/// it no longer stands for anything left to recover.
pub(crate) const LOCAL_RECORD_EXPIRY: Duration = Duration::from_secs(30 * 24 * 3600);

/// R16 (P24): the local record states from lowest to highest — each one a
/// sibling file ([`local_state_path`]); the record's state is the highest
/// present. `resolved` (a failure path cleaned up) ranks just above
/// `open`: every state past `open` means "never roll back".
pub(crate) const LOCAL_STATE_ORDER: [&str; 5] = [
    LOCAL_OPEN,
    LOCAL_RESOLVED,
    LOCAL_PACKAGED,
    STATE_COMPLETED_UNRECORDED,
    LOCAL_COMPLETED,
];

fn local_rank(state: &str) -> Option<usize> {
    LOCAL_STATE_ORDER.iter().position(|known| *known == state)
}

/// R16 (P24): a local record file removal is retried at most this long —
/// a record left behind is safe once it is past `open`, so a held file
/// must not stall the finalize for the full rename budget.
fn local_removal_budget() -> Duration {
    crate::package::rename_retry_budget().min(STARTUP_RETRY_BUDGET)
}

/// One finalize's journal (R13 format: schema 2).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct FinalizeJournal {
    /// The id its `finalize-begin` lane entry names (empty: a journal of
    /// an R12 build, which wrote no lane entry — always unpaired).
    pub(crate) finalize_id: String,
    pub(crate) host_id: String,
    /// R13-S4: the process that wrote it (refreshed on every rewrite).
    pub(crate) writer: crate::util::WriterIdentity,
    /// R15-S4: SHA-256 of the writer's executable file (a live process
    /// counts as the writer only when its executable is in this
    /// executable's folder and hashes to this).
    pub(crate) writer_image_sha256: Option<String>,
    pub(crate) started_unix: u64,
    pub(crate) examiner: String,
    /// SHA-256 of the pending sign-off record.
    pub(crate) signoff_sha256: String,
    /// SHA-256 of what `review/signoff.json` held before (`None`: nothing).
    pub(crate) previous_signoff_sha256: Option<String>,
    /// The `reports/` name its package-case was told to publish under.
    pub(crate) package_name: String,
    /// [`STATE_IN_PROGRESS`] as the finalize writes it; a recovery or
    /// failure path that keeps the journal rewrites it as
    /// [`STATE_RECOVERED`] (only the stale report is left — the next
    /// finalize re-renders it) or [`STATE_INTERRUPTED`].
    pub(crate) state: String,
}

impl FinalizeJournal {
    #[cfg(test)]
    pub(crate) fn signoff_sha256(&self) -> String {
        self.signoff_sha256.clone()
    }

    /// The lane entry this journal must pair with.
    pub(crate) fn begin_entry(&self) -> FinalizeBegin {
        FinalizeBegin {
            finalize_id: self.finalize_id.clone(),
            signoff_record_sha256: self.signoff_sha256.clone(),
            previous_signoff_sha256: self.previous_signoff_sha256.clone(),
            package_name: self.package_name.clone(),
            host_id: self.host_id.clone(),
            pid: self.writer.pid,
            writer_started_ms: self.writer.started_ms,
            writer_image: self.writer.image.clone(),
            started_unix: self.started_unix,
        }
    }

    fn to_json(&self) -> String {
        serde_json::json!({
            "schema_version": 2,
            "kind": JOURNAL_KIND,
            "finalize_id": self.finalize_id,
            "host_id": self.host_id,
            "pid": self.writer.pid,
            "writer_started_ms": self.writer.started_ms,
            "writer_image": self.writer.image,
            "writer_image_sha256": self.writer_image_sha256,
            "started_unix": self.started_unix,
            "examiner": self.examiner,
            "signoff_sha256": self.signoff_sha256,
            "previous_signoff_sha256": self.previous_signoff_sha256,
            "package_name": self.package_name,
            "state": self.state,
        })
        .to_string()
    }

    fn parse(text: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|err| format!("JSON이 아닙니다: {err}"))?;
        if value.get("kind").and_then(|v| v.as_str()) != Some(JOURNAL_KIND) {
            return Err("FrameTrace 최종화 저널이 아닙니다".to_string());
        }
        let text_field = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let signoff_sha256 = text_field("signoff_sha256");
        if signoff_sha256.len() != 64 || !signoff_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("signoff_sha256이 SHA-256이 아닙니다".to_string());
        }
        // An R12 journal carried the record text; it must agree.
        if let Some(record) = value.get("signoff_text").and_then(|v| v.as_str())
            && crate::sha256::digest_bytes(record.as_bytes()) != signoff_sha256
        {
            return Err("signoff_sha256이 signoff_text와 다릅니다".to_string());
        }
        Ok(Self {
            finalize_id: text_field("finalize_id"),
            host_id: text_field("host_id"),
            writer: crate::util::WriterIdentity {
                pid: value.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                started_ms: value.get("writer_started_ms").and_then(|v| v.as_u64()),
                image: value
                    .get("writer_image")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            },
            writer_image_sha256: value
                .get("writer_image_sha256")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            started_unix: value
                .get("started_unix")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            examiner: text_field("examiner"),
            signoff_sha256,
            previous_signoff_sha256: value
                .get("previous_signoff_sha256")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            package_name: text_field("package_name"),
            state: value
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or(STATE_IN_PROGRESS)
                .to_string(),
        })
    }

    /// Writes the journal durably (`util::write_text_atomic`: temp file
    /// fsync'd, write-through rename, directory flushed).
    pub(crate) fn write(&self, case_dir: &Path) -> Result<(), String> {
        let path = journal_path(case_dir);
        crate::util::write_text_atomic(&path, &self.to_json()).map_err(|err| {
            format!(
                "최종화 저널 {}을(를) 쓰지 못했습니다: {err}",
                path.display()
            )
        })
    }

    /// R14-S4: creates the journal EXCLUSIVELY (`create_new`): a journal
    /// already there — another PC's finalize of a shared case that started
    /// meanwhile, R13 overwrote it — refuses this finalize. Written,
    /// fsync'd and its directory flushed; a write that fails removes the
    /// file it created.
    pub(crate) fn create_exclusive(&self, case_dir: &Path) -> Result<(), String> {
        let path = journal_path(case_dir);
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        let mut file = match created {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(format!(
                    "최종화 저널 {}이(가) 이미 있습니다 — 다른 PC나 프로세스의 최종화가 이 케이스에서 시작되었습니다",
                    path.display()
                ));
            }
            Err(err) => {
                return Err(format!(
                    "최종화 저널 {}을(를) 만들지 못했습니다: {err}",
                    path.display()
                ));
            }
        };
        let written = file
            .write_all(self.to_json().as_bytes())
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all());
        drop(file);
        if let Err(err) = written {
            let _ = std::fs::remove_file(&path);
            return Err(format!(
                "최종화 저널 {}을(를) 쓰지 못했습니다: {err}",
                path.display()
            ));
        }
        crate::util::sync_parent_directory(&path);
        Ok(())
    }

    /// R13-S4: rewritten by this process — it is the writer now. R14-S4:
    /// only while the journal on disk is still this finalize's.
    pub(crate) fn rewrite_as(&self, state: &str, case_dir: &Path) -> Result<(), String> {
        if !journal_is_own(case_dir, &self.finalize_id) {
            return Err(format!(
                "{}이(가) 더 이상 이 최종화({})의 저널이 아니어서 다시 쓰지 않았습니다",
                journal_path(case_dir).display(),
                short(&self.finalize_id)
            ));
        }
        let mut kept = self.clone();
        kept.state = state.to_string();
        kept.writer = crate::util::current_writer();
        kept.writer_image_sha256 = own_exe_sha256();
        kept.write(case_dir)
    }

    /// R14 (P18): the local record this finalize writes before its begin
    /// entry.
    pub(crate) fn local_record(&self, case_dir: &Path, state: &str) -> LocalRecord {
        LocalRecord {
            finalize_id: self.finalize_id.clone(),
            case_dir: case_key(case_dir),
            signoff_sha256: self.signoff_sha256.clone(),
            previous_signoff_sha256: self.previous_signoff_sha256.clone(),
            package_name: self.package_name.clone(),
            host_id: self.host_id.clone(),
            started_unix: self.started_unix,
            state: state.to_string(),
            updated_unix: crate::util::now_unix().unwrap_or(0),
            manifest_sha256: None,
        }
    }

    /// R15 (P21) / R16 (P24): advances this finalize's local record to
    /// `state` by CREATING its sibling file (never a rewrite: a file another
    /// program holds cannot stop it), keeping the manifest SHA-256 the
    /// record carries (`packaged` records it; the later states keep it).
    pub(crate) fn advance_local(
        &self,
        case_dir: &Path,
        state: &str,
        manifest_sha256: Option<String>,
    ) -> Result<(), String> {
        let kept = manifest_sha256.or_else(|| {
            read_local_record(&self.finalize_id)
                .ok()
                .flatten()
                .and_then(|record| record.manifest_sha256)
        });
        LocalRecord {
            manifest_sha256: kept,
            ..self.local_record(case_dir, state)
        }
        .write()
    }

    /// R14 (P18): why this journal does not describe the finalize the local
    /// record `local` stands for (`None`: it does).
    fn mismatch_with_local(&self, local: &LocalRecord, case_dir: &Path) -> Option<&'static str> {
        if local.finalize_id != self.finalize_id {
            Some("최종화 ID")
        } else if local.case_dir != case_key(case_dir) {
            Some("케이스 폴더")
        } else if local.signoff_sha256 != self.signoff_sha256 {
            Some("서명 SHA-256")
        } else if local.previous_signoff_sha256 != self.previous_signoff_sha256 {
            Some("이전 서명 SHA-256")
        } else if local.package_name != self.package_name {
            Some("예약 패키지 이름")
        } else if local.host_id != self.host_id {
            Some("호스트 ID")
        } else {
            None
        }
    }

    /// R13-S1: why this journal does not describe the finalize `begin`
    /// records (`None`: it does).
    fn mismatch_with(&self, begin: &FinalizeBegin) -> Option<&'static str> {
        if self.signoff_sha256 != begin.signoff_record_sha256 {
            Some("서명 SHA-256")
        } else if self.previous_signoff_sha256 != begin.previous_signoff_sha256 {
            Some("이전 서명 SHA-256")
        } else if self.package_name != begin.package_name {
            Some("예약 패키지 이름")
        } else if self.host_id != begin.host_id {
            Some("호스트 ID")
        } else {
            None
        }
    }
}

pub(crate) fn journal_path(case_dir: &Path) -> PathBuf {
    case_dir.join(FINALIZE_JOURNAL_REL_PATH)
}

fn previous_signoff_path(case_dir: &Path) -> PathBuf {
    case_dir.join(PREVIOUS_SIGNOFF_REL_PATH)
}

/// Reads at most `cap` bytes of `path`; a larger file is an error (never
/// read whole).
fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(std::io::Error::other(format!(
            "파일이 상한 {} KiB보다 큽니다",
            cap / 1024
        )));
    }
    Ok(bytes)
}

/// Reads the case's journal: `Ok(None)` when there is none. R13-S11: at
/// most [`JOURNAL_MAX_BYTES`] are read.
pub(crate) fn read_journal(case_dir: &Path) -> Result<Option<FinalizeJournal>, String> {
    let path = journal_path(case_dir);
    match read_capped(&path, JOURNAL_MAX_BYTES) {
        Ok(bytes) => String::from_utf8(bytes)
            .map_err(|_| "UTF-8 텍스트가 아닙니다".to_string())
            .and_then(|text| FinalizeJournal::parse(&text))
            .map(Some)
            .map_err(|err| format!("{}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

/// R14-S4: the journal on disk is the one of `finalize_id` (a journal
/// another finalize created is never rewritten or removed by this one).
fn journal_is_own(case_dir: &Path, finalize_id: &str) -> bool {
    matches!(read_journal(case_dir), Ok(Some(journal)) if journal.finalize_id == finalize_id)
}

/// R14 (P18): how a case folder is named in a local record (its canonical
/// path, so two spellings of one folder agree).
fn case_key(case_dir: &Path) -> String {
    std::fs::canonicalize(case_dir)
        .unwrap_or_else(|_| case_dir.to_path_buf())
        .to_string_lossy()
        .to_string()
}

/// R14 (P18): one host-local open-finalize record
/// ([`local_open_dir`]`/<finalize_id>.json`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalRecord {
    pub(crate) finalize_id: String,
    /// [`case_key`] of the case the finalize ran on.
    pub(crate) case_dir: String,
    pub(crate) signoff_sha256: String,
    pub(crate) previous_signoff_sha256: Option<String>,
    pub(crate) package_name: String,
    pub(crate) host_id: String,
    pub(crate) started_unix: u64,
    /// [`LOCAL_OPEN`], [`LOCAL_PACKAGED`], [`LOCAL_COMPLETED`],
    /// [`LOCAL_RESOLVED`] or [`STATE_COMPLETED_UNRECORDED`] (P21).
    pub(crate) state: String,
    pub(crate) updated_unix: u64,
    /// R15 (P21 / S2): SHA-256 of the reserved package's `manifest.sha256`
    /// as package-case left it (from `packaged` on).
    pub(crate) manifest_sha256: Option<String>,
}

impl LocalRecord {
    fn to_json(&self) -> String {
        serde_json::json!({
            "schema_version": 1,
            "kind": LOCAL_RECORD_KIND,
            "finalize_id": self.finalize_id,
            "case_dir": self.case_dir,
            "signoff_sha256": self.signoff_sha256,
            "previous_signoff_sha256": self.previous_signoff_sha256,
            "package_name": self.package_name,
            "host_id": self.host_id,
            "started_unix": self.started_unix,
            "state": self.state,
            "updated_unix": self.updated_unix,
            "manifest_sha256": self.manifest_sha256,
        })
        .to_string()
    }

    fn parse(text: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|err| format!("JSON이 아닙니다: {err}"))?;
        if value.get("kind").and_then(|v| v.as_str()) != Some(LOCAL_RECORD_KIND) {
            return Err("FrameTrace 열린 최종화 기록이 아닙니다".to_string());
        }
        let text_field = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("{key}가 없습니다"))
        };
        let number = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        Ok(Self {
            finalize_id: text_field("finalize_id")?,
            case_dir: text_field("case_dir")?,
            signoff_sha256: text_field("signoff_sha256")?,
            previous_signoff_sha256: value
                .get("previous_signoff_sha256")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            package_name: text_field("package_name")?,
            host_id: text_field("host_id")?,
            started_unix: number("started_unix"),
            state: text_field("state")?,
            updated_unix: number("updated_unix"),
            manifest_sha256: value
                .get("manifest_sha256")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
    }

    /// R16 (P24): records the state `self.state` by CREATING its sibling
    /// file (`create_new`, fsync'd, directory flushed) — never by
    /// rewriting one. A file already there for that state is success when
    /// it is the same record's (a retried advance), an error otherwise; a
    /// write that fails removes the file it created.
    pub(crate) fn write(&self) -> Result<(), String> {
        if local_rank(&self.state).is_none() {
            return Err(format!(
                "알 수 없는 로컬 기록 상태 '{}'는 쓰지 않습니다",
                self.state
            ));
        }
        let path = local_state_path(&self.finalize_id, &self.state)
            .ok_or("이 PC의 로컬 설정 폴더(LOCALAPPDATA / XDG_CONFIG_HOME)를 찾지 못했습니다")?;
        let fail = |err: &dyn std::fmt::Display| {
            format!(
                "열린 최종화 로컬 기록 {}을(를) 만들지 못했습니다: {err}",
                path.display()
            )
        };
        #[cfg(test)]
        if FAIL_LOCAL_STATE.with(|cell| cell.get()) == Some(self.state.as_str()) {
            return Err(fail(&"test: injected local record creation failure"));
        }
        #[cfg(test)]
        if self.state != LOCAL_OPEN && FAIL_LOCAL_PAST_OPEN.with(|cell| cell.get()) {
            return Err(fail(&"test: injected full disk (nothing past open)"));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| fail(&err))?;
        }
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        let mut file = match created {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return match read_local_file(&path) {
                    Ok(existing)
                        if existing.finalize_id == self.finalize_id
                            && existing.state == self.state =>
                    {
                        Ok(())
                    }
                    Ok(_) => Err(fail(&"다른 기록이 이미 있습니다")),
                    Err(err) => Err(fail(&err)),
                };
            }
            Err(err) => return Err(fail(&err)),
        };
        let written = file
            .write_all(self.to_json().as_bytes())
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all());
        drop(file);
        if let Err(err) = written {
            let _ = std::fs::remove_file(&path);
            return Err(fail(&err));
        }
        crate::util::sync_parent_directory(&path);
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    /// R16 test hook: creating the local record file of this state fails
    /// (on this thread).
    pub(crate) static FAIL_LOCAL_STATE: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
    /// R17 test hook: the local record folder takes the `open` file but no
    /// state file after it (a disk that filled up mid-finalize).
    pub(crate) static FAIL_LOCAL_PAST_OPEN: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// R16 (P24): one local record file, parsed (its name's state is checked by
/// the caller).
fn read_local_file(path: &Path) -> Result<LocalRecord, String> {
    let bytes = read_capped(path, JOURNAL_MAX_BYTES).map_err(|err| err.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "UTF-8 텍스트가 아닙니다".to_string())?;
    LocalRecord::parse(&text)
}

/// R14 (P18): the host-local state folder — `%LOCALAPPDATA%\frametrace`
/// on Windows (not roaming: a record must never follow the user to
/// another PC; `%APPDATA%\frametrace`, the config dir, when LOCALAPPDATA is
/// unset), the config dir elsewhere (`$XDG_CONFIG_HOME/frametrace`,
/// `~/.config/frametrace`). Harnesses isolate it with those variables.
#[cfg(not(test))]
fn local_state_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(base) = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(base).join("frametrace"));
    }
    crate::audit_key::config_dir()
}

/// Unit tests never touch the user's real local state: one scratch base per
/// test process. A CLI child a test starts gets it through
/// [`isolate_local_state`] (`LOCALAPPDATA` / `XDG_CONFIG_HOME`).
#[cfg(test)]
fn local_state_dir() -> Option<PathBuf> {
    Some(test_local_base().join("frametrace"))
}

#[cfg(test)]
pub(crate) fn test_local_base() -> PathBuf {
    std::env::temp_dir().join(format!("ft-test-local-{}", std::process::id()))
}

/// Test helper: points a CLI child at this test process's local state.
#[cfg(test)]
pub(crate) fn isolate_local_state(command: &mut std::process::Command) {
    let base = test_local_base();
    command.env("LOCALAPPDATA", &base);
    command.env("XDG_CONFIG_HOME", &base);
}

/// R14 (P18): where this PC keeps its open-finalize records.
pub(crate) fn local_open_dir() -> Option<PathBuf> {
    local_state_dir().map(|dir| dir.join(LOCAL_OPEN_DIR_NAME))
}

/// Ids are 128-bit random hex ([`super::jobs`]); anything else (a journal
/// naming `..\x`) never becomes a path.
fn valid_finalize_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// R16 (P24): the file that records `state` of `finalize_id` — `<id>.json`
/// for `open` (the R14 / R15 name), `<id>.<state>.json` for the others.
fn local_state_path(finalize_id: &str, state: &str) -> Option<PathBuf> {
    let name = if state == LOCAL_OPEN {
        format!("{finalize_id}.json")
    } else {
        format!("{finalize_id}.{state}.json")
    };
    valid_finalize_id(finalize_id)
        .then(|| local_open_dir().map(|dir| dir.join(name)))
        .flatten()
}

/// R16 (P24): every file the local record of `finalize_id` may consist of,
/// lowest state first.
fn local_record_paths(finalize_id: &str) -> Vec<(&'static str, PathBuf)> {
    LOCAL_STATE_ORDER
        .iter()
        .filter_map(|state| local_state_path(finalize_id, state).map(|path| (*state, path)))
        .collect()
}

/// R14 (P18) / R16 (P24): this PC's local record of `finalize_id`
/// (`Ok(None)`: none) — the HIGHEST state whose file is present. Every file
/// present must parse, name that id and state and describe the same
/// finalize; otherwise the record is unreadable (never "lower"). The
/// manifest SHA-256 is the highest state's, or the closest lower one's.
pub(crate) fn read_local_record(finalize_id: &str) -> Result<Option<LocalRecord>, String> {
    let mut found: Vec<LocalRecord> = Vec::new();
    for (state, path) in local_record_paths(finalize_id) {
        let record = match read_local_file(&path) {
            Ok(record) => record,
            Err(_) if std::fs::symlink_metadata(&path).is_err() => continue,
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };
        if record.finalize_id != finalize_id {
            return Err(format!(
                "{}: 파일 이름과 finalize_id가 다릅니다",
                path.display()
            ));
        }
        if record.state != state {
            return Err(format!(
                "{}: 파일 이름의 상태({state})와 기록의 상태({})가 다릅니다",
                path.display(),
                record.state
            ));
        }
        if let Some(first) = found.first()
            && (first.case_dir != record.case_dir
                || first.signoff_sha256 != record.signoff_sha256
                || first.previous_signoff_sha256 != record.previous_signoff_sha256
                || first.package_name != record.package_name
                || first.host_id != record.host_id)
        {
            return Err(format!(
                "{}: 같은 최종화의 로컬 기록 파일들이 서로 다른 최종화를 가리킵니다",
                path.display()
            ));
        }
        found.push(record);
    }
    let manifest = found
        .iter()
        .rev()
        .find_map(|record| record.manifest_sha256.clone());
    Ok(found.pop().map(|mut top| {
        top.manifest_sha256 = top.manifest_sha256.or(manifest);
        top
    }))
}

/// R14 (P18): removes this PC's local record of `finalize_id` (NotFound is
/// success). R15 (P21): retried (an indexer or antivirus handle on a fresh
/// file). R16 (P24): file by file, LOWEST state first, stopping at the
/// first that cannot be removed — what is left is never a lower state than
/// what was there (a held `<id>.json` keeps `packaged` / `completed`).
pub(crate) fn remove_local_record(finalize_id: &str) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_LOCAL_RECORD_REMOVAL.with(|cell| cell.get()) {
        return Err(format!(
            "{finalize_id}: test: injected local record removal failure"
        ));
    }
    for (_, path) in local_record_paths(finalize_id) {
        if std::fs::symlink_metadata(&path).is_err() {
            continue;
        }
        crate::package::retry_within_notify(
            local_removal_budget(),
            &mut || crate::util::remove_file_durable(&path),
            &crate::package::is_final_rename_error,
            &mut |_| {},
        )
        .map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(())
}

/// R16 (P24): the record of `finalize_id` is past `open` or absent once the
/// finalize closed with `outcome` — `completed` for a complete entry,
/// `resolved` otherwise — creating that state's file when the record is
/// lower. Before a journal goes: a journal removed while the record still
/// reads `open` is the R14-S1 / R15-S1 rollback again.
fn close_local(
    journal: &FinalizeJournal,
    case_dir: &Path,
    outcome: &str,
    manifest_sha256: Option<String>,
) -> Result<Settled, String> {
    let Some(record) = read_local_record(&journal.finalize_id)? else {
        return Ok(Settled::Advanced);
    };
    let target = if outcome == STATE_COMPLETED {
        LOCAL_COMPLETED
    } else {
        LOCAL_RESOLVED
    };
    let done = if target == LOCAL_COMPLETED {
        record.state == LOCAL_COMPLETED
    } else {
        record.state != LOCAL_OPEN
    };
    if done {
        return Ok(Settled::Advanced);
    }
    advance_or_drop(journal, case_dir, target, manifest_sha256)
}

/// R17 (P28): how [`advance_or_drop`] left this PC's local record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settled {
    /// The state file was created (or the record was already there).
    Advanced,
    /// It could not be created, so the `open` file — the only thing a
    /// recovery ever rolls back on — was deleted instead: no record (a
    /// journal naming it is `unpaired`, "로컬 기록 없음") or what is left is
    /// past `open`.
    Dropped,
}

/// R17 (P28): advances the local record of `journal`'s finalize to
/// `state`; when that state file cannot be created (a full disk, an ACL
/// change) while the record still reads `open` (or cannot be read), the
/// `<id>.json` (open) file is deleted ON THE SPOT — deleting needs no free
/// space, and without it nothing is ever rolled back. R16 left the record
/// `open`, and a cut lane tail plus a rebuilt journal rolled the finished
/// finalize back. `Err` only when the record is past `open` already and
/// the higher state still failed (R16 behavior: the caller keeps the
/// journal), or when the `open` file could not be deleted either (the
/// caller keeps the journal — the one residual window).
pub(crate) fn advance_or_drop(
    journal: &FinalizeJournal,
    case_dir: &Path,
    state: &str,
    manifest_sha256: Option<String>,
) -> Result<Settled, String> {
    let err = match journal.advance_local(case_dir, state, manifest_sha256) {
        Ok(()) => return Ok(Settled::Advanced),
        Err(err) => err,
    };
    if matches!(
        read_local_record(&journal.finalize_id),
        Ok(Some(record)) if record.state != LOCAL_OPEN
    ) {
        return Err(err);
    }
    remove_open_file(&journal.finalize_id)
        .map(|()| Settled::Dropped)
        .map_err(|removal| format!("{err}; 열린 기록 파일도 지우지 못했습니다: {removal}"))
}

/// R17 (P28): removes only the `open` file (`<id>.json`) of `finalize_id`
/// (absent is success); retried like the other record files.
fn remove_open_file(finalize_id: &str) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_LOCAL_RECORD_REMOVAL.with(|cell| cell.get()) {
        return Err(format!(
            "{finalize_id}: test: injected local record removal failure"
        ));
    }
    let Some(path) = local_state_path(finalize_id, LOCAL_OPEN) else {
        return Ok(());
    };
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(());
    }
    crate::package::retry_within_notify(
        local_removal_budget(),
        &mut || crate::util::remove_file_durable(&path),
        &crate::package::is_final_rename_error,
        &mut |_| {},
    )
    .map_err(|err| format!("{}: {err}", path.display()))
}

/// R17 (P28): the examiner-facing note for a record [`Settled::Dropped`].
pub(crate) fn dropped_note(finalize_id: &str) -> String {
    format!(
        "이 PC의 최종화 로컬 기록(ID {})에 다음 상태를 남기지 못해(디스크 공간·권한) 열린 기록 파일을 지웠습니다 — 이 최종화는 앞으로 자동으로 되돌려지지 않습니다.",
        short(finalize_id)
    )
}

#[cfg(test)]
thread_local! {
    /// R15-S1 test hook: removing a local record fails (on this thread).
    pub(crate) static FAIL_LOCAL_RECORD_REMOVAL: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// R15 (P23): what the sweep knows about the case an `open` record names.
enum CaseVerdict {
    /// The case is there and holds no journal of the record (or the lane
    /// closed it): the record stands for nothing left to recover.
    Done,
    /// The case holds the record's journal: kept.
    Pending,
    /// The case folder or its journal cannot be read: never swept.
    Unknown,
}

fn case_verdict(record: &LocalRecord) -> CaseVerdict {
    let case = Path::new(&record.case_dir);
    match std::fs::metadata(case) {
        Ok(meta) if meta.is_dir() => {}
        _ => return CaseVerdict::Unknown,
    }
    match read_journal(case) {
        Ok(None) => CaseVerdict::Done,
        Ok(Some(journal)) if journal.finalize_id != record.finalize_id => CaseVerdict::Done,
        Ok(Some(_)) => CaseVerdict::Pending,
        Err(_) => CaseVerdict::Unknown,
    }
}

/// R17-S6: an unreadable record of `finalize_id` stands for nothing left to
/// recover — at least one of its files parses (naming that id) and every
/// such file's case is [`CaseVerdict::Done`]. A record none of whose files
/// parses is kept (nothing says which case it belongs to).
fn unreadable_record_done(finalize_id: &str) -> bool {
    let parsed: Vec<LocalRecord> = local_record_paths(finalize_id)
        .iter()
        .filter_map(|(_, path)| read_local_file(path).ok())
        .filter(|record| record.finalize_id == finalize_id)
        .collect();
    !parsed.is_empty()
        && parsed
            .iter()
            .all(|record| matches!(case_verdict(record), CaseVerdict::Done))
}

/// R15-S1: the case's lane holds a TRUSTED complete / resolved entry of
/// the record's finalize (a reachable case only).
fn lane_closed(record: &LocalRecord) -> bool {
    let case = Path::new(&record.case_dir);
    case.is_dir()
        && matches!(
            crate::report::finalize_lane_pairing(case, &record.finalize_id),
            FinalizeLanePairing::Completed { .. } | FinalizeLanePairing::Resolved { .. }
        )
}

/// R14-S13 / R15-S1/S3: sweeps the local record folder at server start.
/// An `open` record whose case lane holds a trusted complete / resolved
/// entry of it is closed at once (R15-S1: a finalize whose last step — the
/// removal — failed). Otherwise a record older than
/// [`LOCAL_RECORD_EXPIRY`] goes when it no longer stands for anything left
/// to recover: its case is there and no longer holds its journal (R16-S7:
/// whatever its state — R15 swept every record past `open` by age alone,
/// so a `packaged` record of a case on an unplugged drive was gone when
/// the drive came back and its finished finalize read "로컬 기록 없음"), or
/// it does not parse. P23: a record whose case cannot be reached (a drive
/// not plugged in) or whose journal does not parse is "unknown" — never
/// swept. R16 (P24): one record is all its state files (removed lowest
/// first). Returns how many records were removed.
pub(crate) fn sweep_local_records() -> usize {
    let Some(dir) = local_open_dir() else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let now = crate::util::now_unix().unwrap_or(0);
    let expired = |unix: u64| now.saturating_sub(unix) > LOCAL_RECORD_EXPIRY.as_secs();
    // id → newest mtime of its files; other `.json` files on their own.
    let mut ids: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut strays: Vec<(PathBuf, u64)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let modified_unix = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(now);
        let (id, state) = stem.split_once('.').unwrap_or((stem, LOCAL_OPEN));
        if valid_finalize_id(id) && local_rank(state).is_some() {
            let newest = ids.entry(id.to_string()).or_insert(0);
            *newest = (*newest).max(modified_unix);
        } else {
            strays.push((path, modified_unix));
        }
    }
    let mut removed = 0;
    for (path, modified_unix) in strays {
        let parses = read_local_file(&path).is_ok();
        if !parses && expired(modified_unix) && crate::util::remove_file_durable(&path).is_ok() {
            removed += 1;
        }
    }
    for (id, newest) in ids {
        let stale = match read_local_record(&id) {
            Ok(None) => false,
            // R17-S6: an unreadable record is judged like the others — by
            // the case its readable files name (R16 swept it by age alone,
            // its case unreachable or still holding its journal).
            Err(_) => expired(newest) && unreadable_record_done(&id),
            Ok(Some(record)) if record.state == LOCAL_OPEN => {
                lane_closed(&record)
                    || (expired(record.updated_unix.max(record.started_unix))
                        && matches!(case_verdict(&record), CaseVerdict::Done))
            }
            Ok(Some(record)) => {
                expired(record.updated_unix.max(record.started_unix))
                    && matches!(case_verdict(&record), CaseVerdict::Done)
            }
        };
        if stale && remove_local_record(&id).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// R13-S11: keeps the bytes of the sign-off a finalize replaces beside the
/// journal (durably), or removes a stale copy when there was none.
pub(crate) fn write_previous_signoff(case_dir: &Path, bytes: Option<&[u8]>) -> Result<(), String> {
    let path = previous_signoff_path(case_dir);
    match bytes {
        Some(bytes) => crate::util::write_bytes_atomic(&path, bytes),
        None => crate::util::remove_file_durable(&path),
    }
    .map_err(|err| {
        format!(
            "이전 검토자 기록 보관본 {}을(를) 쓰지 못했습니다: {err}",
            path.display()
        )
    })
}

/// Removes the journal of `finalize_id` and the previous sign-off kept
/// beside it (retried like the sign-off renames: an indexer or antivirus
/// handle on the fresh file must not leave it behind). R14-S4: a journal
/// that is not `finalize_id`'s (another PC's finalize of a shared case
/// created it meanwhile — R13 deleted it) is left alone; no journal is
/// success.
pub(crate) fn remove_journal(case_dir: &Path, finalize_id: &str) -> Result<(), String> {
    remove_journal_within(case_dir, finalize_id, crate::package::rename_retry_budget())
}

fn remove_journal_within(
    case_dir: &Path,
    finalize_id: &str,
    budget: Duration,
) -> Result<(), String> {
    let path = journal_path(case_dir);
    match read_journal(case_dir) {
        Ok(None) => return Ok(()),
        Ok(Some(journal)) if journal.finalize_id == finalize_id => {}
        Ok(Some(_)) | Err(_) => {
            return Err(format!(
                "{}은(는) 이 최종화({})의 저널이 아니어서 지우지 않았습니다",
                path.display(),
                short(finalize_id)
            ));
        }
    }
    crate::package::retry_within_notify(
        budget,
        &mut || crate::util::remove_file_durable(&path),
        &crate::package::is_final_rename_error,
        &mut |_| {},
    )
    .map_err(|err| format!("{}: {err}", path.display()))?;
    let _ = crate::util::remove_file_durable(&previous_signoff_path(case_dir));
    Ok(())
}

/// R13-S6/S5: moves the journal (and the previous sign-off beside it) to
/// `review/finalize.<label>-<unix>[-n].json` — never deleted. Returns the
/// new path of the journal.
pub(crate) fn set_aside(case_dir: &Path, label: &str) -> Result<PathBuf, String> {
    let path = journal_path(case_dir);
    let stamp = crate::util::now_unix().unwrap_or(0);
    let review = case_dir.join("review");
    let target = (0..1000u32)
        .map(|n| {
            if n == 0 {
                review.join(format!("finalize.{label}-{stamp}.json"))
            } else {
                review.join(format!("finalize.{label}-{stamp}-{n}.json"))
            }
        })
        .find(|candidate| std::fs::symlink_metadata(candidate).is_err())
        .ok_or("옮길 이름을 찾지 못했습니다")?;
    crate::util::rename_durable(&path, &target)
        .map_err(|err| format!("{} → {}: {err}", path.display(), target.display()))?;
    let previous = previous_signoff_path(case_dir);
    if previous.exists() {
        let beside = target.with_extension("previous-signoff.bin");
        let _ = crate::util::rename_durable(&previous, &beside);
    }
    Ok(target)
}

/// The `reports/<name>` folder a finalize reserved, when it holds a
/// published package (a plain folder with `manifest.sha256`) not already
/// marked incomplete.
pub(crate) fn published_reserved(case_dir: &Path, package_name: &str) -> Option<PathBuf> {
    if !crate::package::valid_reserved_package_name(package_name) {
        return None;
    }
    let dir = case_dir.join("reports").join(package_name);
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    (meta.is_dir()
        && !meta.file_type().is_symlink()
        && dir.join("manifest.sha256").is_file()
        && crate::package::finalize_incomplete_reason(&dir).is_none())
    .then_some(dir)
}

/// The `manifest.sha256` of a package is read up to this size.
const MANIFEST_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// R15-S9: a manifest with more lines than this is not judged (the status
/// poll walks it).
const MANIFEST_MAX_LINES: usize = 100_000;

/// R15 (P21 / S2 / S9): SHA-256 of `package`'s `manifest.sha256`, read up
/// to [`MANIFEST_MAX_BYTES`] (the same value as
/// [`super::jobs::package_manifest_sha256`] for any manifest within it).
pub(crate) fn manifest_sha256_capped(package: &Path) -> Option<String> {
    read_capped(&package.join("manifest.sha256"), MANIFEST_MAX_BYTES)
        .ok()
        .map(|bytes| crate::sha256::digest_bytes(&bytes))
}

/// R16-S7: a manifest is hashed (streaming) up to this size when a
/// `packaged` record is judged.
const MANIFEST_STREAM_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// R16-S5/S7: SHA-256 of `package`'s `manifest.sha256` computed STREAMING
/// (constant memory) up to [`MANIFEST_STREAM_MAX_BYTES`] — what a
/// `packaged` / `completed` record is compared with. R15 read it whole up
/// to 16 MiB, so every package with a larger manifest read
/// `package-changed`. The same value as
/// [`super::jobs::package_manifest_sha256`].
pub(crate) fn manifest_sha256_streaming(package: &Path) -> Option<String> {
    let path = package.join("manifest.sha256");
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MANIFEST_STREAM_MAX_BYTES {
        return None;
    }
    let file = std::fs::File::open(&path).ok()?;
    let mut limited = std::io::BufReader::new(file).take(MANIFEST_STREAM_MAX_BYTES + 1);
    let mut counted = CountingReader {
        inner: &mut limited,
        read: 0,
    };
    let sha = crate::sha256::digest_reader(&mut counted).ok()?;
    (counted.read <= MANIFEST_STREAM_MAX_BYTES).then_some(sha)
}

struct CountingReader<'a, R: Read> {
    inner: &'a mut R,
    read: u64,
}

impl<R: Read> Read for CountingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}

/// R15-S8: `package`'s packaged sign-off (`review/signoff.json`, read
/// capped) hashes to `signoff_sha256` — the package is that finalize's,
/// not another one published under the same name.
fn package_signoff_is(package: &Path, signoff_sha256: &str) -> bool {
    read_capped(
        &package.join(crate::report::SIGNOFF_REL_PATH),
        PREVIOUS_SIGNOFF_MAX_BYTES,
    )
    .is_ok_and(|bytes| crate::sha256::digest_bytes(&bytes) == signoff_sha256)
}

/// R15-S8: the published reserved package of the finalize of sign-off
/// `signoff_sha256` — [`published_reserved`] whose packaged sign-off is
/// that one.
fn own_reserved(case_dir: &Path, package_name: &str, signoff_sha256: &str) -> Option<PathBuf> {
    published_reserved(case_dir, package_name).filter(|dir| package_signoff_is(dir, signoff_sha256))
}

/// R15-S4: SHA-256 of this process's executable (computed once).
pub(crate) fn own_exe_sha256() -> Option<String> {
    static OWN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    OWN.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| crate::audit::digest_file(&exe).ok())
    })
    .clone()
}

/// R15-S4: SHA-256 of an executable file, cached by (path, size, mtime) —
/// the status poll asks about the same live writer every time.
fn image_sha256(path: &Path) -> Option<String> {
    type Entry = (PathBuf, u64, SystemTime, String);
    static CACHE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
    let meta = std::fs::metadata(path).ok()?;
    let (len, modified) = (meta.len(), meta.modified().ok()?);
    if let Some(hit) = CACHE.lock().ok().and_then(|cache| {
        cache
            .iter()
            .find(|(p, l, m, _)| p == path && *l == len && *m == modified)
            .map(|entry| entry.3.clone())
    }) {
        return Some(hit);
    }
    let sha = crate::audit::digest_file(path).ok()?;
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|(p, ..)| p != path);
        keep_newest(&mut cache, 8);
        cache.push((path.to_path_buf(), len, modified, sha.clone()));
    }
    Some(sha)
}

/// R16-S6: makes room for one more entry in a small cache by dropping the
/// OLDEST ones (R15's `truncate` kept the oldest and dropped the newer).
fn keep_newest<T>(entries: &mut Vec<T>, capacity: usize) {
    let excess = (entries.len() + 1).saturating_sub(capacity);
    entries.drain(..excess.min(entries.len()));
}

/// R15-S4: two folders are the same one (canonical; case-insensitive on
/// Windows).
fn same_folder(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (a, b) = (canonical(a), canonical(b));
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// R14 (P19): whether the finalize of sign-off `signoff_sha256` and
/// reserved package `package_name` FINISHED, judged from what is outside
/// the lane: `review/signoff.json` is that sign-off, and the reserved
/// package is published (plain folder, not marked incomplete) with a
/// complete `manifest.sha256` — newline-terminated, every line `<sha256>
/// <relative path>` naming a regular file that exists — that lists
/// `review/signoff.json` with that SHA-256, and the packaged sign-off
/// hashes to it. Only the packaged sign-off is hashed (the rest is
/// verify-package's job); returns the manifest's SHA-256.
///
/// R15-S2: no longer what decides that a finalize of THIS PC finished (a
/// forged manifest line and a sign-off copy satisfied it) — that is the
/// `packaged` local record. Only another PC's journal is judged by it
/// ([`mark_other_host_package`]: whether to mark its package beside).
pub(crate) fn finished_evidence(
    case_dir: &Path,
    signoff_sha256: &str,
    package_name: &str,
) -> Option<String> {
    let current = read_capped(
        &case_dir.join(crate::report::SIGNOFF_REL_PATH),
        PREVIOUS_SIGNOFF_MAX_BYTES,
    )
    .ok()?;
    if crate::sha256::digest_bytes(&current) != signoff_sha256 {
        return None;
    }
    let package = published_reserved(case_dir, package_name)?;
    package_carries_signoff(&package, signoff_sha256)
        .then(|| manifest_sha256_capped(&package))
        .flatten()
}

/// R14 (P19 / S8): `package`'s manifest is complete (see
/// [`finished_evidence`]) and its packaged sign-off is `signoff_sha256`.
fn package_carries_signoff(package: &Path, signoff_sha256: &str) -> bool {
    let Ok(bytes) = read_capped(&package.join("manifest.sha256"), MANIFEST_MAX_BYTES) else {
        return false;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return false;
    };
    let Some(body) = text.strip_suffix('\n') else {
        return false;
    };
    let mut signoff_listed = false;
    if body.split('\n').nth(MANIFEST_MAX_LINES).is_some() {
        return false;
    }
    for line in body.split('\n') {
        let Some((hash, rel)) = line.split_once("  ") else {
            return false;
        };
        let relative = Path::new(rel);
        let safe = hash.len() == 64
            && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            && !rel.is_empty()
            && relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)));
        if !safe
            || !std::fs::symlink_metadata(package.join(relative)).is_ok_and(|meta| meta.is_file())
        {
            return false;
        }
        if rel == crate::report::SIGNOFF_REL_PATH {
            if hash != signoff_sha256 {
                return false;
            }
            signoff_listed = true;
        }
    }
    signoff_listed && package_signoff_is(package, signoff_sha256)
}

/// R14-S8: another PC's journal older than [`OTHER_HOST_BLOCK`] is moved
/// aside by a finalize here (never recovered). The package that finalize
/// reserved gets the finalize-incomplete marker BESIDE it (never renamed —
/// this PC cannot tell what the other one did) when it is that finalize's
/// own package (its packaged sign-off is the journal's), the lane holds
/// that finalize's begin entry naming it and no complete / resolved entry
/// for it (any trust), and it is not finished ([`finished_evidence`]).
/// Returns the sentence for the job log.
pub(crate) fn mark_other_host_package(
    case_dir: &Path,
    journal: &FinalizeJournal,
) -> Option<String> {
    let package = published_reserved(case_dir, &journal.package_name)?;
    if !package_carries_signoff(&package, &journal.signoff_sha256)
        || finished_evidence(case_dir, &journal.signoff_sha256, &journal.package_name).is_some()
        || !crate::report::finalize_lane_mentions(
            case_dir,
            &journal.finalize_id,
            &journal.package_name,
        )
    {
        return None;
    }
    let marker = crate::package::finalize_incomplete_marker(&package);
    let why = format!(
        "다른 PC(호스트 {})의 최종화가 끝났는지 확인되지 않음",
        short(&journal.host_id)
    );
    Some(
        match crate::util::write_text_atomic(&marker, &incomplete_marker_text(&package, &why)) {
            Ok(()) => format!(
                "다른 PC의 끝나지 않은 최종화가 예약한 패키지 {} 옆에 최종화 미완료 표식 {}을(를) 남겼습니다 (이름은 바꾸지 않음).",
                package.display(),
                marker.display()
            ),
            Err(err) => format!(
                "다른 PC의 끝나지 않은 최종화가 예약한 패키지 {}에 최종화 미완료 표식을 남기지 못했습니다({err}) — 최종 결과로 쓰지 마십시오.",
                package.display()
            ),
        },
    )
}

/// R12-S5 / R13-S2: complete packages a package-case kept under its
/// work-folder name (`.<reserved>.ft<pid>.part`, marked preserved) because
/// the publish rename failed — only those meant for `package_name`.
fn kept_parts_for(case_dir: &Path, package_name: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(case_dir.join("reports")) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            crate::join::is_pid_part_name(&name, None)
                && crate::join::package_reservation_of(&name).as_deref() == Some(package_name)
        })
        .filter(super::media::is_plain_dir_entry)
        .map(|entry| entry.path())
        .filter(|path| crate::join::part_is_preserved(path))
        .filter(|path| crate::package::finalize_incomplete_reason(path).is_none())
        .collect();
    found.sort();
    found
}

/// The marker text beside a package an unfinished finalize left.
fn incomplete_marker_text(package: &Path, why: &str) -> String {
    format!(
        "FrameTrace: 최종화 미완료 — 이 패키지({})는 끝나지 않은 최종화가 만든 것입니다 ({why}). 이 패키지가 담은 검토자 기록은 확정되지 않았고 감사 로그에 '완료되지 않음'으로 기록됩니다. 최종 결과로 쓰지 말고, 이름을 바꾸지 말고, 최종화를 다시 실행하십시오.\nFinalize incomplete: this package was written by a finalize that did not complete; its sign-off was never confirmed. Do not use it as the final result, do not rename it, finalize again.\n",
        package.display()
    )
}

/// R12-S1 (P14): renames a package an unfinished finalize published to
/// `<name>_INCOMPLETE` (`_INCOMPLETE_2`, … when taken) and writes the
/// marker beside it. Returns the examiner-facing sentence, the folder as it
/// is now and whether it now reads as incomplete (renamed or marked).
/// Never deletes anything.
pub(crate) fn mark_package_incomplete(
    package: &Path,
    why: &str,
    budget: Duration,
) -> (String, PathBuf, bool) {
    let name = package
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut renamed: Result<PathBuf, String> = Err("이름 후보가 없습니다".to_string());
    for n in 1..100u32 {
        let suffix = if n == 1 {
            crate::package::INCOMPLETE_PACKAGE_SUFFIX.to_string()
        } else {
            format!("{}_{n}", crate::package::INCOMPLETE_PACKAGE_SUFFIX)
        };
        let target = package.with_file_name(format!("{name}{suffix}"));
        if std::fs::symlink_metadata(&target).is_ok() {
            continue;
        }
        renamed = crate::package::retry_within_notify(
            budget,
            &mut || crate::util::rename_durable(package, &target),
            &crate::package::is_final_rename_error,
            &mut |_| {},
        )
        .map(|()| target)
        .map_err(|err| err.to_string());
        break;
    }
    let (now, rename_note, renamed_ok) = match renamed {
        Ok(target) => {
            let note = format!("폴더 이름을 {}(으)로 바꿨고", target.display());
            (target, note, true)
        }
        Err(err) => (
            package.to_path_buf(),
            format!("폴더 이름은 바꾸지 못했고({err})"),
            false,
        ),
    };
    let marker = crate::package::finalize_incomplete_marker(&now);
    let (marker_note, marked) =
        match crate::util::write_text_atomic(&marker, &incomplete_marker_text(&now, why)) {
            Ok(()) => (
                format!("옆에 표식 {}을(를) 남겼습니다", marker.display()),
                true,
            ),
            Err(err) => (
                format!("표식 {}도 남기지 못했습니다({err})", marker.display()),
                false,
            ),
        };
    (
        format!(
            "이번 최종화의 패키지는 이미 {}에 만들어졌지만 최종화는 끝나지 않았습니다 — {rename_note} {marker_note}. 이 패키지를 최종 결과로 쓰지 마십시오.",
            package.display()
        ),
        now,
        renamed_ok || marked,
    )
}

/// R12-S5: a complete package kept under its work-folder name gets the
/// finalize-incomplete marker beside it under that name AND under the name
/// it was meant to have, and the preserved-part note is rewritten to say it
/// is not a final result. Returns the sentence and (R13-S8) whether every
/// marker was written.
fn mark_kept_part_incomplete(part: &Path, why: &str) -> (String, bool) {
    let part_name = part
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut markers = vec![crate::package::finalize_incomplete_marker(part)];
    if let Some(reserved) = crate::join::package_reservation_of(&part_name) {
        markers.push(crate::package::finalize_incomplete_marker(
            &part.with_file_name(reserved),
        ));
    }
    let text = incomplete_marker_text(part, why);
    let mut failed: Vec<String> = Vec::new();
    for marker in &markers {
        if let Err(err) = crate::util::write_text_atomic(marker, &text) {
            failed.push(format!("{}: {err}", marker.display()));
        }
    }
    // The preserved note must stay (it keeps the stale sweep away); only
    // its advice changes.
    let _ = crate::util::write_text_atomic(&crate::join::preserved_part_marker(part), &text);
    if failed.is_empty() {
        (
            format!(
                "package-case가 남긴 완성된 패키지 {}는 끝나지 않은 최종화의 것입니다 — 옆에 최종화 미완료 표식을 남겼습니다. 이름을 바꿔 쓰지 말고 최종화를 다시 실행하십시오.",
                part.display()
            ),
            true,
        )
    } else {
        (
            format!(
                "package-case가 남긴 완성된 패키지 {}는 끝나지 않은 최종화의 것입니다 — 최종화 미완료 표식을 남기지 못했습니다({}). 이 폴더를 최종 결과로 쓰지 말고 최종화를 다시 실행하십시오.",
                part.display(),
                failed.join(", ")
            ),
            false,
        )
    }
}

/// What [`mark_unfinished_packages`] did.
#[derive(Debug, Default)]
pub(crate) struct MarkOutcome {
    /// One sentence per folder.
    pub(crate) notes: Vec<String>,
    /// The incomplete package folders as they are now.
    pub(crate) dirs: Vec<PathBuf>,
    /// R13-S8: a folder could neither be renamed nor marked (it may still
    /// read as final) — blocking.
    pub(crate) failed: bool,
}

/// R12-S1/S5 (P14), R13-S2: marks what this finalize's package-case
/// published or kept — ONLY its reserved `package_name` (plus the folder
/// its own `output:` line names, if any) — as incomplete. R12 marked every
/// `reports/package_*` that was not in the journal's `reports_before`, so
/// a package another finalize made after a crash (or every package, with
/// a forged empty list) was renamed.
///
/// R15-S8: a recovery passes `only_signoff` (the begin entry's sign-off
/// SHA-256): a folder under the reserved name whose packaged sign-off is
/// another one (published by something else under that name — a fixed
/// name, a same-second collision) is not this finalize's and is left alone.
/// The live finalize (which saw its own package-case publish) passes
/// `None`.
pub(crate) fn mark_unfinished_packages(
    case_dir: &Path,
    package_name: &str,
    step_text: &str,
    why: &str,
    budget: Duration,
    only_signoff: Option<&str>,
) -> MarkOutcome {
    let ours = |dir: &PathBuf| only_signoff.is_none_or(|sha| package_signoff_is(dir, sha));
    let mut published: Vec<PathBuf> = published_reserved(case_dir, package_name)
        .into_iter()
        .filter(&ours)
        .collect();
    if let Some(named) = super::media::published_package_dir(case_dir, step_text) {
        let name = named
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !published.contains(&named)
            && !crate::package::is_incomplete_package_name(&name)
            && crate::package::finalize_incomplete_reason(&named).is_none()
            && ours(&named)
        {
            published.push(named);
        }
    }
    let mut outcome = MarkOutcome::default();
    for package in published {
        let (note, now, ok) = mark_package_incomplete(&package, why, budget);
        outcome.notes.push(note);
        outcome.dirs.push(now);
        outcome.failed |= !ok;
    }
    for part in kept_parts_for(case_dir, package_name)
        .into_iter()
        .filter(&ours)
    {
        let (note, ok) = mark_kept_part_incomplete(&part, why);
        outcome.notes.push(note);
        outcome.dirs.push(part);
        outcome.failed |= !ok;
    }
    outcome
}

/// R12-S7: writes `bytes` to `path` durably (fsync before the caller
/// renames it into place).
pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}

/// What [`recover`] did, for the job state and /api/status.
#[derive(Debug, Default)]
pub(crate) struct Recovery {
    /// Examiner-facing summary (Korean).
    pub(crate) detail: String,
    /// The report on disk still names the interrupted finalize's sign-off.
    pub(crate) report_stale: bool,
    /// Something is unresolved, unverifiable or someone else's (a sign-off
    /// not taken back, a package not marked, an unreadable / unpaired
    /// journal, a live writer, another PC's recent journal): a new
    /// finalize must not overwrite the journal.
    pub(crate) blocking: bool,
    /// R13: an interrupted finalize of this PC was actually cleaned up
    /// (status `finalize_recovered`). False when nothing was changed.
    pub(crate) recovered: bool,
    /// R13-S5: another PC's journal older than [`OTHER_HOST_BLOCK`] — a
    /// finalize moves it aside (never recovers it).
    pub(crate) set_aside: bool,
}

/// R14: why a journal is `unpaired` — a short code for track U
/// (`finalize_journal.unpaired_code`) and the examiner-facing reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnpairedWhy {
    pub(crate) code: &'static str,
    pub(crate) reason: String,
    /// R18-S2: the status path shows a recovery's remembered verdict
    /// (`manifest_check: "recovery-verdict"`), not one judged now.
    pub(crate) remembered: bool,
}

impl UnpairedWhy {
    fn new(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
            remembered: false,
        }
    }
}

/// R14 `unpaired_code` values.
pub(crate) const UNPAIRED_LEGACY: &str = "legacy-journal";
pub(crate) const UNPAIRED_LANE: &str = "lane";
pub(crate) const UNPAIRED_JOURNAL_MISMATCH: &str = "journal-mismatch";
pub(crate) const UNPAIRED_NO_LOCAL_RECORD: &str = "no-local-record";
pub(crate) const UNPAIRED_LOCAL_MISMATCH: &str = "local-mismatch";
pub(crate) const UNPAIRED_LOCAL_UNREADABLE: &str = "local-unreadable";
pub(crate) const UNPAIRED_HOST_UNKNOWN: &str = "host-unknown";
/// R15 (P21 / S2): this PC recorded the package-case result
/// (`packaged{manifest_sha256}`) and the reserved package's manifest is no
/// longer that one (changed, gone, renamed).
pub(crate) const UNPAIRED_PACKAGE_CHANGED: &str = "package-changed";
/// R15 (P21): the local record is in a state this build does not know.
pub(crate) const UNPAIRED_LOCAL_STATE: &str = "local-state";

/// R15 `in_progress_code` values (`finalize_journal.state: "in-progress"`).
pub(crate) const IN_PROGRESS_LIVE_WRITER: &str = "live-writer";
pub(crate) const IN_PROGRESS_OTHER_ACCOUNT: &str = "other-account";
/// R16-S2 (P25): a live FrameTrace process matches the writer's pid, start
/// time and name, but not this executable's folder / SHA-256.
pub(crate) const IN_PROGRESS_UNVERIFIED_WRITER: &str = "unverified-writer";

/// R13: what a journal on disk is, judged without changing anything.
#[derive(Debug, Clone)]
pub(crate) enum Assessment {
    Unreadable(String),
    /// Written on another PC (P17). `blocking` while younger than
    /// [`OTHER_HOST_BLOCK`].
    OtherHost {
        journal: FinalizeJournal,
        blocking: bool,
        until_unix: u64,
    },
    /// Its writer (same PC) still runs, and this PC holds the local record
    /// of that finalize (R15-S4: without one a live writer is only a note).
    LiveWriter(FinalizeJournal),
    /// R16-S2 (P25): a live process matches the writer's pid, start time
    /// and FrameTrace name but not this executable's folder / SHA-256, and
    /// this PC holds the local record — maybe a finalize still running
    /// (another FrameTrace install). Never recovered; `--dismiss --force`
    /// only moves its journal.
    UnverifiedWriter(FinalizeJournal),
    /// R15-S10: the writer pid is a FrameTrace process this account may not
    /// inspect — possibly another account's finalize on this PC. Blocks
    /// (no dismiss offered) until `until_unix` ([`OTHER_HOST_BLOCK`] after
    /// the journal's start).
    OtherAccount {
        journal: FinalizeJournal,
        image: String,
        until_unix: u64,
    },
    /// No trusted lane entry pairs with it (P15), or this PC holds no
    /// agreeing local open-finalize record (P18) — why.
    Unpaired(FinalizeJournal, UnpairedWhy),
    /// The lane records it complete (`completed`) or resolved (outcome).
    Closed(FinalizeJournal, String),
    /// R14 (P19 / S2), R15 (P21): the finalize finished — only its complete
    /// entry is missing from the lane. Why it is known to have finished
    /// (always this PC's local record past `open`) and (R16-S3) the
    /// manifest SHA-256 THIS PC recorded — what the complete entry
    /// written again carries (never the one on disk now).
    Finished {
        journal: FinalizeJournal,
        why: String,
        manifest_sha256: Option<String>,
        /// R17-S9 / R18-S2: how the manifest was judged — not `Hashed`
        /// only on the status path (over [`MANIFEST_MAX_BYTES`]); a
        /// recovery always hashes it (streaming).
        check: ManifestCheck,
    },
    /// An interrupted finalize of this PC: journal, local record (`open`)
    /// and the lane's begin entry agree.
    Open(FinalizeJournal, FinalizeBegin),
}

/// R18-S2 (P31): how a finished finalize's manifest was judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManifestCheck {
    /// Hashed now (in full on a recovery; up to 16 MiB on the status path).
    Hashed,
    /// Status path, over [`MANIFEST_MAX_BYTES`], no recovery verdict
    /// remembered for that manifest: unknown ("확인 보류",
    /// `blocks_finalize: null`).
    Withheld,
    /// Status path, over [`MANIFEST_MAX_BYTES`]: the verdict a recovery
    /// reached hashing it in full, remembered for that manifest's size and
    /// mtime ([`remembered_verdict`]).
    Remembered,
}

/// R18-S2 (P31): what a recovery (the manifest hashed in full) concluded
/// about a finalize whose local record is past `open` — remembered in
/// memory per (finalize id, manifest size + mtime) so the status path,
/// which never hashes a manifest over 16 MiB, shows it instead of "no
/// block". 891f180's status kept showing `completed-unrecorded` /
/// `blocks_finalize: false` while the recovery had judged the package
/// CHANGED and kept the journal.
#[derive(Debug, Clone)]
enum RecoveryVerdict {
    Finished { why: String },
    PackageChanged(UnpairedWhy),
}

/// R19-S4: (normalized case folder, finalize id, manifest size + mtime) —
/// R18 keyed only the finalize id, so a copy of the case (same journal,
/// same id, a manifest with the same size and mtime) was shown the
/// original's verdict.
type VerdictKey = (PathBuf, String, (u64, SystemTime));

fn recovery_verdicts() -> &'static Mutex<Vec<(VerdictKey, RecoveryVerdict)>> {
    static VERDICTS: Mutex<Vec<(VerdictKey, RecoveryVerdict)>> = Mutex::new(Vec::new());
    &VERDICTS
}

/// Remembers a full-hash assessment's verdict for the manifest stamped
/// `stamp` (taken before AND after the hash, equal — else nothing is
/// remembered) of `case_dir`. Any other assessment of that finalize in
/// that case forgets it.
fn remember_verdict(
    case_dir: &Path,
    finalize_id: &str,
    stamp: Option<(u64, SystemTime)>,
    assessment: &Assessment,
) {
    let verdict = match assessment {
        Assessment::Finished {
            why,
            check: ManifestCheck::Hashed,
            ..
        } => Some(RecoveryVerdict::Finished { why: why.clone() }),
        Assessment::Unpaired(_, why) if why.code == UNPAIRED_PACKAGE_CHANGED => {
            Some(RecoveryVerdict::PackageChanged(why.clone()))
        }
        _ => None,
    };
    let case = crate::paste_path::normalize(case_dir);
    if let Ok(mut verdicts) = recovery_verdicts().lock() {
        verdicts.retain(|((dir, id, _), _)| !(id == finalize_id && *dir == case));
        if let (Some(verdict), Some(stamp)) = (verdict, stamp) {
            keep_newest(&mut verdicts, 16);
            verdicts.push(((case, finalize_id.to_string(), stamp), verdict));
        }
    }
}

fn remembered_verdict(
    case_dir: &Path,
    finalize_id: &str,
    stamp: Option<(u64, SystemTime)>,
) -> Option<RecoveryVerdict> {
    let stamp = stamp?;
    let case = crate::paste_path::normalize(case_dir);
    recovery_verdicts().lock().ok().and_then(|verdicts| {
        verdicts
            .iter()
            .find(|((dir, id, at), _)| *dir == case && id == finalize_id && *at == stamp)
            .map(|(_, verdict)| verdict.clone())
    })
}

/// The words a remembered verdict is shown with.
const REMEMBERED_NOTE: &str = " (복구가 manifest.sha256 전체를 해시해 내린 판정 — 상태 확인은 16 MiB보다 큰 manifest를 다시 해시하지 않으며, 그 뒤 크기·수정 시각이 바뀌면 이 판정은 쓰지 않습니다)";

fn journal_age(journal: &FinalizeJournal) -> Option<Duration> {
    let now = crate::util::now_unix().ok()?;
    // A start time in the future (beyond clock slack) is not believed.
    if journal.started_unix > now + 300 {
        return None;
    }
    Some(Duration::from_secs(
        now.saturating_sub(journal.started_unix),
    ))
}

/// R13-S4 / R14-S3 / R15-S4: whether the journal's writer (on this PC)
/// still runs — a positive match of pid, start time and a FrameTrace
/// executable name (`util::writer_still_running`), AND (R15-S4) the live
/// process's executable is in this executable's folder and hashes to the
/// SHA-256 the journal records. R14 trusted the file NAME: any program
/// copied as `frametrace-x.exe` anywhere was a live writer.
///
/// R16-S2 (P25): a process that passes the R14 match (pid, start time,
/// FrameTrace name) but not the folder / SHA-256 check is
/// [`WriterLiveness::Unverified`] — maybe the writer (another FrameTrace
/// install or build), maybe not; R15 took it as gone, and `--dismiss`
/// from another folder's binary rolled a RUNNING finalize back.
fn writer_liveness(journal: &FinalizeJournal) -> WriterLiveness {
    let writer = &journal.writer;
    if writer.pid == std::process::id() {
        // This process: its own finalize holds `busy`, so a journal seen
        // by a recovery is an earlier, interrupted one.
        return WriterLiveness::Gone;
    }
    if !crate::util::writer_still_running(
        writer,
        crate::util::probe_process(writer.pid),
        crate::util::process_image_name(writer.pid).as_deref(),
    ) {
        return WriterLiveness::Gone;
    }
    let Some(recorded) = journal.writer_image_sha256.as_deref() else {
        return WriterLiveness::Unverified;
    };
    let Some(image) = crate::util::process_image_path(writer.pid) else {
        return WriterLiveness::Unverified;
    };
    let own_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    match (image.parent(), own_dir) {
        (Some(dir), Some(own)) if same_folder(dir, &own) => {}
        _ => return WriterLiveness::Unverified,
    }
    if image_sha256(&image).as_deref() == Some(recorded) {
        WriterLiveness::Verified
    } else {
        WriterLiveness::Unverified
    }
}

/// R16-S2 (P25): what is known about a journal's writer process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterLiveness {
    /// No live process matches the writer (or it is this process).
    Gone,
    /// A live FrameTrace process in this executable's folder with the
    /// recorded SHA-256.
    Verified,
    /// A live process matching pid, start time and FrameTrace name whose
    /// folder or SHA-256 could not be confirmed: maybe the writer.
    Unverified,
}

/// R15-S10: the journal's writer pid is a FrameTrace process this account
/// may not inspect (its executable name). R16-S4: only when the lane's
/// begin entry of the journal's finalize records that same pid
/// (`begin_pid`) — R15 let a planted journal naming any such pid (or a
/// reused one) block the case for 24 h.
/// `inaccessible` is `util::inaccessible_process_name` (tests pass a fake).
pub(crate) fn writer_other_account(
    journal: &FinalizeJournal,
    begin_pid: Option<u32>,
    inaccessible: impl Fn(u32) -> Option<String>,
) -> Option<String> {
    let writer = &journal.writer;
    if writer.pid == 0 || writer.pid == std::process::id() || begin_pid != Some(writer.pid) {
        return None;
    }
    inaccessible(writer.pid).filter(|name| crate::util::is_frametrace_image(name))
}

/// R16-S4: the pid the lane's begin entry records for the journal's
/// finalize (the latest unmatched trusted one).
fn begin_pid(pairing: &FinalizeLanePairing) -> Option<u32> {
    match pairing {
        FinalizeLanePairing::Open(begin) => Some(begin.pid),
        _ => None,
    }
}

/// What [`classify`] judges a readable journal by (gathered from the disk
/// outside every lock; cached by the status poll).
#[derive(Debug, Clone)]
struct Inputs {
    /// The lane's answer for the journal's id.
    pairing: FinalizeLanePairing,
    /// This PC's local record of the id (P18).
    local: Result<Option<LocalRecord>, String>,
    /// R15 (P21): SHA-256 of the reserved package's manifest now (the
    /// published, unmarked reserved package; R16-S7: hashed streaming).
    manifest_sha256: Option<String>,
    /// R17-S9: the status path did not hash the manifest (over
    /// [`MANIFEST_MAX_BYTES`]) — `manifest_sha256` is then `None` and
    /// means nothing.
    manifest_withheld: bool,
    /// R18-S2: size and mtime of the reserved package's manifest when it
    /// was judged (a full hash: equal before and after it, else `None`) —
    /// what a recovery's verdict is remembered by.
    manifest_stamp: Option<(u64, SystemTime)>,
}

/// The reserved package's `manifest.sha256` (published, unmarked).
fn reserved_manifest_path(case_dir: &Path, package_name: &str) -> Option<PathBuf> {
    published_reserved(case_dir, package_name).map(|dir| dir.join("manifest.sha256"))
}

fn manifest_stamp(case_dir: &Path, package_name: &str) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(reserved_manifest_path(case_dir, package_name)?).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// R16-S7: SHA-256 of the manifest of the published reserved package
/// `package_name` (streaming, see [`manifest_sha256_streaming`]) and
/// (R18-S2) its stamp — `None` unless the same before and after the hash.
fn reserved_manifest_sha256(
    case_dir: &Path,
    package_name: &str,
) -> (Option<String>, Option<(u64, SystemTime)>) {
    let before = manifest_stamp(case_dir, package_name);
    let sha =
        published_reserved(case_dir, package_name).and_then(|dir| manifest_sha256_streaming(&dir));
    let after = manifest_stamp(case_dir, package_name);
    (sha, before.filter(|_| before == after))
}

/// R17-S9: the status path's view of the reserved package's manifest —
/// hashed only up to [`MANIFEST_MAX_BYTES`]; a larger one is
/// `(None, true, stamp)` ("확인 보류", never the 1 GiB streaming hash a
/// recovery does: R16 ran that on every status cache miss).
fn status_manifest_sha256(
    case_dir: &Path,
    package_name: &str,
) -> (Option<String>, bool, Option<(u64, SystemTime)>) {
    let Some(dir) = published_reserved(case_dir, package_name) else {
        return (None, false, None);
    };
    let stamp = manifest_stamp(case_dir, package_name);
    match stamp {
        Some((len, _)) if len > MANIFEST_MAX_BYTES => (None, true, stamp),
        _ => (manifest_sha256_capped(&dir), false, stamp),
    }
}

/// What a journal is judged by. `full`: the recovery's view (manifest
/// hashed streaming up to 1 GiB); otherwise the status path's (R17-S9).
fn gather(case_dir: &Path, journal: &FinalizeJournal, full: bool) -> Inputs {
    if journal.finalize_id.is_empty() {
        return Inputs {
            pairing: FinalizeLanePairing::Unpaired(String::new()),
            local: Ok(None),
            manifest_sha256: None,
            manifest_withheld: false,
            manifest_stamp: None,
        };
    }
    let (manifest_sha256, manifest_withheld, manifest_stamp) = if full {
        let (sha, stamp) = reserved_manifest_sha256(case_dir, &journal.package_name);
        (sha, false, stamp)
    } else {
        status_manifest_sha256(case_dir, &journal.package_name)
    };
    Inputs {
        pairing: crate::report::finalize_lane_pairing(case_dir, &journal.finalize_id),
        local: read_local_record(&journal.finalize_id),
        manifest_sha256,
        manifest_withheld,
        manifest_stamp,
    }
}

/// R17-S3: the live process holding the writer's pid started when the
/// journal says its writer did — or one of the two start times is unknown
/// (then R16's rule stands). A known, different start time is a pid reused
/// since by another account's FrameTrace process: not the writer.
pub(crate) fn other_account_start_matches(
    journal: &FinalizeJournal,
    started: Option<SystemTime>,
) -> bool {
    let (Some(recorded), Some(started)) = (journal.writer.started_ms, started) else {
        return true;
    };
    let started_ms = started
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    started_ms.abs_diff(recorded) <= crate::util::START_TIME_SLACK.as_millis() as u64
}

/// R13/R14/R15/R16: classifies the journal read from disk. Order:
/// unreadable → host unknown → another PC → legacy → (in-progress)
/// another account's FrameTrace writer the lane's begin names (R16-S4) /
/// live or maybe-live writer with this PC's local record (R16-S2) → lane
/// complete / resolved → local record (none → "로컬 기록 없음") → local
/// record past `open` (P21: never Open; R16-S3: its manifest must still
/// be the package's) → lane unpaired → begin mismatch → Open.
///
/// R17-S7: a finalize the lane closed (complete / resolved) or this PC's
/// record marks `completed` / `resolved` has no writer left to wait for —
/// the liveness and other-account checks are skipped (R16 judged the live
/// writer first, so a journal a COMPLETED finalize failed to remove, its
/// workstation still running, refused the CLI's `--dismiss`). R17-S2:
/// `assume_gone` (`finalize-journal --writer-gone`) skips them as well —
/// the examiner confirmed the writer is gone.
fn classify(
    case_dir: &Path,
    journal: Result<FinalizeJournal, String>,
    inputs: impl FnOnce(&FinalizeJournal) -> Inputs,
    assume_gone: bool,
) -> Assessment {
    let journal = match journal {
        Ok(journal) => journal,
        Err(err) => return Assessment::Unreadable(err),
    };
    let this_host = crate::util::host_id();
    if this_host == crate::util::HOST_ID_UNKNOWN {
        // R14-S7: this PC cannot tell its journals from another PC's.
        return Assessment::Unpaired(
            journal,
            UnpairedWhy::new(
                UNPAIRED_HOST_UNKNOWN,
                "이 PC의 호스트 ID(MachineGuid / machine-id)를 읽지 못해 이 PC의 최종화인지 알 수 없습니다 — 복구하지 않습니다",
            ),
        );
    }
    if !journal.host_id.is_empty() && journal.host_id != this_host {
        let age = journal_age(&journal);
        return Assessment::OtherHost {
            blocking: age.is_some_and(|age| age < OTHER_HOST_BLOCK),
            until_unix: journal.started_unix + OTHER_HOST_BLOCK.as_secs(),
            journal,
        };
    }
    if journal.finalize_id.is_empty() {
        return Assessment::Unpaired(
            journal,
            UnpairedWhy::new(
                UNPAIRED_LEGACY,
                "이전 버전이 쓴 저널이라 서명 감사 레인에 짝이 되는 최종화 시작 기록이 없습니다",
            ),
        );
    }
    let Inputs {
        pairing,
        local,
        manifest_sha256,
        manifest_withheld,
        manifest_stamp,
    } = inputs(&journal);
    let closed = matches!(
        pairing,
        FinalizeLanePairing::Completed { .. } | FinalizeLanePairing::Resolved { .. }
    ) || matches!(
        &local,
        Ok(Some(record)) if record.state == LOCAL_COMPLETED || record.state == LOCAL_RESOLVED
    );
    let in_progress = journal.state == STATE_IN_PROGRESS && !closed;
    if in_progress
        && !assume_gone
        && let Some(image) = writer_other_account(
            &journal,
            begin_pid(&pairing),
            crate::util::inaccessible_process_name,
        )
        && other_account_start_matches(
            &journal,
            crate::util::process_start_time(journal.writer.pid),
        )
        && journal_age(&journal).is_some_and(|age| age < OTHER_HOST_BLOCK)
    {
        return Assessment::OtherAccount {
            until_unix: journal.started_unix + OTHER_HOST_BLOCK.as_secs(),
            journal,
            image,
        };
    }
    // R18-S3: `assume_gone` takes only an UNVERIFIED writer as gone — a
    // writer verified as this program (folder and SHA-256) is still a
    // live writer, whatever the examiner said (the journal may have been
    // replaced since the CLI's first look).
    let liveness = match (in_progress, assume_gone) {
        (false, _) => WriterLiveness::Gone,
        (true, false) => writer_liveness(&journal),
        (true, true) => match writer_liveness(&journal) {
            WriterLiveness::Verified => WriterLiveness::Verified,
            _ => WriterLiveness::Gone,
        },
    };
    let alive = liveness != WriterLiveness::Gone;
    if matches!(local, Ok(Some(_))) {
        match liveness {
            WriterLiveness::Verified => return Assessment::LiveWriter(journal),
            WriterLiveness::Unverified => return Assessment::UnverifiedWriter(journal),
            WriterLiveness::Gone => {}
        }
    }
    // R15-S4: a live writer without this PC's local record of the
    // finalize is only a note (a planted journal naming any FrameTrace
    // process must not block the case).
    let live_note = if alive {
        format!(
            " (참고: 저널에 적힌 작성자 pid {}가 실행 중인 FrameTrace 프로세스이지만 이 PC에 그 최종화의 로컬 기록이 없어 막지 않습니다)",
            journal.writer.pid
        )
    } else {
        String::new()
    };
    let begin = match pairing {
        // Only the journal goes — nothing else is changed on its account.
        FinalizeLanePairing::Completed { .. } => {
            return Assessment::Closed(journal, STATE_COMPLETED.to_string());
        }
        FinalizeLanePairing::Resolved { outcome } => {
            return Assessment::Closed(journal, outcome);
        }
        FinalizeLanePairing::Unpaired(reason) => Err(reason),
        FinalizeLanePairing::Open(begin) => Ok(begin),
    };
    // P18: this PC started that finalize and has not closed it — checked
    // before the lane's own verdict, so a journal this PC never opened
    // always reads "로컬 기록 없음".
    let local = match local {
        Ok(Some(local)) => local,
        Ok(None) => {
            let reason = format!(
                "{NO_LOCAL_RECORD} — 이 PC에는 이 최종화(ID {})를 시작했다는 로컬 기록({})이 없습니다. 이 PC에서 시작되어 끝나지 않은 최종화가 아니거나, 이미 끝난 최종화의 저널을 누군가 다시 만든 것입니다{live_note}",
                short(&journal.finalize_id),
                local_open_dir()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_else(|| LOCAL_OPEN_DIR_NAME.to_string())
            );
            return Assessment::Unpaired(
                journal,
                UnpairedWhy::new(UNPAIRED_NO_LOCAL_RECORD, reason),
            );
        }
        Err(err) => {
            return Assessment::Unpaired(
                journal,
                UnpairedWhy::new(
                    UNPAIRED_LOCAL_UNREADABLE,
                    format!("이 PC의 열린 최종화 로컬 기록을 읽지 못했습니다: {err}"),
                ),
            );
        }
    };
    if let Some(field) = journal.mismatch_with_local(&local, case_dir) {
        return Assessment::Unpaired(
            journal,
            UnpairedWhy::new(
                UNPAIRED_LOCAL_MISMATCH,
                format!("저널의 {field}이(가) 이 PC의 열린 최종화 로컬 기록과 다릅니다"),
            ),
        );
    }
    // R15 (P21): only an `open` record is ever recovered. A record past it
    // is this PC's own word that the finalize got that far — the lane's
    // tail (cut) and the case folder's contents (forgeable) never
    // override it.
    let package_changed = |journal: FinalizeJournal| {
        let reason = format!(
            "이 PC의 로컬 기록에는 이 최종화의 패키지 단계가 끝났다고 남아 있지만, 예약 패키지 reports/{}의 manifest.sha256이 그때 기록한 것과 다릅니다(바뀌었거나 없음) — 되돌리지도 완료로 기록하지도 않습니다",
            journal.package_name
        );
        Assessment::Unpaired(journal, UnpairedWhy::new(UNPAIRED_PACKAGE_CHANGED, reason))
    };
    // R17-S9: the status path did not hash a manifest over 16 MiB — what
    // the record says is shown "확인 보류" (a recovery hashes it fully).
    // R18-S2 (P31): unknown (`blocks_finalize: null`) unless a recovery's
    // verdict for that very manifest (size + mtime) is remembered — then
    // that verdict, package changed included.
    let withheld = |journal: FinalizeJournal, manifest_sha256: Option<String>| {
        match remembered_verdict(case_dir, &journal.finalize_id, manifest_stamp) {
            Some(RecoveryVerdict::PackageChanged(why)) => {
                let why = UnpairedWhy {
                    reason: format!("{}{REMEMBERED_NOTE}", why.reason),
                    remembered: true,
                    ..why
                };
                return Assessment::Unpaired(journal, why);
            }
            Some(RecoveryVerdict::Finished { why }) => {
                return Assessment::Finished {
                    journal,
                    why: format!("{why}{REMEMBERED_NOTE}"),
                    manifest_sha256,
                    check: ManifestCheck::Remembered,
                };
            }
            None => {}
        }
        let why = format!(
            "확인 보류 — 이 PC의 로컬 기록에는 이 최종화의 패키지 단계가 끝났다고 남아 있지만, 예약 패키지 reports/{}의 manifest.sha256이 {} MiB보다 커서 상태 확인에서는 해시하지 않았습니다 (케이스를 다시 열거나 최종화하면 전체를 해시해 확인합니다)",
            journal.package_name,
            MANIFEST_MAX_BYTES / (1024 * 1024)
        );
        Assessment::Finished {
            journal,
            why,
            manifest_sha256,
            check: ManifestCheck::Withheld,
        }
    };
    match local.state.as_str() {
        LOCAL_OPEN => {}
        STATE_COMPLETED_UNRECORDED | LOCAL_COMPLETED | LOCAL_PACKAGED if manifest_withheld => {
            return withheld(journal, local.manifest_sha256);
        }
        STATE_COMPLETED_UNRECORDED | LOCAL_COMPLETED => {
            // R16-S3: the complete entry written again carries the
            // manifest this PC recorded — and only while it is still the
            // package's (R15 wrote whatever manifest was there now).
            if local.manifest_sha256.is_some() && local.manifest_sha256 != manifest_sha256 {
                return package_changed(journal);
            }
            return Assessment::Finished {
                journal,
                why: "이 PC의 로컬 기록에 이 최종화가 끝났다고 남아 있습니다 (서명 감사 레인의 완료 기록만 없음)".to_string(),
                manifest_sha256: local.manifest_sha256,
                check: ManifestCheck::Hashed,
            };
        }
        LOCAL_RESOLVED => {
            return Assessment::Closed(journal, LOCAL_RESOLVED.to_string());
        }
        LOCAL_PACKAGED => {
            if local.manifest_sha256.is_some() && local.manifest_sha256 == manifest_sha256 {
                let why = format!(
                    "이 PC의 로컬 기록에 이 최종화의 패키지 단계가 끝났다고 남아 있고, 예약 패키지 reports/{}의 manifest.sha256이 그때 기록한 SHA-256 {}와 같습니다",
                    journal.package_name,
                    short(local.manifest_sha256.as_deref().unwrap_or_default())
                );
                return Assessment::Finished {
                    journal,
                    why,
                    manifest_sha256: local.manifest_sha256,
                    check: ManifestCheck::Hashed,
                };
            }
            return package_changed(journal);
        }
        other => {
            let reason = format!("이 PC의 열린 최종화 로컬 기록의 상태 '{other}'를 알 수 없습니다");
            return Assessment::Unpaired(journal, UnpairedWhy::new(UNPAIRED_LOCAL_STATE, reason));
        }
    }
    let begin = match begin {
        Ok(begin) => begin,
        Err(reason) => {
            return Assessment::Unpaired(journal, UnpairedWhy::new(UNPAIRED_LANE, reason));
        }
    };
    if let Some(field) = journal.mismatch_with(&begin) {
        return Assessment::Unpaired(
            journal,
            UnpairedWhy::new(
                UNPAIRED_JOURNAL_MISMATCH,
                format!("저널의 {field}이(가) 서명 감사 레인의 최종화 시작 기록과 다릅니다"),
            ),
        );
    }
    // R15-S2 (P21): the case folder's evidence (a published package with
    // a manifest naming the sign-off) is NOT proof any more — it can be
    // forged; only the `packaged` record above is.
    Assessment::Open(journal, begin)
}

/// [`classify`] of the journal on disk now (`None`: no journal).
pub(crate) fn assess(case_dir: &Path) -> Option<Assessment> {
    assess_with(case_dir, false)
}

/// [`assess`]; `assume_gone`: as if the journal's writer were gone
/// (R17-S2, `--writer-gone`).
fn assess_with(case_dir: &Path, assume_gone: bool) -> Option<Assessment> {
    let journal = match read_journal(case_dir) {
        Ok(None) => return None,
        Ok(Some(journal)) => Ok(journal),
        Err(err) => Err(err),
    };
    // R18-S2 (P31): the verdict of this full-hash assessment is
    // remembered for the status path (see [`RecoveryVerdict`]).
    let mut judged: Option<(String, Option<(u64, SystemTime)>)> = None;
    let assessment = classify(
        case_dir,
        journal,
        |journal| {
            let inputs = gather(case_dir, journal, true);
            judged = Some((journal.finalize_id.clone(), inputs.manifest_stamp));
            inputs
        },
        assume_gone,
    );
    if let Some((finalize_id, stamp)) = judged {
        remember_verdict(case_dir, &finalize_id, stamp, &assessment);
    }
    Some(assessment)
}

/// R18-S1 (P29): `/api/status.finalize_journal.command_unavailable_reason`
/// and the CLI's words when a case path is not paste-safe — no command is
/// given at all.
pub(crate) const COMMAND_UNAVAILABLE_REASON: &str = crate::paste_path::UNSAFE_REASON;

/// The case path as it goes, unescaped, between the command's quotes —
/// `None` (no command) when any character is not paste-safe or it starts
/// with `-` (it would read as an option). Nothing is ever escaped: an
/// escape is a promise about one shell's parser, and R17 showed that
/// promise broken. R19-S1 (P32): the path is first normalized
/// ([`crate::paste_path::normalize`]: absolute, no trailing separator, a
/// drive root `X:\.`) — a trailing `\` made the closing quote literal in
/// Win32 argv conversion and the flags landed inside the path argument.
fn paste_safe_path(case_dir: &Path) -> Option<String> {
    crate::paste_path::paste_safe(case_dir)
}

/// R14-S11: the dismiss command as the examiner pastes it — single-quoted
/// (PowerShell, the Windows default shell, expands `$(…)` inside double
/// quotes; a case path holding one ran it). R18-S1 (P29): `None` unless
/// the path is paste-safe ([`crate::paste_path::paste_safe_char`]); the path is then put in
/// as it is (it holds no quote of any kind).
pub(crate) fn dismiss_command(case_dir: &Path) -> Option<String> {
    journal_command(case_dir, "--dismiss")
}

/// `frametrace finalize-journal '<case>' <flags>` (see [`dismiss_command`]).
fn journal_command(case_dir: &Path, flags: &str) -> Option<String> {
    let path = paste_safe_path(case_dir)?;
    Some(format!("frametrace finalize-journal '{path}' {flags}"))
}

/// R14-S11: the same for `cmd.exe` (double quotes). `None` off Windows,
/// and (R18-S1, P29) whenever the PowerShell form is `None` — the
/// whitelist also refuses `%` (R17-S10: `cmd.exe` expands `%VAR%` inside
/// double quotes with no escape at its prompt) and `!` (S8: delayed
/// expansion).
pub(crate) fn dismiss_command_cmd(case_dir: &Path) -> Option<String> {
    journal_command_cmd(case_dir, "--dismiss")
}

fn journal_command_cmd(case_dir: &Path, flags: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let path = paste_safe_path(case_dir)?;
    Some(format!("frametrace finalize-journal \"{path}\" {flags}"))
}

/// R17-S2: `--writer-gone` — the examiner confirmed the journal's writer
/// is gone: the normal recovery runs as if it were. R18-S7: `sure` adds
/// `--i-am-sure` (a live process still matches the writer's pid and start
/// time, see [`writer_gone_needs_sure`]).
pub(crate) fn writer_gone_command(case_dir: &Path, sure: bool) -> Option<String> {
    journal_command(case_dir, writer_gone_flags(sure))
}

pub(crate) fn writer_gone_command_cmd(case_dir: &Path, sure: bool) -> Option<String> {
    journal_command_cmd(case_dir, writer_gone_flags(sure))
}

fn writer_gone_flags(sure: bool) -> &'static str {
    if sure {
        "--writer-gone --i-am-sure"
    } else {
        "--writer-gone"
    }
}

/// Both forms of a command for examiner-facing text; R18-S1 (P29): when
/// the path is not paste-safe, the command's shape with a placeholder and
/// [`COMMAND_UNAVAILABLE_REASON`] (never the path between quotes).
fn command_text(powershell: Option<String>, cmd: Option<String>, flags: &str) -> String {
    match (powershell, cmd) {
        (Some(powershell), Some(cmd)) => {
            format!("PowerShell: {powershell} / 명령 프롬프트: {cmd}")
        }
        (Some(powershell), None) => powershell,
        _ => format!(
            "frametrace finalize-journal <케이스 폴더> {flags} ({COMMAND_UNAVAILABLE_REASON})"
        ),
    }
}

/// Both forms for examiner-facing text.
fn dismiss_hint(case_dir: &Path) -> String {
    command_text(
        dismiss_command(case_dir),
        dismiss_command_cmd(case_dir),
        "--dismiss",
    )
}

/// R12-S2 / R13: recovers the interrupted finalize `case_dir`'s journal
/// points to — only when it pairs with its lane entry and this PC's local
/// record (see the module docs). `None` when there is no journal. The
/// caller makes sure no finalize of this workstation is running on the
/// case (it holds `busy`). Restoring and the other cleanup steps are never
/// cancellable (R12-S4); `budget` bounds each rename's retries.
pub(crate) fn recover(case_dir: &Path, state: &SharedState, budget: Duration) -> Option<Recovery> {
    let assessment = assess(case_dir)?;
    Some(recover_assessed(case_dir, assessment, state, budget))
}

/// [`recover`] of an assessment already made (R17-S2: `--writer-gone`
/// passes one made as if the writer were gone).
fn recover_assessed(
    case_dir: &Path,
    assessment: Assessment,
    state: &SharedState,
    budget: Duration,
) -> Recovery {
    #[cfg(test)]
    if PANIC_IN_RECOVERY
        .lock()
        .ok()
        .is_some_and(|case| case.as_deref() == Some(case_dir))
    {
        panic!("test: injected recovery panic");
    }
    let hint = dismiss_hint(case_dir);
    let recovery = match assessment {
        Assessment::Unreadable(err) => Recovery {
            detail: format!(
                "이전 최종화가 중단된 흔적(최종화 저널)이 있지만 읽지 못해 아무것도 바꾸지 않았습니다: {err} — 검토자 기록(review/signoff.json)과 reports 폴더의 마지막 패키지를 확인한 뒤 `{hint}`로 저널을 옮기고(지우지 않음) 최종화를 다시 실행하십시오."
            ),
            blocking: true,
            ..Recovery::default()
        },
        Assessment::OtherHost {
            journal,
            blocking,
            until_unix,
        } => Recovery {
            detail: if blocking {
                format!(
                    "다른 PC(호스트 {})에서 시작된 최종화(검토자 입력값: {})의 저널이 있어 아무것도 바꾸지 않았습니다 — 그 PC에서 진행 중이거나 중단된 작업일 수 있습니다. 그 PC에서 끝내거나 확인한 뒤 다시 시도하십시오 (unix {until_unix}까지 이 PC의 최종화를 막습니다; 확인했다면 `{hint}`).",
                    short(&journal.host_id),
                    journal.examiner
                )
            } else {
                format!(
                    "다른 PC(호스트 {})에서 24시간 넘게 전에 시작된 최종화의 저널이 있습니다 — 이 PC는 그것을 복구하지 않습니다(다음 최종화가 저널을 옆으로 옮김).",
                    short(&journal.host_id)
                )
            },
            blocking,
            set_aside: !blocking,
            ..Recovery::default()
        },
        Assessment::LiveWriter(journal) => Recovery {
            detail: live_writer_text(&journal),
            blocking: true,
            ..Recovery::default()
        },
        Assessment::UnverifiedWriter(journal) => Recovery {
            detail: unverified_writer_text(&journal, case_dir),
            blocking: true,
            ..Recovery::default()
        },
        Assessment::OtherAccount {
            journal,
            image,
            until_unix,
        } => Recovery {
            detail: other_account_text(&journal, &image, until_unix, case_dir),
            blocking: true,
            ..Recovery::default()
        },
        Assessment::Unpaired(journal, why) => Recovery {
            detail: format!(
                "최종화 저널(검토자 입력값: {}, 서명 SHA-256 {})이 있지만 확인할 수 없어 아무것도 바꾸지 않았습니다 — {}. 누군가 만든 파일이거나 이전 버전·다른 PC·다른 키의 기록일 수 있습니다. 검토자 기록(review/signoff.json)과 reports 폴더를 확인한 뒤 `{hint}`로 저널을 옮기고(지우지 않음) 최종화를 다시 실행하십시오.",
                journal.examiner,
                short(&journal.signoff_sha256),
                why.reason
            ),
            blocking: true,
            ..Recovery::default()
        },
        Assessment::Closed(journal, outcome) => close_journal(case_dir, &journal, &outcome, budget),
        // `checked: false` is the status path's only (recoveries assess
        // with the manifest hashed in full).
        Assessment::Finished {
            journal,
            why,
            manifest_sha256,
            ..
        } => record_finished(case_dir, &journal, &why, manifest_sha256, budget),
        Assessment::Open(journal, begin) => resolve(
            case_dir,
            &journal,
            &begin,
            state,
            INTERRUPTED_REASON,
            budget,
        ),
    };
    state_lock(state)
        .logs
        .push(format!("경고: {}", recovery.detail));
    recovery
}

/// R14 (P19 / S2): a finished finalize whose `finalize-complete` entry is
/// missing — the entry is written, the journal and the local record go;
/// nothing else is touched. When the entry still cannot be written both
/// are kept as [`STATE_COMPLETED_UNRECORDED`] for the next attempt.
/// A journal the lane (or this PC's record) closes: only the journal goes —
/// R16 (P24): once this PC's local record of it is past `open` (`completed`
/// for a complete entry). When that state file cannot be created the
/// journal is KEPT and the finalize blocked: removing it while the record
/// still reads `open` lets a cut lane tail plus a rebuilt journal roll the
/// finalize back (R15-S1).
fn close_journal(
    case_dir: &Path,
    journal: &FinalizeJournal,
    outcome: &str,
    budget: Duration,
) -> Recovery {
    // R17 (P28): a record that cannot advance loses its `open` file instead.
    let settled = match close_local(journal, case_dir, outcome, None) {
        Ok(settled) => settled,
        Err(err) => {
            let detail = format!(
                "이전 최종화는 서명 감사 레인에 '{outcome}'(으)로 기록되어 있지만 이 PC의 로컬 기록에 그 상태를 남기지 못해({err}) 최종화 저널을 그대로 두었습니다 — 로컬 설정 폴더({})에 쓸 수 있는지 확인한 뒤 케이스를 다시 여십시오 (다른 것은 바꾸지 않음).",
                local_open_dir()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_else(|| LOCAL_OPEN_DIR_NAME.to_string())
            );
            remember_complete_failure(&journal.finalize_id, &detail);
            return Recovery {
                detail,
                blocking: true,
                ..Recovery::default()
            };
        }
    };
    forget_complete_failure(&journal.finalize_id);
    let dropped = if settled == Settled::Dropped {
        format!(" {}", dropped_note(&journal.finalize_id))
    } else {
        String::new()
    };
    Recovery {
        detail: match remove_journal_within(case_dir, &journal.finalize_id, budget) {
            Ok(()) => {
                let _ = remove_local_record(&journal.finalize_id);
                format!(
                    "이전 최종화는 서명 감사 레인에 '{outcome}'(으)로 기록되어 있어, 남아 있던 최종화 저널만 지웠습니다 (다른 것은 바꾸지 않음).{dropped}"
                )
            }
            Err(err) => format!(
                "이전 최종화는 서명 감사 레인에 '{outcome}'(으)로 기록되어 있지만 남은 최종화 저널을 지우지 못했습니다: {err}"
            ),
        },
        ..Recovery::default()
    }
}

/// R16-S3: `manifest` is the SHA-256 this PC's local record holds (the
/// classification checked it is still the package's) — R15 recorded the
/// manifest on disk at recovery time.
fn record_finished(
    case_dir: &Path,
    journal: &FinalizeJournal,
    why: &str,
    manifest: Option<String>,
    budget: Duration,
) -> Recovery {
    let head = format!(
        "이전 최종화(검토자 입력값: {}, 서명 SHA-256 {})는 끝나 있었습니다 — {why}.",
        journal.examiner,
        short(&journal.signoff_sha256)
    );
    #[cfg(test)]
    let injected = FAIL_RECORD_FINISHED.with(|cell| cell.get());
    #[cfg(not(test))]
    let injected = false;
    let complete = if injected {
        Err("test: injected finalize-complete failure".to_string())
    } else {
        crate::report::record_finalize_complete(
            case_dir,
            &journal.finalize_id,
            &journal.signoff_sha256,
            &journal.package_name,
            manifest.as_deref(),
        )
    };
    match complete {
        Ok(()) => {
            // P21 / R16 (P24): `completed` before the journal goes, then
            // removed; when `completed` cannot be created the journal is
            // kept as `completed-unrecorded` (the lane holds the complete
            // entry now: the next recovery only has to record it here).
            // (A Finished record is past `open`: P28's drop never applies.)
            if let Err(err) = advance_or_drop(journal, case_dir, LOCAL_COMPLETED, manifest) {
                let _ = journal.rewrite_as(STATE_COMPLETED_UNRECORDED, case_dir);
                let detail = format!(
                    "{head} 서명 감사 레인에 빠져 있던 완료 기록을 남겼지만 이 PC의 로컬 기록에 완료를 남기지 못해({err}) 최종화 저널을 'completed-unrecorded'로 두었습니다 — 로컬 설정 폴더에 쓸 수 있는지 확인한 뒤 케이스를 다시 여십시오 (다른 것은 바꾸지 않음)."
                );
                remember_complete_failure(&journal.finalize_id, &detail);
                return Recovery {
                    detail,
                    blocking: true,
                    ..Recovery::default()
                };
            }
            forget_complete_failure(&journal.finalize_id);
            let left = remove_journal_within(case_dir, &journal.finalize_id, budget)
                .err()
                .map(|err| format!(" 최종화 저널은 지우지 못했습니다: {err}"))
                .unwrap_or_default();
            let _ = remove_local_record(&journal.finalize_id);
            Recovery {
                detail: format!(
                    "{head} 서명 감사 레인에 빠져 있던 완료 기록만 남겼고 다른 것은 바꾸지 않았습니다.{left}"
                ),
                ..Recovery::default()
            }
        }
        Err(err) => {
            let _ = journal.rewrite_as(STATE_COMPLETED_UNRECORDED, case_dir);
            let _ = journal.advance_local(case_dir, STATE_COMPLETED_UNRECORDED, None);
            let detail = format!(
                "{head} 서명 감사 레인에 완료 기록을 남기지 못했습니다({err}) — 그 기록을 남길 때까지 새 최종화를 막습니다 (다른 것은 바꾸지 않음; 감사 로그 폴더에 쓸 수 있는지 확인한 뒤 케이스를 다시 열거나 최종화를 다시 실행하십시오)."
            );
            // R15-S6: what /api/status shows for this journal until the
            // entry is written (it blocks the finalize — R14 said
            // `blocks_finalize: false` and then refused).
            remember_complete_failure(&journal.finalize_id, &detail);
            Recovery {
                detail,
                blocking: true,
                ..Recovery::default()
            }
        }
    }
}

/// R15-S6: finalize ids whose missing complete entry a recovery could not
/// write, with the examiner-facing text (in memory: the next recovery —
/// this process's or a restarted one's — tries again).
fn complete_failures() -> &'static Mutex<Vec<(String, String)>> {
    static FAILURES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    &FAILURES
}

pub(crate) fn remember_complete_failure(finalize_id: &str, detail: &str) {
    if let Ok(mut failures) = complete_failures().lock() {
        failures.retain(|(id, _)| id != finalize_id);
        keep_newest(&mut failures, 16);
        failures.push((finalize_id.to_string(), detail.to_string()));
    }
}

fn forget_complete_failure(finalize_id: &str) {
    if let Ok(mut failures) = complete_failures().lock() {
        failures.retain(|(id, _)| id != finalize_id);
    }
}

pub(crate) fn complete_failure(finalize_id: &str) -> Option<String> {
    complete_failures().lock().ok().and_then(|failures| {
        failures
            .iter()
            .find(|(id, _)| id == finalize_id)
            .map(|(_, detail)| detail.clone())
    })
}

#[cfg(test)]
thread_local! {
    /// R15-S6 test hook: a recovery's complete entry for a finished
    /// finalize fails (on this thread).
    pub(crate) static FAIL_RECORD_FINISHED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// R15-S10: the examiner-facing text of another account's finalize.
/// R17-S2/S3: waiting comes first — the block lifts by itself 24 h after
/// the journal's start, and only then the ways out.
pub(crate) fn other_account_text(
    journal: &FinalizeJournal,
    image: &str,
    until_unix: u64,
    case_dir: &Path,
) -> String {
    format!(
        "이 케이스의 최종화 저널에 적힌 작성자(pid {}, {image})는 이 PC의 다른 계정의 FrameTrace 프로세스일 수 있습니다 — 이 계정으로는 그 프로세스를 확인할 수 없어 아무것도 바꾸지 않았고 최종화를 막습니다. 기다리면 저절로 풀립니다: unix {until_unix}(저널 시작 24시간 뒤)부터는 이 계정에서도 막지 않습니다 — 그 계정에서 케이스를 열면 그 계정의 로컬 기록으로 정상 복구되고, 이 계정에는 그 최종화의 로컬 기록이 없으면 저널을 옮기기만 할 수 있습니다(되돌리거나 완료로 기록하지 않음). 그 전에는 그 계정에서 최종화가 끝났는지 확인하십시오. {}",
        journal.writer.pid,
        writer_hint(case_dir, journal)
    )
}

/// R16-S2 (P25): the examiner-facing text of a maybe-live writer. R17-S2:
/// waiting first — the block lifts once that process ends.
fn unverified_writer_text(journal: &FinalizeJournal, case_dir: &Path) -> String {
    format!(
        "이 케이스의 최종화 저널에 적힌 작성자(pid {})가 아직 실행 중인 FrameTrace 프로세스이지만, 실행 파일이 이 프로그램과 다른 폴더에 있거나 내용(SHA-256)이 달라 같은 최종화인지 확인할 수 없습니다 — 진행 중인 최종화일 수 있어 아무것도 되돌리지 않았고 최종화를 막습니다. 기다리면 저절로 풀립니다: 그 프로세스(다른 FrameTrace 설치)가 끝나면 다음에 케이스를 열 때 정상 복구됩니다. {}",
        journal.writer.pid,
        writer_hint(case_dir, journal)
    )
}

/// R17-S8: a VERIFIED live writer (this program's folder and SHA-256):
/// only waiting is offered — no command.
fn live_writer_text(journal: &FinalizeJournal) -> String {
    format!(
        "다른 FrameTrace 프로세스(pid {}, 이 프로그램과 같은 실행 파일)가 이 케이스를 최종화하고 있어 최종화 저널을 그대로 두었습니다 — 기다리십시오: 그 최종화가 끝나거나 그 프로세스가 끝나면 다음에 케이스를 열 때 저절로 정리됩니다.",
        journal.writer.pid
    )
}

/// R16-S2/S4: `--dismiss --force` (moves the journal of a writer that may
/// still run — nothing is rolled back or recorded). R18-S1: `None` unless
/// the path is paste-safe.
pub(crate) fn force_dismiss_command(case_dir: &Path) -> Option<String> {
    journal_command(case_dir, "--dismiss --force")
}

pub(crate) fn force_dismiss_command_cmd(case_dir: &Path) -> Option<String> {
    journal_command_cmd(case_dir, "--dismiss --force")
}

/// R18-S7: a live process holds the journal's writer pid AND started when
/// the journal says its writer did (both times known, within the usual
/// slack) — `--writer-gone` then needs `--i-am-sure`: the examiner says
/// that very process is not this case's finalize. (An unverified writer
/// always matches: that is how it was found; another account's process
/// matches when its start time can be read.)
pub(crate) fn writer_gone_needs_sure(journal: &FinalizeJournal) -> bool {
    let writer = &journal.writer;
    if writer.pid == 0 || writer.pid == std::process::id() {
        return false;
    }
    let Some(recorded) = writer.started_ms else {
        return false;
    };
    crate::util::process_start_time(writer.pid).is_some_and(|started| {
        let started_ms = started
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        started_ms.abs_diff(recorded) <= crate::util::START_TIME_SLACK.as_millis() as u64
    })
}

/// R17-S2: what `--dismiss --force` leaves in place for `journal` — the
/// sign-off file and the reserved package, named (read capped: this is
/// also the status path).
fn left_in_place(case_dir: &Path, journal: &FinalizeJournal) -> String {
    let signoff = match read_capped(
        &case_dir.join(crate::report::SIGNOFF_REL_PATH),
        LEFT_IN_PLACE_READ_MAX_BYTES,
    ) {
        Ok(bytes) if crate::sha256::digest_bytes(&bytes) == journal.signoff_sha256 => format!(
            "이 최종화의 검토자 기록 {}(SHA-256 {})",
            crate::report::SIGNOFF_REL_PATH,
            short(&journal.signoff_sha256)
        ),
        Ok(_) => format!(
            "{}(이 최종화의 것이 아닌 지금 내용)",
            crate::report::SIGNOFF_REL_PATH
        ),
        Err(_) => format!("{}(읽지 못함)", crate::report::SIGNOFF_REL_PATH),
    };
    let package = if !crate::package::valid_reserved_package_name(&journal.package_name) {
        "예약 패키지(이름을 알 수 없음)".to_string()
    } else if std::fs::symlink_metadata(case_dir.join("reports").join(&journal.package_name))
        .is_ok()
    {
        format!("패키지 reports/{}", journal.package_name)
    } else {
        format!("예약 패키지 reports/{}(지금 없음)", journal.package_name)
    };
    format!("{signoff}과 {package}이(가) 확정 여부와 관계없이 그대로 남습니다")
}

/// The sign-off is read at most this much for [`left_in_place`] (a
/// sign-off record is a few KiB).
const LEFT_IN_PLACE_READ_MAX_BYTES: u64 = 1024 * 1024;

/// R17-S2: the way out of a block on a writer that may still run, after
/// the "wait" sentence the caller puts first: `--writer-gone` (the normal
/// recovery once the examiner confirmed the writer is gone), then
/// `--dismiss --force` with what it leaves in place.
/// R18-S7: with `--i-am-sure` when [`writer_gone_needs_sure`]. R18-S1:
/// no path between quotes unless it is paste-safe.
fn writer_hint(case_dir: &Path, journal: &FinalizeJournal) -> String {
    let sure = writer_gone_needs_sure(journal);
    let sure_note = if sure {
        format!(
            " (실행 중인 프로세스 pid {}가 저널에 적힌 작성자의 시작 시각과 맞아 --i-am-sure가 필요합니다 — 그 프로세스가 이 케이스의 FrameTrace 최종화가 아님을 직접 확인했을 때만 쓰십시오)",
            journal.writer.pid
        )
    } else {
        String::new()
    };
    format!(
        "그 프로세스가 이 최종화의 작성자가 아니라고(작성자는 이미 끝났다고) 확인했다면 `{}`로 작성자 없음으로 보고 정상 복구를 실행할 수 있습니다{sure_note}. `{}`는 저널만 옮기고 아무것도 되돌리거나 기록하지 않습니다 — {}.",
        command_text(
            writer_gone_command(case_dir, sure),
            writer_gone_command_cmd(case_dir, sure),
            writer_gone_flags(sure)
        ),
        command_text(
            force_dismiss_command(case_dir),
            force_dismiss_command_cmd(case_dir),
            "--dismiss --force"
        ),
        left_in_place(case_dir, journal)
    )
}

fn short(text: &str) -> &str {
    &text[..12.min(text.len())]
}

/// R12-S2: [`recover`] for the case the workstation starts bound to,
/// synchronously (tests; the server uses [`spawn_bound_case_recovery`]).
#[cfg(test)]
pub(crate) fn recover_bound_case(state: &SharedState) {
    let bound = state_lock(state).case_dir.clone();
    if let Some(case_dir) = bound
        && journal_path(&case_dir).exists()
        && let Some(recovery) = recover(&case_dir, state, STARTUP_RETRY_BUDGET)
    {
        super::jobs::apply_recovery(state, recovery);
    }
}

/// Test gate: a test holding it keeps the background recovery waiting.
#[cfg(test)]
pub(crate) static STARTUP_RECOVERY_GATE: Mutex<()> = Mutex::new(());

/// R14-S10 test hook: a recovery of this case panics.
#[cfg(test)]
pub(crate) static PANIC_IN_RECOVERY: Mutex<Option<PathBuf>> = Mutex::new(None);

/// R14-S10: clears `finalize_recovering` for `case` however the recovery
/// thread ends (a panic included — R13 left status `recovering` for good).
struct RecoveringFlag {
    state: SharedState,
    case: PathBuf,
}

impl Drop for RecoveringFlag {
    fn drop(&mut self) {
        let mut guard = state_lock(&self.state);
        if guard.finalize_recovering.as_deref() == Some(self.case.as_path()) {
            guard.finalize_recovering = None;
        }
    }
}

/// R13-S7 (P17): the start-up recovery of the bound case, in the
/// background — the server serves (status `finalize_journal.state:
/// "recovering"`) while it runs, holding `busy` so no job, open-case or
/// shutdown runs under it. R12 ran it before serving: a sign-off another
/// program held kept the app from starting for the whole retry budget.
/// R14-S13: the local record folder is swept first (in its own thread).
/// R14-S10: a panicking recovery still frees `busy` and the status.
pub(crate) fn spawn_bound_case_recovery(state: &SharedState) -> Option<thread::JoinHandle<()>> {
    let _ = thread::Builder::new()
        .name("finalize-open-sweep".into())
        .spawn(|| {
            sweep_local_records();
        });
    let bound = state_lock(state).case_dir.clone()?;
    if !journal_path(&bound).exists() {
        return None;
    }
    let lease = {
        let mut guard = state_lock(state);
        let lease = super::lease_busy_locked(&mut guard, state).ok()?;
        guard.finalize_recovering = Some(bound.clone());
        guard
            .logs
            .push("이전에 중단된 최종화 흔적을 확인하는 중…".to_string());
        lease
    };
    let state = Arc::clone(state);
    Some(thread::spawn(move || {
        let _flag = RecoveringFlag {
            state: Arc::clone(&state),
            case: bound.clone(),
        };
        #[cfg(test)]
        let _gate = STARTUP_RECOVERY_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let recovery = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recover(&bound, &state, STARTUP_RETRY_BUDGET)
        }))
        .unwrap_or_else(|_| {
            let detail = "이전에 중단된 최종화 흔적을 확인하다가 내부 오류로 멈췄습니다 — 케이스를 다시 열면 다시 확인합니다 (최종화 저널은 그대로 둠).".to_string();
            state_lock(&state).logs.push(format!("경고: {detail}"));
            Some(Recovery {
                detail,
                blocking: true,
                ..Recovery::default()
            })
        });
        lease.release_with(|guard| {
            guard.finalize_recovering = None;
            if let Some(recovery) = recovery
                && guard.case_dir.as_deref() == Some(bound.as_path())
            {
                super::jobs::apply_recovery_locked(guard, recovery);
            }
        });
    }))
}

/// The idempotent cleanup of an interrupted finalize — each step is judged
/// from the disk and bounded by what the lane's begin entry records.
fn resolve(
    case_dir: &Path,
    journal: &FinalizeJournal,
    begin: &FinalizeBegin,
    state: &SharedState,
    reason: &str,
    budget: Duration,
) -> Recovery {
    let sha = begin.signoff_record_sha256.clone();
    let mut done: Vec<String> = Vec::new();
    let mut open: Vec<String> = Vec::new();
    // 1. P14 / R13-S2: the package it reserved is never left plain.
    // R15-S8: only a package that carries this finalize's sign-off.
    let marked = mark_unfinished_packages(
        case_dir,
        &begin.package_name,
        "",
        reason,
        budget,
        Some(&sha),
    );
    done.extend(marked.notes);
    if marked.failed {
        open.push(
            "최종화 미완료 표시를 하지 못한 패키지가 있습니다 — 최종 결과로 쓰지 마십시오."
                .to_string(),
        );
    }
    if let Some(dir) = own_reserved(case_dir, &begin.package_name, &sha) {
        open.push(format!(
            "패키지 {}에 최종화 미완료 표시를 하지 못했습니다 — 최종 결과로 쓰지 마십시오.",
            dir.display()
        ));
    }
    // 2. The chained sign-off entry is abandoned — once.
    match crate::report::signoff_lane_state_sha(case_dir, &sha) {
        Ok(lane) if lane.recorded && !lane.abandoned => {
            match crate::report::record_signoff_abandoned_sha(case_dir, &sha, reason) {
                Ok(()) => done.push(
                    "감사 로그에 그 검토자 기록을 '완료되지 않음'으로 남겼습니다.".to_string(),
                ),
                Err(err) => open.push(format!(
                    "감사 로그에 '완료되지 않음' 기록을 남기지 못했습니다: {err}"
                )),
            }
        }
        Ok(_) => {}
        Err(err) => open.push(format!("서명 감사 레인을 읽지 못했습니다: {err}")),
    }
    // 3. The pending record goes; the promoted one is taken back — with
    // bytes that hash to what the begin entry recorded, or not at all.
    let pending = case_dir.join(super::jobs::SIGNOFF_PENDING_REL_PATH);
    if std::fs::read(&pending).is_ok_and(|bytes| crate::sha256::digest_bytes(&bytes) == sha) {
        let _ = crate::util::remove_file_durable(&pending);
    }
    let target = case_dir.join(crate::report::SIGNOFF_REL_PATH);
    let promoted =
        std::fs::read(&target).is_ok_and(|bytes| crate::sha256::digest_bytes(&bytes) == sha);
    if promoted {
        let previous = match &begin.previous_signoff_sha256 {
            None => Ok(None),
            Some(expected) => {
                match read_capped(&previous_signoff_path(case_dir), PREVIOUS_SIGNOFF_MAX_BYTES) {
                    Ok(bytes) if crate::sha256::digest_bytes(&bytes) == *expected => {
                        match crate::report::signoff_lane_state_sha(case_dir, expected) {
                            // R13 I2 (P16): the lane was repaired after the
                            // previous record's entry — whether it was
                            // abandoned is unknown: never restored as
                            // current ("확인 필요"; the copy stays beside
                            // the kept journal).
                            Ok(lane) if lane.repaired_after => {
                                open.push(format!(
                                    "이전 검토자 기록은 서명 감사 레인이 그 항목 뒤에서 수리되어(lane-repair) 확정 여부를 알 수 없어 되돌리지 않았습니다 — 보관본 {PREVIOUS_SIGNOFF_REL_PATH}을 확인하십시오."
                                ));
                                Ok(None)
                            }
                            lane => Ok(Some((bytes, lane.is_ok_and(|lane| lane.abandoned)))),
                        }
                    }
                    Ok(_) => Err(format!(
                        "보관한 이전 검토자 기록({PREVIOUS_SIGNOFF_REL_PATH})이 감사 레인에 기록된 SHA-256과 달라 되돌리지 않았습니다"
                    )),
                    Err(err) => Err(format!(
                        "보관한 이전 검토자 기록({PREVIOUS_SIGNOFF_REL_PATH})을 읽지 못했습니다: {err}"
                    )),
                }
            }
        };
        let restored = previous.and_then(|previous| {
            let (bytes, abandoned) = match &previous {
                Some((bytes, abandoned)) => (Some(bytes.as_slice()), *abandoned),
                None => (None, false),
            };
            restore_previous(&target, bytes, abandoned, state, budget)
        });
        match restored {
            Ok(Some(note)) => done.push(note),
            Ok(None) => done.push(
                "검토자 기록(review/signoff.json)을 최종화 전 상태로 되돌렸습니다.".to_string(),
            ),
            Err(err) => open.push(format!(
                "{}에 끝나지 않은 최종화의 검토자 기록이 남아 있습니다 (되돌리지 못함: {err})",
                target.display()
            )),
        }
    }
    // 4. A report rendered from that sign-off is stale until re-rendered.
    let report_stale = report_names(case_dir, &sha);
    let mut blocking = !open.is_empty();
    if report_stale {
        open.push("reports/case-report.html이 끝나지 않은 최종화의 검토자 기록으로 만들어진 채 남아 있습니다 — 최종화를 다시 실행하면 새로 만들어집니다 (그 전에는 최종 결과로 쓰지 마십시오).".to_string());
    }
    if open.is_empty() {
        blocking |= close_and_remove(case_dir, journal, "recovered", state, budget, &mut open);
    } else {
        // Kept for a later attempt; its state says how far recovery got
        // (the local record stays: it is what lets that attempt act).
        let kept = if blocking {
            STATE_INTERRUPTED
        } else {
            STATE_RECOVERED
        };
        if let Err(err) = journal.rewrite_as(kept, case_dir) {
            open.push(err);
        }
    }
    let mut detail = format!(
        "이전 최종화(검토자 입력값: {}, 서명 SHA-256 {})가 끝나지 않은 채 중단된 것을 찾아(이 PC의 로컬 기록·서명 감사 레인의 최종화 시작 기록과 일치) 정리했습니다.",
        journal.examiner,
        short(&sha)
    );
    for line in &done {
        detail.push(' ');
        detail.push_str(line);
    }
    if !open.is_empty() {
        detail.push_str(" 확인 필요: ");
        detail.push_str(&open.join(" "));
    }
    Recovery {
        detail,
        report_stale,
        blocking,
        recovered: true,
        set_aside: false,
    }
}

fn report_names(case_dir: &Path, sha: &str) -> bool {
    std::fs::read_to_string(case_dir.join("reports/case-report.html"))
        .ok()
        .is_some_and(|html| crate::report::report_signoff_sha256(&html) == Some(sha))
}

/// Closes the begin entry (`finalize-resolved`, `outcome`) and removes the
/// journal and (R14 P18) the local record. A failed lane append is logged
/// and keeps the local record `open` (the journal still goes: a replay of
/// an unclosed begin redoes the same idempotent cleanup). R15 (P21): the
/// record is rewritten `resolved` BEFORE the journal goes, then removed.
///
/// R17-S4 (P28): R16 ignored a failed `resolved` advance and removed the
/// journal while the record still read `open`. Now the `open` file is
/// deleted instead; when that fails too the journal is KEPT (returns
/// `true`: blocking — the next recovery finds the lane's resolved entry and
/// tries again).
fn close_and_remove(
    case_dir: &Path,
    journal: &FinalizeJournal,
    outcome: &str,
    state: &SharedState,
    budget: Duration,
    open: &mut Vec<String>,
) -> bool {
    let resolved = match crate::report::record_finalize_resolved(
        case_dir,
        &journal.finalize_id,
        Some(&journal.signoff_sha256),
        outcome,
    ) {
        Ok(()) => match advance_or_drop(journal, case_dir, LOCAL_RESOLVED, None) {
            Ok(Settled::Advanced) => true,
            Ok(Settled::Dropped) => {
                state_lock(state)
                    .logs
                    .push(format!("경고: {}", dropped_note(&journal.finalize_id)));
                true
            }
            Err(err) => {
                let detail = format!(
                    "이 PC의 최종화 로컬 기록에 '정리됨'을 남기지 못했고 열린 기록도 지우지 못해({err}) 최종화 저널을 그대로 두었습니다 — 로컬 설정 폴더에 쓸 수 있는지 확인한 뒤 케이스를 다시 여십시오."
                );
                remember_complete_failure(&journal.finalize_id, &detail);
                open.push(detail);
                return true;
            }
        },
        Err(err) => {
            state_lock(state).logs.push(format!("경고: {err}"));
            false
        }
    };
    if let Err(err) = remove_journal_within(case_dir, &journal.finalize_id, budget) {
        open.push(format!("최종화 저널을 지우지 못했습니다: {err}"));
    } else if resolved {
        let _ = remove_local_record(&journal.finalize_id);
    }
    false
}

/// R11-S3 / R12-S6/S7: takes a promoted sign-off back — the previous record
/// written back durably (temp file fsync'd, write-through rename over the
/// promoted one) or the promoted one removed when there was none. R12-S6: a
/// previous record that was itself an abandoned finalize's is NOT restored
/// (the promoted one is removed) and `Ok(Some(note))` says so.
pub(crate) fn restore_previous(
    target: &Path,
    previous: Option<&[u8]>,
    previous_abandoned: bool,
    state: &SharedState,
    budget: Duration,
) -> Result<Option<String>, String> {
    let remove = |state: &SharedState| {
        super::jobs::retry_logged_within(
            state,
            "검토자 기록 되돌리기",
            budget,
            &mut || crate::util::remove_file_durable(target),
        )
        .map_err(|err| format!("{}: {err}", target.display()))
    };
    match previous {
        Some(_) if previous_abandoned => {
            remove(state)?;
            Ok(Some(format!(
                "이전 검토자 기록({})은 이미 끝나지 않은 최종화의 기록(감사 로그에 '완료되지 않음')이어서 되돌리지 않았습니다 — 지금 확정된 검토자 기록이 없습니다. 최종화를 다시 실행하십시오.",
                target.display()
            )))
        }
        Some(bytes) => {
            let temp = crate::util::unique_temp_sibling(target);
            write_synced(&temp, bytes).map_err(|err| format!("{}: {err}", temp.display()))?;
            super::jobs::retry_logged_within(
                state,
                "검토자 기록 되돌리기",
                budget,
                &mut || crate::util::rename_durable(&temp, target),
            )
            .map_err(|err| {
                let _ = std::fs::remove_file(&temp);
                format!("{}: {err}", target.display())
            })?;
            Ok(None)
        }
        None => remove(state).map(|()| None),
    }
}

/// R12-S2: the journal is gone once nothing it records is unresolved —
/// after a make-report the workstation ran succeeded (the mark-refresh
/// path), the report no longer names that sign-off; if the promoted
/// sign-off is not in place either and its package is marked, the begin
/// entry is closed and the journal removed. Only a paired journal of this
/// PC is touched (R13; R14: with its local record).
pub(crate) fn clear_if_resolved(case_dir: &Path, state: &SharedState) {
    // The caller checked that no finalize of this workstation runs.
    let Some(assessment) = assess(case_dir) else {
        return;
    };
    let (journal, begin) = match assessment {
        Assessment::Open(journal, begin) => (journal, begin),
        Assessment::Closed(journal, outcome) => {
            // R16 (P24): the record past `open` before the journal goes.
            if close_local(&journal, case_dir, &outcome, None).is_ok()
                && remove_journal(case_dir, &journal.finalize_id).is_ok()
            {
                let _ = remove_local_record(&journal.finalize_id);
            }
            return;
        }
        _ => return,
    };
    let sha = begin.signoff_record_sha256.clone();
    let promoted = std::fs::read(case_dir.join(crate::report::SIGNOFF_REL_PATH))
        .is_ok_and(|bytes| crate::sha256::digest_bytes(&bytes) == sha);
    let lane_open = crate::report::signoff_lane_state_sha(case_dir, &sha)
        .map(|lane| lane.recorded && !lane.abandoned)
        .unwrap_or(true);
    if !promoted
        && !report_names(case_dir, &sha)
        && !lane_open
        && own_reserved(case_dir, &begin.package_name, &sha).is_none()
        && !kept_parts_for(case_dir, &begin.package_name)
            .iter()
            .any(|part| package_signoff_is(part, &sha))
    {
        let mut open = Vec::new();
        let _kept = close_and_remove(
            case_dir,
            &journal,
            "recovered",
            state,
            crate::package::rename_retry_budget(),
            &mut open,
        );
    }
}

/// R13-S11 / R14-S12: the last status answer per case, keyed by the size
/// and mtime of everything it was judged from — the journal, the sign-off
/// lane, `review/signoff.json`, the `reports/` folder (P19), this PC's
/// local record folder (P18) — and by the fingerprint of this PC's audit
/// key set (R13 kept serving a pairing judged under keys since removed or
/// added).
/// R15-S7: also the reserved package folder, its `manifest.sha256` and the
/// local record file itself (R14 missed a change INSIDE the package — a
/// listed file removed — which P19 / P21 judge by).
type Stamp = Option<(u64, SystemTime)>;
/// R16 (P24): the local record is all its state files — one stamp each.
type StatusCacheKey = (PathBuf, [Stamp; 7], Vec<Stamp>, String);

/// One cached answer: the key, the journal read and what it was judged by.
type StatusCacheEntry = (
    StatusCacheKey,
    Result<FinalizeJournal, String>,
    Option<Inputs>,
);

fn status_cache() -> &'static Mutex<Vec<StatusCacheEntry>> {
    static CACHE: Mutex<Vec<StatusCacheEntry>> = Mutex::new(Vec::new());
    &CACHE
}

fn file_stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// The cached journal read and inputs for `case_dir` (disk work outside
/// every lock; the cache mutex is held only to look up / store).
fn cached_journal(case_dir: &Path) -> Option<Assessment> {
    let journal_stamp = file_stamp(&journal_path(case_dir));
    journal_stamp?;
    // The journal itself is small (read capped) and names the package and
    // the local record the key must follow.
    let named = read_journal(case_dir).ok().flatten();
    let package = named
        .as_ref()
        .filter(|journal| crate::package::valid_reserved_package_name(&journal.package_name))
        .map(|journal| case_dir.join("reports").join(&journal.package_name));
    let key: StatusCacheKey = (
        case_dir.to_path_buf(),
        [
            journal_stamp,
            file_stamp(&case_dir.join(crate::report::SIGNOFF_LOG_REL_PATH)),
            file_stamp(&case_dir.join(crate::report::SIGNOFF_REL_PATH)),
            file_stamp(&case_dir.join("reports")),
            local_open_dir().and_then(|dir| file_stamp(&dir)),
            package.as_deref().and_then(file_stamp),
            package
                .as_deref()
                .and_then(|dir| file_stamp(&dir.join("manifest.sha256"))),
        ],
        named
            .as_ref()
            .map(|journal| {
                local_record_paths(&journal.finalize_id)
                    .iter()
                    .map(|(_, path)| file_stamp(path))
                    .collect()
            })
            .unwrap_or_default(),
        crate::audit_key::verification_fingerprint(),
    );
    let hit = status_cache().lock().ok().and_then(|cache| {
        cache
            .iter()
            .find(|(cached, ..)| *cached == key)
            .map(|(_, journal, inputs)| (journal.clone(), inputs.clone()))
    });
    let (journal, inputs) = match hit {
        Some(hit) => hit,
        None => {
            let journal = match read_journal(case_dir) {
                Ok(None) => return None,
                Ok(Some(journal)) => Ok(journal),
                Err(err) => Err(err),
            };
            let inputs = journal
                .as_ref()
                .ok()
                .map(|journal| gather(case_dir, journal, false));
            if let Ok(mut cache) = status_cache().lock() {
                cache.retain(|(cached, ..)| cached.0 != key.0);
                keep_newest(&mut cache, 16);
                cache.push((key, journal.clone(), inputs.clone()));
            }
            (journal, inputs)
        }
    };
    // R16-S5: a record past `open` is judged by the manifest's BYTES — a
    // same-size rewrite with its mtime restored keeps the cache key. Up to
    // 16 MiB it is hashed again on every poll. R17-S9: a larger one is
    // never hashed here ("확인 보류"; every recovery hashes it, streaming)
    // — R16 streamed up to 1 GiB on each cache miss of the status poll.
    let inputs = inputs.map(|mut inputs| {
        let judged_by_manifest = matches!(
            &inputs.local,
            Ok(Some(record)) if record.state != LOCAL_OPEN && record.state != LOCAL_RESOLVED
        );
        if judged_by_manifest && let Ok(journal) = &journal {
            (
                inputs.manifest_sha256,
                inputs.manifest_withheld,
                inputs.manifest_stamp,
            ) = status_manifest_sha256(case_dir, &journal.package_name);
        }
        inputs
    });
    Some(classify(
        case_dir,
        journal,
        |journal| inputs.unwrap_or_else(|| gather(case_dir, journal, false)),
        false,
    ))
}

/// `/api/status` `finalize_journal`: the journal on disk, summarized
/// (`null` when there is none, and while this workstation's own finalize
/// runs — `active`). R13: `recovering` while this server's background
/// start-up recovery runs on the case. See the track U contract in
/// docs/PLAN_R14_2026-10-16.md "§S14 as implemented".
pub(crate) fn status_value(case_dir: &Path, active: bool, recovering: bool) -> serde_json::Value {
    if active {
        return serde_json::Value::Null;
    }
    if recovering {
        return status_base(STATE_RECOVERING);
    }
    match cached_journal(case_dir) {
        Some(assessment) => status_of(case_dir, assessment),
        None => serde_json::Value::Null,
    }
}

/// Every `finalize_journal` field, at its "nothing known" value.
fn status_base(state: &str) -> serde_json::Value {
    serde_json::json!({
            "path": FINALIZE_JOURNAL_REL_PATH,
            "state": state,
            "examiner": null,
            "started_unix": null,
            "signoff_sha256": null,
            "error": null,
            "reason": null,
            "unpaired_code": null,
            "outcome": null,
            "finalize_id": null,
            "host_id": null,
            "host": null,
            "this_host": null,
            "package_name": null,
            "local_record": null,
            "local_record_state": null,
            "in_progress_code": null,
            "blocks_finalize": true,
            "blocked_until_unix": null,
            "dismiss_command": null,
            "dismiss_command_cmd": null,
            "force_dismiss_command": null,
            "force_dismiss_command_cmd": null,
            "writer_gone_command": null,
            "writer_gone_command_cmd": null,
            "writer_gone_i_am_sure": null,
            "command_unavailable_reason": null,
            "manifest_check": null,
    })
}

/// [`status_value`] of an assessment already made (R18-S2: the CLI's is
/// the full-hash one, never the status path's).
fn status_of(case_dir: &Path, assessment: Assessment) -> serde_json::Value {
    let base = status_base;
    // R18-S1 (P29): a command is offered only for a paste-safe path;
    // otherwise every command field stays null and this says why.
    let unavailable = |value: &mut serde_json::Value| {
        if paste_safe_path(case_dir).is_none() {
            value["command_unavailable_reason"] = COMMAND_UNAVAILABLE_REASON.into();
        }
    };
    // R14-S5: the dismiss command is offered only where moving the journal
    // aside is the examiner's way out (nothing a recovery could clean up).
    let offer_dismiss = |value: &mut serde_json::Value| {
        value["dismiss_command"] = dismiss_command(case_dir).into();
        value["dismiss_command_cmd"] = dismiss_command_cmd(case_dir).into();
        unavailable(value);
    };
    // R16-S2/S4: where a writer may still run, only `--dismiss --force`
    // moves the journal (nothing is rolled back or recorded). R17-S8: only
    // for a writer this PC could NOT verify (unverified, other account) —
    // a verified live writer gets no command at all (wait) — and R17-S2
    // with `--writer-gone` (the normal recovery, writer confirmed gone).
    // R18-S7: with `--i-am-sure` while a live process matches the writer's
    // pid and start time.
    let offer_writer_ways = |value: &mut serde_json::Value, journal: &FinalizeJournal| {
        let sure = writer_gone_needs_sure(journal);
        value["force_dismiss_command"] = force_dismiss_command(case_dir).into();
        value["force_dismiss_command_cmd"] = force_dismiss_command_cmd(case_dir).into();
        value["writer_gone_command"] = writer_gone_command(case_dir, sure).into();
        value["writer_gone_command_cmd"] = writer_gone_command_cmd(case_dir, sure).into();
        value["writer_gone_i_am_sure"] = sure.into();
        unavailable(value);
    };
    let with = |state: &str, journal: &FinalizeJournal| {
        let mut value = base(state);
        value["examiner"] = journal.examiner.clone().into();
        value["started_unix"] = journal.started_unix.into();
        value["signoff_sha256"] = journal.signoff_sha256.clone().into();
        value["finalize_id"] = (!journal.finalize_id.is_empty())
            .then(|| journal.finalize_id.clone())
            .into();
        value["host_id"] = (!journal.host_id.is_empty())
            .then(|| journal.host_id.clone())
            .into();
        // Track U reads `host` (the PC the journal names: short host id).
        value["host"] = (!journal.host_id.is_empty())
            .then(|| short(&journal.host_id).to_string())
            .into();
        value["this_host"] =
            (journal.host_id.is_empty() || journal.host_id == crate::util::host_id()).into();
        value["package_name"] = (!journal.package_name.is_empty())
            .then(|| journal.package_name.clone())
            .into();
        // R15 (P21): the state of this PC's local record of it.
        value["local_record_state"] = read_local_record(&journal.finalize_id)
            .ok()
            .flatten()
            .map(|record| record.state)
            .into();
        value
    };
    match assessment {
        Assessment::Unreadable(err) => {
            let mut value = base(STATE_UNREADABLE);
            value["error"] = err.into();
            offer_dismiss(&mut value);
            value
        }
        Assessment::OtherHost {
            journal,
            blocking,
            until_unix,
        } => {
            let mut value = with(STATE_OTHER_HOST, &journal);
            value["blocks_finalize"] = blocking.into();
            value["blocked_until_unix"] = until_unix.into();
            offer_dismiss(&mut value);
            value
        }
        Assessment::LiveWriter(journal) => {
            let mut value = with(STATE_IN_PROGRESS, &journal);
            value["reason"] = live_writer_text(&journal).into();
            value["local_record"] = true.into();
            value["in_progress_code"] = IN_PROGRESS_LIVE_WRITER.into();
            value
        }
        // R16-S2 (P25): blocks; no plain dismiss (it would clean up a
        // finalize that may still run), only `--force`.
        Assessment::UnverifiedWriter(journal) => {
            let mut value = with(STATE_IN_PROGRESS, &journal);
            value["reason"] = unverified_writer_text(&journal, case_dir).into();
            value["local_record"] = true.into();
            value["in_progress_code"] = IN_PROGRESS_UNVERIFIED_WRITER.into();
            offer_writer_ways(&mut value, &journal);
            value
        }
        // R15-S10: blocks; no dismiss command (the examiner is not led to
        // move another account's live journal aside).
        Assessment::OtherAccount {
            journal,
            image,
            until_unix,
        } => {
            let mut value = with(STATE_IN_PROGRESS, &journal);
            value["reason"] = other_account_text(&journal, &image, until_unix, case_dir).into();
            value["in_progress_code"] = IN_PROGRESS_OTHER_ACCOUNT.into();
            value["blocked_until_unix"] = until_unix.into();
            value["local_record"] = serde_json::Value::Null;
            offer_writer_ways(&mut value, &journal);
            value
        }
        Assessment::Unpaired(journal, why) => {
            let mut value = with(STATE_UNPAIRED, &journal);
            value["reason"] = why.reason.into();
            value["unpaired_code"] = why.code.into();
            value["local_record"] = (why.code != UNPAIRED_NO_LOCAL_RECORD).into();
            if why.remembered {
                value["manifest_check"] = "recovery-verdict".into();
            }
            offer_dismiss(&mut value);
            value
        }
        Assessment::Closed(journal, outcome) => {
            let mut value = with(STATE_COMPLETED, &journal);
            value["outcome"] = outcome.into();
            // R16 (P24): a recovery that could not record the closed
            // state locally kept the journal and blocks — said here.
            match complete_failure(&journal.finalize_id) {
                Some(detail) => {
                    value["reason"] = detail.into();
                    value["blocks_finalize"] = true.into();
                }
                None => value["blocks_finalize"] = false.into(),
            }
            value
        }
        Assessment::Finished {
            journal,
            why,
            check,
            ..
        } => {
            let mut value = with(STATE_COMPLETED_UNRECORDED, &journal);
            value["outcome"] = STATE_COMPLETED.into();
            value["local_record"] = true.into();
            // R17-S9: "확인 보류" — the manifest was not hashed here.
            // R18-S2 (P31): then whether it blocks is unknown (null),
            // unless a recovery's full-hash verdict is remembered.
            let blocks = match check {
                ManifestCheck::Hashed => serde_json::Value::Bool(false),
                ManifestCheck::Withheld => {
                    value["manifest_check"] = "withheld".into();
                    serde_json::Value::Null
                }
                ManifestCheck::Remembered => {
                    value["manifest_check"] = "recovery-verdict".into();
                    serde_json::Value::Bool(false)
                }
            };
            // R15-S6: a recovery that could not write the missing entry
            // blocks the finalize — said here in the same words.
            match complete_failure(&journal.finalize_id) {
                Some(detail) => {
                    value["reason"] = detail.into();
                    value["blocks_finalize"] = true.into();
                }
                None => {
                    value["reason"] = why.into();
                    value["blocks_finalize"] = blocks;
                }
            }
            value
        }
        Assessment::Open(journal, _) => {
            // An in-progress journal whose writer is gone is not
            // recovered yet: "interrupted" until a recovery runs.
            let state = if journal.state == STATE_RECOVERED {
                STATE_RECOVERED
            } else {
                STATE_INTERRUPTED
            };
            let mut value = with(state, &journal);
            value["local_record"] = true.into();
            value["blocks_finalize"] = (state != STATE_RECOVERED).into();
            value
        }
    }
}

/// R13-S6 / R14-S3/S5: `frametrace finalize-journal <case> [--dismiss
/// [--force]]` — what the journal is (the same states as /api/status) and,
/// with `dismiss`, moves it aside (never deleted). A journal this PC can
/// clean up (interrupted, finished, completed) is cleaned up FIRST — R13
/// moved an interrupted one aside and left its published package plainly
/// named; only what that leaves is moved, and a cleanup that left
/// something blocking needs `--force`. A journal whose writer still runs
/// is refused without `--force`.
pub(crate) fn cli_command(case_dir: &Path, dismiss: bool, force: bool) -> Result<String, String> {
    let Some(assessment) = assess(case_dir) else {
        return Ok(format!(
            "finalize journal: none ({} does not exist)\n",
            journal_path(case_dir).display()
        ));
    };
    // R18-S2 (P31): the CLI shows the FULL-hash assessment (the status
    // path withholds manifests over 16 MiB).
    let status = status_of(case_dir, assessment.clone());
    let mut out = format!(
        "finalize journal: {}\nstate: {}\n",
        journal_path(case_dir).display(),
        status["state"].as_str().unwrap_or_default()
    );
    for (label, key) in [
        ("examiner", "examiner"),
        ("started_unix", "started_unix"),
        ("signoff_sha256", "signoff_sha256"),
        ("package_name", "package_name"),
        ("host_id", "host_id"),
        ("outcome", "outcome"),
        ("unpaired_code", "unpaired_code"),
        ("reason", "reason"),
        ("error", "error"),
    ] {
        if !status[key].is_null() {
            let value = status[key]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| status[key].to_string());
            out.push_str(&format!("{label}: {value}\n"));
        }
    }
    if status["this_host"] == false {
        out.push_str("host: another PC (never recovered here)\n");
    }
    if !dismiss {
        if let Some(command) = status["dismiss_command"].as_str() {
            out.push_str(&format!("to move it aside (never deleted): {command}\n"));
            if let Some(cmd) = status["dismiss_command_cmd"].as_str() {
                out.push_str(&format!("  (cmd.exe: {cmd})\n"));
            }
        } else if let Some(why) = status["command_unavailable_reason"].as_str() {
            out.push_str(&format!(
                "to move it aside (never deleted): frametrace finalize-journal <케이스 폴더> --dismiss ({why})\n"
            ));
        }
        return Ok(out);
    }
    let assessment = match assessment {
        // R16-S2 (P25): maybe still running — never cleaned up here.
        Assessment::UnverifiedWriter(journal) if !force => {
            return Err(format!(
                "{} 저널을 옮기지 않았습니다.",
                unverified_writer_text(&journal, case_dir)
            ));
        }
        // R17-S8: a verified live writer — wait (no command offered).
        Assessment::LiveWriter(journal) if !force => {
            return Err(format!(
                "{} 저널을 옮기지 않았습니다.",
                live_writer_text(&journal)
            ));
        }
        // R15-S10: another account's FrameTrace writer — never moved
        // without --force.
        Assessment::OtherAccount {
            journal,
            image,
            until_unix,
        } if !force => {
            return Err(format!(
                "{} 저널을 옮기지 않았습니다.",
                other_account_text(&journal, &image, until_unix, case_dir)
            ));
        }
        // R14-S5: cleaned up first, as opening the case would.
        Assessment::Open(..) | Assessment::Finished { .. } | Assessment::Closed(..) => {
            let state: SharedState = Arc::new(Mutex::new(JobState::new()));
            let detail = recover(case_dir, &state, crate::package::rename_retry_budget())
                .map(|recovery| recovery.detail);
            if let Some(detail) = &detail {
                out.push_str(&format!("cleanup: {detail}\n"));
            }
            push_logs(&mut out, &state, detail.as_deref());
            match assess(case_dir) {
                None => {
                    out.push_str("저널이 정리되어 옮길 것이 없습니다.\n");
                    return Ok(out);
                }
                Some(Assessment::Open(journal, _))
                    if journal.state != STATE_RECOVERED && !force =>
                {
                    return Err(format!(
                        "{out}정리 뒤에도 확인할 것이 남아 저널을 옮기지 않았습니다 (위 '확인 필요' 참고) — 확인했다면 --dismiss --force"
                    ));
                }
                Some(left) => left,
            }
        }
        other => other,
    };
    // R16-S8: a writer that may still run (verified or not) is left to
    // finish: only its journal moves — no `finalize-resolved`, its local
    // record kept (R15 recorded `dismissed` and the live finalize then
    // wrote `finalize-complete` after it).
    let closing = match &assessment {
        Assessment::Open(journal, _) => Some(journal.clone()),
        _ => None,
    };
    let still_running = match &assessment {
        Assessment::LiveWriter(journal)
        | Assessment::UnverifiedWriter(journal)
        | Assessment::OtherAccount { journal, .. } => Some(journal.clone()),
        _ => None,
    };
    // R17-S2: named BEFORE the move (what stays where it is).
    let left = still_running
        .as_ref()
        .map(|journal| left_in_place(case_dir, journal));
    let moved = set_aside(case_dir, "dismissed")?;
    out.push_str(&format!("moved to: {}\n", moved.display()));
    if let Some(left) = left {
        out.push_str("writer may still be running: nothing recorded in the sign-off lane, its local record kept\n");
        out.push_str(&format!(
            "그대로 남은 것 (되돌리거나 확정하지 않음): {left}. 작성자가 끝나면 이 최종화가 스스로 마무리하지 못할 수 있으니 review/signoff.json과 그 패키지를 직접 확인하십시오.\n"
        ));
    }
    if let Some(journal) = closing {
        match crate::report::record_finalize_resolved(
            case_dir,
            &journal.finalize_id,
            Some(&journal.signoff_sha256),
            "dismissed",
        ) {
            Ok(()) => {
                // R16 (P24): `resolved` first, then removed. R17 (P28): a
                // record that cannot advance loses its `open` file.
                match advance_or_drop(&journal, case_dir, LOCAL_RESOLVED, None) {
                    Ok(Settled::Advanced) => {
                        let _ = remove_local_record(&journal.finalize_id);
                    }
                    Ok(Settled::Dropped) => {
                        out.push_str(&format!("{}\n", dropped_note(&journal.finalize_id)));
                    }
                    Err(err) => out.push_str(&format!(
                        "warning: 이 PC의 최종화 로컬 기록이 '열림'으로 남았습니다 ({err}) — 그 파일({})을 지울 수 있게 되면 지우십시오 (지워도 되돌리는 것은 없음)\n",
                        local_state_path(&journal.finalize_id, LOCAL_OPEN)
                            .map(|path| path.display().to_string())
                            .unwrap_or_default()
                    )),
                }
                out.push_str("sign-off lane: finalize-resolved (dismissed) recorded\n");
            }
            Err(err) => out.push_str(&format!("warning: {err}\n")),
        }
    }
    out.push_str("저널을 옮겼습니다 (지우지 않음). 아무것도 되돌리지 않았습니다 — review/signoff.json과 reports 폴더의 패키지를 확인한 뒤 최종화를 다시 실행하십시오.\n");
    Ok(out)
}

/// R17-S2: `frametrace finalize-journal <case> --writer-gone` — the
/// examiner confirmed that the process the journal names as its writer
/// (an unverified writer, another account's process) is NOT a finalize
/// still running: the journal is judged as if its writer were gone and the
/// NORMAL recovery runs (an interrupted finalize of this PC is cleaned up
/// as opening the case would — sign-off taken back, package marked; a
/// finished one only gets its complete entry). `--dismiss --force` only
/// moves the journal and leaves the sign-off and package as they are; this
/// is the way to actually clean an interrupted finalize up. A VERIFIED live
/// writer (this program's folder and SHA-256) is refused: wait for it.
///
/// R18-S3: the second look (as if gone) must see the SAME journal as the
/// first — finalize id and writer (pid, start time) — else nothing is
/// done; and a writer verified as this program is refused there too.
/// R18-S7: while a live process still matches the writer's pid and start
/// time ([`writer_gone_needs_sure`]), `i_am_sure` is required. R18-S4: the
/// recovery's warnings (`state.logs`) are printed at the end.
pub(crate) fn cli_writer_gone(case_dir: &Path, i_am_sure: bool) -> Result<String, String> {
    let Some(assessment) = assess(case_dir) else {
        return Ok(format!(
            "finalize journal: none ({} does not exist)\n",
            journal_path(case_dir).display()
        ));
    };
    let live_refusal = |journal: &FinalizeJournal| {
        format!(
            "{} 저널의 작성자가 이 프로그램과 같은 실행 파일로 확인된 실행 중 프로세스여서 --writer-gone을 적용하지 않았습니다.",
            live_writer_text(journal)
        )
    };
    if let Assessment::LiveWriter(journal) = &assessment {
        return Err(live_refusal(journal));
    }
    let first = assessment_journal(&assessment).map(writer_key);
    if let Assessment::UnverifiedWriter(journal) | Assessment::OtherAccount { journal, .. } =
        &assessment
        && writer_gone_needs_sure(journal)
        && !i_am_sure
    {
        return Err(format!(
            "저널에 적힌 작성자 pid {}와 시작 시각이 같은 프로세스가 아직 실행 중입니다 — 진행 중인 최종화일 수 있어 --writer-gone을 적용하지 않았습니다. 그 프로세스가 이 케이스의 FrameTrace 최종화가 아님을 직접 확인했다면 --writer-gone --i-am-sure로 다시 실행하십시오{}.",
            journal.writer.pid,
            writer_gone_command(case_dir, true)
                .map(|command| format!(" (`{command}`)"))
                .unwrap_or_else(|| format!(" ({COMMAND_UNAVAILABLE_REASON})"))
        ));
    }
    #[cfg(test)]
    BETWEEN_WRITER_GONE_LOOKS.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook(case_dir);
        }
    });
    let Some(assumed) = assess_with(case_dir, true) else {
        return Ok("저널이 없어졌습니다 (그 사이에 정리됨).\n".to_string());
    };
    if let Assessment::LiveWriter(journal) = &assumed {
        return Err(live_refusal(journal));
    }
    if assessment_journal(&assumed).map(writer_key) != first {
        return Err(
            "처음 확인한 뒤 최종화 저널이 바뀌었습니다(최종화 ID 또는 작성자가 다름) — 아무것도 바꾸지 않았습니다. 상태를 다시 확인한 뒤 실행하십시오.".to_string(),
        );
    }
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let recovery = recover_assessed(
        case_dir,
        assumed,
        &state,
        crate::package::rename_retry_budget(),
    );
    let mut out = format!(
        "finalize journal: {}\nwriter: treated as gone (--writer-gone)\nrecovery: {}\n",
        journal_path(case_dir).display(),
        recovery.detail
    );
    match assess(case_dir) {
        None => out.push_str("저널이 정리되었습니다.\n"),
        Some(left) => {
            let status = status_of(case_dir, left);
            out.push_str(&format!(
                "journal kept: state {}{}\n",
                status["state"].as_str().unwrap_or_default(),
                status["unpaired_code"]
                    .as_str()
                    .map(|code| format!(" ({code})"))
                    .unwrap_or_default()
            ));
        }
    }
    push_logs(&mut out, &state, Some(&recovery.detail));
    if recovery.blocking {
        return Err(format!(
            "{out}정리 뒤에도 확인할 것이 남았습니다 (위 내용 참고) — 다음 최종화는 막힙니다."
        ));
    }
    Ok(out)
}

#[cfg(test)]
thread_local! {
    /// R18-S3 test hook: runs in [`cli_writer_gone`] between its first
    /// look and the one as if the writer were gone (this thread only).
    #[allow(clippy::type_complexity)]
    pub(crate) static BETWEEN_WRITER_GONE_LOOKS: std::cell::RefCell<Option<Box<dyn Fn(&Path)>>> =
        const { std::cell::RefCell::new(None) };
}

/// R18-S4: what a CLI recovery logged (`state.logs`: warnings such as a
/// dropped local record, a rename retried) — R17's `--writer-gone`
/// printed none of it. The lock is held only to take the lines.
/// The recovery's own summary (`경고: {detail}`, already printed) is
/// skipped.
fn push_logs(out: &mut String, state: &SharedState, detail: Option<&str>) {
    let logs = std::mem::take(&mut state_lock(state).logs);
    let summary = detail.map(|detail| format!("경고: {detail}"));
    for line in logs {
        if summary.as_deref() != Some(line.as_str()) {
            out.push_str(&format!("log: {line}\n"));
        }
    }
}

/// The journal an assessment judged (`None`: unreadable).
fn assessment_journal(assessment: &Assessment) -> Option<&FinalizeJournal> {
    match assessment {
        Assessment::Unreadable(_) => None,
        Assessment::OtherHost { journal, .. }
        | Assessment::LiveWriter(journal)
        | Assessment::UnverifiedWriter(journal)
        | Assessment::OtherAccount { journal, .. }
        | Assessment::Unpaired(journal, _)
        | Assessment::Closed(journal, _)
        | Assessment::Finished { journal, .. }
        | Assessment::Open(journal, _) => Some(journal),
    }
}

/// R18-S3: what the two looks of `--writer-gone` must agree on. R19-S5:
/// the whole identity of the finalize and its writer — also the package
/// name, the host id and the writer's executable hash (R18 compared only
/// id, pid, start time and sign-off hash).
#[allow(clippy::type_complexity)]
fn writer_key(
    journal: &FinalizeJournal,
) -> (
    String,
    String,
    String,
    u32,
    Option<u64>,
    Option<String>,
    Option<String>,
    String,
) {
    (
        journal.finalize_id.clone(),
        journal.package_name.clone(),
        journal.host_id.clone(),
        journal.writer.pid,
        journal.writer.started_ms,
        journal.writer.image.clone(),
        journal.writer_image_sha256.clone(),
        journal.signoff_sha256.clone(),
    )
}

/// R19-S4 test hook: remembers a package-changed verdict for (`case_dir`,
/// `finalize_id`, `stamp`) as a recovery would.
#[cfg(test)]
pub(crate) fn remember_package_changed_for_test(
    case_dir: &Path,
    finalize_id: &str,
    stamp: (u64, SystemTime),
) {
    let case = crate::paste_path::normalize(case_dir);
    let why = UnpairedWhy::new(UNPAIRED_PACKAGE_CHANGED, "test");
    if let Ok(mut verdicts) = recovery_verdicts().lock() {
        verdicts.push((
            (case, finalize_id.to_string(), stamp),
            RecoveryVerdict::PackageChanged(why),
        ));
    }
}

/// R19-S4 test hook: whether a verdict is remembered for that key.
#[cfg(test)]
pub(crate) fn verdict_remembered_for_test(
    case_dir: &Path,
    finalize_id: &str,
    stamp: (u64, SystemTime),
) -> bool {
    remembered_verdict(case_dir, finalize_id, Some(stamp)).is_some()
}
