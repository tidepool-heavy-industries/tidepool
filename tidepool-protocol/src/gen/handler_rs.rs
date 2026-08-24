//! Generator: the Rust request enum, error ADT, and dispatch glue, emitted
//! into `tidepool-handlers`.
//!
//! Replaces what `effect_rust_projection!` / `error_enum!` / `dispatch_body!`
//! expand today. What stays hand-written in the effect's own module is
//! unchanged: the handler struct (its fields are configuration, not contract)
//! and one inherent method per verb.
//!
//! The two method shapes are the reason `dispatch_body!` has two arms, and the
//! reason they differ is real. An errors-tagged verb's method returns
//! `Result<T, <Err>>` and takes NO `cx`, so the arm wraps it with `cx.respond`
//! (`Ok`→`Right`, `Err`→`Left`) and the handler is total by construction. A
//! plain verb's method takes `cx` and returns `Result<Response, EffectError>`,
//! and the arm forwards it.

use super::{header, index_body, module_name, snake_case, GeneratedFile};
use crate::schema::Effect;

/// Where this effect's generated glue lives, relative to the workspace root.
#[must_use]
pub fn path(e: &Effect) -> String {
    format!("tidepool-handlers/src/generated/{}.rs", module_name(e))
}

/// The `mod`-index for the generated glue modules.
///
/// Also lists the generated ADAPTER modules, because they land in the same
/// directory and a directory has exactly one index. Two generators cannot each
/// own `generated/mod.rs`, so the one that owns the effect's primary module owns
/// the index too.
///
/// The header text is deliberately UNCHANGED by that addition. It is the first
/// line of a committed file in another crate, and lane 3 flips nothing — an
/// effect with adapters gains a `pub mod <eff>_adapters;` line here when it is
/// flipped, and until then this file's bytes must not move.
#[must_use]
pub fn module_index(effects: &[Effect]) -> GeneratedFile {
    let mut modules: Vec<String> = effects.iter().map(module_name).collect();
    for e in effects {
        if super::adapter_rs::has_adapters(e) {
            modules.push(super::adapter_rs::module_name(e));
        }
    }
    GeneratedFile {
        path: "tidepool-handlers/src/generated/mod.rs".to_string(),
        contents: index_body(
            "Generated effect request types and dispatch glue",
            &modules,
            false,
        ),
    }
}

/// The whole generated glue file for one effect.
#[must_use]
pub fn file(e: &Effect) -> GeneratedFile {
    GeneratedFile {
        path: path(e),
        contents: body(e),
    }
}

fn body(e: &Effect) -> String {
    let mut out = header(
        "//! ",
        &format!("`{}` request types and dispatch glue", e.name),
    );
    out.push('\n');
    out.push_str(&format!(
        "use crate::handlers::{}::{};\n",
        e.handler_module, e.handler
    ));
    // Derive macros are imported by NAME, not spelled fully-qualified at each
    // attribute: the qualified form pushes `#[derive(..)]` past rustfmt's
    // `attr_fn_like_width` (70), and a generated `.rs` file has to be a fixed
    // point of `cargo fmt` or the format gate and the golden gate fight.
    if e.errors.is_some() {
        out.push_str("use tidepool_bridge_derive::{FromCore, ToCore};\n\n");
    } else {
        out.push_str("use tidepool_bridge_derive::FromCore;\n\n");
    }

    // --- the typed failure ADT -------------------------------------------
    if let Some(adt) = &e.errors {
        out.push_str(&format!(
            "/// The `{}` effect's typed per-verb failure (#335).\n",
            e.name
        ));
        out.push_str("///\n");
        out.push_str(
            "/// `FromCore` is for test-side decoding of a `Left err`; the error is only\n",
        );
        out.push_str("/// ever SENT (`ToCore`) in production. `Debug` backs the `Display` path.\n");
        out.push_str("#[derive(ToCore, FromCore, Debug, PartialEq, Eq)]\n");
        out.push_str(&format!("pub enum {} {{\n", adt.name));
        for v in &adt.variants {
            out.push_str(&format!("    /// {}\n", v.doc));
            let fields: Vec<String> = v
                .fields
                .iter()
                .map(|f| {
                    f.rust
                        .rust_type(&f.ty, &format!("{}::{}::{}", e.name, adt.name, v.ctor))
                })
                .collect();
            out.push_str(&super::render_variant(v.ctor, &fields));
        }
        out.push_str("}\n\n");
    }

    // --- the request enum -------------------------------------------------
    out.push_str(&format!(
        "/// One variant per `{}` GADT constructor, named EXACTLY as in Haskell.\n",
        e.name
    ));
    out.push_str("#[derive(FromCore)]\n");
    out.push_str(&format!("pub enum {} {{\n", e.req_enum));
    for v in &e.verbs {
        let tys: Vec<String> = v
            .args
            .iter()
            .map(|a| {
                a.rust
                    .rust_type(&a.ty, &format!("{}::{}::{}", e.name, v.ctor, a.name))
            })
            .collect();
        out.push_str(&super::render_variant(v.ctor, &tys));
    }
    out.push_str("}\n\n");

    // --- DescribeEffect ---------------------------------------------------
    out.push_str(&format!(
        "impl tidepool_mcp::DescribeEffect for {} {{\n",
        e.handler
    ));
    out.push_str("    fn effect_decl() -> tidepool_mcp::EffectDecl {\n");
    out.push_str(&format!("        tidepool_mcp::{}()\n", e.decl_fn));
    out.push_str("    }\n}\n\n");

    // --- the dispatch -----------------------------------------------------
    out.push_str(&format!(
        "impl tidepool_effect::dispatch::EffectHandler<tidepool_mcp::CapturedOutput> for {} {{\n",
        e.handler
    ));
    out.push_str(&format!("    type Request = {};\n\n", e.req_enum));
    out.push_str("    fn handle(\n");
    out.push_str("        &mut self,\n");
    out.push_str(&format!("        req: {},\n", e.req_enum));
    out.push_str(
        "        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,\n",
    );
    out.push_str(
        "    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {\n",
    );
    out.push_str("        match req {\n");
    for v in &e.verbs {
        // The dispatch arm's binders are LOCAL variable names, not contract:
        // the Haskell argument name (`treeId`) is the contract, the Rust binder
        // is snake_case because Rust's own lint says so. The macro this
        // generator replaces bound the Haskell spelling verbatim and got away
        // with it only because an EXTERNAL macro's expansion is exempt from
        // `non_snake_case`; committed source is not, and `-D warnings` is in
        // the verify list. Effects whose argument names are already snake_case
        // (Exec, Journal) render byte-identically either way.
        let names: Vec<String> = v.args.iter().map(|a| snake_case(a.name)).collect();
        let pat = if names.is_empty() {
            format!("{}::{}", e.req_enum, v.ctor)
        } else {
            format!("{}::{}({})", e.req_enum, v.ctor, names.join(", "))
        };
        // An errors-tagged method takes no `cx` and is total in the error ADT;
        // a plain method takes `cx` and returns the Response itself.
        let call = if v.errors.is_some() {
            format!("cx.respond(self.{}({}))", v.method, names.join(", "))
        } else {
            let mut a = vec!["cx".to_string()];
            a.extend(names.iter().cloned());
            format!("self.{}({})", v.method, a.join(", "))
        };
        // rustfmt wraps a match arm into block form once the single-line
        // rendering would exceed its default 100-column width — `RepoEvent`'s
        // longer verb/method names are the first migrated effect to cross
        // that threshold (Exec/Journal/Worktree never did), so the emitter
        // must reproduce the same wrapping rustfmt would apply, or the format
        // gate and the golden gate fight (this generator's stated acceptance
        // property — see the module doc).
        let single_line = format!("            {pat} => {call},");
        if single_line.chars().count() <= 100 {
            out.push_str(&single_line);
            out.push('\n');
        } else {
            out.push_str(&format!(
                "            {pat} => {{\n                {call}\n            }}\n"
            ));
        }
    }
    out.push_str("        }\n");
    out.push_str("    }\n}\n");
    out
}
