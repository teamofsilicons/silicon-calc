# Native Linux deployment

The deployment files prepare a native Ubuntu 24.04 ARM64 service. They do not publish DNS, create an
Accounts app, or make a release live until the installer is explicitly run on the target host.
Use one API process: live line actors, event streams and presence are held in that process.

## Build and stage

Build on the target Linux architecture with Rust 1.98, a C++ compiler, `pkg-config`, CMake and GNU
build tools. Bundled DuckDB makes the first build substantial. Two build jobs, 16 GiB RAM and at
least 20 GiB free build space are a reasonable starting allowance; these are capacity estimates,
not measured minimums. Reuse the build target between releases.

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/var/cache/silicon-calc-build \
  deploy/build-release.sh /opt/silicon-calc/releases/RELEASE
deploy/package-web.sh /opt/silicon-calc/releases/RELEASE/web
python3 deploy/finalize-release.py /opt/silicon-calc/releases/RELEASE
```

The first command creates `bin/calc-api`, `bin/calc-preflight`, `bin/calc`, the deployment files, Python pins and
build metadata. The web packager adds its standalone server and assets. The finalizer normalizes
read permissions and records every regular file and internal symlink in the release manifest.
Archive and verify the complete staged release when moving it between machines. Keep each release directory
immutable after installation, apart from the managed web cache symlink.

## Host prerequisites

- Dedicated `calc` and `calc-web` system accounts; a local PostgreSQL 16 database `silicon_calc`
  owned by role `calc`. Peer authentication over `/var/run/postgresql` avoids a database password.
- `/var/lib/silicon-calc/blobs`, owned by `calc`, mode 0700. Database records refer to these files;
  preserving only the database is not a usable backup.
- Runtime packages: `apparmor`, `bubblewrap`, `libreoffice-calc`, `fonts-dejavu-core`, `fonts-liberation`,
  `fontconfig`, `ca-certificates`, `libstdc++6`, `libgcc-s1`, PostgreSQL client tools and the AWS CLI.
- Managed Python 3.14 under `/opt/silicon-calc/python`, with the pinned environment under
  `/opt/silicon-calc/runner-venv`. The interpreter and packages must be readable by `calc`.
- Node at `/opt/node/bin/node` for the web app; no Node interpreter is used by the API.
- Unprivileged namespaces must work for the `calc` account. Do not disable the converter sandbox
  or add `RestrictNamespaces` to the API unit. Ubuntu AppArmor or a containing runtime can restrict
  namespaces; verify the actual host instead of weakening global restrictions. `calc-preflight`
  runs as `ExecStartPre` under the service account and systemd restrictions, and blocks startup if
  the real Python, LibreOffice or sandbox paths do not work.

The service requires the enforcing `silicon-calc-converters` AppArmor profile shipped in
`silicon-calc.apparmor`. Ubuntu 24.04 restricts unprivileged user namespaces for otherwise unconfined
systemd services; this named profile grants them only to Calc. Its disconnected-path mediation is
needed while bubblewrap builds its private mount root. The outer unit omits `ProtectKernelTunables`
because its masked `/proc` prevents the inner unprivileged proc mount; the profile instead denies
writes to `/proc/sys`, `/proc/sysrq-trigger` and `/sys`. The calc UID stays unprivileged, all other unit
protections remain, and converters still have private filesystem, PID and network namespaces.
Do not disable global AppArmor policy. Bootstrap probes through systemd with the explicit profile:
a shell launched through SSM can inherit different AppArmor permissions and give a misleading pass.

The bootstrap task owns OS installation. Its Python setup is equivalent to:

```sh
export UV_PYTHON_INSTALL_DIR=/opt/silicon-calc/python
uv python install 3.14
uv venv --python 3.14 /opt/silicon-calc/runner-venv
uv pip install --python /opt/silicon-calc/runner-venv/bin/python \
  --requirement /opt/silicon-calc/releases/RELEASE/runner/requirements.txt
chmod -R a+rX /opt/silicon-calc/python /opt/silicon-calc/runner-venv
```

Start with at least 8 GiB host memory when PostgreSQL, the web app and converters share a host.
The API unit caps its aggregate memory at 4 GiB; each converter also has a 2 GiB watchdog.
Monitor peak memory before reducing capacity, especially for large sheets or simultaneous conversions.
Allow scratch disk space: each converter may use up to 8 GiB before its watchdog stops it.

## Configuration and installation

The AWS secret is JSON with `api` and `web` objects containing string environment values.
Use [runtime.env.example](runtime.env.example) for API keys. Web keys are `APP_ID=calc`, `APP_SECRET`,
a persistent random `SESSION_SECRET` of at least 32 characters, `PUBLIC_URL`, `ACCOUNTS_URL`, and
`APP_API_URL=http://127.0.0.1:8730`. Accounts and the public web origin use HTTPS. Keep the app
secret out of source archives, process arguments, browser variables and logs.

```sh
python3 /opt/silicon-calc/releases/RELEASE/deploy/install.py \
  --release /opt/silicon-calc/releases/RELEASE \
  --secret-id SECRET_ARN --backup-bucket PRIVATE_BUCKET --region us-east-2
```

Run this as root only after reviewing the prepared release. The installer verifies checksums, retrieves
the secret into root-only environment files, saves prior configuration and units, drains an existing
service and takes a database/blob backup before migrations. It loads the Calc AppArmor profile before
starting preflight or the API. It switches `current` atomically, starts
the API (including real converter preflight), then the web app. API database readiness must return
204 at `http://127.0.0.1:8730/readyz`; web `/robots.txt` must return 200. `/healthz` is liveness only.
SIGTERM saves open lines. Ports 8730 and 8731 remain on loopback behind the TLS proxy.

On a failed readiness check, the installer restores the previous symlink, units, environment and Calc
AppArmor profile, then restarts previously running services. If no Calc profile was previously loaded,
rollback unloads only the candidate Calc profile. Database migrations are embedded in the API and run at startup;
they are not automatically reversed. Binary rollback therefore requires compatible migrations.
For incompatible changes, plan a separate data restore before accepting writes.

The TLS proxy must preserve streamed event responses without buffering and allow 500 MB uploads and
longer file conversions. Verify public HTTPS, authenticated editing, sharing, comments, downloads,
browser JavaScript commands and event updates before claiming a live release. Registration and Ting
consent are separate checks; a healthy process does not prove them.

## Backups and recovery

`calc-backup.timer` runs daily at 03:20 UTC plus up to ten minutes of jitter. The root-only backup
task takes the same deployment lock, stops the web app and drains the API, dumps `silicon_calc`, and
archives that dump with the blob directory and private environment files. It then restarts previously
active services before uploading. Failed dumps still restart the services; failed uploads retain the
local backup. Only two successfully uploaded local backups are retained. Deployment configuration
snapshots and failed partial backups are retained for manual inspection.

The private, versioned bucket stores each archive, manifest and checksum below `backups/`; its
30-day lifecycle is managed by infrastructure. Archives contain sheet data and secrets: retain bucket
encryption, public-access blocking and a narrowly scoped instance role. The role needs secret retrieval
for the selected secret and object upload for that bucket prefix. To request a backup now:

```sh
systemctl start calc-backup.service
journalctl -u calc-backup.service --since today
```

Recovery requires a matched archive. Stop both services, verify its checksum, inspect its manifest,
and restore the PostgreSQL dump and blob tree together on a prepared database. Restore ownership of
blobs to `calc` and permissions to 0700; restore environment files only as root. Start the matching
release and repeat readiness and authenticated checks. Do not mix blobs from one backup with a
database from another, or automatically restore over newer accepted writes.

Prove a particular backup restores before relying on it. Record a seed sheet's exact main-line version
and operation count in a private JSON file, for example:

```json
{"sheets":[{"id":"seed-sheet-id","version":2,"ops":2}]}
```

Then run the standalone checker against the completed local backup directory, or an explicit private
S3 prefix (use the bucket's region, which can differ from the host's):

```sh
sudo python3 deploy/verify-backup.py \
  --backup /var/backups/silicon-calc/backup-TIMESTAMP-ID --expect /root/calc-restore-expect.json
sudo python3 deploy/verify-backup.py \
  --s3 s3://BACKUP-BUCKET/backups/backup-TIMESTAMP-ID --region us-east-2 \
  --expect /root/calc-restore-expect.json
```

The checker verifies archive and dump hashes, restores into a fresh `calc_restore_check_*` database,
compares seeded versions and operation counts, checks history continuity from snapshots, and hashes
every restored blob against its content-addressed filename. It also verifies every snapshot, operation,
inverse and merge-receipt blob reference and snapshot size. Optional `totals` expectations can assert
`sheets`, `lines`, `ops` and `snapshots`; omit them when other launch checks create additional sheets.
It removes only its own temporary database and files, and never extracts or uses archived environment
files or stops the running services. A successful result proves database/blob restoration; run a
separate authenticated sheet read/edit journey to verify the live application. S3 checks additionally
need read permission for the selected backup objects. The default extraction limit is 64 GiB; change
`--max-extracted-bytes` explicitly for a larger backup and ensure sufficient temporary disk space.

## Validation

```sh
python3 -m unittest discover -s deploy -p 'test_*.py' -v
cargo test -p calc-api --test harness
CALC_CODE_SANDBOX=bwrap CALC_RUNNER_NUMBERS_ENABLE=0 \
  CALC_RUNNER_PYTHON=/opt/silicon-calc/runner-venv/bin/python \
  /opt/silicon-calc/current/bin/calc-preflight
```

The preflight exports and reopens XLSX, Parquet, XLS, ODS and Numbers and checks a real PDF signature.
Native Apple Numbers is disabled for unattended deployments. On Linux, Numbers files with formulas
cannot be exported losslessly; Calc returns an explicit limitation and XLSX preserves those formulas.
