//! How a note is read (V56 §4.1): one Markdown body, or for an email the
//! conversation of V41 §4.4, a card per message with the newest open and what
//! repeats folded away. The Inbox's right pane and an email note's own tab
//! are the two hosts.
//!
//! This is a reading of the note. The file keeps every byte.

use chrono::Local;
use gpui::{
    Action, AnyElement, App, AppContext as _, Context, Entity, FocusHandle, FontWeight,
    HighlightStyle, Hsla, ImageSource, KeyContext, Length, Pixels, Rems, Resource, ScrollHandle,
    ScrollWheelEvent, SharedString, StyledText, TextRun, TextStyleRefinement, UnderlineStyle,
    Window, WrappedLine, div, hsla, linear_color_stop, linear_gradient, point, prelude::*, px,
    relative, rems, transparent_black,
};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use settings::{Settings as _, SettingsStore};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use theme_settings::ThemeSettings;
use ui::{
    ActiveTheme as _, ButtonLike, Clickable as _, Color, HeadlineSize, Icon, IconName, IconSize,
    KeyBinding, Label, LabelCommon as _, LabelSize, TextSize, Tooltip, h_flex, v_flex,
};
use util::paths::PathStyle;
use util::rel_path::RelPath;

use crate::inbox::InboxItem;
use crate::inbox_email::{
    EmailBlock, EmailField, EmailMessage, EmailThread, FoldKind, drop_restated_subject, full_title,
    initials, mark_attributions, parse_thread, quiet_system_lines, snippet, split_fields,
    ticket_prefix, trailing_ticket,
};
use crate::inbox_view::{NextInboxMessage, PreviousInboxMessage, ToggleInboxFolds};
use crate::markdown_conceal::vault_image_path;
use crate::vault::Vault;

/// The frontmatter `source:` of a note captured from Gmail.
pub(crate) const EMAIL_SOURCE: &str = "gmail";

/// One step of the reader's scroll keys.
pub(crate) const READER_LINE: Pixels = px(40.);
/// The row above a pane: the Inbox's two headers and an email tab's.
pub(crate) const PANE_HEADER_HEIGHT: Pixels = px(52.);
pub(crate) const TITLE_LINE_HEIGHT: f32 = 1.3;
pub(crate) const READER_MAX_WIDTH: Pixels = px(960.);
pub(crate) const FIELD_MIN_WIDTH: Pixels = px(300.);
pub(crate) const FIELD_KEY_WIDTH: Pixels = px(104.);
pub(crate) const FIELD_LINE_HEIGHT: f32 = 1.35;
/// A message of at most this many lines leads its card at the larger size.
pub(crate) const LEAD_LINES: usize = 2;
/// A longer message's size and leading: under a lead, over the folded text.
pub(crate) const MESSAGE_TEXT_SIZE: Rems = rems(0.8125);
pub(crate) const MESSAGE_LINE_HEIGHT: Rems = rems(1.2);
/// A table of this many fields reads down one column, however wide the reader.
pub(crate) const FIELD_SINGLE_COLUMN_ROWS: usize = 6;
/// A changed value, a step over the labels and the rest of the record and
/// under the person's words.
pub(crate) const CHANGED_VALUE_OPACITY: f32 = 0.92;
pub(crate) const SENDER_BADGE_SIZE: Pixels = px(28.);
pub(crate) const TITLE_SIZE: HeadlineSize = HeadlineSize::Large;
/// The reader's side padding, which its rules start from.
pub(crate) const READER_PADDING: Rems = rems(2.5);
/// A card's side padding, inside its one-pixel border.
pub(crate) const CARD_PADDING: Rems = rems(1.);
/// A row's title, and the reader's, on at most this many lines.
pub(crate) const TITLE_LINES: usize = 2;
/// Holds a trailing id to its bar, so a line can't end on a stranded `|`.
pub(crate) const TITLE_ID_SEPARATOR: &str = "|\u{a0}";
pub(crate) const KEYCAP_HEIGHT: Rems = rems(1.125);
pub(crate) const KEYCAP_TEXT_SIZE: Rems = rems(0.75);
/// A list longer than this draws its first items and a count of the rest
/// until the folds open.
pub(crate) const MAX_FIELD_LIST_ITEMS: usize = 3;
/// Past this a field's old value is more to read than it tells.
pub(crate) const MAX_PREVIOUS_VALUE_LENGTH: usize = 32;
pub(crate) const EARLIER_CUE_HEIGHT: Pixels = px(36.);
/// Under the cue, where text passing beneath it fades out instead of being cut.
pub(crate) const CUE_FADE_HEIGHT: Pixels = px(14.);
/// Where a revealed card's top sits in the reader: one gap under the earlier
/// cue, so the card before ends behind it.
pub(crate) const REVEAL_CLEARANCE: Pixels = px(44.);
/// A card's header, which a landing keeps on screen.
pub(crate) const CARD_HEADER_HEIGHT: Pixels = px(44.);
/// Folded text sits at the field table's size, under the new words.
pub(crate) const FOLDED_LINE_HEIGHT: Rems = rems(1.1);
/// Thock Light's muted text sits nearer its text than Dark's. This keeps a
/// visible step and still reads at 4.5:1 on a card.
pub(crate) const SYSTEM_LINE_OPACITY_LIGHT: f32 = 0.95;
/// A folded message's `Comment #3 (by …)` line, a step under the text it
/// introduces.
pub(crate) const ATTRIBUTION_OPACITY: f32 = 0.7;
pub(crate) const ATTRIBUTION_TOP_MARGIN: Pixels = px(8.);
pub(crate) const ATTRIBUTION_BOTTOM_MARGIN: Pixels = px(2.);
/// The current card's ring in reader focus, when the rail says the rest.
pub(crate) const NEUTRAL_RING_OPACITY: f32 = 0.45;
/// Spread round the wheel, past the reds that read as an error and the
/// accent's amber. Shared by both themes, so a person's colour doesn't change
/// with the appearance; only the lightness does.
pub(crate) const SENDER_HUES: [f32; 6] = [212., 268., 176., 134., 322., 88.];

/// The selected note as the reader shows it, rebuilt when its bytes change.
#[derive(Default)]
pub(crate) struct ReaderNote {
    pub(crate) hash: Option<String>,
    /// How an email reads; other notes render whole.
    pub(crate) thread: Option<Rc<EmailThread>>,
    /// The whole subject, when the sender's system cut it short.
    pub(crate) title: Option<String>,
    /// By message and block, built as they come into view.
    pub(crate) markdown: HashMap<(usize, usize), Entity<Markdown>>,
    pub(crate) folds_open: bool,
    /// The message open while the folds are closed; a note opens on its
    /// newest.
    pub(crate) current_message: usize,
    /// The current message moved or the note changed, so the reader follows
    /// once the new layout is in the frame.
    pub(crate) reveal_current: bool,
    /// What the earlier cue says, as of the last painted frame.
    pub(crate) messages_above: usize,
    /// The card running on under the cue with its header gone, which the cue
    /// names instead.
    pub(crate) card_under_cue: Option<usize>,
}

/// What every block of the reader is drawn with.
pub(crate) struct ReaderStyle {
    pub(crate) body: MarkdownStyle,
    /// A message past `LEAD_LINES`, which would run too large at the lead size.
    pub(crate) long_message: MarkdownStyle,
    pub(crate) folded: MarkdownStyle,
    /// A ticketing system's boilerplate among the folds, a step under them.
    pub(crate) folded_system: MarkdownStyle,
    /// A ticketing system's own lines, under the person's words.
    pub(crate) system: MarkdownStyle,
    pub(crate) images: Rc<dyn Fn(&str) -> Option<ImageSource>>,
}

/// One folded stretch of a message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FoldPiece {
    Text(FoldKind),
    Fields,
}

/// How many of `widths`, in order and `gap` apart, fit in `room`.
/// `text` shaped in the interface font. Before the first frame there's no
/// width to shape it at.
pub(crate) fn wrapped_lines(
    text: &str,
    size: Pixels,
    weight: FontWeight,
    width: Pixels,
    window: &Window,
    cx: &App,
) -> Option<Vec<WrappedLine>> {
    if width <= px(0.) {
        return None;
    }
    let mut font = ThemeSettings::get_global(cx).ui_font.clone();
    font.weight = weight;
    let run = TextRun {
        len: text.len(),
        font,
        color: cx.theme().colors().text,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_text(text.to_string().into(), size, &[run], Some(width), None)
        .ok()
        .map(|lines| lines.into_vec())
}

pub(crate) fn line_count(lines: &[WrappedLine]) -> usize {
    lines
        .iter()
        .map(|line| line.wrap_boundaries().len() + 1)
        .sum()
}

/// One keycap look for every Inbox hint. Dimmed, the frame fades with the key.
pub(crate) fn framed_keycap(keycap: KeyBinding, dimmed: bool, cx: &App) -> gpui::Div {
    let colors = cx.theme().colors();
    h_flex()
        .flex_none()
        .h(KEYCAP_HEIGHT)
        .min_w(KEYCAP_HEIGHT)
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(if dimmed {
            colors.border_variant
        } else {
            colors.text_placeholder
        })
        .bg(colors.element_background)
        .child(keycap.size(KEYCAP_TEXT_SIZE).disabled(dimmed))
}

/// The current card's ring while the reader has focus: the rail alone is
/// amber then, so the card can't be taken for the suggested tile.
pub(crate) fn neutral_ring(cx: &App) -> Hsla {
    cx.theme().colors().text_muted.opacity(NEUTRAL_RING_OPACITY)
}

/// A hint's words: they dim along with their keycaps.
pub(crate) fn hint_color(dimmed: bool) -> Color {
    if dimmed {
        Color::Disabled
    } else {
        Color::Muted
    }
}

/// A sender's colour in a theme: their hue, at a lightness that holds up as
/// text on the badge.
pub(crate) fn sender_color(participant: u32, light: bool) -> Hsla {
    let hue = SENDER_HUES[participant as usize % SENDER_HUES.len()] / 360.;
    if light {
        hsla(hue, 0.6, 0.36, 1.)
    } else {
        hsla(hue, 0.5, 0.72, 1.)
    }
}

/// The body without a first heading that repeats the title, since the
/// reader already shows the title above the destination bar. A heading that
/// says something else is the note's own and stays.
pub(crate) fn without_leading_title<'a>(body: &'a str, title: &str) -> &'a str {
    let body = body.trim_start();
    let (first, rest) = body.split_once('\n').unwrap_or((body, ""));
    let heading = first.trim_start_matches('#');
    let is_heading = heading.len() < first.len() && heading.starts_with([' ', '\t']);
    if is_heading && heading.trim().eq_ignore_ascii_case(title.trim()) {
        rest.trim_start()
    } else {
        body
    }
}

/// A field's value as drawn: an empty one still reads as a value.
pub(crate) fn field_text(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

pub(crate) fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// What a card holds folded, in a reader's words and a fixed order, each thing
/// once however many places it came from.
pub(crate) fn fold_summary(pieces: &[FoldPiece]) -> String {
    let holds = |kind| pieces.contains(&FoldPiece::Text(kind));
    let names: Vec<&str> = [
        (holds(FoldKind::Repeated), "quoted replies"),
        (holds(FoldKind::Quoted), "quoted text"),
        (
            holds(FoldKind::System) || pieces.contains(&FoldPiece::Fields),
            "ticket details",
        ),
        (holds(FoldKind::Signature), "signature"),
    ]
    .into_iter()
    .filter_map(|(held, name)| held.then_some(name))
    .collect();
    match names.split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// What a card's fold row says: what opening it would show, or that it hides.
pub(crate) fn fold_label(open: bool, pieces: &[FoldPiece]) -> String {
    if open {
        "Hide".to_string()
    } else {
        format!("Show {}", fold_summary(pieces))
    }
}

/// How a field's value reads: a comma list as `a, b, c` and how many more
/// there are, so it holds one line until the folds open; anything else comes
/// back whole.
pub(crate) fn field_list(value: &str, whole: bool) -> (String, usize) {
    let items = list_items(value);
    if items.len() < 2 || !is_name_list(&items) {
        return (value.to_string(), 0);
    }
    let shown = if whole {
        items.len()
    } else {
        items.len().min(MAX_FIELD_LIST_ITEMS)
    };
    (items[..shown].join(", "), items.len() - shown)
}

/// What a list field's change comes to: `+ neaton`, `− sosh`, the rest being
/// what it already said. Nothing when either value isn't a list of names, or
/// the two have nothing in common and the new one is simply a new value.
pub(crate) fn list_change(value: &str, previous: &str) -> Option<String> {
    let (new, old) = (list_items(value), list_items(previous));
    let shares_an_item = new.iter().any(|item| old.contains(item));
    if !(is_name_list(&new) && is_name_list(&old)) || !shares_an_item {
        return None;
    }
    let difference = |sign: &str, from: &[&str], against: &[&str]| {
        let items: Vec<&str> = from
            .iter()
            .filter(|item| !against.contains(item))
            .copied()
            .collect();
        (!items.is_empty()).then(|| format!("{sign} {}", items.join(", ")))
    };
    let parts: Vec<String> = difference("+", &new, &old)
        .into_iter()
        .chain(difference("−", &old, &new))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

pub(crate) fn list_items(value: &str) -> Vec<&str> {
    value.split(',').map(str::trim).collect()
}

/// Prose has spaces inside its items and a number has no letters.
pub(crate) fn is_name_list(items: &[&str]) -> bool {
    items
        .iter()
        .all(|item| item.contains(char::is_alphabetic) && !item.contains(char::is_whitespace))
}

/// An old value as `was …` gives it: the same reading as the new one.
pub(crate) fn was_text(previous: &str, whole: bool) -> String {
    let previous = field_text(previous);
    match field_list(previous, whole) {
        (shown, 0) => shown,
        (shown, more) => format!("{shown} +{more} more"),
    }
}

/// Whether `was …` tells more than it costs.
pub(crate) fn previous_helps(previous: &str) -> bool {
    was_text(previous, false).chars().count() <= MAX_PREVIOUS_VALUE_LENGTH
}

pub(crate) fn previous_label(count: usize) -> String {
    counted(count, "previous message", "previous messages")
}

/// A card's header as the cue carries it: `neaton · 4 of 4`.
pub(crate) fn carried_label(name: &str, index: usize, total: usize) -> String {
    format!("{name} · {} of {total}", index + 1)
}

/// Who a card is from: the person, when a system relayed their words.
pub(crate) fn message_name(message: &EmailMessage) -> String {
    message
        .author
        .clone()
        .unwrap_or_else(|| sender_name(&message.from))
}

/// `Marta <marta@example.com>` → `Marta`.
pub(crate) fn sender_name(from: &str) -> String {
    let name = from
        .split('<')
        .next()
        .unwrap_or(from)
        .trim()
        .trim_matches('"');
    if name.is_empty() {
        from.trim_matches(['<', '>']).to_string()
    } else {
        name.to_string()
    }
}

/// `via helpdesk@example.com` on a relayed message's card, unless the reader
/// header already names that sender.
pub(crate) fn relayed_via(sender: &str, header_from: Option<&str>) -> Option<String> {
    let header_sender = header_from.map(sender_name);
    (header_sender.as_deref() != Some(sender)).then(|| format!("via {sender}"))
}

/// `Marta <marta@example.com>` → `marta@example.com`; `None` without one.
pub(crate) fn sender_address(from: &str) -> Option<String> {
    let (_, rest) = from.split_once('<')?;
    let (address, _) = rest.split_once('>')?;
    let address = address.trim();
    (!address.is_empty() && address != sender_name(from)).then(|| address.to_string())
}

pub(crate) fn captured_label(captured: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(captured)
        .map(|moment| {
            moment
                .with_timezone(&Local)
                .format("%b %-d, %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| captured.to_string())
}

/// Where a revealed card lands, as a scroll offset. `landings` holds the offset
/// that brings each card up to the current one, the last, to its place under
/// the cue. Too near the end of the thread for that, the current card lands on
/// the nearest card boundary the scroll reaches, so nothing of the card before
/// shows under the cue, as long as the current card's header stays on screen.
pub(crate) fn landing(landings: &[Pixels], farthest: Pixels, viewport_height: Pixels) -> Pixels {
    let Some((&current, earlier)) = landings.split_last() else {
        return px(0.);
    };
    if current <= farthest {
        return current;
    }
    earlier
        .iter()
        .rev()
        .find(|landing| **landing <= farthest)
        .copied()
        .filter(|landing| {
            current - *landing + REVEAL_CLEARANCE + CARD_HEADER_HEIGHT <= viewport_height
        })
        .unwrap_or(farthest)
}

/// What holds an [`EmailReader`]: its listeners reach the reader through the
/// view that owns it.
pub(crate) trait EmailReaderHost: Sized + 'static {
    fn email_reader(&mut self) -> &mut EmailReader;
}

/// A note as read, held by the view that shows it.
pub(crate) struct EmailReader {
    pub(crate) reader_focus: FocusHandle,
    pub(crate) reader_scroll: ScrollHandle,
    pub(crate) reader_note: ReaderNote,
    /// The note being read, as parsed from its file.
    pub(crate) item: Option<InboxItem>,
    /// The reader's width in the frame just painted; zero before the first.
    pub(crate) reader_width: Pixels,
    /// Where the hints' keycaps are read from: the host's own bindings.
    hint_context: KeyContext,
    /// Draws the hints' keycaps dimmed, while the host's keys go elsewhere.
    pub(crate) keys_off: bool,
    pub(crate) vault: Option<Vault>,
}

impl EmailReader {
    pub(crate) fn new(hint_context: KeyContext, vault: Option<Vault>, cx: &mut App) -> Self {
        Self {
            reader_focus: cx.focus_handle(),
            reader_scroll: ScrollHandle::new(),
            reader_note: ReaderNote::default(),
            item: None,
            reader_width: px(0.),
            hint_context,
            keys_off: false,
            vault,
        }
    }

    /// Brings the reader in step with the note its host shows. `key` names
    /// the note's bytes: the same key is the same reading. A note that only
    /// grew keeps its folds, and stays on its message unless a newer one came.
    pub(crate) fn set_note<H: EmailReaderHost>(
        &mut self,
        note: Option<(&str, &InboxItem)>,
        cx: &mut Context<H>,
    ) {
        let Some((key, item)) = note else {
            if self.item.is_some() {
                self.item = None;
                self.reader_note = ReaderNote::default();
            }
            return;
        };
        if self.reader_note.hash.as_deref() == Some(key) {
            return;
        }
        let same_note = self
            .item
            .as_ref()
            .is_some_and(|shown| shown.path == item.path);
        let is_email = item.source.as_deref() == Some(EMAIL_SOURCE);
        let body = without_leading_title(&item.body, &item.title);
        let title = whole_title(item);
        let thread = is_email.then(|| {
            let subject = title.as_deref().unwrap_or(&item.title);
            let mut thread = parse_thread(body, item.from.as_deref());
            drop_restated_subject(&mut thread, subject);
            quiet_system_lines(&mut thread, subject);
            mark_attributions(&mut thread);
            Rc::new(thread)
        });
        let count = |thread: &Option<Rc<EmailThread>>| {
            thread.as_ref().map_or(0, |thread| thread.messages.len())
        };
        let newest = count(&thread).saturating_sub(1);
        let grew = count(&thread) > count(&self.reader_note.thread);
        let kept = same_note && !grew;
        let current_message = if kept {
            self.reader_note.current_message.min(newest)
        } else {
            newest
        };
        let folds_open = same_note && self.reader_note.folds_open;
        self.reader_note = ReaderNote {
            hash: Some(key.to_string()),
            thread,
            title,
            current_message,
            folds_open,
            reveal_current: !kept,
            ..ReaderNote::default()
        };
        self.item = Some(item.clone());
        if !kept {
            self.reader_scroll.set_offset(point(px(0.), px(0.)));
        }
        self.parse_thread_blocks(cx);
    }

    /// The note's title as shown: the whole subject where the sender's system
    /// cut it short.
    pub(crate) fn title(&self) -> Option<String> {
        self.reader_note
            .title
            .clone()
            .or_else(|| self.item.as_ref().map(|item| item.title.clone()))
    }

    pub(crate) fn step<H: EmailReaderHost>(&mut self, forward: bool, cx: &mut Context<H>) {
        self.step_message(forward, cx);
    }

    pub(crate) fn toggle_folds<H: EmailReaderHost>(&mut self, cx: &mut Context<H>) {
        self.reader_note.folds_open = !self.reader_note.folds_open;
        self.reader_note.reveal_current = true;
        cx.notify();
    }

    /// An action's keycap as the host binds it. Asking the focus handle would
    /// answer for whichever pane holds focus.
    pub(crate) fn keycap(&self, action: &dyn Action, window: &Window, cx: &App) -> KeyBinding {
        window
            .highest_precedence_binding_for_action_in_context(action, self.hint_context.clone())
            .map(|binding| binding_keycap(&binding, cx))
            .unwrap_or_else(|| KeyBinding::for_action_in(action, &self.reader_focus, cx))
    }

    /// V39 §4.3's rule for the reader: vault-root paths and the images
    /// folder resolve against the root, bare relative paths against the
    /// note's own folder, web pictures are fetched.
    fn image_resolver(&self) -> Rc<dyn Fn(&str) -> Option<ImageSource>> {
        let vault = self.vault.clone();
        let note_dir = self
            .item
            .as_ref()
            .and_then(|item| Path::new(&item.path).parent())
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Rc::new(move |src: &str| {
            if src.starts_with("http://") || src.starts_with("https://") {
                return Some(ImageSource::Resource(Resource::Uri(src.to_string().into())));
            }
            let vault = vault.as_ref()?;
            let note_dir = RelPath::new(&note_dir, PathStyle::Unix).ok()?;
            let relative = vault_image_path(src, &note_dir, &vault.config.images.dir)?;
            let path = vault.root.join(relative.as_std_path());
            Some(ImageSource::Resource(Resource::Path(Arc::from(path))))
        })
    }

    /// The note's title in its fixed block: whatever its length, what sits
    /// under it stays where the hand expects it. A trailing ticket id is
    /// muted.
    pub(crate) fn render_title(&self, window: &Window, cx: &App) -> gpui::Div {
        let title = self.title().unwrap_or_default();
        let shown_title = self.reader_title(&title, window, cx);
        let id_tone = HighlightStyle {
            color: Some(cx.theme().colors().text_muted),
            ..Default::default()
        };
        let id_start = shown_title
            .rfind(TITLE_ID_SEPARATOR)
            .or_else(|| shown_title.rfind('\n'));
        let id_length = shown_title.len();
        let title_text = div()
            .text_size(TITLE_SIZE.rems())
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().colors().text)
            .line_clamp(TITLE_LINES)
            .text_ellipsis()
            .child(
                StyledText::new(shown_title)
                    .with_highlights(id_start.map(|start| (start..id_length, id_tone))),
            );
        let title_height = rems(TITLE_SIZE.rems().0 * TITLE_LINE_HEIGHT * TITLE_LINES as f32);
        div()
            .debug_selector(|| "INBOX_READER_TITLE".to_string())
            .flex_none()
            .h(title_height)
            .line_height(relative(TITLE_LINE_HEIGHT))
            .child(title_text)
    }

    /// The scrolling body: the note's cards, under the earlier cue once the
    /// reader has scrolled.
    pub(crate) fn render_body<H: EmailReaderHost>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<H>,
    ) -> AnyElement {
        let style = self.reader_style(window, cx);
        let body = self.render_note_body(&style, window, cx);
        let earlier = self.reader_note.messages_above;
        let carried = self.carried_card();
        let earlier_cue = (earlier > 0 || carried.is_some())
            .then(|| self.render_earlier_cue(earlier, carried, window, cx));
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                v_flex()
                    .id("inbox-reader")
                    .size_full()
                    .track_focus(&self.reader_focus)
                    .overflow_y_scroll()
                    .track_scroll(&self.reader_scroll)
                    .on_scroll_wheel(cx.listener(|_, _: &ScrollWheelEvent, _, cx| {
                        cx.notify();
                    }))
                    .px_10()
                    .py_4()
                    .gap_2()
                    .children(body),
            )
            .children(earlier_cue)
            .into_any_element()
    }

    /// What can only be known once the frame is laid out: where the current
    /// card lands, what the cue says and how wide the reader is. The host
    /// calls this from its render.
    pub(crate) fn after_layout<H: EmailReaderHost>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<H>,
    ) {
        if std::mem::take(&mut self.reader_note.reveal_current) {
            // A scroll reads the frame's bounds, so it waits for this frame to
            // lay the card out.
            cx.defer_in(window, |host, _, cx| {
                host.email_reader().reveal_current_message(cx)
            });
        }
        cx.defer_in(window, |host, _, cx| {
            let reader = host.email_reader();
            reader.sync_earlier_cue(cx);
            let width = reader.reader_scroll.bounds().size.width;
            if width != reader.reader_width {
                reader.reader_width = width;
                cx.notify();
            }
        });
    }
}

/// An email's whole subject when its sender's system cut the title short.
pub(crate) fn whole_title(item: &InboxItem) -> Option<String> {
    let body = without_leading_title(&item.body, &item.title);
    (item.source.as_deref() == Some(EMAIL_SOURCE))
        .then(|| full_title(&item.title, body))
        .flatten()
}

impl EmailReader {
    pub(crate) fn scroll<H: EmailReaderHost>(&mut self, amount: Pixels, cx: &mut Context<H>) {
        let offset = self.reader_scroll.offset();
        let max = self.reader_scroll.max_offset();
        let y = (offset.y - amount).clamp(-max.y, px(0.));
        self.reader_scroll.set_offset(point(offset.x, y));
        cx.notify();
    }

    /// Opens a message of the thread and brings it into view.
    pub(crate) fn open_message<H: EmailReaderHost>(&mut self, index: usize, cx: &mut Context<H>) {
        self.reader_note.current_message = index;
        self.reader_note.reveal_current = true;
        cx.notify();
    }

    pub(crate) fn step_message<H: EmailReaderHost>(&mut self, forward: bool, cx: &mut Context<H>) {
        let last = self
            .reader_note
            .thread
            .as_ref()
            .map_or(0, |thread| thread.messages.len().saturating_sub(1));
        let current = self.reader_note.current_message;
        let target = if forward {
            (current + 1).min(last)
        } else {
            current.saturating_sub(1)
        };
        if target != current {
            self.open_message(target, cx);
        }
    }

    /// Brings the current card to the top of the reader, under the earlier
    /// cue. A body takes its height once parsed, so while any is still parsing
    /// this runs again on the frame that lands it.
    pub(crate) fn reveal_current_message<H: EmailReaderHost>(&mut self, cx: &mut Context<H>) {
        let Some(thread) = self
            .reader_note
            .thread
            .as_ref()
            .filter(|thread| thread.messages.len() > 1)
        else {
            return;
        };
        let preamble = usize::from(!thread.preamble.is_empty());
        let current = self.reader_note.current_message;
        let viewport = self.reader_scroll.bounds();
        let landings: Option<Vec<Pixels>> = (0..=current)
            .map(|index| {
                let card = self.reader_scroll.bounds_for_item(index + preamble)?;
                Some((card.top() - viewport.top() - REVEAL_CLEARANCE).max(px(0.)))
            })
            .collect();
        let parsing = self
            .reader_note
            .markdown
            .values()
            .any(|markdown| markdown.read(cx).is_parsing());
        self.reader_note.reveal_current = parsing;
        let Some(landings) = landings else {
            return;
        };
        let farthest = self.reader_scroll.max_offset().y;
        let scrolled = landing(&landings, farthest, viewport.size.height);
        let offset = self.reader_scroll.offset();
        if offset.y != -scrolled {
            self.reader_scroll.set_offset(point(offset.x, -scrolled));
            // Looked at again on the next frame, until the layout holds still.
            self.reader_note.reveal_current = true;
            cx.notify();
        }
    }

    /// The width of everything the reader's rules span: its left padding and
    /// its column.
    pub(crate) fn rule_width(&self, window: &Window) -> Pixels {
        READER_MAX_WIDTH + READER_PADDING.to_pixels(window.rem_size())
    }

    /// Whether `text` takes at most `TITLE_LINES` lines at `width`. Before the
    /// first frame there's no width to say otherwise.
    pub(crate) fn fits_title_lines(
        &self,
        text: &str,
        size: Pixels,
        weight: FontWeight,
        width: Pixels,
        window: &Window,
        cx: &App,
    ) -> bool {
        wrapped_lines(text, size, weight, width, window, cx)
            .is_none_or(|lines| line_count(&lines) <= TITLE_LINES)
    }

    /// Whether a message's own words are short enough to lead its card at the
    /// larger size. Its folds don't count: they're drawn apart, at their own size.
    pub(crate) fn message_leads(&self, message: &EmailMessage, window: &Window, cx: &App) -> bool {
        let lines: Vec<&str> = message
            .blocks
            .iter()
            .filter_map(|block| match block {
                EmailBlock::Text(text) => Some(text.lines()),
                _ => None,
            })
            .flatten()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.len() > LEAD_LINES {
            return false;
        }
        let rem_size = window.rem_size();
        let width = (self.reader_width - READER_PADDING.to_pixels(rem_size) * 2.)
            .min(READER_MAX_WIDTH)
            - CARD_PADDING.to_pixels(rem_size) * 2.
            - px(2.);
        let size = TextSize::Default.rems(cx).to_pixels(rem_size);
        wrapped_lines(
            &lines.join("\n"),
            size,
            FontWeight::NORMAL,
            width,
            window,
            cx,
        )
        .is_none_or(|lines| line_count(&lines) <= LEAD_LINES)
    }

    /// The reader's title. A trailing `| id` never breaks across lines, and a
    /// title too long for its lines keeps it whole: the words are cut as late
    /// as the lines allow and the id follows the ellipsis. An id that begins a
    /// line needs no bar to part it from the words, and holds that line by a
    /// break, since dropping the bar could pull it back up.
    pub(crate) fn reader_title(&self, title: &str, window: &Window, cx: &App) -> String {
        let rem_size = window.rem_size();
        let width =
            (self.reader_width - READER_PADDING.to_pixels(rem_size) * 2.).min(READER_MAX_WIDTH);
        let size = TITLE_SIZE.rems().to_pixels(rem_size);
        let fits =
            |text: &str| self.fits_title_lines(text, size, FontWeight::SEMIBOLD, width, window, cx);
        let Some((subject, id)) = trailing_ticket(title) else {
            return title.to_string();
        };
        let on_its_own_line = |shown: String| {
            let starts_line = wrapped_lines(&shown, size, FontWeight::SEMIBOLD, width, window, cx)
                .is_some_and(|lines| {
                    lines
                        .first()
                        .and_then(|line| {
                            let boundary = line.wrap_boundaries().last()?;
                            let run = line.unwrapped_layout.runs.get(boundary.run_ix)?;
                            let glyph = run.glyphs.get(boundary.glyph_ix)?;
                            shown.get(glyph.index..)
                        })
                        .is_some_and(|rest| rest.trim_start().starts_with(TITLE_ID_SEPARATOR))
                });
            if starts_line {
                shown.replace(&format!(" {TITLE_ID_SEPARATOR}"), "\n")
            } else {
                shown
            }
        };
        let whole = format!("{subject} {TITLE_ID_SEPARATOR}{id}");
        if fits(&whole) {
            return on_its_own_line(whole);
        }
        let ends: Vec<usize> = subject.char_indices().map(|(end, _)| end).collect();
        let cut = |chars: usize| {
            let end = ends.get(chars).copied().unwrap_or(subject.len());
            format!("{}… {TITLE_ID_SEPARATOR}{id}", subject[..end].trim_end())
        };
        let (mut fitting, mut too_long) = (0, ends.len());
        while too_long - fitting > 1 {
            let middle = (fitting + too_long) / 2;
            if fits(&cut(middle)) {
                fitting = middle;
            } else {
                too_long = middle;
            }
        }
        on_its_own_line(cut(fitting))
    }

    pub(crate) fn markdown_style(&self, window: &Window, cx: &App) -> MarkdownStyle {
        let mut style = MarkdownStyle::themed(MarkdownFont::Agent, window, cx);
        style.base_text_style.font_size = px(15.).into();
        style.base_text_style.line_height = relative(1.65);
        style
    }

    /// The body of an email, smaller than a note's: it is scanned, not
    /// studied.
    pub(crate) fn reader_style(&self, window: &Window, cx: &App) -> ReaderStyle {
        let mut body = self.markdown_style(window, cx);
        if self.reader_note.thread.is_some() {
            body.base_text_style.font_size = px(14.).into();
            body.base_text_style.line_height = relative(1.5);
        }
        let muted = cx.theme().colors().text_muted;
        let folded_color = if cx.theme().appearance().is_light() {
            muted.opacity(SYSTEM_LINE_OPACITY_LIGHT)
        } else {
            muted
        };
        let mut folded = body.clone();
        folded.base_text_style.color = folded_color;
        // Markdown text takes its size from the container and its leading from
        // the paragraph; the base style alone reaches neither.
        folded.container_style.text.font_size = Some(TextSize::Small.rems(cx).into());
        folded.paragraph_line_height = FOLDED_LINE_HEIGHT.into();
        // Accent is for links in the new words.
        folded.link = TextStyleRefinement {
            color: Some(folded_color),
            underline: Some(UnderlineStyle {
                color: Some(folded_color),
                thickness: px(1.),
                ..Default::default()
            }),
            ..Default::default()
        };
        // `mark_attributions` makes these lines the folds' only headings.
        folded.heading_level_styles = None;
        folded.heading.text.color = Some(muted.opacity(ATTRIBUTION_OPACITY));
        folded.heading.text.font_weight = Some(FontWeight::NORMAL);
        folded.heading.text.font_size = Some(TextSize::Small.rems(cx).into());
        folded.heading.margin.top = Some(Length::Definite(ATTRIBUTION_TOP_MARGIN.into()));
        folded.heading.margin.bottom = Some(Length::Definite(ATTRIBUTION_BOTTOM_MARGIN.into()));
        let mut system = folded.clone();
        system.paragraph_spacing = px(4.);
        let quieter = muted.opacity(ATTRIBUTION_OPACITY);
        let mut folded_system = folded.clone();
        folded_system.base_text_style.color = quieter;
        folded_system.link.color = Some(quieter);
        folded_system.link.underline = Some(UnderlineStyle {
            color: Some(quieter),
            thickness: px(1.),
            ..Default::default()
        });
        let mut long_message = body.clone();
        long_message.container_style.text.font_size = Some(MESSAGE_TEXT_SIZE.into());
        long_message.paragraph_line_height = MESSAGE_LINE_HEIGHT.into();
        ReaderStyle {
            body,
            long_message,
            folded,
            folded_system,
            system,
            images: self.image_resolver(),
        }
    }

    /// Parses every block up front, folded ones too. `z` and `n` then open on
    /// text that has its height, so a card lands in one step instead of
    /// chasing parses as they finish.
    pub(crate) fn parse_thread_blocks<H: EmailReaderHost>(&mut self, cx: &mut Context<H>) {
        let Some(thread) = self.reader_note.thread.clone() else {
            return;
        };
        for (index, message) in thread.messages.iter().enumerate() {
            for (block_index, block) in message.blocks.iter().enumerate() {
                if let EmailBlock::Text(text)
                | EmailBlock::Meta(text)
                | EmailBlock::Fold { text, .. } = block
                {
                    self.markdown_for((index, block_index), text, cx);
                }
            }
        }
    }

    pub(crate) fn markdown_for<H: EmailReaderHost>(
        &mut self,
        key: (usize, usize),
        text: &str,
        cx: &mut Context<H>,
    ) -> Entity<Markdown> {
        self.reader_note
            .markdown
            .entry(key)
            .or_insert_with(|| {
                let source: SharedString = text.to_string().into();
                cx.new(|cx| Markdown::new(source, None, None, cx))
            })
            .clone()
    }

    pub(crate) fn render_markdown<H: EmailReaderHost>(
        &mut self,
        key: (usize, usize),
        text: &str,
        style: MarkdownStyle,
        images: &Rc<dyn Fn(&str) -> Option<ImageSource>>,
        cx: &mut Context<H>,
    ) -> AnyElement {
        let markdown = self.markdown_for(key, text, cx);
        let images = images.clone();
        MarkdownElement::new(markdown, style)
            .image_resolver(move |src| images(src))
            .into_any_element()
    }

    /// A card's fold row: chevron, key, then what this card holds. It closes
    /// the card, under whatever stayed on screen, so it sits in the same place
    /// on every card. That `z` reaches every fold of the note is the keys
    /// sheet's to say.
    /// A card's one fold control, the last thing in it open or closed. Its
    /// padding only pads the hover, so the chevron stands on the card's left
    /// edge.
    pub(crate) fn render_fold_row<H: EmailReaderHost>(
        &self,
        message: usize,
        label: String,
        window: &Window,
        cx: &mut Context<H>,
    ) -> AnyElement {
        let open = self.reader_note.folds_open;
        let keycap = framed_keycap(self.keycap(&ToggleInboxFolds, window, cx), false, cx);
        let hover = cx.theme().colors().element_hover;
        h_flex()
            .id(("inbox-fold", message))
            .debug_selector(move || format!("INBOX_FOLD-{message}"))
            .flex_none()
            .w_full()
            .gap_1p5()
            .ml_neg_1()
            .px_1()
            .py_0p5()
            .rounded_md()
            .hover(|style| style.bg(hover))
            .cursor_pointer()
            .child(keycap.debug_selector(move || format!("INBOX_FOLD_KEY-{message}")))
            .child(
                div().flex_1().min_w_0().child(
                    Label::new(label)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                ),
            )
            .tooltip(Tooltip::for_action_title(
                if open {
                    "Hide all folds"
                } else {
                    "Show all folds"
                },
                &ToggleInboxFolds,
            ))
            .on_click(cx.listener(|host: &mut H, _, _, cx| {
                host.email_reader().toggle_folds(cx);
            }))
            .into_any_element()
    }

    /// A record as a table, a field to a line, on the card's left edge with no
    /// ground of its own: one column, or for a long record as many as the
    /// reader has room for. It
    /// keeps the labels' tone so the message leads: only a changed value
    /// stands a step up, and accent is for a real link among the new words: a
    /// folded one is muted and underlined. A value holds one line until `z`
    /// opens it whole.
    pub(crate) fn render_fields(
        &self,
        id: &str,
        fields: &[&EmailField],
        dim: bool,
        cx: &App,
    ) -> AnyElement {
        let changed = cx.theme().colors().text.opacity(CHANGED_VALUE_OPACITY);
        let whole = self.reader_note.folds_open;
        let single_column = fields.len() <= FIELD_SINGLE_COLUMN_ROWS;
        h_flex()
            .w_full()
            .flex_wrap()
            .items_start()
            .gap_x_6()
            .gap_y_0p5()
            .line_height(relative(FIELD_LINE_HEIGHT))
            .children(fields.iter().enumerate().map(|(index, field)| {
                let text = field_text(&field.value);
                let change = field
                    .previous
                    .as_deref()
                    .and_then(|previous| list_change(text, previous));
                let is_change = change.is_some();
                let (shown, more) = match change {
                    Some(change) => (change, 0),
                    None => field_list(text, whole),
                };
                let value = Label::new(shown).size(LabelSize::Small).color(
                    if field.link.is_some() && !dim {
                        Color::Accent
                    } else if field.previous.is_some() && !dim {
                        Color::Custom(changed)
                    } else {
                        Color::Muted
                    },
                );
                let value = if field.link.is_some() && dim {
                    value.underline()
                } else {
                    value
                };
                let value = if whole { value } else { value.truncate() };
                let selector = format!("{id}-{index}");
                let previous = field
                    .previous
                    .as_ref()
                    .filter(|previous| !is_change && (whole || previous_helps(previous)));
                let mut value_cell = h_flex()
                    .id(SharedString::from(format!("inbox-field-{selector}")))
                    .flex_1()
                    .min_w_0()
                    .when(whole, |this| this.flex_wrap())
                    .child(div().min_w_0().child(value))
                    .when(more > 0, |this| {
                        let selector = selector.clone();
                        this.child(
                            div()
                                .debug_selector(move || format!("INBOX_MORE-{selector}"))
                                .flex_none()
                                .ml_1p5()
                                .child(
                                    Label::new(format!("+{more} more"))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .single_line(),
                                ),
                        )
                    })
                    .when_some(previous, |this, previous| {
                        let selector = selector.clone();
                        this.child(
                            div()
                                .debug_selector(move || format!("INBOX_CHANGED-{selector}"))
                                .when(!whole, |this| this.flex_none())
                                .child(
                                    Label::new(format!(" · was {}", was_text(previous, whole)))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        )
                    });
                if let Some(link) = field.link.clone() {
                    value_cell = value_cell
                        .cursor_pointer()
                        .tooltip(Tooltip::text(link.clone()))
                        .on_click(move |_, _, cx| cx.open_url(&link));
                } else if more > 0 || is_change {
                    value_cell = value_cell.tooltip(Tooltip::text(text.to_string()));
                }
                h_flex()
                    .flex_grow_1()
                    .map(|this| {
                        if single_column {
                            this.w_full()
                        } else {
                            this.flex_basis(FIELD_MIN_WIDTH)
                        }
                    })
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(move || format!("INBOX_FIELD_KEY-{selector}"))
                            .flex_none()
                            .w(FIELD_KEY_WIDTH)
                            .child(
                                Label::new(field.key.clone())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            ),
                    )
                    .child(value_cell)
            }))
            .into_any_element()
    }

    /// What heads a card's repeated text once its folds open.
    pub(crate) fn earlier_fold_heading(&self) -> &'static str {
        let is_ticket = self
            .item
            .as_ref()
            .map(|item| {
                let title = self.reader_note.title.as_deref();
                title.unwrap_or(&item.title)
            })
            .is_some_and(|title| ticket_prefix(title).is_some());
        if is_ticket {
            "Earlier in this ticket"
        } else {
            "Earlier in this thread"
        }
    }

    /// A message's new blocks as drawn, then its fold row saying what they
    /// fold, however many places that came from. Opened, everything folded
    /// follows the new words in its own order, at the fields' size, so they
    /// still lead the card, and the row stays last.
    pub(crate) fn render_message_blocks<H: EmailReaderHost>(
        &mut self,
        index: usize,
        message: &EmailMessage,
        style: &ReaderStyle,
        window: &Window,
        cx: &mut Context<H>,
    ) -> Vec<AnyElement> {
        let open = self.reader_note.folds_open;
        let text_style = if self.message_leads(message, window, cx) {
            &style.body
        } else {
            &style.long_message
        };
        let mut elements = Vec::new();
        let mut folded_elements = Vec::new();
        let mut folds = Vec::new();
        let mut joined = false;
        for (block_index, block) in message.blocks.iter().enumerate() {
            let key = (index, block_index);
            match block {
                EmailBlock::Text(text) => {
                    let text =
                        self.render_markdown(key, text, text_style.clone(), &style.images, cx);
                    elements.push(
                        div()
                            .debug_selector(move || format!("INBOX_TEXT-{index}-{block_index}"))
                            .child(text)
                            .into_any_element(),
                    );
                }
                EmailBlock::Meta(text) => {
                    elements.push(self.render_markdown(
                        key,
                        text,
                        style.system.clone(),
                        &style.images,
                        cx,
                    ));
                }
                EmailBlock::Fields { shown, unchanged } => {
                    let id = format!("{index}-{block_index}");
                    let (visible, folded) = split_fields(shown, unchanged);
                    if !visible.is_empty() {
                        elements.push(self.render_fields(&id, &visible, false, cx));
                    }
                    if !folded.is_empty() {
                        folds.push(FoldPiece::Fields);
                        if open {
                            folded_elements.push(self.render_fields(
                                &format!("{id}-same"),
                                &folded,
                                true,
                                cx,
                            ));
                        }
                    }
                }
                EmailBlock::Fold { kind, text, .. } => {
                    folds.push(FoldPiece::Text(*kind));
                    if open {
                        if *kind == FoldKind::Repeated && !joined {
                            joined = true;
                            folded_elements.push(
                                div()
                                    .debug_selector(move || format!("INBOX_FOLD_JOIN-{index}"))
                                    .child(
                                        Label::new(self.earlier_fold_heading())
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .into_any_element(),
                            );
                        }
                        let fold_style = if *kind == FoldKind::System {
                            &style.folded_system
                        } else {
                            &style.folded
                        };
                        let text =
                            self.render_markdown(key, text, fold_style.clone(), &style.images, cx);
                        folded_elements.push(
                            div()
                                .debug_selector(move || {
                                    format!("INBOX_FOLDED-{index}-{block_index}")
                                })
                                .pl_3()
                                .border_l_2()
                                .border_color(cx.theme().colors().border_variant)
                                .child(text)
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        elements.append(&mut folded_elements);
        if !folds.is_empty() {
            elements.push(self.render_fold_row(index, fold_label(open, &folds), window, cx));
        }
        elements
    }

    /// One message: a neutral card headed by who wrote it, with their colour
    /// on the badge alone, so a reply's start can't be missed and the words
    /// keep the most weight, whether it's one of many or the only one. While
    /// the folds are closed only the current message is open; the rest
    /// are their header, with a snippet where the address would be.
    pub(crate) fn render_message<H: EmailReaderHost>(
        &mut self,
        index: usize,
        message: &EmailMessage,
        total: usize,
        style: &ReaderStyle,
        window: &Window,
        cx: &mut Context<H>,
    ) -> AnyElement {
        let current = index == self.reader_note.current_message;
        let folds_open = self.reader_note.folds_open;
        let open = total < 2 || current || folds_open;
        let blocks = if open {
            self.render_message_blocks(index, message, style, window, cx)
        } else {
            Vec::new()
        };
        let colors = cx.theme().colors();
        let accent = sender_color(message.participant, cx.theme().appearance().is_light());
        let sender = sender_name(&message.from);
        let header_from = self.item.as_ref().and_then(|item| item.from.as_deref());
        let name = message_name(message);
        let detail = match &message.author {
            Some(_) => relayed_via(&sender, header_from),
            None => sender_address(&message.from),
        };
        let detail = if open {
            detail
        } else {
            snippet(message).or(detail)
        };
        // A lone message has no heading of its own to date it, and the note's
        // capture is when it came.
        let when = message
            .sent
            .map(|sent| sent.format("%b %-d, %H:%M").to_string())
            .or_else(|| {
                let captured = self.item.as_ref()?.captured.as_ref()?;
                (total < 2).then(|| captured_label(captured))
            });
        // The last card has no `n` to give, so it offers the way back, unless
        // the cue already does. Ahead of the time, so the time and `n of 4`
        // keep their columns.
        let last = index + 1 == total;
        let cue_steps_back = self.reader_note.messages_above > 0;
        let step_key = (current && total > 1 && !(last && cue_steps_back)).then(|| {
            let (action, word): (&dyn Action, _) = if last {
                (&PreviousInboxMessage, "previous")
            } else {
                (&NextInboxMessage, "next")
            };
            let filtering = self.keys_off;
            h_flex()
                .debug_selector(move || format!("INBOX_STEP_KEY-{index}"))
                .flex_none()
                .gap_1()
                .child(framed_keycap(
                    self.keycap(action, window, cx),
                    filtering,
                    cx,
                ))
                .child(
                    Label::new(word)
                        .size(LabelSize::Small)
                        .color(hint_color(filtering))
                        .single_line(),
                )
        });
        // What says where `n` starts from, or which card it left on when every
        // card is open.
        let reader_focused = self.reader_focus.is_focused(window);
        let ringed = current && total > 1 && (folds_open || reader_focused);
        let header = h_flex()
            .when(ringed && reader_focused, |this| {
                this.debug_selector(move || format!("INBOX_NEUTRAL_RING-{index}"))
            })
            .w_full()
            .gap_2()
            .pl(CARD_PADDING)
            .pr_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(SENDER_BADGE_SIZE)
                    .rounded_full()
                    .bg(accent.opacity(0.22))
                    .child(
                        Label::new(initials(&name))
                            .size(LabelSize::XSmall)
                            .weight(FontWeight::SEMIBOLD)
                            .color(Color::Custom(accent)),
                    ),
            )
            .child(
                div()
                    .when(ringed, |this| {
                        this.debug_selector(move || format!("INBOX_CARD_RING-{index}"))
                    })
                    .min_w_0()
                    .child(Label::new(name).weight(FontWeight::SEMIBOLD).truncate()),
            )
            .child(div().flex_1().min_w_0().children(detail.map(|detail| {
                Label::new(detail)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .truncate()
            })))
            .children(step_key)
            .children(when.map(|when| {
                div()
                    .debug_selector(move || format!("INBOX_CARD_WHEN-{index}"))
                    .flex_none()
                    .child(
                        Label::new(when)
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .single_line(),
                    )
            }))
            // A lone card holds these columns empty, so its date stands where
            // a thread's does.
            .child(
                div()
                    .when(total > 1, |this| {
                        this.debug_selector(move || format!("INBOX_POSITION-{index}"))
                    })
                    .flex_none()
                    .pl_2()
                    .child(
                        Label::new(format!("{} of {}", index + 1, total.max(2)))
                            .size(LabelSize::XSmall)
                            .color(if total < 2 {
                                Color::Custom(transparent_black())
                            } else if current {
                                Color::Default
                            } else {
                                Color::Muted
                            })
                            .single_line(),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .w(IconSize::XSmall.rems())
                    .when(total > 1, |this| {
                        this.debug_selector(move || format!("INBOX_CARD_CHEVRON-{index}"))
                            .child(
                                Icon::new(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                            )
                    }),
            );
        let border = if !ringed {
            colors.border
        } else if reader_focused {
            neutral_ring(cx)
        } else {
            colors.text_accent.opacity(0.4)
        };
        v_flex()
            .id(SharedString::from(format!("inbox-message-{index}")))
            .debug_selector(move || format!("INBOX_MESSAGE-{index}"))
            .flex_none()
            .w_full()
            .max_w(READER_MAX_WIDTH)
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(border)
            .bg(colors.panel_background)
            .child(header)
            .when(open, |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .gap_3()
                        .px(CARD_PADDING)
                        .py_3()
                        .children(blocks),
                )
            })
            .when(!open, |this| {
                this.cursor_pointer()
                    .on_click(cx.listener(move |host: &mut H, _, _, cx| {
                        host.email_reader().open_message(index, cx)
                    }))
            })
            .into_any_element()
    }

    /// The note's body as the reader's children, one per message.
    pub(crate) fn render_note_body<H: EmailReaderHost>(
        &mut self,
        style: &ReaderStyle,
        window: &Window,
        cx: &mut Context<H>,
    ) -> Vec<AnyElement> {
        let Some(thread) = self.reader_note.thread.clone() else {
            let Some(item) = self.item.clone() else {
                return Vec::new();
            };
            let body = without_leading_title(&item.body, &item.title);
            let body = self.render_markdown((0, 0), body, style.body.clone(), &style.images, cx);
            return vec![
                div()
                    .w_full()
                    .max_w(READER_MAX_WIDTH)
                    .child(body)
                    .into_any_element(),
            ];
        };
        let total = thread.messages.len();
        let preamble = (!thread.preamble.is_empty()).then(|| {
            // Past the messages' keys, which count from zero.
            let key = (total, 0);
            let preamble =
                self.render_markdown(key, &thread.preamble, style.body.clone(), &style.images, cx);
            div()
                .w_full()
                .max_w(READER_MAX_WIDTH)
                .child(preamble)
                .into_any_element()
        });
        preamble
            .into_iter()
            .chain(thread.messages.iter().enumerate().map(|(index, message)| {
                self.render_message(index, message, total, style, window, cx)
            }))
            .collect()
    }

    /// Where the cue line falls in the thread, by the last frame's layout: how
    /// many cards end above it, and the card running on under it once its
    /// header has gone. The card being read isn't one of its own earlier
    /// messages, however far down it the reader is.
    pub(crate) fn messages_above(&self) -> (usize, Option<usize>) {
        let Some(thread) = self
            .reader_note
            .thread
            .as_ref()
            .filter(|thread| thread.messages.len() > 1)
        else {
            return (0, None);
        };
        let scrolled = -self.reader_scroll.offset().y;
        if scrolled <= px(0.) {
            return (0, None);
        }
        let preamble = usize::from(!thread.preamble.is_empty());
        let cue_line = self.reader_scroll.bounds().top() + scrolled + EARLIER_CUE_HEIGHT;
        let card = |index: usize| self.reader_scroll.bounds_for_item(index + preamble);
        // Up to the pixel the layout snaps to: a landed card's neighbour ends
        // right on the line.
        let above = (0..thread.messages.len())
            .take_while(|index| card(*index).is_some_and(|card| card.bottom() <= cue_line + px(1.)))
            .count();
        let under = card(above)
            .is_some_and(|card| card.top() < cue_line)
            .then_some(above);
        (above, under)
    }

    /// Brings the cue up to the frame just painted. Layout can move the scroll
    /// after render has read it (a taller reader clamps the offset, a parsed
    /// body grows a card), and only the finished frame knows what's above.
    pub(crate) fn sync_earlier_cue<H: EmailReaderHost>(&mut self, cx: &mut Context<H>) {
        let (above, under) = self.messages_above();
        if (above, under)
            != (
                self.reader_note.messages_above,
                self.reader_note.card_under_cue,
            )
        {
            self.reader_note.messages_above = above;
            self.reader_note.card_under_cue = under;
            cx.notify();
        }
    }

    /// What the cue says of the card under it, if that card's header has gone.
    pub(crate) fn carried_card(&self) -> Option<String> {
        let index = self.reader_note.card_under_cue?;
        let thread = self.reader_note.thread.as_ref()?;
        let message = thread.messages.get(index)?;
        Some(carried_label(
            &message_name(message),
            index,
            thread.messages.len(),
        ))
    }

    /// Solid, so a card scrolled half out of view ends at a clean edge. It
    /// says what's up there and the key that goes there, and at its right end
    /// carries the header of a card that's scrolled away.
    pub(crate) fn render_earlier_cue<H: EmailReaderHost>(
        &self,
        earlier: usize,
        carried: Option<String>,
        window: &Window,
        cx: &mut Context<H>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let ground = colors.editor_background;
        let rem_size = window.rem_size();
        let inset = READER_PADDING.to_pixels(rem_size) + CARD_PADDING.to_pixels(rem_size) + px(1.);
        let keycap = (earlier > 0)
            .then(|| framed_keycap(self.keycap(&PreviousInboxMessage, window, cx), false, cx));
        h_flex()
            .id("inbox-earlier")
            .debug_selector(|| "INBOX_EARLIER".to_string())
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(EARLIER_CUE_HEIGHT)
            .pr_10()
            .bg(ground)
            .cursor_pointer()
            .child(
                div()
                    .debug_selector(|| "INBOX_EARLIER_FADE".to_string())
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(EARLIER_CUE_HEIGHT)
                    .h(CUE_FADE_HEIGHT)
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(ground, 0.25),
                        linear_color_stop(ground.opacity(0.), 1.),
                    )),
            )
            .child(
                h_flex()
                    .w_full()
                    .max_w(self.rule_width(window))
                    .h_full()
                    .pl(inset)
                    .pr_4()
                    .gap_1p5()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .children(keycap)
                    .when(earlier > 0, |this| {
                        this.child(
                            Label::new(previous_label(earlier))
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .single_line(),
                        )
                    })
                    .children(carried.map(|carried| {
                        div()
                            .debug_selector(|| "INBOX_CUE_CARD".to_string())
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .justify_end()
                            .child(
                                Label::new(carried)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                    })),
            )
            .on_click(cx.listener(|host: &mut H, _, _, cx| {
                host.email_reader()
                    .reader_scroll
                    .set_offset(point(px(0.), px(0.)));
                cx.notify();
            }))
            .into_any_element()
    }
}

/// A binding as a keycap. Vim's text style spells keys out, and an arrow reads
/// better as its glyph.
pub(crate) fn binding_keycap(binding: &gpui::KeyBinding, cx: &App) -> KeyBinding {
    KeyBinding::from_keystrokes(
        binding.keystrokes().into(),
        vim_mode(cx) && !is_arrow(binding),
    )
}

pub(crate) fn is_arrow(binding: &gpui::KeyBinding) -> bool {
    matches!(binding.keystrokes(), [keystroke]
        if matches!(keystroke.key(), "up" | "down" | "left" | "right")
            && !keystroke.modifiers().modified())
}

pub(crate) fn vim_mode(cx: &App) -> bool {
    cx.global::<SettingsStore>()
        .merged_settings()
        .vim_mode
        .unwrap_or(false)
}

/// An action on the note rather than a place for it, so it sits in a header.
/// While its key is off the keycap and the words dim as one hint; a click
/// still works.
pub(crate) fn quiet_action(
    name: &'static str,
    action: &dyn Action,
    keycap: KeyBinding,
    label: &'static str,
    dimmed: bool,
    focus: &FocusHandle,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let focus = focus.clone();
    let boxed_action = action.boxed_clone();
    div()
        .id(SharedString::from(format!("inbox-action-wrap-{name}")))
        .debug_selector(move || format!("INBOX_ACTION-{name}"))
        .flex_none()
        .child(
            ButtonLike::new(SharedString::from(format!("inbox-action-{name}")))
                .child(
                    h_flex()
                        .when(dimmed, |this| {
                            this.debug_selector(move || format!("INBOX_DIMMED-{name}"))
                        })
                        .gap_1()
                        .child(framed_keycap(keycap, dimmed, cx))
                        .child(
                            Label::new(label)
                                .size(LabelSize::Small)
                                .color(hint_color(dimmed)),
                        ),
                )
                .on_click(move |_, window, cx| {
                    focus.dispatch_action(boxed_action.as_ref(), window, cx)
                }),
        )
}
