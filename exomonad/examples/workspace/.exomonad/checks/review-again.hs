{-# LANGUAGE QuasiQuotes #-}
firstReview <- pollWatch ready
inspectFull firstReview
let againLabel = [label|review-repaired|]
(again, revisedQuestions) <- reviewAgain (responseActor reviewer) againLabel (ReviewTask sessionInput revised OwnerRepairs)
let againReadyLabel = "repaired-review-ready" :: WatchLabel
againReady <- watch againReadyLabel (awaitSettled again)
