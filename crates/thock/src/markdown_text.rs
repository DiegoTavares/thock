//! Inline Markdown for panel rows. A vault's task lines are Markdown, so
//! `[name](url)` and `[[wikilinks]]` should read as links in a pane, and
//! `~~text~~` as struck through, instead of as syntax — but the file keeps the
//! raw text, and so does the inline editor.
//! Parsing delegates to `markdown_syntax`, so a pane and the Markdown editor
//! agree on what a link is; the colours below are the same ones the editor
//! paints (spec V10 §7.1), minus the underline a dense row can't afford.

use gpui::{
    AnyElement, App, ElementId, Entity, HighlightStyle, Hsla, InteractiveText, IntoElement as _,
    SharedString, StrikethroughStyle, StyledText, TaskExt as _, WeakEntity, px,
};
use project::{Project, ProjectPath};
use ui::ActiveTheme as _;
use util::ResultExt as _;
use workspace::Workspace;

use crate::markdown_conceal::resolve_wikilink_target;
use crate::markdown_syntax;

/// A run of a line as it should display: literal or linked text, struck
/// through or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSpan {
    pub text: String,
    /// Where the run points, when it is a link label.
    pub target: Option<LinkTarget>,
    /// Whether the run sits inside a `~~strikethrough~~`.
    pub struck: bool,
}

/// Where an inline link points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// An absolute URL, handed to the system handler.
    Url(String),
    /// A note named by a `[[wikilink]]` or by a relative destination,
    /// resolved against the vault when clicked.
    Note(String),
}

/// A construct that displays as something other than its source: a link,
/// shown as its label, or a `~~` delimiter, shown as nothing.
enum Atom {
    Link {
        label: std::ops::Range<usize>,
        target: LinkTarget,
    },
    StrikeOpen,
    StrikeClose,
}

/// Splits `text` into the runs a row displays. Anything that isn't a
/// well-formed construct stays literal, so a half-typed bracket renders as
/// exactly what the user typed.
pub fn parse_inline_spans(text: &str) -> Vec<InlineSpan> {
    let mut atoms = Vec::new();
    for link in markdown_syntax::inline_links(text) {
        let Some(target) = link_target(text, &link) else {
            // Nothing to point at — leave the construct as literal text.
            continue;
        };
        atoms.push((
            link.range,
            Atom::Link {
                label: link.label,
                target,
            },
        ));
    }
    for run in markdown_syntax::inline_strikethroughs(text) {
        atoms.push((run.range.start..run.text.start, Atom::StrikeOpen));
        atoms.push((run.text.end..run.range.end, Atom::StrikeClose));
    }
    // Links and delimiters never overlap, so start order is a total order.
    atoms.sort_by_key(|(range, _)| range.start);

    let mut spans = Vec::new();
    let mut literal_start = 0;
    let mut struck = false;
    for (range, atom) in atoms {
        if literal_start < range.start {
            spans.push(InlineSpan {
                text: text[literal_start..range.start].to_string(),
                target: None,
                struck,
            });
        }
        match atom {
            Atom::Link { label, target } => spans.push(InlineSpan {
                text: text[label].to_string(),
                target: Some(target),
                struck,
            }),
            Atom::StrikeOpen => struck = true,
            Atom::StrikeClose => struck = false,
        }
        literal_start = range.end;
    }
    if literal_start < text.len() {
        spans.push(InlineSpan {
            text: text[literal_start..].to_string(),
            target: None,
            struck,
        });
    }
    spans
}

fn link_target(text: &str, link: &markdown_syntax::InlineLink) -> Option<LinkTarget> {
    if let Some(target) = &link.wikilink_target {
        return Some(LinkTarget::Note(text[target.clone()].to_string()));
    }
    // A title (`[name](url "title")`) has nowhere to go in a one-line row, so
    // the destination is the first word and the title is dropped.
    let destination = text[link.destination.clone()?].split_whitespace().next()?;
    Some(if is_absolute_url(destination) {
        LinkTarget::Url(destination.to_string())
    } else {
        LinkTarget::Note(destination.to_string())
    })
}

fn is_absolute_url(url: &str) -> bool {
    let Some(separator) = url.find(':') else {
        return false;
    };
    let scheme = &url[..separator];
    scheme.starts_with(|character: char| character.is_ascii_alphabetic())
        && scheme.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// The colour a `[[wikilink]]` label takes in the Markdown editor.
pub fn wikilink_color(cx: &App) -> Hsla {
    cx.theme().colors().text_accent
}

/// The colour a `[name](url)` label takes in the Markdown editor.
pub fn external_link_color(cx: &App) -> Hsla {
    cx.theme()
        .syntax()
        .style_for_name("link_uri")
        .and_then(|style| style.color)
        .unwrap_or_else(|| cx.theme().colors().text_accent)
}

/// One row of Markdown rendered for a panel: link labels lose their syntax,
/// take the editor's link colours, and open on click, and `~~struck~~` text
/// loses its delimiters and takes the line. Returns the text element only —
/// the caller wraps it in the `Label`/`LabelLike` that carries the row's
/// size, colour and truncation.
pub fn render_markdown_row(
    id: ElementId,
    text: &str,
    project: &Entity<Project>,
    workspace: &WeakEntity<Workspace>,
    cx: &App,
) -> AnyElement {
    let spans = parse_inline_spans(text);
    if spans
        .iter()
        .all(|span| span.target.is_none() && !span.struck)
    {
        return SharedString::from(text.to_string()).into_any_element();
    }
    let mut display = String::new();
    let mut highlights = Vec::new();
    let mut link_ranges = Vec::new();
    let mut targets = Vec::new();
    for span in spans {
        let start = display.len();
        display.push_str(&span.text);
        let range = start..display.len();
        let color = span.target.as_ref().map(|target| match target {
            LinkTarget::Url(_) => external_link_color(cx),
            LinkTarget::Note(_) => wikilink_color(cx),
        });
        if color.is_some() || span.struck {
            highlights.push((
                range.clone(),
                HighlightStyle {
                    color,
                    strikethrough: span.struck.then(|| StrikethroughStyle {
                        thickness: px(1.),
                        color: None,
                    }),
                    ..Default::default()
                },
            ));
        }
        if let Some(target) = span.target {
            link_ranges.push(range);
            targets.push(target);
        }
    }
    if link_ranges.is_empty() {
        return StyledText::new(display)
            .with_highlights(highlights)
            .into_any_element();
    }
    let project = project.downgrade();
    let workspace = workspace.clone();
    InteractiveText::new(id, StyledText::new(display).with_highlights(highlights))
        .on_click(link_ranges, move |index, window, cx| {
            let followed = match targets.get(index) {
                Some(LinkTarget::Url(url)) => {
                    cx.open_url(url);
                    true
                }
                Some(LinkTarget::Note(target)) => project
                    .upgrade()
                    .and_then(|project| note_project_path(&project, target, cx))
                    .map(|path| {
                        // Deferred: the click is dispatched from inside the
                        // panel's element tree, and opening reaches back into
                        // the workspace that may already be leased.
                        let workspace = workspace.clone();
                        window.defer(cx, move |window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    workspace
                                        .open_path(path, None, true, window, cx)
                                        .detach_and_log_err(cx);
                                })
                                .log_err();
                        });
                    })
                    .is_some(),
                None => false,
            };
            // Without this the row's own click handler fires too, and the
            // panel acts on the row the user just navigated away from. A link
            // that led nowhere falls through, so the row still responds.
            if followed {
                cx.stop_propagation();
            }
        })
        .into_any_element()
}

/// The vault file a note link names, resolved against the project's visible
/// worktrees. `None` when nothing matches — a link to a note that doesn't
/// exist does nothing rather than creating a file.
fn note_project_path(project: &Entity<Project>, target: &str, cx: &App) -> Option<ProjectPath> {
    let project = project.read(cx);
    project.visible_worktrees(cx).find_map(|worktree| {
        let worktree = worktree.read(cx);
        let path = resolve_wikilink_target(
            target,
            worktree.files(false, 0).map(|entry| entry.path.as_ref()),
        )?;
        Some(ProjectPath {
            worktree_id: worktree.id(),
            path: path.into(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> InlineSpan {
        InlineSpan {
            text: value.to_string(),
            target: None,
            struck: false,
        }
    }

    fn url(value: &str, url: &str) -> InlineSpan {
        InlineSpan {
            text: value.to_string(),
            target: Some(LinkTarget::Url(url.to_string())),
            struck: false,
        }
    }

    fn note(value: &str, target: &str) -> InlineSpan {
        InlineSpan {
            text: value.to_string(),
            target: Some(LinkTarget::Note(target.to_string())),
            struck: false,
        }
    }

    fn struck(span: InlineSpan) -> InlineSpan {
        InlineSpan {
            struck: true,
            ..span
        }
    }

    #[test]
    fn plain_text_is_one_span() {
        assert_eq!(
            parse_inline_spans("Review the planner"),
            vec![text("Review the planner")]
        );
    }

    #[test]
    fn splits_text_around_links() {
        assert_eq!(
            parse_inline_spans("See [chat](https://chat.example.com/room) before Friday"),
            vec![
                text("See "),
                url("chat", "https://chat.example.com/room"),
                text(" before Friday"),
            ]
        );
    }

    #[test]
    fn parses_adjacent_links() {
        assert_eq!(
            parse_inline_spans("[a](https://a.example)[b](mailto:b@example.com)"),
            vec![
                url("a", "https://a.example"),
                url("b", "mailto:b@example.com"),
            ]
        );
    }

    #[test]
    fn allows_balanced_parens_in_the_url() {
        assert_eq!(
            parse_inline_spans("[wiki](https://example.com/Foo_(bar))!"),
            vec![url("wiki", "https://example.com/Foo_(bar)"), text("!")]
        );
    }

    #[test]
    fn drops_a_link_title() {
        assert_eq!(
            parse_inline_spans("[a](https://a.example \"Title\")"),
            vec![url("a", "https://a.example")]
        );
    }

    #[test]
    fn wikilinks_become_note_links() {
        assert_eq!(
            parse_inline_spans("Pay [[2026-08-18-invoice]] today"),
            vec![
                text("Pay "),
                note("2026-08-18-invoice", "2026-08-18-invoice"),
                text(" today"),
            ]
        );
    }

    #[test]
    fn a_wikilink_alias_shows_the_alias_and_opens_the_target() {
        assert_eq!(
            parse_inline_spans("[[projects/thock|the app]]"),
            vec![note("the app", "projects/thock")]
        );
    }

    #[test]
    fn a_relative_destination_is_a_note_link() {
        assert_eq!(
            parse_inline_spans("[note](daily/2026-08-17.md)"),
            vec![note("note", "daily/2026-08-17.md")]
        );
    }

    #[test]
    fn malformed_links_stay_literal() {
        for line in [
            "an [unclosed link",
            "[no parens] here",
            "[empty]()",
            "[](https://a.example)",
            "[[unclosed",
            "[[]]",
            "![[embedded]]",
            "![alt](https://a.example/x.png)",
        ] {
            assert_eq!(parse_inline_spans(line), vec![text(line)], "{line}");
        }
    }

    #[test]
    fn a_malformed_link_before_a_good_one_stays_literal() {
        assert_eq!(
            parse_inline_spans("[empty]() then [[note]]"),
            vec![text("[empty]() then "), note("note", "note")]
        );
    }

    #[test]
    fn strikethrough_drops_its_delimiters_and_marks_the_run() {
        assert_eq!(
            parse_inline_spans("Skip ~~the standup~~ today"),
            vec![
                text("Skip "),
                struck(text("the standup")),
                text(" today"),
            ]
        );
    }

    #[test]
    fn a_struck_link_stays_a_link() {
        assert_eq!(
            parse_inline_spans("~~read [[notes/spec]] first~~"),
            vec![
                struck(text("read ")),
                struck(note("notes/spec", "notes/spec")),
                struck(text(" first")),
            ]
        );
    }

    #[test]
    fn malformed_strikethroughs_stay_literal() {
        for line in ["~~unclosed", "~~ padded ~~", "a ~ b"] {
            assert_eq!(parse_inline_spans(line), vec![text(line)], "{line}");
        }
    }

    #[test]
    fn a_bare_url_is_not_a_link() {
        assert_eq!(
            parse_inline_spans("Fill https://example.com/sheet"),
            vec![text("Fill https://example.com/sheet")]
        );
    }
}
