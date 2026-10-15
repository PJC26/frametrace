//! Audit-chain HMAC key resolution (design: `docs/audit-hmac-design.md`).
//!
//! Keyed mode is opt-in: with no configured key, `configured()` returns
//! `Ok(None)` and audit appends write exactly the same unkeyed entries they
//! always did. Sources, in the design's order of preference — the first
//! available wins for appends; verification tries ALL of them:
//!
//! 1. A `--key-source` override, installed once at CLI start:
//!    `env:VAR_NAME` reads hex/base64 material straight from that variable
//!    (the documented hand-off for an external secret store — the key never
//!    touches disk) and `file:PATH` reads a key file at an explicit path.
//! 2. The rotation keyring `audit-keys.json`, written by
//!    `rotate-audit-key`, next to the single-key file;
//!    `FRAMETRACE_AUDIT_KEYRING_FILE` overrides the path. Its `active` key
//!    signs new appends; every other entry is a retired key kept so older
//!    entries stay verifiable.
//! 3. A `0600` key file (the design's OS-store fallback for platforms
//!    without a wired keystore). Default location is
//!    `$XDG_CONFIG_HOME/frametrace/audit-key` or `~/.config/frametrace/
//!    audit-key` on Unix and `%APPDATA%\frametrace\audit-key` on Windows;
//!    `FRAMETRACE_AUDIT_KEY_FILE` overrides the path (CI, portable
//!    installs). The file must be hex- or base64-encoded 32-byte key
//!    material and, on Unix, must have no group/other permission bits —
//!    a world-readable key defeats the control, so it is refused.
//! 4. `FRAMETRACE_AUDIT_KEY` — the same encodings, documented as the
//!    weakest source: any process running as the examiner can read it.
//!
//! `FRAMETRACE_AUDIT_KEY_ID` names whichever key was loaded (default: its
//! fingerprint id `kid-<sha8>`, R7-I4) and is stamped as `entry_hmac_key_id`. The key never
//! lives inside the case directory — that would defeat the control
//! entirely.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A configured audit-chain key: key material plus the non-secret
/// identifier stamped into each keyed entry.
#[derive(Debug, Clone)]
pub struct AuditKey {
    pub id: String,
    pub bytes: Vec<u8>,
    /// D15: when this key was retired by a rotation (keyring `retired`
    /// map). A retired key only authenticates entries that predate its
    /// retirement. `Some(u64::MAX)` = retired at an unrecorded time (a
    /// non-active key of a keyring written before retirement times were
    /// kept); `None` = not retired.
    pub retired_unix: Option<u64>,
}

impl AuditKey {
    /// Builds a key for tests/tools that already hold raw material.
    pub fn new(id: &str, bytes: &[u8]) -> Self {
        Self {
            id: id.to_string(),
            bytes: bytes.to_vec(),
            retired_unix: None,
        }
    }

    /// The same key marked as retired at `unix` (tests and tools).
    pub fn retired_at(mut self, unix: u64) -> Self {
        self.retired_unix = Some(unix);
        self
    }
}

/// The key set verification uses: the entry's declared `key_id` is tried
/// first and every known key is tried as a fallback (a renamed id must
/// never brick old entries — see the design's rotation rules). The set is
/// the union of every configured source — keyring (active + retired),
/// single-key file, env var, and any `--key-source` override — deduplicated
/// on (id, material).
pub fn verification_keys() -> Result<Vec<AuditKey>, String> {
    verification_keys_raw().map_err(|err| crate::audit::key_config_error_note(&err))
}

fn verification_keys_raw() -> Result<Vec<AuditKey>, String> {
    #[cfg(test)]
    if let Some(keys) = TEST_KEYS.with(|keys| keys.borrow().clone()) {
        return Ok(normalize_retired_aliases(&keys));
    }
    let mut keys: Vec<AuditKey> = SOURCE_OVERRIDE.get().cloned().into_iter().collect();
    let extras = keyring_keys()?
        .into_iter()
        .chain([from_key_file()?, from_env()?].into_iter().flatten());
    for key in extras {
        if !keys
            .iter()
            .any(|known| known.id == key.id && known.bytes == key.bytes)
        {
            keys.push(key);
        }
    }
    Ok(normalize_retired_aliases(&keys))
}

/// R5-P8: a fingerprint of the verification key set — changes whenever a
/// key is added, removed, renamed or retired — for caches whose content
/// depends on which entries verify (the sound-level map). It is a SHA-256
/// over each key's id, retirement and the SHA-256 of its material, so no
/// key material is exposed. A key-source error yields a distinct value
/// (the error text), never the fingerprint of an empty set.
pub fn verification_fingerprint() -> String {
    match verification_keys() {
        Ok(keys) => key_set_fingerprint(&keys),
        Err(err) => format!("error:{err}"),
    }
}

/// Pure core of [`verification_fingerprint`]: order-independent.
pub fn key_set_fingerprint(keys: &[AuditKey]) -> String {
    let mut parts: Vec<String> = keys
        .iter()
        .map(|key| {
            format!(
                "{}\u{1}{:?}\u{1}{}",
                key.id,
                key.retired_unix,
                crate::sha256::digest_bytes(&key.bytes)
            )
        })
        .collect();
    parts.sort();
    crate::sha256::digest_bytes(parts.join("\u{2}").as_bytes())
}

/// R5-P7: the same key material supplied by more than one source (a
/// `--key-source` override, the key file, the env var) is ONE key. When the
/// keyring retired that material, every alias carries the keyring's id and
/// retirement — otherwise a retired secret handed in again through
/// `--key-source` (stamped `default`, not retired) would out-rank the
/// keyring's retirement record. Order is kept; duplicates collapse.
pub fn normalize_retired_aliases(keys: &[AuditKey]) -> Vec<AuditKey> {
    let mut out: Vec<AuditKey> = Vec::with_capacity(keys.len());
    for key in keys {
        // The earliest retirement recorded for this material wins.
        let retired = keys
            .iter()
            .filter(|other| other.bytes == key.bytes && other.retired_unix.is_some())
            .min_by_key(|other| other.retired_unix);
        let normalized = match retired {
            Some(retired) => AuditKey {
                id: retired.id.clone(),
                bytes: key.bytes.clone(),
                retired_unix: retired.retired_unix,
            },
            None => key.clone(),
        };
        if !out
            .iter()
            .any(|known| known.id == normalized.id && known.bytes == normalized.bytes)
        {
            out.push(normalized);
        }
    }
    out
}

/// R5-P7: refuses an append key that the keyring has retired. Entries it
/// signed would only fail verification later (or read as forged), so the
/// mistake is reported now instead.
fn refuse_retired_append_key(key: AuditKey) -> Result<AuditKey, String> {
    refuse_retired_key(key, &keyring_keys()?)
}

/// Pure core of [`refuse_retired_append_key`] against a loaded keyring.
fn refuse_retired_key(key: AuditKey, keyring: &[AuditKey]) -> Result<AuditKey, String> {
    let retired = keyring
        .iter()
        .find(|known| known.bytes == key.bytes && known.retired_unix.is_some());
    match retired {
        Some(retired) => Err(format!(
            "the configured audit key is the keyring's retired key \"{}\" — it may no longer sign audit entries; use the keyring's active key (or rotate again)",
            retired.id
        )),
        None => Ok(key),
    }
}

/// Resolves the configured append key, if any. Absent sources are not an
/// error; a *present but unusable* source (bad permissions, malformed
/// material, wrong length) is, so a misconfigured key never silently
/// degrades an intended-keyed log to unkeyed.
pub fn configured() -> Result<Option<AuditKey>, String> {
    configured_with(false)
}

fn configured_with(missing_explicit_keyring_ok: bool) -> Result<Option<AuditKey>, String> {
    #[cfg(test)]
    if let Some(keys) = TEST_KEYS.with(|keys| keys.borrow().clone()) {
        return Ok(keys.into_iter().find(|key| key.retired_unix.is_none()));
    }
    if let Some(key) = SOURCE_OVERRIDE.get() {
        return refuse_retired_append_key(key.clone()).map(Some);
    }
    if let Some(key) = keyring_active_with(missing_explicit_keyring_ok)? {
        return Ok(Some(key));
    }
    if let Some(key) = from_key_file()? {
        return Ok(Some(key));
    }
    from_env()
}

#[cfg(test)]
thread_local! {
    /// Test-only: the key set this thread's `verification_keys()` /
    /// `configured()` return instead of the ambient sources, so consumer
    /// tests can run "this PC holds keys" without touching the real config
    /// or process environment ([`with_test_keys`]).
    static TEST_KEYS: std::cell::RefCell<Option<Vec<AuditKey>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only: runs `f` with `keys` as this thread's audit key set (the
/// first non-retired key appends). An empty slice = "no keys on this PC".
#[cfg(test)]
pub(crate) fn with_test_keys<R>(keys: &[AuditKey], f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Vec<AuditKey>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            TEST_KEYS.with(|keys| *keys.borrow_mut() = previous);
        }
    }
    let _restore = Restore(TEST_KEYS.with(|current| current.replace(Some(keys.to_vec()))));
    f()
}

/// Process-wide `--key-source` override. The CLI installs it once before
/// dispatch; it wins over every ambient source so a secret-manager hand-off
/// is authoritative for that invocation.
static SOURCE_OVERRIDE: OnceLock<AuditKey> = OnceLock::new();

/// Installs the `--key-source` override for this process. Called once at
/// CLI start; a second call is a bug, not a user error.
pub fn install_source_override(spec: &str) -> Result<(), String> {
    let key = resolve_key_source(spec)?;
    SOURCE_OVERRIDE
        .set(key)
        .map_err(|_| "audit key source override was already installed".to_string())
}

/// Resolves a `--key-source` spec without installing it. `env:VAR` reads
/// material from that variable (absent/empty is an error — the user named
/// the source explicitly), `file:PATH` reads a `0600` key file at PATH.
fn resolve_key_source(spec: &str) -> Result<AuditKey, String> {
    match spec.split_once(':') {
        Some(("env", var)) if !var.trim().is_empty() => {
            let var = var.trim();
            key_from_env(var)?.ok_or_else(|| format!("--key-source env:{var}: {var} is not set"))
        }
        Some(("file", path)) if !path.trim().is_empty() => {
            let path = Path::new(path.trim());
            if !path.is_file() {
                return Err(format!(
                    "--key-source file:{}: no such file",
                    path.display()
                ));
            }
            read_key_file(path)
        }
        _ => Err("--key-source must be `env:VAR_NAME` or `file:PATH`".to_string()),
    }
}

/// R7-I4: the id a key file / env key is stamped with —
/// `FRAMETRACE_AUDIT_KEY_ID` when set, otherwise the key's fingerprint id
/// ([`fingerprint_id`]). It used to default to the shared literal
/// `"default"`, so two PCs' different keys claimed the same id and one
/// PC's entries read as forged on the other.
fn configured_key_id(bytes: &[u8]) -> String {
    std::env::var("FRAMETRACE_AUDIT_KEY_ID")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| fingerprint_id(bytes))
}

/// R7-I4: the legacy default key id every unnamed key used to carry. It
/// names no particular key; verification binds it per log by fingerprint
/// (see `audit::verify_chained_jsonl_text`).
pub const LEGACY_DEFAULT_KEY_ID: &str = "default";

/// R7-I4: `kid-<first 8 hex of SHA-256(material)>` — a non-secret id
/// derived from the key itself, so different keys never share an id by
/// accident.
pub fn fingerprint_id(bytes: &[u8]) -> String {
    format!("kid-{}", &crate::sha256::digest_bytes(bytes)[..8])
}

/// R7-I4: whether `key` is the key an entry's declared `key_id` names —
/// i.e. a mismatch under it is a forgery, not "another PC's key". A
/// fingerprint id (`kid-…`) names exactly the material with that
/// fingerprint (whatever id this PC loaded it under); the legacy
/// `"default"` names no key at all; any other id is compared as text
/// (keyring ids such as `2026-Q1`).
pub fn key_answers_to(key: &AuditKey, declared: &str) -> bool {
    if declared == LEGACY_DEFAULT_KEY_ID {
        return false;
    }
    if is_fingerprint_id(declared) {
        return fingerprint_id(&key.bytes) == declared;
    }
    key.id == declared
}

fn is_fingerprint_id(id: &str) -> bool {
    id.strip_prefix("kid-").is_some_and(|hex| {
        hex.len() == 8
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn from_key_file() -> Result<Option<AuditKey>, String> {
    let Some(path) = key_file_path() else {
        return Ok(None);
    };
    if !probe_key_path(&path, "감사 키 파일", explicit_var(KEY_FILE_ENV))? {
        return Ok(None);
    }
    read_key_file(&path).map(Some)
}

/// Reads and validates a single-key file — `0600` on Unix, hex/base64
/// 32-byte material.
fn read_key_file(path: &Path) -> Result<AuditKey, String> {
    check_key_file_permissions(path)?;
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read audit key file {}: {err}", path.display()))?;
    let bytes = parse_key_material(text.trim())
        .map_err(|err| format!("audit key file {}: {err}", path.display()))?;
    Ok(AuditKey {
        id: configured_key_id(&bytes),
        bytes,
        retired_unix: None,
    })
}

fn from_env() -> Result<Option<AuditKey>, String> {
    key_from_env("FRAMETRACE_AUDIT_KEY")
}

/// Reads hex/base64 key material from an environment variable. Absent is
/// `Ok(None)`; present-but-unusable (empty, non-unicode, malformed) is an
/// error — a misconfigured source never silently degrades to unkeyed.
fn key_from_env(var: &str) -> Result<Option<AuditKey>, String> {
    match std::env::var(var) {
        Ok(raw) if !raw.trim().is_empty() => {
            let bytes = parse_key_material(raw.trim()).map_err(|err| format!("{var}: {err}"))?;
            Ok(Some(AuditKey {
                id: configured_key_id(&bytes),
                bytes,
                retired_unix: None,
            }))
        }
        Ok(_) => Err(format!("{var} is set but empty")),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{var} is not valid unicode")),
    }
}

fn key_file_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("FRAMETRACE_AUDIT_KEY_FILE") {
        let path = path.trim();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    config_dir().map(|dir| dir.join("audit-key"))
}

/// `<config>/frametrace/` — XDG on Unix, `%APPDATA%` on Windows.
pub(crate) fn config_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Ok(base) = std::env::var("XDG_CONFIG_HOME")
            && !base.trim().is_empty()
        {
            return Some(PathBuf::from(base).join("frametrace"));
        }
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".config/frametrace"))
    }
    #[cfg(windows)]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|base| PathBuf::from(base).join("frametrace"))
    }
}

/// Unix: the key file must not be readable by group or other — a key an
/// attacker can copy is a key an attacker can sign with. Windows relies on
/// the profile ACL and cannot express mode bits, so there is nothing to
/// check there.
#[cfg(unix)]
fn check_key_file_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|err| format!("failed to stat audit key file {}: {err}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "audit key file {} has permissions {:o}; run `chmod 600` so only the owner can read it",
            path.display(),
            mode & 0o777
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_key_file_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// On-disk keyring format (version 1). `active` names the id new entries
/// are stamped with; every other id is a retired key kept so its entries
/// stay verifiable — the design's rotation rules.
#[derive(serde::Serialize, serde::Deserialize)]
struct KeyringFile {
    version: u32,
    active: String,
    keys: BTreeMap<String, String>,
    /// D15: retirement time (unix seconds) of every retired key id. Absent
    /// in keyrings written before retirement times were recorded; their
    /// non-active keys load as retired at an unrecorded time.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    retired: BTreeMap<String, u64>,
}

/// A loaded keyring for callers that need the active id alongside the keys.
#[derive(Debug)]
pub struct Keyring {
    pub active: String,
    pub keys: Vec<AuditKey>,
}

/// Outcome of `rotate`/`rotate_at`.
#[derive(Debug)]
pub struct Rotation {
    /// The freshly generated key — now the keyring's active key.
    pub new_key: AuditKey,
    /// The previously configured key's id (now retired), or `None` when
    /// rotation enabled keying from scratch.
    pub previous_id: Option<String>,
    /// Where the keyring was written.
    pub keyring_path: PathBuf,
}

fn keyring_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("FRAMETRACE_AUDIT_KEYRING_FILE") {
        let path = path.trim();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    config_dir().map(|dir| dir.join("audit-keys.json"))
}

/// Reads a keyring file. Absent is `Ok(None)`; present-but-broken is an
/// error, matching the single-key file's present-but-unusable rule.
#[doc(hidden)]
pub fn load_keyring(path: &Path) -> Result<Option<Keyring>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    check_key_file_permissions(path)?;
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read audit keyring {}: {err}", path.display()))?;
    let parsed: KeyringFile = serde_json::from_str(&text).map_err(|err| {
        format!(
            "audit keyring {} is not valid keyring JSON: {err}",
            path.display()
        )
    })?;
    if parsed.version != 1 {
        return Err(format!(
            "audit keyring {} has unsupported version {}",
            path.display(),
            parsed.version
        ));
    }
    if !parsed.keys.contains_key(&parsed.active) {
        return Err(format!(
            "audit keyring {} names active key \"{}\" it does not contain",
            path.display(),
            parsed.active
        ));
    }
    let mut keys = Vec::with_capacity(parsed.keys.len());
    for (id, material) in &parsed.keys {
        let bytes = parse_key_material(material)
            .map_err(|err| format!("audit keyring {}: key \"{id}\": {err}", path.display()))?;
        let retired_unix = if *id == parsed.active {
            None
        } else {
            Some(parsed.retired.get(id).copied().unwrap_or(u64::MAX))
        };
        keys.push(AuditKey {
            id: id.clone(),
            bytes,
            retired_unix,
        });
    }
    Ok(Some(Keyring {
        active: parsed.active,
        keys,
    }))
}

fn keyring_keys() -> Result<Vec<AuditKey>, String> {
    let Some(path) = keyring_path() else {
        return Ok(Vec::new());
    };
    if !probe_key_path(&path, "감사 키링", explicit_var(KEYRING_ENV))? {
        return Ok(Vec::new());
    }
    Ok(load_keyring(&path)?.map(|kr| kr.keys).unwrap_or_default())
}

/// `missing_explicit_ok`: rotation creates the keyring an explicit
/// `FRAMETRACE_AUDIT_KEYRING_FILE` names, so it may not exist yet there.
fn keyring_active_with(missing_explicit_ok: bool) -> Result<Option<AuditKey>, String> {
    let Some(path) = keyring_path() else {
        return Ok(None);
    };
    let explicit = if missing_explicit_ok {
        None
    } else {
        explicit_var(KEYRING_ENV)
    };
    if !probe_key_path(&path, "감사 키링", explicit)? {
        return Ok(None);
    }
    // load_keyring guarantees `active` names a contained id.
    Ok(load_keyring(&path)?.and_then(|kr| kr.keys.into_iter().find(|key| key.id == kr.active)))
}

/// R9 P5/I9: the prefix of every error about an audit key configuration
/// that exists but cannot be read. It is NOT a tamper verdict — consumers
/// show it as "키 설정을 읽지 못함" and fail closed (keys held, nothing
/// authenticated), never as a broken chain.
pub const KEY_CONFIG_UNREADABLE: &str = "키 설정을 읽지 못함";

const KEY_FILE_ENV: &str = "FRAMETRACE_AUDIT_KEY_FILE";
const KEYRING_ENV: &str = "FRAMETRACE_AUDIT_KEYRING_FILE";

/// The variable when it is set (non-blank) — the path was configured
/// explicitly, so its absence is an error, not "no key".
fn explicit_var(var: &'static str) -> Option<&'static str> {
    std::env::var(var)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|_| var)
}

/// R9 P5: whether a key path holds a key file to read. `Ok(false)` only
/// when nothing is there and the path was NOT configured explicitly (the
/// default location, "no key"); a path configured through `explicit_var`
/// that does not exist, a directory, a broken link, or anything else that
/// cannot be read is an error (fail closed) — R8 read these as "no key",
/// silently relaxing P0 to P1.
pub(crate) fn probe_key_path(
    path: &Path,
    what: &str,
    explicit_var: Option<&'static str>,
) -> Result<bool, String> {
    let shown = path.display();
    let fail = |why: String| {
        Err(format!(
            "{KEY_CONFIG_UNREADABLE}: {what} {shown} — {why} (변조 판정 아님; 감사 키 설정을 고친 뒤 다시 확인하십시오)"
        ))
    };
    match std::fs::symlink_metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => match explicit_var {
            Some(var) => fail(format!("{var}로 지정된 경로에 파일이 없습니다")),
            None => Ok(false),
        },
        Err(err) => fail(format!("확인할 수 없습니다: {err}")),
        Ok(meta) => {
            let target = if meta.file_type().is_symlink() {
                match std::fs::metadata(path) {
                    Ok(target) => target,
                    Err(err) => return fail(format!("끊긴 링크입니다: {err}")),
                }
            } else {
                meta
            };
            if target.is_dir() {
                fail("파일이 아니라 폴더입니다".to_string())
            } else if !target.is_file() {
                fail("일반 파일이 아닙니다".to_string())
            } else {
                Ok(true)
            }
        }
    }
}

/// Generates a new audit key, registers it as the keyring's active key, and
/// retires the previously configured key — whichever source supplied it —
/// into the keyring so its entries stay verifiable.
pub fn rotate(requested_id: Option<&str>) -> Result<Rotation, String> {
    let path = keyring_path().ok_or_else(|| {
        "cannot rotate the audit key: no config directory resolved; set FRAMETRACE_AUDIT_KEYRING_FILE"
            .to_string()
    })?;
    rotate_at(&path, requested_id, configured_with(true)?)
}

/// `rotate` against an explicit keyring path and an explicit previous key,
/// so tests exercise rotation without process-global env state.
#[doc(hidden)]
pub fn rotate_at(
    keyring_path: &Path,
    requested_id: Option<&str>,
    previous: Option<AuditKey>,
) -> Result<Rotation, String> {
    let loaded = load_keyring(keyring_path)?;
    let mut retired: BTreeMap<String, u64> = loaded
        .as_ref()
        .map(|kr| {
            kr.keys
                .iter()
                .filter_map(|key| {
                    key.retired_unix
                        .filter(|at| *at != u64::MAX)
                        .map(|at| (key.id.clone(), at))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut keys: BTreeMap<String, Vec<u8>> = loaded
        .map(|kr| kr.keys.into_iter().map(|key| (key.id, key.bytes)).collect())
        .unwrap_or_default();

    // OS entropy via getrandom — the only source of key material, so the
    // same CSPRNG guarantee holds on every supported OS.
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|err| format!("failed to draw OS randomness for the new audit key: {err}"))?;

    let id = match requested_id {
        Some(id) => {
            let id = id.trim();
            if !valid_key_id(id) {
                return Err(format!(
                    "invalid key id \"{id}\": use non-empty text without quotes, backslashes, or control characters"
                ));
            }
            id.to_string()
        }
        // Default id is a key-fingerprint prefix — stable, non-secret.
        None => fingerprint_id(&bytes),
    };
    if keys.contains_key(&id) {
        return Err(format!(
            "audit key id \"{id}\" already exists in the keyring"
        ));
    }

    let previous_id = previous.as_ref().map(|prev| prev.id.clone());
    if let Some(prev) = previous {
        match keys.get(&prev.id) {
            Some(known) if *known == prev.bytes => {}
            Some(_) => {
                return Err(format!(
                    "keyring already holds different key material under id \"{}\"; refusing to merge",
                    prev.id
                ));
            }
            None => {
                keys.insert(prev.id, prev.bytes);
            }
        }
    }
    keys.insert(id.clone(), bytes.to_vec());
    // D15: every key but the new active one is retired now (an earlier
    // recorded retirement time is kept — the earliest cut-off wins).
    let now = crate::util::now_unix()?;
    for key_id in keys.keys() {
        if *key_id != id {
            retired.entry(key_id.clone()).or_insert(now);
        }
    }

    write_keyring(
        keyring_path,
        &KeyringFile {
            version: 1,
            active: id.clone(),
            keys: keys
                .iter()
                .map(|(id, bytes)| (id.clone(), hex_encode(bytes)))
                .collect(),
            retired,
        },
    )?;
    Ok(Rotation {
        new_key: AuditKey {
            id,
            bytes: bytes.to_vec(),
            retired_unix: None,
        },
        previous_id,
        keyring_path: keyring_path.to_path_buf(),
    })
}

fn write_keyring(path: &Path, keyring: &KeyringFile) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create audit keyring directory {}: {err}",
                parent.display()
            )
        })?;
    }
    let text = serde_json::to_string_pretty(keyring)
        .map_err(|err| format!("failed to serialize audit keyring: {err}"))?;
    // Atomic replace: the keyring holds every retired key, so a crash
    // mid-write must never leave it truncated (that would strand every
    // older keyed entry). Write + fsync a sibling temp file, then rename it
    // over the keyring — `std::fs::rename` replaces an existing target on
    // both Unix (rename(2)) and Windows (MoveFileExW REPLACE_EXISTING).
    let tmp = keyring_temp_path(path);
    let write_tmp = || -> Result<(), String> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(|err| {
            format!(
                "failed to write audit keyring temp file {}: {err}",
                tmp.display()
            )
        })?;
        file.write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|err| {
                format!(
                    "failed to write audit keyring temp file {}: {err}",
                    tmp.display()
                )
            })?;
        drop(file);
        // A stale temp file from an earlier crash keeps its old mode on
        // truncate; normalize so key material is never group/other-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(
                |err| {
                    format!(
                        "failed to restrict audit keyring {} permissions: {err}",
                        tmp.display()
                    )
                },
            )?;
        }
        std::fs::rename(&tmp, path).map_err(|err| {
            format!(
                "failed to replace audit keyring {} with {}: {err} (the previous keyring is unchanged)",
                path.display(),
                tmp.display()
            )
        })
    };
    if let Err(err) = write_tmp() {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    crate::util::sync_parent_directory(path);
    Ok(())
}

/// Sibling temp path used by [`write_keyring`]'s atomic replace (same
/// directory, so the rename never crosses filesystems).
fn keyring_temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "audit-keys.json".into());
    name.push(".tmp");
    path.with_file_name(name)
}

/// Key ids land inside JSON strings (`entry_hmac_key_id`, keyring `keys`
/// map); rejecting quotes/backslashes/controls keeps them readable
/// everywhere without leaning on escaping.
fn valid_key_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| !ch.is_control() && ch != '"' && ch != '\\')
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Decodes hex- or base64-encoded key material into exactly 32 bytes.
/// Hex is tried first (64 hex chars); anything else is tried as base64.
fn parse_key_material(text: &str) -> Result<Vec<u8>, String> {
    let decoded = if text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit()) {
        hex_decode(text)
    } else {
        base64_decode(text)
    }
    .ok_or_else(|| "key is not valid hex (64 chars) or base64".to_string())?;
    if decoded.len() != 32 {
        return Err(format!(
            "key must decode to exactly 32 bytes, got {}",
            decoded.len()
        ));
    }
    Ok(decoded)
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len() / 2)
        .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok())
        .collect()
}

pub(crate) fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    // Strip padding, then decode whole sextets. Any '=' must sit at the
    // end (enforced implicitly: '=' fails value() inside the body).
    let bytes = text.as_bytes();
    let body = if let Some(body) = bytes.strip_suffix(b"==") {
        body
    } else if let Some(body) = bytes.strip_suffix(b"=") {
        body
    } else {
        bytes
    };
    if body.is_empty() || body.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4 + 3);
    let chunks: Vec<&[u8]> = body.chunks(4).collect();
    for chunk in chunks {
        let mut acc = 0u32;
        for (slot, byte) in chunk.iter().enumerate() {
            acc |= (value(*byte)? as u32) << (18 - 6 * slot);
        }
        let decoded = acc.to_be_bytes();
        let take = match chunk.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return None,
        };
        out.extend_from_slice(&decoded[1..1 + take]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{
        AuditKey, base64_decode, hex_decode, load_keyring, parse_key_material, resolve_key_source,
        rotate_at,
    };
    use std::fs;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "frametrace-audit-key-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn decodes_hex_key() {
        let hex = "00".repeat(32);
        assert_eq!(parse_key_material(&hex).unwrap(), vec![0u8; 32]);
        let hex = "ff".repeat(32);
        assert_eq!(parse_key_material(&hex).unwrap(), vec![0xffu8; 32]);
    }

    #[test]
    fn decodes_base64_key() {
        // 32 zero bytes base64-encode to 44 chars ending '='.
        let b64 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        assert_eq!(parse_key_material(b64).unwrap(), vec![0u8; 32]);
        // Known vector: "hello world" <-> aGVsbG8gd29ybGQ=
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ").unwrap(), b"hello world");
        assert_eq!(base64_decode("aA==").unwrap(), b"h");
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
    }

    #[test]
    fn rejects_wrong_length_and_garbage() {
        assert!(parse_key_material("00").is_err());
        assert!(parse_key_material(&"00".repeat(33)).is_err());
        assert!(parse_key_material("not a key at all!!!").is_err());
        assert!(base64_decode("a").is_none());
        assert!(hex_decode("xyz").is_none());
    }

    /// The rotation contract: entries keyed before rotation verify against
    /// the retired key, entries keyed after verify against the active key,
    /// and the keyring hands verification both generations.
    #[test]
    fn rotate_then_verify_old_and_new_entries() {
        let dir = test_dir("rotate");
        let keyring_path = dir.join("audit-keys.json");
        let log = dir.join("audit.jsonl");
        let old = AuditKey::new("2026-Q1", &[0x11; 32]);

        // Pre-rotation history exists under the old key.
        crate::audit::append_chained_jsonl_keyed(&log, r#"{"kind":"before"}"#, Some(&old)).unwrap();

        let rotation = rotate_at(&keyring_path, Some("2026-Q2"), Some(old.clone())).unwrap();
        assert_eq!(rotation.new_key.id, "2026-Q2");
        assert_eq!(rotation.previous_id.as_deref(), Some("2026-Q1"));
        assert_eq!(rotation.new_key.bytes.len(), 32);
        assert_ne!(rotation.new_key.bytes, old.bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&keyring_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "keyring mode was {mode:o}");
        }

        // New entries chain on under the new key.
        crate::audit::append_chained_jsonl_keyed(
            &log,
            r#"{"kind":"after"}"#,
            Some(&rotation.new_key),
        )
        .unwrap();

        let keyring = load_keyring(&keyring_path).unwrap().unwrap();
        assert_eq!(keyring.active, "2026-Q2");
        assert_eq!(keyring.keys.len(), 2);
        let verification = crate::audit::verify_chained_jsonl_keyed(&log, &keyring.keys).unwrap();
        assert_eq!(verification.entries, 2);
        assert_eq!(verification.keyed_entries, 2);
        assert_eq!(verification.integrity, crate::audit::AuditIntegrity::Keyed);
        assert!(verification.warnings.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    /// D15: rotation records when each key was retired; the loaded keyring
    /// hands verification that cut-off, and a pre-D15 keyring (no
    /// `retired` map) loads its non-active keys as retired at an unknown
    /// time. A retired key no longer signs entries dated after rotation.
    #[test]
    fn rotation_records_retirement_and_cuts_off_the_retired_key() {
        let dir = test_dir("retired-cutoff");
        let keyring_path = dir.join("audit-keys.json");
        let old = AuditKey::new("2026-Q1", &[0x44; 32]);
        rotate_at(&keyring_path, Some("2026-Q2"), Some(old.clone())).unwrap();
        let keyring = load_keyring(&keyring_path).unwrap().unwrap();
        let retired = keyring.keys.iter().find(|k| k.id == "2026-Q1").unwrap();
        let active = keyring.keys.iter().find(|k| k.id == "2026-Q2").unwrap();
        let now = crate::util::now_unix().unwrap();
        let at = retired
            .retired_unix
            .expect("retired key carries its retirement time");
        assert!(at <= now && at + 600 > now, "{at} vs {now}");
        assert_eq!(active.retired_unix, None);
        assert!(
            fs::read_to_string(&keyring_path)
                .unwrap()
                .contains("\"retired\"")
        );

        // A forger holding the old key appends an entry (dated before the
        // rotation, R5-P7: times are not trusted): nothing newer follows it,
        // so it is never authenticated as keyed under the keyring's key set.
        let log = dir.join("audit.jsonl");
        crate::audit::append_chained_jsonl_keyed(
            &log,
            &format!(r#"{{"kind":"forged","created_unix":{}}}"#, at - 3600),
            Some(&old),
        )
        .unwrap();
        let verification = crate::audit::verify_chained_jsonl_keyed(&log, &keyring.keys).unwrap();
        assert_eq!(
            verification.integrity,
            crate::audit::AuditIntegrity::StructuralOnly
        );
        assert!(
            verification.warnings.iter().any(|w| w.contains("retired")),
            "{:?}",
            verification.warnings
        );
        // R5-P7: the retired secret handed in again (a `--key-source`
        // override) may not sign new entries; the active key may.
        let err = super::refuse_retired_key(old.clone(), &keyring.keys).unwrap_err();
        assert!(err.contains("2026-Q1") && err.contains("retired"), "{err}");
        let again = AuditKey::new("default", &[0x44; 32]);
        assert!(super::refuse_retired_key(again, &keyring.keys).is_err());
        assert!(super::refuse_retired_key(active.clone(), &keyring.keys).is_ok());

        // Pre-D15 keyring: non-active keys are retired at an unknown time.
        let legacy = dir.join("legacy-keys.json");
        fs::write(
            &legacy,
            format!(
                "{{\"version\":1,\"active\":\"b\",\"keys\":{{\"a\":\"{}\",\"b\":\"{}\"}}}}",
                "11".repeat(32),
                "22".repeat(32)
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&legacy, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let keyring = load_keyring(&legacy).unwrap().unwrap();
        let a = keyring.keys.iter().find(|k| k.id == "a").unwrap();
        assert_eq!(a.retired_unix, Some(u64::MAX));
        let _ = fs::remove_dir_all(dir);
    }

    /// Two rotations keep every generation verifiable: the keyring grows
    /// retired entries and only ever has one active key.
    #[test]
    fn second_rotation_keeps_every_generation_verifiable() {
        let dir = test_dir("rotate2");
        let keyring_path = dir.join("audit-keys.json");
        let log = dir.join("audit.jsonl");
        let first = AuditKey::new("gen1", &[0x21; 32]);

        let r1 = rotate_at(&keyring_path, Some("gen2"), Some(first.clone())).unwrap();
        let r2 = rotate_at(&keyring_path, Some("gen3"), Some(r1.new_key.clone())).unwrap();

        crate::audit::append_chained_jsonl_keyed(&log, r#"{"kind":"e1"}"#, Some(&first)).unwrap();
        crate::audit::append_chained_jsonl_keyed(&log, r#"{"kind":"e2"}"#, Some(&r1.new_key))
            .unwrap();
        crate::audit::append_chained_jsonl_keyed(&log, r#"{"kind":"e3"}"#, Some(&r2.new_key))
            .unwrap();

        let keyring = load_keyring(&keyring_path).unwrap().unwrap();
        assert_eq!(keyring.active, "gen3");
        assert_eq!(keyring.keys.len(), 3);
        let verification = crate::audit::verify_chained_jsonl_keyed(&log, &keyring.keys).unwrap();
        assert_eq!(verification.entries, 3);
        assert_eq!(verification.integrity, crate::audit::AuditIntegrity::Keyed);
        let _ = fs::remove_dir_all(dir);
    }

    /// Rotating with nothing configured enables keying from scratch: no
    /// retired key, generated fingerprint id.
    #[test]
    fn rotate_with_no_previous_key_enables_keying() {
        let dir = test_dir("rotate-fresh");
        let keyring_path = dir.join("audit-keys.json");
        let rotation = rotate_at(&keyring_path, None, None).unwrap();
        assert!(rotation.previous_id.is_none());
        // R7-I4: the fingerprint id of the new material.
        assert_eq!(
            rotation.new_key.id,
            super::fingerprint_id(&rotation.new_key.bytes)
        );
        assert!(super::key_answers_to(
            &rotation.new_key,
            &rotation.new_key.id
        ));
        assert!(!super::key_answers_to(&rotation.new_key, "default"));
        let keyring = load_keyring(&keyring_path).unwrap().unwrap();
        assert_eq!(keyring.keys.len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    /// Item 17: re-writing an existing keyring goes through temp+rename —
    /// the result holds the old and new keys, and no `.tmp` file is left.
    /// A stale `.tmp` from an earlier crash is overwritten, not trusted.
    #[test]
    fn keyring_rewrite_is_atomic_and_leaves_no_temp_file() {
        let dir = test_dir("keyring-atomic");
        let keyring_path = dir.join("audit-keys.json");
        let tmp = super::keyring_temp_path(&keyring_path);
        assert_eq!(tmp, dir.join("audit-keys.json.tmp"));

        rotate_at(&keyring_path, Some("one"), None).unwrap();
        let first = fs::read_to_string(&keyring_path).unwrap();
        assert!(!tmp.exists());

        fs::write(&tmp, "stale garbage from a crashed write").unwrap();
        let rotation = rotate_at(&keyring_path, Some("two"), None).unwrap();
        assert!(!tmp.exists(), "temp file left behind");
        let second = fs::read_to_string(&keyring_path).unwrap();
        assert_ne!(first, second);
        let keyring = load_keyring(&keyring_path).unwrap().unwrap();
        assert_eq!(keyring.active, "two");
        let ids: Vec<&str> = keyring.keys.iter().map(|key| key.id.as_str()).collect();
        assert!(ids.contains(&"one") && ids.contains(&"two"), "{ids:?}");
        let active = keyring.keys.iter().find(|key| key.id == "two").unwrap();
        assert_eq!(active.bytes, rotation.new_key.bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&keyring_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "keyring mode was {mode:o}");
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rotate_rejects_duplicate_id() {
        let dir = test_dir("rotate-dupe");
        let keyring_path = dir.join("audit-keys.json");
        rotate_at(&keyring_path, Some("dup"), None).unwrap();
        let err = rotate_at(&keyring_path, Some("dup"), None).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    /// A retired id already present in the keyring under different material
    /// is a hard stop — silently keeping it would mask a keying mixup.
    #[test]
    fn rotate_rejects_conflicting_previous_id() {
        let dir = test_dir("rotate-conflict");
        let keyring_path = dir.join("audit-keys.json");
        rotate_at(&keyring_path, Some("default"), None).unwrap();
        let stranger = AuditKey::new("default", &[0x33; 32]);
        let err = rotate_at(&keyring_path, Some("next"), Some(stranger)).unwrap_err();
        assert!(err.contains("different key material"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_keyring_rejects_broken_files() {
        let dir = test_dir("keyring-broken");
        let path = dir.join("audit-keys.json");
        let write = |bytes: &[u8]| {
            fs::write(&path, bytes).unwrap();
            // The permission gate runs before parsing; pin 0600 so the test
            // exercises the format checks, not the mode check.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
        };
        write(b"not json");
        assert!(load_keyring(&path).is_err());
        write(br#"{"version":9,"active":"a","keys":{"a":"00"}}"#);
        assert!(load_keyring(&path).is_err());
        // Active id must name a contained key.
        write(
            format!(
                "{{\"version\":1,\"active\":\"ghost\",\"keys\":{{\"a\":\"{}\"}}}}",
                "00".repeat(32)
            )
            .as_bytes(),
        );
        assert!(load_keyring(&path).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    /// `env:VAR` resolves material without touching disk — the secret-store
    /// hand-off. Unique var name so no other test can observe it.
    #[test]
    fn env_key_source_parses_material() {
        let var = "FRAMETRACE_TEST_KEY_SOURCE";
        unsafe {
            std::env::set_var(var, "11".repeat(32));
        }
        let key = resolve_key_source(&format!("env:{var}")).unwrap();
        assert_eq!(key.bytes, vec![0x11; 32]);
        unsafe {
            std::env::remove_var(var);
        }
        let err = resolve_key_source("env:FRAMETRACE_TEST_KEY_SOURCE_MISSING").unwrap_err();
        assert!(err.contains("is not set"), "{err}");
        assert!(resolve_key_source("env:").is_err());
        assert!(resolve_key_source("bogus").is_err());
        assert!(resolve_key_source("file:/nonexistent/key").is_err());
    }

    #[test]
    fn file_key_source_reads_key_file() {
        let dir = test_dir("key-source-file");
        let path = dir.join("key");
        fs::write(&path, "22".repeat(32)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let key = resolve_key_source(&format!("file:{}", path.display())).unwrap();
        assert_eq!(key.bytes, vec![0x22; 32]);
        // R7-I4: no shared "default" id — the key's own fingerprint id
        // (unless FRAMETRACE_AUDIT_KEY_ID names it).
        if std::env::var_os("FRAMETRACE_AUDIT_KEY_ID").is_none() {
            assert_eq!(key.id, super::fingerprint_id(&[0x22; 32]));
            assert!(key.id.starts_with("kid-"), "{}", key.id);
        }
        let _ = fs::remove_dir_all(dir);
    }
}
