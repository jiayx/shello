# ttys-agent

The single supported host agent for ttys, written in Rust. It runs the local shell in a
PTY and connects that PTY to a ttys session over an outbound WebSocket.

It creates or attaches to a terminal-sharing session, launches the local shell in a
PTY, forwards terminal I/O over WebSocket, reconnects after transient failures, and
requires local approval before a viewer can control the shell. It also publishes
terminal geometry and its PTY profile; Windows builds identify ConPTY so the browser
can choose matching xterm.js behavior.

## Behavior

- Creates a session through an HTTP(S) ttys server, or attaches to an existing
  <code>xxx-xxx</code> session.
- Launches a shell inside a local PTY and mirrors output to the host terminal and the
  remote session.
- Sends an initial terminal size and profile, then polls for size changes every 250 ms.
- Reconnects the host WebSocket with exponential backoff from 250 ms to 5 seconds.
- Batches remote PTY output in up to 16 KiB frames and uses bounded queues to avoid
  unbounded memory growth.
- Prompts in the local terminal for every remote-control request: <code>Y</code>
  approves; <code>N</code>, Return, Ctrl-C, and Escape reject.

While an approval prompt is visible, PTY output is held locally (up to 1 MiB) and is not
sent to viewers. The buffer is restored after the decision; if it fills, the Agent shows
a local warning. This prevents remote activity from obscuring the host's authorization
decision.

## Usage

Create a new session:

~~~bash
cargo run -- -server http://localhost:5173
~~~

Attach to a known session:

~~~bash
cargo run -- -server http://localhost:5173 -session abc-def
~~~

Choose a shell explicitly:

~~~bash
cargo run -- -server https://ttys.example -shell /bin/bash
~~~

Flags:

- <code>-server &lt;url&gt;</code> (default <code>http://localhost:5173</code>): an
  HTTP(S) server base URL, or a direct <code>ws(s)</code> host endpoint.
- <code>-session &lt;xxx-xxx&gt;</code>: attach to an existing session; accepted only
  with an HTTP(S) server URL.
- <code>-shell &lt;path&gt;</code>: executable to start in the PTY.

On Unix the default comes from <code>$SHELL</code> and falls back to
<code>/bin/sh</code>. On Windows the Agent prefers PowerShell 7, Windows PowerShell,
then <code>cmd.exe</code>.

After connecting, the Agent prints a viewer link. Its HTTP parser and WebSocket frames
are deliberately bounded: HTTP bodies are limited to 1 MiB, individual WebSocket
messages to 1 MiB, and modal buffering to 1 MiB.

## Diagnostics

Set <code>TTYS_TRACE</code> to record raw PTY output for browser-rendering diagnostics:

~~~bash
TTYS_TRACE=/tmp/ttys.trace cargo run -- -server http://localhost:5173
~~~

Load that file in <code>/debug/replay</code> on the same ttys web deployment to
reproduce output in xterm.js without a WebSocket session.

## Verification

~~~bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --release --all-targets
cargo build --locked --release
~~~

For the complete protocol and session model, see
[the architecture guide](../docs/architecture.md).
