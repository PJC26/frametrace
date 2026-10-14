use crate::audit;
use crate::tool_policy::{command_version, require_case_output_path, resolve_tool_binary};
use crate::util::{
    canonicalize_display, json_escape, now_unix, sanitize_filename, unique_path, write_text,
};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use wait_timeout::ChildExt;

const DEFAULT_MAX_ENTRIES: usize = 20_000;
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "wmv", "asf", "mpg", "mpeg", "mts", "m2ts", "ts", "3gp",
    "webm", "flv", "dav", "dav_", "nov", "ave", "g64", "g64x", "glv", "blk", "264", "265", "h264",
    "h265", "hevc",
];

#[derive(Debug, Clone)]
pub struct TskInspectOptions {
    pub partition_offset: Option<u64>,
    pub max_entries: usize,
    pub mmls_bin: String,
    pub fls_bin: String,
    /// Bound for mmls/fls runs in seconds; `None` runs unbounded
    /// (`--timeout 0`). The default applies TSK_PROBE_TIMEOUT_SECS — fls
    /// walking a ~230GiB NTFS image through libewf decompression takes
    /// ~25 minutes on real hardware, so the default must not assume
    /// folder-scale images.
    pub timeout_secs: Option<u64>,
}

/// Probe-class default for listing tools (seconds), shared with the e01 lane.
/// mmls finishes in seconds; the bound exists for fls's full-filesystem
/// walk — a real 232GiB seized-disk E01 measured ~25 min, so the default
/// covers images an order of magnitude larger.
pub const TSK_PROBE_TIMEOUT_SECS: u64 = 3600;

impl Default for TskInspectOptions {
    fn default() -> Self {
        Self {
            partition_offset: None,
            max_entries: DEFAULT_MAX_ENTRIES,
            mmls_bin: "mmls".to_string(),
            fls_bin: "fls".to_string(),
            timeout_secs: Some(TSK_PROBE_TIMEOUT_SECS),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TskRecoverOptions {
    pub partition_offset: u64,
    pub inode: String,
    pub output_path: Option<PathBuf>,
    pub recover_deleted: bool,
    pub include_slack: bool,
    pub skip_sparse_holes: bool,
    pub icat_bin: String,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmlsPartition {
    pub slot: String,
    pub start: u64,
    pub end: u64,
    pub length: u64,
    pub description: String,
    pub allocated: bool,
}

impl MmlsPartition {
    fn to_json(&self) -> String {
        format!(
            "{{\"slot\":\"{}\",\"start\":{},\"end\":{},\"length\":{},\"description\":\"{}\",\"allocated\":{}}}",
            json_escape(&self.slot),
            self.start,
            self.end,
            self.length,
            json_escape(&self.description),
            self.allocated
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlsEntry {
    pub raw_line: String,
    pub file_type: Option<String>,
    pub inode: Option<String>,
    pub path: Option<String>,
    pub deleted: bool,
    pub video_candidate: bool,
}

impl FlsEntry {
    fn to_json(&self) -> String {
        format!(
            "{{\"raw_line\":\"{}\",\"file_type\":{},\"inode\":{},\"path\":{},\"deleted\":{},\"video_candidate\":{}}}",
            json_escape(&self.raw_line),
            audit::optional_string(self.file_type.as_deref()),
            audit::optional_string(self.inode.as_deref()),
            audit::optional_string(self.path.as_deref()),
            self.deleted,
            self.video_candidate
        )
    }
}

#[derive(Debug, Clone)]
pub struct TskInspectResult {
    pub image_path: PathBuf,
    pub inspected_unix: u64,
    pub partition_offset: u64,
    pub partitions: Vec<MmlsPartition>,
    pub entries: Vec<FlsEntry>,
    /// False when fls was killed by the timeout mid-walk — `entries`
    /// then holds a *partial* listing (clearly flagged in warnings,
    /// the summary, and the audit event), not a complete one.
    pub fls_completed: bool,
    /// The listing was cut to `--max-entries` (also partial).
    pub entries_truncated: bool,
    pub warnings: Vec<String>,
    pub mmls_log_path: PathBuf,
    pub fls_log_path: PathBuf,
    pub entries_jsonl_path: PathBuf,
    pub summary_path: PathBuf,
}

/// C7 / B16 contract: process exit code of a step that completed and
/// recorded everything but whose result is PARTIAL (inspect-image, or
/// inspect-e01 --filesystem direct triage, with a timed-out or truncated
/// fls listing). The server reports it as a warning / `partial` step,
/// never as a clean pass.
pub const EXIT_PARTIAL: i32 = 5;
/// Prefix of the last stderr line of an [`EXIT_PARTIAL`] exit.
pub const PARTIAL_LINE_PREFIX: &str = "PARTIAL: ";

/// Why an inspection result is partial (`None` = complete).
pub fn partial_reason(result: &TskInspectResult) -> Option<String> {
    let mut reasons = Vec::new();
    if !result.fls_completed {
        reasons.push(
            "fls 시간 초과로 파일 목록이 일부만 수집되었습니다 (fls timed out; listing is partial)",
        );
    }
    if result.entries_truncated {
        reasons
            .push("파일 목록이 --max-entries에서 잘렸습니다 (listing truncated to --max-entries)");
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// C7: default bound for an inspect-image run — one hour per started
/// terabyte of image, at least one hour (`--timeout 0` = unlimited).
/// For an EWF set the sizes of all its segment files count.
pub fn scaled_inspect_timeout_secs(image: &Path) -> u64 {
    scaled_timeout_for_bytes(image_size_for_timeout(image))
}

/// Pure part of [`scaled_inspect_timeout_secs`].
pub fn scaled_timeout_for_bytes(bytes: u64) -> u64 {
    const TB: u64 = 1_000_000_000_000;
    let terabytes = bytes.div_ceil(TB).max(1);
    terabytes.saturating_mul(TSK_PROBE_TIMEOUT_SECS)
}

fn image_size_for_timeout(image: &Path) -> u64 {
    let own = fs::metadata(image).map(|m| m.len()).unwrap_or(0);
    let (Some(stem), Some(ext)) = (
        image.file_stem().and_then(|s| s.to_str()),
        image.extension().and_then(|e| e.to_str()),
    ) else {
        return own;
    };
    let lower = ext.to_ascii_lowercase();
    if !matches!(lower.as_str(), "e01" | "ex01" | "s01" | "l01") {
        return own;
    }
    let letter = lower.chars().next().unwrap_or('e');
    let Some(dir) = image.parent() else {
        return own;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return own;
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let same_stem = path.file_stem().and_then(|s| s.to_str()) == Some(stem);
            let segment_ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .is_some_and(|e| {
                    (3..=4).contains(&e.len())
                        && e.starts_with(letter)
                        && e.chars().all(|c| c.is_ascii_alphanumeric())
                });
            (same_stem && segment_ext).then(|| entry.metadata().map(|m| m.len()).unwrap_or(0))
        })
        .sum::<u64>()
        .max(own)
}

#[derive(Debug, Clone)]
pub struct TskRecoverResult {
    pub image_path: PathBuf,
    pub output_path: PathBuf,
    pub recovered_unix: u64,
    pub partition_offset: u64,
    pub inode: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub validation_status: String,
    pub warnings: Vec<String>,
}

/// `fls_progress` receives the cumulative entry count as fls output is
/// streamed (about every 5000 lines plus once at the end), so a multi-
/// minute filesystem walk reports live progress instead of silence.
pub fn inspect_image(
    case_dir: &Path,
    image_path: &Path,
    options: &TskInspectOptions,
    fls_progress: Option<&(dyn Fn(u64) + Send + Sync)>,
) -> Result<TskInspectResult, String> {
    if options.max_entries == 0 {
        return Err("--max-entries must be greater than 0".to_string());
    }
    let image_path = canonical_image_path(image_path)?;
    let inspected_unix = now_unix()?;
    let mut warnings = Vec::new();

    let mmls_log_path = unique_path(
        &case_dir
            .join("evidence/logs")
            .join(format!("tsk-mmls-{inspected_unix}.txt")),
    );
    let mmls_args = vec![tsk_path_string(&image_path)];
    let mmls = run_capture(
        &options.mmls_bin,
        &["mmls"],
        &mmls_args,
        options.timeout_secs,
    );
    let partitions = match &mmls {
        Ok(output) if output.status_success => {
            write_text(&mmls_log_path, &output.combined_text())
                .map_err(|err| format!("failed to write mmls log: {err}"))?;
            parse_mmls_partitions(&output.stdout)
        }
        Ok(output) => {
            write_text(&mmls_log_path, &output.combined_text())
                .map_err(|err| format!("failed to write mmls log: {err}"))?;
            warnings.push(format!(
                "mmls did not complete successfully; using explicit/default offset: {}",
                output.stderr.trim()
            ));
            Vec::new()
        }
        Err(err) => {
            write_text(&mmls_log_path, &format!("mmls unavailable: {err}\n"))
                .map_err(|write_err| format!("failed to write mmls log: {write_err}"))?;
            warnings.push(format!(
                "mmls unavailable; using explicit/default offset: {err}"
            ));
            Vec::new()
        }
    };

    let partition_offset = choose_partition_offset(&partitions, options.partition_offset);
    // On a truncated GPT export TSK discards the partition table when the
    // first populated GPT entries start beyond the image bounds, leaving
    // only the protective-MBR (0xee) slot. Running fls against that slot
    // always fails with a misleading "cannot determine file system type",
    // so surface the real cause and the remedy instead.
    if options.partition_offset.is_none() && is_protective_mbr_only(&partitions) {
        return Err(
            "mmls reports only the GPT protective (0xee) partition: the disk's GPT entries start beyond this image's bounds, which is characteristic of a truncated/partial export. Re-export with a larger --max-bytes, or pass --partition-offset explicitly if the filesystem data is present".to_string(),
        );
    }
    let fls_log_path = unique_path(
        &case_dir
            .join("evidence/logs")
            .join(format!("tsk-fls-{inspected_unix}.txt")),
    );
    let fls_args = fls_args(&image_path, partition_offset);
    let fls = run_capture_streaming(
        &options.fls_bin,
        &["fls"],
        &fls_args,
        options.timeout_secs,
        fls_progress,
    )?;
    write_text(&fls_log_path, &fls.output.combined_text())
        .map_err(|err| format!("failed to write fls log: {err}"))?;
    let fls_completed = !fls.timed_out;
    if fls.timed_out {
        // A killed fls still produced complete per-line entries — the
        // partial listing is real evidence and must be preserved, but
        // every downstream artifact must know it is not a full walk.
        warnings.push(format!(
            "fls timed out after {}s with {} entries captured — the filesystem listing is PARTIAL, not complete; rerun with a larger --timeout (or --timeout 0) for full coverage",
            options.timeout_secs.unwrap_or_default(),
            fls.lines_seen
        ));
    } else if !fls.output.status_success {
        let mut message = format!(
            "fls failed at offset {}: {}",
            partition_offset,
            fls.output.stderr.trim()
        );
        // A truncated export can still hold the boot sector while $MFT or
        // the FAT lie past EOF; say so instead of leaving a bare
        // "cannot determine file system type".
        if let Some(partition) = partitions
            .iter()
            .find(|partition| partition.allocated && partition.start == partition_offset)
            && let Ok(metadata) = fs::metadata(&image_path)
            && partition.end >= metadata.len() / 512
        {
            message.push_str(
                " (the selected partition extends beyond the image bounds — filesystem metadata may be outside a partial export; retry with a larger --max-bytes)",
            );
        }
        return Err(message);
    }

    let mut entries = parse_fls_entries(&fls.output.stdout);
    let entries_truncated = entries.len() > options.max_entries;
    if entries_truncated {
        warnings.push(format!(
            "filesystem listing truncated from {} to {} entries",
            entries.len(),
            options.max_entries
        ));
        entries.truncate(options.max_entries);
    }

    let entries_jsonl_path = unique_path(
        &case_dir
            .join("db/filesystem")
            .join(format!("tsk-files-{inspected_unix}.jsonl")),
    );
    let jsonl = entries
        .iter()
        .map(FlsEntry::to_json)
        .collect::<Vec<_>>()
        .join("\n");
    write_text(&entries_jsonl_path, &(jsonl + "\n"))
        .map_err(|err| format!("failed to write filesystem entries: {err}"))?;

    let summary_path = unique_path(
        &case_dir
            .join("db/filesystem")
            .join(format!("tsk-inspection-{inspected_unix}.json")),
    );
    let summary = inspect_summary_json(&InspectSummaryInput {
        image_path: &image_path,
        inspected_unix,
        partition_offset,
        partitions: &partitions,
        entries: &entries,
        fls_completed,
        warnings: &warnings,
        mmls_log_path: &mmls_log_path,
        fls_log_path: &fls_log_path,
        entries_jsonl_path: &entries_jsonl_path,
    });
    write_text(&summary_path, &summary)
        .map_err(|err| format!("failed to write filesystem inspection summary: {err}"))?;

    append_tsk_audit(
        case_dir,
        &format!(
            "{{\"schema_version\":1,\"event\":\"inspect-image-filesystem\",\"inspected_unix\":{},\"image_path\":\"{}\",\"partition_offset\":{},\"partition_count\":{},\"entry_count\":{},\"deleted_count\":{},\"video_candidate_count\":{},\"fls_completed\":{},\"mmls_version\":\"{}\",\"fls_version\":\"{}\",\"mmls_log_path\":\"{}\",\"fls_log_path\":\"{}\",\"entries_jsonl_path\":\"{}\",\"summary_path\":\"{}\",\"warnings\":{}}}",
            inspected_unix,
            json_escape(&image_path.to_string_lossy()),
            partition_offset,
            partitions.len(),
            entries.len(),
            entries.iter().filter(|entry| entry.deleted).count(),
            entries.iter().filter(|entry| entry.video_candidate).count(),
            fls_completed,
            json_escape(&tsk_command_version(&options.mmls_bin, &["mmls"])),
            json_escape(&tsk_command_version(&options.fls_bin, &["fls"])),
            json_escape(&mmls_log_path.to_string_lossy()),
            json_escape(&fls_log_path.to_string_lossy()),
            json_escape(&entries_jsonl_path.to_string_lossy()),
            json_escape(&summary_path.to_string_lossy()),
            audit::json_string_array(&warnings)
        ),
    )?;

    Ok(TskInspectResult {
        image_path,
        inspected_unix,
        partition_offset,
        partitions,
        entries,
        fls_completed,
        entries_truncated,
        warnings,
        mmls_log_path,
        fls_log_path,
        entries_jsonl_path,
        summary_path,
    })
}

pub fn recover_inode(
    case_dir: &Path,
    image_path: &Path,
    options: &TskRecoverOptions,
) -> Result<TskRecoverResult, String> {
    let image_path = canonical_image_path(image_path)?;
    if options.inode.trim().is_empty() {
        return Err("inode must not be empty".to_string());
    }
    let recovered_unix = now_unix()?;
    let output_path = match &options.output_path {
        Some(path) => {
            if path.exists() {
                return Err(format!(
                    "output already exists: {} (choose a new --output path)",
                    path.display()
                ));
            }
            path.to_path_buf()
        }
        // Not reserved: no empty placeholder under the final name (C2).
        None => crate::util::unique_available_path(
            &case_dir
                .join("artifacts/recovered/filesystem")
                .join(format!("inode_{}.bin", sanitize_filename(&options.inode))),
        ),
    };
    require_case_output_path(case_dir, &output_path, "inode recovery")?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create recovery output directory: {err}"))?;
        for (path, result) in crate::join::sweep_stale_parts(parent) {
            if let Err(err) = result {
                eprintln!("warning: {err} ({})", path.display());
            }
        }
    }

    let args = icat_args(&image_path, options);
    let icat_bin = resolve_tool_binary(&options.icat_bin, &["icat"])
        .map_err(|err| format!("{err} (install Sleuth Kit and ensure icat is in PATH)"))?;
    // C2: icat streams into a pid-tagged `.part`; only a complete recovery
    // is published under the final name (a killed recovery leaves just the
    // `.part`, which the server sweeps).
    let part_path = crate::join::pid_part_path(&output_path, std::process::id());
    crate::join::remove_file_retry(&part_path)?;
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part_path)
        .map_err(|err| format!("failed to create {}: {err}", part_path.display()))?;
    let final_path = output_path;
    let output_path = part_path.clone();
    let mut command = Command::new(&icat_bin);
    command
        .args(&args)
        .stdout(Stdio::from(output))
        .stderr(Stdio::piped());
    let mut child = crate::util::hide_console(&mut command)
        .spawn()
        .map_err(|err| format!("failed to run {}: {err}", options.icat_bin))?;
    let mut stderr_pipe = child.stderr.take().expect("piped icat stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr_pipe, &mut buffer);
        buffer
    });
    // icat writes the recovered bytes straight to disk; a timeout must never
    // leave that partial file looking like a successful recovery.
    let status = match options.timeout_secs {
        Some(secs) => match child
            .wait_timeout(std::time::Duration::from_secs(secs))
            .map_err(|err| format!("failed to wait for icat: {err}"))?
        {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&output_path);
                return Err(format!(
                    "icat did not finish within {secs}s and was terminated (retry with a larger --timeout)"
                ));
            }
        },
        None => child
            .wait()
            .map_err(|err| format!("failed to wait for icat: {err}"))?,
    };
    // C10: bounded — a grandchild holding the pipe must not hang us.
    let stderr_bytes =
        crate::join::join_reader_bounded(stderr_reader, crate::join::READER_JOIN_CAP)
            .unwrap_or_default();
    if !status.success() {
        let _ = fs::remove_file(&output_path);
        return Err(format!(
            "icat failed for inode {} at offset {}: {}",
            options.inode,
            options.partition_offset,
            String::from_utf8_lossy(&stderr_bytes).trim()
        ));
    }
    let explicit_output = options.output_path.is_some();
    let mut target = final_path;
    let mut published = false;
    for _ in 0..16 {
        match crate::join::publish_part_as(&output_path, &target) {
            Ok(true) => {
                published = true;
                break;
            }
            Ok(false) if !explicit_output => {
                // Another recovery took the name meanwhile: next free one.
                target = crate::util::unique_available_path(&target);
            }
            Ok(false) => break,
            Err(err) => {
                let _ = fs::remove_file(&output_path);
                return Err(err);
            }
        }
    }
    if !published {
        let _ = fs::remove_file(&output_path);
        return Err(format!(
            "output already exists: {} (choose a new --output path)",
            target.display()
        ));
    }
    let output_path = target;

    let output_path = canonicalize_display(&output_path)
        .map_err(|err| format!("failed to canonicalize recovered output: {err}"))?;
    let size_bytes = fs::metadata(&output_path)
        .map_err(|err| format!("failed to read recovered output metadata: {err}"))?
        .len();
    let sha256 = audit::digest_file(&output_path)?;
    let validation_status = "candidate-unvalidated".to_string();
    let mut warnings = Vec::new();
    // An all-zero recovery means the clusters are unallocated or were
    // TRIMmed — routine on SSD images — and must not be reported as
    // recovered content.
    if size_bytes > 0 && file_is_all_zero(&output_path)? {
        warnings.push(
            "recovered content is entirely zero bytes — the clusters are likely unallocated or TRIMmed (common on SSD images); do not report this as recovered content".to_string(),
        );
    }

    append_tsk_audit(
        case_dir,
        &format!(
            "{{\"schema_version\":1,\"event\":\"recover-inode\",\"recovered_unix\":{},\"image_path\":\"{}\",\"partition_offset\":{},\"inode\":\"{}\",\"output_path\":\"{}\",\"size_bytes\":{},\"sha256\":\"{}\",\"validation_status\":\"{}\",\"recover_deleted\":{},\"include_slack\":{},\"skip_sparse_holes\":{},\"icat_version\":\"{}\",\"command\":\"{}\",\"command_args\":{},\"warnings\":{}}}",
            recovered_unix,
            json_escape(&image_path.to_string_lossy()),
            options.partition_offset,
            json_escape(&options.inode),
            json_escape(&output_path.to_string_lossy()),
            size_bytes,
            json_escape(&sha256),
            json_escape(&validation_status),
            options.recover_deleted,
            options.include_slack,
            options.skip_sparse_holes,
            json_escape(&tsk_command_version(&options.icat_bin, &["icat"])),
            json_escape(&options.icat_bin),
            audit::json_string_array(&args),
            audit::json_string_array(&warnings)
        ),
    )?;

    Ok(TskRecoverResult {
        image_path,
        output_path,
        recovered_unix,
        partition_offset: options.partition_offset,
        inode: options.inode.clone(),
        size_bytes,
        sha256,
        validation_status,
        warnings,
    })
}

/// Streams a file and reports whether every byte is zero — used to flag
/// recoveries that are all unallocated slack.
fn file_is_all_zero(path: &Path) -> Result<bool, String> {
    use std::io::Read;
    let mut file =
        File::open(path).map_err(|err| format!("failed to read recovered output: {err}"))?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| format!("failed to read recovered output: {err}"))?;
        if read == 0 {
            return Ok(true);
        }
        if buffer[..read].iter().any(|byte| *byte != 0) {
            return Ok(false);
        }
    }
}

struct InspectSummaryInput<'a> {
    image_path: &'a Path,
    inspected_unix: u64,
    partition_offset: u64,
    partitions: &'a [MmlsPartition],
    entries: &'a [FlsEntry],
    fls_completed: bool,
    warnings: &'a [String],
    mmls_log_path: &'a Path,
    fls_log_path: &'a Path,
    entries_jsonl_path: &'a Path,
}

fn inspect_summary_json(input: &InspectSummaryInput<'_>) -> String {
    let partitions_json = input
        .partitions
        .iter()
        .map(MmlsPartition::to_json)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\n  \"schema_version\": 1,\n  \"image_path\": \"{}\",\n  \"inspected_unix\": {},\n  \"partition_offset\": {},\n  \"partition_count\": {},\n  \"entry_count\": {},\n  \"deleted_count\": {},\n  \"video_candidate_count\": {},\n  \"fls_completed\": {},\n  \"warnings\": {},\n  \"mmls_log_path\": \"{}\",\n  \"fls_log_path\": \"{}\",\n  \"entries_jsonl_path\": \"{}\",\n  \"partitions\": [{}]\n}}\n",
        json_escape(&input.image_path.to_string_lossy()),
        input.inspected_unix,
        input.partition_offset,
        input.partitions.len(),
        input.entries.len(),
        input.entries.iter().filter(|entry| entry.deleted).count(),
        input
            .entries
            .iter()
            .filter(|entry| entry.video_candidate)
            .count(),
        input.fls_completed,
        audit::json_string_array(input.warnings),
        json_escape(&input.mmls_log_path.to_string_lossy()),
        json_escape(&input.fls_log_path.to_string_lossy()),
        json_escape(&input.entries_jsonl_path.to_string_lossy()),
        partitions_json
    )
}

fn append_tsk_audit(case_dir: &Path, body_json: &str) -> Result<(), String> {
    audit::append_chained_jsonl(&case_dir.join("evidence/logs/tsk-audit.jsonl"), body_json)
}

fn canonical_image_path(path: &Path) -> Result<PathBuf, String> {
    // libewf-backed tools (mmls/fls/icat reading E01 directly) cannot open
    // relative or mixed-separator paths — canonicalize to an absolute,
    // separator-normalized, non-extended path before spawning them.
    let path = crate::util::canonicalize_display(path).map_err(|err| {
        format!(
            "failed to canonicalize image path {}: {err}",
            path.display()
        )
    })?;
    if !path.is_file() {
        return Err(format!("forensic image is not a file: {}", path.display()));
    }
    Ok(path)
}

fn run_capture(
    binary: &str,
    allowed: &[&str],
    args: &[String],
    timeout_secs: Option<u64>,
) -> Result<CommandOutput, String> {
    let resolved_binary = resolve_tool_binary(binary, allowed)
        .map_err(|err| format!("{err} (install Sleuth Kit and ensure {binary} is in PATH)"))?;
    let mut command = Command::new(&resolved_binary);
    command.args(args);
    let output = crate::util::run_with_timeout(&mut command, timeout_secs)
        .map_err(|err| format!("failed to run {binary}: {err}"))?;
    if let Some(secs) = timeout_secs
        && !output.status.success()
    {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("did not finish within") || stderr.contains("terminated") {
            return Err(format!(
                "{binary} did not finish within {secs}s and was terminated (retry with a larger --timeout)"
            ));
        }
    }
    Ok(CommandOutput {
        status_success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

/// Timeout-aware capture that counts stdout lines as they stream —
/// fls's full-filesystem walk can take ~25 minutes on a ~230GiB E01,
/// and callers need live entry counts plus the partial output a killed
/// run still produced. Unlike `run_capture`/`run_with_timeout`, a
/// timeout returns Ok with `timed_out: true` instead of discarding the
/// partial buffer.
struct StreamedOutput {
    output: CommandOutput,
    /// The timeout killed the child; `output.stdout` holds every line
    /// captured before termination (each complete line is still usable).
    timed_out: bool,
    /// Newlines seen on stdout before termination (≈ fls entry count).
    lines_seen: u64,
}

fn run_capture_streaming(
    binary: &str,
    allowed: &[&str],
    args: &[String],
    timeout_secs: Option<u64>,
    on_lines: Option<&(dyn Fn(u64) + Send + Sync)>,
) -> Result<StreamedOutput, String> {
    let resolved_binary = resolve_tool_binary(binary, allowed)
        .map_err(|err| format!("{err} (install Sleuth Kit and ensure {binary} is in PATH)"))?;
    let mut command = Command::new(&resolved_binary);
    command.args(args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = crate::util::hide_console(&mut command)
        .spawn()
        .map_err(|err| format!("failed to run {binary}: {err}"))?;
    drain_streaming(child, timeout_secs, on_lines)
}

/// Reads a piped child's stdout/stderr on dedicated threads, counting
/// stdout newlines as they arrive so long-running tools report live
/// progress. A timeout kills the child but keeps every line already
/// captured (`timed_out: true`); the caller decides whether partial
/// output is usable.
fn drain_streaming(
    mut child: std::process::Child,
    timeout_secs: Option<u64>,
    on_lines: Option<&(dyn Fn(u64) + Send + Sync)>,
) -> Result<StreamedOutput, String> {
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    std::thread::scope(|scope| {
        let stderr_reader = scope.spawn(move || {
            let mut buffer = Vec::new();
            let _ = std::io::Read::read_to_end(&mut stderr_pipe, &mut buffer);
            buffer
        });
        let stdout_reader = scope.spawn(move || {
            let mut buffer = Vec::new();
            let mut lines = 0u64;
            let mut last_report = 0u64;
            let mut chunk = [0u8; 16 * 1024];
            loop {
                match std::io::Read::read(&mut stdout_pipe, &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        buffer.extend_from_slice(&chunk[..read]);
                        lines += chunk[..read].iter().filter(|byte| **byte == b'\n').count() as u64;
                        if let Some(report) = on_lines
                            && lines - last_report >= 5000
                        {
                            report(lines);
                            last_report = lines;
                        }
                    }
                }
            }
            if let Some(report) = on_lines {
                report(lines);
            }
            (buffer, lines)
        });
        let status = match timeout_secs {
            Some(secs) => child
                .wait_timeout(std::time::Duration::from_secs(secs))
                .map_err(|err| format!("failed to wait for streamed tool: {err}"))?,
            None => Some(
                child
                    .wait()
                    .map_err(|err| format!("failed to wait for streamed tool: {err}"))?,
            ),
        };
        let timed_out = status.is_none();
        if timed_out {
            let _ = child.kill();
            let _ = child.wait();
        }
        let (stdout, lines_seen) = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        Ok(StreamedOutput {
            output: CommandOutput {
                status_success: status.map(|s| s.success()).unwrap_or(false),
                stdout: String::from_utf8_lossy(&stdout).to_string(),
                stderr: String::from_utf8_lossy(&stderr).to_string(),
            },
            timed_out,
            lines_seen,
        })
    })
}

#[derive(Debug, Clone)]
struct CommandOutput {
    status_success: bool,
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    fn combined_text(&self) -> String {
        format!(
            "status_success: {}\n\nstdout:\n{}\n\nstderr:\n{}\n",
            self.status_success, self.stdout, self.stderr
        )
    }
}

fn tsk_command_version(binary: &str, allowed: &[&str]) -> String {
    command_version(binary, allowed, "-V")
}

fn parse_mmls_partitions(text: &str) -> Vec<MmlsPartition> {
    text.lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 5 || !fields[0].ends_with(':') {
                return None;
            }
            let start = fields.get(2)?.parse::<u64>().ok()?;
            let end = fields.get(3)?.parse::<u64>().ok()?;
            let length = fields.get(4)?.parse::<u64>().ok()?;
            let slot_type = fields[1];
            let description = if fields.len() > 5 {
                fields[5..].join(" ")
            } else {
                String::new()
            };
            let description_lc = description.to_ascii_lowercase();
            let allocated = slot_type != "Meta"
                && slot_type != "-------"
                && !description_lc.contains("unallocated")
                && !description_lc.contains("primary table");
            Some(MmlsPartition {
                slot: fields[0].trim_end_matches(':').to_string(),
                start,
                end,
                length,
                description,
                allocated,
            })
        })
        .collect()
}

/// Picks the partition to inspect. Auto-selection prefers partitions whose
/// mmls description names a data filesystem (NTFS/exFAT/FAT/HFS+/APFS/ext)
/// — on a GPT disk the first allocated partition is typically the tiny
/// EFI System Partition and on DOS layouts the first is often an Extended
/// Table container; both would yield an (almost) empty listing presented
/// as a successful inspection, i.e. a wrong-negative forensic result.
fn choose_partition_offset(partitions: &[MmlsPartition], explicit: Option<u64>) -> u64 {
    explicit.unwrap_or_else(|| {
        let allocated: Vec<&MmlsPartition> = partitions
            .iter()
            .filter(|partition| partition.allocated)
            .collect();
        let looks_like_data_fs = |partition: &MmlsPartition| {
            let description = partition.description.to_ascii_lowercase();
            // EFI System Partitions are FAT but hold the boot loader, not
            // evidence; excluding them before the data-filesystem match is
            // what makes GPT auto-selection correct.
            if description.contains("efi system") {
                return false;
            }
            [
                "ntfs",
                "exfat",
                "fat",
                "hfs",
                "apfs",
                "ext2",
                "ext3",
                "ext4",
                "linux filesystem",
            ]
            .iter()
            .any(|fs| description.contains(fs))
        };
        // When no description matches a filesystem keyword — e.g. win32
        // mmls prints an empty/mojibake name for a non-ASCII GPT label —
        // fall back to the largest allocated partition that is not a
        // known container/reserved slot, not simply the first allocated
        // one (which on GPT is usually the tiny Microsoft reserved
        // partition and holds no filesystem).
        let is_system_slot = |partition: &MmlsPartition| {
            let description = partition.description.to_ascii_lowercase();
            description.contains("reserved partition")
                || description.contains("bios boot")
                || description.contains("efi system")
                || is_gpt_safety_partition(partition)
        };
        allocated
            .iter()
            .find(|partition| looks_like_data_fs(partition))
            .copied()
            .or_else(|| {
                allocated
                    .iter()
                    .copied()
                    .filter(|partition| !is_system_slot(partition))
                    .max_by_key(|partition| partition.length)
            })
            .or_else(|| allocated.first().copied())
            .map(|partition| partition.start)
            .unwrap_or(0)
    })
}

/// The protective-MBR 0xee slot mmls prints for a GPT disk — a metadata
/// placeholder, never a filesystem-bearing partition.
fn is_gpt_safety_partition(partition: &MmlsPartition) -> bool {
    let description = partition.description.to_ascii_lowercase();
    description.contains("safety partition") || description.contains("(0xee)")
}

/// True when every allocated slot is the GPT protective partition — what
/// mmls shows for a truncated GPT export whose real partition entries lie
/// beyond the image bounds (TSK then discards the whole table).
fn is_protective_mbr_only(partitions: &[MmlsPartition]) -> bool {
    let allocated: Vec<&MmlsPartition> = partitions
        .iter()
        .filter(|partition| partition.allocated)
        .collect();
    !allocated.is_empty()
        && allocated
            .iter()
            .all(|partition| is_gpt_safety_partition(partition))
}

fn parse_fls_entries(text: &str) -> Vec<FlsEntry> {
    text.lines().filter_map(parse_fls_entry).collect()
}

fn parse_fls_entry(line: &str) -> Option<FlsEntry> {
    let raw = line.trim();
    if raw.is_empty() {
        return None;
    }
    let (left, right) = raw.split_once(':')?;
    let tokens = left.split_whitespace().collect::<Vec<_>>();
    let file_type = tokens
        .iter()
        .find(|token| token.contains('/'))
        .map(|token| token.trim_matches('+').to_string());
    let inode = tokens
        .iter()
        .rev()
        .find(|token| token.chars().any(|ch| ch.is_ascii_digit()))
        .map(|token| token.trim_matches('+').to_string());
    let path = right.trim().to_string();
    let deleted = tokens.contains(&"*") || path.contains("(deleted)");
    let video_candidate = path_has_video_extension(&path);

    Some(FlsEntry {
        raw_line: raw.to_string(),
        file_type,
        inode,
        path: Some(path),
        deleted,
        video_candidate,
    })
}

fn path_has_video_extension(path: &str) -> bool {
    let Some((_, extension)) = path.rsplit_once('.') else {
        return false;
    };
    let extension = extension
        .trim()
        .trim_end_matches("(deleted)")
        .trim()
        .to_ascii_lowercase();
    VIDEO_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

fn fls_args(image_path: &Path, offset: u64) -> Vec<String> {
    vec![
        "-r".to_string(),
        "-p".to_string(),
        "-o".to_string(),
        offset.to_string(),
        tsk_path_string(image_path),
    ]
}

fn icat_args(image_path: &Path, options: &TskRecoverOptions) -> Vec<String> {
    let mut args = Vec::new();
    if options.skip_sparse_holes {
        args.push("-h".to_string());
    }
    if options.recover_deleted {
        args.push("-r".to_string());
    }
    if options.include_slack {
        args.push("-s".to_string());
    }
    args.extend([
        "-o".to_string(),
        options.partition_offset.to_string(),
        tsk_path_string(image_path),
        options.inode.clone(),
    ]);
    args
}

fn tsk_path_string(path: &Path) -> String {
    let raw = audit::path_string(path);
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        TskRecoverOptions, choose_partition_offset, drain_streaming, fls_args, icat_args,
        is_protective_mbr_only, parse_fls_entry, parse_mmls_partitions,
    };
    use std::path::Path;
    use std::process::{Command, Stdio};

    #[test]
    fn parses_mmls_allocated_partition_offsets() {
        let text = "\
DOS Partition Table
Offset Sector: 0
Units are in 512-byte sectors

      Slot      Start        End          Length       Description
000:  Meta      0000000000   0000000000   0000000001   Primary Table (#0)
001:  -------   0000000000   0000002047   0000002048   Unallocated
002:  000:000   0000002048   0004095999   0004093952   NTFS / exFAT (0x07)
";
        let partitions = parse_mmls_partitions(text);
        assert_eq!(partitions.len(), 3);
        assert_eq!(choose_partition_offset(&partitions, None), 2048);
        assert_eq!(choose_partition_offset(&partitions, Some(63)), 63);
    }

    /// GPT disks list the small EFI System Partition first; auto-selection
    /// must skip it in favor of the data volume, or the examiner sees an
    /// (almost) empty listing presented as a successful inspection.
    #[test]
    fn choose_partition_skips_efi_system_partition() {
        let text = "\
GUID Partition Table (EFI)
Offset Sector: 0
Units are in 512-byte sectors

      Slot      Start        End          Length       Description
000:  Meta      0000000000   0000000000   0000000001   Safety Table
001:  -------   0000000000   0000002047   0000002048   Unallocated
002:  000:000   0000002048   0000004111   0000002064   EFI System Partition (FAT)
003:  000:001   0000004112   0209715199   0209711088   NTFS / exFAT (0x07)
";
        let partitions = parse_mmls_partitions(text);
        assert_eq!(choose_partition_offset(&partitions, None), 4112);
    }

    /// win32 mmls prints an empty description for GPT names it cannot
    /// render (non-ASCII), so the data partition never matches a
    /// filesystem keyword. Auto-selection must then take the largest
    /// allocated non-reserved partition — not the tiny Microsoft reserved
    /// partition that sorts first (real case: #914_260812_PC_SSD_004).
    #[test]
    fn choose_partition_skips_microsoft_reserved_with_blank_description() {
        let text = "\
GUID Partition Table (EFI)
Offset Sector: 0
Units are in 512-byte sectors

      Slot      Start        End          Length       Description
000:  Meta      0000000000   0000000000   0000000001   Safety Table
001:  Meta      0000000001   0000000001   0000000001   GPT Header
002:  Meta      0000000002   0000000033   0000000032   Partition Table
003:  000       0000000034   0000032767   0000032734   Microsoft reserved partition
004:  001       0000032768   0976773119   0976740352
";
        let partitions = parse_mmls_partitions(text);
        assert_eq!(choose_partition_offset(&partitions, None), 32768);
    }

    /// A DOS extended layout lists the container ("Extended Table") before
    /// the logical data volume; the container must not be chosen.
    #[test]
    fn choose_partition_skips_extended_table_container() {
        let text = "\
DOS Partition Table
Offset Sector: 0
Units are in 512-byte sectors

      Slot      Start        End          Length       Description
000:  Meta      0000000000   0000000000   0000000001   Primary Table (#0)
001:  000:000   0000002048   0004116223   0004114176   DOS Extended (0x05)
002:  001:000   0004116480   1000215215   0996098736   NTFS / exFAT (0x07)
";
        let partitions = parse_mmls_partitions(text);
        assert_eq!(choose_partition_offset(&partitions, None), 4116480);
    }

    /// On a truncated GPT export TSK discards the partition table when the
    /// first populated entries start beyond the image bounds, so mmls
    /// prints only the protective-MBR 0xee slot. That layout must be
    /// detected as "protective only" so the examiner gets a clear cause
    /// instead of `fls failed at offset 1` (real case: 16 MiB prefix of
    /// #462_250530_BSH_PC1).
    #[test]
    fn detects_protective_mbr_only_truncated_gpt() {
        let text = "\
DOS Partition Table
Offset Sector: 0
Units are in 512-byte sectors

      Slot      Start        End          Length       Description
000:  Meta      0000000000   0000000000   0000000001   Primary Table (#0)
001:  -------   0000000000   0000000000   0000000001   Unallocated
002:  000:000   0000000001   4294967295   4294967295   GPT Safety Partition (0xee)
";
        let partitions = parse_mmls_partitions(text);
        assert!(is_protective_mbr_only(&partitions));
    }

    /// A normal DOS layout with a real data partition is not
    /// protective-only, and a GPT disk that did parse keeps its real
    /// partitions eligible.
    #[test]
    fn normal_layouts_are_not_protective_only() {
        let dos = "\
DOS Partition Table
Offset Sector: 0
Units are in 512-byte sectors

      Slot      Start        End          Length       Description
000:  Meta      0000000000   0000000000   0000000001   Primary Table (#0)
001:  -------   0000000000   0000002047   0000002048   Unallocated
002:  000:000   0000002048   0004095999   0004093952   NTFS / exFAT (0x07)
";
        assert!(!is_protective_mbr_only(&parse_mmls_partitions(dos)));
        assert!(!is_protective_mbr_only(&[]));
    }

    #[test]
    fn parses_deleted_fls_video_entries() {
        let entry =
            parse_fls_entry("r/r * 1304-128-1: /BLACKBOX/event001.mp4 (deleted)").expect("entry");
        assert_eq!(entry.file_type.as_deref(), Some("r/r"));
        assert_eq!(entry.inode.as_deref(), Some("1304-128-1"));
        assert_eq!(
            entry.path.as_deref(),
            Some("/BLACKBOX/event001.mp4 (deleted)")
        );
        assert!(entry.deleted);
        assert!(entry.video_candidate);
    }

    #[test]
    fn builds_tsk_command_args() {
        let image = Path::new("/cases/image.raw");
        assert_eq!(
            fls_args(image, 2048),
            vec!["-r", "-p", "-o", "2048", "/cases/image.raw"]
        );

        let options = TskRecoverOptions {
            partition_offset: 2048,
            inode: "1304-128-1".to_string(),
            output_path: None,
            recover_deleted: true,
            include_slack: false,
            skip_sparse_holes: true,
            icat_bin: "icat".to_string(),
            timeout_secs: None,
        };
        assert_eq!(
            icat_args(image, &options),
            vec!["-h", "-r", "-o", "2048", "/cases/image.raw", "1304-128-1"]
        );
    }

    /// Spawns a shell child that prints `lines` to stdout and then sleeps
    /// `sleep_secs` before exiting — a stand-in for fls emitting entries
    /// during a long walk. Tests drive `drain_streaming` directly so the
    /// tool-whitelist policy never enters the picture.
    #[cfg(windows)]
    fn spawn_line_printer(lines: &[&str], sleep_secs: u64) -> std::process::Child {
        let mut script = lines
            .iter()
            .map(|line| format!("echo {line}"))
            .collect::<Vec<_>>()
            .join("& ");
        if sleep_secs > 0 {
            // `timeout /t` refuses redirected stdin; ping pacing is the
            // stock Windows sleep that survives Stdio::null().
            script.push_str(&format!("& ping -n {sleep_secs} 127.0.0.1 >NUL"));
        }
        Command::new("cmd")
            .args(["/C", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn line printer")
    }

    #[cfg(unix)]
    fn spawn_line_printer(lines: &[&str], sleep_secs: u64) -> std::process::Child {
        let mut script = lines
            .iter()
            .map(|line| format!("echo {line}"))
            .collect::<Vec<_>>()
            .join("; ");
        if sleep_secs > 0 {
            script.push_str(&format!("; sleep {sleep_secs}"));
        }
        Command::new("sh")
            .args(["-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn line printer")
    }

    #[test]
    fn streaming_capture_counts_lines_and_reports_progress() {
        use std::sync::{Arc, Mutex};
        let reports = Arc::new(Mutex::new(Vec::new()));
        let reports_clone = reports.clone();
        let progress = move |lines: u64| reports_clone.lock().unwrap().push(lines);
        let child = spawn_line_printer(&["alpha", "beta", "gamma"], 0);
        let result = drain_streaming(child, Some(30), Some(&progress)).expect("drained");
        assert!(result.output.status_success);
        assert!(!result.timed_out);
        assert_eq!(result.lines_seen, 3);
        assert!(result.output.stdout.contains("alpha"));
        assert!(result.output.stdout.contains("gamma"));
        // The reader always reports the final line count once, even below
        // the 5000-line incremental threshold.
        assert_eq!(*reports.lock().unwrap(), vec![3]);
    }

    #[test]
    fn streaming_capture_preserves_partial_output_on_timeout() {
        // 5s sleep with a 3s timeout: cmd's own startup latency must not
        // race the kill (it did at 1s under parallel test load), and the
        // grandchild (ping on Windows) keeps the stdout pipe open until
        // it exits — enough to prove partial capture without a
        // minute-long test.
        let child = spawn_line_printer(&["partial-entry"], 5);
        let result = drain_streaming(child, Some(3), None).expect("drained");
        assert!(result.timed_out);
        assert!(!result.output.status_success);
        // The line emitted before the kill must survive in the partial
        // buffer — a timed-out listing is evidence, not an error.
        assert!(result.lines_seen >= 1);
        assert!(result.output.stdout.contains("partial-entry"));
    }

    #[test]
    fn streaming_capture_distinguishes_nonzero_exit_from_timeout() {
        #[cfg(windows)]
        let child = Command::new("cmd")
            .args(["/C", "echo doomed& exit /b 3"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn failing child");
        #[cfg(unix)]
        let child = Command::new("sh")
            .args(["-c", "echo doomed; exit 3"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn failing child");
        let result = drain_streaming(child, Some(30), None).expect("drained");
        assert!(!result.timed_out);
        assert!(!result.output.status_success);
        assert!(result.output.stdout.contains("doomed"));
    }

    fn inspect_result(fls_completed: bool, entries_truncated: bool) -> super::TskInspectResult {
        super::TskInspectResult {
            image_path: Path::new("img.raw").to_path_buf(),
            inspected_unix: 1,
            partition_offset: 0,
            partitions: Vec::new(),
            entries: Vec::new(),
            fls_completed,
            entries_truncated,
            warnings: Vec::new(),
            mmls_log_path: Path::new("m").to_path_buf(),
            fls_log_path: Path::new("f").to_path_buf(),
            entries_jsonl_path: Path::new("e").to_path_buf(),
            summary_path: Path::new("s").to_path_buf(),
        }
    }

    /// C7 / B16: a timed-out or truncated listing is PARTIAL (exit 5),
    /// never a clean completion.
    #[test]
    fn partial_reason_table() {
        use super::{EXIT_PARTIAL, partial_reason};
        assert_eq!(EXIT_PARTIAL, 5);
        assert_eq!(partial_reason(&inspect_result(true, false)), None);
        assert!(
            partial_reason(&inspect_result(false, false))
                .unwrap()
                .contains("timed out")
        );
        assert!(
            partial_reason(&inspect_result(true, true))
                .unwrap()
                .contains("truncated")
        );
        let both = partial_reason(&inspect_result(false, true)).unwrap();
        assert!(both.contains("timed out") && both.contains("truncated"));
    }

    /// C7: one hour per started terabyte, at least one hour; an EWF set
    /// counts all its segments.
    #[test]
    fn inspect_timeout_scales_with_image_size() {
        use super::{scaled_inspect_timeout_secs, scaled_timeout_for_bytes};
        const TB: u64 = 1_000_000_000_000;
        assert_eq!(scaled_timeout_for_bytes(0), 3600);
        assert_eq!(scaled_timeout_for_bytes(232 * 1_000_000_000), 3600);
        assert_eq!(scaled_timeout_for_bytes(TB), 3600);
        assert_eq!(scaled_timeout_for_bytes(TB + 1), 7200);
        assert_eq!(scaled_timeout_for_bytes(4 * TB), 4 * 3600);
        let dir = std::env::temp_dir().join(format!("ft-c7-ewf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("disk.E01");
        std::fs::write(&first, vec![0u8; 10]).unwrap();
        std::fs::write(dir.join("disk.E02"), vec![0u8; 10]).unwrap();
        std::fs::write(dir.join("other.E01"), vec![0u8; 10]).unwrap();
        assert_eq!(super::image_size_for_timeout(&first), 20);
        assert_eq!(scaled_inspect_timeout_secs(&first), 3600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// C2: icat writes into a pid-tagged `.part`; a failed recovery leaves
    /// nothing, a successful one is published under the final name only.
    #[cfg(unix)]
    #[test]
    fn icat_recovery_publishes_only_complete_output() {
        use std::os::unix::fs::PermissionsExt;
        let case = std::env::temp_dir().join(format!("ft-c2-icat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&case);
        std::fs::create_dir_all(case.join("evidence/logs")).unwrap();
        let image = case.join("disk.raw");
        std::fs::write(&image, b"img").unwrap();
        let icat = |name: &str, body: &str| {
            let dir = case.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("icat");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.to_string_lossy().to_string()
        };
        let options = |bin: String| TskRecoverOptions {
            partition_offset: 0,
            inode: "42".to_string(),
            output_path: None,
            recover_deleted: false,
            include_slack: false,
            skip_sparse_holes: false,
            icat_bin: bin,
            timeout_secs: Some(30),
        };
        let out_dir = case.join("artifacts/recovered/filesystem");
        let failing = icat("fail", "printf 'partial'; exit 1");
        assert!(super::recover_inode(&case, &image, &options(failing)).is_err());
        let names = |dir: &Path| -> Vec<String> {
            std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default()
        };
        assert!(names(&out_dir).is_empty(), "{:?}", names(&out_dir));
        let working = icat("ok", "printf 'recovered-bytes'");
        let result = super::recover_inode(&case, &image, &options(working)).unwrap();
        assert!(result.output_path.ends_with("inode_42.bin"));
        assert_eq!(names(&out_dir), ["inode_42.bin"]);
        assert_eq!(
            std::fs::read(&result.output_path).unwrap(),
            b"recovered-bytes"
        );
        let _ = std::fs::remove_dir_all(&case);
    }
}
