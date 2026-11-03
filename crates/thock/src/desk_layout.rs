//! Anchor and Tides (V54): the page is a column of characters pinned to the
//! window's centre, and the side panels tuck away while you type.
//!
//! The workspace places the row on each render (`workspace::center_anchor`);
//! this component tells it how wide the page is, how much room the agent's
//! thread needs, and which sides may ebb, and it ebbs and flows the docks.

use editor::{Editor, EditorEvent, EditorSettings};
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, Focusable as _, Font, Pixels, Subscription,
    Task, WeakEntity, Window, actions,
};
use settings::{Settings as _, SettingsStore, TidePolicy};
use theme_settings::ThemeSettings;
use util::ResultExt as _;
use workspace::dock::{Dock, DockPosition, Tide};
use workspace::{CenterAnchor, ThreadRoom, Workspace};

use crate::{ChatPanel, DeskPanel, PageSettings, ThockSettings};

actions!(
    thock,
    [
        /// Show or tuck away the side panels.
        ToggleTide,
    ]
);

/// How soon a timed entry must start for the Desk's tide mark to light up.
pub const WAITING_WITHIN_MINUTES: u32 = 10;

/// Follows every workspace with Anchor and Tides.
pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        let layout = DeskLayout::new(workspace, window, cx);
        // The action handler keeps the layout alive as long as the workspace.
        workspace.register_action(move |workspace, _: &ToggleTide, window, cx| {
            // Read here, where the window is at hand: a side holding the
            // focus never ebbs (§5.5).
            let focused: Vec<DockPosition> = SIDE_DOCKS
                .into_iter()
                .filter(|position| {
                    workspace
                        .dock_at_position(*position)
                        .read(cx)
                        .focus_handle(cx)
                        .contains_focused(window, cx)
                })
                .collect();
            let layout = layout.downgrade();
            // The workspace is leased while its action runs.
            cx.defer(move |cx| {
                layout
                    .update(cx, |layout, cx| layout.toggle_tide(&focused, cx))
                    .log_err();
            });
        });
    })
    .detach();
}

/// The docks that ebb; the bottom dock never does (§3).
const SIDE_DOCKS: [DockPosition; 2] = [DockPosition::Left, DockPosition::Right];

/// One workspace's page and tides. Which side is tucked away, and why,
/// lives on the docks (`Dock::tide`) and in the row the workspace last
/// placed (`Workspace::center_row`); this keeps no copy of either.
pub struct DeskLayout {
    workspace: WeakEntity<Workspace>,
    /// The thread's room as last asked for.
    thread: Option<ThreadRoom>,
    /// The note in the centre whose typing ebbs the sides.
    editor: Option<(EntityId, Subscription)>,
    /// The centre's active item and its pane count, to tell a new page
    /// from the workspace's other active-item notices (a title, a focus).
    page: Option<(Option<EntityId>, usize)>,
    /// Whether the page was anchored when last looked at.
    anchored: bool,
    /// Brings ebbed sides back after a pause; replacing it restarts the
    /// pause.
    flow_back: Option<Task<()>>,
    /// The chat whose thread asks for room.
    chat: Option<(WeakEntity<ChatPanel>, Subscription)>,
    /// The Desks whose waiting lights a tide mark.
    desks: Vec<(DockPosition, EntityId, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

impl DeskLayout {
    fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let weak_workspace = workspace.weak_handle();
        let workspace_entity = cx.entity();
        let docks = SIDE_DOCKS.map(|position| {
            let dock = workspace.dock_at_position(position).clone();
            let focus = dock.read(cx).focus_handle(cx);
            (position, dock, focus)
        });
        cx.new(|cx| {
            let mut subscriptions = vec![
                cx.subscribe(
                    &workspace_entity,
                    |this: &mut Self, _, event: &workspace::Event, cx| match event {
                        workspace::Event::ActiveItemChanged
                        | workspace::Event::PaneAdded(_)
                        | workspace::Event::PaneRemoved => this.page_changed(cx),
                        _ => {}
                    },
                ),
                // Centred layout has no event of its own.
                cx.observe(&workspace_entity, |this, _, cx| this.anchoring_changed(cx)),
                cx.observe_global::<SettingsStore>(|this, cx| this.settings_changed(cx)),
                theme_settings::observe_buffer_font_size_adjustment(cx, |this, cx| this.sync(cx)),
            ];
            for (position, dock, focus) in docks {
                subscriptions.push(cx.on_focus_in(&focus, window, move |this, _, cx| {
                    this.focus_entered(position, cx)
                }));
                subscriptions.push(cx.observe(&dock, move |this, dock, cx| {
                    this.track_desk(position, &dock, cx)
                }));
            }
            // The workspace is leased by the observer constructing this; look
            // at its centre and its chat once the lease is returned.
            let this = cx.weak_entity();
            cx.defer(move |cx| {
                this.update(cx, |this, cx| {
                    this.page_changed(cx);
                    this.track_chat(cx);
                    this.sync(cx);
                })
                .log_err();
            });
            Self {
                workspace: weak_workspace,
                thread: None,
                editor: None,
                page: None,
                anchored: false,
                flow_back: None,
                chat: None,
                desks: Vec::new(),
                _subscriptions: subscriptions,
            }
        })
    }

    fn dock(&self, position: DockPosition, cx: &App) -> Option<Entity<Dock>> {
        let workspace = self.workspace.upgrade()?;
        Some(workspace.read(cx).dock_at_position(position).clone())
    }

    fn set_tide(&self, position: DockPosition, tide: Tide, cx: &mut Context<Self>) {
        if let Some(dock) = self.dock(position, cx) {
            dock.update(cx, |dock, cx| dock.set_tide(tide, cx));
        }
    }

    fn tide(&self, position: DockPosition, cx: &App) -> Tide {
        self.dock(position, cx)
            .map_or(Tide::Flowing, |dock| dock.read(cx).tide())
    }

    /// What the workspace should keep for the page right now.
    fn anchor(&self, cx: &App) -> CenterAnchor {
        let settings = ThockSettings::get_global(cx);
        let (column, min_column) = page_columns(settings.page, cx);
        let may_ebb = |position: DockPosition| {
            settings
                .tides
                .is_some_and(|tides| tides.policy(position) == TidePolicy::Ebb)
        };
        CenterAnchor {
            column,
            min_column,
            thread: self.thread,
            ebb_left: may_ebb(DockPosition::Left),
            ebb_right: may_ebb(DockPosition::Right),
        }
    }

    /// Hands the workspace the current anchor. Deferred, because the
    /// workspace may be mid-update when a dock or an action gets here.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let Some(workspace) = workspace.upgrade() else {
                return;
            };
            let Some(anchor) = this.read_with(cx, |this, cx| this.anchor(cx)).log_err() else {
                return;
            };
            workspace.update(cx, |workspace, cx| {
                workspace.set_center_anchor(Some(anchor), cx)
            });
        });
    }

    /// Follows the editor in the centre: text typed into it ebbs the sides.
    fn track_editor(&mut self, cx: &mut Context<Self>) {
        let editor = self
            .workspace
            .upgrade()
            .and_then(|workspace| workspace.read(cx).active_item_as::<Editor>(cx));
        let Some(editor) = editor else {
            self.editor = None;
            return;
        };
        if self
            .editor
            .as_ref()
            .is_some_and(|(id, _)| *id == editor.entity_id())
        {
            return;
        }
        let subscription = cx.subscribe(&editor, |this, _, event, cx| {
            // Only typed text: vim's normal mode turns input off, and edits
            // from a panel or a sync never pass through the input handler.
            if let EditorEvent::InputHandled { .. } = event {
                this.typed(cx);
            }
        });
        self.editor = Some((editor.entity_id(), subscription));
    }

    /// Follows the chat mounted over the centre, whose thread asks the page
    /// for room while it shows a message.
    fn track_chat(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self
            .workspace
            .upgrade()
            .and_then(|workspace| ChatPanel::in_workspace(workspace.read(cx)))
        else {
            return;
        };
        let subscription = cx.observe(&chat, |this, chat, cx| {
            let thread = chat.read(cx).thread_room(cx);
            if this.thread != thread {
                this.thread = thread;
                this.sync(cx);
            }
        });
        self.thread = chat.read(cx).thread_room(cx);
        self.chat = Some((chat.downgrade(), subscription));
    }

    fn is_anchored(&self, cx: &App) -> bool {
        self.workspace
            .upgrade()
            .is_some_and(|workspace| workspace.read(cx).is_page_anchored(cx))
    }

    /// You typed in the note (§5.3): the sides that may ebb do, and the
    /// pause before they come back starts over.
    fn typed(&mut self, cx: &mut Context<Self>) {
        let Some(tides) = ThockSettings::get_global(cx).tides else {
            return;
        };
        if !self.is_anchored(cx) {
            return;
        }
        for position in SIDE_DOCKS {
            if tides.policy(position) == TidePolicy::Ebb {
                self.set_tide(position, Tide::Ebbed, cx);
            }
        }
        self.flow_back = tides.flow_back_after.map(|after| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(after).await;
                this.update(cx, |this, cx| this.paused(cx)).log_err();
            })
        });
    }

    /// A pause (§5.4 rule 3): what typing ebbed comes back. A side ebbed to
    /// fit stays a mark; it would only cover the page.
    fn paused(&mut self, cx: &mut Context<Self>) {
        self.flow_back = None;
        for position in SIDE_DOCKS {
            if self.tide(position, cx) == Tide::Ebbed {
                self.set_tide(position, Tide::Flowing, cx);
            }
        }
    }

    /// Whether the side is tucked away: ebbed, drawn as its mark, or kept
    /// whole only because it holds the focus.
    fn is_tucked(&self, position: DockPosition, cx: &App) -> bool {
        self.workspace.upgrade().is_some_and(|workspace| {
            let workspace = workspace.read(cx);
            let row = workspace.center_row();
            workspace.dock_at_position(position).read(cx).is_ebbed()
                || row.tide_mark(position)
                || row.kept_for_focus(position)
        })
    }

    /// Brings a tucked-away side back. Beside an anchored page it comes
    /// back held, so the page makes room for it rather than ebb it to fit
    /// (§4.2); otherwise it simply flows.
    fn flow(&mut self, position: DockPosition, cx: &mut Context<Self>) -> bool {
        if !self.is_tucked(position, cx) {
            return false;
        }
        let tide = if self.is_anchored(cx) {
            Tide::Held
        } else {
            Tide::Flowing
        };
        self.set_tide(position, tide, cx);
        true
    }

    /// Brings every tucked-away side back. Returns whether any was.
    fn flow_all(&mut self, cx: &mut Context<Self>) -> bool {
        let mut flowed = false;
        for position in SIDE_DOCKS {
            flowed |= self.flow(position, cx);
        }
        if flowed {
            self.flow_back = None;
        }
        flowed
    }

    /// Focus went into a dock by any route (§5.4 rule 1): it comes back.
    fn focus_entered(&mut self, position: DockPosition, cx: &mut Context<Self>) {
        self.flow(position, cx);
    }

    /// `thock::ToggleTide` (§5.4 rule 2): brings every tucked-away side
    /// back, or tucks away every side that may ebb when none is. Focus
    /// stays where it is, and a side holding it never ebbs (§5.5).
    fn toggle_tide(&mut self, focused: &[DockPosition], cx: &mut Context<Self>) {
        if self.flow_all(cx) {
            return;
        }
        let Some(tides) = ThockSettings::get_global(cx).tides else {
            return;
        };
        if !self.is_anchored(cx) {
            return;
        }
        self.flow_back = None;
        for position in SIDE_DOCKS {
            if tides.policy(position) == TidePolicy::Ebb && !focused.contains(&position) {
                self.set_tide(position, Tide::Ebbed, cx);
            }
        }
    }

    /// Settings changed: a side that may no longer ebb (`"tides": false`, or
    /// `"stay"`) comes back, and the page is measured again.
    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let tides = ThockSettings::get_global(cx).tides;
        for position in SIDE_DOCKS {
            if tides.is_none_or(|tides| tides.policy(position) == TidePolicy::Stay) {
                self.set_tide(position, Tide::Flowing, cx);
            }
        }
        self.sync(cx);
    }

    /// The page stopped being anchored (centred layout off, a split, nothing
    /// open): every side flows (§5.6), so nothing reappears ebbed or held.
    fn anchoring_changed(&mut self, cx: &mut Context<Self>) {
        let anchored = self.is_anchored(cx);
        if self.anchored == anchored {
            return;
        }
        self.anchored = anchored;
        if !anchored {
            self.flow_back = None;
            for position in SIDE_DOCKS {
                self.set_tide(position, Tide::Flowing, cx);
            }
        }
    }

    /// The page went away (§5.4 rule 4): another item, or a split. Every
    /// side comes back, and the new note's typing is followed.
    fn page_changed(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let page = workspace.read_with(cx, |workspace, cx| {
            Some((
                workspace.active_item(cx).map(|item| item.item_id()),
                workspace.panes().len(),
            ))
        });
        if self.page == page {
            return;
        }
        self.page = page;
        self.track_editor(cx);
        self.anchoring_changed(cx);
        self.flow_all(cx);
    }

    /// Follows the Desk when it is the dock's panel, so its tide mark
    /// lights up while something waits (§5.2).
    fn track_desk(&mut self, position: DockPosition, dock: &Entity<Dock>, cx: &mut Context<Self>) {
        let desk = dock
            .read(cx)
            .active_panel()
            .and_then(|panel| panel.to_any().downcast::<DeskPanel>().ok());
        let tracked = self
            .desks
            .iter()
            .find(|(side, _, _)| *side == position)
            .map(|(_, id, _)| *id);
        if desk.as_ref().map(|desk| desk.entity_id()) == tracked {
            return;
        }
        self.desks.retain(|(side, _, _)| *side != position);
        let Some(desk) = desk else {
            dock.update(cx, |dock, cx| dock.set_tide_waiting(false, cx));
            return;
        };
        let subscription = cx.observe(&desk, {
            let dock = dock.downgrade();
            move |_, desk, cx| {
                let waiting = desk.read(cx).has_waiting();
                dock.update(cx, |dock, cx| dock.set_tide_waiting(waiting, cx))
                    .log_err();
            }
        });
        self.desks.push((position, desk.entity_id(), subscription));
        let waiting = desk.read(cx).has_waiting();
        dock.update(cx, |dock, cx| dock.set_tide_waiting(waiting, cx));
    }
}

/// The page's column and the narrowest it may get (§4.1): the measure in
/// `0` advances of the body font at the buffer font size, plus the
/// editor's own padding.
fn page_columns(page: PageSettings, cx: &App) -> (Pixels, Pixels) {
    let theme = ThemeSettings::get_global(cx);
    let font_size = theme.buffer_font_size(cx);
    let font = match ThockSettings::get_global(cx).body_family() {
        Some(family) => Font {
            family,
            ..theme.buffer_font.clone()
        },
        None => theme.buffer_font.clone(),
    };
    let text_system = cx.text_system();
    let font_id = text_system.resolve_font(&font);
    let advance = text_system
        .ch_advance(font_id, font_size)
        .log_err()
        .unwrap_or(font_size / 2.);
    let padding = editor_padding(font_id, font_size, cx);
    let columns = |measure: u32| advance * measure as f32 + padding;
    (columns(page.measure), columns(page.min_measure))
}

/// What a note's editor takes beside its text: the gutter of a single
/// buffer without line numbers (the fold markers and their margin) and the
/// vertical scrollbar. Line numbers, when on, come out of the measure.
fn editor_padding(font_id: gpui::FontId, font_size: Pixels, cx: &App) -> Pixels {
    let text_system = cx.text_system();
    let folds = if EditorSettings::get_global(cx).gutter.folds {
        text_system
            .ch_width(font_id, font_size)
            .log_err()
            .unwrap_or(font_size / 2.)
            * 3.
    } else {
        Pixels::ZERO
    };
    let margin = -text_system.descent(font_id, font_size);
    folds + margin + ui::EDITOR_SCROLLBAR_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog_panel::BacklogFolds;
    use fs::FakeFs;
    use gpui::{
        TestAppContext, UpdateGlobal as _, VisualContext as _, VisualTestContext, px, size,
    };
    use project::Project;
    use serde_json::json;
    use settings::{
        FlowBackAfter, KeymapFile, KeymapFileLoadResult, ThockSettingsContent, TidesContent,
        TidesSettingsContent,
    };
    use std::path::PathBuf;
    use std::time::Duration;
    use workspace::MultiWorkspace;
    use workspace::dock::test::TestPanel;

    const NOTE: &str = "# Day planner\n- [ ] Workout\n- [ ] 15:00 – 15:30 Standup\n";
    const BACKLOG: &str = "## Soon\n\n- [ ] Write the report\n- [ ] Call the bank\n";

    fn bind(content: &str, cx: &mut App) {
        let bindings = match KeymapFile::load(content, cx) {
            KeymapFileLoadResult::Success { key_bindings }
            | KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
            KeymapFileLoadResult::JsonParseFailure { error } => panic!("bad keymap: {error}"),
        };
        cx.bind_keys(bindings);
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
            crate::agent_panel::init(cx);
            crate::chat_panel::init(cx);
            crate::desk_panel::init(cx);
            init(cx);
            // The shipped keymap, so a missing or shadowed binding fails here.
            bind(
                include_str!("../../../assets/keymaps/default-linux.json"),
                cx,
            );
        });
    }

    struct Setup {
        workspace: Entity<Workspace>,
        desk: Entity<DeskPanel>,
        editor: Entity<Editor>,
        root: PathBuf,
        cx: VisualTestContext,
        _vault_dir: tempfile::TempDir,
    }

    /// A vault on a 24-inch window with centred layout on, a rail open on
    /// the left and the Desk on the right, and `open` in the centre with
    /// the focus in it.
    async fn setup_with(cx: &mut TestAppContext, open: &str) -> Setup {
        setup_with_rail(cx, open, |cx| TestPanel::new(DockPosition::Left, 100, cx)).await
    }

    async fn setup_with_rail(
        cx: &mut TestAppContext,
        open: &str,
        rail: impl FnOnce(&mut App) -> TestPanel,
    ) -> Setup {
        init_test(cx);
        let vault_dir = tempfile::tempdir().unwrap();
        let root = vault_dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".thock")).unwrap();
        std::fs::write(root.join(".thock/config.toml"), "schema = 1\n").unwrap();
        // `backlog.md` must exist on disk for the Backlog to adopt its buffer.
        std::fs::write(root.join("backlog.md"), BACKLOG).unwrap();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            &root,
            json!({
                ".thock": { "config.toml": "schema = 1\n" },
                "backlog.md": BACKLOG,
                "daily": {},
            }),
        )
        .await;
        // Today's, so the Desk writes a ticked task into it.
        fs.insert_file(&root.join(today_note()), NOTE.as_bytes().to_vec())
            .await;
        let project = Project::test(fs.clone(), [root.as_path()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let mut cx = cx.clone();
        let workspace = multi_workspace.read_with(&cx, |multi, _| multi.workspace().clone());
        let desk = workspace.update_in(&mut cx, |workspace, window, cx| {
            let rail = cx.new(|cx| rail(cx));
            workspace.add_panel(rail, window, cx);
            workspace.open_panel::<TestPanel>(window, cx);
            let desk = DeskPanel::new(workspace, BacklogFolds::default(), window, cx);
            workspace.add_panel(desk.clone(), window, cx);
            workspace.open_panel::<DeskPanel>(window, cx);
            workspace.centered_layout = true;
            desk
        });
        cx.simulate_resize(size(px(1920.), px(1080.)));
        cx.run_until_parked();
        let project_path = project
            .read_with(&cx, |project, cx| {
                project.find_project_path(root.join(open), cx)
            })
            .expect("the note is in the vault");
        let editor = workspace
            .update_in(&mut cx, |workspace, window, cx| {
                workspace.open_path(project_path, None, true, window, cx)
            })
            .await
            .unwrap()
            .downcast::<Editor>()
            .unwrap();
        cx.focus(&editor);
        cx.run_until_parked();
        Setup {
            workspace,
            desk,
            editor,
            root,
            cx,
            _vault_dir: vault_dir,
        }
    }

    async fn setup(cx: &mut TestAppContext) -> Setup {
        setup_with(cx, &today_note()).await
    }

    fn today_note() -> String {
        format!(
            "daily/{}.md",
            chrono::Local::now().date_naive().format("%Y-%m-%d")
        )
    }

    fn dock(setup: &Setup, position: DockPosition) -> Entity<Dock> {
        setup.workspace.read_with(&setup.cx, |workspace, _| {
            workspace.dock_at_position(position).clone()
        })
    }

    /// Drawn as its tide mark in the last frame, while still open.
    fn ebbed(setup: &Setup, position: DockPosition) -> bool {
        setup.workspace.read_with(&setup.cx, |workspace, cx| {
            assert!(
                workspace.dock_at_position(position).read(cx).is_open(),
                "an ebbed dock stays open"
            );
            workspace.dock_shows_tide_mark(position)
        })
    }

    fn page_left(setup: &Setup) -> Pixels {
        setup.editor.read_with(&setup.cx, |editor, _| {
            editor
                .last_bounds()
                .map(|bounds| bounds.origin.x)
                .expect("the note was drawn")
        })
    }

    fn advance(setup: &mut Setup, duration: Duration) {
        setup.cx.executor().advance_clock(duration);
        setup.cx.run_until_parked();
    }

    fn type_in_note(setup: &mut Setup, keys: &str) {
        setup.cx.simulate_keystrokes(keys);
        setup.cx.run_until_parked();
    }

    fn update_thock(setup: &mut Setup, update: impl FnOnce(&mut ThockSettingsContent)) {
        setup.cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    update(settings.thock.get_or_insert_default())
                });
            });
        });
        setup.cx.run_until_parked();
    }

    fn tides(setup: &mut Setup, tides: TidesSettingsContent) {
        update_thock(setup, |thock| {
            thock.tides = Some(TidesContent::Settings(tides));
        });
    }

    fn is_focused<V: gpui::Focusable + 'static>(setup: &mut Setup, entity: &Entity<V>) -> bool {
        setup.cx.update(|window, cx| {
            entity
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        })
    }

    #[gpui::test]
    async fn the_page_does_not_move_when_a_dock_opens_closes_ebbs_or_resizes(
        cx: &mut TestAppContext,
    ) {
        let mut setup = setup(cx).await;
        let left = page_left(&setup);
        let centre = setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.anchor_layout())
            .expect("the page is anchored");
        let window = px(1920.);
        assert_eq!(
            px(300.) + centre.left_padding,
            (window - centre.column) / 2.,
            "the page sits in the window's centre"
        );

        let toggle_desk = |setup: &mut Setup| {
            setup
                .workspace
                .update_in(&mut setup.cx, |workspace, window, cx| {
                    workspace.toggle_dock(DockPosition::Right, window, cx)
                });
            setup.cx.focus(&setup.editor);
            setup.cx.run_until_parked();
        };
        toggle_desk(&mut setup);
        assert_eq!(page_left(&setup), left, "closing the Desk");
        toggle_desk(&mut setup);
        assert_eq!(page_left(&setup), left, "opening it again");

        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Right));
        assert_eq!(page_left(&setup), left, "ebbing it");

        advance(&mut setup, Duration::from_secs(9));
        assert!(!ebbed(&setup, DockPosition::Right));
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.right_dock().update(cx, |dock, cx| {
                    dock.resize_active_panel(Some(px(420.)), None, window, cx)
                })
            });
        setup.cx.run_until_parked();
        assert_eq!(page_left(&setup), left, "resizing it");
    }

    #[gpui::test]
    async fn typing_ebbs_both_sides_and_a_pause_brings_them_back(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        assert!(!ebbed(&setup, DockPosition::Left) && !ebbed(&setup, DockPosition::Right));

        type_in_note(&mut setup, "a b c");
        assert!(ebbed(&setup, DockPosition::Left), "the rail ebbs");
        assert!(ebbed(&setup, DockPosition::Right), "the Desk ebbs");
        let editor = setup.editor.clone();
        assert!(is_focused(&mut setup, &editor));

        advance(&mut setup, Duration::from_secs(5));
        type_in_note(&mut setup, "d");
        advance(&mut setup, Duration::from_secs(5));
        assert!(
            ebbed(&setup, DockPosition::Right),
            "typing restarts the pause"
        );
        advance(&mut setup, Duration::from_secs(4));
        assert!(!ebbed(&setup, DockPosition::Left));
        assert!(
            !ebbed(&setup, DockPosition::Right),
            "eight quiet seconds bring it back"
        );
    }

    #[gpui::test]
    async fn without_a_pause_rule_the_sides_wait_to_be_asked(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        tides(
            &mut setup,
            TidesSettingsContent {
                flow_back_after: Some(FlowBackAfter::Never),
                ..Default::default()
            },
        );
        type_in_note(&mut setup, "a");
        advance(&mut setup, Duration::from_secs(120));
        assert!(ebbed(&setup, DockPosition::Right), "null never flows back");
        setup.cx.simulate_keystrokes("ctrl-alt-\\");
        setup.cx.run_until_parked();
        assert!(!ebbed(&setup, DockPosition::Right));
    }

    #[gpui::test]
    async fn a_null_in_settings_json_means_never(cx: &mut TestAppContext) {
        let content: ThockSettingsContent =
            serde_json::from_str(r#"{ "tides": { "flow_back_after": null } }"#).unwrap();
        let Some(TidesContent::Settings(tides)) = content.tides else {
            panic!("a tides block");
        };
        assert_eq!(tides.flow_back_after, Some(FlowBackAfter::Never));
        let content: ThockSettingsContent = serde_json::from_str(r#"{ "tides": false }"#).unwrap();
        assert_eq!(content.tides, Some(TidesContent::Enabled(false)));
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            let settings = ThockSettings::get_global(cx);
            assert_eq!(settings.page.measure, 68);
            assert_eq!(settings.page.min_measure, 48);
            let tides = settings.tides.expect("tides are on by default");
            assert_eq!(tides.left, TidePolicy::Ebb);
            assert_eq!(tides.right, TidePolicy::Ebb);
            assert_eq!(tides.flow_back_after, Some(Duration::from_secs(8)));
        });
    }

    #[gpui::test]
    async fn a_larger_font_widens_the_page_and_keeps_its_measure(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        let column = |setup: &Setup| {
            setup.workspace.read_with(&setup.cx, |workspace, _| {
                workspace.center_anchor().map(|anchor| anchor.column)
            })
        };
        let before = column(&setup).expect("the page is anchored");
        setup
            .cx
            .update(|_, cx| theme_settings::increase_buffer_font_size(cx));
        setup.cx.run_until_parked();
        let after = column(&setup).expect("the page is anchored");
        assert!(after > before, "{after:?} should be wider than {before:?}");
    }

    #[gpui::test]
    async fn tides_off_keeps_every_side_open(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        update_thock(&mut setup, |thock| {
            thock.tides = Some(TidesContent::Enabled(false))
        });
        type_in_note(&mut setup, "a b");
        assert!(!ebbed(&setup, DockPosition::Left) && !ebbed(&setup, DockPosition::Right));
    }

    #[gpui::test]
    async fn vim_motions_read_without_ebbing(cx: &mut TestAppContext) {
        // Vim's normal mode turns the editor's input off, so its keys reach
        // the editor as ignored input; this crate's tests don't link vim,
        // whose actions some of them stand in for.
        let mut setup = setup(cx).await;
        let editor = setup.editor.clone();
        editor.update(&mut setup.cx, |editor, _| editor.set_input_enabled(false));
        type_in_note(&mut setup, "j j k");
        assert!(
            !ebbed(&setup, DockPosition::Right),
            "normal mode is reading"
        );
        editor.update(&mut setup.cx, |editor, _| editor.set_input_enabled(true));
        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Right), "insert mode is writing");
    }

    #[gpui::test]
    async fn ticking_a_task_from_the_desk_does_not_ebb(cx: &mut TestAppContext) {
        // The tick writes the task into today's note, the page in the centre.
        let mut setup = setup(cx).await;
        // The first row is the Soon section's lane; the second is a task.
        setup.cx.simulate_keystrokes("ctrl-alt-u down down x");
        setup.cx.run_until_parked();
        let open = setup
            .desk
            .read_with(&setup.cx, |desk, cx| desk.backlog().read(cx).open_count());
        assert_eq!(open, 1, "the task was ticked");
        let page = setup
            .editor
            .read_with(&setup.cx, |editor, cx| editor.text(cx));
        assert!(page.contains("- [x] Write the report"), "into the page");
        assert!(!ebbed(&setup, DockPosition::Left) && !ebbed(&setup, DockPosition::Right));
    }

    #[gpui::test]
    async fn a_focus_key_brings_the_desk_back_focused_with_its_selection(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        let backlog = setup
            .desk
            .read_with(&setup.cx, |desk, _| desk.backlog().clone());
        let selected =
            |setup: &Setup| backlog.read_with(&setup.cx, |backlog, _| backlog.selected_for_tests());
        setup.cx.simulate_keystrokes("ctrl-alt-u down down");
        setup.cx.run_until_parked();
        let before = selected(&setup);
        assert!(before.is_some());
        setup.cx.focus(&setup.editor);
        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Right));

        setup.cx.simulate_keystrokes("ctrl-alt-u");
        setup.cx.run_until_parked();
        assert!(
            !ebbed(&setup, DockPosition::Right),
            "one key brings it back"
        );
        assert!(is_focused(&mut setup, &backlog), "and focuses it");
        assert_eq!(selected(&setup), before, "where you left it");
        assert!(
            ebbed(&setup, DockPosition::Left),
            "the other side stays tucked away"
        );
    }

    #[gpui::test]
    async fn the_tide_key_shows_and_tucks_away_both_sides_without_moving_focus(
        cx: &mut TestAppContext,
    ) {
        let mut setup = setup(cx).await;
        let editor = setup.editor.clone();
        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Left) && ebbed(&setup, DockPosition::Right));

        setup.cx.simulate_keystrokes("ctrl-alt-\\");
        setup.cx.run_until_parked();
        assert!(!ebbed(&setup, DockPosition::Left) && !ebbed(&setup, DockPosition::Right));
        assert!(is_focused(&mut setup, &editor));

        setup.cx.simulate_keystrokes("ctrl-alt-\\");
        setup.cx.run_until_parked();
        assert!(ebbed(&setup, DockPosition::Left) && ebbed(&setup, DockPosition::Right));
        assert!(is_focused(&mut setup, &editor));
    }

    #[gpui::test]
    async fn a_staying_side_never_ebbs(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        tides(
            &mut setup,
            TidesSettingsContent {
                right: Some(TidePolicy::Stay),
                ..Default::default()
            },
        );
        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Left));
        assert!(!ebbed(&setup, DockPosition::Right), "a staying Desk stays");
        setup.cx.simulate_keystrokes("ctrl-alt-\\ ctrl-alt-\\");
        setup.cx.run_until_parked();
        assert!(!ebbed(&setup, DockPosition::Right), "even for the tide key");

        // Nor does it ebb to fit: on the 14-inch window with a wide Desk,
        // the page shifts instead.
        setup.cx.simulate_resize(size(px(1512.), px(982.)));
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.right_dock().update(cx, |dock, cx| {
                    dock.resize_active_panel(Some(px(440.)), None, window, cx)
                })
            });
        setup.cx.run_until_parked();
        assert!(!ebbed(&setup, DockPosition::Right));
    }

    #[gpui::test]
    async fn a_wide_desk_on_a_small_window_waits_at_the_edge_until_called(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        setup.cx.simulate_resize(size(px(1512.), px(982.)));
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.right_dock().update(cx, |dock, cx| {
                    dock.resize_active_panel(Some(px(440.)), None, window, cx)
                })
            });
        setup.cx.run_until_parked();
        let left = page_left(&setup);
        assert!(ebbed(&setup, DockPosition::Right), "it ebbs to fit");
        let ebbed_by_hand =
            dock(&setup, DockPosition::Right).read_with(&setup.cx, |dock, _| dock.is_ebbed());
        assert!(!ebbed_by_hand, "decided on each render, never stored");

        type_in_note(&mut setup, "a");
        advance(&mut setup, Duration::from_secs(9));
        assert!(
            ebbed(&setup, DockPosition::Right),
            "a pause doesn't bring it over the page"
        );

        setup.cx.simulate_keystrokes("ctrl-alt-\\");
        setup.cx.run_until_parked();
        assert!(
            !ebbed(&setup, DockPosition::Right),
            "called, it comes back as a dock"
        );
        assert!(page_left(&setup) < left, "and the page shifts to make room");

        type_in_note(&mut setup, "b");
        assert!(ebbed(&setup, DockPosition::Right));
        assert_eq!(page_left(&setup), left, "ebbing again centres the page");
    }

    #[gpui::test]
    async fn a_split_centre_never_ebbs(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        setup.cx.simulate_keystrokes("ctrl-\\");
        setup.cx.run_until_parked();
        let panes = setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.panes().len());
        assert_eq!(panes, 2);
        type_in_note(&mut setup, "a b");
        assert!(!ebbed(&setup, DockPosition::Left) && !ebbed(&setup, DockPosition::Right));
        let anchored = setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.anchor_layout());
        assert_eq!(anchored, None, "a split is Zed's own layout");
    }

    #[gpui::test]
    async fn another_note_brings_the_sides_back(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Right));
        let project = setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.project().clone());
        let project_path = project
            .read_with(&setup.cx, |project, cx| {
                project.find_project_path(setup.root.join("backlog.md"), cx)
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
        assert!(!ebbed(&setup, DockPosition::Left) && !ebbed(&setup, DockPosition::Right));
    }

    #[gpui::test]
    async fn ebbed_is_not_saved_with_the_workspace(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Right));
        let docks = setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.capture_dock_state(window, cx)
            });
        assert!(docks.right.visible, "saved as open");

        // Restoring into a dock that starts closed, as a new session's does.
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace
                    .right_dock()
                    .update(cx, |dock, cx| dock.set_open(false, window, cx));
                workspace.set_dock_structure(docks, window, cx);
            });
        setup.cx.focus(&setup.editor);
        setup.cx.run_until_parked();
        assert!(
            !ebbed(&setup, DockPosition::Right),
            "it comes back open and flowing"
        );
    }

    #[gpui::test]
    async fn the_desk_mark_lights_up_when_an_entry_is_about_to_start(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        let waiting = |setup: &Setup| {
            dock(setup, DockPosition::Right).read_with(&setup.cx, |dock, _| dock.tide_waiting())
        };
        let today = chrono::Local::now().date_naive();
        let at = |hour, minute| today.and_hms_opt(hour, minute, 0).unwrap();
        setup
            .desk
            .update(&mut setup.cx, |desk, cx| desk.set_now(at(14, 0), cx));
        setup.cx.run_until_parked();
        assert!(!waiting(&setup), "an hour away is not waiting");

        setup
            .desk
            .update(&mut setup.cx, |desk, cx| desk.set_now(at(14, 52), cx));
        setup.cx.run_until_parked();
        assert!(waiting(&setup), "eight minutes away is");
        type_in_note(&mut setup, "a");
        assert!(
            ebbed(&setup, DockPosition::Right),
            "and the mark carries it"
        );
    }

    fn column(setup: &Setup) -> Pixels {
        setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.anchor_layout())
            .expect("the page is anchored")
            .column
    }

    /// The page's left edge sits where a centred column starts, give or
    /// take layout's rounding to half pixels.
    fn assert_centred(setup: &Setup) {
        let left = page_left(setup);
        let centred = (px(1920.) - column(setup)) / 2.;
        assert!(
            (left - centred).abs() < px(1.),
            "{left:?} against {centred:?}"
        );
    }

    #[gpui::test]
    async fn the_page_is_centred_beside_a_flexible_rail(cx: &mut TestAppContext) {
        let setup = setup_with_rail(cx, &today_note(), |cx| {
            TestPanel::new_flexible(DockPosition::Left, 100, cx)
        })
        .await;
        assert_centred(&setup);
    }

    #[gpui::test]
    async fn the_page_is_centred_beside_a_rail_held_at_its_minimum(cx: &mut TestAppContext) {
        let setup = setup_with_rail(cx, &today_note(), |cx| TestPanel {
            min_size: Some(px(420.)),
            ..TestPanel::new(DockPosition::Left, 100, cx)
        })
        .await;
        assert_centred(&setup);
    }

    #[gpui::test]
    async fn the_tide_key_never_tucks_away_the_panel_you_are_in(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        let backlog = setup
            .desk
            .read_with(&setup.cx, |desk, _| desk.backlog().clone());
        setup.cx.simulate_keystrokes("ctrl-alt-u");
        setup.cx.run_until_parked();
        assert!(is_focused(&mut setup, &backlog));

        setup.cx.simulate_keystrokes("ctrl-alt-\\");
        setup.cx.run_until_parked();
        assert!(
            !ebbed(&setup, DockPosition::Right),
            "the Desk you are in stays"
        );
        assert!(is_focused(&mut setup, &backlog), "and keeps the focus");
        assert!(ebbed(&setup, DockPosition::Left), "the rail tucks away");
    }

    #[gpui::test]
    async fn a_focused_desk_never_ebbs_to_fit(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        let backlog = setup
            .desk
            .read_with(&setup.cx, |desk, _| desk.backlog().clone());
        setup.cx.simulate_keystrokes("ctrl-alt-u");
        setup.cx.run_until_parked();
        setup.cx.simulate_resize(size(px(1512.), px(982.)));
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.right_dock().update(cx, |dock, cx| {
                    dock.resize_active_panel(Some(px(440.)), None, window, cx)
                })
            });
        setup.cx.run_until_parked();
        assert!(
            !ebbed(&setup, DockPosition::Right),
            "it would trap the focus"
        );
        assert!(is_focused(&mut setup, &backlog));

        setup.cx.focus(&setup.editor);
        setup.cx.run_until_parked();
        assert!(
            ebbed(&setup, DockPosition::Right),
            "back in the note, it ebbs to fit again"
        );
    }

    #[gpui::test]
    async fn turning_tides_off_brings_every_side_back(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Left) && ebbed(&setup, DockPosition::Right));
        update_thock(&mut setup, |thock| {
            thock.tides = Some(TidesContent::Enabled(false))
        });
        assert!(!ebbed(&setup, DockPosition::Left) && !ebbed(&setup, DockPosition::Right));
        setup.cx.simulate_keystrokes("ctrl-alt-\\");
        setup.cx.run_until_parked();
        assert!(
            !ebbed(&setup, DockPosition::Left) && !ebbed(&setup, DockPosition::Right),
            "the tide key tucks nothing away with tides off"
        );
    }

    #[gpui::test]
    async fn a_side_switched_to_stay_comes_back(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Right));
        tides(
            &mut setup,
            TidesSettingsContent {
                right: Some(TidePolicy::Stay),
                flow_back_after: Some(FlowBackAfter::Never),
                ..Default::default()
            },
        );
        assert!(
            !ebbed(&setup, DockPosition::Right),
            "a staying side comes back"
        );
        assert!(ebbed(&setup, DockPosition::Left), "the other one waits");
    }

    #[gpui::test]
    async fn centred_layout_off_brings_every_side_back_for_good(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        type_in_note(&mut setup, "a");
        assert!(ebbed(&setup, DockPosition::Right));
        let toggle = |setup: &mut Setup| {
            setup
                .workspace
                .update_in(&mut setup.cx, |workspace, window, cx| {
                    workspace.toggle_centered_layout(&workspace::ToggleCenteredLayout, window, cx)
                });
            setup.cx.run_until_parked();
        };
        toggle(&mut setup);
        assert!(!ebbed(&setup, DockPosition::Right));
        let still_ebbed =
            dock(&setup, DockPosition::Right).read_with(&setup.cx, |dock, _| dock.is_ebbed());
        assert!(
            !still_ebbed,
            "every side flows while the page isn't anchored"
        );
        toggle(&mut setup);
        assert!(
            !ebbed(&setup, DockPosition::Left) && !ebbed(&setup, DockPosition::Right),
            "and nothing comes back ebbed"
        );
    }

    #[gpui::test]
    async fn a_split_leaves_nothing_held_once_it_closes(cx: &mut TestAppContext) {
        let mut setup = setup(cx).await;
        setup.cx.simulate_resize(size(px(1512.), px(982.)));
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                workspace.right_dock().update(cx, |dock, cx| {
                    dock.resize_active_panel(Some(px(440.)), None, window, cx)
                })
            });
        setup.cx.run_until_parked();
        assert!(ebbed(&setup, DockPosition::Right), "it ebbs to fit");

        setup.cx.simulate_keystrokes("ctrl-\\");
        setup.cx.run_until_parked();
        let tide = dock(&setup, DockPosition::Right).read_with(&setup.cx, |dock, _| dock.tide());
        assert_eq!(tide, Tide::Flowing, "a split holds nothing back");

        setup.cx.simulate_keystrokes("ctrl-w");
        setup.cx.run_until_parked();
        let panes = setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.panes().len());
        assert_eq!(panes, 1);
        setup.cx.focus(&setup.editor);
        setup.cx.run_until_parked();
        assert!(
            ebbed(&setup, DockPosition::Right),
            "back to one page, the wide Desk ebbs to fit again"
        );
    }

    #[gpui::test]
    async fn a_flexible_rail_resizes_beside_an_anchored_page(cx: &mut TestAppContext) {
        let mut setup = setup_with_rail(cx, &today_note(), |cx| {
            TestPanel::new_flexible(DockPosition::Left, 100, cx)
        })
        .await;
        setup.cx.simulate_resize(size(px(2560.), px(1440.)));
        setup.cx.run_until_parked();
        setup
            .workspace
            .update_in(&mut setup.cx, |workspace, window, cx| {
                let flex = workspace.dock_flex_for_size(DockPosition::Left, px(400.), window, cx);
                workspace.left_dock().update(cx, |dock, cx| {
                    dock.resize_active_panel(Some(px(400.)), flex, window, cx)
                })
            });
        setup.cx.run_until_parked();
        let rail = setup
            .workspace
            .read_with(&setup.cx, |workspace, _| workspace.anchor_layout())
            .expect("the page is anchored")
            .left;
        assert!(!rail.mark);
        assert!(
            (rail.width - px(400.)).abs() < px(1.),
            "drawn at the width it was resized to: {:?}",
            rail.width
        );
    }
}
