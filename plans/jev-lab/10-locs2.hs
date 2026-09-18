blobs <- traverse (\f -> (,) f <$> blobAt "f726882" f) srcFiles

lineNo loc = case reads (T.unpack (lineOf loc)) of { ((n, _) : _) -> n ; _ -> 0 :: Int }

locsA = [ ("shared definition", "src/app.rs:61:10") ]
     ++ [ ("reported by the compiler", s) | s <- concatMap primaries (snd (head (mechanicalGroups rawA))) ]
     ++ [ ("found by searching for " <> leafA, T.drop 1 (T.dropWhile (/= ':') h)) | h <- T.lines sweepA ]

excerptAt loc = maybe "<file unavailable>" (\body -> excerptFrom body (lineNo loc) 4) (lookup (fileOf loc) blobs)

object [ "locations" .= [ object ["why" .= w, "at" .= l] | (w, l) <- locsA ]
       , "sample" .= excerptAt "src/app.rs:61:10" ]
