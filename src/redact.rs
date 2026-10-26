//! Deterministic path redaction for distributable reports (security review
//! finding: reports and viewer payloads expose full source paths by default).
//!
//! [`Redactor`] rewrites every absolute path found in text — a JSON value
//! that is a path, and (I4) a path embedded in the middle of a string such
//! as a note, a tag, an error message or an ffprobe tag — into a stable
//! token:
//!
//! - paths under the case directory become `<case>/`-relative, so report
//!   tables keep showing which artifact/output is meant;
//! - paths outside the case become `<redacted:HASH>` where HASH is the first
//!   16 hex characters of the SHA-256 over the path string as written, so a
//!   redacted report still lets a reviewer tell "same file" from "different
//!   file" without revealing directory layout;
//! - `file:` URLs become `<redacted-url:HASH>` over the full URL string.
//!
//! Redaction is consistent by construction: the token is a pure function of
//! the path string, so the same path always maps to the same token across
//! all inputs of one report run and across runs.
//!
//! What counts as a path (heuristic, documented so a reader knows its
//! limits): a POSIX root (`/x` at the start of a word), a Windows drive
//! (`C:\` / `C:/`), a UNC or extended path (`\\server`, `\\?\`), or a
//! `file:` URL. An embedded path ends at whitespace or a quote/`<>|*?`
//! character; it continues across a single space when the next word still
//! contains a path separator, or when that word ends the string and has a
//! file extension (`C:\Users\Kim Park\My Clip.mp4`). A space-containing
//! path followed by more prose can therefore leave its last bare file-name
//! word visible — the least-sensitive part of a location. Relative paths,
//! ids and codec names pass through untouched.
//!
//! Redaction only rewrites the documents it is given. Raw case data (the
//! SQLite database, the index files, the hash-chained audit logs) is never
//! rewritten — rewriting a chained log would break its chain hashes — so a
//! redacted package lists those files as not path-redacted
//! (see `package::package_case`).

use crate::util::strip_windows_extended_prefix;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Marker every path-redacted HTML output carries (in addition to the
/// visible [`REDACTION_NOTE`]), so `package-case` can tell a redacted
/// document from an unredacted one without guessing from its content. Data
/// inside generated pages is HTML/JS-escaped, so a note cannot forge it.
pub const REDACTION_MARKER: &str = "<!-- frametrace:path-redaction=applied -->";

/// True when an HTML output was written with path redaction applied.
pub fn html_is_redacted(html: &str) -> bool {
    html.contains(REDACTION_MARKER)
}

/// D12: first line of a `review/data-bundle.js` written in path-redaction
/// mode. The bundle's data is JSON/script-escaped (`<` never appears raw),
/// so case data cannot forge it.
pub const BUNDLE_REDACTION_MARKER: &str = "/* <!-- frametrace:path-redaction=applied --> */";

/// D12: whether a viewer data bundle was written with path redaction.
pub fn bundle_is_redacted(js: &str) -> bool {
    js.lines().next() == Some(BUNDLE_REDACTION_MARKER)
}

/// D12: the bundle text with its redaction marker line prepended.
pub fn mark_bundle_redacted(js: &str) -> String {
    format!("{BUNDLE_REDACTION_MARKER}\n{js}")
}

/// Redact every absolute path embedded in `input`. Whole-input JSON first;
/// if that does not parse, each non-empty line is treated as one JSONL
/// record. Unparseable input is returned unchanged (same failure-soft policy
/// as the report's `jsonl_to_array`, which skips torn lines downstream).
pub fn redact_json_text(input: &str, case_dir: &Path) -> String {
    Redactor::new(case_dir).json_text(input)
}

/// Redact every absolute path embedded in plain text (an HTML/CSV/XML/text
/// document, an error message, a note).
pub fn redact_text(text: &str, case_dir: &Path) -> String {
    Redactor::new(case_dir).text(text)
}

/// The redaction note rendered into reports/review pages so the reader can
/// tell the paths were deliberately rewritten — and what was NOT rewritten.
pub const REDACTION_NOTE: &str = "path redaction applied: absolute source paths (and file names derived from them) were rewritten as <case>/… or <redacted:hash> tokens in this document. Raw case data (the SQLite database, the index files and the hash-chained audit logs) is not rewritten and still contains the absolute paths — see the package README for the list of files that are not path-redacted. Operator and host names are not paths and are shown as recorded unless stated otherwise.";

/// One redaction pass over a case: case roots resolved once, path tokens
/// cached (the same path appears in many records).
pub struct Redactor {
    case_roots: Vec<PathBuf>,
    /// R5-P6: `sanitize_filename` forms of every known root (the case
    /// directory, registered evidence sources, scanned source folders and
    /// the examiner's home directory). A flattened path is recognised only
    /// by one of these prefixes (or a drive letter + `__`), never by a
    /// fixed list of directory names.
    flattened_roots: Vec<String>,
    cache: RefCell<HashMap<String, String>>,
    /// Resolves a candidate path (`Path::canonicalize` in production,
    /// injected by tests). Only ever called for local paths that textually
    /// lie under a case root — see [`Redactor::may_resolve`].
    canonicalize: Canonicalize,
}

/// The filesystem hook [`Redactor`] resolves candidate paths through.
pub type Canonicalize = fn(&Path) -> std::io::Result<PathBuf>;

fn fs_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    path.canonicalize()
}

impl Redactor {
    pub fn new(case_dir: &Path) -> Self {
        Self::with_canonicalizer(case_dir, fs_canonicalize)
    }

    /// [`Redactor::new`] with extra known roots (tests; a caller that knows
    /// more source folders than the case records).
    pub fn with_known_roots(case_dir: &Path, roots: &[&str]) -> Self {
        let mut redactor = Self::new(case_dir);
        for root in roots {
            redactor.add_root(root);
        }
        redactor.collapse_flattened_roots();
        redactor
    }

    /// R5-P6: registers the flattened (`sanitize_filename`) form of a root.
    /// Roots too short to be specific (`/`, `C:\`, `/a`) are ignored — they
    /// would match ordinary identifiers.
    fn add_flattened_root(&mut self, root: &str) {
        let trimmed = root.trim().trim_end_matches(['/', '\\']);
        if trimmed.is_empty() {
            return;
        }
        let flat = crate::util::sanitize_filename(trimmed);
        let meaningful = flat.bytes().filter(|b| b.is_ascii_alphanumeric()).count();
        if meaningful < 3 || flat.len() < 4 || !flat.starts_with('_') {
            // Drive paths (`C__…`) are covered by the drive rule already;
            // anything else that does not start like a flattened root is
            // not one.
            return;
        }
        if !self.flattened_roots.contains(&flat) {
            self.flattened_roots.push(flat);
        }
    }

    /// [`Redactor::new`] with the path-resolution hook made explicit
    /// (tests count the filesystem calls a redaction pass makes).
    pub fn with_canonicalizer(case_dir: &Path, canonicalize: Canonicalize) -> Self {
        let mut case_roots = Vec::new();
        if let Ok(canonical) = case_dir.canonicalize() {
            case_roots.push(strip_windows_extended_prefix(&canonical));
        }
        let given = strip_windows_extended_prefix(case_dir);
        // `has_root`, not `is_absolute`: a POSIX-style `/case` spelling is
        // rooted (though not absolute) on Windows and must still match.
        if given.has_root() && !case_roots.contains(&given) {
            case_roots.push(given);
        }
        let mut redactor = Self {
            case_roots,
            flattened_roots: Vec::new(),
            cache: RefCell::new(HashMap::new()),
            canonicalize,
        };
        let roots = known_roots(case_dir, &redactor.case_roots);
        for root in &roots.explicit {
            redactor.add_root(root);
        }
        for path in &roots.recorded {
            for candidate in flattened_root_candidates(path) {
                redactor.add_flattened_root(&candidate);
            }
        }
        redactor.collapse_flattened_roots();
        redactor
    }

    /// An explicit root (case, evidence source, home, a caller's extra
    /// root): the root and its folder ancestors (R6-I9) — R7-I8: only those
    /// at least two components below the filesystem root, exactly like a
    /// recorded path. A depth-1 root (`HOME=/root`, a source at `/mnt`)
    /// flattens to `_root` / `_mnt`, which would swallow identifiers such
    /// as `_root_cause` or `_mnt_point`.
    fn add_root(&mut self, root: &str) {
        for candidate in flattened_root_candidates(root) {
            self.add_flattened_root(&candidate);
        }
    }

    /// R6-I9: drops every flattened root another (shorter) one already
    /// covers — a word starting with `_tmp_x_src` starts with `_tmp_x` —
    /// so a case with thousands of source folders keeps a short list.
    fn collapse_flattened_roots(&mut self) {
        let mut roots = std::mem::take(&mut self.flattened_roots);
        roots.sort_by_key(|root| root.len());
        let mut kept: Vec<String> = Vec::new();
        for root in roots {
            let covered = kept.iter().any(|shorter| {
                root.len() > shorter.len()
                    && root.as_bytes()[..shorter.len()].eq_ignore_ascii_case(shorter.as_bytes())
                    && matches!(root.as_bytes()[shorter.len()], b'_' | b'-')
            });
            if !covered {
                kept.push(root);
            }
        }
        self.flattened_roots = kept;
    }

    /// [`redact_json_text`] with this pass's roots and cache.
    pub fn json_text(&self, input: &str) -> String {
        if input.trim().is_empty() {
            return input.to_string();
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(input) {
            return self.value(&value).to_string();
        }
        let mut out = String::with_capacity(input.len());
        for (index, line) in input.lines().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(value) => out.push_str(&self.value(&value).to_string()),
                // D7: a line that does not parse (a torn final line, a
                // damaged record) is still redacted — string literal by
                // string literal, decoded first so an escaped path
                // (`C:\\Users\\kim`, a path after `\n`) is seen as written.
                Err(_) => out.push_str(&self.json_literals(line)),
            }
        }
        if input.ends_with('\n') && !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }

    /// D7: redacts unparseable JSON text one string literal at a time. Each
    /// `"…"` literal is decoded, redacted and re-encoded; an unterminated
    /// final literal (torn write) is decoded as far as it goes. Text
    /// between literals is scanned as plain text too.
    pub fn json_literals(&self, line: &str) -> String {
        let bytes = line.as_bytes();
        let mut out = String::with_capacity(line.len());
        let mut plain_start = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] != b'"' {
                i += 1;
                continue;
            }
            out.push_str(&self.text(&line[plain_start..i]));
            // Find the closing quote, skipping escaped characters.
            let mut end = i + 1;
            let mut closed = false;
            while end < bytes.len() {
                match bytes[end] {
                    b'\\' => end += 2,
                    b'"' => {
                        closed = true;
                        break;
                    }
                    _ => end += 1,
                }
            }
            let mut end = end.min(bytes.len());
            while !line.is_char_boundary(end) {
                end += 1;
            }
            let literal = if closed {
                &line[i..=end]
            } else {
                &line[i..end]
            };
            let decoded = if closed {
                serde_json::from_str::<String>(literal).ok()
            } else {
                serde_json::from_str::<String>(&format!("{literal}\""))
                    .ok()
                    .or_else(|| {
                        let trimmed = literal.strip_suffix('\\').unwrap_or(literal);
                        serde_json::from_str::<String>(&format!("{trimmed}\"")).ok()
                    })
            };
            match decoded {
                Some(text) => {
                    let encoded = serde_json::to_string(&self.text(&text))
                        .unwrap_or_else(|_| "\"\"".to_string());
                    if closed {
                        out.push_str(&encoded);
                    } else {
                        // Keep the literal unterminated, as it was.
                        out.push_str(encoded.strip_suffix('"').unwrap_or(&encoded));
                    }
                }
                None => out.push_str(&self.text(literal)),
            }
            i = if closed { end + 1 } else { bytes.len() };
            plain_start = i;
        }
        out.push_str(&self.text(&line[plain_start.min(line.len())..]));
        out
    }

    /// Redacts a parsed JSON value: every string, object keys included (a
    /// path used as a map key must not survive either).
    pub fn value(&self, value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(text) => serde_json::Value::String(self.text(text)),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(|item| self.value(item)).collect())
            }
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.iter()
                    .map(|(key, item)| (self.text(key), self.value(item)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Rewrites every absolute path / `file:` URL found anywhere in `text`.
    pub fn text(&self, text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut copied = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            let Some(kind) = path_start(text, i, &self.flattened_roots) else {
                i += 1;
                continue;
            };
            let end = match kind {
                StartKind::Url => token_end(bytes, i, true),
                StartKind::Path => path_end(bytes, i),
                StartKind::SanitizedPath => sanitized_end(bytes, i),
            };
            // Paths start at ASCII bytes and end before an ASCII terminator,
            // so both ends are char boundaries.
            if end <= i || !text.is_char_boundary(end) {
                i += 1;
                continue;
            }
            out.push_str(&text[copied..i]);
            let found = &text[i..end];
            match kind {
                StartKind::Url => {
                    out.push_str(&format!("<redacted-url:{}>", stable_token(found)));
                }
                StartKind::Path => out.push_str(&self.path_token(found)),
                StartKind::SanitizedPath => {
                    out.push_str(&format!("<redacted:{}>", stable_token(found)));
                }
            }
            copied = end;
            i = end;
        }
        out.push_str(&text[copied..]);
        out
    }

    fn path_token(&self, path: &str) -> String {
        if let Some(hit) = self.cache.borrow().get(path) {
            return hit.clone();
        }
        let token = self.map_path(path);
        self.cache
            .borrow_mut()
            .insert(path.to_string(), token.clone());
        token
    }

    /// R1: whether `path` may be resolved on the filesystem at all. A
    /// remote or device token (`\\203.0.113.5\x`, `//host/x` on Windows,
    /// `\\?\UNC\…`, `\\.\pipe\…`, `\\?\GLOBALROOT\…`, a reserved device
    /// name) is redacted textually only: resolving it would open an SMB
    /// session to the named host (handing it the examiner's NTLM
    /// credentials) or a device that can block the report run. Of the
    /// local paths, only those that textually lie under a case root are
    /// resolved (to see through `..`, symlinks, short names); any other
    /// path becomes a hash token without touching the disk.
    fn may_resolve(&self, path: &str, normalized: &Path) -> bool {
        if crate::util::is_remote_or_device_text(path, cfg!(windows)) {
            return false;
        }
        let text = normalized.to_string_lossy();
        self.case_roots.iter().any(|root| {
            let root = root.to_string_lossy();
            let root = root.trim_end_matches(['\\', '/']);
            let head_matches = text.get(..root.len()).is_some_and(|head| {
                if cfg!(windows) {
                    head.eq_ignore_ascii_case(root)
                } else {
                    head == root
                }
            });
            head_matches
                && text[root.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| c == '/' || c == '\\')
        })
    }

    fn map_path(&self, path: &str) -> String {
        let normalized = strip_windows_extended_prefix(Path::new(path));
        let canonical = if self.may_resolve(path, &normalized) {
            (self.canonicalize)(&normalized)
                .ok()
                .map(|p| strip_windows_extended_prefix(&p))
        } else {
            None
        };
        for candidate in canonical.iter().chain(std::iter::once(&normalized)) {
            for root in &self.case_roots {
                if let Ok(relative) = candidate.strip_prefix(root) {
                    let mut rendered = String::from("<case>");
                    for component in relative.components() {
                        rendered.push('/');
                        let name = component.as_os_str().to_string_lossy();
                        // D5: an artifact named after a flattened source
                        // path (`C__Users_kim_…_proxy_….mp4`) keeps its
                        // directory but not the name.
                        if is_sanitized_path_start(name.as_bytes(), &self.flattened_roots) {
                            rendered.push_str(&format!("<redacted:{}>", stable_token(&name)));
                        } else {
                            rendered.push_str(&name);
                        }
                    }
                    return rendered;
                }
            }
        }
        format!("<redacted:{}>", stable_token(path))
    }
}

/// First 16 hex chars of the SHA-256 over the raw input — deterministic, so
/// the same path always yields the same token, and short enough to keep
/// report tables readable.
fn stable_token(input: &str) -> String {
    crate::sha256::digest_bytes(input.as_bytes())[..16].to_string()
}

/// R5-P6: the host-name token a redacted report or custody statement shows
/// (`<redacted-host:HASH>`, HASH = first 16 hex of the SHA-256 of the
/// name) — the same token for the same host everywhere.
pub fn host_token(host: &str) -> String {
    format!(
        "<redacted-host:{}>",
        &crate::sha256::digest_bytes(host.as_bytes())[..16]
    )
}

/// R5-P6/R6-I9: the roots a flattened path in this case may start with.
/// `explicit`: the case directory, registered evidence sources and the
/// examiner's home directory (registered as given). `recorded`: every
/// absolute path the case's own records name — the index
/// (`db/videos.jsonl`, `db/video_index.json`, scan-run snapshots), the
/// case manifest (`case.json`) and the chained audit/artifact logs
/// (source paths, selectors, output paths, and so the case's earlier
/// location when it was moved) — whose folder ancestors
/// [`flattened_root_candidates`] expands. Best effort: an unreadable file
/// contributes nothing (the drive-letter rule still applies).
struct KnownRoots {
    explicit: Vec<String>,
    recorded: Vec<String>,
}

fn known_roots(case_dir: &Path, case_roots: &[PathBuf]) -> KnownRoots {
    let mut explicit: Vec<String> = case_roots
        .iter()
        .map(|root| root.to_string_lossy().to_string())
        .collect();
    explicit.extend(crate::case_db::evidence_source_paths(case_dir).unwrap_or_default());
    for var in ["HOME", "USERPROFILE"] {
        if let Ok(home) = std::env::var(var)
            && !home.trim().is_empty()
        {
            explicit.push(home);
        }
    }
    let mut recorded = Vec::new();
    for rel in ROOT_SOURCE_FILES {
        collect_recorded_paths(&case_dir.join(rel), &mut recorded);
    }
    if let Ok(entries) = std::fs::read_dir(case_dir.join("db/scan_runs")) {
        for entry in entries.flatten() {
            collect_recorded_paths(&entry.path(), &mut recorded);
        }
    }
    for rel in crate::audit::KNOWN_CHAIN_LANES {
        collect_recorded_paths(&case_dir.join(rel), &mut recorded);
    }
    if let Ok(lanes) = crate::audio_level::lane_rels(case_dir) {
        for rel in lanes {
            collect_recorded_paths(&case_dir.join(rel), &mut recorded);
        }
    }
    explicit.sort();
    explicit.dedup();
    recorded.sort();
    recorded.dedup();
    KnownRoots { explicit, recorded }
}

/// R6-I9: case files whose absolute paths name roots (besides the audit
/// lanes and scan-run snapshots, which are enumerated separately).
const ROOT_SOURCE_FILES: &[&str] = &["case.json", "db/videos.jsonl", "db/video_index.json"];

/// R6-I9: a recorded file larger than this is not scanned for roots (a
/// pathological log must not make every redaction pass read gigabytes).
const ROOT_SOURCE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// R6-I9: every absolute path written anywhere in the JSON / JSONL file at
/// `path` (string values and object keys, mid-string paths included).
fn collect_recorded_paths(path: &Path, out: &mut Vec<String>) {
    if std::fs::metadata(path).map_or(true, |meta| meta.len() > ROOT_SOURCE_MAX_BYTES) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    collect_paths_in_json_text(&text, out);
}

/// R6-I9: [`collect_recorded_paths`] over text (whole JSON first, then
/// line by line).
fn collect_paths_in_json_text(text: &str, out: &mut Vec<String>) {
    fn walk(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(text) => out.extend(embedded_absolute_paths(text)),
            serde_json::Value::Array(items) => items.iter().for_each(|item| walk(item, out)),
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    out.extend(embedded_absolute_paths(key));
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        walk(&value, out);
        return;
    }
    for line in text.lines() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            walk(&value, out);
        }
    }
}

/// R6-I9: the absolute paths [`Redactor::text`] treats as paths in `text`
/// (not flattened names, not URLs).
fn embedded_absolute_paths(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        match path_start(text, i, &[]) {
            Some(StartKind::Path) => {
                let end = path_end(bytes, i);
                if end > i && text.is_char_boundary(end) {
                    out.push(text[i..end].to_string());
                    i = end;
                } else {
                    i += 1;
                }
            }
            Some(StartKind::Url) => i = token_end(bytes, i, true).max(i + 1),
            _ => i += 1,
        }
    }
    out
}

/// R6-I9/CI1: the roots one recorded absolute path contributes, worked out
/// on the TEXT — never through `Path` — so a POSIX path is recognised on
/// Windows and a Windows/UNC path on Linux/macOS alike: every folder
/// ancestor with at least two components below the root, and the path
/// itself (`/tmp/x`, `/tmp/x/src`, `//nas/share`; never `/tmp` or `/home`
/// alone, which would match identifiers such as `_tmp_value`). A macOS
/// `/private/var/…` path also yields its `/var/…` spelling (the same
/// folder through the system symlink). Drive paths (`C:\…`) yield nothing
/// — the drive-letter rule covers them — and so do relative paths.
fn flattened_root_candidates(path: &str) -> Vec<String> {
    let text = path.trim();
    let text = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text.to_string()
    };
    let unc = text.starts_with(r"\\") || text.starts_with("//");
    let lead = if unc {
        "//"
    } else if text.starts_with('/') {
        "/"
    } else {
        return Vec::new();
    };
    let components: Vec<&str> = text
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .take_while(|part| *part != "..")
        .collect();
    let mut spellings = vec![components.clone()];
    if !unc && components.len() > 3 && components[0] == "private" {
        spellings.push(components[1..].to_vec());
    }
    let mut out = Vec::new();
    for parts in spellings {
        for depth in 2..=parts.len() {
            out.push(format!("{lead}{}", parts[..depth].join("/")));
        }
    }
    out
}

#[derive(Clone, Copy)]
enum StartKind {
    Path,
    Url,
    /// D5: a path flattened by `sanitize_filename` into one file-name word
    /// (`C__Users_kim_clip_mp4_proxy_…`, `_home_kim_…`).
    SanitizedPath,
}

/// A byte that may precede a path at the start of a "word".
fn is_word_boundary(prev: Option<u8>) -> bool {
    match prev {
        None => true,
        Some(b) => b.is_ascii_whitespace() || b"\"'`([{=,;|>".contains(&b),
    }
}

/// D7: `i` directly follows a JSON/C escape such as `\n`, `\t` or `\r`
/// (text that still carries its escapes — a raw JSON line, script data):
/// the escape ends the previous word even though its last byte is a letter.
fn after_escape(bytes: &[u8], i: usize) -> bool {
    i >= 2
        && bytes[i - 2] == b'\\'
        && matches!(bytes[i - 1], b'n' | b't' | b'r')
        && (i < 3 || bytes[i - 3] != b'\\')
}

/// D7/R5-P6: punctuation that may glue a path to the preceding text —
/// a colon (`원본경로:/home/…`) or Markdown bold (`**/home/…`). Nothing
/// else: `#/route` (a URL fragment), `x*/2` (arithmetic) and `!/…` are not
/// paths.
fn is_glue_punctuation(bytes: &[u8], i: usize) -> bool {
    match i.checked_sub(1).map(|p| bytes[p]) {
        Some(b':') => true,
        Some(b'*') => i >= 2 && bytes[i - 2] == b'*',
        _ => false,
    }
}

/// URL schemes whose whole URL is redacted (D7: `smb://`, `nfs://`, …).
const NETWORK_URL_SCHEMES: &[&[u8]] = &[b"smb://", b"nfs://", b"afp://", b"cifs://"];

/// D5/R5-P6: a `sanitize_filename`-flattened absolute path starting at
/// `rest`: `C__…` (a drive letter + `__`), or a word that begins with the
/// flattened form of a known root (`_home_kim…` when `/home/kim` is the
/// examiner's home or a source root, `__nas01_share…` for a registered UNC
/// source). An ordinary identifier (`_run_id`, `__init__`) is not one.
fn is_sanitized_path_start(rest: &[u8], roots: &[String]) -> bool {
    let word_len = rest
        .iter()
        .position(|&b| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        .unwrap_or(rest.len());
    let word = &rest[..word_len];
    if word.len() >= 4
        && word[0].is_ascii_alphabetic()
        && word[1] == b'_'
        && word[2] == b'_'
        && word[3].is_ascii_alphanumeric()
    {
        return true;
    }
    roots.iter().any(|root| {
        let root = root.as_bytes();
        word.len() >= root.len()
            && word[..root.len()].eq_ignore_ascii_case(root)
            && word.get(root.len()).is_none_or(|&b| b == b'_' || b == b'-')
    })
}

/// End of a flattened path: the rest of the file-name word (a directory
/// separator, whitespace, quote or prose punctuation ends it).
fn sanitized_end(bytes: &[u8], start: usize) -> usize {
    let end = token_end(bytes, start, false);
    bytes[start..end]
        .iter()
        .position(|&b| b == b'/' || b == b'\\')
        .map_or(end, |offset| start + offset)
}

/// Bytes that end an embedded path.
fn is_terminator(b: u8) -> bool {
    b.is_ascii_whitespace() || b"\"'`<>|*?".contains(&b) || b < 0x20
}

fn path_start(text: &str, i: usize, roots: &[String]) -> Option<StartKind> {
    let bytes = text.as_bytes();
    let prev = i.checked_sub(1).map(|p| bytes[p]);
    // A non-ASCII predecessor (Korean prose glued to a drive path) still
    // starts a drive/UNC/URL path; those shapes are specific enough.
    // D7: a path may also follow a JSON escape (`\n/home/…`), glue
    // punctuation (`경로:/home`, `**/home`) or non-ASCII prose.
    let escaped = after_escape(bytes, i);
    let loose_boundary = is_word_boundary(prev) || prev.is_some_and(|b| b >= 0x80) || escaped;
    let rest = &bytes[i..];
    if rest.len() >= 6
        && rest[..5].eq_ignore_ascii_case(b"file:")
        && (loose_boundary || is_glue_punctuation(bytes, i))
        && !is_terminator(rest[5])
    {
        return Some(StartKind::Url);
    }
    if (loose_boundary || is_glue_punctuation(bytes, i))
        && NETWORK_URL_SCHEMES.iter().any(|scheme| {
            rest.len() > scheme.len()
                && rest[..scheme.len()].eq_ignore_ascii_case(scheme)
                && !is_terminator(rest[scheme.len()])
        })
    {
        return Some(StartKind::Url);
    }
    if rest.starts_with(b"\\\\")
        && prev != Some(b'\\')
        && (loose_boundary || is_glue_punctuation(bytes, i))
        && rest
            .get(2)
            .is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'?' || b == b'.' || b >= 0x80)
    {
        return Some(StartKind::Path);
    }
    if rest.len() >= 3
        && rest[0].is_ascii_alphabetic()
        && rest[1] == b':'
        && (rest[2] == b'\\' || rest[2] == b'/')
        // D7: a `:` before the drive (`원본경로:D:\…`, `path:C:\…`) no
        // longer hides it.
        && (escaped || prev.is_none_or(|b| !(b.is_ascii_alphanumeric() || b"_.-/\\".contains(&b))))
    {
        return Some(StartKind::Path);
    }
    // D7: `//host/share/…` (a UNC path written with forward slashes). Not
    // after `:` — that is a URL (`https://…`).
    if rest.starts_with(b"//")
        && (loose_boundary || (is_glue_punctuation(bytes, i) && prev != Some(b':')))
        && rest.get(2).is_some_and(|&b| b.is_ascii_alphanumeric())
        && {
            let end = token_end(bytes, i, false);
            bytes[i + 2..end].contains(&b'/')
        }
    {
        return Some(StartKind::Path);
    }
    if rest[0] == b'/'
        && !text[..i].ends_with("<case>")
        && rest.get(1).is_some_and(|&b| {
            b != b'/' && (b.is_ascii_alphanumeric() || b"_.~-$".contains(&b) || b >= 0x80)
        })
    {
        let strict = is_word_boundary(prev) || escaped || is_glue_punctuation(bytes, i);
        // After non-ASCII prose (`경로/home/kim`) a slash alone is too
        // common (`판독/검증`): require at least two path components.
        let after_prose = prev.is_some_and(|b| b >= 0x80) && {
            let end = token_end(bytes, i, false);
            bytes[i + 1..end].contains(&b'/')
        };
        if strict || after_prose {
            return Some(StartKind::Path);
        }
    }
    // D5: sanitize_filename-flattened paths, usually a file-name component
    // right after a directory separator.
    if (is_word_boundary(prev) || escaped || matches!(prev, Some(b'/' | b'\\')))
        && is_sanitized_path_start(rest, roots)
    {
        return Some(StartKind::SanitizedPath);
    }
    // R5-P6: a home-relative path (`~/Videos/kim.mp4`, `~\Videos\…`)
    // names the examiner's home as surely as an absolute one.
    if rest.len() >= 3
        && rest[0] == b'~'
        && (rest[1] == b'/' || rest[1] == b'\\')
        && !is_terminator(rest[2])
        && (loose_boundary || is_glue_punctuation(bytes, i))
    {
        return Some(StartKind::Path);
    }
    None
}

/// End of one whitespace-free token starting at `start`. A `,`/`;` ends it
/// only when followed by whitespace or the end (prose punctuation); inside
/// a file name it is kept. The extended prefix `\\?\` keeps its `?`.
fn token_end(bytes: &[u8], start: usize, url: bool) -> usize {
    let extended = !url && bytes[start..].starts_with(b"\\\\?\\");
    let mut end = start;
    while end < bytes.len() {
        let b = bytes[end];
        if is_terminator(b) && !(extended && end == start + 2) {
            break;
        }
        if (b == b',' || b == b';')
            && bytes
                .get(end + 1)
                .is_none_or(|next| next.is_ascii_whitespace())
        {
            break;
        }
        end += 1;
    }
    trim_trailing_punctuation(bytes, start, end)
}

fn trim_trailing_punctuation(bytes: &[u8], start: usize, mut end: usize) -> usize {
    while end > start + 1 {
        let last = bytes[end - 1];
        let unbalanced = |open: u8, close: u8| {
            last == close
                && bytes[start..end].iter().filter(|&&b| b == close).count()
                    > bytes[start..end].iter().filter(|&&b| b == open).count()
        };
        if b".,:;!".contains(&last)
            || unbalanced(b'(', b')')
            || unbalanced(b'[', b']')
            || unbalanced(b'{', b'}')
        {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// End of an embedded path: one token, extended across single spaces while
/// the next word still contains a separator, or is the string's last word
/// and carries a file extension.
fn path_end(bytes: &[u8], start: usize) -> usize {
    let mut end = token_end(bytes, start, false);
    loop {
        if end >= bytes.len() || bytes[end] != b' ' {
            break;
        }
        let next_start = end + 1;
        if next_start >= bytes.len() {
            break;
        }
        let next_end = token_end(bytes, next_start, false);
        if next_end <= next_start {
            break;
        }
        let word = &bytes[next_start..next_end];
        let has_separator = word.iter().any(|&b| b == b'/' || b == b'\\');
        let final_file_name = next_end == bytes.len()
            && word
                .iter()
                .rposition(|&b| b == b'.')
                .is_some_and(|dot| dot > 0 && dot + 1 < word.len());
        if has_separator || final_file_name {
            end = next_end;
        } else {
            break;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::{Redactor, path_start, redact_json_text, redact_text};
    use std::fs;
    use std::path::PathBuf;

    fn looks_like_absolute_path(text: &str) -> bool {
        path_start(text, 0, &[]).is_some()
    }

    fn temp_case(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("frametrace-redact-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detects_absolute_path_shapes() {
        assert!(looks_like_absolute_path("/evidence/a.mp4"));
        assert!(looks_like_absolute_path(r"C:\evidence\a.mp4"));
        assert!(looks_like_absolute_path("C:/evidence/a.mp4"));
        assert!(looks_like_absolute_path(r"\\server\share\a.mp4"));
        assert!(looks_like_absolute_path(r"\\?\C:\evidence\a.mp4"));
        assert!(!looks_like_absolute_path("camera/a.mp4"));
        assert!(!looks_like_absolute_path("vid_000001"));
        assert!(!looks_like_absolute_path("h264"));
        // D7: a forward-slash UNC path (`//host/share/…`) is a path; a bare
        // `//host` is not.
        assert!(looks_like_absolute_path("//host/share"));
        assert!(!looks_like_absolute_path("//host"));
    }

    /// D7: paths glued to punctuation or prose, network URLs, forward-slash
    /// UNC paths and paths after a JSON escape are all redacted; ordinary
    /// prose with slashes and URLs of other schemes is untouched.
    #[test]
    fn paths_after_punctuation_prose_escapes_and_network_urls_are_redacted() {
        let case_dir = temp_case("d7");
        for input in [
            r"원본경로:D:\Evidence\kim\a.mp4",
            r"path:C:\Users\kim\a.mp4",
            "**/home/kim/clip.mp4",
            "경로:/home/kim/clip.mp4",
            "참고/home/kim/clip.mp4",
            "see //nas01/share/kim/a.mp4 now",
            "참고//nas01/share/kim/a.mp4",
            "smb://nas01/share/kim/a.mp4",
            "경로:smb://nas01/share/kim/a.mp4",
            "NFS://nas01/export/kim",
            r"line one\nC:\Users\kim\a.mp4",
            r"line one\n/home/kim/a.mp4",
            r"tab\t/home/kim/a.mp4",
        ] {
            let out = redact_text(input, &case_dir);
            assert!(
                !out.contains("kim") && !out.contains("nas01"),
                "{input} -> {out}"
            );
            assert!(out.contains("<redacted"), "{input} -> {out}");
        }
        for prose in [
            "https://example.com/a/b",
            "판독/검증",
            "IN/OUT 1/2",
            "ratio: 4:3 a:b",
            "</td><td>x</td>",
            "and/or",
        ] {
            assert_eq!(redact_text(prose, &case_dir), prose);
        }
        let _ = fs::remove_dir_all(case_dir);
    }

    /// D7: a JSONL line that does not parse is still redacted, string
    /// literal by string literal (escaped paths decoded first), and keeps
    /// its JSON shape.
    #[test]
    fn unparseable_json_lines_are_redacted_per_literal() {
        let case_dir = temp_case("d7-literals");
        let input = concat!(
            "{\"ok\":\"/home/kim/a.mp4\"}\n",
            "{\"note\":\"line\\nC:\\\\Users\\\\kim\\\\a.mp4\",\"n\":1,broken}\n",
            "{\"torn\":\"/home/kim/torn"
        );
        let out = redact_json_text(input, &case_dir);
        assert!(!out.contains("kim"), "{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(
            lines[1].starts_with("{\"note\":\"line\\n<redacted:"),
            "{out}"
        );
        assert!(lines[1].ends_with(",\"n\":1,broken}"), "{out}");
        assert!(lines[2].starts_with("{\"torn\":\"<redacted:"), "{out}");
        assert!(
            !lines[2].ends_with('"'),
            "torn literal stays unterminated: {out}"
        );
        let _ = fs::remove_dir_all(case_dir);
    }

    /// D5: file names that carry a path flattened by `sanitize_filename`
    /// (legacy proxy/clip names) are redacted, also inside the case
    /// directory; path-free artifact names and ordinary ids are kept.
    #[test]
    fn sanitized_path_file_names_are_redacted() {
        let case_dir = temp_case("d5");
        // R5-P6: POSIX/UNC flattened names are recognised by known roots.
        let redactor =
            Redactor::with_known_roots(&case_dir, &["/home/kim", "/Users/kim/", r"\\nas01\share"]);
        for name in [
            "C__Users_kim_Videos_a_mp4_proxy_1700000000.mp4",
            "D__Evidence_kim_cam1_avi_clip_17.mp4",
            "_home_kim_clip_mp4_proxy_1.mp4",
            "_Users_kim_Movies_a_mp4_thumb_3.jpg",
            "__nas01_share_kim_a_mp4_proxy_2.mp4",
        ] {
            let out = redactor.text(&format!("built {name}."));
            assert!(!out.contains("kim"), "{name} -> {out}");
            assert!(
                out.starts_with("built <redacted:") && out.ends_with(">."),
                "{out}"
            );
        }
        let inside = case_dir.join("artifacts/proxies/C__Users_kim_a_mp4_proxy_1.mp4");
        let out = Redactor::new(&case_dir).text(&inside.to_string_lossy());
        assert!(
            out.starts_with("<case>/artifacts/proxies/<redacted:"),
            "{out}"
        );
        assert!(!out.contains("kim"), "{out}");
        for kept in [
            "proxy_vid_000001_ab12cd34.mp4",
            "thumb_join_abcdef0123456789_0011aabb.jpg",
            "vid_000001",
            "carve_1_avi",
            "__FRAMETRACE_DATA__",
            "snake_case_name",
        ] {
            assert_eq!(redact_text(kept, &case_dir), kept);
        }
        let _ = fs::remove_dir_all(case_dir);
    }

    /// R5-P6: flattened names are matched against the case's KNOWN roots
    /// (a registered evidence source here), not a fixed list of directory
    /// names; `~/` paths and the report host are redacted; URL fragments,
    /// arithmetic and identifiers are not paths.
    #[test]
    fn known_roots_tilde_paths_and_narrow_glue() {
        let case_dir = temp_case("r5-p6");
        crate::case_db::register_evidence_source(
            &case_dir,
            &crate::case_db::EvidenceSourceInput {
                kind: "sd".into(),
                path: PathBuf::from("/evidence/kim-sd"),
                source_id: Some("SD1".into()),
                write_protect: None,
                acquisition_tool: None,
                evidence_hash: None,
                notes: None,
                metadata_json: None,
            },
        )
        .unwrap();
        let redactor = Redactor::new(&case_dir);
        // A flattened path under a registered source whose top directory
        // was not on the old fixed list (`/evidence` was not; mounts like
        // it were missed).
        let out = redactor.text("built _evidence_kim-sd_cam1_mp4_proxy_1.mp4.");
        assert!(!out.contains("kim"), "{out}");
        for input in ["see ~/Videos/kim/a.mp4 now", r"경로:~\Videos\kim\a.mp4"] {
            let out = redactor.text(input);
            assert!(!out.contains("kim"), "{input} -> {out}");
            assert!(out.contains("<redacted:"), "{input} -> {out}");
        }
        for kept in [
            "index.html#/records/vid_1",
            "x*/2",
            "a*/b/c",
            "_run_id",
            "_tmp_value",
            "__init__",
            "#!/route/x",
            "ratio 1*/2",
        ] {
            assert_eq!(redactor.text(kept), kept, "{kept}");
        }
        assert_eq!(
            super::host_token("SECRET-LAPTOP"),
            format!(
                "<redacted-host:{}>",
                &crate::sha256::digest_bytes(b"SECRET-LAPTOP")[..16]
            )
        );
        let _ = fs::remove_dir_all(case_dir);
    }

    /// R6-CI1/I9: on Linux/macOS a scanned source folder (under `/tmp`,
    /// `/var/folders/…`) is not a registered evidence source and no scan-log
    /// line names it (only resumed scans write one), so a proxy named after
    /// its flattened path leaked. The roots now come from the case's own
    /// records — the index, the artifact/audit logs (selectors, UNC
    /// sources, the case's earlier location) — worked out on the text, so
    /// this test is platform independent (POSIX paths are injected as
    /// strings and recognised on Windows too).
    #[test]
    fn recorded_posix_and_unc_paths_are_flattened_roots() {
        let case_dir = temp_case("r6-i9");
        fs::create_dir_all(case_dir.join("db")).unwrap();
        fs::write(
            case_dir.join("db/videos.jsonl"),
            [
                serde_json::json!({"id": "vid_000001", "source_path": "/tmp/frametrace-report-out-redact-4242/SourceSecretDir/a.mp4", "relative_path": "a.mp4"}),
                serde_json::json!({"id": "vid_000002", "source_path": "/private/var/folders/ab/cd/T/ft-x/MacSecret/b.mp4", "relative_path": "b.mp4"}),
            ]
            .map(|v| v.to_string())
            .join("\n"),
        )
        .unwrap();
        fs::create_dir_all(case_dir.join("artifacts/proxies")).unwrap();
        fs::write(
            case_dir.join("artifacts/proxies/proxy-log.jsonl"),
            serde_json::json!({"event": "make-proxy", "selector": r"\\nas02\cam\UncSecret\x.mp4"})
                .to_string(),
        )
        .unwrap();
        fs::create_dir_all(case_dir.join("artifacts/clips")).unwrap();
        fs::write(
            case_dir.join("artifacts/clips/export-log.jsonl"),
            serde_json::json!({"event": "export", "output_path": "/mnt/old/MovedSecret/c1/artifacts/clips/a.mp4"})
                .to_string(),
        )
        .unwrap();
        let redactor = Redactor::new(&case_dir);
        for (name, secret) in [
            (
                "_tmp_frametrace-report-out-redact-4242_SourceSecretDir_a_mp4_proxy_1700000000.mp4",
                "SourceSecretDir",
            ),
            // An unregistered sibling folder of the source (same parent).
            (
                "_tmp_frametrace-report-out-redact-4242_OtherSecret_c_mp4_proxy_2.mp4",
                "OtherSecret",
            ),
            // macOS: the index records /private/var/…, the proxy was named
            // after the /var/… spelling.
            (
                "_var_folders_ab_cd_T_ft-x_MacSecret_b_mp4_proxy_1.mp4",
                "MacSecret",
            ),
            ("__nas02_cam_UncSecret_x_mp4_proxy_3.mp4", "UncSecret"),
            (
                "_mnt_old_MovedSecret_c1_artifacts_clips_a_mp4_thumb_1.jpg",
                "MovedSecret",
            ),
        ] {
            let out = redactor.text(&format!("built {name}."));
            assert!(!out.contains(secret), "{name} -> {out}");
            assert!(out.starts_with("built <redacted:"), "{out}");
        }
        for kept in [
            "_tmp_value",
            "_run_id",
            "_var_value",
            "_mnt_point",
            "__init__",
        ] {
            assert_eq!(redactor.text(kept), kept, "{kept}");
        }
        let _ = fs::remove_dir_all(case_dir);
    }

    /// R6-I9: root candidates are computed on the text, for every OS.
    #[test]
    fn flattened_root_candidates_are_textual_ancestors() {
        assert_eq!(
            super::flattened_root_candidates("/tmp/x/src/a.mp4"),
            ["/tmp/x", "/tmp/x/src", "/tmp/x/src/a.mp4"]
        );
        assert_eq!(
            super::flattened_root_candidates(r"\\?\UNC\nas\share\d"),
            ["//nas/share", "//nas/share/d"]
        );
        assert_eq!(
            super::flattened_root_candidates("/private/var/folders/a"),
            [
                "/private/var",
                "/private/var/folders",
                "/private/var/folders/a",
                "/var/folders",
                "/var/folders/a"
            ]
        );
        assert!(super::flattened_root_candidates("/tmp").is_empty());
        assert!(super::flattened_root_candidates(r"C:\Users\kim").is_empty());
        assert!(super::flattened_root_candidates("rel/a/b").is_empty());
        assert_eq!(super::flattened_root_candidates("/a/b/../c"), ["/a/b"]);
    }

    /// D12: the bundle redaction marker is a first-line comment only a
    /// redacted make-review writes.
    #[test]
    fn bundle_redaction_marker_round_trips() {
        let js = "window.__FRAMETRACE_DATA__ = {};";
        assert!(!super::bundle_is_redacted(js));
        let marked = super::mark_bundle_redacted(js);
        assert!(super::bundle_is_redacted(&marked));
        assert!(marked.ends_with(js));
        // Data mentioning the marker mid-file does not count.
        assert!(!super::bundle_is_redacted(&format!(
            "{js}\n{}",
            super::BUNDLE_REDACTION_MARKER
        )));
    }

    #[test]
    fn paths_inside_the_case_become_case_relative() {
        let case_dir = temp_case("inside");
        let inside = case_dir.join("artifacts/clips/out.mp4");
        fs::create_dir_all(inside.parent().unwrap()).unwrap();
        fs::write(&inside, b"x").unwrap();
        let input = format!(
            "{{\"output_path\":\"{}\"}}",
            inside.to_string_lossy().replace('\\', "\\\\")
        );
        let out = redact_json_text(&input, &case_dir);
        let canonical_inside = inside.canonicalize().unwrap();
        let canonical_case = case_dir.canonicalize().unwrap();
        let relative = canonical_inside
            .strip_prefix(&canonical_case)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        assert!(
            out.contains(&format!("<case>/{relative}")),
            "unexpected: {out}"
        );
        assert!(!out.contains(&case_dir.to_string_lossy().to_string()));
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn outside_paths_become_deterministic_hash_tokens() {
        let case_dir = temp_case("outside");
        let input = r#"{"source_path":"/evidence/secret/clip.mp4","id":"vid_1"}"#;
        let first = redact_json_text(input, &case_dir);
        let second = redact_json_text(input, &case_dir);
        assert_eq!(first, second, "redaction must be deterministic");
        assert!(!first.contains("/evidence/secret"), "{first}");
        assert!(first.contains("<redacted:"), "{first}");
        // Relative values pass through.
        assert!(first.contains("\"vid_1\""));
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn same_path_across_documents_maps_to_the_same_token() {
        let case_dir = temp_case("consistent");
        let a = redact_json_text(r#"{"source_path":"/e/x.mp4"}"#, &case_dir);
        let b = redact_json_text("{\"target_path\":\"/e/x.mp4\"}\n", &case_dir);
        let c = redact_json_text(r#"{"note":"copied from /e/x.mp4 yesterday"}"#, &case_dir);
        let token = |text: &str| {
            let start = text.find("<redacted:").unwrap();
            let end = text[start..].find('>').unwrap() + start;
            text[start..=end].to_string()
        };
        assert_eq!(token(&a), token(&b));
        assert_eq!(token(&a), token(&c), "embedded path uses the same token");
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn different_paths_get_different_tokens() {
        let case_dir = temp_case("distinct");
        let a = redact_json_text(r#"{"p":"/e/a.mp4"}"#, &case_dir);
        let b = redact_json_text(r#"{"p":"/e/b.mp4"}"#, &case_dir);
        assert_ne!(a, b);
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn file_urls_are_redacted_too() {
        let case_dir = temp_case("fileurl");
        let input =
            r#"{"file_url":"file:///evidence/secret/clip.mp4","n":"open file:///x/y.mp4 now"}"#;
        let out = redact_json_text(input, &case_dir);
        assert!(!out.contains("clip.mp4"), "{out}");
        assert!(!out.contains("/x/y.mp4"), "{out}");
        assert_eq!(out.matches("<redacted-url:").count(), 2, "{out}");
        assert!(out.contains(" now"), "{out}");
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn jsonl_lines_and_torn_tails_survive() {
        let case_dir = temp_case("jsonl");
        let input = "{\"output_path\":\"/e/a.mp4\"}\n{\"output_path\":\"/e/b.mp4\"}\n{\"torn";
        let out = redact_json_text(input, &case_dir);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(!lines[0].contains("/e/a.mp4"));
        assert!(!lines[1].contains("/e/b.mp4"));
        assert_eq!(lines[2], "{\"torn");
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn nested_and_array_paths_are_reached() {
        let case_dir = temp_case("nested");
        let input = r#"{"videos":[{"source_path":"/e/a.mp4","tags":["/e/nested.mp4"]}]}"#;
        let out = redact_json_text(input, &case_dir);
        assert!(!out.contains("/e/a.mp4"), "{out}");
        assert!(!out.contains("/e/nested.mp4"), "{out}");
        assert_eq!(out.matches("<redacted:").count(), 2);
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn relative_and_empty_inputs_pass_through() {
        let case_dir = temp_case("passthrough");
        assert_eq!(redact_json_text("", &case_dir), "");
        assert_eq!(
            redact_json_text(r#"{"relative_path":"cam/a.mp4"}"#, &case_dir),
            r#"{"relative_path":"cam/a.mp4"}"#
        );
        let _ = fs::remove_dir_all(case_dir);
    }

    /// I4: paths in the middle of notes, tags, error text and ffprobe tags
    /// are rewritten; the surrounding prose is kept.
    #[test]
    fn mid_string_paths_in_notes_tags_and_ffprobe_tags_are_redacted() {
        let case_dir = temp_case("midstring");
        let input = serde_json::json!({
            "note": r"원본은 C:\Evidence\Kim\cam1.mp4 에 있음; 사본 D:/backup/cam1.mp4.",
            "tags": ["/Volumes/SD/DCIM/100.MP4", "중요"],
            "ffprobe": {"format": {"tags": {"comment": "Recorded to /mnt/sd/DCIM/x.mp4 (cam 2)"}}},
            "error": r"open failed: \\nas01\share\clip.avi (os error 5)",
            "raw": "{\"filename\":\"C:\\\\Users\\\\kim\\\\a.mp4\"}",
        })
        .to_string();
        let out = redact_json_text(&input, &case_dir);
        for secret in [
            "Evidence", "Kim", "backup", "Volumes", "/mnt/sd", "nas01", "Users", "kim\\",
        ] {
            assert!(!out.contains(secret), "{secret} leaked: {out}");
        }
        for kept in [
            "원본은 ",
            " 에 있음; 사본 ",
            "중요",
            "Recorded to ",
            " (cam 2)",
            "(os error 5)",
        ] {
            assert!(out.contains(kept), "{kept} lost: {out}");
        }
        // A sentence-final period is prose, not part of the path.
        let note = serde_json::from_str::<serde_json::Value>(&out).unwrap()["note"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(note.ends_with(">."), "{note}");
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn spaced_path_values_are_one_token_and_prose_is_not_a_path() {
        let case_dir = temp_case("spaces");
        let out = redact_text(r"C:\Users\Kim Park\My Clip.mp4", &case_dir);
        assert!(out.starts_with("<redacted:") && out.ends_with('>'), "{out}");
        assert!(!out.contains("Clip") && !out.contains("Park"), "{out}");
        for prose in [
            "IN/OUT 구간 1/2, and/or 16/9",
            "https://example.com/a/b",
            "</td><td>x</td>",
            "<case>/artifacts/clips/out.mp4",
            "ratio: 4:3 a:b",
        ] {
            assert_eq!(redact_text(prose, &case_dir), prose);
        }
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn embedded_case_paths_become_case_relative_and_redaction_is_idempotent() {
        let case_dir = temp_case("embedded-case");
        let clip = case_dir.join("artifacts/clips/out.mp4");
        fs::create_dir_all(clip.parent().unwrap()).unwrap();
        fs::write(&clip, b"x").unwrap();
        let redactor = Redactor::new(&case_dir);
        let text = format!(
            "exported to {}. Gone: {}",
            clip.display(),
            case_dir.join("artifacts/deleted.mp4").display()
        );
        let once = redactor.text(&text);
        assert!(
            once.contains(
                "exported to <case>/artifacts/clips/out.mp4. Gone: <case>/artifacts/deleted.mp4"
            ),
            "{once}"
        );
        assert_eq!(redactor.text(&once), once);
        let _ = fs::remove_dir_all(case_dir);
    }

    /// R1: remote/device tokens are redacted textually — the redactor
    /// never hands them (nor any path outside the case) to the filesystem.
    #[test]
    fn remote_and_device_paths_are_redacted_without_any_filesystem_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        static SEEN: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
        fn counting(path: &std::path::Path) -> std::io::Result<PathBuf> {
            CALLS.fetch_add(1, Ordering::SeqCst);
            SEEN.lock().unwrap().push(path.to_path_buf());
            Ok(path.to_path_buf())
        }
        let case_dir = temp_case("remote-no-fs");
        let redactor = Redactor::with_canonicalizer(&case_dir, counting);
        let mut notes = vec![
            r"메모: 원본은 \\203.0.113.5\x\y 에 있음".to_string(),
            r"see \\?\UNC\203.0.113.5\share\clip.mp4 now".to_string(),
            r"pipe \\.\pipe\evil and \\?\GLOBALROOT\Device\Harddisk0".to_string(),
            "outside /evidence/secret/clip.mp4 and D:/other/x.mp4".to_string(),
        ];
        if cfg!(windows) {
            notes.push(r"device C:\x\COM1 and C:\cases\nul.txt".to_string());
        }
        for note in &notes {
            let out = redactor.text(note);
            assert!(!out.contains("203.0.113.5"), "{out}");
            assert!(!out.contains("evil") && !out.contains("Harddisk0"), "{out}");
            assert!(!out.contains("secret") && !out.contains("other"), "{out}");
            assert!(out.contains("<redacted:"), "{out}");
        }
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            0,
            "no filesystem call for remote/device/outside paths: {:?}",
            SEEN.lock().unwrap()
        );
        // A path under the case root is still resolved (once, cached).
        let inside = case_dir.join("artifacts/clip.mp4");
        let text = format!("{} {}", inside.display(), inside.display());
        let out = redactor.text(&text);
        assert!(out.starts_with("<case>/artifacts/clip.mp4"), "{out}");
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        let _ = fs::remove_dir_all(case_dir);
    }

    #[test]
    fn path_valued_object_keys_are_redacted() {
        let case_dir = temp_case("keys");
        let out = redact_json_text(r#"{"/e/secret/a.mp4":{"r":"sound"}}"#, &case_dir);
        assert!(!out.contains("secret"), "{out}");
        let _ = fs::remove_dir_all(case_dir);
    }

    /// R7-I8: an explicit depth-1 root (`HOME=/root`, a source at `/mnt`)
    /// is not a flattened root — `_root_cause` / `_mnt_point` are ordinary
    /// identifiers — while a depth-2 explicit root still is.
    #[test]
    fn depth_one_explicit_roots_are_not_flattened_roots() {
        let case_dir = temp_case("r7-i8");
        let redactor = Redactor::with_known_roots(&case_dir, &["/root", "/mnt", "/srv/kim"]);
        for kept in ["_root_cause", "_mnt_point_x", "see _root_value now"] {
            assert_eq!(redactor.text(kept), kept, "{kept}");
        }
        let out = redactor.text("built _srv_kim_cam1_mp4_proxy_1.mp4.");
        assert!(!out.contains("kim"), "{out}");
        let _ = fs::remove_dir_all(case_dir);
    }
}
