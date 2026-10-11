use crate::util::{compact_json_value_if_well_formed, json_escape, path_to_file_url};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CaseManifest {
    pub schema_version: u32,
    pub case_id: String,
    pub title: String,
    pub created_unix: u64,
    pub tool_name: String,
    pub tool_version: String,
    pub platform: String,
    pub operator: Option<String>,
    pub host: Option<String>,
    pub device_id: Option<String>,
    pub device_serial: Option<String>,
    pub write_protect: Option<String>,
    pub acquisition_tool: Option<String>,
    pub evidence_hash: Option<String>,
    pub notes: Option<String>,
}

impl CaseManifest {
    pub fn to_json(&self) -> String {
        format!(
            "{{\n  \"schema_version\": {},\n  \"case_id\": \"{}\",\n  \"title\": \"{}\",\n  \"created_unix\": {},\n  \"tool_name\": \"{}\",\n  \"tool_version\": \"{}\",\n  \"platform\": \"{}\",\n  \"operator\": {},\n  \"host\": {},\n  \"device_id\": {},\n  \"device_serial\": {},\n  \"write_protect\": {},\n  \"acquisition_tool\": {},\n  \"evidence_hash\": {},\n  \"notes\": {}\n}}\n",
            self.schema_version,
            json_escape(&self.case_id),
            json_escape(&self.title),
            self.created_unix,
            json_escape(&self.tool_name),
            json_escape(&self.tool_version),
            json_escape(&self.platform),
            optional_string_json(self.operator.as_deref()),
            optional_string_json(self.host.as_deref()),
            optional_string_json(self.device_id.as_deref()),
            optional_string_json(self.device_serial.as_deref()),
            optional_string_json(self.write_protect.as_deref()),
            optional_string_json(self.acquisition_tool.as_deref()),
            optional_string_json(self.evidence_hash.as_deref()),
            optional_string_json(self.notes.as_deref())
        )
    }
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub hash_files: bool,
    pub use_ffprobe: bool,
    pub max_depth: Option<usize>,
    /// Rescan mode: files whose indexed size+mtime still match are reused
    /// verbatim (no re-hash, no re-probe); only new/changed files are
    /// processed and missing paths are marked stale.
    pub incremental: bool,
    /// Screen each indexed file through the deepfake-lens sidecar and
    /// write artifacts/deepfake/<id>.json. Opt-in: neural members make
    /// per-file screening slow on CPU-only workstations.
    pub deepfake_screen: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            hash_files: false,
            use_ffprobe: true,
            max_depth: None,
            incremental: false,
            deepfake_screen: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeSummary {
    pub ok: bool,
    pub raw_json: Option<String>,
    pub error: Option<String>,
    pub duration_seconds: Option<f64>,
    pub format_name: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Frames per second of the first video stream (`avg_frame_rate`, falling
    /// back to `r_frame_rate`); `None` for legacy records or when ffprobe
    /// reported none / `0/0`.
    #[serde(default)]
    pub fps: Option<f64>,
}

impl ProbeSummary {
    pub fn skipped() -> Self {
        Self {
            ok: false,
            raw_json: None,
            error: Some("ffprobe skipped".to_string()),
            duration_seconds: None,
            format_name: None,
            video_codec: None,
            audio_codec: None,
            width: None,
            height: None,
            fps: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceProfile {
    pub lane: String,
    pub vendor: String,
    pub parser: String,
    pub confidence: String,
    pub recommended_action: String,
    pub evidence: Vec<String>,
}

impl SourceProfile {
    pub fn generic_media(evidence: impl Into<String>) -> Self {
        Self {
            lane: "generic-video".to_string(),
            vendor: "Generic media".to_string(),
            parser: "generic_media".to_string(),
            confidence: "medium".to_string(),
            recommended_action: "Use ffprobe/FFmpeg first; preserve original and export derived clips only when requested.".to_string(),
            evidence: vec![evidence.into()],
        }
    }

    pub fn to_json(&self) -> String {
        let evidence = self
            .evidence
            .iter()
            .map(|item| format!("\"{}\"", json_escape(item)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"lane\":\"{}\",\"vendor\":\"{}\",\"parser\":\"{}\",\"confidence\":\"{}\",\"recommended_action\":\"{}\",\"evidence\":[{}]}}",
            json_escape(&self.lane),
            json_escape(&self.vendor),
            json_escape(&self.parser),
            json_escape(&self.confidence),
            json_escape(&self.recommended_action),
            evidence
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoRecord {
    pub id: String,
    #[serde(with = "lossy_path")]
    pub source_path: PathBuf,
    pub relative_path: String,
    pub extension: String,
    pub size_bytes: u64,
    pub modified_unix: Option<u64>,
    pub sha256: Option<String>,
    pub hash_status: String,
    /// The published JSONL contract flattens the probe summary
    /// (`ffprobe_ok`, `ffprobe_error`, `ffprobe`, `duration_seconds`,
    /// `format_name`, `video_codec`, `audio_codec`, `width`, `height`)
    /// instead of nesting a `probe` object, so the in-memory field is
    /// flattened through this proxy for compatibility.
    #[serde(flatten, with = "probe_summary_flat")]
    pub probe: ProbeSummary,
    pub confidence: String,
    pub source_profile: SourceProfile,
}

impl VideoRecord {
    pub fn to_json(&self) -> String {
        let source_path = self.source_path.to_string_lossy();
        let file_url = path_to_file_url(&self.source_path);
        let sha256 = optional_string_json(self.sha256.as_deref());
        let modified_unix = optional_u64_json(self.modified_unix);
        let duration = optional_f64_json(self.probe.duration_seconds);
        let width = optional_u32_json(self.probe.width);
        let height = optional_u32_json(self.probe.height);
        // Emitted only when known, so records without fps stay byte-identical
        // to what older binaries wrote.
        let fps = match self
            .probe
            .fps
            .filter(|value| value.is_finite() && *value > 0.0)
        {
            Some(value) => format!(",\"fps\":{value:.3}"),
            None => String::new(),
        };
        let ffprobe_json = match &self.probe.raw_json {
            Some(raw) => {
                compact_json_value_if_well_formed(raw).unwrap_or_else(|| "null".to_string())
            }
            None => "null".to_string(),
        };
        format!(
            "{{\"id\":\"{}\",\"source_path\":\"{}\",\"file_url\":\"{}\",\"relative_path\":\"{}\",\"extension\":\"{}\",\"size_bytes\":{},\"modified_unix\":{},\"sha256\":{},\"hash_status\":\"{}\",\"confidence\":\"{}\",\"source_profile\":{},\"duration_seconds\":{},\"format_name\":{},\"video_codec\":{},\"audio_codec\":{},\"width\":{},\"height\":{}{},\"ffprobe_ok\":{},\"ffprobe_error\":{},\"ffprobe\":{}}}",
            json_escape(&self.id),
            json_escape(&source_path),
            json_escape(&file_url),
            json_escape(&self.relative_path),
            json_escape(&self.extension),
            self.size_bytes,
            modified_unix,
            sha256,
            json_escape(&self.hash_status),
            json_escape(&self.confidence),
            self.source_profile.to_json(),
            duration,
            optional_string_json(self.probe.format_name.as_deref()),
            optional_string_json(self.probe.video_codec.as_deref()),
            optional_string_json(self.probe.audio_codec.as_deref()),
            width,
            height,
            fps,
            self.probe.ok,
            optional_string_json(self.probe.error.as_deref()),
            ffprobe_json
        )
    }

    pub fn to_tsv_row(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            self.id,
            self.source_path.to_string_lossy(),
            self.relative_path,
            self.extension,
            self.size_bytes,
            self.sha256.as_deref().unwrap_or(""),
            self.source_profile.vendor,
            self.source_profile.parser,
            self.source_profile.confidence
        )
    }
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub source_path: PathBuf,
    pub scanned_unix: u64,
    pub video_count: usize,
    pub total_bytes: u64,
    /// Files an incremental rescan left untouched because their indexed
    /// size+mtime still matched (0 for full scans).
    pub unchanged_files: usize,
    /// Records replayed verbatim from an interrupted run's checkpoint
    /// instead of being re-hashed/re-probed (0 unless resuming).
    pub resumed_from_checkpoint: usize,
    pub warnings: Vec<String>,
    pub options: ScanOptions,
    pub records: Vec<VideoRecord>,
}

impl ScanResult {
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        out.push_str("  \"schema_version\": 1,\n");
        out.push_str(&format!(
            "  \"source_path\": \"{}\",\n",
            json_escape(&self.source_path.to_string_lossy())
        ));
        out.push_str(&format!("  \"scanned_unix\": {},\n", self.scanned_unix));
        out.push_str(&format!("  \"video_count\": {},\n", self.video_count));
        out.push_str(&format!("  \"total_bytes\": {},\n", self.total_bytes));
        out.push_str(&format!(
            "  \"unchanged_files\": {},\n",
            self.unchanged_files
        ));
        out.push_str(&format!(
            "  \"resumed_from_checkpoint\": {},\n",
            self.resumed_from_checkpoint
        ));
        out.push_str("  \"warnings\": [\n");
        for (index, warning) in self.warnings.iter().enumerate() {
            out.push_str(&format!("    \"{}\"", json_escape(warning)));
            if index + 1 != self.warnings.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ],\n");
        out.push_str("  \"options\": {\n");
        out.push_str(&format!(
            "    \"hash_files\": {},\n",
            self.options.hash_files
        ));
        out.push_str(&format!(
            "    \"use_ffprobe\": {},\n",
            self.options.use_ffprobe
        ));
        out.push_str(&format!(
            "    \"incremental\": {},\n",
            self.options.incremental
        ));
        match self.options.max_depth {
            Some(max_depth) => out.push_str(&format!("    \"max_depth\": {}\n", max_depth)),
            None => out.push_str("    \"max_depth\": null\n"),
        }
        out.push_str("  },\n");
        out.push_str("  \"videos\": [\n");
        for (index, record) in self.records.iter().enumerate() {
            out.push_str("    ");
            out.push_str(&record.to_json());
            if index + 1 != self.records.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n");
        out.push_str("}\n");
        out
    }
}

fn optional_string_json(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", json_escape(value)),
        None => "null".to_string(),
    }
}

fn optional_u64_json(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn optional_u32_json(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn optional_f64_json(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.3}"))
        .unwrap_or_else(|| "null".to_string())
}

/// PathBuf serializes as a structured string on some platforms, so the
/// record JSON contract (a plain string field) needs a lossy flat string.
mod lossy_path {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::path::{Path, PathBuf};

    pub fn serialize<S: Serializer>(value: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string_lossy())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        let text = <String>::deserialize(deserializer)?;
        Ok(PathBuf::from(text))
    }
}

/// Flattens `ProbeSummary` under the published field names. The nested
/// in-memory field `raw_json` maps to the contract's `ffprobe` key, and
/// `ok`/`error` map to `ffprobe_ok`/`ffprobe_error`, so a plain
/// `#[serde(flatten)]` would produce the wrong shape.
mod probe_summary_flat {
    use super::ProbeSummary;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct FlatProbe {
        duration_seconds: Option<f64>,
        format_name: Option<String>,
        video_codec: Option<String>,
        audio_codec: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<f64>,
        ffprobe_ok: bool,
        ffprobe_error: Option<String>,
        /// The published contract inlines the raw ffprobe JSON object here,
        /// but legacy writers may also emit a pre-escaped string or null, so
        /// both shapes are accepted on read.
        #[serde(default, deserialize_with = "deserialize_raw_ffprobe")]
        ffprobe: Option<String>,
    }

    fn deserialize_raw_ffprobe<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<String>, D::Error> {
        let value = Option::<serde_json::Value>::deserialize(deserializer)?;
        Ok(match value {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(text)) => Some(text),
            Some(other) => Some(other.to_string()),
        })
    }

    pub fn serialize<S: Serializer>(
        probe: &ProbeSummary,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        FlatProbe {
            duration_seconds: probe.duration_seconds,
            format_name: probe.format_name.clone(),
            video_codec: probe.video_codec.clone(),
            audio_codec: probe.audio_codec.clone(),
            width: probe.width,
            height: probe.height,
            fps: probe.fps,
            ffprobe_ok: probe.ok,
            ffprobe_error: probe.error.clone(),
            ffprobe: probe.raw_json.clone(),
        }
        .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ProbeSummary, D::Error> {
        let flat = FlatProbe::deserialize(deserializer)?;
        Ok(ProbeSummary {
            ok: flat.ffprobe_ok,
            raw_json: flat.ffprobe,
            error: flat.ffprobe_error,
            duration_seconds: flat.duration_seconds,
            format_name: flat.format_name,
            video_codec: flat.video_codec,
            audio_codec: flat.audio_codec,
            width: flat.width,
            height: flat.height,
            fps: flat.fps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ProbeSummary, SourceProfile, VideoRecord};
    use std::path::PathBuf;

    fn sample_record() -> VideoRecord {
        VideoRecord {
            id: "vid_000001".to_string(),
            source_path: PathBuf::from(r"\\?\C:\evidence\카메라.mp4"),
            relative_path: "카메라.mp4".to_string(),
            extension: "mp4".to_string(),
            size_bytes: 12345,
            modified_unix: Some(1700000000),
            sha256: Some("abc".to_string()),
            hash_status: "complete".to_string(),
            probe: ProbeSummary {
                ok: true,
                raw_json: Some(r#"{"streams":[]}"#.to_string()),
                error: None,
                duration_seconds: Some(12.5),
                format_name: Some("mov,mp4".to_string()),
                video_codec: Some("h264".to_string()),
                audio_codec: None,
                width: Some(1920),
                height: Some(1080),
                fps: None,
            },
            confidence: "ffprobe-confirmed".to_string(),
            source_profile: SourceProfile::generic_media("extension"),
        }
    }

    /// The JSONL compatibility artifacts are the engine's published contract:
    /// legacy records written by older binaries must still deserialize.
    #[test]
    fn legacy_jsonl_line_round_trips_through_serde() {
        let legacy_line = r#"{"id":"vid_000001","source_path":"C:\\Evidence\\a.mp4","file_url":"file:///C:/Evidence/a.mp4","relative_path":"a.mp4","extension":"mp4","size_bytes":1,"modified_unix":null,"sha256":null,"hash_status":"skipped","confidence":"extension-candidate","source_profile":{"lane":"generic-video","vendor":"Generic media","parser":"generic_media","confidence":"medium","recommended_action":"Use ffprobe first","evidence":["extension"]},"duration_seconds":null,"format_name":null,"video_codec":null,"audio_codec":null,"width":null,"height":null,"ffprobe_ok":false,"ffprobe_error":"ffprobe skipped","ffprobe":null}"#;
        let record: VideoRecord = serde_json::from_str(legacy_line).expect("legacy line parses");
        assert_eq!(record.id, "vid_000001");
        assert!(!record.probe.ok);
        assert_eq!(record.probe.error.as_deref(), Some("ffprobe skipped"));

        // to_json() output must itself stay parseable (round-trip contract).
        let reparsed: VideoRecord =
            serde_json::from_str(&record.to_json()).expect("to_json output parses");
        assert_eq!(reparsed.id, record.id);
        assert_eq!(reparsed.probe.ok, record.probe.ok);
    }

    /// `fps` is a flat record key when known and absent (not null) otherwise;
    /// both shapes parse back.
    #[test]
    fn fps_is_flat_key_and_round_trips() {
        let mut record = sample_record();
        assert!(!record.to_json().contains("\"fps\""));
        record.probe.fps = Some(29.97);
        let json = record.to_json();
        assert!(
            json.contains(r#""height":1080,"fps":29.970,"ffprobe_ok""#),
            "{json}"
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["fps"], serde_json::json!(29.97));
        let parsed: VideoRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.probe.fps, Some(29.97));
        // serde path (used by case_db/serve) is flat too.
        let via_serde: serde_json::Value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(via_serde["fps"], serde_json::json!(29.97));
        assert!(via_serde.get("probe").is_none());
        let legacy: VideoRecord =
            serde_json::from_str(&record.to_json().replace(",\"fps\":29.970", "")).unwrap();
        assert_eq!(legacy.probe.fps, None);
    }

    /// ffprobe_ok must survive `#[serde(flatten)]` without duplicate keys in
    /// serialized output, and Korean paths must round-trip losslessly.
    #[test]
    fn serde_flatten_emits_flat_keys_and_korean_paths_survive() {
        let record = sample_record();
        let json = record.to_json();
        assert!(json.contains(r#""ffprobe_ok":true"#));
        assert!(
            !json.contains(r#""probe":"#),
            "probe must stay flat: {json}"
        );
        assert!(
            !json.contains(r#""probe":{"#),
            "probe must stay flat: {json}"
        );
        assert!(json.contains("카메라.mp4"));

        let parsed: VideoRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.source_path, record.source_path);
        assert_eq!(parsed.relative_path, record.relative_path);
        assert_eq!(parsed.probe.width, Some(1920));
    }
}
