use anyhow::{Context as _, Result};
use chrono::{Datelike, Days, NaiveDate, NaiveTime};
use fs::{CreateOptions, Fs, RemoveOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::vault::Vault;

/// The kinds of periodic notes a vault holds, each with its own directory,
/// filename format, and template in `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Daily,
    Weekly,
}

/// An entry in the Timeline panel (and its matching `thock:` command).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineEntry {
    Today,
    Yesterday,
    Tomorrow,
    ThisWeek,
    LastWeek,
}

impl TimelineEntry {
    /// Resolves the entry to the kind of note it opens and the date that
    /// identifies the note. Weekly notes are identified by the Monday starting
    /// their ISO week.
    pub fn resolve(self, today: NaiveDate) -> Option<(NoteKind, NaiveDate)> {
        match self {
            Self::Today => Some((NoteKind::Daily, today)),
            Self::Yesterday => today.pred_opt().map(|date| (NoteKind::Daily, date)),
            Self::Tomorrow => today.succ_opt().map(|date| (NoteKind::Daily, date)),
            Self::ThisWeek => week_start(today).map(|date| (NoteKind::Weekly, date)),
            Self::LastWeek => week_start(today)
                .and_then(|monday| monday.checked_sub_days(Days::new(7)))
                .map(|date| (NoteKind::Weekly, date)),
        }
    }
}

fn week_start(date: NaiveDate) -> Option<NaiveDate> {
    date.checked_sub_days(Days::new(date.weekday().num_days_from_monday() as u64))
}

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
/// recognize daily notes by filename. Weekday / week-number tokens are
/// consumed but carry no year/month/day information; a format must contain
/// year, month, and day tokens for parsing to succeed. Every candidate is
/// verified by formatting it back, so a `Some` result always roundtrips.
pub fn parse_date(text: &str, format: &str) -> Option<NaiveDate> {
    let tokens = format_tokens(format);
    try_match(&tokens, text, DateFields::default(), format, text)
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
        let date = NaiveDate::from_ymd_opt(fields.year?, fields.month?, fields.day?)?;
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
        FormatToken::Run('W', 1) => [2, 1]
            .iter()
            .find_map(|&width| recurse(take_digits(input, width)?.1, fields)),
        FormatToken::Run('W', _) => recurse(take_digits(input, 2)?.1, fields),
        FormatToken::Run('G', 2) => recurse(take_digits(input, 2)?.1, fields),
        FormatToken::Run('G', _) => recurse(take_digits(input, 4)?.1, fields),
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

/// Expands Obsidian-style template tokens: `{{date}}`, `{{date:FORMAT}}`,
/// `{{time}}`, and `{{title}}`. Unrecognized tokens are left as-is.
pub fn expand_template(template: &str, date: NaiveDate, time: NaiveTime, title: &str) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let Some(end_offset) = rest[start + 2..].find("}}") else {
            break;
        };
        let token = &rest[start + 2..start + 2 + end_offset];
        output.push_str(&rest[..start]);
        match token {
            "date" => output.push_str(&date.format("%Y-%m-%d").to_string()),
            "time" => output.push_str(&time.format("%H:%M").to_string()),
            "title" => output.push_str(title),
            _ => {
                if let Some(format) = token.strip_prefix("date:") {
                    output.push_str(&format_date(date, format));
                } else {
                    output.push_str(&rest[start..start + 2 + end_offset + 2]);
                }
            }
        }
        rest = &rest[start + 2 + end_offset + 2..];
    }
    output.push_str(rest);
    output
}

/// The outcome of ensuring a note exists.
#[derive(Debug, PartialEq)]
pub enum EnsureNoteOutcome {
    AlreadyExisted,
    Created,
    /// The note was created empty because the configured template is missing.
    CreatedWithoutTemplate,
}

/// Ensures the note of `kind` for `date` exists, creating it from the vault's
/// template if missing, and returns its path. Existing notes are never
/// touched.
pub async fn ensure_note(
    fs: &Arc<dyn Fs>,
    vault: &Vault,
    kind: NoteKind,
    date: NaiveDate,
    time: NaiveTime,
) -> Result<(PathBuf, EnsureNoteOutcome)> {
    let path = vault.note_path(kind, date);
    let outcome = ensure_note_at(fs, vault, kind, date, time, &path).await?;
    Ok((path, outcome))
}

/// `ensure_note` for an arbitrary destination: Routine links with date
/// templates (V7 §6) create their target from the note kind's template even
/// when the declared path differs from the configured note path.
pub async fn ensure_note_at(
    fs: &Arc<dyn Fs>,
    vault: &Vault,
    kind: NoteKind,
    date: NaiveDate,
    time: NaiveTime,
    path: &Path,
) -> Result<EnsureNoteOutcome> {
    if path_exists(fs, path).await? {
        return Ok(EnsureNoteOutcome::AlreadyExisted);
    }

    let template_path = vault.template_path(kind);
    let (template, outcome) = if path_exists(fs, &template_path).await? {
        let template = fs
            .load(&template_path)
            .await
            .with_context(|| format!("reading template {}", template_path.display()))?;
        (template, EnsureNoteOutcome::Created)
    } else {
        (String::new(), EnsureNoteOutcome::CreatedWithoutTemplate)
    };

    let contents = expand_template(&template, date, time, &note_title(path));
    create_file_if_missing(fs, path, &contents).await?;
    Ok(outcome)
}

/// Writes the shipped example day (`vault::EXAMPLE_DAY_NOTE`) as today's
/// daily note, so a brand-new vault opens on a filled-in page rather than an
/// empty template. Returns the note's path when it was created, `None` when
/// a note for `today` already exists (it is never touched).
pub async fn ensure_example_day(
    fs: &Arc<dyn Fs>,
    vault: &Vault,
    today: NaiveDate,
    time: NaiveTime,
) -> Result<Option<PathBuf>> {
    let path = vault.note_path(NoteKind::Daily, today);
    if path_exists(fs, &path).await? {
        return Ok(None);
    }
    let contents = expand_template(
        crate::vault::EXAMPLE_DAY_NOTE,
        today,
        time,
        &note_title(&path),
    );
    create_file_if_missing(fs, &path, &contents).await?;
    Ok(Some(path))
}

fn note_title(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

async fn path_exists(fs: &Arc<dyn Fs>, path: &Path) -> Result<bool> {
    Ok(fs
        .metadata(path)
        .await
        .with_context(|| format!("checking {}", path.display()))?
        .is_some())
}

/// Creates `path` (and its folder) with `contents` unless something is
/// already there, which is left untouched. If the write fails partway the
/// file is removed again so no partial note is left behind.
pub(crate) async fn create_file_if_missing(
    fs: &Arc<dyn Fs>,
    path: &Path,
    contents: &str,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs.create_dir(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    if let Err(error) = fs.create_file(path, CreateOptions::default()).await {
        // Another writer (the user's other editor, a sync) created it first;
        // theirs wins.
        if matches!(fs.metadata(path).await, Ok(Some(_))) {
            return Ok(());
        }
        return Err(error).with_context(|| format!("creating {}", path.display()));
    }
    if contents.is_empty() {
        return Ok(());
    }
    if let Err(error) = fs.write(path, contents.as_bytes()).await {
        if let Err(cleanup_error) = fs
            .remove_file(
                path,
                RemoveOptions {
                    recursive: false,
                    ignore_if_not_exists: true,
                },
            )
            .await
        {
            log::error!(
                "failed to clean up partially written note {}: {cleanup_error:#}",
                path.display()
            );
        }
        return Err(error).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{DEFAULT_DAILY_TEMPLATE, DEFAULT_WEEKLY_TEMPLATE};
    use fs::FakeFs;
    use gpui::TestAppContext;
    use serde_json::json;

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
    fn timeline_entries_resolve() {
        let tuesday = date(2026, 7, 21);
        assert_eq!(
            TimelineEntry::Today.resolve(tuesday),
            Some((NoteKind::Daily, tuesday))
        );
        assert_eq!(
            TimelineEntry::Yesterday.resolve(tuesday),
            Some((NoteKind::Daily, date(2026, 7, 20)))
        );
        assert_eq!(
            TimelineEntry::Tomorrow.resolve(tuesday),
            Some((NoteKind::Daily, date(2026, 7, 22)))
        );
        assert_eq!(
            TimelineEntry::ThisWeek.resolve(tuesday),
            Some((NoteKind::Weekly, date(2026, 7, 20)))
        );
        assert_eq!(
            TimelineEntry::LastWeek.resolve(tuesday),
            Some((NoteKind::Weekly, date(2026, 7, 13)))
        );

        // A Monday's ThisWeek is itself; a Sunday's is the previous Monday.
        assert_eq!(
            TimelineEntry::ThisWeek.resolve(date(2026, 7, 20)),
            Some((NoteKind::Weekly, date(2026, 7, 20)))
        );
        assert_eq!(
            TimelineEntry::ThisWeek.resolve(date(2026, 7, 26)),
            Some((NoteKind::Weekly, date(2026, 7, 20)))
        );
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
    fn parse_date_rejects_non_matches() {
        assert_eq!(parse_date("not-a-date", "YYYY-MM-DD"), None);
        assert_eq!(parse_date("2026-13-40", "YYYY-MM-DD"), None);
        assert_eq!(parse_date("2026-07-20-extra", "YYYY-MM-DD"), None);
        assert_eq!(parse_date("2026-07", "YYYY-MM-DD"), None);
        // A wrong weekday name fails the roundtrip check.
        assert_eq!(parse_date("Tuesday 2026-07-20", "dddd YYYY-MM-DD"), None);
        // Week-only formats carry no year/month/day.
        assert_eq!(parse_date("2026-W30", "GGGG-[W]WW"), None);
    }

    #[test]
    fn expand_template_tokens() {
        let d = date(2026, 7, 19);
        let t = NaiveTime::from_hms_opt(14, 31, 0).unwrap();
        assert_eq!(
            expand_template(
                "# {{date:dddd, MMMM D, YYYY}}\n{{date}} {{time}} {{title}}",
                d,
                t,
                "2026-07-19",
            ),
            "# Sunday, July 19, 2026\n2026-07-19 14:31 2026-07-19"
        );
    }

    #[test]
    fn expand_template_leaves_unknown_tokens() {
        let d = date(2026, 7, 19);
        let t = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
        assert_eq!(
            expand_template("{{weather}} and {{unclosed", d, t, "x"),
            "{{weather}} and {{unclosed"
        );
    }

    fn test_vault() -> Vault {
        Vault {
            root: PathBuf::from("/vault"),
            config: crate::vault::VaultConfig::default(),
        }
    }

    async fn fake_vault_fs(cx: &TestAppContext) -> Arc<dyn Fs> {
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree(
            "/vault",
            json!({
                "templates": {
                    "daily.md": DEFAULT_DAILY_TEMPLATE,
                    "weekly.md": DEFAULT_WEEKLY_TEMPLATE,
                },
            }),
        )
        .await;
        fs
    }

    #[gpui::test]
    async fn ensure_note_creates_daily_from_template(cx: &mut TestAppContext) {
        let fs = fake_vault_fs(cx).await;
        let vault = test_vault();
        let d = date(2026, 7, 20);
        let t = NaiveTime::from_hms_opt(9, 0, 0).unwrap();

        let (path, outcome) = ensure_note(&fs, &vault, NoteKind::Daily, d, t)
            .await
            .unwrap();
        assert_eq!(outcome, EnsureNoteOutcome::Created);
        assert_eq!(path, Path::new("/vault/daily/2026-07-20.md"));
        let contents = fs.load(&path).await.unwrap();
        assert_eq!(
            contents,
            expand_template(DEFAULT_DAILY_TEMPLATE, d, t, "2026-07-20")
        );
        assert!(contents.starts_with("# Monday, July 20, 2026\n"));

        // A second call must not touch the file.
        fs.write(&path, b"user edits").await.unwrap();
        let (_, outcome) = ensure_note(&fs, &vault, NoteKind::Daily, d, t)
            .await
            .unwrap();
        assert_eq!(outcome, EnsureNoteOutcome::AlreadyExisted);
        assert_eq!(fs.load(&path).await.unwrap(), "user edits");
    }

    #[gpui::test]
    async fn ensure_note_creates_weekly_from_template(cx: &mut TestAppContext) {
        let fs = fake_vault_fs(cx).await;
        let vault = test_vault();
        let monday = date(2026, 7, 20);
        let t = NaiveTime::from_hms_opt(9, 0, 0).unwrap();

        let (path, outcome) = ensure_note(&fs, &vault, NoteKind::Weekly, monday, t)
            .await
            .unwrap();
        assert_eq!(outcome, EnsureNoteOutcome::Created);
        assert_eq!(path, Path::new("/vault/weekly/2026-W30.md"));
        let contents = fs.load(&path).await.unwrap();
        assert!(contents.starts_with("# Week 30, 2026\n"), "got: {contents}");
    }

    #[gpui::test]
    async fn ensure_example_day_writes_once_and_parses_as_a_plan(cx: &mut TestAppContext) {
        let fs = fake_vault_fs(cx).await;
        let vault = test_vault();
        let d = date(2026, 7, 20);
        let t = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        let note_path = Path::new("/vault/daily/2026-07-20.md");

        let path = ensure_example_day(&fs, &vault, d, t).await.unwrap();
        assert_eq!(path.as_deref(), Some(note_path));
        let contents = fs.load(note_path).await.unwrap();
        assert!(contents.starts_with("# Monday, July 20, 2026\n"));
        assert!(
            contents.contains("This first day is an example"),
            "the example must say it is one"
        );
        assert!(
            contents.contains("templates/daily.md"),
            "the example must point at the template it invites the user to customize"
        );
        assert!(!contents.contains("{{"), "every template token must expand");

        // The showcase planner lines must be ones the Day Planner draws.
        let plan = crate::day_plan::parse_day_plan(
            &contents,
            &crate::day_plan::DayPlannerConfig::default(),
        );
        assert!(
            plan.has_timed_items(),
            "the example should land on the planner grid"
        );
        assert!(plan.items.iter().any(|item| item.done));
        assert!(plan.items.iter().any(|item| !item.done));

        // Today's note, once it exists, is never touched.
        fs.write(note_path, b"user edits").await.unwrap();
        assert_eq!(ensure_example_day(&fs, &vault, d, t).await.unwrap(), None);
        assert_eq!(fs.load(note_path).await.unwrap(), "user edits");
    }

    #[gpui::test]
    async fn ensure_note_missing_template_creates_empty(cx: &mut TestAppContext) {
        let fs: Arc<dyn Fs> = FakeFs::new(cx.background_executor.clone());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let (path, outcome) = ensure_note(
            &fs,
            &test_vault(),
            NoteKind::Daily,
            date(2026, 7, 20),
            NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, EnsureNoteOutcome::CreatedWithoutTemplate);
        assert_eq!(fs.load(&path).await.unwrap(), "");
    }

    #[gpui::test]
    async fn create_file_if_missing_never_overwrites(cx: &mut TestAppContext) {
        let fs = FakeFs::new(cx.background_executor.clone());
        fs.insert_tree("/vault", json!({ "note.md": "mine" })).await;
        let fs: Arc<dyn Fs> = fs;
        create_file_if_missing(&fs, Path::new("/vault/note.md"), "seed")
            .await
            .unwrap();
        assert_eq!(fs.load(Path::new("/vault/note.md")).await.unwrap(), "mine");
    }
}
