//! `CoreRecord` derive: render the Haskell `data` declaration a Rust
//! wire-struct/enum mirrors, so the two can no longer drift.
//!
//! The whole decl is computed here at macro-expansion time (the Rust AST fully
//! determines it) and embedded as a string literal in the generated
//! `haskell_decl()`.

use crate::parse::{DataInfo, EnumInfo, StructInfo};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Ident, Type};

/// Map a Rust type to its Haskell rendering.
///
/// `String`/`&str`/`str`/`PathBuf`/`Text` → `Text`; the integer family → `Int`;
/// `bool` → `Bool`; `f32`/`f64` → `Double`; `Vec<T>` → `[<hs T>]`;
/// `Option<T>` → `Maybe (<hs T>)`; a tuple → `(<hs A>, <hs B>, ...)`; any other
/// named ident `X` → `X` (assumed to be a nested record whose Haskell name
/// matches — override with `#[core(hs_type = "...")]` when it differs).
fn hs_type(ty: &Type) -> Result<String, syn::Error> {
    match ty {
        Type::Path(tp) if tp.qself.is_none() => {
            let seg = tp
                .path
                .segments
                .last()
                .ok_or_else(|| syn::Error::new_spanned(ty, "empty type path"))?;
            let ident = seg.ident.to_string();
            // Generic container types.
            if let syn::PathArguments::AngleBracketed(ab) = &seg.arguments {
                let inner: Vec<&Type> = ab
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        syn::GenericArgument::Type(t) => Some(t),
                        _ => None,
                    })
                    .collect();
                match ident.as_str() {
                    "Vec" => {
                        let t = inner.first().ok_or_else(|| {
                            syn::Error::new_spanned(ty, "Vec without a type argument")
                        })?;
                        return Ok(format!("[{}]", hs_type(t)?));
                    }
                    "Option" => {
                        let t = inner.first().ok_or_else(|| {
                            syn::Error::new_spanned(ty, "Option without a type argument")
                        })?;
                        return Ok(format!("Maybe {}", paren_if_multi(&hs_type(t)?)));
                    }
                    _ => {
                        return Err(syn::Error::new_spanned(
                            ty,
                            format!("unsupported generic type `{ident}` for CoreRecord; add a #[core(hs_type = \"...\")] override"),
                        ));
                    }
                }
            }
            let mapped = match ident.as_str() {
                "String" | "str" | "Text" | "PathBuf" => "Text",
                "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize" => {
                    "Int"
                }
                "bool" => "Bool",
                "f32" | "f64" => "Double",
                // A named type: assume its Haskell name matches the Rust ident.
                other => other,
            };
            Ok(mapped.to_string())
        }
        // &str / &T
        Type::Reference(r) => hs_type(&r.elem),
        // Tuple: (A, B, ...) → (hsA, hsB, ...)
        Type::Tuple(t) => {
            let parts: Result<Vec<String>, _> = t.elems.iter().map(hs_type).collect();
            Ok(format!("({})", parts?.join(", ")))
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "unsupported type for CoreRecord; add a #[core(hs_type = \"...\")] override",
        )),
    }
}

/// Wrap a rendered type in parens when it is a multi-token application (so it
/// sits correctly as a single positional constructor argument or `Maybe`
/// operand). `[Text]` / `(A, B)` / `Text` are already atomic.
fn paren_if_multi(rendered: &str) -> String {
    let atomic = !rendered.contains(' ') || rendered.starts_with('[') || rendered.starts_with('(');
    if atomic {
        rendered.to_string()
    } else {
        format!("({rendered})")
    }
}

/// `exit_code` → `exitCode`; single words pass through unchanged.
fn snake_to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for ch in s.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Render the Haskell `data` decl for a struct record.
fn struct_decl(info: &StructInfo) -> Result<String, syn::Error> {
    let core = &info.core_name;
    let mut fields = Vec::with_capacity(info.fields.len());
    for f in &info.fields {
        let hs_name = f
            .hs_name
            .clone()
            .unwrap_or_else(|| snake_to_camel(&f.ident.to_string()));
        let hs_ty = match &f.hs_type {
            Some(t) => t.clone(),
            None => hs_type(&f.ty)?,
        };
        fields.push(format!("{hs_name} :: {hs_ty}"));
    }
    if fields.is_empty() {
        Ok(format!("data {core} = {core} deriving (Show, Eq)"))
    } else {
        Ok(format!(
            "data {core} = {core} {{ {} }} deriving (Show, Eq)",
            fields.join(", ")
        ))
    }
}

/// Render the Haskell `data` decl for an enum (positional variant fields,
/// matching the bridge's positional enum encoding).
fn enum_decl(info: &EnumInfo) -> Result<String, syn::Error> {
    let name = info.name.to_string();
    let mut variants = Vec::with_capacity(info.variants.len());
    for v in &info.variants {
        let mut parts = vec![v.core_name.clone()];
        for ty in &v.fields {
            parts.push(paren_if_multi(&hs_type(ty)?));
        }
        variants.push(parts.join(" "));
    }
    Ok(format!(
        "data {name} = {} deriving (Show, Eq)",
        variants.join(" | ")
    ))
}

/// Build the `impl CoreRecord` + inventory registration for a derive input.
pub fn generate_core_record(info: &DataInfo) -> Result<TokenStream, syn::Error> {
    let (rust_name, hs_name, decl): (&Ident, String, String) = match info {
        DataInfo::Struct(s) => (&s.name, s.core_name.clone(), struct_decl(s)?),
        DataInfo::Enum(e) => (&e.name, e.name.to_string(), enum_decl(e)?),
    };

    Ok(quote! {
        impl tidepool_bridge::CoreRecord for #rust_name {
            fn haskell_decl() -> String {
                #decl.to_string()
            }
        }

        inventory::submit! {
            tidepool_bridge::RegisteredRecord {
                name: #hs_name,
                decl: <#rust_name as tidepool_bridge::CoreRecord>::haskell_decl,
            }
        }
    })
}
