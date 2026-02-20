use crate::parse::{EnumInfo, StructInfo};
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
use syn::{parse_quote, Type};

fn collect_type_params(ty: &Type, params: &HashSet<syn::Ident>, used: &mut HashSet<syn::Ident>) {
    match ty {
        Type::Path(tp) => {
            if tp.qself.is_none() {
                // If it's PhantomData<T>, we DON'T consider T "used" for the purpose
                // of adding FromCore/ToCore bounds, because our PhantomData impl
                // doesn't require T to be FromCore/ToCore.
                if tp.path.segments.last().is_some_and(|s| s.ident == "PhantomData") {
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
        let arity = variant.fields.len();

        let field_conversions = (0..arity).map(|i| {
            let ty = &variant.fields[i];
            quote! {
                <#ty as tidepool_bridge::FromCore>::from_value(&fields[#i], table)?
            }
        });

        let construction = if arity == 0 {
            quote! { #name::#rust_name }
        } else {
            quote! { #name::#rust_name(#(#field_conversions),*) }
        };

        match_arms.push(quote! {
            let variant_id = table.get_by_name(#core_name)
                .ok_or_else(|| tidepool_bridge::BridgeError::UnknownDataConName(#core_name.to_string()))?;
            if *id == variant_id {
                if fields.len() != #arity {
                    return Err(tidepool_bridge::BridgeError::ArityMismatch {
                        con: *id,
                        expected: #arity,
                        got: fields.len(),
                    });
                }
                return Ok(#construction);
            }
        });
    }

    quote! {
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
        let arity = variant.fields.len();

        let field_names: Vec<_> = (0..arity).map(|i| quote::format_ident!("f{}", i)).collect();
        let pattern = if arity == 0 {
            quote! { #name::#rust_name }
        } else {
            quote! { #name::#rust_name(#(#field_names),*) }
        };

        let field_to_values = field_names.iter().map(|f| {
            quote! { tidepool_bridge::ToCore::to_value(#f, table)? }
        });

        match_arms.push(quote! {
            #pattern => {
                let id = table.get_by_name(#core_name)
                    .ok_or_else(|| tidepool_bridge::BridgeError::UnknownDataConName(#core_name.to_string()))?;
                Ok(tidepool_eval::Value::Con(id, vec![#(#field_to_values),*]))
            }
        });
    }

    quote! {
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
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::FromCore);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.fields.iter().map(|(_, ty)| ty.clone()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let arity = info.fields.len();

    let field_constructions: Vec<_> = info
        .fields
        .iter()
        .enumerate()
        .map(|(i, (field_name, field_ty))| {
            quote! {
                #field_name: <#field_ty as tidepool_bridge::FromCore>::from_value(&fields[#i], table)?
            }
        })
        .collect();

    let construction = if info.fields.is_empty() {
        quote! { #name }
    } else {
        quote! { #name { #(#field_constructions),* } }
    };

    quote! {
        impl #impl_generics tidepool_bridge::FromCore for #name #ty_generics #where_clause {
            fn from_value(value: &tidepool_eval::Value, table: &tidepool_repr::DataConTable) -> Result<Self, tidepool_bridge::BridgeError> {
                match value {
                    tidepool_eval::Value::Con(id, fields) => {
                        let con_id = table.get_by_name(#core_name)
                            .ok_or_else(|| tidepool_bridge::BridgeError::UnknownDataConName(#core_name.to_string()))?;
                        if *id != con_id {
                            return Err(tidepool_bridge::BridgeError::UnknownDataCon(*id));
                        }
                        if fields.len() != #arity {
                            return Err(tidepool_bridge::BridgeError::ArityMismatch {
                                con: *id,
                                expected: #arity,
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
    let trait_path: syn::Path = parse_quote!(tidepool_bridge::ToCore);
    let mut generics = info.generics.clone();

    add_trait_bounds(
        &mut generics,
        &trait_path,
        info.fields.iter().map(|(_, ty)| ty.clone()),
    );

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let field_names: Vec<_> = info.fields.iter().map(|(name, _)| name).collect();
    let field_to_values: Vec<_> = field_names
        .iter()
        .map(|f| {
            quote! { tidepool_bridge::ToCore::to_value(#f, table)? }
        })
        .collect();

    let destructure = if info.fields.is_empty() {
        quote! { #name }
    } else {
        quote! { #name { #(#field_names),* } }
    };

    quote! {
        impl #impl_generics tidepool_bridge::ToCore for #name #ty_generics #where_clause {
            fn to_value(&self, table: &tidepool_repr::DataConTable) -> Result<tidepool_eval::Value, tidepool_bridge::BridgeError> {
                let #destructure = self;
                let id = table.get_by_name(#core_name)
                    .ok_or_else(|| tidepool_bridge::BridgeError::UnknownDataConName(#core_name.to_string()))?;
                Ok(tidepool_eval::Value::Con(id, vec![#(#field_to_values),*]))
            }
        }
    }
}
