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
there is no `ordinary` class at all. The schema encodes the **code's**
vocabulary; the PRD's five names were compression, and adopting them would lose
information. Exec's three verbs are all `OuterDispatch(Exec)`.

> **The phase-3 requirement, stated once so no lane loses it.** `classify_hole`'s
> final arm is `_ => HoleRouting::Ask`. An unrecognized constructor is therefore
> indistinguishable from a genuine `AskWith` — *that* is why the `RepoEventAwait`
> omission was silent rather than loud. It did not fail; it misrouted. The
> generated classifier must close this on both ends: **a verb with no handling
> class fails GENERATION**, and **an unrecognized constructor at runtime fails
> LOUD** — never falls through to `Ask`. Exhaustiveness on both ends is the whole
> point of generating the classifier; a generated classifier that kept the
> catch-all would reproduce the bug class it was built to make unrepresentable.

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

*Against a check script, and the placement rule that follows:*

> **A check nothing runs is not a check.** This is the standing placement rule
> for every generated-files guard in this program, not an observation about one
> script. Two live instances motivate it. `scripts/gen-workspace-deps.py`
> supports `--check` and **nothing in the repo invokes it** — there is no CI
> workflow directory. And `bridged_records`, the workspace's one real instance
> of the generated-artifact idiom, sits in `tidepool-handlers`, which
> `.config/nextest.toml`'s `default-filter` excludes wholesale — so **that guard
> never runs on the inner loop**, despite being pure string comparison needing
> no GHC. It is collateral damage from package-level exclusion.
>
> Therefore: a generated-files guard is (a) a **test**, so the runner executes it
> by construction, and (b) placed in a crate **outside** the default-filter
> exclusion set, so `cargo nextest run` actually reaches it. Both, or the guard
> is decorative.

So the generated-files check test lives in `tidepool-protocol` itself — a leaf,
GHC-free, quick-tier crate. This is an evidence-backed improvement on the PRD's
leaning, not a deviation from it.

*The one place the rule cannot be satisfied, stated honestly:* the Class A
whole-registry goldens (§5) need `standard_decls()`, and every crate that can see
it is default-filter-excluded. Those are a **battery-tier** gate
(`scripts/battery.sh -p tidepool-mcp`), not an inner-loop one. That is a real
limitation of the split, not an oversight — the inner-loop guard is the
generated-files check; the goldens are the pre-merge one.

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

## 6. The flip — **done for Exec**

Wave 3, after Class A/B/C all passed on unmodified trunk. Recorded here in the
order it was performed, because §9 asks every later lane to repeat it.

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

Class B item 1. Generated Rust cannot be byte-compared against a `macro_rules!`
expansion — there is no expansion text to compare to — so every way the
generated file differs from what `effect_rust_projection!` / `error_enum!` /
`dispatch_body!` / `effect_decl_projection!` expanded is accounted for here.
**Six differences, all deliberate. Nothing else changed.**

**1. The glue moved to a sibling module, so three methods became `pub(crate)`.**
The macro expanded *inside* `handlers/exec.rs`, so its dispatch arms could reach
module-private methods. The generated glue lives in `crate::generated::exec`, so
`exec_run` / `exec_run_in` / `exec_run_argv` are now `pub(crate)` instead of
private. Visibility widened crate-internally; the public surface is unchanged,
and the methods stay hand-written where they were. This is the one structural
consequence of committed-output generation, and it is the price of the diff
being reviewable.

**2. Derive macros are imported by name.** `#[derive(ToCore, FromCore, Debug,
PartialEq, Eq)]` under `use tidepool_bridge_derive::{FromCore, ToCore};`, rather
than the macro's fully-qualified `#[derive(tidepool_bridge_derive::ToCore, …)]`.
Forced by rustfmt: the qualified form is 97 characters and `attr_fn_like_width`
is 70, so rustfmt would rewrap it and the format gate would fight the golden
gate. Same derives, same order, same resolution.

**3. Error-variant docs now have a home.** Each `errors` variant carries a `doc`
string in the registry, and it is DEAD DATA there — `error_enum!` drops it and
`error_variant_text!` drops it. The generator emits it as a Rust doc comment on
the variant. Strictly more information; nothing about behavior or bytes changes.

**4. `#[must_use]` on the decl builder.** The macro emitted none; a pure builder
that returns a value wants one. Behavior identical.

**5. `extra_imports` is a literal array, not a macro lookup.** Was
`extra_imports_for!(Exec)` — a table keyed on the effect's identifier in
`effect_defs.rs`; is now the same three strings emitted directly from schema
data, and the `(Exec)` arm of that table is deleted. Pinned twice over: by
`preamble.rs`'s `import_gating_pin` (which asserts the exact `EXEC_GIT_IMPORTS`
text) and by the Class A golden.

**6. Public paths are unchanged, deliberately.** `tidepool_mcp::exec_decl`,
`tidepool_handlers::ExecReq`, `tidepool_handlers::ExecError` all resolve exactly
as before — `handlers/exec.rs` re-exports the generated types. No caller moved.

Everything else is identical *by proof*, not by inspection:
`tidepool-mcp/tests/protocol_schema_equivalence.rs` asserted the macro-generated
`EffectDecl` equal to the schema's rendering field for field **while the macro
was still in place**, and the Class A goldens — captured from the hand-written
registry and never regenerated — are green against the generated output.

**Deleted:** `exec_effect_def!` (2502 bytes) from `effect_defs.rs`, its
invocation in `effect_decls.rs`, its invocation in `handlers/exec.rs`, and the
`(Exec)` arm of `extra_imports_for!`. The macro grammar and the other nineteen
definitions are untouched.

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

1. Add `src/effects/<name>.rs` returning an `Effect`, and list it in
   `effects::all()`. Every verb needs a `handling` class; a sited verb needs an
   `extract` policy. `Effect::validate` catches the structural mistakes.
2. If a helper or type is not representable, say so and leave it hand-written
   outside the contract — or add a deliberate schema feature and document it
   here. Do not add a raw hatch.
3. **Prove it before you flip.** Add the effect to
   `tidepool-mcp/tests/protocol_schema_equivalence.rs`. That test compares the
   `EffectDecl` against the schema's rendering field for field, and while the
   `<name>_effect_def!` macro is still in place it is a genuine proof rather
   than a tautology. Green here is the go-ahead; nothing is deleted before it.
   (`EffectDecl` is destructured exhaustively there, so a field added to the
   contract cannot silently go unproven.)
4. Run the generator. Confirm the Class A goldens are unchanged for **every
   other effect** — that is the non-regression half, and it is what makes an
   effect-at-a-time migration safe.
5. Flip: swap each macro invocation for the generated module, delete the
   `<name>_effect_def!` macro and any table arms keyed on the effect
   (`extra_imports_for!`), re-export the generated types from the effect's own
   module so public paths do not move.
6. Re-assert the goldens **without regenerating them**. Untouched goldens green
   against generated output IS the byte-compatibility result; regenerating them
   at this step destroys the proof.
7. Account for every difference from the macro expansion in a §7-style reviewed
   diff. If you cannot explain a difference, it is a bug, not a nuance.
8. Verify: `cargo fmt --all -- --check` (generated `.rs` must be a rustfmt fixed
   point), `cargo nextest run`, and the batteries that touch the effect.

---

## 10. The second flip — Journal (lane 2, confirms §9 repeats)

Journal was chosen as the first repeat of §9 specifically because it is small
and **not** Exec: one verb (`RecordStep`), no error ADT, opt-in row placement
(not in `build_base_stack`), and it is the first migrated effect whose only
`args`/`ret` involve `Value`/`()`.

**The procedure reproduced cleanly — no new diff class.** All six of §7's
accounted-for differences apply to Journal verbatim (sibling-module glue makes
`record_step` `pub(crate)`; derive macros imported by name; `#[must_use]` on
the decl builder; `extra_imports` as a literal array instead of an
`extra_imports_for!` table arm; public paths unchanged). Difference #3 (error
variant docs finding a home as Rust doc comments) does not apply — Journal
declares no `errors` block, so `tidepool-handlers/src/generated/journal.rs`
takes `handler_rs.rs`'s other branch (`use tidepool_bridge_derive::FromCore;`
only, no error enum emitted) for the first time on a *migrated* effect. That
branch already existed in the generator (written generically in phase 1, never
exercised by Exec since Exec has typed errors); Journal is what exercises it,
and it produced the byte-identical dispatch arm
(`JournalReq::RecordStep(kind, key, payload) => self.record_step(cx, kind, key, payload)`)
with no generator change needed.

**No schema extension needed either.** `HsType::Value`, `HsType::Unit`,
`RustBinding::JsonValue`, and `OuterEffect::Journal` were all already declared
in phase 1 (§3.3, §3.5) but unused by Exec's schema entry — `payload: "Value"
as crate::effect_glue::JsonArg` (Journal's arg) is the first migrated verb to
actually render `RustBinding::JsonValue`, and it rendered correctly on the
first try. Nothing in `tidepool-protocol/src/{hs,schema}.rs` changed.

**Byte-compat result.** `journal_decl_matches_the_schema_exactly`
(`tidepool-mcp/tests/protocol_schema_equivalence.rs`) proved the schema against
the still-live `journal_effect_def!` macro before the flip. After the flip, the
Class A `effect_decls.txt` golden (which already carried `journal_decl()` in
its pinned list — captured that way in phase 1 because Journal was known to be
next) was re-asserted **without regenerating it** and stayed byte-identical,
which is the migration's whole proof.

**Deleted:** `journal_effect_def!` from `effect_defs.rs`, its invocation in
`effect_decls.rs`, its invocation in `handlers/journal.rs`, and the `(Journal)`
arm of `extra_imports_for!`. The macro grammar and the remaining definitions
(now eighteen) are untouched. Event/Subagent/Exec rows were not read, written,
or depended on.

---

## 11. Wire-record emission (lane 3 — the capability the mirror retirements need)

Lanes 1 and 2 migrated effects whose `type_defs` were EMPTY. Worktree is the
first effect whose contract includes a *record vocabulary*: thirteen Haskell
declarations in `worktree_effect_def!`'s `type_defs`, thirteen matching Rust
wire structs hand-written in `tidepool-bridge-effects`, and a comment asserting
that the two lists agree positionally. That comment is the artifact this section
exists to delete — not by hoping, but by generating both sides from ONE ordered
field list.

Written BEFORE implementing, per §9 step 2: what follows is the design the
Worktree flip is built against, and the capability the Event and Subagent
retirements reuse unchanged.

### 11.1 The problem, precisely

`tidepool-bridge-effects/src/lib.rs` says, of the `Wt*`/`Ev*`/`Ag*` families:

> Field ORDER in these structs is the wire contract and must match those
> `type_defs` decls positionally.

Two hand-maintained lists, one invariant, zero enforcement. `ToCore` builds the
`Con` in Rust struct-field order; the extractor assigns positions from the
Haskell decl's field order. A field inserted in the middle of one list and
appended to the other type-checks on both sides and silently swaps two payloads
on the wire. Nothing in the workspace fails.

The `type_defs` strings are also the largest surviving raw-Haskell hatch in the
registry: thirteen `data` declarations and seven `instance ToJSON` bodies,
written as source text.

### 11.2 What generates

**One new schema vocabulary, three new emitters.** The schema's `TypeDef` grows
from the phase-1 placeholder (a record with a name and fields) into a shape
that can describe every declaration the Worktree slice carries:

```
TypeDef { name,              // the HASKELL type name — also the ctor for a Record
          wire_rust,         // the Rust wire type name when it differs (`WtWorktreeId`)
          shape: TypeShape,
          json:  JsonInstance,
          derives: WireDerives,
          domain: Option<DomainMap>,
          doc }

TypeShape
  ::= Record   { fields: Vec<RecordField> }        // data X = X { f :: T, … }
    | Sum      { variants: Vec<SumVariant> }       // data X = A | B T | C T U
    | Identity { payload, hs_binder, rust_field, validation }
                                                    // data X = X Text  — an id

RecordField { hs_name, rust_name, ty: HsType, doc }
SumVariant  { ctor, fields: Vec<HsType>, doc }
```

`RecordField` is the whole point: **one `Vec`, two renderings.** The Haskell
`data` declaration's field order and the Rust wire struct's field order are the
same vector traversed twice. They cannot disagree, because there is nothing for
them to disagree about. The positional comment does not get a better guard; it
gets deleted, and the invariant it asserted becomes untrue-by-construction.

From that one `TypeDef` list, three emitters run:

| Emitter | Output | Replaces |
|---|---|---|
| `TypeDef::render_decl`/`render_json` + `Effect::type_def_texts` | the `type_defs` slice inside `<eff>_decl()` — Haskell `data` decls, then `ToJSON` instances, then the error ADT | the raw strings in `worktree_effect_def!` |
| `gen/wire_rs.rs` (new) | `tidepool-bridge-effects/src/generated/<eff>.rs` — the wire structs/enums, their derives, their boundary constructors | the hand-written `Wt*` block |
| `gen/adapter_rs.rs` (new) | `tidepool-handlers/src/generated/<eff>_adapters.rs` — the MECHANICAL domain↔wire conversions | the mechanical half of `handlers/worktree.rs`'s conversion block |

Emission ORDER inside `type_defs` is fixed and reproduces today's bytes: every
shape declaration in schema order, then every `ToJSON` instance in schema order,
then the derived error ADT. That is the order `worktree_effect_def!` already
uses, and the Class A `effect_decls.txt` golden is what proves it.

### 11.3 `ToJSON` instances become a closed policy, not source text

Seven `instance ToJSON …` strings live in the Worktree `type_defs` today. They
are needed — the `errors` block templates a `ToJSON` for `WorktreeError`, so
every type reachable from an error field needs one, and the vendored generic
default only covers single-constructor records. They are also raw Haskell, which
the first hard rule forbids in the schema.

They are not, however, arbitrary. Four distinct shapes cover all seven:

```
JsonInstance
  ::= None                                  // no instance emitted
    | Transparent                           // toJSON (X t) = toJSON t
    | ShownString { binder }                // toJSON k = toJSON (show k)
    | Object { binder, keys: &[(json_key, hs_field)] }
```

`Transparent` covers the four identity types (a receipt reader wants the id, not
a wrapper object). `ShownString` covers `InProgressKind`. `Object` covers
`DirtySummary` (keys equal to field names) and `GitFailureReceipt` (keys
DELIBERATELY renamed — `gitArgs` → `"args"`, `gitCwd` → `"cwd"`, …, so the JSON
reads as a git receipt rather than as a Rust struct dump). The key map is data;
the rename is now visible in the schema instead of buried in a string.

`Object` requires a key for EVERY field of the record, `DirtySummary`'s
identity-mapped ones included. A partial key list is a silently omitted field —
the JSON would simply lack it, and nothing downstream would notice — so the
completeness check is in `TypeDef::validate` and spelling all four is the price.

The `binder` field is carried rather than normalized because the current
instances use three different binders (`d`, `r`, `k`) and the Class A goldens
are byte-locked. A binder is data, not source.

### 11.4 Wire newtypes and fallible boundary constructors

PRD 22's generator requirement: *wire-side integers and identifiers generate as
newtypes with fallible boundary constructors — decode once at the edge, typed
everywhere after.*

**The rule this lane locks:** a newtype with a boundary constructor is minted for
every `TypeShape::Identity` — a type that EXISTS as a type in the contract,
declared `data X = X Text` or `data X = X Int`. A bare `Int` or `Text` FIELD
inside a record is NOT promoted. The reason is the byte lock, and it is worth
stating plainly: promoting `createdAt :: Int` to `createdAt :: CreatedAtMs`
changes the Haskell declaration, and that declaration is pinned by the Class A
golden. A wire-side-only newtype (Rust newtype, transparent codec, unchanged
Haskell) is technically possible but would need hand-written `ToCore`/`FromCore`
impls to stay byte-identical, which trades a proven mechanism for an unproven
one to buy type safety on a timestamp. If a later lane wants a typed timestamp
it changes the Haskell decl deliberately and moves the golden in the same
commit — visibly, which is the whole reason the output is committed.

Under that rule the Worktree slice mints four: `WorktreeId`, `GitOid`, `GitRef`,
`BranchName`. It is exactly the right rule for the lanes that follow — Event's
`EventId Int` / `SubscriptionId Int` and Subagent's `AgentId Int` /
`CycleId Int` are standalone identity declarations carrying integers, so
"wire-side integers generate as newtypes" lands on them directly when those
mirrors retire.

Each `Identity` emits, beside the struct:

```rust
impl WtWorktreeId {
    /// The trust boundary: an untrusted raw value becomes a wire id here or
    /// not at all.
    pub fn new(raw: impl Into<String>) -> Result<Self, WireError> { … }
    pub fn as_str(&self) -> &str { … }
}
```

with the policy declared in the schema:

```
Validation ::= None
             | NonEmpty
             | Segment { max_len, extra_allowed }   // ascii-alphanumeric + these
```

`Segment` is BYTE-oriented, not char-oriented: `max_len` counts bytes and the
alphabet check runs over `.bytes()`. That is not an implementation shortcut —
`is_path_safe` is byte-oriented because the value is joined into a filesystem
path as one component, and a char-oriented generated check would accept
multi-byte input the domain rejects. The two must agree exactly or the
cross-check test below is the only thing standing between them.

`WorktreeId` gets `Segment { max_len: 128, extra_allowed: "-_" }` — which is
exactly `tidepool_worktree::WorktreeId::is_path_safe`, hoisted out of
`handlers/worktree.rs`'s hand-written `worktree_id_from_wire` into declared
schema data. The handler keeps the SEMANTIC half (a rejected id is spelled
`WorktreeNotRegistered`, because no id outside the minted alphabet was ever
registered, and the caller learns nothing about the filesystem); the generator
owns the MECHANICAL half. `GitOid`/`GitRef`/`BranchName` get `NonEmpty` — the
weakest policy that is true today. Tightening one is a schema edit with a test,
not a code change.

**Two honest gaps, both closing on the same trigger.**

1. *The `raw` field stays `pub`.* A private field with `new` as the only
   constructor is the shape that makes an unvalidated wire id unrepresentable.
   It is not taken here because roughly twenty struct-literal construction sites
   live in `handlers/event.rs`, `handlers/agent.rs` and
   `tests/repo_event_with_handler.rs` — files owned by the Event and Subagent
   lanes, which are explicitly out of bounds for this lane.
2. *The `Wt` prefix stays.* PRD 22 retires the `Wt`/`Ag` prefixes "with the
   mirrors". The prefix's actual job is disambiguating
   `tidepool-bridge-effects`'s own namespace, where the hand-written `Ev*`/`Ag*`
   types sit alongside the Worktree ones and REFERENCE them; dropping it while
   those neighbours exist is a rename of shared vocabulary in another lane's
   files, with no proof value for this lane's claim. The schema carries
   `wire_rust: Some("WtWorktreeId")` as one line of data, so the retirement is a
   schema edit.

**The trigger for both, stated so no lane loses it:** when the LAST mirror
family (`Ev*`, then `Ag*`) is generated, drop `wire_rust` from every `TypeDef`
and make every `Identity` field private in one sweep. Both are one-line schema
changes at that point and neither is a rename across lane boundaries.

**One duplication this creates, named rather than hidden.** The `Segment` policy
is now expressed twice: in the schema (generated into `tidepool-bridge-effects`)
and in `tidepool_worktree::WorktreeId::is_path_safe`. They cannot be unified —
`tidepool-worktree` is a domain crate and must not depend on the bridge layer,
and the bridge layer is lower than the domain. A cross-check test in
`tidepool-handlers` asserts the two agree over a shared corpus (including the
traversal-shaped inputs the existing boundary test already pins). That is a real
cost of the crate direction, not an oversight, and the guard is the mitigation.

### 11.5 Adapter skeletons — the generated/hand-written split, stated explicitly

The PRD asks for "mechanical `From<domain>`/`TryFrom<wire>` adapter skeletons
where a richer domain form (PathBuf, Option, newtypes) genuinely differs". The
split is a schema field, so it is visible rather than inferred:

```
DomainMap { domain_path,
            into_wire: Option<AdapterKind>,     // None = no such conversion exists
            from_wire: Option<AdapterKind> }

AdapterKind
  ::= IdentityRaw { as_str, from_raw }          // newtype raw ↔ newtype raw
    | VariantMap  (&[(domain_variant, wire_variant)])
    | HandWritten (reason)                      // NOT generated, and why
```

Each direction is an `Option`, because "absent" and "hand-written" are different
facts and collapsing them puts a reason on a function nobody wrote. Six of the
Worktree types have a conversion in one direction only — nothing accepts a
`GitOid` or a `BranchName` FROM Haskell — and recording those as
`HandWritten("…")` would claim a hand-written counterpart exists. `None` says
what is true: no such conversion, in either column.

**Generated** for the Worktree slice: the four identity conversions
(`IdentityRaw`), `DirtyPolicy` and `InProgressKind` (`VariantMap` — note
`InProgressKind` renames every variant, `Merge` → `InProgressMerge`, which is
precisely the mechanical-but-error-prone case).

**Hand-written, with the reason recorded in the schema:**

| Conversion | Why it stays hand-written |
|---|---|
| `worktree_id_from_wire` | the rejection must become a DOMAIN error (`WorktreeNotRegistered`); only the check is generated |
| `git_failure_receipt_to_wire` | `PathBuf` → lossy `String`, `Option<i32>` → `Option<i64>` |
| `receipt_to_wire` | field renames (`worktree_id`→`tree_id`, `created_at_ms`→`created_at`) plus a `PathBuf` |
| `spec_from_wire`, `worktree_source_from_wire` | compose a FALLIBLE conversion; the error path is semantic |
| `error_to_wire` | a ten-arm map between two error vocabularies, several with different field arities |

`HandWritten(reason)` is not decoration. Today the split between "this
conversion is trivial" and "this conversion carries a decision" exists only in a
reader's head. Recording it in the schema is what lets a later lane see, without
re-deriving it, which conversions it may safely regenerate.

**Where the generated adapters live:** `tidepool-handlers/src/generated/`, not
the schema crate and not `tidepool-bridge-effects`. The schema stays a leaf
(PRD open question 2's leaning, already confirmed in §3.1): it carries Rust PATH
strings, exactly as `RustBinding::Path` already does, and those paths resolve at
the consuming crate. `tidepool-bridge-effects` does not gain a dependency on
`tidepool-worktree`, which is the property that lets test mocks in low crates
keep importing the wire types.

### 11.6 What does NOT change, and where the Haskell slice lands

`Tidepool.Records.Bridged` is untouched. It is generated from the six
`CoreRecord`-deriving Rust structs (`Proc`, `Hit`, `FileMeta`, `Commit`,
`StatusEntry`, `FileDelta`) through a DIFFERENT mechanism, and those types are
cross-effect result records rather than effect-scoped vocabulary. §3.6's ruling
stands: folding that module into the schema is a later lane.

So the capability is stated carefully. It is *"generate a Haskell declaration
slice from the same ordered field list that produces the Rust wire struct"*. Where
the slice LANDS is decided by which mechanism owns the type today:

- **effect-scoped vocabulary** (every `Wt*`, `Ev*`, `Ag*` type) → the effect's
  own `type_defs`, emitted by `decl_rs`. This is Worktree's whole slice, and it
  is what Event and Subagent reuse.
- **cross-effect result records** (the six above) → `Records.Bridged`, still
  emitted by `CoreRecord`. Unchanged by this lane.

Also unchanged, and worth restating because the durable-format lock depends on
it: **nothing in `tidepool-worktree` is generated.** The registry entries,
binding records, journal entries and monitor state are durable JSON on operator
machines. Their serde names, field order and every persisted byte are frozen.
The wire types the generator emits carry no serde at all — they cross to Haskell
through `ToCore`/`FromCore`, not through JSON — so the durable formats are not
in the generator's blast radius by construction. §11.7 proves that rather than
asserting it.

### 11.7 Proof obligations, in the order they are discharged

Extending §5's three classes with the two this lane adds. Nothing is deleted
before every item below is green.

**Class D — durable formats (NEW, and captured FIRST).** Before a line of
generator code exists: canonical sample values of every durable
`tidepool-worktree` type (`WorktreeReceipt`, the registry record and its status,
`WorktreeOrigin`, the binding table's records, journal entries, monitor state,
`WorktreeError` and its payload types) serialized and pinned byte-for-byte in
`tidepool-worktree/tests/`. Captured from the LIVE hand-written types on
unmodified trunk. `tidepool-worktree` is a quick-tier crate (not in
`.config/nextest.toml`'s `default-filter` exclusion set), so this guard runs on
`cargo nextest run` — the §4 placement rule, satisfied.

These goldens are the answer to a question the golden itself cannot beg: they do
not prove the migration is safe, they prove the migration did not touch what it
claimed not to touch. Same discipline as §5's Class A, applied to disk instead
of to the wire.

*Captured, and the surface turned out to be narrower than "receipts and registry
entries" suggested.* `tidepool-worktree/tests/durable_formats.rs` establishes by
grep of every `serde_json::to_*`/`from_*` call site — not by assumption — that
there are exactly THREE durable roots:

| Root | On disk as |
|---|---|
| `WorktreeReceipt` | one JSON file per id, `<registry_root>/records/<id>.json` |
| `Vec<Binding>` | one JSON file per id, the worktree's full lease history |
| `JournalEntry` | JSONL, one object per line, appended to the event journal |

Seven other serde-deriving types in the crate are reachable by a reader's
intuition but never by a write site (`WorktreeSummary`, `Observed<T>`,
`SubscriptionId`, and — the one worth naming — `WorktreeError` and its three
payload types). `WorktreeError` crosses to Haskell through `ToCore`, never to
disk. It is pinned inline anyway, because it is the type this lane's error ADT
regenerates and a cheap pin on a non-durable type costs nothing; but the
distinction is recorded so a later lane does not mistake the pin for a durable
contract it must preserve.

> **A finding the Event lane needs, and Worktree did not have.** `JournalEntry`
> embeds the DOMAIN `RepositoryEvent` / `HeadChangeReceipt` / `CommitReceipt`
> as durable JSONL. So the Event mirror retirement carries a durable-format
> lock that the Worktree one does not: its domain types are on disk, with serde
> names and variant tags that are a persisted contract. Worktree's wire types
> touch no durable byte and that is why this lane's Class D goldens are a
> non-regression check; for Event they will be a genuine constraint on what the
> generator may emit. Capture them before that lane starts, not during it.

**Class E — wire-struct identity (NEW).** Type-level assertions that the
generated wire types are the same types the hand-written ones were: for each,
the field/variant NAMES and ORDER, the Rust types, the derive set, and the
`#[core(name = …)]` mapping. Written against the generated module, and read
side-by-side against the deleted hand-written block in review.

The load-bearing one is ORDER, and it is asserted positionally rather than as a
set — a permutation is exactly the failure the positional comment was standing
guard against, and a set comparison would pass through it.

**Class A (unchanged, and the acceptance moment).** `worktree_decl()` is already
in `protocol_goldens.rs`'s pinned list, so `effect_decls.txt` and
`effects_module.standard.hs` already carry every Worktree byte, captured from
the hand-written registry in phase 1. They are re-asserted after the flip
WITHOUT regeneration. Untouched goldens green against generated output is the
byte-compatibility result; regenerating them at that step destroys the proof.

**Class B (unchanged).** The reviewed diff, in §7's form, extended with the
wire-emission diff classes.

**Class C (unchanged).** Every new `HsType` the Worktree slice needs must already
render byte-identically in `hs.rs`'s coverage table — it does; `[Text]`,
`Maybe Int`, `Maybe GitRef`, `[WorktreeSummary]` are all present.

**Pre-flip equivalence (§9 step 3).** `worktree_decl_matches_the_schema_exactly`
joins `protocol_schema_equivalence.rs` while `worktree_effect_def!` is still
live. Green there is the go-ahead; nothing is deleted before it.

**Missing-handling-class-fails-to-compile** holds for every Worktree verb, as it
does for every verb — `HandlingClass` is a required field, and all five Worktree
verbs are `OuterDispatch(Worktree)`.

### 11.8 Order of work

1. Class D goldens, from unmodified trunk. Nothing else starts first.
2. Schema vocabulary + the three emitters + their quick-tier tests, in
   `tidepool-protocol`. No consuming crate changes yet. **← steps 2 and 3 are
   done; see §11.9 and §11.10.**
3. `effects/worktree.rs` — the Worktree effect described in the schema.
4. Pre-flip equivalence green against the live macro.
5. Flip: generated modules in, `worktree_effect_def!` deleted, the `Wt*`
   hand-written block and its positional comment deleted, adapters rewritten
   onto the generated boundary constructors.
6. Re-assert Class A without regenerating; Class D unchanged; full verify.

**Step 4 cannot start yet, and the reason is §11.9.** Steps 2 and 3 landed with
every proof green, but the helper census turned up a blocker this section did not
anticipate. Do not attempt the flip before reading §11.9.

**Step 1 is done** — `tidepool-worktree/tests/durable_formats.rs` plus its
`goldens/durable/` fixtures, captured from the live hand-written types before any
generator existed. Steps 2 and 3 were built in parallel with it rather than
strictly after, which was safe only because lane 3 changed no file outside
`tidepool-protocol/`: the schema's wire types carry no serde at all, so the
durable formats were outside the blast radius by construction (§11.6). **That
stops being true at step 5**, which rewrites the adapters and is the first change
that could touch a persisted byte. Class D is the baseline it is checked against;
do not regenerate it there.

### 11.9 Helper representability — the finding this section did not anticipate

§11 was written as if the record vocabulary were the hard part. It was not. The
thirteen declarations, the seven `ToJSON` instances and the eleven-variant error
ADT all render byte-identically to `worktree_effect_def!` on the first try, and
the wire structs come out field-for-field identical to the hand-written `Wt*`
block. The HELPERS are the hard part, and §11 never looked at them.

`worktree_effect_def!` carries **fourteen** helpers (§11's prose implied fewer),
all fourteen using the `{ raw [...] }` escape hatch. §3.4's rule is that a thin
wrapper over one verb is representable and everything else stays hand-written
OUTSIDE the contract. Applied honestly, that rule admits **three of fourteen**.

| Helper | Verdict | Why |
|---|---|---|
| `createWorktree` | **Representable** — `HelperBody::Pointfree` | `send . WorktreeCreate`, exactly the point-free form |
| `lookupWorktree` | **Representable** — `HelperBody::Pointfree` | `send . WorktreeLookup` |
| `listWorktrees` | **Representable** — `HelperBody::NullaryLiftEither` (NEW) | `send WorktreeList >>= liftEither`. A shape, not a body: `liftEither` is named once in Rust and the derived sig drops the `Either` |
| `fromCurrentRepository` | Not representable | Pure `WorktreeSpec` constructor application, no verb. Needs a closed Haskell EXPRESSION language |
| `fromRef` | Not representable | Same, with a nested constructor (`SourceRef r`) |
| `fromWorktree` | Not representable | Same, nested constructor over a call to another helper (`worktreeId h`) |
| `allowDirtySnapshot` | Not representable | Record UPDATE (`s { specDirtyPolicy = … }`), no verb |
| `worktreeBranch` | Not representable | `send (WorktreeBranchOf (worktreeId h)) >>= liftEither` — adapts its ARGUMENT through a pure projection, so its parameter type is not the verb's arg type and the sig stops being derivable |
| `worktreeHead` | Not representable | Same shape as `worktreeBranch` |
| `worktreeId` | Not representable | Field-projection chain (`h.handleReceipt.treeId`), no `send` at all |
| `renderWorktreeId` | Not representable | Identity unwrap by pattern match |
| `renderGitOid` | Not representable | Identity unwrap |
| `renderBranchName` | Not representable | Identity unwrap |
| `renderWorktreeError` | Not representable | Ten arms of string formatting over `show` / `T.intercalate` / `T.strip` / `length`. This is a program, not a shape |

**Only one new schema feature was added, and it is a shape.**
`HelperBody::NullaryLiftEither` renders `v = send Ctor >>= liftEither` and
derives the signature with the `Either` consumed. The point-free and applied
`liftEither` variants were deliberately NOT added: the only two helpers that
would want them (`worktreeBranch`, `worktreeHead`) also adapt their argument, so
they stay unrepresentable either way, and adding unexercised variants would be
speculation rather than a reviewed feature.

**Nothing was smuggled in.** The alternative on offer was a closed Haskell
expression AST — `App`/`Var`/`Con`/`FieldAccess`/`RecordUpdate` — which would
cover the four spec builders, the record update, the projection chain and the
three unwraps. That is not a shape; it is the raw hatch with an intermediate
representation, and it would still not reach `renderWorktreeError`. It was
rejected.

**And that is the blocker.** `haskell/lib/Tidepool/Worktree.hs` is a pure
re-export module: it `import Tidepool.Effects (…)` naming **all fourteen** helper
names and re-exports them. So the eleven cannot simply be dropped from the
schema's `helpers` slice — the emitted `Tidepool.Effects` would stop defining
them, that import list would fail to compile, and the Class A
`effects_module.standard.hs` golden would move. The flip is blocked until the
eleven have a home.

**The recommendation, for the lane that does step 5.** Move the eleven into
`haskell/lib/Tidepool/Worktree.hs` as DEFINITIONS instead of re-exports, and give
Worktree an `extra_imports` row pointing at it. This is exactly the
`Tidepool.Shell` arrangement Exec already uses, and §3.6 already rules that the
Haskell library layer *consumes* the contract rather than mirroring it — these
eleven are library code that happens to live in the wrong file. The edit is
mechanically local: that module's import list loses eleven names, its body gains
eleven definitions, and its export list does not change at all. The Class A
golden moves in that same commit, visibly, which is the whole reason the output
is committed. What the flip lane must confirm: that nothing else imports these
names from `Tidepool.Effects` directly, and that the eval preamble reaches them
through the new `extra_imports` row.

The alternative — teach the schema a Haskell expression language — is a much
larger lane and buys nothing this one needs. If a future lane wants it, the four
spec builders and the three `render*` unwraps are the closed subset worth
starting from; `renderWorktreeError` never belongs in a schema.

### 11.10 As built — where §11 was wrong, and what was added beyond it

Everything §11.2–11.6 specified was implementable as written except where noted.
Six corrections and additions, so a later lane inherits the design that exists
rather than the one that was sketched.

1. **A `Named` field type's Rust spelling had no stated source.** §11.2 gave
   `RecordField { hs_name, rust_name, ty }` but never said how `specSource ::
   WorktreeSource` becomes `spec_source: WtWorktreeSource`. It resolves through
   `Effect::wire_rust_of`, which asks the `TypeDef` that DECLARES the type for its
   `wire_name`. There is deliberately no second string: a type from another
   mechanism (a `CoreRecord` bridged record) cannot appear in a generated wire
   struct, and asking for one is a generation-time panic.
2. **`DomainMap`'s directions are `Option`.** Corrected in §11.5 above.
3. **`JsonInstance::Object` must cover every field.** Corrected in §11.3 above.
4. **`Validation::Segment` is byte-oriented.** Corrected in §11.4 above.
5. **The `#[core(name)]` rule is a function, not a convention.**
   `TypeDef::needs_core_name()` is true exactly when the Rust name differs from
   the Haskell name AND the shape is not a `Sum`. §11 stated the rule in prose;
   it is now the only thing the emitter consults, which is why all nine Worktree
   structs carry the attribute and none of the three enums does.
6. **`WireDerives` renders in canonical rank order, not authored order.** A set
   is a set; the rendered line must not depend on how someone listed it. The
   order `ToCore, FromCore, Clone, Copy, Debug, Default, PartialEq, Eq,
   PartialOrd, Ord, Hash` reproduces all three derive spellings in the live `Wt*`
   block, which is the check that it is the RIGHT order and not merely a
   consistent one. `WireDerives::validate` also rejects the sets that would be a
   compile error in the emitted file (`Copy` without `Clone`, `Ord` without `Eq`)
   — a much worse place to learn about it.

Three additions §11 did not call for:

- **`IdentityPayload`** is a closed two-variant enum (`Text`/`Int`) rather than an
  `HsType`, so an identity cannot be declared over `[Text]`. Event's `EventId Int`
  and Subagent's `AgentId Int` land on the `Int` arm directly.
- **`effects::all_described()`** — the test-only view including effects described
  but not yet flipped. `effects::all()` still drives generation, so a described
  effect proves and renders without emitting a file into a crate whose
  hand-written copy is still live. This is the mechanism that let lane 3 ship the
  capability without touching a consuming crate, and lane N reuses it.
- **Wire and adapter files are gated on content, not on an allowlist.**
  `all_files` emits a wire module only when `type_defs` is non-empty and an
  adapter module only when some entry has a `DomainMap`. Exec and Journal
  therefore get neither, with no effect-specific special case anywhere.

One structural consequence worth naming: **`tidepool-handlers/src/generated/` has
one mod-index and `handler_rs` owns it**, so it lists `<eff>_adapters` alongside
`<eff>`. Two generators cannot each own a directory index. Its header text is
byte-stable across this lane by construction — an effect gains its adapter line
there when it is flipped, not before.

### 11.11 `WorktreeSpec`'s meaningless states — declined for this lane, with the reason

An external type review (2026-08-17) landed mid-lane on the domain
`WorktreeSpec { source, label, dirty_policy }`: its fields are public, and
`dirty_policy` is meaningful only for the `CurrentRepository` and `Worktree`
sources. A caller can build `{ source: Ref(r), dirty_policy: AllowDirtySnapshot }`
and `resolve_source`'s `Ref` arm ignores the policy outright — it `rev-parse`s
the ref and returns `snapshot_ref: None` without ever consulting it
(`tidepool-worktree/src/create.rs:256-268`). A configuration layer can therefore
report that it requested a snapshot while actually seeding the named ref. The
proposed shape was a domain SUM — `Current { label, dirty }` |
`Existing { id, label, dirty }` | `Ref { reference, label }` — with the
generated wire type staying a permissive product and the adapter converting.

**The finding is real. The domain sum is declined for this lane.** Not on blast
radius — that turned out to be small (the only production read of the product
outside `create.rs` is this lane's own `spec_from_wire`; the three reads in
`handlers/agent.rs` are inside its `#[cfg(test)]` block). Declined on what the
refactor would actually accomplish:

**A domain sum alone relocates the drop; it does not close the lie.** Behind the
sum, `spec_from_wire` still receives the pair `(Ref, AllowDirtySnapshot)` from
the wire and must do one of two things with it. Silently discard the policy —
which is today's behavior, now localized to one adapter, tidier but with the
operator-visible lie fully intact. Or reject it — which is a genuine fix, and
requires an error variant that does not exist, which means a new wire variant,
which moves the Class A golden. The pair is constructible from authored Haskell
today (`allowDirtySnapshot (fromRef r lbl)` typechecks and succeeds), so
rejecting it is a semantic change to a live contract.

**So the effective fix is a WIRE change, not a domain change** — either that new
error variant, or the Haskell `WorktreeSpec` itself becoming a sum so
`allowDirtySnapshot (fromRef r lbl)` stops typechecking. Both move a byte-locked
golden, and this lane's whole claim is that it moved nothing it did not
deliberately move. Landing a semantic change inside a byte-compatibility
migration would make any post-flip failure ambiguous between the two, which is
the specific thing an effect-at-a-time migration is structured to avoid.

Two things done instead of nothing:

1. **The tolerance is now pinned.** No test covered `Ref` + `AllowDirtySnapshot`
   at all, so the behavior was real, deliberate, documented in one code comment,
   and completely unasserted — a later change to it would have been invisible.
   `tidepool-worktree/tests/dirty_snapshot.rs` now asserts that the two policies
   produce identical results on the `Ref` path, `snapshot_ref: None` included.
   The follow-on lane changes that assertion in the same commit as the fix,
   which is what makes the fix visible in review.
2. **The architecture the review asks for is what this lane builds.** §11.5
   already lists `spec_from_wire` and `worktree_source_from_wire` as
   `HandWritten`, precisely because their error path is semantic. The generated
   permissive wire product and the hand-written richer domain form, converted at
   one explicit adapter, IS the split. A domain sum drops in behind that adapter
   later without touching a wire byte.

**For the follow-on lane, so it inherits the shape and not just the complaint:**
make the Haskell `WorktreeSpec` a sum. That is the version that makes the
meaningless pair unrepresentable at the surface an author actually writes,
rather than at an internal boundary the author never sees. It costs a
`type_defs` change, a `helpers` change, a Class A golden move, and a
`Tidepool/Worktree.hs` edit — which after this lane's flip is **one schema edit
plus one adapter**. That is PRD 22's acceptance line, on a real example, and it
is a fair test of whether the migration bought what it claimed.
