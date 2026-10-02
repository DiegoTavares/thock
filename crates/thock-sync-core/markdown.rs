//! The note model the rules work on: lines with their own terminators, ATX
//! headings outside fences and front matter, and the section geometry of
//! spec §7.2. Also the three hashes of §7.4.

use sha2::{Digest, Sha256};

use crate::write::Heading;

/// How a line ended in the source. Kept per line so untouched lines are
/// written back byte for byte (spec §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    Lf,
    CrLf,
    None,
}

impl Ending {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::None => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub text: String,
    pub ending: Ending,
}

impl Line {
    pub(crate) fn new(text: impl Into<String>, ending: Ending) -> Self {
        Self {
            text: text.into(),
            ending,
        }
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// A note split into lines. `split` and `join` round-trip every input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Document {
    pub lines: Vec<Line>,
    /// The terminator inserted lines use (spec §8.1).
    pub ending: Ending,
}

impl Document {
    pub(crate) fn split(text: &str) -> Self {
        let mut lines = Vec::new();
        let mut crlf = 0usize;
        let mut lf = 0usize;
        let mut rest = text;
        while !rest.is_empty() {
            match rest.find('\n') {
                Some(index) => {
                    let (line, after) = rest.split_at(index);
                    let (content, ending) = match line.strip_suffix('\r') {
                        Some(content) => {
                            crlf += 1;
                            (content, Ending::CrLf)
                        }
                        None => {
                            lf += 1;
                            (line, Ending::Lf)
                        }
                    };
                    lines.push(Line::new(content, ending));
                    rest = after.get(1..).unwrap_or("");
                }
                None => {
                    lines.push(Line::new(rest, Ending::None));
                    rest = "";
                }
            }
        }
        let ending = if crlf > lf { Ending::CrLf } else { Ending::Lf };
        Self { lines, ending }
    }

    pub(crate) fn empty() -> Self {
        Self {
            lines: Vec::new(),
            ending: Ending::Lf,
        }
    }

    pub(crate) fn join(&self) -> String {
        let mut text = String::new();
        for line in &self.lines {
            text.push_str(&line.text);
            text.push_str(line.ending.as_str());
        }
        text
    }

    /// Inserts `texts` as new lines before index `at`. A last line that had
    /// no terminator gets one when something is inserted after it, and the
    /// inserted lines then end the file the same way (spec §8.1).
    pub(crate) fn insert(&mut self, at: usize, texts: &[String]) {
        if texts.is_empty() {
            return;
        }
        let at = at.min(self.lines.len());
        let at_end = at == self.lines.len();
        let unterminated_tail = at_end
            && self
                .lines
                .last()
                .is_some_and(|line| line.ending == Ending::None);
        if unterminated_tail && let Some(last) = self.lines.last_mut() {
            last.ending = self.ending;
        }
        let ending = self.ending;
        let new_lines: Vec<Line> = texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let is_last = at_end && unterminated_tail && index + 1 == texts.len();
                Line::new(text.clone(), if is_last { Ending::None } else { ending })
            })
            .collect();
        self.lines.splice(at..at, new_lines);
    }

    /// Indices of the lines that are headings, with their level and text,
    /// skipping fenced code and a leading front-matter block (spec §7.2).
    pub(crate) fn headings(&self) -> Vec<HeadingLine> {
        self.scan().0
    }

    /// Per line, whether it sits inside a fence or the front matter, where
    /// no rule may match it (spec §7.2). Fence delimiters count as inside.
    pub(crate) fn protected(&self) -> Vec<bool> {
        self.scan().1
    }

    fn scan(&self) -> (Vec<HeadingLine>, Vec<bool>) {
        let mut found = Vec::new();
        let mut protected = vec![false; self.lines.len()];
        let mut fence: Option<(char, usize)> = None;
        let mut in_front_matter = false;
        for (index, line) in self.lines.iter().enumerate() {
            let text = line.text.as_str();
            if index == 0 && text == "---" && front_matter_closes(&self.lines) {
                in_front_matter = true;
                protected[index] = true;
                continue;
            }
            if in_front_matter {
                protected[index] = true;
                if text == "---" || text == "..." {
                    in_front_matter = false;
                }
                continue;
            }
            if let Some((character, length)) = fence {
                protected[index] = true;
                if closes_fence(text, character, length) {
                    fence = None;
                }
                continue;
            }
            if let Some(opened) = opens_fence(text) {
                fence = Some(opened);
                protected[index] = true;
                continue;
            }
            if let Some((level, heading_text)) = parse_heading(text) {
                found.push(HeadingLine {
                    index,
                    level,
                    text: heading_text.to_string(),
                });
            }
        }
        (found, protected)
    }

    /// The heading a write names (spec §7.2): exact lowercase equality first,
    /// then key equality, `ordinal` among the matches of the winning pass.
    pub(crate) fn resolve(&self, heading: &Heading) -> Option<HeadingLine> {
        let headings = self.headings();
        let wanted = heading.text.trim().to_lowercase();
        let exact: Vec<&HeadingLine> = headings
            .iter()
            .filter(|candidate| candidate.text.trim().to_lowercase() == wanted)
            .collect();
        let matches = if exact.is_empty() {
            let wanted_key = heading_key(&heading.text);
            if wanted_key.is_empty() {
                return None;
            }
            headings
                .iter()
                .filter(|candidate| heading_key(&candidate.text) == wanted_key)
                .collect()
        } else {
            exact
        };
        matches
            .get(heading.ordinal)
            .or_else(|| matches.last())
            .map(|found| (*found).clone())
    }

    /// The geometry of the section under `heading` (spec §7.2).
    pub(crate) fn section(&self, heading: &HeadingLine) -> Section {
        let headings = self.headings();
        let start = heading.index + 1;
        let end = headings
            .iter()
            .find(|other| other.index > heading.index && other.level <= heading.level)
            .map(|other| other.index)
            .unwrap_or(self.lines.len());
        let body_end = self.trim_blank_end(start, end);
        let own_end = headings
            .iter()
            .find(|other| other.index > heading.index && other.index < body_end)
            .map(|other| other.index)
            .unwrap_or(body_end);
        Section {
            heading: Some(heading.index),
            start,
            body_end,
            own_end: self.trim_blank_end(start, own_end),
            end,
        }
    }

    /// The whole file as a section, for writes whose heading is `null`.
    pub(crate) fn whole(&self) -> Section {
        let end = self.lines.len();
        let body_end = self.trim_blank_end(0, end);
        Section {
            heading: None,
            start: 0,
            body_end,
            own_end: body_end,
            end,
        }
    }

    /// Trailing blank lines are not body, and neither is a closing `___`
    /// rule: the shipped templates end every section with one, and an
    /// append must land above it to stay in its section.
    fn trim_blank_end(&self, start: usize, mut end: usize) -> usize {
        while end > start && self.lines.get(end - 1).is_some_and(Line::is_blank) {
            end -= 1;
        }
        if end > start
            && self
                .lines
                .get(end - 1)
                .is_some_and(|line| is_thematic_break(&line.text))
        {
            end -= 1;
            while end > start && self.lines.get(end - 1).is_some_and(Line::is_blank) {
                end -= 1;
            }
        }
        end
    }

    /// The body lines a line rule may match: `(index, text)` for every body
    /// line outside fences and front matter.
    pub(crate) fn matchable(&self, section: &Section) -> Vec<(usize, &str)> {
        let protected = self.protected();
        (section.start..section.body_end)
            .filter(|index| !protected.get(*index).copied().unwrap_or(false))
            .filter_map(|index| {
                self.lines
                    .get(index)
                    .map(|line| (index, line.text.as_str()))
            })
            .collect()
    }

    pub(crate) fn body_texts(&self, section: &Section) -> Vec<&str> {
        self.lines
            .get(section.start..section.body_end)
            .unwrap_or(&[])
            .iter()
            .map(|line| line.text.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadingLine {
    pub index: usize,
    pub level: u8,
    pub text: String,
}

/// Line indices: `start..end` is the section range, `start..body_end` the
/// body, `start..own_end` the own lines (spec §7.2). `end` excludes the
/// next heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Section {
    pub heading: Option<usize>,
    pub start: usize,
    pub body_end: usize,
    pub own_end: usize,
    pub end: usize,
}

/// `___`, `---` or `***` (three or more, spaces allowed between), indented
/// at most three spaces.
fn is_thematic_break(text: &str) -> bool {
    let trimmed = text.trim_start_matches(' ');
    if text.len() - trimmed.len() > 3 {
        return false;
    }
    let Some(marker) = trimmed
        .chars()
        .next()
        .filter(|c| matches!(c, '_' | '-' | '*'))
    else {
        return false;
    };
    let count = trimmed.chars().filter(|c| *c == marker).count();
    count >= 3 && trimmed.chars().all(|c| c == marker || c == ' ')
}

fn front_matter_closes(lines: &[Line]) -> bool {
    lines
        .iter()
        .skip(1)
        .any(|line| line.text == "---" || line.text == "...")
}

fn opens_fence(text: &str) -> Option<(char, usize)> {
    let indent = text.len() - text.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = text.get(indent..)?;
    let character = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let length = rest.chars().take_while(|c| *c == character).count();
    if length < 3 {
        return None;
    }
    let info = rest.get(length..)?;
    if character == '`' && info.contains('`') {
        return None;
    }
    Some((character, length))
}

fn closes_fence(text: &str, character: char, length: usize) -> bool {
    let trimmed = text.trim_start_matches(' ');
    if text.len() - trimmed.len() > 3 {
        return false;
    }
    let run = trimmed.chars().take_while(|c| *c == character).count();
    run >= length
        && trimmed
            .get(run..)
            .is_some_and(|rest| rest.trim().is_empty())
}

/// `^#{1,6}[ \t]+\S` → (level, text with the closing hashes and surrounding
/// whitespace removed).
fn parse_heading(text: &str) -> Option<(u8, &str)> {
    let level = text.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let after = text.get(level..)?;
    let content = after.trim_start_matches([' ', '\t']);
    if content.len() == after.len() || content.trim().is_empty() {
        return None;
    }
    let content = content.trim_end();
    let without_closing = content.trim_end_matches('#');
    let heading_text = if without_closing.len() < content.len() {
        let trimmed = without_closing.trim_end_matches([' ', '\t']);
        if trimmed.len() == without_closing.len() && !without_closing.is_empty() {
            content
        } else {
            trimmed
        }
    } else {
        content
    };
    if heading_text.is_empty() {
        return None;
    }
    Some((level as u8, heading_text))
}

/// A heading's comparison key (spec V26 §5.1): links reduced to their label,
/// every character that is not a letter or digit folded to one space,
/// lowercased, trimmed. `## 📅 **Day planner**:` and `## Day-planner` both
/// key as `day planner`.
pub fn heading_key(text: &str) -> String {
    let plain = reduce_links(text);
    let mut key = String::with_capacity(plain.len());
    for character in plain.chars() {
        if character.is_alphanumeric() {
            key.extend(character.to_lowercase());
        } else if !key.is_empty() && !key.ends_with(' ') {
            key.push(' ');
        }
    }
    let trimmed = key.trim_end();
    key.truncate(trimmed.len());
    key
}

/// `[[target]]`, `[[target|alias]]` and `[label](dest)` replaced by what
/// they display; images, embeds, and anything inside inline code or an HTML
/// comment are left alone.
fn reduce_links(text: &str) -> String {
    let excluded = inline_exclusions(text);
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut scan = 0;
    while let Some(relative) = text.get(scan..).and_then(|rest| rest.find('[')) {
        let open = scan + relative;
        if open > 0 && matches!(bytes.get(open - 1), Some(b'!') | Some(b'\\')) {
            scan = open + 1;
            continue;
        }
        if let Some(range) = excluded.iter().find(|range| range.contains(&open)) {
            scan = range.end;
            continue;
        }
        let parsed = if text.get(open..).is_some_and(|rest| rest.starts_with("[[")) {
            parse_wikilink(text, open)
        } else {
            parse_inline_link(text, open)
        };
        match parsed {
            Some((range, label))
                if !excluded
                    .iter()
                    .any(|other| other.start < range.end && range.start < other.end) =>
            {
                out.push_str(text.get(cursor..range.start).unwrap_or(""));
                out.push_str(text.get(label).unwrap_or(""));
                cursor = range.end;
                scan = range.end;
            }
            _ => scan = open + 1,
        }
    }
    out.push_str(text.get(cursor..).unwrap_or(""));
    out
}

type ByteRange = std::ops::Range<usize>;

fn parse_wikilink(text: &str, open: usize) -> Option<(ByteRange, ByteRange)> {
    let inner_start = open + 2;
    let close = text.get(inner_start..)?.find("]]")? + inner_start;
    let inner = text.get(inner_start..close)?;
    if inner.is_empty() || inner.contains('[') || inner.contains(']') {
        return None;
    }
    let label = match inner.split_once('|') {
        Some((target, alias)) => {
            if target.is_empty() || alias.is_empty() {
                return None;
            }
            inner_start + target.len() + 1..close
        }
        None => inner_start..close,
    };
    Some((open..close + 2, label))
}

fn parse_inline_link(text: &str, open: usize) -> Option<(ByteRange, ByteRange)> {
    let label_start = open + 1;
    let label_end = text.get(label_start..)?.find(']')? + label_start;
    let label = text.get(label_start..label_end)?;
    if label.is_empty() || label.contains('[') {
        return None;
    }
    if !text.get(label_end + 1..)?.starts_with('(') {
        return None;
    }
    let destination_start = label_end + 2;
    let mut depth = 1usize;
    for (offset, character) in text.get(destination_start..)?.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let close = destination_start + offset;
                    return Some((open..close + 1, label_start..label_end));
                }
            }
            _ => {}
        }
    }
    None
}

fn inline_exclusions(text: &str) -> Vec<ByteRange> {
    let mut ranges = code_spans(text);
    let mut cursor = 0;
    while let Some(open) = text.get(cursor..).and_then(|rest| rest.find("<!--")) {
        let open = cursor + open;
        let Some(close) = text
            .get(open + 4..)
            .and_then(|rest| rest.find("-->"))
            .map(|index| open + 4 + index + 3)
        else {
            break;
        };
        let range = open..close;
        if !ranges
            .iter()
            .any(|other| other.start < range.end && range.start < other.end)
        {
            ranges.push(range);
        }
        cursor = close;
    }
    ranges
}

/// CommonMark code spans: a backtick run closed by a run of the same length.
fn code_spans(text: &str) -> Vec<ByteRange> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = text.get(cursor..).and_then(|rest| rest.find('`')) {
        let open = cursor + relative;
        let length = text
            .get(open..)
            .map(|rest| rest.chars().take_while(|c| *c == '`').count())
            .unwrap_or(0);
        let mut search = open + length;
        let mut closed = None;
        while let Some(relative) = text.get(search..).and_then(|rest| rest.find('`')) {
            let candidate = search + relative;
            let run = text
                .get(candidate..)
                .map(|rest| rest.chars().take_while(|c| *c == '`').count())
                .unwrap_or(0);
            if run == length {
                closed = Some(candidate + run);
                break;
            }
            search = candidate + run;
        }
        match closed {
            Some(end) => {
                spans.push(open..end);
                cursor = end;
            }
            None => cursor = open + length,
        }
    }
    spans
}

/// The identity of a line for `replace_line` and `remove_line` (spec §7.4):
/// list marker, checkbox, time prefix and trailing comments stripped,
/// whitespace collapsed, then the first 16 hex of its SHA-256.
pub fn line_hash(line: &str) -> String {
    let normalized = normalize_line(line);
    short_hash(normalized.as_bytes())
}

pub(crate) fn normalize_line(line: &str) -> String {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut rest = line.trim_start();
    rest = strip_list_marker(rest);
    rest = strip_checkbox(rest);
    rest = strip_time_prefix(rest);
    let mut tail = rest.trim_end();
    while let Some(open) = tail.rfind("<!--") {
        if !tail.ends_with("-->") || open + 4 > tail.len() - 3 {
            break;
        }
        tail = tail.get(..open).unwrap_or("").trim_end();
    }
    let mut collapsed = String::with_capacity(tail.len());
    for word in tail.split_whitespace() {
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }
    collapsed
}

fn strip_list_marker(text: &str) -> &str {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = text.strip_prefix(marker) {
            return rest;
        }
    }
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits)
        && let Some(after_digits) = text.get(digits..)
        && let Some(rest) = after_digits
            .strip_prefix(". ")
            .or_else(|| after_digits.strip_prefix(") "))
    {
        return rest;
    }
    text
}

fn strip_checkbox(text: &str) -> &str {
    for checkbox in ["[ ] ", "[x] ", "[X] "] {
        if let Some(rest) = text.strip_prefix(checkbox) {
            return rest;
        }
    }
    text
}

/// The V4 time grammar: `H:MM`/`HH:MM`, optionally `–`/`—`/`-`/`to` and a
/// second time (`24:00` allowed as an end), followed by whitespace or the
/// end of the line. A "time" glued to its label is not one and stays.
fn strip_time_prefix(text: &str) -> &str {
    let Some(after_start) = parse_time(text) else {
        return text;
    };
    if let Some(after_end) = parse_range_end(after_start)
        && let Some(rest) = label_after_token(after_end)
    {
        return rest;
    }
    label_after_token(after_start).unwrap_or(text)
}

fn parse_time(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut hour_digits = 0;
    while hour_digits < 2
        && bytes
            .get(hour_digits)
            .is_some_and(|byte| byte.is_ascii_digit())
    {
        hour_digits += 1;
    }
    if hour_digits == 0 || bytes.get(hour_digits) != Some(&b':') {
        return None;
    }
    let minutes = text.get(hour_digits + 1..hour_digits + 3)?;
    if !minutes.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hours: u32 = text.get(..hour_digits)?.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    text.get(hour_digits + 3..)
}

fn parse_range_end(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    let after_separator = trimmed
        .strip_prefix('–')
        .or_else(|| trimmed.strip_prefix('—'))
        .or_else(|| trimmed.strip_prefix('-'))
        .or_else(|| trimmed.strip_prefix("to"))?
        .trim_start();
    if let Some(rest) = after_separator.strip_prefix("24:00") {
        return Some(rest);
    }
    parse_time(after_separator)
}

fn label_after_token(after_token: &str) -> Option<&str> {
    if after_token.is_empty() {
        return Some("");
    }
    let rest = after_token.trim_start();
    (rest.len() < after_token.len()).then_some(rest)
}

/// The hash `replace_section` compares against (spec §7.4): the body lines
/// joined with `\n`, no trailing newline, first 16 hex of SHA-256. `None`
/// when the heading is not in `content`.
pub fn section_hash(content: &str, heading: &Heading) -> Option<String> {
    let document = Document::split(content);
    let found = document.resolve(heading)?;
    let section = document.section(&found);
    Some(hash_lines(&document.body_texts(&section)))
}

pub(crate) fn hash_lines(lines: &[&str]) -> String {
    let joined = lines.join("\n");
    short_hash(joined.as_bytes())
}

fn short_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = hex::encode(digest);
    hex.truncate(16);
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_join_round_trip() {
        for text in [
            "",
            "a",
            "a\n",
            "a\r\nb\r\n",
            "a\nb",
            "\n\n",
            "mixed\r\nendings\nhere\r\n",
        ] {
            assert_eq!(Document::split(text).join(), text);
        }
    }

    #[test]
    fn dominant_ending() {
        assert_eq!(Document::split("a\r\nb\r\nc\n").ending, Ending::CrLf);
        assert_eq!(Document::split("a\r\nb\nc\n").ending, Ending::Lf);
        assert_eq!(Document::split("").ending, Ending::Lf);
    }

    #[test]
    fn insert_after_unterminated_tail() {
        let mut document = Document::split("a\nb");
        document.insert(2, &["c".to_string(), "d".to_string()]);
        assert_eq!(document.join(), "a\nb\nc\nd");
    }

    #[test]
    fn headings_skip_fences_and_front_matter() {
        let text = "---\ntitle: x\n# not a heading\n---\n# Title\n```\n## fenced\n```\n~~~md\n### tilde\n~~~\n## Real\n   ```\n## indented fence\n   ```\n#NoSpace\n####### seven\n## Closing ##\n";
        let document = Document::split(text);
        let headings: Vec<(u8, String)> = document
            .headings()
            .into_iter()
            .map(|heading| (heading.level, heading.text))
            .collect();
        assert_eq!(
            headings,
            vec![
                (1, "Title".to_string()),
                (2, "Real".to_string()),
                (2, "Closing".to_string())
            ]
        );
    }

    #[test]
    fn a_closing_rule_is_not_body() {
        let document = Document::split("## A\nx\n\n___\n\n## B\n- y\n---\n");
        let a = document.resolve(&Heading::new("A")).expect("A");
        let section = document.section(&a);
        assert_eq!((section.start, section.body_end, section.end), (1, 2, 5));
        let b = document.resolve(&Heading::new("B")).expect("B");
        assert_eq!(document.body_texts(&document.section(&b)), vec!["- y"]);
        assert!(is_thematic_break("  * * *"));
        assert!(!is_thematic_break("--"));
        assert!(!is_thematic_break("    ___"));
        assert!(!is_thematic_break("-_-"));
    }

    #[test]
    fn unclosed_front_matter_is_content() {
        let document = Document::split("---\n# Title\n");
        assert_eq!(document.headings().len(), 1);
    }

    #[test]
    fn section_geometry() {
        let text = "# T\n\n## A\nown\n\n### A1\nchild\n\n\n## B\nb\n";
        let document = Document::split(text);
        let a = document.resolve(&Heading::new("A")).expect("A exists");
        let section = document.section(&a);
        assert_eq!(section.start, 3);
        assert_eq!(section.own_end, 4);
        assert_eq!(section.body_end, 7);
        assert_eq!(section.end, 9);
    }

    #[test]
    fn resolve_exact_beats_key_and_ordinal_picks() {
        let text = "## Day-planner\n## Day planner\n## Notes\n## Notes\n";
        let document = Document::split(text);
        assert_eq!(
            document
                .resolve(&Heading::new("Day planner"))
                .map(|h| h.index),
            Some(1)
        );
        assert_eq!(
            document
                .resolve(&Heading::new("📅 Day planner"))
                .map(|h| h.index),
            Some(0)
        );
        let mut second = Heading::new("notes");
        second.ordinal = 1;
        assert_eq!(document.resolve(&second).map(|h| h.index), Some(3));
        second.ordinal = 9;
        assert_eq!(document.resolve(&second).map(|h| h.index), Some(3));
    }

    #[test]
    fn heading_key_matches_the_desk() {
        assert_eq!(heading_key("📅 **Day planner**:"), "day planner");
        assert_eq!(heading_key("[Day planner](plan.md)"), "day planner");
        assert_eq!(heading_key("[[Day planner]]"), "day planner");
        assert_eq!(heading_key("[[plan|Day planner]]"), "day planner");
        assert_eq!(heading_key("Day-planner"), "day planner");
        assert_eq!(heading_key("日次計画"), "日次計画");
        assert_eq!(heading_key("`code [x](y)` and [a](b)"), "code x y and a");
        assert_eq!(heading_key("![img](x.png)"), "img x png");
        assert_eq!(heading_key("***"), "");
    }

    #[test]
    fn normalize_line_strips_markers() {
        assert_eq!(
            normalize_line("  - [ ] 09:30 - 11:00 Deep  work \n"),
            "Deep work"
        );
        assert_eq!(
            normalize_line("- [x] 9:30–11:00 Deep work <!--gcal:abc-->"),
            "Deep work"
        );
        assert_eq!(
            normalize_line("* 09:00 to 24:00 Night <!--a--> <!--thock:also-->"),
            "Night"
        );
        assert_eq!(normalize_line("12. 12:30 Lunch"), "Lunch");
        assert_eq!(
            normalize_line("1234567890. ten digits"),
            "1234567890. ten digits"
        );
        assert_eq!(normalize_line("- 09:30abc glued"), "09:30abc glued");
        assert_eq!(normalize_line("25:00 not a time"), "25:00 not a time");
        assert_eq!(normalize_line("- [ ]"), "[ ]");
        assert_eq!(normalize_line("<!-- only a comment -->"), "");
        assert_eq!(normalize_line("text <!-- unclosed"), "text <!-- unclosed");
    }

    #[test]
    fn section_hash_of_body() {
        let text = "## A\nx\ny\n\n\n## B\n";
        assert_eq!(
            section_hash(text, &Heading::new("A")),
            Some(hash_lines(&["x", "y"]))
        );
        assert_eq!(
            section_hash(text, &Heading::new("B")),
            Some(hash_lines(&[]))
        );
        assert_eq!(section_hash(text, &Heading::new("C")), None);
    }
}
