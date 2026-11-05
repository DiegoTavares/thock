//! The *Connect an agent* modal (`v55-agents-bridge.md` §7, §11): the
//! pairing command to type on the other machine, shown until a client
//! pairs or the user closes it, and the picker *Disconnect agent* opens.
//! Mirrors `connect_phone.rs`.

use gpui::{
    App, ClipboardItem, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, SharedString,
    Subscription, Task, WeakEntity,
};
use picker::{Picker, PickerDelegate};
use settings::Settings as _;
use std::sync::Arc;
use ui::prelude::*;
use ui::{Headline, HeadlineSize, Icon, IconName, IconSize, KeyBinding, Label, ListItem};
use util::ResultExt as _;
use workspace::ModalView;

use crate::bridge_service::{BridgeService, ClientView, PairingInvite};
use crate::calendar_service::ManualSyncFinished;

pub struct ConnectAgentModal {
    focus_handle: FocusHandle,
    invite: Option<PairingInvite>,
    /// Which of the desk's addresses the command names; `up`/`down` cycle it.
    address_index: usize,
    error: Option<SharedString>,
    copied: bool,
    /// How many machines were paired when the modal opened; one more means
    /// the command worked.
    clients_before: usize,
    _pairing: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for ConnectAgentModal {}
impl ModalView for ConnectAgentModal {}

impl Focusable for ConnectAgentModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ConnectAgentModal {
    pub fn new(
        service: WeakEntity<BridgeService>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (clients_before, pairing, subscription) = match service.upgrade() {
            Some(service) => {
                let clients_before = service.read(cx).clients().len();
                let pairing = service.update(cx, |service, cx| service.begin_pairing(cx));
                let subscription = cx.observe(&service, Self::service_changed);
                (clients_before, Some(pairing), subscription)
            }
            None => (0, None, Subscription::new(|| {})),
        };
        let pairing_task = pairing.map(|pairing| {
            cx.spawn(async move |this, cx| {
                let result = pairing.await;
                this.update(cx, |this, cx| {
                    match result {
                        Ok(invite) => this.invite = Some(invite),
                        Err(error) => this.error = Some(format!("{error:#}").into()),
                    }
                    cx.notify();
                })
                .ok();
            })
        });
        Self {
            focus_handle: cx.focus_handle(),
            invite: None,
            address_index: 0,
            error: None,
            copied: false,
            clients_before,
            _pairing: pairing_task,
            _subscription: subscription,
        }
    }

    fn service_changed(&mut self, service: gpui::Entity<BridgeService>, cx: &mut Context<Self>) {
        if self.invite.is_none() {
            return;
        }
        let clients = service.read(cx).clients();
        let code_spent = !service.read(cx).has_pending_code();
        match modal_change(code_spent, self.clients_before, &clients) {
            ModalChange::Stay => {}
            ModalChange::Close => cx.emit(DismissEvent),
            ModalChange::Paired(name) => {
                service.update(cx, |_, cx| {
                    cx.emit(ManualSyncFinished {
                        message: format!(
                            "{name} is connected — its agents can reach this vault now"
                        )
                        .into(),
                        icon: IconName::Check,
                    })
                });
                cx.emit(DismissEvent);
            }
        }
    }

    fn command(&self) -> Option<String> {
        let invite = self.invite.as_ref()?;
        let address = invite.addresses.get(self.address_index)?;
        Some(invite.command(address))
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    /// Enter copies the command; the modal stays until the machine pairs.
    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        self.copy(cx);
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(command) = self.command() {
            cx.write_to_clipboard(ClipboardItem::new_string(command));
            self.copied = true;
            cx.notify();
        }
    }

    fn select_next(&mut self, _: &menu::SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_address(1, cx);
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_address(-1, cx);
    }

    fn cycle_address(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(invite) = &self.invite else {
            return;
        };
        let count = invite.addresses.len();
        if count == 0 {
            return;
        }
        self.address_index =
            (self.address_index as isize + step).rem_euclid(count as isize) as usize;
        self.copied = false;
        cx.notify();
    }
}

impl Render for ConnectAgentModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = |text: &str| {
            Label::new(text.to_string())
                .size(LabelSize::Small)
                .color(Color::Muted)
        };
        let command = self.command();
        let addresses: Vec<String> = self
            .invite
            .as_ref()
            .map(|invite| invite.addresses.clone())
            .unwrap_or_default();
        v_flex()
            .key_context("ThockConnectAgentModal menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .elevation_2(cx)
            .w(rems(36.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .gap_1p5()
                    .child(Icon::new(IconName::Terminal).size(IconSize::XSmall))
                    .child(Headline::new("Connect an agent").size(HeadlineSize::XSmall)),
            )
            .child(div().px_3().pb_2().child(muted(
                "On the other computer, run this command. It pairs that machine with this vault \
                 for as long as you keep it connected; the agents there get the vault's memory \
                 and can leave notes in it.",
            )))
            .child(
                v_flex()
                    .px_3()
                    .py_3()
                    .gap_2()
                    .bg(cx.theme().colors().editor_background)
                    .border_t_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .map(|this| match (&command, &self.error) {
                        (Some(command), _) => this
                            .child(
                                div()
                                    .id("thock-pairing-command")
                                    .font_family(
                                        theme_settings::ThemeSettings::get_global(cx)
                                            .buffer_font
                                            .family
                                            .clone(),
                                    )
                                    .text_sm()
                                    .child(SharedString::from(command.clone())),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new(
                                            "thock-copy-pairing-command",
                                            if self.copied { "Copied" } else { "Copy" },
                                        )
                                        .style(ButtonStyle::Filled)
                                        .key_binding(KeyBinding::for_action_in(
                                            &menu::Confirm,
                                            &self.focus_handle,
                                            cx,
                                        ))
                                        .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                                    )
                                    .when(addresses.len() > 1, |this| {
                                        this.child(muted(&format!(
                                            "address {} of {} — up/down for another",
                                            self.address_index + 1,
                                            addresses.len()
                                        )))
                                    }),
                            ),
                        (None, Some(error)) => this.child(
                            Label::new(error.clone())
                                .size(LabelSize::Small)
                                .color(Color::Error),
                        ),
                        (None, None) => this.child(muted("Getting a code…")),
                    }),
            )
            .when(self.invite.is_some(), |this| {
                this.child(div().px_3().py_2().child(muted(
                    "The code works for 10 minutes and once. Then `thock-bridge connect claude` \
                     (or codex, gemini) there installs the skill. Press escape to close; Connect \
                     Agent in the command palette brings this back.",
                )))
            })
    }
}

/// What a change in the service means for the open modal (R8).
#[derive(Debug, PartialEq, Eq)]
enum ModalChange {
    Stay,
    /// The list changed without a pairing (a disconnect): close quietly.
    Close,
    /// The code was spent: close and say which machine paired.
    Paired(String),
}

fn modal_change(code_spent: bool, clients_before: usize, clients: &[ClientView]) -> ModalChange {
    if code_spent {
        let newest = clients
            .iter()
            .max_by(|a, b| a.paired_at.cmp(&b.paired_at))
            .map(|client| client.name.clone())
            .unwrap_or_else(|| "A machine".to_string());
        ModalChange::Paired(newest)
    } else if clients.len() != clients_before {
        ModalChange::Close
    } else {
        ModalChange::Stay
    }
}

/// `thock::DisconnectAgent`: which paired machine to forget.
pub struct DisconnectAgentPicker {
    picker: gpui::Entity<Picker<DisconnectAgentDelegate>>,
}

impl DisconnectAgentPicker {
    pub fn new(
        service: WeakEntity<BridgeService>,
        clients: Vec<ClientView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let delegate = DisconnectAgentDelegate {
            picker_entity: cx.entity().downgrade(),
            service,
            clients,
            selected_index: 0,
        };
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl ModalView for DisconnectAgentPicker {}
impl EventEmitter<DismissEvent> for DisconnectAgentPicker {}

impl Focusable for DisconnectAgentPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for DisconnectAgentPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ThockDisconnectAgentPicker")
            .w(rems(30.))
            .child(self.picker.clone())
    }
}

pub struct DisconnectAgentDelegate {
    picker_entity: WeakEntity<DisconnectAgentPicker>,
    service: WeakEntity<BridgeService>,
    clients: Vec<ClientView>,
    selected_index: usize,
}

impl PickerDelegate for DisconnectAgentDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "disconnect agent"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Disconnect which computer?".into()
    }

    fn match_count(&self) -> usize {
        self.clients.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        index: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = index;
    }

    fn update_matches(
        &mut self,
        _query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        Task::ready(())
    }

    fn confirm(&mut self, _secondary: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let Some(client) = self.clients.get(self.selected_index) {
            let name = client.name.clone();
            self.service
                .update(cx, |service, cx| {
                    service.disconnect(&name, cx);
                    cx.emit(ManualSyncFinished {
                        message: format!("{name} is disconnected; pair it again any time").into(),
                        icon: IconName::Check,
                    });
                })
                .log_err();
        }
        self.picker_entity
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.picker_entity
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let client = self.clients.get(ix)?;
        Some(
            ListItem::new(ix)
                .inset(true)
                .toggle_state(selected)
                .child(Label::new(client.label(chrono::Utc::now()))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::{FakeFs, Fs as _};
    use gpui::{KeyBinding, TestAppContext, VisualTestContext};
    use project::Project;
    use settings::{KeymapFile, KeymapFileLoadResult, SettingsStore};
    use std::path::Path;
    use workspace::{MultiWorkspace, Workspace};

    /// Opens the modal in a workspace with the shipped keymap bound, over a
    /// service that is gone, so no code is minted and the modal shows its
    /// waiting state.
    async fn open_modal(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            let key_bindings: Vec<KeyBinding> = match KeymapFile::load(
                include_str!("../../../assets/keymaps/default-linux.json"),
                cx,
            ) {
                KeymapFileLoadResult::Success { key_bindings }
                | KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
                KeymapFileLoadResult::JsonParseFailure { error } => {
                    panic!("bad keymap: {error}")
                }
            };
            cx.bind_keys(key_bindings);
        });
        let fs = FakeFs::new(cx.executor());
        fs.create_dir(Path::new("/vault")).await.unwrap();
        let project = Project::test(fs, [Path::new("/vault")], cx).await;
        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = window
            .read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone())
            .unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                let mut modal = ConnectAgentModal::new(WeakEntity::new_invalid(), window, cx);
                modal.invite = Some(PairingInvite {
                    code: "maple-river-stone-ocean-lamp-fox".into(),
                    addresses: vec!["palloma.local".into(), "192.168.1.20".into()],
                    port: 7557,
                    expires_at: String::new(),
                });
                modal
            });
        });
        cx.run_until_parked();
        assert!(modal_open(&workspace, cx));
        (workspace, cx)
    }

    fn modal_open(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, cx| {
            workspace.active_modal::<ConnectAgentModal>(cx).is_some()
        })
    }

    fn modal(
        workspace: &gpui::Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> gpui::Entity<ConnectAgentModal> {
        workspace
            .read_with(cx, |workspace, cx| {
                workspace.active_modal::<ConnectAgentModal>(cx)
            })
            .expect("the modal is open")
    }

    #[test]
    fn the_modal_closes_for_the_right_reason() {
        let client = |name: &str, paired_at: &str| ClientView {
            name: name.into(),
            last_seen: String::new(),
            paired_at: paired_at.into(),
        };
        let robin = client("robin", "2026-11-04T08:00:00Z");
        let hp = client("hp", "2026-11-04T09:00:00Z");
        assert_eq!(
            modal_change(false, 1, std::slice::from_ref(&robin)),
            ModalChange::Stay
        );
        assert_eq!(modal_change(false, 1, &[]), ModalChange::Close);
        assert_eq!(
            modal_change(true, 1, &[robin, hp.clone()]),
            ModalChange::Paired("hp".into())
        );
        // A known name re-pairing: the count stays, the code is spent, and
        // the newest `paired_at` names it.
        let robin_again = client("robin", "2026-11-04T10:00:00Z");
        assert_eq!(
            modal_change(true, 2, &[hp, robin_again]),
            ModalChange::Paired("robin".into())
        );
    }

    #[gpui::test]
    async fn enter_copies_the_command_and_escape_closes(cx: &mut TestAppContext) {
        let (workspace, cx) = open_modal(cx).await;
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(modal_open(&workspace, cx), "copying keeps the modal open");
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some("thock-bridge pair palloma.local:7557 maple-river-stone-ocean-lamp-fox")
        );
        assert!(modal(&workspace, cx).read_with(cx, |modal, _| modal.copied));

        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        let entity = modal(&workspace, cx);
        assert_eq!(
            entity.read_with(cx, |modal, _| modal.command()),
            Some("thock-bridge pair 192.168.1.20:7557 maple-river-stone-ocean-lamp-fox".into())
        );
        assert!(!entity.read_with(cx, |modal, _| modal.copied));
        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert_eq!(entity.read_with(cx, |modal, _| modal.address_index), 0);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!modal_open(&workspace, cx));
        let focus_handle = entity.read_with(cx, |modal, _| modal.focus_handle.clone());
        assert!(
            !cx.update(|window, _| focus_handle.is_focused(window)),
            "escape gives focus back"
        );
    }
}
