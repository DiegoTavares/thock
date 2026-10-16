use editor::Editor;
use gpui::{AnyElement, Modifiers};
use markdown_preview::markdown_preview_view::MarkdownPreviewView;
use ui::{Tooltip, prelude::*, text_for_keystroke};

use super::QuickActionBar;

impl QuickActionBar {
    pub fn render_preview_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Resolve against this toolbar's own pane item rather than the
        // workspace's focused item, so each pane's button reflects and
        // targets the content of the pane it belongs to.
        let active_item = self.active_item.as_ref()?;
        let editor = active_item.act_as::<Editor>(cx)?;
        if !MarkdownPreviewView::is_markdown_file(&editor, cx) {
            return None;
        }

        let alt_click = gpui::Keystroke {
            key: "click".into(),
            modifiers: Modifiers::alt(),
            ..Default::default()
        };

        let button = IconButton::new("toggle-markdown-preview", IconName::Eye)
            .icon_size(IconSize::Small)
            .style(ButtonStyle::Subtle)
            .tooltip(move |_window, cx| {
                Tooltip::with_meta(
                    "Preview Markdown",
                    Some(&markdown_preview::OpenPreview as &dyn gpui::Action),
                    format!(
                        "{} to open in a split",
                        text_for_keystroke(&alt_click.modifiers, &alt_click.key, cx)
                    ),
                    cx,
                )
            })
            .on_click({
                let workspace_handle = self.workspace.clone();
                let active_item = active_item.boxed_clone();
                move |_, window, cx| {
                    let Some(workspace) = workspace_handle.upgrade() else {
                        return;
                    };
                    workspace.update(cx, |workspace, cx| {
                        let Some(pane) = workspace.pane_for(active_item.as_ref()) else {
                            return;
                        };
                        let editor = editor.clone();
                        if window.modifiers().alt {
                            MarkdownPreviewView::open_preview_to_the_side_of_pane(
                                workspace, editor, pane, window, cx,
                            );
                        } else {
                            MarkdownPreviewView::open_preview_in_pane(
                                workspace, editor, pane, window, cx,
                            );
                        }
                    });
                }
            });

        Some(button.into_any_element())
    }
}
