{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE FlexibleInstances #-}
{-# LANGUAGE OverloadedStrings #-}
-- | Human dialogue is ordinary Eff. Each ask mounts one pure Form and keeps
-- its decoder and original payloads until that lease settles.
module Tidepool.Form
  ( Form, Option, option, choice, choices, branches
  , textInput, intInput, numberInput, boolInput, optional
  , present, section, refine, validate, validateForm, ValidationError(..)
  , AutoForm, autoForm, edit
  , FormResult(..), FormCause(..), askUser, note
  ) where
import Prelude
import Data.Text (Text)
import qualified Data.Text as T
import Control.Monad.Freer (Eff, Member)
import Tidepool.Effects.Core
  ( AskUser, Console, FormCause(..), FormAttempt(..), FormTransition(..)
  , formOpenRaw, formAwaitRaw, formRejectRaw, formCommitRaw, formCloseRaw, displayViewRaw )
import Tidepool.Form.Algebra
import Tidepool.Form.GForm
import Tidepool.Form.Wire
import Tidepool.View.Types (View(..))
import Tidepool.View.Wire (encodeView)
import Tidepool.Inspection.Display (Display(..), application)
import Tidepool.Inspection.Tree (DisplayTree(..))

data FormResult a = Submitted a | Dismissed | FormUnavailable FormCause
instance Functor FormResult where
  fmap f (Submitted a) = Submitted (f a)
  fmap _ Dismissed = Dismissed
  fmap _ (FormUnavailable cause) = FormUnavailable cause
instance Display a => Display (FormResult a) where
  displayTree = displayTreePrec 0
  displayTreePrec p (Submitted a) = application p "Submitted" [displayTreePrec 11 a]
  displayTreePrec _ Dismissed = TextLeaf "Dismissed"
  displayTreePrec p (FormUnavailable cause) = application p "FormUnavailable" [TextLeaf (T.pack (show cause))]

askUser :: Member AskUser effects => Form a -> Eff effects (FormResult a)
askUser form = do
  let prepared = prepareForm form
  opened <- formOpenRaw (formDescriptor prepared)
  case opened of
    Left cause -> pure (FormUnavailable cause)
    Right lease ->
      let finish result = do
            _ <- formCloseRaw lease
            pure result
          await = do
            reply <- formAwaitRaw lease
            case reply of
              Left cause -> finish (FormUnavailable cause)
              Right FormDismissed -> finish Dismissed
              Right (FormSubmitted attempt draft) -> case decodeSubmission prepared draft of
                Left errors -> do
                  rejected <- formRejectRaw lease attempt (encodeErrors errors)
                  case rejected of
                    Left cause -> finish (FormUnavailable cause)
                    Right FormApplied -> await
                    Right FormStale -> await
                Right answer -> do
                  committed <- formCommitRaw lease attempt (encodeView 8192 (answerView prepared draft))
                  case committed of
                    Left cause -> finish (FormUnavailable cause)
                    Right FormApplied -> pure (Submitted answer)
                    Right FormStale -> await
       in await

note :: Member Console effects => Text -> Eff effects ()
note = displayViewRaw . encodeView 8192 . Markdown
