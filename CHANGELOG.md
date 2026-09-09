# Release notes

## [Unreleased]

## [0.3.2] - 2026-09-09

### Added

- Host status-bar mouse actions for approving, rejecting, and revoking remote control.
- Web control release without disconnecting the shared shell.

### Changed

- Distinct status colors for read-only access, active control, and pending requests.
- Centered terminal display with a thin scrollbar and a small surrounding gutter.
- Current-protocol snapshot synchronization with automatic retry on timeout.
- Agent version appears in the shared shell and fatal error messages.

### Fixed

- Overlapping xterm scrollbar styles.
- Duplicate Agent startup output, terminal sizing, and initial rendering.
- Trace-file initialization errors interfering with the shared terminal display.

## [0.3.1] - 2026-09-09

### Added

- Automatic Chinese/English interface.
- Persistent host status bar, control-request alerts, and shell scrollback.
- Current-screen recovery on refresh and after network congestion.
- Reusable session links with a three-minute host reconnection grace period.
- Separate server and host status; read-only guidance for unmodified terminal keys.
- Windows virtual terminal input with console settings restored on exit.
- Isolated bootstrap downloads for concurrent sessions; static musl/Rustls Linux fallback.

## [0.3.0] - 2026-09-09

### Changed

- Shello branding with the `shello-agent` CLI and `SHELLO_*` environment variables.
- Full-width terminal, compact toolbar, and session-details drawer.
- New sessions open in a separate tab, preserving the original connection.
- Narrow viewports support terminal scrolling; clipboard fallback works in session details.
- Linux downloads use system TLS with a verified Rustls fallback.
- Versioned Agent releases include platform binaries and checksums.
