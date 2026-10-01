{-# LANGUAGE QuasiQuotes #-}
let campaign = "interview-collect" :: CampaignLabel
let question = Question "source-choice" (DesignQuestion
      "plans/component.md" sourceHead "Choose a source" [] [] ["implementation"])
(expert, _updates) <- unfoldDeferred (batch campaign "expert")
  (childWithProgress @WorkProgress @DesignAnswer $
    withLifetime ActorOwned $ coding projectHead
      (assignment [label|source-expert|] (questionDetails question)))
let interviewItems = [AwaitAnswer question expert]
