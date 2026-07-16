from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPOSITORY_ROOT / "scripts"))

from validate_javascript_dependencies import validate  # noqa: E402


class JavaScriptDependencyPolicyTests(unittest.TestCase):
    def fixture(self) -> tuple[tempfile.TemporaryDirectory[str], Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        package_path = root / "package.json"
        lock_path = root / "package-lock.json"
        package = {
            "name": "fixture",
            "version": "1.0.0",
            "private": True,
            "devDependencies": {"fixture-package": "2.3.4"},
        }
        lock = {
            "name": "fixture",
            "version": "1.0.0",
            "lockfileVersion": 3,
            "packages": {
                "": {
                    "name": "fixture",
                    "version": "1.0.0",
                    "devDependencies": {"fixture-package": "2.3.4"},
                },
                "node_modules/fixture-package": {
                    "version": "2.3.4",
                    "license": "MIT",
                    "resolved": "https://registry.npmjs.org/fixture-package/-/fixture-package-2.3.4.tgz",
                    "integrity": "sha512-fixture",
                },
            },
        }
        package_path.write_text(json.dumps(package), encoding="utf-8")
        lock_path.write_text(json.dumps(lock), encoding="utf-8")
        return temporary, package_path, lock_path

    def mutate_lock(self, path: Path, mutation) -> None:
        lock = json.loads(path.read_text(encoding="utf-8"))
        mutation(lock)
        path.write_text(json.dumps(lock), encoding="utf-8")

    def test_complete_fixture_passes(self) -> None:
        temporary, package, lock = self.fixture()
        self.addCleanup(temporary.cleanup)
        self.assertEqual(validate(package, lock), [])

    def test_unpinned_direct_dependency_fails(self) -> None:
        temporary, package_path, lock_path = self.fixture()
        self.addCleanup(temporary.cleanup)
        package = json.loads(package_path.read_text(encoding="utf-8"))
        package["devDependencies"]["fixture-package"] = "^2.3.4"
        package_path.write_text(json.dumps(package), encoding="utf-8")
        self.mutate_lock(
            lock_path,
            lambda lock: lock["packages"][""]["devDependencies"].update(
                {"fixture-package": "^2.3.4"}
            ),
        )
        errors = validate(package_path, lock_path)
        self.assertTrue(any("not exactly pinned" in error for error in errors), errors)

    def test_unknown_license_fails(self) -> None:
        temporary, package, lock = self.fixture()
        self.addCleanup(temporary.cleanup)
        self.mutate_lock(
            lock,
            lambda value: value["packages"]["node_modules/fixture-package"].update(
                {"license": "LicenseRef-Unknown"}
            ),
        )
        errors = validate(package, lock)
        self.assertTrue(any("unknown or incompatible" in error for error in errors), errors)

    def test_non_registry_source_fails(self) -> None:
        temporary, package, lock = self.fixture()
        self.addCleanup(temporary.cleanup)
        self.mutate_lock(
            lock,
            lambda value: value["packages"]["node_modules/fixture-package"].update(
                {"resolved": "https://example.invalid/package.tgz"}
            ),
        )
        errors = validate(package, lock)
        self.assertTrue(any("allowed npm registry" in error for error in errors), errors)

    def test_non_optional_install_script_fails(self) -> None:
        temporary, package, lock = self.fixture()
        self.addCleanup(temporary.cleanup)
        self.mutate_lock(
            lock,
            lambda value: value["packages"]["node_modules/fixture-package"].update(
                {"hasInstallScript": True}
            ),
        )
        errors = validate(package, lock)
        self.assertTrue(any("install script is forbidden" in error for error in errors), errors)


if __name__ == "__main__":
    unittest.main()
