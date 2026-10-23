//! Download → SHA-256 verify → extract for archive-shipped packages.
//!
//! Uses the OS-bundled, Microsoft-signed `curl.exe` and `tar.exe`
//! (Windows 10 1803+) by absolute System32 path: no new crate dependency,
//! no TLS stack to vendor, and no PATH lookup a planted binary could hijack.

use super::catalog::PinnedArchive;
use super::{Reporter, system32};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Downloads `pin.url` into `staging`, verifies it, and extracts
/// `pin.bin_prefix`. Returns the extracted bin directory.
pub(super) fn fetch_and_extract(
    pin: &PinnedArchive,
    staging: &Path,
    report: Reporter<'_>,
) -> Result<PathBuf, String> {
    let archive = staging.join("download.zip");
    download(pin.url, &archive, report)?;
    report("무결성 확인 중 (SHA-256)".into());
    verify_sha256(&archive, pin.sha256)?;
    report("압축 해제 중".into());
    let extract_root = staging.join("x");
    std::fs::create_dir_all(&extract_root)
        .map_err(|err| format!("압축 해제 폴더를 만들 수 없습니다: {err}"))?;
    extract(&archive, &extract_root, pin.bin_prefix)?;
    let bin = pin
        .bin_prefix
        .split('/')
        .fold(extract_root, |dir, part| dir.join(part));
    if !bin.is_dir() {
        return Err(format!("압축 파일에 {} 폴더가 없습니다", pin.bin_prefix));
    }
    Ok(bin)
}

fn download(url: &str, dest: &Path, report: Reporter<'_>) -> Result<(), String> {
    let mut command = Command::new(system32("curl.exe"));
    crate::util::hide_console(&mut command);
    command
        .args(["--fail", "--location", "--silent", "--show-error"])
        // Refuse a downgrade to plain HTTP anywhere in the redirect chain.
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args(["--retry", "3", "--connect-timeout", "30"])
        .arg("--output")
        .arg(dest)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|err| format!("curl.exe 실행 실패 (Windows 10 1803 이상 필요): {err}"))?;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| format!("다운로드 상태 확인 실패: {err}"))?
        {
            break status;
        }
        let bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        report(format!("다운로드 중 · {:.1} MB", bytes as f64 / 1_048_576.0));
        std::thread::sleep(Duration::from_millis(500));
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    Err(format!("다운로드 실패: {}", stderr.trim()))
}

pub(super) fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let file = File::open(path).map_err(|err| format!("다운로드 파일을 열 수 없습니다: {err}"))?;
    let actual = crate::sha256::digest_reader(file)
        .map_err(|err| format!("SHA-256 계산 실패: {err}"))?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "SHA-256 불일치 — 변조되었거나 다른 파일입니다 (기대 {expected}, 실제 {actual})"
        ))
    }
}

fn extract(archive: &Path, dest: &Path, member_prefix: &str) -> Result<(), String> {
    let mut command = Command::new(system32("tar.exe"));
    crate::util::hide_console(&mut command);
    let output = command
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .arg(member_prefix)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("tar.exe 실행 실패: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "압축 해제 실패: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_sha256_accepts_matching_digest() {
        let dir = std::env::temp_dir().join(format!("ft-sha-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("blob");
        std::fs::write(&file, b"hello world").unwrap();
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert_eq!(verify_sha256(&file, expected), Ok(()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_sha256_rejects_tampered_content() {
        let dir = std::env::temp_dir().join(format!("ft-sha-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("blob");
        std::fs::write(&file, b"hello world!").unwrap();
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let err = verify_sha256(&file, expected).unwrap_err();
        assert!(err.contains("SHA-256 불일치"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
