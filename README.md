<p align="center">
  <img src="apps/web/public/logo.svg" alt="ttys logo" width="96" height="96">
</p>

<h1 align="center">ttys</h1>

Anonymous shared terminal over Cloudflare Workers and Durable Objects.

## Components

- `apps/web`: React + Vite frontend and Cloudflare Worker
- `agent`: Rust host agent for macOS, Linux, and Windows
- `scripts`: local development and bootstrap helpers

The repository has one supported host-agent implementation. It creates and attaches
sessions, runs a local shell in a PTY, reconnects the host websocket, batches terminal
output, synchronizes terminal size, and asks the host before granting remote control.

## Terminal behavior and browser compatibility

The host PTY is the source of truth for terminal dimensions. This preserves full-screen
programs, line wrapping, and terminal state for every viewer: a viewer's browser never
resizes the host shell. The web client instead scales the rendered terminal to the
available container and observes container, browser-window, and mobile visual-viewport
changes so that split views and virtual keyboards do not crop the terminal.

The client uses xterm.js and targets current Chrome, Edge, Firefox, and Safari releases.
It keeps terminal output intact while waiting briefly for the host's dimensions, then
uses xterm's safe default size rather than indefinitely buffering output. Input is
chunked before it crosses the websocket and paused when the browser send buffer is full,
which prevents a large paste from disconnecting a slow or reconnecting host.

The Agent publishes its PTY profile to the session. Unix hosts use standard terminal
behavior; Windows hosts announce ConPTY so xterm.js can use its Windows wrapping and
scrollback compatibility rules. The underlying shell, fonts, and browser remain
responsible for the exact glyph coverage of CJK and emoji content.

## Requirements

- Node.js with `pnpm`
- Rust stable toolchain
- A Cloudflare account for deployment

## Web Development

Install dependencies and start the local web app:

```bash
pnpm install
pnpm dev
```

Build or deploy the Worker:

```bash
pnpm build
pnpm deploy
```

## Host Agent

Build and test:

```bash
cd agent
cargo test --release --all-targets
cargo build --release
```

Start a new local session:

```bash
./scripts/start.sh http://localhost:5173
```

Attach to an existing session:

```bash
./scripts/start.sh http://localhost:5173 <session-id>
```

The equivalent PowerShell entrypoint is:

```powershell
./scripts/start.ps1 -Server http://localhost:5173
```

You can also invoke Cargo directly:

```bash
cd agent
cargo run -- -server http://localhost:5173
cargo run -- -server http://localhost:5173 -session <session-id>
```

Flags:

- `-server`: HTTP(S) base URL or direct `ws(s)` host websocket URL
- `-session`: existing session ID when using an HTTP(S) server URL
- `-shell`: shell executable to launch

For terminal-rendering diagnostics, record raw PTY output with:

```bash
cd agent
TTYS_TRACE=/tmp/ttys.trace cargo run -- -server http://localhost:5173
```

Then open `http://localhost:5173/debug/replay` and load the trace to replay it in
xterm.js without the websocket or Durable Object transport.

## Local Bootstrap Assets

Build the current machine's agent binary into the web download directory:

```bash
./scripts/build-local-agent.sh
```

This produces:

- `apps/web/public/downloads/local/ttys-agent-<os>-<arch>[.exe]`
- `apps/web/public/downloads/local/checksums.txt`

## CI and Releases

`.github/workflows/build-agents.yml` tests native targets and produces release assets for:

- `ttys-agent-darwin-amd64`
- `ttys-agent-darwin-arm64`
- `ttys-agent-linux-amd64`
- `ttys-agent-linux-arm64`
- `ttys-agent-windows-amd64.exe`
- `ttys-agent-windows-arm64.exe`

On `v*` tags, it publishes those assets and `checksums.txt` to the GitHub release.
