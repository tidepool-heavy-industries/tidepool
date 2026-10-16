personaAfter <- PersonaContext.getContext
let personaAfterFields = PersonaLens.toListOf PersonaContext.referencedTexts personaAfter
let personaFieldKey field = (PersonaContext.contextVisibleTextReference field, PersonaContext.contextVisibleTextSelector field)
let personaPreserved = all (\field -> PersonaContext.contextVisibleTextText field `elem` personaOriginals || any (\other -> personaFieldKey field == personaFieldKey other && PersonaContext.contextVisibleTextText field == PersonaContext.contextVisibleTextText other) personaAfterFields) personaFields
let personaReplaced = all (\target -> length [() | field <- personaAfterFields, personaFieldKey field == (PersonaContext.targetReference target, PersonaContext.targetSelector target), PersonaContext.contextVisibleTextText field == PersonaText.replace "original" "condensed" (PersonaContext.targetText target)] == 1) personaChosen
personaTransaction <- case personaStage of { PersonaContext.ContextStaged transaction -> PersonaContext.inspectContextTransaction transaction; PersonaContext.ContextStageRefused _ -> error "context edit refused" }
display (case personaTransaction of { PersonaContext.ContextTransactionCommitted _ -> personaPreserved && personaReplaced; _ -> False })
