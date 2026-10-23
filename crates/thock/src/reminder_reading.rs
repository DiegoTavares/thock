//! Reads what the user types after `(@` into the dates and times it could mean (V45 §8.2).
//!
//! Pure: the query and the local wall-clock time go in, readings come out. Formatting the
//! canonical token is the sync core's job; a [`Reading`] carries the date and optional time it
//! needs.

use chrono::{Datelike, Days, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

/// One thing the query could mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub date: NaiveDate,
    /// `None` is a date-only reminder.
    pub time: Option<NaiveTime>,
    /// The named day's time had already gone by, so this reading moved to the next day.
    pub already_past: bool,
}

/// Everything a query reads as: its readings, soonest first, and the words it didn't understand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Readings {
    pub readings: Vec<Reading>,
    /// Words that were skipped, as typed, for the *ignores "foo"* hint beside the first reading.
    pub ignored: Vec<String>,
}

/// A suggestion offered before anything is typed (V45 §8.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    pub label: &'static str,
    pub reading: Reading,
}

/// Reads a query against `now`, the current local time. A query with no date or time in it has
/// no readings.
pub fn read(query: &str, now: NaiveDateTime) -> Readings {
    let now = now
        .with_second(0)
        .and_then(|now| now.with_nanosecond(0))
        .unwrap_or(now);
    let words = tokenize(query);
    let mut parsed = Parsed::default();
    let mut ignored = Vec::new();
    let mut index = 0;
    while index < words.len() {
        match parse_at(&words, index, now, &mut parsed) {
            Some(consumed) => index += consumed,
            None => {
                if !is_filler(&words[index].lower) {
                    ignored.push(words[index].text.to_string());
                }
                index += 1;
            }
        }
    }
    Readings {
        readings: resolve(&parsed, now),
        ignored,
    }
}

/// The suggestions for an empty query: *Later today 17:00* (only before 16:00), then *Tomorrow*
/// and *Monday* (left out when tomorrow is already Monday) at `tomorrow_at`, the vault's
/// `[ring] tomorrow_at`.
pub fn presets(now: NaiveDateTime, tomorrow_at: NaiveTime) -> Vec<Preset> {
    let today = now.date();
    let at = |date: NaiveDate, time: Option<NaiveTime>| Reading {
        date,
        time,
        already_past: false,
    };
    let mut presets = Vec::new();
    if now.hour() < 16 {
        presets.push(Preset {
            label: "Later today",
            reading: at(today, NaiveTime::from_hms_opt(17, 0, 0)),
        });
    }
    let Some(tomorrow) = today.checked_add_days(Days::new(1)) else {
        return presets;
    };
    presets.push(Preset {
        label: "Tomorrow",
        reading: at(tomorrow, Some(tomorrow_at)),
    });
    if let Some(monday) = next_weekday(today, Weekday::Mon)
        && monday != tomorrow
    {
        presets.push(Preset {
            label: "Monday",
            reading: at(monday, Some(tomorrow_at)),
        });
    }
    presets
}

struct Word<'a> {
    text: &'a str,
    lower: String,
}

#[derive(Default)]
struct Parsed {
    date: Option<NaiveDate>,
    times: Vec<NaiveTime>,
    /// `in 2h` fixes both the date and the time, so nothing else may change either.
    exact: bool,
}

/// Splits on whitespace and commas, then splits a run like `9am` or `2h` at the digit-letter
/// boundary. Words holding `-` (an ISO date) are kept whole.
fn tokenize(query: &str) -> Vec<Word<'_>> {
    let mut words = Vec::new();
    for chunk in query.split(|character: char| character.is_whitespace() || character == ',') {
        if chunk.is_empty() {
            continue;
        }
        if chunk.contains('-') {
            words.push(Word {
                text: chunk,
                lower: chunk.to_lowercase(),
            });
            continue;
        }
        let mut start = 0;
        let mut previous_digit = None;
        for (offset, character) in chunk.char_indices() {
            let digit = character.is_ascii_digit() || character == ':';
            if previous_digit.is_some_and(|previous| previous != digit) {
                words.push(Word {
                    text: &chunk[start..offset],
                    lower: chunk[start..offset].to_lowercase(),
                });
                start = offset;
            }
            previous_digit = Some(digit);
        }
        words.push(Word {
            text: &chunk[start..],
            lower: chunk[start..].to_lowercase(),
        });
    }
    words
}

fn is_filler(word: &str) -> bool {
    matches!(word, "at" | "on")
}

/// Tries to read a phrase starting at `index`, returning how many words it took.
fn parse_at(
    words: &[Word],
    index: usize,
    now: NaiveDateTime,
    parsed: &mut Parsed,
) -> Option<usize> {
    let today = now.date();
    let word = words[index].lower.as_str();
    let next = words.get(index + 1).map(|word| word.lower.as_str());
    let can_date = parsed.date.is_none() && !parsed.exact;
    let can_time = parsed.times.is_empty() && !parsed.exact;

    if can_date {
        let date = match (word, parse_weekday(word)) {
            ("today" | "tod", _) => Some(today),
            ("tomorrow" | "tom" | "tmr" | "tmrw", _) => Some(today.checked_add_days(Days::new(1))?),
            (_, Some(weekday)) => Some(next_weekday(today, weekday)?),
            _ => NaiveDate::parse_from_str(word, "%Y-%m-%d").ok(),
        };
        if let Some(date) = date {
            parsed.date = Some(date);
            return Some(1);
        }

        if word == "next" {
            let monday_next_week = next_weekday(today, Weekday::Mon)?;
            let date = match next.map(|next| (next, parse_weekday(next))) {
                Some(("week", _)) => Some(monday_next_week),
                Some((_, Some(weekday))) => Some(
                    monday_next_week
                        .checked_add_days(Days::new(u64::from(weekday.num_days_from_monday())))?,
                ),
                _ => None,
            };
            if let Some(date) = date {
                parsed.date = Some(date);
                return Some(2);
            }
        }

        if let (Some(month), Some(day)) = (parse_month(word), next.and_then(parse_day)) {
            parsed.date = Some(month_day(today, month, day)?);
            return Some(2);
        }
        if let (Some(day), Some(month)) = (parse_day(word), next.and_then(parse_month)) {
            parsed.date = Some(month_day(today, month, day)?);
            return Some(2);
        }

        if word == "in"
            && let (Some(amount), Some(unit)) = (
                next.and_then(|next| next.parse::<u32>().ok()),
                words.get(index + 2).map(|word| word.lower.as_str()),
            )
        {
            let amount = i64::from(amount);
            let offset = match unit {
                "m" | "min" | "mins" | "minute" | "minutes" => Some(Duration::try_minutes(amount)?),
                "h" | "hr" | "hrs" | "hour" | "hours" => Some(Duration::try_hours(amount)?),
                _ => None,
            };
            if let Some(offset) = offset
                && can_time
            {
                let moment = now.checked_add_signed(offset)?;
                parsed.date = Some(moment.date());
                parsed.times = vec![moment.time()];
                parsed.exact = true;
                return Some(3);
            }
            let days = match unit {
                "d" | "day" | "days" => Some(amount),
                "w" | "wk" | "wks" | "week" | "weeks" => Some(amount.checked_mul(7)?),
                _ => None,
            };
            if let Some(days) = days {
                parsed.date = Some(today.checked_add_days(Days::new(u64::try_from(days).ok()?))?);
                return Some(3);
            }
        }
    }

    if can_time {
        let named = match word {
            "morning" => Some(9),
            "noon" => Some(12),
            "afternoon" => Some(15),
            "evening" => Some(18),
            "tonight" => Some(20),
            _ => None,
        };
        if let Some(hour) = named {
            parsed.times = vec![NaiveTime::from_hms_opt(hour, 0, 0)?];
            return Some(1);
        }

        let (hour, minute) = parse_clock(word)?;
        let meridiem = match next {
            Some("am" | "a.m.") => Some(false),
            Some("pm" | "p.m.") => Some(true),
            _ => None,
        };
        if let Some(pm) = meridiem {
            if !(1..=12).contains(&hour) {
                return None;
            }
            let hour = hour % 12 + if pm { 12 } else { 0 };
            parsed.times = vec![NaiveTime::from_hms_opt(hour, minute, 0)?];
            return Some(2);
        }
        let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
        parsed.times = vec![time];
        // A bare hour on the clock face could be either side of noon.
        if word.len() <= 2 && (1..=12).contains(&hour) {
            parsed
                .times
                .push(NaiveTime::from_hms_opt((hour + 12) % 24, 0, 0)?);
        }
        return Some(1);
    }

    None
}

fn resolve(parsed: &Parsed, now: NaiveDateTime) -> Vec<Reading> {
    let today = now.date();
    let mut readings: Vec<Reading> = match (parsed.date, parsed.times.as_slice()) {
        (None, []) => Vec::new(),
        (Some(date), []) => vec![Reading {
            date,
            time: None,
            already_past: date < today,
        }],
        (date, times) => times
            .iter()
            .filter_map(|&time| {
                let date_named = date.is_some();
                let date = date.unwrap_or(today);
                let past_today = date == today && time <= now.time();
                Some(Reading {
                    date: if past_today {
                        date.checked_add_days(Days::new(1))?
                    } else {
                        date
                    },
                    time: Some(time),
                    already_past: date_named && (past_today || date < today),
                })
            })
            .collect(),
    };
    readings.sort_by_key(|reading| (reading.date, reading.time));
    readings.dedup_by_key(|reading| (reading.date, reading.time));
    readings
}

/// The first `weekday` after `today`; a weekday never means today. `None`
/// past the last date chrono can hold.
fn next_weekday(today: NaiveDate, weekday: Weekday) -> Option<NaiveDate> {
    let ahead = (weekday.num_days_from_monday() + 7 - today.weekday().num_days_from_monday()) % 7;
    today.checked_add_days(Days::new(u64::from(if ahead == 0 { 7 } else { ahead })))
}

/// This year's `month`/`day`, or next year's once this year's has gone by.
fn month_day(today: NaiveDate, month: u32, day: u32) -> Option<NaiveDate> {
    match NaiveDate::from_ymd_opt(today.year(), month, day) {
        Some(date) if date >= today => Some(date),
        _ => NaiveDate::from_ymd_opt(today.year() + 1, month, day),
    }
}

fn parse_weekday(word: &str) -> Option<Weekday> {
    Some(match word {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "weds" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

fn parse_month(word: &str) -> Option<u32> {
    Some(match word {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    })
}

fn parse_day(word: &str) -> Option<u32> {
    if word.len() > 2 {
        return None;
    }
    word.parse().ok().filter(|day| (1..=31).contains(day))
}

/// `10`, `9:30` or `14:05` as an hour and minute, or the compact `930` and
/// `1430` the calendar's time entry takes.
fn parse_clock(word: &str) -> Option<(u32, u32)> {
    let (hour, minute) = match word.split_once(':') {
        Some((hour, minute)) if minute.len() == 2 => (hour, minute.parse().ok()?),
        Some(_) => return None,
        None if word.len() > 2 && word.bytes().all(|byte| byte.is_ascii_digit()) => {
            let (hour, minute) = word.split_at(word.len() - 2);
            (hour, minute.parse().ok()?)
        }
        None => (word, 0),
    };
    if hour.is_empty() || hour.len() > 2 {
        return None;
    }
    let hour = hour.parse().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).unwrap()
    }

    fn time(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).unwrap()
    }

    /// Wednesday 21 October 2026 at 14:05, the spec's clock.
    fn now() -> NaiveDateTime {
        date(10, 21).and_time(time(14, 5))
    }

    fn on(date: NaiveDate) -> Reading {
        Reading {
            date,
            time: None,
            already_past: false,
        }
    }

    fn at(date: NaiveDate, hour: u32, minute: u32) -> Reading {
        Reading {
            date,
            time: Some(time(hour, minute)),
            already_past: false,
        }
    }

    fn past(reading: Reading) -> Reading {
        Reading {
            already_past: true,
            ..reading
        }
    }

    fn readings(query: &str) -> Vec<Reading> {
        let result = read(query, now());
        assert_eq!(result.ignored, Vec::<String>::new(), "{query:?}");
        result.readings
    }

    #[test]
    fn today_and_tomorrow() {
        for query in ["today", "tod", "Today"] {
            assert_eq!(readings(query), [on(date(10, 21))], "{query:?}");
        }
        for query in ["tomorrow", "tom", "tmr", "tmrw"] {
            assert_eq!(readings(query), [on(date(10, 22))], "{query:?}");
        }
    }

    #[test]
    fn weekdays_never_mean_today() {
        assert_eq!(readings("fri"), [on(date(10, 23))]);
        assert_eq!(readings("friday"), [on(date(10, 23))]);
        assert_eq!(readings("mon"), [on(date(10, 26))]);
        assert_eq!(readings("wed"), [on(date(10, 28))]);
        assert_eq!(readings("tue"), [on(date(10, 27))]);
    }

    #[test]
    fn next_week_and_next_weekday() {
        assert_eq!(readings("next mon"), [on(date(10, 26))]);
        assert_eq!(readings("next week"), [on(date(10, 26))]);
        assert_eq!(readings("next fri"), [on(date(10, 30))]);
        assert_eq!(readings("next wed"), [on(date(10, 28))]);
    }

    #[test]
    fn month_and_day_either_way_round() {
        assert_eq!(readings("dec 2"), [on(date(12, 2))]);
        assert_eq!(readings("2 dec"), [on(date(12, 2))]);
        assert_eq!(readings("December 2"), [on(date(12, 2))]);
        assert_eq!(readings("oct 21"), [on(date(10, 21))]);
        let next_year = NaiveDate::from_ymd_opt(2027, 3, 1).unwrap();
        assert_eq!(readings("mar 1"), [on(next_year)]);
        assert_eq!(
            readings("oct 20"),
            [on(NaiveDate::from_ymd_opt(2027, 10, 20).unwrap())]
        );
    }

    #[test]
    fn iso_date() {
        assert_eq!(readings("2026-12-02"), [on(date(12, 2))]);
        assert_eq!(readings("2026-12-02 14:30"), [at(date(12, 2), 14, 30)]);
    }

    #[test]
    fn in_an_amount_of_time() {
        assert_eq!(readings("in 2h"), [at(date(10, 21), 16, 5)]);
        assert_eq!(readings("in 30 min"), [at(date(10, 21), 14, 35)]);
        assert_eq!(readings("in 10h"), [at(date(10, 22), 0, 5)]);
        assert_eq!(readings("in 3 days"), [on(date(10, 24))]);
        assert_eq!(readings("in 2 weeks"), [on(date(11, 4))]);
    }

    #[test]
    fn clock_times() {
        assert_eq!(readings("10:30"), [at(date(10, 22), 10, 30)]);
        assert_eq!(readings("16:30"), [at(date(10, 21), 16, 30)]);
        assert_eq!(readings("9am"), [at(date(10, 22), 9, 0)]);
        assert_eq!(readings("9 pm"), [at(date(10, 21), 21, 0)]);
        assert_eq!(readings("9:30pm"), [at(date(10, 21), 21, 30)]);
        assert_eq!(readings("12pm"), [at(date(10, 22), 12, 0)]);
        assert_eq!(readings("12am"), [at(date(10, 22), 0, 0)]);
        assert_eq!(readings("14:05"), [at(date(10, 22), 14, 5)]);
        assert_eq!(readings("1430"), [at(date(10, 21), 14, 30)]);
        assert_eq!(readings("930"), [at(date(10, 22), 9, 30)]);
        assert_eq!(readings("930pm"), [at(date(10, 21), 21, 30)]);
        assert_eq!(readings("tom 0905"), [at(date(10, 22), 9, 5)]);
        assert!(read("2460", now()).readings.is_empty());
        assert!(read("12345", now()).readings.is_empty());
    }

    #[test]
    fn bare_hour_reads_both_sides_of_noon() {
        assert_eq!(
            readings("10"),
            [at(date(10, 21), 22, 0), at(date(10, 22), 10, 0)]
        );
        assert_eq!(
            readings("3"),
            [at(date(10, 21), 15, 0), at(date(10, 22), 3, 0)]
        );
        assert_eq!(
            readings("tomorrow 10"),
            [at(date(10, 22), 10, 0), at(date(10, 22), 22, 0)]
        );
        let morning = date(10, 21).and_time(time(8, 0));
        assert_eq!(
            read("10", morning).readings,
            [at(date(10, 21), 10, 0), at(date(10, 21), 22, 0)]
        );
        assert_eq!(readings("18"), [at(date(10, 21), 18, 0)]);
    }

    #[test]
    fn named_times_of_day() {
        assert_eq!(readings("morning"), [at(date(10, 22), 9, 0)]);
        assert_eq!(readings("noon"), [at(date(10, 22), 12, 0)]);
        assert_eq!(readings("afternoon"), [at(date(10, 21), 15, 0)]);
        assert_eq!(readings("evening"), [at(date(10, 21), 18, 0)]);
        assert_eq!(readings("tonight"), [at(date(10, 21), 20, 0)]);
        assert_eq!(readings("tomorrow morning"), [at(date(10, 22), 9, 0)]);
    }

    #[test]
    fn dates_with_times() {
        assert_eq!(readings("fri 9am"), [at(date(10, 23), 9, 0)]);
        assert_eq!(readings("tomorrow at 10:30"), [at(date(10, 22), 10, 30)]);
        assert_eq!(readings("dec 2 9pm"), [at(date(12, 2), 21, 0)]);
        assert_eq!(readings("in 3 days noon"), [at(date(10, 24), 12, 0)]);
    }

    #[test]
    fn already_past_only_when_the_day_was_named() {
        assert_eq!(readings("today 9am"), [past(at(date(10, 22), 9, 0))]);
        assert_eq!(readings("today morning"), [past(at(date(10, 22), 9, 0))]);
        assert_eq!(readings("9am"), [at(date(10, 22), 9, 0)]);
        assert_eq!(readings("today 17:00"), [at(date(10, 21), 17, 0)]);
        assert_eq!(
            readings("today 10"),
            [at(date(10, 21), 22, 0), past(at(date(10, 22), 10, 0))]
        );
        assert_eq!(readings("2026-10-01"), [past(on(date(10, 1)))]);
    }

    #[test]
    fn unknown_words_are_ignored_and_named() {
        let result = read("foo tomorrow Bar", now());
        assert_eq!(result.readings, [on(date(10, 22))]);
        assert_eq!(result.ignored, ["foo", "Bar"]);

        let result = read("tomorrow friday", now());
        assert_eq!(result.readings, [on(date(10, 22))]);
        assert_eq!(result.ignored, ["friday"]);

        let result = read("dentist", now());
        assert_eq!(result.readings, []);
        assert_eq!(result.ignored, ["dentist"]);

        assert_eq!(read("", now()), Readings::default());
        assert_eq!(read("13pm", now()).readings, []);
        assert_eq!(read("feb 30", now()).readings, []);
    }

    #[test]
    fn presets_for_an_empty_query() {
        let preset = |label, reading| Preset { label, reading };
        assert_eq!(
            presets(now(), time(9, 0)),
            [
                preset("Later today", at(date(10, 21), 17, 0)),
                preset("Tomorrow", at(date(10, 22), 9, 0)),
                preset("Monday", at(date(10, 26), 9, 0)),
            ]
        );
        let late = date(10, 21).and_time(time(16, 0));
        assert_eq!(
            presets(late, time(9, 0)),
            [
                preset("Tomorrow", at(date(10, 22), 9, 0)),
                preset("Monday", at(date(10, 26), 9, 0)),
            ]
        );
        let sunday = date(10, 25).and_time(time(18, 0));
        assert_eq!(
            presets(sunday, time(9, 0)),
            [preset("Tomorrow", at(date(10, 26), 9, 0))]
        );
        assert_eq!(
            presets(late, time(8, 30)),
            [
                preset("Tomorrow", at(date(10, 22), 8, 30)),
                preset("Monday", at(date(10, 26), 8, 30)),
            ]
        );
    }

    #[test]
    fn readings_past_the_last_date_are_dropped_not_panicked_on() {
        for query in [
            "in 99999999 days",
            "in 4294967295 weeks",
            "in 4294967295 hours",
        ] {
            assert_eq!(read(query, now()).readings, [], "{query:?}");
        }
        // The most minutes a query can name is about 8,000 years: still a date.
        assert_eq!(read("in 4294967295 min", now()).readings.len(), 1);
        let last = NaiveDate::MAX.and_time(time(23, 0));
        for query in ["tomorrow", "fri", "next mon", "next week", "22:00", "10"] {
            assert!(
                read(query, last)
                    .readings
                    .iter()
                    .all(|reading| reading.date == NaiveDate::MAX),
                "{query:?}"
            );
        }
        assert_eq!(presets(last, time(9, 0)), []);
    }
}
