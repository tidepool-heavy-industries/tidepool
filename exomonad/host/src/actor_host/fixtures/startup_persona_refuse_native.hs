personaRefreshStatus <- C.inspectContextTransaction personaRefresh
currentPersonaContext <- C.getContext
nativePersonaReference <- case [reference | C.Native reference C.CompletedExchange _ _ _ <- view C.contextBlocks currentPersonaContext] of
  reference : _ -> pure reference
  [] -> error "native completed exchange fixture is missing"
nativePersonaActivation <- PersonaContext.activate nativePersonaReference personaBody
display ((case personaRefreshStatus of { C.ContextTransactionCommitted receipt -> C.contextReceiptChanged receipt; _ -> False })
  && nativePersonaActivation == Left PersonaContext.NotAuthoredText)
