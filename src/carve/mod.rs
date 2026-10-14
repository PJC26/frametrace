use crate::audit;
use crate::checkpoint::{self, ResumeMode, RunCheckpoint};
use crate::util::{json_escape, now_unix, unique_path, write_text_atomic};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

mod extent;
mod mp4;
mod output;
mod reassemble;
mod resume;
mod signatures;
#[cfg(test)]
mod tests;

use extent::*;
use mp4::*;
use output::*;
use reassemble::*;
use resume::*;
use signatures::*;
pub use signatures::{CarveProgress, find_video_signatures_in};

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
    /// Every RIFF-declared span `[riff_offset, end)` inside which
    /// fragment-prone signatures were not emitted (E3), in offset order.
    spans: Vec<(u64, u64)>,
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
    /// Offset of the RIFF AVI whose declared span contains this container
    /// signature (E3): still a candidate, but possibly that AVI's payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_in: Option<u64>,
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
    /// Carve run that produced the artifact (E4: `<unix>-<hex4>`, also the
    /// middle of its id). `None` for entries written before E4.
    #[serde(default)]
    pub run_id: Option<String>,
    /// When the artifact was written. `None` for entries written before
    /// E4, whose time is only known from the run's `db/carve_results.json`.
    #[serde(default)]
    pub carved_unix: Option<u64>,
}

impl CarvedArtifact {
    fn to_json(&self) -> String {
        // E4 fields go last and only when known, so pre-E4 replays keep
        // their exact line shape.
        let mut run_fields = String::new();
        if let Some(run_id) = &self.run_id {
            run_fields.push_str(&format!(",\"run_id\":\"{}\"", json_escape(run_id)));
        }
        if let Some(carved_unix) = self.carved_unix {
            run_fields.push_str(&format!(",\"carved_unix\":{carved_unix}"));
        }
        format!(
            "{{\"schema_version\":3,\"event\":\"carve-file\",\"id\":\"{}\",\"artifact_type\":\"carved-candidate\",\"validation_status\":\"{}\",\"validation_note\":\"{}\",\"duplicate_of\":{},\"source_path\":\"{}\",\"output_path\":\"{}\",\"offset\":{},\"size_bytes\":{},\"signature\":\"{}\",\"extension\":\"{}\",\"sha256\":\"{}\"{run_fields}}}",
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
    /// Run tag shared by every artifact id of this run (E4).
    pub run_id: String,
    /// RIFF-declared spans inside which fragment-prone signatures
    /// (Annex-B, JPEG, MPEG-PS/TS) were not emitted as candidates (E3).
    pub suppressed_spans: Vec<(u64, u64)>,
    /// Parts of `suppressed_spans` that no carved artifact covers: bytes
    /// that were neither carved nor offered as fragment candidates. Any
    /// entry forces `scan_complete = false`.
    pub unexamined_spans: Vec<(u64, u64)>,
}

fn spans_json(spans: &[(u64, u64)]) -> String {
    spans
        .iter()
        .map(|(start, end)| format!("[{start}, {end}]"))
        .collect::<Vec<_>>()
        .join(", ")
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
        out.push_str(&format!(
            "  \"run_id\": \"{}\",\n  \"suppressed_spans\": [{}],\n  \"unexamined_spans\": [{}],\n",
            json_escape(&self.run_id),
            spans_json(&self.suppressed_spans),
            spans_json(&self.unexamined_spans)
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

    // E4: one tag per run, derived from the checkpoint's run id so an
    // interrupted run resumes under the same tag (replayed and fresh
    // artifacts share it) while every new run gets a fresh one.
    let run_tag = carve_run_tag(checkpoint.run_id());
    let (hits, scanned_to, suppress) = scan_signatures(
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
            spans: prior.suppressed_spans,
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
    // Complete only if the walk covered the whole source: no candidate cap,
    // no narrowed window, and every requested byte was actually read (a short
    // read — file shrank, I/O ended early — leaves `scanned_to < scan_end`).
    let short_read = scanned_to < scan_end && !candidate_limit_reached;
    let whole_source = scan_start == 0 && scan_end >= source_size;
    let scan_complete = !candidate_limit_reached && !short_read && whole_source;
    if short_read {
        warnings.push(format!(
            "short read: signature scan reached offset {scanned_to} but the requested window ended at {scan_end}; bytes beyond were not scanned"
        ));
    }
    if candidate_limit_reached {
        warnings.push(format!(
            "candidate limit reached at {}; scan completeness is not established (conservative even at EOF); additional candidates may exist; rerun with a higher --max-candidates",
            options.max_candidates
        ));
    }

    let mut artifacts = Vec::new();
    let mut resumed_artifacts = 0usize;
    // Output paths of artifacts replayed from the checkpoint; a prior run
    // may already have logged them (see `write_carve_outputs`).
    let mut replayed_outputs: Vec<PathBuf> = Vec::new();
    let mut first_by_hash = HashMap::<String, String>::new();
    // Fragment-prone signatures (elementary streams, embeddable JPEGs)
    // inside a span already claimed by an earlier candidate are the same
    // file's payload — suppress them instead of shredding one file into
    // fragments. Container signatures are never suppressed.
    let is_fragment_sig = |sig: &str| sig.ends_with("-annexb") || sig == "jpeg";
    let mut covered_until = 0u64;
    let replayed_ids: std::collections::HashSet<String> = carved_map
        .values()
        .map(|artifact| artifact.id.clone())
        .collect();
    let mut next_number = 0usize;
    for hit in &hits {
        if is_fragment_sig(&hit.signature) && hit.offset < covered_until {
            continue;
        }
        // Extents run to the next container signature; mid-run fragment-sig
        // anchors inside the same span would otherwise truncate it. A
        // container signature nested inside THIS hit's RIFF span (E3) is
        // a candidate of its own, not this AVI's end boundary.
        let next_offset = hits
            .iter()
            .filter(|candidate| !is_fragment_sig(&candidate.signature))
            .filter(|candidate| candidate.nested_in != Some(hit.offset))
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
        if let Some(owner) = hit.nested_in {
            validation_note.push_str(&format!(
                " Signature lies inside the RIFF AVI span declared at offset {owner}; it may be that container's own payload rather than an independent file."
            ));
        }
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
            replayed_outputs.push(artifact.output_path.clone());
            let mut replayed = artifact.clone();
            replayed.validation_note = validation_note;
            covered_until = covered_until.max(hit.offset + size_bytes);
            artifacts.push(replayed);
            continue;
        }

        // E4: `carve_<run>_<n>` — unique across runs, so a validation,
        // deepfake screening or timeline entry can never be attributed to
        // a different run's artifact that reused the same number.
        // Numbered by position (as before), skipping any replayed id.
        next_number = next_number.max(artifacts.len());
        let id = loop {
            next_number += 1;
            let candidate = format!("carve_{run_tag}_{next_number:06}");
            if !replayed_ids.contains(&candidate) {
                break candidate;
            }
        };
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
            run_id: Some(run_tag.clone()),
            carved_unix: Some(now_unix()?),
        };
        // Checkpoint the artifact right after it lands on disk: a crash
        // before this line re-carves the range, a crash after it replays.
        checkpoint.append_line(&format!("{{\"carved\":{}}}", artifact.to_json()))?;
        covered_until = covered_until.max(hit.offset + size_bytes);
        artifacts.push(artifact);
    }

    if options.reassemble {
        reassemble_fragments_tagged(
            &source_path,
            case_dir,
            source_size,
            &mut artifacts,
            &mut warnings,
            &run_tag,
        )?;
    }

    // E3: suppressed spans are reported, and any part of one that no
    // artifact covers (the AVI was truncated by --max-bytes, skipped, or
    // its structural extent ended early) was never examined for fragment
    // candidates — the scan cannot claim completeness over it.
    let suppressed_spans: Vec<(u64, u64)> = suppress
        .spans
        .iter()
        .map(|(start, end)| ((*start).max(scan_start), (*end).min(scan_end)))
        .filter(|(start, end)| end > start)
        .collect();
    let unexamined_spans = uncovered_parts(&suppressed_spans, &artifacts);
    if !suppressed_spans.is_empty() {
        warnings.push(format!(
            "fragment-signature suppression: Annex-B/JPEG/MPEG-PS/TS signatures inside {} RIFF-declared span(s) were not emitted as candidates (treated as that AVI's payload): {}",
            suppressed_spans.len(),
            span_list(&suppressed_spans)
        ));
    }
    if !unexamined_spans.is_empty() {
        warnings.push(format!(
            "unexamined bytes: {} suppressed span part(s) are not covered by any carved artifact and were never offered as fragment candidates: {}; scan completeness is not established",
            unexamined_spans.len(),
            span_list(&unexamined_spans)
        ));
    }
    let scan_complete = scan_complete && unexamined_spans.is_empty();

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
        run_id: run_tag,
        suppressed_spans,
        unexamined_spans,
    };
    write_carve_outputs(case_dir, &result, &replayed_outputs)?;
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

/// `<unix>-<hex4>` run tag from a checkpoint run id
/// (`carve-file-<unix>-<nanos>`): the run's start second plus four hex
/// digits of the id's SHA-256, so two runs started in the same second
/// still differ. Deterministic for a given run id (a resume keeps it).
pub(super) fn carve_run_tag(run_id: &str) -> String {
    let unix = run_id
        .split('-')
        .find(|part| part.len() >= 9 && part.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or("0");
    let digest = crate::sha256::digest_bytes(run_id.as_bytes());
    format!("{unix}-{}", &digest[..4])
}

/// Parts of `spans` not covered by any artifact's carved extent.
pub(super) fn uncovered_parts(
    spans: &[(u64, u64)],
    artifacts: &[CarvedArtifact],
) -> Vec<(u64, u64)> {
    // Reassembly hypotheses concatenate non-contiguous ranges, so their
    // (offset, size) is not a contiguous extent — only direct carves count.
    let mut covered: Vec<(u64, u64)> = artifacts
        .iter()
        .filter(|artifact| !artifact.signature.starts_with("reassembled-"))
        .map(|artifact| {
            (
                artifact.offset,
                artifact.offset.saturating_add(artifact.size_bytes),
            )
        })
        .collect();
    covered.sort_unstable();
    let mut out = Vec::new();
    for &(start, end) in spans {
        let mut cursor = start;
        for &(cov_start, cov_end) in &covered {
            if cov_end <= cursor || cov_start >= end {
                continue;
            }
            if cov_start > cursor {
                out.push((cursor, cov_start.min(end)));
            }
            cursor = cursor.max(cov_end);
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            out.push((cursor, end));
        }
    }
    out
}

/// At most 16 spans spelled out, then a count — warnings stay readable.
fn span_list(spans: &[(u64, u64)]) -> String {
    let mut parts: Vec<String> = spans
        .iter()
        .take(16)
        .map(|(start, end)| format!("[{start}, {end})"))
        .collect();
    if spans.len() > 16 {
        parts.push(format!("and {} more", spans.len() - 16));
    }
    parts.join(", ")
}
