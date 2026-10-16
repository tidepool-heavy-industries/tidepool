import qualified Persona.Context as PersonaContext
import qualified Persona.Orientation as Orientation
import qualified Persona.Memory as Memory
import qualified Tidepool.Agent.Context as C

personaBefore <- C.getContext
Right () <- Memory.replaceBlock Memory.Persona "persona-memory-updated"
personaAfterWrite <- C.getContext
Right personaBody <- Orientation.renderMemory
Right () <- PersonaContext.refresh
personaAfterRefresh <- C.getContext
display (toListOf C.visibleTexts personaBefore == toListOf C.visibleTexts personaAfterWrite
  && personaBody `elem` toListOf C.visibleTexts personaAfterRefresh
  && personaBody `notElem` toListOf C.visibleTexts personaBefore)
