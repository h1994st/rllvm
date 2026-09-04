#!/usr/bin/env python3
"""Generate the site's index page from README.md.

The README is the single source of truth. Nothing generated here is committed,
so the published page cannot drift from the repository's front page.

Four things change on the way:

* The `# rllvm` heading and the badge images are repository chrome. The page
  has a designed header instead.
* The tagline and the paragraph under it move into front matter, so the layout
  can set them in the hero rather than repeating them in the body.
* Relative links work on github.com and 404 on the site, which has no
  `examples/` or `LICENSE` to serve.
* The version is read from Cargo.toml, so the header cannot claim a release
  that was never cut.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO = "h1994st/rllvm"
BRANCH = "main"

ROOT = Path(__file__).resolve().parent.parent
README = ROOT / "README.md"
MANIFEST = ROOT / "Cargo.toml"
OUTPUT = Path(__file__).resolve().parent / "index.md"

# `[text](target)`, capturing the target. Bare enough to miss exotic markdown,
# which is why an unrewritten relative link fails the build rather than ships.
LINK = re.compile(r"(?<=\]\()([^)\s]+)(?=[)\s])")

ABSOLUTE = ("http://", "https://", "#", "mailto:", "//")


def rewrite(target: str) -> str:
    """Point a repository-relative link back at GitHub."""
    if target.startswith(ABSOLUTE):
        return target
    # A trailing slash means a directory, which GitHub serves under `tree`.
    kind = "tree" if target.endswith("/") else "blob"
    return f"https://github.com/{REPO}/{kind}/{BRANCH}/{target.lstrip('./')}"


def crate_version() -> str:
    """Read `version` from the `[package]` table."""
    section = None
    for line in MANIFEST.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            section = stripped
        elif section == "[package]" and stripped.startswith("version"):
            return stripped.split("=", 1)[1].strip().strip("\"'")
    raise SystemExit("no [package] version in Cargo.toml")


def split_front(markdown: str) -> tuple[str, str, str, str]:
    """Peel the title, tagline and lead paragraph off the top of the README."""
    lines = markdown.splitlines()

    title = "rllvm"
    if lines and lines[0].startswith("# "):
        title = lines[0][2:].strip()
        lines = lines[1:]

    # Everything above the first `##` is the introduction the hero replaces.
    body_start = next(
        (i for i, line in enumerate(lines) if line.startswith("## ")), len(lines)
    )
    intro, body = lines[:body_start], lines[body_start:]

    paragraphs = [
        " ".join(block.split())
        for block in "\n".join(
            line for line in intro if not line.startswith("[![")
        ).split("\n\n")
        if block.strip()
    ]

    tagline = paragraphs[0] if paragraphs else ""
    lead = paragraphs[1] if len(paragraphs) > 1 else ""
    return title, tagline, lead, "\n".join(body).strip()


def yaml_quote(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def main() -> int:
    title, tagline, lead, body = split_front(README.read_text(encoding="utf-8"))
    if not tagline:
        print("README has no tagline paragraph under the title", file=sys.stderr)
        return 1

    body = LINK.sub(lambda match: rewrite(match.group(0)), body)
    leftover = [t for t in LINK.findall(body) if not t.startswith(ABSOLUTE)]
    if leftover:
        print(f"unrewritten relative links: {leftover}", file=sys.stderr)
        return 1

    front = "\n".join(
        [
            "---",
            "layout: default",
            f"title: {yaml_quote(title)}",
            f"tagline: {yaml_quote(tagline)}",
            f"lead: {yaml_quote(lead)}",
            f"version: {yaml_quote(crate_version())}",
            "---",
            "",
        ]
    )

    OUTPUT.write_text(f"{front}\n{body}\n", encoding="utf-8")
    print(f"wrote {OUTPUT.relative_to(ROOT)} from README.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
