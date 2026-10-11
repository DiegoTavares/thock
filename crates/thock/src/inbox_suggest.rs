//! Suggested destinations for inbox items (spec `v41-inbox-desk-view.md` §7):
//! the typed question the desk asks Jev, TypeSafe's decision model on
//! OpenRouter, how its probabilities become a chip, and the two append-only
//! records behind the header's cost and the threshold study. Pure
//! string-in/string-out — no I/O, no GPUI; the service makes the call.

use anyhow::{Context as _, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::inbox::InboxItem;

pub const JEV_MODEL: &str = "typesafe/jev-1.13";
pub const DECISIONS_ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";
/// The gateway provider whose key reaches the endpoint (decision 2).
pub const OPENROUTER_PROVIDER: &str = "openrouter";
/// How much of a body travels with the question, trimmed at a line boundary.
pub const EXCERPT_LIMIT: usize = 1200;
/// Requests in flight at once while the view is open (§7.3).
pub const MAX_IN_FLIGHT: usize = 4;
/// A filled chip at or above this confidence (§7.4).
pub const FILLED_THRESHOLD: f64 = 0.85;
/// An outlined chip at or above this confidence; below it, no chip.
pub const OUTLINED_THRESHOLD: f64 = 0.5;

/// The shipped policy, sent when `triage-policy.md` is missing or has no
/// Proposals table (§7.2).
pub const DEFAULT_POLICY: [&str; 5] = [
    "A link or article to read → someday, as a task carrying the link",
    "A raw idea or thought → someday",
    "Something with a due date within a week, or clearly urgent wording → soon",
    "Something that reads like today's business (an errand, a call, a follow-up) → today",
    "Obvious noise (a test capture, an empty item, a duplicate) → archive",
];

/// The four filing options a key can take. `unsure` is the fifth answer Jev
/// may give and is not a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Destination {
    Today,
    Soon,
    Someday,
    Archive,
}

impl Destination {
    pub const ALL: [Destination; 4] = [Self::Today, Self::Soon, Self::Someday, Self::Archive];

    /// The option word Jev answers with and the records store.
    pub fn key(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Soon => "soon",
            Self::Someday => "someday",
            Self::Archive => "archive",
        }
    }

    /// The word on the chip and the keycap.
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Soon => "Soon",
            Self::Someday => "Someday",
            Self::Archive => "Archive",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.key() == key)
    }
}

/// The *Proposals* table of `routines/inbox/triage-policy.md`, each row
/// flattened to `<looks like> → <destination word>` (§7.2). Rows that file a
/// note into a folder or append to a page map to `unsure`, the ritual's
/// business. Empty when the file has no readable table.
pub fn flatten_policy(markdown: &str) -> Vec<String> {
    let mut in_proposals = false;
    let mut rows = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix('#') {
            in_proposals = heading.trim_start_matches('#').trim() == "Proposals";
            continue;
        }
        if !in_proposals || !trimmed.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        let [looks_like, proposal, ..] = cells.as_slice() else {
            continue;
        };
        let is_rule = looks_like
            .chars()
            .all(|character| matches!(character, '-' | ':' | ' '));
        if is_rule || looks_like.eq_ignore_ascii_case("If it looks like…") {
            continue;
        }
        let looks_like = looks_like.replace('`', "");
        if looks_like.is_empty() {
            continue;
        }
        rows.push(format!("{looks_like} → {}", destination_word(proposal)));
    }
    rows
}

/// A proposal cell's destination word: the policy's own vocabulary mapped
/// onto Jev's five options.
fn destination_word(proposal: &str) -> &'static str {
    let lower = proposal.to_lowercase();
    if lower.contains("someday") {
        "someday"
    } else if lower.contains("soon") {
        "soon"
    } else if lower.contains("today") {
        "today"
    } else if lower.contains("discard") || lower.contains("archive") || lower.contains("delete") {
        "archive"
    } else {
        "unsure"
    }
}

/// `due:` against today, in words Jev reads literally (§7.2): a date within
/// the next seven days, or already past, is `within a week`.
pub fn due_in(due: Option<&str>, today: NaiveDate) -> &'static str {
    let Some(due) = due.and_then(|due| NaiveDate::parse_from_str(due.trim(), "%Y-%m-%d").ok())
    else {
        return "none";
    };
    if (due - today).num_days() <= 7 {
        "within a week"
    } else {
        "later"
    }
}

/// The body after the heading, trimmed to [`EXCERPT_LIMIT`] characters at a
/// line boundary. A picture contributes only its alt text (§7.2).
pub fn excerpt(body: &str) -> String {
    let mut lines = body.lines().map(str::trim_end).peekable();
    while lines.peek().is_some_and(|line| line.trim().is_empty()) {
        lines.next();
    }
    if lines.peek().is_some_and(|line| line.starts_with("# ")) {
        lines.next();
    }
    let mut out = String::new();
    let mut length = 0usize;
    for line in lines {
        let line = alt_text_only(line);
        let line_length = line.chars().count() + usize::from(!out.is_empty());
        if length + line_length > EXCERPT_LIMIT {
            if out.is_empty() {
                out = line.chars().take(EXCERPT_LIMIT).collect();
            }
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&line);
        length += line_length;
    }
    out.trim().to_string()
}

/// The item's excerpt without the url line the planner echoes under the
/// heading: it already travels as `url`.
fn item_excerpt(item: &InboxItem) -> String {
    let text = excerpt(&item.body);
    match &item.url {
        Some(url) => text
            .lines()
            .filter(|line| line.trim() != url.trim())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
        None => text,
    }
}

fn alt_text_only(line: &str) -> String {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("![")
        && let Some((alt, _)) = rest.split_once("](")
    {
        return alt.trim().to_string();
    }
    line.to_string()
}

/// The decision request (§7.2): the item and the policy as `state`, one
/// `choice` question with full-sentence criteria in a fixed order.
pub fn build_request(item: &InboxItem, policy: &[String], today: NaiveDate) -> Value {
    json!({
        "model": JEV_MODEL,
        "state": {
            "item": {
                "title": item.title,
                "source": item.source.clone().unwrap_or_default(),
                "from": item.from.clone().unwrap_or_default(),
                "url": item.url.clone().unwrap_or_default(),
                "excerpt": item_excerpt(item),
                "due_in": due_in(item.due.as_deref(), today),
                "has_image": item.has_image,
            },
            "policy": policy,
        },
        "questions": {
            "destination": {
                "type": "choice",
                "instructions": "Where should `item` go, following `policy`?",
                "criteria": {
                    "today": "Something to do, call, or read today, or a line for today's page.",
                    "soon": "A task for the coming days: `item.due_in` is within a week, or the wording is urgent.",
                    "someday": "A link, article, idea or task with no date pressure.",
                    "archive": "Noise with nothing to act on: a notification, receipt, newsletter, test capture or duplicate.",
                    "unsure": "None of the above fits, or the item is a note that belongs in a folder of its own.",
                },
            },
        },
    })
}

/// What Jev answered for one item (§7.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// `None` when the answer was `unsure`.
    pub destination: Option<Destination>,
    pub choice: String,
    /// 0–1, the peakedness of the distribution; 0 when the answer carried
    /// none, which lands in the no-chip tier.
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
    /// USD, from `usage.cost`.
    pub cost: f64,
}

#[derive(Deserialize)]
struct DecisionsResponse {
    #[serde(default)]
    answers: BTreeMap<String, Answer>,
    #[serde(default)]
    usage: Usage,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    choice: Option<String>,
    #[serde(default)]
    probabilities: BTreeMap<String, f64>,
    #[serde(default)]
    confidence: Option<f64>,
}

#[derive(Default, Deserialize)]
struct Usage {
    #[serde(default)]
    cost: f64,
}

/// Reads a decisions response. A missing `choice` falls back to the most
/// probable option; a missing `confidence` reads as 0, so the row shows no
/// chip rather than a confident one.
pub fn parse_response(text: &str) -> Result<Suggestion> {
    let response: DecisionsResponse =
        serde_json::from_str(text).context("reading the suggestion response")?;
    let answer = response
        .answers
        .get("destination")
        .context("the suggestion response has no `destination` answer")?;
    let choice = answer
        .choice
        .clone()
        .or_else(|| {
            answer
                .probabilities
                .iter()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(option, _)| option.clone())
        })
        .context("the suggestion response names no choice")?;
    Ok(Suggestion {
        destination: Destination::from_key(&choice),
        choice,
        confidence: answer.confidence.unwrap_or(0.0).clamp(0.0, 1.0),
        probabilities: answer.probabilities.clone(),
        cost: response.usage.cost,
    })
}

/// How a suggestion shows on its row (§7.4). The tier never changes what the
/// view does on its own; it changes what `space` will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Confident: a filled chip, `space` files.
    Filled,
    /// Plausible: an outlined chip, `space` files.
    Outlined,
    /// Below the line, or `unsure`: no chip, `space` opens the note.
    None,
}

pub fn tier(suggestion: &Suggestion) -> Tier {
    if suggestion.destination.is_none() {
        return Tier::None;
    }
    if suggestion.confidence >= FILLED_THRESHOLD {
        Tier::Filled
    } else if suggestion.confidence >= OUTLINED_THRESHOLD {
        Tier::Outlined
    } else {
        Tier::None
    }
}

/// One line of `.thock/state/inbox/suggestions.jsonl` (§7.5), keyed by the
/// note's byte hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuggestionRecord {
    pub hash: String,
    pub stem: String,
    pub model: String,
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
    pub cost: f64,
    pub asked_at: String,
}

impl SuggestionRecord {
    pub fn new(hash: &str, stem: &str, suggestion: &Suggestion, asked_at: &str) -> Self {
        Self {
            hash: hash.to_string(),
            stem: stem.to_string(),
            model: JEV_MODEL.to_string(),
            choice: suggestion.choice.clone(),
            confidence: suggestion.confidence,
            probabilities: suggestion.probabilities.clone(),
            cost: suggestion.cost,
            asked_at: asked_at.to_string(),
        }
    }

    pub fn suggestion(&self) -> Suggestion {
        Suggestion {
            destination: Destination::from_key(&self.choice),
            choice: self.choice.clone(),
            confidence: self.confidence,
            probabilities: self.probabilities.clone(),
            cost: self.cost,
        }
    }
}

/// One line of `.thock/state/inbox/decisions.jsonl` (§7.6): what was
/// suggested against what the person chose, and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub stem: String,
    pub suggested: Option<String>,
    pub confidence: Option<f64>,
    pub chosen: String,
    /// The key: `space`, `t`, `s`, `S`, `e`, or `menu`.
    pub via: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub undone: bool,
}

/// What the decision log says so far (§7.6): how often the suggestion was
/// taken, over how many decisions that had one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecisionReport {
    pub decisions: usize,
    pub taken: usize,
}

impl DecisionReport {
    pub fn from_log(text: &str) -> Self {
        let mut report = Self::default();
        for line in text.lines() {
            let Ok(record) = serde_json::from_str::<DecisionRecord>(line) else {
                continue;
            };
            if record.undone {
                continue;
            }
            let Some(suggested) = &record.suggested else {
                continue;
            };
            report.decisions += 1;
            if *suggested == record.chosen {
                report.taken += 1;
            }
        }
        report
    }

    /// `suggestion taken 83% of the time, 212 decisions`, or `None` with
    /// nothing to report.
    pub fn summary(&self) -> Option<String> {
        if self.decisions == 0 {
            return None;
        }
        let percent = self.taken * 100 / self.decisions;
        Some(format!(
            "suggestion taken {percent}% of the time, {} decision{}",
            self.decisions,
            if self.decisions == 1 { "" } else { "s" }
        ))
    }
}

/// The sum of `cost` over the cache's records whose `asked_at` falls in the
/// given `YYYY-MM` month, for the sync popover's Inbox row (§7.5).
pub fn month_cost(cache: &str, month: &str) -> f64 {
    cache
        .lines()
        .filter_map(|line| serde_json::from_str::<SuggestionRecord>(line).ok())
        .filter(|record| record.asked_at.starts_with(month))
        .map(|record| record.cost)
        .sum()
}

/// A dollar figure for the header: four decimals under a cent, two above.
pub fn format_cost(cost: f64) -> String {
    if cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::parse_inbox_note;

    const SHIPPED_POLICY: &str = include_str!("../assets/routines/inbox/triage-policy.md");
    const JEV_CHOICE: &str = include_str!("test_fixtures/jev_choice.json");

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 10).unwrap()
    }

    fn policy() -> Vec<String> {
        DEFAULT_POLICY.iter().map(|row| row.to_string()).collect()
    }

    #[test]
    fn request_for_a_link_matches_the_spec_golden() {
        let item = parse_inbox_note(
            "inbox/2026-10-09-1842-ship-it.md",
            "---\nsource:   google-tasks\ncapture:  3f1c3f1c3f1c\ncaptured: 2026-10-09T18:42:00Z\n\
             title:    Ship it — a practical guide to shipping\nurl:      https://example.com/ship-it\n---\n\n\
             # Ship it — a practical guide to shipping\n\nhttps://example.com/ship-it\n",
        );
        let request = build_request(&item, &policy(), today());
        let expected: Value = serde_json::from_str(
            r##"{
              "model": "typesafe/jev-1.13",
              "state": {
                "item": {
                  "title":   "Ship it — a practical guide to shipping",
                  "source":  "google-tasks",
                  "from":    "",
                  "url":     "https://example.com/ship-it",
                  "excerpt": "",
                  "due_in":  "none",
                  "has_image": false
                },
                "policy": [
                  "A link or article to read → someday, as a task carrying the link",
                  "A raw idea or thought → someday",
                  "Something with a due date within a week, or clearly urgent wording → soon",
                  "Something that reads like today's business (an errand, a call, a follow-up) → today",
                  "Obvious noise (a test capture, an empty item, a duplicate) → archive"
                ]
              },
              "questions": {
                "destination": {
                  "type": "choice",
                  "instructions": "Where should `item` go, following `policy`?",
                  "criteria": {
                    "today":   "Something to do, call, or read today, or a line for today's page.",
                    "soon":    "A task for the coming days: `item.due_in` is within a week, or the wording is urgent.",
                    "someday": "A link, article, idea or task with no date pressure.",
                    "archive": "Noise with nothing to act on: a notification, receipt, newsletter, test capture or duplicate.",
                    "unsure":  "None of the above fits, or the item is a note that belongs in a folder of its own."
                  }
                }
              }
            }"##,
        )
        .unwrap();
        assert_eq!(request, expected);
        // Jev leans to the first option, so the criteria order is part of
        // the contract.
        let keys: Vec<&String> = request["questions"]["destination"]["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(keys, ["today", "soon", "someday", "archive", "unsure"]);
    }

    #[test]
    fn request_trims_long_emails_and_reads_due_dates() {
        let paragraph = "word ".repeat(100).trim_end().to_string();
        let body = format!("# Re: Q4 planning\n\n{paragraph}\n\n{paragraph}\n\n{paragraph}\n");
        let item = parse_inbox_note(
            "inbox/2026-10-09-1900-re-q4.md",
            &format!(
                "---\nsource:   gmail\nfrom:     Marta <marta@example.com>\ntitle:    Re: Q4 planning\n\
                 due:      2026-10-11\n---\n\n{body}"
            ),
        );
        let request = build_request(&item, &policy(), today());
        let excerpt = request["state"]["item"]["excerpt"].as_str().unwrap();
        assert!(excerpt.chars().count() <= EXCERPT_LIMIT);
        // Cut at a line boundary: two whole paragraphs fit, the third does
        // not.
        assert_eq!(excerpt, format!("{paragraph}\n\n{paragraph}"));
        assert_eq!(
            request["state"]["item"]["from"],
            "Marta <marta@example.com>"
        );
        assert_eq!(request["state"]["item"]["due_in"], "within a week");

        assert_eq!(due_in(Some("2026-10-17"), today()), "within a week");
        assert_eq!(due_in(Some("2026-10-18"), today()), "later");
        assert_eq!(due_in(Some("2026-10-01"), today()), "within a week");
        assert_eq!(due_in(Some("soon"), today()), "none");
        assert_eq!(due_in(None, today()), "none");
    }

    #[test]
    fn request_for_a_hand_written_note_with_a_picture() {
        let item = parse_inbox_note(
            "inbox/whiteboard.md",
            "# Whiteboard\n\n![sketch of the routines rail](/images/2026-10-10-0931-whiteboard.jpg)\n\nRoutines could carry state.\n",
        );
        let request = build_request(&item, &policy(), today());
        let state = &request["state"]["item"];
        assert_eq!(state["source"], "");
        assert_eq!(state["has_image"], true);
        // The picture is not evidence Jev can see: only its alt text goes.
        assert_eq!(
            state["excerpt"],
            "sketch of the routines rail\n\nRoutines could carry state."
        );
    }

    #[test]
    fn excerpt_cuts_an_oversized_first_line_at_the_limit() {
        let long = "x".repeat(EXCERPT_LIMIT + 50);
        assert_eq!(
            excerpt(&format!("# T\n{long}\n")).chars().count(),
            EXCERPT_LIMIT
        );
        assert_eq!(excerpt("# Title only\n"), "");
        assert_eq!(excerpt(""), "");
    }

    #[test]
    fn shipped_policy_flattens_to_five_rows_with_the_folder_row_unsure() {
        let rows = flatten_policy(SHIPPED_POLICY);
        assert_eq!(
            rows,
            [
                "A link or article to read → someday",
                "A raw idea or thought → someday",
                "Something with a due: date within a week, or clearly urgent wording → soon",
                "A note about a project or topic that has its own folder or Routine in this vault → unsure",
                "Something that reads like today's business (an errand, a call, a follow-up) → today",
                "Obvious noise (a test capture, an empty item, a duplicate of something already filed) → archive",
            ]
        );
    }

    #[test]
    fn policy_without_a_table_is_empty_so_the_defaults_go() {
        assert!(flatten_policy("# Triage policy\n\nJust prose.\n").is_empty());
        assert!(flatten_policy("").is_empty());
        // A table outside the Proposals section is not the policy.
        assert!(
            flatten_policy("## Destinations\n\n| a | b |\n| --- | --- |\n| x | Soon |\n")
                .is_empty()
        );
        let custom = "## Proposals\n\n| If it looks like… | Propose |\n| --- | --- |\n\
                      | A recipe | Append to `food/recipes.md` |\n| Spam | Discard |\n";
        assert_eq!(
            flatten_policy(custom),
            ["A recipe → unsure", "Spam → archive"]
        );
    }

    #[test]
    fn parses_a_captured_jev_response() {
        let suggestion = parse_response(JEV_CHOICE).unwrap();
        assert_eq!(suggestion.destination, Some(Destination::Someday));
        assert_eq!(suggestion.choice, "someday");
        assert!((suggestion.confidence - 0.91).abs() < 1e-9);
        assert_eq!(suggestion.probabilities.len(), 5);
        assert!((suggestion.probabilities["someday"] - 0.91).abs() < 1e-9);
        assert!((suggestion.cost - 0.0000231).abs() < 1e-12);
        assert_eq!(tier(&suggestion), Tier::Filled);
    }

    #[test]
    fn unsure_and_missing_confidence_never_make_a_chip() {
        let unsure = parse_response(
            r#"{"model":"typesafe/jev-1.13","answers":{"destination":{"choice":"unsure",
                "probabilities":{"today":0.1,"soon":0.1,"someday":0.1,"archive":0.1,"unsure":0.6},
                "confidence":0.95}},"usage":{"cost":0.00001}}"#,
        )
        .unwrap();
        assert_eq!(unsure.destination, None);
        assert_eq!(tier(&unsure), Tier::None);

        let no_confidence = parse_response(
            r#"{"answers":{"destination":{"choice":"soon","probabilities":{"soon":0.9}}}}"#,
        )
        .unwrap();
        assert_eq!(no_confidence.destination, Some(Destination::Soon));
        assert_eq!(no_confidence.confidence, 0.0);
        assert_eq!(no_confidence.cost, 0.0);
        assert_eq!(tier(&no_confidence), Tier::None);

        // No choice: the most probable option stands in.
        let no_choice = parse_response(
            r#"{"answers":{"destination":{"probabilities":{"today":0.2,"archive":0.7},"confidence":0.6}}}"#,
        )
        .unwrap();
        assert_eq!(no_choice.destination, Some(Destination::Archive));

        assert!(parse_response("{}").is_err());
        assert!(parse_response("not json").is_err());
    }

    #[test]
    fn tiers_sit_at_0_85_and_0_5() {
        let mut suggestion = parse_response(JEV_CHOICE).unwrap();
        for (confidence, expected) in [
            (0.85, Tier::Filled),
            (0.8499, Tier::Outlined),
            (0.5, Tier::Outlined),
            (0.4999, Tier::None),
            (0.0, Tier::None),
        ] {
            suggestion.confidence = confidence;
            assert_eq!(tier(&suggestion), expected, "at {confidence}");
        }
    }

    #[test]
    fn records_round_trip_and_report() {
        let suggestion = parse_response(JEV_CHOICE).unwrap();
        let record = SuggestionRecord::new(
            "3f1c",
            "2026-10-09-1842-ship-it",
            &suggestion,
            "2026-10-10T09:14:02Z",
        );
        let line = serde_json::to_string(&record).unwrap();
        let back: SuggestionRecord = serde_json::from_str(&line).unwrap();
        assert_eq!(back, record);
        assert_eq!(back.suggestion(), suggestion);
        assert!(line.starts_with(r#"{"hash":"3f1c","stem":"2026-10-09-1842-ship-it","model":"typesafe/jev-1.13","choice":"someday""#), "{line}");

        let other = SuggestionRecord {
            asked_at: "2026-09-30T23:59:00Z".to_string(),
            cost: 0.5,
            ..record
        };
        let cache = format!(
            "{line}\n{}\nnot json\n",
            serde_json::to_string(&other).unwrap()
        );
        assert!((month_cost(&cache, "2026-10") - 0.0000231).abs() < 1e-12);
        assert_eq!(month_cost(&cache, "2026-09"), 0.5);
        assert_eq!(format_cost(0.0000231), "$0.0000");
        assert_eq!(format_cost(0.0002), "$0.0002");
        assert_eq!(format_cost(0.5), "$0.50");

        let decision = DecisionRecord {
            stem: "a".to_string(),
            suggested: Some("someday".to_string()),
            confidence: Some(0.91),
            chosen: "someday".to_string(),
            via: "space".to_string(),
            at: "2026-10-10T09:14:40Z".to_string(),
            undone: false,
        };
        let line = serde_json::to_string(&decision).unwrap();
        assert!(!line.contains("undone"), "{line}");
        let no_grant = DecisionRecord {
            suggested: None,
            confidence: None,
            chosen: "today".to_string(),
            via: "t".to_string(),
            ..decision.clone()
        };
        assert!(
            serde_json::to_string(&no_grant)
                .unwrap()
                .contains(r#""suggested":null"#)
        );
        let overridden = DecisionRecord {
            chosen: "today".to_string(),
            via: "t".to_string(),
            ..decision.clone()
        };
        let undone = DecisionRecord {
            undone: true,
            ..decision.clone()
        };
        let log = [&decision, &overridden, &no_grant, &undone]
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let report = DecisionReport::from_log(&log);
        assert_eq!(
            report,
            DecisionReport {
                decisions: 2,
                taken: 1
            }
        );
        assert_eq!(
            report.summary().as_deref(),
            Some("suggestion taken 50% of the time, 2 decisions")
        );
        assert_eq!(DecisionReport::default().summary(), None);
    }
}
