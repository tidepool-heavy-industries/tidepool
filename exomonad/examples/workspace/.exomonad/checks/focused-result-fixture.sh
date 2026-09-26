#!/usr/bin/env bash
set -euo pipefail

case "$1" in
  pass|fail|unknown|dirty|missingfile|zero|setup|short) ;;
  *) exit 2 ;;
esac

if [[ "$1" == unknown ]]; then
  echo 'the runner produced no evidence path' >&2
  exit 0
fi
if [[ "$1" == missingfile ]]; then
  echo "focused test evidence: /tmp/exomonad-missing-evidence-$$.json" >&2
  exit 0
fi

evidence_dir=$(mktemp -d)
if [[ "$1" == pass || "$1" == dirty || "$1" == zero ]]; then
  exit_code=0
  passed=1
  failed=0
else
  exit_code=1
  passed=0
  failed=1
fi
working_tree_status=''
if [[ "$1" == dirty ]]; then
  working_tree_status=' M README.md'
fi
runnable='["fixture::one"]'
if [[ "$1" == zero ]]; then
  runnable='[]'
  passed=0
fi
if [[ "$1" == setup ]]; then
  runnable='null'
  passed=0
  failed=0
  exit_code=2
fi
if [[ "$1" == short ]]; then
  passed=0
  failed=0
  exit_code=0
fi
final_code=$exit_code
if [[ "$1" == short ]]; then
  final_code=1
fi
printf '%s\n' 'fixture diagnostic' > "$evidence_dir/output.log"
cat > "$evidence_dir/evidence.json" <<EOF
{"source":"fixture-source","working_tree_status":"$working_tree_status","executable":"fixture-executable","sha256":"fixture-digest","output":"$evidence_dir/output.log","runnable":$runnable,"summaries":[[$passed,$failed,0,0,0]],"exit_code":$exit_code}
EOF
echo "focused test evidence: $evidence_dir/evidence.json" >&2
exit "$final_code"
