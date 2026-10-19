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
        "LGPL build only (T5)"
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
    assert!(!dirs.tools_dir.exists(), "nothing may be created");
    let _ = std::fs::remove_dir_all(base);
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
    assert!(!dirs.tools_dir.exists());
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

#[test]
fn audit_line_records_source_hash_and_result_without_signed_query() {
    let mut tool = base_entry();
    tool.url = Some("https://github.com/o/r/fake.zip".into());
    tool.sha256 = Some("b".repeat(64));
    let ok = audit_line(&tool, Ok(&[PathBuf::from("x/fake.exe")]));
    let value: serde_json::Value = serde_json::from_str(&ok).unwrap();
    assert_eq!(value["event"], "tool-install");
    assert_eq!(value["result"], "ok");
    assert_eq!(value["host"], "github.com");
    assert_eq!(value["sha256"], "b".repeat(64));
    let failed = audit_line(&tool, Err(&"boom".to_string()));
    let value: serde_json::Value = serde_json::from_str(&failed).unwrap();
    assert_eq!(value["result"], "failed");
    assert_eq!(value["error"], "boom");
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
