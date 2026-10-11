use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Sorted `dir/*.<ext>` files, each registered for rebuild. Numeric
/// filename prefixes define the concatenation order — both bundles are
/// one classic script, so declaration order is load-bearing.
fn sorted_parts(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut parts: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|_| panic!("{} missing", dir.display()))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension().is_some_and(|e| e == ext)).then_some(path)
        })
        .collect();
    parts.sort();
    for path in &parts {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    parts
}

/// Repo-relative display name with `/` separators on every platform
/// (Windows `read_dir` paths use `\\`), used for banners and sourceURL.
fn display_name(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Each part is preceded by a `/* ==== path ==== */` banner so a line in a
/// browser error can be traced to its source file, and the bundle ends
/// with a `//# sourceURL=` naming the bundle. Numeric `NN-` prefixes must
/// be unique so the load order is never decided by an accidental tie.
/// scripts/lib/app-bundle.mjs and scripts/check-js.mjs mirror this format.
fn concat(parts: &[PathBuf], bundle_name: &str, ext: &str) -> String {
    let mut seen = std::collections::BTreeSet::new();
    for path in parts {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if let Some((prefix, _)) = name.split_once('-')
            && prefix.chars().all(|c| c.is_ascii_digit())
        {
            assert!(
                seen.insert(prefix.to_string()),
                "duplicate numeric prefix {prefix} among {bundle_name} parts (at {name})"
            );
        }
    }
    let mut bundle = String::new();
    for path in parts {
        bundle.push_str(&format!("/* ==== {} ==== */\n", display_name(path)));
        bundle.push_str(
            &fs::read_to_string(path).unwrap_or_else(|_| panic!("{} unreadable", path.display())),
        );
        if !bundle.ends_with('\n') {
            bundle.push('\n');
        }
    }
    if ext == "js" {
        bundle.push_str(&format!("//# sourceURL={bundle_name}\n"));
    }
    bundle
}

/// Bundles `assets/viewer/*.js` into one classic script for the
/// single-file evidence viewer. Source lives split per concern, but the
/// deliverable must stay a plain concatenation — ES module imports do
/// not resolve under file:// where the exported review page is opened.
fn viewer_bundle(out_dir: &Path) {
    let viewer_dir = Path::new("assets/viewer");
    println!("cargo:rerun-if-changed={}", viewer_dir.display());
    let bundle = concat(
        &sorted_parts(viewer_dir, "js"),
        "assets/viewer/bundle.js",
        "js",
    );
    fs::write(out_dir.join("evidence_viewer.js"), bundle).expect("failed to write viewer bundle");
}

/// STR-01: the workstation shell (`GET /`) is authored as markup in
/// `assets/examiner_app.html` plus `assets/app/*.css` / `assets/app/*.js`,
/// and served as ONE self-contained page: the `<!-- @bundle:app.css -->`
/// and `<!-- @bundle:app.js -->` markers are replaced by inline
/// `<style>` / `<script>` blocks (no extra routes, works offline).
fn app_bundle(out_dir: &Path) {
    let app_dir = Path::new("assets/app");
    let template_path = Path::new("assets/examiner_app.html");
    println!("cargo:rerun-if-changed={}", app_dir.display());
    println!("cargo:rerun-if-changed={}", template_path.display());
    let template = fs::read_to_string(template_path).expect("assets/examiner_app.html missing");
    let css = concat(
        &sorted_parts(app_dir, "css"),
        "assets/app/bundle.css",
        "css",
    );
    let js = concat(&sorted_parts(app_dir, "js"), "assets/app/bundle.js", "js");
    assert!(
        !js.to_ascii_lowercase().contains("</script"),
        "assets/app/*.js must not contain a literal </script"
    );
    assert!(
        !css.to_ascii_lowercase().contains("</style"),
        "assets/app/*.css must not contain a literal </style"
    );
    let mut page = template;
    for (marker, block) in [
        (
            "<!-- @bundle:app.css -->",
            format!("<style>\n{css}</style>"),
        ),
        (
            "<!-- @bundle:app.js -->",
            format!("<script>\n{js}</script>"),
        ),
    ] {
        assert_eq!(
            page.matches(marker).count(),
            1,
            "examiner_app.html must contain exactly one {marker}"
        );
        page = page.replacen(marker, &block, 1);
    }
    fs::write(out_dir.join("examiner_app.html"), page).expect("failed to write app bundle");
}

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    viewer_bundle(&out_dir);
    app_bundle(&out_dir);
}
