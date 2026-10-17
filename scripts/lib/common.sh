# Shared helpers for scripts/dev.sh, scripts/stop.sh, scripts/e2e.sh and scripts/accounts-stack.sh.
# Source it after setting SCRIPT (the name printed in front of messages):
#
#   SCRIPT=dev; . "$(dirname "$0")/lib/common.sh"
#
# Everything a script starts runs detached in its own process group, with its log in $LOG_DIR/<name>.log and its
# pid in $RUN_DIR/<name>.pid (plus <name>.match: what its command line must contain, so a recycled pid is never
# stopped). stop_service only stops a pid whose command still matches.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CALC_DEV="$ROOT/.dev"
LOG_DIR="${LOG_DIR:-$CALC_DEV/logs}"
RUN_DIR="${RUN_DIR:-$CALC_DEV/run}"
PG_BIN="${PG_BIN:-/opt/homebrew/opt/postgresql@16/bin}"
CALC_PGPORT="${CALC_PGPORT:-5482}"
CALC_PGDATA="${CALC_PGDATA:-$CALC_DEV/pg}"
SILICON_ACCOUNTS_DIR="${SILICON_ACCOUNTS_DIR:-$(cd "$ROOT/.." && pwd)/silicon-accounts}"
STACK_JSON="${STACK_JSON:-$ROOT/e2e/stack.json}"

say() { printf '%s: %s\n' "${SCRIPT:-calc}" "$*"; }
warn() { printf '%s: warning: %s\n' "${SCRIPT:-calc}" "$*" >&2; }
fail() {
  printf 'error: %s\n' "$1" >&2
  [ $# -gt 1 ] && printf 'hint: %s\n' "$2" >&2
  exit 1
}
rel() { case "$1" in "$ROOT"/*) printf '%s' "${1#"$ROOT"/}" ;; *) printf '%s' "$1" ;; esac; }

# Is something listening on 127.0.0.1:$1?
port_busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

who_listens() {
  command -v lsof >/dev/null 2>&1 || return 0
  lsof -nP -iTCP:"$1" -sTCP:LISTEN 2>/dev/null | awk 'NR==2 {print $1" (pid "$2")"}'
}

# Calc's own Postgres cluster (127.0.0.1:5482, data .dev/pg, trust auth).
pg_ready() { "$PG_BIN/pg_isready" -h 127.0.0.1 -p "$CALC_PGPORT" -q; }

ensure_postgres() {
  [ -x "$PG_BIN/pg_ctl" ] || fail "Postgres 16 is not at $PG_BIN" "install it (brew install postgresql@16) or set PG_BIN"
  if pg_ready; then
    return 0
  fi
  if [ ! -f "$CALC_PGDATA/PG_VERSION" ]; then
    say "initializing Calc's Postgres cluster in $(rel "$CALC_PGDATA")"
    mkdir -p "$CALC_PGDATA"
    "$PG_BIN/initdb" -D "$CALC_PGDATA" -U postgres --auth=trust --encoding=UTF8 >/dev/null
  fi
  say "starting Calc's Postgres on 127.0.0.1:$CALC_PGPORT (log: $(rel "$CALC_DEV")/pg.log)"
  "$PG_BIN/pg_ctl" -D "$CALC_PGDATA" -o "-p $CALC_PGPORT -k /tmp" -l "$CALC_DEV/pg.log" -w -t 30 start >/dev/null \
    || fail "Postgres did not start on 127.0.0.1:$CALC_PGPORT" "see $(rel "$CALC_DEV")/pg.log"
  printf '%s\n' "started" >"$RUN_DIR/postgres.started"
}

# Creates database $1 on Calc's cluster when it doesn't exist.
ensure_database() {
  local db="$1" exists
  exists="$("$PG_BIN/psql" -h 127.0.0.1 -p "$CALC_PGPORT" -U postgres -d postgres -Atc "select 1 from pg_database where datname = '$db'")"
  if [ "$exists" != 1 ]; then
    "$PG_BIN/createdb" -h 127.0.0.1 -p "$CALC_PGPORT" -U postgres "$db"
    say "created database $db"
  fi
}

# Drops database $1 on Calc's cluster (only names this repository makes: calc_e2e_*).
drop_database() {
  local db="$1"
  case "$db" in calc_e2e_*) ;; *) fail "refusing to drop database $db (only calc_e2e_* databases are dropped)" ;; esac
  PGOPTIONS='--client-min-messages=warning' "$PG_BIN/dropdb" -h 127.0.0.1 -p "$CALC_PGPORT" -U postgres --if-exists --force "$db"
}

# launch <name> <match> <cmd…>: starts the command detached in its own process group.
# Output goes to $LOG_DIR/<name>.log, the pid to $RUN_DIR/<name>.pid, <match> to $RUN_DIR/<name>.match.
launch() {
  local name="$1" match="$2"
  shift 2
  mkdir -p "$LOG_DIR" "$RUN_DIR"
  set -m
  nohup "$@" >"$LOG_DIR/$name.log" 2>&1 </dev/null &
  local pid=$!
  set +m
  printf '%s\n' "$pid" >"$RUN_DIR/$name.pid"
  printf '%s\n' "$match" >"$RUN_DIR/$name.match"
  LAST_PID="$pid"
}

alive() { kill -0 "$1" 2>/dev/null; }

# Is the service recorded as $RUN_DIR/<name>.pid still running (and still the same command)?
service_running() {
  local name="$1" file pid expected cmd
  file="$RUN_DIR/$name.pid"
  [ -f "$file" ] || return 1
  pid="$(tr -dc '0-9' <"$file")"
  [ -n "$pid" ] && alive "$pid" || return 1
  expected="$(cat "$RUN_DIR/$name.match" 2>/dev/null || true)"
  cmd="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  [ -z "$expected" ] || case "$cmd" in *"$expected"*) return 0 ;; *) return 1 ;; esac
}

# stop_service <name> [grace seconds]: SIGTERM to the process group, SIGKILL after the grace period.
stop_service() {
  local name="$1" grace="${2:-10}" file pid expected cmd waited
  file="$RUN_DIR/$name.pid"
  [ -f "$file" ] || return 0
  pid="$(tr -dc '0-9' <"$file")"
  expected="$(cat "$RUN_DIR/$name.match" 2>/dev/null || true)"
  rm -f "$file" "$RUN_DIR/$name.match"
  [ -n "$pid" ] || return 0
  if ! alive "$pid"; then
    [ "${QUIET:-0}" = 1 ] || say "$name (pid $pid) was not running"
    return 0
  fi
  cmd="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  if [ -n "$expected" ]; then
    case "$cmd" in
      *"$expected"*) ;;
      *)
        say "pid $pid is no longer $name (it is: ${cmd:-unknown}); leaving it alone"
        return 0
        ;;
    esac
  fi
  kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
  waited=0
  while alive "$pid" && [ "$waited" -lt $((grace * 5)) ]; do
    sleep 0.2
    waited=$((waited + 1))
  done
  if alive "$pid"; then
    say "$name (pid $pid) did not stop within ${grace} s; killing it"
    kill -KILL -- "-$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null || true
  fi
  [ "${QUIET:-0}" = 1 ] || say "stopped $name (pid $pid)"
}

# wait_http <name> <url> <pid> <timeout s>: until the URL answers 2xx, while the process lives.
wait_http() {
  local name="$1" url="$2" pid="$3" timeout="$4"
  local deadline=$((SECONDS + timeout))
  until curl -fsS -o /dev/null --max-time 3 "$url" 2>/dev/null; do
    if ! alive "$pid"; then
      tail -n 30 "$LOG_DIR/$name.log" >&2 || true
      fail "$name exited during start-up (log: $(rel "$LOG_DIR")/$name.log)"
    fi
    [ "$SECONDS" -lt "$deadline" ] || fail "$name did not answer $url within $timeout s (log: $(rel "$LOG_DIR")/$name.log)"
    sleep 0.3
  done
}

# require_free_port <port> <what>
require_free_port() {
  if port_busy "$1"; then
    fail "port $1 ($2) is already in use by $(who_listens "$1" || echo 'another process')" \
      "stop it (scripts/stop.sh stops what scripts/dev.sh started), or free the port"
  fi
}

# json_get <file> <dotted.path>: prints a value from a JSON file (strings bare, others as JSON).
json_get() {
  node -e '
    const v = process.argv[2].split(".").reduce((o, k) => (o == null ? undefined : o[k]), JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")));
    if (v === undefined || v === null) process.exit(3);
    process.stdout.write(typeof v === "string" ? v : JSON.stringify(v));
  ' "$1" "$2"
}

# The Rust build settings every Calc script uses (docs/BUILDING.md).
cargo_env() {
  export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
}

# Free space on / in GB (integer).
free_gb() { df -g / | awk 'NR==2 {print $4}'; }
