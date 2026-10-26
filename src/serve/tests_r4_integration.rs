//! Round-4 integration: cross-track contracts that only hold once the
//! tracks are merged (path-free proxy names at the proxy route, sign-off
//! v4 written and chained by finalize before the package is built).

use super::*;

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r4i-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn bound(case: &Path) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case.to_path_buf());
    state.media_roots = vec![case.to_path_buf()];
    state.phase = "review-ready";
    Arc::new(Mutex::new(state))
}

fn request(path: &str, body: &str) -> Request {
    Request {
        method: "POST".into(),
        path: path.into(),
        query: String::new(),
        body: body.into(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    }
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("invalid JSON {err}: {text}"))
}

/// D5 × `/api/proxy`: a proxy under the path-free
/// `proxy_<record id>_<hash8>.mp4` name is found (before: the route only
/// knew `<selector>_proxy_*`, so a freshly generated proxy read as
/// "프록시 생성 후 파일을 찾지 못했습니다"), and a pre-D5 name still is.
#[test]
fn proxy_route_finds_path_free_and_legacy_names() {
    let case = scratch("proxy");
    std::fs::create_dir_all(case.join("db")).unwrap();
    std::fs::create_dir_all(case.join("src")).unwrap();
    std::fs::create_dir_all(case.join("artifacts/proxies")).unwrap();
    std::fs::write(case.join("case.json"), br#"{"case_id":"FT-r4i"}"#).unwrap();
    let source = case.join("src/a.mp4");
    std::fs::write(&source, b"source-bytes").unwrap();
    std::fs::write(
        case.join("db/video_paths.tsv"),
        format!(
            "id\tsource_path\trelative_path\nvid_000001\t{}\ta.mp4\n",
            source.to_string_lossy()
        ),
    )
    .unwrap();
    let resolved = crate::video_export::resolve_video_source(&case, "vid_000001").unwrap();
    let record_id = crate::artifacts::artifact_record_id(&case, "vid_000001", &resolved);
    let fresh = case
        .join("artifacts/proxies")
        .join(crate::artifacts::artifact_file_name(
            "proxy", &record_id, &resolved, "mp4",
        ));
    std::fs::write(&fresh, b"proxy").unwrap();
    let state = bound(&case);
    let body = r#"{"id":"vid_000001"}"#;
    let reply = parse(&api_proxy(&request("/api/proxy", body), &state));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["path"], fresh.to_string_lossy().as_ref(), "{reply}");

    std::fs::remove_file(&fresh).unwrap();
    let legacy = case
        .join("artifacts/proxies")
        .join("vid_000001_proxy_1700000000.mp4");
    std::fs::write(&legacy, b"proxy").unwrap();
    let reply = parse(&api_proxy(&request("/api/proxy", body), &state));
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["path"], legacy.to_string_lossy().as_ref(), "{reply}");
    let _ = std::fs::remove_dir_all(case);
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

/// B18 × D13: finalize writes a schema-v4 sign-off and chains it into
/// `signoff-log.jsonl` BEFORE make-report and package-case, so the
/// packaged report and verify-package both see a current v4 binding and
/// the sign-off in the audit chain (before: v3 record, chained only after
/// packaging — the package said "not in the audit chain").
#[test]
fn finalize_packages_a_v4_signoff_that_is_in_the_audit_chain() {
    let root = scratch("finalize-v4");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.mp4"), b"fixture-video-bytes-0123456789").unwrap();
    let case = root.join("case");
    run_cli(&["init-case", case.to_str().unwrap()]);
    run_cli(&[
        "scan-folder",
        case.to_str().unwrap(),
        src.to_str().unwrap(),
        "--hash",
        "--no-ffprobe",
    ]);
    crate::case_db::upsert_review_marks(
        &case,
        &[crate::case_db::ReviewMarkRow {
            record_id: "vid_000001".into(),
            status: "important".into(),
            marked_unix: 100,
            examiner: Some("kim".into()),
            ..Default::default()
        }],
    )
    .unwrap();
    let state = bound(&case);
    let reply = parse(&{
        let _busy = try_acquire_busy(&state).unwrap();
        finalize_with_exe(&cli_bin(), &case, "홍길동", &state)
    });
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(!case.join(SIGNOFF_PENDING_REL_PATH).exists());

    let signoff = std::fs::read_to_string(case.join(crate::report::SIGNOFF_REL_PATH)).unwrap();
    assert!(signoff.contains("\"schema_version\":4"), "{signoff}");
    assert!(
        signoff.contains("\"evidence\":{\"vid_000001\":\""),
        "{signoff}"
    );
    let (recorded, note) = crate::report::signoff_audit_status(&case, &signoff);
    assert!(recorded, "{note}");

    let package = newest_package_dir(&case).expect("package written");
    // The packaged record is the promoted one, byte for byte.
    assert_eq!(
        std::fs::read_to_string(package.join(crate::report::SIGNOFF_REL_PATH)).unwrap(),
        signoff
    );
    let report = std::fs::read_to_string(package.join("reports/case-report.html")).unwrap();
    assert!(
        report.contains("서명 기록이 감사 체인(signoff-log"),
        "packaged report must say the sign-off is in the chain"
    );
    assert!(
        report.contains("서명 형식 v4"),
        "v4 binding shown as current"
    );

    let verification = crate::package::verify_package(&package).unwrap();
    let failures: Vec<String> = verification
        .failures()
        .map(|c| format!("{}: {}", c.subject, c.detail))
        .collect();
    assert!(verification.passed(), "{failures:?}");
    let has = |subject: &str, status: crate::package::CheckStatus, needle: &str| {
        verification
            .checks
            .iter()
            .any(|c| c.subject == subject && c.status == status && c.detail.contains(needle))
    };
    use crate::package::CheckStatus::Pass;
    assert!(
        has(crate::report::SIGNOFF_REL_PATH, Pass, "sign-off v4"),
        "{:?}",
        verification.checks
    );
    assert!(
        has(crate::report::SIGNOFF_LOG_REL_PATH, Pass, "감사 체인"),
        "{:?}",
        verification.checks
    );
    assert!(has(
        crate::report::SIGNOFF_LOG_REL_PATH,
        Pass,
        "audit chain verified"
    ));
    assert!(has(
        "reports/case-report.html",
        Pass,
        "signoff-sha256 marker"
    ));
    let _ = std::fs::remove_dir_all(root);
}
