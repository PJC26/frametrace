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

#[derive(Clone)]
pub(crate) struct PipelineJob {
    pub(crate) kind: InputKind,
    pub(crate) case_dir: PathBuf,
    pub(crate) source_path: PathBuf,
    pub(crate) with_hash: bool,
    pub(crate) with_ffprobe: bool,
    pub(crate) with_deepfake: bool,
    /// Phase-2 "소리 측정" step (measure-audio); only with `with_ffprobe`.
    pub(crate) with_audio: bool,
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
    /// R4-B16: the step completed but its tool reported a partial result
    /// (CLI exit code 5, e.g. an fls listing cut off by its timeout). The
    /// run continues; a job warning says what is missing.
    Partial,
    Failed,
}

impl StepStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Done => "done",
            StepStatus::Partial => "partial",
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
    /// Bumped every time `busy` is taken (P2): a [`BusyGuard`] /
    /// [`BusyLease`] only clears `busy` on drop while it still owns the
    /// acquisition it made, so a late drop can never free a newer job's
    /// flag.
    pub(crate) busy_epoch: u64,
    /// Synchronous case writes running beside `busy` (S1): export-selected
    /// (a long batch — copies and hashes up to 5000 files) and
    /// capture-frame. Shutdown, finalize, open-case and a new analysis
    /// refuse while any is in flight.
    pub(crate) write_ops: usize,
    /// R3: cancel flag of every write op in flight, keyed by its lease id.
    /// `/api/cancel` (with no pipeline job holding `busy`) sets them all; a
    /// lease removes its own entry on drop, so a cancel never reaches a
    /// write op that starts afterwards.
    pub(crate) write_cancels: Vec<(u64, Arc<AtomicBool>)>,
    pub(crate) next_write_op_id: u64,
    /// Set by finalize (under the same lock that saw no write op and no
    /// clip job running) for as long as it seals the case; new write ops
    /// are refused meanwhile (S1).
    pub(crate) sealing: bool,
    pub(crate) cancel_requested: bool,
    /// Byte-level progress hint for steps that run inside an external tool
    /// (ewfexport) where no in-process callback exists: the pipeline spawns
    /// a thread that stats the growing output file and stores its size
    /// here. `None` when the DB `jobs` row is the progress source.
    pub(crate) byte_progress: Option<(PathBuf, Option<u64>)>,
    /// R6-S4: `(bytes hashed, total)` while an existing raw image is
    /// re-hashed against its import record (`/api/status.progress`).
    pub(crate) rehash_progress: Option<(u64, u64)>,
    /// Optional shared-secret gate from FRAMETRACE_TOKEN. `None` keeps the
    /// documented loopback-trust model; when set, every request must prove
    /// the token via header, query, or the planted `ft_token_<port>` cookie (C2).
    pub(crate) token: Option<String>,
    /// Set by POST /api/shutdown; the accept loop polls this flag and exits
    /// once in-flight connections drain so the workstation can actually be
    /// closed from the UI (the windowed launcher has no console to close).
    /// An atomic shared with `serve_on`, which clones it once and then polls
    /// it without ever taking the state lock: accepting connections must not
    /// depend on how long some handler holds `JobState`.
    pub(crate) shutdown: Arc<AtomicBool>,
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
    /// Recent successful `/api/join-check` plans as `(plan_id, source
    /// bytes)`, so a join started from that check knows its expected size.
    pub(crate) join_plan_bytes: Vec<(String, u64)>,
    /// R4-B15: the `--key-source` spec this workstation runs with, handed
    /// to every CLI child it spawns so their audit appends use the same
    /// key (`None` = ambient key configuration only).
    pub(crate) key_source: Option<String>,
    /// R4-B15: `<key id>:<fingerprint>` (or `none`) of the audit key this
    /// workstation resolved at startup; children compare their own key
    /// against it and warn on a mismatch.
    pub(crate) audit_key_tag: Option<String>,
    /// R4-B2: outstanding one-time launch codes `(code, expires)`. A code
    /// opened as `/?launch=<code>` is consumed and plants the auth cookie,
    /// so the token itself never appears on a browser command line.
    pub(crate) launch_codes: Vec<(String, std::time::Instant)>,
    /// R5-S7: connection slots — the unauthenticated header phase and the
    /// authenticated request phase are counted separately (shared with the
    /// accept loop and the connection threads; atomics, no lock).
    pub(crate) conn_limits: Arc<super::http::ConnLimits>,
    /// R5-S6: an E01 analysis stopped because the existing raw image could
    /// not be confirmed as this E01's complete import. Nothing was deleted;
    /// `/api/reimport-raw` discards it and re-runs the same analysis.
    pub(crate) raw_reimport: Option<RawReimport>,
    /// R11-S5: `reports/case-report.html` is known to be out of date — a
    /// failed finalize could not re-render it after taking its sign-off
    /// back, so the report on disk still names that unfinished sign-off.
    /// Cleared when a make-report the workstation runs succeeds.
    pub(crate) report_stale: bool,
    /// R11-S6: what the last failed finalize left that must not be read
    /// as final (sign-off not restored, report not re-rendered, abandon
    /// record not written) — `None` when it left nothing. Kept in memory
    /// so /api/status shows it after the finalize response is gone;
    /// cleared by a finalize that completes.
    pub(crate) finalize_incomplete: Option<String>,
    /// R12-S2: what recovering an interrupted finalize's journal
    /// (`review/finalize.inprogress.json`) did or could not do, at server
    /// start, open-case or the next finalize — `None` when nothing was
    /// recovered. Cleared by a finalize that completes or another case.
    pub(crate) finalize_recovered: Option<String>,
    /// R13-S7: the case whose interrupted finalize the background start-up
    /// recovery is cleaning up right now (it holds `busy`) — status
    /// `finalize_journal.state: "recovering"`.
    pub(crate) finalize_recovering: Option<PathBuf>,
}

/// R5-S6: the pending "raw 불일치 — 재임포트 필요" decision.
#[derive(Clone)]
pub(crate) struct RawReimport {
    pub(crate) raw_path: PathBuf,
    pub(crate) reason: String,
    pub(crate) job: PipelineJob,
}

/// A background case job (UX-06) run under the busy lease.
#[derive(Clone, Debug)]
pub(crate) struct CaseJob {
    /// Stable machine key: `recover-deleted` | `carve` | `audio-measure`.
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

/// One clip export (UX-09) or lossless join (`kind == "join"`). Identical
/// requests share one job (`key`). Joins share the clip list, lock, cap and
/// every protection (status, cancel, shutdown / open-case refusal,
/// finalize and case-job waits).
#[derive(Clone, Debug)]
pub(crate) struct ClipJob {
    pub(crate) id: String,
    /// `export-clip` | `join`.
    pub(crate) kind: &'static str,
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
    /// `{stage, done, total}` file the CLI writes (join only).
    pub(crate) progress_path: Option<PathBuf>,
    /// Bytes the finished output is expected to have (join: the summed
    /// source size, known from the check) — `None` when unknown.
    pub(crate) expected_bytes: Option<u64>,
    /// Join-only details (`None` for clip exports).
    pub(crate) join: Option<JoinJobInfo>,
}

/// What the server needs to drive, clean up and report one join job.
#[derive(Clone, Debug)]
pub(crate) struct JoinJobInfo {
    /// `--job-tag`: names the `.join-<tag>.*` temp files.
    pub(crate) tag: String,
    pub(crate) plan_id: String,
    pub(crate) ids: Vec<String>,
    pub(crate) temps: crate::join::TempPaths,
    /// Case-relative sidecar manifest (set when done).
    pub(crate) manifest_rel: Option<String>,
    pub(crate) duration_check_ok: Option<bool>,
    /// The manifest's `duration_check` `{expected, observed, ok}`.
    pub(crate) duration_check: Option<serde_json::Value>,
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
    pub(crate) fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// True once `/api/shutdown` was accepted. Every job-start path checks
    /// it under the state lock (S7): a request already inside a handler
    /// when the accept loop stopped must not start work the exiting
    /// process would then cut off.
    pub(crate) fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Takes `busy` in an already-held lock (the caller may update more
    /// state in the same critical section). Refuses while another job
    /// holds it or once shutdown was requested; returns the new epoch.
    pub(crate) fn take_busy(&mut self) -> Result<u64, String> {
        if let Some(refusal) = shutdown_refusal(self) {
            return Err(refusal);
        }
        if self.busy {
            return Err(BUSY_ERROR.to_string());
        }
        self.busy = true;
        self.busy_epoch = self.busy_epoch.wrapping_add(1);
        // UX-07: a cancel aimed at a previous job must not kill this one —
        // every run starts with a fresh cancel flag.
        self.cancel_requested = false;
        Ok(self.busy_epoch)
    }

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
            busy_epoch: 0,
            write_ops: 0,
            write_cancels: Vec::new(),
            next_write_op_id: 0,
            sealing: false,
            cancel_requested: false,
            byte_progress: None,
            rehash_progress: None,
            token: None,
            shutdown: Arc::new(AtomicBool::new(false)),
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
            join_plan_bytes: Vec::new(),
            key_source: None,
            audit_key_tag: None,
            launch_codes: Vec::new(),
            conn_limits: Arc::new(super::http::ConnLimits::default()),
            raw_reimport: None,
            report_stale: false,
            finalize_incomplete: None,
            finalize_recovered: None,
            finalize_recovering: None,
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
    epoch: u64,
}

/// Standard refusal while another job holds `busy`.
pub(crate) const BUSY_ERROR: &str = "분석 작업이 진행 중입니다 — 완료 후 다시 시도하십시오.";

/// Refusal for every job start once shutdown was requested (S7).
pub(crate) const SHUTDOWN_ERROR: &str =
    "워크스테이션이 종료 중입니다 — 새 작업을 시작할 수 없습니다.";

/// `Some(refusal)` once shutdown was requested (S7).
pub(crate) fn shutdown_refusal(guard: &JobState) -> Option<String> {
    guard
        .shutdown_requested()
        .then(|| SHUTDOWN_ERROR.to_string())
}

/// Atomically takes `busy` or returns the standard in-progress error.
pub(crate) fn try_acquire_busy(state: &SharedState) -> Result<BusyGuard<'_>, String> {
    let epoch = state_lock(state).take_busy()?;
    Ok(BusyGuard { state, epoch })
}

/// Clears `busy` only while it is still the acquisition `epoch` made (P2).
fn release_busy_if_owned(guard: &mut JobState, epoch: u64) -> bool {
    if guard.busy && guard.busy_epoch == epoch {
        guard.busy = false;
        true
    } else {
        false
    }
}

/// Message recorded when a job thread unwinds while holding `busy`.
pub(crate) const ABANDONED_JOB_ERROR: &str = "작업 스레드가 비정상 종료되었습니다 (내부 오류) — 작업이 완료되지 않았습니다. 다시 시도하십시오.";

/// A job thread unwound while it held `busy`: never leave the UI showing a
/// running job that no longer exists, and never fail silently (P2).
fn record_abandoned_job(guard: &mut JobState) {
    if matches!(guard.phase, "running" | "finalizing") {
        guard.phase = "error";
    }
    if let Some(job) = guard.case_job.as_mut().filter(|job| job.state == "running") {
        job.state = "failed";
        job.error = Some(ABANDONED_JOB_ERROR.to_string());
        job.finished_unix = crate::util::now_unix().ok();
    }
    guard.byte_progress = None;
    guard.enrichment = None;
    guard
        .error
        .get_or_insert_with(|| ABANDONED_JOB_ERROR.to_string());
    guard.logs.push(format!("오류: {ABANDONED_JOB_ERROR}"));
}

/// Shared drop path of [`BusyGuard`] and [`BusyLease`].
fn drop_busy(state: &SharedState, epoch: u64) {
    {
        let mut guard = state_lock(state);
        if release_busy_if_owned(&mut guard, epoch) && std::thread::panicking() {
            record_abandoned_job(&mut guard);
        }
    }
    after_busy_release(state);
}

/// R4-B4: whenever `busy` is released, marks that were queued while a job
/// held it are applied (outside the state lock; skipped while unwinding).
fn after_busy_release(state: &SharedState) {
    if !std::thread::panicking() {
        super::jobs::drain_after_busy_release(state);
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        drop_busy(self.state, self.epoch);
    }
}

/// Owned busy lease for a job that outlives its HTTP handler (UX-06, and
/// the main analysis pipeline — P2): the worker thread holds it and
/// releases `busy` in the same lock as its final state update, so no
/// window exists where another job could take `busy` and then have it
/// cleared under it. Dropping an unreleased lease (a panicking worker)
/// still frees `busy` and records the failure; a lease whose job already
/// cleared `busy` itself (and which another job may have re-taken since)
/// drops as a no-op.
pub(crate) struct BusyLease {
    state: Option<SharedState>,
    epoch: u64,
}

/// Atomically takes `busy` for a background job, like [`try_acquire_busy`]
/// (production code takes it with [`lease_busy_locked`] inside its own
/// critical section, R4-B17).
#[cfg(test)]
pub(crate) fn try_lease_busy(state: &SharedState) -> Result<BusyLease, String> {
    let epoch = state_lock(state).take_busy()?;
    Ok(BusyLease {
        state: Some(Arc::clone(state)),
        epoch,
    })
}

/// [`try_lease_busy`] inside a lock the caller already holds (`guard` must
/// be the lock of `state`), so a job's initial state is published in the
/// same critical section that takes `busy`.
pub(crate) fn lease_busy_locked(
    guard: &mut JobState,
    state: &SharedState,
) -> Result<BusyLease, String> {
    let epoch = guard.take_busy()?;
    Ok(BusyLease {
        state: Some(Arc::clone(state)),
        epoch,
    })
}

impl BusyLease {
    /// The `busy_epoch` this lease took.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Clears `busy` after applying `finish` under the same state lock.
    pub(crate) fn release_with(mut self, finish: impl FnOnce(&mut JobState)) {
        if let Some(state) = self.state.take() {
            {
                let mut guard = state_lock(&state);
                finish(&mut guard);
                release_busy_if_owned(&mut guard, self.epoch);
            }
            after_busy_release(&state);
        }
    }
}

impl Drop for BusyLease {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            drop_busy(&state, self.epoch);
        }
    }
}

/// Registration of one case write (S1: export-selected — possibly long —
/// and capture-frame) in [`JobState::write_ops`]; dropping it (also on a
/// panic) deregisters. R3: carries the op's cancel flag.
pub(crate) struct WriteOpLease {
    state: SharedState,
    id: u64,
    cancel: Arc<AtomicBool>,
}

impl WriteOpLease {
    /// Whether `/api/cancel` asked this write op to stop (checked between
    /// items; a file copy in progress is never torn).
    #[cfg(test)]
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// The flag itself, for worker threads.
    pub(crate) fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }
}

/// R3: asks every write op in flight to stop; returns how many were asked.
pub(crate) fn cancel_write_ops(guard: &mut JobState) -> usize {
    for (_, flag) in &guard.write_cancels {
        flag.store(true, Ordering::SeqCst);
    }
    guard.write_cancels.len()
}

/// Registers a write op against the bound case and returns that case.
/// Refused once shutdown was requested or while finalize seals the case;
/// `busy` alone does not block (examiners capture frames during phase 2).
pub(crate) fn try_begin_write_op(state: &SharedState) -> Result<(WriteOpLease, PathBuf), String> {
    let mut guard = state_lock(state);
    if let Some(refusal) = shutdown_refusal(&guard) {
        return Err(refusal);
    }
    if guard.sealing {
        return Err(
            "최종화(봉인) 중에는 케이스에 쓸 수 없습니다 — 끝난 뒤 다시 시도하십시오.".to_string(),
        );
    }
    let Some(case_dir) = guard.case_dir.clone() else {
        return Err("먼저 케이스를 열거나 분석을 실행하십시오.".to_string());
    };
    guard.write_ops += 1;
    guard.next_write_op_id = guard.next_write_op_id.wrapping_add(1);
    let id = guard.next_write_op_id;
    let cancel = Arc::new(AtomicBool::new(false));
    guard.write_cancels.push((id, Arc::clone(&cancel)));
    Ok((
        WriteOpLease {
            state: Arc::clone(state),
            id,
            cancel,
        },
        case_dir,
    ))
}

impl Drop for WriteOpLease {
    fn drop(&mut self) {
        let mut guard = state_lock(&self.state);
        guard.write_ops = guard.write_ops.saturating_sub(1);
        let id = self.id;
        guard.write_cancels.retain(|(entry, _)| *entry != id);
    }
}

/// Refusal while case writes beside `busy` run (S1/S4): clip exports,
/// joins and write ops. Used by open-case and a new analysis start.
pub(crate) fn side_work_refusal(guard: &JobState, what: &str) -> Option<String> {
    let clips = guard
        .clip_jobs
        .iter()
        .filter(|job| job.state == "running")
        .count();
    if clips > 0 {
        return Some(format!(
            "내보내기·이어붙이기 작업 {clips}건이 진행 중입니다 — 끝나거나 중단한 뒤 {what}."
        ));
    }
    if guard.write_ops > 0 {
        return Some(format!(
            "선택 내보내기·프레임 캡처가 진행 중입니다 — 끝난 뒤 {what}."
        ));
    }
    None
}

/// Finalize's seal flag (S1): set in the same lock as the "no write op,
/// no clip job" check, cleared on drop.
pub(crate) struct SealGuard {
    state: SharedState,
}

/// Starts sealing when no write op and no clip job runs; the caller holds
/// `busy` already.
pub(crate) fn try_begin_seal(state: &SharedState) -> Result<SealGuard, String> {
    let mut guard = state_lock(state);
    if let Some(refusal) = side_work_refusal(&guard, "다시 최종화하십시오") {
        return Err(format!("최종화 차단: {refusal}"));
    }
    guard.sealing = true;
    Ok(SealGuard {
        state: Arc::clone(state),
    })
}

impl Drop for SealGuard {
    fn drop(&mut self) {
        state_lock(&self.state).sealing = false;
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

/// Lock waits / holds at or above this are reported when
/// `FRAMETRACE_DEBUG_LOCKS=1` (diagnostics for UI stalls; silent by default).
const SLOW_LOCK: Duration = Duration::from_millis(200);

fn debug_locks() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FRAMETRACE_DEBUG_LOCKS").as_deref() == Ok("1"))
}

/// Guard returned by [`state_lock`]. Derefs to `JobState`; with
/// `FRAMETRACE_DEBUG_LOCKS=1` it reports (stderr) the caller of any lock
/// held for [`SLOW_LOCK`] or longer — every endpoint and the job threads
/// share this one mutex, so a long hold freezes the whole UI.
pub(crate) struct StateGuard<'a> {
    guard: MutexGuard<'a, JobState>,
    since: Option<(std::time::Instant, &'static std::panic::Location<'static>)>,
}

impl std::ops::Deref for StateGuard<'_> {
    type Target = JobState;
    fn deref(&self) -> &JobState {
        &self.guard
    }
}

impl std::ops::DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut JobState {
        &mut self.guard
    }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        if let Some((since, caller)) = self.since {
            let held = since.elapsed();
            if held >= SLOW_LOCK {
                eprintln!("[locks] state lock held {held:?} at {caller}");
            }
        }
    }
}

#[track_caller]
pub(crate) fn state_lock(state: &SharedState) -> StateGuard<'_> {
    // Read outside the closure: `#[track_caller]` does not reach into
    // closures, which made every report name this line instead of the
    // caller.
    let caller = std::panic::Location::caller();
    let debug = debug_locks().then(|| (std::time::Instant::now(), caller));
    let guard = match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let since = debug.map(|(asked, caller)| {
        let waited = asked.elapsed();
        if waited >= SLOW_LOCK {
            eprintln!("[locks] state lock waited {waited:?} at {caller}");
        }
        (std::time::Instant::now(), caller)
    });
    StateGuard { guard, since }
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
