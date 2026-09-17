# Jev notebook microprograms

Design mockup, 2026-09-16. Assume the [single-operation Jev DSL](jev-dsl.md)
exists. This document proposes notebook authoring syntax and concrete programs;
it is not compiled code, an implemented API, or a live execution report.

Follow-on [linked simulations](../jev-integration/NOTEBOOK-SIMULATIONS.md) now run
actual Jev calls for all four programs using synthetic command observations and
selection-driven continuations. They do not execute this Haskell mockup.

The unit of interaction is one model-authored notebook cell containing a bounded
effectful program. It can run a command, inspect structured output with Jev, follow
the selected branch, run another command, and return a compact typed result. The
frontier model sees the final evidence or an explicit reason to resume judgment.
Intermediate command/Jev effects still happen and consume time; what disappears
is the repeated frontier-model round merely to choose the next inspection.

## Mock DSL conventions

`Cmd.argv`, `Cmd.bashCommand`, and `Cmd.withArguments` are existing command-value
constructors. Jev names below are proposed. Domain helpers such as `readNeighborhood`
and `parseDiagnostics` are hypothetical project Haskell, with contracts below.
No helper implies an existing LSP, automatic runtime integration, or new scheduler.

One request record has model, structured state, and questions. Schema records are
interpreted as `Questions` or `Answers`, following the preferred Servant style:

```haskell
data Locate a mode = Locate
  { target :: mode :- Choice (Candidates a)
  }

data Assess mode = Assess
  { supported :: mode :- Noul
  , contradicted :: mode :- Noul
  , disposition :: mode :- Choice NextSteps
  }

data NextSteps mode = NextSteps
  { returnEvidence :: mode :- Option EvidencePacket EvidenceDescription
  , inspectFurther :: mode :- Option ReadPlan ReadDescription
  , consultOwner   :: mode :- Option Consultation ConsultationDescription
  }
```

`Candidates a` is a checked, runtime-sized homogeneous candidate set with an
explicit no-match alternative. `Option payload description` keeps its payload
locally: source handles, command plans, actor endpoints and closures need no JSON
codec. Only descriptions go to Jev. Static alternatives can instead have distinct
payload types, consumed by an exhaustive `NextSteps (Handlers result)` record.

The following pure builders are illustrative syntax:

```haskell
-- Checked once: unique IDs; 1–255 total alternatives including no-match.
options :: [(CandidateId, Structured, a)] -> Either BuildError (Candidates a Descriptions)
choice :: Instructions -> options Descriptions -> Question (Choice options)
noul :: Instructions -> Question Noul

-- Structured records are derived through a record-only encoder, not arbitrary
-- ToJSON: invalid outer scalar/null state is not accepted by this constructor.
record :: RecordEncoding a => a -> Structured

-- The only Jev effect operation; details follow the main design document.
jev :: (Member Jev effects, JevSchema schema)
    => JevRequest (schema Questions)
    -> Eff effects (Either JevError (JevResponse (schema Answers)))
```

For readable examples, `Micro effects = ExceptT Stop (Eff effects)` is ordinary
application composition. `require` lifts a checked pure result; `stop` returns a
typed `Stop`. `infer` below is just error lifting around the one operation, not a
new effect or a special choice/score API:

```haskell
infer request = ExceptT (mapLeft ProviderFailed <$> jev request)

runCell program = runExceptT program
```

`pick policy answer` returns a retained payload or a typed no-match/ambiguous stop.
The application supplies the policy; no universal probability threshold is implied.
`inspectVerdict` checks cross-question consistency and reserved decision branches.
Neither calls a model. Real signatures must preserve request/candidate association.

`capture` runs an existing `Cmd.Command` through the command owner and returns a
bounded typed observation with job, exit status, stdout/stderr, source snapshot,
and completeness. Expected nonzero statuses (e.g. rg no-match, failing tests) are
data. Launch failures, incomplete output, exceeded budget or changed source return
`Stop` with the retained handle. A live job is retained/awaited, never rerun merely
to obtain its output. `within` accounts for command and Jev attempts, output bytes,
and deadline; the numerical limits below are illustrative application policy.

## 1. Follow a failure to its source and discriminating test

Input: a known failing test command plus an inquiry, e.g. “why does a bookmark
move after an insertion?” The cell runs the test, picks the relevant diagnostic,
reads its source neighborhood, chooses an existing diagnostic test, and assesses
the resulting evidence. It returns an exact witness or a compact consultation.

```haskell
runCell $ within (Limits 3 3 24000) $ do
  failed <- capture bookmarkRegression
  groups <- require (parseDiagnostics failed)
  candidates <- require (options (describeDiagnostics groups))

  located <- infer JevRequest
    { model = jevModel
    , state = record (FailureInquiry inquiry failed.summary)
    , questions = Locate
        { target = choice "Which diagnostic directly explains this failure?" candidates }
    }
  diagnostic <- require (pick selectionPolicy located.answers.target)

  source <- capture (readNeighborhood diagnostic.span)
  tests <- require (options (describeTests (availableChecks diagnostic source)))
  selected <- infer JevRequest
    { model = jevModel
    , state = record (FailureContext inquiry diagnostic source.summary)
    , questions = Locate
        { target = choice
            "Which available check best distinguishes stale saved offsets from incorrect edit normalization?"
            tests }
    }
  check <- require (pick selectionPolicy selected.answers.target)
  observation <- capture check.command

  assessment <- infer (assessFailure inquiry diagnostic source observation)
  require (inspectVerdict decisionPolicy assessment)
```

Concrete offered checks might be `bookmark_after_prefix_insert`,
`bookmark_after_suffix_insert`, and `normalize_edit_roundtrip`, each described by
what it exercises, not just its name. They are discovered from an existing test
catalog and paired with real command values; Jev cannot invent a test or command.
The final `Assess` questions ask whether the stated hypothesis is supported,
whether supplied evidence contradicts it, and which continuation fits. A failed
test is evidence, not automatic permission to edit code or declare a root cause.

Three commands and three dependent Jev calls fit one cell. A conventional flow
could require frontier decisions after the failing log, source read, and follow-up
test. This cell returns `EvidencePacket` or `Stop`, with the original evidence refs.

## 2. Find the ownership boundary through two semantic hops

Input: “find where cancellation can prevent a computed reply from being delivered.”
Text search produces references; Jev chooses a useful source window, then chooses
a concrete relationship to inspect. A final judgment selects an evidence bundle.

```haskell
runCell $ within (Limits 3 3 20000) $ do
  hits <- capture (Cmd.argv ["rg", "--json", "cancel|publish|reply", "src"])
  windows <- require (parseSearchWindows hits)
  starts <- require (options (describeWindows windows))
  start <- infer JevRequest
    { model = jevModel
    , state = record (SearchInquiry inquiry windows.coverage)
    , questions = Locate { target = choice "Where should this investigation begin?" starts }
    }
  focus <- require (pick selectionPolicy start.answers.target)

  first <- capture (readNeighborhood focus)
  edges <- require (options (describeEdges (parseObservedEdges first)))
  next <- infer JevRequest
    { model = jevModel
    , state = record (TraversalContext inquiry first.summary [focus])
    , questions = Locate
        { target = choice "Which unvisited relationship best exposes the delivery gate?" edges }
    }
  edge <- require (pick selectionPolicy next.answers.target)
  second <- capture (readNeighborhood edge.destination)

  result <- infer (assessWitness inquiry [first, second])
  require (inspectVerdict decisionPolicy result)
```

The interesting choice is a publication gate over a cancellation-named telemetry
function. A source index or project parser supplies `parseObservedEdges`; Jev does
not generate relationships. In a shell-only version, the second step can select
among actual search hits instead. `readNeighborhood` uses a retained validated path
and range, constructing argv or positional bash arguments, never interpolated
model-generated shell text. An empty candidate set returns a typed gap.

This is the bounded two-hop version of a hylo. A recursive version uses the same
request builders in its coalgebra and a witness assessment in its algebra. Depth,
visited nodes, snapshots and budgets are deterministic. An algebra requesting more
evidence returns a seed to an outer loop; it cannot retroactively extend its children.

## 3. Deliver the right context to a retained small worker

Input: a worker's typed question and a set of accepted decisions/plan branches.
The goal is often to answer from existing project knowledge without waking the
planner. The cell searches local notes, picks the applicable decision, reads its
actual contract and source delta, then chooses between delivering that evidence
and asking for a new semantic decision.

```haskell
runCell $ within (Limits 3 2 18000) $ do
  inventory <- capture (Cmd.argv ["rg", "--json", "retry|acknowledg|dedup", "project-plan"])
  decisions <- require (parseDecisionCandidates inventory)
  choices <- require (options (describeDecisions decisions))
  located <- infer JevRequest
    { model = jevModel
    , state = record (WorkerInquiry workerQuestion acceptedPlanRevision)
    , questions = Locate
        { target = choice "Which accepted decision governs this exact operation and failure condition?" choices }
    }
  decision <- require (pick selectionPolicy located.answers.target)

  contract <- capture decision.readCommand
  delta <- capture (diffFor decision.sourceRevision workerRevision)
  assessment <- infer (assessApplicability workerQuestion contract delta
    NextSteps
      { returnEvidence = option (describeApplicableContract contract)
                                (packetFor retainedWorker contract delta)
      , inspectFurther = option missingEvidenceDescription boundedReadPlan
      , consultOwner = option semanticGapDescription
                              (consultationFor semanticOwner workerQuestion contract delta)
      })
  verdict <- require (inspectVerdict decisionPolicy assessment)
  dispatchContext verdict
```

`dispatchContext` sends an exact evidence packet through an already-authorized
typed endpoint if the accepted contract applies. It returns a consultation if a
shared semantic choice is missing. At this cell's read budget, `inspectFurther`
returns a follow-up plan; it does not silently exceed the budget. This is where
heterogeneous retained payloads matter: a packet, read plan and consultation have
different types, consumed by exhaustive handlers.

The contract can say “receivers deduplicate stable message IDs.” That answers a
local repair question even if its wording differs. “Retry allowed; duplicate
semantics unspecified” instead takes the semantic-owner branch. The supervisor
sees only the latter case; successful delivery returns its receipt, not a claim
that the recipient incorporated the advice. A provider/send failure stays visible.

## 4. Turn a noisy swarm snapshot into one evidence-backed next action

Input: retained agent-tree observations, publications, known responsibilities, and
candidate commits. Select the consequential interaction, verify the source evidence,
then assemble and assess the smallest packet needed by its actual owner.

```haskell
runCell $ within (Limits 2 3 20000) $ do
  focus <- infer (findInteraction swarmSnapshot)
  interaction <- require (pick selectionPolicy focus.answers.interaction)

  left <- capture (showCandidate interaction.leftArtifact)
  right <- capture (showCandidate interaction.rightArtifact)
  annotated <- infer (judgeEvidencePieces interaction [left, right] acceptedContract)
  packet <- require (assembleCandidatePacket evidencePolicy annotated [left, right])

  final <- infer (assessPacket interaction packet acceptedContract)
  require (inspectVerdict decisionPolicy final)
```

`findInteraction` offers enumerated candidate publication pairs with structured
branch descriptions and a no-issue option. `judgeEvidencePieces` returns keyed
Noul/Score records about retained pieces; `assembleCandidatePacket` retains exact
provenance and checks mechanical completeness. `assessPacket` judges whether the
combined packet establishes a local repair, a missing shared decision, or a need
for further observation. None of its questions reads a sibling answer in the same
request: the later call explicitly receives the assembled packet.

This directly extends the [world experiments](../jev-integration/WORLD-EXPERIMENTS.md):
per-piece judgments were cheap but one necessary piece was only weakly favored.
The final joint check tests sufficiency; it is not assumed to guarantee it. An
unsupported packet returns `NeedEvidence` rather than manufacturing certainty.
Unaffected branches continue through their existing deterministic policy.

## What a notebook result should look like

Return an inspectable value, not every intermediate transcript:

```haskell
data CellOutcome a
  = Resolved a EvidenceRefs TraceRef
  | NeedEvidence Inquiry Observations FollowupPlan TraceRef
  | Consult Consultation EvidenceRefs TraceRef
  | Failed Failure CompletedPrefix TraceRef
```

The compact display shows the selected action, source identity, outstanding doubt,
and where to inspect details. Retain commands, observations, submitted questions,
distributions, candidate mappings, usage and selected transitions behind `TraceRef`.
Use existing retention/journal owners rather than creating another log system.
Earlier external effects are not rolled back if a later step fails.

These examples avoid wakes for intermediate interpretation, not all escalation.
The point where a read budget ends, evidence disagrees, a candidate is missing,
source changes, or the selected branch reserves a decision for a supervisor is a
normal typed return. A long-running command may also require a later activation;
one notebook cell is not a promise to finish within a provider tool timeout.

## DSL acceptance questions exposed by these examples

- Can a resident model author the first cell with ordinary record construction,
  one checked dynamic candidate builder, and useful type errors?
- Can selected values retain source handles, commands and typed actor endpoints
  without serialization, request-scope leakage, or string dispatch?
- Can requests/results preserve nested and keyed records while compiling to the
  native flat map, with complete structured descriptions and distributions?
- Can a notebook cell suspend through multiple command/Jev effects while retaining
  local values, then return a small display with inspectable evidence?
- Do failure and partial execution produce useful continuations rather than
  encouraging rerunning the entire cell and duplicating admitted effects?

The first three are DSL design obligations. Resident suspension, command lifecycle,
actor authority and retained values remain with their existing runtime owners.
This plan does not add engine work or change the production effect surface.
