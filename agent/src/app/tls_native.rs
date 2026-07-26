use super::{Error, Result};
use native_tls::TlsConnector;
use std::io;
use std::net::TcpStream;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Connector;

pub(super) fn connect(tcp: TcpStream, host: &str) -> Result<native_tls::TlsStream<TcpStream>> {
    TlsConnector::new()
        .map_err(|error| Error::Message(error.to_string()))?
        .connect(host, tcp)
        .map_err(|error| Error::Message(error.to_string()))
}

pub(super) fn connector() -> Result<Option<Connector>> {
    Ok(None)
}

pub(super) fn set_poll_timeout(stream: &mut MaybeTlsStream<TcpStream>) -> io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(Duration::from_millis(100))),
        MaybeTlsStream::NativeTls(stream) => stream
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(100))),
        _ => Ok(()),
    }
}
