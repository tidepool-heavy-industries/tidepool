{-# LANGUAGE DataKinds #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE FlexibleContexts #-}
{-# LANGUAGE GADTs #-}
{-# LANGUAGE MonoLocalBinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedRecordDot #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications #-}
{-# LANGUAGE TypeFamilies #-}
{-# LANGUAGE TypeOperators #-}

-- Reading a failed build without spending an agent turn on it.
--
-- `investigate` takes the literal output of a failed check and returns what a
-- repair needs to know: which locations must be edited, which must be left
-- alone, which related tests exist and whether they assert the requirement,
-- and whether the work reaches outside the paths this worker owns. It gathers
-- its own evidence -- it searches the tree for the symbol the compiler named
-- and reads source at the candidate revision -- so nothing upstream has to
-- narrate a sequence of reads.
--
-- It diagnoses; it does not act. An edit outside the owned paths comes back as
-- an `OwnershipRequest` for the parent to authorise or reassign, never as a
-- decision to go ahead. Authority stays with whoever holds the contract.
--
-- Code does everything code can do. Splitting the output into diagnostics,
-- grouping them by shared cause, deduplicating locations, reading blobs and
-- matching owned-path prefixes are all mechanical and are never asked about.
-- The model answers only what the text cannot be read for: whether a location
-- must change, whether it declares the symbol or consumes it, whether it is a
-- test, and whether a test actually asserts the requirement.
--
-- Three requests, and the third depends on the second's answers: the test
-- bodies it reads are chosen by which locations came back as tests. A failure
-- that turns up no tests makes two requests.
module Project.Investigate
  ( -- What comes back
    Investigation (..)
  , Location (..)
  , LocationWhy (..)
  , Judgment (..)
  , GroupVerdict (..)
  , OwnershipRequest (..)
  , Coverage (..)
  , SearchPlan (..)
  , Strategy (..)
  , InvestigationPolicy (..)
  , defaultInvestigationPolicy
    -- The one call
  , investigate
  , renderInvestigation
    -- The mechanical half, pure and testable on its own
  , Diagnostic (..)
  , DiagnosticGroup (..)
  , splitDiagnostics
  , groupDiagnostics
  , diagnosticSites
  , enclosingBlock
  , enclosingName
  , excerptAround
  , locationOf
  , pathOf
  , lineOf
  ) where

import Data.Text (Text)
import qualified Data.Text as Text

import qualified Jev.Operators as J
import Jev.Operators (Cell ((:=)), Packet ((:&), Nil))
import qualified Tidepool.Command as Cmd
import Tidepool.Actors.Shoal (Eff, Member)
import Tidepool.Aeson.Value (Value (String), object, (.=))
import Tidepool.Effects.Core (Commands, Jev)

-- ---------------------------------------------------------------------------
-- The editable policy. Floors first, then the budget that bounds one pass.
-- ---------------------------------------------------------------------------

data InvestigationPolicy = InvestigationPolicy
  { mustChangeFloor :: Double
    -- ^ at or above this, a location is an obligation
  , mustChangeUnclear :: Double
    -- ^ between this and `mustChangeFloor` the answer is undecided and is
    --   reported as such rather than rounded away by the floor
  , declaresFloor :: Double
    -- ^ at or above this, a location defines the symbol and is left alone
  , testFloor :: Double
    -- ^ at or above this, a location is test code
  , assertsFloor :: Double
    -- ^ at or above this, a related test actually asserts the requirement
  , groundedFloor :: Double
    -- ^ the canary: below this the excerpts are not what we think they are
  , locationBudget :: Int
    -- ^ locations examined in one pass; the rest are reported unexamined
  , testBodyBudget :: Int
    -- ^ test bodies read in the second step
  , excerptRadius :: Int
    -- ^ lines of context around a location
  , commonNameHits :: Int
    -- ^ above this many search hits the term is treated as ambiguous and the
    --   occurrences are sifted before any of them become locations
  , sameMeaningFloor :: Double
    -- ^ at or above this, an occurrence of an ambiguous term is the same thing
  , strategyPolicy :: J.Policy
    -- ^ how sure the repair strategy has to be before the report commits to it.
    --   A repair gets acted on, so this is `J.merging`, not a bare floor.
  }

instance Show InvestigationPolicy where
  show policy = "InvestigationPolicy mustChangeFloor=" ++ show (mustChangeFloor policy)
    ++ " locationBudget=" ++ show (locationBudget policy)

defaultInvestigationPolicy :: InvestigationPolicy
defaultInvestigationPolicy = InvestigationPolicy
  { mustChangeFloor = 0.6
  , mustChangeUnclear = 0.35
  , declaresFloor = 0.6
  , testFloor = 0.6
  , assertsFloor = 0.6
  , groundedFloor = 0.5
  , locationBudget = 40
  , testBodyBudget = 8
  , excerptRadius = 4
  , commonNameHits = 25
  , sameMeaningFloor = 0.6
  , strategyPolicy = J.merging
  }

-- ---------------------------------------------------------------------------
-- The mechanical half: pure, no model, no effects
-- ---------------------------------------------------------------------------

-- One compiler diagnostic, verbatim, with the locations it names. `diagSites`
-- pairs each `--> path:line:col` with the label of the nearest `note:` or
-- `help:` line above it; the first, unlabelled one is the primary site.
data Diagnostic = Diagnostic
  { diagHeadline :: Text
  , diagCode :: Text
  , diagSites :: [(Text, Text)]
  , diagText :: Text
  } deriving (Show, Eq)

-- Diagnostics that share a headline and an identical set of secondary
-- locations are one cause. That is a string comparison, so the grouping does
-- not depend on the order the compiler emitted them.
data DiagnosticGroup = DiagnosticGroup
  { groupId :: Text
  , groupHeadline :: Text
  , groupCode :: Text
  , groupShared :: [Text]
  , groupPrimaries :: [Text]
  , groupRepresentative :: Text
  , groupSize :: Int
  } deriving (Show, Eq)

-- Lines that are progress or environment noise rather than a diagnostic.
noiseHeadline :: Text -> Bool
noiseHeadline line = any (`Text.isPrefixOf` line)
  [ "warning: ignoring"
  , "warning: build failed"
  , "warning: unused manifest key"
  ]

-- The compiler's closing tally. It repeats once per target, so it is a summary
-- of the diagnostics above rather than another obligation.
summaryHeadline :: Text -> Bool
summaryHeadline line = "error: could not compile" `Text.isPrefixOf` line

startsDiagnostic :: Text -> Bool
startsDiagnostic line =
  ("error" `Text.isPrefixOf` line || "warning" `Text.isPrefixOf` line)
    && not (noiseHeadline line)
    && not (summaryHeadline line)

-- `path:line:col` after a `-->` marker, or Nothing on any other line.
arrowLocation :: Text -> Maybe Text
arrowLocation line
  | "-->" `Text.isInfixOf` line = Just (Text.strip (Text.drop 3 (snd (Text.breakOn "-->" line))))
  | otherwise = Nothing

secondaryLabel :: Text -> Maybe Text
secondaryLabel line
  | "note:" `Text.isPrefixOf` stripped = Just stripped
  | "help:" `Text.isPrefixOf` stripped = Just stripped
  | otherwise = Nothing
  where stripped = Text.strip line

-- Each location in one diagnostic, tagged with the label that introduced it.
diagnosticSites :: [Text] -> [(Text, Text)]
diagnosticSites = go "primary"
  where
    go _ [] = []
    go label (line : rest) = case arrowLocation line of
      Just at -> (label, at) : go label rest
      Nothing -> go (maybe label id (secondaryLabel line)) rest

errorCode :: Text -> Text
errorCode headline
  | "[" `Text.isInfixOf` headline = Text.takeWhile (/= ']') (Text.drop 1 (snd (Text.breakOn "[" headline)))
  | otherwise = ""

splitDiagnostics :: Text -> [Diagnostic]
splitDiagnostics = chunk . Text.lines
  where
    chunk [] = []
    chunk (line : rest)
      | startsDiagnostic line =
          let (body, after) = break startsDiagnostic rest
              block = line : body
          in Diagnostic
               { diagHeadline = line
               , diagCode = errorCode line
               , diagSites = diagnosticSites block
               , diagText = Text.unlines block
               } : chunk after
      | otherwise = chunk rest

primarySites :: Diagnostic -> [Text]
primarySites d = [ at | (label, at) <- diagSites d, label == "primary" ]

secondarySites :: Diagnostic -> [Text]
secondarySites d = [ label <> " @ " <> at | (label, at) <- diagSites d, label /= "primary" ]

groupDiagnostics :: [Diagnostic] -> [DiagnosticGroup]
groupDiagnostics diagnostics = numbered (foldl add [] diagnostics)
  where
    key d = (diagHeadline d, secondarySites d)
    add acc d
      | any (\(m, _) -> key m == key d) acc =
          [ if key m == key d then (m, ms ++ [d]) else (m, ms) | (m, ms) <- acc ]
      | otherwise = acc ++ [(d, [d])]
    numbered groups =
      [ DiagnosticGroup
          { groupId = "G" <> Text.pack (show n)
          , groupHeadline = diagHeadline representative
          , groupCode = diagCode representative
          , groupShared = secondarySites representative
          , groupPrimaries = concatMap primarySites members
          , groupRepresentative = diagText representative
          , groupSize = length members
          }
      | (n, (representative, members)) <- zip [(1 :: Int) ..] groups
      ]

-- `path:line:col` reduced to the `path:line` this module addresses locations
-- by, so the same line reported at two columns is one location.
locationOf :: Text -> Text
locationOf raw = pathOf raw <> ":" <> Text.pack (show (lineOf raw))

pathOf :: Text -> Text
pathOf = Text.takeWhile (/= ':')

lineOf :: Text -> Int
lineOf raw = case Text.splitOn ":" raw of
  (_ : rest : _) -> digits rest
  _ -> 0
  where
    digits = Text.foldl (\acc c -> acc * 10 + (fromEnum c - 48)) 0 . Text.filter isDigit
    isDigit c = c >= '0' && c <= '9'

-- Numbered source around a line, so an answer that mentions a line number can
-- be checked against the same numbering the compiler used.
excerptAround :: Int -> Int -> Text -> Text
excerptAround radius at body =
  Text.unlines
    [ Text.pack (show (n + low + 1)) <> ": " <> line
    | (n, line) <- zip [(0 :: Int) ..] (take (2 * radius + 1) (drop low (Text.lines body)))
    ]
  where low = max 0 (at - radius - 1)

-- The definition enclosing a line: scan back to a `fn` header and forward to
-- the brace that closes it. Mechanical, and enough to show a whole test.
enclosingBlock :: Int -> Text -> Text
enclosingBlock at body =
  let ls = Text.lines body
      before = take (max 0 (at - 1)) ls
      -- index of the last `fn` header at or above the line, or 0 if there is none
      start = max 0 (length before - length (dropWhile (not . isHeader) (reverse before)))
      fromStart = drop start ls
      taken = balance 0 False fromStart
  in Text.unlines
       [ Text.pack (show (n + start + 1)) <> ": " <> line
       | (n, line) <- zip [(0 :: Int) ..] taken
       ]
  where
    isHeader line =
      let s = Text.strip line
      in ("fn " `Text.isPrefixOf` s)
           || ("pub fn " `Text.isPrefixOf` s)
           || ("async fn " `Text.isPrefixOf` s)
    balance _ _ [] = []
    balance depth seen (line : rest) =
      let opens = Text.count "{" line
          closes = Text.count "}" line
          depth' = depth + opens - closes
          seen' = seen || opens > 0
      in if seen' && depth' <= 0 then [line] else line : balance depth' seen' rest

-- The name of the definition enclosing a line, taken from its `fn` header.
-- Mechanical, and the anchor to search for when a diagnostic names no symbol
-- of its own, as a lint typically does not.
enclosingName :: Int -> Text -> Text
enclosingName at body =
  case [ name | line <- reverse (take (max 0 at) (Text.lines body)), Just name <- [headerName line] ] of
    (name : _) -> name
    [] -> ""
  where
    headerName line =
      let s = Text.strip line
          afterFn = [ rest | prefix <- ["fn ", "pub fn ", "async fn "]
                           , Just rest <- [Text.stripPrefix prefix s] ]
      in case afterFn of
           (rest : _) -> Just (Text.takeWhile (\c -> c /= '(' && c /= '<' && c /= ' ') rest)
           [] -> Nothing

-- How the tree was searched, so a reader can judge whether the search was
-- specific enough and a caller can see when there was nothing to search for.
data SearchPlan
  = SearchQualified Text
    -- ^ the fully qualified name from the diagnostic; the most specific term
  | SearchLeaf Text
    -- ^ the last segment, used when the qualified form appears nowhere
  | SearchEnclosing Text
    -- ^ no symbol in the headline; the definition enclosing the primary site
  | SearchNothing
    -- ^ nothing specific enough to search for
  deriving (Show, Eq)

searchTerm :: SearchPlan -> Text
searchTerm (SearchQualified t) = t
searchTerm (SearchLeaf t) = t
searchTerm (SearchEnclosing t) = t
searchTerm SearchNothing = ""

describePlan :: SearchPlan -> Text
describePlan (SearchQualified t) = "the qualified name " <> t
describePlan (SearchLeaf t) = "the name " <> t <> ", because the qualified form appears nowhere"
describePlan (SearchEnclosing t) = "the enclosing definition " <> t <> ", because the diagnostic names no symbol"
describePlan SearchNothing = "nothing; the diagnostic named no symbol and no enclosing definition was found"

nubText :: [Text] -> [Text]
nubText = foldl (\acc x -> if x `elem` acc then acc else acc ++ [x]) []

-- ---------------------------------------------------------------------------
-- What comes back
-- ---------------------------------------------------------------------------

-- Why a location is in the set. Every location carries this, so a reader can
-- tell a place the compiler named from a place this module went looking for.
data LocationWhy
  = PointedAtByCompiler Text
    -- ^ a `note: ... defined here` target; the text is the note
  | ReportedByCompiler Text
    -- ^ a primary site of the named diagnostic group
  | FoundBySearch Text
    -- ^ turned up by searching the tree for the given term
  deriving (Show, Eq)

data Location = Location
  { locId :: Text
  , locAt :: Text
  , locWhy :: LocationWhy
  , locExcerpt :: Text
  } deriving (Show, Eq)

-- The raw answers for one location, kept whole. Routed categories below are
-- derived from these and never replace them: a caller that disagrees with a
-- floor can re-derive from the numbers.
data Judgment = Judgment
  { mustChange :: Double
  , alreadyHandles :: Double
  , isTest :: Double
  , declaresSymbol :: Double
  , assertsRequirement :: Maybe Double
    -- ^ Nothing unless the second step read this location's enclosing block
  } deriving (Show, Eq)

-- The raw answers for one diagnostic group.
data GroupVerdict = GroupVerdict
  { verdictGroup :: DiagnosticGroup
  , sharedIsCorrect :: Double
  , eachSiteSeparate :: Double
  , anySiteOutsideOwned :: Double
  , helpTextIsPlaceholder :: Double
  } deriving (Show, Eq)

-- Two whole repairs compete for a failure like a changed signature: catch the
-- callers up, or put the signature back. They are not independent per-location
-- judgments, and asking each location in isolation produces answers near a half
-- that a reader who knows the domain correctly distrusts. Fix the strategy
-- first; the obligations then follow mechanically from the diagnostics.
data Strategy
  = CallersCatchUp
    -- ^ the change at the pointed-at definition is intended, so every site the
    --   compiler reported has to catch up with it
  | RestoreDefinition Text
    -- ^ the change at the named definition was not intended, so it goes back
    --   and the reported sites are already correct
  | StrategyUnclear Text
    -- ^ nothing available says which repair was meant; both are set out and
    --   neither is presented as the answer
  deriving (Show, Eq)

-- A diagnosis, not a decision. The parent authorises the expansion, assigns
-- the work elsewhere, or revises the contract.
data OwnershipRequest = OwnershipRequest
  { requestPaths :: [Text]
  , requestSites :: [(Text, Text)]
  , requestReason :: Text
  } deriving (Show, Eq)

-- What a budget stopped us from looking at. A truncated pass says so; it never
-- presents partial inspection as a complete answer.
data Coverage = Coverage
  { candidatesFound :: Int
  , candidatesExamined :: Int
  , unexamined :: [Text]
  , testBodiesRead :: Int
  , testBodiesSkipped :: [Text]
  , notes :: [Text]
  } deriving (Show, Eq)

data Investigation = Investigation
  { invCommand :: Text
  , invExit :: Int
  , invSymbol :: Text
  , invSearchPlan :: SearchPlan
  , invVerdicts :: [GroupVerdict]
  , invJudgments :: [(Location, Judgment)]
  , invStrategy :: Strategy
  , invStrategyExplained :: Text
  , invAlternative :: [Location]
    -- ^ what would have to change under the repair that was not chosen; empty
    --   unless the strategy is unclear
  , invMustEdit :: [Location]
  , invUndecided :: [(Location, Double)]
  , invOmitted :: [Location]
    -- ^ examined, matched no category. Listed by address, because a count tells
    --   a reader something is missing and gives them no way to get it.
  , invLeaveAlone :: [Location]
  , invRelatedTestLocations :: [Location]
  , invAssertingTests :: [Location]
  , invOwnershipRequest :: Maybe OwnershipRequest
  , invIgnoreCompilerSuggestion :: Bool
  , invCoverage :: Coverage
  } deriving (Show)

-- ---------------------------------------------------------------------------
-- The one call
-- ---------------------------------------------------------------------------

-- Run a command in the repository and hand back its stdout, quietly: this is
-- evidence gathering, not work the operator needs to watch scroll past.
readCommand :: Member Commands effs => Text -> [Text] -> Eff effs Text
readCommand directory args = do
  result <- Cmd.quiet (Cmd.run (Cmd.inDirectory directory (Cmd.argv args)))
  pure (Cmd.outputText (Cmd.commandStdout (Cmd.capturedOutput result)))

-- Qualified suffixes of a name, most specific first:
-- `app::ActivePanel::Tags`, `ActivePanel::Tags`, `Tags`. A use site rarely
-- writes the whole path, so the most specific form that appears anywhere is
-- both correct and the least ambiguous thing to search for.
qualifiedSuffixes :: Text -> [Text]
qualifiedSuffixes symbol =
  [ Text.intercalate "::" (drop n parts) | n <- [0 .. max 0 (length parts - 1)] ]
  where parts = Text.splitOn "::" symbol

-- Decide what to search the tree for, and search it. Everything here is
-- mechanical: prefer the most specific qualification that occurs at all, and
-- fall back to the definition enclosing the primary site when the diagnostic
-- names no symbol, as a lint does not.
planSearch
  :: Member Commands effs
  => InvestigationPolicy -> Text -> Text -> Text -> [Text] -> Eff effs (SearchPlan, [Text], [Text])
planSearch policy directory oid symbol anchors = do
  let grep term = readCommand directory ["git", "grep", "-n", term, oid, "--", "src"]
      suffixes = if Text.null symbol then [] else qualifiedSuffixes symbol
  tried <- traverse (\term -> fmap ((,) term) (grep term)) suffixes
  let found = [ (term, Text.lines out) | (term, out) <- tried, not (Text.null (Text.strip out)) ]
      leafTerm = case reverse suffixes of { (leaf : _) -> leaf ; [] -> "" }
  case found of
    ((term, hits) : _) ->
      let plan = if term == leafTerm then SearchLeaf term else SearchQualified term
          leafHits = case [ h | (t, h) <- found, t == leafTerm ] of { (h : _) -> h ; [] -> [] }
          wider =
            [ "a broader search for " <> leafTerm <> " matches "
                <> Text.pack (show (length leafHits)) <> " lines; only the "
                <> Text.pack (show (length hits)) <> " matching " <> term <> " were examined"
            | term /= leafTerm, length leafHits > length hits
            ]
          (kept, dropped) = splitAt (commonNameHits policy) hits
          tooMany =
            [ Text.pack (show (length dropped)) <> " further occurrences of " <> term
                <> " were not examined; the name is too common to follow every one"
            | not (null dropped)
            ]
      in pure (plan, kept, wider ++ tooMany)
    [] -> do
      -- Anchors are tried in order. The definition a `note: ... defined here`
      -- points at names the thing that changed; the call site that tripped over
      -- it only names whatever function encloses it, which is usually too
      -- common to be worth searching for.
      named <- traverse
        (\anchor -> do
            body <- readCommand directory ["git", "show", oid <> ":" <> pathOf anchor]
            pure (enclosingName (lineOf anchor) body))
        (filter (not . Text.null) anchors)
      let enclosing = case filter (not . Text.null) named of
            (name : _) -> name
            [] -> ""
      if Text.null enclosing
        then pure (SearchNothing, [], [])
        else do
          out <- grep enclosing
          let hits = Text.lines out
              (kept, dropped) = splitAt (commonNameHits policy) hits
          pure
            ( SearchEnclosing enclosing
            , kept
            , [ Text.pack (show (length dropped)) <> " further occurrences of " <> enclosing
                  <> " were not examined"
              | not (null dropped)
              ]
            )

backtickedName :: Text -> Text
backtickedName headline = case Text.splitOn "`" headline of
  (_ : name : _) -> name
  _ -> ""

-- | Read a failed check and prepare the next steps.
--
-- @directory@ is the repository to run git in, @oid@ the revision the check
-- ran on, @owned@ the path prefixes this worker may edit, @requirements@ the
-- contract clauses a related test would have to assert, and @output@ the
-- literal check output.
investigate
  :: (Member Jev effs, Member Commands effs)
  => InvestigationPolicy
  -> Text
  -> Text
  -> [Text]
  -> [Text]
  -> [Text]
  -> Text
  -> Int
  -> Text
  -> Eff effs Investigation
investigate policy directory oid owned requirements intent command exit output = do
  let groups = groupDiagnostics (splitDiagnostics output)
      symbol = case groups of
        (g : _) -> backtickedName (groupHeadline g)
        [] -> ""
      primary = case [ at | g <- groups, at <- groupPrimaries g ] of
        (at : _) -> locationOf at
        [] -> ""

  (verdicts, strategy, strategyWhy) <- askGroups policy owned intent command exit groups

  let pointedAt =
        [ (note, locationOf (Text.strip (Text.drop 3 (snd (Text.breakOn " @ " shared)))))
        | g <- groups
        , shared <- groupShared g
        , let note = Text.strip (fst (Text.breakOn " @ " shared))
        ]

  -- Evidence the compiler did not report: places that name the same thing and
  -- compile, which is where a producer or a test lives.
  (plan, sweepHits, searchNotes) <- planSearch policy directory oid symbol
    (map snd pointedAt ++ [primary])

  let term = searchTerm plan
      reportedLocs = nubText [ locationOf at | g <- groups, at <- groupPrimaries g ]
      searched =
        [ locationOf (Text.drop 1 (Text.dropWhile (/= ':') hit))
        | hit <- sweepHits
        ]
      whyFor at
        | at `elem` reportedLocs =
            ReportedByCompiler (maybe "" groupId (firstGroupWith at groups))
        | Just note <- lookup at [ (a, n) | (n, a) <- pointedAt ] = PointedAtByCompiler note
        | otherwise = FoundBySearch term
      candidates = nubText (map snd pointedAt ++ reportedLocs ++ searched)
      (examined, remainder) = splitAt (locationBudget policy) candidates
      files = nubText (map pathOf examined)

  bodies <- traverse (\f -> fmap ((,) f) (readCommand directory ["git", "show", oid <> ":" <> f])) files

  let bodyOf at = maybe "" id (lookup (pathOf at) bodies)
      locations =
        [ Location
            { locId = "L" <> Text.pack (show n)
            , locAt = at
            , locWhy = whyFor at
            , locExcerpt =
                if Text.null (bodyOf at)
                  then "<source unavailable at this revision>"
                  else excerptAround (excerptRadius policy) (lineOf at) (bodyOf at)
            }
        | (n, at) <- zip [(1 :: Int) ..] examined
        ]

  (grounded, judgments) <- askLocations symbol owned command exit locations

  let scored = zip locations judgments
      testLocations =
        [ loc | (loc, j) <- scored, isTest j >= testFloor policy ]
      (readable, skipped) = splitAt (testBodyBudget policy) testLocations

  -- The second step, and the one that makes this a loop rather than a
  -- pipeline: which bodies to read is decided by the answers just received.
  assertions <-
    if null readable || null requirements
      then pure []
      else askAssertions requirements command
             [ (loc, enclosingBlock (lineOf (locAt loc)) (bodyOf (locAt loc))) | loc <- readable ]

  let withAssertion (loc, j) = case lookup (locId loc) assertions of
        Just p -> (loc, j { assertsRequirement = Just p })
        Nothing -> (loc, j)
      final = map withAssertion scored
      ownedBy at = any (`Text.isPrefixOf` at) owned
      isReported loc = case locWhy loc of
        ReportedByCompiler _ -> True
        _ -> False
      isDefinition loc = case locWhy loc of
        PointedAtByCompiler _ -> True
        _ -> False
      -- Once the strategy is fixed the compiler has already named the
      -- obligations, so nothing here is a judgment. Only locations the
      -- compiler did not report still need one.
      alsoRelevant =
        [ loc | (loc, j) <- final, not (isReported loc), not (isDefinition loc)
        , mustChange j >= mustChangeFloor policy ]
      callersRepair = [ loc | (loc, _) <- final, isReported loc ] ++ alsoRelevant
      definitionRepair = [ loc | (loc, _) <- final, isDefinition loc ]
      (mustEdit, alternative) = case strategy of
        CallersCatchUp -> (callersRepair, [])
        RestoreDefinition _ -> (definitionRepair, [])
        StrategyUnclear _ -> (definitionRepair, callersRepair)
      undecided = case strategy of
        StrategyUnclear _ -> []
        _ ->
          [ (loc, mustChange j)
          | (loc, j) <- final
          , not (isReported loc), not (isDefinition loc)
          , mustChange j < mustChangeFloor policy
          , mustChange j >= mustChangeUnclear policy
          , declaresSymbol j < declaresFloor policy
          ]
      leaveAlone =
        [ loc
        | (loc, j) <- final
        , mustChange j < mustChangeFloor policy
        , declaresSymbol j >= declaresFloor policy
        ]
      relatedTests = [ loc | (loc, j) <- final, isTest j >= testFloor policy ]
      asserting =
        [ loc
        | (loc, j) <- final
        , Just p <- [assertsRequirement j]
        , p >= assertsFloor policy
        ]
      routed = nubText
        ( map locAt mustEdit ++ map locAt alternative
            ++ map (locAt . fst) undecided
            ++ map locAt leaveAlone ++ map locAt relatedTests )
      omitted = [ loc | (loc, _) <- final, locAt loc `notElem` routed ]
      strays = [ loc | loc <- mustEdit, not (ownedBy (locAt loc)) ]
      ownership =
        if null strays
          then Nothing
          else Just OwnershipRequest
            { requestPaths = nubText (map (pathOf . locAt) strays)
            , requestSites = [ (locAt loc, renderWhy (locWhy loc)) | loc <- strays ]
            , requestReason =
                "repairing " <> subject <> " requires edits in files outside the owned paths; "
                  <> "another worker's assumptions may depend on them"
            }
      placeholder = any (\v -> helpTextIsPlaceholder v >= 0.9) verdicts
      coverage = Coverage
        { candidatesFound = length candidates
        , candidatesExamined = length examined
        , unexamined = remainder
        , testBodiesRead = length readable
        , testBodiesSkipped = map locAt skipped
        , notes =
            ("searched for " <> describePlan plan)
              : [ "the excerpts did not read as source naming " <> term
                | grounded < groundedFloor policy, not (Text.null term)
                ]
              ++ searchNotes
              ++ [ "no contract requirements were supplied, so related tests were not checked for assertions"
                 | null requirements && not (null testLocations)
                 ]
        }
      -- What the repair is about, in one phrase, whether or not the diagnostic
      -- named a symbol. A lint names none, and "repairing  requires" reads as a
      -- defect in this report rather than in the code.
      subject
        | not (Text.null symbol) = symbol
        | not (Text.null (searchTerm plan)) = searchTerm plan
        | otherwise = "this failure"

  pure Investigation
    { invCommand = command
    , invExit = exit
    , invSymbol = symbol
    , invSearchPlan = plan
    , invVerdicts = verdicts
    , invJudgments = final
    , invStrategy = strategy
    , invStrategyExplained = strategyWhy
    , invAlternative = alternative
    , invMustEdit = mustEdit
    , invUndecided = undecided
    , invOmitted = omitted
    , invLeaveAlone = leaveAlone
    , invRelatedTestLocations = relatedTests
    , invAssertingTests = asserting
    , invOwnershipRequest = ownership
    , invIgnoreCompilerSuggestion = placeholder
    , invCoverage = coverage
    }

firstGroupWith :: Text -> [DiagnosticGroup] -> Maybe DiagnosticGroup
firstGroupWith at groups =
  case [ g | g <- groups, at `elem` map locationOf (groupPrimaries g) ] of
    (g : _) -> Just g
    [] -> Nothing

renderWhy :: LocationWhy -> Text
renderWhy (PointedAtByCompiler note) = note
renderWhy (ReportedByCompiler gid) = "reported by the compiler in " <> gid
renderWhy (FoundBySearch term) = "found by searching the tree for " <> term

-- ---------------------------------------------------------------------------
-- The three requests
-- ---------------------------------------------------------------------------

renderGroup :: DiagnosticGroup -> Text
renderGroup g = Text.unlines
  ( (groupId g <> "| " <> groupHeadline g)
      : ("  sites: " <> Text.intercalate ", " (groupPrimaries g))
      : [ "  shared: " <> s | s <- groupShared g ]
      ++ [ "  one representative diagnostic, verbatim:" ]
      ++ map ("  | " <>) (Text.lines (groupRepresentative g))
  )

-- Every question names the fact that decides it. An earlier draft asked
-- "would a single edit resolve every site", which came back at 0.42 on a
-- fixture where the answer is plainly no; naming why the edits would be
-- separate moved the same judgment to 0.72.
askGroups
  :: Member Jev effs
  => InvestigationPolicy -> [Text] -> [Text] -> Text -> Int -> [DiagnosticGroup]
  -> Eff effs ([GroupVerdict], Strategy, Text)
askGroups policy owned intent command exit groups
  | null groups = pure ([], StrategyUnclear "the output carried no diagnostics", "")
  | otherwise = do
      let pool = J.pool #groups [ (groupId g, String (renderGroup g), g) | g <- groups ]
          packet =
            #groups := pool
              :& #legible := J.noul "Does `diagnostics` contain compiler diagnostics that name file locations?"
              :& #each := J.eachIn pool (\ref ->
                   #shared_is_correct := J.askAbout ref
                     "Is the code at this group's shared location already correct, so that the repair must change the listed sites instead?"
                     :& #each_site_separate := J.askAbout ref
                          "Does each listed site need its own separate edit, because the sites are in different functions or files?"
                     :& #outside_owned := J.askAbout ref
                          "Is at least one listed site in a file that does not start with any prefix in `owned_paths`?"
                     :& #help_text_placeholder := J.askAbout ref
                          "Does the compiler's own suggested fix in the verbatim diagnostic insert a placeholder such as `todo!()` or `unimplemented!()` rather than working code?"
                     :& Nil)
              :& #strategy := J.choice
                   "Which repair was meant? Read `intent` first; the diagnostics alone cannot settle this."
                   ( J.alt #callers_catch_up
                       (String "The code at the location the diagnostics point to as the definition was changed on purpose, and the sites the compiler reported are stale callers that have to catch up with it.")
                       ("callers_catch_up" :: Text)
                     J..| J.alt #restore_the_definition
                       (String "The change at that definition was not meant, and putting it back is the repair; the reported sites are already correct as written.")
                       "restore_the_definition"
                     J..| J.alt #insufficient_evidence
                       (String "Nothing in `intent` or the diagnostics says which of those two repairs was meant.")
                       "insufficient_evidence" )
              :& Nil
      answer <- J.ask
        (J.state (object
          [ "command" .= command
          , "exit_status" .= exit
          , "owned_paths" .= owned
          , "intent" .= intent
          , "diagnostics" .= Text.intercalate "\n" (map renderGroup groups)
          ]))
        packet
      case answer of
        Left failure ->
          pure ( [ blankVerdict g | g <- groups ]
               , StrategyUnclear ("the question could not be asked: " <> Text.pack (show failure))
               , "" )
        Right response ->
          let answers = J.answers response
              verdicts =
                [ case lookup (groupId g) answers.each of
                    Nothing -> blankVerdict g
                    Just per -> GroupVerdict
                      { verdictGroup = g
                      , sharedIsCorrect = per.shared_is_correct.yes
                      , eachSiteSeparate = per.each_site_separate.yes
                      , anySiteOutsideOwned = per.outside_owned.yes
                      , helpTextIsPlaceholder = per.help_text_placeholder.yes
                      }
                | g <- groups
                ]
              reasoning = J.explain (strategyPolicy policy) answers.strategy
              -- A repair is acted on, so it is held to the strictest of the
              -- three named policies rather than a bare probability.
              chosen = case J.accept (strategyPolicy policy) answers.strategy of
                Left doubt -> StrategyUnclear (Text.pack (show doubt) <> "; " <> reasoning)
                Right selection -> case J.selectedKey selection of
                  "callers_catch_up" -> CallersCatchUp
                  "restore_the_definition" ->
                    RestoreDefinition (definitionNamedBy groups)
                  _ -> StrategyUnclear reasoning
          in pure (verdicts, chosen, reasoning)

-- The location the diagnostics point at as the definition, which is what a
-- restore would put back.
definitionNamedBy :: [DiagnosticGroup] -> Text
definitionNamedBy groups =
  case [ locationOf (Text.strip (Text.drop 3 (snd (Text.breakOn " @ " shared))))
       | g <- groups, shared <- groupShared g
       ] of
    (at : _) -> at
    [] -> "the definition"

blankVerdict :: DiagnosticGroup -> GroupVerdict
blankVerdict g = GroupVerdict g 0 0 0 0

renderLocation :: Location -> Text
renderLocation loc =
  locId loc <> "| " <> locAt loc <> "  (" <> renderWhy (locWhy loc) <> ")\n" <> locExcerpt loc

askLocations
  :: Member Jev effs
  => Text -> [Text] -> Text -> Int -> [Location] -> Eff effs (Double, [Judgment])
askLocations symbol owned command exit locations
  | null locations = pure (0, [])
  | otherwise = do
      let pool = J.pool #locations [ (locId loc, String (renderLocation loc), loc) | loc <- locations ]
          packet =
            #locations := pool
              :& #grounded := J.noul
                   ("Does `locations` contain source excerpts that mention " <> symbol <> "?")
              :& #each := J.eachIn pool (\ref ->
                   #must_change := J.askAbout ref
                     "To make `command` succeed, must the code shown at this location be edited?"
                     :& #already_handles := J.askAbout ref
                          ("Does the code shown at this location already handle " <> symbol <> "?")
                     :& #is_test := J.askAbout ref
                          "Is the code shown at this location inside a test?"
                     :& #declares := J.askAbout ref
                          ("Does this location declare " <> symbol <> " rather than consume it?")
                     :& Nil)
              :& Nil
      answer <- J.ask
        (J.state (object
          [ "command" .= command
          , "exit_status" .= exit
          , "owned_paths" .= owned
          , "symbol" .= symbol
          , "locations" .= Text.intercalate "\n" (map renderLocation locations)
          ]))
        packet
      case answer of
        Left _ -> pure (0, [ blankJudgment | _ <- locations ])
        Right response ->
          let answers = J.answers response
          in pure
             ( answers.grounded.yes
             , [ case lookup (locId loc) answers.each of
                   Nothing -> blankJudgment
                   Just per -> Judgment
                     { mustChange = per.must_change.yes
                     , alreadyHandles = per.already_handles.yes
                     , isTest = per.is_test.yes
                     , declaresSymbol = per.declares.yes
                     , assertsRequirement = Nothing
                     }
               | loc <- locations
               ]
             )

blankJudgment :: Judgment
blankJudgment = Judgment 0 0 0 0 Nothing

-- The evidence-dependent step. Which bodies arrive here is decided by the
-- previous request's `is_test` answers, and each carries the whole enclosing
-- definition rather than a window, because an assertion can sit anywhere in a
-- test.
askAssertions
  :: Member Jev effs
  => [Text] -> Text -> [(Location, Text)] -> Eff effs [(Text, Double)]
askAssertions requirements command bodies
  | null bodies = pure []
  | otherwise = do
      let pool = J.pool #bodies
            [ (locId loc, String (locId loc <> "| " <> locAt loc <> "\n" <> body), loc)
            | (loc, body) <- bodies
            ]
          packet =
            #bodies := pool
              :& #each := J.eachIn pool (\ref ->
                   #asserts := J.askAbout ref
                     "Does this test assert an outcome listed in `requirements`, rather than only constructing state or calling the code?"
                     :& Nil)
              :& Nil
      answer <- J.ask
        (J.state (object
          [ "command" .= command
          , "requirements" .= requirements
          , "bodies" .= Text.intercalate "\n"
              [ locId loc <> "| " <> locAt loc <> "\n" <> body | (loc, body) <- bodies ]
          ]))
        packet
      case answer of
        Left _ -> pure []
        Right response ->
          let answers = J.answers response
          in pure [ (key, per.asserts.yes) | (key, per) <- answers.each ]

-- ---------------------------------------------------------------------------
-- Rendering
-- ---------------------------------------------------------------------------

-- One screen. Obligations first, then what not to touch, then anything the
-- reader has to decide, then what was not looked at.
renderInvestigation :: Investigation -> Text
renderInvestigation inv = Text.unlines
  ( [ invCommand inv <> " exited " <> Text.pack (show (invExit inv))
        <> subjectLine
    , ""
    ]
    ++ strategyLines
    ++ [ "", obligationHeading ]
    ++ bullets [ locAt loc <> "  (" <> renderWhy (locWhy loc) <> ")" | loc <- invMustEdit inv ]
    ++ [ "", "leave alone:" ]
    ++ bullets [ locAt loc <> "  (" <> renderWhy (locWhy loc) <> ")" | loc <- invLeaveAlone inv ]
    ++ alternativeLines
    ++ [ "", "undecided, and the report will not decide for you:" ]
    ++ bullets
         [ locAt loc <> "  (" <> renderWhy (locWhy loc) <> undecidedNote p <> ")"
         | (loc, p) <- invUndecided inv
         ]
    ++ [ "", "related test locations:" ]
    ++ bullets
         [ locAt loc <> assertionNote loc | loc <- invRelatedTestLocations inv ]
    ++ ownershipLines
    ++ [ "" | invIgnoreCompilerSuggestion inv ]
    ++ [ "the compiler's suggested fix inserts a placeholder; do not apply it as written"
       | invIgnoreCompilerSuggestion inv
       ]
    ++ [ "", "coverage:" ]
    ++ bullets
         ( [ Text.pack (show (candidatesExamined inv')) <> " of "
               <> Text.pack (show (candidatesFound inv')) <> " candidate locations examined" ]
             ++ [ "not examined: " <> Text.intercalate ", " (unexamined inv')
                | not (null (unexamined inv'))
                ]
             ++ [ Text.pack (show (testBodiesRead inv')) <> " test bodies read" ]
             ++ [ "test bodies not read: " <> Text.intercalate ", " (testBodiesSkipped inv')
                | not (null (testBodiesSkipped inv'))
                ]
             ++ [ "examined and not listed above: "
                    <> Text.intercalate ", " (map locAt (invOmitted inv))
                | not (null (invOmitted inv))
                ]
             ++ notes inv'
         )
  )
  where
    inv' = invCoverage inv
    strategyLines = case invStrategy inv of
      CallersCatchUp ->
        [ "the change at the definition was meant, so the reported sites have to catch up with it"
        , "  " <> invStrategyExplained inv
        ]
      RestoreDefinition at ->
        [ "the change at " <> at <> " was not meant, so putting it back is the repair"
        , "  " <> invStrategyExplained inv
        ]
      StrategyUnclear why ->
        [ "two repairs fit this failure and nothing said which was meant, so this report"
        , "does not choose between them. Both are set out below."
        , "  " <> why
        ]
    obligationHeading = case invStrategy inv of
      StrategyUnclear _ -> "under the repair that puts the definition back, edit:"
      _ -> "must be edited:"
    alternativeLines = case invStrategy inv of
      StrategyUnclear _ ->
        ( "" : "under the repair that brings the callers up to date, edit:" : [] )
          ++ bullets [ locAt loc <> "  (" <> renderWhy (locWhy loc) <> ")" | loc <- invAlternative inv ]
      _ -> []
    undecidedNote p
      | p <= 0 = ""
      | otherwise = "; " <> Text.pack (show (round (p * 100) :: Int)) <> " in 100 that it is involved"
    subjectLine
      | not (Text.null (invSymbol inv)) = "; the diagnostics are about " <> invSymbol inv
      | otherwise = " with no symbol named; searched for " <> describePlan (invSearchPlan inv)
    bullets [] = ["  (none)"]
    bullets xs = map ("  - " <>) xs
    assertionNote loc =
      case [ p | (l, j) <- invJudgments inv, locId l == locId loc, Just p <- [assertsRequirement j] ] of
        (p : _)
          | p >= 0.6 -> "  (asserts a listed requirement)"
          | otherwise -> "  (exercises the code but asserts no listed requirement)"
        [] -> "  (body not read)"
    ownershipLines = case invOwnershipRequest inv of
      Nothing -> []
      Just request ->
        [ ""
        , "requires an ownership decision, which this investigation does not make:"
        , "  " <> requestReason request
        , "  proposed addition to the owned paths: "
            <> Text.intercalate ", " (requestPaths request)
        ]
          ++ [ "  - " <> at <> "  (" <> why <> ")" | (at, why) <- requestSites request ]
