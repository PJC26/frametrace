//! HTTP request parsing, loopback/token auth gating, and response/static/media serving.

use super::*;

// Bounded so a malformed client cannot exhaust memory; export payloads
// carry one small JSON record per selected item.
pub(crate) const MAX_BODY: usize = 4 * 1024 * 1024;

pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: String,
    pub(crate) body: String,
    pub(crate) range: Option<String>,
    /// Raw Origin header, when present (CSRF gate for state-changing POSTs).
    pub(crate) origin: Option<String>,
    /// Raw Host header, when present (DNS-rebinding gate).
    pub(crate) host: Option<String>,
    /// Raw Cookie header (ft_token auth when FRAMETRACE_TOKEN is set).
    pub(crate) cookie: Option<String>,
    /// X-FrameTrace-Token header (explicit-token auth for scripts/curl).
    pub(crate) token_header: Option<String>,
    /// X-FrameTrace-Nonce header (instance-nonce proof for mutating
    /// requests when FRAMETRACE_TOKEN is not configured).
    pub(crate) nonce_header: Option<String>,
}

pub(crate) fn handle_connection(mut stream: TcpStream, state: SharedState) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|err| format!("read timeout: {err}"))?;
    // A peer that stops reading must not pin a worker thread forever.
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .map_err(|err| format!("write timeout: {err}"))?;
    let request = read_request(&mut stream)?;
    if !request_is_localhost_trusted(&request) {
        let response = plain(403, b"cross-origin request rejected".to_vec());
        let _ = stream.write_all(&response);
        let _ = stream.flush();
        // Half-close so simple clients see EOF instead of waiting on the
        // read timeout; we never reuse connections.
        let _ = stream.shutdown(std::net::Shutdown::Write);
        return Ok(());
    }
    let token = state_lock(&state).token.clone();
    let mut plant_cookie = false;
    if let Some(token) = token.as_deref() {
        match request_authorized(&request, token) {
            AuthProof::Denied => {
                let _ = stream.write_all(&plain(401, b"unauthorized".to_vec()));
                let _ = stream.shutdown(std::net::Shutdown::Write);
                return Ok(());
            }
            // A successful ?token= request plants the cookie so follow-up
            // viewer fetches (which cannot add headers) stay authorized.
            AuthProof::Query => plant_cookie = true,
            AuthProof::Header | AuthProof::Cookie => {}
        }
    }
    // Without a configured FRAMETRACE_TOKEN, mutating requests still prove
    // they came from the workstation UI: every non-GET/HEAD request must
    // present the per-instance nonce — planted as a SameSite=Strict
    // ft_nonce cookie on page loads, so same-origin fetches carry it
    // automatically while cross-site forms (which browsers strip it from)
    // and non-browser clients that never learned it are refused. Scripts
    // read the nonce from the ft_nonce cookie a GET / plants or from the
    // user-private instance file and send X-FrameTrace-Nonce
    // (/api/status's "instance" is only a fingerprint, not the nonce).
    if token.is_none() && !matches!(request.method.as_str(), "GET" | "HEAD") {
        let instance = state_lock(&state).instance.clone();
        if !request_nonce_authorized(&request, &instance) {
            let _ = stream.write_all(&plain(401, b"unauthorized".to_vec()));
            let _ = stream.shutdown(std::net::Shutdown::Write);
            return Ok(());
        }
    }
    if request.method == "GET" && request.path == "/media" {
        let result = serve_media(&mut stream, &request, &state);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        return result;
    }
    if request.method == "GET"
        && (request.path.starts_with("/review/") || request.path.starts_with("/case/"))
    {
        let result = serve_case_file(&mut stream, &request, &state);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        return result;
    }
    let mut response = route(&request, &state);
    if plant_cookie && let Some(token) = token.as_deref() {
        set_token_cookie(&mut response, token);
    }
    // GETs under the no-token model also plant the instance nonce so the
    // just-served page can satisfy the mutating-request gate.
    if token.is_none() && request.method == "GET" {
        let instance = state_lock(&state).instance.clone();
        if !instance.is_empty() {
            set_nonce_cookie(&mut response, &instance);
        }
    }
    stream
        .write_all(&response)
        .map_err(|err| format!("write failed: {err}"))?;
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    Ok(())
}

pub(crate) fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut buffer: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let header_end = loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|err| format!("read failed: {err}"))?;
        if read == 0 {
            return Err("connection closed before request header ended".into());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break position;
        }
        if buffer.len() > 64 * 1024 {
            return Err("request header too large".into());
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_uppercase();
    let target = parts.next().unwrap_or("/").to_string();
    let mut content_length = 0usize;
    let mut range = None;
    let mut origin = None;
    let mut host = None;
    let mut cookie = None;
    let mut token_header = None;
    let mut nonce_header = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value
                .parse::<usize>()
                .map_err(|_| "invalid content-length")?;
        } else if name.eq_ignore_ascii_case("range") {
            range = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("origin") {
            origin = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("host") {
            host = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("cookie") {
            cookie = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("x-frametrace-token") {
            token_header = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("x-frametrace-nonce") {
            nonce_header = Some(value.to_string());
        }
    }
    if content_length > MAX_BODY {
        return Err("request body too large".into());
    }
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let read = stream
            .read(&mut chunk)
            .map_err(|err| format!("read failed: {err}"))?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (percent_decode_component(path, false), query.to_string()),
        None => (percent_decode_component(&target, false), String::new()),
    };
    Ok(Request {
        method,
        path,
        query,
        body: String::from_utf8_lossy(&body).to_string(),
        range,
        origin,
        host,
        cookie,
        token_header,
        nonce_header,
    })
}

/// Percent-decodes a URL component. `plus_as_space` selects query-string
/// semantics; path components must NOT translate `+` (Unix filenames can
/// legitimately contain it), so callers pass false there.
pub(crate) fn percent_decode_component(input: &str, plus_as_space: bool) -> String {
    let bytes = input.as_bytes();
    let mut output: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => match bytes.get(index + 1..index + 3) {
                Some(hex) => match u8::from_str_radix(&String::from_utf8_lossy(hex), 16) {
                    Ok(byte) => {
                        output.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        output.push(b'%');
                        index += 1;
                    }
                },
                None => {
                    output.push(b'%');
                    index += 1;
                }
            },
            b'+' if plus_as_space => {
                output.push(b' ');
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).to_string()
}

pub(crate) fn query_value(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        // A pair without '=' is a bare flag; skip it instead of aborting the
        // whole scan, so `?flag&path=...` still serves `path`.
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name == key {
            return Some(percent_decode_component(value, true));
        }
    }
    None
}

/// Top-level field extractor for the tiny fixed-shape JSON bodies this
/// server accepts ({"key": value, ...}). Parsed with serde_json instead of
/// a `"key":` substring scan, so a `"key":` literal embedded in a string
/// *value* (e.g. a marks note quoting JSON) can no longer masquerade as a
/// real field. A malformed body simply yields no values, which every
/// caller already maps to the same "missing/invalid input" error shape.
pub(crate) fn body_value(body: &str, key: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok()?;
    match parsed.get(key)? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

pub(crate) fn json_string(value: &str) -> String {
    format!("\"{}\"", json_escape(value))
}

/// The workstation binds loopback only, but any website open in the
/// examiner's browser can still issue `text/plain` form POSTs without a
/// CORS preflight, and a rebinding domain can point at the same port. Gate
/// every state-changing request on same-origin evidence: an Origin header
/// must be absent (curl / the opened page itself) or match the loopback
/// host, and a Host header must resolve to loopback.
pub(crate) fn request_is_localhost_trusted(request: &Request) -> bool {
    if let Some(origin) = request.origin.as_deref() {
        let origin = origin.trim();
        // `null` origins come from sandboxed frames, not our own pages.
        // The authority must parse to an exact loopback host: a prefix
        // match would admit `http://127.0.0.1.evil.com`.
        let loopback_origin = origin
            .strip_prefix("http://")
            .and_then(origin_authority_host)
            .is_some_and(|host| {
                matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]" | "::1")
            });
        if !loopback_origin {
            return false;
        }
    }
    if let Some(host) = request.host.as_deref() {
        // Same authority parser as Origin: a naive split(':') turned
        // `[::1]:8477` into `[` and rejected genuine IPv6 loopback clients.
        let loopback_host = origin_authority_host(host.trim())
            .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1"));
        if !loopback_host {
            return false;
        }
    }
    true
}

/// Constant-time string equality for secrets (token / instance nonce).
/// The length is not secret (tokens are fixed-format), but the byte
/// comparison never short-circuits on the first mismatch.
pub(crate) fn secret_eq(candidate: &str, secret: &str) -> bool {
    let (a, b) = (candidate.as_bytes(), secret.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// Extracts the lowercase host from the authority portion of an
/// `http://` Origin (the scheme is stripped by the caller). Prefix
/// matching is unsafe here: `http://127.0.0.1.evil.com` and
/// `http://127.0.0.1@evil.example` both contain the loopback literal
/// while pointing at an attacker-controlled host.
pub(crate) fn origin_authority_host(authority: &str) -> Option<String> {
    // Origins are `scheme://host[:port]`; a path tail is cut off and any
    // userinfo (`host@real-host`) disqualifies the value outright.
    let authority = authority.split('/').next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 literal: `[host]` followed by nothing or a valid `:port`.
        let end = rest.find(']')?;
        let tail = &rest[end + 1..];
        if let Some(port) = tail.strip_prefix(':') {
            port.parse::<u16>().ok()?;
        } else if !tail.is_empty() {
            return None;
        }
        &rest[..end]
    } else {
        match authority.split_once(':') {
            Some((host, port)) => {
                port.parse::<u16>().ok()?;
                host
            }
            None => authority,
        }
    };
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Tokens are planted into a Cookie header, so they must only contain
/// characters that survive cookie syntax unambiguously.
pub(crate) fn valid_token(token: &str) -> bool {
    token.len() >= 8
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthProof {
    Header,
    Query,
    Cookie,
    Denied,
}

/// Checks the optional shared-secret gate. A valid `?token=` query value
/// authenticates *and* asks the caller to plant the `ft_token` cookie —
/// page-initiated fetches (video elements, fetch() without headers) can
/// only carry cookies, not custom headers.
pub(crate) fn request_authorized(request: &Request, token: &str) -> AuthProof {
    if request
        .token_header
        .as_deref()
        .is_some_and(|value| secret_eq(value, token))
    {
        return AuthProof::Header;
    }
    if query_value(&request.query, "token").is_some_and(|value| secret_eq(&value, token)) {
        return AuthProof::Query;
    }
    let cookie_match = request.cookie.as_deref().is_some_and(|header| {
        header.split(';').any(|pair| {
            let mut parts = pair.trim().splitn(2, '=');
            parts.next().map(str::trim) == Some("ft_token")
                && parts.next().is_some_and(|value| secret_eq(value, token))
        })
    });
    if cookie_match {
        AuthProof::Cookie
    } else {
        AuthProof::Denied
    }
}

/// Inserts the `ft_token` cookie into an already-rendered response, right
/// after the status line.
pub(crate) fn set_token_cookie(response: &mut Vec<u8>, token: &str) {
    let header = format!("Set-Cookie: ft_token={token}; HttpOnly; SameSite=Strict; Path=/\r\n");
    if let Some(pos) = response.windows(2).position(|w| w == b"\r\n") {
        response.splice(pos + 2..pos + 2, header.into_bytes());
    }
}

/// Checks the instance-nonce proof for mutating requests in the no-token
/// model. An empty configured nonce can never authenticate (entropy
/// failure is fail-closed, matching the instance-file check).
pub(crate) fn request_nonce_authorized(request: &Request, nonce: &str) -> bool {
    if nonce.is_empty() {
        return false;
    }
    if request
        .nonce_header
        .as_deref()
        .is_some_and(|value| secret_eq(value, nonce))
    {
        return true;
    }
    if query_value(&request.query, "nonce").is_some_and(|value| secret_eq(&value, nonce)) {
        return true;
    }
    request.cookie.as_deref().is_some_and(|cookie| {
        cookie.split(';').any(|part| {
            let mut parts = part.trim().splitn(2, '=');
            parts.next().map(str::trim) == Some("ft_nonce")
                && parts.next().is_some_and(|value| secret_eq(value, nonce))
        })
    })
}

/// Public, non-secret fingerprint of the instance nonce. `/api/status`
/// publishes this instead of the raw nonce (SEC-04): the UI only needs a
/// stable per-process identity to notice restarts, and discovery can
/// derive the same fingerprint from the user-private instance file, so the
/// mutation secret itself is never echoed by an unauthenticated GET.
pub(crate) fn instance_fingerprint(instance: &str) -> String {
    use sha2::{Digest, Sha256};
    if instance.is_empty() {
        return String::new();
    }
    let digest = Sha256::digest(format!("frametrace-instance-id:{instance}").as_bytes());
    digest[..12].iter().map(|b| format!("{b:02x}")).collect()
}

/// Inserts the `ft_nonce` cookie so the served page's same-origin fetches
/// carry the instance proof automatically. SameSite=Strict keeps it off
/// cross-site form POSTs — the request then fails the nonce gate.
pub(crate) fn set_nonce_cookie(response: &mut Vec<u8>, nonce: &str) {
    let header = format!("Set-Cookie: ft_nonce={nonce}; HttpOnly; SameSite=Strict; Path=/\r\n");
    if let Some(pos) = response.windows(2).position(|w| w == b"\r\n") {
        response.splice(pos + 2..pos + 2, header.into_bytes());
    }
}

pub(crate) fn page(body: Vec<u8>) -> Vec<u8> {
    respond(200, "text/html; charset=utf-8", body, "no-store", None)
}

pub(crate) fn json(body: String) -> Vec<u8> {
    respond(
        200,
        "application/json; charset=utf-8",
        body.into_bytes(),
        "no-store",
        None,
    )
}

pub(crate) fn plain(code: u16, body: Vec<u8>) -> Vec<u8> {
    respond(code, "text/plain; charset=utf-8", body, "no-store", None)
}

/// Serves a file under the case directory. Responses are streamed in 64 KiB
/// chunks rather than read fully into memory — case artifacts (reports,
/// exported bundles) can be large, and the buffered version multiplied
/// memory use by the concurrent-request count.
pub(crate) fn serve_case_file(
    stream: &mut TcpStream,
    request: &Request,
    state: &SharedState,
) -> Result<(), String> {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return write_simple(stream, 404, b"no case loaded");
    };
    let prefix = if request.path.starts_with("/review/") {
        "/review/"
    } else {
        "/case/"
    };
    let relative = request.path.trim_start_matches(prefix);
    if relative.is_empty() || relative.contains("..") {
        return write_simple(stream, 403, b"invalid path");
    }
    let full = if prefix == "/review/" {
        case_dir.join("review").join(relative)
    } else {
        case_dir.join(relative)
    };
    let path = match full.canonicalize() {
        Ok(path) if path_is_under(&case_dir, &path) => path,
        Ok(_) => return write_simple(stream, 403, b"path outside case"),
        Err(_) => return write_simple(stream, 404, b"file not found"),
    };
    // Directories must not reach the streaming path: File::open succeeds
    // on them and the read would fail mid-response.
    let total = match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => meta.len(),
        _ => return write_simple(stream, 404, b"file not found"),
    };
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(_) => return write_simple(stream, 404, b"file not found"),
    };
    let trusted = is_generated_page(&case_dir, &path);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {total}\r\nCache-Control: private, max-age=60\r\nX-Content-Type-Options: nosniff\r\n{}Connection: close\r\n\r\n",
        mime_for(&path),
        content_safety_headers(&path, trusted, false)
    );
    stream
        .write_all(head.as_bytes())
        .map_err(|err| format!("write failed: {err}"))?;
    stream_body(stream, &mut file, total)
}

/// Writes exactly `remaining` bytes from `reader` to `stream` in bounded
/// chunks — the shared transfer loop for media ranges and case files.
pub(crate) fn stream_body(
    stream: &mut TcpStream,
    reader: &mut impl Read,
    mut remaining: u64,
) -> Result<(), String> {
    let mut chunk = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = remaining.min(chunk.len() as u64) as usize;
        let read = reader
            .read(&mut chunk[..want])
            .map_err(|err| format!("file read failed: {err}"))?;
        if read == 0 {
            break;
        }
        stream
            .write_all(&chunk[..read])
            .map_err(|err| format!("file write failed: {err}"))?;
        remaining -= read as u64;
    }
    stream
        .flush()
        .map_err(|err| format!("file write failed: {err}"))
}

pub(crate) fn path_is_under(root: &Path, candidate: &Path) -> bool {
    let Ok(root_canonical) = root.canonicalize() else {
        return false;
    };
    // Component-wise comparison on canonicalized paths: both sides resolve
    // to the filesystem's real casing, so the prefix check is exact. The
    // old case-folded string compare treated `CASE/` as `case/` — on a
    // case-sensitive filesystem that is a *different* directory, i.e. a
    // containment bypass.
    candidate.starts_with(&root_canonical)
}

/// Extensions a browser renders as a script-capable document when the
/// response is navigated to directly.
pub(crate) fn is_active_document(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some(
            "html"
                | "htm"
                | "xhtml"
                | "xht"
                | "shtml"
                | "svg"
                | "svgz"
                | "xml"
                | "xsl"
                | "xslt"
                | "mht"
                | "mhtml"
        )
    )
}

/// SEC-02: true only for HTML pages FrameTrace itself generates — the
/// review viewer (`review/*.html`), report pages (`reports/**.html`) and
/// the same two locations inside a finalized `reports/package_*/` copy.
/// Everything else under the case (extracted evidence, carved/recovered
/// files, exports) may hold attacker-controlled bytes and is served as
/// untrusted content. `canonical` must already be canonicalized.
pub(crate) fn is_generated_page(case_dir: &Path, canonical: &Path) -> bool {
    let is_html = matches!(
        canonical
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("html" | "htm")
    );
    if !is_html {
        return false;
    }
    let Ok(case_canonical) = case_dir.canonicalize() else {
        return false;
    };
    let Ok(relative) = canonical.strip_prefix(&case_canonical) else {
        return false;
    };
    let parts: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut parts: &[String] = &parts;
    if parts.len() > 2 && parts[0] == "reports" && parts[1].starts_with("package_") {
        parts = &parts[2..];
    }
    match parts.first().map(String::as_str) {
        Some("review") => parts.len() == 2,
        Some("reports") => {
            parts.len() >= 2 && !parts[1..].iter().any(|part| part.starts_with("package_"))
        }
        _ => false,
    }
}

/// Security headers for streamed case/media files (SEC-02).
/// - Generated pages: may only be framed by the workstation itself.
/// - Everything else: CSP `sandbox` (no script, opaque origin, so the
///   workstation's mutating APIs are unreachable even if it renders), and
///   script-capable documents are forced to download with
///   `Content-Disposition: attachment`. Raster/video/json responses keep
///   working because CSP and disposition do not apply to subresources.
pub(crate) fn content_safety_headers(path: &Path, trusted_page: bool, download: bool) -> String {
    if trusted_page {
        return "Content-Security-Policy: frame-ancestors 'self'; object-src 'none'; base-uri 'self'\r\n"
            .to_string();
    }
    let mut headers = String::from(
        "Content-Security-Policy: sandbox; default-src 'none'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'unsafe-inline'\r\n",
    );
    if download || is_active_document(path) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("evidence.bin");
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        // RFC 5987/6266: keep the ASCII fallback filename for old clients but
        // also send filename*=UTF-8''<percent-encoded> so Korean/non-ASCII
        // evidence names survive the download instead of collapsing to "___".
        let encoded = percent_encode_header_value(name);
        headers.push_str(&format!(
            "Content-Disposition: attachment; filename=\"{safe}\"; filename*=UTF-8''{encoded}\r\n"
        ));
    }
    headers
}

/// Streams a media file with HTTP Range support so <video> can seek.
pub(crate) fn serve_media(
    stream: &mut TcpStream,
    request: &Request,
    state: &SharedState,
) -> Result<(), String> {
    let Some(raw_path) = query_value(&request.query, "path") else {
        return write_simple(stream, 400, b"missing path");
    };
    let candidate = PathBuf::from(&raw_path);
    let roots = state_lock(state).media_roots.clone();
    let canonical = match candidate.canonicalize() {
        Ok(path) => path,
        Err(_) => return write_simple(stream, 404, b"media not found"),
    };
    let allowed = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| path_is_under(&root, &canonical));
    if !allowed {
        return write_simple(stream, 403, b"path outside approved roots");
    }
    let total = match std::fs::metadata(&canonical) {
        Ok(meta) => meta.len(),
        Err(_) => return write_simple(stream, 404, b"media not found"),
    };
    let (start, end, code) = match request
        .range
        .as_deref()
        .and_then(|value| parse_range(value, total))
    {
        Some((start, end)) if start < total => {
            let end = end.unwrap_or(total - 1).min(total - 1);
            if end < start {
                return write_range_unsatisfiable(stream, total);
            }
            (start, end, 206u16)
        }
        Some(_) => return write_range_unsatisfiable(stream, total),
        // A zero-byte file must report Content-Length: 0. The previous
        // `total.saturating_sub(1)` produced `end = 0, length = 1` and
        // stalled browsers waiting for a byte that never comes.
        None => (0u64, total.saturating_sub(1), 200u16),
    };
    let length = end.saturating_sub(start) + if total == 0 { 0 } else { 1 };
    let mut file = match std::fs::File::open(&canonical) {
        Ok(file) => file,
        Err(_) => return write_simple(stream, 404, b"media not found"),
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        return write_simple(stream, 500, b"seek failed");
    }
    let mut head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {}\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        if code == 206 { "Partial Content" } else { "OK" },
        mime_for(&canonical)
    );
    // `?download=1` forces a browser save-as with the evidence filename
    // instead of inline playback. Same approved-roots containment applies.
    // Evidence is never trusted content: script-capable documents (html,
    // svg, xml) are always forced to download and sandboxed (SEC-02).
    let wants_download = query_value(&request.query, "download").is_some();
    head.push_str(&content_safety_headers(&canonical, false, wants_download));
    if code == 206 {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .map_err(|err| format!("write failed: {err}"))?;
    let mut reader = file;
    stream_body(stream, &mut reader, length)
}

pub(crate) fn write_range_unsatisfiable(stream: &mut TcpStream, total: u64) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(head.as_bytes())
        .map_err(|err| format!("write failed: {err}"))
}

pub(crate) fn write_simple(stream: &mut TcpStream, code: u16, body: &[u8]) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        if code == 200 { "OK" } else { "Error" },
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body))
        .map_err(|err| format!("write failed: {err}"))
}

/// Parses an HTTP `Range` header against a known entity length.
/// Supports `bytes=start-end`, `bytes=start-`, and the suffix form
/// `bytes=-N` (Safari issues suffix ranges; returning None there would
/// make its <video> requests unanswerable).
pub(crate) fn parse_range(value: &str, total: u64) -> Option<(u64, Option<u64>)> {
    let value = value.strip_prefix("bytes=")?;
    let (start_text, end_text) = value.split_once('-')?;
    if start_text.is_empty() {
        // Suffix range: the last N bytes. A zero or unparseable length is
        // unsatisfiable per RFC 7233 — signal it via start >= total so the
        // caller maps to 416.
        let suffix = end_text.parse::<u64>().ok()?;
        if suffix == 0 {
            return Some((u64::MAX, None));
        }
        let start = total.saturating_sub(suffix);
        return Some((start, None));
    }
    let start = start_text.parse::<u64>().ok()?;
    let end = if end_text.is_empty() {
        None
    } else {
        Some(end_text.parse::<u64>().ok()?)
    };
    Some((start, end))
}

pub(crate) fn respond(
    code: u16,
    content_type: &str,
    body: Vec<u8>,
    cache: &str,
    extra: Option<&str>,
) -> Vec<u8> {
    let status_text = match code {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {code} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: {cache}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(extra) = extra {
        head.push_str(extra);
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(&body);
    bytes
}

/// Percent-encodes a value for RFC 5986/5987 ext-value (`filename*=`).
/// attr-char per RFC 5987 §3.2.1; everything else becomes %XX of the UTF-8
/// byte sequence, so Korean and other non-ASCII names round-trip correctly.
pub(crate) fn percent_encode_header_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &b in value.as_bytes() {
        match b {
            b'!'
            | b'#'
            | b'$'
            | b'&'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
            | b'0'..=b'9'
            | b'A'..=b'Z'
            | b'a'..=b'z' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mpg") | Some("mpeg") => "video/mpeg",
        Some("ts") => "video/mp2t",
        Some("avi") => "video/x-msvideo",
        Some("mov") => "video/quicktime",
        Some("tsv") => "text/tab-separated-values; charset=utf-8",
        _ => "application/octet-stream",
    }
}
