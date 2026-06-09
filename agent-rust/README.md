# ttys-agent-rust

Small synchronous Rust host agent prototype.

## Goals

- Keep the dependency graph small enough for size-focused release builds.
- Use blocking threads and RAII instead of an async runtime.
- Reuse the ttys wire protocol: binary PTY frames plus JSON control frames.

## Current Scope

- macOS and Linux are build-tested locally.
- Windows has a ConPTY platform layer and should be validated on a Windows host.
- Creates sessions through HTTP/HTTPS.
- Connects host websocket through WS/WSS.
- Spawns the user's shell in a PTY and forwards terminal I/O.
- Handles control approval and rejection prompts with output buffering while the prompt is active.

The Windows implementation uses `windows-sys` directly for ConPTY and console mode handling. Transport remains the same small synchronous WebSocket client, with TLS handled by `native-tls`/Schannel on Windows.

## Build

```bash
cargo build --release
```

Run against a local server:

```bash
cargo run -- -server http://localhost:5173
```

Attach to an existing session:

```bash
cargo run -- -server http://localhost:5173 -session <session-id>
```
