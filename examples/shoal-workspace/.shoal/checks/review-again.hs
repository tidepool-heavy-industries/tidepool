firstReview <- pollWatch ready
inspectFull firstReview
let againLabel = "review-repaired" :: RequestLabel
(again, revisedQuestions) <- reviewAgain (forkedActor reviewer) againLabel (ReviewTask sessionInput revised OwnerRepairs)
let againReadyLabel = "repaired-review-ready" :: WatchLabel
againReady <- watch againReadyLabel (awaitSettled again)
