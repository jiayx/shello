use crate::platform::{terminal_size, PtyHandle, TerminalSize};
use crate::protocol::{
    terminal_size_text, ControlRequest, Outgoing, PtyInput, REMOTE_OUTPUT_BATCH_SIZE,
};
use crate::Result;
use std::env;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const TRACE_ENV: &str = "SHELLO_TRACE";
const MAX_MODAL_BUFFER: usize = 1024 * 1024;

pub(crate) fn enqueue<T>(sender: &SyncSender<T>, value: T) {
    let _ = sender.try_send(value);
}

pub(crate) fn enqueue_control(sender: &SyncSender<Outgoing>, value: Outgoing) {
    let _ = sender.try_send(value);
}

pub(crate) fn pty_output_loop(
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

pub(crate) fn pty_input_loop(mut pty: PtyHandle, input: Receiver<PtyInput>) -> Result<()> {
    while let Ok(message) = input.recv() {
        match message {
            PtyInput::Bytes(bytes) => pty.write_all(&bytes)?,
            PtyInput::Resize(size) => pty.resize(size)?,
        }
    }
    Ok(())
}

pub(crate) fn status_loop(
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

pub(crate) fn resize_loop(
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

pub(crate) fn terminal_size_frame(size: TerminalSize) -> Outgoing {
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

pub(crate) struct ApprovalModal {
    active: bool,
    size: TerminalSize,
    request: Option<ControlRequest>,
    dismissed_viewer_id: Option<String>,
    buffered: Vec<u8>,
    dropped_buffered_output: bool,
}

impl ApprovalModal {
    pub(crate) fn new() -> Self {
        Self {
            active: false,
            size: TerminalSize { cols: 80, rows: 24 },
            request: None,
            dismissed_viewer_id: None,
            buffered: Vec::new(),
            dropped_buffered_output: false,
        }
    }

    pub(crate) fn set_size(&mut self, size: TerminalSize) -> Result<()> {
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

        if !self.active && self.dismissed_viewer_id.as_deref() == Some(request.viewer_id.as_str()) {
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
                    " shello control request: viewer {viewer}, {lease}m lease. Press Y to approve or N to deny. "
                ),
            );

        write_local_output(
            format!("\x1b7\x1b[{row};1H\x1b[2K\x1b[7m{message:<width$}\x1b[0m\x07\x1b8").as_bytes(),
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
            write_local_output(b"\r\n[shello-agent: local output was truncated while control approval was pending]\r\n")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(viewer_id: &str, lease_seconds: i32) -> ControlRequest {
        ControlRequest {
            viewer_id: viewer_id.to_string(),
            lease_seconds,
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
}
