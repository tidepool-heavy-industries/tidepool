:{
data BrokenPreview = BrokenPreview Int
instance Show BrokenPreview where show _ = error "preview deliberately fails"
:}
let brokenPreview = BrokenPreview 17
let opaquePreview = (\n -> n + 1) :: Int -> Int
let effectPreview = (do { _ <- actorContext; pure (error "must not execute input") }) :: Eff ActorEffects Int
let textPreview = "first line\nλ second line" :: Text
type AssignmentText = Text
let longTextPreview = T.pack (replicate 2000 'x') <> "\nFINAL-ACCEPTANCE-CONDITION" :: AssignmentText
let oversizedTextPreview = T.pack (replicate 20000 'λ') <> "\nRETAINED-ASSIGNMENT-TAIL" :: Text
