import qualified Control.Lens as Lens
currentPersonaContext <- C.getContext
nativePersonaReference <- case [reference | C.Native reference C.CompletedExchange _ _ _ <- Lens.view C.contextBlocks currentPersonaContext] of
  reference : _ -> pure reference
  [] -> error "native completed exchange fixture is missing"
nativePersonaActivation <- PersonaContext.activate nativePersonaReference personaPrepared
display (nativePersonaActivation == Left PersonaContext.NotAuthoredText)
