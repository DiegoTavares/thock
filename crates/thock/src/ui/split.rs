//! A horizontal two-pane split with a draggable handle (V41 §9).
//!
//! Modelled on gpui-kit's `h_resizable` / `resizable_panel`
//! (`crates/ui/src/resizable/panel.rs` in github.com/longbridge/gpui-component),
//! which is licensed under the Apache License, Version 2.0. This file is a
//! re-implementation of its shape against Zed's own gpui rather than a copy:
//! the left pane takes a fraction of the width, the handle is dragged to
//! change it within two minimum widths, and the new fraction is reported to
//! the owner, who persists it.

use gpui::{
    AnyElement, App, Context, DragMoveEvent, ElementId, Empty, MouseButton, MouseDownEvent, Pixels,
    Render, RenderOnce, Window, div, prelude::*, px, relative,
};
use std::rc::Rc;
use ui::ActiveTheme as _;

/// The hit area either side of the one-pixel handle line.
const HANDLE_GRAB: Pixels = px(8.);

/// What the handle drags; it draws nothing while it moves.
pub struct SplitDrag;

impl Render for SplitDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

type ResizeHandler = Rc<dyn Fn(f32, &mut Window, &mut App)>;

/// Two panes side by side. `fraction` is the left pane's share of the width.
#[derive(IntoElement)]
pub struct Split {
    id: ElementId,
    fraction: f32,
    min_left: Pixels,
    min_right: Pixels,
    left: AnyElement,
    right: AnyElement,
    on_resize: Option<ResizeHandler>,
}

impl Split {
    pub fn new(
        id: impl Into<ElementId>,
        fraction: f32,
        left: impl IntoElement,
        right: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            fraction: fraction.clamp(0.0, 1.0),
            min_left: px(160.),
            min_right: px(240.),
            left: left.into_any_element(),
            right: right.into_any_element(),
            on_resize: None,
        }
    }

    /// The narrowest each pane may be dragged to.
    pub fn min_widths(mut self, left: Pixels, right: Pixels) -> Self {
        self.min_left = left;
        self.min_right = right;
        self
    }

    /// Called with the new left fraction while the handle is dragged.
    pub fn on_resize(mut self, handler: impl Fn(f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_resize = Some(Rc::new(handler));
        self
    }
}

/// The fraction a pointer at `x` inside a container `width` wide asks for,
/// kept inside both minimum widths.
pub fn clamp_fraction(x: Pixels, width: Pixels, min_left: Pixels, min_right: Pixels) -> f32 {
    if width <= px(0.) {
        return 0.5;
    }
    let lowest = (min_left / width).min(1.0);
    let highest = (1.0 - min_right / width).max(lowest);
    (x / width).clamp(lowest, highest)
}

impl RenderOnce for Split {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let border = cx.theme().colors().border;
        let (min_left, min_right) = (self.min_left, self.min_right);
        let handle = div()
            .id("split-handle")
            .relative()
            .flex_none()
            .w(px(1.))
            .h_full()
            .bg(border)
            .child(
                div()
                    .id("split-grab")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(-HANDLE_GRAB / 2.)
                    .w(HANDLE_GRAB)
                    .cursor_col_resize()
                    .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_drag(SplitDrag, |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| SplitDrag)
                    }),
            );
        div()
            .id(self.id)
            .flex()
            .flex_row()
            .size_full()
            .min_w_0()
            .when_some(self.on_resize, |this, on_resize| {
                this.on_drag_move(move |event: &DragMoveEvent<SplitDrag>, window, cx| {
                    let bounds = event.bounds;
                    let fraction = clamp_fraction(
                        event.event.position.x - bounds.left(),
                        bounds.size.width,
                        min_left,
                        min_right,
                    );
                    on_resize(fraction, window, cx);
                })
            })
            .child(
                div()
                    .flex_none()
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .w(relative(self.fraction))
                    .child(self.left),
            )
            .child(handle)
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.right),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handle_stays_inside_both_minimum_widths() {
        let width = px(1000.);
        assert_eq!(clamp_fraction(px(380.), width, px(160.), px(240.)), 0.38);
        assert_eq!(clamp_fraction(px(10.), width, px(160.), px(240.)), 0.16);
        assert_eq!(clamp_fraction(px(990.), width, px(160.), px(240.)), 0.76);
        assert_eq!(clamp_fraction(px(-50.), width, px(160.), px(240.)), 0.16);
        // A pane too narrow for both minimums gives the left its minimum.
        assert_eq!(
            clamp_fraction(px(100.), px(300.), px(160.), px(240.)),
            160. / 300.
        );
        assert_eq!(clamp_fraction(px(100.), px(0.), px(160.), px(240.)), 0.5);
    }
}
