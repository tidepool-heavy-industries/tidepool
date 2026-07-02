//! The resident session: ONE live [`JitEffectMachine`] + the Lane-A decl
//! library + the value-plane [`BindingTable`], driven turn-by-turn. Both planes
//! persist across `session_run` items via the re-entry APIs (`compile_session`
//! for the first turn, then `add_function` + `run_fragment` for each later turn
//! on the SAME machine).
//!
//! The `Session<Open>` / `Session<Closed>` type-state (domain §5) is applied
//! through [`SessionHandle`]: `close` consumes the open handle and returns a
//! `Closed` one with no `run` method, so post-close turns don't typecheck.
//!
//! `run_block` drives a `session_run` block by classifying each item and
//! reusing the per-item handlers: `run_def` (declaration → Lane-A decl log),
//! `run_eval` (bind `x <- e` / `let x = e` → `BindingTable`, or a bare
//! expression → value), and `run_meta` (`:commands`).

use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_codegen::emit::ExternalEnv;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_mcp::{
    decl_module_basename, decl_user_offset, eval_user_offset, input_binding_source, library_vocab,
    rewrite_ghc_errors, template_haskell_show_default, CapturedOutput, EffectDecl,
    PREAMBLE_DEFAULT_DECL,
};
use tidepool_repr::{
    BindingName, DataConTable, Generation, SessionId, SessionModule, SessionVarId,
};
use tidepool_runtime::session::{
    classify_turn, compile_session_turn, ModuleEnv, SessionBind, SessionError, SessionLib,
    ValueTier,
};
use tidepool_runtime::{compile_haskell_salted, value_to_json};

use crate::command::{
    BlockItem, BlockItemResult, ExprText, MetaCommand, SessionCommand, TurnOutcome,
};

/// Default session nursery: 64 MiB (matches the eval runtime default).
pub const DEFAULT_NURSERY_SIZE: usize = 1 << 26;

/// Static configuration for a resident session, assembled once at open.
#[derive(Clone)]
pub struct SessionConfig {
    pub id: SessionId,
    /// Root of the session include tree (`Tidepool/Session/Lib/G<g>.hs` live here).
    pub root: PathBuf,
    /// Base GHC include dirs (generated `Tidepool.Effects` dir + prelude/stdlib).
    pub base_include: Vec<PathBuf>,
    /// Effect decls for this server (`[Console, Ask]` for the Wave-2 MVP).
    pub decls: Vec<EffectDecl>,
    /// The assembled eval preamble (from `tidepool_mcp::build_preamble`).
    pub preamble: String,
    /// The effect-stack type string (e.g. `'[Console, Ask]`).
    pub effect_stack: String,
    /// The `Ask` effect's tag (its index in `decls`).
    pub ask_tag: u64,
    /// Import/pragma surface for the generated `Lib.G<g>` decl modules.
    pub module_env: ModuleEnv,
    /// Session nursery size in bytes.
    pub nursery_size: usize,
}

/// The resident session — the value plane + the decl plane + a generation.
/// Owned by the worker thread; reached only through a [`SessionHandle`].
pub struct Session {
    cfg: SessionConfig,
    lib: SessionLib,
    /// The live machine. `None` until the first expression item bootstraps it
    /// (via `compile_session`); later turns re-enter it via `add_function`.
    machine: Option<JitEffectMachine>,
    /// Monotonic per-turn counter, for unique fragment function names.
    turn_counter: u64,
    /// The value-plane bridge: `name → (SessionVarId, RootSlot, Val.G<g>)`.
    /// Empty until the first `x <- e` / `let x = e`.
    bindings: BindingTable,
    /// Monotonic value-binding generation. Each bind mints a fresh
    /// `Tidepool.Session.Val.G<g>` so its `stableVarId` is collision-free and a
    /// rebind shadows without clobbering the prior root.
    val_gen: Generation,
    /// The DataConTable accumulated across turns (union via `insert_checked`), so
    /// a custom-ADT value bound earlier renders with real con names later.
    session_table: DataConTable,
    /// Per-block `input` payload from the `session_run` request. Injected into
    /// the generated module so `input :: Aeson.Value` is in scope. CLONED (not
    /// taken) by every evaluated item so it is visible to all items in the block
    /// (and after an in-block `ask`/resume); the worker resets it per job.
    eval_input: Option<serde_json::Value>,
    /// Shared slot the server reads to abort a runaway turn at a JIT safepoint.
    /// `None` until the worker wires it via [`Session::set_cancel_slot`]; the
    /// session publishes the machine's [`CancelHandle`] into it the moment the
    /// machine bootstraps, so even a session's FIRST turn is cancellable.
    cancel_slot: Option<crate::worker::CancelSlot>,
    /// Subtrees elided from the last truncated result, indexed by stub id
    /// (`stub_0` ⇒ index 0) — fetched via `:stub <n>`, REPLACED each time a
    /// new truncating result lands. See [`crate::truncate`].
    last_stubs: Vec<serde_json::Value>,
}

impl Session {
    /// Open a fresh session rooted at `cfg.root`.
    pub fn open(cfg: SessionConfig) -> std::io::Result<Session> {
        let lib = SessionLib::open(cfg.id, cfg.root.clone(), cfg.module_env.clone())
            .map_err(|e| std::io::Error::other(e.to_string()))?
            // Decl validation must resolve the same imports eval does (notably
            // the generated `Tidepool.Effects`), so feed it the base include.
            .with_validation_include(cfg.base_include.clone());
        Ok(Session {
            cfg,
            lib,
            machine: None,
            turn_counter: 0,
            bindings: BindingTable::new(),
            val_gen: Generation(0),
            session_table: DataConTable::new(),
            eval_input: None,
            cancel_slot: None,
            last_stubs: Vec::new(),
        })
    }

    /// The directory the session's `Val.G<g>.hi` ifaces are written to / read
    /// from. The same include root the Lane-A `Lib` modules live under, so a
    /// reference turn's `import Tidepool.Session.Val.G<g>` resolves from the
    /// injected HPT and the `.hi` path lines up with `writeSessionIface`.
    fn session_root(&self) -> &Path {
        &self.cfg.root
    }

    /// The GHC include path for a turn: the session's base includes (generated
    /// `Tidepool.Effects` + prelude/stdlib) plus the live `Lib.G<g>` dir. Borrows
    /// `&self`, so block-scope the result before any `&mut self` call (e.g.
    /// `query_inner_type`) — same constraint the inlined copies had.
    fn turn_include(&self) -> Vec<&Path> {
        let mut include: Vec<&Path> = self.cfg.base_include.iter().map(PathBuf::as_path).collect();
        include.push(self.lib.include_dir());
        include
    }

    /// Module names of every live value binding — what a turn injects
    /// (`--inject-val`) AND imports so a session reference typechecks.
    fn live_val_modules(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .live_modules()
            .map(|m| m.module_name())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The CURRENT (newest) `Val.G<g>` module per still-live name — what a turn
    /// IMPORTS (unqualified). This EXCLUDES shadowed older gens: a rebound name
    /// `x` is rooted under both `Val.G1` and `Val.G2`, and importing both
    /// unqualified makes every later `x` an ambiguous occurrence. Old gens are
    /// still INJECTED (see [`Self::live_val_modules`]) so already-compiled
    /// fragments / closure captures keep resolving — they just are not imported.
    fn current_val_modules(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .bindings
            .iter_current()
            .map(|(_, entry)| entry.module.module_name())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The `imports` block a turn prepends: the current `Lib.G<g>` decl module
    /// (if any) plus the current `Val.G<g>` module of each live name (newest
    /// gen only — shadowed gens are injected but not imported).
    fn session_imports(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        if let Some(m) = self.lib.current_module() {
            lines.push(m.module_name());
        }
        lines.extend(self.current_val_modules());
        lines.join("\n")
    }

    /// Merge a turn's DataCons into the session-accumulated table (loud on a
    /// genuine `stableVarId` collision — gen-versioned names make that a real
    /// bug, not churn).
    fn merge_table(&mut self, table: &DataConTable) -> Result<(), String> {
        for dc in table.iter() {
            self.session_table
                .insert_checked(dc.clone())
                .map_err(|e| format!("session DataConTable collision: {e}"))?;
        }
        Ok(())
    }

    /// Run one non-`Close` turn. Errors are folded into [`TurnOutcome::Error`]
    /// (the worker maps that to an MCP error result); an in-turn `ask` suspends
    /// through `handlers` (the [`crate::ask::ReplAskDispatcher`]), not here.
    pub fn run_turn<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        cmd: &SessionCommand,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        // Clear any cancellation left from a prior timed-out turn so this turn
        // starts clean (no-op until the machine bootstraps).
        self.reset_cancel();
        match cmd {
            SessionCommand::Def(decl) => self.run_def(&decl.0),
            SessionCommand::Eval(expr) => self.run_eval(&expr.0, handlers, captured),
            SessionCommand::Cmd(meta) => self.run_meta(meta),
            SessionCommand::Block { items, verbose } => {
                self.run_block(items, handlers, captured, *verbose)
            }
            // Close is handled by SessionHandle::close (type-state); the worker
            // never routes it here.
            SessionCommand::Close => TurnOutcome::Error(
                "internal: Close must be handled via SessionHandle::close".into(),
            ),
        }
    }

    /// `session_run`: run a list of classified [`BlockItem`]s in sequence,
    /// reusing `run_def`/`run_eval`/`run_meta` as the per-item handlers.
    ///
    /// Execution stops on the first error; the failing item is included in the
    /// `items` array with `ok = false`. An in-turn `ask` inside a `Stmt` or
    /// `Auto` item just works: the worker thread blocks inside `run_eval`
    /// (same stack), `session_resume` unblocks it, and the loop continues.
    ///
    /// `Auto` items use the try-cascade: `run_def` is attempted first; on a
    /// GHC parse error (not a type/scope error) the item falls back to
    /// `run_eval`. Non-parse errors from `run_def` surface as-is — the item
    /// is a declaration, just a broken one.
    fn run_block<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        items: &[BlockItem],
        handlers: &mut H,
        captured: &CapturedOutput,
        verbose: bool,
    ) -> TurnOutcome {
        let mut results: Vec<BlockItemResult> = Vec::with_capacity(items.len());
        let mut last_value: Option<serde_json::Value> = None;
        let mut last_type: Option<String> = None;
        let mut last_truncated: Option<String> = None;

        for (index, item) in items.iter().enumerate() {
            let (kind, outcome) = match item {
                BlockItem::Decl(decl) => ("decl", self.run_def(&decl.0)),
                BlockItem::Stmt(expr) => ("stmt", self.run_eval(&expr.0, handlers, captured)),
                BlockItem::Meta(meta) => ("meta", self.run_meta(meta)),
                BlockItem::Auto(expr) => {
                    // Try-cascade: attempt as declaration first. On a GHC
                    // parse error the item is not a decl → fall back to
                    // run_eval (which handles bind vs expr internally).
                    // A non-parse failure (type error, scope error) means it
                    // IS a declaration, just broken — surface the error.
                    let def_result = self.run_def(&expr.0);
                    match def_result {
                        TurnOutcome::Error(ref msg) if is_parse_error(msg) => {
                            ("stmt", self.run_eval(&expr.0, handlers, captured))
                        }
                        other => ("decl", other),
                    }
                }
            };

            let ok = !outcome.is_error();

            // Track the last value-producing expression result.
            if let TurnOutcome::Value {
                ref value,
                ref type_display,
                ref truncated,
            } = outcome
            {
                last_value = Some(value.clone());
                last_type = type_display.clone();
                last_truncated = truncated.clone();
            }

            results.push(BlockItemResult {
                index,
                kind: kind.to_string(),
                ok,
                result: slim_item_result(&outcome),
                result_full: outcome.render(),
            });

            if !ok {
                break; // stop on first error
            }
        }

        // Suppress `value` (and `truncated`) from the last ok value item's slim
        // result — those fields go to the top-level `value`/`truncated` only,
        // eliminating the duplication between items[].value and the top-level value.
        if last_value.is_some() {
            for r in results.iter_mut().rev() {
                if r.ok {
                    if let serde_json::Value::Object(ref mut obj) = r.result {
                        obj.remove("value");
                        obj.remove("truncated");
                    }
                    break;
                }
            }
        }

        TurnOutcome::Block {
            items: results,
            value: last_value,
            last_type,
            last_truncated,
            generation: self.lib.generation().0,
            val_gen: self.val_gen.0,
            verbose,
        }
    }

    /// Declaration handler: append the declaration to the Lane-A log + regenerate
    /// the gen-versioned `Lib.G<g>` module.
    fn run_def(&mut self, decl_text: &str) -> TurnOutcome {
        match self.lib.define(decl_text) {
            Ok(gen) => TurnOutcome::Defined {
                generation: gen.0,
                module: tidepool_repr::SessionModule::lib(gen).module_name(),
                head: decl_head(decl_text).to_string(),
            },
            Err(SessionError::ValidationFailed { ghc_stderr, module_source }) => {
                let basename = decl_module_basename(&module_source);
                let offset = decl_user_offset(&module_source);
                let rewritten = rewrite_ghc_errors(&ghc_stderr, &basename, offset);
                TurnOutcome::Error(format!("declaration failed: {rewritten}"))
            }
            Err(SessionError::BinderExtraction(ghc_text)) => {
                // Parse error on the declaration text; user content = entire file.
                // The extractor uses "SessionDecls.hs" as the temp filename.
                let rewritten = rewrite_ghc_errors(&ghc_text, "SessionDecls.hs", 2);
                TurnOutcome::Error(format!("declaration failed: {rewritten}"))
            }
            Err(e) => TurnOutcome::Error(format!("declaration failed: {e}")),
        }
    }

    /// Expression/bind handler. GHC's parser classifies the turn
    /// (bind vs expr); a BIND (`x <- e` / `let x = e`) roots a value on the live
    /// heap, a reference-with-live-bindings injects the session ifaces, and a
    /// plain expression (no bindings) stays on the proven Wave-2 path.
    fn run_eval<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        // Bind-vs-expr + bound names come from GHC (parse-only), never a Rust
        // scanner. A classify failure (e.g. extractor unavailable) falls back to
        // the plain path, where GHC re-reports any real error.
        let classification = match classify_turn(expr_text) {
            Ok(c) => c,
            Err(_) => return self.run_plain_eval(expr_text, handlers, captured),
        };

        if classification.is_bind {
            match classification.binders.as_slice() {
                // A bind statement with no extractable binder — treat as plain.
                [] => self.run_plain_eval(expr_text, handlers, captured),
                [name] => {
                    let name = name.clone();
                    self.run_bind(expr_text, name, handlers, captured)
                }
                // Multi-binder flat-tuple pattern (`(a, b) <- …`, `let (x,y) = …`):
                // run the action, project each tuple field, root each component.
                names => {
                    let names: Vec<String> = names.to_vec();
                    self.run_multi_bind(expr_text, names, handlers, captured)
                }
            }
        } else if !self.bindings.is_empty() {
            self.run_session_reference(expr_text, handlers, captured)
        } else {
            self.run_plain_eval(expr_text, handlers, captured)
        }
    }

    /// The proven Wave-2 expression path: compile an `M a` expression against the
    /// session include and run it on the resident machine. Unchanged — used when
    /// the turn neither binds nor references a session binding.
    fn run_plain_eval<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        let preamble = self.patched_preamble();
        let imports = self
            .lib
            .current_module()
            .map(|m| format!("{}\n", m.module_name()))
            .unwrap_or_default();
        // Clone (not take): `input` stays in scope for EVERY item in the block
        // — including items that run after an in-block `ask`/resume — and for the
        // type-probe recompiles below. The worker resets `eval_input` per job.
        let eval_input = self.eval_input.clone();
        let source = template_haskell_show_default(
            &preamble,
            &self.cfg.effect_stack,
            expr_text,
            &imports,
            "",
            eval_input.as_ref(),
            None,
        );

        let salt = self.lib.cache_salt();
        // Block-scope `include` so the borrow on `self.cfg.base_include` is
        // released before we call `query_inner_type` (which needs `&mut self`).
        let compile_result = {
            let include = self.turn_include();
            compile_haskell_salted(&source, "result", &include, Some(&salt))
        };
        let (expr, mut table, warnings) = match compile_result {
            Ok(triple) => triple,
            Err(e) => {
                let offset = eval_user_offset(&source);
                let rewritten = rewrite_ghc_errors(&e.to_string(), "Expr.hs", offset);
                return TurnOutcome::Error(format!("compile error: {rewritten}"));
            }
        };
        if warnings.has_io {
            return TurnOutcome::Error(
                "IO type detected in result binding. IO operations are not supported.".into(),
            );
        }
        table.populate_siblings_from_expr(&expr);

        // Query the inner value type (`a` in `M a`) via the bind mechanism:
        // `__t <- <expr>` gives `__t :: a`, not the Eff-wrapped action type.
        let inner_type = self.query_inner_type(expr_text);

        self.turn_counter += 1;
        let frag_name = format!("repl_turn_{}", self.turn_counter);
        let run_result = match self.machine {
            Some(ref mut machine) => {
                match machine.add_function(&frag_name, &expr, &table, &ExternalEnv::new()) {
                    Ok(fid) => machine.run_fragment(fid, &table, handlers, captured),
                    Err(e) => return TurnOutcome::Error(format!("JIT re-entry error: {e}")),
                }
            }
            None => {
                let m =
                    match JitEffectMachine::compile_session(&expr, &table, self.cfg.nursery_size) {
                        Ok(m) => m,
                        Err(e) => return TurnOutcome::Error(format!("JIT compile error: {e}")),
                    };
                // Store + publish the cancel handle BEFORE running, so a runaway
                // on this bare-expression path is cancellable from the start.
                self.bootstrap_machine(m);
                self.machine
                    .as_mut()
                    .expect("just bootstrapped")
                    .run(&table, handlers, captured)
            }
        };

        match run_result {
            Ok(value) => self.value_outcome(value_to_json(&value, &table, 0), inner_type),
            Err(e) => TurnOutcome::Error(format!("runtime error: {e}")),
        }
    }

    /// Assemble a [`TurnOutcome::Value`], truncating an oversized rendered
    /// value to the result budget and stashing the elided subtrees for
    /// `:stub <n>` (see [`crate::truncate`]). Shared by the plain-eval and
    /// reference paths — the two that render a value.
    fn value_outcome(
        &mut self,
        rendered: serde_json::Value,
        type_display: Option<String>,
    ) -> TurnOutcome {
        let (value, stubs, truncated) = crate::truncate::truncate_result(rendered);
        if !stubs.is_empty() {
            self.last_stubs = stubs;
        }
        TurnOutcome::Value {
            value,
            type_display,
            truncated,
        }
    }

    /// BIND path (`x <- action` / `let x = e`): wrap into an `Eff`-typed
    /// `result = do { <stmt>; pure x }`, compile through the session extract
    /// (earlier bindings injected so `action` may reference them), then
    /// `run_fragment_and_bind` to reduce the effect tree, strict-force (Tier-0)
    /// or store-as-is (Tier-1), tenure + root the value, and record it in the
    /// `BindingTable`. The thin `Val.G<g>` iface was already written by the
    /// extract (the type plane); this wires the value plane to the same id.
    fn run_bind<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn_text: &str,
        name: String,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        let preamble = self.patched_preamble();
        let g = self.val_gen.next();
        let inject = self.live_val_modules();
        let imports = self.session_imports();
        let eval_input = self.eval_input.clone();
        let wrapped = wrap_bind_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            turn_text,
            &name,
            eval_input.as_ref(),
        );

        let include = self.turn_include();

        let single = vec![name.clone()];
        let turn = match compile_session_turn(
            &wrapped,
            &include,
            self.session_root(),
            &inject,
            Some(SessionBind {
                names: &single,
                gen: g.0,
            }),
        ) {
            Ok(t) => t,
            Err(e) => {
                let offset = eval_user_offset(&wrapped);
                let rewritten = rewrite_ghc_errors(&e.to_string(), "Expr.hs", offset);
                return TurnOutcome::Error(format!("bind compile error: {rewritten}"));
            }
        };
        if turn.warnings.has_io {
            return TurnOutcome::Error(
                "IO type detected in bound value. IO operations are not supported.".into(),
            );
        }
        let binder = match turn.binders.into_iter().next() {
            Some(b) => b,
            None => return TurnOutcome::Error("bind turn produced no binder metadata".into()),
        };
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(e);
        }

        // Bootstrap the resident machine on the first turn from THIS turn's table
        // (an Eff module → carries the effect ConTags the machine's dispatch
        // needs). Later binds re-enter the live machine.
        if self.machine.is_none() {
            match JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size)
            {
                Ok(m) => self.bootstrap_machine(m),
                Err(e) => return TurnOutcome::Error(format!("JIT compile error: {e}")),
            }
        }

        // Seed the env from EXISTING bindings (so `action` resolves earlier x's);
        // the new binding is added after it is rooted.
        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_bind_{}", self.turn_counter);
        let forced = matches!(binder.tier, ValueTier::Tier0Data);

        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(&frag_name, &turn.expr, &self.session_table, &env) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::Error(format!("JIT bind add_function error: {e}")),
        };
        let slot = match machine.run_fragment_and_bind(
            fid,
            &self.session_table,
            handlers,
            captured,
            forced,
        ) {
            Ok(s) => s,
            Err(e) => return TurnOutcome::Error(format!("bind runtime error: {e}")),
        };

        self.val_gen = g;
        let value = if forced {
            BoundValue::Tier0Forced(slot)
        } else {
            BoundValue::Tier1Closure(slot)
        };
        self.bindings.bind(BindingEntry {
            name: BindingName(name.clone()),
            id: SessionVarId::from_extract(binder.var_id),
            module: SessionModule::val(g),
            value,
            type_display: Some(binder.type_display.clone()),
        });
        TurnOutcome::Bound {
            name,
            type_display: binder.type_display,
        }
    }

    /// MULTI-BIND path: `(a, b) <- action` / `let (x, y) = e`. Wraps the turn as
    /// `result = do { <stmt>; pure (a, b, …) }` so the fragment yields ONE tuple
    /// Con, then projects each field individually, tenures each as a separate root,
    /// and records one `BindingEntry` per component. The extract validates that the
    /// result type is an N-tuple (via `splitTupleType`); non-tuple patterns (e.g.
    /// constructor patterns with the wrong return type) get a loud compile error.
    fn run_multi_bind<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn_text: &str,
        names: Vec<String>,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        let preamble = self.patched_preamble();
        let g = self.val_gen.next();
        let inject = self.live_val_modules();
        let imports = self.session_imports();
        let eval_input = self.eval_input.clone();
        let wrapped = wrap_multi_bind_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            turn_text,
            &names,
            eval_input.as_ref(),
        );

        let include = self.turn_include();

        let turn = match compile_session_turn(
            &wrapped,
            &include,
            self.session_root(),
            &inject,
            Some(SessionBind {
                names: &names,
                gen: g.0,
            }),
        ) {
            Ok(t) => t,
            Err(e) => {
                let offset = eval_user_offset(&wrapped);
                let rewritten = rewrite_ghc_errors(&e.to_string(), "Expr.hs", offset);
                return TurnOutcome::Error(format!("multi-bind compile error: {rewritten}"));
            }
        };
        if turn.warnings.has_io {
            return TurnOutcome::Error(
                "IO type detected in bound value. IO operations are not supported.".into(),
            );
        }
        if turn.binders.len() != names.len() {
            return TurnOutcome::Error(format!(
                "multi-bind: extract returned {} binders, expected {}",
                turn.binders.len(),
                names.len()
            ));
        }
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(e);
        }

        if self.machine.is_none() {
            match JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size)
            {
                Ok(m) => self.bootstrap_machine(m),
                Err(e) => return TurnOutcome::Error(format!("JIT compile error: {e}")),
            }
        }

        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_multi_bind_{}", self.turn_counter);

        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(&frag_name, &turn.expr, &self.session_table, &env) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::Error(format!("JIT multi-bind add_function error: {e}")),
        };
        // run_fragment_and_bind_projected deep-forces the whole tuple first
        // (GC-safe: registers all pending parents as Rust roots), then projects
        // each field from the post-GC NF tuple and tenures each separately.
        let slots = match machine.run_fragment_and_bind_projected(
            fid,
            &self.session_table,
            handlers,
            captured,
            names.len(),
        ) {
            Ok(s) => s,
            Err(e) => return TurnOutcome::Error(format!("multi-bind runtime error: {e}")),
        };

        self.val_gen = g;
        // Zip binders with their slots and record each component.
        // Tier is read from binder metadata (deep_force already handled NF).
        let mut bound_names: Vec<(String, String)> = Vec::new();
        for (binder, slot) in turn.binders.iter().zip(slots.into_iter()) {
            let value = if matches!(binder.tier, ValueTier::Tier0Data) {
                BoundValue::Tier0Forced(slot)
            } else {
                BoundValue::Tier1Closure(slot)
            };
            self.bindings.bind(BindingEntry {
                name: BindingName(binder.name.clone()),
                id: SessionVarId::from_extract(binder.var_id),
                module: SessionModule::val(g),
                value,
                type_display: Some(binder.type_display.clone()),
            });
            bound_names.push((binder.name.clone(), binder.type_display.clone()));
        }
        TurnOutcome::MultiBound {
            components: bound_names,
        }
    }

    /// REFERENCE path (a bare expression mentioning session bindings). Try the
    /// `Eff` wrap first (so `pure (...)` / effectful references work exactly like
    /// Wave-2); on a type error, retry as a PURE value (`result = <expr>`, run
    /// purely) so bare references like `x + 1` / `f 10` / `v ^? key …` resolve.
    /// Both wraps inject the live `Val` ifaces so the reference typechecks.
    fn run_session_reference<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        let preamble = self.patched_preamble();
        let inject = self.live_val_modules();
        let imports = self.session_imports();
        // Clone (not take): `input` stays in scope for EVERY item in the block
        // — including items that run after an in-block `ask`/resume — and for the
        // type-probe recompiles below. The worker resets `eval_input` per job.
        let eval_input = self.eval_input.clone();

        // Eff-first (show-default: REPL renders via Show/toWire, not toJSON).
        let eff_src = template_haskell_show_default(
            &preamble,
            &self.cfg.effect_stack,
            expr_text,
            &imports,
            "",
            eval_input.as_ref(),
            None,
        );
        // Block-scope `include` so the borrow on `self.cfg.base_include` is
        // released before we call `query_inner_type` (which needs `&mut self`).
        let eff_result = {
            let include = self.turn_include();
            compile_session_turn(&eff_src, &include, self.session_root(), &inject, None)
        };
        match eff_result {
            Ok(turn) => {
                // Eff-first succeeded: `captured_type` is `Eff '[…] a`; query inner `a`.
                let inner_type = self.query_inner_type(expr_text);
                self.run_reference_fragment(turn, inner_type, false, handlers, captured)
            }
            Err(eff_err) => {
                // Pure fallback (keep the compile attempt; suppress the redundant error text).
                let pure_src =
                    wrap_pure_ref_source(&preamble, &imports, expr_text, eval_input.as_ref());
                let pure_result = {
                    let include = self.turn_include();
                    compile_session_turn(&pure_src, &include, self.session_root(), &inject, None)
                };
                match pure_result {
                    Ok(turn) => {
                        // Pure path: `captured_type` IS the inner type (`result = <expr>`
                        // has no Eff wrapper, so GHC infers the expression type directly).
                        let inner_type = turn.warnings.captured_type.clone();
                        self.run_reference_fragment(turn, inner_type, true, handlers, captured)
                    }
                    Err(_pure_err) => {
                        let offset = eval_user_offset(&eff_src);
                        let rewritten =
                            rewrite_ghc_errors(&eff_err.to_string(), "Expr.hs", offset);
                        TurnOutcome::Error(format!(
                            "compile error: {rewritten} (also failed as a pure value)"
                        ))
                    }
                }
            }
        }
    }

    /// Run a compiled reference fragment on the resident machine, resolving any
    /// session binders through the seeded `ExternalEnv` (load-through-slot).
    /// `inner_type` is the caller-resolved inner value type (`a` in `M a`).
    /// `pure` controls whether to execute via `run_fragment_pure` (no effects).
    fn run_reference_fragment<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn: tidepool_runtime::session::SessionTurnResult,
        inner_type: Option<String>,
        pure: bool,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        if turn.warnings.has_io {
            return TurnOutcome::Error(
                "IO type detected in result binding. IO operations are not supported.".into(),
            );
        }
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(e);
        }
        if self.machine.is_none() {
            match JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size)
            {
                Ok(m) => self.bootstrap_machine(m),
                Err(e) => return TurnOutcome::Error(format!("JIT compile error: {e}")),
            }
        }
        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_ref_{}", self.turn_counter);

        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(&frag_name, &turn.expr, &self.session_table, &env) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::Error(format!("JIT reference add_function error: {e}")),
        };
        let run_result = if pure {
            machine.run_fragment_pure(fid)
        } else {
            machine.run_fragment(fid, &self.session_table, handlers, captured)
        };
        match run_result {
            Ok(value) => {
                let rendered = value_to_json(&value, &self.session_table, 0);
                self.value_outcome(rendered, inner_type)
            }
            Err(e) => TurnOutcome::Error(format!("runtime error: {e}")),
        }
    }

    /// Compile-only type query: returns the inner value type `a` for a monadic
    /// expression of type `M a` / `Eff '[…] a`. Hoists the expr to a module-level
    /// `__probe` binding then binds `__t <- __probe` so `__t :: a` (the monadic
    /// bind peels the Eff head) — see `wrap_probe_source` for why the module-level
    /// binding matters (a trailing `where` can attach there but not on a
    /// do-statement). Consumes a throwaway generation to avoid iface collisions
    /// with subsequent real binds. Returns `None` if the compile fails (e.g. a
    /// non-monadic expression, which has no inner type to peel).
    fn query_inner_type(&mut self, expr_text: &str) -> Option<String> {
        let g = self.val_gen.next();
        self.val_gen = g;
        let preamble = self.patched_preamble();
        let inject = self.live_val_modules();
        let imports = self.session_imports();
        let eval_input = self.eval_input.clone();
        let wrapped = wrap_probe_source(
            &preamble,
            &self.cfg.effect_stack,
            &imports,
            expr_text,
            eval_input.as_ref(),
        );
        let include = self.turn_include();
        let names = vec!["__t".to_string()];
        compile_session_turn(
            &wrapped,
            &include,
            self.session_root(),
            &inject,
            Some(SessionBind {
                names: &names,
                gen: g.0,
            }),
        )
        .ok()
        .and_then(|turn| turn.binders.into_iter().next())
        .map(|b| b.type_display)
    }

    /// Meta-command handler — `:bindings`, `:reset`, `:t <expr>`, `:i <name>`, `:vocab`.
    fn run_meta(&mut self, meta: &MetaCommand) -> TurnOutcome {
        match meta {
            MetaCommand::Bindings => {
                let mut bindings: Vec<serde_json::Value> = self
                    .bindings
                    .iter_current()
                    .map(|(name, entry)| {
                        serde_json::json!({
                            "name": name.0,
                            "type": entry.type_display.clone().unwrap_or_default(),
                            "module": entry.module.module_name(),
                            "tier": if entry.value.is_forced() { "Tier0Data" } else { "Tier1Closure" },
                        })
                    })
                    .collect();
                bindings.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                TurnOutcome::Meta(serde_json::json!({
                    "bindings": bindings,
                    "generation": self.lib.generation().0,
                    "valGeneration": self.val_gen.0,
                }))
            }
            MetaCommand::Reset => {
                // Drop the resident machine (frees the session heap + every
                // persistent root) and clear both planes: the decl log (reopen
                // the lib at gen 0) and the value bindings + session table.
                self.machine = None;
                self.turn_counter = 0;
                self.bindings = BindingTable::new();
                self.val_gen = Generation(0);
                self.session_table = DataConTable::new();
                self.last_stubs = Vec::new();
                match SessionLib::open(
                    self.cfg.id,
                    self.cfg.root.clone(),
                    self.cfg.module_env.clone(),
                ) {
                    Ok(lib) => {
                        self.lib = lib.with_validation_include(self.cfg.base_include.clone());
                        TurnOutcome::Meta(serde_json::json!({"reset": true}))
                    }
                    Err(e) => TurnOutcome::Error(format!("reset failed: {e}")),
                }
            }
            MetaCommand::Type(ExprText(expr)) => {
                if expr.is_empty() {
                    return TurnOutcome::Meta(serde_json::json!({
                        "error": ":t requires an expression"
                    }));
                }
                let preamble = self.patched_preamble();
                // Consume a throwaway generation to prevent an iface collision
                // with the next real bind (compile_session_turn writes a Val.G<g>.hi
                // even for the discard path). We do NOT add to self.bindings.
                let throwaway_gen = self.val_gen.next();
                self.val_gen = throwaway_gen;
                let inject = self.live_val_modules();
                let imports = self.session_imports();
                let eval_input = self.eval_input.clone();
                let turn_text = format!("let __t = {expr}");
                let wrapped = wrap_bind_source(
                    &preamble,
                    &self.cfg.effect_stack,
                    &imports,
                    &turn_text,
                    "__t",
                    eval_input.as_ref(),
                );
                let include = self.turn_include();
                let names: Vec<String> = vec!["__t".to_string()];
                let turn = match compile_session_turn(
                    &wrapped,
                    &include,
                    self.session_root(),
                    &inject,
                    Some(SessionBind {
                        names: &names,
                        gen: throwaway_gen.0,
                    }),
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        let offset = eval_user_offset(&wrapped);
                        let rewritten = rewrite_ghc_errors(&e.to_string(), "Expr.hs", offset);
                        return TurnOutcome::Meta(serde_json::json!({
                            "error": format!("compile error: {rewritten}")
                        }));
                    }
                };
                match turn.binders.into_iter().next() {
                    Some(binder) => TurnOutcome::Meta(serde_json::json!({
                        "type": binder.type_display
                    })),
                    None => TurnOutcome::Meta(serde_json::json!({
                        "error": "no type information captured"
                    })),
                }
            }
            MetaCommand::Info(name) => {
                // 1. Bound value lookup (highest priority — a session binding shadows types).
                if let Some((_, entry)) = self.bindings.iter_current().find(|(n, _)| n.0 == *name) {
                    return TurnOutcome::Meta(serde_json::json!({
                        "name": name,
                        "type": entry.type_display.clone().unwrap_or_default(),
                        "tier": if entry.value.is_forced() { "Tier0Data" } else { "Tier1Closure" },
                        "module": entry.module.module_name(),
                    }));
                }
                // 2. Built-in effect decl type_defs (data/newtype/type) and GADT constructors.
                for decl in &self.cfg.decls {
                    for type_def in decl.type_defs {
                        if type_def_head(type_def) == Some(name.as_str()) {
                            return TurnOutcome::Meta(serde_json::json!({
                                "name": name,
                                "shape": *type_def,
                            }));
                        }
                    }
                    for con in decl.constructors {
                        if con.split("::").next().map(str::trim) == Some(name.as_str()) {
                            return TurnOutcome::Meta(serde_json::json!({
                                "name": name,
                                "shape": *con,
                                "effect": decl.type_name,
                            }));
                        }
                    }
                }
                // 3. Session-defined types (data/newtype/type/class from declaration items).
                if let Some(src) = self.lib.decl_type_source(name) {
                    return TurnOutcome::Meta(serde_json::json!({
                        "name": name,
                        "shape": src,
                        "source": "session",
                    }));
                }
                // 4. Stdlib/preamble types (`Proc`, `Hit`, `Doc`, … — source-scanned
                // from the same include dirs the session compiles against).
                if let Some(info) = crate::introspect::stdlib_info(&self.cfg.base_include, name) {
                    return TurnOutcome::Meta(info);
                }
                // 5. Total miss.
                TurnOutcome::Meta(serde_json::json!({
                    "error": "not a bound value or known type",
                    "name": name,
                    "hint": "searched session bindings, effect types, session declarations, \
                             and the stdlib/library sources; for an expression's type use \
                             `:t <expr>`",
                }))
            }
            MetaCommand::Stub(n, page) => {
                TurnOutcome::Meta(crate::truncate::stub_fetch(&self.last_stubs, *n, *page))
            }
            MetaCommand::Vocab(only) => {
                let mut dirs: Vec<std::path::PathBuf> = Vec::new();
                if let Ok(cwd) = std::env::current_dir() {
                    if let Some(root) = tidepool_runtime::paths::find_project_root(&cwd) {
                        let lib = root.join(".tidepool").join("lib");
                        if lib.is_dir() {
                            dirs.push(lib);
                        }
                    }
                }
                dirs.extend(tidepool_runtime::paths::global_lib_dirs());
                let vocab = library_vocab(&dirs, only.as_deref());
                TurnOutcome::Meta(serde_json::json!({ "vocab": vocab }))
            }
        }
    }

    /// The eval preamble with every import the session can collide with
    /// (`Tidepool.Prelude`, the project `Library`, and the generated
    /// `Tidepool.Effects` effect verbs) extended with a `hiding (…)` clause
    /// covering EVERY name the session owns across BOTH planes:
    ///   - declaration value binders (`f x = …`),
    ///   - declaration types/classes (`data Hit`),
    ///   - live value-plane binds (`let glob = …` / `x <- …`).
    ///
    /// Without this, a session name that happens to match a Prelude re-export, a
    /// `Library` verb, or an effect verb becomes a GHC "Ambiguous occurrence"
    /// that not only fails the current turn but POISONS every later turn (the
    /// colliding import is regenerated each turn) — a hard-to-debug footgun hit
    /// in practice by `let glob = …` (vs the `Fs` `glob` verb) and `data Hit`
    /// (vs the `Library` `Hit`). Hiding makes the session definition win, the
    /// way GHCi shadowing would. (BUG-7 + the verb/value-plane collision class.)
    fn patched_preamble(&self) -> String {
        let mut names: Vec<String> = Vec::new();
        names.extend(self.lib.decl_value_names().into_iter().map(str::to_string));
        names.extend(self.lib.decl_type_names().into_iter().map(str::to_string));
        names.extend(self.bindings.iter_current().map(|(n, _)| n.0.clone()));
        names.sort();
        names.dedup();
        if names.is_empty() {
            return self.cfg.preamble.clone();
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let p = hide_prelude_names(&self.cfg.preamble, &refs);
        let p = hide_module_names(&p, "Library", &refs);
        let p = hide_module_names(&p, "Tidepool.Effects", &refs);
        // The pagination / orchestration helpers (`memo`, `readGlob`, …) live in
        // the generated Tidepool.Orchestrate module, imported unqualified by the
        // eval expr module. A session bind that collides with one (e.g. `let memo
        // = …`) must shadow the import, not become an ambiguous occurrence.
        hide_module_names(&p, "Tidepool.Orchestrate", &refs)
    }

    /// Drop the resident machine, freeing the session heap. Called from
    /// [`SessionHandle::close`].
    fn free(&mut self) {
        // JitEffectMachine::drop calls free_session_heap for session machines.
        self.machine = None;
    }

    /// Store the per-turn input payload so `run_plain_eval` / `run_session_reference`
    /// can inject it into `template_haskell`. Called by the worker before each turn.
    fn set_eval_input(&mut self, input: Option<serde_json::Value>) {
        self.eval_input = input;
    }

    /// Wire the shared cancel slot the server reads on timeout. Called once by
    /// the worker before the command loop. If the machine has already
    /// bootstrapped, publish its handle immediately.
    pub fn set_cancel_slot(&mut self, slot: crate::worker::CancelSlot) {
        self.cancel_slot = Some(slot);
        self.publish_cancel();
    }

    /// Bootstrap the resident machine AND publish its cancel handle to the
    /// shared slot in one step — so a turn is cancellable from the instant the
    /// machine exists (including a session's first turn, before its runaway
    /// loop starts). All machine-bootstrap sites go through here.
    fn bootstrap_machine(&mut self, m: JitEffectMachine) {
        self.machine = Some(m);
        self.publish_cancel();
    }

    /// Publish the machine's cancel handle into the shared slot (no-op if no
    /// slot is wired or the machine hasn't bootstrapped).
    fn publish_cancel(&mut self) {
        if let Some(slot) = &self.cancel_slot {
            *slot.lock() = self.machine.as_ref().map(|m| m.cancel_handle());
        }
    }

    /// Clear a prior cancellation so the next turn starts clean. The cancel flag
    /// is per-machine and shared, so this resets it via the live handle.
    fn reset_cancel(&mut self) {
        if let Some(m) = &self.machine {
            m.cancel_handle().reset();
        }
    }
}

// ---------------------------------------------------------------------------
// Type-state: Open vs Closed (domain §5)
// ---------------------------------------------------------------------------

/// Phantom marker: the session is open and accepts turns.
pub struct Open;
/// Phantom marker: the session is closed; no turns can be run.
pub struct Closed;

/// A session handle parameterized by lifecycle state. Only `SessionHandle<Open>`
/// has `run`; `close` consumes it and yields a `SessionHandle<Closed>`, so a
/// post-close turn is a compile error (the kimi-r2 #11 type-state mandate).
pub struct SessionHandle<S> {
    inner: Session,
    _state: PhantomData<S>,
}

impl SessionHandle<Open> {
    /// Wrap a freshly-opened session as an open handle.
    pub fn new(session: Session) -> SessionHandle<Open> {
        SessionHandle {
            inner: session,
            _state: PhantomData,
        }
    }

    /// Run one turn against the open session.
    pub fn run<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        cmd: &SessionCommand,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        self.inner.run_turn(cmd, handlers, captured)
    }

    /// Store the `input` payload before a `session_run` block runs.
    /// The worker calls this so `input :: Aeson.Value` is in scope during eval.
    pub fn set_eval_input(&mut self, input: Option<serde_json::Value>) {
        self.inner.set_eval_input(input);
    }

    /// The current declaration generation (0 until the first declaration item).
    pub fn generation(&self) -> u64 {
        self.inner.lib.generation().0
    }

    /// Consume the open handle: free the resident machine and transition to
    /// `Closed`. The returned handle has no `run`.
    pub fn close(mut self) -> SessionHandle<Closed> {
        self.inner.free();
        SessionHandle {
            inner: self.inner,
            _state: PhantomData,
        }
    }
}

// SessionHandle<Closed> deliberately has NO `run` — post-close turns don't
// typecheck. (It is otherwise inert; the worker drops it after close.)

// ---------------------------------------------------------------------------
// Turn-wrapping helpers
// ---------------------------------------------------------------------------

/// Insert `import <m>` lines into the eval preamble immediately before the
/// `default` declaration (the canonical injection point, matching where
/// `template_haskell` places user imports). Uses [`PREAMBLE_DEFAULT_DECL`] as
/// the injection marker — no magic substring (AUDIT-3).
fn insert_imports(preamble: &str, imports: &str) -> String {
    if imports.trim().is_empty() {
        return preamble.to_string();
    }
    let insert_point = preamble
        .find(PREAMBLE_DEFAULT_DECL)
        .unwrap_or(preamble.len());
    let mut out = String::new();
    out.push_str(&preamble[..insert_point]);
    for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
        out.push_str(&format!("import {imp}\n"));
    }
    out.push_str(&preamble[insert_point..]);
    out
}

/// Rewrite `import Tidepool.Prelude hiding (…)` in the preamble to also hide
/// the given names. Applied per-turn so that user-defined functions named after
/// Prelude/lens re-exports (e.g. `over`, `view`, `key`) resolve unambiguously
/// to the session decl rather than the Prelude export (BUG-7).
///
/// Names already present in the hiding list are not duplicated. Names that do
/// not exist in Tidepool.Prelude produce no error (GHC silently ignores
/// redundant hiding entries in most configurations).
fn hide_prelude_names(preamble: &str, extra: &[&str]) -> String {
    const PRELUDE_IMPORT_PREFIX: &str = "import Tidepool.Prelude hiding (";
    let Some(start) = preamble.find(PRELUDE_IMPORT_PREFIX) else {
        return preamble.to_string();
    };
    let rest = &preamble[start..];
    let line_len = rest.find('\n').map_or(rest.len(), |i| i + 1);
    let line = &rest[..line_len];

    // Extract the existing hidden list from "import Tidepool.Prelude hiding (X, Y)\n"
    let paren_open = line.find('(').unwrap_or(line.len()) + 1;
    let paren_close = line.rfind(')').unwrap_or(line.len());
    let mut all: Vec<String> = line[paren_open..paren_close]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    for &n in extra {
        // Parenthesize operator names (`.+` → `(.+)`) — a bare operator in a
        // hiding list is a parse error.
        let e = hiding_entry(n);
        if !all.contains(&e) {
            all.push(e);
        }
    }
    let new_line = format!("import Tidepool.Prelude hiding ({})\n", all.join(", "));
    format!(
        "{}{}{}",
        &preamble[..start],
        new_line,
        &preamble[start + line_len..]
    )
}

/// Parenthesize an operator name for a `hiding` list (`.+` → `(.+)`); plain
/// identifiers pass through. (Operators are invalid bare in an import list.)
fn hiding_entry(name: &str) -> String {
    match name.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' || c == '(' => name.to_string(),
        _ => format!("({name})"),
    }
}

/// Like [`hide_prelude_names`] but for a clause-less `import <module>` line
/// (e.g. the project `import Library` or the generated `import Tidepool.Effects`).
/// Rewrites `import <module>` → `import <module> hiding (<session names>)` so a
/// session-defined name shadows a same-named re-export / effect verb instead of
/// becoming an ambiguous occurrence. No-op when `<module>` isn't imported. GHC
/// tolerates hiding a name the module doesn't export (a dodgy-import warning,
/// same as the Prelude path).
fn hide_module_names(preamble: &str, module: &str, extra: &[&str]) -> String {
    if extra.is_empty() {
        return preamble.to_string();
    }
    let needle = format!("import {module}");
    let mut from = 0;
    loop {
        let Some(rel) = preamble[from..].find(&needle) else {
            return preamble.to_string(); // module not imported
        };
        let start = from + rel;
        let at_line_start = start == 0 || preamble.as_bytes()[start - 1] == b'\n';
        let after = &preamble[start + needle.len()..];
        // Exact module: next char ends the word (newline) or opens a clause (space).
        if at_line_start && (after.starts_with('\n') || after.starts_with(' ')) {
            let rest = &preamble[start..];
            let line_len = rest.find('\n').map_or(rest.len(), |i| i + 1);
            let line = &rest[..line_len];
            let mut all: Vec<String> = match (line.find('('), line.rfind(')')) {
                (Some(po), Some(pc)) if po < pc => line[po + 1..pc]
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            };
            for &n in extra {
                let e = hiding_entry(n);
                if !all.contains(&e) {
                    all.push(e);
                }
            }
            let new_line = format!("import {module} hiding ({})\n", all.join(", "));
            return format!(
                "{}{}{}",
                &preamble[..start],
                new_line,
                &preamble[start + line_len..]
            );
        }
        from = start + needle.len();
    }
}

/// Start a user-code module: the imports-injected preamble, the `-- [user]`
/// marker, and the optional `input :: Aeson.Value` binding. The shared prefix of
/// every `wrap_*` source builder.
fn begin_user_module(preamble: &str, imports: &str, input: Option<&serde_json::Value>) -> String {
    let mut out = insert_imports(preamble, imports);
    out.push_str("-- [user]\n");
    out.push_str(&input_binding_source(input));
    out
}

/// Append each line of `text` to `out`, indented two spaces (the do-block /
/// binding-RHS body layout the wrappers use).
fn push_indented(out: &mut String, text: &str) {
    for line in text.lines() {
        out.push_str(&format!("  {line}\n"));
    }
}

/// Wrap a BIND turn into an `Eff`-typed module whose `result` runs the bind
/// statement and yields the bound value, so `run_fragment_and_bind` reduces the
/// effect tree and roots that value. The monad is pinned to the session effect
/// stack (`Eff <stack> _`, the value type inferred via `PartialTypeSignatures`,
/// which the preamble enables) — a bare `pure (42 :: Int)` action would
/// otherwise leave the monad ambiguous.
fn wrap_bind_source(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    turn_text: &str,
    binder: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    out.push_str(&format!("result :: Eff {effect_stack} _\n"));
    out.push_str("result = do\n");
    push_indented(&mut out, turn_text);
    out.push_str(&format!("  pure {binder}\n"));
    out
}

/// Wrap a MULTI-BIND turn into an `Eff`-typed module whose `result` runs the
/// bind statement and yields a tuple of all bound names. For `(a, b) <- action`
/// with `names = ["a", "b"]` this emits:
/// ```haskell
/// result :: Eff <stack> _
/// result = do
///   (a, b) <- action
///   pure (a, b)
/// ```
/// The tuple field order matches the binder order in the JSON sidecar.
fn wrap_multi_bind_source(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    turn_text: &str,
    names: &[String],
    input: Option<&serde_json::Value>,
) -> String {
    let tuple_expr = format!("({})", names.join(", "));
    let mut out = begin_user_module(preamble, imports, input);
    out.push_str(&format!("result :: Eff {effect_stack} _\n"));
    out.push_str("result = do\n");
    push_indented(&mut out, turn_text);
    out.push_str(&format!("  pure {tuple_expr}\n"));
    out
}

/// Wrap a PURE reference turn as `result = <expr>` (no `Eff`), run via
/// `run_fragment_pure`. For bare value references like `x + 1` / `f 10` /
/// `v ^? key …` that are not monadic.
///
/// Emits a SECOND `__user = <expr>` binding whose sole purpose is type capture:
/// the extractor reads the inferred type off the `__user` binder
/// (`capturedUserType`), so the reference turn can report `{type, value}` for a
/// bare pure expression instead of `type: null`. `__user` is unused at runtime
/// (only `result` is executed) and harmless — session compiles are not `-Werror`.
fn wrap_pure_ref_source(
    preamble: &str,
    imports: &str,
    expr_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    out.push_str("__user =\n");
    push_indented(&mut out, expr_text);
    out.push('\n');
    out.push_str("result =\n");
    push_indented(&mut out, expr_text);
    out
}

/// Wrap an expression for the INNER-TYPE probe. Hoists the whole expression to a
/// module-level `__probe = <expr>` binding (where a trailing `where` attaches
/// legally — a do-statement `__t <- expr where …` does NOT parse), then binds
/// `__t <- __probe` so GHC's monadic bind peels `Eff es a` to the inner `a`. That
/// is the same type-directed peel the `x <- e` bind path uses — no TyCon
/// name-matching. `__probe` is typecheck scaffolding only: the compile targets
/// `result`, so it is never serialized into the turn.
fn wrap_probe_source(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    expr_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    out.push_str("__probe =\n");
    push_indented(&mut out, expr_text);
    out.push('\n');
    out.push_str(&format!("result :: Eff {effect_stack} _\n"));
    out.push_str("result = do\n");
    out.push_str("  __t <- __probe\n");
    out.push_str("  pure __t\n");
    out
}

/// Extract the declared head identifier from a Haskell declaration string,
/// for the slim `{"decl":"name"}` block item result. Strips keyword prefixes
/// for type/class/instance declarations; for function definitions returns the
/// first identifier. Returns `""` for empty or unrecognised text.
fn decl_head(text: &str) -> &str {
    let s = text.trim();
    for kw in &["data ", "newtype ", "type ", "class ", "instance "] {
        if let Some(rest) = s.strip_prefix(kw) {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '(' || c == '=')
                .unwrap_or(rest.len());
            return rest[..end].trim_end();
        }
    }
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(' || c == ':' || c == '=')
        .unwrap_or(s.len());
    &s[..end]
}

/// Compute the slim inline JSON result for one block item (the default shape).
/// Fields are merged directly into the item object in `Block::render()`.
/// The `value` key is present here but stripped for the final expression item
/// (see `run_block` — `obj.remove("value")` after the loop).
fn slim_item_result(outcome: &TurnOutcome) -> serde_json::Value {
    match outcome {
        TurnOutcome::Bound { name, type_display } => serde_json::json!({
            "bound": name,
            "type": type_display,
        }),
        TurnOutcome::MultiBound { components } => serde_json::json!({
            "bound": components.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "types": components.iter().map(|(_, t)| t).collect::<Vec<_>>(),
        }),
        TurnOutcome::Defined { head, .. } => serde_json::json!({
            "decl": head,
        }),
        TurnOutcome::Value {
            value,
            type_display,
            truncated,
        } => {
            let mut obj = serde_json::Map::new();
            if let Some(t) = type_display {
                obj.insert("type".into(), serde_json::json!(t));
            }
            obj.insert("value".into(), value.clone());
            if let Some(hint) = truncated {
                obj.insert("truncated".into(), serde_json::json!(hint));
            }
            serde_json::Value::Object(obj)
        }
        TurnOutcome::Meta(v) => v.clone(),
        TurnOutcome::Error(e) => serde_json::json!({ "error": e }),
        TurnOutcome::Block { .. } => serde_json::json!({ "error": "nested block" }),
    }
}

/// Return `true` when a `run_def` error message indicates a GHC parse (not
/// type or scope) error, so the try-cascade in `run_block` can fall back from
/// `run_def` to `run_eval` for items that are expressions, not declarations.
///
/// GHC parse errors always contain the text "parse error" or "lexical error";
/// type/scope errors use different phrasing ("Couldn't match", "Not in scope",
/// "No instance for", …). The check is case-insensitive to tolerate minor GHC
/// version variation.
///
/// We ALSO treat "binder extraction failed" as a not-a-declaration signal:
/// binder extraction is the parse/scope STAGE (pre-typecheck), so a failure
/// there — including the extractor throwing an uncaught `SourceError` on a
/// non-declaration input like `123 :: Int` — means "this isn't a declaration,
/// try it as an expression." A genuine-but-type-broken declaration parses fine
/// at this stage and instead fails later as a "declaration type-check failed"
/// (validation) error, which does NOT match here and so surfaces as a decl error.
fn is_parse_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("parse error")
        || lower.contains("lexical error")
        || lower.contains("binder extraction failed")
}

/// Extract the declared head name from a Haskell type declaration string.
/// Returns `Some(name)` when the string starts with `data`/`newtype`/`type`
/// and the next token is the type name; `None` for functions, instances, etc.
fn type_def_head(src: &str) -> Option<&str> {
    let s = src.trim();
    let rest = s
        .strip_prefix("data ")
        .or_else(|| s.strip_prefix("newtype "))
        .or_else(|| s.strip_prefix("type "))?;
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '(' || c == '=')
        .unwrap_or(rest.len());
    let head = &rest[..end];
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

#[cfg(test)]
mod slim_tests {
    use super::{decl_head, slim_item_result};
    use crate::command::TurnOutcome;

    #[test]
    fn decl_head_extracts_names() {
        assert_eq!(decl_head("slug t = T.replace \" \" \"-\" t"), "slug");
        assert_eq!(decl_head("data Foo = Bar | Baz"), "Foo");
        assert_eq!(decl_head("newtype Wrapper a = Wrapper { unwrap :: a }"), "Wrapper");
        assert_eq!(decl_head("type Name = Text"), "Name");
        assert_eq!(decl_head("class MyClass a where"), "MyClass");
        assert_eq!(decl_head("  f x = x + 1"), "f");
        assert_eq!(decl_head(""), "");
    }

    #[test]
    fn slim_item_result_shapes() {
        let bound = TurnOutcome::Bound {
            name: "vs".into(),
            type_display: "[Text]".into(),
        };
        let r = slim_item_result(&bound);
        assert_eq!(r["bound"], "vs");
        assert_eq!(r["type"], "[Text]");

        let defined = TurnOutcome::Defined {
            generation: 1,
            module: "Tidepool.Session.Lib.G1".into(),
            head: "slug".into(),
        };
        let r = slim_item_result(&defined);
        assert_eq!(r["decl"], "slug");
        assert!(r.get("generation").is_none(), "no generation in slim decl");
        assert!(r.get("module").is_none(), "no module in slim decl");
    }
}

#[cfg(test)]
mod info_tests {
    use super::type_def_head;
    use tidepool_mcp::lsp_decl;

    #[test]
    fn type_def_head_recognizes_data_newtype_type() {
        assert_eq!(
            type_def_head("data Node = Node { nodeName :: Text }"),
            Some("Node")
        );
        assert_eq!(
            type_def_head("data Position = Position { posLine :: Int }"),
            Some("Position")
        );
        assert_eq!(type_def_head("data Lang = Rust | Python"), Some("Lang"));
        assert_eq!(type_def_head("newtype Foo = Foo Int"), Some("Foo"));
        assert_eq!(type_def_head("type Name = Text"), Some("Name"));
        // Non-type-decl strings return None.
        assert_eq!(type_def_head("matchVars :: Match -> Map Text Text"), None);
        assert_eq!(type_def_head("instance ToJSON Node where"), None);
        assert_eq!(type_def_head("nodeLine :: Node -> Int"), None);
    }

    #[test]
    fn decl_scan_finds_node_in_lsp_decl() {
        let decl = lsp_decl();
        let found = decl
            .type_defs
            .iter()
            .any(|td| type_def_head(td) == Some("LspNode"));
        assert!(found, "LspNode must be discoverable in lsp_decl type_defs");
        let pos_found = decl
            .type_defs
            .iter()
            .any(|td| type_def_head(td) == Some("Position"));
        assert!(
            pos_found,
            "Position must be discoverable in lsp_decl type_defs"
        );
    }
}

#[cfg(test)]
mod hiding_tests {
    use super::{hide_module_names, hide_prelude_names};

    #[test]
    fn prelude_hiding_parenthesizes_operators() {
        let pre = "import Tidepool.Prelude hiding (error)\n";
        let out = hide_prelude_names(pre, &[".+", "slug"]);
        // operator parenthesized, plain name bare; no bare `.+` (parse error)
        assert!(out.contains("(.+)"), "op must be parenthesized: {out}");
        assert!(out.contains("slug"), "plain name present: {out}");
        assert!(
            !out.contains(" .+,") && !out.contains(", .+)"),
            "bare operator leaked: {out}"
        );
    }

    #[test]
    fn library_hiding_added_with_operators() {
        let pre = "import Library\nimport qualified Prelude as P\n";
        let out = hide_module_names(pre, "Library", &[".+", "sh"]);
        assert!(
            out.contains("import Library hiding ("),
            "Library gets a hiding clause: {out}"
        );
        assert!(
            out.contains("(.+)") && out.contains("sh"),
            "names hidden: {out}"
        );
    }

    #[test]
    fn library_hiding_noop_without_import() {
        let pre = "import Tidepool.Prelude hiding (error)\n";
        assert_eq!(
            hide_module_names(pre, "Library", &["sh"]),
            pre,
            "no Library import → unchanged"
        );
    }

    #[test]
    fn effects_hiding_shadows_verbs() {
        // A session-owned name (e.g. a `let glob = …` value bind) must shadow the
        // generated `Tidepool.Effects` verb of the same name instead of an
        // ambiguous occurrence — the verb/value-plane collision footgun.
        let pre = "import Tidepool.Effects\nimport qualified Prelude as P\n";
        let out = hide_module_names(pre, "Tidepool.Effects", &["glob"]);
        assert!(
            out.contains("import Tidepool.Effects hiding (glob)"),
            "Effects verb shadowed by a session name: {out}"
        );
    }

    #[test]
    fn orchestrate_hiding_shadows_helpers() {
        // A session-owned name (e.g. a `let memo = …` value bind) must shadow the
        // generated `Tidepool.Orchestrate` helper of the same name instead of an
        // ambiguous occurrence — the namespace-poison bug class this fix targets.
        let pre = "import Tidepool.Orchestrate\nimport qualified Prelude as P\n";
        let out = hide_module_names(pre, "Tidepool.Orchestrate", &["memo"]);
        assert!(
            out.contains("import Tidepool.Orchestrate hiding (memo)"),
            "Orchestrate helper shadowed by a session name: {out}"
        );
    }
}
