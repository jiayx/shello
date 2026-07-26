# Changelog

All notable user-visible changes are documented here.

## [Unreleased]

### Changed

- Linux releases now prefer a sub-1 MiB system-TLS Agent and provide a verified Rustls
  portable fallback for hosts without a compatible TLS runtime.

## [0.2.1] - 2026-07-26

### Added

- Added <code>ttys-agent --version</code> / <code>-V</code> and a version banner at
  normal Agent startup, using the version embedded in the release binary.

## [0.2.0] - 2026-07-26

### Changed

- Consolidated the host implementation into the Rust Agent; Go and Zig Agents are no
  longer supported.
- Upgraded the web terminal stack to xterm.js 6 and modernized the surrounding Vite,
  React, TypeScript, Cloudflare, and Rust dependency sets.
- Made the host PTY dimensions authoritative and added host profile propagation,
  including ConPTY-aware browser rendering on Windows.
- Improved Agent reconnect behavior, output/input backpressure, bounded buffering, and
  local remote-control approval handling.
- Narrowed GitHub Actions to Agent changes, added locked quality/native test gates, and
  release builds for six host targets.

### Added

- Published architecture and release-process documentation for the Rust-only project.
