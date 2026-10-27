#!/usr/bin/env python3
"""Drain Calc, back up its database and blobs together, then upload the private archive."""
import argparse
import datetime as dt
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import uuid

SERVICES = ("calc-web.service", "calc-api.service")
BACKUPS = Path("/var/backups/silicon-calc")
CONFIG = Path("/etc/silicon-calc")
BLOBS = Path("/var/lib/silicon-calc/blobs")
CURRENT = Path("/opt/silicon-calc/current")
LOCK = Path("/run/lock/silicon-calc-deploy.lock")
DATABASE = "silicon_calc"


def run(*args, capture=False):
    return subprocess.run(args, check=True, text=True, stdout=subprocess.PIPE if capture else None,
                          stderr=subprocess.PIPE)


def active_services():
    return [name for name in SERVICES if subprocess.run(
        ["systemctl", "is-active", "--quiet", name], check=False).returncode == 0]


def restart(services):
    for name in reversed(SERVICES):
        if name in services:
            run("systemctl", "start", name)


def acquire_lock():
    LOCK.parent.mkdir(parents=True, exist_ok=True)
    stream = LOCK.open("a")
    os.chmod(LOCK, 0o600)
    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
    return stream


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def create_backup(bucket, region, *, restart_services=True, keep_local=2):
    """The caller holds LOCK. Upload failures retain the local backup and restore service availability."""
    if not re.fullmatch(r"[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]", bucket):
        raise ValueError("A private backup bucket is required")
    BACKUPS.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(BACKUPS, 0o700)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    name = f"backup-{stamp}-{uuid.uuid4().hex[:8]}"
    partial = BACKUPS / (name + ".partial")
    partial.mkdir(mode=0o700)
    completed = BACKUPS / name
    active = active_services()
    try:
        # Stop both even if one is starting, so no new writes can race the dump/blob snapshot.
        for service in SERVICES:
            run("systemctl", "stop", service)
        with (partial / "postgres.dump").open("wb") as stream:
            subprocess.run(["runuser", "-u", "postgres", "--", "pg_dump", "--format=custom",
                            "--no-password", "--dbname", DATABASE], stdout=stream,
                           stderr=subprocess.PIPE, check=True)
        manifest = {"kind": "silicon-calc-backup", "version": 1, "created_at": stamp,
                    "database": DATABASE, "release": str(CURRENT.resolve()),
                    "postgres_sha256": sha256(partial / "postgres.dump")}
        (partial / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        with tarfile.open(partial / "backup.tar.gz", "w:gz") as archive:
            archive.add(partial / "postgres.dump", arcname="postgres.dump")
            archive.add(partial / "manifest.json", arcname="manifest.json")
            if not BLOBS.is_dir():
                raise RuntimeError("Calc blob directory is missing; refusing an incomplete backup")
            archive.add(BLOBS, arcname="blobs")
            for filename in ("api.env", "web.env", "backup.env"):
                path = CONFIG / filename
                if path.is_file():
                    archive.add(path, arcname="config/" + filename)
        (partial / "postgres.dump").unlink()
        (partial / "SHA256SUMS").write_text(sha256(partial / "backup.tar.gz") + "  backup.tar.gz\n")
        partial.rename(completed)
    finally:
        if restart_services:
            restart(active)
    destination = f"s3://{bucket}/backups/{name}/"
    for filename in ("backup.tar.gz", "manifest.json", "SHA256SUMS"):
        run("aws", "s3", "cp", str(completed / filename), destination + filename,
            "--region", region, "--sse", "AES256", "--only-show-errors")
    # Remove only this tool's complete, uploaded backups after a new upload has succeeded.
    (completed / "uploaded").write_text(destination + "\n")
    retained = sorted((p for p in BACKUPS.glob("backup-*") if p.is_dir()
                       and re.fullmatch(r"backup-\d{8}T\d{6}Z-[0-9a-f]{8}", p.name)
                       and (p / "uploaded").is_file()), reverse=True)
    for old in retained[max(keep_local, 1):]:
        if json.loads((old / "manifest.json").read_text()).get("kind") == "silicon-calc-backup":
            shutil.rmtree(old)
    return completed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bucket", default=os.environ.get("CALC_BACKUP_BUCKET"))
    parser.add_argument("--region", default=os.environ.get("AWS_REGION", "us-east-2"))
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error("Run as root; the archive contains private configuration and sheet data")
    if not args.bucket:
        parser.error("Set CALC_BACKUP_BUCKET or --bucket")
    os.umask(0o077)
    with acquire_lock():
        result = create_backup(args.bucket, args.region)
    print(f"Calc backup saved and uploaded: {result}")


if __name__ == "__main__":
    main()
