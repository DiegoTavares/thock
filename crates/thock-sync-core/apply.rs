//! The application rules of spec §8: deterministic, total, idempotent.

use crate::markdown::{Document, Section, hash_lines, line_hash, normalize_line};
use crate::write::{Heading, Operation, Place, Placement, Write};

/// The marker a kept-both line ends with (spec §8.5).
pub const CONFLICT_MARKER: &str = "<!--thock:also-->";

/// What applying a write did to the note (spec §8). When several things
/// happened, the most structural wins: `Created` over `SectionAdded` over
/// `KeptBoth` over `Applied`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Created,
    SectionAdded,
    Applied,
    KeptBoth,
    Noop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub text: String,
    pub outcome: Outcome,
}

/// Applies `write` to `existing` (`None` when the file does not exist).
/// `seed` is the already-expanded template used when an `append` asks for
/// `create_from_template` on a missing file (spec §8.2 rule 2).
pub fn apply(existing: Option<&str>, write: &Write, seed: Option<&str>) -> Applied {
    if let Some(text) = existing
        && effect_present(text, write)
    {
        return Applied {
            text: text.to_string(),
            outcome: Outcome::Noop,
        };
    }

    let mut outcome = None;
    let mut document = match existing {
        Some(text) => Document::split(text),
        None => {
            outcome = Some(Outcome::Created);
            match &write.operation {
                Operation::Create { content } => {
                    return Applied {
                        text: content.clone(),
                        outcome: Outcome::Created,
                    };
                }
                Operation::Append {
                    create_from_template: true,
                    ..
                } if seed.is_some() => Document::split(seed.unwrap_or_default()),
                operation => match operation.heading() {
                    Some(heading) => Document::split(&format!("{}\n\n", heading.line())),
                    None => Document::empty(),
                },
            }
        }
    };

    let applied = match &write.operation {
        Operation::Create { content } => {
            let lines = content_lines(content);
            let section = document.whole();
            insert_lines(&mut document, &section, section.body_end, &lines, true);
            Outcome::Applied
        }
        Operation::Append {
            heading,
            lines,
            placement,
            blank_line_before,
            ..
        } => {
            let section = locate(&mut document, heading.as_ref(), &mut outcome);
            let at = match placement {
                Placement::End => section.body_end,
                Placement::BeforeChildren => section.own_end,
            };
            insert_lines(&mut document, &section, at, lines, *blank_line_before);
            Outcome::Applied
        }
        Operation::ReplaceLine {
            heading,
            line_hash: wanted,
            ordinal,
            new_line,
        } => {
            let section = locate(&mut document, heading.as_ref(), &mut outcome);
            match find_line(&document, &section, wanted, *ordinal) {
                Some(index) => {
                    if let Some(line) = document.lines.get_mut(index) {
                        line.text = new_line.clone();
                    }
                    Outcome::Applied
                }
                None => {
                    let marked = vec![mark(new_line)];
                    insert_lines(&mut document, &section, section.body_end, &marked, false);
                    Outcome::KeptBoth
                }
            }
        }
        Operation::RemoveLine {
            heading,
            line_hash: wanted,
            ordinal,
        } => {
            let section = locate(&mut document, heading.as_ref(), &mut outcome);
            match find_line(&document, &section, wanted, *ordinal) {
                Some(index) => {
                    if index < document.lines.len() {
                        document.lines.remove(index);
                    }
                    Outcome::Applied
                }
                None => Outcome::Noop,
            }
        }
        Operation::ReplaceSection {
            heading,
            base_hash,
            lines,
        } => {
            let section = locate(&mut document, Some(heading), &mut outcome);
            let current = hash_lines(&document.body_texts(&section));
            if current == *base_hash {
                document.lines.drain(section.start..section.body_end);
                document.insert(section.start, lines);
                Outcome::Applied
            } else if lines.iter().all(|line| line.trim().is_empty()) {
                Outcome::Noop
            } else {
                let mut marked = lines.clone();
                if let Some(first) = marked.iter_mut().find(|line| !line.trim().is_empty()) {
                    *first = mark(first);
                }
                insert_lines(&mut document, &section, section.body_end, &marked, true);
                Outcome::KeptBoth
            }
        }
        Operation::MoveBlock {
            heading,
            line_hash: wanted,
            ordinal,
            to,
            place,
            new_line,
            create_under,
        } => {
            // A missing source is a task the desk renamed, moved or completed
            // meanwhile; a move must never become a copy, so nothing happens.
            let Some(source) = find_section(&document, heading.as_ref()).map(own) else {
                return unchanged(document, outcome);
            };
            let Some(index) = find_line(&document, &source, wanted, *ordinal) else {
                return unchanged(document, outcome);
            };
            let end = block_end(&document, &source, index);
            let mut texts: Vec<String> = document
                .lines
                .get(index..end)
                .unwrap_or(&[])
                .iter()
                .map(|line| line.text.clone())
                .collect();
            if let (Some(new_line), Some(first)) = (new_line, texts.first_mut()) {
                *first = new_line.clone();
            }
            cut(&mut document, index, end);
            let destination = own(locate_group(
                &mut document,
                to.as_ref(),
                create_under.as_ref(),
                &mut outcome,
            ));
            let at = match place {
                Place::Top => first_body_line(&document, &destination),
                Place::End => destination.body_end,
                Place::After {
                    line_hash: anchor,
                    ordinal: anchor_ordinal,
                } => match find_line(&document, &destination, anchor, *anchor_ordinal) {
                    Some(anchor) => block_end(&document, &destination, anchor),
                    None => destination.body_end,
                },
            };
            insert_lines(&mut document, &destination, at, &texts, false);
            Outcome::Applied
        }
        Operation::RemoveBlock {
            heading,
            line_hash: wanted,
            ordinal,
        } => {
            let Some(section) = find_section(&document, heading.as_ref()).map(own) else {
                return unchanged(document, outcome);
            };
            match find_line(&document, &section, wanted, *ordinal) {
                Some(index) => {
                    let end = block_end(&document, &section, index);
                    cut(&mut document, index, end);
                    Outcome::Applied
                }
                None => return unchanged(document, outcome),
            }
        }
    };

    let outcome = match (outcome, applied) {
        (Some(structural), _) => structural,
        (None, applied) => applied,
    };
    Applied {
        text: document.join(),
        outcome,
    }
}

/// Whether `content` already shows the effect of `write` (spec §8.3). A
/// write whose effect is present applies as a no-op, which is what makes
/// re-applying after a crash, or re-applying on top of a snapshot that
/// already carries it, safe.
pub fn effect_present(content: &str, write: &Write) -> bool {
    let document = Document::split(content);
    match &write.operation {
        Operation::Create { content: wanted } => {
            if normalize_endings(content) == normalize_endings(wanted) {
                return true;
            }
            // Rule 3 turned the create into an append; a second application
            // must see that append (spec §8.2 rule 1).
            let section = document.whole();
            contains_run(&document.matchable(&section), &content_lines(wanted))
        }
        Operation::Append { heading, lines, .. } => {
            let Some(section) = find_section(&document, heading.as_ref()) else {
                return false;
            };
            contains_run(&document.matchable(&section), lines)
        }
        Operation::ReplaceLine {
            heading,
            line_hash: wanted,
            ordinal,
            new_line,
        } => {
            let Some(section) = find_section(&document, heading.as_ref()) else {
                return false;
            };
            let marked = mark(new_line);
            let equals =
                |line: &str| line.trim_end() == new_line.trim_end() || line.trim_end() == marked;
            // The target itself decides when it exists: two identical lines
            // must each be tickable. Only when it is gone does any line that
            // reads like `new_line` count.
            match find_line(&document, &section, wanted, *ordinal) {
                Some(index) => document
                    .lines
                    .get(index)
                    .is_some_and(|line| equals(&line.text)),
                None => {
                    let new_hash = line_hash(new_line);
                    document
                        .matchable(&section)
                        .iter()
                        .any(|(_, line)| equals(line) || line_hash(line) == new_hash)
                }
            }
        }
        Operation::RemoveLine {
            heading,
            line_hash: wanted,
            ..
        } => {
            let Some(section) = find_section(&document, heading.as_ref()) else {
                return true;
            };
            !document
                .matchable(&section)
                .iter()
                .any(|(_, line)| line_hash(line) == *wanted)
        }
        Operation::ReplaceSection { heading, lines, .. } => {
            let Some(section) = find_section(&document, Some(heading)) else {
                return false;
            };
            let body = document.body_texts(&section);
            let wanted: Vec<&str> = lines.iter().map(String::as_str).collect();
            if hash_lines(&body) == hash_lines(&wanted) {
                return true;
            }
            lines
                .iter()
                .find(|line| !line.trim().is_empty())
                .is_some_and(|first| body.iter().any(|line| line.trim_end() == mark(first)))
        }
        Operation::MoveBlock {
            heading,
            line_hash: wanted,
            to,
            place,
            new_line,
            create_under,
        } => {
            let Some(destination) =
                find_group(&document, to.as_ref(), create_under.as_ref()).map(own)
            else {
                return false;
            };
            let landed = new_line
                .as_deref()
                .map(line_hash)
                .unwrap_or_else(|| wanted.clone());
            let placed = document
                .matchable(&destination)
                .into_iter()
                .filter(|(_, text)| line_hash(text) == landed && !normalize_line(text).is_empty())
                .any(|(index, _)| match place {
                    Place::Top => index == first_body_line(&document, &destination),
                    Place::End => block_end(&document, &destination, index) == destination.body_end,
                    // A missing anchor put the block at the end (§7.2), so
                    // that is where a retry looks for it.
                    Place::After {
                        line_hash: anchor,
                        ordinal,
                    } => match find_line(&document, &destination, anchor, *ordinal) {
                        Some(anchor) => block_end(&document, &destination, anchor) == index,
                        None => block_end(&document, &destination, index) == destination.body_end,
                    },
                });
            if !placed {
                return false;
            }
            // Moved between groups, the source must have let go of it too;
            // reordered inside one group, the placed line is the source.
            match find_section(&document, heading.as_ref()).map(own) {
                Some(source) if source.heading != destination.heading => !document
                    .matchable(&source)
                    .iter()
                    .any(|(_, text)| {
                        line_hash(text) == *wanted && !normalize_line(text).is_empty()
                    }),
                _ => true,
            }
        }
        Operation::RemoveBlock {
            heading,
            line_hash: wanted,
            ..
        } => {
            let Some(section) = find_section(&document, heading.as_ref()).map(own) else {
                return true;
            };
            !document
                .matchable(&section)
                .iter()
                .any(|(_, text)| line_hash(text) == *wanted && !normalize_line(text).is_empty())
        }
    }
}

/// The write found nothing to do: a `noop` unless the file or a section
/// was created on the way, which the outcome still reports.
fn unchanged(document: Document, outcome: Option<Outcome>) -> Applied {
    Applied {
        text: document.join(),
        outcome: outcome.unwrap_or(Outcome::Noop),
    }
}

/// The section's own lines as a section of their own, so a block rule
/// never reaches into a subsection: a group is the lines under its heading
/// above the next heading (V38 §7.1).
fn own(section: Section) -> Section {
    Section {
        body_end: section.own_end,
        ..section
    }
}

/// The index of the first non-blank line of a section, or its end when it
/// has none: where `place: top` lands.
fn first_body_line(document: &Document, section: &Section) -> usize {
    (section.start..section.body_end)
        .find(|index| document.lines.get(*index).is_some_and(|line| !line.is_blank()))
        .unwrap_or(section.body_end)
}

/// One past the last line of the block starting at `index`: the line plus
/// every indented, non-blank line after it inside the section, with blank
/// lines between them included and trailing blank lines left out. The
/// desk's own span rule, so both ends cut the same block.
fn block_end(document: &Document, section: &Section, index: usize) -> usize {
    let mut end = index + 1;
    let mut last_content = end;
    while end < section.body_end {
        let Some(line) = document.lines.get(end) else {
            break;
        };
        if line.is_blank() {
            end += 1;
        } else if line.text.starts_with(' ') || line.text.starts_with('\t') {
            end += 1;
            last_content = end;
        } else {
            break;
        }
    }
    last_content
}

/// Removes `start..end`. A blank line on each side of the gap would leave
/// two in a row, which neither end ever writes, so one goes with the block.
fn cut(document: &mut Document, start: usize, end: usize) {
    let end = end.min(document.lines.len());
    if start >= end {
        return;
    }
    document.lines.drain(start..end);
    let before_blank = start > 0
        && document
            .lines
            .get(start - 1)
            .is_some_and(|line| line.is_blank());
    let after_blank = document.lines.get(start).is_some_and(|line| line.is_blank());
    if before_blank && after_blank {
        document.lines.remove(start);
    }
}

/// The group a block moves into. A missing `to` is created at the end of
/// `create_under` (itself created like any missing heading), or, with no
/// parent named, where an append would create it.
fn locate_group(
    document: &mut Document,
    to: Option<&Heading>,
    create_under: Option<&Heading>,
    outcome: &mut Option<Outcome>,
) -> Section {
    let Some(to) = to else {
        return document.whole();
    };
    if create_under.is_none()
        && let Some(found) = document.resolve(to)
    {
        return document.section(&found);
    }
    let Some(parent) = create_under else {
        return locate(document, Some(to), outcome);
    };
    // `Someday › Home` is the Home inside Someday, whatever other Home the
    // note has, so the category is looked for inside its section only.
    let parent = locate(document, Some(parent), outcome);
    if let Some(found) = document.resolve_within(to, parent.start..parent.end) {
        return document.section(&found);
    }
    insert_lines(document, &parent, parent.body_end, &[to.line()], true);
    if outcome.is_none() {
        *outcome = Some(Outcome::SectionAdded);
    }
    match document.resolve_within(to, parent.start..document.lines.len()) {
        Some(found) => document.section(&found),
        None => document.whole(),
    }
}

/// The group a `move_block` names as its destination, when the note has
/// it: `to` inside `create_under`'s section when a parent is named, else
/// `to` anywhere.
fn find_group(
    document: &Document,
    to: Option<&Heading>,
    create_under: Option<&Heading>,
) -> Option<Section> {
    let Some(to) = to else {
        return Some(document.whole());
    };
    let found = match create_under {
        Some(parent) => {
            let parent = find_section(document, Some(parent))?;
            document.resolve_within(to, parent.start..parent.end)?
        }
        None => document.resolve(to)?,
    };
    Some(document.section(&found))
}

fn mark(line: &str) -> String {
    format!("{} {CONFLICT_MARKER}", line.trim_end())
}

fn normalize_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn content_lines(content: &str) -> Vec<String> {
    let mut lines: Vec<String> = normalize_endings(content)
        .split('\n')
        .map(str::to_string)
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Whether `lines` appear in order as adjacent lines of `body`, which holds
/// `(index, text)` pairs; a gap in the indices (a fenced block between) breaks
/// the run.
fn contains_run(body: &[(usize, &str)], lines: &[String]) -> bool {
    if lines.is_empty() {
        return true;
    }
    if body.len() < lines.len() {
        return false;
    }
    body.windows(lines.len()).any(|window| {
        let adjacent = window
            .windows(2)
            .all(|pair| pair.get(1).map(|next| next.0) == pair.first().map(|first| first.0 + 1));
        adjacent
            && window
                .iter()
                .zip(lines)
                .all(|((_, have), want)| have.trim_end() == want.trim_end())
    })
}

fn find_section(document: &Document, heading: Option<&Heading>) -> Option<Section> {
    match heading {
        None => Some(document.whole()),
        Some(heading) => document
            .resolve(heading)
            .map(|found| document.section(&found)),
    }
}

/// The section a write targets, creating the heading when the note lacks it
/// (spec §8.2 rule 4): one blank line, the heading, before the first
/// level-1 heading that is not the file's first heading, else at the end.
fn locate(
    document: &mut Document,
    heading: Option<&Heading>,
    outcome: &mut Option<Outcome>,
) -> Section {
    let Some(heading) = heading else {
        return document.whole();
    };
    if let Some(found) = document.resolve(heading) {
        return document.section(&found);
    }
    let headings = document.headings();
    let insert_at = headings
        .iter()
        .skip(1)
        .find(|candidate| candidate.level == 1)
        .map(|candidate| candidate.index);
    let mut texts = Vec::new();
    let at = match insert_at {
        Some(index) => {
            if index > 0
                && document
                    .lines
                    .get(index - 1)
                    .is_some_and(|line| !line.is_blank())
            {
                texts.push(String::new());
            }
            texts.push(heading.line());
            texts.push(String::new());
            index
        }
        None => {
            let end = document.lines.len();
            if document.lines.last().is_some_and(|line| !line.is_blank()) {
                texts.push(String::new());
            }
            texts.push(heading.line());
            end
        }
    };
    document.insert(at, &texts);
    if outcome.is_none() {
        *outcome = Some(Outcome::SectionAdded);
    }
    match document.resolve(heading) {
        Some(found) => document.section(&found),
        None => document.whole(),
    }
}

/// Spec §8.2 rule 5: `lines` go in at `at`; with `blank_line_before`, a
/// blank line first unless the previous line is blank or the heading itself.
fn insert_lines(
    document: &mut Document,
    section: &Section,
    at: usize,
    lines: &[String],
    blank_line_before: bool,
) {
    if lines.is_empty() {
        return;
    }
    let previous = at.checked_sub(1);
    let needs_blank = blank_line_before
        && previous.is_some_and(|index| {
            section.heading != Some(index)
                && document
                    .lines
                    .get(index)
                    .is_some_and(|line| !line.is_blank())
        });
    if needs_blank {
        let mut with_blank = Vec::with_capacity(lines.len() + 1);
        with_blank.push(String::new());
        with_blank.extend(lines.iter().cloned());
        document.insert(at, &with_blank);
    } else {
        document.insert(at, lines);
    }
}

fn find_line(
    document: &Document,
    section: &Section,
    wanted: &str,
    ordinal: usize,
) -> Option<usize> {
    let matches: Vec<usize> = document
        .matchable(section)
        .into_iter()
        // A line with nothing left to name it by (blank, or an empty
        // checkbox) is never a target: every such line shares one hash.
        .filter(|(_, text)| line_hash(text) == wanted && !normalize_line(text).is_empty())
        .map(|(index, _)| index)
        .collect();
    matches.get(ordinal).or_else(|| matches.first()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(operation: Operation) -> Write {
        Write::new("client", "daily/2026-10-02.md", operation)
    }

    fn heading(text: &str) -> Option<Heading> {
        Some(Heading::new(text))
    }

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn append(heading_text: Option<&str>, items: &[&str]) -> Write {
        write(Operation::Append {
            heading: heading_text.map(Heading::new),
            lines: lines(items),
            placement: Placement::End,
            blank_line_before: false,
            create_from_template: false,
        })
    }

    const NOTE: &str = "# Thursday\n\n## Journal\n\nSlept badly.\n\n___\n\n## Day planner\n\n- [x] 08:00 - 08:30 Morning walk\n- [ ] 09:30 - 11:00 Deep work\n\n### Calendar\n\n- [ ] 12:30 Lunch <!--gcal:abc-->\n\n## Personal\n\n- Groceries\n\n# Daily Closure\n\nThe agent's take.\n";

    #[test]
    fn append_end_lands_below_subsections_and_before_children_above() {
        let end = apply(
            Some(NOTE),
            &append(Some("Day planner"), &["- [ ] Buy a card"]),
            None,
        );
        assert_eq!(end.outcome, Outcome::Applied);
        assert!(
            end.text
                .contains("- [ ] 12:30 Lunch <!--gcal:abc-->\n- [ ] Buy a card\n\n## Personal")
        );

        let own = apply(
            Some(NOTE),
            &write(Operation::Append {
                heading: heading("Day planner"),
                lines: lines(&["- [ ] Buy a card"]),
                placement: Placement::BeforeChildren,
                blank_line_before: false,
                create_from_template: false,
            }),
            None,
        );
        assert!(
            own.text
                .contains("- [ ] 09:30 - 11:00 Deep work\n- [ ] Buy a card\n\n### Calendar")
        );
    }

    #[test]
    fn append_with_blank_line_before_prose() {
        let result = apply(
            Some(NOTE),
            &write(Operation::Append {
                heading: heading("Journal"),
                lines: lines(&["**21:14** · Late thought."]),
                placement: Placement::End,
                blank_line_before: true,
                create_from_template: false,
            }),
            None,
        );
        assert!(
            result
                .text
                .contains("Slept badly.\n\n**21:14** · Late thought.\n\n___")
        );
        let empty_section = "## Journal\n\n## Next\n";
        let result = apply(
            Some(empty_section),
            &write(Operation::Append {
                heading: heading("Journal"),
                lines: lines(&["First."]),
                placement: Placement::End,
                blank_line_before: true,
                create_from_template: false,
            }),
            None,
        );
        assert_eq!(result.text, "## Journal\nFirst.\n\n## Next\n");
    }

    #[test]
    fn missing_heading_goes_before_the_agent_heading() {
        let result = apply(
            Some(NOTE),
            &append(Some("Asked on the go"), &["An answer."]),
            None,
        );
        assert_eq!(result.outcome, Outcome::SectionAdded);
        assert!(
            result
                .text
                .contains("- Groceries\n\n## Asked on the go\nAn answer.\n\n# Daily Closure")
        );
        let flat = "# Title\n\n## Journal\nfoo\n";
        let result = apply(Some(flat), &append(Some("Personal"), &["- x"]), None);
        assert_eq!(
            result.text,
            "# Title\n\n## Journal\nfoo\n\n## Personal\n- x\n"
        );
    }

    #[test]
    fn missing_file_variants() {
        let created = apply(
            None,
            &write(Operation::Create {
                content: "hi\n".into(),
            }),
            None,
        );
        assert_eq!(created.outcome, Outcome::Created);
        assert_eq!(created.text, "hi\n");

        let seeded = apply(
            None,
            &write(Operation::Append {
                heading: heading("Journal"),
                lines: lines(&["x"]),
                placement: Placement::End,
                blank_line_before: false,
                create_from_template: true,
            }),
            Some("# Day\n\n## Journal\n\n## Day planner\n"),
        );
        assert_eq!(seeded.outcome, Outcome::Created);
        assert_eq!(seeded.text, "# Day\n\n## Journal\nx\n\n## Day planner\n");

        let bare = apply(None, &append(Some("Journal"), &["x"]), None);
        assert_eq!(bare.text, "## Journal\nx\n\n");
        assert_eq!(bare.outcome, Outcome::Created);

        let no_heading = apply(None, &append(None, &["x", "y"]), None);
        assert_eq!(no_heading.text, "x\ny\n");

        let replace = apply(
            None,
            &write(Operation::ReplaceLine {
                heading: heading("Day planner"),
                line_hash: line_hash("- [ ] Deep work"),
                ordinal: 0,
                new_line: "- [x] Deep work".into(),
            }),
            None,
        );
        assert_eq!(replace.outcome, Outcome::Created);
        assert_eq!(
            replace.text,
            "## Day planner\n- [x] Deep work <!--thock:also-->\n\n"
        );
    }

    #[test]
    fn create_on_existing_appends() {
        let result = apply(
            Some("a\n"),
            &write(Operation::Create {
                content: "b\nc\n".into(),
            }),
            None,
        );
        assert_eq!(result.outcome, Outcome::Applied);
        assert_eq!(result.text, "a\n\nb\nc\n");
        let same = apply(
            Some("b\r\nc\r\n"),
            &write(Operation::Create {
                content: "b\nc\n".into(),
            }),
            None,
        );
        assert_eq!(same.outcome, Outcome::Noop);
    }

    #[test]
    fn tick_and_retime_find_the_line_by_hash() {
        let tick = write(Operation::ReplaceLine {
            heading: heading("Day planner"),
            line_hash: line_hash("Deep work"),
            ordinal: 0,
            new_line: "- [x] 09:30 - 11:00 Deep work".into(),
        });
        let result = apply(Some(NOTE), &tick, None);
        assert_eq!(result.outcome, Outcome::Applied);
        assert!(
            result
                .text
                .contains("- [x] 09:30 - 11:00 Deep work\n\n### Calendar")
        );
        let again = apply(Some(&result.text), &tick, None);
        assert_eq!(again.outcome, Outcome::Noop);
        assert_eq!(again.text, result.text);
    }

    #[test]
    fn replace_missing_line_keeps_both() {
        let edit = write(Operation::ReplaceLine {
            heading: heading("Day planner"),
            line_hash: line_hash("Gone"),
            ordinal: 0,
            new_line: "- [ ] Gone, edited".into(),
        });
        let result = apply(Some(NOTE), &edit, None);
        assert_eq!(result.outcome, Outcome::KeptBoth);
        assert!(result.text.contains(
            "- [ ] 12:30 Lunch <!--gcal:abc-->\n- [ ] Gone, edited <!--thock:also-->\n\n## Personal"
        ));
        assert_eq!(
            apply(Some(&result.text), &edit, None).outcome,
            Outcome::Noop
        );
    }

    #[test]
    fn remove_line_and_ordinal() {
        let text = "## A\n- x\n- x\n- y\n";
        let remove_second = write(Operation::RemoveLine {
            heading: heading("A"),
            line_hash: line_hash("x"),
            ordinal: 1,
        });
        let result = apply(Some(text), &remove_second, None);
        assert_eq!(result.text, "## A\n- x\n- y\n");
        assert_eq!(result.outcome, Outcome::Applied);
        let missing = write(Operation::RemoveLine {
            heading: heading("A"),
            line_hash: line_hash("z"),
            ordinal: 0,
        });
        assert_eq!(apply(Some(text), &missing, None).outcome, Outcome::Noop);
        let out_of_range = write(Operation::RemoveLine {
            heading: heading("A"),
            line_hash: line_hash("y"),
            ordinal: 7,
        });
        assert_eq!(
            apply(Some(text), &out_of_range, None).text,
            "## A\n- x\n- x\n"
        );
    }

    /// With two lines of the same normalised text, `remove_line` cannot tell
    /// whether it already ran; a second application removes the sibling.
    /// Spec §8.3 accepts this: the phone sends one remove per tap.
    #[test]
    fn remove_line_on_duplicates_is_not_idempotent() {
        let text = "## A\n- x\n- x\n";
        let remove = write(Operation::RemoveLine {
            heading: heading("A"),
            line_hash: line_hash("x"),
            ordinal: 0,
        });
        let once = apply(Some(text), &remove, None);
        assert_eq!(once.text, "## A\n- x\n");
        assert!(!effect_present(&once.text, &remove));
        let twice = apply(Some(&once.text), &remove, None);
        assert_eq!(twice.text, "## A\n");
    }

    #[test]
    fn replace_section_fresh_and_stale() {
        let text = "## Goals\n- a\n- b\n\n## Notes\n";
        let fresh = write(Operation::ReplaceSection {
            heading: Heading::new("Goals"),
            base_hash: hash_lines(&["- a", "- b"]),
            lines: lines(&["- a", "- c"]),
        });
        let result = apply(Some(text), &fresh, None);
        assert_eq!(result.outcome, Outcome::Applied);
        assert_eq!(result.text, "## Goals\n- a\n- c\n\n## Notes\n");
        assert_eq!(
            apply(Some(&result.text), &fresh, None).outcome,
            Outcome::Noop
        );

        let stale = write(Operation::ReplaceSection {
            heading: Heading::new("Goals"),
            base_hash: "0000000000000000".into(),
            lines: lines(&["- a", "- d"]),
        });
        let result = apply(Some(text), &stale, None);
        assert_eq!(result.outcome, Outcome::KeptBoth);
        assert_eq!(
            result.text,
            "## Goals\n- a\n- b\n\n- a <!--thock:also-->\n- d\n\n## Notes\n"
        );
        assert_eq!(
            apply(Some(&result.text), &stale, None).outcome,
            Outcome::Noop
        );
    }

    #[test]
    fn crlf_and_unterminated_files() {
        let crlf = "## A\r\n- x\r\n";
        let result = apply(Some(crlf), &append(Some("A"), &["- y"]), None);
        assert_eq!(result.text, "## A\r\n- x\r\n- y\r\n");
        let bare = "## A\n- x";
        let result = apply(Some(bare), &append(Some("A"), &["- y"]), None);
        assert_eq!(result.text, "## A\n- x\n- y");
        let result = apply(Some(bare), &append(None, &["- z"]), None);
        assert_eq!(result.text, "## A\n- x\n- z");
    }

    #[test]
    fn fenced_lines_never_match() {
        let text = "## Snippets\n```\n- [ ] fenced task\n```\n- [ ] real task\n";
        let tick = write(Operation::ReplaceLine {
            heading: heading("Snippets"),
            line_hash: line_hash("fenced task"),
            ordinal: 0,
            new_line: "- [x] fenced task".into(),
        });
        let result = apply(Some(text), &tick, None);
        assert_eq!(result.outcome, Outcome::KeptBoth);
        assert!(!effect_present(
            text,
            &append(Some("Snippets"), &["- [ ] fenced task"])
        ));
        assert!(effect_present(
            text,
            &append(Some("Snippets"), &["- [ ] real task"])
        ));
    }

    const BACKLOG: &str = "# Backlog\n\n## Soon\n\n- [ ] Renew passport\n- [ ] Call the dentist\n\n### Home\n\n- [ ] Fix the gate\n  - the hinge first\n\n  - then the latch\n- [ ] Buy a smoke alarm\n\n## Someday\n\n- [ ] Learn woodworking\n\n### Thock\n\n- [ ] Week widget\n\n## Completed\n\n- [x] Book the car ✅ 2026-10-01\n";

    fn move_block(
        from: &str,
        text: &str,
        to: &str,
        place: Place,
        new_line: Option<&str>,
        create_under: Option<&str>,
    ) -> Write {
        let mut write = write(Operation::MoveBlock {
            heading: heading(from),
            line_hash: line_hash(text),
            ordinal: 0,
            to: heading(to),
            place,
            new_line: new_line.map(str::to_string),
            create_under: create_under.map(Heading::new),
        });
        write.path = "backlog.md".into();
        write
    }

    fn after(text: &str) -> Place {
        Place::After {
            line_hash: line_hash(text),
            ordinal: 0,
        }
    }

    #[test]
    fn move_block_reorders_inside_a_group() {
        let up = move_block("Soon", "Call the dentist", "Soon", Place::Top, None, None);
        let result = apply(Some(BACKLOG), &up, None);
        assert_eq!(result.outcome, Outcome::Applied);
        assert!(
            result
                .text
                .contains("## Soon\n\n- [ ] Call the dentist\n- [ ] Renew passport\n\n### Home")
        );
        assert_eq!(apply(Some(&result.text), &up, None).outcome, Outcome::Noop);

        let already = move_block("Soon", "Renew passport", "Soon", Place::Top, None, None);
        assert_eq!(apply(Some(BACKLOG), &already, None).outcome, Outcome::Noop);
        let to_end = move_block("Soon", "Renew passport", "Soon", Place::End, None, None);
        let result = apply(Some(BACKLOG), &to_end, None);
        assert!(
            result
                .text
                .contains("## Soon\n\n- [ ] Call the dentist\n- [ ] Renew passport\n\n### Home")
        );
        // The group's end is above its categories, never inside one.
        assert!(!result.text.contains("Buy a smoke alarm\n- [ ] Renew passport"));
    }

    #[test]
    fn move_block_carries_children_and_lands_after_an_anchor() {
        let gate = move_block(
            "Home",
            "Fix the gate",
            "Soon",
            after("Renew passport"),
            None,
            None,
        );
        let result = apply(Some(BACKLOG), &gate, None);
        assert_eq!(result.outcome, Outcome::Applied);
        assert_eq!(
            result.text,
            "# Backlog\n\n## Soon\n\n- [ ] Renew passport\n- [ ] Fix the gate\n  - the hinge first\n\n  - then the latch\n- [ ] Call the dentist\n\n### Home\n\n- [ ] Buy a smoke alarm\n\n## Someday\n\n- [ ] Learn woodworking\n\n### Thock\n\n- [ ] Week widget\n\n## Completed\n\n- [x] Book the car ✅ 2026-10-01\n"
        );
        assert!(effect_present(&result.text, &gate));
        assert_eq!(apply(Some(&result.text), &gate, None).outcome, Outcome::Noop);

        // Dropping after a task that has children lands below the children.
        let below = move_block(
            "Home",
            "Buy a smoke alarm",
            "Home",
            after("Fix the gate"),
            None,
            None,
        );
        assert_eq!(apply(Some(BACKLOG), &below, None).outcome, Outcome::Noop);
        let above = move_block("Home", "Buy a smoke alarm", "Home", Place::Top, None, None);
        let result = apply(Some(BACKLOG), &above, None);
        assert!(
            result
                .text
                .contains("### Home\n\n- [ ] Buy a smoke alarm\n- [ ] Fix the gate\n  - the hinge first\n\n  - then the latch\n\n## Someday")
        );
    }

    #[test]
    fn move_block_between_sections_and_missing_anchors() {
        let anchor_gone = move_block(
            "Soon",
            "Call the dentist",
            "Thock",
            after("Nowhere"),
            None,
            None,
        );
        let result = apply(Some(BACKLOG), &anchor_gone, None);
        assert_eq!(result.outcome, Outcome::Applied);
        assert!(
            result
                .text
                .contains("### Thock\n\n- [ ] Week widget\n- [ ] Call the dentist\n\n## Completed")
        );
        assert!(
            result
                .text
                .contains("## Soon\n\n- [ ] Renew passport\n\n### Home")
        );
        assert_eq!(apply(Some(&result.text), &anchor_gone, None).outcome, Outcome::Noop);

        let source_gone = move_block("Soon", "Not here", "Someday", Place::End, None, None);
        let result = apply(Some(BACKLOG), &source_gone, None);
        assert_eq!(result.outcome, Outcome::Noop);
        assert_eq!(result.text, BACKLOG);

        // `Someday › Home` is not Soon's Home: the category is made again
        // under Someday, the desk's own rule for a chevron move.
        let same_name = move_block(
            "Home",
            "Buy a smoke alarm",
            "Home",
            Place::End,
            None,
            Some("Someday"),
        );
        let result = apply(Some(BACKLOG), &same_name, None);
        assert_eq!(result.outcome, Outcome::SectionAdded);
        assert!(
            result
                .text
                .contains("### Thock\n\n- [ ] Week widget\n\n### Home\n- [ ] Buy a smoke alarm\n\n## Completed")
        );
        assert!(result.text.contains("  - then the latch\n\n## Someday"));
        assert!(effect_present(&result.text, &same_name));
        assert_eq!(apply(Some(&result.text), &same_name, None).outcome, Outcome::Noop);

        let new_group = move_block(
            "Home",
            "Buy a smoke alarm",
            "Garden",
            Place::End,
            None,
            Some("Someday"),
        );
        let mut new_group = new_group;
        if let Operation::MoveBlock { to, .. } = &mut new_group.operation {
            *to = Some(Heading {
                text: "Garden".into(),
                level: 3,
                ordinal: 0,
            });
        }
        let result = apply(Some(BACKLOG), &new_group, None);
        assert_eq!(result.outcome, Outcome::SectionAdded);
        assert!(
            result
                .text
                .contains("### Thock\n\n- [ ] Week widget\n\n### Garden\n- [ ] Buy a smoke alarm\n\n## Completed")
        );
        assert_eq!(apply(Some(&result.text), &new_group, None).outcome, Outcome::Noop);
    }

    #[test]
    fn tick_moves_to_completed_with_a_new_line() {
        let tick = move_block(
            "Home",
            "Fix the gate",
            "Completed",
            Place::End,
            Some("- [x] Fix the gate ✅ 2026-10-06"),
            None,
        );
        let result = apply(Some(BACKLOG), &tick, None);
        assert_eq!(result.outcome, Outcome::Applied);
        assert!(result.text.contains("### Home\n\n- [ ] Buy a smoke alarm\n\n## Someday"));
        assert!(
            result
                .text
                .ends_with("## Completed\n\n- [x] Book the car ✅ 2026-10-01\n- [x] Fix the gate ✅ 2026-10-06\n  - the hinge first\n\n  - then the latch\n")
        );
        assert!(effect_present(&result.text, &tick));
        assert_eq!(apply(Some(&result.text), &tick, None).outcome, Outcome::Noop);
    }

    #[test]
    fn remove_block_takes_the_children_and_one_blank_line() {
        let remove = write(Operation::RemoveBlock {
            heading: heading("Home"),
            line_hash: line_hash("Fix the gate"),
            ordinal: 0,
        });
        let result = apply(Some(BACKLOG), &remove, None);
        assert_eq!(result.outcome, Outcome::Applied);
        assert!(result.text.contains("### Home\n\n- [ ] Buy a smoke alarm\n\n## Someday"));
        assert!(effect_present(&result.text, &remove));
        assert_eq!(apply(Some(&result.text), &remove, None).outcome, Outcome::Noop);

        let only = write(Operation::RemoveBlock {
            heading: heading("Thock"),
            line_hash: line_hash("Week widget"),
            ordinal: 0,
        });
        let result = apply(Some(BACKLOG), &only, None);
        assert!(result.text.contains("### Thock\n\n## Completed"));

        let missing = write(Operation::RemoveBlock {
            heading: heading("Nowhere"),
            line_hash: line_hash("Week widget"),
            ordinal: 0,
        });
        assert!(effect_present(BACKLOG, &missing));
        assert_eq!(apply(Some(BACKLOG), &missing, None).outcome, Outcome::Noop);
    }

    #[test]
    fn effect_present_rules() {
        assert!(effect_present(
            NOTE,
            &append(Some("Journal"), &["Slept badly."])
        ));
        assert!(effect_present(NOTE, &append(Some("Journal"), &[])));
        assert!(!effect_present(
            NOTE,
            &append(Some("Journal"), &["Slept badly.", "More"])
        ));
        assert!(effect_present(
            NOTE,
            &append(None, &["- Groceries", "", "# Daily Closure"])
        ));
        assert!(effect_present(
            NOTE,
            &write(Operation::RemoveLine {
                heading: heading("Nowhere"),
                line_hash: line_hash("x"),
                ordinal: 0,
            })
        ));
    }
}
