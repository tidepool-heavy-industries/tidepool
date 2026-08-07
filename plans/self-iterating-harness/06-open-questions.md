# Deferred & open questions

Everything not settled in the originating design conversation, kept here rather
than answered by inference.

## Deferred (known direction, not built in v1)

- **Harness-determined per-context effect sets.** The goal is that the harness
  code chooses which effects are available per turn/stage. v1 ships a hardcoded
  maximal stack, identical in all contexts. (§03)
- **"Actually-doing-things" Agent effects** — file edits, git, exec, http, … .
  v1 starts with what `tidepool-harness` already has (gui + `fork`). (§03)
- **Live hot-reload of harness code mid-session.** v1 uses runtime restart to
  pick up a new version. Live mid-session resume is a genuinely neat
  possibility, not needed for v1. (§02, §04)
- **State-format migration between harness versions.** Required eventually
  (State is JSON-serialized); mechanism deferred. (§04)
- **"Each harness is a GitHub repo."** Lean on agent competency at PR/iteration
  flow. Future; v1 is local git. (§04)
- **Improver-improves-its-own-harness recursion.** Later; v1 keeps task-agent
  and improver roles distinct. (§05)

## Open (no chosen answer yet)

- **Fitness / eval signal for harness improvement.** Human taste for now; real
  metrics only "at scale". The convergence-vs-cargo-cult question. (§05)
- **Polymorphic / loose-constraint yields.** v1 is monomorphic. A
  parked-polymorphic yield is coherent in principle but a real mechanism stretch
  (freer operations do not let-generalize like a GHCi prompt; parking a rank-2
  value is impredicative-adjacent) and is **in tension with parked-continuation
  efficiency** — the fully-flexible version is "recompile the loop with the hole
  filled", which loses the warm heap. Research fork; nothing here forecloses it.
  (§03)
- **Type-level stage / OODA DSL, servant-style wiring guarantees.** Discussed
  (make bad transitions fail to compile, clean custom errors). Not in v1;
  revisit only if expressing the wiring needs it. (§03, §04)
- **Exact emergency-compaction policy.** The ~80% runtime trigger is the
  backstop for unguarded structural compaction; threshold, target-token count,
  and how the forced `Text` is woven into the next `render` are TBD. (§02)

## Minor — to pin during implementation

- `finalize`'s exact signature (name is LOCKED; shape proposed as `a -> Agent x`).
- `runLLMTurn` / `runLLMTurnWithRequiredResp` exact signatures.
- `fork`'s exact signature (forks `Harness`; splits the agent session).
- `State` serialization details beyond "JSON via `ToJSON`/`FromJSON` bounds".
