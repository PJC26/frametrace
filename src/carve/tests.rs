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
    let streamed = super::find_video_signatures_in(&mut std::io::Cursor::new(&content), 2).unwrap();
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
    let root = std::env::temp_dir().join(format!("frametrace-reasm-{name}-{}", std::process::id()));
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
    let root = std::env::temp_dir().join(format!("frametrace-carve-window-{}", std::process::id()));
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
