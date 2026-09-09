# Release notes

## [Unreleased]

### Added

- The web interface selects Chinese or English from the browser's primary language.
  Controls, status messages, session details, replay tools, and time labels are localized.
  Host terminal output is displayed unchanged.

## [0.3.0] - 2026-09-09

### Changed

- Shello shares a local shell through a browser link. The host CLI is `shello-agent`,
  release assets use `shello-agent-*`, and environment variables use `SHELLO_*`.
- The workspace has a full-width terminal, compact toolbar, startup guidance, and
  a right-side session-details drawer.
- Creating a session from an existing session opens a separate tab and preserves
  the original connection.
- Narrow terminal viewports support scrolling beyond the minimum readable font size.
- Connection status takes precedence over temporary notices. Temporary notices expire.
- Clipboard fallback supports the session-details dialog and restores keyboard focus.
- Linux downloads use system TLS with a verified portable Rustls fallback.
- The release script supports version preparation and publishing, checks both TLS
  configurations, and publishes the version commit and tag. GitHub release notes
  come from the matching version section in this file.
