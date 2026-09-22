#!/usr/bin/env python3
"""Validate the recorded claims in the wallpaper catalog manifest.

The catalog records two claims that are expensive to re-derive and easy to let
drift: the endpoint measurement behind each wallpaper's crossfade, and the
decode verification of each packaged dynamic HEIC. This validates the recorded
claims rather than re-measuring them, so it stays cheap enough for every
change.

The manifest is parsed with the standard library's TOML reader rather than a
text scan, so comments, whitespace, multi-line strings, and table association
cannot change what is checked.

Usage: check-wallpaper-manifest.py [MANIFEST]
The WALLPAPER_MANIFEST environment variable is used when no path is given.
"""

import os
import sys
import tomllib

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
DEFAULT_MANIFEST = os.path.join(REPO_ROOT, "assets", "wallpapers", "manifest.toml")

LOOP_ANALYSIS_STRINGS = ("metric", "tool", "comparison", "procedure")
LOOP_POSITIVE_INTEGERS = (
    "crossfade_milliseconds",
    "endpoint_comparison_width",
    "endpoint_comparison_height",
)


class Problem(Exception):
    """A recorded claim that the manifest does not support."""


def require(condition, message):
    if not condition:
        raise Problem(message)


def require_text(value, message):
    require(isinstance(value, str) and value.strip(), message)


def require_positive_integer(value, description):
    require(
        isinstance(value, int) and not isinstance(value, bool) and value > 0,
        f"{description} must be a positive integer, got {value!r}",
    )


def load(manifest_path):
    try:
        with open(manifest_path, "rb") as handle:
            return tomllib.load(handle)
    except FileNotFoundError:
        raise Problem(f"missing wallpaper manifest: {manifest_path}")
    except OSError as error:
        raise Problem(f"could not read wallpaper manifest {manifest_path}: {error}")
    except (tomllib.TOMLDecodeError, UnicodeDecodeError) as error:
        raise Problem(f"invalid wallpaper manifest {manifest_path}: {error}")


def check_loop_analysis(manifest):
    loop_analysis = manifest.get("loop_analysis")
    require(
        isinstance(loop_analysis, dict),
        "the manifest must record a loop_analysis table describing the method",
    )
    for key in LOOP_ANALYSIS_STRINGS:
        require_text(
            loop_analysis.get(key),
            f"loop analysis must record '{key}' so endpoint_ssim values stay reproducible",
        )
    require(
        "unreproducible_for" not in loop_analysis,
        "loop analysis records unreproducible_for; every endpoint_ssim must be "
        "reproducible from the recorded procedure",
    )
    return loop_analysis


def check_wallpapers(manifest):
    wallpapers = manifest.get("wallpaper")
    require(
        isinstance(wallpapers, list) and wallpapers,
        "the catalog must contain at least one wallpaper",
    )

    ids = []
    for wallpaper in wallpapers:
        require(isinstance(wallpaper, dict), "every wallpaper must be a table")
        entry_id = wallpaper.get("id")
        require_text(entry_id, "every wallpaper must record an id")
        require(entry_id not in ids, f"wallpaper ids must be unique: {entry_id}")
        ids.append(entry_id)

        loop = wallpaper.get("loop")
        require(
            isinstance(loop, dict),
            f"wallpaper '{entry_id}' must record a loop table",
        )
        require_text(loop.get("mode"), f"wallpaper '{entry_id}' must record a loop mode")
        require(
            isinstance(loop.get("direct_seek_seamless"), bool),
            f"wallpaper '{entry_id}' must record direct_seek_seamless as a boolean",
        )
        for key in LOOP_POSITIVE_INTEGERS:
            require_positive_integer(loop.get(key), f"wallpaper '{entry_id}' {key}")

        endpoint_ssim = loop.get("endpoint_ssim")
        require(
            isinstance(endpoint_ssim, (int, float))
            and not isinstance(endpoint_ssim, bool)
            and 0 <= endpoint_ssim <= 1,
            f"wallpaper '{entry_id}' must record an endpoint_ssim between 0 and 1, "
            f"got {endpoint_ssim!r}",
        )
        require_text(
            loop.get("verification"),
            f"wallpaper '{entry_id}' must record how its loop transition was verified",
        )

    return ids


def check_reproducible_for(loop_analysis, ids):
    reproducible_for = loop_analysis.get("reproducible_for")
    require_text(
        reproducible_for,
        "loop analysis must record reproducible_for with every catalog entry",
    )
    recorded = [name.strip() for name in reproducible_for.split(",") if name.strip()]
    missing = sorted(set(ids) - set(recorded))
    unexpected = sorted(set(recorded) - set(ids))
    require(
        not missing and not unexpected,
        "reproducible_for must name exactly the catalog entries "
        f"(missing: {', '.join(missing) or 'none'}; "
        f"unexpected: {', '.join(unexpected) or 'none'})",
    )


def check_dynamic_heic_assets(manifest):
    assets = manifest.get("dynamic_heic")
    require(
        isinstance(assets, list) and assets,
        "the catalog must contain at least one dynamic HEIC asset",
    )
    for asset in assets:
        require(isinstance(asset, dict), "every dynamic HEIC asset must be a table")
        asset_id = asset.get("id") or "<unnamed>"
        verified = asset.get("decode_verified", True)
        require(
            isinstance(verified, bool) and verified,
            f"dynamic HEIC asset '{asset_id}' must not record decode_verified = "
            "false; the packaged heic-decode check requires every catalog asset "
            "to decode",
        )


def check_default_wallpaper(manifest, ids):
    default = manifest.get("default_wallpaper")
    require_text(default, "the catalog must record default_wallpaper")
    require(
        default in ids,
        f"default_wallpaper '{default}' is not a catalog entry",
    )


def main():
    if len(sys.argv) > 2:
        print("usage: check-wallpaper-manifest.py [MANIFEST]", file=sys.stderr)
        return 2
    manifest_path = (
        sys.argv[1]
        if len(sys.argv) > 1
        else os.environ.get("WALLPAPER_MANIFEST", DEFAULT_MANIFEST)
    )
    try:
        manifest = load(manifest_path)
        loop_analysis = check_loop_analysis(manifest)
        ids = check_wallpapers(manifest)
        check_reproducible_for(loop_analysis, ids)
        check_dynamic_heic_assets(manifest)
        check_default_wallpaper(manifest, ids)
    except Problem as problem:
        print(problem, file=sys.stderr)
        return 1

    print("Wallpaper manifest passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
