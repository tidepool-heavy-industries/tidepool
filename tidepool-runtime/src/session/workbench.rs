//! Frontend-neutral mechanics for a resident Haskell workbench.
//!
//! This module owns the pieces every resident frontend needs before it can
//! apply its own policy: source-item classification, meta-command tokenization,
//! and prefix-preserving ordered execution. It deliberately does not know
//! about MCP response shapes, actor conversations, effect settlement, or
//! presentation.

use std::future::Future;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    assemble_bind_module, assemble_expression_module, insert_preamble_imports, ExpressionLift,
    TemplateSelector, TurnTemplate, DECL_TEMPLATE_SOURCE,
};

/// One ordered request against a persistent Haskell workbench.
///
/// This is transport-neutral despite the JSON-shaped optional input: MCP,
/// provider-native fenced execution, tests, and future frontends must agree on
/// item sequencing and input mounting without copying the request contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkbenchRequest {
    /// GHCi-capable items run in source order. Execution stops at the first
    /// rejected or suspended item while preserving earlier commits.
    pub items: Vec<String>,
    /// Optional structured payload mounted as @input :: Aeson.Value@.
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    /// Request the frontend's expanded diagnostic receipt when supported.
    #[serde(default)]
    pub verbose: Option<bool>,
}

/// One tokenized `:command`. Frontends interpret the name and arguments they
/// own; tokenization itself has one implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaCommandLine {
    pub name: String,
    pub arguments: String,
}

impl MetaCommandLine {
    /// Parse a command with an optional leading colon.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let command = raw.trim();
        let command = command.strip_prefix(':').unwrap_or(command).trim();
        if command.is_empty() {
            return Err("empty workbench command".to_string());
        }
        let (name, arguments) = command
            .split_once(char::is_whitespace)
            .map_or((command, ""), |(name, arguments)| (name, arguments.trim()));
        Ok(Self {
            name: name.to_string(),
            arguments: arguments.to_string(),
        })
    }
}

/// The policy-free lexical shape of one workbench item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkbenchItem {
    /// A declaration form whose leading keyword makes it unambiguous.
    Declaration(String),
    /// Haskell requiring GHC classification as a declaration, bind, or
    /// expression.
    Haskell(String),
    /// A tokenized meta-command interpreted by the consuming frontend.
    Command(MetaCommandLine),
}

/// Perform only the lexical classification that is stable across resident
/// frontends. GHC remains authoritative for ambiguous Haskell.
pub fn classify_workbench_item(source: &str) -> Result<WorkbenchItem, String> {
    let source = source.trim();
    if source.is_empty() {
        return Ok(WorkbenchItem::Declaration(String::new()));
    }
    if source.starts_with(':') {
        return MetaCommandLine::parse(source).map(WorkbenchItem::Command);
    }

    const DECLARATION_PREFIXES: &[&str] = &[
        "data ",
        "newtype ",
        "type ",
        "class ",
        "instance ",
        "infixl ",
        "infixr ",
        "infix ",
        "foreign ",
        "import ",
        "default ",
        "{-# ",
    ];
    if DECLARATION_PREFIXES
        .iter()
        .any(|prefix| source.starts_with(prefix))
    {
        Ok(WorkbenchItem::Declaration(source.to_string()))
    } else {
        Ok(WorkbenchItem::Haskell(source.to_string()))
    }
}

/// Build the canonical raw-value templates for a resident actor workbench.
/// GHC selects declaration, bind, or expression and tries the two expression
/// lifts in order. Presentation-heavy frontends may post-process outcomes,
/// but should not grow another source assembly path.
#[must_use]
pub fn resident_workbench_templates(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
) -> Vec<TurnTemplate> {
    let preamble = insert_preamble_imports(preamble, imports);
    vec![
        TurnTemplate {
            kind: TemplateSelector::Decl,
            source: DECL_TEMPLATE_SOURCE.to_string(),
        },
        TurnTemplate {
            kind: TemplateSelector::Bind,
            source: assemble_bind_module(
                &preamble,
                "",
                "__result",
                effect_stack,
                "{{TURN_STMT}}",
                "{{BINDERS}}",
                false,
            ),
        },
        TurnTemplate {
            kind: TemplateSelector::BindDiscard,
            source: assemble_bind_module(
                &preamble,
                "",
                "__result",
                effect_stack,
                "{{TURN_STMT}}",
                "()",
                false,
            ),
        },
        TurnTemplate {
            kind: TemplateSelector::Expr,
            source: assemble_expression_module(
                &preamble,
                "__result",
                effect_stack,
                "{{TURN}}",
                ExpressionLift::Effectful,
            ),
        },
        TurnTemplate {
            kind: TemplateSelector::Expr,
            source: assemble_expression_module(
                &preamble,
                "__result",
                effect_stack,
                "{{TURN}}",
                ExpressionLift::Pure,
            ),
        },
    ]
}

/// Suspension-safe cursor over one ordered unit of work.
///
/// A consumer commits only the current item. Leaving it uncommitted is the
/// stop/park operation, so the successful prefix and never-run suffix cannot
/// drift apart while the cursor is stored and later resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkSequence<Item, Committed> {
    items: Vec<Item>,
    committed: Vec<Committed>,
}

impl<Item, Committed> WorkSequence<Item, Committed> {
    #[must_use]
    pub fn new(items: Vec<Item>) -> Self {
        let capacity = items.len();
        Self {
            items,
            committed: Vec::with_capacity(capacity),
        }
    }

    #[must_use]
    pub fn position(&self) -> usize {
        self.committed.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn current(&self) -> Option<&Item> {
        self.items.get(self.position())
    }

    #[must_use]
    pub fn item(&self, index: usize) -> Option<&Item> {
        self.items.get(index)
    }

    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    #[must_use]
    pub fn committed(&self) -> &[Committed] {
        &self.committed
    }

    pub fn committed_mut(&mut self) -> &mut [Committed] {
        &mut self.committed
    }

    /// Commit the current item and advance. Callers invoke this only after
    /// obtaining [`Self::current`]; the returned zero-based index lets them
    /// attach presentation metadata without maintaining another cursor.
    pub fn commit_next(&mut self, output: Committed) -> usize {
        let index = self.position();
        self.committed.push(output);
        index
    }

    #[must_use]
    pub fn into_committed(self) -> Vec<Committed> {
        self.committed
    }
}

/// One already-parsed runnable block and its position in an assistant
/// response. Parsing belongs to `tidepool-model-output`; this type begins the
/// execution contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBlock {
    pub ordinal: usize,
    pub total: usize,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedBlock<T> {
    pub block: ParsedBlock,
    pub output: T,
}

/// A block either commits and permits the next block to run, or parks/settles
/// the sequence with a caller-defined terminal value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockExecution<Committed, Stopped> {
    Committed(Committed),
    Stopped(Stopped),
}

/// Prefix-preserving result of one assistant response's ordered Haskell
/// sequence. On stop or failure, blocks after `block` were never invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockSequenceOutcome<Committed, Stopped, Error> {
    Completed {
        committed: Vec<CommittedBlock<Committed>>,
    },
    Stopped {
        committed: Vec<CommittedBlock<Committed>>,
        block: ParsedBlock,
        outcome: Stopped,
    },
    Failed {
        committed: Vec<CommittedBlock<Committed>>,
        block: ParsedBlock,
        error: Error,
    },
}

/// Execute parsed blocks strictly in source order.
pub async fn run_block_sequence<Committed, Stopped, Error, Run, RunFuture>(
    blocks: Vec<String>,
    mut run: Run,
) -> BlockSequenceOutcome<Committed, Stopped, Error>
where
    Run: FnMut(ParsedBlock) -> RunFuture,
    RunFuture: Future<Output = Result<BlockExecution<Committed, Stopped>, Error>>,
{
    let total = blocks.len();
    let blocks = blocks
        .into_iter()
        .enumerate()
        .map(|(index, source)| ParsedBlock {
            ordinal: index + 1,
            total,
            source,
        })
        .collect();
    let mut sequence = WorkSequence::new(blocks);

    while let Some(block) = sequence.current().cloned() {
        match run(block.clone()).await {
            Ok(BlockExecution::Committed(output)) => {
                sequence.commit_next(CommittedBlock { block, output });
            }
            Ok(BlockExecution::Stopped(outcome)) => {
                return BlockSequenceOutcome::Stopped {
                    committed: sequence.into_committed(),
                    block,
                    outcome,
                };
            }
            Err(error) => {
                return BlockSequenceOutcome::Failed {
                    committed: sequence.into_committed(),
                    block,
                    error,
                };
            }
        }
    }
    BlockSequenceOutcome::Completed {
        committed: sequence.into_committed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_stable_lexical_shapes() {
        assert_eq!(
            classify_workbench_item(" data X = X ").unwrap(),
            WorkbenchItem::Declaration("data X = X".into())
        );
        assert_eq!(
            classify_workbench_item("answer = 42").unwrap(),
            WorkbenchItem::Haskell("answer = 42".into())
        );
        assert_eq!(
            classify_workbench_item(":type answer").unwrap(),
            WorkbenchItem::Command(MetaCommandLine {
                name: "type".into(),
                arguments: "answer".into(),
            })
        );
    }

    #[test]
    fn cursor_retains_prefix_without_consuming_parked_item() {
        let mut sequence = WorkSequence::new(vec!["a", "park", "later"]);
        assert_eq!(sequence.current(), Some(&"a"));
        assert_eq!(sequence.commit_next(1), 0);
        assert_eq!(sequence.current(), Some(&"park"));
        assert_eq!(sequence.committed(), &[1]);
        assert_eq!(sequence.items()[sequence.position()..], ["park", "later"]);
    }

    #[tokio::test]
    async fn stop_preserves_prefix_and_never_runs_suffix() {
        let outcome = run_block_sequence(
            vec!["a".into(), "park".into(), "must-not-run".into()],
            |block| async move {
                if block.source == "park" {
                    Ok::<_, ()>(BlockExecution::Stopped("suspended"))
                } else {
                    Ok(BlockExecution::Committed(block.source.clone()))
                }
            },
        )
        .await;
        assert!(matches!(
            outcome,
            BlockSequenceOutcome::Stopped {
                committed,
                block: ParsedBlock { ordinal: 2, .. },
                outcome: "suspended",
            } if committed.len() == 1
        ));
    }
}
