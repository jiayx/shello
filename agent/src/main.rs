#[cfg(all(feature = "native-tls", feature = "rustls-tls"))]
compile_error!("choose exactly one TLS backend");
#[cfg(not(any(feature = "native-tls", feature = "rustls-tls")))]
compile_error!("a TLS backend is required");

mod app;
#[cfg(unix)]
mod platform;
#[cfg(windows)]
#[path = "platform_windows.rs"]
mod platform;
#[cfg(feature = "native-tls")]
#[path = "tls/native.rs"]
mod tls_backend;
#[cfg(feature = "rustls-tls")]
#[path = "tls/rustls.rs"]
mod tls_backend;

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

fn main() {
    if let Err(error) = app::run() {
        eprintln!("ttys-agent: {error}");
        std::process::exit(1);
    }
}
