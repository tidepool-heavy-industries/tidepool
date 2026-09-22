use crate::parse::{EnumInfo, StructInfo, VariantShape};
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
use syn::{parse_quote, Type};

fn collect_type_params(ty: &Type, params: &HashSet<syn::Ident>, used: &mut HashSet<syn::Ident>) {
    match ty {
        Type::Path(tp) if tp.qself.is_none() => {
            // If it's PhantomData<T>, we DON'T consider T "used" for the purpose
            // of adding FromHaskell/ToHaskell bounds, because our PhantomData impl
            // doesn't require T to be FromHaskell/ToHaskell.
            if tp
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident == "PhantomData")
            {
                return;
            }
            if let Some(ident) = tp.path.get_ident() {
                if params.contains(ident) {
                    used.insert(ident.clone());
                }
            }
            for segment in &tp.path.segments {
                if let syn::PathArguments::AngleBracketed(ab) = &segment.arguments {
                    for arg in &ab.args {
                        if let syn::GenericArgument::Type(inner_ty) = arg {
                            collect_type_params(inner_ty, params, used);
                        }
                    }
                }
            }
        }
        Type::Tuple(tt) => {
            for elem in &tt.elems {
                collect_type_params(elem, params, used);
            }
        }
        Type::Array(ta) => {
            collect_type_params(&ta.elem, params, used);
        }
        _ => {}
    }
}

/// A field is a phantom (`std::marker::PhantomData<_>`) if its outermost path
/// segment is `PhantomData`. Such fields have no Haskell representation and are
/// skipped when computing a variant/struct's Haskell arity and when encoding or
/// decoding fields — analogous to how they are skipped for trait-bound
/// inference in `collect_type_params`.
fn is_phantom_data(ty: &Type) -> bool {
    if let Type::Path(tp) = ty {
        if tp.qself.is_none() {
            return tp
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident == "PhantomData");
        }
    }
    false
}

/// Emit the DataCon lookup expression for a derive site. When `module` is
/// `Some`, the lookup uses `DataConTable::get_by_qualified_name` (full
/// `Module.Constructor` path) and the error variant carries the qualified
/// name. When `module` is `None`, it falls back to the existing
/// name+arity lookup for backward compatibility. The lookup returns a
/// `Result<DataConId, BridgeError>`; arity validation still happens at the
/// call site via the existing field-count check so mismatched arities are
/// surfaced as `ArityMismatch` (qualified-name lookup does not pre-filter by
/// arity).
///
/// `question_mark` selects whether the emitted expression ends in `?`
/// (unwrap-or-propagate — the right call for a single-shape decode/encode
/// site where a missing constructor IS the whole failure) or is left as a
/// bare `Result` for the caller to match on (the enum `FromHaskell` per-variant
/// site: a missing constructor there means "this variant can't match, try
/// the next one", not "abort" — see `generate_from_haskell`, #F7). Keeping one
/// shared emitter (instead of a second copy without the `?`) is what keeps
/// the qualified/unqualified resolution logic itself single-sourced.
fn emit_datacon_lookup(
    module: Option<&String>,
    haskell_name: &str,
    haskell_arity_u32: u32,
    haskell_arity_usize: usize,
    question_mark: bool,
) -> TokenStream {
    let suffix = if question_mark {
        quote! { ? }
    } else {
        quote! {}
    };
    if let Some(module) = module {
        let qualified = format!("{}.{}", module, haskell_name);
        quote! {
            table.get_by_qualified_name(#qualified)
                .ok_or_else(|| tidepool_bridge::BridgeError::UnknownDataConQualified {
                    qualified_name: #qualified.to_string(),
                })#suffix
        }
    } else {
        // Silence unused-var warnings in the `Some` branch where arity isn't
        // consumed by the emitted code.
        let _ = haskell_arity_usize;
        // `get_by_name_arity_checked`, not the lenient `get_by_name_arity`:
        // two distinct constructors sharing this name+arity must be a loud,
        // candidate-naming error — insertion order must never silently
        // decide which one the derive resolves to.
        quote! {
            match table.get_by_name_arity_checked(#haskell_name, #haskell_arity_u32) {
                ::std::result::Result::Ok(::std::option::Option::Some(id)) => {
                    ::std::result::Result::Ok(id)
                }
                ::std::result::Result::Ok(::std::option::Option::None) => {
                    ::std::result::Result::Err(tidepool_bridge::BridgeError::UnknownDataConNameArity {
                        name: #haskell_name.to_string(),
                        arity: #haskell_arity_u32 as usize,
                    })
                }
                ::std::result::Result::Err(ambiguous) => {
                    ::std::result::Result::Err(tidepool_bridge::BridgeError::AmbiguousDataConNameArity {
                        name: ambiguous.name,
                        arity: ambiguous.arity as usize,
                        candidates: ambiguous.candidates,
                    })
                }
            }#suffix
        }
    }
}

/// Haskell arity of a constructor: how many of its Rust fields carry a Haskell
/// representation — every `PhantomData` field is excluded, since it consumes
/// no slot in the encoded `Con`. Shared by both directions (`FromHaskell`/
/// `ToHaskell`) and both shapes (enum variant/struct).
fn haskell_arity<'a>(field_types: impl Iterator<Item = &'a Type>) -> usize {
    field_types.filter(|ty| !is_phantom_data(ty)).count()
}

fn add_trait_bounds(
    generics: &mut syn::Generics,
    trait_path: &syn::Path,
    field_types: impl Iterator<Item = Type>,
) {
    let all_params: HashSet<_> = generics.type_params().map(|p| p.ident.clone()).collect();
    let mut used_params = HashSet::new();
    for field_ty in field_types {
        collect_type_params(&field_ty, &all_params, &mut used_params);
    }
    for param in &mut generics.params {
        if let syn::GenericParam::Type(type_param) = param {
            if used_params.contains(&type_param.ident) {
                type_param.bounds.push(parse_quote!(#trait_path));
            }
        }
    }
}

/// A field failure belongs to an already-matched constructor. Keep it distinct
/// from a top-level miss used by effect dispatch to try another request type.
fn emit_field_decode(ty: &Type, index: usize, constructor: &str) -> TokenStream {
    let field = index + 1;
    quote! {
        <#ty as tidepool_bridge::FromHaskell>::from_value(&fields[#index], table)
            .map_err(|source| tidepool_bridge::field_decode_error(
                #constructor, #field, &fields[#index], table, source,
            ))?
    }
}

pub fn generate_from_haskell(info: &EnumInfo) -> TokenStream {
    let name = &info.name;
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::FromHaskell);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.variants
            .iter()
            .flat_map(|variant| variant.shape.types().into_iter().cloned()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let mut match_arms = Vec::new();

    for variant in &info.variants {
        let rust_name = &variant.rust_name;
        let haskell_name = &variant.haskell_name;
        let haskell_module = variant.haskell_module.as_ref();
        let constructor_identity = haskell_module
            .map(|module| format!("{module}.{haskell_name}"))
            .unwrap_or_else(|| haskell_name.clone());
        let fields = variant.shape.types();
        let rust_arity = fields.len();

        let haskell_arity: usize = haskell_arity(fields.iter().copied());
        let haskell_arity_u32 = haskell_arity as u32;

        // Build per-Rust-field construction expressions. PhantomData fields
        // get a default `PhantomData` literal; other fields pull from the next
        // Haskell field slot.
        let mut haskell_ix: usize = 0;
        let mut field_exprs: Vec<TokenStream> = Vec::with_capacity(rust_arity);
        for ty in &fields {
            if is_phantom_data(ty) {
                field_exprs.push(quote! { <#ty as core::default::Default>::default() });
            } else {
                let i = haskell_ix;
                haskell_ix += 1;
                field_exprs.push(emit_field_decode(ty, i, &constructor_identity));
            }
        }

        let construction = match &variant.shape {
            VariantShape::Unit => quote! { #name::#rust_name },
            VariantShape::Tuple(_) => quote! { #name::#rust_name(#(#field_exprs),*) },
            VariantShape::Named(fields) => {
                let field_names = fields.iter().map(|field| &field.ident);
                quote! { #name::#rust_name { #(#field_names: #field_exprs),* } }
            }
        };

        // `question_mark = false`: a failed lookup for THIS variant's
        // constructor must be a SKIP, not an abort. A bare `?` here — the
        // shape every other `emit_datacon_lookup` call site uses — would fail
        // fast on the FIRST variant whose constructor happens to be absent
        // from this compilation's table, even when the value being decoded is
        // a LATER variant whose constructor IS present. Matching on the bare
        // `Result` instead means a missing constructor just skips to the next
        // variant; only `Err(UnknownDataCon)` after every variant has had a
        // turn is a genuine decode failure.
        let lookup = emit_datacon_lookup(
            haskell_module,
            haskell_name,
            haskell_arity_u32,
            haskell_arity,
            false,
        );

        match_arms.push(quote! {
            if let Ok(variant_id) = #lookup {
                if *id == variant_id {
                    if fields.len() != #haskell_arity {
                        return Err(tidepool_bridge::BridgeError::ArityMismatch {
                            con: *id,
                            expected: #haskell_arity,
                            got: fields.len(),
                        });
                    }
                    return Ok(#construction);
                }
            }
        });
    }

    quote! {
        impl #impl_generics tidepool_bridge::sealed::FromHaskellSealed for #name #ty_generics #where_clause {}

        impl #impl_generics tidepool_bridge::FromHaskell for #name #ty_generics #where_clause {
            fn from_value(value: &tidepool_bridge::HaskellValue, table: &tidepool_repr::DataConTable) -> Result<Self, tidepool_bridge::BridgeError> {
                match value {
                    tidepool_bridge::HaskellValue::Con(id, fields) => {
                        #(#match_arms)*
                        Err(tidepool_bridge::BridgeError::UnknownDataCon(*id))
                    }
                    _ => Err(tidepool_bridge::type_mismatch("Con", value)),
                }
            }
        }
    }
}

pub fn generate_to_haskell(info: &EnumInfo) -> TokenStream {
    let name = &info.name;
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::ToHaskell);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.variants
            .iter()
            .flat_map(|variant| variant.shape.types().into_iter().cloned()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let mut match_arms = Vec::new();

    for variant in &info.variants {
        let rust_name = &variant.rust_name;
        let haskell_name = &variant.haskell_name;
        let haskell_module = variant.haskell_module.as_ref();
        let fields = variant.shape.types();

        let haskell_arity: usize = fields.iter().filter(|ty| !is_phantom_data(ty)).count();
        let haskell_arity_u32 = haskell_arity as u32;

        // Bind ALL rust fields (so the pattern compiles) but we underscore
        // phantom fields since they aren't encoded.
        let field_bindings: Vec<_> = fields
            .iter()
            .enumerate()
            .map(|(i, ty)| {
                let base = quote::format_ident!("f{}", i);
                if is_phantom_data(ty) {
                    // Bind to _<name> so the pattern is still irrefutable but unused.
                    let under = quote::format_ident!("_f{}", i);
                    (under, true)
                } else {
                    (base, false)
                }
            })
            .collect();

        let pattern_idents = field_bindings.iter().map(|(ident, _)| ident);
        // A 0-arity TUPLE variant (`Foo::Bar()`, e.g. an `errors`-block ADT's
        // nullary constructor — #335's `LlmBudget`) still needs the `()`
        // pattern; only a genuine unit variant (`Foo::Bar`) omits it. Mirrors
        // `generate_from_haskell`'s construction-side check just below.
        let pattern = match &variant.shape {
            VariantShape::Unit => quote! { #name::#rust_name },
            VariantShape::Tuple(_) => quote! { #name::#rust_name(#(#pattern_idents),*) },
            VariantShape::Named(fields) => {
                let field_names = fields.iter().map(|field| &field.ident);
                quote! { #name::#rust_name { #(#field_names: #pattern_idents),* } }
            }
        };

        let field_visits = field_bindings
            .iter()
            .filter(|(_, is_phantom)| !is_phantom)
            .map(|(ident, _)| {
                quote! { tidepool_bridge::ToHaskell::visit(#ident, table, visitor)?; }
            });

        let lookup = emit_datacon_lookup(
            haskell_module,
            haskell_name,
            haskell_arity_u32,
            haskell_arity,
            true,
        );

        match_arms.push(quote! {
            #pattern => {
                let id = #lookup;
                visitor.begin_constructor(id, #haskell_arity)?;
                #(#field_visits)*
                visitor.end_constructor()
            }
        });
    }

    quote! {
        impl #impl_generics tidepool_bridge::sealed::ToHaskellSealed for #name #ty_generics #where_clause {}

        impl #impl_generics tidepool_bridge::ToHaskell for #name #ty_generics #where_clause {
            fn visit(
                &self,
                table: &tidepool_repr::DataConTable,
                visitor: &mut dyn tidepool_bridge::HaskellVisitor,
            ) -> Result<(), tidepool_bridge::BridgeError> {
                match self {
                    #(#match_arms)*
                }
            }
        }
    }
}

pub fn generate_struct_from_haskell(info: &StructInfo) -> TokenStream {
    let name = &info.name;
    let haskell_name = &info.haskell_name;
    let haskell_module = info.haskell_module.as_ref();
    let constructor_identity = haskell_module
        .map(|module| format!("{module}.{haskell_name}"))
        .unwrap_or_else(|| haskell_name.clone());
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::FromHaskell);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.fields.iter().map(|f| f.ty.clone()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let haskell_arity: usize = haskell_arity(info.fields.iter().map(|f| &f.ty));
    let haskell_arity_u32 = haskell_arity as u32;

    let mut haskell_ix: usize = 0;
    let field_constructions: Vec<_> = info
        .fields
        .iter()
        .map(|f| {
            let field_name = &f.ident;
            let field_ty = &f.ty;
            if is_phantom_data(field_ty) {
                quote! {
                    #field_name: <#field_ty as core::default::Default>::default()
                }
            } else {
                let i = haskell_ix;
                haskell_ix += 1;
                let decode = emit_field_decode(field_ty, i, &constructor_identity);
                quote! {
                    #field_name: #decode
                }
            }
        })
        .collect();

    let construction = if info.fields.is_empty() {
        quote! { #name }
    } else {
        quote! { #name { #(#field_constructions),* } }
    };

    let lookup = emit_datacon_lookup(
        haskell_module,
        haskell_name,
        haskell_arity_u32,
        haskell_arity,
        true,
    );

    quote! {
        impl #impl_generics tidepool_bridge::sealed::FromHaskellSealed for #name #ty_generics #where_clause {}

        impl #impl_generics tidepool_bridge::FromHaskell for #name #ty_generics #where_clause {
            fn from_value(value: &tidepool_bridge::HaskellValue, table: &tidepool_repr::DataConTable) -> Result<Self, tidepool_bridge::BridgeError> {
                match value {
                    tidepool_bridge::HaskellValue::Con(id, fields) => {
                        let con_id = #lookup;
                        if *id != con_id {
                            return Err(tidepool_bridge::BridgeError::UnknownDataCon(*id));
                        }
                        if fields.len() != #haskell_arity {
                            return Err(tidepool_bridge::BridgeError::ArityMismatch {
                                con: *id,
                                expected: #haskell_arity,
                                got: fields.len(),
                            });
                        }
                        Ok(#construction)
                    }
                    _ => Err(tidepool_bridge::type_mismatch("Con", value)),
                }
            }
        }
    }
}

pub fn generate_struct_to_haskell(info: &StructInfo) -> TokenStream {
    let name = &info.name;
    let haskell_name = &info.haskell_name;
    let haskell_module = info.haskell_module.as_ref();
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::ToHaskell);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.fields.iter().map(|f| f.ty.clone()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let haskell_arity: usize = haskell_arity(info.fields.iter().map(|f| &f.ty));
    let haskell_arity_u32 = haskell_arity as u32;

    // Bind ALL fields in the destructure pattern; phantom fields get `_` prefix
    // to silence unused warnings (the pattern must still cover them).
    let field_bindings: Vec<_> = info
        .fields
        .iter()
        .map(|f| {
            let is_phantom = is_phantom_data(&f.ty);
            (f.ident.clone(), f.ty.clone(), is_phantom)
        })
        .collect();

    let destructure_fields = field_bindings.iter().map(|(name, _, is_phantom)| {
        if *is_phantom {
            // `name: _` in a field pattern binds nothing.
            quote! { #name: _ }
        } else {
            quote! { #name }
        }
    });

    let field_visits: Vec<_> = field_bindings
        .iter()
        .filter(|(_, _, is_phantom)| !is_phantom)
        .map(|(f, _, _)| {
            quote! { tidepool_bridge::ToHaskell::visit(#f, table, visitor)?; }
        })
        .collect();

    let destructure = if info.fields.is_empty() {
        quote! { #name }
    } else {
        quote! { #name { #(#destructure_fields),* } }
    };

    let lookup = emit_datacon_lookup(
        haskell_module,
        haskell_name,
        haskell_arity_u32,
        haskell_arity,
        true,
    );

    quote! {
        impl #impl_generics tidepool_bridge::sealed::ToHaskellSealed for #name #ty_generics #where_clause {}

        impl #impl_generics tidepool_bridge::ToHaskell for #name #ty_generics #where_clause {
            fn visit(
                &self,
                table: &tidepool_repr::DataConTable,
                visitor: &mut dyn tidepool_bridge::HaskellVisitor,
            ) -> Result<(), tidepool_bridge::BridgeError> {
                let #destructure = self;
                let id = #lookup;
                visitor.begin_constructor(id, #haskell_arity)?;
                #(#field_visits)*
                visitor.end_constructor()
            }
        }
    }
}
