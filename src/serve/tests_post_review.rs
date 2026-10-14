//! Phase-3 post-review fixes (§R in docs/PLAN_PHASE3_2026-10-13.md):
//! cancellable export-selected (R3), version-probe outcomes (R6), cheap
//! `/api/status` under a DB writer (R10) and per-port auth cookies (C2).

use super::*;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-pr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn request(method: &str, path: &str, body: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: String::new(),
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

// ---- R3: export-selected is cancellable and never loses its manifest ----

#[test]
fn cancelled_export_selected_writes_a_manifest_marked_partial() {
    let case = scratch("r3-cancel");
    let sources = case.join("src");
    std::fs::create_dir_all(&sources).unwrap();
    let total = 24usize;
    let items: Vec<serde_json::Value> = (0..total)
        .map(|index| {
            let path = sources.join(format!("clip{index:02}.mp4"));
            std::fs::write(&path, format!("video {index}")).unwrap();
            serde_json::json!({"id": format!("vid_{index}"), "path": path.to_string_lossy()})
        })
        .collect();
    let body = serde_json::json!({ "items": items }).to_string();
    let state = bound(&case);
    let cancel_state = Arc::clone(&state);
    // The examiner presses 중단 while the first item is being copied.
    let reply = parse(&super::media::api_export_selected_with(
        &request("POST", "/api/export-selected", &body),
        &state,
        &|index| {
            if index == 0 {
                let cancel = parse(&api_cancel(&cancel_state));
                assert_eq!(cancel["ok"], true, "{cancel}");
                assert_eq!(cancel["cancelled"], "writes", "{cancel}");
            }
        },
    ));
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(reply["cancelled"], true, "{reply}");
    let done = reply["items_done"].as_u64().unwrap() as usize;
    assert!(done >= 1 && done < total, "partial: {reply}");
    assert_eq!(reply["items_total"], total as u64);
    assert!(reply["error"].as_str().unwrap().contains("중단"), "{reply}");
    let export_dir = PathBuf::from(reply["export_dir"].as_str().unwrap());
    // Every selected item is in the manifest; the unreached ones say why.
    let jsonl = std::fs::read_to_string(export_dir.join("manifest.jsonl")).unwrap();
    let rows: Vec<serde_json::Value> = jsonl.lines().map(parse).collect();
    assert_eq!(rows.len(), total);
    let not_done = rows
        .iter()
        .filter(|row| row["skipped"] == super::media::EXPORT_CANCELLED_REASON)
        .count();
    assert_eq!(not_done, total - done);
    let csv = std::fs::read_to_string(export_dir.join("manifest.csv")).unwrap();
    assert_eq!(csv.lines().count(), total + 1, "header + one row per item");
    let audit = parse(&std::fs::read_to_string(export_dir.join("manifest-audit.json")).unwrap());
    assert_eq!(audit["cancelled"], true, "{audit}");
    assert_eq!(audit["items_done"], done as u64);
    assert_eq!(audit["items_total"], total as u64);
    let readme = std::fs::read_to_string(export_dir.join("README.txt")).unwrap();
    assert!(readme.contains("상태: 중단됨"), "{readme}");
    assert!(
        readme.contains(&format!("{total}개 중 {done}개")),
        "{readme}"
    );
    // The chained audit event records the partial batch too.
    let log = std::fs::read_to_string(case.join("evidence/logs/export-log.jsonl")).unwrap();
    assert!(log.contains("\"cancelled\":true"), "{log}");
    // The cancel was consumed with its lease: a new write op is not
    // cancelled before it starts.
    assert!(state_lock(&state).write_cancels.is_empty());
    let (next, _) = try_begin_write_op(&state).unwrap();
    assert!(!next.cancelled());
    let _ = std::fs::remove_dir_all(case);
}

#[test]
fn uncancelled_export_selected_reports_complete() {
    let case = scratch("r3-full");
    let source = case.join("src/a.mp4");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, b"video").unwrap();
    let state = bound(&case);
    let body = serde_json::json!({"items": [{"id": "vid_1", "path": source.to_string_lossy()}]})
        .to_string();
    let reply = parse(&api_export_selected(
        &request("POST", "/api/export-selected", &body),
        &state,
    ));
    assert_eq!(reply["ok"], true, "{reply}");
    let export_dir = PathBuf::from(reply["export_dir"].as_str().unwrap());
    let audit = parse(&std::fs::read_to_string(export_dir.join("manifest-audit.json")).unwrap());
    assert_eq!(audit["cancelled"], false, "{audit}");
    assert_eq!(audit["items_done"], 1);
    let readme = std::fs::read_to_string(export_dir.join("README.txt")).unwrap();
    assert!(readme.contains("상태: 완료"), "{readme}");
    let _ = std::fs::remove_dir_all(case);
}

// ---- R6: version probes — bound, env override, timeout is its own fact ----

#[test]
fn probe_timeout_defaults_to_sixty_seconds_and_honours_the_env_value() {
    use crate::util::{VERSION_PROBE_TIMEOUT_SECS, parse_probe_timeout};
    assert_eq!(VERSION_PROBE_TIMEOUT_SECS, 60);
    assert_eq!(parse_probe_timeout(None), 60);
    assert_eq!(parse_probe_timeout(Some("90")), 90);
    assert_eq!(parse_probe_timeout(Some(" 5 ")), 5);
    for bad in ["", "0", "-3", "abc", "3601", "1.5"] {
        assert_eq!(parse_probe_timeout(Some(bad)), 60, "{bad:?}");
    }
}

#[test]
fn timed_out_version_probe_is_recorded_as_timed_out_not_unavailable() {
    use crate::util::RunError;
    let timed_out = crate::tool_policy::describe_version_probe(
        "C:/tools/ffmpeg.exe",
        60,
        Err(RunError::TimedOut(
            "ffmpeg did not finish within 60s".into(),
        )),
    );
    assert!(
        timed_out.starts_with("version probe timed out after 60s"),
        "{timed_out}"
    );
    assert!(!timed_out.contains("unavailable"), "{timed_out}");
    let failed = crate::tool_policy::describe_version_probe(
        "ffmpeg",
        60,
        Err(RunError::Failed("failed to run ffmpeg: not found".into())),
    );
    assert!(failed.starts_with("unavailable:"), "{failed}");
    // The env probe keeps the outcomes apart too.
    use super::system::{ToolProbe, classify_tool_probe};
    assert_eq!(
        classify_tool_probe(Err(RunError::TimedOut("t".into()))),
        ToolProbe::TimedOut
    );
    assert_eq!(
        classify_tool_probe(Err(RunError::Failed("f".into()))),
        ToolProbe::Failed
    );
}

#[test]
fn a_command_past_its_bound_is_reported_as_a_timeout() {
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("ping");
        command.args(["-n", "6", "127.0.0.1"]);
        command
    } else {
        let mut command = std::process::Command::new("sleep");
        command.arg("5");
        command
    };
    match crate::util::run_with_timeout_detailed(&mut command, Some(1)) {
        Err(crate::util::RunError::TimedOut(message)) => {
            assert!(message.contains("did not finish within 1s"), "{message}")
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
}

#[test]
fn env_probe_reports_every_tool_with_a_distinct_outcome() {
    let env = parse(&api_env(&request("GET", "/api/env", "")));
    assert_eq!(env["ok"], true, "{env}");
    let outcomes = env["tool_probe"].as_object().expect("tool_probe map");
    for (name, _) in super::system::PROBED_TOOLS {
        let outcome = outcomes[*name].as_str().unwrap();
        assert!(
            ["present", "missing", "failed", "timed-out"].contains(&outcome),
            "{name}: {outcome}"
        );
        assert_eq!(env["tools"][*name], outcome == "present", "{env}");
    }
    assert!(env["probe_timed_out"].is_array());
    assert!(env["probe_timeout_secs"].as_u64().unwrap() >= 1);
}

// ---- R10: /api/status stays cheap while a writer holds the case DB ----

#[test]
fn status_answers_fast_while_the_case_db_is_write_locked() {
    let case = scratch("r10-status");
    std::fs::write(case.join("case.json"), br#"{"case_id":"FT-r10"}"#).unwrap();
    let job = crate::case_db::start_job(&case, "carve", &case, Some(100), "{}").unwrap();
    let state = bound(&case);
    // Unlocked: the running job's progress is read and remembered.
    let status = parse(&api_status(&state));
    assert_eq!(status["progress"]["job_type"], "carve", "{status}");
    assert!(status["progress"].get("stale").is_none(), "{status}");
    // A carve child holds the DB (EXCLUSIVE: even readers wait).
    let writer = rusqlite::Connection::open(crate::case_db::case_db_path(&case)).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    // R4-B20: proven by counters, not wall time (flaky on a loaded
    // runner): the poll made exactly one bounded (`_quick`) progress read,
    // and that read found the DB locked and fell back to the remembered
    // progress instead of waiting out a busy timeout.
    let reads = super::system::STATUS_PROGRESS_READS.with(std::cell::Cell::get);
    let fallbacks = super::system::STATUS_PROGRESS_BUSY_FALLBACKS.with(std::cell::Cell::get);
    let status = parse(&api_status(&state));
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        super::system::STATUS_PROGRESS_READS.with(std::cell::Cell::get),
        reads + 1,
        "one bounded progress read per status poll"
    );
    assert_eq!(
        super::system::STATUS_PROGRESS_BUSY_FALLBACKS.with(std::cell::Cell::get),
        fallbacks + 1,
        "the locked DB took the non-waiting fallback"
    );
    // The last progress seen is reported, marked stale — not dropped.
    assert_eq!(status["progress"]["job_type"], "carve", "{status}");
    assert_eq!(status["progress"]["stale"], true, "{status}");
    let _ = crate::case_db::complete_job(&case, &job.job_id, 100, "done");
    let _ = std::fs::remove_dir_all(case);
}

// ---- C2: per-port auth cookies keep two instances apart ----

/// A browser cookie jar for one host (cookies are NOT scoped by port).
#[derive(Default)]
struct HostJar(Vec<(String, String)>);

impl HostJar {
    fn absorb(&mut self, response: &str) {
        let head = response.split("\r\n\r\n").next().unwrap_or_default();
        for line in head.lines() {
            let Some(value) = line.strip_prefix("Set-Cookie: ") else {
                continue;
            };
            let pair = value.split(';').next().unwrap_or_default();
            let (name, value) = pair.split_once('=').unwrap();
            self.0.retain(|(existing, _)| existing != name);
            self.0.push((name.to_string(), value.to_string()));
        }
    }

    fn header(&self) -> String {
        self.0
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn http(addr: std::net::SocketAddr, request: &str) -> String {
    use std::io::{Read, Write};
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut buffer = Vec::new();
    let _ = stream.read_to_end(&mut buffer);
    String::from_utf8_lossy(&buffer).to_string()
}

fn spawn_server(token: Option<&str>) -> (std::net::SocketAddr, SharedState) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    state_lock(&state).token = token.map(str::to_string);
    let serving = Arc::clone(&state);
    std::thread::spawn(move || serve_on(listener, serving));
    (addr, state)
}

fn post_cancel(addr: std::net::SocketAddr, cookie: &str) -> String {
    http(
        addr,
        &format!(
            "POST /api/cancel HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: {cookie}\r\nContent-Length: 0\r\n\r\n"
        ),
    )
}

#[test]
fn two_instances_sharing_one_cookie_jar_both_stay_authorized() {
    // Nonce model (no token): each page load plants its server's nonce.
    let (a, state_a) = spawn_server(None);
    let (b, state_b) = spawn_server(None);
    let mut jar = HostJar::default();
    jar.absorb(&http(a, "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"));
    // The examiner then opens the second instance in the same browser.
    jar.absorb(&http(b, "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"));
    assert_eq!(jar.0.len(), 2, "one nonce cookie per port: {:?}", jar.0);
    for addr in [a, b] {
        let reply = post_cancel(addr, &jar.header());
        assert!(
            reply.starts_with("HTTP/1.1 200"),
            "instance on {} lost its proof: {reply}",
            addr.port()
        );
    }
    // The legacy unsuffixed name only counts when its value validates: the
    // other instance's nonce under the legacy name is refused.
    let nonce_b = state_lock(&state_b).instance.clone();
    let legacy_other = post_cancel(a, &format!("ft_nonce={nonce_b}"));
    assert!(legacy_other.starts_with("HTTP/1.1 401"), "{legacy_other}");
    let nonce_a = state_lock(&state_a).instance.clone();
    let legacy_own = post_cancel(a, &format!("ft_nonce={nonce_a}"));
    assert!(legacy_own.starts_with("HTTP/1.1 200"), "{legacy_own}");
    state_lock(&state_a).shutdown.store(true, Ordering::SeqCst);
    state_lock(&state_b).shutdown.store(true, Ordering::SeqCst);
}

#[test]
fn two_token_instances_sharing_one_cookie_jar_both_stay_authorized() {
    let (a, state_a) = spawn_server(Some("token-aaaa-1"));
    let (b, state_b) = spawn_server(Some("token-bbbb-2"));
    let mut jar = HostJar::default();
    jar.absorb(&http(
        a,
        "GET /?token=token-aaaa-1 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
    ));
    jar.absorb(&http(
        b,
        "GET /?token=token-bbbb-2 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
    ));
    assert_eq!(jar.0.len(), 2, "one token cookie per port: {:?}", jar.0);
    for addr in [a, b] {
        let reply = http(
            addr,
            &format!(
                "GET /api/status HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: {}\r\n\r\n",
                jar.header()
            ),
        );
        assert!(
            reply.starts_with("HTTP/1.1 200"),
            "instance on {} answered: {reply}",
            addr.port()
        );
    }
    state_lock(&state_a).shutdown.store(true, Ordering::SeqCst);
    state_lock(&state_b).shutdown.store(true, Ordering::SeqCst);
}
