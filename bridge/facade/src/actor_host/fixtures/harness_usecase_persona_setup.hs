import qualified Data.Text as PersonaText
import qualified Control.Lens as PersonaLens
import qualified Tidepool.Agent.Context as PersonaContext
personaSource <- readSourceModule "Tidepool.Agent.Context"
let personaOriginals = map (\n -> "persona-original-" <> PersonaText.pack (show n)) ([1..5] :: [Int])
display (either (const "source-unavailable") (const "persona-ready") personaSource)
