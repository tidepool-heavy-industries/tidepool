//! Reply-type constructibility audit for the prepared-STG host answer
//! builder, ahead of Shoal dogfooding the prepared engine (now the default).
//!
//! `PreparedEngine::lower_answer` (`tidepool-runtime/src/session/prepared.rs`)
//! refuses `TypeNode::Unconstructible` anywhere in a suspended effect's answer
//! path, except inside the `Tidepool.Aeson.Value.Value` family (lowered whole
//! through the decode root -- see decision 4 in
//! `plans/handoff/designs/synthetic-sites.md`). A refusal on a verb whose
//! reply the host cannot build means a suspended frame on that verb parks
//! forever: it can never resume. This test finds every such verb up front by
//! compiling ONE turn module that mentions every effect verb of the generated
//! surface (`tidepool_mcp::standard_decls()` -- the same decls
//! `ResidentSession`'s Notebook-shaped tests build their preamble/effect-stack
//! from), then walking the compiled prepared artifact's site rows
//! (`PreparedProgram::sites()`/`verb_sites()`/`type_node()`,
//! `tidepool-repr/src/execution_schema.rs`) for `TypeNode::Unconstructible`.
//!
//! Each verb is mentioned, never RUN: the compiled turn expression is `do {
//! _ <- verb1 arg; _ <- verb2 arg arg; ...; pure () }`, but this test only
//! ever calls `compile()`, never `run_with_sites` -- the artifact is built
//! and inspected, the action inside it never executes. Two things had to be
//! learned empirically to get real per-verb sites out of the compiled
//! artifact:
//!
//! - A bare, unapplied reference (`let _ = verb` or `let _ = (verb :: its
//!   own declared type)`) registers NO site at all. The synthetic-site
//!   projector interns a request GADT constructor from its actual `Con`
//!   occurrence in Core (`synthetic-sites.md` decision 1); a verb like
//!   `readFile = send . FsRead` only produces that occurrence once INLINED
//!   at an application site, so every verb is actually applied here (to
//!   type-directed dummy arguments parsed at test time out of
//!   `EffectDecl::helpers`'s literal Haskell source).
//! - A dead-branch guard (`if False then do {...} else pure ()`) ALSO
//!   registers no sites: GHC's case-of-known-constructor reduction on a
//!   literal `False` scrutinee eliminates the live branch, taking every
//!   verb application with it, before the extractor ever sees it. Relying
//!   instead on this test never calling `run_with_sites` is what actually
//!   keeps the applications both live in the compiled Core and never
//!   executed.
//!
//! Needs a resolvable `$TIDEPOOL_EXTRACT` and its Haskell worker (as
//! `prepared_turn.rs`/`prepared_residency.rs` do):
//! `just test-target tidepool-runtime session 'test(prepared_reply_types)'`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tidepool_repr::execution_schema::{PreparedProgram, TypeNode, TypeNodeId};
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, EngineKind, ModuleEnv, ResidentSession, SessionLib,
    TurnRequest, TurnResult, TurnTemplate,
};
use tidepool_testing::eval_harness;

/// The minimal parts of `prepared_residency.rs`'s own `Notebook` this file
/// needs: one resident session, its compile plumbing, and an expression-turn
/// compile that stops short of running anything. Copied rather than shared so
/// this file never has to touch `prepared_turn.rs`.
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
    fn new(engine: EngineKind) -> Self {
        eval_harness::require_extract();
        let decls = tidepool_mcp::standard_decls();
        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let effect_stack = tidepool_mcp::build_effect_stack_type(&decls);
        let mut include = eval_harness::effects_include().to_vec();
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
        let session = ResidentSession::unbootstrapped_on(
            engine,
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            include.clone(),
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
            turn_text: text,
            templates: &templates,
            include: &include,
            session_root: self.root.path(),
            inject_modules: &self.injected,
            gen: self.generation,
            verdict: None,
            target: None,
            prepared: self.session.prepared_turn_request(&retained),
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
            && name.chars().next().is_some_and(|c| c.is_lowercase() || c == '_')
        {
            return Some((name.to_string(), ty.to_string()));
        }
    }
    None
}

/// Strip a declared type's `forall ... .` and `(Member|Members) ... =>`
/// prefix, returning the plain arrow chain plus the `forall` variable names
/// (in declaration order) so a caller can tell whether the verb is
/// additionally polymorphic in its ANSWER type (a stray non-`effs` variable,
/// e.g. `RunLLMTurn`'s `forall a effs. ... -> Eff effs a`).
fn strip_prefix(ty: &str) -> (String, Vec<String>) {
    let mut s = ty.to_string();
    let mut forall_vars = Vec::new();
    if let Some(fpos) = s.find("forall ") {
        if let Some(dot_rel) = s[fpos..].find(". ") {
            let vars_part = &s[fpos + 7..fpos + dot_rel];
            forall_vars = vars_part.split_whitespace().map(str::to_string).collect();
            s = format!("{}{}", &s[..fpos], &s[fpos + dot_rel + 2..]);
        }
    }
    if let Some(arrow) = s.find("=> ") {
        s = s[arrow + 3..].to_string();
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

/// A type-directed dummy literal for one argument type, or `None` when this
/// test does not know how to construct one (reported as a dropped verb, per
/// the task's "minimal form that type-checks, note any verb you had to drop"
/// fallback).
fn dummy_value(ty: &str) -> Option<&'static str> {
    let t = ty.trim();
    match t {
        "Text" | "FilePath" => Some("\"x\""),
        "Int" => Some("0"),
        "Bool" => Some("False"),
        "Value" => Some("(object [])"),
        _ if t.starts_with("Maybe ") => Some("Nothing"),
        _ if t.starts_with('[') && t.ends_with(']') => Some("[]"),
        _ => None,
    }
}

/// One verb, split into its argument types and whether it is an effectful
/// verb at all (helpers also carry pure utility functions, e.g. `findTally`,
/// `renderInvocationExit` -- those have no request constructor and no site).
struct Verb {
    name: String,
    args: Vec<String>,
    /// Extra `forall` variables beyond `effs` -- the verb is polymorphic in
    /// its own answer type (`RunLLMTurn`'s family) and needs a `@Value` type
    /// application to pin it before it can be applied at all.
    answer_type_var: bool,
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
        answer_type_var: forall_vars.iter().any(|v| v != "effs"),
    })
}

/// Every `(verb name, declared type)` pair this test can extract from
/// `tidepool_mcp::standard_decls()`'s helper text, plus the names it could
/// not parse a signature out of (reported, not silently dropped).
fn verb_signatures() -> (Vec<(String, String)>, Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut sigs = Vec::new();
    let mut unparsed = Vec::new();
    for decl in tidepool_mcp::standard_decls() {
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
/// family: `lower_answer` lowers that family whole through the decode root
/// (`__decodeValue`), so a `Value`-carrying reply is constructible even though
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
            let is_value =
                family.module == "Tidepool.Aeson.Value" && family.occurrence == "Value";
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
        Some(TypeNode::Text | TypeNode::Integer | TypeNode::Natural | TypeNode::Scalar(_)) | None => {}
    }
}

/// Explicit allow-list: the verbs whose reply type the prepared host answer
/// builder currently cannot construct. A future change to the generated
/// surface or to `TypePolicy.hs`/`lower_answer`'s constructibility policy
/// must show up as a diff here, not as a frame that silently never resumes.
const EXPECTED_UNCONSTRUCTIBLE: &[&str] = &[];

#[test]
fn prepared_reply_types_are_constructible() {
    let (verbs, unparsed) = verb_signatures();
    assert!(
        !verbs.is_empty(),
        "expected at least one verb signature out of tidepool_mcp::standard_decls()"
    );
    if !unparsed.is_empty() {
        eprintln!(
            "note: {} helper string(s) carried no parseable `name :: type` line and were \
             dropped: {unparsed:?}",
            unparsed.len()
        );
    }

    let mut notebook = Notebook::new(EngineKind::Prepared);
    let mut dropped = Vec::new();
    let mut statements = Vec::new();
    for (name, ty) in &verbs {
        let Some(verb) = parse_verb(name, ty) else {
            continue; // a pure helper (e.g. `findTally`), not an effect verb
        };
        let mut call_parts = vec![verb.name.clone()];
        if verb.answer_type_var {
            call_parts.push("@Value".to_string());
        }
        let mut ok = true;
        for arg_ty in &verb.args {
            match dummy_value(arg_ty) {
                Some(lit) => call_parts.push(lit.to_string()),
                None => {
                    ok = false;
                    dropped.push(format!("{name} :: {ty} (no dummy value for arg `{arg_ty}`)"));
                    break;
                }
            }
        }
        if ok {
            statements.push(format!("_ <- {}", call_parts.join(" ")));
        }
    }
    if !dropped.is_empty() {
        eprintln!(
            "note: {} verb(s) dropped -- could not build a well-typed dummy call: {dropped:#?}",
            dropped.len()
        );
    }
    assert!(
        !statements.is_empty(),
        "no effect verb could be applied; nothing to check"
    );
    // No dead-branch guard (an earlier `if False then do {...} else pure ()`
    // version of this test found nothing beyond one stray, unrelated site):
    // GHC's case-of-known-constructor reduction eliminates a literal `False`
    // scrutinee's branch before the extractor ever sees it, taking every
    // verb application with it. This test only ever calls `compile()`, never
    // `run_with_sites` -- the whole `do` block below is compiled into the
    // prepared artifact and inspected there, but genuinely never executed,
    // so "each verb is mentioned, never run" holds without a dead branch.
    let turn_text = format!("do {{ {} ; pure () }}", statements.join(" ; "));

    let TurnResult::Expr { compiled, .. } = notebook.compile(&turn_text) else {
        panic!("the verb-surface turn did not classify as an expression");
    };
    let prepared = compiled
        .prepared
        .as_ref()
        .expect("prepared request returned no prepared program");

    // Verb name -> the site row that answers it, via the synthetic
    // `verb_sites` side table (`ConstructorId` indexes `constructors()`
    // directly -- confirmed by `execution_schema/validation.rs`'s own
    // `constructor()` lookup).
    //
    // `verb_sites` decision 1 (`synthetic-sites.md`) deliberately casts a
    // WIDE net -- "an extra row for a non-effect GADT costs one type-graph
    // node ... breadth is the safer error" -- and in practice does pick up
    // at least one non-effect constructor from this turn's own machinery
    // (`GHC.Internal.Data.Typeable.Internal.TrType`, from the `@Value` type
    // application `RunLLMTurn`'s helpers need). Only the generated effects
    // module's own constructors are this test's subject.
    const EFFECTS_MODULE: &str = "Tidepool.Effects.Core";
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
    if !ignored_noise.is_empty() {
        eprintln!(
            "note: ignored {} verb_sites entry(ies) outside `{EFFECTS_MODULE}` (the projector's \
             deliberately broad net, `synthetic-sites.md` decision 1, picking up incidental \
             non-effect constructors this turn's own machinery touches): {ignored_noise:?}",
            ignored_noise.len()
        );
    }

    println!(
        "\n{:<40} {:<24} {}",
        "VERB (request constructor)", "constructible?", "reason"
    );
    println!("{}", "-".repeat(100));

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
        "the compiled artifact's verb_sites table is empty -- no synthetic site was minted for \
         any of the {} mentioned verbs; the turn module's mentions may not be reaching the \
         extractor's constructor-interning pass",
        verbs.len()
    );

    let expected: BTreeSet<String> = EXPECTED_UNCONSTRUCTIBLE
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        unconstructible, expected,
        "the set of unconstructible verbs changed -- update EXPECTED_UNCONSTRUCTIBLE (and tell \
         Shoal) if this is an intended surface/policy change; found: {unconstructible:#?}"
    );
}
