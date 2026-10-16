//! R14 P20 / I3: the `hash-verify-head` anchor. Each completed live
//! re-hash run appends, after its `hash-verify-run` marker, one keyed
//! (HMAC when a key is configured) entry to the SIGN-OFF lane naming the
//! re-verification lane's entry count and last `entry_sha256`. Deleting
//! the newest run's rows AND its marker (two lines, no key: the shorter
//! lane still verifies) then leaves the lane shorter than its head —
//! "꼬리 잘림 — 결과 없음" — instead of the previous run reading green.
//!
//! Kept apart from the finalize pairing code in `report.rs` (track S).

use std::path::Path;

/// The sign-off-lane event a completed re-hash run appends.
pub const HASH_VERIFY_HEAD_EVENT: &str = "hash-verify-head";

/// The field an R14 `hash-verify-run` marker carries: its run wrote (or
/// tried to write) a [`HASH_VERIFY_HEAD_EVENT`] — a result run with this
/// field but no usable head naming it has lost its anchor.
pub const HASH_VERIFY_MARKER_HEAD_FIELD: &str = "head_in";

/// The reason word every reader shows for a re-verification lane shorter
/// than (or rewritten from) its recorded head. `/api/verify-sources`
/// `withheld_reason` CONTAINS it.
pub const HASH_VERIFY_HEAD_TRUNCATED: &str = "꼬리 잘림 — 결과 없음";

/// Appends the [`HASH_VERIFY_HEAD_EVENT`] for the re-verification lane as
/// it is now (call right after the run's completion marker, under the
/// case's scan lock).
pub fn record_hash_verify_head(case_dir: &Path, run_id: &str, run_unix: u64) -> Result<(), String> {
    let lane = case_dir.join(crate::anomaly::HASH_VERIFY_REL_PATH);
    let head = crate::audit::lane_head(&lane)?
        .ok_or_else(|| format!("재검증 기록 레인이 비어 있습니다: {}", lane.display()))?;
    crate::audit::append_chained_jsonl(
        &case_dir.join(super::SIGNOFF_LOG_REL_PATH),
        &serde_json::json!({
            "schema_version": 1,
            "event": HASH_VERIFY_HEAD_EVENT,
            "lane": crate::anomaly::HASH_VERIFY_REL_PATH,
            "run_id": run_id,
            "run_unix": run_unix,
            "entries": head.entries,
            "last_entry_sha256": head.last_entry_sha256,
        })
        .to_string(),
    )
    .map_err(|err| {
        format!("재검증 레인 머리(hash-verify-head)를 서명 감사 레인에 기록하지 못했습니다: {err}")
    })
}

/// What the newest usable [`HASH_VERIFY_HEAD_EVENT`] says about the
/// re-verification lane now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashVerifyHeadCheck {
    /// No head was ever recorded (a case from before R14, or no sign-off
    /// lane) — nothing to compare.
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
    /// PC cannot use after it (a newer head may be among them), it was
    /// repaired after it, or the head is malformed. No result.
    Unknown(String),
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
            Self::Unknown(_) => "unknown",
            Self::NotAuthenticated(_) => "not-authenticated",
        }
    }

    /// Why there is no current result (Korean), `None` when the head does
    /// not withhold it.
    pub fn withhold_reason(&self) -> Option<String> {
        match self {
            Self::Absent | Self::Intact { .. } | Self::NotAuthenticated(_) => None,
            Self::Truncated { entries, now } => Some(format!(
                "{HASH_VERIFY_HEAD_TRUNCATED}: 서명 감사 레인의 재검증 머리(hash-verify-head)는 항목 {entries}개를 기록했으나 재검증 기록 레인에는 {now}개뿐 — 레인 꼬리(더 새 실행의 기록·완료 표식)가 지워졌습니다 (원본 재검증을 다시 실행하십시오)"
            )),
            Self::Rewritten { entries, why } => Some(format!(
                "{HASH_VERIFY_HEAD_TRUNCATED}: 재검증 기록 레인이 서명 감사 레인의 재검증 머리(항목 {entries}개)와 다름 — {why} (원본 재검증을 다시 실행하십시오)"
            )),
            Self::Unknown(why) => Some(format!(
                "재검증 머리 확인 불가 — 결과 없음: {why} (원본 재검증을 다시 실행하십시오)"
            )),
        }
    }

    /// Why the result is not verified as a whole although not withheld.
    pub fn not_verified_reason(&self) -> Option<String> {
        match self {
            Self::NotAuthenticated(why) => Some(format!("재검증 머리까지의 레인 확인 불가: {why}")),
            _ => None,
        }
    }

    /// The custody statement / `verify-package` sentence (English).
    pub fn describe(&self) -> String {
        match self {
            Self::Absent => "no hash-verify-head was recorded in the sign-off lane (re-hash runs before R14); a deleted lane tail cannot be detected".to_string(),
            Self::Intact { entries, .. } => format!(
                "the re-verification lane still holds the {entries} entries its newest hash-verify-head (sign-off lane) recorded"
            ),
            Self::Truncated { entries, now } => format!(
                "TAIL CUT: the sign-off lane's hash-verify-head recorded {entries} entries, the re-verification lane now holds {now} / {HASH_VERIFY_HEAD_TRUNCATED}"
            ),
            Self::Rewritten { entries, why } => format!(
                "REWRITTEN: the re-verification lane differs from its hash-verify-head ({entries} entries): {why} / {HASH_VERIFY_HEAD_TRUNCATED}"
            ),
            Self::Unknown(why) => format!("the hash-verify-head cannot be relied on: {why}"),
            Self::NotAuthenticated(why) => {
                format!("the re-verification lane up to its hash-verify-head is not verifiable here: {why}")
            }
        }
    }
}

/// R14 I3: the newest usable [`HASH_VERIFY_HEAD_EVENT`] in `root`'s
/// sign-off lane against `root`'s re-verification lane (a case folder or
/// a package). `result_marker_wants_head`: the result run's marker says a
/// head was written for it ([`HASH_VERIFY_MARKER_HEAD_FIELD`]);
/// `result_run_id`: that run.
pub fn hash_verify_head_check(
    root: &Path,
    result_run_id: Option<&str>,
    result_marker_wants_head: bool,
) -> HashVerifyHeadCheck {
    let signoff = match crate::audit::read_lane_rows(&root.join(super::SIGNOFF_LOG_REL_PATH)) {
        Ok(rows) => rows,
        Err(err) => {
            return HashVerifyHeadCheck::Unknown(format!(
                "서명 감사 레인(signoff-log)을 읽지 못함: {err}"
            ));
        }
    };
    let is_head = |value: &serde_json::Value| {
        value.get("event").and_then(|v| v.as_str()) == Some(HASH_VERIFY_HEAD_EVENT)
    };
    let newest = signoff.usable().rev().find(|row| is_head(&row.value));
    let Some(head_row) = newest else {
        if result_marker_wants_head {
            return HashVerifyHeadCheck::Unknown(format!(
                "현재 결과 실행({})은 서명 감사 레인에 재검증 머리를 남기는 형식인데 쓸 수 있는 머리가 없음 — 머리 항목이 지워졌거나 인증되지 않음",
                result_run_id.unwrap_or("?")
            ));
        }
        return HashVerifyHeadCheck::Absent;
    };
    // P12 by position: an unusable line after the newest usable head may
    // itself be a newer head — its content is never read.
    let unusable_after = signoff
        .unusable()
        .filter(|row| row.index > head_row.index)
        .count();
    if unusable_after > 0 {
        return HashVerifyHeadCheck::Unknown(format!(
            "서명 감사 레인의 마지막 재검증 머리 뒤에 이 PC에서 쓸 수 없는 줄 {unusable_after}개 — 더 새 머리일 수 있어 비교할 머리를 알 수 없음"
        ));
    }
    if signoff.repaired_after(head_row.index) {
        return HashVerifyHeadCheck::Unknown(
            "서명 감사 레인이 마지막 재검증 머리 뒤에서 수리됨(lane-repair) — 잘린 꼬리에 더 새 머리가 있었을 수 있음".to_string(),
        );
    }
    let value = &head_row.value;
    let head_run = value
        .get("run_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let (Some(entries), Some(last)) = (
        value.get("entries").and_then(|v| v.as_u64()),
        value.get("last_entry_sha256").and_then(|v| v.as_str()),
    ) else {
        return HashVerifyHeadCheck::Unknown(format!(
            "재검증 머리(서명 감사 레인 {}번째 줄)에 entries / last_entry_sha256이 없음",
            head_row.index + 1
        ));
    };
    // The result run says it anchored itself, but the newest head names
    // another run — its own head is gone (the sign-off lane's tail cut, or
    // the head write failed).
    if result_marker_wants_head
        && let Some(result) = result_run_id
        && head_run.as_deref() != Some(result)
        && !signoff.usable().any(|row| {
            is_head(&row.value) && row.value.get("run_id").and_then(|v| v.as_str()) == Some(result)
        })
    {
        return HashVerifyHeadCheck::Unknown(format!(
            "현재 결과 실행({result})의 재검증 머리가 서명 감사 레인에 없음"
        ));
    }
    let head = crate::audit::LaneHead {
        entries,
        last_entry_sha256: last.to_string(),
    };
    match crate::audit::check_lane_anchor(&root.join(crate::anomaly::HASH_VERIFY_REL_PATH), &head) {
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
        crate::audit::AnchorStatus::Unreadable(err) => {
            HashVerifyHeadCheck::Unknown(format!("재검증 기록 레인을 읽지 못함: {err}"))
        }
        crate::audit::AnchorStatus::NotAuthenticated(why) => {
            HashVerifyHeadCheck::NotAuthenticated(why)
        }
    }
}
