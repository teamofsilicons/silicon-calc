#!/usr/bin/env python3
"""Inventory the complete native API and web release after every component has been staged."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil

MANIFEST = "release-manifest.json"
CHECKSUMS = "SHA256SUMS"
METADATA = {MANIFEST, CHECKSUMS}
REQUIRED_FILES = (
    "bin/calc", "bin/calc-api", "bin/calc-preflight", "build.txt", "runner/requirements.txt",
    "web/server.js", "web/.next/BUILD_ID", "deploy/install.py", "deploy/backup.py",
    "deploy/calc-api.service", "deploy/calc-web.service", "deploy/calc-backup.service",
    "deploy/calc-backup.timer", "deploy/silicon-calc.apparmor",
)


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def inventory(release, *, normalize=False):
    """Do not follow links while walking; every link must resolve within this release."""
    release = release.resolve(strict=True)
    files, symlinks = {}, {}

    def visit(path):
        relative = path.relative_to(release).as_posix()
        if relative in METADATA:
            return
        if path.name.startswith(".env"):
            raise ValueError(f"Environment file in release: {relative}")
        if path.is_symlink():
            target = os.readlink(path)
            if os.path.isabs(target):
                raise ValueError(f"Absolute symlink in release: {relative}")
            if not path.resolve(strict=True).is_relative_to(release):
                raise ValueError(f"Escaping symlink in release: {relative}")
            symlinks[relative] = target
        elif path.is_dir():
            if normalize:
                path.chmod(0o755)
            for child in sorted(path.iterdir()):
                visit(child)
        elif path.is_file():
            if normalize:
                path.chmod(0o755 if path.stat().st_mode & 0o111 else 0o644)
            files[relative] = digest(path)
        else:
            raise ValueError(f"Unsupported release file: {relative}")

    if normalize:
        release.chmod(0o755)
    for child in sorted(release.iterdir()):
        visit(child)
    return {"version": 1, "files": files, "symlinks": symlinks}


def finalize(release):
    release = release.resolve(strict=True)
    # Build caches are not release assets. The installer attaches its one managed runtime cache after verification.
    # Do not follow an unexpected ancestor or cache symlink while removing stale cache from prebuilt staging.
    for name in ("web", "web/.next"):
        if (release / name).is_symlink():
            raise ValueError(f"Web build directories must not be symlinks: {name}")
    cache = release / "web/.next/cache"
    if cache.is_symlink() or cache.is_file():
        cache.unlink()
    elif cache.is_dir():
        shutil.rmtree(cache)
    for name in REQUIRED_FILES:
        path = release / name
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"Missing regular release file: {name}")
    for name in ("web/.next/static", "web/public"):
        path = release / name
        if not path.is_dir() or path.is_symlink() or not any(path.iterdir()):
            raise ValueError(f"Missing release assets: {name}")
    for name in ("bin/calc", "bin/calc-api", "bin/calc-preflight"):
        path = release / name
        with path.open("rb") as stream:
            if stream.read(4) != b"\x7fELF" or not path.stat().st_mode & 0o111:
                raise ValueError(f"Expected a Linux executable: {name}")
    result = inventory(release, normalize=True)
    for name, content in (
        (MANIFEST, json.dumps(result, indent=2, sort_keys=True) + "\n"),
        (CHECKSUMS, "".join(f"{checksum}  {name}\n" for name, checksum in sorted(result["files"].items()))),
    ):
        path = release / name
        if path.is_symlink():
            raise ValueError(f"Release metadata must not be a symlink: {name}")
        path.write_text(content)
        path.chmod(0o644)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release", type=Path)
    args = parser.parse_args()
    result = finalize(args.release)
    print(f"Finalized {len(result['files'])} files and {len(result['symlinks'])} symlinks in {args.release}")


if __name__ == "__main__":
    main()
