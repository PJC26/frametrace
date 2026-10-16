//! R12-S2/S3 (P13): the finalize journal `review/finalize.inprogress.json`.
//!
//! A finalize writes it (fsync'd) BEFORE anything durable happens to the
//! sign-off — before the sign-off is chained into the audit lane, rendered
//! into the report and promoted to `review/signoff.json` — and removes it
//! once the finalize completed or its failure path left nothing behind.
//! It holds the sign-off record (text and SHA-256), the bytes of the
//! sign-off it replaces (R12-S3: R11 kept them in memory only), whether
//! those were themselves abandoned (R12-S6), and the `reports/` entries that
//! existed before (to find a package this finalize published, P14).
//!
//! A journal still on disk when the workstation starts or a case is opened
//! means a finalize was interrupted (crash, kill, power loss) or its
//! failure path could not finish. [`recover`] then does what the failure
//! path would have done — every step idempotent, judged from the disk:
//! a package published by that finalize is renamed `…_INCOMPLETE` and
//! marked beside, the sign-off is abandoned in the chain (once), the
//! promoted sign-off is taken back, the pending one removed — and reports
//! a report still rendered from that sign-off as stale. The journal stays
//! until nothing is left unresolved, so a later start retries.
//!
//! Disk work here never runs under the state lock.

use super::*;
use std::collections::BTreeSet;

/// R12-S2: where the journal of a finalize in progress lives.
pub(crate) const FINALIZE_JOURNAL_REL_PATH: &str = "review/finalize.inprogress.json";

const JOURNAL_KIND: &str = "frametrace-finalize-journal";

/// R12-S2: the abandon reason recorded for a finalize found interrupted.
const INTERRUPTED_REASON: &str = "finalize interrupted (workstation stopped before it completed); recovered from review/finalize.inprogress.json";

/// R12-S2: one finalize's journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FinalizeJournal {
    /// Process that wrote it (a live writer is never recovered under).
    pub(crate) pid: u32,
    pub(crate) started_unix: u64,
    pub(crate) examiner: String,
    /// The pending sign-off record, exactly as written.
    pub(crate) signoff_text: String,
    /// What `review/signoff.json` held before (`None`: nothing).
    pub(crate) previous_signoff: Option<Vec<u8>>,
    /// R12-S6: that previous record was itself an abandoned finalize's —
    /// it is never restored.
    pub(crate) previous_abandoned: bool,
    /// Every `reports/` entry name before the finalize ran.
    pub(crate) reports_before: BTreeSet<String>,
    /// [`STATE_IN_PROGRESS`] as the finalize writes it; a recovery that
    /// keeps the journal rewrites it as [`STATE_RECOVERED`] (only the stale
    /// report is left — the next finalize re-renders it) or
    /// [`STATE_INTERRUPTED`] (something still needs a look).
    pub(crate) state: String,
}

/// R12-S2: `state` of a journal a finalize wrote (running, or interrupted
/// and not yet recovered).
pub(crate) const STATE_IN_PROGRESS: &str = "in-progress";
/// R12-S2: recovered; only the stale report remains for a finalize.
pub(crate) const STATE_RECOVERED: &str = "recovered";
/// R12-S2: recovery could not finish (a sign-off not taken back, no abandon
/// record, a package not marked) — "확인 필요".
pub(crate) const STATE_INTERRUPTED: &str = "interrupted";

impl FinalizeJournal {
    pub(crate) fn signoff_sha256(&self) -> String {
        crate::sha256::digest_bytes(self.signoff_text.as_bytes())
    }

    fn to_json(&self) -> String {
        serde_json::json!({
            "schema_version": 1,
            "kind": JOURNAL_KIND,
            "pid": self.pid,
            "started_unix": self.started_unix,
            "examiner": self.examiner,
            "signoff_sha256": self.signoff_sha256(),
            "signoff_text": self.signoff_text,
            "previous_signoff_hex": self.previous_signoff.as_deref().map(hex_encode),
            "previous_signoff_abandoned": self.previous_abandoned,
            "reports_before": self.reports_before.iter().collect::<Vec<_>>(),
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
        let signoff_text = value
            .get("signoff_text")
            .and_then(|v| v.as_str())
            .ok_or("signoff_text 없음")?
            .to_string();
        let journal = Self {
            pid: value.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            started_unix: value
                .get("started_unix")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            examiner: value
                .get("examiner")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            signoff_text,
            previous_signoff: match value.get("previous_signoff_hex") {
                None | Some(serde_json::Value::Null) => None,
                Some(hex) => Some(
                    hex.as_str()
                        .and_then(hex_decode)
                        .ok_or("previous_signoff_hex가 올바르지 않습니다")?,
                ),
            },
            previous_abandoned: value
                .get("previous_signoff_abandoned")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            state: value
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or(STATE_IN_PROGRESS)
                .to_string(),
            reports_before: value
                .get("reports_before")
                .and_then(|v| v.as_array())
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|n| n.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        };
        let recorded = value.get("signoff_sha256").and_then(|v| v.as_str());
        if recorded != Some(journal.signoff_sha256().as_str()) {
            return Err("signoff_sha256이 signoff_text와 다릅니다".to_string());
        }
        Ok(journal)
    }

    /// Writes the journal durably (temp file fsync'd, renamed, directory
    /// synced — `util::write_text_atomic`).
    pub(crate) fn write(&self, case_dir: &Path) -> Result<(), String> {
        let path = journal_path(case_dir);
        crate::util::write_text_atomic(&path, &self.to_json()).map_err(|err| {
            format!(
                "최종화 저널 {}을(를) 쓰지 못했습니다: {err}",
                path.display()
            )
        })
    }
}

pub(crate) fn journal_path(case_dir: &Path) -> PathBuf {
    case_dir.join(FINALIZE_JOURNAL_REL_PATH)
}

/// Reads the case's journal: `Ok(None)` when there is none.
pub(crate) fn read_journal(case_dir: &Path) -> Result<Option<FinalizeJournal>, String> {
    let path = journal_path(case_dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => FinalizeJournal::parse(&text)
            .map(Some)
            .map_err(|err| format!("{}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

/// Removes the journal (retried like the sign-off renames: an indexer or
/// antivirus handle on the fresh file must not leave it behind).
pub(crate) fn remove_journal(case_dir: &Path) -> Result<(), String> {
    let path = journal_path(case_dir);
    crate::package::retry_within_notify(
        crate::package::rename_retry_budget(),
        &mut || match std::fs::remove_file(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
        &crate::package::is_final_rename_error,
        &mut |_| {},
    )
    .map_err(|err| format!("{}: {err}", path.display()))
}

/// Every entry name of `reports/` now (empty when it does not exist).
pub(crate) fn reports_entries(case_dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(case_dir.join("reports"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// P14: `reports/package_*` folders that did not exist in `before` and hold
/// a published package (a plain folder with `manifest.sha256` — a bare
/// reservation holds only its marker) not already marked incomplete.
pub(crate) fn published_since(case_dir: &Path, before: &BTreeSet<String>) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(case_dir.join("reports")) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.starts_with("package_")
                && !before.contains(&name)
                && !crate::package::is_incomplete_package_name(&name)
        })
        .filter(super::media::is_plain_dir_entry)
        .map(|entry| entry.path())
        .filter(|path| path.join("manifest.sha256").is_file())
        .filter(|path| crate::package::finalize_incomplete_reason(path).is_none())
        .collect();
    found.sort();
    found
}

/// R12-S5: complete packages a package-case kept under their work-folder
/// name (`.package_<ts>.ft<pid>.part`, marked preserved) because the
/// publish rename failed — new since `before`.
fn kept_parts_since(case_dir: &Path, before: &BTreeSet<String>) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(case_dir.join("reports")) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            !before.contains(&name) && crate::join::is_pid_part_name(&name, None)
        })
        .filter(super::media::is_plain_dir_entry)
        .map(|entry| entry.path())
        .filter(|path| crate::join::part_is_preserved(path))
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
/// marker beside it. Returns the examiner-facing sentence and the folder
/// as it is now. Never deletes anything.
pub(crate) fn mark_package_incomplete(package: &Path, why: &str) -> (String, PathBuf) {
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
            crate::package::rename_retry_budget(),
            &mut || std::fs::rename(package, &target),
            &crate::package::is_final_rename_error,
            &mut |_| {},
        )
        .map(|()| target)
        .map_err(|err| err.to_string());
        break;
    }
    let (now, rename_note) = match renamed {
        Ok(target) => {
            crate::util::sync_parent_directory(&target);
            let note = format!("폴더 이름을 {}(으)로 바꿨고", target.display());
            (target, note)
        }
        Err(err) => (
            package.to_path_buf(),
            format!("폴더 이름은 바꾸지 못했고({err})"),
        ),
    };
    let marker = crate::package::finalize_incomplete_marker(&now);
    let marker_note =
        match crate::util::write_text_atomic(&marker, &incomplete_marker_text(&now, why)) {
            Ok(()) => format!("옆에 표식 {}을(를) 남겼습니다", marker.display()),
            Err(err) => format!("표식 {}도 남기지 못했습니다({err})", marker.display()),
        };
    (
        format!(
            "이번 최종화의 패키지는 이미 {}에 만들어졌지만 최종화는 끝나지 않았습니다 — {rename_note} {marker_note}. 이 패키지를 최종 결과로 쓰지 마십시오.",
            package.display()
        ),
        now,
    )
}

/// R12-S5: a complete package kept under its work-folder name gets the
/// finalize-incomplete marker beside it under that name AND under the name
/// it was meant to have (the package-case note said to rename it, which
/// made it verify as a final package), and the preserved-part note is
/// rewritten to say it is not a final result. Returns the sentence.
fn mark_kept_part_incomplete(part: &Path, why: &str) -> String {
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
        format!(
            "package-case가 남긴 완성된 패키지 {}는 끝나지 않은 최종화의 것입니다 — 옆에 최종화 미완료 표식을 남겼습니다. 이름을 바꿔 쓰지 말고 최종화를 다시 실행하십시오.",
            part.display()
        )
    } else {
        format!(
            "package-case가 남긴 완성된 패키지 {}는 끝나지 않은 최종화의 것입니다 — 최종화 미완료 표식을 남기지 못했습니다({}). 이 폴더를 최종 결과로 쓰지 말고 최종화를 다시 실행하십시오.",
            part.display(),
            failed.join(", ")
        )
    }
}

/// R12-S1/S5 (P14): marks everything a finalize's package-case published
/// or kept since `before` (plus the folder its `output:` line names, if
/// any) as incomplete. Returns one sentence per folder and the incomplete
/// package folders as they are now.
pub(crate) fn mark_unfinished_packages(
    case_dir: &Path,
    before: &BTreeSet<String>,
    step_text: &str,
    why: &str,
) -> (Vec<String>, Vec<PathBuf>) {
    let mut published = published_since(case_dir, before);
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
    let mut notes = Vec::new();
    let mut dirs = Vec::new();
    for package in published {
        let (note, now) = mark_package_incomplete(&package, why);
        notes.push(note);
        dirs.push(now);
    }
    for part in kept_parts_since(case_dir, before) {
        notes.push(mark_kept_part_incomplete(&part, why));
        dirs.push(part);
    }
    (notes, dirs)
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
    /// Something other than the stale report is unresolved (a sign-off not
    /// taken back, no abandon record, a package not marked, an unreadable
    /// journal, a live writer): a new finalize must not overwrite it.
    pub(crate) blocking: bool,
}

/// R12-S2: recovers the interrupted finalize `case_dir`'s journal records
/// (see the module docs). `None` when there is no journal. The caller makes
/// sure no finalize of this workstation is running on the case (it holds
/// `busy`, or the server is not serving yet); a journal whose writer is
/// another live process is left alone. Restoring and the other cleanup
/// steps are never cancellable (R12-S4).
pub(crate) fn recover(case_dir: &Path, state: &SharedState) -> Option<Recovery> {
    let journal = match read_journal(case_dir) {
        Ok(None) => return None,
        Ok(Some(journal)) => journal,
        Err(err) => {
            return Some(Recovery {
                detail: format!(
                    "이전 최종화가 중단된 흔적(최종화 저널)이 있지만 읽지 못했습니다: {err} — 이 파일을 지우지 말고, 검토자 기록(review/signoff.json)과 reports 폴더의 마지막 패키지를 최종 결과로 쓰기 전에 최종화를 다시 실행하십시오."
                ),
                report_stale: false,
                blocking: true,
            });
        }
    };
    if journal.pid != std::process::id() {
        let born = std::fs::symlink_metadata(journal_path(case_dir))
            .ok()
            .and_then(|meta| meta.modified().ok());
        if crate::util::writer_may_be_alive(crate::util::probe_process(journal.pid), born) {
            return Some(Recovery {
                detail: format!(
                    "다른 FrameTrace 프로세스(pid {})가 이 케이스를 최종화하고 있는 것으로 보여 최종화 저널을 그대로 두었습니다 — 그 최종화가 끝난 뒤 케이스를 다시 여십시오.",
                    journal.pid
                ),
                report_stale: false,
                blocking: true,
            });
        }
    }
    Some(resolve(case_dir, &journal, state, INTERRUPTED_REASON))
}

/// R12-S2: [`recover`] for the case the workstation starts bound to (the
/// restored session or `--case`), into the job state. Called before the
/// server serves (no job can run yet).
pub(crate) fn recover_bound_case(state: &SharedState) {
    let bound = state_lock(state).case_dir.clone();
    if let Some(case_dir) = bound
        && journal_path(&case_dir).exists()
        && let Some(recovery) = recover(&case_dir, state)
    {
        super::jobs::apply_recovery(state, recovery);
    }
}

/// The idempotent cleanup of an unfinished finalize after a restart — each
/// step is judged from the disk, so a step an earlier attempt (the failure
/// path, an earlier recovery) already did is a no-op.
fn resolve(
    case_dir: &Path,
    journal: &FinalizeJournal,
    state: &SharedState,
    reason: &str,
) -> Recovery {
    let sha = journal.signoff_sha256();
    let mut done: Vec<String> = Vec::new();
    let mut open: Vec<String> = Vec::new();
    // 1. P14: a package that finalize published is never left plain.
    let (notes, _) = mark_unfinished_packages(case_dir, &journal.reports_before, "", reason);
    done.extend(notes);
    let unmarked = published_since(case_dir, &journal.reports_before);
    if !unmarked.is_empty() {
        open.push(format!(
            "패키지 {}에 최종화 미완료 표시를 하지 못했습니다 — 최종 결과로 쓰지 마십시오.",
            unmarked
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // 2. The chained sign-off entry is abandoned — once.
    match crate::report::signoff_lane_state(case_dir, &journal.signoff_text) {
        Ok(lane) if lane.recorded && !lane.abandoned => {
            match crate::report::record_signoff_abandoned(case_dir, &journal.signoff_text, reason) {
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
    // 3. The pending record goes; the promoted one is taken back.
    let pending = case_dir.join(super::jobs::SIGNOFF_PENDING_REL_PATH);
    if std::fs::read(&pending).is_ok_and(|bytes| crate::sha256::digest_bytes(&bytes) == sha) {
        let _ = std::fs::remove_file(&pending);
    }
    let target = case_dir.join(crate::report::SIGNOFF_REL_PATH);
    let promoted =
        std::fs::read(&target).is_ok_and(|bytes| crate::sha256::digest_bytes(&bytes) == sha);
    if promoted {
        match restore_previous(
            &target,
            journal.previous_signoff.as_deref(),
            journal.previous_abandoned,
            state,
        ) {
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
    let report_stale = std::fs::read_to_string(case_dir.join("reports/case-report.html"))
        .ok()
        .is_some_and(|html| crate::report::report_signoff_sha256(&html) == Some(sha.as_str()));
    let blocking = !open.is_empty();
    if report_stale {
        open.push("reports/case-report.html이 끝나지 않은 최종화의 검토자 기록으로 만들어진 채 남아 있습니다 — 최종화를 다시 실행하면 새로 만들어집니다 (그 전에는 최종 결과로 쓰지 마십시오).".to_string());
    }
    if open.is_empty() {
        if let Err(err) = remove_journal(case_dir) {
            open.push(format!("최종화 저널을 지우지 못했습니다: {err}"));
        }
    } else {
        // Kept for a later start; its state says how far recovery got.
        let mut kept = journal.clone();
        kept.state = if blocking {
            STATE_INTERRUPTED
        } else {
            STATE_RECOVERED
        }
        .to_string();
        if let Err(err) = kept.write(case_dir) {
            open.push(err);
        }
    }
    let mut detail = format!(
        "이전 최종화(검토자 입력값: {}, 서명 SHA-256 {})가 끝나지 않은 채 중단된 것을 찾아 정리했습니다.",
        journal.examiner,
        &sha[..12.min(sha.len())]
    );
    for line in &done {
        detail.push(' ');
        detail.push_str(line);
    }
    if !open.is_empty() {
        detail.push_str(" 확인 필요: ");
        detail.push_str(&open.join(" "));
    }
    state_lock(state).logs.push(format!("경고: {detail}"));
    Recovery {
        detail,
        report_stale,
        blocking,
    }
}

/// R11-S3 / R12-S6/S7: takes a promoted sign-off back — the previous record
/// written back durably (temp file fsync'd, renamed over the promoted one)
/// or the promoted one removed when there was none. R12-S6: a previous
/// record that was itself an abandoned finalize's is NOT restored (the
/// promoted one is removed) and `Ok(Some(note))` says so — R11 restored it
/// like a confirmed record and the warning about it was lost.
pub(crate) fn restore_previous(
    target: &Path,
    previous: Option<&[u8]>,
    previous_abandoned: bool,
    state: &SharedState,
) -> Result<Option<String>, String> {
    let remove = |state: &SharedState| {
        super::jobs::retry_logged(state, "검토자 기록 되돌리기", &mut || {
            match std::fs::remove_file(target) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        })
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
            let temp = target.with_file_name("signoff.restore.json");
            write_synced(&temp, bytes).map_err(|err| format!("{}: {err}", temp.display()))?;
            super::jobs::retry_logged(state, "검토자 기록 되돌리기", &mut || {
                std::fs::rename(&temp, target)
            })
            .map_err(|err| {
                let _ = std::fs::remove_file(&temp);
                format!("{}: {err}", target.display())
            })?;
            crate::util::sync_parent_directory(target);
            Ok(None)
        }
        None => remove(state).map(|()| None),
    }
}

/// R12-S2: the journal is gone once nothing it records is unresolved —
/// after a make-report the workstation ran succeeded (the mark-refresh
/// path), the report no longer names that sign-off; if the promoted
/// sign-off is not in place either, the journal is removed.
pub(crate) fn clear_if_resolved(case_dir: &Path) {
    // The caller checked that no finalize of this workstation runs.
    let Ok(Some(journal)) = read_journal(case_dir) else {
        return;
    };
    let sha = journal.signoff_sha256();
    let promoted = std::fs::read(case_dir.join(crate::report::SIGNOFF_REL_PATH))
        .is_ok_and(|bytes| crate::sha256::digest_bytes(&bytes) == sha);
    let stale = std::fs::read_to_string(case_dir.join("reports/case-report.html"))
        .ok()
        .is_some_and(|html| crate::report::report_signoff_sha256(&html) == Some(sha.as_str()));
    let lane_open = crate::report::signoff_lane_state(case_dir, &journal.signoff_text)
        .map(|lane| lane.recorded && !lane.abandoned)
        .unwrap_or(true);
    if !promoted
        && !stale
        && !lane_open
        && published_since(case_dir, &journal.reports_before).is_empty()
    {
        let _ = remove_journal(case_dir);
    }
}

/// `/api/status` `finalize_journal`: the journal on disk, summarized
/// (`null` when there is none).
/// `null` too while this workstation's own finalize runs (`active`: the
/// journal is then the live one, not a sign of an interruption). `state`:
/// [`STATE_IN_PROGRESS`] (not recovered yet — e.g. another live process
/// writes it), [`STATE_RECOVERED`], [`STATE_INTERRUPTED`] or `unreadable`.
pub(crate) fn status_value(case_dir: &Path, active: bool) -> serde_json::Value {
    if active {
        return serde_json::Value::Null;
    }
    match read_journal(case_dir) {
        Ok(None) => serde_json::Value::Null,
        Ok(Some(journal)) => serde_json::json!({
            "path": FINALIZE_JOURNAL_REL_PATH,
            "state": journal.state,
            "examiner": journal.examiner,
            "started_unix": journal.started_unix,
            "signoff_sha256": journal.signoff_sha256(),
            "error": null,
        }),
        Err(err) => serde_json::json!({
            "path": FINALIZE_JOURNAL_REL_PATH,
            "state": "unreadable",
            "examiner": null,
            "started_unix": null,
            "signoff_sha256": null,
            "error": err,
        }),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}
