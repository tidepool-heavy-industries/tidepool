import qualified Persona.Attention as Attention
import qualified Persona.Context as PersonaContext
import qualified Persona.Memory as Memory
import qualified Tidepool.Agent.Context as C
import Tidepool.Aeson.Value (encodeValue)

personaPublication <- C.getInitialContext
personaReference <- case personaPublication of
  C.InitialContextPublished _ _ _ reference -> pure reference
  other -> error (T.pack (show other))
Right () <- Memory.remember Memory.Persona "persona-memory-updated"
Right (_, personaPrepared) <- PersonaContext.prepare (Attention.Limits 1 16384) "refresh" Attention.continuity
Right () <- PersonaContext.save "refresh-preparation" personaPrepared
Right () <- PersonaContext.activate personaReference personaPrepared
personaEmptyBatch <- PersonaContext.judge "empty" []
let Right personaEmptyCollection = Attention.collection []
Right personaEmptyPrepared <- PersonaContext.prepareView (Attention.Limits 1 16384)
  "empty" personaEmptyCollection (Attention.View [] [])
Right () <- PersonaContext.save "empty-preparation" personaEmptyPrepared
Right () <- Memory.note "empty-judgment" (encodeValue (Attention.observation personaEmptyBatch))
display (null (Attention.decisions personaEmptyBatch)
  && null (Attention.outcomes (Attention.inspect personaEmptyPrepared))
  && all (\outcome -> case outcome of { Attention.Selected _ presentation -> Attention.presentationId presentation == "original"; _ -> False }) (Attention.outcomes (Attention.inspect personaPrepared)))
