//! Pinned catalog of the optional forensic tool packages the workstation can
//! install on demand. Every download is fixed to one version and one SHA-256
//! (taken from the digest GitHub publishes for the release asset), so an
//! install is reproducible and a tampered or swapped asset is refused.

/// One installable package. Each lands in its own `tools/<dir_name>/`
/// directory: Sleuth Kit ships 32-bit runtime DLLs (ucrtbase, vcruntime140,
/// zlib) that must never sit next to the 64-bit ffmpeg / libewf binaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPackage {
    Ffmpeg,
    SleuthKit,
    Libewf,
}

/// How a package is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    /// Download a zip, verify it, and copy every file under `bin_prefix`.
    Archive(PinnedArchive),
    /// libewf publishes source only; run the bundled MSYS2 build script.
    BuildScript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedArchive {
    pub url: &'static str,
    pub sha256: &'static str,
    /// Archive-relative directory whose files are installed (`/`-separated).
    pub bin_prefix: &'static str,
}

pub const ALL_PACKAGES: [ToolPackage; 3] = [
    ToolPackage::Ffmpeg,
    ToolPackage::SleuthKit,
    ToolPackage::Libewf,
];

impl ToolPackage {
    /// Parses the identifier the UI sends (`ffmpeg`, `sleuthkit`, `libewf`).
    pub fn parse(id: &str) -> Option<Self> {
        ALL_PACKAGES.into_iter().find(|package| package.id() == id.trim())
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::SleuthKit => "sleuthkit",
            Self::Libewf => "libewf",
        }
    }

    /// Subdirectory of `<exe dir>/tools/` the package is installed into.
    pub fn dir_name(self) -> &'static str {
        self.id()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ffmpeg => "FFmpeg",
            Self::SleuthKit => "Sleuth Kit",
            Self::Libewf => "libewf",
        }
    }

    pub fn version(self) -> &'static str {
        match self {
            Self::Ffmpeg => "9.0.2",
            Self::SleuthKit => "4.15.0",
            Self::Libewf => "20240506",
        }
    }

    /// Binaries the workstation resolves from this package.
    pub fn provides(self) -> &'static [&'static str] {
        match self {
            Self::Ffmpeg => &["ffmpeg", "ffprobe"],
            Self::SleuthKit => &["mmls", "fls", "icat"],
            Self::Libewf => &["ewfinfo", "ewfverify", "ewfexport"],
        }
    }

    /// Rough user-facing cost, shown before the examiner confirms.
    pub fn estimate(self) -> &'static str {
        match self {
            Self::Ffmpeg => "약 115MB 다운로드 · 1~3분",
            Self::SleuthKit => "약 18MB 다운로드 · 1분 이내",
            Self::Libewf => "약 350MB 다운로드 + 소스 빌드 · 10~20분",
        }
    }

    pub fn method(self) -> InstallMethod {
        match self {
            Self::Ffmpeg => InstallMethod::Archive(PinnedArchive {
                url: "https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip",
                sha256: "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba",
                bin_prefix: "ffmpeg-9.0.2-essentials_build/bin",
            }),
            Self::SleuthKit => InstallMethod::Archive(PinnedArchive {
                url: "https://github.com/sleuthkit/sleuthkit/releases/download/sleuthkit-4.15.0/sleuthkit-4.15.0-win32.zip",
                sha256: "c2ebab8105b893d97bd8ce35b88e01985e2a106efc97f03adf95840a631b20ce",
                bin_prefix: "sleuthkit-4.15.0-win32/bin",
            }),
            Self::Libewf => InstallMethod::BuildScript,
        }
    }

    /// The package that provides `tool`, if any.
    pub fn providing(tool: &str) -> Option<Self> {
        ALL_PACKAGES
            .into_iter()
            .find(|package| package.provides().contains(&tool))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_every_package_id() {
        for package in ALL_PACKAGES {
            assert_eq!(ToolPackage::parse(package.id()), Some(package));
        }
    }

    #[test]
    fn parse_rejects_unknown_and_path_like_ids() {
        for raw in ["", "ffmpeg.exe", "../ffmpeg", "FFMPEG", "tsk"] {
            assert_eq!(ToolPackage::parse(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn archive_pins_are_https_and_full_sha256() {
        for package in ALL_PACKAGES {
            if let InstallMethod::Archive(pin) = package.method() {
                assert!(pin.url.starts_with("https://"), "{}", pin.url);
                assert_eq!(pin.sha256.len(), 64, "{}", package.id());
                assert!(pin.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
                assert!(!pin.bin_prefix.ends_with('/'));
            }
        }
    }

    #[test]
    fn every_probed_workflow_tool_has_a_provider() {
        for tool in ["ffmpeg", "ffprobe", "ewfinfo", "ewfverify", "ewfexport", "mmls", "fls", "icat"] {
            assert!(ToolPackage::providing(tool).is_some(), "{tool}");
        }
        assert_eq!(ToolPackage::providing("deepfake-lens"), None);
    }

    #[test]
    fn package_dirs_are_distinct() {
        let dirs: std::collections::BTreeSet<_> = ALL_PACKAGES.iter().map(|p| p.dir_name()).collect();
        assert_eq!(dirs.len(), ALL_PACKAGES.len());
    }
}
