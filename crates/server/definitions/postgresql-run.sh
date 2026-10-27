set -eu
# The initial password arrives as an argument, which any user of the machine
# can read in /proc/<pid>/cmdline for as long as the process lives. Move it
# into the environment, readable by root alone, before anything else runs.
# `serve bin data port database user password` becomes `run bin data port
# database user`, the same process, its password in the environment.
if [ "${1-}" = serve ]; then
  BEATBOX_PG_PASSWORD=${7-} exec bash "$0" run "$2" "$3" "$4" "${5-}" "${6-}"
fi
mode=$1
bin=$2
data=$3
port=$4
required=
if [ -s "$data/PG_VERSION" ]; then
  required=$(cat "$data/PG_VERSION")
  case "$required" in *[!0-9.]*|'') exit 2;; esac
fi
version_of() {
  reported=$("$1/postgres" --version)
  reported=${reported#*PostgreSQL) }
  reported=${reported%% *}
  case "$reported" in 9.*) echo "$reported" | cut -d. -f1,2;; *) echo "${reported%%.*}";; esac
}
available() {
  [ -x "$1/postgres" ] && [ -x "$1/initdb" ] && [ -x "$1/pg_isready" ] && [ -x "$1/psql" ]
}
if [ -z "$bin" ]; then
  best=
  found=$(command -v postgres || true)
  for candidate in "${found%/*}" /usr/bin /usr/lib/postgresql/*/bin; do
    if available "$candidate"; then
      version=$(version_of "$candidate")
      if [ -n "$required" ] && [ "$version" != "$required" ]; then continue; fi
      newest=$(printf '%s\n%s\n' "$best" "$version" | sort -V | tail -n 1)
      if [ "$newest" = "$version" ]; then bin=$candidate; best=$version; fi
    fi
  done
fi
if [ -z "$bin" ] || ! available "$bin"; then
  if [ -n "$required" ]; then
    printf '{"code":"server.pg_version_missing","values":{"version":"%s"}}\n' "$required"
  else
    printf '%s\n' '{"code":"server.program_missing","values":{"service":"Database","program":"PostgreSQL"}}'
  fi
  exit 1
fi
if [ -n "$required" ] && [ "$(version_of "$bin")" != "$required" ]; then
  printf '{"code":"server.pg_version_missing","values":{"version":"%s"}}\n' "$required"
  exit 1
fi
if ! command -v setpriv >/dev/null; then
  printf '%s\n' '{"code":"server.program_missing","values":{"service":"Database","program":"setpriv"}}'
  exit 1
fi
as_postgres=(setpriv --reuid=postgres --regid=postgres --init-groups --)
# The database and user asked for, made once the server answers, and only
# if missing: every start, so a name added later is made too, and nothing
# that exists is ever touched. Names go in as psql variables and out
# through %I and %L; the password comes from the environment, never argv.
provision() {
  for _ in $(seq 60); do
    if [ -n "$stopping" ] || ! kill -0 "$child" 2>/dev/null; then return 0; fi
    if "${as_postgres[@]}" "$bin/pg_isready" -q -h "$data" -p "$port"; then break; fi
    sleep 0.5
  done
  BEATBOX_PG_PASSWORD=$password "${as_postgres[@]}" "$bin/psql" --no-psqlrc --quiet \
    --host "$data" --port "$port" --username postgres --dbname postgres \
    --set ON_ERROR_STOP=1 --set user="$user" --set database="$database" <<'SQL'
\set password `printf '%s' "$BEATBOX_PG_PASSWORD"`
SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', :'user', :'password')
  WHERE :'user' <> '' AND NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'user') \gexec
SELECT format('CREATE DATABASE %I OWNER %I', :'database', coalesce(nullif(:'user', ''), 'postgres'))
  WHERE :'database' <> '' AND NOT EXISTS (SELECT FROM pg_database WHERE datname = :'database') \gexec
SQL
}
case "$mode" in
  check) exit 0;;
  health) exec "$bin/pg_isready" -h 127.0.0.1 -p "$port";;
  query) exec runuser -u postgres -- "$bin/psql" --host "$data" --port "$port" --username postgres --dbname "${6:-postgres}" --no-psqlrc --set ON_ERROR_STOP=1 --command "$5";;
  run)
    database=${5-}
    user=${6-}
    password=${BEATBOX_PG_PASSWORD-}
    unset BEATBOX_PG_PASSWORD
    if ! getent passwd postgres >/dev/null; then
      useradd --system --user-group --home-dir /var/lib/postgresql --shell /usr/sbin/nologin postgres
    fi
    install -d -o postgres -g postgres -m 700 "$data"
    if [ ! -s "$data/PG_VERSION" ]; then
      # UTF8 and the C locale on purpose: the agent's environment rarely has
      # a LANG, and initdb would then pick SQL_ASCII, which stores accents
      # unchecked. The password reaches initdb through a pipe, never argv.
      BEATBOX_PG_PASSWORD=$password "${as_postgres[@]}" bash -c 'set -eu; umask 077
        "$1/initdb" -D "$2" --username=postgres --encoding=UTF8 --locale=C \
          --auth-local=peer --auth-host=scram-sha-256 --pwfile=<(printf "%s\n" "$BEATBOX_PG_PASSWORD")
        printf "\nhost all all 0.0.0.0/0 scram-sha-256\n" >> "$2/pg_hba.conf"' bash "$bin" "$data"
    fi
    # A simple command in the background, and setpriv execs: $child is the
    # postmaster itself (a function here would put a subshell in between).
    # The Server stops a service with TERM, which PostgreSQL reads as "wait
    # for every client to leave"; INT is its fast shutdown. Wait until it
    # has really gone, so a restart never finds the old postmaster still
    # holding the port.
    child=
    stopping=
    trap 'stopping=1; [ -z "$child" ] || kill -INT "$child" 2>/dev/null || true' TERM INT
    "${as_postgres[@]}" "$bin/postgres" -D "$data" -h 0.0.0.0 -p "$port" -k "$data" \
      -c shared_buffers=16MB -c max_connections=20 &
    child=$!
    [ -z "$stopping" ] || kill -INT "$child" 2>/dev/null || true
    if [ -n "$database$user" ]; then
      provision || printf 'BeatBox could not create the database or user it was asked for; see above.\n' >&2
    fi
    while :; do
      wait "$child" && status=0 || status=$?
      kill -0 "$child" 2>/dev/null || exit "$status"
    done
    ;;
  *) exit 2;;
esac
