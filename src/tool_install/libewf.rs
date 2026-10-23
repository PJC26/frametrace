//! libewf ships source only, so the workstation runs the repository's
//! pinned MSYS2 build script (embedded at compile time, so a portable copy
//! of the exe does not need the `scripts/` folder) with `-Destination`
//! pointed at the staging directory.

use super::stream::run_streaming;
use super::{Reporter, system32};
use std::path::Path;
use std::process::Command;

const BUILD_SCRIPT: &str = include_str!("../../scripts/build-libewf-tools.ps1");

pub(super) fn build_into(dest: &Path, staging: &Path, report: Reporter<'_>) -> Result<(), String> {
    let script = staging.join("build-libewf-tools.ps1");
    // PowerShell 5.1 reads a BOM-less script as the ANSI code page; the
    // UTF-8 BOM keeps the non-ASCII comments intact.
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(BUILD_SCRIPT.trim_start_matches('\u{feff}').as_bytes());
    std::fs::write(&script, bytes).map_err(|err| format!("빌드 스크립트 저장 실패: {err}"))?;
    report("libewf 빌드 시작 — MSYS2 준비 (최초 1회 수 분 소요)".into());

    let mut command = Command::new(system32("WindowsPowerShell\\v1.0\\powershell.exe"));
    crate::util::hide_console(&mut command);
    command
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-Destination")
        .arg(dest);
    let outcome = run_streaming(&mut command, &|line| report(format!("libewf 빌드 · {line}")))
        .map_err(|err| format!("PowerShell {err}"))?;
    if outcome.status.success() {
        return Ok(());
    }
    Err(format!(
        "libewf 빌드 실패 (종료 코드 {:?}): {}",
        outcome.status.code(),
        outcome.last_line
    ))
}
#[cfg(test)]
mod tests {
    use super::BUILD_SCRIPT;

    #[test]
    fn embedded_script_accepts_destination_parameter() {
        assert!(BUILD_SCRIPT.contains("[string]$Destination"));
    }

    #[test]
    fn embedded_script_keeps_pinned_source_hashes() {
        assert!(BUILD_SCRIPT.contains("Assert-Sha256 $tarball $LibewfSha256"));
        assert!(BUILD_SCRIPT.contains("Assert-Sha256 $sfx $Msys2Sha256"));
    }
}
