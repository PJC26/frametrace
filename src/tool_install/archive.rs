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
    // Fix round 11 L2 (narrowed in round 12 L3): the paths tar.exe sees
    // must survive the ANSI code page (8.3 short form if needed), or the
    // install is refused here. curl gets the same form — harmless for it.
    let work = ansi_safe_dir(staging.path())?;
    let archive = work.join("download.part");

    progress.set_stage(Stage::Download);
    progress.set_bytes(0, size);
    download(&curl, url, &archive, size, net, progress)?;

    progress.check_cancel()?;
    progress.set_stage(Stage::Verify);
    verify_file(&archive, size, sha256)?;

    progress.check_cancel()?;
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
    let out = work.join("x");
    std::fs::create_dir(&out).map_err(|err| format!("cannot create {}: {err}", out.display()))?;
    extract_members(&tar, &archive, &out, &members)?;
    check_extracted_tree(&out, &members)?;
    // FrameTrace's own pinned license texts (validated against the
    // manifest's sha256 when the manifest was loaded).
    let own = staging.path().join("own-licenses");
    std::fs::create_dir(&own).map_err(|err| format!("cannot create {}: {err}", own.display()))?;
    let mut own_moves = Vec::new();
    for license in &tool.bundled_licenses {
        let text = bundled_license_text(&license.file)
            .ok_or_else(|| format!("{} is not compiled in", license.file))?;
        let src = own.join(&license.to);
        std::fs::write(&src, text)
            .map_err(|err| format!("cannot write {}: {err}", src.display()))?;
        own_moves.push((src, dirs.licenses().join(&license.to)));
    }

    // Last cancellable point: from here the files move into place.
    progress.enter_non_cancellable(Stage::Install)?;
    let mut moves = Vec::new();
    for map in &tool.files {
        moves.push((out.join(&map.from), dirs.bin().join(&map.to)));
    }
    for map in &tool.license_files {
        moves.push((out.join(&map.from), dirs.licenses().join(&map.to)));
    }
    moves.extend(own_moves);
    let backup_dir = staging.path().join("backup");
    if let Err(err) = move_all_or_nothing(&moves, &backup_dir) {
        // Fix round 12 H-a: an incomplete rollback keeps the previous files
        // (they are in staging/backup, which dropping `staging` would delete).
        return Err(preserve_rollback_backup(
            err,
            staging,
            &backup_dir,
            &dirs.tools_dir,
            &tool.name,
        ));
    }
    // The replaced files sit in staging/backup; a still-running old
    // ffmpeg.exe cannot be deleted, so say what was left (never "ok"
    // with a silent leftover).
    let staging_left = staging.close();
    Ok(InstallOutcome {
        installed_files: moves.into_iter().map(|(_, dest)| dest).collect(),
        audit_warning: None,
        staging_left,
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
        progress.check_cancel()?;
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
    command
        .args(curl_args(url, https, out, size))
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
                        INSTALL_CANCELLED.to_string()
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

/// curl's argv for one hop. `--disable` must come first: no ~/.curlrc may
/// add options (`--insecure`, `--location`, ...) behind our back.
/// `--globoff`: `[]`/`{}` in a URL are literal, never a curl range glob
/// that would fetch other URLs. No `--location` (hops are followed by
/// [`download`]) and no `--proxy`/`--noproxy`: curl honours the user's
/// proxy environment (`HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`) so the
/// install works behind a corporate proxy; TLS still terminates at the
/// allowlisted host and the pinned SHA-256 decides (T1/T2).
pub(crate) fn curl_args(url: &str, https: bool, out: &Path, size: u64) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = [
        "--disable",
        "--silent",
        "--show-error",
        "--fail",
        "--globoff",
        "--proto",
        if https { "=https" } else { "=http" },
        "--tlsv1.2",
        "--max-redirs",
        "0",
        "--connect-timeout",
        "30",
        "--speed-limit",
        "1024",
        "--speed-time",
        "60",
        "--max-time",
    ]
    .iter()
    .map(std::ffi::OsString::from)
    .collect();
    args.push(MAX_TRANSFER_SECS.to_string().into());
    args.push("--max-filesize".into());
    args.push(size.to_string().into());
    args.push("--output".into());
    args.push(out.as_os_str().to_os_string());
    args.push("--write-out".into());
    args.push("%{http_code} %{redirect_url}".into());
    args.push("--url".into());
    args.push(url.into());
    args
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

/// Fix round 11 L2, narrowed in round 12 L3: `tar.exe` (bsdtar 3.8.9 on
/// Windows 11 26200) reads its argv in the system ANSI code page, so a path
/// character outside it (an emoji, or Hangul on an en-US PC) reaches it as
/// `?` and the install failed with an opaque "archive listing failed".
/// `curl.exe` (8.21.0 there) takes a wide argv and wrote an emoji
/// `--output` path intact; it is handed the same ANSI-safe form anyway
/// (one staging path for both, and it costs curl nothing). The refusal
/// below is about tar.
pub(crate) const ANSI_PATH_REFUSED: &str = "FrameTrace 폴더 경로에 이 PC의 코드 페이지로 쓸 수 없는 문자가 있어 설치할 수 없습니다 — 영문 경로의 폴더로 옮긴 뒤 다시 시도하십시오";

const CP_ACP: u32 = 0;
const CP_UTF8: u32 = 65001;
const WC_NO_BEST_FIT_CHARS: u32 = 0x400;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetACP() -> u32;
    fn WideCharToMultiByte(
        code_page: u32,
        flags: u32,
        wide: *const u16,
        wide_len: i32,
        multi: *mut u8,
        multi_len: i32,
        default_char: *const u8,
        used_default: *mut i32,
    ) -> i32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        multi: *const u8,
        multi_len: i32,
        wide: *mut u16,
        wide_len: i32,
    ) -> i32;
    fn GetShortPathNameW(long: *const u16, short: *mut u16, len: u32) -> u32;
}

/// Whether the process ANSI code page is UTF-8 (the "Beta: UTF-8" option):
/// then every path round-trips.
pub(crate) fn ansi_is_utf8() -> bool {
    // SAFETY: GetACP only reads the process code page.
    unsafe { GetACP() == CP_UTF8 }
}

/// Whether `path` survives a trip through the ANSI code page unchanged
/// (no best-fit mapping, no default character).
pub(crate) fn ansi_round_trips(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.iter().all(|&unit| unit < 0x80) {
        return true;
    }
    let Ok(wide_len) = i32::try_from(wide.len()) else {
        return false;
    };
    let utf8 = ansi_is_utf8();
    // CP_UTF8 takes neither flags nor the used-default pointer.
    let flags = if utf8 { 0 } else { WC_NO_BEST_FIT_CHARS };
    let mut used_default = 0i32;
    let used_ptr: *mut i32 = if utf8 {
        std::ptr::null_mut()
    } else {
        &mut used_default
    };
    // SAFETY: `wide` is valid for `wide_len` units; a null output buffer
    // with length 0 asks for the size only.
    let needed = unsafe {
        WideCharToMultiByte(
            CP_ACP,
            flags,
            wide.as_ptr(),
            wide_len,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            used_ptr,
        )
    };
    if needed <= 0 || used_default != 0 {
        return false;
    }
    let mut multi = vec![0u8; needed as usize];
    // SAFETY: `multi` holds `needed` bytes, as the size query reported.
    let written = unsafe {
        WideCharToMultiByte(
            CP_ACP,
            flags,
            wide.as_ptr(),
            wide_len,
            multi.as_mut_ptr(),
            needed,
            std::ptr::null(),
            used_ptr,
        )
    };
    if written != needed || used_default != 0 {
        return false;
    }
    // SAFETY: `multi` is valid for `written` bytes; size query first.
    let back_len =
        unsafe { MultiByteToWideChar(CP_ACP, 0, multi.as_ptr(), written, std::ptr::null_mut(), 0) };
    if back_len <= 0 {
        return false;
    }
    let mut back = vec![0u16; back_len as usize];
    // SAFETY: `back` holds `back_len` units, as the size query reported.
    let got = unsafe {
        MultiByteToWideChar(
            CP_ACP,
            0,
            multi.as_ptr(),
            written,
            back.as_mut_ptr(),
            back_len,
        )
    };
    got == back_len && back == wide
}

/// The 8.3 short form of an existing `path` (`None` when the volume keeps
/// no short names or the call fails).
fn short_path(path: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let long: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `long` is NUL-terminated; a null buffer of length 0 asks for
    // the size (including the NUL).
    let needed = unsafe { GetShortPathNameW(long.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return None;
    }
    let mut short = vec![0u16; needed as usize];
    // SAFETY: `short` holds `needed` units.
    let written = unsafe { GetShortPathNameW(long.as_ptr(), short.as_mut_ptr(), needed) };
    if written == 0 || written >= needed {
        return None;
    }
    short.truncate(written as usize);
    Some(PathBuf::from(std::ffi::OsString::from_wide(&short)))
}

/// A form of the existing directory `dir` that tar (and curl) receive intact:
/// `dir` itself when it round-trips through the ANSI code page, else its
/// 8.3 short path when that does, else the clear [`ANSI_PATH_REFUSED`]
/// refusal (before anything is downloaded).
pub(crate) fn ansi_safe_dir(dir: &Path) -> Result<PathBuf, String> {
    if ansi_round_trips(dir) {
        return Ok(dir.to_path_buf());
    }
    match short_path(dir) {
        Some(short) if ansi_round_trips(&short) => Ok(short),
        _ => Err(format!("{ANSI_PATH_REFUSED} ({})", dir.display())),
    }
}
