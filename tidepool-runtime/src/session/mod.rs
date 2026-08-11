//! Session declaration accumulation.
//!
//! A [`SessionLib`] accumulates user declarations as **source text** across
//! turns. Each `define` turn:
//!   1. extracts the declaration's binder names **from GHC** (never a Rust-side
//!      Haskell parser) — see [`turn::run_turn`]'s `Decl` verdict;
//!   2. appends a [`render::DeclTurn`] to the ordered log and bumps the
//!      [`Generation`];
//!   3. regenerates the whole `Tidepool.Session.Lib.G<g>` module as a pure
//!      function of the log (selective re-export) and writes it atomically
//!      into the session include tree.
//!
//! Later turns see prior declarations by importing `Tidepool.Session.Lib.G<g>`
//! through the batch-compile pipeline ([`crate::compile_haskell`]) with the
//! session dir on the include path at highest precedence. The value and type
//! planes are handled elsewhere; this module is a standalone, usable
//! declaration REPL on its own.

pub mod engine;
pub mod persistent;
pub mod render;
pub mod resident;
pub mod turn;

pub use persistent::PersistentSession;

pub use engine::{
    extract_ask_request, AbortOutcome, EngineConfig, GateDispatcher, OutputSink, RenderPolicy,
    ResumeOutcome, Retention, SessionEngine, StartError, StartTurn, TurnOutcome,
};

pub use resident::{ResidentError, ResidentOutcome, ResidentSession};

pub use turn::{
    classify_block, compile_session_turn, render_template, run_turn, run_turn_batch,
    BatchItemFailure, BatchTurnItem, BoundBinder, CompiledTurn, SessionBind, SessionTurnResult,
    TemplateSelector, TurnBatchRequest, TurnBatchResult, TurnClassification, TurnKind, TurnRequest,
    TurnResult, TurnTemplate, ValueTier, DECL_TEMPLATE_SOURCE,
};

use std::path::{Path, PathBuf};

use tidepool_extract_cmd::{ExitVerdict, ExtractCmd};
use tidepool_repr::{Generation, SessionId, SessionModule};

pub use render::{
    subtract_import_list_names, DeclLog, DeclTurn, ExportItem, ModuleEnv, RenderedModule,
};

/// The stdlib include dir a candidate gen module needs at validation time, or
/// `None` when the caller's include path already carries one.
///
/// The candidate module imports stdlib sources (`Tidepool.Data.Text`, …), so a
/// stdlib root must be on the GHC search path or validation fails with a bare
/// "Could not find module" that reads like the user's declaration is wrong.
/// Production callers pass the server's stdlib dir through
/// [`SessionLib::with_validation_include`]; when they haven't, fall back to the
/// one locator ([`crate::toolchain::locate_stdlib`]).
///
/// A missing stdlib is a CONFIGURATION error and says so, instead of
/// surfacing as a downstream GHC scope error.
fn stdlib_include_for_validation(
    include: &[PathBuf],
) -> Result<Option<PathBuf>, crate::toolchain::ToolchainError> {
    if include.iter().any(|d| crate::toolchain::is_stdlib_root(d)) {
        return Ok(None);
    }
    crate::toolchain::locate_stdlib(&crate::toolchain::StdlibFallbacks::default())
        .map(|loc| Some(loc.dir))
}

/// Errors from the declaration-accumulation path.
#[derive(thiserror::Error, Debug)]
pub enum SessionError {
    /// Filesystem I/O failure (creating the session root, writing/reading a
    /// gen module, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// GHC binder extraction failed (parse error in the declaration, or the
    /// extractor was unavailable / produced unreadable output).
    #[error("binder extraction failed: {0}")]
    BinderExtraction(String),
    /// The candidate gen module failed to type-check via GHC. The declaration
    /// log has been rolled back; the session remains usable.
    #[error("declaration type-check failed: {0}")]
    ValidationFailed(String),
    /// The extractor exited non-zero and its stdout did not parse as the
    /// diagnostics report — a stale/skewed extractor build, not the user's
    /// declaration (mirrors `CompileError::MalformedDiagnostics` →
    /// `FailureClass::VersionSkew`).
    #[error("malformed extract diagnostics: {0}")]
    MalformedDiagnostics(String),
    /// The toolchain itself is misconfigured — no extract, no stdlib, or a
    /// skewed extract/stdlib pair. Never caused by the user's declaration.
    #[error("toolchain: {0}")]
    Toolchain(#[from] crate::toolchain::ToolchainError),
}

/// [`crate::CompileError`] → [`SessionError`]: an environment problem stays
/// `Io`, a stale/skewed extractor stays `MalformedDiagnostics`, and every
/// user-Haskell-shaped rejection collapses into `BinderExtraction`.
fn compile_error_to_session_error(e: crate::CompileError) -> SessionError {
    use crate::CompileError;
    match e {
        CompileError::Io(io) => SessionError::Io(io),
        CompileError::MalformedDiagnostics(msg) => SessionError::MalformedDiagnostics(msg),
        CompileError::Diagnostics(diags) => SessionError::BinderExtraction(
            diags
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        CompileError::ExtractFailed(msg) => SessionError::BinderExtraction(msg),
        CompileError::MissingOutput(path) => SessionError::BinderExtraction(format!(
            "extractor produced no turn output: {}",
            path.display()
        )),
        CompileError::ReadError(err) => SessionError::BinderExtraction(err.to_string()),
        CompileError::IOTypeDetected => {
            SessionError::BinderExtraction("IO type detected in decl turn".to_string())
        }
    }
}

/// A resident session's declaration library. Owns the ordered decl log, the
/// monotonic generation, and the on-disk include tree.
pub struct SessionLib {
    id: SessionId,
    /// Root of the session include tree: gen modules live at
    /// `<root>/Tidepool/Session/Lib/G<g>.hs`. Placed on the GHC include path at
    /// highest precedence so they shadow any same-named module.
    root: PathBuf,
    log: DeclLog,
    env: ModuleEnv,
    /// Extra `--include` dirs for decl binder-extraction + candidate validation,
    /// beyond `root` and the auto-derived stdlib `lib/`. When `env` imports
    /// modules that live outside the stdlib tree — notably the generated
    /// `Tidepool.Effects` (so a `session_def` helper can be `M`-typed and call
    /// effect verbs) — those dirs must be here or validation fails to resolve
    /// the import. Empty by default (the pure `standalone_default` surface needs
    /// only the stdlib). Set via [`with_validation_include`](Self::with_validation_include).
    extra_include: Vec<PathBuf>,
}

impl SessionLib {
    /// Open a session rooted at `root` (created if absent). `env` controls the
    /// generated modules' pragma/import surface; pass
    /// [`ModuleEnv::standalone_default`] for the pure Lane-A surface.
    pub fn open(
        id: SessionId,
        root: impl Into<PathBuf>,
        env: ModuleEnv,
    ) -> Result<SessionLib, SessionError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(SessionLib {
            id,
            root,
            log: DeclLog::new(),
            env,
            extra_include: Vec::new(),
        })
    }

    /// Add include dirs used when extracting binders and validating candidate
    /// gen modules (e.g. the `Tidepool.Effects` dir + stdlib `lib/` when `env`
    /// is the full-eval [`session_decl_module_env`](crate) surface). Without
    /// these, a decl importing `Tidepool.Effects` fails validation with
    /// "Could not find module `Tidepool.Effects'". Chainable on `open`.
    #[must_use]
    pub fn with_validation_include(mut self, dirs: Vec<PathBuf>) -> Self {
        self.extra_include = dirs;
        self
    }

    /// The include directory to place on the GHC search path (highest precedence).
    #[must_use]
    pub fn include_dir(&self) -> &Path {
        &self.root
    }

    /// The current generation (`Generation(0)` until the first `define`).
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.log.generation()
    }

    /// The current session-library module, or `None` before any declaration.
    #[must_use]
    pub fn current_module(&self) -> Option<SessionModule> {
        let g = self.log.generation();
        (g.0 > 0).then(|| SessionModule::lib(g))
    }

    /// The `import Tidepool.Session.Lib.G<g>` line a turn should prepend to see
    /// the accumulated declarations, or `None` if the session is empty.
    #[must_use]
    pub fn import_line(&self) -> Option<String> {
        self.current_module()
            .map(|m| format!("import {}", m.module_name()))
    }

    /// Source text of the most recent declaration turn that introduces a type or
    /// class named `name` (via `ExportItem::Type` or `ExportItem::Class`).
    /// Returns `None` if no such declaration exists in the session. Used by
    /// `:i <Type>` to surface session-defined type shapes.
    #[must_use]
    pub fn decl_type_source(&self, name: &str) -> Option<&str> {
        self.log
            .turns
            .iter()
            .rev()
            .find(|t| {
                t.items.iter().any(|item| match item {
                    ExportItem::Type { name: n, .. } | ExportItem::Class { name: n, .. } => {
                        n == name
                    }
                    ExportItem::Value { .. } => false,
                })
            })
            .and_then(|t| t.sources.first())
            .map(String::as_str)
    }

    /// Source text of the most recent declaration turn that introduces a
    /// value/function named `name` (via `ExportItem::Value`). Returns `None` if
    /// no such declaration exists. Used by `:i <name>` to surface
    /// session-defined function/value definitions.
    #[must_use]
    pub fn decl_value_source(&self, name: &str) -> Option<&str> {
        self.log
            .turns
            .iter()
            .rev()
            .find(|t| {
                t.items
                    .iter()
                    .any(|item| matches!(item, ExportItem::Value { name: n } if n == name))
            })
            .and_then(|t| t.sources.first())
            .map(String::as_str)
    }

    /// The replayable decl half of a `:program` notebook repaint: turn source
    /// texts in log order, with fully-superseded turns dropped so a name
    /// redefined across separate turns emits only its LATEST definition
    /// instead of overlapping clauses GHC would reject. See
    /// [`DeclLog::replayable_sources`] for the exact latest-wins rule (it mirrors
    /// the eval-time module scoping).
    #[must_use]
    pub fn decl_sources(&self) -> Vec<&str> {
        self.log.replayable_sources()
    }

    /// Names of every value/function binder introduced across all declaration
    /// turns (all generations, not just the current one). Used by the eval
    /// assembler to hide session-defined names from the Prelude import so a
    /// user function named `over`/`view`/etc. resolves unambiguously to the
    /// session decl rather than the Prelude re-export.
    #[must_use]
    pub fn decl_value_names(&self) -> Vec<&str> {
        // Latest-wins with retraction: a name removed by a later retraction turn
        // (its binding migrated to the value plane) is no longer a decl-plane
        // value, so it drops out.
        let mut live: Vec<&str> = Vec::new();
        for turn in &self.log.turns {
            for r in &turn.retracts {
                live.retain(|n| *n != r.as_str());
            }
            for item in &turn.items {
                if let ExportItem::Value { name } = item {
                    live.retain(|n| *n != name.as_str());
                    live.push(name.as_str());
                }
            }
        }
        live
    }

    /// Names of every type/class introduced across declaration turns. Hidden
    /// from the Prelude/Library/effect-verb imports (alongside
    /// [`Self::decl_value_names`]) so a session `data Foo`/`class Foo` shadows a
    /// same-named library type instead of becoming an ambiguous occurrence
    /// (e.g. a session `data Hit` vs the `Library` `Hit`).
    #[must_use]
    pub fn decl_type_names(&self) -> Vec<&str> {
        self.log
            .turns
            .iter()
            .flat_map(|t| t.items.iter())
            .filter_map(|item| match item {
                ExportItem::Type { name, .. } | ExportItem::Class { name, .. } => {
                    Some(name.as_str())
                }
                ExportItem::Value { .. } => None,
            })
            .collect()
    }

    /// The currently in-scope declaration heads paired with the generation of
    /// their latest defining turn — the decl-plane half of the live
    /// `tidepool://session/bindings` resource snapshot. Latest-wins across turns.
    #[must_use]
    pub fn current_decl_heads(&self) -> Vec<(String, u64)> {
        self.log.current_heads()
    }

    /// A cache salt unique to `(session, generation)`. Threaded into
    /// [`crate::compile_haskell_salted`] so two sessions' identical-text modules
    /// don't collide and a generation bump invalidates correctly.
    #[must_use]
    pub fn cache_salt(&self) -> String {
        format!("session:{}:gen:{}", self.id, self.log.generation())
    }

    /// Append a declaration turn. Extracts binder names from GHC, regenerates the
    /// gen-versioned module, writes it atomically, validates it type-checks via GHC,
    /// and returns the new generation.
    ///
    /// `decl_text` may contain several top-level declarations; their binders are
    /// classified together as this turn's introduced names.
    ///
    /// Empty / whitespace-only `decl_text` is a **no-op**: returns the current
    /// generation without bumping it.
    ///
    /// Syntactically-invalid declarations are rejected here (GHC's parser fails →
    /// `SessionError::BinderExtraction`) and the log is left untouched.
    ///
    /// Declarations that parse but fail to type-check are also rejected: the
    /// candidate gen module is compiled via a thin wrapper; on failure the log is
    /// rolled back and the gen module file deleted so subsequent turns cannot pick
    /// up a stale poisoned module (`SessionError::ValidationFailed`). This covers
    /// ALL declaration kinds — `data`, `class`, `instance`, `type`, and values.
    pub fn define(&mut self, decl_text: &str) -> Result<Generation, SessionError> {
        self.define_batch(&[decl_text])
    }

    /// [`Self::define`] plus scoping the declaration against live session
    /// values: `import_modules` (current `Val.G<g>` per still-live name) are
    /// imported unqualified into the rendered decl module; `inject_modules`
    /// (every still-live `Val.G<g>`, including shadowed gens) are passed to the
    /// extract as `--inject-val` so their `.hi` ifaces resolve at validation
    /// time. Lets a decl (`f x = … g …`) reference a prior session value `g`
    /// the way a genuine GHCi top-level definition would.
    pub fn define_with_vals(
        &mut self,
        decl_text: &str,
        import_modules: &[String],
        inject_modules: &[String],
    ) -> Result<Generation, SessionError> {
        self.define_batch_with_vals(&[decl_text], import_modules, inject_modules)
    }

    /// Define SEVERAL declarations as ONE generation — they land in one module
    /// and GHC typechecks them together, so a type signature and its binding,
    /// or a mutual-recursion SCC, split across separate block items still work
    /// (whole-block decl elaboration). `define` is the single-decl case.
    ///
    /// Binders are extracted from the concatenation (one parse), the sources
    /// ride as one `DeclTurn` (`render_module` already emits every source of a
    /// turn into one module), and validation/rollback are identical to
    /// `define`. Empty/whitespace sources are dropped; an all-empty batch is a
    /// no-op.
    ///
    /// Always shadows wildcard-imported names (`Library`, `Tidepool.Prelude`,
    /// …) with this session's own decl heads — GHCi parity for ANY session
    /// decl, pure or genuine: `f x = …` at the prompt always shadows an
    /// imported `f`, and a pure `let`/`<-` bind promoted into a decl
    /// (`tidepool-repl`'s `try_pure_bind_as_decl`) shadows exactly the same
    /// way, so pure and effectful binds stay interchangeable.
    pub fn define_batch(&mut self, decl_texts: &[&str]) -> Result<Generation, SessionError> {
        self.define_batch_with_vals(decl_texts, &[], &[])
    }

    /// [`Self::define_batch`] plus session-value scoping — see
    /// [`Self::define_with_vals`] for what `import_modules`/`inject_modules` do.
    pub fn define_batch_with_vals(
        &mut self,
        decl_texts: &[&str],
        import_modules: &[String],
        inject_modules: &[String],
    ) -> Result<Generation, SessionError> {
        let sources: Vec<String> = decl_texts
            .iter()
            .filter(|s| !s.trim().is_empty())
            .map(|s| (*s).to_string())
            .collect();
        if sources.is_empty() {
            return Ok(self.log.generation());
        }

        let combined = sources.join("\n\n");
        let mut binder_include: Vec<&Path> = vec![self.root.as_path()];
        binder_include.extend(self.extra_include.iter().map(PathBuf::as_path));

        let decl_template = TurnTemplate {
            kind: TemplateSelector::Decl,
            source: turn::DECL_TEMPLATE_SOURCE.to_string(),
        };
        let turn_result = run_turn(TurnRequest {
            turn_text: &combined,
            templates: std::slice::from_ref(&decl_template),
            include: &binder_include,
            session_root: self.root.as_path(),
            inject_modules: &[],
            gen: 0,
            verdict: Some(TurnClassification {
                kind: TurnKind::Decl,
                binders: Vec::new(),
            }),
            target: None,
        })
        .map_err(compile_error_to_session_error)?;
        let items = match turn_result {
            TurnResult::Decl { items, .. } => items,
            other => {
                return Err(SessionError::BinderExtraction(format!(
                    "decl verdict produced an unexpected TurnResult variant: {other:?}"
                )))
            }
        };

        self.log.push(DeclTurn {
            sources,
            items,
            retracts: Vec::new(),
        });
        let gen = self.log.generation();
        let rendered = render::render_module_with_vals(&self.log, gen, &self.env, import_modules);
        // Roll the just-pushed turn back on a write failure, exactly as the
        // validation-failure path below does — a bare `?` here would bump the
        // generation permanently while leaving no on-disk module, poisoning
        // every later turn that imports the (missing) gen module.
        if let Err(e) = self.write_module(&rendered) {
            self.log.turns.pop();
            return Err(e);
        }

        // Validate ALL turns via GHC. On failure, roll back the log and delete
        // the gen module file so later turns don't import a poisoned module.
        if let Err(e) = self.validate_candidate(&rendered, inject_modules) {
            self.log.turns.pop();
            let gen_path = self.root.join(rendered.module.relative_hs_path());
            let _ = std::fs::remove_file(&gen_path);
            return Err(e);
        }

        Ok(gen)
    }

    /// Retract `name` from the decl plane: after its binding migrates to the
    /// value plane, the decl module must stop exporting it, or a later
    /// `let`/`def` would compile against the stale decl (a value bound
    /// `findings <- pure []` then rebound `findings <- pure (findings ++ xs)`
    /// otherwise leaves `findings = []` defined forever). The dual of
    /// `tidepool-repl`'s value-plane eviction — call it when a decl-plane name is
    /// materialized.
    ///
    /// No-op when `name` is not a current decl head. Otherwise appends a
    /// pure-retraction turn and re-renders the current module as a re-export
    /// shell minus `name`. The shell introduces NO new source (only subtracts an
    /// export), so it cannot fail to type-check — GHC validation is skipped,
    /// making retraction cheap (no ~6s compile).
    pub fn retract(&mut self, name: &str) -> Result<(), SessionError> {
        if !self.log.current_heads().iter().any(|(h, _)| h == name) {
            return Ok(());
        }
        self.log.push(DeclTurn {
            sources: Vec::new(),
            items: Vec::new(),
            retracts: vec![name.to_string()],
        });
        let gen = self.log.generation();
        let rendered = render::render_module(&self.log, gen, &self.env);
        if let Err(e) = self.write_module(&rendered) {
            self.log.turns.pop(); // keep the log consistent with disk
            return Err(e);
        }
        Ok(())
    }

    /// Validate that the candidate gen module compiles and type-checks by running
    /// the extract in full-compile mode on a thin wrapper that imports it. The
    /// candidate is already written on disk at this point; this just drives GHC on
    /// it and surfaces any scope / type errors as a clean `SessionError`.
    /// `inject_modules` are passed through as `--inject-val` so a decl
    /// referencing a live session value resolves its `.hi` at validation time.
    fn validate_candidate(
        &self,
        rendered: &RenderedModule,
        inject_modules: &[String],
    ) -> Result<(), SessionError> {
        let temp = tempfile::TempDir::new()?;

        // Thin wrapper: importing the candidate forces GHC to compile it and
        // report any scope/type errors. `result = ()` is a trivial target.
        let module_name = rendered.module.module_name();
        let wrapper_src = format!(
            "module TidepoolValidate where\nimport {module_name} ()\nresult :: ()\nresult = ()\n"
        );
        let wrapper_path = temp.path().join("TidepoolValidate.hs");
        std::fs::write(&wrapper_path, &wrapper_src)?;

        // The candidate imports stdlib sources; resolve the include BEFORE
        // spawning so a misconfigured toolchain is a typed configuration error
        // rather than a GHC "Could not find module" blamed on the declaration.
        let stdlib_include = stdlib_include_for_validation(&self.extra_include)?;

        // A misconfigured $TIDEPOOL_EXTRACT is the same environment problem a
        // spawn failure is (`Io` → Infra), never the user's declaration.
        let mut cmd = ExtractCmd::new().map_err(|e| SessionError::Io(e.into()))?;
        cmd.input(&wrapper_path)
            .output_dir(temp.path())
            .target("result")
            .include(&self.root);

        // Caller-supplied include dirs (e.g. the generated `Tidepool.Effects`
        // dir + stdlib `lib/` under the full-eval decl surface). Required so a
        // decl importing `Tidepool.Effects` resolves at validation time.
        cmd.includes(&self.extra_include);

        cmd.includes(stdlib_include);

        if !inject_modules.is_empty() {
            // `--inject-val` ifaces are looked up under `--session-root`
            // (`Tidepool.Session.ssRoot`) — required whenever we inject any,
            // same as a stmt turn's `compile_session_turn` call.
            cmd.session_root(&self.root).inject_vals(inject_modules);
        }

        // Spawn failure is an environment problem (`Io` → Infra), never
        // `BinderExtraction` (which classifies as the user's Haskell).
        let run = cmd
            .run()
            .map_err(|e| SessionError::Io(crate::extract_spawn_error(e.source)))?;
        let output = &run.output;

        if run.verdict != ExitVerdict::Success {
            // An unparseable report is a stale/skewed extractor, not the
            // user's declaration — the same split every extract call site makes.
            let report = match crate::diag::parse_diag_report(&output.stdout, &output.stderr) {
                Ok(r) => r,
                Err(msg) => return Err(SessionError::MalformedDiagnostics(msg)),
            };
            let rel = rendered.module.relative_hs_path();
            // Speak item-relative coordinates: GHC's line numbers point into
            // the rendered G<g>.hs (header + imports before the user's text).
            // Anchored to the generated module's own path suffix only, so
            // foreign .hs:L:C tokens (panic backtraces) pass through.
            let line_offset = if rendered.body_line > 0 && !rendered.hoisted_lines {
                rendered.body_line
            } else {
                0
            };
            let rendered_text = crate::diag::render_diagnostics(
                &report.diagnostics,
                &crate::diag::RenderOpts {
                    anchor: &rel,
                    label: "<decl>",
                    user_lines: None,
                    line_offset,
                    col_indent: 0,
                    drop_foreign_gen_warnings_except: Some(&rel),
                    source: &rendered.source,
                },
            );
            return Err(SessionError::ValidationFailed(rendered_text));
        }

        Ok(())
    }

    /// Atomically write a rendered module to its place in the include tree.
    fn write_module(&self, rendered: &RenderedModule) -> Result<(), SessionError> {
        let rel = rendered.module.relative_hs_path();
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Atomic replace: write a sibling temp then rename.
        let dir = path.parent().unwrap_or(&self.root);
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        use std::io::Write;
        tmp.write_all(rendered.source.as_bytes())?;
        tmp.persist(&path).map_err(|e| SessionError::Io(e.error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_session_has_no_module() {
        let dir = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(1), dir.path(), ModuleEnv::standalone_default()).unwrap();
        assert_eq!(lib.generation(), Generation(0));
        assert!(lib.current_module().is_none());
        assert!(lib.import_line().is_none());
    }

    #[test]
    fn cache_salt_changes_with_generation_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let lib1 =
            SessionLib::open(SessionId(1), dir.path(), ModuleEnv::standalone_default()).unwrap();
        let lib2 =
            SessionLib::open(SessionId(2), dir.path(), ModuleEnv::standalone_default()).unwrap();
        assert_ne!(lib1.cache_salt(), lib2.cache_salt());
    }
}
