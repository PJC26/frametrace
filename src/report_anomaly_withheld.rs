//! R18 I2 / I5 (P9): the anomaly lane's OWN withholding verdict — the
//! rules the case report's anomaly section applies to the lane rows
//! before the sign-off head is consulted:
//!
//! - `unusable-after`: an entry this PC cannot use after the newest usable
//!   `anomaly-scan-run` marker (anywhere, when there is none) — it may be a
//!   newer run's marker (R11-I2);
//! - `unusable-in-run` (R18 I5): an entry this PC cannot use INSIDE the
//!   newest completed run (after the previous usable marker, before its
//!   own, and after the run's first entry or stamped with its
//!   `scanned_unix`) — that run's candidates may be missing from what is
//!   shown;
//! - `repaired-after`: a usable `lane-repair` marker after it (R13 I2);
//! - `newer-unmarked-run`: a run without a completion marker that is newer
//!   than the newest completed run by position, span or time (R13 P16,
//!   R14 P20, R15 I4), or — in a lane written by a marker-writing scanner —
//!   no completed run at all (R14 I4).
//!
//! ONE function, [`anomaly_lane_withheld`], computes it from the very rows
//! every reader receives (`LaneRows::values_with_trust`, each with its
//! `trust` code): `/api/records-meta` and the review bundle export it as
//! `logChains.anomaly.withheld`, the report's `auditChains.anomaly.withheld`
//! carries it to the report script, which also computes the same rules
//! itself and withholds when either says so (fail closed). The reason
//! sentences are the report's, word for word.
//!
//! R19: interleaved runs are grouped by their marker's `scanned_unix`
//! (I1, [`merge_interleaved`]); every reader computes the verdict from the
//! same lane read as the rows it sends ([`anomaly_lane_read_withheld_json`],
//! I2) — this module never reads the lane itself.
//!
//! R20 I2: a finding row carries its scan's `scan_id` (the marker's); a
//! row with a string `scan_id` belongs to the marker with that `scan_id`
//! only, and unmarked rows group by it ([`run_key`]). Rows without one
//! (older scanners) keep the `scanned_unix` rules.

/// One reason the anomaly lane withholds its current result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnomalyWithheld {
    /// Machine codes, in the report's order: `unusable-after`,
    /// `unusable-in-run`, `repaired-after`, `newer-unmarked-run`.
    pub codes: Vec<&'static str>,
    /// The report's Korean sentences, one per code.
    pub reasons: Vec<String>,
}

impl AnomalyWithheld {
    /// `{"reason_code": <first code>, "codes": [...], "reason": "<sentences
    /// joined by "; ">"}` — the `logChains.anomaly.withheld` object.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "reason_code": self.codes.first(),
            "codes": self.codes,
            "reason": self.reasons.join("; "),
        })
    }
}

/// The `unusable-after` sentence (the report's text).
pub const ANOMALY_UNUSABLE_AFTER_REASON: &str = "가장 최근 완료 실행의 완료 표식 뒤에 이 PC에서 인증할 수 없는 항목이 있어(더 새 실행의 완료 표식일 수 있음) 그 결과를 알 수 없습니다 (아래 \"인증 안 된 감사 항목\" 참조)";

/// The `repaired-after` sentence (the report's text).
pub const ANOMALY_REPAIRED_AFTER_REASON: &str = "마지막 완료 실행 뒤에 레인 수리(lane-repair) 표식이 있어 — 잘린 꼬리에 더 새 실행이 있었을 수 있어 — 최신 실행을 알 수 없습니다 (이상 징후 검사를 다시 실행하십시오)";

/// R18 I5: the start of the `unusable-in-run` sentence.
pub const ANOMALY_UNUSABLE_IN_RUN_NOTE: &str =
    "가장 최근 완료 실행의 기록 범위 안에 이 PC에서 인증할 수 없는 항목";

fn unusable_in_run_reason(count: usize) -> String {
    format!(
        "{ANOMALY_UNUSABLE_IN_RUN_NOTE} {count}개 — 그 실행의 후보가 빠졌을 수 있어 그 결과를 알 수 없습니다 (아래 \"인증 안 된 감사 항목\" 참조)"
    )
}

fn newer_run_reason(count: usize, completed: bool) -> String {
    if completed {
        format!(
            "표식 없는 더 새 실행 있음 — 마지막 완료 실행 뒤에 있거나 그 뒤에 시작한 완료 표식 없는 실행 {count}회: 완료 표식이 지워졌거나 실행이 중단됐을 수 있습니다 (이상 징후 검사를 다시 실행하십시오)"
        )
    } else {
        format!(
            "표식 없는 더 새 실행 있음 — 완료된 실행이 없고 완료 표식 없는 실행 {count}회만 있습니다: 완료 표식이 지워졌거나 실행이 중단됐을 수 있어 결과가 아닙니다 (이상 징후 검사를 다시 실행하십시오)"
        )
    }
}

/// The report script's `usableEntry`: no `trust` (an unclassified caller),
/// `auth` or `sc`.
fn usable(item: &serde_json::Value) -> bool {
    match item.get("trust") {
        None => true,
        Some(trust) => matches!(trust.as_str(), Some("auth" | "sc")),
    }
}

fn event(item: &serde_json::Value) -> Option<&str> {
    item.get("event").and_then(|v| v.as_str())
}

/// JavaScript `a === b` for two (possibly missing) JSON field values of
/// two different entries (objects / arrays are never `===`).
fn js_strict_eq(a: Option<&serde_json::Value>, b: Option<&serde_json::Value>) -> bool {
    use serde_json::Value;
    match (a, b) {
        (None, None) => true,
        (Some(Value::Null), Some(Value::Null)) => true,
        (Some(Value::Bool(a)), Some(Value::Bool(b))) => a == b,
        (Some(Value::Number(a)), Some(Value::Number(b))) => a.as_f64() == b.as_f64(),
        (Some(Value::String(a)), Some(Value::String(b))) => a == b,
        _ => false,
    }
}

/// A finite JSON number (JavaScript `Number.isFinite`).
fn finite(value: Option<&serde_json::Value>) -> Option<f64> {
    value.and_then(|v| v.as_f64())
}

/// R20 I2: an entry's string `scan_id` (the report script's
/// `typeof item.scan_id === "string"`).
fn scan_id(item: &serde_json::Value) -> Option<&str> {
    item.get("scan_id").and_then(|v| v.as_str())
}

/// R20 I2: the entry at `entry` belongs to the run whose marker is
/// `marker` — by `scan_id` when the entry has one (only a marker with the
/// same `scan_id`), else (an older scanner's row) by `scanned_unix`
/// (JavaScript `===`). The report script's `sameAnomalyRun`.
fn same_run(entry: &serde_json::Value, marker: &serde_json::Value) -> bool {
    match scan_id(entry) {
        Some(id) => scan_id(marker) == Some(id),
        None => js_strict_eq(entry.get("scanned_unix"), marker.get("scanned_unix")),
    }
}

/// R20 I2: how unmarked entries group (the report script's `byTime` key):
/// by `scan_id` when present, else by finite `scanned_unix` (or none).
#[derive(Debug, Clone, PartialEq)]
enum RunKey {
    Id(String),
    Time(Option<f64>),
}

fn run_key(item: &serde_json::Value) -> RunKey {
    match scan_id(item) {
        Some(id) => RunKey::Id(id.to_string()),
        None => RunKey::Time(finite(item.get("scanned_unix"))),
    }
}

/// One run of the report's grouping (`anomalyRuns`).
struct Run {
    /// Lane position of its `anomaly-scan-run` marker (`None`: unmarked).
    marker: Option<usize>,
    /// `ts` when it is a finite number.
    ts: Option<f64>,
    /// R20 I2: the run's `scan_id` — the marker's (string) for a completed
    /// run, the entries' for an unmarked run grouped by it.
    id: Option<String>,
    /// Lane positions of its entries.
    entries: Vec<usize>,
}

impl Run {
    fn first(&self) -> f64 {
        self.entries
            .iter()
            .chain(self.marker.iter())
            .map(|at| *at as f64)
            .fold(f64::INFINITY, f64::min)
    }

    fn last(&self) -> f64 {
        self.entries
            .iter()
            .chain(self.marker.iter())
            .map(|at| *at as f64)
            .fold(f64::NEG_INFINITY, f64::max)
    }
}

/// The report's `anomalyRuns` grouping over the usable non-repair entries
/// (`anomalyLog`), with lane positions into `all`.
fn group_runs(all: &[serde_json::Value], log: &[usize]) -> Vec<Run> {
    let mut runs = Vec::new();
    let flush_unmarked = |runs: &mut Vec<Run>, items: &[usize]| {
        let mut by_key: Vec<(RunKey, Vec<usize>)> = Vec::new();
        for &at in items {
            let key = run_key(&all[at]);
            match by_key.iter_mut().find(|(k, _)| *k == key) {
                Some((_, entries)) => entries.push(at),
                None => by_key.push((key, vec![at])),
            }
        }
        for (key, entries) in by_key {
            // R20 I2: a `scan_id` group's time is its first entry's.
            let (id, ts) = match key {
                RunKey::Id(id) => (Some(id), finite(all[entries[0]].get("scanned_unix"))),
                RunKey::Time(ts) => (None, ts),
            };
            runs.push(Run {
                marker: None,
                ts,
                id,
                entries,
            });
        }
    };
    let mut pending: Vec<usize> = Vec::new();
    for &at in log {
        let item = &all[at];
        if event(item) == Some("anomaly-scan-run") {
            let (same, other): (Vec<usize>, Vec<usize>) = pending
                .iter()
                .partition(|entry| same_run(&all[**entry], item));
            flush_unmarked(&mut runs, &other);
            runs.push(Run {
                marker: Some(at),
                ts: finite(item.get("scanned_unix")),
                id: scan_id(item).map(str::to_string),
                entries: same,
            });
            pending.clear();
        } else {
            pending.push(at);
        }
    }
    flush_unmarked(&mut runs, &pending);
    merge_interleaved(&mut runs);
    runs
}

/// R19 I1: interleaved runs (A's finding @100, B's finding @200, A's
/// marker @100, B's marker @200) — the grouping flushes B's finding as an
/// unmarked run BEFORE A's run, and B's completed run read "후보 0건".
/// An unmarked run whose (finite) `scanned_unix` equals that of a LATER
/// completed run belongs to it: its entries join that run (the nearest
/// such one, in lane order, ahead of the run's own entries). The report
/// script does the same (`anomalyRuns`).
///
/// R20 I2: an unmarked run grouped by `scan_id` joins only the nearest
/// later completed run whose marker has that `scan_id` (never one that
/// merely shares the second); an unmarked run without one keeps the time
/// rule.
fn merge_interleaved(runs: &mut Vec<Run>) {
    #[derive(PartialEq, Eq, Hash)]
    enum Key {
        Time(u64),
        Id(String),
    }
    // `+0.0` and `-0.0` are one key (JavaScript `Map`, SameValueZero).
    let time = |ts: f64| Key::Time(if ts == 0.0 { 0u64 } else { ts.to_bits() });
    // Walking back: the nearest later completed run per time / `scan_id`,
    // and the chunks (latest first) each completed run receives.
    let mut nearest: std::collections::HashMap<Key, usize> = Default::default();
    let mut chunks: Vec<Vec<Vec<usize>>> = runs.iter().map(|_| Vec::new()).collect();
    let mut keep = vec![true; runs.len()];
    for index in (0..runs.len()).rev() {
        if runs[index].marker.is_some() {
            if let Some(ts) = runs[index].ts {
                nearest.insert(time(ts), index);
            }
            if let Some(id) = &runs[index].id {
                nearest.insert(Key::Id(id.clone()), index);
            }
            continue;
        }
        let key = match (&runs[index].id, runs[index].ts) {
            (Some(id), _) => Key::Id(id.clone()),
            (None, Some(ts)) => time(ts),
            (None, None) => continue,
        };
        if let Some(&target) = nearest.get(&key) {
            chunks[target].push(std::mem::take(&mut runs[index].entries));
            keep[index] = false;
        }
    }
    for (run, chunks) in runs.iter_mut().zip(chunks) {
        if chunks.is_empty() {
            continue;
        }
        let mut entries: Vec<usize> = chunks.into_iter().rev().flatten().collect();
        entries.append(&mut run.entries);
        run.entries = entries;
    }
    let mut keep = keep.into_iter();
    runs.retain(|_| keep.next().unwrap_or(true));
}

/// R18 I2 / I5 (P9): the anomaly lane's own withholding verdict from its
/// rows as every reader receives them (`values_with_trust`, lane order) —
/// `None` when the lane itself does not withhold (the sign-off head is a
/// separate check, `logChains.anomaly.head`).
pub fn anomaly_lane_withheld(all: &[serde_json::Value]) -> Option<AnomalyWithheld> {
    let is_marker = |item: &serde_json::Value| event(item) == Some("anomaly-scan-run");
    let is_repair = |item: &serde_json::Value| event(item) == Some("lane-repair") && usable(item);
    let usable_markers: Vec<usize> = (0..all.len())
        .filter(|at| is_marker(&all[*at]) && usable(&all[*at]))
        .collect();
    let last_marker = usable_markers.last().copied();
    let after = |at: usize| last_marker.is_none_or(|marker| at > marker);
    let mut out = AnomalyWithheld {
        codes: Vec::new(),
        reasons: Vec::new(),
    };
    if (0..all.len()).any(|at| after(at) && !usable(&all[at])) {
        out.codes.push("unusable-after");
        out.reasons.push(ANOMALY_UNUSABLE_AFTER_REASON.to_string());
    }
    let log: Vec<usize> = (0..all.len())
        .filter(|at| usable(&all[*at]) && !is_repair(&all[*at]))
        .collect();
    let runs = group_runs(all, &log);
    let latest = runs.iter().rposition(|run| run.marker.is_some());
    // R18 I5: inside the newest completed run — between the previous usable
    // marker and its own, and after its first entry or stamped with its
    // `scanned_unix` (a stripped HMAC keeps the content; an older line
    // appended before the run started is not the run's).
    if let (Some(marker), Some(latest_at)) = (last_marker, latest) {
        let start = usable_markers
            .len()
            .checked_sub(2)
            .map_or(0, |prev| usable_markers[prev] + 1);
        let first = runs[latest_at].first();
        let stamp = all[marker].get("scanned_unix");
        let inside = (start..marker)
            .filter(|at| {
                !usable(&all[*at])
                    && (*at as f64 > first || js_strict_eq(all[*at].get("scanned_unix"), stamp))
            })
            .count();
        if inside > 0 {
            out.codes.push("unusable-in-run");
            out.reasons.push(unusable_in_run_reason(inside));
        }
    }
    if (0..all.len()).any(|at| after(at) && is_repair(&all[at])) {
        out.codes.push("repaired-after");
        out.reasons.push(ANOMALY_REPAIRED_AFTER_REASON.to_string());
    }
    let marked_lane = log
        .iter()
        .any(|at| all[*at].get("schema_version").is_some());
    let newer = match latest {
        Some(latest_at) => {
            let latest = &runs[latest_at];
            runs.iter()
                .enumerate()
                .filter(|(index, run)| {
                    run.marker.is_none()
                        && (*index > latest_at
                            || run.first() > latest.first()
                            || matches!((run.ts, latest.ts), (Some(ts), Some(latest_ts))
                                if ts > latest_ts && run.last() > latest.first()))
                })
                .count()
        }
        None if marked_lane => runs.len(),
        None => 0,
    };
    if newer > 0 {
        out.codes.push("newer-unmarked-run");
        out.reasons.push(newer_run_reason(newer, latest.is_some()));
    }
    (!out.codes.is_empty()).then_some(out)
}

/// [`anomaly_lane_withheld`] as JSON: the object, or `null`.
pub fn anomaly_lane_withheld_json(all: &[serde_json::Value]) -> serde_json::Value {
    anomaly_lane_withheld(all).map_or(serde_json::Value::Null, |w| w.to_json())
}

/// The `withheld` object of an anomaly lane that could not be read (`err`:
/// the read error) — withheld, nothing can be said about its result.
pub fn anomaly_unreadable_withheld_json(err: &str) -> serde_json::Value {
    serde_json::json!({
        "reason_code": "unreadable",
        "codes": ["unreadable"],
        "reason": format!("이상 징후 레인을 읽지 못해 현재 결과를 알 수 없습니다: {err}"),
    })
}

/// R19 I2: the `withheld` object from the SAME read of the anomaly lane
/// the caller's rows come from (`lane`: what `audit::read_lane_rows`
/// returned) — never a second read, which a scan in between could make
/// disagree with the rows sent. An unreadable lane is `unreadable`.
pub fn anomaly_lane_read_withheld_json(
    lane: &Result<crate::audit::LaneRows, String>,
) -> serde_json::Value {
    match lane {
        Ok(lane) => anomaly_lane_withheld_json(&lane.values_with_trust()),
        Err(err) => anomaly_unreadable_withheld_json(err),
    }
}

/// R18 I6: the review bundle's `logChains` after redaction. A redacted
/// text that is no longer one JSON object with an `anomaly.head` object
/// (the redactor rewrote it into something unparsable) is NOT dropped
/// silently (R17: the field vanished and so did the viewer's banner): it
/// becomes `{"anomaly":{"head":{"status":"unknown","reason_code":
/// "bundle-unreadable",…},"withheld":{…"bundle-unreadable"…}}}`.
pub fn bundle_log_chains_json(redacted: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(redacted.trim())
        .ok()
        .filter(|value| value["anomaly"]["head"].is_object());
    match parsed {
        Some(value) => value.to_string(),
        None => {
            let why = "번들의 이상 징후 머리 판정을 읽을 수 없음(리댁션 뒤 형식이 깨짐) — 현재 결과를 확인할 수 없음 (make-review를 다시 실행하십시오)";
            serde_json::json!({
                "anomaly": {
                    "head": {
                        "status": "unknown",
                        "reason_code": "bundle-unreadable",
                        "withhold_reason": why,
                        "detail": "the bundle's anomaly head verdict could not be read after redaction",
                    },
                    "withheld": {
                        "reason_code": "bundle-unreadable",
                        "codes": ["bundle-unreadable"],
                        "reason": why,
                    },
                },
            })
            .to_string()
        }
    }
}
