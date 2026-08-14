# Rust / Comet rendered parity

`rust-3e429df-vs-comet-main.png` is the rendered baseline used to drive the
next shell port. The left half is the Rust/GPUI app at commit `3e429df`; the
right half is Comet's reference workbench.

The first pass identified these structural deltas:

- Replace harness/model controls in the primary sidebar with Comet's
  spaces-filter and session-row hierarchy.
- Give session rows Comet's three-line rhythm: space/status, title, and harness
  metadata.
- Move provider authentication exclusively to the Providers settings surface.
- Port the exact spaces picker/popover rather than cycling spaces on the filter
  row.
- Port the composer pill, picker chips, transcript gutters, and the 40px fade
  into the composer.

The current working tree adds the real-state sidebar adapter and first
Comet-style sidebar rows, plus the searchable/frosted spaces picker state and
first popover render. New projects now persist through a Comet-styled name/path
dialog; Comet's full folder-browser/device palette and rendered parity check
remain open. A fresh rendered comparison is pending until the macOS display is
unlocked; while locked, `loginwindow` is frontmost and screen capture returns a
black frame for both the Rust app and the known-good packaged binary.
