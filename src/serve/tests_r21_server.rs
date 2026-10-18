//! Round-21 track S (docs/PLAN_R21_2026-10-20.md): the verbatim local
//! drive form `\\?\C:\…` — what counts as one (S1), which of its
//! components are bindable (S2), the form it is bound in (S3) — and the
//! preflight's canonical join (S5).
//!
//! Nothing here touches the network or a raw volume: the refusals are
//! lexical, and only temp folders this test creates are probed.

// Only the Windows-only tests use the serve items; the S1 text test runs
// everywhere and names its imports itself.
#[cfg(windows)]
use super::*;

/// S1: `\\?\C:` without a root separator is the raw VOLUME device (like
/// `\\.\C:` — its metadata fails with ERROR_INVALID_FUNCTION), not a local
/// drive path. Only `\\?\C:\…` is local. Pure text checks — run on every
/// host with Windows semantics.
#[test]
fn r21_s1_bare_verbatim_drive_is_not_local() {
    use crate::util::{is_remote_or_device_text, is_verbatim_local_drive};
    for bare in [r"\\?\C:", r"\\?\z:"] {
        assert!(!is_verbatim_local_drive(bare), "{bare}");
        assert!(is_remote_or_device_text(bare, true), "{bare}");
        assert!(
            super::http::reject_remote_path_on(bare, &[], true).is_err(),
            "{bare}"
        );
    }
    for local in [r"\\?\C:\", r"\\?\C:\cases\c1"] {
        assert!(is_verbatim_local_drive(local), "{local}");
        assert!(!is_remote_or_device_text(local, true), "{local}");
        assert_eq!(
            super::http::reject_remote_path_on(local, &[], true),
            Ok(()),
            "{local}"
        );
    }
    // Not a drive designator at all.
    assert!(!is_verbatim_local_drive(r"\\?\C:x"));
}

/// S1 on Windows: neither route policy binds the bare volume, and
/// `/api/open-case` refuses it before the `case.json` probe.
#[cfg(windows)]
#[test]
fn r21_s1_bare_verbatim_drive_is_never_bound() {
    let strict = |text: &str| super::http::reject_remote_path(text, &[]);
    for bare in [r"\\?\C:", r"\\?\C:\\"] {
        let want_refused = bare == r"\\?\C:";
        assert_eq!(
            super::state::bind_path(Path::new(bare), strict).is_err(),
            want_refused,
            "{bare}"
        );
        assert_eq!(
            super::state::bind_path(Path::new(bare), super::http::reject_device_path).is_err(),
            want_refused,
            "{bare}"
        );
    }
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let body = serde_json::json!({ "case_dir": r"\\?\C:" }).to_string();
    let opened: serde_json::Value =
        serde_json::from_str(&api_open_case(&post("/api/open-case", body), &state)).unwrap();
    assert_eq!(opened["ok"], false, "{opened}");
    let error = opened["error"].as_str().unwrap_or_default();
    assert!(!error.contains("case.json"), "probed first: {error}");
    assert!(state_lock(&state).case_dir.is_none());
}

/// S2: in a verbatim path `.`, `..` and an empty component are NOT
/// resolved by Windows — they are kept and handed to NT as names, and
/// whatever strips the prefix later would resolve them to another folder.
/// `normalize_checked` refuses them (both route policies with it).
#[cfg(windows)]
#[test]
fn r21_s2_verbatim_dot_and_empty_components_are_not_bindable() {
    let strict = |text: &str| super::http::reject_remote_path(text, &[]);
    for typed in [
        r"\\?\C:\cases\..\c1",
        r"\\?\C:\cases\.\c1",
        r"\\?\C:\cases\\c1",
        r"\\?\C:\..",
        r"\\?\C:\.",
        r"\\?\C:\cases\c1\..",
        r"\\?\C:\cases\c1\.\",
        r"\\?\C:\\cases",
    ] {
        assert_eq!(
            crate::paste_path::normalize_checked(Path::new(typed)),
            None,
            "{typed}"
        );
        assert_eq!(
            super::state::bind_path(Path::new(typed), strict),
            Err(crate::paste_path::UNBINDABLE_REASON),
            "{typed}"
        );
        assert!(
            super::state::bind_path(Path::new(typed), super::http::reject_device_path).is_err(),
            "{typed}"
        );
        assert_eq!(crate::paste_path::paste_safe(Path::new(typed)), None);
    }
    // Ordinary paths still resolve them lexically.
    assert_eq!(
        super::state::bind_path(Path::new(r"C:\cases\x\..\c1\.\"), strict),
        Ok(PathBuf::from(r"C:\cases\c1"))
    );
}

/// S3: a verbatim local drive path is bound in its plain form, so the same
/// folder typed either way is the same binding; a name the plain form
/// would change (a device name, trailing dot or space) stays verbatim.
#[cfg(windows)]
#[test]
fn r21_s3_verbatim_local_drive_binds_in_plain_form() {
    let strict = |text: &str| super::http::reject_remote_path(text, &[]);
    for (typed, want) in [
        (r"\\?\C:\cases\c1", r"C:\cases\c1"),
        (r"\\?\C:\cases\c1\\", r"C:\cases\c1"),
        (r"\\?\C:\", r"C:\."),
        (r"\\?\C:\cases\c 1", r"C:\cases\c 1"),
        // Kept verbatim: plain Win32 parsing would change what they name.
        (r"\\?\C:\cases\aux.mp4", r"\\?\C:\cases\aux.mp4"),
        (r"\\?\C:\cases\c1.", r"\\?\C:\cases\c1."),
        (r"\\?\C:\cases\c1 ", r"\\?\C:\cases\c1 "),
    ] {
        assert_eq!(
            super::state::bind_path(Path::new(typed), strict),
            Ok(PathBuf::from(want)),
            "{typed}"
        );
        assert_eq!(
            super::state::bind_path(Path::new(typed), super::http::reject_device_path),
            Ok(PathBuf::from(want)),
            "{typed}"
        );
    }
    assert_eq!(
        super::state::bind_path(Path::new(r"\\?\C:\cases\c1"), strict),
        super::state::bind_path(Path::new(r"C:\cases\c1\"), strict)
    );
    // The typed verbatim text still gets no pasted command; the plain
    // binding does.
    assert_eq!(
        crate::paste_path::paste_safe(Path::new(r"\\?\C:\cases\c1")),
        None
    );
    assert_eq!(
        crate::paste_path::paste_safe(&PathBuf::from(r"C:\cases\c1")),
        Some(r"C:\cases\c1".to_string())
    );
}

/// S3 end to end: reopening the bound case typed in the other spelling is
/// the SAME case — its finished clip jobs and finalize leftovers stay.
#[cfg(windows)]
#[test]
fn r21_s3_reopening_in_the_other_spelling_keeps_the_case_state() {
    let root = std::env::temp_dir().join(format!("ft-r21s-s3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let case = crate::paste_path::normalize(&root.join("c1"));
    std::fs::create_dir_all(&case).unwrap();
    std::fs::write(case.join("case.json"), b"{}").unwrap();
    let plain = case.display().to_string();
    assert!(!plain.starts_with(r"\\"), "{plain}");
    let verbatim = format!(r"\\?\{plain}");
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let open = |typed: &str| -> serde_json::Value {
        let body = serde_json::json!({ "case_dir": typed }).to_string();
        serde_json::from_str(&api_open_case(&post("/api/open-case", body), &state)).unwrap()
    };
    for (first, second) in [(&verbatim, &plain), (&plain, &verbatim)] {
        let opened = open(first);
        assert_eq!(opened["ok"], true, "{first}: {opened}");
        assert_eq!(state_lock(&state).case_dir.as_deref(), Some(case.as_path()));
        state_lock(&state).report_stale = true;
        let opened = open(second);
        assert_eq!(opened["ok"], true, "{second}: {opened}");
        let guard = state_lock(&state);
        assert_eq!(guard.case_dir.as_deref(), Some(case.as_path()));
        assert!(
            guard.report_stale,
            "{first} then {second}: reopening reset the case state"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

/// S5: the preflight's case path resolved through its nearest existing
/// ancestor joins the missing tail as TEXT — a component that parses as a
/// prefix (`D:`) no longer replaces the canonical buffer.
#[cfg(windows)]
#[test]
fn r21_s5_canonical_via_ancestor_keeps_the_ancestor() {
    let base = std::env::temp_dir().join(format!("ft-r21s-s5-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let canonical = base.canonicalize().unwrap();
    let typed = PathBuf::from(format!(r"{}\missing\D:", base.display()));
    let resolved = super::system::canonical_via_ancestor(&typed);
    assert_eq!(
        resolved.as_os_str(),
        std::ffi::OsString::from(format!(r"{}\missing\D:", canonical.display())).as_os_str(),
    );
    // Ordinary missing tails as before.
    let typed = base.join("a").join("b");
    assert_eq!(
        super::system::canonical_via_ancestor(&typed),
        canonical.join("a").join("b")
    );
    let _ = std::fs::remove_dir_all(base);
}

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
