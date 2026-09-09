# vt100 terminal parser

[vt100 0.16.2](https://github.com/doy/vt100-rust), licensed under [MIT](LICENSE).

Shello uses vt100 to parse the child shell's output, render it above the host's fixed
status bar, and generate screen snapshots for browser refreshes, reconnections, and
recovery from network congestion.

Upstream 0.16.2 does not provide the required clear-screen history behavior or snapshot
support. The patches access internal screen buffers, scroll regions, and parser
callbacks, so the Agent uses this vendored source through a Cargo path dependency.
The upstream MIT license and copyright notice are retained in [LICENSE](LICENSE).

## Changes from upstream 0.16.2

| Files | Changes |
| --- | --- |
| [src/grid.rs](src/grid.rs), [src/screen.rs](src/screen.rs) | `CSI 2 J` preserves cleared rows through the last row containing text in scrollback when history is enabled and no partial scroll region is active, respecting the history limit. `CSI 3 J` clears scrollback and resets its offset without clearing the current screen. |
| [src/screen.rs](src/screen.rs), [src/grid.rs](src/grid.rs) | Adds `Screen::snapshot_formatted()` to reconstruct the current screen. While an alternate screen is active, it restores both primary and alternate buffers, including cursor, attributes, input modes, scroll region, and origin mode. Snapshots exclude scrollback; their reset sequence applies only to the receiving terminal. |
| [src/parser.rs](src/parser.rs), [src/perform.rs](src/perform.rs) | Adds `Parser::process_for_snapshot()` and `snapshot_ready()`. Parser callbacks track complete UTF-8 characters and control sequences so snapshots do not split them. The ordinary `process()` interface is unchanged. |

These patches apply to ordinary shells and full-screen TUIs without application-specific
branches. Text reflow on resize is not implemented; shrinking and expanding a terminal
can still lose truncated content.
