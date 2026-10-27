#!/usr/bin/env bash
# Build the already verified, extracted source on the ARM64 host.
set -euo pipefail
umask 022
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export PATH=/opt/node/bin:/root/.cargo/bin:$PATH
export UV_PYTHON_INSTALL_DIR=/opt/silicon-calc/python
export CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/var/cache/silicon-calc-build
export CALC_SOURCE_REVISION="$(python3 -c 'import json;print(json.load(open("source-manifest.json"))["revision"])')"
release="/opt/silicon-calc/releases/$CALC_SOURCE_REVISION"
/opt/silicon-calc/uv/bin/uv pip install --python /opt/silicon-calc/runner-venv/bin/python --requirement runner/requirements.txt
# build-release invalidates all workspace release crates before using this shared
# target cache; normalized source-archive mtimes must never select an older binary.
bash deploy/build-release.sh "$release" > "/opt/silicon-calc/build/$CALC_SOURCE_REVISION-api.log" 2>&1 &
api_build=$!
if ! sh deploy/package-web.sh "$release/web" > "/opt/silicon-calc/build/$CALC_SOURCE_REVISION-web.log" 2>&1; then
  tail -100 "/opt/silicon-calc/build/$CALC_SOURCE_REVISION-web.log"
  wait "$api_build" || true
  exit 1
fi
if ! wait "$api_build"; then
  tail -100 "/opt/silicon-calc/build/$CALC_SOURCE_REVISION-api.log"
  exit 1
fi
install -m 0644 source-manifest.json deploy/Caddyfile "$release/"
python3 deploy/finalize-release.py "$release"
tar -czf "/opt/silicon-calc/build/$CALC_SOURCE_REVISION-release.tar.gz" -C "$release" .
sha256sum "/opt/silicon-calc/build/$CALC_SOURCE_REVISION-release.tar.gz"
aws s3 cp "/opt/silicon-calc/build/$CALC_SOURCE_REVISION-release.tar.gz" "s3://silicon-calc-production-artifacts-kjimieyvqzeh/releases/$CALC_SOURCE_REVISION-release.tar.gz" --region us-east-2 --only-show-errors
echo "Calc build complete: $CALC_SOURCE_REVISION"
