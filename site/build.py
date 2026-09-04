#!/usr/bin/env python3
"""Generate the site's index page from README.md.

The README is the single source of truth. Nothing here is committed, so the
published page cannot drift from the repository's own front page.

Two things have to change on the way:

* The leading `# rllvm` heading duplicates the title the theme renders.
* Relative links work on github.com and 404 on the site, because the site has
  no `examples/` or `LICENSE` to serve.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO = "h1994st/rllvm"
BRANCH = "main"

ROOT = Path(__file__).resolve().parent.parent
README = ROOT / "README.md"
OUTPUT = Path(__file__).resolve().parent / "index.md"

# `[text](target)`, capturing the target. Bare enough to miss exotic markdown,
# which is why unresolved relative links are reported rather than assumed fixed.
LINK = re.compile(r"(?<=\]\()([^)\s]+)(?=[)\s])")

ABSOLUTE = ("http://", "https://", "#", "mailto:", "//")


def rewrite(target: str) -> str:
    """Point a repository-relative link back at GitHub."""
    if target.startswith(ABSOLUTE):
        return target
    # A trailing slash means a directory, which GitHub serves under `tree`.
    kind = "tree" if target.endswith("/") else "blob"
    return f"https://github.com/{REPO}/{kind}/{BRANCH}/{target.lstrip('./')}"


def main() -> int:
    markdown = README.read_text(encoding="utf-8")

    title = "rllvm"
    lines = markdown.splitlines()
    if lines and lines[0].startswith("# "):
        title = lines[0][2:].strip()
        lines = lines[1:]
    body = "\n".join(lines).lstrip("\n")

    body = LINK.sub(lambda match: rewrite(match.group(0)), body)

    leftover = [
        target
        for target in LINK.findall(body)
        if not target.startswith(ABSOLUTE)
    ]
    if leftover:
        print(f"unrewritten relative links: {leftover}", file=sys.stderr)
        return 1

    OUTPUT.write_text(
        f"---\nlayout: default\ntitle: {title}\n---\n\n{body}",
        encoding="utf-8",
    )
    print(f"wrote {OUTPUT.relative_to(ROOT)} from README.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
