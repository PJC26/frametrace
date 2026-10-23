//! Path-picker helpers: `POST /api/mkdir` (create one new folder, e.g. a
//! case folder, without leaving the picker) and `GET /api/places` (quick
//! jump targets: home, Desktop, Documents, Downloads and drives — under
//! WSL also the Windows drives and Windows user folders below `/mnt`).

use super::*;
use super::system::drive_roots;

const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A single new path segment, valid on Windows as well as Linux so a case
/// folder made under WSL stays openable from Windows.
pub(crate) fn validate_folder_name(raw: &str) -> Result<&str, &'static str> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("폴더 이름을 입력하십시오");
    }
    if name.chars().count() > 120 {
        return Err("폴더 이름이 너무 깁니다 (120자 이하)");
    }
    if name == "." || name == ".."
        || name.chars().any(|c| c.is_control() || r#"\/:*?"<>|"#.contains(c))
    {
        return Err(r#"폴더 이름에 쓸 수 없는 문자가 있습니다 (\ / : * ? " < > |)"#);
    }
    if name.ends_with('.') {
        return Err("폴더 이름은 마침표로 끝날 수 없습니다");
    }
    let stem = name.split('.').next().unwrap_or_default().to_ascii_lowercase();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        return Err("Windows 예약어(CON, NUL, COM1 등)는 폴더 이름으로 쓸 수 없습니다");
    }
    Ok(name)
}

/// Creates exactly one folder `parent/name`; never creates missing parents
/// and never reuses an existing entry.
pub(crate) fn make_folder(parent: &str, name: &str) -> Result<PathBuf, String> {
    let parent = parent.trim();
    super::http::reject_remote_path(parent, &[]).map_err(str::to_string)?;
    let name = validate_folder_name(name).map_err(str::to_string)?;
    let base = PathBuf::from(parent);
    if parent.is_empty() || !base.is_dir() {
        return Err("상위 폴더가 없습니다 — 먼저 폴더를 고르십시오".into());
    }
    let target = base.join(name);
    match std::fs::create_dir(&target) {
        Ok(()) => Ok(target),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err("같은 이름의 폴더나 파일이 이미 있습니다".into())
        }
        Err(e) => Err(format!("폴더를 만들 수 없습니다: {e}")),
    }
}

pub(crate) fn api_mkdir(request: &Request) -> String {
    let parent = body_value(&request.body, "parent").unwrap_or_default();
    let name = body_value(&request.body, "name").unwrap_or_default();
    match make_folder(&parent, &name) {
        Ok(path) => serde_json::json!({"ok": true, "path": path.display().to_string()}).to_string(),
        Err(err) => api_err(&err),
    }
}

fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WSL_DISTRO_NAME").is_some()
            || Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists())
}

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_dir())
}

fn place(label: &str, path: &Path, kind: &str) -> serde_json::Value {
    serde_json::json!({"label": label, "path": path.display().to_string(), "kind": kind})
}

/// Home plus its Desktop / Documents / Downloads, each only when present.
fn user_places(home: &Path, prefix: &str, out: &mut Vec<serde_json::Value>) {
    out.push(place(&format!("{prefix}홈"), home, "home"));
    for (sub, label) in [("Desktop", "바탕 화면"), ("Documents", "문서"), ("Downloads", "다운로드")] {
        let dir = home.join(sub);
        if dir.is_dir() {
            out.push(place(&format!("{prefix}{label}"), &dir, "folder"));
        }
    }
}

/// Windows profiles under `<users_root>` (normally `/mnt/c/Users`) that
/// look like real accounts (they have a Desktop).
pub(crate) fn windows_profiles(users_root: &Path) -> Vec<PathBuf> {
    let skip = ["public", "default", "default user", "all users"];
    let mut found: Vec<PathBuf> = std::fs::read_dir(users_root)
        .map(|read| {
            read.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                    !skip.contains(&name.as_str()) && p.join("Desktop").is_dir()
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

pub(crate) fn places_json(home: Option<PathBuf>, wsl: bool, mnt: &Path) -> String {
    let mut places = Vec::new();
    if wsl {
        for profile in windows_profiles(&mnt.join("c").join("Users")) {
            user_places(&profile, "Windows ", &mut places);
        }
        for letter in b'a'..=b'z' {
            let root = mnt.join((letter as char).to_string());
            if std::fs::read_dir(&root).map(|mut r| r.next().is_some()).unwrap_or(false) {
                let label = format!("Windows {}: 드라이브", (letter as char).to_ascii_uppercase());
                places.push(place(&label, &root, "drive"));
            }
        }
    }
    if let Some(home) = home {
        user_places(&home, if wsl { "Linux " } else { "" }, &mut places);
    }
    for root in drive_roots() {
        let label = if cfg!(windows) { format!("{} 드라이브", root.trim_end_matches('\\')) } else { "루트 (/)".into() };
        places.push(place(&label, Path::new(&root), "drive"));
    }
    serde_json::json!({"ok": true, "wsl": wsl, "places": places}).to_string()
}

pub(crate) fn api_places() -> String {
    places_json(home_dir(), is_wsl(), Path::new("/mnt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ft-fstools-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folder_names_reject_separators_traversal_and_reserved_words() {
        assert_eq!(validate_folder_name("  case-001 "), Ok("case-001"));
        assert_eq!(validate_folder_name("사건 2026"), Ok("사건 2026"));
        for bad in ["", "   ", ".", "..", "a/b", r"a\b", "c:", "x?", "end.", "CON", "nul.txt", "Com3", "a\u{7}"] {
            assert!(validate_folder_name(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(validate_folder_name(&"가".repeat(121)).is_err());
    }

    #[test]
    fn make_folder_creates_one_level_and_refuses_duplicates() {
        let dir = scratch("mk");
        let made = make_folder(&dir.display().to_string(), "새 케이스").unwrap();
        assert!(made.is_dir());
        assert_eq!(made, dir.join("새 케이스"));
        let dup = make_folder(&dir.display().to_string(), "새 케이스").unwrap_err();
        assert!(dup.contains("이미"), "{dup}");
        let missing = make_folder(&dir.join("없음").display().to_string(), "x").unwrap_err();
        assert!(missing.contains("상위 폴더"), "{missing}");
        assert!(!dir.join("없음").exists(), "missing parents are never created");
        assert!(make_folder(&dir.display().to_string(), "../escape").is_err());
        assert!(make_folder(r"\\evil\share", "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn places_list_home_folders_and_wsl_windows_profiles() {
        let root = scratch("places");
        let home = root.join("home");
        std::fs::create_dir_all(home.join("Downloads")).unwrap();
        let mnt = root.join("mnt");
        std::fs::create_dir_all(mnt.join("c/Users/kim/Desktop")).unwrap();
        std::fs::create_dir_all(mnt.join("c/Users/Public/Desktop")).unwrap();
        std::fs::create_dir_all(mnt.join("d")).unwrap(); // empty: not mounted

        let body: serde_json::Value = serde_json::from_str(&places_json(Some(home.clone()), true, &mnt)).unwrap();
        let labels: Vec<String> = body["places"].as_array().unwrap().iter()
            .map(|p| p["label"].as_str().unwrap().to_string()).collect();
        assert_eq!(body["wsl"], true);
        assert!(labels.contains(&"Windows 바탕 화면".to_string()), "{labels:?}");
        assert!(labels.contains(&"Windows C: 드라이브".to_string()), "{labels:?}");
        assert!(!labels.iter().any(|l| l.starts_with("Windows D:")), "empty mount point skipped: {labels:?}");
        assert!(labels.contains(&"Linux 다운로드".to_string()), "{labels:?}");
        assert!(!labels.contains(&"Linux 문서".to_string()), "absent folders skipped");
        assert_eq!(windows_profiles(&mnt.join("c/Users")), vec![mnt.join("c/Users/kim")]);

        let plain: serde_json::Value = serde_json::from_str(&places_json(Some(home), false, &mnt)).unwrap();
        assert_eq!(plain["places"][0]["label"], "홈");
        let _ = std::fs::remove_dir_all(&root);
    }
}
