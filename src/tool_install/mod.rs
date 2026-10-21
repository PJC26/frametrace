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
//! * T5 — nothing is bundled; an installable tool carries a distribution
//!   notice (shown before download) and its license files land in
//!   `tools/bin/licenses` (docs/PLAN_TOOLS_C_2026-10-20.md).
//!
//! Every failure removes the staging directory, so no partial file is left
//! behind (stale `tools/.staging-*` left by a killed run, and replaced pip
//! venvs `tools/python/.<name>.old-*` that were still in use, are swept
//! before each install and at server start); every attempt — success, failure or
//! a refusal before anything started — appends one hash-chained line
//! (`audit::append_chained_jsonl`, `previous_entry_sha256`/`entry_sha256`)
//! to `<exe dir>/tools/install-audit.jsonl`.

// The installers only run on Windows; the pure parts (manifest, URL and
// entry checks) are compiled and unit-tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod archive;
mod pip;
mod rollback;
pub use rollback::{
    RESTORE_FILE, RollbackBackup, rollback_backups, rollback_backups_for_current_exe, rollback_hint,
};
#[cfg(test)]
pub(crate) mod tests;

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// The pinned manifest, compiled in (never read from disk at run time).
pub const MANIFEST_JSON: &str = include_str!("../../tools/manifest.json");

/// Hash-pinned pip requirement files, compiled in by manifest path. Add
/// `("tools/<name>.requirements.txt", include_str!("../../tools/<name>.requirements.txt"))`
/// when a pip tool becomes publishable with every digest pinned; a
/// manifest `requirements_file` missing from this table is not installable.
const REQUIREMENTS: &[(&str, &str)] = &[];

/// License texts FrameTrace ships itself (by manifest path), installed
/// next to a tool's own license files. The manifest pins each file's
/// SHA-256; `validate_manifest` refuses a mismatch, so the compiled-in
/// text is exactly the pinned one.
const BUNDLED_LICENSES: &[(&str, &str)] = &[(
    "tools/licenses/COPYING.GPLv3",
    include_str!("../../tools/licenses/COPYING.GPLv3"),
)];

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
pub struct BundledLicense {
    /// Repository path (a key of the compiled-in license table).
    pub file: String,
    /// Plain file name under `tools/bin/licenses`.
    pub to: String,
    pub sha256: String,
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
    /// Distribution notice shown before anything is downloaded (CLI
    /// prompt, UI confirm dialog): the examiner fetches the file from the
    /// publisher, FrameTrace does not distribute it (T5). Required for
    /// every installable archive tool (`validate_manifest`).
    #[serde(default)]
    pub notice: Option<String>,
    /// English variant of `notice` (UI locale `en`).
    #[serde(default)]
    pub notice_en: Option<String>,
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
    /// Repository license texts installed with the tool (e.g. the GPLv3
    /// text the LGPLv3 incorporates, which the upstream zip lacks).
    #[serde(default)]
    pub bundled_licenses: Vec<BundledLicense>,
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
        for license in &tool.bundled_licenses {
            if !plain_file_name(&license.to) {
                return Err(ctx(&format!("unsafe license name {:?}", license.to)));
            }
            let text = bundled_license_text(&license.file)
                .ok_or_else(|| ctx(&format!("{} is not compiled in", license.file)))?;
            if crate::sha256::digest_bytes(text.as_bytes()) != license.sha256 {
                return Err(ctx(&format!(
                    "{} does not match its pinned sha256",
                    license.file
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
            // T5: the examiner downloads the file from the publisher; the
            // prompt and the dialog say so before anything is fetched.
            if tool.notice.as_deref().is_none_or(|n| n.trim().is_empty()) {
                return Err(ctx(
                    "installable tools must carry a distribution notice (T5)",
                ));
            }
        }
        if tool
            .notice_en
            .as_deref()
            .is_some_and(|n| n.trim().is_empty())
        {
            return Err(ctx("notice_en must not be empty when present"));
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

pub(crate) fn bundled_license_text(file: &str) -> Option<&'static str> {
    BUNDLED_LICENSES
        .iter()
        .find(|(path, _)| *path == file)
        .map(|(_, text)| *text)
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
    /// `tools/python`: the private venvs of on-demand pip tools.
    pub fn python_root(&self) -> PathBuf {
        self.tools_dir.join("python")
    }
    pub fn python_env(&self, name: &str) -> PathBuf {
        self.python_root().join(name)
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
    /// `(binary, PATH copy)` per provided binary: the different copy on
    /// PATH that the tools/bin one takes precedence over
    /// ([`crate::tool_policy::shadowed_by`]), else `None`.
    pub shadowed_by: Vec<(String, Option<String>)>,
    /// Fix round 13 L2: the previous files an incomplete rollback of this
    /// tool left in `tools/rollback-backup-*` (or a kept staging dir /
    /// `tools/python/rollback-backup-*`). Non-empty means `installed` may
    /// be a mix of new and old files — surfaced as a warning, never
    /// guessed away.
    pub rollback_backups: Vec<RollbackBackup>,
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

/// Resolution-only status (`tools/bin`, then PATH, as the pipeline would
/// run the tool) — never spawns anything.
/// PATH is walked once for all of the tool's binaries.
///
/// Fix round 14 L1: `backups` is [`rollback_backups`] of the `tools` dir
/// in use, computed once by the caller for every tool it lists (sizing a
/// kept venv walks all of its files); only this tool's are kept.
pub fn status(tool: &ToolEntry, backups: &[RollbackBackup]) -> ToolStatus {
    let path = crate::tool_policy::PathDirs::from_env();
    let mut paths = Vec::new();
    let mut shadowed_by = Vec::new();
    for binary in &tool.provides {
        if binary == "deepfake-lens" {
            // Its own order (FRAMETRACE_DEEPFAKE_LENS, the private venv,
            // then tools/bin / PATH): no tools/bin-vs-PATH shadow.
            paths.push((binary.clone(), crate::deepfake::resolved_lens_binary()));
            shadowed_by.push((binary.clone(), None));
            continue;
        }
        let lookup = crate::tool_policy::lookup_bare(binary, &path);
        paths.push((binary.clone(), lookup.used));
        shadowed_by.push((binary.clone(), lookup.shadowed_by));
    }
    let rollback_backups = backups
        .iter()
        .filter(|backup| backup.tool.as_deref() == Some(tool.name.as_str()))
        .cloned()
        .collect();
    ToolStatus {
        paths,
        shadowed_by,
        rollback_backups,
    }
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
        // T5: shown by the confirm dialog before anything is fetched.
        "notice": tool.notice,
        "notice_en": tool.notice_en,
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
        // Per binary: a different copy on PATH that the tools/bin one
        // takes precedence over (not used by FrameTrace; the UI warns so
        // the examiner can check it is not planted), else null.
        "shadowed_by": status
            .shadowed_by
            .iter()
            .map(|(binary, path)| (binary.clone(), serde_json::json!(path)))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
        // Fix round 13 L2: an incomplete rollback left previous files
        // behind — `installed` may be new and old files mixed.
        "rollback_incomplete": !status.rollback_backups.is_empty(),
        "rollback_note": status.rollback_backups.first().map(rollback_hint),
        "rollback_backups": status
            .rollback_backups
            .iter()
            .map(|backup| backup.path.display().to_string())
            .collect::<Vec<_>>(),
        "installable": reason.is_none(),
        "not_installable_reason": reason,
        "manual_hint": tool.manual_hint,
        // The resolved ffmpeg's encoders (a build without libx264 — e.g.
        // the BtbN one the installer fetches — uses another H.264 encoder;
        // `note` says which). Probed once
        // per binary (cached), never under the server's state lock.
        "encoders": status
            .paths
            .iter()
            .find(|(binary, _)| binary == "ffmpeg")
            .and_then(|(_, path)| path.as_deref())
            .map(|path| crate::ffmpeg_encoders::for_binary(path).to_json()),
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
///
/// Cancellation is a three-state latch: none → requested (the worker
/// stops at the next check: curl is killed, or the next stage boundary
/// refuses to start) or none → locked (the worker entered a stage that
/// must run to the end — moving files into place, building the venv,
/// pip). The two transitions race on one compare-exchange, so a cancel
/// is either honoured or refused with [`NOT_CANCELLABLE`], never
/// silently dropped.
#[derive(Debug, Default)]
pub struct Progress {
    stage: AtomicU8,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    cancel: AtomicU8,
}

const CANCEL_NONE: u8 = 0;
const CANCEL_REQUESTED: u8 = 1;
const CANCEL_LOCKED: u8 = 2;

/// `POST /api/tools/install-cancel` during a stage that cannot stop.
pub const NOT_CANCELLABLE: &str =
    "지금 단계(파일 이동·Python 환경 구성)는 취소할 수 없습니다 — 끝날 때까지 기다리십시오.";

/// The worker's error when it stopped on a cancel request.
pub const INSTALL_CANCELLED: &str = "install cancelled";

/// Start of an install error that refused because something is in use
/// (the audit line then carries `"code": "busy"`).
pub const BUSY_PREFIX: &str = "busy: ";

/// Start of an install error whose all-or-nothing rollback could not put
/// every previous file back (Fix round 12 H-a): the audit line carries
/// `"code": "rollback_incomplete"` and the error names each file left
/// wrong and where the previous copies were kept.
pub const ROLLBACK_INCOMPLETE_PREFIX: &str = "rollback_incomplete: ";

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
    /// Asks the worker to stop. `Err(NOT_CANCELLABLE)` once it entered a
    /// non-cancellable stage (nothing changes then).
    pub fn request_cancel(&self) -> Result<(), &'static str> {
        match self.cancel.compare_exchange(
            CANCEL_NONE,
            CANCEL_REQUESTED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) | Err(CANCEL_REQUESTED) => Ok(()),
            Err(_) => Err(NOT_CANCELLABLE),
        }
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst) == CANCEL_REQUESTED
    }
    /// Whether a cancel request would still be honoured now.
    pub fn cancellable(&self) -> bool {
        self.cancel.load(Ordering::SeqCst) != CANCEL_LOCKED
    }
    /// Stage boundary: `Err(INSTALL_CANCELLED)` when a cancel was requested.
    pub(crate) fn check_cancel(&self) -> Result<(), String> {
        if self.cancelled() {
            Err(INSTALL_CANCELLED.to_string())
        } else {
            Ok(())
        }
    }
    /// Enters a stage that must run to the end: from now on a cancel
    /// request is refused. `Err(INSTALL_CANCELLED)` (and nothing entered)
    /// when a cancel won the race.
    pub(crate) fn enter_non_cancellable(&self, stage: Stage) -> Result<(), String> {
        match self.cancel.compare_exchange(
            CANCEL_NONE,
            CANCEL_LOCKED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) | Err(CANCEL_LOCKED) => {
                self.set_stage(stage);
                Ok(())
            }
            Err(_) => Err(INSTALL_CANCELLED.to_string()),
        }
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
    /// A staging directory (or the replaced venv) that could not be removed
    /// after a successful install — typically because the old
    /// `tools/bin/ffmpeg.exe` was still running when it was moved into the
    /// backup. The install is complete; the leftover is swept by the next
    /// install or server start (`.staging-*`) once nothing holds it.
    /// Recorded in the audit line and shown by the CLI and the API.
    pub staging_left: Option<PathBuf>,
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

/// `not_installable` on Windows, `unsupported_platform` elsewhere (the
/// code a [`not_installable_reason`] refusal is audited with).
pub fn not_installable_code() -> &'static str {
    if cfg!(windows) {
        "not_installable"
    } else {
        "unsupported_platform"
    }
}

/// The CLI's checks before it shows the install prompt: the tool exists
/// and can be installed here. Each refusal is appended to the install
/// audit log (`result: "refused"`, chained) like the API's; `Err` carries
/// the message (plus the audit warning when the line could not be
/// written).
pub fn preflight<'a>(
    manifest: &'a Manifest,
    name: &str,
    dirs: &InstallDirs,
) -> Result<&'a ToolEntry, String> {
    let with_warning = |message: String, audit: Result<(), String>| match audit {
        Ok(()) => message,
        Err(warning) => format!("{message}\n경고: 설치 기록을 남기지 못했습니다 — {warning}"),
    };
    let Some(tool) = find_tool(manifest, name) else {
        let err = format!("unknown tool '{name}' (see frametrace tools list)");
        let audit = audit_refusal(dirs, name, "unknown_tool", &err);
        return Err(with_warning(err, audit));
    };
    if let Some(reason) = not_installable_reason(tool) {
        let err = match &tool.manual_hint {
            Some(hint) => format!("{name}: 자동 설치 불가 — {reason}\n수동 설치: {hint}"),
            None => format!("{name}: 자동 설치 불가 — {reason}"),
        };
        let audit = audit_refusal(dirs, &tool.name, not_installable_code(), &err);
        return Err(with_warning(err, audit));
    }
    Ok(tool)
}

/// Installs manifest tool `name` into `<exe dir>/tools` (T3: callers are
/// user actions only). Blocking: network and disk work happen on the
/// calling thread — never call this under the workstation's state lock.
pub fn install(name: &str, progress: &Progress) -> Result<InstallOutcome, String> {
    let manifest = manifest()?;
    let dirs = InstallDirs::for_current_exe()?;
    let Some(tool) = find_tool(manifest, name) else {
        let err = format!("unknown tool '{name}' (see frametrace tools list)");
        let _ = audit_refusal(&dirs, name, "unknown_tool", &err);
        return Err(err);
    };
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
        let err = format!("{} cannot be installed automatically: {reason}", tool.name);
        let _ = audit_refusal(dirs, &tool.name, not_installable_code(), &err);
        return Err(err);
    }
    if !cfg!(windows) {
        let _ = audit_refusal(
            dirs,
            &tool.name,
            "unsupported_platform",
            UNSUPPORTED_PLATFORM,
        );
        return Err(UNSUPPORTED_PLATFORM.to_string());
    }
    let result = run_platform_install(tool, dirs, net, progress);
    progress.set_stage(if result.is_ok() {
        Stage::Done
    } else {
        Stage::Failed
    });
    let audit = audit_line(tool, result.as_ref());
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
    // Anything older than this run is a stale staging dir of a killed one.
    let run_started = std::time::SystemTime::now();
    let _lock = InstallLock::acquire(&dirs.tools_dir)?;
    // Fix round 14 L5: the tool being installed first (a test fixture is
    // not in the manifest), then every manifest tool.
    let mut known: Vec<&ToolEntry> = vec![tool];
    if let Ok(manifest) = manifest() {
        known.extend(manifest.tools.iter());
    }
    clean_stale_staging(&dirs.tools_dir, run_started, &known);
    progress.check_cancel()?;
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
    /// Set by [`StagingDir::close`] (removal already attempted).
    closed: bool,
}

impl StagingDir {
    pub(crate) fn create(parent: &Path, prefix: &str) -> Result<Self, String> {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
        for _ in 0..8 {
            let path = parent.join(format!("{prefix}{}", random_hex()));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        closed: false,
                    });
                }
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
    /// Leaves the directory in place (it holds files that must not be
    /// lost, e.g. the previous binaries after an incomplete rollback).
    pub(crate) fn keep(mut self) {
        self.closed = true;
    }
    /// Removes the directory now (the success path) and reports it when it
    /// could not be removed — e.g. `backup/0.bak` is the previous
    /// `ffmpeg.exe`, still running: Windows lets a running image be renamed
    /// but not deleted. `None` when nothing is left behind.
    pub(crate) fn close(mut self) -> Option<PathBuf> {
        self.closed = true;
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => None,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => Some(self.path.clone()),
        }
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.closed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn audit_line(tool: &ToolEntry, result: Result<&InstallOutcome, &String>) -> String {
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
        Ok(outcome) => {
            value["result"] = "ok".into();
            value["files"] = outcome
                .installed_files
                .iter()
                .map(|f| serde_json::Value::from(f.display().to_string()))
                .collect();
            if let Some(left) = &outcome.staging_left {
                value["staging_left"] = left.display().to_string().into();
            }
        }
        Err(err) => {
            value["result"] = "failed".into();
            value["error"] = err.as_str().into();
            if err.starts_with(BUSY_PREFIX) {
                value["code"] = "busy".into();
            } else if err.starts_with(ROLLBACK_INCOMPLETE_PREFIX) {
                value["code"] = "rollback_incomplete".into();
            }
        }
    }
    value.to_string()
}

/// Appends one install-audit line, hash-chained like FrameTrace's other
/// audit logs (`previous_entry_sha256` = SHA-256 of the previous line,
/// `entry_sha256` over this line; keyed HMAC when an audit key is
/// configured), so a removed or edited line breaks the chain.
fn append_audit(path: &Path, line: &str) -> Result<(), String> {
    crate::audit::append_chained_jsonl(path, line)
        .map_err(|err| format!("install audit log {} not written: {err}", path.display()))
}

/// Records an install refused before anything started (unknown tool,
/// not installable, busy, another install running, ...). `Err` carries
/// the warning when the line could not be written.
pub fn audit_refusal(
    dirs: &InstallDirs,
    tool: &str,
    code: &str,
    message: &str,
) -> Result<(), String> {
    // The name may be anything a caller typed: bounded here, escaped by
    // serde.
    let tool: String = tool.chars().take(64).collect();
    let line = serde_json::json!({
        "ts_unix": crate::util::now_unix().unwrap_or(0),
        "event": "tool-install",
        "tool": tool,
        "result": "refused",
        "code": code,
        "error": message,
        "host_id": crate::util::host_id(),
    })
    .to_string();
    append_audit(&dirs.audit_log(), &line)
}

/// Removes `tools/.staging-*` directories — and the replaced pip venvs
/// `tools/python/.<name>.old-*` that could not be removed while a process
/// still ran from them — last modified before `started_before`: leftovers
/// of an install that was killed or whose old files were still in use. The
/// caller holds the [`InstallLock`] (every live staging dir and venv backup
/// belongs to a locked install), so none of them is in use by an install;
/// one that cannot be removed (a file in it still open) is skipped and
/// tried again next time. Returns how many were removed.
///
/// Fix round 14 L5: `tools` (the manifest's, first match wins) names each
/// pip tool's console script (`provides[0]`, which install checks) for
/// [`venv_intact`]; a venv of a tool not listed is checked for a script
/// named after the tool.
pub(crate) fn clean_stale_staging(
    tools_dir: &Path,
    started_before: std::time::SystemTime,
    tools: &[&ToolEntry],
) -> usize {
    // Fix round 13: a staging dir kept after an incomplete rollback (its
    // `backup/RESTORE.txt` lists the previous files) is never swept.
    let staging = |name: &str| {
        // Fix round 14 L4: any entry of that name (even one that is not a
        // readable file) marks a kept backup.
        name.starts_with(".staging-")
            && std::fs::symlink_metadata(tools_dir.join(name).join("backup").join(RESTORE_FILE))
                .is_err()
    };
    // pip.rs: `.<tool>.old-<hex>` next to the tool's venv. Fix round 13
    // M: removed only once the live `<tool>/` is a working venv again —
    // while it is not, the old one may be the only good copy left.
    let python = tools_dir.join("python");
    let old_venv = |name: &str| {
        name.starts_with('.')
            && name.contains(".old-")
            && (name.ends_with(".rm")
                || old_venv_base(name).is_some_and(|base| {
                    venv_intact(&python.join(base), venv_script_name(base, tools))
                }))
    };
    sweep_dir(tools_dir, &staging, started_before) + sweep_dir(&python, &old_venv, started_before)
}

/// `<tool>` of a replaced venv's name `.<tool>.old-<hex>`.
fn old_venv_base(name: &str) -> Option<&str> {
    let (base, _) = name.strip_prefix('.')?.rsplit_once(".old-")?;
    (!base.is_empty()).then_some(base)
}

/// The console script of the pip tool whose venv is `tools/python/<base>`:
/// its `provides[0]` (what [`pip::install_pip`] checks), else `base`.
fn venv_script_name<'a>(base: &'a str, tools: &[&'a ToolEntry]) -> &'a str {
    tools
        .iter()
        .find(|tool| tool.name == base && tool.kind == ToolKind::Pip)
        .and_then(|tool| tool.provides.first())
        .map_or(base, String::as_str)
}

/// Whether `venv` holds a working venv launcher: its interpreter and the
/// tool's console script `<name>` (`provides[0]`; see
/// [`venv_script_name`]).
pub(crate) fn venv_intact(venv: &Path, name: &str) -> bool {
    pip::venv_python(venv).is_file() && pip::venv_script(venv, name).is_file()
}

fn sweep_dir(
    dir: &Path,
    matches: &dyn Fn(&str) -> bool,
    started_before: std::time::SystemTime,
) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    // Listed first: the sweep renames entries.
    let paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| matches(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    let mut removed = 0;
    for path in paths {
        // Never follow a link out of tools/.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        let stale = meta
            .modified()
            .map(|modified| modified < started_before)
            .unwrap_or(false);
        if stale && remove_dir_unless_in_use(&path).is_none() {
            removed += 1;
        }
    }
    removed
}

/// Removes the directory `dir` only when nothing uses a file in it, never
/// half of it (a venv the server runs deepfake-lens from, under a busy
/// lease this process cannot see). On Windows a running program or DLL in
/// it is found first (opening a mapped image for writing is a sharing
/// violation; nothing is written); then the directory is renamed to
/// `<dir>.rm`, which Windows refuses while any file under it is open. In
/// either case it is left whole and tried again next time. Only the renamed
/// copy is deleted. Returns the path left behind, if any (`<dir>` when in
/// use, `<dir>.rm` when its removal failed — both match the sweep's
/// patterns and are retried by it). A `<dir>` already named `….rm` is such
/// a leftover: it is not renamed again (`.rm.rm.rm…` would grow every
/// sweep), only its removal is retried.
pub(crate) fn remove_dir_unless_in_use(dir: &Path) -> Option<PathBuf> {
    let left = |path: &Path| {
        std::fs::symlink_metadata(path)
            .is_ok()
            .then(|| path.to_path_buf())
    };
    if image_in_use_under(dir) {
        return left(dir);
    }
    let doomed = if dir.to_string_lossy().ends_with(".rm") {
        dir.to_path_buf()
    } else {
        let mut doomed = dir.as_os_str().to_owned();
        doomed.push(".rm");
        let doomed = PathBuf::from(doomed);
        if std::fs::rename(dir, &doomed).is_err() {
            return left(dir);
        }
        doomed
    };
    match remove_doomed_dir(&doomed) {
        Ok(()) => None,
        Err(_) => left(&doomed),
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: makes [`remove_doomed_dir`] fail on this thread (a removal
    /// that keeps failing — ACL, a scanner holding a file).
    pub(crate) static FAIL_DOOMED_REMOVAL: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

fn remove_doomed_dir(doomed: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_DOOMED_REMOVAL.with(|fail| fail.get()) {
        return Err(std::io::Error::other("test: removal fails"));
    }
    std::fs::remove_dir_all(doomed)
}

/// Whether a file under `dir` is a running program / loaded DLL (or held
/// without write sharing): opening it for writing — shared, nothing is
/// written — is then a sharing or lock violation. Links are not followed.
#[cfg(windows)]
fn image_in_use_under(dir: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE.
    const SHARE_ALL: u32 = 0x7;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let Ok(kind) = entry.file_type() else {
            return false;
        };
        if kind.is_symlink() {
            false
        } else if kind.is_dir() {
            image_in_use_under(&entry.path())
        } else {
            match std::fs::OpenOptions::new()
                .write(true)
                .share_mode(SHARE_ALL)
                .open(entry.path())
            {
                Ok(_) => false,
                Err(err) => matches!(
                    err.raw_os_error(),
                    Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
                ),
            }
        }
    })
}

/// Elsewhere a file in use can be unlinked without breaking its user.
#[cfg(not(windows))]
fn image_in_use_under(_dir: &Path) -> bool {
    false
}

/// Server start: sweeps stale staging dirs under `<exe dir>/tools` when
/// no install holds the lock (another process installing → skipped).
/// Never creates the tools dir.
pub fn clean_stale_staging_at_start() -> usize {
    let Ok(dirs) = InstallDirs::for_current_exe() else {
        return 0;
    };
    let known: Vec<&ToolEntry> = manifest()
        .map(|manifest| manifest.tools.iter().collect())
        .unwrap_or_default();
    clean_stale_staging_if_unlocked(&dirs.tools_dir, &known)
}

pub(crate) fn clean_stale_staging_if_unlocked(tools_dir: &Path, tools: &[&ToolEntry]) -> usize {
    if !tools_dir.is_dir() {
        return 0;
    }
    let started = std::time::SystemTime::now();
    match InstallLock::acquire(tools_dir) {
        Ok(_lock) => clean_stale_staging(tools_dir, started, tools),
        Err(_) => 0,
    }
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

/// Tries of [`rename_with_retry`] and the pause after each failed one
/// (5 tries, about 2 s in all).
const RENAME_TRIES: u32 = 5;
const RENAME_PAUSE: std::time::Duration = std::time::Duration::from_millis(400);

/// Whether a rename error is one an antivirus scan of the just-extracted
/// file causes for a moment: `ERROR_SHARING_VIOLATION` (32) or
/// `ERROR_ACCESS_DENIED` (5). Windows only; never elsewhere.
fn transient_rename_error(err: &std::io::Error) -> bool {
    cfg!(windows) && matches!(err.raw_os_error(), Some(5) | Some(32))
}

/// `std::fs::rename` with a short bounded retry (Fix round 11 H4): a file
/// held open by an AV scanner right after extraction must not fail the
/// whole install and force a 170 MB re-download. Only the rename is
/// retried; any other error, or the last try's, is returned as is.
pub(crate) fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 1;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if tries < RENAME_TRIES && transient_rename_error(&err) => {
                tries += 1;
                std::thread::sleep(RENAME_PAUSE);
            }
            Err(err) => return Err(err),
        }
    }
}

/// After an incomplete rollback (error starting with
/// [`ROLLBACK_INCOMPLETE_PREFIX`]), moves `backup_dir` — the previous
/// files and their `RESTORE.txt` — out of `staging` to
/// `tools/rollback-backup-<hex>`, which the stale-staging sweep never
/// touches, and returns `err` with the paths updated. When that move
/// fails the staging directory is kept instead (the sweep skips a staging
/// dir holding `backup/RESTORE.txt`; the error says so). Any other error
/// is returned as is and `staging` is dropped (removed) as usual.
/// Fix round 13 L1: `RESTORE.txt` names `tool` on its first line, so
/// `/api/tools`, `frametrace tools list` and `doctor` can attribute it.
pub(crate) fn preserve_rollback_backup(
    err: String,
    staging: StagingDir,
    backup_dir: &Path,
    tools_dir: &Path,
    tool: &str,
) -> String {
    if !err.starts_with(ROLLBACK_INCOMPLETE_PREFIX) {
        return err;
    }
    // Fix round 14 L4: creates RESTORE.txt when the best-effort write in
    // `move_all_or_nothing` left none; the error points at it only if it
    // is there.
    let unwritten = rollback::name_tool_in_restore_file(&backup_dir.join(RESTORE_FILE), tool)
        .err()
        .map(|err| rollback::restore_unwritten_note(&err));
    let lists = |restore: &Path| match &unwritten {
        None => format!("{} lists each one", restore.display()),
        Some(note) => format!("this message lists each one {note}"),
    };
    let kept = tools_dir.join(format!(
        "{}{}",
        rollback::ROLLBACK_BACKUP_PREFIX,
        random_hex()
    ));
    match rename_with_retry(backup_dir, &kept) {
        Ok(()) => {
            let err = err.replace(
                &backup_dir.display().to_string(),
                &kept.display().to_string(),
            );
            drop(staging);
            format!(
                "{err} (restore the previous files from {} by hand — {})",
                kept.display(),
                lists(&kept.join(RESTORE_FILE))
            )
        }
        Err(move_err) => {
            let path = staging.path().to_path_buf();
            staging.keep();
            // Fix round 15: without `backup/RESTORE.txt` the stale-staging
            // sweep cannot tell this folder from an ordinary leftover, so
            // the next server start or install removes it — say so instead
            // of "before installing again" alone.
            let when = if unwritten.is_some() {
                "before FrameTrace is started again or another install runs (either one cleans this folder away)"
            } else {
                "before installing again"
            };
            format!(
                "{err} (the previous files stay in {} — could not move them to {}: {move_err}; restore them by hand {when} — {})",
                path.display(),
                kept.display(),
                lists(&backup_dir.join(RESTORE_FILE))
            )
        }
    }
}

/// `std::fs::remove_file` with the same bounded retry as
/// [`rename_with_retry`] (an AV scan of the file just moved into place).
pub(crate) fn remove_file_with_retry(path: &Path) -> std::io::Result<()> {
    let mut tries = 1;
    loop {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) if tries < RENAME_TRIES && transient_rename_error(&err) => {
                tries += 1;
                std::thread::sleep(RENAME_PAUSE);
            }
            Err(err) => return Err(err),
        }
    }
}

/// `std::fs::remove_dir_all` with the same bounded retry (Fix round 13
/// M: one file of a partial venv held by an AV scan or the indexer).
/// A missing directory is success.
pub(crate) fn remove_dir_all_with_retry(path: &Path) -> std::io::Result<()> {
    let mut tries = 1;
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) if tries < RENAME_TRIES && transient_rename_error(&err) => {
                tries += 1;
                std::thread::sleep(RENAME_PAUSE);
            }
            Err(err) => return Err(err),
        }
    }
}

/// Undoes one completed move: removes `dest` (the new file) and renames
/// its `backup` (the previous file) back, both with the bounded retry.
/// `Err` describes what could not be undone.
fn undo_move(dest: &Path, backup: Option<&Path>) -> Result<(), String> {
    if let Err(err) = remove_file_with_retry(dest) {
        // The previous file cannot replace a new one that is held open
        // either; keep the backup where it is and say so.
        return Err(match backup {
            Some(backup) => format!(
                "{} is the NEW file (cannot remove it: {err}); the previous one is kept at {}",
                dest.display(),
                backup.display()
            ),
            None => format!(
                "{} is the NEW file (cannot remove it: {err})",
                dest.display()
            ),
        });
    }
    if let Some(backup) = backup
        && let Err(err) = rename_with_retry(backup, dest)
    {
        return Err(format!(
            "{} is missing (cannot restore it: {err}); the previous one is kept at {}",
            dest.display(),
            backup.display()
        ));
    }
    Ok(())
}

/// Moves `(src, dest)` pairs into place, replacing existing destinations.
/// Each existing destination is first renamed into `backup_dir`; when any
/// move fails, every destination already moved is removed and its backup
/// restored, so the target directory is either fully updated or unchanged.
///
/// Fix round 12 H-a: the rollback's removes and renames use the same
/// bounded retry as the moves. When it still cannot undo everything the
/// error starts with [`ROLLBACK_INCOMPLETE_PREFIX`] and names every file
/// left wrong and where its previous copy is (inside `backup_dir` — the
/// caller must keep that directory); it is never silent.
pub(crate) fn move_all_or_nothing(
    moves: &[(PathBuf, PathBuf)],
    backup_dir: &Path,
) -> Result<(), String> {
    std::fs::create_dir_all(backup_dir)
        .map_err(|err| format!("cannot create {}: {err}", backup_dir.display()))?;
    // (dest, Some(backup)) for each completed move.
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let mut failure = None;
    // What the rollback could not undo (Fix round 12 H-a), and per such
    // file its original path and kept previous copy (Fix round 13 L1:
    // written to `backup_dir/RESTORE.txt`).
    let mut undo_failed: Vec<String> = Vec::new();
    let mut to_restore: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
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
                if let Err(err) = rename_with_retry(dest, &backup) {
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
        if let Err(err) = rename_with_retry(src, dest) {
            failure = Some(format!("cannot move {} into place: {err}", dest.display()));
            if let Some(backup) = backup {
                // Nothing new is at `dest`: only the previous file to restore.
                if let Err(restore) = rename_with_retry(&backup, dest) {
                    undo_failed.push(format!(
                        "{} is missing (cannot restore it: {restore}); the previous one is kept at {}",
                        dest.display(),
                        backup.display()
                    ));
                    to_restore.push((dest.clone(), Some(backup)));
                }
            }
            break;
        }
        done.push((dest.clone(), backup));
    }
    if let Some(failure) = failure {
        for (dest, backup) in done.into_iter().rev() {
            if let Err(problem) = undo_move(&dest, backup.as_deref()) {
                undo_failed.push(problem);
                to_restore.push((dest, backup));
            }
        }
        if !undo_failed.is_empty() {
            // Best effort: the error itself names every file too.
            let _ = std::fs::write(
                backup_dir.join(RESTORE_FILE),
                rollback::restore_text(None, &to_restore, backup_dir),
            );
            return Err(format!(
                "{ROLLBACK_INCOMPLETE_PREFIX}{failure}; the rollback could NOT restore every previous file — {}",
                undo_failed.join("; ")
            ));
        }
        return Err(failure);
    }
    Ok(())
}
