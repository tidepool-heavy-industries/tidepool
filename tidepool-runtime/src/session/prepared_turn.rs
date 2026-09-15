//! Live session turns projected through the PREPARED-STG path (`--target`),
//! not the Core `--turn` path [`super::turn::run_turn`] drives.
//!
//! `session::turn`'s mechanism renders raw turn text into ONE scaffold module
//! per turn via a `{{TURN}}`/`{{TURN_STMT}}` splice
//! ([`super::workbench::resident_workbench_templates`]) and compiles it as
//! `LegacyCore`. That produces a `CoreExpr`, never a prepared-STG artifact, so
//! it cannot feed [`super::prepared::PreparedRuntime`]. This module is the
//! prepared-STG counterpart: each turn becomes its OWN tiny standalone `.hs`
//! FILE on disk, projected with `tidepool_extract_cmd::ExtractCmd`'s
//! `--target` mode (confirmed end-to-end by the S5/S6/A2/A6 cards — see
//! `haskell/test-prepared-stg/ImportProducer.hs` /
//! `ImportConsumer.hs` for the hand-authored precedent this module automates).
//!
//! One deliberate departure from `session::turn`'s `Tidepool.Session.Val.G<g>`
//! naming: that dotted, hierarchical name is only ever used for an INJECTED
//! `--inject-val` iface, never as the PRIMARY compile input. As the primary
//! input to `--target` mode it breaks, because
//! `haskell/src/Tidepool/GhcPipeline.hs`'s single-file pipeline derives its
//! own notion of "the target module" as `capitalize (takeBaseName path)` —
//! the bare file basename, never the parsed, dotted `module ... where` header
//! — and then filters compiled guts by STRING EQUALITY against that bare
//! name (`runPipeline: target module '<name>' not found among compiled
//! modules: [...]`, `GhcPipeline.hs` lines 657/945/975). A hierarchical
//! module at `Tidepool/Session/Val/G1.hs` compiles fine (GHC itself resolves
//! it) but is never recognized as ITS OWN target, because its real module
//! name (`"Tidepool.Session.Val.G1"`) never string-equals its file's bare
//! basename (`"G1"`) — confirmed by reproducing exactly that rejection while
//! building this mechanism. Ordinary flat module authoring — module name
//! equal to file basename, per [`turn_module_name`] — sidesteps it without
//! touching the extractor; see this module's top-level doc on the mechanism
//! for the finding recorded at the call site that hit it.
//!
//! A later turn's module plainly `import`s an earlier turn's module by name
//! (both live directly under the same `--include` session-root directory, so
//! GHC resolves them as ordinary home modules) and declares every
//! already-bound session entry as a retained-generation executable import
//! (`ExtractCmd::retained_generation`), so the projection excludes that
//! import's body from recovery and links it against the runtime's live
//! binding instead of recompiling it.
//!
//! Scope, deliberately: a turn here is always a single top-level binding (a
//! bare `x = e` Decl, or an Expr wrapped as `it = e`). There is no real GHC
//! turn classification (`TurnKind`/`classify_block`) — callers supply the
//! already-known shape directly ([`TurnForm`]) — and no IO/bind-effect turn
//! semantics. Session-root lifecycle (creation, cleanup) is the caller's job;
//! this module only ever writes into a directory it is given.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tidepool_extract_cmd::{BinError, ExtractCmd, ResolvedExtractBin, SpawnError};
use tidepool_repr::execution_schema::{
    parse_program, DecodeLimits, MachineImports, PreparedProgram, SymbolIdentity,
};
use tidepool_repr::{Generation, SessionVarId};

use super::prepared::{PreparedRuntime, PreparedRuntimeError};

/// The module name (and file basename) one turn's generation compiles under:
/// a FLAT name, deliberately not `session::turn`'s dotted
/// `Tidepool.Session.Val.G<g>` — see this module's top-level doc for why a
/// dotted name breaks as `--target` mode's PRIMARY compile input.
fn turn_module_name(generation: Generation) -> String {
    format!("SessionTurnG{}", generation.0)
}

/// A snapshot of one already-bound session entry, enough to render its
/// `import` line, its retained-generation declaration, and its
/// [`PreparedRuntime::install_prepared`] import pair. Read from
/// [`tidepool_codegen::binding_table::BindingTable::iter_live`] for every
/// turn after the first.
struct RetainedEntry {
    module_name: String,
    generation: u64,
    name: String,
    id: SessionVarId,
}

/// The raw shape of one turn's source. Real turn classification
/// (`TurnKind`/`classify_block`) is out of scope for this mechanism; callers
/// already know which shape they have.
pub enum TurnForm<'a> {
    /// A top-level declaration, verbatim (`"producerValue = [1, 2, 3]"`).
    Decl(&'a str),
    /// A bare expression, wrapped as `<introduces> = <expr>` (matching
    /// `session::turn`'s `it` convention for a reference turn with no
    /// explicit binder).
    Expr(&'a str),
}

/// Failure at any step of projecting and installing one prepared-STG turn.
#[derive(Debug, thiserror::Error)]
pub enum PreparedTurnError {
    #[error("resolving the tidepool-extract binary: {0}")]
    Bin(#[from] BinError),
    #[error("spawning tidepool-extract: {0}")]
    Spawn(#[from] SpawnError),
    #[error("writing turn module {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("tidepool-extract rejected the turn projecting {binder:?}:\n{stderr}")]
    Rejected { binder: String, stderr: String },
    #[error(transparent)]
    Parse(#[from] tidepool_repr::execution_schema::ParseError),
    #[error(transparent)]
    Runtime(#[from] PreparedRuntimeError),
}

/// Compiles and installs live session turns on one [`PreparedRuntime`]
/// through the prepared-STG projection, writing each turn's own module under
/// `session_root` and resolving every prior turn's binding as a
/// retained-generation import.
pub struct SessionTurns {
    bin: ResolvedExtractBin,
    session_root: PathBuf,
    /// Extra `--include` directories every turn's projection also searches
    /// (e.g. `haskell/lib`), beyond `session_root` itself.
    includes: Vec<PathBuf>,
}

impl SessionTurns {
    #[must_use]
    pub fn new(
        bin: ResolvedExtractBin,
        session_root: impl Into<PathBuf>,
        includes: Vec<PathBuf>,
    ) -> Self {
        Self {
            bin,
            session_root: session_root.into(),
            includes,
        }
    }

    /// Project and install the SESSION'S FIRST turn: builds a fresh
    /// [`PreparedRuntime`] from `form`'s own artifact (there is nothing yet
    /// to retain), sets its generation to `Generation(1)`, and binds
    /// `introduces` there. Every later turn goes through [`Self::run`]
    /// instead, against the runtime this returns.
    pub fn first(
        &self,
        form: TurnForm<'_>,
        introduces: &str,
    ) -> Result<(PreparedRuntime, SessionVarId), PreparedTurnError> {
        let generation = Generation(1);
        let prepared = self.project(generation, &[], form, introduces)?;
        // Captured before `from_prepared` takes ownership: the projection's
        // own declared entry IS this turn's binder, the only target it was
        // asked for.
        let entry = prepared.entry();
        let mut runtime = PreparedRuntime::from_prepared(prepared, MachineImports::default())?;
        let first_program = runtime.first_program()?;
        runtime.set_val_gen(generation)?;
        let id = runtime.bind_top(first_program, entry, introduces)?;
        Ok((runtime, id))
    }

    /// Project `form` as the next turn introducing `introduces`, advancing
    /// `runtime`'s generation, installing the result, and binding
    /// `introduces` at the new generation. Every session entry already bound
    /// on `runtime` is declared as a retained-generation import and given a
    /// real Haskell `import` of its `Tidepool.Session.Val.G<g>` module, so a
    /// later turn can reference an earlier one by name.
    pub fn run(
        &self,
        runtime: &mut PreparedRuntime,
        form: TurnForm<'_>,
        introduces: &str,
    ) -> Result<SessionVarId, PreparedTurnError> {
        let generation = runtime.advance_generation();
        // `entry.module.gen()` is exactly this mechanism's own generation
        // (set by `PreparedRuntime::bind_top` to `self.val_gen` at bind
        // time, i.e. the generation `Self::project` compiled that turn
        // under), so `turn_module_name` recovers the REAL GHC module name a
        // prior turn was compiled as — deliberately not
        // `entry.module.module_name()`, which is `BindingTable`'s own
        // `Tidepool.Session.Val.G<g>` bookkeeping name for the Core `--turn`
        // path and does not match what this mechanism actually compiled (see
        // this module's top-level doc).
        let retained: Vec<RetainedEntry> = runtime
            .bindings()
            .iter_live()
            .map(|entry| RetainedEntry {
                module_name: turn_module_name(entry.module.gen()),
                generation: entry.module.gen().0,
                name: entry.name.0.clone(),
                id: entry.id,
            })
            .collect();
        let prepared = self.project(generation, &retained, form, introduces)?;
        let import_pairs: Vec<(SymbolIdentity, SessionVarId)> = retained
            .iter()
            .map(|entry| {
                (
                    SymbolIdentity {
                        unit: "main".to_owned(),
                        module: entry.module_name.clone(),
                        namespace: "value".to_owned(),
                        occurrence: entry.name.clone(),
                        record_parent: None,
                    },
                    entry.id,
                )
            })
            .collect();
        Ok(runtime.turn(prepared, &import_pairs, introduces)?)
    }

    /// Write `form`'s module under `session_root` at `generation`, declare
    /// every entry in `retained` as a retained-generation import (both the
    /// real Haskell `import` and `ExtractCmd::retained_generation`), and
    /// project it with `tidepool-extract`'s `--target` mode.
    fn project(
        &self,
        generation: Generation,
        retained: &[RetainedEntry],
        form: TurnForm<'_>,
        introduces: &str,
    ) -> Result<PreparedProgram, PreparedTurnError> {
        let module_name = turn_module_name(generation);
        let module_path = self.session_root.join(format!("{module_name}.hs"));
        std::fs::create_dir_all(&self.session_root).map_err(|source| PreparedTurnError::Io {
            path: self.session_root.clone(),
            source,
        })?;

        // One `import M (a, b, ...)` line per prior turn's module, grouping
        // names in case a future caller ever binds more than one name per
        // generation (this card's own turns bind exactly one each).
        let mut by_module: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in retained {
            by_module
                .entry(entry.module_name.clone())
                .or_default()
                .push(entry.name.clone());
        }
        let mut imports_src = String::new();
        for (module_name, mut names) in by_module {
            names.sort();
            imports_src.push_str(&format!("import {module_name} ({})\n", names.join(", ")));
        }

        let body = match form {
            TurnForm::Decl(text) => text.to_string(),
            TurnForm::Expr(text) => format!("{introduces} = {text}"),
        };
        let source = format!("module {module_name} where\n\n{imports_src}\n{body}\n");
        std::fs::write(&module_path, source).map_err(|source| PreparedTurnError::Io {
            path: module_path.clone(),
            source,
        })?;

        let out_dir = self
            .session_root
            .join("out")
            .join(format!("G{}", generation.0));
        std::fs::create_dir_all(&out_dir).map_err(|source| PreparedTurnError::Io {
            path: out_dir.clone(),
            source,
        })?;

        let mut cmd = ExtractCmd::with_bin(self.bin.clone());
        cmd.input(&module_path)
            .output_dir(&out_dir)
            .target(introduces)
            .include(&self.session_root);
        for include in &self.includes {
            cmd.include(include);
        }
        for entry in retained {
            cmd.retained_generation(
                tidepool_extract_cmd::SymbolIdentity {
                    unit: "main".to_owned(),
                    module: entry.module_name.clone(),
                    namespace: "value".to_owned(),
                    occurrence: entry.name.clone(),
                    record_parent: None,
                },
                entry.generation,
            );
        }

        let endpoint = cmd.bind()?;
        let run = endpoint.execute(&cmd)?;
        if !run.output.status.success() {
            return Err(PreparedTurnError::Rejected {
                binder: introduces.to_owned(),
                stderr: run.stderr_lossy().into_owned(),
            });
        }

        let artifact_path = out_dir.join(format!("{introduces}.prepared.cbor"));
        let bytes = std::fs::read(&artifact_path).map_err(|source| PreparedTurnError::Io {
            path: artifact_path.clone(),
            source,
        })?;
        let requirements = tidepool_toolchain::prepared_artifact::production_requirements()?;
        Ok(parse_program(
            &bytes,
            &requirements,
            DecodeLimits::default(),
        )?)
    }
}
