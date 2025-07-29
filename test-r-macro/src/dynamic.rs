use crate::deps::get_dependency_params_for_closure;
use crate::helpers::is_testr_attribute;
use proc_macro::TokenStream;
use proc_macro2::Ident;
use quote::{ToTokens, quote};
use syn::punctuated::Punctuated;
use syn::{Expr, ExprClosure, ItemFn, Token, parse_macro_input, parse2};

pub fn test_gen(item: TokenStream) -> TokenStream {
    let ast: ItemFn = syn::parse(item).expect("test generator ast");
    let generator_name = ast.sig.ident.clone();
    let generator_name_str = generator_name.to_string();

    let is_ignored = ast
        .attrs
        .iter()
        .any(|attr| is_testr_attribute(attr, "ignore"));

    let register_ident = Ident::new(
        &format!("test_r_register_generator_{generator_name_str}"),
        generator_name.span(),
    );

    let is_async = ast.sig.asyncness.is_some();

    let register_call = if is_async {
        quote! {
              test_r::core::register_test_generator(
                  #generator_name_str,
                  module_path!(),
                  #is_ignored,
                  test_r::core::TestGeneratorFunction::Async(std::sync::Arc::new(|| Box::pin(async move { #generator_name().await })))
              );
        }
    } else {
        quote! {
            test_r::core::register_test_generator(
                #generator_name_str,
                module_path!(),
                #is_ignored,
                test_r::core::TestGeneratorFunction::Sync(std::sync::Arc::new(|| #generator_name()))
            );
        }
    };

    let wrapped_ast = if is_async {
        quote! {
            async fn #generator_name() -> Vec<test_r::core::GeneratedTest> {
                let mut tests = test_r::core::DynamicTestRegistration::new();
                #ast
                #generator_name(&mut tests).await;
                tests.to_vec()
            }
        }
    } else {
        quote! {
            fn #generator_name() -> Vec<test_r::core::GeneratedTest> {
                let mut tests = test_r::core::DynamicTestRegistration::new();
                #ast
                #generator_name(&mut tests);
                tests.to_vec()
            }
        }
    };

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor(crate_path=::test_r::ctor)]
        fn #register_ident() {
             #register_call
        }

        #wrapped_ast
    };

    result.into()
}

pub fn add_test(input: TokenStream) -> TokenStream {
    let params = parse_macro_input!(input with Punctuated::<Expr, Token![,]>::parse_terminated);

    if params.len() != 4 {
        panic!("add_test! expects exactly 4 parameters");
    }

    let dtr_expr = &params[0];
    let name_expr = &params[1];
    let test_props_expr = &params[2];

    let function_expr = &params[3];

    let function_closure: ExprClosure = parse2(function_expr.to_token_stream())
        .expect("the third parameter of add_test! must be a closure");

    let (dep_getters, _dep_names, bindings) =
        get_dependency_params_for_closure(function_closure.inputs.iter());
    let is_async = matches!(&*function_closure.body, Expr::Async(_));

    let result = if is_async {
        let mut lets = Vec::new();
        for (getter, ident) in dep_getters.iter().zip(bindings) {
            lets.push(quote! {
                let #ident = #getter;
            });
        }
        let body = match &*function_closure.body {
            Expr::Async(inner) => inner.block.clone(),
            _ => panic!("Expected async block"),
        };
        quote! {
            #dtr_expr.add_async_test(#name_expr, #test_props_expr, move |__test_r_deps_arg| {
                Box::pin(async move {
                    #(#lets)*
                    #body
                })
            });
        }
    } else {
        quote! {
            #dtr_expr.add_sync_test(#name_expr, #test_props_expr, move |__test_r_deps_arg| {
                let gen_fn = #function_closure;
                gen_fn(#(#dep_getters),*)
            });
        }
    };

    result.into()
}
