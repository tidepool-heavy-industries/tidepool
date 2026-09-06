#!/bin/sh
# Controlled process peer: exercise transport plumbing, never provider behavior.
set -eu
test "$#" -eq 2
test "$1" = app-server
test "$2" = proxy
printf '%s\n' "$$" > peer.pid
IFS= read -r request
printf '%s\n' "$request" > input.jsonl
if test -f close; then
    printf '%s\n' ' ERROR controlled handshake failure' >&2
    exit 17
fi
if test -f hang; then
    IFS= read -r unused
    exit 18
fi
# Exceed pipe capacity before responding: a missing drain blocks initialization.
i=0
while test "$i" -lt 32768; do
    printf '%s\n' 'diagnostic output must be drained independently of protocol stdout' >&2
    i=$((i + 1))
done
IFS= read -r response < initialize.json
printf '%s\n' "$response"
while IFS= read -r message; do
    printf '%s\n' "$message" >> input.jsonl
done
