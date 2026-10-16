let personaPatch = PersonaContext.contextPatch [PersonaContext.replaceText target (PersonaText.replace "original" "condensed" (PersonaContext.targetText target)) | target <- personaChosen]
personaStage <- PersonaContext.stageContext personaPatch
display (case personaStage of { PersonaContext.ContextStaged _ -> "staged-five"; PersonaContext.ContextStageRefused _ -> "refused" })
