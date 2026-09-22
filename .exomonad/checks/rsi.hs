workEvidence <- observeWork task leadWork
later <- snapshot
let ResponseReady workAnswer = observedResult workEvidence
let Produced (Delivered _ head _) = responseValue workAnswer
let packet = RsiInput head "Human requested: improve the next wave's context packaging." [workEvidence] before later ["The deterministic application slice retains its product gate; live usage remains unmeasured."]
let Right improvementWave = forkGroupLabel "requested-improvement"
let Right improvementLabel = branchLabel "workspace-style"
improvement <- unfold (batch campaign improvementWave) (child (rsiBranch improvementLabel (atRef (GitRef (rsiSource packet))) packet))
