{-# LANGUAGE OverloadedLabels, OverloadedRecordDot #-}
import Tidepool.Effects.Core (Jev, Commands)

sh :: Member Commands effs => [Text] -> Eff effs Text
sh args = do
  r <- Cmd.run (Cmd.argv args)
  pure (either (const "") id (Cmd.stdout r))

parseMs :: Text -> Int
parseMs t = T.foldl (\acc c -> acc * 10 + (fromEnum c - 48)) 0 (T.filter (\c -> c >= '0' && c <= '9') t)

qtexts :: [Text]
qtexts =
  [ "Does `content` contain the exact substring `pub fn`?"
  , "Does `content` import or use the `serde_json` crate?"
  , "Does `content` contain a module annotated `#[cfg(test)]`?"
  , "Does `content` handle a missing file by returning a typed error rather than panicking?"
  , "Does `content` define an `enum`?"
  , "Does `content` implement the `Display` trait for any type?"
  , "Does `content` use `async` or `await` anywhere?"
  , "Does `content` call `.unwrap()` anywhere?"
  , "Does `content` read a file's contents from disk?"
  , "Does `content` write a file's contents to disk?"
  , "Does `content` define a `struct`?"
  , "Does `content` use a `HashMap` or `BTreeMap` type?"
  , "Does `content` take a parameter named `limit` that bounds how many items it returns?"
  ]

do
  content <- T.take 3200 <$> sh ["git", "show", "53ad43c:src/store.rs"]
  let st = J.state (object ["content" .= content])
      packet =
        #q1 := J.noul (qtexts !! 0)
          :& #q2 := J.noul (qtexts !! 1)
          :& #q3 := J.noul (qtexts !! 2)
          :& #q4 := J.noul (qtexts !! 3)
          :& #q5 := J.noul (qtexts !! 4)
          :& #q6 := J.noul (qtexts !! 5)
          :& #q7 := J.noul (qtexts !! 6)
          :& #q8 := J.noul (qtexts !! 7)
          :& #q9 := J.noul (qtexts !! 8)
          :& #q10 := J.noul (qtexts !! 9)
          :& #q11 := J.noul (qtexts !! 10)
          :& #q12 := J.noul (qtexts !! 11)
          :& #q13 := J.noul (qtexts !! 12)
          :& Nil
  t0 <- sh ["date", "+%s%3N"]
  packetAnswer <- J.ask st packet
  t1 <- sh ["date", "+%s%3N"]
  let packetMs = parseMs t1 - parseMs t0
      packetRows = case packetAnswer of
        Left e -> [T.pack (show e)]
        Right r ->
          let a = J.answers r
          in [ T.pack (show a.q1.yes), T.pack (show a.q2.yes), T.pack (show a.q3.yes)
             , T.pack (show a.q4.yes), T.pack (show a.q5.yes), T.pack (show a.q6.yes)
             , T.pack (show a.q7.yes), T.pack (show a.q8.yes), T.pack (show a.q9.yes)
             , T.pack (show a.q10.yes), T.pack (show a.q11.yes), T.pack (show a.q12.yes)
             , T.pack (show a.q13.yes) ]
  t2 <- sh ["date", "+%s%3N"]
  seqResults <- mapM (\q -> J.ask1 st (J.noul q)) qtexts
  t3 <- sh ["date", "+%s%3N"]
  let seqMs = parseMs t3 - parseMs t2
      seqRows = [ either (T.pack . show) (\a -> T.pack (show a.yes)) r | r <- seqResults ]
      ratio = if seqMs == 0 then 0 else (fromIntegral packetMs :: Double) / fromIntegral seqMs
  pure (object
    [ "packet_ms" .= packetMs
    , "sequential_ms" .= seqMs
    , "ratio_packet_over_sequential" .= ratio
    , "packet_rows" .= packetRows
    , "sequential_rows" .= seqRows
    ])
