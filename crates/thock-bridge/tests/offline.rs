//! The client against a stand-in desk (spec V55 §14): pair, call a tool,
//! lose the desk, answer from the cache, queue a write, get the desk back,
//! drain once. The desk here is the library's own `dispatch` over an
//! in-memory `ToolHost` behind a plain `TcpListener`; the real one lives in
//! the app and is tested there.

// The test drives the real binary over piped stdio and waits on it; there is
// no executor here for `smol::process` to be the right answer.
#![allow(clippy::disallowed_methods)]

use chrono::NaiveDate;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thock_bridge::{
    CallMeta, ClientIdentity, HostError, JsonRpcRequest, JsonRpcResponse, PairRequest,
    PairResponse, ReadTarget, RecentNote, Route, SearchHit, SearchQuery, ToolHost, WriteOutcome,
};

const CODE: &str = "maple-river-stone-ocean-lamp-fox";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// What the stand-in desk remembers across restarts.
#[derive(Default)]
struct Vault {
    notes: BTreeMap<String, String>,
    inbox: Vec<String>,
    seen: BTreeMap<String, WriteOutcome>,
    mirrored: BTreeMap<String, String>,
    calls: Vec<String>,
}

struct VaultHost<'a> {
    vault: &'a mut Vault,
    identity: ClientIdentity,
}

impl ToolHost for VaultHost<'_> {
    async fn context(&mut self) -> Result<String, HostError> {
        Ok("# Right now\n\nToday is Wednesday.".into())
    }

    async fn memory_index(&mut self) -> Result<String, HostError> {
        Ok("# Learned\n- one\n".into())
    }

    async fn search(&mut self, query: &SearchQuery) -> Result<Vec<SearchHit>, HostError> {
        Ok(self
            .vault
            .notes
            .iter()
            .filter(|(_, body)| body.contains(&query.query))
            .map(|(path, body)| SearchHit {
                path: path.clone(),
                title: path.clone(),
                modified: NaiveDate::from_ymd_opt(2026, 11, 4).unwrap_or_default(),
                snippet: body.clone(),
            })
            .collect())
    }

    async fn read(&mut self, target: &ReadTarget) -> Result<String, HostError> {
        match target {
            ReadTarget::Path(path) => self
                .vault
                .notes
                .get(path)
                .cloned()
                .ok_or_else(|| HostError(format!("{path} is not in the vault"))),
            ReadTarget::Link(link) => Err(HostError(format!("no note titled {link}"))),
        }
    }

    async fn recent(&mut self, _: u32, _: Option<&str>) -> Result<Vec<RecentNote>, HostError> {
        Ok(Vec::new())
    }

    async fn remember(&mut self, fact: &str, meta: &CallMeta) -> Result<WriteOutcome, HostError> {
        if let Some(outcome) = meta.call_id.as_ref().and_then(|id| self.vault.seen.get(id)) {
            return Ok(*outcome);
        }
        let fact = thock_bridge::check_fact(fact, &[])?;
        if fact.contains("refuse-me") {
            return Err(HostError(
                "that topic is on the person's do-not-keep list".into(),
            ));
        }
        let date = meta
            .date
            .unwrap_or_else(|| chrono::Local::now().date_naive());
        self.vault
            .inbox
            .push(thock_bridge::inbox_line(date, &fact, &self.identity));
        if let Some(id) = &meta.call_id {
            self.vault.seen.insert(id.clone(), WriteOutcome::Applied);
        }
        Ok(WriteOutcome::Applied)
    }

    async fn note_today(
        &mut self,
        lines: &[String],
        _: &CallMeta,
    ) -> Result<WriteOutcome, HostError> {
        let note = self.vault.notes.entry("daily/today.md".into()).or_default();
        for line in lines {
            note.push_str(&thock_bridge::today_line(&self.identity, line));
            note.push('\n');
        }
        Ok(WriteOutcome::Applied)
    }
}

struct Desk {
    port: u16,
    vault: Arc<Mutex<Vault>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Desk {
    fn start(port: u16, vault: Arc<Mutex<Vault>>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind the stand-in desk");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let stop = stop.clone();
            let vault = vault.clone();
            move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).expect("blocking stream");
                            serve_one(stream, &vault);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            }
        });
        Self {
            port,
            vault,
            stop,
            thread: Some(thread),
        }
    }

    fn stop(mut self) -> Arc<Mutex<Vault>> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("the desk thread");
        }
        self.vault.clone()
    }
}

/// One connection through the library's own HTTP helpers, the same ones
/// the desk serves with.
fn serve_one(mut stream: TcpStream, vault: &Arc<Mutex<Vault>>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let (status, content_type, reply) = match thock_bridge::read_request_head(&mut reader) {
        Ok(head) => match thock_bridge::read_body(&mut reader, head.content_length) {
            Ok(body) => respond(&head.method, &head.url, &head.headers, &body, vault),
            Err(status) => (status, "text/plain", Vec::new()),
        },
        Err(status) => (status, "text/plain", Vec::new()),
    };
    thock_bridge::write_response(&mut stream, status, content_type, &reply).expect("reply");
}

fn respond(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    vault: &Arc<Mutex<Vault>>,
) -> (u16, &'static str, Vec<u8>) {
    let route = thock_bridge::parse_route(method, url);
    let mut vault = vault.lock().expect("vault lock");
    vault.calls.push(format!("{method} {url}"));
    if route == Route::Pair {
        let request: PairRequest = serde_json::from_slice(body).expect("pair body");
        if request.code != CODE {
            return (
                403,
                "application/json",
                b"{\"error\":\"wrong code\"}".to_vec(),
            );
        }
        let reply = serde_json::to_vec(&PairResponse {
            token: TOKEN.into(),
            vault: "Diego's vault".into(),
            desk: "palloma".into(),
        })
        .expect("pair reply");
        return (200, "application/json", reply);
    }
    let token = thock_bridge::bearer_token(
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    if token != Some(TOKEN) {
        return (
            401,
            "application/json",
            thock_bridge::response_json("unknown client; pair again from the desk").into_bytes(),
        );
    }
    let harness = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(thock_bridge::HARNESS_HEADER))
        .map(|(_, value)| value.as_str())
        .unwrap_or("agent");
    match route {
        Route::Health => (
            200,
            "application/json",
            json!({"vault": "Diego's vault", "desk": "palloma", "version": "test", "serving_since": "now"})
                .to_string()
                .into_bytes(),
        ),
        Route::Mcp => {
            let request = match JsonRpcRequest::parse(&String::from_utf8_lossy(body)) {
                Ok(request) => request,
                Err(error) => return (400, "application/json", error.to_json().into_bytes()),
            };
            let mut host = VaultHost {
                vault: &mut vault,
                identity: ClientIdentity::new(harness, "robin"),
            };
            match pollster::block_on(thock_bridge::dispatch(&mut host, request, "test")) {
                Some(response) => (200, "application/json", response.to_json().into_bytes()),
                None => (202, "text/plain", Vec::new()),
            }
        }
        Route::Skill => (200, "text/markdown", b"---\nname: thock-memory\n---\nthe skill\n".to_vec()),
        Route::AgentFile { harness, path } => {
            if let Some(reason) =
                thock_bridge::find_unkeepable(&String::from_utf8_lossy(body), &[])
            {
                return (
                    422,
                    "application/json",
                    thock_bridge::response_json(&format!("{path}: {reason}")).into_bytes(),
                );
            }
            vault.mirrored.insert(
                format!("{harness}/robin/{path}"),
                String::from_utf8_lossy(body).into_owned(),
            );
            (204, "text/plain", Vec::new())
        }
        Route::McpNotPost => (405, "text/plain", Vec::new()),
        Route::Pair | Route::NotFound => (404, "text/plain", Vec::new()),
    }
}

struct Client {
    home: tempfile::TempDir,
}

impl Client {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_thock-bridge"));
        command
            .env("THOCK_BRIDGE_HOME", self.home.path())
            .env("HOME", self.home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let output = self
            .command()
            .args(args)
            .output()
            .expect("run thock-bridge");
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn serve(&self) -> Serve {
        let child = self
            .command()
            .args(["serve", "--harness", "claude-code"])
            .spawn()
            .expect("spawn serve");
        Serve { child, next_id: 1 }
    }

    fn outbox_lines(&self) -> usize {
        std::fs::read_to_string(self.home.path().join("data/outbox.jsonl"))
            .map(|raw| raw.lines().filter(|line| !line.trim().is_empty()).count())
            .unwrap_or(0)
    }
}

struct Serve {
    child: Child,
    next_id: u64,
}

impl Serve {
    fn call(&mut self, method: &str, params: Value) -> JsonRpcResponse {
        let id = self.next_id;
        self.next_id += 1;
        let request = JsonRpcRequest::new(json!(id), method, params);
        let stdin = self.child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{}", serde_json::to_string(&request).expect("json")).expect("write");
        stdin.flush().expect("flush");
        let stdout = self.child.stdout.as_mut().expect("stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read a reply");
        let response = JsonRpcResponse::parse(line.trim()).expect("a JSON-RPC reply");
        assert_eq!(response.id, json!(id));
        response
    }

    fn tool(&mut self, name: &str, arguments: Value) -> thock_bridge::CallToolResponse {
        self.call("tools/call", json!({"name": name, "arguments": arguments}))
            .tool_result()
            .expect("a tool result")
    }

    fn finish(mut self) -> String {
        drop(self.child.stdin.take());
        let output = self.child.wait_with_output().expect("serve exits");
        assert!(output.status.success(), "serve exited badly");
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

#[test]
fn pair_call_go_offline_and_drain_once() {
    let vault = Arc::new(Mutex::new(Vault::default()));
    vault.lock().expect("lock").notes.insert(
        "daily/2026-11-03.md".into(),
        "# Tuesday\n\nMet Ana.\n".into(),
    );
    let desk = Desk::start(0, vault.clone());
    let port = desk.port;
    let client = Client::new();

    // Not paired yet: status says so and pairing with the wrong code fails.
    let (ok, stdout, _) = client.run(&["status"]);
    assert!(ok);
    assert!(stdout.contains("Not paired"));
    let (ok, _, stderr) = client.run(&["pair", &format!("127.0.0.1:{port}"), "wrong-code"]);
    assert!(!ok);
    assert!(stderr.contains("refused the code"), "{stderr}");
    let (ok, stdout, stderr) = client.run(&["pair", &format!("127.0.0.1:{port}"), CODE]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("Paired with Diego's vault on palloma"),
        "{stdout}"
    );
    let config = std::fs::read_to_string(client.home.path().join("bridge.toml")).expect("config");
    assert!(config.contains(TOKEN));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(client.home.path().join("bridge.toml"))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the token file is private");
    }

    // Online: initialize is local, context and remember go to the desk.
    let mut serve = client.serve();
    let init = serve.call("initialize", json!({"protocolVersion": "2025-11-25", "clientInfo": {"name": "Claude Code"}, "capabilities": {}}));
    assert_eq!(init.result.expect("init")["serverInfo"]["name"], "thock");
    let tools = serve.call("tools/list", json!({}));
    assert_eq!(
        tools.result.expect("tools")["tools"]
            .as_array()
            .expect("array")
            .len(),
        6
    );
    let context = serve.tool("context", json!({}));
    assert!(!context.is_error);
    assert_eq!(context.first_text(), "# Right now\n\nToday is Wednesday.");
    let read = serve.tool("read", json!({"path": "daily/2026-11-03.md"}));
    assert_eq!(read.first_text(), "# Tuesday\n\nMet Ana.\n");
    let remembered = serve.tool("remember", json!({"fact": "Likes short PR titles"}));
    assert!(!remembered.is_error, "{}", remembered.first_text());
    assert!(
        remembered
            .first_text()
            .starts_with("Noted in memory/inbox.md")
    );
    {
        let vault = vault.lock().expect("lock");
        assert_eq!(vault.inbox.len(), 1);
        assert!(vault.inbox[0].ends_with("Likes short PR titles · via claude-code@robin"));
        assert_eq!(vault.seen.len(), 1, "the call carried an id");
    }

    // The desk goes away: reads come from the cache, writes queue.
    let vault = desk.stop();
    let context = serve.tool("context", json!({}));
    assert!(!context.is_error, "{}", context.first_text());
    assert!(
        context
            .first_text()
            .starts_with("# Right now\n\nToday is Wednesday.")
    );
    assert!(
        context.first_text().contains("(from cache, as of ")
            && context.first_text().contains("is unreachable)"),
        "{}",
        context.first_text()
    );
    let read = serve.tool("read", json!({"path": "daily/2026-11-03.md"}));
    assert!(read.first_text().starts_with("# Tuesday\n\nMet Ana.\n"));
    let miss = serve.tool("read", json!({"path": "daily/2026-11-02.md"}));
    assert!(miss.is_error);
    assert!(
        miss.first_text().contains("isn't in the cache"),
        "{}",
        miss.first_text()
    );
    let queued = serve.tool("remember", json!({"fact": "Second fact, queued"}));
    assert!(!queued.is_error);
    assert_eq!(queued.first_text(), thock_bridge::QUEUED_SENTENCE);
    let queued = serve.tool("note_today", json!({"line": "Shipped it offline."}));
    assert_eq!(queued.first_text(), thock_bridge::QUEUED_SENTENCE);
    // A line the desk will refuse for good queues too (the client can't
    // know the person's list) and is dropped on the drain, not kept forever.
    let queued = serve.tool("remember", json!({"fact": "Something to refuse-me later"}));
    assert_eq!(queued.first_text(), thock_bridge::QUEUED_SENTENCE);
    // A line the client can see is wrong never enters the queue.
    let refused = serve.tool("remember", json!({"fact": "- a bullet"}));
    assert!(refused.is_error);
    assert!(
        refused.first_text().contains("leading"),
        "{}",
        refused.first_text()
    );
    assert_eq!(client.outbox_lines(), 3);
    let stderr = serve.finish();
    assert!(stderr.contains("unreachable"), "{stderr}");

    // Still offline at the next start: the queue stays, in order.
    let mut serve = client.serve();
    let pong = serve.call("ping", json!({}));
    assert!(pong.error.is_none());
    serve.finish();
    assert_eq!(client.outbox_lines(), 3);

    // The desk is back: the start-up drain lands both lines, once, and
    // drops the one the desk refuses.
    let desk = Desk::start(port, vault.clone());
    let mut serve = client.serve();
    let pong = serve.call("ping", json!({}));
    assert!(pong.error.is_none());
    let context = serve.tool("context", json!({}));
    assert!(!context.first_text().contains("from cache"), "online again");
    let stderr = serve.finish();
    assert!(stderr.contains("2 queued line(s) landed"), "{stderr}");
    assert!(stderr.contains("refused a queued remember"), "{stderr}");
    assert_eq!(client.outbox_lines(), 0);
    {
        let vault = vault.lock().expect("lock");
        assert_eq!(vault.inbox.len(), 2, "{:?}", vault.inbox);
        assert!(vault.inbox[1].ends_with("Second fact, queued · via claude-code@robin"));
        assert!(
            vault.notes["daily/today.md"].ends_with("- claude-code@robin · Shipped it offline.\n")
        );
        assert_eq!(vault.seen.len(), 2);
    }

    // A reply lost in flight: the same call id again writes nothing.
    let (second_id, _) = {
        let vault = vault.lock().expect("lock");
        let second = vault.seen.keys().last().expect("an id").clone();
        (second, ())
    };
    let replay = JsonRpcRequest::new(
        json!(99),
        "tools/call",
        json!({"name": "remember", "arguments": {"fact": "Second fact, queued"}}),
    )
    .with_meta(&CallMeta {
        call_id: Some(second_id),
        date: None,
    });
    let response = thock_bridge::request(
        &thock_bridge::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/mcp"),
            headers: vec![
                ("Authorization".into(), format!("Bearer {TOKEN}")),
                ("Content-Type".into(), "application/json".into()),
            ],
            body: serde_json::to_vec(&replay).expect("json"),
        },
        Duration::from_secs(5),
    )
    .expect("replay");
    assert_eq!(response.status, 200);
    assert_eq!(
        vault.lock().expect("lock").inbox.len(),
        2,
        "no double write"
    );

    // A third start has nothing to drain and status reads the desk.
    let mut serve = client.serve();
    serve.call("ping", json!({}));
    let stderr = serve.finish();
    assert!(!stderr.contains("landed"), "{stderr}");
    let (ok, stdout, _) = client.run(&["status"]);
    assert!(ok);
    assert!(stdout.contains("Reachable: yes"), "{stdout}");
    assert!(stdout.contains("Outbox: 0 queued"), "{stdout}");
    assert!(
        stdout.contains("Refused by the desk: 1 line(s)"),
        "{stdout}"
    );

    // Mirror: a harness folder's text files go up under their harness.
    let mirror_dir = client.home.path().join("data/agents/claude-code");
    std::fs::create_dir_all(mirror_dir.join("topics")).expect("mirror dir");
    std::fs::write(mirror_dir.join("MEMORY.md"), "# index\n").expect("write");
    std::fs::write(mirror_dir.join("topics/build.md"), "cmake 3.31\n").expect("write");
    std::fs::write(mirror_dir.join("photo.png"), "png").expect("write");
    std::fs::write(mirror_dir.join("topics/secret.md"), "password: hunter2\n").expect("write");
    let (ok, stdout, stderr) = client.run(&["mirror"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("2 file(s) mirrored"), "{stdout}");
    assert!(
        stdout.contains("refused claude-code/topics/secret.md: 422"),
        "{stdout}"
    );
    assert!(stdout.contains("1 refused"), "{stdout}");
    {
        let vault = vault.lock().expect("lock");
        assert_eq!(vault.mirrored["claude-code/robin/MEMORY.md"], "# index\n");
        assert_eq!(
            vault.mirrored["claude-code/robin/topics/build.md"],
            "cmake 3.31\n"
        );
        assert_eq!(vault.mirrored.len(), 2);
    }
    let (ok, stdout, _) = client.run(&["mirror"]);
    assert!(ok);
    assert!(
        stdout.contains("0 file(s) mirrored"),
        "nothing newer: {stdout}"
    );

    // `update` fetches the skill; `connect --dry-run` only plans.
    let (ok, stdout, stderr) = client.run(&["update"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("Updated"), "{stdout}");
    let skill = std::fs::read_to_string(
        client
            .home
            .path()
            .join(".agents/skills/thock-memory/SKILL.md"),
    )
    .expect("skill");
    assert!(skill.contains("name: thock-memory"));
    let (ok, stdout, stderr) = client.run(&["connect", "claude", "--dry-run"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("claude mcp add --scope user --transport stdio thock --"),
        "{stdout}"
    );
    assert!(stdout.contains("autoMemoryDirectory"), "{stdout}");
    assert!(stdout.contains("dry run"), "{stdout}");
    assert!(!client.home.path().join(".claude/settings.json").exists());

    desk.stop();
}
