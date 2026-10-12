import qualified Persona.Web as PersonaWeb
import qualified Persona.Memory as Memory
import Tidepool.Aeson.Value (encodeValue)

let personaPage = PersonaWeb.Page "https://example.org/retained" "retained-page" "first passage\nsecond passage"
Right personaSelection <- PersonaWeb.select PersonaWeb.ExplicitOnly "Retain both passages" 1 personaPage
let personaEmptyPage = PersonaWeb.Page "https://example.org/empty" "empty-page" ""
Right personaEmptySelection <- PersonaWeb.select PersonaWeb.ExplicitOnly "Retain both passages" 1 personaEmptyPage
Right () <- Memory.note "web-evaluation" (encodeValue (PersonaWeb.judgment personaSelection))
Right () <- Memory.note "empty-web-evaluation" (encodeValue (PersonaWeb.judgment personaEmptySelection))
display (PersonaWeb.original personaSelection == personaPage
  && PersonaWeb.selected personaSelection == [PersonaWeb.Passage 1 1 "first passage", PersonaWeb.Passage 2 2 "second passage"]
  && null (PersonaWeb.omitted personaSelection) && null (PersonaWeb.uncertain personaSelection)
  && PersonaWeb.original personaEmptySelection == personaEmptyPage
  && null (PersonaWeb.selected personaEmptySelection) && null (PersonaWeb.omitted personaEmptySelection)
  && null (PersonaWeb.uncertain personaEmptySelection))
