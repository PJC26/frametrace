//! Round-20 track S (docs/PLAN_R20_2026-10-19.md, P34): what is checked is
//! what is used — a path is checked again after it is normalized, and a
//! verbatim / device path is never reassembled.
//!
//! No test here touches the network: every remote name is under the
//! reserved `.invalid` TLD, and the new code refuses before any
//! filesystem call (the refusal text is the lexical check's, never the
//! "case.json 없음" a filesystem probe would give).

use super::*;

/// R19 reviewer's inputs: each passes `reject_remote_path` as typed (a
/// verbatim local drive, no reserved name) but, reassembled component by
/// component, `PathBuf::push` of the one `Normal` component holding `/`
/// (not a separator in a verbatim path) or a drive designator REPLACED the
/// buffer — a UNC share, the device namespace, another drive.
#[cfg(windows)]
const OFF_DRIVE: [&str; 7] = [
    r"\\?\C:\ft-r20\//ft-r20-nohost.invalid/sh/case1",
    r"\\?\C:\ft-r20\//./pipe/ft-r20-nopipe",
    r"\\?\C:\ft-r20\//?/UNC/ft-r20-nohost.invalid/sh",
    r"\\?\C:\ft-r20\D:",
    r"\\?\C:\ft-r20\D:\case1",
    // Not verbatim, same push: `C:\x\D:` became the drive-relative `D:`.
    r"C:\ft-r20\D:",
    r"C:\ft-r20\D:\case1",
];

#[cfg(windows)]
fn post(path: &str, body: String) -> Request {
    Request {
        method: "POST".into(),
        path: path.into(),
        query: String::new(),
        body,
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    }
}

/// S1: `normalize` keeps a path on the drive it names — its prefix is
/// never replaced by a component's text.
#[cfg(windows)]
#[test]
fn r20_s1_normalize_never_leaves_the_typed_drive() {
    for typed in OFF_DRIVE {
        let normalized = crate::paste_path::normalize(Path::new(typed));
        let text = normalized.display().to_string();
        let head = &typed[..typed.find("ft-r20").unwrap() + "ft-r20".len()];
        assert!(text.starts_with(head), "{typed} -> {text}");
        assert!(
            matches!(
                normalized.components().next(),
                Some(std::path::Component::Prefix(prefix))
                    if matches!(
                        prefix.kind(),
                        std::path::Prefix::VerbatimDisk(b'C') | std::path::Prefix::Disk(b'C')
                    )
            ),
            "{typed} -> {text}"
        );
    }
}

/// S1 (High): `/api/open-case` refuses each of them from the text alone —
/// no `case.json` probe (that probe is the SMB/NTLM leak), nothing bound,
/// nothing added to the approved roots `/media` admits.
#[cfg(windows)]
#[test]
fn r20_s1_open_case_refuses_a_path_that_normalizes_off_the_drive() {
    for typed in OFF_DRIVE {
        let state: SharedState = Arc::new(Mutex::new(JobState::new()));
        let body = serde_json::json!({ "case_dir": typed }).to_string();
        let opened: serde_json::Value =
            serde_json::from_str(&api_open_case(&post("/api/open-case", body), &state)).unwrap();
        assert_eq!(opened["ok"], false, "{typed}: {opened}");
        let error = opened["error"].as_str().unwrap_or_default();
        assert!(
            !error.contains("case.json"),
            "{typed}: refused only after a filesystem probe: {error}"
        );
        assert!(state_lock(&state).case_dir.is_none(), "{typed}");
        assert!(
            super::http::approved_path_roots(&state).is_empty(),
            "{typed}"
        );
    }
}

/// S1/S3: `/api/start` refuses them too (case folder and source) before the
/// source is probed or the case bound. Shutdown is requested, so a start
/// that got past the path checks would stop at the lease with
/// `SHUTDOWN_ERROR` — never touching the case folder either way.
#[cfg(windows)]
#[test]
fn r20_s1_start_refuses_a_path_that_normalizes_off_the_drive() {
    let source = std::env::temp_dir().join(format!("ft-r20s-src-{}", std::process::id()));
    std::fs::create_dir_all(&source).unwrap();
    for typed in OFF_DRIVE {
        for (source_text, case_text) in [
            (source.display().to_string(), typed.to_string()),
            (typed.to_string(), source.join("case").display().to_string()),
        ] {
            let state: SharedState = Arc::new(Mutex::new(JobState::new()));
            state_lock(&state).request_shutdown();
            let body = serde_json::json!({ "source_path": source_text, "case_dir": case_text })
                .to_string();
            let started: serde_json::Value =
                serde_json::from_str(&super::jobs::api_start(&post("/api/start", body), &state))
                    .unwrap();
            assert_eq!(started["ok"], false, "{typed}: {started}");
            let error = started["error"].as_str().unwrap_or_default();
            assert_ne!(error, super::state::SHUTDOWN_ERROR, "{typed}: {started}");
            assert!(!error.contains("존재하지 않습니다"), "{typed}: {started}");
            assert!(state_lock(&state).case_dir.is_none(), "{typed}");
        }
    }
    let _ = std::fs::remove_dir_all(source);
}

/// S2: a device / verbatim root keeps its trailing `\` (`\\.\C:\` — Win32
/// argv conversion reads `\"` as a literal quote, so the flags merged into
/// the path, measured with cmd) — no command is offered for any device or
/// verbatim prefix.
#[cfg(windows)]
#[test]
fn r20_s2_device_roots_get_no_pasted_command() {
    for typed in [
        r"\\.\C:\",
        r"\\.\C:",
        r"\\.\C:\cases\c1",
        r"\\?\C:\",
        r"\\.\pipe\x\",
        r"\\?\GLOBALROOT\Device\x\",
    ] {
        let case = Path::new(typed);
        assert_eq!(crate::paste_path::paste_safe(case), None, "{typed}");
        for command in [
            super::finalize_journal::dismiss_command(case),
            super::finalize_journal::dismiss_command_cmd(case),
            super::finalize_journal::force_dismiss_command(case),
            super::finalize_journal::force_dismiss_command_cmd(case),
            super::finalize_journal::writer_gone_command(case, false),
            super::finalize_journal::writer_gone_command_cmd(case, true),
        ] {
            assert_eq!(command, None, "{typed}");
        }
    }
}

/// S1 (P34): `normalize_checked` refuses every one of them, and
/// `bind_path` (both route policies) with it; ordinary and verbatim local
/// paths still bind, normalized.
#[cfg(windows)]
#[test]
fn r20_s1_bind_path_refuses_what_normalizing_would_reroot() {
    let strict = |text: &str| super::http::reject_remote_path(text, &[]);
    for typed in OFF_DRIVE {
        assert_eq!(
            crate::paste_path::normalize_checked(Path::new(typed)),
            None,
            "{typed}"
        );
        assert!(
            super::state::bind_path(Path::new(typed), strict).is_err(),
            "{typed}"
        );
        assert!(
            super::state::bind_path(Path::new(typed), super::http::reject_device_path).is_err(),
            "{typed}"
        );
    }
    for (typed, want) in [
        (r"C:\cases\c1\", r"C:\cases\c1"),
        (r"C:/cases//c1/", r"C:\cases\c1"),
        (r"E:\", r"E:\."),
        // R21-S3: a verbatim local drive path binds in plain form.
        (r"\\?\C:\cases\c1\\", r"C:\cases\c1"),
        (r"\\?\C:\", r"C:\."),
        (r"\\?\C:\cases\aux.mp4", r"\\?\C:\cases\aux.mp4"),
    ] {
        assert_eq!(
            super::state::bind_path(Path::new(typed), strict),
            Ok(PathBuf::from(want)),
            "{typed}"
        );
    }
    // The start policy still admits a typed UNC share — normalized.
    assert_eq!(
        super::state::bind_path(
            Path::new(r"//ft-r20-nohost.invalid/sh/c1/"),
            super::http::reject_device_path
        ),
        Ok(PathBuf::from(r"\\ft-r20-nohost.invalid\sh\c1"))
    );
    // A stream / drive switch in an ordinary path is refused too.
    for typed in [r"C:\cases\c1:stream", r"C:\cases\D:x"] {
        assert!(
            super::state::bind_path(Path::new(typed), strict).is_err(),
            "{typed}"
        );
    }
    // Device and verbatim UNC are refused on the normalized text as well.
    for typed in [r"\\.\pipe\x\", r"\\?\UNC\ft-r20-nohost.invalid\sh\"] {
        assert!(
            super::state::bind_path(Path::new(typed), strict).is_err(),
            "{typed}"
        );
    }
}

/// S4 (P34): the check runs a second time on the NORMALIZED text — for a
/// relative path that is the current directory joined in (a UNC current
/// directory would be caught there, not by the typed `cases\c1`).
#[test]
fn r20_s4_the_check_sees_the_normalized_path_too() {
    let seen = std::cell::RefCell::new(Vec::new());
    let typed = Path::new("cases").join("c1");
    let bound = super::state::bind_path(&typed, |text| {
        seen.borrow_mut().push(text.to_string());
        Ok(())
    })
    .unwrap();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(bound, crate::paste_path::normalize(&cwd.join(&typed)));
    assert_eq!(
        *seen.borrow(),
        vec![typed.display().to_string(), bound.display().to_string()]
    );
    // A refusal on the normalized text is a refusal.
    let refused = super::state::bind_path(&typed, |text| {
        if text == typed.display().to_string() {
            Ok(())
        } else {
            Err("normalized refused")
        }
    });
    assert_eq!(refused, Err("normalized refused"));
}

/// S3: the restored session and `serve --case` bind the normalized form
/// (so `!=` against an /api/open-case or /api/start binding agrees), and a
/// session path that would normalize off the drive is dropped before any
/// probe touches it.
#[test]
fn r20_s3_restore_and_cli_case_bind_the_normalized_folder() {
    let case = std::env::temp_dir().join(format!("ft-r20s-s3-{}", std::process::id()));
    let source = case.with_extension("src");
    let sep = std::path::MAIN_SEPARATOR;
    let typed_case = format!("{}{sep}{sep}", case.display());
    let typed_source = format!("{}{sep}", source.display());
    let want_case = crate::paste_path::normalize(&case);
    let want_source = crate::paste_path::normalize(&source);
    let text =
        serde_json::json!({ "case_dir": typed_case, "source_path": typed_source }).to_string();
    let probed = std::cell::RefCell::new(Vec::new());
    let is_file = |path: &Path| {
        probed.borrow_mut().push(path.to_path_buf());
        true
    };
    let is_dir = |path: &Path| {
        probed.borrow_mut().push(path.to_path_buf());
        true
    };
    let (bound, roots) = super::state::restore_session_from(&text, &is_file, &is_dir).unwrap();
    assert_eq!(bound, want_case);
    assert_eq!(roots, vec![want_case.clone(), want_source.clone()]);
    assert_eq!(
        *probed.borrow(),
        vec![want_case.join("case.json"), want_source.clone()]
    );
    // The same folder typed with the trailing separator compares equal.
    assert_eq!(
        super::bind_cli_case(Path::new(&typed_case)),
        Ok(want_case.clone())
    );
    assert_eq!(
        super::state::bind_path(Path::new(&typed_case), |text| {
            super::http::reject_remote_path(text, &[])
        }),
        Ok(want_case.clone())
    );
    // A source that IS the case (spelled differently) is not a second root.
    let same = serde_json::json!({ "case_dir": typed_case, "source_path": want_case }).to_string();
    let (_, roots) = super::state::restore_session_from(&same, &|_| true, &|_| true).unwrap();
    assert_eq!(roots, vec![want_case.clone()]);
    #[cfg(windows)]
    for typed in OFF_DRIVE.iter().chain([r"\\.\PhysicalDrive0"].iter()) {
        probed.borrow_mut().clear();
        let text = serde_json::json!({ "case_dir": typed }).to_string();
        assert_eq!(
            super::state::restore_session_from(&text, &is_file, &is_dir),
            None,
            "{typed}"
        );
        assert!(
            probed.borrow().is_empty(),
            "{typed}: probed {:?}",
            probed.borrow()
        );
        assert!(super::bind_cli_case(Path::new(typed)).is_err(), "{typed}");
        // A bad source is dropped; the case still restores.
        let text = serde_json::json!({ "case_dir": typed_case, "source_path": typed }).to_string();
        let (_, roots) = super::state::restore_session_from(&text, &is_file, &is_dir).unwrap();
        assert_eq!(roots, vec![want_case.clone()], "{typed}");
    }
}
