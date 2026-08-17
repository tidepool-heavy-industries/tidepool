# PRD 22 phase 1 — the protocol scaffold

**Contract:** `22-effect-protocol-prd.md`. Its *Hard rules* section is locked;
nothing here relaxes one.
**Scope:** the `tidepool-protocol` schema crate, its generators, the golden
infrastructure, and ONE effect migrated end to end.
**Audience:** every later migration lane. This doc is the foundation — the
schema vocabulary, the golden protocol, and the flip procedure defined here are
what lanes 2..N reuse. Read it before adding an effect to the schema.

---

## 1. The registries, surveyed

The PRD names four hand-maintained registries. Their actual per-effect coverage
is uneven, and that unevenness decides which effect goes first.

| Registry | Location | What it owns |
|---|---|---|
| **R1** macro DSL | `tidepool-mcp/src/effect_defs.rs` (2585 lines, 20 effects) | GADT ctors, Haskell type strings, error ADTs, docs, helper text, handler/method wiring |
| **R2** wire structs | `tidepool-bridge-effects/src/lib.rs` | 6 `CoreRecord` records + the `Wt*`/`Ev*`/`Ag*` positional mirrors |
| **R3** extractor | `haskell/src/Tidepool/Translate.hs` | `intrinsicVerbModules` (2 rows) + `sitedVerbs` (8 rows) |
| **R4** hole classifier | `tidepool-harness/src/engine.rs:359` `classify_hole` | constructor-NAME strings → `HoleRouting` |

**Coverage matrix for the small-effect candidates:**

| Effect | R1 | R2 | R3 | R4 | args | ret record | typed error ADT |
|---|---|---|---|---|---|---|---|
| Time | ✓ | — | — | — | none | — | — |
| Console | ✓ | — | — | ✓ (`Print`) | 1×Text | — | — |
| KV | ✓ | — | — | — | ✓ | — | — |
| Git | ✓ | ✓ (Commit/StatusEntry/FileDelta) | — | — | ✓ | ✓ | ✓ `GitError` |
| **Exec** | **✓** | **✓ (`Proc`)** | — | **✓ (`Run`/`RunIn`/`RunArgv`)** | **✓** | **✓** | **✓ `ExecError`** |
| Fs | ✓ | ✓ | — | — | ✓ | ✓ | ✓ — but 12 verbs, all-raw helpers |

R3 covers exactly ten names — two primop verbs (`eitherDecodeValue`,
`parseISO8601`) and the eight sited verbs of the RunLLMTurn/Fork/Finalize
family. No ordinary bridged effect appears in it. R4 covers twelve effects and
omits Time, Meta, Http, Git, Llm, Lsp, KV, Fs entirely.

---

## 2. Effect choice: **Exec**

The rule was *the smallest effect that still exercises args, a ret record, and a
typed error ADT*. Both Exec and Git clear that bar. Exec wins on four grounds.

**2.1 It spans the triangle where the bug class actually lives.** The PRD's
motivating defect (`RepoEventAwait`, 2026-08-16) was a verb present in R1 and
missing from R4. Exec is the smallest effect present in R1, R2 **and** R4 —
`tidepool-harness/src/engine.rs:450` routes `"Run" | "RunIn" | "RunArgv"` to
`OuterEffectKind::Exec`. Git touches only R1 and R2, so a Git-first migration
would prove the schema against the two registries that were never the problem.
Choosing Exec means phase 3's generated classifier inherits a first row that was
authored, validated, and reviewed in phase 1 rather than retrofitted.

**2.2 It exercises every arg shape the phase-1 schema needs, at minimum size.**
Three verbs: `Run` (1 arg), `RunIn` (2 args — the multi-arg helper form),
`RunArgv` (a `[Text]` list arg → `Vec<String>`, the only non-scalar arg among
the small candidates). Return type is the bridged record `Proc`. `ExecError` has
two single-field variants.

**2.3 It is the only small effect with a live `extra_imports` row.**
`extra_imports_for!(Exec)` yields three import lines
(`Tidepool.Shell` ×2, `Tidepool.Cargo`). That slot is byte-pinned today by
`preamble.rs`'s `import_gating_pin` module, so the migration has an existing,
independent witness that the slot did not move.

**2.4 Its three `raw` helpers dissolve into a deliberate schema feature — the
hard rule's first real test.** All three Exec helpers use the `{ raw [...] }`
escape hatch today, but *not* because they are irregular. Read them: `run` is
exactly the `pointfree` form and `runIn` exactly the `applied` form. They are
`raw` because `helper_text!`'s structured arms require at least one doc line
(`doc [$d0:literal $(, $d:literal)*]`), and `runIn`/`runArgv` carry no doc
comment at all. The escape hatch is standing in for a missing one-line grammar
affordance. Migrating Exec therefore forces exactly the transformation the PRD's
first hard rule demands — *raw hatch → deliberate schema feature* — at the
smallest possible size, with a byte-identical target to prove it against. A
Git-first migration would not exercise the rule at all (all four Git helpers are
already structured).

**What Exec deliberately does NOT exercise**, and which lane picks it up:

| Not exercised | Lane |
|---|---|
| Non-empty `type_defs` (records/ADTs beyond the error ADT) | Journal / Fs |
| Multi-field error variants (`GitFailed Int Text`) | Git |
| `type_params` / `default_row_args` / `helpers_row_polymorphic` | Finalize / RunLLMTurn |
| `prompt_card` | AskUser |
| The `Wt*`/`Ev*`/`Ag*` positional mirrors | Worktree / Event / Subagent |
| R3 sited-verb metadata | phase 3 |

**Hard constraint honored:** no Event or Subagent row is read, written, or
depended on by anything in this phase. The green-threads lane owns those rows.

---

## 3. The schema — `tidepool-protocol`

### 3.1 Crate rules

- **Leaf.** Zero dependencies — not on other tidepool crates, not on serde, not
  on syn. `std` only. (PRD open question 2's leaning, confirmed: adapters
  generate INTO consuming crates; the schema never climbs.)
- **Data-only.** Plain structs and enums plus pure rendering functions. No
  runtime component, no network IDL, no proc-macro.
- Consuming crates depend on it only through *generated files*, so nothing in
  the workspace gains a build-time edge on it.

```
tidepool-protocol/
  src/hs.rs        HsType — the closed Haskell type language + renderer
  src/schema.rs    Effect / Verb / Arg / Helper / ErrorAdt / TypeDef / annotations
  src/effects/     the effect data (phase 1: exec.rs)
  src/gen/         generators, one per emitted artifact
  src/bin/…        the writer + `--check` mode
  tests/           goldens + the current-files check
  goldens/         committed golden artifacts
```

### 3.2 The closed Haskell type language

Today every Haskell type in R1 is a hand-written string. A census of all 20
effects — every `args` type, every `ret`, every error-field type — yields a
**closed** language:

- atoms: `Text`, `Int`, `Bool`, `Value`, `()`
- named types: `Proc`, `Commit`, `WorktreeId`, `LspNode`, … (33 distinct)
- type variables: `v`, `a`
- constructors: `[T]`, `Maybe T`, `Either A B`, `(A, B)`

No string in the current registry falls outside it. So `HsType` is a closed enum
with a precedence-aware renderer: `render_arrow_arg()` (no parens — `Text ->
Maybe Value -> …`) and `render_app_arg()` (parenthesize compound — `Exec (Either
ExecError Proc)`, `Meta (Maybe (Int, Int))`). Both conventions fall out of one
structured type instead of being remembered per call site.

This matters beyond tidiness: a structured type is the precondition for phase
3's extractor policy and for deriving Rust types, and it is the line between *a
deliberate schema feature* and *an escape hatch*. Strings are the hatch.

**Validation (wave 2 deliverable):** a table of every distinct type string
appearing in every effect today, each paired with its `HsType`, asserted to
render byte-identically. Coverage of the whole registry, not just Exec — cheap
to write, and it is the evidence that the closed language is actually closed
before lane 2 relies on it.

### 3.3 Schema types

```
Effect { name, handler, req_enum, decl_fn,
         description: Vec<Line>, prompt_card: Option<Vec<Line>>,
         type_params, default_row_args, helpers_row_polymorphic,
         extra_imports, type_defs, errors: Option<ErrorAdt>,
         verbs: Vec<Verb>, helpers: Vec<Helper> }

Verb   { ctor, method, args: Vec<Arg>, ret: HsType,
         errors: Option<ErrorRef>,
         handling: HandlingClass,          // phase 3 — validated, unemitted
         extract: Option<ExtractPolicy> }  // phase 3 — validated, unemitted

Arg    { name, ty: HsType, rust: RustBinding }
Helper { name, doc: Vec<Line>, body: HelperBody }   // sig DERIVED, see 3.4
```

`RustBinding` is closed over what R1 actually needs: `Derived` (from `HsType` —
`Text`→`String`, `Int`→`i64`, `[Text]`→`Vec<String>`), `CoreValue`
(`tidepool_eval::value::Value`), `JsonValue` (`crate::effect_glue::JsonArg`),
`Bridged(name)`, `Path(p)`. The `Value`→`CoreValue`-vs-`JsonValue` split is a
genuine semantic distinction (an errors-tagged method receives no `cx`, so the
`DataConTable` lookup must happen at Req-decode time — see `effect_glue.rs:131`),
so it is a schema choice, not a free-form string. `Path` is the reviewed
pressure valve for domain types that never cross to Haskell
(`crate::handlers::worktree::WorktreeError`); it carries a Rust path, never
Haskell source. **Exec needs only `Derived`.**

### 3.4 Derived, not declared: helper signatures

Today a helper restates its signature as a string
(`sig "Text -> M (Either ExecError Proc)"`) alongside the constructor that
already implies it (`Run :: Text -> Exec (Either ExecError Proc)`). That is a
drift class: the two can disagree and nothing notices.

In the schema a thin helper names its `ctor` and the sig is **derived** —
substitute the effect head with `M`, thread `Either <Err>` where the verb is
errors-tagged. Verified against Git, Meta, KV, Http, Console, Exec: the
derivation reproduces every hand-written sig exactly.

Helpers that are not thin wrappers over one verb (`sayShow`, `forkSited`'s
`forall a effs. Member Fork effs => …`, Fs's multi-line bodies) are **not
representable** and stay hand-written OUTSIDE the contract until their effect's
lane makes them a deliberate schema feature. That is the hard rule applied
honestly: inexpressible means excluded, not smuggled in as a string.

### 3.5 Homes for the later phases — declared, validated, emitted-nothing

Phase 1 emits nothing for R3 or R4. But the annotations must have a natural home
now, or the schema will need a breaking change to grow one. Both slots are
**required fields** (so a verb that omits one fails generation, which is the
PRD's acceptance line "a verb missing a handling-class annotation fails
generation, not runtime") and both are **ignored by every phase-1 generator**.

**`HandlingClass`** — modelled on what `classify_hole` actually distinguishes,
which is **richer than the PRD's five-name sketch**. The code's `HoleRouting`
has nine variants: `RunLLMTurn`, `Fork`, `Finalize`, `AskUser`, `Note`,
`ReadState`, `Subagent`, `OuterEffect(Console|Worktree|RepoEvent|Exec|Journal)`,
`Ask`. Mapping onto the PRD's vocabulary: *suspend-to-model* is two classes
(RunLLMTurn vs Fork-with-join), *suspend-to-operator* is three (blocking
`AskUser` form, non-blocking `Note`, raw `Ask`), *outer-dispatch* is two
(`Subagent` and `OuterEffect`×5) plus the driver-immediate `ReadState`, and
there is no `ordinary` class at all — an unrecognized constructor falls through
to `Ask`, which is precisely why the `RepoEventAwait` omission was silent rather
than loud. The schema encodes the **code's** vocabulary; the PRD's five names
are an under-approximation and adopting them would lose information. Exec's
three verbs are all `OuterDispatch(Exec)`.

**`ExtractPolicy`** — from R3's eight `sitedVerbs` rows. The only degrees of
freedom exercised today are: `reject_polymorphic` (always true),
`reject_effect_monad` (true for the seven RunLLMTurn/Fork rows, false for
`finalize` — its value crosses in-heap and may be a closure), `answer_shape`
(`Scalar` | `ListOfElement`), `mis_shape` (`FallThrough` | `HardError`),
`type_args ∈ {1,2}`, `value_arity ∈ {1,2}`, plus the sibling name and module.
That is a closed struct, not an open one — `vsCheckType` is a *function field*
in Haskell today, and replacing it with enumerated policy is exactly the PRD's
"a new policy is added deliberately in Haskell, never serialized through the
schema". **Exec is `None`** (not a sited verb).

*Finding for phase 3:* `vsMisShapeIsError` is declared and documented at
`Translate.hs:3264` and set `True` on `forkMap`/`forkCata`, but **nothing reads
it** — the head-swap arm simply falls through. Treat it as declared-but-unenforced
policy; do not assume the generated form preserves behavior until that is
resolved.

### 3.6 What stays hand-written, forever

Handler method bodies; the handler struct (its fields are configuration, not
contract); `Tidepool.Shell` / `Tidepool.Cargo` and the rest of the Haskell
library layer (they *consume* the contract, they do not mirror it); `ToJSON`
instances for bridged records, which live in `Tidepool/Records.hs` as deliberate
orphans; the harness's tree policy; domain types' OS/backend concerns.

**Out of scope for phase 1 specifically:** `Proc` itself. Its Haskell decl is
generated today from the Rust struct through a different mechanism
(`CoreRecord` → `bridged_records_module()` → the committed
`haskell/lib/Tidepool/Records/Bridged.hs`). The schema *references* `Proc` as
`HsType::Named("Proc")` and records that `tidepool-bridge-effects` owns it.
Folding `Tidepool.Records.Bridged` into the schema is a later lane; doing it
here would double the blast radius for no added proof.

---

## 4. Generator staging (PRD open question 1) — **confirmed, with one refinement**

**Decision: committed output + a check TEST. Not build.rs. Not a check script.**

The PRD leaned this way; the evidence supports it, and adds a correction the PRD
could not have known.

*For committed output:*
- The workspace already has exactly one live instance of the pattern —
  `bridged_records` (`tidepool-handlers/tests/bridged_records.rs:104`): generate
  in a low crate, commit the artifact with a `DO NOT EDIT BY HAND` + regen-command
  header, whole-file `assert_eq!`, `TIDEPOOL_REGEN_BRIDGED` env escape hatch.
  It works and it is understood. Reusing it costs no new concept.
- GHC consumers make build.rs feedback slow, and a build.rs artifact is invisible
  in review. Generated Haskell and generated dispatch glue are exactly the
  artifacts a reviewer must see move.
- There is no build.rs precedent for source generation here. `tidepool/build.rs`
  embeds a tree; it generates nothing reviewable.

*Against a check script — the refinement:* `scripts/gen-workspace-deps.py`
supports `--check` and **nothing in the repo invokes it**. There is no CI
workflow directory. A check that nothing runs is not a check. The guard must
therefore be a test, which the test runner executes by construction.

*And where that test lives is load-bearing:* `bridged_records` sits in
`tidepool-handlers`, which `.config/nextest.toml`'s `default-filter` excludes
wholesale — **the existing precedent's guard does not run on the inner loop.**
It is pure string comparison needing no GHC; it is collateral damage from
package-level exclusion. So: **the generated-files check test lives in
`tidepool-protocol` itself** — a leaf, GHC-free, quick-tier crate. `cargo nextest
run` runs it. This is an evidence-backed improvement on the PRD's leaning, not a
deviation from it.

**Mechanism**

- `cargo run -p tidepool-protocol --bin tidepool-protocol-gen` writes every
  generated file.
- `--check` diffs without writing (for a human or a future CI hook).
- `tidepool-protocol/tests/generated_files_are_current.rs` asserts each committed
  file equals its generated bytes; `TIDEPOOL_REGEN_PROTOCOL=1` rewrites.
- Every generated file carries a `DO NOT EDIT` header naming the regen command,
  as `Bridged.hs` does.
- **Three layers, mirroring `bridged_records`:** (1) whole-file compare,
  (2) independent hardcoded pins on the per-item rendering, so a blind regen
  cannot launder a derive change into the goldens, (3) the existing semantic
  tests (handler + GHC) unchanged.

**Constraint on the emitter:** generated `.rs` files are subject to
`cargo fmt --all -- --check`. The generator's output must be a fixed point of
rustfmt, or the fmt gate and the golden gate fight. This is an acceptance item,
not a footnote.

---

## 5. Golden-test design — the reusable deliverable

Three artifact classes, three proof levels. **Every later lane reuses this
harness unchanged; only the effect list grows.**

### Class A — cross-boundary text. Bar: byte-identical, no exceptions.

These bytes are a compile-cache key (`ensure_effects_module` content-addresses
the emitted source; a single changed byte invalidates every cached compile for
every user) and a wire contract.

| Golden | Captured from |
|---|---|
| `effects_module.standard.hs` | `effects_module_source(standard_decls())` — the whole module, all 20 effects |
| `effect_decl.<name>.txt` | every field of every `EffectDecl`, all 20 effects |
| `tool_description.effects_index.txt` | `describe_effects_index(standard_decls())` |

The whole-registry scope is the point. Lane N flips effect N; these goldens prove
the other 19 did not move. That non-regression evidence is what makes an
effect-at-a-time migration safe, and it is worth more than the Exec slice alone.

Captured **before** any flip, from unmodified trunk, and committed as the
baseline.

### Class B — generated Rust source. Bar: reviewed diff + type identity.

Generated Rust cannot be literally byte-compared against a `macro_rules!`
expansion — there is no expansion text to compare to. The PRD anticipates this
("byte-for-byte, **or a reviewed, explained diff**"). Proof is threefold:

1. The generated file is committed and reviewed line by line against the
   documented expansion of `effect_rust_projection!` / `error_enum!` /
   `dispatch_body!`. The line-by-line account goes in §7 of this doc at flip time.
2. Type-identity assertions: `ExecReq`/`ExecError` variant names, order, arity,
   field types, and derive sets are asserted explicitly.
3. The existing Exec tests (`handlers/exec.rs:123-164`, including the GHC
   `test_jit_exec_family`) pass **unchanged** — they were written against the
   macro-generated types and are not touched by the flip.

### Class C — closed-language coverage. Bar: total over the current registry.

`HsType` renders every distinct type string in every effect today, byte-identically
(§3.2), and the helper-sig derivation reproduces every structured helper's
hand-written sig (§3.4). This is what licenses lane 2 to trust the vocabulary.

### Retention

Goldens are **not** deleted at the flip. They become regression pins: the same
files, now asserted against generated rather than hand-written input. A lane that
must change a golden changes it in the same commit as the code, with the diff
visible in review — which is the whole reason for committed output.

---

## 6. The flip

Wave 3, after Class A/B/C all pass on unmodified trunk.

1. `tidepool-mcp/src/generated/exec.rs` — `pub fn exec_decl() -> EffectDecl`.
   `effect_decls.rs:266`'s `exec_effect_def!(…effect_decl_projection)` line is
   replaced by the module.
2. `tidepool-handlers/src/generated/exec.rs` — `ExecError`, `ExecReq`,
   `impl DescribeEffect`, `impl EffectHandler`. `handlers/exec.rs:11`'s
   `exec_effect_def!(…effect_rust_projection)` line is replaced by the module.
3. **`exec_effect_def!` is deleted** from `effect_defs.rs`. The macro grammar and
   the other 19 definitions stay — this is an effect-at-a-time migration, not a
   flag day.
4. Class A goldens re-asserted: identical. This is the acceptance moment.
5. Full verify, including the GHC-heavy batteries that touch Exec
   (`-p tidepool-handlers -E 'test(handler_)'`, `-p tidepool-runtime -E
   'binary(jit_surface)'`).

**Stability locks, all untouched by construction:** the Haskell surface names
(`run`/`runIn`/`runArgv`, `Run`/`RunIn`/`RunArgv`, `ExecError`/`ExecSpawn`/
`ExecBadDir`) are schema data copied verbatim; no serde name is involved (Exec
carries none); CBOR/Core representation is `tidepool-repr`'s and is not touched;
the positional union-tag slot for Exec does not move (no effect is added or
removed from any row). The deploy handshake survives because the emitted
`Tidepool.Effects` bytes are asserted identical — the handshake keys on those
bytes.

---

## 7. Reviewed diff — generated Rust vs macro expansion

*(Filled in at flip time, Class B item 1. Empty until wave 3.)*

---

## 8. Risks and non-goals

**Risks**

- *rustfmt fixed point.* Generated `.rs` must survive `cargo fmt --check`.
  Mitigation: fmt gate runs in verify; emitter adjusted until it is a fixed point.
- *Golden churn from unrelated trunk movement.* Trunk is active. The Class A
  goldens cover all 20 effects, so any lane touching any effect def moves them.
  Mitigation: rebase before submit and re-capture; the goldens are cheap to
  regenerate and the diff is the review artifact. A green-threads edit to the
  Event/Subagent rows will show up as a golden diff — that is the harness
  working, not a conflict.
- *Under-modelling an annotation slot.* Mitigated by deriving both slots from
  the code's actual vocabulary (§3.5) rather than the PRD's sketch.

**Explicit non-goals for phase 1** — each is a later lane, and attempting any of
them here widens the blast radius without adding proof: `Translate.hs` metadata
emission; harness classifier generation; `Tidepool.Records.Bridged` migration;
the `Wt*`/`Ev*`/`Ag*` mirrors; retiring `tidepool-bridge-effects`; any Event or
Subagent row.

---

## 9. Adding an effect (for lanes 2..N)

1. Add `src/effects/<name>.rs` returning an `Effect`. Every verb needs a
   `handling` class; a sited verb needs an `extract` policy.
2. If a helper or type is not representable, say so and leave it hand-written
   outside the contract — or add a deliberate schema feature and document it
   here. Do not add a raw hatch.
3. Run the generator; confirm Class A goldens for **every other effect** are
   unchanged and yours matches its pre-flip capture.
4. Flip: swap the macro invocation for the generated module, delete the
   `<name>_effect_def!` macro, re-assert the goldens.
5. Verify with the batteries that touch the effect.
