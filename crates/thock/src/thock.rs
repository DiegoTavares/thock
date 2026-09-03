pub mod agent;
pub mod agent_panel;
pub mod backlog;
pub mod backlog_panel;
pub mod calendar;
pub mod calendar_google;
pub mod calendar_service;
pub mod day_plan;
pub mod day_planner_panel;
pub mod gmail;
pub mod gmail_google;
pub mod gmail_service;
pub mod google_auth;
pub mod history;
pub mod inbox;
pub mod inbox_service;
pub mod markdown_conceal;
pub mod markdown_syntax;
pub mod markdown_text;
pub mod notes;
pub mod routines;
pub mod routines_panel;
pub mod tasks_google;
pub mod vault;

use anyhow::{Context as _, Result};
use command_palette_hooks::CommandPaletteFilter;
use editor::Editor;
use gpui::{App, AppContext as _, AsyncWindowContext, Task, WeakEntity};
use markdown_preview::markdown_preview_view::MarkdownPreviewView;
use std::path::PathBuf;
use std::sync::Arc;
use workspace::{AppState, OpenOptions, OpenVisible, Workspace};

pub use agent_panel::AgentPanel;
pub use backlog_panel::BacklogPanel;
pub use day_planner_panel::DayPlannerPanel;
pub use routines_panel::{RoutinesPanel, show_panel_if_vault};
pub use vault::{Vault, VaultStatus, default_vault_path, scaffold_vault};

pub fn init(cx: &mut App) {
    routines_panel::init(cx);
    day_planner_panel::init(cx);
    agent_panel::init(cx);
    backlog_panel::init(cx);
    calendar_service::init(cx);
    gmail_service::init(cx);
    inbox_service::init(cx);
    markdown_conceal::init(cx);
    hide_inherited_zed_actions(cx);
}

/// Hides command-palette actions for the Zed subsystems Thock turns off
/// (V12 de-Zed-ification). The subsystems stay registered so upstream rebases
/// stay cheap, but a note-taker never sees debugger, task, collab, or
/// account commands.
fn hide_inherited_zed_actions(cx: &mut App) {
    use std::any::TypeId;
    if CommandPaletteFilter::try_global(cx).is_none() {
        // `update_global` silently no-ops without the global, which would
        // resurface every hidden Zed command.
        log::warn!(
            "Thock: command palette filter not initialized; inherited Zed actions will stay visible"
        );
    }
    CommandPaletteFilter::update_global(cx, |filter, _| {
        for namespace in [
            "call",
            "channel",
            "client",
            "collab",
            "collab_panel",
            "debugger",
            "dev",
            "feedback",
            "onboarding",
            "repl",
            "task",
        ] {
            filter.hide_namespace(namespace);
        }
        filter.hide_action_types(&[
            TypeId::of::<zed_actions::OpenOnboarding>(),
            TypeId::of::<zed_actions::OpenAccountSettings>(),
            TypeId::of::<workspace::welcome::ShowWelcome>(),
        ]);
    });
}

/// Opens `path` and lands the user on a rendered markdown preview of it
/// ("viewing mode") instead of the raw buffer. There is no one-shot
/// open-as-preview API, so this opens the file as an editor first and then
/// attaches an independent preview item to the same pane.
pub async fn open_abs_path_as_preview(
    workspace: WeakEntity<Workspace>,
    path: PathBuf,
    cx: &mut AsyncWindowContext,
) -> Result<()> {
    let item = workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_abs_path(
                path.clone(),
                OpenOptions {
                    visible: Some(OpenVisible::All),
                    ..Default::default()
                },
                window,
                cx,
            )
        })?
        .await?;
    let editor = item
        .downcast::<Editor>()
        .with_context(|| format!("{} did not open as a markdown editor", path.display()))?;
    workspace.update_in(cx, |workspace, window, cx| {
        let pane = workspace.active_pane().clone();
        MarkdownPreviewView::open_preview_in_pane(workspace, editor, pane, window, cx);
    })?;
    Ok(())
}

/// Opens the default vault as the workspace, scaffolding the sample vault
/// first if it doesn't exist yet. On a fresh scaffold, `welcome.md` is opened
/// alongside so the user lands on something oriented. Used at startup when
/// there is no previous session to restore.
pub fn open_startup_vault(app_state: Arc<AppState>, cx: &mut App) -> Task<Result<()>> {
    let vault_root = vault::default_vault_path();
    let scaffold = cx.background_spawn({
        let vault_root = vault_root.clone();
        async move {
            let already_vault = vault_root
                .join(vault::VAULT_MARKER_DIR)
                .join(vault::VAULT_CONFIG_FILE)
                .is_file();
            if !already_vault {
                vault::scaffold_vault(&vault_root)?;
            }
            anyhow::Ok(!already_vault)
        }
    });

    cx.spawn(async move |cx| {
        let open_result = async {
            let freshly_scaffolded = scaffold.await?;
            let mut paths = vec![vault_root.clone()];
            if freshly_scaffolded {
                paths.push(vault_root.join(vault::WELCOME_FILE));
            }
            cx.update(|cx| {
                workspace::open_paths(
                    &paths,
                    app_state.clone(),
                    workspace::OpenOptions::default(),
                    cx,
                )
            })
            .await?;
            anyhow::Ok(())
        }
        .await;

        if let Err(error) = open_result {
            log::error!(
                "Thock: couldn't open the default vault, falling back to an empty workspace: {error:?}"
            );
            cx.update(|cx| workspace::open_new(Default::default(), app_state, cx, |_, _, _| {}))
                .await?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn hides_inherited_zed_actions_from_the_command_palette(cx: &mut TestAppContext) {
        cx.update(|cx| {
            command_palette_hooks::init(cx);
            hide_inherited_zed_actions(cx);

            let filter = CommandPaletteFilter::try_global(cx)
                .expect("command palette filter global should be set by init");

            assert!(
                filter.is_hidden(&zed_actions::feedback::FileBugReport),
                "actions in hidden namespaces should be hidden"
            );
            assert!(filter.is_hidden(&zed_actions::OpenOnboarding));
            assert!(filter.is_hidden(&zed_actions::OpenAccountSettings));
            assert!(filter.is_hidden(&workspace::welcome::ShowWelcome));

            assert!(
                !filter.is_hidden(&routines_panel::OpenToday),
                "Thock actions should stay visible"
            );
        });
    }
}
