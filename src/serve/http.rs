//! HTTP request parsing, loopback/token auth gating, and response/static/media serving.

use super::*;

// Bounded so a malformed client cannot exhaust memory; export payloads
// carry one small JSON record per selected item.
pub(crate) const MAX_BODY: usize = 4 * 1024 * 1024;

/// SEC-06: `/api/capture-frame` carries a base64 canvas JPEG (quality
/// 0.92). A 4K frame is 3-5 MB of JPEG, i.e. up to ~7 MB of base64 — more
/// than [`MAX_BODY`] — so that one route gets a larger request cap.
pub(crate) const MAX_CAPTURE_FRAME_BODY: usize = 16 * 1024 * 1024;

/// Request-body cap for a route (decided from the request line, before any
/// body byte is read).
pub(crate) fn http_body_limit_for(path: &str) -> usize {
    if path == "/api/capture-frame" {
        MAX_CAPTURE_FRAME_BODY
    } else {
        MAX_BODY
    }
}

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Windows review item 11: the workstation URL carries `?token=` on first
/// load, so no response may leak it (or any evidence path in a query) to a
/// link target through the Referer header. Sent on every response.
pub(crate) const REFERRER_POLICY: &str = "Referrer-Policy: no-referrer\r\n";

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

/// Headers only the static-file path needs; kept out of [`Request`] so the
/// many `Request { .. }` literals in tests stay valid.
#[derive(Default, Clone)]
pub(crate) struct RequestExtras {
    pub(crate) if_none_match: Option<String>,
}

/// A request that could not be read; `status == 0` means "close silently".
#[derive(Debug)]
pub(crate) struct HttpReadError {
    pub(crate) status: u16,
    pub(crate) message: String,
}

fn read_error(status: u16, message: impl Into<String>) -> HttpReadError {
    HttpReadError {
        status,
        message: message.into(),
    }
}

fn io_read_error(err: &std::io::Error) -> HttpReadError {
    let timed_out = matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    );
    read_error(
        if timed_out { 408 } else { 0 },
        format!("read failed: {err}"),
    )
}

/// How a request must prove itself (SEC-01/SEC-04 gate classes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpAuthClass {
    /// No proof needed (no token configured, read-only GET/HEAD).
    Public,
    /// FRAMETRACE_TOKEN configured: every request carries it.
    Token,
    /// No token configured, state-changing request: instance nonce.
    Nonce,
}

pub(crate) fn http_auth_class(method: &str, token_configured: bool) -> HttpAuthClass {
    if token_configured {
        HttpAuthClass::Token
    } else if matches!(method, "GET" | "HEAD") {
        HttpAuthClass::Public
    } else {
        HttpAuthClass::Nonce
    }
}

/// Gate for one request. `Ok(plant_cookie)` lets it through (a successful
/// `?token=` asks for the ft_token cookie so follow-up viewer fetches,
/// which cannot add headers, stay authorized); `Err(status)` is the
/// refusal code (403 cross-origin/rebinding, 401 missing proof).
///
/// Without a configured token, mutating requests must present the
/// per-instance nonce — planted as a SameSite=Strict ft_nonce cookie on
/// page loads, so same-origin fetches carry it while cross-site forms and
/// clients that never learned it are refused. Scripts read the nonce from
/// the ft_nonce cookie a GET / plants or from the user-private instance
/// file and send X-FrameTrace-Nonce (/api/status's "instance" is only a
/// fingerprint, not the nonce).
pub(crate) fn authorize(
    request: &Request,
    token: Option<&str>,
    instance: &str,
) -> Result<bool, u16> {
    if !request_is_localhost_trusted(request) {
        return Err(403);
    }
    match http_auth_class(&request.method, token.is_some()) {
        HttpAuthClass::Public => Ok(false),
        HttpAuthClass::Token => match request_authorized(request, token.unwrap_or_default()) {
            AuthProof::Denied => Err(401),
            AuthProof::Query => Ok(true),
            AuthProof::Header | AuthProof::Cookie => Ok(false),
        },
        HttpAuthClass::Nonce => {
            if request_nonce_authorized(request, instance) {
                Ok(false)
            } else {
                Err(401)
            }
        }
    }
}

/// Writes the final response and half-closes so simple clients see EOF
/// instead of waiting on the read timeout; connections are never reused.
pub(crate) fn finish(stream: &mut TcpStream, response: &[u8]) -> Result<(), String> {
    let written = stream
        .write_all(response)
        .map_err(|err| format!("write failed: {err}"));
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    written
}

/// After refusing a request whose body is still in flight (411/413/431),
/// read and discard what the client keeps sending (bounded in bytes and
/// time) so closing the socket does not RST the connection and swallow the
/// status line the client has not read yet.
pub(crate) fn drain_unread(stream: &mut TcpStream) {
    const DRAIN_CAP: usize = 16 * 1024 * 1024;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let started = std::time::Instant::now();
    let mut sink = [0u8; 16 * 1024];
    let mut total = 0usize;
    while total < DRAIN_CAP && started.elapsed() < Duration::from_secs(5) {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => total += n,
        }
    }
}

pub(crate) fn handle_connection(stream: TcpStream, state: SharedState) -> Result<(), String> {
    handle_connection_timed(stream, state, HEADER_READ_TIMEOUT)
}

pub(crate) fn handle_connection_timed(
    mut stream: TcpStream,
    state: SharedState,
    read_timeout: Duration,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|err| format!("read timeout: {err}"))?;
    // A peer that stops reading must not pin a worker thread forever.
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .map_err(|err| format!("write timeout: {err}"))?;
    HEAD_ONLY.with(|flag| flag.set(false));
    let (mut request, extras) = match read_request_ext(&mut stream, read_timeout) {
        Ok(read) => read,
        Err(err) => {
            if err.status != 0 {
                let _ = finish(
                    &mut stream,
                    &plain(err.status, err.message.clone().into_bytes()),
                );
                if matches!(err.status, 400 | 411 | 413 | 431) {
                    drain_unread(&mut stream);
                }
            }
            return Err(err.message);
        }
    };
    // HEAD is answered exactly like GET minus the body.
    let is_head = request.method == "HEAD";
    if is_head {
        request.method = "GET".into();
        HEAD_ONLY.with(|flag| flag.set(true));
    }
    let (token, instance) = {
        let guard = state_lock(&state);
        (guard.token.clone(), guard.instance.clone())
    };
    let plant_cookie = match authorize(&request, token.as_deref(), &instance) {
        Ok(plant) => plant,
        Err(403) => {
            return finish(
                &mut stream,
                &plain(403, b"cross-origin request rejected".to_vec()),
            );
        }
        Err(code) => return finish(&mut stream, &plain(code, b"unauthorized".to_vec())),
    };
    // Item 8: UNC/device paths are refused before any handler (and thus
    // any filesystem call) sees them.
    if !path_fields_for(&request.method, &request.path).is_empty()
        && let Some(reason) = remote_path_violation(&request, &approved_path_roots(&state))
    {
        return finish(&mut stream, &remote_path_refusal(reason));
    }
    if request.method == "GET" && request.path == "/media" {
        let result = serve_media(&mut stream, &request, &state);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        return result;
    }
    // `/viewer/` and `/viewer/index.html` are the embedded viewer page
    // (routed below); everything else under `/viewer/` is a page-relative
    // subresource (thumbs/, data-bundle.js) that lives in case/review/.
    let viewer_subresource =
        request.path.starts_with("/viewer/") && !is_viewer_page_path(&request.path);
    if request.method == "GET"
        && (request.path.starts_with("/review/")
            || request.path.starts_with("/case/")
            || viewer_subresource)
    {
        let result = serve_case_file_with(&mut stream, &request, &extras, &state);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        return result;
    }
    let mut response = route(&request, &state);
    if plant_cookie && let Some(token) = token.as_deref() {
        set_token_cookie(&mut response, token);
    }
    // GETs under the no-token model also plant the instance nonce so the
    // just-served page can satisfy the mutating-request gate.
    if token.is_none() && request.method == "GET" && !instance.is_empty() {
        set_nonce_cookie(&mut response, &instance);
    }
    if is_head && let Some(end) = response.windows(4).position(|w| w == b"\r\n\r\n") {
        response.truncate(end + 4);
    }
    finish(&mut stream, &response)
}

/// Parsed request line + headers (everything before the body).
pub(crate) struct ParsedHead {
    pub(crate) request: Request,
    pub(crate) extras: RequestExtras,
    pub(crate) content_length: usize,
}

/// Parses the header block. Refuses `Transfer-Encoding` bodies (SEC-06:
/// this server only understands Content-Length framing, and honouring a
/// chunked body as empty would desync/smuggle), conflicting duplicate
/// Content-Length values, and bodies over the per-route cap.
pub(crate) fn parse_request_head(head: &str) -> Result<ParsedHead, HttpReadError> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_uppercase();
    let target = parts.next().unwrap_or("/").to_string();
    let mut content_length: Option<usize> = None;
    let mut transfer_encoding = false;
    let mut range = None;
    let mut origin = None;
    let mut host = None;
    let mut cookie = None;
    let mut token_header = None;
    let mut nonce_header = None;
    let mut extras = RequestExtras::default();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value
                .parse::<usize>()
                .map_err(|_| read_error(400, "invalid content-length"))?;
            if content_length.is_some_and(|previous| previous != parsed) {
                return Err(read_error(400, "conflicting content-length"));
            }
            content_length = Some(parsed);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            transfer_encoding = true;
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
        } else if name.eq_ignore_ascii_case("if-none-match") {
            extras.if_none_match = Some(value.to_string());
        }
    }
    if transfer_encoding {
        return Err(read_error(
            411,
            "Transfer-Encoding request bodies are not supported; send Content-Length",
        ));
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (percent_decode_component(path, false), query.to_string()),
        None => (percent_decode_component(&target, false), String::new()),
    };
    let content_length = content_length.unwrap_or(0);
    if content_length > http_body_limit_for(&path) {
        return Err(read_error(413, "request body too large"));
    }
    Ok(ParsedHead {
        request: Request {
            method,
            path,
            query,
            body: String::new(),
            range,
            origin,
            host,
            cookie,
            token_header,
            nonce_header,
        },
        extras,
        content_length,
    })
}

/// Reads one request. `header_deadline` bounds the whole header phase (a
/// slowloris client dripping a byte at a time resets the per-read socket
/// timeout forever, so a wall-clock deadline is also enforced); the body
/// gets 4x that budget.
pub(crate) fn read_request_ext(
    stream: &mut TcpStream,
    header_deadline: Duration,
) -> Result<(Request, RequestExtras), HttpReadError> {
    let started = std::time::Instant::now();
    let mut buffer: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let header_end = loop {
        if started.elapsed() > header_deadline {
            return Err(read_error(408, "request header timed out"));
        }
        let read = stream.read(&mut chunk).map_err(|err| io_read_error(&err))?;
        if read == 0 {
            return Err(read_error(
                0,
                "connection closed before request header ended",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break position;
        }
        if buffer.len() > 64 * 1024 {
            return Err(read_error(431, "request header too large"));
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let parsed = parse_request_head(&head)?;
    let content_length = parsed.content_length;
    let mut body = buffer[header_end + 4..].to_vec();
    let body_deadline = header_deadline * 4;
    while body.len() < content_length {
        if started.elapsed() > body_deadline {
            return Err(read_error(408, "request body timed out"));
        }
        let read = stream.read(&mut chunk).map_err(|err| io_read_error(&err))?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    let mut request = parsed.request;
    request.body = String::from_utf8_lossy(&body).to_string();
    Ok((request, parsed.extras))
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
/// must be absent (curl / the shell's raw-socket requests / the opened
/// page's own same-origin GETs) or name exactly this server's authority,
/// and a Host header must resolve to loopback.
///
/// Origin present => the request came from a browser page, so its full
/// authority (host *and* port) must equal the Host authority: a page served
/// by some other localhost port (a dev server, another local tool) is a
/// different origin and must not drive this workstation with the examiner's
/// cookies (Windows review item 7). Host is then mandatory — without it the
/// comparison cannot be made.
pub(crate) fn request_is_localhost_trusted(request: &Request) -> bool {
    if let Some(origin) = request.origin.as_deref() {
        let origin = origin.trim();
        // `null` origins come from sandboxed frames, not our own pages.
        // The authority must parse to an exact loopback host: a prefix
        // match would admit `http://127.0.0.1.evil.com`.
        let Some((origin_host, origin_port)) =
            origin.strip_prefix("http://").and_then(authority_host_port)
        else {
            return false;
        };
        if !matches!(origin_host.as_str(), "127.0.0.1" | "localhost" | "::1") {
            return false;
        }
        let Some((host_host, host_port)) = request
            .host
            .as_deref()
            .and_then(|host| authority_host_port(host.trim()))
        else {
            return false;
        };
        if origin_host != host_host || origin_port != host_port {
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
    authority_host_port(authority).map(|(host, _)| host)
}

/// `(lowercase host, port)` of an `http` authority (`host[:port]`, IPv6 in
/// brackets — returned without them). A missing port normalizes to 80, the
/// http default, so `Origin: http://127.0.0.1` matches `Host: 127.0.0.1`.
pub(crate) fn authority_host_port(authority: &str) -> Option<(String, u16)> {
    // Origins are `scheme://host[:port]`; a path tail is cut off and any
    // userinfo (`host@real-host`) disqualifies the value outright.
    let authority = authority.split('/').next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 literal: `[host]` followed by nothing or a valid `:port`.
        let end = rest.find(']')?;
        let tail = &rest[end + 1..];
        let port = if let Some(port) = tail.strip_prefix(':') {
            port.parse::<u16>().ok()?
        } else if tail.is_empty() {
            80
        } else {
            return None;
        };
        (&rest[..end], port)
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().ok()?),
            None => (authority, 80),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some((host.to_ascii_lowercase(), port))
}

/// Refusal text for a network (UNC) or device-namespace path.
pub(crate) const REMOTE_PATH_ERROR: &str = "네트워크(UNC)·장치 경로는 허용되지 않습니다 — 로컬 드라이브 경로를 사용하십시오. / UNC and device paths are refused.";

/// Windows review item 8: a user-supplied path that names a network share
/// (`\\server\share`, `//server/share`, `\\?\UNC\…`) or the device
/// namespace (`\\.\…`, `\\?\GLOBALROOT…`, `\??\…`) must be refused BEFORE
/// any filesystem call — merely `canonicalize`/`metadata` on a UNC path
/// makes Windows open an SMB session and hand the examiner's NTLM
/// credentials to whatever host the path names, and those GET routes are
/// reachable from any web page (e.g. `<img src=…/media?path=\\evil\x>`).
///
/// Exceptions:
/// - an approved root that is itself UNC admits paths under that exact
///   share prefix (case-insensitive, component boundary, no `..`);
/// - the verbatim local-drive form `\\?\C:\…` (what `canonicalize` returns
///   on Windows) stays allowed: it never leaves the machine.
///
/// The check is textual and platform-independent, so it holds on every OS.
///
/// Policy (which routes use this vs. [`reject_device_path`]): the SMB/NTLM
/// leak vector is a cross-origin GET — `<img>`/`<video>` send no Origin
/// header, so any web page can make the browser issue one. Every GET route
/// therefore refuses UNC unless it lies under an approved UNC root. The
/// POST routes where the examiner types a source/case path
/// (`/api/preflight`, `/api/start`, see [`route_admits_typed_unc`]) are
/// gated by the Origin port check and the instance nonce, so a typed UNC
/// share is honoured there; device-namespace paths stay refused.
pub(crate) fn reject_remote_path(
    raw: &str,
    approved_roots: &[PathBuf],
) -> Result<(), &'static str> {
    let text = raw.trim();
    if !is_remote_or_device_path(text) {
        return Ok(());
    }
    if is_verbatim_local_drive(text) {
        return Ok(());
    }
    let Some(unc) = unc_share_form(text) else {
        return Err(REMOTE_PATH_ERROR);
    };
    if unc.split('\\').any(|part| part == ".." || part == ".") {
        return Err(REMOTE_PATH_ERROR);
    }
    let admitted = approved_roots
        .iter()
        .filter_map(|root| unc_share_form(&root.to_string_lossy()))
        .any(|root| unc == root || unc.starts_with(&format!("{root}\\")));
    if admitted {
        Ok(())
    } else {
        Err(REMOTE_PATH_ERROR)
    }
}

/// Lenient form of [`reject_remote_path`] for the POST routes where the
/// examiner explicitly types a path ([`route_admits_typed_unc`]): a
/// well-formed UNC share path (`\\server\share\…`, `//server/share/…`,
/// `\\?\UNC\server\share\…`, no `.`/`..` components) is admitted;
/// device-namespace and malformed remote paths are still refused.
pub(crate) fn reject_device_path(raw: &str) -> Result<(), &'static str> {
    let text = raw.trim();
    if is_remote_or_device_path(text)
        && let Some(unc) = unc_share_form(text)
    {
        if unc.split('\\').any(|part| part == ".." || part == ".") {
            return Err(REMOTE_PATH_ERROR);
        }
        return Ok(());
    }
    reject_remote_path(text, &[])
}

/// Routes whose path fields may name a UNC share the examiner typed (see
/// the policy on [`reject_remote_path`]). Only same-origin, nonce-gated
/// POSTs qualify — never a GET.
pub(crate) fn route_admits_typed_unc(method: &str, path: &str) -> bool {
    matches!(
        (method, path),
        ("POST", "/api/preflight") | ("POST", "/api/start")
    )
}

fn is_separator(byte: u8) -> bool {
    byte == b'\\' || byte == b'/'
}

/// Two leading separators (any mix of `\` and `/`) or the NT object prefix.
fn is_remote_or_device_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    (bytes.len() >= 2 && is_separator(bytes[0]) && is_separator(bytes[1]))
        || (bytes.len() >= 4
            && is_separator(bytes[0])
            && bytes[1] == b'?'
            && bytes[2] == b'?'
            && is_separator(bytes[3]))
}

/// `\\?\C:` / `\\?\C:\…` — the verbatim spelling of a local drive path.
fn is_verbatim_local_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 6
        && bytes[0] == b'\\'
        && bytes[1] == b'\\'
        && bytes[2] == b'?'
        && bytes[3] == b'\\'
        && bytes[4].is_ascii_alphabetic()
        && bytes[5] == b':'
        && (bytes.len() == 6 || bytes[6] == b'\\')
}

/// Lowercased `\\server\share[\rest]` form of a UNC path (separators
/// normalized to `\`, `\\?\UNC\` unwrapped, trailing separators dropped);
/// `None` for anything else — device paths, local paths, or a UNC path
/// without both a server and a share component.
fn unc_share_form(text: &str) -> Option<String> {
    let normalized = text.trim().replace('/', "\\");
    let lower = normalized.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix(r"\\?\unc\") {
        rest.to_string()
    } else if lower.starts_with(r"\\?\") || lower.starts_with(r"\\.\") || lower.starts_with(r"\??\")
    {
        return None;
    } else if let Some(rest) = lower.strip_prefix(r"\\") {
        rest.to_string()
    } else {
        return None;
    };
    let rest = rest.trim_end_matches('\\');
    let mut parts = rest.split('\\');
    let server = parts.next().unwrap_or_default();
    let share = parts.next().unwrap_or_default();
    if server.is_empty() || share.is_empty() || matches!(server, "?" | ".") {
        return None;
    }
    Some(format!(r"\\{rest}"))
}

/// Roots that may admit a UNC path: the bound case, the last package and
/// the approved media roots.
pub(crate) fn approved_path_roots(state: &SharedState) -> Vec<PathBuf> {
    let guard = state_lock(state);
    guard
        .case_dir
        .iter()
        .chain(guard.package_dir.iter())
        .chain(guard.media_roots.iter())
        .cloned()
        .collect()
}

/// Request fields that carry a filesystem path, per route. Checked in
/// [`handle_connection_timed`] before the request is routed, so even
/// handlers outside this file (`/api/open-folder` → `resolve_open_folder`)
/// never reach `canonicalize` with a UNC path. Handlers in system.rs /
/// media.rs repeat the check themselves (defense in depth). Routes listed
/// in [`route_admits_typed_unc`] get the lenient check (typed UNC shares
/// admitted, device paths refused); every other route the strict one.
pub(crate) fn path_fields_for(method: &str, path: &str) -> &'static [(bool, &'static str)] {
    // (from_query, field)
    match (method, path) {
        ("GET", "/media") => &[(true, "path")],
        ("GET", "/api/browse") => &[(true, "path")],
        ("GET", "/api/case-info") => &[(true, "case_dir")],
        ("POST", "/api/open-case") => &[(false, "case_dir")],
        ("POST", "/api/open-folder") => &[(false, "path")],
        ("POST", "/api/preflight" | "/api/start") => &[(false, "source_path"), (false, "case_dir")],
        ("POST", "/api/export-clip" | "/api/telemetry" | "/api/proxy") => &[(false, "path")],
        ("POST", "/api/advanced") => &[(false, "extra")],
        _ => &[],
    }
}

/// 400 JSON body for a refused UNC/device path (the browse/start screens
/// show `error` as-is).
pub(crate) fn remote_path_refusal(reason: &str) -> Vec<u8> {
    respond(
        400,
        "application/json; charset=utf-8",
        api_err(reason).into_bytes(),
        "no-store",
        None,
    )
}

/// `Some(reason)` when a path field of `request` names a refused UNC or
/// device path (see [`reject_remote_path`]; routes in
/// [`route_admits_typed_unc`] use [`reject_device_path`]).
pub(crate) fn remote_path_violation(
    request: &Request,
    approved_roots: &[PathBuf],
) -> Option<&'static str> {
    let typed_unc_ok = route_admits_typed_unc(&request.method, &request.path);
    path_fields_for(&request.method, &request.path)
        .iter()
        .filter_map(|(from_query, field)| {
            if *from_query {
                query_value(&request.query, field)
            } else {
                body_value(&request.body, field)
            }
        })
        .find_map(|value| {
            if typed_unc_ok {
                reject_device_path(&value).err()
            } else {
                reject_remote_path(&value, approved_roots).err()
            }
        })
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

/// True when `relative` (already percent-decoded) is a plain relative path
/// that cannot climb out of its base: non-empty, no NUL, no absolute/drive
/// component and no `..` *component*. A `..` inside a file name
/// (`a..b.mp4`) is fine; both `/` and `\\` count as separators so a
/// Windows-style traversal is refused on every platform.
pub(crate) fn relative_path_is_safe(relative: &str) -> bool {
    if relative.is_empty() || relative.contains('\0') {
        return false;
    }
    let mut saw_component = false;
    for part in relative.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return false,
            // `C:` / `C:foo` would re-root a join on Windows.
            part if part.len() >= 2
                && part.as_bytes()[1] == b':'
                && part.as_bytes()[0].is_ascii_alphabetic() =>
            {
                return false;
            }
            _ => saw_component = true,
        }
    }
    // A leading separator would make `join` replace the base entirely.
    saw_component && !relative.starts_with(['/', '\\'])
}

/// Strong validator for a thumbnail (size + mtime nanoseconds).
pub(crate) fn thumb_etag(meta: &std::fs::Metadata) -> String {
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("\"{:x}-{:x}\"", meta.len(), nanos)
}

/// True when an `If-None-Match` header value matches `etag` (list, `*` and
/// weak-prefixed forms; weak comparison is what RFC 9110 prescribes here).
pub(crate) fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    let strip = |tag: &str| tag.trim().trim_start_matches("W/").to_string();
    let ours = strip(etag);
    if_none_match
        .split(',')
        .map(strip)
        .any(|candidate| candidate == "*" || candidate == ours)
}

/// Thumbnails are `review/thumbs/<img>`; they are small, numerous and
/// fetched on every grid scroll (PERF-05).
fn is_thumbnail_path(case_dir: &Path, path: &Path) -> bool {
    let Ok(case_canonical) = case_dir.canonicalize() else {
        return false;
    };
    path.strip_prefix(case_canonical.join("review").join("thumbs"))
        .is_ok()
        && matches!(
            path.extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("jpg" | "jpeg" | "png" | "webp")
        )
}

/// Serves a file under the case directory. Responses are streamed in 64 KiB
/// chunks rather than read fully into memory — case artifacts (reports,
/// exported bundles) can be large, and the buffered version multiplied
/// memory use by the concurrent-request count. Thumbnails carry a strong
/// ETag (304 on a matching `If-None-Match`); a `?v=` content-addressed URL
/// is additionally cacheable as immutable.
pub(crate) fn serve_case_file_with(
    stream: &mut TcpStream,
    request: &Request,
    extras: &RequestExtras,
    state: &SharedState,
) -> Result<(), String> {
    let case_dir = state_lock(state).case_dir.clone();
    let Some(case_dir) = case_dir else {
        return write_simple(stream, 404, b"no case loaded");
    };
    let (prefix, under_review) = if request.path.starts_with("/review/") {
        ("/review/", true)
    } else if request.path.starts_with("/viewer/") {
        ("/viewer/", true)
    } else {
        ("/case/", false)
    };
    let relative = request.path.strip_prefix(prefix).unwrap_or_default();
    if !relative_path_is_safe(relative) {
        return write_simple(stream, 403, b"invalid path");
    }
    let full = if under_review {
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
    let meta = match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => meta,
        _ => return write_simple(stream, 404, b"file not found"),
    };
    let total = meta.len();
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(_) => return write_simple(stream, 404, b"file not found"),
    };
    let trusted = is_generated_page(&case_dir, &path);
    let safety = content_safety_headers(&path, trusted, false);
    let (cache, etag_header) = if is_thumbnail_path(&case_dir, &path) {
        let etag = thumb_etag(&meta);
        let immutable = query_value(&request.query, "v").is_some_and(|v| !v.is_empty());
        let cache = if immutable {
            "private, max-age=31536000, immutable"
        } else {
            "private, max-age=60"
        };
        if extras
            .if_none_match
            .as_deref()
            .is_some_and(|value| etag_matches(value, &etag))
        {
            let head = format!(
                "HTTP/1.1 304 Not Modified\r\nETag: {etag}\r\nCache-Control: {cache}\r\nX-Content-Type-Options: nosniff\r\n{REFERRER_POLICY}Connection: close\r\n\r\n"
            );
            return stream
                .write_all(head.as_bytes())
                .map_err(|err| format!("write failed: {err}"));
        }
        (cache, format!("ETag: {etag}\r\n"))
    } else {
        ("private, max-age=60", String::new())
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {total}\r\nCache-Control: {cache}\r\n{etag_header}X-Content-Type-Options: nosniff\r\n{REFERRER_POLICY}{safety}Connection: close\r\n\r\n",
        mime_for(&path),
    );
    stream
        .write_all(head.as_bytes())
        .map_err(|err| format!("write failed: {err}"))?;
    stream_body(stream, &mut file, total)
}

/// True for the two spellings of the embedded viewer page itself.
pub(crate) fn is_viewer_page_path(path: &str) -> bool {
    matches!(path, "/viewer/" | "/viewer/index.html")
}

/// `GET /viewer/` (UX-02): the evidence viewer compiled into this binary
/// (same slim page make-review writes to review/evidence-viewer.html),
/// served before make-review has run so the examiner can review the
/// phase-1 index while enrichment continues. Data always comes from
/// /api/records (`live=1`) + /api/records-meta. It is a FrameTrace page,
/// so it gets the trusted generated-page headers (frameable by the
/// workstation only), never the sandboxed evidence headers.
pub(crate) fn viewer_page() -> Vec<u8> {
    respond(
        200,
        "text/html; charset=utf-8",
        crate::html_report::render_evidence_viewer_html_slim().into_bytes(),
        "no-store",
        Some(&content_safety_headers(
            Path::new("index.html"),
            true,
            false,
        )),
    )
}

thread_local! {
    // Set on a connection thread answering HEAD: headers go out as for GET,
    // body bytes do not.
    static HEAD_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Writes exactly `remaining` bytes from `reader` to `stream` in bounded
/// chunks — the shared transfer loop for media ranges and case files.
pub(crate) fn stream_body(
    stream: &mut TcpStream,
    reader: &mut impl Read,
    mut remaining: u64,
) -> Result<(), String> {
    if HEAD_ONLY.with(std::cell::Cell::get) {
        return Ok(());
    }
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
    let roots = state_lock(state).media_roots.clone();
    // Item 8: textual UNC/device refusal BEFORE canonicalize touches it.
    if let Err(reason) = reject_remote_path(&raw_path, &roots) {
        return write_simple(stream, 400, reason.as_bytes());
    }
    let candidate = PathBuf::from(&raw_path);
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
        Ok(meta) if meta.is_file() => meta.len(),
        _ => return write_simple(stream, 404, b"media not found"),
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
        "HTTP/1.1 {code} {}\r\nContent-Type: {}\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n{REFERRER_POLICY}Connection: close\r\n",
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
        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\n{REFERRER_POLICY}Connection: close\r\n\r\n"
    );
    stream
        .write_all(head.as_bytes())
        .map_err(|err| format!("write failed: {err}"))
}

pub(crate) fn write_simple(stream: &mut TcpStream, code: u16, body: &[u8]) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\n{REFERRER_POLICY}Connection: close\r\n\r\n",
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
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {code} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: {cache}\r\nX-Content-Type-Options: nosniff\r\n{REFERRER_POLICY}Connection: close\r\n",
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

#[cfg(test)]
mod tests_origin_and_paths {
    use super::*;

    fn request(origin: Option<&str>, host: Option<&str>) -> Request {
        Request {
            method: "POST".into(),
            path: "/api/cancel".into(),
            query: String::new(),
            body: String::new(),
            range: None,
            origin: origin.map(str::to_string),
            host: host.map(str::to_string),
            cookie: None,
            token_header: None,
            nonce_header: None,
        }
    }

    #[test]
    fn origin_authority_must_equal_host_authority_including_port() {
        let trusted =
            |o: Option<&str>, h: Option<&str>| request_is_localhost_trusted(&request(o, h));
        // Another local port is another origin.
        assert!(!trusted(
            Some("http://127.0.0.1:9999"),
            Some("127.0.0.1:8477")
        ));
        assert!(trusted(
            Some("http://127.0.0.1:8477"),
            Some("127.0.0.1:8477")
        ));
        // Origin without Host cannot be compared -> refused.
        assert!(!trusted(Some("http://127.0.0.1:8477"), None));
        // IPv6 loopback on both sides.
        assert!(trusted(Some("http://[::1]:8477"), Some("[::1]:8477")));
        assert!(!trusted(Some("http://[::1]:8477"), Some("[::1]:8478")));
        // Default port 80 normalization and host case.
        assert!(trusted(Some("http://127.0.0.1"), Some("127.0.0.1:80")));
        assert!(trusted(
            Some("http://LOCALHOST:8477"),
            Some("localhost:8477")
        ));
        // Same port but a different loopback spelling is a different origin.
        assert!(!trusted(
            Some("http://localhost:8477"),
            Some("127.0.0.1:8477")
        ));
        // No Origin (shell raw socket, curl): only the Host gate applies.
        assert!(trusted(None, Some("127.0.0.1:8477")));
        assert!(trusted(None, None));
        assert!(!trusted(None, Some("evil.example:8477")));
        // A non-loopback Origin is refused even when Host echoes it.
        assert!(!trusted(
            Some("http://evil.example:8477"),
            Some("evil.example:8477")
        ));
    }

    #[test]
    fn authority_parser_normalizes_ports() {
        assert_eq!(
            authority_host_port("127.0.0.1"),
            Some(("127.0.0.1".into(), 80))
        );
        assert_eq!(authority_host_port("[::1]:9"), Some(("::1".into(), 9)));
        assert_eq!(authority_host_port("a@b"), None);
        assert_eq!(authority_host_port("host:notaport"), None);
        assert_eq!(authority_host_port("[::1]x"), None);
    }

    #[test]
    fn remote_and_device_paths_are_rejected_textually() {
        for raw in [
            r"\\evil\share\a.mp4",
            "//evil/share/a.mp4",
            r"\/evil\share",
            r"/\evil/share",
            r"\\?\UNC\evil\share\a.mp4",
            r"\\.\PhysicalDrive0",
            r"\\.\C:\x",
            r"\\?\GLOBALROOT\Device\Mup\evil\share",
            r"\??\UNC\evil\share",
            r"\\?\Volume{0000}\x",
            r"  \\evil\share",
            r"\\",
            r"\\evil",
        ] {
            assert_eq!(
                reject_remote_path(raw, &[]),
                Err(REMOTE_PATH_ERROR),
                "{raw} must be refused"
            );
        }
        for raw in [
            r"C:\cases\a.mp4",
            "/home/user/a.mp4",
            r"\cases\a.mp4",
            "relative/a.mp4",
            r"\\?\C:\cases\a.mp4",
            r"\\?\d:",
            "",
        ] {
            assert_eq!(reject_remote_path(raw, &[]), Ok(()), "{raw} is local");
        }
    }

    /// The lenient check for typed paths on nonce-gated POSTs: UNC shares
    /// pass, device-namespace and malformed remote paths do not.
    #[test]
    fn typed_unc_is_admitted_but_device_paths_are_not() {
        for raw in [
            r"\\nas\evidence\case7",
            "//nas/evidence/case7/a.mp4",
            r"\\?\UNC\nas\evidence\a.e01",
            r"  \\nas\evidence",
            r"C:\cases\a.mp4",
            r"\\?\C:\cases",
            "",
        ] {
            assert_eq!(reject_device_path(raw), Ok(()), "{raw}");
        }
        for raw in [
            r"\\.\PhysicalDrive0",
            r"\\.\C:\x",
            r"\\?\GLOBALROOT\Device\Mup\evil\share",
            r"\??\UNC\evil\share",
            r"\\?\Volume{0000}\x",
            r"\\",
            r"\\evil",
            r"\\nas\evidence\..\..\x",
        ] {
            assert_eq!(reject_device_path(raw), Err(REMOTE_PATH_ERROR), "{raw}");
        }
        assert!(route_admits_typed_unc("POST", "/api/preflight"));
        assert!(route_admits_typed_unc("POST", "/api/start"));
        for (method, path) in [
            ("GET", "/media"),
            ("GET", "/api/browse"),
            ("GET", "/api/case-info"),
            ("GET", "/api/preflight"),
            ("POST", "/api/open-case"),
            ("POST", "/api/open-folder"),
        ] {
            assert!(!route_admits_typed_unc(method, path), "{method} {path}");
        }
    }

    #[test]
    fn approved_unc_root_admits_only_its_own_share_prefix() {
        let roots = vec![
            PathBuf::from(r"C:\cases\one"),
            PathBuf::from(r"\\NAS\Evidence\case7"),
        ];
        for raw in [
            r"\\nas\evidence\case7",
            r"\\NAS\Evidence\case7\a.mp4",
            "//nas/evidence/case7/sub/a.mp4",
            r"\\?\UNC\nas\evidence\case7\a.mp4",
        ] {
            assert_eq!(reject_remote_path(raw, &roots), Ok(()), "{raw}");
        }
        for raw in [
            r"\\nas\evidence\case70\a.mp4",
            r"\\nas\evidence\other",
            r"\\nas\evidence",
            r"\\evil\evidence\case7\a.mp4",
            r"\\nas\evidence\case7\..\..\x",
            r"\\.\nas\evidence\case7",
        ] {
            assert_eq!(
                reject_remote_path(raw, &roots),
                Err(REMOTE_PATH_ERROR),
                "{raw}"
            );
        }
        // A canonical (verbatim) UNC root works the same way.
        let verbatim = vec![PathBuf::from(r"\\?\UNC\nas\evidence")];
        assert_eq!(
            reject_remote_path(r"\\nas\evidence\a.mp4", &verbatim),
            Ok(())
        );
    }

    #[test]
    fn rendered_responses_carry_referrer_policy() {
        for response in [
            plain(404, b"x".to_vec()),
            json("{}".into()),
            page(b"<html></html>".to_vec()),
            remote_path_refusal(REMOTE_PATH_ERROR),
        ] {
            let text = String::from_utf8_lossy(&response);
            assert!(
                text.contains("\r\nReferrer-Policy: no-referrer\r\n"),
                "{text}"
            );
        }
        let refusal = String::from_utf8_lossy(&remote_path_refusal(REMOTE_PATH_ERROR)).to_string();
        assert!(refusal.starts_with("HTTP/1.1 400 "), "{refusal}");
    }
}
