//! `join-video` and `measure-audio` CLI handlers.

use super::ensure_case;
use crate::audio_level::{self, MeasureOptions};
use crate::case_db;
use crate::checkpoint::ResumeMode;
use crate::join::{self, RunOptions, Tools};
use std::path::Path;

pub struct JoinVideoArgs {
    pub ids: Vec<String>,
    pub tolerance: f64,
    pub plan_id: Option<String>,
    pub job_tag: Option<String>,
    /// Print the plan JSON and stop (no output is written).
    pub check: bool,
}

/// Splits `--ids a,b,c` (also accepts repeated flags) into ids.
pub fn split_ids(raw: &[String]) -> Vec<String> {
    raw.iter()
        .flat_map(|part| part.split(','))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn join_video(case_dir: &Path, args: &JoinVideoArgs) -> Result<(), String> {
    ensure_case(case_dir)?;
    let plan = join::plan(case_dir, &args.ids, args.tolerance)?;
    if args.check {
        println!(
            "{}",
            serde_json::json!({"ok": true, "plan": plan.to_json()})
        );
        return Ok(());
    }
    if let Some(expected) = &args.plan_id
        && !expected.eq_ignore_ascii_case(&plan.plan_id)
    {
        eprintln!("error: {}", join::PLAN_CHANGED_MESSAGE);
        eprintln!("expected plan_id {expected}, current {}", plan.plan_id);
        std::process::exit(join::EXIT_PLAN_CHANGED);
    }
    if let Some(issue) = plan.blocking.first() {
        let all = plan
            .blocking
            .iter()
            .map(|i| format!("[{}] {}", i.code, i.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("이어붙일 수 없습니다: {} ({all})", issue.message));
    }
    let job_tag = match &args.job_tag {
        Some(tag) => tag.clone(),
        None => format!("cli-{}-{}", crate::util::now_unix()?, std::process::id()),
    };
    let outcome = join::run(
        case_dir,
        &plan,
        &RunOptions {
            job_tag,
            tools: Tools::default(),
            timeout_secs: None,
        },
    )?;
    if !outcome.duration_check_ok {
        eprintln!(
            "warning: 출력 길이({:?}초)가 원본 합계({:.3}초)와 다릅니다 — 매니페스트를 확인하십시오",
            outcome.duration_observed, outcome.duration_expected
        );
    }
    println!("{}", outcome.to_json());
    Ok(())
}

pub fn measure_audio(
    case_dir: &Path,
    selection: Option<&Path>,
    force: bool,
    resume: ResumeMode,
) -> Result<(), String> {
    ensure_case(case_dir)?;
    let selectors = match selection {
        Some(path) => {
            let parsed = crate::selection::parse_selection_file(path)?;
            crate::selection::enforce_case_binding(
                case_dir,
                parsed.case_id.as_deref(),
                path,
                crate::selection::ImportKind::Selection,
            )?;
            Some(parsed.items.into_iter().map(|item| item.selector).collect())
        }
        None => None,
    };
    let subject = selection
        .map(Path::to_path_buf)
        .unwrap_or_else(|| case_dir.join("db/videos.jsonl"));
    let job = case_db::start_job(
        case_dir,
        "measure-audio",
        &subject,
        None,
        &format!(
            "{{\"force\":{force},\"selection\":{}}}",
            selection.is_some()
        ),
    )?;
    let options = MeasureOptions {
        selectors,
        force,
        resume,
        ..MeasureOptions::default()
    };
    let job_id = job.job_id.clone();
    let mut progress = |done: usize, total: usize| {
        if done.is_multiple_of(16) || done == total {
            let _ =
                case_db::report_job_progress(case_dir, &job_id, Some(total as u64), done as u64);
        }
    };
    match audio_level::measure_case(case_dir, &options, &mut progress) {
        Ok(summary) => {
            let _ = case_db::complete_job(
                case_dir,
                &job.job_id,
                summary.measured as u64 + summary.resumed as u64,
                "audio measurement complete",
            );
            if !summary.not_found.is_empty() {
                eprintln!(
                    "warning: {} selector(s) not found in the index: {}",
                    summary.not_found.len(),
                    summary.not_found.join(", ")
                );
            }
            println!("{}", summary.to_json());
            // B15: exit 0 only when nothing errored. Exit 4 = the run
            // completed and logged everything, but some files could not be
            // measured (they stay "미확인"). The last stderr line carries the
            // count for the server's "완료(오류 N건)" log.
            if summary.error > 0 {
                eprintln!(
                    "{}{}건 (측정 대상 {}건)",
                    audio_level::ERRORS_LINE_PREFIX,
                    summary.error,
                    summary.targets
                );
                std::process::exit(audio_level::EXIT_SOME_ERRORS);
            }
            Ok(())
        }
        Err(err) => {
            let _ = case_db::fail_job(case_dir, &job.job_id, &err);
            Err(err)
        }
    }
}
