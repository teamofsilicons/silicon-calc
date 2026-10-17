#!/usr/bin/env bash
# Stops what scripts/dev.sh started: the web app, calc-api, the mock Ting, and the Silicon Accounts stack and Calc's
# Postgres when dev.sh was the one that started them (a stack or cluster that was already running is left running).
#
#   scripts/stop.sh            stop them
#   scripts/stop.sh --keep-accounts   leave the Silicon Accounts stack running
#   scripts/stop.sh --quiet    print nothing unless something goes wrong
#
# Only pids recorded in .dev/run/<name>.pid whose command still matches are stopped (the whole process group,
# SIGKILL after a grace period), so a stale pid file never stops anything else.
set -euo pipefail

SCRIPT=stop
# shellcheck source=lib/common.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"

KEEP_ACCOUNTS=0
QUIET=0
for arg in "$@"; do
  case "$arg" in
    --keep-accounts) KEEP_ACCOUNTS=1 ;;
    -q|--quiet) QUIET=1 ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) fail "unknown argument '$arg'" "scripts/stop.sh [--keep-accounts] [--quiet]" ;;
  esac
done
export QUIET

stop_service web 12
stop_service calc-api 15
stop_service mock-ting 5

if [ -f "$RUN_DIR/accounts-stack.by-dev" ]; then
  if [ "$KEEP_ACCOUNTS" = 1 ]; then
    [ "$QUIET" = 1 ] || say "leaving the Silicon Accounts stack running (--keep-accounts)"
  else
    "$ROOT/scripts/accounts-stack.sh" stop
    rm -f "$RUN_DIR/accounts-stack.by-dev"
  fi
else
  [ "$QUIET" = 1 ] || say "the Silicon Accounts stack was not started by scripts/dev.sh; leaving it running (scripts/accounts-stack.sh stop stops it)"
fi

if [ -f "$RUN_DIR/postgres.by-dev" ]; then
  if [ "$KEEP_ACCOUNTS" = 1 ] || { "$ROOT/scripts/accounts-stack.sh" status >/dev/null 2>&1; }; then
    [ "$QUIET" = 1 ] || say "leaving Calc's Postgres running (the Silicon Accounts stack still uses it)"
  else
    "$PG_BIN/pg_ctl" -D "$CALC_PGDATA" -m fast -w stop >/dev/null && rm -f "$RUN_DIR/postgres.by-dev"
    [ "$QUIET" = 1 ] || say "stopped Calc's Postgres"
  fi
fi
[ "$QUIET" = 1 ] || say "done"
