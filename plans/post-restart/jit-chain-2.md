# Spec: jit-chain-2 TL (respawn post-restart; successor to jit-chain)

Resumes the JIT-side latency wave from jit-chain's submitted branch + its
handoff notes. Predecessor landed: the classified free-vars index
groundwork (cluster B, partial), batched table ingestion (cluster C), the
second-fragment gate test + invariant extensions, chain instrumentation +
the measured ratio, and cluster A2's items (check its final state in the
submit note).

## Resume first: cluster B from root.jit-chain.cluster-b @ 2bbc583f

- VERIFIED green on that branch: FreeVarsIndex (single forward pass,
  child-before-parent invariant, Rc-shared sets) + a 103k-node exhaustive
  equivalence test vs the old per-site computation; all SEVEN
  free_vars(extract_subtree) sites converted (spec said 8 — it's 7; the
  ~2691 site is not the pattern).
- NOT verified: the petgraph topo_sort rewrite (Kahn over DiGraph, min-heap
  tie-break by NodeIndex, Dfs reachability) — WRITTEN, hand-traced against
  all 5 goldens, NEVER COMPILED. ~10 min: compile + goldens + the 11 emit
  unit tests.
- Traps recorded by the predecessor: FreeVarsIndex is not Copy — closures
  capturing it across mutable reborrows of the session won't borrow-check;
  use plain fns taking the index (already applied at the deferred-fields
  site). The LetNonRec DCE probe (~2041) was deliberately untouched
  (separately instrumented, consumed by jit_machine.rs) — a judgment call
  to revisit, not an omission.
- petgraph dep line in tidepool-codegen/Cargo.toml may need
  `.workspace = true` conform per the batch's centralized convention
  (gen-workspace-deps.py will flag it).
- Structural fact that closes a risk class: EmitSession.tree is &'a
  CoreExpr, immutable for the struct's lifetime; no replace_subtree in
  emit/*. An index built against one tree cannot be consumed against a
  rebuilt one within expr.rs. Do not re-litigate.

## Then, in order

1. **ConTags latent defects** — plans/self-iterating-harness/
   12-contags-staleness-findings.md (implementation-precision writeup):
   the frozen-tags refresh at add_function (+ the Result stuck-at-
   MissingConTags real bug — deterministic today), and the
   by_qualified_name insert_checked guard (last-writer-wins +
   randomized iteration = order-dependent constructor identity).
2. **NurseryExhausted class completion**: cc1a86c0 fixed ONE
   value_to_heap site; the nested-child/stowed-continuation path failed
   again under load. AUDIT every value_to_heap-class allocation on that
   path, extend the gc-retry pattern to all of them. No whack-a-mole.
3. **Prune re-land** (cb1b131d, reverted at 9f2e18a5): exonerated —
   re-land WITH the populated-session gate test, AFTER the poison-run
   verdict is known (allocation-profile shifts change intermittent-bug
   exposure). Receipt line: "suspected on symptom class, not reproduced;
   reverted out of caution during a release window."
4. **Cluster D** (runtime_apply/runtime_tail_apply helpers absorbing the
   ~12-step inline App protocol + duplicated tail path, expr.rs ~580/~2248,
   + per-function FunctionImports cache): the correctness-sensitive one —
   non-negotiable acceptance list: thunk-in-function-position, normal +
   partial application, poison, nested tail calls, null-without-pending-
   tail, cancellation/GC during application. Full differential + harness
   acceptance for this cluster.
5. **Cluster E** (session-compounding): D7 incremental lambda registry
   (jit_machine.rs ~537 / pipeline.rs ~256 — rebuilt-every-run is O(T²)),
   D8 sorted-range stack-map lookup (stack_map.rs ~77), D9
   seed_external_env = referenced VarIds ∩ table (binding_table.rs ~181;
   root-retention stays global).
6. **Cluster F LAST** (D6, GC-critical): declare_env full-env sort per
   branch point (emit/mod.rs ~445). VERIFY Cranelift's
   declare_value_needs_stack_map contract first; proof via targeted
   stack-map tests under GC_POISON/HEAP_VERIFY, never inference.

## Standing rules

Sonnet devs implement, TL reviews at each merge boundary; expensive gates
once at TL level per cluster; petgraph for all graph algorithms (hard
directive); wire/semantics frozen — any differential delta is a bug;
contention rules verbatim in every dev spec; check the tree (not the
inbox) when gated on a quiet child.
