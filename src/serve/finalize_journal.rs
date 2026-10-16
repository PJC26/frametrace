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

/// One finalize's journal (R13 format: schema 2).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct FinalizeJournal {
    /// The id its `finalize-begin` lane entry names (empty: a journal of
    /// an R12 build, which wrote no lane entry — always unpaired).
    pub(crate) finalize_id: String,
    pub(crate) host_id: String,
    /// R13-S4: the process that wrote it (refreshed on every rewrite).
    pub(crate) writer: crate::util::WriterIdentity,
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

    /// R13-S4: rewritten by this process — it is the writer now.
    pub(crate) fn rewrite_as(&self, state: &str, case_dir: &Path) -> Result<(), String> {
        let mut kept = self.clone();
        kept.state = state.to_string();
        kept.writer = crate::util::current_writer();
        kept.write(case_dir)
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

/// Removes the journal and the previous sign-off kept beside it (retried
/// like the sign-off renames: an indexer or antivirus handle on the fresh
/// file must not leave it behind).
pub(crate) fn remove_journal(case_dir: &Path) -> Result<(), String> {
    remove_journal_within(case_dir, crate::package::rename_retry_budget())
}

fn remove_journal_within(case_dir: &Path, budget: Duration) -> Result<(), String> {
    let path = journal_path(case_dir);
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
pub(crate) fn mark_unfinished_packages(
    case_dir: &Path,
    package_name: &str,
    step_text: &str,
    why: &str,
    budget: Duration,
) -> MarkOutcome {
    let mut published: Vec<PathBuf> = published_reserved(case_dir, package_name)
        .into_iter()
        .collect();
    if let Some(named) = super::media::published_package_dir(case_dir, step_text) {
        let name = named
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !published.contains(&named)
            && !crate::package::is_incomplete_package_name(&name)
            && crate::package::finalize_incomplete_reason(&named).is_none()
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
    for part in kept_parts_for(case_dir, package_name) {
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
    /// Its writer (same PC) still runs.
    LiveWriter(FinalizeJournal),
    /// No trusted lane entry pairs with it (P15) — why.
    Unpaired(FinalizeJournal, String),
    /// The lane records it complete (`completed`) or resolved.
    Closed(FinalizeJournal, String),
    /// An interrupted finalize of this PC with its begin entry.
    Open(FinalizeJournal, FinalizeBegin),
}

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

/// R13-S4: whether the journal's writer (on this PC) still runs.
fn writer_alive(journal: &FinalizeJournal) -> bool {
    let writer = &journal.writer;
    if writer.pid == std::process::id() {
        // This process: its own finalize holds `busy`, so a journal seen
        // by a recovery is an earlier, interrupted one.
        return false;
    }
    let age = journal_age(journal).unwrap_or(OTHER_HOST_BLOCK);
    crate::util::writer_still_running(
        writer,
        crate::util::probe_process(writer.pid),
        crate::util::process_image_name(writer.pid).as_deref(),
        age,
        OTHER_HOST_BLOCK,
    )
}

/// R13: classifies the journal read from disk (`pairing` = the lane's
/// answer for its id, computed by the caller or cached).
fn classify(
    journal: Result<FinalizeJournal, String>,
    pairing: impl FnOnce(&FinalizeJournal) -> FinalizeLanePairing,
) -> Assessment {
    let journal = match journal {
        Ok(journal) => journal,
        Err(err) => return Assessment::Unreadable(err),
    };
    if !journal.host_id.is_empty() && journal.host_id != crate::util::host_id() {
        let age = journal_age(&journal);
        return Assessment::OtherHost {
            blocking: age.is_some_and(|age| age < OTHER_HOST_BLOCK),
            until_unix: journal.started_unix + OTHER_HOST_BLOCK.as_secs(),
            journal,
        };
    }
    if journal.state == STATE_IN_PROGRESS && writer_alive(&journal) {
        return Assessment::LiveWriter(journal);
    }
    if journal.finalize_id.is_empty() {
        return Assessment::Unpaired(
            journal,
            "이전 버전이 쓴 저널이라 서명 감사 레인에 짝이 되는 최종화 시작 기록이 없습니다"
                .to_string(),
        );
    }
    match pairing(&journal) {
        FinalizeLanePairing::Open(begin) => match journal.mismatch_with(&begin) {
            None => Assessment::Open(journal, begin),
            Some(field) => Assessment::Unpaired(
                journal,
                format!("저널의 {field}이(가) 서명 감사 레인의 최종화 시작 기록과 다릅니다"),
            ),
        },
        FinalizeLanePairing::Completed { .. } => {
            Assessment::Closed(journal, STATE_COMPLETED.to_string())
        }
        FinalizeLanePairing::Resolved { outcome } => Assessment::Closed(journal, outcome),
        FinalizeLanePairing::Unpaired(reason) => Assessment::Unpaired(journal, reason),
    }
}

/// [`classify`] of the journal on disk now (`None`: no journal).
pub(crate) fn assess(case_dir: &Path) -> Option<Assessment> {
    let journal = match read_journal(case_dir) {
        Ok(None) => return None,
        Ok(Some(journal)) => Ok(journal),
        Err(err) => Err(err),
    };
    Some(classify(journal, |journal| {
        crate::report::finalize_lane_pairing(case_dir, &journal.finalize_id)
    }))
}

fn dismiss_hint(case_dir: &Path) -> String {
    format!(
        "frametrace finalize-journal \"{}\" --dismiss",
        case_dir.display()
    )
}

/// R12-S2 / R13: recovers the interrupted finalize `case_dir`'s journal
/// points to — only when it pairs with its lane entry (see the module
/// docs). `None` when there is no journal. The caller makes sure no
/// finalize of this workstation is running on the case (it holds `busy`).
/// Restoring and the other cleanup steps are never cancellable (R12-S4);
/// `budget` bounds each rename's retries.
pub(crate) fn recover(case_dir: &Path, state: &SharedState, budget: Duration) -> Option<Recovery> {
    let assessment = assess(case_dir)?;
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
            detail: format!(
                "다른 FrameTrace 프로세스(pid {})가 이 케이스를 최종화하고 있어 최종화 저널을 그대로 두었습니다 — 그 최종화가 끝난 뒤 케이스를 다시 여십시오.",
                journal.writer.pid
            ),
            blocking: true,
            ..Recovery::default()
        },
        Assessment::Unpaired(journal, reason) => Recovery {
            detail: format!(
                "최종화 저널(검토자 입력값: {}, 서명 SHA-256 {})이 있지만 확인할 수 없어 아무것도 바꾸지 않았습니다 — {reason}. 누군가 만든 파일이거나 이전 버전·다른 키의 기록일 수 있습니다. 검토자 기록(review/signoff.json)과 reports 폴더를 확인한 뒤 `{hint}`로 저널을 옮기고(지우지 않음) 최종화를 다시 실행하십시오.",
                journal.examiner,
                short(&journal.signoff_sha256)
            ),
            blocking: true,
            ..Recovery::default()
        },
        Assessment::Closed(_, outcome) => Recovery {
            detail: match remove_journal_within(case_dir, budget) {
                Ok(()) => format!(
                    "이전 최종화는 서명 감사 레인에 '{outcome}'(으)로 기록되어 있어, 남아 있던 최종화 저널만 지웠습니다 (다른 것은 바꾸지 않음)."
                ),
                Err(err) => format!(
                    "이전 최종화는 서명 감사 레인에 '{outcome}'(으)로 기록되어 있지만 남은 최종화 저널을 지우지 못했습니다: {err}"
                ),
            },
            ..Recovery::default()
        },
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
    Some(recovery)
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

/// R13-S7 (P17): the start-up recovery of the bound case, in the
/// background — the server serves (status `finalize_journal.state:
/// "recovering"`) while it runs, holding `busy` so no job, open-case or
/// shutdown runs under it. R12 ran it before serving: a sign-off another
/// program held kept the app from starting for the whole retry budget.
pub(crate) fn spawn_bound_case_recovery(state: &SharedState) -> Option<thread::JoinHandle<()>> {
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
        #[cfg(test)]
        let _gate = STARTUP_RECOVERY_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let recovery = recover(&bound, &state, STARTUP_RETRY_BUDGET);
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
    let marked = mark_unfinished_packages(case_dir, &begin.package_name, "", reason, budget);
    done.extend(marked.notes);
    if marked.failed {
        open.push(
            "최종화 미완료 표시를 하지 못한 패키지가 있습니다 — 최종 결과로 쓰지 마십시오."
                .to_string(),
        );
    }
    if let Some(dir) = published_reserved(case_dir, &begin.package_name) {
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
    let blocking = !open.is_empty();
    if report_stale {
        open.push("reports/case-report.html이 끝나지 않은 최종화의 검토자 기록으로 만들어진 채 남아 있습니다 — 최종화를 다시 실행하면 새로 만들어집니다 (그 전에는 최종 결과로 쓰지 마십시오).".to_string());
    }
    if open.is_empty() {
        close_and_remove(case_dir, journal, "recovered", state, budget, &mut open);
    } else {
        // Kept for a later attempt; its state says how far recovery got.
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
        "이전 최종화(검토자 입력값: {}, 서명 SHA-256 {})가 끝나지 않은 채 중단된 것을 찾아(서명 감사 레인의 최종화 시작 기록과 일치) 정리했습니다.",
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
/// journal. A failed lane append is logged (the journal still goes: a
/// replay of an unclosed begin redoes the same idempotent cleanup).
fn close_and_remove(
    case_dir: &Path,
    journal: &FinalizeJournal,
    outcome: &str,
    state: &SharedState,
    budget: Duration,
    open: &mut Vec<String>,
) {
    if let Err(err) = crate::report::record_finalize_resolved(
        case_dir,
        &journal.finalize_id,
        Some(&journal.signoff_sha256),
        outcome,
    ) {
        state_lock(state).logs.push(format!("경고: {err}"));
    }
    if let Err(err) = remove_journal_within(case_dir, budget) {
        open.push(format!("최종화 저널을 지우지 못했습니다: {err}"));
    }
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
/// PC is touched (R13).
pub(crate) fn clear_if_resolved(case_dir: &Path, state: &SharedState) {
    // The caller checked that no finalize of this workstation runs.
    let Some(assessment) = assess(case_dir) else {
        return;
    };
    let (journal, begin) = match assessment {
        Assessment::Open(journal, begin) => (journal, begin),
        Assessment::Closed(..) => {
            let _ = remove_journal(case_dir);
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
        && published_reserved(case_dir, &begin.package_name).is_none()
        && kept_parts_for(case_dir, &begin.package_name).is_empty()
    {
        let mut open = Vec::new();
        close_and_remove(
            case_dir,
            &journal,
            "recovered",
            state,
            crate::package::rename_retry_budget(),
            &mut open,
        );
    }
}

/// R13-S11: the last status answer per case, keyed by the journal's and
/// the sign-off lane's size and mtime — the poll no longer re-reads the
/// journal and re-verifies the lane every second.
type StatusCacheKey = (
    PathBuf,
    Option<(u64, SystemTime)>,
    Option<(u64, SystemTime)>,
);

/// One cached answer: the key, the journal read and the lane's pairing.
type StatusCacheEntry = (
    StatusCacheKey,
    Result<FinalizeJournal, String>,
    Option<FinalizeLanePairing>,
);

fn status_cache() -> &'static Mutex<Vec<StatusCacheEntry>> {
    static CACHE: Mutex<Vec<StatusCacheEntry>> = Mutex::new(Vec::new());
    &CACHE
}

fn file_stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// The cached journal read and lane pairing for `case_dir` (disk work
/// outside every lock; the cache mutex is held only to look up / store).
fn cached_journal(case_dir: &Path) -> Option<Assessment> {
    let journal_stamp = file_stamp(&journal_path(case_dir))?;
    let key: StatusCacheKey = (
        case_dir.to_path_buf(),
        Some(journal_stamp),
        file_stamp(&case_dir.join(crate::report::SIGNOFF_LOG_REL_PATH)),
    );
    let hit = status_cache().lock().ok().and_then(|cache| {
        cache
            .iter()
            .find(|(cached, ..)| *cached == key)
            .map(|(_, journal, pairing)| (journal.clone(), pairing.clone()))
    });
    let (journal, pairing) = match hit {
        Some(hit) => hit,
        None => {
            let journal = match read_journal(case_dir) {
                Ok(None) => return None,
                Ok(Some(journal)) => Ok(journal),
                Err(err) => Err(err),
            };
            let pairing = journal
                .as_ref()
                .ok()
                .filter(|journal| !journal.finalize_id.is_empty())
                .map(|journal| {
                    crate::report::finalize_lane_pairing(case_dir, &journal.finalize_id)
                });
            if let Ok(mut cache) = status_cache().lock() {
                cache.retain(|(cached, ..)| cached.0 != key.0);
                cache.truncate(15);
                cache.push((key, journal.clone(), pairing.clone()));
            }
            (journal, pairing)
        }
    };
    Some(classify(journal, |_| {
        pairing.unwrap_or_else(|| FinalizeLanePairing::Unpaired(String::new()))
    }))
}

/// `/api/status` `finalize_journal`: the journal on disk, summarized
/// (`null` when there is none, and while this workstation's own finalize
/// runs — `active`). R13: `recovering` while this server's background
/// start-up recovery runs on the case. See the track U contract in
/// docs/PLAN_R13_2026-10-16.md "§S13 as implemented".
pub(crate) fn status_value(case_dir: &Path, active: bool, recovering: bool) -> serde_json::Value {
    if active {
        return serde_json::Value::Null;
    }
    let hint = dismiss_hint(case_dir);
    let base = |state: &str| {
        serde_json::json!({
            "path": FINALIZE_JOURNAL_REL_PATH,
            "state": state,
            "examiner": null,
            "started_unix": null,
            "signoff_sha256": null,
            "error": null,
            "reason": null,
            "finalize_id": null,
            "host_id": null,
            "host": null,
            "this_host": null,
            "package_name": null,
            "blocks_finalize": true,
            "blocked_until_unix": null,
            "dismiss_command": hint,
        })
    };
    if recovering {
        return base(STATE_RECOVERING);
    }
    let Some(assessment) = cached_journal(case_dir) else {
        return serde_json::Value::Null;
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
        value
    };
    match assessment {
        Assessment::Unreadable(err) => {
            let mut value = base(STATE_UNREADABLE);
            value["error"] = err.into();
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
            value
        }
        Assessment::LiveWriter(journal) => {
            let mut value = with(STATE_IN_PROGRESS, &journal);
            value["reason"] = format!(
                "다른 FrameTrace 프로세스(pid {})가 최종화하고 있습니다",
                journal.writer.pid
            )
            .into();
            value
        }
        Assessment::Unpaired(journal, reason) => {
            let mut value = with(STATE_UNPAIRED, &journal);
            value["reason"] = reason.into();
            value
        }
        Assessment::Closed(journal, _) => {
            let mut value = with(STATE_COMPLETED, &journal);
            value["blocks_finalize"] = false.into();
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
            value["blocks_finalize"] = (state != STATE_RECOVERED).into();
            value
        }
    }
}

/// R13-S6: `frametrace finalize-journal <case> [--dismiss]` — what the
/// journal is (the same states as /api/status) and, with `dismiss`, moves
/// it aside (never deleted) and closes its begin entry in the lane.
pub(crate) fn cli_command(case_dir: &Path, dismiss: bool) -> Result<String, String> {
    let Some(assessment) = assess(case_dir) else {
        return Ok(format!(
            "finalize journal: none ({} does not exist)\n",
            journal_path(case_dir).display()
        ));
    };
    let status = status_value(case_dir, false, false);
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
        out.push_str(&format!(
            "to move it aside (never deleted): {}\n",
            dismiss_hint(case_dir)
        ));
        return Ok(out);
    }
    let closing = match &assessment {
        Assessment::LiveWriter(journal) => {
            return Err(format!(
                "다른 FrameTrace 프로세스(pid {})가 이 케이스를 최종화하고 있어 저널을 옮기지 않았습니다 — 그 최종화가 끝난 뒤 다시 실행하십시오.",
                journal.writer.pid
            ));
        }
        Assessment::Open(journal, _) => Some(journal.clone()),
        _ => None,
    };
    let moved = set_aside(case_dir, "dismissed")?;
    out.push_str(&format!("moved to: {}\n", moved.display()));
    if let Some(journal) = closing {
        match crate::report::record_finalize_resolved(
            case_dir,
            &journal.finalize_id,
            Some(&journal.signoff_sha256),
            "dismissed",
        ) {
            Ok(()) => out.push_str("sign-off lane: finalize-resolved (dismissed) recorded\n"),
            Err(err) => out.push_str(&format!("warning: {err}\n")),
        }
    }
    out.push_str("저널을 옮겼습니다 (지우지 않음). 아무것도 되돌리지 않았습니다 — review/signoff.json과 reports 폴더의 패키지를 확인한 뒤 최종화를 다시 실행하십시오.\n");
    Ok(out)
}
