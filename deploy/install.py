#!/usr/bin/env python3
"""Install one prepared native release; restore the prior files and services if checks fail."""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import subprocess
import sys
import time
import urllib.request

# Importing the adjacent helper must not add files to the immutable release
# before its complete manifest is verified, even when invoked without python -B.
sys.dont_write_bytecode = True
import backup

ROOT = Path("/opt/silicon-calc")
CONFIG = Path("/etc/silicon-calc")
UNITS = Path("/etc/systemd/system")
APPARMOR_PROFILE = Path("/etc/apparmor.d/silicon-calc")
APPARMOR_NAME = "silicon-calc-converters"
UNIT_NAMES = ("calc-api.service", "calc-web.service", "calc-backup.service", "calc-backup.timer")


def env_bytes(values):
    lines = []
    for key, value in sorted(values.items()):
        if not re.fullmatch(r"[A-Z][A-Z0-9_]*", key) or not isinstance(value, str):
            raise ValueError("Secret settings must have environment names and string values")
        if any(ord(c) < 32 or ord(c) == 127 for c in value):
            raise ValueError(f"Control characters are not allowed in {key}")
        lines.append(key + '="' + value.replace("\\", "\\\\").replace('"', '\\"') + '"\n')
    return "".join(lines).encode()


def atomic_write(path, contents, mode=0o600):
    temporary = path.with_name(path.name + ".next")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(contents)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        os.chmod(path, mode)
    finally:
        temporary.unlink(missing_ok=True)


def switch_release(release):
    temporary = ROOT / "current.next"
    temporary.unlink(missing_ok=True)
    temporary.symlink_to(release)
    os.replace(temporary, ROOT / "current")


def verify_release(release):
    if release.parent != ROOT / "releases" or not release.is_dir():
        raise ValueError("Release must be a directory immediately below /opt/silicon-calc/releases")
    for name in ("bin/calc-api", "bin/calc-preflight", "bin/calc"):
        path = release / name
        if not path.is_file() or not os.access(path, os.X_OK):
            raise ValueError(f"Release is missing executable {name}")
        with path.open("rb") as stream:
            if stream.read(4) != b"\x7fELF":
                raise ValueError(f"{name} must be a Linux ELF executable")
    for name in ("build.txt", "runner/requirements.txt", "deploy/install.py", "deploy/backup.py", "deploy/silicon-calc.apparmor",
                 "web/server.js", "web/.next/BUILD_ID", *("deploy/" + name for name in UNIT_NAMES)):
        if not (release / name).is_file():
            raise ValueError(f"Release is missing {name}")
    for name in ("web/.next/static", "web/public"):
        if not (release / name).is_dir():
            raise ValueError(f"Release is missing {name}")
    manifest = json.loads((release / "release-manifest.json").read_text())
    if manifest.get("version") != 1 or not isinstance(manifest.get("files"), dict) or not isinstance(manifest.get("symlinks"), dict):
        raise ValueError("Unsupported release manifest")
    actual_files, actual_links = {}, {}
    for parent, directories, files in os.walk(release, followlinks=False):
        for name in directories + files:
            path = Path(parent) / name
            relative = path.relative_to(release).as_posix()
            if relative == "web/.next/cache":
                # Installed releases attach this one writable runtime directory.
                # It is not a build artifact and is never part of the manifest.
                if not path.is_symlink() or os.readlink(path) != "/var/cache/silicon-calc-web":
                    raise ValueError("Invalid managed web cache attachment")
                continue
            if relative in ("release-manifest.json", "SHA256SUMS"):
                if not path.is_file() or path.is_symlink():
                    raise ValueError("Manifest and checksum inventory must be regular files")
                continue
            if name.startswith(".env"):
                raise ValueError("Environment files must not be included in a release")
            if path.is_symlink():
                target = os.readlink(path)
                if os.path.isabs(target) or not path.resolve(strict=True).is_relative_to(release):
                    raise ValueError(f"Invalid release symlink: {relative}")
                actual_links[relative] = target
            elif path.is_file():
                actual_files[relative] = backup.sha256(path)
            elif not path.is_dir():
                raise ValueError(f"Unsupported release entry: {relative}")
    if actual_files != manifest["files"] or actual_links != manifest["symlinks"]:
        raise ValueError("Release manifest does not match the complete file and symlink inventory")


def settings(secret_id, region):
    # Never print the result or place it in command arguments, logs or browser assets.
    raw = backup.run("aws", "secretsmanager", "get-secret-value", "--secret-id", secret_id,
                     "--region", region, "--query", "SecretString", "--output", "text", capture=True).stdout
    parsed = json.loads(raw)
    api, web = dict(parsed["api"]), dict(parsed["web"])
    for key in ("CALC_DATABASE_URL", "CALC_ACCOUNTS_URL", "CALC_APP_SECRET", "CALC_WEB_URL"):
        if not api.get(key):
            raise ValueError(f"Missing {key} in deployment secret")
    for key in ("APP_ID", "APP_SECRET", "SESSION_SECRET", "PUBLIC_URL", "ACCOUNTS_URL", "APP_API_URL"):
        if not web.get(key):
            raise ValueError(f"Missing web {key} in deployment secret")
    if len(web["SESSION_SECRET"]) < 32:
        raise ValueError("SESSION_SECRET must contain at least 32 characters")
    required = {"CALC_BIND": "127.0.0.1:8730", "CALC_BLOB_DIR": "/var/lib/silicon-calc/blobs",
                "CALC_RUNNER_PYTHON": "/opt/silicon-calc/runner-venv/bin/python",
                "CALC_CODE_SANDBOX": "bwrap", "CALC_RUNNER_NUMBERS_ENABLE": "0"}
    for key, value in required.items():
        if key in api and api[key] != value:
            raise ValueError(f"{key} must match the native deployment layout")
        api[key] = value
    if not api["CALC_ACCOUNTS_URL"].startswith("https://") or api.get("CALC_ALLOW_HTTP_ACCOUNTS") in ("1", "true"):
        raise ValueError("Deployed Silicon Accounts must use HTTPS")
    web.update({"NODE_ENV": "production", "HOSTNAME": "127.0.0.1", "PORT": "8731", "NEXT_TELEMETRY_DISABLED": "1"})
    return env_bytes(api), env_bytes(web)


def wait_healthy(url, status, *, attempts=60, headers=None):
    for _ in range(attempts):
        try:
            request = urllib.request.Request(url, headers=headers or {})
            with urllib.request.urlopen(request, timeout=3) as response:
                if response.status == status:
                    return
        except (OSError, ValueError):
            pass
        time.sleep(1)
    raise RuntimeError(f"Release health check failed: {url}")


def restore_files(saved, paths):
    for path in paths:
        source = saved / str(path).lstrip("/")
        if source.is_file():
            path.parent.mkdir(parents=True, exist_ok=True)
            atomic_write(path, source.read_bytes(), source.stat().st_mode & 0o777)
        else:
            path.unlink(missing_ok=True)


def apparmor_loaded():
    profiles = Path("/sys/kernel/security/apparmor/profiles").read_text().splitlines()
    return any(line.startswith(APPARMOR_NAME + " ") for line in profiles)


def install(release, secret_id, region, bucket):
    verify_release(release)
    api_env, web_env = settings(secret_id, region)
    previous = (ROOT / "current").resolve() if (ROOT / "current").is_symlink() else None
    if previous == release:
        raise ValueError("This release is already current; use a new immutable release directory")
    profile_was_loaded = apparmor_loaded()
    if profile_was_loaded and not APPARMOR_PROFILE.is_file():
        raise ValueError("Loaded Calc AppArmor profile has no restorable configuration")
    calc, web = pwd.getpwnam("calc"), pwd.getpwnam("calc-web")
    CONFIG.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(CONFIG, 0o700)
    for path, account in ((backup.BLOBS, calc), (Path("/var/cache/silicon-calc-web"), web)):
        path.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chown(path, account.pw_uid, account.pw_gid)
        os.chmod(path, 0o700)
    cache = release / "web/.next/cache"
    if cache.exists() or cache.is_symlink():
        if cache.is_symlink():
            cache.unlink()
        else:
            shutil.rmtree(cache)
    cache.parent.mkdir(parents=True, exist_ok=True)
    cache.symlink_to("/var/cache/silicon-calc-web")
    paths = [CONFIG / name for name in ("api.env", "web.env", "backup.env")]
    paths.extend(UNITS / name for name in UNIT_NAMES)
    paths.append(APPARMOR_PROFILE)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    saved = backup.BACKUPS / ("deploy-" + stamp)
    saved.mkdir(mode=0o700, parents=True)
    for path in paths:
        if path.is_file():
            destination = saved / str(path).lstrip("/")
            destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            shutil.copy2(path, destination)
    (saved / "previous.json").write_text(json.dumps({"release": str(previous) if previous else None}) + "\n")
    active = backup.active_services()
    timer_active = subprocess.run(["systemctl", "is-active", "--quiet", "calc-backup.timer"], check=False).returncode == 0
    enabled = {name: subprocess.run(["systemctl", "is-enabled", "--quiet", name], check=False,
                                   stderr=subprocess.DEVNULL).returncode == 0 for name in UNIT_NAMES}
    profile_changed = False
    try:
        if timer_active:
            backup.run("systemctl", "stop", "calc-backup.timer")
        if previous:
            backup.create_backup(bucket, region, restart_services=False)
        else:
            for service in backup.SERVICES:
                subprocess.run(["systemctl", "stop", service], check=False, stderr=subprocess.DEVNULL)
        atomic_write(CONFIG / "api.env", api_env)
        atomic_write(CONFIG / "web.env", web_env)
        atomic_write(CONFIG / "backup.env", env_bytes({"CALC_BACKUP_BUCKET": bucket, "AWS_REGION": region}))
        for name in UNIT_NAMES:
            atomic_write(UNITS / name, (release / "deploy" / name).read_bytes(), 0o644)
        # Require this profile before either ExecStartPre or the API starts.
        # Ubuntu's global user-namespace policy is never changed.
        profile_changed = True
        APPARMOR_PROFILE.parent.mkdir(parents=True, exist_ok=True)
        atomic_write(APPARMOR_PROFILE, (release / "deploy/silicon-calc.apparmor").read_bytes(), 0o644)
        backup.run("apparmor_parser", "-r", str(APPARMOR_PROFILE))
        switch_release(release)
        backup.run("systemctl", "daemon-reload")
        # ExecStartPre runs the actual converters with the same account and restrictions as the API.
        backup.run("systemctl", "start", "calc-api.service")
        wait_healthy("http://127.0.0.1:8730/readyz", 204)
        backup.run("systemctl", "start", "calc-web.service")
        wait_healthy("http://127.0.0.1:8731/robots.txt", 200)
        backup.run("systemctl", "enable", "calc-api.service", "calc-web.service", "calc-backup.timer")
        backup.run("systemctl", "start", "calc-backup.timer")
    except BaseException:
        subprocess.run(["systemctl", "stop", "calc-backup.timer"], check=False)
        for service in backup.SERVICES:
            subprocess.run(["systemctl", "stop", service], check=False)
        for name, was_enabled in enabled.items():
            if not was_enabled:
                subprocess.run(["systemctl", "disable", name], check=False)
        restore_files(saved, paths)
        if profile_changed:
            if profile_was_loaded:
                backup.run("apparmor_parser", "-r", str(APPARMOR_PROFILE))
            elif apparmor_loaded():
                # Only unload the candidate's named Calc profile, never the
                # system policy or profiles belonging to other applications.
                backup.run("apparmor_parser", "-R", str(release / "deploy/silicon-calc.apparmor"))
        if previous:
            switch_release(previous)
        else:
            (ROOT / "current").unlink(missing_ok=True)
        backup.run("systemctl", "daemon-reload")
        for name, was_enabled in enabled.items():
            if was_enabled:
                backup.run("systemctl", "enable", name)
        backup.restart(active)
        if timer_active:
            backup.run("systemctl", "start", "calc-backup.timer")
        raise
    print(f"Calc release installed: {release}")
    print(f"Prior configuration retained: {saved}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", type=Path, required=True)
    parser.add_argument("--secret-id", required=True)
    parser.add_argument("--backup-bucket", required=True)
    parser.add_argument("--region", default="us-east-2")
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error("Run this installer as root")
    os.umask(0o077)
    with backup.acquire_lock():
        install(args.release.resolve(), args.secret_id, args.region, args.backup_bucket)


if __name__ == "__main__":
    main()
