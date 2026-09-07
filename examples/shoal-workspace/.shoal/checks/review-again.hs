firstReview <- pollWatch ready
inspectFull firstReview
let Right againLabel = requestLabel "review-repaired"
(again, revisedQuestions) <- reviewAgain (forkedActor reviewer) againLabel (ReviewTask sessionInput revised OwnerRepairs)
let Right againReadyLabel = watchLabel "repaired-review-ready"
againReady <- watch againReadyLabel (awaitSettled again)
