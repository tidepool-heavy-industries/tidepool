# Receipt — agent-wave checkpoint fold (PRD 18, wave 1)

Submitted per the fold-cadence directive: fold first, then load. The vertical
core and coupled-spawn seam run as a FRESH lane (`agent-core`, Chain B) on the
post-fold tip. This directory is that lane's inheritance —
`inheritance-for-agent-core.md` is the entry point.

Branch rebased onto root's tip (`harness-interaction-surface`) before submit.

## Verification of the SUBMITTED tree

Each lane verified its own branch, but the submitted state is root's tip merged
with all three lanes plus a rebase that skipped a now-redundant cherry-pick
(`fc3363dc`, already an ancestor of root's tip). That combination had never
been compiled before, so it was verified as its own thing rather than inferred
from three green branches.

**Instrument:** `cargo` at HEAD of `root.agent-wave` after rebase onto
`harness-interaction-surface`; the two workspace-scoped runs went through
`/home/inanna/dev/tidepool/scripts/ghc-slots.sh detach` (one brokered leg at a
time, never concurrent).

| Check | Result | Log |
|---|---|---|
| `cargo check --workspace` | **rc=0**, clean | `/tmp/tidepool-ghc-detach.bOAuNT.log` |
| `cargo clippy --workspace` | **rc=0** | `/tmp/tidepool-ghc-detach.1xl6Md.log` |
| `cargo fmt --all -- --check` | **rc=0** (cheap, run directly — not slot-taking) | — |
| `cargo check --workspace --all-targets` @ `9266b00d` | **rc=0**, 0 errors | `/tmp/tidepool-ghc-detach.KApx1W.log` |
| `cargo check --workspace --all-targets` @ `f07d9e3a` | **rc=0**, 0 errors | `/tmp/tidepool-ghc-detach.b8G8s0.log` |

**Why the fourth row exists.** Root's tip advanced mid-verification
(`f0d1f6cb`, strict classify-verdict deserialization), so this branch was
rebased again and re-verified rather than submitted on the earlier sha. That
commit touches no file in this wave's diff — but it modifies
`tidepool-runtime/src/session/turn.rs`, and both of this wave's test binaries
compile against that crate. A file-overlap argument would have said "no
conflict, ship it", and this swarm has already ruled that argument
insufficient on its own.

`--all-targets` was used deliberately: a plain `cargo check --workspace` does
not compile test targets, so it would never have touched
`agent_mode_encoding.rs` or `agent_structural_codec.rs` — exactly where a
`tidepool-runtime` change could bite. The earlier rows were run before that
commit landed and are retained as the record of the pre-rebase tree.

The fifth row is the same discipline applied a second time: root's tip
advanced again to `bb8606cc` (wave-1.5 dogfood observability), which is a real
code commit touching `tidepool-harness` and `tidepool-runtime/src/session/mod.rs`.
Rebased and re-checked rather than resubmitting on a stale sha. **`f07d9e3a` is
the row that validates the tree actually being handed up**; the rows above it
are history.

Root's tip moved three times during this submit. Each time the question asked
was "did *code* move", not "did the tip move" — the docs-only advance
(`1cd82195` and siblings) was folded into the handoff without a re-check,
while the two code advances each got one.

Clippy emitted exactly two warnings, both `large size difference between
variants`, in `tidepool-codegen` (lib) and `tidepool-harness` (lib). **Zero
warnings point at any file in this wave's diff.** They are inherited, on two
independent grounds: neither crate appears in this branch's diff, and both
`receipt-adapter-bringup.md` and `receipt-mode-encoding.md` independently
recorded the same two warnings on separate branches *before* this merge — so
they pre-date the fold rather than being an artifact of it.

## Verdicts

| Gate | Verdict |
|---|---|
| Backend adapter (PRD 18 gate 2) | **Vertical proven live** — task → `item/tool/call` → Rust reply → same-turn resume → decoded structured completion, first attempt, ~7s |
| Gate 1(a) — Servant mode encoding | **GO.** `mode :- endpoint` as a closed type family; dispatch executes on the real JIT with a real effect handler |
| Gate 1(b) — list + recursive structural codec | **GO, no named limit.** Self-referential dictionary elaborates and runs |
| Config isolation (PRD 18 criterion 11) | **Proven** across offline, token-free-live, and token-spending-live runs |

## Named guards — specific failure modes, each with its own pass line

Per the named-guards rule: these exist to catch ONE thing each, so the
aggregate is not evidence for them.

**Instrument** for all three groups: `cargo-nextest`, per-test pass lines from
the run named in each row. Aggregates are stated alongside, not instead.

### `tidepool-runtime::agent_structural_codec` — nextest run `b03ae741-5172-4d76-a1ec-310bd5423fa5`

| Guard | What it catches | Result |
|---|---|---|
| `unknown_tag_is_a_loud_decode_error` | silent coercion on an unrecognized constructor tag | PASS |
| `wrong_field_count_is_a_loud_decode_error` | truncated/padded reconstruction on arity mismatch | PASS |
| `plan_nested_depth_two_encodes_correctly` | a flattened encode silently agreeing with itself on round-trip alone | PASS |
| `plan_empty_seq_round_trips` | empty container **on the recursive knot**, not a leaf field | PASS |

Completed vs selected: **10/10**, 0 failed, 0 skipped.

### `tidepool-runtime::agent_mode_encoding` — nextest run `938f5a93-41e2-4f5d-a112-5342d26a728f`

Header line `Starting 10 tests across 1 binary (258 binaries skipped)` confirms
the filter selected only this binary — i.e. none of the three sanctioned reds
were in the selection. `--no-fail-fast` passed explicitly.

| Guard | What it catches | Result |
|---|---|---|
| `dynamic_dispatch_executes_on_real_jit` | a module that merely *compiles* being mistaken for a JIT proof | PASS |
| `single_traversal_invariant_declaration_and_dispatch_keys_match` | schema/dispatcher drift — the invariant `compileTools` exists for | PASS |
| `single_traversal_invariant_names_are_the_expected_two` | the key-set match passing vacuously on two empty sets | PASS |
| `notify_endpoint_dispatches_through_the_same_path` | `Notify` diverging from `Call` | PASS |
| `compile_fail_tools_record_missing_generic` | missing `Generic` derive | PASS |
| `compile_fail_unsupported_endpoint_type` | non-endpoint field | PASS |
| `compile_fail_multi_constructor_call_input` | unsupported input shape | PASS |
| `compiletools_time_duplicate_wire_name` | two selectors normalizing to one wire name | PASS |
| `compiletools_time_invalid_identifier` | normalized name violating backend identifier rules | PASS |
| `compiletools_time_well_formed_record_compiles` | the negative fixtures passing because *everything* fails | PASS |

Completed vs selected: **10/10**. Five diagnostics fixtures — **3 type-level
`TypeError`, 2 `compileTools`-time `ToolCompileError`** — split honestly by
mechanism rather than claiming type-level coverage that does not exist, and
each asserts its own **message text**, not merely that compilation failed.
`compiletools_time_well_formed_record_compiles` is the control that keeps the
negative fixtures honest.

### `tidepool-agent` — my own run at the merge commit

| Guard | What it catches | Result |
|---|---|---|
| `wal_sidecar_appearing_for_a_preexisting_database_is_ignored` | false isolation failures from the operator's live sqlite | PASS |
| `wal_sidecar_for_a_brand_new_database_is_still_flagged` | the exclusion above swallowing a real new-database mutation | PASS |

Completed vs selected: **15/15 passed, 3 skipped** (the skipped three are
`#[ignore]` live-process tests, run separately and reported in
`receipt-adapter-bringup.md` as 3/3).

## Config isolation — instrument named

**Instrument:** the in-code `backend::codex::isolation::ConfigSnapshot` sha256
checker, **not** a shell diff — `~/.codex` holds live sqlite the operator's own
sessions write continuously, so a whole-directory diff produces false
positives. Independently re-verified outside the test process, which is what
makes the claim credible: a checker that only ever validates itself is not
evidence.

| File | sha256 | Across |
|---|---|---|
| `config.toml` | `a8e4e036…56b65` | offline, token-free-live, token-spending-live |
| `auth.json` | `264a9dd2…bba8` (prefix; full value in the lane receipt) | same |
| `installation_id` | `ea5a60ad-5d2d-41c3-8ce0-d0e3b732030f` | same |

The specific mutation avoided is the **project-trust write**: no
`projects.<tempdir>` entry appeared, confirming that omitting `cwd` from
`thread/start` and supplying it at `turn/start` is sufficient.

## Escalation — a real extract-pipeline bug, not a lane workaround

mode-encoding hit an **extractor-level** failure (not a GHC diagnostic) when
`HasAgentApi` was a zero-method class:

```
Dangling NVar reference(s) … 0xfec5b4a8c8bbd5bd = C:HasAgentApi
This is an extract-pipeline bug (a binding was renamed, culled, or missed by
reachability) — not a user error.
```

**Instrument:** `tidepool-extract-bin`'s own diagnostics JSON, captured live in
an ad-hoc local log (not a committed test) — reported from that log, not
re-derived.

Reading: a class with no methods elaborates to a dictionary with nothing in it,
and something in the reachability/culling pass dropped the
dictionary-constructor binding while a reference to it survived.

Worked around by making `HasAgentApi` a `ConstraintKinds` synonym, which has no
dictionary of its own to cull. **The workaround is sound and committed; the
underlying bug is not fixed and is not this lane's to fix.** Routing it up: any
zero-method class in this codebase is exposed to it, and the general guidance
until it is fixed upstream is to prefer constraint synonyms over zero-method
classes for constraint bundling.

This is evidence *for* the mode encoding, not against it — the bug was in
auxiliary constraint bundling, not in the `mode :- endpoint` family
application.

## Deviations, stated rather than buried

- **No flattened-fallback comparison was run**, so **no compile-time or
  term-size number is cited**. The gate-1(a) GO rests on the mode encoding
  working on its own terms, not on a measured advantage over the fallback. Per
  the name-the-instrument rule, an unattributed "materially better" figure
  would have been worse than none.
- **`HasAgentApi` is a `ConstraintKinds` synonym, not a single-param class** —
  forced by the extractor bug above, and within PRD 18's own allowance for the
  exact class/row spelling to follow existing Tidepool machinery.
- **Gate 1(b)'s wire shape is scoped to the proof.** Uniform
  `{"tag","fields":[positional]}` is correct Tidepool↔Tidepool and **wrong
  Tidepool↔model**; the live turn showed the child emitting named-field
  arguments. See `README.md` § "Encoding polarity" — this is the most likely
  way the successor lane goes wrong.
- **One rationale may already be stale.** structural-codec hand-rolled its
  codec partly because the vendored `FromJSON` generic default rejects sums
  with non-nullary constructors. `generic_deriving_337::sum_type_rejected_at_compile_time`
  — which pins exactly that behavior — is now a sanctioned red, meaning either
  sums now derive or only the diagnostic moved. **Not established here**; the
  inheritance doc tells agent-core to read the machinery rather than either
  receipt. Gate 1(b)'s *result* is unaffected either way.

## HOLD lines — all intact

1. **generic-surface's fold** — INTACT. No edit to, import of, or dependency on
   `Form.hs`, their Generic substrate, `Harness.Prelude`, or `preamble.rs`. All
   Haskell here is new files under `Tidepool.Agent.*` on base `GHC.Generics`.
2. **worktree-wave's vertical core / coupled-spawn seam** — INTACT. Not
   designed. `seam::Workspace` remains transitional and marked so at its
   definition. PRD 19's text-stability concern lifted; the joint-design
   announcement has not fired.
3. **realm step-4** — INTACT. Nothing in `resident.rs`
   pending/`ChildSuspended`. Realm parking consumed only through
   `../realm-lanes/continuation-parking-contract.md`; nothing read from
   `jit_machine.rs`.

Also intact: `Tidepool.Agent` was NOT renamed. The collision with the harness
answerer's capability row is documented for root and the harness owner to
settle.
