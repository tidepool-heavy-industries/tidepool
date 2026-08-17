//! Generator: the `EffectDecl` builder, emitted into `tidepool-mcp`.
//!
//! Replaces what `effect_decl_projection!` expands today. The VALUE it builds
//! must be identical field for field — the assembled `Tidepool.Effects` source
//! is content-addressed as a compile-cache key, so a changed byte here
//! invalidates every cached compile for every user.

use super::{header, index_body, module_name, rust_string_literal, GeneratedFile};
use crate::schema::Effect;

/// Where this effect's decl module lives, relative to the workspace root.
#[must_use]
pub fn path(e: &Effect) -> String {
    format!("tidepool-mcp/src/generated/{}.rs", module_name(e))
}

/// The `mod`-index for the generated decl modules.
#[must_use]
pub fn module_index(effects: &[Effect]) -> GeneratedFile {
    GeneratedFile {
        path: "tidepool-mcp/src/generated/mod.rs".to_string(),
        contents: index_body("Generated effect declarations", effects),
    }
}

/// The whole generated decl file for one effect.
#[must_use]
pub fn file(e: &Effect) -> GeneratedFile {
    GeneratedFile {
        path: path(e),
        contents: body(e),
    }
}

fn slice_literal(items: &[String], indent: &str) -> String {
    if items.is_empty() {
        return "&[]".to_string();
    }
    let mut out = String::from("&[\n");
    for i in items {
        out.push_str(indent);
        out.push_str("    ");
        out.push_str(&rust_string_literal(i));
        out.push_str(",\n");
    }
    out.push_str(indent);
    out.push(']');
    out
}

fn body(e: &Effect) -> String {
    let mut out = header("//! ", &format!("`{}` effect declaration", e.name));
    out.push('\n');

    out.push_str(&format!(
        "/// The `{}` effect's static Haskell-side metadata.\n",
        e.name
    ));
    out.push_str("#[must_use]\n");
    out.push_str(&format!("pub fn {}() -> crate::EffectDecl {{\n", e.decl_fn));
    out.push_str("    crate::EffectDecl {\n");
    out.push_str(&format!(
        "        type_name: {},\n",
        rust_string_literal(e.name)
    ));
    out.push_str(&format!(
        "        description: {},\n",
        rust_string_literal(&e.description_text())
    ));
    match e.prompt_card_text() {
        Some(pc) => out.push_str(&format!(
            "        prompt_card: Some({}),\n",
            rust_string_literal(&pc)
        )),
        None => out.push_str("        prompt_card: None,\n"),
    }
    out.push_str(&format!(
        "        constructors: {},\n",
        slice_literal(&e.constructor_signatures(), "        ")
    ));
    out.push_str(&format!(
        "        type_defs: {},\n",
        slice_literal(&e.type_def_texts(), "        ")
    ));
    let imports: Vec<String> = e.extra_imports.iter().map(|s| (*s).to_string()).collect();
    out.push_str(&format!(
        "        extra_imports: {},\n",
        slice_literal(&imports, "        ")
    ));
    out.push_str(&format!(
        "        helpers: {},\n",
        slice_literal(&e.helper_texts(), "        ")
    ));
    let tps: Vec<String> = e.type_params.iter().map(|s| (*s).to_string()).collect();
    out.push_str(&format!(
        "        type_params: {},\n",
        slice_literal(&tps, "        ")
    ));
    let dra: Vec<String> = e
        .default_row_args
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    out.push_str(&format!(
        "        default_row_args: {},\n",
        slice_literal(&dra, "        ")
    ));
    out.push_str(&format!(
        "        helpers_row_polymorphic: {},\n",
        e.helpers_row_polymorphic
    ));
    out.push_str("    }\n");
    out.push_str("}\n");
    out
}
