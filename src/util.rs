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
    let program = command.get_program().to_string_lossy().to_string();
    hide_console(command);
    let Some(secs) = timeout_secs else {
        return command
            .output()
            .map_err(|err| format!("failed to run {program}: {err}"));
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|err| format!("failed to run {program}: {err}"))?;
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
        .map_err(|err| format!("failed to wait for {program}: {err}"))?;
    match status {
        Some(status) => Ok(Output {
            status,
            stdout: stdout_reader.join().unwrap_or_default(),
            stderr: stderr_reader.join().unwrap_or_default(),
        }),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!(
                "{program} did not finish within {secs}s and was terminated (retry with a larger --timeout)"
            ))
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

/// Writes state files (case manifest, indexes) so a crash can never leave a
/// truncated half-file: the payload lands in a sibling temp file first, is
/// fsynced, and only then atomically renamed over the target.
pub fn write_text_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let temp = path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    {
        let mut file = fs::File::create(&temp)?;
        io::Write::write_all(&mut file, text.as_bytes())?;
        io::Write::flush(&mut file)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    // The rename itself is only durable once the directory entry is synced;
    // without this a power loss can roll back the "atomic" swap.
    sync_parent_directory(path);
    Ok(())
}

/// Best-effort directory fsync so file creation/rename survive power loss.
/// Not all platforms/filesystems support opening directories read-only for
/// sync (Windows historically returns `ACCESS_DENIED`); failures are ignored
/// because the crash-durability gain is incremental, not load-bearing.
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
    #[cfg(not(unix))]
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
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest.to_string())
    } else {
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

pub fn json_for_script(input: &str) -> String {
    input
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
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

#[cfg(test)]
mod tests {
    use super::{
        csv_safe_cell, run_with_timeout, sanitize_filename, unique_dir, unique_path,
        write_text_atomic,
    };

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
                if name == "tests.rs" || name.starts_with("tests_") || rel.starts_with("tests/") {
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
