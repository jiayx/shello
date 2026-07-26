fn main() {
    if let Err(error) = app::run() {
        eprintln!("ttys-agent: {error}");
        std::process::exit(1);
    }
}

mod app {
    #[cfg(unix)]
    #[path = "platform.rs"]
    mod platform;
    #[cfg(windows)]
    #[path = "platform_windows.rs"]
    mod platform;

    use self::platform::{default_shell, terminal_size, Pty, PtyHandle, RawTerminal, TerminalSize};
    use native_tls::TlsConnector;
    use serde::Deserialize;
    use std::env;
    use std::fmt::{Display, Formatter};
    use std::fs::OpenOptions;
    use std::io::{self, Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;
    use tungstenite::client::IntoClientRequest;
    use tungstenite::protocol::WebSocketConfig;
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{client_tls_with_config, HandshakeError, Message, WebSocket};

    const BINARY_TTY_OUTPUT: u8 = 0x01;
    const BINARY_STDIN: u8 = 0x02;
    const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");
    const NESTED_AGENT_ENV: &str = "TTYS_AGENT_ACTIVE";
    const TRACE_ENV: &str = "TTYS_TRACE";
    const MAX_HTTP_BODY: usize = 1024 * 1024;
    const MAX_HTTP_RESPONSE: usize = MAX_HTTP_BODY + 64 * 1024;
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
    const IO_TIMEOUT: Duration = Duration::from_secs(15);
    const OUTPUT_QUEUE_CAPACITY: usize = 256;
    const CONTROL_QUEUE_CAPACITY: usize = 4;
    const PTY_INPUT_QUEUE_CAPACITY: usize = 64;
    const STATUS_QUEUE_CAPACITY: usize = 8;
    const MAX_MODAL_BUFFER: usize = 1024 * 1024;
    const REMOTE_OUTPUT_BATCH_SIZE: usize = 16 * 1024;
    const MAX_WEBSOCKET_MESSAGE: usize = 1024 * 1024;
    const SESSION_ID_ALPHABET: &str = "23456789abcdefghjkmnpqrstuvwxyz";

    type Result<T> = std::result::Result<T, Error>;

    #[derive(Debug)]
    pub(super) enum Error {
        Io(io::Error),
        Tls(native_tls::Error),
        Json(serde_json::Error),
        WebSocket(tungstenite::Error),
        Message(String),
    }

    impl Display for Error {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Io(error) => write!(formatter, "{error}"),
                Self::Tls(error) => write!(formatter, "{error}"),
                Self::Json(error) => write!(formatter, "{error}"),
                Self::WebSocket(error) => write!(formatter, "{error}"),
                Self::Message(message) => formatter.write_str(message),
            }
        }
    }

    impl std::error::Error for Error {}

    impl From<io::Error> for Error {
        fn from(value: io::Error) -> Self {
            Self::Io(value)
        }
    }

    impl From<native_tls::Error> for Error {
        fn from(value: native_tls::Error) -> Self {
            Self::Tls(value)
        }
    }

    impl From<native_tls::HandshakeError<TcpStream>> for Error {
        fn from(value: native_tls::HandshakeError<TcpStream>) -> Self {
            Self::Message(value.to_string())
        }
    }

    impl From<serde_json::Error> for Error {
        fn from(value: serde_json::Error) -> Self {
            Self::Json(value)
        }
    }

    impl From<tungstenite::Error> for Error {
        fn from(value: tungstenite::Error) -> Self {
            Self::WebSocket(value)
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Config {
        server: String,
        session: Option<String>,
        shell: Option<String>,
    }

    #[derive(Debug, Eq, PartialEq)]
    enum Command {
        Run(Config),
        Version,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct ConnectInfo {
        viewer_url: String,
        host_websocket_url: String,
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

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
    struct ControlRequest {
        #[serde(rename = "viewerId")]
        viewer_id: String,
        #[serde(rename = "leaseSeconds")]
        lease_seconds: i32,
    }

    #[derive(Deserialize)]
    struct SessionStatus {
        #[serde(rename = "pendingControlRequest")]
        pending_control_request: Option<ControlRequest>,
    }

    #[derive(Deserialize)]
    struct Envelope {
        #[serde(rename = "type")]
        kind: String,
        payload: serde_json::Value,
    }

    enum Outgoing {
        Text(String),
        Tty(Vec<u8>),
    }

    enum PtyInput {
        Bytes(Vec<u8>),
        Resize(TerminalSize),
    }

    #[derive(Debug, Eq, PartialEq)]
    enum TerminalRequest {
        Profile,
        Size,
    }

    pub fn run() -> Result<()> {
        let config = match parse_args(env::args().skip(1))? {
            Command::Run(config) => config,
            Command::Version => {
                println!("ttys-agent {AGENT_VERSION}");
                return Ok(());
            }
        };

        if env::var_os(NESTED_AGENT_ENV).is_some() {
            eprintln!("ttys-agent is already active in this terminal session.");
            eprintln!("Open a new local terminal, or exit the current shared shell before starting another agent.");
            return Ok(());
        }

        eprintln!("ttys-agent v{AGENT_VERSION}");
        let connect = resolve_connection(&config)?;
        let shell = config.shell.unwrap_or_else(default_shell);
        let mut pty = Pty::spawn(&shell)?;

        eprintln!("ttys-agent: shared shell is active.");
        eprintln!("Share URL: {}", connect.viewer_url);
        if cfg!(windows) {
            eprintln!("Exit the shared shell with 'exit'.\n");
        } else {
            eprintln!("Exit the shared shell with Ctrl-D or 'exit'.\n");
        }

        let raw_terminal = RawTerminal::enter()?;

        let (out_tx, out_rx) = mpsc::sync_channel::<Outgoing>(OUTPUT_QUEUE_CAPACITY);
        let (control_tx, control_rx) = mpsc::sync_channel::<Outgoing>(CONTROL_QUEUE_CAPACITY);
        let (pty_tx, pty_rx) = mpsc::sync_channel::<PtyInput>(PTY_INPUT_QUEUE_CAPACITY);
        let (status_tx, status_rx) =
            mpsc::sync_channel::<Option<ControlRequest>>(STATUS_QUEUE_CAPACITY);
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let modal = Arc::new(Mutex::new(ApprovalModal::new()));

        if let Ok(size) = terminal_size() {
            pty.resize(size)?;
            modal.lock().unwrap().set_size(size)?;
            enqueue(&out_tx, terminal_size_frame(size));
        }

        let ws_url = connect.host_websocket_url.clone();
        let remote_pty_tx = pty_tx.clone();
        thread::spawn(move || {
            websocket_loop(&ws_url, control_rx, out_rx, status_tx, remote_pty_tx)
        });

        let pty_out = pty.try_clone()?;
        let pty_done = done_tx.clone();
        let pty_sender = out_tx.clone();
        let pty_modal = Arc::clone(&modal);
        thread::spawn(move || {
            let _ = pty_output_loop(pty_out, pty_sender, pty_modal, trace_writer());
            let _ = pty_done.send(());
        });

        let stdin_sender = control_tx;
        let stdin_modal = Arc::clone(&modal);
        let stdin_pty_tx = pty_tx.clone();
        thread::spawn(move || {
            let _ = stdin_loop(stdin_pty_tx, stdin_sender, stdin_modal);
        });

        let status_modal = Arc::clone(&modal);
        let status_sender = out_tx.clone();
        thread::spawn(move || {
            status_loop(status_rx, status_modal, status_sender);
        });

        let resize_pty_tx = pty_tx.clone();
        let resize_modal = Arc::clone(&modal);
        thread::spawn(move || {
            resize_loop(resize_pty_tx, out_tx, resize_modal);
        });

        let pty_writer = pty.try_clone()?;
        thread::spawn(move || {
            let _ = pty_input_loop(pty_writer, pty_rx);
        });

        let _ = done_rx.recv();
        drop(raw_terminal);
        let _ = pty.wait();
        eprintln!("\nttys-agent: shared shell ended. Remote access is closed.");
        Ok(())
    }

    fn parse_args(args: impl Iterator<Item = String>) -> Result<Command> {
        let mut config = Config {
            server: "http://localhost:5173".to_string(),
            session: None,
            shell: None,
        };
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            if matches!(arg.as_str(), "--version" | "-V") {
                return Ok(Command::Version);
            }
            let (name, inline) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(left, right)| (left, Some(right)));
            let mut value = || -> Result<String> {
                inline
                    .map(str::to_string)
                    .or_else(|| args.next())
                    .ok_or_else(|| Error::Message(format!("missing value for {name}")))
            };
            match name {
                "--server" | "-server" => config.server = value()?,
                "--session" | "-session" => config.session = Some(value()?),
                "--shell" | "-shell" => config.shell = Some(value()?),
                _ => return Err(Error::Message(format!("unknown argument: {arg}"))),
            }
        }
        if config.server.is_empty() {
            return Err(Error::Message("server URL must not be empty".into()));
        }
        if let Some(session) = config.session.as_deref() {
            validate_session_id(session)?;
        }
        Ok(Command::Run(config))
    }

    fn validate_session_id(session_id: &str) -> Result<()> {
        let valid = session_id.len() == 7
            && session_id.as_bytes().get(3) == Some(&b'-')
            && session_id
                .bytes()
                .enumerate()
                .all(|(index, byte)| index == 3 || SESSION_ID_ALPHABET.as_bytes().contains(&byte));
        if valid {
            Ok(())
        } else {
            Err(Error::Message(format!("invalid session ID: {session_id}")))
        }
    }

    fn resolve_connection(config: &Config) -> Result<ConnectInfo> {
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
            let mut stream = TlsConnector::new()?.connect(url.tls_host(), tcp)?;
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

    fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
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

    fn enqueue<T>(sender: &SyncSender<T>, value: T) {
        let _ = sender.try_send(value);
    }

    fn enqueue_control(sender: &SyncSender<Outgoing>, value: Outgoing) {
        let _ = sender.try_send(value);
    }

    fn pty_output_loop(
        mut pty: impl Read,
        sender: SyncSender<Outgoing>,
        modal: Arc<Mutex<ApprovalModal>>,
        mut trace: Option<Box<dyn Write + Send>>,
    ) -> Result<()> {
        let mut buf = [0_u8; 4096];
        loop {
            let n = pty.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            if let Some(trace) = trace.as_mut() {
                let _ = trace.write_all(&buf[..n]);
                let _ = trace.flush();
            }
            if modal.lock().unwrap().handle_pty_output(&buf[..n])? {
                enqueue(&sender, Outgoing::Tty(buf[..n].to_vec()));
            }
        }
    }

    fn trace_writer() -> Option<Box<dyn Write + Send>> {
        let path = env::var_os(TRACE_ENV)?;
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => Some(Box::new(file)),
            Err(error) => {
                eprintln!(
                    "ttys-agent: failed to open TTYS_TRACE={}: {error}",
                    path.to_string_lossy()
                );
                None
            }
        }
    }

    fn stdin_loop(
        pty: SyncSender<PtyInput>,
        sender: SyncSender<Outgoing>,
        modal: Arc<Mutex<ApprovalModal>>,
    ) -> Result<()> {
        let mut buf = [0_u8; 4096];
        loop {
            let n = io::stdin().read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            match modal.lock().unwrap().handle_input(&buf[..n])? {
                ModalInput::Passthrough => {}
                ModalInput::Consumed => continue,
                ModalInput::Decision(decision, buffered) => {
                    enqueue_tty(&sender, buffered);
                    match decision {
                        ModalDecision::Approve(request) => {
                            let payload = serde_json::json!({"type":"control.approve","payload":{"viewerId":request.viewer_id,"leaseSeconds":request.lease_seconds}});
                            enqueue_control(&sender, Outgoing::Text(payload.to_string()));
                        }
                        ModalDecision::Reject(request) => {
                            let payload = serde_json::json!({"type":"control.reject","payload":{"viewerId":request.viewer_id}});
                            enqueue_control(&sender, Outgoing::Text(payload.to_string()));
                        }
                    }
                    continue;
                }
            }
            enqueue(&pty, PtyInput::Bytes(buf[..n].to_vec()));
        }
    }

    fn pty_input_loop(mut pty: PtyHandle, input: Receiver<PtyInput>) -> Result<()> {
        while let Ok(message) = input.recv() {
            match message {
                PtyInput::Bytes(bytes) => pty.write_all(&bytes)?,
                PtyInput::Resize(size) => pty.resize(size)?,
            }
        }
        Ok(())
    }

    fn status_loop(
        status: Receiver<Option<ControlRequest>>,
        modal: Arc<Mutex<ApprovalModal>>,
        sender: SyncSender<Outgoing>,
    ) {
        while let Ok(request) = status.recv() {
            if let Ok(Some(buffered)) = modal.lock().unwrap().sync_request(request) {
                enqueue_tty(&sender, buffered);
            }
        }
    }

    fn resize_loop(
        pty: SyncSender<PtyInput>,
        sender: SyncSender<Outgoing>,
        modal: Arc<Mutex<ApprovalModal>>,
    ) {
        let mut last = terminal_size().ok();
        loop {
            thread::sleep(Duration::from_millis(250));
            let Ok(size) = terminal_size() else {
                continue;
            };
            if last == Some(size) {
                continue;
            }
            last = Some(size);
            enqueue(&pty, PtyInput::Resize(size));
            enqueue(&sender, terminal_size_frame(size));
            let _ = modal.lock().unwrap().set_size(size);
        }
    }

    fn terminal_size_frame(size: TerminalSize) -> Outgoing {
        Outgoing::Text(terminal_size_text(size))
    }

    fn enqueue_tty(sender: &SyncSender<Outgoing>, bytes: Vec<u8>) {
        for chunk in bytes.chunks(REMOTE_OUTPUT_BATCH_SIZE) {
            enqueue(sender, Outgoing::Tty(chunk.to_vec()));
        }
    }

    enum ModalInput {
        Passthrough,
        Consumed,
        Decision(ModalDecision, Vec<u8>),
    }

    enum ModalDecision {
        Approve(ControlRequest),
        Reject(ControlRequest),
    }

    struct ApprovalModal {
        active: bool,
        size: TerminalSize,
        request: Option<ControlRequest>,
        dismissed_viewer_id: Option<String>,
        buffered: Vec<u8>,
        dropped_buffered_output: bool,
    }

    impl ApprovalModal {
        fn new() -> Self {
            Self {
                active: false,
                size: TerminalSize { cols: 80, rows: 24 },
                request: None,
                dismissed_viewer_id: None,
                buffered: Vec::new(),
                dropped_buffered_output: false,
            }
        }

        fn set_size(&mut self, size: TerminalSize) -> Result<()> {
            self.size = size;
            if self.active {
                self.render()?;
            }
            Ok(())
        }

        fn handle_pty_output(&mut self, chunk: &[u8]) -> Result<bool> {
            if self.active {
                let remaining = MAX_MODAL_BUFFER.saturating_sub(self.buffered.len());
                self.buffered
                    .extend_from_slice(&chunk[..remaining.min(chunk.len())]);
                self.dropped_buffered_output |= remaining < chunk.len();
                return Ok(false);
            }
            write_local_output(chunk)?;
            Ok(true)
        }

        fn sync_request(&mut self, request: Option<ControlRequest>) -> Result<Option<Vec<u8>>> {
            let Some(request) = request else {
                self.dismissed_viewer_id = None;
                if self.active {
                    return self.close_and_flush().map(Some);
                }
                self.request = None;
                return Ok(None);
            };

            if !self.active
                && self.dismissed_viewer_id.as_deref() == Some(request.viewer_id.as_str())
            {
                return Ok(None);
            }

            if self.active
                && self.request.as_ref().is_some_and(|current| {
                    current.viewer_id == request.viewer_id
                        && current.lease_seconds == request.lease_seconds
                })
            {
                return Ok(None);
            }

            self.request = Some(request);
            self.active = true;
            self.render().map(|()| None)
        }

        fn handle_input(&mut self, chunk: &[u8]) -> Result<ModalInput> {
            if !self.active {
                return Ok(ModalInput::Passthrough);
            }
            let Some(request) = self.request.clone() else {
                return Ok(ModalInput::Passthrough);
            };
            for byte in chunk {
                match *byte {
                    b'y' | b'Y' => {
                        self.dismissed_viewer_id = Some(request.viewer_id.clone());
                        let buffered = self.close_and_flush()?;
                        return Ok(ModalInput::Decision(
                            ModalDecision::Approve(request),
                            buffered,
                        ));
                    }
                    b'n' | b'N' | b'\r' | b'\n' | 0x03 | 0x1b => {
                        self.dismissed_viewer_id = Some(request.viewer_id.clone());
                        let buffered = self.close_and_flush()?;
                        return Ok(ModalInput::Decision(
                            ModalDecision::Reject(request),
                            buffered,
                        ));
                    }
                    _ => {}
                }
            }
            Ok(ModalInput::Consumed)
        }

        fn render(&self) -> Result<()> {
            let request = self.request.as_ref();
            let viewer = request
                .map(|value| value.viewer_id.as_str())
                .unwrap_or("unknown");
            let lease = request
                .map(|value| (value.lease_seconds / 60).max(1))
                .unwrap_or(0);

            let width = self.size.cols.max(1) as usize;
            let row = self.size.rows.max(1);
            let message = truncate(
                width,
                &format!(
                    " ttys control request: viewer {viewer}, {lease}m lease. Press Y to approve or N to deny. "
                ),
            );

            write_local_output(
                format!("\x1b7\x1b[{row};1H\x1b[2K\x1b[7m{message:<width$}\x1b[0m\x07\x1b8")
                    .as_bytes(),
            )
        }

        fn close(&mut self) -> Result<()> {
            if !self.active {
                return Ok(());
            }
            self.active = false;
            self.request = None;
            let row = self.size.rows.max(1);
            write_local_output(format!("\x1b7\x1b[{row};1H\x1b[2K\x1b8").as_bytes())
        }

        fn close_and_flush(&mut self) -> Result<Vec<u8>> {
            self.close()?;
            let buffered = std::mem::take(&mut self.buffered);
            if !buffered.is_empty() {
                write_local_output(&buffered)?;
            }
            if self.dropped_buffered_output {
                write_local_output(b"\r\n[ttys-agent: local output was truncated while control approval was pending]\r\n")?;
                self.dropped_buffered_output = false;
            }
            Ok(buffered)
        }
    }

    fn write_local_output(bytes: &[u8]) -> Result<()> {
        #[cfg(test)]
        {
            let _ = bytes;
            Ok(())
        }
        #[cfg(not(test))]
        {
            let mut stdout = io::stdout();
            stdout.write_all(bytes)?;
            stdout.flush()?;
            Ok(())
        }
    }

    fn truncate(width: usize, value: &str) -> String {
        if value.len() <= width {
            return value.to_string();
        }
        if width <= 1 {
            return value.chars().take(width).collect();
        }
        let mut output = value.chars().take(width - 1).collect::<String>();
        output.push('…');
        output
    }

    fn websocket_loop(
        url: &str,
        control: Receiver<Outgoing>,
        outgoing: Receiver<Outgoing>,
        status: SyncSender<Option<ControlRequest>>,
        pty: SyncSender<PtyInput>,
    ) {
        let mut delay = Duration::from_millis(250);
        let mut logged_failure = false;
        loop {
            match connect_websocket(url) {
                Ok(mut socket) => {
                    let _ = socket.send(Message::Text(terminal_profile_text().into()));
                    if let Ok(size) = terminal_size() {
                        let _ = socket.send(Message::Text(terminal_size_text(size).into()));
                    }
                    delay = Duration::from_millis(250);
                    logged_failure = false;
                    let _ = run_websocket(&mut socket, &control, &outgoing, &status, &pty);
                }
                Err(error) => {
                    if !logged_failure {
                        eprintln!("\r\nttys-agent: server connection failed: {error}. Retrying...");
                        logged_failure = true;
                    }
                }
            }
            thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(5));
        }
    }

    fn connect_websocket(value: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>> {
        let mut request = value.into_client_request()?;
        request.headers_mut().insert(
            "user-agent",
            "ttys-agent"
                .parse::<tungstenite::http::HeaderValue>()
                .map_err(|error| Error::Message(error.to_string()))?,
        );
        let uri = request.uri();
        let host = uri
            .host()
            .ok_or_else(|| Error::Message("websocket URL is missing a host".into()))?;
        let port = uri.port_u16().unwrap_or_else(|| match uri.scheme_str() {
            Some("wss") => 443,
            _ => 80,
        });
        let tcp = connect_tcp(host, port)?;
        let config = WebSocketConfig::default()
            .read_buffer_size(16 * 1024)
            .write_buffer_size(4 * 1024)
            .max_write_buffer_size(MAX_WEBSOCKET_MESSAGE + 4 * 1024)
            .max_message_size(Some(MAX_WEBSOCKET_MESSAGE))
            .max_frame_size(Some(MAX_WEBSOCKET_MESSAGE));
        let (mut socket, _) = match client_tls_with_config(request, tcp, Some(config), None) {
            Ok(result) => result,
            Err(HandshakeError::Failure(error)) => return Err(error.into()),
            Err(error) => {
                return Err(Error::Message(format!(
                    "websocket handshake failed: {error}"
                )))
            }
        };
        set_websocket_poll_timeout(socket.get_mut())?;
        Ok(socket)
    }

    fn set_websocket_poll_timeout(stream: &mut MaybeTlsStream<TcpStream>) -> Result<()> {
        match stream {
            MaybeTlsStream::Plain(stream) => {
                stream.set_read_timeout(Some(Duration::from_millis(100)))?
            }
            MaybeTlsStream::NativeTls(stream) => stream
                .get_ref()
                .set_read_timeout(Some(Duration::from_millis(100)))?,
            _ => {}
        }
        Ok(())
    }

    fn run_websocket(
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        control: &Receiver<Outgoing>,
        outgoing: &Receiver<Outgoing>,
        status: &SyncSender<Option<ControlRequest>>,
        pty: &SyncSender<PtyInput>,
    ) -> Result<()> {
        loop {
            flush_outgoing(socket, control, outgoing)?;

            match socket.read() {
                Ok(Message::Text(text)) => match handle_text_frame(text.as_bytes(), status)? {
                    Some(TerminalRequest::Profile) => {
                        socket.send(Message::Text(terminal_profile_text().into()))?;
                    }
                    Some(TerminalRequest::Size) => {
                        if let Ok(size) = terminal_size() {
                            socket.send(Message::Text(terminal_size_text(size).into()))?;
                        }
                    }
                    None => {}
                },
                Ok(Message::Binary(payload)) => {
                    if payload.first() == Some(&BINARY_STDIN) {
                        enqueue(pty, PtyInput::Bytes(payload[1..].to_vec()));
                    }
                }
                Ok(Message::Close(_)) => return Err(Error::Message("websocket closed".into())),
                Ok(Message::Ping(payload)) => socket.send(Message::Pong(payload))?,
                Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
                Err(tungstenite::Error::Io(error))
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(Error::WebSocket(error)),
            }
        }
    }

    fn flush_outgoing(
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        control: &Receiver<Outgoing>,
        outgoing: &Receiver<Outgoing>,
    ) -> Result<()> {
        let mut pending_tty = Vec::with_capacity(REMOTE_OUTPUT_BATCH_SIZE);
        while let Ok(message) = control.try_recv() {
            match message {
                Outgoing::Text(text) => socket.send(Message::Text(text.into()))?,
                Outgoing::Tty(chunk) => {
                    pending_tty.extend_from_slice(&chunk);
                    send_tty_output(socket, &mut pending_tty)?;
                }
            }
        }
        while let Ok(message) = outgoing.try_recv() {
            match message {
                Outgoing::Text(text) => {
                    send_tty_output(socket, &mut pending_tty)?;
                    socket.send(Message::Text(text.into()))?;
                }
                Outgoing::Tty(chunk) => {
                    if pending_tty.len() + chunk.len() > REMOTE_OUTPUT_BATCH_SIZE {
                        send_tty_output(socket, &mut pending_tty)?;
                    }
                    if chunk.len() > REMOTE_OUTPUT_BATCH_SIZE {
                        let mut frame = Vec::with_capacity(chunk.len() + 1);
                        frame.push(BINARY_TTY_OUTPUT);
                        frame.extend_from_slice(&chunk);
                        socket.send(Message::Binary(frame.into()))?;
                    } else {
                        pending_tty.extend_from_slice(&chunk);
                    }
                }
            }
        }
        send_tty_output(socket, &mut pending_tty)
    }

    fn send_tty_output(
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        pending_tty: &mut Vec<u8>,
    ) -> Result<()> {
        if pending_tty.is_empty() {
            return Ok(());
        }
        let mut frame = Vec::with_capacity(pending_tty.len() + 1);
        frame.push(BINARY_TTY_OUTPUT);
        frame.extend_from_slice(pending_tty);
        pending_tty.clear();
        socket.send(Message::Binary(frame.into()))?;
        Ok(())
    }

    fn handle_text_frame(
        payload: &[u8],
        status_tx: &SyncSender<Option<ControlRequest>>,
    ) -> Result<Option<TerminalRequest>> {
        let Ok(envelope) = serde_json::from_slice::<Envelope>(payload) else {
            return Ok(None);
        };
        if envelope.kind == "session.status" {
            let status: SessionStatus = serde_json::from_value(envelope.payload)?;
            enqueue(status_tx, status.pending_control_request);
            return Ok(None);
        }
        Ok(match envelope.kind.as_str() {
            "terminal.profile.request" => Some(TerminalRequest::Profile),
            "terminal.size.request" => Some(TerminalRequest::Size),
            _ => None,
        })
    }

    fn terminal_profile_text() -> String {
        let platform = if cfg!(windows) { "windows" } else { "unix" };
        let payload = if platform == "windows" {
            serde_json::json!({"type":"terminal.profile","payload":{"platform":platform,"pty":"conpty"}})
        } else {
            serde_json::json!({"type":"terminal.profile","payload":{"platform":platform}})
        };
        payload.to_string()
    }

    fn terminal_size_text(size: TerminalSize) -> String {
        let payload = serde_json::json!({"type":"terminal.size","payload":{"cols":size.cols,"rows":size.rows}});
        payload.to_string()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Cursor;
        use std::net::TcpListener;

        fn request(viewer_id: &str, lease_seconds: i32) -> ControlRequest {
            ControlRequest {
                viewer_id: viewer_id.to_string(),
                lease_seconds,
            }
        }

        fn local_listener() -> (TcpListener, u16) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            (listener, port)
        }

        #[test]
        fn parses_arguments_and_validates_session_ids() {
            assert_eq!(
                parse_args(
                    [
                        "--server=https://example.test:8443/base".to_string(),
                        "--session".to_string(),
                        "abc-def".to_string(),
                        "--shell=/bin/bash".to_string(),
                    ]
                    .into_iter(),
                )
                .unwrap(),
                Command::Run(Config {
                    server: "https://example.test:8443/base".to_string(),
                    session: Some("abc-def".to_string()),
                    shell: Some("/bin/bash".to_string()),
                })
            );

            assert!(
                parse_args(["--session".to_string(), "abc-io0".to_string()].into_iter()).is_err()
            );
            assert!(parse_args(["--server=".to_string()].into_iter()).is_err());
            assert!(parse_args(["--unknown".to_string()].into_iter()).is_err());
        }

        #[test]
        fn parses_version_arguments() {
            assert_eq!(
                parse_args(["--version".to_string()].into_iter()).unwrap(),
                Command::Version
            );
            assert_eq!(
                parse_args(["-V".to_string()].into_iter()).unwrap(),
                Command::Version
            );
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
                    host_websocket_url: "wss://example.test:8443/api/session/abc-def/host"
                        .to_string(),
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

            let url = ParsedUrl::parse(&format!("http://127.0.0.1:{port}/api/session?source=test"))
                .unwrap();
            assert_eq!(http_request("POST", &url, b"").unwrap(), b"{}");
            server.join().unwrap();
        }

        #[test]
        fn handles_control_frames_and_bounds_queues() {
            let (status_tx, status_rx) = mpsc::sync_channel(1);
            let status_frame = handle_text_frame(
                br#"{"type":"session.status","payload":{"pendingControlRequest":{"viewerId":"viewer-1","leaseSeconds":120}}}"#,
                &status_tx,
            )
            .unwrap();
            assert_eq!(status_frame, None);
            assert_eq!(status_rx.recv().unwrap(), Some(request("viewer-1", 120)));
            let size_request = handle_text_frame(
                br#"{"type":"terminal.size.request","payload":{}}"#,
                &status_tx,
            );
            assert_eq!(size_request.unwrap(), Some(TerminalRequest::Size));
            let profile_request = handle_text_frame(
                br#"{"type":"terminal.profile.request","payload":{}}"#,
                &status_tx,
            );
            assert_eq!(profile_request.unwrap(), Some(TerminalRequest::Profile));
            assert_eq!(handle_text_frame(b"not-json", &status_tx).unwrap(), None);

            let (queue_tx, queue_rx) = mpsc::sync_channel(1);
            enqueue(&queue_tx, 1_u8);
            enqueue(&queue_tx, 2_u8);
            assert_eq!(queue_rx.recv().unwrap(), 1);
            assert!(queue_rx.try_recv().is_err());
        }

        #[test]
        fn terminal_profile_describes_the_current_pty_platform() {
            let profile: serde_json::Value =
                serde_json::from_str(&terminal_profile_text()).unwrap();
            let expected_platform = if cfg!(windows) { "windows" } else { "unix" };

            assert_eq!(profile["type"], "terminal.profile");
            assert_eq!(profile["payload"]["platform"], expected_platform);
            if cfg!(windows) {
                assert_eq!(profile["payload"]["pty"], "conpty");
            } else {
                assert!(profile["payload"].get("pty").is_none());
            }
        }

        #[test]
        fn pauses_and_restores_output_while_control_is_pending() {
            let mut modal = ApprovalModal::new();
            assert_eq!(
                modal.sync_request(Some(request("viewer-1", 60))).unwrap(),
                None
            );
            assert!(!modal.handle_pty_output(b"pending output").unwrap());
            assert!(matches!(
                modal.handle_input(b"x").unwrap(),
                ModalInput::Consumed
            ));

            match modal.handle_input(b"Y").unwrap() {
                ModalInput::Decision(ModalDecision::Approve(decision), buffered) => {
                    assert_eq!(decision, request("viewer-1", 60));
                    assert_eq!(buffered, b"pending output");
                }
                _ => panic!("expected approval decision"),
            }
            assert!(!modal.active);

            assert_eq!(
                modal.sync_request(Some(request("viewer-2", 60))).unwrap(),
                None
            );
            assert!(!modal.handle_pty_output(b"cancelled output").unwrap());
            assert_eq!(
                modal.sync_request(None).unwrap(),
                Some(b"cancelled output".to_vec())
            );
        }

        #[test]
        fn bounds_output_buffer_while_control_is_pending() {
            let mut modal = ApprovalModal::new();
            modal.active = true;
            let output = vec![b'x'; MAX_MODAL_BUFFER + 1];
            assert!(!modal.handle_pty_output(&output).unwrap());
            assert_eq!(modal.buffered.len(), MAX_MODAL_BUFFER);
            assert!(modal.dropped_buffered_output);
        }

        #[test]
        fn prioritizes_control_frames_and_batches_tty_output() {
            let (listener, port) = local_listener();
            let server = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut socket = tungstenite::accept(stream).unwrap();
                assert_eq!(
                    socket.read().unwrap(),
                    Message::Text("control.approve".into())
                );
                assert_eq!(
                    socket.read().unwrap(),
                    Message::Binary(vec![BINARY_TTY_OUTPUT, b'a', b'b', b'c', b'd'].into())
                );
                assert_eq!(socket.read().unwrap(), Message::Text("status".into()));
                assert_eq!(
                    socket.read().unwrap(),
                    Message::Binary(vec![BINARY_TTY_OUTPUT, b'e', b'f'].into())
                );
            });

            let mut socket = connect_websocket(&format!("ws://127.0.0.1:{port}/host")).unwrap();
            let (control_tx, control_rx) = mpsc::sync_channel(1);
            let (out_tx, out_rx) = mpsc::sync_channel(4);
            enqueue_control(&control_tx, Outgoing::Text("control.approve".to_string()));
            enqueue(&out_tx, Outgoing::Tty(b"ab".to_vec()));
            enqueue(&out_tx, Outgoing::Tty(b"cd".to_vec()));
            enqueue(&out_tx, Outgoing::Text("status".to_string()));
            enqueue(&out_tx, Outgoing::Tty(b"ef".to_vec()));
            flush_outgoing(&mut socket, &control_rx, &out_rx).unwrap();
            server.join().unwrap();
        }

        #[test]
        fn receives_websocket_status_and_remote_stdin() {
            let (listener, port) = local_listener();
            let server = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut socket = tungstenite::accept(stream).unwrap();
                socket
                    .send(Message::Text(
                        r#"{"type":"session.status","payload":{"pendingControlRequest":{"viewerId":"viewer-2","leaseSeconds":30}}}"#
                            .into(),
                    ))
                    .unwrap();
                socket
                    .send(Message::Binary(
                        vec![BINARY_STDIN, b'l', b's', b'\n'].into(),
                    ))
                    .unwrap();
                socket.close(None).unwrap();
            });

            let mut socket = connect_websocket(&format!("ws://127.0.0.1:{port}/host")).unwrap();
            let (_control_tx, control_rx) = mpsc::sync_channel(1);
            let (_out_tx, out_rx) = mpsc::sync_channel(1);
            let (status_tx, status_rx) = mpsc::sync_channel(1);
            let (pty_tx, pty_rx) = mpsc::sync_channel(1);
            assert!(run_websocket(&mut socket, &control_rx, &out_rx, &status_tx, &pty_tx).is_err());
            assert_eq!(status_rx.recv().unwrap(), Some(request("viewer-2", 30)));
            match pty_rx.recv().unwrap() {
                PtyInput::Bytes(bytes) => assert_eq!(bytes, b"ls\n"),
                PtyInput::Resize(_) => panic!("expected stdin bytes"),
            }
            server.join().unwrap();
        }

        #[test]
        fn serializes_terminal_size_frame() {
            let text = terminal_size_text(TerminalSize {
                cols: 120,
                rows: 40,
            });
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                serde_json::json!({"type":"terminal.size","payload":{"cols":120,"rows":40}})
            );
        }
    }
}
