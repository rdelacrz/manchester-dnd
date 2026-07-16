#!/usr/bin/env python3
"""Validate the exported minimal runtime filesystem without executing it."""

from __future__ import annotations

import argparse
import hashlib
import stat
import sys
import tarfile
from pathlib import Path, PurePosixPath


REQUIRED_UID = 65532
REQUIRED_GID = 65532
FORBIDDEN_PATHS = {
    "bin/bash",
    "bin/sh",
    "usr/bin/apt",
    "usr/bin/apt-get",
    "usr/bin/cargo",
    "usr/bin/curl",
    "usr/bin/git",
    "usr/bin/rustc",
    "usr/local/bin/cargo",
    "usr/local/bin/rustc",
}
FORBIDDEN_PREFIXES = (
    "build/",
    "root/.cargo/",
    "root/.rustup/",
    "usr/local/cargo/",
    "usr/local/rustup/",
)
FORBIDDEN_SOURCE_NAMES = {"Cargo.toml", "Cargo.lock", "package.json", "package-lock.json"}


def normalize_tar_path(name: str) -> str | None:
    stripped = name.removeprefix("./").rstrip("/")
    if not stripped:
        return None
    path = PurePosixPath(stripped)
    if path.is_absolute() or ".." in path.parts:
        return None
    return path.as_posix()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def validate_rootfs(
    archive_path: Path, expected_asset_manifest: Path
) -> list[str]:
    errors: list[str] = []
    try:
        archive = tarfile.open(archive_path, "r:*")
    except (OSError, tarfile.TarError) as error:
        return [f"cannot open runtime filesystem archive: {error}"]

    with archive:
        members: dict[str, tarfile.TarInfo] = {}
        for member in archive.getmembers():
            normalized = normalize_tar_path(member.name)
            if normalized is None:
                if member.name.removeprefix("./").rstrip("/"):
                    errors.append(f"unsafe runtime archive path: {member.name}")
                continue
            if normalized in members:
                errors.append(f"duplicate runtime archive path: {normalized}")
                continue
            members[normalized] = member

            if normalized in FORBIDDEN_PATHS or normalized.startswith(FORBIDDEN_PREFIXES):
                errors.append(f"build/package-management tool leaked into runtime: {normalized}")
            if PurePosixPath(normalized).name in FORBIDDEN_SOURCE_NAMES:
                errors.append(f"source/build manifest leaked into runtime: {normalized}")
            if normalized.endswith(".rs"):
                errors.append(f"Rust source leaked into runtime: {normalized}")
            if member.isfile() and member.mode & (stat.S_ISUID | stat.S_ISGID):
                errors.append(f"setuid/setgid runtime file is forbidden: {normalized}")
            if member.ischr() or member.isblk() or member.isfifo():
                errors.append(f"device/FIFO runtime entry is forbidden: {normalized}")

        data = members.get("app/data")
        if data is None or not data.isdir():
            errors.append("runtime is missing /app/data directory")
        elif (data.uid, data.gid, stat.S_IMODE(data.mode)) != (
            REQUIRED_UID,
            REQUIRED_GID,
            0o700,
        ):
            errors.append(
                "/app/data must be owned by 65532:65532 with mode 0700 "
                f"(got {data.uid}:{data.gid} {stat.S_IMODE(data.mode):04o})"
            )

        executable = members.get("usr/local/bin/manchester-dnd-web")
        if executable is None or not executable.isfile() or executable.size <= 0:
            errors.append("runtime is missing the non-empty game server executable")
        elif (executable.uid, executable.gid) != (REQUIRED_UID, REQUIRED_GID):
            errors.append("game server executable must be owned by 65532:65532")
        elif executable.mode & 0o022:
            errors.append("game server executable must not be group/world writable")
        elif executable.mode & 0o111 == 0:
            errors.append("game server executable is not executable")

        wasm = members.get("app/site/pkg/manchester-arcana.wasm")
        if wasm is None or not wasm.isfile() or wasm.size <= 0:
            errors.append("runtime is missing the non-empty hydrated WASM artifact")

        manifest_member = members.get("app/content/provenance-manifest.json")
        if manifest_member is None or not manifest_member.isfile():
            errors.append("runtime is missing the release asset provenance manifest")
        else:
            handle = archive.extractfile(manifest_member)
            if handle is None:
                errors.append("cannot read runtime release asset provenance manifest")
            else:
                actual = sha256_bytes(handle.read())
                expected = hashlib.sha256(expected_asset_manifest.read_bytes()).hexdigest()
                if actual != expected:
                    errors.append("runtime release asset provenance manifest digest differs")

        private_prefix = "app/prompts/events/private/"
        private_files = sorted(
            path
            for path, member in members.items()
            if path.startswith(private_prefix)
            and member.isfile()
            and path != private_prefix + ".gitkeep"
        )
        if private_files:
            errors.append(
                "runtime-private source/prompt files leaked into image: "
                + ", ".join(private_files)
            )

    return errors


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    parser.add_argument(
        "--asset-manifest",
        type=Path,
        default=root / "content/provenance-manifest.json",
    )
    args = parser.parse_args()
    errors = validate_rootfs(args.archive, args.asset_manifest)
    if errors:
        for error in errors:
            print(f"runtime filesystem error: {error}", file=sys.stderr)
        return 1
    print("runtime filesystem validated: nonroot, minimal, provenance-complete, no private sources")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
