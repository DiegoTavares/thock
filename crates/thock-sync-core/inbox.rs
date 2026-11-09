//! The lines an inbox decision writes (V40 §5, V41 §6), shared so a desk
//! keystroke and a phone swipe leave byte-identical traces: the task line, the
//! triage-log line, the archive name, and the body rule that decides whether
//! the task carries a link. `ThockKit/Vault/PhoneWrites.swift` mirrors these.

/// Where a decided inbox note goes, beside the triage log.
pub const INBOX_ARCHIVE_DIR: &str = "archives/inbox";

/// The append-only record of every decision (V13 §9.5).
pub const TRIAGE_LOG_PATH: &str = "archives/inbox/triage-log.md";

/// The destination word the desk's morning run writes (V56 §7.1): an
/// Archive decision nobody keyed.
pub const ARCHIVED_AUTOMATICALLY: &str = "Archived · auto";

/// The archive path for an inbox note: the same file name under
/// [`INBOX_ARCHIVE_DIR`].
pub fn archive_path(inbox_path: &str) -> String {
    let name = inbox_path.rsplit('/').next().unwrap_or(inbox_path);
    format!("{INBOX_ARCHIVE_DIR}/{name}")
}

/// Whether a note says more than its title: any non-blank line after the
/// frontmatter and the first line of content (V40 §5). An image line counts.
pub fn inbox_has_body(note: &str) -> bool {
    let mut lines = note.lines().map(|line| line.trim_end());
    let mut content = lines.by_ref().peekable();
    if content.peek() == Some(&"---") {
        content.next();
        for line in content.by_ref() {
            if line == "---" || line == "..." {
                break;
            }
        }
    }
    content
        .filter(|line| !line.trim().is_empty())
        .nth(1)
        .is_some()
}

/// The task line a decision appends: the title, plus an inert link to the
/// archived note when the note has a body worth coming back to. `title` must
/// already be sanitized.
pub fn inbox_task_line(title: &str, stem: &str, has_body: bool) -> String {
    if has_body {
        format!("- [ ] {title} [[{stem}]]")
    } else {
        format!("- [ ] {title}")
    }
}

/// The triage log's line (V13 §9.5): `- <day> · <title> → <destination>`,
/// with the capture marker when the note has a digest so the rebuild scan
/// sees the decision.
pub fn triage_log_line(day: &str, title: &str, destination: &str, digest: Option<&str>) -> String {
    match digest {
        Some(digest) => format!("- {day} · {title} → {destination} <!--inbox:{digest}-->"),
        None => format!("- {day} · {title} → {destination}"),
    }
}

/// What a triage-log line decided, read from its destination text (V53 §4.2).
/// The names are the ones `.thock/gmail.toml`'s `[archive] on` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DecisionClass {
    Today,
    Soon,
    Someday,
    Archive,
    Discard,
    /// Kept as a note, appended to a note, or anything hand-written.
    Other,
}

impl DecisionClass {
    pub const ALL: [DecisionClass; 6] = [
        Self::Today,
        Self::Soon,
        Self::Someday,
        Self::Archive,
        Self::Discard,
        Self::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Soon => "soon",
            Self::Someday => "someday",
            Self::Archive => "archive",
            Self::Discard => "discard",
            Self::Other => "other",
        }
    }

    /// The class `name` spells, case-insensitively.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::ALL
            .into_iter()
            .find(|class| class.name().eq_ignore_ascii_case(name))
    }
}

/// The class of a log line's destination. The Backlog headings are the
/// vault's configured ones, because the desk and the phone write those
/// (`Backlog · <soon heading>`), not the defaults.
pub fn decision_class(
    destination: &str,
    soon_heading: &str,
    someday_heading: &str,
) -> DecisionClass {
    let destination = destination.trim();
    if destination.starts_with("Today") {
        return DecisionClass::Today;
    }
    if let Some(heading) = destination.strip_prefix("Backlog · ") {
        let heading = heading.trim();
        if heading == soon_heading.trim() {
            return DecisionClass::Soon;
        }
        if heading == someday_heading.trim() {
            return DecisionClass::Someday;
        }
    }
    match destination {
        "Archived" | ARCHIVED_AUTOMATICALLY => DecisionClass::Archive,
        "Discarded" => DecisionClass::Discard,
        _ => DecisionClass::Other,
    }
}

/// Whether a log line's destination was written by the morning run rather
/// than a person (V56 §7.1). Gmail archive keeps these undoable for longer.
pub fn is_automatic(destination: &str) -> bool {
    destination.trim() == ARCHIVED_AUTOMATICALLY
}

/// One triage-log line that carries a capture marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriageLogLine {
    pub day: String,
    pub title: String,
    pub destination: String,
    pub digest: String,
}

/// Reads a line [`triage_log_line`] wrote. A line without a marker, or that
/// isn't in the log's shape, is `None`: it can't name a captured item.
pub fn parse_triage_log_line(line: &str) -> Option<TriageLogLine> {
    let rest = line.trim_end().strip_prefix("- ")?;
    let (rest, marker) = rest.rsplit_once(" <!--inbox:")?;
    let digest = marker.strip_suffix("-->")?.trim();
    if digest.is_empty() || digest.len() > 64 || digest.contains(char::is_whitespace) {
        return None;
    }
    let (day, rest) = rest.split_once(" · ")?;
    // The title is sanitized but may still hold an arrow; the destination
    // never does, so the last one splits them.
    let (title, destination) = rest.rsplit_once(" → ")?;
    if day.trim().is_empty() || destination.trim().is_empty() {
        return None;
    }
    Some(TriageLogLine {
        day: day.to_string(),
        title: title.to_string(),
        destination: destination.to_string(),
        digest: digest.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "---\nsource:   thock-ios\ncapture:  4d1f9a02c7b3\ncaptured: 2026-10-10T09:31:00Z\ntitle:    Call Ana\n---\n\n# Call Ana\n\nAbout the article on rest.\n";
    const TITLE_ONLY: &str = "---\nsource:   thock-ios\ncapture:  9a9a9a9a9a9a\ncaptured: 2026-10-10T09:31:00Z\ntitle:    Buy milk\n---\n\n# Buy milk\n";

    #[test]
    fn archive_keeps_the_file_name() {
        assert_eq!(
            archive_path("inbox/2026-10-10-0931-call-ana.md"),
            "archives/inbox/2026-10-10-0931-call-ana.md"
        );
        assert_eq!(archive_path("drop/zone/x.md"), "archives/inbox/x.md");
        assert_eq!(archive_path("x.md"), "archives/inbox/x.md");
    }

    #[test]
    fn body_rule_matches_the_phone() {
        assert!(inbox_has_body(NOTE));
        assert!(!inbox_has_body(TITLE_ONLY));
        assert!(!inbox_has_body("# Heading only\n\n\n"));
        assert!(!inbox_has_body(""));
        assert!(inbox_has_body("# Heading\n![alt](/images/a.png)\n"));
        assert!(inbox_has_body("First line\nSecond line\n"));
        // The frontmatter never counts as a body.
        assert!(!inbox_has_body("---\ntitle: x\nsource: y\n---\n# x\n"));
        // An unterminated frontmatter has nothing after it.
        assert!(!inbox_has_body("---\ntitle: x\n"));
    }

    #[test]
    fn task_and_log_lines_are_the_phones_bytes() {
        assert_eq!(
            inbox_task_line("Call Ana", "2026-10-10-0931-call-ana", true),
            "- [ ] Call Ana [[2026-10-10-0931-call-ana]]"
        );
        assert_eq!(
            inbox_task_line("Buy milk", "2026-10-10-0931-buy-milk", false),
            "- [ ] Buy milk"
        );
        assert_eq!(
            triage_log_line(
                "2026-10-10",
                "Call Ana",
                "Today · Day planner",
                Some("4d1f9a02c7b3")
            ),
            "- 2026-10-10 · Call Ana → Today · Day planner <!--inbox:4d1f9a02c7b3-->"
        );
        assert_eq!(
            triage_log_line(
                "2026-10-10",
                "Call Ana",
                "Backlog · Soon",
                Some("4d1f9a02c7b3")
            ),
            "- 2026-10-10 · Call Ana → Backlog · Soon <!--inbox:4d1f9a02c7b3-->"
        );
        assert_eq!(
            triage_log_line("2026-10-10", "Call Ana", "Archived", None),
            "- 2026-10-10 · Call Ana → Archived"
        );
        assert_eq!(
            triage_log_line(
                "2026-11-08",
                "HD 52112: Maya licence expired",
                ARCHIVED_AUTOMATICALLY,
                Some("d4d4d4d4d4d4")
            ),
            "- 2026-11-08 · HD 52112: Maya licence expired → Archived · auto <!--inbox:d4d4d4d4d4d4-->"
        );
    }

    #[test]
    fn log_lines_parse_back_into_their_parts() {
        let line = triage_log_line(
            "2026-11-02",
            "Re: Q4 planning doc",
            "Backlog · Soon",
            Some("4d1f9a02c7b3"),
        );
        assert_eq!(
            parse_triage_log_line(&line),
            Some(TriageLogLine {
                day: "2026-11-02".to_string(),
                title: "Re: Q4 planning doc".to_string(),
                destination: "Backlog · Soon".to_string(),
                digest: "4d1f9a02c7b3".to_string(),
            })
        );
        // A title with its own arrow and dot still splits at the last arrow.
        let line = triage_log_line("2026-11-02", "A → B · C", "Archived", Some("abc"));
        let parsed = parse_triage_log_line(&line).unwrap();
        assert_eq!(parsed.title, "A → B · C");
        assert_eq!(parsed.destination, "Archived");

        let marker_less = triage_log_line("2026-11-02", "Buy milk", "Archived", None);
        assert_eq!(parse_triage_log_line(&marker_less), None);
        for malformed in [
            "",
            "# Triage log",
            "- 2026-11-02 · no arrow <!--inbox:abc-->",
            "- 2026-11-02 · x → Archived <!--inbox:-->",
            "- 2026-11-02 · x → Archived <!--inbox:abc",
            "* 2026-11-02 · x → Archived <!--inbox:abc-->",
            "- 2026-11-02 · x →  <!--inbox:abc-->",
        ] {
            assert_eq!(parse_triage_log_line(malformed), None, "{malformed:?}");
        }
    }

    #[test]
    fn destinations_classify_with_the_vaults_headings() {
        let class = |destination| decision_class(destination, "Soon", "Someday");
        assert_eq!(class("Today · Day planner"), DecisionClass::Today);
        assert_eq!(class("Backlog · Soon"), DecisionClass::Soon);
        assert_eq!(class("Backlog · Someday"), DecisionClass::Someday);
        assert_eq!(class("Archived"), DecisionClass::Archive);
        assert_eq!(class("Archived · auto"), DecisionClass::Archive);
        assert!(is_automatic("Archived · auto"));
        assert!(!is_automatic("Archived"));
        assert!(!is_automatic("Backlog · Soon"));
        assert_eq!(class("Discarded"), DecisionClass::Discard);
        assert_eq!(class("notes/ideas/ (kept as note)"), DecisionClass::Other);
        assert_eq!(class("Backlog · Later"), DecisionClass::Other);
        assert_eq!(
            decision_class("Backlog · Next up", "Next up", "Maybe"),
            DecisionClass::Soon
        );
        assert_eq!(
            decision_class("Backlog · Soon", "Next up", "Maybe"),
            DecisionClass::Other
        );
        for class in DecisionClass::ALL {
            assert_eq!(DecisionClass::from_name(class.name()), Some(class));
        }
        assert_eq!(
            DecisionClass::from_name(" Archive "),
            Some(DecisionClass::Archive)
        );
        assert_eq!(DecisionClass::from_name("trash"), None);
    }
}
