//! Round-7 track S (docs/PLAN_R7_2026-10-15.md): verified serving of
//! generated pages and case scripts without whole-file reads per request.
//! Each test fails on the code before the fix (R6 read every such file
//! whole, up to 512 MiB, on every request including HEAD).

use super::http::{
    IN_MEMORY_VERIFY_MAX, ServeVerdict, VerifiedBody, stream_verified, verify_for_serving,
    verify_read_bytes,
};
use super::*;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r7s-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn temp_case(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::create_dir_all(dir.join("review")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-r7"}"#).unwrap();
    dir
}

fn bound(case: &Path) -> SharedState {
    let mut state = JobState::new();
    state.case_dir = Some(case.to_path_buf());
    state.media_roots = vec![case.to_path_buf()];
    state.phase = "review-ready";
    Arc::new(Mutex::new(state))
}

fn exchange(port: u16, raw: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    buffer
}

fn start_server(state: &SharedState) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = Arc::clone(state);
    thread::spawn(move || serve_on(listener, serving));
    port
}

fn request_raw(port: u16, method: &str, target: &str) -> Vec<u8> {
    exchange(
        port,
        &format!(
            "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        ),
    )
}

fn split_response(response: &[u8]) -> (String, Vec<u8>) {
    let end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(response.len());
    (
        String::from_utf8_lossy(&response[..end]).to_string(),
        response.get(end + 4..).unwrap_or_default().to_vec(),
    )
}

/// A script larger than the in-memory limit, recorded as generated.
fn big_script(case: &Path, name: &str, size: usize) -> (PathBuf, Vec<u8>) {
    let mut bytes = b"window.__FRAMETRACE_DATA__={pad:\"".to_vec();
    bytes.resize(size - 3, b'a');
    bytes.extend_from_slice(b"\"};");
    let path = case.join("review").join(name);
    std::fs::write(&path, &bytes).unwrap();
    crate::util::generated_pages::record_generated_contents(
        case,
        &[(&format!("review/{name}"), bytes.as_slice())],
    )
    .unwrap();
    (path.canonicalize().unwrap(), bytes)
}

/// R7-S2: a large verified script is hashed in a streaming pass (never
/// read whole into memory) and the body is exactly the verified bytes.
/// HEAD reads no content: before any GET it cannot vouch for the script
/// (403), after one it answers from the cached verdict. R6 read the whole
/// file into memory and hashed it on every request, HEAD included.
/// R8-S2: every GET hashes again (one pass per GET) — the cached identity
/// is no shortcut for content (`tests_r8_server`).
#[test]
fn r7_s2_large_script_streams_and_head_reads_nothing() {
    let case = temp_case("s2-cache");
    let size = (IN_MEMORY_VERIFY_MAX as usize) + 3 * 64 * 1024 + 17;
    let (path, bytes) = big_script(&case, "data-bundle.js", size);
    let state = bound(&case);
    let port = start_server(&state);
    let (head, _) = split_response(&request_raw(port, "HEAD", "/review/data-bundle.js"));
    assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    assert_eq!(verify_read_bytes(&path), 0, "HEAD read content");
    for round in 0..2 {
        let (head, body) = split_response(&request_raw(port, "GET", "/review/data-bundle.js"));
        assert!(head.starts_with("HTTP/1.1 200"), "{round}: {head}");
        assert!(head.contains(&format!("Content-Length: {size}")), "{head}");
        assert!(body == bytes, "round {round}: served bytes differ");
    }
    assert_eq!(
        verify_read_bytes(&path),
        2 * size as u64,
        "one hash pass per GET"
    );
    let (head, body) = split_response(&request_raw(port, "HEAD", "/review/data-bundle.js"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains(&format!("Content-Length: {size}")), "{head}");
    assert!(body.is_empty());
    assert_eq!(
        verify_read_bytes(&path),
        2 * size as u64,
        "HEAD read content"
    );
    state_lock(&state).request_shutdown();
    let _ = std::fs::remove_dir_all(case);
}

/// R7-S2: the size limit is checked on the metadata size before anything
/// is read (R6 read up to 512 MiB + 1 byte first, then refused).
#[test]
fn r7_s2_oversized_files_are_refused_before_reading() {
    let case = temp_case("s2-big");
    let script = case.join("review/huge.js");
    std::fs::File::create(&script)
        .unwrap()
        .set_len(super::http::VERIFIED_SERVE_MAX_BYTES + 1)
        .unwrap();
    let page = case.join("review/huge.html");
    std::fs::File::create(&page)
        .unwrap()
        .set_len(super::http::VERIFIED_SERVE_MAX_BYTES + 1)
        .unwrap();
    let state = bound(&case);
    let port = start_server(&state);
    let (head, _) = split_response(&request_raw(port, "GET", "/review/huge.js"));
    assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    let (head, body) = split_response(&request_raw(port, "HEAD", "/review/huge.html"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        head.contains("sandbox"),
        "an oversized page is never trusted: {head}"
    );
    assert!(body.is_empty());
    assert_eq!(verify_read_bytes(&script.canonicalize().unwrap()), 0);
    assert_eq!(verify_read_bytes(&page.canonicalize().unwrap()), 0);
    state_lock(&state).request_shutdown();
    let _ = std::fs::remove_dir_all(case);
}

/// R7-S2: a large script rewritten in place with bytes of the same length
/// and its modification time restored is not served from the cache: the
/// identity carries the file id and change time (ctime / NTFS
/// ChangeTime), so the next request hashes again and refuses it.
#[test]
fn r7_s2_same_size_swap_of_a_large_script_is_refused() {
    let case = temp_case("s2-swap");
    let size = (IN_MEMORY_VERIFY_MAX as usize) + 4096;
    let (path, bytes) = big_script(&case, "data-bundle.js", size);
    let state = bound(&case);
    let port = start_server(&state);
    let (head, _) = split_response(&request_raw(port, "GET", "/review/data-bundle.js"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut evil = bytes.clone();
    evil[..30].copy_from_slice(b"fetch('/api/finalize');//_____");
    // Make sure the change time can move (coarse clocks).
    std::thread::sleep(Duration::from_millis(50));
    {
        let mut file = std::fs::File::options().write(true).open(&path).unwrap();
        file.write_all(&evil).unwrap();
        file.set_modified(mtime).unwrap();
    }
    let (head, body) = split_response(&request_raw(port, "GET", "/review/data-bundle.js"));
    assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    assert!(!String::from_utf8_lossy(&body).contains("finalize"));
    state_lock(&state).request_shutdown();
    let _ = std::fs::remove_dir_all(case);
}

/// R7-S2: bytes changed between the verification and the end of the
/// transfer are never delivered whole: the stream re-hashes what it
/// sends, holds the last chunk back, and cuts the connection when the
/// hash no longer matches (the client gets fewer bytes than its
/// `Content-Length`); the cached verdict is dropped, so the next request
/// hashes again and refuses the file.
#[test]
fn r7_s2_bytes_changed_mid_transfer_are_cut_off() {
    let case = temp_case("s2-mid");
    let size = (IN_MEMORY_VERIFY_MAX as usize) + 5 * 64 * 1024;
    let (path, bytes) = big_script(&case, "data-bundle.js", size);
    let verdict = verify_for_serving(&case, &path, size as u64, false, false);
    let ServeVerdict::Trusted {
        body: VerifiedBody::Stream(verified),
        ..
    } = verdict
    else {
        panic!("a large listed script streams");
    };
    // Rewritten after the verification, before (and while) it is served.
    let mut evil = bytes.clone();
    let tail = evil.len() - 10;
    evil[tail..].copy_from_slice(b"alert(1);/");
    std::fs::write(&path, &evil).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let reader = thread::spawn(move || {
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut got = Vec::new();
        let _ = client.read_to_end(&mut got);
        got
    });
    let (mut server, _) = listener.accept().unwrap();
    let result = stream_verified(&mut server, &path, verified);
    drop(server);
    let got = reader.join().unwrap();
    assert!(result.is_err(), "a changed file must not complete");
    assert!(
        got.len() < size,
        "the last chunk was sent ({} bytes)",
        got.len()
    );
    assert!(!String::from_utf8_lossy(&got).contains("alert(1)"));
    // The verdict was dropped: the file is hashed again and refused.
    assert!(matches!(
        verify_for_serving(&case, &path, size as u64, false, false),
        ServeVerdict::Untrusted
    ));
    let _ = std::fs::remove_dir_all(case);
}

/// R7-S2: a large generated page is streamed under a hash-list
/// `script-src` computed in the streaming pass (R6 gave pages over 32 MiB
/// no script-src at all — any script allowed), and its bytes are served
/// exactly.
#[test]
fn r7_s2_large_page_streams_under_a_hash_list() {
    let case = temp_case("s2-page");
    let script = "console.log('ok');";
    let mut html = format!("<!doctype html><html><body><script>{script}</script><p>");
    while html.len() < IN_MEMORY_VERIFY_MAX as usize + 100_000 {
        html.push_str("padding padding padding ");
    }
    html.push_str("</p></body></html>");
    std::fs::write(case.join("review/index.html"), &html).unwrap();
    crate::util::generated_pages::record_generated_contents(
        &case,
        &[("review/index.html", html.as_bytes())],
    )
    .unwrap();
    let state = bound(&case);
    let port = start_server(&state);
    let (head, body) = split_response(&request_raw(port, "GET", "/review/index.html"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let expected = crate::util::generated_pages::inline_script_sources(&html).unwrap();
    assert!(
        head.contains(&format!("script-src {expected}")),
        "{head} / {expected}"
    );
    assert!(body == html.as_bytes());
    state_lock(&state).request_shutdown();
    let _ = std::fs::remove_dir_all(case);
}
