:{
data BrokenPreview = BrokenPreview Int
instance Show BrokenPreview where show _ = error "preview deliberately fails"
:}
let brokenPreview = BrokenPreview 17
let opaquePreview = (\n -> n + 1) :: Int -> Int
let effectPreview = (do { _ <- actorContext; pure (error "must not execute input") }) :: Eff ActorEffects Int
let textPreview = "first line\nλ second line" :: Text
