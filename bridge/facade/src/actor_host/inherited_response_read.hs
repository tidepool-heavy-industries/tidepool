observed <- pollWatch inheritedWatch
let retained = case observed of { WatchReady answer -> answer; _ -> error "inherited response did not become ready" }
