# harness-r0 — typed yield + session tree + observatory skeleton

Exo-driven plan. Root (fable) decomposes/specs/merges; leaves implement.
Each segment dir holds the spec a subagent is pointed at. PRD.md is the
product requirements source (v2 + landed amendments).

> **REPLANNED 2026-07-23:** [`TARGET.md`](TARGET.md) is now the
> orchestration source — calendar phases dissolved into
> spike-then-freeze-then-widen. The segment table below records the
> original wave (all merged except 10); what remains lands via
> [`spike/SPEC.md`](spike/SPEC.md) (ONE opus agent), then the F1/F2/F3
> freezes, then the sonnet widen fan-out. 70-acceptance is absorbed into
> the spike's record-replay golden path + widen acceptance leaves.

## The idea in one paragraph

`returnControl @T "prompt"` suspends a session and publishes a typed hole.
A child agent (or the operator) answers by evaluating `resume e` where `e`
must typecheck at `T` — GHC is the schema validator; an ill-typed answer
does not consume the continuation and the GHC error is the retry prompt.
Children run **on the parent's suspended machine** (fragments against the
parent's heap — total sharing, zero copy), which requires exactly one new
GC invariant (segment 40). Sessions form a tree, governed by explicit
forcing, durable via an effect-substitution event log, rendered by a
Datastar observatory. This is a NEW frontend over the eval substrate —
tidepool-repl is untouched; reusable glue factors out as needed.

## Segments

| dir | what | model | starts |
|---|---|---|---|
| `00-scaffold/` | cabal rename; crate skeletons + contracts | sonnet (rename) / root (contracts) | now |
| `10-extract-pass/` | `returnControl` interception + type sidecar | sonnet | now (independent) |
| `20-engine-residency/` | Retention::Persistent + fragment suspend + session map | opus | after 00 |
| `30-harness-core/` | event log, replay, forcing gates, protocol server | sonnet (opus for replay) | after 00 contracts |
| `40-gc-rooting/` | stowed-continuation GC root + nested child runs | opus + fable review | **after 20** |
| `50-ui-edsl/` | `Ui` ADT (Haskell) + Datastar renderer + D1 tree skeleton | sonnet (fable designs ADT) | after 00 contracts |
| `60-auth/` | provider trait: ChatGPT OAuth + API-key | sonnet | now (independent) |
| `70-acceptance/` | PRD §11 end-to-end suite through production path | sonnet | after all merges |

Merge order: 10/60 as ready → 20 → 30/50 → 40 (last engine merge, fable
adversarial review) → 70. Verify per merge: TARGETED nextest over touched
crates (operator policy 2026-07-23); full `scripts/battery.sh` only at
freeze gates and root-level engine merges.

## Token economics

Root = fable: specs, contracts, merges, review — no implementation. Opus
only where judgment is load-bearing (20, 40, replay leaf of 30). All other
leaves sonnet — specs carry the plan (mechanism, chosen approach,
sequencing, traps); sonnet executes, never plans. Haiku for build watches.
Every editing agent gets worktree isolation (spawn_dev/fork_wave + `merge`
tool, never raw git).

## Locked decisions (do not re-derive; escalate conflicts)

- Same-machine sequential children; **no heap cloning anywhere in R0**.
  Parallel fork (divergent sessions) is R2: honest deep copy, no structural
  sharing (moving collector + destructive forwarding + thunk-update writes
  make cross-session sharing a different-GC-architecture project).
- `returnControl` rides the existing `AskWith` channel — no new Haskell
  effect, no new Haskell package. Haskell additions: helper verbs in
  `effect_defs.rs` + `haskell/lib/Tidepool/Ui.hs`.
- Type capture: extract-side `asks.json` sidecar, site-id threaded by a
  head-swap rewrite (see 10-extract-pass/SPEC.md). No decl-closure
  renderer (DeclLog source text + in-scope introspection cover it). R0
  rejects polymorphic AND function-bearing answer types at extract.
- E4 durability: effect-response substitution replay; log pins
  prelude+extract versions; cross-version → browsable-history only.
- Web: Datastar (official Rust SDK) over axum + maud. Haskell never sees
  the web layer — `Ui` eDSL as data through the effect channel.
- Deployment: plain binary run by hand while testing. No NixOS module, no
  systemd in R0. Loopback bind stays.
- Timeout-yield is permanently excluded from stowable paths (parks a
  thread; ask boundaries are trampoline-clean by construction).

## Status (2026-07-23)

- [x] 00 scaffold (rename + contracts merged)
- [x] 10 extract pass (returnControl/Fork/Fanout interception + sidecar +
      combinator-site recognition)
- [x] 20 engine residency (ResidentSession + registry merged)
- [x] 30 harness core — C1 log + C3 forcing merged; C2/C4/C5 dissolved
      into the spike (see SPEC supersession note)
- [x] 40 gc rooting (nested-child roots + NF-force, adversarial suite
      green incl. GHC tier)
- [x] 50 ui edsl — E1 Ui.hs + E2a renderer merged; E2b tree view lands in
      the spike/widen
- [x] 60 auth (genai + openai-auth provider merged)
- [x] 70 acceptance (PRD §11 suite via record-replay, incl. kill-9
      against the real binary)

**BUILDOUT COMPLETE 2026-07-24** — spike merged, freezes recorded
(FREEZES.md), widen waves A/B/C merged (WIDEN.md). What remains is
dogfood-gated (TARGET §7): operator drives the live system; frictions
and wins decide the next scope.
