# Spike: vector maps as pixels (2026-11-01)

`MAPTERM_PIXELS=1` (kitty-graphics terminals): fills and lines are drawn with tiny-skia into
an image at the terminal's real pixel size (anti-aliased, the style's own line widths and
dashes) and shown under the text; labels and markers stay terminal text. Braille is
unchanged otherwise.

- `src/pixels.rs`: the pixel canvas. `DrawContext::fill_dots/line_dots/line_px` draw there in
  pixel mode. `Map::pixel_mode` (cell size in px) and `Map::take_pixels()` (the frame).
- `kitty::show_rgba`: a frame through POSIX shared memory (local) or zlib inline (SSH).
- `MAPTERM_PIXELS_PNG=FILE` with `--snapshot` saves the pixel frame, for checking headless.

Numbers (MacBook Air M3, Georgetown z16, 2400x1462): draw 65-95 ms a frame on the CPU,
send 1.2 ms via shared memory. So ~10-15 fps while moving: the next step is drawing each
vector tile once into its own image (like raster tiles) and moving placements, which would
pan at full speed.

Known: liberty's Whitehurst Freeway comes out as a black band (a line color mapterm can't
read falls back to black; braille's 3-dot cap hid it). fill-extrusion (3D buildings) isn't
drawn. tiny-skia's `arrayref` dependency is BSD-2-Clause, added to deny.toml's allowlist.
