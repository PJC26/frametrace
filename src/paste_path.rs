//! Paths inside commands the examiner pastes (R18 P29, R19 P32).
//!
//! A path goes into a command only when every character is on a
//! WHITELIST ([`paste_safe_char`]) — nothing is ever escaped — and only
//! after [`normalize`]: absolute, no trailing separator (Win32 argv
//! conversion reads `\"` as a literal quote, so `"E:\" --dismiss` reached
//! the program as ONE argument `E:" --dismiss`), a drive root as `X:\.`.
//! Otherwise the command shows a placeholder and [`UNSAFE_REASON`].

use std::path::{Component, Path, PathBuf};

/// The words shown when a path is not paste-safe — no command with that
/// path is given.
pub const UNSAFE_REASON: &str = "경로에 특수 문자 — 명령을 직접 입력하십시오";

/// R18-S1 (P29): the characters a path may hold for a command to be
/// offered for pasting — a WHITELIST. Letters and digits of any script
/// (Hangul folder names), the ASCII space and `- _ . , / \ : ( ) [ ] + = @
/// ~ #`. Everything else is refused, notably: PowerShell's single quotes
/// `'` U+2018 U+2019 U+201A U+201B, its double quotes, `$` and the
/// backtick; `cmd.exe`'s `%`, `!`, `^ & | < > "`; `;`; every control
/// character and every non-ASCII space, dash or quote.
pub fn paste_safe_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            ' ' | '-'
                | '_'
                | '.'
                | ','
                | '/'
                | '\\'
                | ':'
                | '('
                | ')'
                | '['
                | ']'
                | '+'
                | '='
                | '@'
                | '~'
                | '#'
        )
}

/// Refusal text for a path [`normalize_checked`] will not bind (R20-S1).
pub const UNBINDABLE_REASON: &str = "경로 구성 요소에 '/' 또는 ':'가 들어 있어 이 경로는 쓸 수 없습니다 — 일반 로컬 경로(예: C:\\cases\\c1)를 입력하십시오. / A path component holding '/' or ':' is refused.";

/// R19-S1 (P32): `path` made absolute (against this process's current
/// directory, lexically — nothing on disk is read) with no trailing
/// separator; a drive root becomes `X:\.` (`X:` alone is drive-relative),
/// a UNC share root `\\server\share`.
///
/// R20-S1 (P34): the result is joined as TEXT — never by
/// `PathBuf::push`, which REPLACES the buffer for a component that parses
/// as rooted or prefixed (`\\?\C:\x\//srv/sh` became the UNC path
/// `//srv/sh`, `C:\x\D:` the drive-relative `D:`). A verbatim (`\\?\…`) or
/// device-namespace (`\\.\…`) path is not reassembled at all: only its
/// trailing separators are dropped (its root separator kept). R21-S3: a
/// bindable verbatim LOCAL DRIVE path (`\\?\C:\…`) whose every name means
/// the same in Win32 form is returned in that plain form (`C:\…`, the
/// root `C:\.`). Whether the result may be BOUND (opened, made a root) is
/// [`normalize_checked`].
pub fn normalize(path: &Path) -> PathBuf {
    normalize_parts(path).0
}

/// R20-S1 (P34): [`normalize`], or `None` when the path must not be bound:
/// it is empty, or (Windows) a component after the prefix holds `/` or `:`
/// — in a verbatim path `/` is no separator, so such a component would be
/// re-read as a UNC / device / drive path by anything that reassembles or
/// strips the prefix; elsewhere `:` is a drive switch or an NTFS stream.
/// R21-S2: also when a component after a verbatim / device prefix is `.`,
/// `..` or empty (kept as a name there, resolved once the prefix is gone).
/// A caller that binds the result must still run the remote / device /
/// reserved-name checks on it (the text changed).
pub fn normalize_checked(path: &Path) -> Option<PathBuf> {
    let (normalized, bindable) = normalize_parts(path);
    bindable.then_some(normalized)
}

/// Whether a Windows path component (after the prefix) holds `/` or `:`.
fn bad_component(component: &[u8]) -> bool {
    component.iter().any(|byte| matches!(byte, b'/' | b':'))
}

/// R21-S3: the longest plain form a verbatim local drive path is bound
/// in. A longer one stays verbatim: a child program handed the plain text
/// would hit MAX_PATH (260, less room for a file name under the folder).
const PLAIN_MAX_LEN: usize = 240;

/// R21-S3: whether a name under `\\?\C:\` means the same without the
/// prefix — not empty, no trailing `.` or space (Win32 strips them) and no
/// reserved device base name (`aux.mp4` is a file only under the prefix).
/// Empty `names` (the drive root) splits to one `""`, which is stable.
fn plain_stable(name: &str) -> bool {
    name.is_empty()
        || (!name.ends_with(['.', ' ']) && !crate::util::has_reserved_device_component(name))
}

/// Whether `path` starts with a verbatim (`\\?\`) or device-namespace
/// (`\\.\`) prefix.
fn special_prefix(path: &Path) -> bool {
    use std::path::Prefix;
    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(
            prefix.kind(),
            Prefix::Verbatim(_) | Prefix::VerbatimUNC(..) | Prefix::VerbatimDisk(_) | Prefix::DeviceNS(_)
        )
    )
}

/// `(normalized, bindable)` — see [`normalize`] / [`normalize_checked`].
fn normalize_parts(path: &Path) -> (PathBuf, bool) {
    use std::ffi::OsString;
    use std::path::Prefix;
    if path.as_os_str().is_empty() {
        return (PathBuf::new(), false);
    }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if !cfg!(windows) {
        // No prefixes, `/` is the only separator: collecting the
        // components only drops trailing (and repeated) separators.
        return (absolute.components().collect(), true);
    }
    if special_prefix(&absolute) {
        let Some(Component::Prefix(prefix)) = absolute.components().next() else {
            unreachable!("special_prefix saw a prefix");
        };
        // Sliced as text: a path that is not valid Unicode is kept as it
        // is and never bound.
        let Some(whole) = absolute.to_str() else {
            return (absolute, false);
        };
        let head = prefix.as_os_str().len();
        let rest = &whole[head..];
        let kept = rest.trim_end_matches('\\');
        let kept = if kept.is_empty() && !rest.is_empty() {
            "\\"
        } else {
            kept
        };
        // R21-S2: under the prefix `.`, `..` and an empty component are
        // NAMES (Win32 never resolves them), so whatever strips the prefix
        // later would resolve them to another folder — not bindable.
        let names = kept.strip_prefix('\\').unwrap_or(kept);
        let bindable = names.is_empty()
            || !names
                .split('\\')
                .any(|part| matches!(part, "" | "." | "..") || bad_component(part.as_bytes()));
        // R21-S3: a verbatim local drive path is bound in its PLAIN form,
        // so `\\?\C:\cases\c1` and `C:\cases\c1` are one binding (and
        // compare equal on reopen) — unless plain Win32 parsing would
        // change what a name means (see `plain_stable`).
        if bindable
            && let Prefix::VerbatimDisk(_) = prefix.kind()
            && kept.starts_with('\\')
            && names.split('\\').all(plain_stable)
        {
            let drive = &whole[4..head];
            let plain = if names.is_empty() {
                format!("{drive}\\.")
            } else {
                format!("{drive}\\{names}")
            };
            if plain.len() <= PLAIN_MAX_LEN {
                return (PathBuf::from(plain), true);
            }
        }
        return (PathBuf::from(format!("{}{kept}", &whole[..head])), bindable);
    }
    let mut out = OsString::new();
    let mut kind = None;
    let mut rooted = false;
    let mut names: Vec<&std::ffi::OsStr> = Vec::new();
    let mut bindable = true;
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) if kind.is_none() && !rooted && names.is_empty() => {
                kind = Some(prefix.kind());
                out.push(prefix.as_os_str());
            }
            Component::Prefix(_) => bindable = false,
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir | Component::Normal(_) => {
                let name = component.as_os_str();
                bindable &= !bad_component(name.as_encoded_bytes());
                names.push(name);
            }
        }
    }
    match (kind, names.is_empty()) {
        (Some(Prefix::Disk(_)), true) if rooted => out.push("\\."),
        (Some(Prefix::UNC(..)), true) => {}
        _ => {
            if rooted {
                out.push("\\");
            }
            for (index, name) in names.iter().enumerate() {
                if index > 0 {
                    out.push("\\");
                }
                out.push(name);
            }
        }
    }
    (PathBuf::from(out), bindable)
}

/// The [`normalize`]d path as it goes, unescaped, between a command's
/// quotes — `None` when it is empty, starts with `-` (it would read as an
/// option), holds any character that is not [`paste_safe_char`], would not
/// be bound ([`normalize_checked`]), has a verbatim / device prefix
/// (`\\.\C:\` kept its root `\`, which Win32 argv conversion reads as an
/// escaped closing quote — R20-S2) or ends in `\` at all.
pub fn paste_safe(path: &Path) -> Option<String> {
    let normalized = normalize_checked(path)?;
    // R21-S3: a typed verbatim path now normalizes to its plain form; the
    // typed text itself still gets no command (as in R20).
    if special_prefix(path) || special_prefix(&normalized) {
        return None;
    }
    let text = normalized.display().to_string();
    (!text.is_empty()
        && !text.starts_with('-')
        && !text.ends_with('\\')
        && text.chars().all(paste_safe_char))
    .then_some(text)
}

/// One word of a hint command ([`hint_command`]).
pub enum Arg<'a> {
    /// Put in as it is.
    Word(&'a str),
    /// A path: quoted when [`paste_safe`], else `placeholder`.
    Path(&'a Path, &'a str),
}

/// R19-S2: a command for the examiner to type next — each path quoted
/// (double quotes on Windows: both PowerShell and `cmd.exe` read them, and
/// the whitelist holds no `$`, backtick, `%` or `!`; single quotes
/// elsewhere) when it is paste-safe; otherwise its placeholder and, once,
/// `(` [`UNSAFE_REASON`] `)`.
pub fn hint_command(args: &[Arg<'_>]) -> String {
    let mut unsafe_path = false;
    let words: Vec<String> = args
        .iter()
        .map(|arg| match arg {
            Arg::Word(word) => (*word).to_string(),
            Arg::Path(path, placeholder) => match paste_safe(path) {
                Some(text) if cfg!(windows) => format!("\"{text}\""),
                Some(text) => format!("'{text}'"),
                None => {
                    unsafe_path = true;
                    (*placeholder).to_string()
                }
            },
        })
        .collect();
    let mut text = words.join(" ");
    if unsafe_path {
        text.push_str(&format!(" ({UNSAFE_REASON})"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn normalize_strips_trailing_separators_and_keeps_roots_rooted() {
        for (typed, want) in [
            ("E:\\", "E:\\."),
            ("e:/", "e:\\."),
            ("C:\\cases\\c 1\\", "C:\\cases\\c 1"),
            ("C:\\cases\\c 1\\\\", "C:\\cases\\c 1"),
            ("C:/cases//c 1/", "C:\\cases\\c 1"),
            ("C:\\cases\\c1", "C:\\cases\\c1"),
            ("\\\\server\\share\\", "\\\\server\\share"),
            ("\\\\server\\share\\c1\\", "\\\\server\\share\\c1"),
        ] {
            assert_eq!(normalize(Path::new(typed)), PathBuf::from(want), "{typed}");
            assert!(
                !normalize(Path::new(typed))
                    .display()
                    .to_string()
                    .ends_with('\\'),
                "{typed}"
            );
        }
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(normalize(Path::new("cases\\c1\\")), cwd.join("cases\\c1"));
    }

    #[cfg(not(windows))]
    #[test]
    fn normalize_strips_trailing_separators_off_windows() {
        assert_eq!(
            normalize(Path::new("/cases/c 1/")),
            PathBuf::from("/cases/c 1")
        );
        assert_eq!(normalize(Path::new("/")), PathBuf::from("/"));
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(normalize(Path::new("cases/c1/")), cwd.join("cases/c1"));
    }

    #[test]
    fn hint_command_quotes_safe_paths_and_names_unsafe_ones() {
        let safe = std::env::temp_dir().join("홍길동 사건 (1)");
        let raw = safe.join("disk.raw");
        let text = hint_command(&[
            Arg::Word("next: carve-file"),
            Arg::Path(&safe, "<케이스 폴더>"),
            Arg::Path(&raw, "<파일>"),
        ]);
        let quote = if cfg!(windows) { '"' } else { '\'' };
        assert_eq!(
            text,
            format!(
                "next: carve-file {quote}{}{quote} {quote}{}{quote}",
                normalize(&safe).display(),
                normalize(&raw).display()
            )
        );
        let hostile = std::env::temp_dir().join("x'; calc; '");
        let text = hint_command(&[
            Arg::Word("next: carve-file"),
            Arg::Path(&hostile, "<케이스 폴더>"),
            Arg::Path(&raw, "<파일>"),
        ]);
        assert!(
            text.starts_with("next: carve-file <케이스 폴더> "),
            "{text}"
        );
        assert!(text.ends_with(&format!(" ({UNSAFE_REASON})")), "{text}");
        assert!(!text.contains("calc"), "{text}");
        assert_eq!(text.matches(UNSAFE_REASON).count(), 1, "{text}");
    }
}
