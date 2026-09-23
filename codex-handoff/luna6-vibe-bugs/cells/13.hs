let rubric = J.level #low "low" (0 :: Int) J..| J.level #high "high" 1
s <- J.ask (J.state (#task := ("t" :: Text))) (#q := J.score "How important is this?" rubric)
fmap (\a -> a.q.expectation) s
