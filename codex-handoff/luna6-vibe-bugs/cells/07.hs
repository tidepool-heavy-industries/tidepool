{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
answer2 <- J.ask (J.state (#task := ("Find performance problems in this repository" :: Text))) packet
fmap (\r -> [(name, a.slow.yes) | ((name, _), a) <- r.per_file]) answer2
