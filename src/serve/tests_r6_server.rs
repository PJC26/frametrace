//! Round-6 track S (docs/PLAN_R6_2026-10-15.md): server and process fixes.
//! Each test exercises the exact misbehaviour the round-5 review found and
//! fails on the code before the fix (exceptions are named in the plan's
//! "§S6 as implemented").

use super::*;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-r6s-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn temp_case(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("case.json"), br#"{"case_id":"FT-r6"}"#).unwrap();
    dir
}

fn request(method: &str, path: &str, query: &str, body: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        body: body.into(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    }
}

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("invalid JSON {err}: {text}"))
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
        .set_read_timeout(Some(Duration::from_secs(20)))
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

fn get(port: u16, target: &str) -> Vec<u8> {
    exchange(
        port,
        &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
    )
}

fn split_response(response: &[u8]) -> (String, Vec<u8>) {
    let end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(response.len());
    (
        String::from_utf8_lossy(&response[..end]).to_string(),
        response[(end + 4).min(response.len())..].to_vec(),
    )
}

// ---- S2: marks-pending cursor is per item, not per file ----

/// S2: one queued file of 800 ids paged 500 at a time. The R5 cursor was
/// the file number, so the second page (`after=<that file>`) skipped the
/// other 300 ids of the same file. The per-item cursor `"<seq>:<idx>"`
/// returns them; a bare number is still accepted (R5 viewers).
#[test]
fn s2_marks_pending_cursor_never_skips_inside_one_file() {
    let case = temp_case("s2");
    let dir = case.join("marks-pending");
    std::fs::create_dir_all(&dir).unwrap();
    let marks: Vec<serde_json::Value> = (0..800)
        .map(|n| serde_json::json!({"id": format!("vid_{n:06}"), "status": "important"}))
        .collect();
    let body = serde_json::json!({"schema_version": 1, "case_id": "FT-r6", "marks": marks});
    std::fs::write(dir.join(format!("{:032}.json", 7)), body.to_string()).unwrap();
    let later = serde_json::json!({
        "schema_version": 1, "case_id": "FT-r6",
        "marks": [{"id": "vid_900000", "status": "important"}],
    });
    std::fs::write(dir.join(format!("{:032}.json", 8)), later.to_string()).unwrap();
    let state = bound(&case);
    let page = |query: &str| {
        parse(&super::jobs::api_marks_pending(
            &request("GET", "/api/marks-pending", query, ""),
            &state,
        ))
    };
    let mut seen: Vec<String> = Vec::new();
    let mut query = "limit=500".to_string();
    let mut pages = 0;
    loop {
        let current = page(&query);
        pages += 1;
        for entry in current["entries"].as_array().unwrap() {
            seen.push(entry["id"].as_str().unwrap().to_string());
            assert!(entry["cursor"].is_string(), "{entry}");
        }
        match current["next_after"].as_str() {
            Some(next) => query = format!("after={next}&limit=500"),
            None => {
                assert!(current["next_after"].is_null(), "{current}");
                break;
            }
        }
        assert!(pages < 5, "paging does not end");
    }
    assert_eq!(pages, 2);
    assert_eq!(seen.len(), 801, "every queued id once");
    assert_eq!(seen[500], "vid_000500");
    assert_eq!(seen.last().unwrap(), "vid_900000");
    // The first page's cursor names the position inside file 7.
    assert_eq!(page("limit=500")["next_after"], "7:499");
    // The viewer percent-encodes the cursor (`encodeURIComponent`).
    let encoded = page("after=7%3A499&limit=500");
    assert_eq!(encoded["entries"].as_array().unwrap().len(), 301);
    assert_eq!(encoded["after"], "7:499");
    // An R5 numeric cursor still means "everything of file 7 seen".
    let legacy = page("after=7&limit=500");
    assert_eq!(legacy["entries"].as_array().unwrap().len(), 1);
    assert_eq!(legacy["entries"][0]["id"], "vid_900000");
    let _ = std::fs::remove_dir_all(case);
}

// ---- S3: the launch answer is a same-origin step page ----

/// S3: a launch URL opened from a `file://` launch page (Linux/macOS) is a
/// cross-site navigation; the old `303` kept the redirect chain cross-site,
/// so the browser did not send the freshly planted `SameSite=Strict`
/// cookie with `GET /` and the workstation answered 401. The answer is now
/// a 200 page of the workstation's own origin that navigates on itself
/// (keeping the other query parameters and the hash).
#[test]
fn s3_launch_answer_is_a_same_origin_step_page() {
    let token = "tok-r6-launch-0001";
    let mut state = JobState::new();
    state.token = Some(token.to_string());
    let state: SharedState = Arc::new(Mutex::new(state));
    let port = start_server(&state);
    let code = mint_launch_code(&state).unwrap();
    let (head, body) = split_response(&get(port, &format!("/?launch={code}&case=x%20y")));
    let body = String::from_utf8_lossy(&body);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(!head.contains("\r\nLocation:"), "{head}");
    assert!(
        head.contains(&format!(
            "Set-Cookie: ft_token_{port}={token}; HttpOnly; SameSite=Strict"
        )),
        "{head}"
    );
    assert!(head.contains("Cache-Control: no-store"), "{head}");
    assert!(
        body.contains("location.replace(\"/?case=x%20y\" + location.hash)"),
        "{body}"
    );
    assert!(body.contains("url=/?case=x%20y"), "{body}");
    assert!(!body.contains(token) && !body.contains(&code), "{body}");
    // A hostile query value is escaped in both the script and the markup.
    let page = String::from_utf8_lossy(&super::http::launch_step_page("/?q=</script><b>\"x\""))
        .to_string();
    assert!(!page.contains("</script><b>"), "{page}");
    state_lock(&state).request_shutdown();
}

/// S3: no home folder → no launch page (it would land in the shared temp
/// folder); `~/Documents` is used when it exists, else the home itself.
#[test]
fn s3_launch_folder_needs_a_home() {
    let none = |_: &str| None;
    assert_eq!(super::launcher_dir_from(&none, &|_| true), None);
    let home = |name: &str| (name == "HOME").then(|| std::ffi::OsString::from("/home/ex"));
    assert_eq!(
        super::launcher_dir_from(&home, &|_| true),
        Some(PathBuf::from("/home/ex/Documents/FrameTrace/launch"))
    );
    assert_eq!(
        super::launcher_dir_from(&home, &|_| false),
        Some(PathBuf::from("/home/ex/FrameTrace/launch"))
    );
}

/// S3 (unix): only the launch folder itself is made private — its parent
/// (`~/Documents/FrameTrace`, where cases live too) keeps its mode.
#[cfg(unix)]
#[test]
fn s3_launch_page_does_not_lock_down_the_parent() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch("s3-perm").join("FrameTrace");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dir = root.join("launch");
    let url = "http://127.0.0.1:8477/?launch=0123456789abcdef0123456789abcdef";
    write_launcher_file(&dir, url).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&root), 0o755, "the parent kept its mode");
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

// ---- S7: what is vouched for and served is what was written ----

/// S7: the trust list used to hash the file ON DISK after the generator
/// wrote it, so bytes swapped in between were vouched for. It now
/// records the generator's own bytes: a swapped file is not trusted.
#[test]
fn s7_trust_list_records_the_bytes_written() {
    use crate::util::generated_pages::{page_is_trusted, record_generated_contents};
    let case = temp_case("s7-record");
    std::fs::create_dir_all(case.join("review")).unwrap();
    let written = b"<html>generated</html>".as_slice();
    // Another process replaced the file before the record was made.
    std::fs::write(case.join("review/index.html"), b"<html>swapped</html>").unwrap();
    record_generated_contents(&case, &[("review/index.html", written)]).unwrap();
    assert!(!page_is_trusted(&case, "review/index.html"));
    std::fs::write(case.join("review/index.html"), written).unwrap();
    assert!(page_is_trusted(&case, "review/index.html"));
    let _ = std::fs::remove_dir_all(case);
}

/// S7: a served script was verified by a `(path, size, mtime)` cache and
/// then opened AGAIN to be streamed. Rewriting it with bytes of the same
/// length and restoring its modification time served the new bytes as
/// verified. The bytes are now read once, hashed and served as read.
#[test]
fn s7_a_same_size_swap_with_restored_mtime_is_not_served() {
    let case = temp_case("s7-serve");
    std::fs::create_dir_all(case.join("review")).unwrap();
    let bundle = case.join("review/data-bundle.js");
    let good = "window.__FRAMETRACE_DATA__={};";
    std::fs::write(&bundle, good).unwrap();
    crate::util::generated_pages::record_generated_contents(
        &case,
        &[("review/data-bundle.js", good.as_bytes())],
    )
    .unwrap();
    let mtime = std::fs::metadata(&bundle).unwrap().modified().unwrap();
    let state = bound(&case);
    let port = start_server(&state);
    let (head, body) = split_response(&get(port, "/review/data-bundle.js"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(body, good.as_bytes());
    let evil = "fetch('/api/finalize');//_____";
    assert_eq!(evil.len(), good.len());
    std::fs::write(&bundle, evil).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&bundle)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let (head, body) = split_response(&get(port, "/review/data-bundle.js"));
    assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    assert!(!String::from_utf8_lossy(&body).contains("finalize"));
    state_lock(&state).request_shutdown();
    let _ = std::fs::remove_dir_all(case);
}
