//! The *Connect your phone* modal (`v34-vault-sync.md` §7.1, §10.3): one QR
//! holding the pairing code, the vault key and the backend URL, shown until
//! the phone appears on the server or the user closes it.

use gpui::{
    App, Bounds, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, Pixels, Point,
    SharedString, Size, Subscription, Task, WeakEntity, canvas, fill, point, px,
};
use std::time::Duration;
use ui::prelude::*;
use ui::{Headline, HeadlineSize, Icon, IconName, IconSize, Label};
use workspace::ModalView;

use crate::calendar_service::ManualSyncFinished;
use crate::vault_sync::{PairingInvite, VaultSyncService};

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const QR_SIDE: f32 = 240.;

pub struct ConnectPhoneModal {
    service: WeakEntity<VaultSyncService>,
    focus_handle: FocusHandle,
    invite: Option<PairingInvite>,
    qr: Option<QrModules>,
    error: Option<SharedString>,
    /// The phone paired before this modal opened, if any; a different id
    /// means the scan worked.
    phone_before: Option<String>,
    phone_name_before: Option<String>,
    _pairing: Option<Task<()>>,
    _poll: Option<Task<()>>,
    _subscription: Subscription,
}

struct QrModules {
    width: usize,
    dark: Vec<bool>,
}

impl EventEmitter<DismissEvent> for ConnectPhoneModal {}
impl ModalView for ConnectPhoneModal {}

impl Focusable for ConnectPhoneModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ConnectPhoneModal {
    pub fn new(
        service: WeakEntity<VaultSyncService>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (phone_before, phone_name_before, pairing, subscription) = match service.upgrade() {
            Some(service) => {
                let phone = service.read(cx).phone().cloned();
                let pairing = service.update(cx, |service, cx| service.begin_pairing(cx));
                let subscription = cx.observe(&service, Self::service_changed);
                (
                    phone.as_ref().map(|p| p.device_id.clone()),
                    phone.map(|p| p.name),
                    Some(pairing),
                    subscription,
                )
            }
            None => (None, None, None, Subscription::new(|| {})),
        };
        let pairing_task = pairing.map(|pairing| {
            cx.spawn(async move |this, cx| {
                let result = pairing.await;
                this.update(cx, |this, cx| {
                    match result {
                        Ok(invite) => {
                            this.qr = encode_qr(&invite.url);
                            if this.qr.is_none() {
                                this.error = Some("Couldn't draw the code. Try again.".into());
                            }
                            this.invite = Some(invite);
                            this.start_polling(cx);
                        }
                        Err(error) => this.error = Some(format!("{error:#}").into()),
                    }
                    cx.notify();
                })
                .ok();
            })
        });
        Self {
            service,
            focus_handle: cx.focus_handle(),
            invite: None,
            qr: None,
            error: None,
            phone_before,
            phone_name_before,
            _pairing: pairing_task,
            _poll: None,
            _subscription: subscription,
        }
    }

    /// Asks the service for the vault summary every few seconds so the
    /// phone's arrival is noticed without a feed event for it.
    fn start_polling(&mut self, cx: &mut Context<Self>) {
        let service = self.service.clone();
        self._poll = Some(cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if service
                    .update(cx, |service, cx| service.refresh_info(cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn service_changed(&mut self, service: gpui::Entity<VaultSyncService>, cx: &mut Context<Self>) {
        let Some(phone) = service.read(cx).phone().cloned() else {
            return;
        };
        if self.invite.is_none() || Some(&phone.device_id) == self.phone_before.as_ref() {
            return;
        }
        let name = if phone.name.is_empty() {
            "Your phone".to_string()
        } else {
            phone.name
        };
        service.update(cx, |_, cx| {
            cx.emit(ManualSyncFinished {
                message: format!("{name} is connected — your notes are on their way").into(),
                icon: IconName::Check,
            })
        });
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn render_qr(&self, qr: &QrModules) -> impl IntoElement {
        let width = qr.width;
        let dark = qr.dark.clone();
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window, _| {
                // Quiet zone and black-on-white, whatever the theme: phone
                // cameras read that reliably.
                window.paint_quad(fill(bounds, gpui::white()));
                let quiet = px(12.);
                let inner = Bounds {
                    origin: bounds.origin + point(quiet, quiet),
                    size: Size {
                        width: bounds.size.width - quiet * 2.,
                        height: bounds.size.height - quiet * 2.,
                    },
                };
                if width == 0 {
                    return;
                }
                let module = inner.size.width / width as f32;
                for (index, is_dark) in dark.iter().enumerate() {
                    if !is_dark {
                        continue;
                    }
                    let x = (index % width) as f32;
                    let y = (index / width) as f32;
                    let origin: Point<Pixels> = inner.origin + point(module * x, module * y);
                    window.paint_quad(fill(
                        Bounds {
                            origin,
                            size: Size {
                                width: module,
                                height: module,
                            },
                        },
                        gpui::black(),
                    ));
                }
            },
        )
        .w(px(QR_SIDE))
        .h(px(QR_SIDE))
    }
}

/// The URL as QR modules, or `None` when it is too long for any QR version.
fn encode_qr(url: &str) -> Option<QrModules> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    let width = code.width();
    let dark = code
        .to_colors()
        .into_iter()
        .map(|color| color == qrcode::Color::Dark)
        .collect();
    Some(QrModules { width, dark })
}

impl Render for ConnectPhoneModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = |text: &str| {
            Label::new(text.to_string())
                .size(LabelSize::Small)
                .color(Color::Muted)
        };
        v_flex()
            .key_context("ThockConnectPhone")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(28.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .gap_1p5()
                    .child(Icon::new(IconName::CloudDownload).size(IconSize::XSmall))
                    .child(Headline::new("Connect your phone").size(HeadlineSize::XSmall)),
            )
            .child(div().px_3().pb_2().child(muted(
                "Open Thock on your phone and scan this code. Your notes are copied there \
                 locked with a key that exists only on these two devices; Thock itself can't \
                 read them.",
            )))
            .when_some(self.phone_name_before.clone(), |this, name| {
                this.child(div().px_3().pb_2().child(muted(&format!(
                    "{name} is connected now. Scanning replaces it."
                ))))
            })
            .child(
                v_flex()
                    .items_center()
                    .py_3()
                    .bg(cx.theme().colors().editor_background)
                    .border_t_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .map(|this| match (&self.qr, &self.error) {
                        (Some(qr), _) => this.child(self.render_qr(qr)),
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
                    "This code works for 10 minutes. Press escape to close; you can come back \
                     to it from the command palette with Connect Phone.",
                )))
            })
    }
}
