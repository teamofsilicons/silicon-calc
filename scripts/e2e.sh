#!/usr/bin/env bash
# Silicon Calc end to end, as real Carbons and Silicons on the local Silicon Accounts stack:
#
#   1. calc-ting against the stack and the mock Ting      (cargo test -p calc-ting --test stack -- --ignored)
#   2. the journeys (e2e/journeys): the real service over HTTP and the real calc command line
#   3. the web app's browser specs (web/e2e), when the web app has Calc's
#
# It starts what it needs on its own ports, so scripts/dev.sh keeps running undisturbed: Calc's Postgres and the
# Silicon Accounts stack (shared; started when they aren't up, and left running), a mock Ting on 8782, calc-api on
# 8780 with a fresh database (calc_e2e_<time>) and its own blob directory, and the web app on 8781 for the browser
# specs. At the end it stops them, drops the database and prints a pass/fail summary of every step; the exit code
# is 1 when anything failed.
#
#   scripts/e2e.sh                        everything
#   scripts/e2e.sh 01-carbon-sheet 04     only these journeys (names as in e2e/journeys without .ts, or a prefix)
#   scripts/e2e.sh --no-build             use the calc-api and calc already built (the newest of target/e2e,
#                                         target/api and target/cli; CALC_API_BIN and CALC_BIN name others)
#   scripts/e2e.sh --skip-ting --skip-web --skip-journeys
#   scripts/e2e.sh --keep                 leave the e2e calc-api, mock Ting and database running afterwards
#   scripts/e2e.sh --release              build and run calc-api and calc in release mode
#
# When calc-api or calc doesn't build, that is a failure in the summary, and the newest one already built is used
# for the rest, so the journeys still say what works. The 1,000,000-row journey uses E2E_MILLION_ROWS rows.
# Logs: .dev/logs/e2e/. Results: .dev/e2e/results.json (every step, with its error).
# Environment: CARGO_TARGET_DIR [target/e2e], CALC_API_BIN, CALC_BIN, E2E_TIMEOUT_SECONDS [3600 for the journeys].
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
STAMP="$(date +%Y%m%d%H%M%S)"
DB="calc_e2e_$STAMP"
BLOBS="$OUT_DIR/blobs-$STAMP"
RESULTS="$OUT_DIR/results.json"
SUMMARY=()
FAILED=0

record() { # name, status (pass|fail|skip), detail
  SUMMARY+=("$(printf '%-5s %-30s %s' "$2" "$1" "$3")")
  [ "$2" = fail ] && FAILED=1
  return 0
}

cleanup() {
  if [ "$KEEP" = 1 ]; then
    say "kept running: calc-api http://127.0.0.1:$CALC_PORT (database $DB, blobs $(rel "$BLOBS")), mock Ting http://127.0.0.1:$TING_PORT; their pids are in $(rel "$RUN_DIR")"
    return
  fi
  QUIET=1 stop_service web 12 || true
  QUIET=1 stop_service calc-api 15 INT || true
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

# --- build (or pick what is built) ------------------------------------------------------------------------------
cargo_env
release_flag=()
[ "$PROFILE" = release ] && release_flag=(--release)
API_BUILT=""
CLI_BUILT=""
if [ "$BUILD" = 1 ]; then
  gb="$(free_gb)"
  [ "${gb:-0}" -ge 4 ] || warn "only ${gb} GB are free on /; the build may run out of space"
  if [ -z "${CALC_API_BIN:-}" ]; then
    say "building calc-api ($PROFILE) into $(rel "$TARGET_DIR") (log: $(rel "$LOG_DIR")/build.log)"
    if (cd "$ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build ${release_flag[@]+"${release_flag[@]}"} -p calc-api --bin calc-api) >"$LOG_DIR/build.log" 2>&1; then
      API_BUILT="$TARGET_DIR/$PROFILE/calc-api"
      record build-calc-api pass "$(rel "$API_BUILT")"
    else
      grep -E '^error' -A 4 "$LOG_DIR/build.log" | head -n 24 >&2 || true
      record build-calc-api fail "calc-api did not build: $(grep -E '^error' "$LOG_DIR/build.log" | head -1 | cut -c1-140) (log $(rel "$LOG_DIR")/build.log)"
    fi
  fi
  if [ -z "${CALC_BIN:-}" ]; then
    say "building calc ($PROFILE) into $(rel "$TARGET_DIR") (log: $(rel "$LOG_DIR")/build-cli.log)"
    if (cd "$ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build ${release_flag[@]+"${release_flag[@]}"} -p calc-cli --bin calc) >"$LOG_DIR/build-cli.log" 2>&1; then
      CLI_BUILT="$TARGET_DIR/$PROFILE/calc"
      record build-calc pass "$(rel "$CLI_BUILT")"
    else
      grep -E '^error' -A 4 "$LOG_DIR/build-cli.log" | head -n 24 >&2 || true
      record build-calc fail "calc did not build: $(grep -E '^error' "$LOG_DIR/build-cli.log" | head -1 | cut -c1-140) (log $(rel "$LOG_DIR")/build-cli.log)"
    fi
  fi
fi
# shellcheck disable=SC2046
API_BIN="${CALC_API_BIN:-${API_BUILT:-$(newest_bin $(calc_api_candidates "$PROFILE" "$TARGET_DIR"))}}"
# shellcheck disable=SC2046
CLI_BIN="${CALC_BIN:-${CLI_BUILT:-$(newest_bin $(calc_cli_candidates "$PROFILE" "$TARGET_DIR"))}}"
if [ -n "$API_BIN" ]; then
  say "calc-api under test: $(rel "$API_BIN") (built $(built_at "$API_BIN"))"
  [ -n "$API_BUILT" ] || [ -n "${CALC_API_BIN:-}" ] || record calc-api-used skip "the newest calc-api already built: $(rel "$API_BIN"), built $(built_at "$API_BIN")"
fi
if [ -n "$CLI_BIN" ]; then
  say "calc under test: $(rel "$CLI_BIN") (built $(built_at "$CLI_BIN"))"
  [ -n "$CLI_BUILT" ] || [ -n "${CALC_BIN:-}" ] || record calc-used skip "the newest calc already built: $(rel "$CLI_BIN"), built $(built_at "$CLI_BIN")"
else
  CLI_BIN="$TARGET_DIR/$PROFILE/calc"
  record calc-used fail "no calc command line is built anywhere (target/e2e, target/cli): every CLI check fails"
fi

# --- 1. calc-ting against the stack -----------------------------------------------------------------------------
if [ "$DO_TING" = 1 ]; then
  say "calc-ting against the stack and the mock Ting (log: $(rel "$LOG_DIR")/calc-ting.log)"
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
if [ -n "$API_BIN" ] && [ -x "$API_BIN" ] && { [ "$DO_JOURNEYS" = 1 ] || [ "$DO_WEB" = 1 ]; }; then
  for p in "$CALC_PORT" "$TING_PORT"; do require_free_port "$p" "the e2e services"; done
  launch mock-ting "mock-ting/server.ts" node "$ROOT/e2e/mock-ting/server.ts" --port "$TING_PORT" --stack "$STACK_JSON" --log
  wait_http mock-ting "http://127.0.0.1:$TING_PORT/healthz" "$LAST_PID" 20
  ensure_database "$DB"
  DB_MADE=1
  mkdir -p "$BLOBS"
  runner_python=()
  if [ -x "$CALC_DEV/runner-venv/bin/python" ]; then runner_python=(CALC_RUNNER_PYTHON="$CALC_DEV/runner-venv/bin/python"); fi
  launch calc-api "$API_BIN" env \
    CALC_BIND="127.0.0.1:$CALC_PORT" \
    CALC_DATABASE_URL="postgres://postgres@127.0.0.1:$CALC_PGPORT/$DB" \
    CALC_ACCOUNTS_URL="$PUBLIC_URL" CALC_ACCOUNTS_API_URL="$API_URL" \
    CALC_APP_ID=calc CALC_APP_SECRET="$CALC_SECRET" \
    CALC_TING_URL="http://127.0.0.1:$TING_PORT" CALC_TING_APP_ID=ting \
    CALC_WEB_URL="http://127.0.0.1:$WEB_PORT" CALC_BLOB_DIR="$BLOBS" \
    CALC_LOG="${CALC_LOG:-info,sqlx=warn}" \
    ${runner_python[@]+"${runner_python[@]}"} \
    "$API_BIN"
  api_pid="$LAST_PID"
  deadline=$((SECONDS + 90))
  until curl -fsS -o /dev/null --max-time 3 "http://127.0.0.1:$CALC_PORT/healthz" 2>/dev/null; do
    if ! alive "$api_pid"; then
      tail -n 20 "$LOG_DIR/calc-api.log" >&2 || true
      record calc-api fail "calc-api exited during start-up: $(grep -iE 'error' "$LOG_DIR/calc-api.log" | tail -1 | cut -c1-160) (log $(rel "$LOG_DIR")/calc-api.log)"
      break
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      record calc-api fail "calc-api did not answer within 90 s (log $(rel "$LOG_DIR")/calc-api.log)"
      break
    fi
    sleep 0.3
  done
  if curl -fsS -o /dev/null --max-time 3 "http://127.0.0.1:$CALC_PORT/healthz" 2>/dev/null; then services_up=1; fi
elif [ -z "$API_BIN" ]; then
  record calc-api fail "no calc-api is built anywhere (target/e2e, target/api)"
fi

# --- 2. the journeys --------------------------------------------------------------------------------------------
if [ "$DO_JOURNEYS" = 1 ]; then
  if [ "$services_up" = 1 ]; then
    say "the journeys (log: $(rel "$LOG_DIR")/journeys.log)"
    status=0
    rm -f "$RESULTS"
    CALC_API_URL="http://127.0.0.1:$CALC_PORT" CALC_WEB_URL="http://127.0.0.1:$WEB_PORT" MOCK_TING_URL="http://127.0.0.1:$TING_PORT" \
      CALC_CLI="$CLI_BIN" STACK_JSON="$STACK_JSON" JOURNEY_RESULTS="$RESULTS" E2E_DIR="$OUT_DIR" \
      perl -e 'alarm shift; exec @ARGV' "${E2E_TIMEOUT_SECONDS:-3600}" node "$ROOT/e2e/journeys/run.ts" ${JOURNEYS[@]+"${JOURNEYS[@]}"} 2>&1 | tee "$LOG_DIR/journeys.log" || status=$?
    if [ -f "$RESULTS" ]; then
      while IFS=$'\t' read -r name st detail; do record "$name" "$st" "$detail"; done < <(node -e '
        const r = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
        const one = (s) => (s || "").split("\n")[0].slice(0, 200);
        for (const j of r.journeys) {
          console.log([`${j.name}`, j.failed ? "fail" : (j.passed ? "pass" : "skip"), `${j.passed} passed, ${j.failed} failed, ${j.skipped} skipped (${(j.ms / 1000).toFixed(0)} s)`].join("\t"));
          for (const s of j.steps) {
            const st = s.status === "pass" ? "ok" : s.status === "fail" ? (s.required ? "FAIL" : "fail") : "skip";
            console.log([`  ${s.name.slice(0, 96)}`, st, s.status === "fail" ? one(s.error) : ""].join("\t"));
          }
        }
      ' "$RESULTS")
      [ "$status" = 0 ] || [ "$status" = 1 ] || record journeys fail "the journey runner ended with exit $status (log $(rel "$LOG_DIR")/journeys.log)"
    else
      record journeys fail "the journeys did not finish (exit $status; log $(rel "$LOG_DIR")/journeys.log)"
    fi
  else
    record journeys fail "calc-api did not start on $CALC_PORT, so no journey ran (log $(rel "$LOG_DIR")/calc-api.log)"
  fi
else
  record journeys skip "--skip-journeys"
fi

# --- 3. the web app's browser specs -------------------------------------------------------------------------------
if [ "$DO_WEB" = 1 ]; then
  if [ -f "$ROOT/web/playwright.config.ts" ] && grep -q '"test:e2e"' "$ROOT/web/package.json" && grep -qiE 'calc|CALC_API_URL' "$ROOT/web/playwright.config.ts"; then
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
      # Every spec on its own line.
      while IFS= read -r line; do
        case "$line" in
          *"✓"*) record "  $(printf '%s' "$line" | sed -E 's/^ *✓ *[0-9]+ *//; s/ \([0-9.]+m?s\)$//' | cut -c1-96)" ok "" ;;
          *"✘"*) record "  $(printf '%s' "$line" | sed -E 's/^ *✘ *[0-9]+ *//; s/ \([0-9.]+m?s\)$//' | cut -c1-96)" fail "" ;;
        esac
      done < <(grep -E '^ *(✓|✘) ' "$LOG_DIR/web-e2e.log" || true)
    else
      record web-e2e fail "calc-api did not start, so the browser specs could not run"
    fi
  else
    record web-e2e skip "web/ has no Calc browser specs yet (web/playwright.config.ts for Calc and a test:e2e script)"
  fi
else
  record web-e2e skip "--skip-web"
fi

echo
echo "Silicon Calc end to end ($(date '+%Y-%m-%d %H:%M'))"
for line in "${SUMMARY[@]}"; do echo "  $line"; done
echo
if [ "$FAILED" = 1 ]; then
  echo "FAILED (logs: $(rel "$LOG_DIR")/, every step with its error: $(rel "$RESULTS"))"
  exit 1
fi
echo "PASSED"
