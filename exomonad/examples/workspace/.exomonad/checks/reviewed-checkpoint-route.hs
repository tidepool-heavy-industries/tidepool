{-# LANGUAGE QuasiQuotes #-}
let campaign = "reviewed-checkpoint" :: CampaignLabel
let task = Task (batch campaign "review") "plans/component.md" sourceHead
      "Review this slice" "Exercise exact source admission"
      ["slice.txt"] "read exact source" []
let candidate = Candidate sourceHead [] ["browser gate remains"]
let reviewRequest = ReviewRequest (AssignedTask task) candidate OwnerRepairs
(reviewer, _) <- reviewCandidate task OwnerRepairs candidate
