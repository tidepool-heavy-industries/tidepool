use crate::parse::EnumInfo;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{parse_quote, Type};
use std::collections::HashSet;

fn collect_type_params(ty: &Type, params: &HashSet<syn::Ident>, used: &mut HashSet<syn::Ident>) {
    match ty {
        Type::Path(tp) => {
            if tp.qself.is_none() {
                // If it's PhantomData<T>, we DON'T consider T "used" for the purpose
                // of adding FromCore/ToCore bounds, because our PhantomData impl
                // doesn't require T to be FromCore/ToCore.
                if tp.path.segments.last().map_or(false, |s| s.ident == "PhantomData") {
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

pub fn generate_from_core(info: &EnumInfo) -> TokenStream {
    let name = &info.name;
    let trait_path: syn::Path = parse_quote!(core_bridge::FromCore);
    let mut generics = info.generics.clone();

    let all_params: HashSet<_> = info.generics.type_params().map(|p| p.ident.clone()).collect();
    let mut used_params = HashSet::new();
    for variant in &info.variants {
        for field_ty in &variant.fields {
            collect_type_params(field_ty, &all_params, &mut used_params);
        }
    }

    // Add trait bounds only for used type parameters
    for param in &mut generics.params {
        if let syn::GenericParam::Type(type_param) = param {
            if used_params.contains(&type_param.ident) {
                type_param.bounds.push(parse_quote!(#trait_path));
            }
        }
    }

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let mut match_arms = Vec::new();

    for variant in &info.variants {
        let rust_name = &variant.rust_name;
        let core_name = &variant.core_name;
        let arity = variant.fields.len();

        let field_conversions = (0..arity).map(|i| {
            let ty = &variant.fields[i];
            quote! {
                <#ty as core_bridge::FromCore>::from_value(&fields[#i], table)?
            }
        });

        let construction = if arity == 0 {
            quote! { #name::#rust_name }
        } else {
            quote! { #name::#rust_name(#(#field_conversions),*) }
        };

        match_arms.push(quote! {
            let variant_id = table.get_by_name(#core_name)
                .ok_or_else(|| core_bridge::BridgeError::UnknownDataConName(#core_name.to_string()))?;
            if *id == variant_id {
                if fields.len() != #arity {
                    return Err(core_bridge::BridgeError::ArityMismatch {
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
        impl #impl_generics core_bridge::FromCore for #name #ty_generics #where_clause {
            fn from_value(value: &core_eval::Value, table: &core_repr::DataConTable) -> Result<Self, core_bridge::BridgeError> {
                match value {
                    core_eval::Value::Con(id, fields) => {
                        #(#match_arms)*
                        Err(core_bridge::BridgeError::UnknownDataCon(*id))
                    }
                    _ => Err(core_bridge::BridgeError::TypeMismatch {
                        expected: "Con".to_string(),
                        got: match value {
                            core_eval::Value::Lit(l) => format!("Lit({:?})", l),
                            core_eval::Value::Con(id, _) => format!("Con({:?})", id),
                            core_eval::Value::Closure(_, _, _) => "Closure".to_string(),
                            core_eval::Value::ThunkRef(id) => format!("ThunkRef({:?})", id),
                            core_eval::Value::JoinCont(_, _, _) => "JoinCont".to_string(),
                        },
                    })
                }
            }
        }
    }
}

pub fn generate_to_core(info: &EnumInfo) -> TokenStream {
    let name = &info.name;
    let trait_path: syn::Path = parse_quote!(core_bridge::ToCore);
    let mut generics = info.generics.clone();

    let all_params: HashSet<_> = info.generics.type_params().map(|p| p.ident.clone()).collect();
    let mut used_params = HashSet::new();
    for variant in &info.variants {
        for field_ty in &variant.fields {
            collect_type_params(field_ty, &all_params, &mut used_params);
        }
    }

    // Add trait bounds only for used type parameters
    for param in &mut generics.params {
        if let syn::GenericParam::Type(type_param) = param {
            if used_params.contains(&type_param.ident) {
                type_param.bounds.push(parse_quote!(#trait_path));
            }
        }
    }

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
            quote! { core_bridge::ToCore::to_value(#f, table)? }
        });

        match_arms.push(quote! {
            #pattern => {
                let id = table.get_by_name(#core_name)
                    .ok_or_else(|| core_bridge::BridgeError::UnknownDataConName(#core_name.to_string()))?;
                Ok(core_eval::Value::Con(id, vec![#(#field_to_values),*]))
            }
        });
    }

    quote! {
        impl #impl_generics core_bridge::ToCore for #name #ty_generics #where_clause {
            fn to_value(&self, table: &core_repr::DataConTable) -> Result<core_eval::Value, core_bridge::BridgeError> {
                match self {
                    #(#match_arms)*
                }
            }
        }
    }
}
