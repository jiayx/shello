use crate::connection::connect_tcp;
use crate::platform::terminal_size;
use crate::protocol::{
    terminal_profile_text, terminal_size_text, Envelope, Outgoing, PtyInput, SessionStatus,
    TerminalRequest, BINARY_STDIN, BINARY_TTY_OUTPUT, REMOTE_OUTPUT_BATCH_SIZE,
};
use crate::{tls_backend as tls, Error, Result};
use std::io;
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, SyncSender};
#[cfg(test)]
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{client_tls_with_config, HandshakeError, Message, WebSocket};

const MAX_WEBSOCKET_MESSAGE: usize = 1024 * 1024;
const OUTPUT_MESSAGES_PER_TURN: usize = 16;

fn enqueue<T>(sender: &SyncSender<T>, value: T) {
    let _ = sender.try_send(value);
}

pub(crate) fn websocket_loop(
    url: &str,
    control: Receiver<Outgoing>,
    outgoing: crate::output::OutputReceiver,
    status: SyncSender<SessionStatus>,
    pty: SyncSender<PtyInput>,
    shutdown: Receiver<()>,
) {
    // Track only this connection's ordered stream for viewer snapshot requests.
    let mut screen = vt100::Parser::new(24, 80, 0);
    let mut delay = Duration::from_millis(250);
    loop {
        if shutdown.try_recv().is_ok() {
            return;
        }
        match connect_websocket(url) {
            Ok(mut socket) => {
                let _ = socket.send(Message::Text(terminal_profile_text().into()));
                if let Ok(size) = terminal_size() {
                    let _ = socket.send(Message::Text(
                        terminal_size_text(crate::terminal::content_size(size)).into(),
                    ));
                }
                delay = Duration::from_millis(250);
                if start_output(&mut socket, &outgoing, &mut screen)
                    .and_then(|()| {
                        run_websocket(
                            &mut socket,
                            &control,
                            &outgoing,
                            &status,
                            &pty,
                            &mut screen,
                            &shutdown,
                        )
                    })
                    .is_ok()
                {
                    return;
                }
            }
            Err(Error::WebSocket(tungstenite::Error::Http(response)))
                if response.status().as_u16() == 410 =>
            {
                mark_session_ended(&status);
                return;
            }
            Err(_) => {}
        }
        // Approvals belong to the lost connection, never to a later request.
        for _ in control.try_iter().take(4) {}
        let _ = status.send(SessionStatus::default());
        if shutdown.recv_timeout(delay).is_ok() {
            return;
        }
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

fn start_output(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    outgoing: &crate::output::OutputReceiver,
    screen: &mut vt100::Parser,
) -> Result<()> {
    let (size, snapshot) = outgoing.restart().ok_or_else(|| {
        Error::Message("waiting for a complete terminal sequence before resynchronizing".into())
    })?;
    *screen = vt100::Parser::new(24, 80, 0);
    track_terminal_size(screen, &size);
    screen.process_for_snapshot(&snapshot);
    socket.send(Message::Text(size.into()))?;
    let mut snapshot = snapshot;
    send_tty_output(socket, &mut snapshot)
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
    outgoing: &crate::output::OutputReceiver,
    status: &SyncSender<SessionStatus>,
    pty: &SyncSender<PtyInput>,
    screen: &mut vt100::Parser,
    shutdown: &Receiver<()>,
) -> Result<()> {
    let mut last_received = Instant::now();
    let mut last_ping = Instant::now();
    let mut snapshot_requests: std::collections::VecDeque<String> =
        std::collections::VecDeque::new();
    loop {
        if shutdown.try_recv().is_ok() {
            // The PTY has ended. Drain its bounded queue before the end marker.
            for _ in 0..16 {
                if flush_outgoing(socket, control, outgoing, screen).is_err() {
                    break;
                }
            }
            let _ = end_session(socket);
            return Ok(());
        }
        flush_outgoing(socket, control, outgoing, screen)?;
        if screen.snapshot_ready() {
            for request_id in snapshot_requests.drain(..) {
                socket.send(Message::Text(snapshot_text(screen, &request_id).into()))?;
            }
        }

        if last_received.elapsed() >= Duration::from_secs(45) {
            return Err(Error::Message("websocket heartbeat timed out".into()));
        }
        if last_ping.elapsed() >= Duration::from_secs(15) {
            socket.send(Message::Ping(b"shello".to_vec().into()))?;
            last_ping = Instant::now();
        }
        let incoming = socket.read();
        if incoming.is_ok() {
            last_received = Instant::now();
        }
        match incoming {
            Ok(Message::Text(text)) => match handle_text_frame(text.as_bytes(), status)? {
                Some(TerminalRequest::Profile) => {
                    socket.send(Message::Text(terminal_profile_text().into()))?;
                }
                Some(TerminalRequest::Size) => {
                    if let Ok(size) = terminal_size() {
                        socket.send(Message::Text(
                            terminal_size_text(crate::terminal::content_size(size)).into(),
                        ))?;
                    }
                }
                Some(TerminalRequest::Snapshot(request_id)) => {
                    if snapshot_requests.len() >= 128 {
                        snapshot_requests.pop_front();
                    }
                    snapshot_requests.push_back(request_id);
                }
                Some(TerminalRequest::Ended) => return Ok(()),
                None => {}
            },
            Ok(Message::Binary(payload)) => {
                if payload.first() == Some(&BINARY_STDIN) {
                    enqueue(pty, PtyInput::Bytes(payload[1..].to_vec()));
                }
            }
            Ok(Message::Close(frame)) => {
                if frame
                    .as_ref()
                    .is_some_and(|f| matches!(u16::from(f.code), 4000 | 4001))
                {
                    mark_session_ended(status);
                    return Ok(());
                }
                return Err(Error::Message("websocket closed".into()));
            }
            Ok(Message::Ping(payload)) => socket.send(Message::Pong(payload))?,
            Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Err(tungstenite::Error::Io(error))
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => return Err(Error::WebSocket(error)),
        }
    }
}

fn mark_session_ended(status: &SyncSender<SessionStatus>) {
    let _ = status.send(SessionStatus {
        ended: true,
        ..SessionStatus::default()
    });
}

fn end_session(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>) -> Result<()> {
    socket.send(Message::Text(
        r#"{"type":"session.end","payload":{}}"#.into(),
    ))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match socket.read() {
            Ok(Message::Text(text)) => {
                if serde_json::from_str::<Envelope>(&text).is_ok_and(|f| f.kind == "session.ended")
                {
                    let _ = socket.close(None);
                    return Ok(());
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(Message::Ping(data)) => {
                socket.send(Message::Pong(data))?;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let _ = socket.close(None);
    Ok(())
}

fn flush_outgoing(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    control: &Receiver<Outgoing>,
    outgoing: &crate::output::OutputReceiver,
    screen: &mut vt100::Parser,
) -> Result<()> {
    if outgoing.overflowed() {
        return Err(Error::Message(
            "terminal output buffer full; reconnecting to resynchronize".into(),
        ));
    }
    let mut pending_tty = Vec::with_capacity(REMOTE_OUTPUT_BATCH_SIZE);
    for message in control.try_iter().take(4) {
        match message {
            Outgoing::Text(text) => {
                track_terminal_size(screen, &text);
                socket.send(Message::Text(text.into()))?;
            }
            Outgoing::Tty(chunk) => {
                screen.process_for_snapshot(&chunk);
                pending_tty.extend_from_slice(&chunk);
                send_tty_output(socket, &mut pending_tty)?;
            }
        }
    }
    // Leave time to receive requests even when PTY output continuously refills
    // the queue. Draining until empty can starve incoming control messages.
    for message in outgoing.try_iter().take(OUTPUT_MESSAGES_PER_TURN) {
        match message {
            Outgoing::Text(text) => {
                track_terminal_size(screen, &text);
                send_tty_output(socket, &mut pending_tty)?;
                socket.send(Message::Text(text.into()))?;
            }
            Outgoing::Tty(chunk) => {
                screen.process_for_snapshot(&chunk);
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
    if outgoing.overflowed() {
        return Err(Error::Message(
            "terminal output buffer full; reconnecting to resynchronize".into(),
        ));
    }
    send_tty_output(socket, &mut pending_tty)
}

pub(crate) fn track_terminal_size(screen: &mut vt100::Parser, text: &str) {
    let Ok(frame) = serde_json::from_str::<Envelope>(text) else {
        return;
    };
    if frame.kind != "terminal.size" {
        return;
    }
    if let (Some(rows), Some(cols)) = (
        frame.payload["rows"].as_u64(),
        frame.payload["cols"].as_u64(),
    ) {
        if let (Ok(rows), Ok(cols)) = (u16::try_from(rows), u16::try_from(cols)) {
            if rows > 0 && cols > 0 {
                screen.screen_mut().set_size(rows, cols);
            }
        }
    }
}

fn snapshot_text(parser: &vt100::Parser, request_id: &str) -> String {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    serde_json::json!({
        "type": "terminal.snapshot",
        "payload": {
            "requestId": request_id,
            "size": {"rows": rows, "cols": cols},
            "data": String::from_utf8_lossy(&screen.snapshot_formatted()),
        }
    })
    .to_string()
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
    status_tx: &SyncSender<SessionStatus>,
) -> Result<Option<TerminalRequest>> {
    let Ok(envelope) = serde_json::from_slice::<Envelope>(payload) else {
        return Ok(None);
    };
    if envelope.kind == "session.status" {
        let ended = envelope.payload["state"] == "closed" || envelope.payload["state"] == "ended";
        let mut status: SessionStatus = serde_json::from_value(envelope.payload)?;
        status.ended = ended;
        status.connected = !status.ended;
        let ended = status.ended;
        let _ = status_tx.send(status);
        return Ok(if ended {
            Some(TerminalRequest::Ended)
        } else {
            None
        });
    }
    Ok(match envelope.kind.as_str() {
        "terminal.profile.request" => Some(TerminalRequest::Profile),
        "terminal.size.request" => Some(TerminalRequest::Size),
        "terminal.snapshot.request" => envelope.payload["requestId"]
            .as_str()
            .map(|id| TerminalRequest::Snapshot(id.to_owned())),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ControlRequest;
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn ending_session_waits_for_server_acknowledgement() {
        let (listener, port) = local_listener();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let Message::Text(text) = socket.read().unwrap() else {
                panic!("expected end marker")
            };
            assert_eq!(
                serde_json::from_str::<Envelope>(&text).unwrap().kind,
                "session.end"
            );
            socket
                .send(Message::Text(
                    r#"{"type":"session.ended","payload":{}}"#.into(),
                ))
                .unwrap();
            assert!(matches!(socket.read().unwrap(), Message::Close(_)));
        });
        let mut socket = connect_websocket(&format!("ws://127.0.0.1:{port}/host")).unwrap();
        end_session(&mut socket).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn closed_status_terminates_retry_and_clears_local_control_state() {
        let (tx, rx) = mpsc::sync_channel(1);
        assert_eq!(handle_text_frame(br#"{"type":"session.status","payload":{"state":"closed","pendingControlRequest":null}}"#, &tx).unwrap(), Some(TerminalRequest::Ended));
        let status = rx.recv().unwrap();
        assert!(status.ended);
        assert!(!status.connected);
        assert!(status.controller_viewer_id.is_none());
    }

    fn restored_snapshot(source: &vt100::Parser) -> vt100::Parser {
        let frame: serde_json::Value =
            serde_json::from_str(&snapshot_text(source, "refresh-1")).unwrap();
        assert_eq!(frame["payload"]["requestId"], "refresh-1");
        let (rows, cols) = source.screen().size();
        assert_eq!(frame["payload"]["size"]["rows"], rows);
        let mut viewer = vt100::Parser::new(rows, cols, 100);
        viewer.process(b"stale screen");
        viewer.process(frame["payload"]["data"].as_str().unwrap().as_bytes());
        viewer
    }

    #[test]
    fn refresh_waits_for_fragmented_escape_and_utf8_output() {
        let mut parser = vt100::Parser::new(8, 40, 0);
        parser.process_for_snapshot(b"prompt $ \x1b[3");
        assert!(!parser.snapshot_ready());
        parser.process_for_snapshot(b"2m");
        assert!(parser.snapshot_ready());
        parser.process_for_snapshot(&[0xe4, 0xb8]);
        assert!(!parser.snapshot_ready());
        parser.process_for_snapshot(&[0xad]);
        assert!(parser.snapshot_ready());
        parser.process_for_snapshot(b"\x1b]0;title\x1b");
        assert!(!parser.snapshot_ready());
        parser.process_for_snapshot(b"\\");
        assert!(parser.snapshot_ready());
        assert_eq!(
            restored_snapshot(&parser).screen().contents(),
            parser.screen().contents()
        );
    }

    #[test]
    fn refresh_restores_idle_prompt_and_following_output() {
        let mut source = vt100::Parser::new(8, 40, 0);
        source.process(b"old output\r\n\x1b[32mshello $ \x1b[0m");
        let mut viewer = restored_snapshot(&source);
        assert_eq!(viewer.screen().contents(), source.screen().contents());
        assert_eq!(
            viewer.screen().cursor_position(),
            source.screen().cursor_position()
        );
        let continuation = b"echo ok\r\nok\r\nshello $ ";
        source.process(continuation);
        viewer.process(continuation);
        assert_eq!(
            viewer.screen().state_formatted(),
            source.screen().state_formatted()
        );
    }

    #[test]
    fn refresh_restores_tui_modes_scroll_region_and_primary_screen_on_exit() {
        let mut source = vt100::Parser::new(8, 40, 0);
        source.process(b"shello $ tui\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[2;7r\x1b[2;1HTUI");
        let mut viewer = restored_snapshot(&source);
        assert!(viewer.screen().alternate_screen());
        assert_eq!(
            viewer.screen().state_formatted(),
            source.screen().state_formatted()
        );
        let update = b"\x1b[7;1Hbottom\r\nnext";
        source.process(update);
        viewer.process(update);
        assert_eq!(viewer.screen().contents(), source.screen().contents());
        let exit = b"\x1b[?1000l\x1b[?1006l\x1b[?1049l\x1b[?25h\r\nshello $ ";
        source.process(exit);
        viewer.process(exit);
        assert_eq!(
            viewer.screen().state_formatted(),
            source.screen().state_formatted()
        );
    }

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
    fn congested_stream_restarts_with_snapshot_before_live_output() {
        let (listener, port) = local_listener();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let Message::Text(size) = socket.read().unwrap() else {
                panic!("expected size")
            };
            assert_eq!(
                serde_json::from_str::<Envelope>(&size).unwrap().kind,
                "terminal.size"
            );
            let mut viewer = vt100::Parser::new(24, 80, 0);
            for _ in 0..2 {
                let Message::Binary(frame) = socket.read().unwrap() else {
                    panic!("expected output")
                };
                assert_eq!(frame[0], BINARY_TTY_OUTPUT);
                viewer.process(&frame[1..]);
            }
            assert_eq!(viewer.screen().contents(), "prompt> hello!");
        });
        let mut socket = connect_websocket(&format!("ws://127.0.0.1:{port}/host")).unwrap();
        let (_control_tx, control_rx) = mpsc::sync_channel(1);
        let (tx, rx) = crate::output::channel(8);
        tx.push(Outgoing::Tty(b"prompt> ".to_vec()));
        tx.push(Outgoing::Tty(b"hello".to_vec()));
        let mut screen = vt100::Parser::new(24, 80, 0);
        assert!(flush_outgoing(&mut socket, &control_rx, &rx, &mut screen).is_err());
        start_output(&mut socket, &rx, &mut screen).unwrap();
        tx.push(Outgoing::Tty(b"!".to_vec()));
        flush_outgoing(&mut socket, &control_rx, &rx, &mut screen).unwrap();
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
        assert_eq!(
            status_rx.recv().unwrap().pending_control_request,
            Some(request("viewer-1", 120))
        );
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
        let (out_tx, out_rx) = crate::output::channel(1024);
        enqueue(&control_tx, Outgoing::Text("control.approve".to_string()));
        out_tx.push(Outgoing::Tty(b"ab".to_vec()));
        out_tx.push(Outgoing::Tty(b"cd".to_vec()));
        out_tx.push(Outgoing::Text("status".to_string()));
        out_tx.push(Outgoing::Tty(b"ef".to_vec()));
        flush_outgoing(
            &mut socket,
            &control_rx,
            &out_rx,
            &mut vt100::Parser::new(24, 80, 0),
        )
        .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn output_flush_leaves_a_receive_opportunity_before_queue_is_empty() {
        let (listener, port) = local_listener();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            assert_eq!(
                socket.read().unwrap(),
                Message::Binary(
                    [
                        vec![BINARY_TTY_OUTPUT],
                        vec![b'x'; OUTPUT_MESSAGES_PER_TURN]
                    ]
                    .concat()
                    .into()
                )
            );
        });
        let mut socket = connect_websocket(&format!("ws://127.0.0.1:{port}/host")).unwrap();
        let (_control_tx, control_rx) = mpsc::sync_channel(1);
        let (out_tx, out_rx) = crate::output::channel(OUTPUT_MESSAGES_PER_TURN + 1);
        for _ in 0..=OUTPUT_MESSAGES_PER_TURN {
            out_tx.push(Outgoing::Tty(vec![b'x']));
        }
        flush_outgoing(
            &mut socket,
            &control_rx,
            &out_rx,
            &mut vt100::Parser::new(24, 80, 0),
        )
        .unwrap();
        assert!(matches!(out_rx.try_recv().unwrap(), Outgoing::Tty(_)));
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
        let (_out_tx, out_rx) = crate::output::channel(1);
        let (status_tx, status_rx) = mpsc::sync_channel(1);
        let (pty_tx, pty_rx) = mpsc::sync_channel(1);
        assert!(run_websocket(
            &mut socket,
            &control_rx,
            &out_rx,
            &status_tx,
            &pty_tx,
            &mut vt100::Parser::new(24, 80, 0),
            &mpsc::channel().1
        )
        .is_err());
        assert_eq!(
            status_rx.recv().unwrap().pending_control_request,
            Some(request("viewer-2", 30))
        );
        match pty_rx.recv().unwrap() {
            PtyInput::Bytes(bytes) => assert_eq!(bytes, b"ls\n"),
            PtyInput::Resize(_) => panic!("expected stdin bytes"),
        }
        server.join().unwrap();
    }
}
