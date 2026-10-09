# Typed build, test, and release delivery

## Scope and decision boundary

This plan tracks the remaining delivery obligations for the approved typed
build/test migration originating at `22083007685652cad1df368c2fb19119405d72db`
(main `b3082780a1e1`). The original implementation parcels and checkpoint
history are retired; Git preserves them. Keep this plan open until the full
migration gate below is satisfied.

Keep release milestones distinct. M1 is production embedded sequential host
and browser acceptance. M2 is production concurrent execution, publication,
capture, and worker lifecycle acceptance. M3 adds the integrated recursive
harness, complete performance evidence, and live browser/TUI trial. A component
test, source inspection, older bundle, or M2 pass does not qualify a later
bundle or milestone. The exact frozen descriptor and executed reports own each
claim.

## Current release gates

Use [the package qualification guide](../build/package/README.md) as the
authoritative procedure, roster, deadlines, bundle contract, and report schema.
Do not duplicate its detailed case descriptions here. The release owner must
freeze and qualify one matched bundle containing the runtime, compiler,
catalog, source entry, and browser assets. Required work remains open until the
exact candidate has passed:

- M1 browser and embedded-host cohort, M2 descriptor-owned production cohort,
  and the prepared-child cohort required by each new prepared release.
- Catalog consumer gate in a fresh environment with checkout and Buck inputs
  absent; catalog inspection or self-consistency is not consumer acceptance.
- M3 `m3-recursive`, both `harness-performance` workloads, and the live
  Tailscale browser/TUI trial described by the package guide. Retain M3 evidence
  separately from M2. Do not infer performance completeness from behavioral
  completion: missing or ambiguous joins, attribution, queue observations,
  phase coverage, cleanup, or identity leave measurement incomplete.
- Final matched qualification and durable acceptance annotation with source
  and artifact identities, exact executed counts, exit status, reports, and
  retained logs. Never edit a frozen bundle or transfer acceptance across
  descriptor identities.

The supplementary performance cohorts do not block M2 acceptance, but they
are required for M3. No result currently recorded in this plan substitutes for
the fresh candidate's pending gates.

## Remaining performance comparison

[Compiler and harness reuse delivery](compiler-harness-reuse.md) owns the
performance hypotheses, implementation parcels, and measurement dimensions.
Complete its matched comparison through the production HTTP/Harness/actor/
compiler route with only provider replies scripted. Compare first preparation,
immediate startup, durable startup, and first/repeated notebook calls. Run
three final repetitions with matched source, descriptor, and profile. Compare
2/8/16 foreground and preparation compiler-job allowances at one worker-process
count; report realized ready width, throughput, and memory. Keep fast-dev and
production results separate. Report diagnostic/request volume and allocation
or GC evidence when available; do not convert absent observation into zero or
claim a speedup from architectural changes alone.

The frozen qualification owner exposes the independent
`--foreground-jobs` and `--preparation-jobs` selections for sealed
`owned-resident` execution. Confirm each report records the requested
allowances and one worker process, and that the production observer shows the
realized grants and width. A selected value or requested width alone is not
evidence that the runtime granted it.

## Full migration gate

The first qualified server is not the end of the repository migration. Close
these obligations on the joined revision and record reproducible evidence:

- Every retained suite has an authoritative discovery/execution owner and
  nonzero counted execution where applicable. Every positive fixture has a
  typed, generated, or production issuer; no test-only authority factory or
  mutable fixture cache substitutes for production ownership.
- Joined Harness runtime, generated schemas, browser assets, and Tidepool pins
  match. Generated artifacts have declared producers and consumers. Fixture
  edits rebuild their producer and dependent tests without unrelated relinks;
  Rust test edits do not regenerate unchanged fixtures.
- Superseded generators, fixture registries, compatibility paths, test
  frameworks, and runner/selection policies are removed after their consumers
  migrate. Retained failure and historical rejection cases remain meaningful;
  missing authority continues to refuse.
- Declared builds and tests do not rely on ambient compiler daemons, mutable
  source writes, or undeclared inputs. Run relevant broad acceptance on the
  joined revision; retain commands, source/artifact hashes, actual counts,
  status, and logs.
- Collect the cold/warm A,A,B,A phase comparison, allocation/GC observations,
  and artifact sizes required by the compiler/harness reuse plan. Separate
  measured results from structural expectations; this campaign does not delay
  first qualified-server delivery.

## Build and evidence procedure

Follow [the repository build guide](../docs/swarm-builds.md) and
[contributor verification rules](../AGENTS.md). Use provisioned checkouts,
pinned tools, the admitted build workflow, and local-only Buck execution unless
the exact target has accepted remote evidence. Workers compile affected
production and test consumers early. Run independent work concurrently when
measured process peaks fit enclosing cgroups and host headroom; coordinate
actual checkout, output, configuration, resource, or frozen-input conflicts.
Record a concrete constraint and exact pending command when deferring a check.
Source-only work remains unqualified until the relevant checks execute.
