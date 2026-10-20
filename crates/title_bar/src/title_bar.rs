//! Thock's title bar: the window controls, the application menu where the
//! platform has no menu bar, the vault name and the update button.

mod application_menu;
mod title_bar_settings;
mod update_version;

use crate::application_menu::{ApplicationMenu, show_menus};
pub use platform_title_bar::{
    self, DraggedWindowTab, MergeAllWindows, MoveTabToNewWindow, PlatformTitleBar,
    ShowNextWindowTab, ShowPreviousWindowTab,
};

#[cfg(not(target_os = "macos"))]
use crate::application_menu::{
    ActivateDirection, ActivateMenuLeft, ActivateMenuRight, OpenApplicationMenu,
};

use gpui::{
    AnyElement, AnyView, App, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, Styled, Subscription, WeakEntity, Window, actions,
};
use project::Project;
use settings::Settings as _;
use title_bar_settings::TitleBarSettings;
use ui::{prelude::*, utils::platform_title_bar_height};
use update_version::UpdateVersion;
use workspace::{AccessibleMode, MultiWorkspace, Workspace};

const MAX_VAULT_NAME_LENGTH: usize = 40;

actions!(
    dev,
    [
        /// Steps the title bar's update button through every updater state, to check it without a release.
        SimulateUpdateAvailable
    ]
);

pub fn init(cx: &mut App) {
    platform_title_bar::PlatformTitleBar::init(cx);

    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        let multi_workspace = workspace.multi_workspace().cloned();
        let item = cx.new(|cx| TitleBar::new("title-bar", workspace, multi_workspace, window, cx));
        workspace.set_titlebar_item(item.into(), window, cx);

        workspace.register_action(|workspace, _: &SimulateUpdateAvailable, _window, cx| {
            if let Some(title_bar) = title_bar_of(workspace) {
                title_bar.update(cx, |title_bar, cx| title_bar.toggle_update_simulation(cx));
            }
        });

        #[cfg(not(target_os = "macos"))]
        workspace.register_action(|workspace, action: &OpenApplicationMenu, window, cx| {
            if let Some(menu) = application_menu_of(workspace, cx) {
                menu.update(cx, |menu, cx| menu.open_menu(action, window, cx));
            }
        });

        #[cfg(not(target_os = "macos"))]
        workspace.register_action(|workspace, _: &ActivateMenuRight, window, cx| {
            if let Some(menu) = application_menu_of(workspace, cx) {
                menu.update(cx, |menu, cx| {
                    menu.navigate_menus_in_direction(ActivateDirection::Right, window, cx)
                });
            }
        });

        #[cfg(not(target_os = "macos"))]
        workspace.register_action(|workspace, _: &ActivateMenuLeft, window, cx| {
            if let Some(menu) = application_menu_of(workspace, cx) {
                menu.update(cx, |menu, cx| {
                    menu.navigate_menus_in_direction(ActivateDirection::Left, window, cx)
                });
            }
        });
    })
    .detach();
}

fn title_bar_of(workspace: &Workspace) -> Option<Entity<TitleBar>> {
    workspace
        .titlebar_item()
        .and_then(|item| item.downcast::<TitleBar>().ok())
}

#[cfg(not(target_os = "macos"))]
fn application_menu_of(workspace: &Workspace, cx: &App) -> Option<Entity<ApplicationMenu>> {
    title_bar_of(workspace)?.read(cx).application_menu.clone()
}

pub struct TitleBar {
    platform_titlebar: Entity<PlatformTitleBar>,
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    multi_workspace: Option<WeakEntity<MultiWorkspace>>,
    application_menu: Option<Entity<ApplicationMenu>>,
    update_version: Entity<UpdateVersion>,
    /// A second row drawn under the title bar across the window's width —
    /// Thock's masthead. The title bar row itself keeps its height and
    /// contents.
    masthead: Option<AnyView>,
    _subscriptions: Vec<Subscription>,
}

impl Render for TitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.multi_workspace.is_none()
            && let Some(multi_workspace) = self
                .workspace
                .upgrade()
                .and_then(|workspace| workspace.read(cx).multi_workspace().cloned())
        {
            self.multi_workspace = Some(multi_workspace.clone());
            self.platform_titlebar.update(cx, |titlebar, _cx| {
                titlebar.set_multi_workspace(multi_workspace);
            });
        }

        let button_layout = TitleBarSettings::get_global(cx).button_layout;
        let show_menus = show_menus(cx);

        // The collapsed application menu expands into a full menu bar on hover,
        // and the vault name gives way to make room for it, except in
        // accessible mode, where the bar is always expanded and the name must
        // stay reachable.
        let mut show_vault_name = true;
        let left = h_flex()
            .h_full()
            .gap_0p5()
            .when_some(
                self.application_menu.clone().filter(|_| !show_menus),
                |this, menu| {
                    show_vault_name &= !menu.update(cx, |menu, cx| menu.all_menus_shown(cx))
                        || cx.accessible_mode();
                    this.child(menu)
                },
            )
            .when(show_vault_name, |this| {
                this.children(self.render_vault_name(cx))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .into_any_element();

        let right = h_flex()
            .pr_1()
            .gap_1()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(self.update_version.clone())
            .into_any_element();

        let children: Vec<AnyElement> = vec![left, right];

        if show_menus {
            self.platform_titlebar.update(cx, |this, _| {
                this.set_button_layout(button_layout);
                this.set_children(
                    self.application_menu
                        .clone()
                        .map(|menu| menu.into_any_element()),
                );
            });

            let height = platform_title_bar_height(window);
            let title_bar_color = self.platform_titlebar.update(cx, |platform_titlebar, cx| {
                platform_titlebar.title_bar_color(window, cx)
            });

            v_flex()
                .w_full()
                .child(self.platform_titlebar.clone().into_any_element())
                .child(
                    h_flex()
                        .bg(title_bar_color)
                        .h(height)
                        .pl_2()
                        .justify_between()
                        .w_full()
                        .children(children),
                )
                .children(self.masthead.clone())
                .into_any_element()
        } else {
            self.platform_titlebar.update(cx, |this, _| {
                this.set_button_layout(button_layout);
                this.set_children(children);
            });
            match self.masthead.clone() {
                Some(masthead) => v_flex()
                    .w_full()
                    .child(self.platform_titlebar.clone().into_any_element())
                    .child(masthead)
                    .into_any_element(),
                None => self.platform_titlebar.clone().into_any_element(),
            }
        }
    }
}

impl TitleBar {
    pub fn new(
        id: impl Into<ElementId>,
        workspace: &Workspace,
        multi_workspace: Option<WeakEntity<MultiWorkspace>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let project = workspace.project().clone();

        let application_menu = match PlatformStyle::platform() {
            PlatformStyle::Mac => option_env!("ZED_USE_CROSS_PLATFORM_MENU")
                .is_some()
                .then(|| cx.new(|cx| ApplicationMenu::new(window, cx))),
            PlatformStyle::Linux | PlatformStyle::Windows => {
                Some(cx.new(|cx| ApplicationMenu::new(window, cx)))
            }
        };

        let mut subscriptions = Vec::new();
        if let Some(workspace) = workspace.weak_handle().upgrade() {
            subscriptions.push(cx.observe(&workspace, |_, _, cx| cx.notify()));
        }
        subscriptions.push(cx.observe_button_layout_changed(window, |_, _, cx| cx.notify()));

        let update_version = cx.new(|cx| UpdateVersion::new(cx));
        let platform_titlebar = cx.new(|cx| {
            let mut titlebar = PlatformTitleBar::new(id, cx);
            if let Some(multi_workspace) = multi_workspace.clone() {
                titlebar = titlebar.with_multi_workspace(multi_workspace);
            }
            titlebar
        });

        Self {
            platform_titlebar,
            project,
            workspace: workspace.weak_handle(),
            multi_workspace,
            application_menu,
            update_version,
            masthead: None,
            _subscriptions: subscriptions,
        }
    }

    /// Sets the row drawn under the title bar, or removes it with `None`.
    pub fn set_masthead(&mut self, masthead: Option<AnyView>, cx: &mut Context<Self>) {
        self.masthead = masthead;
        cx.notify();
    }

    /// The title bar of `workspace`, when it is one of these.
    pub fn of(workspace: &Workspace) -> Option<Entity<TitleBar>> {
        title_bar_of(workspace)
    }

    fn toggle_update_simulation(&mut self, cx: &mut Context<Self>) {
        self.update_version.update(cx, |update_version, cx| {
            update_version.update_simulation(cx)
        });
        cx.notify();
    }

    /// The name of the first folder open in the window, which for a vault is
    /// the vault's own folder.
    fn render_vault_name(&self, cx: &App) -> Option<impl IntoElement> {
        let worktree = self.project.read(cx).visible_worktrees(cx).next()?;
        let name = worktree.read(cx).root_name().file_name()?.to_string();
        Some(
            h_flex().px_1().child(
                Label::new(util::truncate_and_trailoff(&name, MAX_VAULT_NAME_LENGTH))
                    .size(LabelSize::Small),
            ),
        )
    }
}
