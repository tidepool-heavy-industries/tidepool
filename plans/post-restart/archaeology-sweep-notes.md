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

## Uncertain-keep list

Populated during the per-module passes below as items are found where the
signal-vs-noise call isn't obvious. See individual commit messages for the
running list; anything still open at the end of the lane is copied here
before submit.
