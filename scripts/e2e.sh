#!/usr/bin/env bash
# Silicon Calc end to end, as real Carbons and Silicons on the local Silicon Accounts stack:
#
#   1. calc-ting against the stack and the mock Ting      (cargo test -p calc-ting --test stack -- --ignored)
#   2. the journeys (e2e/journeys): the real service over HTTP and the real calc command line
#   3. the web app's browser specs (web/e2e), when the web app has Calc's
#
# It starts what it needs on its own ports, so scripts/dev.sh keeps running undisturbed: the Silicon Accounts stack
# (shared; started when it isn't up, and left running), a mock Ting on 8782, calc-api on 8780 with a fresh database
# (calc_e2e_<time>) and its own blob directory, and the web app on 8781 for the browser specs. At the end it stops
# them, drops the database and prints a pass/fail summary; the exit code is 1 when anything failed.
#
#   scripts/e2e.sh                        everything
#   scripts/e2e.sh 01-carbon-sheet 05-versions     only these journeys (names as in e2e/journeys, without .ts)
#   scripts/e2e.sh --no-build             use the calc-api and calc already built
#   scripts/e2e.sh --skip-ting --skip-web --skip-journeys
#   scripts/e2e.sh --keep                 leave the e2e calc-api, mock Ting and database running afterwards
#   scripts/e2e.sh --release              build and run calc-api and calc in release mode
#
# Logs: .dev/logs/e2e/. Results: .dev/e2e/results.json.
# Environment: CARGO_TARGET_DIR [target/e2e], E2E_TIMEOUT_SECONDS [1800 per journey run].
set -euo pipefail

SCRIPT=e2e
# shellcheck source=lib/common.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"
LOG_DIR="$CALC_DEV/logs/e2e"
RUN_DIR="$CALC_DEV/run/e2e"
OUT_DIR="$CALC_DEV/e2e"
mkdir -p "$LOG_DIR" "$RUN_DIR" "$OUT_DIR"

BUILD=1
KEEP=0
PROFILE=debug
DO_TING=1
DO_JOURNEYS=1
DO_WEB=1
JOURNEYS=()
for arg in "$@"; do
  case "$arg" in
    --no-build) BUILD=0 ;;
    --keep) KEEP=1 ;;
    --release) PROFILE=release ;;
    --skip-ting) DO_TING=0 ;;
    --skip-journeys) DO_JOURNEYS=0 ;;
    --skip-web) DO_WEB=0 ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    -*) fail "unknown option '$arg'" "see scripts/e2e.sh --help" ;;
    *) JOURNEYS+=("$arg") ;;
  esac
done

CALC_PORT=8780
WEB_PORT=8781
TING_PORT=8782
TARGET_DIR="${CARGO_TARGET_DIR:-target/e2e}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$ROOT/$TARGET_DIR" ;; esac
BIN="$TARGET_DIR/$PROFILE"
STAMP="$(date +%Y%m%d%H%M%S)"
DB="calc_e2e_$STAMP"
BLOBS="$OUT_DIR/blobs-$STAMP"
RESULTS="$OUT_DIR/results.json"
SUMMARY=()
FAILED=0

record() { # name, status (pass|fail|skip), detail
  SUMMARY+=("$(printf '%-6s %-34s %s' "$2" "$1" "$3")")
  [ "$2" = fail ] && FAILED=1
  return 0
}

cleanup() {
  if [ "$KEEP" = 1 ]; then
    say "kept: calc-api http://127.0.0.1:$CALC_PORT (database $DB), mock Ting http://127.0.0.1:$TING_PORT; stop with: RUN_DIR=$RUN_DIR scripts/e2e.sh --help shows how (or kill the pids in $(rel "$RUN_DIR"))"
    return
  fi
  QUIET=1 stop_service web 12 || true
  QUIET=1 stop_service calc-api 15 || true
  QUIET=1 stop_service mock-ting 5 || true
  if [ -n "${DB_MADE:-}" ]; then drop_database "$DB" || warn "could not drop $DB"; fi
  rm -rf "$BLOBS"
}
trap cleanup EXIT

# --- the stack ------------------------------------------------------------------------------------------------
ensure_postgres
"$ROOT/scripts/accounts-stack.sh" start
PUBLIC_URL="$(json_get "$STACK_JSON" accounts.public_url)"
API_URL="$(json_get "$STACK_JSON" accounts.api_url)"
CALC_SECRET="$(json_get "$STACK_JSON" apps.calc.app_secret)"

# --- build ----------------------------------------------------------------------------------------------------
cargo_env
release_flag=()
[ "$PROFILE" = release ] && release_flag=(--release)
if [ "$BUILD" = 1 ]; then
  gb="$(free_gb)"
  [ "${gb:-0}" -ge 4 ] || warn "only ${gb} GB are free on /; the build may run out of space"
  say "building calc-api and calc ($PROFILE) into $(rel "$TARGET_DIR") (log: $(rel "$LOG_DIR")/build.log)"
  if ! (cd "$ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build ${release_flag[@]+"${release_flag[@]}"} -p calc-api --bin calc-api) >"$LOG_DIR/build.log" 2>&1; then
    tail -n 30 "$LOG_DIR/build.log" >&2
    record build fail "calc-api did not build (log: $(rel "$LOG_DIR")/build.log)"
  fi
  if ! (cd "$ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build ${release_flag[@]+"${release_flag[@]}"} -p calc-cli) >"$LOG_DIR/build-cli.log" 2>&1; then
    tail -n 30 "$LOG_DIR/build-cli.log" >&2
    record build-cli fail "calc did not build (log: $(rel "$LOG_DIR")/build-cli.log)"
  fi
fi

# --- 1. calc-ting against the stack -----------------------------------------------------------------------------
if [ "$DO_TING" = 1 ]; then
  say "calc-ting against the stack and the mock Ting"
  if (cd "$ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" STACK_JSON="$STACK_JSON" perl -e 'alarm shift; exec @ARGV' 900 cargo test -p calc-ting --test stack -- --ignored) >"$LOG_DIR/calc-ting.log" 2>&1; then
    record calc-ting pass "$(grep -E '^test result' "$LOG_DIR/calc-ting.log" | tail -1)"
  else
    tail -n 30 "$LOG_DIR/calc-ting.log" >&2
    record calc-ting fail "see $(rel "$LOG_DIR")/calc-ting.log"
  fi
else
  record calc-ting skip "--skip-ting"
fi

# --- the e2e services -------------------------------------------------------------------------------------------
services_up=0
if [ -x "$BIN/calc-api" ] && { [ "$DO_JOURNEYS" = 1 ] || [ "$DO_WEB" = 1 ]; }; then
  for p in "$CALC_PORT" "$TING_PORT"; do require_free_port "$p" "the e2e services"; done
  launch mock-ting "mock-ting/server.ts" node "$ROOT/e2e/mock-ting/server.ts" --port "$TING_PORT" --stack "$STACK_JSON" --log
  wait_http mock-ting "http://127.0.0.1:$TING_PORT/healthz" "$LAST_PID" 20
  ensure_database "$DB"
  DB_MADE=1
  mkdir -p "$BLOBS"
  runner_python=()
  if [ -x "$CALC_DEV/runner-venv/bin/python" ]; then runner_python=(CALC_RUNNER_PYTHON="$CALC_DEV/runner-venv/bin/python"); fi
  launch calc-api "$BIN/calc-api" env \
    CALC_BIND="127.0.0.1:$CALC_PORT" \
    CALC_DATABASE_URL="postgres://postgres@127.0.0.1:$CALC_PGPORT/$DB" \
    CALC_ACCOUNTS_URL="$PUBLIC_URL" CALC_ACCOUNTS_API_URL="$API_URL" \
    CALC_APP_ID=calc CALC_APP_SECRET="$CALC_SECRET" \
    CALC_TING_URL="http://127.0.0.1:$TING_PORT" CALC_TING_APP_ID=ting \
    CALC_WEB_URL="http://127.0.0.1:$WEB_PORT" CALC_BLOB_DIR="$BLOBS" \
    CALC_LOG="${CALC_LOG:-info,sqlx=warn}" \
    ${runner_python[@]+"${runner_python[@]}"} \
    "$BIN/calc-api"
  if wait_http calc-api "http://127.0.0.1:$CALC_PORT/healthz" "$LAST_PID" 90; then services_up=1; fi
fi

# --- 2. the journeys --------------------------------------------------------------------------------------------
if [ "$DO_JOURNEYS" = 1 ]; then
  if [ "$services_up" = 1 ]; then
    say "the journeys (log: $(rel "$LOG_DIR")/journeys.log)"
    status=0
    CALC_API_URL="http://127.0.0.1:$CALC_PORT" CALC_WEB_URL="http://127.0.0.1:$WEB_PORT" MOCK_TING_URL="http://127.0.0.1:$TING_PORT" \
      CALC_CLI="$BIN/calc" STACK_JSON="$STACK_JSON" JOURNEY_RESULTS="$RESULTS" \
      perl -e 'alarm shift; exec @ARGV' "${E2E_TIMEOUT_SECONDS:-1800}" node "$ROOT/e2e/journeys/run.ts" ${JOURNEYS[@]+"${JOURNEYS[@]}"} 2>&1 | tee "$LOG_DIR/journeys.log" || status=$?
    if [ -f "$RESULTS" ]; then
      while IFS=$'\t' read -r name st detail; do record "$name" "$st" "$detail"; done < <(node -e '
        const r = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
        for (const j of r.journeys) console.log([`journey ${j.name}`, j.failed ? "fail" : (j.passed ? "pass" : "skip"), `${j.passed} passed, ${j.failed} failed, ${j.skipped} skipped${j.error ? " — " + j.error.split("\n")[0].slice(0, 160) : ""}`].join("\t"));
      ' "$RESULTS")
    else
      record journeys fail "the journeys did not finish (exit $status; log $(rel "$LOG_DIR")/journeys.log)"
    fi
  else
    record journeys fail "calc-api did not start on $CALC_PORT (log $(rel "$LOG_DIR")/calc-api.log)"
  fi
else
  record journeys skip "--skip-journeys"
fi

# --- 3. the web app's browser specs -------------------------------------------------------------------------------
if [ "$DO_WEB" = 1 ]; then
  if [ -f "$ROOT/web/playwright.config.ts" ] && grep -q '"test:e2e"' "$ROOT/web/package.json" && grep -qi 'calc' "$ROOT/web/playwright.config.ts"; then
    if [ "$services_up" = 1 ]; then
      say "the web app's browser specs (log: $(rel "$LOG_DIR")/web-e2e.log)"
      secret_file="$CALC_DEV/web-session-secret"
      [ -s "$secret_file" ] || { openssl rand -base64 48 | tr -d '\n' >"$secret_file"; chmod 600 "$secret_file"; }
      if (cd "$ROOT/web" && env TEST_STACK_JSON="$STACK_JSON" APP_ID=calc APP_SECRET="$CALC_SECRET" ACCOUNTS_URL="$PUBLIC_URL" ACCOUNTS_API_URL="$API_URL" \
          APP_API_URL="http://127.0.0.1:$CALC_PORT" CALC_API_URL="http://127.0.0.1:$CALC_PORT" E2E_PORT="$WEB_PORT" PUBLIC_URL="http://127.0.0.1:$WEB_PORT" \
          SESSION_SECRET="$(cat "$secret_file")" MOCK_MESSAGING_URL="$(json_get "$STACK_JSON" accounts.mock_messaging_url)" \
          MOCK_TING_URL="http://127.0.0.1:$TING_PORT" EXTRA_IMG_ORIGINS="$(json_get "$STACK_JSON" accounts.mock_iris_url)" NEXT_TELEMETRY_DISABLED=1 \
          perl -e 'alarm shift; exec @ARGV' 1800 pnpm test:e2e) >"$LOG_DIR/web-e2e.log" 2>&1; then
        record web-e2e pass "$(grep -E '[0-9]+ passed' "$LOG_DIR/web-e2e.log" | tail -1 | sed 's/^ *//')"
      else
        tail -n 30 "$LOG_DIR/web-e2e.log" >&2
        record web-e2e fail "$(grep -E '[0-9]+ (failed|passed)' "$LOG_DIR/web-e2e.log" | tail -2 | tr '\n' ' ') (log $(rel "$LOG_DIR")/web-e2e.log)"
      fi
    else
      record web-e2e fail "calc-api did not start, so the browser specs could not run"
    fi
  else
    record web-e2e skip "web/ has no Calc browser specs yet (web/playwright.config.ts and a test:e2e script)"
  fi
else
  record web-e2e skip "--skip-web"
fi

echo
echo "Silicon Calc end to end"
for line in "${SUMMARY[@]}"; do echo "  $line"; done
echo
if [ "$FAILED" = 1 ]; then
  echo "FAILED (logs: $(rel "$LOG_DIR")/, results: $(rel "$RESULTS"))"
  exit 1
fi
echo "PASSED"
