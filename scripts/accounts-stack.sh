#!/usr/bin/env bash
# The Silicon Accounts stack Calc signs in with, from the Silicon Accounts checkout next to this one
# (../silicon-accounts), with its database in Calc's own Postgres cluster:
#
#   public site (Next.js, hosted sign-in pages)  http://localhost:9990
#   accounts-api                                 http://127.0.0.1:9989
#   mock Google/Apple                            http://127.0.0.1:9991
#   mock email/SMS (codes: /_messages)           http://127.0.0.1:9992
#   fake apps                                    http://127.0.0.1:9993
#   mock Iris (profile photos)                   http://127.0.0.1:9994
#   stand-in Silicon Apps API                    http://127.0.0.1:9996
#   database                                     postgres://postgres@127.0.0.1:5482/silicon_accounts_calc
#
# and Calc's two apps on it (e2e/stack/apps.json): `calc` (the web app, the service and the calc command line:
# email and mock Google/Apple sign-in, the device flow and the public client for the CLI, email as an optional
# detail) and `ting` (the receiving app of Calc's App verification proof, which the mock Ting verifies with).
#
#   scripts/accounts-stack.sh start    start the stack (or keep the one already up), seed calc and ting, check
#                                      that Calc's proof for Ting verifies, write e2e/stack.json
#   scripts/accounts-stack.sh seed     seed calc and ting again and rewrite e2e/stack.json
#   scripts/accounts-stack.sh status   say whether it is up
#   scripts/accounts-stack.sh stop     stop it (Postgres keeps running)
#
# It runs the checkout's own scripts/dev.sh with its already-built binaries (target/integration, --no-build),
# detached, without the developer platform. Its logs and pid files live in that checkout's .dev/logs/9990 and
# .dev/run/9990 (the way it keeps every stack); nothing else there changes. Never touches its other stacks or
# its own Postgres cluster (5444).
#
# Environment: SILICON_ACCOUNTS_DIR [../silicon-accounts], ACCOUNTS_TARGET_DIR [target/integration],
# PG_BIN [/opt/homebrew/opt/postgresql@16/bin], STACK_JSON [e2e/stack.json].
set -euo pipefail

SCRIPT=accounts-stack
# shellcheck source=lib/common.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"

CMD="${1:-start}"

ACCOUNTS_PORT=9990
ACCOUNTS_API_PORT=9989
MOCK_OIDC_PORT=9991
MOCK_MESSAGING_PORT=9992
FAKE_APPS_PORT=9993
MOCK_IRIS_PORT=9994
DEVELOPER_PORT=9995
APPS_API_PORT=9996
ACCOUNTS_DB_NAME=silicon_accounts_calc

A="$SILICON_ACCOUNTS_DIR"
A_TARGET="${ACCOUNTS_TARGET_DIR:-target/integration}"
case "$A_TARGET" in /*) A_BIN="$A_TARGET/debug" ;; *) A_BIN="$A/$A_TARGET/debug" ;; esac
A_RUN="$A/.dev/run/$ACCOUNTS_PORT"
A_LOG="$A/.dev/logs/$ACCOUNTS_PORT"
APPS_JSON="$ROOT/e2e/stack/apps.json"
PUBLIC_URL="http://localhost:$ACCOUNTS_PORT"
API_URL="http://127.0.0.1:$ACCOUNTS_API_PORT"

accounts_env() {
  # Everything the checkout's dev.sh reads for a second stack. ACCOUNTS_PGDATA points its dev-db.sh at Calc's
  # cluster, so it can only ever start (or create a database in) the cluster on 5482.
  env ACCOUNTS_PORT="$ACCOUNTS_PORT" ACCOUNTS_API_PORT="$ACCOUNTS_API_PORT" MOCK_OIDC_PORT="$MOCK_OIDC_PORT" \
    MOCK_MESSAGING_PORT="$MOCK_MESSAGING_PORT" FAKE_APPS_PORT="$FAKE_APPS_PORT" MOCK_IRIS_PORT="$MOCK_IRIS_PORT" \
    DEVELOPER_PORT="$DEVELOPER_PORT" APPS_API_PORT="$APPS_API_PORT" \
    ACCOUNTS_PGPORT="$CALC_PGPORT" ACCOUNTS_DB_NAME="$ACCOUNTS_DB_NAME" ACCOUNTS_PGDATA="$CALC_PGDATA" \
    PG_BIN="$PG_BIN" CARGO_TARGET_DIR="$A_TARGET" "$@"
}

is_up() {
  curl -fsS -o /dev/null --max-time 3 "$API_URL/readyz" 2>/dev/null \
    && curl -fsS -o /dev/null --max-time 20 "$PUBLIC_URL/v1/meta" 2>/dev/null \
    && curl -fsS -o /dev/null --max-time 3 "http://127.0.0.1:$MOCK_MESSAGING_PORT/_messages?limit=1" 2>/dev/null \
    && [ -s "$A_RUN/testkit.json" ]
}

check_checkout() {
  [ -d "$A" ] || fail "the Silicon Accounts checkout is not at $A" "set SILICON_ACCOUNTS_DIR to it"
  [ -x "$A/scripts/dev.sh" ] || fail "$A/scripts/dev.sh is missing" "is $A a Silicon Accounts checkout?"
  local b
  for b in accounts-api accounts-migrate accounts-seed silicon-accounts; do
    [ -x "$A_BIN/$b" ] || fail "$A_BIN/$b is not built" \
      "build it once: (cd $A && CARGO_TARGET_DIR=$A_TARGET cargo build -p silicon-accounts-server -p silicon-accounts-cli)"
  done
  [ -x "$A/testkit/node_modules/.bin/tsx" ] || fail "the Accounts testkit's dependencies are not installed" \
    "run: pnpm -C $A/testkit install --frozen-lockfile"
}

seed() {
  local env_file="$A_RUN/accounts-api.env" stamp sum force=()
  [ -f "$env_file" ] || fail "the stack's environment ($env_file) is missing" "start the stack first: scripts/accounts-stack.sh start"
  mkdir -p "$RUN_DIR"
  stamp="$RUN_DIR/accounts-seed.sha256"
  sum="$(cat "$APPS_JSON" "$env_file" | shasum -a 256 | cut -d' ' -f1)"
  # signin_defaults apply when an app is created; --force applies them again after apps.json changed.
  if [ ! -f "$stamp" ] || [ "$(cat "$stamp")" != "$sum" ]; then force=(--force); fi
  say "seeding calc and ting from $(rel "$APPS_JSON")${force[0]+ (applying their sign-in setups)}"
  mkdir -p "$LOG_DIR"
  bash -c 'set -a; . "$1"; set +a; shift; exec "$@"' seed "$env_file" "$A_BIN/accounts-seed" --fake-apps "$APPS_JSON" ${force[@]+"${force[@]}"} \
    >"$LOG_DIR/accounts-seed.log" 2>&1 || { cat "$LOG_DIR/accounts-seed.log" >&2; fail "accounts-seed failed (log: $(rel "$LOG_DIR")/accounts-seed.log)"; }
  printf '%s\n' "$sum" >"$stamp"
  write_stack_json
}

write_stack_json() {
  # Writes e2e/stack.json and checks the apps work: calc gets an App verification proof for ting, ting verifies it.
  node "$ROOT/e2e/lib/write-stack.ts" \
    --out "$STACK_JSON" --apps "$APPS_JSON" --accounts-dir "$A" --accounts-bin "$A_BIN" --accounts-logs "$A_LOG" \
    --public-url "$PUBLIC_URL" --api-url "$API_URL" --oidc-port "$MOCK_OIDC_PORT" --messaging-port "$MOCK_MESSAGING_PORT" \
    --fake-apps-port "$FAKE_APPS_PORT" --iris-port "$MOCK_IRIS_PORT" --apps-api-port "$APPS_API_PORT" \
    --database-url "postgres://postgres@127.0.0.1:$CALC_PGPORT/$ACCOUNTS_DB_NAME" \
    || fail "the stack is up, but Calc's apps on it don't work (see above)" "scripts/accounts-stack.sh seed applies e2e/stack/apps.json again"
}

case "$CMD" in
  start)
    check_checkout
    mkdir -p "$RUN_DIR" "$LOG_DIR"
    ensure_postgres
    if is_up; then
      say "the Silicon Accounts stack is already up on $PUBLIC_URL (api $API_URL)"
    else
      for p in "$ACCOUNTS_PORT" "$ACCOUNTS_API_PORT" "$MOCK_OIDC_PORT" "$MOCK_MESSAGING_PORT" "$FAKE_APPS_PORT" "$MOCK_IRIS_PORT" "$APPS_API_PORT"; do
        # Ports held by this stack's own earlier processes are freed by its dev.sh (it stops what it started).
        if port_busy "$p" && ! ls "$A_RUN"/*.pid >/dev/null 2>&1; then
          fail "port $p is in use by $(who_listens "$p" || echo 'another process'), not by Calc's Accounts stack" "free it, then run this again"
        fi
      done
      say "starting the Silicon Accounts stack for Calc (log: $(rel "$LOG_DIR")/accounts-stack.log; the stack's own logs: $A_LOG)"
      if ! (cd "$A" && accounts_env "$A/scripts/dev.sh" --no-build --developer=off --detach) >"$LOG_DIR/accounts-stack.log" 2>&1; then
        tail -n 40 "$LOG_DIR/accounts-stack.log" >&2
        fail "the Silicon Accounts stack did not start (log: $(rel "$LOG_DIR")/accounts-stack.log)"
      fi
      is_up || fail "the Silicon Accounts stack started but does not answer on $PUBLIC_URL and $API_URL" "see $LOG_DIR/accounts-stack.log and $A_LOG"
      say "the Silicon Accounts stack is up on $PUBLIC_URL (api $API_URL)"
    fi
    seed
    ;;
  seed)
    is_up || fail "the Silicon Accounts stack is not up" "scripts/accounts-stack.sh start"
    seed
    ;;
  status)
    if is_up; then
      echo "up: $PUBLIC_URL (api $API_URL, mock email $(printf 'http://127.0.0.1:%s/_messages' "$MOCK_MESSAGING_PORT"))"
    else
      echo "down"
      exit 1
    fi
    ;;
  stop)
    if [ -d "$A_RUN" ] && ls "$A_RUN"/*.pid >/dev/null 2>&1; then
      say "stopping the Silicon Accounts stack on $PUBLIC_URL"
      # Never --db: the checkout's dev-db.sh would act on its own cluster.
      (cd "$A" && ACCOUNTS_PORT="$ACCOUNTS_PORT" "$A/scripts/stop.sh" --quiet)
      say "stopped"
    else
      say "the Silicon Accounts stack on port $ACCOUNTS_PORT is not running"
    fi
    ;;
  -h|--help|help)
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
    ;;
  *)
    fail "unknown command '$CMD'" "use start, seed, status or stop"
    ;;
esac
