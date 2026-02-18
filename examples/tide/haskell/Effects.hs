{-# LANGUAGE GADTs, DataKinds, TypeOperators, FlexibleContexts #-}
module Effects (module Effects, module Control.Monad.Freer) where

import Control.Monad.Freer
import Types

data Repl a where
  ReadLine :: Repl (Maybe TExpr)
  Display  :: String -> Repl ()

data Console a where
  Print :: String -> Console ()

data Env a where
  EnvLookup :: String -> Env (Maybe TVal)
  EnvExtend :: String -> TVal -> Env ()

data Net a where
  HttpGet :: String -> Net String

data Fs a where
  FsRead  :: String -> Fs String
  FsWrite :: String -> String -> Fs ()

readLine' :: Member Repl effs => Eff effs (Maybe TExpr)
readLine' = send ReadLine

display :: Member Repl effs => String -> Eff effs ()
display s = send (Display s)

printLine :: Member Console effs => String -> Eff effs ()
printLine s = send (Print s)

envLookup :: Member Env effs => String -> Eff effs (Maybe TVal)
envLookup s = send (EnvLookup s)

envExtend :: Member Env effs => String -> TVal -> Eff effs ()
envExtend k v = send (EnvExtend k v)

httpGet :: Member Net effs => String -> Eff effs String
httpGet url = send (HttpGet url)

fsRead :: Member Fs effs => String -> Eff effs String
fsRead path = send (FsRead path)

fsWrite :: Member Fs effs => String -> String -> Eff effs ()
fsWrite path contents = send (FsWrite path contents)
