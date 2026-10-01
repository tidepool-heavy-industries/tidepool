#!/usr/bin/env bash
set -euo pipefail
# Deterministic test-runner fixture: real command/evidence transport, no Cargo.
mode=$(cat review-flow.txt)
head=$(git rev-parse HEAD)
status=$(git status --porcelain)
dir=$(mktemp -d)
case "$mode" in
  pass|selection|duplicates) passed=1; failed=0; code=0; runnable='["fixture::one"]' ;;
  fail) passed=0; failed=1; code=1; runnable='["fixture::one"]' ;;
  zero) passed=0; failed=0; code=0; runnable='[]' ;;
  infrastructure) passed=1; failed=0; code=2; runnable='["fixture::one"]' ;;
  mismatch) passed=0; failed=1; code=1; runnable='["fixture::one"]'; head=0000000000000000000000000000000000000000 ;;
  unknown) printf 'runner evidence unavailable\n' >&2; exit 2 ;;
  *) printf 'runner unavailable\n' >&2; exit 2 ;;
esac
matched=$runnable
if [[ "$mode" == selection ]]; then matched='["other::one"]'; fi
if [[ "$mode" == duplicates ]]; then matched='["fixture::one","fixture::one"]'; fi
python3 - "$dir" "$head" "$status" "$passed" "$failed" "$code" "$runnable" "$matched" <<'PYTHON'
import json,sys
from pathlib import Path
folder,head,status,passed,failed,code,runnable,matched=sys.argv[1:]
p=Path(folder)
(p/'output.log').write_text('fixture diagnostic\n')
(p/'evidence.json').write_text(json.dumps(dict(source=head,working_tree_status=status,
 executable='fixture',sha256='fixture',output=str(p/'output.log'),matched=json.loads(matched),
 runnable=json.loads(runnable),summaries=[[int(passed),int(failed),0,0,0]],exit_code=int(code))))
PYTHON
printf 'focused test evidence: %s/evidence.json\n' "$dir"
exit "$code"
