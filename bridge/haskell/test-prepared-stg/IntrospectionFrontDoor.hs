{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedStrings #-}

module IntrospectionFrontDoor where

import Control.Monad.Freer (Eff, Member)
import Data.Text (Text)
import qualified Data.Text as Text
import qualified Tidepool.Introspection as Introspection

-- Every shipped front-door verb compiled against one caller-supplied compile
-- view.
-- The probes are compiled by the resident handler, never evaluated here.
frontDoor
  :: Member Introspection.Introspection effects
  => Maybe Introspection.ScopeProvenance
  -> Introspection.IdentifierRef
  -> Eff
       effects
       ( Either Introspection.QueryError Introspection.ScopeProvenance
       , Either Introspection.QueryError Introspection.NameRefList
       , Either Introspection.QueryError Introspection.Fit
       , Either Introspection.QueryError Introspection.ExpressionType
       , Either Introspection.QueryError Introspection.TypeMatches
       )
frontDoor expectedScope identifier = do
  view <- Introspection.currentScopeView
  bindings <- Introspection.scopeBindings expectedScope
  fit <- Introspection.rowFit expectedScope identifier
  expression <- Introspection.typeOfExpression expectedScope "readFile"
  matches <- Introspection.typeSearch expectedScope "Int -> Int"
  pure (view, bindings, fit, expression, matches)

-- Binding enumeration carries identity only; presentation and row
-- availability stay on `Introspection.info`.
nameRefListProjection
  :: Introspection.NameRefList
  -> ([Introspection.IdentifierRef], Int, Int, Introspection.ScopeProvenance)
nameRefListProjection bindings =
  ( Introspection.refs bindings
  , Introspection.refsObserved bindings
  , Introspection.refsLimit bindings
  , Introspection.refListProvenance bindings
  )

fitRecordProjection
  :: Introspection.Fit
  -> ( Introspection.FitAvailability
     , Introspection.IdentifierRef
     , Introspection.ScopeProvenance
     )
fitRecordProjection fit =
  ( Introspection.fitAvailability fit
  , Introspection.fitIdentifier fit
  , Introspection.fitProvenance fit
  )

expressionProjection
  :: Introspection.ExpressionType
  -> (Text, Text, Introspection.FitAvailability, Introspection.ScopeProvenance)
expressionProjection expression =
  ( Introspection.expressionSource expression
  , Introspection.expressionDisplay expression
  , Introspection.expressionAvailability expression
  , Introspection.expressionProvenance expression
  )

typeMatchProjection
  :: Introspection.TypeMatches
  -> [(Text, Introspection.TypeMatchQuality, Introspection.FitAvailability)]
typeMatchProjection matches =
  [ ( Introspection.typeMatchName match
    , Introspection.typeMatchQuality match
    , Introspection.typeMatchAvailability match
    )
  | match <- Introspection.typeMatches matches
  ]

-- A stale caller view is a typed refusal carrying both provenances.
scopeOutcome :: Either Introspection.QueryError Introspection.ScopeProvenance -> Text
scopeOutcome outcome = case outcome of
  Right provenance -> Introspection.provenanceFingerprint provenance
  Left (Introspection.ScopeChanged expected current) ->
    Introspection.provenanceFingerprint expected
      <> " -> "
      <> Introspection.provenanceFingerprint current
  Left (Introspection.ExpressionRejected diagnostics) ->
    Text.intercalate "; " (map renderDiagnostic diagnostics)
  Left _ -> "other introspection refusal"

renderDiagnostic :: Introspection.Diagnostic -> Text
renderDiagnostic diagnostic =
  severity (Introspection.diagnosticSeverity diagnostic)
    <> maybe Text.empty renderSpan (Introspection.diagnosticSpan diagnostic)
    <> ": "
    <> Introspection.diagnosticMessage diagnostic
  where
    severity Introspection.DiagnosticError = "error"
    severity Introspection.DiagnosticWarning = "warning"
    renderSpan location =
      " ["
        <> Introspection.spanFile location
        <> ":"
        <> Text.pack (show (Introspection.spanStartLine location))
        <> ":"
        <> Text.pack (show (Introspection.spanStartCol location))
        <> "-"
        <> Text.pack (show (Introspection.spanEndLine location))
        <> ":"
        <> Text.pack (show (Introspection.spanEndCol location))
        <> "]"

result :: Eff '[] Int
result = pure 43
