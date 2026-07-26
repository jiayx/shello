use super::{Error, Result};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::io;
use std::net::TcpStream;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Connector;

pub(super) fn connect(
    tcp: TcpStream,
    host: &str,
) -> Result<StreamOwned<ClientConnection, TcpStream>> {
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|_| Error::Message(format!("invalid TLS server name: {host}")))?;
    let connection = ClientConnection::new(config(), server_name)
        .map_err(|error| Error::Message(error.to_string()))?;
    Ok(StreamOwned::new(connection, tcp))
}

pub(super) fn connector() -> Result<Option<Connector>> {
    Ok(Some(Connector::Rustls(config())))
}

pub(super) fn set_poll_timeout(stream: &mut MaybeTlsStream<TcpStream>) -> io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(Duration::from_millis(100))),
        MaybeTlsStream::Rustls(stream) => stream
            .sock
            .set_read_timeout(Some(Duration::from_millis(100))),
        _ => Ok(()),
    }
}

fn config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Arc::new(
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("ring supports Rustls default protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }))
}
