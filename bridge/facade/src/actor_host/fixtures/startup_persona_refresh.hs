import qualified Persona.Attention as Attention
import qualified Persona.Context as PersonaContext
import qualified Persona.Memory as Memory
import qualified Tidepool.Agent.Context as C

personaPublication <- C.getInitialContext
personaReference <- case personaPublication of
  C.InitialContextPublished _ _ _ reference -> pure reference
  other -> error (show other)
Right () <- Memory.remember Memory.Persona "persona-memory-updated"
Right (_, personaPrepared) <- PersonaContext.prepare (Attention.Limits 1 16384) "refresh" Attention.continuity
Right () <- PersonaContext.save "refresh-preparation" personaPrepared
Right () <- PersonaContext.activate personaReference personaPrepared
display (all (\outcome -> case outcome of { Attention.Selected _ presentation -> Attention.presentationId presentation == "original"; _ -> False }) (Attention.outcomes (Attention.inspect personaPrepared)))
