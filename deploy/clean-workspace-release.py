#!/usr/bin/env python3
"""Invalidate local release crates while retaining the third-party build cache."""
import json
from pathlib import Path
import subprocess
import sys


def clean(manifest: Path) -> None:
    manifest = manifest.resolve(strict=True)
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--no-deps", "--format-version", "1",
        "--manifest-path", str(manifest),
    ], text=True))
    members = set(metadata["workspace_members"])
    packages = sorted(package["id"] for package in metadata["packages"] if package["id"] in members)
    if not packages or len(packages) != len(members):
        raise RuntimeError("Cargo metadata did not resolve every workspace member")
    command = ["cargo", "clean", "--release", "--manifest-path", str(manifest)]
    for package in packages:
        command.extend(["--package", package])
    subprocess.run(command, check=True)


if __name__ == "__main__":
    clean(Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent / "Cargo.toml")
