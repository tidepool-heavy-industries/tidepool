# Spec: R0 acceptance — PRD §11 through the production path

Runs after all segment merges. Tests drive the REAL entry points (built
binaries, real extract, real protocol over loopback) — hand-wired
harnesses are smoke-only (repo rule: tests-drive-real-entry-point).

## The suite (each item = one test, PRD §11 traceability in comments)

**A — typed yield/resume**
1. Program with `returnControl @Verdict q` suspends; protocol publishes a
   hole carrying `q` + pretty-printed `Verdict` (from the sidecar).
2. Ill-typed `resume e` → continuation NOT consumed, hole stays open,
   GHC error returned as the retry prompt; retry with well-typed value
   succeeds.
3. Well-typed answer forces to NF; parent resumes at the suspension with
   the value. A bottom inside the value does not consume (retry works).
4. Polymorphic site → extract rejection naming the site (build-time
   test, lives with segment 10 but re-asserted here end-to-end).
5. Child answerer session inherits parent bindings (child evals
   interrogate a parent-bound structure) and sees only the eval surface.

**C — governance**
6. Fresh instance, `returnControlFork` request → thunk node appears;
   event log + meters show ZERO tokens and ZERO effects for it until a
   forcing event exists (consent integrity — assert literal zero).
7. Unforced node displays effect row, fan badge (exact/bounded/dynamic),
   price class before the force control (protocol-level assert).
8. Operator answers a model-directed hole through the same answer verb
   (interception).

**E — durability**
9. kill -9 mid-suspension → restart → tree reconstructed from the log,
   hole still answerable, answer completes the parent (substitution
   replay).
10. Tampered/mismatched version header → node demoted to
    browsable-history, loudly, no live restore attempted.

**F — deployment shape**
11. Server binds loopback only (socket inspection); protocol verbs all
    reproducible via curl (script doubles as docs).
12. OAuth mode and API-key mode pass the same driving-turn smoke.

## ANTI-PATTERNS

- DO NOT mock the extract or the JIT anywhere in this suite.
- DO NOT assert on incidental strings (GHC error TEXT changes across
  versions — assert on error-class markers the harness attaches).
- Long-running: wire into `scripts/battery.sh` behind the same
  GHC-heavy tier gating as tidepool-runtime's suites.

## DONE

Suite green in battery; the §6 metrics that R0 can measure (interception
latency <2s via SSE test clock; consent integrity zero; answer validity
on the suite's retry cases) recorded in the run log.
