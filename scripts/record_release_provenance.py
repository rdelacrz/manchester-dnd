#!/usr/bin/env python3
"""Record an unsigned in-toto/SLSA provenance statement for a local image."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any


MATERIAL_PATHS = (
    "Dockerfile",
    ".dockerignore",
    ".grype.yaml",
    "Cargo.lock",
    "deny.toml",
    "package-lock.json",
    "rust-toolchain.toml",
    "content/provenance-manifest.json",
)


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(*command: str, cwd: Path) -> str:
    return subprocess.run(
        command,
        cwd=cwd,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    ).stdout.strip()


def inspect_image(image: str, root: Path) -> dict[str, Any]:
    value = json.loads(run("docker", "image", "inspect", image, cwd=root))
    if not isinstance(value, list) or len(value) != 1 or not isinstance(value[0], dict):
        raise ValueError("docker image inspect returned an unexpected result")
    return value[0]


def build_statement(
    *,
    image_name: str,
    image_inspect: dict[str, Any],
    sbom_digest: str,
    materials: list[dict[str, Any]],
    git_commit: str,
    git_dirty: bool,
    builder_id: str,
    invocation_id: str,
) -> dict[str, Any]:
    image_id = image_inspect.get("Id")
    if not isinstance(image_id, str) or not image_id.startswith("sha256:"):
        raise ValueError("image Id is not a sha256 digest")
    config = image_inspect.get("Config")
    labels = config.get("Labels") if isinstance(config, dict) else None
    if not isinstance(labels, dict):
        raise ValueError("image is missing OCI labels")
    revision = labels.get("org.opencontainers.image.revision")
    if not isinstance(revision, str) or not revision:
        raise ValueError("image is missing OCI revision label")

    return {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [
            {
                "name": image_name,
                "digest": {"sha256": image_id.removeprefix("sha256:")},
            }
        ],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {
            "buildDefinition": {
                "buildType": "https://github.com/rdelacrz/manchester-dnd/blob/main/Dockerfile",
                "externalParameters": {
                    "distributionProfile": "private-mvp-only",
                    "vcsRevisionLabel": revision,
                },
                "internalParameters": {
                    "sourceState": "dirty-nonrelease" if git_dirty else "clean",
                    "signatureStatus": "unsigned",
                    "signatureReason": "No published registry identity while Q15/Q16 block distribution; preserve this digest-bound statement and sign at an approved release boundary.",
                },
                "resolvedDependencies": materials
                + [
                    {
                        "uri": f"git+https://github.com/rdelacrz/manchester-dnd@{git_commit}",
                        "digest": {"gitCommit": git_commit},
                    }
                ],
            },
            "runDetails": {
                "builder": {"id": builder_id},
                "metadata": {
                    "invocationId": invocation_id,
                    "finishedOn": image_inspect.get("Created"),
                },
                "byproducts": [
                    {
                        "name": "runtime CycloneDX SBOM",
                        "digest": {"sha256": sbom_digest},
                    }
                ],
            },
        },
    }


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", required=True)
    parser.add_argument("--sbom", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        inspect = inspect_image(args.image, root)
        git_commit = run("git", "rev-parse", "HEAD", cwd=root)
        git_dirty = bool(run("git", "status", "--porcelain", cwd=root))
        materials = [
            {
                "uri": f"file:{relative}",
                "digest": {"sha256": sha256_file(root / relative)},
            }
            for relative in MATERIAL_PATHS
        ]
        statement = build_statement(
            image_name=args.image,
            image_inspect=inspect,
            sbom_digest=sha256_file(args.sbom),
            materials=materials,
            git_commit=git_commit,
            git_dirty=git_dirty,
            builder_id=os.environ.get(
                "GITHUB_WORKFLOW_REF", "local:manchester-arcana-release-gate"
            ),
            invocation_id=os.environ.get("GITHUB_RUN_ID", "local-unpublished-build"),
        )
    except (OSError, ValueError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"release provenance error: {error}", file=sys.stderr)
        return 1

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(statement, indent=2) + "\n", encoding="utf-8")
    print(
        f"release provenance recorded: {args.output} "
        f"({'dirty non-release source' if git_dirty else 'clean source'})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
