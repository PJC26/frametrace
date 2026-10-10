//! Paged record APIs (/api/records, /api/records-meta) and filesystem-inspection lookup.

use super::*;

/// `GET /api/records?offset=0&limit=500&q=foo` — paged slice of the case
/// video index so the reviewer UI does not have to parse one giant inline
/// JSON block for a large case. `q` is a case-insensitive substring match
/// over id/paths/name; the JSON index stays the shape authority so the
/// API can never drift from what the viewer renders offline.
/// Returns the parsed `db/video_index.json`, shared via `Arc` (PERF-01).
/// The global state lock is held only to look up / store the cache entry —
/// never while reading, parsing or filtering — so a large index no longer
/// stalls every other endpoint, and a page fetch costs one `Arc` clone
/// instead of a deep copy of the whole document.
pub(crate) fn cached_index(
    state: &SharedState,
    case_dir: &Path,
) -> Result<Arc<serde_json::Value>, &'static str> {
    let index_path = case_dir.join("db/video_index.json");
    let meta = index_path.metadata().map_err(|_| "no case index yet")?;
    let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
    let len = meta.len();
    if let Some(cache) = &state_lock(state).index_cache
        && cache.path == index_path
        && cache.mtime == mtime
        && cache.len == len
    {
        return Ok(Arc::clone(&cache.value));
    }
    let text = crate::util::read_to_string(&index_path).map_err(|_| "no case index yet")?;
    let value = Arc::new(
        serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|_| "case index is not valid JSON")?,
    );
    state_lock(state).index_cache = Some(IndexCache {
        path: index_path,
        mtime,
        len,
        value: Arc::clone(&value),
    });
    Ok(value)
}

pub(crate) fn api_records(request: &Request, state: &SharedState) -> String {
    let Some(case_dir) = state_lock(state).case_dir.clone() else {
        return "{\"ok\":false,\"error\":\"no case open\"}".to_string();
    };
    let index = match cached_index(state, &case_dir) {
        Ok(index) => index,
        Err(err) => return api_err(err),
    };
    let empty = Vec::new();
    let videos = index
        .get("videos")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let query = query_value(&request.query, "q").map(|q| q.to_lowercase());
    // FTS5 trigram index accelerates the same substring contract for
    // DB-indexed records; `None` (no db, short query, or any error)
    // falls back to the full substring scan rather than returning an
    // honest-but-wrong empty set.
    let fts = query.as_deref().and_then(|q| {
        crate::case_db::fts_search_video_ids(&case_dir, q)
            .ok()
            .flatten()
    });
    let substring_match = |video: &&serde_json::Value, q: &str| -> bool {
        [
            "id",
            "relative_path",
            "source_path",
            "name",
            "original_name",
        ]
        .iter()
        .filter_map(|key| video.get(*key).and_then(|v| v.as_str()))
        .any(|text| text.to_lowercase().contains(q))
    };
    let matches = |video: &&serde_json::Value| -> bool {
        let Some(q) = query.as_deref() else {
            return true;
        };
        if let Some((hits, indexed)) = &fts {
            let id = video.get("id").and_then(|v| v.as_str());
            return match id {
                Some(id) if hits.contains(id) => true,
                // Indexed in the db but not in FTS results = no match;
                // anything else (carved/filesystem records outside the
                // videos table) still gets the substring contract.
                Some(id) if indexed.contains(id) => false,
                _ => substring_match(video, q),
            };
        }
        substring_match(video, q)
    };
    let offset = query_value(&request.query, "offset")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = query_value(&request.query, "limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(500)
        .clamp(1, 5000);
    let filtered: Vec<&serde_json::Value> = videos.iter().filter(matches).collect();
    let total = filtered.len();
    let page: Vec<serde_json::Value> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    serde_json::json!({"ok": true, "total": total, "offset": offset, "videos": page}).to_string()
}

/// `GET /api/records-meta` — the non-record half of the viewer data
/// bundle (manifest, logs, thumbs map, annotations, deepfake reports) so
/// the paged API can fully replace the inline `__FRAMETRACE_DATA__`
/// payload when the viewer is served over the workstation.
pub(crate) fn api_records_meta(state: &SharedState) -> String {
    let Some(case_dir) = state_lock(state).case_dir.clone() else {
        return "{\"ok\":false,\"error\":\"no case open\"}".to_string();
    };
    let read_json = |path: PathBuf| -> serde_json::Value {
        crate::util::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(serde_json::Value::Null)
    };
    let read_jsonl = |path: PathBuf| -> serde_json::Value {
        let items: Vec<serde_json::Value> = crate::util::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect();
        serde_json::Value::Array(items)
    };
    // The scan object minus the (paged) videos array — built from the
    // shared index cache so the (large) videos array is neither re-parsed
    // nor cloned here.
    let scan = match cached_index(state, &case_dir).as_deref() {
        Ok(serde_json::Value::Object(map)) => {
            let mut scan: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter(|(key, _)| key.as_str() != "videos")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            scan.insert("videos".to_string(), serde_json::Value::Array(Vec::new()));
            serde_json::Value::Object(scan)
        }
        Ok(other) => other.clone(),
        Err(_) => serde_json::Value::Null,
    };
    let fls_entries = std::fs::read_dir(case_dir.join("db/filesystem"))
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("tsk-files-") && name.ends_with(".jsonl")
                        })
                })
                .max()
        })
        .map(read_jsonl)
        .unwrap_or(serde_json::Value::Array(Vec::new()));
    // Same thumb mapping the bundle generator emits: review/thumbs/<id>.jpg
    // relative to the served viewer page.
    let mut thumbs = serde_json::Map::new();
    if let Ok(entries) = std::fs::read_dir(case_dir.join("review/thumbs")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jpg")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                thumbs.insert(
                    stem.to_string(),
                    serde_json::Value::String(format!("thumbs/{stem}.jpg")),
                );
            }
        }
    }
    // Proxies already on disk so the viewer can prefer them for
    // browser-unplayable containers without a redundant generate pass.
    let mut proxies: Vec<serde_json::Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(case_dir.join("artifacts/proxies")) {
        for entry in entries.flatten() {
            let path = crate::audit::canonical_or_original(&entry.path());
            if path.extension().and_then(|e| e.to_str()) == Some("mp4") {
                proxies.push(serde_json::Value::String(crate::audit::path_string(&path)));
            }
        }
    }
    proxies.sort_by(|a, b| a.as_str().unwrap_or("").cmp(b.as_str().unwrap_or("")));
    let annotations = serde_json::json!({
        "marks": crate::case_db::load_review_marks(&case_dir).unwrap_or_default(),
        "tags": crate::case_db::load_review_tags(&case_dir).unwrap_or_default(),
    });
    let body = serde_json::json!({
        "ok": true,
        "manifest": read_json(case_dir.join("case.json")),
        // The viewer absolutizes relative carve/recovery artifact paths
        // against this — same field name as the standalone data bundle.
        "caseDir": crate::audit::path_string(&case_dir),
        "scan": scan,
        "carveLog": read_jsonl(case_dir.join("artifacts/carved/carve-log.jsonl")),
        "filesystemLog": read_jsonl(case_dir.join("evidence/logs/tsk-audit.jsonl")),
        "validationLog": read_jsonl(case_dir.join("evidence/logs/validation-log.jsonl")),
        "anomalyLog": read_jsonl(case_dir.join("evidence/logs/anomaly-log.jsonl")),
        "flsEntries": fls_entries,
        "thumbs": thumbs,
        "annotations": annotations,
        "deepfake": crate::deepfake::collect_reports(&case_dir),
        "telemetry": crate::telemetry::collect_reports(&case_dir),
        "proxies": proxies,
    });
    serde_json::to_string(&body).unwrap_or_else(|_| "{\"ok\":false}".to_string())
}

/// Newest `db/filesystem/tsk-inspection-*.json` summary — carries the
/// image path and auto-selected partition offset the recover step needs.
pub(crate) fn newest_tsk_inspection(case_dir: &Path) -> Option<(PathBuf, u64)> {
    let path = std::fs::read_dir(case_dir.join("db/filesystem"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tsk-inspection-") && name.ends_with(".json"))
        })
        .max()?;
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let image = value.get("image_path")?.as_str()?;
    let offset = value.get("partition_offset")?.as_u64()?;
    Some((PathBuf::from(image), offset))
}
