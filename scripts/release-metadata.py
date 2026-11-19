import re
import subprocess
import sys
import tomllib
from pathlib import Path


CONFIG = tomllib.loads(Path("cliff.toml").read_text())
TAG_PATTERN = CONFIG["git"]["tag_pattern"]
CHANGELOG = Path("CHANGELOG.md")


def read_version():
    manifest = tomllib.loads(Path("Cargo.toml").read_text())
    version = manifest["package"]["version"]
    if not re.fullmatch(TAG_PATTERN, f"v{version}"):
        sys.exit("Genkan's Cargo version must be a stable semantic version")
    for path in ("Cargo.lock", "fuzz/Cargo.lock"):
        packages = tomllib.loads(Path(path).read_text())["package"]
        if [p["version"] for p in packages if p["name"] == "genkan"] != [version]:
            sys.exit(f"{path} does not match the Genkan version in Cargo.toml")
    return version


def prepare(version):
    tags = subprocess.check_output(
        ["git", "tag", "--merged", "HEAD", "--sort=-version:refname"], text=True
    ).splitlines()
    latest = next((tag for tag in tags if re.fullmatch(TAG_PATTERN, tag)), None)
    current = f"v{version}"
    if (latest or CHANGELOG.exists()) and latest != current:
        print(f"Waiting for {current} to be tagged before preparing the next release", file=sys.stderr)
        return

    cliff = ["git-cliff", "--use-branch-tags"]
    tag = subprocess.check_output([*cliff, "--unreleased", "--bumped-version"], text=True).strip()
    if not tag or tag == latest:
        return
    if not re.fullmatch(TAG_PATTERN, tag):
        sys.exit(f"git-cliff returned an invalid release tag: {tag!r}")

    entry = subprocess.check_output(
        [*cliff, "--unreleased", "--tag", tag, "--strip", "all"], text=True
    ).strip()
    heading, _, body = entry.partition("\n")
    if not heading.startswith(f"## [{tag[1:]}] - "):
        sys.exit(f"git-cliff did not generate a changelog entry for {tag}")
    base = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    entry = f"{heading}\n<!-- release-base: {base} -->\n{body}\n"
    if CHANGELOG.exists():
        # Keep the released sections as reviewed, including hand-edited notes.
        header, separator, history = CHANGELOG.read_text().partition("\n## [")
        CHANGELOG.write_text(f"{header.rstrip()}\n\n{entry}{separator}{history}")
    else:
        CHANGELOG.write_text(f"{CONFIG['changelog']['header'].rstrip()}\n\n{entry}")
    if tag != current:
        for name in ("Cargo.toml", "Cargo.lock", "fuzz/Cargo.lock"):
            path = Path(name)
            old = f'name = "genkan"\nversion = "{version}"'
            text = path.read_text()
            if text.count(old) != 1:
                sys.exit(f"Cannot unambiguously update the Genkan version in {name}")
            path.write_text(text.replace(old, f'name = "genkan"\nversion = "{tag[1:]}"'))
    print(tag)


def release_entry(version):
    sections = CHANGELOG.read_text().split("\n## [", 2)
    if len(sections) < 2 or not sections[1].startswith(f"{version}] - "):
        sys.exit("Top changelog entry does not match the Genkan version in Cargo.toml")
    heading, _, rest = f"## [{sections[1]}".partition("\n")
    marker, _, body = rest.partition("\n")
    match = re.fullmatch(r"<!-- release-base: ([0-9a-f]{40}) -->", marker)
    if not match:
        sys.exit("Top changelog entry is missing its release base commit")
    return match[1], f"{heading}\n{body}".rstrip()


def check_base(version):
    prepared_base, _ = release_entry(version)
    parent = subprocess.check_output(["git", "rev-parse", "HEAD^1"], text=True).strip()
    if prepared_base != parent:
        sys.exit("Stale release proposal: main advanced after these notes were prepared")


def notes(version, title):
    if title != f"chore(release): v{version}":
        sys.exit("Release PR title does not match the Genkan version in Cargo.toml")
    print(release_entry(version)[1])


if sys.argv[1:] == ["prepare"]:
    prepare(read_version())
elif sys.argv[1:] == ["check-base"]:
    check_base(read_version())
elif len(sys.argv) == 3 and sys.argv[1] == "notes":
    notes(read_version(), sys.argv[2])
else:
    sys.exit("Usage: release-metadata.py prepare | check-base | notes 'chore(release): vX.Y.Z'")
