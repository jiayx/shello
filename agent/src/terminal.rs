use crate::platform::{terminal_size, PtyHandle, TerminalSize};
use crate::protocol::{terminal_size_text, ControlRequest, Outgoing, PtyInput, SessionStatus};
use crate::Result;
use std::env;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const TRACE_ENV: &str = "SHELLO_TRACE";
const SCROLLBACK_LINES: usize = 10_000;
// Muted backgrounds keep persistent status informative rather than alarming.
const STATUS_READY: &str = "38;5;152;48;5;23";
const STATUS_CONTROL: &str = "38;5;153;48;5;24";
const STATUS_ATTENTION: &str = "38;5;223;48;5;58";
const STATUS_NEUTRAL: &str = "38;5;252;48;5;238";
// Save a mode immediately before our first change, rather than snapshotting and
// resetting an unrelated fixed set of terminal modes.
#[derive(Default)]
struct ChangedModes(Vec<u16>);
impl ChangedModes {
    fn remember(&mut self, modes: &[u16], frame: &mut Vec<u8>) -> io::Result<()> {
        for &mode in modes {
            if !self.0.contains(&mode) {
                write!(frame, "\x1b[?{mode}s")?;
                self.0.push(mode);
            }
        }
        Ok(())
    }
    fn restore(&self, frame: &mut Vec<u8>) -> io::Result<()> {
        for mode in &self.0 {
            write!(frame, "\x1b[?{mode}r")?;
        }
        Ok(())
    }
}

pub(crate) fn enqueue<T>(sender: &SyncSender<T>, value: T) {
    let _ = sender.try_send(value);
}

pub(crate) fn enqueue_control(sender: &SyncSender<Outgoing>, value: Outgoing) {
    let _ = sender.try_send(value);
}

pub(crate) fn pty_output_loop(
    mut pty: impl Read,
    sender: crate::output::OutputSender,
    modal: Arc<Mutex<HostTerminal>>,
    mut trace: Option<Box<dyn Write + Send>>,
    input: SyncSender<PtyInput>,
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
        let replies = {
            let mut terminal = modal.lock().unwrap();
            let replies = terminal.handle_pty_output(&buf[..n])?;
            // Serialize output with resize notifications in both screen models.
            sender.push(Outgoing::Tty(buf[..n].to_vec()));
            replies
        };
        if !replies.is_empty() {
            let _ = input.send(PtyInput::Bytes(replies));
        }
    }
}

pub(crate) fn trace_writer() -> Option<Box<dyn Write + Send>> {
    let path = env::var_os(TRACE_ENV)?;
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => Some(Box::new(file)),
        Err(error) => {
            eprintln!(
                "shello-agent: failed to open SHELLO_TRACE={}: {error}",
                path.to_string_lossy()
            );
            None
        }
    }
}

pub(crate) fn stdin_loop(
    pty: SyncSender<PtyInput>,
    sender: SyncSender<Outgoing>,
    modal: Arc<Mutex<HostTerminal>>,
) -> Result<()> {
    // A short timeout preserves standalone Escape while joining fragmented mouse
    // reports. Never forward a partial report and its coordinates separately.
    let (reads_tx, reads_rx) = std::sync::mpsc::sync_channel(64);
    thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        loop {
            let result = io::stdin().read(&mut buf).map(|n| buf[..n].to_vec());
            let done = result.as_ref().map_or(true, |bytes| bytes.is_empty());
            if reads_tx.send(result).is_err() || done {
                break;
            }
        }
    });
    let mut decoder = InputDecoder::default();
    loop {
        let chunks = match reads_rx.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(bytes)) if bytes.is_empty() => return Ok(()),
            Ok(Ok(bytes)) => decoder.feed(&bytes),
            Ok(Err(error)) => return Err(error.into()),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => decoder.flush_key(),
        };
        for chunk in chunks {
            match modal.lock().unwrap().handle_input(&chunk)? {
                ModalInput::Passthrough => enqueue(&pty, PtyInput::Bytes(chunk)),
                ModalInput::Consumed => {}
                ModalInput::Decision(decision) => {
                    let payload = match decision {
                        ModalDecision::Approve(request) => {
                            serde_json::json!({"type":"control.approve","payload":{"viewerId":request.viewer_id,"leaseSeconds":request.lease_seconds}})
                        }
                        ModalDecision::Reject(request) => {
                            serde_json::json!({"type":"control.reject","payload":{"viewerId":request.viewer_id}})
                        }
                    };
                    enqueue_control(&sender, Outgoing::Text(payload.to_string()));
                }
            }
        }
    }
}

#[derive(Default)]
struct InputDecoder {
    pending: Vec<u8>,
}
impl InputDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let mut text = Vec::new();
        for &byte in bytes {
            if self.pending.is_empty() && byte != 27 {
                text.push(byte);
                continue;
            }
            if !text.is_empty() {
                frames.push(std::mem::take(&mut text));
            }
            self.pending.push(byte);
            let done = match self.pending.as_slice() {
                [27] | [27, b'['] | [27, b'O'] => false,
                [27, b'[', ..] => (0x40..=0x7e).contains(&byte),
                _ => true,
            };
            if done || self.pending.len() >= 256 {
                frames.push(std::mem::take(&mut self.pending));
            }
        }
        if !text.is_empty() {
            frames.push(text);
        }
        frames
    }
    fn flush_key(&mut self) -> Vec<Vec<u8>> {
        if self.pending.is_empty() || self.pending.starts_with(b"\x1b[<") {
            return Vec::new();
        }
        vec![std::mem::take(&mut self.pending)]
    }
}

fn mouse_report(bytes: &[u8]) -> Option<(u16, u16, bool)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let body = text.strip_prefix("\x1b[<")?;
    let pressed = body.ends_with('M');
    let body = body.strip_suffix('M').or_else(|| body.strip_suffix('m'))?;
    let mut fields = body.split(';');
    let button = fields.next()?.parse().ok()?;
    let _column: u16 = fields.next()?.parse().ok()?;
    let row = fields.next()?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some((button, row, pressed))
}

pub(crate) fn pty_input_loop(mut pty: PtyHandle, input: Receiver<PtyInput>) -> Result<()> {
    while let Ok(message) = input.recv() {
        match message {
            PtyInput::Bytes(bytes) => pty.write_all(&bytes)?,
            PtyInput::Resize(size) => pty.resize(size)?,
        }
    }
    Ok(())
}

pub(crate) fn status_loop(status: Receiver<SessionStatus>, modal: Arc<Mutex<HostTerminal>>) {
    while let Ok(status) = status.recv() {
        let _ = modal.lock().unwrap().sync_status(status);
    }
}

pub(crate) fn content_size(size: TerminalSize) -> TerminalSize {
    TerminalSize {
        cols: size.cols.max(1),
        rows: size.rows.saturating_sub(1).max(1),
    }
}

pub(crate) fn resize_loop(
    pty: SyncSender<PtyInput>,
    sender: crate::output::OutputSender,
    modal: Arc<Mutex<HostTerminal>>,
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
        let mut terminal = modal.lock().unwrap();
        if terminal.finished {
            return;
        }
        let _ = terminal.set_size(size);
        let _ = pty.send(PtyInput::Resize(content_size(size)));
        sender.push(terminal_size_frame(content_size(size)));
    }
}

pub(crate) fn terminal_size_frame(size: TerminalSize) -> Outgoing {
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

#[derive(Default)]
struct TerminalReplies {
    bytes: Vec<u8>,
}

impl vt100::Callbacks for TerminalReplies {
    fn unhandled_osc(&mut self, _screen: &mut vt100::Screen, params: &[&[u8]]) {
        if let [kind, b"?"] = params {
            let color = match *kind {
                b"10" => "cccc/cccc/cccc",
                b"11" => "0000/0000/0000",
                _ => return,
            };
            if self.bytes.len() < 65000 {
                self.bytes.extend_from_slice(
                    format!("\x1b]{};rgb:{color}\x1b\\", String::from_utf8_lossy(kind)).as_bytes(),
                );
            }
        }
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let p = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let reply = match (i1, i2, c, p) {
            (None, None, 'n', 6) => {
                let (r, c) = screen.cursor_position();
                format!("\x1b[{};{}R", r + 1, c + 1)
            }
            (None, None, 'n', 5) => "\x1b[0n".into(),
            (None, None, 'c', 0) => "\x1b[?1;2c".into(),
            (Some(b'>'), None, 'c', 0) => "\x1b[>0;0;0c".into(),
            (None, None, 't', 18) => {
                let (r, c) = screen.size();
                format!("\x1b[8;{r};{c}t")
            }
            (Some(b'?'), Some(b'$'), 'p', _) => format!("\x1b[?{p};0$y"),
            _ => String::new(),
        };
        if self.bytes.len() + reply.len() <= 65536 {
            self.bytes.extend_from_slice(reply.as_bytes());
        }
    }
}

// Track paste/escape framing across reads so pasted text cannot approve a request.
#[derive(Default)]
struct InputGuard {
    sequence: Vec<u8>,
    paste: bool,
}
impl InputGuard {
    fn explicit_key(&mut self, chunk: &[u8]) -> Option<u8> {
        let key = if self.sequence.is_empty() && !self.paste && chunk.len() == 1 {
            Some(chunk[0])
        } else {
            None
        };
        for &byte in chunk {
            if byte == 27 {
                self.sequence.clear();
                self.sequence.push(byte);
            } else if !self.sequence.is_empty() {
                self.sequence.push(byte);
                if self.sequence.len() == 2 && byte != b'[' {
                    self.sequence.clear();
                } else if self.sequence.len() > 2 && (0x40..=0x7e).contains(&byte) {
                    match self.sequence.as_slice() {
                        b"\x1b[200~" => self.paste = true,
                        b"\x1b[201~" => self.paste = false,
                        _ => {}
                    }
                    self.sequence.clear();
                } else if self.sequence.len() > 256 {
                    self.sequence.clear();
                }
            }
        }
        key
    }
}

// The child owns a virtual screen. Only our compositor writes to the physical screen.
pub(crate) struct HostTerminal {
    size: TerminalSize,
    parser: vt100::Parser<TerminalReplies>,
    previous_rows: Vec<Vec<u8>>,
    previous_screen: Option<vt100::Screen>,
    changed_modes: ChangedModes,
    status: SessionStatus,
    dismissed_viewer_id: Option<String>,
    started: bool,
    finished: bool,
    input_guard: InputGuard,
    request_bell_pending: bool,
    #[cfg(test)]
    last_frame: Vec<u8>,
}

impl HostTerminal {
    pub(crate) fn new() -> Self {
        Self {
            size: TerminalSize { cols: 80, rows: 24 },
            parser: vt100::Parser::new_with_callbacks(
                23,
                80,
                SCROLLBACK_LINES,
                TerminalReplies::default(),
            ),
            previous_rows: Vec::new(),
            previous_screen: None,
            changed_modes: ChangedModes::default(),
            status: SessionStatus::default(),
            dismissed_viewer_id: None,
            started: false,
            finished: false,
            input_guard: InputGuard::default(),
            request_bell_pending: false,
            #[cfg(test)]
            last_frame: Vec::new(),
        }
    }

    pub(crate) fn start(&mut self, size: TerminalSize, banner: &[u8]) -> Result<()> {
        self.started = true;
        // Alternate-screen exit restores its saved cursor, attributes and charset.
        let setup = b"\x1b[?1049h\x1b[?6l\x1b[r\x1b(B\x0f\x1b[0m\x1b[2J".to_vec();
        write_local_output(&setup)?;
        self.set_size(size)?;
        self.parser.process(banner);
        self.render()?;
        #[cfg(test)]
        {
            let mut transcript = setup;
            transcript.extend_from_slice(&self.last_frame);
            self.last_frame = transcript;
        }
        Ok(())
    }

    pub(crate) fn stop(&mut self) -> Result<()> {
        if self.finished || !self.started {
            return Ok(());
        }
        self.finished = true;
        let mut cleanup = Vec::new();
        if let Some(previous) = &self.previous_screen {
            // Only release application modes still active. An application's own
            // cleanup already synchronized to the terminal needs no second reset.
            let defaults = vt100::Parser::new(1, 1, 0);
            cleanup.extend_from_slice(&defaults.screen().input_mode_diff(previous));
            if previous.hide_cursor() {
                cleanup.extend_from_slice(b"\x1b[?25h");
            }
        }
        if self.changed_modes.0.contains(&7) {
            cleanup.extend_from_slice(b"\x1b[?7h");
        }
        cleanup.extend_from_slice(b"\x1b[?1049l");
        self.changed_modes.restore(&mut cleanup)?;
        #[cfg(test)]
        {
            self.last_frame = cleanup.clone();
        }
        write_local_output(&cleanup)
    }

    pub(crate) fn set_size(&mut self, size: TerminalSize) -> Result<()> {
        self.size = TerminalSize {
            cols: size.cols.max(1),
            rows: size.rows.max(1),
        };
        let content = content_size(self.size);
        self.parser
            .screen_mut()
            .set_size(content.rows, content.cols);
        self.previous_rows.clear();
        self.render()
    }

    fn handle_pty_output(&mut self, chunk: &[u8]) -> Result<Vec<u8>> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.parser.process(chunk);
        if !self.owns_scroll() {
            self.parser.screen_mut().set_scrollback(0);
        }
        self.render()?;
        Ok(std::mem::take(&mut self.parser.callbacks_mut().bytes))
    }

    fn sync_status(&mut self, status: SessionStatus) -> Result<()> {
        let previous_request = self.request().cloned();
        if status.pending_control_request.is_none() {
            self.dismissed_viewer_id = None;
        }
        self.status = status;
        self.request_bell_pending |=
            self.request().is_some() && self.request() != previous_request.as_ref();
        if self.request().is_some() {
            self.parser.screen_mut().set_scrollback(0);
        }
        self.render()
    }

    fn request(&self) -> Option<&ControlRequest> {
        self.status
            .pending_control_request
            .as_ref()
            .filter(|r| self.dismissed_viewer_id.as_deref() != Some(r.viewer_id.as_str()))
    }

    fn handle_input(&mut self, chunk: &[u8]) -> Result<ModalInput> {
        if self.finished {
            return Ok(ModalInput::Consumed);
        }
        let pasted = self.input_guard.paste;
        let key = self.input_guard.explicit_key(chunk);
        if !pasted {
            if let Some((button, row, pressed)) = mouse_report(chunk) {
                if self.owns_scroll() {
                    if self.request().is_none() && pressed && matches!(button & !28, 64 | 65) {
                        self.scroll(button & 1 == 0)?;
                    }
                    return Ok(ModalInput::Consumed);
                }
                if row > content_size(self.size).rows
                    || self.parser.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None
                {
                    return Ok(ModalInput::Consumed);
                }
            } else if self.parser.screen().scrollback() > 0 {
                self.parser.screen_mut().set_scrollback(0);
                self.render()?;
            }
        }
        let Some(request) = self.request().cloned() else {
            return Ok(ModalInput::Passthrough);
        };
        let approve = match key {
            Some(b'y' | b'Y') => true,
            Some(b'n' | b'N' | b'\r' | b'\n' | 3 | 27) => false,
            _ => return Ok(ModalInput::Consumed),
        };
        self.dismissed_viewer_id = Some(request.viewer_id.clone());
        self.render()?;
        Ok(ModalInput::Decision(if approve {
            ModalDecision::Approve(request)
        } else {
            ModalDecision::Reject(request)
        }))
    }

    fn owns_scroll(&self) -> bool {
        !self.parser.screen().alternate_screen()
            && self.parser.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None
    }

    fn scroll(&mut self, up: bool) -> Result<()> {
        let offset = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(if up {
            offset.saturating_add(3)
        } else {
            offset.saturating_sub(3)
        });
        self.render()
    }

    fn footer(&self) -> (String, &'static str) {
        let (message, color) = self.live_footer();
        if self.parser.screen().scrollback() > 0 {
            (
                format!(
                    " SCROLL -{} |{}",
                    self.parser.screen().scrollback(),
                    message
                ),
                color,
            )
        } else {
            (message, color)
        }
    }

    fn live_footer(&self) -> (String, &'static str) {
        if let Some(request) = self.request() {
            return (
                format!(
                    " Y:allow N:deny | Shello control request | {}s | {}",
                    request.lease_seconds, request.viewer_id
                ),
                STATUS_ATTENTION,
            );
        }
        if self.status.ended {
            return (
                " Shello | SHARING ENDED - local shell only".into(),
                STATUS_NEUTRAL,
            );
        }
        if !self.status.connected {
            return (
                " Shello | DISCONNECTED - reconnecting".into(),
                STATUS_ATTENTION,
            );
        }
        if self.dismissed_viewer_id.is_some() {
            return (" Shello | Sending decision...".into(), STATUS_NEUTRAL);
        }
        if let Some(viewer) = &self.status.controller_viewer_id {
            return (
                format!(
                    " Shello | REMOTE CONTROL | {} | {} watching",
                    viewer, self.status.viewer_count
                ),
                STATUS_CONTROL,
            );
        }
        (
            format!(
                " Shello | SHARING - view only | {} watching",
                self.status.viewer_count
            ),
            STATUS_READY,
        )
    }

    fn render(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let screen = self.parser.screen();
        let rows: Vec<_> = screen.rows_formatted(0, self.size.cols).collect();
        let mut frame = Vec::new();
        self.changed_modes.remember(&[25, 7], &mut frame)?;
        frame.extend_from_slice(b"\x1b[?25l\x1b[?7l");
        for (index, row) in rows.iter().enumerate() {
            if self.previous_rows.get(index) != Some(row) {
                write!(frame, "\x1b[{};1H\x1b[0m\x1b[2K", index + 1)?;
                frame.extend_from_slice(row);
            }
        }
        let (message, color) = self.footer();
        // Status fields are untrusted; use single-column printable ASCII only.
        let message: String = message
            .chars()
            .filter(|c| c.is_ascii() && !c.is_ascii_control())
            .take(self.size.cols as usize)
            .collect();
        if self.size.rows > 1 {
            write!(
                frame,
                "\x1b[{};1H\x1b[0;{}m{:<width$}\x1b[0m",
                self.size.rows,
                color,
                message,
                width = self.size.cols as usize
            )?;
        }
        // The physical terminal reports wheels to Shello only for ordinary shell
        // output. The child parser keeps its own modes and live cursor unchanged.
        let mut physical_modes = vt100::Parser::new(1, 1, 0);
        physical_modes.process(&self.parser.screen().input_mode_formatted());
        if self.owns_scroll() {
            physical_modes.process(b"\x1b[?1000h\x1b[?1006h");
        }
        let hidden = screen.scrollback() > 0 || screen.hide_cursor();
        if hidden {
            physical_modes.process(b"\x1b[?25l");
        }
        let modes = physical_modes.screen();
        let defaults = vt100::Parser::new(1, 1, 0);
        let previous = self
            .previous_screen
            .as_ref()
            .unwrap_or_else(|| defaults.screen());
        for (mode, changed) in [
            (
                1,
                modes.application_cursor() != previous.application_cursor(),
            ),
            (
                66,
                modes.application_keypad() != previous.application_keypad(),
            ),
            (2004, modes.bracketed_paste() != previous.bracketed_paste()),
        ] {
            if changed {
                self.changed_modes.remember(&[mode], &mut frame)?;
            }
        }
        if modes.mouse_protocol_mode() != previous.mouse_protocol_mode() {
            // Mouse tracking modes are mutually exclusive: enabling one may
            // implicitly disable another, so they form one affected group.
            self.changed_modes
                .remember(&[9, 1000, 1002, 1003], &mut frame)?;
        }
        if modes.mouse_protocol_encoding() != previous.mouse_protocol_encoding() {
            self.changed_modes.remember(&[1005, 1006], &mut frame)?;
        }
        frame.extend_from_slice(&modes.input_mode_diff(previous));
        let (row, col) = screen.cursor_position();
        write!(
            frame,
            "\x1b[{};{}H",
            row + 1,
            col.min(self.size.cols - 1) + 1
        )?;
        frame.extend_from_slice(&screen.attributes_formatted());
        if !hidden {
            frame.extend_from_slice(b"\x1b[?25h");
        }
        if self.request_bell_pending {
            frame.push(7);
        }
        #[cfg(test)]
        {
            self.last_frame = frame.clone();
        }
        write_local_output(&frame)?;
        self.request_bell_pending = false;
        self.previous_rows = rows;
        self.previous_screen = Some(modes.clone());
        Ok(())
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

pub(crate) struct ScreenGuard(pub(crate) Arc<Mutex<HostTerminal>>);
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        if let Ok(mut terminal) = self.0.lock() {
            let _ = terminal.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_lines(screen: &vt100::Screen) -> Vec<String> {
        let mut view = screen.clone();
        view.set_scrollback(usize::MAX);
        let mut lines = Vec::new();
        for offset in (1..=view.scrollback()).rev() {
            view.set_scrollback(offset);
            lines.push(view.rows(0, view.size().1).next().unwrap());
        }
        view.set_scrollback(0);
        lines.extend(view.rows(0, view.size().1));
        lines
    }

    #[test]
    fn new_requests_ring_once_and_repeated_status_does_not_ring() {
        let mut terminal = HostTerminal::new();
        let pending = SessionStatus {
            connected: true,
            pending_control_request: Some(ControlRequest {
                viewer_id: "viewer".into(),
                lease_seconds: 60,
            }),
            ..SessionStatus::default()
        };
        terminal.sync_status(pending.clone()).unwrap();
        assert_eq!(terminal.last_frame.iter().filter(|&&b| b == 7).count(), 1);
        terminal.sync_status(pending.clone()).unwrap();
        assert!(!terminal.last_frame.contains(&7));
        terminal.handle_pty_output(b"ongoing output").unwrap();
        assert!(!terminal.last_frame.contains(&7));
        terminal.handle_input(b"n").unwrap();
        terminal
            .sync_status(SessionStatus {
                connected: true,
                ..SessionStatus::default()
            })
            .unwrap();
        terminal.sync_status(pending).unwrap();
        assert!(terminal.last_frame.contains(&7));
    }

    #[test]
    fn real_zsh_ctrl_l_preserves_ordered_output_history() {
        // Captured from zsh -f -i in a 60-column, 9-row PTY, including a real Ctrl-L.
        let frames: Vec<Vec<u8>> =
            serde_json::from_str(include_str!("../tests/fixtures/zsh-ctrl-l.json")).unwrap();
        let mut terminal = HostTerminal::new();
        terminal
            .set_size(TerminalSize { rows: 10, cols: 60 })
            .unwrap();
        for frame in &frames {
            terminal.handle_pty_output(frame).unwrap();
        }
        let lines = all_lines(terminal.parser.screen());
        let numbered: Vec<_> = lines
            .into_iter()
            .filter(|line| line.starts_with("ROW-") || line.starts_with("NEW-"))
            .collect();
        let expected: Vec<_> = (1..=35)
            .map(|n| format!("ROW-{n:03}"))
            .chain((1..=15).map(|n| format!("NEW-{n:03}")))
            .collect();
        assert_eq!(numbered, expected);
        terminal.handle_input(b"\x1b[<64;10;3M").unwrap();
        let mut physical = vt100::Parser::new(10, 60, 0);
        terminal.previous_rows.clear();
        terminal.render().unwrap();
        physical.process(&terminal.last_frame);
        assert_eq!(
            physical.screen().rows(0, 60).take(9).collect::<Vec<_>>(),
            terminal.parser.screen().rows(0, 60).collect::<Vec<_>>()
        );
    }

    #[test]
    fn clearing_scrollback_preserves_live_cursor_attributes_and_removes_old_view() {
        let mut terminal = HostTerminal::new();
        terminal
            .set_size(TerminalSize { rows: 6, cols: 60 })
            .unwrap();
        terminal
            .handle_pty_output(&b"old output\r\n".repeat(20))
            .unwrap();
        terminal.handle_input(b"\x1b[<64;1;1M").unwrap();
        assert!(terminal.parser.screen().scrollback() > 0);
        terminal
            .handle_pty_output(b"\x1b[2;4H\x1b[31m\x1b[3")
            .unwrap();
        terminal.handle_pty_output(b"J").unwrap();
        assert_eq!(terminal.parser.screen().scrollback(), 0);
        assert_eq!(terminal.parser.screen().cursor_position(), (1, 3));
        assert_eq!(all_lines(terminal.parser.screen()).len(), 5);
        terminal.handle_pty_output(b"X").unwrap();
        assert_eq!(
            terminal.parser.screen().cell(1, 3).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
    }

    #[test]
    fn alternate_screen_clear_does_not_archive_tui_redraws() {
        let mut terminal = HostTerminal::new();
        terminal
            .handle_pty_output(b"shell\r\n\x1b[?1049hTUI\x1b[2J\x1b[3J\x1b[?1049l")
            .unwrap();
        assert!(!all_lines(terminal.parser.screen())
            .iter()
            .any(|line| line.contains("TUI")));
    }

    #[test]
    fn wheel_scrolls_output_without_sending_shell_history_keys() {
        let mut terminal = HostTerminal::new();
        terminal
            .set_size(TerminalSize { rows: 6, cols: 80 })
            .unwrap();
        let output: String = (0..40).map(|n| format!("line {n}\r\n")).collect();
        terminal.handle_pty_output(output.as_bytes()).unwrap();
        assert!(matches!(
            terminal.handle_input(b"\x1b[<64;10;3M").unwrap(),
            ModalInput::Consumed
        ));
        let snapshot = terminal.parser.screen().contents();
        assert_eq!(terminal.parser.screen().scrollback(), 3);
        terminal.handle_pty_output(b"new output\r\n").unwrap();
        assert_eq!(terminal.parser.screen().contents(), snapshot);
        assert!(terminal.parser.screen().scrollback() > 0);
        let mut physical = vt100::Parser::new(6, 80, 0);
        terminal.previous_rows.clear();
        terminal.render().unwrap();
        physical.process(&terminal.last_frame);
        assert!(physical
            .screen()
            .rows(0, 80)
            .last()
            .unwrap()
            .contains("SCROLL"));
        while terminal.parser.screen().scrollback() > 0 {
            terminal.handle_input(b"\x1b[<65;10;3M").unwrap();
        }
        assert_eq!(terminal.parser.screen().scrollback(), 0);
        assert!(terminal.parser.screen().contents().contains("new output"));
        terminal.handle_input(b"\x1b[<64;10;3M").unwrap();
        assert!(matches!(
            terminal.handle_input(b"\x1b[A").unwrap(),
            ModalInput::Passthrough
        ));
        assert_eq!(terminal.parser.screen().scrollback(), 0);
    }

    #[test]
    fn tui_wheels_follow_application_mouse_mode() {
        let mut terminal = HostTerminal::new();
        terminal
            .handle_pty_output(b"\x1b[?1049h\x1b[?1003h\x1b[?1006h")
            .unwrap();
        assert!(matches!(
            terminal.handle_input(b"\x1b[<64;10;3M").unwrap(),
            ModalInput::Passthrough
        ));
        assert_eq!(terminal.parser.screen().scrollback(), 0);
        terminal
            .handle_pty_output(b"\x1b[?1003l\x1b[?1006l")
            .unwrap();
        assert!(matches!(
            terminal.handle_input(b"\x1b[<64;10;3M").unwrap(),
            ModalInput::Consumed
        ));
        assert_eq!(
            terminal
                .previous_screen
                .as_ref()
                .unwrap()
                .mouse_protocol_mode(),
            vt100::MouseProtocolMode::None
        );
        terminal.handle_pty_output(b"\x1b[?1049l").unwrap();
        assert_eq!(
            terminal
                .previous_screen
                .as_ref()
                .unwrap()
                .mouse_protocol_mode(),
            vt100::MouseProtocolMode::PressRelease
        );
    }

    #[test]
    fn input_decoder_handles_fragmented_coalesced_wheels_and_normal_keys() {
        let mut decoder = InputDecoder::default();
        assert!(decoder.feed(b"\x1b[").is_empty());
        assert!(decoder.feed(b"<64;10;").is_empty());
        assert!(decoder.flush_key().is_empty());
        assert_eq!(
            decoder.feed(b"3M\x1b[<65;10;3Mls\r"),
            vec![
                b"\x1b[<64;10;3M".to_vec(),
                b"\x1b[<65;10;3M".to_vec(),
                b"ls\r".to_vec()
            ]
        );
        assert!(decoder.feed(b"\x1b").is_empty());
        assert_eq!(decoder.flush_key(), vec![vec![27]]);
        assert_eq!(decoder.feed(b"\x1b[A"), vec![b"\x1b[A".to_vec()]);
    }

    #[test]
    fn pasted_mouse_text_is_not_interpreted_as_scrolling() {
        let mut terminal = HostTerminal::new();
        terminal.handle_input(b"\x1b[200~").unwrap();
        assert!(matches!(
            terminal.handle_input(b"\x1b[<64;10;3M").unwrap(),
            ModalInput::Passthrough
        ));
        assert_eq!(terminal.parser.screen().scrollback(), 0);
        terminal.handle_input(b"\x1b[201~").unwrap();
    }

    #[test]
    fn plain_shell_does_not_reset_unmodified_input_modes() {
        let mut terminal = HostTerminal::new();
        terminal
            .start(TerminalSize { rows: 24, cols: 80 }, b"banner\r\n")
            .unwrap();
        terminal.handle_pty_output(b"hello").unwrap();
        assert!(!terminal.changed_modes.0.contains(&2004));
        assert!(!terminal.changed_modes.0.contains(&1));
        terminal.stop().unwrap();
        let cleanup = String::from_utf8(terminal.last_frame.clone()).unwrap();
        for untouched in ["2004", "66", "[?1l", "[0m", "[r", "[<u"] {
            assert!(
                !cleanup.contains(untouched),
                "unexpected reset: {untouched}"
            );
        }
    }

    #[test]
    fn app_cleanup_is_not_repeated_when_sharing_ends() {
        let mut terminal = HostTerminal::new();
        terminal
            .start(TerminalSize { rows: 24, cols: 80 }, b"")
            .unwrap();
        terminal
            .handle_pty_output(b"\x1b[?1h\x1b=\x1b[?2004h\x1b[?1003h\x1b[?1006h")
            .unwrap();
        terminal
            .handle_pty_output(b"\x1b[?1l\x1b>\x1b[?2004l\x1b[?1003l\x1b[?1006l")
            .unwrap();
        terminal.stop().unwrap();
        let cleanup = String::from_utf8(terminal.last_frame.clone()).unwrap();
        for already_off in ["\x1b[?1l", "\x1b>", "\x1b[?2004l", "\x1b[?1003l"] {
            assert!(!cleanup.contains(already_off));
        }
        assert!(cleanup.contains("\x1b[?2004r"));
    }

    #[test]
    fn sharing_exit_restores_outer_screen_and_cursor_attributes() {
        let mut physical = vt100::Parser::new(24, 80, 0);
        physical.process(b"before sharing\x1b[31m\x1b[2;4H");
        let original = physical.screen().contents();
        let mut terminal = HostTerminal::new();
        terminal
            .start(TerminalSize { rows: 24, cols: 80 }, b"shello banner\r\n")
            .unwrap();
        physical.process(&terminal.last_frame);
        assert!(physical.screen().alternate_screen());
        assert!(!physical.screen().contents().contains("before sharing"));
        terminal
            .handle_pty_output(
                b"\x1b[?25l\x1b[?1h\x1b=\x1b[?2004h\x1b[?1003h\x1b[?1006h\x1b[32;44mTUI",
            )
            .unwrap();
        physical.process(&terminal.last_frame);
        terminal.stop().unwrap();
        physical.process(&terminal.last_frame);
        assert!(!physical.screen().alternate_screen());
        assert_eq!(physical.screen().contents(), original);
        assert_eq!(physical.screen().cursor_position(), (1, 3));
        assert!(!physical.screen().hide_cursor());
        assert!(!physical.screen().application_cursor());
        assert!(!physical.screen().application_keypad());
        assert!(!physical.screen().bracketed_paste());
        assert_eq!(
            physical.screen().mouse_protocol_mode(),
            vt100::MouseProtocolMode::None
        );
        physical.process(b"X");
        assert_eq!(
            physical.screen().cell(1, 3).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
        let cleanup = terminal.last_frame.clone();
        terminal
            .handle_pty_output(b"late background output")
            .unwrap();
        terminal.stop().unwrap();
        assert_eq!(terminal.last_frame, cleanup);
    }

    #[test]
    fn tui_display_cleanup_restores_cursor_colors_and_content() {
        let mut terminal = HostTerminal::new();
        let mut physical = vt100::Parser::new(24, 80, 0);
        terminal.handle_pty_output(b"shell").unwrap();
        physical.process(&terminal.last_frame);
        terminal
            .handle_pty_output(b"\x1b[?1049h\x1b[?25l\x1b[31;44;1;4;7mTUI")
            .unwrap();
        physical.process(&terminal.last_frame);
        assert!(physical.screen().hide_cursor());
        terminal
            .handle_pty_output(b"\x1b[0m\x1b[?25h\x1b[?1049l prompt")
            .unwrap();
        physical.process(&terminal.last_frame);
        assert!(!physical.screen().hide_cursor());
        assert_eq!(
            physical.screen().rows(0, 80).next().unwrap(),
            "shell prompt"
        );
        let cell = physical.screen().cell(0, 6).unwrap();
        assert_eq!(cell.fgcolor(), vt100::Color::Default);
        assert_eq!(cell.bgcolor(), vt100::Color::Default);
        assert!(!cell.bold() && !cell.underline() && !cell.inverse());
    }

    #[test]
    fn exiting_tui_restores_physical_input_modes() {
        let mut terminal = HostTerminal::new();
        terminal
            .set_size(TerminalSize { rows: 24, cols: 80 })
            .unwrap();
        let mut physical = vt100::Parser::new(24, 80, 0);
        physical.process(&terminal.last_frame);
        terminal
            .handle_pty_output(b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?1003h\x1b[?1006h\x1b[?2004h")
            .unwrap();
        physical.process(&terminal.last_frame);
        assert_eq!(
            physical.screen().mouse_protocol_mode(),
            vt100::MouseProtocolMode::AnyMotion
        );
        assert_eq!(
            physical.screen().mouse_protocol_encoding(),
            vt100::MouseProtocolEncoding::Sgr
        );
        terminal
            .handle_pty_output(b"\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b>\x1b[?1049l")
            .unwrap();
        physical.process(&terminal.last_frame);
        assert_eq!(
            physical.screen().mouse_protocol_mode(),
            vt100::MouseProtocolMode::PressRelease
        );
        assert_eq!(
            physical.screen().mouse_protocol_encoding(),
            vt100::MouseProtocolEncoding::Sgr
        );
        assert!(!physical.screen().application_cursor());
        assert!(!physical.screen().application_keypad());
        assert!(!physical.screen().bracketed_paste());
        assert!(matches!(
            terminal.handle_input(b"echo hello\r").unwrap(),
            ModalInput::Passthrough
        ));
        // Re-entry must enable mouse reporting again for applications that request it.
        terminal
            .handle_pty_output(b"\x1b[?1002h\x1b[?1005h")
            .unwrap();
        physical.process(&terminal.last_frame);
        assert_eq!(
            physical.screen().mouse_protocol_mode(),
            vt100::MouseProtocolMode::ButtonMotion
        );
        assert_eq!(
            physical.screen().mouse_protocol_encoding(),
            vt100::MouseProtocolEncoding::Utf8
        );
    }

    #[test]
    fn fullscreen_and_scroll_do_not_overwrite_footer() {
        let mut terminal = HostTerminal::new();
        terminal
            .set_size(TerminalSize { rows: 6, cols: 60 })
            .unwrap();
        let mut physical = vt100::Parser::new(6, 60, 0);
        physical.process(&terminal.last_frame);
        for output in [
            b"\x1b[?1049h\x1b[2J\x1b[HOpenCode".as_slice(),
            b"\x1b[5;1Hbottom\nscroll\nagain",
            b"\x1b[?1049l\x1b[2J\x1b[Hshell",
        ] {
            terminal.handle_pty_output(output).unwrap();
            physical.process(&terminal.last_frame);
            assert!(physical
                .screen()
                .rows(0, 60)
                .last()
                .unwrap()
                .contains("Shello"));
            let expected: Vec<_> = terminal.parser.screen().rows(0, 60).collect();
            let actual: Vec<_> = physical.screen().rows(0, 60).take(5).collect();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn approval_keeps_output_live_and_uses_server_control_state() {
        let mut terminal = HostTerminal::new();
        terminal
            .sync_status(SessionStatus {
                connected: true,
                pending_control_request: Some(ControlRequest {
                    viewer_id: "viewer-1".into(),
                    lease_seconds: 60,
                }),
                ..SessionStatus::default()
            })
            .unwrap();
        terminal.handle_pty_output(b"live output").unwrap();
        assert!(terminal.parser.screen().contents().contains("live output"));
        assert!(terminal.footer().0.contains("Y:allow"));
        assert!(matches!(
            terminal.handle_input(b"yes pasted").unwrap(),
            ModalInput::Consumed
        ));
        assert!(matches!(
            terminal.handle_input(b"y").unwrap(),
            ModalInput::Decision(ModalDecision::Approve(_))
        ));
        assert!(terminal.footer().0.contains("Sending"));
        terminal
            .sync_status(SessionStatus {
                connected: true,
                controller_viewer_id: Some("viewer-1".into()),
                ..SessionStatus::default()
            })
            .unwrap();
        assert!(terminal.footer().0.contains("REMOTE CONTROL"));
        terminal.sync_status(SessionStatus::default()).unwrap();
        assert!(terminal.footer().0.contains("DISCONNECTED"));
    }

    #[test]
    fn fragmented_paste_cannot_approve() {
        let mut guard = InputGuard::default();
        for chunk in [b"\x1b[2".as_slice(), b"00~", b"y", b"\x1b[20", b"1~"] {
            assert_eq!(guard.explicit_key(chunk), None);
        }
        assert_eq!(guard.explicit_key(b"y"), Some(b'y'));
    }

    #[test]
    fn terminal_queries_use_content_dimensions() {
        let mut terminal = HostTerminal::new();
        terminal
            .set_size(TerminalSize { rows: 26, cols: 80 })
            .unwrap();
        let replies = terminal
            .handle_pty_output(b"\x1b[18t\x1b[25;4H\x1b[6n")
            .unwrap();
        assert_eq!(replies, b"\x1b[8;25;80t\x1b[25;4R");
    }
}
