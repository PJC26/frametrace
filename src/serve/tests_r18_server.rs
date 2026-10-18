//! Round-18 track S (docs/PLAN_R18_2026-10-19.md): a pasted command is
//! paste-safe or absent (P29), a withheld status verdict is "unknown"
//! unless a recovery's is remembered (P31), and S3–S8.

use super::*;

const COMMAND_KEYS: [&str; 6] = [
    "dismiss_command",
    "dismiss_command_cmd",
    "force_dismiss_command",
    "force_dismiss_command_cmd",
    "writer_gone_command",
    "writer_gone_command_cmd",
];

fn scratch18(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r18s-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A case folder named `name` holding an unreadable journal (status
/// `unreadable` offers the dismiss command).
fn unreadable_case(root: &Path, name: &str) -> PathBuf {
    let case = root.join(name);
    std::fs::create_dir_all(case.join("review")).unwrap();
    std::fs::write(super::finalize_journal::journal_path(&case), b"{not json").unwrap();
    case
}

/// Folder names PowerShell / cmd.exe would read as more than a path
/// inside the quotes the command puts around it — and that Windows
/// accepts as a folder name. The R17 reviewer's PoC first: PowerShell
/// takes U+2018/U+2019/U+201A/U+201B as single quotes too, and 891f180
/// doubled only ASCII `'`.
const HOSTILE_NAMES: [&str; 9] = [
    "x\u{2019}; Write-Output injected; \u{2019}",
    "x\u{2018}; Write-Output injected; \u{2018}",
    "x\u{201A}; Write-Output injected; \u{201A}",
    "x\u{201B}; Write-Output injected; \u{201B}",
    "x'; Write-Output injected; '",
    "x!PATH!y",
    "x^&y",
    "x%PATH%y",
    "x$(Write-Output injected)y",
];

/// R18-S1 (P29, the R17 PoC): a case folder whose name holds a character
/// a shell may interpret gets NO command — every command field is null and
/// `command_unavailable_reason` says to type it. ff898f5 escaped only
/// ASCII `'`: `x’; calc; ’` yielded a dismiss command running `calc`.
#[test]
fn r18_s1_hostile_case_paths_get_no_command() {
    let root = scratch18("s1-hostile");
    for name in HOSTILE_NAMES {
        let case = unreadable_case(&root, name);
        let shown = super::finalize_journal::status_value(&case, false, false);
        assert_eq!(shown["state"], "unreadable", "{name}: {shown}");
        for key in COMMAND_KEYS {
            assert!(shown[key].is_null(), "{name}: {key}: {shown}");
        }
        assert_eq!(
            shown["command_unavailable_reason"], "경로에 특수 문자 — 명령을 직접 입력하십시오",
            "{name}: {shown}"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

/// The PowerShell parser's view of `command`: statement count, parse
/// errors and the single command's elements (type, value) — `None` when
/// no Windows PowerShell can be run.
#[cfg(windows)]
fn powershell_parse(command: &str) -> Option<serde_json::Value> {
    let dir = scratch18(&format!(
        "ps-{}",
        crate::sha256::digest_bytes(command.as_bytes())
            .get(..12)
            .unwrap_or_default()
    ));
    let input = dir.join("command.txt");
    let output = dir.join("parsed.json");
    std::fs::write(&input, command.as_bytes()).unwrap();
    let script = r#"
$text = [System.IO.File]::ReadAllText($env:FT_PS_IN, (New-Object System.Text.UTF8Encoding $false))
$tokens = $null; $errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseInput($text, [ref]$tokens, [ref]$errors)
$statements = $ast.EndBlock.Statements
$elements = @()
if ($statements.Count -eq 1 -and $statements[0] -is [System.Management.Automation.Language.PipelineAst] -and $statements[0].PipelineElements.Count -eq 1) {
  foreach ($e in $statements[0].PipelineElements[0].CommandElements) {
    $value = $e.Extent.Text
    $kind = ''
    if ($e -is [System.Management.Automation.Language.StringConstantExpressionAst]) { $value = $e.Value; $kind = [string]$e.StringConstantType }
    $elements += @{ type = $e.GetType().Name; kind = $kind; value = $value }
  }
}
$out = @{ statements = $statements.Count; errors = $errors.Count; pipeline = $statements[0].PipelineElements.Count; elements = $elements }
[System.IO.File]::WriteAllText($env:FT_PS_OUT, (ConvertTo-Json $out -Depth 5 -Compress), (New-Object System.Text.UTF8Encoding $false))
"#;
    let wide: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand"])
        .arg(crate::util::base64_encode(&wide))
        .env("FT_PS_IN", &input)
        .env("FT_PS_OUT", &output)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let parsed = match status {
        Ok(status) if status.success() => std::fs::read_to_string(&output)
            .ok()
            .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()),
        _ => None,
    };
    let _ = std::fs::remove_dir_all(dir);
    if parsed.is_none() {
        eprintln!("SKIPPED: Windows PowerShell's parser could not be run");
    }
    parsed
}

/// R18-S1 (P29): every PowerShell command the status offers parses, with
/// PowerShell's own parser, as ONE command whose path argument is the
/// case folder as a single-quoted constant — for an ordinary (Korean,
/// spaced, bracketed) folder name AND for every hostile one (where
/// ff898f5 offered a command that parsed as several statements).
#[cfg(windows)]
#[test]
fn r18_s1_powershell_parses_each_offered_command_as_one() {
    let root = scratch18("s1-parse");
    let mut names: Vec<&str> = vec!["홍길동 사건 (1) [2026]_v2.0-최종"];
    names.extend(HOSTILE_NAMES);
    let mut parsed_any = false;
    for name in names {
        let case = unreadable_case(&root, name);
        let shown = super::finalize_journal::status_value(&case, false, false);
        let Some(command) = shown["dismiss_command"].as_str() else {
            continue;
        };
        let Some(parsed) = powershell_parse(command) else {
            let _ = std::fs::remove_dir_all(&root);
            return;
        };
        parsed_any = true;
        assert_eq!(parsed["statements"], 1, "{name}: {command}: {parsed}");
        assert_eq!(parsed["errors"], 0, "{name}: {command}: {parsed}");
        assert_eq!(parsed["pipeline"], 1, "{name}: {command}: {parsed}");
        let elements = parsed["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 4, "{name}: {command}: {parsed}");
        assert_eq!(elements[0]["value"], "frametrace", "{parsed}");
        assert_eq!(elements[1]["value"], "finalize-journal", "{parsed}");
        assert_eq!(
            elements[2]["type"], "StringConstantExpressionAst",
            "{parsed}"
        );
        assert_eq!(elements[2]["kind"], "SingleQuoted", "{parsed}");
        assert_eq!(
            elements[2]["value"].as_str().unwrap(),
            case.display().to_string(),
            "{name}: {parsed}"
        );
        assert_eq!(elements[3]["value"], "--dismiss", "{parsed}");
    }
    assert!(parsed_any, "the ordinary folder name got no command");
    let _ = std::fs::remove_dir_all(root);
}

/// An interrupted finalize stopped after its package step whose manifest
/// is grown past 16 MiB (the status path's hash cap); this PC's
/// `packaged` record names `recorded` (`None`: the new bytes' SHA-256).
fn large_manifest_case(
    bin: &Path,
    case: &Path,
    recorded: Option<String>,
) -> super::finalize_journal::FinalizeJournal {
    use super::tests_r16_server::{finalize_ok, interrupted_finalize};
    finalize_ok(bin, case, "홍길동");
    let journal = interrupted_finalize(bin, case, super::jobs::FinalizeStop::Package);
    let manifest = case
        .join("reports")
        .join(&journal.package_name)
        .join("manifest.sha256");
    let mut bytes = std::fs::read(&manifest).unwrap();
    let line = format!("{}  padding.bin\n", "f".repeat(64));
    while bytes.len() <= 17 * 1024 * 1024 {
        bytes.extend_from_slice(line.as_bytes());
    }
    std::fs::write(&manifest, &bytes).unwrap();
    super::finalize_journal::LocalRecord {
        manifest_sha256: Some(recorded.unwrap_or_else(|| crate::sha256::digest_bytes(&bytes))),
        ..journal.local_record(case, super::finalize_journal::LOCAL_PACKAGED)
    }
    .write()
    .unwrap();
    journal
}

/// R18-S2 (P31, the R17 PoC): the status path does not hash a manifest
/// over 16 MiB — and ff898f5 then showed `completed-unrecorded`,
/// `blocks_finalize: false` even after a recovery (which hashes it in
/// full) had judged the package CHANGED and kept the journal. Now the
/// withheld status says `blocks_finalize: null` (unknown) until a
/// recovery's verdict for that manifest (same size and mtime) is
/// remembered, and then shows that verdict.
#[test]
fn r18_s2_status_shows_the_recoverys_package_changed_verdict() {
    use super::tests_r16_server::{bound_state, cli_bin, finalize_case, keyless, parse};
    let Some((root, case)) = finalize_case("r18-s2-changed") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let journal = large_manifest_case(&bin, &case, Some("e".repeat(64)));
        let state = bound_state(&case);
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["manifest_check"], "withheld", "{shown}");
        assert!(
            shown["blocks_finalize"].is_null(),
            "a withheld verdict is unknown, not 'does not block': {shown}"
        );
        super::finalize_journal::recover_bound_case(&state);
        assert!(super::finalize_journal::journal_path(&case).exists());
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["state"], "unpaired", "{shown}");
        assert_eq!(shown["unpaired_code"], "package-changed", "{shown}");
        assert_eq!(shown["blocks_finalize"], true, "{shown}");
        assert_eq!(shown["manifest_check"], "recovery-verdict", "{shown}");
        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R18-S1 (P29), pure: the whitelist refuses every character a shell may
/// read — including those no Windows folder name can hold — and keeps
/// ordinary (Hangul, spaced, bracketed, 8.3) paths.
#[test]
fn r18_s1_whitelist_refuses_every_shell_character() {
    use super::finalize_journal as fj;
    let refused = [
        "\u{2018}", "\u{2019}", "\u{201A}", "\u{201B}", "'", "\"", "\u{201C}", "\u{201D}",
        "\u{201E}", "`", "$", ";", "%", "!", "^", "&", "|", "<", ">", "*", "?", "\t", "\n", "\r",
        "\u{7}", "\u{1b}", "\u{7f}", "\u{a0}", "\u{2013}", "\u{2014}", "\u{3000}", "\u{ff02}",
        "\u{ff07}", "\u{fffd}", "\u{200b}",
    ];
    for bad in refused {
        let path = PathBuf::from(format!("C:\\cases\\x{bad}y"));
        for command in [
            fj::dismiss_command(&path),
            fj::dismiss_command_cmd(&path),
            fj::force_dismiss_command(&path),
            fj::force_dismiss_command_cmd(&path),
            fj::writer_gone_command(&path, false),
            fj::writer_gone_command_cmd(&path, true),
        ] {
            assert!(command.is_none(), "{bad:?}: {command:?}");
        }
    }
    // R19-S1 (P32): a relative `-x` is made absolute first — the path in
    // the command never starts with `-` (it cannot read as an option).
    if let Some(command) = fj::dismiss_command(Path::new("-x")) {
        assert!(!command.contains(" '-"), "an option: {command}");
    }
    for good in [
        "C:\\cases\\홍길동 사건 (1)",
        "C:\\Users\\DAERYU~1\\AppData\\Local\\Temp\\ft-r18s-a_b.c",
        "/home/user/cases/a+b=c@d#e[1],2",
    ] {
        let command = fj::dismiss_command(Path::new(good)).expect(good);
        // R19-S1 (P32): the path as normalized (absolute, no trailing
        // separator).
        let normalized = crate::paste_path::normalize(Path::new(good));
        assert_eq!(
            command,
            format!(
                "frametrace finalize-journal '{}' --dismiss",
                normalized.display()
            )
        );
        assert_eq!(
            fj::dismiss_command_cmd(Path::new(good)).is_some(),
            cfg!(windows),
            "{good}"
        );
    }
    assert_eq!(
        fj::writer_gone_command(Path::new("C:\\a"), true).unwrap(),
        format!(
            "frametrace finalize-journal '{}' --writer-gone --i-am-sure",
            crate::paste_path::normalize(Path::new("C:\\a")).display()
        )
    );
}

/// R18-S1: the CLI and the recovery's words never put a hostile path
/// between quotes — they give the command's shape and the reason.
#[test]
fn r18_s1_cli_text_names_no_hostile_path() {
    let root = scratch18("s1-cli");
    let case = unreadable_case(&root, HOSTILE_NAMES[0]);
    let out = super::finalize_journal::cli_command(&case, false, false).unwrap();
    assert!(
        out.contains(super::finalize_journal::COMMAND_UNAVAILABLE_REASON),
        "{out}"
    );
    assert!(
        out.contains("frametrace finalize-journal <케이스 폴더> --dismiss"),
        "{out}"
    );
    assert!(!out.contains("finalize-journal '"), "{out}");
    let state = super::tests_r16_server::bound_state(&case);
    super::finalize_journal::recover_bound_case(&state);
    let logs = state_lock(&state).logs.join("\n");
    assert!(
        logs.contains(super::finalize_journal::COMMAND_UNAVAILABLE_REASON),
        "{logs}"
    );
    assert!(!logs.contains("finalize-journal '"), "{logs}");
    let _ = std::fs::remove_dir_all(root);
}

/// R18-S2 (P31): a recovery's FINISHED verdict on a large manifest is
/// shown (`recovery-verdict`, not blocking) while the manifest keeps its
/// size and mtime; once either changes the status is unknown again.
#[test]
fn r18_s2_remembered_finished_verdict_follows_the_manifest_stamp() {
    use super::tests_r16_server::{bound_state, cli_bin, finalize_case, keyless, parse};
    let Some((root, case)) = finalize_case("r18-s2-finished") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let journal = large_manifest_case(&bin, &case, None);
        let state = bound_state(&case);
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert!(shown["blocks_finalize"].is_null(), "{shown}");
        // A full-hash look that changes nothing (what a recovery judges by).
        assert!(matches!(
            super::finalize_journal::assess(&case),
            Some(super::finalize_journal::Assessment::Finished { .. })
        ));
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["state"], "completed-unrecorded", "{shown}");
        assert_eq!(shown["manifest_check"], "recovery-verdict", "{shown}");
        assert_eq!(shown["blocks_finalize"], false, "{shown}");
        // The manifest changes (one byte more): no verdict for it.
        let manifest = case
            .join("reports")
            .join(&journal.package_name)
            .join("manifest.sha256");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&manifest)
            .unwrap();
        std::io::Write::write_all(&mut file, b"\n").unwrap();
        drop(file);
        let shown = parse(&api_status(&state))["finalize_journal"].clone();
        assert_eq!(shown["manifest_check"], "withheld", "{shown}");
        assert!(shown["blocks_finalize"].is_null(), "{shown}");
        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R18-S2 (P31): `frametrace finalize-journal <case>` judges by the FULL
/// hash — the package change the status path withholds is named.
#[test]
fn r18_s2_cli_uses_the_full_hash() {
    use super::tests_r16_server::{cli_bin, finalize_case, keyless};
    let Some((root, case)) = finalize_case("r18-s2-cli") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let journal = large_manifest_case(&bin, &case, Some("e".repeat(64)));
        let out = super::finalize_journal::cli_command(&case, false, false).unwrap();
        assert!(out.contains("state: unpaired"), "{out}");
        assert!(out.contains("unpaired_code: package-changed"), "{out}");
        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R18-S5: another account's block promises nothing this account cannot
/// do — recovery happens in THAT account; here only moving the journal.
#[test]
fn r18_s5_other_account_text_is_conditional() {
    let journal = super::finalize_journal::FinalizeJournal {
        writer: crate::util::WriterIdentity {
            pid: 4242,
            started_ms: None,
            image: None,
        },
        package_name: "package_1".to_string(),
        ..Default::default()
    };
    let text = super::finalize_journal::other_account_text(
        &journal,
        "frametrace-app.exe",
        1_800_000_000,
        Path::new("C:/cases/a"),
    );
    assert!(!text.contains("이 PC가 정상 복구"), "{text}");
    assert!(text.contains("그 계정에서 케이스를 열면"), "{text}");
    assert!(text.contains("옮기기만"), "{text}");
    assert!(text.find("기다리면").unwrap() < text.find("--writer-gone").unwrap());
}

/// R18-S6: a finalize refused for a lane that takes no append says so in
/// its own words (R17 said "이상 징후 검사를 시작하지 않았습니다").
#[test]
fn r18_s6_finalize_refusal_has_its_own_words() {
    use super::tests_r16_server::{bound_state, cli_bin, finalize_case, keyless, parse};
    let Some((root, case)) = finalize_case("r18-s6") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        let anomaly = case.join(crate::report::ANOMALY_LOG_REL_PATH);
        std::fs::create_dir_all(anomaly.parent().unwrap()).unwrap();
        let mut torn = std::fs::read(&anomaly).unwrap_or_default();
        torn.extend_from_slice(b"{\"torn\":");
        std::fs::write(&anomaly, &torn).unwrap();
        let state = bound_state(&case);
        let response = parse(&finalize_with_exe(&bin, &case, "김철수", &state));
        let error = response["error"].as_str().unwrap_or_default();
        assert_eq!(response["ok"], false, "{response}");
        assert!(
            error.contains(&format!(
                "{}에 이어 쓸 수 없음: ",
                crate::report::ANOMALY_LOG_REL_PATH
            )),
            "{error}"
        );
        assert!(!error.contains("이상 징후 검사를 시작하지"), "{error}");
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R18-S4: `--writer-gone` prints what the recovery logged — here the
/// note that a local record that could not advance was dropped.
#[test]
fn r18_s4_writer_gone_prints_the_recovery_log() {
    use super::tests_r16_server::{
        cli_bin, finalize_case, finalize_ok, interrupted_finalize, keyless,
    };
    let Some((root, case)) = finalize_case("r18-s4") else {
        return;
    };
    let bin = cli_bin().unwrap();
    keyless(|| {
        finalize_ok(&bin, &case, "홍길동");
        let journal = interrupted_finalize(&bin, &case, super::jobs::FinalizeStop::Chain);
        super::finalize_journal::FAIL_LOCAL_PAST_OPEN.with(|cell| cell.set(true));
        let out = super::finalize_journal::cli_writer_gone(&case, false);
        super::finalize_journal::FAIL_LOCAL_PAST_OPEN.with(|cell| cell.set(false));
        let out = out.unwrap_or_else(|err| err);
        assert!(
            out.contains(&format!(
                "log: 경고: {}",
                super::finalize_journal::dropped_note(&journal.finalize_id)
            )),
            "{out}"
        );
        let _ = super::finalize_journal::remove_local_record(&journal.finalize_id);
    });
    let _ = std::fs::remove_dir_all(root);
}

/// R18-S7: `--writer-gone` needs `--i-am-sure` exactly when a live process
/// holds the writer's pid AND started when the writer did.
#[test]
fn r18_s7_sure_is_needed_only_for_a_matching_live_process() {
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let mut child = super::tests_r16_server::spawn_live_writer();
    let writer = super::tests_r16_server::live_writer_of(child.id());
    let result = std::panic::catch_unwind(|| {
        let mut journal = super::finalize_journal::FinalizeJournal {
            writer: writer.clone(),
            ..Default::default()
        };
        assert!(super::finalize_journal::writer_gone_needs_sure(&journal));
        journal.writer.started_ms = writer.started_ms.map(|ms| ms + 600_000);
        assert!(
            !super::finalize_journal::writer_gone_needs_sure(&journal),
            "a reused pid"
        );
        journal.writer.started_ms = None;
        assert!(
            !super::finalize_journal::writer_gone_needs_sure(&journal),
            "unknown"
        );
        journal.writer = crate::util::WriterIdentity {
            pid: std::process::id(),
            ..writer.clone()
        };
        assert!(
            !super::finalize_journal::writer_gone_needs_sure(&journal),
            "this process"
        );
    });
    let _ = child.kill();
    let _ = child.wait();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// R18-S3: the two looks of `--writer-gone` must see the same journal —
/// one replaced in between (another finalize id) is refused, and one
/// whose writer turned out VERIFIED as this program is refused even
/// though the examiner said "gone". Nothing is rolled back either way.
#[test]
fn r18_s3_writer_gone_refuses_a_journal_changed_between_its_looks() {
    use super::tests_r16_server::{
        cli_bin, finalize_case, finalize_ok, journal_of_live_writer, keyless, signoff_sha,
        spawn_live_writer,
    };
    if !cfg!(any(windows, target_os = "linux")) {
        return;
    }
    let Some((root, case)) = finalize_case("r18-s3") else {
        return;
    };
    let bin = cli_bin().unwrap();
    let mut child = spawn_live_writer();
    let result = std::panic::catch_unwind(|| {
        keyless(|| {
            finalize_ok(&bin, &case, "홍길동");
            let journal = journal_of_live_writer(&bin, &case, &child, Some("0".repeat(64)));
            let signoff = signoff_sha(&case);
            let hook = |f: Box<dyn Fn(&Path)>| {
                super::finalize_journal::BETWEEN_WRITER_GONE_LOOKS
                    .with(|cell| *cell.borrow_mut() = Some(f));
            };
            let clear = || {
                super::finalize_journal::BETWEEN_WRITER_GONE_LOOKS
                    .with(|cell| *cell.borrow_mut() = None);
            };
            // Verified between the looks.
            let verified = super::finalize_journal::FinalizeJournal {
                writer_image_sha256: super::finalize_journal::own_exe_sha256(),
                ..journal.clone()
            };
            hook(Box::new(move |case: &Path| verified.write(case).unwrap()));
            let refused = super::finalize_journal::cli_writer_gone(&case, true);
            clear();
            let refused = refused.unwrap_err();
            assert!(
                refused.contains("--writer-gone을 적용하지 않았습니다"),
                "{refused}"
            );
            assert_eq!(signoff_sha(&case), signoff);
            // Another finalize id between the looks.
            journal.write(&case).unwrap();
            let replaced = super::finalize_journal::FinalizeJournal {
                finalize_id: "f".repeat(32),
                ..journal.clone()
            };
            hook(Box::new(move |case: &Path| replaced.write(case).unwrap()));
            let refused = super::finalize_journal::cli_writer_gone(&case, true);
            clear();
            let refused = refused.unwrap_err();
            assert!(refused.contains("저널이 바뀌었습니다"), "{refused}");
            assert_eq!(signoff_sha(&case), signoff);
            assert!(super::finalize_journal::journal_path(&case).exists());
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
