extern crate proc_macro;

mod codegen;
mod parse;

use parse::DataInfo;
use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

#[proc_macro_derive(FromCore, attributes(core))]
pub fn derive_from_core(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match parse::parse_input(&input) {
        Ok(DataInfo::Enum(info)) => codegen::generate_from_core(&info).into(),
        Ok(DataInfo::Struct(info)) => codegen::generate_struct_from_core(&info).into(),
        Err(e) => e.to_compile_error().into(),
    }
}

#[proc_macro_derive(ToCore, attributes(core))]
pub fn derive_to_core(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match parse::parse_input(&input) {
        Ok(DataInfo::Enum(info)) => codegen::generate_to_core(&info).into(),
        Ok(DataInfo::Struct(info)) => codegen::generate_struct_to_core(&info).into(),
        Err(e) => e.to_compile_error().into(),
    }
}
