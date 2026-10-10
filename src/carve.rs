use crate::audit;
use crate::checkpoint::{self, ResumeMode, RunCheckpoint};
use crate::util::{json_escape, now_unix, unique_path, write_text_atomic};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const CHUNK_SIZE: usize = 1024 * 1024;
/// Overlap must cover the longest signature lookahead: MPEG-TS detection
/// needs 3 more 0x47 syncs at +188/+376/+564, so 4*188 = 752 bytes.
const OVERLAP_SIZE: usize = 4 * 188;
const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_MAX_CANDIDATES: usize = 64;
const MIN_CARVE_BYTES: u64 = 16;
/// Signature-scan progress is checkpointed every this many 1 MiB chunks,
/// so a resume repeats at most ~64 MiB of scanning on huge images.
const CHECKPOINT_CHUNK_INTERVAL: usize = 64;
/// Consecutive Annex-B SPS anchors closer than this are one stream —
/// every anchor's carved extent already runs to the next container
/// signature, so mid-run anchors can never produce a distinct artifact.
/// Coalescing at scan time keeps `hits` (and every checkpoint line)
/// from flooding with redundant anchors inside contiguous stream data.
const ANNEXB_COALESCE_BYTES: u64 = 4 * 1024 * 1024;
/// Upper bound on the span a single RIFF header may claim for
/// embedded-payload suppression — a corrupt/false header cannot
/// blind the fragment scan beyond this.
const MAX_RIFF_CLAIM_BYTES: u64 = 1024 * 1024 * 1024;

/// Scan-time suppression state for fragment-prone signatures (Annex-B
/// anchors, JPEG SOIs). Carve-time suppression cannot run during the
/// scan itself: tens of GB of contiguous AVIs would emit an SPS anchor
/// per GOP — tens of thousands of hits — exhausting the candidate cap
/// mid-scan and bloating every checkpoint line. Two rules keep `hits`
/// honest without losing coverage:
///  - anchors inside a span a RIFF header already declared are that
///    container's payload, never independent files;
///  - Annex-B anchors closer than ANNEXB_COALESCE_BYTES are redundant
///    mid-stream re-sends of SPS/VPS.
#[derive(Default)]
struct ScanSuppress {
    /// Absolute offset through which RIFF-declared container payload runs.
    embedded_until: u64,
    /// Next absolute offset at which an Annex-B anchor may emit.
    annexb_next_emit: u64,
}

#[derive(Debug, Clone)]
pub struct CarveOptions {
    pub max_bytes: u64,
    pub max_candidates: usize,
    /// Opt-in fragment reassembly pass. Off by default: joins are
    /// hypotheses, not evidence — enabling emits additional
    /// `reassembled-*` candidate artifacts alongside (never instead of)
    /// the original fragments.
    pub reassemble: bool,
    /// First byte offset the signature scan reads (default 0). Bounds the
    /// scan window — unlike `max_bytes`, which only caps each artifact's
    /// extent.
    pub scan_offset: Option<u64>,
    /// Maximum bytes the signature scan reads past `scan_offset`
    /// (default: to EOF). A bounded window is the honest way to probe a
    /// large image: the result records the window and `scan_complete`
    /// only claims completeness for the scanned range.
    pub scan_length: Option<u64>,
}

impl Default for CarveOptions {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_candidates: DEFAULT_MAX_CANDIDATES,
            reassemble: false,
            scan_offset: None,
            scan_length: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarveHit {
    pub offset: u64,
    pub signature: String,
    pub extension: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CarvedArtifact {
    pub id: String,
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub offset: u64,
    pub size_bytes: u64,
    pub signature: String,
    pub extension: String,
    pub sha256: String,
    pub validation_status: String,
    pub validation_note: String,
    pub duplicate_of: Option<String>,
}

impl CarvedArtifact {
    fn to_json(&self) -> String {
        format!(
            "{{\"schema_version\":3,\"event\":\"carve-file\",\"id\":\"{}\",\"artifact_type\":\"carved-candidate\",\"validation_status\":\"{}\",\"validation_note\":\"{}\",\"duplicate_of\":{},\"source_path\":\"{}\",\"output_path\":\"{}\",\"offset\":{},\"size_bytes\":{},\"signature\":\"{}\",\"extension\":\"{}\",\"sha256\":\"{}\"}}",
            json_escape(&self.id),
            json_escape(&self.validation_status),
            json_escape(&self.validation_note),
            audit::optional_string(self.duplicate_of.as_deref()),
            json_escape(&self.source_path.to_string_lossy()),
            json_escape(&self.output_path.to_string_lossy()),
            self.offset,
            self.size_bytes,
            json_escape(&self.signature),
            json_escape(&self.extension),
            json_escape(&self.sha256)
        )
    }
}

#[derive(Debug, Clone)]
pub struct CarveResult {
    pub source_path: PathBuf,
    pub carved_unix: u64,
    pub source_size_bytes: u64,
    pub artifacts: Vec<CarvedArtifact>,
    /// Artifacts replayed from an interrupted run's checkpoint (their
    /// output files already exist) instead of being re-carved.
    pub resumed_artifacts: usize,
    /// Byte offset the signature scan resumed from (0 = full scan ran).
    pub resumed_scan_offset: u64,
    pub warnings: Vec<String>,
    pub candidate_limit_reached: bool,
    /// The signature scan reached the end of its window (EOF, or the
    /// `--scan-length` bound) without stopping at the candidate cap.
    /// With a bounded window it says nothing about bytes outside it.
    pub scan_complete: bool,
    /// Absolute scan window actually covered, [start, end).
    pub scan_start_offset: u64,
    pub scan_end_offset: u64,
    pub options: CarveOptions,
}

impl CarveResult {
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        out.push_str("  \"schema_version\": 1,\n");
        out.push_str(&format!(
            "  \"source_path\": \"{}\",\n",
            json_escape(&self.source_path.to_string_lossy())
        ));
        out.push_str(&format!("  \"carved_unix\": {},\n", self.carved_unix));
        out.push_str(&format!(
            "  \"source_size_bytes\": {},\n",
            self.source_size_bytes
        ));
        out.push_str(&format!(
            "  \"artifact_count\": {},\n",
            self.artifacts.len()
        ));
        out.push_str(&format!(
            "  \"resumed_artifacts\": {},\n",
            self.resumed_artifacts
        ));
        out.push_str(&format!(
            "  \"candidate_limit_reached\": {},\n  \"scan_complete\": {},\n",
            self.candidate_limit_reached, self.scan_complete
        ));
        out.push_str(&format!(
            "  \"scan_start_offset\": {},\n  \"scan_end_offset\": {},\n",
            self.scan_start_offset, self.scan_end_offset
        ));
        out.push_str("  \"options\": {\n");
        out.push_str(&format!("    \"max_bytes\": {},\n", self.options.max_bytes));
        out.push_str(&format!(
            "    \"max_candidates\": {},\n",
            self.options.max_candidates
        ));
        out.push_str(&format!(
            "    \"reassemble\": {},\n",
            self.options.reassemble
        ));
        out.push_str(&format!(
            "    \"scan_offset\": {},\n    \"scan_length\": {}\n",
            self.options
                .scan_offset
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".to_string()),
            self.options
                .scan_length
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".to_string())
        ));
        out.push_str("  },\n");
        out.push_str("  \"warnings\": [\n");
        for (index, warning) in self.warnings.iter().enumerate() {
            out.push_str(&format!("    \"{}\"", json_escape(warning)));
            if index + 1 != self.warnings.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ],\n");
        out.push_str("  \"artifacts\": [\n");
        for (index, artifact) in self.artifacts.iter().enumerate() {
            out.push_str("    ");
            out.push_str(&artifact.to_json());
            if index + 1 != self.artifacts.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n");
        out.push_str("}\n");
        out
    }
}

pub fn carve_file(
    case_dir: &Path,
    source_path: &Path,
    options: &CarveOptions,
    resume: ResumeMode,
    progress: Option<&CarveProgress<'_>>,
) -> Result<CarveResult, String> {
    let source_path = source_path
        .canonicalize()
        .map_err(|err| format!("failed to canonicalize carve source: {err}"))?;
    if !source_path.is_file() {
        return Err(format!(
            "carve source is not a file: {}",
            source_path.display()
        ));
    }
    // A relative case dir logs invocation-cwd-relative output_paths that
    // no later consumer can resolve — anchor it before anything is
    // derived from it (same treatment source_path already gets).
    std::fs::create_dir_all(case_dir).map_err(|err| format!("failed to create case dir: {err}"))?;
    let case_dir = &crate::util::canonicalize_display(case_dir)
        .map_err(|err| format!("failed to canonicalize case dir: {err}"))?;
    if options.max_bytes < MIN_CARVE_BYTES {
        return Err(format!("--max-bytes must be at least {MIN_CARVE_BYTES}"));
    }
    if options.max_candidates == 0 {
        return Err("--max-candidates must be greater than 0".to_string());
    }

    let metadata = std::fs::metadata(&source_path)
        .map_err(|err| format!("failed to read source metadata: {err}"))?;
    let source_size = metadata.len();
    // Scan window: --scan-offset/--scan-length bound which bytes the
    // signature walk reads; anything outside was never inspected and the
    // result must not imply it was.
    let scan_start = options.scan_offset.unwrap_or(0).min(source_size);
    let scan_end = options
        .scan_length
        .map(|length| scan_start.saturating_add(length))
        .unwrap_or(u64::MAX)
        .min(source_size);
    if scan_end <= scan_start && source_size > 0 {
        return Err(format!(
            "scan window is empty: --scan-offset {} with --scan-length {:?} covers no bytes of a {}-byte source",
            scan_start, options.scan_length, source_size
        ));
    }
    // The fingerprint covers source identity AND the option set, so a
    // checkpoint from a different image or different --max-*/window
    // values never silently skips work.
    let fingerprint = carve_fingerprint(&source_path, &metadata, options);
    let mut checkpoint = RunCheckpoint::begin(
        &case_dir.join("db/carve-progress.jsonl"),
        "carve-file",
        &fingerprint,
        resume,
    )?;
    let prior = load_carve_checkpoint(&checkpoint)?;
    // A checkpointed offset inside the window resumes there; anything at
    // or before the window start begins at the window start.
    let resume_offset = prior.scan_offset.max(scan_start);
    let carved_map = prior.carved;

    // Early preflight: if the volume cannot hold even one --max-bytes
    // artifact, fail in seconds rather than after the signature scan.
    // Exact per-artifact sizes are checked again before each write below,
    // since the scan discovers hits incrementally.
    let carve_dir = case_dir.join("artifacts/carved");
    crate::diskspace::ensure_available(
        &carve_dir,
        options
            .max_bytes
            .min(scan_end.saturating_sub(resume_offset)),
        "carve-file",
    )?;

    let hits = scan_signatures(
        &source_path,
        ScanWindow {
            start: scan_start,
            resume: resume_offset,
            end: scan_end,
        },
        prior.hits,
        ScanSuppress {
            embedded_until: prior.embedded_until,
            annexb_next_emit: prior.annexb_next_emit,
        },
        options.max_candidates,
        &mut checkpoint,
        progress,
    )?;
    let mut warnings = Vec::new();
    if checkpoint.reset_stale() {
        warnings.push(
            "previous carve checkpoint was recorded for different inputs; started fresh"
                .to_string(),
        );
    }
    if scan_start > 0 || scan_end < source_size {
        warnings.push(format!(
            "scan window limited to [{scan_start}, {scan_end}) of {source_size} bytes by --scan-offset/--scan-length; bytes outside the window were not scanned"
        ));
    }
    if is_ewf_container(&source_path) {
        warnings.push(
            "source is an EWF (E01) container: hit offsets are container offsets, not disk offsets, and chunks that span EWF segment boundaries produce damaged candidates; for evidentiary carving run import-e01 first and carve the exported raw image"
                .to_string(),
        );
    }
    let candidate_limit_reached = hits.len() >= options.max_candidates;
    let scan_complete = !candidate_limit_reached;
    if candidate_limit_reached {
        warnings.push(format!(
            "candidate limit reached at {}; scan completeness is not established (conservative even at EOF); additional candidates may exist; rerun with a higher --max-candidates",
            options.max_candidates
        ));
    }

    let mut artifacts = Vec::new();
    let mut resumed_artifacts = 0usize;
    let mut first_by_hash = HashMap::<String, String>::new();
    // Fragment-prone signatures (elementary streams, embeddable JPEGs)
    // inside a span already claimed by an earlier candidate are the same
    // file's payload — suppress them instead of shredding one file into
    // fragments. Container signatures are never suppressed.
    let is_fragment_sig = |sig: &str| sig.ends_with("-annexb") || sig == "jpeg";
    let mut covered_until = 0u64;
    for hit in &hits {
        if is_fragment_sig(&hit.signature) && hit.offset < covered_until {
            continue;
        }
        // Extents run to the next container signature; mid-run fragment-sig
        // anchors inside the same span would otherwise truncate it.
        let next_offset = hits
            .iter()
            .filter(|candidate| !is_fragment_sig(&candidate.signature))
            .map(|candidate| candidate.offset)
            .filter(|offset| offset > &hit.offset)
            .min()
            .unwrap_or(source_size);
        let available = next_offset.saturating_sub(hit.offset);
        let (size_bytes, boundary_note) =
            refine_extent(&source_path, hit, available, options.max_bytes);
        // Single-run TS noise floor: a real fragment needs more than the
        // 4 detection packets; anything smaller is almost always garbage.
        let min_bytes = if hit.signature == "mpegts-sync" {
            (188 * 8) as u64
        } else {
            MIN_CARVE_BYTES
        };
        if size_bytes < min_bytes {
            warnings.push(format!("skipped tiny candidate at offset {}", hit.offset));
            continue;
        }

        let mut validation_note = format!(
            "{} {}",
            validation_note_for_signature(&hit.signature),
            boundary_note
        );
        if available > options.max_bytes {
            let note = format!(
                "truncated by max_bytes={} from a heuristic span of {} bytes at offset {}",
                options.max_bytes, available, hit.offset
            );
            warnings.push(note.clone());
            validation_note.push_str(&format!(" {note}."));
        }
        if let Some(artifact) = carved_map.get(&hit.offset)
            && artifact.output_path.is_file()
        {
            let current_hash = audit::digest_file(&artifact.output_path)?;
            if !current_hash.eq_ignore_ascii_case(&artifact.sha256) {
                return Err(format!(
                    "stale completed carve artifact {}: current hash differs from recorded hash; output preserved; rerun with --no-resume to create fresh artifacts",
                    artifact.output_path.display()
                ));
            }
            first_by_hash
                .entry(artifact.sha256.clone())
                .or_insert_with(|| artifact.id.clone());
            resumed_artifacts += 1;
            let mut replayed = artifact.clone();
            replayed.validation_note = validation_note;
            covered_until = covered_until.max(hit.offset + size_bytes);
            artifacts.push(replayed);
            continue;
        }

        let id = format!("carve_{:06}", artifacts.len() + 1);
        let output_path = unique_path(
            &case_dir
                .join("artifacts/carved")
                .join(format!("{}_{:012x}.{}", id, hit.offset, hit.extension)),
        );
        // Per-artifact check: the exact size is only known once the scan
        // finishes, so verify before each copy_range.
        crate::diskspace::ensure_available(&output_path, size_bytes, "carve-file")?;
        copy_range(&source_path, hit.offset, size_bytes, &output_path)
            .map_err(|err| format!("failed to carve {}: {err}", output_path.display()))?;
        let sha256 = audit::digest_file(&output_path)?;
        let duplicate_of = first_by_hash.get(&sha256).cloned();
        if duplicate_of.is_none() {
            first_by_hash.insert(sha256.clone(), id.clone());
        }
        let validation_status = if duplicate_of.is_some() {
            "duplicate-candidate"
        } else {
            "candidate-unvalidated"
        }
        .to_string();
        let artifact = CarvedArtifact {
            id,
            source_path: source_path.clone(),
            output_path,
            offset: hit.offset,
            size_bytes,
            signature: hit.signature.clone(),
            extension: hit.extension.clone(),
            sha256,
            validation_status,
            validation_note,
            duplicate_of,
        };
        // Checkpoint the artifact right after it lands on disk: a crash
        // before this line re-carves the range, a crash after it replays.
        checkpoint.append_line(&format!("{{\"carved\":{}}}", artifact.to_json()))?;
        covered_until = covered_until.max(hit.offset + size_bytes);
        artifacts.push(artifact);
    }

    if options.reassemble {
        reassemble_fragments(
            &source_path,
            case_dir,
            source_size,
            &mut artifacts,
            &mut warnings,
        )?;
    }

    let result = CarveResult {
        source_path,
        carved_unix: now_unix()?,
        source_size_bytes: source_size,
        artifacts,
        resumed_artifacts,
        resumed_scan_offset: resume_offset,
        warnings,
        candidate_limit_reached,
        scan_complete,
        scan_start_offset: scan_start,
        scan_end_offset: scan_end,
        options: options.clone(),
    };
    write_carve_outputs(case_dir, &result)?;
    if resumed_artifacts > 0 || resume_offset > 0 {
        let line = format!(
            "{{\"schema_version\":1,\"event\":\"carve-resume\",\"run_id\":\"{}\",\"resumed_artifacts\":{},\"resumed_scan_offset\":{}}}",
            json_escape(checkpoint.run_id()),
            resumed_artifacts,
            resume_offset
        );
        audit::append_chained_jsonl(&case_dir.join("artifacts/carved/carve-log.jsonl"), &line)?;
    }
    checkpoint.finish()?;
    Ok(result)
}

/// Fingerprint of everything that decides carve output: canonical source
/// path, size+mtime, and the option set.
fn carve_fingerprint(
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
struct CarveCheckpointState {
    /// Byte offset the signature scan had reached (0 = not started).
    scan_offset: u64,
    /// Cumulative hits at that offset (each `scan` line carries the full
    /// set found so far; the last line wins).
    hits: Vec<CarveHit>,
    /// Artifacts already carved, keyed by their hit's offset.
    carved: HashMap<u64, CarvedArtifact>,
    /// Scan suppression state at the checkpoint: without these a resume
    /// inside a RIFF-declared span forgets the container boundary and
    /// re-floods `hits` with embedded Annex-B/JPEG anchors.
    embedded_until: u64,
    annexb_next_emit: u64,
}

/// Decodes a prior run's carve checkpoint lines into `CarveCheckpointState`.
fn load_carve_checkpoint(checkpoint: &RunCheckpoint) -> Result<CarveCheckpointState, String> {
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

/// Serializes cumulative signature-scan progress for the checkpoint,
/// including suppression state so a resumed scan inside a container span
/// does not forget the boundary it crossed.
fn scan_progress_line(offset: u64, hits: &[CarveHit], suppress: &ScanSuppress) -> String {
    let hits_json = hits
        .iter()
        .map(|hit| {
            format!(
                "{{\"offset\":{},\"signature\":\"{}\",\"extension\":\"{}\"}}",
                hit.offset,
                json_escape(&hit.signature),
                json_escape(&hit.extension)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"scan\":{{\"offset\":{offset},\"embedded_until\":{},\"annexb_next_emit\":{},\"hits\":[{hits_json}]}}}}",
        suppress.embedded_until, suppress.annexb_next_emit
    )
}

/// Signature scan over the source file that can resume mid-file: it
/// re-reads the OVERLAP_SIZE window before `resume_offset` (so signatures
/// straddling the boundary are still found — duplicates dedup at the end)
/// and checkpoints cumulative progress every CHECKPOINT_CHUNK_INTERVAL
/// chunks plus once at the end.
/// Called with `(bytes_scanned, total_bytes)` during the signature walk;
/// `None` disables mid-run progress reporting (tests, headless callers).
/// Progress callback receiving (completed_bytes, total_bytes). Borrowed so
/// callers can pass closures over the case DB path without 'static.
pub type CarveProgress<'a> = dyn Fn(u64, u64) + Send + Sync + 'a;

/// Absolute scan window: [start, end) is the declared coverage, `resume`
/// is where this run picks up (>= start, from a same-window checkpoint).
struct ScanWindow {
    start: u64,
    resume: u64,
    end: u64,
}

fn scan_signatures(
    source_path: &Path,
    window: ScanWindow,
    prior_hits: Vec<CarveHit>,
    prior_suppress: ScanSuppress,
    max_candidates: usize,
    checkpoint: &mut RunCheckpoint,
    progress: Option<&CarveProgress<'_>>,
) -> Result<Vec<CarveHit>, String> {
    let scan_start = window.start;
    let resume_offset = window.resume;
    let scan_end = window.end;
    let mut hits = prior_hits;
    hits.sort_by_key(|hit| hit.offset);
    hits.dedup_by_key(|hit| hit.offset);
    if hits.len() >= max_candidates {
        // The interrupted run had already stopped scanning at the cap.
        hits.truncate(max_candidates);
        return Ok(hits);
    }
    let mut file =
        File::open(source_path).map_err(|err| format!("failed to open carve source: {err}"))?;
    let overlap_start = resume_offset.saturating_sub(OVERLAP_SIZE as u64);
    let mut overlap = vec![0u8; (resume_offset - overlap_start) as usize];
    file.seek(SeekFrom::Start(overlap_start))
        .and_then(|_| file.read_exact(&mut overlap))
        .and_then(|_| file.seek(SeekFrom::Start(resume_offset)))
        .map_err(|err| format!("failed to seek carve source to resume offset: {err}"))?;
    let mut offset = resume_offset;
    let mut since_checkpoint = 0usize;
    // Restored from the checkpoint: a resume offset inside a RIFF-declared
    // span stays suppressed instead of flooding hits with embedded anchors.
    let mut suppress = prior_suppress;

    while offset < scan_end {
        let want = CHUNK_SIZE.min((scan_end - offset) as usize);
        let mut chunk = vec![0u8; want];
        let read = file
            .read(&mut chunk)
            .map_err(|err| format!("failed to read carve source: {err}"))?;
        if read == 0 {
            break;
        }
        chunk.truncate(read);

        let mut scan = Vec::with_capacity(overlap.len() + chunk.len());
        scan.extend_from_slice(&overlap);
        scan.extend_from_slice(&chunk);
        let scan_start = offset.saturating_sub(overlap.len() as u64);
        scan_buffer_state(&scan, scan_start, offset, &mut hits, &mut suppress);
        hits.sort_by_key(|hit| hit.offset);
        hits.dedup_by_key(|hit| hit.offset);
        if hits.len() >= max_candidates {
            hits.truncate(max_candidates);
            break;
        }

        overlap.clear();
        let keep = OVERLAP_SIZE.min(scan.len());
        overlap.extend_from_slice(&scan[scan.len() - keep..]);
        offset = offset.saturating_add(read as u64);
        since_checkpoint += 1;
        if since_checkpoint >= CHECKPOINT_CHUNK_INTERVAL {
            checkpoint.append_line(&scan_progress_line(offset, &hits, &suppress))?;
            if let Some(report) = progress {
                report(offset, scan_end);
            }
            since_checkpoint = 0;
        }
    }
    checkpoint.append_line(&scan_progress_line(offset, &hits, &suppress))?;
    if let Some(report) = progress {
        report(offset, scan_end);
    }
    // The overlap window exists to catch signatures straddling a resume
    // boundary — but a hit anchored before the declared window start is
    // outside the scan scope the result claims to cover.
    hits.retain(|hit| hit.offset >= scan_start);
    hits.sort_by_key(|hit| hit.offset);
    hits.dedup_by_key(|hit| hit.offset);
    Ok(hits)
}

/// EWF segment files begin with the libewf file signature
/// ("EVF\x09\x0d\x0a\xff\x00"). Carving the container directly means hit
/// offsets are container offsets and candidates can be damaged at
/// segment boundaries — a warning the result must carry.
fn is_ewf_container(path: &Path) -> bool {
    const EWF_MAGIC: [u8; 8] = [b'E', b'V', b'F', 0x09, 0x0d, 0x0a, 0xff, 0x00];
    let mut head = [0u8; 8];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .map(|_| head == EWF_MAGIC)
        .unwrap_or(false)
}

fn validation_note_for_signature(signature: &str) -> &'static str {
    match signature {
        "mp4-ftyp" => {
            "MP4 ftyp signature found and carved contiguously; verify moov/mdat structure and playback before reporting as recovered."
        }
        "riff-avi" => {
            "RIFF AVI signature found and carved contiguously; verify AVI index/playback before reporting as recovered."
        }
        "dahua-dhav" => {
            "Dahua DHAV signature found; treat as proprietary candidate and validate with FFmpeg or vendor player."
        }
        "hikvision-imkh" => {
            "Hikvision IMKH signature found; strip the 40-byte header with export-hik and validate playback before reporting."
        }
        "h264-annexb" => {
            "H.264 Annex-B elementary-stream run (no container header) — deleted-recording body or recorder stream temp file; remux with ffmpeg -f h264 for playback; original filename/metadata unrecoverable."
        }
        "h265-annexb" => {
            "H.265 Annex-B elementary-stream run (no container header) — deleted-recording body or recorder stream temp file; remux with ffmpeg -f hevc for playback; original filename/metadata unrecoverable."
        }
        "jpeg" => {
            "JPEG signature found; carved contiguously to the next signature — verify EOI marker (FF D9) and decode before reporting."
        }
        "mpeg-ps" => {
            "MPEG program-stream pack header found; carved contiguously — verify pack alignment and playback before reporting."
        }
        "ebml-mkv" => {
            "EBML (Matroska/WebM) header found; carved contiguously — verify segment structure and playback before reporting."
        }
        "mpegts-sync" => {
            "MPEG-TS sync-aligned packet run (188-byte packets, 0x47 sync); a fragmented TS file may appear as multiple separate run candidates."
        }
        _ => {
            "Signature-based contiguous carve only; verify playback/container integrity before reporting as recovered video."
        }
    }
}

/// Structural extent refinement: MP4/RIFF/TS containers declare or imply
/// their own end, so for those signatures the carved span is tightened to
/// the real boundary instead of blindly running to the next signature or
/// EOF. Returns (extent_bytes, boundary_note) — the note always states how
/// the end was determined so downstream review can weigh it.
fn refine_extent(source: &Path, hit: &CarveHit, available: u64, max_bytes: u64) -> (u64, String) {
    let span = available.min(max_bytes);
    match hit.signature.as_str() {
        "mp4-ftyp" => mp4_extent(source, hit.offset, span),
        "riff-avi" => riff_extent(source, hit.offset, span),
        "mpegts-sync" => ts_extent(source, hit.offset, span),
        _ => (
            span,
            "End boundary uses the next retained signature or EOF, limited by max_bytes; signature boundaries are heuristics, not proof of a complete recovered file.".to_string(),
        ),
    }
}

/// Walk MP4 box structure from `start`: each box is size(u32be)+type(4cc),
/// size==1 means a 64-bit largesize, size==0 means "to EOF". Three
/// outcomes: a clean end on a box boundary (structural completeness), a
/// box whose declared size overruns the span (truncated tail — possible
/// fragmentation), or a position that fails to parse as a box (the
/// fragment gap — bytes beyond belong to other data and are excluded).
fn mp4_extent(source: &Path, start: u64, span: u64) -> (u64, String) {
    let fallback = || {
        (
            span,
            "MP4 box walk unavailable; fell back to heuristic boundary.".to_string(),
        )
    };
    let Ok(mut file) = File::open(source) else {
        return fallback();
    };
    let hard_end = start + span;
    let mut cursor = start;
    let mut boxes = 0u32;
    let mut saw_ftyp = false;
    let mut saw_moov = false;
    let mut saw_mdat = false;
    loop {
        if cursor == hard_end {
            return (
                span,
                format!(
                    "MP4 box structure complete ({} boxes{}{}{}) — structural end boundary.",
                    boxes,
                    if saw_ftyp { ", ftyp" } else { "" },
                    if saw_moov { ", moov" } else { "" },
                    if saw_mdat { ", mdat" } else { "" }
                ),
            );
        }
        if cursor + 8 > hard_end {
            return (
                span,
                format!(
                    "MP4 tail truncated at +{} — fewer than 8 bytes remain for the next box header (possible fragmentation or capped span).",
                    cursor - start
                ),
            );
        }
        let mut hdr = [0u8; 8];
        if file.seek(SeekFrom::Start(cursor)).is_err() || file.read_exact(&mut hdr).is_err() {
            return fallback();
        }
        let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
        let typ = &hdr[4..8];
        // Box types are printable ASCII; anything else means the walk has
        // left the container — i.e., the fragment gap.
        if !typ.iter().all(|b| (0x20..=0x7e).contains(b)) || (size32 < 8 && size32 > 1) {
            return (
                cursor - start,
                format!(
                    "MP4 box structure broke at +{} — probable fragmentation gap; bytes beyond are not part of this container.",
                    cursor - start
                ),
            );
        }
        let size = match size32 {
            0 => hard_end - cursor,
            1 => {
                let mut large = [0u8; 8];
                if file.read_exact(&mut large).is_err() {
                    return fallback();
                }
                u64::from_be_bytes(large)
            }
            s => u64::from(s),
        };
        if size < 8 {
            return (
                cursor - start,
                format!(
                    "MP4 box structure broke at +{} (invalid box size {}) — probable fragmentation gap.",
                    cursor - start,
                    size
                ),
            );
        }
        boxes += 1;
        match typ {
            b"ftyp" => saw_ftyp = true,
            b"moov" => saw_moov = true,
            b"mdat" => saw_mdat = true,
            _ => {}
        }
        let next = cursor.saturating_add(size);
        if next > hard_end {
            return (
                span,
                format!(
                    "MP4 '{}' box at +{} declares {} bytes but only {} remain — truncated tail (possible fragmentation).",
                    String::from_utf8_lossy(typ),
                    cursor - start,
                    size,
                    hard_end - cursor
                ),
            );
        }
        cursor = next;
        if boxes >= 65536 {
            return (
                cursor - start,
                format!(
                    "MP4 box walk capped at {} boxes — boundary uncertain.",
                    boxes
                ),
            );
        }
    }
}

/// RIFF declares its file size at +4 (u32le, size-8) — the most precise
/// boundary any of the carved formats offers.
fn riff_extent(source: &Path, start: u64, span: u64) -> (u64, String) {
    if let Ok(mut file) = File::open(source) {
        let mut hdr = [0u8; 8];
        if file.seek(SeekFrom::Start(start)).is_ok() && file.read_exact(&mut hdr).is_ok() {
            let declared = u64::from(u32::from_le_bytes(hdr[4..8].try_into().unwrap())) + 8;
            if declared <= span {
                return (
                    declared,
                    "RIFF declared-size boundary — structurally exact end.".to_string(),
                );
            }
            return (
                span,
                format!(
                    "RIFF declares {} bytes but only {} remain in span — truncated tail (possible fragmentation).",
                    declared, span
                ),
            );
        }
    }
    (
        span,
        "RIFF size field unreadable; heuristic boundary.".to_string(),
    )
}

/// MPEG-TS extent = the contiguous run of 188-byte packets each starting
/// with the 0x47 sync byte. The run ends at the first desync — for a
/// fragmented recording that is the fragment's end, not the file's, and
/// the note says so rather than implying a complete recovery.
fn ts_extent(source: &Path, start: u64, span: u64) -> (u64, String) {
    let fallback = || {
        (
            span,
            "TS extent walk unavailable; heuristic boundary.".to_string(),
        )
    };
    let Ok(mut file) = File::open(source) else {
        return fallback();
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        return fallback();
    }
    let hard_end = start + span;
    let mut cursor = start;
    let mut buf = vec![0u8; 256 * 1024];
    while cursor < hard_end {
        // Reads stay packet-aligned so buf[i] at i%188==0 is always a
        // packet boundary position.
        let want = (((hard_end - cursor) / 188) as usize).min(buf.len() / 188) * 188;
        if want == 0 {
            break;
        }
        let Ok(read) = file.read(&mut buf[..want]) else {
            return fallback();
        };
        if read == 0 {
            break;
        }
        let mut i = 0usize;
        while i < read {
            if buf[i] != 0x47 {
                let rel = cursor + i as u64 - start;
                return (
                    cursor + i as u64 - start,
                    format!(
                        "TS sync lost at +{} after {} contiguous packets — contiguous fragment end (possible fragmentation).",
                        rel,
                        rel / 188
                    ),
                );
            }
            i += 188;
        }
        cursor += read as u64;
    }
    (
        span,
        "MPEG-TS sync-aligned run continued to the span boundary.".to_string(),
    )
}

// --- Opt-in fragment reassembly -----------------------------------------
//
// Reassembly produces HYPOTHESES, never verdicts: every joined artifact
// stays `candidate-unvalidated` and its note names the join method plus
// the fragments/regions involved, so a later decode-validation step is
// what arbitrates correctness. Two methods, ordered by how defensible
// the join evidence is:
//
// 1. MPEG-TS continuity-counter joins (structural evidence). TS packets
//    carry a per-PID 4-bit continuity counter that increments once per
//    payload-bearing packet of that PID. When fragment B's first packets
//    continue fragment A's per-PID counters exactly, the join rests on
//    the same evidence the transport format itself uses — about as
//    defensible as carving gets.
// 2. MP4 bifragment joins (hypothesis only). A truncated MP4 whose moov
//    box survived yields an exact tail length via stsz/stz2; when moov
//    lies beyond the gap it can sometimes be located by scanning the
//    anonymous (signature-less) regions for a plausible `moov` box.
//    Both are emitted as candidates — the tool never claims the join is
//    correct, and never deletes the original fragments.
//
// Joined artifacts are derived data: they are NOT checkpointed (a resume
// replays the source fragments and deterministically re-derives them)
// and they never consume the --max-candidates budget.

const MAX_REASSEMBLED: usize = 32;
const MAX_JOINS_PER_HEAD: usize = 4;
const MIN_REGION_BYTES: u64 = 512;
const TS_TAIL_PACKETS: u64 = 32;

struct JoinSpec {
    ranges: Vec<(u64, u64)>,
    signature: &'static str,
    extension: &'static str,
    offset: u64,
    note: String,
}

fn reassemble_fragments(
    source: &Path,
    case_dir: &Path,
    source_size: u64,
    artifacts: &mut Vec<CarvedArtifact>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    let regions = anonymous_regions(artifacts, source_size);
    let mut joins: Vec<JoinSpec> = Vec::new();
    ts_cc_joins(source, artifacts, &mut joins, warnings);
    mp4_bifragment_joins(source, artifacts, &regions, &mut joins, warnings);
    if joins.len() > MAX_REASSEMBLED {
        warnings.push(format!(
            "reassembly emitted the first {} join hypotheses; {} more were discarded",
            MAX_REASSEMBLED,
            joins.len() - MAX_REASSEMBLED
        ));
        joins.truncate(MAX_REASSEMBLED);
    }

    let known_hashes: HashMap<String, String> = artifacts
        .iter()
        .map(|artifact| (artifact.sha256.clone(), artifact.id.clone()))
        .collect();
    let carve_dir = case_dir.join("artifacts/carved");
    for (index, join) in joins.into_iter().enumerate() {
        let id = format!("reasm_{:06}", index + 1);
        let output_path =
            unique_path(&carve_dir.join(format!("{}_{:012x}.{}", id, join.offset, join.extension)));
        let total: u64 = join.ranges.iter().map(|(_, len)| len).sum();
        crate::diskspace::ensure_available(&output_path, total, "carve-file --reassemble")?;
        copy_ranges(source, &join.ranges, &output_path).map_err(|err| {
            format!(
                "failed to write reassembled {}: {err}",
                output_path.display()
            )
        })?;
        let sha256 = audit::digest_file(&output_path)?;
        let duplicate_of = artifacts
            .iter()
            .find(|artifact| artifact.sha256 == sha256)
            .map(|artifact| artifact.id.clone())
            .or_else(|| known_hashes.get(&sha256).cloned());
        let size_bytes = total;
        artifacts.push(CarvedArtifact {
            id,
            source_path: source.to_path_buf(),
            output_path,
            offset: join.offset,
            size_bytes,
            signature: join.signature.to_string(),
            extension: join.extension.to_string(),
            sha256,
            validation_status: if duplicate_of.is_some() {
                "duplicate-candidate"
            } else {
                "candidate-unvalidated"
            }
            .to_string(),
            validation_note: join.note,
            duplicate_of,
        });
    }
    Ok(())
}

/// Disk regions not claimed by any artifact's carved extent: before the
/// first signature and between fragments. These are the only places a
/// signature-less continuation fragment can physically live.
fn anonymous_regions(artifacts: &[CarvedArtifact], source_size: u64) -> Vec<(u64, u64)> {
    let mut claimed: Vec<(u64, u64)> = artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.offset,
                artifact.offset.saturating_add(artifact.size_bytes),
            )
        })
        .collect();
    claimed.sort();
    let mut regions = Vec::new();
    let mut cursor = 0u64;
    for (start, end) in claimed {
        if start > cursor && start - cursor >= MIN_REGION_BYTES {
            regions.push((cursor, start - cursor));
        }
        cursor = cursor.max(end);
    }
    if source_size > cursor && source_size - cursor >= MIN_REGION_BYTES {
        regions.push((cursor, source_size - cursor));
    }
    regions
}

/// Per-packet TS header decode: (pid, continuity counter, has_payload).
/// afc==0 is reserved and yields None.
fn ts_packet_header(packet: &[u8]) -> Option<(u16, u8, bool)> {
    if packet.len() < 4 || packet[0] != 0x47 {
        return None;
    }
    let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
    let afc = (packet[3] >> 4) & 0x3;
    if afc == 0 {
        return None;
    }
    Some((pid, packet[3] & 0x0f, afc != 2))
}

/// Last continuity counter per PID over the fragment's tail packets.
fn ts_tail_cc(source: &Path, offset: u64, len: u64) -> HashMap<u16, u8> {
    let mut map = HashMap::new();
    let packets = (len / 188).min(TS_TAIL_PACKETS);
    let Ok(mut file) = File::open(source) else {
        return map;
    };
    let tail = offset + len - packets * 188;
    if file.seek(SeekFrom::Start(tail)).is_err() {
        return map;
    }
    let mut buf = vec![0u8; (packets * 188) as usize];
    if file.read_exact(&mut buf).is_err() {
        return map;
    }
    for packet in buf.chunks_exact(188) {
        if let Some((pid, cc, _)) = ts_packet_header(packet) {
            map.insert(pid, cc);
        }
    }
    map
}

/// First packet (cc, has_payload) per PID over the fragment's head.
fn ts_head_cc(source: &Path, offset: u64, len: u64) -> HashMap<u16, (u8, bool)> {
    let mut map = HashMap::new();
    let packets = (len / 188).min(TS_TAIL_PACKETS);
    let Ok(mut file) = File::open(source) else {
        return map;
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return map;
    }
    let mut buf = vec![0u8; (packets * 188) as usize];
    if file.read_exact(&mut buf).is_err() {
        return map;
    }
    for packet in buf.chunks_exact(188) {
        if let Some((pid, cc, has_payload)) = ts_packet_header(packet) {
            map.entry(pid).or_insert((cc, has_payload));
        }
    }
    map
}

/// Whether `head` continues `tail_map`'s per-PID counters. A payload
/// packet must be last_cc+1 (mod 16); an adaptation-only packet repeats
/// the counter. Requires ≥1 shared PID and zero contradictions — any
/// mismatch rejects the join outright.
fn ts_cc_continues(
    tail_map: &HashMap<u16, u8>,
    head: &HashMap<u16, (u8, bool)>,
) -> Option<Vec<u16>> {
    let mut shared = 0usize;
    let mut matched = Vec::new();
    for (pid, (cc, has_payload)) in head {
        let Some(last) = tail_map.get(pid) else {
            continue;
        };
        shared += 1;
        let expected = if *has_payload {
            (last + 1) & 0x0f
        } else {
            *last
        };
        if *cc != expected {
            return None;
        }
        matched.push(*pid);
    }
    (shared > 0).then_some(matched)
}

/// Greedy disk-order chaining of TS fragments whose boundary counters
/// interlock. Groups of ≥2 become one joined artifact each.
fn ts_cc_joins(
    source: &Path,
    artifacts: &[CarvedArtifact],
    joins: &mut Vec<JoinSpec>,
    warnings: &mut Vec<String>,
) {
    let mut ts: Vec<&CarvedArtifact> = artifacts
        .iter()
        .filter(|artifact| artifact.signature == "mpegts-sync")
        .collect();
    ts.sort_by_key(|artifact| artifact.offset);
    let mut group: Vec<&CarvedArtifact> = Vec::new();
    let mut tail_map: HashMap<u16, u8> = HashMap::new();
    let mut group_pids: Vec<u16> = Vec::new();
    for artifact in ts {
        let head = ts_head_cc(source, artifact.offset, artifact.size_bytes);
        let matched = ts_cc_continues(&tail_map, &head);
        if !group.is_empty() && matched.is_none() {
            flush_ts_group(&group, &group_pids, joins, warnings);
            group.clear();
            group_pids.clear();
        }
        if let Some(pids) = matched {
            group_pids.extend(pids);
        }
        group.push(artifact);
        tail_map = ts_tail_cc(source, artifact.offset, artifact.size_bytes);
    }
    flush_ts_group(&group, &group_pids, joins, warnings);
}

fn flush_ts_group(
    group: &[&CarvedArtifact],
    pids: &[u16],
    joins: &mut Vec<JoinSpec>,
    warnings: &mut Vec<String>,
) {
    if group.len() < 2 {
        return;
    }
    let members: Vec<String> = group
        .iter()
        .map(|artifact| format!("{}@{}", artifact.id, artifact.offset))
        .collect();
    let mut unique_pids: Vec<u16> = pids.to_vec();
    unique_pids.sort_unstable();
    unique_pids.dedup();
    joins.push(JoinSpec {
        ranges: group
            .iter()
            .map(|artifact| (artifact.offset, artifact.size_bytes))
            .collect(),
        signature: "reassembled-ts-cc",
        extension: "ts",
        offset: group[0].offset,
        note: format!(
            "REASSEMBLY HYPOTHESIS: {} MPEG-TS fragments ({}) joined — per-PID continuity counters interlock at every boundary (pids {:?}). Structural join evidence; still a candidate — validate playback before reporting.",
            group.len(),
            members.join(", "),
            unique_pids
        ),
    });
    warnings.push(format!(
        "reassembled {} TS fragments into one join candidate (continuity-counter match)",
        group.len()
    ));
}

// --- MP4 bifragment joins -------------------------------------------------

#[derive(Debug)]
struct Mp4Box {
    typ: [u8; 4],
    offset: u64,
    size: u64,
    /// Offset of the box payload (after the 8/16-byte header).
    payload: u64,
    /// Declared size ran past `span` — box is truncated.
    truncated: bool,
}

/// Top-level box walk over [start, start+span); stops at the first
/// position that does not parse as a box. Truncated final boxes are
/// included with `truncated: true`.
fn mp4_top_boxes(source: &Path, start: u64, span: u64) -> Vec<Mp4Box> {
    let mut boxes = Vec::new();
    let Ok(mut file) = File::open(source) else {
        return boxes;
    };
    let hard_end = start + span;
    let mut cursor = start;
    while cursor + 8 <= hard_end && boxes.len() < 65536 {
        let mut hdr = [0u8; 8];
        if file.seek(SeekFrom::Start(cursor)).is_err() || file.read_exact(&mut hdr).is_err() {
            break;
        }
        let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
        let typ: [u8; 4] = hdr[4..8].try_into().unwrap();
        if !typ.iter().all(|b| (0x20..=0x7e).contains(b)) || (size32 < 8 && size32 > 1) {
            break;
        }
        let (size, payload) = match size32 {
            0 => (hard_end - cursor, cursor + 8),
            1 => {
                let mut large = [0u8; 8];
                if file.read_exact(&mut large).is_err() {
                    break;
                }
                (u64::from_be_bytes(large), cursor + 16)
            }
            s => (u64::from(s), cursor + 8),
        };
        if size < 8 {
            break;
        }
        let truncated = cursor + size > hard_end;
        boxes.push(Mp4Box {
            typ,
            offset: cursor - start,
            size,
            payload: payload - start,
            truncated,
        });
        if truncated || size32 == 0 {
            break;
        }
        cursor += size;
    }
    boxes
}

/// Sum of every stsz/stz2 sample table under `moov` — the exact payload
/// byte count mdat must hold. Nested boxes are walked through the
/// standard container chain (moov→trak→mdia→minf→stbl); unknown
/// containers are skipped rather than aborting.
fn mp4_sample_payload_bytes(source: &Path, frag_start: u64, moov: &Mp4Box) -> Option<u64> {
    let moov_end = moov.offset + moov.size;
    let mut total = 0u64;
    // Stack of (cursor, end) container payload ranges; seed with moov's body.
    let mut stack = vec![(frag_start + moov.payload, frag_start + moov_end)];
    let Ok(mut file) = File::open(source) else {
        return None;
    };
    let mut found = false;
    while let Some((mut cursor, end)) = stack.pop() {
        while cursor + 8 <= end {
            let mut hdr = [0u8; 8];
            if file.seek(SeekFrom::Start(cursor)).is_err() || file.read_exact(&mut hdr).is_err() {
                break;
            }
            let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
            let typ = &hdr[4..8];
            if !typ.iter().all(|b| (0x20..=0x7e).contains(b)) || (size32 < 8 && size32 > 1) {
                break;
            }
            let (size, header) = match size32 {
                0 => (end - cursor, 8u64),
                1 => {
                    let mut large = [0u8; 8];
                    if file.read_exact(&mut large).is_err() {
                        break;
                    }
                    (u64::from_be_bytes(large), 16u64)
                }
                s => (u64::from(s), 8u64),
            };
            if size < header || cursor + size > end {
                break;
            }
            match typ {
                b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"dinf" | b"mvex" | b"moof"
                | b"traf" => {
                    stack.push((cursor + header, cursor + size));
                }
                b"stsz" => {
                    let payload = cursor + header;
                    let mut meta = [0u8; 12];
                    if file.seek(SeekFrom::Start(payload)).is_err()
                        || file.read_exact(&mut meta).is_err()
                    {
                        return None;
                    }
                    let sample_size = u32::from_be_bytes(meta[4..8].try_into().unwrap());
                    let count = u32::from_be_bytes(meta[8..12].try_into().unwrap());
                    if count > 8_000_000 {
                        return None;
                    }
                    if sample_size != 0 {
                        total += u64::from(sample_size) * u64::from(count);
                    } else {
                        let mut entries = vec![0u8; count as usize * 4];
                        if file.read_exact(&mut entries).is_err() {
                            return None;
                        }
                        for chunk in entries.chunks_exact(4) {
                            total += u64::from(u32::from_be_bytes(chunk.try_into().unwrap()));
                        }
                    }
                    found = true;
                }
                b"stz2" => {
                    let payload = cursor + header;
                    let mut meta = [0u8; 12];
                    if file.seek(SeekFrom::Start(payload)).is_err()
                        || file.read_exact(&mut meta).is_err()
                    {
                        return None;
                    }
                    let field = meta[7];
                    let count = u32::from_be_bytes(meta[8..12].try_into().unwrap());
                    if count > 16_000_000 {
                        return None;
                    }
                    let packed = (u64::from(field) * u64::from(count)).div_ceil(8) as usize;
                    let mut entries = vec![0u8; packed];
                    if file.read_exact(&mut entries).is_err() {
                        return None;
                    }
                    match field {
                        4 => {
                            for byte in &entries {
                                total += u64::from(byte >> 4) + u64::from(byte & 0x0f);
                            }
                        }
                        8 => {
                            for byte in &entries {
                                total += u64::from(*byte);
                            }
                        }
                        16 => {
                            for chunk in entries.chunks_exact(2) {
                                total += u64::from(u16::from_be_bytes(chunk.try_into().unwrap()));
                            }
                        }
                        _ => return None,
                    }
                    found = true;
                }
                _ => {}
            }
            cursor += size;
        }
    }
    found.then_some(total)
}

/// Scan `region` for a plausible moov box: 'moov' 4cc at a position
/// where the preceding u32 size is sane and the box interior starts
/// with a printable-ASCII child type. Returns (rel_offset, box_size).
fn find_moov_in_region(
    source: &Path,
    region_start: u64,
    search_from: u64,
    region_len: u64,
) -> Option<(u64, u64)> {
    let Ok(mut file) = File::open(source) else {
        return None;
    };
    // moov for typical files is small and sits near the gap; a 16 MiB
    // window bounds I/O while covering realistic bifragment layouts.
    let window = region_len.saturating_sub(search_from).min(16 * 1024 * 1024);
    let abs = region_start + search_from;
    if file.seek(SeekFrom::Start(abs)).is_err() {
        return None;
    }
    let mut buf = vec![0u8; window as usize];
    let read = file.read(&mut buf).ok()? as u64;
    // i is the 'moov' type offset; its u32 size sits at i-4, so 4 is the
    // minimum scan index — a box may legitimately start the window.
    let mut i = 4usize;
    while i + 8 <= read as usize {
        if &buf[i..i + 4] == b"moov" && i + 12 <= read as usize {
            let size = u32::from_be_bytes(buf[i - 4..i].try_into().unwrap()) as u64;
            if size >= 16 && i as u64 - 4 + size <= read {
                // moov's payload opens with a child box: size(4)+type(4) —
                // the TYPE is at +8, not +4 (that would be the child size).
                let child_typ = &buf[i + 8..i + 12];
                if child_typ.iter().all(|b| (0x20..=0x7e).contains(b)) {
                    // Caller wants the offset from the REGION start, and
                    // this window began `search_from` bytes into it.
                    return Some((search_from + i as u64 - 4, size));
                }
            }
        }
        i += 1;
    }
    None
}

/// MP4 hypotheses. moov-present fragments get an stsz-bounded truth: a
/// shorter true extent emits a refined (de-tailed) artifact, a longer
/// one emits gap-fill joins from anonymous regions. moov-absent
/// fragments truncated inside mdat get joins that hunt the missing moov
/// inside a region — attribution always unverified.
fn mp4_bifragment_joins(
    source: &Path,
    artifacts: &[CarvedArtifact],
    regions: &[(u64, u64)],
    joins: &mut Vec<JoinSpec>,
    warnings: &mut Vec<String>,
) {
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.signature == "mp4-ftyp")
    {
        let boxes = mp4_top_boxes(source, artifact.offset, artifact.size_bytes);
        let moov = boxes.iter().find(|b| &b.typ == b"moov" && !b.truncated);
        let mdat = boxes.iter().find(|b| &b.typ == b"mdat");
        let Some(mdat) = mdat else { continue };
        let mdat_end = mdat.offset + mdat.size;
        // stsz re-bounding is only sound when mdat is the LAST box: if a
        // moov (or anything else) follows it inside the fragment, those
        // bytes are real content and cutting at the payload end would
        // amputate them.
        let mdat_is_last = boxes.last().is_some_and(|b| b.typ == *b"mdat");
        match moov {
            Some(moov) if mdat_is_last => {
                match mp4_sample_payload_bytes(source, artifact.offset, moov) {
                Some(total) => {
                    let true_end = mdat.payload + total;
                    if true_end < artifact.size_bytes && artifact.size_bytes - true_end >= 8 {
                        joins.push(JoinSpec {
                            ranges: vec![(artifact.offset, true_end)],
                            signature: "mp4-stsz-refined",
                            extension: "mp4",
                            offset: artifact.offset,
                            note: format!(
                                "REASSEMBLY HYPOTHESIS: {} re-bounded by moov stsz — mdat payload ends at +{} but the carve ran to +{}; the trailing {} bytes are foreign data, not part of this file.",
                                artifact.id,
                                true_end,
                                artifact.size_bytes,
                                artifact.size_bytes - true_end
                            ),
                        });
                    } else if true_end > artifact.size_bytes {
                        let need = true_end - artifact.size_bytes;
                        emit_mp4_gapfills(
                            artifact, regions, need, None, joins, warnings,
                            "moov stsz bounds the missing tail exactly",
                        );
                    }
                }
                None => warnings.push(format!(
                    "{}: moov present but stsz/stz2 unreadable — mdat bound unknown; reassembly skipped",
                    artifact.id
                )),
                }
            }
            Some(_) => {} // moov follows mdat — tail boxes are real content
            None => {
                // moov absent from the fragment. Either mdat is truncated
                // (continuation holds payload then moov) or mdat completed
                // and the gap cut the moov itself — the only honest anchor
                // is hunting a plausible moov in an anonymous region.
                let need = mdat_end.saturating_sub(artifact.size_bytes);
                emit_mp4_gapfills(
                    artifact,
                    regions,
                    need,
                    Some(source),
                    joins,
                    warnings,
                    "moov lies beyond the fragment; tail+moov attribution unverified",
                );
            }
        }
    }
}

/// Emit up to MAX_JOINS_PER_HEAD join hypotheses for a truncated head:
/// head bytes + `need` continuation bytes from each anonymous region
/// large enough. When `moov_hunt` is set, the join also scans past the
/// payload bytes for a plausible moov box and extends the range to
/// include it — otherwise the joined file could never validate anyway.
fn emit_mp4_gapfills(
    artifact: &CarvedArtifact,
    regions: &[(u64, u64)],
    need: u64,
    moov_hunt: Option<&Path>,
    joins: &mut Vec<JoinSpec>,
    warnings: &mut Vec<String>,
    basis: &str,
) {
    let mut emitted = 0usize;
    for (region_off, region_len) in regions.iter().copied() {
        if emitted >= MAX_JOINS_PER_HEAD {
            break;
        }
        if region_len < need {
            continue;
        }
        // The tail's placement inside the region matters: when a moov was
        // found, its position anchors the layout — the `need` payload
        // bytes are whatever immediately PRECEDES it, so foreign gap bytes
        // earlier in the region are excluded. Without a moov anchor the
        // only guess is a blind prefix of the region.
        let mut tail_off = region_off;
        let mut tail_len = need;
        let mut moov_note = String::new();
        if let Some(source) = moov_hunt {
            match find_moov_in_region(source, region_off, need, region_len) {
                Some((rel, size)) => {
                    let payload_start = rel.saturating_sub(need);
                    tail_off = region_off + payload_start;
                    tail_len = rel + size - payload_start;
                    moov_note = format!(
                        "; plausible moov box located at region+{rel} ({size} bytes) — tail anchored to the {tail_len} bytes ending at the moov, not a blind region prefix"
                    );
                }
                None => continue, // no moov → join cannot validate; skip
            }
        }
        joins.push(JoinSpec {
            ranges: vec![
                (artifact.offset, artifact.size_bytes),
                (tail_off, tail_len),
            ],
            signature: "reassembled-mp4-bifragment",
            extension: "mp4",
            offset: artifact.offset,
            note: format!(
                "REASSEMBLY HYPOTHESIS: {} (truncated at +{}) + {} bytes from anonymous region at disk offset {} ({}{}). Join NOT verified — validate with ffprobe/decode before any evidentiary claim.",
                artifact.id,
                artifact.size_bytes,
                tail_len,
                region_off,
                basis,
                moov_note
            ),
        });
        emitted += 1;
    }
    if emitted > 0 {
        warnings.push(format!(
            "{}: emitted {} bifragment join hypothes{} (unverified candidates)",
            artifact.id,
            emitted,
            if emitted == 1 { "is" } else { "es" }
        ));
    }
}

/// Chunked signature scan over any byte source, identical to the on-disk
/// carve walk (1 MiB chunks, 752-byte overlap). `#[doc(hidden)]`: exposed so
/// the fuzz harness can drive the scanner without touching the filesystem;
/// not part of the supported API.
#[doc(hidden)]
pub fn find_video_signatures_in(
    reader: &mut impl Read,
    max_candidates: usize,
) -> Result<Vec<CarveHit>, String> {
    let mut hits = Vec::new();
    let mut offset = 0u64;
    let mut overlap = Vec::<u8>::new();
    let mut suppress = ScanSuppress::default();

    loop {
        let mut chunk = vec![0u8; CHUNK_SIZE];
        let read = reader
            .read(&mut chunk)
            .map_err(|err| format!("failed to read carve source: {err}"))?;
        if read == 0 {
            break;
        }
        chunk.truncate(read);

        let mut scan = Vec::with_capacity(overlap.len() + chunk.len());
        scan.extend_from_slice(&overlap);
        scan.extend_from_slice(&chunk);
        let scan_start = offset.saturating_sub(overlap.len() as u64);
        scan_buffer_state(&scan, scan_start, offset, &mut hits, &mut suppress);
        hits.sort_by_key(|hit| hit.offset);
        hits.dedup_by_key(|hit| hit.offset);
        if hits.len() >= max_candidates {
            hits.truncate(max_candidates);
            break;
        }

        overlap.clear();
        let keep = OVERLAP_SIZE.min(scan.len());
        overlap.extend_from_slice(&scan[scan.len() - keep..]);
        offset = offset.saturating_add(read as u64);
    }

    hits.sort_by_key(|hit| hit.offset);
    hits.dedup_by_key(|hit| hit.offset);
    Ok(hits)
}

#[cfg(test)]
fn scan_buffer(scan: &[u8], scan_start: u64, current_chunk_start: u64, hits: &mut Vec<CarveHit>) {
    scan_buffer_state(
        scan,
        scan_start,
        current_chunk_start,
        hits,
        &mut ScanSuppress::default(),
    );
}

fn scan_buffer_state(
    scan: &[u8],
    scan_start: u64,
    current_chunk_start: u64,
    hits: &mut Vec<CarveHit>,
    suppress: &mut ScanSuppress,
) {
    for index in 0..scan.len() {
        let absolute = scan_start + index as u64;
        if absolute < current_chunk_start.saturating_sub(OVERLAP_SIZE as u64) {
            continue;
        }

        // Every signature is suppressed inside a RIFF-declared container
        // span: those bytes are the outer container's payload, and no real
        // file can begin there. The claim is capped (MAX_RIFF_CLAIM_BYTES)
        // so a corrupt header cannot blind the scan beyond a bounded span.
        let embedded = absolute < suppress.embedded_until;
        if index >= 4 && scan.get(index..index + 4) == Some(b"ftyp") && !embedded {
            hits.push(CarveHit {
                offset: absolute - 4,
                signature: "mp4-ftyp".to_string(),
                extension: "mp4".to_string(),
            });
        }
        if scan.get(index..index + 4) == Some(b"RIFF")
            && scan.get(index + 8..index + 11) == Some(b"AVI")
            && !embedded
        {
            hits.push(CarveHit {
                offset: absolute,
                signature: "riff-avi".to_string(),
                extension: "avi".to_string(),
            });
            // The RIFF header declares its own file extent: the size
            // field counts bytes after it, so the payload ends at
            // absolute + 8 + declared. Signatures inside that span are
            // this container's payload, not candidates — suppress them
            // at scan time or embedded SPS anchors/JPEG thumbnails flood
            // `hits` and exhaust the candidate cap. The strict "AVI "
            // check keeps suppression off weaker near-matches.
            if scan.get(index + 8..index + 12) == Some(b"AVI ") {
                let declared = scan
                    .get(index + 4..index + 8)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64)
                    .unwrap_or(0);
                if declared > 8 {
                    suppress.embedded_until = suppress
                        .embedded_until
                        .max(absolute + 8 + declared.min(MAX_RIFF_CLAIM_BYTES));
                }
            }
        }
        if scan.get(index..index + 4) == Some(b"DHAV") && !embedded {
            hits.push(CarveHit {
                offset: absolute,
                signature: "dahua-dhav".to_string(),
                extension: "dav".to_string(),
            });
        }
        if scan.get(index..index + 4) == Some(b"IMKH") && !embedded {
            hits.push(CarveHit {
                offset: absolute,
                signature: "hikvision-imkh".to_string(),
                extension: "mpg".to_string(),
            });
        }
        // JPEG stills: dashcam snapshots / embedded thumbnails —
        // thumbnails inside a declared container span are payload.
        if scan.get(index..index + 3) == Some(&[0xff, 0xd8, 0xff][..])
            && absolute >= suppress.embedded_until
        {
            hits.push(CarveHit {
                offset: absolute,
                signature: "jpeg".to_string(),
                extension: "jpg".to_string(),
            });
        }
        // MPEG program-stream pack start (DVR exports): 00 00 01 BA
        // followed by the MPEG-1/MPEG-2 pack-header marker 0x21 or 0x44.
        // Pack starts inside a declared container span are payload, not
        // candidates — AVI payloads can carry PS/TS fragments.
        if scan.get(index..index + 4) == Some(&[0, 0, 1, 0xba][..])
            && absolute >= suppress.embedded_until
        {
            let marker = scan.get(index + 4).copied().unwrap_or(0);
            if marker == 0x21 || marker == 0x44 {
                hits.push(CarveHit {
                    offset: absolute,
                    signature: "mpeg-ps".to_string(),
                    extension: "mpg".to_string(),
                });
            }
        }
        // EBML header — Matroska/WebM containers. A full MKV cannot sit
        // inside an AVI payload, so headers in a declared span are noise.
        if scan.get(index..index + 4) == Some(&[0x1a, 0x45, 0xdf, 0xa3][..])
            && absolute >= suppress.embedded_until
        {
            hits.push(CarveHit {
                offset: absolute,
                signature: "ebml-mkv".to_string(),
                extension: "mkv".to_string(),
            });
        }
        // MPEG-TS: 188-byte packets with a 0x47 sync byte. Require 3 more
        // syncs ahead (kills random 0x47 noise) and none 188 bytes behind
        // (a sync behind means mid-run — the run start was already emitted).
        // index < 188 can't see the previous sync, so those positions are
        // only trusted at file start (no overlap); runs whose start lands
        // inside the overlap are emitted there by the same rule because the
        // previous chunk already had their full lookahead.
        let overlap_len = current_chunk_start.saturating_sub(scan_start);
        let run_start = if index >= 188 {
            scan[index - 188] != 0x47
        } else {
            overlap_len == 0
        };
        if scan[index] == 0x47
            && scan.get(index + 188) == Some(&0x47)
            && scan.get(index + 376) == Some(&0x47)
            && scan.get(index + 564) == Some(&0x47)
            && run_start
            && absolute >= suppress.embedded_until
        {
            hits.push(CarveHit {
                offset: absolute,
                signature: "mpegts-sync".to_string(),
                extension: "ts".to_string(),
            });
        }
        // Annex-B elementary streams: dashcams write raw H.264/H.265 to
        // in-progress temp files, and overwritten/deleted recording bodies
        // survive only as headerless stream runs — no container signature
        // exists to find them. Anchor on SPS (0x67) or H.265 VPS/SPS
        // (0x40/0x42) and require another start code within 512 bytes.
        // Anchors inside a RIFF-declared payload are suppressed here at
        // scan time; anchors inside spans of other containers and
        // mid-stream re-anchors are suppressed at carve time, and
        // consecutive anchors within ANNEXB_COALESCE_BYTES are one
        // stream — a mid-run anchor can never yield a distinct artifact
        // because every anchor's extent runs to the next container hit.
        if scan.get(index..index + 4) == Some(&[0, 0, 0, 1][..])
            && absolute >= suppress.embedded_until
            && absolute >= suppress.annexb_next_emit
        {
            let nal = scan.get(index + 4).copied().unwrap_or(0);
            let (signature, extension) = match nal {
                0x67 => ("h264-annexb", "h264"),
                0x40 | 0x42 => ("h265-annexb", "h265"),
                _ => ("", ""),
            };
            if !signature.is_empty() {
                let lookahead_end = (index + 517).min(scan.len());
                let has_second = scan
                    .get(index + 5..lookahead_end)
                    .map(|tail| tail.windows(4).any(|w| w == [0, 0, 0, 1]))
                    .unwrap_or(false);
                if has_second {
                    hits.push(CarveHit {
                        offset: absolute,
                        signature: signature.to_string(),
                        extension: extension.to_string(),
                    });
                    suppress.annexb_next_emit = absolute + ANNEXB_COALESCE_BYTES;
                }
            }
        }
    }
}

fn copy_range(source: &Path, offset: u64, size_bytes: u64, output: &Path) -> io::Result<()> {
    copy_ranges(source, &[(offset, size_bytes)], output)
}

/// Concatenate several source ranges into one output file — the physical
/// form of a reassembly hypothesis (head + continuation fragments).
fn copy_ranges(source: &Path, ranges: &[(u64, u64)], output: &Path) -> io::Result<()> {
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

fn write_carve_outputs(case_dir: &Path, result: &CarveResult) -> Result<(), String> {
    write_text_atomic(&case_dir.join("db/carve_results.json"), &result.to_json())
        .map_err(|err| format!("failed to write carve results: {err}"))?;

    let log_path = case_dir.join("artifacts/carved/carve-log.jsonl");
    for artifact in &result.artifacts {
        audit::append_chained_jsonl(&log_path, &artifact.to_json())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CarveHit, scan_buffer, validation_note_for_signature};

    #[test]
    fn finds_mp4_ftyp_start_offset() {
        let mut hits = Vec::new();
        scan_buffer(b"\0\0\0\x18ftypmp42payload", 100, 100, &mut hits);
        assert_eq!(
            hits[0],
            CarveHit {
                offset: 100,
                signature: "mp4-ftyp".to_string(),
                extension: "mp4".to_string()
            }
        );
    }

    #[test]
    fn finds_avi_and_dhav_signatures() {
        let mut hits = Vec::new();
        // Declared RIFF size covers only the "AVI " fourCC — the DHAV
        // signature sits beyond the container's claimed span.
        scan_buffer(b"RIFF\x04\x00\x00\x00AVI data DHAVmore", 0, 0, &mut hits);
        assert!(hits.iter().any(|hit| hit.signature == "riff-avi"));
        assert!(hits.iter().any(|hit| hit.signature == "dahua-dhav"));
    }

    #[test]
    fn finds_imkh_signature() {
        let mut hits = Vec::new();
        scan_buffer(b"padIMKHpayloaddatahere", 0, 0, &mut hits);
        assert!(hits.iter().any(|hit| hit.signature == "hikvision-imkh"));
        assert!(validation_note_for_signature("hikvision-imkh").contains("export-hik"));
    }

    #[test]
    fn annexb_detects_sps_anchored_stream_start() {
        let mut buf = vec![0u8; 4096];
        // H.264 SPS at 100, second start code (PPS) at 140 — real run.
        buf[100..104].copy_from_slice(&[0, 0, 0, 1]);
        buf[104] = 0x67;
        buf[140..144].copy_from_slice(&[0, 0, 0, 1]);
        buf[144] = 0x68;
        let mut hits = Vec::new();
        scan_buffer(&buf, 0, 0, &mut hits);
        let h: Vec<_> = hits
            .iter()
            .filter(|h| h.signature == "h264-annexb")
            .collect();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].offset, 100);
        assert_eq!(h[0].extension, "h264");

        // H.265 VPS also anchors — separate buffer: an anchor within
        // ANNEXB_COALESCE_BYTES of an emitted hit is a mid-run anchor
        // and correctly suppressed, so it can't share this buffer.
        let mut buf_h265 = vec![0u8; 4096];
        buf_h265[2000..2004].copy_from_slice(&[0, 0, 0, 1]);
        buf_h265[2004] = 0x40;
        buf_h265[2100..2104].copy_from_slice(&[0, 0, 0, 1]);
        buf_h265[2104] = 0x42;
        let mut hits2 = Vec::new();
        scan_buffer(&buf_h265, 0, 0, &mut hits2);
        assert!(hits2.iter().any(|h| h.signature == "h265-annexb"));

        // Lone SPS with no second start code in lookahead: rejected.
        let mut lone = vec![0u8; 2048];
        lone[100..104].copy_from_slice(&[0, 0, 0, 1]);
        lone[104] = 0x67;
        let mut hits3 = Vec::new();
        scan_buffer(&lone, 0, 0, &mut hits3);
        assert!(hits3.iter().all(|h| !h.signature.ends_with("-annexb")));
    }

    #[test]
    fn annexb_inside_riff_declared_span_is_suppressed_at_scan_time() {
        let mut buf = vec![0u8; 8192];
        buf[0..4].copy_from_slice(b"RIFF");
        buf[4..8].copy_from_slice(&(4096u32).to_le_bytes());
        buf[8..12].copy_from_slice(b"AVI ");
        // SPS-anchored run inside the declared span: payload, suppressed.
        buf[100..104].copy_from_slice(&[0, 0, 0, 1]);
        buf[104] = 0x67;
        buf[140..144].copy_from_slice(&[0, 0, 0, 1]);
        buf[144] = 0x68;
        // Second run past the declared end (0 + 8 + 4096 = 4104): real.
        buf[5000..5004].copy_from_slice(&[0, 0, 0, 1]);
        buf[5004] = 0x67;
        buf[5100..5104].copy_from_slice(&[0, 0, 0, 1]);
        buf[5104] = 0x65;
        let mut hits = Vec::new();
        scan_buffer(&buf, 0, 0, &mut hits);
        assert!(hits.iter().any(|h| h.signature == "riff-avi"));
        let annexb: Vec<_> = hits
            .iter()
            .filter(|h| h.signature == "h264-annexb")
            .collect();
        assert_eq!(annexb.len(), 1);
        assert_eq!(annexb[0].offset, 5000);
    }

    #[test]
    fn annexb_anchors_coalesce_within_one_stream_run() {
        // Two SPS anchors 100 KB apart are one stream — the first
        // anchor's carved extent already reaches the next container
        // signature, so the second can never yield a distinct artifact.
        let mut buf = vec![0u8; 200_000];
        buf[100..104].copy_from_slice(&[0, 0, 0, 1]);
        buf[104] = 0x67;
        buf[140..144].copy_from_slice(&[0, 0, 0, 1]);
        buf[144] = 0x68;
        buf[100_000..100_004].copy_from_slice(&[0, 0, 0, 1]);
        buf[100_004] = 0x67;
        buf[100_100..100_104].copy_from_slice(&[0, 0, 0, 1]);
        buf[100_104] = 0x65;
        let mut hits = Vec::new();
        scan_buffer(&buf, 0, 0, &mut hits);
        let annexb: Vec<_> = hits
            .iter()
            .filter(|h| h.signature == "h264-annexb")
            .collect();
        assert_eq!(annexb.len(), 1);
        assert_eq!(annexb[0].offset, 100);
    }

    #[test]
    fn mpegts_detects_run_start_not_mid_run() {
        // Eight sync-aligned packets form one run — only the first packet
        // boundary may produce a hit.
        let mut buf = vec![0u8; 188 * 8];
        for i in 0..8 {
            buf[i * 188] = 0x47;
        }
        let mut hits = Vec::new();
        scan_buffer(&buf, 0, 0, &mut hits);
        let ts: Vec<_> = hits
            .iter()
            .filter(|h| h.signature == "mpegts-sync")
            .collect();
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].offset, 0);

        // A run starting at offset 500 inside other data.
        let mut buf2 = vec![0u8; 2000];
        for i in 0..6 {
            buf2[500 + i * 188] = 0x47;
        }
        let mut hits2 = Vec::new();
        scan_buffer(&buf2, 0, 0, &mut hits2);
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].offset, 500);
        assert_eq!(hits2[0].extension, "ts");
    }

    #[test]
    fn extents_follow_container_structure() {
        let root = std::env::temp_dir().join(format!("ft-extent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        // Complete MP4: ftyp(24) + free(8) + mdat(16) = 48 bytes total.
        let mp4_ok = root.join("ok.mp4");
        let mut data = Vec::new();
        data.extend_from_slice(b"\0\0\0\x18ftypisom\0\0\0\0isommp42");
        data.extend_from_slice(b"\0\0\0\x08free");
        data.extend_from_slice(b"\0\0\0\x10mdat12345678");
        fs::write(&mp4_ok, &data).unwrap();
        let (extent, note) = super::mp4_extent(&mp4_ok, 0, data.len() as u64);
        assert_eq!(extent, data.len() as u64);
        assert!(note.contains("box structure complete"), "{note}");

        // Fragmented MP4: ftyp(24) then foreign (non-box) bytes — gap at +24.
        let mp4_gap = root.join("gap.mp4");
        let mut gdata = Vec::new();
        gdata.extend_from_slice(b"\0\0\0\x18ftypisom\0\0\0\0isommp42");
        gdata.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF].repeat(64));
        fs::write(&mp4_gap, &gdata).unwrap();
        let (extent, note) = super::mp4_extent(&mp4_gap, 0, gdata.len() as u64);
        assert_eq!(extent, 24);
        assert!(note.contains("fragmentation gap"), "{note}");

        // Truncated MP4: mdat declares more than the span holds.
        let mp4_trunc = root.join("trunc.mp4");
        let mut tdata = Vec::new();
        tdata.extend_from_slice(b"\0\0\0\x18ftypisom\0\0\0\0isommp42");
        tdata.extend_from_slice(b"\0\0\x03\xE8mdat");
        tdata.extend_from_slice(&[0u8; 64]);
        fs::write(&mp4_trunc, &tdata).unwrap();
        let (extent, note) = super::mp4_extent(&mp4_trunc, 0, tdata.len() as u64);
        assert_eq!(extent, tdata.len() as u64);
        assert!(note.contains("truncated"), "{note}");

        // RIFF with declared size smaller than the span.
        let riff = root.join("a.avi");
        let mut rdata = Vec::new();
        rdata.extend_from_slice(b"RIFF");
        rdata.extend_from_slice(&40u32.to_le_bytes());
        rdata.extend_from_slice(b"AVI ");
        rdata.extend_from_slice(&[0u8; 64]);
        fs::write(&riff, &rdata).unwrap();
        let (extent, note) = super::riff_extent(&riff, 0, rdata.len() as u64);
        assert_eq!(extent, 48);
        assert!(note.contains("declared-size"), "{note}");

        // MPEG-TS: 10 aligned packets then desync — fragment end at packet 10.
        let ts = root.join("f.ts");
        let mut tsdata = vec![0u8; 188 * 10 + 300];
        for i in 0..10 {
            tsdata[i * 188] = 0x47;
        }
        fs::write(&ts, &tsdata).unwrap();
        let (extent, note) = super::ts_extent(&ts, 0, tsdata.len() as u64);
        assert_eq!(extent, 188 * 10);
        assert!(note.contains("sync lost"), "{note}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cap_and_byte_limits_are_explicit_and_conservative() {
        for cap in [1, 2, 3] {
            let (case_dir, source, _) = resume_fixture(&format!("cap-{cap}"));
            let options = super::CarveOptions {
                max_candidates: cap,
                max_bytes: 32,
                ..Default::default()
            };
            let result = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap();
            assert_eq!(result.candidate_limit_reached, cap <= 2);
            assert_eq!(result.scan_complete, cap > 2);
            assert_eq!(result.artifacts.len(), cap.min(2));
            assert_eq!(
                result
                    .warnings
                    .iter()
                    .any(|w| w.contains("candidate limit reached")),
                cap <= 2
            );
            for artifact in &result.artifacts {
                assert_eq!(artifact.size_bytes, 32);
                assert!(
                    artifact
                        .validation_note
                        .contains("truncated by max_bytes=32")
                );
                assert!(artifact.validation_note.contains("possible fragmentation"));
                assert!(matches!(
                    artifact.validation_status.as_str(),
                    "candidate-unvalidated" | "duplicate-candidate"
                ));
            }
            let json: serde_json::Value = serde_json::from_str(&result.to_json()).unwrap();
            assert_eq!(json["candidate_limit_reached"], cap <= 2);
            assert_eq!(json["scan_complete"], cap > 2);
            let _ = fs::remove_dir_all(case_dir.parent().unwrap());
        }
    }

    #[test]
    fn overlap_duplicates_do_not_consume_candidate_budget() {
        let (case_dir, source, _) = resume_fixture("overlap");
        let mut content = vec![0; super::CHUNK_SIZE + 256];
        let first = super::CHUNK_SIZE - 24;
        let second = super::CHUNK_SIZE + 128;
        let ftyp = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";
        content[first..first + ftyp.len()].copy_from_slice(ftyp);
        content[second..second + ftyp.len()].copy_from_slice(ftyp);
        fs::write(&source, &content).unwrap();
        let options = super::CarveOptions {
            max_candidates: 2,
            ..Default::default()
        };
        let result = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(
            result
                .artifacts
                .iter()
                .map(|a| a.offset)
                .collect::<Vec<_>>(),
            vec![first as u64, second as u64]
        );
        let streamed =
            super::find_video_signatures_in(&mut std::io::Cursor::new(&content), 2).unwrap();
        assert_eq!(streamed.len(), 2);
        assert_eq!(streamed[1].offset, second as u64);
        let fingerprint = carve_fingerprint(
            &source.canonicalize().unwrap(),
            &fs::metadata(&source).unwrap(),
            &options,
        );
        let mut checkpoint = RunCheckpoint::begin(
            &case_dir.join("db/carve-progress.jsonl"),
            "carve-file",
            &fingerprint,
            ResumeMode::Auto,
        )
        .unwrap();
        checkpoint
            .append_line(&scan_progress_line(
                super::CHUNK_SIZE as u64,
                &[streamed[0].clone(), streamed[0].clone()],
                &super::ScanSuppress::default(),
            ))
            .unwrap();
        drop(checkpoint);
        let resumed = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(resumed.artifacts.len(), 2);
        assert_eq!(resumed.artifacts[1].offset, second as u64);
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    #[test]
    fn stale_completed_artifact_is_rejected_without_overwrite() {
        let (case_dir, source, content) = resume_fixture("changed-output");
        let options = super::CarveOptions::default();
        let first = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap();
        let fingerprint = carve_fingerprint(
            &source.canonicalize().unwrap(),
            &fs::metadata(&source).unwrap(),
            &options,
        );
        let mut checkpoint = RunCheckpoint::begin(
            &case_dir.join("db/carve-progress.jsonl"),
            "carve-file",
            &fingerprint,
            ResumeMode::Auto,
        )
        .unwrap();
        let hits = first
            .artifacts
            .iter()
            .map(|a| CarveHit {
                offset: a.offset,
                signature: a.signature.clone(),
                extension: a.extension.clone(),
            })
            .collect::<Vec<_>>();
        checkpoint
            .append_line(&scan_progress_line(
                content.len() as u64,
                &hits,
                &super::ScanSuppress::default(),
            ))
            .unwrap();
        checkpoint
            .append_line(&format!("{{\"carved\":{}}}", first.artifacts[0].to_json()))
            .unwrap();
        drop(checkpoint);
        let output = &first.artifacts[0].output_path;
        let mut revised = fs::read(output).unwrap();
        revised[12] = 1;
        fs::write(output, &revised).unwrap();
        let results_before = fs::read(case_dir.join("db/carve_results.json")).unwrap();
        let err = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap_err();
        assert!(
            err.contains("current hash differs from recorded hash"),
            "{err}"
        );
        assert_eq!(fs::read(output).unwrap(), revised);
        assert_eq!(
            fs::read(case_dir.join("db/carve_results.json")).unwrap(),
            results_before
        );
        assert!(case_dir.join("db/carve-progress.jsonl").exists());
        let fresh = carve_file(&case_dir, &source, &options, ResumeMode::Disabled, None).unwrap();
        assert_ne!(&fresh.artifacts[0].output_path, output);
        assert_eq!(fs::read(output).unwrap(), revised);
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    #[test]
    fn explains_validation_boundary_by_signature() {
        assert!(validation_note_for_signature("mp4-ftyp").contains("moov/mdat"));
        assert!(validation_note_for_signature("dahua-dhav").contains("proprietary"));
    }

    use super::{ResumeMode, RunCheckpoint, carve_file, carve_fingerprint, scan_progress_line};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn resume_fixture(name: &str) -> (PathBuf, PathBuf, Vec<u8>) {
        let root = std::env::temp_dir().join(format!(
            "frametrace-carve-resume-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        fs::create_dir_all(case_dir.join("artifacts/carved")).unwrap();
        fs::create_dir_all(case_dir.join("db")).unwrap();
        let source = root.join("image.raw");
        // Two MP4 ftyp signatures at offsets 0 and 128 inside 256 bytes.
        // Each ftyp box is followed by an mdat box declaring 1000 bytes —
        // well-formed box structure so the structural extent walk reaches
        // the span cap rather than a fragmentation gap on the padding.
        let mut content = vec![0u8; 256];
        content[..8].copy_from_slice(b"\0\0\0\x18ftyp");
        content[24..32].copy_from_slice(b"\0\0\x03\xE8mdat");
        content[128..136].copy_from_slice(b"\0\0\0\x18ftyp");
        content[152..160].copy_from_slice(b"\0\0\x03\xE8mdat");
        fs::write(&source, &content).unwrap();
        (case_dir, source, content)
    }

    /// An interrupted carve resumes the signature scan AND replays already
    /// carved artifacts instead of re-copying their ranges.
    #[test]
    fn resume_replays_scan_and_completed_artifacts() {
        let (case_dir, source, content) = resume_fixture("replay");
        let options = super::CarveOptions::default();
        let first = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(first.artifacts.len(), 2);
        let first_output = first.artifacts[0].output_path.clone();

        // Simulate a crash after artifact 1 landed: drop the results index,
        // the audit log, and the second artifact's output file.
        let _ = fs::remove_file(case_dir.join("db/carve_results.json"));
        let _ = fs::remove_file(case_dir.join("artifacts/carved/carve-log.jsonl"));
        fs::remove_file(&first.artifacts[1].output_path).unwrap();

        // Rebuild the checkpoint the crashed run would have left: full scan
        // progress + the first artifact's carved record.
        let canonical = source.canonicalize().unwrap();
        let metadata = fs::metadata(&source).unwrap();
        let fingerprint = carve_fingerprint(&canonical, &metadata, &options);
        {
            let mut checkpoint = RunCheckpoint::begin(
                &case_dir.join("db/carve-progress.jsonl"),
                "carve-file",
                &fingerprint,
                ResumeMode::Auto,
            )
            .unwrap();
            let hits = vec![
                super::CarveHit {
                    offset: 0,
                    signature: "mp4-ftyp".to_string(),
                    extension: "mp4".to_string(),
                },
                super::CarveHit {
                    offset: 128,
                    signature: "mp4-ftyp".to_string(),
                    extension: "mp4".to_string(),
                },
            ];
            checkpoint
                .append_line(&scan_progress_line(
                    content.len() as u64,
                    &hits,
                    &super::ScanSuppress::default(),
                ))
                .unwrap();
            checkpoint
                .append_line(&format!("{{\"carved\":{}}}", first.artifacts[0].to_json()))
                .unwrap();
        }

        let second = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(second.resumed_artifacts, 1);
        assert_eq!(second.resumed_scan_offset, content.len() as u64);
        assert_eq!(second.artifacts.len(), 2);
        // The resumed artifact keeps its original output path; the
        // re-carved one lands wherever the run puts it.
        assert_eq!(second.artifacts[0].output_path, first_output);
        assert_eq!(second.artifacts[0].sha256, first.artifacts[0].sha256);
        assert!(
            !case_dir.join("db/carve-progress.jsonl").exists(),
            "checkpoint deleted on full success"
        );
        // The resume is audit-logged into the carve log.
        let log = fs::read_to_string(case_dir.join("artifacts/carved/carve-log.jsonl")).unwrap();
        assert!(log.contains("\"event\":\"carve-resume\""), "{log}");
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// A checkpoint recorded for a different source/options fingerprint is
    /// discarded — the run re-scans and re-carves rather than trusting it.
    #[test]
    fn resume_rejects_stale_fingerprint() {
        let (case_dir, source, content) = resume_fixture("stale");
        let options = super::CarveOptions::default();
        {
            let mut checkpoint = RunCheckpoint::begin(
                &case_dir.join("db/carve-progress.jsonl"),
                "carve-file",
                "bogus",
                ResumeMode::Auto,
            )
            .unwrap();
            checkpoint
                .append_line(&scan_progress_line(
                    content.len() as u64,
                    &[],
                    &super::ScanSuppress::default(),
                ))
                .unwrap();
        }
        let result = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(result.resumed_artifacts, 0);
        assert_eq!(result.resumed_scan_offset, 0);
        assert_eq!(result.artifacts.len(), 2);
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    // --- reassembly tests -------------------------------------------------

    fn ts_packet(pid: u16, cc: u8, payload: bool) -> [u8; 188] {
        let mut packet = [0u8; 188];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) & 0x1f) as u8;
        packet[2] = (pid & 0xff) as u8;
        packet[3] = (if payload { 0x10 } else { 0x20 }) | (cc & 0x0f);
        packet
    }

    fn bx(typ: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(&payload);
        v
    }

    fn moov_with_stsz(sample_size: u32, count: u32) -> Vec<u8> {
        let mut stsz_payload = vec![0u8; 4];
        stsz_payload.extend_from_slice(&sample_size.to_be_bytes());
        stsz_payload.extend_from_slice(&count.to_be_bytes());
        bx(
            b"moov",
            bx(
                b"trak",
                bx(b"mdia", bx(b"minf", bx(b"stbl", bx(b"stsz", stsz_payload)))),
            ),
        )
    }

    fn fake_artifact(
        id: &str,
        offset: u64,
        size: u64,
        signature: &str,
        ext: &str,
        source: &Path,
    ) -> super::CarvedArtifact {
        super::CarvedArtifact {
            id: id.into(),
            source_path: source.to_path_buf(),
            output_path: source.to_path_buf(),
            offset,
            size_bytes: size,
            signature: signature.into(),
            extension: ext.into(),
            sha256: String::new(),
            validation_status: "candidate-unvalidated".into(),
            validation_note: String::new(),
            duplicate_of: None,
        }
    }

    fn reasm_case(name: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("frametrace-reasm-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        fs::create_dir_all(case_dir.join("artifacts/carved")).unwrap();
        fs::create_dir_all(case_dir.join("db")).unwrap();
        (case_dir, root.join("image.raw"))
    }

    /// Two TS fragments whose per-PID continuity counters interlock at the
    /// boundary produce one joined candidate — the only reassembly method
    /// backed by the format's own structural evidence.
    #[test]
    fn reassemble_ts_cc_joins_interlocked_fragments() {
        let (case_dir, source) = reasm_case("ts-join");
        let mut data = Vec::new();
        for i in 0..12u8 {
            data.extend_from_slice(&ts_packet(0x100, i, true));
        }
        let a_len = data.len() as u64;
        data.extend_from_slice(&[0xEEu8; 512]);
        let b_off = data.len() as u64;
        for i in 12..22u8 {
            data.extend_from_slice(&ts_packet(0x100, i, true));
        }
        let b_len = data.len() as u64 - b_off;
        data.extend_from_slice(&[0u8; 1024]);
        fs::write(&source, &data).unwrap();

        let mut artifacts = vec![
            fake_artifact("carve_000001", 0, a_len, "mpegts-sync", "ts", &source),
            fake_artifact("carve_000002", b_off, b_len, "mpegts-sync", "ts", &source),
        ];
        let mut warnings = Vec::new();
        super::reassemble_fragments(
            &source,
            &case_dir,
            data.len() as u64,
            &mut artifacts,
            &mut warnings,
        )
        .unwrap();

        assert_eq!(artifacts.len(), 3);
        let join = &artifacts[2];
        assert_eq!(join.signature, "reassembled-ts-cc");
        assert_eq!(join.size_bytes, a_len + b_len);
        assert!(
            join.validation_note.contains("continuity"),
            "{}",
            join.validation_note
        );
        assert!(
            join.validation_note.contains("HYPOTHESIS"),
            "{}",
            join.validation_note
        );
        assert_eq!(
            fs::metadata(&join.output_path).unwrap().len(),
            a_len + b_len
        );
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// A counter mismatch at the boundary rejects the join outright —
    /// adjacent-but-unrelated runs must not be fused.
    #[test]
    fn reassemble_ts_cc_rejects_counter_mismatch() {
        let (case_dir, source) = reasm_case("ts-mismatch");
        let mut data = Vec::new();
        for i in 0..12u8 {
            data.extend_from_slice(&ts_packet(0x100, i, true));
        }
        let a_len = data.len() as u64;
        data.extend_from_slice(&[0xEEu8; 512]);
        let b_off = data.len() as u64;
        for i in 14..24u8 {
            data.extend_from_slice(&ts_packet(0x100, i, true));
        }
        let b_len = data.len() as u64 - b_off;
        fs::write(&source, &data).unwrap();

        let mut artifacts = vec![
            fake_artifact("carve_000001", 0, a_len, "mpegts-sync", "ts", &source),
            fake_artifact("carve_000002", b_off, b_len, "mpegts-sync", "ts", &source),
        ];
        let mut warnings = Vec::new();
        super::reassemble_fragments(
            &source,
            &case_dir,
            data.len() as u64,
            &mut artifacts,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(artifacts.len(), 2);
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// moov's stsz says the mdat payload is smaller than the declared box
    /// — the carve absorbed foreign bytes, so a refined (de-tailed)
    /// candidate is emitted with the excess explained.
    #[test]
    fn reassemble_mp4_stsz_refines_absorbed_tail() {
        let (case_dir, source) = reasm_case("mp4-refine");
        let mut data = Vec::new();
        data.extend_from_slice(&bx(b"ftyp", b"isom\0\0\0\0isommp42".to_vec()));
        let moov = moov_with_stsz(300, 1); // one 300-byte sample
        data.extend_from_slice(&moov);
        let mdat_payload = data.len() as u64 + 8;
        data.extend_from_slice(&bx(b"mdat", vec![0xAA; 492])); // declared 500, real 300
        let frag_len = data.len() as u64;
        data.extend_from_slice(&[0u8; 1024]);
        fs::write(&source, &data).unwrap();

        let mut artifacts = vec![fake_artifact(
            "carve_000001",
            0,
            frag_len,
            "mp4-ftyp",
            "mp4",
            &source,
        )];
        let mut warnings = Vec::new();
        super::reassemble_fragments(
            &source,
            &case_dir,
            data.len() as u64,
            &mut artifacts,
            &mut warnings,
        )
        .unwrap();

        assert_eq!(artifacts.len(), 2);
        let refined = &artifacts[1];
        assert_eq!(refined.signature, "mp4-stsz-refined");
        assert_eq!(refined.size_bytes, mdat_payload + 300);
        assert!(
            refined.validation_note.contains("foreign data"),
            "{}",
            refined.validation_note
        );
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// Head truncated inside mdat + moov-derived exact tail length → a
    /// bifragment join hypothesis is emitted from the anonymous region.
    #[test]
    fn reassemble_mp4_gapfill_emits_bifragment_hypothesis() {
        let (case_dir, source) = reasm_case("mp4-gapfill");
        let mut data = Vec::new();
        data.extend_from_slice(&bx(b"ftyp", b"isom\0\0\0\0isommp42".to_vec()));
        let moov = moov_with_stsz(1000, 1); // needs 1000 payload bytes
        data.extend_from_slice(&moov);
        data.extend_from_slice(&(2000u32).to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[0xBBu8; 400]); // only 400 of 1000 present
        let head_len = data.len() as u64;
        data.extend_from_slice(&[0xCCu8; 1500]); // anonymous region
        fs::write(&source, &data).unwrap();

        let mut artifacts = vec![fake_artifact(
            "carve_000001",
            0,
            head_len,
            "mp4-ftyp",
            "mp4",
            &source,
        )];
        let mut warnings = Vec::new();
        super::reassemble_fragments(
            &source,
            &case_dir,
            data.len() as u64,
            &mut artifacts,
            &mut warnings,
        )
        .unwrap();

        assert_eq!(artifacts.len(), 2);
        let join = &artifacts[1];
        assert_eq!(join.signature, "reassembled-mp4-bifragment");
        assert_eq!(join.size_bytes, head_len + 600);
        assert!(
            join.validation_note.contains("NOT verified"),
            "{}",
            join.validation_note
        );
        assert!(
            warnings.iter().any(|w| w.contains("bifragment")),
            "{warnings:?}"
        );
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// moov beyond the gap: the head holds ftyp+truncated-mdat only, so the
    /// join hunts a plausible moov inside an anonymous region and extends
    /// the tail to include it.
    #[test]
    fn reassemble_mp4_hunts_moov_beyond_gap() {
        let (case_dir, source) = reasm_case("mp4-hunt");
        let mut data = Vec::new();
        data.extend_from_slice(&bx(b"ftyp", b"isom\0\0\0\0isommp42".to_vec()));
        data.extend_from_slice(&(3000u32).to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[0xAAu8; 1000]); // head ends here, mid-mdat
        let head_len = data.len() as u64;
        // Bifragment continuation: the remaining 1992 declared-mdat
        // payload bytes, then the moov the gap cut off.
        data.extend_from_slice(&[0xBBu8; 1992]);
        let moov = moov_with_stsz(2992, 1);
        let moov_rel = data.len() as u64 - head_len;
        data.extend_from_slice(&moov);
        data.extend_from_slice(&[0u8; 1024]);
        fs::write(&source, &data).unwrap();

        // The carved head covers only what the gap left: ftyp + mdat
        // header + 1000 payload bytes (mdat declares 3000 → truncated).
        let mut artifacts = vec![fake_artifact(
            "carve_000001",
            0,
            head_len,
            "mp4-ftyp",
            "mp4",
            &source,
        )];
        let mut warnings = Vec::new();
        super::reassemble_fragments(
            &source,
            &case_dir,
            data.len() as u64,
            &mut artifacts,
            &mut warnings,
        )
        .unwrap();

        let join = artifacts
            .iter()
            .find(|a| a.signature == "reassembled-mp4-bifragment")
            .expect("moov-hunt join emitted");
        // tail = region[0 .. moov_rel + moov_size]
        assert_eq!(join.size_bytes, head_len + moov_rel + moov.len() as u64);
        assert!(
            join.validation_note.contains("attribution unverified"),
            "{}",
            join.validation_note
        );
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// moov AFTER mdat (normal layout): the fragment holds ftyp+mdat+moov
    /// and ends on a box boundary — stsz re-bounding must NOT fire, or the
    /// "refined" artifact would amputate the moov itself.
    #[test]
    fn reassemble_mp4_moov_after_mdat_is_not_refined() {
        let (case_dir, source) = reasm_case("mp4-moovlast");
        let mut data = Vec::new();
        data.extend_from_slice(&bx(b"ftyp", b"isom\0\0\0\0isommp42".to_vec()));
        data.extend_from_slice(&bx(b"mdat", vec![0xAA; 992])); // declared 1000
        let moov = moov_with_stsz(992, 1);
        data.extend_from_slice(&moov);
        let frag_len = data.len() as u64;
        data.extend_from_slice(&[0u8; 1024]);
        fs::write(&source, &data).unwrap();

        let mut artifacts = vec![fake_artifact(
            "carve_000001",
            0,
            frag_len,
            "mp4-ftyp",
            "mp4",
            &source,
        )];
        let mut warnings = Vec::new();
        super::reassemble_fragments(
            &source,
            &case_dir,
            data.len() as u64,
            &mut artifacts,
            &mut warnings,
        )
        .unwrap();

        assert!(
            !artifacts.iter().any(|a| a.signature == "mp4-stsz-refined"),
            "moov-after-mdat fragment must not be 'refined' into amputating the moov"
        );
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    /// Reassembly is strictly opt-in: a normal carve emits only fragments;
    /// --reassemble adds join candidates on top of them.
    #[test]
    fn reassemble_is_opt_in_via_options() {
        let (case_dir, source) = reasm_case("opt-in");
        let mut data = Vec::new();
        for i in 0..12u8 {
            data.extend_from_slice(&ts_packet(0x100, i, true));
        }
        data.extend_from_slice(&[0xEEu8; 512]);
        for i in 12..22u8 {
            data.extend_from_slice(&ts_packet(0x100, i, true));
        }
        data.extend_from_slice(&[0u8; 2048]);
        fs::write(&source, &data).unwrap();

        let plain = carve_file(
            &case_dir,
            &source,
            &super::CarveOptions::default(),
            ResumeMode::Auto,
            None,
        )
        .unwrap();
        assert_eq!(plain.artifacts.len(), 2);
        assert!(
            !plain
                .artifacts
                .iter()
                .any(|a| a.signature.starts_with("reassembled"))
        );

        let options = super::CarveOptions {
            reassemble: true,
            ..Default::default()
        };
        let joined = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(joined.artifacts.len(), 3);
        assert_eq!(joined.artifacts[2].signature, "reassembled-ts-cc");
        let _ = fs::remove_dir_all(case_dir.parent().unwrap());
    }

    #[test]
    fn scan_window_limits_hits_and_reports_coverage() {
        let root =
            std::env::temp_dir().join(format!("frametrace-carve-window-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let case_dir = root.join("case");
        fs::create_dir_all(case_dir.join("artifacts/carved")).unwrap();
        fs::create_dir_all(case_dir.join("db")).unwrap();
        let source = root.join("image.raw");
        // ftyp signatures at 64 (inside the window), 640 and 2048
        // (outside it) — only the in-window anchor may be reported.
        let mut content = vec![0u8; 4096];
        let ftyp = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";
        for offset in [64usize, 640, 2048] {
            content[offset..offset + ftyp.len()].copy_from_slice(ftyp);
        }
        fs::write(&source, &content).unwrap();

        let options = super::CarveOptions {
            scan_offset: Some(512),
            scan_length: Some(1024),
            ..Default::default()
        };
        let result = carve_file(&case_dir, &source, &options, ResumeMode::Auto, None).unwrap();
        assert_eq!(result.scan_start_offset, 512);
        assert_eq!(result.scan_end_offset, 1536);
        assert!(result.scan_complete);
        assert_eq!(result.artifacts.len(), 1);
        assert_eq!(result.artifacts[0].offset, 640);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("scan window limited to [512, 1536)")),
            "window scope must be stated: {:?}",
            result.warnings
        );

        // A window that covers no source bytes is a caller error, not an
        // empty result that could be mistaken for "scanned, found nothing".
        let empty = super::CarveOptions {
            scan_offset: Some(99999),
            ..Default::default()
        };
        assert!(carve_file(&case_dir, &source, &empty, ResumeMode::Auto, None).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
