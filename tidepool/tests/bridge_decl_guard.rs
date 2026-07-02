//! Decl↔bridge drift guard (friction #25): every `#[core(name = "X")]` in
//! tidepool-handlers must name something the effect decls actually declare.
//! The LspNode outage (fdc82ce renamed the Haskell constructor `Node` →
//! `LspNode`; the bridge attr kept `"Node"`) was invisible for a day because
//! nothing tied the two sources — this test is that tie.

#[test]
fn bridge_core_names_appear_in_effect_decls() {
    let handlers_src = include_str!("../../tidepool-handlers/src/lib.rs");
    let mut corpus = String::new();
    let mut decls = tidepool_mcp::standard_decls();
    decls.push(tidepool_mcp::meta_decl()); // --debug-gated, still bridged
    for decl in decls {
        for td in decl.type_defs {
            corpus.push_str(td);
            corpus.push('\n');
        }
        for c in decl.constructors {
            corpus.push_str(c);
            corpus.push('\n');
        }
    }
    // Also generic wire types declared outside effect decls (Proc etc. live in
    // records/type_defs already; the preamble's fixed decls cover the rest).
    corpus.push_str(&tidepool_mcp::build_preamble(&tidepool_mcp::standard_decls(), false));

    const NEEDLE: &str = "#[core(name = \"";
    let mut missing = Vec::new();
    let mut rest = handlers_src;
    while let Some(i) = rest.find(NEEDLE) {
        rest = &rest[i + NEEDLE.len()..];
        let name = &rest[..rest.find('"').expect("unterminated core(name attr")];
        if !corpus.contains(name) {
            missing.push(name.to_string());
        }
    }
    assert!(
        missing.is_empty(),
        "bridge #[core(name)] attrs with no matching declaration in the effect \
         decls / preamble (rename drift — the LspNode-outage class): {missing:?}"
    );
}
