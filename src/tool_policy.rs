use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub fn resolve_tool_binary(input: &str, allowed_names: &[&str]) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "tool binary must not be empty; allowed tools: {}",
            allowed_names.join(", ")
        ));
    }

    if is_allowed_tool_name(trimmed, allowed_names) {
        return resolve_bare_tool_path(trimmed, allowed_names).ok_or_else(|| {
            format!(
                "tool '{trimmed}' not found in PATH or the tools/bin directory; install it and retry (a bare name is never executed directly to avoid planted-binary search paths)"
            )
        });
    }

    if !looks_like_path(trimmed) {
        return Err(format!(
            "unsupported tool binary '{trimmed}'; allowed tools: {}",
            allowed_names.join(", ")
        ));
    }

    // Checks run on the display form so argv[0], error strings and audit
    // text never carry the Windows `\\?\` verbatim prefix. The allowlist
    // check is by file name, which the prefix strip never changes.
    let canonical = crate::util::canonicalize_display(Path::new(trimmed))
        .map_err(|err| format!("failed to canonicalize tool binary '{trimmed}': {err}"))?;
    if !canonical.is_file() {
        return Err(format!(
            "tool binary is not a file: {}",
            canonical.display()
        ));
    }
    let file_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "tool binary has an invalid file name: {}",
                canonical.display()
            )
        })?;
    if !is_allowed_tool_name(file_name, allowed_names) {
        return Err(format!(
            "unsupported tool binary '{}'; allowed tools: {}",
            canonical.display(),
            allowed_names.join(", ")
        ));
    }

    Ok(canonical.to_string_lossy().to_string())
}

pub fn command_version(input: &str, allowed_names: &[&str], version_arg: &str) -> String {
    let binary = match resolve_tool_binary(input, allowed_names) {
        Ok(binary) => binary,
        Err(err) => return format!("unavailable: {err}"),
    };
    // P7: a version probe is bounded — a wedged tool must not hang the
    // command (or the HTTP handler) that asked for its version.
    let secs = crate::util::version_probe_timeout_secs();
    let result =
        crate::util::run_with_timeout_detailed(Command::new(&binary).arg(version_arg), Some(secs));
    describe_version_probe(&binary, secs, result)
}

/// Provenance text for one version probe. R6: a probe that ran out of
/// time says exactly that — the tool resolved and started, its version is
/// simply not known — and is never recorded as "unavailable", which would
/// claim as fact that the tool was missing or broken.
pub fn describe_version_probe(
    binary: &str,
    secs: u64,
    result: Result<std::process::Output, crate::util::RunError>,
) -> String {
    match result {
        Err(crate::util::RunError::TimedOut(_)) => format!(
            "version probe timed out after {secs}s ({binary} started but did not report its version; version not recorded)"
        ),
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            stdout
                .lines()
                .chain(stderr.lines())
                .find(|line| !line.trim().is_empty())
                .unwrap_or("unknown")
                .trim()
                .to_string()
        }
        Ok(output) => format!(
            "unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(err) => format!("unavailable: {}", err.message()),
    }
}

pub fn require_case_output_path(
    case_dir: &Path,
    output_path: &Path,
    label: &str,
) -> Result<(), String> {
    let case_root = case_dir
        .canonicalize()
        .map_err(|err| format!("failed to canonicalize case directory: {err}"))?;
    let absolute_output = lexical_absolute_path(output_path)
        .map_err(|err| format!("failed to resolve {label} output path: {err}"))?;
    let existing_parent = nearest_existing_parent(
        absolute_output
            .parent()
            .ok_or_else(|| format!("{label} output path has no parent"))?,
    );
    let canonical_parent = existing_parent
        .canonicalize()
        .map_err(|err| format!("failed to canonicalize {label} output parent: {err}"))?;
    if !canonical_parent.starts_with(&case_root) {
        return Err(format!(
            "{label} output must be inside the case directory {}; parent resolves to {}",
            crate::util::strip_windows_extended_prefix(&case_root).display(),
            crate::util::strip_windows_extended_prefix(&canonical_parent).display()
        ));
    }

    // The parent check only covers existing ancestors. An already-existing
    // leaf is a separate escape vector: a symlink leaf resolves outside the
    // case root even though its parent directory is inside. Reject symlink
    // leaves outright and re-verify containment for any existing target.
    match std::fs::symlink_metadata(&absolute_output) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "{label} output path is a symlink: {}",
                    absolute_output.display()
                ));
            }
            let canonical_leaf = absolute_output
                .canonicalize()
                .map_err(|err| format!("failed to canonicalize {label} output: {err}"))?;
            if !canonical_leaf.starts_with(&case_root) {
                return Err(format!(
                    "{label} output must be inside the case directory {}; resolves to {}",
                    crate::util::strip_windows_extended_prefix(&case_root).display(),
                    crate::util::strip_windows_extended_prefix(&canonical_leaf).display()
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("failed to inspect {label} output: {err}")),
    }

    Ok(())
}

pub fn require_case_report_path(
    case_dir: &Path,
    output_path: &Path,
    default_relative: &str,
    label: &str,
) -> Result<(PathBuf, bool), String> {
    require_case_output_path(case_dir, output_path, label)?;
    let resolve = || -> std::io::Result<(PathBuf, PathBuf)> {
        let root = case_dir.canonicalize()?;
        let absolute = lexical_absolute_path(output_path)?;
        let ancestor = nearest_existing_parent(absolute.parent().unwrap());
        let resolved = ancestor
            .canonicalize()?
            .join(absolute.strip_prefix(&ancestor).unwrap());
        Ok((root, resolved))
    };
    let (root, resolved) =
        resolve().map_err(|err| format!("failed to resolve {label} output: {err}"))?;
    let relative = resolved
        .strip_prefix(&root)
        .map_err(|_| format!("{label} output must be inside the case directory"))?;
    let is_default = relative == Path::new(default_relative);
    let protected = [
        "case.json",
        "db/case.db",
        "db/videos.jsonl",
        "db/video_paths.tsv",
        "db/video_index.json",
    ]
    .iter()
    .any(|path| relative == Path::new(path))
        || ["db/scan_runs", "evidence/logs", "review", "reports"]
            .iter()
            .any(|path| relative.starts_with(path));
    if protected && !is_default {
        return Err(format!(
            "{label} output targets a protected case path: {}",
            output_path.display()
        ));
    }
    match std::fs::symlink_metadata(&resolved) {
        Ok(metadata) if !is_default || !metadata.file_type().is_file() => {
            return Err(format!(
                "{label} output already exists: {}",
                output_path.display()
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("failed to inspect {label} output: {err}")),
    }
    Ok((resolved, is_default))
}

pub fn write_case_report(
    case_dir: &Path,
    output_path: &Path,
    default_relative: &str,
    label: &str,
    text: &str,
) -> Result<(), String> {
    let (resolved, is_default) =
        require_case_report_path(case_dir, output_path, default_relative, label)?;
    let write = || -> std::io::Result<()> {
        if is_default {
            crate::util::write_text_atomic(&resolved, text)
        } else {
            std::fs::create_dir_all(resolved.parent().unwrap())?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&resolved)?;
            std::io::Write::write_all(&mut file, text.as_bytes())
        }
    };
    write().map_err(|err| {
        format!(
            "failed to write {label} output {}: {err}",
            output_path.display()
        )
    })
}

fn is_allowed_tool_name(candidate: &str, allowed_names: &[&str]) -> bool {
    allowed_names.iter().any(|allowed| {
        candidate == *allowed
            || candidate
                .strip_suffix(".exe")
                .is_some_and(|without_exe| without_exe == *allowed)
    })
}

/// Bare tool names are resolved against PATH here instead of being handed to
/// `Command::new`, because the OS process search also covers the current
/// directory (Windows `CreateProcess`) and empty/relative PATH entries
/// (Unix `execvp`) — exactly where a planted binary would ride along with
/// evidence media. When the tool cannot be found, `None` is returned: the
/// caller must refuse to spawn rather than fall back to a bare name, which
/// would reintroduce the planting vector this resolver exists to prevent.
fn resolve_bare_tool_path(name: &str, allowed_names: &[&str]) -> Option<String> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    if let Some(path) = find_in_path_dirs(name, allowed_names, &path_var) {
        return Some(path.to_string_lossy().to_string());
    }
    // Portable layout: `<exe dir>/tools/bin` ships optional forensic tools
    // next to the binary so examiners never need admin rights to extend PATH.
    // The directory is part of the trusted install tree, unlike the CWD.
    find_in_tools_bin(name, allowed_names)
}

/// Whether an optional forensic tool has been dropped into the portable
/// `tools/bin` directory (or an installer package dir) next to the executable.
pub fn tools_bin_tool_exists(name: &str) -> bool {
    tools_search_dirs().iter().any(|dir| {
        [name.to_string(), format!("{name}.exe")]
            .iter()
            .any(|candidate| dir.join(candidate).is_file())
    })
}

/// `<exe dir>/tools/bin` (hand-copied portable layout) first, then the
/// per-package dirs the in-app installer writes (`tools/ffmpeg`, ...). The
/// package list is fixed by the catalog — never a directory glob — so only
/// install-tree paths the workstation itself owns are trusted.
fn tools_search_dirs() -> Vec<PathBuf> {
    let Some(tools) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tools")))
    else {
        return Vec::new();
    };
    std::iter::once(tools.join("bin"))
        .chain(
            crate::tool_install::ALL_PACKAGES
                .iter()
                .map(|package| tools.join(package.dir_name())),
        )
        .collect()
}

fn find_in_tools_bin(name: &str, allowed_names: &[&str]) -> Option<String> {
    for dir in tools_search_dirs() {
        for candidate_name in [name.to_string(), format!("{name}.exe")] {
            let candidate = dir.join(&candidate_name);
            let Ok(metadata) = std::fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let Ok(canonical) = crate::util::canonicalize_display(&candidate) else {
                continue;
            };
            let file_name = canonical.file_name().and_then(|n| n.to_str());
            if file_name.map(|n| is_allowed_tool_name(n, allowed_names)) == Some(true) {
                return Some(canonical.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn find_in_path_dirs(name: &str, allowed_names: &[&str], path_var: &str) -> Option<PathBuf> {
    for dir in std::env::split_paths(path_var) {
        // An empty PATH entry conventionally means the current directory.
        if dir.as_os_str().is_empty() {
            continue;
        }
        let Ok(dir) = dir.canonicalize() else {
            continue;
        };
        for candidate_name in [name.to_string(), format!("{name}.exe")] {
            let candidate = dir.join(&candidate_name);
            let Ok(metadata) = std::fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let Ok(canonical) = crate::util::canonicalize_display(&candidate) else {
                continue;
            };
            if let Some(file_name) = canonical.file_name().and_then(|file| file.to_str())
                && is_allowed_tool_name(file_name, allowed_names)
            {
                return Some(canonical);
            }
        }
    }
    None
}

fn looks_like_path(value: &str) -> bool {
    value.contains('/')
        || value.contains('\\')
        || value.starts_with('.')
        || Path::new(value).components().count() > 1
}

fn lexical_absolute_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    let raw = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(normalize_lexically(&raw))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn nearest_existing_parent(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    loop {
        if current.exists() {
            return current;
        }
        if !current.pop() {
            return PathBuf::from(".");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn resolves_bare_tools_from_exe_relative_tools_bin() {
        let exe = std::env::current_exe().unwrap();
        let tools_bin = exe.parent().unwrap().join("tools").join("bin");
        std::fs::create_dir_all(&tools_bin).unwrap();
        let probe = tools_bin.join("frametrace-probe-icat.exe");
        std::fs::write(&probe, b"stub").unwrap();
        let Some(resolved) =
            super::resolve_bare_tool_path("frametrace-probe-icat", &["frametrace-probe-icat"])
        else {
            panic!("tool in tools/bin must resolve");
        };
        assert!(
            resolved.ends_with("frametrace-probe-icat.exe"),
            "unexpected resolution: {resolved}"
        );
        let _ = std::fs::remove_file(&probe);
    }

    #[test]
    fn resolves_tools_installed_into_package_dirs() {
        // Given: a binary the in-app installer placed under tools/sleuthkit/
        let exe = std::env::current_exe().unwrap();
        let package_dir = exe.parent().unwrap().join("tools").join("sleuthkit");
        std::fs::create_dir_all(&package_dir).unwrap();
        let probe = package_dir.join("frametrace-probe-mmls.exe");
        std::fs::write(&probe, b"stub").unwrap();
        // When: the bare name is resolved
        let resolved =
            super::resolve_bare_tool_path("frametrace-probe-mmls", &["frametrace-probe-mmls"]);
        let _ = std::fs::remove_file(&probe);
        // Then: the package-dir copy is found
        let resolved = resolved.expect("tool in tools/<package> must resolve");
        assert!(resolved.ends_with("frametrace-probe-mmls.exe"), "{resolved}");
    }

    #[test]
    fn missing_bare_tool_is_an_error_never_a_bare_name() {
        // A tool absent from PATH and tools/bin must not fall back to a bare
        // name: Command::new would then search the CWD (Windows) or empty
        // PATH entries (Unix) — the planted-binary vector.
        let err = super::resolve_tool_binary(
            "frametrace-definitely-missing-tool",
            &["frametrace-definitely-missing-tool"],
        )
        .unwrap_err();
        assert!(err.contains("not found"), "{err}");
        assert!(!err.starts_with("frametrace-definitely-missing-tool"));
    }

    use super::{find_in_path_dirs, require_case_output_path, resolve_tool_binary};
    use std::fs;
    use std::path::Path;

    #[test]
    fn rejects_unapproved_bare_tool_names() {
        let err = resolve_tool_binary("sh", &["ffprobe"]).unwrap_err();
        assert!(err.contains("unsupported tool binary"));
    }

    #[test]
    fn accepted_bare_tool_names_resolve_to_allowed_files() {
        let resolved = resolve_tool_binary("ffprobe", &["ffprobe"]).unwrap();
        let file_name = Path::new(&resolved)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(resolved.as_str());
        assert!(
            file_name == "ffprobe"
                || file_name
                    .strip_suffix(".exe")
                    .is_some_and(|without_exe| without_exe == "ffprobe"),
            "unexpected resolution: {resolved}"
        );
    }

    #[test]
    fn finds_tool_in_custom_path_dirs_without_current_directory() {
        let base = std::env::temp_dir().join(format!(
            "frametrace-path-search-test-{}",
            std::process::id()
        ));
        let tool_dir = base.join("tools");
        let dot_dir = base.join(".");
        fs::create_dir_all(&tool_dir).unwrap();
        fs::write(tool_dir.join("ffprobe.exe"), b"not really ffprobe").unwrap();

        let custom_path = std::env::join_paths([&tool_dir, &dot_dir])
            .expect("joining test PATH entries")
            .to_string_lossy()
            .to_string();
        let found = find_in_path_dirs("ffprobe", &["ffprobe"], &custom_path).unwrap();
        assert_eq!(found.file_name().unwrap(), "ffprobe.exe");

        // A directory that only holds an unapproved variant must not match.
        let impostor_dir = base.join("impostor");
        fs::create_dir_all(&impostor_dir).unwrap();
        fs::write(impostor_dir.join("ffprobe.bat"), b"impostor").unwrap();
        let impostor_path = impostor_dir.display().to_string();
        assert!(find_in_path_dirs("ffprobe", &["ffprobe"], &impostor_path).is_none());

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn resolved_tool_paths_never_carry_the_verbatim_prefix() {
        let base = std::env::temp_dir().join(format!(
            "frametrace-tool-display-test-{}",
            std::process::id()
        ));
        let tool_dir = base.join("bin");
        fs::create_dir_all(&tool_dir).unwrap();
        let tool = tool_dir.join("frametrace-probe-disp.exe");
        fs::write(&tool, b"stub").unwrap();

        let explicit =
            resolve_tool_binary(&tool.display().to_string(), &["frametrace-probe-disp"]).unwrap();
        let via_path = find_in_path_dirs(
            "frametrace-probe-disp",
            &["frametrace-probe-disp"],
            &tool_dir.display().to_string(),
        )
        .unwrap();
        for resolved in [explicit.clone(), via_path.display().to_string()] {
            assert!(
                !resolved.starts_with(r"\\?\"),
                "verbatim prefix: {resolved}"
            );
            assert!(Path::new(&resolved).is_absolute(), "{resolved}");
            // Still the same file as the input once both are canonicalized.
            assert_eq!(
                Path::new(&resolved).canonicalize().unwrap(),
                tool.canonicalize().unwrap()
            );
        }

        // Allowlist enforcement is unchanged for explicit paths.
        let err = resolve_tool_binary(&tool.display().to_string(), &["ffprobe"]).unwrap_err();
        assert!(err.contains("unsupported tool binary"), "{err}");
        assert!(!err.contains(r"\\?\"), "{err}");

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn containment_errors_show_plain_paths() {
        let base = std::env::temp_dir().join(format!(
            "frametrace-output-policy-display-test-{}",
            std::process::id()
        ));
        let case_dir = base.join("case");
        fs::create_dir_all(&case_dir).unwrap();
        let err = require_case_output_path(&case_dir, &base.join("out.mp4"), "test").unwrap_err();
        assert!(err.contains("inside the case directory"), "{err}");
        assert!(!err.contains(r"\\?\"), "{err}");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_case_output_outside_case_root() {
        let base = std::env::temp_dir().join(format!(
            "frametrace-output-policy-test-{}",
            std::process::id()
        ));
        let case_dir = base.join("case");
        fs::create_dir_all(&case_dir).unwrap();
        let outside = base.join("outside.mp4");
        let err = require_case_output_path(&case_dir, &outside, "test").unwrap_err();
        assert!(err.contains("inside the case directory"));
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn accepts_case_output_inside_case_root() {
        let base = std::env::temp_dir().join(format!(
            "frametrace-output-policy-inside-test-{}",
            std::process::id()
        ));
        let case_dir = base.join("case");
        fs::create_dir_all(&case_dir).unwrap();
        let inside = case_dir.join("artifacts/out.mp4");
        require_case_output_path(&case_dir, &inside, "test").unwrap();
        let _ = fs::remove_dir_all(base);
    }
}
