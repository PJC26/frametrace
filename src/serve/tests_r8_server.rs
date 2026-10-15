//! Round-8 track S (docs/PLAN_R8_2026-10-15.md): verified serving without
//! an identity shortcut (S2), the report page on unreadable inputs (S4)
//! and step-bound job warnings (S7). Each test fails on the code before
//! the fix.

use super::http::{ServeVerdict, VerifiedBody, verify_for_serving, verify_read_bytes};
use super::*;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r8s-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn temp_case(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::create_dir_all(dir.join("review")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-r8"}"#).unwrap();
    dir
}

/// A script larger than the in-memory limit, recorded as generated.
fn big_script(case: &Path, name: &str) -> (PathBuf, Vec<u8>) {
    let size = super::http::IN_MEMORY_VERIFY_MAX as usize + 2 * 64 * 1024 + 5;
    let mut bytes = b"window.__FRAMETRACE_DATA__={pad:\"".to_vec();
    bytes.resize(size - 3, b'a');
    bytes.extend_from_slice(b"\"};");
    let path = case.join("review").join(name);
    std::fs::write(&path, &bytes).unwrap();
    crate::util::generated_pages::record_generated_contents(
        case,
        &[(&format!("review/{name}"), bytes.as_slice())],
    )
    .unwrap();
    (path.canonicalize().unwrap(), bytes)
}

fn streams_trusted(verdict: &ServeVerdict) -> bool {
    matches!(
        verdict,
        ServeVerdict::Trusted {
            body: VerifiedBody::Stream(_),
            ..
        }
    )
}

// ---- S2: every GET hashes before the first byte ----

/// R8-S2: R7 skipped the hash pass of a large script or page when its
/// cached identity (size, mtime, file id, change time) matched, and
/// started sending before the end-of-transfer re-hash could notice a
/// change. Every GET now hashes the file first.
#[test]
fn r8_s2_every_get_hashes_before_sending() {
    let case = temp_case("s2-every");
    let (path, bytes) = big_script(&case, "data-bundle.js");
    let len = bytes.len() as u64;
    for round in 1..=3u64 {
        let verdict = verify_for_serving(&case, &path, len, false, false);
        assert!(streams_trusted(&verdict), "round {round}");
        assert_eq!(
            verify_read_bytes(&path),
            round * len,
            "round {round}: the file was not hashed before serving"
        );
    }
    // HEAD (headers only) still reads nothing.
    let verdict = verify_for_serving(&case, &path, len, false, true);
    assert!(matches!(
        verdict,
        ServeVerdict::Trusted {
            body: VerifiedBody::HeadOnly,
            ..
        }
    ));
    assert_eq!(verify_read_bytes(&path), 3 * len);
    let _ = std::fs::remove_dir_all(case);
}

/// The NTFS basic information (all four timestamps, attributes) of `path`.
#[cfg(windows)]
fn basic_info(file: &std::fs::File) -> windows::Win32::Storage::FileSystem::FILE_BASIC_INFO {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
    };
    let mut basic = FILE_BASIC_INFO::default();
    // SAFETY: the open file's handle; a properly sized out-structure.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    }
    .unwrap();
    basic
}

/// R8-S2 (Windows): the attack R7's cache allowed. A large listed script
/// is served once; it is then rewritten in place with bytes of the same
/// length and EVERY timestamp — `ChangeTime` included — restored through
/// `SetFileInformationByHandle(FileBasicInfo)`. The identity R7 cached by
/// is then identical, and R7 vouched for the new bytes without reading
/// them. Now they are hashed and refused.
#[cfg(windows)]
#[test]
fn r8_s2_restored_change_time_does_not_vouch_for_new_bytes() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{FILE_BASIC_INFO, FileBasicInfo};
    let case = temp_case("s2-ctime");
    let (path, bytes) = big_script(&case, "data-bundle.js");
    let len = bytes.len() as u64;
    assert!(streams_trusted(&verify_for_serving(
        &case, &path, len, false, false
    )));
    let before = super::http::file_identity(&std::fs::File::open(&path).unwrap()).unwrap();
    let original = basic_info(&std::fs::File::open(&path).unwrap());
    std::thread::sleep(Duration::from_millis(50));
    {
        let mut file = std::fs::File::options().write(true).open(&path).unwrap();
        let mut evil = bytes.clone();
        evil[..30].copy_from_slice(b"fetch('/api/finalize');//_____");
        file.write_all(&evil).unwrap();
        file.flush().unwrap();
        file.sync_all().unwrap();
        // SAFETY: the open file's handle; the structure and its size.
        unsafe {
            windows::Win32::Storage::FileSystem::SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileBasicInfo,
                (&original as *const FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        }
        .unwrap();
    }
    let after = super::http::file_identity(&std::fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(
        before, after,
        "premise: the whole identity can be restored after a rewrite"
    );
    assert!(
        matches!(
            verify_for_serving(&case, &path, len, false, false),
            ServeVerdict::Untrusted
        ),
        "rewritten bytes were vouched for by a restored identity"
    );
    let _ = std::fs::remove_dir_all(case);
}

// ---- S4: the report page on unreadable inputs ----

/// R8-S4: make-report embeds an unreadable `case.json` /
/// `db/video_index.json` as `null` (R6-S1); the report script then threw
/// on `manifest.title` and the whole report stayed blank. It now renders
/// and says which input could not be read.
#[test]
fn r8_s4_report_page_survives_null_inputs() {
    use std::process::{Command, Stdio};
    let html = crate::report::render_case_report(&crate::report::ReportInputs {
        manifest_json: "{\"case_id\":\"x\"};alert(1)",
        index_json: "",
        export_log_jsonl: "",
        proxy_log_jsonl: "",
        thumbnail_log_jsonl: "",
        carve_log_jsonl: "",
        filesystem_log_jsonl: "",
        validation_log_jsonl: "",
        anomaly_log_jsonl: "",
        batch_log_jsonl: "",
        join_log_jsonl: "",
        audio_levels_json: "",
        scan_runs_json: "[]",
        marks_json: "[]",
        redaction_applied: false,
        review_warnings_json: "",
        read_errors_json: "",
        telemetry_json: "",
    });
    assert!(html.contains("const manifestInput = objectOrNull(null);"));
    let script = html
        .split_once("<script>")
        .unwrap()
        .1
        .split_once("</script>")
        .unwrap()
        .0;
    let Ok(mut child) = Command::new("node")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        eprintln!("node not found; report script run skipped");
        return;
    };
    let mut stdin = child.stdin.take().unwrap();
    write!(
        stdin,
        "const elements = new Map(); const document = {{getElementById(id) {{ if (!elements.has(id)) elements.set(id, {{}}); return elements.get(id); }}}};\n{script}\nprocess.stdout.write(JSON.stringify(Object.fromEntries(elements)));"
    )
    .unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "the report script threw: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let elements: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let notice = elements["read-errors"]["innerHTML"].as_str().unwrap_or("");
    assert!(notice.contains("케이스 파일(case.json)"), "{notice}");
    assert!(
        notice.contains("색인 파일(db/video_index.json)"),
        "{notice}"
    );
    assert!(notice.contains("읽지 못했습니다"), "{notice}");
    assert_eq!(
        elements["title"]["textContent"],
        "FrameTrace 영상 포렌식 보고서"
    );
}

// ---- S7: job warnings bound to the step ----

/// A fake CLI that prints `lines` on stderr (the shell expands the step
/// token variable in them) and exits 0.
fn fake_cli(dir: &Path, lines: &[String]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut body = String::from("#!/bin/sh\n");
        for line in lines {
            body.push_str(&format!("echo \"{line}\" >&2\n"));
        }
        body.push_str("echo ok\nexit 0\n");
        let path = dir.join("fake-r8.sh");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    #[cfg(windows)]
    {
        let mut body = String::from("@echo off\r\n");
        for line in lines {
            body.push_str(&format!("echo {line} 1>&2\r\n"));
        }
        body.push_str("echo ok\r\nexit /b 0\r\n");
        let path = dir.join("fake-r8.cmd");
        std::fs::write(&path, body).unwrap();
        path
    }
}

fn token_ref() -> String {
    if cfg!(windows) {
        format!("%{}%", crate::util::STEP_WARNING_ENV)
    } else {
        format!("${{{}}}", crate::util::STEP_WARNING_ENV)
    }
}

/// R8-S7: any stderr line starting with a marker became a job warning —
/// and a step's stderr relays text it does not control (ffmpeg quoting
/// evidence names), so evidence could forge one. The cap was shared, so
/// a flood of step warnings also hid an audit-key mismatch. Now only lines
/// with the step's random token count, each marker has its own cap, and
/// the overflow is said.
#[test]
fn r8_s7_only_step_bound_marker_lines_become_job_warnings() {
    let dir = scratch("s7");
    let step = crate::util::tokened_marker(crate::util::STEP_WARNING_MARKER, &token_ref());
    let key = crate::util::tokened_marker(KEY_MISMATCH_MARKER, &token_ref());
    let mut lines = vec![
        format!("{}forged plain", crate::util::STEP_WARNING_MARKER),
        format!(
            "{}forged token",
            crate::util::tokened_marker(
                crate::util::STEP_WARNING_MARKER,
                "0123456789abcdef0123456789abcdef"
            )
        ),
        format!("{KEY_MISMATCH_MARKER}forged key"),
    ];
    for n in 0..12 {
        lines.push(format!("{step}genuine step {n}"));
    }
    lines.push(format!("{key}genuine key mismatch"));
    let exe = fake_cli(&dir, &lines);
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    run_step(&exe, &["make-review".into(), "x".into()], &state).unwrap();
    let status: serde_json::Value = serde_json::from_str(&api_status(&state)).unwrap();
    let warnings = status["warnings"].to_string();
    assert!(!warnings.contains("forged"), "{warnings}");
    assert!(warnings.contains("genuine key mismatch"), "{warnings}");
    assert!(warnings.contains("genuine step 7"), "{warnings}");
    assert!(!warnings.contains("genuine step 8"), "{warnings}");
    assert!(warnings.contains("4 more warning(s)"), "{warnings}");
    let _ = std::fs::remove_dir_all(dir);
}

/// R8-S7: the child side — a token in the environment binds the line,
/// anything else is not a token.
#[test]
fn r8_s7_marker_lines_carry_the_step_token() {
    assert_eq!(
        crate::util::tokened_marker(KEY_MISMATCH_MARKER, "ab12"),
        "FRAMETRACE-KEY-MISMATCH[ab12]: "
    );
    let token = super::jobs::step_marker_token().unwrap();
    assert_eq!(token.len(), 32);
    assert_ne!(token, super::jobs::step_marker_token().unwrap());
}
