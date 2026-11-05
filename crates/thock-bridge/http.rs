//! The little HTTP the bridge needs (spec §7): the desk's routes, bearer
//! auth, and a blocking client over `std::net` for the binary. Plain HTTP
//! on the local network is the V1 contract, so no TLS stack and no async
//! runtime come along; the desk serves with `tiny_http` and sets
//! `Content-Length`, so the client reads to end of stream.

use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs as _};
use std::time::Duration;

/// Which desk route a request names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Pair,
    Health,
    Mcp,
    McpNotPost,
    Skill,
    /// `PUT /v1/agents/{harness}/{path}`: the harness and the path under
    /// its mirror folder, still to be validated by the host.
    AgentFile {
        harness: String,
        path: String,
    },
    NotFound,
}

pub fn parse_route(method: &str, url: &str) -> Route {
    let path = url.split('?').next().unwrap_or(url);
    let path = path.trim_end_matches('/');
    match (method, path) {
        ("POST", "/v1/pair") => Route::Pair,
        ("GET", "/v1/health") => Route::Health,
        ("POST", "/mcp") => Route::Mcp,
        (_, "/mcp") => Route::McpNotPost,
        ("GET", "/v1/skill") => Route::Skill,
        ("PUT", rest) if rest.starts_with("/v1/agents/") => {
            let rest = &rest["/v1/agents/".len()..];
            match rest.split_once('/') {
                Some((harness, path)) if !harness.is_empty() && !path.is_empty() => {
                    Route::AgentFile {
                        harness: percent_decode(harness),
                        path: percent_decode(path),
                    }
                }
                _ => Route::NotFound,
            }
        }
        _ => Route::NotFound,
    }
}

/// The token after `Bearer ` in an `Authorization` header, if any.
pub fn bearer_token<'a>(headers: impl Iterator<Item = (&'a str, &'a str)>) -> Option<&'a str> {
    headers
        .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .find_map(|(_, value)| {
            let value = value.trim();
            let (scheme, token) = value.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then(|| token.trim())
                .filter(|token| !token.is_empty())
        })
}

/// The JSON error body the desk answers with.
pub fn response_json(error: &str) -> String {
    serde_json::json!({ "error": error }).to_string()
}

pub fn percent_encode(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &text[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// One blocking round trip. `url` is `http://host:port/path`; anything else
/// is refused. A connection failure, a timeout or a server error above 499
/// is an `Err`, which is what the client's offline path keys on.
pub fn request(request: &HttpRequest, timeout: Duration) -> Result<HttpResponse, String> {
    let rest = request
        .url
        .strip_prefix("http://")
        .ok_or_else(|| format!("{} is not an http:// address", request.url))?;
    let (authority, target) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            port.parse::<u16>()
                .map_err(|_| format!("{authority} has no usable port"))?,
        ),
        None => (authority, 80),
    };
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("{host} could not be resolved: {error}"))?
        .next()
        .ok_or_else(|| format!("{host} could not be resolved"))?;
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .map_err(|error| format!("couldn't reach {host}:{port}: {error}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    let mut head = format!(
        "{} {target} HTTP/1.0\r\nHost: {host}:{port}\r\nConnection: close\r\n",
        request.method
    );
    for (name, value) in &request.headers {
        if !name.eq_ignore_ascii_case("content-length") && !name.eq_ignore_ascii_case("host") {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", request.body.len()));
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(&request.body);
    stream
        .write_all(&bytes)
        .map_err(|error| format!("couldn't send to {host}:{port}: {error}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|error| format!("no answer from {host}:{port}: {error}"))?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Result<HttpResponse, String> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "the desk's answer had no headers".to_string())?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "the desk's answer had no status".to_string())?;
    let mut body = raw[split + 4..].to_vec();
    let chunked = head.lines().any(|line| {
        line.to_ascii_lowercase()
            .replace(' ', "")
            .starts_with("transfer-encoding:chunked")
    });
    if chunked {
        body = dechunk(&body);
    }
    Ok(HttpResponse { status, body })
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < body.len() {
        let Some(line_end) = body[index..]
            .windows(2)
            .position(|window| window == b"\r\n")
        else {
            break;
        };
        let size_line = String::from_utf8_lossy(&body[index..index + line_end]);
        let size = size_line
            .split(';')
            .next()
            .and_then(|hex| usize::from_str_radix(hex.trim(), 16).ok())
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = index + line_end + 2;
        let end = (start + size).min(body.len());
        out.extend_from_slice(&body[start..end]);
        index = end + 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_matched_by_method_and_path() {
        assert_eq!(parse_route("POST", "/v1/pair"), Route::Pair);
        assert_eq!(parse_route("GET", "/v1/health?x=1"), Route::Health);
        assert_eq!(parse_route("POST", "/mcp"), Route::Mcp);
        assert_eq!(parse_route("GET", "/mcp"), Route::McpNotPost);
        assert_eq!(parse_route("DELETE", "/mcp/"), Route::McpNotPost);
        assert_eq!(parse_route("GET", "/v1/skill"), Route::Skill);
        assert_eq!(
            parse_route("PUT", "/v1/agents/claude-code/MEMORY.md"),
            Route::AgentFile {
                harness: "claude-code".into(),
                path: "MEMORY.md".into()
            }
        );
        assert_eq!(
            parse_route("PUT", "/v1/agents/codex/notes/a%20b.md"),
            Route::AgentFile {
                harness: "codex".into(),
                path: "notes/a b.md".into()
            }
        );
        assert_eq!(parse_route("PUT", "/v1/agents/codex"), Route::NotFound);
        assert_eq!(parse_route("GET", "/v1/agents/codex/x.md"), Route::NotFound);
        assert_eq!(parse_route("GET", "/"), Route::NotFound);
    }

    #[test]
    fn bearer_tokens_are_extracted() {
        let headers = [("Authorization", "Bearer abc"), ("X", "y")];
        assert_eq!(bearer_token(headers.iter().copied()), Some("abc"));
        let headers = [("authorization", "bearer   abc ")];
        assert_eq!(bearer_token(headers.iter().copied()), Some("abc"));
        let headers = [("Authorization", "Basic abc")];
        assert_eq!(bearer_token(headers.iter().copied()), None);
        let headers = [("Authorization", "Bearer ")];
        assert_eq!(bearer_token(headers.iter().copied()), None);
    }

    #[test]
    fn percent_coding_round_trips() {
        let original = "notes/a b/ç.md";
        assert_eq!(percent_decode(&percent_encode(original)), original);
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn responses_are_parsed_with_and_without_chunking() {
        let raw = b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok";
        assert_eq!(
            parse_response(raw).unwrap(),
            HttpResponse {
                status: 201,
                body: b"ok".to_vec()
            }
        );
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap().body, b"abcde");
        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn a_closed_port_is_an_error_not_a_panic() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let result = request(
            &HttpRequest {
                method: "GET".into(),
                url: format!("http://127.0.0.1:{port}/v1/health"),
                headers: Vec::new(),
                body: Vec::new(),
            },
            Duration::from_millis(500),
        );
        assert!(result.unwrap_err().contains("couldn't reach"));
        assert!(
            request(
                &HttpRequest {
                    method: "GET".into(),
                    url: "https://example.com/".into(),
                    headers: Vec::new(),
                    body: Vec::new(),
                },
                Duration::from_millis(500),
            )
            .unwrap_err()
            .contains("not an http:// address")
        );
    }
}
