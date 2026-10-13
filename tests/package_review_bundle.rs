//! Packaging carries the standalone viewer's sibling data (I1).

use frametrace::package::{CheckStatus, package_case, verify_package};
use std::fs;
use std::path::{Path, PathBuf};

fn case_fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "frametrace-pkgbundle-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let case_dir = root.join("case");
    fs::create_dir_all(case_dir.join("db")).unwrap();
    fs::write(case_dir.join("case.json"), b"{}").unwrap();
    fs::write(case_dir.join("db/video_index.json"), b"{}").unwrap();
    fs::write(case_dir.join("db/videos.jsonl"), b"").unwrap();
    fs::write(case_dir.join("db/video_paths.tsv"), b"id\tsource_path\n").unwrap();
    frametrace::case_db::upsert_review_marks(&case_dir, &[]).unwrap();
    root
}

fn write_review(case_dir: &Path, files: &[&str]) {
    fs::create_dir_all(case_dir.join("review")).unwrap();
    for name in files {
        fs::write(
            case_dir.join("review").join(name),
            format!("content of {name}"),
        )
        .unwrap();
    }
}

#[test]
fn package_includes_data_bundle_and_carve_report_when_present() {
    let root = case_fixture("include");
    let case_dir = root.join("case");
    write_review(
        &case_dir,
        &[
            "evidence-viewer.html",
            "data-bundle.js",
            "carve-report.html",
        ],
    );
    let output = root.join("package");
    package_case(&case_dir, Some(&output)).unwrap();
    let manifest = fs::read_to_string(output.join("manifest.sha256")).unwrap();
    for rel in ["review/data-bundle.js", "review/carve-report.html"] {
        let line = manifest
            .lines()
            .find(|line| line.ends_with(&format!("  {rel}")))
            .unwrap_or_else(|| panic!("{rel} not in manifest.sha256"));
        let actual = frametrace::audit::digest_file(&output.join(rel)).unwrap();
        assert_eq!(&line[..64], actual, "{rel} hash");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_refuses_viewer_without_data_bundle() {
    let root = case_fixture("refuse");
    let case_dir = root.join("case");
    write_review(&case_dir, &["evidence-viewer.html"]);
    let output = root.join("package");
    let err = package_case(&case_dir, Some(&output)).unwrap_err();
    assert!(err.contains("make-review"), "{err}");
    assert!(!output.exists());
    fs::remove_dir_all(root).unwrap();
}

/// An older package (viewer without its bundle) is NOT VERIFIED, not
/// FAIL: the rest of the package still verifies (contract §10.2).
#[test]
fn verify_package_not_verified_when_viewer_lacks_data_bundle() {
    let root = case_fixture("verify-fail");
    let case_dir = root.join("case");
    write_review(&case_dir, &["evidence-viewer.html", "data-bundle.js"]);
    let output = root.join("package");
    package_case(&case_dir, Some(&output)).unwrap();
    // Remove the bundle from the package and from manifest.sha256 so only
    // the viewer-sibling check can object.
    fs::remove_file(output.join("review/data-bundle.js")).unwrap();
    let manifest_path = output.join("manifest.sha256");
    let kept = fs::read_to_string(&manifest_path)
        .unwrap()
        .lines()
        .filter(|line| !line.ends_with("  review/data-bundle.js"))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    fs::write(&manifest_path, kept).unwrap();
    let result = verify_package(&output).unwrap();
    assert!(
        result
            .checks
            .iter()
            .any(|c| c.subject == "review/evidence-viewer.html"
                && c.status == CheckStatus::NotVerified
                && c.detail.contains("data-bundle.js")),
        "{:?}",
        result.checks
    );
    assert!(
        result
            .failures()
            .all(|c| c.subject != "review/evidence-viewer.html"),
        "{:?}",
        result.checks
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verify_package_passes_complete_review_dir() {
    let root = case_fixture("verify-pass");
    let case_dir = root.join("case");
    write_review(
        &case_dir,
        &[
            "evidence-viewer.html",
            "data-bundle.js",
            "carve-report.html",
        ],
    );
    let output = root.join("package");
    package_case(&case_dir, Some(&output)).unwrap();
    let result = verify_package(&output).unwrap();
    assert!(result.passed(), "{:?}", result.checks);
    assert!(
        result
            .checks
            .iter()
            .any(|c| c.subject == "review/evidence-viewer.html" && c.status == CheckStatus::Pass)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_carve_report_with_carve_log_is_not_verified() {
    let root = case_fixture("carve");
    let case_dir = root.join("case");
    frametrace::audit::append_chained_jsonl_keyed(
        &case_dir.join("artifacts/carved/carve-log.jsonl"),
        r#"{"kind":"carve"}"#,
        None,
    )
    .unwrap();
    let output = root.join("package");
    package_case(&case_dir, Some(&output)).unwrap();
    let result = verify_package(&output).unwrap();
    assert!(
        result.checks.iter().any(
            |c| c.subject == "review/carve-report.html" && c.status == CheckStatus::NotVerified
        )
    );
    fs::remove_dir_all(root).unwrap();
}
