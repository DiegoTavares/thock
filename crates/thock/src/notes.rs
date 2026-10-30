use anyhow::{Context as _, Result};
use chrono::{Datelike, Days, NaiveDate, NaiveTime};
use fs::{CreateOptions, Fs, RemoveOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::vault::Vault;

pub use thock_sync_core::{format_date, parse_date};

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

/// How far back `cleanup_dates` reaches without a record of the last day a
/// note was ensured, and the most dates a stale record may add (V51 §4.3).
pub const CLEANUP_WINDOW_DAYS: u64 = 30;
pub const CLEANUP_MAX_DAYS: u64 = 400;

/// The past dates whose daily notes are checked for being template-only
/// (V51 §4.3): from `CLEANUP_WINDOW_DAYS` back, or from `last_ensured` when
/// that is older, up to yesterday, oldest first, never today and never more
/// than `CLEANUP_MAX_DAYS`.
pub fn cleanup_dates(today: NaiveDate, last_ensured: Option<NaiveDate>) -> Vec<NaiveDate> {
    let (Some(floor), Some(cap)) = (
        today.checked_sub_days(Days::new(CLEANUP_WINDOW_DAYS)),
        today.checked_sub_days(Days::new(CLEANUP_MAX_DAYS)),
    ) else {
        return Vec::new();
    };
    let start = last_ensured.map_or(floor, |last| last.min(floor)).max(cap);
    std::iter::successors(Some(start), |date| date.succ_opt())
        .take_while(|date| *date < today)
        .collect()
}

/// Whether `note` is what `ensure_note` writes for `date` from `template`
/// and nothing more (V51 §4.2): the same lines, once line endings, trailing
/// whitespace and trailing blank lines are ignored, except that a template
/// line carrying `{{time}}` matches whatever the note has on that line,
/// since the creation time is not known any more.
pub fn is_template_only(note: &str, template: &str, date: NaiveDate, title: &str) -> bool {
    let note_lines = normalized_lines(note);
    let template_lines = normalized_lines(template);
    note_lines.len() == template_lines.len()
        && template_lines
            .iter()
            .zip(&note_lines)
            .all(|(template_line, note_line)| {
                template_line.contains("{{time}}")
                    || expand_template(template_line, date, NaiveTime::MIN, title) == *note_line
            })
}

fn normalized_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    while lines.last() == Some(&"") {
        lines.pop();
    }
    lines
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

/// Tells the vault's reminders service that the note at `path` was just
/// ensured, so its day section is written now rather than on the next file
/// event (V45 §7.2). Nothing happens outside a vault.
pub fn announce_note(project: &gpui::Entity<project::Project>, path: &Path, cx: &mut gpui::App) {
    let Some(service) = crate::reminder_service::service_for_project(project, cx) else {
        return;
    };
    service.update(cx, |service, cx| {
        if let Ok(relative) = path.strip_prefix(&service.vault().root) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            service.note_created(&relative, cx);
        }
    });
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

/// The `{{title}}` of the note at `path`: its file stem.
pub(crate) fn note_title(path: &Path) -> String {
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
    fn cleanup_dates_reach_thirty_days_or_the_last_ensured_day() {
        let today = date(2026, 10, 30);
        let dates = cleanup_dates(today, None);
        assert_eq!(dates.len(), 30);
        assert_eq!(dates.first(), Some(&date(2026, 9, 30)));
        assert_eq!(dates.last(), Some(&date(2026, 10, 29)));

        // A record inside the window changes nothing; an older one extends it.
        assert_eq!(cleanup_dates(today, Some(date(2026, 10, 20))).len(), 30);
        let dates = cleanup_dates(today, Some(date(2026, 9, 15)));
        assert_eq!(dates.first(), Some(&date(2026, 9, 15)));
        assert_eq!(dates.len(), 45);

        // A years-old record is capped; today is never included.
        let dates = cleanup_dates(today, Some(date(2020, 1, 1)));
        assert_eq!(dates.len(), CLEANUP_MAX_DAYS as usize);
        assert!(dates.iter().all(|d| *d < today));
        assert!(cleanup_dates(today, Some(today)).iter().all(|d| *d < today));
    }

    #[test]
    fn template_only_ignores_what_saving_and_the_clock_change() {
        let d = date(2026, 10, 29);
        let template =
            "# {{date:dddd, MMMM D, YYYY}}\n\nStarted {{time}}\n\n## Journal\n\n{{weather}}\n";
        let made = expand_template(template, d, NaiveTime::from_hms_opt(9, 7, 0).unwrap(), "x");
        assert!(is_template_only(&made, template, d, "x"));

        // A different time, CRLF endings, trailing spaces and extra blank
        // lines at the end are all still the template.
        let later = expand_template(template, d, NaiveTime::from_hms_opt(23, 1, 0).unwrap(), "x");
        assert!(is_template_only(&later, template, d, "x"));
        let crlf = made.replace('\n', "\r\n");
        assert!(is_template_only(&crlf, template, d, "x"));
        let padded = made.replace("## Journal", "## Journal   ") + "\n\n\n";
        assert!(is_template_only(&padded, template, d, "x"));
        assert!(is_template_only(made.trim_end(), template, d, "x"));

        // One character of the user's, a line more, or another day's date
        // is not.
        assert!(!is_template_only(&format!("{made}."), template, d, "x"));
        assert!(!is_template_only(
            &format!("{made}- Called Ana\n"),
            template,
            d,
            "x"
        ));
        assert!(!is_template_only(&made, template, date(2026, 10, 28), "x"));
        assert!(!is_template_only("", template, d, "x"));
        assert!(is_template_only("", "", d, "x"));
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
