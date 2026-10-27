# Silicon Calc production

The web app is `https://calc.teamofsilicons.com`; the service is
`https://calc-api.teamofsilicons.com`. JavaScript commands execute in the browser's
isolated worker. The service commits their materialized results and runs only
trusted file converters.

The native Ubuntu 24.04 ARM64 host is in AWS `us-west-2`. Its retained private
artifact bucket and runtime secret are in `us-east-2`; pass that region for
artifact and secret operations. This split avoids the exhausted host/IP quota
in `us-east-2`. Use the `silicon-production` profile. The host stack is
`silicon-calc-production`; `production.json` records verified release details.

Only ports 80 and 443 are public. Administration uses SSM, with no inbound SSH.
Caddy terminates HTTPS and proxies to web `127.0.0.1:8731` and API
`127.0.0.1:8730`. One API process owns the in-memory workbook actors. Postgres
uses Unix-socket peer authentication for the `calc` account. Sheet data lives in
the `silicon_calc` database and `/var/lib/silicon-calc/blobs` together.

## Release

1. Run the repository checks. `package-source.py` archives the complete selected
   working source, including uncommitted files, and records a content hash and
   its base Git revision. It excludes local configuration, credentials, and
   development state. Review the archive before upload.
2. Upload the source under the private bucket's `releases/` prefix. Through SSM,
   download it and verify its archive SHA-256 before extracting or executing it.
3. On a bootstrapped host, run `bash deploy/build-on-host.sh`. It installs the
   pinned converter dependencies, builds API/CLI and web, checks the web, and
   produces a complete file/symlink manifest. The build uses no runtime secrets.
4. Run the release's `deploy/install.py` with its absolute release path, runtime
   secret ARN, backup bucket and `--region us-east-2`. Secrets are read with the
   instance role into root-only environment files. The installer checks the
   manifest, backs up existing state, switches `current`, runs real converter
   preflight, and checks readiness. Failures restore prior units/configuration
   and the prior release link. Schema downgrades are not automatic.
5. Validate and install the release's Caddyfile, then reload Caddy. Verify public
   HTTPS, a genuine Accounts sign-in, editing/reload, browser JavaScript, file
   export, and the exact deployed hashes before recording completion.

Native Numbers automation is disabled. On Linux, values-only Numbers conversion
uses the sandboxed library; formula-bearing Numbers export reports the library's
limitation. LibreOffice supplies XLS, ODS, and PDF conversions. Both converters
run without network access, with read-only runtime files and private scratch.

## Backups and recovery

`calc-backup.timer` runs daily at 03:20 UTC. The backup job briefly stops web and
API, drains open workbooks, captures Postgres and blobs together, restarts the
services, and uploads an encrypted private archive. It retains two successful
local backups and 30 days in S3. Archives include private runtime configuration
and must never be published. Host root disks are encrypted and retained on
instance termination.

Run `verify-backup.py` against a downloaded or S3 backup and an explicit seed
sheet expectation. It checks archive/dump hashes, restores into an isolated
temporary database, verifies sheet heads and referenced blobs, and removes only
its own temporary resources. Test restore before relying on a backup.

To roll back code, select a verified prior release with the installer. Preserve
the database and blobs unless an explicitly reviewed data restore is required.
Use SSM in `us-west-2`; use the release receipt's region for Secrets Manager/S3.
