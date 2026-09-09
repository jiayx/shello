#[cfg(all(feature = "native-tls", feature = "rustls-tls"))]
compile_error!("choose exactly one TLS backend");
#[cfg(not(any(feature = "native-tls", feature = "rustls-tls")))]
compile_error!("a TLS backend is required");

mod cli;
mod connection;
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
use protocol::{ControlRequest, Outgoing, PtyInput};
use std::env;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use terminal::{
    enqueue, pty_input_loop, pty_output_loop, resize_loop, status_loop, stdin_loop,
    terminal_size_frame, trace_writer, ApprovalModal,
};
use transport::websocket_loop;

const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const NESTED_AGENT_ENV: &str = "SHELLO_AGENT_ACTIVE";
const OUTPUT_QUEUE_CAPACITY: usize = 256;
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

    eprintln!("shello-agent v{AGENT_VERSION}");
    let connect = resolve_connection(&config)?;
    let shell = config.shell.unwrap_or_else(default_shell);
    let mut pty = Pty::spawn(&shell)?;

    eprintln!("shello-agent: shared shell is active.");
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
    thread::spawn(move || websocket_loop(&ws_url, control_rx, out_rx, status_tx, remote_pty_tx));

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
    eprintln!("\nshello-agent: shared shell ended. Remote access is closed.");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("shello-agent: {error}");
        std::process::exit(1);
    }
}
