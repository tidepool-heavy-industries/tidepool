observed <- pollWatch inheritedWatch
let retained = case observed of { WatchReady answer -> responseValue answer; _ -> error "inherited response did not become ready" }
inspectFull (fst retained, snd retained 41)
