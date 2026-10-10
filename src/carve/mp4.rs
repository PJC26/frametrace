use super::*;

#[derive(Debug)]
pub(super) struct Mp4Box {
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
pub(super) fn mp4_top_boxes(source: &Path, start: u64, span: u64) -> Vec<Mp4Box> {
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
pub(super) fn mp4_sample_payload_bytes(
    source: &Path,
    frag_start: u64,
    moov: &Mp4Box,
) -> Option<u64> {
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
pub(super) fn find_moov_in_region(
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
pub(super) fn mp4_bifragment_joins(
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
pub(super) fn emit_mp4_gapfills(
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
