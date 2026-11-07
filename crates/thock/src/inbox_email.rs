//! Reads a captured Gmail note as a conversation (V41 §4.4): who said what,
//! and what each message adds to the ones before it. Ticketing systems resend
//! the whole ticket with every reply, so most of a thread's lines are repeats.
//!
//! This is a reading of the note and nothing else. The file keeps every byte,
//! and whatever is folded here is one key away in the reader.

use chrono::NaiveDateTime;
use std::collections::{HashMap, HashSet};

/// The shortest run of lines worth folding: below it the fold's own row would
/// take as much room as the text.
const MIN_FOLDED_LINES: usize = 3;
/// `Key: value` lines only read as a record in a run this long; fewer are
/// prose that happens to hold a colon.
const MIN_FIELDS: usize = 3;
const MAX_FIELD_KEY_LENGTH: usize = 24;
const MAX_FIELD_KEY_WORDS: usize = 3;
const MAX_AUTHOR_LENGTH: usize = 40;
/// A link longer than this is shown by its address, shortened.
const MAX_LINK_LENGTH: usize = 60;

/// Shorter than this, a cut subject could match any line of the body.
const MIN_TITLE_STEM_LENGTH: usize = 12;
const MAX_TITLE_LENGTH: usize = 200;
const MAX_TICKET_QUEUE_LENGTH: usize = 6;
/// How a ticket says which other system it came from, ahead of a link there.
const IMPORT_NOTE_PREFIX: &str = "This ticket was imported from ";
/// The sentence and its link.
const MAX_IMPORT_NOTE_LINES: usize = 2;

/// The level the folds' attribution lines take: nothing else in a fold is one.
const ATTRIBUTION_HEADING: &str = "###### ";
const SECTION_PREFIX: &str = "## ";
const SECTION_DATE_SEPARATOR: &str = " — ";
const SECTION_DATE_FORMAT: &str = "%Y-%m-%d %H:%M";
const URL_DEFENSE_PREFIX: &str = "https://urldefense.com/v3/__";

#[derive(Debug, Clone, PartialEq)]
pub struct EmailThread {
    /// What sits above the first message: anything the person added.
    pub preamble: String,
    pub messages: Vec<EmailMessage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmailMessage {
    /// The `From` header as captured.
    pub from: String,
    /// Who wrote it when `from` is a system relaying them.
    pub author: Option<String>,
    pub sent: Option<NaiveDateTime>,
    /// The sender's position in the thread, in order of first appearance, so
    /// one person keeps one colour.
    pub participant: u32,
    pub blocks: Vec<EmailBlock>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmailBlock {
    /// Markdown.
    Text(String),
    /// Markdown: a ticketing system's line that stays beside a person's words,
    /// set apart by `quiet_system_lines`.
    Meta(String),
    /// A run of `Key: value` lines. `shown` holds what this message
    /// introduced or changed, `unchanged` what it only repeated.
    Fields {
        shown: Vec<EmailField>,
        unchanged: Vec<EmailField>,
    },
    Fold {
        kind: FoldKind,
        /// Markdown.
        text: String,
        lines: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldKind {
    /// Lines an earlier message of the thread already said.
    Repeated,
    /// The trail of a reply: `On … wrote:` and its `>` lines.
    Quoted,
    Signature,
    /// What a ticketing system wrote above a person's words.
    System,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmailField {
    pub key: String,
    pub value: String,
    /// The value an earlier message gave this key, when it has changed.
    pub previous: Option<String>,
    /// The first web link in the value.
    pub link: Option<String>,
}

/// Reads a note's body into its messages. A note without message headings is
/// one message from `from`.
pub fn parse_thread(body: &str, from: Option<&str>) -> EmailThread {
    let (mut preamble, mut sections) = split_sections(body);
    if sections.is_empty() {
        preamble.clear();
        sections.push(Section {
            from: from.unwrap_or_default(),
            sent: None,
            lines: body.lines().collect(),
        });
    }
    let mut reader = ThreadReader::default();
    EmailThread {
        preamble,
        messages: sections
            .iter()
            .map(|section| reader.read(section))
            .collect(),
    }
}

/// The whole subject when a system cut it to `Start of the subj...` and the
/// body still has it. The body's own prefix may differ (`Ticket 42:` for
/// `HD 42:`), so the subject keeps its prefix and takes only the rest.
pub fn full_title(title: &str, body: &str) -> Option<String> {
    let cut = title.strip_suffix("...").or(title.strip_suffix('…'))?;
    let (prefix, stem) = match cut.split_once(": ") {
        Some((prefix, stem)) => (Some(prefix), stem),
        None => (None, cut),
    };
    if stem.chars().count() < MIN_TITLE_STEM_LENGTH {
        return None;
    }
    let rest = body.lines().find_map(|line| {
        let (_, after) = line.split_once(stem)?;
        let found = format!("{stem}{after}");
        let found = found.trim_end();
        let longer = found.trim_end_matches(['.', '…']).len() > stem.len();
        (longer && found.chars().count() <= MAX_TITLE_LENGTH).then(|| found.to_string())
    })?;
    Some(match prefix {
        Some(prefix) => format!("{prefix}: {rest}"),
        None => rest,
    })
}

/// Takes a message's opening line off when it only restates `title`: the
/// reader shows the subject above the bar. This is the reading alone; the note
/// keeps the line.
pub fn drop_restated_subject(thread: &mut EmailThread, title: &str) {
    for message in &mut thread.messages {
        let Some(EmailBlock::Text(text)) = message.blocks.first_mut() else {
            continue;
        };
        let (first, rest) = text.split_once('\n').unwrap_or((text.as_str(), ""));
        if !restates_subject(first, title) {
            continue;
        }
        let rest = rest.trim_start().to_string();
        if rest.is_empty() {
            message.blocks.remove(0);
        } else {
            *text = rest;
        }
    }
}

/// Tells a relayed message's system text from its person's words, so the
/// message opens on the words: all that comes before the `(by …)` line, any
/// line restating `title` and a note of where the ticket was imported from
/// fold, the `(by …)` line goes since the card's header gives it, and a note of
/// the ticket's own edit stays quiet, or goes when a changed field already says
/// it. An import note repeated from an earlier message is told from the comments
/// around it too. This is the reading alone; the note keeps every line.
pub fn quiet_system_lines(thread: &mut EmailThread, title: &str) {
    for message in &mut thread.messages {
        if message.author.is_none() {
            continue;
        }
        let shown_fields = shown_fields(&message.blocks);
        let mut before_author = message.blocks.iter().any(
            |block| matches!(block, EmailBlock::Text(text) if text.lines().any(|line| parse_author(line).is_some())),
        );
        for block in std::mem::take(&mut message.blocks) {
            match block {
                EmailBlock::Text(text) if !text.lines().any(is_fence) => split_system_text(
                    &text,
                    title,
                    &shown_fields,
                    &mut before_author,
                    &mut message.blocks,
                ),
                EmailBlock::Fold {
                    kind: FoldKind::Repeated,
                    text,
                    ..
                } if !text.lines().any(is_fence) => {
                    split_repeated_notes(&text, &mut message.blocks)
                }
                other => message.blocks.push(other),
            }
        }
    }
}

/// Sets each `Comment #3 (by …)` line in a message's folded text as a heading,
/// the one thing in a fold that is, so the reader can draw it apart from the
/// words it introduces. A label on the line above joins it. This is the reading
/// alone; the note keeps every line.
pub fn mark_attributions(thread: &mut EmailThread) {
    for message in &mut thread.messages {
        for block in &mut message.blocks {
            if let EmailBlock::Fold { kind, text, .. } = block
                && *kind != FoldKind::Quoted
                && !text.lines().any(is_fence)
            {
                *text = text
                    .split("\n\n")
                    .map(attribution_as_heading)
                    .collect::<Vec<_>>()
                    .join("\n\n");
            }
        }
    }
}

fn attribution_as_heading(paragraph: &str) -> String {
    let lines: Vec<&str> = paragraph.lines().collect();
    let Some(author_line) = lines.iter().position(|line| is_author_line(line)) else {
        return paragraph.to_string();
    };
    let start = by_line_start(&lines, author_line);
    let heading = format!(
        "{ATTRIBUTION_HEADING}{}",
        lines[start..=author_line].join(" ")
    );
    lines[..start]
        .iter()
        .copied()
        .chain([heading.as_str()])
        .chain(lines[author_line + 1..].iter().copied())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The fields a message keeps on screen, as `(key, value)`.
fn shown_fields(blocks: &[EmailBlock]) -> Vec<(String, String)> {
    blocks
        .iter()
        .filter_map(|block| match block {
            EmailBlock::Fields { shown, unchanged } => Some(split_fields(shown, unchanged).0),
            _ => None,
        })
        .flatten()
        .map(|field| (field.key.clone(), field.value.clone()))
        .collect()
}

fn split_system_text(
    text: &str,
    title: &str,
    shown_fields: &[(String, String)],
    before_author: &mut bool,
    blocks: &mut Vec<EmailBlock>,
) {
    for paragraph in text.split("\n\n") {
        let lines: Vec<&str> = paragraph.lines().collect();
        match lines.iter().rposition(|line| parse_author(line).is_some()) {
            Some(author_line) => {
                *before_author = false;
                let by_line = by_line_start(&lines, author_line);
                push_system_fold(blocks, &lines[..by_line]);
                // The card's header already gives the author and the date.
                if !is_author_line(lines[author_line]) {
                    let by_line = lines[by_line..=author_line].join("\n");
                    push_block(blocks, EmailBlock::Meta(by_line));
                }
                let own = &lines[author_line + 1..];
                if !own.is_empty() {
                    push_block(blocks, EmailBlock::Text(own.join("\n")));
                }
            }
            None if *before_author || is_import_note(&lines) => push_system_fold(blocks, &lines),
            None if lines.iter().all(|line| is_system_line(line, title)) => {
                for line in lines {
                    if is_change_line(line) {
                        if !change_is_shown(line, shown_fields) {
                            push_block(blocks, EmailBlock::Meta(line.to_string()));
                        }
                    } else {
                        push_system_fold(blocks, &[line]);
                    }
                }
            }
            None => push_block(blocks, EmailBlock::Text(lines.join("\n"))),
        }
    }
}

/// Parts the import notes in a repeated fold from the comments around them.
fn split_repeated_notes(text: &str, blocks: &mut Vec<EmailBlock>) {
    fn push_comments(blocks: &mut Vec<EmailBlock>, paragraphs: &mut Vec<&str>) {
        if paragraphs.is_empty() {
            return;
        }
        let text = paragraphs.join("\n\n");
        let lines = text.lines().filter(|line| !is_blank(line)).count();
        paragraphs.clear();
        blocks.push(EmailBlock::Fold {
            kind: FoldKind::Repeated,
            text,
            lines,
        });
    }
    let mut comments = Vec::new();
    for paragraph in text.split("\n\n") {
        let lines: Vec<&str> = paragraph.lines().collect();
        if is_import_note(&lines) {
            push_comments(blocks, &mut comments);
            push_system_fold(blocks, &lines);
        } else {
            comments.push(paragraph);
        }
    }
    push_comments(blocks, &mut comments);
}

/// Where the `(by …)` line starts: a system may set its label on the line
/// above (`Description` then `(by ana on …):`).
fn by_line_start(lines: &[&str], author_line: usize) -> usize {
    let label_above = lines[author_line].trim_start().starts_with("(by ")
        && author_line
            .checked_sub(1)
            .and_then(|index| lines.get(index))
            .is_some_and(|line| line.split_whitespace().count() <= MAX_FIELD_KEY_WORDS);
    author_line - usize::from(label_above)
}

fn push_system_fold(blocks: &mut Vec<EmailBlock>, lines: &[&str]) {
    let count = lines.iter().filter(|line| !is_blank(line)).count();
    if count > 0 {
        push_block(
            blocks,
            EmailBlock::Fold {
                kind: FoldKind::System,
                text: lines.join("\n"),
                lines: count,
            },
        );
    }
}

fn is_import_note(lines: &[&str]) -> bool {
    lines.len() <= MAX_IMPORT_NOTE_LINES
        && lines
            .first()
            .is_some_and(|line| line.trim_start().starts_with(IMPORT_NOTE_PREFIX))
}

fn is_system_line(line: &str, title: &str) -> bool {
    restates_subject(line, title) || is_change_line(line)
}

/// `Changing Status to CLOSED`: a ticket noting its own edit, as its key and
/// value.
fn parse_change(line: &str) -> Option<(&str, &str)> {
    line.trim()
        .strip_prefix("Changing ")
        .and_then(|rest| rest.split_once(" to "))
        .filter(|(key, value)| {
            key.split_whitespace().count() <= MAX_FIELD_KEY_WORDS
                && value.split_whitespace().count() == 1
        })
}

fn is_change_line(line: &str) -> bool {
    parse_change(line).is_some()
}

fn change_is_shown(line: &str, shown_fields: &[(String, String)]) -> bool {
    parse_change(line).is_some_and(|(key, value)| {
        shown_fields.iter().any(|(shown_key, shown_value)| {
            shown_key.eq_ignore_ascii_case(key) && shown_value.eq_ignore_ascii_case(value)
        })
    })
}

/// Splits `HD 42: The farm is down` into the ticket and its subject, for a
/// list row, where the number would spend the title's room. `Q4: planning` and
/// `Re: lunch` aren't tickets.
pub fn ticket_prefix(title: &str) -> Option<(&str, &str)> {
    let (prefix, subject) = title.split_once(": ")?;
    let subject = subject.trim();
    let is_ticket = match prefix.split_whitespace().collect::<Vec<_>>()[..] {
        [word, number] => (word == "Ticket" || is_queue(word)) && is_number(number),
        [key] => is_ticket_key(key),
        _ => false,
    };
    (is_ticket && !subject.is_empty()).then_some((prefix, subject))
}

/// Splits `The farm is down | FS-42` into the subject and the id another
/// system gave it, which a row can do without when it's short of room.
pub fn trailing_ticket(title: &str) -> Option<(&str, &str)> {
    let (subject, id) = title.rsplit_once(" | ")?;
    let subject = subject.trim_end();
    (is_ticket_key(id.trim()) && !subject.is_empty()).then_some((subject, id.trim()))
}

fn is_queue(word: &str) -> bool {
    (2..=MAX_TICKET_QUEUE_LENGTH).contains(&word.len())
        && word.chars().all(|character| character.is_ascii_uppercase())
}

fn is_number(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|character| character.is_ascii_digit())
}

/// `INC-42`.
fn is_ticket_key(key: &str) -> bool {
    key.split_once('-')
        .is_some_and(|(word, number)| is_queue(word) && is_number(number))
}

/// The two may name the ticket differently (`Ticket 42:The farm` under
/// `HD 42: The farm`), so what comes after the prefix is what's compared.
fn restates_subject(line: &str, title: &str) -> bool {
    let subject = |text: &str| {
        let text = text.split_once(':').map_or(text, |(_, rest)| rest);
        normalized(text).to_lowercase()
    };
    let title = subject(title);
    title.chars().count() >= MIN_TITLE_STEM_LENGTH && subject(line) == title
}

/// The line a collapsed message shows: the first of its own words, past the
/// ticket's `(by …)` line.
pub fn snippet(message: &EmailMessage) -> Option<String> {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            EmailBlock::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .flat_map(|text| text.split("\n\n"))
        .find(|paragraph| !paragraph.lines().any(|line| parse_author(line).is_some()))
        .and_then(|paragraph| {
            paragraph
                .lines()
                .map(normalized)
                .find(|line| !line.is_empty())
        })
}

/// A value that says nothing: `N/A`, a dash, or no value at all.
fn is_empty_value(value: &str) -> bool {
    let value = value.trim();
    value.is_empty()
        || ["—", "-", "n/a"]
            .iter()
            .any(|empty| value.eq_ignore_ascii_case(empty))
}

/// How a record reads: what stays on screen and what folds. A record the
/// thread hasn't seen is reference, not news, so all of it folds; a later copy
/// keeps what it introduced or changed. Values that say nothing are dropped
/// from both, unless they replaced a real one.
pub fn split_fields<'a>(
    shown: &'a [EmailField],
    unchanged: &'a [EmailField],
) -> (Vec<&'a EmailField>, Vec<&'a EmailField>) {
    let says_something = |field: &&EmailField| !is_empty_value(&field.value);
    let first_sight = unchanged.is_empty() && shown.iter().all(|field| field.previous.is_none());
    if first_sight {
        return (Vec::new(), shown.iter().filter(says_something).collect());
    }
    (
        shown
            .iter()
            .filter(|field| field.previous.is_some() || says_something(field))
            .collect(),
        unchanged.iter().filter(says_something).collect(),
    )
}

/// The two letters of a sender's badge: `Marta Reis` → `MR`.
pub fn initials(name: &str) -> String {
    let name = name.split('@').next().unwrap_or(name);
    let mut words = name
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty());
    let first = words.next();
    let last = words.next_back();
    [first, last]
        .into_iter()
        .flatten()
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .collect()
}

struct Section<'a> {
    from: &'a str,
    sent: Option<NaiveDateTime>,
    lines: Vec<&'a str>,
}

/// A message heading is `## <from> — <date>` as `thread_sections` writes it.
/// The date is what tells it from a heading inside an email.
fn parse_section_heading(line: &str) -> Option<(&str, NaiveDateTime)> {
    let (from, date) = line
        .strip_prefix(SECTION_PREFIX)?
        .rsplit_once(SECTION_DATE_SEPARATOR)?;
    let sent = NaiveDateTime::parse_from_str(date.trim(), SECTION_DATE_FORMAT).ok()?;
    Some((from.trim(), sent))
}

fn split_sections(body: &str) -> (String, Vec<Section<'_>>) {
    let mut preamble = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    for line in body.lines() {
        if let Some((from, sent)) = parse_section_heading(line) {
            sections.push(Section {
                from,
                sent: Some(sent),
                lines: Vec::new(),
            });
        } else if let Some(section) = sections.last_mut() {
            section.lines.push(line);
        } else {
            preamble.push(line);
        }
    }
    (preamble.join("\n").trim().to_string(), sections)
}

fn normalized(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn is_quoted(line: &str) -> bool {
    line.trim_start().starts_with('>')
}

fn is_fence(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("```") || line.starts_with("~~~")
}

/// `-- ` on a line of its own, after a blank line so that a Markdown heading
/// underline isn't taken for one.
fn is_signature_rule(lines: &[&str], index: usize) -> bool {
    lines.get(index).is_some_and(|line| line.trim_end() == "--")
        && (index == 0 || lines.get(index - 1).is_some_and(|line| is_blank(line)))
}

/// Which lines belong to a code fence, its own two lines included.
fn fenced_lines(lines: &[&str]) -> Vec<bool> {
    let mut in_fence = false;
    lines
        .iter()
        .map(|line| {
            let edge = is_fence(line);
            let fenced = in_fence || edge;
            in_fence ^= edge;
            fenced
        })
        .collect()
}

/// Whether `wrote` finishes an attribution wrapped over two lines, the first
/// being `before`: a sentence of the sender's own stays out of the fold.
fn continues_attribution(before: Option<&&str>, wrote: &str) -> bool {
    !wrote.trim_start().starts_with("On ")
        && before.is_some_and(|line| {
            let line = line.trim();
            line.starts_with("On ") && !line.ends_with(['.', '!', '?'])
        })
}

/// Where the trail of a reply starts, when the message has one. A `>` run with
/// the sender's words after it is an inline reply, and those stay.
fn quote_start(lines: &[&str], fenced: &[bool]) -> Option<usize> {
    let forwarded = (0..lines.len()).find(|&index| {
        let line = lines[index].trim();
        !fenced[index]
            && ((line.starts_with("-----") && line.contains("Original Message"))
                || (line.starts_with("From: ")
                    && lines
                        .get(index + 1)
                        .is_some_and(|next| next.trim().starts_with("Sent: "))))
    });
    let last_own = lines
        .iter()
        .rposition(|line| !is_blank(line) && !is_quoted(line));
    let after_own = last_own.map_or(0, |index| index + 1);
    let trailing = lines[after_own..]
        .iter()
        .any(|line| is_quoted(line))
        .then(|| match last_own {
            Some(last) if lines[last].trim_end().ends_with("wrote:") => {
                let before = last.checked_sub(1).and_then(|index| lines.get(index));
                if continues_attribution(before, lines[last]) {
                    last - 1
                } else {
                    last
                }
            }
            _ => after_own,
        });
    match (forwarded, trailing) {
        (Some(forwarded), Some(trailing)) => Some(forwarded.min(trailing)),
        (forwarded, trailing) => forwarded.or(trailing),
    }
}

fn fold(kind: FoldKind, lines: &[&str]) -> Option<EmailBlock> {
    let count = lines.iter().filter(|line| !is_blank(line)).count();
    (count > 0).then(|| EmailBlock::Fold {
        kind,
        text: compact_links(lines.join("\n").trim()),
        lines: count,
    })
}

/// Splits what the sender wrote from the signature and the quoted trail.
fn split_tail<'a, 'b>(lines: &'a [&'b str]) -> (&'a [&'b str], Vec<EmailBlock>) {
    let fenced = fenced_lines(lines);
    let signature =
        (0..lines.len()).find(|&index| !fenced[index] && is_signature_rule(lines, index));
    let quote = quote_start(lines, &fenced);
    match (signature, quote) {
        (Some(signature), Some(quote)) if signature < quote => (
            &lines[..signature],
            [
                fold(FoldKind::Signature, &lines[signature..quote]),
                fold(FoldKind::Quoted, &lines[quote..]),
            ]
            .into_iter()
            .flatten()
            .collect(),
        ),
        (_, Some(quote)) => (
            &lines[..quote],
            fold(FoldKind::Quoted, &lines[quote..])
                .into_iter()
                .collect(),
        ),
        (Some(signature), None) => (
            &lines[..signature],
            fold(FoldKind::Signature, &lines[signature..])
                .into_iter()
                .collect(),
        ),
        (None, None) => (lines, Vec::new()),
    }
}

/// Lines split on blanks, a code fence kept whole.
fn paragraphs<'a>(lines: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut paragraphs = Vec::new();
    let mut current = Vec::new();
    let mut in_fence = false;
    for line in lines {
        if is_fence(line) {
            in_fence = !in_fence;
        }
        if is_blank(line) && !in_fence {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
        } else {
            current.push(*line);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs
}

fn parse_field(line: &str) -> Option<(String, String)> {
    let (key, value) = line.trim().split_once(':')?;
    let key_shaped = key.chars().next()?.is_uppercase()
        && key.len() <= MAX_FIELD_KEY_LENGTH
        && key.split_whitespace().count() <= MAX_FIELD_KEY_WORDS
        && key
            .chars()
            .all(|character| character.is_alphanumeric() || " -_/".contains(character));
    // Without a space after the colon it's a time or an address, not a field.
    let separated = value.is_empty() || value.starts_with(char::is_whitespace);
    (key_shaped && separated).then(|| (key.trim().to_string(), normalized(value)))
}

fn parse_fields(paragraph: &[&str]) -> Option<Vec<(String, String)>> {
    paragraph.iter().map(|line| parse_field(line)).collect()
}

/// The `Key: value` lines from the start of `paragraphs` when they read as a
/// record, and how many paragraphs they took either way. Repeated keys are a
/// log, not a record.
fn field_run(paragraphs: &[Vec<&str>]) -> (Option<Vec<(String, String)>>, usize) {
    let run: Vec<Vec<(String, String)>> = paragraphs
        .iter()
        .map_while(|paragraph| parse_fields(paragraph))
        .collect();
    let taken = run.len();
    let fields: Vec<(String, String)> = run.into_iter().flatten().collect();
    let keys: HashSet<&str> = fields.iter().map(|(key, _)| key.as_str()).collect();
    let is_record = fields.len() >= MIN_FIELDS && keys.len() == fields.len();
    (is_record.then_some(fields), taken)
}

/// `(by ana on 03/02/26 10:25am):` → `ana`.
fn parse_author(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once("(by ")?;
    let (author, details) = rest.split_once(" on ")?;
    let (date, _) = details.split_once(')')?;
    let author = author.trim();
    let shaped = !author.is_empty() && author.len() <= MAX_AUTHOR_LENGTH && !author.contains(')');
    // The date is what tells a ticket's line from prose: `(by the team on Friday)`.
    (shaped && date.contains(|character: char| character.is_ascii_digit())).then_some(author)
}

/// The line is only the `(by …)` attribution, with no words of the person's
/// after it.
fn is_author_line(line: &str) -> bool {
    parse_author(line).is_some() && line.trim_end().ends_with([')', ':'])
}

/// A sender with a display name is a person; a bare address may be a system
/// writing on someone's behalf.
fn is_bare_address(from: &str) -> bool {
    !from.contains('<') && from.contains('@')
}

fn participant_key(from: &str, author: Option<&str>) -> String {
    let key = author.unwrap_or_else(|| {
        from.split_once('<')
            .and_then(|(_, rest)| rest.split_once('>'))
            .map_or(from, |(address, _)| address)
    });
    key.trim().to_lowercase()
}

/// A stretch of a message on its way to becoming blocks.
enum Part {
    Text {
        text: String,
        lines: usize,
        repeated: bool,
    },
    Fields(Vec<(String, String)>),
}

impl Part {
    fn folds(&self) -> bool {
        matches!(self, Part::Text { repeated: true, lines, .. } if *lines >= MIN_FOLDED_LINES)
    }
}

fn push_text(parts: &mut Vec<Part>, paragraph: &[&str], is_repeated: bool) {
    let added = paragraph.join("\n");
    if let Some(Part::Text {
        text,
        lines,
        repeated,
    }) = parts.last_mut()
        && *repeated == is_repeated
    {
        text.push_str("\n\n");
        text.push_str(&added);
        *lines += paragraph.len();
        return;
    }
    parts.push(Part::Text {
        text: added,
        lines: paragraph.len(),
        repeated: is_repeated,
    });
}

/// A reply opens by repeating the subject line, which reads as noise above
/// what's new. It joins the message's first fold instead of keeping a fold of
/// its own too short to be worth a row.
fn fold_opening_repeat(parts: &mut Vec<Part>) {
    let opens_with_short_repeat = matches!(
        parts.first(),
        Some(first @ Part::Text { repeated: true, .. }) if !first.folds()
    );
    if !opens_with_short_repeat || !parts.iter().any(Part::folds) {
        return;
    }
    let Part::Text {
        text: opening,
        lines: opening_lines,
        ..
    } = parts.remove(0)
    else {
        return;
    };
    if let Some(Part::Text { text, lines, .. }) = parts.iter_mut().find(|part| part.folds()) {
        *text = format!("{opening}\n\n{text}");
        *lines += opening_lines;
    }
}

fn push_block(blocks: &mut Vec<EmailBlock>, block: EmailBlock) {
    match (blocks.last_mut(), &block) {
        (Some(EmailBlock::Text(last)), EmailBlock::Text(added))
        | (Some(EmailBlock::Meta(last)), EmailBlock::Meta(added)) => {
            last.push_str("\n\n");
            last.push_str(added);
        }
        (
            Some(EmailBlock::Fold {
                kind: FoldKind::System,
                text,
                lines,
            }),
            EmailBlock::Fold {
                kind: FoldKind::System,
                text: added,
                lines: more,
            },
        ) => {
            text.push_str("\n\n");
            text.push_str(added);
            *lines += more;
        }
        _ => blocks.push(block),
    }
}

/// What the thread has said so far, carried from message to message.
#[derive(Default)]
struct ThreadReader {
    seen_lines: HashSet<String>,
    fields: HashMap<String, String>,
    participants: Vec<String>,
}

impl ThreadReader {
    fn is_repeated(&self, paragraph: &[&str]) -> bool {
        paragraph
            .iter()
            .all(|line| !is_quoted(line) && self.seen_lines.contains(&normalized(line)))
    }

    fn participant(&mut self, key: String) -> u32 {
        let position = self
            .participants
            .iter()
            .position(|participant| *participant == key)
            .unwrap_or_else(|| {
                self.participants.push(key);
                self.participants.len() - 1
            });
        position as u32
    }

    fn read_fields(&mut self, fields: Vec<(String, String)>) -> EmailBlock {
        let mut shown = Vec::new();
        let mut unchanged = Vec::new();
        for (key, value) in fields {
            let previous = self.fields.insert(key.clone(), value.clone());
            let repeated = previous.as_ref() == Some(&value);
            let field = EmailField {
                link: first_link(&value),
                value: display_links(&value),
                previous: previous
                    .filter(|_| !repeated)
                    .map(|previous| display_links(&previous)),
                key,
            };
            if repeated {
                unchanged.push(field);
            } else {
                shown.push(field);
            }
        }
        EmailBlock::Fields { shown, unchanged }
    }

    fn read(&mut self, section: &Section) -> EmailMessage {
        let (own, tail) = split_tail(&section.lines);
        let paragraphs = paragraphs(own);
        let mut parts = Vec::new();
        let mut author = None;
        let mut index = 0;
        let mut prose_until = 0;
        while let Some(paragraph) = paragraphs.get(index) {
            if index >= prose_until {
                let (fields, taken) = field_run(&paragraphs[index..]);
                if let Some(fields) = fields {
                    parts.push(Part::Fields(fields));
                    index += taken;
                    continue;
                }
                // A log's tail mustn't read as a record once its repeat is
                // behind it.
                prose_until = index + taken;
            }
            let repeated = self.is_repeated(paragraph);
            if !repeated && author.is_none() && is_bare_address(section.from) {
                author = paragraph.iter().find_map(|line| parse_author(line));
            }
            push_text(&mut parts, paragraph, repeated);
            index += 1;
        }
        fold_opening_repeat(&mut parts);

        let mut blocks = Vec::new();
        for part in parts {
            let folds = part.folds();
            let block = match part {
                Part::Text { text, lines, .. } if folds => EmailBlock::Fold {
                    kind: FoldKind::Repeated,
                    text: compact_links(&text),
                    lines,
                },
                Part::Text { text, .. } => EmailBlock::Text(compact_links(&text)),
                Part::Fields(fields) => self.read_fields(fields),
            };
            push_block(&mut blocks, block);
        }
        blocks.extend(tail);

        self.seen_lines.extend(
            own.iter()
                .filter(|line| !is_blank(line))
                .map(|line| normalized(line)),
        );
        let participant = self.participant(participant_key(section.from, author));
        EmailMessage {
            from: section.from.to_string(),
            author: author.map(str::to_string),
            sent: section.sent,
            participant,
            blocks,
        }
    }
}

fn is_web_link(token: &str) -> bool {
    token.starts_with("https://") || token.starts_with("http://")
}

/// Trailing punctuation belongs to the sentence, not the link.
fn split_link(token: &str) -> (&str, &str) {
    let link = token.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    token.split_at(link.len())
}

fn first_link(value: &str) -> Option<String> {
    value
        .split_whitespace()
        .map(|token| split_link(token).0)
        .find(|token| is_web_link(token))
        .map(str::to_string)
}

/// The address a person would recognise: a link rewriter's wrapper comes off
/// and the scheme goes. The link it opens stays the one that was sent.
fn link_label(link: &str) -> String {
    let inner = link
        .strip_prefix(URL_DEFENSE_PREFIX)
        .and_then(|rest| rest.split_once("__;"))
        .map_or(link, |(inner, _)| inner);
    let address = inner
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    if address.chars().count() <= MAX_LINK_LENGTH {
        return address.to_string();
    }
    let shortened: String = address.chars().take(MAX_LINK_LENGTH).collect();
    format!("{shortened}…")
}

fn shortens(link: &str) -> bool {
    is_web_link(link)
        && (link.len() > MAX_LINK_LENGTH || link.starts_with(URL_DEFENSE_PREFIX))
        // Brackets would end the Markdown link early.
        && !link.contains(['(', ')', '[', ']', '<', '>'])
}

fn map_links(line: &str, replace: impl Fn(&str) -> String) -> String {
    let mut in_code = false;
    line.split_inclusive(char::is_whitespace)
        .map(|token| {
            let ticks = token.matches('`').count();
            let is_code = in_code || ticks > 0;
            in_code ^= ticks % 2 == 1;
            let word = token.trim_end();
            let (link, punctuation) = split_link(word);
            if !is_code && shortens(link) {
                let space = &token[word.len()..];
                format!("{}{punctuation}{space}", replace(link))
            } else {
                token.to_string()
            }
        })
        .collect()
}

fn escape_markdown(text: &str) -> String {
    text.chars()
        .flat_map(|character| {
            let escaped = matches!(character, '*' | '_' | '`');
            escaped.then_some('\\').into_iter().chain([character])
        })
        .collect()
}

fn display_links(value: &str) -> String {
    map_links(value, link_label)
}

/// Long bare links become Markdown links named by their address, outside
/// code fences. Thread emails carry them a line wide.
fn compact_links(text: &str) -> String {
    let mut in_fence = false;
    text.lines()
        .map(|line| {
            if is_fence(line) {
                in_fence = !in_fence;
            }
            if in_fence {
                return line.to_string();
            }
            map_links(line, |link| {
                format!("[{}]({link})", escape_markdown(&link_label(link)))
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folds_attribution_lines_become_its_headings() {
        let fold = |kind, text: &str| EmailThread {
            preamble: String::new(),
            messages: vec![EmailMessage {
                from: "helpdesk@example.com".to_string(),
                author: None,
                sent: None,
                participant: 0,
                blocks: vec![EmailBlock::Fold {
                    kind,
                    text: text.to_string(),
                    lines: 1,
                }],
            }],
        };
        let marked = |kind, text: &str| {
            let mut thread = fold(kind, text);
            mark_attributions(&mut thread);
            match thread.messages.remove(0).blocks.remove(0) {
                EmailBlock::Fold { text, .. } => text,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            marked(
                FoldKind::Repeated,
                "Comment #3 (by bo on 10/15/26 09:38am):\n\nOh I see.\n\nDescription\n(by ana on 10/13/26 05:25pm):\n\nThe farm is down."
            ),
            "###### Comment #3 (by bo on 10/15/26 09:38am):\n\nOh I see.\n\n###### Description (by ana on 10/13/26 05:25pm):\n\nThe farm is down."
        );
        let quoted = "> Comment #3 (by bo on 10/15/26 09:38am):";
        assert_eq!(marked(FoldKind::Quoted, quoted), quoted);
        let prose = "This was fixed (by the team on Friday).";
        assert_eq!(marked(FoldKind::Repeated, prose), prose);
    }

    fn thread(body: &str) -> EmailThread {
        parse_thread(body, Some("Marta Reis <marta@example.com>"))
    }

    fn folds(message: &EmailMessage) -> Vec<(FoldKind, usize)> {
        message
            .blocks
            .iter()
            .filter_map(|block| match block {
                EmailBlock::Fold { kind, lines, .. } => Some((*kind, *lines)),
                _ => None,
            })
            .collect()
    }

    fn text(message: &EmailMessage) -> String {
        message
            .blocks
            .iter()
            .filter_map(|block| match block {
                EmailBlock::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    const TICKET: &str = "\
Ticket 42: The render farm is down

Comment #1 (by rsilva on 03/02/26 09:10am):

The farm stopped taking jobs this morning.
Could someone look into it?

Sub-Type:    Farm

Status:      OPEN

Priority:    3 Medium

Team:        render-ops
";

    const TICKET_REPLY: &str = "\
Ticket 42: The render farm is down

Comment #2 (by jlopes on 03/02/26 10:25am):

The print server is down again.

Comment #1 (by rsilva on 03/02/26 09:10am):

The farm stopped taking jobs this morning.
Could someone look into it?

Sub-Type:    Farm

Status:      IN PROGRESS

Priority:    3 Medium

Team:        render-ops
";

    fn ticket_thread() -> EmailThread {
        thread(&format!(
            "## helpdesk@example.com — 2026-11-04 08:55\n\n{TICKET}\n\
             ## helpdesk@example.com — 2026-11-04 09:13\n\n{TICKET_REPLY}"
        ))
    }

    #[test]
    fn headings_split_messages_and_the_rest_stays_in_them() {
        let thread = thread(
            "My note on this.\n\n\
             ## Marta Reis <marta@example.com> — 2026-10-26 14:18\n\n\
             Hello.\n\n## Summary\n\nA heading of her own.\n\n\
             ## Ana <ana@example.com> — 2026-10-26 14:21\n\nHi.\n",
        );
        assert_eq!(thread.preamble, "My note on this.");
        assert_eq!(thread.messages.len(), 2);
        assert_eq!(thread.messages[0].from, "Marta Reis <marta@example.com>");
        assert_eq!(
            thread.messages[0].sent.map(|sent| sent.to_string()),
            Some("2026-10-26 14:18:00".to_string())
        );
        assert!(text(&thread.messages[0]).contains("## Summary"));
        assert_eq!(text(&thread.messages[1]), "Hi.");
    }

    #[test]
    fn a_note_without_headings_is_one_message_from_its_sender() {
        let thread = thread("Just the one.\n");
        assert_eq!(thread.preamble, "");
        assert_eq!(thread.messages.len(), 1);
        assert_eq!(thread.messages[0].from, "Marta Reis <marta@example.com>");
        assert_eq!(thread.messages[0].sent, None);
        assert_eq!(text(&thread.messages[0]), "Just the one.");
    }

    #[test]
    fn a_relayed_message_names_the_person_behind_it() {
        let thread = ticket_thread();
        let authors: Vec<Option<&str>> = thread
            .messages
            .iter()
            .map(|message| message.author.as_deref())
            .collect();
        assert_eq!(authors, [Some("rsilva"), Some("jlopes")]);
        // Same address, two people, two colours.
        assert_eq!(thread.messages[0].participant, 0);
        assert_eq!(thread.messages[1].participant, 1);
    }

    #[test]
    fn a_person_quoting_a_ticket_stays_the_author() {
        let thread = thread(
            "## Marta Reis <marta@example.com> — 2026-11-04 08:55\n\n\
             About (by rsilva on 03/02/26 09:10am): it's fixed.\n",
        );
        assert_eq!(thread.messages[0].author, None);
    }

    #[test]
    fn prose_about_someone_is_not_a_ticket_line() {
        for line in [
            "This was fixed (by the team on Friday).",
            "(by ana) and then on 3 March (see below)",
        ] {
            let thread = parse_thread(line, Some("helpdesk@example.com"));
            assert_eq!(thread.messages[0].author, None, "{line}");
        }
    }

    #[test]
    fn what_a_reply_only_repeats_folds_away() {
        let thread = ticket_thread();
        let reply = &thread.messages[1];
        // The subject line joins the repeated comment rather than opening the
        // card; only the new comment is left to read.
        assert_eq!(
            text(reply),
            "Comment #2 (by jlopes on 03/02/26 10:25am):\n\nThe print server is down again."
        );
        assert_eq!(folds(reply), [(FoldKind::Repeated, 4)]);
    }

    #[test]
    fn nothing_is_folded_in_the_first_message() {
        let thread = ticket_thread();
        assert_eq!(folds(&thread.messages[0]), []);
    }

    #[test]
    fn a_short_repeat_stays_where_it_is() {
        let thread = thread(
            "## Marta <marta@example.com> — 2026-11-04 08:55\n\nThe fix is out.\n\nThanks,\n\n\
             ## Ana <ana@example.com> — 2026-11-04 09:00\n\nIt works here.\n\nThanks,\n",
        );
        assert_eq!(folds(&thread.messages[1]), []);
        assert_eq!(text(&thread.messages[1]), "It works here.\n\nThanks,");
    }

    #[test]
    fn fields_show_what_changed_and_fold_the_rest() {
        let thread = ticket_thread();
        let EmailBlock::Fields { shown, unchanged } = thread.messages[0]
            .blocks
            .iter()
            .find(|block| matches!(block, EmailBlock::Fields { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(shown.len(), 4);
        assert_eq!(unchanged.len(), 0);

        let EmailBlock::Fields { shown, unchanged } = thread.messages[1]
            .blocks
            .iter()
            .find(|block| matches!(block, EmailBlock::Fields { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            shown,
            &[EmailField {
                key: "Status".to_string(),
                value: "IN PROGRESS".to_string(),
                previous: Some("OPEN".to_string()),
                link: None,
            }]
        );
        assert_eq!(unchanged.len(), 3);
    }

    #[test]
    fn prose_and_logs_are_not_fields() {
        for body in [
            "Note: the farm is back.\nThanks: Ana\n",
            "Error: first\nError: second\nError: third\n",
            "at 10:30\nhttps://example.com\nsee https://example.com\n",
        ] {
            let thread = thread(body);
            assert!(
                matches!(thread.messages[0].blocks[..], [EmailBlock::Text(_)]),
                "{body}"
            );
        }
    }

    fn quieted(body: &str, title: &str) -> Vec<EmailBlock> {
        let mut thread = thread(body);
        quiet_system_lines(&mut thread, title);
        thread.messages.remove(0).blocks
    }

    #[test]
    fn a_ticket_reply_keeps_only_the_persons_words_at_full_strength() {
        let blocks = quieted(
            "## helpdesk@example.com — 2026-11-06 09:38\n\n\
             This ticket is now closed. Please fill out this survey.\n\n\
             Ticket 42:The farm stopped taking jobs\n\n\
             Comment #4 (by ana on 11/06/26 08:38am):\n\n\
             Glad the publishes succeeded!\n\n\
             Changing Status to CLOSED\n",
            "HD 42: The farm stopped taking jobs",
        );
        assert_eq!(
            blocks,
            [
                EmailBlock::Fold {
                    kind: FoldKind::System,
                    text: "This ticket is now closed. Please fill out this survey.\n\n\
                           Ticket 42:The farm stopped taking jobs"
                        .to_string(),
                    lines: 2,
                },
                EmailBlock::Text("Glad the publishes succeeded!".to_string()),
                EmailBlock::Meta("Changing Status to CLOSED".to_string()),
            ]
        );
    }

    #[test]
    fn a_card_opens_on_the_words_and_the_status_row_says_the_change() {
        let mut thread = thread(
            "## helpdesk@example.com — 2026-11-05 09:38\n\n\
             Description\n(by bo on 11/05/26 08:38am):\n\n\
             Looking into it.\n\n\
             Status:  OPEN\n\nTeam:  ops\n\nHost:  a1\n\n\
             ## helpdesk@example.com — 2026-11-06 09:38\n\n\
             This ticket is now closed.\n\n\
             Ticket 42:The farm stopped taking jobs\n\n\
             Comment #4 (by ana on 11/06/26 08:38am):\n\n\
             Glad the publishes succeeded!\n\n\
             Changing Status to CLOSED\n\n\
             Changing Team to infra\n\n\
             Status:  CLOSED\n\nTeam:  ops\n\nHost:  a1\n",
        );
        quiet_system_lines(&mut thread, "HD 42: The farm stopped taking jobs");
        assert_eq!(
            thread.messages[0].blocks[0],
            EmailBlock::Text("Looking into it.".to_string())
        );
        let blocks = &thread.messages[1].blocks;
        assert!(
            matches!(
                &blocks[0],
                EmailBlock::Fold { kind: FoldKind::System, lines: 2, text }
                    if text.starts_with("This ticket is now closed.")
            ),
            "{blocks:?}"
        );
        assert_eq!(
            blocks[1],
            EmailBlock::Text("Glad the publishes succeeded!".to_string())
        );
        // The Status row says the first; nothing on screen says the second.
        assert_eq!(
            blocks[2],
            EmailBlock::Meta("Changing Team to infra".to_string())
        );
    }

    #[test]
    fn a_change_a_folded_record_holds_stays_on_screen() {
        let blocks = quieted(
            "## helpdesk@example.com — 2026-11-06 09:38\n\n\
             Comment #4 (by ana on 11/06/26 08:38am):\n\n\
             Done.\n\n\
             Changing Status to CLOSED\n\n\
             Status:  CLOSED\n\nTeam:  ops\n\nHost:  a1\n",
            "Anything long enough",
        );
        assert!(
            blocks.contains(&EmailBlock::Meta("Changing Status to CLOSED".to_string())),
            "{blocks:?}"
        );
    }

    #[test]
    fn a_line_above_the_by_line_is_only_its_label_when_short() {
        let lines = [
            "Please see the long note below",
            "(by ana on 11/06/26 12:55pm):",
        ];
        assert_eq!(by_line_start(&lines, 1), 1);
        let lines = ["Description", "(by ana on 11/06/26 12:55pm):"];
        assert_eq!(by_line_start(&lines, 1), 0);
        let lines = ["Comment #2 (by ana on 11/06/26 12:55pm):"];
        assert_eq!(by_line_start(&lines, 0), 0);
    }

    #[test]
    fn a_card_drops_the_author_line_its_header_already_gives() {
        let mut thread = thread(
            "## helpdesk@example.com — 2026-11-05 09:38\n\n\
             Comment #3 (by bo on 11/05/26 08:38am):\n\n\
             Looking into it.\n\n\
             ## helpdesk@example.com — 2026-11-06 09:38\n\n\
             Comment #4 (by ana on 11/06/26 08:38am):\n\n\
             Glad the publishes succeeded!\n",
        );
        quiet_system_lines(&mut thread, "HD 42: The farm stopped taking jobs");
        assert_eq!(thread.messages[1].author.as_deref(), Some("ana"));
        assert_eq!(
            thread.messages[1].blocks,
            [EmailBlock::Text(
                "Glad the publishes succeeded!".to_string()
            )]
        );
    }

    #[test]
    fn words_sharing_a_paragraph_with_the_author_line_stay_full_strength() {
        let blocks = quieted(
            "## helpdesk@example.com — 2026-11-06 09:38\n\n\
             Description\n(by ana on 11/06/26 12:55pm):\nThe farm is down.\n",
            "Anything long enough",
        );
        assert_eq!(blocks, [EmailBlock::Text("The farm is down.".to_string())]);
    }

    #[test]
    fn a_repeated_import_note_parts_from_the_comments_around_it() {
        let mut thread = EmailThread {
            preamble: String::new(),
            messages: vec![EmailMessage {
                from: "helpdesk@example.com".to_string(),
                author: Some("ana".to_string()),
                sent: None,
                participant: 0,
                blocks: vec![EmailBlock::Fold {
                    kind: FoldKind::Repeated,
                    text: "The farm is down.\nStill.\n\n\
                           This ticket was imported from Freshservice:\n\
                           https://example.com/tickets/1\n\n\
                           Thanks"
                        .to_string(),
                    lines: 5,
                }],
            }],
        };
        quiet_system_lines(&mut thread, "Anything long enough");
        assert_eq!(
            thread.messages[0].blocks,
            [
                EmailBlock::Fold {
                    kind: FoldKind::Repeated,
                    text: "The farm is down.\nStill.".to_string(),
                    lines: 2,
                },
                EmailBlock::Fold {
                    kind: FoldKind::System,
                    text: "This ticket was imported from Freshservice:\n\
                           https://example.com/tickets/1"
                        .to_string(),
                    lines: 2,
                },
                EmailBlock::Fold {
                    kind: FoldKind::Repeated,
                    text: "Thanks".to_string(),
                    lines: 1,
                },
            ]
        );
    }

    #[test]
    fn a_person_writing_directly_has_no_system_lines() {
        let body = "## Ana <ana@example.com> — 2026-11-06 09:38\n\n\
                    Changing Status to CLOSED\n\nThanks!\n";
        let blocks = quieted(body, "Anything long enough");
        assert!(matches!(blocks[..], [EmailBlock::Text(_)]), "{blocks:?}");
    }

    #[test]
    fn a_snippet_skips_the_system_lines() {
        let mut thread = thread(
            "## helpdesk@example.com — 2026-11-06 09:38\n\n\
             This ticket is now closed.\n\n\
             Comment #4 (by ana on 11/06/26 08:38am):\n\n\
             Glad it worked.\n",
        );
        quiet_system_lines(&mut thread, "Anything long enough");
        assert_eq!(
            snippet(&thread.messages[0]).as_deref(),
            Some("Glad it worked.")
        );
    }

    #[test]
    fn a_trailing_id_leaves_the_subject() {
        assert_eq!(
            trailing_ticket("mxfs not working | FS-596203"),
            Some(("mxfs not working", "FS-596203"))
        );
        assert_eq!(trailing_ticket("Pipes | and more"), None);
        assert_eq!(trailing_ticket("mxfs not working"), None);
        assert_eq!(trailing_ticket(" | FS-1"), None);
    }

    #[test]
    fn an_import_note_folds_and_words_about_imports_stay() {
        let body = "## helpdesk@example.com — 2026-11-06 09:38\n\nDescription\n(by ana on 03/02/26 10:25am):\n\nThe farm is down.\n\nThis ticket was imported from Freshservice:\nhttps://example.com/tickets/1\n\nThis ticket was imported from a mess, and\nnobody knows how\nor why.";
        let blocks = quieted(body, "HD 42: The farm stopped taking jobs");
        assert!(matches!(
            &blocks[..],
            [
                EmailBlock::Text(words),
                EmailBlock::Fold { kind: FoldKind::System, lines: 2, .. },
                EmailBlock::Text(rest),
            ] if words == "The farm is down." && rest.ends_with("or why.")
        ));
    }

    #[test]
    fn a_ticket_prefix_leaves_the_subject() {
        for (title, expected) in [
            (
                "HD 520596: mxfs not working",
                Some(("HD 520596", "mxfs not working")),
            ),
            ("INC-42: Disk full", Some(("INC-42", "Disk full"))),
            ("Ticket 7: Printer", Some(("Ticket 7", "Printer"))),
            ("Re: Q4 planning doc", None),
            ("Q4: planning", None),
            ("Phase 2: planning", None),
            ("HD 42:", None),
            ("No prefix at all", None),
        ] {
            assert_eq!(ticket_prefix(title), expected, "{title}");
        }
    }

    #[test]
    fn a_log_stays_prose_past_its_repeated_key() {
        let thread = thread("Error: first\n\nWarning: second\n\nError: third\n\nInfo: fourth\n");
        assert!(
            matches!(thread.messages[0].blocks[..], [EmailBlock::Text(_)]),
            "{:?}",
            thread.messages[0].blocks
        );
    }

    #[test]
    fn a_changed_field_reads_its_old_link_by_address() {
        let wrapped = "https://urldefense.com/v3/__https://example.com/old__;Iw!!AC!x$";
        let thread = thread(&format!(
            "## helpdesk@example.com — 2026-11-04 08:55\n\n\
             Link: {wrapped}\nStatus: OPEN\nHost: N/A\n\n\
             ## helpdesk@example.com — 2026-11-04 09:13\n\n\
             Link: https://example.com/new\nStatus: OPEN\nHost: N/A\n"
        ));
        let EmailBlock::Fields { shown, .. } = &thread.messages[1].blocks[0] else {
            panic!("fields: {:?}", thread.messages[1].blocks);
        };
        assert_eq!(shown[0].previous.as_deref(), Some("example.com/old"));
    }

    #[test]
    fn a_field_keeps_its_link() {
        let thread = thread("ID: 42, https://example.com/tickets/42\nStatus: OPEN\nHost: N/A\n");
        let EmailBlock::Fields { shown, .. } = &thread.messages[0].blocks[0] else {
            panic!("fields: {:?}", thread.messages[0].blocks);
        };
        assert_eq!(
            shown[0].link.as_deref(),
            Some("https://example.com/tickets/42")
        );
    }

    #[test]
    fn a_reply_trail_folds_with_its_attribution() {
        let thread = thread(
            "It's done.\n\nOn Wed, Oct 7, 2026 at 12:00 PM Ana <ana@example.com> wrote:\n\
             > Is it done?\n>\n> Thanks\n",
        );
        assert_eq!(text(&thread.messages[0]), "It's done.");
        assert_eq!(folds(&thread.messages[0]), [(FoldKind::Quoted, 4)]);
    }

    #[test]
    fn an_attribution_split_over_two_lines_folds_whole() {
        let thread = thread(
            "It's done.\n\nOn Wed, Oct 7, 2026 at 12:00 PM Ana\n<ana@example.com> wrote:\n\n\
             > Is it done?\n",
        );
        assert_eq!(text(&thread.messages[0]), "It's done.");
        assert_eq!(folds(&thread.messages[0]), [(FoldKind::Quoted, 3)]);
    }

    #[test]
    fn a_sentence_above_the_attribution_stays() {
        let thread = thread("On Monday I'll ship it.\nAs Ana wrote:\n> Ship it\n");
        assert_eq!(text(&thread.messages[0]), "On Monday I'll ship it.");
        assert_eq!(folds(&thread.messages[0]), [(FoldKind::Quoted, 2)]);
    }

    #[test]
    fn an_inline_reply_keeps_its_quotes() {
        let body = "> Is it done?\n\nYes.\n\n> And the docs?\n\nThose too.\n";
        let thread = thread(body);
        assert_eq!(folds(&thread.messages[0]), []);
        assert_eq!(text(&thread.messages[0]), body.trim());
    }

    #[test]
    fn a_forwarded_header_starts_the_trail() {
        let thread = thread(
            "See below.\n\nFrom: Ana <ana@example.com>\nSent: Monday\nTo: Marta\n\nThe original.\n",
        );
        assert_eq!(text(&thread.messages[0]), "See below.");
        assert_eq!(folds(&thread.messages[0]), [(FoldKind::Quoted, 4)]);
    }

    #[test]
    fn a_signature_folds_and_a_heading_underline_does_not() {
        let signed = thread("See you.\n\n-- \nAna\nStudio Lead\n");
        assert_eq!(text(&signed.messages[0]), "See you.");
        assert_eq!(folds(&signed.messages[0]), [(FoldKind::Signature, 3)]);

        let heading = thread("Title\n--\nBody.\n");
        assert_eq!(folds(&heading.messages[0]), []);
    }

    #[test]
    fn a_signature_then_a_trail_are_two_folds() {
        let thread = thread("Done.\n\n-- \nAna\n\nOn Monday Marta wrote:\n> Done?\n");
        assert_eq!(
            folds(&thread.messages[0]),
            [(FoldKind::Signature, 2), (FoldKind::Quoted, 2)]
        );
    }

    #[test]
    fn a_code_fence_stays_whole() {
        let body = "```\nError: one\n\nStatus: two\n\nHost: three\n```\n";
        let thread = thread(body);
        assert_eq!(text(&thread.messages[0]), body.trim());
    }

    #[test]
    fn a_fence_keeps_what_looks_like_a_tail() {
        for body in [
            "```\nargs\n\n--\nmore\n```\n",
            "```\nFrom: a\nSent: b\n```\n",
            "```\n-----Original Message-----\n```\n",
        ] {
            let thread = thread(body);
            assert_eq!(folds(&thread.messages[0]), [], "{body}");
            assert_eq!(text(&thread.messages[0]), body.trim());
        }
    }

    #[test]
    fn long_links_read_as_their_address_and_keep_their_target() {
        let wrapped =
            "https://urldefense.com/v3/__https://github.com/acme/repo/pull/36__;Iw!!AC!x$";
        assert_eq!(
            compact_links(&format!("See {wrapped}.")),
            format!("See [github.com/acme/repo/pull/36]({wrapped}).")
        );
        let long = format!("https://example.com/{}", "a".repeat(80));
        let compacted = compact_links(&long);
        assert!(compacted.starts_with("[example.com/aaa"), "{compacted}");
        assert!(compacted.contains("…]("), "{compacted}");
        assert!(compacted.ends_with(&format!("({long})")), "{compacted}");

        let underscored = format!("https://example.com/a_b_{}", "c".repeat(60));
        assert!(compact_links(&underscored).starts_with("[example.com/a\\_b\\_c"));
    }

    #[test]
    fn short_links_and_links_in_code_are_left_alone() {
        let long = format!("https://example.com/{}", "a".repeat(80));
        for text in [
            "See https://example.com/a for more.".to_string(),
            "[named](https://example.com/a)".to_string(),
            "```\nhttps://urldefense.com/v3/__https://example.com/a__;Iw!!x$\n```".to_string(),
            format!("Run `{long}` first."),
            format!("Run `curl {long} -o out` first."),
        ] {
            assert_eq!(compact_links(&text), text);
        }
    }

    #[test]
    fn a_cut_subject_is_completed_from_the_body() {
        let body = "## helpdesk@example.com — 2026-10-13 17:25\n\n\
                    Ticket 42:Backups not restarted when the disk is full | FS-1\n\nHello.\n";
        assert_eq!(
            full_title("HD 42: Backups not restarted when t...", body).as_deref(),
            Some("HD 42: Backups not restarted when the disk is full | FS-1")
        );
        assert_eq!(
            full_title("Backups not restarted when t…", body).as_deref(),
            Some("Backups not restarted when the disk is full | FS-1")
        );
    }

    #[test]
    fn a_subject_stays_when_the_body_cannot_complete_it() {
        let body = "Backups not restarted when t...\n";
        for title in [
            "Backups not restarted when t",
            "Nothing like the body at all...",
            "Short...",
            "Backups not restarted when t...",
        ] {
            assert_eq!(full_title(title, body), None, "{title}");
        }
    }

    #[test]
    fn a_line_restating_the_subject_leaves_the_reading() {
        let mut restated = ticket_thread();
        drop_restated_subject(&mut restated, "HD 42: The render farm is down");
        assert!(text(&restated.messages[0]).starts_with("Comment #1"));

        let mut unrelated = ticket_thread();
        drop_restated_subject(&mut unrelated, "HD 42: Something else entirely");
        assert_eq!(unrelated, ticket_thread());
    }

    #[test]
    fn a_short_subject_is_not_taken_for_a_restatement() {
        let mut short = thread("Done: yes\n\nThe rest of it.");
        let before = short.clone();
        drop_restated_subject(&mut short, "Note: yes");
        assert_eq!(short, before);
    }

    #[test]
    fn a_message_that_was_only_the_subject_has_no_text_left() {
        let mut only = thread("The render farm is down\n");
        drop_restated_subject(&mut only, "HD 42: The render farm is down");
        assert!(only.messages[0].blocks.is_empty());
    }

    #[test]
    fn a_snippet_skips_the_ticket_line() {
        let thread = ticket_thread();
        assert_eq!(
            snippet(&thread.messages[1]).as_deref(),
            Some("The print server is down again.")
        );
        let empty = EmailMessage {
            blocks: Vec::new(),
            ..thread.messages[0].clone()
        };
        assert_eq!(snippet(&empty), None);
    }

    fn field(key: &str, value: &str, previous: Option<&str>) -> EmailField {
        EmailField {
            key: key.to_string(),
            value: value.to_string(),
            previous: previous.map(str::to_string),
            link: None,
        }
    }

    fn keys(fields: &[&EmailField]) -> Vec<String> {
        fields.iter().map(|field| field.key.clone()).collect()
    }

    #[test]
    fn a_new_record_folds_whole_without_its_empty_values() {
        let shown = [
            field("Status", "OPEN", None),
            field("Host", "N/A", None),
            field("Logs", "", None),
            field("Phone", "—", None),
        ];
        let (visible, folded) = split_fields(&shown, &[]);
        assert!(visible.is_empty());
        assert_eq!(keys(&folded), ["Status"]);
    }

    #[test]
    fn a_later_record_keeps_what_changed_on_screen() {
        let shown = [
            field("Status", "IN PROGRESS", Some("OPEN")),
            field("Owner", "N/A", Some("ana")),
            field("Resolution", "Fixed", None),
            field("Host", "N/A", None),
        ];
        let unchanged = [
            field("Team", "render-ops", None),
            field("Phone", "n/a", None),
        ];
        let (visible, folded) = split_fields(&shown, &unchanged);
        assert_eq!(keys(&visible), ["Status", "Owner", "Resolution"]);
        assert_eq!(keys(&folded), ["Team"]);
    }

    #[test]
    fn initials_come_from_the_first_and_last_words() {
        assert_eq!(initials("Marta Reis"), "MR");
        assert_eq!(initials("Ana (Mia) Lima"), "AL");
        assert_eq!(initials("Copilot"), "C");
        assert_eq!(initials("help_desk@example.com"), "HD");
        assert_eq!(initials(""), "");
    }
}
