//! Where the centre's page sits between the docks when centred layout is on.
//!
//! The page is a column of fixed width pinned to the middle of the window,
//! so opening, closing or resizing a dock changes the margins beside it and
//! never the page itself. [`anchor`] decides the whole row on every render.

use gpui::{Pixels, px};

use crate::dock::DockPosition;

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
    pub thread: Option<ThreadRoom>,
    /// Whether the left dock may ebb when it and the page don't fit.
    pub ebb_left: bool,
    /// Whether the right dock may ebb when it and the page don't fit.
    pub ebb_right: bool,
}

/// Room asked for on the right of the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreadRoom {
    pub width: Pixels,
    /// Whether the right dock may ebb to make it. Only a thread with
    /// messages may; anything else takes the room when it's free.
    pub ebbs_docks: bool,
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
    /// The dock holds the focus, so it is kept whole even where it would
    /// ebb to fit: focus never sits in a mark.
    pub focused: bool,
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
        self.open_width.is_some() && !self.ebbed && self.may_ebb && !self.focused
    }

    /// Would ebb to fit, but holds the focus.
    fn kept(&self, overflows: bool) -> bool {
        overflows && self.open_width.is_some() && !self.ebbed && self.may_ebb && self.focused
    }

    fn drawn(&self, ebbs_to_fit: bool, kept_for_focus: bool, width: Pixels) -> SideLayout {
        SideLayout {
            width,
            open_width: self.open_width.unwrap_or_default(),
            mark: self.open_width.is_some() && (self.ebbed || ebbs_to_fit),
            ebbs_to_fit,
            kept_for_focus,
        }
    }
}

/// Everything [`anchor`] needs for one row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnchorInput {
    /// The row's width, from the left dock's outer edge to the right's.
    pub row: Pixels,
    /// Where the window's centre falls in the row, from its left edge.
    pub centre: Pixels,
    pub left: AnchorSide,
    pub right: AnchorSide,
    pub column: Pixels,
    pub min_column: Pixels,
    /// Room the right margin should hold beside the page.
    pub thread: Option<ThreadRoom>,
}

/// How one side dock is drawn in an anchored row.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SideLayout {
    /// What the dock takes: nothing when closed, the mark when ebbed.
    pub width: Pixels,
    /// The dock's own width, which its panel keeps behind a mark.
    pub open_width: Pixels,
    /// Drawn as its tide mark: ebbed, or ebbed to fit.
    pub mark: bool,
    /// Ebbed by the row to fit the page, not by typing.
    pub ebbs_to_fit: bool,
    /// Would ebb to fit, but is kept whole because it holds the focus.
    pub kept_for_focus: bool,
}

/// The row [`anchor`] settled on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnchorLayout {
    pub left: SideLayout,
    /// From the left dock (or its mark) to the page.
    pub left_padding: Pixels,
    pub column: Pixels,
    /// From the page to the right dock (or its mark); holds the thread's
    /// room when it doesn't float.
    pub right_padding: Pixels,
    pub right: SideLayout,
    /// The thread was asked for but couldn't fit, so it floats over the page.
    pub thread_floats: bool,
}

/// How the centre's row is laid out in a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CenterRow {
    /// Zed's own layout. The thread keeps its room on the right when it
    /// fits, after the right dock ebbs for it if it may.
    Ordinary {
        thread: Option<Pixels>,
        right_ebbs_for_thread: bool,
        /// The right dock's own width, which its panel keeps behind a mark.
        right_width: Pixels,
    },
    /// The page pinned to the window's centre.
    Anchored(AnchorLayout),
}

impl Default for CenterRow {
    fn default() -> Self {
        Self::Ordinary {
            thread: None,
            right_ebbs_for_thread: false,
            right_width: Pixels::ZERO,
        }
    }
}

impl CenterRow {
    pub fn anchored(&self) -> Option<AnchorLayout> {
        match self {
            Self::Anchored(layout) => Some(*layout),
            Self::Ordinary { .. } => None,
        }
    }

    /// Whether the dock at `position` is drawn as its tide mark in this row.
    pub fn tide_mark(&self, position: DockPosition) -> bool {
        match (self, position) {
            (Self::Anchored(layout), DockPosition::Left) => layout.left.mark,
            (Self::Anchored(layout), DockPosition::Right) => layout.right.mark,
            (
                Self::Ordinary {
                    right_ebbs_for_thread,
                    ..
                },
                DockPosition::Right,
            ) => *right_ebbs_for_thread,
            _ => false,
        }
    }

    /// The dock's own width at `position`, which its panel keeps behind a
    /// mark so nothing in it reflows.
    pub fn open_width(&self, position: DockPosition) -> Pixels {
        match (self, position) {
            (Self::Anchored(layout), DockPosition::Left) => layout.left.open_width,
            (Self::Anchored(layout), DockPosition::Right) => layout.right.open_width,
            (Self::Ordinary { right_width, .. }, DockPosition::Right) => *right_width,
            _ => Pixels::ZERO,
        }
    }

    /// Whether the dock at `position` would be ebbed to fit but holds the
    /// focus, so it is kept whole.
    pub fn kept_for_focus(&self, position: DockPosition) -> bool {
        match (self, position) {
            (Self::Anchored(layout), DockPosition::Left) => layout.left.kept_for_focus,
            (Self::Anchored(layout), DockPosition::Right) => layout.right.kept_for_focus,
            _ => false,
        }
    }

    /// Whether the row ebbs the dock at `position` to make room, rather
    /// than the dock being ebbed by typing.
    pub fn ebbs_to_fit(&self, position: DockPosition) -> bool {
        match (self, position) {
            (Self::Anchored(layout), DockPosition::Left) => layout.left.ebbs_to_fit,
            (Self::Anchored(layout), DockPosition::Right) => layout.right.ebbs_to_fit,
            (Self::Ordinary { .. }, _) => self.tide_mark(position),
            _ => false,
        }
    }
}

/// The row without an anchored page: the thread keeps its room on the
/// right when the centre still holds `min_column` beside it; else the right
/// dock ebbs for it when both may; else the thread floats.
pub fn ordinary_row(
    row: Pixels,
    left_width: Option<Pixels>,
    right: AnchorSide,
    min_column: Pixels,
    thread: Option<ThreadRoom>,
) -> CenterRow {
    let Some(thread) = thread else {
        return CenterRow::default();
    };
    let center = |right_ebbs: bool| row - left_width.unwrap_or_default() - right.width(right_ebbs);
    let right_width = right.open_width.unwrap_or_default();
    if center(false) - thread.width >= min_column {
        CenterRow::Ordinary {
            thread: Some(thread.width),
            right_ebbs_for_thread: false,
            right_width,
        }
    } else if thread.ebbs_docks && right.can_ebb() && center(true) - thread.width >= min_column {
        CenterRow::Ordinary {
            thread: Some(thread.width),
            right_ebbs_for_thread: true,
            right_width,
        }
    } else {
        CenterRow::default()
    }
}

/// Places the page: centred on the window when the margins allow it; else
/// the docks that may ebb do; else the page shifts toward the side with
/// room; else it narrows toward `min_column`; else the thread floats and the
/// page takes what is left.
pub fn anchor(input: AnchorInput) -> AnchorLayout {
    if let Some(layout) = fit(&input, input.thread) {
        return layout;
    }
    let mut layout = fit(&input, None).unwrap_or_else(|| squeeze(&input));
    layout.thread_floats = input.thread.is_some();
    layout
}

/// Steps 1 to 4 with `thread` kept in the right margin, or `None` when the
/// page can't keep `min_column` beside it.
fn fit(input: &AnchorInput, thread: Option<ThreadRoom>) -> Option<AnchorLayout> {
    let row = input.row;
    let column = input.column.min(row);
    let centred = (input.centre - column / 2.).clamp(Pixels::ZERO, row - column);
    let left_room = centred;
    let right_room = row - centred - column;
    let room_width = thread.map_or(Pixels::ZERO, |thread| thread.width);
    // Only a thread with messages ebbs a dock for its room.
    let ebb_room = thread
        .filter(|thread| thread.ebbs_docks)
        .map_or(Pixels::ZERO, |thread| thread.width);
    let left_need = |ebbs: bool| input.left.width(ebbs) + ANCHOR_GAP;
    let right_need = |ebbs: bool, room: Pixels| input.right.width(ebbs) + room + ANCHOR_GAP;

    let left_overflows = left_need(false) > left_room;
    let right_overflows = right_need(false, ebb_room) > right_room;
    let left_ebbs = left_overflows && input.left.can_ebb();
    let right_ebbs = right_overflows && input.right.can_ebb();
    let left_kept = input.left.kept(left_overflows);
    let right_kept = input.right.kept(right_overflows);
    let left = left_need(left_ebbs);
    let right = right_need(right_ebbs, room_width);
    let left_width = input.left.width(left_ebbs);
    let right_width = input.right.width(right_ebbs);
    let place = |x: Pixels, column: Pixels| AnchorLayout {
        left: input.left.drawn(left_ebbs, left_kept, left_width),
        left_padding: x - left_width,
        column,
        right_padding: row - right_width - x - column,
        right: input.right.drawn(right_ebbs, right_kept, right_width),
        thread_floats: false,
    };

    if left <= left_room && right <= right_room {
        return Some(place(centred, column));
    }
    let room = row - left - right;
    if column <= room {
        let x = centred.max(left).min(row - right - column);
        return Some(place(x, column));
    }
    (room >= input.min_column.min(column)).then(|| place(left, room))
}

/// Step 5's last resort: no thread, and the page takes whatever is between
/// the docks' gaps, even below `min_column`. Docks that can't ebb shrink
/// together when even they don't fit, so neither runs off the window.
fn squeeze(input: &AnchorInput) -> AnchorLayout {
    let left_ebbs = input.left.can_ebb();
    let right_ebbs = input.right.can_ebb();
    let mut left = input.left.width(left_ebbs);
    let mut right = input.right.width(right_ebbs);
    let docks = left + right;
    if docks > input.row && docks > Pixels::ZERO {
        let scale = input.row / docks;
        left = left * scale;
        right = right * scale;
    }
    let gap = ANCHOR_GAP.min(((input.row - left - right) / 2.).max(Pixels::ZERO));
    let column = (input.row - left - right - gap * 2.).max(Pixels::ZERO);
    AnchorLayout {
        left: input.left.drawn(left_ebbs, input.left.kept(true), left),
        left_padding: gap,
        column,
        right_padding: gap,
        right: input.right.drawn(right_ebbs, input.right.kept(true), right),
        thread_floats: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMN: Pixels = px(740.);
    const MIN_COLUMN: Pixels = px(530.);
    const THREAD: Pixels = px(408.);
    const MESSAGES: ThreadRoom = ThreadRoom {
        width: THREAD,
        ebbs_docks: true,
    };

    fn open(width: f32) -> AnchorSide {
        AnchorSide {
            open_width: Some(px(width)),
            ebbed: false,
            may_ebb: true,
            focused: false,
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
            row: px(window),
            centre: px(window) / 2.,
            left,
            right,
            column: COLUMN,
            min_column: MIN_COLUMN,
            thread: thread.then_some(MESSAGES),
        })
    }

    fn input(window: f32, left: AnchorSide, right: AnchorSide) -> AnchorInput {
        AnchorInput {
            row: px(window),
            centre: px(window) / 2.,
            left,
            right,
            column: COLUMN,
            min_column: MIN_COLUMN,
            thread: None,
        }
    }

    /// Where the page starts, from the window's left edge.
    fn page_x(window: f32, left: AnchorSide, right: AnchorSide, thread: bool) -> Pixels {
        let layout = row(window, left, right, thread);
        let left_width = if layout.left.ebbs_to_fit {
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
                assert!(!layout.left.ebbs_to_fit && !layout.right.ebbs_to_fit);
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
        assert!(layout.right.ebbs_to_fit);
        assert!(!layout.left.ebbs_to_fit);
        assert_eq!(page_x(1512., open(255.), open(440.), false), centred(1512.));

        // A Desk that stays pushes the page left, as little as needed.
        let layout = row(1512., open(255.), stay(440.), false);
        assert!(!layout.right.ebbs_to_fit);
        assert_eq!(layout.column, COLUMN);
        assert_eq!(
            page_x(1512., open(255.), stay(440.), false),
            px(1512.) - px(440.) - ANCHOR_GAP - COLUMN
        );
    }

    #[test]
    fn only_a_thread_with_room_ebbs_the_desk() {
        assert!(!row(1512., open(255.), open(320.), false).right.ebbs_to_fit);
        assert!(row(1512., open(255.), open(320.), true).right.ebbs_to_fit);
    }

    #[test]
    fn a_thread_at_1512_ebbs_the_desk_and_shifts_the_page_left() {
        // §4.4: mark + thread + gap ≈ 438 against a 386 margin.
        let layout = row(1512., open(255.), open(320.), true);
        assert!(layout.right.ebbs_to_fit);
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
        assert!(layout.right.ebbs_to_fit);
        assert_eq!(page_x(1920., open(255.), open(320.), true), centred(1920.));
    }

    #[test]
    fn a_staying_desk_shifts_the_page_for_the_thread_without_narrowing() {
        // §4.4: 320 + 408 + 24 = 752 against a 590 margin: shift by 162.
        let layout = row(1920., open(255.), stay(320.), true);
        assert!(!layout.right.ebbs_to_fit);
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
        assert!(layout.left.ebbs_to_fit && layout.right.ebbs_to_fit);
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
        assert!(!layout.right.ebbs_to_fit, "it is ebbed already");
        assert_eq!(
            layout.right_padding,
            px(1512.) - centred(1512.) - COLUMN - TIDE_MARK_WIDTH
        );
    }

    #[test]
    fn without_centred_layout_the_desk_ebbs_for_a_thread_that_cannot_fit() {
        // The V44 numbers, with the thread's room asked for and no rail.
        let desk = open(320.);
        let room = |window: f32| ordinary_row(px(window), None, desk, MIN_COLUMN, Some(MESSAGES));
        assert_eq!(
            room(1440.),
            CenterRow::Ordinary {
                thread: Some(THREAD),
                right_ebbs_for_thread: false,
                right_width: px(320.)
            },
            "a 1120 centre holds the thread beside the Desk"
        );
        assert_eq!(
            room(1024.),
            CenterRow::Ordinary {
                thread: Some(THREAD),
                right_ebbs_for_thread: true,
                right_width: px(320.)
            },
            "a 704 centre gets it from the Desk ebbing"
        );
        assert_eq!(room(760.), CenterRow::default(), "too narrow: it floats");
        assert_eq!(
            ordinary_row(px(1024.), None, stay(320.), MIN_COLUMN, Some(MESSAGES)),
            CenterRow::default(),
            "a staying Desk never ebbs for it"
        );
        assert_eq!(
            ordinary_row(px(1024.), None, desk, MIN_COLUMN, None),
            CenterRow::default()
        );
        assert!(room(1024.).tide_mark(DockPosition::Right));
        assert!(!room(1440.).tide_mark(DockPosition::Right));
        assert_eq!(
            ordinary_row(
                px(1024.),
                None,
                desk,
                MIN_COLUMN,
                Some(ThreadRoom {
                    ebbs_docks: false,
                    ..MESSAGES
                })
            ),
            CenterRow::default(),
            "a bubble floats rather than ebb the Desk"
        );
    }

    #[test]
    fn a_bubble_takes_free_room_but_never_ebbs_a_dock() {
        let bubble = ThreadRoom {
            ebbs_docks: false,
            ..MESSAGES
        };
        let layout = anchor(AnchorInput {
            thread: Some(bubble),
            ..input(1920., open(255.), open(320.))
        });
        assert!(
            !layout.right.mark,
            "a thread with messages would ebb the Desk here"
        );
        assert!(!layout.thread_floats, "the page shifts for it instead");
        let layout = anchor(AnchorInput {
            thread: Some(bubble),
            ..input(1512., open(255.), open(320.))
        });
        assert!(!layout.right.mark);
        assert!(layout.thread_floats, "with no free room it floats");
    }

    #[test]
    fn the_page_centres_on_the_window_not_the_row() {
        // A sidebar takes 200 on the left of a 1920 window: the row is 1720
        // and the window's centre falls at 760 in it.
        let layout = anchor(AnchorInput {
            row: px(1720.),
            centre: px(760.),
            ..input(1720., closed(), open(320.))
        });
        assert_eq!(layout.left_padding, px(760.) - COLUMN / 2.);
        assert_eq!(layout.column, COLUMN);
    }

    #[test]
    fn docks_that_stay_shrink_to_fit_a_tiny_window() {
        let layout = row(500., stay(255.), stay(320.), false);
        assert!(layout.left.width + layout.right.width <= px(500.));
        assert!(layout.left.width > Pixels::ZERO && layout.right.width > Pixels::ZERO);
    }

    #[test]
    fn a_focused_dock_is_kept_whole_where_it_would_ebb() {
        let focused = AnchorSide {
            focused: true,
            ..open(440.)
        };
        let layout = row(1512., open(255.), focused, false);
        assert!(!layout.right.mark && !layout.right.ebbs_to_fit);
        assert!(layout.right.kept_for_focus);
        assert!(!row(1920., open(255.), focused, false).right.kept_for_focus);
    }

    #[test]
    fn a_tiny_window_squeezes_the_page_without_going_negative() {
        let layout = row(500., stay(255.), stay(320.), true);
        assert!(layout.thread_floats);
        assert_eq!(layout.column, Pixels::ZERO);
        assert!(layout.left_padding >= Pixels::ZERO && layout.right_padding >= Pixels::ZERO);
    }
}
