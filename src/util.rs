use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wait_timeout::ChildExt;

/// Metadata-only probes (ffprobe, ewfinfo) are expected to finish in seconds;
/// anything still running after two minutes is almost certainly wedged on
/// hostile media, which a forensics tool must survive.
pub const PROBE_TIMEOUT_SECS: u64 = 120;

/// Default bound of a presence/version probe (`tool -V`, `ffmpeg
/// -version`). Usually well under a second, but the first launch of a
/// tool on a workstation can take tens of seconds (antivirus scanning a
/// freshly unpacked portable ffmpeg, a cold network profile), so R6 raised
/// it from 15 s; [`TOOL_PROBE_TIMEOUT_ENV`] overrides it.
pub const VERSION_PROBE_TIMEOUT_SECS: u64 = 60;

/// Environment variable overriding [`VERSION_PROBE_TIMEOUT_SECS`]
/// (whole seconds, 1..=3600; anything else keeps the default).
pub const TOOL_PROBE_TIMEOUT_ENV: &str = "FRAMETRACE_TOOL_PROBE_TIMEOUT";

/// The version-probe bound in effect (R6).
pub fn version_probe_timeout_secs() -> u64 {
    parse_probe_timeout(std::env::var(TOOL_PROBE_TIMEOUT_ENV).ok().as_deref())
}

/// [`version_probe_timeout_secs`] with the raw env value injected (tests).
pub fn parse_probe_timeout(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| (1..=3600).contains(secs))
        .unwrap_or(VERSION_PROBE_TIMEOUT_SECS)
}

/// Why [`run_with_timeout_detailed`] produced no output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The command could not be started or waited for.
    Failed(String),
    /// It ran past its bound and was terminated — a fact about the run,
    /// not about whether the tool exists or works (R6).
    TimedOut(String),
}

impl RunError {
    pub fn message(&self) -> &str {
        match self {
            RunError::Failed(message) | RunError::TimedOut(message) => message,
        }
    }
}

/// Windows `SEM_FAILCRITICALERRORS`: no critical-error / missing-DLL
/// loader message box; the call fails instead.
#[cfg(windows)]
pub const SEM_FAILCRITICALERRORS: u32 = 0x0001;
/// Windows `SEM_NOGPFAULTERRORBOX`: a crashing child (an ffmpeg build
/// faulting on hostile media) terminates instead of parking on the
/// Windows Error Reporting "has stopped working" dialog, which would keep
/// the step (and its timeout-less callers) waiting on a click nobody sees.
#[cfg(windows)]
pub const SEM_NOGPFAULTERRORBOX: u32 = 0x0002;
/// Windows `SEM_NOOPENFILEERRORBOX`: no "cannot find file" message box.
#[cfg(windows)]
pub const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetErrorMode(mode: u32) -> u32;
    fn GetErrorMode() -> u32;
}

/// R4-B21: the process error mode as the OS reports it right now
/// (`GetErrorMode`), so tests check the real flag word instead of the value
/// [`suppress_os_error_dialogs`] claims to have set. 0 off Windows.
pub fn current_error_mode() -> u32 {
    #[cfg(windows)]
    {
        // SAFETY: GetErrorMode only reads a process-wide flag word.
        unsafe { GetErrorMode() }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// P7: process-wide error mode set once at startup so neither this
/// process nor the tools it spawns (children inherit the mode) can block
/// on a modal system dialog — a missing DLL in an ffmpeg/libewf build
/// otherwise pops "code execution cannot proceed" and the probe (or the
/// whole pipeline step) waits on a click nobody sees. Existing flags are
/// kept. No-op off Windows. Returns the resulting mode (0 off Windows).
pub fn suppress_os_error_dialogs() -> u32 {
    #[cfg(windows)]
    {
        let wanted = SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX;
        // SAFETY: SetErrorMode only swaps a process-wide flag word; the
        // first call reads the previous mode so it can be merged.
        unsafe {
            let previous = SetErrorMode(wanted);
            SetErrorMode(previous | wanted);
            previous | wanted
        }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

// ---- Path-shape policy (pure, no filesystem access) ----
//
// Shared by the HTTP path gate (`serve::http::reject_remote_path`) and the
// report redactor (`redact::Redactor`), so neither ever hands a remote or
// device path to `canonicalize`/`metadata` — on Windows that opens an SMB
// session (leaking the examiner's NTLM credentials to the named host) or
// a device that can block the caller for good.

/// Whether one path component names a reserved DOS device. Windows maps
/// the name to the device regardless of extension (`NUL.txt`) and of
/// trailing spaces/dots, and case-insensitively; the superscript digits
/// `¹²³` count as digits for COM/LPT.
pub(crate) fn is_reserved_device_name(component: &str) -> bool {
    let base = component
        .split(['.', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) {
        return true;
    }
    let Some(digit) = base
        .strip_prefix("COM")
        .or_else(|| base.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut chars = digit.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('0'..='9' | '¹' | '²' | '³'), None)
    )
}

/// Whether any `\`/`/`-separated component of `text` is a reserved device.
///
/// R4-B9: a drive-relative component (`C:NUL`, `d:com1.txt`) names the
/// device too — Windows resolves `C:NUL` against drive C's current
/// directory and still opens NUL — so the part after a leading `X:` is
/// checked as its own component.
pub(crate) fn has_reserved_device_component(text: &str) -> bool {
    text.split(['\\', '/']).any(|component| {
        is_reserved_device_name(component)
            || drive_relative_tail(component).is_some_and(is_reserved_device_name)
    })
}

/// `NUL` of `C:NUL`: the name after a leading drive designator, if any.
fn drive_relative_tail(component: &str) -> Option<&str> {
    let bytes = component.as_bytes();
    (bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':').then(|| &component[2..])
}

/// A component that stays a device name even under the verbatim `\\?\`
/// prefix once that prefix is stripped again: the bare name, optionally
/// with trailing spaces/dots (`AUX`, `nul.`) or a stream suffix (`CON:x`).
/// `aux.mp4` is not bare — it has a real extension.
fn is_bare_reserved_device_name(component: &str) -> bool {
    let head = component.split(':').next().unwrap_or_default();
    let trimmed = head.trim_end_matches([' ', '.']);
    !trimmed.contains('.') && is_reserved_device_name(trimmed)
}

/// R4: whether a path is refused for naming a reserved Windows device.
///
/// - Ordinary (Win32-namespace) paths — `C:\x\aux.mp4`,
///   `\\server\s\nul.txt`, relative paths — keep the conservative legacy
///   rule: a component whose base name (before the first `.`/`:`,
///   trailing spaces ignored) is a device is refused, extension or not.
///   Windows 10 and older open the device for `aux.mp4`; Windows 11
///   narrowed that to the bare name, but the server must be safe on every
///   supported Windows.
/// - A verbatim local path `\\?\C:\…` bypasses Win32 name parsing, so
///   `\\?\C:\x\aux.mp4` is an ordinary file and is admitted. A *bare*
///   device component (`\\?\C:\x\AUX`, `…\nul.`) is still refused: code
///   that strips the verbatim prefix for display or a child process's argv
///   would turn it back into the device.
///
/// Residual (accepted, documented in PLAN_PHASE3 §R): a verbatim
/// `aux.mp4` whose prefix is stripped and reopened on Windows 10 would
/// reach the device again; the media/export handlers open the canonical
/// (verbatim) path.
pub(crate) fn reserved_device_refused(text: &str) -> bool {
    if is_verbatim_local_drive(text) {
        text[4..]
            .split(['\\', '/'])
            .any(is_bare_reserved_device_name)
    } else {
        has_reserved_device_component(text)
    }
}

fn is_separator(byte: u8) -> bool {
    byte == b'\\' || byte == b'/'
}

/// Two leading separators (any mix of `\` and `/`) or the NT object prefix.
/// Off Windows a leading `/` is always a local absolute path (S8).
pub(crate) fn is_remote_or_device_path(text: &str, windows: bool) -> bool {
    let bytes = text.as_bytes();
    if !windows && bytes.first() == Some(&b'/') {
        return false;
    }
    (bytes.len() >= 2 && is_separator(bytes[0]) && is_separator(bytes[1]))
        || (bytes.len() >= 4
            && is_separator(bytes[0])
            && bytes[1] == b'?'
            && bytes[2] == b'?'
            && is_separator(bytes[3]))
}

/// `\\?\C:\…` — the verbatim spelling of a local drive path. R21-S1: the
/// root separator is required — `\\?\C:` alone names the raw VOLUME
/// device (like `\\.\C:`; its metadata fails with ERROR_INVALID_FUNCTION),
/// not the drive's root folder, so it is a device path.
pub(crate) fn is_verbatim_local_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 7
        && bytes[0] == b'\\'
        && bytes[1] == b'\\'
        && bytes[2] == b'?'
        && bytes[3] == b'\\'
        && bytes[4].is_ascii_alphabetic()
        && bytes[5] == b':'
        && bytes[6] == b'\\'
}

/// Lowercased `\\server\share[\rest]` form of a UNC path (separators
/// normalized to `\`, `\\?\UNC\` unwrapped, trailing separators dropped);
/// `None` for anything else — device paths, local paths, or a UNC path
/// without both a server and a share component.
pub(crate) fn unc_share_form(text: &str) -> Option<String> {
    let normalized = text.trim().replace('/', "\\");
    let lower = normalized.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix(r"\\?\unc\") {
        rest.to_string()
    } else if lower.starts_with(r"\\?\") || lower.starts_with(r"\\.\") || lower.starts_with(r"\??\")
    {
        return None;
    } else if let Some(rest) = lower.strip_prefix(r"\\") {
        rest.to_string()
    } else {
        return None;
    };
    let rest = rest.trim_end_matches('\\');
    let mut parts = rest.split('\\');
    let server = parts.next().unwrap_or_default();
    let share = parts.next().unwrap_or_default();
    if server.is_empty() || share.is_empty() || matches!(server, "?" | ".") {
        return None;
    }
    Some(format!(r"\\{rest}"))
}

/// R1: whether `text` names something that must never reach a filesystem
/// call under `windows` path semantics: a UNC / device-namespace path
/// (`\\host\…`, `//host/…`, `\\?\UNC\…`, `\\.\…`, `\\?\GLOBALROOT\…`,
/// `\??\…`) or a path with a reserved device component
/// ([`reserved_device_refused`]). The verbatim local drive form `\\?\C:\…`
/// is local. Off Windows a leading `/` is always local (S8).
pub(crate) fn is_remote_or_device_text(text: &str, windows: bool) -> bool {
    let text = text.trim();
    if windows && reserved_device_refused(text) {
        return true;
    }
    is_remote_or_device_path(text, windows) && !is_verbatim_local_drive(text)
}

pub fn now_unix() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|err| format!("system time before UNIX epoch: {err}"))
}

/// Windows `CREATE_NO_WINDOW` process-creation flag.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Keeps an external console tool (ffmpeg, ffprobe, icat, ...) from
/// flashing a console window when the caller has no console of its own
/// (`frametrace-app.exe` is a `windows_subsystem = "windows"` binary).
/// No-op off Windows. Every `Command` that reaches `spawn`/`output`/
/// `status` goes through this (or `run_with_timeout`/`spawn_tracked`,
/// which call it) — a source-scan test enforces that.
pub fn hide_console(command: &mut Command) -> &mut Command {
    hide_console_with_flags(command, 0)
}

/// [`hide_console`] plus extra process-creation flags. `creation_flags`
/// REPLACES the flag word on std's `Command`, so the one place that needs
/// other flags (suspended spawn for job tracking, breakaway for the
/// browser launch) must set them together with `CREATE_NO_WINDOW` here
/// instead of calling `creation_flags` itself.
pub fn hide_console_with_flags(command: &mut Command, extra_flags: u32) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW | extra_flags);
    }
    #[cfg(not(windows))]
    {
        let _ = extra_flags;
    }
    command
}

/// Runs an external command, killing it after `timeout_secs` when set.
/// `None` keeps the historical unlimited behaviour for long conversions.
pub fn run_with_timeout(
    command: &mut Command,
    timeout_secs: Option<u64>,
) -> Result<Output, String> {
    run_with_timeout_detailed(command, timeout_secs).map_err(|err| err.message().to_string())
}

/// [`run_with_timeout`] that tells a timeout apart from a failure to run.
pub fn run_with_timeout_detailed(
    command: &mut Command,
    timeout_secs: Option<u64>,
) -> Result<Output, RunError> {
    let program = command.get_program().to_string_lossy().to_string();
    hide_console(command);
    let Some(secs) = timeout_secs else {
        return command
            .output()
            .map_err(|err| RunError::Failed(format!("failed to run {program}: {err}")));
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|err| RunError::Failed(format!("failed to run {program}: {err}")))?;
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdout_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout_pipe, &mut buffer);
        buffer
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr_pipe, &mut buffer);
        buffer
    });
    let status = child
        .wait_timeout(Duration::from_secs(secs))
        .map_err(|err| RunError::Failed(format!("failed to wait for {program}: {err}")))?;
    match status {
        Some(status) => Ok(Output {
            status,
            stdout: stdout_reader.join().unwrap_or_default(),
            stderr: stderr_reader.join().unwrap_or_default(),
        }),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(RunError::TimedOut(format!(
                "{program} did not finish within {secs}s and was terminated (retry with a larger --timeout)"
            )))
        }
    }
}

pub fn create_case_layout(case_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(case_dir)?;
    for child in [
        "evidence",
        "evidence/hashes",
        "evidence/images",
        "evidence/logs",
        "artifacts",
        "artifacts/recovered",
        "artifacts/recovered/filesystem",
        "artifacts/carved",
        "artifacts/proxies",
        "artifacts/thumbnails",
        "artifacts/clips",
        "db",
        "db/filesystem",
        "review",
        "reports",
    ] {
        fs::create_dir_all(case_dir.join(child))?;
    }
    Ok(())
}

pub fn write_text(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)
}

/// Picks a not-yet-existing artifact path AND atomically reserves it by
/// creating an empty placeholder with `O_EXCL` semantics. Two concurrent
/// pipelines (workstation + examiner CLI) can therefore never be handed
/// the same path and silently overwrite each other's recovered evidence —
/// the loser of the reservation race simply gets the next candidate.
/// Callers overwrite the placeholder through their normal create/open,
/// which is safe because only the reserving process can hold this name.
pub fn unique_path(path: &Path) -> PathBuf {
    unique_path_impl(path, true)
}

/// Same naming scheme without the placeholder reservation, for callers
/// that enforce a strict "output path must not exist" contract themselves
/// (export-dav/export-hik hard-reject any pre-existing target, so a
/// reservation placeholder would look like a collision). These call
/// sites create the real artifact immediately after the check and accept
/// the tiny check-to-create window instead.
pub fn unique_available_path(path: &Path) -> PathBuf {
    unique_path_impl(path, false)
}

fn unique_path_impl(path: &Path, reserve: bool) -> PathBuf {
    let claim = |candidate: &Path| -> bool {
        if reserve {
            reserve_file(candidate)
        } else {
            !candidate.exists()
        }
    };
    if claim(path) {
        return path.to_path_buf();
    }

    let candidate = unique_path_suffixes(
        path,
        &mut (1..10_000).map(|index| format!("_{index:03}")),
        claim,
    );
    if let Some(candidate) = candidate {
        return candidate;
    }

    // Exhausted the numeric range: fall back to a nanosecond-stamped name.
    // Returning the ORIGINAL path here would hand callers an existing file to
    // overwrite, which is unacceptable for forensic artifacts.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let candidate = unique_path_suffixes(path, &mut std::iter::once(format!("_{stamp}")), claim);
    if let Some(candidate) = candidate {
        return candidate;
    }
    // Pathological: even the nanosecond name is taken. Reserve a final
    // suffixed attempt; on failure return the stamped name anyway — the
    // caller's write will fail loudly rather than overwrite silently.
    path.with_file_name(format!(
        "{}_{stamp}.{}",
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("artifact"),
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("bin")
    ))
}

/// Directory-target variant: the claim is a real directory (created
/// atomically), not a placeholder file, so callers like package-case can
/// `create_dir_all` the result.
pub fn unique_dir(path: &Path) -> PathBuf {
    if reserve_dir(path) {
        return path.to_path_buf();
    }
    if let Some(candidate) =
        unique_dir_suffixes(path, &mut (1..10_000).map(|index| format!("_{index:03}")))
    {
        return candidate;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    if let Some(candidate) = unique_dir_suffixes(path, &mut std::iter::once(format!("_{stamp}"))) {
        return candidate;
    }
    path.to_path_buf()
}

fn unique_path_suffixes(
    path: &Path,
    suffixes: &mut dyn Iterator<Item = String>,
    claim: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("artifact");
    let extension = path.extension().and_then(|extension| extension.to_str());
    for suffix in suffixes {
        let filename = match extension {
            Some(extension) if !extension.is_empty() => {
                format!("{stem}{suffix}.{extension}")
            }
            _ => format!("{stem}{suffix}"),
        };
        let candidate = parent.join(filename);
        if claim(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn unique_dir_suffixes(path: &Path, suffixes: &mut dyn Iterator<Item = String>) -> Option<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    for suffix in suffixes {
        let candidate = parent.join(format!("{name}{suffix}"));
        if reserve_dir(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn reserve_dir(path: &Path) -> bool {
    fs::create_dir(path).is_ok()
}

/// Atomically claims `path` by creating it exclusively. Returns true when
/// this call won the claim. A pre-existing empty placeholder from a crashed
/// earlier run is treated as claimed space and skipped (safe: artifacts are
/// never appended after the fact).
fn reserve_file(path: &Path) -> bool {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => true,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
        // Unresolvable name (permission, invalid syntax): report claimed so
        // callers keep the original path and surface the real write error.
        Err(_) => true,
    }
}

/// R13-S14: a per-process counter for [`write_text_atomic`] temp names.
static ATOMIC_TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// R13-S14: a temp name next to `path` that no other writer — another
/// process, or another thread of this one — uses at the same time:
/// `.<name>.tmp-<pid>-<seq>-<nanos>`. R12 used the pid alone, so two
/// threads writing the same file (the C4/S14 abandoned-export tests, two
/// marks saves) wrote into one temp file and one rename failed.
pub fn unique_temp_sibling(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let seq = ATOMIC_TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(
        ".{file_name}.tmp-{}-{seq}-{nanos:08x}",
        std::process::id()
    ))
}

/// Writes state files (case manifest, indexes) so a crash can never leave a
/// truncated half-file: the payload lands in a sibling temp file first, is
/// fsynced, and only then atomically renamed over the target
/// ([`rename_durable`]: write-through on Windows, directory synced).
pub fn write_text_atomic(path: &Path, text: &str) -> io::Result<()> {
    write_bytes_atomic(path, text.as_bytes())
}

#[cfg(test)]
thread_local! {
    /// Test hook: [`write_bytes_atomic`] of exactly this path fails after
    /// the temp file was written (R13-S14: the temp name is unique now, so
    /// tests can no longer pre-create it as a blocker).
    pub(crate) static FAIL_ATOMIC_WRITE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// [`write_text_atomic`] for any bytes.
pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = unique_temp_sibling(path);
    let written = (|| {
        let mut file = fs::File::create(&temp)?;
        io::Write::write_all(&mut file, bytes)?;
        io::Write::flush(&mut file)?;
        file.sync_all()?;
        #[cfg(test)]
        if FAIL_ATOMIC_WRITE.with(|fail| fail.borrow().as_deref() == Some(path)) {
            return Err(io::Error::other("injected: atomic write failure"));
        }
        Ok(())
    })();
    if let Err(err) = written.and_then(|()| rename_durable(&temp, path)) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

/// R13-S9 / R14-S6: renames `from` over `to` and then flushes the parent
/// directory, so the new name survives a power loss. Windows:
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`.
/// `MOVEFILE_WRITE_THROUGH` only promises anything for a move that copies
/// across volumes (the call then returns once the copy is on disk); a
/// same-volume rename is a metadata change it does NOT flush (R13 claimed
/// it did) — so the directory is flushed after a successful call too
/// ([`sync_parent_directory`]). When that call fails (e.g. a target another
/// program holds open with delete sharing, which `fs::rename`'s
/// POSIX-semantics rename can replace) `fs::rename` is tried. Elsewhere:
/// `rename(2)` and an fsync of the parent directory. The directory flush is
/// best-effort on every platform (see [`sync_parent_directory`]).
pub fn rename_durable(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        use windows::core::HSTRING;
        let source = HSTRING::from(from.as_os_str());
        let target = HSTRING::from(to.as_os_str());
        // SAFETY: two NUL-terminated wide strings that outlive the call.
        let moved = unsafe {
            MoveFileExW(
                &source,
                &target,
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved.is_ok() {
            sync_parent_directory(to);
            return Ok(());
        }
    }
    fs::rename(from, to)?;
    sync_parent_directory(to);
    Ok(())
}

/// R13-S9: removes `path` and flushes its directory (a removal is durable
/// only once the directory entry is on disk). NotFound is success.
pub fn remove_file_durable(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            sync_parent_directory(path);
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Best-effort directory flush so a file creation, rename or removal
/// survives power loss. Unix: `fsync` of the parent directory. Windows
/// (R13-S9 — R12 documented this but did nothing there): the parent is
/// opened with write access and backup semantics (the only way to get a
/// directory handle) and `FlushFileBuffers` flushes its metadata. Failures
/// are ignored (a file system that cannot flush a directory, no write
/// access) — a same-volume rename or removal is then only as durable as
/// the file system's own metadata journaling makes it (NTFS: the change is
/// journaled and lazily flushed; there is no stronger Win32 guarantee).
pub fn sync_parent_directory(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    #[cfg(unix)]
    {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE};
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FlushFileBuffers, OPEN_EXISTING,
        };
        use windows::core::HSTRING;
        let dir = HSTRING::from(parent.as_os_str());
        // SAFETY: a wide path that outlives the call; the handle is closed.
        unsafe {
            if let Ok(handle) = CreateFileW(
                &dir,
                GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            ) {
                let _ = FlushFileBuffers(handle);
                let _ = CloseHandle(handle);
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
    }
}

pub fn read_to_string(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
}

/// Resolves an artifact `output_path` recorded in a carve/recover log.
/// Logs written before `carve_file` canonicalized the case dir (or copied
/// between cases) can hold paths relative to the invocation cwd — e.g.
/// `<case-name>\artifacts\carved\file.avi` — so probe the same candidates
/// every consumer uses: case-relative, case-parent-relative, then
/// `artifacts/carved/<basename>` inside the case. Falls back to the raw
/// path when nothing exists so callers surface an honest miss.
pub fn resolve_case_artifact_path(case_dir: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() || raw.starts_with("file:") {
        return path.to_path_buf();
    }
    let candidates = [
        case_dir.join(path),
        case_dir
            .parent()
            .map(|p| p.join(path))
            .unwrap_or_else(|| path.to_path_buf()),
        path.file_name()
            .map(|n| case_dir.join("artifacts/carved").join(n))
            .unwrap_or_else(|| path.to_path_buf()),
    ];
    candidates
        .iter()
        .find(|c| c.exists())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// Canonicalizes a path and strips the Windows extended-length prefix (`\\?\`)
/// so user-facing output and audit logs keep ordinary paths. `\\?\UNC\` maps
/// back to the leading `\\server\share` form.
pub fn canonicalize_display(path: &Path) -> io::Result<PathBuf> {
    Ok(strip_windows_extended_prefix(&path.canonicalize()?))
}

pub fn strip_windows_extended_prefix(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        // R5 (P3): a name that is not valid Unicode is handled on its OS
        // units — `to_string_lossy` used to replace them with U+FFFD and
        // so name another (usually nonexistent) file.
        return strip_extended_prefix_lossless(path);
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest.to_string())
    } else {
        path.to_path_buf()
    }
}

/// [`strip_windows_extended_prefix`] on the UTF-16 units (Windows); other
/// platforms have no verbatim prefix, so the path is returned unchanged.
fn strip_extended_prefix_lossless(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        let wide = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
        let unc = wide(r"\\?\UNC\");
        let verbatim = wide(r"\\?\");
        let stripped: Vec<u16> = if units.starts_with(&unc) {
            wide(r"\\")
                .into_iter()
                .chain(units[unc.len()..].iter().copied())
                .collect()
        } else if units.starts_with(&verbatim) {
            units[verbatim.len()..].to_vec()
        } else {
            return path.to_path_buf();
        };
        PathBuf::from(std::ffi::OsString::from_wide(&stripped))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Neutralizes spreadsheet formula injection in one CSV cell, then applies
/// RFC 4180 quoting. Evidence-derived text (file names, notes, tags) that
/// starts with `=`, `+`, `-`, `@`, TAB or CR is evaluated as a formula by
/// Excel/LibreOffice; such cells get a leading `'` so they open as text.
/// Plain numbers (e.g. `-3600`, `+1.5`) are left untouched so numeric
/// columns survive. The prefix is a CSV-only transformation; the JSON and
/// HTML outputs keep the original value.
pub fn csv_safe_cell(value: &str) -> String {
    let is_number = !value.is_empty()
        && value.parse::<f64>().is_ok()
        && value
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.' | 'e' | 'E'));
    let neutralized = if !is_number && value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if neutralized.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", neutralized.replace('"', "\"\""))
    } else {
        neutralized
    }
}

pub fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            ch if ch < ' ' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
}

pub fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// The single file-name sanitizer. Keeps ASCII alphanumerics, `-` and `_`;
/// everything else (separators, dots, control characters, spaces, non-ASCII)
/// becomes `_` one-for-one. Windows reserved device names (CON, PRN, AUX, NUL,
/// COM1-9, LPT1-9) get a leading `_` (such names could never be created on
/// Windows, so no existing file changes name), trailing dots/spaces are
/// impossible by construction, and an empty result becomes `clip`.
///
/// The output is NOT length-capped on purpose: proxies, clips and telemetry
/// are found again by re-sanitizing the selector (often a full source path),
/// and the HTML report mirrors this rule in JS (`sanitizeSelector`). Changing
/// the mapping would orphan artifacts in existing cases.
pub fn sanitize_filename(input: &str) -> String {
    let mut out: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        return "clip".to_string();
    }
    let upper = out.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && matches!(upper.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        out.insert(0, '_');
    }
    out
}

/// The literal a generated page embeds in a `<script>` block for the JSON
/// text `input`.
///
/// R6-S1: `input` must be ONE JSON value — whatever reaches the script is
/// data and nothing else. Before, the text was only escaped for
/// `<`/`>`/`&` and embedded verbatim: a crafted `case.json` such as
/// `{"case_id":"x"};fetch(…);//` ran in the workstation origin from a
/// page the generated-pages list vouches for. Input that is not exactly
/// one JSON value becomes `null` (callers that read files say so — see
/// [`script_json_input`]).
///
/// R7-S1: the input is VALIDATED, not parsed into a tree: serde_json's
/// [`serde_json::value::RawValue`] checks the whole grammar (one value,
/// only whitespace after it) without building a `Value`, and
/// [`script_safe_json`] then copies it once (insignificant whitespace
/// dropped; `<`, `>`, `&`, U+2028/U+2029 escaped). R6 built a `Value` of
/// a file list that can be hundreds of MB and copied it six more times
/// (`to_string` plus one `replace` per character). The value's text is
/// kept as written (numbers, escapes, key order).
pub fn json_for_script(input: &str) -> String {
    match validated_json(input) {
        Some(raw) => script_safe_json(raw, true),
        None => "null".to_string(),
    }
}

#[cfg(test)]
thread_local! {
    /// R7-S1 test hook: inputs [`validated_json`] accepted on this thread.
    pub(crate) static RAW_JSON_VALIDATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// R7-S1: `input`'s single JSON value (outer whitespace trimmed) when
/// `input` is exactly one JSON value, else `None`. Grammar check only —
/// no tree is built, nothing is allocated for the value.
fn validated_json(input: &str) -> Option<&str> {
    let raw = serde_json::from_str::<&serde_json::value::RawValue>(input).ok()?;
    #[cfg(test)]
    RAW_JSON_VALIDATIONS.with(|count| count.set(count.get() + 1));
    Some(raw.get())
}

/// R7-S1: one pass over VALID JSON text: whitespace outside strings is
/// dropped and, with `escape`, `<`, `>`, `&` and the JS line terminators
/// U+2028/U+2029 are written as `\uXXXX` (they can only occur inside
/// strings, where the escape means the same character). Not a parser —
/// `json` must already be valid JSON.
///
/// R8-S3: with `escape` (the text goes into a script as a JS object
/// literal) an object key whose value is `__proto__` is written as the
/// computed key `["__proto__"]`. In an object literal a plain
/// `"__proto__":` (escaped spellings included) is the prototype setter,
/// not a property, and two of them in one object are a SyntaxError — a
/// crafted `case.json` with a duplicate `__proto__` key blanked the page.
/// A computed key defines an own data property exactly as `JSON.parse`
/// does (duplicates allowed, the last value wins), so the literal still
/// means the same JSON value. Without `escape` the output stays JSON.
fn script_safe_json(json: &str, escape: bool) -> String {
    let bytes = json.as_bytes();
    let mut out = String::with_capacity(json.len() + 16);
    let mut run_start = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    // R8-S3: open containers (true = object; depth is bounded by the
    // validating parser's recursion limit) and whether the next string
    // is an object key.
    let mut containers: Vec<bool> = Vec::new();
    let mut expect_key = false;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        // (replacement, bytes consumed) when this byte is not copied as is.
        let mut replace: Option<(&str, usize)> = None;
        if !in_string && escape {
            match byte {
                b'{' => {
                    containers.push(true);
                    expect_key = true;
                }
                b'[' => {
                    containers.push(false);
                    expect_key = false;
                }
                b'}' | b']' => {
                    containers.pop();
                    expect_key = false;
                }
                b',' => expect_key = containers.last() == Some(&true),
                b':' => expect_key = false,
                b'"' if expect_key => {
                    expect_key = false;
                    if let Some(end) = proto_key_end(bytes, index) {
                        out.push_str(&json[run_start..index]);
                        out.push_str("[\"__proto__\"]");
                        index = end;
                        run_start = index;
                        continue;
                    }
                }
                _ => {}
            }
        }
        if in_string {
            if escaped {
                escaped = false;
            } else {
                match byte {
                    b'\\' => escaped = true,
                    b'"' => in_string = false,
                    b'<' if escape => replace = Some(("\\u003c", 1)),
                    b'>' if escape => replace = Some(("\\u003e", 1)),
                    b'&' if escape => replace = Some(("\\u0026", 1)),
                    0xE2 if escape && bytes.get(index + 1) == Some(&0x80) => {
                        match bytes.get(index + 2) {
                            Some(0xA8) => replace = Some(("\\u2028", 3)),
                            Some(0xA9) => replace = Some(("\\u2029", 3)),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b' ' | b'\t' | b'\n' | b'\r' => replace = Some(("", 1)),
                _ => {}
            }
        }
        match replace {
            Some((text, consumed)) => {
                out.push_str(&json[run_start..index]);
                out.push_str(text);
                index += consumed;
                run_start = index;
            }
            None => index += 1,
        }
    }
    out.push_str(&json[run_start..]);
    out
}

/// R8-S3: when the JSON string starting at `bytes[open]` (a `"`) reads
/// `__proto__` once unescaped, the index just past its closing quote.
/// Only short strings are decoded: `__proto__` written with every
/// character as `\uXXXX` is 54 bytes between the quotes.
fn proto_key_end(bytes: &[u8], open: usize) -> Option<usize> {
    const LONGEST: usize = 9 * 6;
    let mut escaped = false;
    let mut close = None;
    for (offset, &byte) in bytes.get(open + 1..)?.iter().enumerate().take(LONGEST + 1) {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            close = Some(open + 1 + offset);
            break;
        }
    }
    let close = close?;
    let literal = std::str::from_utf8(&bytes[open..=close]).ok()?;
    (serde_json::from_str::<String>(literal).ok()? == "__proto__").then_some(close + 1)
}

/// [`json_for_script`] for an already built value (program-made data,
/// nothing to validate): compact JSON escaped in the same single pass.
pub fn script_json_value(value: &serde_json::Value) -> String {
    match serde_json::to_string(value) {
        Ok(text) => script_safe_json(&text, true),
        Err(_) => "null".to_string(),
    }
}

/// R6-S1: validates an input file's text (`label` names it) before a
/// generator embeds it: `Ok(compact JSON)` when it is exactly one JSON
/// value, else `Err(warning)` — the caller embeds `null` and prints the
/// warning (never a silent blank page). R7-S1: validated without a tree,
/// like [`json_for_script`].
pub fn script_json_input(label: &str, text: &str) -> Result<String, String> {
    match serde_json::from_str::<&serde_json::value::RawValue>(text) {
        Ok(raw) => Ok(script_safe_json(raw.get(), false)),
        Err(err) => Err(format!(
            "{label}이(가) 올바른 JSON이 아니어서 페이지에 넣지 않았습니다 (null로 대체) / {label} is not valid JSON and was embedded as null: {err}"
        )),
    }
}

/// R7-S6: stderr prefix of a warning a CLI child wants the workstation to
/// show as a JOB warning (status `warnings`, log) — the windowed app has
/// no console, so a plain `warning:` line of a step was never seen.
pub const STEP_WARNING_MARKER: &str = "FRAMETRACE-STEP-WARNING: ";

/// R7-S6: environment variable the workstation sets on its CLI children:
/// they print warnings with [`STEP_WARNING_MARKER`] (see
/// [`print_step_warning`]).
///
/// R8-S7: its value is a random per-step token (R7: `1`). Any stderr line
/// that merely STARTS with a marker was promoted — and a step's stderr
/// carries text it does not control (ffmpeg/ffprobe messages quoting
/// evidence names and metadata), so evidence could forge a job warning.
/// Only lines carrying the step's token ([`tokened_marker`]) count now.
pub const STEP_WARNING_ENV: &str = "FRAMETRACE_STEP_WARNINGS";

/// R8-S7: the per-step token in [`STEP_WARNING_ENV`], when it is one
/// (16–64 ASCII hex digits). R9-S7: the token this process adopted at
/// start ([`adopt_step_warning_token`]); the environment only when it
/// adopted none (library callers, tests).
pub fn step_warning_token() -> Option<String> {
    if let Some(adopted) = ADOPTED_STEP_TOKEN.get() {
        return adopted.clone();
    }
    valid_step_token(std::env::var(STEP_WARNING_ENV).ok()?)
}

fn valid_step_token(token: String) -> Option<String> {
    let valid = (16..=64).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_hexdigit());
    valid.then_some(token)
}

/// R9-S7: the step token read (and removed from the environment) at
/// process start.
static ADOPTED_STEP_TOKEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// R9-S7: keeps this step's token for [`step_warning_token`] and removes
/// [`STEP_WARNING_ENV`] from the process environment, so the tools the
/// step runs (ffmpeg, ffprobe, Sleuth Kit, libewf, python, ...) never
/// inherit it. R8 left it in the environment: every grandchild held the
/// token that makes a stderr line a job warning, and its output reaches
/// the same stderr.
///
/// # Safety
/// Removes an environment variable: call it first thing in `main`, before
/// any other thread exists (no concurrent environment access).
pub unsafe fn adopt_step_warning_token() {
    // SAFETY: forwarded to the caller (single-threaded process start).
    let token = unsafe { take_env_var(STEP_WARNING_ENV) };
    let _ = ADOPTED_STEP_TOKEN.set(token.and_then(valid_step_token));
}

/// R9-S7: reads `name` and removes it from this process's environment.
///
/// # Safety
/// As [`std::env::remove_var`]: no other thread may access the
/// environment at the same time.
pub unsafe fn take_env_var(name: &str) -> Option<String> {
    let value = std::env::var_os(name)?;
    // SAFETY: forwarded to the caller.
    unsafe { std::env::remove_var(name) };
    value.into_string().ok()
}

/// R8-S7: `marker` (`"NAME: "`) bound to a step token: `"NAME[token]: "`.
pub fn tokened_marker(marker: &str, token: &str) -> String {
    format!("{}[{token}]: ", marker.trim_end_matches(": "))
}

/// R8-S7: one machine-readable stderr line for the workstation: `marker`
/// bound to this step's token, or the plain `marker` line when no step
/// token was given (a person at a console). Line breaks in `message`
/// become spaces (one line per warning).
pub fn step_marker_line(marker: &str, message: &str) -> String {
    let message = message.replace(['\r', '\n'], " ");
    match step_warning_token() {
        Some(token) => format!("{}{message}", tokened_marker(marker, &token)),
        None => format!("{marker}{message}"),
    }
}

/// R7-S6: prints a warning on stderr — `warning: …` for a person at a
/// console, [`STEP_WARNING_MARKER`]`…` (R8-S7: bound to the step token)
/// when the workstation runs this process as a step (it promotes such
/// lines to job warnings).
pub fn print_step_warning(message: &str) {
    if step_warning_token().is_some() {
        eprintln!("{}", step_marker_line(STEP_WARNING_MARKER, message));
    } else {
        eprintln!("warning: {}", message.replace(['\r', '\n'], " "));
    }
}

pub fn path_to_file_url(path: &Path) -> String {
    // `std::fs::canonicalize` returns `\\?\`-prefixed paths on Windows; those
    // prefixes would survive into the URL and break it (`file:////?/C:/...`),
    // so strip them before encoding.
    let path =
        strip_windows_extended_prefix(&path.canonicalize().unwrap_or_else(|_| PathBuf::from(path)));
    let raw = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        format!("file:///{}", percent_encode_path(&raw))
    } else {
        format!("file://{}", percent_encode_path(&raw))
    }
}

pub fn compact_json_value_if_well_formed(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let opener = trimmed.chars().next()?;
    let closer = match opener {
        '{' => '}',
        '[' => ']',
        _ => return None,
    };

    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut compact = String::new();

    for ch in trimmed.chars() {
        if in_string {
            compact.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                compact.push(ch);
            }
            '{' | '[' => {
                stack.push(if ch == '{' { '}' } else { ']' });
                compact.push(ch);
            }
            '}' | ']' => {
                if stack.pop()? != ch {
                    return None;
                }
                compact.push(ch);
            }
            ch if ch.is_whitespace() => {}
            _ => compact.push(ch),
        }
    }

    if in_string || escaped || !stack.is_empty() || !compact.ends_with(closer) {
        return None;
    }
    Some(compact)
}

fn percent_encode_path(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            byte => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// R4-B5: which HTML pages under a case folder FrameTrace itself generated.
///
/// The workstation used to trust `review/*.html` / `reports/**.html` by
/// location alone, so a case folder handed over by someone else (or
/// tampered with) could run arbitrary script in the workstation origin and
/// drive every mutating API. Now a page at a generated location is trusted
/// only when its SHA-256 is listed in `review/.generated.json` AND that
/// list carries a valid HMAC under a per-user key kept outside the case
/// (`<config>/frametrace/generated-pages.key`) — a foreign or edited case
/// cannot forge the list. Everything else is served sandboxed (no script).
///
/// Writers: each generator (make-review, make-report, make-custody) calls
/// [`record_generated_pages`] with exactly the files it just wrote
/// (R5-S1 — nothing ever walks the folders and vouches for whatever HTML
/// happens to be there). R5-S2: the list also pins the scripts generated
/// pages load (`review/data-bundle.js`); the workstation serves a script
/// from the case folder only when its bytes are listed.
pub mod generated_pages {
    use std::path::{Path, PathBuf};

    /// Case-relative path of the manifest.
    pub const MANIFEST_REL: &str = "review/.generated.json";
    /// Most recent hashes kept (older finalized package copies stay
    /// trusted until this many newer pages were generated).
    const MAX_HASHES: usize = 1024;
    const MAC_DOMAIN: &str = "frametrace-generated-pages-v1";

    fn key_path() -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("FRAMETRACE_PAGES_KEY_FILE")
            && !custom.trim().is_empty()
        {
            return Some(PathBuf::from(custom.trim()));
        }
        // Unit tests never touch the examiner's real config directory.
        if cfg!(test) {
            return Some(
                std::env::temp_dir()
                    .join(format!("frametrace-test-pages-key-{}", std::process::id())),
            );
        }
        crate::audit_key::config_dir().map(|dir| dir.join("generated-pages.key"))
    }

    fn parse_hex32(text: &str) -> Option<Vec<u8>> {
        let text = text.trim();
        if text.len() != 64 {
            return None;
        }
        (0..32)
            .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok())
            .collect()
    }

    /// The per-user page key; created (0600) on first use when `create`.
    fn key(create: bool) -> Option<Vec<u8>> {
        let path = key_path()?;
        if let Ok(text) = std::fs::read_to_string(&path) {
            return parse_hex32(&text);
        }
        if !create {
            return None;
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).ok()?;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // create_new: two writers racing on the first use agree on whichever
        // key landed first instead of overwriting each other.
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, hex.as_bytes()));
        #[cfg(unix)]
        if created.is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let _ = created;
        parse_hex32(&std::fs::read_to_string(&path).ok()?)
    }

    fn mac(key: &[u8], hashes: &[String]) -> String {
        let mut sorted: Vec<&str> = hashes.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        let message = format!("{MAC_DOMAIN}\n{}", sorted.join("\n"));
        crate::hmac::hmac_sha256_hex(key, message.as_bytes())
    }

    /// Verified hash list of the case (empty when missing, unreadable,
    /// foreign or tampered — i.e. nothing is trusted).
    pub fn trusted_hashes(case_dir: &Path) -> Vec<String> {
        let Some(key) = key(false) else {
            return Vec::new();
        };
        read_verified(case_dir, &key)
    }

    fn read_verified(case_dir: &Path, key: &[u8]) -> Vec<String> {
        let Ok(text) = std::fs::read_to_string(case_dir.join(MANIFEST_REL)) else {
            return Vec::new();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Vec::new();
        };
        let hashes: Vec<String> = value
            .get("hashes")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let recorded = value.get("mac").and_then(serde_json::Value::as_str);
        let expected = mac(key, &hashes);
        match recorded {
            Some(recorded) if constant_time_eq(recorded, &expected) => hashes,
            _ => Vec::new(),
        }
    }

    fn constant_time_eq(a: &str, b: &str) -> bool {
        a.len() == b.len()
            && a.bytes()
                .zip(b.bytes())
                .fold(0u8, |acc, (x, y)| acc | (x ^ y))
                == 0
    }

    /// Whether the case-relative page exists and is listed in the verified
    /// manifest.
    pub fn page_is_trusted(case_dir: &Path, rel: &str) -> bool {
        file_is_trusted(case_dir, &case_dir.join(rel))
    }

    /// Whether the file at `path` (a page or script under the case) has
    /// bytes the verified manifest lists (R5-S2).
    pub fn file_is_trusted(case_dir: &Path, path: &Path) -> bool {
        file_sha256(path).is_some_and(|sha| trusted_hashes(case_dir).contains(&sha))
    }

    /// SHA-256 (hex) of a file's bytes.
    pub fn file_sha256(path: &Path) -> Option<String> {
        crate::audit::digest_file(path).ok()
    }

    /// Records the listed case-relative pages and scripts (missing files
    /// are skipped) as generated. Returns how many were recorded. A manifest that does
    /// not verify under this user's key is replaced, never extended — its
    /// hashes were never trusted.
    pub fn record_generated_pages(case_dir: &Path, rels: &[&str]) -> Result<usize, String> {
        let hashes: Vec<String> = rels
            .iter()
            .map(|rel| case_dir.join(rel))
            .filter(|path| path.is_file())
            .filter_map(|path| file_sha256(&path))
            .collect();
        record_hashes(case_dir, hashes)
    }

    /// R6-S7: records the generator's OWN bytes — `(case-relative path,
    /// content it wrote)` — so what is vouched for is exactly what the
    /// generator produced. Hashing the file on disk afterwards vouched for
    /// whatever another process had swapped in between write and record.
    pub fn record_generated_contents(
        case_dir: &Path,
        files: &[(&str, &[u8])],
    ) -> Result<usize, String> {
        use sha2::Digest;
        let hashes: Vec<String> = files
            .iter()
            .map(|(_, bytes)| {
                sha2::Sha256::digest(bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect()
            })
            .collect();
        record_hashes(case_dir, hashes)
    }

    fn record_hashes(case_dir: &Path, new_hashes: Vec<String>) -> Result<usize, String> {
        let key = key(true).ok_or_else(|| {
            "생성 페이지 키를 만들 수 없습니다 (설정 폴더 확인) / generated-page key unavailable"
                .to_string()
        })?;
        let mut hashes = read_verified(case_dir, &key);
        let mut recorded = 0usize;
        for hash in new_hashes {
            hashes.retain(|known| *known != hash);
            hashes.push(hash);
            recorded += 1;
        }
        if hashes.len() > MAX_HASHES {
            let excess = hashes.len() - MAX_HASHES;
            hashes.drain(..excess);
        }
        let body = serde_json::json!({
            "schema_version": 1,
            "note": "SHA-256 of pages FrameTrace generated; HMAC under the examiner's per-user key (not stored in the case).",
            "hashes": hashes,
            "mac": mac(&key, &hashes),
        });
        super::write_text_atomic(&case_dir.join(MANIFEST_REL), &format!("{body}\n"))
            .map_err(|err| format!("생성 페이지 목록 저장 실패: {err}"))?;
        Ok(recorded)
    }

    /// CSP `script-src` sources for a verified page: `'self'` plus the
    /// SHA-256 of every inline `<script>` block. `None` when the page uses
    /// inline event handlers or `javascript:` URLs (a hash list cannot
    /// allow those without `'unsafe-hashes'`), so such pages get no
    /// script-src rather than a broken one.
    pub fn inline_script_sources(html: &str) -> Option<String> {
        let lower = html.to_ascii_lowercase();
        if lower.contains("javascript:") || has_inline_handler(&lower) {
            return None;
        }
        let mut sources = vec!["'self'".to_string()];
        let mut cursor = 0usize;
        while let Some(found) = lower[cursor..].find("<script") {
            let tag_start = cursor + found;
            let Some(tag_len) = lower[tag_start..].find('>') else {
                break;
            };
            let tag = &lower[tag_start..tag_start + tag_len];
            let body_start = tag_start + tag_len + 1;
            let Some(body_len) = lower[body_start..].find("</script") else {
                break;
            };
            if !tag.contains(" src=") && !tag.contains(" src =") {
                use sha2::Digest;
                let digest =
                    sha2::Sha256::digest(&html.as_bytes()[body_start..body_start + body_len]);
                sources.push(format!("'sha256-{}'", super::base64_encode(&digest)));
            }
            cursor = body_start + body_len;
        }
        Some(sources.join(" "))
    }

    /// R7-S2: [`inline_script_sources`] over a page read in chunks — the
    /// SHA-256 of every inline `<script>` body, without the page in
    /// memory. It always yields a hash list (`'self'` + hashes): inline
    /// event handlers and `javascript:` URLs are not looked for, so a page
    /// that uses them gets them blocked rather than allowed — the strict
    /// side. A page served while being streamed must never get the
    /// "no script-src" answer: bytes changed after verification could
    /// render (and run) before the transfer is cut off.
    #[derive(Default)]
    pub struct InlineScriptScan {
        state: ScanState,
        /// Bytes of `<script` / `</script` matched so far.
        matched: usize,
        /// Lower-cased attributes of the open tag (bounded).
        tag: Vec<u8>,
        tag_overflow: bool,
        inline: bool,
        hasher: Option<sha2::Sha256>,
        /// Original bytes of a partial `</script` match inside a body.
        pending: Vec<u8>,
        sources: Vec<String>,
    }

    #[derive(Default, PartialEq)]
    enum ScanState {
        #[default]
        Text,
        Tag,
        Body,
    }

    const OPEN: &[u8] = b"<script";
    const CLOSE: &[u8] = b"</script";
    /// Longest `<script …>` attribute text kept; a longer tag is treated
    /// as external (its body is not hashed — the script is blocked).
    const TAG_MAX: usize = 4096;

    impl InlineScriptScan {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn feed(&mut self, chunk: &[u8]) {
            use sha2::Digest;
            let mut index = 0usize;
            while index < chunk.len() {
                match self.state {
                    ScanState::Text => {
                        if self.matched == 0 {
                            match chunk[index..].iter().position(|b| *b == b'<') {
                                Some(at) => index += at,
                                None => return,
                            }
                        }
                        let byte = chunk[index].to_ascii_lowercase();
                        index += 1;
                        if byte == OPEN[self.matched] {
                            self.matched += 1;
                            if self.matched == OPEN.len() {
                                self.state = ScanState::Tag;
                                self.matched = 0;
                                self.tag.clear();
                                self.tag_overflow = false;
                            }
                        } else {
                            self.matched = usize::from(byte == b'<');
                        }
                    }
                    ScanState::Tag => {
                        let byte = chunk[index];
                        index += 1;
                        if byte == b'>' {
                            let attrs = String::from_utf8_lossy(&self.tag);
                            self.inline = !self.tag_overflow
                                && !attrs.contains(" src=")
                                && !attrs.contains(" src =");
                            self.state = ScanState::Body;
                            self.hasher = Some(sha2::Sha256::new());
                            self.pending.clear();
                            self.matched = 0;
                        } else if self.tag.len() < TAG_MAX {
                            self.tag.push(byte.to_ascii_lowercase());
                        } else {
                            self.tag_overflow = true;
                        }
                    }
                    ScanState::Body => {
                        let hasher = self.hasher.get_or_insert_with(sha2::Sha256::new);
                        if self.matched == 0 {
                            let end = chunk[index..]
                                .iter()
                                .position(|b| *b == b'<')
                                .map_or(chunk.len(), |at| index + at);
                            hasher.update(&chunk[index..end]);
                            index = end;
                            if index == chunk.len() {
                                return;
                            }
                        }
                        let raw = chunk[index];
                        let byte = raw.to_ascii_lowercase();
                        index += 1;
                        if byte == CLOSE[self.matched] {
                            self.matched += 1;
                            self.pending.push(raw);
                            if self.matched == CLOSE.len() {
                                let digest = self.hasher.take().map(|h| h.finalize());
                                if self.inline
                                    && let Some(digest) = digest
                                {
                                    self.sources.push(format!(
                                        "'sha256-{}'",
                                        super::base64_encode(&digest)
                                    ));
                                }
                                self.pending.clear();
                                self.matched = 0;
                                self.state = ScanState::Text;
                            }
                        } else {
                            hasher.update(&self.pending);
                            self.pending.clear();
                            if byte == b'<' {
                                self.pending.push(raw);
                                self.matched = 1;
                            } else {
                                hasher.update([raw]);
                                self.matched = 0;
                            }
                        }
                    }
                }
            }
        }

        /// The CSP `script-src` sources: `'self'` plus every complete
        /// inline script's hash.
        pub fn finish(self) -> String {
            let mut sources = vec!["'self'".to_string()];
            sources.extend(self.sources);
            sources.join(" ")
        }
    }

    /// ` on<letters>=` inside a tag (approximate; a false positive only
    /// drops the optional script-src, never trusts more).
    fn has_inline_handler(lower: &str) -> bool {
        let bytes = lower.as_bytes();
        let mut index = 0;
        while let Some(found) = lower[index..].find(" on") {
            let mut cursor = index + found + 3;
            let letters_start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_lowercase() {
                cursor += 1;
            }
            while cursor < bytes.len() && bytes[cursor] == b' ' {
                cursor += 1;
            }
            if cursor > letters_start && cursor < bytes.len() && bytes[cursor] == b'=' {
                return true;
            }
            index = letters_start;
        }
        false
    }
}

/// R5-S5: what can be learned about a process id right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessProbe {
    /// No such process (or only a zombie): nothing of it can still write.
    Gone,
    /// A live process holds the id; `started` when its start time could be
    /// read (Windows, Linux).
    Running { started: Option<SystemTime> },
    /// The platform cannot tell.
    Unknown,
}

/// R5-S5: probes process `pid` (liveness and start time).
pub fn probe_process(pid: u32) -> ProcessProbe {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, FILETIME};
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        const STILL_ACTIVE: u32 = 259;
        const ERROR_INVALID_PARAMETER: i32 = 87;
        // SAFETY: plain Win32 calls; the handle is closed below.
        unsafe {
            let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(handle) => handle,
                // No process with that id.
                Err(err) if err.code().0 & 0xFFFF == ERROR_INVALID_PARAMETER => {
                    return ProcessProbe::Gone;
                }
                // Exists but not ours to inspect (another user, protected).
                Err(_) => return ProcessProbe::Running { started: None },
            };
            let mut code = 0u32;
            let alive = GetExitCodeProcess(handle, &mut code).is_ok() && code == STILL_ACTIVE;
            let mut created = FILETIME::default();
            let mut exited = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            let started =
                GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user)
                    .ok()
                    .and_then(|_| {
                        // 100 ns ticks since 1601-01-01.
                        let ticks = (u64::from(created.dwHighDateTime) << 32)
                            | u64::from(created.dwLowDateTime);
                        const EPOCH_DIFF_TICKS: u64 = 116_444_736_000_000_000;
                        ticks.checked_sub(EPOCH_DIFF_TICKS).map(|since| {
                            std::time::UNIX_EPOCH + std::time::Duration::from_nanos(since * 100)
                        })
                    });
            let _ = CloseHandle(handle);
            if alive {
                ProcessProbe::Running { started }
            } else {
                ProcessProbe::Gone
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        // /proc/<pid>/stat: state is the field after the `(comm)`, the
        // start time (clock ticks after boot) the 20th field after it.
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return if std::path::Path::new("/proc/self/stat").exists() {
                ProcessProbe::Gone
            } else {
                ProcessProbe::Unknown
            };
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            return ProcessProbe::Running { started: None };
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if matches!(fields.first(), Some(&"Z") | Some(&"X")) {
            return ProcessProbe::Gone;
        }
        unsafe extern "C" {
            fn sysconf(name: i32) -> i64;
        }
        const SC_CLK_TCK: i32 = 2;
        // SAFETY: sysconf with a constant name.
        let ticks_per_sec = unsafe { sysconf(SC_CLK_TCK) };
        let boot = std::fs::read_to_string("/proc/stat").ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("btime "))
                .and_then(|value| value.trim().parse::<u64>().ok())
        });
        let started = match (fields.get(19).and_then(|v| v.parse::<u64>().ok()), boot) {
            (Some(ticks), Some(boot)) if ticks_per_sec > 0 => Some(
                std::time::UNIX_EPOCH
                    + std::time::Duration::from_secs(boot)
                    + std::time::Duration::from_millis(ticks * 1000 / ticks_per_sec as u64),
            ),
            _ => None,
        };
        ProcessProbe::Running { started }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let Ok(pid) = i32::try_from(pid) else {
            return ProcessProbe::Unknown;
        };
        // SAFETY: signal 0 only checks for existence / permission.
        if unsafe { kill(pid, 0) } == 0 {
            return ProcessProbe::Running { started: None };
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(1) => ProcessProbe::Running { started: None }, // EPERM: exists
            Some(3) => ProcessProbe::Gone,                      // ESRCH
            _ => ProcessProbe::Unknown,
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        ProcessProbe::Unknown
    }
}

/// Slack for clock granularity when comparing a process start time with a
/// file's birth (R5-S5).
pub const START_TIME_SLACK: std::time::Duration = std::time::Duration::from_secs(2);

/// R5-S5 (pure): may the process `probe` describes still be the writer of
/// a file born at `born`? A live process that started AFTER the file was
/// born is a recycled pid, not the writer; one whose start time is unknown
/// is assumed to be the writer (never delete a live job's file).
pub fn writer_may_be_alive(probe: ProcessProbe, born: Option<SystemTime>) -> bool {
    match probe {
        ProcessProbe::Gone => false,
        ProcessProbe::Unknown | ProcessProbe::Running { started: None } => true,
        ProcessProbe::Running {
            started: Some(started),
        } => born.is_none_or(|born| started <= born + START_TIME_SLACK),
    }
}

/// R13-S4/P17: this PC's identity for the finalize journal and its lane
/// entries — `sha256("frametrace-host-v1" NUL machine-id NUL hostname)`,
/// first 32 hex digits. The machine id is the Windows `MachineGuid`
/// (`HKLM\SOFTWARE\Microsoft\Cryptography`), `/etc/machine-id` on Linux;
/// macOS and others use the host name alone. It never names the user and
/// is never written to this PC's configuration, so two PCs sharing a case
/// folder over the network tell their journals apart. Computed once per
/// process.
///
/// R14-S7: on Windows, where every installation has a `MachineGuid`, an
/// unreadable one makes the id [`HOST_ID_UNKNOWN`] — such a PC never
/// recovers a finalize journal (it cannot tell its own from another
/// PC's). Linux without `/etc/machine-id` (minimal containers) and macOS
/// (no machine id read here) fall back to the host name alone. Limits
/// (docs/audit-hmac-design.md): a cloned VM / disk image keeps the
/// MachineGuid and, if not renamed, the host name, so two clones share an
/// id. The host-local open-finalize record (R14 P18) is what a recovery
/// really relies on.
pub fn host_id() -> String {
    static HOST_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST_ID
        .get_or_init(|| {
            let machine = machine_id();
            if machine.is_none() && cfg!(windows) {
                return HOST_ID_UNKNOWN.to_string();
            }
            let material = format!(
                "frametrace-host-v1\0{}\0{}",
                machine.unwrap_or_default(),
                host_name().unwrap_or_default().to_lowercase()
            );
            crate::sha256::digest_bytes(material.as_bytes())[..32].to_string()
        })
        .clone()
}

/// R14-S7: [`host_id`] of a PC whose machine id could not be read.
pub const HOST_ID_UNKNOWN: &str = "unknown";

/// The host name (Windows `GetComputerNameExW` physical DNS host name —
/// R14-S7: R13 read the `COMPUTERNAME` environment variable, which any
/// parent process can set; else `gethostname`).
pub fn host_name() -> Option<String> {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::{
            ComputerNamePhysicalDnsHostname, GetComputerNameExW,
        };
        let mut buffer = vec![0u16; 512];
        let mut size = buffer.len() as u32;
        // SAFETY: the buffer holds `size` wide chars; the API writes at
        // most that many and sets `size` to the length written.
        let read = unsafe {
            GetComputerNameExW(
                ComputerNamePhysicalDnsHostname,
                Some(windows::core::PWSTR(buffer.as_mut_ptr())),
                &mut size,
            )
        };
        read.ok()?;
        let len = (size as usize).min(buffer.len());
        let name = String::from_utf16_lossy(&buffer[..len]).trim().to_string();
        (!name.is_empty()).then_some(name)
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn gethostname(name: *mut u8, len: usize) -> i32;
        }
        let mut buffer = [0u8; 256];
        // SAFETY: the buffer is writable for its whole length.
        if unsafe { gethostname(buffer.as_mut_ptr(), buffer.len()) } != 0 {
            return None;
        }
        let len = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        let name = String::from_utf8_lossy(&buffer[..len]).trim().to_string();
        (!name.is_empty()).then_some(name)
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

fn machine_id() -> Option<String> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
        use windows::core::{HSTRING, PCWSTR};
        let subkey = HSTRING::from(r"SOFTWARE\Microsoft\Cryptography");
        let value = HSTRING::from("MachineGuid");
        let mut buffer = vec![0u16; 128];
        let mut size: u32 = (buffer.len() * 2) as u32;
        // SAFETY: a read into a buffer of `size` bytes; the registry API
        // writes at most that many.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                PCWSTR::from_raw(subkey.as_ptr()),
                PCWSTR::from_raw(value.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if status.is_err() {
            return None;
        }
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let id = String::from_utf16_lossy(&buffer[..len]).trim().to_string();
        (!id.is_empty()).then_some(id)
    }
    #[cfg(target_os = "linux")]
    {
        ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .iter()
            .find_map(|path| fs::read_to_string(path).ok())
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

/// R13-S4: the executable file name of process `pid` (`frametrace-app.exe`,
/// …) when the platform tells it (Windows, Linux), lowercased.
pub fn process_image_name(pid: u32) -> Option<String> {
    process_image_path(pid)
        .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().to_lowercase()))
}

/// R15-S4: the full path of process `pid`'s executable when the platform
/// tells it (Windows `QueryFullProcessImageNameW`, Linux `/proc/<pid>/exe`).
pub fn process_image_path(pid: u32) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        };
        // SAFETY: plain Win32 calls; the buffer holds `size` wide chars and
        // the handle is closed below.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buffer = vec![0u16; 1024];
            let mut size = buffer.len() as u32;
            let read = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buffer.as_mut_ptr()),
                &mut size,
            );
            let _ = CloseHandle(handle);
            read.ok()?;
            Some(PathBuf::from(String::from_utf16_lossy(
                &buffer[..size as usize],
            )))
        }
    }
    #[cfg(target_os = "linux")]
    {
        fs::read_link(format!("/proc/{pid}/exe")).ok().map(|exe| {
            let text = exe.to_string_lossy();
            PathBuf::from(text.trim_end_matches(" (deleted)"))
        })
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// R15-S10: the executable file name (lowercased) of process `pid` when
/// that process EXISTS but this process may not inspect it (another
/// account's process: Windows `OpenProcess` → ERROR_ACCESS_DENIED, the
/// name then read from a Toolhelp process snapshot; Linux `/proc/<pid>/exe`
/// → EACCES, the name from `/proc/<pid>/stat`). `None` when the process is
/// gone, inspectable, or its name unknown.
pub fn inaccessible_process_name(pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        };
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        const ERROR_ACCESS_DENIED: i32 = 5;
        if pid == 0 {
            return None;
        }
        // SAFETY: plain Win32 calls; every handle is closed below and the
        // entry's size is set before the first call.
        unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(handle) => {
                    let _ = CloseHandle(handle);
                    return None;
                }
                Err(err) if err.code().0 & 0xFFFF == ERROR_ACCESS_DENIED => {}
                Err(_) => return None,
            }
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut found = None;
            let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
            while more {
                if entry.th32ProcessID == pid {
                    let len = entry
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    found = Some(String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase());
                    break;
                }
                more = Process32NextW(snapshot, &mut entry).is_ok();
            }
            let _ = CloseHandle(snapshot);
            found
        }
    }
    #[cfg(target_os = "linux")]
    {
        match fs::read_link(format!("/proc/{pid}/exe")) {
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                let start = stat.find('(')?;
                let end = stat.rfind(')')?;
                (start < end).then(|| stat[start + 1..end].to_lowercase())
            }
            _ => None,
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// R17-S3: the start time of process `pid`, also for a process this
/// account may not open (another account's): [`probe_process`] first,
/// then on Windows the system process list (`NtQuerySystemInformation`
/// `SystemProcessInformation`, which reports every process's creation time
/// without opening it — the Toolhelp snapshot carries none). Linux reads
/// `/proc/<pid>/stat` in `probe_process` already (world-readable). `None`
/// when unknown.
pub fn process_start_time(pid: u32) -> Option<SystemTime> {
    if let ProcessProbe::Running {
        started: Some(started),
    } = probe_process(pid)
    {
        return Some(started);
    }
    #[cfg(windows)]
    {
        system_process_start_time(pid)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// R17-S3: `CreateTime` of process `pid` from
/// `NtQuerySystemInformation(SystemProcessInformation)` (no handle to the
/// process needed). The entry layout is the documented prefix of
/// `SYSTEM_PROCESS_INFORMATION` (winternl.h: `NextEntryOffset`,
/// `NumberOfThreads`, 48 reserved bytes holding — at their offset 24 —
/// `CreateTime`, then `ImageName`, `BasePriority`, `UniqueProcessId`).
#[cfg(windows)]
pub(crate) fn system_process_start_time(pid: u32) -> Option<SystemTime> {
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQuerySystemInformation(
            class: u32,
            info: *mut std::ffi::c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }
    #[repr(C)]
    #[allow(dead_code)] // layout only: the fields before `create_time` / the id
    struct Entry {
        next_entry_offset: u32,
        number_of_threads: u32,
        working_set_private_size: i64,
        hard_fault_count: u32,
        number_of_threads_high_watermark: u32,
        cycle_time: u64,
        create_time: i64,
        user_time: i64,
        kernel_time: i64,
        image_name_length: u16,
        image_name_maximum_length: u16,
        image_name_buffer: *const u16,
        base_priority: i32,
        unique_process_id: usize,
    }
    const SYSTEM_PROCESS_INFORMATION: u32 = 5;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    const MAX_BYTES: usize = 64 * 1024 * 1024;
    const EPOCH_DIFF_TICKS: u64 = 116_444_736_000_000_000;
    if pid == 0 {
        return None;
    }
    // u64 words: the entries are 8-byte aligned in the buffer.
    let mut buffer: Vec<u64> = vec![0; 64 * 1024];
    loop {
        let bytes = buffer.len() * 8;
        let mut returned = 0u32;
        // SAFETY: the buffer is `bytes` long and writable; the call writes
        // at most that many bytes.
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESS_INFORMATION,
                buffer.as_mut_ptr().cast(),
                bytes as u32,
                &mut returned,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            let wanted = (returned as usize).max(bytes * 2) + 64 * 1024;
            if wanted > MAX_BYTES {
                return None;
            }
            buffer.resize(wanted.div_ceil(8), 0);
            continue;
        }
        if status < 0 {
            return None;
        }
        let filled = (returned as usize).min(bytes);
        let base = buffer.as_ptr().cast::<u8>();
        let mut offset = 0usize;
        while offset + std::mem::size_of::<Entry>() <= filled {
            // SAFETY: the entry lies inside the filled part of the buffer;
            // read unaligned (only plain integers are used).
            let entry: Entry = unsafe { std::ptr::read_unaligned(base.add(offset).cast()) };
            if entry.unique_process_id == pid as usize {
                let ticks = u64::try_from(entry.create_time).ok()?;
                return ticks.checked_sub(EPOCH_DIFF_TICKS).map(|since| {
                    std::time::UNIX_EPOCH + std::time::Duration::from_nanos(since * 100)
                });
            }
            if entry.next_entry_offset == 0 {
                return None;
            }
            offset += entry.next_entry_offset as usize;
        }
        return None;
    }
}

/// R13-S4: who a journal writer is — pid, process start time (unix ms)
/// and executable name — so a later reader can tell the writer from a
/// process that merely reuses its pid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WriterIdentity {
    pub pid: u32,
    pub started_ms: Option<u64>,
    pub image: Option<String>,
}

/// R13-S4: [`WriterIdentity`] of this process.
pub fn current_writer() -> WriterIdentity {
    let pid = std::process::id();
    let started_ms = match probe_process(pid) {
        ProcessProbe::Running {
            started: Some(started),
        } => started
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64),
        _ => None,
    };
    let image = process_image_name(pid).or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().to_lowercase()))
    });
    WriterIdentity {
        pid,
        started_ms,
        image,
    }
}

/// R13-S4 / R14-S3 (pure): is the process `probe` describes (executable
/// `image` now) still the writer `writer`? Only a POSITIVE match counts:
/// the writer's start time was recorded AND the live process's start time
/// is known and within the clock slack of it, AND its executable name is
/// known, is a FrameTrace executable (`frametrace…`) and — when recorded —
/// the writer's. R12 judged by the pid alone ("can't tell" = alive); R13
/// still took an unknown start time as alive for 24 h, so a planted
/// journal naming `pid: 4` (System, whose start time a user process cannot
/// read) and a future `started_unix` blocked the case for a day and
/// refused `--dismiss`. A platform that cannot read process start times
/// (macOS here) therefore never sees a live writer — the busy lease and
/// the exclusive journal creation (R14-S4) are what keep two finalizes of
/// one case apart.
pub fn writer_still_running(
    writer: &WriterIdentity,
    probe: ProcessProbe,
    image: Option<&str>,
) -> bool {
    if writer.pid == 0 {
        return false;
    }
    let Some(now) = image else {
        return false;
    };
    if !is_frametrace_image(now) {
        return false;
    }
    if let Some(recorded) = writer.image.as_deref()
        && !recorded.eq_ignore_ascii_case(now)
    {
        return false;
    }
    let (
        ProcessProbe::Running {
            started: Some(started),
        },
        Some(recorded),
    ) = (probe, writer.started_ms)
    else {
        return false;
    };
    let started_ms = started
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    started_ms.abs_diff(recorded) <= START_TIME_SLACK.as_millis() as u64
}

/// R14-S3: whether an executable file name is one of FrameTrace's
/// (`frametrace.exe`, `frametrace-app.exe`, `frametrace` …).
pub fn is_frametrace_image(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("frametrace")
}

/// Standard (padded) base64 of `bytes` (CSP hash sources).
pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        csv_safe_cell, run_with_timeout, sanitize_filename, unique_dir, unique_path,
        write_text_atomic,
    };

    /// R6-S1: only one JSON value ever reaches a generated script; text
    /// around it (or anything that is not JSON) becomes `null`, and
    /// string content cannot close the script element.
    #[test]
    fn json_for_script_embeds_json_values_only() {
        use super::{json_for_script, script_json_input};
        assert_eq!(json_for_script(r#"{"a":1}"#), r#"{"a":1}"#);
        assert_eq!(json_for_script(r#"{"a":1};alert(1)//"#), "null");
        assert_eq!(json_for_script("1,alert(1)"), "null");
        assert_eq!(json_for_script(""), "null");
        assert_eq!(
            json_for_script("{\"z\":1,\"a\":\"</script>&\u{2028}\"}"),
            "{\"z\":1,\"a\":\"\\u003c/script\\u003e\\u0026\\u2028\"}"
        );
        assert!(script_json_input("case.json", "{} x").is_err());
        assert_eq!(script_json_input("case.json", " [1, 2] ").unwrap(), "[1,2]");
        // R7-S1: whitespace inside strings stays, outside goes; the
        // script escapes apply inside strings only.
        assert_eq!(
            json_for_script("{ \"a b\" : [ \"x <y>\\u0041\\\"&\" , 2 ] ,\n\"c\":\"\u{2029}\" }"),
            "{\"a b\":[\"x \\u003cy\\u003e\\u0041\\\"\\u0026\",2],\"c\":\"\\u2029\"}"
        );
        assert_eq!(json_for_script("[1] [2]"), "null");
        assert_eq!(json_for_script("\"\u{1}\""), "null");
    }

    /// R8-S3: a `__proto__` key (any spelling) is the prototype setter in
    /// a JS object literal, and a duplicate one is a SyntaxError — a
    /// crafted `case.json` blanked every generated page. Such keys are now
    /// computed keys: the page script still parses, and evaluates to the
    /// same value `JSON.parse` gives (own properties, last value wins,
    /// plain prototype). `script_json_input` output stays JSON.
    #[test]
    fn json_for_script_neutralises_proto_keys() {
        use super::{json_for_script, script_json_input};
        let input = r#"{"__proto__":{"x":1},"a":[{"__proto__":2,"__proto__":3}],"b":"__proto__","__proto__x":4,"c":{"k":"__proto__"},"__proto__":[5]}"#;
        let embedded = json_for_script(input);
        assert_eq!(
            embedded,
            r#"{["__proto__"]:{"x":1},"a":[{["__proto__"]:2,["__proto__"]:3}],"b":"__proto__","__proto__x":4,"c":{"k":"__proto__"},["__proto__"]:[5]}"#
        );
        assert_eq!(
            script_json_input("case.json", input).unwrap(),
            input,
            "the intermediate form stays JSON"
        );
        assert_eq!(json_for_script(r#"["__proto__"]"#), r#"["__proto__"]"#);
        // Same JSON value as JSON.parse, and a script that parses.
        let probe = format!(
            "const v = {embedded};\nconst p = JSON.parse({});\nconst plain = o => o === null || typeof o !== 'object' || (Object.getPrototypeOf(o) === (Array.isArray(o) ? Array.prototype : Object.prototype) && Object.values(o).every(plain));\nprocess.stdout.write(JSON.stringify([JSON.stringify(v) === JSON.stringify(p), plain(v), Object.keys(v).join(',')]));",
            serde_json::to_string(input).unwrap()
        );
        let Ok(output) = std::process::Command::new("node")
            .arg("-e")
            .arg(&probe)
            .output()
        else {
            eprintln!("node not found; script evaluation skipped");
            return;
        };
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            r#"[true,true,"__proto__,a,b,__proto__x,c"]"#
        );
    }

    /// R7-S2: the streaming inline-script scan gives the same `script-src`
    /// as the whole-page scan for a handler-free page, however the page is
    /// cut into chunks (tags and `</script` split across chunk edges,
    /// upper case, external scripts, a `<` inside a body).
    #[test]
    fn streaming_inline_script_scan_matches_the_page_scan() {
        use super::generated_pages::{InlineScriptScan, inline_script_sources};
        let page = "<html><head><SCRIPT>let a = 1 < 2;</SCRIPT><script src=\"x.js\"></script>\
            <script type=\"module\">if (a </ 3) {}</script ></head><body><p>x</p>\
            <script>\nconst s = \"</scr\" + \"ipt>\";\n</sCrIpT><scriptx>y</script>\
            <script>never closed";
        let whole = inline_script_sources(page).unwrap();
        assert_eq!(whole.matches("'sha256-").count(), 4, "{whole}");
        for size in [1usize, 2, 3, 5, 7, 8, 9, 13, 64, page.len()] {
            let mut scan = InlineScriptScan::new();
            for chunk in page.as_bytes().chunks(size) {
                scan.feed(chunk);
            }
            assert_eq!(scan.finish(), whole, "chunk size {size}");
        }
    }

    /// R7-S1: a multi-MB input is validated without a `Value` tree (the
    /// raw path accepted it, and numbers keep the text they were written
    /// with — a tree turns `1e2` into `100.0` and a 30-digit integer into a
    /// float) and gives the same embedding the R6 tree path gave for
    /// ordinary JSON.
    #[test]
    fn json_for_script_validates_large_inputs_without_a_tree() {
        use super::{RAW_JSON_VALIDATIONS, json_for_script};
        let videos: Vec<serde_json::Value> = (0..40_000)
            .map(|i| {
                serde_json::json!({
                    "id": format!("vid_{i:06}"),
                    "relative_path": format!("CH1/</script><b>{i}</b> & \u{2028}.mp4"),
                    "size_bytes": i * 1024,
                    "duration_seconds": 1.5 + i as f64,
                    "ok": i % 2 == 0,
                    "nested": {"tags": ["a", "b"], "none": null},
                })
            })
            .collect();
        let pretty =
            serde_json::to_string_pretty(&serde_json::json!({ "videos": videos })).unwrap();
        assert!(pretty.len() > 8 * 1024 * 1024, "{}", pretty.len());
        // The R6 embedding: parse into a tree, re-serialize, escape.
        let tree: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        let r6 = serde_json::to_string(&tree)
            .unwrap()
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026")
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029");
        let before = RAW_JSON_VALIDATIONS.with(std::cell::Cell::get);
        let embedded = json_for_script(&pretty);
        assert_eq!(RAW_JSON_VALIDATIONS.with(std::cell::Cell::get), before + 1);
        assert!(
            embedded == r6,
            "multi-MB output differs from the R6 embedding"
        );
        assert!(!embedded.contains("</script"));
        // The raw path keeps number text a tree would rewrite.
        let numbers = format!(
            "{{\"n\":1e2,\"big\":123456789012345678901234567890,\"pad\":\"{}\"}}",
            "x".repeat(2 << 20)
        );
        let embedded = json_for_script(&numbers);
        assert!(embedded.starts_with("{\"n\":1e2,\"big\":123456789012345678901234567890,"));
        // A crafted tail after a multi-MB value is still refused.
        assert_eq!(json_for_script(&format!("{pretty};alert(1)")), "null");
    }

    /// R5 (P3): a non-Unicode name keeps its exact UTF-16 units when the
    /// verbatim prefix is removed (before: `to_string_lossy` turned the
    /// lone surrogate into U+FFFD — another file name).
    #[cfg(windows)]
    #[test]
    fn extended_prefix_strip_is_lossless_for_non_unicode_names() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
        let mut name = wide(r"\\?\C:\cases\clip");
        name.push(0xD800); // lone surrogate: not valid Unicode
        name.extend(wide(".mp4"));
        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&name));
        let stripped = super::strip_windows_extended_prefix(&path);
        let mut expected = wide(r"C:\cases\clip");
        expected.push(0xD800);
        expected.extend(wide(".mp4"));
        assert_eq!(
            stripped.as_os_str().encode_wide().collect::<Vec<u16>>(),
            expected
        );
        let mut unc = wide(r"\\?\UNC\srv\share\x");
        unc.push(0xDC00);
        let stripped = super::strip_windows_extended_prefix(&std::path::PathBuf::from(
            std::ffi::OsString::from_wide(&unc),
        ));
        let mut expected = wide(r"\\srv\share\x");
        expected.push(0xDC00);
        assert_eq!(
            stripped.as_os_str().encode_wide().collect::<Vec<u16>>(),
            expected
        );
    }

    #[test]
    fn sanitize_filename_blocks_hostile_names() {
        assert_eq!(sanitize_filename("vid_000001"), "vid_000001");
        assert_eq!(sanitize_filename("a/b:c.mp4"), "a_b_c_mp4");
        assert_eq!(sanitize_filename("../../etc"), "______etc");
        assert_eq!(sanitize_filename("name. "), "name__");
        assert_eq!(sanitize_filename("a\u{0}b\n\u{7f}"), "a_b__");
        assert_eq!(sanitize_filename(""), "clip");
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("nul"), "_nul");
        assert_eq!(sanitize_filename("Com1"), "_Com1");
        assert_eq!(sanitize_filename("lpt9"), "_lpt9");
        assert_eq!(sanitize_filename("COM0"), "COM0");
        assert_eq!(sanitize_filename("console"), "console");
        assert_eq!(sanitize_filename("한글.avi"), "___avi");
        // Not capped: existing artifacts are located by re-sanitizing.
        assert_eq!(sanitize_filename(&"x".repeat(500)).len(), 500);
    }

    #[test]
    fn csv_safe_cell_neutralizes_formula_prefixes() {
        assert_eq!(csv_safe_cell("=1+1"), "'=1+1");
        assert_eq!(csv_safe_cell("+cmd|' /C calc'!A0"), "'+cmd|' /C calc'!A0");
        assert_eq!(csv_safe_cell("-2+3"), "'-2+3");
        assert_eq!(csv_safe_cell("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(csv_safe_cell("\t=1"), "'\t=1");
        assert_eq!(csv_safe_cell("\r=1"), "\"'\r=1\"");
        // Neutralized and quoted when it also needs RFC 4180 quoting.
        assert_eq!(
            csv_safe_cell("=HYPERLINK(\"http://x\",\"y\")"),
            "\"'=HYPERLINK(\"\"http://x\"\",\"\"y\"\")\""
        );
        // Ordinary values and plain numbers are untouched.
        assert_eq!(
            csv_safe_cell("블랙박스_20240101.mp4"),
            "블랙박스_20240101.mp4"
        );
        assert_eq!(csv_safe_cell("-3600"), "-3600");
        assert_eq!(csv_safe_cell("+1.5e3"), "+1.5e3");
        assert_eq!(csv_safe_cell(""), "");
        assert_eq!(csv_safe_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_safe_cell("a\"b"), "\"a\"\"b\"");
    }

    /// Guards the assumption that std's Windows spawn path does not leak one
    /// child's inheritable stdio pipe ends into a sibling spawned concurrently
    /// from another thread. std (1.94, `sys/process/windows.rs`) creates the
    /// child-side pipe ends (`OBJ_INHERIT`) and calls `CreateProcessW` under a
    /// process-wide `CREATE_PROCESS_LOCK`, and drops those ends before the
    /// lock is released. If that ever regressed, a short child's stdout reader
    /// would not reach EOF after the child exits, because a long-running
    /// sibling would hold a copy of its write end — reader joins after child
    /// exit (cancellable job steps, run_with_timeout, parallel thumbnails)
    /// would then hang until the unrelated sibling exits.
    #[cfg(windows)]
    #[test]
    fn concurrent_spawns_do_not_leak_stdout_pipes_into_siblings() {
        use std::io::Read;
        use std::process::{Command, Stdio};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;

        let system32 = std::path::PathBuf::from(
            std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()),
        )
        .join("System32");
        let ping = system32.join("PING.EXE");
        let cmd = system32.join("cmd.exe");

        // Long-running siblings, spawned continuously from other threads while
        // the short children below are spawned, so the spawn windows overlap.
        let stop = Arc::new(AtomicBool::new(false));
        let long_running: Arc<Mutex<Vec<std::process::Child>>> = Arc::default();
        let spawners: Vec<_> = (0..2)
            .map(|_| {
                let stop = Arc::clone(&stop);
                let long_running = Arc::clone(&long_running);
                let ping = ping.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        let child = Command::new(&ping)
                            .args(["-n", "120", "127.0.0.1"])
                            .stdin(Stdio::null())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped())
                            .spawn()
                            .expect("spawn long-running sibling");
                        let mut guard = long_running.lock().unwrap();
                        guard.push(child);
                        if guard.len() >= 24 {
                            break;
                        }
                    }
                })
            })
            .collect();

        let mut outcome = Ok(());
        for round in 0..40 {
            let mut short = Command::new(&cmd)
                .args(["/D", "/C", "echo short"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn short child");
            let mut stdout = short.stdout.take().unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let result = stdout.read_to_end(&mut buf).map(|_| buf);
                let _ = tx.send(result);
            });
            short.wait().expect("wait short child");
            // The short child has exited; its stdout reader must see EOF
            // promptly. A leaked write end held by a 120 s sibling would keep
            // this blocked far past the bound.
            match rx.recv_timeout(Duration::from_secs(15)) {
                Ok(Ok(buf)) => {
                    assert!(String::from_utf8_lossy(&buf).contains("short"));
                }
                Ok(Err(err)) => {
                    outcome = Err(format!("round {round}: reader failed: {err}"));
                    break;
                }
                Err(_) => {
                    outcome = Err(format!(
                        "round {round}: stdout reader did not reach EOF within 15s after the \
                         child exited — a concurrently spawned sibling inherited its pipe"
                    ));
                    break;
                }
            }
        }

        stop.store(true, Ordering::SeqCst);
        for spawner in spawners {
            spawner.join().unwrap();
        }
        let mut siblings = std::mem::take(&mut *long_running.lock().unwrap());
        assert!(!siblings.is_empty(), "no long-running sibling was spawned");
        for child in &mut siblings {
            let _ = child.kill();
            let _ = child.wait();
        }
        outcome.unwrap();
    }

    #[test]
    fn run_with_timeout_kills_stuck_children() {
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = std::process::Command::new("ping");
            command.args(["-n", "30", "127.0.0.1"]);
            command
        };
        #[cfg(not(target_os = "windows"))]
        let mut command = {
            let mut command = std::process::Command::new("sleep");
            command.arg("30");
            command
        };
        let error = run_with_timeout(&mut command, Some(1)).unwrap_err();
        assert!(error.contains("did not finish within 1s"), "{error}");
    }

    /// `CREATE_NO_WINDOW` must not break spawning a console child or
    /// capturing its output (the flag detaches it from any console).
    #[cfg(windows)]
    #[test]
    fn hide_console_child_still_runs_and_pipes_output() {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "echo hidden-ok"]);
        let output = super::hide_console(&mut command).output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hidden-ok"));
        // Through run_with_timeout as well (the common path).
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "echo timed-ok"]);
        let output = run_with_timeout(&mut command, Some(30)).unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("timed-ok"));
    }

    /// Every production `Command::new` must reach the OS through
    /// `hide_console` (directly, or via `run_with_timeout` /
    /// `spawn_tracked`), otherwise the windowed app flashes a console per
    /// external tool. Files that only spawn in tests are skipped; files
    /// listed in PENDING are owned by another fix track and are expected
    /// to leave this list as those land.
    #[test]
    fn every_command_spawn_site_hides_the_console() {
        const PENDING: &[&str] = &[];
        const DEFINES_HELPERS: &[&str] = &["util.rs", "serve/proc.rs"];
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src.clone()];
        let mut offenders = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&src)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let name = path.file_name().unwrap().to_string_lossy();
                // Test-only files (`test_support.rs` is `#[cfg(test)]` in
                // lib.rs as a whole, so it has no inner `#[cfg(test)]`).
                if name == "tests.rs"
                    || name.starts_with("tests_")
                    || name == "test_support.rs"
                    || rel.starts_with("tests/")
                {
                    continue;
                }
                if DEFINES_HELPERS.contains(&rel.as_str()) || PENDING.contains(&rel.as_str()) {
                    continue;
                }
                let text = fs::read_to_string(&path).unwrap();
                // Production code only: drop everything from the first
                // `#[cfg(test)]` module on.
                let production = text.split("#[cfg(test)]").next().unwrap_or("");
                if !production.contains("Command::new(") {
                    continue;
                }
                let covered = production.contains("hide_console(")
                    || production.contains("hide_console_with_flags(")
                    || production.contains("run_with_timeout(")
                    || production.contains("run_with_timeout_detailed(")
                    || production.contains("spawn_tracked(");
                if !covered {
                    offenders.push(rel);
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "Command::new without hide_console/run_with_timeout/spawn_tracked in: {offenders:?}"
        );
    }

    #[test]
    fn atomic_write_replaces_existing_content_without_temp_leftovers() {
        let dir = std::env::temp_dir().join(format!("ft-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        write_text_atomic(&path, "first").unwrap();
        write_text_atomic(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp-")
            })
            .collect();
        assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::{
        canonicalize_display, compact_json_value_if_well_formed, json_escape, path_to_file_url,
        strip_windows_extended_prefix,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn escapes_json_control_characters() {
        assert_eq!(json_escape("a\"b\\c\n"), "a\\\"b\\\\c\\n");
    }

    #[test]
    fn strips_windows_extended_prefixes() {
        assert_eq!(
            strip_windows_extended_prefix(Path::new(r"\\?\C:\Cases\a.mp4")),
            PathBuf::from(r"C:\Cases\a.mp4")
        );
        assert_eq!(
            strip_windows_extended_prefix(Path::new(r"\\?\UNC\server\share\a.mp4")),
            PathBuf::from(r"\\server\share\a.mp4")
        );
        assert_eq!(
            strip_windows_extended_prefix(Path::new(r"C:\Cases\a.mp4")),
            PathBuf::from(r"C:\Cases\a.mp4")
        );
    }

    #[test]
    fn canonicalizes_to_display_paths() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-canonical-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let canonical = canonicalize_display(&dir).unwrap();
        assert!(
            !canonical.as_os_str().to_string_lossy().starts_with(r"\\?\"),
            "unexpected extended prefix: {canonical:?}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn creates_file_url() {
        let url = path_to_file_url(Path::new("/tmp/a b.mp4"));
        assert!(url.starts_with("file://"));
        assert!(url.contains("a%20b.mp4"));
    }

    #[cfg(windows)]
    #[test]
    fn file_url_never_contains_extended_prefix() {
        let url = path_to_file_url(Path::new(r"C:\Windows\System32\drivers\etc\hosts"));
        assert!(
            url.starts_with("file:///C:/Windows"),
            "unexpected url: {url}"
        );
        assert!(!url.contains("%3F"), "unexpected encoded prefix: {url}");
    }

    #[test]
    fn compacts_only_well_formed_json_values() {
        assert_eq!(
            compact_json_value_if_well_formed("{\n  \"a\": \"x y\"\n}").as_deref(),
            Some("{\"a\":\"x y\"}")
        );
        assert_eq!(compact_json_value_if_well_formed("not json"), None);
        assert_eq!(compact_json_value_if_well_formed("{\"a\":1"), None);
    }

    #[test]
    fn creates_unique_path_with_suffix() {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-unique-path-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        fs::write(&path, b"one").unwrap();
        assert_eq!(unique_path(&path), dir.join("clip_001.mp4"));
        fs::write(dir.join("clip_001.mp4"), b"two").unwrap();
        assert_eq!(unique_path(&path), dir.join("clip_002.mp4"));
        let _ = fs::remove_dir_all(dir);
    }

    /// Two racers must never be handed the same artifact path: the loser of
    /// the `O_EXCL` reservation gets the next candidate instead of a path
    /// that would overwrite the winner's recovered evidence.
    #[test]
    fn concurrent_unique_path_reservations_never_collide() {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-unique-race-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recovered.bin");

        let mut handles = Vec::new();
        for _ in 0..8 {
            let path = path.clone();
            handles.push(std::thread::spawn(move || unique_path(&path)));
        }
        let claimed: Vec<PathBuf> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let mut unique_names: Vec<String> = claimed
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        unique_names.sort();
        unique_names.dedup();
        // All 8 racers received distinct names (no overwrite hazard).
        assert_eq!(unique_names.len(), 8, "{claimed:?}");
        // Every claimed path exists on disk (reservation held).
        for p in &claimed {
            assert!(p.is_file(), "{p:?}");
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// Directory targets (package-case output) claim via create_dir, so a
    /// caller's create_dir_all on the result never collides with a
    /// placeholder-file reservation.
    #[test]
    fn unique_dir_yields_creatable_directory_paths() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-unique-dir-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let base = dir.join("package_123");

        let first = unique_dir(&base);
        assert!(first.is_dir(), "claim must create the directory: {first:?}");
        // Caller-style population on the claimed path must succeed.
        fs::write(first.join("manifest.json"), b"{}").unwrap();

        let second = unique_dir(&base);
        assert!(second.is_dir());
        assert_ne!(first, second, "racing package runs must not share output");
        fs::write(second.join("manifest.json"), b"{}").unwrap();
        let _ = fs::remove_dir_all(dir);
    }
}
