//! `frametrace tools list | install <name> [--yes]` (track S3).

use crate::tool_install::{self, InstallDirs, Progress, Stage};
use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;

pub fn tools_list(json: bool, all: bool) -> Result<(), String> {
    let manifest = tool_install::manifest()?;
    let tools: Vec<_> = manifest
        .tools
        .iter()
        .filter(|tool| all || !tool.on_demand)
        .map(|tool| (tool, tool_install::status(tool)))
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

pub fn tools_install(name: &str, yes: bool) -> Result<(), String> {
    tools_install_in(name, yes, &InstallDirs::for_current_exe()?)
}

fn tools_install_in(name: &str, yes: bool, dirs: &InstallDirs) -> Result<(), String> {
    let manifest = tool_install::manifest()?;
    // Unknown / not installable: refused here, before the prompt, and
    // recorded in the chained install audit log like the API's refusals.
    let tool = tool_install::preflight(manifest, name, dirs)?;
    // T3: name, source and size are shown before anything is fetched.
    println!(
        "도구: {} {}",
        tool.title,
        tool.version.as_deref().unwrap_or("")
    );
    if let Some(file) = &tool.file_name {
        println!("파일: {file}");
    }
    if let Some(url) = &tool.url {
        println!("출처: {url}");
    }
    if let Some(size) = tool.size {
        println!(
            "크기: {size} 바이트 ({:.1} MB)",
            size as f64 / (1024.0 * 1024.0)
        );
    }
    if let Some(sha) = &tool.sha256 {
        println!("SHA-256: {sha}");
    }
    if let Some(license) = &tool.license {
        println!("라이선스: {license}");
    }
    println!("설치 위치: {}", dirs.tools_dir.display());
    // Manifest tools resolve from tools/bin first (tool_policy): a copy
    // on PATH stops being used once this install lands.
    for (binary, path) in &tool_install::status(tool).paths {
        if let Some(path) = path
            && !std::path::Path::new(path).starts_with(&dirs.tools_dir)
        {
            println!(
                "참고: 지금은 PATH의 {binary}({path})가 쓰입니다 — 설치하면 tools/bin의 것이 먼저 쓰입니다."
            );
        }
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
        println!(
            "참고: 이전 파일이 아직 실행 중이라 임시 폴더를 지우지 못했습니다 — {} (다음 설치·서버 시작 때 정리)",
            left.display()
        );
    }
    println!(
        "완료: {name} 설치 (감사 기록: {})",
        dirs.audit_log().display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
