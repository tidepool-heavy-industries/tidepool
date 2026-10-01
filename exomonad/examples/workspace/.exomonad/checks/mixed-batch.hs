{-# LANGUAGE QuasiQuotes #-}
data MixedResult = TextResult Text | NumberResult Int deriving (Show, Eq)
let textBranch = withContext (selected id) (withLifetime ActorOwned $ coding @Text projectHead (assignment [label|text-worker|] ("Inspect text" :: Text)))
let numberBranch = withContext (selected id) (withLifetime ActorOwned $ coding @Int projectHead (assignment [label|number-worker|] ("Inspect numbers" :: Text)))
let mixedPlan = (,) <$> projectWorkChildWith @Bool "text" (\_ -> WorkProgress [] []) TextResult textBranch <*> projectWorkChildWith @Text "number" (\_ -> WorkProgress [] []) NumberResult numberBranch
Right mixed <- unfoldWorkBatch (batch "typed-batch" "mixed") mixedPlan keepWork
let ((_, textResponse, textProgress), (_, numberResponse, numberProgress)) = routedMembers mixed
