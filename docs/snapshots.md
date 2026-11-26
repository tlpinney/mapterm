# Snapshots and scripting

`--snapshot COLSxROWS` prints one frame and exits; `--format` picks text (the default),
`ansi`, `html`, `describe` or `png`, and `--pick COL,ROW` prints the features at a spot as
JSON (see [Inspect](data-layers.md#inspect)).

`--format describe` describes the view instead of drawing it: its center, size and bounds;
every visible label with what it is (`transportation_name/primary`, `poi/cafe`, ...), its
cell, lon/lat and direction and distance from the center; what covers the view (water,
parks, buildings: their shares and a 25 x 10 grid); the named roads, most
important first, each with its class, which way it runs, its ends as cells and its length
in view (`- 17th Street Northwest [primary]: runs north-south, from cell (25, 0) to (25,
39), about 580 m in view`), also with `--no-labels`. OpenMapTiles and Protomaps tiles
both work; and each data layer's features nearest the center with their properties. Language
models read that better than any picture of the map, for few tokens: about 1,700 for a
busy street view (500 for a quiet one), against ~6,600 for the braille text.

`--format png` draws the view as an image, 16 x 34 pixels per cell (100 x 30 is 1600 x
1020; ~1,600-2,000 tokens to a model), with smooth fills and lines but no labels, over
any `--raster` imagery (fetched at the image's resolution; fills that would hide it are
left out, as in the viewer). Pair it
with `--format describe` when road layout or other shapes matter.

Tested with Claude Sonnet 5.5 and Haiku 5.5 on street-level views with known answers:
close-call questions about where labelled places are (30), and, with labels off, where
water and parks are (30) and which way major roads run (8):

| given | labels (S / H) | areas (S / H) | roads (S / H) |
|---|---|---|---|
| braille text | 80% / 60% | 67% / 40% | 38% / 38% |
| image of the terminal render | 83% / 87% | 87% / 70% | 62% / 12% |
| `--format describe` | 100% / 100% | 93% / 83% | 62% / 75% |
| `--format png` | | 83% / 70% | 75% / 75% |
| `--format png` + `--format describe` | | 93% / 90% | 75% / 75% |

So: the description alone by default; add the PNG when shapes matter. The same facts as
JSON, protobuf text or base64 protobuf scored no better and cost more tokens (base64
protobuf about twice the prose). The road set is small (8 questions), and its answers
come from road colors in the render, so "major" there doesn't always match a road's class.
The same questions over the Protomaps planet (`--format describe` from Protomaps tiles at the
same views) score about the same: areas 87% / 77% and roads 75% / 62%, against 90% / 80%
and 75% / 62% from OpenMapTiles in the same run. The answers come from OpenMapTiles
renders, and the two datasets differ on a few small green spaces (Georgetown, Paris).
