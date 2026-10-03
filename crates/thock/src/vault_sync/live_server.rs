//! The desk's sync service against a real Plus server (`thock/script/integration`
//! starts one). Each test returns early unless THOCK_INTEGRATION_URL,
//! THOCK_INTEGRATION_ADMIN_TOKEN and THOCK_INTEGRATION_INVITE are set. The phone
//! half is plain HTTP per the contract; the Swift suite covers the real phone.
//!
//! Requests go over a blocking HTTP/1.0 socket inside the fake client's
//! future: the round trip finishes within one poll, so GPUI's deterministic
//! test executor never parks on outside I/O, and no HTTP stack is pulled in.

use super::*;
use fs::FakeFs;
use gpui::TestAppContext;
use http_client::FakeHttpClient;
use settings::SettingsStore;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::PathBuf;
use thock_sync_core::{Heading, Placement};

const KEY: [u8; 32] = [42u8; 32];
const DAILY: &str = "daily/2026-10-02.md";
const ODD: &str = "notes/Café & ideas #1.md";
const GONE: &str = "notes/100% done?.md";

struct Live {
    url: String,
    admin_token: String,
    invite: String,
}

impl Live {
    fn from_env() -> Option<Self> {
        let var = |name| std::env::var(name).ok().filter(|value| !value.is_empty());
        let live = Self {
            url: var("THOCK_INTEGRATION_URL")?
                .trim_end_matches('/')
                .to_string(),
            admin_token: var("THOCK_INTEGRATION_ADMIN_TOKEN")?,
            invite: var("THOCK_INTEGRATION_INVITE")?,
        };
        Some(live)
    }

    /// A JSON call that must succeed.
    fn call(
        &self,
        method: &str,
        route: &str,
        credential: &str,
        body: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let (status, value) = self.try_call(method, route, credential, body);
        assert!(
            (200..300).contains(&status),
            "{method} {route} answered {status}: {value}"
        );
        value
    }

    fn try_call(
        &self,
        method: &str,
        route: &str,
        credential: &str,
        body: Option<serde_json::Value>,
    ) -> (u16, serde_json::Value) {
        let body = body.map(|body| body.to_string()).unwrap_or_default();
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {credential}")),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let (status, bytes) = roundtrip(
            method,
            &format!("{}{route}", self.url),
            &headers,
            body.as_bytes(),
        )
        .expect("reaching the integration server");
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    /// Connects a fresh Plus user; returns its credential and user id.
    fn connect_desk(&self) -> (String, String) {
        let device = format!(
            "integration desk {}",
            hex::encode(rand::random::<[u8; 8]>())
        );
        let connected = self.call(
            "POST",
            "/v1/connect",
            "",
            Some(serde_json::json!({"invite_code": self.invite, "device": device})),
        );
        let credential = connected["credential"].as_str().expect("a credential");
        let users = self.call("GET", "/admin/users", &self.admin_token, None);
        let user_id = users
            .as_array()
            .and_then(|users| users.iter().find(|user| user["device"] == device.as_str()))
            .and_then(|user| user["id"].as_str())
            .expect("the new user is listed");
        (credential.to_string(), user_id.to_string())
    }
}

/// One HTTP/1.0 exchange. The server closes the connection after answering,
/// so the body is everything up to EOF and never chunked.
fn roundtrip(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(u16, Vec<u8>)> {
    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().context("no host")?.to_string();
    let port = parsed.port_or_known_default().context("no port")?;
    let target = &parsed[url::Position::BeforePath..];
    let mut stream = TcpStream::connect((host.as_str(), port))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut head = format!("{method} {target} HTTP/1.0\r\nHost: {host}:{port}\r\n");
    for (name, value) in headers {
        if !name.eq_ignore_ascii_case("content-length") {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    // One write: a handler that never reads the body can answer and close
    // while a second write is still in flight, which resets the connection.
    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    stream.write_all(&request)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("no end of headers")?;
    let status_line = String::from_utf8_lossy(&response[..split]);
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .context("no status")?;
    Ok((status, response[split + 4..].to_vec()))
}

fn live_http() -> Arc<dyn HttpClient> {
    FakeHttpClient::create(|mut request| async move {
        // The feed never ends, which a read-to-EOF client can't hold; the
        // service treats a refused feed as "poll instead", which is all
        // these tests need.
        if request.uri().path() == "/v1/vault/feed" {
            return Ok(Response::builder().status(503).body(AsyncBody::from(
                r#"{"error":"no feed here","code":"unavailable"}"#,
            ))?);
        }
        let mut body = Vec::new();
        request.body_mut().read_to_end(&mut body).await?;
        let headers: Vec<_> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let (status, bytes) = roundtrip(
            request.method().as_str(),
            &request.uri().to_string(),
            &headers,
            &body,
        )?;
        Ok(Response::builder()
            .status(status)
            .body(AsyncBody::from(bytes))?)
    })
}

async fn start_service(
    live: &Live,
    credential: &str,
    cx: &mut TestAppContext,
) -> (Arc<FakeFs>, Entity<VaultSyncService>) {
    cx.update(|cx| {
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);
    });
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        "/vault",
        serde_json::json!({
            ".thock": {"config.toml": ""},
            "daily": {"2026-10-02.md": "# Today\n\n## Day planner\n- [ ] Walk\n"},
            "notes": {
                "Café & ideas #1.md": "# Café\n\nwith ünïcode, a tab\there\n",
                "100% done?.md": "no newline at the end",
            },
        }),
    )
    .await;
    let project = Project::test(fs.clone(), [Path::new("/vault")], cx).await;
    cx.run_until_parked();
    let api = SyncApi::new(live_http(), live.url.clone(), credential.to_string());
    // What `begin_pairing` does before the session starts.
    api.ensure_vault("Integration desk", &thock_sync_core::key_check(&KEY))
        .await
        .unwrap();
    let service = cx.new(|cx| VaultSyncService::new(project.clone(), cx));
    service.update(cx, |service, cx| {
        let vault = Vault {
            root: PathBuf::from("/vault"),
            config: crate::vault::VaultConfig::default(),
        };
        service.configure_for_test(vault, api, KEY, cx)
    });
    cx.run_until_parked();
    (fs, service)
}

fn run_pass(service: &Entity<VaultSyncService>, catch_up: bool, cx: &mut TestAppContext) {
    service.update(cx, |service, cx| {
        service.pending.catch_up |= catch_up;
        service.pending.drain = true;
        service.schedule_work(cx);
    });
    cx.run_until_parked();
}

/// What a phone holds after a pull: path → plaintext, plus the cursor.
fn phone_pull(
    live: &Live,
    phone: &str,
    since: Option<u64>,
) -> (BTreeMap<String, Option<String>>, u64) {
    let route = match since {
        Some(since) => format!("/v1/vault/files?since={since}"),
        None => "/v1/vault/files".to_string(),
    };
    let page = live.call("GET", &route, phone, None);
    assert_eq!(page["has_more"], false);
    let mut files = BTreeMap::new();
    for row in page["files"].as_array().expect("files") {
        let path = row["path"].as_str().expect("path").to_string();
        if row["deleted"] == true {
            files.insert(path, None);
            continue;
        }
        let (status, envelope) = roundtrip(
            "GET",
            row["download_url"].as_str().expect("a download url"),
            &[],
            &[],
        )
        .expect("downloading");
        assert_eq!(status, 200, "download of {path}");
        assert_eq!(
            Some(thock_sync_core::content_hash(&envelope).as_str()),
            row["content_hash"].as_str(),
            "{path}"
        );
        let plaintext = thock_sync_core::open(
            &KEY,
            SealContext::File {
                path: path.clone(),
                blob_id: row["blob_id"].as_str().expect("blob id").to_string(),
            },
            &envelope,
        )
        .expect("the desk's envelope opens with the shared key");
        files.insert(path, Some(String::from_utf8(plaintext).expect("utf-8")));
    }
    (files, page["next_since"].as_u64().expect("next_since"))
}

fn pair_phone(live: &Live, desk: &str) -> String {
    let pairing = live.call("POST", "/v1/vault/pairings", desk, None);
    let paired = live.call(
        "POST",
        "/v1/vault/pair",
        "",
        Some(serde_json::json!({
            "code": pairing["code"], "device_name": "Integration iPhone", "platform": "ios",
        })),
    );
    assert_eq!(
        paired["vault"]["key_check"].as_str(),
        Some(thock_sync_core::key_check(&KEY).as_str())
    );
    paired["credential"]
        .as_str()
        .expect("a phone credential")
        .to_string()
}

fn queue_phone_write(
    live: &Live,
    phone: &str,
    base_version: u64,
    line: &str,
) -> (u16, serde_json::Value) {
    let client_id = {
        let hex = hex::encode(rand::random::<[u8; 16]>());
        format!(
            "{}-{}-4{}-a{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[13..16],
            &hex[17..20],
            &hex[20..]
        )
    };
    let write = Write {
        v: 1,
        client_id: client_id.clone(),
        path: DAILY.to_string(),
        made_at: "2026-10-02T13:58:02Z".to_string(),
        device_id: "phone".to_string(),
        operation: Operation::Append {
            heading: Some(Heading {
                text: "Day planner".to_string(),
                level: 2,
                ordinal: 0,
            }),
            lines: vec![line.to_string()],
            placement: Placement::End,
            blank_line_before: false,
            create_from_template: false,
        },
    };
    let envelope = thock_sync_core::seal(
        &KEY,
        SealContext::Write {
            client_id: client_id.clone(),
        },
        write.to_json().as_bytes(),
    );
    live.try_call(
        "POST",
        "/v1/vault/writes",
        phone,
        Some(serde_json::json!({
            "client_id": client_id,
            "path": DAILY,
            "base_version": base_version,
            "payload": base64::engine::general_purpose::STANDARD.encode(envelope),
        })),
    )
}

#[gpui::test]
async fn desk_and_phone_converge_through_the_server(cx: &mut TestAppContext) {
    let Some(live) = Live::from_env() else {
        eprintln!("skipping: THOCK_INTEGRATION_URL is not set; run thock/script/integration");
        return;
    };
    let (desk, _) = live.connect_desk();
    let (fs, service) = start_service(&live, &desk, cx).await;
    service.read_with(cx, |service, _| {
        assert!(
            matches!(service.status(), PhoneSyncState::PhoneNotConnected),
            "{:?}",
            service.status()
        );
        assert_eq!(service.state.files.len(), 4);
    });

    // The phone pairs and pulls exactly what is on the desk's disk.
    let phone = pair_phone(&live, &desk);
    let (files, cursor) = phone_pull(&live, &phone, None);
    for (path, text) in &files {
        let on_disk = fs.load(&Path::new("/vault").join(path)).await.unwrap();
        assert_eq!(text.as_deref(), Some(on_disk.as_str()), "{path}");
    }
    assert_eq!(
        files.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![".thock/config.toml", DAILY, GONE, ODD]
    );

    // A phone write is drained, applied, re-uploaded and acked.
    let base = service.read_with(cx, |service, _| service.state.files[DAILY].version);
    let (status, accepted) = queue_phone_write(&live, &phone, base, "- [ ] Buy a card");
    assert_eq!(status, 201, "{accepted}");
    let seq = accepted["seq"].as_u64().expect("seq");
    run_pass(&service, false, cx);
    let note = fs.load(&Path::new("/vault").join(DAILY)).await.unwrap();
    assert_eq!(
        note,
        "# Today\n\n## Day planner\n- [ ] Walk\n- [ ] Buy a card\n"
    );
    let vault = live.call("GET", "/v1/vault", &phone, None);
    assert_eq!(vault["writes"]["pending"], 0);
    assert_eq!(vault["writes"]["acked_through_seq"].as_u64(), Some(seq));
    let (changed, cursor) = phone_pull(&live, &phone, Some(cursor));
    assert_eq!(changed.get(DAILY), Some(&Some(note)));
    assert_eq!(vault["writes"]["acked_at_version"], vault["latest_version"]);
    service.read_with(cx, |service, _| {
        assert!(
            matches!(service.status(), PhoneSyncState::UpToDate { .. }),
            "{:?}",
            service.status()
        );
        assert_eq!(service.state.acked_through_seq, seq);
    });

    // A deletion at the desk reaches the phone as a tombstone.
    fs.remove_file(&Path::new("/vault").join(GONE), RemoveOptions::default())
        .await
        .unwrap();
    run_pass(&service, true, cx);
    let (changed, _) = phone_pull(&live, &phone, Some(cursor));
    assert_eq!(changed.get(GONE), Some(&None));
    assert_eq!(changed.len(), 1, "{changed:?}");
}

#[gpui::test]
async fn a_revoked_phone_and_a_lapsed_plan_stop_the_right_calls(cx: &mut TestAppContext) {
    let Some(live) = Live::from_env() else {
        eprintln!("skipping: THOCK_INTEGRATION_URL is not set; run thock/script/integration");
        return;
    };
    let (desk, user_id) = live.connect_desk();
    let (fs, service) = start_service(&live, &desk, cx).await;

    // The desk's "Disconnect phone" path: the old credential stops at once.
    let phone = pair_phone(&live, &desk);
    run_pass(&service, false, cx);
    let phone_id = service.read_with(cx, |service, _| {
        service
            .phone()
            .expect("the phone is listed")
            .device_id
            .clone()
    });
    let api = service.read_with(cx, |service, _| {
        service.session.as_ref().expect("a session").api.clone()
    });
    api.revoke_device(&phone_id).await.unwrap();
    let (status, refused) = queue_phone_write(&live, &phone, 0, "- [ ] Too late");
    assert_eq!(
        (status, refused["code"].as_str()),
        (401, Some("unauthorized"))
    );

    // A lapse pauses the desk instead of failing it.
    live.call(
        "POST",
        &format!("/admin/users/{user_id}/vault/lapse"),
        &live.admin_token,
        Some(serde_json::json!({"lapsed": true})),
    );
    fs.insert_file(
        &Path::new("/vault/notes/after lapse.md"),
        b"# Kept on the desk\n".to_vec(),
    )
    .await;
    run_pass(&service, true, cx);
    service.read_with(cx, |service, _| {
        assert!(
            matches!(service.status(), PhoneSyncState::Paused),
            "{:?}",
            service.status()
        );
        assert!(!service.state.files.contains_key("notes/after lapse.md"));
    });

    // Renewal: the next pass uploads what waited.
    live.call(
        "POST",
        &format!("/admin/users/{user_id}/vault/lapse"),
        &live.admin_token,
        Some(serde_json::json!({"lapsed": false})),
    );
    run_pass(&service, true, cx);
    service.read_with(cx, |service, _| {
        assert!(service.state.files.contains_key("notes/after lapse.md"));
    });
}
