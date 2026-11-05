//! The rules every bridge write obeys (spec §9.1–§9.2, §6.3–§6.4): which
//! paths a tool may touch, which facts are refused, and the exact lines the
//! desk appends on an agent's behalf.

use chrono::NaiveDate;
use thock_sync_core::{Heading, Operation, Placement, Write};

use crate::ClientIdentity;

pub const INBOX_PATH: &str = "memory/inbox.md";
pub const FACT_MAX_CHARS: usize = 300;
/// The `profile.md` heading whose lines are the person's do-not-keep list.
pub const FORBIDDEN_HEADING: &str = "What Thock should not keep";
const VIA_MARKER: &str = " · via ";
const SEPARATOR: &str = " · ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Empty,
    Absolute,
    Traversal,
    Hidden,
    EmptyComponent,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "the path is empty"),
            Self::Absolute => write!(f, "the path must be relative to the vault"),
            Self::Traversal => write!(f, "the path leaves the vault"),
            Self::Hidden => write!(
                f,
                "paths under a dot-folder are Thock's own and stay closed"
            ),
            Self::EmptyComponent => write!(f, "the path has an empty segment"),
        }
    }
}

/// Whether a vault-relative path may be read or written through the bridge
/// (§9.1): relative, `/`-separated, no `..`, no empty segment, no segment
/// that starts with `.`. The host still canonicalises the joined path and
/// refuses one that resolves outside the root.
pub fn validate_vault_path(path: &str) -> Result<(), PathError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path.starts_with('/') || path.starts_with('\\') || path.chars().nth(1) == Some(':') {
        return Err(PathError::Absolute);
    }
    for component in path.split(['/', '\\']) {
        if component.is_empty() {
            return Err(PathError::EmptyComponent);
        }
        if component == ".." {
            return Err(PathError::Traversal);
        }
        if component.starts_with('.') {
            return Err(PathError::Hidden);
        }
    }
    Ok(())
}

/// A harness name as the desk records it: lowercase ASCII letters, digits
/// and dashes, never empty, never long. Anything else came from a header
/// and is untrusted.
pub fn sanitize_harness(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for character in raw.trim().chars().take(60) {
        let lower = character.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "agent".to_string()
    } else {
        out.chars().take(40).collect()
    }
}

/// The words of four letters or more under `## What Thock should not keep`
/// in `profile.md`, lowercased and deduplicated (§9.2). Empty when the
/// section is missing.
pub fn forbidden_topics(profile: &str) -> Vec<String> {
    let mut topics: Vec<String> = Vec::new();
    let mut inside = false;
    for line in profile.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix('#') {
            let text = heading.trim_start_matches('#').trim();
            inside = text.eq_ignore_ascii_case(FORBIDDEN_HEADING)
                || text
                    .trim_start_matches(['*', '_'])
                    .trim_end_matches(['*', '_'])
                    .eq_ignore_ascii_case(FORBIDDEN_HEADING);
            continue;
        }
        if !inside {
            continue;
        }
        let body = trimmed
            .trim_start_matches(['-', '*', '+'])
            .trim_start_matches("[ ]")
            .trim_start_matches("[x]")
            .trim();
        for word in words(body) {
            if word.chars().count() >= 4 && !topics.contains(&word) {
                topics.push(word);
            }
        }
    }
    topics
}

/// Lowercased alphanumeric runs.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterRefusal {
    Empty,
    TooLong,
    Multiline,
    LeadingBullet,
    ForbiddenTopic(String),
    Credential(&'static str),
}

/// A refused fact, with the sentence the agent gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub FilterRefusal);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            FilterRefusal::Empty => write!(f, "the line is empty"),
            FilterRefusal::TooLong => {
                write!(
                    f,
                    "the line is over {FACT_MAX_CHARS} characters; keep it to one fact"
                )
            }
            FilterRefusal::Multiline => write!(f, "one line only; it holds a line break"),
            FilterRefusal::LeadingBullet => {
                write!(
                    f,
                    "write the fact without a leading `- `; the desk adds the bullet"
                )
            }
            FilterRefusal::ForbiddenTopic(_) => {
                write!(f, "that topic is on the person's do-not-keep list")
            }
            FilterRefusal::Credential(kind) => {
                write!(f, "that looks like {kind}; secrets are never kept")
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// Applies the shape, credential and forbidden-topic filters (§9.2) and
/// returns the trimmed line that may be written.
pub fn check_fact(text: &str, forbidden: &[String]) -> Result<String, Refusal> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(Refusal(FilterRefusal::Empty));
    }
    if trimmed.contains(['\n', '\r']) {
        return Err(Refusal(FilterRefusal::Multiline));
    }
    if trimmed.chars().count() > FACT_MAX_CHARS {
        return Err(Refusal(FilterRefusal::TooLong));
    }
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        return Err(Refusal(FilterRefusal::LeadingBullet));
    }
    if let Some(kind) = looks_like_credential(trimmed) {
        return Err(Refusal(FilterRefusal::Credential(kind)));
    }
    let fact_words = words(trimmed);
    if let Some(topic) = forbidden
        .iter()
        .find(|topic| fact_words.iter().any(|word| word == *topic))
    {
        return Err(Refusal(FilterRefusal::ForbiddenTopic(topic.clone())));
    }
    Ok(trimmed.to_string())
}

/// The credential shapes of §9.2, hand-matched so the library carries no
/// regex engine. `strict` adds the bare runs (32+ hex or base64, 13–19
/// digits, IBANs) that a one-line fact never needs but a harness's memory
/// file is full of (commit SHAs, content hashes; decision 32).
fn credential_shape(text: &str, strict: bool) -> Option<&'static str> {
    let lower = text.to_lowercase();
    for token in text
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | ')' | ',' | ';'))
    {
        if token.len() < 12 {
            continue;
        }
        if token.starts_with("sk-") || token.starts_with("ghp_") || token.starts_with("AKIA") {
            return Some("an API token");
        }
        if ["xoxa-", "xoxb-", "xoxp-"]
            .iter()
            .any(|prefix| token.starts_with(prefix))
        {
            return Some("an API token");
        }
        if !strict {
            continue;
        }
        if token.len() >= 32 && token.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some("a key");
        }
        if token.len() >= 32
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
            && token.chars().any(|c| c.is_ascii_digit())
            && token.chars().any(|c| c.is_ascii_uppercase())
            && token.chars().any(|c| c.is_ascii_lowercase())
        {
            return Some("a key");
        }
        if is_iban(token) {
            return Some("an account number");
        }
    }
    for keyword in [
        "password", "passwd", "secret", "token", "api_key", "api-key", "api key", "apikey",
    ] {
        let mut from = 0;
        while let Some(found) = lower[from..].find(keyword) {
            let after = &lower[from + found + keyword.len()..];
            if after.trim_start().starts_with([':', '=']) {
                return Some("a secret");
            }
            from += found + keyword.len();
        }
    }
    if strict && has_long_digit_run(text) {
        return Some("an account or card number");
    }
    None
}

fn looks_like_credential(text: &str) -> Option<&'static str> {
    credential_shape(text, true)
}

/// 13 to 19 digits, allowing the single spaces or dashes card numbers are
/// written with.
fn has_long_digit_run(text: &str) -> bool {
    let mut digits = 0usize;
    let mut previous_was_separator = false;
    for character in text.chars() {
        if character.is_ascii_digit() {
            digits += 1;
            previous_was_separator = false;
        } else if matches!(character, ' ' | '-') && digits > 0 && !previous_was_separator {
            previous_was_separator = true;
        } else {
            if (13..=19).contains(&digits) {
                return true;
            }
            digits = 0;
            previous_was_separator = false;
        }
    }
    (13..=19).contains(&digits)
}

fn is_iban(token: &str) -> bool {
    let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let bytes = token.as_bytes();
    (15..=34).contains(&bytes.len())
        && bytes[0].is_ascii_uppercase()
        && bytes[1].is_ascii_uppercase()
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
        && bytes[4..].iter().all(|b| b.is_ascii_alphanumeric())
        && bytes[4..].iter().any(|b| b.is_ascii_digit())
}

/// The first line of a whole file that the vault must not keep (M5a): a
/// credential shape or a do-not-keep topic. Mirrored harness memory goes
/// through this before it lands under `memory/agents/`.
pub fn find_unkeepable(text: &str, forbidden: &[String]) -> Option<String> {
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(kind) = credential_shape(trimmed, false) {
            return Some(format!("line {} looks like {kind}", index + 1));
        }
        let line_words = words(trimmed);
        if let Some(topic) = forbidden
            .iter()
            .find(|topic| line_words.iter().any(|word| word == *topic))
        {
            return Some(format!(
                "line {} touches a topic on the person's do-not-keep list ({topic})",
                index + 1
            ));
        }
    }
    None
}

/// The inbox line a `remember` lands as (§6.3):
/// `- 2026-11-04 · Prefers PR titles in the imperative · via claude-code@robin`.
pub fn inbox_line(date: NaiveDate, fact: &str, identity: &ClientIdentity) -> String {
    format!(
        "- {date}{SEPARATOR}{}{VIA_MARKER}{}",
        fact.trim(),
        identity.label()
    )
}

/// An inbox line taken apart, for Reflect's provenance and the tests that
/// pin what the skill tells an agent to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxLine {
    pub date: NaiveDate,
    pub fact: String,
    pub via: Option<ClientIdentity>,
}

pub fn parse_inbox_line(line: &str) -> Option<InboxLine> {
    let body = line.trim().strip_prefix("- ")?;
    let (date, rest) = body.split_once(SEPARATOR)?;
    let date = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    let (fact, via) = match rest.rsplit_once(VIA_MARKER) {
        Some((fact, via)) => {
            let (harness, host) = via.trim().split_once('@')?;
            (
                fact,
                Some(ClientIdentity {
                    harness: harness.to_string(),
                    host: host.to_string(),
                }),
            )
        }
        None => (rest, None),
    };
    Some(InboxLine {
        date,
        fact: fact.trim().to_string(),
        via,
    })
}

/// The daily-note line a `note_today` lands as (§6.4):
/// `- claude-code@robin · Shipped the PR.`
pub fn today_line(identity: &ClientIdentity, line: &str) -> String {
    format!("- {}{SEPARATOR}{}", identity.label(), line.trim())
}

pub fn parse_today_line(line: &str) -> Option<(ClientIdentity, String)> {
    let body = line.trim().strip_prefix("- ")?;
    let (who, text) = body.split_once(SEPARATOR)?;
    let (harness, host) = who.trim().split_once('@')?;
    Some((
        ClientIdentity {
            harness: harness.to_string(),
            host: host.to_string(),
        },
        text.trim().to_string(),
    ))
}

/// The sync-core write that appends one inbox line at the end of
/// `memory/inbox.md`, creating the file when it is missing.
pub fn inbox_write(client_id: &str, line: &str) -> Write {
    Write::new(
        client_id,
        INBOX_PATH,
        Operation::Append {
            heading: None,
            lines: vec![line.to_string()],
            placement: Placement::End,
            blank_line_before: false,
            create_from_template: false,
            after: None,
        },
    )
}

/// The sync-core write that appends lines under the agents section of a
/// daily note (§6.4): the section is created at the end when missing and
/// the note from the daily template, through the same `Append` the phone
/// uses, so the rule is one.
pub fn today_write(client_id: &str, note_path: &str, heading: &str, lines: &[String]) -> Write {
    Write::new(
        client_id,
        note_path,
        Operation::Append {
            heading: Some(Heading::new(heading.trim())),
            lines: lines.to_vec(),
            placement: Placement::End,
            blank_line_before: false,
            create_from_template: true,
            after: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use thock_sync_core::{Outcome, apply};

    fn identity() -> ClientIdentity {
        ClientIdentity::new("claude-code", "robin")
    }

    #[test]
    fn vault_paths_are_relative_and_never_hidden() {
        for ok in ["daily/2026-11-03.md", "notes/Café & ideas.md", "a/b/c.md"] {
            assert_eq!(validate_vault_path(ok), Ok(()), "{ok}");
        }
        let cases = [
            ("", PathError::Empty),
            ("   ", PathError::Empty),
            ("/etc/passwd", PathError::Absolute),
            ("C:\\x.md", PathError::Absolute),
            ("../x.md", PathError::Traversal),
            ("daily/../../x.md", PathError::Traversal),
            (".thock/config.toml", PathError::Hidden),
            (".claude/skills/x.md", PathError::Hidden),
            ("memory/.hidden/x.md", PathError::Hidden),
            ("daily//x.md", PathError::EmptyComponent),
            ("daily/", PathError::EmptyComponent),
        ];
        for (path, error) in cases {
            assert_eq!(validate_vault_path(path), Err(error), "{path:?}");
        }
    }

    #[test]
    fn forbidden_topics_come_from_the_profile_section() {
        let profile = "# Diego\n\n## What Thock may pull in\n- [x] Calendar\n\n\
                       ## What Thock should not keep\n- My salary and anything about pay\n\
                       - the divorce\n\n## Areas\n- Health\n";
        assert_eq!(
            forbidden_topics(profile),
            vec!["salary", "anything", "about", "divorce"]
        );
        assert!(forbidden_topics("# Nothing here\n").is_empty());
    }

    #[derive(Deserialize)]
    struct FilterCase {
        text: String,
        refused: bool,
        #[serde(default)]
        why: String,
    }

    #[test]
    fn filter_fixtures() {
        let raw = include_str!("fixtures/filters/cases.json");
        let cases: Vec<FilterCase> = serde_json::from_str(raw).unwrap();
        let forbidden = vec!["salary".to_string(), "divorce".to_string()];
        assert!(cases.len() > 10);
        for case in cases {
            let result = check_fact(&case.text, &forbidden);
            assert_eq!(
                result.is_err(),
                case.refused,
                "{:?} ({}) -> {result:?}",
                case.text,
                case.why
            );
        }
    }

    #[test]
    fn refusals_explain_themselves() {
        let forbidden = vec!["salary".to_string()];
        let long = "x".repeat(FACT_MAX_CHARS + 1);
        assert_eq!(
            check_fact(&long, &forbidden).unwrap_err().to_string(),
            format!("the line is over {FACT_MAX_CHARS} characters; keep it to one fact")
        );
        assert_eq!(
            check_fact("a\nb", &forbidden).unwrap_err().to_string(),
            "one line only; it holds a line break"
        );
        assert_eq!(
            check_fact("- a bullet", &forbidden)
                .unwrap_err()
                .to_string(),
            "write the fact without a leading `- `; the desk adds the bullet"
        );
        assert_eq!(
            check_fact("Their SALARY went up", &forbidden)
                .unwrap_err()
                .to_string(),
            "that topic is on the person's do-not-keep list"
        );
        assert_eq!(
            check_fact("  Likes short PRs  ", &forbidden).unwrap(),
            "Likes short PRs"
        );
    }

    #[test]
    fn the_inbox_line_round_trips() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let line = inbox_line(date, "Prefers PR titles in the imperative", &identity());
        assert_eq!(
            line,
            "- 2026-11-04 · Prefers PR titles in the imperative · via claude-code@robin"
        );
        assert_eq!(
            parse_inbox_line(&line).unwrap(),
            InboxLine {
                date,
                fact: "Prefers PR titles in the imperative".into(),
                via: Some(identity()),
            }
        );
        let plain = parse_inbox_line("- 2026-09-15 · Rui invoices monthly.").unwrap();
        assert_eq!(plain.via, None);
        assert_eq!(plain.fact, "Rui invoices monthly.");
        assert!(parse_inbox_line("Rui invoices monthly.").is_none());
    }

    #[test]
    fn the_today_line_round_trips() {
        let line = today_line(&identity(), "Shipped the PR (#184); CI green.");
        assert_eq!(
            line,
            "- claude-code@robin · Shipped the PR (#184); CI green."
        );
        assert_eq!(
            parse_today_line(&line).unwrap(),
            (identity(), "Shipped the PR (#184); CI green.".to_string())
        );
    }

    #[test]
    fn the_writes_apply_as_appends() {
        let write = inbox_write("bridge", "- 2026-11-04 · A fact · via codex@hp");
        let applied = apply(Some("# Noted\n\n_explainer_\n"), &write, None);
        assert_eq!(applied.outcome, Outcome::Applied);
        assert!(
            applied
                .text
                .ends_with("- 2026-11-04 · A fact · via codex@hp\n")
        );
        assert!(applied.text.starts_with("# Noted\n\n_explainer_\n"));
        let again = apply(Some(&applied.text), &write, None);
        assert_eq!(
            again.outcome,
            Outcome::Noop,
            "the same line is not written twice"
        );

        let write = today_write(
            "bridge",
            "daily/2026-11-04.md",
            "From your agents",
            vec![today_line(&identity(), "Shipped it.")].as_slice(),
        );
        let created = apply(None, &write, Some("# Wednesday\n\n## Notes\n"));
        assert_eq!(created.outcome, Outcome::Created);
        assert!(created.text.contains("## From your agents\n"));
        assert!(
            created
                .text
                .ends_with("- claude-code@robin · Shipped it.\n")
        );
        let added = apply(Some("# Wednesday\n\n## Notes\n- x\n"), &write, None);
        assert_eq!(added.outcome, Outcome::SectionAdded);
        let existing = "# Wednesday\n\n## From your agents\n\n- codex@hp · Earlier.\n";
        let appended = apply(Some(existing), &write, None);
        assert_eq!(appended.outcome, Outcome::Applied);
        assert!(
            appended
                .text
                .contains("- codex@hp · Earlier.\n- claude-code@robin · Shipped it.\n")
        );
    }

    #[test]
    fn whole_files_are_scanned_line_by_line() {
        let forbidden = vec!["salary".to_string()];
        assert_eq!(
            find_unkeepable("# Memory\n- cmake 3.31 on robin\n", &forbidden),
            None
        );
        assert_eq!(
            find_unkeepable(
                "# Memory\n\n- token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef12\n",
                &forbidden
            )
            .as_deref(),
            Some("line 3 looks like an API token")
        );
        assert_eq!(
            find_unkeepable("- Their salary is private\n", &forbidden).as_deref(),
            Some("line 1 touches a topic on the person's do-not-keep list (salary)")
        );
        // A memory file is full of hashes and ids; only prefixed tokens and
        // `password:`-style lines refuse a whole file.
        assert_eq!(
            find_unkeepable(
                "- fixed in 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b\n- order 1234567890123\n",
                &forbidden
            ),
            None
        );
        assert_eq!(
            find_unkeepable("password: hunter2\n", &forbidden).as_deref(),
            Some("line 1 looks like a secret")
        );
    }

    #[test]
    fn harness_names_are_sanitized() {
        assert_eq!(sanitize_harness("Claude Code"), "claude-code");
        assert_eq!(sanitize_harness("  codex_cli  "), "codex-cli");
        assert_eq!(sanitize_harness("!!!"), "agent");
        assert_eq!(sanitize_harness(""), "agent");
        assert!(sanitize_harness(&"a".repeat(100)).len() <= 40);
    }
}
