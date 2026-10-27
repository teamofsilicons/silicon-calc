#!/usr/bin/env bash
# Silicon Calc for local work, one command, everything detached:
#
#   Calc's Postgres            127.0.0.1:5482 (.dev/pg, user postgres, trust)
#   Silicon Accounts for Calc  http://localhost:9990 (its API 127.0.0.1:9989, mock email 9992 …): scripts/accounts-stack.sh
#   mock Silicon Ting          http://127.0.0.1:8735 (what Calc sent: /tings)
#   calc-api                   http://127.0.0.1:8730 (built into $CARGO_TARGET_DIR, default target/e2e)
#   the web app                http://127.0.0.1:8731 (when web/package.json exists; web/.env.local from e2e/stack.json)
#
# and prints the URLs and the test accounts (a Carbon with a Silicon, and a second Carbon: made once, kept in
# .dev/accounts.json), with the calc command line signed in as the Silicon (CALC_HOME=.dev/calc-home).
#
#   scripts/dev.sh              start what isn't running; calc-api is built and started again every time
#   scripts/dev.sh --restart    also start the mock Ting and the web app again
#   scripts/dev.sh --no-build   run the calc-api and calc already built (the newest of target/e2e, target/api and
#                               target/cli; CALC_API_BIN and CALC_BIN name others)
#   scripts/dev.sh --no-web     don't start the web app
#   scripts/dev.sh --fresh-db   start calc-api on an empty database (drops silicon_calc and .dev/blobs first)
#   scripts/dev.sh --release    build and run calc-api in release mode (a faster engine; a much longer first build)
#
# When a build fails, the newest calc-api (or calc) already built is used instead, and dev.sh says so.
# Logs: .dev/logs/<name>.log. Pid files: .dev/run/<name>.pid. scripts/stop.sh stops exactly what this started.
#
# Environment: CARGO_TARGET_DIR [target/e2e], CALC_API_BIN, CALC_BIN, CALC_LOG [info,sqlx=warn], PG_BIN,
# SILICON_ACCOUNTS_DIR.
set -euo pipefail

SCRIPT=dev
# shellcheck source=lib/common.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"

BUILD=1
RESTART=0
WEB=1
FRESH_DB=0
PROFILE=debug
for arg in "$@"; do
  case "$arg" in
    --no-build) BUILD=0 ;;
    --restart) RESTART=1 ;;
    --no-web) WEB=0 ;;
    --fresh-db) FRESH_DB=1 ;;
    --release) PROFILE=release ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) fail "unknown argument '$arg'" "scripts/dev.sh [--restart] [--no-build] [--no-web] [--fresh-db] [--release]" ;;
  esac
done

CALC_PORT=8730
WEB_PORT=8731
TING_PORT=8735
DB_NAME=silicon_calc
TARGET_DIR="${CARGO_TARGET_DIR:-target/e2e}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$ROOT/$TARGET_DIR" ;; esac
mkdir -p "$LOG_DIR" "$RUN_DIR" "$CALC_DEV/blobs"

# --- 1. Postgres and the Silicon Accounts stack (both kept when they already run) -----------------------------
if ! pg_ready; then
  ensure_postgres
  printf '%s\n' "dev" >"$RUN_DIR/postgres.by-dev"
fi
if ! "$ROOT/scripts/accounts-stack.sh" status >/dev/null 2>&1; then
  "$ROOT/scripts/accounts-stack.sh" start
  printf '%s\n' "dev" >"$RUN_DIR/accounts-stack.by-dev"
else
  "$ROOT/scripts/accounts-stack.sh" seed
fi

PUBLIC_URL="$(json_get "$STACK_JSON" accounts.public_url)"
API_URL="$(json_get "$STACK_JSON" accounts.api_url)"
CALC_SECRET="$(json_get "$STACK_JSON" apps.calc.app_secret)"
MESSAGING_URL="$(json_get "$STACK_JSON" accounts.mock_messaging_url)"
IRIS_URL="$(json_get "$STACK_JSON" accounts.mock_iris_url)"

# --- 2. build (or pick what is built) ----------------------------------------------------------------------------
API_BUILT=""
CLI_BUILT=""
if [ "$BUILD" = 1 ] && [ -z "${CALC_API_BIN:-}" ]; then
  gb="$(free_gb)"
  [ "${gb:-0}" -ge 4 ] || warn "only ${gb} GB are free on /; the build may run out of space"
  cargo_env
  release_flag=()
  [ "$PROFILE" = release ] && release_flag=(--release)
  say "building calc-api ($PROFILE) into $(rel "$TARGET_DIR") (log: $(rel "$LOG_DIR")/build.log)"
  if (cd "$ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build ${release_flag[@]+"${release_flag[@]}"} -p calc-api --bin calc-api) >"$LOG_DIR/build.log" 2>&1; then
    API_BUILT="$TARGET_DIR/$PROFILE/calc-api"
  else
    grep -E '^(error|warning: unused)' -A 3 "$LOG_DIR/build.log" | head -n 20 >&2 || true
    warn "calc-api did not build (log: $(rel "$LOG_DIR")/build.log); using the newest calc-api already built"
  fi
  if [ -z "${CALC_BIN:-}" ]; then
    say "building the calc command line (log: $(rel "$LOG_DIR")/build-cli.log)"
    if (cd "$ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build ${release_flag[@]+"${release_flag[@]}"} -p calc-cli --bin calc) >"$LOG_DIR/build-cli.log" 2>&1; then
      CLI_BUILT="$TARGET_DIR/$PROFILE/calc"
    else
      warn "the calc command line did not build (log: $(rel "$LOG_DIR")/build-cli.log); using the newest calc already built, if any"
    fi
  fi
fi
# shellcheck disable=SC2046
API_BIN="${CALC_API_BIN:-${API_BUILT:-$(newest_bin $(calc_api_candidates "$PROFILE" "$TARGET_DIR"))}}"
# shellcheck disable=SC2046
CLI_BIN="${CALC_BIN:-${CLI_BUILT:-$(newest_bin $(calc_cli_candidates "$PROFILE" "$TARGET_DIR"))}}"
[ -n "$API_BIN" ] && [ -x "$API_BIN" ] || fail "no calc-api is built (looked in target/e2e, target/api)" \
  "build it: CARGO_TARGET_DIR=target/e2e cargo build -p calc-api, or set CALC_API_BIN"
say "calc-api: $(rel "$API_BIN") (built $(built_at "$API_BIN"))"

# --- 3. the mock Ting ------------------------------------------------------------------------------------------
[ "$RESTART" = 1 ] && stop_service mock-ting 5
if service_running mock-ting; then
  say "the mock Ting already runs on http://127.0.0.1:$TING_PORT"
else
  stop_service mock-ting 5
  require_free_port "$TING_PORT" "the mock Ting"
  say "starting the mock Ting on http://127.0.0.1:$TING_PORT"
  launch mock-ting "mock-ting/server.ts" node "$ROOT/e2e/mock-ting/server.ts" --port "$TING_PORT" --stack "$STACK_JSON" --log
  wait_http mock-ting "http://127.0.0.1:$TING_PORT/healthz" "$LAST_PID" 20
fi

# --- 4. calc-api ---------------------------------------------------------------------------------------------
stop_service calc-api 15 INT
require_free_port "$CALC_PORT" "calc-api"
if [ "$FRESH_DB" = 1 ]; then
  say "starting from an empty database: dropping $DB_NAME and $(rel "$CALC_DEV")/blobs"
  PGOPTIONS='--client-min-messages=warning' "$PG_BIN/dropdb" -h 127.0.0.1 -p "$CALC_PGPORT" -U postgres --if-exists --force "$DB_NAME"
  rm -rf "$CALC_DEV/blobs"
  mkdir -p "$CALC_DEV/blobs"
fi
ensure_database "$DB_NAME"
runner_python=()
if [ -x "$CALC_DEV/runner-venv/bin/python" ]; then runner_python=(CALC_RUNNER_PYTHON="$CALC_DEV/runner-venv/bin/python"); fi
say "starting calc-api on http://127.0.0.1:$CALC_PORT (log: $(rel "$LOG_DIR")/calc-api.log)"
launch calc-api "$API_BIN" env \
  CALC_BIND="127.0.0.1:$CALC_PORT" \
  CALC_DATABASE_URL="postgres://postgres@127.0.0.1:$CALC_PGPORT/$DB_NAME" \
  CALC_ACCOUNTS_URL="$PUBLIC_URL" \
  CALC_ACCOUNTS_API_URL="$API_URL" \
  CALC_APP_ID=calc \
  CALC_APP_SECRET="$CALC_SECRET" \
  CALC_TING_URL="http://127.0.0.1:$TING_PORT" \
  CALC_TING_APP_ID=ting \
  CALC_WEB_URL="http://127.0.0.1:$WEB_PORT" \
  CALC_BLOB_DIR="$CALC_DEV/blobs" \
  CALC_LOG="${CALC_LOG:-info,sqlx=warn}" \
  ${runner_python[@]+"${runner_python[@]}"} \
  "$API_BIN"
api_pid="$LAST_PID"
deadline=$((SECONDS + 90))
until curl -fsS -o /dev/null --max-time 3 "http://127.0.0.1:$CALC_PORT/healthz" 2>/dev/null; do
  if ! alive "$api_pid"; then
    tail -n 30 "$LOG_DIR/calc-api.log" >&2 || true
    if grep -qE 'previously applied but (is missing|has been modified)' "$LOG_DIR/calc-api.log" 2>/dev/null; then
      fail "calc-api refused the database $DB_NAME: its migrations differ from the ones this calc-api was built with" \
        "scripts/dev.sh --fresh-db starts on an empty database (the dev sheets are lost), or run a newer calc-api"
    fi
    fail "calc-api exited during start-up (log: $(rel "$LOG_DIR")/calc-api.log)"
  fi
  [ "$SECONDS" -lt "$deadline" ] || fail "calc-api did not answer within 90 s (log: $(rel "$LOG_DIR")/calc-api.log)"
  sleep 0.3
done

# --- 5. the web app --------------------------------------------------------------------------------------------
WEB_STATE="not started (--no-web)"
if [ "$WEB" = 1 ] && [ -f "$ROOT/web/package.json" ]; then
  [ "$RESTART" = 1 ] && stop_service web 12
  # web/.env.local: written here when it is missing or was written here before; a hand-made one is left alone.
  ENV_LOCAL="$ROOT/web/.env.local"
  if [ ! -f "$ENV_LOCAL" ] || grep -q '^# Written by scripts/dev.sh' "$ENV_LOCAL"; then
    secret_file="$CALC_DEV/web-session-secret"
    [ -s "$secret_file" ] || { openssl rand -base64 48 | tr -d '\n' >"$secret_file"; chmod 600 "$secret_file"; }
    node "$ROOT/e2e/lib/write-env-local.ts" --out "$ENV_LOCAL" --stack "$STACK_JSON" --session-secret-file "$secret_file" \
      --public-url "http://127.0.0.1:$WEB_PORT" --api-url "http://127.0.0.1:$CALC_PORT" --ting-url "http://127.0.0.1:$TING_PORT"
  else
    warn "web/.env.local was not written by scripts/dev.sh, so it is left as it is (it must point at calc-api on $CALC_PORT and the stack in e2e/stack.json)"
  fi
  if service_running web; then
    WEB_STATE="http://127.0.0.1:$WEB_PORT   (already running; --restart starts it again)"
  else
    stop_service web 12
    require_free_port "$WEB_PORT" "the web app"
    if [ ! -e "$ROOT/web/node_modules/.bin/next" ]; then
      say "installing the web app's dependencies (log: $(rel "$LOG_DIR")/web-install.log)"
      pnpm -C "$ROOT/web" install --frozen-lockfile >"$LOG_DIR/web-install.log" 2>&1 \
        || { tail -n 20 "$LOG_DIR/web-install.log" >&2; fail "pnpm install in web/ failed (log: $(rel "$LOG_DIR")/web-install.log)"; }
    fi
    say "starting the web app (next dev) on http://127.0.0.1:$WEB_PORT (log: $(rel "$LOG_DIR")/web.log)"
    # Its own build directory, so a next dev of the web app's own tests (.next, .next-e2e) is never disturbed.
    launch web "next dev --port $WEB_PORT" env -C "$ROOT/web" PORT="$WEB_PORT" NEXT_DIST_DIR=.next-dev NEXT_TELEMETRY_DISABLED=1 \
      "$ROOT/web/node_modules/.bin/next" dev --port "$WEB_PORT" --hostname 127.0.0.1
    web_pid="$LAST_PID"
    deadline=$((SECONDS + 180))
    code=""
    while :; do
      code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 60 "http://127.0.0.1:$WEB_PORT/" 2>/dev/null || true)"
      case "$code" in ''|000) ;; *) break ;; esac
      alive "$web_pid" || { tail -n 30 "$LOG_DIR/web.log" >&2; fail "the web app exited during start-up (log: $(rel "$LOG_DIR")/web.log)"; }
      [ "$SECONDS" -lt "$deadline" ] || fail "the web app did not answer within 180 s (log: $(rel "$LOG_DIR")/web.log)"
      sleep 0.5
    done
    case "$code" in
      5*)
        warn "the web app answers / with HTTP $code; see $(rel "$LOG_DIR")/web.log"
        WEB_STATE="http://127.0.0.1:$WEB_PORT   (answers / with HTTP $code: see .dev/logs/web.log)"
        ;;
      *) WEB_STATE="http://127.0.0.1:$WEB_PORT" ;;
    esac
  fi
elif [ "$WEB" = 1 ]; then
  WEB_STATE="not started (web/package.json does not exist yet)"
fi

# --- 6. test accounts, and the calc command line signed in as the Silicon -------------------------------------------
ACCOUNTS_JSON="$RUN_DIR/accounts-view.json"
node "$ROOT/e2e/bin/accounts.ts" dev-accounts --file "$CALC_DEV/accounts.json" >"$ACCOUNTS_JSON" 2>"$LOG_DIR/accounts.log" \
  || { cat "$LOG_DIR/accounts.log" >&2; fail "the test accounts could not be made (log: $(rel "$LOG_DIR")/accounts.log)"; }
chmod 600 "$ACCOUNTS_JSON"
get() { json_get "$ACCOUNTS_JSON" "$1"; }
CALC_HOME_DIR="$CALC_DEV/calc-home"
CLI_STATE="not built yet"
if [ -n "$CLI_BIN" ] && [ -x "$CLI_BIN" ]; then
  mkdir -p "$CALC_HOME_DIR"
  chmod 700 "$CALC_HOME_DIR"
  if printf '%s' "$(get silicon.stk)" | env CALC_HOME="$CALC_HOME_DIR" CALC_API_URL="http://127.0.0.1:$CALC_PORT" CALC_ACCOUNTS_URL="$PUBLIC_URL" \
      "$CLI_BIN" --quiet login --silicon "$(get silicon.id)" --stk-stdin --force >"$LOG_DIR/calc-login.log" 2>&1; then
    CLI_STATE="$(rel "$CLI_BIN") (built $(built_at "$CLI_BIN")), signed in as $(get silicon.id)"
  else
    CLI_STATE="$(rel "$CLI_BIN") (built $(built_at "$CLI_BIN")); signing in as $(get silicon.id) failed: see .dev/logs/calc-login.log"
  fi
fi

cat <<EOF

Silicon Calc dev stack is up
  web app              $WEB_STATE
  calc-api             http://127.0.0.1:$CALC_PORT   ($(rel "$API_BIN"), log .dev/logs/calc-api.log)
  mock Silicon Ting    http://127.0.0.1:$TING_PORT   (what Calc sent: /tings, /_requests)
  Silicon Accounts     $PUBLIC_URL   (api $API_URL; logs $(json_get "$STACK_JSON" accounts.logs))
  email codes          $MESSAGING_URL/_messages
  profile photos       $IRIS_URL
  database             postgres://postgres@127.0.0.1:$CALC_PGPORT/$DB_NAME
  stack                $(rel "$STACK_JSON") (URLs, the calc and ting apps and their secrets)

Test accounts ($(rel "$CALC_DEV")/accounts.json)
  Carbon    $(get carbon.id)  $(get carbon.display_name)  sign in with $(get carbon.email) (the code: $MESSAGING_URL/_messages?to=$(get carbon.email))
  Carbon    $(get second_carbon.id)  $(get second_carbon.display_name)  $(get second_carbon.email)
  Silicon   $(get silicon.id)  $(get silicon.display_name)  (credential saved in .dev/accounts.json; custodian $(get silicon.custodian))

The calc command line: $CLI_STATE
  export CALC_HOME=$CALC_HOME_DIR CALC_API_URL=http://127.0.0.1:$CALC_PORT CALC_ACCOUNTS_URL=$PUBLIC_URL
  $( [ -n "$CLI_BIN" ] && rel "$CLI_BIN" || echo calc) whoami
  A calc access token for scripts: node e2e/bin/accounts.ts token --silicon $(get silicon.id)

Stop it: scripts/stop.sh
EOF
