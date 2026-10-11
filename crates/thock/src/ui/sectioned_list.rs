//! A list of titled sections over `gpui::uniform_list` (V41 §9): one spine
//! the Inbox view uses today and a future Backlog refresh can share.
//!
//! The delegate shape follows gpui-kit's `ListDelegate`
//! (`crates/ui/src/list/list.rs` in github.com/longbridge/gpui-component,
//! Apache License, Version 2.0): the view says how many sections and items it
//! has and renders a header or a row on demand; the layout here flattens that
//! into the single index space `uniform_list` wants and answers the questions
//! keyboard navigation asks of it.

use gpui::{AnyElement, Context, ElementId, Render, UniformList, Window, uniform_list};
use std::rc::Rc;

/// What a view renders into a sectioned list.
pub trait SectionedListDelegate: Render + Sized {
    fn sections(&self) -> usize;
    fn items_in(&self, section: usize) -> usize;
    fn render_header(
        &mut self,
        section: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement;
    fn render_item(
        &mut self,
        section: usize,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement;

    /// The flattened rows: headers for non-empty sections, then their items.
    fn layout(&self) -> SectionedLayout {
        SectionedLayout::new((0..self.sections()).map(|section| self.items_in(section)))
    }
}

/// One row of the flattened list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionedRow {
    Header(usize),
    Item { section: usize, index: usize },
}

/// The flattened row order for a set of section sizes. An empty section has
/// no header, so the list never shows a title with nothing under it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SectionedLayout {
    rows: Vec<SectionedRow>,
}

impl SectionedLayout {
    pub fn new(counts: impl IntoIterator<Item = usize>) -> Self {
        let mut rows = Vec::new();
        for (section, count) in counts.into_iter().enumerate() {
            if count == 0 {
                continue;
            }
            rows.push(SectionedRow::Header(section));
            rows.extend((0..count).map(|index| SectionedRow::Item { section, index }));
        }
        Self { rows }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row(&self, row: usize) -> Option<SectionedRow> {
        self.rows.get(row).copied()
    }

    /// The items in list order, as `(row, section, index)`.
    pub fn items(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(row, entry)| match entry {
                SectionedRow::Item { section, index } => Some((row, *section, *index)),
                SectionedRow::Header(_) => None,
            })
    }

    pub fn item_count(&self) -> usize {
        self.items().count()
    }

    /// The row an item sits on, for scrolling it into view.
    pub fn row_of(&self, section: usize, index: usize) -> Option<usize> {
        self.items()
            .find(|(_, s, i)| *s == section && *i == index)
            .map(|(row, _, _)| row)
    }

    /// The item `steps` positions from `(section, index)` in list order,
    /// clamped to the ends; the first item when nothing is selected.
    pub fn step(&self, from: Option<(usize, usize)>, steps: isize) -> Option<(usize, usize)> {
        let items: Vec<(usize, usize)> = self.items().map(|(_, s, i)| (s, i)).collect();
        if items.is_empty() {
            return None;
        }
        let position = from
            .and_then(|(section, index)| items.iter().position(|item| *item == (section, index)));
        let target = match position {
            Some(position) => (position as isize + steps).clamp(0, items.len() as isize - 1),
            None if steps < 0 => items.len() as isize - 1,
            None => 0,
        };
        items.get(target as usize).copied()
    }

    pub fn first(&self) -> Option<(usize, usize)> {
        self.items().next().map(|(_, s, i)| (s, i))
    }

    pub fn last(&self) -> Option<(usize, usize)> {
        self.items().last().map(|(_, s, i)| (s, i))
    }
}

/// The `uniform_list` for a delegate, rendering headers and items through
/// it. The caller tracks scrolling with its own `UniformListScrollHandle`.
///
/// Every row gets the height of the first item, so items must render at one
/// fixed height and headers must be no taller than an item.
pub fn sectioned_list<V: SectionedListDelegate + 'static>(
    id: impl Into<ElementId>,
    layout: Rc<SectionedLayout>,
    cx: &mut Context<V>,
) -> UniformList {
    let measured_row = layout.items().next().map(|(row, _, _)| row);
    uniform_list(
        id,
        layout.len(),
        cx.processor(move |view, range: std::ops::Range<usize>, window, cx| {
            range
                .filter_map(|row| layout.row(row))
                .map(|row| match row {
                    SectionedRow::Header(section) => view.render_header(section, window, cx),
                    SectionedRow::Item { section, index } => {
                        view.render_item(section, index, window, cx)
                    }
                })
                .collect()
        }),
    )
    // `uniform_list` sizes every row like the one it measures; a header is
    // shorter than an item, so measuring row 0 would squeeze the items.
    .with_width_from_item(measured_row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_skips_empty_sections_and_walks_items_in_order() {
        let layout = SectionedLayout::new([2, 0, 1]);
        assert_eq!(layout.len(), 5);
        assert_eq!(layout.item_count(), 3);
        assert_eq!(layout.row(0), Some(SectionedRow::Header(0)));
        assert_eq!(
            layout.row(2),
            Some(SectionedRow::Item {
                section: 0,
                index: 1
            })
        );
        assert_eq!(layout.row(3), Some(SectionedRow::Header(2)));
        assert_eq!(layout.row(5), None);
        assert_eq!(layout.row_of(2, 0), Some(4));
        assert_eq!(layout.row_of(1, 0), None);

        assert_eq!(layout.first(), Some((0, 0)));
        assert_eq!(layout.last(), Some((2, 0)));
        assert_eq!(layout.step(None, 1), Some((0, 0)));
        assert_eq!(layout.step(None, -1), Some((2, 0)));
        assert_eq!(layout.step(Some((0, 1)), 1), Some((2, 0)));
        assert_eq!(layout.step(Some((2, 0)), 1), Some((2, 0)));
        assert_eq!(layout.step(Some((0, 0)), -1), Some((0, 0)));
        assert_eq!(layout.step(Some((2, 0)), -2), Some((0, 0)));
        // A selection that no longer exists restarts from the top.
        assert_eq!(layout.step(Some((7, 7)), 1), Some((0, 0)));

        let empty = SectionedLayout::new([0, 0]);
        assert!(empty.is_empty());
        assert_eq!(empty.step(None, 1), None);
    }
}
