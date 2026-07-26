# Changelog

All notable user-visible changes are documented here.

## [Unreleased]

### Added

- Added a two-stage release script that prepares an editable CHANGELOG draft,
  validates both TLS configurations, and publishes the release commit and tag.

### Changed

- Release notes are now taken from the matching reviewed CHANGELOG section instead
  of being generated from commit titles.

## [0.2.2] - 2026-07-26

### Added

- Added Rustls-based portable Linux assets for AMD64 and ARM64. The bootstrap
  verifies the standard binary first and automatically falls back to the matching
  portable asset when the host cannot load it.
- Added packaged-binary CLI integration tests alongside responsibility-local unit
  tests for both TLS configurations.

### Changed

- Linux releases now prefer system TLS to keep the standard AMD64 and ARM64
  binaries lightweight.
- Reorganized the Agent into focused CLI, connection, protocol, terminal, and
  transport modules while keeping startup orchestration in <code>main.rs</code>.

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
