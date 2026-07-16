#!/usr/bin/env python3
"""Fail closed when a distributable asset lacks complete release provenance."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path, PurePosixPath
from typing import Any


MANIFEST_PATH = "content/provenance-manifest.json"
ASSET_DIRECTORIES = ("content", "prompts", "public")
FIXED_ASSETS = ("style/main.css", "THIRD_PARTY_NOTICES.md")
IGNORED_ASSETS = {
    MANIFEST_PATH,
    "prompts/events/private/.gitkeep",
    "public/.gitkeep",
}
PRIVATE_RUNTIME_PREFIX = "prompts/events/private/"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
ALLOWED_CLASSES = {
    "legal_notice",
    "pack_manifest",
    "pack_notice",
    "pack_provenance",
    "prompt",
    "rule_content",
    "rules_traceability",
    "style",
    "theme_tokens",
}


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def discover_assets(root: Path) -> tuple[set[str], list[str]]:
    assets: set[str] = set()
    errors: list[str] = []

    for directory in ASSET_DIRECTORIES:
        base = root / directory
        if not base.is_dir():
            errors.append(f"required asset directory is missing: {directory}")
            continue
        for path in base.rglob("*"):
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                errors.append(f"release asset must not be a symlink: {relative}")
                continue
            if not path.is_file() or relative in IGNORED_ASSETS:
                continue
            if relative.startswith(PRIVATE_RUNTIME_PREFIX):
                errors.append(
                    "private runtime prompt/source material must not be a release asset: "
                    + relative
                )
                continue
            assets.add(relative)

    for relative in FIXED_ASSETS:
        path = root / relative
        if path.is_symlink():
            errors.append(f"release asset must not be a symlink: {relative}")
        elif not path.is_file():
            errors.append(f"required release asset is missing: {relative}")
        else:
            assets.add(relative)

    return assets, errors


def _nonempty_strings(value: Any) -> bool:
    return (
        isinstance(value, list)
        and bool(value)
        and all(isinstance(item, str) and bool(item.strip()) for item in value)
    )


def _safe_relative_path(value: Any) -> bool:
    if not isinstance(value, str) or not value or "\\" in value:
        return False
    path = PurePosixPath(value)
    return not path.is_absolute() and ".." not in path.parts and path.as_posix() == value


def validate_manifest(root: Path, manifest_path: Path) -> list[str]:
    errors: list[str] = []
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return [f"cannot read provenance manifest {manifest_path}: {error}"]

    if not isinstance(manifest, dict):
        return ["provenance manifest root must be an object"]
    if manifest.get("schema") != "manchester-arcana/release-provenance/v1":
        errors.append("unsupported release provenance schema")
    if manifest.get("digest_algorithm") != "sha256":
        errors.append("digest_algorithm must be sha256")
    if manifest.get("distribution_profile") != "private-mvp-only":
        errors.append("distribution_profile must remain private-mvp-only until Q15/Q16 close")
    if not _nonempty_strings(manifest.get("distribution_blockers")):
        errors.append("distribution_blockers must be a non-empty string list")

    sources = manifest.get("sources")
    if not isinstance(sources, dict) or not sources:
        errors.append("sources must be a non-empty object")
        sources = {}
    else:
        for source_id, source in sources.items():
            if not isinstance(source_id, str) or not isinstance(source, dict):
                errors.append("each source catalog entry must be a named object")
                continue
            if not isinstance(source.get("author"), str) or not source["author"].strip():
                errors.append(f"source {source_id!r} is missing author")
            if not isinstance(source.get("version"), str) or not source["version"].strip():
                errors.append(f"source {source_id!r} is missing version")
            url = source.get("source_url")
            original = source.get("original_work")
            if not (isinstance(url, str) and url.strip()) and original is not True:
                errors.append(
                    f"source {source_id!r} needs a source_url or original_work=true"
                )
            if original is True and not isinstance(source.get("ownership_note"), str):
                errors.append(f"original source {source_id!r} needs an ownership_note")

    licenses = manifest.get("licenses")
    if not isinstance(licenses, dict) or not licenses:
        errors.append("licenses must be a non-empty object")
        licenses = {}
    else:
        for license_id, terms in licenses.items():
            if not isinstance(license_id, str) or not isinstance(terms, dict):
                errors.append("each license catalog entry must be a named object")
                continue
            if not isinstance(terms.get("terms"), str) or not terms["terms"].strip():
                errors.append(f"license {license_id!r} is missing terms")
            if not isinstance(terms.get("public_distribution_allowed"), bool):
                errors.append(
                    f"license {license_id!r} needs public_distribution_allowed boolean"
                )

    assets = manifest.get("assets")
    if not isinstance(assets, list):
        errors.append("assets must be an array")
        assets = []

    manifest_paths: set[str] = set()
    for index, asset in enumerate(assets):
        label = f"assets[{index}]"
        if not isinstance(asset, dict):
            errors.append(f"{label} must be an object")
            continue
        relative = asset.get("path")
        if not _safe_relative_path(relative):
            errors.append(f"{label}.path is not a safe normalized relative path")
            continue
        label = relative
        if relative in manifest_paths:
            errors.append(f"duplicate asset provenance: {relative}")
            continue
        manifest_paths.add(relative)

        if asset.get("class") not in ALLOWED_CLASSES:
            errors.append(f"{label}: unknown asset class {asset.get('class')!r}")
        if not _nonempty_strings(asset.get("source_ids")):
            errors.append(f"{label}: source_ids must be non-empty")
        else:
            for source_id in asset["source_ids"]:
                if source_id not in sources:
                    errors.append(f"{label}: unknown source_id {source_id!r}")
        if not _nonempty_strings(asset.get("license_ids")):
            errors.append(f"{label}: license_ids must be non-empty")
        else:
            for license_id in asset["license_ids"]:
                if license_id not in licenses:
                    errors.append(f"{label}: unknown license_id {license_id!r}")
        if not _nonempty_strings(asset.get("transformations")):
            errors.append(f"{label}: transformations must be non-empty")
        if not _nonempty_strings(asset.get("campaign_pack_references")):
            errors.append(f"{label}: campaign_pack_references must be non-empty")
        if not isinstance(asset.get("public_distribution_allowed"), bool):
            errors.append(f"{label}: public_distribution_allowed must be boolean")
        if asset.get("public_distribution_allowed") is True:
            for license_id in asset.get("license_ids", []):
                terms = licenses.get(license_id)
                if isinstance(terms, dict) and not terms.get(
                    "public_distribution_allowed", False
                ):
                    errors.append(
                        f"{label}: public distribution conflicts with {license_id}"
                    )

        recorded_digest = asset.get("sha256")
        if not isinstance(recorded_digest, str) or not SHA256_RE.fullmatch(
            recorded_digest
        ):
            errors.append(f"{label}: sha256 must be 64 lowercase hexadecimal characters")
            continue
        path = root / relative
        if not path.is_file():
            errors.append(f"{label}: recorded asset does not exist")
        elif path.is_symlink():
            errors.append(f"{label}: recorded asset must not be a symlink")
        else:
            actual_digest = sha256_file(path)
            if actual_digest != recorded_digest:
                errors.append(
                    f"{label}: digest mismatch (recorded {recorded_digest}, "
                    f"actual {actual_digest})"
                )

    discovered, discovery_errors = discover_assets(root)
    errors.extend(discovery_errors)
    for relative in sorted(discovered - manifest_paths):
        errors.append(f"release asset has no provenance entry: {relative}")
    for relative in sorted(manifest_paths - discovered):
        errors.append(f"provenance entry is not a discovered release asset: {relative}")
    return errors


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--manifest", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    root = args.root.resolve()
    manifest = args.manifest or root / MANIFEST_PATH
    errors = validate_manifest(root, manifest)
    if errors:
        for error in errors:
            print(f"release provenance error: {error}", file=sys.stderr)
        return 1

    asset_count = len(json.loads(manifest.read_text(encoding="utf-8"))["assets"])
    print(
        f"release provenance validated: {asset_count} assets; "
        "private-MVP distribution blockers Q15/Q16 remain explicit"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
