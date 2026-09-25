# The discard hold; `discard_hold.rs` documents the held forms and the intent.
# Defines `git` for one Bash command: a held form runs only when
# EXOMONAD_DISCARD_EXPECTED_TIP names the ref's actual tip; every other call is
# `command git` unchanged. Safe under errexit and nounset: the hold runs in an
# `||` context and reads every variable with a default.
__exomonad_discard_hold() {
  local -a args=("$@") globals=() rest=()
  local i=0 sub
  while [ "$i" -lt "${#args[@]}" ]; do
    case "${args[$i]}" in
      -C|-c|--git-dir|--work-tree|--namespace)
        globals+=("${args[$i]}" "${args[$((i + 1))]:-}"); i=$((i + 2)) ;;
      -*) globals+=("${args[$i]}"); i=$((i + 1)) ;;
      *) break ;;
    esac
  done
  [ "$i" -lt "${#args[@]}" ] || return 0
  sub="${args[$i]}"
  rest=("${args[@]:$((i + 1))}")
  case "$sub" in
    reset|rebase|branch|push) ;;
    *) return 0 ;;
  esac
  __exomonad_git() { command git "${globals[@]}" "$@" 2>/dev/null; }
  local verb="" tip="" target="" dropped="" arg
  local -a positional=()
  case "$sub" in
    reset)
      local hard=""
      for arg in "${rest[@]}"; do
        case "$arg" in
          --hard) hard=1 ;;
          --) break ;;
          -*) ;;
          *) positional+=("$arg") ;;
        esac
      done
      [ -n "$hard" ] || return 0
      verb="reset"
      tip=$(__exomonad_git rev-parse --verify -q HEAD) || return 0
      target=$(__exomonad_git rev-parse --verify -q "${positional[0]:-HEAD}^{commit}") || return 0
      dropped=$(__exomonad_git rev-list "$tip" "^$target") || return 0
      ;;
    rebase)
      local onto="" skip="" expect_value=""
      for arg in "${rest[@]}"; do
        if [ -n "$expect_value" ]; then
          [ "$expect_value" = onto ] && onto="$arg"
          expect_value=""; continue
        fi
        case "$arg" in
          --skip) skip=1 ;;
          --onto) expect_value=onto ;;
          --onto=*) onto="${arg#--onto=}" ;;
          -s|-X|-x|--strategy|--strategy-option|--exec) expect_value=other ;;
          -*) ;;
          *) positional+=("$arg") ;;
        esac
      done
      verb="rebase"
      if [ -n "$skip" ]; then
        tip=$(__exomonad_git rev-parse --verify -q REBASE_HEAD) || return 0
        dropped="$tip"
      elif [ -n "$onto" ]; then
        local upstream fork
        tip=$(__exomonad_git rev-parse --verify -q "${positional[1]:-HEAD}^{commit}") || return 0
        target=$(__exomonad_git rev-parse --verify -q "$onto^{commit}") || return 0
        upstream="${positional[0]:-}"
        [ -n "$upstream" ] || upstream='@{upstream}'
        upstream=$(__exomonad_git rev-parse --verify -q "$upstream^{commit}") || return 0
        fork=$(__exomonad_git merge-base "$upstream" "$tip") || return 0
        dropped=$(__exomonad_git rev-list "$fork" "^$target") || return 0
      else
        return 0
      fi
      ;;
    branch)
      local delete="" force=""
      for arg in "${rest[@]}"; do
        case "$arg" in
          -D) delete=1; force=1 ;;
          -d|--delete) delete=1 ;;
          -f|--force) force=1 ;;
          -*) ;;
          *) positional+=("$arg") ;;
        esac
      done
      [ -n "$delete" ] && [ -n "$force" ] || return 0
      verb="branch deletion"
      local name named
      for name in "${positional[@]}"; do
        named=$(__exomonad_git rev-parse --verify -q "refs/heads/$name") || continue
        local lost
        lost=$(__exomonad_git rev-list "$named" --not --exclude="refs/heads/$name" --all) || continue
        if [ -n "$lost" ]; then
          if [ -n "$dropped" ]; then
            printf 'exomonad discard hold: delete one branch per command when more than one would drop committed work (%s)\n' "${positional[*]}" >&2
            return 1
          fi
          tip="$named"; dropped="$lost"
        fi
      done
      ;;
    push)
      local force="" expect_value="" spec src dst published pushed
      for arg in "${rest[@]}"; do
        if [ -n "$expect_value" ]; then expect_value=""; continue; fi
        case "$arg" in
          -f|--force|--force-with-lease|--force-with-lease=*) force=1 ;;
          --repo|-o|--push-option|--receive-pack|--exec) expect_value=1 ;;
          -*) ;;
          *) positional+=("$arg") ;;
        esac
      done
      verb="force push"
      for spec in "${positional[@]:1}"; do
        case "$spec" in +*) ;; *) [ -n "$force" ] || continue ;; esac
        spec="${spec#+}"
        src="${spec%%:*}"; dst="${spec#*:}"
        [ -n "$src" ] || continue
        dst="${dst#refs/heads/}"
        published=$(__exomonad_git rev-parse --verify -q "refs/remotes/${positional[0]}/$dst") || continue
        pushed=$(__exomonad_git rev-parse --verify -q "$src^{commit}") || continue
        local lost
        lost=$(__exomonad_git rev-list "$published" "^$pushed") || continue
        if [ -n "$lost" ]; then
          if [ -n "$dropped" ]; then
            printf 'exomonad discard hold: force-push one ref per command when more than one would drop published work\n' >&2
            return 1
          fi
          tip="$published"; target="$pushed"; dropped="$lost"
        fi
      done
      if [ -n "$force" ] && [ "${#positional[@]}" -lt 2 ]; then
        printf 'exomonad discard hold: a force push must name its remote and refspec, so the published ref it overwrites can be compared before it runs\n' >&2
        return 1
      fi
      ;;
  esac
  [ -n "$dropped" ] || return 0
  local listed="" count=0 oid
  while IFS= read -r oid; do
    [ -n "$oid" ] || continue
    count=$((count + 1))
    if [ "$count" -le 10 ]; then listed="${listed:+$listed, }${oid:0:7}"; fi
  done <<< "$dropped"
  [ "$count" -le 10 ] || listed="$listed and $((count - 10)) more"
  local expected="${EXOMONAD_DISCARD_EXPECTED_TIP:-}" named_target="${EXOMONAD_DISCARD_TARGET:-}" resolved=""
  local how="Pass a DiscardIntent naming the actual tip: withDiscardIntent (DiscardIntent expectedTip target reason) on the command, or EXOMONAD_DISCARD_EXPECTED_TIP=<tip> (and EXOMONAD_DISCARD_TARGET=<target>) in its environment. A clean worktree does not waive this hold."
  if [ -z "$expected" ]; then
    printf 'Ref is at %s, and no expected tip was given; %s would drop committed %s. Inspect/rebase or confirm the actual tip.\n%s\n' \
      "${tip:0:7}" "$verb" "$listed" "$how" >&2
    return 1
  fi
  resolved=$(__exomonad_git rev-parse --verify -q "$expected^{commit}") || resolved=""
  if [ "$resolved" != "$tip" ]; then
    printf 'Ref is at %s, not expected %s; %s would drop committed %s. Inspect/rebase or confirm the actual tip.\n%s\n' \
      "${tip:0:7}" "${expected:0:7}" "$verb" "$listed" "$how" >&2
    return 1
  fi
  if [ -n "$named_target" ] && [ -n "$target" ]; then
    resolved=$(__exomonad_git rev-parse --verify -q "$named_target^{commit}") || resolved=""
    if [ "$resolved" != "$target" ]; then
      printf 'Target is at %s, not intended %s; %s would drop committed %s. Inspect/rebase or confirm the actual target.\n%s\n' \
        "${target:0:7}" "${named_target:0:7}" "$verb" "$listed" "$how" >&2
      return 1
    fi
  fi
  printf 'Discard confirmed at %s: %s drops committed %s%s\n' \
    "${tip:0:7}" "$verb" "$listed" "${EXOMONAD_DISCARD_REASON:+ ($EXOMONAD_DISCARD_REASON)}" >&2
  return 0
}
git() { __exomonad_discard_hold "$@" || return $?; command git "$@"; }
