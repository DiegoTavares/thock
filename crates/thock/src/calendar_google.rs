//! The Google Calendar REST provider (spec `v8-calendar-sync.md` §6, §10.2):
//! `calendarList.list` / `events.list` with conditional requests, and the
//! response → `CalendarEvent` normalization. Read-only toward Google. The
//! OAuth machinery lives in `google_auth.rs` since V9 unified it with Gmail.

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{DateTime, Local, NaiveDate, Timelike as _};
use futures::AsyncReadExt as _;
use gpui::{AsyncApp, Task};
use http_client::{AsyncBody, HttpClient, Request, http};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::calendar::{
    CalendarEvent, CalendarProvider, EventFilters, EventKind, Fetched, event_marker_id,
};
use crate::google_auth::{
    AuthRevoked, GoogleClient, Unauthorized, read_refresh_token_allowing_legacy,
    refresh_access_token, token_lifetime,
};

const API_BASE: &str = "https://www.googleapis.com/calendar/v3";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarListEntry {
    pub id: String,
    pub summary: String,
    pub primary: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CalendarListPage {
    items: Vec<CalendarListEntry>,
    next_page_token: Option<String>,
}

/// Every entry from `calendarList.list`, for the calendar picker (spec §6.3).
pub async fn list_calendars(
    http: &Arc<dyn HttpClient>,
    access_token: &str,
) -> Result<Vec<CalendarListEntry>> {
    let mut entries = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("minAccessRole", "reader");
        if let Some(token) = &page_token {
            query.append_pair("pageToken", token);
        }
        let url = format!("{API_BASE}/users/me/calendarList?{}", query.finish());
        let body = get_json(http, &url, access_token, None)
            .await?
            .context("calendarList.list unexpectedly returned 304")?
            .1;
        let page: CalendarListPage =
            serde_json::from_str(&body).context("failed to parse calendarList response")?;
        entries.extend(page.items);
        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => return Ok(entries),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GoogleEventTime {
    pub date: Option<NaiveDate>,
    pub date_time: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GoogleAttendee {
    #[serde(rename = "self")]
    pub is_self: bool,
    #[serde(rename = "responseStatus")]
    pub response_status: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GoogleOrganizer {
    #[serde(rename = "self")]
    pub is_self: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GoogleEvent {
    pub id: String,
    pub status: Option<String>,
    pub summary: Option<String>,
    pub start: GoogleEventTime,
    pub end: GoogleEventTime,
    pub attendees: Vec<GoogleAttendee>,
    pub organizer: Option<GoogleOrganizer>,
    /// `default`, `focusTime`, `outOfOffice`, `workingLocation`, … — absent
    /// on older API responses, which means `default`.
    pub event_type: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct EventsPage {
    etag: Option<String>,
    items: Vec<GoogleEvent>,
    next_page_token: Option<String>,
}

#[derive(Debug)]
pub enum DayFetch {
    /// The stored ETag still matches; nothing changed for this calendar.
    NotModified,
    Events {
        etag: Option<String>,
        events: Vec<GoogleEvent>,
    },
}

/// One day-bracketed `events.list` (spec §10.2): `singleEvents=true`, ordered
/// by start time, conditional on `etag`, following `nextPageToken`.
pub async fn fetch_day_events(
    http: &Arc<dyn HttpClient>,
    access_token: &str,
    calendar_id: &str,
    date: NaiveDate,
    etag: Option<&str>,
) -> Result<DayFetch> {
    let (time_min, time_max) = day_window(date)?;
    let mut events = Vec::new();
    let mut first_etag = None;
    let mut page_token: Option<String> = None;
    loop {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("timeMin", &time_min)
            .append_pair("timeMax", &time_max)
            .append_pair("singleEvents", "true")
            .append_pair("orderBy", "startTime");
        if let Some(token) = &page_token {
            query.append_pair("pageToken", token);
        }
        let url = format!(
            "{API_BASE}/calendars/{}/events?{}",
            url_path_escape(calendar_id),
            query.finish()
        );
        // Only the first page is conditional; follow-up pages are part of the
        // same changed result.
        let conditional = page_token.is_none().then_some(etag).flatten();
        let Some((header_etag, body)) = get_json(http, &url, access_token, conditional).await?
        else {
            return Ok(DayFetch::NotModified);
        };
        let page: EventsPage =
            serde_json::from_str(&body).context("failed to parse events.list response")?;
        if first_etag.is_none() {
            first_etag = header_etag.or(page.etag);
        }
        events.extend(page.items);
        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => {
                return Ok(DayFetch::Events {
                    etag: first_etag,
                    events,
                });
            }
        }
    }
}

/// `Ok(None)` on 304; otherwise the response ETag header and body.
async fn get_json(
    http: &Arc<dyn HttpClient>,
    url: &str,
    access_token: &str,
    if_none_match: Option<&str>,
) -> Result<Option<(Option<String>, String)>> {
    let mut builder = Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json");
    if let Some(etag) = if_none_match {
        builder = builder.header("If-None-Match", etag);
    }
    let request = builder.body(AsyncBody::default())?;
    let mut response = http.send(request).await?;
    if response.status() == http::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if response.status() == http::StatusCode::UNAUTHORIZED {
        return Err(anyhow!(Unauthorized));
    }
    let mut body = String::new();
    response.body_mut().read_to_string(&mut body).await?;
    if !response.status().is_success() {
        bail!(
            "Google API request failed with status {}: {body}",
            response.status()
        );
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    Ok(Some((etag, body)))
}

fn url_path_escape(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes()).collect()
}

/// The local day as RFC 3339 bounds: `[00:00 today, 00:00 tomorrow)`.
fn day_window(date: NaiveDate) -> Result<(String, String)> {
    let bound = |date: NaiveDate| -> Result<String> {
        // Some zones spring forward at midnight (e.g. Cuba), so 00:00 may not
        // exist; fall back to the first wall-clock hour that does.
        for hour in [0, 1, 2, 3] {
            let start = date
                .and_hms_opt(hour, 0, 0)
                .and_then(|naive| naive.and_local_timezone(Local).earliest());
            if let Some(start) = start {
                return Ok(start.to_rfc3339());
            }
        }
        Err(anyhow!("could not resolve a local start of day for {date}"))
    };
    Ok((
        bound(date)?,
        bound(date.succ_opt().context("date overflow")?)?,
    ))
}

/// Filters and normalizes raw events into [`CalendarEvent`]s (spec §7.2):
/// keeps events the user is going to, drops declined/unanswered invites and,
/// by default, all-day and untitled ones. Times are wall-clock local minutes
/// clamped to the day, matching the planner's clamp.
pub fn normalize_events(
    items: &[GoogleEvent],
    calendar_id: &str,
    date: NaiveDate,
    filters: &EventFilters,
) -> Vec<CalendarEvent> {
    let mut events = Vec::new();
    for item in items {
        if item.status.as_deref() == Some("cancelled") {
            continue;
        }
        if !attendance_passes(item, filters) {
            continue;
        }
        let title = item.summary.as_deref().map(str::trim).unwrap_or("");
        let title = if title.is_empty() {
            if !filters.private_busy {
                continue;
            }
            "(busy)".to_string()
        } else {
            title.to_string()
        };
        let Some(time) = event_day_minutes(item, date, filters) else {
            continue;
        };
        events.push(CalendarEvent {
            id: event_marker_id(calendar_id, &item.id),
            title,
            time: time.into_timed(),
            kind: item
                .event_type
                .as_deref()
                .map(EventKind::from_google)
                .unwrap_or_default(),
        });
    }
    events
}

enum DayMinutes {
    AllDay,
    Timed(u32, u32),
}

impl DayMinutes {
    fn into_timed(self) -> Option<(u32, u32)> {
        match self {
            Self::AllDay => None,
            Self::Timed(start, end) => Some((start, end)),
        }
    }
}

/// `None` when the event doesn't pass the timing filters or doesn't overlap
/// `date`. An event spanning midnight is clamped to the day window (V4 §5.5
/// precedent).
fn event_day_minutes(
    item: &GoogleEvent,
    date: NaiveDate,
    filters: &EventFilters,
) -> Option<DayMinutes> {
    if let (Some(start), Some(end)) = (&item.start.date_time, &item.end.date_time) {
        let start = DateTime::parse_from_rfc3339(start)
            .ok()?
            .with_timezone(&Local);
        let end = DateTime::parse_from_rfc3339(end)
            .ok()?
            .with_timezone(&Local);
        if start.date_naive() > date || end.date_naive() < date {
            return None;
        }
        let start_min = if start.date_naive() < date {
            0
        } else {
            start.hour() * 60 + start.minute()
        };
        let end_min = if end.date_naive() > date {
            crate::day_plan::MINUTES_PER_DAY
        } else {
            end.hour() * 60 + end.minute()
        };
        if end_min == 0 {
            // Ends exactly at this day's midnight: yesterday's event.
            return None;
        }
        if end_min < start_min {
            return None;
        }
        return Some(DayMinutes::Timed(start_min, end_min));
    }
    if let (Some(start), Some(end)) = (item.start.date, item.end.date) {
        // All-day: `end` is exclusive.
        if !filters.all_day || start > date || end <= date {
            return None;
        }
        return Some(DayMinutes::AllDay);
    }
    None
}

fn attendance_passes(item: &GoogleEvent, filters: &EventFilters) -> bool {
    match item.attendees.iter().find(|attendee| attendee.is_self) {
        Some(attendee) => match attendee.response_status.as_deref() {
            Some("declined") => false,
            Some("accepted") => true,
            // `tentative` and `needsAction` are dropped by default (§7.2).
            _ => !filters.accepted_only,
        },
        None => {
            if item
                .organizer
                .as_ref()
                .is_some_and(|organizer| organizer.is_self)
            {
                filters.include_solo
            } else {
                // An event on a subscribed calendar the user is not invited
                // to — they chose to sync that calendar.
                true
            }
        }
    }
}

/// [`CalendarProvider`] over the REST client: owns the access token and the
/// per-`(calendar, date)` ETag/event cache. The refresh token stays in the
/// keychain and is read on demand.
pub struct GoogleProvider {
    inner: Arc<ProviderInner>,
}

struct ProviderInner {
    http: Arc<dyn HttpClient>,
    client: GoogleClient,
    calendars: Vec<String>,
    filters: EventFilters,
    state: Mutex<ProviderState>,
}

#[derive(Default)]
struct ProviderState {
    access_token: Option<AccessToken>,
    refresh_token: Option<String>,
    /// Keyed on `(calendar_id, date)` — the ETag is per calendar *and* per
    /// query window; yesterday's entries are dropped on rollover.
    cache: HashMap<(String, NaiveDate), CachedCalendarDay>,
}

struct CachedCalendarDay {
    etag: Option<String>,
    events: Vec<CalendarEvent>,
}

#[derive(Clone)]
struct AccessToken {
    token: String,
    expires_at: Instant,
}

impl GoogleProvider {
    pub fn new(
        http: Arc<dyn HttpClient>,
        client: GoogleClient,
        calendars: Vec<String>,
        filters: EventFilters,
    ) -> Self {
        Self {
            inner: Arc::new(ProviderInner {
                http,
                client,
                calendars,
                filters,
                state: Mutex::new(ProviderState::default()),
            }),
        }
    }

    /// Seeds the access token minted during the connect flow, saving the
    /// first poll a refresh round-trip.
    pub fn seed_access_token(&self, token: String, expires_in: Option<u64>) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.access_token = Some(AccessToken {
                token,
                expires_at: Instant::now() + token_lifetime(expires_in),
            });
        }
    }
}

impl CalendarProvider for GoogleProvider {
    fn fetch_day(&self, date: NaiveDate, cx: &AsyncApp) -> Task<Result<Fetched>> {
        let inner = self.inner.clone();
        cx.spawn(async move |cx| inner.fetch_day(date, cx).await)
    }
}

impl ProviderInner {
    async fn fetch_day(self: &Arc<Self>, date: NaiveDate, cx: &mut AsyncApp) -> Result<Fetched> {
        let access_token = self.valid_access_token(cx).await?;
        match self.fetch_day_with_token(date, &access_token).await {
            Err(error) if error.is::<Unauthorized>() => {
                // The token aged out server-side: refresh once and retry.
                if let Ok(mut state) = self.state.lock() {
                    state.access_token = None;
                }
                let access_token = self.valid_access_token(cx).await?;
                match self.fetch_day_with_token(date, &access_token).await {
                    Err(error) if error.is::<Unauthorized>() => Err(anyhow!(AuthRevoked)),
                    other => other,
                }
            }
            other => other,
        }
    }

    async fn fetch_day_with_token(&self, date: NaiveDate, access_token: &str) -> Result<Fetched> {
        let mut all_events = Vec::new();
        let mut any_changed = false;
        for calendar_id in &self.calendars {
            let stored_etag = {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| anyhow!("provider state poisoned"))?;
                state
                    .cache
                    .get(&(calendar_id.clone(), date))
                    .and_then(|cached| cached.etag.clone())
            };
            let fetched = fetch_day_events(
                &self.http,
                access_token,
                calendar_id,
                date,
                stored_etag.as_deref(),
            )
            .await?;
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("provider state poisoned"))?;
            match fetched {
                DayFetch::NotModified => {
                    if let Some(cached) = state.cache.get(&(calendar_id.clone(), date)) {
                        all_events.extend(cached.events.iter().cloned());
                    }
                }
                DayFetch::Events { etag, events } => {
                    any_changed = true;
                    let normalized = normalize_events(&events, calendar_id, date, &self.filters);
                    all_events.extend(normalized.iter().cloned());
                    state.cache.insert(
                        (calendar_id.clone(), date),
                        CachedCalendarDay {
                            etag,
                            events: normalized,
                        },
                    );
                }
            }
            // Rollover cleanup: the window (and so the ETag) is per day.
            state
                .cache
                .retain(|(_, cached_date), _| *cached_date == date);
        }
        if any_changed {
            Ok(Fetched::Events(all_events))
        } else {
            Ok(Fetched::Unchanged)
        }
    }

    async fn valid_access_token(self: &Arc<Self>, cx: &mut AsyncApp) -> Result<String> {
        if let Ok(state) = self.state.lock()
            && let Some(access) = &state.access_token
            && access.expires_at > Instant::now()
        {
            return Ok(access.token.clone());
        }
        let refresh_token = match self
            .state
            .lock()
            .ok()
            .and_then(|state| state.refresh_token.clone())
        {
            Some(token) => token,
            None => {
                // Legacy fallback: a V8 calendar-only token keeps calendar
                // sync alive until the first workspace connect upgrades it.
                let (_, token) = read_refresh_token_allowing_legacy(cx)
                    .await?
                    .ok_or_else(|| anyhow!(AuthRevoked))?;
                if let Ok(mut state) = self.state.lock() {
                    state.refresh_token = Some(token.clone());
                }
                token
            }
        };
        let response = refresh_access_token(&self.http, &self.client, &refresh_token).await?;
        let token = response.access_token.clone();
        if let Ok(mut state) = self.state.lock() {
            state.access_token = Some(AccessToken {
                token: token.clone(),
                expires_at: Instant::now() + token_lifetime(response.expires_in),
            });
        }
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use http_client::{FakeHttpClient, Response};

    fn timed_event(id: &str, summary: &str, start: &str, end: &str) -> GoogleEvent {
        GoogleEvent {
            id: id.to_string(),
            status: Some("confirmed".to_string()),
            summary: Some(summary.to_string()),
            start: GoogleEventTime {
                date: None,
                date_time: Some(start.to_string()),
            },
            end: GoogleEventTime {
                date: None,
                date_time: Some(end.to_string()),
            },
            attendees: Vec::new(),
            organizer: Some(GoogleOrganizer { is_self: true }),
            event_type: None,
        }
    }

    fn attendee(is_self: bool, status: &str) -> GoogleAttendee {
        GoogleAttendee {
            is_self,
            response_status: Some(status.to_string()),
        }
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 18).unwrap()
    }

    /// Local wall-clock RFC 3339 for the test date, so the expectations are
    /// timezone-independent.
    fn local(hour: u32, minute: u32) -> String {
        date()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
            .and_local_timezone(Local)
            .earliest()
            .unwrap()
            .to_rfc3339()
    }

    #[test]
    fn normalization_filters_and_clamps() {
        let mut cancelled = timed_event("cancelled", "Gone", &local(9, 0), &local(10, 0));
        cancelled.status = Some("cancelled".to_string());
        let mut declined = timed_event("declined", "No thanks", &local(9, 0), &local(10, 0));
        declined.attendees = vec![attendee(true, "declined")];
        let mut unanswered = timed_event("unanswered", "Maybe", &local(9, 0), &local(10, 0));
        unanswered.attendees = vec![attendee(true, "needsAction")];
        let mut accepted = timed_event("accepted", "Standup", &local(9, 30), &local(10, 0));
        accepted.attendees = vec![attendee(false, "declined"), attendee(true, "accepted")];
        let mut untitled = timed_event("untitled", "", &local(11, 0), &local(12, 0));
        untitled.summary = None;
        let all_day_event = GoogleEvent {
            id: "allday".to_string(),
            summary: Some("Holiday".to_string()),
            start: GoogleEventTime {
                date: Some(date()),
                date_time: None,
            },
            end: GoogleEventTime {
                date: date().succ_opt(),
                date_time: None,
            },
            organizer: Some(GoogleOrganizer { is_self: true }),
            ..Default::default()
        };
        let spans_midnight = timed_event(
            "overnight",
            "Red-eye",
            &date()
                .pred_opt()
                .unwrap()
                .and_hms_opt(23, 0, 0)
                .unwrap()
                .and_local_timezone(Local)
                .earliest()
                .unwrap()
                .to_rfc3339(),
            &local(1, 30),
        );
        let items = vec![
            cancelled,
            declined,
            unanswered,
            accepted,
            untitled,
            all_day_event,
            spans_midnight,
        ];

        let events = normalize_events(&items, "primary", date(), &EventFilters::default());
        assert_eq!(
            events
                .iter()
                .map(|event| (event.title.as_str(), event.time))
                .collect::<Vec<_>>(),
            vec![("Standup", Some((570, 600))), ("Red-eye", Some((0, 90))),]
        );
        assert_eq!(events[0].id, event_marker_id("primary", "accepted"));

        // Loosened filters admit the optional categories.
        let filters = EventFilters {
            accepted_only: false,
            include_solo: true,
            all_day: true,
            private_busy: true,
        };
        let events = normalize_events(&items, "primary", date(), &filters);
        let titles: Vec<&str> = events.iter().map(|event| event.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Maybe", "Standup", "(busy)", "Holiday", "Red-eye"]
        );
        assert_eq!(
            events
                .iter()
                .find(|event| event.title == "Holiday")
                .unwrap()
                .time,
            None
        );
    }

    #[test]
    fn google_event_types_map_to_event_kinds() {
        let with_type = |event_type: Option<&str>| {
            let mut item = timed_event("id", "Focus time", &local(9, 0), &local(17, 0));
            item.event_type = event_type.map(str::to_string);
            normalize_events(&[item], "primary", date(), &EventFilters::default())
                .first()
                .map(|event| event.kind)
        };
        assert_eq!(with_type(Some("focusTime")), Some(EventKind::FocusTime));
        assert_eq!(with_type(Some("outOfOffice")), Some(EventKind::OutOfOffice));
        assert_eq!(with_type(Some("workingLocation")), Some(EventKind::Default));
        assert_eq!(with_type(Some("default")), Some(EventKind::Default));
        // Absent on older responses, and on anything Google adds later.
        assert_eq!(with_type(None), Some(EventKind::Default));
        assert_eq!(with_type(Some("somethingNew")), Some(EventKind::Default));
    }

    #[test]
    fn solo_events_are_gated_on_include_solo() {
        let solo = timed_event("solo", "Focus block", &local(9, 0), &local(10, 0));
        let filters = EventFilters {
            include_solo: false,
            ..EventFilters::default()
        };
        assert!(
            normalize_events(std::slice::from_ref(&solo), "primary", date(), &filters).is_empty()
        );
        assert_eq!(
            normalize_events(&[solo], "primary", date(), &EventFilters::default()).len(),
            1
        );

        // Not invited and not the organizer: a subscribed calendar's event.
        let mut foreign = timed_event("foreign", "Team offsite", &local(9, 0), &local(10, 0));
        foreign.organizer = Some(GoogleOrganizer { is_self: false });
        assert_eq!(
            normalize_events(&[foreign], "team", date(), &filters).len(),
            1
        );
    }

    #[test]
    fn fetch_day_events_follows_pagination_and_reports_etag() {
        let http = FakeHttpClient::create(|request| async move {
            let uri = request.uri().to_string();
            assert!(uri.contains("singleEvents=true"), "{uri}");
            assert!(uri.contains("orderBy=startTime"), "{uri}");
            let body = if uri.contains("pageToken=page2") {
                r#"{"items": [{"id": "b"}]}"#
            } else {
                r#"{"etag": "\"tag1\"", "items": [{"id": "a"}], "nextPageToken": "page2"}"#
            };
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(body.as_bytes().to_vec()))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let fetched = block_on(fetch_day_events(&http, "token", "primary", date(), None)).unwrap();
        match fetched {
            DayFetch::Events { etag, events } => {
                assert_eq!(etag.as_deref(), Some("\"tag1\""));
                assert_eq!(events.len(), 2);
                assert_eq!(events[1].id, "b");
            }
            DayFetch::NotModified => panic!("expected events"),
        }
    }

    #[test]
    fn fetch_day_events_honors_not_modified() {
        let http = FakeHttpClient::create(|request| async move {
            assert_eq!(
                request
                    .headers()
                    .get("If-None-Match")
                    .and_then(|value| value.to_str().ok()),
                Some("\"tag1\"")
            );
            Ok(Response::builder()
                .status(304)
                .body(AsyncBody::default())
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let fetched = block_on(fetch_day_events(
            &http,
            "token",
            "primary",
            date(),
            Some("\"tag1\""),
        ))
        .unwrap();
        assert!(matches!(fetched, DayFetch::NotModified));
    }

    #[test]
    fn unauthorized_is_typed() {
        let http = FakeHttpClient::create(|_| async move {
            Ok(Response::builder()
                .status(401)
                .body(AsyncBody::default())
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let error =
            block_on(fetch_day_events(&http, "token", "primary", date(), None)).unwrap_err();
        assert!(error.is::<Unauthorized>());
    }

    #[test]
    fn calendar_list_parses_entries() {
        let http = FakeHttpClient::create(|_| async move {
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(
                    br#"{"items": [
                        {"id": "diego@example.com", "summary": "Diego", "primary": true},
                        {"id": "team@group.calendar.google.com", "summary": "Team"}
                    ]}"#
                    .to_vec(),
                ))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let entries = block_on(list_calendars(&http, "token")).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].primary);
        assert_eq!(entries[1].summary, "Team");
    }
}
