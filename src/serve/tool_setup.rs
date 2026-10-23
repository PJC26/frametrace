//! `/api/tools/*`: the in-app installer for missing forensic tools.
//!
//! One install runs at a time on a background thread; the page polls
//! `GET /api/tools/install-status` and re-probes `/api/env?refresh=1` once
//! the phase leaves `running`. Requests pass the same token / instance-nonce
//! gate as every other mutating route (see `http::authorize`).

use super::*;
use crate::tool_install::{self, ALL_PACKAGES, ToolPackage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Running,
    Done,
    Failed,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
struct InstallStatus {
    package: Option<ToolPackage>,
    phase: Phase,
    message: String,
}

static STATUS: Mutex<InstallStatus> = Mutex::new(InstallStatus {
    package: None,
    phase: Phase::Idle,
    message: String::new(),
});

fn status_lock() -> MutexGuard<'static, InstallStatus> {
    STATUS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Claims the single install slot; `Err` while another install runs.
fn try_begin(package: ToolPackage) -> Result<(), String> {
    let mut guard = status_lock();
    if let (Phase::Running, Some(running)) = (guard.phase, guard.package) {
        return Err(format!("{} 설치가 이미 진행 중입니다", running.label()));
    }
    *guard = InstallStatus {
        package: Some(package),
        phase: Phase::Running,
        message: "설치 준비 중".into(),
    };
    Ok(())
}

fn set_message(message: String) {
    status_lock().message = message;
}

fn finish(result: Result<PathBuf, String>) {
    let mut guard = status_lock();
    match result {
        Ok(dir) => {
            guard.phase = Phase::Done;
            guard.message = format!("설치 완료: {}", shell_path(dir).display());
        }
        Err(err) => {
            guard.phase = Phase::Failed;
            guard.message = err;
        }
    }
}

pub(crate) fn api_tools_install(request: &Request) -> String {
    let raw = body_value(&request.body, "package").unwrap_or_default();
    let Some(package) = ToolPackage::parse(&raw) else {
        return api_err("알 수 없는 도구 패키지입니다");
    };
    if let Err(err) = try_begin(package) {
        return api_err(&err);
    }
    let spawned = thread::Builder::new()
        .name(format!("tool-install-{}", package.id()))
        .spawn(move || finish(tool_install::install(package, &set_message)));
    match spawned {
        Ok(_) => serde_json::json!({ "ok": true, "package": package.id() }).to_string(),
        Err(err) => {
            finish(Err(format!("설치 스레드를 시작할 수 없습니다: {err}")));
            api_err("설치를 시작할 수 없습니다")
        }
    }
}

pub(crate) fn api_tools_install_status() -> String {
    let status = status_lock().clone();
    let packages: Vec<_> = ALL_PACKAGES.iter().map(|p| package_json(*p)).collect();
    serde_json::json!({
        "ok": true,
        "supported": cfg!(windows),
        "phase": status.phase.as_str(),
        "package": status.package.map(ToolPackage::id),
        "message": status.message,
        "packages": packages,
    })
    .to_string()
}

fn package_json(package: ToolPackage) -> serde_json::Value {
    let installed = package
        .provides()
        .iter()
        .all(|tool| crate::tool_policy::resolve_tool_binary(tool, &[tool]).is_ok());
    serde_json::json!({
        "id": package.id(),
        "label": package.label(),
        "version": package.version(),
        "estimate": package.estimate(),
        "provides": package.provides(),
        "installed": installed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        *status_lock() = InstallStatus { package: None, phase: Phase::Idle, message: String::new() };
    }

    // One test owns the global slot end to end so parallel test threads
    // never observe each other's phase.
    #[test]
    fn install_slot_is_exclusive_and_reports_outcome() {
        reset();
        assert_eq!(try_begin(ToolPackage::Ffmpeg), Ok(()));

        let busy = try_begin(ToolPackage::SleuthKit).unwrap_err();
        assert!(busy.contains("FFmpeg"), "{busy}");

        finish(Err("네트워크 오류".into()));
        let body: serde_json::Value = serde_json::from_str(&api_tools_install_status()).unwrap();
        assert_eq!(body["phase"], "failed");
        assert_eq!(body["message"], "네트워크 오류");
        assert_eq!(try_begin(ToolPackage::SleuthKit), Ok(()), "a failed install frees the slot");
        reset();
    }

    #[test]
    fn install_rejects_unknown_package_without_claiming_the_slot() {
        let request = Request {
            method: "POST".into(),
            path: "/api/tools/install".into(),
            query: String::new(),
            body: r#"{"package":"../evil"}"#.into(),
            range: None,
            origin: None,
            host: Some("127.0.0.1:8477".into()),
            cookie: None,
            token_header: None,
            nonce_header: None,
        };
        let body: serde_json::Value = serde_json::from_str(&api_tools_install(&request)).unwrap();
        assert_eq!(body["ok"], false);
    }

    #[test]
    fn status_lists_every_catalog_package() {
        let body: serde_json::Value = serde_json::from_str(&api_tools_install_status()).unwrap();
        let ids: Vec<_> = body["packages"].as_array().unwrap().iter().map(|p| p["id"].clone()).collect();
        assert_eq!(ids, ["ffmpeg", "sleuthkit", "libewf"]);
    }
}
