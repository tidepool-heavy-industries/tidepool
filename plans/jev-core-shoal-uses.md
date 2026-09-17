# Jev as Shoal's semantic control plane

Design mockup, 2026-09-16. This assumes the proposed single-operation Jev effect
and Servant-style mode-interpreted records. It is not compiled code and these
exact requests have not been live-tested. The shapes follow the observed Jev API
and the publisher's vendored guidance.

The intended cadence is not one question per call and not hundreds of artificial
questions. A normal semantic boundary should expose roughly 5–20 useful judgments
over one prepared state. Haskell performs exact queries, graph operations, policy,
and execution. Jev annotates or selects existing possibilities.

## Shared notation

```haskell
data Investigation mode = Investigation
  { mechanism          :: mode :- Choice Mechanisms
  , nextProbe          :: mode :- Choice (Candidates Probe)
  , evidenceSufficient :: mode :- Noul
  , contradicted       :: mode :- Noul
  , verification       :: mode :- Choice (Candidates Check)
  }

-- One and only one effect operation.
jev :: (Member Jev effects, JevSchema schema)
    => JevRequest (schema Questions)
    -> Eff effects (Either JevError (JevResponse (schema Answers)))
```

`Candidates a` pairs a model-facing semantic name and structured description
with a local Haskell payload `a`. Command values, source handles, `AgentRef`s,
and continuations never cross the wire. Choice names below are intentionally
meaningful because experiments showed that alternative keys affect inference.
Equivalent alternatives are coalesced before request construction.

The examples show request bodies for `POST /v1/systemone`. A Choice answer returns
`choice`, `probabilities`, and `confidence`; a Noul returns `noul`; a Score returns
`score`, `legend`, `probabilities`, and `confidence`. Every response is validated
against the exact submitted schema before policy can consume it.

## 1. Investigation turn compiler

One notebook cell gathers diagnostics, references, ownership, recent changes, and
available checks. A single Jev packet chooses the working mechanism, next focused
probe, and eventual verification while independently judging sufficiency and
contradiction.

```haskell
compileInvestigation inquiry = do
  failed  <- capture inquiry.reproducer
  refs    <- lspReferences inquiry.symbol
  changes <- gitChanges inquiry.revisionRange
  checks  <- discoverChecks inquiry.symbol

  probes <- require $ options
    [ candidate "inspect_actor_retry_boundary"
        (ProbeDescription Actor "Inspect retry-to-handler delivery")
        (lspCallHierarchy inquiry.handler)
    , candidate "inspect_inbox_admission"
        (ProbeDescription Node "Inspect whether admission occurs twice")
        (lspReferences inquiry.inboxInsert)
    , candidate "inspect_history_projection"
        (ProbeDescription Shoal "Inspect callback-to-row projection")
        (lspReferences inquiry.rowInsert)
    ]
  tests <- require (options (describeChecks checks))

  answer <- infer JevRequest
    { model = jevModel
    , state = record (InvestigationWorld inquiry failed refs changes checks)
    , questions = Investigation
        { mechanism = choice mechanismRubric
        , nextProbe = choice nextProbeRubric probes
        , evidenceSufficient = noul sufficiencyRubric
        , contradicted = noul contradictionRubric
        , verification = choice verificationRubric tests
        }
    }

  decision <- require (investigationPolicy answer)
  probeResult <- capture decision.nextProbe
  checkResult <- traverse capture decision.verification
  pure (InvestigationStep decision probeResult checkResult)
```

Representative Jev request:

```json
{
  "model": "jev-latest",
  "state": {
    "inquiry": "Why does retry produce a second receiver callback?",
    "diagnostic": "callback count: expected 1, observed 2",
    "observations": {
      "inbox": "message m42 admitted once",
      "actor": "m42 callback observed before and after acknowledgment timeout",
      "projection": "one row inserted per callback"
    },
    "ownership": {
      "inbox": "node",
      "callback_delivery": "actor",
      "visible_projection": "shoal"
    },
    "available_checks": {
      "actor_retry_fixture": "Retries stable message m42 across acknowledgment timeout",
      "inbox_admission_fixture": "Checks only first admission",
      "projection_snapshot": "Renders history without a retry"
    }
  },
  "questions": {
    "mechanism": {
      "type": "choice",
      "instructions": "Which mechanism directly explains the second callback?",
      "criteria": {
        "actor_retry_redelivery": "The timeout retry redelivers stable message m42 to the actor handler",
        "inbox_double_admission": "Two inbox records were admitted",
        "projection_reinvocation": "The projection invokes the actor handler",
        "insufficient_evidence": "The observations do not distinguish these mechanisms"
      }
    },
    "next_probe": {
      "type": "choice",
      "instructions": "Which focused code query best discriminates the remaining mechanisms?",
      "criteria": {
        "inspect_actor_retry_boundary": "Follow callers across retry scheduling and actor handler delivery",
        "inspect_inbox_admission": "Inspect inbox insertion callers",
        "inspect_history_projection": "Inspect callback-to-visible-row projection"
      }
    },
    "evidence_sufficient": {
      "type": "noul",
      "instructions": "Does the supplied evidence establish the mechanism of the second callback?"
    },
    "contradicted": {
      "type": "noul",
      "instructions": "Does any supplied observation contradict actor retry redelivery?"
    },
    "verification": {
      "type": "choice",
      "instructions": "If callback deduplication is changed, which available check most directly verifies it?",
      "criteria": {
        "actor_retry_fixture": "Retries m42 and observes callback count",
        "inbox_admission_fixture": "Checks only initial admission",
        "projection_snapshot": "Has no retry behavior"
      }
    }
  }
}
```

## 2. Semantic traversal of code graphs

Haskell obtains and filters the exact graph. Jev selects a semantic cut through
the current frontier; it does not follow pointers or maintain the visited set.

```haskell
data Traverse mode = Traverse
  { nextEdge              :: mode :- Choice (Candidates Edge)
  , currentNodeIsWitness  :: mode :- Noul
  , frontierHasUsefulEdge :: mode :- Noul
  , missingEvidence       :: mode :- Noul
  }

inspectCoalg seed = do
  graph <- deterministicNeighborhood seed
  let frontier = removeVisited seed.visited (currentEdges graph)
  candidates <- require (options (describeEdges frontier <> [stopCandidate]))
  judged <- infer JevRequest
    { model = jevModel
    , state = record (TraversalWorld seed.inquiry graph seed.path)
    , questions = Traverse
        { nextEdge = choice edgeRubric candidates
        , currentNodeIsWitness = noul witnessRubric
        , frontierHasUsefulEdge = noul usefulFrontierRubric
        , missingEvidence = noul missingEvidenceRubric
        }
    }
  pure (applyTraversalPolicy seed graph judged)
```

```json
{
  "model": "jev-latest",
  "state": {
    "inquiry": "Where can cancellation prevent a computed reply from reaching its requester?",
    "current_node": {
      "symbol": "complete_request",
      "module": "session/supervisor.rs",
      "summary": "Receives worker completion and conditionally publishes a reply"
    },
    "path": ["run_worker", "complete_request"],
    "edges": {
      "publication_gate": {
        "relation": "calls",
        "destination": "publish_if_active",
        "nearby_source": "if !request.is_cancelled() { publish(reply) }",
        "visited": false
      },
      "telemetry": {
        "relation": "calls",
        "destination": "record_completion_latency",
        "nearby_source": "records duration and status",
        "visited": false
      },
      "constructor": {
        "relation": "called-by",
        "destination": "spawn_request",
        "nearby_source": "creates request state before worker launch",
        "visited": true
      }
    }
  },
  "questions": {
    "next_edge": {
      "type": "choice",
      "instructions": "Which unvisited relationship most directly advances the inquiry?",
      "criteria": {
        "follow_publication_gate": "Inspect publish_if_active, which gates reply publication on cancellation state",
        "follow_telemetry": "Inspect latency telemetry with no delivery authority",
        "stop_with_current_witness": "The current node already answers the inquiry",
        "unresolved_no_useful_edge": "No supplied edge or current source can answer the inquiry"
      }
    },
    "current_node_is_witness": {
      "type": "noul",
      "instructions": "Does current_node itself establish where cancellation suppresses delivery?"
    },
    "frontier_has_useful_edge": {
      "type": "noul",
      "instructions": "Does at least one unvisited supplied edge plausibly lead to the delivery gate?"
    },
    "missing_evidence": {
      "type": "noul",
      "instructions": "Would choosing a conclusive witness require source not present in current_node or edges?"
    }
  }
}
```

## 3. Swarm traffic control

For multiple legitimate recipients, independent Nouls express membership better
than one Choice. The request also scores delay cost. Haskell combines these with
hard scheduler policy and authorized actor handles.

```haskell
data Attention mode = Attention
  { relevant              :: mode :- Noul
  , changesNextAction     :: mode :- Noul
  , contradictsAssumption :: mode :- Noul
  , delayCost             :: mode :- Score DelayLevels
  }

data Traffic mode = Traffic
  { recipients :: mode :- Each Attention
  , duplicate  :: mode :- Noul
  , supersedes :: mode :- Choice (Candidates MessageRef)
  }

routeUpdate update = do
  snapshot <- actorSnapshot
  pending  <- retainedRelatedMessages update
  judged <- infer (trafficRequest update snapshot pending)
  let plan = trafficPolicy snapshot judged
  traverse_ queueMessage plan.recipients
  traverse_ wakeAuthorized plan.wakeNow
  pure plan.receipts
```

The flattened wire questions for two candidate actors look like:

```json
{
  "model": "jev-latest",
  "state": {
    "update": "Offset-retaining callers must migrate to stable IDs after edits.",
    "agents": {
      "parser_worker": {
        "assignment": "Implement stable-ID index",
        "next_step": "Update the index writer",
        "waiting_on": null
      },
      "bookmark_reviewer": {
        "assignment": "Review bookmark movement across insertions",
        "next_step": "Evaluate callers retaining offsets",
        "waiting_on": "stable-ID compatibility contract"
      }
    },
    "wake_policy": "Wake now only when delay blocks the next action or risks invalid work.",
    "pending_messages": {
      "m17": "The new index preserves lookup behavior for callers that do not retain offsets."
    }
  },
  "questions": {
    "parser_worker_relevant": {
      "type": "noul",
      "instructions": "Is update relevant to parser_worker's current assignment?"
    },
    "parser_worker_changes_next_action": {
      "type": "noul",
      "instructions": "Would update change parser_worker's next action?"
    },
    "bookmark_reviewer_relevant": {
      "type": "noul",
      "instructions": "Is update relevant to bookmark_reviewer's current assignment?"
    },
    "bookmark_reviewer_changes_next_action": {
      "type": "noul",
      "instructions": "Would update change bookmark_reviewer's next action?"
    },
    "bookmark_reviewer_delay_cost": {
      "type": "score",
      "instructions": "How costly is delaying update until bookmark_reviewer's next normal wake?",
      "criteria": [
        "Background information only",
        "Useful at the next checkpoint",
        "Blocks the recipient's next action",
        "Continuing now risks invalidating work"
      ]
    },
    "duplicate": {
      "type": "noul",
      "instructions": "Does pending_messages already communicate the operational consequence in update?"
    },
    "supersedes": {
      "type": "choice",
      "instructions": "Which retained message, if any, is made obsolete by update?",
      "criteria": {
        "message_m17": "Earlier partial compatibility statement",
        "supersedes_none": "Update adds information without making a retained message obsolete"
      }
    }
  }
}
```

`trafficPolicy` may queue the update for both actors but wake only the reviewer.
Jev never creates authority, sends the message, or changes lifecycle state.

## 4. Evidence-set construction

Jev labels a pool of exact source artifacts. Haskell chooses a nonredundant set,
retains provenance, and assembles the wake packet without generated prose.

```haskell
data EvidenceJudgment mode = EvidenceJudgment
  { relevant      :: mode :- Noul
  , supports      :: mode :- Noul
  , contradicts   :: mode :- Noul
  , diagnosticity :: mode :- Score EvidenceLevels
  }

data EvidenceLens mode = EvidenceLens
  { pieces                 :: mode :- Each EvidenceJudgment
  , primaryWitness         :: mode :- Choice (Candidates EvidenceRef)
  , poolCanResolveQuestion :: mode :- Noul
  , semanticGapRemains     :: mode :- Noul
  }

buildEvidencePacket inquiry artifacts = do
  judged <- infer (evidenceLensRequest inquiry artifacts)
  selected <- require (selectNonredundantEvidence evidencePolicy judged artifacts)
  require (checkMechanicalCompleteness selected)
  pure (packetFromExactArtifacts inquiry selected)
```

```json
{
  "model": "jev-latest",
  "state": {
    "question": "Does the accepted contract permit two visible rows for one retried message ID?",
    "artifacts": {
      "accepted_contract": {
        "kind": "decision",
        "text": "Retries may repeat delivery; receivers deduplicate stable message IDs; duplicate visibility is undecided."
      },
      "actor_trace": {
        "kind": "runtime observation",
        "text": "m42 callback occurred twice around acknowledgment timeout"
      },
      "projection_source": {
        "kind": "source span",
        "text": "history.push(row_from_callback(callback))"
      },
      "unrelated_test": {
        "kind": "test output",
        "text": "initial inbox admission succeeds"
      }
    }
  },
  "questions": {
    "accepted_contract_relevant": {
      "type": "noul",
      "instructions": "Does artifacts.accepted_contract bear directly on question?"
    },
    "actor_trace_supports_mechanism": {
      "type": "noul",
      "instructions": "Does artifacts.actor_trace support retry redelivery as the source of the second callback?"
    },
    "projection_source_connects_visibility": {
      "type": "noul",
      "instructions": "Does artifacts.projection_source connect callback multiplicity to visible row multiplicity?"
    },
    "unrelated_test_relevant": {
      "type": "noul",
      "instructions": "Does artifacts.unrelated_test help decide question?"
    },
    "primary_witness": {
      "type": "choice",
      "instructions": "Which artifact most directly establishes the unresolved semantic boundary in question?",
      "criteria": {
        "accepted_contract": "Accepted decision explicitly leaves duplicate visibility undecided",
        "actor_trace": "Observed duplicate callback mechanism",
        "projection_source": "Source connecting callbacks to rows",
        "unrelated_test": "First-admission success"
      }
    },
    "pool_can_resolve_question": {
      "type": "noul",
      "instructions": "Does the complete artifact pool determine whether the duplicate visible row is permitted?"
    },
    "semantic_gap_remains": {
      "type": "noul",
      "instructions": "Does answering question require a new semantic decision rather than more evidence about current behavior?"
    }
  }
}
```

## 5. Adaptive verification planning

Candidate checks are discovered mechanically. Jev chooses the most informative
one and judges whether observed failures justify expanding verification.

```haskell
data Verification mode = Verification
  { firstCheck          :: mode :- Choice (Candidates Check)
  , patchMatchesFailure :: mode :- Noul
  , regressionRisk      :: mode :- Score RiskLevels
  , broadCheckWarranted :: mode :- Noul
  , evidenceSupportsFix :: mode :- Noul
  }

verifyAdaptively patch = do
  inventory <- discoverChecksFor patch.changedSymbols
  first <- infer (verificationRequest patch inventory Nothing)
  check <- require (pick verificationSelection first.answers.firstCheck)
  result <- capture check
  final <- infer (verificationRequest patch inventory (Just result))
  require (verificationPolicy patch result final)
```

The first and final calls occur at different evidence boundaries. Within each
call, every independently useful question is asked together.

```json
{
  "model": "jev-latest",
  "state": {
    "patch": {
      "changed_owner": "actor",
      "change": "Deduplicate stable message IDs before invoking the callback",
      "reported_failure": "Retry invokes callback twice for m42"
    },
    "available_checks": {
      "actor_retry_fixture": "Retries one stable ID and asserts one callback",
      "actor_delivery_suite": "All actor delivery and retry cases",
      "inbox_admission_fixture": "Only inbox admission",
      "workspace_tests": "Every workspace target"
    },
    "latest_result": {
      "check": "actor_retry_fixture",
      "status": "passed",
      "previously_failed": true
    }
  },
  "questions": {
    "first_check": {
      "type": "choice",
      "instructions": "Which available check is the most focused discriminating verification for the patch?",
      "criteria": {
        "actor_retry_fixture": "Exact stable-ID retry regression",
        "actor_delivery_suite": "Broader owning-subsystem behavior",
        "inbox_admission_fixture": "Different mechanism",
        "workspace_tests": "Maximum breadth with low initial diagnosticity"
      }
    },
    "patch_matches_failure": {
      "type": "noul",
      "instructions": "Does patch intervene on the mechanism described by reported_failure?"
    },
    "regression_risk": {
      "type": "score",
      "instructions": "How broadly could patch alter established behavior?",
      "criteria": [
        "Localized to the exact retry fixture",
        "May affect adjacent actor delivery cases",
        "Crosses actor or persistence contracts",
        "Could affect unrelated workspace behavior"
      ]
    },
    "broad_check_warranted": {
      "type": "noul",
      "instructions": "Given patch and latest_result, is actor_delivery_suite warranted before handoff?"
    },
    "evidence_supports_fix": {
      "type": "noul",
      "instructions": "Does latest_result provide evidence that patch fixes reported_failure?"
    }
  }
}
```

## 6. Semantic stop conditions

Stopping is an explicit Haskell policy over independent judgments, not a magical
Jev `done` bit.

```haskell
data Completion mode = Completion
  { mechanismSupported      :: mode :- Noul
  , contradictoryEvidence  :: mode :- Noul
  , productionConsumerFound :: mode :- Noul
  , ownershipEstablished    :: mode :- Noul
  , contractResolved        :: mode :- Noul
  , reproductionObtained    :: mode :- Noul
  , remainingRiskMaterial   :: mode :- Noul
  , supervisorNeeded        :: mode :- Noul
  }

completionPolicy a
  | a.contradictoryEvidence || a.supervisorNeeded = Escalate
  | not a.productionConsumerFound                 = Continue FindConsumer
  | not a.contractResolved                        = Consult SemanticOwner
  | a.remainingRiskMaterial                       = Continue GatherEvidence
  | a.mechanismSupported && a.reproductionObtained = StopWithEvidence
  | otherwise                                     = Continue DiscriminateHypotheses
```

```json
{
  "model": "jev-latest",
  "state": {
    "inquiry": "Why does m42 create two visible rows after timeout retry?",
    "evidence": {
      "mechanism": "Retry redelivers m42 to the actor handler",
      "production_consumer": "history projection inserts one row per callback",
      "focused_reproduction": "actor retry fixture reproduces two callbacks and rows",
      "accepted_contract": "Receiver callback deduplication required; duplicate visibility undecided"
    }
  },
  "questions": {
    "mechanism_supported": {"type": "noul", "instructions": "Does evidence support the stated mechanism?"},
    "contradictory_evidence": {"type": "noul", "instructions": "Does evidence contain a material contradiction to the stated mechanism?"},
    "production_consumer_found": {"type": "noul", "instructions": "Does evidence identify the production consumer connecting callbacks to visible rows?"},
    "ownership_established": {"type": "noul", "instructions": "Does evidence establish which subsystem owns callback deduplication?"},
    "contract_resolved": {"type": "noul", "instructions": "Does accepted_contract decide whether two visible rows are permitted?"},
    "reproduction_obtained": {"type": "noul", "instructions": "Does evidence contain a focused reproduction of the reported behavior?"},
    "remaining_risk_material": {"type": "noul", "instructions": "Is there unresolved uncertainty that could materially change the next action?"},
    "supervisor_needed": {"type": "noul", "instructions": "Does progress require a semantic decision or reconciliation beyond the local accepted contract?"}
  }
}
```

The call can return `contract_resolved ≈ false` and `supervisor_needed ≈ true`;
Haskell then wakes the semantic owner with exact evidence. It does not keep
searching for facts to answer a question the project has never decided.

## 7. Cheap supervision between expensive awakenings

A coordinator periodically annotates retained worker state. The result identifies
which workers can continue cheaply and which phase transition warrants expensive
attention.

```haskell
data WorkerAssessment mode = WorkerAssessment
  { onAssignment      :: mode :- Noul
  , advancesParent    :: mode :- Noul
  , mechanicallyBlocked :: mode :- Noul
  , semanticBlocker   :: mode :- Noul
  , duplicatesSibling :: mode :- Noul
  }

data Supervision mode = Supervision
  { workers                 :: mode :- Each WorkerAssessment
  , consequentialInteraction :: mode :- Choice (Candidates Interaction)
  , supervisorWakeNeeded    :: mode :- Noul
  }

supervise snapshot = do
  judged <- infer (supervisionRequest snapshot)
  let actions = supervisionPolicy judged snapshot.authorizedActions
  traverse_ queueSteering actions.steering
  traverse_ continueWorker actions.continue
  when actions.wakeSupervisor (wakeAuthorized snapshot.supervisor)
```

```json
{
  "model": "jev-latest",
  "state": {
    "parent_goal": "Make retry delivery idempotent without changing inbox admission",
    "workers": {
      "actor_impl": {
        "assignment": "Implement callback deduplication",
        "latest": "Patch passes actor retry fixture; asks whether duplicate visible rows are permitted"
      },
      "inbox_audit": {
        "assignment": "Verify inbox admits m42 once",
        "latest": "Confirmed one admission with retained trace"
      },
      "projection_probe": {
        "assignment": "Find callback-to-row consumer",
        "latest": "Found one row inserted per callback"
      }
    },
    "accepted_contract": "Callback deduplication required; duplicate visibility undecided",
    "candidate_interactions": {
      "implementation_and_projection": "Actor patch changes callback count observed by projection",
      "implementation_and_inbox": "Actor patch does not modify inbox admission",
      "audit_and_projection": "Both are read-only evidence about different boundaries",
      "no_consequential_interaction": "Workers can settle independently"
    }
  },
  "questions": {
    "actor_impl_on_assignment": {"type": "noul", "instructions": "Is actor_impl still advancing its assigned part of parent_goal?"},
    "actor_impl_semantic_blocker": {"type": "noul", "instructions": "Does actor_impl require a decision not supplied by accepted_contract?"},
    "inbox_audit_advances_parent": {"type": "noul", "instructions": "Does inbox_audit's latest result materially narrow parent_goal?"},
    "projection_probe_advances_parent": {"type": "noul", "instructions": "Does projection_probe's latest result materially narrow parent_goal?"},
    "workers_duplicate_effort": {"type": "noul", "instructions": "Are any workers currently performing materially duplicate work?"},
    "consequential_interaction": {
      "type": "choice",
      "instructions": "Which supplied interaction most affects how the parent should coordinate the workers?",
      "criteria": {
        "implementation_and_projection": "Actor callback deduplication directly changes projection input multiplicity",
        "implementation_and_inbox": "Actor patch and inbox admission ownership",
        "audit_and_projection": "Relationship between two evidence-only tasks",
        "no_consequential_interaction": "No interaction changes coordination"
      }
    },
    "supervisor_wake_needed": {
      "type": "noul",
      "instructions": "Does the current worker state require an expensive supervisor decision now, rather than continued local work or queued context?"
    }
  }
}
```

## Combined resident shape

The seven capabilities reuse the same mechanism; they are not seven effects:

```haskell
shoalTurn task = do
  world0 <- gatherInitialWorld task
  investigation <- jev (investigationRequest world0)
  world1 <- executeInvestigationPolicy world0 investigation

  traversal <- boundedHylo
    (\layer -> jev (evidenceFoldRequest layer))
    (\seed  -> observe seed >>= jev . traversalRequest seed)
    world1.startSeeds

  evidence <- assembleExactEvidence traversal
  completion <- jev (completionRequest world1 evidence)
  disposition <- applyCompletionPolicy completion

  case disposition of
    StopWithEvidence -> verifyAndPublish evidence
    Continue plan    -> executeBounded plan
    Consult owner    -> routeEvidence owner evidence
    Escalate         -> wakeSupervisorWith evidence
```

In practice, the turn should minimize **semantic boundaries**, not maximize the
number of questions in one request. Ask together when questions use the same
available evidence and do not require one another's answers. Start another call
only after Haskell has fetched new evidence, constructed new candidates, or moved
to a genuinely different world-state.
