//! Shared job state, pipeline descriptors, busy-flag guard, and session persistence.

use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum InputKind {
    Folder,
    E01,
    /// Same evidence kind as E01 but triaged directly on the segment set.
    E01Direct,
}

impl InputKind {
    pub(crate) fn step_names(self) -> &'static [&'static str] {
        match self {
            InputKind::Folder => &[
                "케이스 준비",
                "소스 등록",
                "스캔 · 색인",
                "재생성 검증",
                "리뷰 생성",
            ],
            InputKind::E01 => &[
                "케이스 준비",
                "E01 검증 · 추출",
                "이미지 조사 (mmls/fls)",
                "논리 파일 색인",
                "리뷰 생성",
            ],
            InputKind::E01Direct => &[
                "케이스 준비",
                "E01 메타데이터 (ewfinfo)",
                "파일시스템 조사 (export 생략)",
                "논리 파일 색인",
                "리뷰 생성",
            ],
        }
    }
}

pub(crate) struct PipelineJob {
    pub(crate) kind: InputKind,
    pub(crate) case_dir: PathBuf,
    pub(crate) source_path: PathBuf,
    pub(crate) with_hash: bool,
    pub(crate) with_ffprobe: bool,
    pub(crate) with_deepfake: bool,
    pub(crate) skip_e01_verify: bool,
    /// Triage mode: inspect-e01 --filesystem reads the segment set
    /// directly instead of exporting hundreds of GiB to raw first.
    pub(crate) e01_direct: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum StepStatus {
    Pending,
    Running,
    Done,
    Failed,
}

impl StepStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Done => "done",
            StepStatus::Failed => "failed",
        }
    }
}

pub(crate) struct JobState {
    /// idle | running | review-partial | review-ready | finalizing | done | error.
    /// `review-partial` (UX-02): the fast hash-less index exists and the
    /// viewer is usable while phase 2 (hash / validate / anomalies /
    /// make-review) enriches it in the background — `busy` stays true
    /// while that enrichment runs, false once it stopped (cancel/failure).
    pub(crate) phase: &'static str,
    pub(crate) steps: [StepStatus; 5],
    pub(crate) step_names: Vec<&'static str>,
    pub(crate) logs: Vec<String>,
    pub(crate) case_dir: Option<PathBuf>,
    pub(crate) media_roots: Vec<PathBuf>,
    pub(crate) package_dir: Option<PathBuf>,
    pub(crate) error: Option<String>,
    pub(crate) busy: bool,
    pub(crate) cancel_requested: bool,
    /// Byte-level progress hint for steps that run inside an external tool
    /// (ewfexport) where no in-process callback exists: the pipeline spawns
    /// a thread that stats the growing output file and stores its size
    /// here. `None` when the DB `jobs` row is the progress source.
    pub(crate) byte_progress: Option<(PathBuf, Option<u64>)>,
    /// Optional shared-secret gate from FRAMETRACE_TOKEN. `None` keeps the
    /// documented loopback-trust model; when set, every request must prove
    /// the token via header, query, or the planted `ft_token` cookie.
    pub(crate) token: Option<String>,
    /// Set by POST /api/shutdown; the accept loop polls this flag and exits
    /// once in-flight connections drain so the workstation can actually be
    /// closed from the UI (the windowed launcher has no console to close).
    pub(crate) shutdown_requested: bool,
    /// Random per-process nonce echoed by /api/status and written to the
    /// user-private instance file. `find_running_server` requires this
    /// marker so a spoofed `"app":"frametrace"` string on the probe port
    /// range can no longer impersonate the workstation.
    pub(crate) instance: String,
    /// Parsed video_index.json cache shared by /api/records and
    /// /api/records-meta. A large index re-parses only when the file
    /// actually changes; handlers clone the `Arc` (not the document) and
    /// release the state lock before filtering (PERF-01).
    pub(crate) index_cache: Option<IndexCache>,
    /// Parsed live scan checkpoint, keyed by path+mtime+len so polling
    /// `/api/records?live=1` does not re-parse an unchanged checkpoint.
    pub(crate) live_cache: Option<LiveCache>,
    /// Non-fatal step failures from the last pipeline run (FOR-08):
    /// `(step index, message)`. Surfaced as `warnings` in /api/status so a
    /// skipped validation/anomaly pass is never mistaken for a clean run.
    pub(crate) warnings: Vec<(usize, String)>,
    /// True when the current case binding came from the startup
    /// workstation-session file rather than an explicit user action.
    /// The UI pauses its auto-jump to the review stage and offers
    /// "이어서 검토 / 새 분석" instead of silently landing the examiner
    /// inside a previous case. Cleared by open-case and by a new
    /// analysis start.
    pub(crate) restored: bool,
    /// Phase-2 enrichment step currently running (UX-02); `None` outside
    /// phase 2. Surfaced as `enrichment` in /api/status.
    pub(crate) enrichment: Option<Enrichment>,
    /// Monotonic counter bumped whenever the case index (or the live scan
    /// checkpoint overlaid on it) changes on disk; the viewer polls it to
    /// learn that a refetch would show newer data.
    pub(crate) index_generation: u64,
    /// File fingerprint the current `index_generation` was taken from.
    pub(crate) index_fingerprint: Option<IndexFingerprint>,
    /// Bumped (under `MARKS_GATE`) whenever marks reach the case DB while
    /// phase 2 runs; make-review re-runs until it has seen the latest.
    pub(crate) marks_epoch: u64,
    /// Current or last background case job (UX-06: recover-deleted /
    /// carve). Kept after it ends so /api/status can report `done` /
    /// `failed` / `cancelled` to a client that polls after the fact.
    pub(crate) case_job: Option<CaseJob>,
    /// Clip-export jobs (UX-09), running and recently finished. They do
    /// NOT hold `busy`, so marks autosave keeps working during an export.
    pub(crate) clip_jobs: Vec<ClipJob>,
    /// Monotonic id source for `clip_jobs`.
    pub(crate) clip_seq: u64,
}

/// A background case job (UX-06) run under the busy lease.
#[derive(Clone, Debug)]
pub(crate) struct CaseJob {
    /// Stable machine key: `recover-deleted` | `carve`.
    pub(crate) kind: &'static str,
    pub(crate) started_unix: u64,
    pub(crate) finished_unix: Option<u64>,
    /// 1-based stage within `stages` (main step, then make-review).
    pub(crate) stage: usize,
    pub(crate) stages: usize,
    pub(crate) stage_label: &'static str,
    /// running | done | failed | cancelled.
    pub(crate) state: &'static str,
    pub(crate) error: Option<String>,
}

/// One clip export (UX-09). Identical requests share one job (`key`).
#[derive(Clone, Debug)]
pub(crate) struct ClipJob {
    pub(crate) id: String,
    /// Dedupe key: case + selector + range + burn-in options.
    pub(crate) key: String,
    pub(crate) record_id: String,
    pub(crate) output: PathBuf,
    /// Output path relative to the case directory (what the UI shows).
    pub(crate) rel_path: String,
    pub(crate) started_unix: u64,
    pub(crate) finished_unix: Option<u64>,
    /// running | done | failed | cancelled.
    pub(crate) state: &'static str,
    pub(crate) sha256: Option<String>,
    pub(crate) size_bytes: Option<u64>,
    pub(crate) error: Option<String>,
    pub(crate) cancel_requested: bool,
}

/// One phase-2 enrichment step (UX-02).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Enrichment {
    /// Stable machine key: hash | validate | anomalies | make-review.
    pub(crate) step: &'static str,
    /// Korean label for the UI chip.
    pub(crate) label: &'static str,
    /// 1-based position within the enrichment plan.
    pub(crate) stage: usize,
    pub(crate) stages: usize,
}

/// `(path, mtime, len)` of the index plus the live checkpoint; any change
/// bumps `JobState::index_generation`.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct IndexFingerprint {
    pub(crate) case_dir: PathBuf,
    pub(crate) index: Option<(SystemTime, u64)>,
    pub(crate) progress: Option<(SystemTime, u64)>,
}

impl JobState {
    pub(crate) fn new() -> Self {
        Self {
            phase: "idle",
            steps: [StepStatus::Pending; 5],
            step_names: InputKind::Folder.step_names().to_vec(),
            logs: Vec::new(),
            case_dir: None,
            media_roots: Vec::new(),
            package_dir: None,
            error: None,
            busy: false,
            cancel_requested: false,
            byte_progress: None,
            token: None,
            shutdown_requested: false,
            instance: new_instance_id(),
            index_cache: None,
            live_cache: None,
            warnings: Vec::new(),
            restored: false,
            enrichment: None,
            index_generation: 0,
            index_fingerprint: None,
            marks_epoch: 0,
            case_job: None,
            clip_jobs: Vec::new(),
            clip_seq: 0,
        }
    }
}

/// RAII busy-flag guard for handlers that run a synchronous `run_step`
/// child. The previous check-then-act pattern (`read busy → early-return
/// → run`) let two concurrent POSTs both observe `busy=false` and start
/// overlapping CLI jobs — the audit chain itself can't fork (appends take
/// an fs2 byte-range lock), but a second scan/transcode pass could rewrite
/// the index while the first queue was still reading it.
pub(crate) struct BusyGuard<'a> {
    pub(crate) state: &'a SharedState,
}

/// Atomically takes `busy` or returns the standard in-progress error.
pub(crate) fn try_acquire_busy(state: &SharedState) -> Result<BusyGuard<'_>, String> {
    let mut guard = state_lock(state);
    if guard.busy {
        return Err("분석 작업이 진행 중입니다 — 완료 후 다시 시도하십시오.".to_string());
    }
    guard.busy = true;
    // UX-07: a cancel aimed at a previous job must not kill this one —
    // every run starts with a fresh cancel flag.
    guard.cancel_requested = false;
    Ok(BusyGuard { state })
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        state_lock(self.state).busy = false;
    }
}

/// Owned busy lease for a job that outlives its HTTP handler (UX-06): the
/// worker thread holds it and releases `busy` in the same lock as its
/// final state update, so no window exists where another job could take
/// `busy` and then have it cleared under it. Dropping an unreleased lease
/// (a panicking worker) still frees `busy`.
pub(crate) struct BusyLease {
    state: Option<SharedState>,
}

/// Atomically takes `busy` for a background job, like [`try_acquire_busy`].
pub(crate) fn try_lease_busy(state: &SharedState) -> Result<BusyLease, String> {
    std::mem::forget(try_acquire_busy(state)?);
    Ok(BusyLease {
        state: Some(Arc::clone(state)),
    })
}

impl BusyLease {
    /// Clears `busy` after applying `finish` under the same state lock.
    pub(crate) fn release_with(mut self, finish: impl FnOnce(&mut JobState)) {
        if let Some(state) = self.state.take() {
            let mut guard = state_lock(&state);
            finish(&mut guard);
            guard.busy = false;
        }
    }
}

impl Drop for BusyLease {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            state_lock(&state).busy = false;
        }
    }
}

/// Convenience for the many `{"ok":false,"error":...}` early returns.
pub(crate) fn api_err(message: &str) -> String {
    format!("{{\"ok\":false,\"error\":{}}}", json_string(message))
}

/// Random 128-bit hex nonce identifying this server process.
pub(crate) fn new_instance_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Entropy failure degrades to an empty marker — the strict instance
        // check in find_running_server then simply never matches, which is
        // fail-closed (we spawn a fresh server instead of reusing a stranger).
        return String::new();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Cached parse of `db/video_index.json`, keyed by path + mtime + length.
pub(crate) struct IndexCache {
    pub(crate) path: PathBuf,
    pub(crate) mtime: SystemTime,
    pub(crate) len: u64,
    pub(crate) value: Arc<serde_json::Value>,
}

pub(crate) struct LiveCache {
    pub(crate) path: PathBuf,
    pub(crate) mtime: SystemTime,
    pub(crate) len: u64,
    pub(crate) records: Arc<Vec<serde_json::Value>>,
    pub(crate) skipped: usize,
}

pub(crate) type SharedState = Arc<Mutex<JobState>>;

pub(crate) fn state_lock(state: &SharedState) -> MutexGuard<'_, JobState> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// `<config>/frametrace/workstation-session.json` — remembers the last
/// opened case so restarting the workstation returns to it instead of
/// resetting to an empty stage 1 (and 404ing /review/*).
pub(crate) fn session_file() -> Option<PathBuf> {
    // Unit tests drive open-case/start in-process; never let them overwrite
    // the examiner's real session file.
    if cfg!(test) {
        return Some(std::env::temp_dir().join(format!(
            "frametrace-test-session-{}.json",
            std::process::id()
        )));
    }
    crate::audit_key::config_dir().map(|dir| dir.join("workstation-session.json"))
}

pub(crate) fn save_session(case_dir: &Path, source_path: Option<&Path>) {
    let Some(file) = session_file() else { return };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let source = source_path
        .map(|p| format!(",\"source_path\":{}", json_string(&p.display().to_string())))
        .unwrap_or_default();
    let body = format!(
        "{{\"case_dir\":{}{}}}",
        json_string(&case_dir.display().to_string()),
        source
    );
    // A torn session file reads as "no session" — silently losing the
    // case binding on next launch is a poor failure mode for one write.
    let _ = crate::util::write_text_atomic(&file, &body);
}

/// Restore the last case binding from the session file. Returns the case
/// dir plus any extra media root (folder-input source path) that still
/// exists on disk.
pub(crate) fn restore_session() -> Option<(PathBuf, Vec<PathBuf>)> {
    let text = std::fs::read_to_string(session_file()?).ok()?;
    let case_dir = PathBuf::from(body_value(&text, "case_dir")?);
    if !case_dir.join("case.json").is_file() {
        return None;
    }
    let mut roots = vec![case_dir.clone()];
    if let Some(source) = body_value(&text, "source_path") {
        let source = PathBuf::from(source);
        if source.is_dir() && source != case_dir {
            roots.push(source);
        }
    }
    Some((case_dir, roots))
}
