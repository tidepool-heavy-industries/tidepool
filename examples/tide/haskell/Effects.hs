{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts, OverloadedStrings #-}
module Effects (module Effects, module Control.Monad.Freer) where

import Control.Monad.Freer
import Data.Text (Text)
import Types

data Repl a where
  ReadLine :: Repl (Maybe TExpr)
  Display  :: Text -> Repl ()

data Console a where
  Print :: Text -> Console ()

data Env a where
  EnvLookup :: Text -> Env (Maybe TVal)
  EnvExtend :: Text -> TVal -> Env ()
  EnvRemove :: Text -> Env ()
  EnvSnapshot :: Env [(Text, TVal)]

data Net a where
  HttpGet :: Text -> Net Text

data Fs a where
  FsRead  :: Text -> Fs Text
  FsWrite :: Text -> Text -> Fs ()

readLine' :: Member Repl effs => Eff effs (Maybe TExpr)
readLine' = send ReadLine

display :: Member Repl effs => Text -> Eff effs ()
display s = send (Display s)

printLine :: Member Console effs => Text -> Eff effs ()
printLine s = send (Print s)

envLookup :: Member Env effs => Text -> Eff effs (Maybe TVal)
envLookup s = send (EnvLookup s)

envExtend :: Member Env effs => Text -> TVal -> Eff effs ()
envExtend k v = send (EnvExtend k v)

envRemove :: Member Env effs => Text -> Eff effs ()
envRemove k = send (EnvRemove k)

envSnapshot :: Member Env effs => Eff effs [(Text, TVal)]
envSnapshot = send EnvSnapshot

httpGet :: Member Net effs => Text -> Eff effs Text
httpGet url = send (HttpGet url)

fsRead :: Member Fs effs => Text -> Eff effs Text
fsRead path = send (FsRead path)

fsWrite :: Member Fs effs => Text -> Text -> Eff effs ()
fsWrite path contents = send (FsWrite path contents)
