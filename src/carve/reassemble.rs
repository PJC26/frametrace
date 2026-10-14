use super::*;

//
// Joined artifacts are derived data: they are NOT checkpointed (a resume
// replays the source fragments and deterministically re-derives them)
// and they never consume the --max-candidates budget.

pub(super) const MAX_REASSEMBLED: usize = 32;

pub(super) const MAX_JOINS_PER_HEAD: usize = 4;

pub(super) const MIN_REGION_BYTES: u64 = 512;

pub(super) const TS_TAIL_PACKETS: u64 = 32;

pub(super) struct JoinSpec {
    pub(super) ranges: Vec<(u64, u64)>,
    pub(super) signature: &'static str,
    pub(super) extension: &'static str,
    pub(super) offset: u64,
    pub(super) note: String,
}

/// Test entry point with a fixed run tag.
#[cfg(test)]
pub(super) fn reassemble_fragments(
    source: &Path,
    case_dir: &Path,
    source_size: u64,
    artifacts: &mut Vec<CarvedArtifact>,
    warnings: &mut Vec<String>,
) -> Result<(), String> {
    reassemble_fragments_tagged(source, case_dir, source_size, artifacts, warnings, "test")
}

pub(super) fn reassemble_fragments_tagged(
    source: &Path,
    case_dir: &Path,
    source_size: u64,
    artifacts: &mut Vec<CarvedArtifact>,
    warnings: &mut Vec<String>,
    run_tag: &str,
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
        // Run-unique like carve ids (E4).
        let id = format!("reasm_{run_tag}_{:06}", index + 1);
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
            run_id: Some(run_tag.to_string()),
            carved_unix: Some(now_unix()?),
        });
    }
    Ok(())
}

/// Disk regions not claimed by any artifact's carved extent: before the
/// first signature and between fragments. These are the only places a
/// signature-less continuation fragment can physically live.
pub(super) fn anonymous_regions(artifacts: &[CarvedArtifact], source_size: u64) -> Vec<(u64, u64)> {
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
pub(super) fn ts_packet_header(packet: &[u8]) -> Option<(u16, u8, bool)> {
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
pub(super) fn ts_tail_cc(source: &Path, offset: u64, len: u64) -> HashMap<u16, u8> {
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
pub(super) fn ts_head_cc(source: &Path, offset: u64, len: u64) -> HashMap<u16, (u8, bool)> {
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
pub(super) fn ts_cc_continues(
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
pub(super) fn ts_cc_joins(
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

pub(super) fn flush_ts_group(
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
        // E7: a continuity counter is 4 bits, so ANY two fragments agree
        // on a shared PID by chance 1 time in 16 per boundary. That is a
        // weak structural hint, not join evidence — the grade says so.
        note: format!(
            "REASSEMBLY HYPOTHESIS (evidence grade: weak): {} MPEG-TS fragments ({}) joined because per-PID continuity counters line up at every boundary (pids {:?}). Weak structural evidence only: a 4-bit counter matches by chance about 1 in 16 per boundary PID, so unrelated fragments can interlock; still a candidate — validate playback and content continuity before reporting.",
            group.len(),
            members.join(", "),
            unique_pids
        ),
    });
    warnings.push(format!(
        "reassembled {} TS fragments into one weak join hypothesis (continuity counters line up; about 1-in-16 chance agreement per boundary PID)",
        group.len()
    ));
}

// --- MP4 bifragment joins -------------------------------------------------
