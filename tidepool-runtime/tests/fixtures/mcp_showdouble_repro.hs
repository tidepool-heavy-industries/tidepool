{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Data.Map.Strict as Map
import qualified Data.Set as Set
import qualified Tidepool.Aeson.KeyMap as KM
import qualified Data.List as L
import qualified Tidepool.TextFormat as TF
import qualified Tidepool.Table as Tab
import Control.Monad.Freer hiding (run)
import Library
import qualified Prelude as P
default (Int, Text)
error :: Text -> a
error = P.error . T.unpack

data Console a where
  Print :: Text -> Console ()
data KV a where
  KvGet :: Text -> KV (Maybe Value)
  KvSet :: Text -> Value -> KV ()
  KvDelete :: Text -> KV ()
  KvKeys :: KV [Text]
data Fs a where
  FsRead :: Text -> Fs Text
  FsWrite :: Text -> Text -> Fs ()
  FsListDir :: Text -> Fs [Text]
  FsGlob :: Text -> Fs [Text]
  FsExists :: Text -> Fs Bool
  FsMetadata :: Text -> Fs (Int, Bool, Bool)
data SG a where
  SgFind :: Text -> Text -> Text -> [Text] -> SG [Value]
  SgPreview :: Text -> Text -> Text -> [Text] -> SG [Value]
  SgReplace :: Text -> Text -> Text -> [Text] -> SG Int
  SgRuleFind :: Text -> Value -> [Text] -> SG [Value]
  SgRuleReplace :: Text -> Value -> Text -> [Text] -> SG Int
data Http a where
  HttpGet :: Text -> Http Value
  HttpPost :: Text -> Value -> Http Value
  HttpRequest :: Text -> Text -> [(Text,Text)] -> Text -> Http Value
data Exec a where
  Run :: Text -> Exec (Int, Text, Text)
  RunIn :: Text -> Text -> Exec (Int, Text, Text)
  RunJson :: Text -> Exec Value
data Meta a where
  MetaConstructors :: Meta [(Text, Int)]
  MetaLookupCon :: Text -> Meta (Maybe (Int, Int))
  MetaPrimOps :: Meta [Text]
  MetaEffects :: Meta [Text]
  MetaDiagnostics :: Meta [Text]
  MetaVersion :: Meta Text
  MetaHelp :: Meta [Text]
data Git a where
  GitLog :: Text -> Int -> Git [Value]
  GitShow :: Text -> Git Value
  GitDiff :: Text -> Git [Value]
  GitBlame :: Text -> Int -> Int -> Git [Value]
  GitTree :: Text -> Text -> Git [Value]
  GitBranches :: Git [Value]
data Llm a where
  LlmChat :: Text -> Llm Text
  LlmStructured :: Text -> Value -> Llm Value
data Ask a where
  Ask :: Text -> Ask Value

type M = Eff '[Console, KV, Fs, SG, Http, Exec, Meta, Git, Llm, Ask]

say :: Text -> M ()
say t = do
  send (Print t)
  v <- send (KvGet "__sayChars")
  let cur = case v of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  send (KvSet "__sayChars" (toJSON (cur + T.length t)))

showI :: Int -> Text
showI n = show n

valSize :: Value -> Int
valSize v = case v of
  String t -> T.length t + 2
  Number _ -> 8
  Bool b -> if b then 4 else 5
  Null -> 4
  Array xs -> arrSz xs 2
  Object m -> objSz (KM.toList m) 2
arrSz :: [Value] -> Int -> Int
arrSz [] acc = acc
arrSz [x] acc = acc + valSize x
arrSz (x:xs) acc = arrSz xs (acc + valSize x + 2)
objSz :: [(Key, Value)] -> Int -> Int
objSz [] acc = acc
objSz [(k,v)] acc = acc + T.length (KM.toText k) + 4 + valSize v
objSz ((k,v):rest) acc = objSz rest (acc + T.length (KM.toText k) + 4 + valSize v + 2)
truncArr :: Int -> Int -> [Value] -> ([Value], Int, [(Int, Value)])
truncArr _ nid [] = ([], nid, [])
truncArr bud nid (x:xs)
  | bud <= 30 = ([marker], nid + 1, [(nid, Array (x:xs))])
  | sz <= bud = let (r, nid', s) = truncArr (bud - sz - 2) nid xs in (x : r, nid', s)
  | otherwise = let m = String ("[~" <> showI sz <> " chars -> stub_" <> showI nid <> "]")
                    (r, nid', s) = truncArr (bud - 50) (nid + 1) xs
                in (m : r, nid', (nid, x) : s)
  where sz = valSize x
        n = 1 + length xs
        tsz = sz + arrSz xs 0
        marker = String ("[" <> showI n <> " more, ~" <> showI tsz <> " chars -> stub_" <> showI nid <> "]")
truncKvs :: Int -> Int -> [(Key, Value)] -> ([(Key, Value)], Int, [(Int, Value)])
truncKvs _ nid [] = ([], nid, [])
truncKvs bud nid ((k,v):rest)
  | bud <= 30 = ([(KM.fromText "...", String marker)], nid + 1, [(nid, object (map (\(k',v') -> KM.toText k' .= v') ((k,v):rest)))])
  | sz <= bud = let (r, nid', s) = truncKvs (bud - sz - 2) nid rest in ((k,v) : r, nid', s)
  | otherwise = let m = String ("[~" <> showI (valSize v) <> " chars -> stub_" <> showI nid <> "]")
                    (r, nid', s) = truncKvs (bud - 50) (nid + 1) rest
                in ((k, m) : r, nid', (nid, v) : s)
  where sz = T.length (KM.toText k) + 4 + valSize v
        n = 1 + length rest
        tsz = sz + objSz rest 0
        marker = "[" <> showI n <> " more fields, ~" <> showI tsz <> " chars -> stub_" <> showI nid <> "]"
truncGo :: Int -> Int -> Value -> (Value, Int, [(Int, Value)])
truncGo bud nid v
  | valSize v <= bud = (v, nid, [])
  | otherwise = case v of
      Array xs -> let (items, nid', stubs) = truncArr bud nid xs in (Array items, nid', stubs)
      Object m -> let (pairs, nid', stubs) = truncKvs bud nid (KM.toList m)
                  in (object (map (\(k',v') -> KM.toText k' .= v') pairs), nid', stubs)
      String t -> let keep = max' 10 (bud - 30)
                  in (String (T.take keep t <> "...[" <> showI (T.length t) <> " chars]"), nid, [])
      _ -> (v, nid, [])
truncVal :: Int -> Value -> (Value, [(Int, Value)])
truncVal budget val = let (v, _, stubs) = truncGo budget 0 val in (v, stubs)
lookupStub :: Int -> [(Int, Value)] -> Maybe Value
lookupStub _ [] = Nothing
lookupStub sid ((k,v):rest) = if sid == k then Just v else lookupStub sid rest
paginateResult :: Int -> Value -> M Value
paginateResult budget val
  | valSize val <= budget = pure val
  | otherwise = do
      let (truncated, stubs) = truncVal budget val
      case stubs of
        [] -> pure truncated
        _ -> do
          let stubInfo = Array (map (\(sid, sv) -> object ["id" .= ("stub_" <> showI sid), "size" .= toJSON (valSize sv)]) stubs)
          resp <- send (Ask ("truncated: " <> show truncated <> " stubs: " <> show stubInfo))
          case resp ^? _String of
            Just s -> case parseIntM (T.drop 5 s) of
              Just sid -> case lookupStub sid stubs of
                Just subtree -> paginateResult budget subtree
                Nothing -> pure truncated
              Nothing -> pure truncated
            _ -> pure truncated

result :: M Value
result = do
  send (KvSet "__sayChars" (toJSON (0 :: Int)))
  _r <- do
    let xs = [10 :: Int, 20, 30]
        n = length xs
        d = fromIntegral n :: Double
    pure (pack (showDouble d))
  _scV <- send (KvGet "__sayChars")
  let _sayC = case _scV of { Just b -> case b ^? _Number of { Just n -> round n; _ -> 0 }; Nothing -> 0 }
  paginateResult (max' 100 (4096 - _sayC)) (toJSON _r)
