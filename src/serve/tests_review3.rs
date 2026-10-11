//! Round-3 adversarial review findings: sign-off binding of ranges/tags,
//! sign-off on a failed mark read, cancel scoping, clip-job races, phase
//! during recover/carve, open-case vs running exports, verify-sources
//! cancel.

use super::*;

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-rev3-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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
    let output = Command::new(cli_bin()).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "frametrace {args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Case with one indexed source. Returns (root, case).
fn case_fixture(label: &str) -> (PathBuf, PathBuf) {
    let root = scratch(label);
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("a.mp4"), b"fixture-video-bytes-0123456789").unwrap();
    let case = root.join("case");
    run_cli(&["init-case", case.to_str().unwrap()]);
    run_cli(&[
        "scan-folder",
        case.to_str().unwrap(),
        src_dir.to_str().unwrap(),
        "--hash",
        "--no-ffprobe",
    ]);
    (root, case)
}

fn bound(case: &Path, phase: &'static str) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case.to_path_buf());
    state.media_roots = vec![case.to_path_buf()];
    state.phase = phase;
    Arc::new(Mutex::new(state))
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("invalid JSON {err}: {text}"))
}

fn request(method: &str, path: &str, query: &str, body: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        body: body.into(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    }
}

/// Imports one mark (fixed `marked_unix`, so only the range/tags differ
/// between imports) plus optional tags.
fn import_mark(root: &Path, case: &Path, range_out: f64, tags: &[&str]) {
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(case.join("case.json")).unwrap()).unwrap();
    let mut marks = serde_json::json!({
        "schema_version": 2,
        "case_id": manifest["case_id"],
        "marks": [{
            "id": "vid_000001", "status": "important", "note": "n",
            "examiner": "kim", "marked_unix": 100,
            "range": {"in": 1.0, "out": range_out},
        }],
    });
    if !tags.is_empty() {
        marks["tags"] = serde_json::json!([{"id": "vid_000001", "tags": tags}]);
    }
    let path = root.join("marks.json");
    std::fs::write(&path, marks.to_string()).unwrap();
    crate::cli::handlers::import_marks_quiet(case, &path).unwrap();
}

fn signoff_check(case: &Path, root: &Path, tag: &str) -> crate::package::PackageCheck {
    let out = root.join(format!("pkg-{tag}"));
    crate::package::package_case(case, Some(&out)).unwrap();
    let verification = crate::package::verify_package(&out).unwrap();
    verification
        .checks
        .into_iter()
        .find(|c| c.subject == crate::report::SIGNOFF_REL_PATH)
        .expect("sign-off check present")
}

/// BLOCKER 1: after a sign-off, moving a mark's IN-OUT range (or adding a
/// tag) must make make-report and verify-package say the sign-off no
/// longer covers the content.
#[test]
fn signoff_detects_range_and_tag_changes() {
    let (root, case) = case_fixture("signoff-range");
    import_mark(&root, &case, 2.0, &[]);
    let state = bound(&case, "review-ready");
    let response = finalize_with_exe(&cli_bin(), &case, "홍길동", &state);
    assert!(response.contains("\"ok\":true"), "{response}");
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(
        report.contains("판독 마크는 변경되지 않았습니다"),
        "fresh sign-off"
    );
    assert_eq!(
        signoff_check(&case, &root, "fresh").status,
        crate::package::CheckStatus::Pass
    );

    // Range moved (same status / note / marked_unix).
    import_mark(&root, &case, 9.5, &[]);
    crate::cli::handlers::make_report(&case, false, false, None).unwrap();
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(
        report.contains("이 기록 이후 판독 마크가 변경되었습니다"),
        "range change after sign-off must be reported"
    );
    let check = signoff_check(&case, &root, "range");
    assert_eq!(
        check.status,
        crate::package::CheckStatus::Fail,
        "{}",
        check.detail
    );

    // Range restored, tag added: still a change.
    import_mark(&root, &case, 2.0, &["번호판"]);
    crate::cli::handlers::make_report(&case, false, false, None).unwrap();
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(report.contains("이 기록 이후 판독 마크가 변경되었습니다"));
    assert_eq!(
        signoff_check(&case, &root, "tags").status,
        crate::package::CheckStatus::Fail
    );
    let signoff = std::fs::read_to_string(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap();
    assert!(signoff.contains("\"schema_version\":3"), "{signoff}");
    let _ = std::fs::remove_dir_all(root);
}

/// BLOCKER 1: a pre-v3 sign-off (status/note binding only) must read as
/// "binding unknown", never as "unchanged".
#[test]
fn legacy_signoff_reads_as_binding_unknown() {
    let (root, case) = case_fixture("signoff-legacy");
    import_mark(&root, &case, 2.0, &[]);
    let legacy =
        crate::report::signoff_content_sha256(&crate::cli::handlers::review_marks_json_from_rows(
            &crate::case_db::load_review_marks(&case).unwrap(),
        ));
    std::fs::write(
        case.join(crate::report::SIGNOFF_REL_PATH),
        format!(r#"{{"schema_version":2,"examiner":"홍길동","signed_unix":5,"content_sha256":"{legacy}"}}"#),
    )
    .unwrap();
    crate::cli::handlers::make_report(&case, false, false, None).unwrap();
    let report = std::fs::read_to_string(case.join("reports/case-report.html")).unwrap();
    assert!(
        !report.contains("변경되지 않았습니다"),
        "legacy must not read as unchanged"
    );
    assert!(report.contains("IN-OUT 구간·태그가 이후 바뀌었는지는 확인할 수 없습니다"));
    let check = signoff_check(&case, &root, "legacy");
    assert_eq!(
        check.status,
        crate::package::CheckStatus::NotVerified,
        "{}",
        check.detail
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Finding 8: a review-mark read failure must refuse the sign-off rather
/// than bind it to an empty mark list.
#[cfg(unix)]
#[test]
fn signoff_refused_when_marks_cannot_be_read() {
    let root = scratch("signoff-dberr");
    let case = root.join("case");
    std::fs::create_dir_all(case.join("db")).unwrap();
    std::fs::create_dir_all(case.join("review")).unwrap();
    std::fs::write(case.join("case.json"), br#"{"case_id":"FT-x"}"#).unwrap();
    std::fs::write(
        crate::case_db::case_db_path(&case),
        b"this is not a sqlite database at all, just garbage bytes.................",
    )
    .unwrap();
    let err = super::jobs::write_signoff(&case, crate::report::SIGNOFF_REL_PATH, "홍길동")
        .expect_err("sign-off over an unreadable DB must fail");
    assert!(err.contains("최종화 차단"), "{err}");
    assert!(!case.join(crate::report::SIGNOFF_REL_PATH).exists());
    let _ = std::fs::remove_dir_all(root);
}
