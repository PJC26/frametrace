//! Round-9 track S (docs/PLAN_R9_2026-10-15.md): verified streaming that
//! compares every chunk before sending it (S1), finalize error texts when
//! the package is already published (S4) or the abandon record fails (S5).
//! Each test fails on the code before the fix (or, for a new hook, does
//! not compile there).
//!
//! Round-10 track S (docs/PLAN_R10_2026-10-15.md) adds the `r10_*` tests:
//! sign-off promotion retried (S1), an incomplete finalize's package
//! renamed `…_INCOMPLETE` and reported (S2) — the one package-case named
//! (S3) —, the real step binary keeping its token from its grandchildren
//! (S5), a skip instead of a panic without the CLI binary (S6) and the
//! verified-stream transfer deadline (S9).

use super::http::{
    IN_MEMORY_VERIFY_MAX, STREAM_CHUNK_HOOK, ServeVerdict, VERIFY_CHUNK, VerifiedBody,
    stream_verified, verify_for_serving,
};
use super::*;

// Included per test file (no mod.rs line needed); see the helper's header.
#[allow(clippy::duplicate_mod)]
#[path = "tests_fake_tools.rs"]
mod fake_tools;
use fake_tools::write_fake;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r9s-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn temp_case(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::create_dir_all(dir.join("review")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-r9"}"#).unwrap();
    dir
}

/// A listed script of `chunks` full [`VERIFY_CHUNK`]s plus 7 bytes (well
/// above the in-memory limit), each chunk's bytes distinct.
fn big_script(case: &Path, chunks: usize) -> (PathBuf, Vec<u8>) {
    let size = chunks * VERIFY_CHUNK + 7;
    assert!(size as u64 > IN_MEMORY_VERIFY_MAX);
    let mut bytes = b"window.__FRAMETRACE_DATA__={pad:\"".to_vec();
    while bytes.len() < size - 3 {
        bytes.push(b'a' + (bytes.len() / VERIFY_CHUNK % 26) as u8);
    }
    bytes.extend_from_slice(b"\"};");
    let path = case.join("review").join("data-bundle.js");
    std::fs::write(&path, &bytes).unwrap();
    crate::util::generated_pages::record_generated_contents(
        case,
        &[("review/data-bundle.js", bytes.as_slice())],
    )
    .unwrap();
    (path.canonicalize().unwrap(), bytes)
}

/// Overwrites `at..at+patch.len()` of `path` in place (same size).
fn patch_in_place(path: &Path, at: usize, patch: &[u8]) {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::options().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(at as u64)).unwrap();
    file.write_all(patch).unwrap();
    file.sync_all().unwrap();
}

/// Serves `path` (already verified) through a real socket; returns the
/// serving result and every byte the client received.
fn serve_and_collect(
    path: &Path,
    verified: super::http::VerifiedStream,
) -> (Result<(), String>, Vec<u8>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let reader = thread::spawn(move || {
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut got = Vec::new();
        let _ = client.read_to_end(&mut got);
        got
    });
    let (mut server, _) = listener.accept().unwrap();
    let result = stream_verified(&mut server, path, verified);
    drop(server);
    (result, reader.join().unwrap())
}

fn verified_stream(case: &Path, path: &Path, len: usize) -> super::http::VerifiedStream {
    match verify_for_serving(case, path, len as u64, false, false) {
        ServeVerdict::Trusted {
            body: VerifiedBody::Stream(verified),
            ..
        } => verified,
        _ => panic!("a large listed script streams"),
    }
}

/// The client got a prefix of the ORIGINAL bytes only — not one altered
/// byte — and no more than the chunks before `changed_chunk`.
fn assert_only_original_prefix(got: &[u8], original: &[u8], changed_chunk: usize) {
    assert!(
        got.len() <= changed_chunk * VERIFY_CHUNK,
        "{} bytes reached the client; the change starts in chunk {changed_chunk}",
        got.len()
    );
    let differs = got.iter().zip(original).position(|(a, b)| a != b);
    assert_eq!(
        differs, None,
        "an altered byte reached the client at offset {differs:?}"
    );
}

// ---- S1: every chunk compared before it is sent ----

/// R9-S1: R8 hashed a large script or page in a first pass, then streamed
/// it in a second pass that re-hashed what it sent and held back only the
/// LAST chunk. A same-size rewrite of a middle chunk between the passes
/// was delivered (the cut came after it). Now each 64 KiB chunk is
/// compared with the first pass's hash of it before it is sent.
#[test]
fn r9_s1_rewrite_between_passes_sends_no_changed_byte() {
    let case = temp_case("s1-between");
    let (path, bytes) = big_script(&case, 70);
    let verified = verified_stream(&case, &path, bytes.len());
    let changed = 10;
    patch_in_place(&path, changed * VERIFY_CHUNK + 5, b"alert(1);//");
    let (result, got) = serve_and_collect(&path, verified);
    assert!(result.is_err(), "a changed file must not complete");
    assert!(!String::from_utf8_lossy(&got).contains("alert(1)"));
    assert_only_original_prefix(&got, &bytes, changed);
    // The verdict was dropped: the next request hashes and refuses it.
    assert!(matches!(
        verify_for_serving(&case, &path, bytes.len() as u64, false, false),
        ServeVerdict::Untrusted
    ));
    let _ = std::fs::remove_dir_all(case);
}

/// R9-S1: the rewrite happens DURING the transfer (after three chunks
/// went through), same size, in a later chunk and in the last one. No
/// altered byte reaches the client: the transfer is cut at the first
/// changed chunk.
#[test]
fn r9_s1_mid_transfer_same_size_rewrite_sends_no_changed_byte() {
    let case = temp_case("s1-mid");
    let (path, bytes) = big_script(&case, 70);
    let verified = verified_stream(&case, &path, bytes.len());
    let changed = 20;
    let target = path.clone();
    let total = bytes.len();
    STREAM_CHUNK_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |sent: usize| {
            if sent == 3 {
                patch_in_place(&target, changed * VERIFY_CHUNK, b"fetch('/api/finalize')");
                patch_in_place(&target, total - 10, b"alert(2);/");
            }
        }));
    });
    let (result, got) = serve_and_collect(&path, verified);
    STREAM_CHUNK_HOOK.with(|hook| *hook.borrow_mut() = None);
    assert!(result.is_err(), "a changed file must not complete");
    let text = String::from_utf8_lossy(&got);
    assert!(
        !text.contains("fetch('/api/finalize')"),
        "altered chunk sent"
    );
    assert!(!text.contains("alert(2)"));
    assert_only_original_prefix(&got, &bytes, changed);
    assert!(
        got.len() >= 2 * VERIFY_CHUNK,
        "unchanged chunks before the rewrite still flow ({} bytes)",
        got.len()
    );
    let _ = std::fs::remove_dir_all(case);
}

/// R9-S1: an unchanged file still streams exactly, and the per-chunk hash
/// list stays bounded (8192 chunks for the 512 MiB limit).
#[test]
fn r9_s1_unchanged_file_streams_exactly_with_bounded_chunk_list() {
    assert_eq!(super::http::MAX_VERIFY_CHUNKS, 8192);
    let case = temp_case("s1-same");
    let (path, bytes) = big_script(&case, 70);
    let verified = verified_stream(&case, &path, bytes.len());
    let (result, got) = serve_and_collect(&path, verified);
    assert!(result.is_ok(), "{result:?}");
    assert!(got == bytes, "body differs from the verified bytes");
    let _ = std::fs::remove_dir_all(case);
}

// ---- S4 / S5: finalize errors that say what happened ----

/// R10-S6: the real CLI binary in `dir` (`None` when it is not built).
fn cli_bin_in(dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "frametrace.exe"
    } else {
        "frametrace"
    };
    let bin = dir.join(name);
    bin.is_file().then_some(bin)
}

/// The real CLI binary next to the test executable. R10-S6: `None` — the
/// test is SKIPPED with a message — when it is not built: `cargo test
/// --lib` builds no binary, and R9's tests panicked there.
fn cli_bin() -> Option<PathBuf> {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let found = cli_bin_in(&dir);
    if found.is_none() {
        eprintln!(
            "SKIPPED: the frametrace binary is not built in {} (run `cargo test` or `cargo build` first)",
            dir.display()
        );
    }
    found
}

fn run_cli(bin: &Path, args: &[&str]) {
    let output = std::process::Command::new(bin).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "frametrace {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A case with one indexed source. Returns (root, case); `None` (skipped)
/// without the CLI binary.
fn finalize_case(tag: &str) -> Option<(PathBuf, PathBuf)> {
    let bin = cli_bin()?;
    let root = scratch(tag);
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.mp4"), b"fixture-video-bytes-r9").unwrap();
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

fn finalize_error(response: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(response).unwrap();
    assert_eq!(value["ok"], false, "{response}");
    value["error"].as_str().unwrap_or_default().to_string()
}

/// A fake CLI that logs every call (`%*`) to `log` and, for package-case,
/// publishes `reports/<package>` and prints its `output:` line as the
/// real one does; `decoy` (if any) is a second, NEWER package folder it
/// creates after it without naming it.
fn packaging_fake(root: &Path, log: &Path, package: &str, decoy: Option<&str>) -> PathBuf {
    let unix_decoy = decoy
        .map(|d| {
            format!(
                " && mkdir -p \"$2/reports/{d}\" && echo y > \"$2/reports/{d}/manifest.sha256\""
            )
        })
        .unwrap_or_default();
    let cmd_decoy = decoy
        .map(|d| {
            format!("\nmkdir \"%~2\\reports\\{d}\"\necho y> \"%~2\\reports\\{d}\\manifest.sha256\"")
        })
        .unwrap_or_default();
    write_fake(
        root,
        &format!(
            "echo \"$*\" >> \"{log}\"\ncase \"$1\" in package-case) mkdir -p \"$2/reports/{package}\" && echo x > \"$2/reports/{package}/manifest.sha256\"{unix_decoy} && echo \"output: $2/reports/{package}\";; esac\nexit 0",
            log = log.display()
        ),
        &format!(
            "echo %*>> \"{log}\"\nif \"%1\"==\"package-case\" (\nmkdir \"%~2\\reports\\{package}\"\necho x> \"%~2\\reports\\{package}\\manifest.sha256\"{cmd_decoy}\necho output: %~2\\reports\\{package}\n)\nexit /b 0",
            log = log.display()
        ),
    )
}

/// Calls the fake logged, one per line.
fn logged_calls(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// Makes promoting the sign-off fail for good: `review/signoff.json` is a
/// non-empty folder. The rename budget is shortened to the bare backoff
/// on this thread (finalize runs on it).
fn block_promotion(case: &Path) -> PathBuf {
    let blocker = case.join(crate::report::SIGNOFF_REL_PATH);
    std::fs::create_dir_all(&blocker).unwrap();
    std::fs::write(blocker.join("keep"), b"x").unwrap();
    crate::package::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(Some(Duration::ZERO)));
    blocker
}

fn unblock_promotion(blocker: &Path) {
    crate::package::RENAME_BUDGET_OVERRIDE.with(|cell| cell.set(None));
    std::fs::remove_dir_all(blocker).unwrap();
}

/// R9-S4: package-case published its package, then promoting the sign-off
/// failed. R8 answered only "검토자 기록 확정 실패" while a complete-looking
/// package sat in reports/. The error now names the package and says the
/// finalize is incomplete, and a marker beside the package says so too
/// (outside it: verify-package must still see exactly its files).
/// R10-S2/S3: the package is the one package-case named and is renamed
/// `…_INCOMPLETE` (the marker sits beside the renamed folder).
#[test]
fn r9_s4_published_package_with_failed_promotion_is_named_incomplete() {
    let Some((root, case)) = finalize_case("s4") else {
        return;
    };
    let fake = packaging_fake(&root, &root.join("calls.log"), "package_1", None);
    let blocker = block_promotion(&case);
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let error = finalize_error(&finalize_with_exe(&fake, &case, "홍길동", &state));
    unblock_promotion(&blocker);
    let package = case.join("reports").join("package_1_INCOMPLETE");
    assert!(package.is_dir(), "the published package, renamed");
    assert!(error.contains("검토자 기록 확정 실패"), "{error}");
    assert!(error.contains("최종화 미완료"), "{error}");
    assert!(
        error.contains(&package.display().to_string()),
        "the published package is not named: {error}"
    );
    let marker = super::jobs::finalize_incomplete_marker(&package);
    assert_eq!(marker.parent(), package.parent(), "beside, not inside");
    let text = std::fs::read_to_string(&marker).expect("incomplete marker");
    assert!(text.contains("최종화 미완료"), "{text}");
    assert!(!case.join(SIGNOFF_PENDING_REL_PATH).exists());
    let _ = std::fs::remove_dir_all(root);
}

/// R9-S5: the finalize failed and recording the abandoned sign-off failed
/// too. R8 only logged a warning line; the returned error did not say the
/// audit trail now shows the unfinished finalize's sign-off as the latest.
#[test]
fn r9_s5_failed_abandon_record_is_explained_in_the_error() {
    let Some((root, case)) = finalize_case("s5") else {
        return;
    };
    let fake = write_fake(
        &root,
        "case \"$1\" in package-case) rm -f \"$2/evidence/logs/signoff-log.jsonl\"; mkdir \"$2/evidence/logs/signoff-log.jsonl\"; echo 'disk full' >&2; exit 1;; esac\nexit 0",
        "if \"%1\"==\"package-case\" (\ndel /q \"%~2\\evidence\\logs\\signoff-log.jsonl\"\nmkdir \"%~2\\evidence\\logs\\signoff-log.jsonl\"\necho disk full 1>&2\nexit /b 1\n)\nexit /b 0",
    );
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let error = finalize_error(&finalize_with_exe(&fake, &case, "홍길동", &state));
    assert!(error.contains("disk full"), "{error}");
    assert!(
        error.contains("'완료되지 않음'으로 남기지 못했습니다"),
        "the failed abandon record is not explained: {error}"
    );
    assert!(error.contains("마지막 기록"), "{error}");
    assert!(!case.join(crate::report::SIGNOFF_REL_PATH).exists());
    let _ = std::fs::remove_dir_all(root);
}

// ---- R10 S1 / S2 / S3: an incomplete finalize, end to end ----

/// R10-S1: the sign-off promotion was a single rename — a handle an
/// antivirus or indexer held on the fresh pending file for a moment
/// failed a finalize whose package was already published. It is retried
/// within the rename budget now (injected failures: more than R9's one
/// attempt and more than the five-attempt backoff).
#[test]
fn r10_s1_promotion_is_retried_past_a_transient_failure() {
    let Some((root, case)) = finalize_case("r10-s1") else {
        return;
    };
    let fake = packaging_fake(&root, &root.join("calls.log"), "package_1", None);
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    super::jobs::FAIL_SIGNOFF_PROMOTIONS.with(|left| left.set(6));
    let response = finalize_with_exe(&fake, &case, "홍길동", &state);
    let left = super::jobs::FAIL_SIGNOFF_PROMOTIONS.with(|left| left.replace(0));
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["ok"], true, "{response}");
    assert_eq!(left, 0, "every injected failure was retried");
    assert!(case.join(crate::report::SIGNOFF_REL_PATH).is_file());
    assert!(case.join("reports").join("package_1").is_dir());
    let _ = std::fs::remove_dir_all(root);
}

/// R10-S1 (Windows, real handle): another process-like holder opens the
/// pending sign-off without delete sharing — what a real-time scan does —
/// for 2.5 s. R9 tried the promotion once and failed the finalize.
#[cfg(windows)]
#[test]
fn r10_s1_promotion_waits_out_a_real_sharing_violation() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some((root, case)) = finalize_case("r10-s1w") else {
        return;
    };
    let fake = packaging_fake(&root, &root.join("calls.log"), "package_1", None);
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let pending = case.join(SIGNOFF_PENDING_REL_PATH);
    let holder = thread::spawn(move || {
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(60) {
            // FILE_SHARE_READ only: no rename / delete while held.
            if let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(&pending)
            {
                thread::sleep(Duration::from_millis(2500));
                drop(file);
                return true;
            }
            thread::sleep(Duration::from_millis(2));
        }
        false
    });
    let response = finalize_with_exe(&fake, &case, "홍길동", &state);
    assert!(
        holder.join().unwrap(),
        "the holder never saw the pending file"
    );
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["ok"], true, "{response}");
    assert!(case.join(crate::report::SIGNOFF_REL_PATH).is_file());
    let _ = std::fs::remove_dir_all(root);
}

/// R10-S1/S2: a finalize that published its package and then could not
/// promote the sign-off.
/// - S2: R9 only wrote a marker BESIDE the package, which nothing read
///   and which a copy of the folder alone lost. The folder is renamed
///   `…_INCOMPLETE` now: verify-package says NOT VERIFIED (finalize
///   incomplete), the finalize result and /api/status name it, and it is
///   never "the" package. A later successful finalize names it again
///   (never deletes it).
/// - S1: the case report rendered with the abandoned sign-off is
///   rendered again (R9 re-rendered only after a failed report/package).
#[test]
fn r10_s2_incomplete_finalize_renames_and_reports_the_package() {
    let Some((root, case)) = finalize_case("r10-s2") else {
        return;
    };
    let log = root.join("calls.log");
    let fake = packaging_fake(&root, &log, "package_1", None);
    let blocker = block_promotion(&case);
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let response = finalize_with_exe(&fake, &case, "홍길동", &state);
    unblock_promotion(&blocker);
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["ok"], false, "{response}");
    let renamed = case.join("reports").join("package_1_INCOMPLETE");
    assert_eq!(value["finalize_incomplete"], true, "{response}");
    assert_eq!(
        value["incomplete_package_dir"].as_str(),
        Some(renamed.to_string_lossy().as_ref()),
        "{response}"
    );
    assert_eq!(
        value["incomplete_package_url"].as_str(),
        Some("reports/package_1_INCOMPLETE")
    );
    assert!(renamed.is_dir());
    assert!(!case.join("reports").join("package_1").exists());
    assert!(crate::package::finalize_incomplete_marker(&renamed).is_file());
    // verify-package: NOT VERIFIED "finalize", whatever else it finds.
    let checked = crate::package::verify_package(&renamed).unwrap();
    assert!(checked.finalize_incomplete);
    assert!(checked.checks.iter().any(|check| {
        check.subject == crate::package::FINALIZE_CHECK_SUBJECT
            && check.status == crate::package::CheckStatus::NotVerified
            && check.detail.contains("FINALIZE INCOMPLETE")
    }));
    assert_eq!(newest_package_dir(&case), None, "never the package");
    let status: serde_json::Value = serde_json::from_str(&api_status(&state)).unwrap();
    assert_eq!(
        status["incomplete_packages"][0].as_str(),
        Some(renamed.to_string_lossy().as_ref()),
        "{status}"
    );
    assert_eq!(status["package_incomplete"], true, "{status}");
    assert_eq!(
        status["package_dir"].as_str(),
        Some(renamed.to_string_lossy().as_ref())
    );
    // S1: the report was rendered again after the failed promotion,
    // without the abandoned sign-off.
    let calls = logged_calls(&log);
    let last = calls.last().expect("calls logged");
    assert!(
        last.starts_with("make-report") && !last.contains("--signoff"),
        "the report was not re-rendered: {calls:?}"
    );
    // A later finalize that completes: its own package, and the earlier
    // incomplete one named (kept).
    let fake = packaging_fake(&root, &log, "package_2", None);
    let response = finalize_with_exe(&fake, &case, "홍길동", &state);
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["ok"], true, "{response}");
    let second = case.join("reports").join("package_2");
    assert_eq!(
        value["package_dir"].as_str(),
        Some(second.to_string_lossy().as_ref())
    );
    assert_eq!(
        value["incomplete_packages"][0].as_str(),
        Some(renamed.to_string_lossy().as_ref()),
        "{response}"
    );
    assert!(renamed.is_dir(), "never deleted");
    let logs = state_lock(&state).logs.join("\n");
    assert!(logs.contains("미완료 패키지 1개"), "{logs}");
    let _ = std::fs::remove_dir_all(root);
}

/// R10-S3: the package to mark (and to report on success) is the one the
/// package-case step named on its `output:` line. R9 took the folder
/// with the newest modification time — here a NEWER folder the step did
/// not publish — so the decoy was marked and the real package passed as
/// final.
#[test]
fn r10_s3_the_marked_package_is_the_one_package_case_named() {
    let Some((root, case)) = finalize_case("r10-s3") else {
        return;
    };
    let log = root.join("calls.log");
    let fake = packaging_fake(&root, &log, "package_1", Some("package_9"));
    let blocker = block_promotion(&case);
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let error = finalize_error(&finalize_with_exe(&fake, &case, "홍길동", &state));
    unblock_promotion(&blocker);
    let reports = case.join("reports");
    assert!(reports.join("package_1_INCOMPLETE").is_dir(), "{error}");
    assert!(reports.join("package_9").is_dir(), "the decoy is untouched");
    assert!(!crate::package::finalize_incomplete_marker(&reports.join("package_9")).exists());
    assert!(error.contains("package_1_INCOMPLETE"), "{error}");
    // Success: the reported package is the named one, not the newest.
    let fake = packaging_fake(&root, &log, "package_3", Some("package_8"));
    let response = finalize_with_exe(&fake, &case, "홍길동", &state);
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["ok"], true, "{response}");
    assert_eq!(value["package_url"].as_str(), Some("reports/package_3"));
    assert_eq!(
        state_lock(&state).package_dir.as_deref(),
        Some(reports.join("package_3").as_path())
    );
    let _ = std::fs::remove_dir_all(root);
}

/// R10-S6: without the CLI binary the finalize tests skip with a message
/// (R9 panicked under `cargo test --lib`).
#[test]
fn r10_s6_missing_cli_binary_is_a_skip_not_a_panic() {
    let empty = scratch("r10-s6");
    assert_eq!(cli_bin_in(&empty), None);
    let _ = std::fs::remove_dir_all(empty);
}

// ---- S7 (R9) / S5 (R10): the step token stays with the step ----

/// R9-S7 / R10-S5: the CLI step kept its token in the environment, so
/// every tool it ran (ffmpeg, ffprobe, Sleuth Kit, python, ...) inherited
/// the secret that makes a stderr line a job warning; `main` now adopts
/// the token and removes the variable before anything runs.
///
/// R10-S5: R9's test only removed a probe variable from the TEST process
/// (a multithreaded `set_var`) and never ran the real binary. This runs
/// the real `frametrace` as a step — the token in its environment exactly
/// as `run_step` sets it, every variable set on the child only — and
/// lets it start a grandchild: the deepfake-lens sidecar (unix: a
/// `deepfake-lens` script; Windows: the `python -m deepfake_lens.cli`
/// fallback, whose module also runs `cmd /C echo %FRAMETRACE_STEP_WARNINGS%`).
/// The grandchild sees an ordinary variable of the step (so it would see
/// the token) but not the token.
#[test]
fn r10_s5_real_step_binary_keeps_the_token_from_its_grandchildren() {
    let Some(bin) = cli_bin() else {
        return;
    };
    let root = scratch("r10-s5");
    let media = root.join("clip.mp4");
    std::fs::write(&media, b"not really a video").unwrap();
    let token = "5e5e0123456789abcdef0123456789ab";
    let probe = "r10-s5-probe-visible";
    let mut command = std::process::Command::new(&bin);
    command
        .arg("deepfake-screen")
        .arg(&media)
        .env(crate::util::STEP_WARNING_ENV, token)
        .env("FRAMETRACE_R10_S5_PROBE", probe)
        .env_remove("PYTHONPATH");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = root.join("deepfake-lens");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '{\"verdict\":\"token=[%s] probe=[%s]\"}\\n' \"$FRAMETRACE_STEP_WARNINGS\" \"$FRAMETRACE_R10_S5_PROBE\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        command.env("FRAMETRACE_DEEPFAKE_LENS", &script);
    }
    #[cfg(windows)]
    {
        let package = root.join("py").join("deepfake_lens");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("__init__.py"), b"").unwrap();
        std::fs::write(
            package.join("cli.py"),
            "import json, os, subprocess\n\
             echoed = subprocess.run(['cmd', '/C', 'echo [%FRAMETRACE_STEP_WARNINGS%]'], capture_output=True, text=True).stdout.strip()\n\
             print(json.dumps({'verdict': 'token=[%s] probe=[%s] echo=%s' % (os.environ.get('FRAMETRACE_STEP_WARNINGS', ''), os.environ.get('FRAMETRACE_R10_S5_PROBE', ''), echoed)}))\n",
        )
        .unwrap();
        // A console script that does not exist: the python fallback runs.
        command
            .env(
                "FRAMETRACE_DEEPFAKE_LENS",
                root.join("missing").join("deepfake-lens.exe"),
            )
            .env("FRAMETRACE_DEEPFAKE_LENS_HOME", root.join("py"));
    }
    let output = command.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if cfg!(windows) && text.contains("python fallback also unavailable") {
        eprintln!("SKIPPED: no python on PATH for the deepfake-lens fallback grandchild: {text}");
        let _ = std::fs::remove_dir_all(root);
        return;
    }
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(&format!("probe=[{probe}]")),
        "the grandchild did not run or sees no step variable: {text}"
    );
    assert!(
        !text.contains(token),
        "the grandchild inherited the step token: {text}"
    );
    assert!(text.contains("token=[]"), "{text}");
    let _ = std::fs::remove_dir_all(root);
}

// ---- R10 S9: a slow client cannot pin a verified file ----

/// R10-S9: the verified stream wrote under a 60 s per-write timeout only,
/// so a client that stalls (or reads just enough to beat it) kept the
/// file handle open for as long as it liked — on Windows that blocks
/// regenerating or renaming the file. The transfer now has a deadline:
/// here a client that reads nothing for 6 s is cut within the (test)
/// deadline, the handle is released, and the client never gets the
/// whole file.
#[test]
fn r10_s9_stalled_client_is_cut_at_the_transfer_deadline() {
    let case = temp_case("r10-s9");
    let (path, bytes) = big_script(&case, 2000);
    let verified = verified_stream(&case, &path, bytes.len());
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let reader = thread::spawn(move || {
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        thread::sleep(Duration::from_secs(6));
        client
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut got = Vec::new();
        let _ = client.read_to_end(&mut got);
        got.len()
    });
    let (mut server, _) = listener.accept().unwrap();
    super::http::STREAM_BUDGET_OVERRIDE.with(|cell| cell.set(Some(Duration::from_millis(1500))));
    let started = std::time::Instant::now();
    let result = stream_verified(&mut server, &path, verified);
    let took = started.elapsed();
    super::http::STREAM_BUDGET_OVERRIDE.with(|cell| cell.set(None));
    drop(server);
    assert!(result.is_err(), "a stalled client completed the transfer");
    assert!(took < Duration::from_secs(5), "held for {took:?}");
    // The handle is gone: the file can be replaced at once.
    std::fs::remove_file(&path).expect("the served file is no longer held");
    let got = reader.join().unwrap();
    assert!(got < bytes.len(), "the whole file arrived ({got} bytes)");
    let _ = std::fs::remove_dir_all(case);
}
