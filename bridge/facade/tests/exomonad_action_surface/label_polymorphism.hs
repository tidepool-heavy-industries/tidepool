{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}
{-# LANGUAGE TypeApplications #-}

module LabelPolymorphism where

import Control.Monad.Freer (Eff)
import Tidepool.Actors.Exomonad

-- '[label|...|]' is 'IsWatchLabel'-polymorphic: the same literal resolves as
-- a 'WatchLabel' here (at 'watch') and as a 'Label' below (at 'assignment'),
-- with no per-call-site conversion.
watchesSettlement :: Response Int -> Eff ActorEffects (Watch (Settlement Int))
watchesSettlement response = watch [label|settlement-ready|] (awaitSettled response)

spawnsWatchedChild :: ForkGroupPath -> Eff ActorEffects (Response Int, Watch (Settlement Int))
spawnsWatchedChild group =
  spawnWatched [label|spawned-child|] group
    (child (coding @Int projectHead (assignment [label|worker|] ())))

-- A full 40-hex sha reads directly as a 'GitOid' literal, with no
-- constructor or 'renderGitOid' round trip needed to name a committed source.
committedSha :: GitOid
committedSha = "0123456789abcdef0123456789abcdef01234567"

-- A fresh fork group is built from string literals, never the bare
-- 'ForkGroupPath' constructor (which is not exported as a term).
freshGroup :: ForkGroupPath
freshGroup = batch "campaign" "wave"

nestedGroup :: ForkGroupPath
nestedGroup = subgroup "lane-a"
