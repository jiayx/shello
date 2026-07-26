# ttys-agent

The supported Rust host agent for ttys.

It creates or attaches to a terminal-sharing session, launches the local shell in a
PTY, forwards terminal I/O over WebSocket, reconnects after transient failures, and
requires local approval before a viewer can control the shell. PTY output is batched
for remote delivery and paused for both sides while the approval prompt is active.
The Agent also publishes its terminal geometry and PTY profile; Windows builds identify
ConPTY so viewers can enable the matching xterm.js compatibility behavior.

## Verification

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --release --all-targets
cargo build --release
```

## Usage

```bash
cargo run -- -server http://localhost:5173
cargo run -- -server http://localhost:5173 -session abc-def
```

Use `-shell <path>` to choose a shell. On Unix the default comes from `$SHELL` (falling
back to `/bin/sh`); on Windows the agent prefers PowerShell 7, Windows PowerShell, then
`cmd.exe`.
