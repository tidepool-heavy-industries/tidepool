# Research: ast-grep Rust API and Haskell ADT Design

**Priority:** MEDIUM — enables structural search/replace effects
**Status:** DRAFT — design for ast-grep-core integration
**Reference:** [ast-grep-core docs.rs](https://docs.rs/ast-grep-core/latest/ast_grep_core/)

## Summary

This document outlines the design for integrating `ast-grep-core` into the Tidepool effect system. We propose a Haskell ADT that mirrors the structural search capabilities of `ast-grep`, allowing Haskell programs to perform AST-based queries and transformations via a specialized effect.

## ast-grep Rust API Overview

The `ast-grep-core` crate provides the foundation for structural matching. Key types include:

### 1. Node (SgNode)
Represents a node in the syntax tree, wrapping a `tree-sitter` node with language and source context.

```rust
pub struct Node<'r, L: Language> {
    pub(crate) inner: tree_sitter::Node<'r>,
    pub(crate) source: &'r [u8],
    pub(crate) lang: L,
}
```

**Key Methods:**
- `kind()`: Returns the node type (e.g., "function_item").
- `text()`: Returns the source text of the node.
- `range()`: Returns start/end positions.
- `children()` / `named_children()`: Iterators for traversal.
- `find(matcher)` / `find_all(matcher)`: Search for patterns within this node.

### 2. Matcher Trait
The core interface for anything that can match a `Node`.

```rust
pub trait Matcher<L: Language> {
    fn match_node_with_env<'tree>(
        &self,
        node: Node<'tree, L>,
        env: &mut Cow<Env<'tree, L>>,
    ) -> Option<MatchResult<'tree, L>>;
}
```

### 3. Pattern
Usually constructed from a string, but represents a template tree.

```rust
pub struct Pattern<L: Language> {
    pub(crate) selector: Option<NodeKind>,
    pub(crate) root: PatternNode,
    pub(crate) lang: L,
}
```

### 4. Rule (SerializableRule)
From `ast_grep_config`, these provide the boolean logic and relational matching (has, inside, precedes, follows).

```rust
pub enum SerializableRule {
    All(Vec<SerializableRule>),
    Any(Vec<SerializableRule>),
    Not(Box<SerializableRule>),
    Inside(InsideRule),
    Has(HasRule),
    Precedes(RelationalRule),
    Follows(RelationalRule),
    Pattern(String),
    Kind(String),
    Regex(String),
}
```

## Proposed Haskell ADT

To enable type-safe pattern construction in Haskell, we define a structured `Pattern` and `Rule` ADT.

### 1. Syntax Node
Mirroring the `SgNode` structure for results.

```haskell
data Range = Range 
  { startPos :: (Int, Int)
  , endPos   :: (Int, Int)
  } deriving (Show, Eq, Generic, ToCore, FromCore)

data SyntaxNode = SyntaxNode
  { nodeKind     :: String
  , nodeText     :: String
  , nodeRange    :: Range
  , nodeChildren :: [SyntaxNode]
  } deriving (Show, Eq, Generic, ToCore, FromCore)
```

### 2. Pattern ADT
A structural representation of code patterns, avoiding raw strings where possible.

```haskell
data Pattern
  = PNode String [Pattern]  -- Match node of kind with specific children
  | PLeaf String            -- Match exact text
  | PCapture String         -- $VAR (matches single node)
  | PListCapture String     -- $$$VAR (matches multiple nodes)
  | PWildcard             -- _ (matches anything)
  deriving (Show, Eq, Generic, ToCore, FromCore)
```

### 3. Rule ADT
The high-level logic for combining patterns and constraints.

```haskell
data Rule
  = RPattern Pattern
  | RKind String
  | RRegex String
  | RAll [Rule]
  | RAny [Rule]
  | RNot Rule
  | RInside RelationalRule
  | RHas RelationalRule
  | RPrecedes RelationalRule
  | RFollows RelationalRule
  deriving (Show, Eq, Generic, ToCore, FromCore)

data RelationalRule = RelationalRule
  { relRule   :: Rule
  , relStopBy :: Maybe StopBy
  , relField  :: Maybe String
  } deriving (Show, Eq, Generic, ToCore, FromCore)

data StopBy = StopByEnd | StopByNeighbor | StopByRule Rule
  deriving (Show, Eq, Generic, ToCore, FromCore)
```

### 4. Language Enum
Supported tree-sitter languages.

```haskell
data Language
  = Rust | Haskell | JavaScript | TypeScript | Python | Go | C | Cpp
  deriving (Show, Eq, Generic, ToCore, FromCore)
```

## Proposed Effect Interface

The `AstGrep` effect allows Haskell to interact with the search engine.

```haskell
newtype Capture = Capture String deriving (Show, Eq, Ord, Generic, ToCore, FromCore)

data Match = Match
  { matchedNode :: SyntaxNode
  , captures    :: [(Capture, SyntaxNode)]
  } deriving (Show, Eq, Generic, ToCore, FromCore)

data AstGrepEffect a where
  ParseFile :: FilePath -> Language -> AstGrepEffect SyntaxNode
  FindAll   :: Rule -> SyntaxNode -> AstGrepEffect [Match]
  Replace   :: Rule -> String -> SyntaxNode -> AstGrepEffect String
```

## Rust Handler Sketch

The Rust-side handler will bridge the Haskell ADTs to `ast-grep-core` types.

### Crate Dependencies
- `ast-grep-core`: Core matching logic.
- `ast-grep-language`: Language definitions.
- `tidepool-bridge`: For `FromCore`/`ToCore` conversion.

### Implementation Logic
The handler must recursively convert the Haskell `Rule` into an `ast_grep_core::Matcher`. 

```rust
pub struct AstGrepHandler;

impl EffectHandler for AstGrepHandler {
    type Request = AstGrepReq;

    fn handle(&mut self, req: AstGrepReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            AstGrepReq::ParseFile(path, lang) => {
                let sg_lang = to_sg_lang(lang);
                let content = std::fs::read_to_string(path)
                    .map_err(|e| EffectError::Handler(e.to_string()))?;
                let doc = sg_lang.parse_doc(&content);
                cx.respond(to_syntax_node(doc.root()))
            }
            AstGrepReq::FindAll(rule, node) => {
                let matcher = compile_rule(rule);
                let sg_node = from_syntax_node(node);
                let matches: Vec<Match> = sg_node.find_all(matcher)
                    .map(to_match)
                    .collect();
                cx.respond(matches)
            }
            // ... Replace implementation
        }
    }
}
```

**Note on Pattern conversion:** Since `ast-grep-core` prefers string patterns for parsing, the `compile_rule` function might need to serialize the Haskell `Pattern` ADT back to a string format that `ast-grep` understands, or use lower-level `ast-grep-core` APIs to construct `PatternNode` structures directly.

## Open Questions / Tradeoffs

1. **Pattern Serialization:** Should we serialize the Haskell `Pattern` ADT to a string for `ast-grep` to parse, or try to build the `PatternNode` tree manually? Serialization is easier but might introduce parsing overhead.
2. **Performance:** `ast-grep` is highly optimized. Converting between Haskell `SyntaxNode` and Rust `SgNode` repeatedly might be expensive. We should consider keeping a handle to the Rust `SgNode` in the Haskell side if possible (e.g., via an opaque `StablePtr` or ID).
3. **Template representation:** The `Replace` effect currently takes a `String` template. Should this also be an ADT to ensure type safety for captures?
4. **Field support:** `ast-grep` supports matching specific fields (e.g., the `name` field of a `function_item`). Our `RelationalRule` includes `relField`, but we need to ensure this is correctly mapped to tree-sitter fields.
