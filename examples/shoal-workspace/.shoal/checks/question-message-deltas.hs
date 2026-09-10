let original = Question "q" (DesignQuestion "plan.md" "a1" "old" [] [] [])
let amended = original { questionDetails = (questionDetails original) { questionFinding = "new" } }
let advanced = amended { questionDetails = (questionDetails amended) { questionSource = "a2" } }
let delta questions = workMessage (id :: Text -> Text) (workChange "lane" (ProgressCursor 1) (WorkProgress [] [original]) (WorkProgress [] questions))
let questionMessageChecks = (delta [amended] == Just "lane: +plan.md#q@a1 new", delta [advanced] == Just "lane: +plan.md#q@a2 new", delta [] == Just "lane: -plan.md#q@a1", delta [original] == Nothing)
