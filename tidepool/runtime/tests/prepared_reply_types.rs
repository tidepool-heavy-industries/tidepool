//! Reply-type constructibility audit for the prepared-STG host answer
//! builder, ahead of Exomonad dogfooding the prepared engine (now the default).
//!
//! `PreparedEngine::lower_answer` (`tidepool/runtime/src/session/prepared.rs`)
//! refuses `TypeNode::Unconstructible` anywhere in a suspended effect's answer
//! path, except inside the `Tidepool.Aeson.Value.Value` family (constructed by
//! the authenticated structural visitor). A refusal on a verb whose
//! reply the host cannot build means a suspended frame on that verb parks
//! forever: it can never resume. This test finds every such verb up front by
//! compiling ONE turn module that mentions every effect verb of a generated
//! surface, then walking the compiled prepared artifact's site rows
//! (`PreparedProgram::sites()`/`verb_sites()`/`type_node()`,
//! `tidepool/repr/src/execution_schema.rs`) for `TypeNode::Unconstructible`.
//! It runs this audit twice, over two different surfaces:
//!
//! - `tidepool_mcp::standard_decls()` -- the ordinary MCP eval/session
//!   surface (`ResidentSession`'s own Notebook-shaped tests build their
//!   preamble/effect-stack from this).
//! - `tidepool_mcp::all_decls()` -- the WIDER surface Exomonad's actor host
//!   actually compiles against (`exomonad/harness/src/engine.rs`'s
//!   `agent_decls()` is `standard_decls()` plus `Fork`/`Finalize`; `all_decls`
//!   is that plus every other schema-owned effect --
//!   `bridge/mcp/src/generated/mod.rs`'s `schema_decls()`: `Journal`,
//!   `Worktree`, repo/agent events, `Actor*`, `Sleep`, `Green`, and more).
//!
//! Each verb is mentioned, never RUN: the compiled turn expression is `do {
//! _ <- verb1 arg; _ <- verb2 arg arg; ...; pure () }`, but this test only
//! ever calls `compile()`, never `run_with_sites` -- the artifact is built
//! and inspected, the action inside it never executes. Three things had to be
//! learned empirically to get real per-verb sites out of the compiled
//! artifact:
//!
//! - A bare, unapplied reference (`let _ = verb` or `let _ = (verb :: its
//!   own declared type)`) registers NO site at all. The synthetic-site
//!   projector interns a request GADT constructor from its actual `Con`
//!   occurrence in Core (by design); a verb like
//!   `readFile = send . FsRead` only produces that occurrence once INLINED
//!   at an application site, so every verb is actually applied here.
//! - A dead-branch guard (`if False then do {...} else pure ()`) ALSO
//!   registers no sites: GHC's case-of-known-constructor reduction on a
//!   literal `False` scrutinee eliminates the live branch, taking every
//!   verb application with it, before the extractor ever sees it. Relying
//!   instead on this test never calling `run_with_sites` is what actually
//!   keeps the applications both live in the compiled Core and never
//!   executed.
//! - Every argument is `(error "prepared_reply_types dummy")`, not a
//!   type-directed literal: `error :: forall a. HasCallStack => String -> a`
//!   type-checks at ANY concrete argument type -- records, bridged types,
//!   whatever a wider surface's verbs demand -- without this test needing a
//!   type-directed dummy-value table at all. Since nothing here ever runs,
//!   the bottom is never forced.
//!
//! Needs a resolvable `$TIDEPOOL_EXTRACT` and its Haskell worker (as
//! `prepared_turn.rs`/`prepared_residency.rs` do):
//! `just test-target tidepool-runtime session 'test(prepared_reply_types)'`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tidepool_mcp::EffectDecl;
use tidepool_repr::execution_schema::{PreparedProgram, TypeNode, TypeNodeId};
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, ModuleEnv, ResidentSession, SessionLib, TurnRequest,
    TurnResult, TurnTemplate,
};
use tidepool_testing::eval_harness;

/// The minimal parts of `prepared_residency.rs`'s own `Notebook` this file
/// needs: one resident session, its compile plumbing, and an expression-turn
/// compile that stops short of running anything. Copied rather than shared so
/// this file never has to touch `prepared_turn.rs`. Parameterized over the
/// decl list (`standard_decls()` vs `all_decls()`), unlike the original.
struct Notebook {
    session: ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    preamble: String,
    effect_stack: String,
    include: Vec<PathBuf>,
    root: tempfile::TempDir,
    injected: Vec<String>,
    generation: u64,
}

impl Notebook {
    fn new(decls: &[EffectDecl]) -> Self {
        eval_harness::require_extract();
        let preamble = tidepool_mcp::build_preamble(decls, false);
        let effect_stack = tidepool_mcp::build_effect_stack_type(decls);
        // NOT `eval_harness::effects_include()`: that helper hardcodes
        // `standard_decls()`, materializing `Tidepool/Effects.hs` (the
        // per-window SHIM, which defines `type M = Eff '[<these decls>]`) for
        // the wrong, narrower row. `Tidepool.Effects.Core` (the OTHER half)
        // is genuinely universal regardless of which decls are passed
        // (`ensure_effects_module_at` always renders it from the full
        // `all_decls()` vocabulary) -- only the shim's `M` is decls-scoped,
        // and it must match THIS test's own `decls`, or a verb outside
        // `standard_decls()` (e.g. `RepoEvent`'s `awaitSubscriptionRaw`)
        // resolves fine (Core has its GADT universally) but its `Member`
        // constraint fails against the stale, narrower `M` row -- confirmed
        // empirically: exactly this mismatch, naming exactly the
        // `standard_decls()` row, on the first `all_decls()` attempt here.
        let mut include = tidepool_mcp::ensure_effects_module(decls)
            .expect("write Tidepool.Effects module for this test's own decls")
            .include_paths()
            .to_vec();
        include.push(eval_harness::prelude_path());
        let root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            tidepool_repr::SessionId(1),
            root.path().join("decl-lib"),
            ModuleEnv::standalone_default(),
        )
        .expect("open decl plane")
        .with_validation_include(vec![eval_harness::prelude_path()]);
        include.push(lib.include_dir().to_path_buf());
        let session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        Self {
            session,
            preamble,
            effect_stack,
            include,
            root,
            injected: Vec::new(),
            generation: 0,
        }
    }

    fn templates(&self) -> Vec<TurnTemplate> {
        resident_workbench_templates(
            &self.preamble,
            &self.effect_stack,
            &self.injected.join("\n"),
        )
    }

    fn compile(&mut self, text: &str) -> TurnResult {
        self.generation += 1;
        let retained = self.session.prepared_retained();
        let templates = self.templates();
        let include: Vec<&Path> = self.include.iter().map(PathBuf::as_path).collect();
        run_turn(TurnRequest {
            session_id: None,
            turn_text: text,
            templates: &templates,
            include: &include,
            session_root: self.root.path(),
            inject_modules: &self.injected,
            gen: self.generation,
            verdict: None,
            target: None,
            retained_imports: &retained,
        })
        .unwrap_or_else(|failure| {
            panic!(
                "turn module failed to compile: {}\n{}",
                tidepool_runtime::classify_compile(&failure.error).message,
                failure
                    .attempted_source
                    .as_deref()
                    .unwrap_or("<no attempted source>")
            )
        })
    }
}

/// Pull `(name, declared_type)` out of one `EffectDecl::helpers` entry: the
/// literal Haskell source of one verb, doc comment(s) then a `name :: type`
/// signature line then its body (see `effect_defs.rs`'s `helper_text!`). The
/// signature line is the first line that is not a `--` doc comment or `{-#
/// ... #-}` pragma and contains `" :: "` right after a bare identifier.
fn extract_verb_sig(helper: &str) -> Option<(String, String)> {
    for line in helper.split('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("--") || trimmed.starts_with("{-#") {
            continue;
        }
        let idx = line.find(" :: ")?;
        let name = line[..idx].trim();
        let ty = line[idx + 4..].trim();
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '\'')
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_lowercase() || c == '_')
        {
            return Some((name.to_string(), ty.to_string()));
        }
    }
    None
}

/// Strip a declared type's `forall ... .` and constraint (`Member`/`Members`
/// / a tuple context) `... =>` prefix, repeatedly (some declarations stack
/// more than one), returning the plain arrow chain plus every `forall`
/// variable name seen (in declaration order) so a caller can tell whether the
/// verb is additionally polymorphic in its ANSWER type (a stray non-`effs`
/// variable, e.g. `RunLLMTurn`'s `forall a effs. ... -> Eff effs a`, or
/// `Finalize`'s `forall v a effs. ...`).
fn strip_prefix(ty: &str) -> (String, Vec<String>) {
    let mut s = ty.to_string();
    let mut forall_vars = Vec::new();
    loop {
        let mut changed = false;
        if let Some(fpos) = s.find("forall ") {
            if let Some(dot_rel) = s[fpos..].find(". ") {
                let vars_part = &s[fpos + 7..fpos + dot_rel];
                forall_vars.extend(vars_part.split_whitespace().map(str::to_string));
                s = format!("{}{}", &s[..fpos], &s[fpos + dot_rel + 2..]);
                changed = true;
            }
        }
        if let Some(arrow) = s.find("=> ") {
            s = s[arrow + 3..].to_string();
            changed = true;
        }
        if !changed {
            break;
        }
    }
    (s, forall_vars)
}

/// Split a type's top-level arrow chain (`A -> B -> C`) into `[A, B, C]`,
/// respecting `(...)`/`[...]` nesting so a compound argument like `[(Text,
/// Bool)]` or `Maybe Value` is never itself split.
fn split_arrows(s: &str) -> Vec<String> {
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        if depth == 0 && c == '-' && chars.get(i + 1) == Some(&'>') {
            parts.push(cur.trim().to_string());
            cur.clear();
            i += 2;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    parts.push(cur.trim().to_string());
    parts
}

/// A universal dummy argument, valid at any type: `error` type-checks against
/// any concrete argument type (records, bridged FFI types, whatever a wider
/// surface's verbs demand) since nothing this test compiles is ever run.
const DUMMY_ARG: &str = "(error \"prepared_reply_types dummy\")";

/// One verb, split into its argument types and whether it is an effectful
/// verb at all (helpers also carry pure utility functions, e.g. `findTally`,
/// `renderInvocationExit` -- those have no request constructor and no site).
struct Verb {
    name: String,
    args: Vec<String>,
    /// Extra `forall` variables beyond `effs`, in declaration order -- the
    /// verb is polymorphic in its own answer type (`RunLLMTurn`/`Fork`'s
    /// family: `forall a effs. ...`) and needs a type application to pin it
    /// before it can be applied at all.
    extra_forall_vars: Vec<String>,
}

fn parse_verb(name: &str, ty: &str) -> Option<Verb> {
    if !ty.contains("Eff effs") {
        return None; // a pure helper, not an effect verb
    }
    let (stripped, forall_vars) = strip_prefix(ty);
    let mut parts = split_arrows(&stripped);
    parts.pop(); // drop the `Eff effs R` tail
    Some(Verb {
        name: name.to_string(),
        args: parts,
        extra_forall_vars: forall_vars.into_iter().filter(|v| v != "effs").collect(),
    })
}

/// Verbs skipped outright, with why -- not "dropped" for lack of a dummy
/// value (that no longer happens: `DUMMY_ARG` is universal), but because no
/// well-typed call could apply them AT ALL, or because a site could never be
/// minted for them regardless of how they are called.
///
/// `finalize`/`finalizeSited` (`Finalize`, `type_params ["v"]`): its answer
/// index is the request GADT's own last argument `a` in `FinalizeWith :: Int
/// -> v -> Finalize v a`, which is skipped as open by design -- no site is
/// EVER minted for it, so
/// calling it (which would also need `v` pinned to the row's `Finalize Void`
/// default via a `@Void` application before `a` could be pinned by a second
/// one -- `v` precedes `a` in its `forall`) buys this audit nothing.
const SKIPPED_VERBS: &[(&str, &str)] = &[
    (
        "finalize",
        "Finalize's reply index (`a` in `Finalize v a`) is open, not closed -- \
         skipped as open by design; no site is ever minted",
    ),
    (
        "finalizeSited",
        "same as `finalize` -- its reply index is open, no site is ever minted",
    ),
];

/// Every `(verb name, declared type)` pair this test can extract from
/// `decls`'s helper text, plus the names it could not parse a signature out
/// of (reported, not silently dropped).
fn verb_signatures(decls: &[EffectDecl]) -> (Vec<(String, String)>, Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut sigs = Vec::new();
    let mut unparsed = Vec::new();
    for decl in decls {
        for helper in decl.helpers {
            match extract_verb_sig(helper) {
                Some((name, ty)) => {
                    if seen.insert(name.clone()) {
                        sigs.push((name, ty));
                    }
                }
                None => unparsed.push(format!("{}: {:?}", decl.type_name, helper)),
            }
        }
    }
    (sigs, unparsed)
}

/// Walk `wire`'s type graph and collect every `Unconstructible` node's reason,
/// EXCLUDING nodes reached only through the `Tidepool.Aeson.Value.Value`
/// family: the structural visitor owns that representation, so a
/// `Value`-carrying reply is constructible even though
/// `Value`'s `Object` row nests `Data.Map.Internal.Map` underneath.
fn unconstructible_reasons(program: &PreparedProgram, wire: TypeNodeId) -> Vec<String> {
    let mut reasons = Vec::new();
    let mut visited = BTreeSet::new();
    walk(program, wire, false, &mut visited, &mut reasons);
    reasons
}

fn walk(
    program: &PreparedProgram,
    id: TypeNodeId,
    inside_aeson_value: bool,
    visited: &mut BTreeSet<u32>,
    reasons: &mut Vec<String>,
) {
    if !visited.insert(id.0) {
        return;
    }
    match program.type_node(id) {
        Some(TypeNode::Data {
            family,
            arguments,
            rows,
        }) => {
            let is_value = family.module == "Tidepool.Aeson.Value" && family.occurrence == "Value";
            let inside = inside_aeson_value || is_value;
            for arg in arguments {
                walk(program, *arg, inside, visited, reasons);
            }
            for row in rows {
                for field in &row.fields {
                    walk(program, *field, inside, visited, reasons);
                }
            }
        }
        Some(TypeNode::Unconstructible { reason, .. }) => {
            if !inside_aeson_value {
                reasons.push(reason.clone());
            }
        }
        Some(TypeNode::Text | TypeNode::Integer | TypeNode::Natural | TypeNode::Scalar(_))
        | None => {}
    }
}

/// The generated effects module's fixed name, independent of which decls
/// populate it.
const EFFECTS_MODULE: &str = "Tidepool.Effects.Core";

/// Compile one turn module applying every effect verb `decls` exposes, then
/// return the set of request constructors (as `Module.Ctor` labels) whose
/// reply type is `Unconstructible` outside the `Value` family. Prints the
/// full verb table.
fn audit(surface_name: &str, decls: &[EffectDecl]) -> BTreeSet<String> {
    let (verbs, unparsed) = verb_signatures(decls);
    assert!(
        !verbs.is_empty(),
        "[{surface_name}] expected at least one verb signature"
    );
    if !unparsed.is_empty() {
        eprintln!(
            "[{surface_name}] note: {} helper string(s) carried no parseable `name :: type` \
             line and were dropped: {unparsed:?}",
            unparsed.len()
        );
    }

    let mut notebook = Notebook::new(decls);
    let mut skipped = Vec::new();
    let mut statements = Vec::new();
    for (name, ty) in &verbs {
        if let Some((_, reason)) = SKIPPED_VERBS.iter().find(|(n, _)| n == name) {
            skipped.push(format!("{name} :: {ty} ({reason})"));
            continue;
        }
        let Some(verb) = parse_verb(name, ty) else {
            continue; // a pure helper (e.g. `findTally`), not an effect verb
        };
        let mut call_parts = vec![verb.name.clone()];
        for var in &verb.extra_forall_vars {
            // `Finalize`-shaped verbs (more than one extra var) are handled
            // via `SKIPPED_VERBS` above, never reach here; every verb that
            // does has exactly one (`a`), pinned to `Value`.
            let _ = var;
            call_parts.push("@Value".to_string());
        }
        call_parts.extend(std::iter::repeat_n(DUMMY_ARG.to_string(), verb.args.len()));
        statements.push(format!("_ <- {}", call_parts.join(" ")));
    }
    if !skipped.is_empty() {
        eprintln!(
            "[{surface_name}] note: {} verb(s) skipped outright: {skipped:#?}",
            skipped.len()
        );
    }
    assert!(
        !statements.is_empty(),
        "[{surface_name}] no effect verb could be applied; nothing to check"
    );
    // No dead-branch guard: this test only ever calls `compile()`, never
    // `run_with_sites` -- the whole `do` block is compiled into the prepared
    // artifact and inspected there, but genuinely never executed, so "each
    // verb is mentioned, never run" holds without one (see module doc).
    let turn_text = format!("do {{ {} ; pure () }}", statements.join(" ; "));

    let TurnResult::Expr { compiled, .. } = notebook.compile(&turn_text) else {
        panic!("[{surface_name}] the verb-surface turn did not classify as an expression");
    };
    let prepared = &compiled.prepared;

    // Verb name -> the site row that answers it, via the synthetic
    // `verb_sites` side table (`ConstructorId` indexes `constructors()`
    // directly -- confirmed by `execution_schema/validation.rs`'s own
    // `constructor()` lookup).
    //
    // Only constructors defined in the generated effects module may have
    // synthetic host-answer rows. Every applied constructor from a declared
    // effect family must have one, even if its reply type is unconstructible.
    let constructors = prepared.constructors();
    let mut verb_site: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut ignored_noise = Vec::new();
    for (ctor_id, site) in prepared.verb_sites() {
        if let Some(decl) = constructors.get(ctor_id.0 as usize) {
            let label = format!("{}.{}", decl.identity.module, decl.identity.occurrence);
            if decl.identity.module == EFFECTS_MODULE {
                verb_site.insert(label, *site);
            } else {
                ignored_noise.push(label);
            }
        }
    }
    let effect_families: BTreeSet<&str> = decls
        .iter()
        .map(|decl| decl.type_name)
        .filter(|family| *family != "Finalize")
        .collect();
    let missing_sites: Vec<String> = constructors
        .iter()
        .filter(|decl| {
            decl.identity.module == EFFECTS_MODULE
                && decl.family.module == EFFECTS_MODULE
                && effect_families.contains(decl.family.occurrence.as_str())
        })
        .filter_map(|decl| {
            let label = format!("{}.{}", decl.identity.module, decl.identity.occurrence);
            (!verb_site.contains_key(&label)).then_some(label)
        })
        .collect();
    assert!(
        missing_sites.is_empty(),
        "[{surface_name}] generated effect constructors applied by this turn lack synthetic reply rows: {missing_sites:?}"
    );
    if !ignored_noise.is_empty() {
        eprintln!(
            "[{surface_name}] note: ignored {} verb_sites entry(ies) outside \
             `{EFFECTS_MODULE}`: {ignored_noise:?}",
            ignored_noise.len()
        );
    }

    println!(
        "\n[{surface_name}] {:<40} {:<24} reason",
        "VERB (request constructor)", "constructible?"
    );
    println!("{}", "-".repeat(110));

    let mut unconstructible = BTreeSet::new();
    let mut reported_any_site = false;
    for (label, site_id) in &verb_site {
        let Some(row) = prepared.site(*site_id) else {
            println!("{label:<40} {:<24} <no site row for verb_sites entry>", "?");
            continue;
        };
        reported_any_site = true;
        let reasons = unconstructible_reasons(prepared, row.wire);
        if reasons.is_empty() {
            println!("{label:<40} {:<24}", "constructible");
        } else {
            println!(
                "{label:<40} {:<24} {}",
                "UNCONSTRUCTIBLE",
                reasons.join(" | ")
            );
            unconstructible.insert(label.clone());
        }
    }
    assert!(
        reported_any_site,
        "[{surface_name}] the compiled artifact's verb_sites table is empty -- no synthetic \
         site was minted for any of the {} mentioned verbs; the turn module's mentions may not \
         be reaching the extractor's constructor-interning pass",
        verbs.len()
    );
    unconstructible
}

/// Explicit allow-list: the verbs whose reply type the prepared host answer
/// builder currently cannot construct, on the ordinary `standard_decls()`
/// surface. A future change to the generated surface or to
/// `TypePolicy.hs`/`lower_answer`'s constructibility policy must show up as a
/// diff here, not as a frame that silently never resumes.
const EXPECTED_UNCONSTRUCTIBLE: &[&str] = &[];

#[test]
fn prepared_reply_types_are_constructible() {
    let unconstructible = audit("standard_decls", &tidepool_mcp::standard_decls());
    let expected: BTreeSet<String> = EXPECTED_UNCONSTRUCTIBLE
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        unconstructible, expected,
        "[standard_decls] the set of unconstructible verbs changed -- update \
         EXPECTED_UNCONSTRUCTIBLE (and tell Exomonad) if this is an intended surface/policy \
         change; found: {unconstructible:#?}"
    );
}

/// Same allow-list, on `all_decls()` -- the wider surface Exomonad's actor host
/// actually compiles against (see the module doc).
const EXPECTED_UNCONSTRUCTIBLE_ALL: &[&str] = &[];

#[test]
fn prepared_reply_types_are_constructible_on_all_decls() {
    let unconstructible = audit("all_decls", &tidepool_mcp::all_decls());
    let expected: BTreeSet<String> = EXPECTED_UNCONSTRUCTIBLE_ALL
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        unconstructible, expected,
        "[all_decls] the set of unconstructible verbs changed -- update \
         EXPECTED_UNCONSTRUCTIBLE_ALL (and tell Exomonad) if this is an intended surface/policy \
         change; found: {unconstructible:#?}"
    );
}
