//! Recording time parsed from file names — a Rust port of the viewer's
//! `TIME_PATTERNS` / `parseTimeFromName` (`assets/viewer/30-records.js`).
//!
//! The four patterns are matched in the same order with the same leftmost,
//! backtracking semantics as the JavaScript regular expressions, so the
//! grid's "recording time" and the join planner never disagree. Parity is
//! pinned by the shared fixture `tests/fixtures/rectime-cases.json`, which
//! both the unit tests here and `scripts/check-rectime-fixture.mjs` read.
//!
//! Values are **naive**: the device wrote a wall-clock time without a zone.
//! `secs_naive` is that wall-clock reading counted as if it were UTC — it is
//! only meaningful for differences between two values of the same basis and
//! must never be presented as an absolute (UTC) instant.

/// A recording time read from a file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NaiveRecTime {
    /// Wall-clock seconds counted as if the naive time were UTC. Day
    /// overflow rolls over exactly like JavaScript's `new Date(y, m, d)`
    /// (Feb 30 → Mar 2), matching the viewer.
    pub secs_naive: i64,
    /// `YYYY-MM-DD HH:MM:SS` (or `YYYY-MM-DD` when `has_time` is false),
    /// built from the matched digits verbatim.
    pub text: String,
    /// `YYYY-MM-DD` from the matched digits (the viewer's `date`).
    pub date: String,
    /// False when only a date matched — the 00:00:00 is synthetic and must
    /// not be used as a recording time or an ordering basis.
    pub has_time: bool,
}

#[derive(Clone, Copy)]
enum Tok {
    /// `20\d{2}`
    Year,
    /// `0[1-9]|1[0-2]`
    Month,
    /// `0[1-9]|[12]\d|3[01]`
    Day,
    /// `[01]\d|2[0-3]`
    Hour,
    /// `[0-5]\d`
    MinSec,
    /// `[set]?` (greedy)
    Opt(&'static str),
    /// `[set]+` (greedy)
    Plus(&'static str),
    /// A literal character.
    Lit(char),
    /// `(?!\d)`
    NotDigit,
}

struct Pattern {
    toks: &'static [Tok],
    /// The JavaScript `/i` flag.
    ci: bool,
    has_time: bool,
}

const DATE_SEP: &str = "_.-";
const DATE_TIME_SEP: &str = " T_-";
const TIME_SEP: &str = ":_.-";

const PATTERNS: [Pattern; 4] = [
    // /(20\d{2})[_.\-]?(0[1-9]|1[0-2])[_.\-]?(0[1-9]|[12]\d|3[01])[ T_\-]+([01]\d|2[0-3])h([0-5]\d)m([0-5]\d)s/i
    Pattern {
        toks: &[
            Tok::Year,
            Tok::Opt(DATE_SEP),
            Tok::Month,
            Tok::Opt(DATE_SEP),
            Tok::Day,
            Tok::Plus(DATE_TIME_SEP),
            Tok::Hour,
            Tok::Lit('h'),
            Tok::MinSec,
            Tok::Lit('m'),
            Tok::MinSec,
            Tok::Lit('s'),
        ],
        ci: true,
        has_time: true,
    },
    // /(20\d{2})[_.\-]?(0[1-9]|1[0-2])[_.\-]?(0[1-9]|[12]\d|3[01])[ T_\-]+([01]\d|2[0-3])[:_.\-]?([0-5]\d)[:_.\-]?([0-5]\d)/
    Pattern {
        toks: &[
            Tok::Year,
            Tok::Opt(DATE_SEP),
            Tok::Month,
            Tok::Opt(DATE_SEP),
            Tok::Day,
            Tok::Plus(DATE_TIME_SEP),
            Tok::Hour,
            Tok::Opt(TIME_SEP),
            Tok::MinSec,
            Tok::Opt(TIME_SEP),
            Tok::MinSec,
        ],
        ci: false,
        has_time: true,
    },
    // /(20\d{2})(0[1-9]|1[0-2])(0[1-9]|[12]\d|3[01])[ T_\-]+([01]\d|2[0-3])([0-5]\d)([0-5]\d)/
    Pattern {
        toks: &[
            Tok::Year,
            Tok::Month,
            Tok::Day,
            Tok::Plus(DATE_TIME_SEP),
            Tok::Hour,
            Tok::MinSec,
            Tok::MinSec,
        ],
        ci: false,
        has_time: true,
    },
    // /(20\d{2})[_.\-]?(0[1-9]|1[0-2])[_.\-]?(0[1-9]|[12]\d|3[01])(?!\d)/
    Pattern {
        toks: &[
            Tok::Year,
            Tok::Opt(DATE_SEP),
            Tok::Month,
            Tok::Opt(DATE_SEP),
            Tok::Day,
            Tok::NotDigit,
        ],
        ci: false,
        has_time: false,
    },
];

fn digit(ch: char) -> bool {
    ch.is_ascii_digit()
}

fn two(chars: &[char], pos: usize, ok: impl Fn(char, char) -> bool) -> bool {
    pos + 2 <= chars.len() && ok(chars[pos], chars[pos + 1])
}

fn in_set(set: &str, ch: char, ci: bool) -> bool {
    // Under `/i` the class `[ T_\-]` also matches `t`; JS non-unicode
    // case folding never maps non-ASCII onto ASCII, so ASCII folding is
    // exact here.
    set.chars().any(|member| {
        member == ch || (ci && member.is_ascii_alphabetic() && member.eq_ignore_ascii_case(&ch))
    })
}

/// Backtracking matcher: returns true when `toks[ti..]` matches at `pos`,
/// recording each fixed-width digit field's start position into `caps`.
fn match_at(
    toks: &[Tok],
    ti: usize,
    chars: &[char],
    pos: usize,
    ci: bool,
    caps: &mut Vec<usize>,
) -> bool {
    let Some(tok) = toks.get(ti) else {
        return true;
    };
    let field = |width: usize, ok: bool, caps: &mut Vec<usize>| -> bool {
        if !ok {
            return false;
        }
        caps.push(pos);
        if match_at(toks, ti + 1, chars, pos + width, ci, caps) {
            return true;
        }
        caps.pop();
        false
    };
    match *tok {
        Tok::Year => field(
            4,
            pos + 4 <= chars.len()
                && chars[pos] == '2'
                && chars[pos + 1] == '0'
                && digit(chars[pos + 2])
                && digit(chars[pos + 3]),
            caps,
        ),
        Tok::Month => field(
            2,
            two(chars, pos, |a, b| {
                (a == '0' && ('1'..='9').contains(&b)) || (a == '1' && ('0'..='2').contains(&b))
            }),
            caps,
        ),
        Tok::Day => field(
            2,
            two(chars, pos, |a, b| {
                (a == '0' && ('1'..='9').contains(&b))
                    || ((a == '1' || a == '2') && digit(b))
                    || (a == '3' && (b == '0' || b == '1'))
            }),
            caps,
        ),
        Tok::Hour => field(
            2,
            two(chars, pos, |a, b| {
                ((a == '0' || a == '1') && digit(b)) || (a == '2' && ('0'..='3').contains(&b))
            }),
            caps,
        ),
        Tok::MinSec => field(
            2,
            two(chars, pos, |a, b| ('0'..='5').contains(&a) && digit(b)),
            caps,
        ),
        Tok::Opt(set) => {
            if pos < chars.len()
                && in_set(set, chars[pos], ci)
                && match_at(toks, ti + 1, chars, pos + 1, ci, caps)
            {
                return true;
            }
            match_at(toks, ti + 1, chars, pos, ci, caps)
        }
        Tok::Plus(set) => {
            let mut end = pos;
            while end < chars.len() && in_set(set, chars[end], ci) {
                end += 1;
            }
            // Greedy: longest run first, backing off to one character.
            while end > pos {
                if match_at(toks, ti + 1, chars, end, ci, caps) {
                    return true;
                }
                end -= 1;
            }
            false
        }
        Tok::Lit(lit) => {
            pos < chars.len()
                && (chars[pos] == lit || (ci && chars[pos].eq_ignore_ascii_case(&lit)))
                && match_at(toks, ti + 1, chars, pos + 1, ci, caps)
        }
        Tok::NotDigit => {
            (pos >= chars.len() || !digit(chars[pos]))
                && match_at(toks, ti + 1, chars, pos, ci, caps)
        }
    }
}

fn number(chars: &[char], start: usize, width: usize) -> i64 {
    chars[start..start + width].iter().fold(0, |acc, ch| {
        acc * 10 + i64::from(ch.to_digit(10).unwrap_or(0))
    })
}

fn text_of(chars: &[char], start: usize, width: usize) -> String {
    chars[start..start + width].iter().collect()
}

/// Days since 1970-01-01 of the proleptic Gregorian date (month 1-12).
/// `day` may exceed the month length; the excess rolls into the next
/// month(s), as JavaScript's `Date` constructor does.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let (y, m) = if month <= 2 {
        (year - 1, i64::from(month) + 9)
    } else {
        (year, i64::from(month) - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468 + i64::from(day) - 1
}

/// `(year, month, day)` of a day count since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYYMMDD-HHMMSS` for seconds counted on a (naive or UTC) epoch scale.
pub fn compact_stamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// `YYYY-MM-DD HH:MM:SS` for seconds counted on a (naive or UTC) epoch scale.
pub fn display_stamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// `parseTimeFromName`: the first of the four patterns (in order) that
/// matches anywhere in `text` (leftmost position) wins.
pub fn parse_name_time(text: &str) -> Option<NaiveRecTime> {
    let chars: Vec<char> = text.chars().collect();
    for pattern in &PATTERNS {
        for start in 0..chars.len() {
            let mut caps = Vec::with_capacity(6);
            if !match_at(pattern.toks, 0, &chars, start, pattern.ci, &mut caps) {
                continue;
            }
            let year = number(&chars, caps[0], 4);
            let month = number(&chars, caps[1], 2) as u32;
            let day = number(&chars, caps[2], 2) as u32;
            let (hh, mm, ss) = if pattern.has_time {
                (
                    number(&chars, caps[3], 2),
                    number(&chars, caps[4], 2),
                    number(&chars, caps[5], 2),
                )
            } else {
                (0, 0, 0)
            };
            let date = format!(
                "{}-{}-{}",
                text_of(&chars, caps[0], 4),
                text_of(&chars, caps[1], 2),
                text_of(&chars, caps[2], 2)
            );
            let text = if pattern.has_time {
                format!(
                    "{date} {}:{}:{}",
                    text_of(&chars, caps[3], 2),
                    text_of(&chars, caps[4], 2),
                    text_of(&chars, caps[5], 2)
                )
            } else {
                date.clone()
            };
            let secs_naive = days_from_civil(year, month, day) * 86_400 + hh * 3600 + mm * 60 + ss;
            return Some(NaiveRecTime {
                secs_naive,
                text,
                date,
                has_time: pattern.has_time,
            });
        }
    }
    None
}

/// `recordingTimeFor` without the mtime fallback: the first candidate
/// string that parses at all wins — even a date-only match, which then
/// shadows later candidates exactly as in the viewer.
pub fn parse_record_time(candidates: &[&str]) -> Option<NaiveRecTime> {
    candidates.iter().find_map(|text| parse_name_time(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rectime-cases.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn matches_shared_fixture() {
        let fixture = fixture();
        let cases = fixture["cases"].as_array().unwrap();
        assert!(cases.len() >= 20, "fixture should cover the pattern set");
        for case in cases {
            let input = case["input"].as_str().unwrap();
            let got = parse_name_time(input);
            let expect = &case["expect"];
            if expect.is_null() {
                assert!(got.is_none(), "{input:?} should not parse: {got:?}");
                continue;
            }
            let got = got.unwrap_or_else(|| panic!("{input:?} should parse"));
            assert_eq!(
                got.secs_naive,
                expect["secs_naive"].as_i64().unwrap(),
                "{input:?}"
            );
            assert_eq!(got.date, expect["date"].as_str().unwrap(), "{input:?}");
            assert_eq!(
                got.has_time,
                expect["has_time"].as_bool().unwrap(),
                "{input:?}"
            );
            assert_eq!(got.text, expect["text"].as_str().unwrap(), "{input:?}");
        }
    }

    #[test]
    fn date_only_match_shadows_later_candidates_like_the_viewer() {
        // relative_path is tried before source_path in the viewer.
        let got =
            parse_record_time(&["", "20260910/clip.mp4", "C:/x/20260910_120000.mp4"]).unwrap();
        assert!(!got.has_time);
        assert_eq!(got.text, "2026-09-10");
    }

    #[test]
    fn civil_round_trip_and_stamps() {
        for days in [-1000i64, 0, 59, 365, 20_000, 20_706] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        let t = parse_name_time("20260910_120000").unwrap();
        assert_eq!(compact_stamp(t.secs_naive), "20260910-120000");
        assert_eq!(display_stamp(t.secs_naive), "2026-09-10 12:00:00");
    }
}
