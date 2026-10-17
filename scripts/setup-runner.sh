#!/usr/bin/env bash
# Builds the Python the code runner uses: a virtual environment at .dev/runner-venv with exactly the packages in
# runner/requirements.txt (pandas, numpy, pyarrow, duckdb, numbers-parser, xlwt).
#
#   scripts/setup-runner.sh            # create or update it
#   scripts/setup-runner.sh --fresh    # throw the old one away first
#
# Then point the service at it (scripts/dev.sh and the tests find it on their own when it is there):
#   export CALC_RUNNER_PYTHON="$PWD/.dev/runner-venv/bin/python"
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
venv="$root/.dev/runner-venv"
requirements="$root/runner/requirements.txt"
python_version="${CALC_RUNNER_PYTHON_VERSION:-3.14}"

uv="$(command -v uv || true)"
if [[ -z "$uv" && -x /opt/homebrew/bin/uv ]]; then
  uv=/opt/homebrew/bin/uv
fi
if [[ -z "$uv" ]]; then
  echo "setup-runner: uv is not installed. Install it (brew install uv, or https://docs.astral.sh/uv/), then run this again." >&2
  exit 1
fi

if [[ "${1:-}" == "--fresh" && -d "$venv" ]]; then
  echo "setup-runner: removing the old environment at $venv"
  rm -rf "$venv"
fi

mkdir -p "$root/.dev"
if [[ ! -x "$venv/bin/python" ]]; then
  echo "setup-runner: creating $venv with Python $python_version"
  "$uv" venv --python "$python_version" "$venv"
fi

echo "setup-runner: installing runner/requirements.txt"
"$uv" pip install --python "$venv/bin/python" --requirement "$requirements"

# Check that every package the runner gives code imports, so a broken install shows up now and not mid-run.
"$venv/bin/python" -I -B - <<'PY'
import importlib, sys
mods = ["pandas", "numpy", "pyarrow", "pyarrow.parquet", "duckdb", "numbers_parser", "xlwt"]
for m in mods:
    importlib.import_module(m)
import pandas, numpy, pyarrow, duckdb, numbers_parser
print(f"setup-runner: Python {sys.version.split()[0]}, pandas {pandas.__version__}, numpy {numpy.__version__}, "
      f"pyarrow {pyarrow.__version__}, duckdb {duckdb.__version__}, numbers-parser {numbers_parser.__version__}")
PY

echo "setup-runner: ready. CALC_RUNNER_PYTHON=$venv/bin/python"
