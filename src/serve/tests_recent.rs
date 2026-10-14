//! Stage-1 start screen: recent-projects store (`recent-cases.json`),
//! GET /api/recent-cases + forget, and evidence-kind auto-detection.

use super::*;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ft-recent-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn entry(dir: &str, when: u64) -> RecentCase {
    RecentCase {
        case_dir: dir.into(),
        name: String::new(),
        source_path: None,
        last_opened_unix: when,
    }
}

fn make_case(dir: &Path) {
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("case.json"), "{}").unwrap();
}

fn route_req(method: &str, path: &str, body: &str) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: String::new(),
        body: body.into(),
        range: None,
        origin: None,
        host: Some("127.0.0.1:8477".into()),
        cookie: None,
        token_header: None,
        nonce_header: None,
    }
}

fn body_of(response: &[u8]) -> String {
    let text = String::from_utf8_lossy(response).to_string();
    text.split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn upsert_inserts_first_dedupes_and_caps() {
    let mut list = Vec::new();
    for i in 0..(RECENT_CAP + 5) {
        list = upsert_recent(list, entry(&format!("/nonexistent/ft/case-{i}"), i as u64));
    }
    assert_eq!(list.len(), RECENT_CAP, "capped");
    // Most recent first; the oldest five fell off.
    assert_eq!(
        list[0].case_dir,
        format!("/nonexistent/ft/case-{}", RECENT_CAP + 4)
    );
    assert!(!list.iter().any(|c| c.case_dir == "/nonexistent/ft/case-0"));

    // Re-opening an existing entry moves it to the top without duplicating;
    // a trailing separator is the same folder.
    let mut with_source = entry("/nonexistent/ft/case-10", 1);
    with_source.source_path = Some("/ev/src".into());
    let list = upsert_recent(list, with_source);
    let list = upsert_recent(list, entry("/nonexistent/ft/case-10/", 999));
    assert_eq!(list.len(), RECENT_CAP);
    assert_eq!(list[0].last_opened_unix, 999);
    assert_eq!(
        list.iter()
            .filter(|c| c.case_dir.trim_end_matches('/') == "/nonexistent/ft/case-10")
            .count(),
        1
    );
    // A later open without a source keeps the remembered source.
    assert_eq!(list[0].source_path.as_deref(), Some("/ev/src"));
}

#[test]
fn store_round_trips_orders_and_dedupes_by_canonical_dir() {
    let root = scratch("store");
    let file = root.join("cfg/recent-cases.json");
    let a = root.join("a");
    let b = root.join("b");
    make_case(&a);
    make_case(&b);
    record_recent_in(&file, &a, Some(Path::new("/ev/a")), 100).unwrap();
    record_recent_in(&file, &b, None, 200).unwrap();
    // Same folder through a non-canonical spelling: deduped, moved first.
    let alias = root.join("b/../a");
    record_recent_in(&file, &alias, None, 300).unwrap();
    let list = load_recent(&file);
    assert_eq!(list.len(), 2, "{list:?}");
    assert_eq!(list[0].case_dir, canonical_case_dir(&a));
    assert_eq!(list[0].last_opened_unix, 300);
    assert_eq!(list[0].source_path.as_deref(), Some("/ev/a"));
    assert_eq!(list[0].name, "a");
    assert_eq!(list[1].case_dir, canonical_case_dir(&b));
    // Written atomically: no temp leftovers next to the store.
    let leftovers: Vec<_> = std::fs::read_dir(root.join("cfg"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "recent-cases.json")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn corrupt_or_missing_store_is_an_empty_list_and_recovers() {
    let root = scratch("corrupt");
    let file = root.join("recent-cases.json");
    assert!(load_recent(&file).is_empty(), "missing file");
    std::fs::write(&file, b"{not json\x00\xff").unwrap();
    assert!(load_recent(&file).is_empty(), "corrupt file");
    assert_eq!(recent_cases_json_from(&file), serde_json::json!([]));
    // The next record overwrites the corrupt file with a valid one.
    let case = root.join("case");
    make_case(&case);
    record_recent_in(&file, &case, None, 5).unwrap();
    assert_eq!(load_recent(&file).len(), 1);
    // Entries with an empty case_dir are dropped; a bare array is accepted.
    std::fs::write(
        &file,
        r#"[{"case_dir":"","last_opened_unix":9},{"case_dir":"/x/y","last_opened_unix":1}]"#,
    )
    .unwrap();
    let list = load_recent(&file);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].case_dir, "/x/y");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn missing_folder_is_flagged_and_forget_removes_it() {
    let root = scratch("forget");
    let file = root.join("recent-cases.json");
    let kept = root.join("kept");
    make_case(&kept);
    let gone = root.join("gone");
    make_case(&gone);
    record_recent_in(&file, &gone, None, 1).unwrap();
    record_recent_in(&file, &kept, None, 2).unwrap();
    let gone_text = canonical_case_dir(&gone);
    std::fs::remove_dir_all(&gone).unwrap();

    let items = recent_cases_json_from(&file);
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["exists"], true);
    let missing = &items[1];
    assert_eq!(missing["case_dir"], gone_text.as_str());
    assert_eq!(missing["exists"], false);
    assert_eq!(missing["phase_hint"], "missing");
    assert!(missing["reviewed"].is_null() && missing["total"].is_null());

    assert!(forget_recent_in(&file, &gone_text).unwrap());
    assert!(
        !forget_recent_in(&file, &gone_text).unwrap(),
        "already gone"
    );
    let list = load_recent(&file);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].case_dir, canonical_case_dir(&kept));
    // The case folder itself is never touched by forget.
    assert!(kept.join("case.json").is_file());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn phase_hint_and_counts_come_from_cheap_case_reads() {
    let root = scratch("phase");
    let case = root.join("case");
    make_case(&case);
    let hint = |with_db: bool| recent_case_json(&entry(&case.display().to_string(), 1), with_db);
    assert_eq!(hint(true)["phase_hint"], "empty");

    // Interrupted scan: checkpoint present, no review → done = record lines.
    std::fs::write(
        scan_progress_path(&case),
        "{\"checkpoint\":\"scan-folder\",\"run_id\":\"r\"}\n{\"id\":\"a\"}\n{\"id\":\"b\"}\n",
    )
    .unwrap();
    let v = hint(true);
    assert_eq!(v["phase_hint"], "interrupted");
    assert_eq!(v["checkpoint_done"], 2);
    std::fs::remove_file(scan_progress_path(&case)).unwrap();

    std::fs::write(case.join("db/video_index.json"), r#"{"videos":[]}"#).unwrap();
    assert_eq!(hint(true)["phase_hint"], "partial");
    std::fs::create_dir_all(case.join("review")).unwrap();
    std::fs::write(case.join("review/index.html"), "<html>").unwrap();
    assert_eq!(hint(true)["phase_hint"], "review");
    std::fs::create_dir_all(case.join("reports")).unwrap();
    std::fs::write(case.join("reports/case-report.html"), "<html>").unwrap();
    assert_eq!(hint(true)["phase_hint"], "finalized");

    // Counts: read-only DB open; marks / videos rows.
    {
        let conn = crate::case_db::open_case_db(&case).unwrap();
        crate::case_db::init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO review_marks (record_id, status, marked_unix) VALUES ('v1','important',1)",
            [],
        )
        .unwrap();
    }
    let v = hint(true);
    assert_eq!(v["reviewed"], 1);
    assert_eq!(v["total"], 0);
    // Past the time budget the counts are skipped, not blocked on.
    let v = hint(false);
    assert!(v["reviewed"].is_null() && v["total"].is_null());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn recent_routes_list_and_forget() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let root = scratch("routes");
    let case = root.join("case");
    make_case(&case);
    // Item 7: this test's own list file (parallel tests never share it).
    let _own = override_recent_cases_file(root.join("recent.json"));
    let file = recent_cases_file().unwrap();
    assert_eq!(file, root.join("recent.json"));
    record_recent_in(&file, &case, Some(Path::new("/ev/src")), u64::MAX / 2).unwrap();
    let listed = body_of(&route(&route_req("GET", "/api/recent-cases", ""), &state));
    let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let mine = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["case_dir"] == canonical_case_dir(&case).as_str())
        .cloned()
        .expect("recorded case is listed");
    for key in [
        "case_dir",
        "name",
        "source_path",
        "last_opened_unix",
        "exists",
        "phase_hint",
        "reviewed",
        "total",
    ] {
        assert!(mine.get(key).is_some(), "missing {key}: {mine}");
    }
    assert_eq!(mine["source_path"], "/ev/src");
    assert_eq!(mine["exists"], true);

    let body = serde_json::json!({"case_dir": canonical_case_dir(&case)}).to_string();
    let forgot = body_of(&route(
        &route_req("POST", "/api/recent-cases/forget", &body),
        &state,
    ));
    assert!(forgot.contains("\"removed\":true"), "{forgot}");
    let empty = body_of(&route(
        &route_req("POST", "/api/recent-cases/forget", "{}"),
        &state,
    ));
    assert!(empty.contains("\"ok\":false"), "{empty}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_case_records_the_case_in_the_recent_list() {
    let state: SharedState = Arc::new(Mutex::new(JobState::new()));
    let root = scratch("open");
    let case = root.join("case");
    make_case(&case);
    let _own = override_recent_cases_file(root.join("recent.json"));
    let body = serde_json::json!({"case_dir": case.display().to_string()}).to_string();
    let out = api_open_case(&route_req("POST", "/api/open-case", &body), &state);
    assert!(out.contains("\"ok\":true"), "{out}");
    assert!(out.contains("\"has_index\":false"), "{out}");
    let list = load_recent(&recent_cases_file().unwrap());
    assert!(
        list.iter().any(|c| c.case_dir == canonical_case_dir(&case)),
        "{list:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn evidence_kind_is_auto_detected() {
    assert!(has_ewf_signature(b"EVF\x09\x0d\x0a\xff\x00rest"));
    assert!(has_ewf_signature(b"EVF2\x0d\x0a\x81\x00"));
    assert!(has_ewf_signature(b"LVF\x09\x0d\x0a\xff\x00"));
    assert!(!has_ewf_signature(b"EVF\x09"));
    assert!(!has_ewf_signature(b"\x00\x00\x00\x18ftypmp42"));
    assert_eq!(source_kind(false, "/ev/BLACKBOX", false), "folder");
    assert_eq!(source_kind(true, "D:/img/disk.E01", false), "e01");
    assert_eq!(source_kind(true, "disk.ex01", false), "e01");
    assert_eq!(source_kind(true, "disk.bin", true), "e01");
    assert_eq!(source_kind(true, "clip.mp4", false), "other");

    let root = scratch("kind");
    std::fs::write(root.join("disk.E01"), b"x").unwrap();
    let mut evf = b"EVF\x09\x0d\x0a\xff\x00".to_vec();
    evf.extend_from_slice(&[0u8; 32]);
    std::fs::write(root.join("renamed.img"), &evf).unwrap();
    std::fs::write(root.join("notes.txt"), b"hello world").unwrap();
    let kind = |name: &str| {
        let v: serde_json::Value =
            serde_json::from_str(&browse_json(&root.join(name).display().to_string(), false))
                .unwrap();
        (
            v["kind"].as_str().unwrap_or("").to_string(),
            v["e01_magic"].clone(),
        )
    };
    assert_eq!(kind("disk.E01").0, "e01");
    assert_eq!(kind("renamed.img"), ("e01".into(), serde_json::json!(true)));
    assert_eq!(
        kind("notes.txt"),
        ("other".into(), serde_json::json!(false))
    );
    assert_eq!(kind("nope").0, "missing");
    let dir: serde_json::Value =
        serde_json::from_str(&browse_json(&root.display().to_string(), true)).unwrap();
    assert_eq!(dir["kind"], "folder");
    assert!(
        dir.get("canonical").is_none(),
        "only case folders report it"
    );
    // A case folder reports its canonical path (dedupe against the list).
    let case = root.join("case");
    make_case(&case);
    std::fs::write(case.join("db/case.db"), b"").unwrap();
    let alias = root.join("case/../case");
    let listed: serde_json::Value =
        serde_json::from_str(&browse_json(&alias.display().to_string(), false)).unwrap();
    assert_eq!(listed["is_case"], true);
    assert_eq!(listed["canonical"], canonical_case_dir(&case).as_str());
    let _ = std::fs::remove_dir_all(&root);
}

/// Item 7: the override is per thread — another test thread keeps the
/// shared per-process file, and neither is the examiner's real list.
#[test]
fn recent_file_override_is_per_thread_and_never_the_real_config() {
    let root = scratch("override");
    let own = root.join("recent.json");
    {
        let _own = override_recent_cases_file(own.clone());
        assert_eq!(recent_cases_file().unwrap(), own);
        let other = std::thread::spawn(recent_cases_file)
            .join()
            .unwrap()
            .unwrap();
        assert_ne!(other, own);
        assert!(
            other.starts_with(std::env::temp_dir()),
            "{}",
            other.display()
        );
    }
    let after = recent_cases_file().unwrap();
    assert_ne!(after, own, "the guard clears the override");
    if let Some(config) = crate::audit_key::config_dir() {
        assert!(!after.starts_with(config), "{}", after.display());
    }
    let _ = std::fs::remove_dir_all(&root);
}
