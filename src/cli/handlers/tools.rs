//! `frametrace tools list | install <name> [--yes]` (track S3).

use crate::tool_install::{self, InstallDirs, Progress, Stage};
use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;

pub fn tools_list(json: bool, all: bool) -> Result<(), String> {
    let manifest = tool_install::manifest()?;
    // Fix round 14 L1: listed (and sized) once, not once per tool.
    let backups = tool_install::rollback_backups_for_current_exe();
    let tools: Vec<_> = manifest
        .tools
        .iter()
        .filter(|tool| all || !tool.on_demand)
        .map(|tool| (tool, tool_install::status(tool, &backups)))
        .collect();
    if json {
        let items: Vec<serde_json::Value> = tools
            .iter()
            .map(|(tool, status)| tool_install::tool_json(tool, status))
            .collect();
        println!(
            "{}",
            serde_json::json!({"platform": tool_install::PLATFORM, "tools": items})
        );
        return Ok(());
    }
    for (tool, status) in tools {
        let version = tool.version.as_deref().unwrap_or("-");
        let need = if tool.required { "필수" } else { "선택" };
        println!("{} {version} [{need}] — {}", tool.name, status.label());
        for (binary, path) in &status.paths {
            println!("  {binary}: {}", path.as_deref().unwrap_or("없음"));
            if binary == "ffmpeg"
                && let Some(note) = path
                    .as_deref()
                    .and_then(|path| crate::ffmpeg_encoders::for_binary(path).build_note())
            {
                println!("  참고: {note}");
            }
        }
        for (binary, other) in &status.shadowed_by {
            if let Some(other) = other {
                println!(
                    "  주의: PATH에 다른 {binary}가 있습니다 — {other} (쓰지 않음, tools/bin 우선). 직접 둔 것이 아니면 확인하십시오."
                );
            }
        }
        // Fix round 13 L2: an incomplete rollback left previous files.
        for line in rollback_lines(&status) {
            println!("{line}");
        }
        if !status.installed() {
            match tool_install::not_installable_reason(tool) {
                None => println!("  설치: frametrace tools install {}", tool.name),
                Some(reason) => {
                    println!("  자동 설치 불가: {reason}");
                    if let Some(hint) = &tool.manual_hint {
                        println!("  수동 설치: {hint}");
                    }
                }
            }
        }
    }
    Ok(())
}

/// `tools list` warning lines for the tool's kept rollback backups.
fn rollback_lines(status: &tool_install::ToolStatus) -> Vec<String> {
    status
        .rollback_backups
        .iter()
        .map(|backup| format!("  경고: {}", tool_install::rollback_hint(backup)))
        .collect()
}

pub fn tools_install(name: &str, yes: bool) -> Result<(), String> {
    tools_install_in(name, yes, &InstallDirs::for_current_exe()?)
}

fn tools_install_in(name: &str, yes: bool, dirs: &InstallDirs) -> Result<(), String> {
    let manifest = tool_install::manifest()?;
    // Unknown / not installable: refused here, before the prompt, and
    // recorded in the chained install audit log like the API's refusals.
    let tool = tool_install::preflight(manifest, name, dirs)?;
    // Fix round 14 L1: the status of the `tools` dir being installed into.
    let backups = tool_install::rollback_backups(&dirs.tools_dir);
    for line in install_prompt_lines(tool, dirs, &tool_install::status(tool, &backups)) {
        println!("{line}");
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(
                "설치 확인이 필요합니다 — 대화형 터미널에서 실행하거나 --yes를 붙이십시오".into(),
            );
        }
        print!("설치할까요? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(|err| format!("cannot read the answer: {err}"))?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return Err("설치를 취소했습니다.".into());
        }
    }
    let progress = Arc::new(Progress::default());
    let worker_progress = Arc::clone(&progress);
    let owned = name.to_string();
    let worker = std::thread::spawn(move || tool_install::install(&owned, &worker_progress));
    let mut last_stage = Stage::Queued;
    let mut last_decile = u64::MAX;
    while !worker.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let stage = progress.stage();
        if stage != last_stage {
            println!("단계: {}", stage.as_str());
            last_stage = stage;
        }
        let (done, total) = progress.bytes();
        if stage == Stage::Download && total > 0 {
            let decile = done * 10 / total;
            if decile != last_decile {
                println!("  {}%", decile * 10);
                last_decile = decile;
            }
        }
    }
    let outcome = worker
        .join()
        .map_err(|_| "설치 작업이 비정상 종료되었습니다.".to_string())??;
    for file in &outcome.installed_files {
        println!("설치됨: {}", file.display());
    }
    if let Some(warning) = &outcome.audit_warning {
        println!("경고: 설치 기록을 남기지 못했습니다 — {warning}");
    }
    if let Some(left) = &outcome.staging_left {
        println!("{}", staging_left_note(left));
    }
    println!(
        "완료: {name} 설치 (감사 기록: {})",
        dirs.audit_log().display()
    );
    Ok(())
}

/// What `tools install` shows before asking (T3): name, file, source,
/// size, SHA-256, licence, the distribution notice (T5: the examiner
/// downloads the file from the publisher; FrameTrace does not distribute
/// it), the install location, and a note per binary currently resolved
/// from PATH (`status`), which stops being used once tools/bin has it.
fn install_prompt_lines(
    tool: &tool_install::ToolEntry,
    dirs: &InstallDirs,
    status: &tool_install::ToolStatus,
) -> Vec<String> {
    let mut lines = vec![format!(
        "도구: {} {}",
        tool.title,
        tool.version.as_deref().unwrap_or("")
    )];
    if let Some(file) = &tool.file_name {
        lines.push(format!("파일: {file}"));
    }
    if let Some(url) = &tool.url {
        lines.push(format!("출처: {url}"));
    }
    if let Some(size) = tool.size {
        lines.push(format!(
            "크기: {size} 바이트 ({:.1} MB)",
            size as f64 / (1024.0 * 1024.0)
        ));
    }
    if let Some(sha) = &tool.sha256 {
        lines.push(format!("SHA-256: {sha}"));
    }
    if let Some(license) = &tool.license {
        lines.push(format!("라이선스: {license}"));
    }
    if let Some(notice) = &tool.notice {
        lines.push(format!("고지: {notice}"));
    }
    lines.push(format!("설치 위치: {}", dirs.tools_dir.display()));
    for (binary, path) in &status.paths {
        if let Some(path) = path
            && !std::path::Path::new(path).starts_with(&dirs.tools_dir)
        {
            lines.push(format!(
                "참고: 지금은 PATH의 {binary}({path})가 쓰입니다 — 설치하면 tools/bin의 것이 먼저 쓰입니다."
            ));
        }
    }
    // Fix round 14 L1: a kept backup of an earlier incomplete rollback is
    // shown before reinstalling too (the folder stays after the install).
    lines.extend(rollback_lines(status));
    lines
}

/// The note for a leftover the install could not remove (an old binary or
/// venv still running). It is swept by the next install or server start
/// only once nothing runs from it any more.
fn staging_left_note(left: &std::path::Path) -> String {
    format!(
        "참고: 이전 파일이 아직 실행 중이라 임시 폴더를 지우지 못했습니다 — {} (그 프로그램을 닫으면 다음 설치·서버 시작 때 정리됩니다)",
        left.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_left_note_says_when_it_is_cleaned() {
        let note = staging_left_note(std::path::Path::new(
            "C:/FrameTrace/tools/python/.deepfake-lens.old-0123",
        ));
        assert!(note.contains(".deepfake-lens.old-0123"), "{note}");
        assert!(note.contains("그 프로그램을 닫으면"), "{note}");
        assert!(!note.contains("(다음 설치·서버 시작 때 정리)"), "{note}");
    }

    /// T5: the prompt shows the manifest's distribution notice exactly
    /// once, after the licence line and before the install location; a
    /// PATH copy gets its own note. (Not-installable tools never reach
    /// the prompt — `cli_refusals_are_audited_before_the_prompt`.)
    #[test]
    fn install_prompt_shows_the_distribution_notice() {
        let manifest = crate::tool_install::manifest().unwrap();
        let ffmpeg = crate::tool_install::find_tool(manifest, "ffmpeg").unwrap();
        let dirs = InstallDirs {
            tools_dir: std::path::PathBuf::from("C:/FrameTrace/tools"),
        };
        let status = crate::tool_install::ToolStatus {
            paths: vec![
                (
                    "ffmpeg".to_string(),
                    Some("C:/ProgramData/chocolatey/bin/ffmpeg.exe".to_string()),
                ),
                ("ffprobe".to_string(), None),
            ],
            shadowed_by: Vec::new(),
            rollback_backups: Vec::new(),
        };
        let lines = install_prompt_lines(ffmpeg, &dirs, &status);
        assert_eq!(
            lines[0],
            format!("도구: FFmpeg {}", ffmpeg.version.as_deref().unwrap())
        );
        let notices: Vec<usize> = (0..lines.len())
            .filter(|&i| lines[i].starts_with("고지: "))
            .collect();
        assert_eq!(notices.len(), 1, "{lines:#?}");
        let at = notices[0];
        assert_eq!(
            lines[at],
            format!("고지: {}", ffmpeg.notice.as_deref().unwrap())
        );
        assert!(lines[at].contains("배포하지 않"), "{}", lines[at]);
        assert!(lines[at - 1].starts_with("라이선스: "), "{lines:#?}");
        assert!(lines[at + 1].starts_with("설치 위치: "), "{lines:#?}");
        assert!(lines.iter().any(|l| l.starts_with("SHA-256: 4a7642b2")));
        let path_notes: Vec<&String> = lines.iter().filter(|l| l.starts_with("참고: ")).collect();
        assert_eq!(path_notes.len(), 1, "{lines:#?}");
        assert!(path_notes[0].contains("chocolatey"), "{}", path_notes[0]);
        // A tool without a notice prints no "고지:" line.
        let mut plain = ffmpeg.clone();
        plain.notice = None;
        let lines = install_prompt_lines(&plain, &dirs, &status);
        assert!(!lines.iter().any(|l| l.starts_with("고지:")), "{lines:#?}");
    }

    /// Fix round 14 L1: `status` takes the backups listed once from the
    /// `tools` dir in use (here a fixture dir, not the test exe's) and
    /// keeps only the tool's own; the install prompt warns about them.
    #[test]
    fn status_and_the_prompt_use_the_given_tools_dir_backups() {
        let base = std::env::temp_dir().join(format!("ft-cli-backups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dirs = InstallDirs {
            tools_dir: base.join("tools"),
        };
        let kept = dirs.tools_dir.join("rollback-backup-00aa");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join("RESTORE.txt"), "tool: ffmpeg\nhow\n").unwrap();
        std::fs::write(kept.join("0.bak"), b"old").unwrap();
        let other = dirs.tools_dir.join("rollback-backup-00bb");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("RESTORE.txt"), "tool: libewf\nhow\n").unwrap();
        let backups = crate::tool_install::rollback_backups(&dirs.tools_dir);
        assert_eq!(backups.len(), 2);
        let manifest = crate::tool_install::manifest().unwrap();
        let ffmpeg = crate::tool_install::find_tool(manifest, "ffmpeg").unwrap();
        let status = crate::tool_install::status(ffmpeg, &backups);
        assert_eq!(status.rollback_backups.len(), 1);
        assert_eq!(status.rollback_backups[0].path, kept);
        let lines = install_prompt_lines(ffmpeg, &dirs, &status);
        let warnings: Vec<&String> = lines.iter().filter(|l| l.starts_with("  경고: ")).collect();
        assert_eq!(warnings.len(), 1, "{lines:#?}");
        assert!(
            warnings[0].contains(&kept.display().to_string()),
            "{}",
            warnings[0]
        );
        // No backups for a tool without any: no warning line.
        let ffmpeg_clean = crate::tool_install::status(ffmpeg, &[]);
        assert!(ffmpeg_clean.rollback_backups.is_empty());
        let lines = install_prompt_lines(ffmpeg, &dirs, &ffmpeg_clean);
        assert!(!lines.iter().any(|l| l.contains("경고")), "{lines:#?}");
        let _ = std::fs::remove_dir_all(base);
    }

    /// `frametrace tools install` refusals before the prompt (unknown tool,
    /// not installable here) are appended to the chained install audit log,
    /// like the API's — and nothing else is written.
    #[test]
    fn cli_refusals_are_audited_before_the_prompt() {
        let base = std::env::temp_dir().join(format!("ft-cli-refuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dirs = InstallDirs {
            tools_dir: base.join("tools"),
        };
        let err = tools_install_in("no-such-tool", true, &dirs).unwrap_err();
        assert!(err.contains("unknown tool 'no-such-tool'"), "{err}");
        // libewf has no published checksum: never installable.
        let err = tools_install_in("libewf", true, &dirs).unwrap_err();
        assert!(err.contains("자동 설치 불가"), "{err}");
        crate::tool_install::tests::assert_only_refusals(
            &dirs,
            &["unknown_tool", crate::tool_install::not_installable_code()],
        );
        let text = std::fs::read_to_string(dirs.audit_log()).unwrap();
        let tools: Vec<String> = text
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["tool"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(tools, ["no-such-tool", "libewf"]);
        let _ = std::fs::remove_dir_all(base);
    }
}
