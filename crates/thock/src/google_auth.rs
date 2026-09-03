//! The shared Google OAuth core (spec `v9-gmail-backlog-capture.md` §6):
//! PKCE installed-app authorization, token exchange and refresh, and
//! refresh-token storage in the system keychain. One consent grants every
//! Google scope Thock uses (Calendar + Gmail, both read-only); the calendar
//! and mail API clients live in `calendar_google.rs` / `gmail_google.rs`.

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use futures::AsyncReadExt as _;
use gpui::AsyncApp;
use http_client::{AsyncBody, HttpClient, Request, http};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::calendar::GoogleClientOverride;
use crate::calendar_google::{CalendarListEntry, list_calendars};
use crate::vault::VAULT_MARKER_DIR;

/// `.thock/google.toml` (V13 §7.4): what belongs to the *connection* rather
/// than to any feature — the account, and the optional `[google]` client
/// override. Written by the connect flow; the per-feature files keep only
/// their own keys.
pub const GOOGLE_CONFIG_FILE: &str = "google.toml";

/// Keychain slot for the unified refresh token: username is the account
/// email, password is the token. No token ever touches the vault.
pub const KEYCHAIN_URL: &str = "https://thock.local/google";

/// Where V8 builds stored the calendar-only token. Honored read-only for
/// calendar sync until the first workspace connect upgrades it (spec §6.2);
/// Gmail never accepts it because it lacks the Gmail scope.
const LEGACY_CALENDAR_KEYCHAIN_URL: &str = "https://thock.local/calendar/google";

/// Everything one consent grants (V9 §6.1; V13 §7.3 adds Tasks) — one
/// screen, three read-only scopes, one refresh token.
pub const SCOPES: [&str; 3] = [
    "https://www.googleapis.com/auth/calendar.readonly",
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/tasks.readonly",
];

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// The bundled Google *Desktop app* OAuth client, baked in at build time.
/// Google's own guidance is that a desktop client secret is not confidential,
/// which is why PKCE is mandatory here rather than optional. `[google]` in
/// `.thock/google.toml` (or, for pre-V13 vaults, a feature's own config
/// file) overrides the pair.
const BUNDLED_CLIENT_ID: Option<&str> = option_env!("THOCK_GOOGLE_CLIENT_ID");
const BUNDLED_CLIENT_SECRET: Option<&str> = option_env!("THOCK_GOOGLE_CLIENT_SECRET");

/// The user's grant is gone (`invalid_grant`, a 401 that survives a token
/// refresh, or a token without the needed scope): the owning service must
/// move to `Disconnected` instead of retrying.
#[derive(Debug)]
pub struct AuthRevoked;

impl std::fmt::Display for AuthRevoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Google sign-in expired or was revoked")
    }
}

impl std::error::Error for AuthRevoked {}

/// A plain 401 from a Google API: retried once behind a token refresh before
/// it escalates to [`AuthRevoked`].
#[derive(Debug)]
pub(crate) struct Unauthorized;

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Google rejected the access token")
    }
}

impl std::error::Error for Unauthorized {}

#[derive(Debug, Clone)]
pub struct GoogleClient {
    pub client_id: String,
    pub client_secret: Option<String>,
}

impl GoogleClient {
    /// The bundled desktop client with any `[google]` override applied.
    pub fn resolve(overrides: &GoogleClientOverride) -> Result<Self> {
        let client_id = overrides
            .client_id
            .clone()
            .or_else(|| BUNDLED_CLIENT_ID.map(str::to_string))
            .context(
                "no Google OAuth client is available — add [google] client_id \
                 to .thock/google.toml",
            )?;
        let client_secret = overrides
            .client_secret
            .clone()
            .or_else(|| BUNDLED_CLIENT_SECRET.map(str::to_string));
        Ok(Self {
            client_id,
            client_secret,
        })
    }
}

/// The connection-level settings a Google-backed service resolves before it
/// can poll: the account and any `[google]` client override.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoogleSettings {
    pub account: Option<String>,
    pub google: GoogleClientOverride,
}

/// The account/override keys of any `.thock/*.toml`, read leniently: every
/// other key is some feature's business.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GoogleSettingsContent {
    account: Option<String>,
    google: GoogleOverrideContent,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GoogleOverrideContent {
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// Resolves the account and client override for a service whose own config is
/// `.thock/<own_file>` (V13 §7.4): `google.toml` first, then the service's
/// own file, then the other Google config files in a fixed order. That last
/// step is the migration — a pre-V13 vault with `account` in `calendar.toml`
/// keeps working untouched. Legacy keys are read here, never rewritten and
/// never deleted. Blocking I/O, same as the services' config reloads.
pub fn resolve_google_settings(vault_root: &Path, own_file: &str) -> GoogleSettings {
    let mut resolved = GoogleSettings::default();
    let mut order = vec![GOOGLE_CONFIG_FILE, own_file];
    for other in [
        crate::calendar::CALENDAR_CONFIG_FILE,
        crate::gmail::GMAIL_CONFIG_FILE,
    ] {
        if other != own_file {
            order.push(other);
        }
    }
    for file_name in order {
        let path = vault_root.join(VAULT_MARKER_DIR).join(file_name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(content) = toml::from_str::<GoogleSettingsContent>(&text) else {
            // The owning service already warns about its own unparseable
            // file; resolution just moves on.
            continue;
        };
        if resolved.account.is_none() {
            resolved.account = content.account.filter(|account| !account.trim().is_empty());
        }
        // The override resolves as a pair, so an id and a secret can never
        // come from two different files.
        if resolved.google.client_id.is_none() && content.google.client_id.is_some() {
            resolved.google = GoogleClientOverride {
                client_id: content.google.client_id,
                client_secret: content.google.client_secret,
            };
        }
        if resolved.account.is_some() && resolved.google.client_id.is_some() {
            break;
        }
    }
    resolved
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// The outcome of the workspace connect flow (spec §6.1): the account is
/// identified by the primary calendar's id, which for Google is the account
/// email; the calendar list feeds the picker.
pub struct Connected {
    pub email: String,
    pub access_token: String,
    pub calendars: Vec<CalendarListEntry>,
}

/// Runs the full OAuth 2.0 installed-app flow for every Thock scope at once:
/// loopback listener, system browser, PKCE code exchange, then
/// `calendarList.list` to identify the account. Stores the refresh token in
/// the unified keychain slot (retiring the legacy calendar-only one) before
/// returning.
pub async fn connect_workspace(
    http: Arc<dyn HttpClient>,
    client: GoogleClient,
    cx: &mut AsyncApp,
) -> Result<Connected> {
    let verifier =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>());

    let (redirect_uri, callback) = oauth_callback_server::start_oauth_callback_server()?;
    let auth_url = build_auth_url(&client.client_id, &redirect_uri, &challenge, &state);
    cx.update(|cx| cx.open_url(&auth_url));

    let params = callback
        .await
        .context("the sign-in window was closed before authorization completed")??;
    if params.state != state {
        bail!("OAuth state mismatch — rejecting the callback");
    }

    let tokens = exchange_code(&http, &client, &params.code, &verifier, &redirect_uri).await?;
    let refresh_token = tokens
        .refresh_token
        .context("Google did not return a refresh token")?;
    let calendars = list_calendars(&http, &tokens.access_token).await?;
    let email = calendars
        .iter()
        .find(|entry| entry.primary)
        .map(|entry| entry.id.clone())
        .context("no primary calendar in the account's calendar list")?;

    write_refresh_token(&email, &refresh_token, cx).await?;
    Ok(Connected {
        email,
        access_token: tokens.access_token,
        calendars,
    })
}

fn build_auth_url(client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &SCOPES.join(" "))
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .finish();
    format!("{AUTH_ENDPOINT}?{query}")
}

async fn exchange_code(
    http: &Arc<dyn HttpClient>,
    client: &GoogleClient,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    let mut params = vec![
        ("client_id", client.client_id.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ];
    if let Some(secret) = &client.client_secret {
        params.push(("client_secret", secret));
    }
    post_token_request(http, &params).await
}

/// Mints a fresh access token from the stored refresh token. `invalid_grant`
/// means the user revoked access — surfaced as [`AuthRevoked`].
pub async fn refresh_access_token(
    http: &Arc<dyn HttpClient>,
    client: &GoogleClient,
    refresh_token: &str,
) -> Result<TokenResponse> {
    let mut params = vec![
        ("client_id", client.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    if let Some(secret) = &client.client_secret {
        params.push(("client_secret", secret));
    }
    post_token_request(http, &params).await
}

async fn post_token_request(
    http: &Arc<dyn HttpClient>,
    params: &[(&str, &str)],
) -> Result<TokenResponse> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params.iter().copied())
        .finish();
    let request = Request::builder()
        .method(http::Method::POST)
        .uri(TOKEN_ENDPOINT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(AsyncBody::from(body.into_bytes()))?;
    let mut response = http.send(request).await?;
    let mut body = String::new();
    response.body_mut().read_to_string(&mut body).await?;
    if !response.status().is_success() {
        if body.contains("invalid_grant") {
            return Err(anyhow!(AuthRevoked));
        }
        bail!(
            "Google token request failed with status {}: {body}",
            response.status()
        );
    }
    serde_json::from_str(&body).context("failed to parse Google token response")
}

/// Authorized GET returning the response body. 401 is typed as
/// [`Unauthorized`] for the retry-once-behind-refresh path; a 403 for a token
/// without the needed scope (a pre-V13 grant, say) is [`AuthRevoked`], so it
/// degrades to a reconnect affordance instead of an endless retry.
pub(crate) async fn api_get_json(
    http: &Arc<dyn HttpClient>,
    url: &str,
    access_token: &str,
    what: &str,
) -> Result<String> {
    let request = Request::builder()
        .method(http::Method::GET)
        .uri(url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .body(AsyncBody::default())?;
    let mut response = http.send(request).await?;
    if response.status() == http::StatusCode::UNAUTHORIZED {
        return Err(anyhow!(Unauthorized));
    }
    let mut body = String::new();
    response.body_mut().read_to_string(&mut body).await?;
    if response.status() == http::StatusCode::FORBIDDEN
        && body.to_lowercase().contains("insufficient")
    {
        return Err(anyhow!(AuthRevoked));
    }
    if !response.status().is_success() {
        bail!(
            "{what} request failed with status {}: {body}",
            response.status()
        );
    }
    Ok(body)
}

pub(crate) fn token_lifetime(expires_in: Option<u64>) -> Duration {
    // A one-minute safety margin so a token is never used mid-expiry.
    Duration::from_secs(expires_in.unwrap_or(3600).saturating_sub(60).max(60))
}

/// `(email, refresh token)` from the unified keychain slot, or `None` when
/// nothing is stored. Gmail uses this: a legacy calendar-only token lacks the
/// Gmail scope, so falling back would only trade a clear "connect" affordance
/// for a confusing 403.
pub async fn read_refresh_token(cx: &AsyncApp) -> Result<Option<(String, String)>> {
    read_from(KEYCHAIN_URL, cx).await
}

/// The unified token, falling back to the legacy calendar-only slot so an
/// already-connected calendar keeps syncing across the upgrade (spec §6.2).
pub async fn read_refresh_token_allowing_legacy(cx: &AsyncApp) -> Result<Option<(String, String)>> {
    if let Some(credentials) = read_from(KEYCHAIN_URL, cx).await? {
        return Ok(Some(credentials));
    }
    read_from(LEGACY_CALENDAR_KEYCHAIN_URL, cx).await
}

async fn read_from(keychain_url: &str, cx: &AsyncApp) -> Result<Option<(String, String)>> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    let Some((email, token)) = provider.read_credentials(keychain_url, cx).await? else {
        return Ok(None);
    };
    let token = String::from_utf8(token).context("stored refresh token is not UTF-8")?;
    Ok(Some((email, token)))
}

/// Stores the unified refresh token and retires the legacy calendar-only
/// entry, whose failure to delete (it may simply not exist) is only logged.
pub async fn write_refresh_token(email: &str, token: &str, cx: &AsyncApp) -> Result<()> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    provider
        .write_credentials(KEYCHAIN_URL, email, token.as_bytes(), cx)
        .await?;
    if let Err(error) = provider
        .delete_credentials(LEGACY_CALENDAR_KEYCHAIN_URL, cx)
        .await
    {
        log::debug!("Thock: no legacy Google credential to retire: {error:#}");
    }
    Ok(())
}

/// Deletes both the unified and the legacy keychain entries.
pub async fn delete_refresh_token(cx: &AsyncApp) -> Result<()> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    let unified = provider.delete_credentials(KEYCHAIN_URL, cx).await;
    let legacy = provider
        .delete_credentials(LEGACY_CALENDAR_KEYCHAIN_URL, cx)
        .await;
    // Deleting an entry that was never written may error; only fail when
    // neither slot could be cleared.
    if unified.is_err() && legacy.is_err() {
        return unified;
    }
    Ok(())
}

/// Lazily-refreshed access token over the unified keychain slot, for API
/// clients that don't need the calendar provider's ETag machinery. Missing
/// credentials surface as [`AuthRevoked`].
pub struct TokenKeeper {
    client: GoogleClient,
    state: Mutex<KeeperState>,
}

#[derive(Default)]
struct KeeperState {
    access_token: Option<(String, Instant)>,
    refresh_token: Option<String>,
}

impl TokenKeeper {
    pub fn new(client: GoogleClient) -> Self {
        Self {
            client,
            state: Mutex::new(KeeperState::default()),
        }
    }

    /// Drops the cached access token so the next call re-mints one — the
    /// retry-once-behind-refresh path after a 401.
    pub fn invalidate_access_token(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.access_token = None;
        }
    }

    pub async fn valid_access_token(
        &self,
        http: &Arc<dyn HttpClient>,
        cx: &mut AsyncApp,
    ) -> Result<String> {
        if let Ok(state) = self.state.lock()
            && let Some((token, expires_at)) = &state.access_token
            && *expires_at > Instant::now()
        {
            return Ok(token.clone());
        }
        let refresh_token = match self
            .state
            .lock()
            .ok()
            .and_then(|state| state.refresh_token.clone())
        {
            Some(token) => token,
            None => {
                let (_, token) = read_refresh_token(cx)
                    .await?
                    .ok_or_else(|| anyhow!(AuthRevoked))?;
                if let Ok(mut state) = self.state.lock() {
                    state.refresh_token = Some(token.clone());
                }
                token
            }
        };
        let response = refresh_access_token(http, &self.client, &refresh_token).await?;
        let token = response.access_token.clone();
        if let Ok(mut state) = self.state.lock() {
            state.access_token = Some((
                token.clone(),
                Instant::now() + token_lifetime(response.expires_in),
            ));
        }
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use http_client::{FakeHttpClient, Response};

    #[test]
    fn auth_url_carries_both_scopes_pkce_and_loopback_redirect() {
        let url = build_auth_url(
            "client-123",
            "http://127.0.0.1:9000/callback",
            "chal",
            "nonce",
        );
        assert!(url.starts_with(AUTH_ENDPOINT));
        for needle in [
            "client_id=client-123",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A9000%2Fcallback",
            "calendar.readonly+https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.readonly\
             +https%3A%2F%2Fwww.googleapis.com%2Fauth%2Ftasks.readonly",
            "code_challenge=chal",
            "code_challenge_method=S256",
            "access_type=offline",
            "prompt=consent",
            "state=nonce",
        ] {
            assert!(url.contains(needle), "missing {needle} in {url}");
        }
    }

    /// The §7.4 migration matrix: `google.toml` only, legacy only, both,
    /// neither, and conflicting values — resolved identically for every
    /// service.
    #[test]
    fn google_settings_resolution_matrix() {
        let write = |root: &Path, file: &str, text: &str| {
            let dir = root.join(VAULT_MARKER_DIR);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(file), text).unwrap();
        };

        // Neither file: nothing resolves.
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_google_settings(dir.path(), "calendar.toml");
        assert_eq!(resolved, GoogleSettings::default());

        // google.toml only: both services see the same account.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "google.toml",
            "schema = 1\naccount = \"unified@example.com\"\n\n[google]\nclient_id = \"id-g\"\n",
        );
        for own in ["calendar.toml", "gmail.toml"] {
            let resolved = resolve_google_settings(dir.path(), own);
            assert_eq!(resolved.account.as_deref(), Some("unified@example.com"));
            assert_eq!(resolved.google.client_id.as_deref(), Some("id-g"));
        }

        // Legacy only: the account in calendar.toml keeps working untouched,
        // for the calendar service and (via the fixed cross-file order) for
        // gmail too.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "calendar.toml",
            "account = \"legacy@example.com\"\n",
        );
        for own in ["calendar.toml", "gmail.toml"] {
            let resolved = resolve_google_settings(dir.path(), own);
            assert_eq!(resolved.account.as_deref(), Some("legacy@example.com"));
        }

        // Both, conflicting: google.toml wins and the stale legacy key is
        // inert; the account and the override may still come from different
        // files, but an override id/secret pair never splits across two.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "google.toml",
            "account = \"unified@example.com\"\n",
        );
        write(
            dir.path(),
            "calendar.toml",
            "account = \"legacy@example.com\"\n\n[google]\nclient_id = \"id-c\"\nclient_secret = \"s-c\"\n",
        );
        write(
            dir.path(),
            "gmail.toml",
            "account = \"other@example.com\"\n\n[google]\nclient_id = \"id-m\"\n",
        );
        let resolved = resolve_google_settings(dir.path(), "gmail.toml");
        assert_eq!(resolved.account.as_deref(), Some("unified@example.com"));
        // gmail's own file outranks calendar.toml for the override.
        assert_eq!(resolved.google.client_id.as_deref(), Some("id-m"));
        assert_eq!(resolved.google.client_secret, None);
        let resolved = resolve_google_settings(dir.path(), "calendar.toml");
        assert_eq!(resolved.google.client_id.as_deref(), Some("id-c"));
        assert_eq!(resolved.google.client_secret.as_deref(), Some("s-c"));

        // An unparseable file is skipped, never fatal.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "google.toml", "not [valid");
        write(
            dir.path(),
            "gmail.toml",
            "account = \"fallback@example.com\"\n",
        );
        let resolved = resolve_google_settings(dir.path(), "gmail.toml");
        assert_eq!(resolved.account.as_deref(), Some("fallback@example.com"));
    }

    #[test]
    fn invalid_grant_is_typed_as_auth_revoked() {
        let http = FakeHttpClient::create(|_| async move {
            Ok(Response::builder()
                .status(400)
                .body(AsyncBody::from(br#"{"error": "invalid_grant"}"#.to_vec()))
                .unwrap())
        });
        let http: Arc<dyn HttpClient> = http;
        let client = GoogleClient {
            client_id: "id".to_string(),
            client_secret: None,
        };
        let error = block_on(refresh_access_token(&http, &client, "stale")).unwrap_err();
        assert!(error.is::<AuthRevoked>());
    }
}
