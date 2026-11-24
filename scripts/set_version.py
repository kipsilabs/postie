#!/usr/bin/env python3
"""Stamps a release tag's version onto the Rust crates before a release build.

The release tag is the only source of the version: rust-release.yml runs this after checkout, so
`postie --version`, `postie-web --version`, the Docker image and the desktop bundles report the
tagged version without a version-bump commit. Nothing is committed back.

Usage: set_version.py <tag> [repo root]    e.g. set_version.py v0.1.1

Writes the version into every workspace crate's Cargo.toml, their entries in rust/Cargo.lock and
rust/desktop/Cargo.lock (so `--locked` builds still pass), and rust/desktop/tauri.conf.json. The
desktop bundle gets the version without its pre-release suffix (v0.1.2-rc.1 -> 0.1.2): Windows MSI
installers only accept numeric versions.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

TAG_RE = re.compile(r"^v(?P<core>\d+\.\d+\.\d+)(?P<pre>-[0-9A-Za-z.-]+)?$")
PACKAGE_VERSION_RE = re.compile(r'^(version\s*=\s*)"[^"]*"', re.MULTILINE)
PACKAGE_NAME_RE = re.compile(r'^name\s*=\s*"([^"]+)"', re.MULTILINE)
TAURI_VERSION_RE = re.compile(r'^(  "version":\s*)"[^"]*"', re.MULTILINE)
LOCK_PACKAGE_RE = re.compile(r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]|\Z)")


def parse_tag(tag: str) -> tuple[str, str]:
    """Returns (crate version, desktop bundle version) for a `vX.Y.Z[-pre]` tag."""
    m = TAG_RE.match(tag.strip())
    if not m:
        raise ValueError(f"not a release tag (want vX.Y.Z or vX.Y.Z-pre): {tag!r}")
    core = m.group("core")
    return core + (m.group("pre") or ""), core


def package_section(manifest: str) -> str:
    """The [package] table of a Cargo.toml (up to the next table header)."""
    m = re.search(r"(?ms)^\[package\]\n(.*?)(?=^\[|\Z)", manifest)
    if not m:
        raise ValueError("no [package] table")
    return m.group(1)


def set_manifest_version(manifest: str, version: str) -> tuple[str, str]:
    """Rewrites the [package] version; returns (new text, crate name)."""
    section = package_section(manifest)
    name = PACKAGE_NAME_RE.search(section)
    if not name or not PACKAGE_VERSION_RE.search(section):
        raise ValueError("[package] has no literal name/version")
    new_section = PACKAGE_VERSION_RE.sub(rf'\g<1>"{version}"', section, count=1)
    return manifest.replace(section, new_section, 1), name.group(1)


def set_lock_versions(lock: str, crates: set[str], version: str) -> str:
    """Rewrites the version of the workspace crates (local packages: no `source`) in a Cargo.lock."""

    def stamp(m: re.Match[str]) -> str:
        block = m.group(0)
        name = PACKAGE_NAME_RE.search(block)
        if not name or name.group(1) not in crates or re.search(r"^source\s*=", block, re.M):
            return block
        return PACKAGE_VERSION_RE.sub(rf'\g<1>"{version}"', block, count=1)

    return LOCK_PACKAGE_RE.sub(stamp, lock)


def set_tauri_version(conf: str, version: str) -> str:
    new, n = TAURI_VERSION_RE.subn(rf'\g<1>"{version}"', conf, count=1)
    if n != 1:
        raise ValueError('no top-level "version" in tauri.conf.json')
    return new


def stamp(root: Path, tag: str) -> list[Path]:
    """Stamps the tag's version under `root`; returns the files it changed."""
    version, bundle_version = parse_tag(tag)
    rust = root / "rust"
    crates: set[str] = set()
    changed: list[Path] = []

    def write(path: Path, text: str) -> None:
        if path.read_text(encoding="utf-8") != text:
            path.write_text(text, encoding="utf-8", newline="\n")
            changed.append(path)

    for manifest in sorted(rust.glob("*/Cargo.toml")):
        text = manifest.read_text(encoding="utf-8")
        if "[package]" not in text:
            continue
        new, name = set_manifest_version(text, version)
        crates.add(name)
        write(manifest, new)
    if not crates:
        raise ValueError(f"no crates found under {rust}")

    for lock in (rust / "Cargo.lock", rust / "desktop" / "Cargo.lock"):
        write(lock, set_lock_versions(lock.read_text(encoding="utf-8"), crates, version))

    conf = rust / "desktop" / "tauri.conf.json"
    write(conf, set_tauri_version(conf.read_text(encoding="utf-8"), bundle_version))
    return changed


def main(argv: list[str]) -> int:
    if len(argv) not in (2, 3):
        print(__doc__.strip().splitlines()[3], file=sys.stderr)
        return 2
    root = Path(argv[2]) if len(argv) == 3 else Path(__file__).resolve().parent.parent
    try:
        changed = stamp(root, argv[1])
    except (ValueError, OSError) as e:
        print(f"set_version: {e}", file=sys.stderr)
        return 1
    version, bundle_version = parse_tag(argv[1])
    print(f"version {version} (desktop bundle {bundle_version}); {len(changed)} files updated")
    for path in changed:
        print(f"  {path.relative_to(root)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
