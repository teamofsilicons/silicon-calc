import os
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import backup
import install


class DeploymentTests(unittest.TestCase):
    def test_installer_subprocess_does_not_add_bytecode_to_verified_release(self):
        with tempfile.TemporaryDirectory() as directory:
            release = Path(directory)
            for name in ("install.py", "backup.py"):
                shutil.copyfile(Path(__file__).with_name(name), release / name)
            before = {p.name: backup.sha256(p) for p in release.iterdir()}
            environment = os.environ.copy()
            for name in ("PYTHONDONTWRITEBYTECODE", "PYTHONPYCACHEPREFIX", "PYTHONPATH"):
                environment.pop(name, None)
            result = subprocess.run([sys.executable, str(release / "install.py"), "--help"],
                                    cwd=release, env=environment, capture_output=True, text=True, check=True)
            self.assertIn("--release", result.stdout)
            self.assertEqual(sorted(p.name for p in release.iterdir()), sorted(before))
            self.assertEqual({p.name: backup.sha256(p) for p in release.iterdir()}, before)

    def test_environment_files_are_private_and_reject_line_injection(self):
        self.assertEqual(install.env_bytes({"VALUE": 'a"b\\c$'}), b'VALUE="a\\"b\\\\c$"\n')
        for value in ("first\nSECOND=bad", "null\0", "tab\t"):
            with self.assertRaises(ValueError):
                install.env_bytes({"VALUE": value})
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "api.env"
            install.atomic_write(path, b"private")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            install.atomic_write(path, b"changed")
            self.assertEqual(path.read_bytes(), b"changed")

    def test_release_manifest_covers_every_file_and_rejects_escaping_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            release = root / "releases/candidate"
            (release / "bin").mkdir(parents=True)
            (release / "web/.next/static").mkdir(parents=True)
            (release / "web/public").mkdir()
            (release / "deploy").mkdir()
            (release / "runner").mkdir()
            for name in ("calc-api", "calc-preflight", "calc"):
                path = release / "bin" / name
                path.write_bytes(b"\x7fELFfixture")
                path.chmod(0o755)
            for name in ("web/server.js", "web/.next/BUILD_ID", "deploy/install.py", "deploy/backup.py",
                         "runner/requirements.txt", "build.txt", "deploy/silicon-calc.apparmor", *("deploy/" + n for n in install.UNIT_NAMES)):
                (release / name).write_text("fixture")
            files = {p.relative_to(release).as_posix(): backup.sha256(p) for p in release.rglob("*") if p.is_file()}
            (release / "release-manifest.json").write_text(json.dumps({"version": 1, "files": files, "symlinks": {}}))
            with patch.object(install, "ROOT", root):
                install.verify_release(release)
                extra = release / "unlisted"
                extra.write_text("not in manifest")
                with self.assertRaisesRegex(ValueError, "complete file"):
                    install.verify_release(release)
                extra.unlink()
                (release / "web/server.js").write_text("modified")
                with self.assertRaisesRegex(ValueError, "complete file"):
                    install.verify_release(release)
                (release / "web/server.js").write_text("fixture")
                (root / "outside").write_text("outside")
                extra.symlink_to("../../outside")
                with self.assertRaisesRegex(ValueError, "Invalid release symlink"):
                    install.verify_release(release)
                extra.unlink()
                cache = release / "web/.next/cache"
                cache.symlink_to("/var/cache/silicon-calc-web")
                # A previously installed release remains a valid rollback candidate.
                install.verify_release(release)
                cache.unlink()
                cache.symlink_to("/var/cache/another-app")
                with self.assertRaisesRegex(ValueError, "managed web cache"):
                    install.verify_release(release)
                cache.unlink()
                cache.mkdir()
                with self.assertRaisesRegex(ValueError, "managed web cache"):
                    install.verify_release(release)

    def test_backup_stops_writes_and_restarts_before_upload(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            blobs, config = root / "blobs", root / "config"
            blobs.mkdir()
            config.mkdir()
            (blobs / "cell.blob").write_bytes(b"saved cells")
            (config / "api.env").write_bytes(b"fixture config")
            events = []

            def command(*args, **kwargs):
                events.append(args)
                if args[0] == "aws":
                    self.assertIn(("systemctl", "start", "calc-api.service"), events)
                    self.assertIn(("systemctl", "start", "calc-web.service"), events)
                return subprocess.CompletedProcess(args, 0)

            def dump(args, **kwargs):
                self.assertEqual(args[0], "runuser")
                self.assertIn(("systemctl", "stop", "calc-api.service"), events)
                self.assertIn(("systemctl", "stop", "calc-web.service"), events)
                kwargs["stdout"].write(b"database snapshot")
                return subprocess.CompletedProcess(args, 0)

            with patch.multiple(backup, BACKUPS=root / "backups", BLOBS=blobs, CONFIG=config, CURRENT=root / "current"), \
                    patch.object(backup, "active_services", return_value=list(backup.SERVICES)), \
                    patch.object(backup, "run", side_effect=command), patch.object(backup.subprocess, "run", side_effect=dump):
                result = backup.create_backup("calc-backups-fixture", "us-east-2")
            self.assertEqual(result.stat().st_mode & 0o777, 0o700)
            with tarfile.open(result / "backup.tar.gz") as archive:
                self.assertEqual(archive.extractfile("postgres.dump").read(), b"database snapshot")
                self.assertEqual(archive.extractfile("blobs/cell.blob").read(), b"saved cells")
            self.assertTrue((result / "uploaded").is_file())

    def test_backup_failure_still_restarts_previously_active_services(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(backup, "BACKUPS", Path(directory)), \
                patch.object(backup, "active_services", return_value=list(backup.SERVICES)), \
                patch.object(backup, "run") as commands, \
                patch.object(backup.subprocess, "run", side_effect=RuntimeError("dump failed")):
            with self.assertRaisesRegex(RuntimeError, "dump failed"):
                backup.create_backup("calc-backups-fixture", "us-east-2")
            commands.assert_any_call("systemctl", "start", "calc-api.service")
            commands.assert_any_call("systemctl", "start", "calc-web.service")

    def test_failed_candidate_restores_previous_release_configuration_and_units(self):
        for profile_state, failure in ((state, phase) for state in ("loaded", "unloaded", "absent")
                                       for phase in ("readiness", "profile load")):
            with self.subTest(profile_state=profile_state, failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                app, config, units = root / "app", root / "config", root / "units"
                profile = root / "apparmor/silicon-calc"
                profile.parent.mkdir()
                if profile_state != "absent":
                    profile.write_bytes(b"previous Calc profile")
                previous, candidate = app / "releases/previous", app / "releases/candidate"
                previous.mkdir(parents=True)
                (candidate / "deploy").mkdir(parents=True)
                (candidate / "web/.next").mkdir(parents=True)
                (candidate / "deploy/silicon-calc.apparmor").write_bytes(b"candidate Calc profile")
                config.mkdir()
                units.mkdir()
                (app / "current").symlink_to(previous)
                for name in ("api.env", "web.env", "backup.env"):
                    (config / name).write_bytes(b"previous configuration")
                for name in install.UNIT_NAMES:
                    (units / name).write_bytes(b"previous unit")
                    (candidate / "deploy" / name).write_bytes(b"candidate unit")
                # Patch managed cache access so the test cannot touch /var/cache.
                real_path = install.Path
                cache_path = root / "cache"
                def path(value):
                    return cache_path if value == "/var/cache/silicon-calc-web" else real_path(value)
                events = []
                def command(*args, **kwargs):
                    events.append((args, profile.read_bytes() if profile.is_file() else None))
                    if failure == "profile load" and args[0] == "apparmor_parser" and sum(e[0][0] == "apparmor_parser" for e in events) == 1:
                        raise RuntimeError("profile load failed")
                    return subprocess.CompletedProcess(args, 0)
                account = type("Account", (), {"pw_uid": os.getuid(), "pw_gid": os.getgid()})()
                with patch.multiple(install, ROOT=app, CONFIG=config, UNITS=units, APPARMOR_PROFILE=profile), \
                        patch.object(install, "Path", side_effect=path), \
                        patch.object(install, "apparmor_loaded", side_effect=[profile_state == "loaded", True]), \
                        patch.multiple(backup, BACKUPS=root / "backups", BLOBS=root / "blobs"), \
                        patch.object(install, "verify_release"), \
                        patch.object(install, "settings", return_value=(b"new api", b"new web")), \
                        patch.object(install.pwd, "getpwnam", return_value=account), \
                        patch.object(backup, "active_services", return_value=list(backup.SERVICES)), \
                        patch.object(backup, "create_backup"), patch.object(backup, "run", side_effect=command), \
                        patch.object(backup, "restart") as restart, \
                        patch.object(install.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)), \
                        patch.object(install, "wait_healthy", side_effect=RuntimeError("candidate not ready")):
                    with self.assertRaisesRegex(RuntimeError, "candidate not ready|profile load failed"):
                        install.install(candidate, "fixture-secret", "us-east-2", "fixture-backup-bucket")
                    restart.assert_called_once_with(list(backup.SERVICES))
                commands = [args for args, _ in events]
                load = ("apparmor_parser", "-r", str(profile))
                if failure == "readiness":
                    self.assertLess(commands.index(load), commands.index(("systemctl", "start", "calc-api.service")))
                else:
                    self.assertNotIn(("systemctl", "start", "calc-api.service"), commands)
                profile_events = [event for event in events if event[0][0] == "apparmor_parser"]
                self.assertEqual(profile_events[0], (load, b"candidate Calc profile"))
                if profile_state == "loaded":
                    self.assertEqual(profile_events[1], (load, b"previous Calc profile"))
                else:
                    self.assertEqual(profile_events[1][0], ("apparmor_parser", "-R", str(candidate / "deploy/silicon-calc.apparmor")))
                self.assertEqual(len(profile_events), 2)
                self.assertEqual((app / "current").resolve(), previous.resolve())
                for name in ("api.env", "web.env", "backup.env"):
                    self.assertEqual((config / name).read_bytes(), b"previous configuration")
                for name in install.UNIT_NAMES:
                    self.assertEqual((units / name).read_bytes(), b"previous unit")
                if profile_state == "absent":
                    self.assertFalse(profile.exists())
                else:
                    self.assertEqual(profile.read_bytes(), b"previous Calc profile")


if __name__ == "__main__":
    unittest.main()
