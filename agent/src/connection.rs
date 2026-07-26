use crate::cli::Config;
use crate::{tls_backend as tls, Error, Result};
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const MAX_HTTP_BODY: usize = 1024 * 1024;
const MAX_HTTP_RESPONSE: usize = MAX_HTTP_BODY + 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ConnectInfo {
    pub(crate) viewer_url: String,
    pub(crate) host_websocket_url: String,
}

#[derive(Clone, Debug)]
struct ParsedUrl {
    scheme: String,
    host: String,
    port: Option<u16>,
    path_and_query: String,
}

#[derive(Deserialize)]
struct CreateSessionResponse {
    #[serde(rename = "viewerUrl")]
    viewer_url: String,
    #[serde(rename = "hostWebSocketUrl")]
    host_websocket_url: String,
}

pub(crate) fn resolve_connection(config: &Config) -> Result<ConnectInfo> {
    let base = ParsedUrl::parse(&config.server)?;
    match base.scheme.as_str() {
        "ws" | "wss" => {
            if config.session.is_some() {
                return Err(Error::Message(
                    "-session is not supported with a direct websocket URL".into(),
                ));
            }
            Ok(ConnectInfo {
                viewer_url: viewer_url_from_websocket(&base),
                host_websocket_url: base.render(),
            })
        }
        "http" | "https" => {
            if let Some(session) = &config.session {
                Ok(ConnectInfo {
                    viewer_url: join_url(&base, &format!("/s/{session}"))?,
                    host_websocket_url: websocket_url(
                        &base,
                        &format!("/api/session/{session}/host"),
                    )?,
                })
            } else {
                create_session(&base)
            }
        }
        scheme => Err(Error::Message(format!(
            "unsupported server scheme: {scheme}"
        ))),
    }
}

fn create_session(base: &ParsedUrl) -> Result<ConnectInfo> {
    let body = http_request(
        "POST",
        &ParsedUrl::parse(&join_url(base, "/api/session")?)?,
        b"",
    )?;
    let created: CreateSessionResponse = serde_json::from_slice(&body)?;
    Ok(ConnectInfo {
        viewer_url: join_url(base, &created.viewer_url)?,
        host_websocket_url: websocket_url(base, &created.host_websocket_url)?,
    })
}

fn join_url(base: &ParsedUrl, path: &str) -> Result<String> {
    if path.starts_with("http://") || path.starts_with("https://") {
        return Ok(path.to_string());
    }
    Ok(base.with_path(path).render())
}

fn websocket_url(base: &ParsedUrl, path: &str) -> Result<String> {
    let mut next = ParsedUrl::parse(&join_url(base, path)?)?;
    next.scheme = if next.scheme == "https" { "wss" } else { "ws" }.to_string();
    Ok(next.render())
}

fn viewer_url_from_websocket(url: &ParsedUrl) -> String {
    let mut viewer = url.clone();
    viewer.scheme = if url.scheme == "wss" { "https" } else { "http" }.to_string();
    let path = viewer.path();
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    if parts.len() >= 3 {
        viewer.path_and_query = format!("/s/{}", parts[2]);
    }
    viewer.render()
}

fn http_request(method: &str, url: &ParsedUrl, body: &[u8]) -> Result<Vec<u8>> {
    let port = url.port_or_default()?;
    let target = request_target(url);
    let host_header = url.host_header(port);
    let request = format!(
            "{method} {target} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: ttys-agent\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
    let tcp = connect_tcp(&url.host, port)?;
    if url.scheme == "https" {
        let mut stream = tls::connect(tcp, url.tls_host())?;
        stream.write_all(request.as_bytes())?;
        stream.write_all(body)?;
        read_http_response(stream)
    } else {
        let mut stream = tcp;
        stream.write_all(request.as_bytes())?;
        stream.write_all(body)?;
        read_http_response(stream)
    }
}

pub(crate) fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addresses: Vec<_> = (host, port).to_socket_addrs()?.collect();
    if addresses.is_empty() {
        return Err(Error::Message(format!(
            "no addresses found for {host}:{port}"
        )));
    }

    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => {
                stream.set_nodelay(true)?;
                stream.set_read_timeout(Some(IO_TIMEOUT))?;
                stream.set_write_timeout(Some(IO_TIMEOUT))?;
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }

    match last_error {
        Some(error) => Err(Error::Io(error)),
        None => Err(Error::Message(format!(
            "no addresses found for {host}:{port}"
        ))),
    }
}

fn read_http_response(mut stream: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream
        .by_ref()
        .take((MAX_HTTP_RESPONSE + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_HTTP_RESPONSE {
        return Err(Error::Message("HTTP response too large".into()));
    }
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| Error::Message("invalid HTTP response".into()))?
        + 4;
    let status_line = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| Error::Message("invalid HTTP response headers".into()))?
        .lines()
        .next()
        .ok_or_else(|| Error::Message("invalid HTTP status line".into()))?;
    if parse_http_status(status_line)? != 200 {
        return Err(Error::Message(format!("request failed: {status_line}")));
    }
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
    let body = &bytes[header_end..];
    if headers.contains("transfer-encoding: chunked") {
        return decode_chunked_body(body);
    }
    if body.len() > MAX_HTTP_BODY {
        return Err(Error::Message("HTTP body too large".into()));
    }
    Ok(body.to_vec())
}

fn parse_http_status(status_line: &str) -> Result<u16> {
    let mut parts = status_line.split_whitespace();
    let version = parts
        .next()
        .ok_or_else(|| Error::Message("invalid HTTP status line".into()))?;
    if !version.starts_with("HTTP/") {
        return Err(Error::Message("invalid HTTP status line".into()));
    }
    parts
        .next()
        .ok_or_else(|| Error::Message("invalid HTTP status line".into()))?
        .parse::<u16>()
        .map_err(|_| Error::Message("invalid HTTP status code".into()))
}

fn decode_chunked_body(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut decoded = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| Error::Message("invalid chunked response".into()))?;
        let size_line = std::str::from_utf8(&body[..line_end]).unwrap_or_default();
        let size_text = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| Error::Message("invalid chunk size".into()))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(decoded);
        }
        if body.len() < size + 2 || &body[size..size + 2] != b"\r\n" {
            return Err(Error::Message("truncated chunked response".into()));
        }
        if decoded.len() + size > MAX_HTTP_BODY {
            return Err(Error::Message("HTTP body too large".into()));
        }
        decoded.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

fn request_target(url: &ParsedUrl) -> String {
    url.path_and_query.clone()
}

impl ParsedUrl {
    fn parse(value: &str) -> Result<Self> {
        let (scheme, rest) = value
            .split_once("://")
            .ok_or_else(|| Error::Message(format!("invalid URL: {value}")))?;
        if !matches!(scheme, "http" | "https" | "ws" | "wss") {
            return Err(Error::Message(format!("unsupported URL scheme: {scheme}")));
        }
        let rest = rest.split('#').next().unwrap_or(rest);
        let (authority, path_and_query) = match rest.find(['/', '?']) {
            Some(index) => (&rest[..index], normalize_path(&rest[index..])),
            None => (rest, "/".to_string()),
        };
        if authority.is_empty() || authority.contains('@') {
            return Err(Error::Message("invalid URL authority".into()));
        }
        let (host, port) = parse_authority(authority)?;
        Ok(Self {
            scheme: scheme.to_string(),
            host,
            port,
            path_and_query,
        })
    }

    fn render(&self) -> String {
        let mut output = format!("{}://{}", self.scheme, self.host);
        if let Some(port) = self.port {
            output.push(':');
            output.push_str(&port.to_string());
        }
        output.push_str(&self.path_and_query);
        output
    }

    fn with_path(&self, path: &str) -> Self {
        let mut next = self.clone();
        next.path_and_query = normalize_path(path);
        next
    }

    fn path(&self) -> &str {
        self.path_and_query.split('?').next().unwrap_or("/")
    }

    fn port_or_default(&self) -> Result<u16> {
        self.port
            .or(match self.scheme.as_str() {
                "http" | "ws" => Some(80),
                "https" | "wss" => Some(443),
                _ => None,
            })
            .ok_or_else(|| Error::Message("missing URL port".into()))
    }

    fn host_header(&self, port: u16) -> String {
        if self.port.is_some() {
            format!("{}:{port}", self.host)
        } else {
            self.host.clone()
        }
    }

    fn tls_host(&self) -> &str {
        self.host.trim_start_matches('[').trim_end_matches(']')
    }
}

fn parse_authority(authority: &str) -> Result<(String, Option<u16>)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .ok_or_else(|| Error::Message("invalid IPv6 host".into()))?;
        let port = if let Some(port) = rest.strip_prefix(':') {
            Some(parse_port(port)?)
        } else if rest.is_empty() {
            None
        } else {
            return Err(Error::Message("invalid URL authority".into()));
        };
        return Ok((format!("[{host}]"), port));
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(parse_port(port)?)),
        None => (authority, None),
    };
    if host.is_empty() || host.contains(':') || !host.is_ascii() {
        return Err(Error::Message("URL host must be non-empty ASCII".into()));
    }
    Ok((host.to_string(), port))
}

fn parse_port(value: &str) -> Result<u16> {
    value
        .parse::<u16>()
        .map_err(|_| Error::Message(format!("invalid URL port: {value}")))
}

fn normalize_path(value: &str) -> String {
    if value.starts_with('/') {
        value.to_string()
    } else {
        format!("/{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::TcpListener;
    use std::thread;

    fn local_listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[test]
    fn resolves_http_direct_websocket_and_session_connections() {
        let session_config = Config {
            server: "https://example.test:8443/base".to_string(),
            session: Some("abc-def".to_string()),
            shell: None,
        };
        assert_eq!(
            resolve_connection(&session_config).unwrap(),
            ConnectInfo {
                viewer_url: "https://example.test:8443/s/abc-def".to_string(),
                host_websocket_url: "wss://example.test:8443/api/session/abc-def/host".to_string(),
            }
        );

        let direct = Config {
            server: "ws://localhost:9000/api/session/abc-def/host".to_string(),
            session: None,
            shell: None,
        };
        assert_eq!(
            resolve_connection(&direct).unwrap(),
            ConnectInfo {
                viewer_url: "http://localhost:9000/s/abc-def".to_string(),
                host_websocket_url: direct.server,
            }
        );
    }

    #[test]
    fn parses_urls_safely_and_preserves_routes() {
        let parsed = ParsedUrl::parse("https://example.test:8443/base?q=1#ignored").unwrap();
        assert_eq!(parsed.render(), "https://example.test:8443/base?q=1");
        assert_eq!(parsed.path(), "/base");
        assert_eq!(parsed.host_header(8443), "example.test:8443");
        assert_eq!(
            websocket_url(&parsed, "/api/session/abc-def/host").unwrap(),
            "wss://example.test:8443/api/session/abc-def/host"
        );
        assert_eq!(
            join_url(&parsed, "https://other.test/path").unwrap(),
            "https://other.test/path"
        );

        let ipv6 = ParsedUrl::parse("http://[::1]:8080/api").unwrap();
        assert_eq!(ipv6.render(), "http://[::1]:8080/api");
        assert_eq!(ipv6.tls_host(), "::1");

        for invalid in [
            "ftp://example.test",
            "http://user@example.test",
            "http://example.test:not-a-port",
            "http://::1/without-brackets",
            "http://[::1",
        ] {
            assert!(
                ParsedUrl::parse(invalid).is_err(),
                "{invalid} should be rejected"
            );
        }
    }

    #[test]
    fn parses_http_responses_and_chunked_bodies() {
        assert_eq!(parse_http_status("HTTP/1.1 200 OK").unwrap(), 200);
        assert_eq!(parse_http_status("HTTP/2 204").unwrap(), 204);
        assert!(parse_http_status("200 OK").is_err());
        assert!(parse_http_status("HTTP/1.1 nope").is_err());

        assert_eq!(
            read_http_response(Cursor::new(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"
            ))
            .unwrap(),
            b"hello"
        );
        assert_eq!(
                read_http_response(Cursor::new(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4;extension=value\r\nttys\r\n2\r\n!!\r\n0\r\n\r\n"
                ))
                .unwrap(),
                b"ttys!!"
            );
        assert!(read_http_response(Cursor::new(b"HTTP/1.1 503 Unavailable\r\n\r\n")).is_err());
        assert!(decode_chunked_body(b"3\r\nab").is_err());
    }

    #[test]
    fn limits_http_body_size() {
        let oversized = vec![b'x'; MAX_HTTP_BODY + 1];
        assert!(read_http_response(Cursor::new(
            [b"HTTP/1.1 200 OK\r\n\r\n".as_slice(), oversized.as_slice()].concat()
        ))
        .is_err());
    }

    #[test]
    fn performs_http_request_against_a_local_server() {
        let (listener, port) = local_listener();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /api/session?source=test HTTP/1.1\r\n"));
            assert!(request.contains(&format!("Host: 127.0.0.1:{port}\r\n")));
            assert!(request.contains("User-Agent: ttys-agent\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
        });

        let url =
            ParsedUrl::parse(&format!("http://127.0.0.1:{port}/api/session?source=test")).unwrap();
        assert_eq!(http_request("POST", &url, b"").unwrap(), b"{}");
        server.join().unwrap();
    }
}
