/// Appends or replaces one top-level scalar field on a serialized JSON object
/// through raw text edits only, so existing byte-stable records keep their
/// field order and spelling.
///
/// `#[doc(hidden)]`: exposed so the fuzz harness can hit this hand-rolled
/// editor with arbitrary input; not part of the supported API.
#[doc(hidden)]
pub fn set_json_field(line: &str, key: &str, value: &str) -> String {
    if find_top_level_key(line, key).is_some() {
        replace_json_field(line, key, value)
    } else {
        insert_json_field(line, key, value)
    }
}

/// Byte range of `"key":<value>` at JSON depth 1 (top-level object members
/// only). Nested occurrences — e.g. a metadata tag named `index_status`
/// inside the inlined ffprobe object — are deliberately not matched, so
/// stale markers can never rewrite recorded probe evidence.
pub(super) fn find_top_level_key(line: &str, key: &str) -> Option<std::ops::Range<usize>> {
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

/// Consumes one scalar JSON value (string, number, bool, null) from the
/// start of `text` and returns its raw serialized form.
pub(super) fn extract_scalar_value(text: &str) -> Option<String> {
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

pub(super) fn replace_json_field(line: &str, key: &str, value: &str) -> String {
    let Some(range) = find_top_level_key(line, key) else {
        return insert_json_field(line, key, value);
    };
    format!("{}{}{}", &line[..range.start], value, &line[range.end..])
}

pub(super) fn insert_json_field(line: &str, key: &str, value: &str) -> String {
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
