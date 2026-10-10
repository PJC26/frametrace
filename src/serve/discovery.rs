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
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".frametrace").join("server.json"))
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
pub(crate) fn find_running_server() -> Option<u16> {
    if let Some((port, instance, token)) = read_instance_record()
        && let Some(body) = probe_status_body(port, token.as_deref())
    {
        // Current servers publish only the nonce's fingerprint (SEC-04);
        // older builds echoed the raw nonce — accept either.
        let matches = [instance_fingerprint(&instance), instance]
            .iter()
            .filter(|value| !value.is_empty())
            .any(|value| {
                let needle = format!("\"instance\":\"{value}\"");
                body.windows(needle.len()).any(|w| w == needle.as_bytes())
            });
        if matches {
            return Some(port);
        }
        // Stale or foreign instance file: keep scanning below.
    }
    for port in 8477..=8486 {
        let Some(buf) = probe_status_body(port, token_for_port(port).as_deref()) else {
            continue;
        };
        let has = |key: &[u8]| buf.windows(key.len()).any(|w| w == key);
        if has(b"\"app\":\"frametrace\"") && has(b"\"has_job\"") && has(b"\"case_dir\"") {
            return Some(port);
        }
    }
    None
}
