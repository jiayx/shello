# Architecture

ttys shares one local PTY through a Cloudflare Durable Object. The Agent is the only
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
  V->>D: Request remote control
  D->>A: control.request
  A->>H: Local Y/N prompt
  H->>A: Approve or reject
  A->>D: control.grant or control.deny
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
- If the host disconnects, viewers see a reconnecting state. The host has a 60-second
  grace period before the session closes.
- Browser viewers retain an identity token in session storage. Only one WebSocket is
  active for a given identity; a newer connection replaces the older one.
- Replayed PTY output is capped at 2 MiB, so a reconnecting viewer can catch up without
  making a long-running session unbounded.

## Input and authorization

Viewers are read-only until they request remote control. The Durable Object forwards the
request to the Agent, which displays it in the host's real terminal. The host uses
<code>Y</code> to grant and <code>N</code>, Return, Ctrl-C, or Escape to deny. A control
lease is bounded to at most 30 minutes (with a 60-second minimum) and pending requests
expire after 30 seconds.

The Agent pauses remote PTY output while that decision is shown, buffering at most
1 MiB. After a decision it restores the output stream. This protects the host prompt
from being buried by a noisy command and prevents a slow viewer from creating an
unbounded queue.

## Terminal protocol and compatibility

The host size is authoritative. The Agent publishes its initial PTY dimensions and
updates them as the terminal is resized; viewers ask for a fresh size/profile after
connecting or recovering. Browsers adapt only their display surface and never resize
the host PTY.

| Direction | Message | Purpose |
| --- | --- | --- |
| Agent → Durable Object | binary type <code>0x01</code> | PTY output |
| Viewer → Durable Object | binary type <code>0x02</code> | PTY stdin, only with a control lease |
| Agent ↔ Durable Object | <code>terminal.size</code>, <code>terminal.profile</code> | host geometry and PTY compatibility |
| Durable Object → Agent | <code>terminal.size.request</code>, <code>terminal.profile.request</code> | recover metadata after a connection change |
| Viewer / Agent | <code>control.request</code>, <code>control.grant</code>, <code>control.deny</code> | remote-control authorization |

Unix Agents publish the normal terminal profile. Windows Agents publish ConPTY, which
enables xterm.js's Windows-specific wrapping and scrollback behavior. The client uses
ResizeObserver, visualViewport, and window resize events to keep the display visible in
split panes and when mobile virtual keyboards appear.

The data path applies explicit bounds: 1 MiB HTTP body and WebSocket-message limits,
16 KiB Agent output batches, a 4 MiB browser output staging limit, 32 KiB browser input
chunks, and a 512 KiB browser buffered-amount threshold. These limits favor a visible
backpressure state or bounded replay over hidden data loss.

## Bootstrap and downloads

<code>/start</code> and <code>/start.ps1</code> select an OS/architecture-specific
Agent asset, download it together with <code>checksums.txt</code>, verify SHA-256, and
run it attached to the current terminal. In local development the Worker serves locally
built assets under <code>/downloads/local/</code>; deployed environments use the
configured GitHub release or the explicit binary/checksum URLs.

See [the root README](../README.md) for installation and
[the release guide](releasing.md) for the delivery process.
