import qualified Tidepool.Agent.Context as C
Right runtimeFacts <- getRuntimeInfo
display (case (runtimeExecutingSpec runtimeFacts, runtimeStartup runtimeFacts) of
  (Just selected, RuntimeStartupObserved (C.InitialContextPublished initial _ _ _)) ->
    runtimeSpecEntry selected == Just "Persona.Spec.runtimeSpec"
      && runtimeSpecInstall selected == EXPECTED_INSTALL
      && runtimeSpecSource selected == EXPECTED_SOURCE
      && runtimeExecutingGeneration runtimeFacts == Just EXPECTED_GENERATION
      && C.initialContextSpecInstall initial == EXPECTED_INSTALL
      && C.initialContextSpecSource initial == EXPECTED_SOURCE
      && C.initialContextSpecEntry initial == runtimeSpecEntry selected
      && C.initialContextSpecRevision initial == runtimeSpecRevision selected
  _ -> False)
