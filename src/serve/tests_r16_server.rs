//! Round-16 track S (docs/PLAN_R16_2026-10-18.md): the local record
//! advances only by creating sibling files (P24), a maybe-live writer is
//! never rolled back automatically (P25), and S3–S8.

use super::*;

pub(super) fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r16s-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The real CLI binary next to the test executable (`None`: skipped).
pub(super) fn cli_bin() -> Option<PathBuf> {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let bin = dir.join(if cfg!(windows) {
        "frametrace.exe"
    } else {
        "frametrace"
    });
    if bin.is_file() {
        Some(bin)
    } else {
        eprintln!(
            "SKIPPED: the frametrace binary is not built in {} (run `cargo test` or `cargo build` first)",
            dir.display()
        );
        None
    }
}

fn run_cli(bin: &Path, args: &[&str]) {
    let output = std::process::Command::new(bin).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "frametrace {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A case with one indexed source (real CLI). `None` without the binary.
pub(super) fn finalize_case(tag: &str) -> Option<(PathBuf, PathBuf)> {
    let bin = cli_bin()?;
    let root = scratch(tag);
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.mp4"), b"fixture-video-bytes-r16").unwrap();
    let case = root.join("case");
    run_cli(&bin, &["init-case", case.to_str().unwrap()]);
    run_cli(
        &bin,
        &[
            "scan-folder",
            case.to_str().unwrap(),
            src.to_str().unwrap(),
            "--hash",
            "--no-ffprobe",
        ],
    );
    Some((root, case))
}

pub(super) fn bound_state(case: &Path) -> SharedState {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.to_path_buf());
    state
}

pub(super) fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("invalid JSON {err}: {text}"))
}

pub(super) fn package_entries(case: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(case.join("reports"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|name| name.starts_with("package_"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

pub(super) fn lane_path(case: &Path) -> PathBuf {
    case.join(crate::report::SIGNOFF_LOG_REL_PATH)
}

pub(super) fn lane_text(case: &Path) -> String {
    std::fs::read_to_string(lane_path(case)).unwrap_or_default()
}

pub(super) fn finalize_ok(bin: &Path, case: &Path, examiner: &str) -> Vec<u8> {
    let state = bound_state(case);
    let response = parse(&finalize_with_exe(bin, case, examiner, &state));
    assert_eq!(response["ok"], true, "{response}");
    std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap()
}

pub(super) fn last_begin(case: &Path) -> serde_json::Value {
    lane_text(case)
        .lines()
        .rev()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|value| value["event"] == crate::report::FINALIZE_BEGIN_EVENT)
        .expect("a finalize-begin entry")
}

/// The journal an attacker rebuilds from the plaintext begin entry.
pub(super) fn journal_from_begin(
    begin: &serde_json::Value,
) -> super::finalize_journal::FinalizeJournal {
    super::finalize_journal::FinalizeJournal {
        finalize_id: begin["finalize_id"].as_str().unwrap().into(),
        host_id: begin["host_id"].as_str().unwrap().into(),
        writer: crate::util::WriterIdentity::default(),
        writer_image_sha256: None,
        started_unix: begin["started_unix"].as_u64().unwrap(),
        examiner: "공격자".into(),
        signoff_sha256: begin["signoff_record_sha256"].as_str().unwrap().into(),
        previous_signoff_sha256: begin["previous_signoff_sha256"]
            .as_str()
            .map(str::to_string),
        package_name: begin["package_name"].as_str().unwrap().into(),
        state: super::finalize_journal::STATE_IN_PROGRESS.into(),
    }
}

/// Drops the lane's last line (the tail of an append-only chain can be cut).
pub(super) fn drop_last_lane_line(case: &Path) -> String {
    let text = lane_text(case);
    let mut lines: Vec<&str> = text.lines().collect();
    let last = lines.pop().unwrap().to_string();
    let mut kept = lines.join("\n");
    kept.push('\n');
    std::fs::write(lane_path(case), kept).unwrap();
    last
}

pub(super) fn keyless<R>(f: impl FnOnce() -> R) -> R {
    crate::audit_key::with_test_keys(&[], f)
}

pub(super) fn signoff_sha(case: &Path) -> String {
    crate::sha256::digest_bytes(&std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap())
}

pub(super) fn lane_events_for(case: &Path, id: &str) -> Vec<String> {
    lane_text(case)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| value["finalize_id"] == id)
        .map(|value| value["event"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// `<32 hex>.json` — the name of a finalize's first (`open`) local record.
#[cfg(windows)]
fn is_open_record_name(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(|stem| {
        stem.len() == 32 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// R16-S1 (P24, real CLI, real locked handle): the R15 reviewer's PoC.
/// While a good finalize runs, another program (an indexer, antivirus or
/// backup) opens its local record file with a handle that does NOT share
/// delete. 82a6cb1 advanced the record by rewriting that file (MoveFileEx
/// over it, then `fs::rename`) — both fail against such a handle, as does
/// the retried removal, so the record stayed `open`. Then the lane's
/// `finalize-complete` line is cut, the journal rebuilt from the plaintext
/// begin entry and the older sign-off copied into
/// `finalize.previous-signoff.bin`: 82a6cb1's recovery abandoned the good
/// sign-off, restored the older bytes and renamed the good package
/// `_INCOMPLETE`. (Run on 82a6cb1 first: it failed.) Now the state
/// advances by creating `<id>.packaged.json` / `<id>.completed.json`
/// beside the locked file, so the record reads `completed` and the
/// recovery only writes the cut complete entry again.
#[cfg(windows)]
#[test]
fn r16_s1_locked_local_record_still_advances() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    let Some((root, case)) = finalize_case("s1-lock") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let older = finalize_ok(&bin, &case, "이전검토자");
        let dir = super::finalize_journal::local_open_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        // The "indexer": as soon as this case's journal names the running
        // finalize, opens that finalize's open record (other tests of this
        // process finalize in parallel) sharing read and write but NOT
        // delete, and holds it.
        let stop = Arc::new(AtomicBool::new(false));
        let locker = {
            let stop = Arc::clone(&stop);
            let dir = dir.clone();
            let case = case.clone();
            std::thread::spawn(move || {
                let mut held: Option<(String, std::fs::File)> = None;
                while !stop.load(Ordering::SeqCst) {
                    if held.is_none()
                        && let Ok(Some(journal)) = super::finalize_journal::read_journal(&case)
                    {
                        let name = format!("{}.json", journal.finalize_id);
                        assert!(is_open_record_name(&name), "{name}");
                        if let Ok(file) = std::fs::OpenOptions::new()
                            .read(true)
                            .share_mode(0x1 | 0x2) // FILE_SHARE_READ | FILE_SHARE_WRITE
                            .open(dir.join(&name))
                        {
                            held = Some((name, file));
                        }
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                held.map(|(name, _file)| name)
            })
        };
        crate::package::RENAME_BUDGET_OVERRIDE
            .with(|cell| cell.set(Some(Duration::from_millis(800))));
        let good = finalize_ok(&bin, &case, "홍길동");
        crate::package::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(None));
        let begin = last_begin(&case);
        let journal = journal_from_begin(&begin);
        // What the locked handle left: read while it is still held.
        let left = super::finalize_journal::read_local_record(&journal.finalize_id)
            .unwrap()
            .expect("the locked record could not be removed");
        stop.store(true, Ordering::SeqCst);
        let locked = locker.join().unwrap();
        assert_eq!(
            locked.as_deref(),
            Some(format!("{}.json", journal.finalize_id).as_str()),
            "the locker held the finalize's open record"
        );
        assert_ne!(
            left.state,
            super::finalize_journal::LOCAL_OPEN,
            "a locked record file kept the good finalize `open`"
        );
        let packages = package_entries(&case);
        let cut = drop_last_lane_line(&case);
        assert!(
            cut.contains(crate::report::FINALIZE_COMPLETE_EVENT),
            "{cut}"
        );
        journal.write(&case).unwrap();
        std::fs::write(
            case.join(super::finalize_journal::PREVIOUS_SIGNOFF_REL_PATH),
            &older,
        )
        .unwrap();
        let state = bound_state(&case);
        super::finalize_journal::recover_bound_case(&state);
        assert_eq!(
            signoff_sha(&case),
            crate::sha256::digest_bytes(&good),
            "the good sign-off was replaced by the older bytes"
        );
        assert_eq!(
            package_entries(&case),
            packages,
            "the good package was renamed _INCOMPLETE"
        );
        assert!(
            !lane_text(&case).contains(crate::report::SIGNOFF_ABANDONED_EVENT),
            "the good sign-off was abandoned"
        );
        assert_eq!(
            lane_events_for(&case, &journal.finalize_id),
            vec![
                crate::report::FINALIZE_BEGIN_EVENT.to_string(),
                crate::report::FINALIZE_COMPLETE_EVENT.to_string()
            ]
        );
        assert!(!super::finalize_journal::journal_path(&case).exists());
        assert!(
            super::finalize_journal::read_local_record(&journal.finalize_id)
                .unwrap()
                .is_none()
        );
    });
    let _ = std::fs::remove_dir_all(root);
}

/// A finalize (김철수) that "dies" at `stop`; returns its journal.
pub(super) fn interrupted_finalize(
    bin: &Path,
    case: &Path,
    stop: super::jobs::FinalizeStop,
) -> super::finalize_journal::FinalizeJournal {
    let state = bound_state(case);
    super::jobs::STOP_FINALIZE_AT.with(|cell| cell.set(Some(stop)));
    let response = finalize_with_exe(bin, case, "김철수", &state);
    super::jobs::STOP_FINALIZE_AT.with(|cell| cell.set(None));
    assert!(response.contains("test: stopped"), "{response}");
    super::finalize_journal::read_journal(case)
        .unwrap()
        .expect("the journal is on disk")
}

pub(super) fn local_state(id: &str) -> Option<String> {
    super::finalize_journal::read_local_record(id)
        .unwrap()
        .map(|record| record.state)
}

/// A live FrameTrace process in this executable's folder with this
/// executable's bytes (the test binary running the ignored R15 helper).
pub(super) fn spawn_live_writer() -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "serve::tests_r14_server::r15_live_writer_helper",
            "--ignored",
            "--test-threads=1",
        ])
        .env("FT_R15_LIVE_WRITER", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap()
}

/// The identity a journal records for `pid` once its start time and image
/// name can be read.
pub(super) fn live_writer_of(pid: u32) -> crate::util::WriterIdentity {
    let read = || {
        let started_ms = match crate::util::probe_process(pid) {
            crate::util::ProcessProbe::Running {
                started: Some(started),
            } => started
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_millis() as u64),
            _ => None,
        };
        crate::util::WriterIdentity {
            pid,
            started_ms,
            image: crate::util::process_image_name(pid),
        }
    };
    let mut writer = read();
    for _ in 0..50 {
        if writer.started_ms.is_some() && writer.image.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        writer = read();
    }
    writer
}

/// An interrupted finalize (stopped after package-case, record `open`)
/// whose journal names a live FrameTrace process as its writer, with
/// `image_sha256` as the writer's executable SHA-256.
pub(super) fn journal_of_live_writer(
    bin: &Path,
    case: &Path,
    child: &std::process::Child,
    image_sha256: Option<String>,
) -> super::finalize_journal::FinalizeJournal {
    let mut journal = interrupted_finalize(bin, case, super::jobs::FinalizeStop::Package);
    journal.writer = live_writer_of(child.id());
    journal.writer_image_sha256 = image_sha256;
    journal.write(case).unwrap();
    journal
}

/// R16-S2 (P25): a live process matching the journal's pid, start time and
/// FrameTrace image name, but whose executable is in another folder or has
/// another SHA-256 (a second FrameTrace install running the finalize), is
/// a "maybe-live" writer. 82a6cb1 judged it not alive, so `--dismiss` (or
/// opening the case with another build) cleaned the RUNNING finalize up —
/// rolled its sign-off back and renamed its package `_INCOMPLETE`. Now:
/// `in-progress` / `unverified-writer`, nothing is recovered, `--dismiss`
/// is refused, `--dismiss --force` only moves the journal. (Run on
/// 82a6cb1 first: it failed — `interrupted`.)
#[test]
fn r16_s2_unverified_live_writer_is_never_rolled_back() {
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let Some((root, case)) = finalize_case("s2-unverified") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let mut child = spawn_live_writer();
    let result = std::panic::catch_unwind(|| {
        keyless(|| {
            finalize_ok(&bin, &case, "홍길동");
            let journal = journal_of_live_writer(&bin, &case, &child, Some("0".repeat(64)));
            let signoff_before = signoff_sha(&case);
            let packages = package_entries(&case);
            let state = bound_state(&case);
            let shown = parse(&api_status(&state))["finalize_journal"].clone();
            assert_eq!(shown["state"], "in-progress", "{shown}");
            assert_eq!(shown["in_progress_code"], "unverified-writer", "{shown}");
            assert_eq!(shown["blocks_finalize"], true, "{shown}");
            assert!(shown["dismiss_command"].is_null(), "{shown}");
            assert!(
                super::finalize_journal::cli_command(&case, true, false).is_err(),
                "--dismiss without --force"
            );
            super::finalize_journal::recover_bound_case(&state);
            assert_eq!(signoff_sha(&case), signoff_before, "rolled back");
            assert_eq!(package_entries(&case), packages, "package renamed");
            assert!(super::finalize_journal::journal_path(&case).is_file());
            super::finalize_journal::cli_command(&case, true, true).unwrap();
            assert!(!super::finalize_journal::journal_path(&case).exists());
            assert_eq!(signoff_sha(&case), signoff_before, "rolled back by --force");
            assert_eq!(package_entries(&case), packages);
            // S8: the writer may still finish — nothing is recorded for it.
            assert_eq!(
                lane_events_for(&case, &journal.finalize_id),
                vec![crate::report::FINALIZE_BEGIN_EVENT.to_string()]
            );
            assert_eq!(
                local_state(&journal.finalize_id).as_deref(),
                Some(super::finalize_journal::LOCAL_OPEN)
            );
            let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
        })
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(root);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// R16-S8: `--dismiss --force` of a VERIFIED live writer's journal only
/// moves the journal. 82a6cb1 also wrote `finalize-resolved (dismissed)`
/// and removed the local record, and the live finalize then wrote
/// `finalize-complete` — begin → resolved → complete in one lane. (Run on
/// 82a6cb1 first: it failed.)
#[test]
fn r16_s8_force_dismiss_of_a_live_writer_records_nothing() {
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let Some((root, case)) = finalize_case("s8-force") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let mut child = spawn_live_writer();
    let result = std::panic::catch_unwind(|| {
        keyless(|| {
            finalize_ok(&bin, &case, "홍길동");
            let journal = journal_of_live_writer(
                &bin,
                &case,
                &child,
                super::finalize_journal::own_exe_sha256(),
            );
            let shown = parse(&api_status(&bound_state(&case)))["finalize_journal"].clone();
            assert_eq!(shown["in_progress_code"], "live-writer", "{shown}");
            super::finalize_journal::cli_command(&case, true, true).unwrap();
            assert!(!super::finalize_journal::journal_path(&case).exists());
            assert_eq!(
                lane_events_for(&case, &journal.finalize_id),
                vec![crate::report::FINALIZE_BEGIN_EVENT.to_string()]
            );
            assert!(local_state(&journal.finalize_id).is_some());
            let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
        })
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(root);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// R16-S3: the complete entry a recovery writes again carries the manifest
/// SHA-256 THIS PC recorded, never the one on disk now. A finalize whose
/// complete entry failed (`completed-unrecorded`) and whose package
/// manifest was then changed: 82a6cb1 wrote the complete entry with the
/// changed manifest's SHA-256. Now: `unpaired` / `package-changed`, no
/// entry. (Run on 82a6cb1 first: it failed.)
#[test]
fn r16_s3_rewritten_complete_uses_the_recorded_manifest() {
    let Some((root, case)) = finalize_case("s3-manifest") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        super::jobs::FAIL_FINALIZE_COMPLETE.with(|cell| cell.set(true));
        finalize_ok(&bin, &case, "홍길동");
        super::jobs::FAIL_FINALIZE_COMPLETE.with(|cell| cell.set(false));
        let journal = super::finalize_journal::read_journal(&case)
            .unwrap()
            .expect("kept as completed-unrecorded");
        let manifest = case
            .join("reports")
            .join(&journal.package_name)
            .join("manifest.sha256");
        let mut text = std::fs::read_to_string(&manifest).unwrap();
        text.push_str(&format!("{}  extra.txt\n", "e".repeat(64)));
        std::fs::write(&manifest, text).unwrap();
        let state = bound_state(&case);
        super::finalize_journal::recover_bound_case(&state);
        assert_eq!(
            lane_events_for(&case, &journal.finalize_id),
            vec![crate::report::FINALIZE_BEGIN_EVENT.to_string()],
            "a complete entry with the changed manifest was written"
        );
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["state"], "unpaired", "{shown}");
        assert_eq!(shown["unpaired_code"], "package-changed", "{shown}");
        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R16-S5: the status poll judges `packaged` by the manifest's bytes, not
/// by its (size, mtime) — a same-size rewrite with the old mtime restored
/// kept 82a6cb1 reporting `completed-unrecorded` (blocks_finalize false).
/// (Run on 82a6cb1 first: it failed.)
#[test]
fn r16_s5_status_rehashes_the_manifest() {
    let Some((root, case)) = finalize_case("s5-rehash") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        finalize_ok(&bin, &case, "홍길동");
        let journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Packaged);
        let state = bound_state(&case);
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["state"], "completed-unrecorded", "{shown}");
        let manifest = case
            .join("reports")
            .join(&journal.package_name)
            .join("manifest.sha256");
        let modified = std::fs::metadata(&manifest).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&manifest).unwrap();
        // Same length: one hash digit changed.
        bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
        std::fs::write(&manifest, &bytes).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&manifest)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["state"], "unpaired", "{shown}");
        assert_eq!(shown["unpaired_code"], "package-changed", "{shown}");
        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R16-S7 (P23 extended): a `packaged` / `completed` record older than 30
/// days whose case cannot be reached is kept (82a6cb1 swept every record
/// past `open` by age alone). (Run on 82a6cb1 first: it failed.)
#[test]
fn r16_s7_sweep_keeps_records_past_open_of_unreachable_cases() {
    let root = scratch("s7-sweep");
    let old = crate::util::now_unix().unwrap() - 40 * 24 * 3600;
    let away = root.join("not-plugged-in");
    let records: Vec<super::finalize_journal::LocalRecord> = [
        ('c', super::finalize_journal::LOCAL_PACKAGED),
        ('d', super::finalize_journal::LOCAL_COMPLETED),
    ]
    .into_iter()
    .map(|(digit, state)| super::finalize_journal::LocalRecord {
        finalize_id: digit.to_string().repeat(32),
        case_dir: away.to_string_lossy().to_string(),
        signoff_sha256: "1".repeat(64),
        previous_signoff_sha256: None,
        package_name: "package_1".into(),
        host_id: crate::util::host_id(),
        started_unix: old,
        state: state.into(),
        updated_unix: old,
        manifest_sha256: Some("2".repeat(64)),
    })
    .collect();
    for record in &records {
        record.write().unwrap();
    }
    super::finalize_journal::sweep_local_records();
    let kept: Vec<bool> = records
        .iter()
        .map(|record| local_state(&record.finalize_id).is_some())
        .collect();
    for record in &records {
        let _ = super::finalize_journal::remove_local_record(&record.finalize_id);
    }
    assert_eq!(kept, vec![true, true]);
    let _ = std::fs::remove_dir_all(root);
}

/// R16-S7: a `packaged` record is judged by the manifest's SHA-256
/// computed streaming — a manifest over 16 MiB made 82a6cb1 report
/// `package-changed` for every such finalize. (Run on 82a6cb1 first: it
/// failed.)
#[test]
fn r16_s7_large_manifest_is_hashed_streaming() {
    let Some((root, case)) = finalize_case("s7-large") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        finalize_ok(&bin, &case, "홍길동");
        let journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Package);
        let manifest = case
            .join("reports")
            .join(&journal.package_name)
            .join("manifest.sha256");
        let mut bytes = std::fs::read(&manifest).unwrap();
        let line = format!("{}  padding.bin\n", "f".repeat(64));
        while bytes.len() <= 17 * 1024 * 1024 {
            bytes.extend_from_slice(line.as_bytes());
        }
        std::fs::write(&manifest, &bytes).unwrap();
        super::finalize_journal::LocalRecord {
            manifest_sha256: Some(crate::sha256::digest_bytes(&bytes)),
            ..journal.local_record(&case, super::finalize_journal::LOCAL_PACKAGED)
        }
        .write()
        .unwrap();
        let shown = parse(&api_status(&bound_state(&case)))["finalize_journal"].clone();
        assert_eq!(shown["state"], "completed-unrecorded", "{shown}");
        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R16 (P24, portable): when `completed` cannot be created after the
/// complete entry, the journal is KEPT as `completed-unrecorded` and the
/// record stays `packaged`; the R15-S1 attack (cut the complete line,
/// rebuild the journal, older bytes beside it) then finds a finished
/// finalize, not an open one. A recovery that still cannot create
/// `completed` keeps the journal and blocks; once it can, everything goes.
#[test]
fn r16_p24_uncreatable_completed_keeps_the_journal() {
    let Some((root, case)) = finalize_case("p24-keep") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let older = finalize_ok(&bin, &case, "이전검토자");
        super::finalize_journal::FAIL_LOCAL_STATE
            .with(|cell| cell.set(Some(super::finalize_journal::LOCAL_COMPLETED)));
        let good = finalize_ok(&bin, &case, "홍길동");
        let journal = super::finalize_journal::read_journal(&case)
            .unwrap()
            .expect("kept");
        assert_eq!(
            journal.state,
            super::finalize_journal::STATE_COMPLETED_UNRECORDED
        );
        assert_eq!(
            local_state(&journal.finalize_id).as_deref(),
            Some(super::finalize_journal::LOCAL_PACKAGED)
        );
        // The lane closed it; the recovery cannot record that here yet.
        let state = bound_state(&case);
        super::finalize_journal::recover_bound_case(&state);
        assert!(super::finalize_journal::journal_path(&case).is_file());
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["blocks_finalize"], true, "{shown}");
        // The attack while it is so.
        let packages = package_entries(&case);
        let cut = drop_last_lane_line(&case);
        assert!(
            cut.contains(crate::report::FINALIZE_COMPLETE_EVENT),
            "{cut}"
        );
        journal_from_begin(&last_begin(&case)).write(&case).unwrap();
        std::fs::write(
            case.join(super::finalize_journal::PREVIOUS_SIGNOFF_REL_PATH),
            &older,
        )
        .unwrap();
        super::finalize_journal::recover_bound_case(&state);
        assert_eq!(signoff_sha(&case), crate::sha256::digest_bytes(&good));
        assert_eq!(package_entries(&case), packages);
        assert!(!lane_text(&case).contains(crate::report::SIGNOFF_ABANDONED_EVENT));
        assert!(super::finalize_journal::journal_path(&case).is_file());
        super::finalize_journal::FAIL_LOCAL_STATE.with(|cell| cell.set(None));
        super::finalize_journal::recover_bound_case(&state);
        assert!(!super::finalize_journal::journal_path(&case).exists());
        assert!(local_state(&journal.finalize_id).is_none());
        assert_eq!(
            lane_events_for(&case, &journal.finalize_id),
            vec![
                crate::report::FINALIZE_BEGIN_EVENT.to_string(),
                crate::report::FINALIZE_COMPLETE_EVENT.to_string()
            ]
        );
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R16 (P24): the record is the highest state file present; a removal
/// goes lowest first and stops at the first file it cannot remove, so the
/// state never drops (a directory in place of `<id>.json` stands for a
/// file another program holds — portable).
#[test]
fn r16_p24_removal_never_lowers_the_state() {
    let root = scratch("p24-order");
    let case = root.join("case");
    std::fs::create_dir_all(case.join("review")).unwrap();
    let journal = super::finalize_journal::FinalizeJournal {
        finalize_id: "e".repeat(32),
        host_id: crate::util::host_id(),
        signoff_sha256: "1".repeat(64),
        package_name: "package_1".into(),
        started_unix: crate::util::now_unix().unwrap(),
        state: super::finalize_journal::STATE_IN_PROGRESS.into(),
        ..Default::default()
    };
    journal
        .local_record(&case, super::finalize_journal::LOCAL_OPEN)
        .write()
        .unwrap();
    journal
        .advance_local(
            &case,
            super::finalize_journal::LOCAL_PACKAGED,
            Some("3".repeat(64)),
        )
        .unwrap();
    // A retried advance is success; the state file is never rewritten.
    journal
        .advance_local(&case, super::finalize_journal::LOCAL_PACKAGED, None)
        .unwrap();
    journal
        .advance_local(&case, super::finalize_journal::LOCAL_COMPLETED, None)
        .unwrap();
    let record = super::finalize_journal::read_local_record(&journal.finalize_id)
        .unwrap()
        .unwrap();
    assert_eq!(record.state, super::finalize_journal::LOCAL_COMPLETED);
    assert_eq!(record.manifest_sha256, Some("3".repeat(64)));
    let dir = super::finalize_journal::local_open_dir().unwrap();
    let open = dir.join(format!("{}.json", journal.finalize_id));
    std::fs::remove_file(&open).unwrap();
    std::fs::create_dir(&open).unwrap();
    crate::package::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(Some(Duration::from_millis(50))));
    let removed = super::finalize_journal::remove_local_record(&journal.finalize_id);
    crate::package::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(None));
    assert!(removed.is_err());
    let packaged = dir.join(format!("{}.packaged.json", journal.finalize_id));
    let completed = dir.join(format!("{}.completed.json", journal.finalize_id));
    assert!(packaged.is_file() && completed.is_file());
    std::fs::remove_dir(&open).unwrap();
    assert_eq!(
        local_state(&journal.finalize_id).as_deref(),
        Some(super::finalize_journal::LOCAL_COMPLETED)
    );
    super::finalize_journal::remove_local_record(&journal.finalize_id).unwrap();
    assert!(!packaged.exists() && !completed.exists());
    // A state file that names another state is unreadable, never lower.
    journal
        .local_record(&case, super::finalize_journal::LOCAL_OPEN)
        .write()
        .unwrap();
    let text = std::fs::read_to_string(&open).unwrap();
    std::fs::write(&completed, text).unwrap();
    assert!(super::finalize_journal::read_local_record(&journal.finalize_id).is_err());
    super::finalize_journal::remove_local_record(&journal.finalize_id).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// R16-S4: another account's FrameTrace writer blocks only when the lane's
/// begin entry of that finalize records the same pid (82a6cb1: any
/// journal naming such a pid — planted, or a pid reused since — blocked
/// the case for 24 h).
#[test]
fn r16_s4_other_account_needs_the_begin_entrys_pid() {
    let journal = super::finalize_journal::FinalizeJournal {
        finalize_id: "f".repeat(32),
        writer: crate::util::WriterIdentity {
            pid: 4242,
            ..Default::default()
        },
        ..Default::default()
    };
    let frametrace = |_: u32| Some("frametrace-app.exe".to_string());
    let other = |_: u32| Some("svchost.exe".to_string());
    assert!(super::finalize_journal::writer_other_account(&journal, None, frametrace).is_none());
    assert!(
        super::finalize_journal::writer_other_account(&journal, Some(4243), frametrace).is_none()
    );
    assert!(super::finalize_journal::writer_other_account(&journal, Some(4242), other).is_none());
    assert_eq!(
        super::finalize_journal::writer_other_account(&journal, Some(4242), frametrace).as_deref(),
        Some("frametrace-app.exe")
    );
    // The force hint is what the block offers (never a plain dismiss).
    let case = Path::new("C:/cases/a b");
    assert!(
        super::finalize_journal::force_dismiss_command(case)
            .unwrap()
            .ends_with("--dismiss --force")
    );
}

/// R16-S6: the remembered complete-entry failures drop the OLDEST when
/// full (82a6cb1's `truncate(15)` kept the oldest 15 and dropped the
/// newer ones).
#[test]
fn r16_s6_failure_memory_drops_the_oldest() {
    let ids: Vec<String> = (0..20).map(|n| format!("{n:032x}")).collect();
    for id in &ids {
        super::finalize_journal::remember_complete_failure(id, &format!("detail {id}"));
    }
    assert!(super::finalize_journal::complete_failure(&ids[0]).is_none());
    for id in &ids[ids.len() - 16..] {
        assert_eq!(
            super::finalize_journal::complete_failure(id),
            Some(format!("detail {id}")),
            "{id}"
        );
    }
}

/// R16 (track I integration): a lane the finalize's `make-report --rehash`
/// scan appends to that cannot take an append (a torn last line) refuses
/// the finalize BEFORE anything is written — no begin entry, journal or
/// local record, the sign-off untouched, a message naming the lane.
#[test]
fn r16_torn_scan_lane_refuses_the_finalize_up_front() {
    let Some((root, case)) = finalize_case("torn-lane") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let signed = finalize_ok(&bin, &case, "홍길동");
        let lane_before = lane_text(&case);
        let anomaly = case.join(crate::report::ANOMALY_LOG_REL_PATH);
        std::fs::create_dir_all(anomaly.parent().unwrap()).unwrap();
        let mut torn = std::fs::read(&anomaly).unwrap_or_default();
        torn.extend_from_slice(b"{\"torn\":");
        std::fs::write(&anomaly, &torn).unwrap();
        let state = bound_state(&case);
        let response = parse(&finalize_with_exe(&bin, &case, "김철수", &state));
        assert_eq!(response["ok"], false, "{response}");
        let error = response["error"].as_str().unwrap_or_default();
        assert!(
            error.contains(crate::report::ANOMALY_LOG_REL_PATH),
            "{error}"
        );
        assert_eq!(
            lane_text(&case),
            lane_before,
            "the finalize wrote a lane entry"
        );
        assert!(!super::finalize_journal::journal_path(&case).exists());
        assert_eq!(
            std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap(),
            signed
        );
        assert!(!case.join(super::jobs::SIGNOFF_PENDING_REL_PATH).exists());
    });
    let _ = std::fs::remove_dir_all(root);
}
