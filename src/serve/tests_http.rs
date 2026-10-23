//! HTTP-layer hardening and caching: route auth classes, path traversal by
//! component, chunked bodies, slowloris, thumbnail ETag/304, and the
//! pre-lowercased search / FTS / thumbnail-listing caches.

use super::*;

fn req(method: &str, path: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: String::new(),
        body: String::new(),
        range: None,
        origin: None,
        host: None,
        cookie: None,
        token_header: None,
        nonce_header: None,
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-http-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every `("METHOD", "/path")` arm of `route()`, read from mod.rs so a new
/// route cannot slip in without a row in [`EXPECTED_ROUTES`].
fn routes_in_mod_rs() -> Vec<(String, String)> {
    let source = include_str!("mod.rs");
    let start = source.find("pub(crate) fn route(").unwrap();
    let mut routes = Vec::new();
    for line in source[start..].lines() {
        let line = line.trim();
        for method in ["GET", "POST"] {
            let marker = format!("(\"{method}\", \"");
            if let Some(rest) = line.strip_prefix(&marker)
                && let Some(path) = rest.split('"').next()
            {
                routes.push((method.to_string(), path.to_string()));
            }
        }
    }
    routes
}

const EXPECTED_ROUTES: &[(&str, &str)] = &[
    ("GET", "/"),
    ("GET", "/api/env"),
    ("GET", "/api/browse"),
    ("GET", "/api/records"),
    ("GET", "/api/records-meta"),
    ("GET", "/api/status"),
    ("GET", "/api/mark-conflicts"),
    // R4-B6 / R4-B2.
    ("GET", "/api/marks-pending"),
    ("POST", "/api/launch-code"),
    ("POST", "/api/start"),
    ("POST", "/api/finalize"),
    ("POST", "/api/recover-deleted"),
    ("POST", "/api/export-selected"),
    ("POST", "/api/verify-audit"),
    ("POST", "/api/cancel"),
    ("POST", "/api/shutdown"),
    ("POST", "/api/open-case"),
    ("POST", "/api/import-marks"),
    ("POST", "/api/capture-frame"),
    ("POST", "/api/export-clip"),
    ("POST", "/api/telemetry"),
    ("POST", "/api/transcode-queue"),
    ("POST", "/api/proxy"),
    ("POST", "/api/advanced"),
    ("POST", "/api/carve"),
    ("POST", "/api/open-folder"),
    ("POST", "/api/verify-sources"),
    ("GET", "/api/case-info"),
    ("POST", "/api/preflight"),
    ("GET", "/api/export-clip-status"),
    ("POST", "/api/export-clip-cancel"),
    ("POST", "/api/join-check"),
    ("POST", "/api/join"),
    ("POST", "/api/audio-measure"),
    ("GET", "/api/recent-cases"),
    ("POST", "/api/recent-cases/forget"),
    // R5-S6.
    ("POST", "/api/reimport-raw"),
    ("POST", "/api/tools/install"),
    ("GET", "/api/tools/install-status"),
    ("POST", "/api/mkdir"),
    ("GET", "/api/places"),
];

#[test]
fn route_table_matches_router_and_auth_classes() {
    let mut actual = routes_in_mod_rs();
    actual.sort();
    let mut expected: Vec<(String, String)> = EXPECTED_ROUTES
        .iter()
        .map(|(m, p)| (m.to_string(), p.to_string()))
        .collect();
    expected.sort();
    assert_eq!(actual, expected, "route() and EXPECTED_ROUTES drifted");

    // File-serving paths handled before route() share the same gate.
    let mut all: Vec<(String, String)> = expected;
    for path in [
        "/media",
        "/review/x.html",
        "/case/x.json",
        "/viewer/data.js",
    ] {
        all.push(("GET".into(), path.into()));
    }
    let token = "tok-abcdefgh";
    let nonce = "0123456789abcdef0123456789abcdef";
    for (method, path) in &all {
        // Token configured: every route is Token class; nothing is public.
        assert_eq!(http_auth_class(method, true), HttpAuthClass::Token);
        assert_eq!(authorize(&req(method, path), Some(token), nonce), Err(401));
        let mut with_header = req(method, path);
        with_header.token_header = Some(token.into());
        assert_eq!(authorize(&with_header, Some(token), nonce), Ok(false));
        let mut with_query = req(method, path);
        with_query.query = format!("token={token}");
        assert_eq!(authorize(&with_query, Some(token), nonce), Ok(true));
        let mut with_cookie = req(method, path);
        with_cookie.cookie = Some(format!("ft_token={token}"));
        assert_eq!(authorize(&with_cookie, Some(token), nonce), Ok(false));
        // A nonce is not a substitute for the token.
        let mut with_nonce = req(method, path);
        with_nonce.nonce_header = Some(nonce.into());
        assert_eq!(authorize(&with_nonce, Some(token), nonce), Err(401));

        // No token: reads are public, mutations need the nonce.
        if method == "GET" {
            assert_eq!(http_auth_class(method, false), HttpAuthClass::Public);
            assert_eq!(authorize(&req(method, path), None, nonce), Ok(false));
        } else {
            assert_eq!(http_auth_class(method, false), HttpAuthClass::Nonce);
            assert_eq!(authorize(&req(method, path), None, nonce), Err(401));
            let mut ok = req(method, path);
            ok.nonce_header = Some(nonce.into());
            assert_eq!(authorize(&ok, None, nonce), Ok(false));
            let mut wrong = req(method, path);
            wrong.nonce_header = Some("nope".into());
            assert_eq!(authorize(&wrong, None, nonce), Err(401));
            // Empty configured nonce fails closed.
            assert_eq!(authorize(&ok, None, ""), Err(401));
        }
        // Rebinding hosts are refused before any proof is looked at.
        let mut rebind = req(method, path);
        rebind.host = Some("evil.example".into());
        rebind.token_header = Some(token.into());
        assert_eq!(authorize(&rebind, Some(token), nonce), Err(403));
    }
}

#[test]
fn traversal_is_checked_by_component() {
    for ok in [
        "a..b.mp4",
        "dir/a..b.mp4",
        "..hidden",
        "file..",
        "thumbs/x.jpg",
        "./x.jpg",
        "a//b",
    ] {
        assert!(relative_path_is_safe(ok), "{ok} should be allowed");
    }
    for bad in [
        "", "..", "../x", "a/../x", "a/..", "a\\..\\x", "..\\x", "/x", "\\x", "C:/x", "c:x",
        "a\0b", ".",
    ] {
        assert!(!relative_path_is_safe(bad), "{bad:?} should be refused");
    }
}

fn start_server(
    case_dir: Option<&Path>,
    token: Option<&str>,
) -> (std::net::SocketAddr, SharedState) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let mut job = JobState::new();
    job.case_dir = case_dir.map(Path::to_path_buf);
    job.token = token.map(str::to_string);
    let state: SharedState = Arc::new(Mutex::new(job));
    let serve_state = Arc::clone(&state);
    thread::spawn(move || serve_on(listener, serve_state));
    (addr, state)
}

fn raw(addr: std::net::SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).to_string()
}

fn status_line(response: &str) -> &str {
    response.lines().next().unwrap_or_default()
}

#[test]
fn http_traversal_over_the_wire() {
    let case_dir = scratch("trav");
    std::fs::create_dir_all(case_dir.join("review")).unwrap();
    std::fs::write(case_dir.join("review/a..b.mp4"), b"video-bytes").unwrap();
    std::fs::write(case_dir.join("secret.txt"), b"top-secret").unwrap();
    let (addr, _state) = start_server(Some(&case_dir), None);
    let get = |target: &str| {
        raw(
            addr,
            &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        )
    };
    let ok = get("/review/a..b.mp4");
    assert!(status_line(&ok).contains("200"), "{ok}");
    assert!(ok.ends_with("video-bytes"), "{ok}");
    for bad in [
        "/review/../secret.txt",
        "/review/%2e%2e/secret.txt",
        "/review/a/../../secret.txt",
        "/case/../secret.txt",
        "/review/..%5csecret.txt",
        "/review//secret.txt",
    ] {
        let response = get(bad);
        assert!(status_line(&response).contains("403"), "{bad}: {response}");
        assert!(!response.contains("top-secret"), "{bad}");
    }
}

#[test]
fn chunked_and_oversized_bodies_are_refused() {
    let (addr, _state) = start_server(None, None);
    let chunked = raw(
        addr,
        "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
    );
    assert!(status_line(&chunked).contains("411"), "{chunked}");
    // Content-Length + Transfer-Encoding smuggling shape: still refused.
    let both = raw(
        addr,
        "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\nabcd",
    );
    assert!(status_line(&both).contains("411"), "{both}");
    let conflicting = raw(
        addr,
        "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 4\r\nContent-Length: 5\r\n\r\nabcd",
    );
    assert!(status_line(&conflicting).contains("400"), "{conflicting}");
    let five_mib = 5 * 1024 * 1024;
    let too_big = raw(
        addr,
        &format!(
            "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {five_mib}\r\n\r\n"
        ),
    );
    assert!(status_line(&too_big).contains("413"), "{too_big}");
}

#[test]
fn capture_frame_has_a_larger_body_cap_only_for_itself() {
    assert_eq!(http_body_limit_for("/api/start"), MAX_BODY);
    assert_eq!(
        http_body_limit_for("/api/capture-frame"),
        MAX_CAPTURE_FRAME_BODY
    );
    let head = |path: &str, length: usize| {
        parse_request_head(&format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {length}\r\n"
        ))
        .map(|parsed| parsed.content_length)
        .map_err(|err| err.status)
    };
    let five_mib = 5 * 1024 * 1024;
    assert_eq!(head("/api/capture-frame", five_mib), Ok(five_mib));
    assert_eq!(head("/api/start", five_mib), Err(413));
    assert_eq!(
        head("/api/capture-frame", MAX_CAPTURE_FRAME_BODY + 1),
        Err(413)
    );
}

/// Slowloris: a client that stops (or drips) mid-header must be cut off by
/// the read timeout / wall-clock deadline rather than pinning a worker.
#[test]
fn slow_header_clients_are_timed_out() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let state = Arc::clone(&state);
            let result = handle_connection_timed(stream, state, Duration::from_millis(400));
            done_tx.send(result).unwrap();
        }
    });
    // Stalled: partial header, then silence.
    let mut stalled = TcpStream::connect(addr).unwrap();
    stalled.write_all(b"GET / HTTP/1.1\r\nHost: 127").unwrap();
    let started = std::time::Instant::now();
    let result = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(result.is_err(), "stalled client must not be served");
    assert!(started.elapsed() < Duration::from_secs(4));
    let mut reply = String::new();
    stalled
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let _ = stalled.read_to_string(&mut reply);
    assert!(reply.contains("408"), "{reply}");

    // Drip: a byte every 100 ms never trips the per-read timeout, only the
    // overall header deadline.
    let mut drip = TcpStream::connect(addr).unwrap();
    let started = std::time::Instant::now();
    let dripper = thread::spawn(move || {
        for byte in b"GET /aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" {
            if drip.write_all(&[*byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
    let result = done_rx.recv_timeout(Duration::from_secs(6)).unwrap();
    assert!(result.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let _ = dripper.join();
}

#[test]
fn etag_matching_handles_lists_weak_and_star() {
    let tag = "\"1f-abc\"";
    assert!(etag_matches("\"1f-abc\"", tag));
    assert!(etag_matches("W/\"1f-abc\"", tag));
    assert!(etag_matches("\"zzz\", \"1f-abc\"", tag));
    assert!(etag_matches("*", tag));
    assert!(!etag_matches("\"other\"", tag));
    assert!(!etag_matches("", tag));
}

#[test]
fn thumbnails_get_etag_304_and_immutable_when_versioned() {
    let case_dir = scratch("thumb");
    std::fs::create_dir_all(case_dir.join("review/thumbs")).unwrap();
    std::fs::write(case_dir.join("review/thumbs/a.jpg"), b"jpeg-bytes").unwrap();
    std::fs::write(case_dir.join("review/notes.txt"), b"not a thumb").unwrap();
    let (addr, _state) = start_server(Some(&case_dir), None);
    let get = |target: &str, extra: &str| {
        raw(
            addr,
            &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra}\r\n"),
        )
    };
    let first = get("/review/thumbs/a.jpg", "");
    assert!(status_line(&first).contains("200"), "{first}");
    assert!(first.contains("private, max-age=60"), "{first}");
    let etag = first
        .lines()
        .find_map(|line| line.strip_prefix("ETag: "))
        .expect("etag header")
        .trim()
        .to_string();
    assert!(etag.starts_with('"') && !etag.starts_with("W/"), "{etag}");
    assert!(first.ends_with("jpeg-bytes"));

    let second = get(
        "/review/thumbs/a.jpg",
        &format!("If-None-Match: {etag}\r\n"),
    );
    assert!(status_line(&second).contains("304"), "{second}");
    assert!(!second.contains("jpeg-bytes"), "{second}");
    assert!(second.contains(&format!("ETag: {etag}")), "{second}");
    let stale = get("/review/thumbs/a.jpg", "If-None-Match: \"stale\"\r\n");
    assert!(status_line(&stale).contains("200"), "{stale}");
    let viewer_path = get(
        "/viewer/thumbs/a.jpg",
        &format!("If-None-Match: {etag}\r\n"),
    );
    assert!(status_line(&viewer_path).contains("304"), "{viewer_path}");

    let immutable = get("/review/thumbs/a.jpg?v=abc123", "");
    assert!(
        immutable.contains("Cache-Control: private, max-age=31536000, immutable"),
        "{immutable}"
    );
    // Non-thumbnail files keep the old policy and carry no validator.
    let note = get("/review/notes.txt", "");
    assert!(!note.contains("ETag:"), "{note}");
}

// ---- search / FTS / thumbnail-listing caches ---------------------------

fn search_case(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("case.json"), b"{}").unwrap();
    dir
}

fn write_videos(case_dir: &Path, videos: &[serde_json::Value]) {
    let index = serde_json::json!({"video_count": videos.len(), "videos": videos});
    std::fs::write(case_dir.join("db/video_index.json"), index.to_string()).unwrap();
}

fn vid(id: &str, name: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "source_path": format!("/EV/{name}"),
        "relative_path": name,
        "name": name,
    })
}

fn state_for(case_dir: &Path) -> SharedState {
    let mut job = JobState::new();
    job.case_dir = Some(case_dir.to_path_buf());
    Arc::new(Mutex::new(job))
}

fn query_ids(state: &SharedState, q: &str) -> Vec<String> {
    let mut request = req("GET", "/api/records");
    request.query = format!("q={q}&limit=5000");
    let reply: serde_json::Value = serde_json::from_str(&api_records(&request, state)).unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    reply["videos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect()
}

/// The contract the caches must preserve: case-insensitive substring over
/// id / relative_path / source_path / name / original_name.
fn naive_search(videos: &[serde_json::Value], q: &str) -> Vec<String> {
    let q = q.to_lowercase();
    videos
        .iter()
        .filter(|video| {
            [
                "id",
                "relative_path",
                "source_path",
                "name",
                "original_name",
            ]
            .iter()
            .filter_map(|key| video.get(*key).and_then(|v| v.as_str()))
            .any(|text| text.to_lowercase().contains(&q))
        })
        .map(|video| video["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn cached_search_matches_naive_and_invalidates_on_index_change() {
    let case_dir = search_case("search");
    let mut videos: Vec<serde_json::Value> = (0..40)
        .map(|n| {
            vid(
                &format!("vid_{n:03}"),
                &format!("Cam{}_Front-{n}.MP4", n % 4),
            )
        })
        .collect();
    videos.push(vid("vid_kr", "사고 블랙박스.mp4"));
    write_videos(&case_dir, &videos);
    let state = state_for(&case_dir);

    for q in [
        "cam1", "FRONT-3", "front-", "mp4", "사고", "nomatch", "vid_01",
    ] {
        let expected = naive_search(&videos, q);
        // First call builds the cache, second serves from it: identical.
        assert_eq!(query_ids(&state, q), expected, "cold {q}");
        assert_eq!(query_ids(&state, q), expected, "warm {q}");
    }

    // Index changes -> generation bump -> the lowercased rows are rebuilt.
    videos.push(vid("vid_new", "ZEBRA_crossing.mp4"));
    write_videos(&case_dir, &videos);
    assert_eq!(query_ids(&state, "zebra"), vec!["vid_new".to_string()]);
    videos.remove(0);
    write_videos(&case_dir, &videos);
    assert_eq!(query_ids(&state, "vid_000"), Vec::<String>::new());
    assert_eq!(query_ids(&state, "cam1"), naive_search(&videos, "cam1"));
}

fn db_insert(case_dir: &Path, id: &str, source_path: &str) {
    let conn = crate::case_db::open_case_db(case_dir).unwrap();
    crate::case_db::init_schema(&conn).unwrap();
    conn.execute(
        "INSERT INTO videos (id, source_path, file_url, relative_path, extension,
             size_bytes, hash_status, confidence, source_profile_json, ffprobe_ok,
             first_indexed_unix, last_indexed_unix, record_json)
         VALUES (?1, ?2, '', ?2, 'mp4', 1, 'pending', 'confirmed', '{}', 0, 0, 0, '{}')",
        rusqlite::params![id, source_path],
    )
    .unwrap();
}

#[test]
fn fts_results_are_cached_and_invalidated_when_the_db_changes() {
    let case_dir = search_case("fts");
    let videos = vec![vid("vid_a", "alpha.mp4"), vid("vid_c", "plain.mp4")];
    write_videos(&case_dir, &videos);
    db_insert(&case_dir, "vid_a", "/EV/alpha.mp4");
    db_insert(&case_dir, "vid_c", "/EV/plain.mp4");
    let state = state_for(&case_dir);

    let (hits, indexed) = &*cached_fts_search(&case_dir, "alpha").expect("fts available");
    assert!(hits.contains("vid_a") && !hits.contains("vid_c"));
    assert_eq!(indexed.len(), 2);
    // Same query again: same Arc, i.e. served from the cache.
    let first = cached_fts_search(&case_dir, "alpha").unwrap();
    let second = cached_fts_search(&case_dir, "alpha").unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(query_ids(&state, "alpha"), vec!["vid_a".to_string()]);

    // DB-side change: the DB says vid_c now lives at an "alpha" path. A
    // stale FTS cache would call it "indexed but not a hit" and drop it.
    std::thread::sleep(Duration::from_millis(20));
    let conn = crate::case_db::open_case_db(&case_dir).unwrap();
    conn.execute(
        "UPDATE videos SET source_path = '/EV/alpha-two.mp4' WHERE id = 'vid_c'",
        [],
    )
    .unwrap();
    drop(conn);
    let third = cached_fts_search(&case_dir, "alpha").unwrap();
    assert!(!Arc::ptr_eq(&first, &third), "cache must be invalidated");
    assert!(third.0.contains("vid_c"));
    assert_eq!(
        query_ids(&state, "alpha"),
        vec!["vid_a".to_string(), "vid_c".to_string()]
    );
    // A too-short query has no FTS answer and falls back to substring scan.
    assert!(cached_fts_search(&case_dir, "al").is_none());
    assert_eq!(query_ids(&state, "al"), naive_search(&videos, "al"));
}

#[test]
fn fts_cache_stays_bounded() {
    let case_dir = search_case("ftsbound");
    write_videos(&case_dir, &[vid("vid_a", "alpha.mp4")]);
    db_insert(&case_dir, "vid_a", "/EV/alpha.mp4");
    for n in 0..(FTS_CACHE_CAPACITY * 3) {
        let _ = cached_fts_search(&case_dir, &format!("query-{n}"));
    }
    let len = records_fts_cache_len();
    assert!(len <= FTS_CACHE_CAPACITY, "cache grew to {len}");
}

#[test]
fn thumb_listing_cache_sees_added_and_removed_thumbs() {
    let case_dir = search_case("thumbs");
    let thumbs = case_dir.join("review/thumbs");
    std::fs::create_dir_all(&thumbs).unwrap();
    for stem in ["a", "b"] {
        std::fs::write(thumbs.join(format!("{stem}.jpg")), b"x").unwrap();
    }
    std::fs::write(thumbs.join("ignore.png"), b"x").unwrap();
    assert_eq!(*cached_thumb_stems(&case_dir), vec!["a", "b"]);
    assert_eq!(*cached_thumb_stems(&case_dir), vec!["a", "b"]);
    std::fs::write(thumbs.join("c.jpg"), b"x").unwrap();
    assert_eq!(*cached_thumb_stems(&case_dir), vec!["a", "b", "c"]);
    std::fs::remove_file(thumbs.join("a.jpg")).unwrap();
    assert_eq!(*cached_thumb_stems(&case_dir), vec!["b", "c"]);

    let state = state_for(&case_dir);
    let meta: serde_json::Value = serde_json::from_str(&api_records_meta(&state)).unwrap();
    assert_eq!(meta["thumbs"]["b"], "thumbs/b.jpg");
    assert_eq!(meta["thumbs"]["c"], "thumbs/c.jpg");
    assert!(meta["thumbs"].get("a").is_none());
}

#[test]
fn refused_head_with_unread_body_still_delivers_the_status() {
    let (addr, _state) = start_server(None, None);
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let five_mib = 5 * 1024 * 1024;
    stream
        .write_all(
            format!(
                "POST /api/start HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {five_mib}\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    // The client keeps sending its body while the server has already refused.
    let block = vec![b'x'; 64 * 1024];
    for _ in 0..(five_mib / block.len()) {
        stream.write_all(&block).expect("body write must not reset");
    }
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    let response = String::from_utf8_lossy(&buffer);
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
}

#[test]
fn head_matches_get_without_a_body_and_media_directories_are_refused() {
    let case_dir = scratch("head");
    std::fs::create_dir_all(case_dir.join("review")).unwrap();
    std::fs::write(case_dir.join("review/a.mp4"), b"video-bytes").unwrap();
    let (addr, state) = start_server(Some(&case_dir), None);
    state_lock(&state).media_roots = vec![case_dir.clone()];
    let head = |target: &str| {
        raw(
            addr,
            &format!("HEAD {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        )
    };
    let status = head("/api/status");
    assert!(status_line(&status).contains("200"), "{status}");
    assert!(status.ends_with("\r\n\r\n"), "{status}");
    let file = head("/review/a.mp4");
    assert!(status_line(&file).contains("200"), "{file}");
    assert!(file.contains("Content-Length: 11"), "{file}");
    assert!(!file.contains("video-bytes"), "{file}");
    let dir = raw(
        addr,
        &format!(
            "GET /media?path={} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            case_dir.join("review").display()
        ),
    );
    assert!(status_line(&dir).contains("404"), "{dir}");
}

/// Windows review item 8 over the wire: UNC / device paths are refused
/// with 400 on every GET path route and on the POST routes outside the
/// typed-path set, before any handler runs; on the typed-path POSTs
/// (`/api/preflight`, `/api/start`) device paths are still refused. (The
/// paths name hosts that do not exist; the refusal is textual, so no SMB
/// lookup is attempted even on Windows.) Typed UNC acceptance is checked
/// without the wire in `typed_unc_policy_per_route`, so no test ever
/// makes Windows resolve a UNC host.
#[test]
fn unc_and_device_paths_are_refused_on_every_path_route() {
    let case_dir = scratch("unc-routes");
    let (addr, state) = start_server(Some(&case_dir), None);
    state_lock(&state).media_roots = vec![case_dir.clone()];
    let nonce = state_lock(&state).instance.clone();
    let get = |target: &str| {
        raw(
            addr,
            &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        )
    };
    for target in [
        r"/media?path=\\evil\share\a.mp4",
        "/media?path=%5C%5Cevil%5Cshare%5Ca.mp4",
        // A leading `/` is a local path off Windows (S8), so the mixed
        // spelling stands in for `//` on every OS.
        r"/media?path=\/evil\share\a.mp4",
        r"/media?path=\\?\UNC\evil\share\a.mp4",
        r"/media?path=\\.\PhysicalDrive0",
        r"/api/browse?path=\/evil/share",
        r"/api/browse?path=\\evil\share",
        r"/api/case-info?case_dir=\\evil\share\case",
    ] {
        let response = get(target);
        assert!(
            status_line(&response).contains("400"),
            "{target}: {response}"
        );
        assert!(response.contains("\"ok\":false"), "{target}: {response}");
    }
    let post = |path: &str, body: &str| {
        raw(
            addr,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nX-FrameTrace-Nonce: {nonce}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    for (path, body) in [
        ("/api/open-case", r#"{"case_dir":"\\\\evil\\share\\case"}"#),
        ("/api/open-folder", r#"{"path":"\\\\evil\\share"}"#),
        (
            "/api/preflight",
            r#"{"source_path":"\\\\.\\PhysicalDrive0"}"#,
        ),
        (
            "/api/preflight",
            r#"{"source_path":"C:\\nope","case_dir":"\\\\?\\GLOBALROOT\\Device\\Mup\\evil\\s"}"#,
        ),
        ("/api/start", r#"{"source_path":"\\\\.\\C:\\x"}"#),
        (
            "/api/start",
            r#"{"source_path":"C:\\nope","case_dir":"\\??\\UNC\\evil\\s"}"#,
        ),
        (
            "/api/export-clip",
            r#"{"id":"carve_1","path":"\\\\evil\\s\\a.mp4"}"#,
        ),
        (
            "/api/advanced",
            r#"{"tool":"known-hash","extra":"\\\\evil\\s\\h.txt"}"#,
        ),
    ] {
        let response = post(path, body);
        assert!(
            status_line(&response).contains("400"),
            "{path} {body}: {response}"
        );
    }
    // Local paths still reach their handlers (no 400 from the guard).
    let local = get("/api/browse?path=");
    assert!(status_line(&local).contains("200"), "{local}");
    let missing = get(&format!(
        "/media?path={}",
        case_dir.join("missing.mp4").display()
    ));
    assert!(status_line(&missing).contains("404"), "{missing}");
    let _ = std::fs::remove_dir_all(case_dir);
}

/// The central guard's per-route UNC policy: GET routes refuse UNC unless
/// it lies under an approved UNC root (cross-origin GETs carry no Origin,
/// so they are the SMB/NTLM leak vector); the nonce-gated typed-path
/// POSTs admit an explicitly typed UNC share; other POSTs stay strict.
#[test]
fn typed_unc_policy_per_route() {
    use super::http::remote_path_violation;
    let with = |method: &str, path: &str, query: &str, body: &str| {
        let mut request = req(method, path);
        request.query = query.into();
        request.body = body.into();
        request
    };
    let none: &[std::path::PathBuf] = &[];
    // Typed UNC on the typed-path POSTs: admitted.
    for (path, body) in [
        (
            "/api/preflight",
            r#"{"source_path":"\\\\nas\\evidence\\ev"}"#,
        ),
        (
            "/api/preflight",
            r#"{"source_path":"C:\\ev","case_dir":"\\\\nas\\cases\\c1"}"#,
        ),
        (
            "/api/start",
            r#"{"source_path":"\\\\?\\UNC\\nas\\evidence\\a.e01"}"#,
        ),
        (
            "/api/start",
            r#"{"source_path":"C:\\ev","case_dir":"\\\\nas\\cases\\c1"}"#,
        ),
    ] {
        assert_eq!(
            remote_path_violation(&with("POST", path, "", body), none),
            None,
            "{path} {body}"
        );
    }
    // GET routes: refused without an approved UNC root ...
    for (path, query) in [
        ("/media", "path=%5C%5Cnas%5Cevidence%5Ca.mp4"),
        ("/api/browse", "path=%5C%5Cnas/evidence"),
        ("/api/case-info", "case_dir=%5C%5Cnas%5Ccases%5Cc1"),
    ] {
        assert_eq!(
            remote_path_violation(&with("GET", path, query, ""), none),
            Some(super::http::REMOTE_PATH_ERROR),
            "{path}?{query}"
        );
    }
    // ... and admitted under one (the case moved onto a share).
    let roots = vec![std::path::PathBuf::from(r"\\nas\evidence")];
    assert_eq!(
        remote_path_violation(
            &with("GET", "/media", "path=%5C%5Cnas%5Cevidence%5Ca.mp4", ""),
            &roots
        ),
        None
    );
    // Other POSTs stay strict.
    assert_eq!(
        remote_path_violation(
            &with(
                "POST",
                "/api/open-case",
                "",
                r#"{"case_dir":"\\\\nas\\cases\\c1"}"#
            ),
            none
        ),
        Some(super::http::REMOTE_PATH_ERROR)
    );
}

/// Handlers refuse UNC paths themselves too (defense in depth for callers
/// that do not pass through `handle_connection`).
#[test]
fn handlers_refuse_unc_paths_without_the_http_guard() {
    let browse = super::system::browse_json(r"\\evil\share", false);
    assert!(browse.contains("\"ok\":false"), "{browse}");
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let mut request = req("POST", "/api/open-case");
    request.body = r#"{"case_dir":"\\\\evil\\share\\case"}"#.into();
    let opened = api_open_case(&request, &state);
    assert!(opened.contains("\"ok\":false"), "{opened}");
    assert!(state_lock(&state).case_dir.is_none());
    let mut info = req("GET", "/api/case-info");
    info.query = "case_dir=%5C%5Cevil%5Cshare".into();
    let info = api_case_info(&info, &state);
    assert!(info.contains("UNC"), "{info}");
    // Preflight admits a typed UNC share (policy) but still refuses the
    // device namespace on its own.
    let mut preflight = req("POST", "/api/preflight");
    preflight.body = r#"{"source_path":"\\\\?\\GLOBALROOT\\Device\\Mup\\evil\\share"}"#.into();
    let preflight = api_preflight(&preflight);
    assert!(preflight.contains("UNC"), "{preflight}");
    let mut start = req("POST", "/api/start");
    start.body = r#"{"source_path":"\\\\.\\PhysicalDrive0"}"#.into();
    let started = api_start(&start, &state);
    assert!(started.contains("UNC"), "{started}");
    assert!(!state_lock(&state).busy);
    // Export items with a UNC path are skipped before canonicalize.
    let dir = scratch("unc-export");
    let item = serde_json::json!({"id": "x", "path": r"\\evil\share\a.mp4"});
    let field = |item: &serde_json::Value, key: &str| {
        item.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let outcome = super::media::export_one(&item, std::slice::from_ref(&dir), &dir, &field);
    assert_eq!(outcome.skip_reason, "remote-path-refused");
    let _ = std::fs::remove_dir_all(dir);
}

/// Item 11: every response kind carries `Referrer-Policy: no-referrer`.
#[test]
fn every_response_sends_referrer_policy_no_referrer() {
    let case_dir = scratch("referrer");
    std::fs::create_dir_all(case_dir.join("review")).unwrap();
    std::fs::write(case_dir.join("review/a.mp4"), b"bytes").unwrap();
    let (addr, state) = start_server(Some(&case_dir), None);
    state_lock(&state).media_roots = vec![case_dir.clone()];
    let media = case_dir.join("review").join("a.mp4");
    let get = |target: &str, extra: &str| {
        raw(
            addr,
            &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra}\r\n"),
        )
    };
    for (target, extra) in [
        ("/".to_string(), ""),
        ("/api/status".to_string(), ""),
        ("/nope".to_string(), ""),
        ("/review/a.mp4".to_string(), ""),
        ("/review/missing.mp4".to_string(), ""),
        (format!("/media?path={}", media.display()), ""),
        (
            format!("/media?path={}", media.display()),
            "Range: bytes=0-1\r\n",
        ),
        (
            format!("/media?path={}", media.display()),
            "Range: bytes=99-\r\n",
        ),
        ("/media".to_string(), ""),
        (r"/media?path=\\evil\s\a.mp4".to_string(), ""),
    ] {
        let response = get(&target, extra);
        let head = &response[..response.find("\r\n\r\n").unwrap_or(response.len())];
        assert!(
            head.contains("\r\nReferrer-Policy: no-referrer"),
            "{target} {extra}: {head}"
        );
    }
    // Refusals before routing (403 cross-origin) carry it as well.
    let refused = raw(
        addr,
        "GET /api/status HTTP/1.1\r\nHost: evil.example\r\n\r\n",
    );
    assert!(
        refused.contains("Referrer-Policy: no-referrer"),
        "{refused}"
    );
    let _ = std::fs::remove_dir_all(case_dir);
}

/// Item 7 over the wire: a page on another localhost port cannot drive
/// this server even with the planted cookies; the same authority can.
#[test]
fn origin_from_another_local_port_is_refused() {
    let (addr, state) = start_server(None, None);
    let nonce = state_lock(&state).instance.clone();
    let port = addr.port();
    let post = |origin: &str| {
        raw(
            addr,
            &format!(
                "POST /api/cancel HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {origin}\r\nCookie: ft_nonce={nonce}\r\nContent-Length: 0\r\n\r\n"
            ),
        )
    };
    let other_port = if port == 9999 { 9998 } else { 9999 };
    let foreign = post(&format!("http://127.0.0.1:{other_port}"));
    assert!(status_line(&foreign).contains("403"), "{foreign}");
    let same = post(&format!("http://127.0.0.1:{port}"));
    assert!(status_line(&same).contains("200"), "{same}");
    // The shell's raw-socket requests send no Origin at all: unchanged.
    let shell = raw(
        addr,
        &format!(
            "POST /api/cancel HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-FrameTrace-Nonce: {nonce}\r\nContent-Length: 0\r\n\r\n"
        ),
    );
    assert!(status_line(&shell).contains("200"), "{shell}");
}
