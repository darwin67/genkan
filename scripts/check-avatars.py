#!/usr/bin/env python3
"""Validate the built-in account pictures (RFD 7).

Every SVG in the avatar directory must be listed in the manifest with its
SHA-256, and every manifest entry must have its SVG. The sources must stay
plain vector drawings: a fixed set of shape and gradient elements on a
100-unit square, with no text, scripts, styles, images, or references other
than to gradients defined in the same file.

Each entry's PNG, rendered from the SVG by `make update-avatars`, must be a
`png_size`-square PNG matching its recorded hash.

Usage: check-avatars.py [AVATAR_DIRECTORY]
The AVATAR_DIRECTORY environment variable is used when no path is given.
"""

import hashlib
import os
import re
import sys
import tomllib
import xml.etree.ElementTree as ElementTree

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
DEFAULT_DIRECTORY = os.path.join(REPO_ROOT, "assets", "avatars")

SVG = "{http://www.w3.org/2000/svg}"
ELEMENTS = {"svg", "defs", "linearGradient", "stop", "g", "rect", "circle", "ellipse", "path"}
ATTRIBUTES = {
    "xmlns", "width", "height", "viewBox", "id", "x", "y", "x1", "y1", "x2", "y2",
    "cx", "cy", "r", "rx", "ry", "d", "fill", "stroke", "stroke-width",
    "stroke-linecap", "opacity", "stop-opacity", "stop-color", "offset",
    "transform", "gradientUnits",
}
NAME = re.compile(r"[a-z][a-z0-9-]{0,31}")
SHA256 = re.compile(r"[0-9a-f]{64}")
REFERENCE = re.compile(r"url\(#([A-Za-z0-9-]+)\)")


class Problem(Exception):
    """A source or manifest entry that breaks the avatar rules."""


def require(condition, message):
    if not condition:
        raise Problem(message)


def check_svg(path):
    with open(path, "rb") as source:
        data = source.read()
    require(b"<!" not in data and b"<?" not in data, f"{path}: no doctype, entities or processing instructions")
    try:
        root = ElementTree.fromstring(data)
    except ElementTree.ParseError as error:
        raise Problem(f"{path}: not well-formed XML: {error}") from error
    require(root.tag == SVG + "svg", f"{path}: root must be an SVG element")
    require(root.get("viewBox") == "0 0 100 100", f"{path}: viewBox must be 0 0 100 100")
    gradients = set()
    references = set()
    for element in root.iter():
        require(element.tag.startswith(SVG), f"{path}: foreign element {element.tag}")
        tag = element.tag[len(SVG):]
        require(tag in ELEMENTS, f"{path}: element <{tag}> is not allowed")
        require(not (element.text or "").strip(), f"{path}: <{tag}> must not contain text")
        for attribute, value in element.attrib.items():
            require(attribute in ATTRIBUTES, f"{path}: attribute {attribute} on <{tag}> is not allowed")
            if "url(" in value:
                match = REFERENCE.fullmatch(value)
                require(match is not None, f"{path}: {attribute} may only reference a local gradient")
                references.add(match.group(1))
        if tag == "linearGradient":
            gradients.add(element.get("id"))
    missing = references - gradients
    require(not missing, f"{path}: undefined gradients {sorted(missing)}")
    return hashlib.sha256(data).hexdigest()


def check(directory):
    manifest_path = os.path.join(directory, "manifest.toml")
    try:
        with open(manifest_path, "rb") as source:
            manifest = tomllib.load(source)
    except FileNotFoundError as error:
        raise Problem(f"missing avatar manifest: {manifest_path}") from error
    except tomllib.TOMLDecodeError as error:
        raise Problem(f"{manifest_path}: {error}") from error
    require(manifest.get("schema_version") == 1, "schema_version must be 1")
    require(manifest.get("png_size") == 512, "png_size must be 512")
    entries = manifest.get("avatar")
    require(isinstance(entries, list) and entries, "the manifest must list avatars")
    names = []
    for entry in entries:
        name = entry.get("name")
        require(isinstance(name, str) and NAME.fullmatch(name), f"invalid avatar name {name!r}")
        require(name not in names, f"duplicate avatar {name}")
        names.append(name)
        require(isinstance(entry.get("title"), str) and entry["title"], f"{name}: missing title")
        for key in ("svg_sha256", "png_sha256"):
            require(SHA256.fullmatch(str(entry.get(key, ""))), f"{name}: {key} must be a SHA-256")
        path = os.path.join(directory, f"{name}.svg")
        require(os.path.isfile(path) and not os.path.islink(path), f"{name}: missing {name}.svg")
        require(check_svg(path) == entry["svg_sha256"], f"{name}: svg_sha256 does not match {name}.svg")
        png = os.path.join(directory, f"{name}.png")
        require(os.path.isfile(png) and not os.path.islink(png), f"{name}: missing {name}.png")
        with open(png, "rb") as source:
            data = source.read()
        require(
            data[:16] == b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"
            and int.from_bytes(data[16:20], "big") == manifest["png_size"]
            and int.from_bytes(data[20:24], "big") == manifest["png_size"],
            f"{name}.png must be a {manifest['png_size']}-pixel square PNG",
        )
        require(hashlib.sha256(data).hexdigest() == entry["png_sha256"], f"{name}: png_sha256 does not match {name}.png")
    allowed = {"manifest.toml"} | {f"{name}.svg" for name in names} | {f"{name}.png" for name in names}
    extra = sorted(file for file in os.listdir(directory) if file not in allowed)
    require(not extra, f"unlisted files in {directory}: {extra}")


def main():
    directory = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("AVATAR_DIRECTORY", DEFAULT_DIRECTORY)
    try:
        check(directory)
    except Problem as problem:
        print(f"avatar check failed: {problem}", file=sys.stderr)
        return 1
    print("Avatar sources passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
