//! Guided, hash-pinned installs of external tools (docs/PLAN_TOOLS_2026-10-20.md
//! track S, principles T1–T6).
//!
//! * T1 — every installable tool is pinned in `tools/manifest.json` (version,
//!   URL, size, SHA-256 copied from the publisher's checksum file). The
//!   manifest is compiled into the binary; a tool without a published
//!   checksum (`sha256: null`) is never installed, only a manual hint shown.
//! * T2 — downloads are HTTPS-only (`curl --proto =https --tlsv1.2`) and every
//!   hop, redirects included, must name a host on the manifest allowlist.
//!   Redirects are followed by this module, not by curl, so a hop is checked
//!   before it is requested.
//! * T3 — nothing here runs on its own; the CLI (`frametrace tools install`)
//!   and `POST /api/tools/install` are the only callers.
//! * T4 — no new crates: Windows' own `%SystemRoot%\System32\curl.exe` and
//!   `tar.exe` (bsdtar), always by absolute path. Archives are listed before
//!   extraction and refused whole when any entry is absolute, climbs with
//!   `..`, is a link, or is otherwise unsafe; only the manifest's files are
//!   extracted and moved into `<exe dir>/tools/bin`.
//! * T5 — license files listed in the manifest land in `tools/bin/licenses`.
//!
//! Every failure removes the staging directory, so no partial file is left
//! behind; every attempt (success or failure) appends one line to
//! `<exe dir>/tools/install-audit.jsonl`.

// The installers only run on Windows; the pure parts (manifest, URL and
// entry checks) are compiled and unit-tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod archive;
mod pip;
#[cfg(test)]
pub(crate) mod tests;

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

/// The pinned manifest, compiled in (never read from disk at run time).
pub const MANIFEST_JSON: &str = include_str!("../../tools/manifest.json");

/// Hash-pinned pip requirement files, compiled in by manifest path. Add
/// `("tools/<name>.requirements.txt", include_str!("../../tools/<name>.requirements.txt"))`
/// when a pip tool becomes publishable with every digest pinned; a
/// manifest `requirements_file` missing from this table is not installable.
const REQUIREMENTS: &[(&str, &str)] = &[];

/// Platform key the manifest's `platform` field is matched against.
pub const PLATFORM: &str = if cfg!(all(windows, target_arch = "x86_64")) {
    "windows-x86_64"
} else if cfg!(windows) {
    "windows-other"
} else {
    "unsupported"
};

/// Refusal off Windows (the installers drive Windows' built-in curl/tar).
pub const UNSUPPORTED_PLATFORM: &str = "unsupported on this platform: guided tool installs need Windows (built-in curl.exe and tar.exe) — install the tool manually and put it on PATH";

/// Refusal while another install (this or another process) holds the lock.
pub const INSTALL_LOCKED: &str = "다른 도구 설치가 진행 중입니다 — 끝난 뒤 다시 시도하십시오.";

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub allowed_hosts: Vec<String>,
    pub tools: Vec<ToolEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Archive,
    Pip,
}

impl ToolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolKind::Archive => "archive",
            ToolKind::Pip => "pip",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileMap {
    /// Entry path inside the archive (`/`-separated).
    pub from: String,
    /// Plain file name under `tools/bin` (or `tools/bin/licenses`).
    pub to: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolEntry {
    pub name: String,
    pub title: String,
    pub kind: ToolKind,
    pub required: bool,
    #[serde(default)]
    pub on_demand: bool,
    pub platform: String,
    pub version: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub source_url: Option<String>,
    pub url: Option<String>,
    pub file_name: Option<String>,
    pub format: Option<String>,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub checksum_source: Option<String>,
    pub unavailable_reason: Option<String>,
    #[serde(default)]
    pub files: Vec<FileMap>,
    #[serde(default)]
    pub license_files: Vec<FileMap>,
    pub provides: Vec<String>,
    pub manual_hint: Option<String>,
    // pip tools
    pub package: Option<String>,
    pub index_url: Option<String>,
    pub requirements_file: Option<String>,
}

/// The parsed, validated built-in manifest.
pub fn manifest() -> Result<&'static Manifest, String> {
    static PARSED: std::sync::OnceLock<Result<Manifest, String>> = std::sync::OnceLock::new();
    PARSED
        .get_or_init(|| {
            let manifest: Manifest = serde_json::from_str(MANIFEST_JSON)
                .map_err(|err| format!("tools/manifest.json is malformed: {err}"))?;
            validate_manifest(&manifest)?;
            Ok(manifest)
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub fn find_tool<'a>(manifest: &'a Manifest, name: &str) -> Option<&'a ToolEntry> {
    manifest.tools.iter().find(|tool| tool.name == name)
}

/// The manifest entry that provides binary `binary` (e.g. `ffprobe` →
/// `ffmpeg`).
pub fn tool_for_binary(binary: &str) -> Option<&'static ToolEntry> {
    let manifest = manifest().ok()?;
    manifest
        .tools
        .iter()
        .find(|tool| tool.provides.iter().any(|p| p == binary))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A plain destination file name: no separators, no `..`, no ADS colon,
/// no reserved device name.
pub(crate) fn plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && !crate::util::is_reserved_device_name(name)
}

/// Whole-archive safety rule for one listed entry name (T4). Refused:
/// empty names, absolute paths (`/x`, `\x`, `C:...`), backslashes, `..`
/// components, ADS/drive colons, control characters and names that did
/// not decode (U+FFFD from a lossy listing).
pub(crate) fn archive_entry_is_safe(name: &str) -> bool {
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return false;
    }
    if name
        .chars()
        .any(|c| c.is_control() || c == '\\' || c == ':' || c == '\u{fffd}')
    {
        return false;
    }
    trimmed
        .split('/')
        .all(|part| !part.is_empty() && part != ".." && part != "." && !part.ends_with(' '))
}

/// Manifest `from` paths are also passed to `tar -x` as member names, so
/// on top of [`archive_entry_is_safe`] they carry no glob metacharacter
/// and never start with `-`.
fn manifest_member_is_safe(name: &str) -> bool {
    archive_entry_is_safe(name)
        && !name.ends_with('/')
        && !name.starts_with('-')
        && !name.contains(['*', '?', '[', ']'])
}

/// Whole-archive listing check (T4), from bsdtar's `-tf` (names) and
/// `-tvf` (mode column) output. One unsafe name, one link or special
/// entry, or a missing / duplicated (also case-insensitively, as Windows
/// would collide them) manifest member refuses the whole archive.
pub(crate) fn check_listing(
    names: &[String],
    verbose: &[String],
    members: &[&str],
) -> Result<(), String> {
    if names.is_empty() {
        return Err("archive refused: it is empty".into());
    }
    if let Some(bad) = names.iter().find(|name| !archive_entry_is_safe(name)) {
        return Err(format!(
            "archive refused: unsafe entry {bad:?} (absolute path, '..', drive/stream colon or invalid name)"
        ));
    }
    if verbose.len() != names.len() {
        return Err("archive refused: inconsistent listing".into());
    }
    if let Some(bad) = verbose
        .iter()
        .find(|line| !matches!(line.as_bytes().first(), Some(b'-' | b'd')))
    {
        return Err(format!("archive refused: link or special entry: {bad}"));
    }
    for member in members {
        let exact = names.iter().filter(|name| name.as_str() == *member).count();
        let folded = names
            .iter()
            .filter(|name| name.eq_ignore_ascii_case(member))
            .count();
        if exact == 0 {
            return Err(format!("archive refused: {member} is not in the archive"));
        }
        if exact > 1 || folded > 1 {
            return Err(format!("archive refused: {member} appears more than once"));
        }
    }
    Ok(())
}

pub fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    if manifest.schema != 1 {
        return Err(format!(
            "tools/manifest.json: unsupported schema {}",
            manifest.schema
        ));
    }
    if manifest.allowed_hosts.is_empty() {
        return Err("tools/manifest.json: allowed_hosts is empty".into());
    }
    for host in &manifest.allowed_hosts {
        if host.is_empty() || host != &host.to_ascii_lowercase() || host.contains([':', '/', '@']) {
            return Err(format!("tools/manifest.json: bad allowed host {host:?}"));
        }
    }
    let net = NetPolicy::for_manifest(manifest);
    let mut names = std::collections::HashSet::new();
    for tool in &manifest.tools {
        let ctx = |what: &str| format!("tools/manifest.json: tool {:?}: {what}", tool.name);
        if !valid_name(&tool.name) || !names.insert(tool.name.as_str()) {
            return Err(ctx("invalid or duplicate name"));
        }
        if tool.provides.is_empty() || !tool.provides.iter().all(|p| valid_name(p)) {
            return Err(ctx("provides must list plain binary names"));
        }
        for map in tool.files.iter().chain(&tool.license_files) {
            if !manifest_member_is_safe(&map.from) || !plain_file_name(&map.to) {
                return Err(ctx(&format!(
                    "unsafe file mapping {:?} -> {:?}",
                    map.from, map.to
                )));
            }
        }
        if let Some(sha) = &tool.sha256 {
            if !is_sha256_hex(sha) {
                return Err(ctx("sha256 must be 64 lowercase hex digits"));
            }
            if tool.kind != ToolKind::Archive {
                return Err(ctx("sha256 is only meaningful for archive tools"));
            }
            if tool.size.unwrap_or(0) == 0 {
                return Err(ctx("a pinned archive needs its exact size"));
            }
            if tool.checksum_source.as_deref().unwrap_or("").is_empty() {
                return Err(ctx("a pinned archive must name its checksum source"));
            }
            if tool.format.as_deref() != Some("zip") {
                return Err(ctx("only zip archives are supported"));
            }
            if tool.files.is_empty() {
                return Err(ctx("a pinned archive must list the files to install"));
            }
            let url = tool
                .url
                .as_deref()
                .ok_or_else(|| ctx("pinned archive without url"))?;
            let parsed = check_url(url, &net).map_err(|err| ctx(&err))?;
            if parsed
                .path
                .split(['/', '?', '#'])
                .any(|seg| seg.eq_ignore_ascii_case("latest"))
            {
                return Err(ctx("\"latest\" URLs are not pinned (T1)"));
            }
            let last = parsed.path.rsplit('/').next().unwrap_or("");
            let file_name = tool.file_name.as_deref().unwrap_or("");
            if !plain_file_name(file_name) || last != file_name {
                return Err(ctx("file_name must be the URL's last path segment"));
            }
        }
        if tool.kind == ToolKind::Pip {
            if let Some(index) = &tool.index_url
                && index != pip::PINNED_INDEX_URL
            {
                return Err(ctx("pip tools install from https://pypi.org/simple only"));
            }
            if tool.requirements_file.is_some() && tool.version.is_none() {
                return Err(ctx("a pip tool with requirements needs an exact version"));
            }
        }
    }
    Ok(())
}

/// Why `tool` cannot be installed here, or `None` when it can.
pub fn not_installable_reason(tool: &ToolEntry) -> Option<String> {
    if tool.platform != PLATFORM {
        return Some(if cfg!(windows) {
            format!(
                "this build targets {PLATFORM}, the tool is pinned for {}",
                tool.platform
            )
        } else {
            UNSUPPORTED_PLATFORM.to_string()
        });
    }
    let unavailable = || {
        tool.unavailable_reason
            .clone()
            .unwrap_or_else(|| "no published checksum is pinned for this tool".into())
    };
    match tool.kind {
        ToolKind::Archive => {
            if tool.sha256.is_none() || tool.size.is_none() || tool.url.is_none() {
                return Some(unavailable());
            }
        }
        ToolKind::Pip => match requirements_text(tool) {
            None => return Some(unavailable()),
            Some(text) => {
                if let Err(err) = pip::check_requirements(text, tool) {
                    return Some(err);
                }
            }
        },
    }
    None
}

/// For a pip tool: the "Python 필요" refusal when no interpreter resolves
/// (checked before an install is started), else `None`.
pub fn python_missing_reason(tool: &ToolEntry) -> Option<String> {
    if tool.kind != ToolKind::Pip {
        return None;
    }
    pip::RealPython::resolve().err()
}

pub(crate) fn requirements_text(tool: &ToolEntry) -> Option<&'static str> {
    let file = tool.requirements_file.as_deref()?;
    REQUIREMENTS
        .iter()
        .find(|(path, _)| *path == file)
        .map(|(_, text)| *text)
}

// ---------------------------------------------------------------- status

/// Where installed tools live: `<exe dir>/tools` (`bin/`, `bin/licenses/`,
/// `python/<name>/`, `install-audit.jsonl`).
#[derive(Debug, Clone)]
pub struct InstallDirs {
    pub tools_dir: PathBuf,
}

impl InstallDirs {
    pub fn for_current_exe() -> Result<Self, String> {
        let exe = std::env::current_exe()
            .map_err(|err| format!("cannot locate the FrameTrace executable: {err}"))?;
        let dir = exe
            .parent()
            .ok_or("the FrameTrace executable has no parent directory")?;
        Ok(Self {
            tools_dir: dir.join("tools"),
        })
    }
    pub fn bin(&self) -> PathBuf {
        self.tools_dir.join("bin")
    }
    pub fn licenses(&self) -> PathBuf {
        self.bin().join("licenses")
    }
    pub fn python_env(&self, name: &str) -> PathBuf {
        self.tools_dir.join("python").join(name)
    }
    pub fn audit_log(&self) -> PathBuf {
        self.tools_dir.join("install-audit.jsonl")
    }
}

/// The private venv's console script for an on-demand pip tool, when it
/// was installed by [`install`] (`<exe dir>/tools/python/<name>/Scripts/<name>.exe`).
pub fn private_venv_executable(name: &str) -> Option<PathBuf> {
    let dirs = InstallDirs::for_current_exe().ok()?;
    let exe = pip::venv_script(&dirs.python_env(name), name);
    exe.is_file().then_some(exe)
}

#[derive(Debug, Clone)]
pub struct ToolStatus {
    /// `(binary, resolved absolute path)` per provided binary.
    pub paths: Vec<(String, Option<String>)>,
}

impl ToolStatus {
    pub fn installed(&self) -> bool {
        self.paths.iter().all(|(_, path)| path.is_some())
    }
    pub fn partial(&self) -> bool {
        !self.installed() && self.paths.iter().any(|(_, path)| path.is_some())
    }
    pub fn label(&self) -> &'static str {
        if self.installed() {
            "installed"
        } else if self.partial() {
            "partial"
        } else {
            "missing"
        }
    }
}

/// Resolution-only status (PATH, then `tools/bin`, as the pipeline would
/// run the tool) — never spawns anything.
pub fn status(tool: &ToolEntry) -> ToolStatus {
    let paths = tool
        .provides
        .iter()
        .map(|binary| {
            let path = if binary == "deepfake-lens" {
                crate::deepfake::resolved_lens_binary()
            } else {
                crate::tool_policy::resolve_tool_binary(binary, &[binary.as_str()]).ok()
            };
            (binary.clone(), path)
        })
        .collect();
    ToolStatus { paths }
}

/// Doctor/UI hint for a missing binary: the install command when the
/// providing tool is installable here, its manual hint otherwise.
pub fn install_hint_for_binary(binary: &str) -> Option<String> {
    let tool = tool_for_binary(binary)?;
    if tool.on_demand {
        return None;
    }
    Some(match not_installable_reason(tool) {
        None => format!("설치: frametrace tools install {}", tool.name),
        Some(_) => tool
            .manual_hint
            .clone()
            .unwrap_or_else(|| format!("수동 설치 필요 ({})", tool.name)),
    })
}

/// JSON summary of one tool (shared by `GET /api/tools` and
/// `frametrace tools list --json`).
pub fn tool_json(tool: &ToolEntry, status: &ToolStatus) -> serde_json::Value {
    // A pip tool also needs an interpreter (resolution only).
    let reason = not_installable_reason(tool).or_else(|| python_missing_reason(tool));
    let host = match tool.kind {
        ToolKind::Archive => tool
            .url
            .as_deref()
            .and_then(|url| check_url(url, &NetPolicy::for_manifest(manifest().ok()?)).ok())
            .map(|parsed| parsed.host),
        // The pinned index (https://pypi.org/simple).
        ToolKind::Pip => Some("pypi.org".to_string()),
    };
    serde_json::json!({
        "name": tool.name,
        "title": tool.title,
        // Track U's names (docs/ui-rebuild-contract.md §28).
        "label": tool.title,
        "pinned_version": tool.version,
        "source_host": host,
        "size_bytes": tool.size,
        "unavailable_reason": reason,
        "kind": tool.kind.as_str(),
        "required": tool.required,
        "on_demand": tool.on_demand,
        "version": tool.version,
        "license": tool.license,
        "homepage": tool.homepage,
        "source_url": tool.source_url,
        "provides": tool.provides,
        "status": status.label(),
        "installed": status.installed(),
        "paths": status
            .paths
            .iter()
            .map(|(binary, path)| (binary.clone(), serde_json::json!(path)))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
        "installable": reason.is_none(),
        "not_installable_reason": reason,
        "manual_hint": tool.manual_hint,
        "download": if tool.kind == ToolKind::Archive && tool.sha256.is_some() {
            serde_json::json!({
                "file_name": tool.file_name,
                "url": tool.url,
                "host": host,
                "size": tool.size,
                "sha256": tool.sha256,
                "checksum_source": tool.checksum_source,
            })
        } else {
            serde_json::Value::Null
        },
    })
}

// --------------------------------------------------------------- progress

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    Queued = 0,
    Download = 1,
    Verify = 2,
    Extract = 3,
    Install = 4,
    Venv = 5,
    Pip = 6,
    Done = 7,
    Failed = 8,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Queued => "queued",
            Stage::Download => "download",
            Stage::Verify => "verify",
            Stage::Extract => "extract",
            Stage::Install => "install",
            Stage::Venv => "venv",
            Stage::Pip => "pip",
            Stage::Done => "done",
            Stage::Failed => "failed",
        }
    }
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Stage::Download,
            2 => Stage::Verify,
            3 => Stage::Extract,
            4 => Stage::Install,
            5 => Stage::Venv,
            6 => Stage::Pip,
            7 => Stage::Done,
            8 => Stage::Failed,
            _ => Stage::Queued,
        }
    }
}

/// Lock-free install progress, readable while the install runs (the
/// workstation's status poll never waits on the installer).
#[derive(Debug, Default)]
pub struct Progress {
    stage: AtomicU8,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    cancel: AtomicBool,
}

impl Progress {
    pub fn stage(&self) -> Stage {
        Stage::from_u8(self.stage.load(Ordering::SeqCst))
    }
    pub fn set_stage(&self, stage: Stage) {
        self.stage.store(stage as u8, Ordering::SeqCst);
    }
    pub fn bytes(&self) -> (u64, u64) {
        (
            self.bytes_done.load(Ordering::SeqCst),
            self.bytes_total.load(Ordering::SeqCst),
        )
    }
    pub fn set_bytes(&self, done: u64, total: u64) {
        self.bytes_done.store(done, Ordering::SeqCst);
        self.bytes_total.store(total, Ordering::SeqCst);
    }
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// What a successful install did.
#[derive(Debug, Clone, Default)]
pub struct InstallOutcome {
    /// Absolute paths of every file written (binaries, licenses, venv exe).
    pub installed_files: Vec<PathBuf>,
    /// Set when the audit line could not be written (the install itself
    /// succeeded — reported, never swallowed).
    pub audit_warning: Option<String>,
}

// ------------------------------------------------------------- net policy

/// Which URLs a download may touch. Production policy = the manifest's
/// host allowlist, HTTPS only. Unit tests alone can admit plain HTTP to
/// `127.0.0.1` (their local fake source); that switch does not exist in
/// a non-test build.
#[derive(Debug, Clone)]
pub struct NetPolicy {
    allowed_hosts: Vec<String>,
    loopback_http: bool,
}

impl NetPolicy {
    pub fn for_manifest(manifest: &Manifest) -> Self {
        Self {
            allowed_hosts: manifest.allowed_hosts.clone(),
            loopback_http: false,
        }
    }
    #[cfg(test)]
    pub(crate) fn test_loopback(hosts: &[&str]) -> Self {
        Self {
            allowed_hosts: hosts.iter().map(|h| h.to_string()).collect(),
            loopback_http: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedUrl {
    pub(crate) https: bool,
    pub(crate) host: String,
    pub(crate) path: String,
}

/// T2 gate for one URL (the manifest URL and every redirect hop).
pub(crate) fn check_url(url: &str, net: &NetPolicy) -> Result<ParsedUrl, String> {
    if url.len() > 4096
        || url
            .bytes()
            .any(|b| b <= 0x20 || b >= 0x7f || b == b'\\' || b == b'"')
    {
        return Err("refused URL: control, space, backslash or non-ASCII characters".into());
    }
    let (https, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://")
        && net.loopback_http
    {
        (false, rest)
    } else {
        return Err(format!("refused URL (HTTPS only): {}", redact_query(url)));
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    if authority.contains('@') || authority.contains('[') {
        return Err("refused URL: credentials or IP literal in the host".into());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty() {
        return Err("refused URL: no host".into());
    }
    if let Some(port) = port {
        let port: u16 = port
            .parse()
            .map_err(|_| "refused URL: bad port".to_string())?;
        if !net.loopback_http && port != 443 {
            return Err("refused URL: non-default HTTPS port".into());
        }
    }
    if !net.allowed_hosts.contains(&host) {
        return Err(format!(
            "refused URL: host {host} is not on the tool source allowlist ({})",
            net.allowed_hosts.join(", ")
        ));
    }
    if !https && host != "127.0.0.1" {
        return Err("refused URL: plain HTTP is only allowed to the test loopback".into());
    }
    Ok(ParsedUrl {
        https,
        host,
        path: path.to_string(),
    })
}

/// A URL for messages: the query (signed redirect tokens) is dropped.
pub(crate) fn redact_query(url: &str) -> String {
    match url.find(['?', '#']) {
        Some(at) => format!("{}?…", &url[..at]),
        None => url.to_string(),
    }
}

// ---------------------------------------------------------------- install

/// Installs manifest tool `name` into `<exe dir>/tools` (T3: callers are
/// user actions only). Blocking: network and disk work happen on the
/// calling thread — never call this under the workstation's state lock.
pub fn install(name: &str, progress: &Progress) -> Result<InstallOutcome, String> {
    let manifest = manifest()?;
    let tool = find_tool(manifest, name)
        .ok_or_else(|| format!("unknown tool '{name}' (see frametrace tools list)"))?;
    let dirs = InstallDirs::for_current_exe()?;
    install_entry(tool, &dirs, &NetPolicy::for_manifest(manifest), progress)
}

/// [`install`] with explicit target and network policy (the unit tests'
/// fake source and temp tools dir).
pub(crate) fn install_entry(
    tool: &ToolEntry,
    dirs: &InstallDirs,
    net: &NetPolicy,
    progress: &Progress,
) -> Result<InstallOutcome, String> {
    if let Some(reason) = not_installable_reason(tool) {
        return Err(format!(
            "{} cannot be installed automatically: {reason}",
            tool.name
        ));
    }
    if !cfg!(windows) {
        return Err(UNSUPPORTED_PLATFORM.to_string());
    }
    let result = run_platform_install(tool, dirs, net, progress);
    progress.set_stage(if result.is_ok() {
        Stage::Done
    } else {
        Stage::Failed
    });
    let audit = audit_line(tool, result.as_ref().map(|o| o.installed_files.as_slice()));
    let audit_result = append_audit(&dirs.audit_log(), &audit);
    match result {
        Ok(mut outcome) => {
            outcome.audit_warning = audit_result.err();
            Ok(outcome)
        }
        Err(err) => Err(err),
    }
}

#[cfg(windows)]
fn run_platform_install(
    tool: &ToolEntry,
    dirs: &InstallDirs,
    net: &NetPolicy,
    progress: &Progress,
) -> Result<InstallOutcome, String> {
    let _lock = InstallLock::acquire(&dirs.tools_dir)?;
    match tool.kind {
        ToolKind::Archive => archive::install_archive(tool, dirs, net, progress),
        ToolKind::Pip => {
            let text = requirements_text(tool).ok_or("no pinned requirements")?;
            let python = pip::RealPython::resolve()?;
            pip::install_pip(tool, text, dirs, &python, progress)
        }
    }
}

#[cfg(not(windows))]
fn run_platform_install(
    _tool: &ToolEntry,
    _dirs: &InstallDirs,
    _net: &NetPolicy,
    _progress: &Progress,
) -> Result<InstallOutcome, String> {
    Err(UNSUPPORTED_PLATFORM.to_string())
}

/// Cross-process install lock (`tools/.install.lock`, fs2 exclusive): the
/// CLI and a running workstation never install at the same time.
pub(crate) struct InstallLock {
    _file: std::fs::File,
}

impl InstallLock {
    pub(crate) fn acquire(tools_dir: &Path) -> Result<Self, String> {
        use fs2::FileExt;
        std::fs::create_dir_all(tools_dir).map_err(|err| {
            format!(
                "cannot create {} (is the FrameTrace folder writable?): {err}",
                tools_dir.display()
            )
        })?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(tools_dir.join(".install.lock"))
            .map_err(|err| format!("cannot open the install lock: {err}"))?;
        file.try_lock_exclusive()
            .map_err(|_| INSTALL_LOCKED.to_string())?;
        Ok(Self { _file: file })
    }
}

/// A fresh, uniquely named directory under `parent` that is removed
/// (recursively) when dropped — on success and on every failure path.
pub(crate) struct StagingDir {
    path: PathBuf,
}

impl StagingDir {
    pub(crate) fn create(parent: &Path, prefix: &str) -> Result<Self, String> {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
        for _ in 0..8 {
            let path = parent.join(format!("{prefix}{}", random_hex()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(format!(
                        "cannot create staging dir {}: {err}",
                        path.display()
                    ));
                }
            }
        }
        Err("cannot create a unique staging directory".into())
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn audit_line(tool: &ToolEntry, result: Result<&[PathBuf], &String>) -> String {
    let host = tool.url.as_deref().map(|url| {
        let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
        rest.split(['/', '?']).next().unwrap_or("").to_string()
    });
    let mut value = serde_json::json!({
        "ts_unix": crate::util::now_unix().unwrap_or(0),
        "event": "tool-install",
        "tool": tool.name,
        "kind": tool.kind.as_str(),
        "version": tool.version,
        "url": tool.url,
        "host": host,
        "size": tool.size,
        "sha256": tool.sha256,
        "checksum_source": tool.checksum_source,
        "requirements_sha256": requirements_text(tool).map(|t| crate::sha256::digest_bytes(t.as_bytes())),
        "host_id": crate::util::host_id(),
    });
    match result {
        Ok(files) => {
            value["result"] = "ok".into();
            value["files"] = files
                .iter()
                .map(|f| serde_json::Value::from(f.display().to_string()))
                .collect();
        }
        Err(err) => {
            value["result"] = "failed".into();
            value["error"] = err.as_str().into();
        }
    }
    value.to_string()
}

fn append_audit(path: &Path, line: &str) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| format!("install audit log {} not writable: {err}", path.display()))?;
    writeln!(file, "{line}")
        .and_then(|()| file.sync_all())
        .map_err(|err| format!("install audit log {} not written: {err}", path.display()))
}

/// 64 random bits as hex (staging-directory names).
pub(crate) fn random_hex() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        // Entropy failure: fall back to pid + time (uniqueness, not secrecy).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        bytes = (nanos ^ u64::from(std::process::id())).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Moves `(src, dest)` pairs into place, replacing existing destinations.
/// Each existing destination is first renamed into `backup_dir`; when any
/// move fails, every destination already moved is removed and its backup
/// restored, so the target directory is either fully updated or unchanged.
pub(crate) fn move_all_or_nothing(
    moves: &[(PathBuf, PathBuf)],
    backup_dir: &Path,
) -> Result<(), String> {
    std::fs::create_dir_all(backup_dir)
        .map_err(|err| format!("cannot create {}: {err}", backup_dir.display()))?;
    // (dest, Some(backup)) for each completed move.
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let mut failure = None;
    for (index, (src, dest)) in moves.iter().enumerate() {
        if let Some(parent) = dest.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            failure = Some(format!("cannot create {}: {err}", parent.display()));
            break;
        }
        let backup = match std::fs::symlink_metadata(dest) {
            Ok(_) => {
                let backup = backup_dir.join(format!("{index}.bak"));
                if let Err(err) = std::fs::rename(dest, &backup) {
                    failure = Some(format!(
                        "cannot replace {} (is it running?): {err}",
                        dest.display()
                    ));
                    break;
                }
                Some(backup)
            }
            Err(_) => None,
        };
        if let Err(err) = std::fs::rename(src, dest) {
            if let Some(backup) = &backup {
                let _ = std::fs::rename(backup, dest);
            }
            failure = Some(format!("cannot move {} into place: {err}", dest.display()));
            break;
        }
        done.push((dest.clone(), backup));
    }
    if let Some(failure) = failure {
        for (dest, backup) in done.into_iter().rev() {
            let _ = std::fs::remove_file(&dest);
            if let Some(backup) = backup {
                let _ = std::fs::rename(&backup, &dest);
            }
        }
        return Err(failure);
    }
    Ok(())
}
