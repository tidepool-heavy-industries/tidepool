laneEvidence <- observeLane (componentTask RelationProjection) lead
later <- snapshot
let ResponseReady laneAnswer = observedResult laneEvidence
let Preparation partial = responseValue laneAnswer
let packet = RsiInput (candidateCommit partial) "Human requested: improve the next wave's context packaging." [laneEvidence] before later ["The deterministic application slice is preparation; live usage remains unmeasured."]
let Right improvementWave = forkGroupLabel "requested-improvement"
let Right improvementLabel = branchLabel "workspace-style"
improvement <- unfold (batch campaign improvementWave) (child (rsiBranch improvementLabel (atRef (GitRef (rsiSource packet))) packet))
