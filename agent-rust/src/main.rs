fn main() {
    if let Err(error) = app::run() {
        eprintln!("ttys-agent-rust: {error}");
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
    use std::net::TcpStream;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;
    use tungstenite::client::IntoClientRequest;
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{connect, Message, WebSocket};

    const BINARY_TTY_OUTPUT: u8 = 0x01;
    const BINARY_STDIN: u8 = 0x02;
    const NESTED_AGENT_ENV: &str = "TTYS_AGENT_ACTIVE";
    const TRACE_ENV: &str = "TTYS_TRACE";
    const MAX_HTTP_BODY: usize = 1024 * 1024;

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

    #[derive(Clone, Debug)]
    struct Config {
        server: String,
        session: Option<String>,
        shell: Option<String>,
    }

    #[derive(Debug)]
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

    #[derive(Clone, Deserialize)]
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
        Binary(Vec<u8>),
    }

    enum PtyInput {
        Bytes(Vec<u8>),
        Resize(TerminalSize),
    }

    pub fn run() -> Result<()> {
        if env::var_os(NESTED_AGENT_ENV).is_some() {
            eprintln!("ttys-agent is already active in this terminal session.");
            eprintln!("Open a new local terminal, or exit the current shared shell before starting another agent.");
            return Ok(());
        }

        let config = parse_args(env::args().skip(1))?;
        let connect = resolve_connection(&config)?;
        let shell = config.shell.unwrap_or_else(default_shell);
        let mut pty = Pty::spawn(&shell)?;

        eprintln!("ttys-agent-rust: shared shell is active.");
        eprintln!("Share URL: {}", connect.viewer_url);
        if cfg!(windows) {
            eprintln!("Exit the shared shell with 'exit'.\n");
        } else {
            eprintln!("Exit the shared shell with Ctrl-D or 'exit'.\n");
        }

        let raw_terminal = RawTerminal::enter()?;

        let (out_tx, out_rx) = mpsc::channel::<Outgoing>();
        let (pty_tx, pty_rx) = mpsc::channel::<PtyInput>();
        let (status_tx, status_rx) = mpsc::channel::<Option<ControlRequest>>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let (connected_tx, connected_rx) = mpsc::sync_channel::<()>(1);
        let modal = Arc::new(Mutex::new(ApprovalModal::new()));

        if let Ok(size) = terminal_size() {
            pty.resize(size)?;
            modal.lock().unwrap().set_size(size)?;
            let _ = out_tx.send(terminal_size_frame(size));
        }

        let ws_url = connect.host_websocket_url.clone();
        let remote_pty_tx = pty_tx.clone();
        thread::spawn(move || {
            websocket_loop(&ws_url, out_rx, status_tx, remote_pty_tx, connected_tx)
        });

        connected_rx
            .recv()
            .map_err(|_| Error::Message("websocket connection ended before startup".into()))?;

        let pty_out = pty.try_clone()?;
        let pty_done = done_tx.clone();
        let pty_sender = out_tx.clone();
        let pty_modal = Arc::clone(&modal);
        thread::spawn(move || {
            let _ = pty_output_loop(pty_out, pty_sender, pty_modal, trace_writer());
            let _ = pty_done.send(());
        });

        let stdin_sender = out_tx.clone();
        let stdin_modal = Arc::clone(&modal);
        let stdin_pty_tx = pty_tx.clone();
        thread::spawn(move || {
            let _ = stdin_loop(stdin_pty_tx, stdin_sender, stdin_modal);
        });

        let status_modal = Arc::clone(&modal);
        thread::spawn(move || {
            status_loop(status_rx, status_modal);
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
        eprintln!("\nttys-agent-rust: shared shell ended. Remote access is closed.");
        Ok(())
    }

    fn parse_args(args: impl Iterator<Item = String>) -> Result<Config> {
        let mut config = Config {
            server: "http://localhost:5173".to_string(),
            session: None,
            shell: None,
        };
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
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
        Ok(config)
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
        let host = url.host.as_str();
        let port = url.port_or_default()?;
        let target = request_target(url);
        let request = format!(
            "{method} {target} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: ttys-agent-rust\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let tcp = TcpStream::connect((host, port))?;
        if url.scheme == "https" {
            let mut stream = TlsConnector::new()?.connect(host, tcp)?;
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

    fn read_http_response(mut stream: impl Read) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        let header_end = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| Error::Message("invalid HTTP response".into()))?
            + 4;
        let status_line = std::str::from_utf8(&bytes[..header_end])
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default();
        if !status_line.contains(" 200 ") {
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
            Some((host, port))
                if !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()) =>
            {
                (host, Some(parse_port(port)?))
            }
            _ => (authority, None),
        };
        if host.is_empty() || !host.is_ascii() {
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
        } else if value.starts_with('?') {
            format!("/{value}")
        } else {
            format!("/{value}")
        }
    }

    fn pty_output_loop(
        mut pty: impl Read,
        sender: Sender<Outgoing>,
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
            modal.lock().unwrap().handle_pty_output(&buf[..n])?;
            let mut message = Vec::with_capacity(n + 1);
            message.push(BINARY_TTY_OUTPUT);
            message.extend_from_slice(&buf[..n]);
            let _ = sender.send(Outgoing::Binary(message));
        }
    }

    fn trace_writer() -> Option<Box<dyn Write + Send>> {
        let path = env::var_os(TRACE_ENV)?;
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => Some(Box::new(file)),
            Err(error) => {
                eprintln!(
                    "ttys-agent-rust: failed to open TTYS_TRACE={}: {error}",
                    path.to_string_lossy()
                );
                None
            }
        }
    }

    fn stdin_loop(
        pty: Sender<PtyInput>,
        sender: Sender<Outgoing>,
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
                ModalInput::Decision(decision) => {
                    match decision {
                        ModalDecision::Approve(request) => {
                            let payload = serde_json::json!({"type":"control.approve","payload":{"viewerId":request.viewer_id,"leaseSeconds":request.lease_seconds}});
                            let _ = sender.send(Outgoing::Text(payload.to_string()));
                        }
                        ModalDecision::Reject(request) => {
                            let payload = serde_json::json!({"type":"control.reject","payload":{"viewerId":request.viewer_id}});
                            let _ = sender.send(Outgoing::Text(payload.to_string()));
                        }
                    }
                    continue;
                }
            }
            let _ = pty.send(PtyInput::Bytes(buf[..n].to_vec()));
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

    fn status_loop(status: Receiver<Option<ControlRequest>>, modal: Arc<Mutex<ApprovalModal>>) {
        while let Ok(request) = status.recv() {
            let _ = modal.lock().unwrap().sync_request(request);
        }
    }

    fn resize_loop(
        pty: Sender<PtyInput>,
        sender: Sender<Outgoing>,
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
            let _ = pty.send(PtyInput::Resize(size));
            let _ = sender.send(terminal_size_frame(size));
            let _ = modal.lock().unwrap().set_size(size);
        }
    }

    fn terminal_size_frame(size: TerminalSize) -> Outgoing {
        Outgoing::Text(terminal_size_text(size))
    }

    enum ModalInput {
        Passthrough,
        Consumed,
        Decision(ModalDecision),
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
    }

    impl ApprovalModal {
        fn new() -> Self {
            Self {
                active: false,
                size: TerminalSize { cols: 80, rows: 24 },
                request: None,
                dismissed_viewer_id: None,
                buffered: Vec::new(),
            }
        }

        fn set_size(&mut self, size: TerminalSize) -> Result<()> {
            self.size = size;
            if self.active {
                self.render()?;
            }
            Ok(())
        }

        fn handle_pty_output(&mut self, chunk: &[u8]) -> Result<()> {
            if self.active {
                self.buffered.extend_from_slice(chunk);
                return Ok(());
            }
            io::stdout().write_all(chunk)?;
            io::stdout().flush()?;
            Ok(())
        }

        fn sync_request(&mut self, request: Option<ControlRequest>) -> Result<()> {
            let Some(request) = request else {
                self.dismissed_viewer_id = None;
                if self.active {
                    self.close()?;
                    self.flush_buffered_output()?;
                }
                self.request = None;
                return Ok(());
            };

            if !self.active
                && self.dismissed_viewer_id.as_deref() == Some(request.viewer_id.as_str())
            {
                return Ok(());
            }

            if self.active
                && self.request.as_ref().is_some_and(|current| {
                    current.viewer_id == request.viewer_id
                        && current.lease_seconds == request.lease_seconds
                })
            {
                return Ok(());
            }

            self.request = Some(request);
            self.active = true;
            self.render()
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
                        self.close()?;
                        self.flush_buffered_output()?;
                        return Ok(ModalInput::Decision(ModalDecision::Approve(request)));
                    }
                    b'n' | b'N' | b'\r' | b'\n' | 0x03 | 0x1b => {
                        self.dismissed_viewer_id = Some(request.viewer_id.clone());
                        self.close()?;
                        self.flush_buffered_output()?;
                        return Ok(ModalInput::Decision(ModalDecision::Reject(request)));
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

            print!("\x1b7\x1b[{row};1H\x1b[2K\x1b[7m{message:<width$}\x1b[0m\x07\x1b8");
            io::stdout().flush()?;
            Ok(())
        }

        fn close(&mut self) -> Result<()> {
            if !self.active {
                return Ok(());
            }
            self.active = false;
            self.request = None;
            let row = self.size.rows.max(1);
            print!("\x1b7\x1b[{row};1H\x1b[2K\x1b8");
            io::stdout().flush()?;
            Ok(())
        }

        fn flush_buffered_output(&mut self) -> Result<()> {
            if !self.buffered.is_empty() {
                io::stdout().write_all(&self.buffered)?;
                self.buffered.clear();
                io::stdout().flush()?;
            }
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
        outgoing: Receiver<Outgoing>,
        status: Sender<Option<ControlRequest>>,
        pty: Sender<PtyInput>,
        connected: mpsc::SyncSender<()>,
    ) {
        let mut delay = Duration::from_millis(250);
        let mut notified = false;
        loop {
            match connect_websocket(url) {
                Ok(mut socket) => {
                    if let Ok(size) = terminal_size() {
                        let _ = socket.send(Message::Text(terminal_size_text(size).into()));
                    }
                    delay = Duration::from_millis(250);
                    if !notified {
                        let _ = connected.send(());
                        notified = true;
                    }
                    let _ = run_websocket(&mut socket, &outgoing, &status, &pty);
                }
                Err(error) => {
                    eprintln!("\r\nttys-agent-rust: server connection failed: {error}. Retrying...")
                }
            }
            thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_secs(5));
        }
    }

    fn connect_websocket(value: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>> {
        let mut request = value.into_client_request()?;
        request
            .headers_mut()
            .insert("user-agent", "ttys-agent-rust".parse().unwrap());
        let (mut socket, _) = connect(request)?;
        set_websocket_timeouts(socket.get_mut())?;
        Ok(socket)
    }

    fn set_websocket_timeouts(stream: &mut MaybeTlsStream<TcpStream>) -> Result<()> {
        match stream {
            MaybeTlsStream::Plain(stream) => {
                stream.set_read_timeout(Some(Duration::from_millis(100)))?;
                stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            }
            MaybeTlsStream::NativeTls(stream) => {
                stream
                    .get_ref()
                    .set_read_timeout(Some(Duration::from_millis(100)))?;
                stream
                    .get_ref()
                    .set_write_timeout(Some(Duration::from_secs(5)))?;
            }
            _ => {}
        }
        Ok(())
    }

    fn run_websocket(
        socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
        outgoing: &Receiver<Outgoing>,
        status: &Sender<Option<ControlRequest>>,
        pty: &Sender<PtyInput>,
    ) -> Result<()> {
        loop {
            while let Ok(message) = outgoing.try_recv() {
                match message {
                    Outgoing::Text(text) => socket.send(Message::Text(text.into()))?,
                    Outgoing::Binary(bytes) => socket.send(Message::Binary(bytes.into()))?,
                }
            }

            match socket.read() {
                Ok(Message::Text(text)) => {
                    if handle_text_frame(text.as_bytes(), status)? {
                        if let Ok(size) = terminal_size() {
                            socket.send(Message::Text(terminal_size_text(size).into()))?;
                        }
                    }
                }
                Ok(Message::Binary(payload)) => {
                    if payload.first() == Some(&BINARY_STDIN) {
                        let _ = pty.send(PtyInput::Bytes(payload[1..].to_vec()));
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

    fn handle_text_frame(
        payload: &[u8],
        status_tx: &Sender<Option<ControlRequest>>,
    ) -> Result<bool> {
        let Ok(envelope) = serde_json::from_slice::<Envelope>(payload) else {
            return Ok(false);
        };
        if envelope.kind == "session.status" {
            let status: SessionStatus = serde_json::from_value(envelope.payload)?;
            let _ = status_tx.send(status.pending_control_request);
            return Ok(false);
        }
        Ok(envelope.kind == "terminal.size.request")
    }

    fn terminal_size_text(size: TerminalSize) -> String {
        let payload = serde_json::json!({"type":"terminal.size","payload":{"cols":size.cols,"rows":size.rows}});
        payload.to_string()
    }
}
