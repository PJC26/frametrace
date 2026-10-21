//! On-demand pip tools (deepfake-lens): a private venv under
//! `<exe dir>/tools/python/<name>`, filled by
//! `pip install --require-hashes --no-deps --only-binary :all:` from a
//! compiled-in, fully hash-pinned requirements file, index pinned to
//! PyPI. The interpreter comes from `tool_policy` (absolute path only).

use super::*;
use std::ffi::OsString;
use std::process::Command;

/// The only index a pip tool installs from (no extra index, ever).
pub(crate) const PINNED_INDEX_URL: &str = "https://pypi.org/simple";

const VENV_TIMEOUT_SECS: u64 = 300;
const PIP_TIMEOUT_SECS: u64 = 1800;

pub(crate) fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

/// The console script `name` inside `venv`.
pub(crate) fn venv_script(venv: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join(format!("{name}.exe"))
    } else {
        venv.join("bin").join(name)
    }
}

fn is_hash_option(token: &str) -> bool {
    token
        .strip_prefix("--hash=sha256:")
        .is_some_and(super::is_sha256_hex)
}

fn is_pinned_spec(token: &str) -> Option<(&str, &str)> {
    let (name, version) = token.split_once("==")?;
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    let version_ok = !version.is_empty()
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'!' | b'_' | b'-'));
    (name_ok && version_ok).then_some((name, version))
}

fn normalize(name: &str) -> String {
    name.to_ascii_lowercase().replace(['_', '.'], "-")
}

/// Every requirement is `name==version` followed only by
/// `--hash=sha256:<hex>` options; no other pip option (`-i`,
/// `--extra-index-url`, `-e`, `-r`, `--find-links`, URLs, paths, markers)
/// is accepted, and the tool's own package must be pinned to its manifest
/// version.
pub(crate) fn check_requirements(text: &str, tool: &ToolEntry) -> Result<(), String> {
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let mut saw_package = false;
    let mut count = 0usize;
    for line in joined.lines() {
        let line = line.split(" #").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let spec = tokens.next().unwrap_or("");
        let Some((name, version)) = is_pinned_spec(spec) else {
            return Err(format!("requirements: not an exact pin: {spec}"));
        };
        let hashes: Vec<&str> = tokens.collect();
        if hashes.is_empty() || !hashes.iter().all(|t| is_hash_option(t)) {
            return Err(format!(
                "requirements: {spec} must carry only --hash=sha256:<digest> options"
            ));
        }
        if let Some(package) = &tool.package
            && normalize(name) == normalize(package)
        {
            if tool.version.as_deref() != Some(version) {
                return Err(format!(
                    "requirements: {spec} does not match the manifest version"
                ));
            }
            saw_package = true;
        }
        count += 1;
    }
    if count == 0 || !saw_package {
        return Err("requirements: the tool's own package is not pinned".into());
    }
    Ok(())
}

/// `python -I -m pip install ...` argv inside the venv (T1/T2: hashes
/// required, no dependency resolution, wheels only, PyPI only, no user
/// config or environment options).
pub(crate) fn pip_install_args(requirements: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-I",
        "-m",
        "pip",
        "install",
        "--isolated",
        "--no-input",
        "--disable-pip-version-check",
        "--no-cache-dir",
        "--require-hashes",
        "--no-deps",
        "--only-binary",
        ":all:",
        "--index-url",
        PINNED_INDEX_URL,
        "-r",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(requirements.as_os_str().to_os_string());
    args
}

/// Interpreter operations, faked by the unit tests.
pub(crate) trait PythonOps {
    fn create_venv(&self, venv: &Path) -> Result<(), String>;
    fn pip_install(&self, venv: &Path, requirements: &Path) -> Result<(), String>;
}

pub(crate) struct RealPython {
    python: PathBuf,
}

/// Refusal when no Python interpreter resolves.
pub const PYTHON_REQUIRED: &str = "Python 필요: 이 도구는 Python 3로 설치됩니다 — https://www.python.org/downloads/ 에서 Python을 설치(PATH 등록)한 뒤 다시 시도하십시오";

impl RealPython {
    pub(crate) fn resolve() -> Result<Self, String> {
        Self::from_resolution(crate::tool_policy::resolve_tool_binary(
            "python",
            &["python", "python3", "py"],
        ))
    }

    /// No interpreter → the "Python 필요" hint; nothing is installed.
    pub(crate) fn from_resolution(resolved: Result<String, String>) -> Result<Self, String> {
        let python = resolved.map_err(|err| format!("{PYTHON_REQUIRED} ({err})"))?;
        Ok(Self {
            python: PathBuf::from(python),
        })
    }
}

/// The refusal when a program runs from the venv to be replaced (audited
/// with code `busy`).
pub(crate) fn venv_in_use(target: &Path) -> String {
    format!(
        "{}{} is in use (a program runs from it) — close it and try again; nothing was changed",
        super::BUSY_PREFIX,
        target.display()
    )
}

fn run_python(command: &mut Command, secs: u64, what: &str) -> Result<(), String> {
    command
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONHOME")
        .env("PYTHONNOUSERSITE", "1");
    let output = crate::util::run_with_timeout(command, Some(secs))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = stderr.lines().rev().take(8).collect();
    Err(format!(
        "{what} failed ({}): {}",
        output.status,
        tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
    ))
}

impl PythonOps for RealPython {
    fn create_venv(&self, venv: &Path) -> Result<(), String> {
        let mut command = Command::new(&self.python);
        command.args(["-I", "-m", "venv"]).arg(venv);
        run_python(&mut command, VENV_TIMEOUT_SECS, "python -m venv")
    }
    fn pip_install(&self, venv: &Path, requirements: &Path) -> Result<(), String> {
        let mut command = Command::new(venv_python(venv));
        command.args(pip_install_args(requirements));
        run_python(&mut command, PIP_TIMEOUT_SECS, "pip install")
    }
}

/// Builds the private venv for `tool`. A venv is not relocatable, so it
/// is built at its final path; an existing env is moved aside first and
/// restored when anything fails (the new, partial env is removed; see
/// `rollback::undo_venv` for when that cannot be finished).
pub(crate) fn install_pip(
    tool: &ToolEntry,
    requirements: &str,
    dirs: &InstallDirs,
    python: &dyn PythonOps,
    progress: &Progress,
) -> Result<InstallOutcome, String> {
    check_requirements(requirements, tool)?;
    let script_name = tool.provides.first().ok_or("tool provides nothing")?;
    let target = dirs.python_env(&tool.name);
    let parent = target.parent().ok_or("bad venv path")?.to_path_buf();
    // A program running from the live venv (the server's deepfake-lens,
    // whose busy lease the CLI cannot see) does not stop the rename below
    // on Windows — the env would be rebuilt underneath it. Refuse instead;
    // nothing has been touched yet.
    if super::image_in_use_under(&target) {
        return Err(venv_in_use(&target));
    }
    let staging = StagingDir::create(&dirs.tools_dir, ".staging-")?;
    let req_path = staging.path().join("requirements.txt");
    std::fs::write(&req_path, requirements)
        .map_err(|err| format!("cannot write requirements: {err}"))?;
    std::fs::create_dir_all(&parent)
        .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    // From here the venv is rebuilt in place: not cancellable (a cancel
    // that won the race stops the install before anything moved).
    progress.enter_non_cancellable(Stage::Venv)?;
    let backup = if std::fs::symlink_metadata(&target).is_ok() {
        let backup = parent.join(format!(".{}.old-{}", tool.name, random_hex()));
        std::fs::rename(&target, &backup).map_err(|err| {
            format!(
                "cannot move the existing {} aside (in use?): {err}",
                target.display()
            )
        })?;
        Some(backup)
    } else {
        None
    };
    let built = (|| {
        python.create_venv(&target)?;
        progress.set_stage(Stage::Pip);
        python.pip_install(&target, &req_path)?;
        let exe = venv_script(&target, script_name);
        if !exe.is_file() {
            return Err(format!(
                "pip finished but {} is missing — not installed",
                exe.display()
            ));
        }
        Ok(exe)
    })();
    match built {
        Ok(exe) => {
            // The replaced venv, when it cannot be removed (a process still
            // runs from it), is reported like a leftover staging dir — and
            // left whole, never half-deleted under a running process.
            let old_left = backup.and_then(|backup| super::remove_dir_unless_in_use(&backup));
            let staging_left = staging.close();
            Ok(InstallOutcome {
                installed_files: vec![exe],
                audit_warning: None,
                staging_left: old_left.or(staging_left),
            })
        }
        // Fix round 13 M: retried, and never silent — a previous venv that
        // cannot be put back is kept where no sweep removes it.
        Err(err) => Err(super::rollback::undo_venv(
            err,
            &tool.name,
            &target,
            backup.as_deref(),
            &parent,
        )),
    }
}
