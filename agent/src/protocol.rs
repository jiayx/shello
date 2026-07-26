use crate::platform::TerminalSize;
use serde::Deserialize;

pub(crate) const BINARY_TTY_OUTPUT: u8 = 0x01;
pub(crate) const BINARY_STDIN: u8 = 0x02;
pub(crate) const REMOTE_OUTPUT_BATCH_SIZE: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct ControlRequest {
    #[serde(rename = "viewerId")]
    pub(crate) viewer_id: String,
    #[serde(rename = "leaseSeconds")]
    pub(crate) lease_seconds: i32,
}

#[derive(Deserialize)]
pub(crate) struct SessionStatus {
    #[serde(rename = "pendingControlRequest")]
    pub(crate) pending_control_request: Option<ControlRequest>,
}

#[derive(Deserialize)]
pub(crate) struct Envelope {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) payload: serde_json::Value,
}

pub(crate) enum Outgoing {
    Text(String),
    Tty(Vec<u8>),
}
pub(crate) enum PtyInput {
    Bytes(Vec<u8>),
    Resize(TerminalSize),
}
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum TerminalRequest {
    Profile,
    Size,
}

pub(crate) fn terminal_profile_text() -> String {
    let platform = if cfg!(windows) { "windows" } else { "unix" };
    if platform == "windows" {
        serde_json::json!({"type":"terminal.profile","payload":{"platform":platform,"pty":"conpty"}}).to_string()
    } else {
        serde_json::json!({"type":"terminal.profile","payload":{"platform":platform}}).to_string()
    }
}

pub(crate) fn terminal_size_text(size: TerminalSize) -> String {
    serde_json::json!({"type":"terminal.size","payload":{"cols":size.cols,"rows":size.rows}})
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_profile_describes_the_current_pty_platform() {
        let profile: serde_json::Value = serde_json::from_str(&terminal_profile_text()).unwrap();
        let expected_platform = if cfg!(windows) { "windows" } else { "unix" };

        assert_eq!(profile["type"], "terminal.profile");
        assert_eq!(profile["payload"]["platform"], expected_platform);
        if cfg!(windows) {
            assert_eq!(profile["payload"]["pty"], "conpty");
        } else {
            assert!(profile["payload"].get("pty").is_none());
        }
    }

    #[test]
    fn serializes_terminal_size_frame() {
        let text = terminal_size_text(TerminalSize {
            cols: 120,
            rows: 40,
        });
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            serde_json::json!({"type":"terminal.size","payload":{"cols":120,"rows":40}})
        );
    }
}
