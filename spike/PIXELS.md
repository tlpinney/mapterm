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

## Tiles (same day)

Pixel mode now draws a fine grid of tiles (each about 512 px; the grid zoom is the view's
plus log2(pixels per dot / 2), i.e. +2 on Retina) once each, caches them (192 MB, least
recently used out), sends each to the terminal once, and every frame only places them
(cell + pixel offset, cropped to the map area; `kitty::place_px`). Each grid tile is drawn
with a camera fitted to it, so neighbouring source tiles' features meet seamlessly.

Like a web map, a tile stays blank while its data loads (never drawn half-empty), and new
tiles get 8 ms of drawing a frame, center first; the rest stay blank and fill in over the
next frames (the viewer keeps drawing while `PixFrame::pending > 0`).

Numbers (Mapbox Streets, Georgetown z16, 30 tiles on screen): panning with the arrow keys,
112 frames, p50 0.6 ms a frame (was 65-95 ms), 5 tiles drawn in all (max 2 in a frame,
worst frame 11.7 ms). A cold full screen is ~18 tiles at ~9 ms each, filled in over ~0.3 s.

## Zooming: stand-ins from other zooms

A tile not drawn yet at this zoom (out of time, or its data loading) is filled with the
cached tiles of other zooms, scaled into place (bilinear) into a tile-sized image placed
pixel-exactly, as web maps do while zooming: zooming in scales up the coarser tiles,
zooming out scales down the finer ones. The nearest zoom is drawn last. Stand-ins last one
frame and are freed with the next; the exact tile replaces them once drawn. Stand-ins get
12 ms a frame after the 8 ms of exact drawing; past that, edge tiles stay blank for a frame.
(Kitty can only stretch an image to whole cells, so scaling in the terminal would leave
seams; composing stand-ins ourselves keeps them exact.)

Numbers (Mapbox Streets, two zoom-ins and three zoom-outs with + and -, 229 frames): p50
1.1 ms, p90 21 ms, max 30 ms (156 ms before the stand-in budget), no blank frames.

Still: feature-state changes (hover highlights) don't invalidate tiles yet; no cross-fade
(a stand-in is swapped for the exact tile at once).
