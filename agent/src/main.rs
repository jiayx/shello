#[cfg(all(feature = "native-tls", feature = "rustls-tls"))]
compile_error!("choose exactly one TLS backend");
#[cfg(not(any(feature = "native-tls", feature = "rustls-tls")))]
compile_error!("a TLS backend is required");

mod cli;
mod connection;
mod output;
#[cfg(unix)]
mod platform;
#[cfg(windows)]
#[path = "platform_windows.rs"]
mod platform;
mod protocol;
mod terminal;
#[cfg(feature = "native-tls")]
#[path = "tls/native.rs"]
mod tls_backend;
#[cfg(feature = "rustls-tls")]
#[path = "tls/rustls.rs"]
mod tls_backend;
mod transport;

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub(crate) enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    WebSocket(tungstenite::Error),
    Message(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::WebSocket(error) => write!(formatter, "{error}"),
            Self::Message(message) => formatter.write_str(message),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
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

use cli::{parse_args, Command};
use connection::resolve_connection;
use platform::{default_shell, terminal_size, Pty, RawTerminal};
use protocol::{Outgoing, PtyInput, SessionStatus};
use std::env;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use terminal::{
    content_size, pty_input_loop, pty_output_loop, resize_loop, status_loop, stdin_loop,
    terminal_size_frame, trace_writer, HostTerminal,
};
use transport::websocket_loop;

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const NESTED_AGENT_ENV: &str = "SHELLO_AGENT_ACTIVE";
const OUTPUT_QUEUE_CAPACITY: usize = 1024 * 1024;
const CONTROL_QUEUE_CAPACITY: usize = 4;
const PTY_INPUT_QUEUE_CAPACITY: usize = 64;
const STATUS_QUEUE_CAPACITY: usize = 8;

fn run() -> Result<()> {
    let config = match parse_args(env::args().skip(1))? {
        Command::Run(config) => config,
        Command::Version => {
            println!("shello-agent {AGENT_VERSION}");
            return Ok(());
        }
    };

    if env::var_os(NESTED_AGENT_ENV).is_some() {
        eprintln!("shello-agent is already active in this terminal session.");
        eprintln!("Open a new local terminal, or exit the current shared shell before starting another agent.");
        return Ok(());
    }

    let connect = resolve_connection(&config)?;
    let shell = config.shell.unwrap_or_else(default_shell);
    let size = terminal_size()?;
    let mut pty = Pty::spawn(&shell, content_size(size))?;

    let exit_hint = if cfg!(windows) {
        "Exit the shared shell with 'exit'."
    } else {
        "Exit the shared shell with Ctrl-D or 'exit'."
    };
    let banner = format!(
        "shello-agent v{AGENT_VERSION}\r\nshello-agent: shared shell is active.\r\nShare URL: {}\r\n{exit_hint}\r\n\r\n",
        connect.viewer_url
    );

    let trace = trace_writer();
    let raw_terminal = RawTerminal::enter()?;

    let (out_tx, out_rx) = output::channel(OUTPUT_QUEUE_CAPACITY);
    let (control_tx, control_rx) = mpsc::sync_channel::<Outgoing>(CONTROL_QUEUE_CAPACITY);
    let (pty_tx, pty_rx) = mpsc::sync_channel::<PtyInput>(PTY_INPUT_QUEUE_CAPACITY);
    let (status_tx, status_rx) = mpsc::sync_channel::<SessionStatus>(STATUS_QUEUE_CAPACITY);
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let (transport_done_tx, transport_done_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let modal = Arc::new(Mutex::new(HostTerminal::new()));

    let _screen_guard = terminal::ScreenGuard(Arc::clone(&modal));
    modal.lock().unwrap().start(size, banner.as_bytes())?;
    out_tx.push(terminal_size_frame(content_size(size)));
    out_tx.push(Outgoing::Tty(banner.into_bytes()));

    let ws_url = connect.host_websocket_url.clone();
    let remote_pty_tx = pty_tx.clone();
    thread::spawn(move || {
        websocket_loop(
            &ws_url,
            control_rx,
            out_rx,
            status_tx,
            remote_pty_tx,
            shutdown_rx,
        );
        let _ = transport_done_tx.send(());
    });

    let pty_out = pty.try_clone()?;
    let pty_done = done_tx.clone();
    let pty_sender = out_tx.clone();
    let pty_modal = Arc::clone(&modal);
    let replies_tx = pty_tx.clone();
    thread::spawn(move || {
        let _ = pty_output_loop(pty_out, pty_sender, pty_modal, trace, replies_tx);
        let _ = pty_done.send(());
    });

    let stdin_sender = control_tx;
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
    let _ = shutdown_tx.send(());
    let _ = modal.lock().unwrap().stop();
    drop(raw_terminal);
    let _ = pty.wait();
    let _ = transport_done_rx.recv_timeout(std::time::Duration::from_secs(3));
    eprintln!("\nshello-agent: sharing stopped. Your share link can be reused.");
    eprintln!("Share URL: {}", connect.viewer_url);
    if let Ok(command) = connection::reconnect_command(&connect.viewer_url) {
        eprintln!("Share a new shell using the same link (previous programs are not restored):\n  {command}");
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("shello-agent v{AGENT_VERSION}: {error}");
        std::process::exit(1);
    }
}
