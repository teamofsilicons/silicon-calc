import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
import uuid

spec = importlib.util.spec_from_file_location("verify_backup", Path(__file__).with_name("verify-backup.py"))
verify_backup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify_backup)

EXPECTED = {"sheets": [{"id": "seed", "version": 1, "ops": 1}]}
BLOB = b"test snapshot content"
KEY = hashlib.sha256(BLOB).hexdigest()


def make_backup(root, dump=b"PGDMPfixture", extra=None, blob=BLOB):
    source = root / "backup"
    source.mkdir()
    manifest = {"kind": "silicon-calc-backup", "version": 1,
                "postgres_sha256": hashlib.sha256(dump).hexdigest()}
    (source / "manifest.json").write_text(json.dumps(manifest))
    with tarfile.open(source / "backup.tar.gz", "w:gz") as archive:
        for name, content in (("postgres.dump", dump), ("manifest.json", json.dumps(manifest).encode()),
                              (f"blobs/{KEY[:2]}/{KEY}", blob), ("config/api.env", b"MUST_NOT_EXTRACT=private")):
            info = tarfile.TarInfo(name)
            info.size = len(content)
            archive.addfile(info, io.BytesIO(content))
        if extra:
            archive.addfile(extra)
    (source / "SHA256SUMS").write_text(verify_backup.sha256(source / "backup.tar.gz") + "  backup.tar.gz\n")
    return source


class FakePostgres:
    def __init__(self, *, fail_restore=False, wrong_version=False, missing_blob=False):
        self.calls = []
        self.fail_restore, self.wrong_version, self.missing_blob = fail_restore, wrong_version, missing_blob

    def validate(self, dump):
        self.calls.append("validate")

    def create(self, database):
        self.calls.append(("create", database))

    def restore(self, database, dump):
        self.calls.append(("restore", database))
        if self.fail_restore:
            raise RuntimeError("restore failed")

    def query(self, database, sql):
        self.calls.append(("query", database))
        return {"totals": {"sheets": 1, "lines": 1, "ops": 1, "snapshots": 1}, "invalid_lines": 0,
                "sheets": [{"id": "seed", "version": 2 if self.wrong_version else 1, "ops": 1}],
                "blobs": [{"key": "f" * 64 if self.missing_blob else KEY, "bytes": len(BLOB)}]}

    def drop(self, database):
        self.calls.append(("drop", database))


class BackupVerificationTests(unittest.TestCase):
    def test_restore_verifies_data_without_extracting_environment_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pg = FakePostgres()
            result = verify_backup.verify(make_backup(root), EXPECTED, root, pg)
            self.assertTrue(result["postgres_restore"])
            self.assertEqual(result["referenced_blobs_checked"], 1)
            self.assertFalse((root / "restored/config").exists())
            self.assertEqual((root / "restored/postgres.dump").stat().st_mode & 0o777, 0o600)
            database = pg.calls[1][1]
            self.assertRegex(database, r"^calc_restore_check_[0-9a-f]{32}$")
            self.assertEqual(pg.calls[-1], ("drop", database))

    def test_restore_and_validation_failures_always_drop_the_owned_database(self):
        for failure in ("fail_restore", "wrong_version", "missing_blob", "corrupt_blob"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                pg = FakePostgres(**({failure: True} if failure != "corrupt_blob" else {}))
                source = make_backup(root, blob=b"damaged" if failure == "corrupt_blob" else BLOB)
                with self.assertRaises((ValueError, RuntimeError)):
                    verify_backup.verify(source, EXPECTED, root, pg)
                self.assertEqual(pg.calls[-1], ("drop", pg.calls[1][1]))

    def test_bad_archive_checksum_cannot_create_a_database(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = make_backup(root)
            (source / "backup.tar.gz").write_bytes(b"changed archive")
            pg = FakePostgres()
            with self.assertRaisesRegex(ValueError, "archive checksum"):
                verify_backup.verify(source, EXPECTED, root, pg)
            self.assertEqual(pg.calls, [])

    def test_tar_traversal_and_links_are_rejected_before_database_actions(self):
        for kind in ("traversal", "symlink", "hardlink"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                member = tarfile.TarInfo("../outside" if kind == "traversal" else "blobs/link")
                if kind != "traversal":
                    member.type = tarfile.SYMTYPE if kind == "symlink" else tarfile.LNKTYPE
                    member.linkname = "/etc/silicon-calc/api.env"
                pg = FakePostgres()
                with self.assertRaisesRegex(ValueError, "unsafe"):
                    verify_backup.verify(make_backup(root, extra=member), EXPECTED, root, pg)
                self.assertEqual(pg.calls, [])
                self.assertFalse((root.parent / "outside").exists())

    def test_cleanup_refuses_any_production_database_name(self):
        pg = verify_backup.Postgres()
        for method in (pg.create, pg.drop):
            with self.assertRaises(ValueError):
                method("silicon_calc")

    @unittest.skipUnless(os.environ.get("CALC_BACKUP_TEST_POSTGRES") == "1", "explicit local Postgres integration opt-in")
    def test_real_pg_dump_restore_and_blob_verification(self):
        pg = verify_backup.Postgres(prefix=(), host="127.0.0.1", port=5482,
                                   bin_dir="/opt/homebrew/opt/postgresql@16/bin")
        source_database = "calc_restore_check_" + uuid.uuid4().hex
        pg.create(source_database)
        try:
            schema = f"""
            CREATE TABLE sheets(id text PRIMARY KEY);
            CREATE TABLE lines(id text PRIMARY KEY, sheet_id text, is_main boolean, head bigint);
            CREATE TABLE ops(line_id text, seq bigint, op_blob text, inverse_blob text);
            CREATE TABLE snapshots(line_id text, seq bigint, blob text, bytes bigint);
            CREATE TABLE merge_receipts(source_blob text);
            INSERT INTO sheets VALUES ('seed');
            INSERT INTO lines VALUES ('line', 'seed', true, 1);
            INSERT INTO ops VALUES ('line', 1, '{KEY}', NULL);
            INSERT INTO snapshots VALUES ('line', 0, '{KEY}', {len(BLOB)});
            """
            pg.call("psql", *pg.connection(), "--dbname", source_database, "--no-psqlrc", "--set", "ON_ERROR_STOP=1",
                    input=schema.encode())
            dump = pg.call("pg_dump", *pg.connection(), "--format=custom", "--dbname", source_database)
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result = verify_backup.verify(make_backup(root, dump=dump), EXPECTED, root, pg)
                self.assertEqual(result["totals"], {"sheets": 1, "lines": 1, "ops": 1, "snapshots": 1})
                self.assertTrue(result["temporary_database_removed"])
                self.assertEqual(result["referenced_blobs_checked"], 1)
        finally:
            pg.drop(source_database)


if __name__ == "__main__":
    unittest.main()
