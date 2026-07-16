from __future__ import annotations

import io
import stat
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPOSITORY_ROOT / "scripts"))

from record_release_provenance import build_statement  # noqa: E402
from validate_runtime_rootfs import validate_rootfs  # noqa: E402


class RuntimeRootfsPolicyTests(unittest.TestCase):
    def fixture(
        self, *, data_mode: int = 0o700, private_source: bool = False, shell: bool = False
    ) -> tuple[tempfile.TemporaryDirectory[str], Path, Path]:
        temporary = tempfile.TemporaryDirectory()
        root = Path(temporary.name)
        manifest = root / "provenance-manifest.json"
        manifest.write_text('{"fixture":true}\n', encoding="utf-8")
        archive = root / "rootfs.tar"

        def add_directory(handle: tarfile.TarFile, name: str, mode: int = 0o755) -> None:
            info = tarfile.TarInfo(name)
            info.type = tarfile.DIRTYPE
            info.uid = 65532
            info.gid = 65532
            info.mode = mode
            handle.addfile(info)

        def add_file(handle: tarfile.TarFile, name: str, value: bytes, mode: int = 0o644) -> None:
            info = tarfile.TarInfo(name)
            info.size = len(value)
            info.uid = 65532
            info.gid = 65532
            info.mode = mode
            handle.addfile(info, io.BytesIO(value))

        with tarfile.open(archive, "w") as handle:
            add_directory(handle, "app")
            add_directory(handle, "app/data", data_mode)
            add_directory(handle, "app/site")
            add_directory(handle, "app/site/pkg")
            add_file(handle, "app/site/pkg/manchester-arcana.wasm", b"wasm")
            add_directory(handle, "app/content")
            add_file(handle, "app/content/provenance-manifest.json", manifest.read_bytes())
            add_directory(handle, "app/prompts")
            add_directory(handle, "app/prompts/events")
            add_directory(handle, "app/prompts/events/private")
            add_file(handle, "app/prompts/events/private/.gitkeep", b"")
            add_directory(handle, "usr")
            add_directory(handle, "usr/local")
            add_directory(handle, "usr/local/bin")
            add_file(
                handle,
                "usr/local/bin/manchester-dnd-web",
                b"executable",
                mode=0o755,
            )
            if private_source:
                add_file(
                    handle,
                    "app/prompts/events/private/real-source.md",
                    b"private",
                )
            if shell:
                add_directory(handle, "bin")
                add_file(handle, "bin/sh", b"shell", mode=0o755)
        return temporary, archive, manifest

    def test_minimal_nonroot_fixture_passes(self) -> None:
        temporary, archive, manifest = self.fixture()
        self.addCleanup(temporary.cleanup)
        self.assertEqual(validate_rootfs(archive, manifest), [])

    def test_private_source_fails_closed(self) -> None:
        temporary, archive, manifest = self.fixture(private_source=True)
        self.addCleanup(temporary.cleanup)
        errors = validate_rootfs(archive, manifest)
        self.assertTrue(any("private" in error for error in errors), errors)

    def test_writable_data_permissions_fail_closed(self) -> None:
        temporary, archive, manifest = self.fixture(data_mode=0o777)
        self.addCleanup(temporary.cleanup)
        errors = validate_rootfs(archive, manifest)
        self.assertTrue(any("mode 0700" in error for error in errors), errors)

    def test_shell_fails_minimal_runtime_policy(self) -> None:
        temporary, archive, manifest = self.fixture(shell=True)
        self.addCleanup(temporary.cleanup)
        errors = validate_rootfs(archive, manifest)
        self.assertTrue(any("tool leaked" in error for error in errors), errors)


class ReleaseProvenanceStatementTests(unittest.TestCase):
    def inspect(self) -> dict:
        return {
            "Id": "sha256:" + "a" * 64,
            "Created": "2026-07-15T00:00:00Z",
            "Config": {
                "Labels": {"org.opencontainers.image.revision": "fixture-revision"}
            },
        }

    def test_statement_binds_image_sbom_source_and_materials(self) -> None:
        statement = build_statement(
            image_name="fixture:release",
            image_inspect=self.inspect(),
            sbom_digest="b" * 64,
            materials=[{"uri": "file:Dockerfile", "digest": {"sha256": "c" * 64}}],
            git_commit="d" * 40,
            git_dirty=False,
            builder_id="fixture-builder",
            invocation_id="fixture-run",
        )

        self.assertEqual(statement["_type"], "https://in-toto.io/Statement/v1")
        self.assertEqual(statement["subject"][0]["digest"]["sha256"], "a" * 64)
        predicate = statement["predicate"]
        self.assertEqual(
            predicate["runDetails"]["byproducts"][0]["digest"]["sha256"],
            "b" * 64,
        )
        self.assertEqual(
            predicate["buildDefinition"]["internalParameters"]["sourceState"],
            "clean",
        )

    def test_dirty_source_is_explicitly_nonrelease(self) -> None:
        statement = build_statement(
            image_name="fixture:release",
            image_inspect=self.inspect(),
            sbom_digest="b" * 64,
            materials=[],
            git_commit="d" * 40,
            git_dirty=True,
            builder_id="fixture-builder",
            invocation_id="fixture-run",
        )
        self.assertEqual(
            statement["predicate"]["buildDefinition"]["internalParameters"][
                "sourceState"
            ],
            "dirty-nonrelease",
        )

    def test_missing_revision_label_fails(self) -> None:
        inspect = self.inspect()
        inspect["Config"]["Labels"] = {}
        with self.assertRaises(ValueError):
            build_statement(
                image_name="fixture:release",
                image_inspect=inspect,
                sbom_digest="b" * 64,
                materials=[],
                git_commit="d" * 40,
                git_dirty=False,
                builder_id="fixture-builder",
                invocation_id="fixture-run",
            )


if __name__ == "__main__":
    unittest.main()
