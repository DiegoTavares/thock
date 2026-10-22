//! A one-month calendar with a keyboard cursor (V45 §8.3), and the day model
//! it shares with the masthead's two-week strip: weeks start on Monday, and
//! days and months are named in English.
//!
//! Ported from gpui-kit's calendar (`crates/base/src/calendar.rs` and its
//! styled facade `crates/component/src/time/calendar.rs` in
//! github.com/longbridge/gpui-kit, formerly gpui-component).
//! Copyright 2024 - 2026 Longbridge <https://longbridge.com>. Licensed under
//! the Apache License, Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0);
//! distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
//! KIND. Changed for Thock: one month and a single date, the selection is the
//! keyboard cursor and its month is the one shown, weeks start on Monday, no
//! month or year views or ranges, and Zed's theme and `ui` components.

use crate::thock_theme::on_accent_color;
use chrono::{Datelike, Days, Months, NaiveDate, Weekday};
use gpui::{App, ElementId, FontWeight, SharedString, Window, div, prelude::*, px, rems};
use std::rc::Rc;
use ui::prelude::*;
use ui::{IconButton, IconButtonShape, IconName, IconSize};

/// The Monday of `date`'s week.
pub fn monday_of(date: NaiveDate) -> NaiveDate {
    date - Days::new(date.weekday().num_days_from_monday() as u64)
}

/// The full English name of `date`'s weekday.
pub fn weekday_name(date: NaiveDate) -> &'static str {
    match date.weekday() {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

/// The full English name of `date`'s month.
pub fn month_name(date: NaiveDate) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[date.month0() as usize]
}

/// The weeks of `date`'s month, Monday first, padded with the neighbouring
/// months' days so every row is full.
pub fn month_weeks(date: NaiveDate) -> Vec<[NaiveDate; 7]> {
    let Some(first) = date.with_day(1) else {
        return Vec::new();
    };
    let Some(next_first) = first.checked_add_months(Months::new(1)) else {
        return Vec::new();
    };
    let mut weeks = Vec::new();
    let mut monday = monday_of(first);
    while monday < next_first {
        weeks.push(std::array::from_fn(|offset| {
            monday + Days::new(offset as u64)
        }));
        monday = monday + Days::new(7);
    }
    weeks
}

/// `date` moved by `months`, landing on the month's last day when it is
/// shorter (31 January plus one month is 28 or 29 February).
pub fn add_months(date: NaiveDate, months: i32) -> NaiveDate {
    let moved = if months >= 0 {
        date.checked_add_months(Months::new(months.unsigned_abs()))
    } else {
        date.checked_sub_months(Months::new(months.unsigned_abs()))
    };
    moved.unwrap_or(date)
}

/// `date` moved by `days`, staying put at the ends of the calendar.
pub fn add_days(date: NaiveDate, days: i64) -> NaiveDate {
    let moved = if days >= 0 {
        date.checked_add_days(Days::new(days.unsigned_abs()))
    } else {
        date.checked_sub_days(Days::new(days.unsigned_abs()))
    };
    moved.unwrap_or(date)
}

type DateHandler = Rc<dyn Fn(NaiveDate, &mut Window, &mut App)>;

/// The month of `selected`, with `selected` highlighted and `today` marked.
/// Clicking a day or an arrow reports the new date to the owner, who keeps
/// the selection.
#[derive(IntoElement)]
pub struct Calendar {
    id: ElementId,
    selected: NaiveDate,
    today: NaiveDate,
    on_select: Option<DateHandler>,
}

impl Calendar {
    pub fn new(id: impl Into<ElementId>, selected: NaiveDate, today: NaiveDate) -> Self {
        Self {
            id: id.into(),
            selected,
            today,
            on_select: None,
        }
    }

    /// Called with the clicked day, or the selection moved a month by an arrow.
    pub fn on_select(
        mut self,
        handler: impl Fn(NaiveDate, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    fn render_arrow(&self, months: i32) -> IconButton {
        let (name, icon) = if months < 0 {
            ("calendar-previous", IconName::ChevronLeft)
        } else {
            ("calendar-next", IconName::ChevronRight)
        };
        let target = add_months(self.selected, months);
        let on_select = self.on_select.clone();
        IconButton::new(name, icon)
            .shape(IconButtonShape::Square)
            .icon_size(IconSize::Small)
            .on_click(move |_, window, cx| {
                if let Some(on_select) = &on_select {
                    on_select(target, window, cx);
                }
            })
    }

    fn render_day(&self, date: NaiveDate, cx: &App) -> AnyElement {
        let colors = cx.theme().colors();
        let selected = date == self.selected;
        let in_month = date.month() == self.selected.month();
        let text_color = if selected {
            on_accent_color(cx)
        } else if in_month {
            colors.text
        } else {
            colors.text_placeholder
        };
        let on_select = self.on_select.clone();
        div()
            .id(SharedString::from(format!("calendar-day-{date}")))
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .text_size(rems(0.8125))
            .text_color(text_color)
            .when(selected, |this| {
                this.bg(colors.text_accent)
                    .font_weight(FontWeight::SEMIBOLD)
            })
            .when(date == self.today && !selected, |this| {
                this.border_1().border_color(colors.text_accent)
            })
            .when(!selected, |this| {
                this.hover(|this| this.bg(colors.element_hover))
            })
            .on_click(move |_, window, cx| {
                if let Some(on_select) = &on_select {
                    on_select(date, window, cx);
                }
            })
            .child(SharedString::from(date.day().to_string()))
            .into_any_element()
    }
}

impl RenderOnce for Calendar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let title = format!("{} {}", month_name(self.selected), self.selected.year());
        let header = h_flex()
            .justify_between()
            .items_center()
            .child(self.render_arrow(-1))
            .child(
                Label::new(title)
                    .size(LabelSize::Default)
                    .weight(FontWeight::SEMIBOLD),
            )
            .child(self.render_arrow(1));
        let weekday_row = h_flex().children(
            month_weeks(self.selected)
                .first()
                .into_iter()
                .flatten()
                .map(|date| {
                    div()
                        .size(px(30.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(rems(0.6875))
                        .text_color(colors.text_muted)
                        .child(SharedString::from(&weekday_name(*date)[..2]))
                }),
        );
        let weeks = month_weeks(self.selected)
            .into_iter()
            .map(|week| h_flex().children(week.iter().map(|date| self.render_day(*date, cx))))
            .collect::<Vec<_>>();
        v_flex()
            .id(self.id)
            .gap_0p5()
            .child(header)
            .child(weekday_row)
            .children(weeks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn a_month_is_whole_weeks_from_monday() {
        let weeks = month_weeks(date(2026, 10, 21));
        assert_eq!(weeks.len(), 5);
        assert_eq!(weeks[0][0], date(2026, 9, 28));
        assert_eq!(weeks[4][6], date(2026, 11, 1));
        assert!(weeks.iter().all(|week| week[0].weekday() == Weekday::Mon));
    }

    #[test]
    fn a_month_starting_on_monday_has_no_leading_days() {
        let weeks = month_weeks(date(2026, 6, 15));
        assert_eq!(weeks[0][0], date(2026, 6, 1));
        assert_eq!(weeks.last().unwrap()[6], date(2026, 7, 5));
    }

    #[test]
    fn moving_a_month_clamps_to_the_shorter_month() {
        assert_eq!(add_months(date(2026, 1, 31), 1), date(2026, 2, 28));
        assert_eq!(add_months(date(2026, 3, 31), -1), date(2026, 2, 28));
        assert_eq!(add_months(date(2026, 12, 2), 1), date(2027, 1, 2));
    }

    #[test]
    fn names_and_mondays() {
        assert_eq!(monday_of(date(2026, 10, 21)), date(2026, 10, 19));
        assert_eq!(weekday_name(date(2026, 10, 21)), "Wednesday");
        assert_eq!(month_name(date(2026, 10, 21)), "October");
    }
}
