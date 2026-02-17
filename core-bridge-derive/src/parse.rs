use syn::{Attribute, Data, DeriveInput, Fields, Generics, Ident, Lit, Type};

pub struct EnumInfo {
    pub name: Ident,
    pub generics: Generics,
    pub variants: Vec<VariantInfo>,
}

pub struct VariantInfo {
    pub rust_name: Ident,
    pub core_name: String,
    pub fields: Vec<Type>,
}

pub fn parse_enum(input: &DeriveInput) -> Result<EnumInfo, syn::Error> {
    let data_enum = match &input.data {
        Data::Enum(e) => e,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "Only enums are supported",
            ))
        }
    };

    let mut variants = Vec::new();
    for variant in &data_enum.variants {
        let rust_name = variant.ident.clone();
        let core_name = get_core_name(&variant.attrs)?;

        let fields = match &variant.fields {
            Fields::Unnamed(f) => f.unnamed.iter().map(|field| field.ty.clone()).collect(),
            Fields::Unit => Vec::new(),
            Fields::Named(_) => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "Named fields are not supported in variants",
                ))
            }
        };

        let core_name_str = core_name.unwrap_or_else(|| rust_name.to_string());

        variants.push(VariantInfo {
            rust_name,
            core_name: core_name_str,
            fields,
        });
    }

    Ok(EnumInfo {
        name: input.ident.clone(),
        generics: input.generics.clone(),
        variants,
    })
}

fn get_core_name(attrs: &[Attribute]) -> Result<Option<String>, syn::Error> {
    for attr in attrs {
        if attr.path().is_ident("core") {
            let mut core_name = None;
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Str(s) = lit {
                        core_name = Some(s.value());
                        Ok(())
                    } else {
                        Err(meta.error("expected string literal for 'name'"))
                    }
                } else {
                    Err(meta.error("unknown core attribute"))
                }
            })?;
            if core_name.is_some() {
                return Ok(core_name);
            }
        }
    }
    Ok(None)
}
