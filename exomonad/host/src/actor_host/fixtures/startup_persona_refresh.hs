import qualified Persona.Context as PersonaContext
import qualified Persona.Orientation as Orientation
import qualified Persona.Memory as Memory
import qualified Tidepool.Agent.Context as C

personaBefore <- C.getContext
Right () <- Memory.replaceBlock Memory.Persona "persona-memory-updated"
personaAfterWrite <- C.getContext
Right personaBody <- Orientation.renderMemory
Right personaRefresh <- PersonaContext.refresh
personaStagedStatus <- C.inspectContextTransaction personaRefresh
personaAfterRefresh <- C.getContext
display (toListOf C.visibleTexts personaBefore == toListOf C.visibleTexts personaAfterWrite
  && toListOf C.visibleTexts personaBefore == toListOf C.visibleTexts personaAfterRefresh
  && (case personaStagedStatus of { C.ContextTransactionStaged -> True; _ -> False })
  && personaBody `notElem` toListOf C.visibleTexts personaBefore)
