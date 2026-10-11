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
/// `00-host.js` (window.FTViewer) sorts first; the hosted workstation page
/// inlines the very same bundle (docs/ui-rebuild-contract.md §3).
fn viewer_js() -> String {
    let viewer_dir = Path::new("assets/viewer");
    println!("cargo:rerun-if-changed={}", viewer_dir.display());
    let bundle = concat(
        &sorted_parts(viewer_dir, "js"),
        "assets/viewer/bundle.js",
        "js",
    );
    assert!(
        !bundle.to_ascii_lowercase().contains("</script"),
        "assets/viewer/*.js must not contain a literal </script"
    );
    bundle
}

/// The shared viewer markup (docs/ui-rebuild-contract.md §1.2): ONE
/// fragment, `assets/viewer/viewer.body.html`, rooted at
/// `<div id="ftViewer" class="ft-viewer" data-ft-viewer="2">`. Every page
/// that shows the viewer carries a single include marker that is replaced
/// by it, so the standalone/exported viewer and the viewer hosted inside
/// the workstation shell can never drift apart.
const VIEWER_INCLUDE: &str = "<!-- @include:viewer.body -->";

fn viewer_fragment() -> String {
    let path = PathBuf::from("assets/viewer/viewer.body.html");
    println!("cargo:rerun-if-changed={}", path.display());
    let fragment = fs::read_to_string(&path).expect("assets/viewer/viewer.body.html missing");
    assert!(
        fragment.starts_with(r#"<div id="ftViewer" class="ft-viewer" data-ft-viewer="2">"#),
        "viewer.body.html must start with the #ftViewer root"
    );
    assert_eq!(
        fragment.matches(r#"id="ftViewer""#).count(),
        1,
        "viewer.body.html must contain exactly one #ftViewer root"
    );
    // The standalone template is filled by html_report::fill_template; the
    // fragment must never carry one of its slot tokens or another marker.
    for token in [
        "__CSS__",
        "__DATA__",
        "__JS__",
        "<!-- @include:",
        "<!-- @bundle:",
    ] {
        assert!(
            !fragment.contains(token),
            "viewer.body.html must not contain {token}"
        );
    }
    fragment.trim_end().to_string()
}

/// Replaces the include marker. `required` pages must carry exactly one;
/// optional pages at most one. Returns whether the fragment went in.
fn include_viewer_body(page: String, template_name: &str, required: bool) -> (String, bool) {
    match page.matches(VIEWER_INCLUDE).count() {
        0 if !required => (page, false),
        1 => (page.replacen(VIEWER_INCLUDE, &viewer_fragment(), 1), true),
        n => panic!(
            "{template_name} must contain {} {VIEWER_INCLUDE} (found {n})",
            if required {
                "exactly one"
            } else {
                "at most one"
            }
        ),
    }
}

/// Shared design tokens (assets/tokens.css) are the FIRST part of both CSS
/// bundles, so the shell and the viewer resolve one palette/type/spacing
/// source. The viewer export is a single file, so its stylesheet is
/// materialised here (tokens + assets/evidence_viewer.css) for include_str.
fn tokens_path() -> PathBuf {
    let path = PathBuf::from("assets/tokens.css");
    println!("cargo:rerun-if-changed={}", path.display());
    path
}

/// Shared UI kit (assets/kit.css: buttons, inputs, kbd, icon glyphs) is the
/// SECOND part of both CSS bundles, right after the tokens, so one
/// definition styles every control on every page
/// (docs/ui-rebuild-contract.md §5.4).
fn kit_path() -> PathBuf {
    let path = PathBuf::from("assets/kit.css");
    println!("cargo:rerun-if-changed={}", path.display());
    path
}

/// Offline inline SVG icon sprite (assets/icons.svg). A page template may
/// carry at most one `<!-- @bundle:icons.svg -->` marker; when present it
/// is replaced by the sprite so icons work under file:// as well
/// (docs/ui-rebuild-contract.md §5.5).
fn inline_icons(page: String, template_name: &str) -> String {
    const MARKER: &str = "<!-- @bundle:icons.svg -->";
    let sprite_path = PathBuf::from("assets/icons.svg");
    println!("cargo:rerun-if-changed={}", sprite_path.display());
    match page.matches(MARKER).count() {
        0 => page,
        1 => {
            let sprite = fs::read_to_string(&sprite_path).expect("assets/icons.svg unreadable");
            page.replacen(MARKER, sprite.trim_end(), 1)
        }
        n => panic!("{template_name} must contain at most one {MARKER} (found {n})"),
    }
}

/// The viewer's own stylesheets (scoped under `.ft-viewer`): the page
/// stylesheet, then the control styles (action bar, player toolbar, menus,
/// toasts) which refine it.
fn viewer_own_css() -> Vec<PathBuf> {
    let viewer_css = PathBuf::from("assets/evidence_viewer.css");
    let controls_css = PathBuf::from("assets/viewer/controls.css");
    println!("cargo:rerun-if-changed={}", viewer_css.display());
    println!("cargo:rerun-if-changed={}", controls_css.display());
    vec![viewer_css, controls_css]
}

fn viewer_css_bundle(out_dir: &Path) {
    // Order matters: tokens, the shared kit, then the viewer's own sheets.
    let mut parts = vec![tokens_path(), kit_path()];
    parts.extend(viewer_own_css());
    let css = concat(&parts, "assets/viewer/bundle.css", "css");
    fs::write(out_dir.join("evidence_viewer.css"), css).expect("failed to write viewer css");
}

/// The standalone/exported viewer template, materialised in OUT_DIR with
/// the fragment and the icon sprite inlined; html_report.rs include_str!s
/// it and fills the `__CSS__` / `__DATA__` / `__JS__` slots per case.
fn viewer_template(out_dir: &Path) {
    let template_path = Path::new("assets/evidence_viewer.html");
    println!("cargo:rerun-if-changed={}", template_path.display());
    let template = fs::read_to_string(template_path).expect("assets/evidence_viewer.html missing");
    for slot in ["__CSS__", "__DATA__", "__JS__"] {
        assert_eq!(
            template.matches(slot).count(),
            1,
            "evidence_viewer.html must contain exactly one {slot}"
        );
    }
    let (page, _) = include_viewer_body(template, "evidence_viewer.html", true);
    let page = inline_icons(page, "evidence_viewer.html");
    for slot in ["__CSS__", "__DATA__", "__JS__"] {
        assert_eq!(
            page.matches(slot).count(),
            1,
            "the icon sprite must not contain {slot}"
        );
    }
    fs::write(out_dir.join("evidence_viewer.html"), page).expect("failed to write viewer template");
}

/// One shell page flavour: template + token sheet (+ kit) + `app/` parts.
struct ShellPage<'a> {
    template: &'a Path,
    tokens: PathBuf,
    kit: Option<PathBuf>,
    app_dir: &'a Path,
    bundle_dir: &'a str,
}

/// STR-01: the workstation shell (`GET /`) is authored as markup in
/// `assets/examiner_app.html` plus `assets/app/*.css` / `assets/app/*.js`,
/// and served as ONE self-contained page: the `<!-- @bundle:app.css -->`
/// and `<!-- @bundle:app.js -->` markers are replaced by inline
/// `<style>` / `<script>` blocks (no extra routes, works offline).
///
/// Hosted viewer (docs/ui-rebuild-contract.md §3): when the template carries
/// the viewer include marker, the viewer fragment goes there, the viewer's
/// own stylesheets join the shell CSS block (tokens + kit stay bundled
/// once), and the viewer bundle is inlined as its own `<script>` right
/// before the shell script, so `window.FTViewer` exists when the shell
/// boots. Without the marker the page is exactly the shell bundle.
fn shell_page(spec: &ShellPage) -> String {
    println!("cargo:rerun-if-changed={}", spec.app_dir.display());
    println!("cargo:rerun-if-changed={}", spec.template.display());
    let template = fs::read_to_string(spec.template)
        .unwrap_or_else(|_| panic!("{} missing", spec.template.display()));
    let name = display_name(spec.template);
    let (template, hosted) = include_viewer_body(template, &name, false);
    let mut css_parts = vec![spec.tokens.clone()];
    css_parts.extend(spec.kit.clone());
    css_parts.extend(sorted_parts(spec.app_dir, "css"));
    if hosted {
        css_parts.extend(viewer_own_css());
    }
    let css = concat(
        &css_parts,
        &format!("{}/bundle.css", spec.bundle_dir),
        "css",
    );
    let js = concat(
        &sorted_parts(spec.app_dir, "js"),
        &format!("{}/bundle.js", spec.bundle_dir),
        "js",
    );
    assert!(
        !js.to_ascii_lowercase().contains("</script"),
        "{}/*.js must not contain a literal </script",
        spec.bundle_dir
    );
    assert!(
        !css.to_ascii_lowercase().contains("</style"),
        "{}/*.css must not contain a literal </style",
        spec.bundle_dir
    );
    let script = if hosted {
        format!(
            "<script>\n{}</script>\n<script>\n{js}</script>",
            viewer_js()
        )
    } else {
        format!("<script>\n{js}</script>")
    };
    let mut page = template;
    for (marker, block) in [
        (
            "<!-- @bundle:app.css -->",
            format!("<style>\n{css}</style>"),
        ),
        ("<!-- @bundle:app.js -->", script),
    ] {
        assert_eq!(
            page.matches(marker).count(),
            1,
            "{name} must contain exactly one {marker}"
        );
        page = page.replacen(marker, &block, 1);
    }
    inline_icons(page, &name)
}

fn app_bundle(out_dir: &Path) {
    let page = shell_page(&ShellPage {
        template: Path::new("assets/examiner_app.html"),
        tokens: tokens_path(),
        kit: Some(kit_path()),
        app_dir: Path::new("assets/app"),
        bundle_dir: "assets/app",
    });
    fs::write(out_dir.join("examiner_app.html"), page).expect("failed to write app bundle");
}

/// FRAMETRACE_UI=legacy rollback (docs/ui-rebuild-contract.md §9.3): the
/// pre-rebuild shell (465e7db snapshot under assets/legacy/, never edited)
/// bundled with the same rules. It embeds the viewer through its iframe
/// (`/review/evidence-viewer.html?embed=1`), which the standalone viewer
/// still honours. Remove together with assets/legacy/ one release later.
fn legacy_bundle(out_dir: &Path) {
    let tokens = PathBuf::from("assets/legacy/tokens.css");
    println!("cargo:rerun-if-changed={}", tokens.display());
    let page = shell_page(&ShellPage {
        template: Path::new("assets/legacy/examiner_app.html"),
        tokens,
        kit: None,
        app_dir: Path::new("assets/legacy/app"),
        bundle_dir: "assets/legacy/app",
    });
    fs::write(out_dir.join("examiner_app_legacy.html"), page)
        .expect("failed to write legacy app bundle");
}

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("evidence_viewer.js"), viewer_js())
        .expect("failed to write viewer bundle");
    viewer_css_bundle(&out_dir);
    viewer_template(&out_dir);
    app_bundle(&out_dir);
    legacy_bundle(&out_dir);
}
