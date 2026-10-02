#!/usr/bin/env -S bash --
# Inert Git fixture: record exact process inputs and answer only guard probes.
printf '%s\0' "$#" "$0" "$PWD" "${MOCK_MARKER:-}" "$@" >> "$MOCK_LOG"
while [[ $# -gt 0 ]]; do
  case "$1" in
    -C|-c|--git-dir|--work-tree|--namespace) shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
case "${1:-}" in
  rev-parse)
    case "${!#}" in
      HEAD|tip\^\{commit\}) printf '%s\n' tip ;;
      target\^\{commit\}) printf '%s\n' target ;;
      stale\^\{commit\}) printf '%s\n' stale ;;
      *) exit 1 ;;
    esac
    ;;
  rev-list)
    if [[ ${MOCK_DROP:-} == yes ]]; then printf '%s\n' tip; fi
    ;;
  reset)
    printf 'selected executable ran\n'
    exit "${MOCK_EXIT:-0}"
    ;;
  *) exit 92 ;;
esac
