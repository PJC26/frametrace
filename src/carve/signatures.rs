use super::*;

/// Serializes cumulative signature-scan progress for the checkpoint,
/// including suppression state so a resumed scan inside a container span
/// does not forget the boundary it crossed.
pub(super) fn scan_progress_line(
    offset: u64,
    hits: &[CarveHit],
    suppress: &ScanSuppress,
) -> String {
    let hits_json = hits
        .iter()
        .map(|hit| {
            let nested = hit
                .nested_in
                .map(|owner| format!(",\"nested_in\":{owner}"))
                .unwrap_or_default();
            format!(
                "{{\"offset\":{},\"signature\":\"{}\",\"extension\":\"{}\"{nested}}}",
                hit.offset,
                json_escape(&hit.signature),
                json_escape(&hit.extension)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let spans_json = suppress
        .spans
        .iter()
        .map(|(start, end)| format!("[{start},{end}]"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"scan\":{{\"offset\":{offset},\"embedded_until\":{},\"annexb_next_emit\":{},\"suppressed_spans\":[{spans_json}],\"hits\":[{hits_json}]}}}}",
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
pub(super) struct ScanWindow {
    pub(super) start: u64,
    pub(super) resume: u64,
    pub(super) end: u64,
}

pub(super) fn scan_signatures(
    source_path: &Path,
    window: ScanWindow,
    prior_hits: Vec<CarveHit>,
    prior_suppress: ScanSuppress,
    max_candidates: usize,
    checkpoint: &mut RunCheckpoint,
    progress: Option<&CarveProgress<'_>>,
) -> Result<(Vec<CarveHit>, u64, ScanSuppress), String> {
    let scan_start = window.start;
    let resume_offset = window.resume;
    let scan_end = window.end;
    let mut hits = prior_hits;
    hits.sort_by_key(|hit| hit.offset);
    hits.dedup_by_key(|hit| hit.offset);
    if hits.len() >= max_candidates {
        // The interrupted run had already stopped scanning at the cap.
        hits.truncate(max_candidates);
        return Ok((hits, resume_offset, prior_suppress));
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
    // `offset` is how far the walk actually read; the caller compares it with
    // the requested end to detect a short read. The suppression state goes
    // back too: its spans are where fragment signatures were NOT emitted,
    // which the caller must account for before claiming completeness.
    Ok((hits, offset, suppress))
}

/// EWF segment files begin with the libewf file signature
/// ("EVF\x09\x0d\x0a\xff\x00"). Carving the container directly means hit
/// offsets are container offsets and candidates can be damaged at
/// segment boundaries — a warning the result must carry.
pub(super) fn is_ewf_container(path: &Path) -> bool {
    const EWF_MAGIC: [u8; 8] = [b'E', b'V', b'F', 0x09, 0x0d, 0x0a, 0xff, 0x00];
    let mut head = [0u8; 8];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .map(|_| head == EWF_MAGIC)
        .unwrap_or(false)
}

pub(super) fn validation_note_for_signature(signature: &str) -> &'static str {
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
pub(super) fn scan_buffer(
    scan: &[u8],
    scan_start: u64,
    current_chunk_start: u64,
    hits: &mut Vec<CarveHit>,
) {
    scan_buffer_state(
        scan,
        scan_start,
        current_chunk_start,
        hits,
        &mut ScanSuppress::default(),
    );
}

pub(super) fn scan_buffer_state(
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

        // Inside a RIFF-declared container span (E3) only the
        // fragment-prone signatures (Annex-B anchors, JPEG, MPEG-PS/TS
        // runs) are suppressed — those are the container's own payload
        // and would flood `hits`. Container signatures with a real file
        // header (MP4 ftyp, RIFF AVI, DHAV, IMKH, EBML) are still emitted,
        // tagged `nested_in` = the span's RIFF offset: a false or
        // oversized RIFF size field (capped at MAX_RIFF_CLAIM_BYTES) must
        // not blind the scan to unrelated files inside that span. Spans
        // are recorded so the carve can report them and withhold
        // `scan_complete` where suppressed bytes were never carved.
        let embedded = absolute < suppress.embedded_until;
        let nested_in = if embedded {
            suppress.spans.last().map(|(start, _)| *start)
        } else {
            None
        };
        let container_hit = |offset: u64, signature: &str, extension: &str| CarveHit {
            offset,
            signature: signature.to_string(),
            extension: extension.to_string(),
            nested_in,
        };
        if index >= 4 && scan.get(index..index + 4) == Some(b"ftyp") {
            hits.push(container_hit(absolute - 4, "mp4-ftyp", "mp4"));
        }
        // A RIFF header re-seen in the chunk overlap is the one that opened
        // its own span — never a hit nested inside itself.
        let span_owner = suppress
            .spans
            .binary_search_by_key(&absolute, |(start, _)| *start)
            .is_ok();
        if scan.get(index..index + 4) == Some(b"RIFF")
            && scan.get(index + 8..index + 11) == Some(b"AVI")
            && !span_owner
        {
            hits.push(container_hit(absolute, "riff-avi", "avi"));
            // The RIFF header declares its own file extent: the size
            // field counts bytes after it, so the payload ends at
            // absolute + 8 + declared. Fragment signatures inside that
            // span are this container's payload — suppress them at scan
            // time or embedded SPS anchors/JPEG thumbnails flood `hits`
            // and exhaust the candidate cap. The strict "AVI " check keeps
            // suppression off weaker near-matches, and a nested RIFF never
            // widens the outer claim.
            if !embedded && scan.get(index + 8..index + 12) == Some(b"AVI ") {
                let declared = scan
                    .get(index + 4..index + 8)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64)
                    .unwrap_or(0);
                if declared > 8 {
                    let end = absolute + 8 + declared.min(MAX_RIFF_CLAIM_BYTES);
                    suppress.embedded_until = suppress.embedded_until.max(end);
                    suppress.spans.push((absolute, end));
                }
            }
        }
        if scan.get(index..index + 4) == Some(b"DHAV") {
            hits.push(container_hit(absolute, "dahua-dhav", "dav"));
        }
        if scan.get(index..index + 4) == Some(b"IMKH") {
            hits.push(container_hit(absolute, "hikvision-imkh", "mpg"));
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
                nested_in: None,
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
                    nested_in: None,
                });
            }
        }
        // EBML header — Matroska/WebM containers. Inside a declared AVI
        // span it is emitted as a nested candidate (see above).
        if scan.get(index..index + 4) == Some(&[0x1a, 0x45, 0xdf, 0xa3][..]) {
            hits.push(container_hit(absolute, "ebml-mkv", "mkv"));
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
                nested_in: None,
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
                        nested_in: None,
                    });
                    suppress.annexb_next_emit = absolute + ANNEXB_COALESCE_BYTES;
                }
            }
        }
    }
}
