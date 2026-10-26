//! R14 P20 / I3, R15 P22: the `hash-verify-head` anchor. Every completed
//! anomaly / re-hash scan appends, at the END of the scan (after the
//! anomaly lane's `anomaly-scan-run` marker), one keyed (HMAC when a key is
//! configured) entry to the SIGN-OFF lane naming, for EVERY result lane —
//! the re-verification lane (`hash-verify.jsonl`) and the anomaly lane
//! (`anomaly-log.jsonl`) — its entry count and last `entry_sha256`.
//! Deleting the newest run's rows AND its marker (no key: the shorter lane
//! still verifies) then leaves the lane shorter than its head —
//! "꼬리 잘림 — 결과 없음" — instead of the previous run reading green.
//!
//! R15 P22: when no usable head exists, a sign-off lane holding any line
//! this PC cannot use, a chain failure or a repair marker is `Unknown`
//! (withheld) — the unusable line may be the stripped head, and its
//! content is never read.
//!
//! R16 P26: a head is paired with its own scan — the result run's head
//! (same id; for the anomaly lane a v2 head with the marker's `scan_id`)
//! is the reference, and an older build's (v1) head or another scan's
//! head after it withholds. Anchor comparison (tail cut / rewritten) is
//! judged before any trailing-line rule.
//!
//! R17 P27: anchor candidates are head rows whose own trust is usable,
//! also before a sign-off chain break (a tail cut stays "꼬리 잘림"); a
//! version mix (`mixed-version`) needs a usable marker without `scan_id`
//! — no usable marker is `marker-unusable`; unauthenticated content is
//! never a reason to name a cause.
//!
//! R18 I1: that marker must also lie after the newest v2 head's anchored
//! range with no unusable line after it — an older marker the selection
//! fell back to is `marker-unusable`. I4: only known sign-off event names
//! are quoted in a reason.
//!
//! Kept apart from the finalize pairing code in `report.rs` (track S).

use std::path::Path;

/// The sign-off-lane event a completed scan appends.
pub const HASH_VERIFY_HEAD_EVENT: &str = "hash-verify-head";

/// The field an R14+ `hash-verify-run` marker (and an R15
/// `anomaly-scan-run` marker) carries: its run wrote (or tried to write) a
/// [`HASH_VERIFY_HEAD_EVENT`] — a result run with this field but no usable
/// head naming it has lost its anchor.
pub const HASH_VERIFY_MARKER_HEAD_FIELD: &str = "head_in";

/// The reason word every reader shows for a result lane shorter than (or
/// rewritten from) its recorded head. `/api/verify-sources`
/// `withheld_reason` CONTAINS it.
pub const HASH_VERIFY_HEAD_TRUNCATED: &str = "꼬리 잘림 — 결과 없음";

/// R15 I5: the reason words when every sign-off line this PC cannot use
/// (after the newest usable head, or anywhere when there is none) is
/// signed under ANOTHER PC's key — it may be that PC's (newer) head, not a
/// stripped one. Still no result here; check it on that PC.
pub const HEAD_OTHER_PC_NOTE: &str = "다른 PC 머리일 수 있음";

/// R15 P22: the anomaly lane the head also anchors.
pub const ANOMALY_LOG_REL_PATH: &str = "evidence/logs/anomaly-log.jsonl";

/// R15 P22: the head entry's `schema_version` (2: `lanes` map, written at
/// the end of the scan; 1: R14, re-verification lane only).
pub const HEAD_SCHEMA_VERSION: u64 = 2;

/// A result lane the head anchors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadLane {
    /// `evidence/logs/hash-verify.jsonl` (runs named by `run_id`).
    HashVerify,
    /// `evidence/logs/anomaly-log.jsonl` (runs named by `scan_id`).
    Anomaly,
}

impl HeadLane {
    pub const ALL: [HeadLane; 2] = [HeadLane::HashVerify, HeadLane::Anomaly];

    pub fn rel(self) -> &'static str {
        match self {
            Self::HashVerify => crate::anomaly::HASH_VERIFY_REL_PATH,
            Self::Anomaly => ANOMALY_LOG_REL_PATH,
        }
    }

    /// The head field naming the run whose scan wrote the head.
    fn id_field(self) -> &'static str {
        match self {
            Self::HashVerify => "run_id",
            Self::Anomaly => "scan_id",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::HashVerify => "재검증 기록 레인",
            Self::Anomaly => "이상 징후 레인",
        }
    }

    fn head_word(self) -> &'static str {
        match self {
            Self::HashVerify => "재검증 머리",
            Self::Anomaly => "이상 징후 머리",
        }
    }

    fn rerun(self) -> &'static str {
        match self {
            Self::HashVerify => "원본 재검증을 다시 실행하십시오",
            Self::Anomaly => "이상 징후 검사를 다시 실행하십시오",
        }
    }

    fn english(self) -> &'static str {
        match self {
            Self::HashVerify => "re-verification lane",
            Self::Anomaly => "anomaly lane",
        }
    }
}

/// R15 P22: appends the [`HASH_VERIFY_HEAD_EVENT`] for every result lane
/// as it is now. Call LAST in the scan (after the anomaly lane's run
/// marker), under the case's scan lock. `scan_id`: the scan's id (the
/// anomaly marker's `scan_id`); `run_id`: the re-verification run id when
/// the scan re-hashed (the same id), `None` otherwise.
pub fn record_scan_head(
    case_dir: &Path,
    scan_id: &str,
    run_id: Option<&str>,
    scanned_unix: u64,
) -> Result<(), String> {
    let mut lanes = serde_json::Map::new();
    let mut body = serde_json::json!({
        "schema_version": HEAD_SCHEMA_VERSION,
        "event": HASH_VERIFY_HEAD_EVENT,
        "scan_id": scan_id,
        "run_id": run_id,
        "run_unix": scanned_unix,
    });
    for lane in HeadLane::ALL {
        let path = case_dir.join(lane.rel());
        let head = crate::audit::lane_head(&path)
            .map_err(|err| format!("{} 머리를 읽지 못했습니다: {err}", lane.noun()))?;
        let anchor = match &head {
            Some(head) => serde_json::json!({
                "entries": head.entries,
                "last_entry_sha256": head.last_entry_sha256,
            }),
            None => serde_json::json!({"entries": 0, "last_entry_sha256": null}),
        };
        // R14 readers: the re-verification lane's anchor at the top level.
        if lane == HeadLane::HashVerify
            && let Some(head) = &head
        {
            body["lane"] = serde_json::Value::String(lane.rel().to_string());
            body["entries"] = serde_json::Value::from(head.entries);
            body["last_entry_sha256"] = serde_json::Value::String(head.last_entry_sha256.clone());
        }
        lanes.insert(lane.rel().to_string(), anchor);
    }
    body["lanes"] = serde_json::Value::Object(lanes);
    crate::audit::append_chained_jsonl(
        &case_dir.join(super::SIGNOFF_LOG_REL_PATH),
        &body.to_string(),
    )
    .map_err(|err| {
        format!("결과 레인 머리(hash-verify-head)를 서명 감사 레인에 기록하지 못했습니다: {err}")
    })
}

/// What the newest usable [`HASH_VERIFY_HEAD_EVENT`] says about a result
/// lane now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashVerifyHeadCheck {
    /// No head was ever recorded (a case from before R14, or no sign-off
    /// lane) and the sign-off lane holds nothing that could be a hidden
    /// head; or (anomaly lane) the newest head predates R15 and does not
    /// anchor that lane — nothing to compare.
    Absent,
    /// The lane still holds the anchored entry at the anchored position
    /// (entries appended since are fine).
    Intact {
        entries: u64,
        run_id: Option<String>,
    },
    /// The lane holds fewer entries than the head — its tail was removed.
    Truncated { entries: u64, now: u64 },
    /// The entry at the head's position is a different one, or the lane
    /// file is gone.
    Rewritten { entries: u64, why: String },
    /// The head cannot be relied on: the sign-off lane holds lines this
    /// PC cannot use after it (or anywhere, with no usable head — a newer
    /// head may be among them), a chain failure, a repair after it, or the
    /// head is malformed. No result. `code`: [`Self::reason_code`].
    Unknown { code: &'static str, why: String },
    /// The anchored entry is there, but the lane up to it is not usable
    /// here (NOT VERIFIED, not "changed").
    NotAuthenticated(String),
}

impl HashVerifyHeadCheck {
    /// The `/api/verify-sources` `head_status` word.
    pub fn status(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Intact { .. } => "intact",
            Self::Truncated { .. } => "truncated",
            Self::Rewritten { .. } => "rewritten",
            Self::Unknown { .. } => "unknown",
            Self::NotAuthenticated(_) => "not-authenticated",
        }
    }

    /// R15: why the head is `unknown` (`/api/verify-sources`
    /// `head_reason`): `other-pc` (the unusable sign-off lines are another
    /// PC's — [`HEAD_OTHER_PC_NOTE`]), `unusable-line`, `chain-failed`,
    /// `repaired`, `malformed`, `own-head-missing`, `unreadable`; R16 P26:
    /// `mixed-version` ([`HEAD_MIXED_VERSION_NOTE`]), `scan-mismatch`
    /// ([`HEAD_SCAN_MISMATCH_NOTE`]); R17 I1 (P27): `marker-unusable`
    /// ([`HEAD_MARKER_UNUSABLE_NOTE`] — a v2 head anchors the anomaly lane
    /// but no completion marker there is usable: possibly tampering, not a
    /// version mix); R20 I1: `lane-changed` ([`HEAD_LANE_CHANGED_NOTE`] —
    /// the caller's anomaly rows are shorter than the head, the lane now
    /// is not). `None` for every other state.
    pub fn reason_code(&self) -> Option<&'static str> {
        match self {
            Self::Unknown { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Why there is no current result of the re-verification lane
    /// (Korean), `None` when the head does not withhold it.
    pub fn withhold_reason(&self) -> Option<String> {
        self.withhold_reason_in(HeadLane::HashVerify)
    }

    /// [`Self::withhold_reason`] for `lane`.
    pub fn withhold_reason_in(&self, lane: HeadLane) -> Option<String> {
        let (noun, head, rerun) = (lane.noun(), lane.head_word(), lane.rerun());
        match self {
            Self::Absent | Self::Intact { .. } | Self::NotAuthenticated(_) => None,
            Self::Truncated { entries, now } => Some(format!(
                "{HASH_VERIFY_HEAD_TRUNCATED}: 서명 감사 레인의 {head}(hash-verify-head)는 항목 {entries}개를 기록했으나 {noun}에는 {now}개뿐 — 레인 꼬리(더 새 실행의 기록·완료 표식)가 지워졌습니다 ({rerun})"
            )),
            Self::Rewritten { entries, why } => Some(format!(
                "{HASH_VERIFY_HEAD_TRUNCATED}: {noun}이 서명 감사 레인의 {head}(항목 {entries}개)와 다름 — {why} ({rerun})"
            )),
            Self::Unknown { why, .. } => {
                Some(format!("{head} 확인 불가 — 결과 없음: {why} ({rerun})"))
            }
        }
    }

    /// Why the result is not verified as a whole although not withheld.
    pub fn not_verified_reason(&self) -> Option<String> {
        match self {
            Self::NotAuthenticated(why) => Some(format!("재검증 머리까지의 레인 확인 불가: {why}")),
            _ => None,
        }
    }

    /// The custody statement / `verify-package` sentence (English) for the
    /// re-verification lane.
    pub fn describe(&self) -> String {
        self.describe_in(HeadLane::HashVerify)
    }

    /// [`Self::describe`] for `lane`.
    pub fn describe_in(&self, lane: HeadLane) -> String {
        let name = lane.english();
        match self {
            Self::Absent => format!(
                "no hash-verify-head anchoring the {name} was recorded in the sign-off lane (scans before R14, or before R15 for the anomaly lane); a deleted lane tail cannot be detected"
            ),
            Self::Intact { entries, .. } => format!(
                "the {name} still holds the {entries} entries its newest hash-verify-head (sign-off lane) recorded"
            ),
            Self::Truncated { entries, now } => format!(
                "TAIL CUT: the sign-off lane's hash-verify-head recorded {entries} entries, the {name} now holds {now} / {HASH_VERIFY_HEAD_TRUNCATED}"
            ),
            Self::Rewritten { entries, why } => format!(
                "REWRITTEN: the {name} differs from its hash-verify-head ({entries} entries): {why} / {HASH_VERIFY_HEAD_TRUNCATED}"
            ),
            Self::Unknown { code, why } => {
                format!("the hash-verify-head cannot be relied on ({code}): {why}")
            }
            Self::NotAuthenticated(why) => {
                format!("the {name} up to its hash-verify-head is not verifiable here: {why}")
            }
        }
    }

    /// The report's `auditChains.anomaly.head` object (R15 track I → the
    /// report script): `{"status", "reason_code", "withhold_reason",
    /// "detail"}`.
    pub fn to_json_in(&self, lane: HeadLane) -> serde_json::Value {
        serde_json::json!({
            "status": self.status(),
            "reason_code": self.reason_code(),
            "withhold_reason": self.withhold_reason_in(lane),
            "detail": self.describe_in(lane),
        })
    }
}

/// R15 I5: an unusable line signed under ANOTHER PC's key (not stripped,
/// not superseded, not in a broken lane).
fn other_pc_line(row: &crate::audit::LaneRow) -> bool {
    matches!(row.trust, crate::audit::LineTrust::OtherPc { .. })
        && row.superseded_by.is_none()
        && row.lane_broken_at.is_none()
}

/// R16 I4: an unusable line that is, by its own (unauthenticated) shape,
/// another sign-off event — a JSON object whose `event` names something
/// other than [`HASH_VERIFY_HEAD_EVENT`]. Only the WORDING depends on it
/// (the content is not relied on: the result is withheld either way).
fn other_event_line(row: &crate::audit::LaneRow) -> bool {
    row.value
        .get("event")
        .and_then(|v| v.as_str())
        .is_some_and(|event| event != HASH_VERIFY_HEAD_EVENT)
}

/// R18 I4: the sign-off-lane event names this build writes — the only
/// unauthenticated `event` strings a reason sentence may quote.
pub const KNOWN_SIGNOFF_EVENTS: &[&str] = &[
    "signoff",
    "signoff-abandoned",
    "finalize-begin",
    "finalize-complete",
    "finalize-resolved",
    "lane-repair",
];

/// R18 I4: the "적힌 event" part of the `unusable-line` sentence — known
/// names ([`KNOWN_SIGNOFF_EVENTS`]) quoted, any other name only counted
/// (unauthenticated text is never embedded in a reason).
fn quoted_event_names(events: &std::collections::BTreeSet<&str>) -> String {
    let known: Vec<&str> = events
        .iter()
        .copied()
        .filter(|event| KNOWN_SIGNOFF_EVENTS.contains(event))
        .collect();
    let other = events.len() - known.len();
    match (known.is_empty(), other) {
        (false, 0) => format!("적힌 event: {}", known.join(", ")),
        (false, other) => format!(
            "적힌 event: {} 외 알려지지 않은 이름 {other}개(표시하지 않음)",
            known.join(", ")
        ),
        (true, other) => format!("적힌 event는 알려지지 않은 이름 {other}개(표시하지 않음)"),
    }
}

/// R16 I4: what resolves an `unknown` head on this PC.
pub const HEAD_RESCAN_NOTE: &str = "이 PC에서 다시 스캔하면 새 머리가 기록돼 해소";

/// `Unknown` for `count` unusable sign-off lines (`where_`: Korean
/// position phrase), split by I5: all another PC's → `other-pc`. R16 I4:
/// the `unusable-line` wording says whether every line is, by its shape,
/// another event ([`HEAD_OTHER_EVENT_NOTE`]; R17 I3: neutral — no cause is
/// asserted from unauthenticated content) or some line could be a head
/// (head-shaped or unreadable — "알 수 없음", may be a stripped head).
fn unusable_unknown(
    rows: &[&crate::audit::LaneRow],
    where_: &str,
    lane: HeadLane,
) -> HashVerifyHeadCheck {
    let count = rows.len();
    let head = lane.head_word();
    if rows.iter().all(|row| other_pc_line(row)) {
        let keys: std::collections::BTreeSet<String> = rows
            .iter()
            .filter_map(|row| match &row.trust {
                crate::audit::LineTrust::OtherPc { key_id } => Some(key_id.clone()),
                _ => None,
            })
            .collect();
        // R19 I3: a forgeable id is quoted only when quotable.
        let keys = keys
            .iter()
            .map(|id| crate::audit::key_id_quoted(id))
            .collect::<Vec<_>>()
            .join(", ");
        return HashVerifyHeadCheck::Unknown {
            code: "other-pc",
            why: format!(
                "{HEAD_OTHER_PC_NOTE} — 서명 감사 레인의 {where_} 다른 PC의 키({keys})로 서명된 줄 {count}개: 그 PC가 쓴 더 새 {head}일 수 있어 이 PC에서는 비교할 머리를 알 수 없음 (키 보유 PC에서 확인하거나 이 PC에서 다시 실행; 변조 판정 아님)"
            ),
        };
    }
    if rows.iter().all(|row| other_event_line(row)) {
        let events: std::collections::BTreeSet<&str> = rows
            .iter()
            .filter_map(|row| row.value.get("event").and_then(|v| v.as_str()))
            .collect();
        let events = quoted_event_names(&events);
        return HashVerifyHeadCheck::Unknown {
            code: "unusable-line",
            why: format!(
                "{HEAD_OTHER_EVENT_NOTE} — 서명 감사 레인의 {where_} 이 PC에서 인증할 수 없는 줄 {count}개({events}): 내용이 인증되지 않아 머리인지 아닌지·누가 왜 썼는지 판단하지 않으며 현재 결과를 보류함 ({HEAD_RESCAN_NOTE})"
            ),
        };
    }
    HashVerifyHeadCheck::Unknown {
        code: "unusable-line",
        why: format!(
            "서명 감사 레인의 {where_} 이 PC에서 쓸 수 없는 줄 {count}개 — 머리 모양이거나 내용을 알 수 없는 줄이 있어 더 새 {head}일 수 있음(HMAC이 제거된 머리 포함) — 비교할 머리를 알 수 없음 ({HEAD_RESCAN_NOTE})"
        ),
    }
}

/// R16 I4: the start of the `unusable-line` reason when every such line
/// is, by its shape, another event (not a head). R17 I3 (P27): neutral —
/// the shape is unauthenticated content, so it is never a reason to say
/// "not tampering" (R16: "구조적으로 다른 이벤트 … 변조 판정 아님").
pub const HEAD_OTHER_EVENT_NOTE: &str = "모양상 다른 이벤트이나 내용은 인증 안 됨";

/// R14 I3: the newest usable head against `root`'s re-verification lane
/// (a case folder or a package). `result_marker_wants_head`: the result
/// run's marker says a head was written for it
/// ([`HASH_VERIFY_MARKER_HEAD_FIELD`]); `result_run_id`: that run.
pub fn hash_verify_head_check(
    root: &Path,
    result_run_id: Option<&str>,
    result_marker_wants_head: bool,
) -> HashVerifyHeadCheck {
    lane_head_check(
        root,
        HeadLane::HashVerify,
        result_run_id,
        result_marker_wants_head,
    )
}

/// R15 P22 / I2: the newest usable head against `root`'s anomaly lane. The
/// result run is the anomaly lane's newest usable `anomaly-scan-run`
/// marker (the report's "현재 결과" candidate); its `scan_id` / `head_in`
/// decide whether its own head must be there.
///
/// R20 I1: `lane` is the caller's ONE read of the anomaly lane
/// (`audit::read_lane_rows(root/ANOMALY_LOG_REL_PATH)`, the read its rows
/// and `withheld` verdict come from) — the marker search AND the head's
/// anchor comparison use those rows; nothing here reads the anomaly lane
/// again (R19 read it twice more, so a scan in between could make rows,
/// `withheld` and `head` describe different lanes). Only the sign-off
/// lane is read here.
pub fn anomaly_head_check(
    root: &Path,
    lane: &Result<crate::audit::LaneRows, String>,
) -> HashVerifyHeadCheck {
    let lane = match lane {
        Ok(lane) => lane,
        Err(err) => {
            return HashVerifyHeadCheck::Unknown {
                code: "unreadable",
                why: format!("이상 징후 레인을 읽지 못함: {err}"),
            };
        }
    };
    let marker = lane
        .usable()
        .rev()
        .find(|row| row.value.get("event").and_then(|v| v.as_str()) == Some("anomaly-scan-run"));
    let scan_id = marker
        .and_then(|row| row.value.get("scan_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let wants = marker.is_some_and(|row| {
        row.value
            .get(HASH_VERIFY_MARKER_HEAD_FIELD)
            .is_some_and(|v| !v.is_null())
    });
    // R17 I1 (P27): whether a USABLE marker exists decides between an
    // older build's run (`mixed-version`) and a marker this PC cannot use
    // (`marker-unusable` — possibly stripped). R18 I1: where it sits, and
    // whether a line this PC cannot use follows it, too.
    lane_head_check_for(
        root,
        HeadLane::Anomaly,
        Some(lane),
        ResultRun {
            id: scan_id.as_deref(),
            wants_head: wants,
            usable_marker: marker.is_some(),
            marker_index: marker.map(|row| row.index),
            unusable_after_marker: marker
                .is_some_and(|marker| lane.unusable().any(|row| row.index > marker.index)),
        },
    )
}

/// [`anomaly_head_check`] for a caller that sends no anomaly rows of its
/// own (`verify-package`, tests): reads the anomaly lane ONCE and checks
/// the head against that read.
pub fn read_anomaly_head_check(root: &Path) -> HashVerifyHeadCheck {
    anomaly_head_check(
        root,
        &crate::audit::read_lane_rows(&root.join(ANOMALY_LOG_REL_PATH)),
    )
}

/// R17 I1: what the result lane says about its current result run.
#[derive(Debug, Clone, Copy)]
struct ResultRun<'a> {
    /// The result run's id (`run_id` / the anomaly marker's `scan_id`).
    id: Option<&'a str>,
    /// Its marker says a head was written for it
    /// ([`HASH_VERIFY_MARKER_HEAD_FIELD`]).
    wants_head: bool,
    /// The lane holds a usable completion marker at all (`id` may still be
    /// `None`: an older build's marker without `scan_id`).
    usable_marker: bool,
    /// R18 I1: the lane index of that marker.
    marker_index: Option<usize>,
    /// R18 I1: the result lane holds a line this PC cannot use after it.
    unusable_after_marker: bool,
}

/// R16 P26: a usable head's anchor for `lane` — `Ok(Some((entries,
/// last_entry_sha256)))` (`(0, "")` for a lane empty at the time),
/// `Ok(None)` when the head does not anchor `lane` (an R14 head and the
/// anomaly lane), `Err` when it is malformed.
fn head_anchor(value: &serde_json::Value, lane: HeadLane) -> Result<Option<(u64, String)>, String> {
    // R15: a v2 head names every lane in `lanes`; an R14 head anchors the
    // re-verification lane at the top level only.
    let anchor = match value.get("lanes") {
        Some(lanes) => match lanes.get(lane.rel()) {
            Some(anchor) => anchor,
            None => return Err(format!("lanes에 {}이 없음", lane.rel())),
        },
        None if lane == HeadLane::HashVerify => value,
        None => return Ok(None),
    };
    let entries = anchor.get("entries").and_then(|v| v.as_u64());
    let last = anchor.get("last_entry_sha256").and_then(|v| v.as_str());
    match (entries, last) {
        (Some(0), _) => Ok(Some((0, String::new()))),
        (Some(entries), Some(last)) => Ok(Some((entries, last.to_string()))),
        _ => Err("entries / last_entry_sha256이 없음".to_string()),
    }
}

/// R16 P26: a v2 head (R15+: `lanes`, written at the END of the scan).
fn is_v2_head(value: &serde_json::Value) -> bool {
    value.get("lanes").is_some()
}

/// R16 P26: the start of the `mixed-version` reason — an older build's
/// (v1) head after the result run's own head, or an anomaly result run
/// without `scan_id` in a lane a v2 head anchors.
pub const HEAD_MIXED_VERSION_NOTE: &str = "이전 버전 머리 섞임";

/// R16 P26: the start of the `scan-mismatch` reason — a newer head names
/// another scan than the result run's.
pub const HEAD_SCAN_MISMATCH_NOTE: &str = "다른 스캔의 머리";

/// R20 I1: the start of the `lane-changed` reason — the anomaly rows the
/// caller read are shorter than the head, but the lane as it is now is not
/// (a scan finished between the two reads): unknown, not "꼬리 잘림".
pub const HEAD_LANE_CHANGED_NOTE: &str = "읽는 사이에 레인이 바뀜";

/// R17 I1 (P27): the start of the `marker-unusable` reason — a v2 head
/// anchors the anomaly lane but the lane holds no completion marker this
/// PC can use (its HMAC stripped, or it was deleted): possibly tampering,
/// never worded as a version mix.
pub const HEAD_MARKER_UNUSABLE_NOTE: &str = "완료 표식을 쓸 수 없음";

/// The usable [`HASH_VERIFY_HEAD_EVENT`]s in `root`'s sign-off lane against
/// `root`'s `lane` (P22; R16 P26).
///
/// R16 P26 order: (1) the newest usable head anchoring `lane` is compared
/// FIRST — `Truncated` / `Rewritten` is certain whatever follows in the
/// sign-off lane (R15 let one unusable line after it turn "꼬리 잘림" into
/// `unknown`); (2) the R15 no-head / trailing-line / repair / malformed
/// rules; (3) the head must pair with the result run — the newest usable
/// head naming the result id (for the anomaly lane: a v2 head with the
/// marker's `scan_id`) is the reference, and a v1 head (`mixed-version`)
/// or a head naming another scan (`scan-mismatch`) AFTER it withholds; an
/// anomaly result run without `scan_id` (an older build's marker) while a
/// v2 head exists is `mixed-version` too.
pub fn lane_head_check(
    root: &Path,
    lane: HeadLane,
    result_id: Option<&str>,
    result_marker_wants_head: bool,
) -> HashVerifyHeadCheck {
    lane_head_check_for(
        root,
        lane,
        None,
        ResultRun {
            id: result_id,
            wants_head: result_marker_wants_head,
            usable_marker: result_id.is_some(),
            marker_index: None,
            unusable_after_marker: false,
        },
    )
}

/// `result_lane`: the result lane as the caller already read it (R20 I1 —
/// the anchor comparison then uses those rows); `None` reads it here.
fn lane_head_check_for(
    root: &Path,
    lane: HeadLane,
    result_lane: Option<&crate::audit::LaneRows>,
    result: ResultRun<'_>,
) -> HashVerifyHeadCheck {
    use crate::audit::AnchorStatus;
    let (result_id, result_marker_wants_head) = (result.id, result.wants_head);
    let head_word = lane.head_word();
    let signoff = match crate::audit::read_lane_rows(&root.join(super::SIGNOFF_LOG_REL_PATH)) {
        Ok(rows) => rows,
        Err(err) => {
            return HashVerifyHeadCheck::Unknown {
                code: "unreadable",
                why: format!("서명 감사 레인(signoff-log)을 읽지 못함: {err}"),
            };
        }
    };
    let heads: Vec<&crate::audit::LaneRow> = signoff
        .usable()
        .filter(|row| {
            row.value.get("event").and_then(|v| v.as_str()) == Some(HASH_VERIFY_HEAD_EVENT)
        })
        .collect();
    let id_of = |row: &crate::audit::LaneRow| {
        row.value
            .get(lane.id_field())
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };

    // (1) P26: anchors first — the newest head that anchors `lane` (it
    // recorded the most entries: a lane shorter than an older head is
    // shorter than this one too). R17 I2 (P27): the candidates are the
    // head rows whose OWN trust is usable — also before a chain break
    // (`lane_broken_at`), like `check_lane_anchor`: what such a head says
    // was verified when it was written, so a lane shorter than it lost its
    // tail whatever broke the sign-off chain later (R16 let one
    // chain-breaking line turn "꼬리 잘림" into `own-head-missing`). Rows
    // from the break on are `ChainBroken` — never candidates. Every later
    // step uses fully usable heads only.
    let anchored = signoff
        .rows
        .iter()
        .filter(|row| {
            row.trust.is_usable()
                && row.value.get("event").and_then(|v| v.as_str()) == Some(HASH_VERIFY_HEAD_EVENT)
        })
        .rev()
        .find_map(|row| {
            head_anchor(&row.value, lane)
                .ok()
                .flatten()
                .map(|anchor| (row, anchor))
        });
    let anchor_status = anchored.map(|(row, (entries, last))| -> Result<_, HashVerifyHeadCheck> {
        let status = if entries == 0 {
            // The lane was empty when the head was written: nothing to lose.
            AnchorStatus::Intact
        } else {
            let head = crate::audit::LaneHead {
                entries,
                last_entry_sha256: last,
            };
            match result_lane {
                Some(rows) => match crate::audit::check_lane_anchor_in(rows, &head) {
                    // R20 I1: the caller's rows are SHORTER than the head
                    // — a cut tail, or a scan finished (rows, then head)
                    // between the caller's read and this sign-off read.
                    // Only this tamper verdict is confirmed against the
                    // lane as it is now (append-only: a cut lane stays
                    // short); a lane that grew is `lane-changed` (unknown,
                    // withheld — never "intact" for rows not seen, never
                    // "꼬리 잘림" for a lane that was not cut).
                    status @ (AnchorStatus::Truncated { .. } | AnchorStatus::Missing) => {
                        match crate::audit::check_lane_anchor(&root.join(lane.rel()), &head) {
                            AnchorStatus::Truncated { .. } | AnchorStatus::Missing => status,
                            AnchorStatus::Rewritten => AnchorStatus::Rewritten,
                            _ => {
                                return Err(HashVerifyHeadCheck::Unknown {
                                    code: "lane-changed",
                                    why: format!(
                                        "{HEAD_LANE_CHANGED_NOTE} — 읽은 {}에는 항목 {}개, {head_word}는 {entries}개를 기록함(다른 검사가 그 사이에 끝났을 수 있음) — 다시 확인하십시오",
                                        lane.noun(),
                                        rows.rows.len()
                                    ),
                                });
                            }
                        }
                    }
                    status => status,
                },
                None => crate::audit::check_lane_anchor(&root.join(lane.rel()), &head),
            }
        };
        Ok((row, entries, status))
    });
    let anchor_status = match anchor_status.transpose() {
        Ok(status) => status,
        Err(check) => return check,
    };
    match &anchor_status {
        Some((_, entries, AnchorStatus::Truncated { now })) => {
            return HashVerifyHeadCheck::Truncated {
                entries: *entries,
                now: *now,
            };
        }
        Some((_, entries, AnchorStatus::Missing)) => {
            return HashVerifyHeadCheck::Truncated {
                entries: *entries,
                now: 0,
            };
        }
        Some((_, entries, AnchorStatus::Rewritten)) => {
            return HashVerifyHeadCheck::Rewritten {
                entries: *entries,
                why: format!("{entries}번째 항목이 머리가 기록한 항목이 아님(레인 재작성)"),
            };
        }
        _ => {}
    }

    // (2) No usable head at all.
    let Some(newest) = heads.last().copied() else {
        if result_marker_wants_head {
            return HashVerifyHeadCheck::Unknown {
                code: "own-head-missing",
                why: format!(
                    "현재 결과 실행({})은 서명 감사 레인에 {head_word}를 남기는 형식인데 쓸 수 있는 머리가 없음 — 머리 항목이 지워졌거나 인증되지 않음",
                    result_id.unwrap_or("?")
                ),
            };
        }
        // R15 P22 (I1): no usable head — a line this PC cannot use, a chain
        // failure or a repair in the sign-off lane may BE the newest head
        // (its HMAC stripped, or cut by the repair). Its content is never
        // relied on: unknown, not "absent".
        if let Some(failure) = &signoff.failure {
            return HashVerifyHeadCheck::Unknown {
                code: "chain-failed",
                why: format!(
                    "쓸 수 있는 {head_word}가 없고 서명 감사 레인의 감사 체인 검증이 실패함 — 깨진 줄이 머리였을 수 있음: {failure}"
                ),
            };
        }
        let unusable: Vec<&crate::audit::LaneRow> = signoff.unusable().collect();
        if !unusable.is_empty() {
            return unusable_unknown(&unusable, "쓸 수 있는 머리가 없는데", lane);
        }
        if !signoff.repairs.is_empty() {
            return HashVerifyHeadCheck::Unknown {
                code: "repaired",
                why: format!(
                    "쓸 수 있는 {head_word}가 없고 서명 감사 레인이 수리됨(lane-repair) — 잘린 꼬리에 머리가 있었을 수 있음"
                ),
            };
        }
        return HashVerifyHeadCheck::Absent;
    };
    // P12 by position: an unusable line after the newest usable head may
    // itself be a newer head — its content is never relied on.
    let unusable_after: Vec<&crate::audit::LaneRow> = signoff
        .unusable()
        .filter(|row| row.index > newest.index)
        .collect();
    if !unusable_after.is_empty() {
        return unusable_unknown(&unusable_after, "마지막 머리 뒤에", lane);
    }
    if signoff.repaired_after(newest.index) {
        return HashVerifyHeadCheck::Unknown {
            code: "repaired",
            why: format!(
                "서명 감사 레인이 마지막 {head_word} 뒤에서 수리됨(lane-repair) — 잘린 꼬리에 더 새 머리가 있었을 수 있음"
            ),
        };
    }
    if let Err(why) = head_anchor(&newest.value, lane) {
        return HashVerifyHeadCheck::Unknown {
            code: "malformed",
            why: format!(
                "{head_word}(서명 감사 레인 {}번째 줄)에 {why}",
                newest.index + 1
            ),
        };
    }

    // (3) P26: the head must pair with the result run.
    let paired = result_id.and_then(|result| {
        heads.iter().rposition(|row| {
            id_of(row).as_deref() == Some(result)
                && (lane == HeadLane::HashVerify || is_v2_head(&row.value))
        })
    });
    match (result_id, paired) {
        (Some(result), Some(at)) => {
            for row in &heads[at + 1..] {
                if !is_v2_head(&row.value) {
                    return HashVerifyHeadCheck::Unknown {
                        code: "mixed-version",
                        why: format!(
                            "{HEAD_MIXED_VERSION_NOTE} — 현재 결과 실행({result})의 {head_word}(서명 감사 레인 {}번째 줄) 뒤에 이전 버전(R14 형식, lanes 없음) 머리가 있음(서명 감사 레인 {}번째 줄): 이전 버전 빌드가 그 뒤에 스캔했는데 그 실행이 현재 결과가 아님 — 어느 실행이 현재 결과인지 머리로 확인할 수 없음 ({HEAD_RESCAN_NOTE})",
                            heads[at].index + 1,
                            row.index + 1
                        ),
                    };
                }
                let other = id_of(row);
                let mismatch = match lane {
                    HeadLane::Anomaly => other.as_deref() != Some(result),
                    // A scan without re-hash names no re-verification run.
                    HeadLane::HashVerify => other.as_deref().is_some_and(|id| id != result),
                };
                if mismatch {
                    return HashVerifyHeadCheck::Unknown {
                        code: "scan-mismatch",
                        why: format!(
                            "{HEAD_SCAN_MISMATCH_NOTE} — 현재 결과 실행({result})의 {head_word} 뒤에 다른 스캔({})의 머리가 있음(서명 감사 레인 {}번째 줄): 그 스캔의 완료 표식이 이 레인에서 쓸 수 있는 결과가 아님 — 지워졌거나 인증되지 않음 ({HEAD_RESCAN_NOTE})",
                            other.as_deref().unwrap_or("?"),
                            row.index + 1
                        ),
                    };
                }
            }
        }
        (Some(result), None) if result_marker_wants_head => {
            // The result run says it anchored itself, but no usable head
            // names it — its own head is gone (the sign-off lane's tail
            // cut, or the head write failed).
            return HashVerifyHeadCheck::Unknown {
                code: "own-head-missing",
                why: format!("현재 결과 실행({result})의 {head_word}가 서명 감사 레인에 없음"),
            };
        }
        _ => {
            if lane == HeadLane::Anomaly
                && let Some(newest_v2) = heads.iter().rev().find(|row| is_v2_head(&row.value))
            {
                // R17 I1 (P27): an older build's run is a USABLE marker
                // without `scan_id`; no usable marker at all while a v2 head
                // anchors the lane is a marker removed or not authenticated
                // here — never worded as a version mix.
                if result_id.is_none() && !result.usable_marker {
                    return HashVerifyHeadCheck::Unknown {
                        code: "marker-unusable",
                        why: format!(
                            "{HEAD_MARKER_UNUSABLE_NOTE} — 서명 감사 레인에 R15 형식 {head_word}가 있는데 이상 징후 레인에 이 PC에서 쓸 수 있는 완료 표식(anomaly-scan-run)이 없음: 완료 표식이 지워졌거나 HMAC이 제거됐거나 인증되지 않음(변조 가능성 — 버전 혼재로 보지 않음) — 어느 실행이 현재 결과인지 확인할 수 없음"
                        ),
                    };
                }
                // R18 I1 (P27): a usable marker without `scan_id` is an
                // older build's run AFTER the newest v2 head only — it lies
                // past that head's anchored range and no line this PC
                // cannot use follows it. One inside the range is OLDER than
                // the head's own scan, whose marker is then gone or not
                // usable here (R17 fell back to it and said
                // `mixed-version`: the reviewer's PoC-A); a line this PC
                // cannot use after it may be a newer marker.
                if result_id.is_none() {
                    // A malformed newest v2 anchor: fail closed (inside).
                    let anchored = head_anchor(&newest_v2.value, lane)
                        .ok()
                        .flatten()
                        .map(|(entries, _)| entries);
                    let at = result.marker_index.unwrap_or(0);
                    let inside = anchored.is_none_or(|entries| (at as u64) < entries);
                    if inside || result.unusable_after_marker {
                        let place = if inside {
                            format!(
                                "scan_id 없는 완료 표식(이상 징후 레인 {}번째 줄)은 그 머리가 기록한 범위(항목 {}개) 안 — 그 머리의 스캔보다 오래된 실행이라 현재 결과가 아님",
                                at + 1,
                                anchored.map_or_else(|| "?".to_string(), |e| e.to_string())
                            )
                        } else {
                            format!(
                                "scan_id 없는 완료 표식(이상 징후 레인 {}번째 줄) 뒤에 이 PC에서 쓸 수 없는 줄이 있음 — 더 새 완료 표식일 수 있음",
                                at + 1
                            )
                        };
                        return HashVerifyHeadCheck::Unknown {
                            code: "marker-unusable",
                            why: format!(
                                "{HEAD_MARKER_UNUSABLE_NOTE} — 서명 감사 레인의 R15 형식 {head_word}(서명 감사 레인 {}번째 줄)의 스캔 완료 표식(anomaly-scan-run)을 이 PC에서 쓸 수 없음; {place}: 완료 표식이 지워졌거나 HMAC이 제거됐거나 인증되지 않음(변조 가능성 — 버전 혼재로 보지 않음) — 어느 실행이 현재 결과인지 확인할 수 없음",
                                newest_v2.index + 1
                            ),
                        };
                    }
                }
                return HashVerifyHeadCheck::Unknown {
                    code: "mixed-version",
                    why: format!(
                        "{HEAD_MIXED_VERSION_NOTE} — 현재 결과 후보인 이상 징후 완료 표식에 scan_id가 없음(이전 버전 빌드의 실행)인데 서명 감사 레인에 R15 형식 {head_word}가 있어 결과 실행과 머리를 짝지을 수 없음 ({HEAD_RESCAN_NOTE})"
                    ),
                };
            }
            // R17 I5 (P26): the re-verification result run has no head of
            // its own (an older build's run, no `head_in`) while a v2 head
            // names ANOTHER re-verification run — that run is not the
            // current result here.
            if lane == HeadLane::HashVerify
                && let Some(result) = result_id
                && let Some(row) = heads
                    .iter()
                    .rev()
                    .find(|row| is_v2_head(&row.value) && id_of(row).is_some_and(|id| id != result))
            {
                return HashVerifyHeadCheck::Unknown {
                    code: "scan-mismatch",
                    why: format!(
                        "{HEAD_SCAN_MISMATCH_NOTE} — 현재 결과 실행({result})의 {head_word}는 없고(머리를 남기지 않는 형식) 서명 감사 레인에 다른 재검증 실행({})의 R15 형식 머리가 있음(서명 감사 레인 {}번째 줄): 그 실행이 이 레인에서 쓸 수 있는 현재 결과가 아님 — 지워졌거나 인증되지 않았거나 이전 버전 빌드가 그 뒤에 스캔함 ({HEAD_RESCAN_NOTE})",
                        id_of(row).as_deref().unwrap_or("?"),
                        row.index + 1
                    ),
                };
            }
        }
    }

    match anchor_status {
        // Only R14 heads, and the anomaly lane: nothing anchors it.
        None => HashVerifyHeadCheck::Absent,
        Some((row, entries, status)) => match status {
            AnchorStatus::Intact => HashVerifyHeadCheck::Intact {
                entries,
                run_id: id_of(row),
            },
            AnchorStatus::Unreadable(err) => HashVerifyHeadCheck::Unknown {
                code: "unreadable",
                why: format!("{}을 읽지 못함: {err}", lane.noun()),
            },
            AnchorStatus::NotAuthenticated(why) => HashVerifyHeadCheck::NotAuthenticated(why),
            // Returned in (1).
            AnchorStatus::Truncated { now } => HashVerifyHeadCheck::Truncated { entries, now },
            AnchorStatus::Missing => HashVerifyHeadCheck::Truncated { entries, now: 0 },
            AnchorStatus::Rewritten => HashVerifyHeadCheck::Rewritten {
                entries,
                why: format!("{entries}번째 항목이 머리가 기록한 항목이 아님(레인 재작성)"),
            },
        },
    }
}
