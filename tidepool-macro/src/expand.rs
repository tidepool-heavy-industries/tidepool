use proc_macro2::TokenStream;
use quote::quote;
use syn::LitStr;

/// Expands the `haskell_eval!` macro.
///
/// Input must be a string literal representing the path to a CBOR-serialized Core expression.
pub fn expand(input: TokenStream) -> TokenStream {
    let path_lit = match syn::parse2::<LitStr>(input) {
        Ok(lit) => lit,
        Err(err) => return err.to_compile_error(),
    };
    let path = path_lit.value();

    quote! {
        {
            static __CBOR: &[u8] = include_bytes!(#path);
            let __expr = core_repr::serial::read::read_cbor(__CBOR)
                .expect("failed to deserialize CBOR — re-run extraction (cargo xtask extract)");
            let mut __heap = core_eval::heap::VecHeap::new();
            let __env = core_eval::env::Env::new();
            core_eval::eval::eval(&__expr, &__env, &mut __heap)
        }
    }
}
