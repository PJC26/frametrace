/// Sets one top-level field on a serialized JSON object record. `value` is a
/// raw JSON fragment (e.g. `"\"stale\""` or `"9"`). The record is parsed with
/// serde_json (`preserve_order`), so existing key order is kept, a nested key
/// with the same name is never touched, and an existing top-level key is
/// replaced in place. Input that is not a JSON object, or a `value` that is
/// not valid JSON, is returned unchanged.
///
/// `#[doc(hidden)]`: exposed so the fuzz harness can hit this editor with
/// arbitrary input; not part of the supported API.
#[doc(hidden)]
pub fn set_json_field(line: &str, key: &str, value: &str) -> String {
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str::<serde_json::Value>(line)
    else {
        return line.to_string();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) else {
        return line.to_string();
    };
    map.insert(key.to_string(), parsed);
    serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| line.to_string())
}

/// Splits index text into top-level JSON object records. The splitter is
/// hand-rolled (depth + string/escape tracking), so it is exposed for the
/// fuzz harness; not part of the supported API.
#[doc(hidden)]
pub fn json_record_lines(text: &str) -> Vec<String> {
    let mut records = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for ch in text.chars() {
        if depth > 0 {
            current.push(ch);
        } else if ch.is_whitespace() {
            continue;
        } else {
            current.push(ch);
        }

        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && !current.trim().is_empty() {
                    records.push(current.trim().to_string());
                    current.clear();
                }
            }
            _ => {}
        }
    }

    if !current.trim().is_empty() {
        records.push(current.trim().to_string());
    }
    records
}

pub(super) fn parse_usize(value: &str) -> Option<usize> {
    value.parse::<usize>().ok()
}

/// Escapes a value for the tab-separated `db/video_paths.tsv` format.
/// `pub(crate)` so case_merge can emit rows in the same shape.
pub(crate) fn tsv_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::set_json_field;

    // Reference oracle: the previous hand-rolled text splicer, kept only to
    // prove the serde_json replacement is equivalent on tricky inputs.
    fn old_set_json_field(line: &str, key: &str, value: &str) -> String {
        if find_top_level_key(line, key).is_some() {
            replace_json_field(line, key, value)
        } else {
            insert_json_field(line, key, value)
        }
    }

    fn find_top_level_key(line: &str, key: &str) -> Option<std::ops::Range<usize>> {
        let bytes = line.as_bytes();
        let needle = format!("\"{key}\":");
        let needle = needle.as_bytes();
        let mut depth: usize = 0;
        let mut in_string = false;
        let mut escaped = false;
        let mut index = 0usize;

        while index < bytes.len() {
            let ch = bytes[index];
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == b'\\' {
                    escaped = true;
                } else if ch == b'"' {
                    in_string = false;
                }
                index += 1;
                continue;
            }
            match ch {
                b'"' => in_string = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
            // A member key starts at depth 1 right after a `{` or `,`.
            if depth == 1
                && (bytes[..=index].ends_with(b"{") || bytes[..=index].ends_with(b","))
                && bytes[index + 1..].starts_with(needle)
            {
                let value_start = index + 1 + needle.len();
                let value = extract_scalar_value(&line[value_start..])?;
                let range = value_start..value_start + value.len();
                return Some(range);
            }
            index += 1;
        }
        None
    }

    fn extract_scalar_value(text: &str) -> Option<String> {
        let trimmed = text.trim_start();
        let offset = text.len() - trimmed.len();
        let mut chars = trimmed.chars();
        match chars.next()? {
            '"' => {
                let mut out = String::from("\"");
                let mut escaped = false;
                for ch in trimmed[1..].chars() {
                    out.push(ch);
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        return Some(format!("{}{}", &text[..offset], out));
                    }
                }
                None
            }
            first if first == '-' || first.is_ascii_digit() => {
                let raw: String = std::iter::once(first)
                    .chain(chars.take_while(|ch| {
                        ch.is_ascii_digit() || matches!(ch, '.' | 'e' | 'E' | '+' | '-')
                    }))
                    .collect();
                (!raw.is_empty()).then(|| format!("{}{}", &text[..offset], raw))
            }
            't' if trimmed.starts_with("true") => Some(format!("{}true", &text[..offset])),
            'f' if trimmed.starts_with("false") => Some(format!("{}false", &text[..offset])),
            'n' if trimmed.starts_with("null") => Some(format!("{}null", &text[..offset])),
            _ => None,
        }
    }

    fn replace_json_field(line: &str, key: &str, value: &str) -> String {
        let Some(range) = find_top_level_key(line, key) else {
            return insert_json_field(line, key, value);
        };
        format!("{}{}{}", &line[..range.start], value, &line[range.end..])
    }

    fn insert_json_field(line: &str, key: &str, value: &str) -> String {
        let trimmed = line.trim_end();
        let Some(close_index) = trimmed.rfind('}') else {
            return line.to_string();
        };
        let prefix = &trimmed[..close_index];
        let suffix = &trimmed[close_index..];
        let separator = if prefix.trim_end().ends_with('{') {
            ""
        } else {
            ","
        };
        format!("{prefix}{separator}\"{key}\":{value}{suffix}")
    }

    #[test]
    fn serde_editor_matches_old_text_splicer() {
        let cases: &[(&str, &str, &str)] = &[
            (r#"{"id":"a","n":1}"#, "index_status", "\"stale\""),
            (
                r#"{"id":"a","index_status":"fresh","n":1}"#,
                "index_status",
                "\"stale\"",
            ),
            (r#"{"a":"\"x\" \\ 한글","b":2}"#, "k", "7"),
            (
                r#"{"name":"한글 \"index_status\":\"x\"","z":1}"#,
                "index_status",
                "\"stale\"",
            ),
            (
                r#"{"s":"{\"index_status\":\"y\"}","t":3}"#,
                "index_status",
                "\"stale\"",
            ),
            (
                r#"{"probe":{"index_status":"deep","x":[1,{"index_status":2}]},"id":"q"}"#,
                "index_status",
                "\"stale\"",
            ),
            (r#"{"a":1,"stale_since_unix":5}"#, "stale_since_unix", "9"),
            (
                r#"{"a":"back\\","stale_since_unix":5,"b":null}"#,
                "stale_since_unix",
                "10",
            ),
            (r#"{}"#, "k", "true"),
            (r#"{"a":true,"k":false}"#, "k", "null"),
        ];
        for (line, key, value) in cases {
            assert_eq!(
                set_json_field(line, key, value),
                old_set_json_field(line, key, value),
                "line={line} key={key}"
            );
        }
    }

    #[test]
    fn set_json_field_is_safe_on_non_objects() {
        assert_eq!(set_json_field("not json", "k", "1"), "not json");
        assert_eq!(set_json_field("[1]", "k", "1"), "[1]");
        assert_eq!(set_json_field("{\"a\":1}", "k", "{bad"), "{\"a\":1}");
    }

    #[test]
    fn replaces_non_scalar_top_level_value_without_duplicating_key() {
        let out = set_json_field(r#"{"k":{"x":1},"a":2}"#, "k", "3");
        assert_eq!(out, r#"{"k":3,"a":2}"#);
    }
}
