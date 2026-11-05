//! `thock-bridge`: the client a harness launches to reach a Thock vault
//! (spec `thock/specs/v55-agents-bridge.md` §12). `serve` is the stdio MCP
//! proxy; the other subcommands pair, connect a harness, mirror its memory
//! and report. Logging goes to stderr only; stdout is the MCP channel.

use chrono::Local;
use serde_json::{Value, json};
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thock_bridge::{
    Cache, ClientConfig, HARNESS_HEADER, HttpRequest, JsonRpcRequest, JsonRpcResponse, MirrorState,
    Outbox, OutboxEntry, PROTOCOL_HEADER, PROTOCOL_VERSION, PairRequest, PairResponse,
    error_response, offline_suffix, plan_mirror, sanitize_harness,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const TIMEOUT: Duration = Duration::from_secs(8);
const SKILL_NAME: &str = "thock-memory";
const USAGE: &str = "thock-bridge — your other agents share the vault's memory

  thock-bridge pair <host:port> <code>          pair this machine with the desk
  thock-bridge serve [--harness <name>]         the stdio MCP server a harness launches
  thock-bridge connect <claude|codex|gemini> [--dry-run] [--yes]
                                                install the skill and register the server
  thock-bridge status                           desk reachability, cache age, outbox, mirror
  thock-bridge mirror                           one mirror pass of the harness memory folders
  thock-bridge update                           refresh the installed skill from the desk
";

/// Where the client keeps its config and state (§6.2). `THOCK_BRIDGE_HOME`
/// moves both, for tests and for a second identity on one machine.
struct Home {
    config_file: PathBuf,
    data_dir: PathBuf,
}

impl Home {
    fn resolve() -> Self {
        match std::env::var_os("THOCK_BRIDGE_HOME") {
            Some(home) => {
                let home = PathBuf::from(home);
                Self {
                    config_file: home.join("bridge.toml"),
                    data_dir: home.join("data"),
                }
            }
            None => Self {
                config_file: paths::config_dir().join("bridge.toml"),
                data_dir: paths::data_dir().join("bridge"),
            },
        }
    }

    fn cache(&self) -> Cache {
        Cache::new(self.data_dir.join("cache"))
    }

    fn outbox(&self) -> Outbox {
        Outbox::new(self.data_dir.join("outbox.jsonl"))
    }

    fn agents_dir(&self) -> PathBuf {
        self.data_dir.join("agents")
    }

    fn mirror_state_file(&self) -> PathBuf {
        self.data_dir.join("mirror-state.json")
    }

    fn load_config(&self) -> Option<ClientConfig> {
        let raw = std::fs::read_to_string(&self.config_file).ok()?;
        match ClientConfig::parse(&raw) {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!(
                    "thock-bridge: {} is not readable: {error}",
                    self.config_file.display()
                );
                None
            }
        }
    }

    fn save_config(&self, config: &ClientConfig) -> Result<(), String> {
        if let Some(parent) = self.config_file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&self.config_file, config.render()).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&self.config_file, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("pair") => pair(&args[1..]),
        Some("serve") => serve(&args[1..]),
        Some("connect") => connect(&args[1..]),
        Some("status") => status(),
        Some("mirror") => mirror_command(),
        Some("update") => update(),
        Some("--version") | Some("-V") => {
            println!("thock-bridge {VERSION}");
            Ok(())
        }
        _ => {
            eprint!("{USAGE}");
            Err(String::new())
        }
    };
    if let Err(error) = code {
        if !error.is_empty() {
            eprintln!("thock-bridge: {error}");
        }
        std::process::exit(1);
    }
}

fn hostname() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .split('.')
        .next()
        .unwrap_or("this-machine")
        .to_string()
}

fn desk_url(desk: &str, path: &str) -> String {
    let desk = desk.trim().trim_end_matches('/');
    let desk = desk.strip_prefix("http://").unwrap_or(desk);
    format!("http://{desk}{path}")
}

/// A request to the desk with the paired token.
fn desk_request(
    config: &ClientConfig,
    method: &str,
    path: &str,
    body: Vec<u8>,
    extra: Vec<(String, String)>,
) -> HttpRequest {
    let mut headers = vec![
        (
            "Authorization".to_string(),
            format!("Bearer {}", config.token),
        ),
        ("Content-Type".to_string(), "application/json".to_string()),
        (PROTOCOL_HEADER.to_string(), PROTOCOL_VERSION.to_string()),
    ];
    headers.extend(extra);
    HttpRequest {
        method: method.to_string(),
        url: desk_url(&config.desk, path),
        headers,
        body,
    }
}

// --- pair ---

fn pair(args: &[String]) -> Result<(), String> {
    let [desk, code] = args else {
        return Err("usage: thock-bridge pair <host:port> <code>".into());
    };
    let home = Home::resolve();
    let body = serde_json::to_vec(&PairRequest {
        code: code.trim().to_string(),
        name: hostname(),
        version: VERSION.to_string(),
    })
    .map_err(|error| error.to_string())?;
    let response = thock_bridge::request(
        &HttpRequest {
            method: "POST".into(),
            url: desk_url(desk, "/v1/pair"),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body,
        },
        TIMEOUT,
    )?;
    if !response.is_success() {
        return Err(match response.status {
            410 => {
                "that code was already used or has expired; run Connect an agent on the desk again"
                    .into()
            }
            403 => "the desk refused the code; check it and try again".into(),
            other => format!("the desk answered {other}: {}", response.text().trim()),
        });
    }
    let paired: PairResponse = serde_json::from_slice(&response.body)
        .map_err(|error| format!("the desk's answer wasn't readable: {error}"))?;
    let config = ClientConfig {
        desk: desk.trim().trim_start_matches("http://").to_string(),
        token: paired.token,
        vault: paired.vault.clone(),
        client_name: hostname(),
    };
    home.save_config(&config)?;
    println!(
        "Paired with {} on {}. Next: thock-bridge connect claude|codex|gemini",
        paired.vault, paired.desk
    );
    Ok(())
}

// --- serve ---

struct Proxy {
    home: Home,
    config: Option<ClientConfig>,
    cache: Cache,
    outbox: Outbox,
    harness: String,
    /// Set once the desk answers, cleared when it stops; drives the log
    /// lines so a flapping desk is visible without a line per call.
    online: Option<bool>,
}

impl Proxy {
    fn desk_label(&self) -> String {
        self.config
            .as_ref()
            .map(|config| config.desk.clone())
            .unwrap_or_else(|| "(not paired)".into())
    }

    fn note_online(&mut self, online: bool) {
        if self.online != Some(online) {
            eprintln!(
                "thock-bridge: the desk at {} is {}",
                self.desk_label(),
                if online {
                    "reachable"
                } else {
                    "unreachable; answering from cache"
                }
            );
            self.online = Some(online);
        }
    }

    fn harness_header(&self) -> Vec<(String, String)> {
        vec![(HARNESS_HEADER.to_string(), self.harness.clone())]
    }

    /// Forwards one request to the desk. `Err` means the desk did not
    /// answer usefully, which is the cue for the offline path.
    fn forward(&self, request: &JsonRpcRequest) -> Result<JsonRpcResponse, String> {
        let config = self.config.as_ref().ok_or_else(|| {
            "not paired with a desk yet; run `thock-bridge pair <host:port> <code>`".to_string()
        })?;
        let body = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        let http = desk_request(config, "POST", "/mcp", body, self.harness_header());
        let response = thock_bridge::request(&http, TIMEOUT)?;
        if response.status >= 500 {
            return Err(format!("the desk answered {}", response.status));
        }
        if response.status == 401 {
            return Ok(error_response(
                request.id.clone().unwrap_or(Value::Null),
                -32001,
                "the desk no longer knows this machine; pair again from the desk",
            ));
        }
        if response.status == 202 {
            return Err("the desk accepted a notification without a reply".into());
        }
        JsonRpcResponse::parse(&response.text())
            .map_err(|error| format!("the desk's answer wasn't readable: {error}"))
    }

    fn drain_outbox(&mut self) {
        if self.outbox.is_empty() || self.config.is_none() {
            return;
        }
        let outbox = self.outbox.clone();
        let result = outbox.drain(|entry| {
            let request = JsonRpcRequest::new(
                json!(format!("outbox-{}", entry.id)),
                "tools/call",
                json!({"name": entry.tool, "arguments": entry.args}),
            )
            .with_meta(&thock_bridge::CallMeta {
                call_id: Some(entry.id.clone()),
                date: entry.date(),
            });
            let response = self.forward(&request)?;
            match response.error {
                Some(error) => Err(error.message),
                None => Ok(()),
            }
        });
        match result {
            Ok(sent) if sent > 0 => {
                eprintln!("thock-bridge: {sent} queued line(s) landed on the desk");
                self.note_online(true);
            }
            Ok(_) => {}
            Err(stop) => {
                eprintln!(
                    "thock-bridge: a queued {} is still waiting: {}",
                    stop.entry.tool, stop.reason
                );
            }
        }
    }

    fn mirror(&mut self) {
        if let Some(config) = &self.config {
            match mirror_pass(&self.home, config, false) {
                Ok(count) if count > 0 => eprintln!("thock-bridge: mirrored {count} file(s)"),
                Ok(_) => {}
                Err(error) => eprintln!("thock-bridge: mirror skipped: {error}"),
            }
        }
    }

    fn handle(&mut self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        if request.method == "initialize"
            && self.harness == "agent"
            && let Some(name) = request
                .params
                .as_ref()
                .and_then(|params| params.get("clientInfo"))
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str)
        {
            self.harness = sanitize_harness(name);
        }
        let id = request.id.clone()?;
        match request.method.as_str() {
            "initialize" | "ping" | "tools/list" | "resources/list" => {
                pollster_free_local(request, VERSION)
            }
            "tools/call" | "resources/read" => {
                self.drain_outbox();
                let tool = request.tool_name().map(str::to_string);
                let arguments = request.arguments();
                let is_write = matches!(tool.as_deref(), Some("remember" | "note_today"));
                let request = if is_write {
                    request.with_meta(&thock_bridge::CallMeta {
                        call_id: Some(uuid::Uuid::new_v4().to_string()),
                        date: Some(Local::now().date_naive()),
                    })
                } else {
                    request
                };
                match self.forward(&request) {
                    Ok(response) => {
                        self.note_online(true);
                        self.remember_answer(&request, tool.as_deref(), &arguments, &response);
                        if is_write {
                            self.mirror();
                        }
                        Some(response)
                    }
                    Err(reason) => {
                        self.note_online(false);
                        Some(self.offline_answer(
                            id,
                            &request,
                            tool.as_deref(),
                            &arguments,
                            &reason,
                        ))
                    }
                }
            }
            other => Some(error_response(
                id,
                thock_bridge::METHOD_NOT_FOUND,
                &format!("{other} is not served here"),
            )),
        }
    }

    /// Keeps what the desk said, so the next outage can answer (§9.4).
    fn remember_answer(
        &self,
        request: &JsonRpcRequest,
        tool: Option<&str>,
        arguments: &Value,
        response: &JsonRpcResponse,
    ) {
        if request.method == "resources/read" {
            let uri = request
                .params
                .as_ref()
                .and_then(|params| params.get("uri"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let text = response
                .result
                .as_ref()
                .and_then(|result| result.get("contents"))
                .and_then(|contents| contents.get(0))
                .and_then(|first| first.get("text"))
                .and_then(Value::as_str);
            if let Some(text) = text {
                let stored = if uri == thock_bridge::INDEX_RESOURCE_URI {
                    self.cache.store_index(text)
                } else if let Some(path) = uri.strip_prefix(thock_bridge::NOTE_RESOURCE_PREFIX) {
                    self.cache.store_note(path, text)
                } else {
                    Ok(())
                };
                if let Err(error) = stored {
                    eprintln!("thock-bridge: couldn't cache {uri}: {error}");
                }
            }
            return;
        }
        let Some(result) = response.tool_result() else {
            return;
        };
        if result.is_error {
            return;
        }
        let text = result.first_text();
        let stored = match tool {
            Some("context") => self.cache.store_context(text),
            Some("read") => match arguments.get("path").and_then(Value::as_str) {
                Some(path) => self.cache.store_note(path.trim(), text),
                None => Ok(()),
            },
            Some("search") => match search_key(arguments) {
                Some(key) => self.cache.store_search(&key, text),
                None => Ok(()),
            },
            _ => Ok(()),
        };
        if let Err(error) = stored {
            eprintln!("thock-bridge: couldn't write the cache: {error}");
        }
    }

    /// The desk is away: reads come from the cache, marked as such; writes
    /// queue; the rest says what is missing and names the desk (G3, G7).
    fn offline_answer(
        &self,
        id: Value,
        request: &JsonRpcRequest,
        tool: Option<&str>,
        arguments: &Value,
        reason: &str,
    ) -> JsonRpcResponse {
        let desk = self.desk_label();
        let from_cache = |cached: Option<thock_bridge::CachedText>, what: &str| match cached {
            Some(cached) => thock_bridge::CallToolResponse::text(format!(
                "{}{}",
                cached.text,
                offline_suffix(&cached.as_of, &desk)
            )),
            None => thock_bridge::CallToolResponse::error(format!(
                "{what} isn't in the cache and the desk at {desk} is unreachable ({reason})"
            )),
        };
        if request.method == "resources/read" {
            let uri = request
                .params
                .as_ref()
                .and_then(|params| params.get("uri"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let cached = if uri == thock_bridge::INDEX_RESOURCE_URI {
                self.cache.index()
            } else {
                uri.strip_prefix(thock_bridge::NOTE_RESOURCE_PREFIX)
                    .and_then(|path| self.cache.note(path))
            };
            return match cached {
                Some(cached) => thock_bridge::result_response(
                    id,
                    json!({"contents": [{"uri": uri, "mimeType": "text/markdown", "text": format!("{}{}", cached.text, offline_suffix(&cached.as_of, &desk))}]}),
                ),
                None => error_response(
                    id,
                    thock_bridge::INVALID_PARAMS,
                    &format!("{uri} isn't in the cache and the desk at {desk} is unreachable"),
                ),
            };
        }
        let answer = match tool {
            Some("context") => from_cache(self.cache.context(), "the vault's context"),
            Some("read") => match arguments.get("path").and_then(Value::as_str) {
                Some(path) => from_cache(self.cache.note(path.trim()), path.trim()),
                None => thock_bridge::CallToolResponse::error(format!(
                    "a note can only be found by title when the desk at {desk} is reachable"
                )),
            },
            Some("search") => from_cache(
                search_key(arguments).and_then(|key| self.cache.search(&key)),
                "that search",
            ),
            Some("recent") => thock_bridge::CallToolResponse::error(format!(
                "recent needs the desk, and the desk at {desk} is unreachable ({reason})"
            )),
            Some(name @ ("remember" | "note_today")) => {
                let entry = OutboxEntry::new(name, arguments.clone());
                match self.outbox.push(&entry) {
                    Ok(()) => thock_bridge::CallToolResponse::text(thock_bridge::QUEUED_SENTENCE),
                    Err(error) => thock_bridge::CallToolResponse::error(format!(
                        "the desk at {desk} is unreachable and the line couldn't be queued: {error}"
                    )),
                }
            }
            Some(other) => thock_bridge::CallToolResponse::error(format!("no tool named {other}")),
            None => {
                return error_response(
                    id,
                    thock_bridge::INVALID_PARAMS,
                    "tools/call needs a tool name",
                );
            }
        };
        thock_bridge::result_response(id, json!(answer))
    }
}

/// `initialize`, `ping` and the lists need no host, so the library's
/// dispatcher answers them with a host that is never called.
fn pollster_free_local(request: JsonRpcRequest, version: &str) -> Option<JsonRpcResponse> {
    let id = request.id.clone()?;
    Some(match request.method.as_str() {
        "initialize" => thock_bridge::result_response(id, thock_bridge::initialize_result(version)),
        "ping" => thock_bridge::result_response(id, json!({})),
        "tools/list" => thock_bridge::result_response(id, json!({"tools": thock_bridge::tools()})),
        "resources/list" => {
            thock_bridge::result_response(id, json!({"resources": thock_bridge::resources()}))
        }
        other => error_response(
            id,
            thock_bridge::METHOD_NOT_FOUND,
            &format!("{other} is not served here"),
        ),
    })
}

fn search_key(arguments: &Value) -> Option<String> {
    let query = arguments.get("query")?.as_str()?.trim().to_string();
    if query.is_empty() {
        return None;
    }
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| usize::try_from(limit).unwrap_or(50))
        .unwrap_or(10)
        .clamp(1, 50);
    let since = arguments
        .get("since")
        .and_then(Value::as_str)
        .and_then(|since| chrono::NaiveDate::parse_from_str(since.trim(), "%Y-%m-%d").ok());
    let folder = arguments
        .get("folder")
        .and_then(Value::as_str)
        .map(|folder| folder.trim().trim_matches('/').to_string())
        .filter(|folder| !folder.is_empty());
    Some(
        thock_bridge::SearchQuery {
            query,
            limit,
            since,
            folder,
        }
        .cache_key(),
    )
}

fn serve(args: &[String]) -> Result<(), String> {
    let mut harness = "agent".to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--harness" => {
                harness = sanitize_harness(args.get(index + 1).map(String::as_str).unwrap_or(""));
                index += 2;
            }
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    let home = Home::resolve();
    let config = home.load_config();
    if config.is_none() {
        eprintln!(
            "thock-bridge: not paired yet; tools will answer so until `thock-bridge pair` runs"
        );
    }
    let mut proxy = Proxy {
        cache: home.cache(),
        outbox: home.outbox(),
        home,
        config,
        harness,
        online: None,
    };
    proxy.drain_outbox();
    proxy.mirror();

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        let read = stdin
            .lock()
            .read_line(&mut line)
            .map_err(|error| format!("stdin closed badly: {error}"))?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match JsonRpcRequest::parse(trimmed) {
            Ok(request) => proxy.handle(request),
            Err(error) => Some(error),
        };
        if let Some(response) = response {
            let mut out = stdout.lock();
            writeln!(out, "{}", response.to_json())
                .and_then(|()| out.flush())
                .map_err(|error| format!("stdout closed: {error}"))?;
        }
    }
    proxy.mirror();
    Ok(())
}

// --- mirror ---

/// Uploads every harness folder's newer files (§9.6). Returns how many went.
fn mirror_pass(home: &Home, config: &ClientConfig, verbose: bool) -> Result<usize, String> {
    let agents_dir = home.agents_dir();
    let Ok(harnesses) = std::fs::read_dir(&agents_dir) else {
        return Ok(0);
    };
    let state_file = home.mirror_state_file();
    let mut state = MirrorState::load(&state_file);
    let mut count = 0;
    for harness in harnesses.flatten() {
        let Some(name) = harness.file_name().to_str().map(sanitize_harness) else {
            continue;
        };
        if !harness.path().is_dir() {
            continue;
        }
        let scoped = MirrorState {
            uploaded: state
                .uploaded
                .iter()
                .filter_map(|(key, value)| {
                    key.strip_prefix(&format!("{name}/"))
                        .map(|rest| (rest.to_string(), *value))
                })
                .collect(),
        };
        for file in plan_mirror(&harness.path(), &scoped) {
            let bytes = std::fs::read(&file.absolute).map_err(|error| error.to_string())?;
            let path = file
                .relative
                .split('/')
                .map(thock_bridge::percent_encode)
                .collect::<Vec<_>>()
                .join("/");
            let request = desk_request(
                config,
                "PUT",
                &format!("/v1/agents/{}/{path}", thock_bridge::percent_encode(&name)),
                bytes,
                vec![("Content-Type".into(), "application/octet-stream".into())],
            );
            let response = thock_bridge::request(&request, TIMEOUT)?;
            if !response.is_success() {
                return Err(format!(
                    "the desk refused {}/{}: {} {}",
                    name,
                    file.relative,
                    response.status,
                    response.text().trim()
                ));
            }
            state.record(&format!("{name}/{}", file.relative), file.modified_secs);
            state.save(&state_file).map_err(|error| error.to_string())?;
            count += 1;
            if verbose {
                println!("mirrored {name}/{}", file.relative);
            }
        }
    }
    Ok(count)
}

fn mirror_command() -> Result<(), String> {
    let home = Home::resolve();
    let config = home
        .load_config()
        .ok_or("not paired; run `thock-bridge pair` first")?;
    let count = mirror_pass(&home, &config, true)?;
    println!(
        "{count} file(s) mirrored into {}'s memory/agents/",
        config.vault
    );
    Ok(())
}

// --- status ---

fn status() -> Result<(), String> {
    let home = Home::resolve();
    let Some(config) = home.load_config() else {
        println!(
            "Not paired. Run `thock-bridge pair <host:port> <code>` with the code from Connect an agent on the desk."
        );
        return Ok(());
    };
    println!("Desk: {} (vault {})", config.desk, config.vault);
    let health = thock_bridge::request(
        &desk_request(&config, "GET", "/v1/health", Vec::new(), Vec::new()),
        Duration::from_secs(3),
    );
    match health {
        Ok(response) if response.is_success() => {
            let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
            println!(
                "Reachable: yes (Thock {} on {}, serving since {})",
                body["version"].as_str().unwrap_or("?"),
                body["desk"].as_str().unwrap_or("?"),
                body["serving_since"].as_str().unwrap_or("?")
            );
        }
        Ok(response) if response.status == 401 => {
            println!("Reachable: yes, but the desk no longer knows this machine; pair again");
        }
        Ok(response) => println!("Reachable: answered {}", response.status),
        Err(error) => println!("Reachable: no ({error})"),
    }
    match home.cache().age() {
        Some(age) => println!("Cache: context as of {age}"),
        None => println!("Cache: empty"),
    }
    let outbox = home.outbox();
    println!("Outbox: {} queued line(s)", outbox.len());
    let state = MirrorState::load(&home.mirror_state_file());
    println!(
        "Mirror: {} file(s) recorded from {}",
        state.uploaded.len(),
        home.agents_dir().display()
    );
    Ok(())
}

// --- connect / update ---

struct Harness {
    id: &'static str,
    skills_dir: &'static str,
    label: &'static str,
}

fn harness_for(name: &str) -> Option<Harness> {
    Some(match name {
        "claude" | "claude-code" => Harness {
            id: "claude-code",
            skills_dir: ".claude/skills",
            label: "Claude Code",
        },
        "codex" => Harness {
            id: "codex",
            skills_dir: ".codex/skills",
            label: "Codex",
        },
        "gemini" => Harness {
            id: "gemini",
            skills_dir: ".gemini/skills",
            label: "Gemini CLI",
        },
        _ => return None,
    })
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home_dir().clone())
}

fn canonical_skill_file() -> PathBuf {
    home_dir()
        .join(".agents/skills")
        .join(SKILL_NAME)
        .join("SKILL.md")
}

fn fetch_skill(config: &ClientConfig) -> Result<String, String> {
    let response = thock_bridge::request(
        &desk_request(config, "GET", "/v1/skill", Vec::new(), Vec::new()),
        TIMEOUT,
    )?;
    if !response.is_success() {
        return Err(format!(
            "the desk answered {} for the skill; is the Agents Routine still installed?",
            response.status
        ));
    }
    Ok(response.text())
}

/// The plan `connect` prints before touching anything.
struct Step {
    describe: String,
    apply: Box<dyn FnOnce() -> Result<(), String>>,
}

fn confirm(prompt: &str, yes: bool) -> Result<bool, String> {
    if yes {
        return Ok(true);
    }
    eprint!("{prompt} [y/N] ");
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    Ok(answer.trim().eq_ignore_ascii_case("y"))
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, text).map_err(|error| error.to_string())
}

fn link_skill(target_dir: &Path) -> Result<(), String> {
    let link = target_dir.join(SKILL_NAME);
    let canonical = canonical_skill_file()
        .parent()
        .map(Path::to_path_buf)
        .ok_or("no skill folder")?;
    if link.symlink_metadata().is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(target_dir).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&canonical, &link).map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = canonical;
        Err("symlinks are not supported here; copy the skill folder by hand".into())
    }
}

fn mcp_add_command(harness: &Harness, binary: &str) -> Vec<String> {
    let serve = ["serve", "--harness", harness.id];
    match harness.id {
        "claude-code" => [
            "claude",
            "mcp",
            "add",
            "--scope",
            "user",
            "--transport",
            "stdio",
            "thock",
            "--",
            binary,
        ]
        .into_iter()
        .chain(serve)
        .map(String::from)
        .collect(),
        "codex" => ["codex", "mcp", "add", "thock", "--", binary]
            .into_iter()
            .chain(serve)
            .map(String::from)
            .collect(),
        _ => ["gemini", "mcp", "add", "-s", "user", "thock", binary]
            .into_iter()
            .chain(serve)
            .map(String::from)
            .collect(),
    }
}

/// What to add by hand when the CLI's own command isn't available.
fn manual_block(harness: &Harness, binary: &str) -> String {
    match harness.id {
        "claude-code" => format!(
            "~/.claude.json (user scope):\n{}",
            serde_json::to_string_pretty(&json!({"mcpServers": {"thock": {"type": "stdio", "command": binary, "args": ["serve", "--harness", "claude-code"]}}})).unwrap_or_default()
        ),
        "codex" => format!(
            "~/.codex/config.toml:\n[mcp_servers.thock]\ncommand = \"{binary}\"\nargs = [\"serve\", \"--harness\", \"codex\"]\n"
        ),
        _ => format!(
            "~/.gemini/settings.json:\n{}",
            serde_json::to_string_pretty(&json!({"mcpServers": {"thock": {"command": binary, "args": ["serve", "--harness", "gemini"]}}})).unwrap_or_default()
        ),
    }
}

fn connect(args: &[String]) -> Result<(), String> {
    let mut dry_run = false;
    let mut yes = false;
    let mut name = None;
    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--yes" | "-y" => yes = true,
            other if name.is_none() => name = Some(other.to_string()),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let name =
        name.ok_or("usage: thock-bridge connect <claude|codex|gemini> [--dry-run] [--yes]")?;
    let harness = harness_for(&name).ok_or_else(|| {
        format!("{name} isn't a harness this command knows; the skill and the server work from any MCP client, see the printed config block")
    })?;
    let home = Home::resolve();
    let config = home
        .load_config()
        .ok_or("not paired; run `thock-bridge pair` first")?;
    let skill = fetch_skill(&config)?;
    let binary = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "thock-bridge".to_string());

    let mut steps: Vec<Step> = Vec::new();
    let canonical = canonical_skill_file();
    steps.push(Step {
        describe: format!("write {}", canonical.display()),
        apply: Box::new({
            let canonical = canonical.clone();
            move || write_text(&canonical, &skill)
        }),
    });
    let skills_dir = home_dir().join(harness.skills_dir);
    steps.push(Step {
        describe: format!(
            "link {} -> {}",
            skills_dir.join(SKILL_NAME).display(),
            canonical
                .parent()
                .map(Path::display)
                .map(|p| p.to_string())
                .unwrap_or_default()
        ),
        apply: Box::new(move || link_skill(&skills_dir)),
    });
    let command = mcp_add_command(&harness, &binary);
    let block = manual_block(&harness, &binary);
    steps.push(Step {
        describe: format!("run `{}`", command.join(" ")),
        apply: Box::new(move || {
            let status = std::process::Command::new(&command[0])
                .args(&command[1..])
                .status();
            match status {
                Ok(status) if status.success() => Ok(()),
                Ok(status) => {
                    eprintln!(
                        "the command exited with {status}; add this by hand instead:\n{block}"
                    );
                    Ok(())
                }
                Err(error) => {
                    eprintln!(
                        "couldn't run {}: {error}; add this by hand instead:\n{block}",
                        command[0]
                    );
                    Ok(())
                }
            }
        }),
    });
    if harness.id == "claude-code" {
        let mirror_dir = home.agents_dir().join(harness.id);
        let settings = home_dir().join(".claude/settings.json");
        steps.push(Step {
            describe: format!(
                "set autoMemoryDirectory = {} in {}",
                mirror_dir.display(),
                settings.display()
            ),
            apply: Box::new(move || set_auto_memory_directory(&settings, &mirror_dir)),
        });
    } else {
        let mirror_dir = home.agents_dir().join(harness.id);
        steps.push(Step {
            describe: format!(
                "create the mirror folder {} (point {}'s memory there to mirror it)",
                mirror_dir.display(),
                harness.label
            ),
            apply: Box::new(move || {
                std::fs::create_dir_all(&mirror_dir).map_err(|error| error.to_string())
            }),
        });
    }

    println!("Connecting {} to {}:", harness.label, config.vault);
    for step in &steps {
        println!("  - {}", step.describe);
    }
    if dry_run {
        println!("(dry run: nothing changed)");
        return Ok(());
    }
    for step in steps {
        if confirm(&format!("{}?", step.describe), yes)? {
            (step.apply)()?;
            println!("  done: {}", step.describe);
        } else {
            println!("  skipped: {}", step.describe);
        }
    }
    println!(
        "Done. Start {} anywhere and it has the `thock` tools; the skill tells it when to use them.",
        harness.label
    );
    Ok(())
}

fn set_auto_memory_directory(settings: &Path, mirror_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(mirror_dir).map_err(|error| error.to_string())?;
    let mut value: Value = match std::fs::read_to_string(settings) {
        Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw)
            .map_err(|error| format!("{} isn't valid JSON: {error}", settings.display()))?,
        _ => json!({}),
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| format!("{} isn't a JSON object", settings.display()))?;
    object.insert(
        "autoMemoryDirectory".into(),
        Value::String(mirror_dir.display().to_string()),
    );
    let text = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    write_text(settings, &format!("{text}\n"))
}

fn update() -> Result<(), String> {
    let home = Home::resolve();
    let config = home
        .load_config()
        .ok_or("not paired; run `thock-bridge pair` first")?;
    let skill = fetch_skill(&config)?;
    let canonical = canonical_skill_file();
    write_text(&canonical, &skill)?;
    println!("Updated {}", canonical.display());
    Ok(())
}
