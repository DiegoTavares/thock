//! The Inbox's morning run (spec `v56-inbox-auto-archive.md` §7): which
//! items it asks Jev about, the bar an answer must clear, the guardrail for
//! threads the person wrote in, the record it keeps in
//! `.thock/state/inbox/auto-archive.jsonl`, and what the view's
//! Auto-archived group and the sync popover say about it. Pure: the
//! `InboxService` drives it and does every read and write.

use anyhow::{Context as _, Result};
use chrono::{DateTime, Datelike as _, NaiveDate, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::inbox_email::EmailThread;
use crate::inbox_suggest::{DecisionRecord, Destination, FILLED_THRESHOLD, Suggestion, VIA_AUTO};

/// The run's record, beside the suggestion cache and the decision log.
pub const AUTO_ARCHIVE_FILE: &str = "auto-archive.jsonl";
/// `[inbox] auto_archive_threshold` when unset (V56 decision 6).
pub const DEFAULT_THRESHOLD: f64 = 0.95;
/// Below the filled chip the run would archive what the view wouldn't even
/// highlight.
pub const MIN_THRESHOLD: f64 = FILLED_THRESHOLD;

/// The configured threshold clamped to `[MIN_THRESHOLD, 1.0]`; unset or not
/// a number reads as [`DEFAULT_THRESHOLD`].
pub fn effective_threshold(configured: Option<f64>) -> f64 {
    match configured {
        Some(threshold) if threshold.is_finite() => threshold.clamp(MIN_THRESHOLD, 1.0),
        _ => DEFAULT_THRESHOLD,
    }
}

/// The `refreshed:` stamp in `triage-policy.md`'s front matter (V56 §4.2):
/// the refresh skill wrote the file, so the run may follow it.
pub fn policy_refreshed(markdown: &str) -> Option<NaiveDate> {
    let mut lines = markdown.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    for line in lines {
        let line = line.trim();
        if line == "---" || line == "..." {
            return None;
        }
        if let Some(value) = line.strip_prefix("refreshed:") {
            let value = value.trim().trim_matches(['"', '\'']);
            return NaiveDate::parse_from_str(value, "%Y-%m-%d").ok();
        }
    }
    None
}

/// Why the run left an item in the Inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkipReason {
    /// Jev's answer didn't clear the bar.
    Below,
    /// The person wrote in the thread (G6).
    Replied,
    /// The request or the filing failed.
    Error,
}

/// One line of `auto-archive.jsonl` (V56 §7.3). `enabled` and `disabled`
/// mark when `[inbox] auto_archive` was seen to change, so the first run's
/// watermark is the moment it was turned on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum RunEvent {
    Enabled {
        at: DateTime<Utc>,
    },
    Disabled {
        at: DateTime<Utc>,
    },
    Started {
        run: String,
        at: DateTime<Utc>,
        watermark: DateTime<Utc>,
        policy: String,
        threshold: f64,
        /// How many items the run set out to ask about, for *stopped after
        /// 4 of 11*.
        candidates: usize,
    },
    Archived {
        run: String,
        stem: String,
        path: String,
        confidence: f64,
        /// The triage-log line it wrote, so a correction finds it exactly.
        line: String,
    },
    Skipped {
        run: String,
        stem: String,
        reason: SkipReason,
    },
    Finished {
        run: String,
        at: DateTime<Utc>,
        archived: usize,
        skipped: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stopped: Option<String>,
    },
}

impl RunEvent {
    pub fn to_json_line(&self) -> Result<String> {
        serde_json::to_string(self).context("serializing an auto-archive record")
    }
}

/// An item a run filed.
#[derive(Debug, Clone, PartialEq)]
pub struct ArchivedItem {
    pub stem: String,
    /// Where the note went, vault-relative.
    pub path: String,
    pub confidence: f64,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub at: DateTime<Utc>,
    pub archived: usize,
    pub skipped: usize,
    pub stopped: Option<String>,
}

/// One run, folded from its records.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub watermark: DateTime<Utc>,
    pub policy: String,
    pub threshold: f64,
    pub candidates: usize,
    /// In the order they were filed.
    pub archived: Vec<ArchivedItem>,
    pub skipped: Vec<(String, SkipReason)>,
    /// The last `finished` record: a retried run writes another.
    pub finished: Option<Finished>,
}

impl Run {
    /// Every stem the run already decided on, filed or skipped.
    pub fn recorded(&self) -> HashSet<String> {
        self.archived
            .iter()
            .map(|item| item.stem.clone())
            .chain(self.skipped.iter().map(|(stem, _)| stem.clone()))
            .collect()
    }

    pub fn stopped(&self) -> Option<&str> {
        self.finished.as_ref()?.stopped.as_deref()
    }
}

/// `auto-archive.jsonl` folded into its runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunLog {
    pub runs: Vec<Run>,
    /// When `[inbox] auto_archive` was last seen turned on, if it is on.
    pub enabled_at: Option<DateTime<Utc>>,
}

impl RunLog {
    /// Unreadable lines are skipped; a record for a run with no `started`
    /// is too.
    pub fn parse(text: &str) -> Self {
        let mut log = Self::default();
        for event in text
            .lines()
            .filter_map(|line| serde_json::from_str::<RunEvent>(line).ok())
        {
            log.apply(event);
        }
        log
    }

    pub fn apply(&mut self, event: RunEvent) {
        match event {
            RunEvent::Enabled { at } => self.enabled_at = Some(at),
            RunEvent::Disabled { .. } => self.enabled_at = None,
            RunEvent::Started {
                run,
                at,
                watermark,
                policy,
                threshold,
                candidates,
            } => self.runs.push(Run {
                id: run,
                started_at: at,
                watermark,
                policy,
                threshold,
                candidates,
                archived: Vec::new(),
                skipped: Vec::new(),
                finished: None,
            }),
            RunEvent::Archived {
                run,
                stem,
                path,
                confidence,
                line,
            } => {
                if let Some(run) = self.run_mut(&run) {
                    run.archived.push(ArchivedItem {
                        stem,
                        path,
                        confidence,
                        line,
                    });
                }
            }
            RunEvent::Skipped { run, stem, reason } => {
                if let Some(run) = self.run_mut(&run) {
                    run.skipped.push((stem, reason));
                }
            }
            RunEvent::Finished {
                run,
                at,
                archived,
                skipped,
                stopped,
            } => {
                if let Some(run) = self.run_mut(&run) {
                    run.finished = Some(Finished {
                        at,
                        archived,
                        skipped,
                        stopped,
                    });
                }
            }
        }
    }

    fn run_mut(&mut self, id: &str) -> Option<&mut Run> {
        self.runs.iter_mut().rev().find(|run| run.id == id)
    }

    pub fn latest(&self) -> Option<&Run> {
        self.runs.last()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled_at.is_some()
    }
}

/// What the run should do on this trigger (V56 §7.3).
#[derive(Debug, Clone, PartialEq)]
pub enum Due {
    /// It already ran today.
    No,
    /// Carry on with a run that never finished (the app quit mid-run) or,
    /// asked for by hand, one that stopped: its stems already recorded are
    /// skipped.
    Resume {
        run: String,
        watermark: DateTime<Utc>,
        skip: HashSet<String>,
    },
    /// A new run over what arrived after `watermark`.
    Fresh {
        watermark: DateTime<Utc>,
        skip: HashSet<String>,
    },
}

/// Whether a run is due at `now`, in the local day `now`'s zone gives. "First
/// of the day" is read from the file: a `finished` dated today means it ran.
/// `manual` is `thock::RunInboxAutoArchive`, which runs again whatever the
/// day and retries a run that stopped.
pub fn due<Tz: TimeZone>(log: &RunLog, now: &DateTime<Tz>, manual: bool) -> Due {
    let now_utc = now.with_timezone(&Utc);
    let enabled_at = log.enabled_at.unwrap_or(now_utc);
    let Some(latest) = log.latest() else {
        return Due::Fresh {
            watermark: enabled_at,
            skip: HashSet::new(),
        };
    };
    let resume = || Due::Resume {
        run: latest.id.clone(),
        watermark: latest.watermark,
        skip: latest.recorded(),
    };
    let Some(finished) = &latest.finished else {
        return resume();
    };
    if latest.stopped().is_some() {
        if manual {
            return resume();
        }
        // A run that stopped on an earlier day never asked about the rest of
        // its items; the next one starts where it did.
        if finished.at.with_timezone(&now.timezone()).date_naive() != now.date_naive() {
            return Due::Fresh {
                watermark: latest.watermark.max(enabled_at),
                skip: latest.recorded(),
            };
        }
        return Due::No;
    }
    if !manual && finished.at.with_timezone(&now.timezone()).date_naive() == now.date_naive() {
        return Due::No;
    }
    Due::Fresh {
        watermark: latest.started_at.max(enabled_at),
        skip: HashSet::new(),
    }
}

/// A new run's id: its start, to the second.
pub fn run_id(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// The stems to ask about, newest first (§7.2): items that arrived after the
/// watermark (their `captured:` time, else the file's), minus `skip`. An
/// item with no time at all is never a candidate.
pub fn candidates<'a>(
    items: impl IntoIterator<Item = (&'a str, Option<DateTime<Utc>>)>,
    watermark: DateTime<Utc>,
    skip: &HashSet<String>,
) -> Vec<String> {
    let mut found: Vec<(DateTime<Utc>, &str)> = items
        .into_iter()
        .filter_map(|(stem, arrived)| Some((arrived?, stem)))
        .filter(|(arrived, stem)| *arrived > watermark && !skip.contains(*stem))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(a.1)));
    found
        .into_iter()
        .map(|(_, stem)| stem.to_string())
        .collect()
}

/// The bar (§7.2): Jev chose `archive` at or above the threshold.
pub fn clears_bar(suggestion: &Suggestion, threshold: f64) -> bool {
    suggestion.destination == Some(Destination::Archive) && suggestion.confidence >= threshold
}

/// The guardrail (G6): any message from the account's address, or written
/// by one of the person's names, means the thread is theirs. Rust checks
/// this, never the model.
pub fn person_wrote(thread: &EmailThread, account: Option<&str>, names: &[String]) -> bool {
    let account = account
        .map(|account| account.trim().to_lowercase())
        .filter(|account| !account.is_empty());
    thread.messages.iter().any(|message| {
        let from_account = account
            .as_deref()
            .is_some_and(|account| message.from.to_lowercase().contains(account));
        let by_name = message.author.as_deref().is_some_and(|author| {
            names.iter().any(|name| {
                !name.trim().is_empty() && author.trim().eq_ignore_ascii_case(name.trim())
            })
        });
        from_account || by_name
    })
}

/// The display names the account sends under, read off `From` headers that
/// carry its address (`Diego Tavares <diego@example.com>`): how a ticket
/// system relaying the person is likely to name them.
pub fn account_names<'a>(froms: impl IntoIterator<Item = &'a str>, account: &str) -> Vec<String> {
    let account = account.trim().to_lowercase();
    if account.is_empty() {
        return Vec::new();
    }
    let mut names: Vec<String> = Vec::new();
    for from in froms {
        if !from.to_lowercase().contains(&account) {
            continue;
        }
        let Some((name, _)) = from.split_once('<') else {
            continue;
        };
        let name = name.trim().trim_matches('"').trim();
        if !name.is_empty() && !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            names.push(name.to_string());
        }
    }
    names
}

/// The run the Auto-archived group shows (§8.1): the latest that archived
/// anything, so a run that cleared nothing (a manual one, say) never empties
/// the group.
pub fn group_run(log: &RunLog) -> Option<&Run> {
    log.runs.iter().rev().find(|run| !run.archived.is_empty())
}

/// An item of the view's Auto-archived group.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupItem {
    pub run: String,
    pub item: ArchivedItem,
}

/// The run's filings that nobody has corrected yet (§8.1), in the order they
/// were filed: not sent back (`undone` on the run's record) and not re-filed
/// (a record that `corrects` the run). Whether the file is still in
/// `archives/inbox/` is the caller's to check.
pub fn group_items(run: &Run, decisions: &str) -> Vec<GroupItem> {
    let mut corrected: HashSet<String> = HashSet::new();
    for record in decisions
        .lines()
        .filter_map(|line| serde_json::from_str::<DecisionRecord>(line).ok())
    {
        let undone_here =
            record.via == VIA_AUTO && record.undone && record.run.as_deref() == Some(&run.id);
        let refiled = !record.undone && record.corrects.as_deref() == Some(&run.id);
        if undone_here || refiled {
            corrected.insert(record.stem);
        }
    }
    run.archived
        .iter()
        .filter(|item| !corrected.contains(&item.stem))
        .map(|item| GroupItem {
            run: run.id.clone(),
            item: item.clone(),
        })
        .collect()
}

/// Where auto-archive stands, for the group header and the sync popover
/// (§9).
#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Off,
    /// On, but `triage-policy.md` has no `refreshed:` stamp.
    WaitingForPolicy,
    /// On, but there is no grant to ask Jev with.
    Unavailable,
    /// On, and no run has happened yet.
    Pending,
    Ran {
        day: NaiveDate,
        archived: usize,
    },
    Stopped {
        processed: usize,
        candidates: usize,
        reason: String,
        /// There is a grant with allowance left, so Retry can work.
        retryable: bool,
    },
}

/// The status from the config, the policy stamp, the grant and the log.
/// `day_of` turns a record's time into the person's local day.
pub fn status(
    on: bool,
    stamped: bool,
    available: bool,
    log: &RunLog,
    day_of: impl Fn(DateTime<Utc>) -> NaiveDate,
) -> RunStatus {
    if !on {
        return RunStatus::Off;
    }
    if !stamped {
        return RunStatus::WaitingForPolicy;
    }
    let latest = log.latest();
    if let Some(run) = latest
        && let Some(reason) = run.stopped()
    {
        return RunStatus::Stopped {
            processed: run.archived.len() + run.skipped.len(),
            candidates: run.candidates,
            reason: reason.to_string(),
            retryable: available,
        };
    }
    if !available {
        return RunStatus::Unavailable;
    }
    match latest {
        Some(run) => match &run.finished {
            Some(finished) => RunStatus::Ran {
                day: day_of(finished.at),
                archived: run.archived.len(),
            },
            None => RunStatus::Pending,
        },
        None => RunStatus::Pending,
    }
}

/// `this morning` for today, the weekday within the week, else the date.
pub fn run_day_words(day: NaiveDate, today: NaiveDate) -> String {
    let days = (today - day).num_days();
    if days <= 0 {
        "this morning".to_string()
    } else if days < 7 {
        weekday_name(day.weekday()).to_string()
    } else {
        format!("on {}", day.format("%b %-d"))
    }
}

fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

/// The status line that takes the group header's place (§5.1, §7.4), or
/// `None` when the header reads as the group's own title.
pub fn status_line(status: &RunStatus) -> Option<String> {
    match status {
        RunStatus::WaitingForPolicy => Some("Auto-archive waiting for a policy".to_string()),
        RunStatus::Unavailable => {
            Some("Auto-archive needs suggestions to be available".to_string())
        }
        // Retry is offered beside the line only when it can work; without a
        // grant or allowance the line says why instead.
        RunStatus::Stopped {
            processed,
            candidates,
            retryable,
            ..
        } => Some(if *retryable {
            format!("Auto-archive stopped after {processed} of {candidates}")
        } else {
            format!("Auto-archive stopped after {processed} of {candidates} · allowance spent")
        }),
        RunStatus::Off | RunStatus::Pending | RunStatus::Ran { .. } => None,
    }
}

/// The group header when there is no status to show: `Auto-archived this
/// morning · 7`.
pub fn group_title(run_day: NaiveDate, today: NaiveDate, count: usize) -> String {
    format!("Auto-archived {} · {count}", run_day_words(run_day, today))
}

/// The sync popover's Inbox line (§9), `None` when there is nothing to say.
pub fn popover_line(status: &RunStatus, today: NaiveDate) -> Option<String> {
    match status {
        RunStatus::Ran { day, archived } if *day == today => Some(if *archived == 0 {
            "Auto-archive ran · nothing to clear".to_string()
        } else {
            format!("Auto-archived {archived} today")
        }),
        status => status_line(status),
    }
}

/// `.thock/config.toml` without the two V56 keys in `[inbox]`
/// (`thock::TurnOffInboxAutoArchive`, §5.3). Everything else, comments and
/// order included, stays as written.
pub fn without_auto_archive_keys(text: &str) -> Result<String> {
    let mut document: toml_edit::DocumentMut =
        text.parse().context("parsing .thock/config.toml")?;
    if let Some(table) = document
        .get_mut("inbox")
        .and_then(|item| item.as_table_like_mut())
    {
        table.remove("auto_archive");
        table.remove("auto_archive_threshold");
    }
    Ok(document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox_email::parse_thread;
    use crate::inbox_suggest::parse_response;
    use crate::vault::{VaultConfig, parse_vault_config};

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a valid time")
            .with_timezone(&Utc)
    }

    fn suggestion(choice: &str, confidence: f64) -> Suggestion {
        parse_response(&format!(
            r#"{{"answers":{{"destination":{{"choice":"{choice}","confidence":{confidence}}}}}}}"#
        ))
        .expect("a valid answer")
    }

    fn log(events: &[RunEvent]) -> RunLog {
        let text: String = events
            .iter()
            .map(|event| event.to_json_line().expect("serializes") + "\n")
            .collect();
        RunLog::parse(&text)
    }

    fn started(run: &str, when: &str, watermark: &str) -> RunEvent {
        RunEvent::Started {
            run: run.to_string(),
            at: at(when),
            watermark: at(watermark),
            policy: "p".to_string(),
            threshold: 0.95,
            candidates: 3,
        }
    }

    fn archived(run: &str, stem: &str) -> RunEvent {
        RunEvent::Archived {
            run: run.to_string(),
            stem: stem.to_string(),
            path: format!("archives/inbox/{stem}.md"),
            confidence: 0.97,
            line: format!("- 2026-11-08 · {stem} → Archived · auto"),
        }
    }

    fn finished(run: &str, when: &str, stopped: Option<&str>) -> RunEvent {
        RunEvent::Finished {
            run: run.to_string(),
            at: at(when),
            archived: 1,
            skipped: 0,
            stopped: stopped.map(str::to_string),
        }
    }

    #[test]
    fn the_threshold_clamps_at_the_filled_chip() {
        assert_eq!(effective_threshold(None), 0.95);
        assert_eq!(effective_threshold(Some(0.97)), 0.97);
        assert_eq!(effective_threshold(Some(0.5)), 0.85);
        assert_eq!(effective_threshold(Some(1.5)), 1.0);
        assert_eq!(effective_threshold(Some(f64::NAN)), 0.95);
    }

    #[test]
    fn the_bar_is_archive_at_the_threshold() {
        assert!(clears_bar(&suggestion("archive", 0.95), 0.95));
        assert!(!clears_bar(&suggestion("archive", 0.94), 0.95));
        assert!(!clears_bar(&suggestion("unsure", 0.99), 0.95));
        assert!(!clears_bar(&suggestion("soon", 0.99), 0.95));
        let clamped = effective_threshold(Some(0.6));
        assert!(clears_bar(&suggestion("archive", 0.85), clamped));
        assert!(!clears_bar(&suggestion("archive", 0.84), clamped));
    }

    #[test]
    fn candidates_are_what_arrived_after_the_watermark_newest_first() {
        let watermark = at("2026-11-07T07:00:00Z");
        let items = [
            ("before", Some(at("2026-11-06T20:00:00Z"))),
            ("captured-late", Some(at("2026-11-08T06:00:00Z"))),
            ("file-time", Some(at("2026-11-07T09:00:00Z"))),
            ("no-time", None),
            ("on-the-mark", Some(watermark)),
            ("recorded", Some(at("2026-11-08T05:00:00Z"))),
        ];
        let skip: HashSet<String> = ["recorded".to_string()].into();
        assert_eq!(
            candidates(items, watermark, &skip),
            ["captured-late", "file-time"]
        );
    }

    #[test]
    fn a_thread_the_person_wrote_in_is_skipped() {
        let body = "## Ana <ana@example.com> — 2026-11-07 09:00\n\nCan you look?\n\n\
                    ## Diego Tavares <diego@example.com> — 2026-11-07 10:00\n\nOn it.\n";
        let thread = parse_thread(body, Some("Ana <ana@example.com>"));
        assert!(person_wrote(&thread, Some("Diego@Example.com"), &[]));
        assert!(!person_wrote(&thread, Some("someone@example.com"), &[]));
        assert!(!person_wrote(&thread, None, &[]));

        let ticket = "## helpdesk@example.com — 2026-11-07 09:00\n\n\
                      Comment #2 (by D. Tavares on 11/07/26 09:00am):\n\nTaking this.\n";
        let thread = parse_thread(ticket, Some("helpdesk@example.com"));
        assert!(
            thread
                .messages
                .iter()
                .any(|message| message.author.is_some()),
            "{thread:?}"
        );
        let author = thread
            .messages
            .iter()
            .find_map(|message| message.author.clone())
            .unwrap_or_default();
        assert!(person_wrote(
            &thread,
            Some("diego@example.com"),
            std::slice::from_ref(&author)
        ));
        assert!(!person_wrote(
            &thread,
            Some("diego@example.com"),
            &["Ana".to_string()]
        ));

        assert_eq!(
            account_names(
                [
                    "\"Diego Tavares\" <diego@example.com>",
                    "Ana <ana@example.com>",
                    "diego@example.com",
                    "diego tavares <DIEGO@example.com>",
                ],
                "diego@example.com"
            ),
            ["Diego Tavares"]
        );
    }

    #[test]
    fn once_a_day_and_a_crash_resumes() {
        let today = at("2026-11-08T08:00:00Z");
        let enabled = RunEvent::Enabled {
            at: at("2026-11-01T12:00:00Z"),
        };
        // Never ran: everything since it was turned on.
        assert_eq!(
            due(&log(std::slice::from_ref(&enabled)), &today, false),
            Due::Fresh {
                watermark: at("2026-11-01T12:00:00Z"),
                skip: HashSet::new()
            }
        );

        let yesterday = [
            enabled.clone(),
            started("r-1", "2026-11-07T07:00:00Z", "2026-11-01T12:00:00Z"),
            archived("r-1", "a"),
            finished("r-1", "2026-11-07T07:01:00Z", None),
        ];
        assert_eq!(
            due(&log(&yesterday), &today, false),
            Due::Fresh {
                watermark: at("2026-11-07T07:00:00Z"),
                skip: HashSet::new()
            }
        );

        let ran_today = [
            started("r-2", "2026-11-08T07:00:00Z", "2026-11-07T07:00:00Z"),
            finished("r-2", "2026-11-08T07:01:00Z", None),
        ];
        let mut events = yesterday.to_vec();
        events.extend(ran_today);
        assert_eq!(due(&log(&events), &today, false), Due::No);
        // By hand it runs again, from this morning's start.
        assert_eq!(
            due(&log(&events), &today, true),
            Due::Fresh {
                watermark: at("2026-11-08T07:00:00Z"),
                skip: HashSet::new()
            }
        );

        // Quit mid-run: the next poll picks it up and skips what it filed.
        let crashed = [
            enabled,
            started("r-3", "2026-11-08T07:00:00Z", "2026-11-07T07:00:00Z"),
            archived("r-3", "a"),
            RunEvent::Skipped {
                run: "r-3".to_string(),
                stem: "b".to_string(),
                reason: SkipReason::Below,
            },
        ];
        assert_eq!(
            due(&log(&crashed), &today, false),
            Due::Resume {
                run: "r-3".to_string(),
                watermark: at("2026-11-07T07:00:00Z"),
                skip: ["a".to_string(), "b".to_string()].into(),
            }
        );
    }

    #[test]
    fn a_stopped_run_waits_for_retry_or_tomorrow() {
        let events = [
            RunEvent::Enabled {
                at: at("2026-11-01T12:00:00Z"),
            },
            started("r-1", "2026-11-08T07:00:00Z", "2026-11-07T07:00:00Z"),
            archived("r-1", "a"),
            finished("r-1", "2026-11-08T07:01:00Z", Some("allowance spent")),
        ];
        let log = log(&events);
        let today = at("2026-11-08T09:00:00Z");
        assert_eq!(due(&log, &today, false), Due::No);
        assert_eq!(
            due(&log, &today, true),
            Due::Resume {
                run: "r-1".to_string(),
                watermark: at("2026-11-07T07:00:00Z"),
                skip: ["a".to_string()].into(),
            }
        );
        assert_eq!(
            due(&log, &at("2026-11-09T07:00:00Z"), false),
            Due::Fresh {
                watermark: at("2026-11-07T07:00:00Z"),
                skip: ["a".to_string()].into(),
            }
        );
        let day = |moment: DateTime<Utc>| moment.date_naive();
        let stopped = status(true, true, true, &log, day);
        assert_eq!(
            stopped,
            RunStatus::Stopped {
                processed: 1,
                candidates: 3,
                reason: "allowance spent".to_string(),
                retryable: true,
            }
        );
        assert_eq!(
            status_line(&stopped).as_deref(),
            Some("Auto-archive stopped after 1 of 3")
        );
        // No grant or allowance: the line says so, and offers nothing.
        let spent = status(true, true, false, &log, day);
        assert!(matches!(
            spent,
            RunStatus::Stopped {
                retryable: false,
                ..
            }
        ));
        assert_eq!(
            status_line(&spent).as_deref(),
            Some("Auto-archive stopped after 1 of 3 · allowance spent")
        );
    }

    #[test]
    fn the_status_reads_off_waiting_and_ran() {
        let day = |moment: DateTime<Utc>| moment.date_naive();
        let today = NaiveDate::from_ymd_opt(2026, 11, 8).expect("a valid date");
        let empty = RunLog::default();
        assert_eq!(status(false, true, true, &empty, day), RunStatus::Off);
        // Cold start: on, but the refresh hasn't stamped the policy yet.
        let waiting = status(true, false, true, &empty, day);
        assert_eq!(waiting, RunStatus::WaitingForPolicy);
        assert_eq!(
            popover_line(&waiting, today).as_deref(),
            Some("Auto-archive waiting for a policy")
        );
        assert_eq!(
            status(true, true, false, &empty, day),
            RunStatus::Unavailable
        );
        assert_eq!(status(true, true, true, &empty, day), RunStatus::Pending);

        let ran = log(&[
            started("r-1", "2026-11-08T07:00:00Z", "2026-11-07T07:00:00Z"),
            archived("r-1", "a"),
            finished("r-1", "2026-11-08T07:01:00Z", None),
        ]);
        let status_now = status(true, true, true, &ran, day);
        assert_eq!(
            status_now,
            RunStatus::Ran {
                day: today,
                archived: 1
            }
        );
        assert_eq!(
            popover_line(&status_now, today).as_deref(),
            Some("Auto-archived 1 today")
        );
        let nothing = RunStatus::Ran {
            day: today,
            archived: 0,
        };
        assert_eq!(
            popover_line(&nothing, today).as_deref(),
            Some("Auto-archive ran · nothing to clear")
        );
        assert_eq!(
            group_title(today, today, 7),
            "Auto-archived this morning · 7"
        );
        let friday = NaiveDate::from_ymd_opt(2026, 11, 6).expect("a valid date");
        assert_eq!(group_title(friday, today, 7), "Auto-archived Friday · 7");
        let older = NaiveDate::from_ymd_opt(2026, 10, 30).expect("a valid date");
        assert_eq!(run_day_words(older, today), "on Oct 30");
    }

    #[test]
    fn the_group_drops_what_was_sent_back_or_re_filed() {
        let ran = log(&[
            started("r-1", "2026-11-08T07:00:00Z", "2026-11-07T07:00:00Z"),
            archived("r-1", "a"),
            archived("r-1", "b"),
            archived("r-1", "c"),
            finished("r-1", "2026-11-08T07:01:00Z", None),
        ]);
        let run = ran.latest().expect("a run");
        let record =
            |stem: &str, via: &str, undone: bool, run: Option<&str>, corrects: Option<&str>| {
                serde_json::to_string(&DecisionRecord {
                    stem: stem.to_string(),
                    suggested: Some("archive".to_string()),
                    confidence: Some(0.97),
                    chosen: "archive".to_string(),
                    via: via.to_string(),
                    at: "2026-11-08T07:00:00Z".to_string(),
                    undone,
                    run: run.map(str::to_string),
                    corrects: corrects.map(str::to_string),
                })
                .expect("serializes")
            };
        let decisions = [
            record("a", VIA_AUTO, true, Some("r-1"), None),
            record("b", VIA_AUTO, false, Some("r-1"), None),
            record("c", VIA_AUTO, false, Some("r-1"), None),
            record("c", "s", false, None, Some("r-1")),
            // An older run's undo doesn't reach this one.
            record("b", VIA_AUTO, true, Some("r-0"), None),
        ]
        .join("\n");
        let stems: Vec<String> = group_items(run, &decisions)
            .into_iter()
            .map(|item| item.item.stem)
            .collect();
        assert_eq!(stems, ["b"]);
    }

    #[test]
    fn records_round_trip_with_the_specs_fields() {
        let line = started("r-1", "2026-11-08T07:00:00Z", "2026-11-07T07:00:00Z")
            .to_json_line()
            .expect("serializes");
        for field in [
            r#""event":"started""#,
            r#""run":"r-1""#,
            r#""watermark":"#,
            r#""policy":"p""#,
            r#""threshold":0.95"#,
        ] {
            assert!(line.contains(field), "{line}");
        }
        let skipped = RunEvent::Skipped {
            run: "r-1".to_string(),
            stem: "a".to_string(),
            reason: SkipReason::Replied,
        };
        let line = skipped.to_json_line().expect("serializes");
        assert!(line.contains(r#""reason":"replied""#), "{line}");
        assert_eq!(
            serde_json::from_str::<RunEvent>(&line).expect("parses"),
            skipped
        );
        let done = finished("r-1", "2026-11-08T07:01:00Z", None)
            .to_json_line()
            .expect("serializes");
        assert!(!done.contains("stopped"), "{done}");
        assert_eq!(RunLog::parse("not json\n{}\n"), RunLog::default());
        assert_eq!(run_id(at("2026-11-08T07:00:12Z")), "20261108T070012Z");
    }

    #[test]
    fn the_stamp_is_read_from_front_matter_only() {
        let stamped = "---\nrefreshed: 2026-11-08\n---\n# Triage policy\n";
        assert_eq!(
            policy_refreshed(stamped),
            NaiveDate::from_ymd_opt(2026, 11, 8)
        );
        assert_eq!(
            policy_refreshed("# Triage policy\n\nrefreshed: 2026-11-08\n"),
            None
        );
        assert_eq!(
            policy_refreshed("---\ntitle: x\n---\nrefreshed: 2026-11-08\n"),
            None
        );
        assert_eq!(policy_refreshed("---\nrefreshed: soon\n---\n"), None);
        assert_eq!(
            policy_refreshed(include_str!("../assets/routines/inbox/triage-policy.md")),
            None,
            "the shipped defaults are never a refreshed policy (G2)"
        );
    }

    #[test]
    fn turning_off_removes_only_the_v56_keys() {
        let config = "schema = 1\n\n# Mine.\n[inbox]\nsuggestions = false\nauto_archive = true\n\
                      auto_archive_threshold = 0.97\nnames = [\"D. Tavares\"]\n\n[backlog]\nfile = \"todo.md\"\n";
        let off = without_auto_archive_keys(config).expect("edits");
        assert_eq!(
            off,
            "schema = 1\n\n# Mine.\n[inbox]\nsuggestions = false\nnames = [\"D. Tavares\"]\n\n[backlog]\nfile = \"todo.md\"\n"
        );
        let parsed = parse_vault_config(&off).expect("parses");
        assert!(!parsed.inbox.auto_archive);
        assert!(!parsed.inbox.suggestions);
        assert_eq!(parsed.inbox.names, ["D. Tavares"]);
        assert_eq!(
            without_auto_archive_keys("schema = 1\n").expect("edits"),
            "schema = 1\n"
        );
        assert_eq!(VaultConfig::default().inbox.auto_archive, false);
    }

    const REFRESH_SKILL: &str =
        include_str!("../assets/routines/inbox/skills/refresh-inbox-policy.md");

    /// The fenced block right after the paragraph containing `marker`.
    fn block_after<'a>(skill: &'a str, marker: &str) -> &'a str {
        let start = skill
            .find(marker)
            .unwrap_or_else(|| panic!("{marker:?} in the skill"));
        let rest = &skill[start..];
        let open = rest.find("```").expect("a fenced block");
        let rest = &rest[open..];
        let body_start = rest.find('\n').expect("a fence line") + 1;
        let body = &rest[body_start..];
        let end = body.find("```").expect("a closing fence");
        &body[..end]
    }

    /// Does what the skill's `enable` step tells the agent: the line under
    /// an existing `[inbox]`, else the table at the end.
    fn enable_as_the_skill_says(config: &str) -> String {
        let line = block_after(REFRESH_SKILL, "already has an `[inbox]` table");
        let table = block_after(REFRESH_SKILL, "has no `[inbox]` table");
        match config.find("[inbox]\n") {
            Some(at) => {
                let insert_at = at + "[inbox]\n".len();
                format!("{}{}{}", &config[..insert_at], line, &config[insert_at..])
            }
            None => {
                let mut out = config.to_string();
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
                out.push_str(table);
                out
            }
        }
    }

    #[test]
    fn the_enable_step_writes_toml_the_config_reads() {
        let with_table =
            "schema = 1\n\n[inbox]\nsuggestions = true\n\n[backlog]\nfile = \"backlog.md\"\n";
        let enabled = enable_as_the_skill_says(with_table);
        assert!(
            enabled.starts_with("schema = 1\n\n[inbox]\nauto_archive = true\n"),
            "{enabled}"
        );
        let parsed = parse_vault_config(&enabled).expect("parses");
        assert!(parsed.inbox.auto_archive);
        assert!(parsed.inbox.suggestions);

        let without = "schema = 1\n\n[backlog]\nfile = \"backlog.md\"\n";
        let enabled = enable_as_the_skill_says(without);
        let parsed = parse_vault_config(&enabled).expect("parses");
        assert!(parsed.inbox.auto_archive);
        assert_eq!(parsed.backlog.file, "backlog.md");
        // And turning it off undoes exactly that.
        let off = without_auto_archive_keys(&enabled).expect("edits");
        assert!(!parse_vault_config(&off).expect("parses").inbox.auto_archive);
    }

    #[test]
    fn the_policy_the_skill_writes_reads_back_without_fallbacks() {
        use crate::inbox_suggest::{flatten_policy, parse_criteria};
        let policy = block_after(REFRESH_SKILL, "The file you write has this shape");
        assert_eq!(
            policy_refreshed(policy),
            NaiveDate::from_ymd_opt(2026, 11, 8)
        );
        let rows = flatten_policy(policy);
        assert_eq!(
            rows.first().map(String::as_str),
            Some("A ticket under OpenCue about Maya or Houdini licences → archive")
        );
        assert_eq!(rows.len(), 5, "{rows:?}");
        let criteria = parse_criteria(policy);
        assert!(criteria.fallbacks.is_empty(), "{criteria:?}");
        assert_eq!(
            criteria.get("archive"),
            Some(
                "Nothing for this person to act on: a notification, receipt, newsletter, test \
                 capture or duplicate, or a ticket that belongs to another team under the policy."
            )
        );
        // The shipped policy carries the same sentences, so a vault that
        // never refreshes asks the same question.
        let shipped = parse_criteria(include_str!("../assets/routines/inbox/triage-policy.md"));
        assert!(shipped.fallbacks.is_empty(), "{shipped:?}");
        assert_eq!(shipped.sentences, criteria.sentences);
    }
}
