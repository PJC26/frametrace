//! Round-19 track S (docs/PLAN_R19_2026-10-19.md): a pasted command is
//! checked by the argv the program really receives (P32) — S1 — and the
//! S4/S5 keys.

use super::*;

#[cfg(windows)]
fn scratch19(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r19s-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A tiny exe that writes the arguments it received (one per line, UTF-8)
/// to `%FT_ARGV_OUT%` — built once with `rustc`. `None` when no `rustc`
/// can be run.
#[cfg(windows)]
fn argv_echo_exe() -> Option<PathBuf> {
    static EXE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let dir = scratch19("echo");
        let source = dir.join("argv_echo.rs");
        let exe = dir.join("argv_echo.exe");
        std::fs::write(
            &source,
            "fn main() {\n    let out = std::env::var_os(\"FT_ARGV_OUT\").expect(\"FT_ARGV_OUT\");\n    let args: Vec<String> = std::env::args().skip(1).collect();\n    std::fs::write(out, args.join(\"\\n\")).expect(\"write\");\n}\n",
        )
        .unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let built = std::process::Command::new(rustc)
            .arg(&source)
            .arg("-o")
            .arg(&exe)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match built {
            Ok(status) if status.success() && exe.is_file() => Some(exe),
            _ => {
                eprintln!("SKIPPED: rustc could not build the argv-echo exe");
                None
            }
        }
    })
    .clone()
}

/// The argv `program` received (`None`: it did not run).
#[cfg(windows)]
fn received_argv(mut command: std::process::Command, out: &Path) -> Option<Vec<String>> {
    let _ = std::fs::remove_file(out);
    let status = command
        .env("FT_ARGV_OUT", out)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    let text = std::fs::read_to_string(out).ok()?;
    let _ = status;
    Some(text.split('\n').map(str::to_string).collect())
}

/// What `frametrace` would receive from `line` (a command starting with
/// `frametrace `) pasted into Windows PowerShell 5.1.
#[cfg(windows)]
fn powershell_argv(echo: &Path, line: &str, out: &Path) -> Option<Vec<String>> {
    let rest = line.strip_prefix("frametrace ").expect(line);
    let mut command = std::process::Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("& '{}' {rest}", echo.display()));
    received_argv(command, out)
}

/// The same pasted at a `cmd.exe` prompt (`cmd /d /s /c "<line>"`).
#[cfg(windows)]
fn cmd_argv(echo: &Path, line: &str, out: &Path) -> Option<Vec<String>> {
    use std::os::windows::process::CommandExt;
    let rest = line.strip_prefix("frametrace ").expect(line);
    let mut command = std::process::Command::new("cmd.exe");
    command.raw_arg(format!("/d /s /c \"\"{}\" {rest}\"", echo.display()));
    received_argv(command, out)
}

/// R19-S1 (P32, the R18 reviewer's finding): a case path ending in `\` —
/// a drive root, or a folder typed with a trailing backslash — turned the
/// closing quote into a literal character during Win32 argv conversion
/// (`"E:\"` and PowerShell's `"C:\cases\c 1\"`), so the flags ended up
/// INSIDE the path argument and the CLI said "finalize journal: none",
/// exit 0. Every offered command, pasted into Windows PowerShell 5.1 and
/// into `cmd /d /s /c`, must reach the program as exactly
/// `[finalize-journal, <normalized path>, <flags>...]`.
#[cfg(windows)]
#[test]
fn r19_s1_pasted_commands_reach_the_program_as_separate_arguments() {
    let Some(echo) = argv_echo_exe() else {
        return;
    };
    let out = echo.with_file_name("argv.txt");
    let cwd = std::env::current_dir().unwrap();
    let relative_expected = format!("{}\\cases\\c 1", cwd.display());
    let cases: Vec<(&str, String)> = vec![
        ("E:\\", "E:\\.".to_string()),
        ("C:\\cases\\c 1\\", "C:\\cases\\c 1".to_string()),
        ("C:\\cases\\c 1\\\\", "C:\\cases\\c 1".to_string()),
        ("C:/cases/c 1/", "C:\\cases\\c 1".to_string()),
        ("C:\\cases\\c1\\", "C:\\cases\\c1".to_string()),
        (
            "C:\\cases\\홍길동 사건 (1)\\",
            "C:\\cases\\홍길동 사건 (1)".to_string(),
        ),
        ("C:\\cases\\c 1", "C:\\cases\\c 1".to_string()),
        ("cases\\c 1\\", relative_expected),
    ];
    let mut checked = 0;
    let mut wrong = Vec::new();
    for (typed, expected) in &cases {
        let case = Path::new(typed);
        let offered: Vec<(Option<String>, &str, bool)> = vec![
            (
                super::finalize_journal::dismiss_command(case),
                "--dismiss",
                false,
            ),
            (
                super::finalize_journal::dismiss_command_cmd(case),
                "--dismiss",
                true,
            ),
            (
                super::finalize_journal::force_dismiss_command(case),
                "--dismiss --force",
                false,
            ),
            (
                super::finalize_journal::force_dismiss_command_cmd(case),
                "--dismiss --force",
                true,
            ),
            (
                super::finalize_journal::writer_gone_command(case, false),
                "--writer-gone",
                false,
            ),
            (
                super::finalize_journal::writer_gone_command_cmd(case, true),
                "--writer-gone --i-am-sure",
                true,
            ),
        ];
        for (command, flags, is_cmd) in offered {
            let command = command.unwrap_or_else(|| panic!("{typed}: no command offered"));
            let mut want = vec!["finalize-journal".to_string(), expected.clone()];
            want.extend(flags.split(' ').map(str::to_string));
            let got = if is_cmd {
                cmd_argv(&echo, &command, &out)
            } else {
                powershell_argv(&echo, &command, &out)
            };
            let Some(got) = got else {
                eprintln!("SKIPPED: the shell could not run `{command}`");
                continue;
            };
            if got != want {
                wrong.push(format!(
                    "{typed}: {} `{command}` -> {got:?} (want {want:?})",
                    if is_cmd { "cmd" } else { "PowerShell 5.1" }
                ));
            }
            checked += 1;
        }
    }
    // R20-S2: a device root keeps its trailing `\` (`\\.\C:\` — cmd merged
    // the flags into the path, measured): no command is offered at all.
    for typed in [r"\\.\C:\", r"\\.\C:", r"\\.\C:\cases\c1\"] {
        let case = Path::new(typed);
        for command in [
            super::finalize_journal::dismiss_command(case),
            super::finalize_journal::dismiss_command_cmd(case),
            super::finalize_journal::force_dismiss_command(case),
            super::finalize_journal::force_dismiss_command_cmd(case),
            super::finalize_journal::writer_gone_command(case, false),
            super::finalize_journal::writer_gone_command_cmd(case, true),
        ]
        .into_iter()
        .flatten()
        {
            wrong.push(format!("{typed}: a device path was offered `{command}`"));
        }
    }
    assert!(checked > 0, "no shell could be run");
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// R19-S2 (P32): the CLI's next-step hints, pasted into Windows
/// PowerShell 5.1 and `cmd /d /s /c`, reach the program as the right
/// arguments — paths typed with a trailing `\` included (the R18 hints
/// printed them bare: `next: carve-file C:\cases\c 1 …` split at the
/// space).
#[cfg(windows)]
#[test]
fn r19_s2_cli_hints_reach_the_program_as_separate_arguments() {
    let Some(echo) = argv_echo_exe() else {
        return;
    };
    let out = echo.with_file_name("argv-hint.txt");
    let case = Path::new("C:\\cases\\홍길동 사건 (1)\\");
    let raw = Path::new("E:\\images\\disk 1.raw");
    let folder = Path::new("E:\\");
    let hints = [
        (
            crate::cli::handlers::forensic::next_carve_file_hint(case, raw),
            vec![
                "carve-file",
                "C:\\cases\\홍길동 사건 (1)",
                "E:\\images\\disk 1.raw",
            ],
        ),
        (
            crate::cli::handlers::forensic::next_scan_folder_hint(case, folder),
            vec![
                "scan-folder",
                "C:\\cases\\홍길동 사건 (1)",
                "E:\\.",
                "--no-ffprobe",
            ],
        ),
    ];
    let mut checked = 0;
    for (hint, want) in hints {
        let line = format!("frametrace {}", hint.strip_prefix("next: ").expect(&hint));
        for is_cmd in [false, true] {
            let got = if is_cmd {
                cmd_argv(&echo, &line, &out)
            } else {
                powershell_argv(&echo, &line, &out)
            };
            let Some(got) = got else {
                continue;
            };
            assert_eq!(got, want, "{} `{line}`", if is_cmd { "cmd" } else { "PS" });
            checked += 1;
        }
    }
    assert!(checked > 0, "no shell could be run");
}

/// R19-S2: a hint whose path is not paste-safe names it by a placeholder
/// and gives the reason once — no path between quotes.
#[test]
fn r19_s2_cli_hints_never_quote_a_hostile_path() {
    let hostile = std::env::temp_dir().join("x\u{2019}; Write-Output injected; \u{2019}");
    let raw = std::env::temp_dir().join("disk.raw");
    let hint = crate::cli::handlers::forensic::next_carve_file_hint(&hostile, &raw);
    assert!(
        hint.starts_with("next: carve-file <케이스 폴더> "),
        "{hint}"
    );
    assert!(!hint.contains("injected"), "{hint}");
    assert!(
        hint.ends_with(&format!(" ({})", crate::paste_path::UNSAFE_REASON)),
        "{hint}"
    );
    let hint = crate::cli::handlers::forensic::next_scan_folder_hint(&raw, &hostile);
    assert!(hint.contains(" <폴더> --no-ffprobe ("), "{hint}");
    assert!(!hint.contains("injected"), "{hint}");
}

/// R19-S1 (P32): `/api/open-case` binds the NORMALIZED folder — a case
/// typed with a trailing separator is bound (and shown, and put into
/// commands) without it.
#[test]
fn r19_s1_open_case_binds_the_normalized_folder() {
    let root = std::env::temp_dir().join(format!("ft-r19s-open-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let case = root.join("c 1");
    std::fs::create_dir_all(&case).unwrap();
    std::fs::write(case.join("case.json"), b"{}").unwrap();
    let sep = std::path::MAIN_SEPARATOR;
    let typed = format!("{}{sep}{sep}", case.display());
    let body = serde_json::json!({ "case_dir": typed }).to_string();
    let request = Request {
        method: "POST".into(),
        path: "/api/open-case".into(),
        query: String::new(),
        body,
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    };
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let opened: serde_json::Value = serde_json::from_str(&api_open_case(&request, &state)).unwrap();
    assert_eq!(opened["ok"], true, "{opened}");
    let bound = state_lock(&state).case_dir.clone().unwrap();
    let text = bound.display().to_string();
    assert!(!text.ends_with(sep), "{text}");
    assert_eq!(
        text,
        crate::paste_path::normalize(&case).display().to_string()
    );
    let _ = std::fs::remove_dir_all(root);
}

/// R19-S4: a remembered recovery verdict is keyed by the (normalized)
/// case folder too — the same finalize id and manifest stamp in ANOTHER
/// folder (a copy of the case) is not shown that verdict; the same folder
/// typed with a trailing separator is.
#[test]
fn r19_s4_remembered_verdict_is_keyed_by_case_folder() {
    let base = std::env::temp_dir().join(format!("ft-r19s-s4-{}", std::process::id()));
    let case = base.join("a");
    let copy = base.join("b");
    let id = format!("{:032x}", u128::from(std::process::id()) * 7919);
    let stamp = (17 * 1024 * 1024, std::time::SystemTime::UNIX_EPOCH);
    super::finalize_journal::remember_package_changed_for_test(&case, &id, stamp);
    assert!(super::finalize_journal::verdict_remembered_for_test(
        &case, &id, stamp
    ));
    let sep = std::path::MAIN_SEPARATOR;
    assert!(super::finalize_journal::verdict_remembered_for_test(
        Path::new(&format!("{}{sep}", case.display())),
        &id,
        stamp
    ));
    assert!(!super::finalize_journal::verdict_remembered_for_test(
        &copy, &id, stamp
    ));
}

/// R19-S5: the two looks of `--writer-gone` must also agree on the
/// package name, the host id and the writer's executable hash — R18's
/// key compared only id, pid, start time and sign-off hash, so a journal
/// rewritten in between with another package / host / image went on to
/// the recovery.
#[test]
fn r19_s5_writer_gone_refuses_a_journal_whose_identity_changed() {
    use super::tests_r16_server::{
        cli_bin, finalize_case, finalize_ok, journal_of_live_writer, keyless, signoff_sha,
        spawn_live_writer,
    };
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let Some((root, case)) = finalize_case("r19-s5") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let mut child = spawn_live_writer();
    let result = std::panic::catch_unwind(|| {
        keyless(|| {
            finalize_ok(&bin, &case, "홍길동");
            let journal = journal_of_live_writer(&bin, &case, &child, Some("0".repeat(64)));
            let signoff = signoff_sha(&case);
            let changes: Vec<(&str, super::finalize_journal::FinalizeJournal)> = vec![
                (
                    "package_name",
                    super::finalize_journal::FinalizeJournal {
                        package_name: format!("{}_x", journal.package_name),
                        ..journal.clone()
                    },
                ),
                (
                    "writer_image_sha256",
                    super::finalize_journal::FinalizeJournal {
                        writer_image_sha256: Some("1".repeat(64)),
                        ..journal.clone()
                    },
                ),
                (
                    "host_id",
                    super::finalize_journal::FinalizeJournal {
                        host_id: "f".repeat(64),
                        ..journal.clone()
                    },
                ),
            ];
            for (field, changed) in changes {
                journal.write(&case).unwrap();
                super::finalize_journal::BETWEEN_WRITER_GONE_LOOKS.with(|cell| {
                    *cell.borrow_mut() =
                        Some(Box::new(move |case: &Path| changed.write(case).unwrap()))
                });
                let refused = super::finalize_journal::cli_writer_gone(&case, true);
                super::finalize_journal::BETWEEN_WRITER_GONE_LOOKS
                    .with(|cell| *cell.borrow_mut() = None);
                let refused = refused.expect_err(field);
                assert!(
                    refused.contains("저널이 바뀌었습니다"),
                    "{field}: {refused}"
                );
                assert_eq!(signoff_sha(&case), signoff, "{field}");
                assert!(
                    super::finalize_journal::journal_path(&case).exists(),
                    "{field}"
                );
            }
            let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
        })
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(root);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
