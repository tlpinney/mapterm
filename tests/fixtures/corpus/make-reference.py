# /// script
# requires-python = ">=3.10"
# dependencies = ["fiona==1.10.1"]
# ///
"""Write GDAL's reading of every corpus file beside it as <file>.gdal.json, the reference
tests/differential.rs compares mapterm's readers against.

    uv run tests/fixtures/corpus/make-reference.py

GDAL comes inside the fiona wheel (fiona 1.10.1 bundles GDAL 3.9.2), so nothing else needs
installing. Rerun it after adding files to the corpus and commit the .gdal.json files.
"""

import json
import sys
from pathlib import Path

import fiona

# GDAL reads KML (its own KML driver) but fiona leaves it off by default.
fiona.drvsupport.supported_drivers["KML"] = "r"

HERE = Path(__file__).parent
# GPX layers in the order mapterm emits them: waypoints, tracks, routes. (route_points
# and track_points repeat the same points one feature each.)
GPX_LAYERS = ["waypoints", "tracks", "routes"]


def plain(v):
    """A property value as JSON: dates and times as ISO strings, bytes as hex."""
    if isinstance(v, (bytes, bytearray)):
        return v.hex()
    if hasattr(v, "isoformat"):
        return v.isoformat()
    return v


def read(path, layers):
    out = []
    for layer in layers:
        with fiona.open(path, layer=layer) as src:
            for f in src:
                g = f.geometry
                out.append(
                    {
                        "layer": layer,
                        "geometry": None if g is None else json.loads(json.dumps(g.__geo_interface__)),
                        "properties": {k: plain(v) for k, v in dict(f.properties).items()},
                    }
                )
    return out


def main():
    files = sorted(p for p in HERE.rglob("*") if p.suffix.lower() in {".shp", ".fgb", ".gpx", ".kml"})
    for path in files:
        try:
            layers = fiona.listlayers(path)
            if path.suffix.lower() == ".gpx":
                layers = [name for name in GPX_LAYERS if name in layers]
            features = read(path, layers)
        except Exception as e:  # a file GDAL can't read is worth knowing about, not fatal
            print(f"{path.relative_to(HERE)}: GDAL failed: {e}", file=sys.stderr)
            continue
        ref = {"gdal": fiona.__gdal_version__, "layers": layers, "features": features}
        out = path.with_name(path.name + ".gdal.json")
        out.write_text(json.dumps(ref, ensure_ascii=False, sort_keys=True) + "\n")
        print(f"{path.relative_to(HERE)}: {len(features)} features")


main()
