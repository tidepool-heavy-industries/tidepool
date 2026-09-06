# Custody worker — authorized before first bootstrap use

## Bounded assignment

Make admitted actor bootstrap structurally unable to run before its legitimate
worktree binding is installed. Existing Rust identity/BindingTable is the owner;
no sleeps, broad grants, second registry, retrying authorization errors or Haskell
precheck convention. Parent service TL exclusively integrates shared actor_host
changes. Read owning AGENTS; return focused candidate and independent review.

## Verified facts and uncertainty

At Tidepool baseline `e5a1842`, historical child 9 was admitted in a two-child
applicative unfold with `boundHead` and the same seed as successful sibling 10.
Receipt had correct parent/new child tree. No provider binding or first response
was recorded; first worktree operation failed `WorktreeUnauthorized`. Replacement
13 succeeded with a new allocation and slightly newer doc seed. This does NOT prove
an exact race. Denial logs lack operation/principal/binding snapshot.

Source trace: `haskell/actors/Tidepool/Actors/Internal/Agent.hs` captures target
worktree and invokes `worktreeHead` before `requestSessionSited`. Child computation
already uses child identity. Admission in `tidepool-actor` resident actor code
creates workspace/spawns actor; `tidepool/src/actor_host.rs` handles PolicyInstalled
asynchronously and later `prepare_actor_worktree` installs binding. Worktree handler
checks exact actor/runtime/incarnation custody against BindingTable.current.
Locate current lines/callers rather than trusting old line numbers.

## Required invariant and ownership

Rust admission/bootstrap must either establish binding before the first RunRequest
can use it, or hold that bootstrap behind explicit custody readiness. Provider-ready
is too late. Allocation, binding and launch cancellation must roll back/release only
the exact resources they own; completed effects and user work survive. Reuse existing
process/worktree supervision and stale-incarnation checks. Add bounded structured
denial evidence (operation, expected/actual principal and binding readiness) without
using rendered text as state.

Coordinate with service TL on launch barriers, but keep worktree authority distinct
from controller-ready and provider-ready. Shared actor_host edits go through TL;
independent worktree/actor test modules can be separate workers if useful.

## Acceptance and review

Deterministically delay custody installation, not timing sleeps. Immediate first
bootstrap worktree operation and two-child unfold must succeed only once authorized;
sibling access and stale incarnation remain denied. Exercise cancel after allocation,
after binding and before provider start; provider launch failure; exact release;
missing/failed install; duplicate lifecycle notices. Fresh reviewer traces both
admission and all cleanup paths and examines tested candidate revision.

Return exact commit, commands/outcomes, retained failure evidence and remaining
uncertainty. A regression proving the class does not retroactively prove the precise
historical cause. Do not widen authority to make the test green.
