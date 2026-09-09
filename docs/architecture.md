# Architecture

Shello shares one local PTY through a Cloudflare Durable Object. The Agent is the only
component that can create a shell; the browser is a renderer and, after an explicit
grant, an input source.

~~~mermaid
sequenceDiagram
  participant H as Host terminal
  participant A as Rust Agent
  participant D as Durable Object
  participant V as Viewer browser
  H->>A: Start Agent and local PTY
  A->>D: Create or attach host WebSocket
  A-->>H: Print viewer link
  V->>D: Connect viewer WebSocket
  D-->>V: Status, terminal profile, replayed output
  D->>A: terminal.snapshot.request
  A->>D: Current screen snapshot
  D-->>V: Restore this viewer screen
  V->>D: Request remote control
  D->>A: control.request
  A->>H: Local Y/N prompt
  H->>A: Approve or reject
  A->>D: control.approve or control.reject
  V->>D: Input only after grant
  D->>A: PTY stdin
~~~

## Responsibilities

| Component | Owns |
| --- | --- |
| Rust Agent | local PTY/shell, terminal size and profile, reconnection, local authorization prompt |
| Durable Object | session lifecycle, host/viewer membership, control lease, bounded output replay |
| Web client | xterm.js rendering, stable viewer identity, viewer reconnect, input chunking and backpressure |
| Worker | HTTP APIs, static client assets, bootstrap scripts, download proxy |

The host Agent connects outward; no listener is opened on the host machine. A Worker
creates session IDs in the form <code>xxx-xxx</code> and routes each session to its
Durable Object. The Agent's printed viewer URL is the capability needed to observe that
session.

## Session lifecycle

A session starts <code>idle</code>, becomes <code>ready</code> when a viewer is present,
and becomes <code>active</code> once the host connects. The Durable Object persists the
lifecycle metadata, while the hot terminal replay buffer and current terminal
profile/size remain in memory.

- An idle session expires after two hours; all sessions have a 24-hour maximum lifetime.
- Normal shell exit sends `session.end`; the server persists the ended sharing state and
  acknowledges `session.ended`. Viewers immediately see that the host ended sharing.
- Unexpected host disconnection has a 180-second grace period. Viewers see a countdown;
  pending requests and control leases are cleared and require fresh approval after
  reconnection. Grace timeout ends the current sharing run; the pairing code remains reusable.
- Pairing-code expiration, independently of a sharing run, rejects connections with
  HTTP 410. Socket restoration cannot revive expired codes. Viewers remain connected
  after a run ends and receive status when a host reuses the code. Each host connection
  starts a new output generation, clearing old replay, screen contents and permissions.
- Agents send WebSocket pings every 15 seconds and reconnect after 45 seconds without
  incoming traffic. Normal exit waits at most three seconds for the transport thread;
  a lost network can prevent delivery of the exit notification.
- The exit message includes a platform-specific command with the current session ID
  to reconnect a new shell to the same link. Both web-created and Agent-created
  sessions support this. Previous programs are not restored; the pairing code retains
  its existing idle and maximum lifetime limits.
- Browser viewers retain an identity token in session storage. Only one WebSocket is
  active for a given identity; a newer connection replaces the older one.
- Replayed PTY output is capped at 2 MiB. Each viewer connection also requests the
  current screen from the Agent, restoring an idle prompt even when the Durable Object
  has hibernated and its replay cache is empty. Snapshots contain current screen buffers
  and input modes, without scrollback or the host status bar.

## Input and authorization

Viewers are read-only until they request remote control. The Durable Object forwards the
request to the Agent, which displays it in the host's real terminal. The host uses
<code>Y</code> to grant and <code>N</code>, Return, Ctrl-C, or Escape to deny. A control
lease is bounded to at most 30 minutes (with a 60-second minimum) and pending requests
expire after 30 seconds.

The Agent reserves the physical terminal's bottom row for a persistent local status
bar. An embedded VT100 emulator renders the child PTY into the remaining rows, isolating
clear-screen, alternate-screen, and scrolling operations from the bar. Approval consumes
local keys while application output continues to both host and viewers. Control state
comes from server status; disconnection clears pending local authorization.

The compositor uses an alternate screen and restores the original terminal on exit.
Private modes are saved only immediately before Shello changes them. Application
mode changes follow the emulator's state diff. Cleanup releases only modes still
active and restores the affected modes on terminals supporting xterm mode saving;
unrelated modes are left untouched. Screen and cursor restoration use alternate-screen
exit rather than a blanket display reset. Unix
termios and Windows console modes and code pages are restored by the platform guard.
Application mode changes are synchronized for both enabling and disabling; unsupported
escape sequences remain inside the virtual terminal.
It supports standard text, colors, cursor movement, and mouse input modes; terminal
image protocols and enhanced keyboard protocols are not advertised. The compositor
retains up to 10,000 primary-screen scrollback lines. For a primary-screen application
without mouse reporting, Shello requests SGR mouse reports and consumes wheel events
to scroll the live primary-screen history. New PTY output and terminal-query replies remain live.
Scrolling to the bottom or keyboard input restores the live view. CSI 2 J moves the
occupied viewport into bounded history before erasing it; CSI 3 J erases only scrollback.
The vendored vt100 parser implements these operations without resetting cursor,
attributes, or saved terminal state. Browser xterm uses scrollOnEraseInDisplay for
matching clear-screen behavior. Mouse modes requested
by applications remain authoritative; alternate screens have no Shello scrollback.
Fragmented mouse reports are assembled before routing so coordinates cannot become
shell input. The renderer tracks its effective physical modes separately from the child.
Outside local approval, keyboard input is forwarded to the child. Full-screen TUI
applications manage their own scrolling through standard terminal modes.
The compositor does not read the outer terminal's pre-sharing display. The browser
receives the original PTY stream and the content dimensions, excluding the host-only
status bar, and retains received output for browser-side scrolling.

## Terminal protocol and compatibility

The host size is authoritative. The Agent publishes its initial PTY dimensions and
updates them as the terminal is resized; viewers ask for a fresh size/profile after
connecting or recovering. Browsers proportionally scale the complete terminal canvas
up or down to fit the viewport, preserving the host's layout and centering any remaining
space. Browser resizing never changes the host PTY dimensions.

| Direction | Message | Purpose |
| --- | --- | --- |
| Agent → Durable Object | binary type <code>0x01</code> | PTY output |
| Viewer → Durable Object | binary type <code>0x02</code> | PTY stdin, only with a control lease |
| Agent ↔ Durable Object | <code>terminal.size</code>, <code>terminal.profile</code> | host geometry and PTY compatibility |
| Durable Object → Agent | <code>terminal.size.request</code>, <code>terminal.profile.request</code> | recover metadata after a connection change |
| Durable Object → Agent | <code>terminal.snapshot.request</code> | request a screen for a specific viewer connection |
| Agent → Durable Object → Viewer | <code>terminal.snapshot</code> | restore current screen, cursor, modes and dimensions |
| Agent ↔ Durable Object | <code>session.end</code>, <code>session.ended</code> | explicit sharing termination and acknowledgement |
| Viewer / Agent | <code>control.request</code>, <code>control.approve</code>, <code>control.reject</code> | remote-control authorization |

The Agent maintains a screen model from the ordered outgoing stream. Snapshot replies
are emitted between complete UTF-8 characters and terminal escape sequences, before
subsequent output. The browser queues snapshot rendering with normal output. Connection
request IDs are stored in WebSocket attachments so routing survives hibernation and
replies for replaced connections are ignored. Screen recovery does not send shell input
or resize the host.

Unix Agents publish the normal terminal profile. Windows Agents publish ConPTY, which
enables xterm.js's Windows-specific wrapping and scrollback behavior. The client uses
ResizeObserver, visualViewport, and window resize events to keep the display visible in
split panes and when mobile virtual keyboards appear.

The data path applies explicit bounds: 1 MiB HTTP body and WebSocket-message limits,
16 KiB Agent output batches, a 4 MiB browser output staging limit, 32 KiB browser input
chunks, and a 512 KiB browser buffered-amount threshold. These limits favor a visible
backpressure state or bounded replay over hidden data loss. The Agent buffers at most
1 MiB / 256 output messages. Overflow invalidates the connection instead of skipping
bytes within a live stream. A producer-side terminal model keeps processing all output;
reconnection starts with an atomic screen snapshot followed by its ordered continuation.
Snapshots wait for complete UTF-8 and control sequences. Network I/O never holds the
output-state lock. Scrollback during congestion is not guaranteed.

## Bootstrap and downloads

<code>/start</code> and <code>/start.ps1</code> select an OS/architecture-specific
Agent asset, download it together with <code>checksums.txt</code>, verify SHA-256, and
run it attached to the current terminal. Linux first uses a sub-1 MiB Agent linked to
the host TLS runtime. If its harmless <code>--version</code> probe cannot start, the
shell bootstrap verifies and switches to the same-architecture <code>-portable</code>
statically linked musl/Rustls asset. Each bootstrap invocation uses a separate temporary
directory, removed after the Agent exits, so simultaneous sessions do not overwrite
running binaries. In local development the Worker serves locally built assets under
<code>/downloads/local/</code>; deployed environments use the configured GitHub release
or the explicit binary/checksum URLs.

See [the root README](../README.md) for installation and
[the release guide](releasing.md) for the delivery process.
