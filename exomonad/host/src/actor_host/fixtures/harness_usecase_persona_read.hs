personaBefore <- PersonaContext.getContext
let personaFields = PersonaLens.toListOf PersonaContext.referencedTexts personaBefore
let personaChosen = [target | target <- PersonaContext.textTargets personaBefore, PersonaContext.targetText target `elem` personaOriginals]
display (length personaChosen == 5)
