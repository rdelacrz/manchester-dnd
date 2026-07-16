#!/usr/bin/env python3
"""Validate the pinned browser-test dependency graph and license policy."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


EXACT_VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
ALLOWED_LICENSES = {
    "Apache-2.0",
    "MIT",
    # axe-core is test-only. MPL-2.0 file-level copyleft does not transfer to
    # the application and its source/license stays in the installed package.
    "MPL-2.0",
}
REGISTRY_PREFIX = "https://registry.npmjs.org/"


def _load_object(path: Path) -> tuple[dict[str, Any] | None, list[str]]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return None, [f"cannot read {path}: {error}"]
    if not isinstance(value, dict):
        return None, [f"{path} root must be an object"]
    return value, []


def validate(package_path: Path, lock_path: Path) -> list[str]:
    package, errors = _load_object(package_path)
    lock, lock_errors = _load_object(lock_path)
    errors.extend(lock_errors)
    if package is None or lock is None:
        return errors

    if package.get("private") is not True:
        errors.append("package.json must remain private")
    if lock.get("lockfileVersion") != 3:
        errors.append("package-lock.json must use lockfileVersion 3")
    packages = lock.get("packages")
    if not isinstance(packages, dict):
        errors.append("package-lock.json packages must be an object")
        return errors
    root = packages.get("")
    if not isinstance(root, dict):
        errors.append("package-lock.json is missing its root package")
        return errors

    direct = package.get("devDependencies")
    locked_direct = root.get("devDependencies")
    if not isinstance(direct, dict) or not direct:
        errors.append("package.json devDependencies must be non-empty")
        direct = {}
    if direct != locked_direct:
        errors.append("package.json and package-lock.json devDependencies differ")
    for name, requirement in direct.items():
        if not isinstance(name, str) or not isinstance(requirement, str):
            errors.append("direct dependency names and versions must be strings")
        elif not EXACT_VERSION_RE.fullmatch(requirement):
            errors.append(f"direct JavaScript dependency is not exactly pinned: {name}@{requirement}")

    for location, metadata in packages.items():
        if location == "":
            continue
        label = location.removeprefix("node_modules/")
        if not isinstance(metadata, dict):
            errors.append(f"lockfile package {label!r} must be an object")
            continue
        version = metadata.get("version")
        if not isinstance(version, str) or not EXACT_VERSION_RE.fullmatch(version):
            errors.append(f"lockfile package is not exactly versioned: {label}")
        license_id = metadata.get("license")
        if license_id not in ALLOWED_LICENSES:
            errors.append(f"unknown or incompatible JavaScript license: {label} ({license_id!r})")
        resolved = metadata.get("resolved")
        if not isinstance(resolved, str) or not resolved.startswith(REGISTRY_PREFIX):
            errors.append(f"JavaScript package is not from the allowed npm registry: {label}")
        integrity = metadata.get("integrity")
        if not isinstance(integrity, str) or not integrity.startswith("sha512-"):
            errors.append(f"JavaScript package lacks sha512 lockfile integrity: {label}")
        if metadata.get("hasInstallScript") is True and metadata.get("optional") is not True:
            errors.append(f"non-optional JavaScript install script is forbidden: {label}")

    return errors


def parse_args() -> argparse.Namespace:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", type=Path, default=root / "package.json")
    parser.add_argument("--lock", type=Path, default=root / "package-lock.json")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    errors = validate(args.package, args.lock)
    if errors:
        for error in errors:
            print(f"JavaScript dependency policy error: {error}", file=sys.stderr)
        return 1
    count = len(json.loads(args.lock.read_text(encoding="utf-8"))["packages"]) - 1
    print(f"JavaScript dependency policy validated: {count} pinned packages")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
