//! Round-12 track S (docs/PLAN_R12_2026-10-16.md): a cancel never leaves a
//! plainly named package of an unfinished finalize (S1, P14), the finalize
//! journal makes an interrupted finalize recoverable (S2/S3, P13), cleanup
//! steps are not cancellable (S4), a kept work folder is marked (S5), an
//! abandoned "previous" sign-off is not restored (S6).
//!
//! Each scenario fails on 632f557 (the code before the fix) — the tests
//! that use a new hook or function do not compile there (R8 convention);
//! what the old code did is noted on each test.

use super::*;

// Included per test file (no mod.rs line needed); see the helper's header.
#[allow(clippy::duplicate_mod)]
#[path = "tests_fake_tools.rs"]
mod fake_tools;
use fake_tools::write_fake;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r12s-{tag}-{}", std::process::id()));
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
    std::fs::write(src.join("a.mp4"), b"fixture-video-bytes-r12").unwrap();
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

/// A bare case for the fake CLI (no binary needed).
fn fake_case(tag: &str) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let case = root.join("case");
    std::fs::create_dir_all(case.join("db")).unwrap();
    std::fs::create_dir_all(case.join("review")).unwrap();
    std::fs::create_dir_all(case.join("reports")).unwrap();
    std::fs::write(case.join("case.json"), br#"{"case_id":"FT-r12"}"#).unwrap();
    (root, case)
}

fn bound_state(case: &Path) -> SharedState {
    // R13-S2: the fakes publish `package_1`, so the finalize reserves it.
    super::jobs::PACKAGE_BASE_FOR_TEST.with(|cell| cell.set(Some("package_1")));
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.to_path_buf());
    state
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("invalid JSON {err}: {text}"))
}

/// A fake CLI logging every call; its package-case publishes
/// `reports/package_1` (manifest + `output:` line, as the real one), then
/// waits about `wait_secs` and exits `code`.
fn publishing_fake(root: &Path, log: &Path, wait_secs: u32, code: u32) -> PathBuf {
    let unix_wait = if wait_secs > 0 {
        format!(" sleep {wait_secs};")
    } else {
        String::new()
    };
    let cmd_wait = if wait_secs > 0 {
        format!("\n{}", fake_tools::cmd_sleep(wait_secs))
    } else {
        String::new()
    };
    write_fake(
        root,
        &format!(
            "echo \"$*\" >> \"{log}\"\ncase \"$1\" in package-case) mkdir -p \"$2/reports/package_1\" && echo x > \"$2/reports/package_1/manifest.sha256\" && echo \"output: $2/reports/package_1\";{unix_wait} exit {code};; esac\nexit 0",
            log = log.display()
        ),
        &format!(
            "echo %*>> \"{log}\"\nif \"%1\"==\"package-case\" (\nmkdir \"%~2\\reports\\package_1\"\necho x> \"%~2\\reports\\package_1\\manifest.sha256\"\necho output: %~2\\reports\\package_1{cmd_wait}\nexit /b {code}\n)\nexit /b 0",
            log = log.display()
        ),
    )
}

fn logged_calls(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
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

fn lane_text(case: &Path) -> String {
    std::fs::read_to_string(case.join(crate::report::SIGNOFF_LOG_REL_PATH)).unwrap_or_default()
}

/// Runs the finalize on another thread (`setup` runs there first — the
/// thread-local hooks) and, once `reports/package_1/manifest.sha256`
/// exists (package-case published), cancels it the way the UI does —
/// `/api/cancel` while the job holds `busy` (the shutdown dialog's "작업
/// 중단 후 서버 종료" sends exactly that).
fn finalize_cancelled_after_publish(
    fake: PathBuf,
    case: &Path,
    state: &SharedState,
    setup: fn(),
) -> serde_json::Value {
    state_lock(state).busy = true;
    let worker = {
        let state = Arc::clone(state);
        let case = case.to_path_buf();
        thread::spawn(move || {
            super::jobs::PACKAGE_BASE_FOR_TEST.with(|cell| cell.set(Some("package_1")));
            setup();
            let response = finalize_with_exe(&fake, &case, "홍길동", &state);
            super::jobs::CANCEL_IGNORES_PUBLISH.with(|hook| hook.set(false));
            response
        })
    };
    let published = case.join("reports/package_1/manifest.sha256");
    let started = std::time::Instant::now();
    while !published.is_file() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "package-case never published"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let cancelled = parse(&api_cancel(state));
    assert_eq!(cancelled["ok"], true, "{cancelled}");
    let response = parse(&worker.join().unwrap());
    state_lock(state).busy = false;
    response
}

/// R12-S1 (P14): a cancel arriving after package-case published its
/// package is deferred — the package step runs to its end and the
/// finalize completes with that package. On 632f557 the cancel killed the
/// step after the publish rename: the finalize failed, abandoned the
/// sign-off and took it back, and the plainly named `package_1` stayed —
/// a package that verifies standalone for a finalize that "failed".
#[test]
fn r12_s1_cancel_after_publish_is_deferred_and_the_finalize_completes() {
    let (root, case) = fake_case("s1-defer");
    let log = root.join("calls.log");
    let fake = publishing_fake(&root, &log, 3, 0);
    let state = bound_state(&case);
    let response = finalize_cancelled_after_publish(fake, &case, &state, || {});
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["package_url"].as_str(), Some("reports/package_1"));
    assert_eq!(package_entries(&case), vec!["package_1".to_string()]);
    assert!(case.join(crate::report::SIGNOFF_REL_PATH).is_file());
    assert!(
        !lane_text(&case).contains(crate::report::SIGNOFF_ABANDONED_EVENT),
        "the completed finalize's sign-off was abandoned"
    );
    assert!(
        !super::finalize_journal::journal_path(&case).exists(),
        "the journal of a completed finalize stays"
    );
    let logs = state_lock(&state).logs.join("\n");
    assert!(logs.contains("패키지가 이미 게시되어"), "{logs}");
    let _ = std::fs::remove_dir_all(root);
}

/// R12-S1 (P14): package-case exited 0 and the cancel came in the polling
/// gap before the post-exit check. 632f557 reported it as cancelled
/// (finalize failed, plain package left).
#[test]
fn r12_s1_cancel_in_the_polling_gap_after_a_clean_exit_is_ignored() {
    let (root, case) = fake_case("s1-gap");
    let log = root.join("calls.log");
    let fake = publishing_fake(&root, &log, 0, 0);
    let state = bound_state(&case);
    super::jobs::CANCEL_AFTER_EXIT.with(|hook| hook.set(Some("package-case")));
    let response = parse(&finalize_with_exe(&fake, &case, "홍길동", &state));
    super::jobs::CANCEL_AFTER_EXIT.with(|hook| hook.set(None));
    assert!(state_lock(&state).cancel_requested, "the hook fired");
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(package_entries(&case), vec!["package_1".to_string()]);
    assert!(case.join(crate::report::SIGNOFF_REL_PATH).is_file());
    let _ = std::fs::remove_dir_all(root);
}

/// R12-S1 (P14) + S4: the kill still lands after the publish rename (the
/// published check and the kill are not atomic — forced here). The
/// failure path finds the package (new `reports/package_*` / its `output:`
/// line), renames it `package_1_INCOMPLETE` with the marker beside it, and
/// the re-render of the report — a cleanup step — is not killed by the
/// cancel flag that is still set. 632f557: `package_1` stayed plain, and
/// the re-render was killed at its first poll (report left stale).
#[test]
fn r12_s1_kill_after_publish_marks_the_package_incomplete() {
    let (root, case) = fake_case("s1-kill");
    let log = root.join("calls.log");
    let fake = publishing_fake(&root, &log, 30, 0);
    let state = bound_state(&case);
    let response = finalize_cancelled_after_publish(fake, &case, &state, || {
        super::jobs::CANCEL_IGNORES_PUBLISH.with(|hook| hook.set(true));
    });
    assert_eq!(response["ok"], false, "{response}");
    let error = response["error"].as_str().unwrap();
    assert!(error.contains("사용자가 분석을 중단했습니다"), "{error}");
    assert!(error.contains("이미"), "{error}");
    assert_eq!(
        package_entries(&case),
        vec![
            "package_1_INCOMPLETE".to_string(),
            "package_1_INCOMPLETE.finalize-incomplete.txt".to_string()
        ]
    );
    let renamed = case.join("reports").join("package_1_INCOMPLETE");
    assert!(crate::package::finalize_incomplete_reason(&renamed).is_some());
    assert_eq!(
        response["incomplete_package_dirs"],
        serde_json::json!([renamed.to_string_lossy()])
    );
    assert!(lane_text(&case).contains(crate::report::SIGNOFF_ABANDONED_EVENT));
    assert!(!case.join(crate::report::SIGNOFF_REL_PATH).exists());
    // S4: the cleanup re-render ran to its end despite the cancel flag.
    let calls = logged_calls(&log);
    let last = calls.last().unwrap();
    assert!(
        last.starts_with("make-report") && !last.contains("--signoff"),
        "{calls:?}"
    );
    assert_eq!(response["report_stale"], false, "{response}");
    let status = parse(&api_status(&state));
    assert_eq!(status["finalize_incomplete"], true, "{status}");
    assert_eq!(
        status["incomplete_packages"],
        serde_json::json!([renamed.to_string_lossy()])
    );
    assert_eq!(status["finalize_journal"], serde_json::Value::Null);
    assert!(!super::finalize_journal::journal_path(&case).exists());
    let _ = std::fs::remove_dir_all(root);
}

/// R12-S1: package-case published and then exited non-zero (a timeout or
/// a kill from outside looks the same). 632f557 left `package_1` plain.
#[test]
fn r12_s1_failure_after_publish_marks_the_package_incomplete() {
    let (root, case) = fake_case("s1-fail");
    let log = root.join("calls.log");
    let fake = publishing_fake(&root, &log, 0, 1);
    let state = bound_state(&case);
    let response = parse(&finalize_with_exe(&fake, &case, "홍길동", &state));
    assert_eq!(response["ok"], false, "{response}");
    assert!(!case.join("reports/package_1").exists());
    assert!(
        crate::package::finalize_incomplete_reason(&case.join("reports/package_1_INCOMPLETE"))
            .is_some()
    );
    assert_eq!(response["finalize_incomplete"], true, "{response}");
    let _ = std::fs::remove_dir_all(root);
}

/// R12-S5: package-case kept a complete package under its work-folder
/// name (publish rename failed) with the note "rename it yourself". A
/// finalize failing that way marks it beside under both names, so the
/// renamed folder still reads as incomplete, and rewrites the note.
/// 632f557: renaming it to its reserved name gave a package that verified as final.
#[test]
fn r12_s5_kept_work_folder_of_a_failed_finalize_is_marked() {
    let (root, case) = fake_case("s5-part");
    let part = ".package_1.ft999.part";
    let keep = crate::join::PRESERVED_PART_SUFFIX;
    let fake = write_fake(
        &root,
        &format!(
            "case \"$1\" in package-case) mkdir -p \"$2/reports/{part}\" && echo x > \"$2/reports/{part}/manifest.sha256\" && echo keep > \"$2/reports/{part}{keep}\"; echo 'rename failed' >&2; exit 1;; esac\nexit 0"
        ),
        &format!(
            "if \"%1\"==\"package-case\" (\nmkdir \"%~2\\reports\\{part}\"\necho x> \"%~2\\reports\\{part}\\manifest.sha256\"\necho keep> \"%~2\\reports\\{part}{keep}\"\necho rename failed 1>&2\nexit /b 1\n)\nexit /b 0"
        ),
    );
    let state = bound_state(&case);
    let response = parse(&finalize_with_exe(&fake, &case, "홍길동", &state));
    assert_eq!(response["ok"], false, "{response}");
    let reports = case.join("reports");
    let kept = reports.join(part);
    assert!(crate::package::finalize_incomplete_reason(&kept).is_some());
    let note = std::fs::read_to_string(crate::join::preserved_part_marker(&kept)).unwrap();
    assert!(note.contains("최종화 미완료"), "{note}");
    assert!(
        crate::join::part_is_preserved(&kept),
        "still kept from sweeps"
    );
    // The examiner renames it as the old note said: still incomplete.
    let renamed = reports.join("package_1");
    std::fs::rename(&kept, &renamed).unwrap();
    assert!(crate::package::finalize_incomplete_reason(&renamed).is_some());
    let _ = std::fs::remove_dir_all(root);
}

/// A real-CLI finalize that completes (examiner 홍길동); returns the
/// sign-off bytes.
fn first_finalize(bin: &Path, case: &Path) -> Vec<u8> {
    let state = bound_state(case);
    let response = parse(&finalize_with_exe(bin, case, "홍길동", &state));
    assert_eq!(response["ok"], true, "{response}");
    std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap()
}

/// A second finalize (김철수) that "dies" at `stop`; returns its sign-off
/// SHA-256 (from the journal it leaves).
fn interrupted_finalize(bin: &Path, case: &Path, stop: super::jobs::FinalizeStop) -> String {
    let state = bound_state(case);
    super::jobs::STOP_FINALIZE_AT.with(|cell| cell.set(Some(stop)));
    let response = finalize_with_exe(bin, case, "김철수", &state);
    super::jobs::STOP_FINALIZE_AT.with(|cell| cell.set(None));
    assert!(response.contains("test: stopped"), "{response}");
    super::finalize_journal::read_journal(case)
        .unwrap()
        .expect("the journal is on disk")
        .signoff_sha256()
}

fn abandoned_in_lane(case: &Path, sha: &str) -> bool {
    lane_text(case)
        .lines()
        .any(|line| line.contains(crate::report::SIGNOFF_ABANDONED_EVENT) && line.contains(sha))
}

/// R12-S2/S3 (P13, real CLI): the workstation died between the sign-off
/// promotion and package-case. On restart the journal is found: the
/// previous sign-off is restored byte for byte from the journal, the
/// attempt is abandoned in the chain, and the report still rendered from
/// it is reported stale (journal kept until a finalize re-renders it).
/// 632f557 left no clue: the promoted sign-off stayed current.
#[test]
fn r12_s2_crash_after_promotion_is_recovered_at_start() {
    let Some((root, case)) = finalize_case("s2-promoted") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let signed = first_finalize(&bin, &case);
    let packages = package_entries(&case);
    let sha = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Promotion);
    let promoted = std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap();
    assert_eq!(crate::sha256::digest_bytes(&promoted), sha, "promoted");
    // "Restart": a fresh server state bound to the case.
    let state = bound_state(&case);
    super::finalize_journal::recover_bound_case(&state);
    assert_eq!(
        std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap(),
        signed,
        "the previous sign-off is restored"
    );
    assert!(abandoned_in_lane(&case, &sha), "{}", lane_text(&case));
    assert_eq!(package_entries(&case), packages, "no package added");
    let status = parse(&api_status(&state));
    assert_eq!(status["finalize_recovered"], true, "{status}");
    let detail = status["finalize_recovered_detail"].as_str().unwrap();
    assert!(detail.contains("되돌렸습니다"), "{detail}");
    assert!(detail.contains("case-report.html"), "{detail}");
    assert_eq!(status["report_stale"], true, "{status}");
    assert_eq!(status["finalize_journal"]["state"], "recovered", "{status}");
    assert_eq!(status["finalize_journal"]["signoff_sha256"], sha.as_str());
    // A second recovery is a no-op (no second abandon record).
    let abandons = lane_text(&case)
        .matches(crate::report::SIGNOFF_ABANDONED_EVENT)
        .count();
    super::finalize_journal::recover_bound_case(&bound_state(&case));
    assert_eq!(
        lane_text(&case)
            .matches(crate::report::SIGNOFF_ABANDONED_EVENT)
            .count(),
        abandons
    );
    // The next finalize completes and clears everything.
    let response = parse(&finalize_with_exe(&bin, &case, "홍길동", &state));
    assert_eq!(response["ok"], true, "{response}");
    let status = parse(&api_status(&state));
    assert_eq!(status["finalize_recovered"], false, "{status}");
    assert_eq!(status["report_stale"], false, "{status}");
    assert_eq!(status["finalize_journal"], serde_json::Value::Null);
    let _ = std::fs::remove_dir_all(root);
}

/// R12-S2 (P13 + P14, real CLI): the workstation died after package-case
/// published its package. Opening the case finds the journal: the package
/// is renamed `…_INCOMPLETE` (verify-package: FINALIZE INCOMPLETE), the
/// previous sign-off restored, the attempt abandoned. 632f557: a plain
/// package that verified, its sign-off current.
#[test]
fn r12_s2_crash_after_publish_is_recovered_on_open_case() {
    let Some((root, case)) = finalize_case("s2-published") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let signed = first_finalize(&bin, &case);
    let before = package_entries(&case);
    let sha = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Package);
    let after = package_entries(&case);
    let published: Vec<&String> = after.iter().filter(|n| !before.contains(n)).collect();
    assert_eq!(published.len(), 1, "{after:?}");
    let published = case.join("reports").join(published[0]);
    assert!(crate::package::verify_package(&published).unwrap().passed());
    // R14 (P19): a published package that carries the promoted sign-off
    // with a complete manifest is a FINISHED finalize (see
    // `r14_p19_crash_after_publish_is_a_finished_finalize`); this test
    // keeps the interrupted path with a package whose manifest names a
    // file that is missing (the publish did not finish).
    std::fs::remove_file(published.join("README.txt")).unwrap();
    // Another server state opens the case.
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let request = Request {
        method: "POST".into(),
        path: "/api/open-case".into(),
        query: String::new(),
        body: serde_json::json!({"case_dir": case.to_string_lossy()}).to_string(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    };
    let opened = parse(&api_open_case(&request, &state));
    assert_eq!(opened["ok"], true, "{opened}");
    assert!(!published.exists(), "the published package was renamed");
    let renamed = published.with_file_name(format!(
        "{}_INCOMPLETE",
        published.file_name().unwrap().to_string_lossy()
    ));
    let checked = crate::package::verify_package(&renamed).unwrap();
    assert!(checked.finalize_incomplete, "{:?}", checked.checks);
    assert_eq!(
        std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap(),
        signed
    );
    assert!(abandoned_in_lane(&case, &sha));
    let status = parse(&api_status(&state));
    assert_eq!(status["finalize_recovered"], true, "{status}");
    assert!(
        status["finalize_recovered_detail"]
            .as_str()
            .unwrap()
            .contains("_INCOMPLETE"),
        "{status}"
    );
    assert_eq!(
        status["incomplete_packages"],
        serde_json::json!([renamed.to_string_lossy()])
    );
    let _ = std::fs::remove_dir_all(root);
}

/// R12-S2 (real CLI): died right after the sign-off was chained (before
/// the report and the promotion). Recovery abandons the chained entry,
/// removes the pending record, leaves `review/signoff.json` alone, and —
/// nothing else being left — removes the journal.
#[test]
fn r12_s2_crash_before_promotion_leaves_the_signoff_alone() {
    let Some((root, case)) = finalize_case("s2-chained") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let signed = first_finalize(&bin, &case);
    let sha = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Chain);
    assert!(case.join(SIGNOFF_PENDING_REL_PATH).is_file());
    let state = bound_state(&case);
    super::finalize_journal::recover_bound_case(&state);
    assert!(abandoned_in_lane(&case, &sha));
    assert!(!case.join(SIGNOFF_PENDING_REL_PATH).exists());
    assert_eq!(
        std::fs::read(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap(),
        signed
    );
    assert!(!super::finalize_journal::journal_path(&case).exists());
    let status = parse(&api_status(&state));
    assert_eq!(status["finalize_recovered"], true, "{status}");
    assert_eq!(status["report_stale"], false, "{status}");
    assert_eq!(status["finalize_journal"], serde_json::Value::Null);
    let _ = std::fs::remove_dir_all(root);
}

/// A fake that runs the real `bin` except package-case, which runs
/// `package_sh` / `package_cmd` and fails.
fn delegating_fake(root: &Path, bin: &Path, package_sh: &str, package_cmd: &str) -> PathBuf {
    write_fake(
        root,
        &format!(
            "case \"$1\" in package-case) {package_sh} echo 'package disk full' >&2; exit 1;; esac\nexec \"{}\" \"$@\"",
            bin.display()
        ),
        &format!(
            "if \"%1\"==\"package-case\" (\n{package_cmd}\necho package disk full 1>&2\nexit /b 1\n)\n\"{}\" %*\nexit /b %errorlevel%",
            bin.display()
        ),
    )
}

/// R12-S6 (real CLI): `review/signoff.json` holds a sign-off an earlier
/// finalize abandoned (its restore had failed). A finalize that fails now
/// does not "restore" it as if confirmed: it is removed and the error and
/// `finalize_incomplete` say so. 632f557 restored it and reported nothing
/// left over (the earlier warning was gone).
#[test]
fn r12_s6_abandoned_previous_signoff_is_not_restored() {
    let Some((root, case)) = finalize_case("s6-abandoned") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let old = crate::report::Signoff::build_v4(&case, "이전검토자", 1_700_000_000).unwrap();
    std::fs::write(case.join(crate::report::SIGNOFF_REL_PATH), &old.text).unwrap();
    crate::report::record_signoff_audit(&case, &old).unwrap();
    crate::report::record_signoff_abandoned(&case, &old.text, "earlier finalize failed").unwrap();
    let fake = delegating_fake(&root, &bin, "", "");
    let state = bound_state(&case);
    let response = parse(&finalize_with_exe(&fake, &case, "홍길동", &state));
    assert_eq!(response["ok"], false, "{response}");
    let error = response["error"].as_str().unwrap();
    assert!(error.contains("되돌리지 않았습니다"), "{error}");
    assert!(!case.join(crate::report::SIGNOFF_REL_PATH).exists());
    assert_eq!(response["finalize_incomplete"], true, "{response}");
    let status = parse(&api_status(&state));
    assert!(
        status["finalize_incomplete_detail"]
            .as_str()
            .unwrap()
            .contains("되돌리지 않았습니다"),
        "{status}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// R12-S2/S7 (Windows, real CLI): taking the promoted sign-off back fails
/// (another program holds it without delete sharing). The journal stays;
/// once the handle is gone, the next start's recovery removes the
/// promoted record. 632f557 kept that state in memory only — after a
/// restart nothing knew the sign-off had to be taken back.
#[cfg(windows)]
#[test]
fn r12_s2_restore_failure_is_retried_by_the_recovery() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some((root, case)) = finalize_case("s2-restore") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let gate = root.join("held.gate");
    let fake = delegating_fake(&root, &bin, "", &fake_tools::cmd_wait_for_file(&gate));
    let target = case.join(crate::report::SIGNOFF_REL_PATH);
    let release = Arc::new(AtomicBool::new(false));
    let holder = {
        let target = target.clone();
        let gate = gate.clone();
        let release = Arc::clone(&release);
        thread::spawn(move || {
            let started = std::time::Instant::now();
            while started.elapsed() < Duration::from_secs(120) {
                // FILE_SHARE_READ | FILE_SHARE_WRITE: no delete / rename.
                if let Ok(file) = std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(3)
                    .open(&target)
                {
                    std::fs::write(&gate, b"held").unwrap();
                    while !release.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(20));
                    }
                    drop(file);
                    return true;
                }
                thread::sleep(Duration::from_millis(2));
            }
            false
        })
    };
    let state = bound_state(&case);
    crate::package::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(Some(Duration::ZERO)));
    let response = parse(&finalize_with_exe(&fake, &case, "홍길동", &state));
    crate::package::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(None));
    release.store(true, Ordering::SeqCst);
    assert!(holder.join().unwrap(), "the holder never saw the sign-off");
    assert_eq!(response["ok"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("되돌리지 못함"),
        "{response}"
    );
    let journal = super::finalize_journal::read_journal(&case)
        .unwrap()
        .expect("the journal stays while the sign-off is not taken back");
    assert!(target.is_file(), "the promoted record is still there");
    assert_eq!(journal.state, super::finalize_journal::STATE_INTERRUPTED);
    let status = parse(&api_status(&state));
    assert_eq!(
        status["finalize_journal"]["state"], "interrupted",
        "{status}"
    );
    // Restart: the recovery takes it back now.
    let restarted = bound_state(&case);
    super::finalize_journal::recover_bound_case(&restarted);
    assert!(
        !target.exists(),
        "no previous record: the promoted one goes"
    );
    assert!(abandoned_in_lane(&case, &journal.signoff_sha256()));
    let status = parse(&api_status(&restarted));
    assert!(
        status["finalize_recovered_detail"]
            .as_str()
            .unwrap()
            .contains("되돌렸습니다"),
        "{status}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// R12-S2/S3 (R13 format): the journal round-trips exactly, and a journal
/// that is not one (here: its SHA field is no SHA-256) is refused (never
/// recovered on a guess). R13-S11: the previous bytes live beside it.
#[test]
fn r12_s2_journal_round_trips_and_refuses_a_mismatched_record() {
    let (root, case) = fake_case("s2-journal");
    let journal = super::finalize_journal::FinalizeJournal {
        finalize_id: "00112233445566778899aabbccddeeff".into(),
        host_id: crate::util::host_id(),
        writer: crate::util::WriterIdentity {
            pid: 4242,
            started_ms: Some(1_700_000_000_123),
            image: Some("frametrace-app.exe".into()),
        },
        writer_image_sha256: None,
        started_unix: 1_700_000_000,
        examiner: "홍길동".into(),
        signoff_sha256: crate::sha256::digest_bytes(b"{\"examiner\":\"x\"}"),
        previous_signoff_sha256: Some(crate::sha256::digest_bytes(&[0, 159, 255])),
        package_name: "package_1700000000".into(),
        state: super::finalize_journal::STATE_RECOVERED.to_string(),
    };
    journal.write(&case).unwrap();
    assert_eq!(
        super::finalize_journal::read_journal(&case).unwrap(),
        Some(journal.clone())
    );
    let path = super::finalize_journal::journal_path(&case);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace(&journal.signoff_sha256, "not-a-sha")).unwrap();
    assert!(super::finalize_journal::read_journal(&case).is_err());
    // An unreadable journal blocks the next finalize instead of being
    // overwritten.
    let state = bound_state(&case);
    let refused = parse(&finalize_with_exe(
        Path::new("unused"),
        &case,
        "홍길동",
        &state,
    ));
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("이전에 중단된 최종화"),
        "{refused}"
    );
    assert!(path.is_file(), "kept");
    let _ = std::fs::remove_dir_all(root);
}
