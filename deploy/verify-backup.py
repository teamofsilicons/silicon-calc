#!/usr/bin/env python3
"""Verify an explicit Calc backup by restoring it into an isolated temporary database."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import urlsplit
import uuid

HASH = re.compile(r"[0-9a-f]{64}")
DB_NAME = re.compile(r"calc_restore_check_[0-9a-f]{32}")
MAX_BYTES = 64 * 1024**3


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def command(args, **kwargs):
    # Never echo subprocess output: backups can contain private data and configuration.
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{Path(args[0]).name} failed (exit {result.returncode}); no private output printed")
    return result.stdout


def expectations(path):
    data = json.loads(path.read_text())
    if not isinstance(data, dict) or not isinstance(data.get("sheets"), list) or not data["sheets"]:
        raise ValueError("Expectations must include at least one seeded sheet")
    seen = set()
    for sheet in data["sheets"]:
        if not isinstance(sheet, dict) or set(sheet) != {"id", "version", "ops"}:
            raise ValueError("Each expected sheet needs id, version and ops")
        if not isinstance(sheet["id"], str) or not sheet["id"] or sheet["id"] in seen:
            raise ValueError("Expected sheet IDs must be nonempty and unique")
        seen.add(sheet["id"])
        if any(type(sheet[k]) is not int or sheet[k] < 0 for k in ("version", "ops")):
            raise ValueError("Expected versions and operation counts must be nonnegative integers")
    totals = data.get("totals", {})
    if not isinstance(totals, dict) or set(totals) - {"sheets", "lines", "ops", "snapshots"}:
        raise ValueError("Unsupported expected totals")
    if any(type(v) is not int or v < 0 for v in totals.values()):
        raise ValueError("Expected totals must be nonnegative integers")
    return data


def download_backup(prefix, region, destination):
    url = urlsplit(prefix)
    if url.scheme != "s3" or not re.fullmatch(r"[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]", url.netloc) \
            or not url.path.strip("/") or url.query or url.fragment:
        raise ValueError("Supply an explicit s3://bucket/backup-prefix without credentials or query parameters")
    destination.mkdir(mode=0o700)
    for name in ("backup.tar.gz", "manifest.json", "SHA256SUMS"):
        command(["aws", "s3", "cp", prefix.rstrip("/") + "/" + name,
                 str(destination / name), "--region", region, "--only-show-errors"])
        (destination / name).chmod(0o600)
    return destination


def unpack_backup(source, destination, max_bytes):
    archive_path = source / "backup.tar.gz"
    checksum = (source / "SHA256SUMS").read_text().strip()
    if not re.fullmatch(r"[0-9a-f]{64}  backup\.tar\.gz", checksum):
        raise ValueError("Invalid backup SHA256SUMS")
    digest = sha256(archive_path)
    if digest != checksum.split()[0]:
        raise ValueError("Backup archive checksum does not match")
    outer_manifest = json.loads((source / "manifest.json").read_text())
    destination.mkdir(mode=0o700)
    seen = set()
    total = 0
    with tarfile.open(archive_path, "r:gz") as archive:
        for member in archive:
            name = PurePosixPath(member.name)
            if name.is_absolute() or ".." in name.parts or str(name) != member.name.rstrip("/") \
                    or member.name in seen or not (member.isfile() or member.isdir()):
                raise ValueError("Backup has an unsafe, duplicate or unsupported archive member")
            seen.add(member.name)
            allowed_file = str(name) in {"postgres.dump", "manifest.json"}
            if name.parts and name.parts[0] == "blobs":
                if member.isfile():
                    allowed_file = len(name.parts) == 3 and bool(HASH.fullmatch(name.parts[2])) \
                        and name.parts[1] == name.parts[2][:2]
                elif len(name.parts) > 2 or (len(name.parts) == 2 and not re.fullmatch(r"[0-9a-f]{2}", name.parts[1])):
                    raise ValueError("Unexpected blob directory")
            elif name.parts and name.parts[0] == "config":
                if (member.isdir() and str(name) == "config") or (member.isfile() and str(name) in {
                    "config/api.env", "config/web.env", "config/backup.env"
                }):
                    # Do not extract, display, source or use production environment files.
                    continue
            elif member.isdir():
                raise ValueError("Unexpected backup directory")
            if member.isfile() and not allowed_file:
                raise ValueError("Unexpected backup file")
            total += member.size
            if total > max_bytes or member.size > shutil.disk_usage(destination).free - 64 * 1024**2:
                raise ValueError("Backup exceeds extraction limit or available scratch disk")
            target = destination.joinpath(*name.parts)
            if member.isdir():
                target.mkdir(mode=0o700, parents=True, exist_ok=True)
            else:
                target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                with archive.extractfile(member) as src, target.open("xb") as dst:
                    os.chmod(target, 0o600)
                    shutil.copyfileobj(src, dst)
    manifest_path, dump = destination / "manifest.json", destination / "postgres.dump"
    if not manifest_path.is_file() or not dump.is_file() or not (destination / "blobs").is_dir():
        raise ValueError("Backup is missing its manifest, database or blob directory")
    manifest = json.loads(manifest_path.read_text())
    if manifest != outer_manifest or manifest.get("kind") != "silicon-calc-backup" or manifest.get("version") != 1:
        raise ValueError("Backup manifests do not match the supported format")
    if sha256(dump) != manifest.get("postgres_sha256"):
        raise ValueError("PostgreSQL dump checksum does not match")
    with dump.open("rb") as stream:
        if stream.read(5) != b"PGDMP":
            raise ValueError("Expected a PostgreSQL custom-format dump")
    return digest


class Postgres:
    def __init__(self, *, prefix=("runuser", "-u", "postgres", "--"), bin_dir=None,
                 host="/var/run/postgresql", port=5432):
        self.prefix, self.bin_dir, self.host, self.port = prefix, bin_dir, host, port

    def call(self, tool, *args, input=None, stdin=None):
        executable = str(Path(self.bin_dir) / tool) if self.bin_dir else tool
        env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "C.UTF-8", "PGCONNECT_TIMEOUT": "10"}
        return command([*self.prefix, executable, *args], input=input, stdin=stdin, env=env, timeout=1800)

    def connection(self):
        return ["--host", self.host, "--port", str(self.port), "--username", "postgres", "--no-password"]

    def validate(self, dump):
        with dump.open("rb") as stream:
            self.call("pg_restore", "--list", stdin=stream)

    def create(self, database):
        if not DB_NAME.fullmatch(database):
            raise ValueError("Refusing a database name outside the restore-check namespace")
        self.call("createdb", *self.connection(), "--template", "template0", database)

    def restore(self, database, dump):
        if not DB_NAME.fullmatch(database):
            raise ValueError("Refusing a database name outside the restore-check namespace")
        with dump.open("rb") as stream:
            self.call("pg_restore", *self.connection(), "--no-owner", "--no-privileges",
                      "--exit-on-error", "--single-transaction", "--dbname", database, stdin=stream)

    def query(self, database, sql):
        return json.loads(self.call("psql", *self.connection(), "--dbname", database, "--no-psqlrc",
                                    "--tuples-only", "--no-align", "--set", "ON_ERROR_STOP=1",
                                    input=sql.encode()))

    def drop(self, database):
        if not DB_NAME.fullmatch(database):
            raise ValueError("Refusing to remove a database outside the restore-check namespace")
        self.call("dropdb", *self.connection(), "--force", database)


SUMMARY_SQL = """
SELECT jsonb_build_object(
 'totals', jsonb_build_object('sheets',(SELECT count(*) FROM sheets),
   'lines',(SELECT count(*) FROM lines),'ops',(SELECT count(*) FROM ops),
   'snapshots',(SELECT count(*) FROM snapshots)),
 'sheets', COALESCE((SELECT jsonb_agg(jsonb_build_object('id',sheet_id,'version',head,
   'ops',(SELECT count(*) FROM ops WHERE line_id=l.id))) FROM lines l WHERE is_main), '[]'::jsonb),
 'invalid_lines', (SELECT count(*) FROM lines l WHERE
   NOT EXISTS (SELECT 1 FROM snapshots s WHERE s.line_id=l.id AND s.seq <= l.head)
   OR EXISTS (SELECT 1 FROM ops o WHERE o.line_id=l.id AND o.seq > l.head)
   OR (SELECT count(*) FROM ops o WHERE o.line_id=l.id AND o.seq >
     (SELECT max(seq) FROM snapshots s WHERE s.line_id=l.id AND s.seq <= l.head))
     <> l.head - (SELECT max(seq) FROM snapshots s WHERE s.line_id=l.id AND s.seq <= l.head)),
 'blobs', COALESCE((SELECT jsonb_agg(jsonb_build_object('key',key,'bytes',bytes)) FROM (
   SELECT blob AS key, bytes FROM snapshots
   UNION SELECT op_blob, NULL::bigint FROM ops WHERE op_blob IS NOT NULL
   UNION SELECT inverse_blob, NULL::bigint FROM ops WHERE inverse_blob IS NOT NULL
   UNION SELECT source_blob, NULL::bigint FROM merge_receipts
 ) refs), '[]'::jsonb));
"""


def check_restored(summary, expected, blobs):
    if summary["invalid_lines"] != 0:
        raise ValueError("Restored line history cannot replay from its saved snapshots")
    actual = {s["id"]: s for s in summary["sheets"]}
    for sheet in expected["sheets"]:
        if actual.get(sheet["id"]) != sheet:
            raise ValueError("A restored seeded sheet version or operation count does not match")
    if any(summary["totals"].get(key) != value for key, value in expected.get("totals", {}).items()):
        raise ValueError("Restored database totals do not match")
    checked = {}
    for path in blobs.rglob("*"):
        if path.is_file():
            if not HASH.fullmatch(path.name) or path.parent.name != path.name[:2] or sha256(path) != path.name:
                raise ValueError("A restored blob content checksum does not match its key")
            checked[path.name] = path.stat().st_size
    referenced = set()
    for blob in summary["blobs"]:
        key = blob["key"]
        if key not in checked or (blob["bytes"] is not None and blob["bytes"] != checked[key]):
            raise ValueError("A referenced snapshot or operation blob is missing or has the wrong size")
        referenced.add(key)
    return {"totals": summary["totals"], "expected_sheets_verified": len(expected["sheets"]),
            "blob_files_checked": len(checked), "referenced_blobs_checked": len(referenced)}


def verify(source, expected, workspace, postgres, max_bytes=MAX_BYTES):
    restored = workspace / "restored"
    digest = unpack_backup(source, restored, max_bytes)
    dump = restored / "postgres.dump"
    postgres.validate(dump)
    database = "calc_restore_check_" + uuid.uuid4().hex
    created = False
    try:
        postgres.create(database)
        created = True
        postgres.restore(database, dump)
        summary = postgres.query(database, SUMMARY_SQL)
        result = check_restored(summary, expected, restored / "blobs")
    finally:
        if created:
            postgres.drop(database)
    return {"ok": True, "archive_sha256": digest, "postgres_restore": True,
            "temporary_database_removed": True, **result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--backup", type=Path, help="Private directory containing backup.tar.gz, manifest.json and SHA256SUMS")
    source.add_argument("--s3", help="Explicit private S3 backup prefix containing those three files")
    parser.add_argument("--region", default=os.environ.get("AWS_REGION", "us-east-2"))
    parser.add_argument("--expect", type=Path, required=True, help="Private JSON file with expected seeded sheet id/version/ops")
    parser.add_argument("--max-extracted-bytes", type=int, default=MAX_BYTES)
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error("Run as root to restore through the local postgres peer account")
    if args.max_extracted_bytes <= 0:
        parser.error("The extraction limit must be positive")
    def interrupted(_signum, _frame):
        raise InterruptedError("Restore check interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    os.umask(0o077)
    expected = expectations(args.expect)
    with tempfile.TemporaryDirectory(prefix="silicon-calc-restore-check-") as directory:
        workspace = Path(directory)
        source = download_backup(args.s3, args.region, workspace / "download") if args.s3 else args.backup.resolve()
        report = verify(source, expected, workspace, Postgres(), args.max_extracted_bytes)
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        # Exception messages from our own validation omit data; avoid printing raw
        # parser, filesystem or subprocess exceptions that could contain private text.
        message = str(error) if type(error) in (ValueError, RuntimeError) else type(error).__name__
        print("Calc backup restore check failed: " + message, file=sys.stderr)
        sys.exit(1)
