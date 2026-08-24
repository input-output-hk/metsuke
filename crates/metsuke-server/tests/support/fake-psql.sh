#!/bin/sh
#
# The `psql` the server tests run. A script in the repo rather than one a test
# writes, because exec'ing a file this process wrote a moment ago races another
# test thread's fork and comes back ETXTBSY.
#
# Everything it reads and writes sits beside the socket directory the config
# named, which is all it can find: the callers clear the environment. What a
# test leaves there, and what it reads back, is tests/support/mod.rs.
#
# It refuses a --command as the real psql does — tests/fixtures/psql.
set -eu

socket=
previous=
for argument in "$@"; do
  if [ "$previous" = --host ]; then socket=$argument; fi
  previous=$argument
done
if [ -z "$socket" ]; then
  echo "fake psql: no --host in $*" >&2
  exit 64
fi

printf '%s\n' "PGOPTIONS=${PGOPTIONS-}" "PGPASSFILE=${PGPASSFILE-}" "$@" >"$socket/argv"

for argument in "$@"; do
  [ "$argument" = --command ] || continue
  echo 'ERROR:  syntax error at or near ":"' >&2
  exit 1
done

# Builtins alone, an emptied environment leaving no `cat` to find. A script
# without a closing newline gains one, which no answer this replays depends on.
while IFS= read -r line || [ -n "$line" ]; do printf '%s\n' "$line"; done >"$socket/argv.sql"

if [ -f "$socket/failure" ]; then
  while IFS= read -r line || [ -n "$line" ]; do printf '%s\n' "$line" >&2; done <"$socket/failure"
  exit 2
fi
while IFS= read -r line || [ -n "$line" ]; do printf '%s\n' "$line"; done <"$socket/answer.csv"
