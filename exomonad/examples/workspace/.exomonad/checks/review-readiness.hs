{-# LANGUAGE QuasiQuotes #-}
let campaign = "review-readiness" :: CampaignLabel
let workerLabel = [label|candidate|]
let task = Task (batch campaign "submission") "plans/component.md" sourceHead
      "Submit a candidate" "Check the terminal source receipt"
      ["candidate source"] "read exact source" []
(worker, updates) <- unfoldDeferred (taskGroup task)
  (childWithProgress @WorkProgress @(Outcome Candidate)
    (withLifetime ActorOwned $ coding projectHead (assignment workerLabel task)))
Right readiness <- followWork [("candidate", worker, updates)] (notifyReviewReady me)
