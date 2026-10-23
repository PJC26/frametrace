//! HTTP request parsing, loopback/token auth gating, and response/static/media serving.

use super::*;
#[cfg(test)]
pub(crate) use crate::util::is_reserved_device_name;
use crate::util::{
    is_remote_or_device_path, is_verbatim_local_drive, reserved_device_refused, unc_share_form,
};

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

/// R4-B12: the whole header block must arrive within this (was 30 s): a
/// slowloris client dripping one byte at a time holds a connection thread
/// for at most this long.
pub(crate) const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// R5-S7: connections that may sit in the unauthenticated header phase at
/// once, server-wide. R4-B12 counted them per peer ADDRESS, but every
/// local process — the examiner's own page included — is 127.0.0.1, so
/// the per-peer cap of 16 was in effect a global cap that a burst of the
/// viewer's own parallel requests (thumbnails, records pages) could hit
/// and get 503 for. The header phase is short for a real client; only a
/// slow-drip client stays in it, for at most [`HEADER_READ_TIMEOUT`].
pub(crate) const MAX_PREAUTH: usize = 48;

/// R5-S7: authenticated requests in flight at once — a separate pool, so
/// unauthenticated connections (up to [`MAX_PREAUTH`]) can never use up
/// the slots of requests that already proved the token / nonce.
pub(crate) const MAX_AUTHED: usize = 64;

/// R5-S7: the two connection pools (lock-free counters).
#[derive(Default, Debug)]
pub(crate) struct ConnLimits {
    pub(crate) preauth: std::sync::atomic::AtomicUsize,
    pub(crate) authed: std::sync::atomic::AtomicUsize,
}

/// One taken slot of a [`ConnLimits`] pool; released on drop (also while
/// a panicking handler unwinds).
pub(crate) struct ConnSlot {
    limits: Arc<ConnLimits>,
    authed: bool,
}

impl ConnLimits {
    fn pool(&self, authed: bool) -> &std::sync::atomic::AtomicUsize {
        if authed { &self.authed } else { &self.preauth }
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.limits
            .pool(self.authed)
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Takes a slot of the pre-auth (`authed == false`) or authenticated
/// pool, or `None` when that pool is full.
pub(crate) fn take_conn_slot(limits: &Arc<ConnLimits>, authed: bool) -> Option<ConnSlot> {
    use std::sync::atomic::Ordering;
    let cap = if authed { MAX_AUTHED } else { MAX_PREAUTH };
    limits
        .pool(authed)
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            (count < cap).then_some(count + 1)
        })
        .ok()
        .map(|_| ConnSlot {
            limits: Arc::clone(limits),
            authed,
        })
}

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
#[cfg(test)]
pub(crate) fn authorize(
    request: &Request,
    token: Option<&str>,
    instance: &str,
) -> Result<bool, u16> {
    authorize_on(request, token, instance, None)
}

/// [`authorize`] for a server listening on `port` (C2: the auth cookies
/// are named per port; `None` = only the legacy names, tests).
pub(crate) fn authorize_on(
    request: &Request,
    token: Option<&str>,
    instance: &str,
    port: Option<u16>,
) -> Result<bool, u16> {
    if !request_is_localhost_trusted(request) {
        return Err(403);
    }
    match http_auth_class(&request.method, token.is_some()) {
        HttpAuthClass::Public => Ok(false),
        HttpAuthClass::Token => {
            match request_authorized_on(request, token.unwrap_or_default(), port) {
                AuthProof::Denied => Err(401),
                AuthProof::Query => Ok(true),
                AuthProof::Header | AuthProof::Cookie => Ok(false),
            }
        }
        HttpAuthClass::Nonce => {
            if request_nonce_authorized(request, instance, port) {
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

#[cfg(test)]
pub(crate) fn handle_connection_timed(
    stream: TcpStream,
    state: SharedState,
    read_timeout: Duration,
) -> Result<(), String> {
    let limits = Arc::clone(&state_lock(&state).conn_limits);
    handle_connection_with(stream, state, read_timeout, &limits, None)
}

/// The 503 a full connection pool answers with (no thread, no state lock).
pub(crate) fn refuse_busy(stream: &mut TcpStream, message: &[u8]) {
    let _ = finish(stream, &plain(503, message.to_vec()));
    // Bounded discard so the close does not RST away the 503.
    drain_unread(stream);
}

/// [`handle_connection_timed`] with the connection pools handed in: the
/// accept loop clones them once, so serving a connection that is refused
/// before the auth gate (400/408/503) never waits for the state lock.
/// `preauth` is the header-phase slot the accept loop already took
/// (`None`: take one here).
pub(crate) fn handle_connection_with(
    mut stream: TcpStream,
    state: SharedState,
    read_timeout: Duration,
    limits: &Arc<ConnLimits>,
    preauth: Option<ConnSlot>,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|err| format!("read timeout: {err}"))?;
    // A peer that stops reading must not pin a worker thread forever.
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .map_err(|err| format!("write timeout: {err}"))?;
    HEAD_ONLY.with(|flag| flag.set(false));
    // R5-S7: bounded unauthenticated connections server-wide; the slot is
    // held through the header phase and the auth gate only.
    let preauth = match preauth.or_else(|| take_conn_slot(limits, false)) {
        Some(slot) => slot,
        None => {
            refuse_busy(
                &mut stream,
                b"too many unauthenticated connections - retry shortly",
            );
            return Err("pre-auth connection cap reached".into());
        }
    };
    let refuse_read = |stream: &mut TcpStream, err: HttpReadError| {
        if err.status != 0 {
            let _ = finish(stream, &plain(err.status, err.message.clone().into_bytes()));
            if matches!(err.status, 400 | 411 | 413 | 431) {
                drain_unread(stream);
            }
        }
        Err(err.message)
    };
    let (head, pending_body) = match read_request_head(&mut stream, read_timeout) {
        Ok(read) => read,
        Err(err) => return refuse_read(&mut stream, err),
    };
    let ParsedHead {
        mut request,
        extras,
        ..
    } = head;
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
    // C2: cookies are scoped by host, not by port, so the auth cookies are
    // named after the port this server listens on.
    let port = stream.local_addr().ok().map(|addr| addr.port());
    // R4-B2: `GET /?launch=<code>` is its own gate — a valid one-time code
    // plants the auth cookie and redirects to `/`.
    if request.method == "GET"
        && request.path == "/"
        && let Some(code) = query_value(&request.query, "launch")
    {
        drop(preauth);
        let mut response = launch_response(&request, &state, &code, port);
        if is_head && let Some(end) = response.windows(4).position(|w| w == b"\r\n\r\n") {
            response.truncate(end + 4);
        }
        let result = finish(&mut stream, &response);
        if pending_body.has_unread() {
            drain_unread(&mut stream);
        }
        return result;
    }
    // S3: Origin/Host, token and nonce live in the header block, so the
    // gate runs before a single body byte is read — an unauthenticated
    // client can no longer make the server buffer a 16 MiB body first.
    let authorized = authorize_on(&request, token.as_deref(), &instance, port);
    // R5-S7: an authorized request moves to its own pool BEFORE leaving
    // the header-phase one, so a connection is always counted somewhere.
    let _authed = if authorized.is_ok() {
        match take_conn_slot(limits, true) {
            Some(slot) => Some(slot),
            None => {
                drop(preauth);
                refuse_busy(&mut stream, b"server busy - too many requests in flight");
                return Err("authenticated connection cap reached".into());
            }
        }
    } else {
        None
    };
    drop(preauth);
    let plant_cookie = match authorized {
        Ok(plant) => plant,
        Err(code) => {
            let message: &[u8] = if code == 403 {
                b"cross-origin request rejected"
            } else {
                b"unauthorized"
            };
            let result = finish(&mut stream, &plain(code, message.to_vec()));
            if pending_body.has_unread() {
                // Discard (never buffer) what the client keeps sending, so
                // closing does not RST away the refusal it has not read.
                drain_unread(&mut stream);
            }
            return result;
        }
    };
    request.body = match read_request_body(&mut stream, pending_body) {
        Ok(body) => body,
        Err(err) => return refuse_read(&mut stream, err),
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
        set_token_cookie(&mut response, token, port);
    }
    // GETs under the no-token model also plant the instance nonce so the
    // just-served page can satisfy the mutating-request gate.
    if token.is_none() && request.method == "GET" && !instance.is_empty() {
        set_nonce_cookie(&mut response, &instance, port);
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

/// The not-yet-read part of a request body after [`read_request_head`]:
/// bytes that arrived with the header plus how many are still expected.
pub(crate) struct PendingBody {
    leftover: Vec<u8>,
    content_length: usize,
    started: std::time::Instant,
    body_deadline: Duration,
}

impl PendingBody {
    /// Whether body bytes are still in flight on the socket.
    pub(crate) fn has_unread(&self) -> bool {
        self.leftover.len() < self.content_length
    }
}

/// Reads one request's header block. `header_deadline` bounds the whole
/// header phase (a slowloris client dripping a byte at a time resets the
/// per-read socket timeout forever, so a wall-clock deadline is also
/// enforced); the body ([`read_request_body`]) gets 4x that budget. S3:
/// the auth gate runs between the two, on the header alone.
pub(crate) fn read_request_head(
    stream: &mut TcpStream,
    header_deadline: Duration,
) -> Result<(ParsedHead, PendingBody), HttpReadError> {
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
    let pending = PendingBody {
        leftover: buffer[header_end + 4..].to_vec(),
        content_length,
        started,
        body_deadline: header_deadline * 4,
    };
    Ok((parsed, pending))
}

/// Body phase after [`read_request_head`]: reads up to Content-Length within
/// the body budget (4x the header deadline, measured from the first byte).
pub(crate) fn read_request_body(
    stream: &mut TcpStream,
    pending: PendingBody,
) -> Result<String, HttpReadError> {
    let PendingBody {
        mut leftover,
        content_length,
        started,
        body_deadline,
    } = pending;
    let mut chunk = [0u8; 2048];
    while leftover.len() < content_length {
        if started.elapsed() > body_deadline {
            return Err(read_error(408, "request body timed out"));
        }
        let read = stream.read(&mut chunk).map_err(|err| io_read_error(&err))?;
        if read == 0 {
            break;
        }
        leftover.extend_from_slice(&chunk[..read]);
    }
    leftover.truncate(content_length);
    Ok(String::from_utf8_lossy(&leftover).to_string())
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
///
/// S8: only Windows gives two leading separators the UNC meaning; on
/// Linux/macOS `//mnt/x` is an ordinary local path, so a path that starts
/// with `/` is never remote there.
///
/// S2: on Windows a component that names a reserved DOS device (`CON`,
/// `NUL`, `COM1`, `LPT1`, … — also with an extension, `nul.txt`) is
/// refused too: `C:\x\COM1` opens the serial port, and a `metadata` /
/// `read` on it can block a handler thread for good.
pub(crate) fn reject_remote_path(
    raw: &str,
    approved_roots: &[PathBuf],
) -> Result<(), &'static str> {
    reject_remote_path_on(raw, approved_roots, cfg!(windows))
}

/// [`reject_remote_path`] with the platform made explicit (`windows`
/// selects Windows path semantics) so both are tested on every host.
pub(crate) fn reject_remote_path_on(
    raw: &str,
    approved_roots: &[PathBuf],
    windows: bool,
) -> Result<(), &'static str> {
    let text = raw.trim();
    if windows && reserved_device_refused(text) {
        return Err(RESERVED_DEVICE_ERROR);
    }
    if !is_remote_or_device_path(text, windows) {
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
    reject_device_path_on(raw, cfg!(windows))
}

/// [`reject_device_path`] with explicit platform semantics (tests).
pub(crate) fn reject_device_path_on(raw: &str, windows: bool) -> Result<(), &'static str> {
    let text = raw.trim();
    if windows && reserved_device_refused(text) {
        return Err(RESERVED_DEVICE_ERROR);
    }
    if is_remote_or_device_path(text, windows)
        && let Some(unc) = unc_share_form(text)
    {
        if unc.split('\\').any(|part| part == ".." || part == ".") {
            return Err(REMOTE_PATH_ERROR);
        }
        return Ok(());
    }
    reject_remote_path_on(text, &[], windows)
}

/// Refusal text for a path naming a reserved Windows device (S2).
pub(crate) const RESERVED_DEVICE_ERROR: &str = "Windows 예약 장치 이름(CON·PRN·AUX·NUL·COM0–9·LPT0–9 등)이 들어간 경로는 허용되지 않습니다. / Paths naming a reserved Windows device are refused.";

/// Routes whose path fields may name a UNC share the examiner typed (see
/// the policy on [`reject_remote_path`]). Only same-origin, nonce-gated
/// POSTs qualify — never a GET.
pub(crate) fn route_admits_typed_unc(method: &str, path: &str) -> bool {
    matches!(
        (method, path),
        ("POST", "/api/preflight") | ("POST", "/api/start")
    )
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
        ("POST", "/api/mkdir") => &[(false, "parent")],
        ("POST", "/api/preflight" | "/api/start") => &[(false, "source_path"), (false, "case_dir")],
        ("POST", "/api/export-clip" | "/api/telemetry" | "/api/proxy") => &[(false, "path")],
        ("POST", "/api/advanced") => &[(false, "extra")],
        // Join and sound measurement take indexed ids only — never a path
        // (the CLI resolves sources from the case index).
        ("POST", "/api/join-check" | "/api/join" | "/api/audio-measure") => &[],
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

/// C2: name of an auth cookie (`ft_token` / `ft_nonce`) for the server on
/// `port`. Browsers scope cookies by host only — a `Path=/` cookie set by
/// 127.0.0.1:8478 replaces the one 127.0.0.1:8477 planted — so two
/// workstation instances side by side overwrote each other's proof and
/// answered each other's pages with intermittent 401s. Each server plants
/// `ft_token_<port>` / `ft_nonce_<port>`. `None` (no listening port known,
/// tests) yields the legacy name.
pub(crate) fn auth_cookie_name(base: &str, port: Option<u16>) -> String {
    match port {
        Some(port) => format!("{base}_{port}"),
        None => base.to_string(),
    }
}

/// Whether the Cookie header carries `base` for `port` with a value equal
/// to `secret`. The per-port name is checked; the legacy unsuffixed name
/// (planted by builds before C2) is accepted for one release as a
/// fallback — only when its value validates, so another instance's stale
/// legacy cookie can never stand in for this one's.
pub(crate) fn auth_cookie_matches(
    cookie_header: Option<&str>,
    base: &str,
    port: Option<u16>,
    secret: &str,
) -> bool {
    let scoped = auth_cookie_name(base, port);
    cookie_header.is_some_and(|header| {
        header.split(';').any(|pair| {
            let mut parts = pair.trim().splitn(2, '=');
            let name = parts.next().map(str::trim).unwrap_or_default();
            (name == scoped || name == base) && parts.next().is_some_and(|v| secret_eq(v, secret))
        })
    })
}

/// Checks the optional shared-secret gate. A valid `?token=` query value
/// authenticates *and* asks the caller to plant the `ft_token_<port>`
/// cookie — page-initiated fetches (video elements, fetch() without
/// headers) can only carry cookies, not custom headers.
#[cfg(test)]
pub(crate) fn request_authorized(request: &Request, token: &str) -> AuthProof {
    request_authorized_on(request, token, None)
}

/// [`request_authorized`] for the server on `port` (C2 cookie names).
pub(crate) fn request_authorized_on(
    request: &Request,
    token: &str,
    port: Option<u16>,
) -> AuthProof {
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
    if auth_cookie_matches(request.cookie.as_deref(), "ft_token", port, token) {
        AuthProof::Cookie
    } else {
        AuthProof::Denied
    }
}

/// Inserts the `ft_token_<port>` cookie into an already-rendered response,
/// right after the status line.
pub(crate) fn set_token_cookie(response: &mut Vec<u8>, token: &str, port: Option<u16>) {
    let name = auth_cookie_name("ft_token", port);
    let header = format!("Set-Cookie: {name}={token}; HttpOnly; SameSite=Strict; Path=/\r\n");
    if let Some(pos) = response.windows(2).position(|w| w == b"\r\n") {
        response.splice(pos + 2..pos + 2, header.into_bytes());
    }
}

/// The `/` redirect target of a launch request: the query without its
/// `launch` and `token` parameters (raw, still percent-encoded, order
/// kept). A parameter holding a control character or a space is dropped
/// rather than echoed into a header.
pub(crate) fn launch_redirect_target(query: &str) -> String {
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let name = pair.split_once('=').map_or(*pair, |(name, _)| name);
            name != "launch" && name != "token"
        })
        .filter(|pair| !pair.chars().any(|c| c.is_control() || c == ' '))
        .collect();
    if kept.is_empty() {
        "/".to_string()
    } else {
        format!("/?{}", kept.join("&"))
    }
}

/// R6-S3: the `200` page a launch request answers with: it replaces itself
/// (`location.replace`, keeping the address-bar hash) with `target` — a
/// same-origin path from [`launch_redirect_target`]. `<noscript>` falls
/// back to a meta refresh, plus a plain link. `no-store`, no referrer.
pub(crate) fn launch_step_page(target: &str) -> Vec<u8> {
    let script_target =
        crate::util::script_json_value(&serde_json::Value::String(target.to_string()));
    let safe = crate::util::html_escape(target);
    let body = format!(
        "<!doctype html>\n<html lang=\"ko\"><head><meta charset=\"utf-8\">\n<meta name=\"referrer\" content=\"no-referrer\">\n<title>FrameTrace</title>\n<noscript><meta http-equiv=\"refresh\" content=\"0;url={safe}\"></noscript>\n</head><body><p>FrameTrace 워크스테이션을 여는 중… / Opening the FrameTrace workstation…</p>\n<p><a href=\"{safe}\">열리지 않으면 여기를 누르십시오 / click here if nothing happens</a></p>\n<script>location.replace({script_target} + location.hash);</script>\n</body></html>\n"
    );
    respond(
        200,
        "text/html; charset=utf-8",
        body.into_bytes(),
        "no-store",
        None,
    )
}

/// R4-B2: answer to `GET /?launch=<code>`.
/// - valid, unexpired, unused code → a same-origin step page
///   ([`launch_step_page`], R6-S3) to `/` with the `ft_token_<port>`
///   cookie (the code is consumed: single use);
/// - otherwise, a request that already proves the token (cookie/header)
///   is simply sent on to `/`;
/// - otherwise `401` with an explanation (expired or reused link).
///
/// Without a configured token there is nothing to plant: redirect.
pub(crate) fn launch_response(
    request: &Request,
    state: &SharedState,
    code: &str,
    port: Option<u16>,
) -> Vec<u8> {
    if !request_is_localhost_trusted(request) {
        return plain(403, b"cross-origin request rejected".to_vec());
    }
    // R5 (contract with U14): the redirect drops `launch` (and any
    // `token`) but keeps every other query parameter, so a launch URL such
    // as `/?launch=…&case=…` still lands where it pointed.
    let target = launch_redirect_target(&request.query);
    // R6-S3: a same-origin HTML step instead of `303`. A launch URL opened
    // from a `file://` launch page (Linux/macOS) or another site is a
    // cross-site navigation, and a redirect keeps that chain cross-site:
    // the browser stored the `SameSite=Strict` token cookie but did not
    // send it with the redirected `GET /` → 401. A navigation the
    // workstation's own page starts is same-site, so the cookie goes.
    let redirect = || launch_step_page(&target);
    let (token, valid) = {
        let mut guard = state_lock(state);
        let valid = super::consume_launch_code(&mut guard, code.trim(), std::time::Instant::now());
        (guard.token.clone(), valid)
    };
    let Some(token) = token else {
        return redirect();
    };
    if valid {
        let mut response = redirect();
        set_token_cookie(&mut response, &token, port);
        return response;
    }
    if request_authorized_on(request, &token, port) != AuthProof::Denied {
        return redirect();
    }
    respond(
        401,
        "text/html; charset=utf-8",
        "<!doctype html><meta charset=\"utf-8\"><title>FrameTrace</title><p>실행 링크가 만료되었거나 이미 사용되었습니다. FrameTrace를 다시 실행하면 새 링크로 열립니다.</p><p>This launch link expired or was already used; start FrameTrace again.</p>"
            .as_bytes()
            .to_vec(),
        "no-store",
        None,
    )
}

/// Checks the instance-nonce proof for mutating requests in the no-token
/// model. An empty configured nonce can never authenticate (entropy
/// failure is fail-closed, matching the instance-file check).
pub(crate) fn request_nonce_authorized(request: &Request, nonce: &str, port: Option<u16>) -> bool {
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
    auth_cookie_matches(request.cookie.as_deref(), "ft_nonce", port, nonce)
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

/// Inserts the `ft_nonce_<port>` cookie so the served page's same-origin
/// fetches carry the instance proof automatically. SameSite=Strict keeps
/// it off cross-site form POSTs — the request then fails the nonce gate.
pub(crate) fn set_nonce_cookie(response: &mut Vec<u8>, nonce: &str, port: Option<u16>) {
    let name = auth_cookie_name("ft_nonce", port);
    let header = format!("Set-Cookie: {name}={nonce}; HttpOnly; SameSite=Strict; Path=/\r\n");
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
    let mut total = meta.len();
    // R5-S2: a script under the case folder runs in the workstation origin
    // when a trusted page loads it (`review/data-bundle.js`), so its bytes
    // must be pinned in the same HMAC'd manifest as the pages — an edited
    // or planted script is refused outright (a page's hash says nothing
    // about the sibling file it loads). R4-B5: a page at a generated
    // location is trusted only when the manifest lists its exact bytes;
    // an unlisted one renders sandboxed (no script).
    // R6-S7 / R7-S2: what is served under the trusted answer is exactly
    // what was verified — see [`verify_for_serving`].
    let is_script = is_script_path(&path);
    let is_page = !is_script && is_generated_page(&case_dir, &path);
    let head_only = HEAD_ONLY.with(std::cell::Cell::get);
    let mut verified_stream: Option<VerifiedStream> = None;
    let mut memory_body: Option<Vec<u8>> = None;
    let mut safety = content_safety_headers(&path, false, false);
    if is_script || is_page {
        match verify_for_serving(&case_dir, &path, total, is_page, head_only) {
            ServeVerdict::Trusted { script_src, body } => {
                if is_page {
                    safety = trusted_page_headers(script_src.as_deref());
                }
                match body {
                    VerifiedBody::Memory(bytes) => {
                        total = bytes.len() as u64;
                        memory_body = Some(bytes);
                    }
                    VerifiedBody::Stream(verified) => verified_stream = Some(verified),
                    VerifiedBody::HeadOnly => {}
                }
            }
            ServeVerdict::Untrusted | ServeVerdict::Unknown if is_script => {
                return write_simple(
                    stream,
                    403,
                    b"script not verified - regenerate the review pages on this computer",
                );
            }
            ServeVerdict::Untrusted | ServeVerdict::Unknown => {
                safety = UNVERIFIED_PAGE_HEADERS.to_string();
            }
        }
    }
    let mut file: Box<dyn Read> = match (memory_body, verified_stream.is_some()) {
        (Some(bytes), _) => Box::new(std::io::Cursor::new(bytes)),
        (None, true) => Box::new(std::io::empty()),
        (None, false) => match std::fs::File::open(&path) {
            Ok(file) => Box::new(file),
            Err(_) => return write_simple(stream, 404, b"file not found"),
        },
    };
    if mime_for(&path).starts_with("text/html") {
        let frame = html_frame_headers(&safety);
        safety.push_str(&frame);
    }
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
    match verified_stream {
        Some(verified) => stream_verified(stream, &path, verified),
        None => stream_body(stream, &mut file, total),
    }
}

/// R7-S2: files up to this size are verified by reading them into memory
/// and served from those bytes (the R6-S7 rule); larger ones are hashed
/// in a streaming pass and streamed (see [`verify_for_serving`]), so a
/// request never holds more than this much of a file in memory.
pub(crate) const IN_MEMORY_VERIFY_MAX: u64 = 4 * 1024 * 1024;

/// R7-S2: what [`verify_for_serving`] decided.
pub(crate) enum ServeVerdict {
    /// The manifest lists the bytes; `script_src` for a page's CSP.
    Trusted {
        script_src: Option<String>,
        body: VerifiedBody,
    },
    /// Not listed, unreadable, too large, or changed while hashed.
    Untrusted,
    /// HEAD without a cached verdict: nothing was read.
    Unknown,
}

/// R7-S2: how the verified bytes are delivered.
pub(crate) enum VerifiedBody {
    /// Read once into memory and served as read (small files).
    Memory(Vec<u8>),
    /// Streamed from the verified handle by [`stream_verified`]: each
    /// [`VERIFY_CHUNK`] is re-read and compared with the hashing pass's
    /// hash of that chunk BEFORE it is sent (R9-S1; no whole-file re-hash
    /// at the end any more), the last chunk is held back until length,
    /// end of file and identity still match, and the whole transfer has a
    /// deadline (R10-S9).
    Stream(VerifiedStream),
    /// HEAD: no body.
    HeadOnly,
}

/// R7-S2: an open, verified file to stream with [`stream_verified`].
pub(crate) struct VerifiedStream {
    file: std::fs::File,
    len: u64,
    /// R9-S1: SHA-256 of every [`VERIFY_CHUNK`]-byte chunk the hashing
    /// pass read (the last one may be shorter), in file order. At most
    /// [`MAX_VERIFY_CHUNKS`] entries (32 bytes each: 256 KiB for a
    /// 512 MiB file).
    chunks: Vec<[u8; 32]>,
    identity: FileIdentity,
}

/// R9-S1: the unit both passes read, hash and compare.
pub(crate) const VERIFY_CHUNK: usize = 64 * 1024;

/// R9-S1: most chunks a verified file can have
/// (`VERIFIED_SERVE_MAX_BYTES / VERIFY_CHUNK` = 8192).
pub(crate) const MAX_VERIFY_CHUNKS: usize =
    (VERIFIED_SERVE_MAX_BYTES as usize).div_ceil(VERIFY_CHUNK);

/// R9-S1: fills `buf` from `file` until it is full or the file ends
/// (`Interrupted` retried) - both passes cut the file at the same
/// [`VERIFY_CHUNK`] boundaries whatever sizes the OS reads return.
fn read_chunk(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

/// R7-S2: a file's identity as far as the platform tells it — size,
/// modification time, file id (volume + index / device + inode) and
/// change time (ctime / NTFS `ChangeTime`, updated by every write).
///
/// R8-S2: NOT proof that the bytes are unchanged — NTFS `ChangeTime` is
/// settable with `SetFileInformationByHandle(FileBasicInfo)`, so a
/// same-size rewrite can restore the whole identity. It only detects a
/// change DURING one verification or transfer, and answers HEAD (no body)
/// from the last GET's verdict; content is hashed before every GET.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    len: u64,
    modified_nanos: Option<u128>,
    file_id: Option<(u64, u64)>,
    changed: Option<i128>,
}

/// R7-S2: [`FileIdentity`] of an open file (`None` when its metadata
/// cannot be read).
pub(crate) fn file_identity(file: &std::fs::File) -> Option<FileIdentity> {
    let meta = file.metadata().ok()?;
    let modified_nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos());
    #[cfg(unix)]
    let (file_id, changed) = {
        use std::os::unix::fs::MetadataExt;
        (
            Some((meta.dev(), meta.ino())),
            Some(i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec())),
        )
    };
    #[cfg(windows)]
    let (file_id, changed) = windows_file_identity(file);
    #[cfg(not(any(unix, windows)))]
    let (file_id, changed) = (None, None);
    Some(FileIdentity {
        len: meta.len(),
        modified_nanos,
        file_id,
        changed,
    })
}

/// Volume serial + file index and the NTFS change time of an open file.
#[cfg(windows)]
fn windows_file_identity(file: &std::fs::File) -> (Option<(u64, u64)>, Option<i128>) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx,
    };
    let handle = HANDLE(file.as_raw_handle());
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `handle` is the open file's handle (borrowed for the call);
    // `info` is a properly sized out-structure.
    let id = unsafe { GetFileInformationByHandle(handle, &mut info) }
        .ok()
        .map(|()| {
            (
                u64::from(info.dwVolumeSerialNumber),
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            )
        });
    let mut basic = FILE_BASIC_INFO::default();
    // SAFETY: as above; the buffer size is the structure's size.
    let changed = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    }
    .ok()
    .map(|()| i128::from(basic.ChangeTime));
    (id, changed)
}

/// R7-S2: a verification result for one file identity.
#[derive(Clone)]
struct CachedVerdict {
    identity: FileIdentity,
    sha: String,
    script_src: Option<String>,
}

/// Most cached verdicts kept (the map is cleared when it is full).
const VERIFY_CACHE_MAX: usize = 512;

/// R7-S2: verdicts by canonical path. Its own lock — never `state_lock`
/// — and held only for the map operation, never during disk work.
fn verify_cache() -> &'static Mutex<std::collections::HashMap<PathBuf, CachedVerdict>> {
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<PathBuf, CachedVerdict>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn cache_lookup(path: &Path, identity: &FileIdentity) -> Option<CachedVerdict> {
    let cache = verify_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
        .get(path)
        .filter(|entry| entry.identity == *identity)
        .cloned()
}

fn cache_store(path: &Path, entry: CachedVerdict) {
    let mut cache = verify_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() >= VERIFY_CACHE_MAX && !cache.contains_key(path) {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), entry);
}

fn cache_forget(path: &Path) {
    verify_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(path);
}

/// R7-S2 test hook: content bytes read for verification per path (hash
/// passes and in-memory reads; not the serving pass).
#[cfg(test)]
pub(crate) fn verify_read_bytes(path: &Path) -> u64 {
    verify_reads()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
fn verify_reads() -> &'static Mutex<std::collections::HashMap<PathBuf, u64>> {
    static READS: std::sync::OnceLock<Mutex<std::collections::HashMap<PathBuf, u64>>> =
        std::sync::OnceLock::new();
    READS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn note_verify_read(_path: &Path, _bytes: usize) {
    #[cfg(test)]
    {
        *verify_reads()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(_path.to_path_buf())
            .or_default() += _bytes as u64;
    }
}

/// R7-S2: decides whether a script or generated page (`page`) of `len`
/// bytes (the size from its metadata) may be served under the trusted
/// answer, and how.
///
/// R6-S7 read every such file whole (up to 512 MiB) into memory and hashed
/// it on EVERY request, HEAD included. Now:
/// - `len` above [`VERIFIED_SERVE_MAX_BYTES`] is refused before anything
///   is read;
/// - files up to [`IN_MEMORY_VERIFY_MAX`] are read once, hashed and served
///   from those bytes (unchanged rule);
/// - larger files are hashed in a streaming pass on EVERY GET (R8-S2: R7
///   skipped the pass when the cached [`FileIdentity`] matched, and that
///   identity can be restored after a rewrite); the result is cached for
///   HEAD only. The pass also keeps the SHA-256 of each 64 KiB chunk
///   (R9-S1). The body is then streamed from the same handle, every chunk
///   compared with its first-pass hash BEFORE it is sent (the connection
///   is cut at the first changed chunk, so no changed byte goes out), and
///   the last chunk is sent only after the length and identity still
///   match ([`stream_verified`]), else the connection is cut (the client
///   never gets a complete response). Streamed pages always get a
///   hash-list `script-src`
///   ([`crate::util::generated_pages::InlineScriptScan`]);
/// - HEAD reads no content: a cached verdict answers, otherwise
///   [`ServeVerdict::Unknown`] (scripts 403, pages sandboxed).
pub(crate) fn verify_for_serving(
    case_dir: &Path,
    path: &Path,
    len: u64,
    page: bool,
    head_only: bool,
) -> ServeVerdict {
    if len > VERIFIED_SERVE_MAX_BYTES {
        return ServeVerdict::Untrusted;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return ServeVerdict::Untrusted;
    };
    let Some(identity) = file_identity(&file).filter(|identity| identity.len == len) else {
        return ServeVerdict::Untrusted;
    };
    if head_only {
        // Headers only: no content byte leaves, so the cached verdict of
        // the last GET may answer without reading the file.
        return match cache_lookup(path, &identity) {
            Some(entry) if trusted_sha(case_dir, &entry.sha) => ServeVerdict::Trusted {
                script_src: entry.script_src,
                body: VerifiedBody::HeadOnly,
            },
            Some(_) => ServeVerdict::Untrusted,
            None => ServeVerdict::Unknown,
        };
    }
    if len <= IN_MEMORY_VERIFY_MAX {
        let mut bytes = Vec::with_capacity(len as usize);
        if (&file)
            .take(IN_MEMORY_VERIFY_MAX + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > IN_MEMORY_VERIFY_MAX
        {
            return ServeVerdict::Untrusted;
        }
        note_verify_read(path, bytes.len());
        let sha = sha256_hex(&bytes);
        if !trusted_sha(case_dir, &sha) {
            return ServeVerdict::Untrusted;
        }
        let script_src = if page {
            crate::util::generated_pages::inline_script_sources(&String::from_utf8_lossy(&bytes))
        } else {
            None
        };
        if bytes.len() as u64 == len && file_identity(&file).as_ref() == Some(&identity) {
            cache_store(
                path,
                CachedVerdict {
                    identity,
                    sha,
                    script_src: script_src.clone(),
                },
            );
        }
        return ServeVerdict::Trusted {
            script_src,
            body: VerifiedBody::Memory(bytes),
        };
    }
    // R8-S2: a GET always hashes the file before its first byte is sent —
    // the cached verdict is never a shortcut for content. Its identity
    // (size, mtime, file id, change time) can be forged: NTFS `ChangeTime`
    // is settable through `SetFileInformationByHandle`, so a same-size
    // rewrite with every timestamp restored matched the cache, and the
    // changed bytes went out (and rendered) before the re-hash at the end
    // of the transfer noticed.
    let Some(HashedFile {
        sha,
        script_src,
        read,
        chunks,
    }) = hash_file_streaming(path, &mut file, page)
    else {
        return ServeVerdict::Untrusted;
    };
    if read != len || file_identity(&file).as_ref() != Some(&identity) {
        // Changed while it was hashed: nothing to vouch for.
        cache_forget(path);
        return ServeVerdict::Untrusted;
    }
    cache_store(
        path,
        CachedVerdict {
            identity: identity.clone(),
            sha: sha.clone(),
            script_src: script_src.clone(),
        },
    );
    if !trusted_sha(case_dir, &sha) {
        return ServeVerdict::Untrusted;
    }
    if file.seek(std::io::SeekFrom::Start(0)).is_err() {
        return ServeVerdict::Untrusted;
    }
    ServeVerdict::Trusted {
        // A streamed page always gets a hash list (never "no script-src").
        script_src: if page {
            script_src.or_else(|| Some("'self'".to_string()))
        } else {
            script_src
        },
        body: VerifiedBody::Stream(VerifiedStream {
            file,
            len,
            chunks,
            identity,
        }),
    }
}

/// Whether the case's verified manifest lists `sha`.
fn trusted_sha(case_dir: &Path, sha: &str) -> bool {
    crate::util::generated_pages::trusted_hashes(case_dir)
        .iter()
        .any(|known| known == sha)
}

/// R7-S2 / R9-S1: what the hashing pass of [`verify_for_serving`] saw.
struct HashedFile {
    /// SHA-256 (hex) of the whole file, as the manifest lists it.
    sha: String,
    /// The streamed page's `script-src` (pages only).
    script_src: Option<String>,
    /// Bytes read.
    read: u64,
    /// R9-S1: SHA-256 of each [`VERIFY_CHUNK`]-byte chunk, in order.
    chunks: Vec<[u8; 32]>,
}

/// R7-S2: SHA-256 (hex) of `file` read to its end in [`VERIFY_CHUNK`]
/// chunks (at most [`VERIFIED_SERVE_MAX_BYTES`] + 1 bytes), the streamed
/// page's `script-src` when `page`, and the byte count read. R9-S1: also
/// the SHA-256 of each chunk, so the serving pass can compare every chunk
/// BEFORE sending it ([`stream_verified`]); bounded by
/// [`MAX_VERIFY_CHUNKS`] (a longer file is refused). `path` names the file
/// for the test hook only.
fn hash_file_streaming(path: &Path, file: &mut std::fs::File, page: bool) -> Option<HashedFile> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    let mut scan = page.then(crate::util::generated_pages::InlineScriptScan::new);
    let mut chunk = vec![0u8; VERIFY_CHUNK];
    let mut chunks: Vec<[u8; 32]> = Vec::new();
    let mut read_total = 0u64;
    loop {
        let read = read_chunk(file, &mut chunk).ok()?;
        if read == 0 {
            break;
        }
        note_verify_read(path, read);
        read_total += read as u64;
        if read_total > VERIFIED_SERVE_MAX_BYTES || chunks.len() >= MAX_VERIFY_CHUNKS {
            return None;
        }
        hasher.update(&chunk[..read]);
        chunks.push(sha256_bytes(&chunk[..read]));
        if let Some(scan) = scan.as_mut() {
            scan.feed(&chunk[..read]);
        }
        if read < chunk.len() {
            break;
        }
    }
    let sha = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(HashedFile {
        sha,
        script_src: scan.map(|scan| scan.finish()),
        read: read_total,
        chunks,
    })
}

/// R7-S2: streams a verified file. R9-S1: every [`VERIFY_CHUNK`]-byte
/// chunk is read, hashed and compared with the hashing pass's hash of the
/// same chunk BEFORE any of its bytes is sent; at the first mismatch the
/// connection is cut, so not one changed byte reaches the client (R8 sent
/// every chunk but the last and compared only the whole-file hash at the
/// end, so a page rewritten between or during the passes had its changed
/// middle delivered - rendered, if not scripted). One chunk is still held
/// back; it goes out only once every byte has been read and the length,
/// the end of file and the file's identity still match. Otherwise the
/// cached verdict is dropped and the connection is cut short of its
/// `Content-Length` - the client never receives a complete response of
/// bytes that were not verified (and a script that did not arrive whole
/// does not run).
pub(crate) fn stream_verified(
    stream: &mut TcpStream,
    path: &Path,
    mut verified: VerifiedStream,
) -> Result<(), String> {
    if HEAD_ONLY.with(std::cell::Cell::get) {
        return Ok(());
    }
    let cut = |stream: &mut TcpStream, why: &str| -> Result<(), String> {
        cache_forget(path);
        let _ = stream.shutdown(std::net::Shutdown::Both);
        Err(format!(
            "{} changed while it was served ({why}) - transfer cut before the changed bytes",
            path.display()
        ))
    };
    // R10-S9: the open handle pins the file (on Windows nothing can
    // rename or replace it meanwhile — a regenerated review bundle, an
    // `_INCOMPLETE` package rename). A client that keeps reading just
    // enough to beat the 60 s per-write timeout held it for hours; the
    // whole transfer now has a deadline from its size, and each write may
    // block only until then.
    let deadline = std::time::Instant::now() + stream_budget(verified.len);
    let send = |stream: &mut TcpStream, bytes: &[u8]| -> Result<(), String> {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(format!(
                "{} was not taken by the client in time - transfer cut (slow or stalled client)",
                path.display()
            ));
        }
        let _ = stream.set_write_timeout(Some(left.min(STREAM_WRITE_STALL)));
        stream
            .write_all(bytes)
            .map_err(|err| format!("file write failed: {err}"))
    };
    let mut held: Vec<u8> = Vec::with_capacity(VERIFY_CHUNK);
    let mut chunk = vec![0u8; VERIFY_CHUNK];
    let mut read_total = 0u64;
    let mut index = 0usize;
    while read_total < verified.len {
        let want = (verified.len - read_total).min(VERIFY_CHUNK as u64) as usize;
        let read = read_chunk(&mut verified.file, &mut chunk[..want])
            .map_err(|err| format!("file read failed: {err}"))?;
        if read == 0 {
            break;
        }
        // R9-S1: compared before a byte of it leaves.
        if verified.chunks.get(index) != Some(&sha256_bytes(&chunk[..read])) {
            return cut(stream, &format!("chunk {index} differs"));
        }
        index += 1;
        if !held.is_empty() {
            send(stream, &held)?;
        }
        held.clear();
        held.extend_from_slice(&chunk[..read]);
        read_total += read as u64;
        #[cfg(test)]
        STREAM_CHUNK_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(index);
            }
        });
    }
    let mut probe = [0u8; 1];
    let at_end = matches!(verified.file.read(&mut probe), Ok(0));
    if read_total != verified.len
        || index != verified.chunks.len()
        || !at_end
        || file_identity(&verified.file).as_ref() != Some(&verified.identity)
    {
        return cut(stream, "length or identity");
    }
    send(stream, &held)?;
    stream
        .flush()
        .map_err(|err| format!("file write failed: {err}"))
}

/// R10-S9: longest one write of a verified stream may block (a stalled
/// client), within the transfer deadline.
const STREAM_WRITE_STALL: std::time::Duration = std::time::Duration::from_secs(20);

/// R10-S9: the slowest average rate a verified transfer may take before
/// it is cut: 256 KiB/s (a loopback viewer reads tens of MB/s; a 512 MiB
/// verified file then has about 35 minutes), plus [`STREAM_BASE_BUDGET`].
const STREAM_MIN_RATE: u64 = 256 * 1024;

/// R10-S9: the deadline every verified transfer gets regardless of size.
const STREAM_BASE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(test)]
thread_local! {
    /// R10-S9 test hook: a fixed transfer deadline on this thread.
    pub(crate) static STREAM_BUDGET_OVERRIDE: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

/// R10-S9: how long a verified transfer of `len` bytes may take.
fn stream_budget(len: u64) -> std::time::Duration {
    #[cfg(test)]
    if let Some(budget) = STREAM_BUDGET_OVERRIDE.with(|cell| cell.get()) {
        return budget;
    }
    STREAM_BASE_BUDGET + std::time::Duration::from_secs(len / STREAM_MIN_RATE)
}

/// R9-S1 test hook type (see [`STREAM_CHUNK_HOOK`]).
#[cfg(test)]
pub(crate) type ChunkHook = Box<dyn FnMut(usize)>;

#[cfg(test)]
thread_local! {
    /// R9-S1 test hook: called on the serving thread after each chunk of
    /// [`stream_verified`] passed its comparison (argument: chunks
    /// compared so far) - lets a test rewrite the file mid-transfer.
    pub(crate) static STREAM_CHUNK_HOOK: std::cell::RefCell<Option<ChunkHook>> =
        const { std::cell::RefCell::new(None) };
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

/// R4-B5: headers for a page at a generated location whose bytes the
/// manifest does not vouch for — rendered inline but sandboxed (opaque
/// origin, no script), so it can neither run code nor reach the APIs.
pub(crate) const UNVERIFIED_PAGE_HEADERS: &str = "Content-Security-Policy: sandbox; default-src 'none'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'unsafe-inline'; frame-ancestors 'self'\r\n";

/// R4-B5: headers for a verified generated page; `script_src` (inline
/// script hashes + `'self'`) is added when the page allows it.
pub(crate) fn trusted_page_headers(script_src: Option<&str>) -> String {
    match script_src {
        Some(sources) => format!(
            "Content-Security-Policy: frame-ancestors 'self'; object-src 'none'; base-uri 'self'; script-src {sources}\r\n"
        ),
        None => content_safety_headers(Path::new("index.html"), true, false),
    }
}

/// R6-S7: generated pages and case scripts up to this size can be trusted
/// (R7-S2: checked against the metadata size before anything is read). A
/// larger generated page is served sandboxed (never trusted); a larger
/// script is refused.
pub(crate) const VERIFIED_SERVE_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// SHA-256 (hex) of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// R5-S2: extensions a browser executes as a classic or module script.
pub(crate) fn is_script_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("js" | "mjs" | "cjs")
    )
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
    // R5-S2: evidence is never served with a script MIME type — with
    // `nosniff`, `<script src="/media?path=…evil.js">` is then refused by
    // the browser instead of running in the workstation origin.
    let media_type = if is_script_path(&canonical) {
        "application/octet-stream"
    } else {
        mime_for(&canonical)
    };
    let mut head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {media_type}\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n{REFERRER_POLICY}Connection: close\r\n",
        if code == 206 { "Partial Content" } else { "OK" },
    );
    // `?download=1` forces a browser save-as with the evidence filename
    // instead of inline playback. Same approved-roots containment applies.
    // Evidence is never trusted content: script-capable documents (html,
    // svg, xml) are always forced to download and sandboxed (SEC-02).
    let wants_download = query_value(&request.query, "download").is_some();
    let safety = content_safety_headers(&canonical, false, wants_download);
    if mime_for(&canonical).starts_with("text/html") {
        head.push_str(&html_frame_headers(&safety));
    }
    head.push_str(&safety);
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
        303 => "See Other",
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
    if content_type.starts_with("text/html") {
        head.push_str(&html_frame_headers(extra.unwrap_or_default()));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(&body);
    bytes
}

/// R4-B13: anti-framing headers every HTML response carries — a CSP
/// `frame-ancestors 'self'` (unless `existing` headers already set one)
/// and the legacy `X-Frame-Options: SAMEORIGIN` for older engines — so no
/// other site can frame a workstation page (clickjacking the finalize or
/// delete buttons).
pub(crate) fn html_frame_headers(existing: &str) -> String {
    let mut headers = String::new();
    if !existing.contains("frame-ancestors") {
        headers.push_str("Content-Security-Policy: frame-ancestors 'self'\r\n");
    }
    if !existing.contains("X-Frame-Options") {
        headers.push_str("X-Frame-Options: SAMEORIGIN\r\n");
    }
    headers
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
                reject_remote_path_on(raw, &[], true),
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
            // R21-S1: the root separator makes it the drive, not the
            // volume device (`\\?\d:` is refused — tests_r21_server).
            r"\\?\d:\",
            "",
        ] {
            assert_eq!(
                reject_remote_path_on(raw, &[], true),
                Ok(()),
                "{raw} is local"
            );
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
            assert_eq!(reject_device_path_on(raw, true), Ok(()), "{raw}");
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
            assert_eq!(
                reject_device_path_on(raw, true),
                Err(REMOTE_PATH_ERROR),
                "{raw}"
            );
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
            assert_eq!(reject_remote_path_on(raw, &roots, true), Ok(()), "{raw}");
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
                reject_remote_path_on(raw, &roots, true),
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
