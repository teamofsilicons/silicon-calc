# Trusted file converters

JavaScript commands run only in the browser. This directory contains fixed file conversion programs;
no submitted JavaScript, Python or SQL is executed by the service.

Run `scripts/setup-runner.sh` to install the pinned Python converter environment into `.dev/runner-venv`.
The service uses `CALC_RUNNER_PYTHON`; Node is not a service interpreter. Python converters have no IP
network or personal-file access. macOS uses `sandbox-exec`; Linux needs `bubblewrap`. They read the
interpreter and system libraries and write only their private scratch directory. Conversions have a
60-second timeout, a 2 GB memory limit, capped output and a scratch-space limit. The legacy settings
`CALC_CODE_TIMEOUT_SECONDS` and `CALC_CODE_SANDBOX` now apply only to these conversions.
`CALC_CODE_SANDBOX=off` disables converter confinement and is for local development only.

Parquet uses bundled DuckDB with a new in-memory database per conversion. Queries and file paths are
constructed internally, automatic extensions are disabled, and no database cache is shared across
conversions. The tab's first row supplies column names.

LibreOffice is required for XLS, ODS and PDF exports and XLS and ODS imports. Install the native app on
macOS or `libreoffice` on Linux, or set `CALC_RUNNER_LIBREOFFICE` to the `soffice` executable.
Each conversion gets its own profile, disables macros and link updates, and runs under the same
resource limits. The trusted converter additionally gets local UNIX sockets for its own IPC;
IP networking remains denied. The short `/tmp/sc` scratch path is intentional: LibreOffice's
socket name includes a profile hash and must fit the operating system's socket-path limit.

Native Numbers conversion is optional and **disabled by default**. On a local macOS desktop,
set `CALC_RUNNER_NUMBERS_ENABLE=1` to use installed Apple Numbers through a fixed AppleScript
and XLSX bridge, retaining formulas and formatting. The bridge starts Numbers through LaunchServices when
needed. This can activate Numbers and needs existing
macOS Automation permission. It is unsuitable for an exposed or unattended server: the launcher
has a timeout, but the already running Numbers app uses its own permissions and is outside the
runner's filesystem sandbox, process group and memory watchdog. Only the conversion's own
document is closed; existing documents are not modified. A timed-out app operation may continue.

With native conversion disabled (including Linux), `numbers-parser` reads Numbers files and
writes values and supported styles. Its writer does not support formulas: an export containing
formulas returns a clear error instead of silently replacing them with values. Rust callers can explicitly
set `ExportOptions::values_only`; HTTP Numbers downloads preserve formulas or fail. XLSX retains formulas
on all platforms.
