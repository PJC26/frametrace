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
    /// `repaired`, `malformed`, `own-head-missing`, `unreadable`. `None`
    /// for every other state.
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

/// `Unknown` for `count` unusable sign-off lines (`where_`: Korean
/// position phrase), split by I5: all another PC's → `other-pc`.
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
        let keys = keys.into_iter().collect::<Vec<_>>().join(", ");
        return HashVerifyHeadCheck::Unknown {
            code: "other-pc",
            why: format!(
                "{HEAD_OTHER_PC_NOTE} — 서명 감사 레인의 {where_} 다른 PC의 키(\"{keys}\")로 서명된 줄 {count}개: 그 PC가 쓴 더 새 {head}일 수 있어 이 PC에서는 비교할 머리를 알 수 없음 (키 보유 PC에서 확인하거나 이 PC에서 다시 실행; 변조 판정 아님)"
            ),
        };
    }
    HashVerifyHeadCheck::Unknown {
        code: "unusable-line",
        why: format!(
            "서명 감사 레인의 {where_} 이 PC에서 쓸 수 없는 줄 {count}개 — 더 새 {head}일 수 있어(HMAC이 제거된 머리 포함) 비교할 머리를 알 수 없음"
        ),
    }
}

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
pub fn anomaly_head_check(root: &Path) -> HashVerifyHeadCheck {
    let lane = match crate::audit::read_lane_rows(&root.join(ANOMALY_LOG_REL_PATH)) {
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
    lane_head_check(root, HeadLane::Anomaly, scan_id.as_deref(), wants)
}

/// The newest usable [`HASH_VERIFY_HEAD_EVENT`] in `root`'s sign-off lane
/// against `root`'s `lane` (P22).
pub fn lane_head_check(
    root: &Path,
    lane: HeadLane,
    result_id: Option<&str>,
    result_marker_wants_head: bool,
) -> HashVerifyHeadCheck {
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
    let is_head = |value: &serde_json::Value| {
        value.get("event").and_then(|v| v.as_str()) == Some(HASH_VERIFY_HEAD_EVENT)
    };
    let names_result = |value: &serde_json::Value, result: &str| {
        value.get(lane.id_field()).and_then(|v| v.as_str()) == Some(result)
    };
    let newest = signoff.usable().rev().find(|row| is_head(&row.value));
    let Some(head_row) = newest else {
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
        // read: unknown, not "absent".
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
    // itself be a newer head — its content is never read.
    let unusable_after: Vec<&crate::audit::LaneRow> = signoff
        .unusable()
        .filter(|row| row.index > head_row.index)
        .collect();
    if !unusable_after.is_empty() {
        return unusable_unknown(&unusable_after, "마지막 머리 뒤에", lane);
    }
    if signoff.repaired_after(head_row.index) {
        return HashVerifyHeadCheck::Unknown {
            code: "repaired",
            why: format!(
                "서명 감사 레인이 마지막 {head_word} 뒤에서 수리됨(lane-repair) — 잘린 꼬리에 더 새 머리가 있었을 수 있음"
            ),
        };
    }
    let value = &head_row.value;
    let head_run = value
        .get(lane.id_field())
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // R15: a v2 head names every lane in `lanes`; an R14 head anchors the
    // re-verification lane at the top level only.
    let anchor = match value.get("lanes") {
        Some(lanes) => match lanes.get(lane.rel()) {
            Some(anchor) => anchor,
            None => {
                return HashVerifyHeadCheck::Unknown {
                    code: "malformed",
                    why: format!(
                        "{head_word}(서명 감사 레인 {}번째 줄)의 lanes에 {}이 없음",
                        head_row.index + 1,
                        lane.rel()
                    ),
                };
            }
        },
        None if lane == HeadLane::HashVerify => value,
        // An R14 head: the anomaly lane was never anchored.
        None => return HashVerifyHeadCheck::Absent,
    };
    let entries = anchor.get("entries").and_then(|v| v.as_u64());
    let last = anchor.get("last_entry_sha256").and_then(|v| v.as_str());
    let (entries, last) = match (entries, last) {
        (Some(0), _) => (0, ""),
        (Some(entries), Some(last)) => (entries, last),
        _ => {
            return HashVerifyHeadCheck::Unknown {
                code: "malformed",
                why: format!(
                    "{head_word}(서명 감사 레인 {}번째 줄)에 entries / last_entry_sha256이 없음",
                    head_row.index + 1
                ),
            };
        }
    };
    // The result run says it anchored itself, but no usable head names it
    // — its own head is gone (the sign-off lane's tail cut, or the head
    // write failed).
    if result_marker_wants_head
        && let Some(result) = result_id
        && head_run.as_deref() != Some(result)
        && !signoff
            .usable()
            .any(|row| is_head(&row.value) && names_result(&row.value, result))
    {
        return HashVerifyHeadCheck::Unknown {
            code: "own-head-missing",
            why: format!("현재 결과 실행({result})의 {head_word}가 서명 감사 레인에 없음"),
        };
    }
    if entries == 0 {
        // The lane was empty when the head was written: nothing to lose.
        return HashVerifyHeadCheck::Intact {
            entries,
            run_id: head_run,
        };
    }
    let head = crate::audit::LaneHead {
        entries,
        last_entry_sha256: last.to_string(),
    };
    match crate::audit::check_lane_anchor(&root.join(lane.rel()), &head) {
        crate::audit::AnchorStatus::Intact => HashVerifyHeadCheck::Intact {
            entries,
            run_id: head_run,
        },
        crate::audit::AnchorStatus::Truncated { now } => {
            HashVerifyHeadCheck::Truncated { entries, now }
        }
        crate::audit::AnchorStatus::Missing => HashVerifyHeadCheck::Truncated { entries, now: 0 },
        crate::audit::AnchorStatus::Rewritten => HashVerifyHeadCheck::Rewritten {
            entries,
            why: format!("{entries}번째 항목이 머리가 기록한 항목이 아님(레인 재작성)"),
        },
        crate::audit::AnchorStatus::Unreadable(err) => HashVerifyHeadCheck::Unknown {
            code: "unreadable",
            why: format!("{}을 읽지 못함: {err}", lane.noun()),
        },
        crate::audit::AnchorStatus::NotAuthenticated(why) => {
            HashVerifyHeadCheck::NotAuthenticated(why)
        }
    }
}
