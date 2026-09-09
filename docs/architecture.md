# Architecture

Shello connects a local shell to browser viewers through Cloudflare. Only the Agent
creates the shell and writes to its PTY; the server enforces viewer control permissions.

~~~mermaid
flowchart LR
  H[Host terminal] <--> A[Rust Agent / PTY]
  A <-->|Outbound WebSocket| D[Durable Object]
  D <--> V[Browser / xterm.js]
  W[Worker / API and downloads] --> D
~~~

| Component | Responsibility |
| --- | --- |
| Agent | Shell, local status bar, terminal state, host approval, reconnection |
| Durable Object | Session lifecycle, connections, control leases |
| Web client | Terminal rendering, viewer input, reconnect, Chinese/English interface |
| Worker | Session API, static assets, bootstrap scripts, download proxy |

## Sessions and permissions

Session IDs are reusable pairing codes in `xxx-xxx` format. An idle session expires
after two hours; the maximum lifetime is 24 hours.

| State | Meaning |
| --- | --- |
| `idle` | No host or viewer has joined |
| `ready` | Waiting for a host |
| `active` | Host connected |
| `ended` | Sharing ended; the pairing code remains usable until expiration |
| `closed` | Pairing code expired; new connections receive HTTP 410 |

Normal shell exit sends `session.end`, acknowledged by `session.ended`. An unexpected
host disconnection has a 180-second grace period before sharing ends. Viewers remain
connected to receive status and a subsequent host connection. Each host connection
clears control permissions and starts a new output generation.

Viewers need local host approval to type. Requests expire after 30 seconds; control
leases last 60 seconds to 30 minutes. Host disconnection clears pending requests and
leases. A controller can release its lease from the web toolbar; the host can revoke
it through the local status bar. Both operations keep the shell and viewer connections
active. Viewer identity is stored per session in `sessionStorage`; a new connection
with the same identity replaces the previous one.

The Agent retries connections with 250 ms–5 s exponential backoff, sends pings every
15 seconds, and reconnects after 45 seconds without incoming traffic. Normal exit waits
up to three seconds for the transport thread; an unavailable network can prevent the
exit notification from reaching the server.

## Terminal display

The host's dimensions are authoritative. Browsers scale and center the terminal canvas;
resizing a browser does not resize the host PTY. Windows hosts identify ConPTY so
xterm.js can use its corresponding wrapping and scrollback behavior.

The Agent uses a vendored vt100 parser to render the child shell above a fixed status
bar in the local alternate screen. The status bar and pre-sharing terminal contents
are not sent to viewers. Each new approval request emits one terminal bell. The status bar has clickable
approve/deny actions with keyboard equivalents; footer clicks are consumed locally.

Ordinary shells have up to 10,000 lines of local scrollback. Mouse-wheel input scrolls
history; typing or returning to the bottom resumes the live view. Full-screen apps use
their own input modes. CSI 2 J preserves cleared output in history; CSI 3 J clears
scrollback. Terminal image and enhanced keyboard protocols are not advertised.

Exit leaves the alternate screen and restores OS terminal settings. Private modes
changed by Shello are saved and restored where the terminal supports xterm mode saving.
Windows enables virtual terminal input and disables Quick Edit while sharing, then
restores console modes and code pages.

## Screen recovery and congestion

A viewer connection requests a current-screen snapshot, including screen buffers,
cursor, dimensions, and input modes. Snapshots exclude scrollback. Requests identify a
specific viewer connection; replies for replaced connections are ignored. The server forwards live output without retaining a replay buffer.

The Agent maintains two screen models: one receives every PTY output byte, while the
other follows the current connection's outgoing stream for viewer snapshots. Snapshots
are taken only between complete UTF-8 characters and terminal control sequences.

The output queue holds at most 1 MiB / 256 messages. Overflow invalidates the connection
and clears queued output. The producer-side model continues updating; reconnection
starts with its screen snapshot followed by ordered live output. Network I/O never
holds the output-state lock. Scrollback during congestion is not guaranteed.

Other limits: Agent output batches are 16 KiB; HTTP bodies and WebSocket messages are
limited to 1 MiB. The browser stages up to 4 MiB of output, chunks input at 32 KiB, and
uses a 512 KiB outgoing buffered-amount threshold.

## Protocol

| Message | Purpose |
| --- | --- |
| Binary `0x01` / `0x02` | Host output / authorized viewer input |
| `session.status` | Lifecycle, host state, viewers and control permissions |
| `terminal.size`, `terminal.profile` | Host geometry and PTY metadata |
| `terminal.size.request`, `terminal.profile.request` | Request host metadata |
| `terminal.snapshot.request`, `terminal.snapshot` | Request and restore a viewer's screen |
| `control.request`, `control.approve`, `control.reject`, `control.release`, `control.revoke` | Host authorization |
| `session.end`, `session.ended` | End sharing and acknowledge |

The Durable Object persists lifecycle metadata and WebSocket attachments. Terminal
profile/size caches stay in memory. Browser snapshot rendering shares the
normal output queue to preserve ordering.

For bootstrap delivery and platform binaries, see [deployment and releases](releasing.md).
