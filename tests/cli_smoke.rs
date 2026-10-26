mod common;
use common::IsolatedCommand;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn frametrace() -> &'static str {
    env!("CARGO_BIN_EXE_frametrace")
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after UNIX epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("frametrace-{name}-{}-{nanos}", std::process::id()))
}

fn run(args: &[&str]) -> Output {
    Command::new(frametrace())
        .args(args)
        .isolated()
        .output()
        .expect("frametrace binary should run")
}

/// R5 U11: a spawned binary never inherits the runner's `FRAMETRACE_*`
/// variables (only what `isolated()` / the test sets explicitly).
#[test]
fn isolated_children_drop_inherited_frametrace_vars() {
    let picked = common::inherited_frametrace_vars([
        "PATH",
        "FRAMETRACE_AUDIT_KEY",
        "frametrace_token",
        "XFRAMETRACE_UI",
        "FRAMETRACE_UI",
    ]);
    assert_eq!(
        picked,
        vec![
            std::ffi::OsString::from("FRAMETRACE_AUDIT_KEY"),
            std::ffi::OsString::from("frametrace_token"),
            std::ffi::OsString::from("FRAMETRACE_UI"),
        ]
    );
    let mut command = Command::new("frametrace-env-probe");
    command.isolated();
    let envs: Vec<(String, Option<String>)> = command
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().to_string(),
                v.map(|v| v.to_string_lossy().to_string()),
            )
        })
        .collect();
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy().to_string();
        if !name.to_ascii_uppercase().starts_with("FRAMETRACE_")
            || name.eq_ignore_ascii_case("FRAMETRACE_INSTANCE_FILE")
        {
            continue;
        }
        assert!(
            envs.iter().any(|(k, v)| k == &name && v.is_none()),
            "{name} is removed from the child: {envs:?}"
        );
    }
    // The isolation itself is set, never inherited.
    assert!(
        envs.iter()
            .any(|(k, v)| k == "FRAMETRACE_INSTANCE_FILE" && v.is_some())
    );
    assert!(envs.iter().any(|(k, v)| k == "APPDATA" && v.is_some()));
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// T6: `qa performance` has a hard 50k rows/min wall-clock target inside the
/// binary (src/qa.rs). That is meaningless on a debug build or a loaded CI
/// runner, so the smoke test tolerates ONLY the throughput miss: the report
/// must still exist, carry the measured numbers, and (for `qa release`) every
/// other check must pass. Set FRAMETRACE_PERF_STRICT=1 to require the target,
/// or FRAMETRACE_PERF_RELAXED=1 to relax it on a release/CI build.
fn perf_relaxed() -> bool {
    let on = |k: &str| std::env::var(k).as_deref() == Ok("1");
    !on("FRAMETRACE_PERF_STRICT") && (cfg!(debug_assertions) || on("FRAMETRACE_PERF_RELAXED"))
}

fn assert_perf_report_tolerant(output: &Output, report: &Path) {
    if output.status.success() {
        return;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed = fs::read_to_string(report)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let throughput_only = parsed
        .as_ref()
        .is_some_and(|v| v["qa_type"] == "performance" && v["rows_per_minute"].is_number());
    assert!(
        perf_relaxed() && throughput_only && stderr.contains("performance QA failed"),
        "qa performance failed (throughput target 50000 rows/min; debug builds and loaded CI          runners are slow - set FRAMETRACE_PERF_RELAXED=1 to tolerate, FRAMETRACE_PERF_STRICT=1          to enforce)
stderr:
{stderr}"
    );
    eprintln!("NOTE: qa performance below 50k rows/min tolerated (relaxed): {stderr}");
}

fn assert_release_tolerant(output: &Output, release_json: &Path) {
    if output.status.success() {
        return;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed = fs::read_to_string(release_json)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let only_perf_blocked = parsed.as_ref().is_some_and(|v| {
        v["checks"].as_array().is_some_and(|checks| {
            checks
                .iter()
                .all(|c| c["status"] == "PASS" || c["name"] == "performance")
        })
    });
    assert!(
        perf_relaxed() && only_perf_blocked,
        "qa release failed for a reason other than the (relaxable) performance throughput          check
stderr:
{stderr}
report: {parsed:?}"
    );
    eprintln!("NOTE: qa release blocked only by tolerated performance throughput: {stderr}");
}

#[test]
fn review_annotations_roundtrip_and_explicit_removals_preserve_unrelated_rows() {
    let root = unique_temp_dir("annotations");
    let case_dir = root.join("case");
    assert_success(&run(&["init-case", path(&case_dir)]));
    let input = root.join("marks.json");
    let seed = serde_json::json!({"schema_version":2,"examiner":"fallback","marks":[
        {"id":"a","status":"important","note":"memo","examiner":"Alice"},
        {"id":"b","status":"reviewed","note":"keep","examiner":"Bob"}
    ],"tags":[{"id":"a","tags":["사고","custom"]},{"id":"b","tags":["keep"]}]});
    fs::write(&input, seed.to_string()).unwrap();
    assert_success(&run(&["import-marks", path(&case_dir), path(&input)]));
    assert_success(&run(&["export-marks", path(&case_dir)]));
    let exported = case_dir.join("db/review-marks.json");
    let read = || {
        serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&exported).unwrap()).unwrap()
    };
    let initial = read();
    assert_eq!(initial["marks"][0]["examiner"], "Alice");
    assert_eq!(
        initial["tags"][0]["tags"],
        serde_json::json!(["사고", "custom"])
    );
    assert_success(&run(&["import-marks", path(&case_dir), path(&exported)]));
    fs::write(&input, r#"{"schema_version":2,"protocol":"patch-v1","marks":[{"id":"a","status":"important","note":""}],"tags":[{"id":"a","tags":[]}]}"#).unwrap();
    assert_success(&run(&["import-marks", path(&case_dir), path(&input)]));
    assert_success(&run(&["export-marks", path(&case_dir)]));
    assert_eq!(read()["marks"][0]["note"], "");
    fs::write(
        &input,
        r#"{"schema_version":2,"protocol":"patch-v1","marks":[],"deleted_ids":["a"]}"#,
    )
    .unwrap();
    assert_success(&run(&["import-marks", path(&case_dir), path(&input)]));
    assert_success(&run(&["export-marks", path(&case_dir)]));
    let final_state = read();
    assert_eq!(final_state["marks"].as_array().unwrap().len(), 1);
    assert_eq!(final_state["marks"][0]["id"], "b");
    assert_eq!(final_state["marks"][0]["examiner"], "Bob");
    assert_eq!(final_state["marks"][0]["note"], "keep");
    assert_eq!(
        final_state["tags"],
        serde_json::json!([{"id":"b","tags":["keep"]}])
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn anomaly_rehash_reports_coverage_in_log_and_stdout() {
    let root = unique_temp_dir("anomaly-coverage");
    let case_dir = root.join("case");
    assert_success(&run(&["init-case", path(&case_dir)]));
    let media = root.join("clip.mp4");
    fs::write(&media, b"payload").unwrap();
    let digest = frametrace::audit::digest_file(&media).unwrap();
    for (name, rows, counts, limited) in [
        (
            "skips",
            vec![
                serde_json::json!({"id":"missing", "source_path":root.join("missing.mp4"), "sha256":digest}),
                serde_json::json!({"id":"unhashed", "source_path":media}),
            ],
            [1, 1, 0],
            true,
        ),
        (
            "error",
            vec![serde_json::json!({"id":"directory", "source_path":root, "sha256":digest})],
            [0, 0, 1],
            true,
        ),
        (
            "complete",
            vec![serde_json::json!({"id":"readable", "source_path":media, "sha256":digest})],
            [0, 0, 0],
            false,
        ),
    ] {
        let index = rows
            .iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>();
        fs::write(case_dir.join("db/videos.jsonl"), index).unwrap();
        let output = run(&["qa", "anomalies", path(&case_dir)]);
        assert_success(&output);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let log = fs::read_to_string(case_dir.join("evidence/logs/anomaly-log.jsonl")).unwrap();
        let entry: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
        assert_eq!(entry["event"], "anomaly-scan-run");
        for (field, count) in ["skipped_no_hash", "skipped_missing", "skipped_error"]
            .iter()
            .zip(counts)
        {
            assert_eq!(entry[field], count, "{name}: {entry}");
            assert!(
                stdout.contains(&format!("{field}={count}")),
                "{name}: {stdout}"
            );
        }
        assert_eq!(entry["coverage_limited"], limited);
        if limited {
            assert!(
                entry["detail"]
                    .as_str()
                    .unwrap()
                    .contains("coverage limitation")
            );
            assert!(stdout.contains("coverage limitation"), "{stdout}");
            assert!(
                !log.lines()
                    .last()
                    .unwrap()
                    .contains("live re-hashing of every indexed file")
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn report_outputs_preserve_existing_and_protected_case_files() {
    let root = unique_temp_dir("output-protection");
    let case_dir = root.join("case");
    assert_success(&run(&["init-case", path(&case_dir)]));
    let list = root.join("hashes.txt");
    fs::write(&list, "").unwrap();
    let mut failures = Vec::new();
    for (command, default) in [
        ("timeline", "db/timeline.jsonl"),
        ("compare-cases", "reports/case-compare.json"),
        ("known-hash-filter", "reports/known-hash-filter.json"),
        ("export-dfxml", "reports/case-index.dfxml"),
        ("export-marks", "db/review-marks.json"),
    ] {
        let invoke = |output: Option<&Path>| {
            let mut args = vec![command, path(&case_dir)];
            if command == "compare-cases" {
                args.push(path(&case_dir));
            } else if command == "known-hash-filter" {
                args.push(path(&list));
            }
            if let Some(output) = output {
                args.extend(["--output", path(output)]);
            }
            run(&args)
        };
        let custom = case_dir.join(format!("{command}-custom.out"));
        fs::write(&custom, "preserve me").unwrap();
        let output = invoke(Some(&custom));
        if output.status.success() || fs::read(&custom).unwrap() != b"preserve me" {
            failures.push(format!("{command} overwrote an existing custom output"));
        }
        for protected in [
            "case.json",
            "db/case.db",
            "db/videos.jsonl",
            "db/video_paths.tsv",
            "db/video_index.json",
            "db/scan_runs/new.json",
            "evidence/logs/new.jsonl",
            "review/new.html",
            "reports/other-report.json",
        ] {
            let target = case_dir.join(protected);
            let before = fs::read(&target).ok();
            let output = invoke(Some(&target));
            let after = fs::read(&target).ok();
            if output.status.success() || before != after {
                failures.push(format!("{command} modified protected {protected}"));
            }
            match before {
                Some(bytes) => fs::write(&target, bytes).unwrap(),
                None => {
                    let _ = fs::remove_file(&target);
                }
            }
        }
        for _ in 0..2 {
            assert_success(&invoke(None));
            assert!(case_dir.join(default).is_file());
        }
        assert_success(&invoke(Some(&case_dir.join(default))));
        let fresh = case_dir.join(format!("exports/{command}.out"));
        assert_success(&invoke(Some(&fresh)));
        assert!(fresh.is_file());
    }
    fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn help_command_succeeds() {
    let output = run(&["--help"]);
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("scan-folder"));
}

#[test]
fn case_lifecycle_smoke_test_uses_real_binary() {
    let root = unique_temp_dir("cli-lifecycle");
    let case_dir = root.join("case");
    let media_dir = root.join("media");
    fs::create_dir_all(&media_dir).expect("media dir should be created");
    fs::write(media_dir.join("clip.mp4"), b"\0\0\0\x18ftypmp42payload")
        .expect("fixture video should be written");

    assert_success(&run(&[
        "init-case",
        path(&case_dir),
        "--title",
        "Smoke Case",
    ]));
    assert_success(&run(&[
        "scan-folder",
        path(&case_dir),
        path(&media_dir),
        "--no-ffprobe",
    ]));
    assert_success(&run(&["make-review", path(&case_dir)]));
    assert_success(&run(&["make-report", path(&case_dir)]));

    let corpus_manifest = root.join("corpus.tsv");
    let indexed_source = media_dir
        .join("clip.mp4")
        .canonicalize()
        .expect("fixture path should canonicalize");
    fs::write(
        &corpus_manifest,
        format!("source_path\tsha256\n{}\t\n", indexed_source.display()),
    )
    .expect("corpus manifest should be written");
    assert_success(&run(&[
        "qa",
        "accuracy",
        path(&case_dir),
        path(&corpus_manifest),
    ]));
    assert_success(&run(&[
        "qa",
        "reproducibility",
        path(&case_dir),
        path(&case_dir),
    ]));
    assert_success(&run(&["qa", "report-defense", path(&case_dir)]));
    assert_perf_report_tolerant(
        &run(&[
            "qa",
            "performance",
            path(&root.join("qa-performance")),
            "--rows",
            "1000",
        ]),
        &root.join("qa-performance/performance-report.json"),
    );
    assert_release_tolerant(
        &run(&[
            "qa",
            "release",
            path(&case_dir),
            "--corpus-manifest",
            path(&corpus_manifest),
            "--comparison-case",
            path(&case_dir),
            "--performance-output-dir",
            path(&root.join("qa-release-performance")),
            "--performance-rows",
            "1000",
        ]),
        &case_dir.join("reports/qa/release-readiness.json"),
    );

    assert_success(&run(&["package-case", path(&case_dir)]));
    assert_success(&run(&["inspect", path(&case_dir)]));

    let selection_file = root.join("selection.json");
    let fixture_id = "vid_000001";
    fs::write(
        &selection_file,
        format!(
            r#"{{"schema_version":1,"items":[{{"selector":"{fixture_id}","kind":"video","action":"export","format":"mp4"}}]}}"#
        ),
    )
    .expect("selection file should be written");
    let dry_run = run(&[
        "export-batch",
        path(&case_dir),
        path(&selection_file),
        "--dry-run",
    ]);
    assert_success(&dry_run);
    assert!(String::from_utf8_lossy(&dry_run.stdout).contains("would export"));

    let marks_file = root.join("marks.json");
    fs::write(
        &marks_file,
        r#"{"schema_version":1,"marks":[{"id":"vid_000001","status":"important","marked_unix":100}]}"#,
    )
    .expect("marks file should be written");
    assert_success(&run(&["import-marks", path(&case_dir), path(&marks_file)]));
    // export-marks output is confined inside the case directory.
    let marks_out = case_dir.join("db/exported-marks.json");
    assert_success(&run(&[
        "export-marks",
        path(&case_dir),
        "--output",
        path(&marks_out),
    ]));
    let exported = fs::read_to_string(&marks_out).expect("marks export should be readable");
    assert!(exported.contains("vid_000001"));
    assert!(exported.contains("important"));

    assert!(case_dir.join("db/video_index.json").is_file());
    assert!(case_dir.join("review/index.html").is_file());
    assert!(case_dir.join("review/evidence-viewer.html").is_file());
    assert!(case_dir.join("reports/case-report.html").is_file());
    assert!(case_dir.join("reports/qa/accuracy-report.json").is_file());
    assert!(
        case_dir
            .join("reports/qa/reproducibility-report.json")
            .is_file()
    );
    assert!(
        case_dir
            .join("reports/qa/report-defense-checklist.md")
            .is_file()
    );
    assert!(
        root.join("qa-performance/performance-report.json")
            .is_file()
    );
    assert!(case_dir.join("reports/qa/release-readiness.json").is_file());
    assert!(
        root.join("qa-release-performance/performance-report.json")
            .is_file()
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn init_case_refuses_existing_case_and_generates_unique_ids() {
    let root = unique_temp_dir("cli-init-refuse");
    let first_dir = root.join("case-first");
    fs::create_dir_all(&root).expect("root dir should be created");
    assert_success(&run(&[
        "init-case",
        path(&first_dir),
        "--title",
        "Original Case",
        "--operator",
        "Original Operator",
    ]));
    let first_manifest =
        fs::read_to_string(first_dir.join("case.json")).expect("first manifest should be readable");

    let refusal = run(&["init-case", path(&first_dir), "--title", "Replacement"]);
    assert!(
        !refusal.status.success(),
        "re-init must fail\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&refusal.stdout),
        String::from_utf8_lossy(&refusal.stderr)
    );
    let stderr = String::from_utf8_lossy(&refusal.stderr);
    assert!(
        stderr.contains("already exists"),
        "stderr should explain the refusal, was: {stderr}"
    );
    let after_refusal =
        fs::read_to_string(first_dir.join("case.json")).expect("manifest should remain");
    assert_eq!(
        first_manifest, after_refusal,
        "refused re-init must not modify the existing manifest"
    );

    let second_dir = root.join("case-second");
    assert_success(&run(&["init-case", path(&second_dir)]));
    let read_case_id = |dir: &Path| {
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("case.json")).unwrap()).unwrap();
        manifest["case_id"].as_str().unwrap().to_string()
    };
    let second_id = read_case_id(&second_dir);
    assert!(!second_id.is_empty(), "second case id should be printed");
    let first_id = read_case_id(&first_dir);
    assert_ne!(
        first_id, second_id,
        "case ids created back-to-back must not collide"
    );

    let empty_dir = root.join("empty");
    fs::create_dir(&empty_dir).unwrap();
    assert_success(&run(&["init-case", path(&empty_dir)]));
    assert!(empty_dir.join("evidence/logs").is_dir());
    assert!(empty_dir.join("artifacts/clips").is_dir());
    assert!(empty_dir.join("db").is_dir());
    let id = read_case_id(&empty_dir);
    let suffix = id.rsplit('-').next().unwrap();
    assert_eq!(suffix.len(), 32);
    assert!(suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let occupied = root.join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("evidence.bin"), b"preserve me").unwrap();
    assert!(!run(&["init-case", path(&occupied)]).status.success());
    assert_eq!(
        fs::read(occupied.join("evidence.bin")).unwrap(),
        b"preserve me"
    );
    assert!(!occupied.join("case.json").exists());
    assert_eq!(fs::read_dir(&occupied).unwrap().count(), 1);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn concurrent_case_initialization_has_one_winner() {
    let root = unique_temp_dir("cli-init-concurrent");
    fs::create_dir_all(&root).unwrap();
    let barrier = std::sync::Barrier::new(4);
    let outputs = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    run(&["init-case", path(&root)])
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("case.json")).unwrap()).unwrap();
    assert!(manifest["case_id"].as_str().unwrap().starts_with("FT-"));
    assert!(!fs::read_dir(&root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
    fs::remove_dir_all(root).unwrap();
}

fn case_snapshot(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                files.insert(relative, Vec::new());
                visit(root, &path, files);
            } else {
                files.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    visit(dir, dir, &mut files);
    files
}

#[test]
fn case_binding_rejects_cross_case_imports_before_side_effects() {
    let root = unique_temp_dir("cli-case-binding");
    let case_dir = root.join("case");
    let other_dir = root.join("other");
    assert_success(&run(&["init-case", path(&case_dir)]));
    assert_success(&run(&["init-case", path(&other_dir)]));
    let other: serde_json::Value =
        serde_json::from_slice(&fs::read(other_dir.join("case.json")).unwrap()).unwrap();
    let import = root.join("import.json");
    fs::write(
        &import,
        serde_json::json!({
            "schema_version": 1,
            "case_id": other["case_id"],
            "items": [{"selector": "vid_000001", "action": "validate"}],
            "marks": [{"id": "vid_000001", "status": "important"}]
        })
        .to_string(),
    )
    .unwrap();
    let before = case_snapshot(&case_dir);
    for command in [
        "export-batch",
        "validate-batch",
        "recover-batch",
        "import-marks",
    ] {
        let output = if command == "recover-batch" {
            run(&[
                command,
                path(&case_dir),
                path(&root.join("missing.img")),
                path(&import),
            ])
        } else {
            run(&[command, path(&case_dir), path(&import)])
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{command} accepted another case");
        assert!(stderr.contains("case_id mismatch"), "{command}: {stderr}");
        assert_eq!(
            case_snapshot(&case_dir),
            before,
            "{command} mutated the case"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn case_binding_round_trips_marks_and_warns_for_legacy_imports() {
    let root = unique_temp_dir("cli-binding-roundtrip");
    let case_dir = root.join("case");
    assert_success(&run(&["init-case", path(&case_dir)]));
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(case_dir.join("case.json")).unwrap()).unwrap();
    let import = root.join("import.json");
    for case_id in [
        None,
        Some(serde_json::Value::Null),
        Some(manifest["case_id"].clone()),
    ] {
        let mut payload = serde_json::json!({
            "schema_version": 1,
            "items": [{"selector": "vid_000001", "action": "validate"}],
            "marks": [{"id": "vid_000001", "status": "important", "marked_unix": 100}]
        });
        if let Some(id) = &case_id {
            payload["case_id"] = id.clone();
        }
        fs::write(&import, payload.to_string()).unwrap();
        for command in [
            "import-marks",
            "export-batch",
            "validate-batch",
            "recover-batch",
        ] {
            let output = if command == "recover-batch" {
                run(&[
                    command,
                    path(&case_dir),
                    path(&root.join("missing.img")),
                    path(&import),
                ])
            } else {
                run(&[command, path(&case_dir), path(&import)])
            };
            if matches!(command, "import-marks" | "export-batch") {
                assert_success(&output);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                stderr.contains("warning:"),
                case_id.as_ref().is_none_or(|id| id.is_null()),
                "{command}: {stderr}"
            );
            if case_id.as_ref().is_none_or(|id| id.is_null()) {
                assert!(
                    stderr.contains("case binding unavailable"),
                    "{command}: {stderr}"
                );
            } else {
                assert!(!stderr.contains("case_id mismatch"), "{command}: {stderr}");
            }
        }
    }
    assert_success(&run(&["export-marks", path(&case_dir)]));
    let exported = case_dir.join("db/review-marks.json");
    let payload: serde_json::Value = serde_json::from_slice(&fs::read(&exported).unwrap()).unwrap();
    assert_eq!(payload["case_id"], manifest["case_id"]);
    assert_success(&run(&["import-marks", path(&case_dir), path(&exported)]));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn external_tool_failure_reports_install_guidance() {
    let root = unique_temp_dir("cli-external-tool");
    let case_dir = root.join("case");
    let e01_path = root.join("sample.E01");
    fs::create_dir_all(&root).expect("root dir should be created");
    fs::write(&e01_path, b"not a real image").expect("fixture E01 should be written");
    assert_success(&run(&[
        "init-case",
        path(&case_dir),
        "--title",
        "External Tool Case",
    ]));

    let output = run(&[
        "inspect-e01",
        path(&case_dir),
        path(&e01_path),
        "--ewfinfo",
        "frametrace-definitely-missing-ewfinfo",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("install libewf tools"),
        "stderr was: {stderr}"
    );

    let _ = fs::remove_dir_all(root);
}

/// rotate-audit-key end-to-end: two generations of signed marker entries
/// verify keyed because the retired key stays in the keyring.
#[test]
fn rotate_audit_key_round_trips_through_verify() {
    let root = unique_temp_dir("cli-rotate");
    fs::create_dir_all(&root).expect("root dir should be created");
    let keyring = root.join("audit-keys.json");
    let log = root.join("audit.jsonl");

    let framed = |args: &[&str]| {
        Command::new(frametrace())
            .args(args)
            .isolated()
            .env("FRAMETRACE_AUDIT_KEYRING_FILE", &keyring)
            .env_remove("FRAMETRACE_AUDIT_KEY")
            .env_remove("FRAMETRACE_AUDIT_KEY_FILE")
            .output()
            .expect("frametrace binary should run")
    };

    // R10-I9: a missing EXPLICIT keyring path is created only with --init.
    let refused = framed(&["rotate-audit-key", "--key-id", "k0", "--log", path(&log)]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--init"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!keyring.exists() && !log.exists());
    assert_success(&framed(&[
        "rotate-audit-key",
        "--init",
        "--key-id",
        "k1",
        "--log",
        path(&log),
    ]));
    assert!(keyring.is_file());
    let first = fs::read_to_string(&log).expect("marker log should be readable");
    assert!(first.contains("\"audit-key-rotate\""), "{first}");
    assert!(first.contains("\"from\":null,\"to\":\"k1\""), "{first}");
    assert!(first.contains("\"entry_hmac_key_id\":\"k1\""), "{first}");

    assert_success(&framed(&[
        "rotate-audit-key",
        "--key-id",
        "k2",
        "--log",
        path(&log),
    ]));
    let text = fs::read_to_string(&log).expect("marker log should be readable");
    assert!(text.contains("\"from\":\"k1\",\"to\":\"k2\""), "{text}");
    assert!(text.contains("\"entry_hmac_key_id\":\"k2\""), "{text}");

    let verify = framed(&["verify-audit", path(&log)]);
    assert_success(&verify);
    let stdout = String::from_utf8_lossy(&verify.stdout);
    assert!(stdout.contains("integrity-keyed"), "{stdout}");
    assert!(stdout.contains("keyed entries: 2"), "{stdout}");

    let _ = fs::remove_dir_all(root);
}

fn path(path: &Path) -> &str {
    path.to_str().expect("test paths should be UTF-8")
}

/// T5: `qa reproducibility` must be shown against a REAL second scan of the
/// same media (two independent cases), not a case compared with itself - and
/// it must refuse when the second case's media differ.
#[test]
fn qa_reproducibility_compares_two_independent_scans() {
    let root = unique_temp_dir("qa-repro-two-scans");
    let media_dir = root.join("media");
    fs::create_dir_all(&media_dir).unwrap();
    fs::write(media_dir.join("a.mp4"), b"\0\0\0\x18ftypmp42payload-a").unwrap();
    fs::write(media_dir.join("b.mp4"), b"\0\0\0\x18ftypmp42payload-bb").unwrap();

    let scan_case = |name: &str, media: &Path| -> PathBuf {
        let case_dir = root.join(name);
        assert_success(&run(&["init-case", path(&case_dir), "--title", "Repro"]));
        assert_success(&run(&[
            "scan-folder",
            path(&case_dir),
            path(media),
            "--no-ffprobe",
        ]));
        case_dir
    };
    let first = scan_case("case-first", &media_dir);
    let second = scan_case("case-second", &media_dir);
    assert_ne!(first, second, "must be two distinct case directories");

    let out = run(&["qa", "reproducibility", path(&first), path(&second)]);
    assert_success(&out);
    let report = fs::read_to_string(first.join("reports/qa/reproducibility-report.json"))
        .expect("reproducibility report");
    assert!(report.contains("\"passed\": true"), "{report}");
    assert!(
        report.contains("case-second"),
        "right case recorded: {report}"
    );

    // A different media set must NOT compare equal (guards against a
    // comparison that always passes).
    let other_media = root.join("media-other");
    fs::create_dir_all(&other_media).unwrap();
    fs::write(other_media.join("a.mp4"), b"\0\0\0\x18ftypmp42payload-a").unwrap();
    let third = scan_case("case-third", &other_media);
    let out = run(&["qa", "reproducibility", path(&first), path(&third)]);
    assert!(
        !out.status.success(),
        "different scans must fail reproducibility\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let _ = fs::remove_dir_all(root);
}

/// R13-I3: `--key-source env:VAR` takes only key-secret variable names
/// (`FRAMETRACE_*`, `*_SECRET`, `*_KEY`); a loader variable
/// (`DYLD_INSERT_LIBRARIES`, `LD_PRELOAD` — R12's deny-list let the first
/// through) or any other name is refused before anything is read, with
/// the real binary.
#[test]
fn r13_i3_key_source_env_takes_only_key_secret_names() {
    let dir = unique_temp_dir("r13-i3-key-source");
    fs::create_dir_all(dir.join("evidence/logs")).unwrap();
    let case = dir.to_string_lossy().to_string();
    let material = "33".repeat(32);
    // A refused name is never read, so the refused runs leave the
    // variable unset: setting DYLD_INSERT_LIBRARIES / LD_PRELOAD for
    // real would make the loader abort the child before main.
    let run_with = |var: &str, set: bool| {
        let mut cmd = Command::new(frametrace());
        cmd.args(["--key-source", &format!("env:{var}"), "verify-audit", &case])
            .isolated();
        if set {
            cmd.env(var, &material);
        }
        cmd.output().expect("frametrace binary should run")
    };
    for (var, why) in [
        ("DYLD_INSERT_LIBRARIES", "dynamic-loader"),
        ("LD_PRELOAD", "dynamic-loader"),
        ("LD_AUDIT_KEY", "dynamic-loader"),
        ("PATH", "system environment variable"),
        ("MY_TOKEN", "not a key-secret variable name"),
    ] {
        let output = run_with(var, false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{var}: {stderr}");
        assert!(
            stderr.contains("refused") && stderr.contains(why),
            "{var}: {stderr}"
        );
    }
    for var in [
        "FRAMETRACE_R13_TEST_MATERIAL",
        "R13_AUDIT_KEY",
        "R13_SECRET",
    ] {
        let output = run_with(var, true);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("refused"), "{var}: {stderr}");
    }
    let _ = fs::remove_dir_all(&dir);
}
