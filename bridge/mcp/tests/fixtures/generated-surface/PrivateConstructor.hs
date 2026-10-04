{-# LANGUAGE DataKinds, GADTs, FlexibleContexts, RankNTypes, TypeOperators #-}
module PrivateConstructor where
import qualified Tidepool.Effects.Authored as Authored
privateConstructor = Authored.ActorReceiveWith
