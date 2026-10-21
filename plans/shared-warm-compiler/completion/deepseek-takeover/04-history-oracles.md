# D — Test the issued contract through its real consumer

Own the focused runtime/actor histories and their adjacent Haskell fixtures.
Coordinate production changes with A/B/C; do not create a new fixture framework,
authority factory, SQL setup repair or alternate compiler entry point.

## 1. Fix the hot-query diagnostic, without deleting its oracle

The failing cell is exomonad/actor/src/fixtures/compiler-world-info.hs, consumed
by public_module_world_hot_queries_skip_retained_history_preparation.
Its local bounded helper uses list (++) and show to construct String. Tidepool's
Prelude defines show as String-returning but unlines as [Text] → Text; both
diagnostic branches combine those incompatible families. Whole-cell typechecking
therefore rejects the cell before any query-answer or hot-path assertion.

Use String throughout this diagnostic: keep the JIT-safe Tidepool show and bounded
list helper, explicitly import qualified Prelude, and use its String-returning
unlines at both sites. The installed error is re-exported from Prelude and accepts
String. Do not switch to Prelude.show or globally change the installed API to
repair a fixture. OverloadedStrings does not convert values already typed String.
Keep bounded output, every query assertion and the controlled wrong-answer arm.
Compile/execute this exact file through the real notebook consumer and confirm
the negative arm reaches its deliberate answer refusal rather than typechecking.

This is test code with a real compilation obligation. A Rust include_str does
not typecheck Haskell; the native production-path case must remain in the focused
gate. Substantial fixtures stay in declared adjacent files, not escaped strings.

## 2. Correct the cold-history projection oracle

The actual cold case is in
tidepool/runtime/src/session/activation_input_tests/historical_original_recovery.rs.
At 28e8 it executes reader R = 41 and checks exact R→G→H bindings, then expects
the public compile context's Original roles to contain only R. That public view
selects facade G5; defining reader G4 is retained interface-only with native
availability. declaration_join::shared_source_edges and publication filtering
deliberately hide authenticated session implementation owners.

The test asks the wrong projection to prove execution authority. Do not promote
R into public roles, nor merely change expected [R] to []. Instead require:

- The accepted export refers to the exact defining R.
- R's actual issued native selection and R→G→H dependencies survive unchanged.
- G/H custody does not become independent direct-root permission.
- A newer H/G producing 99 does not replace R's historical H=41.
- Producer output removal and real cold recovery preserve the combined 41099
  semantic result and the same exact historical bindings.

Use the actual accepted join, native admission and snapshot owner to inspect
these facts. No getter or constructor whose sole purpose is to manufacture test
authority. If direct Original-root policy needs an independent case, use its
actual source-publication producer; the public facade promises another contract.
Run the complete existing history past the previous assertion; later phases are
not already qualified just because the first oracle was wrong.

## 3. Add the genuine eviction/reacquisition control

Follow the reviewed production-path design at:
/srv/swarm/evidence/inanna/stage-input-design-20261020/private-world-eviction-gate-design-20261021.md.

Important decisions from that design:

- Reuse the successful finite fixture with a separate declared eviction case.
  A private native completion alone does NOT admit its Haskell World. Query the
  actual adopted private Tip to realize the competing P World.
- Save the SAME public B inner World and source owner. Create and query a real
  private successor, retire private work without public publication, query the
  saved B to force reacquisition, then query B again for a resident hit.
- Use the measured case-only 2 MiB retained-storage budget: recorded individual
  Worlds are about 1.9 MiB and their first shared pair about 2.8 MiB. This is
  optional retention pressure, not request validity or a production default.
  Recheck the fit premise; report changes rather than silently raising the limit.
- Correlate exact worker/World/realization identities with actual eviction,
  missing-old-reference, new admission and subsequent hit events. A successor
  may legitimately need a separate missing-predecessor fallback: distinguish its
  extra Complete from an arbitrary retry instead of asserting a universal count.
- Inspect the complete retained trace only after service settlement. The queued
  writer's flush is a no-op; cleanup alone does not establish trace completeness.
  Missing events are failed qualification, not proof they never happened.
- Continue the finite history's semantic and CAF/cutoff behavior. FIFO index
  removal does not imply physical backing deletion while another owner retains it.

Use the existing native target generator and runner. Do not expose private
realization references or add a second durable observability log for this test.

## 4. Preserve the neighboring detection coverage

After A/B integrate, replay real completion-only enrichment and snapshot restore,
the latest corrected parent/child fixture, full finite history and captured-A
recovery. Keep missing-child, foreign witness, unsupported root and stale-owner
refusals. An unrelated setup error cannot satisfy a negative case.

After C integrates, batch the ordinary notebook/query/discovery/reload gates.
Record preconditions, first actual failing boundary and remaining unreached
assertions. Tests should make the next owner consume the newly transformed
result; consuming an unchanged parent misses this wave's failure class.
