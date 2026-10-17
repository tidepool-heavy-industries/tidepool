personaRefreshStatus <- C.inspectContextTransaction personaRefresh
currentPersonaContext <- C.getContext
personaInitialContext <- C.getInitialContext
personaStartupReference <- case personaInitialContext of
  C.InitialContextPublished _ _ _ reference -> pure reference
  _ -> error "persona startup publication fixture is missing"
let personaCommittedBodies = [body | C.Text (Just reference) _ body _ <- view C.contextBlocks currentPersonaContext, reference == personaStartupReference]
nativePersonaReference <- case [reference | C.Native reference C.CompletedExchange _ _ _ <- view C.contextBlocks currentPersonaContext] of
  reference : _ -> pure reference
  [] -> error "native completed exchange fixture is missing"
nativePersonaActivation <- PersonaContext.activate nativePersonaReference personaBody
display ((case personaRefreshStatus of { C.ContextTransactionCommitted receipt -> C.contextReceiptChanged receipt; _ -> False })
  && personaCommittedBodies == [personaBody]
  && nativePersonaActivation == Left PersonaContext.NotAuthoredText)
