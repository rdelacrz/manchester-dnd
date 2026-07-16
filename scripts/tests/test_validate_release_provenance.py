from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPOSITORY_ROOT / "scripts"))

from validate_release_provenance import validate_manifest  # noqa: E402


class ReleaseProvenanceGateTests(unittest.TestCase):
    def fixture(self) -> tuple[tempfile.TemporaryDirectory[str], Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        asset_paths = [
            "content/rules.json",
            "prompts/system.txt",
            "public/mark.svg",
            "style/main.css",
            "THIRD_PARTY_NOTICES.md",
        ]
        for relative in asset_paths:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"fixture:{relative}\n", encoding="utf-8")

        manifest = {
            "schema": "manchester-arcana/release-provenance/v1",
            "digest_algorithm": "sha256",
            "distribution_profile": "private-mvp-only",
            "distribution_blockers": ["Q15", "Q16"],
            "sources": {
                "fixture-original": {
                    "author": "Fixture author",
                    "version": "1",
                    "source_url": None,
                    "original_work": True,
                    "ownership_note": "Synthetic unit-test fixture.",
                }
            },
            "licenses": {
                "LicenseRef-Fixture": {
                    "terms": "Unit-test fixture only.",
                    "public_distribution_allowed": False,
                }
            },
            "assets": [
                {
                    "path": relative,
                    "class": (
                        "style"
                        if relative == "style/main.css"
                        else "legal_notice"
                        if relative == "THIRD_PARTY_NOTICES.md"
                        else "prompt"
                        if relative.startswith("prompts/")
                        else "rule_content"
                    ),
                    "source_ids": ["fixture-original"],
                    "license_ids": ["LicenseRef-Fixture"],
                    "sha256": hashlib.sha256(
                        (root / relative).read_bytes()
                    ).hexdigest(),
                    "transformations": ["Synthetic fixture."],
                    "campaign_pack_references": ["fixture:test"],
                    "public_distribution_allowed": False,
                }
                for relative in asset_paths
            ],
        }
        manifest_path = root / "content/provenance-manifest.json"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        return temporary, root, manifest_path

    def test_complete_fixture_passes(self) -> None:
        temporary, root, manifest = self.fixture()
        self.addCleanup(temporary.cleanup)
        self.assertEqual(validate_manifest(root, manifest), [])

    def test_missing_asset_entry_fails_closed(self) -> None:
        temporary, root, manifest_path = self.fixture()
        self.addCleanup(temporary.cleanup)
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["assets"] = manifest["assets"][1:]
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

        errors = validate_manifest(root, manifest_path)

        self.assertTrue(any("has no provenance entry" in error for error in errors), errors)

    def test_digest_mismatch_fails_closed(self) -> None:
        temporary, root, manifest_path = self.fixture()
        self.addCleanup(temporary.cleanup)
        (root / "content/rules.json").write_text("changed\n", encoding="utf-8")

        errors = validate_manifest(root, manifest_path)

        self.assertTrue(any("digest mismatch" in error for error in errors), errors)

    def test_unknown_license_fails_closed(self) -> None:
        temporary, root, manifest_path = self.fixture()
        self.addCleanup(temporary.cleanup)
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        manifest["assets"][0]["license_ids"] = ["LicenseRef-Unknown"]
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

        errors = validate_manifest(root, manifest_path)

        self.assertTrue(any("unknown license_id" in error for error in errors), errors)

    def test_runtime_private_prompt_fails_closed(self) -> None:
        temporary, root, manifest_path = self.fixture()
        self.addCleanup(temporary.cleanup)
        private = root / "prompts/events/private/real-source.md"
        private.parent.mkdir(parents=True)
        private.write_text("private material\n", encoding="utf-8")

        errors = validate_manifest(root, manifest_path)

        self.assertTrue(any("must not be a release asset" in error for error in errors), errors)


if __name__ == "__main__":
    unittest.main()
