#!/usr/bin/env python3
"""Check local inline Markdown links and heading fragments without network access.

Defaults to AGENTS.md, README.md, and docs/**/*.md. Ignores fenced examples
and external URLs. Understands ATX headings, duplicate GitHub-style anchors,
percent-encoded paths, and directory links with README.md fragments. This is
an internal documentation check, not a Markdown parser or external URL check.
"""

import re
import sys
import unicodedata
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent.parent


def prose(path):
    """Yield line numbers and text outside backtick/tilde fences."""
    fence = None
    for number, line in enumerate(path.read_text().splitlines(), 1):
        match = re.match(r"^\s{0,3}(`{3,}|~{3,})", line)
        if match:
            marker = match[1]
            if fence is None:
                fence = marker
            elif marker[0] == fence[0] and len(marker) >= len(fence):
                fence = None
        elif fence is None:
            yield number, line


def anchors(path):
    used = set()
    for _, line in prose(path):
        heading = re.match(r"^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$", line)
        if not heading:
            continue
        title = re.sub(r"\[([^\]]+)\]\([^)]*\)", r"\1", heading[1])
        title = re.sub(r"<[^>]*>", "", title).lower()
        slug = "".join(
            char for char in title
            if char in "-_ " or unicodedata.category(char)[0] not in "PS"
        ).replace(" ", "-")
        candidate = slug
        suffix = 0
        while candidate in used:
            suffix += 1
            candidate = f"{slug}-{suffix}"
        used.add(candidate)
    return used


def main():
    files = [Path(arg).resolve() for arg in sys.argv[1:]] if len(sys.argv) > 1 else [
        ROOT / "AGENTS.md", ROOT / "README.md", *sorted((ROOT / "docs").rglob("*.md"))
    ]
    errors = []
    checked = 0
    headings = {}
    for source in files:
        for number, line in prose(source):
            for match in re.finditer(r"\]\((<[^>]+>|[^\s)]+)(?:\s+\"[^\"]*\")?\)", line):
                link = match[1].strip("<>")
                url = urlsplit(link)
                if url.scheme or url.netloc:
                    continue
                checked += 1
                target = (source.parent / unquote(url.path)).resolve() if url.path else source
                reason = None
                if not target.exists():
                    reason = "missing target"
                elif url.fragment:
                    if target.is_dir():
                        target = target / "README.md"
                    if target.suffix != ".md" or not target.is_file():
                        reason = "fragment target is not a Markdown file"
                    else:
                        if target not in headings:
                            headings[target] = anchors(target)
                        if unquote(url.fragment) not in headings[target]:
                            reason = "missing heading"
                if reason:
                    name = source.relative_to(ROOT) if source.is_relative_to(ROOT) else source
                    errors.append(f"{name}:{number}: {reason}: {link}")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"{checked} internal links checked across {len(files)} Markdown files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
