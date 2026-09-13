# Wave 4: connected prepared execution

Baseline: `08669ea2896d061fa9e7a00a7f32c5e2ca77182f`.
This wave is not production cutover. Compile closed programs once, instantiate
immutable tops per invocation, execute connected strict forms, observe bounded
results without forcing, and release the invocation. Wave 7's final Core-code
removal is intended as a dogfooding assignment for the new swarm.

## Contracts and order

1. Prelude: schema 5 carries global dead-end evidence (no successful result at
   saturation, not a language-error classification). Dead-end entry signatures
   retain arguments and use an empty result vector; the explicit evidence is
   what distinguishes them from ordinary zero-result entries. Character atoms
   become target-width words; scalar tag 3 is retired, not reused. Operations
   intern by name and signature. Internal identities use namespace local and
   deterministic collision-free spelling, reserved before target filtering;
   external identities and retained-generation matching stay exact.
2. Compile-once owner: all entries declared before definitions, pinned code and
   descriptors, constructor observation metadata, static bytes and stack maps.
   Admit closed programs only, no thunk RHS or unsupported application/operation,
   including nested bodies. Fail with typed errors and offending IDs.
3. Static image: immutable top constructors/functions, internal managed
   relocations, exact-start bitmap and per-invocation top table. Validate every
   managed edge stays inside the image. Instantiate fallibly and publish only
   after complete relocation. Collector admits static starts with valid tags;
   nursery-to-static edges remain unchanged. No scan of immutable static fields.
4. ABI and lowering: internal multi-results with Cranelift implicit sret; host
   caller area only at platform boundary. Status controls payload publication.
   Register every live managed SSA component at safepoints. Fold analyses over
   the flat arena; use a worklist for emission. Implement Return, exact direct
   Call, evaluated Enter, classified Case, Constructor/Function Let, joins and
   Jump. Reserve recursive groups once, then initialize all siblings without
   host calls before publication. Updated Enter is an integrity error this wave.
5. Nonforcing observation consumes a rooted result vector (empty is valid),
   constructor identity plus logical reps from the compiled owner, and a shared
   node budget. Unobservable kinds and budget exhaustion are typed failures;
   partial output cleanup is stack-safe. Delete superseded sketches as real
   owners land. No managed host arguments or escaping invocation pointers.

## Seeds and delegation

Lead commits types, signatures, hardest path and acceptance tests before Luna
implementation. Search `wave4:PARCEL` task markers. Unfinished paths must not be
admitted into execution; no successful placeholder values. Investigation and
exact-command tooling need no artificial scaffolding. No wave-4 TODO remains
at the review boundary. Main agent writes this plan and semantic decisions.

Luna High, fresh context, three workers maximum, exclusive file ownership,
one build slot. Short briefs point to seed SHA and marker with exact acceptance.
Two unsuccessful attempts escalate to Terra with findings. Prelude projection
edits serialize; independent readers/tests may run together. Builds use the
repository dev shell. Do not touch untracked generated example artifacts.

## Acceptance

Prelude: duplicate and suffix-colliding internal names; stable full/target
identities; dead-end saturation/prefix checks; character constructor/case reps.
Bottoming imports project and validate but do not execute in this closed wave.
Connected: mixed results beyond registers, live caller across callee GC and
live results across later GC; call/case/let through one real adapter; recursive
allocation without intervening host calls; zero fast-path allocation host calls.
Static: cyclic image, nursery-to-static collection, escaping edges rejected,
bad static starts/tags rejected, no partial publication on instantiation failure.
Observation: distinct identities sharing a family-relative tag, deep small-stack
decode, cycles exhaust budget, functions reject, partial-result cleanup safe.

Compile changed targets and workspace tests; run focused checks then
`just changed 08669ea2896d061fa9e7a00a7f32c5e2ca77182f`, `just fixtures-check`,
format checks and `git diff --check` at fold. Record actual failures and exact
commands. Push review checkpoint only after connected execution is demonstrated;
never claim workspace green or production parity from narrow checks.

## Trial record

Count lead preparation/seed tool rounds, assignment, review and correction
messages contemporaneously per parcel. Worker rounds/outcome and disagreements
are separate. Token usage is unavailable unless explicitly exposed by harness.

| Parcel | Lead preparation rounds | Assignment | Review | Correction | Worker rounds | Outcome |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| P: schema/projection | 3 | 0 | 0 | 0 | 0 | seed in progress |

Closing summary pending: shapes handled cleanly, corrected, unsuitable for
delegation, with one-line reasons. No reconstructed token or cost estimates.
