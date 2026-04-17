use crate::parse::{EnumInfo, StructInfo};
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
use syn::{parse_quote, Type};

fn collect_type_params(ty: &Type, params: &HashSet<syn::Ident>, used: &mut HashSet<syn::Ident>) {
    match ty {
        Type::Path(tp) if tp.qself.is_none() => {
            // If it's PhantomData<T>, we DON'T consider T "used" for the purpose
            // of adding FromCore/ToCore bounds, because our PhantomData impl
            // doesn't require T to be FromCore/ToCore.
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
/// segment is `PhantomData`. Such fields have no Core representation and are
/// skipped when computing a variant/struct's Core arity and when encoding or
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
/// `DataConId`; arity validation still happens at the call site via the
/// existing field-count check so mismatched arities are surfaced as
/// `ArityMismatch` (qualified-name lookup does not pre-filter by arity).
fn emit_datacon_lookup(
    module: Option<&String>,
    core_name: &str,
    core_arity_u32: u32,
    core_arity_usize: usize,
) -> TokenStream {
    if let Some(module) = module {
        let qualified = format!("{}.{}", module, core_name);
        quote! {
            table.get_by_qualified_name(#qualified)
                .ok_or_else(|| tidepool_bridge::BridgeError::UnknownDataConQualified {
                    qualified_name: #qualified.to_string(),
                })?
        }
    } else {
        // Silence unused-var warnings in the `Some` branch where arity isn't
        // consumed by the emitted code.
        let _ = core_arity_usize;
        quote! {
            table.get_by_name_arity(#core_name, #core_arity_u32)
                .ok_or_else(|| tidepool_bridge::BridgeError::UnknownDataConNameArity {
                    name: #core_name.to_string(),
                    arity: #core_arity_u32 as usize,
                })?
        }
    }
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

pub fn generate_from_core(info: &EnumInfo) -> TokenStream {
    let name = &info.name;
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::FromCore);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.variants.iter().flat_map(|v| v.fields.iter().cloned()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let mut match_arms = Vec::new();

    for variant in &info.variants {
        let rust_name = &variant.rust_name;
        let core_name = &variant.core_name;
        let core_module = variant.core_module.as_ref();
        let rust_arity = variant.fields.len();

        // Core arity excludes PhantomData fields — they have no Core
        // representation and must not consume slots in the encoded Con.
        let core_arity: usize = variant
            .fields
            .iter()
            .filter(|ty| !is_phantom_data(ty))
            .count();
        let core_arity_u32 = core_arity as u32;

        // Build per-Rust-field construction expressions. PhantomData fields
        // get a default `PhantomData` literal; other fields pull from the next
        // Core field slot.
        let mut core_ix: usize = 0;
        let mut field_exprs: Vec<TokenStream> = Vec::with_capacity(rust_arity);
        for ty in &variant.fields {
            if is_phantom_data(ty) {
                field_exprs.push(quote! { <#ty as core::default::Default>::default() });
            } else {
                let i = core_ix;
                core_ix += 1;
                field_exprs.push(quote! {
                    <#ty as tidepool_bridge::FromCore>::from_value(&fields[#i], table)?
                });
            }
        }

        let construction = if rust_arity == 0 {
            quote! { #name::#rust_name }
        } else {
            quote! { #name::#rust_name(#(#field_exprs),*) }
        };

        let lookup = emit_datacon_lookup(core_module, core_name, core_arity_u32, core_arity);

        match_arms.push(quote! {
            let variant_id = #lookup;
            if *id == variant_id {
                if fields.len() != #core_arity {
                    return Err(tidepool_bridge::BridgeError::ArityMismatch {
                        con: *id,
                        expected: #core_arity,
                        got: fields.len(),
                    });
                }
                return Ok(#construction);
            }
        });
    }

    quote! {
        impl #impl_generics tidepool_bridge::sealed::FromCoreSealed for #name #ty_generics #where_clause {}

        impl #impl_generics tidepool_bridge::FromCore for #name #ty_generics #where_clause {
            fn from_value(value: &tidepool_eval::Value, table: &tidepool_repr::DataConTable) -> Result<Self, tidepool_bridge::BridgeError> {
                match value {
                    tidepool_eval::Value::Con(id, fields) => {
                        #(#match_arms)*
                        Err(tidepool_bridge::BridgeError::UnknownDataCon(*id))
                    }
                    _ => Err(tidepool_bridge::BridgeError::TypeMismatch {
                        expected: "Con".to_string(),
                        got: match value {
                            tidepool_eval::Value::Lit(l) => format!("Lit({:?})", l),
                            tidepool_eval::Value::Con(id, _) => format!("Con({:?})", id),
                            tidepool_eval::Value::Closure(_, _, _) => "Closure".to_string(),
                            tidepool_eval::Value::ThunkRef(id) => format!("ThunkRef({:?})", id),
                            tidepool_eval::Value::JoinCont(_, _, _) => "JoinCont".to_string(),
                            tidepool_eval::Value::ConFun(id, arity, args) => format!("ConFun({:?}, {}/{})", id, args.len(), arity),
                            tidepool_eval::Value::ByteArray(bs) => match bs.lock() {
                                Ok(b) => format!("ByteArray(len={})", b.len()),
                                Err(_) => "ByteArray(poisoned)".to_string(),
                            },
                        },
                    })
                }
            }
        }
    }
}

pub fn generate_to_core(info: &EnumInfo) -> TokenStream {
    let name = &info.name;
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::ToCore);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.variants.iter().flat_map(|v| v.fields.iter().cloned()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let mut match_arms = Vec::new();

    for variant in &info.variants {
        let rust_name = &variant.rust_name;
        let core_name = &variant.core_name;
        let core_module = variant.core_module.as_ref();
        let rust_arity = variant.fields.len();

        let core_arity: usize = variant
            .fields
            .iter()
            .filter(|ty| !is_phantom_data(ty))
            .count();
        let core_arity_u32 = core_arity as u32;

        // Bind ALL rust fields (so the pattern compiles) but we underscore
        // phantom fields since they aren't encoded.
        let field_bindings: Vec<_> = variant
            .fields
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
        let pattern = if rust_arity == 0 {
            quote! { #name::#rust_name }
        } else {
            quote! { #name::#rust_name(#(#pattern_idents),*) }
        };

        let field_to_values = field_bindings
            .iter()
            .filter(|(_, is_phantom)| !is_phantom)
            .map(|(ident, _)| {
                quote! { tidepool_bridge::ToCore::to_value(#ident, table)? }
            });

        let lookup = emit_datacon_lookup(core_module, core_name, core_arity_u32, core_arity);

        match_arms.push(quote! {
            #pattern => {
                let id = #lookup;
                Ok(tidepool_eval::Value::Con(id, vec![#(#field_to_values),*]))
            }
        });
    }

    quote! {
        impl #impl_generics tidepool_bridge::sealed::ToCoreSealed for #name #ty_generics #where_clause {}

        impl #impl_generics tidepool_bridge::ToCore for #name #ty_generics #where_clause {
            fn to_value(&self, table: &tidepool_repr::DataConTable) -> Result<tidepool_eval::Value, tidepool_bridge::BridgeError> {
                match self {
                    #(#match_arms)*
                }
            }
        }
    }
}

pub fn generate_struct_from_core(info: &StructInfo) -> TokenStream {
    let name = &info.name;
    let core_name = &info.core_name;
    let core_module = info.core_module.as_ref();
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::FromCore);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.fields.iter().map(|(_, ty)| ty.clone()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let core_arity: usize = info
        .fields
        .iter()
        .filter(|(_, ty)| !is_phantom_data(ty))
        .count();
    let core_arity_u32 = core_arity as u32;

    let mut core_ix: usize = 0;
    let field_constructions: Vec<_> = info
        .fields
        .iter()
        .map(|(field_name, field_ty)| {
            if is_phantom_data(field_ty) {
                quote! {
                    #field_name: <#field_ty as core::default::Default>::default()
                }
            } else {
                let i = core_ix;
                core_ix += 1;
                quote! {
                    #field_name: <#field_ty as tidepool_bridge::FromCore>::from_value(&fields[#i], table)?
                }
            }
        })
        .collect();

    let construction = if info.fields.is_empty() {
        quote! { #name }
    } else {
        quote! { #name { #(#field_constructions),* } }
    };

    let lookup = emit_datacon_lookup(core_module, core_name, core_arity_u32, core_arity);

    quote! {
        impl #impl_generics tidepool_bridge::sealed::FromCoreSealed for #name #ty_generics #where_clause {}

        impl #impl_generics tidepool_bridge::FromCore for #name #ty_generics #where_clause {
            fn from_value(value: &tidepool_eval::Value, table: &tidepool_repr::DataConTable) -> Result<Self, tidepool_bridge::BridgeError> {
                match value {
                    tidepool_eval::Value::Con(id, fields) => {
                        let con_id = #lookup;
                        if *id != con_id {
                            return Err(tidepool_bridge::BridgeError::UnknownDataCon(*id));
                        }
                        if fields.len() != #core_arity {
                            return Err(tidepool_bridge::BridgeError::ArityMismatch {
                                con: *id,
                                expected: #core_arity,
                                got: fields.len(),
                            });
                        }
                        Ok(#construction)
                    }
                    _ => Err(tidepool_bridge::BridgeError::TypeMismatch {
                        expected: "Con".to_string(),
                        got: match value {
                            tidepool_eval::Value::Lit(l) => format!("Lit({:?})", l),
                            tidepool_eval::Value::Con(id, _) => format!("Con({:?})", id),
                            tidepool_eval::Value::Closure(_, _, _) => "Closure".to_string(),
                            tidepool_eval::Value::ThunkRef(id) => format!("ThunkRef({:?})", id),
                            tidepool_eval::Value::JoinCont(_, _, _) => "JoinCont".to_string(),
                            tidepool_eval::Value::ConFun(id, arity, args) => format!("ConFun({:?}, {}/{})", id, args.len(), arity),
                            tidepool_eval::Value::ByteArray(bs) => match bs.lock() {
                                Ok(b) => format!("ByteArray(len={})", b.len()),
                                Err(_) => "ByteArray(poisoned)".to_string(),
                            },
                        },
                    })
                }
            }
        }
    }
}

pub fn generate_struct_to_core(info: &StructInfo) -> TokenStream {
    let name = &info.name;
    let core_name = &info.core_name;
    let core_module = info.core_module.as_ref();
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::ToCore);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.fields.iter().map(|(_, ty)| ty.clone()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let core_arity: usize = info
        .fields
        .iter()
        .filter(|(_, ty)| !is_phantom_data(ty))
        .count();
    let core_arity_u32 = core_arity as u32;

    // Bind ALL fields in the destructure pattern; phantom fields get `_` prefix
    // to silence unused warnings (the pattern must still cover them).
    let field_bindings: Vec<_> = info
        .fields
        .iter()
        .map(|(field_name, ty)| {
            let is_phantom = is_phantom_data(ty);
            (field_name.clone(), ty.clone(), is_phantom)
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

    let field_to_values: Vec<_> = field_bindings
        .iter()
        .filter(|(_, _, is_phantom)| !is_phantom)
        .map(|(f, _, _)| {
            quote! { tidepool_bridge::ToCore::to_value(#f, table)? }
        })
        .collect();

    let destructure = if info.fields.is_empty() {
        quote! { #name }
    } else {
        quote! { #name { #(#destructure_fields),* } }
    };

    let lookup = emit_datacon_lookup(core_module, core_name, core_arity_u32, core_arity);

    quote! {
        impl #impl_generics tidepool_bridge::sealed::ToCoreSealed for #name #ty_generics #where_clause {}

        impl #impl_generics tidepool_bridge::ToCore for #name #ty_generics #where_clause {
            fn to_value(&self, table: &tidepool_repr::DataConTable) -> Result<tidepool_eval::Value, tidepool_bridge::BridgeError> {
                let #destructure = self;
                let id = #lookup;
                Ok(tidepool_eval::Value::Con(id, vec![#(#field_to_values),*]))
            }
        }
    }
}
