use crate::audit_key::AuditKey;
use crate::sha256;
use crate::util::{json_escape, read_to_string};
use fs2::FileExt;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// Log-level integrity classification per `docs/audit-hmac-design.md`'s
/// verification matrix. The chain's structural check always runs; keyed
/// integrity is reported on top of it, never instead of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditIntegrity {
    /// No keyed entries, or keyed entries that could not all be
    /// authenticated (missing key / mixed chain). Structural checks still
    /// passed — this is the pre-keying guarantee, reported honestly.
    StructuralOnly,
    /// Every entry carries an `entry_hmac_sha256` that verified against a
    /// configured key.
    Keyed,
}

impl AuditIntegrity {
    /// The stable label `verify-audit` prints and receipts record.
    pub fn label(&self) -> &'static str {
        match self {
            Self::StructuralOnly => "integrity-structural-only",
            Self::Keyed => "integrity-keyed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditChainVerification {
    pub entries: usize,
    pub last_entry_sha256: String,
    /// Entries carrying `entry_hmac_sha256`/`entry_hmac_key_id`.
    pub keyed_entries: usize,
    /// Keyed entries whose HMAC could not be checked because no key was
    /// configured, or (R6-I4) because their key id is not on this PC (a
    /// subset of `keyed_entries`).
    pub unauthenticated_keyed_entries: usize,
    /// Log-level mark per the design's verification matrix.
    pub integrity: AuditIntegrity,
    /// Caveats that keep the mark honest (mixed chain, missing key, ...).
    pub warnings: Vec<String>,
    /// R6-I2/I3: what the keyed layer established for each entry, in line
    /// order (`entry_auth[n]` is audit line `n + 1`).
    pub entry_auth: Vec<EntryAuth>,
    /// R7-I3: set when an entry signed under a key id this PC does not
    /// hold FOLLOWS an entry this PC authenticated: the log's tail may have
    /// been re-written under an invented key id. The chain still verifies
    /// structurally, but the lane must never read PASS — it is NOT
    /// VERIFIED, with this reason.
    pub rewrite_not_excluded: Option<String>,
    /// R8 P0/P1: whether this verification ran on a PC holding audit keys
    /// ([`trust_policy`] of the key set it was given). Every trust decision
    /// over this lane goes through [`AuditChainVerification::line_trust`] /
    /// [`AuditChainVerification::lane_trust`], which read THIS — never a
    /// signal inside the case files (lane presence, HMAC fields), which an
    /// attacker can delete or strip.
    pub requirement: AuthRequirement,
    /// R8 P2: the `entry_hmac_key_id` each entry carries (`None` = no
    /// HMAC), in line order like `entry_auth` — so "not authenticable here"
    /// can name the key to check it with.
    pub entry_key_ids: Vec<Option<String>>,
    /// R8-I8: entries before this lane's authenticated key-introduction
    /// marker (`{"kind":"audit-key-introduced"}`, signed by a key this PC
    /// holds, the lane's FIRST keyed entry): written before audit keying
    /// was introduced for the lane. The marker's HMAC covers the chain hash
    /// of everything before it, so this prefix cannot be extended later —
    /// but it was never authenticated, so it is self-consistency only.
    pub pre_key_entries: usize,
    /// R9-I10: the lane's first keyed entry is a key-introduction marker
    /// signed under a key this PC does NOT hold: `(entries before it, that
    /// key id)`. Those unkeyed entries are another PC's pre-key prefix —
    /// "check on the PC holding key <id>" (P2), not "HMAC stripped".
    pub pre_key_other_pc: Option<(usize, String)>,
}

/// R8 §0 — THE governing rule (docs/audit-hmac-design.md "Trust policy"):
/// whether authentication is required is decided by whether THIS PC holds
/// audit keys. Never by anything inside the case files — a lane's
/// presence, `entry_hmac_*` fields, a "keyed case" — because an attacker
/// can delete or strip those to switch the requirement off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRequirement {
    /// P0: this PC holds audit keys. Nothing that is not authenticated HERE
    /// is ever shown as trusted / current / measured / reusable.
    Required,
    /// P1: no audit key on this PC. Everything is self-consistency only and
    /// every trust display says so ("자기 일관성만 확인(인증 아님)").
    SelfConsistentOnly,
}

impl AuthRequirement {
    pub fn keys_held(self) -> bool {
        matches!(self, Self::Required)
    }
}

/// R8 P0/P1: the trust policy for a verification key set.
pub fn trust_policy(keys: &[AuditKey]) -> AuthRequirement {
    if keys.is_empty() {
        AuthRequirement::SelfConsistentOnly
    } else {
        AuthRequirement::Required
    }
}

/// R8 P0/P1: this PC's trust policy and verification key set. A key source
/// that is present but unusable is an error — callers fail closed (it is
/// never read as "no keys", which would relax P0 to P1).
pub fn trust_policy_here() -> Result<(AuthRequirement, Vec<AuditKey>), String> {
    let keys = crate::audit_key::verification_keys()?;
    Ok((trust_policy(&keys), keys))
}

/// P1 wording: what a match proves on a PC without audit keys.
pub const SELF_CONSISTENT_NOTE: &str = "자기 일관성만 확인(인증 아님)";

/// R8: what one audit entry may be relied on for, under this PC's policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineTrust {
    /// HMAC verified here under a key this PC holds (P0 satisfied).
    Authenticated,
    /// P1: no audit key on this PC — self-consistency only. `key_id` names
    /// the key the entry is HMAC-signed under, when it is.
    SelfConsistentOnly { key_id: Option<String> },
    /// R8-I8: written before the lane's authenticated key-introduction
    /// marker — self-consistency only, never "authenticated".
    PreKey,
    /// P2: signed under a key this PC does not hold — not authenticable
    /// here; check it on the PC holding `key_id`. Never a FAIL.
    OtherPc { key_id: String },
    /// P0: keys are held here and the entry is not authenticated (no HMAC —
    /// possibly stripped — or signed only by a retired key nothing later
    /// proves).
    NotAuthenticated { retired_unproven: bool },
    /// R9 P3: the entry sits at or after the point where the lane's chain
    /// stops verifying (an edited / torn line, or an authenticated entry
    /// bound to another lane or case, R9-I11) — nothing vouches for it, on
    /// any PC.
    ChainBroken,
}

impl LineTrust {
    pub fn is_authenticated(&self) -> bool {
        matches!(self, Self::Authenticated)
    }

    /// R9 P3: the per-entry trust code carried to the viewer (bundle and
    /// `/api/records-meta` log arrays, field `trust`) and the report:
    /// `"auth"` (authenticated here), `"sc"` (self-consistency only — no
    /// keys here, or the documented pre-key prefix), `"pc:<key id>"`
    /// (another PC's key — check it there, not a tamper verdict),
    /// `"unauth"` (keys held here and not authenticated, or after a chain
    /// break).
    pub fn code(&self) -> String {
        match self {
            Self::Authenticated => "auth".to_string(),
            Self::SelfConsistentOnly { .. } | Self::PreKey => "sc".to_string(),
            Self::OtherPc { key_id } => format!("pc:{key_id}"),
            Self::NotAuthenticated { .. } | Self::ChainBroken => "unauth".to_string(),
        }
    }

    /// May the entry be shown at all as a (caveated) result? True for
    /// authenticated and self-consistent-only entries; false for entries P0
    /// refuses (other PC, not authenticated).
    pub fn is_usable(&self) -> bool {
        matches!(
            self,
            Self::Authenticated | Self::SelfConsistentOnly { .. } | Self::PreKey
        )
    }

    /// One Korean sentence: why the entry is or is not relied on here.
    pub fn note(&self) -> String {
        match self {
            Self::Authenticated => "이 PC의 감사 키로 인증됨".to_string(),
            Self::SelfConsistentOnly { key_id: Some(id) } => format!(
                "{SELF_CONSISTENT_NOTE} — 이 PC에 감사 키가 없어 HMAC을 확인하지 않음 (키 \"{id}\" 보유 PC에서 확인)"
            ),
            Self::SelfConsistentOnly { key_id: None } => {
                format!("{SELF_CONSISTENT_NOTE} — 감사 키 HMAC이 없는 항목")
            }
            Self::PreKey => format!(
                "{SELF_CONSISTENT_NOTE} — 감사 키 도입 이전에 기록된 항목 (키 도입 표식이 그 앞부분을 고정)"
            ),
            Self::OtherPc { key_id } => format!(
                "이 PC에서 인증 불가 — 키 \"{key_id}\" 보유 PC에서 확인 (다른 PC의 키로 서명됨, 변조 판정 아님)"
            ),
            Self::NotAuthenticated {
                retired_unproven: true,
            } => "이 PC에서 인증되지 않음 — 은퇴한 감사 키로만 서명되었고 그 뒤에 현재 키 항목이 없음"
                .to_string(),
            Self::NotAuthenticated {
                retired_unproven: false,
            } => "이 PC에서 인증되지 않음 — 감사 키가 있는 PC에서 HMAC 없는 항목은 믿지 않음 (HMAC이 제거됐거나 키 없이 추가된 항목일 수 있음)"
                .to_string(),
            Self::ChainBroken => {
                "감사 체인 검증이 실패한 지점 이후의 항목 — 믿을 수 없음".to_string()
            }
        }
    }
}

/// R8: what a whole lane may be relied on for, under this PC's policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneTrust {
    /// P0 satisfied: keys held, every entry authenticated here.
    Authenticated,
    /// P1 (no keys here), or keys held with every entry authenticated
    /// except the R8-I8 pre-key prefix: structure verified, self-consistency
    /// only (the text says what).
    SelfConsistentOnly(String),
    /// P0/P2: keys held and at least one entry is not authenticated here.
    /// NOT VERIFIED — never PASS, and never FAIL (a FAIL needs a key this
    /// PC holds to refute an entry, which is a verification error).
    NotVerified {
        reason: String,
        /// P2: the keys the unauthenticable entries name — "check on the
        /// PC holding key <id>".
        other_pc_key_ids: Vec<String>,
    },
}

impl LaneTrust {
    /// The report's chain status word.
    pub fn status(&self) -> &'static str {
        match self {
            Self::Authenticated => "keyed",
            Self::SelfConsistentOnly(_) => "structural",
            Self::NotVerified { .. } => "not-verified",
        }
    }

    /// The stable label `verify-audit`, doctor, custody and the server print.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Authenticated => AuditIntegrity::Keyed.label(),
            Self::SelfConsistentOnly(_) => AuditIntegrity::StructuralOnly.label(),
            Self::NotVerified { .. } => "integrity-not-verified",
        }
    }

    /// A lane check may PASS (possibly with the self-consistency caveat).
    pub fn passes(&self) -> bool {
        !matches!(self, Self::NotVerified { .. })
    }

    /// The caveat / reason text (empty when authenticated).
    pub fn reason(&self) -> &str {
        match self {
            Self::Authenticated => "",
            Self::SelfConsistentOnly(text) => text,
            Self::NotVerified { reason, .. } => reason,
        }
    }
}

impl AuditChainVerification {
    /// R8: the trust of audit line `index + 1` under this PC's policy.
    pub fn line_trust(&self, index: usize) -> LineTrust {
        let key_id = self.entry_key_ids.get(index).cloned().flatten();
        if self.requirement == AuthRequirement::SelfConsistentOnly {
            return LineTrust::SelfConsistentOnly { key_id };
        }
        match self.entry_auth.get(index) {
            Some(EntryAuth::Authenticated) => LineTrust::Authenticated,
            Some(EntryAuth::UnknownKey) => LineTrust::OtherPc {
                key_id: key_id.unwrap_or_default(),
            },
            Some(EntryAuth::RetiredUnproven) => LineTrust::NotAuthenticated {
                retired_unproven: true,
            },
            Some(EntryAuth::Unkeyed) if index < self.pre_key_entries => LineTrust::PreKey,
            // R9-I10: another PC's pre-key prefix — P2 wording, never
            // "stripped". Still not usable here (NOT VERIFIED).
            Some(EntryAuth::Unkeyed)
                if self
                    .pre_key_other_pc
                    .as_ref()
                    .is_some_and(|(len, _)| index < *len) =>
            {
                LineTrust::OtherPc {
                    key_id: self
                        .pre_key_other_pc
                        .as_ref()
                        .map(|(_, id)| id.clone())
                        .unwrap_or_default(),
                }
            }
            Some(EntryAuth::Unkeyed | EntryAuth::NoKey) | None => LineTrust::NotAuthenticated {
                retired_unproven: false,
            },
        }
    }

    /// R9 (contract with track U): `(unauthenticated, other_pc)` line
    /// counts under this PC's policy — lines not authenticated here (no
    /// HMAC, possibly stripped, or a retired key nothing later proves) and
    /// lines signed under another PC's key. Both 0 without keys here.
    pub fn trust_counts(&self) -> (usize, usize) {
        let (mut unauthenticated, mut other_pc) = (0, 0);
        for index in 0..self.entries {
            match self.line_trust(index) {
                LineTrust::NotAuthenticated { .. } | LineTrust::ChainBroken => unauthenticated += 1,
                LineTrust::OtherPc { .. } => other_pc += 1,
                _ => {}
            }
        }
        (unauthenticated, other_pc)
    }

    /// R8: the trust of the whole lane under this PC's policy — the one
    /// mapping every lane consumer (verify-audit, verify-package, the
    /// report's chain table, custody, doctor, the workstation audit API)
    /// uses.
    pub fn lane_trust(&self) -> LaneTrust {
        let mut other_pc: std::collections::BTreeMap<String, usize> = Default::default();
        let (mut unkeyed, mut retired, mut pre_key, mut authenticated) = (0, 0, 0, 0);
        let mut signed_ids: std::collections::BTreeSet<String> = Default::default();
        for index in 0..self.entries {
            match self.line_trust(index) {
                LineTrust::Authenticated => authenticated += 1,
                LineTrust::SelfConsistentOnly { key_id } => signed_ids.extend(key_id),
                LineTrust::PreKey => pre_key += 1,
                LineTrust::OtherPc { key_id } => *other_pc.entry(key_id).or_default() += 1,
                LineTrust::NotAuthenticated {
                    retired_unproven: true,
                } => retired += 1,
                LineTrust::NotAuthenticated {
                    retired_unproven: false,
                }
                | LineTrust::ChainBroken => unkeyed += 1,
            }
        }
        let quoted = |ids: Vec<&String>| {
            ids.iter()
                .map(|id| format!("\"{id}\""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if self.requirement == AuthRequirement::SelfConsistentOnly {
            let mut text = format!(
                "{SELF_CONSISTENT_NOTE}: 이 PC에 감사 키가 없어 HMAC을 확인하지 않았습니다 — 구조(해시 체인)만 확인되어, 케이스 폴더를 가진 누구나 레인 전체를 다시 계산할 수 있습니다"
            );
            if !signed_ids.is_empty() {
                text.push_str(&format!(
                    " (HMAC 항목은 키 {} 보유 PC에서 확인)",
                    quoted(signed_ids.iter().collect())
                ));
            }
            return LaneTrust::SelfConsistentOnly(text);
        }
        if self.entries == 0 {
            return LaneTrust::SelfConsistentOnly("항목 없음 — 인증할 항목이 없습니다".to_string());
        }
        let other_total: usize = other_pc.values().sum();
        let not_here = unkeyed + retired + other_total;
        if not_here == 0 && self.rewrite_not_excluded.is_none() {
            if pre_key == 0 && authenticated == self.entries {
                return LaneTrust::Authenticated;
            }
            return LaneTrust::SelfConsistentOnly(format!(
                "감사 키 도입 이전 항목 {pre_key}개는 {SELF_CONSISTENT_NOTE} — 그 뒤 {authenticated}개는 이 PC의 감사 키로 인증됨 ({}번째 줄의 키 도입 표식이 앞부분을 고정)",
                pre_key + 1
            ));
        }
        let mut parts = vec![format!(
            "이 PC에서 인증되지 않은 줄 {not_here}개 (전체 {}개 중) — NOT VERIFIED: 이 PC에 감사 키가 있으면 인증되지 않은 줄은 신뢰하지 않습니다",
            self.entries
        )];
        if unkeyed > 0 {
            parts.push(format!(
                "HMAC 없는 줄 {unkeyed}개 (HMAC이 제거됐거나 키 없이 추가됐을 수 있음)"
            ));
        }
        if retired > 0 {
            parts.push(format!(
                "은퇴한 키로만 서명되고 이후 현재 키 항목이 없는 줄 {retired}개"
            ));
        }
        if other_total > 0 {
            parts.push(format!(
                "다른 PC 키로 서명된 줄 {other_total}개 — 이 PC에서 인증 불가, 키 {} 보유 PC에서 확인 (변조 판정 아님)",
                quoted(other_pc.keys().collect())
            ));
        }
        if pre_key > 0 {
            parts.push(format!(
                "감사 키 도입 이전 항목 {pre_key}개는 {SELF_CONSISTENT_NOTE}"
            ));
        }
        if let Some(reason) = &self.rewrite_not_excluded {
            parts.push(reason.clone());
        }
        LaneTrust::NotVerified {
            reason: parts.join("; "),
            other_pc_key_ids: other_pc.into_keys().collect(),
        }
    }
}

/// R9 P3: one entry of a chained lane as read through [`read_lane_rows`].
#[derive(Debug, Clone)]
pub struct LaneRow {
    /// Index of the entry in the lane (`0` = audit line 1).
    pub index: usize,
    /// The line exactly as recorded.
    pub raw: String,
    /// The parsed entry (`Null` when the line is not JSON — only possible
    /// past a chain break).
    pub value: serde_json::Value,
    /// What this entry may be relied on for under this PC's policy.
    pub trust: LineTrust,
}

impl LaneRow {
    /// R9 P3: may this entry feed a "current / confirmed / passed" claim,
    /// or a path / hash used for re-verification? Authenticated here, the
    /// documented pre-key prefix, or — on a PC without keys —
    /// self-consistency only (every display says so, P1). Never another
    /// PC's entry, an unauthenticated one where keys are held, or anything
    /// after a chain break.
    pub fn usable(&self) -> bool {
        self.trust.is_usable()
    }

    /// The entry with its per-entry `trust` code (the viewer/report
    /// contract, [`LineTrust::code`]) and, when it is not usable, a
    /// `trust_note` saying why.
    pub fn value_with_trust(&self) -> serde_json::Value {
        let mut value = match &self.value {
            serde_json::Value::Object(map) => serde_json::Value::Object(map.clone()),
            _ => serde_json::json!({}),
        };
        value["trust"] = serde_json::Value::String(self.trust.code());
        if !self.usable() {
            value["trust_note"] = serde_json::Value::String(self.trust.note());
        }
        value
    }
}

/// R9 P3: every entry of a chained lane with its [`LineTrust`], plus the
/// lane's trust — THE way code reads rows of a chained lane
/// ([`read_lane_rows`]).
#[derive(Debug, Clone)]
pub struct LaneRows {
    /// The lane file exists.
    pub present: bool,
    /// Every entry in lane order (`rows[n].index == n`).
    pub rows: Vec<LaneRow>,
    /// The lane's trust under this PC's policy (for a broken chain or an
    /// unreadable key configuration: NOT VERIFIED with that reason).
    pub lane: LaneTrust,
    /// The chain does not verify (FAIL): why. Entries from the break on
    /// are [`LineTrust::ChainBroken`].
    pub failure: Option<String>,
    /// R9 P5: the audit key configuration exists but could not be read —
    /// treated as "keys held" (fail closed) with nothing authenticated;
    /// NOT a tamper verdict.
    pub key_error: Option<String>,
    /// R9-I11: authenticated entries written before lane/case binding
    /// existed (no `audit_lane`) — whether they were moved here from
    /// another lane or case cannot be checked.
    pub unbound_authenticated: usize,
    /// The verification of the verifying prefix (all of it when
    /// `failure` is `None`).
    pub verification: Option<AuditChainVerification>,
}

impl LaneRows {
    fn absent(requirement: AuthRequirement, key_error: Option<String>) -> Self {
        let lane = match (&key_error, requirement) {
            (Some(err), _) => LaneTrust::NotVerified {
                reason: err.clone(),
                other_pc_key_ids: Vec::new(),
            },
            _ => LaneTrust::SelfConsistentOnly("항목 없음 — 레인이 없습니다".to_string()),
        };
        Self {
            present: false,
            rows: Vec::new(),
            lane,
            failure: None,
            key_error,
            unbound_authenticated: 0,
            verification: None,
        }
    }

    /// Entries that may feed claims ([`LaneRow::usable`]).
    pub fn usable(&self) -> impl DoubleEndedIterator<Item = &LaneRow> {
        self.rows.iter().filter(|row| row.usable())
    }

    /// Entries that may NOT feed claims — shown, if at all, as
    /// "인증 안 됨".
    pub fn unusable(&self) -> impl DoubleEndedIterator<Item = &LaneRow> {
        self.rows.iter().filter(|row| !row.usable())
    }

    /// Parsed values of the usable entries, in lane order.
    pub fn usable_values(&self) -> Vec<serde_json::Value> {
        self.usable().map(|row| row.value.clone()).collect()
    }

    /// Every entry with its `trust` code ([`LaneRow::value_with_trust`]).
    /// Lines that are not a JSON object (only possible past a chain break,
    /// e.g. a torn final line) are left out, as the readers always did.
    pub fn values_with_trust(&self) -> Vec<serde_json::Value> {
        self.rows
            .iter()
            .filter(|row| row.value.is_object())
            .map(LaneRow::value_with_trust)
            .collect()
    }

    /// [`Self::values_with_trust`] as JSONL text (one object per line).
    pub fn jsonl_with_trust(&self) -> String {
        self.values_with_trust()
            .iter()
            .map(|value| format!("{value}\n"))
            .collect()
    }

    /// The report's chain status word: `absent`, `failed`, or
    /// [`LaneTrust::status`].
    pub fn status(&self) -> &'static str {
        if !self.present {
            "absent"
        } else if self.failure.is_some() {
            "failed"
        } else {
            self.lane.status()
        }
    }

    /// Why the lane has its status (empty when authenticated and bound).
    pub fn detail(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(failure) = &self.failure {
            parts.push(format!("체인 검증 실패: {failure}"));
        }
        if !self.lane.reason().is_empty() && self.failure.is_none() {
            parts.push(self.lane.reason().to_string());
        }
        let unusable = self.unusable().count();
        if unusable > 0 {
            parts.push(format!(
                "인증 안 된 항목 {unusable}개는 결과·근거로 쓰지 않고 \"인증 안 됨\"으로 따로 표시"
            ));
        }
        if self.unbound_authenticated > 0 {
            parts.push(format!(
                "레인·케이스 결합이 없는 이전 형식의 인증 항목 {}개 (다른 레인·케이스에서 옮겨 왔는지 확인 불가 — 레거시)",
                self.unbound_authenticated
            ));
        }
        if let Some(verification) = &self.verification {
            parts.extend(verification.warnings.iter().cloned());
        }
        parts.join("; ")
    }

    /// The report / API chain object: `{"path", "status", "entries",
    /// "unauthenticated", "detail"}`.
    pub fn chain_json(&self, rel: &str) -> serde_json::Value {
        serde_json::json!({
            "path": rel,
            "status": self.status(),
            "entries": self.rows.len(),
            "unauthenticated": self.unusable().count(),
            "detail": self.detail(),
        })
    }
}

/// R9 P3: reads every entry of the chained lane at `path` with its
/// [`LineTrust`] under THIS PC's key set. A missing lane is `present:
/// false`; a lane that exists but cannot be read is an error (D11). A key
/// configuration that exists but cannot be read (P5) does not fail the
/// read: it is "keys held" with nothing authenticated, and `key_error`
/// says so (not a tamper verdict).
pub fn read_lane_rows(path: &Path) -> Result<LaneRows, String> {
    match crate::audit_key::verification_keys() {
        Ok(keys) => read_lane_rows_inner(path, &keys, None),
        Err(err) => read_lane_rows_inner(path, &[], Some(key_config_error_note(&err))),
    }
}

/// Test hook: [`read_lane_rows`] as it runs when the key configuration
/// could not be read (`err`).
#[cfg(test)]
pub(crate) fn read_lane_rows_with_key_error(path: &Path, err: &str) -> Result<LaneRows, String> {
    read_lane_rows_inner(path, &[], Some(key_config_error_note(err)))
}

/// [`read_lane_rows`] with an explicit verification key set.
pub fn read_lane_rows_with_keys(path: &Path, keys: &[AuditKey]) -> Result<LaneRows, String> {
    read_lane_rows_inner(path, keys, None)
}

/// R9 P5/I9: the wording for an audit key configuration that exists but
/// cannot be read — never a tamper verdict.
pub fn key_config_error_note(err: &str) -> String {
    if err.starts_with(crate::audit_key::KEY_CONFIG_UNREADABLE) {
        err.to_string()
    } else {
        format!(
            "{}: {err} — 감사 키가 있는 것으로 보고 아무 항목도 인증하지 않음 (변조 판정 아님; 키 설정을 고친 뒤 다시 확인)",
            crate::audit_key::KEY_CONFIG_UNREADABLE
        )
    }
}

/// R9 P5/I9: whether a verification error is "the audit key configuration
/// could not be read" — NOT VERIFIED (fail closed), never a FAIL / tamper
/// verdict.
pub fn is_key_config_error(err: &str) -> bool {
    err.starts_with(crate::audit_key::KEY_CONFIG_UNREADABLE)
}

/// R9 P4: every chained lane of a case for a case-wide check — the known
/// lanes anywhere under `artifacts/` and `evidence/logs/`
/// ([`collect_chain_lanes`]) plus any other `*.jsonl` directly in
/// `evidence/logs/` (verified as before, never skipped), as sorted
/// absolute paths.
pub fn case_lane_paths(case_dir: &Path) -> Vec<PathBuf> {
    let mut rels = collect_chain_lanes(case_dir);
    if let Ok(entries) = std::fs::read_dir(case_dir.join("evidence").join("logs")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.file_type().is_ok_and(|kind| kind.is_file()) && name.ends_with(".jsonl") {
                rels.push(format!("evidence/logs/{name}"));
            }
        }
    }
    rels.sort();
    rels.dedup();
    rels.iter()
        .map(|rel| {
            rel.split('/')
                .fold(case_dir.to_path_buf(), |path, part| path.join(part))
        })
        .collect()
}

fn read_lane_rows_inner(
    path: &Path,
    keys: &[AuditKey],
    key_error: Option<String>,
) -> Result<LaneRows, String> {
    let forced = key_error.is_some();
    let requirement = if forced {
        AuthRequirement::Required
    } else {
        trust_policy(keys)
    };
    let Some(text) = read_log_locked(path)? else {
        return Ok(LaneRows::absent(requirement, key_error));
    };
    let source = path.display().to_string();
    let body = text.strip_suffix('\n').unwrap_or(&text);
    let lines: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    };
    let (mut verification, mut failure) = match verify_chained_jsonl_text(&text, &source, keys) {
        Ok(verification) => (verification, None),
        Err(err) => {
            let (count, bytes) = verified_prefix(&text, keys);
            let prefix = verify_chained_jsonl_text(&text[..bytes], &source, keys)
                .map_err(|err| format!("audit lane {source}: verified prefix: {err}"))?;
            debug_assert_eq!(prefix.entries, count);
            (prefix, Some(err))
        }
    };
    if forced {
        verification.requirement = AuthRequirement::Required;
    }
    // R9-I11: an authenticated entry bound to another lane or case.
    let binding = lane_binding_of(path);
    let mut unbound_authenticated = 0usize;
    let mut verified = verification.entries;
    for (index, line) in lines.iter().enumerate().take(verification.entries) {
        if !verification.line_trust(index).is_authenticated() {
            continue;
        }
        match check_entry_binding(line, binding.as_ref()) {
            Ok(true) => {}
            Ok(false) => unbound_authenticated += 1,
            Err(err) => {
                failure = Some(format!("audit line {}: {err}", index + 1));
                verified = index;
                break;
            }
        }
    }
    let rows = lines
        .iter()
        .enumerate()
        .map(|(index, line)| LaneRow {
            index,
            raw: (*line).to_string(),
            value: serde_json::from_str(line).unwrap_or(serde_json::Value::Null),
            trust: if index < verified {
                verification.line_trust(index)
            } else {
                LineTrust::ChainBroken
            },
        })
        .collect();
    let lane = if let Some(err) = &key_error {
        LaneTrust::NotVerified {
            reason: err.clone(),
            other_pc_key_ids: Vec::new(),
        }
    } else if let Some(failure) = &failure {
        LaneTrust::NotVerified {
            reason: format!("체인 검증 실패: {failure}"),
            other_pc_key_ids: Vec::new(),
        }
    } else {
        verification.lane_trust()
    };
    Ok(LaneRows {
        present: true,
        rows,
        lane,
        failure,
        key_error,
        unbound_authenticated,
        verification: Some(verification),
    })
}

/// R9-I11: the field naming, inside the HMAC-covered bytes, the lane an
/// entry was appended to (case-relative, `/`-separated).
pub const ENTRY_LANE_FIELD: &str = "audit_lane";
/// R9-I11: the field naming the `case_id` (from `case.json`) of the case
/// an entry was appended in.
pub const ENTRY_CASE_FIELD: &str = "audit_case_id";

/// R9-I11: where a lane file sits — its case (or package) root, its
/// root-relative lane name, and that root's `case_id` when `case.json`
/// names one. `None` for a file that is not a known lane under any
/// ancestor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneBinding {
    pub rel: String,
    pub case_id: Option<String>,
}

pub fn lane_binding_of(path: &Path) -> Option<LaneBinding> {
    for root in path.ancestors().skip(1) {
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if is_known_chain_lane(&rel) {
            let case_id = read_to_string(&root.join("case.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|value| {
                    value
                        .get("case_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .filter(|id| !id.is_empty());
            return Some(LaneBinding { rel, case_id });
        }
    }
    None
}

/// R9-I11: checks an authenticated entry's lane/case binding against
/// where it was found. `Ok(true)` bound and matching, `Ok(false)` no
/// binding (a legacy entry), `Err` bound elsewhere.
fn check_entry_binding(line: &str, here: Option<&LaneBinding>) -> Result<bool, String> {
    let lane = extract_json_string(line, ENTRY_LANE_FIELD);
    let case = extract_json_string(line, ENTRY_CASE_FIELD);
    if lane.is_none() && case.is_none() {
        return Ok(false);
    }
    let Some(here) = here else {
        // A bound entry in a file that is not a known lane: it was moved
        // out of its lane.
        return Err(format!(
            "인증된 항목이 레인 {}에 결합되어 있으나 알려진 레인 위치가 아닌 곳에 있음 — 레인이 옮겨짐",
            lane.unwrap_or_default()
        ));
    };
    if let Some(lane) = &lane
        && *lane != here.rel
    {
        return Err(format!(
            "인증된 항목이 레인 {lane}에 결합되어 있으나 {}에 있음 — 다른 레인의 항목을 옮긴 것",
            here.rel
        ));
    }
    if let Some(case) = &case {
        match &here.case_id {
            Some(id) if id == case => {}
            Some(id) => {
                return Err(format!(
                    "인증된 항목이 케이스 {case}에 결합되어 있으나 이 케이스는 {id} — 다른 케이스의 레인을 옮긴 것"
                ));
            }
            None => {
                return Err(format!(
                    "인증된 항목이 케이스 {case}에 결합되어 있으나 이 위치에 case.json의 case_id가 없음 — 결합을 확인할 수 없음"
                ));
            }
        }
    }
    Ok(true)
}

/// R9-I11: the binding fields a keyed append adds to `body` for the lane
/// at `path` (none for a file that is not a known lane, or when the body
/// already names them).
fn bind_body(path: &Path, body: &str) -> String {
    if body.contains(&format!("\"{ENTRY_LANE_FIELD}\"")) {
        return body.to_string();
    }
    let Some(binding) = lane_binding_of(path) else {
        return body.to_string();
    };
    let mut fields = format!("\"{ENTRY_LANE_FIELD}\":\"{}\"", json_escape(&binding.rel));
    if let Some(case_id) = &binding.case_id {
        fields.push_str(&format!(
            ",\"{ENTRY_CASE_FIELD}\":\"{}\"",
            json_escape(case_id)
        ));
    }
    let inner = body[1..body.len() - 1].trim();
    if inner.is_empty() {
        format!("{{{fields}}}")
    } else {
        format!("{},{fields}}}", &body[..body.len() - 1])
    }
}

/// R8-I8: the `kind` of the marker `rotate-audit-key --log <case>` signs
/// into a lane that holds entries but no keyed entry yet (instead of the
/// rotation marker): the entries before it were written before audit
/// keying was introduced for the lane.
pub const KEY_INTRODUCED_KIND: &str = "audit-key-introduced";

/// R6-I2/I3/I4: the keyed-layer result of one chained entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAuth {
    /// No HMAC fields.
    Unkeyed,
    /// HMAC fields, but no audit key is configured on this PC.
    NoKey,
    /// R6-I4: HMAC under a key id none of this PC's keys has, and no
    /// configured key verifies it — authenticity cannot be checked here
    /// (not a forgery verdict).
    UnknownKey,
    /// HMAC verified under a key that is not retired, or under a retired
    /// key with a newer key's entry later in the chain proving its age.
    Authenticated,
    /// HMAC verified only under a retired key and no newer key's entry
    /// follows (R5-P7): not authenticated.
    RetiredUnproven,
}

impl EntryAuth {
    /// HMAC fields present (whether or not they could be checked here).
    pub fn is_keyed(self) -> bool {
        !matches!(self, Self::Unkeyed)
    }
}

pub fn digest_file(path: &Path) -> Result<String, String> {
    let file = File::open(path)
        .map_err(|err| format!("failed to open {} for hashing: {err}", path.display()))?;
    sha256::digest_reader(BufReader::new(file))
        .map_err(|err| format!("failed to hash {}: {err}", path.display()))
}

/// Appends one hash-chained JSONL entry. The whole read-modify-append happens
/// under an OS-level exclusive lock so concurrent workers (and a second CLI
/// process) can never interleave entries, and the entry itself is written with
/// a single append + fsync so a crash can at worst tear the final line — which
/// `append` refuses to chain onto and `verify-audit` reports distinctly.
pub fn append_chained_jsonl(path: &Path, body_json: &str) -> Result<(), String> {
    // Keyed mode is opt-in: no configured key → the exact same unkeyed
    // entries this function has always written.
    let key = crate::audit_key::configured()?;
    append_chained_jsonl_keyed(path, body_json, key.as_ref())
}

/// `append_chained_jsonl` with an explicit key, so tests and tools can
/// exercise keyed appends without touching process-global env state.
#[doc(hidden)]
pub fn append_chained_jsonl_keyed(
    path: &Path,
    body_json: &str,
    key: Option<&AuditKey>,
) -> Result<(), String> {
    let body = body_json.trim();
    if !body.starts_with('{') || !body.ends_with('}') {
        return Err("audit log body must be a JSON object".to_string());
    }
    // R9-I11: a keyed entry names, inside the HMAC-covered bytes, the lane
    // and case it was appended to, so an authenticated lane copied to
    // another lane or case no longer verifies there.
    let bound;
    let body = if key.is_some() {
        bound = bind_body(path, body);
        bound.as_str()
    } else {
        body
    };
    let was_new = !path.exists();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create audit log directory: {err}"))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|err| format!("failed to open audit log {}: {err}", path.display()))?;
    file.lock_exclusive().map_err(|err| {
        format!(
            "failed to lock audit log {} (another worker may be writing): {err}",
            path.display()
        )
    })?;
    let result = append_chained_locked(&mut file, body, key);
    let _ = file.unlock();
    // First-time log creation must have its directory entry synced too, or a
    // power loss can lose the whole audit trail despite per-line fsync.
    if result.is_ok() && was_new {
        crate::util::sync_parent_directory(path);
    }
    result
}

/// The bytes of the last non-blank line of `file`, read backwards from the
/// end in growing windows (a single 200-result audio entry can exceed one
/// window). `Ok(None)` for an empty / all-blank log. A log whose final
/// byte is not `\n` is a torn write and is refused, as before.
fn last_committed_line(file: &mut File) -> Result<Option<Vec<u8>>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let len = file.seek(SeekFrom::End(0)).map_err(|err| err.to_string())?;
    if len == 0 {
        return Ok(None);
    }
    let mut last = [0u8; 1];
    file.seek(SeekFrom::Start(len - 1))
        .and_then(|_| file.read_exact(&mut last))
        .map_err(|err| err.to_string())?;
    if last[0] != b'\n' {
        return Err(
            "audit log ends with an incomplete line (likely a torn write); run verify-audit and repair the log before appending"
                .to_string(),
        );
    }
    let is_blank = |line: &[u8]| {
        std::str::from_utf8(line)
            .map(|text| text.trim().is_empty())
            .unwrap_or(false)
    };
    let mut window: u64 = 64 * 1024;
    loop {
        let start = len.saturating_sub(window);
        let mut buffer = vec![0u8; (len - start) as usize];
        file.seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut buffer))
            .map_err(|err| err.to_string())?;
        // Bytes before the first newline may be a partial line unless the
        // window reaches the start of the file.
        let complete: &[u8] = if start == 0 {
            &buffer
        } else {
            match buffer.iter().position(|b| *b == b'\n') {
                Some(pos) => &buffer[pos + 1..],
                None => &[],
            }
        };
        let found = complete
            .split(|b| *b == b'\n')
            .rev()
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
            .find(|line| !line.is_empty() && !is_blank(line));
        if let Some(line) = found {
            // `str::lines` (the verifier) strips a trailing `\r` too.
            return Ok(Some(line.to_vec()));
        }
        if start == 0 {
            return Ok(None);
        }
        window = window.saturating_mul(4);
    }
}

fn append_chained_locked(
    file: &mut File,
    body: &str,
    key: Option<&AuditKey>,
) -> Result<(), String> {
    use std::io::Write;
    // Only the tail is read: the chain needs the last committed line, and
    // re-reading the whole log on every append made N appends O(N²).
    let previous_entry_sha256 = last_committed_line(file)
        .map_err(|err| format!("failed to read audit log for chaining: {err}"))?
        .map(|line| sha256::digest_bytes(&line))
        .unwrap_or_else(|| "GENESIS".to_string());

    let without_close = &body[..body.len() - 1];
    let chained = format!(
        "{without_close},\"previous_entry_sha256\":\"{}\"}}",
        json_escape(&previous_entry_sha256)
    );
    let entry_sha256 = sha256::digest_bytes(chained.as_bytes());
    let line = if let Some(key) = key {
        // The HMAC covers EXACTLY the bytes entry_sha256 covers (the line
        // through previous_entry_sha256 + closing brace), so keyed fields
        // are purely additive and unkeyed verification is untouched.
        let entry_hmac = crate::hmac::hmac_sha256_hex(&key.bytes, chained.as_bytes());
        format!(
            "{},\"entry_sha256\":\"{}\",\"entry_hmac_sha256\":\"{}\",\"entry_hmac_key_id\":\"{}\"}}\n",
            &chained[..chained.len() - 1],
            json_escape(&entry_sha256),
            json_escape(&entry_hmac),
            json_escape(&key.id)
        )
    } else {
        format!(
            "{},\"entry_sha256\":\"{}\"}}\n",
            &chained[..chained.len() - 1],
            json_escape(&entry_sha256)
        )
    };
    // Append mode always writes at the end; one write_all + fsync keeps the
    // window for a torn write down to the final line only.
    file.write_all(line.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|err| format!("failed to append chained audit log entry: {err}"))
}

/// Returns the sha256 of the last committed line — the chain head that the
/// next appended entry will cite as `previous_entry_sha256`. Anchoring a
/// deliverable to this value proves which audit state produced it.
pub fn chain_head(path: &Path) -> Result<Option<String>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read audit log {}: {err}", path.display()))?;
    Ok(text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| sha256::digest_bytes(line.as_bytes())))
}

/// D16: every hash-chained audit lane FrameTrace writes, relative to a case
/// (or package) root. A lane on this list is always chain-verified — never
/// classified by sniffing its first line, which a tampered first line could
/// defeat.
pub const KNOWN_CHAIN_LANES: &[&str] = &[
    "artifacts/carved/carve-log.jsonl",
    "artifacts/clips/export-log.jsonl",
    "artifacts/joined/join-log.jsonl",
    "artifacts/logs/batch-log.jsonl",
    "artifacts/logs/transcode-queue-log.jsonl",
    "artifacts/proxies/proxy-log.jsonl",
    "artifacts/thumbnails/thumbnail-log.jsonl",
    "evidence/logs/anomaly-log.jsonl",
    "evidence/logs/audio-level-log.jsonl",
    "evidence/logs/capture-log.jsonl",
    "evidence/logs/case-compare-log.jsonl",
    "evidence/logs/case-merge-log.jsonl",
    "evidence/logs/dfxml-export-log.jsonl",
    "evidence/logs/e01-audit.jsonl",
    "evidence/logs/evidence-source-log.jsonl",
    "evidence/logs/export-log.jsonl",
    "evidence/logs/hash-verify.jsonl",
    "evidence/logs/known-hash-log.jsonl",
    "evidence/logs/scan-log.jsonl",
    "evidence/logs/signoff-log.jsonl",
    "evidence/logs/timeline-log.jsonl",
    "evidence/logs/tsk-audit.jsonl",
    "evidence/logs/validation-log.jsonl",
];

/// D16/R5-P10: whether a root-relative (`/`-separated) path is a chained
/// audit lane: a [`KNOWN_CHAIN_LANES`] entry, or a continuation lane of
/// the sound-measurement log (`evidence/logs/audio-level-log.<n>.jsonl`,
/// R5-P8). Never "any `*log.jsonl` under `artifacts/`": a file that merely
/// ends in `log.jsonl` is not an audit log FrameTrace wrote and must not be
/// anchored by a sign-off or verified as one.
pub fn is_known_chain_lane(rel: &str) -> bool {
    let rel = rel.replace('\\', "/");
    KNOWN_CHAIN_LANES.contains(&rel.as_str()) || audio_level_lane_number(&rel).is_some()
}

/// R5-P8: the continuation number of a sound-measurement lane —
/// `Some(0)` for `evidence/logs/audio-level-log.jsonl`, `Some(n)` (n ≥ 1)
/// for `evidence/logs/audio-level-log.<n>.jsonl`, `None` otherwise. Only
/// the canonical spelling counts (`.1.`, never `.01.`).
pub fn audio_level_lane_number(rel: &str) -> Option<u32> {
    let rest = rel.strip_prefix("evidence/logs/audio-level-log.")?;
    if rest == "jsonl" {
        return Some(0);
    }
    let digits = rest.strip_suffix(".jsonl")?;
    if digits.is_empty()
        || digits.len() > 6
        || digits.starts_with('0')
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    digits.parse::<u32>().ok()
}

/// Every chained audit lane present under `root` (a case or package
/// directory): files under `artifacts/` and `evidence/logs/` that are
/// lanes by name ([`is_known_chain_lane`]), as sorted `/`-separated
/// relative paths. Deterministic for a given tree.
pub fn collect_chain_lanes(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, depth: u32, out: &mut Vec<String>) {
        if depth > 6 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(root, &path, depth + 1, out);
            } else if kind.is_file()
                && let Ok(rel) = path.strip_prefix(root)
            {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if is_known_chain_lane(&rel) {
                    out.push(rel);
                }
            }
        }
    }
    let mut out = Vec::new();
    for top in ["artifacts", "evidence/logs"] {
        walk(root, &root.join(top), 0, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// A lane's head at one moment (D13 truncation anchor): how many entries
/// it had and the `entry_sha256` of the last one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneHead {
    pub entries: u64,
    pub last_entry_sha256: String,
}

/// The head of the lane at `path`: entry count and last `entry_sha256`.
/// `Ok(None)` for a missing or empty lane. Lines are counted exactly as
/// the verifier splits them (on `\n`).
pub fn lane_head(path: &Path) -> Result<Option<LaneHead>, String> {
    let Some(text) = read_log_locked(path)? else {
        return Ok(None);
    };
    let body = text.strip_suffix('\n').unwrap_or(&text);
    if body.is_empty() {
        return Ok(None);
    }
    let lines: Vec<&str> = body.split('\n').collect();
    let last = lines.last().copied().unwrap_or_default();
    let last_entry_sha256 = recorded_entry_hash(last).ok_or_else(|| {
        format!(
            "audit lane {} ends with a line without entry_sha256",
            path.display()
        )
    })?;
    Ok(Some(LaneHead {
        entries: lines.len() as u64,
        last_entry_sha256,
    }))
}

/// What became of a recorded lane head (D13/D14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorStatus {
    /// The lane still holds the anchored entry at the anchored position
    /// (entries appended since are fine).
    Intact,
    /// The lane file is gone.
    Missing,
    /// The lane has fewer entries than when it was anchored — its tail
    /// was removed.
    Truncated { now: u64 },
    /// The entry at the anchored position is a different one — the lane
    /// was rewritten.
    Rewritten,
    /// The lane could not be read.
    Unreadable(String),
}

/// Checks a recorded lane head against the lane at `path` now.
pub fn check_lane_anchor(path: &Path, head: &LaneHead) -> AnchorStatus {
    let text = match read_log_locked(path) {
        Ok(Some(text)) => text,
        Ok(None) => return AnchorStatus::Missing,
        Err(err) => return AnchorStatus::Unreadable(err),
    };
    let body = text.strip_suffix('\n').unwrap_or(&text);
    let lines: Vec<&str> = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    };
    let anchored = head.entries as usize;
    if anchored == 0 {
        return AnchorStatus::Intact;
    }
    if lines.len() < anchored {
        return AnchorStatus::Truncated {
            now: lines.len() as u64,
        };
    }
    match recorded_entry_hash(lines[anchored - 1]) {
        Some(hash) if hash == head.last_entry_sha256 => AnchorStatus::Intact,
        _ => AnchorStatus::Rewritten,
    }
}

/// D17: a file's modification time at the platform's full resolution
/// (nanoseconds since the epoch where the filesystem keeps them), so two
/// writes within one second still compare different.
pub fn modified_nanos(meta: &std::fs::Metadata) -> Option<u128> {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos())
}

pub fn verify_chained_jsonl(path: &Path) -> Result<AuditChainVerification, String> {
    let keys = crate::audit_key::verification_keys()?;
    verify_chained_jsonl_keyed(path, &keys)
}

/// R9-I11: the lane/case binding of every authenticated entry of the lane
/// at `path` (`text` its content): an entry bound to another lane or case
/// fails verification; entries without a binding (written before R9) are
/// reported in a warning as legacy.
fn apply_lane_binding(
    path: &Path,
    text: &str,
    mut verification: AuditChainVerification,
) -> Result<AuditChainVerification, String> {
    let binding = lane_binding_of(path);
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut unbound = 0usize;
    if !text.is_empty() {
        for (index, line) in body.split('\n').enumerate() {
            if verification.entry_auth.get(index) != Some(&EntryAuth::Authenticated) {
                continue;
            }
            if !check_entry_binding(line, binding.as_ref())
                .map_err(|err| format!("audit line {}: {err}", index + 1))?
            {
                unbound += 1;
            }
        }
    }
    if unbound > 0 && binding.is_some() {
        verification.warnings.push(format!(
            "{unbound} authenticated entr{} carry no lane/case binding (written before R9) — legacy: whether they were moved here from another lane or case cannot be checked",
            if unbound == 1 { "y" } else { "ies" }
        ));
    }
    Ok(verification)
}

/// Reads the audit log for verification under the same exclusive lock the
/// appender holds (fs2 byte-range lock on the log file itself), so a
/// verify running concurrently with an append never observes a
/// half-written final line and reports a spurious torn write.
///
/// Windows semantics: fs2's `lock_exclusive` is a `LockFileEx` byte-range
/// lock, which is *mandatory* — while it is held, EVERY other handle to
/// the file is refused with `ERROR_LOCK_VIOLATION` (os error 33),
/// including a second handle opened by this same process. POSIX advisory
/// locks do not block the locking process, which is why locking one
/// handle and then reading through `fs::read_to_string` worked on
/// Unix but failed on Windows. The read therefore goes through the SAME
/// handle that holds the lock — the lock-owning handle always retains
/// access to its locked range — mirroring how `append_chained_locked`
/// reads through its own locked handle.
///
/// Fail-open is preserved: a verify that could not take the lock (or
/// could not open the file for locking at all, e.g. read-only media)
/// still reads unlocked rather than refusing to run.
///
/// A missing log is an error, never an intact empty chain: verification
/// must not create the file it is asked to verify (deleting a log would
/// otherwise be indistinguishable from "verified, 0 entries").
fn read_log_for_verification(path: &Path) -> Result<String, String> {
    read_log_locked(path)?.ok_or_else(|| {
        format!(
            "audit log {} does not exist (missing log is not verifiable; it may have been deleted)",
            path.display()
        )
    })
}

/// The lock-aware read behind [`read_log_for_verification`] for callers
/// that treat a missing log as "nothing logged yet" (`Ok(None)`), e.g. the
/// carve resume de-duplication. Any other failure is an error.
pub(crate) fn read_log_locked(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    let read_error =
        |err: std::io::Error| format!("failed to read audit log {}: {err}", path.display());
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return Err(format!(
                "audit log {} is not a regular file",
                path.display()
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(read_error(err)),
    }
    match std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(path)
    {
        // `append` access is requested for the lock, not for writing:
        // Windows exclusive locking requires a handle with write access.
        Ok(mut file) => {
            let _ = file.lock_exclusive();
            let mut text = String::new();
            file.read_to_string(&mut text)
                .map(|_| Some(text))
                .map_err(read_error)
        }
        Err(_) => read_to_string(path).map(Some).map_err(read_error),
    }
}

/// `verify_chained_jsonl` with an explicit key set — the design's
/// `{key_id: key}` map. Tests use this so no process-global env state is
/// touched; production callers go through `verify_chained_jsonl`.
#[doc(hidden)]
pub fn verify_chained_jsonl_keyed(
    path: &Path,
    keys: &[AuditKey],
) -> Result<AuditChainVerification, String> {
    let text = read_log_for_verification(path)?;
    let verification = verify_chained_jsonl_text(&text, &path.display().to_string(), keys)?;
    apply_lane_binding(path, &text, verification)
}

/// In-memory form of `verify_chained_jsonl` over the raw log text, so the
/// chain parser can be exercised without a filesystem (fuzz harness).
/// `source_name` is only used to name the input in error messages.
#[doc(hidden)]
pub fn verify_chained_jsonl_text(
    text: &str,
    source_name: &str,
    keys: &[AuditKey],
) -> Result<AuditChainVerification, String> {
    let normalized = crate::audit_key::normalize_retired_aliases(keys);
    let keys = normalized.as_slice();
    let complete_tail = text.is_empty() || text.ends_with('\n');
    // Split on '\n' only: `str::lines` would also swallow a trailing '\r',
    // letting CR bytes be inserted without any hash noticing. Every line,
    // including the last, must be a complete chained entry — blank or
    // whitespace-only lines are content no hash covers, so they fail
    // verification instead of being skipped.
    let body = text.strip_suffix('\n').unwrap_or(text);
    let lines: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    };
    let mut previous_entry_sha256 = "GENESIS".to_string();
    let mut last_entry_sha256 = previous_entry_sha256.clone();
    let mut entries = 0usize;
    let mut keyed_entries = 0usize;
    let mut unauthenticated_keyed = 0usize;
    // Line number of the first keyed entry. Keying is introduced, never
    // withdrawn, by the appender while the key is present (a configured
    // key signs every append), so an unkeyed entry AFTER it is either
    // stripped HMAC fields or a legitimate hand-over (the case moved to a
    // machine/account without the keyring). The two are indistinguishable
    // here, so it is a loud warning — never a hard failure of an intact
    // chain — and the log is never graded keyed (see below).
    let mut first_keyed_line: Option<usize> = None;
    // Latest keyed line seen so far, and the first transition point
    // (unkeyed line, keyed line right before it).
    let mut last_keyed_line: Option<usize> = None;
    let mut first_transition: Option<(usize, usize)> = None;
    let mut unkeyed_after_keyed: Vec<usize> = Vec::new();
    // D15: the newest key generation that has signed an entry so far, and
    // the key ids an in-log `audit-key-rotate` marker retired. A retired
    // key signing after either is refused.
    let mut newest_generation: Option<KeyGeneration> = None;
    let mut rotated_away: std::collections::BTreeSet<String> = Default::default();
    // R5-P7: entries authenticated only by a retired key, not yet followed
    // in the chain by an entry of a newer key generation. Only such a later
    // signature (or the rotation marker the new key signs) proves an entry
    // predates the retirement — entry time fields are never trusted for it.
    let mut unproven_retired: Vec<(usize, KeyGeneration)> = Vec::new();
    let mut entry_auth: Vec<EntryAuth> = Vec::with_capacity(lines.len());
    // R6-I4: keyed entries under key ids this PC does not hold.
    let mut unknown_key_ids: std::collections::BTreeSet<String> = Default::default();
    let mut unknown_key_entries = 0usize;
    // R7-I3: the first line this PC authenticated, and every unknown-key
    // line after it (line, key id) — a possible tail rewrite.
    let mut first_authenticated: Option<usize> = None;
    let mut unknown_after_authenticated: Vec<(usize, String)> = Vec::new();
    // R8 P2: the key id each entry names, for "check on the PC holding it".
    let mut entry_key_ids: Vec<Option<String>> = Vec::with_capacity(lines.len());
    // R7-I4 / R8-I7: the legacy shared id `"default"` names no particular
    // key (every PC defaulted to it). A "default" entry a key held here
    // verifies is authenticated; one no held key verifies is ANOTHER PC's
    // "default" key (not authenticable here) — never a FAIL, even when other
    // "default" entries of the same log verify here (a log continued on two
    // PCs that both used the shared default id holds both).

    for (index, line) in lines.iter().enumerate() {
        let line_number = index + 1;
        // What the keyed layer found on this line, reported up to the
        // log-level tallies.
        enum Keyed {
            Unkeyed,
            Verified(KeyGeneration),
            Unauthenticated,
            UnknownKey(String),
        }
        let verify_line = || -> Result<Keyed, String> {
            if line.trim().is_empty() {
                return Err(format!(
                    "audit line {line_number} is blank: content outside any chained entry (no hash covers it)"
                ));
            }
            let recorded_previous =
                extract_json_string(line, "previous_entry_sha256").ok_or_else(|| {
                    format!("audit line {line_number} is missing previous_entry_sha256")
                })?;
            if recorded_previous != previous_entry_sha256 {
                return Err(format!(
                    "audit line {line_number} previous hash mismatch: expected {previous_entry_sha256}, found {recorded_previous}"
                ));
            }

            let recorded_entry = extract_json_string(line, "entry_sha256")
                .ok_or_else(|| format!("audit line {line_number} is missing entry_sha256"))?;
            let signed_entry = entry_without_recorded_hash(line).ok_or_else(|| {
                format!("audit line {line_number} has invalid entry_sha256 placement")
            })?;
            let computed_entry = sha256::digest_bytes(signed_entry.as_bytes());
            if recorded_entry != computed_entry {
                return Err(format!(
                    "audit line {line_number} entry hash mismatch: expected {computed_entry}, found {recorded_entry}"
                ));
            }
            // The chained field must be the one the appender wrote last
            // before entry_sha256 — a duplicate key elsewhere in the body
            // must not be what the chain check read.
            if !signed_entry.ends_with(&format!(
                ",\"previous_entry_sha256\":\"{}\"}}",
                json_escape(&recorded_previous)
            )) {
                return Err(format!(
                    "audit line {line_number} has previous_entry_sha256 outside its chained position"
                ));
            }

            // Keyed layer — purely additive over the same signed bytes.
            let recorded_hmac = extract_json_string(line, "entry_hmac_sha256");
            let recorded_key_id = extract_json_string(line, "entry_hmac_key_id");
            // Everything after the signed bytes is the documented trailer
            // only, byte for byte: entry_sha256, then (keyed) the HMAC and
            // key id. Any other field or trailing junk there is content
            // entry_sha256 does not cover, so the line fails.
            let mut canonical = format!(
                "{},\"entry_sha256\":\"{}\"",
                &signed_entry[..signed_entry.len() - 1],
                json_escape(&recorded_entry)
            );
            if let (Some(hmac), Some(key_id)) = (&recorded_hmac, &recorded_key_id) {
                canonical.push_str(&format!(
                    ",\"entry_hmac_sha256\":\"{}\",\"entry_hmac_key_id\":\"{}\"",
                    json_escape(hmac),
                    json_escape(key_id)
                ));
            }
            canonical.push('}');
            if canonical != *line && recorded_hmac.is_some() == recorded_key_id.is_some() {
                return Err(format!(
                    "audit line {line_number} has content after entry_sha256 that the entry hash does not cover (only entry_hmac_sha256/entry_hmac_key_id may follow it)"
                ));
            }
            match (recorded_hmac, recorded_key_id) {
                (None, None) => Ok(Keyed::Unkeyed),
                (Some(_), None) | (None, Some(_)) => Err(format!(
                    "audit line {line_number} carries only one of entry_hmac_sha256/entry_hmac_key_id"
                )),
                (Some(recorded), Some(key_id)) => {
                    if keys.is_empty() {
                        // Keyed entry, no key anywhere: the structural
                        // check above already passed; authenticity is
                        // reported as unverified at the log level.
                        return Ok(Keyed::Unauthenticated);
                    }
                    // Declared key_id first, then every known key — a
                    // renamed id must not brick old entries (rotation
                    // rules in the design doc).
                    let tag = decode_hex_tag(&recorded).unwrap_or([0xff; 32]);
                    let answers = |key: &AuditKey| crate::audit_key::key_answers_to(key, &key_id);
                    let matching: Vec<&AuditKey> = keys
                        .iter()
                        .filter(|key| answers(key))
                        .chain(keys.iter().filter(|key| !answers(key)))
                        .filter(|key| {
                            crate::hmac::tags_equal(
                                &crate::hmac::hmac_sha256(&key.bytes, signed_entry.as_bytes()),
                                &tag,
                            )
                        })
                        .collect();
                    // R8 P2: only a key this PC holds AND that the entry
                    // names can refute it (FAIL); `key_answers_to` never
                    // binds the legacy "default" id to a key.
                    let held_here = keys.iter().any(answers);
                    if matching.is_empty() && !held_here {
                        // R6-I4: signed under a key this PC does not hold
                        // (another workstation's key, a key removed from
                        // the keyring): it cannot be checked here — not
                        // authenticated (R7-I3 decides whether a rewrite
                        // can be ruled out).
                        return Ok(Keyed::UnknownKey(key_id));
                    }
                    if matching.is_empty() {
                        return Err(format!(
                            "audit line {line_number} HMAC verification failed (key_id \"{key_id}\"): the entry was re-written or signed under a different key"
                        ));
                    }
                    // D15/R5-P7: a retired key only authenticates entries
                    // the chain order places before its retirement. The
                    // first key that may sign this entry wins; when only
                    // retired keys match and the order shows the entry is
                    // later, it fails like any other forged HMAC. (Keys are
                    // normalized by material first, so the same secret
                    // supplied again under another id or source is still
                    // the retired key.)
                    let mut refusal = None;
                    for key in &matching {
                        match retired_key_refusal(key, newest_generation, &rotated_away) {
                            None => return Ok(Keyed::Verified(key_generation(key))),
                            Some(reason) => {
                                refusal.get_or_insert(reason);
                            }
                        }
                    }
                    Err(format!(
                        "audit line {line_number} is signed under retired audit key \"{key_id}\" but {}: an entry signed with a key after it was retired is not authentic",
                        refusal.unwrap_or_default()
                    ))
                }
            }
        };
        match verify_line() {
            Ok(Keyed::Verified(generation)) => {
                // A newer generation's signature chains over (and so
                // proves the age of) every earlier retired-key entry.
                unproven_retired.retain(|(_, earlier)| *earlier >= generation);
                if matches!(generation, KeyGeneration::Retired(_)) {
                    unproven_retired.push((line_number, generation));
                }
                keyed_entries += 1;
                first_keyed_line.get_or_insert(line_number);
                last_keyed_line = Some(line_number);
                newest_generation = newest_generation.max(Some(generation));
                if let Some(from) = rotation_marker_from(line) {
                    rotated_away.insert(from);
                }
                first_authenticated.get_or_insert(line_number);
                entry_auth.push(EntryAuth::Authenticated);
            }
            Ok(Keyed::Unauthenticated) => {
                keyed_entries += 1;
                unauthenticated_keyed += 1;
                first_keyed_line.get_or_insert(line_number);
                last_keyed_line = Some(line_number);
                entry_auth.push(EntryAuth::NoKey);
            }
            Ok(Keyed::UnknownKey(key_id)) => {
                keyed_entries += 1;
                unauthenticated_keyed += 1;
                first_keyed_line.get_or_insert(line_number);
                last_keyed_line = Some(line_number);
                if first_authenticated.is_some() {
                    unknown_after_authenticated.push((line_number, key_id.clone()));
                }
                unknown_key_ids.insert(key_id);
                unknown_key_entries += 1;
                entry_auth.push(EntryAuth::UnknownKey);
            }
            Ok(Keyed::Unkeyed) => {
                if let Some(last_keyed) = last_keyed_line {
                    first_transition.get_or_insert((line_number, last_keyed));
                    unkeyed_after_keyed.push(line_number);
                }
                entry_auth.push(EntryAuth::Unkeyed);
            }
            Err(error) => {
                if line_number == lines.len() && !complete_tail {
                    return Err(format!(
                        "audit log {source_name} ends with an incomplete final entry (torn write): {error}; remove or repair the last line, then re-run verify"
                    ));
                }
                return Err(error);
            }
        }

        entry_key_ids.push(
            (entry_auth.last() != Some(&EntryAuth::Unkeyed))
                .then(|| extract_json_string(line, "entry_hmac_key_id"))
                .flatten(),
        );
        previous_entry_sha256 = sha256::digest_bytes(line.as_bytes());
        last_entry_sha256 = recorded_entry_hash(line).unwrap_or_else(|| last_entry_sha256.clone());
        entries += 1;
    }
    // R8-I8: the lane's first keyed entry is an authenticated
    // key-introduction marker → the unkeyed entries before it predate
    // keying (self-consistency only, see `pre_key_entries`). Evaluated
    // after the retired-key fix-up below has run (see its call site).
    let pre_key_of = |entry_auth: &[EntryAuth]| match entry_auth
        .iter()
        .position(|auth| *auth != EntryAuth::Unkeyed)
    {
        Some(first)
            if first > 0
                && !keys.is_empty()
                && entry_auth[first] == EntryAuth::Authenticated
                && serde_json::from_str::<serde_json::Value>(lines[first])
                    .ok()
                    .and_then(|value| {
                        value
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .as_deref()
                    == Some(KEY_INTRODUCED_KIND) =>
        {
            first
        }
        _ => 0,
    };

    // Log-level mark per the design's verification matrix.
    let mut warnings = Vec::new();
    let mut integrity = AuditIntegrity::StructuralOnly;
    let no_key_entries = unauthenticated_keyed - unknown_key_entries;
    if no_key_entries > 0 {
        warnings.push(format!(
            "{no_key_entries} keyed entr{} present but no audit key is configured — authenticity unverified (structural chain verified)",
            if no_key_entries == 1 { "y" } else { "ies" }
        ));
    }
    // R7-I3: an unknown key id AFTER a line this PC authenticated is not
    // "another PC's log" — this PC held the log's key, so the tail may
    // have been re-written (re-chained) under an invented key id. No
    // in-log rotation marker can excuse it: a marker naming the new id
    // would itself be signed by that unknown key, i.e. forgeable by the
    // same rewriter.
    let rewrite_not_excluded = unknown_after_authenticated.first().map(|(line, key_id)| {
        format!(
            "이 PC에 없는 키 — 재작성 배제 불가 (이 PC에서 인증 불가 — 키 \"{key_id}\" 보유 PC에서 확인): audit line {line} (and {} more) is signed under key id \"{key_id}\" that this PC does not hold, after audit line {} that this PC authenticated — another PC may have continued the log, or the tail was re-written under an invented key id; NOT VERIFIED here (check it on the PC that holds that key)",
            unknown_after_authenticated.len() - 1,
            first_authenticated.unwrap_or(0)
        )
    });
    if let Some(reason) = &rewrite_not_excluded {
        warnings.push(reason.clone());
    }
    let plain_unknown = unknown_key_entries - unknown_after_authenticated.len();
    if plain_unknown > 0 {
        let unknown_key_entries = plain_unknown;
        warnings.push(format!(
            "{unknown_key_entries} keyed entr{} signed under audit key id(s) {} that this PC does not hold — authenticity cannot be checked here (not a tampering verdict; structural chain verified)",
            if unknown_key_entries == 1 { "y" } else { "ies" },
            unknown_key_ids
                .iter()
                .map(|id| format!("\"{id}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (line, _) in &unproven_retired {
        if let Some(auth) = entry_auth.get_mut(line - 1) {
            *auth = EntryAuth::RetiredUnproven;
        }
    }
    if keyed_entries > 0 && keyed_entries < entries {
        warnings.push(
            "mixed keyed/unkeyed entries — log-level integrity degrades to structural-only"
                .to_string(),
        );
    }
    if let (Some(first_keyed), Some((first_unkeyed, keyed_before))) =
        (first_keyed_line, first_transition)
    {
        warnings.push(format!(
            "keyed→unkeyed transition at audit line {first_unkeyed} after keyed line {keyed_before} — unkeyed entries from line {first_unkeyed} on are not authenticated ({} unkeyed entr{} after the first keyed line {first_keyed}); HMAC fields may have been stripped, or the log was continued without the audit key (e.g. the case moved to a machine without the keyring) — structural chain verified, log not graded keyed",
            unkeyed_after_keyed.len(),
            if unkeyed_after_keyed.len() == 1 { "y" } else { "ies" }
        ));
    }
    if !keys.is_empty() && entries > 0 && keyed_entries == 0 {
        warnings.push(
            "an audit key is configured but the log has 0 keyed entries — it predates keying, or was fully re-chained without HMAC fields (structural chain verified only)"
                .to_string(),
        );
    }
    if let Some((first, _)) = unproven_retired.first() {
        warnings.push(format!(
            "{} entr{} from audit line {first} on {} signed only by a retired audit key and not followed by any entry of a newer key — the chain cannot show {} written before the key was retired (entry times are not trusted for this), so {} not authenticated; log not graded keyed",
            unproven_retired.len(),
            if unproven_retired.len() == 1 { "y" } else { "ies" },
            if unproven_retired.len() == 1 { "is" } else { "are" },
            if unproven_retired.len() == 1 { "it was" } else { "they were" },
            if unproven_retired.len() == 1 { "it is" } else { "they are" },
        ));
    }
    // A keyed→unkeyed transition implies keyed_entries < entries, so such
    // a log can never reach the keyed grade; neither can a log whose tail
    // is signed only by a retired key (R5-P7).
    if keyed_entries == entries
        && entries > 0
        && unauthenticated_keyed == 0
        && unproven_retired.is_empty()
    {
        integrity = AuditIntegrity::Keyed;
    }

    let pre_key_entries = pre_key_of(&entry_auth);
    // R9-I10: the introduction marker of another PC's key.
    let pre_key_other_pc = match entry_auth
        .iter()
        .position(|auth| *auth != EntryAuth::Unkeyed)
    {
        Some(first)
            if first > 0
                && entry_auth[first] == EntryAuth::UnknownKey
                && serde_json::from_str::<serde_json::Value>(lines[first])
                    .ok()
                    .and_then(|value| {
                        value
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .as_deref()
                    == Some(KEY_INTRODUCED_KIND) =>
        {
            entry_key_ids
                .get(first)
                .cloned()
                .flatten()
                .map(|key_id| (first, key_id))
        }
        _ => None,
    };
    Ok(AuditChainVerification {
        pre_key_other_pc,
        entries,
        last_entry_sha256,
        keyed_entries,
        unauthenticated_keyed_entries: unauthenticated_keyed,
        integrity,
        warnings,
        entry_auth,
        rewrite_not_excluded,
        requirement: trust_policy(keys),
        entry_key_ids,
        pre_key_entries,
    })
}

/// R7-I10: the longest prefix of `text` that verifies as a chain under
/// `keys`, as (entries, byte length incl. the final newline). A lane whose
/// tail was torn or edited is "repaired" by truncating it to this prefix;
/// a continuation marker records the prefix's SHA-256 so the repaired lane
/// can still be matched against what the marker saw.
pub fn verified_prefix(text: &str, keys: &[AuditKey]) -> (usize, usize) {
    let mut ends: Vec<usize> = Vec::new();
    for (offset, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            ends.push(offset + 1);
        }
    }
    // Verification of a prefix is monotone (a verifying prefix's own
    // prefixes verify), so binary search the line count.
    let verifies = |count: usize| {
        let bytes = if count == 0 { 0 } else { ends[count - 1] };
        verify_chained_jsonl_text(&text[..bytes], "prefix", keys).is_ok()
    };
    let (mut low, mut high) = (0usize, ends.len());
    while low < high {
        let mid = (low + high).div_ceil(2);
        if verifies(mid) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    (low, if low == 0 { 0 } else { ends[low - 1] })
}

/// Which generation of audit key signed an entry (D15): a retired key
/// ranks by its retirement time, every key that is not retired ranks above
/// all retired ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum KeyGeneration {
    Retired(u64),
    Current,
}

fn key_generation(key: &AuditKey) -> KeyGeneration {
    match key.retired_unix {
        Some(at) => KeyGeneration::Retired(at),
        None => KeyGeneration::Current,
    }
}

/// `from` of a `{"kind":"audit-key-rotate","from":"<id>",...}` marker.
fn rotation_marker_from(line: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    (value.get("kind").and_then(serde_json::Value::as_str) == Some("audit-key-rotate"))
        .then(|| value.get("from")?.as_str().map(str::to_string))
        .flatten()
}

/// Why `key` may not authenticate an entry (D15, R5-P7), or `None` when it
/// may. Only retired keys are ever refused, and only by chain ORDER: after
/// the log recorded the key's rotation marker, or after a newer key
/// generation already signed an earlier entry of the same log. Entry time
/// fields are deliberately ignored — they are written by whoever holds the
/// key, so a forger could back-date them. A retired-key entry that no
/// newer signature follows is accepted structurally but never graded
/// keyed (see `unproven_retired` in [`verify_chained_jsonl_text`]).
fn retired_key_refusal(
    key: &AuditKey,
    newest_generation: Option<KeyGeneration>,
    rotated_away: &std::collections::BTreeSet<String>,
) -> Option<String> {
    let retired = key.retired_unix?;
    if rotated_away.contains(&key.id) {
        return Some(format!(
            "an earlier audit-key-rotate entry of this log already retired \"{}\"",
            key.id
        ));
    }
    if newest_generation.is_some_and(|newest| newest > KeyGeneration::Retired(retired)) {
        return Some(format!(
            "a newer audit key already signed an earlier entry of this log (\"{}\" is retired)",
            key.id
        ));
    }
    None
}

/// Hex-decodes a recorded `entry_hmac_sha256` into a fixed-size tag.
/// Malformed (non-hex / wrong length) tags decode to a sentinel that can
/// never equal a real tag, so the comparison stays total.
fn decode_hex_tag(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut tag = [0u8; 32];
    for (index, byte) in tag.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(tag)
}

fn recorded_entry_hash(line: &str) -> Option<String> {
    extract_json_string(line, "entry_sha256")
}

/// The indexed SHA-256 of the record `selector` names (an id, or — for a
/// record whose display path is exact — its source/relative path), or of
/// the record whose file IS `source_path`.
///
/// R6-I10: the file comparison is lossless (`model::record_os_path`, the
/// raw OS form of a non-Unicode name), and a display string is only
/// matched for a record whose display path is exact: two files whose names
/// differ only in non-Unicode units share one lossy display string, and
/// the old string match handed the first one's hash to the other.
pub fn indexed_source_hash(case_dir: &Path, selector: &str, source_path: &Path) -> Option<String> {
    let text = read_to_string(&case_dir.join("db/videos.jsonl")).ok()?;
    // Callers pass the resolved (canonical) path and the index records
    // canonical paths, so no filesystem call is made here.
    let rows: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();
    let field = |row: &serde_json::Value, key: &str| {
        row.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let names_file = |row: &serde_json::Value| {
        crate::model::record_os_path(row).is_some_and(|path| path.as_path() == source_path)
    };
    let exact_display = |row: &serde_json::Value| {
        row.get("source_path_raw_hex")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
    };
    // Strongest identity first: the id, then the file itself, then an
    // exact display spelling.
    let by_id = |row: &serde_json::Value| {
        !selector.is_empty() && field(row, "id").as_deref() == Some(selector)
    };
    let shows_selector = |row: &serde_json::Value| {
        !selector.is_empty()
            && (field(row, "source_path").as_deref() == Some(selector)
                || field(row, "relative_path").as_deref() == Some(selector))
    };
    if let Some(row) = rows
        .iter()
        .find(|row| by_id(row))
        .or_else(|| rows.iter().find(|row| names_file(row)))
    {
        return field(row, "sha256");
    }
    // R7-I9: the display-string fallback is used only when it is
    // unambiguous — every record showing that string is an exact-display
    // record and they all carry the same hash. Two different records (or
    // a non-Unicode record whose lossy display collides with it) leave the
    // hash unknown rather than handing over the first one's.
    let candidates: Vec<&serde_json::Value> =
        rows.iter().filter(|row| shows_selector(row)).collect();
    let first = candidates.first()?;
    if !candidates.iter().all(|row| exact_display(row)) {
        return None;
    }
    let sha = field(first, "sha256")?;
    candidates
        .iter()
        .all(|row| field(row, "sha256").is_some_and(|other| other.eq_ignore_ascii_case(&sha)))
        .then_some(sha)
}

pub fn json_string_array(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{items}]")
}

pub fn optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

pub fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

pub fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn extract_json_string(line: &str, key: &str) -> Option<String> {
    // Audit entries are hash-chained over the raw serialized bytes, so
    // parsing must not rewrite the line — read-only field access only.
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

fn entry_without_recorded_hash(line: &str) -> Option<String> {
    let marker = ",\"entry_sha256\":";
    let start = line.rfind(marker)?;
    Some(format!("{}}}", &line[..start]))
}

/// R6-I10 tests: a non-Unicode name `<stem><bad unit>` whose lossy display
/// string is the same for every `variant` (each one a different file).
/// Test helper (R9 P3): writes `jsonl` (one JSON object per line) as a
/// real chained lane at `path`, unkeyed — fixtures for consumers that now
/// read rows through [`read_lane_rows`], which trusts nothing past a
/// chain break. Replaces any existing file.
#[cfg(test)]
pub(crate) fn test_write_lane(path: &Path, jsonl: &str) {
    let _ = std::fs::remove_file(path);
    for line in jsonl.lines().filter(|line| !line.trim().is_empty()) {
        append_chained_jsonl_keyed(path, line, None).unwrap();
    }
}

#[cfg(test)]
pub(crate) fn test_non_unicode_twin(stem: &str, variant: u8) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = stem.as_bytes().to_vec();
        bytes.push(0xf8 + (variant % 8));
        std::ffi::OsString::from_vec(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut units: Vec<u16> = stem.encode_utf16().collect();
        units.push(0xD800 + u16::from(variant));
        std::ffi::OsString::from_wide(&units)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AuditIntegrity, EntryAuth, append_chained_jsonl, append_chained_jsonl_keyed, digest_file,
        verify_chained_jsonl, verify_chained_jsonl_keyed,
    };
    use crate::audit_key::AuditKey;
    use crate::util::read_to_string;
    use std::fs;
    use std::io::Write;
    use std::path::Path;

    fn test_key(id: &str, seed: u8) -> AuditKey {
        AuditKey::new(id, &[seed; 32])
    }

    #[test]
    fn appends_chained_json_lines() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");

        append_chained_jsonl(&path, r#"{"kind":"one"}"#).unwrap();
        append_chained_jsonl(&path, r#"{"kind":"two"}"#).unwrap();
        let text = read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("\"previous_entry_sha256\":\"GENESIS\""));
        assert!(text.contains("\"entry_sha256\""));

        let verification = verify_chained_jsonl(&path).unwrap();
        assert_eq!(verification.entries, 2);
        assert_ne!(verification.last_entry_sha256, "GENESIS");

        let _ = fs::remove_dir_all(dir);
    }

    /// B10: appends read only the log tail; entries larger than the first
    /// read window and many appends still chain and verify, and a torn
    /// last line is still refused.
    #[test]
    fn tail_read_append_chains_large_entries_and_refuses_torn_tail() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-tail-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let big = "x".repeat(300 * 1024);
        append_chained_jsonl(&path, &format!(r#"{{"kind":"big","pad":"{big}"}}"#)).unwrap();
        append_chained_jsonl(&path, &format!(r#"{{"kind":"big2","pad":"{big}"}}"#)).unwrap();
        for i in 0..50 {
            append_chained_jsonl(&path, &format!(r#"{{"kind":"n","i":{i}}}"#)).unwrap();
        }
        append_chained_jsonl(&path, &format!(r#"{{"kind":"after-big","pad":"{big}"}}"#)).unwrap();
        append_chained_jsonl(&path, r#"{"kind":"last"}"#).unwrap();
        let verification = verify_chained_jsonl(&path).unwrap();
        assert_eq!(verification.entries, 54);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"torn\":")
            .unwrap();
        let err = append_chained_jsonl(&path, r#"{"kind":"x"}"#).unwrap_err();
        assert!(err.contains("incomplete line"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn verify_rejects_missing_log_without_creating_it() {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-audit-missing-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deleted-log.jsonl");

        let err = verify_chained_jsonl(&path).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        assert!(!path.exists(), "verify must not create a missing log");
        let err = verify_chained_jsonl_keyed(&path, &[]).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        assert!(!path.exists());
        // A directory where a log is expected is not an intact chain either.
        fs::create_dir_all(dir.join("dir-log.jsonl")).unwrap();
        assert!(verify_chained_jsonl(&dir.join("dir-log.jsonl")).is_err());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_tampered_audit_lines() {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-audit-tamper-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");

        append_chained_jsonl(&path, r#"{"kind":"one"}"#).unwrap();
        let text = read_to_string(&path).unwrap().replace("one", "two");
        fs::write(&path, text).unwrap();

        assert!(verify_chained_jsonl(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn hashes_files() {
        let path =
            std::env::temp_dir().join(format!("frametrace-audit-hash-test-{}", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            digest_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn append_refuses_to_chain_onto_an_incomplete_tail() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-torn-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        append_chained_jsonl(&path, r#"{"kind":"one"}"#).unwrap();
        // Simulate a crash mid-append: partial JSON with no trailing newline.
        let mut handle = fs::OpenOptions::new().append(true).open(&path).unwrap();
        handle.write_all(b"{\"kind\":\"tor").unwrap();
        drop(handle);

        let append_error = append_chained_jsonl(&path, r#"{"kind":"two"}"#).unwrap_err();
        assert!(append_error.contains("incomplete line"), "{append_error}");
        let verify_error = verify_chained_jsonl(&path).unwrap_err();
        assert!(
            verify_error.contains("incomplete final entry"),
            "{verify_error}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_appends_keep_the_chain_intact() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-race-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        std::thread::scope(|scope| {
            for worker in 0..4u32 {
                let path = path.clone();
                scope.spawn(move || {
                    for index in 0..15u32 {
                        append_chained_jsonl(
                            &path,
                            &format!("{{\"kind\":\"w{worker}\",\"i\":{index}}}"),
                        )
                        .unwrap();
                    }
                });
            }
        });
        let verification = verify_chained_jsonl(&path).unwrap();
        assert_eq!(verification.entries, 60);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Keyed append → keyed verify round trip: fields land after
    /// entry_sha256 and the log reports integrity-keyed.
    #[test]
    fn keyed_append_and_verify_round_trip() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-keyed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let key = test_key("2026-Q1", 0x42);

        append_chained_jsonl_keyed(&path, r#"{"kind":"one"}"#, Some(&key)).unwrap();
        append_chained_jsonl_keyed(&path, r#"{"kind":"two"}"#, Some(&key)).unwrap();
        let text = read_to_string(&path).unwrap();
        for line in text.lines() {
            // Keyed fields are appended after entry_sha256, exactly per
            // the design's versioned field order.
            let entry = line.find("\"entry_sha256\"").unwrap();
            let hmac = line.find("\"entry_hmac_sha256\"").unwrap();
            let key_id = line.find("\"entry_hmac_key_id\"").unwrap();
            assert!(entry < hmac && hmac < key_id, "{line}");
            assert!(line.contains("\"entry_hmac_key_id\":\"2026-Q1\""), "{line}");
        }

        let verification = verify_chained_jsonl_keyed(&path, &[key]).unwrap();
        assert_eq!(verification.entries, 2);
        assert_eq!(verification.keyed_entries, 2);
        assert_eq!(verification.integrity, AuditIntegrity::Keyed);
        assert!(verification.warnings.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A log rewritten under a different key fails verification outright —
    /// this is the threat the keyed chain exists to catch.
    #[test]
    fn keyed_log_rejects_wrong_key() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-wrongkey-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let writer_key = test_key("case-key", 0x01);
        let attacker_key = test_key("case-key", 0x02);

        append_chained_jsonl_keyed(&path, r#"{"kind":"one"}"#, Some(&writer_key)).unwrap();
        // Attacker rewrites the log under their own key: structure is
        // internally consistent, but the HMAC was made with key material
        // the verifier does not hold.
        let _ = fs::remove_file(&path);
        append_chained_jsonl_keyed(&path, r#"{"kind":"evil"}"#, Some(&attacker_key)).unwrap();

        let err = verify_chained_jsonl_keyed(&path, &[writer_key]).unwrap_err();
        assert!(err.contains("HMAC verification failed"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// R6-I10: two indexed files whose names differ only in non-Unicode
    /// units share a lossy display string; each one's hash is looked up by
    /// its lossless path, never the first display match.
    #[test]
    fn indexed_source_hash_tells_non_unicode_twins_apart() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-twins-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("db")).unwrap();
        let one = dir.join(super::test_non_unicode_twin("clip", 0));
        let two = dir.join(super::test_non_unicode_twin("clip", 1));
        assert_eq!(one.to_string_lossy(), two.to_string_lossy());
        let row = |id: &str, path: &Path, sha: &str| {
            serde_json::json!({"id": id, "source_path": path.to_string_lossy(),
                "source_path_raw_hex": crate::model::path_raw_hex(path), "sha256": sha})
            .to_string()
        };
        fs::write(
            dir.join("db/videos.jsonl"),
            format!(
                "{}\n{}\n",
                row("vid_000001", &one, &"1".repeat(64)),
                row("vid_000002", &two, &"2".repeat(64))
            ),
        )
        .unwrap();
        assert_eq!(
            super::indexed_source_hash(&dir, &two.to_string_lossy(), &two),
            Some("2".repeat(64))
        );
        assert_eq!(
            super::indexed_source_hash(&dir, "", &one),
            Some("1".repeat(64))
        );
        assert_eq!(
            super::indexed_source_hash(&dir, "vid_000002", &one),
            Some("2".repeat(64)),
            "an id names its record"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// R6-I4: an entry signed under a key id this PC does not hold (another
    /// workstation's key) cannot be checked here — "not verified", never a
    /// tampering FAIL. A key this PC DOES hold under that id that fails is
    /// still a FAIL (above). Per-entry results are reported.
    #[test]
    fn unknown_key_id_is_not_verifiable_not_tampered() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-unknown-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let mine = test_key("mine", 0x11);
        let theirs = test_key("other-pc", 0x12);
        append_chained_jsonl_keyed(&path, r#"{"kind":"two"}"#, Some(&theirs)).unwrap();
        append_chained_jsonl_keyed(&path, r#"{"kind":"one"}"#, Some(&mine)).unwrap();
        append_chained_jsonl_keyed(&path, r#"{"kind":"three"}"#, None).unwrap();
        let verification = verify_chained_jsonl_keyed(&path, std::slice::from_ref(&mine)).unwrap();
        assert_eq!(verification.rewrite_not_excluded, None);
        assert_eq!(verification.integrity, AuditIntegrity::StructuralOnly);
        assert_eq!(verification.unauthenticated_keyed_entries, 1);
        assert_eq!(
            verification.entry_auth,
            [
                EntryAuth::UnknownKey,
                EntryAuth::Authenticated,
                EntryAuth::Unkeyed
            ]
        );
        assert!(
            verification
                .warnings
                .iter()
                .any(|w| w.contains("\"other-pc\"") && w.contains("not a tampering verdict")),
            "{:?}",
            verification.warnings
        );
        let none = verify_chained_jsonl_keyed(&path, &[]).unwrap();
        assert_eq!(none.entry_auth[..2], [EntryAuth::NoKey, EntryAuth::NoKey]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A keyed log verified on a machine without the key still passes
    /// structurally but reports authenticity as unverified.
    #[test]
    fn keyed_log_without_key_is_structural_only_with_warning() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-keyless-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let key = test_key("default", 0x07);

        append_chained_jsonl_keyed(&path, r#"{"kind":"one"}"#, Some(&key)).unwrap();

        let verification = verify_chained_jsonl_keyed(&path, &[]).unwrap();
        assert_eq!(verification.entries, 1);
        assert_eq!(verification.keyed_entries, 1);
        assert_eq!(verification.unauthenticated_keyed_entries, 1);
        assert_eq!(verification.integrity, AuditIntegrity::StructuralOnly);
        assert!(
            verification
                .warnings
                .iter()
                .any(|warning| warning.contains("authenticity unverified")),
            "{:?}",
            verification.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Mixed keyed/unkeyed chains (e.g. a log that grew across a keying
    /// rollout) verify structurally but degrade the log-level mark.
    #[test]
    fn mixed_chain_degrades_to_structural_only() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-mixed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let key = test_key("default", 0x09);

        append_chained_jsonl_keyed(&path, r#"{"kind":"unkeyed"}"#, None).unwrap();
        append_chained_jsonl_keyed(&path, r#"{"kind":"keyed"}"#, Some(&key)).unwrap();

        let verification = verify_chained_jsonl_keyed(&path, &[key]).unwrap();
        assert_eq!(verification.entries, 2);
        assert_eq!(verification.keyed_entries, 1);
        assert_eq!(verification.integrity, AuditIntegrity::StructuralOnly);
        assert!(
            verification
                .warnings
                .iter()
                .any(|warning| warning.contains("mixed keyed/unkeyed")),
            "{:?}",
            verification.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A rotated key id verifies under a later configured key set: the
    /// declared id is tried first, all known keys are the fallback.
    #[test]
    fn rotated_key_ids_still_verify_via_fallback() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-rotate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        // Entry written under id "old" — same bytes the verifier now only
        // knows as "current" (a renamed id must not brick the entry).
        let old = test_key("old", 0x11);
        let current = test_key("current", 0x11);
        append_chained_jsonl_keyed(&path, r#"{"kind":"one"}"#, Some(&old)).unwrap();
        let verification = verify_chained_jsonl_keyed(&path, &[current]).unwrap();
        assert_eq!(verification.integrity, AuditIntegrity::Keyed);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Unkeyed logs (everything written before keying existed, or with no
    /// key configured) verify and report structural-only explicitly.
    #[test]
    fn unkeyed_log_reports_structural_only() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-unkeyed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        append_chained_jsonl_keyed(&path, r#"{"kind":"one"}"#, None).unwrap();
        let verification = verify_chained_jsonl(&path).unwrap();
        assert_eq!(verification.integrity, AuditIntegrity::StructuralOnly);
        assert_eq!(verification.keyed_entries, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Re-chains `log` from scratch, keeping each entry's body but writing
    /// lines `0..keep_keyed` keyed and the rest unkeyed — what an attacker
    /// who strips HMAC fields and recomputes the plain sha256 chain makes.
    fn rechain_stripping_hmac(log: &str, keep_keyed: usize, key: &AuditKey, out: &Path) {
        let _ = fs::remove_file(out);
        for (index, line) in log.lines().enumerate() {
            let cut = line.rfind(",\"previous_entry_sha256\":").unwrap();
            let body = format!("{}}}", &line[..cut]);
            let key = (index < keep_keyed).then_some(key);
            append_chained_jsonl_keyed(out, &body, key).unwrap();
        }
    }

    /// Item 13(a): HMAC stripped from EVERY line and the chain recomputed —
    /// structurally valid, so it must at least warn when a key is configured.
    #[test]
    fn fully_unkeyed_rechain_warns_when_key_is_configured() {
        use super::verify_chained_jsonl_text;
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-strip-all-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let key = test_key("k1", 0x51);
        let path = dir.join("audit.jsonl");
        for kind in ["one", "two", "three"] {
            append_chained_jsonl_keyed(&path, &format!(r#"{{"kind":"{kind}"}}"#), Some(&key))
                .unwrap();
        }
        let stripped = dir.join("stripped.jsonl");
        rechain_stripping_hmac(&read_to_string(&path).unwrap(), 0, &key, &stripped);
        let text = read_to_string(&stripped).unwrap();
        assert!(!text.contains("entry_hmac_sha256"));

        let keyed = verify_chained_jsonl_text(&text, "t", std::slice::from_ref(&key)).unwrap();
        assert_eq!(keyed.integrity, AuditIntegrity::StructuralOnly);
        assert_eq!(keyed.keyed_entries, 0);
        assert!(
            keyed
                .warnings
                .iter()
                .any(|warning| warning.contains("0 keyed entries")),
            "{:?}",
            keyed.warnings
        );
        // No key configured: an unkeyed log is the normal pre-keying case.
        let unkeyed = verify_chained_jsonl_text(&text, "t", &[]).unwrap();
        assert!(unkeyed.warnings.is_empty(), "{:?}", unkeyed.warnings);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Item 13(b): HMAC stripped from line 3 onward. The chain is
    /// structurally intact and a legitimate hand-over (case moved to a
    /// machine without the keyring) looks identical, so it verifies — with
    /// or without the key — but always as structural-only, with a warning
    /// that names the transition line and the keyed line before it.
    #[test]
    fn keyed_to_unkeyed_transition_warns_and_is_never_graded_keyed() {
        use super::verify_chained_jsonl_text;
        let dir = std::env::temp_dir().join(format!(
            "frametrace-audit-strip-tail-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let key = test_key("k1", 0x52);
        let path = dir.join("audit.jsonl");
        for kind in ["one", "two", "three", "four"] {
            append_chained_jsonl_keyed(&path, &format!(r#"{{"kind":"{kind}"}}"#), Some(&key))
                .unwrap();
        }
        let stripped = dir.join("stripped.jsonl");
        rechain_stripping_hmac(&read_to_string(&path).unwrap(), 2, &key, &stripped);
        let text = read_to_string(&stripped).unwrap();

        for keys in [std::slice::from_ref(&key), &[][..]] {
            let verification = verify_chained_jsonl_text(&text, "t", keys)
                .unwrap_or_else(|err| panic!("keys={}: {err}", keys.len()));
            assert_eq!(verification.entries, 4);
            assert_eq!(verification.keyed_entries, 2);
            assert_eq!(
                verification.integrity,
                AuditIntegrity::StructuralOnly,
                "keys={}",
                keys.len()
            );
            assert!(
                verification.warnings.iter().any(|warning| warning
                    .contains("keyed→unkeyed transition at audit line 3 after keyed line 2")
                    && warning.contains("from line 3 on are not authenticated")
                    && warning.contains("2 unkeyed entries")),
                "keys={}: {:?}",
                keys.len(),
                verification.warnings
            );
        }

        // Re-keyed after the gap (keyed, unkeyed, keyed): still a warning
        // naming the first transition, still not graded keyed.
        let gap = dir.join("gap.jsonl");
        append_chained_jsonl_keyed(&gap, r#"{"kind":"a"}"#, Some(&key)).unwrap();
        append_chained_jsonl_keyed(&gap, r#"{"kind":"b"}"#, None).unwrap();
        append_chained_jsonl_keyed(&gap, r#"{"kind":"c"}"#, Some(&key)).unwrap();
        let gap_text = read_to_string(&gap).unwrap();
        let verification =
            verify_chained_jsonl_text(&gap_text, "t", std::slice::from_ref(&key)).unwrap();
        assert_eq!(verification.integrity, AuditIntegrity::StructuralOnly);
        assert!(
            verification.warnings.iter().any(|warning| warning
                .contains("keyed→unkeyed transition at audit line 2 after keyed line 1")),
            "{:?}",
            verification.warnings
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// D15/R5-P7: a retired key only authenticates entries the chain order
    /// places before its retirement — after a newer key signed the log, or
    /// after the log's own rotation marker, its HMAC is refused; a tail it
    /// alone signed is never graded keyed. Entry times are ignored.
    #[test]
    fn retired_key_cannot_authenticate_entries_after_its_retirement() {
        use super::verify_chained_jsonl_text;
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-retired-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let old = test_key("old", 0x61);
        let new = test_key("new", 0x62);
        let keys = [old.clone().retired_at(1_000), new.clone()];
        let log = |name: &str, entries: &[(&str, &AuditKey)]| -> String {
            let path = dir.join(name);
            for (body, key) in entries {
                append_chained_jsonl_keyed(&path, body, Some(key)).unwrap();
            }
            read_to_string(&path).unwrap()
        };

        // R5-P7: order, never time. An entry signed by the retired key and
        // FOLLOWED by an entry of the newer key is proven to predate the
        // retirement — whatever time fields it carries.
        let proven = log(
            "proven.jsonl",
            &[
                (r#"{"kind":"a","created_unix":2000}"#, &old),
                (r#"{"kind":"b"}"#, &new),
            ],
        );
        let verification = verify_chained_jsonl_text(&proven, "t", &keys).unwrap();
        assert_eq!(verification.integrity, AuditIntegrity::Keyed);
        // A retired-key tail no newer signature follows cannot be shown to
        // predate the retirement: a back-dated time no longer buys the
        // keyed grade (it did before R5: `created_unix` 900 < 1000 passed).
        for (name, body) in [
            ("backdated.jsonl", r#"{"kind":"a","created_unix":900}"#),
            ("undated.jsonl", r#"{"kind":"a"}"#),
            ("late.jsonl", r#"{"kind":"a","created_unix":2000}"#),
        ] {
            let tail = log(name, &[(body, &old)]);
            let verification = verify_chained_jsonl_text(&tail, "t", &keys).unwrap();
            assert_eq!(
                verification.integrity,
                AuditIntegrity::StructuralOnly,
                "{name}"
            );
            assert!(
                verification
                    .warnings
                    .iter()
                    .any(|w| w.contains("retired audit key") && w.contains("line 1")),
                "{name}: {:?}",
                verification.warnings
            );
        }
        // A verifier that does not know about the retirement still accepts
        // it as keyed (no keyring retirement = an ordinary key).
        let late = log("late2.jsonl", &[(r#"{"kind":"a"}"#, &old)]);
        assert_eq!(
            verify_chained_jsonl_text(&late, "t", std::slice::from_ref(&old))
                .unwrap()
                .integrity,
            AuditIntegrity::Keyed
        );
        // The retired secret supplied again under another id (a
        // `--key-source` / env alias, not marked retired) is still the
        // retired key: no keyed grade, and refused after the marker.
        let alias = test_key("default", 0x61);
        let aliased = [alias.clone(), old.clone().retired_at(1_000), new.clone()];
        let verification = verify_chained_jsonl_text(&late, "t", &aliased).unwrap();
        assert_eq!(verification.integrity, AuditIntegrity::StructuralOnly);

        // Undated, but after the newer key already signed this log.
        let after_new = log(
            "after-new.jsonl",
            &[
                (r#"{"kind":"a"}"#, &old),
                (r#"{"kind":"b"}"#, &new),
                (r#"{"kind":"c"}"#, &old),
            ],
        );
        let err = verify_chained_jsonl_text(&after_new, "t", &keys).unwrap_err();
        assert!(
            err.contains("audit line 3") && err.contains("newer audit key"),
            "{err}"
        );

        // After the log's own rotation marker (even a marker the old
        // generation could not out-rank by time).
        let marked = log(
            "marked.jsonl",
            &[
                (r#"{"kind":"a"}"#, &old),
                (
                    r#"{"kind":"audit-key-rotate","from":"old","to":"new"}"#,
                    &old,
                ),
                (r#"{"kind":"x"}"#, &old),
            ],
        );
        let err = verify_chained_jsonl_text(&marked, "t", &keys).unwrap_err();
        assert!(
            err.contains("audit line 3") && err.contains("audit-key-rotate"),
            "{err}"
        );
        let err = verify_chained_jsonl_text(&marked, "t", &aliased).unwrap_err();
        assert!(
            err.contains("audit line 3") && err.contains("audit-key-rotate"),
            "alias of the retired key must be refused after the marker: {err}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// D13/D14: a lane anchor survives appends but catches a removed tail
    /// (which the chain itself cannot see), a rewrite and a deleted lane.
    #[test]
    fn lane_anchor_detects_truncation_rewrite_and_removal_but_not_appends() {
        use super::{AnchorStatus, check_lane_anchor, lane_head};
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-anchor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lane-log.jsonl");
        assert_eq!(lane_head(&path).unwrap(), None);
        for kind in ["one", "two", "three"] {
            append_chained_jsonl_keyed(&path, &format!(r#"{{"kind":"{kind}"}}"#), None).unwrap();
        }
        let head = lane_head(&path).unwrap().unwrap();
        assert_eq!(head.entries, 3);
        assert_eq!(check_lane_anchor(&path, &head), AnchorStatus::Intact);
        append_chained_jsonl_keyed(&path, r#"{"kind":"four"}"#, None).unwrap();
        assert_eq!(check_lane_anchor(&path, &head), AnchorStatus::Intact);

        // Tail removed at an entry boundary: still a valid chain (D14) —
        // only the anchor reveals it.
        let text = read_to_string(&path).unwrap();
        let kept: String = text
            .lines()
            .take(2)
            .map(|line| format!("{line}\n"))
            .collect();
        fs::write(&path, &kept).unwrap();
        assert!(verify_chained_jsonl(&path).is_ok());
        assert_eq!(
            check_lane_anchor(&path, &head),
            AnchorStatus::Truncated { now: 2 }
        );

        // Rewritten with as many entries: the anchored entry differs.
        let _ = fs::remove_file(&path);
        for kind in ["x", "y", "z", "w"] {
            append_chained_jsonl_keyed(&path, &format!(r#"{{"kind":"{kind}"}}"#), None).unwrap();
        }
        assert_eq!(check_lane_anchor(&path, &head), AnchorStatus::Rewritten);
        let _ = fs::remove_file(&path);
        assert_eq!(check_lane_anchor(&path, &head), AnchorStatus::Missing);
        let _ = fs::remove_dir_all(&dir);
    }

    /// D16: chain lanes are recognised by name, so a lane with a tampered
    /// first line is still verified as a chain; data files are not lanes.
    #[test]
    fn known_chain_lanes_are_recognised_by_name() {
        use super::{collect_chain_lanes, is_known_chain_lane};
        for rel in [
            "evidence/logs/hash-verify.jsonl",
            "evidence/logs/audio-level-log.jsonl",
            "evidence/logs/signoff-log.jsonl",
            "evidence/logs/e01-audit.jsonl",
            "artifacts/joined/join-log.jsonl",
            "evidence/logs/audio-level-log.1.jsonl",
            "evidence/logs/audio-level-log.12.jsonl",
        ] {
            assert!(is_known_chain_lane(rel), "{rel}");
        }
        for rel in [
            "db/videos.jsonl",
            "db/progress.jsonl",
            "marks-pending/conflicts.jsonl",
            "review/signoff.json",
            "evidence/logs/notes.jsonl",
            // R5-P10: a name that merely ends in log.jsonl is not a lane.
            "artifacts/some/new-lane-log.jsonl",
            "artifacts/clips/my-catalog.jsonl",
            "artifacts/exports/2026/backlog.jsonl",
            "evidence/logs/user-audit.jsonl",
            "evidence/logs/audio-level-log.01.jsonl",
            "evidence/logs/audio-level-log.x.jsonl",
        ] {
            assert!(!is_known_chain_lane(rel), "{rel}");
        }
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-lanes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for rel in [
            "evidence/logs/scan-log.jsonl",
            "evidence/logs/hash-verify.jsonl",
            "artifacts/carved/carve-log.jsonl",
            "evidence/logs/notes.jsonl",
            "db/progress.jsonl",
            "artifacts/exports/backlog.jsonl",
        ] {
            fs::create_dir_all(dir.join(rel).parent().unwrap()).unwrap();
            fs::write(dir.join(rel), "{}\n").unwrap();
        }
        assert_eq!(
            collect_chain_lanes(&dir),
            [
                "artifacts/carved/carve-log.jsonl",
                "evidence/logs/hash-verify.jsonl",
                "evidence/logs/scan-log.jsonl",
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// FOR-13: nothing outside the hashed bytes may ride along — not an
    /// extra field after entry_sha256 on the LAST line (which no later
    /// previous_entry_sha256 covers), not junk/blank lines after the last
    /// entry, not CR bytes, not a duplicate chain key.
    #[test]
    fn content_not_covered_by_entry_hash_fails_verification() {
        use super::verify_chained_jsonl_text;
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-junk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        append_chained_jsonl_keyed(&path, r#"{"kind":"one"}"#, None).unwrap();
        append_chained_jsonl_keyed(&path, r#"{"kind":"two"}"#, None).unwrap();
        let good = read_to_string(&path).unwrap();
        assert_eq!(
            verify_chained_jsonl_text(&good, "t", &[]).unwrap().entries,
            2
        );

        let last_with_extra_field = format!(
            "{},\"examiner\":\"forged\"}}\n",
            good.trim_end().strip_suffix('}').unwrap()
        );
        let tampered = [
            ("extra field after entry_sha256", last_with_extra_field),
            ("junk line", format!("{good}garbage\n")),
            ("blank line", format!("{good}\n")),
            ("whitespace line", format!("{good}   \n")),
            ("trailing json object", format!("{good}{{}}\n")),
            ("CR before newline", good.replace('\n', "\r\n")),
        ];
        for (label, text) in tampered {
            let err = verify_chained_jsonl_text(&text, "t", &[]).unwrap_err();
            assert!(err.contains("audit line"), "{label}: {err}");
        }

        // Keyed trailer: same rule, the key id must be the canonical form.
        let key = test_key("k1", 0x33);
        let keyed_path = dir.join("keyed.jsonl");
        append_chained_jsonl_keyed(&keyed_path, r#"{"kind":"one"}"#, Some(&key)).unwrap();
        let keyed = read_to_string(&keyed_path).unwrap();
        assert!(verify_chained_jsonl_text(&keyed, "t", std::slice::from_ref(&key)).is_ok());
        let keyed_extra = keyed.replace("\"}\n", "\",\"x\":1}\n");
        assert!(verify_chained_jsonl_text(&keyed_extra, "t", std::slice::from_ref(&key)).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// R7-I3: a keyed lane whose tail was re-written under an invented key
    /// id after lines this PC authenticated is NOT VERIFIED ("재작성 배제
    /// 불가"), never covered by the "not a tampering verdict" wording.
    #[test]
    fn unknown_key_after_authenticated_lines_is_rewrite_not_excluded() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-r7i3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let mine = test_key("mine", 0x21);
        let bogus = test_key("bogus-id", 0x22);
        append_chained_jsonl_keyed(&path, r#"{"kind":"one"}"#, Some(&mine)).unwrap();
        append_chained_jsonl_keyed(&path, r#"{"kind":"forged"}"#, Some(&bogus)).unwrap();
        let verification = verify_chained_jsonl_keyed(&path, std::slice::from_ref(&mine)).unwrap();
        let reason = verification.rewrite_not_excluded.clone().expect("flagged");
        assert!(reason.contains("재작성 배제 불가"), "{reason}");
        assert!(reason.contains("\"bogus-id\""), "{reason}");
        assert!(
            !verification
                .warnings
                .iter()
                .any(|w| w.contains("not a tampering verdict")),
            "{:?}",
            verification.warnings
        );
        assert_eq!(verification.integrity, AuditIntegrity::StructuralOnly);
        // Even an in-log rotation marker naming the new id does not excuse
        // it (the marker is signed by the same unknown key).
        append_chained_jsonl_keyed(
            &path,
            r#"{"kind":"audit-key-rotate","from":"mine","to":"bogus-id"}"#,
            Some(&bogus),
        )
        .unwrap();
        let verification = verify_chained_jsonl_keyed(&path, std::slice::from_ref(&mine)).unwrap();
        assert!(verification.rewrite_not_excluded.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    /// R7-I4: the legacy shared id "default" names no particular key. A
    /// "default" entry from ANOTHER PC (whose key also defaulted to
    /// "default") is not verifiable here — not a FAIL — while a "default"
    /// entry this PC's key cannot verify in a log whose other "default"
    /// entries it DID verify is still a forgery. New ids are fingerprints.
    #[test]
    fn legacy_default_key_id_is_compared_by_fingerprint() {
        use super::verify_chained_jsonl_text;
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-r7i4-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // This PC's key, now loaded under its fingerprint id.
        let mine = AuditKey::new(&crate::audit_key::fingerprint_id(&[0x31; 32]), &[0x31; 32]);
        assert!(
            mine.id.starts_with("kid-") && mine.id.len() == 12,
            "{}",
            mine.id
        );
        let other_pc = test_key("default", 0x32);
        let foreign = dir.join("foreign.jsonl");
        append_chained_jsonl_keyed(&foreign, r#"{"kind":"one"}"#, Some(&other_pc)).unwrap();
        let verification =
            verify_chained_jsonl_keyed(&foreign, std::slice::from_ref(&mine)).unwrap();
        assert_eq!(verification.entry_auth, [EntryAuth::UnknownKey]);
        // Even a key literally named "default" here (an old keyring) does
        // not make another PC's "default" entry a FAIL.
        let mine_named_default = test_key("default", 0x31);
        let verification =
            verify_chained_jsonl_keyed(&foreign, std::slice::from_ref(&mine_named_default))
                .unwrap();
        assert_eq!(verification.entry_auth, [EntryAuth::UnknownKey]);
        // This PC's own old "default" log stays verifiable by fingerprint.
        let own = dir.join("own.jsonl");
        let mine_old = test_key("default", 0x31);
        append_chained_jsonl_keyed(&own, r#"{"kind":"one"}"#, Some(&mine_old)).unwrap();
        append_chained_jsonl_keyed(&own, r#"{"kind":"two"}"#, Some(&mine_old)).unwrap();
        let verification = verify_chained_jsonl_keyed(&own, std::slice::from_ref(&mine)).unwrap();
        assert_eq!(verification.integrity, AuditIntegrity::Keyed);
        // R8-I7: a "default" line in it that no held key verifies is
        // ANOTHER PC's legacy "default" key (a log two PCs continued) — not
        // authenticable here, never a FAIL; the lane is NOT VERIFIED (P0)
        // and names the key to check it with (P2).
        let text = read_to_string(&own).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        let start = lines[1].find("\"entry_hmac_sha256\":\"").unwrap() + 21;
        lines[1].replace_range(start..start + 64, &"0".repeat(64));
        let forged = format!("{}\n", lines.join("\n"));
        let mixed = verify_chained_jsonl_text(&forged, "t", std::slice::from_ref(&mine)).unwrap();
        assert_eq!(
            mixed.entry_auth,
            [EntryAuth::Authenticated, EntryAuth::UnknownKey]
        );
        assert_eq!(
            mixed.line_trust(1),
            super::LineTrust::OtherPc {
                key_id: "default".to_string()
            }
        );
        let trust = mixed.lane_trust();
        assert!(!trust.passes(), "{trust:?}");
        assert!(
            trust.reason().contains("\"default\" 보유 PC에서 확인"),
            "{trust:?}"
        );
        // A fingerprint id names exactly that material.
        let fp = dir.join("fp.jsonl");
        append_chained_jsonl_keyed(
            &fp,
            r#"{"kind":"one"}"#,
            Some(&AuditKey::new(&mine.id, &[0x33; 32])),
        )
        .unwrap();
        let err = verify_chained_jsonl_keyed(&fp, std::slice::from_ref(&mine)).unwrap_err();
        assert!(err.contains("HMAC verification failed"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// R7-I9: the display-string fallback refuses an ambiguous match (two
    /// records showing the same path with different hashes) instead of
    /// taking the first one's hash.
    #[test]
    fn indexed_source_hash_refuses_ambiguous_display_paths() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-r7i9-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("db")).unwrap();
        let row = |id: &str, path: &str, sha: &str| {
            serde_json::json!({"id": id, "source_path": path, "relative_path": "cam/a.mp4",
                "sha256": sha})
            .to_string()
        };
        fs::write(
            dir.join("db/videos.jsonl"),
            format!(
                "{}\n{}\n",
                row("vid_000001", "/media/sd1/cam/a.mp4", &"1".repeat(64)),
                row("vid_000002", "/media/sd2/cam/a.mp4", &"2".repeat(64))
            ),
        )
        .unwrap();
        let elsewhere = Path::new("/nowhere/a.mp4");
        assert_eq!(
            super::indexed_source_hash(&dir, "cam/a.mp4", elsewhere),
            None,
            "two records share the display path"
        );
        assert_eq!(
            super::indexed_source_hash(&dir, "/media/sd2/cam/a.mp4", elsewhere),
            Some("2".repeat(64))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// R7-I10: the longest verifying prefix of a lane with a torn tail or
    /// an edited line.
    #[test]
    fn verified_prefix_stops_before_the_damage() {
        let dir =
            std::env::temp_dir().join(format!("frametrace-audit-r7i10-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        for n in 0..3 {
            append_chained_jsonl_keyed(&path, &format!(r#"{{"n":{n}}}"#), None).unwrap();
        }
        let good = read_to_string(&path).unwrap();
        assert_eq!(super::verified_prefix(&good, &[]), (3, good.len()));
        let torn = format!("{good}{{\"n\":3,\"previous");
        assert_eq!(super::verified_prefix(&torn, &[]), (3, good.len()));
        let edited = good.replacen("\"n\":1", "\"n\":9", 1);
        let first_len = good.find('\n').unwrap() + 1;
        assert_eq!(super::verified_prefix(&edited, &[]), (1, first_len));
        let _ = fs::remove_dir_all(&dir);
    }

    fn r8_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ft-audit-r8-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// R8 P0/P1: whether a lane must be authenticated comes from THIS PC
    /// holding keys. A keyed lane re-chained with every HMAC stripped is a
    /// valid chain — R7 graded it "structural" (a PASS) on a PC holding the
    /// key; now it is NOT VERIFIED there, and only self-consistency on a PC
    /// without keys.
    #[test]
    fn r8_stripped_lane_is_never_trusted_where_keys_are_held() {
        use super::{AuthRequirement, LaneTrust, LineTrust};
        let dir = r8_dir("strip");
        let mine = test_key("mine", 0x71);
        let genuine = dir.join("genuine.jsonl");
        let stripped = dir.join("stripped.jsonl");
        for body in [r#"{"kind":"one"}"#, r#"{"kind":"two"}"#] {
            append_chained_jsonl_keyed(&genuine, body, Some(&mine)).unwrap();
            append_chained_jsonl_keyed(&stripped, body, None).unwrap();
        }
        let keys = std::slice::from_ref(&mine);
        let ok = verify_chained_jsonl_keyed(&genuine, keys).unwrap();
        assert_eq!(ok.lane_trust(), LaneTrust::Authenticated);
        let v = verify_chained_jsonl_keyed(&stripped, keys).unwrap();
        assert_eq!(v.requirement, AuthRequirement::Required);
        assert_eq!(
            v.line_trust(0),
            LineTrust::NotAuthenticated {
                retired_unproven: false
            }
        );
        let trust = v.lane_trust();
        assert!(!trust.passes(), "{trust:?}");
        assert_eq!(trust.status(), "not-verified");
        assert!(trust.reason().contains("HMAC 없는 줄 2개"), "{trust:?}");
        // P1: on a PC without keys the same lane is self-consistency only.
        let keyless = verify_chained_jsonl_keyed(&stripped, &[]).unwrap();
        assert_eq!(keyless.requirement, AuthRequirement::SelfConsistentOnly);
        let trust = keyless.lane_trust();
        assert!(trust.passes());
        assert!(trust.reason().contains("자기 일관성만 확인(인증 아님)"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// R8 P2: a lane continued on another PC (its own key) is NOT VERIFIED
    /// on each PC — never a FAIL — and names the key to check the other
    /// lines with, on both PCs.
    #[test]
    fn r8_multi_pc_lane_is_not_authenticable_here_never_failed() {
        use super::LineTrust;
        let dir = r8_dir("multi");
        let a = test_key(&crate::audit_key::fingerprint_id(&[0x73; 32]), 0x73);
        let b = test_key(&crate::audit_key::fingerprint_id(&[0x74; 32]), 0x74);
        let lane = dir.join("lane.jsonl");
        append_chained_jsonl_keyed(&lane, r#"{"kind":"a"}"#, Some(&a)).unwrap();
        append_chained_jsonl_keyed(&lane, r#"{"kind":"b"}"#, Some(&b)).unwrap();
        for (here, other, other_line) in [(&a, &b, 1usize), (&b, &a, 0usize)] {
            let v = verify_chained_jsonl_keyed(&lane, std::slice::from_ref(here)).unwrap();
            assert_eq!(
                v.line_trust(other_line),
                LineTrust::OtherPc {
                    key_id: other.id.clone()
                }
            );
            let trust = v.lane_trust();
            assert!(!trust.passes(), "{trust:?}");
            assert!(
                trust
                    .reason()
                    .contains(&format!("\"{}\" 보유 PC에서 확인", other.id)),
                "{trust:?}"
            );
            match trust {
                super::LaneTrust::NotVerified {
                    other_pc_key_ids, ..
                } => assert_eq!(other_pc_key_ids, std::slice::from_ref(&other.id)),
                other => panic!("{other:?}"),
            }
        }
        // A PC holding both keys authenticates the whole lane.
        let both = verify_chained_jsonl_keyed(&lane, &[a, b]).unwrap();
        assert_eq!(both.lane_trust(), super::LaneTrust::Authenticated);
        let _ = fs::remove_dir_all(&dir);
    }

    /// R8-I8: entries before a lane's authenticated key-introduction marker
    /// (its first keyed entry) are self-consistency only — not "forged" —
    /// and that prefix cannot be manufactured: a marker this PC did not
    /// sign, a rotation marker, or an earlier keyed entry introduces
    /// nothing, and a line slipped in before the marker breaks the chain.
    #[test]
    fn r8_i8_key_introduction_marker_bounds_the_pre_key_prefix() {
        use super::{LaneTrust, LineTrust};
        let dir = r8_dir("prekey");
        let mine = test_key("mine", 0x75);
        let other = test_key("other-pc", 0x76);
        let introduced = format!(r#"{{"kind":"{}","to":"mine"}}"#, super::KEY_INTRODUCED_KIND);
        let lane = dir.join("lane.jsonl");
        append_chained_jsonl_keyed(&lane, r#"{"kind":"old1"}"#, None).unwrap();
        append_chained_jsonl_keyed(&lane, r#"{"kind":"old2"}"#, None).unwrap();
        append_chained_jsonl_keyed(&lane, &introduced, Some(&mine)).unwrap();
        append_chained_jsonl_keyed(&lane, r#"{"kind":"new"}"#, Some(&mine)).unwrap();
        let v = verify_chained_jsonl_keyed(&lane, std::slice::from_ref(&mine)).unwrap();
        assert_eq!(v.pre_key_entries, 2);
        assert_eq!(v.line_trust(0), LineTrust::PreKey);
        assert_eq!(v.line_trust(3), LineTrust::Authenticated);
        let trust = v.lane_trust();
        assert!(
            matches!(&trust, LaneTrust::SelfConsistentOnly(why) if why.contains("도입 이전 항목 2개")),
            "{trust:?}"
        );
        // Another PC (not holding `mine`): no pre-key prefix there.
        let there = verify_chained_jsonl_keyed(&lane, std::slice::from_ref(&other)).unwrap();
        assert_eq!(there.pre_key_entries, 0);
        assert!(!there.lane_trust().passes());
        // A line inserted before the marker breaks the chain.
        let text = read_to_string(&lane).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        let first = lines[0].to_string();
        lines.insert(1, &first);
        let spliced = format!("{}\n", lines.join("\n"));
        assert!(
            super::verify_chained_jsonl_text(&spliced, "t", std::slice::from_ref(&mine)).is_err()
        );
        // A rotation marker (not an introduction) introduces nothing.
        let rotated = dir.join("rotated.jsonl");
        append_chained_jsonl_keyed(&rotated, r#"{"kind":"old"}"#, None).unwrap();
        append_chained_jsonl_keyed(
            &rotated,
            r#"{"kind":"audit-key-rotate","from":null,"to":"mine"}"#,
            Some(&mine),
        )
        .unwrap();
        let v = verify_chained_jsonl_keyed(&rotated, std::slice::from_ref(&mine)).unwrap();
        assert_eq!(v.pre_key_entries, 0);
        assert!(!v.lane_trust().passes());
        // Nor does an introduction that is not the first keyed entry.
        let late = dir.join("late.jsonl");
        append_chained_jsonl_keyed(&late, r#"{"kind":"k"}"#, Some(&mine)).unwrap();
        append_chained_jsonl_keyed(&late, r#"{"kind":"stripped"}"#, None).unwrap();
        append_chained_jsonl_keyed(&late, &introduced, Some(&mine)).unwrap();
        let v = verify_chained_jsonl_keyed(&late, std::slice::from_ref(&mine)).unwrap();
        assert_eq!(v.pre_key_entries, 0);
        assert!(!v.lane_trust().passes());
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
#[path = "tests_audit_r9.rs"]
mod tests_r9;
