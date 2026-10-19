import qualified Tidepool.Agent.Context as C
Right runtimeFacts <- getRuntimeInfo
Right runtimeDocs <- currentEffectDocs
display (case (runtimeExecutingSpec runtimeFacts, runtimeStartup runtimeFacts) of
  (Just selected, RuntimeStartupObserved (C.InitialContextPublished initial _ _ _)) ->
    effectDocsInstall runtimeDocs == runtimeSpecInstall selected
      && effectDocsSource runtimeDocs == runtimeSpecSource selected
      && effectDocsGeneration runtimeDocs == runtimeExecutingGeneration runtimeFacts
      && InvocationContextReadWrite `elem` map effectDocumentationIdentity (effectDocsEffects runtimeDocs)
      && InvocationReload `elem` map effectDocumentationIdentity (effectDocsEffects runtimeDocs)
      && ActorEffect EffectCommands `elem` map effectDocumentationIdentity (effectDocsEffects runtimeDocs)
      && not (ActorEffect EffectModelCallOpenAI `elem` map effectDocumentationIdentity (effectDocsEffects runtimeDocs))
      && runtimeSpecEntry selected == Just "Persona.Spec.runtimeSpec"
      && runtimeSpecInstall selected == EXPECTED_INSTALL
      && runtimeSpecSource selected == EXPECTED_SOURCE
      && runtimeExecutingGeneration runtimeFacts == Just EXPECTED_GENERATION
      && C.initialContextSpecInstall initial == EXPECTED_INSTALL
      && C.initialContextSpecSource initial == EXPECTED_SOURCE
      && C.initialContextSpecEntry initial == runtimeSpecEntry selected
      && C.initialContextSpecRevision initial == runtimeSpecRevision selected
  _ -> False)
