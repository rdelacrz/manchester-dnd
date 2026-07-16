#!/usr/bin/env bash

set -Eeuo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$root_dir" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit


ROOT = Path(sys.argv[1]).resolve()
EXCLUDED_PARTS = {".git", "graphify-out", "node_modules", "target"}
INLINE_LINK = re.compile(r"!?\[[^\]]*\]\(([^)\n]+)\)")
REFERENCE_LINK = re.compile(r"^\s*\[[^\]]+\]:\s*(\S+)")
FENCE = re.compile(r"^\s*(```|~~~)")


def markdown_files() -> list[Path]:
    return sorted(
        path
        for path in ROOT.rglob("*.md")
        if not any(part in EXCLUDED_PARTS for part in path.relative_to(ROOT).parts)
    )


def destination(raw: str) -> str:
    raw = raw.strip()
    if raw.startswith("<") and ">" in raw:
        return raw[1 : raw.index(">")]
    return raw.split(maxsplit=1)[0].strip("<>")


def github_slugs(path: Path) -> set[str]:
    slugs: set[str] = set()
    occurrences: dict[str, int] = {}
    in_fence = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        match = re.match(r"^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$", line)
        if not match:
            continue
        heading = re.sub(r"<[^>]+>", "", match.group(1)).strip().lower()
        slug = re.sub(r"[^\w\- ]", "", heading, flags=re.UNICODE)
        slug = re.sub(r"\s+", "-", slug)
        count = occurrences.get(slug, 0)
        occurrences[slug] = count + 1
        slugs.add(slug if count == 0 else f"{slug}-{count}")
    return slugs


errors: list[str] = []
files = markdown_files()
for source in files:
    in_fence = False
    for line_number, line in enumerate(source.read_text(encoding="utf-8").splitlines(), 1):
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue

        # Avoid treating examples inside inline code as documentation links.
        visible = re.sub(r"`[^`]*`", "", line)
        raw_destinations = [match.group(1) for match in INLINE_LINK.finditer(visible)]
        reference = REFERENCE_LINK.match(visible)
        if reference:
            raw_destinations.append(reference.group(1))

        for raw in raw_destinations:
            dest = destination(raw)
            if not dest:
                errors.append(f"{source.relative_to(ROOT)}:{line_number}: empty link")
                continue

            parsed = urlsplit(dest)
            if parsed.scheme in {"http", "https", "mailto"} or dest.startswith("//"):
                continue
            if parsed.scheme:
                errors.append(
                    f"{source.relative_to(ROOT)}:{line_number}: unsupported link scheme: {parsed.scheme}"
                )
                continue

            path_part = unquote(parsed.path)
            if path_part:
                target = (ROOT / path_part.lstrip("/")) if path_part.startswith("/") else (source.parent / path_part)
            else:
                target = source
            target = target.resolve()

            try:
                target.relative_to(ROOT)
            except ValueError:
                errors.append(
                    f"{source.relative_to(ROOT)}:{line_number}: link escapes repository: {dest}"
                )
                continue

            if not target.exists():
                errors.append(
                    f"{source.relative_to(ROOT)}:{line_number}: missing target: {dest}"
                )
                continue

            if parsed.fragment and target.is_file() and target.suffix.lower() == ".md":
                anchor = unquote(parsed.fragment).lower()
                if anchor not in github_slugs(target):
                    errors.append(
                        f"{source.relative_to(ROOT)}:{line_number}: missing heading #{parsed.fragment} in {target.relative_to(ROOT)}"
                    )

if errors:
    print("documentation link check failed:", file=sys.stderr)
    for error in errors:
        print(f"  {error}", file=sys.stderr)
    raise SystemExit(1)

print(f"documentation link check: {len(files)} Markdown files passed")
PY
