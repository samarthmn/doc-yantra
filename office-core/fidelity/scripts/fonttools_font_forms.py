#!/usr/bin/env python3
"""Prepare full-face and retain-GID fonts for the Task 5 raster experiment."""

from __future__ import annotations

import argparse
import json
import resource
import sys
from pathlib import Path

from fontTools import __version__ as fonttools_version
from fontTools import subset
from fontTools.ttLib import TTFont


def peak_rss_bytes() -> int:
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return int(peak if sys.platform == "darwin" else peak * 1024)


def font_stats(path: Path) -> dict[str, object]:
    font = TTFont(path, lazy=False)
    glyph_instruction_bytes = 0
    if "glyf" in font:
        for glyph in font["glyf"].glyphs.values():
            program = getattr(glyph, "program", None)
            if program is not None:
                glyph_instruction_bytes += len(program.getBytecode())
    tracked = [tag for tag in ("glyf", "loca", "fpgm", "prep", "cvt ", "MATH") if tag in font]
    return {
        "bytes": path.stat().st_size,
        "glyph_slots": font["maxp"].numGlyphs,
        "tracked_tables": tracked,
        "glyph_instruction_bytes": glyph_instruction_bytes,
    }


def extract(args: argparse.Namespace) -> dict[str, object]:
    font = TTFont(args.source, fontNumber=args.face_index, lazy=False)
    font.recalcTimestamp = False
    Path(args.output).parent.mkdir(parents=True, exist_ok=True)
    font.save(args.output)
    return {
        "operation": "extract",
        "fonttools_version": fonttools_version,
        "peak_rss_bytes": peak_rss_bytes(),
        "font": font_stats(Path(args.output)),
    }


def sparse(args: argparse.Namespace) -> dict[str, object]:
    font = TTFont(args.source, lazy=False)
    options = subset.Options()
    options.retain_gids = True
    options.hinting = True
    options.notdef_glyph = True
    options.notdef_outline = True
    options.recommended_glyphs = True
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.name_legacy = True
    options.layout_features = ["*"]
    subsetter = subset.Subsetter(options=options)
    subsetter.populate(gids={0, *(int(value) for value in args.gids.split(",") if value)})
    subsetter.subset(font)
    font.recalcTimestamp = False
    Path(args.output).parent.mkdir(parents=True, exist_ok=True)
    font.save(args.output)
    return {
        "operation": "sparse",
        "fonttools_version": fonttools_version,
        "peak_rss_bytes": peak_rss_bytes(),
        "font": font_stats(Path(args.output)),
    }


def stats(args: argparse.Namespace) -> dict[str, object]:
    return {
        "operation": "stats",
        "fonttools_version": fonttools_version,
        "peak_rss_bytes": peak_rss_bytes(),
        "font": font_stats(Path(args.source)),
    }


def dehint(args: argparse.Namespace) -> dict[str, object]:
    font = TTFont(args.source, lazy=False)
    options = subset.Options()
    options.retain_gids = True
    options.hinting = False
    options.notdef_glyph = True
    options.notdef_outline = True
    options.recommended_glyphs = True
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.name_legacy = True
    options.layout_features = ["*"]
    subsetter = subset.Subsetter(options=options)
    subsetter.populate(gids=range(font["maxp"].numGlyphs))
    subsetter.subset(font)
    font.recalcTimestamp = False
    Path(args.output).parent.mkdir(parents=True, exist_ok=True)
    font.save(args.output)
    return {
        "operation": "dehint",
        "fonttools_version": fonttools_version,
        "peak_rss_bytes": peak_rss_bytes(),
        "font": font_stats(Path(args.output)),
    }


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)

    extract_parser = commands.add_parser("extract")
    extract_parser.add_argument("--source", type=Path, required=True)
    extract_parser.add_argument("--face-index", type=int, required=True)
    extract_parser.add_argument("--output", type=Path, required=True)
    extract_parser.set_defaults(run=extract)

    sparse_parser = commands.add_parser("sparse")
    sparse_parser.add_argument("--source", type=Path, required=True)
    sparse_parser.add_argument("--gids", required=True)
    sparse_parser.add_argument("--output", type=Path, required=True)
    sparse_parser.set_defaults(run=sparse)

    stats_parser = commands.add_parser("stats")
    stats_parser.add_argument("--source", type=Path, required=True)
    stats_parser.set_defaults(run=stats)

    dehint_parser = commands.add_parser("dehint")
    dehint_parser.add_argument("--source", type=Path, required=True)
    dehint_parser.add_argument("--output", type=Path, required=True)
    dehint_parser.set_defaults(run=dehint)
    return root


def main() -> None:
    args = parser().parse_args()
    print(json.dumps(args.run(args), sort_keys=True))


if __name__ == "__main__":
    main()
