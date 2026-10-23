//! On-demand installer for the optional forensic tools (FFmpeg, Sleuth Kit,
//! libewf) into `<exe dir>/tools/<package>/`, where
//! [`crate::tool_policy`] resolves them. Every install writes an
//! `install-receipt.json` (version, source, per-file SHA-256) so the tool
//! chain behind a report can be reconstructed later.

mod archive;
pub mod catalog;
mod libewf;
mod stream;

pub use catalog::{ALL_PACKAGES, InstallMethod, ToolPackage};

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Progress sink: receives one human-readable status line at a time.
pub type Reporter<'a> = &'a dyn Fn(String);

pub const RECEIPT_FILE: &str = "install-receipt.json";

/// `<exe dir>/tools`, the trusted install tree next to the binary.
pub fn tools_root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|err| format!("실행 파일 위치 확인 실패: {err}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "실행 파일 폴더를 확인할 수 없습니다".to_string())?;
    Ok(dir.join("tools"))
}

/// Installs `package` and returns the directory it now lives in.
pub fn install(package: ToolPackage, report: Reporter<'_>) -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Err(
            "자동 설치는 Windows 전용입니다 — 패키지 관리자로 설치하십시오 (예: apt install ffmpeg sleuthkit ewf-tools)"
                .into(),
        );
    }
    let root = tools_root()?;
    install_into(&root, package, report)
}

fn install_into(root: &Path, package: ToolPackage, report: Reporter<'_>) -> Result<PathBuf, String> {
    let staging = root.join(format!(".staging-{}", package.dir_name()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|err| format!("임시 폴더를 만들 수 없습니다 ({}): {err}", staging.display()))?;
    let result = stage_package(package, &staging, report)
        .and_then(|staged| promote(root, package, &staged));
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Produces a complete package directory inside `staging`.
fn stage_package(package: ToolPackage, staging: &Path, report: Reporter<'_>) -> Result<PathBuf, String> {
    let staged = staging.join("pkg");
    std::fs::create_dir_all(&staged).map_err(|err| format!("임시 폴더 생성 실패: {err}"))?;
    let source = match package.method() {
        InstallMethod::Archive(pin) => {
            let bin = archive::fetch_and_extract(&pin, staging, report)?;
            copy_files(&bin, &staged)?;
            serde_json::json!({ "method": "archive", "url": pin.url, "sha256": pin.sha256 })
        }
        InstallMethod::BuildScript => {
            libewf::build_into(&staged, staging, report)?;
            serde_json::json!({ "method": "build-script", "script": "scripts/build-libewf-tools.ps1" })
        }
    };
    for tool in package.provides() {
        if !staged.join(format!("{tool}.exe")).is_file() {
            return Err(format!("설치 결과에 {tool}.exe가 없습니다"));
        }
    }
    report("설치 기록 작성 중".into());
    write_receipt(package, &staged, source)?;
    Ok(staged)
}

/// Swaps the staged directory into `tools/<package>` (old copy removed).
fn promote(root: &Path, package: ToolPackage, staged: &Path) -> Result<PathBuf, String> {
    let dest = root.join(package.dir_name());
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|err| {
            format!("기존 {} 폴더를 지울 수 없습니다 (사용 중인지 확인): {err}", package.label())
        })?;
    }
    std::fs::rename(staged, &dest).map_err(|err| format!("설치 폴더 이동 실패: {err}"))?;
    Ok(dest)
}

fn copy_files(from: &Path, to: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(from).map_err(|err| format!("{} 읽기 실패: {err}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("폴더 항목 읽기 실패: {err}"))?;
        let path = entry.path();
        if path.is_file() {
            std::fs::copy(&path, to.join(entry.file_name()))
                .map_err(|err| format!("{} 복사 실패: {err}", path.display()))?;
        }
    }
    Ok(())
}

fn write_receipt(package: ToolPackage, dir: &Path, source: serde_json::Value) -> Result<(), String> {
    let mut files = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .map_err(|err| format!("설치 폴더 읽기 실패: {err}"))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file())
        .collect();
    names.sort();
    for path in names {
        let file = std::fs::File::open(&path).map_err(|err| format!("{} 열기 실패: {err}", path.display()))?;
        let digest = crate::sha256::digest_reader(file).map_err(|err| format!("해시 계산 실패: {err}"))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        files.push(serde_json::json!({ "name": name, "sha256": digest }));
    }
    let installed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let receipt = serde_json::json!({
        "package": package.id(),
        "version": package.version(),
        "source": source,
        "installed_at_unix": installed_at,
        "files": files,
    });
    let text = serde_json::to_string_pretty(&receipt).map_err(|err| format!("설치 기록 직렬화 실패: {err}"))?;
    std::fs::write(dir.join(RECEIPT_FILE), text).map_err(|err| format!("설치 기록 저장 실패: {err}"))
}

/// Absolute path of a binary shipped with Windows.
fn system32(relative: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ft-tool-install-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn promote_replaces_previous_install() {
        let root = temp_dir("promote");
        let old = root.join("ffmpeg");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("stale.dll"), b"old").unwrap();
        let staged = root.join(".staging-ffmpeg").join("pkg");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("ffmpeg.exe"), b"new").unwrap();

        let dest = promote(&root, ToolPackage::Ffmpeg, &staged).unwrap();

        assert!(dest.join("ffmpeg.exe").is_file());
        assert!(!dest.join("stale.dll").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn receipt_lists_every_file_with_its_digest() {
        let dir = temp_dir("receipt");
        std::fs::write(dir.join("ffmpeg.exe"), b"hello world").unwrap();

        write_receipt(ToolPackage::Ffmpeg, &dir, serde_json::json!({ "method": "archive" })).unwrap();

        let receipt: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(RECEIPT_FILE)).unwrap()).unwrap();
        assert_eq!(receipt["package"], "ffmpeg");
        assert_eq!(receipt["version"], "9.0.2");
        assert_eq!(
            receipt["files"][0]["sha256"],
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
