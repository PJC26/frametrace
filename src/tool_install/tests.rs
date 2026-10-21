//! Track S6: the installer against a local fake source only — no real
//! internet. Pure checks (manifest, URL policy, listing rules, pip
//! requirements, rollback) run everywhere; the curl/tar paths are
//! Windows-only.

use super::*;

fn base_entry() -> ToolEntry {
    ToolEntry {
        name: "faketool".into(),
        title: "Fake tool".into(),
        kind: ToolKind::Archive,
        required: false,
        on_demand: false,
        platform: PLATFORM.into(),
        version: Some("1.0".into()),
        license: Some("MIT".into()),
        // Installable fixtures (sha256 set by the tests) need one (T5).
        notice: Some("Fake tool: downloaded from its publisher.".into()),
        notice_en: None,
        homepage: Some("https://example.invalid/".into()),
        source_url: None,
        url: None,
        file_name: None,
        format: Some("zip".into()),
        size: None,
        sha256: None,
        checksum_source: Some("test".into()),
        unavailable_reason: None,
        files: vec![FileMap {
            from: "pkg/bin/fake.exe".into(),
            to: "fake.exe".into(),
        }],
        license_files: vec![FileMap {
            from: "pkg/LICENSE.txt".into(),
            to: "fake-LICENSE.txt".into(),
        }],
        bundled_licenses: Vec::new(),
        provides: vec!["fake".into()],
        manual_hint: Some("manual".into()),
        package: None,
        index_url: None,
        requirements_file: None,
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-ti-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn manifest_with(tool: ToolEntry) -> Manifest {
    Manifest {
        schema: 1,
        allowed_hosts: vec!["github.com".into()],
        tools: vec![tool],
    }
}

// ------------------------------------------------------------ manifest

#[test]
fn shipped_manifest_is_valid_and_pins_only_published_checksums() {
    let manifest = manifest().expect("tools/manifest.json must parse and validate");
    let ffmpeg = find_tool(manifest, "ffmpeg").unwrap();
    assert!(ffmpeg.required);
    assert_eq!(
        ffmpeg.sha256.as_deref(),
        Some("4a7642b2264c03e8a0ce8a3825b933ee5580656f45695a086fe7e294045ffc0a")
    );
    assert_eq!(ffmpeg.size, Some(170_611_883));
    assert!(
        ffmpeg
            .url
            .as_deref()
            .unwrap()
            .contains("autobuild-2026-09-30-13-08")
    );
    assert!(
        ffmpeg.url.as_deref().unwrap().contains("-lgpl-"),
        "the pinned BtbN asset is the win64-lgpl variant (same file the examiner downloads)"
    );
    // Option C: the title names no licence, the licence names the mix and
    // the notice says FrameTrace does not distribute the file (T5).
    assert_eq!(ffmpeg.title, "FFmpeg");
    let license = ffmpeg.license.as_deref().unwrap();
    assert!(
        license.starts_with("LGPL-3.0-or-later (FFmpeg)"),
        "{license}"
    );
    assert!(license.contains("GPL-2.0-only"), "{license}");
    assert!(
        ffmpeg
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("배포하지 않"))
    );
    assert!(
        ffmpeg
            .notice_en
            .as_deref()
            .is_some_and(|n| n.contains("does not distribute"))
    );
    assert_eq!(ffmpeg.provides, ["ffmpeg", "ffprobe"]);
    assert!(
        ffmpeg
            .checksum_source
            .as_deref()
            .unwrap()
            .ends_with("/checksums.sha256")
    );
    // No official checksum → not installable, manual hint instead.
    for name in ["libewf", "sleuthkit", "deepfake-lens"] {
        let tool = find_tool(manifest, name).unwrap();
        assert!(tool.sha256.is_none(), "{name}");
        assert!(not_installable_reason(tool).is_some(), "{name}");
        assert!(tool.manual_hint.is_some(), "{name}");
    }
    let lens = find_tool(manifest, "deepfake-lens").unwrap();
    assert!(lens.on_demand);
    assert_eq!(lens.kind, ToolKind::Pip);
    // Every pinned requirements file must be compiled in.
    for tool in &manifest.tools {
        if tool.requirements_file.is_some() {
            assert!(requirements_text(tool).is_some(), "{}", tool.name);
        }
    }
    if cfg!(all(windows, target_arch = "x86_64")) {
        assert_eq!(not_installable_reason(ffmpeg), None);
    } else {
        assert_eq!(
            not_installable_reason(ffmpeg).as_deref(),
            Some(UNSUPPORTED_PLATFORM)
        );
    }
}

#[test]
fn manifest_validation_refuses_unpinned_or_foreign_sources() {
    let pinned = |url: &str, file_name: &str| {
        let mut tool = base_entry();
        tool.url = Some(url.into());
        tool.file_name = Some(file_name.into());
        tool.size = Some(10);
        tool.sha256 = Some("a".repeat(64));
        tool
    };
    let ok = pinned(
        "https://github.com/o/r/releases/download/v1/fake.zip",
        "fake.zip",
    );
    validate_manifest(&manifest_with(ok.clone())).unwrap();
    for (tool, why) in [
        (
            pinned(
                "https://github.com/o/r/releases/latest/download/fake.zip",
                "fake.zip",
            ),
            "latest",
        ),
        (pinned("http://github.com/o/fake.zip", "fake.zip"), "HTTPS"),
        (
            pinned("https://evil.example/fake.zip", "fake.zip"),
            "allowlist",
        ),
        (
            pinned("https://github.com/o/fake.zip", "other.zip"),
            "file_name",
        ),
        (
            {
                let mut t = ok.clone();
                t.sha256 = Some("A".repeat(64));
                t
            },
            "sha256",
        ),
        (
            {
                let mut t = ok.clone();
                t.files[0].from = "../fake.exe".into();
                t
            },
            "unsafe file mapping",
        ),
        (
            {
                let mut t = ok.clone();
                t.files[0].to = "sub/fake.exe".into();
                t
            },
            "unsafe file mapping",
        ),
        (
            {
                let mut t = ok.clone();
                t.size = None;
                t
            },
            "size",
        ),
    ] {
        let err = validate_manifest(&manifest_with(tool)).unwrap_err();
        assert!(err.contains(why), "{why}: {err}");
    }
}

/// Option C (T5): an installable archive tool must say, before anything is
/// downloaded, that FrameTrace does not distribute it. A tool without a
/// pinned checksum (never installable) needs no notice.
#[test]
fn installable_tools_must_carry_a_distribution_notice() {
    let mut tool = base_entry();
    tool.url = Some("https://github.com/o/r/releases/download/v1/fake.zip".into());
    tool.file_name = Some("fake.zip".into());
    tool.size = Some(10);
    tool.sha256 = Some("a".repeat(64));
    validate_manifest(&manifest_with(tool.clone())).unwrap();
    for missing in [None, Some(String::new()), Some("  \n".to_string())] {
        let mut bad = tool.clone();
        bad.notice = missing.clone();
        let err = validate_manifest(&manifest_with(bad)).unwrap_err();
        assert!(err.contains("distribution notice"), "{missing:?}: {err}");
        assert!(err.contains("\"faketool\""), "{err}");
    }
    let mut blank_en = tool.clone();
    blank_en.notice_en = Some(String::new());
    let err = validate_manifest(&manifest_with(blank_en)).unwrap_err();
    assert!(err.contains("notice_en"), "{err}");
    // libewf-like: no published checksum, not installable, no notice needed.
    let mut manual = base_entry();
    manual.notice = None;
    assert!(manual.sha256.is_none());
    validate_manifest(&manifest_with(manual)).unwrap();
}

/// Fix round 11 M2: the shipped ffmpeg notice states facts only — the
/// licence mix (ko and en name the same three), no assurance that the
/// file may be passed on, "do not copy" — and never a legal conclusion
/// that it is redistributable under the GPL.
#[test]
fn the_ffmpeg_notice_states_facts_not_a_gpl_redistribution_conclusion() {
    let ffmpeg = find_tool(manifest().unwrap(), "ffmpeg").unwrap();
    let ko = ffmpeg.notice.as_deref().unwrap();
    let en = ffmpeg.notice_en.as_deref().unwrap();
    for licence in ["GPL-2.0-only", "LGPL-3.0", "Apache-2.0", "zvbi"] {
        assert!(ko.contains(licence) && en.contains(licence), "{licence}");
    }
    assert!(
        ko.contains("보증하지 않습니다") && ko.contains("복사하지 말고"),
        "{ko}"
    );
    assert!(
        en.contains("makes no assurance") && en.contains("Do not copy"),
        "{en}"
    );
    for conclusion in ["GPL 조건을 따릅니다", "GPL 의무"] {
        assert!(!ko.contains(conclusion), "{conclusion}: {ko}");
    }
    for conclusion in ["under GPL terms", "GPL obligations"] {
        assert!(!en.contains(conclusion), "{conclusion}: {en}");
    }
}

#[test]
fn tool_json_carries_the_distribution_notice() {
    let status = ToolStatus {
        paths: Vec::new(),
        shadowed_by: Vec::new(),
        rollback_backups: Vec::new(),
    };
    let manifest = manifest().unwrap();
    let ffmpeg = find_tool(manifest, "ffmpeg").unwrap();
    let json = tool_json(ffmpeg, &status);
    assert_eq!(json["notice"].as_str(), ffmpeg.notice.as_deref());
    assert_eq!(json["notice_en"].as_str(), ffmpeg.notice_en.as_deref());
    assert!(json["notice"].as_str().is_some_and(|n| !n.is_empty()));
    assert!(json["notice_en"].as_str().is_some_and(|n| !n.is_empty()));
    // Top level only: the CLI prompt and the dialog read it from there.
    assert!(json["download"].is_object(), "{json}");
    assert!(json["download"].get("notice").is_none(), "{json}");
    assert!(json["download"].get("notice_en").is_none(), "{json}");
    let libewf = tool_json(find_tool(manifest, "libewf").unwrap(), &status);
    assert!(libewf["notice"].is_null(), "{libewf}");
    assert!(libewf["notice_en"].is_null(), "{libewf}");
}

// ------------------------------------------------------------- URL policy

#[test]
fn url_policy_is_https_and_allowlist_only() {
    let net = NetPolicy {
        allowed_hosts: vec!["github.com".into()],
        loopback_http: false,
    };
    assert_eq!(
        check_url("https://GitHub.com/a/b.zip", &net).unwrap().host,
        "github.com"
    );
    assert!(check_url("https://github.com:443/a", &net).is_ok());
    for bad in [
        "http://github.com/a",
        "ftp://github.com/a",
        "file:///C:/a.zip",
        "https://evil.example/a",
        "https://github.com.evil.example/a",
        "https://user@github.com/a",
        "https://github.com:8443/a",
        "https://github.com/a b",
        "https://github.com\\@evil/a",
        "https://[::1]/a",
        "http://127.0.0.1/a",
    ] {
        assert!(check_url(bad, &net).is_err(), "{bad}");
    }
    // The loopback switch exists only for the unit tests' fake source.
    let test = NetPolicy::test_loopback(&["127.0.0.1"]);
    assert!(check_url("http://127.0.0.1:9/a", &test).is_ok());
    assert!(check_url("http://localhost:9/a", &test).is_err());
    assert!(redact_query("https://h/x?sig=secret").ends_with("x?…"));
}

// ---------------------------------------------------------- listing rules

#[test]
fn entry_names_climbing_or_absolute_are_unsafe() {
    for good in ["a", "a/b.exe", "a/b/", "pkg/bin/ffmpeg.exe"] {
        assert!(archive_entry_is_safe(good), "{good}");
    }
    for bad in [
        "",
        "/abs.exe",
        "../evil.exe",
        "a/../../evil.exe",
        "a\\..\\evil.exe",
        "C:/evil.exe",
        "c:evil.exe",
        "a/b.exe:stream",
        "a//b",
        "./a",
        "a/\u{fffd}.exe",
        "a/\nb",
    ] {
        assert!(!archive_entry_is_safe(bad), "{bad:?}");
    }
}

fn lines(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn listing_refuses_links_unsafe_names_and_duplicates() {
    let members = ["pkg/bin/fake.exe"];
    let names = lines(&["pkg/", "pkg/bin/", "pkg/bin/fake.exe"]);
    let verbose = lines(&[
        "drwxr-xr-x  0 0 0 0 Jan  1  1980 pkg/",
        "drwxr-xr-x  0 0 0 0 Jan  1  1980 pkg/bin/",
        "-rw-r--r--  0 0 0 4 Jan  1  1980 pkg/bin/fake.exe",
    ]);
    check_listing(&names, &verbose, &members).unwrap();

    let err = check_listing(
        &lines(&["pkg/bin/fake.exe", "../evil.exe"]),
        &lines(&["-rw", "-rw"]),
        &members,
    )
    .unwrap_err();
    assert!(err.contains("unsafe entry"), "{err}");
    for kind in ["lrwxrwxrwx", "hrw-r--r--", "crw-r--r--"] {
        let err = check_listing(
            &lines(&["pkg/bin/fake.exe", "pkg/link"]),
            &lines(&["-rw-r--r-- pkg/bin/fake.exe", &format!("{kind} pkg/link")]),
            &members,
        )
        .unwrap_err();
        assert!(err.contains("link or special"), "{kind}: {err}");
    }
    let err = check_listing(&lines(&["pkg/x"]), &lines(&["-rw"]), &members).unwrap_err();
    assert!(err.contains("not in the archive"), "{err}");
    let err = check_listing(
        &lines(&["pkg/bin/fake.exe", "PKG/bin/FAKE.exe"]),
        &lines(&["-rw", "-rw"]),
        &members,
    )
    .unwrap_err();
    assert!(err.contains("more than once"), "{err}");
    assert!(check_listing(&[], &[], &members).is_err());
}

#[test]
fn null_sha256_tool_is_refused_before_touching_disk() {
    let base = scratch("nullsha");
    let dirs = InstallDirs {
        tools_dir: base.join("tools"),
    };
    let tool = base_entry(); // sha256: None
    let err = install_entry(
        &tool,
        &dirs,
        &NetPolicy::test_loopback(&["127.0.0.1"]),
        &Progress::default(),
    )
    .unwrap_err();
    assert!(err.contains("cannot be installed automatically"), "{err}");
    assert_only_refusals(
        &dirs,
        &[if cfg!(windows) {
            "not_installable"
        } else {
            "unsupported_platform"
        }],
    );
    let _ = std::fs::remove_dir_all(base);
}

/// Nothing but the install audit log exists under `tools/`, and it holds
/// one chained `refused` line per expected code.
pub(crate) fn assert_only_refusals(dirs: &InstallDirs, codes: &[&str]) {
    let names: Vec<String> = std::fs::read_dir(&dirs.tools_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["install-audit.jsonl"], "a refusal touched the disk");
    let text = std::fs::read_to_string(dirs.audit_log()).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let got: Vec<&str> = lines.iter().map(|l| l["code"].as_str().unwrap()).collect();
    assert_eq!(got, codes, "{text}");
    for line in &lines {
        assert_eq!(line["event"], "tool-install");
        assert_eq!(line["result"], "refused");
        assert!(line["previous_entry_sha256"].is_string(), "{line}");
        assert!(line["entry_sha256"].is_string(), "{line}");
    }
    let verified = crate::audit::verify_chained_jsonl_keyed(&dirs.audit_log(), &[]).unwrap();
    assert_eq!(verified.entries, codes.len());
}

#[cfg(not(windows))]
#[test]
fn installs_report_unsupported_platform_off_windows() {
    let manifest = manifest().unwrap();
    let ffmpeg = find_tool(manifest, "ffmpeg").unwrap();
    let base = scratch("unsupported");
    let dirs = InstallDirs {
        tools_dir: base.join("tools"),
    };
    let err = install_entry(
        ffmpeg,
        &dirs,
        &NetPolicy::for_manifest(manifest),
        &Progress::default(),
    )
    .unwrap_err();
    assert!(err.contains("unsupported on this platform"), "{err}");
    assert_only_refusals(&dirs, &["unsupported_platform"]);
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn moves_are_all_or_nothing() {
    let base = scratch("moves");
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("a.exe"), b"old-a").unwrap();
    let src = base.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.exe"), b"new-a").unwrap();
    // The second source does not exist: the first move must be undone.
    let moves = vec![
        (src.join("a.exe"), bin.join("a.exe")),
        (src.join("missing.exe"), bin.join("b.exe")),
    ];
    assert!(move_all_or_nothing(&moves, &base.join("backup")).is_err());
    assert_eq!(std::fs::read(bin.join("a.exe")).unwrap(), b"old-a");
    assert!(!bin.join("b.exe").exists());
    // And a complete set replaces the old file.
    std::fs::write(src.join("a.exe"), b"new-a").unwrap();
    std::fs::write(src.join("b.exe"), b"new-b").unwrap();
    let moves = vec![
        (src.join("a.exe"), bin.join("a.exe")),
        (src.join("b.exe"), bin.join("b.exe")),
    ];
    move_all_or_nothing(&moves, &base.join("backup2")).unwrap();
    assert_eq!(std::fs::read(bin.join("a.exe")).unwrap(), b"new-a");
    assert_eq!(std::fs::read(bin.join("b.exe")).unwrap(), b"new-b");
    let _ = std::fs::remove_dir_all(base);
}

/// Fix round 11 H4: a just-extracted file held open for a moment (an AV
/// scan: no sharing) is moved once it is released; one held for longer
/// than the retry budget still fails all-or-nothing (the old file stays).
#[cfg(windows)]
#[test]
fn a_briefly_locked_file_is_moved_after_a_retry_and_a_held_one_rolls_back() {
    use std::os::windows::fs::OpenOptionsExt;
    let hold = |path: PathBuf, millis: u64| {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(millis));
            drop(file);
        })
    };
    let base = scratch("moves-retry");
    let bin = base.join("bin");
    let src = base.join("src");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(bin.join("a.exe"), b"old-a").unwrap();
    std::fs::write(src.join("a.exe"), b"new-a").unwrap();
    std::fs::write(src.join("b.exe"), b"new-b").unwrap();
    // Held for 500 ms: the rename is retried and succeeds.
    let started = std::time::Instant::now();
    let holder = hold(src.join("b.exe"), 500);
    let moves = vec![
        (src.join("a.exe"), bin.join("a.exe")),
        (src.join("b.exe"), bin.join("b.exe")),
    ];
    move_all_or_nothing(&moves, &base.join("backup")).unwrap();
    holder.join().unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(400));
    assert_eq!(std::fs::read(bin.join("a.exe")).unwrap(), b"new-a");
    assert_eq!(std::fs::read(bin.join("b.exe")).unwrap(), b"new-b");

    // Held past the budget: fails, the first move is undone.
    std::fs::write(src.join("a.exe"), b"newer-a").unwrap();
    std::fs::write(src.join("c.exe"), b"new-c").unwrap();
    let holder = hold(src.join("c.exe"), 4000);
    let started = std::time::Instant::now();
    let moves = vec![
        (src.join("a.exe"), bin.join("a.exe")),
        (src.join("c.exe"), bin.join("c.exe")),
    ];
    let err = move_all_or_nothing(&moves, &base.join("backup2")).unwrap_err();
    let waited = started.elapsed();
    holder.join().unwrap();
    assert!(err.contains("c.exe"), "{err}");
    assert!(
        waited >= std::time::Duration::from_millis(1500)
            && waited < std::time::Duration::from_secs(4),
        "bounded retry: {waited:?}"
    );
    assert_eq!(
        std::fs::read(bin.join("a.exe")).unwrap(),
        b"new-a",
        "rolled back"
    );
    assert!(!bin.join("c.exe").exists());
    // Not a transient error (missing source): no retry, no wait.
    let started = std::time::Instant::now();
    assert!(rename_with_retry(&src.join("missing.exe"), &bin.join("m.exe")).is_err());
    assert!(started.elapsed() < std::time::Duration::from_millis(300));
    let _ = std::fs::remove_dir_all(base);
}

/// Fix round 12 H-a: the rollback itself retries. A later move fails
/// (its source held past the budget) while the file the first move just
/// put in place is held too (an AV scan of the new ffmpeg.exe):
/// - held briefly → the rollback's remove is retried, the old file is back;
/// - held past the rollback's budget → `rollback_incomplete`, never silent:
///   the error names the file left wrong and where the previous copy is,
///   which is still there; `preserve_rollback_backup` moves it out of the
///   staging directory, and the audit line carries the distinct code.
#[cfg(windows)]
#[test]
fn a_rollback_blocked_by_a_held_new_file_is_retried_or_reported_loudly() {
    use std::os::windows::fs::OpenOptionsExt;
    let hold_now = |path: &Path| {
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .unwrap()
    };
    // Holds `dest` as soon as the first move has put the new file there
    // (its backup exists and `dest` exists again), for `millis`.
    let hold_new_when_moved = |dest: PathBuf, backup: PathBuf, millis: u64| {
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if backup.exists()
                    && dest.exists()
                    && let Ok(file) = std::fs::OpenOptions::new()
                        .read(true)
                        .share_mode(0)
                        .open(&dest)
                {
                    std::thread::sleep(std::time::Duration::from_millis(millis));
                    drop(file);
                    return true;
                }
                if std::time::Instant::now() > deadline {
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        })
    };
    for (case, hold_new_ms) in [("brief", 2200u64), ("held", 6000u64)] {
        let base = scratch(&format!("rollback-{case}"));
        let bin = base.join("bin");
        let src = base.join("src");
        let backup_dir = base.join("staging/backup");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(bin.join("a.exe"), b"old-a").unwrap();
        std::fs::write(src.join("a.exe"), b"new-a").unwrap();
        std::fs::write(src.join("c.exe"), b"new-c").unwrap();
        // c's source is held for 4 s: its move fails after ~1.6 s.
        let c_hold = hold_now(&src.join("c.exe"));
        let c_holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(4000));
            drop(c_hold);
        });
        let a_holder =
            hold_new_when_moved(bin.join("a.exe"), backup_dir.join("0.bak"), hold_new_ms);
        let moves = vec![
            (src.join("a.exe"), bin.join("a.exe")),
            (src.join("c.exe"), bin.join("c.exe")),
        ];
        let err = move_all_or_nothing(&moves, &backup_dir).unwrap_err();
        assert!(a_holder.join().unwrap(), "{case}: the new a.exe was held");
        c_holder.join().unwrap();
        assert!(err.contains("c.exe"), "{case}: {err}");
        assert!(!bin.join("c.exe").exists(), "{case}");
        if case == "brief" {
            assert!(
                !err.starts_with(ROLLBACK_INCOMPLETE_PREFIX),
                "{case}: the rollback retried and completed: {err}"
            );
            assert_eq!(
                std::fs::read(bin.join("a.exe")).unwrap(),
                b"old-a",
                "{case}"
            );
            assert!(!backup_dir.join("0.bak").exists(), "{case}: restored");
        } else {
            assert!(err.starts_with(ROLLBACK_INCOMPLETE_PREFIX), "{case}: {err}");
            assert!(
                err.contains(&bin.join("a.exe").display().to_string())
                    && err.contains("NEW file")
                    && err.contains(&backup_dir.join("0.bak").display().to_string()),
                "{case}: names the file and the kept copy: {err}"
            );
            assert_eq!(
                std::fs::read(backup_dir.join("0.bak")).unwrap(),
                b"old-a",
                "{case}: the previous file is kept"
            );
            // The installer keeps it out of the staging dir it drops.
            let tools = base.join("tools");
            std::fs::create_dir_all(&tools).unwrap();
            let staging = StagingDir::create(&base, ".staging-").unwrap();
            let staging_path = staging.path().to_path_buf();
            let staged_backup = staging_path.join("backup");
            std::fs::rename(&backup_dir, &staged_backup).unwrap();
            let staged_err = err.replace(
                &backup_dir.display().to_string(),
                &staged_backup.display().to_string(),
            );
            let final_err =
                preserve_rollback_backup(staged_err, staging, &staged_backup, &tools, "fake");
            assert!(!staging_path.exists(), "the staging dir is removed");
            let kept: Vec<PathBuf> = std::fs::read_dir(&tools)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            assert_eq!(kept.len(), 1, "{kept:?}");
            let kept = &kept[0];
            assert!(
                kept.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("rollback-backup-"),
                "{kept:?}"
            );
            assert_eq!(std::fs::read(kept.join("0.bak")).unwrap(), b"old-a");
            // Fix round 13 L1: RESTORE.txt maps the kept copy back to its
            // original path and names the tool; doctor/api list it.
            let restore = std::fs::read_to_string(kept.join(RESTORE_FILE)).unwrap();
            assert!(
                restore.starts_with(
                    "tool: fake
"
                ),
                "{restore}"
            );
            assert!(
                restore
                    .lines()
                    .any(|line| line == format!("{} <- 0.bak", bin.join("a.exe").display())),
                "{restore}"
            );
            assert!(restore.contains("To restore by hand"), "{restore}");
            assert!(final_err.contains(&kept.join(RESTORE_FILE).display().to_string()));
            let listed = rollback_backups(&tools);
            assert_eq!(listed.len(), 1, "{listed:?}");
            assert_eq!(&listed[0].path, kept);
            assert_eq!(listed[0].tool.as_deref(), Some("fake"));
            assert!(listed[0].size_bytes >= 5);
            assert!(
                final_err.starts_with(ROLLBACK_INCOMPLETE_PREFIX)
                    && final_err.contains(&kept.join("0.bak").display().to_string())
                    && !final_err.contains(&staging_path.display().to_string()),
                "{final_err}"
            );
            // The audit line carries the distinct code.
            let mut tool = base_entry();
            tool.url = Some("https://github.com/o/r/fake.zip".into());
            let line: serde_json::Value =
                serde_json::from_str(&audit_line(&tool, Err(&final_err))).unwrap();
            assert_eq!(line["result"], "failed");
            assert_eq!(line["code"], "rollback_incomplete");
        }
        let _ = std::fs::remove_dir_all(base);
    }
}

/// Fix round 14 L4: when `move_all_or_nothing` left no `RESTORE.txt`,
/// `preserve_rollback_backup` creates one (tool line, how to restore, a
/// note that the list is in the error) so the backup is attributed; when
/// even that cannot be written (here a directory of that name) the error
/// says so instead of pointing at the file, and the kept staging dir /
/// backup is still never swept.
#[test]
fn a_missing_or_unwritable_restore_file_is_created_or_reported() {
    let incomplete = format!("{ROLLBACK_INCOMPLETE_PREFIX}boom");
    // Missing: created, and the backup is listed under the tool.
    let base = scratch("restore-missing");
    let tools = base.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let staging = StagingDir::create(&tools, ".staging-").unwrap();
    let backup = staging.path().join("backup");
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("0.bak"), b"old").unwrap();
    let err = preserve_rollback_backup(incomplete.clone(), staging, &backup, &tools, "fake");
    let listed = rollback_backups(&tools);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].tool.as_deref(), Some("fake"));
    let text = std::fs::read_to_string(&listed[0].restore_file).unwrap();
    assert!(text.starts_with("tool: fake\n"), "{text}");
    assert!(text.contains("To restore by hand"), "{text}");
    assert!(text.ends_with(rollback::MISSING_LIST_NOTE), "{text}");
    assert!(
        err.contains(&format!(
            "{} lists each one",
            listed[0].restore_file.display()
        )),
        "{err}"
    );
    assert!(!err.contains("could not be written"), "{err}");
    let _ = std::fs::remove_dir_all(&base);

    // Unwritable (a directory named RESTORE.txt): reported, not claimed.
    let base = scratch("restore-unwritable");
    let tools = base.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let staging = StagingDir::create(&tools, ".staging-").unwrap();
    let backup = staging.path().join("backup");
    std::fs::create_dir_all(backup.join(RESTORE_FILE)).unwrap();
    std::fs::write(backup.join("0.bak"), b"old").unwrap();
    let err = preserve_rollback_backup(incomplete.clone(), staging, &backup, &tools, "fake");
    assert!(err.starts_with(ROLLBACK_INCOMPLETE_PREFIX), "{err}");
    assert!(err.contains("(RESTORE.txt could not be written: "), "{err}");
    assert!(!err.contains("RESTORE.txt lists each one"), "{err}");
    assert!(err.contains("this message lists each one"), "{err}");
    let listed = rollback_backups(&tools);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].tool, None, "no tool line to read");
    assert!(listed[0].path.join("0.bak").is_file());
    let _ = std::fs::remove_dir_all(&base);

    // A staging dir kept with such a RESTORE.txt is not swept.
    let base = scratch("restore-unwritable-staging");
    let tools = base.join("tools");
    let kept = tools.join(".staging-kept").join("backup");
    std::fs::create_dir_all(kept.join(RESTORE_FILE)).unwrap();
    std::fs::write(kept.join("0.bak"), b"old").unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    assert_eq!(clean_stale_staging(&tools, later, &[]), 0);
    assert!(kept.join("0.bak").is_file());
    assert_eq!(rollback_backups(&tools).len(), 1);
    let _ = std::fs::remove_dir_all(&base);

    // undo_venv: the previous venv is kept, RESTORE.txt fails to write →
    // the error says so (and still names where the venv is and where it
    // goes); the kept dir is attributed by its sub-folder name.
    let base = scratch("restore-venv-unwritable");
    let python = base.join("python");
    let old = python.join(".lens.old-00aa");
    std::fs::create_dir_all(old.join("Lib")).unwrap();
    std::fs::write(old.join("Lib").join("marker.txt"), b"good").unwrap();
    // A *file* at the target: remove_dir_all fails (not a directory).
    let target = python.join("lens");
    std::fs::write(&target, b"not a venv").unwrap();
    rollback::FAIL_RESTORE_WRITE.with(|fail| fail.set(true));
    let err = rollback::undo_venv("pip failed".into(), "lens", &target, Some(&old), &python);
    rollback::FAIL_RESTORE_WRITE.with(|fail| fail.set(false));
    assert!(err.starts_with(ROLLBACK_INCOMPLETE_PREFIX), "{err}");
    assert!(
        err.contains("(RESTORE.txt could not be written: test: RESTORE.txt write fails)"),
        "{err}"
    );
    assert!(!err.contains("says how"), "{err}");
    assert!(
        err.contains(&format!("move it back to {}", target.display())),
        "{err}"
    );
    let listed = rollback_backups(&base);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].tool.as_deref(), Some("lens"));
    assert!(!listed[0].restore_file.exists());
    assert!(
        listed[0]
            .path
            .join("lens")
            .join("Lib")
            .join("marker.txt")
            .is_file()
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Fix round 12 H-a: an ordinary failure passes `preserve_rollback_backup`
/// unchanged and the staging dir is removed as before.
#[test]
fn preserve_rollback_backup_leaves_other_errors_alone() {
    let base = scratch("rollback-other");
    let staging = StagingDir::create(&base, ".staging-").unwrap();
    let path = staging.path().to_path_buf();
    std::fs::create_dir_all(path.join("backup")).unwrap();
    let err = preserve_rollback_backup("boom".into(), staging, &path.join("backup"), &base, "fake");
    assert_eq!(err, "boom");
    assert!(!path.exists());
    let line: serde_json::Value =
        serde_json::from_str(&audit_line(&base_entry(), Err(&err))).unwrap();
    assert!(line.get("code").is_none(), "{line}");
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn audit_line_records_source_hash_and_result_without_signed_query() {
    let mut tool = base_entry();
    tool.url = Some("https://github.com/o/r/fake.zip".into());
    tool.sha256 = Some("b".repeat(64));
    let mut outcome = InstallOutcome {
        installed_files: vec![PathBuf::from("x/fake.exe")],
        ..Default::default()
    };
    let ok = audit_line(&tool, Ok(&outcome));
    let value: serde_json::Value = serde_json::from_str(&ok).unwrap();
    assert_eq!(value["event"], "tool-install");
    assert_eq!(value["result"], "ok");
    assert!(value.get("staging_left").is_none(), "{value}");
    outcome.staging_left = Some(PathBuf::from("tools/.staging-x"));
    let left: serde_json::Value = serde_json::from_str(&audit_line(&tool, Ok(&outcome))).unwrap();
    assert_eq!(left["result"], "ok");
    assert_eq!(
        left["staging_left"],
        PathBuf::from("tools/.staging-x").display().to_string()
    );
    assert_eq!(value["host"], "github.com");
    assert_eq!(value["sha256"], "b".repeat(64));
    let failed = audit_line(&tool, Err(&"boom".to_string()));
    let value: serde_json::Value = serde_json::from_str(&failed).unwrap();
    assert_eq!(value["result"], "failed");
    assert_eq!(value["error"], "boom");
}

#[test]
fn shipped_ffmpeg_entry_installs_the_pinned_gplv3_text() {
    let manifest = manifest().unwrap();
    let ffmpeg = find_tool(manifest, "ffmpeg").unwrap();
    assert_eq!(ffmpeg.bundled_licenses.len(), 1);
    let gpl = &ffmpeg.bundled_licenses[0];
    assert_eq!(gpl.file, "tools/licenses/COPYING.GPLv3");
    assert_eq!(gpl.to, "ffmpeg-COPYING.GPLv3.txt");
    let text = bundled_license_text(&gpl.file).unwrap();
    assert_eq!(crate::sha256::digest_bytes(text.as_bytes()), gpl.sha256);
    assert!(text.contains("GNU GENERAL PUBLIC LICENSE"));
    assert!(text.contains("Version 3, 29 June 2007"));
    assert!(
        !text.contains('\r'),
        "kept byte-exact (LF), see .gitattributes"
    );
}

#[test]
fn bundled_license_must_match_its_pin_and_be_compiled_in() {
    let mut tool = base_entry();
    tool.bundled_licenses = vec![BundledLicense {
        file: "tools/licenses/COPYING.GPLv3".into(),
        to: "x-COPYING.GPLv3.txt".into(),
        sha256: "0".repeat(64),
    }];
    let err = validate_manifest(&manifest_with(tool.clone())).unwrap_err();
    assert!(err.contains("does not match its pinned sha256"), "{err}");
    tool.bundled_licenses[0].file = "tools/licenses/NOPE".into();
    let err = validate_manifest(&manifest_with(tool.clone())).unwrap_err();
    assert!(err.contains("is not compiled in"), "{err}");
    tool.bundled_licenses[0].file = "tools/licenses/COPYING.GPLv3".into();
    tool.bundled_licenses[0].sha256 =
        "8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903".into();
    tool.bundled_licenses[0].to = "../escape.txt".into();
    let err = validate_manifest(&manifest_with(tool.clone())).unwrap_err();
    assert!(err.contains("unsafe license name"), "{err}");
    tool.bundled_licenses[0].to = "x-COPYING.GPLv3.txt".into();
    validate_manifest(&manifest_with(tool)).unwrap();
}

#[test]
fn cancel_is_honoured_until_a_non_cancellable_stage_then_refused() {
    // Requested first: the next boundary stops, the locked stage is never
    // entered.
    let progress = Progress::default();
    assert!(progress.cancellable());
    progress.check_cancel().unwrap();
    progress.request_cancel().unwrap();
    progress.request_cancel().unwrap(); // idempotent
    assert!(progress.cancelled());
    assert_eq!(progress.check_cancel().unwrap_err(), INSTALL_CANCELLED);
    assert_eq!(
        progress.enter_non_cancellable(Stage::Install).unwrap_err(),
        INSTALL_CANCELLED
    );
    assert_ne!(progress.stage(), Stage::Install);

    // Locked first: the cancel is refused and changes nothing.
    let progress = Progress::default();
    progress.set_stage(Stage::Extract);
    progress.enter_non_cancellable(Stage::Install).unwrap();
    assert_eq!(progress.stage(), Stage::Install);
    assert!(!progress.cancellable());
    assert_eq!(progress.request_cancel().unwrap_err(), NOT_CANCELLABLE);
    assert!(!progress.cancelled());
    progress.check_cancel().unwrap();
    // A later non-cancellable stage (pip after venv) stays entered.
    progress.enter_non_cancellable(Stage::Pip).unwrap();
    assert_eq!(progress.stage(), Stage::Pip);
}

/// A venv's interpreter and the console script `name` (what
/// `venv_intact` looks for).
fn write_venv_launcher(venv: &Path, name: &str) {
    std::fs::create_dir_all(pip::venv_python(venv).parent().unwrap()).unwrap();
    std::fs::write(pip::venv_python(venv), b"py").unwrap();
    std::fs::write(pip::venv_script(venv, name), b"exe").unwrap();
}

#[test]
fn stale_staging_dirs_are_swept_but_not_live_or_foreign_entries() {
    let base = scratch("stale");
    let tools = base.join("tools");
    std::fs::create_dir_all(tools.join(".staging-old1/x")).unwrap();
    std::fs::write(tools.join(".staging-old1/x/download.part"), b"partial").unwrap();
    std::fs::create_dir_all(tools.join(".staging-old2")).unwrap();
    std::fs::write(tools.join(".staging-file"), b"not a dir").unwrap();
    std::fs::create_dir_all(tools.join("bin")).unwrap();
    // Fix round 3: a replaced pip venv that could not be removed (a
    // process still ran from it) — reported as staging_left — is swept
    // too; the live venv and other python/ entries are not.
    let python = tools.join("python");
    std::fs::create_dir_all(python.join(".deepfake-lens.old-0123abcd/Scripts")).unwrap();
    std::fs::create_dir_all(python.join("deepfake-lens/Scripts")).unwrap();
    // Fix round 13 M: only once the live venv works.
    write_venv_launcher(&python.join("deepfake-lens"), "deepfake-lens");
    std::fs::write(python.join(".x.old-file"), b"not a dir").unwrap();
    // Fix round 4: a `<dir>.rm` whose removal failed earlier is swept too.
    std::fs::create_dir_all(tools.join(".staging-old3.rm/x")).unwrap();
    std::fs::create_dir_all(python.join(".y.old-0a.rm")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let run_started = std::time::SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(50));
    // Created by "this" run: newer than its start, kept.
    std::fs::create_dir_all(tools.join(".staging-live")).unwrap();
    std::fs::create_dir_all(python.join(".other.old-live")).unwrap();
    assert_eq!(clean_stale_staging(&tools, run_started, &[]), 5);
    assert!(!tools.join(".staging-old3.rm").exists());
    assert!(!python.join(".y.old-0a.rm").exists());
    for dir in [&tools, &python] {
        let rm: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".rm"))
            .collect();
        assert!(rm.is_empty(), "no .rm left: {rm:?}");
    }
    assert!(!tools.join(".staging-old1").exists());
    assert!(!tools.join(".staging-old2").exists());
    assert!(!python.join(".deepfake-lens.old-0123abcd").exists());
    assert!(python.join("deepfake-lens/Scripts").is_dir());
    assert!(python.join(".x.old-file").is_file());
    assert!(python.join(".other.old-live").is_dir());
    assert!(tools.join(".staging-live").is_dir());
    assert!(tools.join(".staging-file").is_file());
    assert!(tools.join("bin").is_dir());
    // A missing tools dir is fine and is not created.
    assert_eq!(clean_stale_staging(&base.join("nope"), run_started, &[]), 0);
    assert_eq!(clean_stale_staging_if_unlocked(&base.join("nope"), &[]), 0);
    assert!(!base.join("nope").exists());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn server_start_sweep_skips_while_another_install_holds_the_lock() {
    let base = scratch("stale-locked");
    let tools = base.join("tools");
    std::fs::create_dir_all(tools.join(".staging-dead")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    {
        let _held = InstallLock::acquire(&tools).unwrap();
        assert_eq!(clean_stale_staging_if_unlocked(&tools, &[]), 0);
        assert!(
            tools.join(".staging-dead").is_dir(),
            "swept under a held lock"
        );
    }
    assert_eq!(clean_stale_staging_if_unlocked(&tools, &[]), 1);
    assert!(!tools.join(".staging-dead").exists());
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn refusals_are_chained_onto_existing_install_audit_lines() {
    let base = scratch("refusal-chain");
    let dirs = InstallDirs {
        tools_dir: base.join("tools"),
    };
    let tool = base_entry();
    append_audit(
        &dirs.audit_log(),
        &audit_line(&tool, Err(&"boom".to_string())),
    )
    .unwrap();
    let long_name = "x".repeat(500);
    audit_refusal(&dirs, &long_name, "unknown_tool", "no such tool").unwrap();
    audit_refusal(&dirs, "ffmpeg", "busy", "busy \"quoted\"\nline").unwrap();
    let text = std::fs::read_to_string(dirs.audit_log()).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "one line per event, no injected lines");
    assert_eq!(lines[0]["result"], "failed");
    assert_eq!(lines[0]["previous_entry_sha256"], "GENESIS");
    assert_eq!(lines[1]["result"], "refused");
    assert_eq!(lines[1]["tool"].as_str().unwrap().len(), 64, "name bounded");
    assert_eq!(lines[2]["code"], "busy");
    assert_eq!(lines[2]["error"], "busy \"quoted\"\nline");
    let first = text.lines().next().unwrap();
    assert_eq!(
        lines[1]["previous_entry_sha256"],
        crate::sha256::digest_bytes(first.as_bytes())
    );
    let verified = crate::audit::verify_chained_jsonl_keyed(&dirs.audit_log(), &[]).unwrap();
    assert_eq!(verified.entries, 3);
    // Editing a line breaks the chain.
    std::fs::write(dirs.audit_log(), text.replace("\"busy\"", "\"ok\"")).unwrap();
    assert!(crate::audit::verify_chained_jsonl_keyed(&dirs.audit_log(), &[]).is_err());
    let _ = std::fs::remove_dir_all(base);
}

// ------------------------------------------------------------------- pip

fn pip_tool() -> ToolEntry {
    let mut tool = base_entry();
    tool.name = "lens".into();
    tool.kind = ToolKind::Pip;
    tool.on_demand = true;
    tool.package = Some("deepfake-lens".into());
    tool.version = Some("1.2.3".into());
    tool.index_url = Some(pip::PINNED_INDEX_URL.into());
    tool.files.clear();
    tool.license_files.clear();
    tool.provides = vec!["lens".into()];
    tool
}

fn good_requirements() -> String {
    format!(
        "# pinned\ndeepfake-lens==1.2.3 \\\n    --hash=sha256:{}\nnumpy==2.0.0 --hash=sha256:{} --hash=sha256:{}\n",
        "1".repeat(64),
        "2".repeat(64),
        "3".repeat(64)
    )
}

#[test]
fn pip_requirements_must_be_exact_pins_with_hashes_only() {
    let tool = pip_tool();
    pip::check_requirements(&good_requirements(), &tool).unwrap();
    let hash = format!("--hash=sha256:{}", "1".repeat(64));
    for (text, why) in [
        ("deepfake-lens==1.2.3\n".to_string(), "only --hash"),
        (format!("deepfake-lens>=1.2 {hash}\n"), "exact pin"),
        (format!("deepfake-lens==9.9.9 {hash}\n"), "manifest version"),
        (format!("numpy==2.0.0 {hash}\n"), "own package"),
        (
            format!("deepfake-lens==1.2.3 {hash} --extra-index-url https://evil\n"),
            "only --hash",
        ),
        (
            format!("-i https://evil/simple\ndeepfake-lens==1.2.3 {hash}\n"),
            "exact pin",
        ),
        (format!("-e .\ndeepfake-lens==1.2.3 {hash}\n"), "exact pin"),
        (
            format!("deepfake-lens==1.2.3 --hash=md5:{}\n", "1".repeat(32)),
            "only --hash",
        ),
        (String::new(), "own package"),
    ] {
        let err = pip::check_requirements(&text, &tool).unwrap_err();
        assert!(err.contains(why), "{text:?}: {err}");
    }
}

#[test]
fn pip_runs_with_hashes_required_no_deps_wheels_only_and_pinned_index() {
    let args: Vec<String> = pip::pip_install_args(Path::new("req.txt"))
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    for flag in [
        "-I",
        "--isolated",
        "--require-hashes",
        "--no-deps",
        "--only-binary",
        ":all:",
        "--no-input",
    ] {
        assert!(args.iter().any(|a| a == flag), "{flag} in {args:?}");
    }
    let index = args.iter().position(|a| a == "--index-url").unwrap();
    assert_eq!(args[index + 1], "https://pypi.org/simple");
    assert!(!args.iter().any(|a| a.contains("extra-index")));
    assert_eq!(args.last().unwrap(), "req.txt");
}

#[test]
fn missing_python_is_a_clear_hint() {
    let err = pip::RealPython::from_resolution(Err("tool 'python' not found".into()))
        .err()
        .unwrap();
    assert!(err.starts_with("Python 필요"), "{err}");
}

/// Fake interpreter: records calls, writes the console script, or fails.
struct FakePython {
    fail_pip: bool,
    write_script: bool,
    calls: std::sync::Mutex<Vec<String>>,
}

impl pip::PythonOps for FakePython {
    fn create_venv(&self, venv: &Path) -> Result<(), String> {
        self.calls.lock().unwrap().push("venv".into());
        std::fs::create_dir_all(pip::venv_python(venv).parent().unwrap()).unwrap();
        std::fs::write(pip::venv_python(venv), b"py").unwrap();
        Ok(())
    }
    fn pip_install(&self, venv: &Path, requirements: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(requirements).unwrap();
        self.calls
            .lock()
            .unwrap()
            .push(format!("pip:{}", text.len()));
        if self.fail_pip {
            return Err("pip install failed (exit 1): hash mismatch".into());
        }
        if self.write_script {
            std::fs::write(pip::venv_script(venv, "lens"), b"exe").unwrap();
        }
        Ok(())
    }
}

#[test]
fn pip_install_builds_private_venv_and_restores_the_old_one_on_failure() {
    let base = scratch("pip");
    let dirs = InstallDirs {
        tools_dir: base.join("tools"),
    };
    let tool = pip_tool();
    let req = good_requirements();
    let ok = FakePython {
        fail_pip: false,
        write_script: true,
        calls: Default::default(),
    };
    let outcome = pip::install_pip(&tool, &req, &dirs, &ok, &Progress::default()).unwrap();
    let exe = pip::venv_script(&dirs.python_env("lens"), "lens");
    assert_eq!(outcome.installed_files, vec![exe.clone()]);
    assert!(exe.is_file());
    assert_eq!(ok.calls.lock().unwrap()[0], "venv");

    // A failing reinstall leaves the working env exactly as it was.
    let failing = FakePython {
        fail_pip: true,
        write_script: false,
        calls: Default::default(),
    };
    let err = pip::install_pip(&tool, &req, &dirs, &failing, &Progress::default()).unwrap_err();
    assert!(err.contains("hash mismatch"), "{err}");
    assert!(exe.is_file(), "old venv restored");
    // pip "succeeding" without producing the script is a failure too.
    let hollow = FakePython {
        fail_pip: false,
        write_script: false,
        calls: Default::default(),
    };
    std::fs::remove_dir_all(dirs.python_env("lens")).unwrap();
    let err = pip::install_pip(&tool, &req, &dirs, &hollow, &Progress::default()).unwrap_err();
    assert!(err.contains("missing"), "{err}");
    assert!(!dirs.python_env("lens").exists(), "partial venv removed");
    // Unhashed requirements never reach the interpreter.
    let never = FakePython {
        fail_pip: false,
        write_script: true,
        calls: Default::default(),
    };
    assert!(
        pip::install_pip(
            &tool,
            "deepfake-lens==1.2.3\n",
            &dirs,
            &never,
            &Progress::default()
        )
        .is_err()
    );
    assert!(never.calls.lock().unwrap().is_empty());
    // No staging directory survives any of the above.
    let leftovers: Vec<_> = std::fs::read_dir(&dirs.tools_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".staging-"))
        .collect();
    assert!(leftovers.is_empty());
    let _ = std::fs::remove_dir_all(base);
}

/// Fix round 13 M: a replaced venv `.<tool>.old-*` is swept only once the
/// live `<tool>/` is a working venv again — while it is broken (an
/// incomplete undo left a partial env) the old one may be the only good
/// copy and is kept.
#[test]
fn the_sweep_keeps_an_old_venv_while_the_live_one_is_broken() {
    let base = scratch("sweep-broken-live");
    let tools = base.join("tools");
    let python = tools.join("python");
    let old = python.join(".lens.old-00aa");
    std::fs::create_dir_all(old.join("Lib")).unwrap();
    std::fs::write(old.join("Lib").join("marker.txt"), b"good").unwrap();
    // Partial live venv: an interpreter but no console script.
    let live = python.join("lens");
    std::fs::create_dir_all(pip::venv_python(&live).parent().unwrap()).unwrap();
    std::fs::write(pip::venv_python(&live), b"py").unwrap();
    // No live venv at all for another tool.
    std::fs::create_dir_all(python.join(".gone.old-00bb")).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    assert_eq!(clean_stale_staging(&tools, later, &[]), 0);
    assert!(old.join("Lib").join("marker.txt").is_file());
    assert!(python.join(".gone.old-00bb").is_dir());
    // Once the live venv works, the old one goes.
    write_venv_launcher(&live, "lens");
    assert_eq!(clean_stale_staging(&tools, later, &[]), 1);
    assert!(!old.exists());
    assert!(python.join(".gone.old-00bb").is_dir());
    assert_eq!(
        old_venv_base(".deepfake-lens.old-0123abcd"),
        Some("deepfake-lens")
    );
    assert_eq!(old_venv_base(".old-12"), None);
    let _ = std::fs::remove_dir_all(base);
}

/// Fix round 14 L5: the sweep checks a pip tool's venv for its
/// `provides[0]` console script (what install checks), not for a script
/// named after the tool — a tool whose name differs from `provides[0]`
/// no longer keeps `.<tool>.old-*` forever.
#[test]
fn the_sweep_checks_the_provided_console_script() {
    let base = scratch("sweep-provides");
    let tools = base.join("tools");
    let python = tools.join("python");
    let mut tool = pip_tool();
    tool.name = "lens-kit".into();
    tool.provides = vec!["lens-cli".into()];
    let old = python.join(".lens-kit.old-00aa");
    std::fs::create_dir_all(&old).unwrap();
    let live = python.join("lens-kit");
    write_venv_launcher(&live, "lens-cli");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    // Without the tool list: looked for `lens-kit` → kept (the old bug).
    assert_eq!(clean_stale_staging(&tools, later, &[]), 0);
    assert!(old.is_dir());
    // An archive tool of that name does not count.
    let mut archive = tool.clone();
    archive.kind = ToolKind::Archive;
    assert_eq!(clean_stale_staging(&tools, later, &[&archive]), 0);
    // With it: `lens-cli` is there → the old venv goes.
    assert_eq!(venv_script_name("lens-kit", &[&tool]), "lens-cli");
    assert_eq!(venv_script_name("other", &[&tool]), "other");
    assert_eq!(clean_stale_staging(&tools, later, &[&tool]), 1);
    assert!(!old.exists());
    // A live venv with only a script named after the tool is not intact.
    let old = python.join(".lens-kit.old-00bb");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::remove_file(pip::venv_script(&live, "lens-cli")).unwrap();
    std::fs::write(pip::venv_script(&live, "lens-kit"), b"exe").unwrap();
    assert_eq!(clean_stale_staging(&tools, later, &[&tool]), 0);
    assert!(old.is_dir());
    let _ = std::fs::remove_dir_all(base);
}

/// Fix round 13 L1: a staging dir kept because its backup could not be
/// moved out (it holds `backup/RESTORE.txt`) is never swept, and is
/// listed as a rollback backup; kept `rollback-backup-*` dirs are listed
/// with their tool.
#[test]
fn kept_rollback_backups_are_listed_and_never_swept() {
    let base = scratch("kept-backups");
    let tools = base.join("tools");
    let kept_staging = tools.join(".staging-kept");
    std::fs::create_dir_all(kept_staging.join("backup")).unwrap();
    std::fs::write(kept_staging.join("backup").join("0.bak"), b"old").unwrap();
    std::fs::write(
        kept_staging.join("backup").join(RESTORE_FILE),
        rollback::restore_text(
            Some("ffmpeg"),
            &[(
                tools.join("bin").join("ffmpeg.exe"),
                Some(kept_staging.join("backup").join("0.bak")),
            )],
            &kept_staging.join("backup"),
        ),
    )
    .unwrap();
    std::fs::create_dir_all(tools.join(".staging-plain")).unwrap();
    let archive_kept = tools.join("rollback-backup-00cc");
    std::fs::create_dir_all(&archive_kept).unwrap();
    // A round-12 leftover without RESTORE.txt: listed, tool unknown.
    std::fs::write(archive_kept.join("0.bak"), b"old").unwrap();
    let venv_kept = tools.join("python").join("rollback-backup-00dd");
    std::fs::create_dir_all(venv_kept.join("lens").join("Lib")).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    assert_eq!(
        clean_stale_staging(&tools, later, &[]),
        1,
        "only .staging-plain"
    );
    assert!(kept_staging.join("backup").join("0.bak").is_file());
    assert!(archive_kept.is_dir() && venv_kept.is_dir());
    let listed = rollback_backups(&tools);
    let summary: Vec<(PathBuf, Option<String>)> = listed
        .iter()
        .map(|b| (b.path.clone(), b.tool.clone()))
        .collect();
    assert_eq!(
        summary,
        vec![
            (kept_staging.clone(), Some("ffmpeg".to_string())),
            (archive_kept.clone(), None),
            (venv_kept.clone(), Some("lens".to_string())),
        ]
    );
    assert_eq!(
        listed[0].restore_file,
        kept_staging.join("backup").join(RESTORE_FILE)
    );
    let text = std::fs::read_to_string(&listed[0].restore_file).unwrap();
    assert!(
        text.lines().any(|line| line
            == format!(
                "{} <- 0.bak",
                tools.join("bin").join("ffmpeg.exe").display()
            )),
        "{text}"
    );
    assert!(rollback_hint(&listed[0]).contains(&kept_staging.display().to_string()));
    let _ = std::fs::remove_dir_all(base);
}

/// Fix round 13 L2: a tool with a kept rollback backup carries
/// `rollback_incomplete: true` and the note in `/api/tools` (tool_json).
#[test]
fn tool_json_flags_a_kept_rollback_backup() {
    let manifest = manifest().unwrap();
    let ffmpeg = find_tool(manifest, "ffmpeg").unwrap();
    let clean = ToolStatus {
        paths: Vec::new(),
        shadowed_by: Vec::new(),
        rollback_backups: Vec::new(),
    };
    let json = tool_json(ffmpeg, &clean);
    assert_eq!(json["rollback_incomplete"], false);
    assert!(json["rollback_note"].is_null());
    let backup = RollbackBackup {
        path: PathBuf::from("C:/FT/tools/rollback-backup-00ee"),
        restore_file: PathBuf::from("C:/FT/tools/rollback-backup-00ee/RESTORE.txt"),
        tool: Some("ffmpeg".into()),
        size_bytes: 3 * 1024 * 1024,
    };
    let status = ToolStatus {
        paths: vec![("ffmpeg".into(), Some("C:/FT/tools/bin/ffmpeg.exe".into()))],
        shadowed_by: Vec::new(),
        rollback_backups: vec![backup],
    };
    let json = tool_json(ffmpeg, &status);
    assert_eq!(json["rollback_incomplete"], true);
    let note = json["rollback_note"].as_str().unwrap();
    assert!(
        note.contains("rollback-backup-00ee") && note.contains("3.0 MB"),
        "{note}"
    );
    assert_eq!(
        json["rollback_backups"][0],
        Path::new("C:/FT/tools/rollback-backup-00ee")
            .display()
            .to_string()
    );
}

/// Fix round 13 M: pip fails and one file of the new, partial venv is
/// held (an AV scan / the indexer: no sharing), so removing it fails:
/// - held briefly → the remove is retried, the previous venv is back;
/// - held past the budget → `rollback_incomplete`, never silent; the
///   previous venv is moved whole to `python/rollback-backup-*/<tool>`
///   with RESTORE.txt (no `.lens.old-*` left for a sweep to delete), a
///   later sweep keeps it, and the audit line carries the code.
#[cfg(windows)]
#[test]
fn a_pip_undo_blocked_by_a_held_file_is_retried_or_kept_safe() {
    use std::os::windows::fs::OpenOptionsExt;
    struct HoldingPython {
        hold_ms: u64,
        holder: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    }
    impl pip::PythonOps for HoldingPython {
        fn create_venv(&self, venv: &Path) -> Result<(), String> {
            std::fs::create_dir_all(pip::venv_python(venv).parent().unwrap()).unwrap();
            std::fs::write(pip::venv_python(venv), b"new-py").unwrap();
            Ok(())
        }
        fn pip_install(&self, venv: &Path, _requirements: &Path) -> Result<(), String> {
            let held = venv.join("Scripts").join("held.pyd");
            std::fs::write(&held, b"half").unwrap();
            let file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&held)
                .unwrap();
            let millis = self.hold_ms;
            *self.holder.lock().unwrap() = Some(std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(millis));
                drop(file);
            }));
            Err("pip install failed (exit 1): network".into())
        }
    }
    for (case, hold_ms) in [("brief", 1000u64), ("held", 6000u64)] {
        let base = scratch(&format!("pip-undo-{case}"));
        let dirs = InstallDirs {
            tools_dir: base.join("tools"),
        };
        let tool = pip_tool();
        let req = good_requirements();
        let ok = FakePython {
            fail_pip: false,
            write_script: true,
            calls: Default::default(),
        };
        pip::install_pip(&tool, &req, &dirs, &ok, &Progress::default()).unwrap();
        let target = dirs.python_env("lens");
        let marker = target.join("Lib").join("marker.txt");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"previous").unwrap();
        let python = HoldingPython {
            hold_ms,
            holder: Default::default(),
        };
        let err = pip::install_pip(&tool, &req, &dirs, &python, &Progress::default()).unwrap_err();
        python
            .holder
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .join()
            .unwrap();
        assert!(err.contains("pip install failed"), "{case}: {err}");
        let names = |dir: &Path| -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        if case == "brief" {
            assert!(
                !err.starts_with(ROLLBACK_INCOMPLETE_PREFIX),
                "{case}: {err}"
            );
            assert_eq!(std::fs::read(&marker).unwrap(), b"previous", "restored");
            assert!(venv_intact(&target, "lens"));
            assert_eq!(names(&dirs.python_root()), vec!["lens".to_string()]);
        } else {
            assert!(err.starts_with(ROLLBACK_INCOMPLETE_PREFIX), "{case}: {err}");
            assert!(
                err.contains("NEW, partial venv") && err.contains(&target.display().to_string()),
                "{err}"
            );
            let python_dir = names(&dirs.python_root());
            assert!(
                !python_dir.iter().any(|n| n.contains(".old-")),
                "no backup under a swept name: {python_dir:?}"
            );
            let kept_dir = python_dir
                .iter()
                .find(|n| n.starts_with("rollback-backup-"))
                .map(|n| dirs.python_root().join(n))
                .expect("kept backup dir");
            let kept = kept_dir.join("lens");
            assert_eq!(
                std::fs::read(kept.join("Lib").join("marker.txt")).unwrap(),
                b"previous"
            );
            assert!(venv_intact(&kept, "lens"), "the whole previous venv");
            assert!(err.contains(&kept.display().to_string()), "{err}");
            let restore = std::fs::read_to_string(kept_dir.join(RESTORE_FILE)).unwrap();
            assert!(restore.starts_with("tool: lens\n"), "{restore}");
            assert!(
                restore
                    .lines()
                    .any(|line| line == format!("{} <- lens", target.display())),
                "{restore}"
            );
            // A later sweep (server start / next install) keeps it.
            let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
            clean_stale_staging(&dirs.tools_dir, later, &[]);
            assert!(kept.join("Lib").join("marker.txt").is_file());
            let listed = rollback_backups(&dirs.tools_dir);
            assert_eq!(listed.len(), 1, "{listed:?}");
            assert_eq!(listed[0].tool.as_deref(), Some("lens"));
            let line: serde_json::Value =
                serde_json::from_str(&audit_line(&tool, Err(&err))).unwrap();
            assert_eq!(line["code"], "rollback_incomplete");
        }
        let _ = std::fs::remove_dir_all(base);
    }
}

/// Fix round 5 (L5): a renamed `<dir>.rm` whose removal keeps failing is
/// retried as it is — never renamed again to `.rm.rm`, `.rm.rm.rm`, … on
/// every sweep — and removed once the removal works.
#[test]
fn a_leftover_rm_dir_is_retried_not_renamed_again() {
    let base = scratch("rm-loop");
    let tools = base.join("tools");
    let staging = tools.join(".staging-stuck");
    std::fs::create_dir_all(staging.join("x")).unwrap();
    std::fs::write(staging.join("x").join("f"), b"x").unwrap();
    let rm = tools.join(".staging-stuck.rm");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    let names = || -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&tools)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    FAIL_DOOMED_REMOVAL.with(|fail| fail.set(true));
    assert_eq!(remove_dir_unless_in_use(&staging), Some(rm.clone()));
    assert_eq!(remove_dir_unless_in_use(&rm), Some(rm.clone()));
    for _ in 0..3 {
        assert_eq!(clean_stale_staging(&tools, later, &[]), 0);
    }
    assert_eq!(names(), vec![".staging-stuck.rm".to_string()]);
    assert!(rm.join("x").join("f").is_file());
    // The same for a replaced venv's leftover.
    let old = tools.join("python").join(".lens.old-00ff.rm");
    std::fs::create_dir_all(&old).unwrap();
    assert_eq!(clean_stale_staging(&tools, later, &[]), 0);
    assert!(old.is_dir() && !tools.join("python").join(".lens.old-00ff.rm.rm").exists());
    FAIL_DOOMED_REMOVAL.with(|fail| fail.set(false));
    assert_eq!(clean_stale_staging(&tools, later, &[]), 2);
    assert_eq!(names(), vec!["python".to_string()]);
    assert!(!old.exists());
    let _ = std::fs::remove_dir_all(base);
}

// ------------------------------------------- Windows: curl + tar, fake source

#[cfg(windows)]
pub(crate) mod fake {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    #[derive(Clone)]
    pub(crate) enum Reply {
        Body(Vec<u8>),
        Redirect(String),
        /// Declares the full length, sends `sent` bytes, then closes.
        Truncated {
            declared: usize,
            sent: Vec<u8>,
        },
        Status(u16),
        /// Sends the first half, waits for the gate, then the rest.
        Gated(Vec<u8>, Arc<(Mutex<bool>, Condvar)>),
    }

    pub(crate) struct FakeSource {
        pub(crate) port: u16,
        pub(crate) hits: Arc<AtomicUsize>,
        routes: Arc<Mutex<HashMap<String, Reply>>>,
    }

    impl FakeSource {
        pub(crate) fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }
        pub(crate) fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
        pub(crate) fn route(&self, path: &str, reply: Reply) {
            self.routes.lock().unwrap().insert(path.to_string(), reply);
        }
    }

    pub(crate) fn open_gate(gate: &Arc<(Mutex<bool>, Condvar)>) {
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    }

    /// A tiny HTTP/1.1 server on 127.0.0.1 (thread leaks with the test).
    pub(crate) fn serve(routes: HashMap<String, Reply>) -> FakeSource {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let routes = Arc::new(Mutex::new(routes));
        let shared = Arc::clone(&routes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let routes = Arc::clone(&shared);
                std::thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match stream.read(&mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    let text = String::from_utf8_lossy(&head);
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let reply = routes
                        .lock()
                        .unwrap()
                        .get(&path)
                        .cloned()
                        .unwrap_or(Reply::Status(404));
                    let _ = match reply {
                        Reply::Body(body) => write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .and_then(|()| stream.write_all(&body)),
                        Reply::Redirect(location) => write!(
                            stream,
                            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        ),
                        Reply::Truncated { declared, sent } => write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
                        )
                        .and_then(|()| stream.write_all(&sent)),
                        Reply::Status(code) => write!(
                            stream,
                            "HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        ),
                        Reply::Gated(body, gate) => {
                            let half = body.len() / 2;
                            let first = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .and_then(|()| stream.write_all(&body[..half]))
                            .and_then(|()| stream.flush());
                            let mut open = gate.0.lock().unwrap();
                            while !*open {
                                open = gate.1.wait(open).unwrap();
                            }
                            drop(open);
                            first.and_then(|()| stream.write_all(&body[half..]))
                        }
                    };
                    let _ = stream.flush();
                });
            }
        });
        FakeSource { port, hits, routes }
    }

    pub(crate) enum Entry<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Symlink(&'a str, &'a str),
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// Minimal stored (uncompressed) zip with unix modes, so a symlink
    /// entry is a real symlink to libarchive.
    pub(crate) fn zip(entries: &[Entry]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for entry in entries {
            let (name, data, mode): (&str, &[u8], u32) = match entry {
                Entry::File(name, data) => (name, data, 0o100_644),
                Entry::Dir(name) => (name, b"", 0o040_755),
                Entry::Symlink(name, target) => (name, target.as_bytes(), 0o120_777),
            };
            let offset = out.len() as u32;
            let crc = crc32(data);
            let len = data.len() as u32;
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0x21u16.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0x21u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&len.to_le_bytes());
            central.extend_from_slice(&len.to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&(mode << 16).to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        let cd_len = central.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_len.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// [`good_zip`] plus a 4 MiB unlisted file, so half the body is far
    /// beyond curl's write buffer (progress shows on disk mid-download).
    pub(crate) fn big_zip() -> Vec<u8> {
        let pad = vec![0x5au8; 4 << 20];
        zip(&[
            Entry::File("pkg/bin/fake.exe", b"MZ fake binary"),
            Entry::File("pkg/pad.bin", &pad),
            Entry::File("pkg/LICENSE.txt", b"license text"),
        ])
    }

    /// The well-formed archive the fake entry points at.
    pub(crate) fn good_zip() -> Vec<u8> {
        zip(&[
            Entry::Dir("pkg/"),
            Entry::Dir("pkg/bin/"),
            Entry::File("pkg/bin/fake.exe", b"MZ fake binary"),
            Entry::File("pkg/bin/other.exe", b"not listed in the manifest"),
            Entry::File("pkg/doc/readme.html", b"<p>docs</p>"),
            Entry::File("pkg/LICENSE.txt", b"license text"),
        ])
    }
}

#[cfg(windows)]
mod windows_installs {
    use super::fake::{self, Entry, Reply};
    use super::*;
    use std::collections::HashMap;

    pub(crate) fn archive_entry(url: &str, bytes: &[u8]) -> ToolEntry {
        let mut tool = base_entry();
        tool.url = Some(url.into());
        tool.file_name = Some("fake.zip".into());
        tool.size = Some(bytes.len() as u64);
        tool.sha256 = Some(crate::sha256::digest_bytes(bytes));
        tool
    }

    struct Fixture {
        base: PathBuf,
        dirs: InstallDirs,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let base = scratch(tag);
            let dirs = InstallDirs {
                tools_dir: base.join("tools"),
            };
            Self { base, dirs }
        }
        fn install(&self, tool: &ToolEntry) -> Result<InstallOutcome, String> {
            install_entry(
                tool,
                &self.dirs,
                &NetPolicy::test_loopback(&["127.0.0.1"]),
                &Progress::default(),
            )
        }
        /// Nothing installed, no staging dir left, nothing written beside
        /// the tools dir.
        fn assert_clean(&self) {
            assert!(
                !self.dirs.bin().join("fake.exe").exists(),
                "binary installed"
            );
            let staged: Vec<String> = std::fs::read_dir(&self.dirs.tools_dir)
                .map(|it| {
                    it.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| n.starts_with(".staging-"))
                        .collect()
                })
                .unwrap_or_default();
            assert!(staged.is_empty(), "staging left behind: {staged:?}");
            let beside: Vec<String> = std::fs::read_dir(&self.base)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != "tools")
                .collect();
            assert!(beside.is_empty(), "escaped the tools dir: {beside:?}");
        }
        fn audit_results(&self) -> Vec<String> {
            std::fs::read_to_string(self.dirs.audit_log())
                .unwrap_or_default()
                .lines()
                .map(|l| {
                    serde_json::from_str::<serde_json::Value>(l).unwrap()["result"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn installs_only_listed_files_after_redirect_and_writes_one_audit_line() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::new());
        source.route("/real/fake.zip", Reply::Body(bytes.clone()));
        // The redirect target is on the same allowed host (127.0.0.1).
        source.route("/start", Reply::Redirect(source.url("/real/fake.zip")));
        let tool = archive_entry(&source.url("/start"), &bytes);
        let fx = Fixture::new("ok");
        let outcome = fx.install(&tool).unwrap();
        let bin = fx.dirs.bin();
        assert_eq!(
            std::fs::read(bin.join("fake.exe")).unwrap(),
            b"MZ fake binary"
        );
        assert_eq!(
            std::fs::read(fx.dirs.licenses().join("fake-LICENSE.txt")).unwrap(),
            b"license text"
        );
        assert!(!bin.join("other.exe").exists(), "unlisted file installed");
        assert!(!bin.join("readme.html").exists());
        assert_eq!(outcome.installed_files.len(), 2);
        assert!(outcome.audit_warning.is_none());
        assert_eq!(fx.audit_results(), ["ok"]);
        fx.assert_clean_staging_only();
        assert_eq!(source.hits(), 2, "one redirect hop + the download");
        // Reinstalling replaces the files in place.
        fx.install(&tool).unwrap();
        assert_eq!(fx.audit_results(), ["ok", "ok"]);
    }

    impl Fixture {
        fn assert_clean_staging_only(&self) {
            let staged = std::fs::read_dir(&self.dirs.tools_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with(".staging-"));
            assert!(!staged, "staging left behind");
        }
    }

    /// Fix round 11 L2: a FrameTrace folder whose path has a character
    /// outside the ANSI code page (an emoji) either installs through the
    /// 8.3 short path or is refused up front with the clear message —
    /// never the opaque "archive listing failed", and never contacting the
    /// source when refused.
    #[test]
    fn an_emoji_folder_installs_via_the_short_path_or_is_refused_clearly() {
        let emoji = "ft-\u{1F600}-dir";
        assert!(archive::ansi_round_trips(Path::new("C:/plain/ascii")));
        assert_eq!(
            archive::ansi_round_trips(Path::new(emoji)),
            archive::ansi_is_utf8(),
            "an emoji survives only a UTF-8 code page"
        );
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::new());
        source.route("/fake.zip", Reply::Body(bytes.clone()));
        let tool = archive_entry(&source.url("/fake.zip"), &bytes);
        let base = scratch("emoji").join(emoji);
        let dirs = InstallDirs {
            tools_dir: base.join("tools"),
        };
        let result = install_entry(
            &tool,
            &dirs,
            &NetPolicy::test_loopback(&["127.0.0.1"]),
            &Progress::default(),
        );
        match &result {
            Ok(_) => {
                assert_eq!(
                    std::fs::read(dirs.bin().join("fake.exe")).unwrap(),
                    b"MZ fake binary"
                );
            }
            Err(err) => {
                assert!(err.contains(archive::ANSI_PATH_REFUSED), "{err}");
                assert!(!dirs.bin().join("fake.exe").exists());
                assert_eq!(source.hits(), 0, "refused before any download");
            }
        }
        if let Err(err) = &result {
            assert!(!err.contains("archive listing failed"), "{err}");
        }
        let leftover = std::fs::read_dir(&dirs.tools_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with(".staging-"));
        assert!(!leftover, "staging left behind");
        eprintln!(
            "emoji folder install: {}",
            match &result {
                Ok(_) => "installed (short path)".to_string(),
                Err(err) => err.clone(),
            }
        );
        let _ = std::fs::remove_dir_all(base.parent().unwrap());
    }

    #[test]
    fn hash_mismatch_is_refused_and_nothing_is_left() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        let mut tool = archive_entry(&source.url("/fake.zip"), &bytes);
        tool.sha256 = Some("0".repeat(64));
        let fx = Fixture::new("sha");
        let err = fx.install(&tool).unwrap_err();
        assert!(err.contains("SHA-256 mismatch"), "{err}");
        fx.assert_clean();
        assert_eq!(fx.audit_results(), ["failed"]);
    }

    #[test]
    fn size_mismatch_is_refused_both_ways() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        // Pinned larger than served: the post-download size check.
        let mut tool = archive_entry(&source.url("/fake.zip"), &bytes);
        tool.size = Some(bytes.len() as u64 + 1);
        let fx = Fixture::new("size-big");
        let err = fx.install(&tool).unwrap_err();
        assert!(err.contains("size mismatch"), "{err}");
        fx.assert_clean();
        // Pinned smaller than served: curl's --max-filesize stops it.
        let mut tool = archive_entry(&source.url("/fake.zip"), &bytes);
        tool.size = Some(bytes.len() as u64 - 1);
        let fx = Fixture::new("size-small");
        let err = fx.install(&tool).unwrap_err();
        assert!(
            err.contains("larger than the pinned size") || err.contains("size mismatch"),
            "{err}"
        );
        fx.assert_clean();
    }

    fn refuse_archive(tag: &str, entries: &[Entry], expect: &[&str]) {
        let bytes = fake::zip(entries);
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        let tool = archive_entry(&source.url("/fake.zip"), &bytes);
        let fx = Fixture::new(tag);
        let err = fx.install(&tool).unwrap_err();
        assert!(
            expect.iter().any(|needle| err.contains(needle)),
            "{tag}: {err}"
        );
        fx.assert_clean();
        assert!(!fx.base.parent().unwrap().join("evil.exe").exists());
    }

    #[test]
    fn zip_slip_absolute_drive_and_link_entries_refuse_the_whole_archive() {
        refuse_archive(
            "slip",
            &[
                Entry::File("pkg/bin/fake.exe", b"MZ"),
                Entry::File("pkg/LICENSE.txt", b"l"),
                Entry::File("../evil.exe", b"x"),
            ],
            &["unsafe entry"],
        );
        refuse_archive(
            "slip-deep",
            &[
                Entry::File("pkg/bin/fake.exe", b"MZ"),
                Entry::File("pkg/LICENSE.txt", b"l"),
                Entry::File("pkg/../../evil.exe", b"x"),
            ],
            &["unsafe entry"],
        );
        refuse_archive(
            "absolute",
            &[
                Entry::File("pkg/bin/fake.exe", b"MZ"),
                Entry::File("pkg/LICENSE.txt", b"l"),
                Entry::File("/evil.exe", b"x"),
            ],
            &["unsafe entry", "refused"],
        );
        refuse_archive(
            "drive",
            &[
                Entry::File("pkg/bin/fake.exe", b"MZ"),
                Entry::File("pkg/LICENSE.txt", b"l"),
                Entry::File("C:/evil.exe", b"x"),
            ],
            &["unsafe entry", "refused"],
        );
        refuse_archive(
            "backslash",
            &[
                Entry::File("pkg/bin/fake.exe", b"MZ"),
                Entry::File("pkg/LICENSE.txt", b"l"),
                Entry::File("pkg\\..\\..\\evil.exe", b"x"),
            ],
            &["unsafe entry", "refused"],
        );
        refuse_archive(
            "symlink",
            &[
                Entry::File("pkg/bin/fake.exe", b"MZ"),
                Entry::File("pkg/LICENSE.txt", b"l"),
                Entry::Symlink("pkg/escape", "../../.."),
            ],
            &["link or special entry", "refused"],
        );
        // The manifest file itself is a link.
        refuse_archive(
            "member-link",
            &[
                Entry::Symlink("pkg/bin/fake.exe", "C:/Windows/System32/cmd.exe"),
                Entry::File("pkg/LICENSE.txt", b"l"),
            ],
            &["link or special entry", "refused"],
        );
        refuse_archive(
            "missing-member",
            &[Entry::File("pkg/LICENSE.txt", b"l")],
            &["not in the archive"],
        );
    }

    #[test]
    fn host_outside_allowlist_is_never_contacted() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        let tool = archive_entry(
            &format!("http://localhost:{}/fake.zip", source.port),
            &bytes,
        );
        let fx = Fixture::new("host");
        let err = fx.install(&tool).unwrap_err();
        assert!(err.contains("not on the tool source allowlist"), "{err}");
        assert_eq!(source.hits(), 0, "the refused host was contacted");
        fx.assert_clean();
    }

    #[test]
    fn redirect_to_another_host_is_refused_before_following() {
        let bytes = fake::good_zip();
        let target = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Redirect(format!("http://localhost:{}/fake.zip", target.port)),
        )]));
        let tool = archive_entry(&source.url("/fake.zip"), &bytes);
        let fx = Fixture::new("redirect");
        let err = fx.install(&tool).unwrap_err();
        assert!(err.contains("not on the tool source allowlist"), "{err}");
        assert_eq!(source.hits(), 1);
        assert_eq!(target.hits(), 0, "the redirect target was contacted");
        fx.assert_clean();
        // A redirect loop on an allowed host is bounded.
        let looping = fake::serve(HashMap::new());
        looping.route("/loop", Reply::Redirect(looping.url("/loop")));
        let fx = Fixture::new("loop");
        let err = fx
            .install(&archive_entry(&looping.url("/loop"), &bytes))
            .unwrap_err();
        assert!(err.contains("redirects"), "{err}");
        assert_eq!(looping.hits(), archive::MAX_HOPS + 1);
        fx.assert_clean();
    }

    #[test]
    fn interrupted_download_and_http_errors_clean_up() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::from([
            (
                "/cut.zip".to_string(),
                Reply::Truncated {
                    declared: bytes.len(),
                    sent: bytes[..bytes.len() / 3].to_vec(),
                },
            ),
            ("/gone.zip".to_string(), Reply::Status(404)),
        ]));
        let fx = Fixture::new("cut");
        let err = fx
            .install(&archive_entry(&source.url("/cut.zip"), &bytes))
            .unwrap_err();
        assert!(err.contains("download failed"), "{err}");
        fx.assert_clean();
        let fx = Fixture::new("404");
        let err = fx
            .install(&archive_entry(&source.url("/gone.zip"), &bytes))
            .unwrap_err();
        assert!(err.contains("download failed"), "{err}");
        fx.assert_clean();
    }

    #[test]
    fn a_held_install_lock_refuses_a_second_install() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        let fx = Fixture::new("lock");
        let _held = InstallLock::acquire(&fx.dirs.tools_dir).unwrap();
        let err = fx
            .install(&archive_entry(&source.url("/fake.zip"), &bytes))
            .unwrap_err();
        assert_eq!(err, INSTALL_LOCKED);
        assert_eq!(source.hits(), 0);
    }

    #[test]
    fn installs_the_pinned_gplv3_text_next_to_the_archive_license() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        let mut tool = archive_entry(&source.url("/fake.zip"), &bytes);
        tool.bundled_licenses = vec![BundledLicense {
            file: "tools/licenses/COPYING.GPLv3".into(),
            to: "fake-COPYING.GPLv3.txt".into(),
            sha256: "8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903".into(),
        }];
        let fx = Fixture::new("gplv3");
        let outcome = fx.install(&tool).unwrap();
        let installed = std::fs::read(fx.dirs.licenses().join("fake-COPYING.GPLv3.txt")).unwrap();
        assert_eq!(
            crate::sha256::digest_bytes(&installed),
            "8ceb4b9ee5adedde47b31e975c1d90c73ad27b6b165a1dcd80c7c545eb65b903"
        );
        assert!(fx.dirs.licenses().join("fake-LICENSE.txt").is_file());
        assert_eq!(outcome.installed_files.len(), 3);
        fx.assert_clean_staging_only();
    }

    #[test]
    fn a_cancel_before_the_first_hop_contacts_nothing_and_cleans_up() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        let tool = archive_entry(&source.url("/fake.zip"), &bytes);
        let fx = Fixture::new("cancel-early");
        let progress = Progress::default();
        progress.request_cancel().unwrap();
        let err = install_entry(
            &tool,
            &fx.dirs,
            &NetPolicy::test_loopback(&["127.0.0.1"]),
            &progress,
        )
        .unwrap_err();
        assert_eq!(err, INSTALL_CANCELLED);
        assert_eq!(source.hits(), 0);
        assert_eq!(progress.stage(), Stage::Failed);
        fx.assert_clean();
        assert_eq!(fx.audit_results(), ["failed"]);
    }

    #[test]
    fn curl_never_globs_follows_redirects_or_overrides_the_proxy() {
        let args: Vec<String> = archive::curl_args(
            "https://github.com/o/r/a[1]{x}.zip",
            true,
            Path::new("C:/t/out.part"),
            42,
        )
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
        assert_eq!(args[0], "--disable", "curlrc must be ignored first");
        assert!(args.contains(&"--globoff".to_string()));
        assert!(args.windows(2).any(|w| w == ["--proto", "=https"]));
        assert!(args.windows(2).any(|w| w == ["--max-redirs", "0"]));
        assert!(args.windows(2).any(|w| w == ["--max-filesize", "42"]));
        for banned in [
            "--location",
            "-L",
            "--proxy",
            "--noproxy",
            "--insecure",
            "-k",
        ] {
            assert!(!args.contains(&banned.to_string()), "{banned} in {args:?}");
        }
        assert_eq!(args.last().unwrap(), "https://github.com/o/r/a[1]{x}.zip");
    }

    #[test]
    fn bracketed_urls_are_fetched_literally_not_as_a_curl_glob() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::new());
        source.route("/pkg[1].zip", Reply::Body(bytes.clone()));
        // Without --globoff curl would expand `[1]` and ask for /pkg1.zip.
        source.route("/pkg1.zip", Reply::Status(500));
        let tool = archive_entry(&source.url("/pkg[1].zip"), &bytes);
        let fx = Fixture::new("globoff");
        fx.install(&tool).unwrap();
        assert!(fx.dirs.bin().join("fake.exe").is_file());
        assert_eq!(source.hits(), 1);
    }

    #[test]
    fn stale_staging_of_a_killed_install_is_swept_before_installing() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        let fx = Fixture::new("sweep");
        let dead = fx.dirs.tools_dir.join(".staging-deadbeef00000000");
        std::fs::create_dir_all(dead.join("x/pkg")).unwrap();
        std::fs::write(dead.join("download.part"), b"half").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        fx.install(&archive_entry(&source.url("/fake.zip"), &bytes))
            .unwrap();
        assert!(!dead.exists(), "stale staging left behind");
        fx.assert_clean_staging_only();
    }

    /// Kills the held process on every exit path of a test.
    struct Running(std::process::Child);

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_running_old_binary_is_reported_as_staging_left_not_silently_ok() {
        let bytes = fake::good_zip();
        let source = fake::serve(HashMap::from([(
            "/fake.zip".to_string(),
            Reply::Body(bytes.clone()),
        )]));
        let fx = Fixture::new("held");
        // The previous tools/bin/fake.exe is running (a copy of cmd.exe):
        // Windows lets the install rename it into the
        // staging backup, but not delete it.
        std::fs::create_dir_all(fx.dirs.bin()).unwrap();
        let old = fx.dirs.bin().join("fake.exe");
        std::fs::copy(archive::system32_tool("cmd.exe").unwrap(), &old).unwrap();
        let mut command = std::process::Command::new(&old);
        // An interactive cmd reading its (open, idle) stdin pipe: runs
        // until killed, starts nothing else.
        command
            .args(["/d", "/q", "/k"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let running = Running(command.spawn().unwrap());
        std::thread::sleep(std::time::Duration::from_millis(300));

        let outcome = fx
            .install(&archive_entry(&source.url("/fake.zip"), &bytes))
            .unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"MZ fake binary");
        let left = outcome
            .staging_left
            .clone()
            .expect("the held backup must be reported");
        assert!(left.starts_with(&fx.dirs.tools_dir), "{}", left.display());
        assert!(
            left.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".staging-")
        );
        assert!(left.join("backup").join("0.bak").is_file());
        // The audit line says "ok" and names the leftover.
        let text = std::fs::read_to_string(fx.dirs.audit_log()).unwrap();
        let line: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(line["result"], "ok");
        assert_eq!(line["staging_left"], left.display().to_string());

        // Once the old binary exits, the next sweep removes the leftover.
        drop(running);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            clean_stale_staging(&fx.dirs.tools_dir, std::time::SystemTime::now(), &[]),
            1
        );
        assert!(!left.exists());
    }

    /// Fix round 4: the sweep never half-deletes a directory still in use
    /// — an old venv a process runs from (the server's deepfake-lens,
    /// whose busy lease the CLI cannot see) or a staging dir with a file
    /// held open. A running program does NOT stop a directory rename (seen
    /// here: the first version of this fix deleted `lens.py` under the
    /// running `python.exe`), so mapped images are checked first; an open
    /// handle makes the rename to `<dir>.rm` fail. Either way the dir is
    /// skipped whole; once released, it is removed (no `.rm` is left).
    #[test]
    fn the_sweep_skips_a_dir_in_use_whole_instead_of_half_deleting_it() {
        let base = scratch("sweep-held");
        let tools = base.join("tools");
        let venv = tools.join("python").join(".deepfake-lens.old-0123abcd");
        // Fix round 13 M: the live venv works, so the old one may go.
        write_venv_launcher(&tools.join("python").join("deepfake-lens"), "deepfake-lens");
        let scripts = venv.join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::create_dir_all(venv.join("Lib").join("site-packages")).unwrap();
        let module = venv.join("Lib").join("site-packages").join("lens.py");
        std::fs::write(&module, b"print('lens')").unwrap();
        let exe = scripts.join("python.exe");
        std::fs::copy(archive::system32_tool("cmd.exe").unwrap(), &exe).unwrap();
        let staging = tools.join(".staging-held");
        std::fs::create_dir_all(staging.join("x")).unwrap();
        let held_file = staging.join("x").join("download.part");
        std::fs::write(&held_file, b"partial").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let run_started = std::time::SystemTime::now();

        let mut command = std::process::Command::new(&exe);
        command
            .args(["/d", "/q", "/k"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let running = Running(command.spawn().unwrap());
        // std opens with FILE_SHARE_READ | WRITE | DELETE — still a handle.
        let held = std::fs::File::open(&held_file).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));

        assert_eq!(clean_stale_staging(&tools, run_started, &[]), 0);
        assert!(module.is_file(), "the venv in use is left whole");
        assert!(exe.is_file());
        assert!(held_file.is_file(), "the staging dir in use is left whole");
        assert!(
            !tools
                .join("python")
                .join(".deepfake-lens.old-0123abcd.rm")
                .exists()
        );
        assert!(!tools.join(".staging-held.rm").exists());
        // The pip path reports the whole venv as left behind.
        assert_eq!(remove_dir_unless_in_use(&venv), Some(venv.clone()));
        assert!(module.is_file());

        drop(held);
        drop(running);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(clean_stale_staging(&tools, run_started, &[]), 2);
        assert!(!venv.exists() && !staging.exists());
        let left: Vec<_> = std::fs::read_dir(&tools)
            .unwrap()
            .chain(std::fs::read_dir(tools.join("python")).unwrap())
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".rm"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        let _ = std::fs::remove_dir_all(base);
    }

    /// Fix round 5 (L2): a program running from the live venv does not
    /// stop the rename on Windows, so a reinstall would rebuild the env
    /// underneath it. It is refused (`busy`) before anything is touched;
    /// once the program exits the reinstall works.
    #[test]
    fn a_pip_reinstall_refuses_while_a_program_runs_from_the_live_venv() {
        let base = scratch("pip-busy");
        let dirs = InstallDirs {
            tools_dir: base.join("tools"),
        };
        let tool = pip_tool();
        let req = good_requirements();
        let python = || FakePython {
            fail_pip: false,
            write_script: true,
            calls: Default::default(),
        };
        pip::install_pip(&tool, &req, &dirs, &python(), &Progress::default()).unwrap();
        let venv = dirs.python_env("lens");
        let module = venv.join("Lib").join("site-packages").join("lens.py");
        std::fs::create_dir_all(module.parent().unwrap()).unwrap();
        std::fs::write(&module, b"print('lens')").unwrap();
        let exe = venv.join("Scripts").join("held.exe");
        std::fs::copy(archive::system32_tool("cmd.exe").unwrap(), &exe).unwrap();
        let mut command = std::process::Command::new(&exe);
        command
            .args(["/d", "/q", "/k"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let running = Running(command.spawn().unwrap());
        std::thread::sleep(std::time::Duration::from_millis(300));

        let again = python();
        let err = pip::install_pip(&tool, &req, &dirs, &again, &Progress::default()).unwrap_err();
        assert!(
            err.starts_with(BUSY_PREFIX) && err.contains("in use"),
            "{err}"
        );
        assert!(again.calls.lock().unwrap().is_empty(), "python never ran");
        assert!(
            module.is_file() && exe.is_file(),
            "the live venv is untouched"
        );
        let python_dir: Vec<String> = std::fs::read_dir(venv.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(python_dir, vec!["lens".to_string()], "no .lens.old-*");
        assert!(
            !std::fs::read_dir(&dirs.tools_dir)
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().starts_with(".staging-")),
            "no staging dir"
        );
        // The failed audit line carries the busy code.
        let line: serde_json::Value = serde_json::from_str(&audit_line(&tool, Err(&err))).unwrap();
        assert_eq!(line["result"], "failed");
        assert_eq!(line["code"], "busy");

        drop(running);
        std::thread::sleep(std::time::Duration::from_millis(300));
        pip::install_pip(&tool, &req, &dirs, &python(), &Progress::default()).unwrap();
        assert!(!module.exists(), "rebuilt once nothing runs from it");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn system_tools_resolve_to_absolute_system32_paths() {
        for name in ["curl.exe", "tar.exe"] {
            let path = archive::system32_tool(name).unwrap();
            assert!(path.is_absolute(), "{}", path.display());
            assert!(
                path.to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("\\system32\\"),
                "{}",
                path.display()
            );
        }
        assert!(archive::system32_tool("definitely-not-a-tool.exe").is_err());
    }
}

#[cfg(windows)]
pub(crate) use windows_installs::archive_entry as test_archive_entry;

/// Fix round 3: `status()` walks PATH once for all of a tool's binaries
/// (it runs for every tool on each GET /api/tools), and never reports a
/// tools/bin-vs-PATH shadow for deepfake-lens (its private venv comes
/// first).
#[test]
fn status_walks_path_once_and_never_shadows_deepfake_lens() {
    let manifest = manifest().unwrap();
    let libewf = find_tool(manifest, "libewf").unwrap();
    assert_eq!(libewf.provides.len(), 3);
    let before = crate::tool_policy::path_walks();
    let status = status(libewf, &[]);
    assert_eq!(crate::tool_policy::path_walks() - before, 1);
    assert_eq!(status.paths.len(), 3);
    assert_eq!(status.shadowed_by.len(), 3);
    let lens = find_tool(manifest, "deepfake-lens").unwrap();
    let status = super::status(lens, &[]);
    assert_eq!(
        status.shadowed_by,
        vec![("deepfake-lens".to_string(), None::<String>)]
    );
}
