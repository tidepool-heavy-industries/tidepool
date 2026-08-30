use std::future::Future;

/// One already-parsed runnable block and its position in the assistant
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

/// Execute parsed blocks strictly in source order. Successful prefix commits
/// remain visible when a later block stops or fails; no later block is called.
pub async fn run_block_sequence<Committed, Stopped, Error, Run, RunFuture>(
    blocks: Vec<String>,
    mut run: Run,
) -> BlockSequenceOutcome<Committed, Stopped, Error>
where
    Run: FnMut(ParsedBlock) -> RunFuture,
    RunFuture: Future<Output = Result<BlockExecution<Committed, Stopped>, Error>>,
{
    let total = blocks.len();
    let mut committed = Vec::with_capacity(total);
    for (index, source) in blocks.into_iter().enumerate() {
        let block = ParsedBlock {
            ordinal: index + 1,
            total,
            source,
        };
        match run(block.clone()).await {
            Ok(BlockExecution::Committed(output)) => {
                committed.push(CommittedBlock { block, output });
            }
            Ok(BlockExecution::Stopped(outcome)) => {
                return BlockSequenceOutcome::Stopped {
                    committed,
                    block,
                    outcome,
                };
            }
            Err(error) => {
                return BlockSequenceOutcome::Failed {
                    committed,
                    block,
                    error,
                };
            }
        }
    }
    BlockSequenceOutcome::Completed { committed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn commits_in_order() {
        let outcome = run_block_sequence(vec!["a".into(), "b".into()], |block| async move {
            Ok::<_, ()>(BlockExecution::<_, ()>::Committed(format!(
                "{}:{}",
                block.ordinal, block.source
            )))
        })
        .await;
        assert_eq!(
            outcome,
            BlockSequenceOutcome::Completed {
                committed: vec![
                    CommittedBlock {
                        block: ParsedBlock {
                            ordinal: 1,
                            total: 2,
                            source: "a".into(),
                        },
                        output: "1:a".into(),
                    },
                    CommittedBlock {
                        block: ParsedBlock {
                            ordinal: 2,
                            total: 2,
                            source: "b".into(),
                        },
                        output: "2:b".into(),
                    },
                ],
            }
        );
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
