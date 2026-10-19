//! Windows archive installer: built-in curl.exe download (manual,
//! allowlist-checked redirects) → size + SHA-256 check → bsdtar listing
//! check → extraction of the manifest's members only → tree check →
//! all-or-nothing move into `tools/bin`.

use super::*;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

/// Redirect hops followed (GitHub release assets use one).
pub(crate) const MAX_HOPS: usize = 5;
/// Hard bound on one transfer (curl's own `--max-time` is the same).
const MAX_TRANSFER_SECS: u64 = 3600;
/// Listing/extraction bound.
const TAR_TIMEOUT_SECS: u64 = 600;

/// `%SystemRoot%\System32\<name>` — never a bare name (T4).
pub(crate) fn system32_tool(name: &str) -> Result<PathBuf, String> {
    let root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)
        .ok_or("SystemRoot is not set — cannot locate the Windows system tools")?;
    if !root.is_absolute() {
        return Err(format!("SystemRoot is not absolute: {}", root.display()));
    }
    let path = root.join("System32").join(name);
    if !path.is_file() {
        return Err(format!(
            "{name} not found at {} (Windows 10 1803 or later ships it)",
            path.display()
        ));
    }
    Ok(path)
}

pub(crate) fn install_archive(
    tool: &ToolEntry,
    dirs: &InstallDirs,
    net: &NetPolicy,
    progress: &Progress,
) -> Result<InstallOutcome, String> {
    let url = tool.url.as_deref().ok_or("no url pinned")?;
    let size = tool.size.ok_or("no size pinned")?;
    let sha256 = tool.sha256.as_deref().ok_or("no sha256 pinned")?;
    check_url(url, net)?;
    let curl = system32_tool("curl.exe")?;
    let tar = system32_tool("tar.exe")?;
    // Staging lives under tools/ so the final renames stay on one volume.
    let staging = StagingDir::create(&dirs.tools_dir, ".staging-")?;
    let archive = staging.path().join("download.part");

    progress.set_stage(Stage::Download);
    progress.set_bytes(0, size);
    download(&curl, url, &archive, size, net, progress)?;

    progress.set_stage(Stage::Verify);
    verify_file(&archive, size, sha256)?;

    progress.set_stage(Stage::Extract);
    let members: Vec<&str> = tool
        .files
        .iter()
        .chain(&tool.license_files)
        .map(|map| map.from.as_str())
        .collect();
    let names = tar_lines(&tar, &["-tf"], &archive)?;
    let verbose = tar_lines(&tar, &["-tvf"], &archive)?;
    check_listing(&names, &verbose, &members)?;
    let out = staging.path().join("x");
    std::fs::create_dir(&out).map_err(|err| format!("cannot create {}: {err}", out.display()))?;
    extract_members(&tar, &archive, &out, &members)?;
    check_extracted_tree(&out, &members)?;

    progress.set_stage(Stage::Install);
    let mut moves = Vec::new();
    for map in &tool.files {
        moves.push((out.join(&map.from), dirs.bin().join(&map.to)));
    }
    for map in &tool.license_files {
        moves.push((out.join(&map.from), dirs.licenses().join(&map.to)));
    }
    move_all_or_nothing(&moves, &staging.path().join("backup"))?;
    Ok(InstallOutcome {
        installed_files: moves.into_iter().map(|(_, dest)| dest).collect(),
        audit_warning: None,
    })
}

/// Downloads `url` to `out`, following at most [`MAX_HOPS`] redirects
/// itself so every hop is checked against the allowlist before curl
/// connects to it.
pub(crate) fn download(
    curl: &Path,
    url: &str,
    out: &Path,
    size: u64,
    net: &NetPolicy,
    progress: &Progress,
) -> Result<(), String> {
    let mut current = url.to_string();
    for _ in 0..=MAX_HOPS {
        let parsed = check_url(&current, net)?;
        let _ = std::fs::remove_file(out);
        let (code, location) = curl_hop(curl, &current, parsed.https, out, size, progress)?;
        match code {
            200 => return Ok(()),
            301 | 302 | 303 | 307 | 308 => {
                if location.is_empty() {
                    return Err(format!("download failed: HTTP {code} without a Location"));
                }
                current = location;
            }
            other => return Err(format!("download failed: HTTP {other}")),
        }
    }
    Err(format!("download failed: more than {MAX_HOPS} redirects"))
}

fn curl_hop(
    curl: &Path,
    url: &str,
    https: bool,
    out: &Path,
    size: u64,
    progress: &Progress,
) -> Result<(u16, String), String> {
    let mut command = Command::new(curl);
    // `--disable` must come first: no ~/.curlrc may add options
    // (`--insecure`, `--location`, ...) behind our back.
    command
        .arg("--disable")
        .args(["--silent", "--show-error", "--fail"])
        .args(["--proto", if https { "=https" } else { "=http" }])
        .arg("--tlsv1.2")
        .args(["--max-redirs", "0"])
        .args(["--connect-timeout", "30"])
        .args(["--speed-limit", "1024", "--speed-time", "60"])
        .args(["--max-time", &MAX_TRANSFER_SECS.to_string()])
        .args(["--max-filesize", &size.to_string()])
        .arg("--output")
        .arg(out)
        .args(["--write-out", "%{http_code} %{redirect_url}"])
        .args(["--url", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::util::hide_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|err| format!("cannot start {}: {err}", curl.display()))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut text);
        text
    });
    let err_reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut text);
        String::from_utf8_lossy(&text).into_owned()
    });
    let started = Instant::now();
    let status = loop {
        match child.wait_timeout(Duration::from_millis(200)) {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let done = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
                progress.set_bytes(done.min(size), size);
                let expired = started.elapsed() > Duration::from_secs(MAX_TRANSFER_SECS + 60);
                if progress.cancelled() || expired {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(if expired {
                        "download timed out".to_string()
                    } else {
                        "download cancelled".to_string()
                    });
                }
            }
            Err(err) => {
                let _ = child.kill();
                return Err(format!("waiting for curl failed: {err}"));
            }
        }
    };
    let written = out_reader.join().unwrap_or_default();
    let errors = err_reader.join().unwrap_or_default();
    if !status.success() {
        let code = status.code().unwrap_or(-1);
        let what = match code {
            63 => "the file is larger than the pinned size".to_string(),
            22 => format!(
                "server error ({})",
                written.split(' ').next().unwrap_or("?")
            ),
            _ => errors.trim().to_string(),
        };
        return Err(format!("download failed (curl exit {code}): {what}"));
    }
    let (code, location) = written
        .trim()
        .split_once(' ')
        .unwrap_or((written.trim(), ""));
    let code: u16 = code
        .parse()
        .map_err(|_| format!("download failed: unexpected curl output {written:?}"))?;
    Ok((code, location.trim().to_string()))
}

/// Size, then SHA-256, against the manifest — before anything is
/// extracted (T1).
pub(crate) fn verify_file(path: &Path, size: u64, sha256: &str) -> Result<(), String> {
    let len = std::fs::metadata(path)
        .map_err(|err| format!("downloaded file missing: {err}"))?
        .len();
    if len != size {
        return Err(format!(
            "size mismatch: downloaded {len} bytes, the manifest pins {size} — not installed"
        ));
    }
    let file = std::fs::File::open(path).map_err(|err| format!("cannot read download: {err}"))?;
    let actual =
        crate::sha256::digest_reader(file).map_err(|err| format!("cannot hash download: {err}"))?;
    if actual != sha256 {
        return Err(format!(
            "SHA-256 mismatch: got {actual}, the manifest pins {sha256} — not installed"
        ));
    }
    Ok(())
}

fn tar_lines(tar: &Path, mode: &[&str], archive: &Path) -> Result<Vec<String>, String> {
    let mut command = Command::new(tar);
    command.args(mode).arg(archive);
    let output = crate::util::run_with_timeout(&mut command, Some(TAR_TIMEOUT_SECS))?;
    if !output.status.success() {
        return Err(format!(
            "archive listing failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .filter(|line| !line.is_empty())
        .collect())
}

fn extract_members(tar: &Path, archive: &Path, out: &Path, members: &[&str]) -> Result<(), String> {
    let mut command = Command::new(tar);
    command.arg("-x").arg("-f").arg(archive).arg("-C").arg(out);
    command.args(members);
    let output = crate::util::run_with_timeout(&mut command, Some(TAR_TIMEOUT_SECS))?;
    if !output.status.success() {
        return Err(format!(
            "archive extraction failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// After extraction: only regular files and directories, no reparse point
/// (symlink/junction), and exactly the manifest's members.
pub(crate) fn check_extracted_tree(out: &Path, members: &[&str]) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let mut found = Vec::new();
    let mut stack = vec![out.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|err| format!("cannot read extraction: {err}"))?;
        for entry in entries {
            let path = entry
                .map_err(|err| format!("cannot read extraction: {err}"))?
                .path();
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|err| format!("cannot inspect extraction: {err}"))?;
            if meta.file_type().is_symlink()
                || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(format!(
                    "archive refused: extracted a link {}",
                    path.display()
                ));
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(out)
                    .map_err(|_| "extraction escaped its directory".to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                if !members.contains(&rel.as_str()) {
                    return Err(format!("archive refused: unexpected extracted file {rel}"));
                }
                found.push(rel);
            } else {
                return Err(format!("archive refused: special file {}", path.display()));
            }
        }
    }
    for member in members {
        if !found.iter().any(|f| f == member) {
            return Err(format!("archive refused: {member} was not extracted"));
        }
    }
    Ok(())
}
