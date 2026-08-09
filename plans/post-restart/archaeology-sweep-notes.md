# Archaeology sweep — inventory and triage

Lane note for the comment/doc-comment/test-name sweep across the runtime
family (tidepool-codegen except `jit_machine.rs`, tidepool-runtime,
tidepool-heap, tidepool-harness, tidepool-repl, tidepool-effect). Scope:
comments, doc comments, and test names only — zero behavior/signature/logic
change. Excluded: `jit_machine.rs` (parallel refactor owns it), `CLAUDE.md`
files, `plans/` deletions, `tidepool-runtime/src/session/resident.rs`'s
pending/ChildSuspended-carrying functions (`run_child` through `classify`,
roughly lines 330-822 — another lane is active there).

## Method

1. `grep -rnE '^\s*//' <crate>/src --include='*.rs' | grep -oE '\b[A-Z][0-9]{1,2}\b'`
   restricted to comment lines, then hand-filtered: legitimate domain tokens
   (register names, GC generation counters, primop type suffixes like `I64`)
   are NOT archaeology and are left alone. Only internal wave/review-ID codes
   (e.g. `L7`, `D7`, `A3`, `C2`, `component K`, `F5`, `M3`, `E1`/`E2`-as-review-tag,
   `Finding N`, `repo-review-<date>/...`) are in scope for deletion/condensing.
2. `grep -rlE "used to|previously|the old |no longer|this replaces"` for
   historical-narration prose (headers and inline).
3. Per file: condense to present-tense contract, move genuinely load-bearing
   history to a regression test name or this note, delete pure archaeology.

## Per-crate raw hit counts (comment-line label tokens, pre-triage)

| Crate | Files w/ label hits | Files w/ "used to"/"previously" |
|---|---|---|
| tidepool-codegen (excl. jit_machine.rs) | 24 | 20 |
| tidepool-runtime | 9 | 8 |
| tidepool-heap | 2 | 2 |
| tidepool-harness | 12 | 4 |
| tidepool-repl | 5 | 7 |
| tidepool-effect | 3 | 2 |

Many raw hits are false positives (register names, type widths, GC generation
labels `G0`-`G3` where that IS the domain vocabulary for young/old-space
generations, not a review ID) and are excluded per-file during the actual
edit pass — the before/after count in the final receipt is measured the same
filtered way, not the raw grep count, so it is apples-to-apples.

## Moved-content destinations

- `tidepool-codegen/src/effect_machine.rs`'s `RootedLocal`/`RootedStack` prose
  (the "Finding 3 fix, repo-review-2026-07-06/01-gc-memory-safety.md" header
  and the forensic-report-length doc comments) condenses to a three-line
  contract; the historical bug (unrooted locals across GC-capable forces in
  `parse_result`'s E arm) lives on as a regression test name in
  `tests/nested_child_gc_rooting.rs` if such coverage exists there, else noted
  here: **prior bug — a bare `*mut u8` local held across `force_ptr` in the
  `E` arm survived a GC that relocated it; `RootedLocal`/`RootedStack` make
  register→force→truncate the default instead of a hand-audited per-site
  ritual.**
- Any other still-load-bearing history moved during the per-module passes is
  recorded in that module's commit message rather than duplicated here, to
  keep this note from re-accumulating archaeology of its own.
- `tidepool-repl` pass: no content needed relocation — every load-bearing item
  found (the `worker.rs` cache-dir self-heal, `session.rs`'s `BUG-7` Prelude/
  verb-plane collision-poisoning hazard, and `session.rs`'s quasiquote
  byte-offset corruption hazard in `push_verbatim_binding`) had its invariant
  already stated in prose alongside the label/date being removed, so the
  present-tense comment left behind carries the same warning in place. See
  commit range `559c9246..6b4af3b4` for the per-file rationale.
- `tidepool-runtime` pass (commits `ba6088c1..b433e480`): every load-bearing
  item found had its invariant already stated in prose alongside the
  label/citation being removed, so the present-tense comment left behind
  carries the same warning — no relocation needed, but two are worth calling
  out explicitly since they guard real regressions:
  - `tidepool-runtime/src/diag.rs`'s
    `user_named_result_binding_with_own_span_in_range_is_kept` test: **prior
    bug — a name-based `SCAFFOLD_BINDERS` list used to classify a GHC
    "Relevant bindings include" entry as scaffold-noise by matching its
    binder NAME; a legitimate user binding named the same as a scaffold
    binder (e.g. `result`) was wrongly dropped from the rendered diagnostic.
    Classification is now by the entry's own `(bound at file:line:col)` span
    against the user's line range — never by name — so a user name can never
    collide with scaffold-sounding names again.** The doc comment dropped
    only the "the old `SCAFFOLD_BINDERS` list" phrasing; the invariant itself
    (span-only classification) is stated in the comment left behind.
  - `tidepool-runtime/src/session/turn.rs`'s `DECL_TEMPLATE_SOURCE`-adjacent
    comments used to cite a deleted `binders.rs`'s `wrap_decls` as the origin
    of the decl template's 17-extension pragma block. That function no
    longer exists anywhere in the tree (Rust or Haskell side), so the
    byte-identical-to-`wrap_decls` claim was already unverifiable; the
    load-bearing part — the pragma block's extension list is what makes
    certain lexer/parser-gated syntax (`LambdaCase`, quasiquotes, etc.)
    legal in a bare declaration, and dropping any of them silently narrows
    what a session decl can compile — is preserved via the existing
    `turn_classification_corpus_old_and_new_path_agree` test's `Case` table
    (the `lambda_case_decl`, `quasiquote_decl` entries) and their
    surrounding comment, unchanged in substance.
- `tidepool-harness` pass (commits `9331f1e7..HEAD`, ~15 commits, one per
  file): `R0`/`R1`/`R2` and `v1` are NOT archaeology in this crate — the
  crate's own `tidepool-harness/CLAUDE.md` self-describes as "typed-yield
  session harness (R0 build)" and the code uses `R0`/`R1`/`R2`/`v1`
  consistently as present-tense scope markers ("out of R0 scope", "unsupported
  in v1"), not as internal wave/review-IDs — left untouched throughout. The
  actual review-ID/wave-label hits removed were `W1`, `W2`, `C2`, `S3`, `F1`,
  `F2`, `F3`, `B1` (14 token occurrences across `engine.rs`, `forcing.rs`,
  `harness.rs`, `registry.rs`, `selfharness/driver.rs`, `selfharness/mod.rs`,
  `uiof.rs`), plus dangling citations to plan docs that no longer exist in
  this tree (`02-runtime.md`, `08-wave1-correctness.md`,
  `03-agent-surface.md`, `01/02-runtime.md`, `01/02/03`, `00-scaffold`'s
  contract doc, `09-askuser-form-gui.md`, `plans/harness-r0/…`) — condensed to
  the present-tense invariant the citation was backing, verified still true
  by reading the referenced code. Three items are load-bearing enough to call
  out explicitly (invariants preserved in the comments left behind, only the
  history/label framing dropped):
  - `tidepool-harness/src/selfharness/harness_source.rs`'s module doc: **prior
    bug (empirically hit, not hypothetical) — splicing a harness file into
    the session decl plane instead of importing it as a static module gives
    its types a generation-versioned "home module" that differs between the
    outer harness compile and a separately-compiled nested Agent turn; since
    a `Value`'s constructor id is a stable hash of (defining module, name,
    arity), a value built under one home module case-traps on `resume` when
    the other side expects its own.** Fixed by importing the SAME static
    module from both sides so constructor ids always agree — the doc comment
    dropped only the "confirmed empirically" past-tense framing, the
    mechanism and hazard are unchanged in the comment left behind.
  - `tidepool-harness/src/harness.rs`'s `Harness::run_id` field: **prior bug —
    without a unique per-construction run identity scoping each instance's
    node decl-plane directories, two concurrent `Harness`es sharing one cache
    root (a different process, or a second `Harness` in-process) could
    construct the same node directory and `remove_dir_all` the other's live
    declarations out from under it.** `run_id` scopes every node dir to
    `harness-sessions/<run_id>/node-<id>` so this can't happen; only the
    `(F3 fix)` label was dropped, the invariant is unchanged.
  - `tidepool-harness/src/harness.rs`'s `cleanup_failed_child` /
    `drive_answerer_to_value`: **prior bug (external review flagged) — only
    the success path called `node_done` + `drop_session` on a fork/fanout
    child; a provider/join/log fault inside `drive_answerer_to_value`, or a
    `resume_parent` failure after, propagated via `?` and orphaned the child
    as a live resident session stuck `Running` forever. Separately, cap
    exhaustion used to hard-fail straight out of the same function, which
    leaked a `Running` answerer and wedged the parent.** Both are fixed
    structurally now — `cleanup_failed_child` runs on every fork/fanout error
    path, and cap exhaustion routes through the `handle_cap_exhaustion`
    escalation ladder instead of returning — the doc comments dropped the
    "used to"/"the leak external review flagged" narration but kept the
    mechanism description (what would leak, and what prevents it now) intact.

- `tidepool-codegen/src/host_fns/` pass (commits `8708a2fe..df9133d0`, one per
  file; `cancel.rs`, `streaming.rs`, `primops.rs` needed no changes). Removed
  review-ID/wave-label hits: `T6` (mod.rs, harmonized to the bare `leaf N`
  numbering `machine_state.rs` — out of scope for this lane — already uses),
  `M3`, `L1`, `L2`, `L3`, `L4`, `L8`, `S3-C1`/`S3-C2`/`S3-C3`/`S3-C6`, `Wave
  1.A`/`Wave 1.B`, `component D`/`component K`, plus citations to
  `repo-review-2026-07-06/01-gc-memory-safety.md` and
  `codex-review-2026-08-08.md`. Kept as-is (not archaeology): GitHub issue/PR
  citations (`#325`, `#273`, `#336`, PR #272 — consistent with the
  `tidepool-effect`/`tidepool-repl` passes' precedent of keeping tracking
  numbers), `segment 40` (self-described section title in this crate's own
  `CLAUDE.md`), and `BUG-1`/`BUG-2` in `primops.rs` (the canonical names of
  two still-live, still-tested regressions in `tests/proptest_host_arrays.rs`,
  not a wave label — removing them would sever the comment-to-test link).
  Four items are load-bearing enough to call out explicitly (invariants
  preserved in the comments left behind, only the label/citation framing
  dropped):
  - `tidepool-codegen/src/host_fns/force.rs`'s `heap_force`: **prior bug — a
    JIT call chain returning null without setting `has_runtime_error` (App's
    `null_propagate_block` / `trampoline_resolve`'s defensive paths) could get
    memoized as a thunk's `THUNK_EVALUATED` indirection; a LATER force
    following that indirection dereferences the null pointer — segfault.**
    Fixed by treating a null result the same as the `code_ptr == 0` case:
    record `RuntimeError::BadPointer` and memoize the poison object (never
    null) instead. Covered by
    `test_heap_force_thunk_null_result_is_not_memoized_as_null`.
  - `tidepool-codegen/src/host_fns/force.rs`'s `deep_force`: **prior bug — the
    original loop re-registered every still-pending work item as a GC root on
    every iteration, because a work item was a bare `*mut u8` inside a `Vec`
    that reallocates as it grows; any root registered at a raw address into
    that `Vec`'s backing buffer would dangle across a later `push`.** Fixed by
    giving each work item its own heap-stable `RootedLocal` cell, registered
    once at push and truncated once at pop — this relies on `work` staying a
    strict LIFO stack (push children only after finishing their parent), an
    invariant now stated directly in the function doc rather than in a
    separate "M3" section.
  - `tidepool-codegen/src/host_fns/errors.rs`'s `materialize_message` (Text
    branch, `nf == 3`): **prior bug — this branch read the offset/len Con
    fields without the null guard every other field access in the function
    has; a `Text` value with a null offset field segfaulted inside
    `read_small_int`'s `read_tag` instead of yielding a message-less error.**
    Fixed by guarding null there too. Covered by
    `materialize_message_text_null_offset_field_does_not_segfault`.
  - `tidepool-codegen/src/host_fns/errors.rs`'s `MAX_CALL_DEPTH` /
    `debug_app_return`: **prior bug — the call-depth counter was only ever
    incremented, never decremented on return, so it measured TOTAL calls made
    rather than live nesting; ~20k purely-sequential, non-nested applications
    tripped the same `RuntimeError::StackOverflow` a genuinely 20k-deep
    recursion would.** Fixed by decrementing in `debug_app_return` on every
    return path, so the counter now bounds only concurrently-active,
    unreturned calls. The doc comments on both `MAX_CALL_DEPTH` and
    `debug_app_return` state this contract directly now instead of citing the
    finding that fixed it.
  - Also worth noting (not separately relocated — the invariant is fully
    restated in the comment in place):
    `tidepool-codegen/src/host_fns/gc.rs`'s `verify_heap_post_gc` BLACKHOLE
    capture check documents that `for_each_pointer_field` (in
    `tidepool-heap`, out of scope for this lane) once skipped tracing
    `THUNK_BLACKHOLE` captures, which the fix in that crate closed; the
    check here is now stated as pure defense-in-depth ("a second, redundant
    check … not a gap"), not a live gap description.

- `tidepool-codegen/src/emit/` pass (commits `788876cb..05ab2e9a`, one per
  file except `expr.rs` and `primop.rs` which each got several as the review
  progressed through their sections; `jit_machine.rs`, `effect_machine.rs`,
  and `host_fns/` are out of scope for this lane — already swept or owned by
  a parallel refactor). Removed review-label/citation hits: `Wave 1, component
  C` / `Wave 1.A` / `Wave 1.B` / `Wave-1 miscompile` (`mod.rs`, `expr.rs` —
  session re-entry / `ExternalEnv` docs), `Gap A` (`expr.rs`'s LetRec Phase
  2.5/3c docs — a Var-alias RHS taking the trivially-resolvable eager path),
  `Finding 5` (`apply.rs`'s call-depth merge-block comment), a dangling
  `plans/ghci-implementation-plan.md §2/§3` citation and a dangling
  `plans/stack-safety.md` citation (both `expr.rs`; neither file exists in
  this tree), two `codex-review-2026-08-08.md item 1` citations and one
  `f137d34`-commit-hash citation (all three in `primop.rs`'s `unbox_addr`/
  `unbox_bytearray` address/array-kind guards). Also condensed: `apply.rs`'s
  "characterization commit" narration, `free_vars_index.rs`'s "used to
  compute... copy-and-walk" module header, `primop.rs`'s "used to duplicate
  independently" (`unwrap_boxing_chain`), and `expr.rs`'s stale pre-petgraph
  narration in `topo_sort_deferred_simple`'s docs and its golden-test module
  (the module doc said tests were pinning behavior "BEFORE it is replaced by
  a petgraph-backed implementation" — the replacement had already happened;
  a test comment and a nearby impl comment still described the old iterative
  "sorted"/"remaining" hand-rolled-sort mechanics instead of the actual
  min-heap Kahn's-algorithm implementation). Renamed one test,
  `diamond_ties_break_by_input_order_within_a_pass` →
  `diamond_ties_break_by_input_order`, since "a pass" is not a concept the
  current heap-based algorithm has. Numbered play-by-play comments
  (`// 1. ...`, `// 2. ...`) trimmed in `join.rs` (`emit_join`/`emit_jump`)
  and `case.rs` (`emit_case`). Kept as-is (not archaeology): `#313`/`#313
  t11`/`#325` GitHub-issue and regression-fixture citations (`t11` is a real,
  still-exercised fixture name in `tidepool-runtime/tests/repro/repro313.rs`),
  `M1`/`M2`/`M5` bare parenthetical citations in `case.rs`/`expr.rs` (each
  names a real, still-existing regression test —
  `tidepool-codegen/tests/case_trap_scrut_ptr.rs`,
  `raise_lazy_trivial_guard.rs`, `letrec_field_freevar_deps.rs`, matching the
  host_fns pass's `BUG-1`/`BUG-2` precedent), `proptest_jit_dispatch B2`
  (`primop.rs` — a live classification tag in that suite's own `B1`-`B4`
  verdict taxonomy, not a wave label), and `Phase 1`/`2`/`2.5`/`3a`-`3d`
  (`expr.rs`'s LetRec emission — consistently used, current step-naming
  vocabulary for that one function's algorithm, not a review-ID). One item is
  load-bearing enough to call out explicitly (the invariant is preserved in
  the comments left behind; only the citation/label framing was dropped):
  - `tidepool-codegen/src/emit/primop.rs`'s `unbox_addr`/`unbox_bytearray`
    (and the shared `unwrap_boxing_chain` they call): **prior bug — after
    unwrapping a chain of 1-field boxing-wrapper Cons, the final heap value's
    payload was read directly as an address (`unbox_addr`) or as a
    length-prefixed buffer pointer (`unbox_bytearray`) with no check that the
    value was actually a `TAG_LIT` of an address/array-carrying class. A
    Thunk, Closure, or a Lit of an unrelated class (e.g. a bare `Int#`/`Word#`
    tag word that escaped case dispatch, or a `Text` value where an `Addr#`
    was expected) got its payload bytes reinterpreted as a pointer and
    dereferenced — a real memory-safety escape, not a hypothetical.** Fixed by
    requiring `TAG_LIT` + the correct address/array-carrying `lit_tag` before
    the payload load, trapping cleanly via `runtime_shape_trap` (kind
    `AddrKind`/`ArrayKind`) otherwise. The doc comments dropped only the
    `codex-review-2026-08-08.md item 1` / `f137d34` commit-hash framing; the
    guard mechanism and the hazard it closes are unchanged in the comments
    left behind.

- `tidepool-codegen/src/` top-level-file pass (`old_space.rs`, `layout.rs`,
  `nursery.rs`, `stack_map.rs`, `coverage.rs`, `context.rs`, `signal_safety.rs`,
  `yield_type.rs`, `pipeline.rs`, `lower.rs`, `lib.rs`, `heap_bridge.rs`,
  `datacon_env.rs`, `machine_state.rs`, `alloc.rs`, `debug.rs`,
  `binding_table.rs`; commits `bbd0c488..00c0610e`, one per file that needed
  changes — `layout.rs`, `coverage.rs`, `context.rs`, `lib.rs`, `alloc.rs`
  needed none; `jit_machine.rs`, `effect_machine.rs`, `host_fns/`, `emit/` are
  out of scope for this lane, already swept or owned by a parallel refactor).
  Removed review-label/citation hits: `L8` + a `repo-review-2026-07-06/01-gc-
  memory-safety.md` citation (`nursery.rs`), `D8` (`stack_map.rs`), `D7`
  (`debug.rs`, `pipeline.rs` — lambda-registry incremental-rebuild docs/tests),
  `D9` and a `kimi-r2 #9 rewrite` / `Wave-0 scaffold` citation
  (`binding_table.rs`), `L5`/`M6` + a second `repo-review-2026-07-06/...`
  citation (`lower.rs`), `T6 leaf N` harmonized to the bare `leaf N` numbering
  `host_fns/mod.rs`'s already-completed pass uses (`machine_state.rs`), `M3`/
  `M4` (`machine_state.rs`). Also condensed "used to"/"previously"/"the old
  X"/"this replaces" narration: `heap_bridge.rs`'s `NonConvertibleValue` doc
  (a stale claim about overloading `TAG_FORWARDED`) and `value_to_heap`'s
  "differs from the old recursive version" ordering note; `old_space.rs`'s
  `run_multi_bind`-corruption regression-test doc (see load-bearing item
  below) and a "latent until the Wave-1.B bind path" aside; `datacon_env.rs`'s
  "pre-prune"/"post-prune" narration on the shadowed-binder test; `signal_safety.rs`'s
  "no longer emits bare traps" phrasing (present-tensed, no behavior claim
  lost); `machine_state.rs`'s `install_session_buffer`/`reclaim_session_heap`/
  `clear_gc_state`/`set_gc_state` docs (each cited "the free-fn doc this
  replaces" or "mirrors the old X free fn" — those free functions no longer
  exist in this crate, so the citations were dangling; replaced with
  self-contained present-tense contracts) and its `CURRENT_MACHINE` thread-local
  doc ("matching the correctness the per-thread thread-locals this replaces
  already had"). Kept as-is (not archaeology): `#325` (`lower.rs`'s
  non-Lam-crossing recursive-join test — a real, still-exercised regression
  cited consistently across `emit/join.rs`, `emit/mod.rs`, `host_fns/cancel.rs`,
  `host_fns/gc.rs`, and `tests/external_cancellation.rs`), `proptest_boundary_
  roundtrip B5` (`heap_bridge.rs` — a live classification tag in that suite's
  own taxonomy, `tests/proptest_boundary_roundtrip.rs` still names and asserts
  on it directly), `#329`/`#340` (`machine_state.rs` — also cited in
  `host_fns/mod.rs`'s already-swept doc and `jit_machine.rs`), `segment 40`
  (`machine_state.rs` — same self-described `CLAUDE.md` section title kept in
  the `host_fns/` pass), and `W4`/`self-iterating-harness W4`
  (`heap_bridge.rs` — used pervasively and consistently as the current name
  for the finalize-by-reference mechanism across `jit_machine.rs` (out of
  scope, owned by a parallel refactor), several `tests/realm_*.rs` files, and
  `tidepool-runtime/src/session/resident.rs`; a codename functioning as
  current vocabulary, not a one-off review citation — left alone everywhere
  in this lane's scope for consistency with the excluded file). Two items are
  load-bearing enough to call out explicitly (invariant preserved in the
  comment left behind, only the archaeology framing dropped):
  - `tidepool-codegen/src/old_space.rs`'s
    `test_tenure_same_root_twice_follows_forward`: **prior bug — tenuring the
    same top-level root pointer twice in one call (e.g.
    `(a, b) <- pure (dup, dup)`, both tuple fields the same heap object) hit
    `measure_closure_bytes` returning 0 bytes for the second call's
    already-forwarded root, so `tenure` rooted the raw `TAG_FORWARDED` stub
    directly instead of following the forwarding pointer — the resulting
    slot read back `heap tag: 255` instead of the tenured value. This
    shipped and corrupted `run_multi_bind`.** Fixed by following the forward
    pointer before rooting. The doc comment now states the invariant
    ("tenuring the same root twice must follow the forward, not root the
    stale stub") directly instead of narrating "before the fix"/"after the
    fix"; the regression test itself (name and body) is unchanged.
  - `tidepool-codegen/src/signal_safety.rs`'s fatal-signal-outside-JIT-
    protection handler: **the stderr breadcrumb on this path is load-bearing
    — a silent thread exit here previously presented to the embedder as an
    unexplained HANG (the eval caller's request never returns), which cost a
    full debugging session before `.tidepool/crash.log` was found to be the
    actual signal.** The comment is condensed to state the invariant
    ("without it, a silent thread exit presents as a HANG") without the
    debugging-session anecdote; the `write(2)` call and crash-log path are
    unchanged.

## Uncertain-keep list

Populated during the per-module passes below as items are found where the
signal-vs-noise call isn't obvious. See individual commit messages for the
running list; anything still open at the end of the lane is copied here
before submit.
