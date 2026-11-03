//! Where the centre's page sits between the docks when centred layout is on.
//!
//! The page is a column of fixed width pinned to the middle of the window,
//! so opening, closing or resizing a dock changes the margins beside it and
//! never the page itself. [`anchor`] decides the whole row on every render.

use gpui::{Pixels, px};

/// The narrowest gap between the column and anything beside it.
pub const ANCHOR_GAP: Pixels = px(24.);
/// The width an ebbed dock keeps at the window's edge.
pub const TIDE_MARK_WIDTH: Pixels = px(6.);

/// What a view asks the centre for while centred layout is on
/// ([`crate::Workspace::set_center_anchor`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterAnchor {
    /// The page's width.
    pub column: Pixels,
    /// How far the page may narrow before something floats over it.
    pub min_column: Pixels,
    /// Room to keep in the right margin beside the page, if any.
    pub thread: Option<Pixels>,
    /// Whether the left dock may ebb when it and the page don't fit.
    pub ebb_left: bool,
    /// Whether the right dock may ebb when it and the page don't fit.
    pub ebb_right: bool,
}

/// One side's dock as [`anchor`] sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnchorSide {
    /// The dock's width while it is open; `None` when it is closed.
    pub open_width: Option<Pixels>,
    /// The dock is already ebbed, so it takes only its tide mark.
    pub ebbed: bool,
    /// The dock may ebb to fit the page.
    pub may_ebb: bool,
}

impl AnchorSide {
    fn width(&self, ebbs_to_fit: bool) -> Pixels {
        match self.open_width {
            Some(_) if self.ebbed || ebbs_to_fit => TIDE_MARK_WIDTH,
            Some(width) => width,
            None => Pixels::ZERO,
        }
    }

    fn can_ebb(&self) -> bool {
        self.open_width.is_some() && !self.ebbed && self.may_ebb
    }
}

/// Everything [`anchor`] needs for one row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnchorInput {
    pub window: Pixels,
    pub left: AnchorSide,
    pub right: AnchorSide,
    pub column: Pixels,
    pub min_column: Pixels,
    /// Room the right margin should hold beside the page.
    pub thread: Option<Pixels>,
}

/// The row [`anchor`] settled on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnchorLayout {
    /// From the left dock (or its mark) to the page.
    pub left_padding: Pixels,
    pub column: Pixels,
    /// From the page to the right dock (or its mark); holds the thread's
    /// room when it doesn't float.
    pub right_padding: Pixels,
    pub left_ebbs_to_fit: bool,
    pub right_ebbs_to_fit: bool,
    /// The thread was asked for but couldn't fit, so it floats over the page.
    pub thread_floats: bool,
}

/// Places the page: centred on the window when the margins allow it; else
/// the docks that may ebb do; else the page shifts toward the side with
/// room; else it narrows toward `min_column`; else the thread floats and the
/// page takes what is left.
pub fn anchor(input: AnchorInput) -> AnchorLayout {
    if let Some(layout) = fit(&input, input.thread.unwrap_or_default()) {
        return layout;
    }
    let mut layout = fit(&input, Pixels::ZERO).unwrap_or_else(|| squeeze(&input));
    layout.thread_floats = input.thread.is_some();
    layout
}

/// Steps 1 to 4 with `thread` kept in the right margin, or `None` when the
/// page can't keep `min_column` beside it.
fn fit(input: &AnchorInput, thread: Pixels) -> Option<AnchorLayout> {
    let window = input.window;
    let column = input.column.min(window);
    let margin = (window - column) / 2.;
    let left_need = |ebbs: bool| input.left.width(ebbs) + ANCHOR_GAP;
    let right_need = |ebbs: bool| input.right.width(ebbs) + thread + ANCHOR_GAP;

    let left_ebbs = left_need(false) > margin && input.left.can_ebb();
    let right_ebbs = right_need(false) > margin && input.right.can_ebb();
    let left = left_need(left_ebbs);
    let right = right_need(right_ebbs);
    let place = |x: Pixels, column: Pixels| AnchorLayout {
        left_padding: x - input.left.width(left_ebbs),
        column,
        right_padding: window - input.right.width(right_ebbs) - x - column,
        left_ebbs_to_fit: left_ebbs,
        right_ebbs_to_fit: right_ebbs,
        thread_floats: false,
    };

    if left <= margin && right <= margin {
        return Some(place(margin, column));
    }
    let room = window - left - right;
    if column <= room {
        let x = margin.max(left).min(window - right - column);
        return Some(place(x, column));
    }
    (room >= input.min_column.min(column)).then(|| place(left, room))
}

/// Step 5's last resort: no thread, and the page takes whatever is between
/// the docks' gaps, even below `min_column`.
fn squeeze(input: &AnchorInput) -> AnchorLayout {
    let left_ebbs = input.left.can_ebb();
    let right_ebbs = input.right.can_ebb();
    let left = input.left.width(left_ebbs);
    let right = input.right.width(right_ebbs);
    let gap = ANCHOR_GAP.min(((input.window - left - right) / 2.).max(Pixels::ZERO));
    let column = (input.window - left - right - gap * 2.).max(Pixels::ZERO);
    AnchorLayout {
        left_padding: gap,
        column,
        right_padding: gap,
        left_ebbs_to_fit: left_ebbs,
        right_ebbs_to_fit: right_ebbs,
        thread_floats: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMN: Pixels = px(740.);
    const MIN_COLUMN: Pixels = px(530.);
    const THREAD: Pixels = px(408.);

    fn open(width: f32) -> AnchorSide {
        AnchorSide {
            open_width: Some(px(width)),
            ebbed: false,
            may_ebb: true,
        }
    }

    fn stay(width: f32) -> AnchorSide {
        AnchorSide {
            may_ebb: false,
            ..open(width)
        }
    }

    fn closed() -> AnchorSide {
        AnchorSide::default()
    }

    fn row(window: f32, left: AnchorSide, right: AnchorSide, thread: bool) -> AnchorLayout {
        anchor(AnchorInput {
            window: px(window),
            left,
            right,
            column: COLUMN,
            min_column: MIN_COLUMN,
            thread: thread.then_some(THREAD),
        })
    }

    /// Where the page starts, from the window's left edge.
    fn page_x(window: f32, left: AnchorSide, right: AnchorSide, thread: bool) -> Pixels {
        let layout = row(window, left, right, thread);
        let left_width = if layout.left_ebbs_to_fit {
            TIDE_MARK_WIDTH
        } else {
            left.width(false)
        };
        left_width + layout.left_padding
    }

    fn centred(window: f32) -> Pixels {
        (px(window) - COLUMN) / 2.
    }

    #[test]
    fn the_page_is_centred_on_the_window_whatever_the_docks_are() {
        let layout = row(1280., closed(), closed(), false);
        assert_eq!(layout.column, COLUMN);
        assert_eq!(page_x(1280., closed(), closed(), false), centred(1280.));
        for window in [1512., 1920.] {
            for (left, right) in [
                (closed(), closed()),
                (open(255.), closed()),
                (closed(), open(320.)),
                (open(255.), open(320.)),
                (stay(255.), stay(320.)),
            ] {
                let layout = row(window, left, right, false);
                assert_eq!(layout.column, COLUMN, "{window}: {left:?} {right:?}");
                assert_eq!(page_x(window, left, right, false), centred(window));
                assert!(!layout.left_ebbs_to_fit && !layout.right_ebbs_to_fit);
            }
        }
    }

    #[test]
    fn the_macbook_holds_the_rail_and_the_desk_beside_a_centred_page() {
        // §4.4: margins of about 386 hold the rail at 255 and the Desk at 320.
        let layout = row(1512., open(255.), open(320.), false);
        assert_eq!(page_x(1512., open(255.), open(320.), false), centred(1512.));
        assert_eq!(layout.column, COLUMN);
        assert_eq!(layout.left_padding, centred(1512.) - px(255.));
        assert_eq!(layout.right_padding, centred(1512.) - px(320.));
    }

    #[test]
    fn a_wide_desk_ebbs_to_fit_and_a_staying_one_shifts_the_page() {
        let layout = row(1512., open(255.), open(440.), false);
        assert!(layout.right_ebbs_to_fit);
        assert!(!layout.left_ebbs_to_fit);
        assert_eq!(page_x(1512., open(255.), open(440.), false), centred(1512.));

        // A Desk that stays pushes the page left, as little as needed.
        let layout = row(1512., open(255.), stay(440.), false);
        assert!(!layout.right_ebbs_to_fit);
        assert_eq!(layout.column, COLUMN);
        assert_eq!(
            page_x(1512., open(255.), stay(440.), false),
            px(1512.) - px(440.) - ANCHOR_GAP - COLUMN
        );
    }

    #[test]
    fn only_a_thread_with_room_ebbs_the_desk() {
        assert!(!row(1512., open(255.), open(320.), false).right_ebbs_to_fit);
        assert!(row(1512., open(255.), open(320.), true).right_ebbs_to_fit);
    }

    #[test]
    fn a_thread_at_1512_ebbs_the_desk_and_shifts_the_page_left() {
        // §4.4: mark + thread + gap ≈ 438 against a 386 margin.
        let layout = row(1512., open(255.), open(320.), true);
        assert!(layout.right_ebbs_to_fit);
        assert!(!layout.thread_floats);
        assert_eq!(layout.column, COLUMN);
        let shift = centred(1512.) - page_x(1512., open(255.), open(320.), true);
        assert_eq!(
            shift,
            TIDE_MARK_WIDTH + THREAD + ANCHOR_GAP - centred(1512.)
        );
        assert!(layout.right_padding >= THREAD + ANCHOR_GAP);
    }

    #[test]
    fn a_thread_at_1920_ebbs_the_desk_and_keeps_the_page_centred() {
        let layout = row(1920., open(255.), open(320.), true);
        assert!(layout.right_ebbs_to_fit);
        assert_eq!(page_x(1920., open(255.), open(320.), true), centred(1920.));
    }

    #[test]
    fn a_staying_desk_shifts_the_page_for_the_thread_without_narrowing() {
        // §4.4: 320 + 408 + 24 = 752 against a 590 margin: shift by 162.
        let layout = row(1920., open(255.), stay(320.), true);
        assert!(!layout.right_ebbs_to_fit);
        assert_eq!(layout.column, COLUMN);
        assert_eq!(
            centred(1920.) - page_x(1920., open(255.), stay(320.), true),
            px(162.)
        );
    }

    #[test]
    fn the_page_narrows_to_min_column_before_the_thread_floats() {
        let layout = row(1700., stay(255.), stay(320.), true);
        assert!(!layout.thread_floats);
        assert!(layout.column < COLUMN && layout.column >= MIN_COLUMN);
        assert_eq!(
            layout.column,
            px(1700.) - px(255.) - px(320.) - THREAD - ANCHOR_GAP * 2.
        );
        // Below min_column the thread floats, and the page takes what is
        // left without it.
        let layout = row(1280., stay(255.), stay(320.), true);
        assert!(layout.thread_floats);
        assert_eq!(
            layout.column,
            px(1280.) - px(255.) - px(320.) - ANCHOR_GAP * 2.
        );
    }

    #[test]
    fn at_1280_both_docks_ebb_to_keep_the_page_centred() {
        let layout = row(1280., open(255.), open(320.), false);
        assert!(layout.left_ebbs_to_fit && layout.right_ebbs_to_fit);
        assert_eq!(layout.column, COLUMN);
        assert_eq!(page_x(1280., open(255.), open(320.), false), centred(1280.));
    }

    #[test]
    fn an_ebbed_dock_takes_only_its_mark() {
        let ebbed = AnchorSide {
            ebbed: true,
            ..open(440.)
        };
        let layout = row(1512., open(255.), ebbed, false);
        assert!(!layout.right_ebbs_to_fit, "it is ebbed already");
        assert_eq!(
            layout.right_padding,
            px(1512.) - centred(1512.) - COLUMN - TIDE_MARK_WIDTH
        );
    }

    #[test]
    fn a_tiny_window_squeezes_the_page_without_going_negative() {
        let layout = row(500., stay(255.), stay(320.), true);
        assert!(layout.thread_floats);
        assert_eq!(layout.column, Pixels::ZERO);
        assert!(layout.left_padding >= Pixels::ZERO && layout.right_padding >= Pixels::ZERO);
    }
}
