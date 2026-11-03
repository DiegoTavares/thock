//! Where the centre's page sits between the docks when centred layout is on.
//!
//! The page fills the room between the docks up to its measure, centred on
//! the window when the margins allow it. Tucking a dock away while you type
//! never moves it. [`anchor`] decides the whole row on every render,
//! from the docks' widths and each side's [`Tide`], which the view that sets
//! the anchor owns.

use gpui::{Pixels, px};

use crate::dock::DockPosition;

/// The narrowest gap between the column and anything beside it.
pub const ANCHOR_GAP: Pixels = px(24.);
/// The width an ebbed dock keeps at the window's edge.
pub const TIDE_MARK_WIDTH: Pixels = px(6.);

/// How a side dock rides the tides beside an anchored page. Never
/// serialized.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Tide {
    /// Drawn whole, or as its mark when not even the page's narrowest fits
    /// beside it.
    #[default]
    Flowing,
    /// Tucked away while you write: drawn as its tide mark.
    Ebbed,
    /// Brought back by hand: the page makes room for it rather than tuck
    /// it away to fit.
    Held,
}

/// One side as the view setting the anchor sees it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TideSide {
    /// The dock may be tucked away to fit the page.
    pub may_ebb: bool,
    pub tide: Tide,
}

/// What a view asks the centre for while centred layout is on
/// ([`crate::Workspace::set_center_anchor`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterAnchor {
    /// The page's widest.
    pub column: Pixels,
    /// How far the page may narrow before a dock ebbs to fit or something
    /// floats over it.
    pub min_column: Pixels,
    /// Room to keep in the right margin beside the page, if any.
    pub thread: Option<ThreadRoom>,
    pub left: TideSide,
    pub right: TideSide,
}

/// Room asked for on the right of the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreadRoom {
    pub width: Pixels,
    /// Whether the page may move or a dock ebb to make it. Only a thread
    /// with messages may; anything else takes the room when it's free.
    pub moves_page: bool,
}

/// One side's dock as [`anchor`] sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnchorSide {
    /// The dock's width while it is open; `None` when it is closed.
    pub open_width: Option<Pixels>,
    pub tide: Tide,
    /// The dock may ebb to fit the page.
    pub may_ebb: bool,
    /// The dock holds the focus, so it is drawn whole whatever its tide:
    /// focus never sits in a mark.
    pub focused: bool,
}

impl AnchorSide {
    fn is_mark(&self, ebbs_to_fit: bool) -> bool {
        self.open_width.is_some() && !self.focused && (self.tide == Tide::Ebbed || ebbs_to_fit)
    }

    fn width(&self, ebbs_to_fit: bool) -> Pixels {
        match self.open_width {
            Some(_) if self.is_mark(ebbs_to_fit) => TIDE_MARK_WIDTH,
            Some(width) => width,
            None => Pixels::ZERO,
        }
    }

    /// What the side takes from the page's room: its open width, or its
    /// mark when ebbed to fit. A side ebbed while you type keeps its open
    /// width here, so the page doesn't move when it ebbs.
    fn room(&self, ebbs_to_fit: bool) -> Pixels {
        match self.open_width {
            Some(_) if ebbs_to_fit => TIDE_MARK_WIDTH,
            Some(width) => width,
            None => Pixels::ZERO,
        }
    }

    /// Held docks stay whole; an ebbed one is placed as if it still flowed.
    fn can_ebb_to_fit(&self) -> bool {
        self.open_width.is_some() && !self.focused && self.may_ebb && self.tide != Tide::Held
    }

    fn drawn(&self, ebbs_to_fit: bool, width: Pixels) -> SideLayout {
        SideLayout {
            width,
            open_width: self.open_width.unwrap_or_default(),
            mark: self.is_mark(ebbs_to_fit),
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
    /// What the dock takes: nothing when closed, the mark when tucked away.
    pub width: Pixels,
    /// The dock's own width, which its panel keeps behind a mark.
    pub open_width: Pixels,
    /// Drawn as its tide mark: ebbed, or ebbed to fit.
    pub mark: bool,
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
    /// Zed's own layout, with the thread's room on the right when it fits.
    Ordinary { thread: Option<Pixels> },
    /// The page pinned to the window's centre.
    Anchored(AnchorLayout),
}

impl Default for CenterRow {
    fn default() -> Self {
        Self::Ordinary { thread: None }
    }
}

impl CenterRow {
    pub fn anchored(&self) -> Option<AnchorLayout> {
        match self {
            Self::Anchored(layout) => Some(*layout),
            Self::Ordinary { .. } => None,
        }
    }

    fn side(&self, position: DockPosition) -> Option<SideLayout> {
        match (self, position) {
            (Self::Anchored(layout), DockPosition::Left) => Some(layout.left),
            (Self::Anchored(layout), DockPosition::Right) => Some(layout.right),
            _ => None,
        }
    }

    /// Whether the dock at `position` is drawn as its tide mark in this row.
    pub fn tide_mark(&self, position: DockPosition) -> bool {
        self.side(position).is_some_and(|side| side.mark)
    }

    /// The dock's own width at `position`, which its panel keeps behind a
    /// mark so nothing in it reflows.
    pub fn open_width(&self, position: DockPosition) -> Pixels {
        self.side(position)
            .map_or(Pixels::ZERO, |side| side.open_width)
    }
}

/// The row without an anchored page: the thread keeps its room on the
/// right when `centre`, the width Zed's own row leaves between the docks,
/// still holds `min_column` beside it, and floats otherwise. The docks are
/// left as they are.
pub fn ordinary_row(centre: Pixels, min_column: Pixels, thread: Option<ThreadRoom>) -> CenterRow {
    CenterRow::Ordinary {
        thread: thread
            .filter(|thread| centre - thread.width >= min_column)
            .map(|thread| thread.width),
    }
}

/// Places the page: as wide as the room between the docks allows, up to
/// `column`, and centred on the window when the margins allow it, else
/// shifted toward the side with room. When not even `min_column` fits, the
/// docks that may ebb do; else the thread floats and the page takes what is
/// left. A thread without messages never moves the page: it takes the room
/// only when the right margin already has it free.
pub fn anchor(input: AnchorInput) -> AnchorLayout {
    let without_thread = || place(&input, Pixels::ZERO).unwrap_or_else(|| squeeze(&input));
    let Some(thread) = input.thread else {
        return without_thread();
    };
    if thread.moves_page
        && let Some(layout) = place(&input, thread.width)
    {
        return layout;
    }
    let mut layout = without_thread();
    layout.thread_floats = thread.moves_page || layout.right_padding < thread.width + ANCHOR_GAP;
    layout
}

/// Steps 1 to 3 with `room` kept in the right margin, or `None` when the
/// page can't keep `min_column` beside it even with the docks that may ebb
/// tucked away. A side ebbed while you type is placed as if it were open,
/// so tucking it away never moves the page.
fn place(input: &AnchorInput, room: Pixels) -> Option<AnchorLayout> {
    let row = input.row;
    let at = |left_ebbs: bool, right_ebbs: bool| {
        let left = input.left.room(left_ebbs) + ANCHOR_GAP;
        let right = input.right.room(right_ebbs) + room + ANCHOR_GAP;
        let free = row - left - right;
        if free < input.min_column.min(input.column) {
            return None;
        }
        let column = input.column.min(free);
        let x = (input.centre - column / 2.)
            .max(left)
            .min(row - right - column);
        let left_width = input.left.width(left_ebbs);
        let right_width = input.right.width(right_ebbs);
        Some(AnchorLayout {
            left: input.left.drawn(left_ebbs, left_width),
            left_padding: x - left_width,
            column,
            right_padding: row - right_width - x - column,
            right: input.right.drawn(right_ebbs, right_width),
            thread_floats: false,
        })
    };

    // One side first: the right while the thread asks for room there, else
    // the wider one.
    let right_first = room > Pixels::ZERO || input.right.room(false) >= input.left.room(false);
    let one_side = if right_first {
        [(false, true), (true, false)]
    } else {
        [(true, false), (false, true)]
    };
    [(false, false)]
        .into_iter()
        .chain(one_side)
        .chain([(true, true)])
        .filter(|(left, right)| {
            (!left || input.left.can_ebb_to_fit()) && (!right || input.right.can_ebb_to_fit())
        })
        .find_map(|(left, right)| at(left, right))
}

/// The last resort: no thread, and the page takes whatever is between
/// the docks' gaps, even below `min_column`. Docks drawn whole shrink
/// together when even they don't fit, so neither runs off the window;
/// marks keep their width.
fn squeeze(input: &AnchorInput) -> AnchorLayout {
    let left_ebbs = input.left.can_ebb_to_fit();
    let right_ebbs = input.right.can_ebb_to_fit();
    let mut left = input.left.width(left_ebbs);
    let mut right = input.right.width(right_ebbs);
    let left_whole = !input.left.is_mark(left_ebbs);
    let right_whole = !input.right.is_mark(right_ebbs);
    let whole = if left_whole { left } else { Pixels::ZERO }
        + if right_whole { right } else { Pixels::ZERO };
    let room = (input.row - (left + right - whole)).max(Pixels::ZERO);
    if whole > room {
        let scale = room / whole;
        if left_whole {
            left = left * scale;
        }
        if right_whole {
            right = right * scale;
        }
    }
    let gap = ANCHOR_GAP.min(((input.row - left - right) / 2.).max(Pixels::ZERO));
    let column = (input.row - left - right - gap * 2.).max(Pixels::ZERO);
    AnchorLayout {
        left: input.left.drawn(left_ebbs, left),
        left_padding: gap,
        column,
        right_padding: gap,
        right: input.right.drawn(right_ebbs, right),
        thread_floats: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMN: Pixels = px(870.);
    const MIN_COLUMN: Pixels = px(530.);
    const THREAD: Pixels = px(408.);
    const MESSAGES: ThreadRoom = ThreadRoom {
        width: THREAD,
        moves_page: true,
    };
    const BUBBLE: ThreadRoom = ThreadRoom {
        width: THREAD,
        moves_page: false,
    };

    fn open(width: f32) -> AnchorSide {
        AnchorSide {
            open_width: Some(px(width)),
            tide: Tide::Flowing,
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

    fn with_tide(tide: Tide, side: AnchorSide) -> AnchorSide {
        AnchorSide { tide, ..side }
    }

    fn focused(side: AnchorSide) -> AnchorSide {
        AnchorSide {
            focused: true,
            ..side
        }
    }

    fn closed() -> AnchorSide {
        AnchorSide::default()
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

    fn row(window: f32, left: AnchorSide, right: AnchorSide, thread: bool) -> AnchorLayout {
        anchor(AnchorInput {
            thread: thread.then_some(MESSAGES),
            ..input(window, left, right)
        })
    }

    /// Where the page starts, from the row's left edge.
    fn page_x(layout: &AnchorLayout) -> Pixels {
        layout.left.width + layout.left_padding
    }

    fn centred(window: f32) -> Pixels {
        (px(window) - COLUMN) / 2.
    }

    #[test]
    fn the_page_is_centred_on_the_window_when_the_margins_allow() {
        for (window, left, right) in [
            (1280., closed(), closed()),
            (1512., closed(), closed()),
            (1512., open(255.), closed()),
            (1920., open(255.), closed()),
            (1920., closed(), open(320.)),
            (1920., open(255.), open(320.)),
            (1920., stay(255.), stay(320.)),
        ] {
            let layout = row(window, left, right, false);
            assert_eq!(layout.column, COLUMN, "{window}: {left:?} {right:?}");
            assert_eq!(page_x(&layout), centred(window));
            assert!(!layout.left.mark && !layout.right.mark);
        }
    }

    #[test]
    fn the_page_shifts_toward_the_side_with_room_before_it_narrows() {
        // 255 + 24 on the left, 320 + 24 on the right: 889 between them.
        let layout = row(1512., open(255.), open(320.), false);
        assert_eq!(layout.column, COLUMN);
        assert_eq!(page_x(&layout), px(1512.) - px(320.) - ANCHOR_GAP - COLUMN);
        assert_eq!(layout.right_padding, ANCHOR_GAP);
    }

    #[test]
    fn the_page_fills_the_room_between_the_docks_up_to_its_measure() {
        // A 440 Desk on the MacBook leaves 769 between the gaps.
        let layout = row(1512., open(255.), open(440.), false);
        assert!(!layout.left.mark && !layout.right.mark);
        assert_eq!(
            layout.column,
            px(1512.) - px(255.) - px(440.) - ANCHOR_GAP * 2.
        );
        assert_eq!(layout.left_padding, ANCHOR_GAP);
        assert_eq!(layout.right_padding, ANCHOR_GAP);
    }

    #[test]
    fn a_dock_ebbs_to_fit_only_when_not_even_min_column_fits() {
        // 1100 leaves 477 beside both docks: the wider one ebbs.
        let layout = row(1100., open(255.), open(320.), false);
        assert!(layout.right.mark && !layout.left.mark);
        assert_eq!(
            layout.column,
            px(1100.) - px(255.) - TIDE_MARK_WIDTH - ANCHOR_GAP * 2.
        );

        // A Desk that stays leaves it to the rail.
        let layout = row(1100., open(255.), stay(320.), false);
        assert!(layout.left.mark && !layout.right.mark);

        // Not even one is enough at 700: both ebb.
        let layout = row(700., open(255.), open(320.), false);
        assert!(layout.left.mark && layout.right.mark);
        assert_eq!(
            layout.column,
            px(700.) - (TIDE_MARK_WIDTH + ANCHOR_GAP) * 2.
        );
    }

    #[test]
    fn a_held_dock_is_never_ebbed_to_fit() {
        let layout = row(1100., open(255.), with_tide(Tide::Held, open(320.)), false);
        assert!(!layout.right.mark);
        assert_eq!(layout.right.width, px(320.));
        assert!(layout.left.mark, "the other side makes room for it");
    }

    #[test]
    fn an_ebbed_dock_is_a_mark_and_the_page_stays_where_it_was() {
        for (window, width) in [(1512., 440.), (1920., 320.), (1100., 320.)] {
            let flowing = row(window, open(255.), open(width), false);
            let ebbed = row(
                window,
                open(255.),
                with_tide(Tide::Ebbed, open(width)),
                false,
            );
            assert!(ebbed.right.mark);
            assert_eq!(ebbed.right.width, TIDE_MARK_WIDTH);
            assert_eq!(ebbed.right.open_width, px(width));
            assert_eq!(ebbed.column, flowing.column, "{window}");
            assert_eq!(page_x(&ebbed), page_x(&flowing), "{window}");
        }
    }

    #[test]
    fn a_focused_ebbed_dock_is_drawn_whole() {
        let layout = row(
            1920.,
            open(255.),
            focused(with_tide(Tide::Ebbed, open(320.))),
            false,
        );
        assert!(!layout.right.mark);
        assert_eq!(layout.right.width, px(320.));
        assert_eq!(page_x(&layout), centred(1920.));
    }

    #[test]
    fn a_focused_dock_is_kept_whole_where_it_would_ebb() {
        let layout = row(1100., open(255.), focused(open(320.)), false);
        assert!(!layout.right.mark);
        assert_eq!(layout.right.width, px(320.));
        assert!(layout.left.mark);
    }

    #[test]
    fn only_a_thread_with_room_ebbs_the_desk() {
        assert!(!row(1512., open(255.), open(320.), false).right.mark);
        assert!(row(1512., open(255.), open(320.), true).right.mark);
    }

    #[test]
    fn a_thread_at_1512_ebbs_the_desk_and_the_page_fills_what_is_left() {
        // 255 + 320 + 408 and the gaps leave 481 < min_column, so the Desk
        // ebbs, and the page takes the 795 left.
        let layout = row(1512., open(255.), open(320.), true);
        assert!(layout.right.mark);
        assert!(!layout.thread_floats);
        assert_eq!(
            layout.column,
            px(1512.) - px(255.) - TIDE_MARK_WIDTH - THREAD - ANCHOR_GAP * 2.
        );
        assert!(layout.right_padding >= THREAD + ANCHOR_GAP);
    }

    #[test]
    fn a_thread_at_1920_keeps_the_desk_and_shifts_the_page_left() {
        for right in [open(320.), stay(320.)] {
            let layout = row(1920., open(255.), right, true);
            assert!(!layout.right.mark);
            assert!(!layout.thread_floats);
            assert_eq!(layout.column, COLUMN);
            assert_eq!(
                page_x(&layout),
                px(1920.) - px(320.) - THREAD - ANCHOR_GAP - COLUMN
            );
        }
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
    fn without_an_anchored_page_the_thread_takes_its_room_or_floats() {
        // The V44 numbers, with the thread's room asked for and no rail.
        // The centre is what the window leaves beside a 320 Desk.
        let room = |window: f32, thread| ordinary_row(px(window - 320.), MIN_COLUMN, thread);
        assert_eq!(
            room(1440., Some(MESSAGES)),
            CenterRow::Ordinary {
                thread: Some(THREAD)
            },
            "a 1120 centre holds the thread beside the Desk"
        );
        assert_eq!(
            room(1024., Some(MESSAGES)),
            CenterRow::default(),
            "a 704 centre can't: it floats, and the Desk is left alone"
        );
        assert_eq!(room(1440., None), CenterRow::default());
        assert!(!room(1024., Some(MESSAGES)).tide_mark(DockPosition::Right));
    }

    #[test]
    fn a_bubble_takes_free_room_but_never_moves_the_page_or_ebbs_a_dock() {
        let bubble = |window: f32, right: AnchorSide| {
            anchor(AnchorInput {
                thread: Some(BUBBLE),
                ..input(window, open(255.), right)
            })
        };
        let layout = bubble(1920., closed());
        assert!(!layout.thread_floats, "the right margin is free");
        assert_eq!(page_x(&layout), centred(1920.));
        assert!(layout.right_padding >= THREAD + ANCHOR_GAP);

        // 320 + 408 + 24 = 752 against a 590 margin: no free room.
        let layout = bubble(1920., open(320.));
        assert!(layout.thread_floats);
        assert!(!layout.right.mark, "the Desk stays whole");
        assert_eq!(page_x(&layout), centred(1920.), "and the page stays put");

        let layout = bubble(1512., open(320.));
        assert!(layout.thread_floats);
        assert!(!layout.right.mark);
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
    fn marks_keep_their_width_under_squeeze() {
        let layout = row(300., with_tide(Tide::Ebbed, open(255.)), stay(320.), false);
        assert!(layout.left.mark);
        assert_eq!(layout.left.width, TIDE_MARK_WIDTH);
        assert_eq!(layout.right.width, px(300.) - TIDE_MARK_WIDTH);
    }

    #[test]
    fn a_tiny_window_squeezes_the_page_without_going_negative() {
        let layout = row(500., stay(255.), stay(320.), true);
        assert!(layout.thread_floats);
        assert_eq!(layout.column, Pixels::ZERO);
        assert!(layout.left_padding >= Pixels::ZERO && layout.right_padding >= Pixels::ZERO);
    }
}
