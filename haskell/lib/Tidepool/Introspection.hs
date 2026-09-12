-- | Structured, read-only inspection of the resident Haskell scope.
--
-- Import this module qualified so the familiar 'info' and 'typeOf' names do
-- not compete with application vocabulary.
module Tidepool.Introspection
  ( Introspection
  , NameScope (..)
  , NameNamespace (..)
  , NameQuery (..)
  , IdentifierNamespace (..)
  , IdentifierRef (..)
  , ScopeProvenance (..)
  , TypeExpression (..)
  , TypeInfo (..)
  , FieldInfo (..)
  , ConstructorInfo (..)
  , ClassMethodInfo (..)
  , DeclarationInfo (..)
  , IdentifierInfo (..)
  , QueryError (..)
  , here
  , inModule
  , inNamespace
  , info
  , typeOf
  , constructors
  ) where

import Data.Text (Text)
import Tidepool.Effects

-- | Query an unqualified name in the caller's current lexical scope.
here :: Text -> NameQuery
here = NameQuery CurrentScope AnyName

-- | Query a name through an explicit module's public exports.
inModule :: Text -> Text -> NameQuery
inModule moduleName = NameQuery (PublicModule moduleName) AnyName

-- | Select a namespace when an unqualified spelling is ambiguous.
inNamespace :: NameNamespace -> NameQuery -> NameQuery
inNamespace namespace query = query { queryNamespace = namespace }

-- | Project constructors without making callers inspect every declaration
-- alternative. Kinds which do not declare a constructor return an empty list.
constructors :: IdentifierInfo -> [ConstructorInfo]
constructors details = case identifierDeclaration details of
  DataDeclaration _ values -> values
  NewtypeDeclaration _ value -> [value]
  ConstructorDeclaration _ value -> [value]
  _ -> []
