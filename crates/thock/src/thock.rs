pub mod agent;
pub mod agent_panel;
pub mod backlog;
pub mod backlog_panel;
pub mod calendar;
pub mod calendar_google;
pub mod calendar_service;
pub mod chat_panel;
pub mod connect_phone;
pub mod day_plan;
pub mod day_planner_panel;
pub mod desk_panel;
pub mod getting_started;
pub mod gmail;
pub mod gmail_google;
pub mod gmail_service;
pub mod google_auth;
pub mod history;
pub mod hosted_agent;
pub mod inbox;
pub mod inbox_service;
pub mod inbox_suggest;
pub mod inbox_view;
pub mod markdown_conceal;
pub mod markdown_syntax;
pub mod markdown_text;
pub mod masthead;
pub mod memory;
pub mod notes;
pub mod plus;
pub mod readwise;
pub mod readwise_service;
pub mod reminders;
pub mod routine_collections;
pub mod routines;
pub mod routines_panel;
pub mod sync_status;
pub mod tasks_google;
pub mod thock_theme;
pub mod ui;
pub mod vault;
pub mod vault_sync;
pub mod wikilink_completion;

use anyhow::{Context as _, Result};
use command_palette_hooks::CommandPaletteFilter;
use editor::Editor;
use gpui::{App, AppContext as _, AsyncWindowContext, Task, TaskExt as _, WeakEntity};
use markdown_preview::markdown_preview_view::MarkdownPreviewView;
use std::path::PathBuf;
use std::sync::Arc;
use workspace::{AppState, OpenOptions, OpenVisible, Workspace};

/// The `body_font` value that keeps the editor's own font.
pub const BODY_FONT_BUFFER: &str = "buffer";
/// The `heading_font` value that draws headings in the body face.
pub const HEADING_FONT_BODY: &str = "body";

/// The `thock` block of `settings.json` (V43 §7.1): the faces a vault note
/// is set in while its Markdown is concealed. A change applies to open notes
/// right away, like `buffer_font_family`.
#[derive(Clone, Debug, PartialEq, settings::RegisterSetting)]
pub struct ThockSettings {
    /// The family notes are set in, or `BODY_FONT_BUFFER` for the editor's own.
    pub body_font: String,
    /// The family `#` and `##` headings are drawn in, or `HEADING_FONT_BODY`.
    pub heading_font: String,
    /// How often the chat may start a conversation on its own (V44 §5.8).
    pub openers: settings::OpenerFrequency,
}

impl settings::Settings for ThockSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let thock = content.thock.clone().unwrap_or_default();
        Self {
            body_font: thock
                .body_font
                .unwrap_or_else(|| "IBM Plex Sans".to_string()),
            heading_font: thock.heading_font.unwrap_or_else(|| "Petrona".to_string()),
            openers: thock.openers.unwrap_or_default(),
        }
    }
}

impl ThockSettings {
    /// The family a concealed note's body is set in, or `None` to leave the
    /// editor's own font through.
    pub fn body_family(&self) -> Option<gpui::SharedString> {
        (self.body_font != BODY_FONT_BUFFER).then(|| self.body_font.clone().into())
    }

    /// The family headings are drawn in: the heading face, or the body
    /// face when asked for `"body"`, which may itself be the editor's own.
    pub fn heading_family(&self) -> Option<gpui::SharedString> {
        if self.heading_font == HEADING_FONT_BODY {
            self.body_family()
        } else {
            Some(self.heading_font.clone().into())
        }
    }
}

pub use agent_panel::AgentPanel;
pub use backlog_panel::BacklogPanel;
pub use chat_panel::ChatPanel;
pub use day_planner_panel::DayPlannerPanel;
pub use desk_panel::DeskPanel;
pub use routines_panel::{RoutinesPanel, show_panel_if_vault};
pub use sync_status::SyncStatusIndicator;
pub use vault::{Vault, VaultStatus, default_vault_path, scaffold_vault};

pub fn init(cx: &mut App) {
    routines_panel::init(cx);
    desk_panel::init(cx);
    agent_panel::init(cx);
    chat_panel::init(cx);
    calendar_service::init(cx);
    gmail_service::init(cx);
    inbox_service::init(cx);
    inbox_view::init(cx);
    readwise_service::init(cx);
    reminders::init(cx);
    sync_status::init(cx);
    vault_sync::init(cx);
    markdown_conceal::init(cx);
    masthead::init(cx);
    wikilink_completion::init(cx);
    hide_inherited_zed_actions(cx);
}

/// Hides command-palette actions that inherited crates still register but a
/// note-taker never needs: account and collaboration plumbing, developer
/// tooling, and Zed-branded surfaces (V12 de-Zed-ification). Namespaces whose
/// crates V42 deleted are gone from the registry and need no hiding.
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
        for namespace in ["client", "collab", "debugger", "dev", "feedback"] {
            filter.hide_namespace(namespace);
        }
        filter.hide_action_types(&[TypeId::of::<workspace::welcome::ShowWelcome>()]);
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

/// What a fresh scaffold leaves for the startup flow to open: `welcome.md`
/// and, when today had no note yet, the filled-in example day.
struct FreshVault {
    example_day: Option<PathBuf>,
}

/// Opens the default vault as the workspace, scaffolding the sample vault
/// first if it doesn't exist yet. On a fresh scaffold, `welcome.md` is opened
/// as a rendered preview so the user lands on a readable page, not raw
/// markup (V18 §5.2), and today's note is written as a filled-in example and
/// opened on top of it, so the first thing on screen is what a day can look
/// like (V22). Used at startup when there is no previous session to restore.
pub fn open_startup_vault(app_state: Arc<AppState>, cx: &mut App) -> Task<Result<()>> {
    let vault_root = vault::default_vault_path();
    let scaffold = cx.background_spawn({
        let vault_root = vault_root.clone();
        let fs = app_state.fs.clone();
        async move {
            let already_vault = vault_root
                .join(vault::VAULT_MARKER_DIR)
                .join(vault::VAULT_CONFIG_FILE)
                .is_file();
            if already_vault {
                return anyhow::Ok(None);
            }
            vault::scaffold_vault(&vault_root)?;
            let example_day = match vault::Vault::detect(&vault_root) {
                vault::VaultStatus::Valid(vault) => {
                    let now = chrono::Local::now();
                    notes::ensure_example_day(&fs, &vault, now.date_naive(), now.time()).await?
                }
                other => {
                    anyhow::bail!("the vault just scaffolded at {vault_root:?} is {other:?}")
                }
            };
            anyhow::Ok(Some(FreshVault { example_day }))
        }
    });

    cx.spawn(async move |cx| {
        let open_result = async {
            let fresh_vault = scaffold.await?;
            let opened = cx
                .update(|cx| {
                    workspace::open_paths(
                        std::slice::from_ref(&vault_root),
                        app_state.clone(),
                        workspace::OpenOptions::default(),
                        cx,
                    )
                })
                .await?;
            if let Some(fresh_vault) = fresh_vault {
                let welcome_path = vault_root.join(vault::WELCOME_FILE);
                let workspace = opened.workspace.downgrade();
                opened.window.update(cx, |_, window, cx| {
                    cx.spawn_in(window, async move |_, cx| {
                        open_abs_path_as_preview(workspace.clone(), welcome_path, cx).await?;
                        // Opened last so it is the active tab; the rendered
                        // welcome stays one tab behind it.
                        if let Some(example_day) = fresh_vault.example_day {
                            workspace
                                .update_in(cx, |workspace, window, cx| {
                                    workspace.open_abs_path(
                                        example_day,
                                        OpenOptions {
                                            visible: Some(OpenVisible::All),
                                            ..Default::default()
                                        },
                                        window,
                                        cx,
                                    )
                                })?
                                .await?;
                        }
                        anyhow::Ok(())
                    })
                    .detach_and_log_err(cx);
                })?;
            }
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
            assert!(filter.is_hidden(&workspace::welcome::ShowWelcome));

            assert!(
                !filter.is_hidden(&routines_panel::OpenToday),
                "Thock actions should stay visible"
            );
        });
    }
}
