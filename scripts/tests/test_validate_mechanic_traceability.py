from __future__ import annotations

import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPOSITORY_ROOT / "scripts"))

from validate_mechanic_traceability import _manifest_digest, validate_repository  # noqa: E402


class MechanicTraceabilityGateTests(unittest.TestCase):
    def fixture(self) -> tuple[tempfile.TemporaryDirectory[str], Path]:
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        files = [
            "content/mechanics/engine-traceability.json",
            "content/packs/core-mvp/definitions/characters.json",
            "content/packs/core-mvp/definitions/encounter.json",
            "content/packs/core-mvp/manifest.json",
            "content/packs/core-mvp/mechanics/traceability.json",
            "content/packs/core-mvp/notices/NOTICE.txt",
            "content/packs/core-mvp/provenance.json",
            "crates/game-core/src/encounter.rs",
            "crates/game-core/src/hero.rs",
            "crates/game-core/src/rules_matrix.rs",
            "docs/planning/03-rules-and-gameplay.md",
            "docs/planning/12-mvp-policy-resolutions.md",
        ]
        for relative in files:
            source = REPOSITORY_ROOT / relative
            destination = root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
        return temporary, root

    def test_repository_fixture_is_complete(self) -> None:
        temporary, root = self.fixture()
        self.addCleanup(temporary.cleanup)
        self.assertEqual(validate_repository(root), [])

    def test_advertised_capability_gap_fails_closed(self) -> None:
        temporary, root = self.fixture()
        self.addCleanup(temporary.cleanup)
        path = root / "content/packs/core-mvp/manifest.json"
        manifest = json.loads(path.read_text(encoding="utf-8"))
        manifest["required_engine_capabilities"].remove("combat.attack")
        manifest["digest"] = _manifest_digest(manifest)
        path.write_text(f"{json.dumps(manifest, indent=2)}\n", encoding="utf-8")

        errors = validate_repository(root)

        self.assertTrue(
            any("reachable capability set differs from manifest" in error for error in errors),
            errors,
        )

    def test_nonexistent_rust_test_reference_fails_closed(self) -> None:
        temporary, root = self.fixture()
        self.addCleanup(temporary.cleanup)
        path = root / "content/packs/core-mvp/mechanics/traceability.json"
        traceability = json.loads(path.read_text(encoding="utf-8"))
        traceability["entries"][0]["test_ids"][0] = (
            "manchester_dnd_core::encounter::tests::invented_placeholder_test"
        )
        path.write_text(f"{json.dumps(traceability, indent=2)}\n", encoding="utf-8")

        errors = validate_repository(root)

        self.assertTrue(any("symbol not found" in error for error in errors), errors)

    def test_unbound_source_key_fails_closed(self) -> None:
        temporary, root = self.fixture()
        self.addCleanup(temporary.cleanup)
        path = root / "content/mechanics/engine-traceability.json"
        registry = json.loads(path.read_text(encoding="utf-8"))
        registry["source_documents"][0]["source_keys"].remove(
            "source:srd5.1:ability-score-standard-array"
        )
        path.write_text(f"{json.dumps(registry, indent=2)}\n", encoding="utf-8")

        errors = validate_repository(root)

        self.assertTrue(
            any("has no versioned document" in error for error in errors), errors
        )


if __name__ == "__main__":
    unittest.main()
