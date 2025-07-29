use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{ItemMod, Token, parse_macro_input};

pub fn tag(attr: TokenStream, item: TokenStream) -> TokenStream {
    if let Ok(ast) = syn::parse::<ItemMod>(item.clone()) {
        let random = rand::random::<u64>();
        let register_ident = Ident::new(
            &format!("test_r_register_mod_{}_tag_{}", ast.ident, random),
            Span::call_site(),
        );

        let mod_name_str = ast.ident.to_string();

        let tag = parse_macro_input!(attr as Ident);
        let tag_str = tag.to_string();
        let tag = quote! { #tag_str.to_string() };

        let register_call = quote! {
              test_r::core::register_suite_tag(
                  #mod_name_str,
                  module_path!(),
                  #tag
              );
        };

        let result = quote! {
            #[cfg(test)]
            #[test_r::ctor::ctor(crate_path=::test_r::ctor)]
            fn #register_ident() {
                 #register_call
            }

            #ast
        };

        result.into()
    } else {
        // applied to a test function
        item
    }
}

pub fn tag_suite(input: TokenStream) -> TokenStream {
    let params = parse_macro_input!(input with Punctuated::<Ident, Token![,]>::parse_terminated);

    if params.len() != 2 {
        panic!(
            "tag_suite! expects exactly 2 identifiers as parameters: the name of the suite module and the tag"
        );
    }

    let mod_name_str = params[0].to_string();
    let tag_str = params[1].to_string();

    let random = rand::random::<u64>();
    let register_ident = Ident::new(
        &format!("test_r_register_mod_{mod_name_str}_tag_{random}"),
        Span::call_site(),
    );

    let tag = quote! { #tag_str.to_string() };

    let register_call = quote! {
          test_r::core::register_suite_tag(
              #mod_name_str,
              module_path!(),
              #tag
          );
    };

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor(crate_path=::test_r::ctor)]
        fn #register_ident() {
             #register_call
        }
    };

    result.into()
}

pub fn sequential(item: TokenStream) -> TokenStream {
    let ast: ItemMod = syn::parse(item).expect("#[sequential] must be applied to a module");

    let register_ident = Ident::new(
        &format!("test_r_register_mod_{}_sequential", ast.ident),
        Span::call_site(),
    );

    let mod_name_str = ast.ident.to_string();
    let register_call = quote! {
          test_r::core::register_suite_sequential(
              #mod_name_str,
              module_path!(),
          );
    };

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor(crate_path=::test_r::ctor)]
        fn #register_ident() {
             #register_call
        }

        #ast
    };

    result.into()
}

pub fn sequential_suite(input: TokenStream) -> TokenStream {
    let params = parse_macro_input!(input with Punctuated::<Ident, Token![,]>::parse_terminated);

    if params.len() != 1 {
        panic!(
            "sequential_suite! expects exactly 1 identifier as parameter: the name of the suite module"
        );
    }

    let mod_name_str = params[0].to_string();

    let register_ident = Ident::new(
        &format!("test_r_register_mod_{mod_name_str}_sequential"),
        Span::call_site(),
    );

    let register_call = quote! {
          test_r::core::register_suite_sequential(
              #mod_name_str,
              module_path!(),
          );
    };

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor(crate_path=::test_r::ctor)]
        fn #register_ident() {
             #register_call
        }
    };

    result.into()
}
