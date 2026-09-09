# shello-agent

The Shello host agent is written in Rust. It runs the local shell in a
PTY and connects that PTY to a Shello session over an outbound WebSocket.

It creates or attaches to a terminal-sharing session, launches the local shell in a
PTY, forwards terminal I/O over WebSocket, reconnects after transient failures, and
requires local approval before a viewer can control the shell. It also publishes
terminal geometry and its PTY profile; Windows builds identify ConPTY so the browser
can choose matching xterm.js behavior.

## Behavior

- Creates a session through an HTTP(S) Shello server, or attaches to an existing
  <code>xxx-xxx</code> session.
- Launches a shell inside a local PTY and mirrors output to the host terminal and the
  remote session.
- Sends an initial terminal size and profile, then polls for size changes every 250 ms.
- Reconnects the host WebSocket with exponential backoff from 250 ms to 5 seconds.
- Batches remote PTY output in up to 16 KiB frames and uses bounded queues to avoid
  unbounded memory growth.
- Prompts in the local terminal for every remote-control request: <code>Y</code>
  approves; <code>N</code>, Return, Ctrl-C, and Escape reject.

A persistent local status bar displays sharing, control, approval, and connection state.
The child PTY uses the remaining rows. Output continues while approval is pending.
Each new control request emits one terminal bell; repeated status updates do not repeat
it. The terminal application determines whether the bell is audible, visual, or muted.
The compositor synchronizes supported input and display modes with the child, and
isolates unsupported terminal-control sequences from the physical terminal. On exit,
it leaves the alternate screen and restores OS terminal settings. Only private modes
actually affected by Shello are saved and restored on terminals with xterm mode-saving
support. Application modes already disabled by the child are not reset again; only
still-active modes are released before leaving.

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
cargo run -- -server https://shello.example -shell /bin/bash
~~~

Show the embedded Agent version:

~~~bash
cargo run -- --version
# shello-agent 0.3.0
~~~

The Agent also prints its version as the first line of a normal startup. Release
binaries embed the package version from <code>agent/Cargo.toml</code>.

## TLS delivery

The default build uses the operating system TLS provider. Linux release assets therefore
depend on the host's OpenSSL runtime but remain below 1 MiB. The release also includes a
<code>-portable</code> Linux asset built with Rustls and bundled trust roots. The
<code>/start</code> bootstrap verifies the lightweight asset first and automatically falls
back to that portable asset only if the lightweight binary cannot start.

The two backends are mutually exclusive Cargo features:

~~~bash
# Default lightweight system-TLS build
cargo build --release --no-default-features --features native-tls

# Portable Linux fallback
cargo build --release --no-default-features --features rustls-tls
~~~

Flags:

- <code>-server &lt;url&gt;</code> (default <code>http://localhost:5173</code>): an
  HTTP(S) server base URL, or a direct <code>ws(s)</code> host endpoint.
- <code>-session &lt;xxx-xxx&gt;</code>: attach to an existing session; accepted only
  with an HTTP(S) server URL.
- <code>-shell &lt;path&gt;</code>: executable to start in the PTY.
- <code>--version</code> or <code>-V</code>: print the embedded version and exit.

On Unix the default comes from <code>$SHELL</code> and falls back to
<code>/bin/sh</code>. On Windows the Agent prefers PowerShell 7, Windows PowerShell,
then <code>cmd.exe</code>.

After connecting, the Agent prints a viewer link. Its HTTP parser and WebSocket frames
are deliberately bounded: HTTP bodies are limited to 1 MiB, individual WebSocket
messages to 1 MiB, and modal buffering to 1 MiB.

## Diagnostics

Set <code>SHELLO_TRACE</code> to record raw PTY output for browser-rendering diagnostics:

~~~bash
SHELLO_TRACE=/tmp/shello.trace cargo run -- -server http://localhost:5173
~~~

Load that file in <code>/debug/replay</code> on the same Shello web deployment to
reproduce output in xterm.js without a WebSocket session.

## Source layout and tests

The binary entry point in <code>src/main.rs</code> owns startup orchestration and
top-level error handling. Its supporting code is split into a small set of
responsibility-based modules: <code>cli</code>, <code>connection</code>,
<code>protocol</code>, <code>terminal</code>, and <code>transport</code>. Platform
PTY implementations and the selectable TLS backends are separate
infrastructure modules.

Private behavior is tested next to its implementation in each module's
<code>#[cfg(test)]</code> block. Tests that execute the packaged binary live under
<code>tests/</code>; this currently verifies version output and CLI error handling.

## Verification

~~~bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --release --all-targets
cargo clippy --locked --no-default-features --features rustls-tls --all-targets -- -D warnings
cargo test --locked --no-default-features --features rustls-tls --release --all-targets
cargo build --locked --release
~~~

For the complete protocol and session model, see
[the architecture guide](../docs/architecture.md).
