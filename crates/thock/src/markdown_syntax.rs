//! Line-based Markdown scanner for the conceal feature (spec V10 §6). Pure
//! functions over strings — no GPUI, no tree-sitter — so every rule here is
//! unit-testable. Wikilinks are not in the Markdown grammar at all, and the
//! fence tracking needed to leave code blocks alone is a simple line state
//! machine; `markdown_text.rs` is the precedent for this shape of parser.

use std::ops::Range;

/// What a scanned span means for display. `Marker` and `Rule` become folds;
/// the label/heading kinds become text colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    /// Syntax characters hidden while the cursor is off the line.
    Marker,
    /// A `___` thematic-break line, drawn as a horizontal rule.
    Rule,
    /// ATX heading text, coloured by level (1–6).
    Heading(u8),
    /// A `[[wikilink]]` label — an internal link.
    WikilinkLabel,
    /// A `[text](dest)` label — an external link.
    LinkLabel,
    /// A task list item's `[ ]` / `[x]` marker, drawn as a checkbox.
    Checkbox(bool),
    /// The text between a pair of `~~` delimiters, drawn struck through.
    Strikethrough,
    /// An email note's machinery line (frontmatter fence or sync key),
    /// folded away entirely, trailing newline included (V16 §5.1).
    EmailHidden,
    /// An email note's `link:` frontmatter line, drawn as an "Open in …"
    /// label instead of the raw URL.
    EmailLink,
    /// The `## ` marker of a message header, drawn as a sender dot; `true`
    /// when the message is from the connected account.
    EmailMarker(bool),
    /// A sender name — in a message header or the envelope's `from:` value;
    /// `true` for the connected account's own messages.
    EmailSender(bool),
    /// Muted email chrome: a message header's ` — date` tail and the
    /// envelope's visible frontmatter keys.
    EmailDate,
    /// A quoted-history line (`> …` or its `… wrote:` attribution).
    EmailQuote,
}

/// A byte range of the scanned text and how it should display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcealSpan {
    pub range: Range<usize>,
    pub kind: SpanKind,
}

impl ConcealSpan {
    fn new(range: Range<usize>, kind: SpanKind) -> Self {
        Self { range, kind }
    }
}

/// Scans `text` for everything V10 conceals or colours. Spans are ordered by
/// line; `Marker` spans may sit inside a `Heading` span (a link in a heading)
/// but fold spans never overlap each other. A malformed construct produces no
/// spans at all — half-typed text renders exactly as typed (C3).
pub fn conceal_spans(text: &str) -> Vec<ConcealSpan> {
    let mut spans = Vec::new();
    each_content_line(text, |line_start, line| {
        if is_thematic_break(line) {
            spans.push(ConcealSpan::new(
                line_start..line_start + line.len(),
                SpanKind::Rule,
            ));
            return;
        }
        scan_line(line, line_start, &mut spans);
    });
    spans
}

/// The well-formed links on a single line, in order, with byte ranges into
/// that line. Honours the inline exclusions `conceal_spans` applies — nothing
/// inside inline code or an HTML comment, and no images or embeds — but not
/// the block ones, since fence and front-matter state belongs to a whole
/// document rather than to a lone line.
pub fn inline_links(line: &str) -> Vec<InlineLink> {
    let excluded = inline_exclusions(line);
    let mut links = Vec::new();
    each_inline_link(line, 0, &excluded, |link| {
        links.push(link);
        true
    });
    links
}

/// The `~~struck~~` runs on a single line, in order, with byte ranges into
/// that line. Honours the same inline exclusions as `inline_links`, and never
/// reads a `~~` that sits inside a link construct as a delimiter.
pub fn inline_strikethroughs(line: &str) -> Vec<InlineStrikethrough> {
    each_strikethrough(line, 0, &strikethrough_exclusions(line))
}

/// A `[[wikilink]]` located under a cursor: the full construct's byte range
/// and the target note's byte range (the part before the `|` of an alias).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikilinkReference {
    pub range: Range<usize>,
    pub target: Range<usize>,
}

/// The wikilink whose `[[...]]` construct contains byte `offset` — anywhere
/// on the brackets, target, or alias counts. Honours the same exclusions as
/// `conceal_spans`: nothing inside fences, front matter, or inline code, and
/// `![[embeds]]` don't count.
pub fn wikilink_at(text: &str, offset: usize) -> Option<WikilinkReference> {
    let mut found = None;
    each_content_line(text, |line_start, line| {
        if found.is_some() || offset < line_start || offset > line_start + line.len() {
            return;
        }
        let excluded = inline_exclusions(line);
        each_inline_link(line, 0, &excluded, |link| {
            let range = line_start + link.range.start..line_start + link.range.end;
            if range.contains(&offset) {
                if let Some(target) = link.wikilink_target {
                    found = Some(WikilinkReference {
                        target: line_start + target.start..line_start + target.end,
                        range,
                    });
                }
                return false;
            }
            // Links are ordered, so once one ends past the offset none of
            // the rest can contain it.
            range.end <= offset
        });
    });
    found
}

/// Calls `f` with `(line_start, line)` for every line outside fenced code
/// blocks and YAML front matter — the shared exclusion state machine (C1/C2)
/// behind `conceal_spans` and `wikilink_at`. `line` excludes any trailing
/// carriage return.
fn each_content_line(text: &str, mut f: impl FnMut(usize, &str)) {
    let mut fence: Option<(u8, usize)> = None;
    let mut front_matter = false;
    let mut offset = 0;

    for (index, raw_line) in text.split('\n').enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let line_start = offset;
        offset += raw_line.len() + 1;

        if index == 0 && line.trim_end() == "---" {
            front_matter = true;
            continue;
        }
        if front_matter {
            let trimmed = line.trim_end();
            if trimmed == "---" || trimmed == "..." {
                front_matter = false;
            }
            continue;
        }
        if let Some((marker, length)) = fence {
            if closes_fence(line, marker, length) {
                fence = None;
            }
            continue;
        }
        if let Some(opened) = opens_fence(line) {
            fence = Some(opened);
            continue;
        }
        f(line_start, line);
    }
}

/// The indent of a line that can still open a block construct (0–3 spaces),
/// or `None` when the line is indented too far.
fn block_indent(line: &str) -> Option<usize> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    (indent <= 3).then_some(indent)
}

/// Whether `line` opens a fenced code block, returning the fence character
/// and run length used to match the closing fence.
fn opens_fence(line: &str) -> Option<(u8, usize)> {
    let indent = block_indent(line)?;
    let rest = &line.as_bytes()[indent..];
    let marker = *rest.first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = rest.iter().take_while(|&&byte| byte == marker).count();
    if length < 3 {
        return None;
    }
    // An info string on a backtick fence may not contain backticks.
    if marker == b'`' && rest[length..].contains(&b'`') {
        return None;
    }
    Some((marker, length))
}

/// Whether `line` closes a fence opened with `marker` × `length`: the same
/// character, at least as long, and nothing else on the line.
fn closes_fence(line: &str, marker: u8, length: usize) -> bool {
    let Some(indent) = block_indent(line) else {
        return false;
    };
    let rest = &line.as_bytes()[indent..];
    let run = rest.iter().take_while(|&&byte| byte == marker).count();
    run >= length && rest[run..].iter().all(|byte| byte.is_ascii_whitespace())
}

/// `^ {0,3}_{3,}[ \t]*$` — the one thematic-break form V10 conceals. `---`
/// is ambiguous with front matter and setext underlines and `***` is rare,
/// so they stay visible (spec §3).
fn is_thematic_break(line: &str) -> bool {
    let Some(indent) = block_indent(line) else {
        return false;
    };
    let rest = &line.as_bytes()[indent..];
    let run = rest.iter().take_while(|&&byte| byte == b'_').count();
    run >= 3
        && rest[run..]
            .iter()
            .all(|&byte| byte == b' ' || byte == b'\t')
}

/// An ATX heading's `(level, marker_start, text_start)`, requiring at least
/// one space or tab after the hashes — `#tag` is not a heading (C3).
fn atx_heading(line: &str) -> Option<(u8, usize, usize)> {
    let indent = block_indent(line)?;
    let bytes = line.as_bytes();
    let mut cursor = indent;
    while cursor < bytes.len() && bytes[cursor] == b'#' {
        cursor += 1;
    }
    let level = cursor - indent;
    if level == 0 || level > 6 {
        return None;
    }
    let whitespace_start = cursor;
    while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'\t') {
        cursor += 1;
    }
    if cursor == whitespace_start {
        return None;
    }
    Some((level as u8, indent, cursor))
}

fn scan_line(line: &str, line_start: usize, spans: &mut Vec<ConcealSpan>) {
    let code_spans = code_span_ranges(line);
    let comments = html_comment_ranges(line, &code_spans);
    let mut inline_from = 0;

    if let Some((level, marker_start, text_start)) = atx_heading(line) {
        let text = line[text_start..].trim_end();
        // An empty heading has nothing left to show once the marker is
        // hidden, so the whole line is left alone (C3/C5).
        if text.is_empty() {
            return;
        }
        spans.push(ConcealSpan::new(
            line_start + marker_start..line_start + text_start,
            SpanKind::Marker,
        ));
        spans.push(ConcealSpan::new(
            line_start + text_start..line_start + text_start + text.len(),
            SpanKind::Heading(level),
        ));
        inline_from = text_start;
    } else if let Some((range, checked)) = task_checkbox(line) {
        spans.push(ConcealSpan::new(
            line_start + range.start..line_start + range.end,
            SpanKind::Checkbox(checked),
        ));
        inline_from = range.end;
    }

    for comment in &comments {
        spans.push(ConcealSpan::new(
            line_start + comment.start..line_start + comment.end,
            SpanKind::Marker,
        ));
    }

    let excluded: Vec<Range<usize>> = code_spans.into_iter().chain(comments).collect();
    let link_ranges = scan_inline(line, inline_from, line_start, &excluded, spans);

    // A `~~` inside a link construct belongs to its destination or label, not
    // to a strikethrough — and folding one would overlap the link's own folds.
    let struck_excluded: Vec<Range<usize>> = excluded.into_iter().chain(link_ranges).collect();
    for run in each_strikethrough(line, inline_from, &struck_excluded) {
        spans.push(ConcealSpan::new(
            line_start + run.range.start..line_start + run.text.start,
            SpanKind::Marker,
        ));
        spans.push(ConcealSpan::new(
            line_start + run.text.start..line_start + run.text.end,
            SpanKind::Strikethrough,
        ));
        spans.push(ConcealSpan::new(
            line_start + run.text.end..line_start + run.range.end,
            SpanKind::Marker,
        ));
    }
}

/// The ranges of inline code spans in `line`, delimiters included. Backtick
/// runs pair with the next run of the same length; an unmatched run is
/// literal text and scanning continues past it.
fn code_span_ranges(line: &str) -> Vec<Range<usize>> {
    let bytes = line.as_bytes();
    let mut runs = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'`' {
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor] == b'`' {
                cursor += 1;
            }
            runs.push((start, cursor - start));
        } else {
            cursor += 1;
        }
    }

    let mut ranges = Vec::new();
    let mut run_index = 0;
    while run_index < runs.len() {
        let (open_start, open_length) = runs[run_index];
        let closing = (run_index + 1..runs.len()).find(|&next| runs[next].1 == open_length);
        match closing {
            Some(close_index) => {
                let (close_start, close_length) = runs[close_index];
                ranges.push(open_start..close_start + close_length);
                run_index = close_index + 1;
            }
            None => run_index += 1,
        }
    }
    ranges
}

/// The line ranges no inline construct may overlap — inline code spans and
/// HTML comments — merged into one list.
fn inline_exclusions(line: &str) -> Vec<Range<usize>> {
    let code_spans = code_span_ranges(line);
    let comments = html_comment_ranges(line, &code_spans);
    code_spans.into_iter().chain(comments).collect()
}

/// The ranges of single-line `<!-- … -->` comments, delimiters included. A
/// comment that never closes on its line, or one inside inline code, is
/// left visible (C3) — as is a comment spanning several lines, since hiding
/// it would fold the newlines that separate the surrounding prose.
fn html_comment_ranges(line: &str, code_spans: &[Range<usize>]) -> Vec<Range<usize>> {
    const OPEN: &str = "<!--";
    const CLOSE: &str = "-->";
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while let Some(open) = line[cursor..].find(OPEN).map(|index| index + cursor) {
        let body = open + OPEN.len();
        let Some(close) = line[body..]
            .find(CLOSE)
            .map(|index| index + body + CLOSE.len())
        else {
            break;
        };
        let range = open..close;
        if !overlaps_excluded(code_spans, &range) {
            ranges.push(range);
        }
        cursor = close;
    }
    ranges
}

/// The `[ ]` / `[x]` marker of a task list item and whether it is checked.
/// A bullet must come first and a space or end of line must follow, so a
/// stray `[x]` in prose is left alone (C3). Any indent counts — nested tasks
/// are still tasks.
fn task_checkbox(line: &str) -> Option<(Range<usize>, bool)> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let after_bullet = line[indent..].strip_prefix(['-', '*', '+'])?;
    let after_space = after_bullet.trim_start_matches([' ', '\t']);
    if after_space.len() == after_bullet.len() {
        return None;
    }
    let mut characters = after_space.strip_prefix('[')?.chars();
    let checked = match characters.next()? {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None,
    };
    let after_checkbox = characters.as_str().strip_prefix(']')?;
    if !after_checkbox.is_empty() && !after_checkbox.starts_with([' ', '\t']) {
        return None;
    }
    let end = line.len() - after_checkbox.len();
    Some((end - "[ ]".len()..end, checked))
}

fn overlaps_excluded(excluded: &[Range<usize>], range: &Range<usize>) -> bool {
    excluded
        .iter()
        .any(|other| other.start < range.end && range.start < other.end)
}

/// Pushes the spans of every link on `line`, returning their line-relative
/// construct ranges.
fn scan_inline(
    line: &str,
    from: usize,
    line_start: usize,
    excluded: &[Range<usize>],
    spans: &mut Vec<ConcealSpan>,
) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    each_inline_link(line, from, excluded, |link| {
        let kind = if link.wikilink_target.is_some() {
            SpanKind::WikilinkLabel
        } else {
            SpanKind::LinkLabel
        };
        spans.push(ConcealSpan::new(
            line_start + link.range.start..line_start + link.label.start,
            SpanKind::Marker,
        ));
        spans.push(ConcealSpan::new(
            line_start + link.label.start..line_start + link.label.end,
            kind,
        ));
        spans.push(ConcealSpan::new(
            line_start + link.label.end..line_start + link.range.end,
            SpanKind::Marker,
        ));
        ranges.push(link.range);
        true
    });
    ranges
}

/// A well-formed link parsed from a single line, all ranges line-relative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineLink {
    /// The full construct: `[[...]]` or `[text](dest)`.
    pub range: Range<usize>,
    /// The displayed text — the wikilink target or alias, or the link label.
    pub label: Range<usize>,
    /// The note a `[[wikilink]]` points at; `None` for `[text](dest)` links.
    pub wikilink_target: Option<Range<usize>>,
    /// The destination of a `[text](dest)` link, which may carry a trailing
    /// title; `None` for wikilinks.
    pub destination: Option<Range<usize>>,
}

/// Walks the well-formed links of `line` from `from`, skipping images,
/// embeds, escapes, and anything overlapping an excluded range. `visit`
/// returns whether to keep walking.
fn each_inline_link(
    line: &str,
    from: usize,
    excluded: &[Range<usize>],
    mut visit: impl FnMut(InlineLink) -> bool,
) {
    let bytes = line.as_bytes();
    let mut cursor = from;
    while let Some(relative) = line[cursor..].find('[') {
        let open = cursor + relative;
        // `![alt](src)` is an image and `![[target]]` an embed — the `!`
        // disqualifies the match (C4). A backslash escape stays literal.
        if open > 0 && (bytes[open - 1] == b'!' || bytes[open - 1] == b'\\') {
            cursor = open + 1;
            continue;
        }
        if let Some(range) = excluded.iter().find(|range| range.contains(&open)) {
            cursor = range.end;
            continue;
        }

        let parsed = if line[open..].starts_with("[[") {
            parse_wikilink(line, open)
        } else {
            parse_inline_link(line, open)
        };
        match parsed {
            Some(link) if !overlaps_excluded(excluded, &link.range) => {
                cursor = link.range.end;
                if !visit(link) {
                    return;
                }
            }
            _ => cursor = open + 1,
        }
    }
}

/// Parses `[[target]]` or `[[target|alias]]` at `open`.
fn parse_wikilink(line: &str, open: usize) -> Option<InlineLink> {
    let inner_start = open + 2;
    let close = line[inner_start..].find("]]")? + inner_start;
    let inner = &line[inner_start..close];
    let end = close + 2;
    if inner.is_empty() || inner.contains('[') || inner.contains(']') {
        return None;
    }
    let (target, label) = match inner.split_once('|') {
        Some((target, alias)) => {
            if target.is_empty() || alias.is_empty() {
                return None;
            }
            let alias_start = inner_start + target.len() + 1;
            (inner_start..inner_start + target.len(), alias_start..close)
        }
        None => (inner_start..close, inner_start..close),
    };
    Some(InlineLink {
        range: open..end,
        label,
        wikilink_target: Some(target),
        destination: None,
    })
}

/// A `~~struck~~` run parsed from a single line, all ranges line-relative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineStrikethrough {
    /// The full construct, `~~` delimiters included.
    pub range: Range<usize>,
    /// The struck text between the delimiters.
    pub text: Range<usize>,
}

/// The ranges a `~~` delimiter may not sit inside: inline code, HTML
/// comments, and whole link constructs.
fn strikethrough_exclusions(line: &str) -> Vec<Range<usize>> {
    let inline = inline_exclusions(line);
    let mut excluded = inline.clone();
    each_inline_link(line, 0, &inline, |link| {
        excluded.push(link.range);
        true
    });
    excluded
}

/// Walks the `~~struck~~` runs of `line` from `from`. A run whose delimiters
/// overlap an excluded range is skipped without consuming its text, so a
/// later well-formed run on the same line is still found.
fn each_strikethrough(
    line: &str,
    from: usize,
    excluded: &[Range<usize>],
) -> Vec<InlineStrikethrough> {
    let bytes = line.as_bytes();
    let mut runs = Vec::new();
    let mut cursor = from;
    while let Some(relative) = line[cursor..].find("~~") {
        let open = cursor + relative;
        cursor = open + 2;
        // A longer tilde run opens a fence or is literal text, and a
        // backslash escapes the construct (C3/C4).
        if bytes.get(open + 2) == Some(&b'~')
            || (open > 0 && matches!(bytes[open - 1], b'~' | b'\\'))
        {
            continue;
        }
        let Some(run) = parse_strikethrough(line, open) else {
            continue;
        };
        let delimiters = [run.range.start..run.text.start, run.text.end..run.range.end];
        if delimiters
            .iter()
            .any(|delimiter| overlaps_excluded(excluded, delimiter))
        {
            continue;
        }
        cursor = run.range.end;
        runs.push(run);
    }
    runs
}

/// Parses `~~struck~~` at `open`. The text may not be empty or start or end
/// with whitespace, and the closing run must be exactly two tildes — anything
/// else stays literal (C3).
fn parse_strikethrough(line: &str, open: usize) -> Option<InlineStrikethrough> {
    let text_start = open + 2;
    let close = line[text_start..].find("~~")? + text_start;
    let text = line.get(text_start..close)?;
    if text.is_empty()
        || text.starts_with(char::is_whitespace)
        || text.ends_with(char::is_whitespace)
        || line.as_bytes().get(close + 2) == Some(&b'~')
    {
        return None;
    }
    Some(InlineStrikethrough {
        range: open..close + 2,
        text: text_start..close,
    })
}

/// Parses `[text](dest)` at `open`. `dest` may contain balanced parens, as
/// real URLs do.
fn parse_inline_link(line: &str, open: usize) -> Option<InlineLink> {
    let text_start = open + 1;
    let text_end = line[text_start..].find(']')? + text_start;
    let text = &line[text_start..text_end];
    // An empty label leaves nothing to show (C5); a `[` inside the label
    // means this bracket wasn't the link's opener.
    if text.is_empty() || text.contains('[') {
        return None;
    }
    if !line[text_end + 1..].starts_with('(') {
        return None;
    }
    let dest_start = text_end + 2;
    let mut depth = 1usize;
    let mut close = None;
    for (offset, character) in line[dest_start..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(dest_start + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let end = close + 1;
    Some(InlineLink {
        range: open..end,
        label: text_start..text_end,
        wikilink_target: None,
        destination: Some(dest_start..close),
    })
}

/// The parsed shape of a synced email note (spec V16): a full replacement
/// span set (V10's spans with message headers restyled, plus the envelope),
/// one entry per `## Sender — date` message, and the quoted-history runs
/// ready to crease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailPlan {
    pub spans: Vec<ConcealSpan>,
    pub messages: Vec<EmailMessage>,
    pub quotes: Vec<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessage {
    /// The `## …` header line, newline excluded.
    pub header_line: Range<usize>,
    /// End of the header line to the end of the section's last line — the
    /// range the reply's crease folds.
    pub body: Range<usize>,
    /// Whether the sender matches the connected account.
    pub own: bool,
}

/// Frontmatter keys that are sync machinery, folded away in email view; the
/// human keys (`from`, `link`, `due`, …) stay visible (V16 §5.1). `title`
/// is machinery because the `# Title` heading repeats it.
const MACHINERY_KEYS: [&str; 4] = ["source", "capture", "captured", "title"];

/// Whether a `source:` value names a registered mail source (V16 §4).
fn is_mail_source(value: &str) -> bool {
    value.eq_ignore_ascii_case("gmail")
}

/// Scans an email note: `Some` only when the frontmatter carries a
/// registered mail `source:`. `account` is the connected Google account for
/// the own-reply tint; senders never match a `None` account. Pure — the
/// caller resolves config and account (V16 §8).
pub fn email_plan(text: &str, account: Option<&str>) -> Option<EmailPlan> {
    let spans = scan_envelope(text)?;
    let mut plan = EmailPlan {
        spans,
        messages: Vec::new(),
        quotes: Vec::new(),
    };

    let mut headers: Vec<(
        Range<usize>,
        Range<usize>,
        Range<usize>,
        Option<Range<usize>>,
    )> = Vec::new();
    each_content_line(text, |line_start, line| {
        let Some((level, marker_start, text_start)) = atx_heading(line) else {
            return;
        };
        if level != 2 {
            return;
        }
        let heading_text = line[text_start..].trim_end();
        if heading_text.is_empty() {
            return;
        }
        let text_range = line_start + text_start..line_start + text_start + heading_text.len();
        // `Sender — date`; a header without the em-dash tail is all sender.
        let (sender, date) = match heading_text.rsplit_once(" — ") {
            Some((sender, _)) => {
                let sender = sender.trim_end();
                let sender_range = text_range.start..text_range.start + sender.len();
                (sender_range.clone(), Some(sender_range.end..text_range.end))
            }
            None => (text_range, None),
        };
        headers.push((
            line_start..line_start + line.len(),
            line_start + marker_start..line_start + text_start,
            sender,
            date,
        ));
    });

    let mut base = conceal_spans(text);
    for (header_line, marker, sender, date) in &headers {
        base.retain(|span| {
            let on_header =
                span.range.start >= header_line.start && span.range.end <= header_line.end;
            !(on_header
                && (span.kind == SpanKind::Heading(2)
                    || (span.kind == SpanKind::Marker && span.range == *marker)))
        });
        let own = account.is_some_and(|account| {
            let account = account.trim();
            !account.is_empty()
                && text
                    .get(sender.clone())
                    .is_some_and(|text| text.to_lowercase().contains(&account.to_lowercase()))
        });
        base.push(ConcealSpan::new(marker.clone(), SpanKind::EmailMarker(own)));
        base.push(ConcealSpan::new(sender.clone(), SpanKind::EmailSender(own)));
        if let Some(date) = date {
            base.push(ConcealSpan::new(date.clone(), SpanKind::EmailDate));
        }
        let body_start = header_line.end;
        let body_end = match headers
            .iter()
            .find(|(next, ..)| next.start > header_line.start)
        {
            Some((next, ..)) => next.start.saturating_sub(1).max(body_start),
            None => text.strip_suffix('\n').map_or(text.len(), str::len),
        };
        plan.messages.push(EmailMessage {
            header_line: header_line.clone(),
            body: body_start..body_end.max(body_start),
            own,
        });
    }

    let bodies: Vec<Range<usize>> = plan
        .messages
        .iter()
        .map(|message| message.body.clone())
        .collect();
    for body in bodies {
        scan_quotes(text, body, &mut plan);
    }
    plan.spans.append(&mut base);
    plan.spans.sort_by_key(|span| span.range.start);
    Some(plan)
}

/// The envelope spans, or `None` when the note has no frontmatter or its
/// `source:` isn't a registered mail source. Machinery lines fold away with
/// their newlines so the envelope compacts to the human lines; unknown keys
/// keep their value untouched — never hide what we don't understand (G6).
fn scan_envelope(text: &str) -> Option<Vec<ConcealSpan>> {
    let mut spans = Vec::new();
    let mut source_is_mail = false;
    let mut offset = 0;
    let mut lines = text.split('\n');

    let first = lines.next()?;
    if first.trim_end() != "---" {
        return None;
    }
    let hidden_line = |start: usize, raw_len: usize| {
        ConcealSpan::new(
            start..(start + raw_len + 1).min(text.len()),
            SpanKind::EmailHidden,
        )
    };
    spans.push(hidden_line(0, first.len()));
    offset += first.len() + 1;

    let mut closed = false;
    for raw_line in lines {
        let line_start = offset;
        offset += raw_line.len() + 1;
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            spans.push(hidden_line(line_start, raw_line.len()));
            closed = true;
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key_range = line_start..line_start + key.len() + 1;
        match key.trim().to_ascii_lowercase().as_str() {
            "source" => {
                source_is_mail = is_mail_source(value.trim());
                spans.push(hidden_line(line_start, raw_line.len()));
            }
            key if MACHINERY_KEYS.contains(&key) => {
                spans.push(hidden_line(line_start, raw_line.len()));
            }
            "from" => {
                spans.push(ConcealSpan::new(key_range, SpanKind::EmailDate));
                let sender = value.trim();
                if !sender.is_empty() {
                    let start = line_start + line.len() - value.trim_start().len();
                    spans.push(ConcealSpan::new(
                        start..start + sender.len(),
                        SpanKind::EmailSender(false),
                    ));
                }
            }
            "link" => {
                spans.push(ConcealSpan::new(
                    line_start..line_start + trimmed.len(),
                    SpanKind::EmailLink,
                ));
            }
            _ => {
                spans.push(ConcealSpan::new(key_range, SpanKind::EmailDate));
            }
        }
    }
    (closed && source_is_mail).then_some(spans)
}

/// Creases quoted history inside one message body: a run of two or more
/// `>` lines — plus an immediately preceding `… wrote:` attribution line —
/// collapses as a unit; every quoted line is coloured regardless of run
/// length (V16 §5.4).
fn scan_quotes(text: &str, body: Range<usize>, plan: &mut EmailPlan) {
    let Some(body_text) = text.get(body.clone()) else {
        return;
    };
    let mut lines = Vec::new();
    let mut offset = body.start;
    for raw_line in body_text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        lines.push((offset, line));
        offset += raw_line.len() + 1;
    }

    let is_quote =
        |line: &str| block_indent(line).is_some_and(|indent| line[indent..].starts_with('>'));
    let mut index = 0;
    while index < lines.len() {
        let (_, line) = lines[index];
        if !is_quote(line) {
            index += 1;
            continue;
        }
        let run_start = index;
        while index < lines.len() && is_quote(lines[index].1) {
            index += 1;
        }
        for &(line_start, line) in &lines[run_start..index] {
            plan.spans.push(ConcealSpan::new(
                line_start..line_start + line.len(),
                SpanKind::EmailQuote,
            ));
        }
        if index - run_start < 2 {
            continue;
        }
        let mut crease_start = lines[run_start].0;
        if run_start > 0 {
            let (attribution_start, attribution) = lines[run_start - 1];
            if attribution.trim_end().ends_with("wrote:") && !attribution.trim().is_empty() {
                crease_start = attribution_start;
                plan.spans.push(ConcealSpan::new(
                    attribution_start..attribution_start + attribution.len(),
                    SpanKind::EmailQuote,
                ));
            }
        }
        let (last_start, last_line) = lines[index - 1];
        plan.quotes.push(crease_start..last_start + last_line.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> Vec<(Range<usize>, SpanKind)> {
        conceal_spans(text)
            .into_iter()
            .map(|span| (span.range, span.kind))
            .collect()
    }

    fn slices(text: &str) -> Vec<(&str, SpanKind)> {
        conceal_spans(text)
            .into_iter()
            .map(|span| (&text[span.range.clone()], span.kind))
            .collect()
    }

    #[test]
    fn heading_levels_conceal_marker_and_colour_text() {
        assert_eq!(
            slices("# Title"),
            vec![("# ", SpanKind::Marker), ("Title", SpanKind::Heading(1))]
        );
        assert_eq!(
            slices("### Deep"),
            vec![("### ", SpanKind::Marker), ("Deep", SpanKind::Heading(3))]
        );
        assert_eq!(
            slices("##\t Tabbed  "),
            vec![
                ("##\t ", SpanKind::Marker),
                ("Tabbed", SpanKind::Heading(2))
            ]
        );
    }

    #[test]
    fn heading_marker_offsets_are_line_absolute() {
        let text = "plain\n## Two\n";
        assert_eq!(
            spans(text),
            vec![(6..9, SpanKind::Marker), (9..12, SpanKind::Heading(2)),]
        );
    }

    #[test]
    fn malformed_headings_are_left_alone() {
        assert_eq!(spans("#tag"), vec![]);
        assert_eq!(spans("#"), vec![]);
        assert_eq!(spans("##   "), vec![]);
        assert_eq!(spans("####### seven"), vec![]);
        assert_eq!(spans("    # indented code"), vec![]);
    }

    #[test]
    fn wikilink_conceals_brackets_and_colours_target() {
        assert_eq!(
            slices("see [[daily/today]] now"),
            vec![
                ("[[", SpanKind::Marker),
                ("daily/today", SpanKind::WikilinkLabel),
                ("]]", SpanKind::Marker),
            ]
        );
    }

    #[test]
    fn wikilink_alias_shows_only_the_alias() {
        assert_eq!(
            slices("[[projects/thock|the app]]"),
            vec![
                ("[[projects/thock|", SpanKind::Marker),
                ("the app", SpanKind::WikilinkLabel),
                ("]]", SpanKind::Marker),
            ]
        );
    }

    #[test]
    fn malformed_wikilinks_are_left_alone() {
        assert_eq!(spans("[[unclosed"), vec![]);
        assert_eq!(spans("[[]]"), vec![]);
        assert_eq!(spans("[[a|]]"), vec![]);
        assert_eq!(spans("[[|b]]"), vec![]);
        assert_eq!(spans("![[embedded note]]"), vec![]);
    }

    #[test]
    fn inline_link_conceals_brackets_and_destination() {
        assert_eq!(
            slices("read [the docs](https://zed.dev/docs) today"),
            vec![
                ("[", SpanKind::Marker),
                ("the docs", SpanKind::LinkLabel),
                ("](https://zed.dev/docs)", SpanKind::Marker),
            ]
        );
    }

    #[test]
    fn inline_link_destination_may_contain_balanced_parens() {
        assert_eq!(
            slices("[wiki](https://en.wikipedia.org/wiki/Foo_(bar))"),
            vec![
                ("[", SpanKind::Marker),
                ("wiki", SpanKind::LinkLabel),
                (
                    "](https://en.wikipedia.org/wiki/Foo_(bar))",
                    SpanKind::Marker
                ),
            ]
        );
    }

    #[test]
    fn malformed_inline_links_are_left_alone() {
        assert_eq!(spans("[text]("), vec![]);
        assert_eq!(spans("[text]"), vec![]);
        assert_eq!(spans("[](url)"), vec![]);
        assert_eq!(spans("![alt](src.png)"), vec![]);
        assert_eq!(spans("\\[not](a-link)"), vec![]);
    }

    #[test]
    fn relative_destinations_are_still_links() {
        assert_eq!(
            slices("[note](../weekly/2026-W34.md)"),
            vec![
                ("[", SpanKind::Marker),
                ("note", SpanKind::LinkLabel),
                ("](../weekly/2026-W34.md)", SpanKind::Marker),
            ]
        );
    }

    #[test]
    fn thematic_break_conceals_the_whole_line() {
        assert_eq!(slices("___"), vec![("___", SpanKind::Rule)]);
        assert_eq!(slices("  _____\t"), vec![("  _____\t", SpanKind::Rule)]);
        assert_eq!(spans("__"), vec![]);
        assert_eq!(spans("___ text"), vec![]);
        assert_eq!(spans("---"), vec![]);
        assert_eq!(spans("***"), vec![]);
    }

    #[test]
    fn fenced_code_blocks_are_never_concealed() {
        let text = "```bash\n# not a heading\n[[not a link]]\n```\n# real\n";
        assert_eq!(
            slices(text),
            vec![("# ", SpanKind::Marker), ("real", SpanKind::Heading(1))]
        );
    }

    #[test]
    fn tilde_fences_and_longer_closers_track_state() {
        let text = "~~~\n# hidden\n~~~~\n# shown\n";
        assert_eq!(
            slices(text),
            vec![("# ", SpanKind::Marker), ("shown", SpanKind::Heading(1))]
        );
    }

    #[test]
    fn unclosed_fence_conceals_nothing_after_it() {
        let text = "```\n# hidden\n[[also hidden]]\n";
        assert_eq!(spans(text), vec![]);
    }

    #[test]
    fn inline_code_spans_are_never_concealed() {
        assert_eq!(spans("`[[x]]`"), vec![]);
        assert_eq!(spans("a `[b](c)` d"), vec![]);
        assert_eq!(spans("`` [x](y) ``"), vec![]);
    }

    #[test]
    fn constructs_after_an_unmatched_backtick_still_conceal() {
        assert_eq!(
            slices("tick ` then [[link]]"),
            vec![
                ("[[", SpanKind::Marker),
                ("link", SpanKind::WikilinkLabel),
                ("]]", SpanKind::Marker),
            ]
        );
    }

    #[test]
    fn front_matter_is_left_alone() {
        let text = "---\ntitle: [[not a link]]\n---\n# after\n";
        assert_eq!(
            slices(text),
            vec![("# ", SpanKind::Marker), ("after", SpanKind::Heading(1))]
        );
    }

    #[test]
    fn dashes_later_in_the_file_are_not_front_matter() {
        let text = "# first\n---\n[[link]]\n";
        assert_eq!(
            slices(text),
            vec![
                ("# ", SpanKind::Marker),
                ("first", SpanKind::Heading(1)),
                ("[[", SpanKind::Marker),
                ("link", SpanKind::WikilinkLabel),
                ("]]", SpanKind::Marker),
            ]
        );
    }

    #[test]
    fn links_inside_headings_conceal_their_markers() {
        assert_eq!(
            slices("## see [[target|it]]"),
            vec![
                ("## ", SpanKind::Marker),
                ("see [[target|it]]", SpanKind::Heading(2)),
                ("[[target|", SpanKind::Marker),
                ("it", SpanKind::WikilinkLabel),
                ("]]", SpanKind::Marker),
            ]
        );
    }

    #[test]
    fn multiple_links_on_one_line_all_conceal() {
        assert_eq!(
            slices("[[a]] and [b](c)"),
            vec![
                ("[[", SpanKind::Marker),
                ("a", SpanKind::WikilinkLabel),
                ("]]", SpanKind::Marker),
                ("[", SpanKind::Marker),
                ("b", SpanKind::LinkLabel),
                ("](c)", SpanKind::Marker),
            ]
        );
    }

    /// The `(construct, label, target-or-destination)` slices of every link
    /// on `line`.
    fn links(line: &str) -> Vec<(&str, &str, Option<&str>)> {
        inline_links(line)
            .into_iter()
            .map(|link| {
                let dest = link
                    .wikilink_target
                    .or(link.destination)
                    .map(|range| &line[range]);
                (&line[link.range], &line[link.label], dest)
            })
            .collect()
    }

    #[test]
    fn inline_links_reports_both_link_forms_with_their_destinations() {
        assert_eq!(
            links("see [[notes/spec|the spec]] and [docs](https://a.example \"T\")"),
            vec![
                ("[[notes/spec|the spec]]", "the spec", Some("notes/spec")),
                (
                    "[docs](https://a.example \"T\")",
                    "docs",
                    Some("https://a.example \"T\"")
                ),
            ]
        );
    }

    #[test]
    fn inline_links_skips_excluded_and_malformed_constructs() {
        assert_eq!(
            links("a `[[x]]` b <!-- [y](z) --> [[ok]]"),
            vec![("[[ok]]", "ok", Some("ok"))]
        );
        assert_eq!(links("![[embed]] ![alt](src) [[unclosed"), vec![]);
    }

    /// The `(construct, struck text)` slices of every strikethrough on `line`.
    fn struck(line: &str) -> Vec<(&str, &str)> {
        inline_strikethroughs(line)
            .into_iter()
            .map(|run| (&line[run.range], &line[run.text]))
            .collect()
    }

    #[test]
    fn strikethrough_conceals_its_delimiters_and_marks_the_text() {
        assert_eq!(
            slices("drop ~~this plan~~ today"),
            vec![
                ("~~", SpanKind::Marker),
                ("this plan", SpanKind::Strikethrough),
                ("~~", SpanKind::Marker),
            ]
        );
        assert_eq!(
            struck("~~one~~ and ~~two~~"),
            vec![("~~one~~", "one"), ("~~two~~", "two")]
        );
    }

    #[test]
    fn malformed_strikethroughs_are_left_alone() {
        for line in [
            "~~unclosed",
            "~~~~",
            "~~ padded ~~",
            "~~trailing space ~~",
            "~~~three~~~",
            "\\~~escaped~~",
            "a `~~code~~` b",
            "<!-- ~~comment~~ -->",
            "tilde ~ alone",
        ] {
            assert_eq!(struck(line), vec![], "{line:?}");
        }
    }

    #[test]
    fn strikethrough_spans_a_link_but_never_reads_one_as_a_delimiter() {
        assert_eq!(
            struck("~~see [docs](https://a.example)~~"),
            vec![(
                "~~see [docs](https://a.example)~~",
                "see [docs](https://a.example)"
            )]
        );
        // The `~~` here is part of the destination, not a delimiter.
        assert_eq!(struck("[a](https://a.example/~~x~~)"), vec![]);
    }

    #[test]
    fn a_struck_task_line_keeps_its_checkbox_and_links() {
        assert_eq!(
            slices("- [ ] ~~read [[notes/spec]]~~\n"),
            vec![
                ("[ ]", SpanKind::Checkbox(false)),
                ("[[", SpanKind::Marker),
                ("notes/spec", SpanKind::WikilinkLabel),
                ("]]", SpanKind::Marker),
                ("~~", SpanKind::Marker),
                ("read [[notes/spec]]", SpanKind::Strikethrough),
                ("~~", SpanKind::Marker),
            ]
        );
    }

    #[test]
    fn tilde_fences_are_not_strikethroughs() {
        assert_eq!(spans("~~~\n~~struck~~\n~~~\n"), vec![]);
    }

    /// The `(construct, target)` slices of the wikilink at `offset`.
    fn reference(text: &str, offset: usize) -> Option<(&str, &str)> {
        wikilink_at(text, offset)
            .map(|reference| (&text[reference.range.clone()], &text[reference.target]))
    }

    #[test]
    fn wikilink_at_finds_the_construct_from_any_of_its_columns() {
        let text = "see [[wiki]] end";
        for offset in 4..12 {
            assert_eq!(
                reference(text, offset),
                Some(("[[wiki]]", "wiki")),
                "offset {offset}"
            );
        }
        assert_eq!(reference(text, 3), None);
        assert_eq!(reference(text, 12), None);
    }

    #[test]
    fn wikilink_at_returns_the_target_of_an_aliased_link() {
        let text = "[[projects/thock|the app]]";
        // On the alias, on the pipe, and on the brackets all count.
        for offset in [0, 10, 16, 20, 25] {
            assert_eq!(
                reference(text, offset),
                Some(("[[projects/thock|the app]]", "projects/thock")),
                "offset {offset}"
            );
        }
    }

    #[test]
    fn wikilink_at_honours_the_conceal_exclusions() {
        assert_eq!(reference("```\n[[x]]\n```\n", 6), None);
        assert_eq!(reference("a `[[x]]` b", 4), None);
        assert_eq!(reference("![[embed]]", 4), None);
        assert_eq!(reference("---\nkey: [[x]]\n---\n", 10), None);
        assert_eq!(reference("[[unclosed", 1), None);
    }

    #[test]
    fn wikilink_at_respects_line_boundaries() {
        let text = "plain\n[[a]]\n[[b]]\n";
        assert_eq!(reference(text, 6), Some(("[[a]]", "a")));
        assert_eq!(reference(text, 10), Some(("[[a]]", "a")));
        // The newline after `[[a]]` is past the construct.
        assert_eq!(reference(text, 11), None);
        assert_eq!(reference(text, 12), Some(("[[b]]", "b")));
        assert_eq!(reference(text, 0), None);
        assert_eq!(reference(text, text.len() + 10), None);
    }

    #[test]
    fn wikilink_at_ignores_external_links_but_finds_later_wikilinks() {
        let text = "[docs](https://a.example) then [[note]]";
        assert_eq!(reference(text, 2), None);
        assert_eq!(reference(text, 33), Some(("[[note]]", "note")));
    }

    #[test]
    fn task_checkboxes_conceal_the_brackets_and_carry_their_state() {
        assert_eq!(
            slices("- [ ] open task\n"),
            vec![("[ ]", SpanKind::Checkbox(false))]
        );
        assert_eq!(
            slices("* [x] done\n+ [X] also done\n"),
            vec![
                ("[x]", SpanKind::Checkbox(true)),
                ("[X]", SpanKind::Checkbox(true)),
            ]
        );
        // Nested tasks are tasks, and an empty task is still a checkbox.
        assert_eq!(
            slices("    - [ ] nested\n"),
            vec![("[ ]", SpanKind::Checkbox(false))]
        );
        assert_eq!(slices("- [ ]\n"), vec![("[ ]", SpanKind::Checkbox(false))]);
    }

    #[test]
    fn checkbox_lookalikes_are_left_alone() {
        for line in [
            "[ ] no bullet\n",
            "-[ ] no space after the bullet\n",
            "- [] empty brackets\n",
            "- [y] not a state\n",
            "- [ ]text with no gap\n",
            "1. [ ] ordered lists are not task lists here\n",
            "```\n- [ ] in a fence\n```\n",
        ] {
            assert_eq!(spans(line), vec![], "{line:?}");
        }
    }

    #[test]
    fn a_task_line_still_scans_its_links() {
        assert_eq!(
            slices("- [x] read [[notes/spec]]\n"),
            vec![
                ("[x]", SpanKind::Checkbox(true)),
                ("[[", SpanKind::Marker),
                ("notes/spec", SpanKind::WikilinkLabel),
                ("]]", SpanKind::Marker),
            ]
        );
    }

    #[test]
    fn html_comments_are_concealed_whole() {
        assert_eq!(
            slices("task <!--gmail:9f2c--> tail\n"),
            vec![("<!--gmail:9f2c-->", SpanKind::Marker)]
        );
        assert_eq!(
            slices("<!--a--> mid <!--b-->\n"),
            vec![
                ("<!--a-->", SpanKind::Marker),
                ("<!--b-->", SpanKind::Marker)
            ]
        );
        // The empty comment closes on its own dashes.
        assert_eq!(slices("<!---->\n"), vec![("<!---->", SpanKind::Marker)]);
    }

    #[test]
    fn unclosed_and_multi_line_comments_stay_visible() {
        assert_eq!(spans("half <!-- open\n"), vec![]);
        assert_eq!(spans("<!--\nspanning\n-->\n"), vec![]);
        assert_eq!(spans("a `<!--code-->` b\n"), vec![]);
        assert_eq!(spans("```\n<!--fenced-->\n```\n"), vec![]);
    }

    #[test]
    fn markup_inside_a_comment_is_not_scanned_separately() {
        assert_eq!(
            slices("note <!-- [[hidden]] [text](url) -->\n"),
            vec![("<!-- [[hidden]] [text](url) -->", SpanKind::Marker)]
        );
        assert_eq!(wikilink_at("note <!-- [[hidden]] -->", 13), None);
    }

    const EMAIL: &str = "---\n\
        source:   gmail\n\
        capture:  8f3c21ab9d04\n\
        captured: 2026-08-28T09:12:44-07:00\n\
        title:    Renewal quote\n\
        from:     Marta Reyes <marta@acmeinsure.com>\n\
        link:     https://mail.google.com/mail/u/d@e.com/#all/198f\n\
        ---\n\
        \n\
        # Renewal quote\n\
        \n\
        ## Marta Reyes <marta@acmeinsure.com> — 2026-08-26 14:02\n\
        \n\
        Hi Diego,\n\
        \n\
        ## Diego Tavares <diego.exodo@gmail.com> — 2026-08-27 08:41\n\
        \n\
        Can we bump the rider?\n\
        \n\
        On Wed, Marta wrote:\n\
        > premium comes to $1,284/yr\n\
        > down 4% from last year\n";

    fn spans_of<'a>(plan: &'a EmailPlan, text: &'a str, kind: SpanKind) -> Vec<&'a str> {
        plan.spans
            .iter()
            .filter(|span| span.kind == kind)
            .map(|span| &text[span.range.clone()])
            .collect()
    }

    #[test]
    fn email_plan_requires_a_mail_source() {
        assert_eq!(email_plan("# plain note\n", None), None);
        assert_eq!(email_plan("---\nsource: tasks\n---\nbody\n", None), None);
        assert_eq!(
            email_plan("---\nsource: gmail\nbody without close\n", None),
            None
        );
        assert!(email_plan("---\nsource: gmail\n---\nbody\n", None).is_some());
    }

    #[test]
    fn email_envelope_hides_machinery_and_styles_the_rest() {
        let plan = email_plan(EMAIL, None).unwrap();
        let hidden = spans_of(&plan, EMAIL, SpanKind::EmailHidden);
        assert_eq!(hidden.len(), 6, "{hidden:?}");
        assert!(hidden[0].starts_with("---\n"));
        assert!(hidden[1].starts_with("source:") && hidden[1].ends_with('\n'));
        assert!(hidden[4].starts_with("title:"));
        assert!(hidden[5].starts_with("---"));

        // The envelope's `from:` value plus both header senders — no
        // account, so nothing tints as own.
        assert_eq!(
            spans_of(&plan, EMAIL, SpanKind::EmailSender(false)),
            vec![
                "Marta Reyes <marta@acmeinsure.com>",
                "Marta Reyes <marta@acmeinsure.com>",
                "Diego Tavares <diego.exodo@gmail.com>",
            ]
        );
        assert_eq!(
            spans_of(&plan, EMAIL, SpanKind::EmailLink),
            vec!["link:     https://mail.google.com/mail/u/d@e.com/#all/198f"]
        );
        // The `from:` key is muted chrome, the header dates too.
        let muted = spans_of(&plan, EMAIL, SpanKind::EmailDate);
        assert!(muted.contains(&"from:"), "{muted:?}");
        assert!(muted.contains(&" — 2026-08-26 14:02"), "{muted:?}");
    }

    #[test]
    fn email_messages_split_sender_and_body_and_tint_own_replies() {
        let plan = email_plan(EMAIL, Some("diego.exodo@gmail.com")).unwrap();
        assert_eq!(plan.messages.len(), 2);
        assert!(!plan.messages[0].own);
        assert!(plan.messages[1].own);
        assert_eq!(
            spans_of(&plan, EMAIL, SpanKind::EmailSender(true)),
            vec!["Diego Tavares <diego.exodo@gmail.com>"]
        );
        assert_eq!(
            spans_of(&plan, EMAIL, SpanKind::EmailMarker(false)),
            vec!["## "]
        );

        // No Heading(2) or heading-marker span survives on header lines.
        assert!(spans_of(&plan, EMAIL, SpanKind::Heading(2)).is_empty());
        assert_eq!(
            spans_of(&plan, EMAIL, SpanKind::Heading(1)),
            vec!["Renewal quote"]
        );

        // Body: from the header line's end to the line before the next
        // header (or the file's last line).
        let first = &plan.messages[0];
        assert_eq!(
            &EMAIL[first.header_line.clone()],
            "## Marta Reyes <marta@acmeinsure.com> — 2026-08-26 14:02"
        );
        assert_eq!(&EMAIL[first.body.clone()], "\n\nHi Diego,\n");
        let second = &plan.messages[1];
        assert!(EMAIL[second.body.clone()].ends_with("> down 4% from last year"));
    }

    #[test]
    fn email_quotes_crease_with_their_attribution() {
        let plan = email_plan(EMAIL, None).unwrap();
        assert_eq!(plan.quotes.len(), 1);
        assert_eq!(
            &EMAIL[plan.quotes[0].clone()],
            "On Wed, Marta wrote:\n> premium comes to $1,284/yr\n> down 4% from last year"
        );
        let quote_lines = spans_of(&plan, EMAIL, SpanKind::EmailQuote);
        assert_eq!(quote_lines.len(), 3);

        // A single quoted line colours but never creases.
        let one = "---\nsource: gmail\n---\n## A — 1\n\nx\n> lone quote\ntail\n";
        let plan = email_plan(one, None).unwrap();
        assert!(plan.quotes.is_empty());
        assert_eq!(
            spans_of(&plan, one, SpanKind::EmailQuote),
            vec!["> lone quote"]
        );
    }

    #[test]
    fn email_headers_inside_fences_are_not_messages() {
        let text = "---\nsource: gmail\n---\n\n```\n## Not a message — ever\n```\n\nbody\n";
        let plan = email_plan(text, None).unwrap();
        assert!(plan.messages.is_empty());

        // A `##` heading without the em-dash tail is all sender — a
        // hand-added section still reads as a section.
        let text = "---\nsource: gmail\n---\n\n## Notes\n\nplain\n";
        let plan = email_plan(text, None).unwrap();
        assert_eq!(plan.messages.len(), 1);
        assert_eq!(
            spans_of(&plan, text, SpanKind::EmailSender(false)),
            vec!["Notes"]
        );
    }

    #[test]
    fn crlf_lines_keep_offsets_and_exclude_the_carriage_return() {
        let text = "# a\r\n[[b]]\r\n";
        let all = conceal_spans(text);
        for span in &all {
            assert!(!text[span.range.clone()].contains('\r'));
        }
        assert_eq!(
            all.iter()
                .map(|span| &text[span.range.clone()])
                .collect::<Vec<_>>(),
            vec!["# ", "a", "[[", "b", "]]"]
        );
    }
}
