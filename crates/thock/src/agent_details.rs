//! A connected machine's details (`v55-agents-bridge.md` §11, decision 36):
//! when it paired, when it last called and through which agent, with
//! Disconnect behind a confirmation. The Routines panel's machine rows and
//! the Disconnect agent picker open it; nothing disconnects on a bare row.

use gpui::{App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, WeakEntity};
use ui::prelude::*;
use ui::{Headline, HeadlineSize, Icon, IconName, IconSize, KeyBinding, Label};
use util::ResultExt as _;
use workspace::ModalView;

use crate::bridge_service::{BridgeService, ClientView};
use crate::calendar_service::ManualSyncFinished;

/// The two buttons of either page, in `down` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Details: Disconnect. Confirmation: Disconnect, for good.
    Disconnect,
    /// Details: Close. Confirmation: Keep.
    Keep,
}

impl Choice {
    fn other(self) -> Self {
        match self {
            Self::Disconnect => Self::Keep,
            Self::Keep => Self::Disconnect,
        }
    }
}

pub struct AgentDetailsModal {
    service: WeakEntity<BridgeService>,
    client: ClientView,
    focus_handle: FocusHandle,
    pub(crate) selected: Choice,
    /// Showing "Disconnect robin?" rather than the details.
    pub(crate) confirming: bool,
}

impl EventEmitter<DismissEvent> for AgentDetailsModal {}
impl ModalView for AgentDetailsModal {}

impl Focusable for AgentDetailsModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AgentDetailsModal {
    pub fn new(
        service: WeakEntity<BridgeService>,
        client: ClientView,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            service,
            client,
            focus_handle: cx.focus_handle(),
            selected: Choice::Disconnect,
            confirming: false,
        }
    }

    /// `escape` on the confirmation goes back to the details; on the
    /// details it closes.
    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        if self.confirming {
            self.confirming = false;
            self.selected = Choice::Disconnect;
            cx.notify();
        } else {
            cx.emit(DismissEvent);
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        self.activate(self.selected, cx);
    }

    fn activate(&mut self, choice: Choice, cx: &mut Context<Self>) {
        match (self.confirming, choice) {
            (false, Choice::Keep) => cx.emit(DismissEvent),
            (false, Choice::Disconnect) => {
                self.confirming = true;
                self.selected = Choice::Disconnect;
                cx.notify();
            }
            (true, Choice::Keep) => {
                self.confirming = false;
                self.selected = Choice::Disconnect;
                cx.notify();
            }
            (true, Choice::Disconnect) => self.disconnect(cx),
        }
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        let name = self.client.name.clone();
        self.service
            .update(cx, |service, cx| {
                service.disconnect(&name, cx);
                cx.emit(ManualSyncFinished {
                    message: format!("{name} is disconnected; pair it again any time").into(),
                    icon: IconName::Check,
                });
            })
            .log_err();
        cx.emit(DismissEvent);
    }

    fn select_next(&mut self, _: &menu::SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.other();
        cx.notify();
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = self.selected.other();
        cx.notify();
    }

    fn render_button(
        &self,
        id: &'static str,
        label: &'static str,
        choice: Choice,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selected == choice;
        Button::new(id, label)
            .style(if selected {
                ButtonStyle::Filled
            } else {
                ButtonStyle::Subtle
            })
            .toggle_state(selected)
            .when(selected, |button| {
                button.key_binding(KeyBinding::for_action_in(
                    &menu::Confirm,
                    &self.focus_handle,
                    cx,
                ))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.activate(choice, cx)))
    }
}

impl Render for AgentDetailsModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = |text: String| Label::new(text).size(LabelSize::Small).color(Color::Muted);
        let now = chrono::Utc::now();
        let client = &self.client;
        let last_seen = client
            .last_seen_ago(now)
            .unwrap_or_else(|| "never".to_string());
        let harness = if client.last_harness.is_empty() {
            "none yet".to_string()
        } else {
            client.last_harness.clone()
        };
        let paired_via = if client.paired_via.is_empty() {
            "this desk".to_string()
        } else {
            client.paired_via.clone()
        };
        v_flex()
            .key_context("ThockAgentDetailsModal menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .elevation_2(cx)
            .w(rems(30.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .gap_1p5()
                    .child(Icon::new(IconName::Screen).size(IconSize::XSmall))
                    .child(Headline::new(client.name.clone()).size(HeadlineSize::XSmall)),
            )
            .map(|this| {
                if self.confirming {
                    this.child(div().px_3().pb_2().child(muted(format!(
                        "Disconnect {}? It will need a new code to pair again.",
                        client.name
                    ))))
                    .child(
                        h_flex()
                            .px_3()
                            .pb_3()
                            .gap_2()
                            .child(self.render_button(
                                "thock-agent-disconnect-confirmed",
                                "Disconnect",
                                Choice::Disconnect,
                                cx,
                            ))
                            .child(self.render_button(
                                "thock-agent-keep",
                                "Keep",
                                Choice::Keep,
                                cx,
                            )),
                    )
                } else {
                    this.child(
                        v_flex()
                            .px_3()
                            .pb_2()
                            .gap_0p5()
                            .child(muted(format!("Paired on {}", client.paired_on())))
                            .child(muted(format!("Last seen {last_seen}")))
                            .child(muted(format!("Last agent seen: {harness}")))
                            .child(muted(format!("Paired against {paired_via}")))
                            .child(muted(
                                "It reaches this vault through `thock-bridge` on that machine."
                                    .to_string(),
                            )),
                    )
                    .child(
                        h_flex()
                            .px_3()
                            .pb_3()
                            .gap_2()
                            .child(self.render_button(
                                "thock-agent-disconnect",
                                "Disconnect",
                                Choice::Disconnect,
                                cx,
                            ))
                            .child(self.render_button(
                                "thock-agent-close",
                                "Close",
                                Choice::Keep,
                                cx,
                            )),
                    )
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_alternate() {
        assert_eq!(Choice::Disconnect.other(), Choice::Keep);
        assert_eq!(Choice::Keep.other(), Choice::Disconnect);
    }
}
