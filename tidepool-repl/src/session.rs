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
use tidepool_codegen::old_space::RootSlot;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_mcp::{
    first_sentence, helper_sig, input_binding_source, library_vocab, template_haskell_show_default,
    CapturedOutput, EffectDecl, PREAMBLE_DEFAULT_DECL,
};
use tidepool_repr::{
    BindingName, DataConTable, Generation, SessionId, SessionModule, SessionVarId,
};
use tidepool_runtime::session::errmap::{
    dedupe_diagnostics, drop_foreign_gen_warnings, remap_generated_coords,
};
use tidepool_runtime::session::{
    classify_turn, compile_session_turn, ModuleEnv, SessionBind, SessionError, SessionLib,
    TurnKind, ValueTier,
};
use tidepool_runtime::{
    classify_compile, classify_session, compile_haskell_salted, value_to_json, CompileError,
    CompileResult, FailureClass, Phase,
};

use crate::command::{
    BlockItem, BlockItemResult, BoundComponent, ExprText, ItemKind, MetaCommand, ResponseShape,
    SessionCommand, TurnOutcome,
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
    /// PURE binds routed into the decl plane (`x <- pure e` / `let x = e` → the
    /// decl `x = e`, which generalizes). They have no heap value — they resolve
    /// via the decl-module import — but they ARE part of the session
    /// environment, so this registry surfaces them in `:bindings`/stale/etc.
    /// alongside effectful (materialized) binds. Latest-wins per name; a
    /// cross-plane rebind removes the name from the other plane.
    pure_binds: std::collections::BTreeMap<String, PureBind>,
}

/// A pure bind that lives in the decl plane (GHCi-environment model): its value
/// is its source (`defining_expr`), re-instantiated per use by GHC. No root.
struct PureBind {
    type_display: String,
    defining_expr: String,
    gen: Generation,
}

/// One run item's outcome inside `run_block`: its position, classified kind, and
/// the [`TurnOutcome`] it produced. Replaces the positional
/// `(usize, &'static str, TurnOutcome)` tuple the block-runner used to carry.
struct ItemRun {
    index: usize,
    kind: ItemKind,
    outcome: TurnOutcome,
}

/// How a reference fragment is executed: `Effectful` runs the effect tree via
/// `run_fragment`; `Pure` runs it with no effects via `run_fragment_pure`.
/// Replaces the `pure: bool` flag `run_reference_fragment` used to take.
#[derive(Clone, Copy)]
enum EvalMode {
    Effectful,
    Pure,
}

/// Map a binder's [`ValueTier`] to the [`BoundValue`] wrapping its root slot —
/// the single source of truth for the tier → bound-value expansion (was a bool
/// round-trip at each bind site).
fn bound_value(tier: ValueTier, slot: RootSlot) -> BoundValue {
    match tier {
        ValueTier::Tier0Data => BoundValue::Tier0Forced(slot),
        ValueTier::Tier1Closure => BoundValue::Tier1Closure(slot),
    }
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
            pure_binds: std::collections::BTreeMap::new(),
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
        // A name that migrates decl→value is RETRACTED from the decl plane by
        // `bind_materialized` (via `SessionLib::retract`), so `current_module`
        // no longer exports it and there is no decl/value collision to hide here
        // — the value plane's `Val.G<g>` module is the sole provider. (One
        // mechanism: retraction at the source, not per-consumer hiding.)
        let mut lines: Vec<String> = Vec::new();
        if let Some(m) = self.lib.current_module() {
            lines.push(m.module_name());
        }
        lines.extend(self.current_val_modules());
        lines.join("\n")
    }

    /// [`Self::session_imports`] plus the quasi-quoter import when the turn's
    /// text splices one — the same per-request gating the oneshot eval does
    /// (`[fmt|]`/`[j|]`/… cost the quoter-module import only when used).
    fn turn_imports(&self, turn_text: &str) -> String {
        let mut imports = self.session_imports();
        if tidepool_mcp::uses_qq(turn_text) {
            if !imports.is_empty() {
                imports.push('\n');
            }
            imports.push_str("Tidepool.QQ (fmt, j, patch, uri)");
        }
        imports
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

    /// Bind `entry` on the value (materialized) plane, EVICTING any pure
    /// decl-plane binding of the same name in the same step. The live
    /// environment is split across two planes — `bindings` (value) and
    /// `pure_binds` (decl) — with the invariant that a name lives in AT MOST
    /// one. That invariant is enforced HERE (and in [`Self::bind_pure`]) so the
    /// bind sites can't smear a name across both planes by forgetting the paired
    /// cross-plane removal.
    fn bind_materialized(&mut self, entry: BindingEntry) {
        // Symmetric with `bind_pure`: a name moving to the value plane is
        // retracted from the decl plane too, so `SessionLib` stops exporting a
        // now-stale decl (`findings <- pure []` then `findings <- pure
        // (findings ++ xs)` would otherwise leave `findings = []` defined, and a
        // later `let`/`def` would compile against it). `retract` is a no-op when
        // the name was never a decl head (a plain `p <- run …`). Best-effort: a
        // rare module-write failure leaves the binding materialized correctly.
        let _ = self.lib.retract(&entry.name.0);
        self.pure_binds.remove(&entry.name.0);
        self.bindings.bind(entry);
    }

    /// Register `pb` on the decl (pure) plane under `name`, EVICTING any
    /// materialized value-plane binding of the same name from the current view.
    /// The dual of [`Self::bind_materialized`] — the single site that upholds
    /// the one-name-one-plane invariant for pure binds.
    fn bind_pure(&mut self, name: &str, pb: PureBind) {
        self.bindings.remove_current(name);
        self.pure_binds.insert(name.to_string(), pb);
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

    /// The declaration text of an item that is a top-level DECLARATION (and so
    /// batches into a decl run), or `None` for a bind/expression/meta (a
    /// singleton). A keyword decl is one lexically; an `Auto` item is one iff
    /// GHC's parser — the single authority (`Tidepool.Binders.classifyTurn`,
    /// decl+stmt contexts) — classifies it as a declaration. This parse verdict,
    /// not the coarse `Auto` tag, is what excludes a trailing call from a decl
    /// batch. On a classify failure (extractor unavailable), the `Auto` item is
    /// NOT decl-shaped so it takes the resilient per-item path.
    ///
    /// Returning the text (rather than a bool) lets the segment scan CARRY the
    /// decl sources as it walks, so the batch path never re-derives "this is a
    /// Decl/Auto" with a panicking match.
    fn decl_shaped_text<'a>(&self, item: &'a BlockItem) -> Option<&'a str> {
        match item {
            BlockItem::Decl(d) => Some(&d.0),
            BlockItem::Auto(e) if classify_turn(&e.0).is_ok_and(|c| c.kind == TurnKind::Decl) => {
                Some(&e.0)
            }
            BlockItem::Auto(_) | BlockItem::Stmt(_) | BlockItem::Meta(_) => None,
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
        // The `results` INDEX of the item whose `TurnOutcome::Value` most
        // recently set `last_value` — recorded at the moment it is assigned, so
        // the post-loop step below strips fields from exactly that item, never
        // an unrelated item re-derived by some other heuristic.
        let mut last_value_pos: Option<usize> = None;

        // Process items, batching maximal runs of consecutive decl-shaped
        // items (Decl/Auto) so a sig+binding pair or a mutual-recursion SCC
        // split across items typecheck TOGETHER (whole-block decl elaboration).
        // Optimistic: try `define_batch` on the whole run; on success emit a
        // per-source decl result, else fall back to processing each item
        // individually (the exact prior behavior — a stmt-shaped Auto item, or
        // a genuinely broken decl, lands here). Stmt/Meta items are singletons.
        let mut index = 0;
        'outer: while index < items.len() {
            // Collect this segment's (index, kind, outcome) tuples. A DECL-shaped
            // item (a keyword decl, or an `Auto` the GHC parser classifies as a
            // top-level declaration) starts a batch run; a stmt/meta/expression
            // is a singleton. The parse verdict — not the lexical `Auto` tag — is
            // what keeps a trailing call (`sq 7` after `sq :: T` / `sq x = …`)
            // OUT of the decl batch: it classifies as an expression, ends the run,
            // and lands on the stmt path (the tool's "define then call in one
            // block" idiom).
            let segment: Vec<ItemRun> = if let Some(first) = self.decl_shaped_text(&items[index]) {
                // Carry the decl sources as we scan the maximal decl-shaped run,
                // so the batch path never re-matches items to recover their text.
                let start = index;
                let mut texts: Vec<String> = vec![first.to_string()];
                let mut end = index + 1;
                while end < items.len() {
                    match self.decl_shaped_text(&items[end]) {
                        Some(t) => {
                            // Within-block REDEFINITION ends the segment: if this
                            // item defines a head an earlier item in the segment
                            // also DEFINES (not a sig+binding pair — those must
                            // batch), batching would hand GHC two equation groups
                            // it merges as multi-clause (first wins). Splitting
                            // starts a new generation, so replace-latest applies,
                            // matching the cross-turn GHCi-parity rule. (#320)
                            let h = decl_head(t);
                            if !h.is_empty()
                                && defines_head(t, h)
                                && texts
                                    .iter()
                                    .any(|prev| decl_head(prev) == h && defines_head(prev, h))
                            {
                                break;
                            }
                            texts.push(t.to_string());
                            end += 1;
                        }
                        None => break,
                    }
                }

                // Try to elaborate the whole run as one generation. Every item is
                // parser-confirmed a declaration, so the batch is not poisoned by
                // a stray expression; a failure here is a genuine type/scope error
                // and falls to the per-item path for a precise, per-item message.
                let batched = if texts.len() >= 2 {
                    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                    self.lib.define_batch(&refs).ok()
                } else {
                    None
                };

                index = end;
                match batched {
                    Some(gen) => texts
                        .iter()
                        .enumerate()
                        .map(|(k, t)| ItemRun {
                            index: start + k,
                            kind: ItemKind::Decl,
                            outcome: self.defined_outcome(t, decl_head(t).to_string(), gen),
                        })
                        .collect(),
                    None => {
                        // Fallback: per-item, stopping the whole block on
                        // the first error (matched by the outer break).
                        let mut out = Vec::with_capacity(texts.len());
                        for (k, it) in items[start..end].iter().enumerate() {
                            let (kind, outcome) = self.run_one_item(it, handlers, captured);
                            let err = outcome.is_error();
                            out.push(ItemRun {
                                index: start + k,
                                kind,
                                outcome,
                            });
                            if err {
                                break;
                            }
                        }
                        out
                    }
                }
            } else {
                let (kind, outcome) = self.run_one_item(&items[index], handlers, captured);
                index += 1;
                vec![ItemRun {
                    index: index - 1,
                    kind,
                    outcome,
                }]
            };

            for ItemRun {
                index: idx,
                kind,
                outcome,
            } in segment
            {
                // `ok` also reflects a meta command that reported an `error` in
                // its payload (e.g. `:i` on a missing name) — a `Meta` outcome,
                // not an `Error` variant, but still a failure for ok-scripting
                // and the stop-on-first-error contract. (#319)
                let ok = !outcome.is_error()
                    && !matches!(&outcome, TurnOutcome::Meta(v) if v.get("error").is_some());

                // Track the last value-producing expression result, and WHICH
                // `results` slot it will land in.
                if let TurnOutcome::Value {
                    ref value,
                    ref type_display,
                    ref truncated,
                } = outcome
                {
                    last_value = Some(value.clone());
                    last_type = type_display.clone();
                    last_truncated = truncated.clone();
                    last_value_pos = Some(results.len());
                }

                results.push(BlockItemResult {
                    index: idx,
                    kind,
                    ok,
                    result: slim_item_result(&outcome),
                    result_full: outcome.render(),
                });

                if !ok {
                    break 'outer; // stop on first error
                }
            }
        }

        // The top-level `value` reflects the block's FINAL executed item ONLY —
        // a block ending in a bind/decl/meta (or one that errored after an
        // earlier expression ran) leaves it null, matching the documented
        // contract ("a block ending in a bind leaves `value` null") and GHCi
        // intuition. `last_value_pos` was recorded at assignment time, so this
        // is a direct index comparison, not a re-derived "last ok item" scan.
        if last_value_pos != results.len().checked_sub(1) {
            last_value = None;
            last_type = None;
            last_truncated = None;
            last_value_pos = None;
        }

        // Suppress `value` (and `truncated`) from the item that produced
        // `last_value` — that data now lives at the top level only, eliminating
        // duplication between items[].value and the top-level value. Strips
        // exactly `results[last_value_pos]`, never an unrelated item (e.g. a
        // trailing `:stub` meta result that happens to carry its OWN `value`
        // key) — the bug this replaced re-scanned for "the last ok item of any
        // kind" and could strip the wrong one.
        if let Some(pos) = last_value_pos {
            if let Some(r) = results.get_mut(pos) {
                if let serde_json::Value::Object(ref mut obj) = r.result {
                    obj.remove("value");
                    obj.remove("truncated");
                }
            }
        }

        let shape = if verbose {
            ResponseShape::Verbose {
                generation: self.lib.generation().0,
                val_gen: self.val_gen.0,
            }
        } else {
            ResponseShape::Slim
        };
        TurnOutcome::Block {
            items: results,
            value: last_value,
            last_type,
            last_truncated,
            shape,
        }
    }

    /// Declaration handler: append the declaration to the Lane-A log + regenerate
    /// the gen-versioned `Lib.G<g>` module.
    fn run_def(&mut self, decl_text: &str) -> TurnOutcome {
        let head = decl_head(decl_text).to_string();
        match self.lib.define(decl_text) {
            Ok(gen) => self.defined_outcome(decl_text, head, gen),
            Err(e) => TurnOutcome::Error(session_fail(&e, "declaration failed")),
        }
    }

    /// If `expr_text` is a PURE bind of `name`, route it as the top-level decl
    /// `name = <rhs>` (so GHC generalizes it — GHCi parity) and return a `Bound`
    /// outcome. Returns `None` when it is not a pure bind, or when the decl
    /// route fails BECAUSE the RHS references a materialized/effectful value
    /// (out of decl scope) — the caller then falls back to the materialize path.
    ///
    /// When the decl route fails for ANY OTHER reason (a plain type error), we
    /// return that error as `Some(Error)` instead of `None`, so the caller does
    /// NOT materialize. Materializing such a case papers over a real error with
    /// a broken binding: a polymorphic value (`toList :: Tree a -> [a]`)
    /// materialized as a monomorphic `Tier1Closure` gets a thin iface with a
    /// FREE type variable, which then fails cryptically ("Iface type variable
    /// out of scope: a") only when later referenced. Surfacing the decl error is
    /// the same actionable message the bare-decl form already gives.
    ///
    /// `define_batch` always shadows wildcard-imported names (ledger #36) — a
    /// pure bind must be able to shadow a Prelude/Library/effect-verb name
    /// exactly as a genuine top-level decl does (`run_def`/`define_batch`), so
    /// pure and effectful binds stay interchangeable (do-block plane-opacity
    /// invariant): `let lookup = 42` must shadow `Prelude.lookup`, not raise
    /// an "Ambiguous occurrence" that a bare `f x = …` decl would never hit.
    fn try_pure_bind_as_decl(&mut self, expr_text: &str, name: &str) -> Option<TurnOutcome> {
        let decl = pure_bind_to_decl(expr_text, name)?;
        match self.lib.define_batch(&[decl.as_str()]) {
            Ok(gen) => {
                let type_display = self.probe_pure_type(name).unwrap_or_default();
                // Register in the environment (decl plane) so :bindings/stale/etc.
                // see it; `bind_pure` evicts any materialized binding of `name`
                // from the value plane (cross-plane shadow, one-plane invariant).
                self.bind_pure(
                    name,
                    PureBind {
                        type_display: type_display.clone(),
                        defining_expr: expr_text.to_string(),
                        gen,
                    },
                );
                Some(TurnOutcome::Bound {
                    name: name.to_string(),
                    type_display,
                })
            }
            Err(e) => {
                // Materialize is the right fallback when the RHS references a
                // value-plane-only binding the decl plane genuinely lacks:
                //  (a) a materialized session value (matched by name), or
                //  (b) the `input` payload lane (injected only on the value/stmt
                //      plane). We detect (b) PRECISELY by the decl error itself —
                //      "not in scope: input" — NOT by scanning the text for the
                //      word `input`: a user's OWN locally-bound `input` (a lambda
                //      param, an inner `let`, or a decl-plane `let input = …`)
                //      COMPILES on the decl plane, so it never trips this, and a
                //      typo like `inputt` yields a different name. do-block
                //      invariant #1 (payload lane in scope in every item).
                // Otherwise the decl failure is a real error (collision / type)
                // — surface it rather than materialize a broken binding.
                let err_str = e.to_string();
                let refs_materialized_value = self
                    .bindings
                    .iter_current()
                    .any(|(n, _)| mentions_word(expr_text, &n.0));
                // Whole-word `input` (GHC: "Variable not in scope: input :: Value"),
                // never `inputText`/`input'` — check the char after the match is not
                // an identifier continuation.
                let refs_input_lane = {
                    let needle = "not in scope: input";
                    err_str.match_indices(needle).any(|(i, _)| {
                        let after = &err_str[i + needle.len()..];
                        !after
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '\'' || c == '_')
                    })
                };
                if refs_materialized_value || refs_input_lane {
                    None
                } else {
                    Some(TurnOutcome::Error(session_fail(&e, "bind compile error")))
                }
            }
        }
    }

    /// Read the (generalized) type of a pure session name by compiling it as a
    /// pure reference and reading the captured type. Best-effort — `None` if it
    /// doesn't typecheck as a bare pure value.
    ///
    /// Probes in an NMR context (mirroring the decl module the bind actually
    /// lives in), NOT the eval preamble's monomorphism restriction. A pure bind
    /// generalizes under NMR, so its true type shows — GHCi parity: `n <- pure
    /// 5` reads `Num a => a`, `xs <- pure []` reads `[a]`. Under MR a
    /// constrained bind (`f h = h.path :: HasField "path" r a => r -> a`, the
    /// core record-dot idiom) can't monomorphize the unresolved constraint and
    /// the probe FAILS — an empty type display; NMR reports it faithfully.
    fn probe_pure_type(&mut self, name: &str) -> Option<String> {
        let preamble = to_nmr_pragmas(&self.patched_preamble());
        let imports = self.session_imports();
        let inject = self.live_val_modules();
        let eval_input = self.eval_input.clone();
        let src = wrap_pure_ref_source(&preamble, &imports, name, eval_input.as_ref());
        let include = self.turn_include();
        compile_session_turn(&src, &include, self.session_root(), &inject, None)
            .ok()
            .and_then(|turn| turn.warnings.captured_type)
    }

    /// Build the `Defined` outcome for one decl head at generation `gen`,
    /// computing the `stale` set (live binds whose defining expression
    /// references this (re)defined name — notebook display truthfulness) and
    /// the inferred `type` the server had at compile time (#317).
    /// Shared by `run_def` and the whole-block decl-batch path. `text` is the
    /// decl item's source, used to gate the type probe to VALUE bindings.
    fn defined_outcome(&mut self, text: &str, head: String, gen: Generation) -> TurnOutcome {
        let mut stale: Vec<String> = self
            .bindings
            .iter_current()
            .filter(|(_, e)| {
                e.defining_expr
                    .as_deref()
                    .is_some_and(|src| mentions_word(src, &head))
            })
            .map(|(n, _)| n.0.clone())
            .collect();
        // Pure binds (decl-backed) that reference the redefined head are also
        // stale (they hold their old generalized value until re-run).
        stale.extend(
            self.pure_binds
                .iter()
                .filter(|(_, pb)| mentions_word(&pb.defining_expr, &head))
                .map(|(n, _)| n.clone()),
        );
        // Paint the inferred type — render every mutation fully, once, at
        // mutation time. Best-effort: only for items with an actual DEFINING
        // equation for `head` (a value/function binding — `defines_head` is
        // false for type/class/data/instance/import/fixity decls and for a
        // signature-only item in a split sig+bind pair, so the primary bind
        // item is the one painted). The probe is one extra extract compile;
        // its failure never fails the decl — the field is simply omitted.
        let type_display = if defines_head(text, &head) {
            self.probe_pure_type(&head)
        } else {
            None
        };
        TurnOutcome::Defined {
            generation: gen.0,
            module: tidepool_repr::SessionModule::lib(gen).module_name(),
            head,
            type_display,
            stale,
        }
    }

    /// Run a single block item (no batching): the per-item dispatch used both
    /// for stmt/meta items and as the fallback when a decl batch fails.
    fn run_one_item<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        item: &BlockItem,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> (ItemKind, TurnOutcome) {
        match item {
            BlockItem::Decl(decl) => (ItemKind::Decl, self.run_def(&decl.0)),
            BlockItem::Stmt(expr) => (ItemKind::Stmt, self.run_eval(&expr.0, handlers, captured)),
            BlockItem::Meta(meta) => (ItemKind::Meta, self.run_meta(meta)),
            BlockItem::Auto(expr) => {
                // Try-cascade: attempt as declaration first. On a GHC parse
                // error the item is not a decl → fall back to run_eval (bind
                // vs expr internally). A non-parse failure (type/scope error)
                // means it IS a declaration, just broken — surface the error.
                let def_result = self.run_def(&expr.0);
                match def_result {
                    TurnOutcome::Error(ref msg) if is_parse_error(msg) => {
                        (ItemKind::Stmt, self.run_eval(&expr.0, handlers, captured))
                    }
                    other => (ItemKind::Decl, other),
                }
            }
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

        if classification.kind == TurnKind::Bind {
            match classification.binders.as_slice() {
                // A bind whose pattern binds NO name (`_ <- e`, `(_,_) <- e`):
                // run the RHS effect and discard. Compiling the raw `_ <- e` as
                // an expression fails "parse error on input '<-'" — strip the
                // discarding pattern and route the RHS `e` like a bare effectful
                // expression. (#321)
                [] => {
                    let rhs = split_discard_bind(expr_text).unwrap_or(expr_text);
                    if !self.bindings.is_empty() || self.lib.generation().0 > 0 {
                        self.run_session_reference(rhs, handlers, captured)
                    } else {
                        self.run_plain_eval(rhs, handlers, captured)
                    }
                }
                [name] => {
                    let name = name.clone();
                    // "A pure binding is a declaration": a PURE bind (`let x = e`,
                    // `x <- pure e`, `x <- return e`) is routed into the decl
                    // plane as the top-level binding `x = e`, so it GENERALIZES
                    // (GHCi parity — `xs <- pure []` stays polymorphic, `n <-
                    // pure 5` instantiates at Double later) instead of freezing
                    // to a monomorphic heap value. If it fails to compile as a
                    // decl (its RHS references a materialized/effectful value, so
                    // it's out of decl scope), fall back to the materialize path.
                    //
                    // EXCEPT a self-referential monadic pure bind (`n <- pure
                    // (n+1)`): the decl route would emit the RECURSIVE top-level
                    // `n = n + 1` (self-forcing blackhole). GHCi's `>>=` reads the
                    // PRIOR `n` and shadows, which is exactly what the materialize
                    // path does — so divert straight to it.
                    if !self_referential_monadic_pure_bind(expr_text, &name) {
                        if let Some(outcome) = self.try_pure_bind_as_decl(expr_text, &name) {
                            return outcome;
                        }
                    }
                    self.run_bind(expr_text, name, handlers, captured)
                }
                // Multi-binder flat-tuple pattern (`(a, b) <- …`, `let (x,y) = …`):
                // run the action, project each tuple field, root each component.
                names => {
                    let names: Vec<String> = names.to_vec();
                    self.run_multi_bind(expr_text, names, handlers, captured)
                }
            }
        } else {
            // A confirmed bare EXPRESSION (not a bind, not a discard-bind RHS)
            // — GHCi-style: bind its value to `it` (rebinding each turn) and
            // render the response from that SAME single evaluation. See
            // `run_bare_expr`.
            let imports = self.turn_imports(expr_text);
            let inject = self.live_val_modules();
            self.run_bare_expr(expr_text, &imports, &inject, handlers, captured)
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
        let mut imports = self
            .lib
            .current_module()
            .map(|m| format!("{}\n", m.module_name()))
            .unwrap_or_default();
        // Same per-turn quasi-quoter gating as `turn_imports` (this path
        // assembles its imports independently of session_imports).
        if tidepool_mcp::uses_qq(expr_text) {
            imports.push_str("Tidepool.QQ (fmt, j, patch, uri)\n");
        }
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
        let CompileResult {
            expr,
            mut table,
            warnings,
        } = match compile_result {
            Ok(r) => r,
            Err(e) => return TurnOutcome::Error(compile_fail(&e, &source)),
        };
        if warnings.has_io {
            return io_type_fail();
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
                    Err(e) => return TurnOutcome::Error(run_fail("JIT re-entry error", e)),
                }
            }
            None => {
                let m =
                    match JitEffectMachine::compile_session(&expr, &table, self.cfg.nursery_size) {
                        Ok(m) => m,
                        Err(e) => return TurnOutcome::Error(run_fail("JIT compile error", e)),
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
            Err(e) => TurnOutcome::Error(run_fail("runtime error", e)),
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

    /// Like [`Self::value_outcome`], but for a turn that ALSO bound the
    /// result to `it` ([`Self::run_bare_expr`]) — the truncation hint
    /// additionally names the `it` affordance (type + rendered size + a
    /// neutral note), and a result whose rendered size exceeds
    /// [`crate::truncate::HUGE_CEILING`] collapses to a terse header
    /// (`{type, size, note}`) instead of a partial structural preview. See
    /// [`crate::truncate::truncate_for_it`].
    fn value_outcome_bound_it(
        &mut self,
        rendered: serde_json::Value,
        type_display: Option<String>,
    ) -> TurnOutcome {
        let (value, stubs, truncated) =
            crate::truncate::truncate_for_it(rendered, type_display.as_deref());
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
        let imports = self.turn_imports(turn_text);
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
            Err(e) => return TurnOutcome::Error(compile_fail(&e, &wrapped)),
        };
        if turn.warnings.has_io {
            return TurnOutcome::Error(tag_failure(
                FailureClass::UserHaskell,
                Phase::Compile,
                "IO type detected in bound value. IO operations are not supported.".into(),
            ));
        }
        let binder = match turn.binders.into_iter().next() {
            Some(b) => b,
            None => {
                return TurnOutcome::Error(tag_failure(
                    FailureClass::Infra,
                    Phase::Compile,
                    "bind turn produced no binder metadata".into(),
                ))
            }
        };
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(tag_failure(FailureClass::Runtime, Phase::Run, e));
        }

        // Bootstrap the resident machine on the first turn from THIS turn's table
        // (an Eff module → carries the effect ConTags the machine's dispatch
        // needs). Later binds re-enter the live machine.
        if self.machine.is_none() {
            match JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size)
            {
                Ok(m) => self.bootstrap_machine(m),
                Err(e) => return TurnOutcome::Error(run_fail("JIT compile error", e)),
            }
        }

        // Seed the env from EXISTING bindings (so `action` resolves earlier x's);
        // the new binding is added after it is rooted.
        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_bind_{}", self.turn_counter);

        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(&frag_name, &turn.expr, &self.session_table, &env) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::Error(run_fail("JIT bind add_function error", e)),
        };
        // `run_fragment_and_bind` still takes a `forced: bool` (a codegen API);
        // the tier is the single source of truth — derive the flag at the call
        // and expand the same tier back to a `BoundValue` via `bound_value`.
        let slot = match machine.run_fragment_and_bind(
            fid,
            &self.session_table,
            handlers,
            captured,
            matches!(binder.tier, ValueTier::Tier0Data),
        ) {
            Ok(s) => s,
            Err(e) => return TurnOutcome::Error(run_fail("bind runtime error", e)),
        };

        self.val_gen = g;
        let value = bound_value(binder.tier, slot);
        // `bind_materialized` records the value binding AND evicts any pure decl
        // of the same name (cross-plane shadow, one-plane invariant).
        self.bind_materialized(BindingEntry {
            name: BindingName(name.clone()),
            id: SessionVarId::from_extract(binder.var_id),
            module: SessionModule::val(g),
            value,
            type_display: Some(binder.type_display.clone()),
            defining_expr: Some(turn_text.to_string()),
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
        let imports = self.turn_imports(turn_text);
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
            Err(e) => return TurnOutcome::Error(compile_fail(&e, &wrapped)),
        };
        if turn.warnings.has_io {
            return TurnOutcome::Error(tag_failure(
                FailureClass::UserHaskell,
                Phase::Compile,
                "IO type detected in bound value. IO operations are not supported.".into(),
            ));
        }
        if turn.binders.len() != names.len() {
            return TurnOutcome::Error(tag_failure(
                FailureClass::Infra,
                Phase::Compile,
                format!(
                    "multi-bind: extract returned {} binders, expected {}",
                    turn.binders.len(),
                    names.len()
                ),
            ));
        }
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(tag_failure(FailureClass::Runtime, Phase::Run, e));
        }

        if self.machine.is_none() {
            match JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size)
            {
                Ok(m) => self.bootstrap_machine(m),
                Err(e) => return TurnOutcome::Error(run_fail("JIT compile error", e)),
            }
        }

        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_multi_bind_{}", self.turn_counter);

        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(&frag_name, &turn.expr, &self.session_table, &env) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::Error(run_fail("JIT multi-bind add_function error", e)),
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
            Err(e) => return TurnOutcome::Error(run_fail("multi-bind runtime error", e)),
        };

        self.val_gen = g;
        // Zip binders with their slots and record each component.
        // Tier is read from binder metadata (deep_force already handled NF).
        let mut components: Vec<BoundComponent> = Vec::new();
        for (binder, slot) in turn.binders.iter().zip(slots.into_iter()) {
            let value = bound_value(binder.tier, slot);
            self.bind_materialized(BindingEntry {
                name: BindingName(binder.name.clone()),
                id: SessionVarId::from_extract(binder.var_id),
                module: SessionModule::val(g),
                value,
                type_display: Some(binder.type_display.clone()),
                // The whole multi-bind turn defines each component (`(a,b) <- e`).
                defining_expr: Some(turn_text.to_string()),
            });
            components.push(BoundComponent {
                name: binder.name.clone(),
                type_display: binder.type_display.clone(),
            });
        }
        TurnOutcome::MultiBound { components }
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
        let imports = self.turn_imports(expr_text);
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
                self.run_reference_fragment(
                    turn,
                    inner_type,
                    EvalMode::Effectful,
                    handlers,
                    captured,
                )
            }
            Err(_eff_err) => {
                // Pure fallback. The Eff-first wrap is an internal routing detail
                // (§run_session_reference): for a pure expression it ALWAYS fails
                // with `Couldn't match … with Eff …` noise against the internal
                // `do _r <- __user; paginateResult …` scaffold. When the pure
                // wrap ALSO fails, its error is the faithful one — `result =
                // <expr>` is the minimal framing, so the genuine cause (scope /
                // type error) maps straight to the user's `<item>` with no Eff
                // wrapper leaking through. So we surface `pure_err`, never
                // `_eff_err`, and drop the old "(also failed as a pure value)"
                // suffix (which itself leaked the dual-wrap detail).
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
                        self.run_reference_fragment(
                            turn,
                            inner_type,
                            EvalMode::Pure,
                            handlers,
                            captured,
                        )
                    }
                    Err(pure_err) => TurnOutcome::Error(compile_fail(&pure_err, &pure_src)),
                }
            }
        }
    }

    /// Run a compiled reference fragment on the resident machine, resolving any
    /// session binders through the seeded `ExternalEnv` (load-through-slot).
    /// `inner_type` is the caller-resolved inner value type (`a` in `M a`).
    /// `mode` selects effectful (`run_fragment`) vs pure (`run_fragment_pure`).
    fn run_reference_fragment<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        turn: tidepool_runtime::session::SessionTurnResult,
        inner_type: Option<String>,
        mode: EvalMode,
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        if turn.warnings.has_io {
            return io_type_fail();
        }
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(tag_failure(FailureClass::Runtime, Phase::Run, e));
        }
        // Seed the machine from an Eff fragment first, so its table has the
        // freer cons even when THIS fragment is pure (a pure fragment omits
        // `Val` etc.; a later Eff fragment would then fail).
        self.ensure_effect_machine();
        if self.machine.is_none() {
            match JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size)
            {
                Ok(m) => self.bootstrap_machine(m),
                Err(e) => return TurnOutcome::Error(run_fail("JIT compile error", e)),
            }
        }
        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_ref_{}", self.turn_counter);

        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(&frag_name, &turn.expr, &self.session_table, &env) {
            Ok(f) => f,
            Err(e) => return TurnOutcome::Error(run_fail("JIT reference add_function error", e)),
        };
        let run_result = match mode {
            EvalMode::Pure => machine.run_fragment_pure(fid),
            EvalMode::Effectful => {
                machine.run_fragment(fid, &self.session_table, handlers, captured)
            }
        };
        match run_result {
            Ok(value) => {
                let rendered = value_to_json(&value, &self.session_table, 0);
                self.value_outcome(rendered, inner_type)
            }
            Err(e) => TurnOutcome::Error(run_fail("runtime error", e)),
        }
    }

    /// GHCi-style `it`: a confirmed bare final EXPRESSION (never a bind or a
    /// discard-bind RHS — those keep the unchanged `run_plain_eval`/
    /// `run_session_reference` path) is bound to `it`, rebinding on every such
    /// turn. The expression's effects (if any) fire EXACTLY ONCE.
    ///
    /// **Single compile, single run:** wraps `expr_text` as a MATERIALIZING
    /// bind whose `result` yields the TUPLE `(it, toWire it)` — `it <- __user`
    /// tried first (monadic — `__user` hoisted to a module-level binding so a
    /// trailing `where` still attaches, mirroring `wrap_probe_source`); on a
    /// compile failure, `let it = __user` (pure fallback — `let` doesn't
    /// require the RHS to unify with the session's `Eff` stack, matching
    /// `wrap_pure_ref_source`'s existing Eff-then-pure retry). Either way,
    /// `run_fragment_and_bind_render` runs the ONE compiled fragment exactly
    /// once: field 0 (`it`) drives `__user`'s effect and gets tenured; field 1
    /// (`toWire it`, pure) is bridged to an owned `Value` for the response —
    /// see that method's doc for why bridging field 1 BEFORE tenuring field 0
    /// makes this safe even when `toWire` is the identity and the two fields
    /// alias the same heap object (`pure input`).
    fn run_bare_expr<H: DispatchEffect<CapturedOutput>>(
        &mut self,
        expr_text: &str,
        imports: &str,
        inject: &[String],
        handlers: &mut H,
        captured: &CapturedOutput,
    ) -> TurnOutcome {
        let preamble = self.patched_preamble();
        let g = self.val_gen.next();
        let eval_input = self.eval_input.clone();
        // TWO names, matching the `(it, toWire it)` tuple `result` now
        // yields: this rides the SAME multi-binder `splitTupleType` path
        // `run_multi_bind`/`wrap_multi_bind_source` already use (purely
        // type-driven — `--bind-name` need not correspond to a real bound
        // identifier in the source), so `emitBindArtifacts` splits
        // `result`'s captured `(T, Value)` type into `it :: T` +
        // `__it_render :: Value` instead of (wrongly) taking the whole tuple
        // type as `it`'s type — which is what a single `--bind-name it`
        // would do. `__it_render`'s binder metadata is discarded below (only
        // `turn.binders[0]`, i.e. `it`, is used); its harmless synthetic
        // export just rides along in the same thin session iface.
        let it_names = vec!["it".to_string(), "__it_render".to_string()];

        let monadic_src = wrap_bare_it_monadic(
            &preamble,
            &self.cfg.effect_stack,
            imports,
            expr_text,
            eval_input.as_ref(),
        );
        let monadic_result = {
            let include = self.turn_include();
            compile_session_turn(
                &monadic_src,
                &include,
                self.session_root(),
                inject,
                Some(SessionBind {
                    names: &it_names,
                    gen: g.0,
                }),
            )
        };

        let turn = match monadic_result {
            Ok(t) => t,
            Err(_monadic_err) => {
                let pure_src = wrap_bare_it_pure(
                    &preamble,
                    &self.cfg.effect_stack,
                    imports,
                    expr_text,
                    eval_input.as_ref(),
                );
                let include = self.turn_include();
                match compile_session_turn(
                    &pure_src,
                    &include,
                    self.session_root(),
                    inject,
                    Some(SessionBind {
                        names: &it_names,
                        gen: g.0,
                    }),
                ) {
                    Ok(t) => t,
                    Err(pure_err) => return TurnOutcome::Error(compile_fail(&pure_err, &pure_src)),
                }
            }
        };

        if turn.warnings.has_io {
            return TurnOutcome::Error(tag_failure(
                FailureClass::UserHaskell,
                Phase::Compile,
                "IO type detected in bound value. IO operations are not supported.".into(),
            ));
        }
        let it_binder = match turn.binders.into_iter().next() {
            Some(b) => b,
            None => {
                return TurnOutcome::Error(tag_failure(
                    FailureClass::Infra,
                    Phase::Compile,
                    "bare-expression bind produced no binder metadata".into(),
                ))
            }
        };
        if let Err(e) = self.merge_table(&turn.table) {
            return TurnOutcome::Error(tag_failure(FailureClass::Runtime, Phase::Run, e));
        }

        // Bootstrap the resident machine on the first turn from THIS turn's
        // table (an Eff module either way — both wraps end in
        // `pure (it, toWire it)`).
        if self.machine.is_none() {
            match JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size)
            {
                Ok(m) => self.bootstrap_machine(m),
                Err(e) => return TurnOutcome::Error(run_fail("JIT compile error", e)),
            }
        }

        let env = self.bindings.seed_external_env();
        self.turn_counter += 1;
        let frag_name = format!("repl_it_{}", self.turn_counter);
        let machine = self.machine.as_mut().expect("machine bootstrapped above");
        let fid = match machine.add_function(&frag_name, &turn.expr, &self.session_table, &env) {
            Ok(f) => f,
            Err(e) => {
                return TurnOutcome::Error(run_fail("JIT bare-expression add_function error", e))
            }
        };

        // Run EXACTLY ONCE: the effectful step loop drives `__user` to
        // completion here (field 0's bind), whichever wrap compiled. Field 1
        // (`toWire it`, pure) rides along in the SAME run — no second
        // compile, no second execution of `__user`'s effect.
        let (it_slot, rendered_value) = match machine.run_fragment_and_bind_render(
            fid,
            &self.session_table,
            handlers,
            captured,
            matches!(it_binder.tier, ValueTier::Tier0Data),
        ) {
            Ok(sv) => sv,
            Err(e) => return TurnOutcome::Error(run_fail("runtime error", e)),
        };

        self.val_gen = g;
        let it_value = bound_value(it_binder.tier, it_slot);
        self.bind_materialized(BindingEntry {
            name: BindingName("it".to_string()),
            id: SessionVarId::from_extract(it_binder.var_id),
            module: SessionModule::val(g),
            value: it_value,
            type_display: Some(it_binder.type_display.clone()),
            defining_expr: Some(expr_text.to_string()),
        });

        let rendered = value_to_json(&rendered_value, &self.session_table, 0);
        self.value_outcome_bound_it(rendered, Some(it_binder.type_display))
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
        let imports = self.turn_imports(expr_text);
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
                // The unified environment view: materialized (effectful) value
                // binds AND pure binds (decl-backed, GHCi-environment model).
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
                bindings.extend(self.pure_binds.iter().map(|(name, pb)| {
                    serde_json::json!({
                        "name": name,
                        "type": pb.type_display,
                        "module": tidepool_repr::SessionModule::lib(pb.gen).module_name(),
                        // A pure bind is a lazy decl, not a materialized value.
                        "tier": "DeclBacked",
                    })
                }));
                bindings.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                TurnOutcome::Meta(serde_json::json!({
                    "bindings": bindings,
                    "generation": self.lib.generation().0,
                    "valGeneration": self.val_gen.0,
                }))
            }
            MetaCommand::Reset => {
                // Rebuild-then-swap: open the new lib FIRST, mutate `self` only
                // on success. If `SessionLib::open` fails (IO error), the
                // session is left completely untouched instead of a half-reset
                // state (old decl log kept, but every bind that referenced it
                // already cleared) behind an error implying nothing changed.
                match SessionLib::open(
                    self.cfg.id,
                    self.cfg.root.clone(),
                    self.cfg.module_env.clone(),
                ) {
                    Ok(lib) => {
                        self.lib = lib.with_validation_include(self.cfg.base_include.clone());
                        // Drop the resident machine (frees the session heap +
                        // every persistent root) and clear both planes: the
                        // decl log (just rebuilt above at gen 0) and the value
                        // bindings + session table.
                        self.machine = None;
                        // `self.machine = None` bypasses `bootstrap_machine`,
                        // so publish the (now empty) cancel handle explicitly —
                        // otherwise the shared `CancelSlot` keeps the dropped
                        // machine's stale `CancelHandle` until the next
                        // bootstrap, and a server-side timeout in that window
                        // cancels a dead flag instead of a live one.
                        self.publish_cancel();
                        self.turn_counter = 0;
                        self.bindings = BindingTable::new();
                        self.val_gen = Generation(0);
                        self.session_table = DataConTable::new();
                        self.last_stubs = Vec::new();
                        self.pure_binds.clear();
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
                let imports = self.turn_imports(expr);
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
                        return TurnOutcome::Meta(serde_json::json!({
                            "error": format!(
                                "compile error: {}",
                                remap_item_err(&e.to_string(), &wrapped)
                            )
                        }))
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
                // 1b. Pure bind (decl-backed) — part of the environment too.
                if let Some(pb) = self.pure_binds.get(name) {
                    return TurnOutcome::Meta(serde_json::json!({
                        "name": name,
                        "type": pb.type_display,
                        "tier": "DeclBacked",
                        "module": tidepool_repr::SessionModule::lib(pb.gen).module_name(),
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
                // 3b. Session-defined values/functions (`f x = …`). These are
                // decls, not bindings or types, so they fell through to a total
                // miss before — the exact place a caller reaches for `:i`. (#318)
                if let Some(src) = self.lib.decl_value_source(name) {
                    return TurnOutcome::Meta(serde_json::json!({
                        "name": name,
                        "shape": src,
                        "source": "session",
                    }));
                }
                // 4. Stdlib/preamble types (`Proc`, `Hit`, … — source-scanned
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
            MetaCommand::Program => {
                // Repaint the session as a replayable notebook: declarations
                // first (in log order — they never depend on binds), then each
                // live bind's defining text (val-gen order). Effectful binds
                // re-RUN their effect on replay; that's honest — the heap is a
                // cache of this document, not the source of truth.
                let decls: Vec<&str> = self.lib.decl_sources();
                let mut binds: Vec<(&BindingName, &BindingEntry)> =
                    self.bindings.iter_current().collect();
                binds.sort_by_key(|(_, e)| e.module.gen.0);

                let mut program = String::new();
                for d in &decls {
                    program.push_str(d.trim_end());
                    program.push_str("\n\n");
                }
                if !binds.is_empty() {
                    program.push_str("-- binds (re-run to restore heap values):\n");
                    for (name, e) in &binds {
                        match &e.defining_expr {
                            Some(src) => {
                                program.push_str(src.trim_end());
                                program.push('\n');
                            }
                            None => program.push_str(&format!(
                                "-- {} :: {} (defining text unavailable)\n",
                                name.0,
                                e.type_display.clone().unwrap_or_default()
                            )),
                        }
                    }
                }
                TurnOutcome::Meta(serde_json::json!({
                    "program": program,
                    "decls": decls.len(),
                    "binds": binds.len(),
                    "generation": self.lib.generation().0,
                }))
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
                TurnOutcome::Meta(serde_json::json!({
                    "vocab": vocab,
                    // The vocab digest lists .tidepool/lib verbs (each module
                    // tagged bare-in-scope vs needs-import). For the built-in
                    // EFFECT verbs (run/grepGlob/kvSet/…), which live in the
                    // effect decls not the lib dirs, use `:browse`.
                    "hint": ":browse lists built-in effect verbs (:browse <Effect> for one effect's \
                             verbs+constructors). Module tags below: \"bare (Library re-export)\" = \
                             in scope directly; \"needs: import <Mod>\" = add that import first.",
                }))
            }
            MetaCommand::Browse(only) => {
                TurnOutcome::Meta(browse_effects(&self.cfg.decls, only.as_deref()))
            }
        }
    }

    /// A read-only JSON snapshot of the live session environment for the
    /// `tidepool://session/bindings` resource: one entry per in-scope binding.
    /// Each is `{name, type, kind (decl|bind), generation}`. `decl` entries are
    /// the decl-plane heads (`f x = …`, `data Foo`, `class C`); `bind` entries
    /// are value-plane binds (`x <- e`) and pure decl-backed binds (`let x = e`,
    /// `x <- pure e`). A pure bind lives in the decl log too, so its name is
    /// EXCLUDED from the decl listing here and surfaced once, as a `bind`.
    pub fn bindings_snapshot(&self) -> serde_json::Value {
        let mut entries: Vec<serde_json::Value> = Vec::new();
        // Decl plane: current in-scope heads (latest-wins), minus pure-bind
        // names (those are surfaced as `bind` below).
        for (name, gen) in self.lib.current_decl_heads() {
            if self.pure_binds.contains_key(&name) {
                continue;
            }
            entries.push(serde_json::json!({
                "name": name,
                "type": "",
                "kind": "decl",
                "generation": gen,
            }));
        }
        // Value plane: materialized (effectful) binds.
        for (name, entry) in self.bindings.iter_current() {
            entries.push(serde_json::json!({
                "name": name.0,
                "type": entry.type_display.clone().unwrap_or_default(),
                "kind": "bind",
                "generation": entry.module.gen.0,
            }));
        }
        // Pure binds (decl-backed values) — environment members, kind `bind`.
        for (name, pb) in &self.pure_binds {
            entries.push(serde_json::json!({
                "name": name,
                "type": pb.type_display,
                "kind": "bind",
                "generation": pb.gen.0,
            }));
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        serde_json::json!({
            "bindings": entries,
            "generation": self.lib.generation().0,
            "valGeneration": self.val_gen.0,
        })
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

    /// Ensure the resident machine exists AND its DataConTable carries the freer
    /// `Eff` constructors (`Val`/`Leaf`/`Node`/…), by bootstrapping from a
    /// trivial Eff fragment (`pure ()`) if the machine hasn't started. A PURE
    /// fragment's table omits those cons, so a session whose FIRST machine-using
    /// op is a pure reference — common now that pure binds are decls that don't
    /// bootstrap — would otherwise fail a later Eff fragment with "missing
    /// freer-simple constructor 'Val'". No-op once the machine is up.
    fn ensure_effect_machine(&mut self) {
        if self.machine.is_some() {
            return;
        }
        let preamble = self.patched_preamble();
        let imports = self.session_imports();
        let inject = self.live_val_modules();
        let eval_input = self.eval_input.clone();
        let src = template_haskell_show_default(
            &preamble,
            &self.cfg.effect_stack,
            "pure ()",
            &imports,
            "",
            eval_input.as_ref(),
            None,
        );
        let compiled = {
            let include = self.turn_include();
            compile_session_turn(&src, &include, self.session_root(), &inject, None)
        };
        if let Ok(turn) = compiled {
            let _ = self.merge_table(&turn.table);
            if let Ok(m) =
                JitEffectMachine::compile_session(&turn.expr, &turn.table, self.cfg.nursery_size)
            {
                self.bootstrap_machine(m);
            }
        }
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

    /// The server's effect decls — the worker re-ensures the generated
    /// Tidepool.Effects staging dir from these before each turn (self-heal).
    pub fn decls(&self) -> &[EffectDecl] {
        &self.inner.cfg.decls
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

    /// A read-only JSON snapshot of the live session bindings — the worker
    /// republishes it after each turn for the `tidepool://session/bindings`
    /// resource.
    pub fn bindings_snapshot(&self) -> serde_json::Value {
        self.inner.bindings_snapshot()
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
/// Locate the user's code inside a wrapped module source by the markers the
/// wrappers emit, returning `(line_offset, col_indent)` for coordinate remap.
/// `__user =` (pure-ref / shared eval template, optionally `do`-wrapped) is
/// checked FIRST — those templates also contain a `result` binding, but the
/// user text lives under `__user`. Bind wrappers emit no `__user`, so their
/// `result = do` is the marker.
fn user_code_offset(source: &str) -> Option<(usize, usize)> {
    // Verbatim embeddings (pure-ref / probe / shared eval template): user
    // text starts right after the bracket, at its ORIGINAL columns — line
    // offset only, no column indent.
    for marker in ["__user = let {\n __b =\n", "__probe = let {\n __b =\n"] {
        if let Some(pos) = source.find(marker) {
            return Some((source[..pos + marker.len()].matches('\n').count(), 0));
        }
    }
    // Bind wrappers: verbatim inside `result = do {`. (A `let` bind's first
    // line gains 2 columns from the decl-brace boundary edit — accepted.)
    const RESULT_DO: &str = "\nresult = do {\n";
    source
        .find(RESULT_DO)
        .map(|pos| (source[..pos + RESULT_DO.len()].matches('\n').count(), 0))
}

/// Remap `Expr.hs:<L>:<C>` GHC coordinates in a compile error to item-relative
/// ones (`<item>:l:c`), using the wrapped source the error was produced from.
/// Foreign paths pass through; unknown wrappers return the error untouched.
/// Prepend the greppable failure-class/phase tag line onto a repl error message
/// so a caller can branch on class/phase, exactly as the MCP server's envelope
/// does. Both servers share the ONE `tidepool_runtime` classifier; this only
/// formats. The original message is embedded verbatim, so string sniffs over it
/// (e.g. `is_parse_error`) still match.
fn tag_failure(class: FailureClass, phase: Phase, body: String) -> String {
    format!(
        "**failure-class:** `{}`  **phase:** `{}`\n{}",
        class.tag(),
        phase.tag(),
        body
    )
}

/// A compile-phase failure envelope from a structured [`CompileError`] (the
/// eval/bind/reference path): classify it and remap GHC coords in the message
/// onto the user's item. A wire-format skew (`CompileError::ReadError`) becomes
/// version-skew/compile here; skew/infra messages carry no coords, so the remap
/// is a passthrough for them.
fn compile_fail(err: &CompileError, source: &str) -> String {
    let env = classify_compile(err);
    tag_failure(env.class, env.phase, remap_item_err(&env.message, source))
}

/// A compile-phase failure envelope from the declaration path's [`SessionError`].
/// The body is the ORIGINAL `"<prefix>: <err>"` text (not the classifier's
/// re-messaging), so the Auto decl→stmt fallback's `is_parse_error` sniff still
/// finds the "binder extraction failed" marker; the envelope supplies only the
/// class/phase tag.
fn session_fail(err: &SessionError, prefix: &str) -> String {
    let env = classify_session(err);
    tag_failure(env.class, env.phase, format!("{prefix}: {err}"))
}

/// A run-phase JIT/eval failure envelope (always runtime/run), with `context`
/// naming the site (e.g. "runtime error", "JIT compile error").
fn run_fail(context: &str, detail: impl std::fmt::Display) -> String {
    tag_failure(
        FailureClass::Runtime,
        Phase::Run,
        format!("{context}: {detail}"),
    )
}

/// The result binding typed to IO — a compile-phase user-Haskell constraint.
/// (Shared across the eval/bind/multi-bind/reference paths.)
fn io_type_fail() -> TurnOutcome {
    TurnOutcome::Error(tag_failure(
        FailureClass::UserHaskell,
        Phase::Compile,
        "IO type detected in result binding. IO operations are not supported.".into(),
    ))
}

fn remap_item_err(err: &str, source: &str) -> String {
    // Session-lib generation warnings are dependency noise on the stmt plane
    // (a gen-25 -Wx-partial otherwise rides every later item's errors).
    let deduped = drop_foreign_gen_warnings(&dedupe_diagnostics(err), None);
    let remapped = match user_code_offset(source) {
        Some((offset, indent)) => {
            remap_generated_coords(&deduped, "Expr.hs", "<item>", offset, indent)
        }
        None => deduped,
    };
    prepend_lib_brick_hint(remapped)
}

/// If EVERY error location in a failed compile points at a project-lib module
/// (`.tidepool/lib/…` / global `…/tidepool/lib/…`) rather than the user's
/// `<item>`, the user's item is fine — a library module is broken and the
/// auto-imported `Library` re-exports it, so it fails to compile for every
/// eval (friction #24, the "bricked lib module" class). Say so and name the
/// culprit(s) instead of leaving a baffling error against code the user
/// didn't just write.
fn prepend_lib_brick_hint(err: String) -> String {
    // A `<item>:` coordinate means the user's own code is implicated — never
    // reframe then.
    if err.contains("<item>:") {
        return err;
    }
    let mut culprits: Vec<String> = err
        .lines()
        .filter_map(|l| {
            let hs = l.find(".hs:")?;
            let seg = &l[..hs + 3];
            let start = seg.rfind(char::is_whitespace).map_or(0, |i| i + 1);
            let path = &seg[start..];
            (path.contains(".tidepool/lib/") || path.contains("/tidepool/lib/"))
                .then(|| path.to_string())
        })
        .collect();
    if culprits.is_empty() {
        return err;
    }
    culprits.sort_unstable();
    culprits.dedup();
    format!(
        "your item is fine — a project library module is broken and `Library` \
         auto-imports it, so it won't compile for ANY eval until fixed: {}\n\
         (fix or remove the module; if the break also blocks a `writeFile` repair, \
         edit it host-side — the session can't compile its own fix while `Library` \
         is broken)\n\n{err}",
        culprits.join(", ")
    )
}

fn begin_user_module(preamble: &str, imports: &str, input: Option<&serde_json::Value>) -> String {
    let mut out = insert_imports(preamble, imports);
    out.push_str("-- [user]\n");
    out.push_str(&input_binding_source(input));
    out
}

/// Append `name = <text>` with the user text embedded VERBATIM — no
/// indentation transform. Explicit `let { }` brackets suspend the layout
/// algorithm (Report rule L, explicit context), so unindented user lines are
/// legal and quasiquote payloads keep byte-exact fidelity (per-line indenting
/// was the "+2 corrupts multi-line QQ" bug class). `__b` is local to each RHS.
fn push_verbatim_binding(out: &mut String, name: &str, text: &str) {
    out.push_str(name);
    out.push_str(" = let {\n __b =\n");
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(" } in __b\n");
}

/// Emit a turn's statement VERBATIM inside an explicit `do { }` block. `let`
/// statements need explicit decl braces there (a layout `let` swallows the
/// following `;` — pinned by jit_surface's
/// `let_in_braced_do_fails_loudly`): two boundary edits at known
/// positions, never interior transformation, so payloads and columns survive
/// byte-exact (modulo +2 on the `let`'s own first line).
fn push_braced_stmt(out: &mut String, turn_text: &str) {
    let trimmed = turn_text.trim_start();
    let let_rest = trimmed
        .strip_prefix("let")
        .filter(|r| r.starts_with(|c: char| c.is_whitespace()));
    match let_rest {
        Some(rest) if !rest.trim_start().starts_with('{') => {
            out.push_str("let {");
            out.push_str(rest);
            if !rest.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(" }\n");
        }
        _ => {
            out.push_str(turn_text);
            if !turn_text.ends_with('\n') {
                out.push('\n');
            }
        }
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
    out.push_str("result = do {\n");
    push_braced_stmt(&mut out, turn_text);
    // Column-1 closer: closes any user implicit contexts (n < m) down to the
    // explicit brace context. (Edge: a user inner block aligned at exactly
    // column 1 would receive a layout `;` here — vanishingly rare, loud.)
    out.push_str(&format!(" ; pure {binder}\n }}\n"));
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
    out.push_str("result = do {\n");
    push_braced_stmt(&mut out, turn_text);
    out.push_str(&format!(" ; pure {tuple_expr}\n }}\n"));
    out
}

/// Wrap a bare EXPRESSION as a MATERIALIZING bind of `it` — the monadic
/// attempt (tried first by [`Session::run_bare_expr`]): `it <- __user`, an
/// `Eff`-typed action, exactly like a real `x <- e` bind. `__user` is hoisted
/// to a module-level binding (not inlined into the do-block) so a trailing
/// `where` on the user's expression still attaches legally — mirrors
/// `wrap_probe_source`/`wrap_pure_ref_source`.
///
/// `result` yields `(it, toWire it)` — ONE compile, run via
/// `run_fragment_and_bind_render` (`run_bare_expr`'s single-compile bind+render
/// primitive): field 0 (`it`) is the ONE execution of `__user`'s effect; field
/// 1 (`toWire it`, pure) rides along in the same run and is bridged to an
/// owned `Value` BEFORE field 0 is tenured, so the two fields aliasing the
/// same heap object (`toWire = id`, e.g. `pure input`) cannot corrupt — see
/// `run_fragment_and_bind_render`'s doc for the read-before-tenure ordering.
fn wrap_bare_it_monadic(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    expr_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    push_verbatim_binding(&mut out, "__user", expr_text);
    out.push('\n');
    out.push_str(&format!("result :: Eff {effect_stack} _\n"));
    out.push_str("result = do {\n it <- __user ; pure (it, toWire it)\n }\n");
    out
}

/// Pure-fallback sibling of [`wrap_bare_it_monadic`], tried when the monadic
/// wrap fails to compile (`__user`'s type doesn't unify with the session's
/// `Eff` stack — a bare non-monadic expression like `x + 1` / `v ^? key …`).
/// `let` doesn't require its RHS to unify with the do-block's monad, so this
/// binds `it` materially without invoking any effect. Same single-compile
/// `(it, toWire it)` tuple as the monadic wrap.
fn wrap_bare_it_pure(
    preamble: &str,
    effect_stack: &str,
    imports: &str,
    expr_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    push_verbatim_binding(&mut out, "__user", expr_text);
    out.push('\n');
    out.push_str(&format!("result :: Eff {effect_stack} _\n"));
    out.push_str("result = do {\n let { it = __user } ; pure (it, toWire it)\n }\n");
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
/// Insert `NoMonomorphismRestriction` into a preamble's `LANGUAGE` pragma (the
/// same edit [`tidepool_mcp::decl_pragmas`] makes), so a probe compile
/// generalizes a constrained pure bind instead of failing to monomorphize it.
/// Idempotent — a no-op if NMR is already present.
fn to_nmr_pragmas(preamble: &str) -> String {
    if preamble.contains("NoMonomorphismRestriction") {
        return preamble.to_string();
    }
    preamble.replacen(
        "NoImplicitPrelude,",
        "NoImplicitPrelude, NoMonomorphismRestriction,",
        1,
    )
}

fn wrap_pure_ref_source(
    preamble: &str,
    imports: &str,
    expr_text: &str,
    input: Option<&serde_json::Value>,
) -> String {
    let mut out = begin_user_module(preamble, imports, input);
    push_verbatim_binding(&mut out, "__user", expr_text);
    out.push('\n');
    push_verbatim_binding(&mut out, "result", expr_text);
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
    push_verbatim_binding(&mut out, "__probe", expr_text);
    out.push('\n');
    out.push_str(&format!("result :: Eff {effect_stack} _\n"));
    out.push_str("result = do\n");
    out.push_str("  __t <- __probe\n");
    out.push_str("  pure __t\n");
    out
}

/// Normalize a PURE bind of `name` to its equivalent top-level declaration
/// `name = <rhs>`. `let name = e` → `name = e`; `name <- pure e` /
/// `name <- return e` → `name = e`. Returns `None` for effectful binds or
/// shapes we don't confidently normalize (which then take the materialize
/// path). `pure`/`return` of a value is semantically pure, so unwrapping one
/// layer is exact.
fn pure_bind_to_decl(expr_text: &str, name: &str) -> Option<String> {
    let t = expr_text.trim();
    // `let name = e` (single binder — classify already gave one binder `name`).
    if let Some(rest) = t.strip_prefix("let ") {
        let rest = rest.trim_start();
        // Guard against `let { … }` explicit-brace / pattern shapes.
        if rest.starts_with(name) {
            return Some(rest.trim().to_string());
        }
        return None;
    }
    // `name <- pure e` / `name <- return e`.
    let after_name = t.strip_prefix(name)?.trim_start();
    let rhs = after_name.strip_prefix("<-")?.trim_start();
    for kw in ["pure ", "return "] {
        if let Some(e) = rhs.strip_prefix(kw) {
            return Some(format!("{name} = {}", e.trim()));
        }
    }
    None
}

/// Whether `expr_text` is a MONADIC pure bind (`name <- pure e` /
/// `name <- return e`) whose RHS `e` references `name`. Such a bind must read
/// the PRIOR `name` and shadow — GHCi `>>=` semantics (`pure e >>= \name -> …`
/// evaluates `e` in the outer scope, so `name` there is the old binding). The
/// decl route ([`pure_bind_to_decl`]) would instead emit a top-level
/// `name = e`, which in Haskell is RECURSIVE (`n = n + 1` self-forces to a
/// blackhole). So these divert to the materialize/shadow path.
///
/// `let name = e` is deliberately EXCLUDED: Haskell `let` is recursive, so
/// `let n = n + 1` looping matches GHCi — only the `<-` form shadows.
fn self_referential_monadic_pure_bind(expr_text: &str, name: &str) -> bool {
    let t = expr_text.trim();
    if t.starts_with("let ") {
        return false;
    }
    let Some(rhs) = t
        .strip_prefix(name)
        .map(str::trim_start)
        .and_then(|a| a.strip_prefix("<-"))
        .map(str::trim_start)
    else {
        return false;
    };
    ["pure ", "return "]
        .iter()
        .find_map(|kw| rhs.strip_prefix(kw))
        .is_some_and(|e| mentions_word(e, name))
}

/// Whether `text` contains `word` as a whole identifier (Haskell ident
/// boundaries: alnum, `_`, `'`). Used to find binds that reference a
/// redefined decl (the `stale:` field).
fn mentions_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '\'';
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(rel) = text[start..].find(word) {
        let i = start + rel;
        let before_ok = i == 0 || !text[..i].chars().next_back().is_some_and(ident);
        let after = i + word.len();
        let after_ok = after >= bytes.len() || !text[after..].chars().next().is_some_and(ident);
        if before_ok && after_ok {
            return true;
        }
        start = i + 1;
    }
    false
}

/// Whether a decl item's text contains a DEFINING equation for `head` (as
/// opposed to only a type signature `head :: T`). Drives the within-block
/// redefinition split in the decl batcher (#320): sig+binding pairs must stay
/// batched; two defining items for one head must not. A heuristic over lines
/// (operator heads in prefix parens are not detected — GHC's verdict stays
/// authoritative for what actually compiles).
fn defines_head(text: &str, head: &str) -> bool {
    if head.is_empty() {
        return false;
    }
    text.lines().any(|l| {
        let l = l.trim_start();
        match l.strip_prefix(head) {
            Some(rest) => {
                let boundary_ok = rest
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '\''));
                boundary_ok && !rest.trim_start().starts_with("::")
            }
            None => false,
        }
    })
}

/// For a discarding bind `pat <- e` (whose pattern binds no name, e.g. `_ <- e`
/// or `(_,_) <- e`), return the RHS `e` so it can run as a plain effectful
/// expression. The classifier has already confirmed a top-level `<-`, and a
/// pattern can't contain `<-`, so the first `<-` is the bind arrow. (#321)
fn split_discard_bind(text: &str) -> Option<&str> {
    let idx = text.find("<-")?;
    Some(text[idx + 2..].trim())
}

/// Strip leading blank lines and `--` line comments so `decl_head` extracts
/// the real token instead of the comment marker. A leading `-- slugify a
/// title\nslug t = ...` must classify head `"slug"`, not `"--"` — a stray
/// `"--"` head poisons the type-probe gate (`defines_head`), the within-block
/// redefinition splitter, and `mentions_word`'s stale-bind detection, since
/// all three key off the (wrong) head text.
///
/// Follows the Haskell lexical rule that a dash run immediately followed by
/// another symbol character is an OPERATOR, not a comment (`-->`, `|--`), so
/// such lines are left alone for the caller's own parsing.
fn strip_leading_comments(text: &str) -> &str {
    const SYMBOL_CHARS: &str = "!#$%&*+./<=>?@\\^|~:-";
    let mut s = text;
    loop {
        let trimmed = s.trim_start_matches(char::is_whitespace);
        match trimmed.strip_prefix("--") {
            Some(rest) if !rest.starts_with(|c: char| SYMBOL_CHARS.contains(c)) => {
                s = match rest.find('\n') {
                    Some(nl) => &rest[nl + 1..],
                    None => "",
                };
            }
            _ => return trimmed,
        }
    }
}

/// Extract the declared head identifier from a Haskell declaration string,
/// for the slim `{"decl":"name"}` block item result. Strips keyword prefixes
/// for type/class/instance declarations; for function definitions returns the
/// first identifier. Returns `""` for empty or unrecognised text.
fn decl_head(text: &str) -> &str {
    let s = strip_leading_comments(text).trim();
    // Import: name the module being imported, not the `import` keyword. (#317)
    if let Some(rest) = s.strip_prefix("import ") {
        let rest = rest.trim_start();
        let rest = rest.strip_prefix("qualified ").unwrap_or(rest).trim_start();
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '(')
            .unwrap_or(rest.len());
        return rest[..end].trim_end();
    }
    // Fixity: name the operator(s) being fixed, not the `infixl`/`infixr`/`infix`
    // keyword (skip the optional precedence digits). (#317)
    for kw in &["infixl ", "infixr ", "infix "] {
        if let Some(rest) = s.strip_prefix(kw) {
            return rest
                .trim_start()
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim();
        }
    }
    for kw in &["data ", "newtype ", "type ", "class ", "instance "] {
        if let Some(rest) = s.strip_prefix(kw) {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '(' || c == '=')
                .unwrap_or(rest.len());
            return rest[..end].trim_end();
        }
    }
    // Prefix operator definition `(<>) x y = …` / `(<>) = …`: name the operator
    // rather than returning "" (the `(` used to zero the token). (#317)
    if let Some(rest) = s.strip_prefix('(') {
        if let Some(close) = rest.find(')') {
            return rest[..close].trim();
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
            "bound": components.iter().map(|c| &c.name).collect::<Vec<_>>(),
            "types": components.iter().map(|c| &c.type_display).collect::<Vec<_>>(),
        }),
        TurnOutcome::Defined {
            head,
            type_display,
            stale,
            ..
        } => {
            let mut obj = serde_json::json!({ "decl": head });
            // Paint the inferred type the server had at compile time, so
            // `{decl:"heatOf"}` doesn't cost the caller a `:t` (#317). Omitted
            // (best-effort) for non-value decls and probe failures.
            if let Some(ty) = type_display.as_deref().filter(|t| !t.is_empty()) {
                obj["type"] = serde_json::json!(ty);
            }
            if !stale.is_empty() {
                obj["stale"] = serde_json::json!(stale);
            }
            obj
        }
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

/// Render the `:browse` result. Bare (`None`) lists every effect with its
/// one-line description; `Some(name)` (case-insensitive) lists that effect's
/// helper verbs (`name :: signature`) and constructors, or — for an unknown
/// name — an `error` payload naming the valid effects (the #319 not-ok
/// convention: a `Meta` object carrying an `error` key surfaces as not-ok).
fn browse_effects(decls: &[EffectDecl], only: Option<&str>) -> serde_json::Value {
    match only {
        None => {
            let effects: Vec<serde_json::Value> = decls
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "effect": d.type_name,
                        "description": first_sentence(d.description),
                    })
                })
                .collect();
            serde_json::json!({
                "effects": effects,
                "hint": ":browse <Effect> (case-insensitive) lists that effect's verbs + constructors.",
            })
        }
        Some(name) => match decls
            .iter()
            .find(|d| d.type_name.eq_ignore_ascii_case(name))
        {
            Some(d) => {
                let verbs: Vec<String> = d.helpers.iter().filter_map(|h| helper_sig(h)).collect();
                serde_json::json!({
                    "effect": d.type_name,
                    "description": d.description,
                    "verbs": verbs,
                    "constructors": d.constructors,
                    // Real field names, straight from the type_def — the doc
                    // prose above can drift, this can't (#346).
                    "types": d.type_defs,
                })
            }
            None => {
                let valid: Vec<&str> = decls.iter().map(|d| d.type_name).collect();
                serde_json::json!({
                    "error": format!(
                        "unknown effect '{name}'; valid effects: {}",
                        valid.join(", ")
                    ),
                    "effects": valid,
                })
            }
        },
    }
}

#[cfg(test)]
mod slim_tests {
    use super::self_referential_monadic_pure_bind;
    use super::{browse_effects, first_sentence, helper_sig, EffectDecl};
    use super::{
        decl_head, pure_bind_to_decl, slim_item_result, split_discard_bind,
        strip_leading_comments,
    };

    /// Two-effect fixture mirroring the real decl shape: a comment-prefixed
    /// helper (so the sig line is not the first line) and a multi-sentence
    /// description.
    fn sample_decls() -> Vec<EffectDecl> {
        vec![
            EffectDecl {
                type_name: "Exec",
                description: "Run shell commands. And capture output.",
                constructors: &["Run :: Text -> Exec (Int, Text, Text)"],
                type_defs: &[],
                helpers: &[
                    "-- | Run a shell command; returns a `Proc`.\nrun :: Text -> M Proc\nrun cmd = undefined",
                    "readProcess :: Text -> M Text\nreadProcess cmd = undefined",
                ],
            },
            EffectDecl {
                type_name: "KV",
                description: "Persistent key-value store.",
                constructors: &["KvSet :: Text -> Value -> KV ()"],
                type_defs: &[],
                helpers: &["kvSet :: Text -> Value -> M ()\nkvSet k v = undefined"],
            },
        ]
    }

    #[test]
    fn first_sentence_takes_leading_sentence() {
        assert_eq!(
            first_sentence("Run shell commands. And capture output."),
            "Run shell commands."
        );
        // No sentence break → whole (trimmed) string.
        assert_eq!(first_sentence("  just one clause  "), "just one clause");
    }

    #[test]
    fn helper_sig_skips_comments_and_finds_signature() {
        assert_eq!(
            helper_sig("-- | doc line\nrun :: Text -> M Proc\nrun cmd = undefined").as_deref(),
            Some("run :: Text -> M Proc")
        );
        // No comment: still the sig line.
        assert_eq!(
            helper_sig("kvSet :: Text -> Value -> M ()\nkvSet k v = undefined").as_deref(),
            Some("kvSet :: Text -> Value -> M ()")
        );
    }

    #[test]
    fn browse_bare_lists_effects_with_descriptions() {
        let v = browse_effects(&sample_decls(), None);
        let effects = v["effects"].as_array().expect("effects array");
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0]["effect"], "Exec");
        // One-line description only (first sentence).
        assert_eq!(effects[0]["description"], "Run shell commands.");
        assert_eq!(effects[1]["effect"], "KV");
        // Bare browse is not an error.
        assert!(v.get("error").is_none());
    }

    #[test]
    fn browse_known_effect_lists_verbs_and_constructors() {
        let v = browse_effects(&sample_decls(), Some("Exec"));
        assert_eq!(v["effect"], "Exec");
        let verbs: Vec<&str> = v["verbs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert!(verbs.contains(&"run :: Text -> M Proc"), "verbs: {verbs:?}");
        assert!(verbs.contains(&"readProcess :: Text -> M Text"));
        let cons = v["constructors"].as_array().unwrap();
        assert_eq!(cons[0], "Run :: Text -> Exec (Int, Text, Text)");
        assert!(v.get("error").is_none());
    }

    #[test]
    fn browse_is_case_insensitive() {
        let v = browse_effects(&sample_decls(), Some("exec"));
        assert_eq!(v["effect"], "Exec");
        assert!(v.get("error").is_none());
    }

    #[test]
    fn browse_unknown_effect_errors_with_candidates() {
        let v = browse_effects(&sample_decls(), Some("Nope"));
        let err = v["error"].as_str().expect("error string");
        // Names the valid effects so the caller can retry.
        assert!(err.contains("Exec"), "{err}");
        assert!(err.contains("KV"), "{err}");
        assert!(err.contains("Nope"), "{err}");
    }

    #[test]
    fn pure_bind_to_decl_normalizes() {
        // let x = e  ->  x = e
        assert_eq!(
            pure_bind_to_decl("let n = 5", "n").as_deref(),
            Some("n = 5")
        );
        // x <- pure e  ->  x = e
        assert_eq!(
            pure_bind_to_decl("xs <- pure []", "xs").as_deref(),
            Some("xs = []")
        );
        // x <- return e  ->  x = e
        assert_eq!(
            pure_bind_to_decl("y <- return (foo bar)", "y").as_deref(),
            Some("y = (foo bar)")
        );
        // effectful bind: not normalized (falls back to materialize)
        assert_eq!(pure_bind_to_decl("p <- run \"ls\"", "p"), None);
        // let with a type annotation on the RHS survives
        assert_eq!(
            pure_bind_to_decl("let d = 5 :: Double", "d").as_deref(),
            Some("d = 5 :: Double")
        );
    }

    #[test]
    fn self_ref_monadic_pure_bind_detected() {
        // The accumulator idiom: `<-` form referencing the prior binding — must
        // divert to materialize (shadow), NOT the recursive decl route.
        assert!(self_referential_monadic_pure_bind("n <- pure (n + 1)", "n"));
        assert!(self_referential_monadic_pure_bind(
            "xs <- return (0 : xs)",
            "xs"
        ));
        // Non-self-referential `<-` pure binds still take the decl route.
        assert!(!self_referential_monadic_pure_bind("xs <- pure []", "xs"));
        assert!(!self_referential_monadic_pure_bind(
            "n <- pure (m + 1)",
            "n"
        ));
        // `let` is recursive in GHCi — left on the decl route intentionally.
        assert!(!self_referential_monadic_pure_bind("let n = n + 1", "n"));
        // A substring of the name is not a self-reference (whole-word only).
        assert!(!self_referential_monadic_pure_bind(
            "n <- pure (nn + 1)",
            "n"
        ));
    }
    use crate::command::TurnOutcome;

    #[test]
    fn lib_brick_hint_reframes_lib_only_errors() {
        let lib_err = "/home/u/proj/.tidepool/lib/Repo.hs:31:12: error: [GHC-88464]\n    Variable not in scope: readGlob\nCompilation failed.\n";
        let out = super::prepend_lib_brick_hint(lib_err.to_string());
        assert!(out.starts_with("your item is fine"), "{out}");
        assert!(out.contains(".tidepool/lib/Repo.hs"), "{out}");
        // A user-item error is never reframed.
        let user_err = "<item>:2:1: error: not in scope: foo\n";
        assert_eq!(
            super::prepend_lib_brick_hint(user_err.to_string()),
            user_err
        );
        // A base/non-lib error is not reframed.
        let base_err = "Expr.hs:5:1: error: whatever\n";
        assert_eq!(
            super::prepend_lib_brick_hint(base_err.to_string()),
            base_err
        );
    }

    #[test]
    fn decl_head_extracts_names() {
        assert_eq!(decl_head("slug t = T.replace \" \" \"-\" t"), "slug");
        assert_eq!(decl_head("data Foo = Bar | Baz"), "Foo");
        assert_eq!(
            decl_head("newtype Wrapper a = Wrapper { unwrap :: a }"),
            "Wrapper"
        );
        assert_eq!(decl_head("type Name = Text"), "Name");
        assert_eq!(decl_head("class MyClass a where"), "MyClass");
        assert_eq!(decl_head("  f x = x + 1"), "f");
        assert_eq!(decl_head(""), "");
        // #317: import → module, fixity → operator, prefix-op def → operator.
        assert_eq!(decl_head("import Data.Char"), "Data.Char");
        assert_eq!(decl_head("import qualified Data.Map as M"), "Data.Map");
        assert_eq!(decl_head("infixl 6 <+>"), "<+>");
        assert_eq!(decl_head("infixr 5 >>>"), ">>>");
        assert_eq!(decl_head("(<+>) = (++)"), "<+>");
        assert_eq!(decl_head("(<>) x y = x <> y"), "<>");
    }

    #[test]
    fn decl_head_skips_leading_comments() {
        // A leading `-- comment` line must not poison the head as "--".
        assert_eq!(
            decl_head("-- slugify a title\nslug t = T.replace \" \" \"-\" t"),
            "slug"
        );
        // Blank lines + multiple comment lines before the real decl.
        assert_eq!(
            decl_head("\n-- first note\n-- second note\ndata Foo = Bar"),
            "Foo"
        );
        // A dash-run immediately followed by a symbol char is an OPERATOR, not
        // a comment (Haskell lexical rule) — left untouched.
        assert_eq!(decl_head("(-->) x y = x"), "-->");
        assert_eq!(strip_leading_comments("--> merge x y"), "--> merge x y");
    }

    #[test]
    fn defines_head_sig_vs_binding() {
        use super::defines_head;
        // A binding defines; a bare signature does not.
        assert!(defines_head("rf x = x + 1", "rf"));
        assert!(!defines_head("rf :: Int -> Int", "rf"));
        // Sig+binding in one item defines.
        assert!(defines_head("rf :: Int -> Int\nrf x = x + 1", "rf"));
        // Identifier-boundary: `rfoo` does not define `rf`.
        assert!(!defines_head("rfoo x = 1", "rf"));
        // Multi-clause single item defines (once).
        assert!(defines_head("f 0 = 0\nf n = n", "f"));
    }

    #[test]
    fn split_discard_bind_strips_pattern() {
        assert_eq!(
            split_discard_bind("_ <- pure (5 :: Int)"),
            Some("pure (5 :: Int)")
        );
        assert_eq!(
            split_discard_bind("(_, _) <- pure (1, 2)"),
            Some("pure (1, 2)")
        );
        assert_eq!(
            split_discard_bind("_ <- run \"echo hi\""),
            Some("run \"echo hi\"")
        );
        assert_eq!(split_discard_bind("no arrow here"), None);
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
            type_display: Some("Text -> Text".into()),
            stale: Vec::new(),
        };
        let r = slim_item_result(&defined);
        assert_eq!(r["decl"], "slug");
        assert_eq!(r["type"], "Text -> Text", "inferred type painted (#317)");
        assert!(r.get("stale").is_none(), "no stale key when nothing stale");
        assert!(r.get("generation").is_none(), "no generation in slim decl");
        assert!(r.get("module").is_none(), "no module in slim decl");

        // A non-value decl (or a probe failure) carries no `type` field.
        let defined_no_type = TurnOutcome::Defined {
            generation: 1,
            module: "Tidepool.Session.Lib.G1".into(),
            head: "Node".into(),
            type_display: None,
            stale: Vec::new(),
        };
        let r = slim_item_result(&defined_no_type);
        assert_eq!(r["decl"], "Node");
        assert!(r.get("type").is_none(), "no type key for a non-value decl");
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

#[cfg(test)]
mod reset_tests {
    use super::{
        Generation, MetaCommand, ModuleEnv, PureBind, Session, SessionConfig, SessionId,
        TurnOutcome, DEFAULT_NURSERY_SIZE,
    };

    fn minimal_config(root: std::path::PathBuf) -> SessionConfig {
        SessionConfig {
            id: SessionId(1),
            root,
            base_include: Vec::new(),
            decls: Vec::new(),
            preamble: String::new(),
            effect_stack: String::new(),
            ask_tag: 0,
            module_env: ModuleEnv::standalone_default(),
            nursery_size: DEFAULT_NURSERY_SIZE,
        }
    }

    /// F4(a): a failed `SessionLib::open` inside `:reset` must leave the
    /// session's state COMPLETELY untouched — not a half-reset where the value
    /// plane / turn counter were already cleared before the reopen was even
    /// attempted. `SessionLib::open` only does `fs::create_dir_all`, so this
    /// forces a real IO failure with no GHC/extract dependency.
    #[test]
    fn reset_leaves_state_untouched_when_reopen_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = Session::open(minimal_config(dir.path().to_path_buf()))
            .expect("session opens on a fresh dir");

        // Poke markers into fields the pre-fix "clear before open" code path
        // wiped unconditionally, even on a failed reopen.
        session.turn_counter = 7;
        session.val_gen = Generation(3);
        session.pure_binds.insert(
            "marker".to_string(),
            PureBind {
                type_display: "Int".to_string(),
                defining_expr: "42".to_string(),
                gen: Generation(1),
            },
        );

        // Sabotage `cfg.root` so the reopen inside `:reset` fails: replace the
        // existing directory with a plain file at the same path.
        std::fs::remove_dir_all(&session.cfg.root).expect("remove session root");
        std::fs::write(&session.cfg.root, b"not a directory").expect("write blocker file");

        let outcome = session.run_meta(&MetaCommand::Reset);
        assert!(
            matches!(outcome, TurnOutcome::Error(_)),
            "a failed reopen must surface as an error: {outcome:?}"
        );

        assert_eq!(
            session.turn_counter, 7,
            "a failed reopen must not clear turn_counter"
        );
        assert_eq!(
            session.val_gen,
            Generation(3),
            "a failed reopen must not clear val_gen"
        );
        assert!(
            session.pure_binds.contains_key("marker"),
            "a failed reopen must not clear pure_binds — the old decl log must stay usable"
        );
    }

    /// F4(b): an in-block `:reset` drops the resident machine WITHOUT going
    /// through `bootstrap_machine`'s `publish_cancel` call, so the shared
    /// `CancelSlot` must be cleared explicitly — otherwise it keeps the
    /// dropped machine's stale `CancelHandle` until the next bootstrap, and a
    /// server-side timeout in that window cancels a dead flag instead of a
    /// live one. Needs a real compiled machine (GHC extract), so this test
    /// mirrors `tests/common/mod.rs`'s minimal-stack setup at the bare
    /// `Session` level.
    #[test]
    fn reset_clears_stale_cancel_handle_from_slot() {
        if !tidepool_testing::eval_harness::extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }
        let stack = tidepool_handlers::build_minimal_stack();
        let (decls, ask_tag) = tidepool_handlers::base_decls_with_ask(&stack);
        let effects_dir =
            tidepool_mcp::ensure_effects_module(&decls).expect("write Tidepool.Effects module");
        let prelude_dir = tidepool_testing::eval_harness::prelude_path();
        let module_env = tidepool_mcp::session_decl_module_env(&decls, false);
        let preamble = crate::truncate::passthrough_paginate(
            &tidepool_mcp::build_preamble_non_interactive(&decls, false),
        );
        let effect_stack = tidepool_mcp::build_effect_stack_type(&decls);

        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = SessionConfig {
            id: SessionId(1),
            root: dir.path().to_path_buf(),
            base_include: vec![effects_dir, prelude_dir],
            decls,
            preamble,
            effect_stack,
            ask_tag,
            module_env,
            nursery_size: DEFAULT_NURSERY_SIZE,
        };
        let mut session = Session::open(cfg).expect("session opens");

        let slot = crate::worker::empty_cancel_slot();
        session.set_cancel_slot(slot.clone());
        session.ensure_effect_machine();
        assert!(
            session.machine.is_some(),
            "machine must bootstrap from a trivial `pure ()` fragment"
        );
        assert!(
            slot.lock().is_some(),
            "bootstrap must publish a cancel handle into the shared slot"
        );

        session.run_meta(&MetaCommand::Reset);

        assert!(
            slot.lock().is_none(),
            "in-block :reset must clear the stale cancel handle, not leave the dropped \
             machine's handle in the shared slot"
        );
    }
}
