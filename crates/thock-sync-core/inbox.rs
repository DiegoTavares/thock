//! The lines an inbox decision writes (V40 §5, V41 §6), shared so a desk
//! keystroke and a phone swipe leave byte-identical traces: the task line, the
//! triage-log line, the archive name, and the body rule that decides whether
//! the task carries a link. `ThockKit/Vault/PhoneWrites.swift` mirrors these.

/// Where a decided inbox note goes, beside the triage log.
pub const INBOX_ARCHIVE_DIR: &str = "archives/inbox";

/// The append-only record of every decision (V13 §9.5).
pub const TRIAGE_LOG_PATH: &str = "archives/inbox/triage-log.md";

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
    }
}
