{-# LANGUAGE QuasiQuotes #-}
let campaign = "final-handoffs" :: CampaignLabel
let wave = "lanes" :: ForkGroupLabel
let leftLabel = [label|left|]
let rightLabel = [label|right|] :: Label
let task = Task (batch campaign wave) "plans/component.md" sourceHead "Deliver the component" "Exercise typed subtree handoff" ["component source"] "read final source" []
(left, leftProgress) <- unfoldDeferred (taskGroup task) (childWithProgress @WorkProgress @Delivery (withLifetime ActorOwned $ coding projectHead (assignment leftLabel task)))
