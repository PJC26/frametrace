use crate::artifacts::{ProxyOptions, ThumbnailOptions};
use crate::carve::CarveOptions;
use crate::e01::E01Options;
use crate::model::ScanOptions;
use crate::tsk::{TskInspectOptions, TskRecoverOptions};
use crate::validation::ValidationOptions;
use crate::video_export::{ExportFormat, ExportOptions};
use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub mod handlers;
use handlers::*;

#[derive(Parser, Debug)]
#[command(name = "FrameTrace", version, about = "Windows local workstation concept for reviewing video evidence", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
    /// Audit-chain key source override for this invocation: `env:VAR` reads
    /// hex/base64 key material from that variable (secret-store injection
    /// without touching disk), `file:PATH` reads a 0600 key file at PATH.
    /// Wins over every configured source
    #[arg(long, global = true, value_name = "env:VAR|file:PATH")]
    pub key_source: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Create a local forensic case folder.
    InitCase {
        case_dir: PathBuf,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        operator: Option<String>,
        #[arg(long)]
        device_id: Option<String>,
        #[arg(long)]
        device_serial: Option<String>,
        #[arg(long)]
        write_protect: Option<String>,
        #[arg(long)]
        acquisition_tool: Option<String>,
        #[arg(long)]
        evidence_hash: Option<String>,
        #[arg(long)]
        notes: Option<String>,
    },
    /// Index video candidates and likely manufacturer/parser lanes
    ScanFolder {
        case_dir: PathBuf,
        source_dir: PathBuf,
        #[arg(long)]
        hash: bool,
        #[arg(long)]
        no_ffprobe: bool,
        #[arg(long)]
        max_depth: Option<usize>,
        /// Rescan incrementally: reuse indexed records whose size+mtime still
        /// match (no re-hash/re-probe), reprocess new/changed files, mark
        /// missing paths stale
        #[arg(long)]
        incremental: bool,
        /// Resume from an interrupted run's checkpoint (the default when a
        /// checkpoint exists and its input fingerprint still matches)
        #[arg(long, overrides_with = "no_resume")]
        resume: bool,
        /// Discard any interrupted-run checkpoint and process every file
        #[arg(long, overrides_with = "resume")]
        no_resume: bool,
        /// Screen each file through the deepfake-lens sidecar and write
        /// artifacts/deepfake/<id>.json (slow on CPU-only machines)
        #[arg(long)]
        deepfake: bool,
    },
    /// Register an evidence source in the SQLite case database
    RegisterSource {
        case_dir: PathBuf,
        path: PathBuf,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        source_id: Option<String>,
        #[arg(long)]
        write_protect: Option<String>,
        #[arg(long)]
        acquisition_tool: Option<String>,
        #[arg(long)]
        evidence_hash: Option<String>,
        #[arg(long)]
        notes: Option<String>,
    },
    /// Record E01 metadata through libewf ewfinfo
    InspectE01 {
        case_dir: PathBuf,
        e01_file: PathBuf,
        #[arg(long)]
        hash_e01: bool,
        #[arg(long)]
        ewfinfo: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
        /// Also run the Sleuth Kit filesystem listing on the E01 itself —
        /// mmls/fls decompress via libewf, so no raw export is needed
        #[arg(long)]
        filesystem: bool,
    },
    /// Verify an E01 with libewf, export it to raw image form, hash the raw output
    ImportE01 {
        case_dir: PathBuf,
        e01_file: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        max_bytes: Option<u64>,
        #[arg(long)]
        skip_verify: bool,
        #[arg(long)]
        hash_e01: bool,
        #[arg(long)]
        ewfinfo: Option<String>,
        #[arg(long)]
        ewfverify: Option<String>,
        #[arg(long)]
        ewfexport: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Generate a serverless HTML review dashboard at review/index.html
    MakeReview {
        case_dir: PathBuf,
        /// Rewrite absolute source paths as <case>/… or <redacted:hash> tokens
        #[arg(long)]
        redact_paths: bool,
        /// Pre-build H.264 proxies for browser-unplayable records so the
        /// standalone review bundle plays them immediately
        #[arg(long)]
        build_proxies: bool,
    },
    /// Print the manufacturer/source parser plugin catalog as JSON
    ListParsers,
    /// Generate a case report at reports/case-report.html
    MakeReport {
        case_dir: PathBuf,
        /// Re-hash every indexed file for full revalidation; default trusts
        /// the stored index hashes and only flags stale index records
        #[arg(long)]
        rehash: bool,
        /// Rewrite absolute source paths as <case>/… or <redacted:hash> tokens
        /// before the report is generated (distributable-report privacy mode)
        #[arg(long)]
        redact_paths: bool,
        /// Read the finalize sign-off from this file instead of
        /// review/signoff.json (the workstation passes the not-yet-promoted
        /// record of a finalize in progress)
        #[arg(long)]
        signoff: Option<PathBuf>,
    },
    /// Generate reports/custody-statement.html — a printable chain-of-custody
    /// document (sources, job ledger, live audit-chain verification)
    MakeCustody { case_dir: PathBuf },
    /// Build a checksummed report/review package directory with manifest files
    PackageCase {
        case_dir: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        /// Package even while review marks are still queued
        /// (marks-pending/) or save conflicts are unresolved; the state is
        /// printed as a warning (default: refuse)
        #[arg(long)]
        allow_pending: bool,
        /// Publish under exactly reports/<NAME> (package_…); fail instead
        /// of picking another name when it is taken. The workstation's
        /// finalize passes the name it recorded in the sign-off lane.
        #[arg(long, conflicts_with = "output")]
        package_name: Option<String>,
    },
    /// Show the finalize journal of a case (review/finalize.inprogress.json:
    /// an interrupted finalize, or one another PC is running) and what the
    /// sign-off lane says about it; --dismiss moves it aside (never deletes)
    /// so the next finalize is no longer blocked
    FinalizeJournal {
        case_dir: PathBuf,
        /// Move the journal to review/finalize.dismissed-<unix>.json and
        /// record that in the sign-off lane. An interrupted finalize this PC
        /// can clean up is cleaned up first (as opening the case would);
        /// otherwise nothing is changed: check review/signoff.json and the
        /// reports folder yourself, then finalize again
        #[arg(long)]
        dismiss: bool,
        /// With --dismiss: also move a journal whose writer process still
        /// seems to run, or one whose cleanup left something to check
        #[arg(long, requires = "dismiss")]
        force: bool,
    },
    /// Verify a package written by package-case: manifest.sha256 entries vs
    /// files, package-manifest.json, audit chains, database snapshot and
    /// sign-off consistency; reports what is verified vs not verified
    VerifyPackage {
        package_dir: PathBuf,
        /// Exit with code 3 (not 0) when every file checks out but the
        /// package is from a finalize that did not complete, or its
        /// sign-off (record or audit-lane entry) is NOT VERIFIED or matched
        /// only structurally (no audit-key HMAC checked)
        #[arg(long)]
        strict: bool,
    },
    /// Export an indexed video or selected range as a client-deliverable MP4/AVI
    ExportVideo {
        case_dir: PathBuf,
        selector: String,
        #[arg(long)]
        format: String,
        #[arg(long)]
        start: Option<f64>,
        #[arg(long)]
        duration: Option<f64>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        timeout: Option<u64>,
        /// Burn exhibit label, case id, hash prefix, and ms timecode onto frames
        #[arg(long)]
        burn_in: bool,
        /// Exhibit label for burn-in, e.g. "갑 제3호증"
        #[arg(long)]
        exhibit: Option<String>,
        /// Digest the source at export time and log it as source_sha256;
        /// a disagreement with the indexed hash is flagged, not hidden
        #[arg(long)]
        hash_source: bool,
    },
    /// Remux a Dahua DAV export to MP4 without re-encoding (real-sample validation pending)
    ExportDav {
        case_dir: PathBuf,
        dav_file: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Queue browser-unplayable / proprietary-format videos for H.264 proxy transcoding with chained hash logging
    TranscodeQueue {
        case_dir: PathBuf,
        /// Comma-separated ids to queue (default: all unplayable candidates)
        #[arg(long)]
        only: Option<String>,
        /// Re-transcode even when a proxy already exists
        #[arg(long)]
        force: bool,
    },
    /// Extract dashcam GPS telemetry (NMEA + camm GPS per the public spec, sample-unvalidated) into artifacts/telemetry; proprietary IMU tracks are detected, not decoded
    ExtractTelemetry {
        case_dir: PathBuf,
        /// Indexed id (vid_*) or media file path
        selector: String,
    },
    /// Remux a Hikvision IMKH export to MP4 (strip 40-byte header; real-sample validation pending)
    ExportHik {
        case_dir: PathBuf,
        hik_file: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Generate a lower-bitrate review proxy MP4
    MakeProxy {
        case_dir: PathBuf,
        selector: String,
        #[arg(long)]
        max_width: Option<u32>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Generate a JPEG thumbnail for review/reporting
    MakeThumbnail {
        case_dir: PathBuf,
        selector: String,
        #[arg(long)]
        time: Option<f64>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Scan a raw file or forensic image for contiguous candidates and carve them
    CarveFile {
        case_dir: PathBuf,
        source_file: PathBuf,
        #[arg(long)]
        max_bytes: Option<u64>,
        #[arg(long)]
        max_candidates: Option<usize>,
        /// Resume from an interrupted run's checkpoint (default: auto when
        /// the checkpoint's source/options fingerprint still matches)
        #[arg(long, overrides_with = "no_resume")]
        resume: bool,
        /// Discard any interrupted-run checkpoint and rescan/recarve
        #[arg(long, overrides_with = "resume")]
        no_resume: bool,
        /// Also emit fragment-reassembly hypotheses (TS continuity-counter
        /// joins, MP4 bifragment gap-fills) as additional unvalidated
        /// candidate artifacts alongside the original fragments
        #[arg(long)]
        reassemble: bool,
        /// Start the signature scan at this byte offset (default 0).
        /// Bounds which bytes are scanned — unlike --max-bytes, which
        /// only caps each carved artifact's extent.
        #[arg(long)]
        scan_offset: Option<u64>,
        /// Scan at most this many bytes past --scan-offset (default: to
        /// EOF). The result records the scanned window explicitly.
        #[arg(long)]
        scan_length: Option<u64>,
    },
    /// List active/deleted files in a raw forensic image with Sleuth Kit mmls/fls
    InspectImage {
        case_dir: PathBuf,
        image_file: PathBuf,
        #[arg(long)]
        partition_offset: Option<u64>,
        #[arg(long, default_value_t = 20000)]
        max_entries: usize,
        #[arg(long)]
        mmls: Option<String>,
        #[arg(long)]
        fls: Option<String>,
        /// Bound for the mmls/fls runs in seconds (default 3600 — a
        /// ~230GiB image needs ~25 min for a full fls listing)
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Recover one filesystem inode from a raw forensic image with Sleuth Kit icat
    RecoverInode {
        case_dir: PathBuf,
        image_file: PathBuf,
        inode: String,
        #[arg(long, default_value_t = 0)]
        partition_offset: u64,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        recover_deleted: bool,
        #[arg(long)]
        include_slack: bool,
        #[arg(long)]
        skip_sparse_holes: bool,
        #[arg(long)]
        icat: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Batch-recover viewer-selected deleted inodes from a raw image with Sleuth Kit icat
    RecoverBatch {
        case_dir: PathBuf,
        image_file: PathBuf,
        /// Selection file; optional when --deleted-videos picks targets
        /// from the latest inspect-image entries
        selection: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        partition_offset: u64,
        /// Recover every deleted video-candidate inode found by the latest
        /// filesystem inspection instead of a selection file
        #[arg(long)]
        deleted_videos: bool,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Validate an indexed video, carved candidate, recovered inode, or artifact path with ffprobe
    ValidateArtifact {
        case_dir: PathBuf,
        selector: String,
        #[arg(long)]
        ffprobe: Option<String>,
    },
    /// Verify a chained JSONL audit log and report tamper status
    VerifyAudit { log_path: PathBuf },
    /// Repair an audit lane whose chain does not verify (e.g. a torn final
    /// line): cut it back to its verifying prefix, keep the cut bytes
    /// beside the lane (`<lane>.repair-<unix>.removed`), and append a
    /// `lane-repair` marker recording their SHA-256. The lane then shows as
    /// 수리됨 (repaired)
    RepairLane {
        log_path: PathBuf,
        /// Only report what would be cut; change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Rotate the audit-chain HMAC key: generate a new key, register it as
    /// the keyring's active key, and retire the previous key so older keyed
    /// entries stay verifiable
    RotateAuditKey {
        /// Key id stamped as entry_hmac_key_id on new entries
        /// (default: kid-<sha8 of the key>, matched by fingerprint)
        #[arg(long)]
        key_id: Option<String>,
        /// Append a signed audit-key-rotate marker entry to this audit log,
        /// or to every verifying lane of a case when given a case folder
        /// (repeatable; the marker is keyed under the NEW key)
        #[arg(long)]
        log: Vec<PathBuf>,
        /// Create a NEW keyring at the path FRAMETRACE_AUDIT_KEYRING_FILE
        /// names when nothing is there yet (without it, a missing explicit
        /// keyring is an error — the drive may just be disconnected)
        #[arg(long)]
        init: bool,
    },
    /// Batch-process a viewer selection file (export/proxy/thumbnail per item)
    ExportBatch {
        case_dir: PathBuf,
        selection: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// Batch-validate a viewer selection file with ffprobe
    ValidateBatch {
        case_dir: PathBuf,
        selection: PathBuf,
        /// Resume from an interrupted run's checkpoint (default: auto when
        /// the checkpoint's selection/options fingerprint still matches)
        #[arg(long, overrides_with = "no_resume")]
        resume: bool,
        /// Discard any interrupted-run checkpoint and revalidate every item
        #[arg(long, overrides_with = "resume")]
        no_resume: bool,
    },
    /// Join consecutive indexed videos losslessly (ffmpeg concat, stream
    /// copy only) into artifacts/joined with a sidecar manifest and a
    /// chained join-log entry. Exit code 3 when --plan-id no longer matches
    JoinVideo {
        case_dir: PathBuf,
        /// Indexed video ids (vid_*), comma-separated or repeated
        #[arg(long, required = true, num_args = 1..)]
        ids: Vec<String>,
        /// Junction tolerance in seconds (0.5-5.0)
        #[arg(long, default_value_t = crate::join::DEFAULT_TOLERANCE_SECONDS)]
        tolerance: f64,
        /// plan_id from a prior check; refuses (exit 3) when files changed
        #[arg(long)]
        plan_id: Option<String>,
        /// Temp-file tag ([A-Za-z0-9_-], ≤64); default cli-<unix>-<pid>
        #[arg(long)]
        job_tag: Option<String>,
        /// Print the plan JSON (the /api/join-check shape) and stop
        #[arg(long)]
        check: bool,
    },
    /// Measure the real sound level (peak/RMS dBFS via ffmpeg volumedetect)
    /// of indexed videos that have an audio track; results go to
    /// evidence/logs/audio-level-log.jsonl. Already-valid measurements are
    /// skipped unless --all
    MeasureAudio {
        case_dir: PathBuf,
        /// Optional viewer selection file limiting the targets
        selection: Option<PathBuf>,
        /// Re-measure files that already have a valid measurement
        #[arg(long)]
        all: bool,
        /// Resume from an interrupted run's checkpoint (default: auto)
        #[arg(long, overrides_with = "no_resume")]
        resume: bool,
        /// Discard any interrupted-run checkpoint
        #[arg(long, overrides_with = "resume")]
        no_resume: bool,
    },
    /// Merge other cases' video indexes into this case's index with
    /// merged_from provenance; sha256 duplicates are kept and marked
    /// duplicate_of (candidate-grade; no re-verification)
    MergeCases {
        case_dir: PathBuf,
        /// Source case directories to merge, in order
        #[arg(required = true, num_args = 1..)]
        source_case_dirs: Vec<PathBuf>,
    },
    /// Diff this case's video index against another case's (candidate-grade report)
    CompareCases {
        case_dir: PathBuf,
        other_case_dir: PathBuf,
        /// Output path inside this case (default: reports/case-compare.json)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Split the case index into known/unknown files against a sha256 list
    /// (one digest per line or `sha256,<rest>` rows; candidate-grade report)
    KnownHashFilter {
        case_dir: PathBuf,
        hash_list: PathBuf,
        /// Output path inside the case (default: reports/known-hash-filter.json)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Export the case video index as DFXML (Digital Forensics XML; values are
    /// recorded index claims, not re-verified measurements)
    ExportDfxml {
        case_dir: PathBuf,
        /// Output path inside the case (default: reports/case-index.dfxml)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Emit a candidate-grade JSONL event stream merging index, ffprobe, and carve timestamps
    Timeline {
        case_dir: PathBuf,
        /// Output path inside the case (default: db/timeline.jsonl)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Import reviewer marks exported from the evidence viewer into the case DB
    ImportMarks { case_dir: PathBuf, marks: PathBuf },
    /// Export stored reviewer marks as a JSON file for the viewer
    ExportMarks {
        case_dir: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Create a synthetic SQLite index benchmark database for scale validation
    BenchmarkDb {
        output_dir: PathBuf,
        #[arg(long, default_value_t = 10000)]
        rows: usize,
    },
    /// Screen a file for synthetic-media signals via the deepfake-lens
    /// sidecar (`deepfake-lens forensic <file> --format json`). Scores are
    /// review-priority signals, not authenticity verdicts.
    DeepfakeScreen {
        file: PathBuf,
        /// Write the full JSON report to this path instead of stdout
        #[arg(long)]
        json_out: Option<PathBuf>,
    },
    /// Screen every case record that lacks a deepfake artifact — covers
    /// indexed videos plus carved and filesystem-recovered files from
    /// E01 pipelines that never pass through scan-folder.
    DeepfakeScan {
        case_dir: PathBuf,
        /// Re-screen records that already have an artifact
        #[arg(long)]
        force: bool,
        /// Re-screen only records whose existing artifact records a
        /// screening failure (ok:false or error) — leaves good results
        /// untouched
        #[arg(long)]
        retry_failed: bool,
    },
    /// Print the current case/index status
    Inspect { case_dir: PathBuf },
    /// Diagnose tool availability, disk space, case DB integrity, and audit health
    Doctor {
        /// Case directory to inspect — omit for system checks only
        #[arg(long)]
        case: Option<PathBuf>,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Run forensic QA validation checks
    Qa {
        #[command(subcommand)]
        command: QaCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum QaCommands {
    /// Compare an indexed case against a TSV ground-truth corpus manifest
    Accuracy {
        case_dir: PathBuf,
        corpus_manifest: PathBuf,
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    /// Compare two case directories for normalized deterministic equivalence
    Reproducibility {
        left_case_dir: PathBuf,
        right_case_dir: PathBuf,
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    /// Check whether required report-defensible artifacts exist
    ReportDefense {
        case_dir: PathBuf,
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    /// Cross-check the SQLite index against db/videos.jsonl for divergence
    Consistency {
        case_dir: PathBuf,
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    /// Run a SQLite-backed scale benchmark and emit a performance report
    Performance {
        output_dir: PathBuf,
        #[arg(long, default_value_t = 10000)]
        rows: usize,
    },
    /// Run release-readiness QA and emit pass/fail blockers
    Release {
        case_dir: PathBuf,
        #[arg(long)]
        corpus_manifest: Option<PathBuf>,
        #[arg(long)]
        comparison_case: Option<PathBuf>,
        #[arg(long)]
        output_dir: Option<PathBuf>,
        #[arg(long)]
        performance_output_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 100000)]
        performance_rows: usize,
    },
    /// Scan indexed videos for candidate anomaly findings (not legal proof)
    Anomalies { case_dir: PathBuf },
}

/// R11-S8: a non-zero exit code a command that SUCCEEDED asks for
/// (`verify-package --strict` on an incomplete finalize: 3); `main` exits
/// with it after `run` returns `Ok`.
static REQUESTED_EXIT_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

fn request_exit_code(code: i32) {
    REQUESTED_EXIT_CODE.store(code, std::sync::atomic::Ordering::SeqCst);
}

/// R11-S8: the exit code for a successful [`run`] (0 unless a command
/// asked for another).
pub fn requested_exit_code() -> i32 {
    REQUESTED_EXIT_CODE.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let kind = e.kind();
            e.print()
                .map_err(|err| format!("failed to print command line help: {err}"))?;
            return if matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                Ok(())
            } else {
                Err("invalid command line".to_string())
            };
        }
    };

    if let Some(spec) = &cli.key_source {
        crate::audit_key::install_source_override(spec)?;
    }
    // R4-B15: a child started by the workstation checks that it signs with
    // the same audit key source as its parent and says so when it does not.
    // R5-S9: with the machine-readable prefix the workstation promotes the
    // line to a job warning (an examiner never sees a child's stderr).
    if let Some(warning) = crate::serve::key_source_mismatch_warning() {
        // R8-S7: bound to the step's token (forged lines do not count).
        eprintln!(
            "{}",
            crate::util::step_marker_line(crate::serve::KEY_MISMATCH_MARKER, &warning)
        );
    }

    match cli.command {
        Commands::InitCase {
            case_dir,
            title,
            operator,
            device_id,
            device_serial,
            write_protect,
            acquisition_tool,
            evidence_hash,
            notes,
        } => {
            let options = InitCaseOptions {
                title,
                operator,
                device_id,
                device_serial,
                write_protect,
                acquisition_tool,
                evidence_hash,
                notes,
            };
            init_case(&case_dir, &options)
        }
        Commands::ScanFolder {
            case_dir,
            source_dir,
            hash,
            no_ffprobe,
            max_depth,
            incremental,
            resume,
            no_resume,
            deepfake,
        } => {
            let options = ScanOptions {
                hash_files: hash,
                use_ffprobe: !no_ffprobe,
                max_depth,
                incremental,
                deepfake_screen: deepfake,
            };
            scan_folder(
                &case_dir,
                &source_dir,
                options,
                crate::checkpoint::ResumeMode::from_flags(resume, no_resume),
            )
        }
        Commands::RegisterSource {
            case_dir,
            path,
            kind,
            source_id,
            write_protect,
            acquisition_tool,
            evidence_hash,
            notes,
        } => {
            let options = RegisterSourceOptions {
                kind,
                source_id,
                write_protect,
                acquisition_tool,
                evidence_hash,
                notes,
            };
            register_source(&case_dir, &path, options)
        }
        Commands::InspectE01 {
            case_dir,
            e01_file,
            hash_e01,
            ewfinfo,
            timeout,
            filesystem,
        } => {
            let options = E01Options {
                output_path: None,
                max_bytes: None,
                skip_verify: false,
                hash_e01,
                ewfinfo_bin: ewfinfo.unwrap_or_else(|| "ewfinfo".to_string()),
                ewfverify_bin: "ewfverify".to_string(),
                ewfexport_bin: "ewfexport".to_string(),
                timeout_secs: timeout,
            };
            inspect_e01(&case_dir, &e01_file, options, filesystem)
        }
        Commands::ImportE01 {
            case_dir,
            e01_file,
            output,
            max_bytes,
            skip_verify,
            hash_e01,
            ewfinfo,
            ewfverify,
            ewfexport,
            timeout,
        } => {
            let options = E01Options {
                output_path: output,
                max_bytes,
                skip_verify,
                hash_e01,
                ewfinfo_bin: ewfinfo.unwrap_or_else(|| "ewfinfo".to_string()),
                ewfverify_bin: ewfverify.unwrap_or_else(|| "ewfverify".to_string()),
                ewfexport_bin: ewfexport.unwrap_or_else(|| "ewfexport".to_string()),
                timeout_secs: timeout,
            };
            import_e01(&case_dir, &e01_file, options)
        }
        Commands::ExportDav {
            case_dir,
            dav_file,
            output,
            timeout,
        } => export_dav(&case_dir, &dav_file, output, timeout),
        Commands::ExportHik {
            case_dir,
            hik_file,
            output,
            timeout,
        } => export_hik(&case_dir, &hik_file, output, timeout),
        Commands::MakeReview {
            case_dir,
            redact_paths,
            build_proxies,
        } => make_review(&case_dir, redact_paths, build_proxies),
        Commands::ListParsers => {
            println!("{}", crate::detector::parser_catalog_json());
            Ok(())
        }
        Commands::MakeReport {
            case_dir,
            rehash,
            redact_paths,
            signoff,
        } => make_report(&case_dir, rehash, redact_paths, signoff.as_deref()),
        Commands::MakeCustody { case_dir } => make_custody(&case_dir),
        Commands::PackageCase {
            case_dir,
            output,
            allow_pending,
            package_name,
        } => {
            let options = PackageOptions { output_dir: output };
            package_case_with_pending(&case_dir, options, allow_pending, package_name.as_deref())
        }
        Commands::FinalizeJournal {
            case_dir,
            dismiss,
            force,
        } => {
            let text = crate::serve::finalize_journal_command(&case_dir, dismiss, force)?;
            print!("{text}");
            Ok(())
        }
        Commands::VerifyPackage {
            package_dir,
            strict,
        } => {
            let code = crate::cli::handlers::verify_package_with(&package_dir, strict)?;
            request_exit_code(code);
            Ok(())
        }
        Commands::ExportVideo {
            case_dir,
            selector,
            format,
            start,
            duration,
            output,
            timeout,
            burn_in,
            exhibit,
            hash_source,
        } => {
            let fmt = ExportFormat::parse(&format)?;
            let options = ExportOptions {
                format: fmt,
                start_seconds: start,
                duration_seconds: duration,
                output_path: output,
                timeout_secs: timeout,
                hash_source,
                burn_in: if burn_in || exhibit.is_some() {
                    Some(crate::video_export::BurnInSpec {
                        exhibit: exhibit.unwrap_or_default(),
                    })
                } else {
                    None
                },
            };
            export_video(&case_dir, &selector, options)
        }
        Commands::TranscodeQueue {
            case_dir,
            only,
            force,
        } => transcode_queue(&case_dir, only.as_deref(), force),
        Commands::ExtractTelemetry { case_dir, selector } => {
            extract_telemetry(&case_dir, &selector)
        }
        Commands::MakeProxy {
            case_dir,
            selector,
            max_width,
            output,
        } => {
            let options = ProxyOptions {
                max_width: max_width.unwrap_or_else(|| ProxyOptions::default().max_width),
                output_path: output,
                timeout_secs: None,
            };
            make_proxy(&case_dir, &selector, options)
        }
        Commands::MakeThumbnail {
            case_dir,
            selector,
            time,
            output,
        } => {
            let options = ThumbnailOptions {
                time_seconds: time.unwrap_or(0.0),
                output_path: output,
                timeout_secs: None,
            };
            make_thumbnail(&case_dir, &selector, options)
        }
        Commands::CarveFile {
            case_dir,
            source_file,
            max_bytes,
            max_candidates,
            resume,
            no_resume,
            reassemble,
            scan_offset,
            scan_length,
        } => {
            let mut options = CarveOptions::default();
            if let Some(mb) = max_bytes {
                options.max_bytes = mb;
            }
            if let Some(mc) = max_candidates {
                options.max_candidates = mc;
            }
            options.reassemble = reassemble;
            options.scan_offset = scan_offset;
            options.scan_length = scan_length;
            carve_file(
                &case_dir,
                &source_file,
                options,
                crate::checkpoint::ResumeMode::from_flags(resume, no_resume),
            )
        }
        Commands::InspectImage {
            case_dir,
            image_file,
            partition_offset,
            max_entries,
            mmls,
            fls,
            timeout,
        } => {
            let options = TskInspectOptions {
                partition_offset,
                max_entries,
                mmls_bin: mmls.unwrap_or_else(|| "mmls".to_string()),
                fls_bin: fls.unwrap_or_else(|| "fls".to_string()),
                // 0 disables the cap: large multi-segment E01 sets need
                // minutes of libewf decompression inside fls, and the
                // probe default would kill them mid-walk.
                timeout_secs: match timeout {
                    Some(0) => None,
                    Some(t) => Some(t),
                    // R4-C7: default scales with the image (1 h per started TB).
                    None => Some(crate::tsk::scaled_inspect_timeout_secs(&image_file)),
                },
            };
            inspect_image(&case_dir, &image_file, options)
        }
        Commands::RecoverInode {
            case_dir,
            image_file,
            inode,
            partition_offset,
            output,
            recover_deleted,
            include_slack,
            skip_sparse_holes,
            icat,
            timeout,
        } => {
            let options = TskRecoverOptions {
                partition_offset,
                inode,
                output_path: output,
                recover_deleted,
                include_slack,
                skip_sparse_holes,
                icat_bin: icat.unwrap_or_else(|| "icat".to_string()),
                timeout_secs: timeout,
            };
            recover_inode(&case_dir, &image_file, options)
        }
        Commands::RecoverBatch {
            case_dir,
            image_file,
            selection,
            partition_offset,
            deleted_videos,
            timeout,
        } => recover_batch(
            &case_dir,
            &image_file,
            selection.as_deref(),
            partition_offset,
            deleted_videos,
            timeout,
        ),
        Commands::ValidateArtifact {
            case_dir,
            selector,
            ffprobe,
        } => {
            let options = ValidationOptions {
                ffprobe_bin: ffprobe.unwrap_or_else(|| "ffprobe".to_string()),
            };
            validate_artifact(&case_dir, &selector, options)
        }
        Commands::VerifyAudit { log_path } => verify_audit(&log_path),
        Commands::RepairLane { log_path, dry_run } => repair_lane(&log_path, dry_run),
        Commands::RotateAuditKey { key_id, log, init } => {
            rotate_audit_key(key_id.as_deref(), &log, init)
        }
        Commands::ExportBatch {
            case_dir,
            selection,
            dry_run,
        } => export_batch(&case_dir, &selection, dry_run),
        Commands::ValidateBatch {
            case_dir,
            selection,
            resume,
            no_resume,
        } => validate_batch(
            &case_dir,
            &selection,
            crate::checkpoint::ResumeMode::from_flags(resume, no_resume),
        ),
        Commands::JoinVideo {
            case_dir,
            ids,
            tolerance,
            plan_id,
            job_tag,
            check,
        } => join_video(
            &case_dir,
            &JoinVideoArgs {
                ids: split_ids(&ids),
                tolerance,
                plan_id,
                job_tag,
                check,
            },
        ),
        Commands::MeasureAudio {
            case_dir,
            selection,
            all,
            resume,
            no_resume,
        } => measure_audio(
            &case_dir,
            selection.as_deref(),
            all,
            crate::checkpoint::ResumeMode::from_flags(resume, no_resume),
        ),
        Commands::KnownHashFilter {
            case_dir,
            hash_list,
            output,
        } => known_hash_filter(&case_dir, &hash_list, output),
        Commands::ExportDfxml { case_dir, output } => export_dfxml(&case_dir, output),
        Commands::Timeline { case_dir, output } => timeline(&case_dir, output),
        Commands::MergeCases {
            case_dir,
            source_case_dirs,
        } => merge_cases(&case_dir, &source_case_dirs),
        Commands::CompareCases {
            case_dir,
            other_case_dir,
            output,
        } => compare_cases(&case_dir, &other_case_dir, output),
        Commands::ImportMarks { case_dir, marks } => import_marks(&case_dir, &marks),
        Commands::ExportMarks { case_dir, output } => export_marks(&case_dir, output.as_deref()),
        Commands::BenchmarkDb { output_dir, rows } => {
            let options = BenchmarkOptions { rows };
            benchmark_db(&output_dir, options)
        }
        Commands::DeepfakeScreen { file, json_out } => deepfake_screen(&file, json_out.as_deref()),
        Commands::DeepfakeScan {
            case_dir,
            force,
            retry_failed,
        } => deepfake_scan(&case_dir, force, retry_failed),
        Commands::Inspect { case_dir } => inspect(&case_dir),
        Commands::Doctor { case, json } => doctor(case.as_deref(), json),
        Commands::Qa { command } => run_qa(command),
    }
}

fn run_qa(command: QaCommands) -> Result<(), String> {
    match command {
        QaCommands::Accuracy {
            case_dir,
            corpus_manifest,
            output_dir,
        } => {
            let output_dir = output_dir.unwrap_or_else(|| case_dir.join("reports/qa"));
            let report = crate::qa::accuracy_report(&case_dir, &corpus_manifest, &output_dir)?;
            println!("accuracy QA passed: {}", report.report_path.display());
            Ok(())
        }
        QaCommands::Reproducibility {
            left_case_dir,
            right_case_dir,
            output_dir,
        } => {
            let output_dir = output_dir.unwrap_or_else(|| left_case_dir.join("reports/qa"));
            let report =
                crate::qa::reproducibility_report(&left_case_dir, &right_case_dir, &output_dir)?;
            println!(
                "reproducibility QA passed: {}",
                report.report_path.display()
            );
            Ok(())
        }
        QaCommands::ReportDefense {
            case_dir,
            output_dir,
        } => {
            let output_dir = output_dir.unwrap_or_else(|| case_dir.join("reports/qa"));
            let report = crate::qa::report_defense_check(&case_dir, &output_dir)?;
            println!("report-defense QA passed: {}", report.report_path.display());
            Ok(())
        }
        QaCommands::Consistency {
            case_dir,
            output_dir,
        } => {
            let output_dir = output_dir.unwrap_or_else(|| case_dir.join("reports/qa"));
            let report = crate::qa::consistency_report(&case_dir, &output_dir)?;
            println!("consistency QA passed: {}", report.report_path.display());
            Ok(())
        }
        QaCommands::Performance { output_dir, rows } => {
            let report = crate::qa::performance_report(&output_dir, rows)?;
            println!("performance QA passed: {}", report.report_path.display());
            Ok(())
        }
        QaCommands::Release {
            case_dir,
            corpus_manifest,
            comparison_case,
            output_dir,
            performance_output_dir,
            performance_rows,
        } => {
            let output_dir = output_dir.unwrap_or_else(|| case_dir.join("reports/qa"));
            let options = crate::qa::ReleaseReadinessOptions {
                corpus_manifest,
                comparison_case_dir: comparison_case,
                performance_output_dir,
                performance_rows,
            };
            let report = crate::qa::release_readiness_report(&case_dir, &output_dir, &options)?;
            println!(
                "release readiness QA passed: {}",
                report.report_path.display()
            );
            Ok(())
        }
        QaCommands::Anomalies { case_dir } => {
            let result = crate::anomaly::scan_case(&case_dir, true)?;
            // R12-I4: first line, before any finding text.
            if let Some(run_id) = &result.hash_verify_run_id {
                println!("{}{run_id}", crate::anomaly::HASH_VERIFY_RUN_ID_PREFIX);
            }
            println!(
                "anomaly scan complete: {} candidate finding(s)",
                result.findings.len()
            );
            println!(
                "coverage: skipped_no_hash={} skipped_missing={} skipped_error={}",
                result.coverage.skipped_no_hash,
                result.coverage.skipped_missing,
                result.coverage.skipped_error
            );
            if result.coverage.limited() {
                println!(
                    "coverage limitation: not every indexed file was revalidated; see the run entry in {}",
                    result.log_path.display()
                );
            }
            println!("log: {}", result.log_path.display());
            for finding in &result.findings {
                println!(
                    "- [{}] {} · {}",
                    finding.kind, finding.selector, finding.detail
                );
            }
            Ok(())
        }
    }
}
