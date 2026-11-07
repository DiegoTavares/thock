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

/// Where the trail of a reply starts, when the message has one. A `>` run with
/// the sender's words after it is an inline reply, and those stay.
fn quote_start(lines: &[&str]) -> Option<usize> {
    let forwarded = lines.iter().enumerate().position(|(index, line)| {
        let line = line.trim();
        (line.starts_with("-----") && line.contains("Original Message"))
            || (line.starts_with("From: ")
                && lines
                    .get(index + 1)
                    .is_some_and(|next| next.trim().starts_with("Sent: ")))
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
                let wrapped = last > 0 && lines[last - 1].trim_start().starts_with("On ");
                if wrapped { last - 1 } else { last }
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
    let signature = (0..lines.len()).find(|&index| is_signature_rule(lines, index));
    let quote = quote_start(lines);
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

/// The `Key: value` lines from the start of `paragraphs`, and how many
/// paragraphs they took, when they read as a record. Repeated keys are a log,
/// not a record.
fn field_run(paragraphs: &[Vec<&str>]) -> Option<(Vec<(String, String)>, usize)> {
    let run: Vec<Vec<(String, String)>> = paragraphs
        .iter()
        .map_while(|paragraph| parse_fields(paragraph))
        .collect();
    let taken = run.len();
    let fields: Vec<(String, String)> = run.into_iter().flatten().collect();
    let keys: HashSet<&str> = fields.iter().map(|(key, _)| key.as_str()).collect();
    (fields.len() >= MIN_FIELDS && keys.len() == fields.len()).then_some((fields, taken))
}

/// `(by ana on 03/02/26 10:25am):` → `ana`.
fn parse_author(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once("(by ")?;
    let (author, details) = rest.split_once(" on ")?;
    let author = author.trim();
    let shaped = !author.is_empty() && author.len() <= MAX_AUTHOR_LENGTH;
    (shaped && details.contains(')')).then_some(author)
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
    if let (Some(EmailBlock::Text(last)), EmailBlock::Text(added)) = (blocks.last_mut(), &block) {
        last.push_str("\n\n");
        last.push_str(added);
    } else {
        blocks.push(block);
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
                previous: previous.filter(|_| !repeated),
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
        while let Some(paragraph) = paragraphs.get(index) {
            if let Some((fields, taken)) = field_run(&paragraphs[index..]) {
                parts.push(Part::Fields(fields));
                index += taken;
                continue;
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
    line.split_inclusive(char::is_whitespace)
        .map(|token| {
            let word = token.trim_end();
            let (link, punctuation) = split_link(word);
            if shortens(link) {
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
        for text in [
            "See https://example.com/a for more.",
            "[named](https://example.com/a)",
            "```\nhttps://urldefense.com/v3/__https://example.com/a__;Iw!!x$\n```",
        ] {
            assert_eq!(compact_links(text), text);
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
    fn initials_come_from_the_first_and_last_words() {
        assert_eq!(initials("Marta Reis"), "MR");
        assert_eq!(initials("Ana (Mia) Lima"), "AL");
        assert_eq!(initials("Copilot"), "C");
        assert_eq!(initials("help_desk@example.com"), "HD");
        assert_eq!(initials(""), "");
    }
}
