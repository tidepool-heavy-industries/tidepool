//! Live session turns projected through the PREPARED-STG path (`--target`),
//! not the Core `--turn` path [`super::turn::run_turn`] drives.
//!
//! `session::turn`'s Core mechanism compiles a turn to a `CoreExpr`, which
//! cannot feed [`super::prepared::PreparedRuntime`]. Here each turn is its
//! own module `Tidepool.Session.Val.G<g>` (the same value-module name the
//! Core session uses), written under `session_root` at
//! [`SessionModule::relative_hs_path`] and projected with
//! `tidepool_extract_cmd::ExtractCmd`'s `--target` mode. Module text comes
//! from the one template owner, [`super::turn::prepared_turn_module`].
//!
//! A later turn imports each earlier turn's module by name (all live under
//! the same `--include` session root) and declares every live binding as a
//! retained-generation import, so the projection links it against the
//! runtime's live value instead of recompiling its body. Both the import
//! line and the retained identity come from the identity recorded on the
//! binding when it was bound ([`PreparedOrigin`]); nothing is reconstructed
//! from names, and [`PreparedRuntime::turn`] resolves and leases exactly the
//! imports the projected program declares.
//!
//! Scope, deliberately: a turn is a single top-level binding (a bare
//! `x = e` Decl, or an Expr wrapped as `it = e`). There is no GHC turn
//! classification and no IO/bind-effect turn semantics. Session-root
//! lifecycle is the caller's job; this module only writes into the
//! directory it is given.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tidepool_codegen::binding_table::{BoundValue, PreparedOrigin};
use tidepool_extract_cmd::{BinError, ExtractCmd, ResolvedExtractBin, SpawnError};
use tidepool_repr::execution_schema::{
    parse_program, DecodeLimits, MachineImports, PreparedProgram, SymbolIdentity,
};
use tidepool_repr::{Generation, SessionModule, SessionVarId};

use super::prepared::{PreparedRuntime, PreparedRuntimeError};

/// The raw shape of one turn's source. Callers already know which shape
/// they have; real turn classification is out of scope here.
pub enum TurnForm<'a> {
    /// A top-level declaration, verbatim (`"producerValue = [1, 2, 3]"`).
    Decl(&'a str),
    /// A bare expression, wrapped as `<introduces> = <expr>` (the Core
    /// session's `it` convention for a turn with no explicit binder).
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

/// One live prepared binding a later turn may import: its recorded identity
/// and the generation it was bound at.
struct Retained {
    identity: SymbolIdentity,
    generation: u64,
}

/// Compiles and installs live session turns on one [`PreparedRuntime`]
/// through the prepared-STG projection.
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
    /// [`PreparedRuntime`] from `form`'s own artifact (nothing to retain
    /// yet), starts generation 1, and binds `introduces` there. Every later
    /// turn goes through [`Self::run`] against the runtime this returns.
    pub fn first(
        &self,
        form: TurnForm<'_>,
        introduces: &str,
    ) -> Result<(PreparedRuntime, SessionVarId), PreparedTurnError> {
        let generation = Generation(1);
        let prepared = self.project(generation, &[], form, introduces)?;
        let entry = prepared.entry();
        let mut runtime = PreparedRuntime::from_prepared(prepared, MachineImports::default())?;
        let first_program = runtime.first_program()?;
        runtime.set_val_gen(generation)?;
        let id = runtime.bind_top(first_program, entry, introduces)?;
        Ok((runtime, id))
    }

    /// Project `form` as the next turn introducing `introduces`: advance
    /// `runtime`'s generation, declare every live binding as a
    /// retained-generation import, install the result, and bind
    /// `introduces` at the new generation.
    pub fn run(
        &self,
        runtime: &mut PreparedRuntime,
        form: TurnForm<'_>,
        introduces: &str,
    ) -> Result<SessionVarId, PreparedTurnError> {
        let generation = runtime.advance_generation();
        let retained: Vec<Retained> = runtime
            .bindings()
            .iter_live()
            .filter_map(|entry| match &entry.value {
                BoundValue::Prepared {
                    origin: Some(PreparedOrigin { identity, .. }),
                    ..
                } => Some(Retained {
                    identity: identity.clone(),
                    generation: entry.module.gen().0,
                }),
                _ => None,
            })
            .collect();
        let prepared = self.project(generation, &retained, form, introduces)?;
        Ok(runtime.turn(prepared, introduces)?)
    }

    /// Write `form`'s module under `session_root` at `generation`, declare
    /// every entry in `retained` as a retained-generation import (both the
    /// Haskell `import` and `ExtractCmd::retained_generation`), and project
    /// it with `tidepool-extract`'s `--target` mode.
    fn project(
        &self,
        generation: Generation,
        retained: &[Retained],
        form: TurnForm<'_>,
        introduces: &str,
    ) -> Result<PreparedProgram, PreparedTurnError> {
        let module = SessionModule::val(generation);
        let module_path = self.session_root.join(module.relative_hs_path());
        let module_dir = module_path
            .parent()
            .map_or_else(|| self.session_root.clone(), std::path::Path::to_path_buf);
        std::fs::create_dir_all(&module_dir).map_err(|source| PreparedTurnError::Io {
            path: module_dir.clone(),
            source,
        })?;

        let mut by_module: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in retained {
            by_module
                .entry(entry.identity.module.clone())
                .or_default()
                .push(entry.identity.occurrence.clone());
        }
        let imports: Vec<(String, Vec<String>)> = by_module
            .into_iter()
            .map(|(module, mut names)| {
                names.sort();
                names.dedup();
                (module, names)
            })
            .collect();
        let body = match form {
            TurnForm::Decl(text) => text.to_string(),
            TurnForm::Expr(text) => format!("{introduces} = {text}"),
        };
        let source = super::turn::prepared_turn_module(&module.module_name(), &imports, &body);
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
            cmd.retained_generation(extract_identity(&entry.identity), entry.generation);
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

/// The extractor request's copy of a symbol identity. `tidepool-extract-cmd`
/// does not depend on `tidepool-repr`, so this is the one conversion site.
fn extract_identity(identity: &SymbolIdentity) -> tidepool_extract_cmd::SymbolIdentity {
    tidepool_extract_cmd::SymbolIdentity {
        unit: identity.unit.clone(),
        module: identity.module.clone(),
        namespace: identity.namespace.clone(),
        occurrence: identity.occurrence.clone(),
        record_parent: identity.record_parent.clone(),
    }
}
