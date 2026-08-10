# Codex external review — 2026-08-08 (findings ledger + routing)

Four read-only audits by an external model family over the last ~day of
work, prompted from root's suspicion list. Worktree unmodified. One
verification limitation: Rust tests could not run in the review sandbox
(sccache `Operation not permitted`) — environment, not test failures.

Status legend: CONFIRMED = anchored in code; HYPOTHESIS = plausible,
reachability not established. Each item names its owning lane.

## 1. FfiStrlen tag-as-address escape — CONFIRMED latent; ConTags-independent
**Owner: unassigned (codegen hardening dev) · Affects: generic-surface gate**

The `==` case-trap's suspected cause is NOT KnownSymbol (generic impl uses
`datatypeName`/`conName`/`selName` packing ordinary String; no `symbolVal`
path). The real hole is lower: `tidepool-codegen/src/emit/primop.rs:1976`
accepts any raw SSA value without validating raw kind, and for heap values
recursively unwraps any one-field constructor and reads its literal payload
WITHOUT requiring a literal tag (~`primop.rs:2642`). An unboxed tag/word can
reach `FfiStrlen` as an address — `0x1` is consistent.

**Consequence for the generic-surface gate:** the ConTags fix does not touch
this path. A PASSING post-rebase `==` re-run proves the repro moved, not the
defect fixed. The gate outcome is informational either way; the hardening
item below is owed regardless.

Fix shape: `unbox_addr` accepts only explicitly typed address/raw-address
values; require `TAG_LIT` + expected literal representation before loading
an address payload; negative test feeding nullary and one-field non-string
constructors to `FfiStrlen`.

**LANDED (strlen-hardening, 1d3543c6, folded 2026-08-08):** `unbox_addr`
now rejects Raw SSA values without `LIT_TAG_ADDR` and requires the final
payload (after 1-field wrapper unwrap) be `TAG_LIT` with an
address-carrying class, routing failures through a new
`ShapeTrapKind::AddrKind` poison+breadcrumb trap. Covers every
Addr#-consuming primop via the shared helper. The one-field negative test
SIGSEGV'd on pre-fix code (stash A/B) — real memory-safety hole, not
hypothetical. Suite 687/687. **Follow-up also landed**
(bytearray-hardening, 4e2920e9, folded same day): `unbox_bytearray`'s
analogous gap closed with `ShapeTrapKind::ArrayKind`, the con-unwrap
traversal deduplicated into a shared `unwrap_boxing_chain` (final
class-checks kept separate — accepted literal classes genuinely differ),
all array-consuming primops covered, and a second independent stash-A/B
SIGSEGV proof. Suite 690/690. **The tag-as-pointer class is now closed on
both unbox paths** (`unbox_numeric` already had an equivalent guard).

## 2. Compile cache cannot key GHC flags — CONFIRMED gap
**Owner: generic-surface (design input to dev-1's Harness.Prelude profile)**

Cache keys (see `tidepool-runtime/src/cache.rs:26`, compile call at
`tidepool-runtime/src/lib.rs:150`): rendered source, target binder, session
salt, include paths + recursive .hs/.hs-boot contents, extractor
fingerprint. NOT keyed: GHC flags/compilation profile, GHC/package-db
identity, working directory, cache-schema/JIT-ABI epoch.

Source pragmas are safe (part of rendered source). **A Harness.Prelude
compilation profile supplying extensions as command-line flags is exactly
the unsafe case** — neither the request nor the key can express flags, so
flag changes hit stale cache. Either the profile injects pragmas into
rendered source (cache-safe by construction) or the cache key gains a
flags/profile component first.

Also noted: the dedicated cache property suite genuinely distinguishes
hit/miss (not vacuously green); Haskell-verified and lazy-consumption
suites intentionally reuse warm caches and are weak evidence for cold
compilation or flag invalidation.

## 3. Realm registry: invariant is a test receipt, not a production check — CONFIRMED; plus one HYPOTHESIS
**Owner: realm-build**

Per-frame state is present as intended and the rejected-bottom path
correctly keeps the frame parked and rooted. But
`stowed_roots_count() == parked_count()` is asserted only in tests —
registry mutators don't check it. Add a `debug_assert` after every park,
rejected resume, successful removal, re-park, and realm drain.

HYPOTHESIS (reachability not established): parked response streams — their
registry is machine-global and cleared by each run guard; a continuation
holding an unforced streamed-response tail across an external realm park
could reference an ID cleared by another realm's run teardown. Wants a
focused realm test, not assumed a bug.

Also: JSON/time constructor metadata is machine-global; `add_function`
accumulates and rejects conflicting IDs — document as a global-ID invariant
and test with two realm tables.

**Resolution at fold (realm-build, 2026-08-08):** the streamed-tail
hypothesis was a REAL bug — `parked_streams` was cleared on every run
teardown, so a parked continuation lost its StreamId the moment any run
returned. Fixed and pinned red/green; map growth while realms are parked
is the documented steady state, bounded by cycle-scoped machine life.
Domain-constructor isolation pinned (`realm_global_id_isolation.rs`).
**Carried up and DECIDED (root):** the residual envelope divergence
(ConTags/json/time ids machine-global, last-writer-wins, agreement
assumed unchecked — a divergent tag would silently corrupt how a parked
SIBLING realm's continuation reads on resume) is ACCEPTED as a documented
residual for now: divergence requires an extractor id-minting change,
which fires the pinned id-stability tests upstream, so the hole is
double-covered today. (Enumerated 2026-08-09 after "the three pinned
tests" proved citable-but-unresolvable: tidepool-repr::
extend_checked_equivalence::{distinct_ids_sharing_a_qualified_name_collide_regardless_of_input_order,
merge_table_skip_filter_cannot_dodge_the_qualified_name_collision_guard}
+ tidepool-runtime::session_table_qualified_identity — ALL DataConId
qualified-name guards; NONE observes VarIds. The set covers constructor
identity only.) A cheap loud agreement check (envelope-subset tag
checksum verified at add_function/park) is ATTACHED TO STEP 4's work item
— same lane, same files, when the go-signal fires. Not closed silently;
not worth a standalone lane.

## 4. Prefix check does not yet prove the seam's UnhandledEffect promise — CONFIRMED gap in the in-flight design
**Owner: realm-build lane B (step 3) — URGENT, signature changing now**

The realm-prefix branch verifies CALLER-SUPPLIED metadata while dispatch
remains through an opaque monomorphized `H`. If metadata says a tag lies
beyond the shared prefix but the concrete `H` has a handler at that
position, the request can silently reach the WRONG handler — a misroute,
the one outcome continuation-parking-contract.md promises cannot happen. Prefix-checking metadata
alone does not establish "mismatch ⇒ clean UnhandledEffect".

Either require exact handled-prefix equality, or bind authenticated roster
metadata to the actual handler stack and select the correct per-frame
dispatcher dynamically.

## 5. GC walker design sound; falsification narrow — CONFIRMED both ways
**Owner: realm-build**

Not a missing-both-walkers defect: the continuation cell is an external
root slot (collection rewrites it, Cheney traverses the graph); the RBP
walker correctly doesn't see parked Box cells; other frame fields are
Rust-owned or already persistent-root-registered.

But the negative control (F3/F4, tag 221) exercises only captured
constructor chains. Nested/mid-effect continuations, streamed tails,
finalized closures, and binding parks are unfalsified. Add those shapes
before treating the suite as broad GC coverage.

## 6. Extension-list drift across four surfaces — CONFIRMED
**Owner: generic-surface (fold into Harness.Prelude work)**

Analogous extension lists already drift: production pragmas
(`tidepool-mcp/src/preamble.rs:27`), standalone session defaults
(`tidepool-runtime/src/session/render.rs:267`), binder parser profile
(`tidepool-runtime/src/session/binders.rs:21`), generated Effects module
(`tidepool-mcp/src/eval_prep.rs:116`). Only some are compared. Semantic
extensions (OverloadedStrings, scoped variables, defaulting/generalization,
deriving strategies, record-field resolution) can change meaning silently.

Fix: one canonical extension-set definition with explicitly tested per-
surface deltas; the Prelude re-export module and flag profile get a
set-equality test.

## 7. D1's "cheap visitor + hard subset assert" defense does not exist — CONFIRMED absent
**Owner: extract-wave (D1) — spec amended**

`collectUsedDataCons` is a second full unseeded translation
(`haskell/src/Tidepool/Translate.hs:1023`), not a syntax visitor; the
authoritative translation returns `tsUsedDCs` (`Translate.hs:443`); Main
SILENTLY UNIONS the two and proceeds (`haskell/app/Main.hs:348`). The
runLLMTurn/fork rewrite makes seeded and unseeded paths genuinely diverge,
so disagreement is possible. No missing constructor was found today, but
the promised fail-hard defense is absent — and silent under-collection is
the signature of the still-owed garbage-con_tag intermittent.

Required with the D1 fix: walk emitted FlatNode constructor/data-alt IDs
and hard-fail if output metadata omits any; independent reachable-Core
collector; a mutation test deleting one `recordDC` call must fail
extraction.

## 8. Checkpoint state codec: multiple silent-corruption paths — CONFIRMED
**Owner: generic-surface step 7 (GCheckpoint) — this is the pre-audit target list**

Envelope parser is appropriately loud; the state codec is not:

- Outbound generic Rust `value_to_json`, inbound authored Haskell
  `FromJSON` (`tidepool-harness/src/selfharness/state_cross.rs:56`) — not
  inverse by construction.
- `Nothing`, `Just x`, and unit all collapse to JSON null
  (`tidepool-runtime/src/render.rs:127`).
- Nullary constructors → strings, records → `_con`, positional → a third
  shape (~`render.rs:259`); Haskell sum decoding expects `tag` — formats
  not generally inverse.
- `Maybe` decode maps null → `Nothing`, losing `Just ()` / nested
  `Just Nothing`.
- Haskell `ToJSON ()` emits null; `FromJSON ()` accepts only `[]`.
- Depth limits / opaque runtime values become sentinel STRINGS, not errors.
- Malformed scientific components can default to zero.

Any of these can silently alter restored state or falsely implicate an
authored codec pair. GCheckpoint: one typed versioned codec both
directions, loud rejection of unsupported values; golden tests over nested
Maybe, unit, non-finite numbers, mixed constructor sums, recursion,
unknown fields, depth overflow.

## Routing summary

| # | Finding | Route |
|---|---------|-------|
| 1 | FfiStrlen escape | new codegen-hardening dev (unowned); gate-meaning note to generic-surface |
| 2 | Cache can't key flags | generic-surface, dev-1 design input NOW |
| 3 | Registry asserts + stream hypothesis | realm-build |
| 4 | Prefix check ≠ misroute-proof | realm-build lane B, urgent |
| 5 | Falsifier shapes | realm-build |
| 6 | Extension drift | generic-surface |
| 7 | D1 defense absent | extract-wave (spec amended in place) |
| 8 | Checkpoint codec | generic-surface step 7 target list |

## Second pass — engine review (2026-08-08, post-realm-fold, read-only)

Overall verdict: the unified-machine direction is validated ("not an
epicycle: permanent GC-rooted continuations plus per-frame realm state are
the right primitives"); one machine can host the outer + answerer
continuations with separate source capability rows. Tree is "halfway
between prototype and production." Five findings:

**9. HIGH — retryable resume error wedges ResidentSession.**
`resident.rs:671` clears `pending` BEFORE calling the machine, but
`jit_machine.rs:1328` deliberately leaves the continuation intact when
answer NF-forcing fails (so the caller can retry). After that failure:
machine still suspended, `pending` None, `is_idle()` wrongly true, next
turn passes the resident check and panics on the machine's stowed-
continuation assertion. Fix: the machine registry is authoritative —
remove a public pending ID only when the frame was actually consumed; the
current slot path needs the same clear-after-consume ordering.
**Routed: dedicated dev (resident-pending-fix), narrow exception to the
step-4 hold** (ordering fix + regression test only; the registry-only
conversion still belongs to step 4).

**10. MEDIUM — one-compile plan cites machinery Phase B did not build.**
`one-compile-bootstrap.md:25` says render+loop multi-target emission comes
from "Phase B's multi-binder machinery," but Phase B deferred that
writeWholeModuleClosed work (`one-spawn-turn-protocol-phase-b.md:99`) and
the writer still accepts exactly one target (`Main.hs:333`) — so all four
boot compiles still exist. Recommendation adopted: adapt `--all-closed`'s
existing multi-binder loop (`Main.hs:185`) into a STRICT explicit
`--targets render,loop` mode (fail if either target fails; preserve
per-target asks/warnings) rather than inventing multi-target extraction.
**Routed: extract-wave (item 0 premise correction).**

**11. MEDIUM — slot-and-registry exclusion is convention, not machine
invariant.** The legacy slot entry (`jit_machine.rs:1213`) checks only the
slot; a caller can park a realm then invoke a legacy suspendable entry,
creating both suspension kinds; `resume_parked` then panics (:3165).
Production integration must make this structurally impossible —
registry-only ResidentSession (step 4) is the real fix.
**Routed: interim machine-level guard (legacy suspendable entries assert
`parked_count()==0`) to runner-unification's executor seam; structural fix
stays step 4.**

**12. MEDIUM — malformed classification silently becomes executable
semantics.** `turn.rs:887` defaults missing/unknown kind to Expr, drops
non-string binders, defaults malformed binder lists to empty — a corrupted
or version-skewed "bind" verdict becomes a discard bind or expression
instead of an infrastructure error, contradicting phase-b's own loud-
VersionSkew philosophy. Fix: strict deserialization (only decl|bind|expr,
required string-array binders, everything else rejected loudly).
**Routed: dev (strict-classify), grouped with 13.**

**13. LOW/PERF — REPL Auto ignores its own verdict.** `session.rs:808`
pays batch GHC classification then still runs the obsolete decl-probe
cascade, so an ordinary expression can cost classification + failed decl
compile + expr compile. Fix: dispatch directly from a present verdict;
keep the cascade only for the no-verdict degradation path.
**Routed: dev (strict-classify).**

The review's recommended order (pending fix → strict multi-target → seed
deletion → registry-only conversion → one cycle-owned machine → prefix
descriptor bound to cycle runtime) matches the standing plan: items 1-2
route as above, 3 is extract-wave item 0, 4-5 are realm step 4 + PRD 18,
6 is the contract's derive-don't-declare guidance already in force.

## 14. Vendored-Aeson FromJSON sum rejection DOES NOT EXIST — inherited; design decision needed (corrected 2026-08-09)

CORRECTED from an earlier wrong mechanism (a string-literal grep match was
reported as verification — the day's failure class, again): FromJSON.hs has
ZERO TypeErrors. `0c49f0a2`'s `GFromJSONSum 'False` branch routes to a real
working TaggedObject decoder — the compile-time rejection was NEVER
implemented on the FromJSON side (Value.hs has it; the false "compile-time
rejection" comment at FromJSON.hs:153 was INHERITED along with the
duplicated `IsNullarySum` family from the sibling module where it is true).
Pinning test `generic_deriving_337::sum_type_rejected_at_compile_time` is
red — sanctioned.

**OPEN DESIGN DECISION (Inanna/morning):** implement the TypeError on
`GFromJSONSum 'False` (restoring the commit message's claimed guarantee),
or declare non-nullary FromJSON sums supported-but-lossy and retire the
test. A behaviour change either way, not a repair.

**Sanctioned reds are THREE** until their fixes fold:
`mock_stack_matches_production` (mock-derive in flight),
`sum_type_rejected_at_compile_time` (this item),
`qq_fmt_brace_inside_hole_non_string_expr_still_works` (inherited,
toExp Let case commented out, byte-identical at HEAD~1 — unowned).

**Durable findings:** a guarantee whose enforcing test lives in a tier
nobody routinely runs is not enforced; and its companion — duplicate a
mechanism and you duplicate its documentation into a context where it
lies. Open sweep question: what else is pinned only behind
--ignore-default-filter?

### Item 14 addendum (2026-08-09): full-shard red census — SEVEN in tidepool-runtime

With fail-fast truncation lifted, a fresh full shard (881 tests,
--no-fail-fast, cache-consistent A/B'd) shows 7 pre-existing reds. THIS
LIST is the source of truth for "sanctioned" (counts in messages go stale):
1. mock_stack_matches_production (mock-derive in flight)
2. sum_type_rejected_at_compile_time (item 14 design decision)
3. mixed_nullary_sum_still_rejected_at_compile_time (same family/root as 2)
4. qq_fmt_brace_inside_hole_non_string_expr_still_works (toExp Let case)
5. user_union_normalize::user_defined_union_survives_effectful_normalize
   ("missing freer-simple constructor 'Union' in DataConTable" — D2/ConTags
   -adjacent; A/B-confirmed pre-existing on trunk)
6. jit_surface::works_from_json_float (decodeFloat_Int# unboxed-tuple
   primop, self-described extract landmine)
7. stdlib_regressions_02_medium::works_int_prism_floors_not_truncates
Items 5-7 surfaced only because truncation ended — the crate's full red
set had never been seen on any routine path. Morning triage owns 3-7's
routing. HAZARD note: git stash is SHARED across all worktrees on this
box — A/B stashers must push/pop LIFO immediately; prefer diff-to-patch
plus checkout for A/B legs.

### Item 14 decision (Inanna + root, 2026-08-09, pre-flight)

**Symmetric lossless support, both directions.** Today writing a
payload-carrying sum to JSON is compile-banned (Value.hs) while reading
one is quietly allowed (FromJSON.hs) — direction asymmetry nobody chose.
Checkpoints need these types to round-trip, so: support both sides,
losslessly, with round-trip tests; retire the reject-at-compile-time
pinning tests as part of the same change. Owner: checkpoint-persistence
lane (Chain A).

## 15. Zero-method class dictionary culled — extract-pipeline bug (agent-wave, 2026-08-09)

A zero-method class's dictionary-constructor binding is culled by the
extractor while a reference survives — reported as a dangling NVar, not a
GHC error. ConstraintKinds workaround committed and sound (agent-wave
checkpoint); the BUG is unfixed and any zero-method class is exposed.
Owner: extract-wave/spawn-latency territory (morning routing). Repro
context in agent-lanes/receipt-agent-wave-checkpoint.md.

## 16. Two of four standing extractor gates never invoke the extractor (extract-wave, 2026-08-09) — BOX-WIDE RULE

haskell_suite_differential and corpus_report replay FROZEN CBOR (zero
extractor invocations — verified); they are JIT-vs-eval differentials
that pass identically with a catastrophically broken extractor, unless
fixtures are regenerated. Empirically shown: injected extractor fault,
extract-fidelity 30/30 clean, neither fixture gate had a path to it. The
citation was wrong, not the instruments — same class as the pinned trio,
one level up. Honest extractor coverage = extract-fidelity-test (real
pipeline; KNOWN HOLE: fixtures never touch JSON/Aeson — morning fixture
work) + harness acceptance (real extracts, ~1845s).

BOX-WIDE RULE (root-adopted from extract-wave's wave rule): fixture
regeneration under haskell/test/{suite_cbor,corpus_cbor} is a SEQUENCED
ROOT/WAVE-LEVEL ACTION, never an individual lane's call — shared dirs,
in-flight lanes, redeploy-class blast radius, and a booby trap:
haskell/CLAUDE.md warns pruning *_u<n>.cbor drops `compared` below
COMPARED_FLOOR, so naive regeneration breaks the floor it protects.
All regeneration requests route through root (or the extract-wave TL
within its subtree).

### Item 14 second addendum (2026-08-09): three reds UNMASKED by the vacuity sweep

Converting silent-negative skip sites to loud made three previously
vacuous-green tests visibly red — pre-existing (stash-confirmed identical
on unmodified files), newly observable:
8. repro_decl_library_import (2 tests) — GHC "module not loaded",
   session-aware multi-module decl-compile path
9. dogfood_observability first test — same path/signature
Same failure family; likely one root cause on the multi-module
decl-compile path. Morning triage. (derived_sum_shape_equals_its_literal
is FIXED on this tip and leaves the sanctioned list.)

## 17. The Fork row has FIVE hand-maintained mirrors across two crates; the mock-derive fold fixed ONE (extract-wave sweep, 2026-08-09)

Production truth: `fork_decl()` is in `standard_decls()`
(tidepool-mcp/src/effect_decls.rs:261). Extract-wave's verified sweep
found five hand-written copies of the standard effect row spelling it
`… Ask, RunLLMTurn` with Fork omitted:

    tidepool-testing/src/eval_harness.rs:403   EFFECT_NAMES list        — FIXED (mock-derive fold: now derived from standard_decls())
    tidepool-testing/src/eval_harness.rs:486   mock module SOURCE (`type M = Eff '[…]`) — STALE
    tidepool-testing/src/eval_harness.rs:804   same row in a doc comment                — STALE
    tidepool-mcp/src/lib.rs:877                assertion on preamble content            — STALE, will FAIL in the tidepool-mcp shard
    tidepool-mcp/src/lib.rs:887                second assertion, same file              — STALE, same

The lib.rs assertions live in a shard nobody's current gates run, so the
red is INVISIBLE until the centralized verification pass runs that crate.
Fourth instance of the one-definition-N-mirrors family; the fixed list's
own doc comment predicted its own drift while four sibling mirrors sat
unmentioned.

BUG-HUNT SCOPE — state the property, not the target: "no hand-written
copy of the standard effect row survives" (grep the row shape; there may
be a sixth spelling the sweep didn't match). NOT a blind string edit:
adding Fork to the mock module source plausibly needs a matching GADT
decl + stub handler in the mock, or every mock-compiling test breaks.
Owner: the post-centralization verification pass.

## 18. extract-wave's parent-merge resolution dropped three definitions git couldn't see (root repair at fold, 2026-08-09)

The fold gate (`cargo check --workspace --all-targets`) caught three
casualties of the hand-authored resolution in 5992c529 — the same
rename-by-refactor blindness its own doc commit (d704dff7) describes:

1. `LiveTurn` struct + `live_turn()`/`apply_delta()`/`finish_live_turn()`
   — the per-node live-streaming subsystem from their side. The field
   survived, the subsystem didn't. Root repair: REMOVED the orphaned
   field (trunk's drain-and-discard `stream_turn` is the survivor);
   the full subsystem is recoverable at `5992c529^1` harness.rs:376-555.
   BUG-HUNT DECISION: was live-turn streaming meant to land? Zero
   consumers existed on either side (web observatory never wired it),
   so nothing user-visible was lost — but if the observatory wants
   live tokens, restore from that ref rather than rewriting.
2. `HeapSummary` + `Harness::heap_stats` — RESTORED verbatim (consumer:
   `acceptance_lazy_boot.rs`, item 0's own acceptance test).
3. One stale `extract_available()` call in `agent_stack_scoping.rs` —
   converted to `support::require_extract()` per the vacuity sweep.

Workspace check clean after repair. The lesson is item counting: their
receipts said "two semantic merge regressions caught and fixed at fold";
the gate found a third and fourth. A divergent-branch fold's semantic
break count is not knowable from the merge — only from the build.

## 19. External review pass (two codex reviewers, 2026-08-09 post-centralization) — triaged

Reviewer 2 read ed56cb96 (pre extract-wave fold): its topology claims
(extract-wave unmerged/142 ahead, no checkpoint branch) are STALE —
that fold landed at b6180c16. Code-level findings below survive
independent of topology. FIXED IMMEDIATELY (model-facing doc drift,
committed with this entry): Worktree.hs taught read-HEAD-then-register,
reopening the TOCTOU that PRD 19's register-then-reconcile-inside-scope
closed; Event.hs's example used the retired sendMessage spelling instead
of pokeAgent/whenSafe. Both now match the PRDs.

BUG-HUNT QUEUE (new, verified-by-inspection claims — each needs a
confirming test before a fix):
- R2.1 HIGH: WorktreeId (id.rs:14) accepts arbitrary text and flows into
  path components (handlers/worktree.rs:72, registry.rs:202,
  binding.rs:137) — separators/.. can escape managed roots. Also
  create.rs:175 does not enforce worktree_root-outside-repo. Validate
  where wire values become domain values.
- R2.2 HIGH: manager-owned dirty-snapshot path — create.rs:205 hands
  snapshot.rs:74 a nested GIT_INDEX_FILE dir that is never created
  before git read-tree. No coverage exercises dirty capture through
  WorktreeManager::create.
- R2.3 HIGH (= R1.5a): FormAnswer::Unit round-trip — web server
  (server.rs:279-280) coerces any non-object answer to {}, Haskell
  rejects and re-prompts; askUser @() and nullary single-constructor
  types can never complete. Fix transport, keep object-adaptation to
  the legacy flat path only.
- R2.4 HIGH: journal tolerates a malformed final row in memory but never
  truncates it (journal.rs:89) — later appends land after garbage.
  monitor.rs:319 partial reconciliation batch + retained baseline can
  duplicate commits under new EventIds. Needs tail repair + idempotent
  batches. (Adjacent to, not covered by, worktree-wave's mid-row fix.)
- R2.5 HIGH: binding.rs:105 is in-memory check + rewrite — no
  interprocess CAS, two processes can bind one worktree. Single-owning-
  server assumption must become explicit (doc) or enforced (lock);
  binding becomes transactional inside coupled spawn when agent-core
  lands.
- R2.6 MED-HIGH: harness.rs:405 error-coordinate attribution prefers
  Expr on overlapping windows — a Bind error can get the Expr excerpt.
  Compile protocol should carry candidate identity. (The lane disclosed
  window-containment as non-guessing; overlap is the case that breaks
  that claim.)
- R2.7 MED: git.rs:181 drops rename origin paths (stale old path in
  synthetic tree); snapshot.rs:170 treats submodule-status failure as
  no-submodules (fail-open against the crate's fail-loud contract).
- R2.8 MED (= R1.5b): polling defaults disagree — monitor.rs:113 5s vs
  handlers/event.rs:115-117 250ms (20x git traffic). Pick one, derive
  the other.

DESIGN LEDGER (not bugs; next-construction order both reviewers agree
on): (a) withHandler is cooperative polling — a parked waitAgent is not
woken by a commit; event arrival must eventually schedule a resident
cycle via the same runtime-owned wakeup as mailbox arrival (R1.1);
(b) the one-cycle coupled-spawn vertical (agent-core lane 1) is the
missing vertical, then runtime-owned wakeups, then the dev-tree example
becomes a policy program (R1.3, matches the standing Chain B plan);
(c) PRD 14 persistence codec still asymmetric (state_cross.rs) —
checkpoint-persistence-lane.md owns it; PRD 14 is not "complete" until.
