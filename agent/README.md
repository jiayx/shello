# shello-agent

The Rust host Agent runs a shell in a PTY and connects it to Shello over an outbound
WebSocket. For sharing instructions, see the [project README](../README.md).

## CLI

Run these commands from `agent/`:

~~~bash
cargo run -- -server http://localhost:5173
cargo run -- -server http://localhost:5173 -session abc-def
cargo run -- -server http://localhost:5173 -shell /bin/bash
cargo run -- --version
# shello-agent 0.3.1
~~~

| Option | Meaning |
| --- | --- |
| `-server <url>` | HTTP(S) server or direct `ws(s)` host endpoint; default `http://localhost:5173` |
| `-session <xxx-xxx>` | Existing session; only valid with an HTTP(S) server URL |
| `-shell <path>` | Shell executable |
| `--version`, `-V` | Print version and exit |

Without `-session`, an HTTP(S) connection creates a session. A direct WebSocket URL
already identifies its session. Unix uses `$SHELL`, falling back to `/bin/sh`;
Windows prefers PowerShell 7, Windows PowerShell, then `cmd.exe`.

Starting an Agent inside an already shared shell is blocked. Separate terminals can
share independently. `SHELLO_AGENT_ACTIVE` marks an active shared shell.

## Build and test

~~~bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --release --all-targets
cargo build --locked --release
~~~

The default `native-tls` feature uses the system TLS provider. To build or test the
Rustls backend, add `--no-default-features --features rustls-tls`; the two TLS features
are mutually exclusive. Static Linux release builds additionally target musl, as
configured in the [release workflow](../.github/workflows/build-agents.yml).

From the repository root, `./scripts/build-local-agent.sh` writes the current
platform's binary and `checksums.txt` to `apps/web/public/downloads/local/`.

## Diagnostics

~~~bash
SHELLO_TRACE=/tmp/shello.trace cargo run -- -server http://localhost:5173
~~~

Load the trace at `http://localhost:5173/debug/replay` to inspect rendering without a
live session. Trace files contain raw terminal output.

## Source layout

| Module | Responsibility |
| --- | --- |
| `main`, `cli`, `connection` | Startup, arguments, connection setup |
| `platform`, `platform_windows` | PTY and OS terminal settings |
| `terminal` | Local display, status bar, input and scrollback |
| `output` | Bounded output queue and recovery screen model |
| `transport`, `protocol`, `tls` | WebSocket messages and TLS |
| `vendor/vt100` | MIT-licensed terminal parser with Shello's screen/history support |

Unit tests live beside their implementation; terminal fixtures live in `tests/fixtures/`.
See [architecture](../docs/architecture.md) for runtime behavior and limits.
