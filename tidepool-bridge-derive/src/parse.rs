use syn::{Attribute, Data, DeriveInput, Fields, Generics, Ident, Lit, Type};

pub struct EnumInfo {
    pub name: Ident,
    pub generics: Generics,
    pub variants: Vec<VariantInfo>,
}

pub struct VariantInfo {
    pub rust_name: Ident,
    pub core_name: String,
    /// Optional source-module path (e.g. `"Pattern.Memory"`). When present,
    /// derive-generated lookups call `get_by_qualified_name("<module>.<core_name>")`
    /// to disambiguate constructors that share name and arity across modules.
    pub core_module: Option<String>,
    pub fields: Vec<Type>,
}

pub struct StructInfo {
    pub name: Ident,
    pub generics: Generics,
    pub core_name: String,
    /// See [`VariantInfo::core_module`].
    pub core_module: Option<String>,
    pub fields: Vec<FieldInfo>,
}

/// A single named struct field, with optional Haskell-side overrides parsed
/// from a field-level `#[core(hs = "...", hs_type = "...")]` attribute.
pub struct FieldInfo {
    pub ident: Ident,
    pub ty: Type,
    /// Overrides the Haskell field NAME (default: snake_case → camelCase of the
    /// Rust ident). E.g. `#[core(hs = "exitCode")]`.
    pub hs_name: Option<String>,
    /// Overrides the whole rendered Haskell field TYPE (default: mapped from the
    /// Rust type). E.g. `pos: LspPosition` whose Haskell type is `Position`
    /// uses `#[core(hs_type = "Position")]`.
    pub hs_type: Option<String>,
}

pub enum DataInfo {
    Enum(EnumInfo),
    Struct(StructInfo),
}

pub fn parse_input(input: &DeriveInput) -> Result<DataInfo, syn::Error> {
    match &input.data {
        Data::Enum(_) => parse_enum(input).map(DataInfo::Enum),
        Data::Struct(s) => {
            let CoreAttr {
                name: core_name,
                module: core_module,
                ..
            } = parse_core_attr(&input.attrs)?;
            let core_name = core_name.unwrap_or_else(|| input.ident.to_string());
            let fields = match &s.fields {
                Fields::Named(f) => {
                    let mut out = Vec::with_capacity(f.named.len());
                    for field in &f.named {
                        let Some(ident) = field.ident.clone() else {
                            continue;
                        };
                        let attr = parse_core_attr(&field.attrs)?;
                        out.push(FieldInfo {
                            ident,
                            ty: field.ty.clone(),
                            hs_name: attr.hs,
                            hs_type: attr.hs_type,
                        });
                    }
                    out
                }
                Fields::Unit => Vec::new(),
                Fields::Unnamed(_) => {
                    return Err(syn::Error::new_spanned(
                        input,
                        "Tuple structs are not supported, use named fields",
                    ))
                }
            };
            Ok(DataInfo::Struct(StructInfo {
                name: input.ident.clone(),
                generics: input.generics.clone(),
                core_name,
                core_module,
                fields,
            }))
        }
        Data::Union(_) => Err(syn::Error::new_spanned(input, "Unions are not supported")),
    }
}

pub fn parse_enum(input: &DeriveInput) -> Result<EnumInfo, syn::Error> {
    let data_enum = match &input.data {
        Data::Enum(e) => e,
        _ => return Err(syn::Error::new_spanned(input, "Only enums are supported")),
    };

    let mut variants = Vec::new();
    for variant in &data_enum.variants {
        let rust_name = variant.ident.clone();
        let CoreAttr {
            name: core_name,
            module: core_module,
            ..
        } = parse_core_attr(&variant.attrs)?;

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
            core_module,
            fields,
        });
    }

    Ok(EnumInfo {
        name: input.ident.clone(),
        generics: input.generics.clone(),
        variants,
    })
}

/// Parsed contents of a single `#[core(...)]` attribute.
#[derive(Default)]
pub(crate) struct CoreAttr {
    /// Explicit `name = "..."` override; defaults to the Rust identifier.
    pub(crate) name: Option<String>,
    /// Optional `module = "..."` qualifier. When set, derived lookups use
    /// `get_by_qualified_name("<module>.<name>")` to pick the correct
    /// constructor when name+arity collisions exist across source modules.
    pub(crate) module: Option<String>,
    /// Field-level `hs = "..."`: overrides the generated Haskell field name.
    pub(crate) hs: Option<String>,
    /// Field-level `hs_type = "..."`: overrides the generated Haskell field type.
    pub(crate) hs_type: Option<String>,
}

pub(crate) fn parse_core_attr(attrs: &[Attribute]) -> Result<CoreAttr, syn::Error> {
    let mut out = CoreAttr::default();
    for attr in attrs {
        if attr.path().is_ident("core") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Str(s) = lit {
                        out.name = Some(s.value());
                        Ok(())
                    } else {
                        Err(meta.error("expected string literal for 'name'"))
                    }
                } else if meta.path.is_ident("module") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Str(s) = lit {
                        out.module = Some(s.value());
                        Ok(())
                    } else {
                        Err(meta.error("expected string literal for 'module'"))
                    }
                } else if meta.path.is_ident("hs") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Str(s) = lit {
                        out.hs = Some(s.value());
                        Ok(())
                    } else {
                        Err(meta.error("expected string literal for 'hs'"))
                    }
                } else if meta.path.is_ident("hs_type") {
                    let value = meta.value()?;
                    let lit: Lit = value.parse()?;
                    if let Lit::Str(s) = lit {
                        out.hs_type = Some(s.value());
                        Ok(())
                    } else {
                        Err(meta.error("expected string literal for 'hs_type'"))
                    }
                } else {
                    Err(meta.error(
                        "unknown core attribute (expected 'name', 'module', 'hs', or 'hs_type')",
                    ))
                }
            })?;
        }
    }
    Ok(out)
}
