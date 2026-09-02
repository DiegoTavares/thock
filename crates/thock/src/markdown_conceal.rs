//! Concealed markup in the Markdown editor (spec V10). While the cursor is
//! elsewhere, a vault note's markup renders the way it would in preview:
//! heading markers, link syntax, `~~` delimiters and HTML comments are folded
//! away behind an invisible placeholder, headings and link labels are
//! coloured, struck text takes its line, a task
//! list's `[ ]` draws as a checkbox, and a `___` line as a rule. Putting the cursor on a line restores that whole line's
//! source (§4.2). Everything here is display-only — folds and highlights live
//! in the `DisplayMap` and no code path writes to the buffer (§4.3).

use crate::markdown_syntax::{self, SpanKind};
use crate::markdown_text;
use crate::vault::{Vault, VaultStatus};
use editor::actions::GoToDefinition;
use editor::display_map::{Crease, CreaseId};
use editor::{Editor, EditorEvent, EditorMode, FoldPlaceholder, HighlightKey};
use gpui::{
    App, AppContext as _, Context, Empty, Entity, HighlightStyle, Hsla, IntoElement as _,
    ParentElement as _, SharedString, StrikethroughStyle, Styled as _, Subscription, Task,
    TaskExt as _, WeakEntity, Window, div, px,
};
use multi_buffer::{
    Anchor, MultiBufferOffset, MultiBufferPoint, MultiBufferSnapshot, ToOffset as _, ToPoint as _,
};
use project::ProjectPath;
use settings::SettingsStore;
use std::any::TypeId;
use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use ui::{ActiveTheme as _, Color, Icon, IconName, IconSize};
use util::paths::PathStyle;
use util::rel_path::RelPath;
use workspace::Workspace;

gpui::actions!(
    thock,
    [
        /// Shows or hides the Markdown markup in the current note.
        ToggleMarkdownSource,
        /// Shows or hides the email view on a synced email note.
        ToggleEmailView,
        /// Collapses or expands the email reply under the cursor.
        ToggleMessage,
        /// Moves to the next reply in an email note.
        NextMessage,
        /// Moves to the previous reply in an email note.
        PreviousMessage
    ]
);

const REPARSE_DEBOUNCE: Duration = Duration::from_millis(50);

/// Tags the conceal folds so removal never touches the user's own folds and
/// theirs never diff as ours (§10.2).
struct ConcealFoldTag;

fn fold_type_tag() -> TypeId {
    TypeId::of::<ConcealFoldTag>()
}

/// Tags the email view's reply and quote folds (V16 §5.3). A separate tag
/// from `ConcealFoldTag` so the conceal diff never removes a collapsed reply
/// and the reveal rule never pops one open.
struct EmailFoldTag;

fn email_fold_type_tag() -> TypeId {
    TypeId::of::<EmailFoldTag>()
}

/// A scanned span re-anchored into the buffer so it survives edits between
/// reparses.
struct AnchorSpan {
    range: Range<Anchor>,
    kind: SpanKind,
}

/// One message's crease geometry, anchored (V16 §5.3).
struct MessageAnchors {
    header_start: Anchor,
    body: Range<Anchor>,
}

/// The email-view half of the plan: present only when the buffer parses as
/// an email note and the view is enabled.
struct EmailAnchors {
    messages: Vec<MessageAnchors>,
    quotes: Vec<Range<Anchor>>,
}

/// What the addon knows about the buffer's vault at install time.
#[derive(Clone, Default)]
pub struct ConcealSettings {
    pub conceal: bool,
    pub email_view: bool,
    /// The connected Google account, for the own-reply tint (V16 §6).
    pub account: Option<String>,
}

pub struct MarkdownConcealAddon {
    enabled: bool,
    email_enabled: bool,
    account: Option<String>,
    spans: Vec<AnchorSpan>,
    email: Option<EmailAnchors>,
    crease_ids: Vec<CreaseId>,
    /// The collapsed-by-default state is imposed once per open (V16 §5.3);
    /// after that the user's toggles are law.
    default_folds_applied: bool,
    reparse: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl editor::Addon for MarkdownConcealAddon {
    fn extend_key_context(&self, key_context: &mut gpui::KeyContext, _: &App) {
        key_context.add("ThockMarkdownConceal");
    }

    fn to_any(&self) -> &dyn std::any::Any {
        self
    }

    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

pub fn init(cx: &mut App) {
    cx.observe_new(|editor: &mut Editor, _, cx| register(editor, cx))
        .detach();
}

/// Installs the addon on editors where conceal applies: a full, writable,
/// singleton editor over a `.md` file that lives under a Thock vault root
/// (§4.4). Every other editor — a README in a code repo, pickers, minimaps —
/// is left exactly as it is today.
fn register(editor: &mut Editor, cx: &mut Context<Editor>) {
    if !editor.mode().is_full()
        || matches!(editor.mode(), EditorMode::Minimap { .. })
        || editor.read_only(cx)
    {
        return;
    }
    if let Some(settings) = vault_markdown_settings(editor, cx) {
        install(editor, settings, cx);
        return;
    }
    // The buffer may acquire a qualifying file later — an untitled buffer
    // saved as `note.md` into a vault, or a file renamed to `.md` — so retry
    // the file gate whenever the file handle changes.
    cx.subscribe(&cx.entity(), |editor, _, event: &EditorEvent, cx| {
        if !matches!(event, EditorEvent::FileHandleChanged) {
            return;
        }
        if editor.addon::<MarkdownConcealAddon>().is_some() {
            return;
        }
        if let Some(settings) = vault_markdown_settings(editor, cx) {
            install(editor, settings, cx);
        }
    })
    .detach();
}

/// Whether the editor's buffer is a `.md` file under a Thock vault root,
/// returning the vault's markdown settings when it is. The account read is
/// blocking config I/O, same as the sync services' reloads — done once per
/// qualifying editor, and only when the email view could use it.
fn vault_markdown_settings(editor: &Editor, cx: &App) -> Option<ConcealSettings> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let file = buffer.read(cx).file()?;
    if file.path().extension() != Some("md") {
        return None;
    }
    let file = project::File::from_dyn(Some(file))?;
    let vault_root = file.worktree.read(cx).abs_path();
    match Vault::detect(&vault_root) {
        VaultStatus::Valid(vault) => {
            let email_view = vault.config.markdown.email_view;
            let account = email_view
                .then(|| {
                    crate::google_auth::resolve_google_settings(
                        &vault_root,
                        crate::gmail::GMAIL_CONFIG_FILE,
                    )
                    .account
                })
                .flatten();
            Some(ConcealSettings {
                conceal: vault.config.markdown.conceal,
                email_view,
                account,
            })
        }
        _ => None,
    }
}

/// Installs the addon and its subscriptions on an editor that passed the
/// vault gate. Split from `register` so tests can drive an editor without a
/// vault on the real filesystem.
fn install(editor: &mut Editor, settings: ConcealSettings, cx: &mut Context<Editor>) {
    let mut subscriptions = Vec::new();
    subscriptions.push(cx.subscribe(
        &cx.entity(),
        |editor, _, event: &EditorEvent, cx| match event {
            EditorEvent::BufferEdited => schedule_reparse(editor, cx),
            EditorEvent::SelectionsChanged { .. } => apply_folds(editor, cx),
            _ => {}
        },
    ));
    // A user fold operation (`unfold_at`, `zR`, a gutter click) sweeps
    // intersecting conceal folds away with it and notifies the editor —
    // the display map itself never notifies. Heal on the editor's own
    // notify rather than waiting for the next cursor move; this terminates
    // because a healed apply is a no-op that doesn't notify again.
    subscriptions.push(cx.observe(&cx.entity(), |editor, _, cx| apply_folds(editor, cx)));
    // Highlight styles capture theme colours, so a theme change needs a
    // re-apply to pick up the new palette.
    subscriptions
        .push(cx.observe_global::<SettingsStore>(|editor, cx| apply_highlights(editor, cx)));

    let editor_handle = cx.weak_entity();
    subscriptions.push(
        editor.register_action::<ToggleMarkdownSource>(move |_, _, cx| {
            editor_handle
                .update(cx, |editor, cx| toggle(editor, cx))
                .ok();
        }),
    );
    // Registered before the built-in `go_to_definition` (custom editor
    // actions are installed first at paint), so this listener sees the
    // action first and propagates it when the cursor isn't on a wikilink.
    let editor_handle = cx.weak_entity();
    subscriptions.push(editor.register_action::<GoToDefinition>(
        move |_, window, cx| go_to_wikilink(&editor_handle, window, cx),
    ));
    let editor_handle = cx.weak_entity();
    subscriptions.push(editor.register_action::<ToggleEmailView>(move |_, _, cx| {
        editor_handle
            .update(cx, |editor, cx| toggle_email_view(editor, cx))
            .ok();
    }));
    let editor_handle = cx.weak_entity();
    subscriptions.push(editor.register_action::<ToggleMessage>(move |_, _, cx| {
        editor_handle
            .update(cx, |editor, cx| toggle_message(editor, cx))
            .ok();
    }));
    let editor_handle = cx.weak_entity();
    subscriptions.push(editor.register_action::<NextMessage>(move |_, window, cx| {
        editor_handle
            .update(cx, |editor, cx| move_to_message(editor, true, window, cx))
            .ok();
    }));
    let editor_handle = cx.weak_entity();
    subscriptions.push(editor.register_action::<PreviousMessage>(move |_, window, cx| {
        editor_handle
            .update(cx, |editor, cx| move_to_message(editor, false, window, cx))
            .ok();
    }));

    editor.register_addon(MarkdownConcealAddon {
        enabled: settings.conceal,
        email_enabled: settings.email_view,
        account: settings.account,
        spans: Vec::new(),
        email: None,
        crease_ids: Vec::new(),
        default_folds_applied: false,
        reparse: Task::ready(()),
        _subscriptions: subscriptions,
    });
    schedule_reparse(editor, cx);
}

/// Makes `[[wikilinks]]` act like code references: go-to-definition with the
/// newest cursor on one opens the linked note. Anywhere else the action
/// propagates to the built-in handler, so nothing changes for it. A wikilink
/// whose target doesn't resolve does nothing — no file is created.
fn go_to_wikilink(editor: &WeakEntity<Editor>, window: &mut Window, cx: &mut App) {
    let Some(editor) = editor.upgrade() else {
        cx.propagate();
        return;
    };
    // Outer `None`: the cursor isn't on a wikilink — fall through to the
    // built-in. Inner `None`: a wikilink that doesn't resolve — swallow.
    let destination = editor.update(cx, |editor, cx| {
        let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
        let offset = editor
            .selections
            .newest_anchor()
            .head()
            .to_offset(&buffer_snapshot);
        let text = buffer_snapshot.text();
        let reference = markdown_syntax::wikilink_at(&text, offset.0)?;
        let target = text.get(reference.target)?;
        Some(wikilink_destination(editor, target, cx))
    });
    match destination {
        None => cx.propagate(),
        Some(None) => {}
        Some(Some((workspace, project_path))) => {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.open_path(project_path, None, true, window, cx)
                })
                .detach_and_log_err(cx);
        }
    }
}

/// The workspace and project path a wikilink target opens, resolved against
/// the buffer's worktree snapshot — in memory, no disk IO.
fn wikilink_destination(
    editor: &Editor,
    target: &str,
    cx: &App,
) -> Option<(Entity<Workspace>, ProjectPath)> {
    let buffer = editor.buffer().read(cx).as_singleton()?;
    let file = project::File::from_dyn(buffer.read(cx).file())?;
    let worktree = file.worktree.read(cx);
    let path = resolve_wikilink_target(
        target,
        worktree.files(false, 0).map(|entry| entry.path.as_ref()),
    )?;
    let project_path = ProjectPath {
        worktree_id: worktree.id(),
        path: path.into(),
    };
    Some((editor.workspace()?, project_path))
}

/// Resolves a wikilink target against the vault's file list: an exact
/// vault-relative path (as written, or with `.md` appended) wins, otherwise
/// the first file anywhere in the vault the target names — Obsidian-style
/// basename linking.
pub(crate) fn resolve_wikilink_target<'a>(
    target: &str,
    files: impl Iterator<Item = &'a RelPath>,
) -> Option<&'a RelPath> {
    let exact = RelPath::new(Path::new(target), PathStyle::Unix)
        .ok()
        .map(Cow::into_owned);
    let with_extension = RelPath::new(Path::new(&format!("{target}.md")), PathStyle::Unix)
        .ok()
        .map(Cow::into_owned);
    let mut extension_match = None;
    let mut name_match = None;
    for path in files {
        if exact.as_deref() == Some(path) {
            return Some(path);
        }
        if extension_match.is_none() && with_extension.as_deref() == Some(path) {
            extension_match = Some(path);
        }
        // An extensionless target denotes a note, so a bare stem only
        // matches `.md` files; any other file stays reachable by writing
        // its full name with the extension (`[[scan.pdf]]`).
        if name_match.is_none()
            && (path.file_name() == Some(target)
                || (path.extension() == Some("md") && path.file_stem() == Some(target)))
        {
            name_match = Some(path);
        }
    }
    extension_match.or(name_match)
}

fn toggle(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon_mut::<MarkdownConcealAddon>() else {
        return;
    };
    addon.enabled = !addon.enabled;
    apply_highlights(editor, cx);
    apply_folds(editor, cx);
}

/// Turns the email view on or off for this buffer (V16 §4). Off removes the
/// reply/quote creases and every email fold; back on reimposes the default
/// collapsed state, as if the note were freshly opened.
fn toggle_email_view(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon_mut::<MarkdownConcealAddon>() else {
        return;
    };
    addon.email_enabled = !addon.email_enabled;
    if !addon.email_enabled {
        addon.email = None;
        addon.default_folds_applied = false;
        update_email_creases(editor, cx);
        remove_tagged_folds(editor, email_fold_type_tag(), cx);
    }
    // The fold diff is range-only, and the two modes give the same ranges
    // different placeholders (`## ` is a marker in one, a sender dot in the
    // other) — drop every conceal fold so the reparse re-applies them fresh.
    remove_tagged_folds(editor, fold_type_tag(), cx);
    schedule_reparse(editor, cx);
}

fn remove_tagged_folds(editor: &mut Editor, tag: TypeId, cx: &mut Context<Editor>) {
    let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
    let display_snapshot = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
    let ranges: Vec<Range<MultiBufferOffset>> = display_snapshot
        .folds_in_range(MultiBufferOffset(0)..buffer_snapshot.len())
        .filter(|fold| fold.placeholder.type_tag == Some(tag))
        .map(|fold| {
            fold.range.start.to_offset(&buffer_snapshot)..fold.range.end.to_offset(&buffer_snapshot)
        })
        .collect();
    if !ranges.is_empty() {
        editor.display_map.update(cx, |map, cx| {
            map.remove_folds_with_type(ranges, tag, cx);
        });
        cx.notify();
    }
}

/// Collapses or expands the reply the cursor is on — header row or anywhere
/// in the body. Goes through the `DisplayMap` directly so a keyboard toggle
/// never enters fold persistence (V16 §5.3).
fn toggle_message(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon::<MarkdownConcealAddon>() else {
        return;
    };
    let Some(email) = &addon.email else {
        return;
    };
    let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
    let cursor = editor
        .selections
        .newest_anchor()
        .head()
        .to_point(&buffer_snapshot);
    let Some((body, placeholder)) = email.messages.iter().find_map(|message| {
        let header_row = message.header_start.to_point(&buffer_snapshot).row;
        let end_row = message.body.end.to_point(&buffer_snapshot).row;
        (header_row..=end_row).contains(&cursor.row).then(|| {
            (
                message.body.start.to_offset(&buffer_snapshot)
                    ..message.body.end.to_offset(&buffer_snapshot),
                email_body_placeholder(&message.body, &buffer_snapshot),
            )
        })
    }) else {
        return;
    };
    let Some(placeholder) = placeholder else {
        return;
    };
    let display_snapshot = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
    // Match the whole-body fold exactly — a collapsed quote inside an
    // expanded reply is also an email fold in this range and must not read
    // as "the reply is folded".
    let already_folded = display_snapshot.folds_in_range(body.start..body.end).any(|fold| {
        fold.placeholder.type_tag == Some(email_fold_type_tag())
            && fold.range.start.to_offset(&buffer_snapshot) == body.start
            && fold.range.end.to_offset(&buffer_snapshot) == body.end
    });
    // Removal sweeps every intersecting email fold, so a collapsed quote
    // inside the reply must be re-folded after the body expands.
    let nested: Vec<Range<MultiBufferOffset>> = display_snapshot
        .folds_in_range(body.start..body.end)
        .filter(|fold| fold.placeholder.type_tag == Some(email_fold_type_tag()))
        .map(|fold| {
            fold.range.start.to_offset(&buffer_snapshot)..fold.range.end.to_offset(&buffer_snapshot)
        })
        .filter(|range| *range != body)
        .collect();
    editor.display_map.update(cx, |map, cx| {
        if already_folded {
            map.remove_folds_with_type(vec![body.clone()], email_fold_type_tag(), cx);
            let requoted: Vec<Crease<MultiBufferOffset>> = nested
                .into_iter()
                .map(|range| Crease::simple(range, quote_placeholder()))
                .collect();
            if !requoted.is_empty() {
                map.fold(requoted, cx);
            }
        } else {
            map.fold(vec![Crease::simple(body, placeholder)], cx);
        }
    });
    cx.notify();
}

/// Moves the cursor to the next or previous message header (V16 §5.5).
fn move_to_message(
    editor: &mut Editor,
    forward: bool,
    window: &mut Window,
    cx: &mut Context<Editor>,
) {
    let Some(addon) = editor.addon::<MarkdownConcealAddon>() else {
        return;
    };
    let Some(email) = &addon.email else {
        return;
    };
    let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
    let cursor_row = editor
        .selections
        .newest_anchor()
        .head()
        .to_point(&buffer_snapshot)
        .row;
    let mut header_rows: Vec<u32> = email
        .messages
        .iter()
        .map(|message| message.header_start.to_point(&buffer_snapshot).row)
        .collect();
    header_rows.sort_unstable();
    let target = if forward {
        header_rows.into_iter().find(|&row| row > cursor_row)
    } else {
        header_rows.into_iter().rev().find(|&row| row < cursor_row)
    };
    let Some(row) = target else {
        return;
    };
    editor.change_selections(Default::default(), window, cx, |selections| {
        let point = MultiBufferPoint::new(row, 0);
        selections.select_ranges([point..point]);
    });
}

/// Reparses the whole buffer on a background task after a short debounce,
/// then re-anchors the span plan and applies it. Replacing the previous task
/// is the debounce — only the last edit in a burst parses.
fn schedule_reparse(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon::<MarkdownConcealAddon>() else {
        return;
    };
    let email_enabled = addon.email_enabled;
    let account = addon.account.clone();
    let task = cx.spawn(async move |editor, cx| {
        cx.background_executor().timer(REPARSE_DEBOUNCE).await;
        let Ok(snapshot) = editor.read_with(cx, |editor, cx| editor.buffer().read(cx).snapshot(cx))
        else {
            return;
        };
        let (spans, email) = cx
            .background_spawn(async move {
                let text = snapshot.text();
                let anchor = |range: &Range<usize>| {
                    snapshot.anchor_after(MultiBufferOffset(range.start))
                        ..snapshot.anchor_before(MultiBufferOffset(range.end))
                };
                let plan = email_enabled
                    .then(|| markdown_syntax::email_plan(&text, account.as_deref()))
                    .flatten();
                let (plain_spans, email) = match plan {
                    Some(plan) => {
                        let email = EmailAnchors {
                            messages: plan
                                .messages
                                .iter()
                                .map(|message| MessageAnchors {
                                    header_start: snapshot.anchor_after(MultiBufferOffset(
                                        message.header_line.start,
                                    )),
                                    body: anchor(&message.body),
                                })
                                .collect(),
                            quotes: plan.quotes.iter().map(|quote| anchor(quote)).collect(),
                        };
                        (plan.spans, Some(email))
                    }
                    None => (markdown_syntax::conceal_spans(&text), None),
                };
                let spans = plain_spans
                    .into_iter()
                    .map(|span| AnchorSpan {
                        range: anchor(&span.range),
                        kind: span.kind,
                    })
                    .collect::<Vec<_>>();
                (spans, email)
            })
            .await;
        editor
            .update(cx, |editor, cx| {
                if let Some(addon) = editor.addon_mut::<MarkdownConcealAddon>() {
                    addon.spans = spans;
                    addon.email = email;
                }
                update_email_creases(editor, cx);
                apply_highlights(editor, cx);
                apply_folds(editor, cx);
                apply_default_email_folds(editor, cx);
            })
            .ok();
    });
    if let Some(addon) = editor.addon_mut::<MarkdownConcealAddon>() {
        addon.reparse = task;
    }
}

/// Replaces the addon's creases with the current plan's reply and quote
/// creases. Fold state is unaffected — folds live in the fold map and their
/// anchors survive edits; creases are only the *definitions* the gutter and
/// `fold_at` use (V16 §5.3, A2).
fn update_email_creases(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon::<MarkdownConcealAddon>() else {
        return;
    };
    let old_ids = addon.crease_ids.clone();
    let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
    let mut creases: Vec<Crease<Anchor>> = Vec::new();
    if let Some(email) = &addon.email {
        for message in &email.messages {
            if let Some(placeholder) =
                email_body_placeholder(&message.body, &buffer_snapshot)
            {
                creases.push(Crease::simple(message.body.clone(), placeholder));
            }
        }
        for quote in &email.quotes {
            creases.push(Crease::simple(quote.clone(), quote_placeholder()));
        }
    }
    let new_ids = editor.display_map.update(cx, |map, cx| {
        map.remove_creases(old_ids, cx);
        map.insert_creases(creases, cx)
    });
    if let Some(addon) = editor.addon_mut::<MarkdownConcealAddon>() {
        addon.crease_ids = new_ids;
    }
}

/// Imposes the V16 §5.3/§5.4 default once per open: every reply but the
/// newest collapsed, quoted history collapsed. Applied through the
/// `DisplayMap` so the defaults never enter fold persistence.
fn apply_default_email_folds(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon::<MarkdownConcealAddon>() else {
        return;
    };
    if addon.default_folds_applied {
        return;
    }
    let Some(email) = &addon.email else {
        return;
    };
    let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
    let mut folds: Vec<Crease<MultiBufferOffset>> = Vec::new();
    let newest = email.messages.len().saturating_sub(1);
    for message in email.messages.iter().take(newest) {
        let range = message.body.start.to_offset(&buffer_snapshot)
            ..message.body.end.to_offset(&buffer_snapshot);
        if let Some(placeholder) = email_body_placeholder(&message.body, &buffer_snapshot)
            && range.start < range.end
        {
            folds.push(Crease::simple(range, placeholder));
        }
    }
    for quote in &email.quotes {
        let range = quote.start.to_offset(&buffer_snapshot)
            ..quote.end.to_offset(&buffer_snapshot);
        if range.start < range.end {
            folds.push(Crease::simple(range, quote_placeholder()));
        }
    }
    if let Some(addon) = editor.addon_mut::<MarkdownConcealAddon>() {
        addon.default_folds_applied = true;
    }
    if !folds.is_empty() {
        editor.display_map.update(cx, |map, cx| map.fold(folds, cx));
        cx.notify();
    }
}

/// Recomputes the desired fold set (all conceal spans minus those on revealed
/// lines) and diffs it against the folds currently in the display map. Runs
/// on every selection change, so it queries real fold state rather than
/// trusting bookkeeping — a `zR` that wiped our folds heals on the next
/// cursor move (§10.2).
///
/// This must go through the `DisplayMap` directly: `Editor::fold_creases`
/// routes through `folds_did_change`, which persists folds into workspace
/// restoration data (§10.1).
fn apply_folds(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon::<MarkdownConcealAddon>() else {
        return;
    };
    let enabled = addon.enabled;
    let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);

    let revealed = revealed_rows(editor, &buffer_snapshot);
    let mut desired: Vec<(Range<MultiBufferOffset>, SpanKind)> = Vec::new();
    if enabled {
        for span in &addon.spans {
            if !is_folded(span.kind) {
                continue;
            }
            let start = span.range.start.to_offset(&buffer_snapshot);
            let end = span.range.end.to_offset(&buffer_snapshot);
            if start >= end {
                continue;
            }
            let start_row = span.range.start.to_point(&buffer_snapshot).row;
            let end_point = span.range.end.to_point(&buffer_snapshot);
            // A line-inclusive span (`EmailHidden` folds its newline) ends at
            // the next row's column 0; that row isn't part of the span for
            // the reveal rule.
            let end_row = if end_point.column == 0 && end_point.row > start_row {
                end_point.row - 1
            } else {
                end_point.row
            };
            let is_revealed = revealed
                .iter()
                .any(|&(first, last)| start_row <= last && end_row >= first);
            if !is_revealed {
                desired.push((start..end, span.kind));
            }
        }
    }

    let display_snapshot = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
    let mut existing: Vec<(Range<MultiBufferOffset>, Option<SharedString>)> = Vec::new();
    let mut restored_impostors: Vec<Range<MultiBufferOffset>> = Vec::new();
    for fold in display_snapshot.folds_in_range(MultiBufferOffset(0)..buffer_snapshot.len()) {
        let range = fold.range.start.to_offset(&buffer_snapshot)
            ..fold.range.end.to_offset(&buffer_snapshot);
        if fold.placeholder.type_tag == Some(fold_type_tag()) {
            existing.push((range, fold.placeholder.collapsed_text.clone()));
        } else if fold.placeholder.type_tag == Some(email_fold_type_tag()) {
            continue;
        } else if addon.spans.iter().any(|span| {
            is_folded(span.kind)
                && span.range.start.to_offset(&buffer_snapshot) == range.start
                && span.range.end.to_offset(&buffer_snapshot) == range.end
        }) || addon.email.as_ref().is_some_and(|email| {
            let matches = |anchors: &Range<Anchor>| {
                anchors.start.to_offset(&buffer_snapshot) == range.start
                    && anchors.end.to_offset(&buffer_snapshot) == range.end
            };
            email.messages.iter().any(|message| matches(&message.body))
                || email.quotes.iter().any(matches)
        }) {
            // A fold restored from the workspace database that exactly
            // matches a conceal span — or an email reply or quote crease
            // (V16 §7) — is one of ours that got swept into fold
            // persistence: purge it before it renders as a literal `⋯`
            // (§10.1). The email default-state pass then re-collapses.
            restored_impostors.push(range);
        }
    }

    // The diff compares placeholder text as well as range: toggling the
    // email view gives the same `## ` range a different placeholder (marker
    // space vs sender dot), and a range-only diff would keep the stale one.
    let matches = |(range, text): &(Range<MultiBufferOffset>, Option<SharedString>),
                   (desired, kind): &(Range<MultiBufferOffset>, SpanKind)| {
        range == desired && text.as_deref() == Some(collapsed_text_for(*kind))
    };
    let stale: Vec<Range<MultiBufferOffset>> = existing
        .iter()
        .filter(|fold| !desired.iter().any(|want| matches(fold, want)))
        .map(|(range, _)| range.clone())
        .collect();
    let new: Vec<(Range<MultiBufferOffset>, SpanKind)> = desired
        .iter()
        .filter(|want| {
            !existing.iter().any(|fold| matches(fold, want))
                || restored_impostors.contains(&want.0)
        })
        .cloned()
        .collect();
    if stale.is_empty() && new.is_empty() && restored_impostors.is_empty() {
        return;
    }

    let editor_handle = cx.weak_entity();
    let creases: Vec<Crease<MultiBufferOffset>> = new
        .into_iter()
        .map(|(range, kind)| {
            let placeholder = match kind {
                SpanKind::Rule => rule_placeholder(editor_handle.clone()),
                SpanKind::Checkbox(checked) => checkbox_placeholder(checked),
                SpanKind::EmailMarker(own) => sender_dot_placeholder(own),
                SpanKind::EmailLink => email_link_placeholder(),
                _ => marker_placeholder(),
            };
            Crease::simple(range, placeholder)
        })
        .collect();

    editor.display_map.update(cx, |map, cx| {
        if !restored_impostors.is_empty() {
            map.unfold_intersecting(restored_impostors, false, cx);
        }
        if !stale.is_empty() {
            map.remove_folds_with_type(stale, fold_type_tag(), cx);
        }
        if !creases.is_empty() {
            map.fold(creases, cx);
        }
    });
    cx.notify();
}

/// The collapsed text each folded kind's placeholder carries — the fold
/// diff's placeholder key. Must agree with what the placeholder constructors
/// set.
fn collapsed_text_for(kind: SpanKind) -> &'static str {
    match kind {
        SpanKind::Checkbox(false) => "☐",
        SpanKind::Checkbox(true) => "☑",
        SpanKind::EmailMarker(_) => "●",
        SpanKind::EmailLink => "Open in Gmail ↗",
        _ => " ",
    }
}

/// Whether a span is concealed behind a fold placeholder rather than merely
/// coloured.
fn is_folded(kind: SpanKind) -> bool {
    matches!(
        kind,
        SpanKind::Marker
            | SpanKind::Rule
            | SpanKind::Checkbox(_)
            | SpanKind::EmailHidden
            | SpanKind::EmailLink
            | SpanKind::EmailMarker(_)
    )
}

/// The buffer rows currently revealed, as inclusive `(first, last)` pairs:
/// any row a selection's head or tail lies on, or that a selection covers —
/// the union across all cursors, with no "primary" (§5 R1–R3). The pending
/// selection is counted too: while the mouse drags, `disjoint` excludes it
/// (and may be empty), and the lines under the drag must stay revealed.
fn revealed_rows(editor: &Editor, buffer_snapshot: &MultiBufferSnapshot) -> Vec<(u32, u32)> {
    editor
        .selections
        .disjoint_anchors()
        .iter()
        .chain(editor.selections.pending_anchor())
        .map(|selection| {
            let start = selection.start.to_point(buffer_snapshot).row;
            let end = selection.end.to_point(buffer_snapshot).row;
            (start, end)
        })
        .collect()
}

/// Placeholder for hidden markup: a single-character collapsed text rendered
/// by a zero-width element, so nothing is drawn where the markers were.
///
/// This is the §11.1 fallback, not the zero-length ideal: an empty collapsed
/// text produces a zero-output transform that underflows the tab map's
/// `TabStops` iterator (`chunk_len - 1` on an empty chunk), so each concealed
/// span costs one invisible display column instead (§10.3).
fn marker_placeholder() -> FoldPlaceholder {
    FoldPlaceholder {
        render: Arc::new(|_, _, _| Empty.into_any_element()),
        constrain_width: false,
        merge_adjacent: false,
        type_tag: Some(fold_type_tag()),
        gutter_toggle: false,
        collapsed_text: Some(" ".into()),
    }
}

/// Placeholder for a task list's `[ ]` / `[x]`: the checkbox it stands for,
/// drawn the way the rest of the app draws checkboxes. Display-only — the
/// box is not clickable, since ticking it would write to the buffer.
fn checkbox_placeholder(checked: bool) -> FoldPlaceholder {
    FoldPlaceholder {
        render: Arc::new(move |_, _, cx| {
            let colors = cx.theme().colors();
            let mut checkbox = div()
                .size(px(13.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_xs()
                .border_1()
                .border_color(if checked {
                    colors.border_selected
                } else {
                    colors.border
                });
            if checked {
                checkbox = checkbox.child(
                    Icon::new(IconName::Check)
                        .size(IconSize::XSmall)
                        .color(Color::Accent),
                );
            }
            div()
                .h_full()
                .flex()
                .items_center()
                .child(checkbox)
                .into_any_element()
        }),
        constrain_width: false,
        merge_adjacent: false,
        type_tag: Some(fold_type_tag()),
        gutter_toggle: false,
        collapsed_text: Some(if checked { "☑" } else { "☐" }.into()),
    }
}

/// Placeholder for a `___` line: a drawn horizontal rule. The collapsed text
/// must be non-empty for the rendered element to survive to layout, which
/// costs one display column on a line that is otherwise entirely folded.
fn rule_placeholder(editor: WeakEntity<Editor>) -> FoldPlaceholder {
    FoldPlaceholder {
        render: Arc::new(move |_, _, cx| {
            // The placeholder render never sees the layout's max width, so
            // the rule is sized from the editor's last known bounds — close
            // enough to edge-to-edge without overflowing into a horizontal
            // scroll.
            let width = editor
                .upgrade()
                .and_then(|editor| {
                    editor
                        .read(cx)
                        .last_bounds()
                        .map(|bounds| bounds.size.width)
                })
                .map(|width| width * 0.9)
                .unwrap_or(px(480.));
            div()
                .w(width)
                .h_full()
                .flex()
                .items_center()
                .child(div().w_full().h(px(1.)).bg(cx.theme().colors().border))
                .into_any_element()
        }),
        constrain_width: false,
        merge_adjacent: false,
        type_tag: Some(fold_type_tag()),
        gutter_toggle: false,
        collapsed_text: Some(" ".into()),
    }
}

/// Placeholder for a message header's `## ` marker: a small sender dot in
/// the sender's colour — the conversation-spine glyph (V16 §5.2).
fn sender_dot_placeholder(own: bool) -> FoldPlaceholder {
    FoldPlaceholder {
        render: Arc::new(move |_, _, cx| {
            div()
                .h_full()
                .flex()
                .items_center()
                .child(
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .bg(sender_color(own, cx)),
                )
                .into_any_element()
        }),
        constrain_width: false,
        merge_adjacent: false,
        type_tag: Some(fold_type_tag()),
        gutter_toggle: false,
        collapsed_text: Some("●".into()),
    }
}

/// Placeholder for the envelope's `link:` line: an "Open in Gmail" label in
/// the link colour instead of the raw URL (V16 §5.1).
fn email_link_placeholder() -> FoldPlaceholder {
    FoldPlaceholder {
        render: Arc::new(|_, _, cx| {
            div()
                .h_full()
                .flex()
                .items_center()
                .text_color(markdown_text::external_link_color(cx))
                .child("Open in Gmail ↗")
                .into_any_element()
        }),
        constrain_width: false,
        merge_adjacent: false,
        type_tag: Some(fold_type_tag()),
        gutter_toggle: false,
        collapsed_text: Some("Open in Gmail ↗".into()),
    }
}

/// Placeholder for a collapsed reply body: a muted `⋯ N lines` pill at the
/// end of the header row (V16 §5.3). `None` when the body has no rows to
/// hide.
fn email_body_placeholder(
    body: &Range<Anchor>,
    buffer_snapshot: &MultiBufferSnapshot,
) -> Option<FoldPlaceholder> {
    let rows = body
        .end
        .to_point(buffer_snapshot)
        .row
        .saturating_sub(body.start.to_point(buffer_snapshot).row);
    if rows == 0 {
        return None;
    }
    let label = SharedString::from(format!(
        "⋯ {rows} {}",
        if rows == 1 { "line" } else { "lines" }
    ));
    Some(email_fold_placeholder(label))
}

/// Placeholder for collapsed quoted history (V16 §5.4).
fn quote_placeholder() -> FoldPlaceholder {
    email_fold_placeholder("⋯ quoted history".into())
}

/// The shared shape of the email view's user-toggleable folds: muted label,
/// email tag, and a gutter toggle so a collapsed reply advertises itself.
fn email_fold_placeholder(label: SharedString) -> FoldPlaceholder {
    let collapsed = label.clone();
    FoldPlaceholder {
        render: Arc::new(move |_, _, cx| {
            div()
                .h_full()
                .flex()
                .items_center()
                .text_color(cx.theme().colors().text_muted)
                .child(label.clone())
                .into_any_element()
        }),
        constrain_width: false,
        merge_adjacent: false,
        type_tag: Some(email_fold_type_tag()),
        gutter_toggle: true,
        collapsed_text: Some(collapsed),
    }
}

fn sender_color(own: bool, cx: &App) -> Hsla {
    if own {
        cx.theme().status().created
    } else {
        cx.theme().colors().text_accent
    }
}

/// The number of highlight slots: two link colours, three heading colours,
/// the strikethrough, and the email view's sender/own/muted trio.
const HIGHLIGHT_SLOTS: usize = 9;

/// The slot strikethrough spans take. Late so its style merges over the
/// colour slots, and it carries no colour of its own — a struck link keeps
/// its link colour and gains the line.
const STRIKETHROUGH_SLOT: usize = 5;

const EMAIL_SENDER_SLOT: usize = 6;
const EMAIL_OWN_SENDER_SLOT: usize = 7;
const EMAIL_MUTED_SLOT: usize = 8;

/// The highlight slot for a styled span. Links get the higher-priority
/// slots so a link label inside a heading keeps its link colour.
fn highlight_slot(kind: SpanKind) -> Option<usize> {
    match kind {
        SpanKind::WikilinkLabel => Some(0),
        SpanKind::LinkLabel => Some(1),
        // Levels 4–6 reuse level 3 — three signals are enough to read
        // structure at a glance (§7.1).
        SpanKind::Heading(level) => Some(1 + (level.clamp(1, 3) as usize)),
        SpanKind::Strikethrough => Some(STRIKETHROUGH_SLOT),
        SpanKind::EmailSender(false) => Some(EMAIL_SENDER_SLOT),
        SpanKind::EmailSender(true) => Some(EMAIL_OWN_SENDER_SLOT),
        SpanKind::EmailDate | SpanKind::EmailQuote => Some(EMAIL_MUTED_SLOT),
        SpanKind::Marker
        | SpanKind::Rule
        | SpanKind::Checkbox(_)
        | SpanKind::EmailHidden
        | SpanKind::EmailLink
        | SpanKind::EmailMarker(_) => None,
    }
}

fn slot_style(slot: usize, cx: &App) -> HighlightStyle {
    if slot == STRIKETHROUGH_SLOT {
        return HighlightStyle {
            strikethrough: Some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            }),
            ..Default::default()
        };
    }
    let font_weight = matches!(slot, EMAIL_SENDER_SLOT | EMAIL_OWN_SENDER_SLOT)
        .then_some(gpui::FontWeight::SEMIBOLD);
    HighlightStyle {
        color: Some(slot_color(slot, cx)),
        font_weight,
        ..Default::default()
    }
}

fn slot_color(slot: usize, cx: &App) -> Hsla {
    let colors = cx.theme().colors();
    match slot {
        0 => markdown_text::wikilink_color(cx),
        1 => markdown_text::external_link_color(cx),
        EMAIL_SENDER_SLOT => sender_color(false, cx),
        EMAIL_OWN_SENDER_SLOT => sender_color(true, cx),
        EMAIL_MUTED_SLOT => colors.text_muted,
        _ => {
            let players = &cx.theme().players().0;
            // Slot 0 of the player palette is the local-user colour; heading
            // levels take the stable slots after it, the same bet the Day
            // Planner makes (§7.1, A4).
            players
                .get(1..)
                .filter(|palette| !palette.is_empty())
                .and_then(|palette| palette.get((slot - 2) % palette.len()))
                .map(|player| player.cursor)
                .unwrap_or(colors.text_accent)
        }
    }
}

/// Reapplies all colour highlights from the current span plan. Colours are
/// independent of reveal — a heading on the cursor's line stays coloured
/// while showing its `#` (§8.2).
fn apply_highlights(editor: &mut Editor, cx: &mut Context<Editor>) {
    let Some(addon) = editor.addon::<MarkdownConcealAddon>() else {
        return;
    };
    let mut by_slot: [Vec<Range<Anchor>>; HIGHLIGHT_SLOTS] = Default::default();
    if addon.enabled {
        for span in &addon.spans {
            if let Some(slot) = highlight_slot(span.kind) {
                by_slot[slot].push(span.range.clone());
            }
        }
    }
    for (slot, ranges) in by_slot.into_iter().enumerate() {
        editor.clear_highlights(HighlightKey::ThockMarkdownConceal(slot), cx);
        if !ranges.is_empty() {
            let style = slot_style(slot, cx);
            editor.highlight_text(HighlightKey::ThockMarkdownConceal(slot), ranges, style, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor::DisplayPoint;
    use editor::display_map::DisplayRow;
    use editor::test::editor_test_context::EditorTestContext;
    use fs::FakeFs;
    use gpui::{
        Entity, Modifiers, MouseButton, TestAppContext, VisualContext as _, VisualTestContext,
    };
    use multi_buffer::{MultiBufferPoint, MultiBufferRow};
    use project::Project;
    use serde_json::json;
    use std::path::Path;
    use text::Bias;

    const NOTE: &str = "# Title\nsee [[wiki]] and [docs](https://a.example)\n___\nplain tail\n";

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
        });
    }

    async fn setup(cx: &mut TestAppContext, text: &str) -> (Entity<Editor>, VisualTestContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/vault", json!({ "note.md": text })).await;
        let project = Project::test(fs, [Path::new("/vault")], cx).await;
        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer("/vault/note.md", cx)
            })
            .await
            .unwrap();
        let window = cx
            .add_window(|window, cx| Editor::for_buffer(buffer, Some(project.clone()), window, cx));
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window.root(&mut cx).unwrap();
        editor.update_in(&mut cx, |editor, _, cx| {
            install(editor, test_settings(), cx)
        });
        settle(&mut cx);
        (editor, cx)
    }

    fn test_settings() -> ConcealSettings {
        ConcealSettings {
            conceal: true,
            email_view: true,
            account: Some("diego.exodo@gmail.com".to_string()),
        }
    }

    /// Lets the reparse debounce elapse and all effects flush.
    fn settle(cx: &mut VisualTestContext) {
        cx.executor().advance_clock(REPARSE_DEBOUNCE * 2);
        cx.run_until_parked();
    }

    fn display_text(editor: &Entity<Editor>, cx: &mut VisualTestContext) -> String {
        editor.update(cx, |editor, cx| editor.display_text(cx))
    }

    fn move_cursor_to(editor: &Entity<Editor>, row: u32, cx: &mut VisualTestContext) {
        editor.update_in(cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections
                    .select_ranges([MultiBufferPoint::new(row, 0)..MultiBufferPoint::new(row, 0)]);
            });
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn conceals_markup_when_the_cursor_is_elsewhere(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 3, &mut cx);
        assert_eq!(
            display_text(&editor, &mut cx),
            " Title\nsee  wiki  and  docs \n \nplain tail\n"
        );
    }

    #[gpui::test]
    async fn the_cursors_line_shows_its_source(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 0, &mut cx);
        assert_eq!(
            display_text(&editor, &mut cx),
            "# Title\nsee  wiki  and  docs \n \nplain tail\n"
        );
        move_cursor_to(&editor, 1, &mut cx);
        assert_eq!(
            display_text(&editor, &mut cx),
            " Title\nsee [[wiki]] and [docs](https://a.example)\n \nplain tail\n"
        );
    }

    #[gpui::test]
    async fn conceal_folds_do_not_pin_a_gutter_toggle(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 3, &mut cx);

        let snapshot = editor.update_in(&mut cx, |editor, window, cx| editor.snapshot(window, cx));
        let toggle = cx.update(|window, cx| {
            snapshot.render_crease_toggle(MultiBufferRow(0), false, editor.clone(), window, cx)
        });
        assert!(
            toggle.is_none(),
            "a row folded only by conceal must keep the gutter's hover behaviour"
        );

        move_cursor_to(&editor, 1, &mut cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            let range = MultiBufferPoint::new(3, 0)..MultiBufferPoint::new(3, 5);
            editor.fold_creases(
                vec![Crease::simple(range, FoldPlaceholder::default())],
                false,
                window,
                cx,
            );
        });
        let snapshot = editor.update_in(&mut cx, |editor, window, cx| editor.snapshot(window, cx));
        let toggle = cx.update(|window, cx| {
            snapshot.render_crease_toggle(MultiBufferRow(3), false, editor.clone(), window, cx)
        });
        assert!(
            toggle.is_some(),
            "a fold the user made still advertises itself in the gutter"
        );
    }

    #[gpui::test]
    async fn a_multi_line_selection_reveals_every_covered_line(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections
                    .select_ranges([MultiBufferPoint::new(0, 0)..MultiBufferPoint::new(2, 0)]);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            display_text(&editor, &mut cx),
            "# Title\nsee [[wiki]] and [docs](https://a.example)\n___\nplain tail\n"
        );
    }

    #[gpui::test]
    async fn a_pending_mouse_drag_reveals_the_lines_it_covers(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 3, &mut cx);
        let mut cx = EditorTestContext::for_editor_in(editor.clone(), &mut cx).await;
        cx.run_until_parked();

        // Mouse down without a mouse up leaves only a pending selection —
        // `disjoint` excludes it — and the line under it must reveal (§5 R1).
        let start = cx.pixel_position_for(DisplayPoint::new(DisplayRow(0), 0));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.update_editor(|editor, _, _| {
            assert!(editor.selections.pending_anchor().is_some());
        });
        assert_eq!(
            cx.display_text(),
            "# Title\nsee  wiki  and  docs \n \nplain tail\n"
        );

        // Dragging extends the pending selection; every covered line must
        // reveal while the drag is still in flight (§5 R3).
        let end = cx.pixel_position_for(DisplayPoint::new(DisplayRow(1), 0));
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
        assert_eq!(
            cx.display_text(),
            "# Title\nsee [[wiki]] and [docs](https://a.example)\n \nplain tail\n"
        );
    }

    #[gpui::test]
    async fn checkboxes_and_comments_conceal_and_reveal(cx: &mut TestAppContext) {
        let note = "- [ ] open <!--id:7-->\n- [x] done\nplain tail\n";
        let (editor, mut cx) = setup(cx, note).await;
        move_cursor_to(&editor, 2, &mut cx);
        assert_eq!(
            display_text(&editor, &mut cx),
            "- ☐ open  \n- ☑ done\nplain tail\n"
        );

        // The cursor's line shows the source it stands for, comment included.
        move_cursor_to(&editor, 0, &mut cx);
        assert_eq!(
            display_text(&editor, &mut cx),
            "- [ ] open <!--id:7-->\n- ☑ done\nplain tail\n"
        );
    }

    #[gpui::test]
    async fn strikethrough_delimiters_conceal_and_reveal(cx: &mut TestAppContext) {
        let note = "- [ ] ~~dropped~~ task\nplain tail\n";
        let (editor, mut cx) = setup(cx, note).await;
        move_cursor_to(&editor, 1, &mut cx);
        assert_eq!(
            display_text(&editor, &mut cx),
            "- ☐  dropped  task\nplain tail\n"
        );

        move_cursor_to(&editor, 0, &mut cx);
        assert_eq!(display_text(&editor, &mut cx), note);
    }

    #[gpui::test]
    async fn struck_text_is_highlighted_without_a_colour_of_its_own(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, "~~gone [[wiki]]~~\n").await;
        editor.update_in(&mut cx, |editor, _, cx| {
            let (style, ranges) = editor
                .text_highlights(HighlightKey::ThockMarkdownConceal(STRIKETHROUGH_SLOT), cx)
                .expect("the struck run is highlighted");
            assert_eq!(ranges.len(), 1);
            assert!(style.strikethrough.is_some());
            // A struck link keeps its link colour (§7.1).
            assert_eq!(style.color, None);
        });
    }

    #[gpui::test]
    async fn toggling_source_restores_checkboxes_and_comments(cx: &mut TestAppContext) {
        let note = "- [x] done <!--id:7-->\nplain tail\n";
        let (editor, mut cx) = setup(cx, note).await;
        move_cursor_to(&editor, 1, &mut cx);
        assert_eq!(display_text(&editor, &mut cx), "- ☑ done  \nplain tail\n");

        editor.update_in(&mut cx, |editor, _, cx| toggle(editor, cx));
        cx.run_until_parked();
        assert_eq!(display_text(&editor, &mut cx), note);
    }

    #[gpui::test]
    async fn the_buffer_is_never_modified(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 3, &mut cx);
        move_cursor_to(&editor, 0, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| toggle(editor, cx));
        editor.update_in(&mut cx, |editor, _, cx| toggle(editor, cx));
        cx.run_until_parked();
        editor.update(&mut cx, |editor, cx| {
            assert_eq!(editor.buffer().read(cx).snapshot(cx).text(), NOTE);
            assert!(!editor.buffer().read(cx).read(cx).is_dirty());
        });
    }

    #[gpui::test]
    async fn toggling_shows_and_hides_the_source(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 3, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| toggle(editor, cx));
        cx.run_until_parked();
        assert_eq!(display_text(&editor, &mut cx), NOTE);
        editor.update_in(&mut cx, |editor, _, cx| toggle(editor, cx));
        cx.run_until_parked();
        assert_eq!(
            display_text(&editor, &mut cx),
            " Title\nsee  wiki  and  docs \n \nplain tail\n"
        );
    }

    #[gpui::test]
    async fn editing_reconceals_after_the_debounce(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 3, &mut cx);
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                let end = MultiBufferPoint::new(4, 0);
                selections.select_ranges([end..end]);
            });
            editor.insert("## Two\nmore\n", window, cx);
        });
        settle(&mut cx);
        move_cursor_to(&editor, 5, &mut cx);
        assert_eq!(
            display_text(&editor, &mut cx),
            " Title\nsee  wiki  and  docs \n \nplain tail\n Two\nmore\n"
        );
    }

    #[gpui::test]
    async fn display_points_map_through_concealed_markers(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 3, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| {
            let snapshot = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
            // "# Title" displays as " Title" — the marker fold costs one
            // invisible column (§10.3) — so buffer column 2 is display
            // column 1, and display column 4 lands inside "Title".
            let display = snapshot.point_to_display_point(MultiBufferPoint::new(0, 2), Bias::Left);
            assert_eq!(display.row().0, 0);
            assert_eq!(display.column(), 1);
            let display = snapshot.point_to_display_point(MultiBufferPoint::new(0, 5), Bias::Left);
            assert_eq!(display.column(), 4);
            let buffer_point = snapshot.display_point_to_point(display, Bias::Left);
            assert_eq!(buffer_point, text::Point::new(0, 5));
        });
    }

    #[gpui::test]
    async fn soft_wrap_survives_concealed_folds(cx: &mut TestAppContext) {
        let long = "# A heading long enough to wrap several times when the wrap width is small [[with a wikilink]]\nplain\n";
        let (editor, mut cx) = setup(cx, long).await;
        move_cursor_to(&editor, 1, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| {
            editor
                .display_map
                .update(cx, |map, cx| map.set_wrap_width(Some(px(160.)), cx));
            let snapshot = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
            assert!(!snapshot.text().contains("[["));
        });
    }

    #[gpui::test]
    async fn restored_folds_matching_spans_are_purged(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 3, &mut cx);
        // Simulate a fold restored from the workspace database: same range as
        // the heading marker, default placeholder, no type tag (§10.1).
        editor.update_in(&mut cx, |editor, _, cx| {
            let placeholder = editor.default_fold_placeholder(cx);
            editor.display_map.update(cx, |map, cx| {
                map.fold(
                    vec![Crease::simple(
                        MultiBufferOffset(0)..MultiBufferOffset(2),
                        placeholder,
                    )],
                    cx,
                );
            });
        });
        move_cursor_to(&editor, 2, &mut cx);
        move_cursor_to(&editor, 3, &mut cx);
        assert_eq!(
            display_text(&editor, &mut cx),
            " Title\nsee  wiki  and  docs \n \nplain tail\n"
        );
        editor.update_in(&mut cx, |editor, _, cx| {
            let snapshot = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
            let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
            let untagged = snapshot
                .folds_in_range(MultiBufferOffset(0)..buffer_snapshot.len())
                .filter(|fold| fold.placeholder.type_tag != Some(fold_type_tag()))
                .count();
            assert_eq!(untagged, 0);
        });
    }

    #[gpui::test]
    async fn a_wiped_fold_set_heals_on_the_next_cursor_move(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        move_cursor_to(&editor, 3, &mut cx);
        // `unfold_all` and vim's `zR` remove every fold, ours included
        // (§10.2).
        editor.update_in(&mut cx, |editor, _, cx| {
            let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
            editor.display_map.update(cx, |map, cx| {
                map.unfold_intersecting([MultiBufferOffset(0)..buffer_snapshot.len()], true, cx);
            });
        });
        assert_eq!(display_text(&editor, &mut cx), NOTE);
        move_cursor_to(&editor, 0, &mut cx);
        assert_eq!(
            display_text(&editor, &mut cx),
            "# Title\nsee  wiki  and  docs \n \nplain tail\n"
        );
    }

    const EMAIL_NOTE: &str = "---\n\
        source:   gmail\n\
        capture:  8f3c\n\
        captured: 2026-08-28T09:12:44-07:00\n\
        title:    Renewal quote\n\
        from:     Marta Reyes <marta@acmeinsure.com>\n\
        link:     https://mail.google.com/mail/u/d/#all/198f\n\
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
        On Wed, Marta wrote:\n\
        > quoted one\n\
        > quoted two\n\
        \n\
        Sure.\n";

    /// The V16 default open state: machinery folded into the envelope, the
    /// older reply collapsed to its header row, quoted history collapsed,
    /// the newest reply readable.
    const EMAIL_DISPLAY: &str = "     from:     Marta Reyes <marta@acmeinsure.com>\n\
        Open in Gmail ↗\n\
        \u{20}\n\
        \u{20}Renewal quote\n\
        \n\
        ●Marta Reyes <marta@acmeinsure.com> — 2026-08-26 14:02⋯ 3 lines\n\
        ●Diego Tavares <diego.exodo@gmail.com> — 2026-08-27 08:41\n\
        \n\
        ⋯ quoted history\n\
        \n\
        Sure.\n";

    #[gpui::test]
    async fn an_email_note_opens_as_envelope_and_spine(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, EMAIL_NOTE).await;
        move_cursor_to(&editor, 21, &mut cx);
        assert_eq!(display_text(&editor, &mut cx), EMAIL_DISPLAY);
    }

    #[gpui::test]
    async fn a_message_header_reveals_its_source_without_expanding_the_reply(
        cx: &mut TestAppContext,
    ) {
        let (editor, mut cx) = setup(cx, EMAIL_NOTE).await;
        move_cursor_to(&editor, 11, &mut cx);
        let display = display_text(&editor, &mut cx);
        // The cursor's line shows its raw source (§5 R1)…
        assert!(
            display.contains("## Marta Reyes <marta@acmeinsure.com> — 2026-08-26 14:02"),
            "{display}"
        );
        // …but the reply stays collapsed: email folds are not conceal folds.
        assert!(display.contains("⋯ 3 lines"), "{display}");
        assert!(!display.contains("Hi Diego,"), "{display}");
    }

    #[gpui::test]
    async fn toggle_message_collapses_and_expands_from_header_or_body(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, EMAIL_NOTE).await;
        // Cursor in the newest reply's body: collapse it.
        move_cursor_to(&editor, 21, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| toggle_message(editor, cx));
        let display = display_text(&editor, &mut cx);
        assert!(display.contains("⋯ 6 lines"), "{display}");
        assert!(!display.contains("Sure."), "{display}");

        // Toggle from the header row: expand again — the quote inside keeps
        // its own collapsed state.
        move_cursor_to(&editor, 15, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| toggle_message(editor, cx));
        let display = display_text(&editor, &mut cx);
        assert!(display.contains("Sure."), "{display}");
        assert!(display.contains("⋯ quoted history"), "{display}");

        // The older reply expands the same way.
        move_cursor_to(&editor, 11, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| toggle_message(editor, cx));
        let display = display_text(&editor, &mut cx);
        assert!(display.contains("Hi Diego,"), "{display}");
    }

    #[gpui::test]
    async fn gutter_folding_uses_the_reply_crease(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, EMAIL_NOTE).await;
        move_cursor_to(&editor, 21, &mut cx);
        // A gutter click routes through `unfold_at` / `fold_at` (V16 §5.3).
        // Unfolding the collapsed reply also sweeps the row's conceal folds —
        // the BufferFoldToggled heal must restore them without a cursor move.
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.unfold_at(MultiBufferRow(11), window, cx)
        });
        cx.run_until_parked();
        let display = display_text(&editor, &mut cx);
        assert!(display.contains("Hi Diego,"), "{display}");
        assert!(display.contains("●Marta"), "conceal folds must heal: {display}");

        // Folding again finds the inserted crease and its placeholder.
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.fold_at(MultiBufferRow(11), window, cx)
        });
        cx.run_until_parked();
        let display = display_text(&editor, &mut cx);
        assert!(display.contains("⋯ 3 lines"), "{display}");
        assert!(!display.contains("Hi Diego,"), "{display}");
    }

    #[gpui::test]
    async fn message_motions_step_between_headers(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, EMAIL_NOTE).await;
        move_cursor_to(&editor, 0, &mut cx);
        let cursor_row = |editor: &Entity<Editor>, cx: &mut VisualTestContext| {
            editor.update(cx, |editor, cx| {
                let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
                editor
                    .selections
                    .newest_anchor()
                    .head()
                    .to_point(&buffer_snapshot)
                    .row
            })
        };
        editor.update_in(&mut cx, |editor, window, cx| {
            move_to_message(editor, true, window, cx)
        });
        assert_eq!(cursor_row(&editor, &mut cx), 11);
        editor.update_in(&mut cx, |editor, window, cx| {
            move_to_message(editor, true, window, cx)
        });
        assert_eq!(cursor_row(&editor, &mut cx), 15);
        editor.update_in(&mut cx, |editor, window, cx| {
            move_to_message(editor, true, window, cx)
        });
        assert_eq!(cursor_row(&editor, &mut cx), 15, "no header past the last");
        editor.update_in(&mut cx, |editor, window, cx| {
            move_to_message(editor, false, window, cx)
        });
        assert_eq!(cursor_row(&editor, &mut cx), 11);
    }

    #[gpui::test]
    async fn toggling_email_view_off_restores_plain_conceal(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, EMAIL_NOTE).await;
        move_cursor_to(&editor, 21, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| toggle_email_view(editor, cx));
        settle(&mut cx);
        let display = display_text(&editor, &mut cx);
        assert!(display.contains("source:   gmail"), "{display}");
        assert!(display.contains("Hi Diego,"), "{display}");
        assert!(!display.contains('⋯'), "{display}");
        assert!(!display.contains('●'), "{display}");

        // Back on: the default collapsed state reimposes, as freshly opened.
        editor.update_in(&mut cx, |editor, _, cx| toggle_email_view(editor, cx));
        settle(&mut cx);
        assert_eq!(display_text(&editor, &mut cx), EMAIL_DISPLAY);
    }

    #[gpui::test]
    async fn an_email_note_is_never_modified_by_the_view(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, EMAIL_NOTE).await;
        move_cursor_to(&editor, 21, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| toggle_message(editor, cx));
        move_cursor_to(&editor, 11, &mut cx);
        editor.update_in(&mut cx, |editor, _, cx| toggle_message(editor, cx));
        editor.update_in(&mut cx, |editor, _, cx| toggle_email_view(editor, cx));
        settle(&mut cx);
        editor.update(&mut cx, |editor, cx| {
            assert_eq!(editor.buffer().read(cx).snapshot(cx).text(), EMAIL_NOTE);
            assert!(!editor.buffer().read(cx).read(cx).is_dirty());
        });
    }

    #[test]
    fn wikilink_targets_resolve_by_exact_path_then_stem() {
        let files = [
            "daily/2026-08-19.md",
            "projects/thock.md",
            "projects/thock",
            "inbox/other.md",
        ];
        let resolve = |target: &str| {
            let paths: Vec<&RelPath> = files
                .iter()
                .map(|path| RelPath::from_unix_str(path).unwrap())
                .collect();
            resolve_wikilink_target(target, paths.into_iter()).map(RelPath::as_unix_str)
        };
        assert_eq!(resolve("projects/thock.md"), Some("projects/thock.md"));
        assert_eq!(resolve("daily/2026-08-19"), Some("daily/2026-08-19.md"));
        // An extensionless exact path wins over appending `.md`.
        assert_eq!(resolve("projects/thock"), Some("projects/thock"));
        // Basename linking finds a file anywhere in the vault.
        assert_eq!(resolve("other"), Some("inbox/other.md"));
        assert_eq!(resolve("missing"), None);
        assert_eq!(resolve("../escape"), None);
    }

    #[test]
    fn stem_matching_only_resolves_to_notes() {
        let files = ["data/report.pdf", "notes/report.md"];
        let resolve = |target: &str| {
            let paths: Vec<&RelPath> = files
                .iter()
                .map(|path| RelPath::from_unix_str(path).unwrap())
                .collect();
            resolve_wikilink_target(target, paths.into_iter()).map(RelPath::as_unix_str)
        };
        // An extensionless target denotes a note: the earlier `.pdf` with the
        // same stem must not shadow the `.md` file.
        assert_eq!(resolve("report"), Some("notes/report.md"));
        // Non-md files stay reachable by writing the extension.
        assert_eq!(resolve("report.pdf"), Some("data/report.pdf"));
    }

    /// Opens `note.md` from a FakeFs vault inside a real `Workspace`, with
    /// the addon installed, so `GoToDefinition` can be dispatched through the
    /// window exactly as `g d` would be.
    async fn setup_workspace(
        cx: &mut TestAppContext,
        note: &str,
    ) -> (Entity<Workspace>, Entity<Editor>, VisualTestContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            json!({
                "note.md": note,
                "sub": { "other.md": "# Other\n" },
            }),
        )
        .await;
        let project = Project::test(fs, [Path::new("/vault")], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let project_path = project
            .read_with(&mut cx, |project, cx| {
                project.find_project_path("/vault/note.md", cx)
            })
            .unwrap();
        let editor = workspace
            .update_in(&mut cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap()
            .downcast::<Editor>()
            .unwrap();
        editor.update_in(&mut cx, |editor, _, cx| {
            install(editor, test_settings(), cx)
        });
        cx.focus(&editor);
        settle(&mut cx);
        (workspace, editor, cx)
    }

    fn active_item_path(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
        workspace.update(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.project_path(cx))
                .map(|project_path| project_path.path.as_unix_str().to_string())
                .unwrap_or_default()
        })
    }

    #[gpui::test]
    async fn go_to_definition_on_a_wikilink_opens_the_linked_note(cx: &mut TestAppContext) {
        let (workspace, editor, mut cx) = setup_workspace(cx, "see [[other]] here\n").await;
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                let on_link = MultiBufferPoint::new(0, 7);
                selections.select_ranges([on_link..on_link]);
            });
        });
        cx.dispatch_action(GoToDefinition::default());
        cx.run_until_parked();
        assert_eq!(active_item_path(&workspace, &mut cx), "sub/other.md");
    }

    #[gpui::test]
    async fn go_to_definition_off_a_wikilink_falls_through(cx: &mut TestAppContext) {
        let (workspace, editor, mut cx) = setup_workspace(cx, "see [[other]] here\n").await;
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                let on_plain_text = MultiBufferPoint::new(0, 16);
                selections.select_ranges([on_plain_text..on_plain_text]);
            });
        });
        cx.dispatch_action(GoToDefinition::default());
        cx.run_until_parked();
        assert_eq!(active_item_path(&workspace, &mut cx), "note.md");
    }

    #[gpui::test]
    async fn go_to_definition_on_an_unresolved_wikilink_does_nothing(cx: &mut TestAppContext) {
        // Swallowed, not propagated — and no file is created.
        let (workspace, editor, mut cx) = setup_workspace(cx, "see [[missing]] here\n").await;
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                let on_link = MultiBufferPoint::new(0, 7);
                selections.select_ranges([on_link..on_link]);
            });
        });
        cx.dispatch_action(GoToDefinition::default());
        cx.run_until_parked();
        assert_eq!(active_item_path(&workspace, &mut cx), "note.md");
    }

    #[gpui::test]
    async fn go_to_definition_works_with_conceal_toggled_off(cx: &mut TestAppContext) {
        let (workspace, editor, mut cx) = setup_workspace(cx, "see [[other]] here\n").await;
        editor.update_in(&mut cx, |editor, _, cx| toggle(editor, cx));
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                let on_link = MultiBufferPoint::new(0, 7);
                selections.select_ranges([on_link..on_link]);
            });
        });
        cx.run_until_parked();
        cx.dispatch_action(GoToDefinition::default());
        cx.run_until_parked();
        assert_eq!(active_item_path(&workspace, &mut cx), "sub/other.md");
    }

    #[gpui::test]
    async fn headings_and_link_labels_are_coloured(cx: &mut TestAppContext) {
        let (editor, mut cx) = setup(cx, NOTE).await;
        let highlighted = |editor: &mut Editor, slot: usize, cx: &mut Context<Editor>| {
            let buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
            editor
                .text_highlights(HighlightKey::ThockMarkdownConceal(slot), cx)
                .map(|(_, ranges)| {
                    ranges
                        .iter()
                        .map(|range| {
                            let start = range.start.to_offset(&buffer_snapshot);
                            let end = range.end.to_offset(&buffer_snapshot);
                            NOTE[start.0..end.0].to_string()
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        editor.update_in(&mut cx, |editor, _, cx| {
            assert_eq!(highlighted(editor, 0, cx), vec!["wiki"]);
            assert_eq!(highlighted(editor, 1, cx), vec!["docs"]);
            assert_eq!(highlighted(editor, 2, cx), vec!["Title"]);
            toggle(editor, cx);
            assert!(highlighted(editor, 0, cx).is_empty());
            assert!(highlighted(editor, 2, cx).is_empty());
        });
    }
}
