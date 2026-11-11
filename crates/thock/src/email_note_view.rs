//! An email note in its own tab (spec `v56-email-reader-and-reply.md` §4):
//! the conversation the Inbox shows, for a note opened from anywhere in the
//! vault. The editor still opens the file; when the buffer turns out to be an
//! email, this view takes the editor's place, and `o` gives the source back.

use std::collections::HashSet;

use editor::Editor;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, Global, KeyContext, SharedString, Subscription, TaskExt as _, WeakEntity, Window,
    actions, div, prelude::*,
};
use language::{Buffer, BufferEvent};
use project::{Project, ProjectItem as _};
use ui::{
    ActiveTheme as _, Color, Icon, IconName, Label, LabelCommon as _, LabelSize, Tooltip, h_flex,
    v_flex,
};
use util::ResultExt as _;
use workspace::item::{Item, ItemBufferKind, ItemEvent, ItemHandle};
use workspace::{Pane, Workspace};

use crate::email_reader::{
    EMAIL_SOURCE, EmailReader, EmailReaderHost, PANE_HEADER_HEIGHT, READER_LINE, quiet_action,
    sender_address, sender_name,
};
use crate::inbox::{InboxItem, note_hash, parse_inbox_note};
use crate::inbox_service;
use crate::inbox_view::{
    NextInboxMessage, OpenInboxLink, PreviousInboxMessage, ScrollInboxReaderDown,
    ScrollInboxReaderPageDown, ScrollInboxReaderPageUp, ScrollInboxReaderUp, ToggleInboxFolds,
};

pub const EMAIL_VIEW_KEY: &str = "ThockEmailView";

actions!(
    thock,
    [
        /// Shows the Markdown behind the email you're reading, so you can
        /// edit it.
        EditEmailNote,
        /// Shows the email note you're editing as a conversation again.
        ReadEmailNote,
    ]
);

/// Editors opened to show an email's source. They stay editors.
#[derive(Default)]
struct SourceEditors(HashSet<EntityId>);

impl Global for SourceEditors {}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        workspace.register_action(|workspace, _: &ReadEmailNote, window, cx| {
            read_active_note(workspace, window, cx);
        });
        cx.subscribe_in(
            &cx.entity(),
            window,
            |_, _, event: &workspace::Event, window, cx| {
                if let workspace::Event::ItemAdded { item } = event
                    && let Some(editor) = item.downcast::<Editor>()
                {
                    // The pane is still adding the item; swap once it's done.
                    cx.defer_in(window, move |workspace, window, cx| {
                        route_editor(workspace, editor, window, cx);
                    });
                }
            },
        )
        .detach();
    })
    .detach();
}

/// The note an editor shows, when that is an email in the project.
fn email_note(editor: &Entity<Editor>, cx: &App) -> Option<(Entity<Buffer>, InboxItem)> {
    let buffer = editor.read(cx).buffer().read(cx).as_singleton()?;
    let item = parse_email(&buffer, cx)?;
    Some((buffer, item))
}

fn parse_email(buffer: &Entity<Buffer>, cx: &App) -> Option<InboxItem> {
    let buffer = buffer.read(cx);
    let path = buffer.project_path(cx)?;
    let path = path.path.as_unix_str();
    if !path.ends_with(".md") {
        return None;
    }
    let item = parse_inbox_note(path, &buffer.text());
    (item.source.as_deref() == Some(EMAIL_SOURCE)).then_some(item)
}

/// §4.4: an editor that just opened on an email note gives its place to the
/// reader, unless the source was asked for or has unsaved changes.
fn route_editor(
    workspace: &mut Workspace,
    editor: Entity<Editor>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if cx
        .try_global::<SourceEditors>()
        .is_some_and(|editors| editors.0.contains(&editor.entity_id()))
    {
        return;
    }
    let Some((buffer, _)) = email_note(&editor, cx) else {
        return;
    };
    if buffer.read(cx).is_dirty() {
        return;
    }
    replace_with_reader(workspace, &editor, buffer, window, cx);
}

/// `thock::ReadEmailNote`: the active editor's note as a conversation. Unsaved
/// changes are saved first, since the reader has no way to.
fn read_active_note(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(editor) = workspace
        .active_item(cx)
        .and_then(|item| item.downcast::<Editor>())
    else {
        return;
    };
    let Some((buffer, _)) = email_note(&editor, cx) else {
        return;
    };
    if !buffer.read(cx).is_dirty() {
        replace_with_reader(workspace, &editor, buffer, window, cx);
        return;
    }
    let save = workspace
        .project()
        .update(cx, |project, cx| project.save_buffer(buffer.clone(), cx));
    cx.spawn_in(window, async move |workspace, cx| {
        save.await?;
        workspace.update_in(cx, |workspace, window, cx| {
            replace_with_reader(workspace, &editor, buffer, window, cx);
        })
    })
    .detach_and_log_err(cx);
}

fn replace_with_reader(
    workspace: &mut Workspace,
    editor: &Entity<Editor>,
    buffer: Entity<Buffer>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(pane) = workspace.pane_for(editor) else {
        return;
    };
    let project = workspace.project().clone();
    let workspace_handle = workspace.weak_handle();
    let view = cx.new(|cx| EmailNoteView::new(workspace_handle, project, buffer, window, cx));
    replace_item(&pane, editor.entity_id(), Box::new(view), window, cx);
}

/// Puts `new` where `old` sits in `pane`, keeping focus if the tab had it.
fn replace_item(
    pane: &Entity<Pane>,
    old: EntityId,
    new: Box<dyn ItemHandle>,
    window: &mut Window,
    cx: &mut App,
) {
    pane.update(cx, |pane, cx| {
        let Some(index) = pane.items().position(|item| item.item_id() == old) else {
            return;
        };
        let was_active = pane.active_item().is_some_and(|item| item.item_id() == old);
        // Asked of the item's own handle: the pane's only knows what the
        // last frame drew, and this item may not have had one yet.
        let focused = was_active
            && pane.items().nth(index).is_some_and(|item| {
                let focus = item.item_focus_handle(cx);
                focus.is_focused(window) || focus.contains_focused(window, cx)
            });
        // The old item goes first: a pane holds one item per file, and would
        // take the new one for the old.
        pane.remove_item(old, false, false, window, cx);
        pane.add_item_inner(new, false, focused, was_active, Some(index), window, cx);
    });
}

pub struct EmailNoteView {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    buffer: Entity<Buffer>,
    reader: EmailReader,
    /// The note as last parsed from the buffer, and the hash of its bytes.
    note: Option<(String, InboxItem)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ItemEvent> for EmailNoteView {}

impl EmailReaderHost for EmailNoteView {
    fn email_reader(&mut self) -> &mut EmailReader {
        &mut self.reader
    }
}

impl Focusable for EmailNoteView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.reader.reader_focus.clone()
    }
}

impl EmailNoteView {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        buffer: Entity<Buffer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let vault = inbox_service::service_for_project(&project, cx)
            .and_then(|service| service.read(cx).vault().cloned());
        let reader = EmailReader::new(key_context(), vault, cx);
        let focus = reader.reader_focus.clone();
        let subscriptions = vec![
            cx.subscribe(&buffer, |this, _, event: &BufferEvent, cx| {
                if matches!(
                    event,
                    BufferEvent::Edited { .. }
                        | BufferEvent::Reloaded
                        | BufferEvent::FileHandleChanged
                ) {
                    this.reparse(cx);
                }
            }),
            // The current card's ring follows focus.
            cx.on_focus_in(&focus, window, |_, _, cx| cx.notify()),
            cx.on_focus_out(&focus, window, |_, _, _, cx| cx.notify()),
        ];
        let mut this = Self {
            workspace,
            project,
            buffer,
            reader,
            note: None,
            _subscriptions: subscriptions,
        };
        this.reparse(cx);
        this
    }

    /// Reads the note off the buffer again: sync appended a message, or it
    /// was edited elsewhere.
    fn reparse(&mut self, cx: &mut Context<Self>) {
        let buffer = self.buffer.read(cx);
        let text = buffer.text();
        let path = buffer
            .project_path(cx)
            .map(|path| path.path.as_unix_str().to_string())
            .unwrap_or_default();
        let hash = note_hash(&text);
        if self.note.as_ref().is_some_and(|(shown, _)| *shown == hash) {
            return;
        }
        self.note = Some((hash, parse_inbox_note(&path, &text)));
        cx.emit(ItemEvent::UpdateTab);
        cx.notify();
    }

    fn item(&self) -> Option<&InboxItem> {
        self.note.as_ref().map(|(_, item)| item)
    }

    fn link(&self) -> Option<String> {
        let item = self.item()?;
        item.link
            .clone()
            .or_else(|| item.url.clone())
            .filter(|link| link.starts_with("https://") || link.starts_with("http://"))
    }

    /// `thock::EditEmailNote`: an editor on the same buffer takes this tab's
    /// place, and is remembered so it isn't swapped straight back.
    fn edit(&mut self, _: &EditEmailNote, window: &mut Window, cx: &mut Context<Self>) {
        let buffer = self.buffer.clone();
        let project = self.project.clone();
        let this = cx.entity_id();
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    let Some(pane) = workspace.pane_for_item_id(this) else {
                        return;
                    };
                    let editor = cx.new(|cx| Editor::for_buffer(buffer, Some(project), window, cx));
                    let editor_id = editor.entity_id();
                    cx.default_global::<SourceEditors>().0.insert(editor_id);
                    cx.observe_release(&editor, move |_, _, cx| {
                        cx.default_global::<SourceEditors>().0.remove(&editor_id);
                    })
                    .detach();
                    replace_item(&pane, this, Box::new(editor), window, cx);
                })
                .log_err();
        });
    }

    fn open_link(&mut self, _: &OpenInboxLink, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(link) = self.link() {
            cx.open_url(&link);
        }
    }

    fn scroll_down(&mut self, _: &ScrollInboxReaderDown, _: &mut Window, cx: &mut Context<Self>) {
        self.reader.scroll(READER_LINE, cx);
    }

    fn scroll_up(&mut self, _: &ScrollInboxReaderUp, _: &mut Window, cx: &mut Context<Self>) {
        self.reader.scroll(-READER_LINE, cx);
    }

    fn scroll_page_down(
        &mut self,
        _: &ScrollInboxReaderPageDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let half = self.reader.reader_scroll.bounds().size.height / 2.;
        self.reader.scroll(half, cx);
    }

    fn scroll_page_up(
        &mut self,
        _: &ScrollInboxReaderPageUp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let half = self.reader.reader_scroll.bounds().size.height / 2.;
        self.reader.scroll(-half, cx);
    }

    fn next_message(&mut self, _: &NextInboxMessage, _: &mut Window, cx: &mut Context<Self>) {
        self.reader.step(true, cx);
    }

    fn previous_message(
        &mut self,
        _: &PreviousInboxMessage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reader.step(false, cx);
    }

    fn toggle_folds(&mut self, _: &ToggleInboxFolds, _: &mut Window, cx: &mut Context<Self>) {
        self.reader.toggle_folds(cx);
    }

    /// Who the thread is from, then what can be done with the note, each
    /// with its key.
    fn render_header(&self, window: &Window, cx: &App) -> AnyElement {
        let focus = &self.reader.reader_focus;
        let from = self.item().and_then(|item| item.from.clone());
        let sender = from.as_deref().map(|from| {
            let address = sender_address(from).filter(|address| *address != sender_name(from));
            h_flex()
                .id("email-header-sender")
                .debug_selector(|| "EMAIL_HEADER_SENDER".to_string())
                .min_w_0()
                .gap_1p5()
                .child(
                    div().min_w_0().child(
                        Label::new(sender_name(from))
                            .size(LabelSize::Small)
                            .truncate(),
                    ),
                )
                .children(address.map(|address| {
                    // Far stronger than the name's shrink, so this is gone
                    // before the name loses a character.
                    div().min_w_0().flex_shrink(1000.).child(
                        Label::new(address)
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                }))
                .tooltip(Tooltip::text(from.to_string()))
        });
        let action = |name, action: &dyn gpui::Action, label| {
            let keycap = self.reader.keycap(action, window, cx);
            quiet_action(name, action, keycap, label, false, focus, cx)
        };
        h_flex()
            .flex_none()
            .h(PANE_HEADER_HEIGHT)
            .w_full()
            .pr_10()
            .child(
                h_flex()
                    .w_full()
                    .max_w(self.reader.rule_width(window))
                    .h_full()
                    .pl_10()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .children(sender),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .child(action("edit", &EditEmailNote, "Edit"))
                            .when_some(self.link(), |this, link| {
                                this.child(
                                    action("link", &OpenInboxLink, "Open in Gmail ↗")
                                        .tooltip(Tooltip::text(link)),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

fn key_context() -> KeyContext {
    let mut context = KeyContext::new_with_defaults();
    context.add(EMAIL_VIEW_KEY);
    context.add("menu");
    context
}

impl Item for EmailNoteView {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.reader
            .title()
            .or_else(|| self.item().map(|item| item.title.clone()))
            .unwrap_or_else(|| "Email".to_string())
            .into()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Envelope))
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        None
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(EntityId, &dyn project::ProjectItem),
    ) {
        f(self.buffer.entity_id(), self.buffer.read(cx));
    }

    // One tab per note: opening the note again finds this one.
    fn buffer_kind(&self, _cx: &App) -> ItemBufferKind {
        ItemBufferKind::Singleton
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for EmailNoteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let note = self.note.clone();
        self.reader
            .set_note(note.as_ref().map(|(hash, item)| (hash.as_str(), item)), cx);
        self.reader.after_layout(window, cx);
        let header = self.render_header(window, cx);
        let title = self.reader.render_title(window, cx);
        let body = self.reader.render_body(window, cx);
        v_flex()
            .key_context(key_context())
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .on_action(cx.listener(Self::edit))
            .on_action(cx.listener(Self::open_link))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_page_down))
            .on_action(cx.listener(Self::scroll_page_up))
            .on_action(cx.listener(Self::next_message))
            .on_action(cx.listener(Self::previous_message))
            .on_action(cx.listener(Self::toggle_folds))
            .child(header)
            .child(
                div().flex_none().w_full().pr_10().pt_2p5().child(
                    div()
                        .w_full()
                        .max_w(self.reader.rule_width(window))
                        .pl_10()
                        .child(title),
                ),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{KeyBinding, TestAppContext, VisualTestContext};
    use serde_json::json;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};
    use std::path::Path;
    use workspace::MultiWorkspace;

    const THREAD: &str = "---\nsource:   gmail\ncapture:  b2b2b2b2b2b2\ncaptured: 2026-10-09T19:30:00Z\ntitle:    Farm is down\nfrom:     Ana <ana@example.com>\nlink:     https://mail.google.com/mail/u/me@example.com/#all/t1\n---\n\n# Farm is down\n\n## Ana <ana@example.com> — 2026-10-09 08:55\n\nThe farm stopped taking jobs.\n\n## Rui <rui@example.com> — 2026-10-09 09:13\n\nLooking at it now.\n";
    const PLAIN: &str = "# Groceries\n\n- [ ] Milk\n";

    fn keymap_bindings(content: &str, cx: &App) -> Vec<KeyBinding> {
        match KeymapFile::load(content, cx) {
            KeymapFileLoadResult::Success { key_bindings }
            // Actions from crates this one doesn't link are expected to be
            // missing here; the app crate's keymap guard checks every name.
            | KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
            KeymapFileLoadResult::JsonParseFailure { error } => panic!("bad keymap: {error}"),
        }
    }

    struct Setup {
        project: Entity<Project>,
        workspace: Entity<Workspace>,
        cx: VisualTestContext,
    }

    async fn setup(cx: &mut TestAppContext) -> Setup {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
            init(cx);
            let bindings = keymap_bindings(
                include_str!("../../../assets/keymaps/default-linux.json"),
                cx,
            );
            cx.bind_keys(bindings);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/vault",
            json!({
                "archives": { "inbox": { "farm.md": THREAD } },
                "groceries.md": PLAIN,
            }),
        )
        .await;
        let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let workspace = multi_workspace.read_with(&cx, |multi, _| multi.workspace().clone());
        cx.run_until_parked();
        Setup {
            project,
            workspace,
            cx,
        }
    }

    async fn open(setup: &mut Setup, path: &str) {
        let project_path = setup
            .project
            .read_with(&setup.cx, |project, cx| {
                project.find_project_path(Path::new("/vault").join(path), cx)
            })
            .unwrap();
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap();
        setup.cx.run_until_parked();
    }

    fn reader(setup: &Setup) -> Option<Entity<EmailNoteView>> {
        setup.workspace.read_with(&setup.cx, |workspace, cx| {
            workspace.active_item(cx)?.downcast::<EmailNoteView>()
        })
    }

    fn editors(setup: &Setup) -> usize {
        setup.workspace.read_with(&setup.cx, |workspace, cx| {
            workspace.items_of_type::<Editor>(cx).count()
        })
    }

    fn tabs(setup: &Setup) -> usize {
        setup.workspace.read_with(&setup.cx, |workspace, cx| {
            workspace.active_pane().read(cx).items_len()
        })
    }

    #[gpui::test]
    async fn an_email_note_opens_as_the_conversation(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        open(&mut setup, "archives/inbox/farm.md").await;

        let view = reader(&setup).expect("the reader took the editor's place");
        assert_eq!(editors(&setup), 0);
        assert_eq!(tabs(&setup), 1);
        view.read_with(&setup.cx, |view, cx| {
            let thread = view.reader.reader_note.thread.as_ref().unwrap();
            assert_eq!(thread.messages.len(), 2);
            assert_eq!(view.reader.reader_note.current_message, 1);
            assert_eq!(view.tab_content_text(0, cx).as_ref(), "Farm is down");
        });
        // Focus is the reader's, so its keys answer without a click.
        let focused = setup
            .cx
            .update(|window, cx| view.read(cx).focus_handle(cx).contains_focused(window, cx));
        assert!(focused);

        // Opening the note again finds the tab rather than a second one.
        open(&mut setup, "archives/inbox/farm.md").await;
        assert_eq!(tabs(&setup), 1);
        assert!(reader(&setup).is_some());
    }

    #[gpui::test]
    async fn any_other_note_stays_an_editor(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        open(&mut setup, "groceries.md").await;
        assert!(reader(&setup).is_none());
        assert_eq!(editors(&setup), 1);
    }

    #[gpui::test]
    async fn unsaved_changes_keep_the_editor(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        let project_path = setup
            .project
            .read_with(&setup.cx, |project, cx| {
                project.find_project_path(Path::new("/vault/archives/inbox/farm.md"), cx)
            })
            .unwrap();
        let buffer = setup
            .project
            .update(&mut setup.cx, |project, cx| {
                project.open_buffer(project_path, cx)
            })
            .await
            .unwrap();
        buffer.update(&mut setup.cx, |buffer, cx| {
            buffer.edit([(0..0, " ")], None, cx);
        });
        open(&mut setup, "archives/inbox/farm.md").await;
        assert!(reader(&setup).is_none());
        assert_eq!(editors(&setup), 1);
    }

    #[gpui::test]
    async fn o_shows_the_source_and_the_action_reads_it_again(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        open(&mut setup, "archives/inbox/farm.md").await;
        assert!(reader(&setup).is_some());

        setup.cx.simulate_keystrokes("o");
        setup.cx.run_until_parked();
        assert!(reader(&setup).is_none(), "the editor took the tab");
        assert_eq!(editors(&setup), 1);
        assert_eq!(tabs(&setup), 1);
        let text = setup.workspace.read_with(&setup.cx, |workspace, cx| {
            let editor = workspace.active_item(cx)?.downcast::<Editor>()?;
            Some(editor.read(cx).text(cx))
        });
        assert_eq!(text.as_deref(), Some(THREAD));

        // Asked for, so opening the note again leaves the source up.
        open(&mut setup, "archives/inbox/farm.md").await;
        assert!(reader(&setup).is_none());

        setup.cx.dispatch_action(ReadEmailNote);
        setup.cx.run_until_parked();
        assert!(reader(&setup).is_some());
        assert_eq!(editors(&setup), 0);
        assert_eq!(tabs(&setup), 1);
    }

    #[gpui::test]
    async fn the_readers_keys_step_and_fold(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        open(&mut setup, "archives/inbox/farm.md").await;
        let view = reader(&setup).unwrap();
        let state = |setup: &Setup| {
            view.read_with(&setup.cx, |view, _| {
                (
                    view.reader.reader_note.current_message,
                    view.reader.reader_note.folds_open,
                )
            })
        };
        assert_eq!(state(&setup), (1, false));
        setup.cx.simulate_keystrokes("shift-n");
        assert_eq!(state(&setup), (0, false));
        setup.cx.simulate_keystrokes("shift-n");
        assert_eq!(state(&setup), (0, false), "nothing before the first");
        setup.cx.simulate_keystrokes("n");
        assert_eq!(state(&setup), (1, false));
        setup.cx.simulate_keystrokes("z");
        assert_eq!(state(&setup), (1, true));
        setup.cx.simulate_keystrokes("z");
        assert_eq!(state(&setup), (1, false));
    }

    #[gpui::test]
    async fn a_message_appended_to_the_file_becomes_the_open_card(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        open(&mut setup, "archives/inbox/farm.md").await;
        let view = reader(&setup).unwrap();
        setup.cx.simulate_keystrokes("shift-n z");
        let buffer = view.read_with(&setup.cx, |view, _| view.buffer.clone());
        buffer.update(&mut setup.cx, |buffer, cx| {
            let end = buffer.len();
            buffer.edit(
                [(
                    end..end,
                    "\n## Ana <ana@example.com> — 2026-10-09 10:02\n\nBack up, thanks.\n",
                )],
                None,
                cx,
            );
        });
        setup.cx.run_until_parked();
        view.read_with(&setup.cx, |view, _| {
            let note = &view.reader.reader_note;
            assert_eq!(note.thread.as_ref().unwrap().messages.len(), 3);
            assert_eq!(note.current_message, 2, "the new message opens");
            assert!(note.folds_open, "the folds stay as they were left");
        });
    }

    #[gpui::test]
    fn both_platforms_bind_the_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
        for keymap in [
            include_str!("../../../assets/keymaps/default-linux.json"),
            include_str!("../../../assets/keymaps/default-macos.json"),
        ] {
            let bindings = cx.update(|cx| keymap_bindings(keymap, cx));
            let bound = |action: &dyn gpui::Action| {
                bindings
                    .iter()
                    .any(|binding| binding.action().partial_eq(action))
            };
            assert!(bound(&EditEmailNote));
            assert!(bound(&ReadEmailNote));
        }
    }
}
