use crate::protocol::Outgoing;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

struct State {
    screen: vt100::Parser,
    queue: VecDeque<Outgoing>,
    bytes: usize,
    capacity: usize,
    overflowed: bool,
}

#[derive(Clone)]
pub(crate) struct OutputSender(Arc<Mutex<State>>);
pub(crate) struct OutputReceiver(Arc<Mutex<State>>);

pub(crate) fn channel(capacity: usize) -> (OutputSender, OutputReceiver) {
    let state = Arc::new(Mutex::new(State {
        screen: vt100::Parser::new(24, 80, 0),
        queue: VecDeque::new(),
        bytes: 0,
        capacity,
        overflowed: false,
    }));
    (OutputSender(Arc::clone(&state)), OutputReceiver(state))
}

fn size(message: &Outgoing) -> usize {
    match message {
        Outgoing::Tty(bytes) => bytes.len(),
        Outgoing::Text(text) => text.len(),
    }
}

impl OutputSender {
    pub(crate) fn push(&self, message: Outgoing) {
        let mut state = self.0.lock().unwrap();
        // Track every byte on the producer side, including while disconnected.
        // This lock is never held during network I/O.
        match &message {
            Outgoing::Tty(bytes) => state.screen.process_for_snapshot(bytes),
            Outgoing::Text(text) => crate::transport::track_terminal_size(&mut state.screen, text),
        }
        if state.overflowed {
            return;
        }
        let bytes = size(&message);
        if state.queue.len() >= 256 || bytes > state.capacity.saturating_sub(state.bytes) {
            state.queue.clear();
            state.bytes = 0;
            state.overflowed = true;
            return;
        }
        state.bytes += bytes;
        state.queue.push_back(message);
    }
}

impl OutputReceiver {
    pub(crate) fn overflowed(&self) -> bool {
        self.0.lock().unwrap().overflowed
    }

    // Atomically pair a screen snapshot with the start of its continuation.
    // Incomplete UTF-8 / escape sequences must finish before taking a snapshot.
    pub(crate) fn restart(&self) -> Option<(String, Vec<u8>)> {
        let mut state = self.0.lock().unwrap();
        if !state.screen.snapshot_ready() {
            return None;
        }
        let (rows, cols) = state.screen.screen().size();
        let dimensions =
            crate::protocol::terminal_size_text(crate::platform::TerminalSize { rows, cols });
        let snapshot = state.screen.screen().snapshot_formatted();
        state.queue.clear();
        state.bytes = 0;
        state.overflowed = false;
        Some((dimensions, snapshot))
    }

    pub(crate) fn try_recv(&self) -> Result<Outgoing, std::sync::mpsc::TryRecvError> {
        let mut state = self.0.lock().unwrap();
        let message = state
            .queue
            .pop_front()
            .ok_or(std::sync::mpsc::TryRecvError::Empty)?;
        state.bytes -= size(&message);
        Ok(message)
    }

    pub(crate) fn try_iter(&self) -> impl Iterator<Item = Outgoing> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_keeps_tracking_and_restarts_at_an_atomic_boundary() {
        let (tx, rx) = channel(8);
        tx.push(Outgoing::Tty(b"prompt> ".to_vec()));
        tx.push(Outgoing::Tty(b"\x1b[32mhello".to_vec()));
        assert!(rx.overflowed());
        assert!(rx.try_recv().is_err());
        tx.push(Outgoing::Tty(b" world".to_vec()));
        let (_, snapshot) = rx.restart().unwrap();
        let mut viewer = vt100::Parser::new(24, 80, 0);
        viewer.process(&snapshot);
        assert_eq!(viewer.screen().contents(), "prompt> hello world");
        assert!(!rx.overflowed());
        tx.push(Outgoing::Tty(b"!".to_vec()));
        let Outgoing::Tty(next) = rx.try_recv().unwrap() else {
            panic!("expected output")
        };
        viewer.process(&next);
        assert_eq!(viewer.screen().contents(), "prompt> hello world!");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn overflow_waits_for_complete_utf8_and_escape_sequences() {
        let (tx, rx) = channel(1);
        tx.push(Outgoing::Tty(vec![0xe4, 0xb8]));
        assert!(rx.restart().is_none());
        tx.push(Outgoing::Tty(vec![0xad, 27, b'[']));
        assert!(rx.restart().is_none());
        tx.push(Outgoing::Tty(b"0m".to_vec()));
        assert!(rx.restart().is_some());
    }
}
