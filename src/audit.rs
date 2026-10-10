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
    /// configured (a subset of `keyed_entries`).
    pub unauthenticated_keyed_entries: usize,
    /// Log-level mark per the design's verification matrix.
    pub integrity: AuditIntegrity,
    /// Caveats that keep the mark honest (mixed chain, missing key, ...).
    pub warnings: Vec<String>,
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

fn append_chained_locked(
    file: &mut File,
    body: &str,
    key: Option<&AuditKey>,
) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut existing = String::new();
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_to_string(&mut existing))
        .map_err(|err| format!("failed to read audit log for chaining: {err}"))?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        return Err(
            "audit log ends with an incomplete line (likely a torn write); run verify-audit and repair the log before appending"
                .to_string(),
        );
    }
    let previous_entry_sha256 = existing
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| sha256::digest_bytes(line.as_bytes()))
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

pub fn verify_chained_jsonl(path: &Path) -> Result<AuditChainVerification, String> {
    let keys = crate::audit_key::verification_keys()?;
    let text = read_log_for_verification(path)?;
    verify_chained_jsonl_text(&text, &path.display().to_string(), &keys)
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
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "audit log {} does not exist (missing log is not verifiable; it may have been deleted)",
                path.display()
            ));
        }
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
                .map(|_| text)
                .map_err(read_error)
        }
        Err(_) => read_to_string(path).map_err(read_error),
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
    verify_chained_jsonl_text(&text, &path.display().to_string(), keys)
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
    let complete_tail = text.is_empty() || text.ends_with('\n');
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let mut previous_entry_sha256 = "GENESIS".to_string();
    let mut last_entry_sha256 = previous_entry_sha256.clone();
    let mut entries = 0usize;
    let mut keyed_entries = 0usize;
    let mut unauthenticated_keyed = 0usize;

    for (index, line) in lines.iter().enumerate() {
        let line_number = index + 1;
        // What the keyed layer found on this line, reported up to the
        // log-level tallies.
        enum Keyed {
            Unkeyed,
            Verified,
            Unauthenticated,
        }
        let verify_line = || -> Result<Keyed, String> {
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

            // Keyed layer — purely additive over the same signed bytes.
            let recorded_hmac = extract_json_string(line, "entry_hmac_sha256");
            let recorded_key_id = extract_json_string(line, "entry_hmac_key_id");
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
                    let verified = keys
                        .iter()
                        .filter(|key| key.id == key_id)
                        .chain(keys.iter().filter(|key| key.id != key_id))
                        .any(|key| {
                            crate::hmac::tags_equal(
                                &crate::hmac::hmac_sha256(&key.bytes, signed_entry.as_bytes()),
                                &decode_hex_tag(&recorded).unwrap_or([0xff; 32]),
                            )
                        });
                    if !verified {
                        return Err(format!(
                            "audit line {line_number} HMAC verification failed (key_id \"{key_id}\"): the entry was re-written or signed under a different key"
                        ));
                    }
                    Ok(Keyed::Verified)
                }
            }
        };
        match verify_line() {
            Ok(Keyed::Verified) => keyed_entries += 1,
            Ok(Keyed::Unauthenticated) => {
                keyed_entries += 1;
                unauthenticated_keyed += 1;
            }
            Ok(Keyed::Unkeyed) => {}
            Err(error) => {
                if line_number == lines.len() && !complete_tail {
                    return Err(format!(
                        "audit log {source_name} ends with an incomplete final entry (torn write): {error}; remove or repair the last line, then re-run verify"
                    ));
                }
                return Err(error);
            }
        }

        previous_entry_sha256 = sha256::digest_bytes(line.as_bytes());
        last_entry_sha256 = recorded_entry_hash(line).unwrap_or_else(|| last_entry_sha256.clone());
        entries += 1;
    }

    // Log-level mark per the design's verification matrix.
    let mut warnings = Vec::new();
    let mut integrity = AuditIntegrity::StructuralOnly;
    if keyed_entries > 0 && unauthenticated_keyed > 0 {
        warnings.push(format!(
            "{unauthenticated_keyed} keyed entr{} present but no audit key is configured — authenticity unverified (structural chain verified)",
            if unauthenticated_keyed == 1 { "y" } else { "ies" }
        ));
    }
    if keyed_entries > 0 && keyed_entries < entries {
        warnings.push(
            "mixed keyed/unkeyed entries — log-level integrity degrades to structural-only"
                .to_string(),
        );
    }
    if keyed_entries == entries && entries > 0 && unauthenticated_keyed == 0 {
        integrity = AuditIntegrity::Keyed;
    }

    Ok(AuditChainVerification {
        entries,
        last_entry_sha256,
        keyed_entries,
        unauthenticated_keyed_entries: unauthenticated_keyed,
        integrity,
        warnings,
    })
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

pub fn indexed_source_hash(case_dir: &Path, selector: &str, source_path: &Path) -> Option<String> {
    let source = source_path.to_string_lossy();
    let text = read_to_string(&case_dir.join("db/videos.jsonl")).ok()?;
    for line in text.lines() {
        let id = extract_json_string(line, "id");
        let indexed_source = extract_json_string(line, "source_path");
        let relative_path = extract_json_string(line, "relative_path");
        let matches_selector = id.as_deref() == Some(selector)
            || indexed_source.as_deref() == Some(selector)
            || relative_path.as_deref() == Some(selector)
            || indexed_source.as_deref() == Some(source.as_ref());
        if matches_selector {
            return extract_json_string(line, "sha256");
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::{
        AuditIntegrity, append_chained_jsonl, append_chained_jsonl_keyed, digest_file,
        verify_chained_jsonl, verify_chained_jsonl_keyed,
    };
    use crate::audit_key::AuditKey;
    use crate::util::read_to_string;
    use std::fs;
    use std::io::Write;

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
        let writer_key = test_key("default", 0x01);
        let attacker_key = test_key("default", 0x02);

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
}
