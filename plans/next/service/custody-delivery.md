# Pre-bootstrap custody candidate

Production/test revision: `d5fedea7eec82b32744f3b27d4e90e4a316e4892`.
Base: `f0a408ed` (service custody scaffold). This is an implementation candidate,
not independent review or mounted-service acceptance.

The actor kernel now acquires exact BindingTable custody before evaluating its
entry. Installation is offloaded to a blocking task, not gated on provider
readiness. The kernel and host share the opaque lease; shutdown records terminal
kind, and the final owner settles that exact generation. Host launch validates
existing custody instead of installing it too late. Duplicate/stale claims fail.
Uncertain process launch or cleanup leaves the binding retained, not reusable.
Existing handler denials report operation, expected/actual principal and readiness.

The scaffold's default unsupported implementation was removed: all implementers
must supply custody installation. The lease interface additionally fences
possible process existence and distinguishes release from retention by the actor.
This corrects the scaffold's assumption that unconditional last-owner Drop is
safe when host launch/cleanup can fail or panic with a surviving process.

## Direct checks

- `NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(custody)'`: 5 executed,
  5 passed. Real temporary Git and real hosted Haskell, including two siblings
  with deterministically delayed installation, install failure before provider
  publication, exact-incarnation exclusion, last-owner release, missing targets,
  and retained binding after uncertain cleanup. No native model or TUI involved.
- `NEXTEST_TEST_THREADS=1 just test-lib tidepool-handlers
  'test(actor_worktree_authority_is_exact_to_resource_and_incarnation)'`: 1
  executed, passed, including stale/sibling/resource denial after release.
- `nix develop --command cargo build -p tidepool --bin shoal`: built, not launched.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

Changed host documentation/research test modules compiled in the tidepool lib
target; those tests were not executed. No broad battery or fixtures update.
Logs and binary SHA-256 identities are retained in `target/custody-evidence/`
in the implementer's worktree. Earlier failed build/test logs remain there too.

## Corrections and limits

The enclosing unfold can commit before deferred child bootstrap fails. Tests
therefore observe provider publication and child retirement, not a fabricated
synchronous failure of the parent tool call. This is distinct from the historical
incident: the regression proves the class, not its precise historical cause.

Migration decision for internal cleanup receipts: existing serialized outcomes
retain their representations; add `custodyRetainedByActor` for a successful
process reap whose final binding release is not yet observed. It is explicitly
degraded in rendered cleanup reporting, never completion. Root/run-map consumers
must incorporate this additive outcome before deployment of the new host.

Real tmux failure, timeout, cancellation and panic paths were source-reviewed,
not executed. The old tmux boundary can return an ambiguous spawn failure before
a pane identity is available; custody is conservatively retained in that case.
This candidate does not add process discovery/recovery or weaken that fence.
The service owner must verify/reconcile this against its replacing process
supervisor. Mounted native service/observer acceptance remains externally gated.
Neither the running host nor the native dependency pin was changed.
