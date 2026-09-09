<p align="center">
  <img src="apps/web/public/logo.svg" alt="Shello logo" width="96" height="96">
</p>

<h1 align="center">Shello</h1>

<p align="center">One command. Share your shell.</p>

Share a local terminal from a browser-friendly link, without opening an inbound port.
The host runs one Rust Agent; Cloudflare Workers and a Durable Object broker the
browser and host WebSocket connections.

The host CLI is `shello-agent`, the web package is `@shello/web`, and Agent
environment variables use the `SHELLO_*` prefix.

## What is in this repository

- <code>apps/web</code>: React + Vite frontend and Cloudflare Worker
- <code>agent</code>: Rust host agent for macOS, Linux, and Windows
- <code>scripts</code>: local development and bootstrap helpers

There is one supported host-agent implementation. It creates or attaches to a session,
runs a local shell in a PTY, reconnects after transient network failures, batches
terminal output, synchronizes terminal metadata, and requires a local decision before
a viewer can write to the shell.

## Quick start

Run the web application locally:

~~~bash
pnpm install
pnpm dev
~~~

In another terminal, start an Agent:

~~~bash
./scripts/start.sh http://localhost:5173
~~~

The Agent prints a viewer URL. Opening it lets a viewer observe the terminal. A viewer
must request control, and the host must approve it locally before keystrokes are
forwarded.

For a deployed instance, run the Agent from the browser bootstrap page:

~~~bash
curl -fsSL https://your-shello.example/start | sh
~~~

The bootstrap script selects the matching release binary, verifies it against
<code>checksums.txt</code>, then runs it attached to the current terminal. On Linux it
first uses the lightweight system-TLS Agent; if that binary cannot load the host's TLS
runtime, it verifies and uses the portable Rustls fallback. Windows users can use
<code>/start.ps1</code> from PowerShell. See [the release guide](docs/releasing.md) for
the published asset names and deployment configuration.

## How it works

~~~mermaid
flowchart LR
  H["Host terminal"] <--> A["Rust Agent<br/>PTY + local approval"]
  A <--> D["Durable Object<br/>session state"]
  D <--> V["Viewer browser<br/>xterm.js"]
  W["Worker<br/>API + bootstrap"] --> D
  W --> R["GitHub Release<br/>signed-by-checksum assets"]
~~~

The Agent opens an outbound WebSocket and owns the real shell and PTY. The Durable
Object keeps the session lifecycle, WebSocket membership, control lease, and a bounded
output replay buffer. The browser renders the stream and sends input only while it owns
the remote-control lease. More detail is in [the architecture guide](docs/architecture.md).

## Terminal behavior and compatibility

The host PTY is the source of truth for terminal dimensions. A viewer never resizes the
host shell, which preserves full-screen programs, line wrapping, and terminal state
across viewers. The web client scales its rendered xterm.js terminal to the available
container and reacts to container, browser-window, and mobile visual-viewport changes.

Current Chrome, Edge, Firefox, and Safari releases are supported. The client briefly
stages output until host dimensions arrive, then falls back to a safe terminal size
instead of stalling forever. Browser input is chunked and back-pressured, so a large
paste does not overwhelm a slow or reconnecting host.

The Agent publishes its PTY profile to the session. Unix hosts use standard terminal
behavior; Windows hosts announce ConPTY so xterm.js can apply its Windows wrapping and
scrollback behavior. The shell, installed fonts, and browser still determine exact CJK
and emoji glyph coverage.

## Security model

Shello is designed for deliberate, temporary sharing—not as a multi-user access-control
system. Anyone who has a live viewer link can observe the terminal. Treat that link as
a secret, close the session when the task ends, and do not expose credentials or
production consoles unless that risk is acceptable.

Remote input is disabled by default. Each control request is surfaced in the host's
real terminal, where <code>Y</code> approves it and <code>N</code>, Return, Ctrl-C, or
Escape rejects it. Approval pauses remote delivery while the prompt is active, keeping
host interaction and the decision visible. Shello does not add end-to-end encryption or
identity-based authorization on top of the deployment's HTTPS and Cloudflare access
controls.

## Requirements

- Node.js with <code>pnpm</code>
- Rust stable toolchain
- A Cloudflare account for deployment

## Web development

Install dependencies and start the local web app:

~~~bash
pnpm install
pnpm dev
~~~

Build or deploy the Worker:

~~~bash
pnpm build
pnpm deploy
~~~

The Worker needs a Durable Object binding. The included
[<code>wrangler.jsonc</code>](apps/web/wrangler.jsonc) configures the production
bootstrap to serve assets from the <code>jiayx/ttys</code> GitHub release. Set
<code>BOOTSTRAP_BINARY_BASE_URL</code> and <code>BOOTSTRAP_CHECKSUMS_URL</code> to use
another trusted asset location, or set <code>BOOTSTRAP_GITHUB_REPOSITORY</code> for
another GitHub repository.

## Host Agent

Build and test:

~~~bash
cd agent
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --release --all-targets
cargo build --locked --release
~~~

Start a new local session:

~~~bash
./scripts/start.sh http://localhost:5173
~~~

Attach to an existing session:

~~~bash
./scripts/start.sh http://localhost:5173 <session-id>
~~~

The equivalent PowerShell entrypoint is:

~~~powershell
./scripts/start.ps1 -Server http://localhost:5173
~~~

You can also invoke Cargo directly:

~~~bash
cd agent
cargo run -- -server http://localhost:5173
cargo run -- -server http://localhost:5173 -session <session-id>
~~~

Flags:

- <code>-server</code>: HTTP(S) base URL or direct <code>ws(s)</code> host websocket URL
- <code>-session</code>: existing <code>xxx-xxx</code> session ID; valid only with an
  HTTP(S) server URL
- <code>-shell</code>: shell executable to launch

With an HTTP(S) server URL, omitting <code>-session</code> creates a new session. A
direct WebSocket URL already names the host endpoint and therefore cannot be combined
with <code>-session</code>. On Unix, the default shell comes from <code>$SHELL</code>
(falling back to <code>/bin/sh</code>); on Windows the Agent prefers PowerShell 7,
Windows PowerShell, then <code>cmd.exe</code>.

For terminal-rendering diagnostics, record raw PTY output with:

~~~bash
cd agent
SHELLO_TRACE=/tmp/shello.trace cargo run -- -server http://localhost:5173
~~~

Then open <code>http://localhost:5173/debug/replay</code> and load the trace to replay
it in xterm.js without the WebSocket or Durable Object transport.

## Local bootstrap assets

Build the current machine's Agent binary into the web download directory:

~~~bash
./scripts/build-local-agent.sh
~~~

This produces:

- <code>apps/web/public/downloads/local/shello-agent-&lt;os&gt;-&lt;arch&gt;[.exe]</code>
- <code>apps/web/public/downloads/local/checksums.txt</code>

## CI and releases

[<code>.github/workflows/build-agents.yml</code>](.github/workflows/build-agents.yml)
is intentionally Agent-only: it runs when <code>agent/**</code> or the workflow
changes. Pull requests perform the Rust quality gate on Ubuntu and native lint/test
checks on macOS ARM64 and Windows AMD64. A <code>v*</code> tag repeats those checks
once, then builds the six delivery targets:

- <code>shello-agent-darwin-amd64</code>
- <code>shello-agent-darwin-arm64</code>
- <code>shello-agent-linux-amd64</code>
- <code>shello-agent-linux-arm64</code>
- <code>shello-agent-linux-amd64-portable</code>
- <code>shello-agent-linux-arm64-portable</code>
- <code>shello-agent-windows-amd64.exe</code>
- <code>shello-agent-windows-arm64.exe</code>

The standard Linux assets use the system TLS runtime to stay lightweight. The
portable Linux assets use Rustls and are downloaded only when the standard asset cannot
start. Pushing a <code>v*</code> tag additionally bundles those binaries, generates
<code>checksums.txt</code>, and publishes the GitHub Release. Follow
[the release guide](docs/releasing.md) for the versioning and verification checklist.
The workflow does not run for a direct <code>main</code> push, so a paired push of
<code>main</code> and its tag creates exactly one release run. Use pull requests for
pre-merge CI, or <code>workflow_dispatch</code> for an explicit manual full-build check.

## Documentation

- [Architecture and runtime behavior](docs/architecture.md)
- [Release process](docs/releasing.md)
- [Agent reference](agent/README.md)
- [Change log](CHANGELOG.md)
