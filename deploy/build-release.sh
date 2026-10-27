#!/usr/bin/env bash
# Build on the same Linux architecture and libc generation as the service host.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[[ "$(uname -s)" == Linux ]] || { echo 'Build this release on Linux.' >&2; exit 1; }
output="${1:?Usage: deploy/build-release.sh OUTPUT_DIRECTORY}"
mkdir -p "$output"
output="$(cd "$output" && pwd)"
cd "$root"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target/deploy}"
# Source archives have reproducible timestamps. Cargo's mtime check can otherwise
# reuse old local crates, even when their contents changed. Keep dependency caches.
python3 deploy/clean-workspace-release.py
cargo build --locked --release -p calc-api -p calc-cli --bin calc-api --bin calc-preflight --bin calc
install -d "$output/bin" "$output/deploy" "$output/runner"
install -m 0755 "$CARGO_TARGET_DIR/release/calc-api" "$CARGO_TARGET_DIR/release/calc-preflight" "$CARGO_TARGET_DIR/release/calc" "$output/bin/"
install -m 0755 deploy/install.py deploy/backup.py deploy/finalize-release.py "$output/deploy/"
install -m 0644 deploy/*.service deploy/*.timer deploy/silicon-calc.apparmor deploy/runtime.env.example deploy/README.md "$output/deploy/"
install -m 0644 runner/requirements.txt "$output/runner/requirements.txt"
{
  printf 'architecture=%s\n' "$(uname -m)"
  printf 'rust=%s\n' "$(rustc --version)"
  printf 'source_revision=%s\n' "${CALC_SOURCE_REVISION:-$(git rev-parse HEAD 2>/dev/null || printf unknown)}"
  printf 'built_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$output/build.txt"
printf 'Linux release files: %s\n' "$output"
printf 'After staging web, finalize the complete release: python3 deploy/finalize-release.py %q\n' "$output"
