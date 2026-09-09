#!/bin/sh
set -eu
view=$1
count=0
printf '%s\n' "$$"
while IFS= read -r command; do
    case "$command" in
        oom\ *)
            if python3 -c 'import pathlib,sys; pathlib.Path(sys.argv[2], "cgroup.procs").write_text("0"); f=open(sys.argv[1], "w"); f.write("before-oom"); f.flush(); a=bytearray(128*1024*1024)' "$view/oom-value" "${command#oom }"; then
                printf 'unexpected-success\n'
            else
                printf 'command-failed\n'
            fi ;;
        hold_build) exec 8>"$view/target/open"; printf 'held\n' ;;
        build_write) printf live >&8; printf 'wrote\n' ;;
        close_build) exec 8>&-; printf 'closed\n' ;;
        hold) exec 9>"$view/held"; printf 'held\n' ;;
        close) exec 9>&-; printf 'closed\n' ;;
        write) count=$((count + 1)); printf '%s\n' "$count" >"$view/value"; printf 'wrote\n' ;;
        remember) cd "$view"; printf 'remembered\n' ;;
        relative) if (printf relative >relative) 2>/dev/null; then printf 'wrote\n'; else printf 'readonly\n'; fi ;;
        reenter) cd "$view"; printf 'reentered\n' ;;
        quit) exit 0 ;;
        *) exit 2 ;;
    esac
done
