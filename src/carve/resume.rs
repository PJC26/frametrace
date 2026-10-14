use super::*;

/// Fingerprint of everything that decides carve output: canonical source
/// path, size+mtime, and the option set.
pub(super) fn carve_fingerprint(
    source_path: &Path,
    metadata: &std::fs::Metadata,
    options: &CarveOptions,
) -> String {
    let modified_unix = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    checkpoint::fingerprint(&[
        "carve-file",
        &source_path.to_string_lossy(),
        &metadata.len().to_string(),
        &modified_unix
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        &options.max_bytes.to_string(),
        &options.max_candidates.to_string(),
        &options.reassemble.to_string(),
        &options
            .scan_offset
            .map(|value| value.to_string())
            .unwrap_or_default(),
        &options
            .scan_length
            .map(|value| value.to_string())
            .unwrap_or_default(),
    ])
}

/// Decoded prior-run carve progress: the latest signature-scan position
/// plus every completed artifact keyed by hit offset.
#[derive(Debug, Default)]
pub(super) struct CarveCheckpointState {
    /// Byte offset the signature scan had reached (0 = not started).
    pub(super) scan_offset: u64,
    /// Cumulative hits at that offset (each `scan` line carries the full
    /// set found so far; the last line wins).
    pub(super) hits: Vec<CarveHit>,
    /// Artifacts already carved, keyed by their hit's offset.
    pub(super) carved: HashMap<u64, CarvedArtifact>,
    /// Scan suppression state at the checkpoint: without these a resume
    /// inside a RIFF-declared span forgets the container boundary and
    /// re-floods `hits` with embedded Annex-B/JPEG anchors.
    pub(super) embedded_until: u64,
    pub(super) annexb_next_emit: u64,
    /// RIFF-declared suppression spans recorded so far (E3).
    pub(super) suppressed_spans: Vec<(u64, u64)>,
}

/// Decodes a prior run's carve checkpoint lines into `CarveCheckpointState`.
pub(super) fn load_carve_checkpoint(
    checkpoint: &RunCheckpoint,
) -> Result<CarveCheckpointState, String> {
    let corrupt = |detail: String| {
        format!(
            "checkpoint {} is corrupt ({detail}); delete it or rerun with --no-resume",
            checkpoint.path().display()
        )
    };
    let mut state = CarveCheckpointState::default();
    for line in checkpoint.data_lines() {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|err| corrupt(format!("record line does not parse: {err}")))?;
        if let Some(scan) = value.get("scan") {
            state.scan_offset = scan
                .get("offset")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| corrupt("scan record is missing offset".to_string()))?;
            // Absent in checkpoints from before suppression persisted —
            // resuming without it only risks hit flooding, not corruption.
            state.embedded_until = scan
                .get("embedded_until")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            state.annexb_next_emit = scan
                .get("annexb_next_emit")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            // Absent before E3: an older checkpoint resumes with no
            // recorded spans (its suppressed bytes go unreported, as they
            // did when that checkpoint was written).
            state.suppressed_spans = serde_json::from_value(
                scan.get("suppressed_spans")
                    .cloned()
                    .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
            )
            .map_err(|err| corrupt(format!("scan record suppressed_spans do not parse: {err}")))?;
            state.hits = serde_json::from_value(
                scan.get("hits").cloned().unwrap_or(serde_json::Value::Null),
            )
            .map_err(|err| corrupt(format!("scan record hits do not parse: {err}")))?;
        } else if let Some(carved_value) = value.get("carved") {
            let artifact: CarvedArtifact = serde_json::from_value(carved_value.clone())
                .map_err(|err| corrupt(format!("carved record does not parse: {err}")))?;
            state.carved.insert(artifact.offset, artifact);
        }
    }
    Ok(state)
}
