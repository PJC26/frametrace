//! Paged record APIs (/api/records, /api/records-meta) and filesystem-inspection lookup.

use super::*;
use std::collections::{HashMap, VecDeque};

/// PERF-07: one pre-lowercased record, so a search does no per-request
/// `to_lowercase` over every field of every record.
pub(crate) struct SearchRow {
    id: Option<String>,
    fields: Vec<String>,
}

struct SearchIndexEntry {
    case_dir: PathBuf,
    generation: u64,
    overlaid: bool,
    rows: Arc<Vec<SearchRow>>,
}

static SEARCH_INDEX_CACHE: Mutex<Option<SearchIndexEntry>> = Mutex::new(None);

const SEARCH_FIELDS: [&str; 5] = [
    "id",
    "relative_path",
    "source_path",
    "name",
    "original_name",
];

fn build_search_rows(videos: &[serde_json::Value]) -> Vec<SearchRow> {
    videos
        .iter()
        .map(|video| SearchRow {
            id: video.get("id").and_then(|v| v.as_str()).map(str::to_string),
            fields: SEARCH_FIELDS
                .iter()
                .filter_map(|key| video.get(*key).and_then(|v| v.as_str()))
                .map(str::to_lowercase)
                .collect(),
        })
        .collect()
}

/// Lowercased search rows for `videos`, reused while the index generation
/// (index file or live checkpoint changed) is unchanged. Holds one entry —
/// the current case — so memory is bounded by a single index.
pub(crate) fn cached_search_rows(
    case_dir: &Path,
    generation: u64,
    overlaid: bool,
    videos: &[serde_json::Value],
) -> Arc<Vec<SearchRow>> {
    let mut guard = SEARCH_INDEX_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = guard.as_ref()
        && entry.case_dir == case_dir
        && entry.generation == generation
        && entry.overlaid == overlaid
        && entry.rows.len() == videos.len()
    {
        return Arc::clone(&entry.rows);
    }
    let rows = Arc::new(build_search_rows(videos));
    *guard = Some(SearchIndexEntry {
        case_dir: case_dir.to_path_buf(),
        generation,
        overlaid,
        rows: Arc::clone(&rows),
    });
    rows
}

type FileStamp = Option<(SystemTime, u64)>;

struct FtsCacheEntry {
    case_dir: PathBuf,
    query: String,
    stamp: (FileStamp, FileStamp),
    hit: Option<Arc<crate::case_db::FtsSearchHit>>,
}

/// Each entry holds the full indexed-id set, so keep only a few queries.
pub(crate) const FTS_CACHE_CAPACITY: usize = 8;
static FTS_CACHE: Mutex<VecDeque<FtsCacheEntry>> = Mutex::new(VecDeque::new());

#[cfg(test)]
pub(crate) fn records_fts_cache_len() -> usize {
    FTS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
}

fn file_stamp(path: &Path) -> FileStamp {
    path.metadata()
        .ok()
        .map(|meta| (meta.modified().unwrap_or(UNIX_EPOCH), meta.len()))
}

/// The case DB changes through the main file or (WAL mode) its `-wal`
/// sidecar, so both feed the cache key.
fn case_db_stamp(case_dir: &Path) -> (FileStamp, FileStamp) {
    let db = crate::case_db::case_db_path(case_dir);
    let mut wal = db.clone().into_os_string();
    wal.push("-wal");
    (file_stamp(&db), file_stamp(Path::new(&wal)))
}

/// [`crate::case_db::fts_search_video_ids`] memoized by (case, query, DB
/// stamp), LRU-bounded. Errors are not cached.
pub(crate) fn cached_fts_search(
    case_dir: &Path,
    query: &str,
) -> Option<Arc<crate::case_db::FtsSearchHit>> {
    let stamp = case_db_stamp(case_dir);
    {
        let mut cache = FTS_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(position) = cache
            .iter()
            .position(|e| e.case_dir == case_dir && e.query == query && e.stamp == stamp)
        {
            let entry = cache.remove(position)?;
            let hit = entry.hit.clone();
            cache.push_back(entry);
            return hit;
        }
    }
    let hit = crate::case_db::fts_search_video_ids(case_dir, query)
        .ok()?
        .map(Arc::new);
    let mut cache = FTS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|e| !(e.case_dir == case_dir && e.query == query));
    cache.push_back(FtsCacheEntry {
        case_dir: case_dir.to_path_buf(),
        query: query.to_string(),
        stamp,
        hit: hit.clone(),
    });
    while cache.len() > FTS_CACHE_CAPACITY {
        cache.pop_front();
    }
    hit
}

struct ThumbListing {
    case_dir: PathBuf,
    dir_mtime: SystemTime,
    stems: Arc<Vec<String>>,
}

static THUMB_LISTING_CACHE: Mutex<Option<ThumbListing>> = Mutex::new(None);

/// Sorted `review/thumbs/*.jpg` stems, cached by the directory's mtime
/// (PERF-05). A listing taken within 2 s of the directory's last change is
/// not cached: filesystems with coarse mtime could otherwise hide a file
/// added in the same tick.
pub(crate) fn cached_thumb_stems(case_dir: &Path) -> Arc<Vec<String>> {
    let dir = case_dir.join("review/thumbs");
    let mtime = dir.metadata().ok().and_then(|m| m.modified().ok());
    if let Some(mtime) = mtime
        && let Some(entry) = THUMB_LISTING_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        && entry.case_dir == case_dir
        && entry.dir_mtime == mtime
    {
        return Arc::clone(&entry.stems);
    }
    let mut stems: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jpg")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                stems.push(stem.to_string());
            }
        }
    }
    stems.sort();
    let stems = Arc::new(stems);
    let settled = mtime.is_some_and(|m| {
        SystemTime::now()
            .duration_since(m)
            .is_ok_and(|age| age > Duration::from_secs(2))
    });
    if let (Some(mtime), true) = (mtime, settled) {
        *THUMB_LISTING_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ThumbListing {
            case_dir: case_dir.to_path_buf(),
            dir_mtime: mtime,
            stems: Arc::clone(&stems),
        });
    }
    stems
}

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

/// Path of the in-flight scan checkpoint (`scan_folder` appends one
/// verbatim record line per processed file; deleted when the run ends).
pub(crate) fn scan_progress_path(case_dir: &Path) -> PathBuf {
    case_dir.join("db/scan-progress.jsonl")
}

/// Current index generation for `case_dir` (UX-02). Stats the index and
/// the live checkpoint and bumps `JobState::index_generation` whenever
/// either changed since the last call, so the counter only ever grows and
/// any reader comparing two values sees "changed" exactly when the data
/// a live page would return changed. The state lock is held only for the
/// compare-and-bump, never across filesystem calls.
pub(crate) fn index_generation(state: &SharedState, case_dir: &Path) -> u64 {
    let stat = |path: PathBuf| {
        path.metadata()
            .ok()
            .filter(|meta| meta.is_file())
            .map(|meta| (meta.modified().unwrap_or(UNIX_EPOCH), meta.len()))
    };
    let fingerprint = IndexFingerprint {
        case_dir: case_dir.to_path_buf(),
        index: stat(case_dir.join("db/video_index.json")),
        progress: stat(scan_progress_path(case_dir)),
    };
    let mut guard = state_lock(state);
    if guard.index_fingerprint.as_ref() != Some(&fingerprint) {
        guard.index_generation = guard.index_generation.wrapping_add(1);
        guard.index_fingerprint = Some(fingerprint);
    }
    guard.index_generation
}

/// Records from a live scan checkpoint. The header line (it carries a
/// `checkpoint` key) is skipped; a line that does not parse — normally
/// only the torn last line of a write in progress — is counted, never
/// silently dropped, so the caller can report it.
pub(crate) fn read_live_checkpoint(case_dir: &Path) -> (Vec<serde_json::Value>, usize) {
    let Ok(text) = crate::util::read_to_string(&scan_progress_path(case_dir)) else {
        return (Vec::new(), 0);
    };
    let mut records = Vec::new();
    let mut skipped = 0usize;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(value) if value.get("checkpoint").is_some() => {}
            Ok(value) if value.is_object() && value.get("id").is_some() => records.push(value),
            _ => skipped += 1,
        }
    }
    (records, skipped)
}

/// [`read_live_checkpoint`] memoized by path+mtime+len (NICE): a poll
/// against an unchanged checkpoint costs one `stat` and an `Arc` clone.
/// Like [`cached_index`], the state lock is never held while reading.
pub(crate) fn cached_live_checkpoint(
    state: &SharedState,
    case_dir: &Path,
) -> (Arc<Vec<serde_json::Value>>, usize) {
    let path = scan_progress_path(case_dir);
    let Ok(meta) = path.metadata() else {
        return (Arc::new(Vec::new()), 0);
    };
    let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
    let len = meta.len();
    if let Some(cache) = &state_lock(state).live_cache
        && cache.path == path
        && cache.mtime == mtime
        && cache.len == len
    {
        return (Arc::clone(&cache.records), cache.skipped);
    }
    let (records, skipped) = read_live_checkpoint(case_dir);
    let records = Arc::new(records);
    state_lock(state).live_cache = Some(LiveCache {
        path,
        mtime,
        len,
        records: Arc::clone(&records),
        skipped,
    });
    (records, skipped)
}

/// Overlays live checkpoint records on the index list: a record whose id
/// (or, failing that, source path) is already indexed replaces it in
/// place — order and offsets stay stable for the pager — and unknown
/// records (a first scan still running) are appended in checkpoint order.
pub(crate) fn overlay_live_records(
    base: &[serde_json::Value],
    live: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let key_of = |video: &serde_json::Value, field: &str| {
        video
            .get(field)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let mut merged: Vec<serde_json::Value> = base.to_vec();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut by_path: HashMap<String, usize> = HashMap::new();
    for (position, video) in merged.iter().enumerate() {
        if let Some(id) = key_of(video, "id") {
            by_id.insert(id, position);
        }
        if let Some(path) = key_of(video, "source_path") {
            by_path.insert(path, position);
        }
    }
    for record in live {
        let position = key_of(&record, "id")
            .and_then(|id| by_id.get(&id).copied())
            .or_else(|| key_of(&record, "source_path").and_then(|p| by_path.get(&p).copied()));
        match position {
            Some(position) => merged[position] = record,
            None => {
                let position = merged.len();
                if let Some(id) = key_of(&record, "id") {
                    by_id.insert(id, position);
                }
                if let Some(path) = key_of(&record, "source_path") {
                    by_path.insert(path, position);
                }
                merged.push(record);
            }
        }
    }
    merged
}

pub(crate) fn api_records(request: &Request, state: &SharedState) -> String {
    // A recover/carve job keeps the review phase (it may be
    // `review-partial`) but is not the scan pipeline: its busy never makes
    // a leftover checkpoint live.
    let (case_dir, phase, busy) = {
        let guard = state_lock(state);
        let case_job_running = guard
            .case_job
            .as_ref()
            .is_some_and(|job| job.state == "running");
        (
            guard.case_dir.clone(),
            guard.phase,
            guard.busy && !case_job_running,
        )
    };
    let Some(case_dir) = case_dir else {
        return "{\"ok\":false,\"error\":\"no case open\"}".to_string();
    };
    // UX-02: `live=1` while the pipeline is still producing data overlays
    // the scan checkpoint on the (possibly hash-less) index so the viewer
    // sees enriched records as they land. Outside those phases the
    // checkpoint is a leftover of an interrupted run and is ignored —
    // including in `review-partial` without an active job (an index-only
    // case opened via /api/open-case, or phase 2 cancelled/failed): a
    // dead run's checkpoint must not be presented as live data.
    let live_phase = busy && matches!(phase, "running" | "review-partial");
    let live = live_phase && query_value(&request.query, "live").as_deref() == Some("1");
    let generation = index_generation(state, &case_dir);
    // Index-only version for the strict (non-live) pager: a restart is
    // warranted when the index file itself changed, not merely because a
    // checkpoint line was appended.
    let etag = case_dir
        .join("db/video_index.json")
        .metadata()
        .ok()
        .map(|meta| {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{mtime}-{}", meta.len())
        });
    let index = match cached_index(state, &case_dir) {
        Ok(index) => Some(index),
        // A first scan still running has no index yet — the checkpoint
        // alone is the live list.
        Err(_) if live && scan_progress_path(&case_dir).is_file() => None,
        Err(err) => return api_err(err),
    };
    let empty = Vec::new();
    let base = index
        .as_ref()
        .and_then(|index| index.get("videos"))
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let (live_records, live_skipped) = if live {
        cached_live_checkpoint(state, &case_dir)
    } else {
        (Arc::new(Vec::new()), 0)
    };
    let live_count = live_records.len();
    let overlaid;
    let videos: &Vec<serde_json::Value> = if live && live_count > 0 {
        overlaid = overlay_live_records(base, live_records.iter().cloned().collect());
        &overlaid
    } else {
        base
    };
    let query = query_value(&request.query, "q").map(|q| q.to_lowercase());
    // FTS5 trigram index accelerates the same substring contract for
    // DB-indexed records; `None` (no db, short query, or any error)
    // falls back to the full substring scan rather than returning an
    // honest-but-wrong empty set. Results are cached per (query, DB
    // stamp) so paging through a result set does not reopen the DB.
    let fts = query
        .as_deref()
        .and_then(|q| cached_fts_search(&case_dir, q));
    let rows = query
        .as_ref()
        .map(|_| cached_search_rows(&case_dir, generation, live && live_count > 0, videos));
    let matched_index = |position: usize| -> bool {
        let (Some(q), Some(rows)) = (query.as_deref(), rows.as_ref()) else {
            return true;
        };
        let row = &rows[position];
        let substring_match = || row.fields.iter().any(|text| text.contains(q));
        if let Some(hit) = &fts {
            let (hits, indexed) = &**hit;
            return match row.id.as_deref() {
                Some(id) if hits.contains(id) => true,
                // Indexed in the db but not in FTS results = no match;
                // anything else (carved/filesystem records outside the
                // videos table) still gets the substring contract.
                Some(id) if indexed.contains(id) => false,
                _ => substring_match(),
            };
        }
        substring_match()
    };
    let offset = query_value(&request.query, "offset")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = query_value(&request.query, "limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(500)
        .clamp(1, 5000);
    let filtered: Vec<&serde_json::Value> = videos
        .iter()
        .enumerate()
        .filter(|(position, _)| matched_index(*position))
        .map(|(_, video)| video)
        .collect();
    let total = filtered.len();
    let page: Vec<serde_json::Value> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    serde_json::json!({
        "ok": true,
        "total": total,
        "offset": offset,
        "videos": page,
        "generation": generation,
        "etag": etag,
        "live": live,
        "phase": phase,
        "live_records": live_count,
        "live_skipped": live_skipped,
    })
    .to_string()
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
    for stem in cached_thumb_stems(&case_dir).iter() {
        thumbs.insert(
            stem.clone(),
            serde_json::Value::String(format!("thumbs/{stem}.jpg")),
        );
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
