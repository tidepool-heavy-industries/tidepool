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
    let mut contents = index_body(
        "Generated effect declarations",
        &effects.iter().map(module_name).collect::<Vec<_>>(),
        true,
    );
    contents.push_str("\n/// Every schema-owned Haskell effect declaration.\n");
    contents.push_str("pub(crate) fn schema_decls() -> Vec<crate::EffectDecl> {\n");
    contents.push_str("    vec![\n");
    for effect in effects {
        contents.push_str(&format!("        {}(),\n", effect.decl_fn));
    }
    contents.push_str("    ]\n}\n");
    contents.push_str("\n/// Effects with an explicitly curated authored vocabulary.\n");
    let curated: Vec<_> = effects
        .iter()
        .filter(|effect| !matches!(effect.authored_surface, crate::schema::AuthoredSurface::All))
        .map(|effect| rust_string_literal(effect.name))
        .collect();
    let compact_curated = format!(
        "pub(crate) const CURATED_EFFECTS: &[&str] = &[{}];\n",
        curated.join(", ")
    );
    if compact_curated.len() <= 100 {
        contents.push_str(&compact_curated);
    } else {
        contents.push_str("pub(crate) const CURATED_EFFECTS: &[&str] = &[\n");
        for name in curated {
            contents.push_str(&format!("    {name},\n"));
        }
        contents.push_str("];\n");
    }
    contents.push_str("\n/// Hidden names grouped by their owning effect.\n");
    contents.push_str("pub(crate) const AUTHORED_HIDDEN_BY_EFFECT: &[(&str, &[&str])] = &[\n");
    for effect in effects
        .iter()
        .filter(|effect| !matches!(effect.authored_surface, crate::schema::AuthoredSurface::All))
    {
        let mut hidden = Vec::new();
        for type_def in &effect.type_defs {
            if !effect.authored_surface.includes_type_def(type_def.name) {
                hidden.push(type_def.name);
            }
        }
        for verb in &effect.verbs {
            if !effect.authored_surface.includes_verb(verb.ctor) {
                hidden.push(verb.ctor);
            }
        }
        for helper in &effect.helpers {
            if !effect.authored_surface.includes_helper(helper.name) {
                hidden.push(helper.name);
            }
        }
        if hidden.len() == 1 {
            contents.push_str(&format!(
                "    ({}, &[{}]),\n",
                rust_string_literal(effect.name),
                rust_string_literal(hidden[0])
            ));
        } else {
            contents.push_str("    (\n");
            contents.push_str(&format!("        {},\n", rust_string_literal(effect.name)));
            contents.push_str("        &[\n");
            for name in hidden {
                contents.push_str(&format!("            {},\n", rust_string_literal(name)));
            }
            contents.push_str("        ],\n");
            contents.push_str("    ),\n");
        }
    }
    contents.push_str("];\n");
    GeneratedFile {
        path: "tidepool-mcp/src/generated/mod.rs".to_string(),
        contents,
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
    let tps: Vec<String> = e
        .type_params
        .iter()
        .copied()
        .map(crate::schema::TypeParam::render)
        .collect();
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
