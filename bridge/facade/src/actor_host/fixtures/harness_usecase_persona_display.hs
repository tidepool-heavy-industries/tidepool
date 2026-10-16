display (either (const "source-unavailable") (PersonaText.intercalate "\n" . filter (PersonaText.isPrefixOf "module Tidepool.Agent.Context") . PersonaText.lines . snd) personaSource)
