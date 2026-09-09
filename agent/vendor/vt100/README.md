# vt100 terminal parser

Source: [vt100 0.16.2](https://github.com/doy/vt100-rust), MIT licensed.

The Shello build uses this local source with primary-screen scrollback preservation
for CSI 2 J and scrollback-only erasure for CSI 3 J. Cursor state, saved state,
attributes, and alternate-screen contents are retained according to each operation.
