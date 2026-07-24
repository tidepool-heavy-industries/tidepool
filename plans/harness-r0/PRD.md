# PRD — tidepool-harness: typed interaction & observatory (v2 + landed amendments)

The v2 PRD text was authored in the proxied design conversation
(2026-07-23) and is requirements-source; `README.md` in this dir is the
orchestration source; the operator wins on everything. Reproduced here
with the post-sizing amendments appended (they supersede conflicting v2
text).

## Landed amendments (supersede v2 where they conflict)

1. **A1 mechanism**: ask-site table REPLACED by extract-side sidecar +
   in-band `typedSite` payload key (see `10-extract-pass/SPEC.md`). Open
   question §10.3 (table versioning) dissolved — the type travels in the
   logged request.
2. **A5/A6 scoped**: NF-forcing applies to data-kinded answers
   (deepseq-style walk, new primitive); function-bearing answer types are
   REJECTED AT EXTRACT in R0 (A6/HOAS deferred to P1; bottoms-under-lambda
   out of scope by construction).
3. **A1 decl closure**: not shipped in R0 — same-machine children inherit
   scope (decls literally in scope; Meta/`:i` introspection); operator
   rendering greps `DeclLog` source text.
4. **E4**: replay = effect-response substitution (log records sources +
   per-effect request/response pairs; header pins prelude+extract).
   Same-version restores live with request-sequence divergence checking;
   cross-version demotes to browsable-history (operator-override rung
   later; sequence check is blind to pure divergence, hence the pin).
5. **C3**: fan-out badge is three-valued (exact/bounded/dynamic);
   dynamic converts to exact at materialization and re-checks forcing
   policy. Per-branch effect rows require stack subsetting — added to
   dependencies, not assumed.
6. **Naming**: cabal package renamed `tidepool-extract`; Rust crate takes
   `tidepool-harness`. Typed verbs: `returnControl @T` /
   `returnControlFork @T` (schema-`ask` unchanged as fast path).
7. **Fork semantics (from heap investigation)**: R0 sequential children
   run on the parent's suspended machine — total sharing by identity, no
   copy. Divergent-session fork is R2 honest deep copy; structural
   sharing across sessions is a different-GC-architecture project
   (moving collector + destructive forwarding + thunk-update writes).
8. **Deployment (operator ruling)**: F1/F3 deferred — plain binary run by
   hand while testing; loopback bind + tailnet reachability stand; NixOS
   module/systemd when worth deploying.
9. **Web stack**: Datastar (official Rust SDK) over axum + maud,
   superseding the §12 htmx default; Haskell-side UI is the
   purpose-specific `Ui` eDSL translated by tidepool-web (Haskell never
   sees the web layer).
10. **R0/R2 boundary confirmed by sizing**: sequential children
    weeks-shaped (the one new invariant: GC-rooting the stowed
    continuation); parallel fork months-shaped. Engine-side R0 critical
    path ≈ 4–6 weeks single-person, swarm-parallelized.
11. **D1 node-scale bar dropped (operator, 2026-07-23)**: real trees
    won't approach 10³–10⁴ nodes — tree view is plain Datastar
    server-rendering with collapse-by-default; no virtualization, no
    Preact-island escape hatch. Revisit only on observed slowness.

## v2 PRD (verbatim requirements reference)

> **Status:** v2 draft — discussion starter. Owner: operator. Builds on
> tidepool-harness spec v0.1.

### 1. Problem
The v0.1 harness can fork typed computations into isolated context
branches, but: (1) the answer channel is still a bespoke protocol
(schema-validated JSON caps expressiveness at JSON Schema); (2) the human
has no seat (no way to steer branches, answer asks, approve spending, see
the tree); (3) the system is invisible (heap, effects, spend,
context-residency unobservable).

### 2. Product thesis
One mechanism solves all three: the typed continuation, held visibly in a
tree, consumable by either user. A suspended computation waiting for `T`
is simultaneously: the model's next task, the human's next form, the
approval gate for the work beneath it, and a node the observatory
renders. One tree of typed holes, four views of it.

### 3. Users
Operator (single human owner-author, Haskell-fluent; web observatory
primary). Calling model (frontier LLM driving sessions and inhabiting
holes; types are this user's UI). In-program models (Llm effect; out of
scope). Single-tenant is a product decision.

### 4. Goals / non-goals
G1 answers are Haskell values type-checked by GHC for model and human
askees. G2 humans participate through type-generated structured UI with
prose as first-class escape. G3 spawning governed by explicit consent
graduating to policy. G4 full system state browsable in a web UI good
enough to live in. G5 headless on a remote box, private-network only.
Non-goals: multi-tenant/public anything, native/terminal GUI as product,
mobile-optimized, replacing general coding agents, marketplace.

### 5. Requirements (P0 unless noted)
**A typed yield/resume**: A1 hole = question + pretty-printed
post-elaboration type + decls [amended: decls via scope/DeclLog]. A2
consumed only by a value of type T constructed in Haskell, validated by
compilation; ill-typed attempts don't consume; GHC error = retry prompt.
A3 model answerers = child sessions inheriting parent heap bindings +
branch context, holding only eval, answering via `resume`; no dedicated
answer tool. A4 monomorphic-after-elaboration or extract-rejected. A5
answers forced to NF before consumption [amended: data-kinded only]. A6
(P1) HOAS answers [amended: extract-rejected in R0]. A7 (P1) answerers
introspect the expected type in-session. A8 (P1) prose-fallback type
extensions are proposed-then-approved, logged, persistent.

**B tidepool-ui** (R1): B1 Dialog effect renders `UI a`, returns typed a;
widgets: choice/text/prose/slider/toggle; products compose; monadic
sequence paginates. B2 every choice an open sum (prose path routes to
calling model as fallback interpreter). B3 `uiOf @T` (type is the
wizard). B4 `[form|…|]` quasiquoter with typed holes checked at extract.
B5 prose-primacy law (prose wins over widgets; resolution shown before
consumption; synchronous). B6 (P1) unfurl/gallery/allOf/firstOf. B7 (P2)
dialog programs storable/versioned; learned constructors persist.

**C governance**: C1 unforced branches are thunks (no session, tokens,
effects); forcing is the only way work begins. C2 `autoForce = never` at
t0. C3 pre-force display: effect row, fan-out + price class, teaser
[amended: three-valued fan badge]. C4 any hole interceptable by the
operator. C5 (P1) forcing-policy ladder by predicate over effect rows;
Ask-bearing and write-bearing rows default manual. C6 (P1) auto-forced
regions keep runaway guards. C7 (P1) teaser-honesty: harness-generated
only.

**D observatory**: D1 live tree (state glyph, row badge, price, teaser;
force/answer in place; usable at 10³–10⁴ nodes). D2 node inspector
(shared-trunk marker + local turns; pending form; effect-log tail). D3
forms render fully in HTML. D4 heap browser (bindings: name/type/size/
generation; lazy stub expansion; eval-in-binding). D5 (P1) trace view.
D6 (P1) meters (tokens, cache-hit, context-residency, asks vs policy).
D7 (P1) interception as first-class UI. D8 (P2) type-generation browser.
UX bar: operator prefers it within a week; live updates; no
token-ambiguous actions.

**E protocol**: E1 everything is HTTP+SSE protocol first (events: node
lifecycle, hole published/consumed, heap deltas, meters; verbs: force,
answer, cancel, eval-in-binding); web UI has no private APIs; snapshots
paginate. E2 curl-able; thin CLI optional. E3 (P1) replayable event-log
format. E4 durable append-only log; unclean restart reconstructs tree;
open holes survive [amended: substitution replay].

**F deployment**: F1 single binary, loopback only [systemd deferred]. F2
tailnet-style private overlay + TLS; reachability = spend authority. F3
nix flake output + NixOS module [deferred by operator]. F4 R0 ChatGPT
subscription OAuth; API-key co-equal.

### 6. Success metrics
Context-residency order-of-magnitude below raw data on demos; ≥90% typed
answers consume by second attempt; hole visible <2s, answerable in-tree;
consent integrity literal zero unforced spend; weekly dogfood retention;
wizard warming (A8, later).

### 7. Rollout
R0 protocol + typed yield (A1–A5, C1–C3, E1–E2, E4, F-as-amended, D1
skeleton): safe, durable, runnable — and ugly. R1 dialog + observatory
(B1–B5, D1–D4, D7, C4). R2 laziness + policy (B6, C5–C7, D5–D6). R3
memory (B7, D8, E3, A8).

### 9. Risks (retained)
Fallback interpreter load-bearing → show resolutions pre-consumption, log
elaborations. Approval fatigue → informative badges, later ladder. Teaser
gaming → C7. Type-extension sprawl → D8 + approval. Provider policy shift
→ API-key first-class. Web surface = spend authority → F2 non-negotiable.
