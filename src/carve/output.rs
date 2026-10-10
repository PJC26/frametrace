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
        io::copy(&mut reader, &mut writer)?;
    }
    writer.flush()
}

pub(super) fn write_carve_outputs(case_dir: &Path, result: &CarveResult) -> Result<(), String> {
    write_text_atomic(&case_dir.join("db/carve_results.json"), &result.to_json())
        .map_err(|err| format!("failed to write carve results: {err}"))?;

    let log_path = case_dir.join("artifacts/carved/carve-log.jsonl");
    for artifact in &result.artifacts {
        audit::append_chained_jsonl(&log_path, &artifact.to_json())?;
    }
    Ok(())
}
