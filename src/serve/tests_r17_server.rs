//! Round-17 track S (docs/PLAN_R17_2026-10-18.md): a local record that
//! cannot advance is dropped instead of left `open` (P28), `--writer-gone`,
//! and S3–S10.

use super::tests_r16_server::{
    bound_state, cli_bin, drop_last_lane_line, finalize_case, finalize_ok, interrupted_finalize,
    journal_from_begin, journal_of_live_writer, keyless, lane_events_for, lane_text, last_begin,
    live_writer_of, local_state, package_entries, parse, scratch, signoff_sha, spawn_live_writer,
};
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// The R14-S1 attack on a finished finalize: cut the lane's last line
/// (`finalize-complete`), rebuild the journal from the plaintext begin
/// entry and put the older sign-off's bytes beside it; then a recovery.
fn cut_tail_and_recover(case: &Path, older: &[u8]) -> super::finalize_journal::FinalizeJournal {
    let cut = drop_last_lane_line(case);
    assert!(
        cut.contains(crate::report::FINALIZE_COMPLETE_EVENT),
        "{cut}"
    );
    let journal = journal_from_begin(&last_begin(case));
    journal.write(case).unwrap();
    std::fs::write(
        case.join(super::finalize_journal::PREVIOUS_SIGNOFF_REL_PATH),
        older,
    )
    .unwrap();
    super::finalize_journal::recover_bound_case(&bound_state(case));
    journal
}

/// Nothing of the good finalize was rolled back.
fn assert_not_rolled_back(case: &Path, good: &[u8], packages: &[String]) {
    assert_eq!(
        signoff_sha(case),
        crate::sha256::digest_bytes(good),
        "the good sign-off was replaced by the older bytes"
    );
    assert_eq!(
        package_entries(case),
        packages,
        "the good package was renamed _INCOMPLETE"
    );
    assert!(
        !lane_text(case).contains(crate::report::SIGNOFF_ABANDONED_EVENT),
        "the good sign-off was abandoned"
    );
}

/// R17-S1 (P28, portable "disk full after `open`"): while a good finalize
/// runs, the names of its `packaged` and `completed` state files are taken
/// (directories squat them), so neither can be created — the way a full
/// disk or an ACL change refuses them. 9bac14c kept the record `open` and
/// the journal `completed-unrecorded`; once the squatters were gone, the
/// R14-S1 attack (cut the complete line, rebuild the journal, older bytes
/// beside it) rolled the good finalize back. Now the `open` file is
/// deleted on the spot (no free space needed) and the journal goes: the
/// rebuilt journal is `unpaired` ("로컬 기록 없음") and nothing changes.
#[test]
fn r17_s1_record_that_cannot_advance_is_dropped() {
    let Some((root, case)) = finalize_case("s1-full") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let older = finalize_ok(&bin, &case, "이전검토자");
        let dir = super::finalize_journal::local_open_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let squatter = {
            let stop = Arc::clone(&stop);
            let dir = dir.clone();
            let case = case.clone();
            std::thread::spawn(move || {
                let mut squatted: Option<String> = None;
                while !stop.load(Ordering::SeqCst) {
                    if squatted.is_none()
                        && let Ok(Some(journal)) = super::finalize_journal::read_journal(&case)
                    {
                        for state in [
                            super::finalize_journal::LOCAL_PACKAGED,
                            super::finalize_journal::LOCAL_COMPLETED,
                        ] {
                            std::fs::create_dir(
                                dir.join(format!("{}.{state}.json", journal.finalize_id)),
                            )
                            .unwrap();
                        }
                        squatted = Some(journal.finalize_id);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                squatted
            })
        };
        crate::package::RENAME_BUDGET_OVERRIDE
            .with(|cell| cell.set(Some(Duration::from_millis(300))));
        let good = finalize_ok(&bin, &case, "홍길동");
        crate::package::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(None));
        stop.store(true, Ordering::SeqCst);
        let id = squatter
            .join()
            .unwrap()
            .expect("the squatter saw the journal");
        // The disk has room again.
        for state in [
            super::finalize_journal::LOCAL_PACKAGED,
            super::finalize_journal::LOCAL_COMPLETED,
        ] {
            std::fs::remove_dir(dir.join(format!("{id}.{state}.json"))).unwrap();
        }
        let packages = package_entries(&case);
        let journal = cut_tail_and_recover(&case, &older);
        assert_eq!(journal.finalize_id, id);
        assert_not_rolled_back(&case, &good, &packages);
        assert_eq!(
            lane_events_for(&case, &id),
            vec![crate::report::FINALIZE_BEGIN_EVENT.to_string()],
            "the recovery wrote something for the rebuilt journal"
        );
        assert!(local_state(&id).is_none(), "the open record was kept");
        let shown = parse(&api_status(&bound_state(&case)))["finalize_journal"].clone();
        assert_eq!(shown["unpaired_code"], "no-local-record", "{shown}");
        let _ = super::finalize_journal::remove_local_record(&id);
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R17-S4 (P28): a recovery whose `finalize-resolved` entry was written
/// but whose local `resolved` state file could not be created AND whose
/// `open` file could not be removed. 9bac14c ignored the failed advance
/// and removed the journal while the record still read `open` (the
/// invariant "the journal goes only once the record is past `open`"
/// broken). Now the journal is kept (blocking) until the record is past
/// `open` or gone; the next recovery that can do so removes both.
#[test]
fn r17_s4_unrecordable_resolved_never_leaves_an_open_record_alone() {
    let Some((root, case)) = finalize_case("s4-resolved") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        finalize_ok(&bin, &case, "홍길동");
        let journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Chain);
        super::finalize_journal::FAIL_LOCAL_STATE
            .with(|cell| cell.set(Some(super::finalize_journal::LOCAL_RESOLVED)));
        super::finalize_journal::FAIL_LOCAL_RECORD_REMOVAL.with(|cell| cell.set(true));
        super::finalize_journal::recover_bound_case(&bound_state(&case));
        super::finalize_journal::FAIL_LOCAL_STATE.with(|cell| cell.set(None));
        super::finalize_journal::FAIL_LOCAL_RECORD_REMOVAL.with(|cell| cell.set(false));
        assert!(
            lane_events_for(&case, &journal.finalize_id)
                .contains(&crate::report::FINALIZE_RESOLVED_EVENT.to_string()),
            "the recovery closed the begin entry"
        );
        let journal_gone = !super::finalize_journal::journal_path(&case).exists();
        let record = local_state(&journal.finalize_id);
        assert!(
            !(journal_gone && record.as_deref() == Some(super::finalize_journal::LOCAL_OPEN)),
            "the journal went while the local record still reads open"
        );
        // Once the record can advance, the lane's resolved entry closes it.
        super::finalize_journal::recover_bound_case(&bound_state(&case));
        assert!(!super::finalize_journal::journal_path(&case).exists());
        assert!(local_state(&journal.finalize_id).is_none());
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R17-S5: a torn scan lane refuses the finalize BEFORE the journal check /
/// recovery block — 9bac14c ran the recovery (and closed a superseded
/// journal) first, appending to the sign-off lane of a case the finalize
/// then refused.
#[test]
fn r17_s5_torn_scan_lane_refuses_before_any_recovery() {
    let Some((root, case)) = finalize_case("s5-torn") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        finalize_ok(&bin, &case, "홍길동");
        let journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Chain);
        let journal_bytes = std::fs::read(super::finalize_journal::journal_path(&case)).unwrap();
        let anomaly = case.join(crate::report::ANOMALY_LOG_REL_PATH);
        std::fs::create_dir_all(anomaly.parent().unwrap()).unwrap();
        let mut torn = std::fs::read(&anomaly).unwrap_or_default();
        torn.extend_from_slice(b"{\"torn\":");
        std::fs::write(&anomaly, &torn).unwrap();
        let lane_before = lane_text(&case);
        let state = bound_state(&case);
        let response = parse(&finalize_with_exe(&bin, &case, "김철수", &state));
        assert_eq!(response["ok"], false, "{response}");
        let error = response["error"].as_str().unwrap_or_default();
        assert!(
            error.contains(crate::report::ANOMALY_LOG_REL_PATH),
            "{error}"
        );
        assert_eq!(lane_text(&case), lane_before, "the sign-off lane changed");
        assert_eq!(
            std::fs::read(super::finalize_journal::journal_path(&case)).unwrap(),
            journal_bytes,
            "the journal changed"
        );
        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
    });
    let _ = std::fs::remove_dir_all(root);
}

fn set_old(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 24 * 3600);
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// R17-S6: an UNREADABLE local record (its state files disagree) older
/// than 30 days is swept only when its case is reachable and holds no
/// journal of it — 9bac14c swept it by age alone, so a crash-torn record
/// of a case on an unplugged drive (or one whose journal is still there)
/// was gone when it was needed.
#[test]
fn r17_s6_unreadable_record_is_swept_only_when_its_case_is_done() {
    let root = scratch("r17-s6-sweep");
    let old = crate::util::now_unix().unwrap() - 40 * 24 * 3600;
    let reachable = root.join("case-with-journal");
    let done = root.join("case-done");
    let away = root.join("not-plugged-in");
    std::fs::create_dir_all(reachable.join("review")).unwrap();
    std::fs::create_dir_all(done.join("review")).unwrap();
    let dir = super::finalize_journal::local_open_dir().unwrap();
    let ids: Vec<String> = ['a', 'b', '9']
        .iter()
        .map(|digit| format!("{}{}", "7".repeat(31), digit))
        .collect();
    for (id, case) in ids.iter().zip([&reachable, &away, &done]) {
        let record = super::finalize_journal::LocalRecord {
            finalize_id: id.clone(),
            case_dir: case.to_string_lossy().to_string(),
            signoff_sha256: "1".repeat(64),
            previous_signoff_sha256: None,
            package_name: "package_1".into(),
            host_id: crate::util::host_id(),
            started_unix: old,
            state: super::finalize_journal::LOCAL_OPEN.into(),
            updated_unix: old,
            manifest_sha256: None,
        };
        record.write().unwrap();
        // A second state file naming the wrong state: unreadable.
        let open = dir.join(format!("{id}.json"));
        let completed = dir.join(format!("{id}.completed.json"));
        std::fs::copy(&open, &completed).unwrap();
        set_old(&open, 40);
        set_old(&completed, 40);
        assert!(super::finalize_journal::read_local_record(id).is_err());
    }
    super::finalize_journal::FinalizeJournal {
        finalize_id: ids[0].clone(),
        signoff_sha256: "1".repeat(64),
        state: super::finalize_journal::STATE_IN_PROGRESS.into(),
        ..Default::default()
    }
    .write(&reachable)
    .unwrap();
    super::finalize_journal::sweep_local_records();
    let present = |id: &str| dir.join(format!("{id}.json")).exists();
    let kept = (present(&ids[0]), present(&ids[1]), present(&ids[2]));
    for id in &ids {
        let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
        let _ = std::fs::remove_file(dir.join(format!("{id}.completed.json")));
    }
    assert_eq!(
        kept,
        (true, true, false),
        "(case holds its journal, case unreachable, case done)"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// R17-S7: a journal left behind by a finalize that COMPLETED (the lane
/// holds its complete entry, this PC's record reads `completed`) whose
/// writer — the workstation — still runs. 9bac14c judged the live writer
/// first, so `finalize-journal --dismiss` from the CLI was refused ("다른
/// FrameTrace 프로세스가 최종화하고 있어") although nothing was running.
/// Now the lane's complete entry (or the local `completed`) skips the
/// liveness check: the journal is only removed.
#[test]
fn r17_s7_completed_finalize_is_never_a_live_writer() {
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let Some((root, case)) = finalize_case("s7-done") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let mut child = spawn_live_writer();
    let result = std::panic::catch_unwind(|| {
        keyless(|| {
            finalize_ok(&bin, &case, "홍길동");
            let mut journal = journal_from_begin(&last_begin(&case));
            journal.writer = live_writer_of(child.id());
            journal.writer_image_sha256 = super::finalize_journal::own_exe_sha256();
            journal.write(&case).unwrap();
            journal
                .local_record(&case, super::finalize_journal::LOCAL_COMPLETED)
                .write()
                .unwrap();
            let shown = parse(&api_status(&bound_state(&case)))["finalize_journal"].clone();
            assert_eq!(shown["state"], "completed", "{shown}");
            assert_eq!(shown["blocks_finalize"], false, "{shown}");
            let out = super::finalize_journal::cli_command(&case, true, false);
            assert!(out.is_ok(), "{out:?}");
            assert!(!super::finalize_journal::journal_path(&case).exists());
            assert!(local_state(&journal.finalize_id).is_none());
        })
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(root);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// R17-S1 (P28, test hook): the local folder takes the `open` file and
/// nothing after it. The good finalize completes, its `open` file is
/// deleted on the spot, the journal goes; the R14-S1 attack then finds no
/// local record and changes nothing.
#[test]
fn r17_s1_full_disk_after_open_drops_the_record() {
    let Some((root, case)) = finalize_case("s1-hook") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let older = finalize_ok(&bin, &case, "이전검토자");
        super::finalize_journal::FAIL_LOCAL_PAST_OPEN.with(|cell| cell.set(true));
        let good = finalize_ok(&bin, &case, "홍길동");
        super::finalize_journal::FAIL_LOCAL_PAST_OPEN.with(|cell| cell.set(false));
        let id = last_begin(&case)["finalize_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!super::finalize_journal::journal_path(&case).exists());
        assert!(local_state(&id).is_none());
        let packages = package_entries(&case);
        cut_tail_and_recover(&case, &older);
        assert_not_rolled_back(&case, &good, &packages);
        assert_eq!(
            lane_events_for(&case, &id),
            vec![crate::report::FINALIZE_BEGIN_EVENT.to_string()]
        );
        let shown = parse(&api_status(&bound_state(&case)))["finalize_journal"].clone();
        assert_eq!(shown["state"], "unpaired", "{shown}");
        assert_eq!(shown["unpaired_code"], "no-local-record", "{shown}");
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R17-S1 (P28): when the `open` file cannot be deleted either, the journal
/// is kept (`completed-unrecorded`) and blocks; a recovery that still can
/// do neither keeps it; once the record can advance (or be deleted) the
/// lane's complete entry closes it.
#[test]
fn r17_s1_undeletable_open_record_keeps_the_journal() {
    let Some((root, case)) = finalize_case("s1-keep") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        finalize_ok(&bin, &case, "이전검토자");
        super::finalize_journal::FAIL_LOCAL_PAST_OPEN.with(|cell| cell.set(true));
        super::finalize_journal::FAIL_LOCAL_RECORD_REMOVAL.with(|cell| cell.set(true));
        finalize_ok(&bin, &case, "홍길동");
        let journal = super::finalize_journal::read_journal(&case)
            .unwrap()
            .expect("kept");
        assert_eq!(
            journal.state,
            super::finalize_journal::STATE_COMPLETED_UNRECORDED
        );
        assert_eq!(
            local_state(&journal.finalize_id).as_deref(),
            Some(super::finalize_journal::LOCAL_OPEN)
        );
        let state = bound_state(&case);
        super::finalize_journal::recover_bound_case(&state);
        assert!(super::finalize_journal::journal_path(&case).is_file());
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["blocks_finalize"], true, "{shown}");
        // The disk has room again: the record goes past `open`, then away.
        super::finalize_journal::FAIL_LOCAL_PAST_OPEN.with(|cell| cell.set(false));
        super::finalize_journal::FAIL_LOCAL_RECORD_REMOVAL.with(|cell| cell.set(false));
        super::finalize_journal::recover_bound_case(&state);
        assert!(!super::finalize_journal::journal_path(&case).exists());
        assert!(local_state(&journal.finalize_id).is_none());
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R17-S1 (P28): a recovery that closes a journal by the lane while the
/// record still reads `open` and cannot advance deletes the `open` file
/// and removes the journal (no free space needed).
#[test]
fn r17_s1_lane_closed_journal_drops_an_unadvanceable_record() {
    let Some((root, case)) = finalize_case("s1-close") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        finalize_ok(&bin, &case, "홍길동");
        // A good finalize whose journal removal "failed": the lane holds
        // its complete entry, the record reads `open` again.
        let journal = journal_from_begin(&last_begin(&case));
        journal.write(&case).unwrap();
        journal
            .local_record(&case, super::finalize_journal::LOCAL_OPEN)
            .write()
            .unwrap();
        super::finalize_journal::FAIL_LOCAL_PAST_OPEN.with(|cell| cell.set(true));
        super::finalize_journal::recover_bound_case(&bound_state(&case));
        super::finalize_journal::FAIL_LOCAL_PAST_OPEN.with(|cell| cell.set(false));
        assert!(!super::finalize_journal::journal_path(&case).exists());
        assert!(local_state(&journal.finalize_id).is_none());
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R17-S2 / S8: an unverified live writer's block offers waiting first,
/// then `--writer-gone` and `--dismiss --force` naming what the latter
/// leaves in place; `--writer-gone` runs the NORMAL recovery (the
/// interrupted finalize's sign-off taken back, its package marked) where
/// `--force` only moved the journal. A VERIFIED live writer gets no
/// command (S8) and `--writer-gone` is refused.
#[test]
fn r17_s2_writer_gone_runs_the_normal_recovery() {
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let Some((root, case)) = finalize_case("s2-gone") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let mut child = spawn_live_writer();
    let result = std::panic::catch_unwind(|| {
        keyless(|| {
            let before = finalize_ok(&bin, &case, "홍길동");
            // Verified first (S8): no command, --writer-gone refused.
            let mut journal = journal_of_live_writer(
                &bin,
                &case,
                &child,
                super::finalize_journal::own_exe_sha256(),
            );
            let shown = parse(&api_status(&bound_state(&case)))["finalize_journal"].clone();
            assert_eq!(shown["in_progress_code"], "live-writer", "{shown}");
            for key in [
                "force_dismiss_command",
                "force_dismiss_command_cmd",
                "writer_gone_command",
                "writer_gone_command_cmd",
                "dismiss_command",
            ] {
                assert!(shown[key].is_null(), "{key}: {shown}");
            }
            // R18-S3: refused even with --i-am-sure.
            assert!(super::finalize_journal::cli_writer_gone(&case, true).is_err());
            // Unverified: waiting first, then the two commands.
            journal.writer_image_sha256 = Some("0".repeat(64));
            journal.write(&case).unwrap();
            let shown = parse(&api_status(&bound_state(&case)))["finalize_journal"].clone();
            assert_eq!(shown["in_progress_code"], "unverified-writer", "{shown}");
            let gone = shown["writer_gone_command"].as_str().unwrap();
            // R18-S7: the live process matches pid and start time.
            assert!(gone.ends_with("--writer-gone --i-am-sure"), "{gone}");
            assert_eq!(shown["writer_gone_i_am_sure"], true, "{shown}");
            assert!(shown["force_dismiss_command"].is_string(), "{shown}");
            if cfg!(windows) {
                assert!(shown["writer_gone_command_cmd"].is_string(), "{shown}");
            }
            let reason = shown["reason"].as_str().unwrap();
            let wait = reason.find("기다리면").expect(reason);
            assert!(wait < reason.find("--writer-gone").unwrap(), "{reason}");
            assert!(wait < reason.find("--force").unwrap(), "{reason}");
            assert!(
                reason.contains(&format!("reports/{}", journal.package_name)),
                "{reason}"
            );
            assert!(reason.contains("review/signoff.json"), "{reason}");
            // R18-S7: without --i-am-sure nothing is done.
            let refused = super::finalize_journal::cli_writer_gone(&case, false).unwrap_err();
            assert!(refused.contains("--i-am-sure"), "{refused}");
            assert!(super::finalize_journal::journal_path(&case).exists());
            let out = super::finalize_journal::cli_writer_gone(&case, true).unwrap();
            assert!(out.contains("--writer-gone"), "{out}");
            assert_eq!(
                signoff_sha(&case),
                crate::sha256::digest_bytes(&before),
                "the interrupted finalize's sign-off was not taken back: {out}"
            );
            assert!(
                package_entries(&case)
                    .iter()
                    .any(|name| name.starts_with(&journal.package_name)
                        && name.contains(crate::package::INCOMPLETE_PACKAGE_SUFFIX)),
                "{:?}",
                package_entries(&case)
            );
            assert!(
                lane_text(&case).contains(crate::report::SIGNOFF_ABANDONED_EVENT),
                "abandoned"
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

/// R17-S2: `--dismiss --force` says what it leaves in place.
#[test]
fn r17_s2_force_names_what_it_leaves() {
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let Some((root, case)) = finalize_case("s2-force") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let mut child = spawn_live_writer();
    let result = std::panic::catch_unwind(|| {
        keyless(|| {
            finalize_ok(&bin, &case, "홍길동");
            let journal = journal_of_live_writer(&bin, &case, &child, Some("0".repeat(64)));
            let out = super::finalize_journal::cli_command(&case, true, true).unwrap();
            assert!(
                out.contains(&format!("reports/{}", journal.package_name)),
                "{out}"
            );
            assert!(out.contains("이 최종화의 검토자 기록"), "{out}");
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

/// R17-S3: another account's process holding the writer's pid counts as
/// the writer only when its start time (when known) is the recorded one —
/// a pid reused since no longer blocks for 24 h.
#[test]
fn r17_s3_other_account_needs_the_writers_start_time() {
    let journal = super::finalize_journal::FinalizeJournal {
        writer: crate::util::WriterIdentity {
            pid: 4242,
            started_ms: Some(1_700_000_000_000),
            image: None,
        },
        ..Default::default()
    };
    let at = |ms: u64| Some(UNIX_EPOCH + Duration::from_millis(ms));
    let matches = super::finalize_journal::other_account_start_matches;
    assert!(matches(&journal, at(1_700_000_000_500)));
    assert!(!matches(&journal, at(1_700_000_600_000)), "a reused pid");
    assert!(matches(&journal, None), "unknown: R16's rule stands");
    let unrecorded = super::finalize_journal::FinalizeJournal::default();
    assert!(matches(&unrecorded, at(1)));
}

/// R17-S3: the system process list reports this process's start time
/// (what another account's process is judged by) as `GetProcessTimes` does.
#[cfg(windows)]
#[test]
fn r17_s3_system_process_list_reads_start_times() {
    let own = std::process::id();
    let listed = crate::util::system_process_start_time(own).expect("listed");
    let probed = match crate::util::probe_process(own) {
        crate::util::ProcessProbe::Running {
            started: Some(started),
        } => started,
        other => panic!("{other:?}"),
    };
    let diff = listed
        .duration_since(probed)
        .unwrap_or_else(|err| err.duration());
    assert!(diff < Duration::from_millis(5), "{diff:?}");
    assert!(crate::util::system_process_start_time(u32::MAX - 3).is_none());
}

/// R17-S9: the status path never streams a manifest over 16 MiB — it
/// shows the record's word "확인 보류" (`manifest_check: "withheld"`); the
/// recovery hashes it in full and records the finalize complete.
#[test]
fn r17_s9_status_withholds_large_manifests() {
    let Some((root, case)) = finalize_case("s9-large") else {
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
        let state = bound_state(&case);
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["state"], "completed-unrecorded", "{shown}");
        assert_eq!(shown["manifest_check"], "withheld", "{shown}");
        // R18-S2 (P31): unknown, not "does not block".
        assert!(shown["blocks_finalize"].is_null(), "{shown}");
        assert!(
            shown["reason"].as_str().unwrap().starts_with("확인 보류"),
            "{shown}"
        );
        super::finalize_journal::recover_bound_case(&state);
        assert_eq!(
            lane_events_for(&case, &journal.finalize_id),
            vec![
                crate::report::FINALIZE_BEGIN_EVENT.to_string(),
                crate::report::FINALIZE_COMPLETE_EVENT.to_string()
            ]
        );
        assert!(!super::finalize_journal::journal_path(&case).exists());
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R17-S10: `cmd.exe` expands `%VAR%` inside double quotes and has no
/// escape for it at its prompt — a case path holding `%` gets no
/// `cmd.exe` form. R18-S1 (P29): no PowerShell form either — `%` is not
/// on the paste-safe whitelist.
#[test]
fn r17_s10_percent_paths_get_no_cmd_form() {
    let plain = Path::new("C:/cases/a b");
    let percent = Path::new("C:/cases/%USERPROFILE%x");
    assert_eq!(
        super::finalize_journal::dismiss_command_cmd(plain).is_some(),
        cfg!(windows)
    );
    for cmd in [
        super::finalize_journal::dismiss_command_cmd(percent),
        super::finalize_journal::force_dismiss_command_cmd(percent),
        super::finalize_journal::writer_gone_command_cmd(percent, false),
        super::finalize_journal::dismiss_command(percent),
    ] {
        assert!(cmd.is_none(), "{cmd:?}");
    }
}
