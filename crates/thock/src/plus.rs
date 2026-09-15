//! Thock Plus (V25 Stage 1): the app side of the hosted tier. Talks to the
//! Thock Plus backend with the credential an invite code earns, keeps that
//! credential in the system keychain, and reads the entitlement the hosted
//! agent runs on: plan, balance, the gateway key, and the model behind each
//! tier. Nothing here is ever shown to the user as a key or a model name.

use anyhow::{Context as _, Result, anyhow};
use futures::AsyncReadExt as _;
use gpui::AsyncApp;
use http_client::{AsyncBody, HttpClient, Request, http};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agent::ModelTier;

pub const DEFAULT_BACKEND_URL: &str = "https://plus.thethock.com";
const KEYCHAIN_URL: &str = "https://plus.thethock.com/credential";
const KEYCHAIN_USERNAME: &str = "thock-plus";

/// Where the backend lives: `THOCK_PLUS_URL` for local runs, then the
/// user-level `[plus] url` setting, then production.
pub fn backend_url() -> String {
    let configured = std::env::var("THOCK_PLUS_URL")
        .ok()
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .or_else(|| crate::agent::load_global_field("plus", "url"))
        .unwrap_or_else(|| DEFAULT_BACKEND_URL.to_string());
    configured.trim_end_matches('/').to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntitlementStatus {
    Active,
    Exhausted,
    Revoked,
    #[serde(other)]
    Unknown,
}

/// The abstract tiers, mapped by the backend's plan config to real model
/// ids. The app only ever forwards these to the harness.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ModelTiers {
    pub default: String,
    #[serde(default)]
    pub fast: String,
}

impl ModelTiers {
    pub fn for_tier(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Default => &self.default,
            ModelTier::Fast if !self.fast.is_empty() => &self.fast,
            ModelTier::Fast => &self.default,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GatewayGrant {
    pub provider: String,
    pub api_key: String,
    pub models: ModelTiers,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct PlanLimits {
    #[serde(default)]
    pub warn_at_percent: u32,
    #[serde(default)]
    pub max_turns_per_session: u32,
}

/// What `GET /v1/entitlement` returns. Unknown fields are ignored so the
/// backend can grow without breaking older builds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Entitlement {
    pub user_id: String,
    pub status: EntitlementStatus,
    pub plan_id: String,
    pub plan_name: String,
    #[serde(default)]
    pub allowance_units: i64,
    #[serde(default)]
    pub used_units: i64,
    #[serde(default)]
    pub remaining_units: i64,
    #[serde(default)]
    pub warn_at_percent: u32,
    #[serde(default)]
    pub cycle_ends_at: String,
    #[serde(default)]
    pub gateway: Option<GatewayGrant>,
    #[serde(default)]
    pub limits: PlanLimits,
}

impl Entitlement {
    /// Whole percent of the cycle's allowance already used, capped at 100.
    pub fn used_percent(&self) -> u32 {
        if self.allowance_units <= 0 {
            return 100;
        }
        let percent = self.used_units.max(0) * 100 / self.allowance_units;
        percent.clamp(0, 100) as u32
    }

    pub fn is_exhausted(&self) -> bool {
        self.status == EntitlementStatus::Exhausted || self.remaining_units <= 0
    }

    /// The 80% (by default) warning the spec asks for, computed here so the
    /// panel and the toasts agree.
    pub fn is_running_low(&self) -> bool {
        let threshold = if self.warn_at_percent == 0 {
            80
        } else {
            self.warn_at_percent
        };
        !self.is_exhausted() && self.used_percent() >= threshold
    }

    /// One line for the panel footer.
    pub fn balance_summary(&self) -> String {
        if self.is_exhausted() {
            return "Out of allowance for this cycle".to_string();
        }
        format!("{}% of this cycle's allowance used", self.used_percent())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConnectResponse {
    pub credential: String,
    pub entitlement: Entitlement,
}

/// The backend refused the request for a reason the person should read as
/// is. `Revoked` and `Unauthorized` are the two the panel acts on: both mean
/// the stored credential is dead and the app should fall back to the free
/// path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlusError {
    Revoked(String),
    Unauthorized(String),
    Rejected { status: u16, message: String },
}

impl PlusError {
    pub fn message(&self) -> &str {
        match self {
            Self::Revoked(message) | Self::Unauthorized(message) => message,
            Self::Rejected { message, .. } => message,
        }
    }

    /// True when the stored credential can't be used again.
    pub fn invalidates_credential(&self) -> bool {
        matches!(self, Self::Revoked(_) | Self::Unauthorized(_))
    }
}

impl std::fmt::Display for PlusError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for PlusError {}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

async fn send_json(
    http: &Arc<dyn HttpClient>,
    method: http::Method,
    url: &str,
    credential: Option<&str>,
    body: Option<String>,
) -> Result<(http::StatusCode, String)> {
    let mut builder = Request::builder()
        .method(method)
        .uri(url)
        .header("Accept", "application/json");
    if let Some(credential) = credential {
        builder = builder.header("Authorization", format!("Bearer {credential}"));
    }
    let request = match body {
        Some(body) => builder
            .header("Content-Type", "application/json")
            .body(AsyncBody::from(body.into_bytes()))?,
        None => builder.body(AsyncBody::default())?,
    };
    let mut response = http
        .send(request)
        .await
        .with_context(|| format!("reaching the Thock Plus service at {url}"))?;
    let mut text = String::new();
    response.body_mut().read_to_string(&mut text).await?;
    Ok((response.status(), text))
}

fn error_message(status: http::StatusCode, body: &str) -> String {
    serde_json::from_str::<ErrorBody>(body)
        .map(|body| body.error)
        .unwrap_or_else(|_| format!("The Thock Plus service answered with status {status}."))
}

fn classify_failure(status: http::StatusCode, body: &str) -> anyhow::Error {
    let message = error_message(status, body);
    let error = match status {
        http::StatusCode::UNAUTHORIZED => PlusError::Unauthorized(message),
        http::StatusCode::FORBIDDEN => PlusError::Revoked(message),
        _ => PlusError::Rejected {
            status: status.as_u16(),
            message,
        },
    };
    anyhow!(error)
}

/// Trades an invite code for a credential and the first entitlement read.
pub async fn connect(
    http: &Arc<dyn HttpClient>,
    base_url: &str,
    invite_code: &str,
    device: &str,
) -> Result<ConnectResponse> {
    let body =
        serde_json::json!({ "invite_code": invite_code.trim(), "device": device }).to_string();
    let (status, text) = send_json(
        http,
        http::Method::POST,
        &format!("{base_url}/v1/connect"),
        None,
        Some(body),
    )
    .await?;
    if !status.is_success() {
        return Err(classify_failure(status, &text));
    }
    serde_json::from_str(&text).context("reading the Thock Plus connect response")
}

pub async fn fetch_entitlement(
    http: &Arc<dyn HttpClient>,
    base_url: &str,
    credential: &str,
) -> Result<Entitlement> {
    let (status, text) = send_json(
        http,
        http::Method::GET,
        &format!("{base_url}/v1/entitlement"),
        Some(credential),
        None,
    )
    .await?;
    if !status.is_success() {
        return Err(classify_failure(status, &text));
    }
    serde_json::from_str(&text).context("reading the Thock Plus entitlement")
}

/// Tells the backend to kill the gateway key. A dead credential (already
/// revoked server-side) counts as success: the outcome is the same.
pub async fn disconnect(
    http: &Arc<dyn HttpClient>,
    base_url: &str,
    credential: &str,
) -> Result<()> {
    let (status, text) = send_json(
        http,
        http::Method::POST,
        &format!("{base_url}/v1/disconnect"),
        Some(credential),
        None,
    )
    .await?;
    if status.is_success()
        || status == http::StatusCode::UNAUTHORIZED
        || status == http::StatusCode::FORBIDDEN
    {
        return Ok(());
    }
    Err(classify_failure(status, &text))
}

/// The stored credential, if the user has connected Thock Plus on this
/// machine.
pub async fn read_credential(cx: &AsyncApp) -> Result<Option<String>> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    let Some((_, credential)) = provider.read_credentials(KEYCHAIN_URL, cx).await? else {
        return Ok(None);
    };
    let credential =
        String::from_utf8(credential).context("the stored Thock Plus credential is not UTF-8")?;
    Ok(Some(credential).filter(|credential| !credential.is_empty()))
}

pub async fn write_credential(credential: &str, cx: &AsyncApp) -> Result<()> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    provider
        .write_credentials(KEYCHAIN_URL, KEYCHAIN_USERNAME, credential.as_bytes(), cx)
        .await
}

pub async fn delete_credential(cx: &AsyncApp) -> Result<()> {
    let provider = cx.update(|cx| zed_credentials_provider::global(cx));
    provider.delete_credentials(KEYCHAIN_URL, cx).await
}

/// A short, non-identifying device label for the backend's records.
pub fn device_label() -> String {
    format!("Thock on {}", std::env::consts::OS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use http_client::{FakeHttpClient, Response};

    fn entitlement_json(status: &str, allowance: i64, used: i64) -> String {
        serde_json::json!({
            "user_id": "u1",
            "status": status,
            "plan_id": "dev",
            "plan_name": "Thock Plus (dev)",
            "allowance_units": allowance,
            "used_units": used,
            "remaining_units": (allowance - used).max(0),
            "warn_at_percent": 80,
            "cycle_ends_at": "2026-10-11T12:00:00Z",
            "gateway": {
                "provider": "openrouter",
                "api_key": "sk-or-v1-test",
                "models": {"default": "google/gemini-2.5-flash", "fast": "google/gemini-2.5-flash-lite"}
            },
            "limits": {"warn_at_percent": 80, "max_turns_per_session": 50},
            "a_future_field": true
        })
        .to_string()
    }

    fn fake(status: u16, body: String) -> Arc<dyn HttpClient> {
        FakeHttpClient::create(move |_| {
            let body = body.clone();
            async move {
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from(body.into_bytes()))
                    .unwrap())
            }
        })
    }

    #[test]
    fn entitlement_parses_and_computes_the_balance() {
        let entitlement: Entitlement =
            serde_json::from_str(&entitlement_json("active", 500, 410)).unwrap();
        assert_eq!(entitlement.status, EntitlementStatus::Active);
        assert_eq!(entitlement.used_percent(), 82);
        assert!(entitlement.is_running_low());
        assert!(!entitlement.is_exhausted());
        let gateway = entitlement.gateway.as_ref().unwrap();
        assert_eq!(
            gateway.models.for_tier(ModelTier::Default),
            "google/gemini-2.5-flash"
        );
        assert_eq!(
            gateway.models.for_tier(ModelTier::Fast),
            "google/gemini-2.5-flash-lite"
        );
        assert_eq!(entitlement.limits.max_turns_per_session, 50);

        let exhausted: Entitlement =
            serde_json::from_str(&entitlement_json("exhausted", 500, 500)).unwrap();
        assert!(exhausted.is_exhausted());
        assert!(!exhausted.is_running_low());
        assert_eq!(
            exhausted.balance_summary(),
            "Out of allowance for this cycle"
        );

        // A status this build doesn't know about must not fail the parse.
        let odd: Entitlement = serde_json::from_str(&entitlement_json("paused", 500, 10)).unwrap();
        assert_eq!(odd.status, EntitlementStatus::Unknown);
    }

    #[test]
    fn fast_tier_falls_back_to_default_when_unmapped() {
        let tiers: ModelTiers = serde_json::from_str(r#"{"default": "x/y"}"#).unwrap();
        assert_eq!(tiers.for_tier(ModelTier::Fast), "x/y");
    }

    #[test]
    fn connect_returns_the_credential_and_entitlement() {
        let body = serde_json::json!({
            "credential": "tpk_abc",
            "entitlement": serde_json::from_str::<serde_json::Value>(&entitlement_json("active", 500, 0)).unwrap()
        })
        .to_string();
        let http = fake(200, body);
        let response = block_on(connect(
            &http,
            "https://plus.test",
            "THOCK-AAAA-BBBB",
            "test",
        ))
        .unwrap();
        assert_eq!(response.credential, "tpk_abc");
        assert_eq!(response.entitlement.remaining_units, 500);
    }

    #[test]
    fn backend_errors_are_typed_and_keep_their_sentence() {
        let http = fake(
            404,
            r#"{"error": "That invite code isn't one we recognize."}"#.to_string(),
        );
        let error =
            block_on(connect(&http, "https://plus.test", "THOCK-NOPE", "test")).unwrap_err();
        let error = error.downcast::<PlusError>().unwrap();
        assert_eq!(
            error,
            PlusError::Rejected {
                status: 404,
                message: "That invite code isn't one we recognize.".to_string()
            }
        );
        assert!(!error.invalidates_credential());

        let http = fake(
            403,
            r#"{"error": "Your Thock Plus access was turned off."}"#.to_string(),
        );
        let error = block_on(fetch_entitlement(&http, "https://plus.test", "tpk_x")).unwrap_err();
        let error = error.downcast::<PlusError>().unwrap();
        assert!(matches!(error, PlusError::Revoked(_)));
        assert!(error.invalidates_credential());

        let http = fake(401, "not json".to_string());
        let error = block_on(fetch_entitlement(&http, "https://plus.test", "tpk_x")).unwrap_err();
        let error = error.downcast::<PlusError>().unwrap();
        assert!(matches!(error, PlusError::Unauthorized(_)));
        assert!(error.message().contains("401"));
    }

    #[test]
    fn disconnect_treats_a_dead_credential_as_done() {
        let http = fake(403, r#"{"error": "gone"}"#.to_string());
        block_on(disconnect(&http, "https://plus.test", "tpk_x")).unwrap();
        let http = fake(500, r#"{"error": "boom"}"#.to_string());
        assert!(block_on(disconnect(&http, "https://plus.test", "tpk_x")).is_err());
    }

    #[test]
    fn backend_url_strips_trailing_slashes() {
        assert!(!backend_url().ends_with('/'));
    }
}
