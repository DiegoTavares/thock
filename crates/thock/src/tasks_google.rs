//! The Google Tasks REST source (spec `v13-inbox-routine.md` §7.2): list
//! resolution by title, task paging, and URL extraction. Read-only toward
//! Google — Thock never completes, deletes, or edits a task; the phone-side
//! list is the user's to clear.

use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, Local, NaiveDate};
use gpui::{AsyncApp, Task};
use http_client::HttpClient;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::google_auth::{AuthRevoked, GoogleClient, TokenKeeper, Unauthorized, api_get_json};
use crate::inbox::{CapturedItem, InboxFetched, InboxSource};

const API_BASE: &str = "https://tasks.googleapis.com/tasks/v1";

/// The alias Google resolves to the account's default list (`My Tasks`) —
/// where a mobile share sheet drops things with zero decisions, which is why
/// it is the default (spec §13 #3). Using the alias skips `lists.list`
/// entirely when no list is configured.
const DEFAULT_LIST_ID: &str = "@default";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct TaskList {
    id: String,
    title: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TaskListsPage {
    items: Vec<TaskList>,
    next_page_token: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct TaskLink {
    link: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct GoogleTask {
    id: String,
    title: String,
    status: String,
    notes: Option<String>,
    due: Option<String>,
    updated: Option<String>,
    links: Vec<TaskLink>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TasksPage {
    items: Vec<GoogleTask>,
    next_page_token: Option<String>,
}

/// Every task list in the account, following `nextPageToken`.
async fn list_task_lists(http: &Arc<dyn HttpClient>, access_token: &str) -> Result<Vec<TaskList>> {
    let mut lists = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("maxResults", "100");
        if let Some(token) = &page_token {
            query.append_pair("pageToken", token);
        }
        let url = format!("{API_BASE}/users/@me/lists?{}", query.finish());
        let body = api_get_json(http, &url, access_token, "Google Tasks lists").await?;
        let page: TaskListsPage =
            serde_json::from_str(&body).context("failed to parse Google Tasks lists response")?;
        lists.extend(page.items);
        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => return Ok(lists),
        }
    }
}

/// Every open task in the list, following `nextPageToken`. No `updatedMin`,
/// no incremental machinery (spec §7.2): an uncompleted list is small, and
/// dedup is by id.
async fn list_open_tasks(
    http: &Arc<dyn HttpClient>,
    access_token: &str,
    list_id: &str,
) -> Result<Vec<GoogleTask>> {
    let mut tasks = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("showCompleted", "false")
            .append_pair("showHidden", "false")
            .append_pair("maxResults", "100");
        if let Some(token) = &page_token {
            query.append_pair("pageToken", token);
        }
        let url = format!(
            "{API_BASE}/lists/{}/tasks?{}",
            url_path_escape(list_id),
            query.finish()
        );
        let body = api_get_json(http, &url, access_token, "Google Tasks").await?;
        let page: TasksPage =
            serde_json::from_str(&body).context("failed to parse Google Tasks response")?;
        tasks.extend(page.items);
        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => return Ok(tasks),
        }
    }
}

fn url_path_escape(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes()).collect()
}

/// The first http(s) URL in `text`, as a whitespace-delimited token.
fn first_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| token.starts_with("https://") || token.starts_with("http://"))
        .map(str::to_string)
}

/// `links[0].link`, else the first URL in the notes, else in the title
/// (spec §7.2). `links` is documented read-only and frequently absent — the
/// URL usually arrives inside `notes`.
fn task_url(task: &GoogleTask) -> Option<String> {
    task.links
        .first()
        .map(|link| link.link.clone())
        .filter(|link| !link.is_empty())
        .or_else(|| task.notes.as_deref().and_then(first_url))
        .or_else(|| first_url(&task.title))
}

/// Google Tasks `due` is RFC 3339 but its time component is meaningless
/// (spec §11): parse the date, discard the rest, or every due date drifts by
/// a timezone.
fn task_due(due: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(due.get(..10)?, "%Y-%m-%d").ok()
}

fn captured_item(task: &GoogleTask) -> CapturedItem {
    CapturedItem {
        source: "google-tasks",
        external_id: task.id.clone(),
        title: task.title.clone(),
        from: None,
        url: task_url(task),
        link: None,
        body: task.notes.clone().filter(|notes| !notes.trim().is_empty()),
        occurred_at: task
            .updated
            .as_deref()
            .and_then(|updated| DateTime::parse_from_rfc3339(updated).ok())
            .map(|updated| updated.with_timezone(&Local).fixed_offset()),
        due: task.due.as_deref().and_then(task_due),
    }
}

/// [`InboxSource`] over the REST client: one configured list, defaulting to
/// the account's default list. Only `needsAction` tasks are captured, so
/// completing a task on the phone *before* Thock polls means "never mind"
/// (spec §7.2).
pub struct GoogleTasksSource {
    inner: Arc<TasksInner>,
}

struct TasksInner {
    http: Arc<dyn HttpClient>,
    keeper: TokenKeeper,
    /// The configured list title; `None` means `@default`.
    list_title: Option<String>,
    list_id: Mutex<Option<String>>,
}

impl GoogleTasksSource {
    pub fn new(
        http: Arc<dyn HttpClient>,
        client: GoogleClient,
        list_title: Option<String>,
    ) -> Self {
        Self {
            inner: Arc::new(TasksInner {
                http,
                keeper: TokenKeeper::new(client),
                list_title,
                list_id: Mutex::new(None),
            }),
        }
    }
}

impl InboxSource for GoogleTasksSource {
    fn id(&self) -> &'static str {
        "google-tasks"
    }

    fn fetch(&self, _skip: &HashSet<String>, cx: &AsyncApp) -> Task<Result<InboxFetched>> {
        let inner = self.inner.clone();
        cx.spawn(async move |cx| {
            let token = inner.keeper.valid_access_token(&inner.http, cx).await?;
            match inner.fetch_with_token(&token).await {
                Err(error) if error.is::<Unauthorized>() => {
                    // The token aged out server-side: refresh once and retry.
                    inner.keeper.invalidate_access_token();
                    let token = inner.keeper.valid_access_token(&inner.http, cx).await?;
                    match inner.fetch_with_token(&token).await {
                        Err(error) if error.is::<Unauthorized>() => Err(anyhow!(AuthRevoked)),
                        other => other,
                    }
                }
                other => other,
            }
        })
    }
}

impl TasksInner {
    async fn fetch_with_token(&self, access_token: &str) -> Result<InboxFetched> {
        let Some(list_id) = self.resolve_list_id(access_token).await? else {
            return Ok(InboxFetched::Holding(format!(
                "list \"{}\" not found",
                self.list_title.as_deref().unwrap_or_default()
            )));
        };
        let tasks = match list_open_tasks(&self.http, access_token, &list_id).await {
            Ok(tasks) => tasks,
            Err(error) => {
                // The cached list may have been deleted; re-resolve next poll.
                if let Ok(mut cached) = self.list_id.lock() {
                    *cached = None;
                }
                return Err(error);
            }
        };
        Ok(InboxFetched::Items(
            tasks
                .iter()
                .filter(|task| task.status == "needsAction")
                .map(captured_item)
                .collect(),
        ))
    }

    /// The configured list resolved by title (case-insensitively), or the
    /// `@default` alias when none is configured. A configured list that
    /// doesn't exist is a holding state, not an error — and only after
    /// `lists.list` succeeded.
    async fn resolve_list_id(&self, access_token: &str) -> Result<Option<String>> {
        let Some(title) = &self.list_title else {
            return Ok(Some(DEFAULT_LIST_ID.to_string()));
        };
        if let Ok(cached) = self.list_id.lock()
            && let Some(id) = cached.clone()
        {
            return Ok(Some(id));
        }
        let lists = list_task_lists(&self.http, access_token).await?;
        let id = lists
            .into_iter()
            .find(|list| list.title.eq_ignore_ascii_case(title))
            .map(|list| list.id);
        if let (Ok(mut cached), Some(id)) = (self.list_id.lock(), &id) {
            *cached = Some(id.clone());
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use http_client::{AsyncBody, FakeHttpClient, Response};

    fn inner(list_title: Option<&str>, http: Arc<dyn HttpClient>) -> TasksInner {
        TasksInner {
            http,
            keeper: TokenKeeper::new(GoogleClient {
                client_id: "id".to_string(),
                client_secret: None,
            }),
            list_title: list_title.map(str::to_string),
            list_id: Mutex::new(None),
        }
    }

    fn items(fetched: InboxFetched) -> Vec<CapturedItem> {
        match fetched {
            InboxFetched::Items(items) => items,
            InboxFetched::Holding(reason) => panic!("unexpected holding: {reason}"),
        }
    }

    #[test]
    fn default_list_skips_list_resolution_and_maps_fields() {
        let http = FakeHttpClient::create(|request| async move {
            let uri = request.uri().to_string();
            // No /users/@me/lists call: the @default alias goes straight to
            // the tasks collection.
            assert!(uri.contains("/lists/%40default/tasks"), "{uri}");
            assert!(uri.contains("showCompleted=false"), "{uri}");
            assert!(uri.contains("showHidden=false"), "{uri}");
            let body = r#"{"items": [
                {"id": "t1", "title": "Ship it", "status": "needsAction",
                 "notes": "worth a read\nhttps://example.com/ship-it end",
                 "due": "2026-08-27T00:00:00.000Z",
                 "updated": "2026-08-23T18:22:07.000Z"},
                {"id": "t2", "title": "Done already", "status": "completed"},
                {"id": "t3", "title": "Linked", "status": "needsAction",
                 "links": [{"type": "email", "link": "https://linked.example.com"}]}
            ]}"#;
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(body.as_bytes().to_vec()))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let fetched = block_on(inner(None, http).fetch_with_token("token")).unwrap();
        let items = items(fetched);
        // The completed task is "never mind" — never captured.
        assert_eq!(items.len(), 2);
        let ship = &items[0];
        assert_eq!(ship.source, "google-tasks");
        assert_eq!(ship.external_id, "t1");
        assert_eq!(ship.title, "Ship it");
        // No links entry, so the URL came out of the notes.
        assert_eq!(ship.url.as_deref(), Some("https://example.com/ship-it"));
        assert_eq!(ship.link, None);
        assert_eq!(
            ship.body.as_deref(),
            Some("worth a read\nhttps://example.com/ship-it end")
        );
        // Date parsed, meaningless time component discarded.
        assert_eq!(ship.due, NaiveDate::from_ymd_opt(2026, 8, 27));
        assert!(ship.occurred_at.is_some());
        // A real links[0] wins over notes and title.
        assert_eq!(items[1].url.as_deref(), Some("https://linked.example.com"));
    }

    #[test]
    fn configured_list_resolves_by_title_and_paginates() {
        let http = FakeHttpClient::create(|request| async move {
            let uri = request.uri().to_string();
            let body = if uri.contains("/users/@me/lists") {
                r#"{"items": [
                    {"id": "list-a", "title": "My Tasks"},
                    {"id": "list-b", "title": "Thock"}
                ]}"#
                .to_string()
            } else if uri.contains("/lists/list-b/tasks") && !uri.contains("pageToken") {
                r#"{"items": [{"id": "t1", "title": "One", "status": "needsAction"}],
                    "nextPageToken": "page-2"}"#
                    .to_string()
            } else if uri.contains("pageToken=page-2") {
                r#"{"items": [{"id": "t2", "title": "Two", "status": "needsAction"}]}"#.to_string()
            } else {
                panic!("unexpected request to {uri}");
            };
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(body.into_bytes()))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let fetched = block_on(inner(Some("thock"), http).fetch_with_token("token")).unwrap();
        let items = items(fetched);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].external_id, "t1");
        assert_eq!(items[1].external_id, "t2");
    }

    #[test]
    fn missing_list_is_a_holding_state_not_an_error() {
        let http = FakeHttpClient::create(|request| async move {
            let uri = request.uri().to_string();
            assert!(
                uri.contains("/users/@me/lists"),
                "unexpected request to {uri}"
            );
            Ok(Response::builder()
                .status(200)
                .body(AsyncBody::from(br#"{"items": []}"#.to_vec()))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let fetched = block_on(inner(Some("My Tasks"), http).fetch_with_token("token")).unwrap();
        assert_eq!(
            fetched,
            InboxFetched::Holding("list \"My Tasks\" not found".to_string())
        );
    }

    #[test]
    fn insufficient_scope_is_auth_revoked() {
        let http = FakeHttpClient::create(|_| async move {
            Ok(Response::builder()
                .status(403)
                .body(AsyncBody::from(
                    br#"{"error": {"status": "PERMISSION_DENIED",
                        "message": "Request had insufficient authentication scopes."}}"#
                        .to_vec(),
                ))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let error = block_on(inner(None, http).fetch_with_token("token")).unwrap_err();
        assert!(error.is::<AuthRevoked>());
    }

    #[test]
    fn url_extraction_order_and_due_parsing() {
        let task = GoogleTask {
            title: "see https://title.example.com".to_string(),
            notes: Some("no url here".to_string()),
            ..Default::default()
        };
        assert_eq!(
            task_url(&task).as_deref(),
            Some("https://title.example.com")
        );
        assert_eq!(
            task_due("2026-08-27T00:00:00.000Z"),
            NaiveDate::from_ymd_opt(2026, 8, 27)
        );
        assert_eq!(task_due("junk"), None);
    }
}
