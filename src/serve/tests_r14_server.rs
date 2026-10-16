//! Round-14 track S (docs/PLAN_R14_2026-10-16.md): a recovery acts only
//! when this PC's local open-finalize record, the lane's begin entry and
//! the journal agree (P18), and external evidence beats the lane tail
//! (P19).

use super::*;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r14s-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The real CLI binary next to the test executable (`None`: skipped).
fn cli_bin() -> Option<PathBuf> {
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
fn finalize_case(tag: &str) -> Option<(PathBuf, PathBuf)> {
    let bin = cli_bin()?;
    let root = scratch(tag);
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.mp4"), b"fixture-video-bytes-r14").unwrap();
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

fn bound_state(case: &Path) -> SharedState {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.to_path_buf());
    state
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("invalid JSON {err}: {text}"))
}

fn package_entries(case: &Path) -> Vec<String> {
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

fn lane_path(case: &Path) -> PathBuf {
    case.join(crate::report::SIGNOFF_LOG_REL_PATH)
}

fn lane_text(case: &Path) -> String {
    std::fs::read_to_string(lane_path(case)).unwrap_or_default()
}

fn finalize_ok(bin: &Path, case: &Path, examiner: &str) -> Vec<u8> {
    let state = bound_state(case);
    let response = parse(&finalize_with_exe(bin, case, examiner, &state));
    assert_eq!(response["ok"], true, "{response}");
    std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap()
}

fn last_begin(case: &Path) -> serde_json::Value {
    lane_text(case)
        .lines()
        .rev()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|value| value["event"] == crate::report::FINALIZE_BEGIN_EVENT)
        .expect("a finalize-begin entry")
}

/// The journal an attacker rebuilds from the plaintext begin entry.
fn journal_from_begin(begin: &serde_json::Value) -> super::finalize_journal::FinalizeJournal {
    super::finalize_journal::FinalizeJournal {
        finalize_id: begin["finalize_id"].as_str().unwrap().into(),
        host_id: begin["host_id"].as_str().unwrap().into(),
        writer: crate::util::WriterIdentity::default(),
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
fn drop_last_lane_line(case: &Path) -> String {
    let text = lane_text(case);
    let mut lines: Vec<&str> = text.lines().collect();
    let last = lines.pop().unwrap().to_string();
    let mut kept = lines.join("\n");
    kept.push('\n');
    std::fs::write(lane_path(case), kept).unwrap();
    last
}

/// R14-S1 (P18, real CLI, keyed lane): the R13 reviewers' PoC. After two
/// good keyed finalizes, the lane's last line (`finalize-complete`) is cut,
/// the journal rebuilt from the plaintext begin entry (pid 0) and the
/// previous sign-off's bytes put in `finalize.previous-signoff.bin`. The
/// begin entry is then the lane's latest unmatched one, so 40126c3's
/// recovery abandoned the good keyed sign-off, wrote the older bytes back
/// and renamed the package `_INCOMPLETE`. Without this PC's local
/// open-finalize record nothing changes: `unpaired`, "로컬 기록 없음".
#[test]
fn r14_s1_cut_complete_and_rebuilt_journal_changes_nothing() {
    let Some((root, case)) = finalize_case("s1-cut") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let key = crate::audit_key::AuditKey::new("r14-pc", &[0x6b; 32]);
    crate::audit_key::with_test_keys(std::slice::from_ref(&key), || {
        let older = finalize_ok(&bin, &case, "이전검토자");
        let good = finalize_ok(&bin, &case, "홍길동");
        assert_ne!(older, good);
        let packages = package_entries(&case);
        assert_eq!(packages.len(), 2, "{packages:?}");
        let cut = drop_last_lane_line(&case);
        assert!(
            cut.contains(crate::report::FINALIZE_COMPLETE_EVENT),
            "{cut}"
        );
        assert!(cut.contains("hmac"), "keyed lane: {cut}");
        let begin = last_begin(&case);
        journal_from_begin(&begin).write(&case).unwrap();
        std::fs::write(
            case.join(super::finalize_journal::PREVIOUS_SIGNOFF_REL_PATH),
            &older,
        )
        .unwrap();
        let lane_before = lane_text(&case);
        let state = bound_state(&case);
        super::finalize_journal::recover_bound_case(&state);
        assert_eq!(
            std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap(),
            good,
            "the good keyed sign-off was replaced by the older bytes"
        );
        assert_eq!(
            package_entries(&case),
            packages,
            "the completed package was renamed _INCOMPLETE"
        );
        assert_eq!(
            lane_text(&case),
            lane_before,
            "the lane gained an entry (abandon of the good sign-off)"
        );
        let status = parse(&api_status(&state));
        let shown = &status["finalize_journal"];
        assert_eq!(shown["state"], "unpaired", "{status}");
        assert!(
            shown["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("로컬 기록 없음"),
            "{status}"
        );
        assert_eq!(status["finalize_recovered"], false, "{status}");
        assert!(super::finalize_journal::journal_path(&case).is_file());
    });
    let _ = std::fs::remove_dir_all(root);
}

fn key() -> crate::audit_key::AuditKey {
    crate::audit_key::AuditKey::new("r14-pc", &[0x6b; 32])
}

fn keyed<R>(f: impl FnOnce() -> R) -> R {
    crate::audit_key::with_test_keys(&[key()], f)
}

/// No audit key on this PC (every lane entry self-consistent). Tests whose
/// recovery must PAIR use it: the test keys are thread-local, so the CLI
/// steps a finalize runs (make-report --rehash appends a `hash-verify-head`
/// entry after the begin entry, track I) write unkeyed entries — under a
/// key those would be entries this PC cannot vouch for (P19: unpaired).
fn keyless<R>(f: impl FnOnce() -> R) -> R {
    crate::audit_key::with_test_keys(&[], f)
}

/// A finalize (김철수) that "dies" at `stop`; returns its journal.
fn interrupted_finalize(
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

fn signoff_bytes(case: &Path) -> Vec<u8> {
    std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap()
}

fn signoff_sha(case: &Path) -> String {
    crate::sha256::digest_bytes(&signoff_bytes(case))
}

fn local_record(id: &str) -> Option<super::finalize_journal::LocalRecord> {
    super::finalize_journal::read_local_record(id).unwrap()
}

fn lane_events_for(case: &Path, id: &str) -> Vec<String> {
    lane_text(case)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| value["finalize_id"] == id)
        .map(|value| value["event"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// R14 (P18, real CLI): a finalize writes its local record before the
/// begin entry and removes it once complete; an interrupted one keeps it
/// (state `open`, the journal's values) until the recovery closes it.
#[test]
fn r14_p18_local_record_follows_the_finalize() {
    let Some((root, case)) = finalize_case("p18-local") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let signed = finalize_ok(&bin, &case, "홍길동");
        let done = last_begin(&case);
        assert!(local_record(done["finalize_id"].as_str().unwrap()).is_none());
        let journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Promotion);
        // A trusted `hash-verify-head` (track I, written by make-report
        // --rehash) follows the begin entry: it does not unpair it.
        let after_begin = lane_text(&case)
            .split(&journal.finalize_id)
            .nth(1)
            .unwrap_or_default()
            .to_string();
        assert!(after_begin.contains("hash-verify-head"), "{after_begin}");
        let local = local_record(&journal.finalize_id).expect("kept while interrupted");
        assert_eq!(local.state, super::finalize_journal::LOCAL_OPEN);
        assert_eq!(local.signoff_sha256, journal.signoff_sha256);
        assert_eq!(local.package_name, journal.package_name);
        let dir = super::finalize_journal::local_open_dir().unwrap();
        assert!(
            dir.starts_with(super::finalize_journal::test_local_base()),
            "tests never touch the real local state: {}",
            dir.display()
        );
        let state = bound_state(&case);
        super::finalize_journal::recover_bound_case(&state);
        assert_eq!(
            signoff_bytes(&case),
            signed,
            "recovered (journal + lane + local agree)"
        );
        // Only the stale report is left: the journal and the local record
        // stay until a finalize re-renders it, then both go.
        assert!(local_record(&journal.finalize_id).is_some());
        let response = parse(&finalize_with_exe(&bin, &case, "홍길동", &state));
        assert_eq!(response["ok"], true, "{response}");
        assert!(local_record(&journal.finalize_id).is_none());
        assert!(!super::finalize_journal::journal_path(&case).exists());
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R14 (P19, real CLI): the workstation died after package-case published
/// a complete package carrying the promoted sign-off — the finalize
/// FINISHED; only its `finalize-complete` entry is missing. 40126c3 rolled
/// it back (abandoned the sign-off, restored the older one, renamed the
/// package `_INCOMPLETE`). Now only the complete entry is written.
#[test]
fn r14_p19_crash_after_publish_is_a_finished_finalize() {
    let Some((root, case)) = finalize_case("p19-finished") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        finalize_ok(&bin, &case, "홍길동");
        let journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Package);
        let promoted = signoff_bytes(&case);
        assert_eq!(
            crate::sha256::digest_bytes(&promoted),
            journal.signoff_sha256
        );
        let packages = package_entries(&case);
        let state = bound_state(&case);
        assert_eq!(
            parse(&api_status(&state))["finalize_journal"]["state"],
            "completed-unrecorded"
        );
        super::finalize_journal::recover_bound_case(&state);
        assert_eq!(
            signoff_bytes(&case),
            promoted,
            "the finished sign-off stays"
        );
        assert_eq!(package_entries(&case), packages, "nothing renamed");
        assert_eq!(
            lane_events_for(&case, &journal.finalize_id),
            vec![
                crate::report::FINALIZE_BEGIN_EVENT.to_string(),
                crate::report::FINALIZE_COMPLETE_EVENT.to_string()
            ]
        );
        assert!(!lane_text(&case).contains(crate::report::SIGNOFF_ABANDONED_EVENT));
        assert!(!super::finalize_journal::journal_path(&case).exists());
        assert!(local_record(&journal.finalize_id).is_none());
        let status = parse(&api_status(&state));
        assert_eq!(status["finalize_recovered"], false, "{status}");
        assert_eq!(status["finalize_journal"], serde_json::Value::Null);
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R14 (P19): a begin entry followed by an entry this PC cannot vouch for
/// (here a correctly chained entry without the HMAC this PC's key would
/// give it — the cut complete entry may have been replaced by it) is
/// `unpaired`: nothing is rolled back. 40126c3 recovered it. (A torn or
/// edited line breaks the chain, and no entry of a broken lane pairs.)
#[test]
fn r14_p19_untrusted_line_after_the_begin_is_unpaired() {
    let Some((root, case)) = finalize_case("p19-untrusted") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyed(|| {
        finalize_ok(&bin, &case, "홍길동");
        let journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Promotion);
        crate::audit_key::with_test_keys(&[], || {
            crate::audit::append_chained_jsonl(
                &lane_path(&case),
                &serde_json::json!({"schema_version": 1, "event": "note"}).to_string(),
            )
            .unwrap();
        });
        let before = lane_text(&case);
        let state = bound_state(&case);
        super::finalize_journal::recover_bound_case(&state);
        assert_eq!(
            signoff_sha(&case),
            journal.signoff_sha256,
            "nothing rolled back"
        );
        assert_eq!(lane_text(&case), before);
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["state"], "unpaired", "{shown}");
        assert_eq!(shown["unpaired_code"], "lane", "{shown}");
        assert!(
            shown["reason"].as_str().unwrap().contains("확인할 수 없는"),
            "{shown}"
        );
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S2 (real CLI): the `finalize-complete` append failed. 40126c3
/// removed the journal anyway — the begin entry stayed open for good. Now
/// the journal and the local record are kept as `completed-unrecorded`
/// (not blocking) and the next recovery writes the entry, changing
/// nothing else.
#[test]
fn r14_s2_unrecorded_completion_is_recorded_next_time() {
    let Some((root, case)) = finalize_case("s2-unrecorded") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        finalize_ok(&bin, &case, "이전검토자");
        super::jobs::FAIL_FINALIZE_COMPLETE.with(|cell| cell.set(true));
        let state = bound_state(&case);
        let response = parse(&finalize_with_exe(&bin, &case, "홍길동", &state));
        super::jobs::FAIL_FINALIZE_COMPLETE.with(|cell| cell.set(false));
        assert_eq!(response["ok"], true, "{response}");
        let journal = super::finalize_journal::read_journal(&case)
            .unwrap()
            .expect("kept");
        assert_eq!(
            journal.state,
            super::finalize_journal::STATE_COMPLETED_UNRECORDED
        );
        assert_eq!(
            local_record(&journal.finalize_id).unwrap().state,
            super::finalize_journal::STATE_COMPLETED_UNRECORDED
        );
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["state"], "completed-unrecorded", "{shown}");
        assert_eq!(shown["blocks_finalize"], false, "{shown}");
        let good = signoff_bytes(&case);
        let packages = package_entries(&case);
        super::finalize_journal::recover_bound_case(&bound_state(&case));
        assert_eq!(signoff_bytes(&case), good);
        assert_eq!(package_entries(&case), packages);
        assert!(
            lane_events_for(&case, &journal.finalize_id)
                .contains(&crate::report::FINALIZE_COMPLETE_EVENT.to_string())
        );
        assert!(!super::finalize_journal::journal_path(&case).exists());
        assert!(local_record(&journal.finalize_id).is_none());
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S3 (real CLI): the R13 reviewer's PoC — `pid: 4` (System: its start
/// time cannot be read by a user process) and a `started_unix` in the
/// future. 40126c3 took the unknown start time as a live writer for 24 h:
/// every finalize blocked, `--dismiss` refused. A writer counts only on a
/// positive match of a FrameTrace executable.
#[test]
fn r14_s3_unknown_start_time_is_not_a_live_writer() {
    let Some((root, case)) = finalize_case("s3-pid4") else {
        return;
    };
    let bin = cli_bin().unwrap();
    finalize_ok(&bin, &case, "홍길동");
    let mut journal = journal_from_begin(&last_begin(&case));
    journal.writer = crate::util::WriterIdentity {
        pid: 4,
        started_ms: Some(1_700_000_000_000),
        image: Some("frametrace-app.exe".into()),
    };
    journal.started_unix = crate::util::now_unix().unwrap() + 20 * 3600;
    journal.write(&case).unwrap();
    let state = bound_state(&case);
    let shown = parse(&api_status(&state))["finalize_journal"].clone();
    assert_ne!(shown["state"], "in-progress", "{shown}");
    let mut dismiss = std::process::Command::new(&bin);
    dismiss.args(["finalize-journal", case.to_str().unwrap(), "--dismiss"]);
    super::finalize_journal::isolate_local_state(&mut dismiss);
    let output = dismiss.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!super::finalize_journal::journal_path(&case).exists());
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S3: a journal whose writer really runs (this test process, a
/// FrameTrace executable whose start time is known) is refused by
/// `--dismiss` and moved by `--dismiss --force`.
#[test]
fn r14_s3_dismiss_force_moves_a_live_writers_journal() {
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let Some((root, case)) = finalize_case("s3-force") else {
        return;
    };
    let bin = cli_bin().unwrap();
    finalize_ok(&bin, &case, "홍길동");
    let mut journal = journal_from_begin(&last_begin(&case));
    journal.writer = crate::util::current_writer();
    journal.write(&case).unwrap();
    let run = |args: &[&str]| {
        let mut command = std::process::Command::new(&bin);
        command.args(args);
        super::finalize_journal::isolate_local_state(&mut command);
        command.output().unwrap()
    };
    let case_text = case.to_str().unwrap();
    let shown = run(&["finalize-journal", case_text]);
    assert!(
        String::from_utf8_lossy(&shown.stdout).contains("state: in-progress"),
        "{}",
        String::from_utf8_lossy(&shown.stdout)
    );
    let refused = run(&["finalize-journal", case_text, "--dismiss"]);
    assert!(!refused.status.success());
    assert!(super::finalize_journal::journal_path(&case).is_file());
    let forced = run(&["finalize-journal", case_text, "--dismiss", "--force"]);
    assert!(
        forced.status.success(),
        "{}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert!(!super::finalize_journal::journal_path(&case).exists());
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S4: the journal is created exclusively and only its own finalize
/// removes or rewrites it. 40126c3 overwrote another finalize's journal
/// (two PCs on a shared case) and deleted it on completion.
#[test]
fn r14_s4_journal_is_exclusive_and_removed_only_by_its_finalize() {
    let root = scratch("s4-exclusive");
    let case = root.join("case");
    std::fs::create_dir_all(case.join("review")).unwrap();
    let theirs = super::finalize_journal::FinalizeJournal {
        finalize_id: "a".repeat(32),
        host_id: "f".repeat(32),
        signoff_sha256: "1".repeat(64),
        package_name: "package_1".into(),
        state: super::finalize_journal::STATE_IN_PROGRESS.into(),
        ..Default::default()
    };
    theirs.create_exclusive(&case).unwrap();
    let mine = super::finalize_journal::FinalizeJournal {
        finalize_id: "b".repeat(32),
        ..theirs.clone()
    };
    let err = mine.create_exclusive(&case).unwrap_err();
    assert!(err.contains("이미 있습니다"), "{err}");
    assert!(super::finalize_journal::remove_journal(&case, &mine.finalize_id).is_err());
    assert!(mine.rewrite_as("interrupted", &case).is_err());
    assert_eq!(
        super::finalize_journal::read_journal(&case).unwrap(),
        Some(theirs.clone())
    );
    super::finalize_journal::remove_journal(&case, &theirs.finalize_id).unwrap();
    assert!(!super::finalize_journal::journal_path(&case).exists());
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S5 (real CLI): `--dismiss` of an interrupted finalize this PC can
/// clean up runs that cleanup FIRST. 40126c3 moved the journal aside and
/// left the published (incomplete) package plainly named and the
/// unfinished sign-off promoted.
#[test]
fn r14_s5_dismiss_of_an_open_journal_cleans_up_first() {
    let Some((root, case)) = finalize_case("s5-dismiss-open") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let signed = finalize_ok(&bin, &case, "홍길동");
    let mut journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Package);
    // Its writer (this test process) is gone, as after a crash.
    journal.writer = crate::util::WriterIdentity::default();
    journal.write(&case).unwrap();
    let reserved = case.join("reports").join(&journal.package_name);
    // The publish did not finish (a listed file is missing).
    std::fs::remove_file(reserved.join("README.txt")).unwrap();
    let mut dismiss = std::process::Command::new(&bin);
    dismiss.args(["finalize-journal", case.to_str().unwrap(), "--dismiss"]);
    super::finalize_journal::isolate_local_state(&mut dismiss);
    let output = dismiss.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        output.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !reserved.exists(),
        "the reserved package was marked: {text}"
    );
    assert!(
        case.join("reports")
            .join(format!("{}_INCOMPLETE", journal.package_name))
            .is_dir()
    );
    assert_eq!(signoff_bytes(&case), signed, "restored before the dismiss");
    assert!(!super::finalize_journal::journal_path(&case).exists());
    assert!(local_record(&journal.finalize_id).is_none());
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S7: the host id comes from the machine id and the OS host name
/// (`GetComputerNameExW` — not the `COMPUTERNAME` variable).
#[test]
fn r14_s7_host_id_does_not_follow_the_environment() {
    let id = crate::util::host_id();
    assert!(
        id == crate::util::HOST_ID_UNKNOWN
            || (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())),
        "{id}"
    );
    if cfg!(windows) {
        let name = crate::util::host_name().expect("GetComputerNameExW");
        assert!(!name.is_empty());
    }
}

/// R14-S8 (real CLI): another PC's journal older than 24 h is moved aside
/// by a finalize here, and the package that finalize reserved — published,
/// carrying its sign-off, but not finished — gets the finalize-incomplete
/// marker beside it, without a rename. 40126c3 left it plainly final.
#[test]
fn r14_s8_other_hosts_unfinished_package_is_marked_when_set_aside() {
    let Some((root, case)) = finalize_case("s8-other-host") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let signed = finalize_ok(&bin, &case, "홍길동");
    let mut journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Package);
    // The other PC had rolled its sign-off back: not finished.
    std::fs::write(case.join(crate::report::SIGNOFF_REL_PATH), &signed).unwrap();
    journal.host_id = "e".repeat(32);
    journal.started_unix = crate::util::now_unix().unwrap() - 25 * 3600;
    journal.write(&case).unwrap();
    let reserved = case.join("reports").join(&journal.package_name);
    let state = bound_state(&case);
    let response = parse(&finalize_with_exe(&bin, &case, "홍길동", &state));
    assert_eq!(response["ok"], true, "{response}");
    assert!(reserved.is_dir(), "never renamed");
    assert!(
        crate::package::finalize_incomplete_marker(&reserved).is_file(),
        "marked beside"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S9: a closed journal shows the lane's `outcome`, and no dismiss
/// command (R14-S5: only where dismissing is the way out).
#[test]
fn r14_s9_closed_journal_shows_its_outcome() {
    let Some((root, case)) = finalize_case("s9-outcome") else {
        return;
    };
    let bin = cli_bin().unwrap();
    finalize_ok(&bin, &case, "홍길동");
    journal_from_begin(&last_begin(&case)).write(&case).unwrap();
    let shown = parse(&api_status(&bound_state(&case)))["finalize_journal"].clone();
    assert_eq!(shown["state"], "completed", "{shown}");
    assert_eq!(shown["outcome"], "completed", "{shown}");
    assert_eq!(shown["dismiss_command"], serde_json::Value::Null, "{shown}");
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S10: a recovery thread that panics still frees `busy` and the
/// `recovering` status. 40126c3 left status `recovering` for good.
#[test]
fn r14_s10_panicking_startup_recovery_releases_everything() {
    let root = scratch("s10-panic");
    let case = root.join("case");
    std::fs::create_dir_all(case.join("review")).unwrap();
    std::fs::write(super::finalize_journal::journal_path(&case), b"{}").unwrap();
    let state = bound_state(&case);
    *super::finalize_journal::PANIC_IN_RECOVERY.lock().unwrap() = Some(case.clone());
    let worker = super::finalize_journal::spawn_bound_case_recovery(&state).unwrap();
    worker.join().unwrap();
    *super::finalize_journal::PANIC_IN_RECOVERY.lock().unwrap() = None;
    assert!(!state_lock(&state).busy);
    assert!(state_lock(&state).finalize_recovering.is_none());
    let status = parse(&api_status(&state));
    assert_ne!(
        status["finalize_journal"]["state"], "recovering",
        "{status}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S11: the dismiss command is single-quoted (PowerShell expands
/// `$(…)` inside the double quotes 40126c3 used), with a cmd.exe form on
/// Windows.
#[test]
fn r14_s11_dismiss_command_is_single_quoted() {
    let case = Path::new("C:/cases/it's $(calc)");
    let command = super::finalize_journal::dismiss_command(case);
    if cfg!(windows) {
        assert_eq!(
            command,
            "frametrace finalize-journal 'C:/cases/it''s $(calc)' --dismiss"
        );
        assert_eq!(
            super::finalize_journal::dismiss_command_cmd(case).unwrap(),
            "frametrace finalize-journal \"C:/cases/it's $(calc)\" --dismiss"
        );
    } else {
        assert_eq!(
            command,
            "frametrace finalize-journal 'C:/cases/it'\\''s $(calc)' --dismiss"
        );
        assert!(super::finalize_journal::dismiss_command_cmd(case).is_none());
    }
}

/// R14-S12: the status cache follows the audit key set. 40126c3 kept the
/// pairing judged under the old keys.
#[test]
fn r14_s12_status_cache_follows_the_key_set() {
    let Some((root, case)) = finalize_case("s12-keys") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let state = bound_state(&case);
    keyless(|| {
        finalize_ok(&bin, &case, "홍길동");
        interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Promotion);
        assert_eq!(
            parse(&api_status(&state))["finalize_journal"]["state"],
            "interrupted"
        );
    });
    let other = crate::audit_key::AuditKey::new("another-pc", &[0x11; 32]);
    crate::audit_key::with_test_keys(&[other], || {
        assert_eq!(
            parse(&api_status(&state))["finalize_journal"]["state"],
            "unpaired"
        );
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R14-S13: the start-up sweep removes local records older than 30 days
/// that stand for nothing left to recover, and keeps the others.
#[test]
fn r14_s13_sweep_expires_stale_local_records() {
    let root = scratch("s13-sweep");
    let case = root.join("case");
    std::fs::create_dir_all(case.join("review")).unwrap();
    let now = crate::util::now_unix().unwrap();
    let old = now - 40 * 24 * 3600;
    let record = |id: char, state: &str, updated: u64| super::finalize_journal::LocalRecord {
        finalize_id: id.to_string().repeat(32),
        case_dir: case.to_string_lossy().to_string(),
        signoff_sha256: "1".repeat(64),
        previous_signoff_sha256: None,
        package_name: "package_1".into(),
        host_id: crate::util::host_id(),
        started_unix: updated,
        state: state.into(),
        updated_unix: updated,
    };
    let completed_old = record(
        '1',
        super::finalize_journal::STATE_COMPLETED_UNRECORDED,
        old,
    );
    let orphan_old = record('2', super::finalize_journal::LOCAL_OPEN, old);
    let open_old_with_journal = record('3', super::finalize_journal::LOCAL_OPEN, old);
    let open_fresh = record('4', super::finalize_journal::LOCAL_OPEN, now);
    for record in [
        &completed_old,
        &orphan_old,
        &open_old_with_journal,
        &open_fresh,
    ] {
        record.write().unwrap();
    }
    super::finalize_journal::FinalizeJournal {
        finalize_id: open_old_with_journal.finalize_id.clone(),
        signoff_sha256: "1".repeat(64),
        state: super::finalize_journal::STATE_IN_PROGRESS.into(),
        ..Default::default()
    }
    .write(&case)
    .unwrap();
    super::finalize_journal::sweep_local_records();
    assert!(local_record(&completed_old.finalize_id).is_none());
    assert!(local_record(&orphan_old.finalize_id).is_none());
    assert!(local_record(&open_old_with_journal.finalize_id).is_some());
    assert!(local_record(&open_fresh.finalize_id).is_some());
    for record in [&open_old_with_journal, &open_fresh] {
        super::finalize_journal::remove_local_record(&record.finalize_id).unwrap();
    }
    let _ = std::fs::remove_dir_all(root);
}
