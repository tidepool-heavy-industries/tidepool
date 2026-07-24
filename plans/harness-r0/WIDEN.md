# Widen (S3) — leaf briefs behind the freezes

All leaves build against FREEZES.md (F1/F2/F3 — do not reopen) on
post-spike main (`ffd31153`+). Testing policy: TARGETED nextest over
touched crates only; no battery. Sonnet leaves; specs carry the plan.
Waves are grouped by file-footprint disjointness, not by theme:

- **Wave A (parallel now):** A1 uiOf (tidepool-harness, new module),
  A2 web-widen (tidepool-web), A3 acceptance (tests only).
  provider-responses (provider/) already in flight rides along.
- **Wave B (after A merges; engine neighborhood, sequential):**
  B1 scheduler+fanout, then B2 elaboration flow.
- **Gated:** `[form|]` quasiquoter — needs a GO/NO-GO probe that
  QuasiQuotes survive the extract pipeline before any build; not in a
  wave until that probe passes.

## A1 — uiOf: server-derived forms (tidepool-harness)

Goal (§6): derive a `Ui` form for a typed hole's answer type T from the
compiled artifact's `DataConTable` (flLabel is captured) — operators
answer `returnControl @T` holes via a real form, not raw Haskell, on
MECHANICAL paths (D6). New module `tidepool-harness/src/uiof.rs`:
`ui_of(dct: &DataConTable, ty: &str) -> Option<Ui>` — single-constructor
records → Card of labeled TextIn/Choice (Bool → Choice yes/no, small sum
types → Choice over nullary constructors); multi-constructor with fields
or unmappable types → None (fall back to the Code+eval card as today).
Submission mapping: `{values}` keyed by field label → build the `resume`
expression mechanically (record syntax) — reject (route to prose path)
when any field is missing/unparseable. Unit tests over a fixture
DataConTable; one GHC-tier test proving a derived form's mechanical
answer resumes a real hole. DO NOT touch web rendering (A2 renders
whatever Ui you emit) or Haskell Generic machinery (ruled out, §6).

## A2 — web-widen: panes + protocol polish (tidepool-web)

One leaf, whole crate to itself. (1) Meters pane: fold Usage from
TurnDelta events per node + rollup, live via SSE. (2) Trace pane:
per-node effect req/resp tail (Effect events), collapsed by default.
(3) Protocol: `eval_in_binding(node, name, expr)` verb (exists in spec,
unbuilt), snapshot pagination (kill the O(n) dense-id scan — cursor
paging), curl examples in crate README. (4) How-to-drive doc:
README section covering boot, ssh -L auth, create/force/answer via
panes and via curl, --replay mode. Keep the vendored-JS/no-CDN rule
(D5); every capability protocol-first (a pane is a client). Targeted:
`cargo nextest run -p tidepool-web` + snapshot tests.

## A3 — acceptance suite (tests only)

PRD §11 through the production path (tests-drive-real-entry-point),
record-replay CI-shaped, zero live calls: suspend→hole publishes with
type; ill-typed resume rejected verbatim + continuation intact; bottom
does not consume; polymorphic/function sites fail at extract naming the
site; consent-integrity (fork request with no forcing event → literal
zero child effect/turn events, audited from the log); kill -9 →
restart → tree reconstructed, hole answerable (drive the real binary,
not fold_tree_state directly); `answer_return_control` exercised
end-to-end (spike built it untested). New files under
tidepool-harness/tests/ + tidepool-web/tests/ ONLY — no src changes; if
a test needs a src hook, report back instead of adding it.

## B1 — scheduler + fanout (tidepool-harness engine; after wave A)

`returnControlFanout @T :: [Text] -> M [T]` (F3): one park, N thunk
children, answers collected in order, resume with `[T]`. Extract already
intercepts generically — add the verb + `"fan": n` payload; harness
registers N children; child evals still serialize on the parked parent
machine (sequential-isolated; §6 contention rule). Harden: side-maps off
process-statics into Harness fields; fan badge Exact(n) wiring;
turn caps per child. GHC-tier test: fan of 3, one child ill-typed
first attempt, order preserved.

## B2 — elaboration flow (after B1)

F1's exception path: non-empty prose (or unknown shape) → calling model
as elaborator — prompt = hole card + submission + "produce `resume
expr`", show-before-consume (operator sees the proposed expr; a confirm
verb consumes). Mechanical path stays zero-model (D6 inversion is law).
Touches engine.rs/harness.rs + one web confirm verb.

## Merge order

provider-responses / A1 / A2 / A3 as ready (disjoint) → B1 → B2.
Root verifies each with the leaf's targeted filter + golden_path.
