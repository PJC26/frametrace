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

/// R19-S1 (P32): `path` made absolute (against this process's current
/// directory, lexically — nothing on disk is read) with no trailing
/// separator; a drive root becomes `X:\.` (`X:` alone is drive-relative),
/// a UNC share root `\\server\share`. A verbatim (`\\?\`) path keeps its
/// root as it is.
pub fn normalize(path: &Path) -> PathBuf {
    if path.as_os_str().is_empty() {
        return PathBuf::new();
    }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    // Collecting the components drops trailing (and repeated) separators.
    let collected: PathBuf = absolute.components().collect();
    let mut components = collected.components();
    let root_only = matches!(
        (components.next(), components.next(), components.next()),
        (Some(Component::Prefix(_)), Some(Component::RootDir), None)
    );
    if root_only && let Some(Component::Prefix(prefix)) = collected.components().next() {
        return match prefix.kind() {
            std::path::Prefix::Disk(_) => collected.join("."),
            std::path::Prefix::UNC(..) => PathBuf::from(prefix.as_os_str()),
            _ => collected,
        };
    }
    collected
}

/// The [`normalize`]d path as it goes, unescaped, between a command's
/// quotes — `None` when it is empty, starts with `-` (it would read as an
/// option) or holds any character that is not [`paste_safe_char`].
pub fn paste_safe(path: &Path) -> Option<String> {
    let text = normalize(path).display().to_string();
    (!text.is_empty() && !text.starts_with('-') && text.chars().all(paste_safe_char))
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
