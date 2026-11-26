# Experimental

Features that are off by default and may change or go away.

## Pixel mode

On terminals that show kitty graphics (Ghostty, kitty), `MAPTERM_PIXELS=1`
draws fills and lines as a real image at the screen's resolution, with smooth edges and
the style's own line widths and dashes, instead of braille dots. Labels and markers stay
text. It is rough and may change. Elsewhere the variable is ignored.

## Smooth scrolling

Terminals move content a whole character at a time. As an experiment, mapterm
can move the map by single pixels on terminals that support an experimental
smooth-scroll protocol. Today that is only
[Ghostty Smooth](https://github.com/tlpinney/ghostty/tree/smooth), a personal
fork of Ghostty (see its
[`SMOOTH.md`](https://github.com/tlpinney/ghostty/blob/smooth/SMOOTH.md)), and
the [herdr fork](https://github.com/tlpinney/herdr/tree/tlpinney) relays it
from herdr panes. Both are experimental and may change.

It is off unless you turn it on with an environment variable:

- `MAPTERM_SMOOTH=shift`: dragging follows the pointer pixel by pixel;
  scrolling still zooms.
- `MAPTERM_SMOOTH=1`: two-finger up/down pans the map smoothly instead of
  zooming.

With either set, `s` toggles it while mapterm runs. Terminals without the
protocol ignore it and behave as usual.

The animation at the top of the [README](../README.md) and the full video were recorded this way:
real mapterm in Ghostty Smooth with `MAPTERM_SMOOTH=shift`. In other terminals the map
moves the same way, but a whole character at a time.
