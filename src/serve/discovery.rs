//! Already-running server discovery: per-user instance file + structural loopback probe.

use super::*;

/// Path of the per-user instance file. `FRAMETRACE_INSTANCE_FILE` overrides
/// it (tests, multi-instance harnesses); the default lives in the user's
/// home directory so other local users cannot read the nonce.
pub(crate) fn instance_file_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("FRAMETRACE_INSTANCE_FILE")
        && !custom.trim().is_empty()
    {
        return Some(PathBuf::from(custom));
    }
    Some(home_dir()?.join(".frametrace").join("server.json"))
}

/// The user's home: on Windows `USERPROFILE` first — a stray `HOME` (set by
/// Git Bash / MSYS shells, sometimes to another drive) must not move the
/// instance file away from where the windowed app (launched from Explorer,
/// no `HOME`) looks for it. Elsewhere `HOME` first.
fn home_dir() -> Option<PathBuf> {
    let (first, second) = if cfg!(windows) {
        ("USERPROFILE", "HOME")
    } else {
        ("HOME", "USERPROFILE")
    };
    std::env::var_os(first)
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os(second).filter(|value| !value.is_empty()))
        .map(PathBuf::from)
}

/// Removes the instance file at shutdown, but only while it still records
/// THIS process (a newer server may have overwritten it; deleting that
/// would orphan the live instance from discovery). Failures are reported,
/// not swallowed. Returns whether a file was removed.
// Called from serve::run once the accept loop ends (integration step).
pub(crate) fn remove_instance_file_if_owned() -> bool {
    let Some(path) = instance_file_path() else {
        return false;
    };
    match remove_instance_file_if_pid(&path, std::process::id()) {
        Ok(removed) => removed,
        Err(err) => {
            eprintln!(
                "인스턴스 파일을 지우지 못했습니다 ({}): {err} / could not remove the instance file",
                path.display()
            );
            false
        }
    }
}

/// Deletes `path` only when its recorded `pid` equals `pid`. A missing file
/// is `Ok(false)`; a file of another process (or unparsable) is left alone.
/// Read-then-delete is not atomic: a server starting in the same instant
/// could rewrite the file in between and lose it; that server is then just
/// not discoverable until its next start (a stale window, never a leak).
pub(crate) fn remove_instance_file_if_pid(path: &Path, pid: u32) -> Result<bool, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(format!("read failed: {err}")),
    };
    let recorded = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| value.get("pid").and_then(|pid| pid.as_u64()));
    if recorded != Some(u64::from(pid)) {
        return Ok(false);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("remove failed: {err}")),
    }
}

/// Records `{port, pid, instance, token?}` in a user-private file (0600 on unix).
/// The nonce lets future launches distinguish the real workstation from a
/// process that merely prints the `"app":"frametrace"` marker.
pub(crate) fn write_instance_file(port: u16, instance: &str, token: Option<&str>) {
    let Some(path) = instance_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let token_field = match token {
        Some(token) => format!(",\"token\":{}", json_string(token)),
        None => String::new(),
    };
    let body = format!(
        "{{\"port\":{port},\"pid\":{},\"instance\":{}{token_field}}}",
        std::process::id(),
        json_string(instance)
    );
    if std::fs::write(&path, body).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

/// Reads the recorded `{port, instance, token?}` triple (token: SEC-04), if
/// the file exists and parses. Returns `None` for missing/corrupt files —
/// callers then fall back to the structural port scan.
pub(crate) fn read_instance_record() -> Option<(u16, String, Option<String>)> {
    let text = std::fs::read_to_string(instance_file_path()?).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let port = value.get("port")?.as_u64()? as u16;
    let instance = value.get("instance")?.as_str()?.to_string();
    if instance.is_empty() {
        return None;
    }
    let token = value
        .get("token")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    Some((port, instance, token))
}

/// Token to reach the workstation on `port`: FRAMETRACE_TOKEN if set,
/// else the one recorded in the user-private instance file for that port.
pub(crate) fn token_for_port(port: u16) -> Option<String> {
    if let Ok(token) = std::env::var("FRAMETRACE_TOKEN")
        && !token.trim().is_empty()
    {
        return Some(token.trim().to_string());
    }
    read_instance_record()
        .filter(|(instance_port, _, _)| *instance_port == port)
        .and_then(|(_, _, token)| token)
}

pub(crate) fn probe_status_body(port: u16, token: Option<&str>) -> Option<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(800)));
    let auth = token
        .map(|token| format!("X-FrameTrace-Token: {token}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
        if buf.windows(18).any(|w| w == b"\"app\":\"frametrace\"") {
            break;
        }
    }
    Some(buf)
}

/// Probes for an already-running FrameTrace server. The instance file is
/// the primary channel: the responder must echo the nonce only our server
/// knows. When no instance file exists (server started by an older build),
/// fall back to the structural scan — but require the full status shape
/// (`app` + `has_job` + `case_dir` keys), not just the spoofable marker.
///
/// Residual risk of the primary path (Windows review item 6, accepted):
/// the token recorded in the instance file is sent to that file's port
/// BEFORE the responder has proven it is our server (the nonce check needs
/// the authenticated status body). If our server died and another local
/// process grabbed the port, that process learns the dead server's token.
/// Accepted because (a) the file is user-private (0600 / the user's
/// profile), so only a process that could already read the token from disk
/// can steer us to a port; (b) the token is a per-process random value that
/// dies with the server that minted it — the next server mints a new one;
/// (c) a clean shutdown removes the file while it still names this process
/// ([`remove_instance_file_if_owned`]), narrowing the window to crashes.
///
/// R4-B1: there is NO structural fallback any more. A process that merely
/// answers `/api/status` like a workstation on 8477..8486 (but is not
/// named by the instance file) used to be reused — the browser was then
/// pointed at it. Now only an instance-file-verified server is reused;
/// any other occupant of a preferred port is skipped and a fresh
/// workstation starts on the next free port (with a warning).
pub(crate) fn find_running_server() -> Option<u16> {
    verified_running_server(read_instance_record())
}

/// [`find_running_server`] on an explicit instance record (tests).
pub(crate) fn verified_running_server(
    record: Option<(u16, String, Option<String>)>,
) -> Option<u16> {
    let (port, instance, token) = record?;
    let body = probe_status_body(port, token.as_deref())?;
    // Current servers publish only the nonce's fingerprint (SEC-04);
    // older builds echoed the raw nonce — accept either.
    let matches = [instance_fingerprint(&instance), instance]
        .iter()
        .filter(|value| !value.is_empty())
        .any(|value| {
            let needle = format!("\"instance\":\"{value}\"");
            body.windows(needle.len()).any(|w| w == needle.as_bytes())
        });
    matches.then_some(port)
}

/// R4-B2: asks the (verified) workstation on `port` for a one-time launch
/// code, authenticating with `token` in a header. `None` on any failure —
/// the caller then prints the bare URL instead of ever printing the token.
pub(crate) fn request_launch_code(port: u16, token: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let request = format!(
        "POST /api/launch-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-FrameTrace-Token: {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let _ = stream.take(64 * 1024).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    if !text.starts_with("HTTP/1.1 200") {
        return None;
    }
    let body = text.split("\r\n\r\n").nth(1)?;
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    value
        .get("code")
        .and_then(serde_json::Value::as_str)
        .filter(|code| code.len() == 32 && code.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
}

/// A full, successful `/api/status` reply: a `200` status line plus the
/// `app` / `has_job` / `case_dir` keys (the marker alone is spoofable).
/// R4-B1: no longer enough to reuse a server (identity needs the instance
/// file); kept for diagnostics and its tests.
#[cfg(test)]
pub(crate) fn status_body_is_workstation(buf: &[u8]) -> bool {
    let ok_line = buf.starts_with(b"HTTP/1.1 200") || buf.starts_with(b"HTTP/1.0 200");
    let has = |key: &[u8]| buf.windows(key.len()).any(|w| w == key);
    ok_line && has(b"\"app\":\"frametrace\"") && has(b"\"has_job\"") && has(b"\"case_dir\"")
}

/// Port and token of the server running in THIS process, published by `run`
/// right after bind and before any instance-file write. The WebView2 shell
/// reads it instead of racing the instance file (which may be unwritable,
/// HOME-less, or not yet written when the TCP connect succeeds).
static LOCAL_SERVER: Mutex<Option<(u16, Option<String>)>> = Mutex::new(None);

pub(crate) fn publish_local_server(port: u16, token: Option<&str>) {
    *LOCAL_SERVER.lock().unwrap_or_else(|p| p.into_inner()) =
        Some((port, token.map(str::to_string)));
}

/// `Some(token_or_none)` once the in-process server for `port` has published.
pub(crate) fn local_server_token(port: u16) -> Option<Option<String>> {
    LOCAL_SERVER
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .filter(|(published, _)| *published == port)
        .map(|(_, token)| token.clone())
}

/// Token for the shell: the in-process value wins; the instance file is a
/// fallback for a separately launched shell attaching to another server.
pub(crate) fn resolve_shell_token(
    port: u16,
    file_lookup: impl Fn(u16) -> Option<String>,
) -> Option<String> {
    match local_server_token(port) {
        Some(token) => token,
        None => file_lookup(port),
    }
}

pub(crate) fn build_workstation_url(port: u16, token: Option<&str>) -> String {
    match token {
        Some(token) => format!("http://127.0.0.1:{port}/?token={token}"),
        None => format!("http://127.0.0.1:{port}/"),
    }
}

/// What the shell should show: the workstation URL, or an explicit error
/// (instead of silently navigating into a blank 401).
/// `owned`: the server runs in this process, so wait briefly for it to
/// publish its token.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn shell_target(port: u16, owned: bool, wait: Duration) -> Result<String, String> {
    if owned {
        let deadline = std::time::Instant::now() + wait;
        while local_server_token(port).is_none() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        if local_server_token(port).is_none() {
            return Err(format!(
                "워크스테이션 서버가 시작되지 않았습니다 (127.0.0.1:{port}). / The workstation server did not start."
            ));
        }
    }
    let token = resolve_shell_token(port, token_for_port);
    if token.is_none() {
        let denied = probe_status_body(port, None)
            .map(|body| body.starts_with(b"HTTP/1.1 401") || body.starts_with(b"HTTP/1.0 401"))
            .unwrap_or(false);
        if denied {
            return Err(format!(
                "서버(127.0.0.1:{port})가 접근 토큰을 요구하지만 찾을 수 없습니다. 앱을 다시 시작하거나 FRAMETRACE_TOKEN을 설정하세요. / The server requires an access token that could not be found; restart the app or set FRAMETRACE_TOKEN."
            ));
        }
    }
    Ok(build_workstation_url(port, token.as_deref()))
}

#[cfg(test)]
mod tests_shell_token {
    use super::*;

    #[test]
    fn in_process_token_wins_even_without_instance_file() {
        publish_local_server(1, Some("tok12345"));
        let token = resolve_shell_token(1, |_| None);
        assert_eq!(token.as_deref(), Some("tok12345"));
        assert_eq!(
            build_workstation_url(1, token.as_deref()),
            "http://127.0.0.1:1/?token=tok12345"
        );
        // A different port falls back to the file lookup.
        assert_eq!(
            resolve_shell_token(2, |_| Some("fromfile".into())).as_deref(),
            Some("fromfile")
        );
    }
}

#[cfg(test)]
mod tests_instance_file {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ft-disc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn instance_file_removed_only_when_pid_matches() {
        let dir = scratch("owned");
        let file = dir.join("server.json");
        std::fs::write(&file, r#"{"port":8477,"pid":4242,"instance":"n"}"#).unwrap();
        // Another process's record stays in place.
        assert_eq!(remove_instance_file_if_pid(&file, 4243), Ok(false));
        assert!(file.is_file(), "a foreign instance file must survive");
        // Our own record is removed.
        assert_eq!(remove_instance_file_if_pid(&file, 4242), Ok(true));
        assert!(!file.exists());
        // Already gone is not an error.
        assert_eq!(remove_instance_file_if_pid(&file, 4242), Ok(false));
        // Corrupt / pid-less files are never deleted.
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(remove_instance_file_if_pid(&file, 4242), Ok(false));
        assert!(file.is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fallback_shape_requires_200_and_full_status() {
        let full: &[u8] =
            b"HTTP/1.1 200 OK\r\n\r\n{\"app\":\"frametrace\",\"has_job\":false,\"case_dir\":null}";
        assert!(status_body_is_workstation(full));
        let denied: &[u8] = b"HTTP/1.1 401 Unauthorized\r\n\r\n{\"app\":\"frametrace\",\"has_job\":false,\"case_dir\":null}";
        assert!(!status_body_is_workstation(denied));
        let marker_only: &[u8] = b"HTTP/1.1 200 OK\r\n\r\n{\"app\":\"frametrace\"}";
        assert!(!status_body_is_workstation(marker_only));
    }

    /// The fallback scan never offers a token: a fake listener captures the
    /// raw request the unauthenticated probe sends.
    #[test]
    fn unauthenticated_probe_sends_no_token_header() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            String::from_utf8_lossy(&buf).to_string()
        });
        let _ = probe_status_body(port, None);
        let request = seen.join().unwrap();
        assert!(request.starts_with("GET /api/status"), "{request}");
        assert!(
            !request.to_ascii_lowercase().contains("x-frametrace-token"),
            "{request}"
        );
    }

    #[test]
    fn instance_file_path_prefers_userprofile_on_windows() {
        // Reads the real environment only (never writes): the chosen home
        // must be the platform's primary variable whenever it is set.
        let primary = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        if let Some(value) = std::env::var_os(primary).filter(|v| !v.is_empty()) {
            assert_eq!(home_dir(), Some(PathBuf::from(value)));
        }
    }
}
