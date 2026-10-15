//! Round-9 track S (docs/PLAN_R9_2026-10-15.md): verified streaming that
//! compares every chunk before sending it (S1), finalize error texts when
//! the package is already published (S4) or the abandon record fails (S5).
//! Each test fails on the code before the fix (or, for a new hook, does
//! not compile there).

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

/// The real CLI binary next to the test executable.
fn cli_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let name = if cfg!(windows) {
        "frametrace.exe"
    } else {
        "frametrace"
    };
    let bin = exe.parent().unwrap().parent().unwrap().join(name);
    assert!(
        bin.is_file(),
        "frametrace binary not built at {}",
        bin.display()
    );
    bin
}

fn run_cli(args: &[&str]) {
    let output = std::process::Command::new(cli_bin())
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "frametrace {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A case with one indexed source. Returns (root, case).
fn finalize_case(tag: &str) -> (PathBuf, PathBuf) {
    let root = scratch(tag);
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.mp4"), b"fixture-video-bytes-r9").unwrap();
    let case = root.join("case");
    run_cli(&["init-case", case.to_str().unwrap()]);
    run_cli(&[
        "scan-folder",
        case.to_str().unwrap(),
        src.to_str().unwrap(),
        "--hash",
        "--no-ffprobe",
    ]);
    (root, case)
}

fn finalize_error(response: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(response).unwrap();
    assert_eq!(value["ok"], false, "{response}");
    value["error"].as_str().unwrap_or_default().to_string()
}

/// R9-S4: package-case published its package, then promoting the sign-off
/// failed. R8 answered only "검토자 기록 확정 실패" while a complete-looking
/// package sat in reports/. The error now names the package and says the
/// finalize is incomplete, and a marker beside the package says so too
/// (outside it: verify-package must still see exactly its files).
#[test]
fn r9_s4_published_package_with_failed_promotion_is_named_incomplete() {
    let (root, case) = finalize_case("s4");
    let fake = write_fake(
        &root,
        "case \"$1\" in package-case) mkdir -p \"$2/reports/package_1\" && echo x > \"$2/reports/package_1/manifest.sha256\";; esac\nexit 0",
        "if \"%1\"==\"package-case\" (\nmkdir \"%~2\\reports\\package_1\"\necho x> \"%~2\\reports\\package_1\\manifest.sha256\"\n)\nexit /b 0",
    );
    // The promotion rename cannot replace a (non-empty) directory.
    let blocker = case.join(crate::report::SIGNOFF_REL_PATH);
    std::fs::create_dir_all(&blocker).unwrap();
    std::fs::write(blocker.join("keep"), b"x").unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).case_dir = Some(case.clone());
    let error = finalize_error(&finalize_with_exe(&fake, &case, "홍길동", &state));
    let package = case.join("reports").join("package_1");
    assert!(package.is_dir(), "the fake published a package");
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
    let (root, case) = finalize_case("s5");
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

// ---- S7: the step token stays with the step ----

/// R9-S7: the CLI step kept its token in the environment, so every tool it
/// ran (ffmpeg, ffprobe, Sleuth Kit, ...) inherited the secret that makes a
/// stderr line a job warning. `main` now adopts the token and removes the
/// variable before anything runs; a child started afterwards does not see
/// it. (A probe variable stands in for `FRAMETRACE_STEP_WARNINGS`, which
/// other tests' children use.)
#[test]
fn r9_s7_adopted_secret_is_not_inherited_by_children() {
    const PROBE: &str = "FRAMETRACE_R9_S7_PROBE";
    // SAFETY: test-only variable no other code reads.
    unsafe { std::env::set_var(PROBE, "0123456789abcdef0123") };
    // SAFETY: as above.
    let taken = unsafe { crate::util::take_env_var(PROBE) };
    assert_eq!(taken.as_deref(), Some("0123456789abcdef0123"));
    assert!(std::env::var_os(PROBE).is_none());
    let output = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", &format!("echo [%{PROBE}%]")])
            .output()
    } else {
        std::process::Command::new("sh")
            .args(["-c", &format!("echo \"[${{{PROBE}}}]\"")])
            .output()
    }
    .unwrap();
    let seen = String::from_utf8_lossy(&output.stdout);
    assert!(!seen.contains("0123456789abcdef"), "child saw it: {seen}");
    // Nothing to take the second time.
    // SAFETY: as above.
    assert_eq!(unsafe { crate::util::take_env_var(PROBE) }, None);
}
