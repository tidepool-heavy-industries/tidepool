{-# LANGUAGE DataKinds #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE OverloadedStrings #-}

module IntrospectionFrontDoor
  ( ProbeToken (..)
  , echoToken
  , frontDoor
  , expectStale
  , result
  ) where

import Prelude hiding (FilePath)
import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Effects.Authored as Effects
import Tidepool.FilePath (FilePath)
import qualified Tidepool.Introspection as I

-- A nominal type makes search distinguish the published binding from unrelated
-- Int -> Int functions. The hosted caller publishes its own queryEcho alias.
data ProbeToken = ProbeToken Int

echoToken :: ProbeToken -> ProbeToken
echoToken value = value

require :: Bool -> Text -> Eff effects ()
require condition message =
  if condition then pure () else error (Text.unpack message)

answer :: Text -> Either I.QueryError value -> Eff effects value
answer label outcome = case outcome of
  Right value -> pure value
  Left issue -> error (Text.unpack (label <> ": " <> Text.pack (show issue)))

valueType :: I.IdentifierInfo -> I.TypeExpression
valueType detail = case I.identifierDeclaration detail of
  I.ValueDeclaration expression -> expression
  _ -> error "discovered value has a non-value declaration"

-- GHC may qualify the same nominal type when presenting a selected alias.
-- Remove only this known source module qualifier, then compare the full type.
tokenType :: Text -> Text
tokenType = Text.replace "IntrospectionFrontDoor." ""

fingerprint :: I.ScopeProvenance -> Text
fingerprint = I.provenanceFingerprint

-- This is a read-only discovery program until the final ordinary readFile.
-- FitAvailable describes a compiler row check; the read still needs the real
-- FsRead interpreter and its runtime file authority.
frontDoor
  :: (Member I.Introspection effects, Member Effects.FsRead effects)
  => FilePath
  -> Eff effects Text
frontDoor path = do
  view <- answer "currentScopeView" =<< I.currentScopeView
  require (I.provenanceScope view == I.CurrentScope) "wrong current scope"
  require (I.provenanceGeneration view >= 0 && Text.length (fingerprint view) == 64)
    "missing scope identity"

  echo <- answer "info queryEcho" =<< I.info (I.NameQuery I.CurrentScope I.ValueName "queryEcho")
  let echoRef = I.inspectedIdentifier echo
      echoType = valueType echo
  require (I.identifierName echoRef == "queryEcho"
    && I.identifierNamespace echoRef == I.ValueIdentifier
    && not (Text.null (I.identifierModule echoRef))) "wrong published symbol"
  require (tokenType (I.typeCanonical echoType) == "ProbeToken -> ProbeToken"
    && null (I.typeVariables echoType) && null (I.typeConstraints echoType))
    "wrong published symbol type"
  require (I.identifierProvenance echo == view) "info used another scope"

  bindings <- answer "scopeBindings" =<< I.scopeBindings (Just view)
  require (echoRef `elem` I.refs bindings) "enumeration lost the discovered symbol"
  require (I.refListProvenance bindings == view
    && I.refsLimit bindings > 0
    && length (I.refs bindings) == min (I.refsObserved bindings) (I.refsLimit bindings))
    "wrong binding count or scope"

  operation <- answer "info readFile" =<< I.info (I.NameQuery I.CurrentScope I.ValueName "readFile")
  let operationRef = I.inspectedIdentifier operation
      operationType = I.typeCanonical (valueType operation)
  require (operationRef == I.IdentifierRef "Tidepool.Effects.Core" "readFile" I.ValueIdentifier)
    "discovered the wrong ordinary operation"
  require (operationType == "forall effs. Member FsRead effs => FilePath -> Eff effs (Either FsError Text)"
    && I.identifierProvenance operation == view) "wrong readFile declaration or source"

  fit <- answer "rowFit" =<< I.rowFit (Just view) operationRef
  require (I.fitIdentifier fit == operationRef && I.fitAvailability fit == I.FitAvailable
    && I.fitProvenance fit == view) "wrong keyed row fit"

  -- The module is genuine (it supplied the discovered operation), while the
  -- canonical occurrence is absent. Missing is advisory Unknown, not a row
  -- mismatch or a same-spelled replacement chosen from another generation.
  let absentRef = I.IdentifierRef (I.identifierModule operationRef)
        "__absentQueryIdentifier" I.ValueIdentifier
  absent <- answer "rowFit absent canonical name" =<< I.rowFit (Just view) absentRef
  require (I.fitIdentifier absent == absentRef && I.fitAvailability absent == I.FitUnknown
    && I.fitProvenance absent == view) "absent canonical name was not unknown"
  malformed <- I.rowFit (Just view) (I.IdentifierRef "" "readFile" I.ValueIdentifier)
  case malformed of
    Left (I.UnsupportedDeclaration detail) -> require (not (Text.null detail)) "missing malformed-identity reason"
    _ -> error "malformed canonical identifier was not an admission refusal"

  expression <- answer "typeOfExpression" =<< I.typeOfExpression (Just view) "queryEcho"
  require (I.expressionSource expression == "queryEcho"
    && tokenType (I.expressionDisplay expression) == "ProbeToken -> ProbeToken"
    && I.expressionAvailability expression == I.FitAvailable
    && I.expressionProvenance expression == view) "wrong expression type or source"

  matches <- answer "typeSearch" =<< I.typeSearch (Just view)
    "IntrospectionFrontDoor.ProbeToken -> IntrospectionFrontDoor.ProbeToken"
  match <- case filter ((== echoRef) . I.typeMatchReference) (I.typeMatches matches) of
    [found] -> pure found
    _ -> error ("type search did not retain the actual discovered symbol: "
      <> show (map I.typeMatchReference (I.typeMatches matches)))
  require (I.typeMatchName match == "queryEcho"
    && I.typeMatchModule match == Just (I.identifierModule echoRef)
    && tokenType (I.typeMatchSignature match) == "ProbeToken -> ProbeToken"
    && I.typeMatchQuality match == I.TypeMatchExact
    && I.typeMatchAvailability match == I.FitAvailable
    && I.typeMatchesProvenance matches == view
    && I.typeMatchesObserved matches == length (I.typeMatches matches)
    && I.typeMatchesLimit matches > 0
    && I.typeMatchesObserved matches <= I.typeMatchesLimit matches)
    "wrong type-search identity, signature, quality, availability or bound"

  after <- answer "scopeBindings after queries" =<< I.scopeBindings (Just view)
  finalView <- answer "currentScopeView after queries" =<< I.currentScopeView
  require (after == bindings && finalView == view) "read-only queries published a name"

  -- Haskell applies the known operation only after validating its discovered
  -- identity. Its result comes from the ordinary effect interpreter, not a
  -- query response or a forged successful file result.
  contents <- case operationRef of
    I.IdentifierRef "Tidepool.Effects.Core" "readFile" I.ValueIdentifier -> do
      readResult <- Effects.readFile path
      case readResult of
        Right text -> pure text
        Left issue -> error ("ordinary readFile failed: " <> show issue)
    _ -> error "cannot invoke the discovered operation"
  pure (Text.intercalate "\n"
    [ "scope|" <> Text.pack (show (I.provenanceScope view)) <> "|"
        <> Text.pack (show (I.provenanceGeneration view)) <> "|" <> fingerprint view
    , "bindings|" <> I.identifierModule echoRef <> "|" <> I.identifierName echoRef <> "|"
        <> Text.pack (show (I.refsObserved bindings)) <> "|"
        <> Text.pack (show (I.refsLimit bindings)) <> "|" <> fingerprint (I.refListProvenance bindings)
    , "operation|" <> I.identifierModule operationRef <> "|" <> I.identifierName operationRef
        <> "|" <> Text.pack (show (I.identifierNamespace operationRef)) <> "|" <> operationType
    , "fit|" <> I.identifierModule (I.fitIdentifier fit) <> "|"
        <> I.identifierName (I.fitIdentifier fit) <> "|" <> Text.pack (show (I.fitAvailability fit))
        <> "|" <> fingerprint (I.fitProvenance fit)
    , "expression|" <> I.expressionSource expression <> "|" <> tokenType (I.expressionDisplay expression)
        <> "|" <> Text.pack (show (I.expressionAvailability expression))
        <> "|" <> fingerprint (I.expressionProvenance expression)
    , "search|" <> I.typeMatchName match <> "|" <> tokenType (I.typeMatchSignature match)
        <> "|" <> Text.pack (show (I.typeMatchQuality match))
        <> "|" <> Text.pack (show (I.typeMatchAvailability match))
        <> "|" <> fingerprint (I.typeMatchesProvenance matches)
    , "read|" <> contents
    ])

-- The caller retains a real entry snapshot, then publishes another cell before
-- consuming it. Both returned views must come from the ordinary query effect.
expectStale
  :: Member I.Introspection effects
  => I.ScopeProvenance
  -> Eff effects Text
expectStale expected = do
  outcome <- I.scopeBindings (Just expected)
  case outcome of
    Left (I.ScopeChanged before after) -> do
      current <- answer "current scope after stale refusal" =<< I.currentScopeView
      require (before == expected && after == current && before /= after)
        "stale refusal lost its actual views"
      pure ("stale|ScopeChanged|" <> fingerprint before <> "|" <> fingerprint after)
    _ -> error ("a real publication did not invalidate the consumed pin: " <> show outcome)

-- The existing build owner compiles this effectful specialization. Hosted
-- execution supplies the published queryEcho binding and the real input file.
result :: Eff '[I.Introspection, Effects.FsRead] Text
result = frontDoor "query-input.txt"
