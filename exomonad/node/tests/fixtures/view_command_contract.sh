set -eu
printf '%s\000' "$PWD" "$1" "$VIEW_CONTRACT_VALUE" "${HOME-unset}"
if read -r unexpected; then
    exit 91
fi
printf 'eof\000'
printf 'separate stderr\n' >&2
