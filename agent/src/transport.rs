use crate::connection::connect_tcp;
use crate::platform::terminal_size;
use crate::protocol::{
    terminal_profile_text, terminal_size_text, ControlRequest, Envelope, Outgoing, PtyInput,
    SessionStatus, TerminalRequest, BINARY_STDIN, BINARY_TTY_OUTPUT, REMOTE_OUTPUT_BATCH_SIZE,
};
use crate::{tls_backend as tls, Error, Result};
use std::io;
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, SyncSender};
use std::thread;
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{client_tls_with_config, HandshakeError, Message, WebSocket};

const MAX_WEBSOCKET_MESSAGE: usize = 1024 * 1024;

fn enqueue<T>(sender: &SyncSender<T>, value: T) {
    let _ = sender.try_send(value);
}

pub(crate) fn websocket_loop(
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
                    eprintln!("\r\nshello-agent: server connection failed: {error}. Retrying...");
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
        "shello-agent"
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
    let (mut socket, _) =
        match client_tls_with_config(request, tcp, Some(config), tls::connector()?) {
            Ok(result) => result,
            Err(HandshakeError::Failure(error)) => return Err(error.into()),
            Err(error) => {
                return Err(Error::Message(format!(
                    "websocket handshake failed: {error}"
                )))
            }
        };
    tls::set_poll_timeout(socket.get_mut())?;
    Ok(socket)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

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
        enqueue(&control_tx, Outgoing::Text("control.approve".to_string()));
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
}
