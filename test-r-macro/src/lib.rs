use proc_macro::TokenStream;
use proc_macro2::Ident;
use quote::quote;

#[proc_macro_attribute]
pub fn test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let ast: syn::ItemFn = syn::parse(item).expect("test ast");
    let test_name = ast.sig.ident.clone();
    let test_name_str = test_name.to_string();

    let register_ident = Ident::new(&format!("test_r_register_{}", test_name_str), test_name.span());

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor]
        fn #register_ident() {
             test_r::core::register_test(
                #test_name_str,
                module_path!(),
                Box::new(|| #test_name())
            );
        }

        #ast
    };

    result.into()
}

#[proc_macro]
pub fn uses_test_r(_item: TokenStream) -> TokenStream {
    r#"
        #[cfg(test)]
        pub fn main() {
            test_r::core::test_runner();
        }
    "#.parse().unwrap()
}