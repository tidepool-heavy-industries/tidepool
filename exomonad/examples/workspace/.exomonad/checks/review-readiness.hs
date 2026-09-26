{-# LANGUAGE QuasiQuotes #-}
let campaign = "review-readiness" :: CampaignLabel
let workerLabel = [label|candidate|]
let task = Task (batch campaign "submission") "plans/component.md" sourceHead
      "Submit a candidate" "Check the terminal source receipt"
      ["candidate source"] "read exact source" []
(worker, updates) <- unfold (taskGroup task)
  (childWithProgress @WorkProgress @(Outcome Candidate)
    (coding projectHead (assignment workerLabel task)))
readiness <- followWork [("candidate", worker, updates)] (notifyReviewReady me)
