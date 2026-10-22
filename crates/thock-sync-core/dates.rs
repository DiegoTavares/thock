//! The moment.js-style date vocabulary of `[daily] filename`, `[weekly]
//! filename` and `{{date:...}}` template tokens, shared so the desk and the
//! phone name and recognise daily notes the same way.

use chrono::{Datelike, NaiveDate};

/// Formats a date using the moment.js-style token vocabulary shared by
/// filename formats and `{{date:...}}` template tokens.
///
/// Supported tokens: `YYYY`, `YY`, `MMMM`, `MMM`, `MM`, `M`, `DD`, `D`,
/// `dddd`, `ddd`, `dd`, `d`, `WW`, `W` (ISO week), `GGGG`, `GG` (ISO week
/// year). Text inside `[brackets]` is emitted literally; all other characters
/// pass through unchanged.
pub fn format_date(date: NaiveDate, format: &str) -> String {
    let mut output = String::with_capacity(format.len() + 8);
    let characters: Vec<char> = format.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        match character {
            '[' => {
                index += 1;
                while index < characters.len() && characters[index] != ']' {
                    output.push(characters[index]);
                    index += 1;
                }
                if index < characters.len() {
                    index += 1;
                }
            }
            'Y' | 'M' | 'D' | 'd' | 'W' | 'G' => {
                let mut run = 1;
                while index + run < characters.len() && characters[index + run] == character {
                    run += 1;
                }
                emit_date_token(&mut output, date, character, run);
                index += run;
            }
            _ => {
                output.push(character);
                index += 1;
            }
        }
    }
    output
}

fn emit_date_token(output: &mut String, date: NaiveDate, token: char, run: usize) {
    let expansion = match (token, run) {
        ('Y', 2) => format!("{:02}", date.year() % 100),
        ('Y', _) => format!("{:04}", date.year()),
        ('M', 1) => date.month().to_string(),
        ('M', 2) => format!("{:02}", date.month()),
        ('M', 3) => date.format("%b").to_string(),
        ('M', _) => date.format("%B").to_string(),
        ('D', 1) => date.day().to_string(),
        ('D', _) => format!("{:02}", date.day()),
        ('d', 1) => date.weekday().num_days_from_sunday().to_string(),
        ('d', 2) => {
            let mut name = date.format("%A").to_string();
            name.truncate(2);
            name
        }
        ('d', 3) => date.format("%a").to_string(),
        ('d', _) => date.format("%A").to_string(),
        ('W', 1) => date.iso_week().week().to_string(),
        ('W', _) => format!("{:02}", date.iso_week().week()),
        ('G', 2) => format!("{:02}", date.iso_week().year() % 100),
        ('G', _) => format!("{:04}", date.iso_week().year()),
        _ => date.format("%A").to_string(),
    };
    output.push_str(&expansion);
}

/// Parses `text` against a `format_date` format string, returning the date
/// that formats to exactly `text` — the inverse of `format_date`, used to
/// recognize daily and weekly notes by filename. A format must carry year,
/// month and day tokens, or ISO year and week tokens, in which case the
/// Monday of that week is returned; weekday tokens are consumed but carry no
/// information. Every candidate is verified by formatting it back, so a
/// `Some` result always roundtrips.
pub fn parse_date(text: &str, format: &str) -> Option<NaiveDate> {
    let tokens = format_tokens(format);
    try_match(&tokens, text, DateFields::default(), format, text)
}

/// Whether `format` carries a year, a month and a day of the month, so a
/// stem read back through it names one calendar day. An ISO-week format
/// (`GGGG-[W]WW`) does not.
pub(crate) fn names_a_day(format: &str) -> bool {
    let tokens = format_tokens(format);
    let has = |wanted: char| {
        tokens
            .iter()
            .any(|token| matches!(token, FormatToken::Run(character, _) if *character == wanted))
    };
    has('Y') && has('M') && has('D')
}

enum FormatToken {
    Literal(char),
    Run(char, usize),
}

/// Tokenizes a format string exactly the way `format_date` walks it: bracket
/// literals, runs of the date token characters, and passthrough characters.
fn format_tokens(format: &str) -> Vec<FormatToken> {
    let characters: Vec<char> = format.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        match character {
            '[' => {
                index += 1;
                while index < characters.len() && characters[index] != ']' {
                    tokens.push(FormatToken::Literal(characters[index]));
                    index += 1;
                }
                if index < characters.len() {
                    index += 1;
                }
            }
            'Y' | 'M' | 'D' | 'd' | 'W' | 'G' => {
                let mut run = 1;
                while index + run < characters.len() && characters[index + run] == character {
                    run += 1;
                }
                tokens.push(FormatToken::Run(character, run));
                index += run;
            }
            _ => {
                tokens.push(FormatToken::Literal(character));
                index += 1;
            }
        }
    }
    tokens
}

#[derive(Clone, Copy, Default)]
struct DateFields {
    year: Option<i32>,
    month: Option<u32>,
    day: Option<u32>,
    iso_year: Option<i32>,
    week: Option<u32>,
}

impl DateFields {
    /// The date the fields name: a calendar date when the format carries
    /// one, otherwise the Monday of the ISO week it carries (a weekly note's
    /// stem, `GGGG-[W]WW`).
    fn resolve(self) -> Option<NaiveDate> {
        match (self.year, self.month, self.day) {
            (Some(year), Some(month), Some(day)) => NaiveDate::from_ymd_opt(year, month, day),
            _ => NaiveDate::from_isoywd_opt(self.iso_year?, self.week?, chrono::Weekday::Mon),
        }
    }
}

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const WEEKDAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// Backtracking matcher over the token list. Variable-width numeric tokens
/// (`M`, `D`, `W`) try the longer width first; the final roundtrip check
/// rejects any loose match that doesn't reproduce the input.
fn try_match(
    tokens: &[FormatToken],
    input: &str,
    fields: DateFields,
    format: &str,
    full_input: &str,
) -> Option<NaiveDate> {
    let Some((token, rest_tokens)) = tokens.split_first() else {
        if !input.is_empty() {
            return None;
        }
        let date = fields.resolve()?;
        return (format_date(date, format) == full_input).then_some(date);
    };
    let recurse =
        |input: &str, fields: DateFields| try_match(rest_tokens, input, fields, format, full_input);
    match token {
        FormatToken::Literal(literal) => recurse(input.strip_prefix(*literal)?, fields),
        FormatToken::Run('Y', 2) => {
            let (value, rest) = take_digits(input, 2)?;
            recurse(
                rest,
                DateFields {
                    year: Some(2000 + value as i32),
                    ..fields
                },
            )
        }
        FormatToken::Run('Y', _) => {
            let (value, rest) = take_digits(input, 4)?;
            recurse(
                rest,
                DateFields {
                    year: Some(value as i32),
                    ..fields
                },
            )
        }
        FormatToken::Run('M', 1) => [2, 1].iter().find_map(|&width| {
            let (value, rest) = take_digits(input, width)?;
            recurse(
                rest,
                DateFields {
                    month: Some(value),
                    ..fields
                },
            )
        }),
        FormatToken::Run('M', 2) => {
            let (value, rest) = take_digits(input, 2)?;
            recurse(
                rest,
                DateFields {
                    month: Some(value),
                    ..fields
                },
            )
        }
        FormatToken::Run('M', run) => MONTH_NAMES.iter().enumerate().find_map(|(index, name)| {
            let name = if *run == 3 { &name[..3] } else { name };
            let rest = input.strip_prefix(name)?;
            recurse(
                rest,
                DateFields {
                    month: Some(index as u32 + 1),
                    ..fields
                },
            )
        }),
        FormatToken::Run('D', 1) => [2, 1].iter().find_map(|&width| {
            let (value, rest) = take_digits(input, width)?;
            recurse(
                rest,
                DateFields {
                    day: Some(value),
                    ..fields
                },
            )
        }),
        FormatToken::Run('D', _) => {
            let (value, rest) = take_digits(input, 2)?;
            recurse(
                rest,
                DateFields {
                    day: Some(value),
                    ..fields
                },
            )
        }
        FormatToken::Run('d', 1) => recurse(take_digits(input, 1)?.1, fields),
        FormatToken::Run('d', 2) => WEEKDAY_NAMES
            .iter()
            .find_map(|name| recurse(input.strip_prefix(&name[..2])?, fields)),
        FormatToken::Run('d', run) => WEEKDAY_NAMES.iter().find_map(|name| {
            let name = if *run == 3 { &name[..3] } else { name };
            recurse(input.strip_prefix(name)?, fields)
        }),
        FormatToken::Run('W', 1) => [2, 1].iter().find_map(|&width| {
            let (value, rest) = take_digits(input, width)?;
            recurse(
                rest,
                DateFields {
                    week: Some(value),
                    ..fields
                },
            )
        }),
        FormatToken::Run('W', _) => {
            let (value, rest) = take_digits(input, 2)?;
            recurse(
                rest,
                DateFields {
                    week: Some(value),
                    ..fields
                },
            )
        }
        FormatToken::Run('G', 2) => {
            let (value, rest) = take_digits(input, 2)?;
            recurse(
                rest,
                DateFields {
                    iso_year: Some(2000 + value as i32),
                    ..fields
                },
            )
        }
        FormatToken::Run('G', _) => {
            let (value, rest) = take_digits(input, 4)?;
            recurse(
                rest,
                DateFields {
                    iso_year: Some(value as i32),
                    ..fields
                },
            )
        }
        FormatToken::Run(..) => None,
    }
}

/// Takes exactly `width` ASCII digits from the start of `input`.
fn take_digits(input: &str, width: usize) -> Option<(u32, &str)> {
    let digits = input.get(..width)?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((digits.parse().ok()?, input.get(width..)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn format_date_tokens() {
        let d = date(2026, 7, 20); // a Monday
        assert_eq!(format_date(d, "YYYY-MM-DD"), "2026-07-20");
        assert_eq!(format_date(d, "YY M D"), "26 7 20");
        assert_eq!(
            format_date(d, "dddd, MMMM D, YYYY"),
            "Monday, July 20, 2026"
        );
        assert_eq!(format_date(d, "ddd MMM DD"), "Mon Jul 20");
        assert_eq!(format_date(d, "dd"), "Mo");
        assert_eq!(format_date(d, "d"), "1");
        assert_eq!(format_date(date(2026, 7, 5), "D MMM"), "5 Jul");
    }

    #[test]
    fn format_date_week_tokens() {
        for d in [
            date(2026, 7, 20),
            date(2026, 1, 1),
            date(2025, 12, 29), // ISO week year differs from calendar year
            date(2027, 1, 3),
        ] {
            assert_eq!(
                format_date(d, "GGGG-[W]WW"),
                format!("{:04}-W{:02}", d.iso_week().year(), d.iso_week().week()),
                "for {d}"
            );
        }
        assert_eq!(format_date(date(2026, 2, 2), "[Week] W"), "Week 6");
    }

    #[test]
    fn format_date_literals_pass_through() {
        let d = date(2026, 7, 20);
        assert_eq!(format_date(d, "[Day] D [of] MMMM"), "Day 20 of July");
        assert_eq!(format_date(d, "YYYY/MM/DD daily"), "2026/07/20 1aily");
    }

    #[test]
    fn parse_date_inverts_format_date() {
        for format in [
            "YYYY-MM-DD",
            "DD-MM-YYYY",
            "YYYY/M/D",
            "[day-]YYYY-MM-DD",
            "D MMM YYYY",
            "MMMM D, YYYY",
            "dddd YYYY-MM-DD",
            "ddd DD MM YY",
        ] {
            for d in [date(2026, 7, 20), date(2026, 1, 5), date(2027, 12, 31)] {
                let formatted = format_date(d, format);
                assert_eq!(
                    parse_date(&formatted, format),
                    Some(d),
                    "for {format:?} / {formatted:?}"
                );
            }
        }
    }

    #[test]
    fn parse_date_resolves_a_week_to_its_monday() {
        for (text, format, monday) in [
            ("2026-W42", "GGGG-[W]WW", date(2026, 10, 12)),
            ("2027-W01", "GGGG-[W]WW", date(2027, 1, 4)),
            ("2026-W53", "GGGG-[W]WW", date(2026, 12, 28)),
            ("26w1", "GG[w]W", date(2025, 12, 29)),
        ] {
            assert_eq!(parse_date(text, format), Some(monday), "{text:?}");
        }
        assert_eq!(parse_date("2026-W54", "GGGG-[W]WW"), None);
        assert_eq!(
            parse_date("2026-W4", "GGGG-[W]WW"),
            None,
            "two digits are required"
        );
    }

    #[test]
    fn parse_date_rejects_non_matches() {
        assert_eq!(parse_date("not-a-date", "YYYY-MM-DD"), None);
        assert_eq!(parse_date("2026-13-40", "YYYY-MM-DD"), None);
        assert_eq!(parse_date("2026-07-20-extra", "YYYY-MM-DD"), None);
        assert_eq!(parse_date("2026-07", "YYYY-MM-DD"), None);
        // A wrong weekday name fails the roundtrip check.
        assert_eq!(parse_date("Tuesday 2026-07-20", "dddd YYYY-MM-DD"), None);
        // A weekday alone names no date.
        assert_eq!(parse_date("Monday", "dddd"), None);
    }
}
