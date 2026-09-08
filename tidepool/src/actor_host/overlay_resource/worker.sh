set -eu
view=$1
count=0
printf '%s\n' "$$"
while IFS= read -r command; do
    case "$command" in
        hold) exec 9>"$view/held"; printf 'held\n' ;;
        close) exec 9>&-; printf 'closed\n' ;;
        write) count=$((count + 1)); printf '%s\n' "$count" >"$view/value"; printf 'wrote\n' ;;
        quit) exit 0 ;;
        *) exit 2 ;;
    esac
done
