use super::*;

/// Structural extent refinement: MP4/RIFF/TS containers declare or imply
/// their own end, so for those signatures the carved span is tightened to
/// the real boundary instead of blindly running to the next signature or
/// EOF. Returns (extent_bytes, boundary_note) — the note always states how
/// the end was determined so downstream review can weigh it.
pub(super) fn refine_extent(
    source: &Path,
    hit: &CarveHit,
    available: u64,
    max_bytes: u64,
) -> (u64, String) {
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
pub(super) fn mp4_extent(source: &Path, start: u64, span: u64) -> (u64, String) {
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
                    "MP4 top-level box headers tile the span exactly ({} boxes{}{}{}); only box headers were checked — box payloads, sample tables and playback were not verified{}.",
                    boxes,
                    if saw_ftyp { ", ftyp" } else { "" },
                    if saw_moov { ", moov" } else { "" },
                    if saw_mdat { ", mdat" } else { "" },
                    if saw_moov { "" } else { "; no moov box seen" }
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
pub(super) fn riff_extent(source: &Path, start: u64, span: u64) -> (u64, String) {
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
pub(super) fn ts_extent(source: &Path, start: u64, span: u64) -> (u64, String) {
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
