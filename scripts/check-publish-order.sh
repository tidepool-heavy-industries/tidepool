#!/usr/bin/env bash
# Validate that PUBLISHING.md's publish order is a valid topological order of
# the workspace's normal-dependency graph (cargo metadata is the source of
# truth; publish = false crates — tidepool-testing and the two examples —
# are excluded on both sides). A topological order is not unique: this does
# NOT require PUBLISHING.md to match some canonical order, only that no
# crate is listed before a crate it depends on. Read-only, --locked, safe to
# run repeatedly.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISHING_MD="$ROOT_DIR/PUBLISHING.md"

if [[ ! -f "$PUBLISHING_MD" ]]; then
    echo "check-publish-order: $PUBLISHING_MD not found" >&2
    exit 1
fi

metadata="$(cargo metadata --locked --format-version 1 --manifest-path "$ROOT_DIR/Cargo.toml")"

# Declared order: strip "N. " / "N.  " prefixes and any trailing "(binary)" annotation.
mapfile -t declared_order < <(
    grep -E '^[[:space:]]*[0-9]+\.[[:space:]]+' "$PUBLISHING_MD" \
        | sed -E 's/^[[:space:]]*[0-9]+\.[[:space:]]+//; s/[[:space:]]+\(binary\)[[:space:]]*$//'
)

if [[ ${#declared_order[@]} -eq 0 ]]; then
    echo "check-publish-order: found no numbered publish-order list in $PUBLISHING_MD" >&2
    exit 1
fi

# publish_names: workspace members that DO publish (publish == null in cargo-metadata JSON).
publish_names_json="$(
    jq -c --argjson wm "$(jq -c '.workspace_members' <<<"$metadata")" '
        .packages
        | map(select(.id as $id | $wm | index($id)))
        | map(select(.publish == null))
        | map(.name)
    ' <<<"$metadata"
)"
mapfile -t publish_names < <(jq -r '.[]' <<<"$publish_names_json")

# edges: "dependent<TAB>dependency" for every NORMAL (kind == null) edge between
# two publish_names members.
edges="$(
    jq -r --argjson wm "$(jq -c '.workspace_members' <<<"$metadata")" --argjson pub "$publish_names_json" '
        (.packages | map(select(.id as $id | $wm | index($id))) ) as $wspkgs
        | ($wspkgs | map({(.id): .name}) | add) as $id2name
        | ($pub | map({(.): true}) | add) as $pubset
        | (.resolve.nodes | map(select(.id as $id | $id2name[$id] as $n | $n != null and $pubset[$n]))) as $nodes
        | $nodes[]
        | .id as $depender_id
        | .deps[]
        | select(.dep_kinds | any(.kind == null))
        | select(.pkg as $p | $id2name[$p] as $n | $n != null and $pubset[$n])
        | "\($id2name[$depender_id])\t\($id2name[.pkg])"
    ' <<<"$metadata"
)"

# --- validate: declared_order must contain exactly publish_names (as a set) ---
declare -A pos
for i in "${!declared_order[@]}"; do
    pos["${declared_order[$i]}"]=$i
done

missing_from_doc=()
for n in "${publish_names[@]}"; do
    [[ -v pos["$n"] ]] || missing_from_doc+=("$n")
done

extra_in_doc=()
declare -A pubset
for n in "${publish_names[@]}"; do pubset["$n"]=1; done
for n in "${declared_order[@]}"; do
    [[ -v pubset["$n"] ]] || extra_in_doc+=("$n")
done

fail=0

if [[ ${#missing_from_doc[@]} -gt 0 || ${#extra_in_doc[@]} -gt 0 ]]; then
    fail=1
    echo "check-publish-order: PUBLISHING.md's crate list does not match the publishable workspace members" >&2
    if [[ ${#missing_from_doc[@]} -gt 0 ]]; then
        echo "  missing from PUBLISHING.md: ${missing_from_doc[*]}" >&2
    fi
    if [[ ${#extra_in_doc[@]} -gt 0 ]]; then
        echo "  listed in PUBLISHING.md but not a publishable workspace member: ${extra_in_doc[*]}" >&2
    fi
fi

# --- validate: every edge (dependent depends on dependency) has dependency before dependent ---
violations=0
if [[ -n "$edges" ]]; then
    while IFS=$'\t' read -r dependent dependency; do
        [[ -v pos["$dependent"] && -v pos["$dependency"] ]] || continue
        if (( pos["$dependency"] >= pos["$dependent"] )); then
            violations=$((violations + 1))
            fail=1
            printf 'check-publish-order: VIOLATION — %s (position %d) is listed before its dependency %s (position %d)\n' \
                "$dependent" "$((pos[$dependent] + 1))" "$dependency" "$((pos[$dependency] + 1))" >&2
        fi
    done <<<"$edges"
fi

if [[ $fail -ne 0 ]]; then
    echo "check-publish-order: FAILED ($violations ordering violation(s))" >&2
    exit 1
fi

echo "check-publish-order: PUBLISHING.md's order is a valid topological order (${#declared_order[@]} publishable crates, checked against cargo metadata's normal-dependency graph)"
