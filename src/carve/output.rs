use super::*;

pub(super) fn copy_range(
    source: &Path,
    offset: u64,
    size_bytes: u64,
    output: &Path,
) -> io::Result<()> {
    copy_ranges(source, &[(offset, size_bytes)], output)
}

/// Concatenate several source ranges into one output file — the physical
/// form of a reassembly hypothesis (head + continuation fragments).
pub(super) fn copy_ranges(source: &Path, ranges: &[(u64, u64)], output: &Path) -> io::Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut input = File::open(source)?;
    let output_file = File::create(output)?;
    let mut writer = BufWriter::new(output_file);
    for (offset, size_bytes) in ranges {
        input.seek(SeekFrom::Start(*offset))?;
        let mut reader = (&mut input).take(*size_bytes);
        let copied = io::copy(&mut reader, &mut writer)?;
        if copied != *size_bytes {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "short read carving {size_bytes} bytes at offset {offset}: copied {copied}"
                ),
            ));
        }
    }
    writer.flush()?;
    // Evidence copies must be durable before they are logged/hashed.
    writer.get_ref().sync_all()
}

/// Writes the results index and appends one `carve-file` line per artifact.
///
/// `replayed_outputs` are artifacts replayed from a checkpoint. If the run
/// that carved them died AFTER appending their log lines but BEFORE its
/// checkpoint was removed, those lines are already in the carve log; a
/// replayed artifact whose (id, output path, sha256) is already logged is
/// not appended again, so a resume never duplicates artifact entries.
/// Fresh artifacts (always a new unique output path) are always logged.
pub(super) fn write_carve_outputs(
    case_dir: &Path,
    result: &CarveResult,
    replayed_outputs: &[PathBuf],
) -> Result<(), String> {
    write_text_atomic(&case_dir.join("db/carve_results.json"), &result.to_json())
        .map_err(|err| format!("failed to write carve results: {err}"))?;

    let log_path = case_dir.join("artifacts/carved/carve-log.jsonl");
    let already_logged = if replayed_outputs.is_empty() {
        Vec::new()
    } else {
        logged_carve_files(&log_path)?
    };
    for artifact in &result.artifacts {
        let key = (
            artifact.id.clone(),
            artifact.output_path.to_string_lossy().to_string(),
            artifact.sha256.to_ascii_lowercase(),
        );
        if replayed_outputs.contains(&artifact.output_path) && already_logged.contains(&key) {
            continue;
        }
        audit::append_chained_jsonl(&log_path, &artifact.to_json())?;
    }
    Ok(())
}

/// (id, output_path, lowercase sha256) of every `carve-file` entry already
/// in the carve log. A missing log is empty; an unreadable one is an error
/// (never guess that nothing was logged).
fn logged_carve_files(log_path: &Path) -> Result<Vec<(String, String, String)>, String> {
    // Through the audit module's lock-aware reader: on Windows the
    // appender's byte-range lock is mandatory, so a plain second-handle
    // read during an append would fail with ERROR_LOCK_VIOLATION.
    // R9 P3: through the one lane reader (lock-aware); only entries usable
    // here count as "already logged" — an unauthenticated entry never
    // stops a resumed artifact from getting its own authenticated record.
    let lane = audit::read_lane_rows(log_path).map_err(|err| {
        format!(
            "failed to read carve log {} to de-duplicate resumed artifacts: {err}",
            log_path.display()
        )
    })?;
    let mut out = Vec::new();
    for row in lane.usable() {
        let value = &row.value;
        if value.get("event").and_then(|v| v.as_str()) != Some("carve-file") {
            continue;
        }
        let field = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        out.push((
            field("id"),
            field("output_path"),
            field("sha256").to_ascii_lowercase(),
        ));
    }
    Ok(out)
}
