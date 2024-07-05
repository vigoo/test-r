use proc_macro::TokenStream;

use proc_macro2::{Ident, Span};
use quote::{quote, ToTokens};
use syn::{FnArg, ItemFn, ReturnType, Type};

#[proc_macro_attribute]
pub fn test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let ast: syn::ItemFn = syn::parse(item).expect("test ast");
    let test_name = ast.sig.ident.clone();
    let test_name_str = test_name.to_string();

    let is_ignored = ast.attrs.iter().any(|attr| attr.path().is_ident("ignore"));

    let register_ident = Ident::new(
        &format!("test_r_register_{}", test_name_str),
        test_name.span(),
    );

    let is_async = ast.sig.asyncness.is_some();
    let dep_getters = get_dependency_params(&ast);

    let register_call = if is_async {
        quote! {
              test_r::core::register_test(
                  #test_name_str,
                  module_path!(),
                  #is_ignored,
                  test_r::core::TestFunction::Async(std::sync::Arc::new(|deps| Box::pin(async move { #test_name(#(#dep_getters),*).await })))
              );
        }
    } else {
        quote! {
            test_r::core::register_test(
                #test_name_str,
                module_path!(),
                #is_ignored,
                test_r::core::TestFunction::Sync(std::sync::Arc::new(|deps| #test_name(#(#dep_getters),*)))
            );
        }
    };

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor]
        fn #register_ident() {
             #register_call
        }

        #ast
    };

    result.into()
}

fn get_dependency_params(ast: &ItemFn) -> Vec<proc_macro2::TokenStream> {
    let mut dep_getters = Vec::new();
    for param in &ast.sig.inputs {
        let dep_type = match param {
            FnArg::Receiver(_) => {
                panic!("Test functions cannot have a self parameter")
            }
            FnArg::Typed(typ) => match &*typ.ty {
                Type::Reference(reference) => {
                    match &*reference.elem {
                        Type::Path(path) => path.clone(),
                        _ => {
                            panic!("Test functions can only have parameters which are immutable references to concrete types, but got {:?}", reference.elem.to_token_stream())
                            // TODO: nicer error report
                        }
                    }
                }
                _ => {
                    panic!("Test functions can only have parameters which are immutable references to concrete types, but got {:?}", typ.ty.to_token_stream())
                    // TODO: nicer error report
                }
            },
        };
        let merged_ident = dep_type
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("_");
        let dep_name = Ident::new(&merged_ident, Span::call_site());
        let dep_name_str = dep_name.to_string().to_lowercase();

        let getter_ident = Ident::new(
            &format!("test_r_get_dep_{}", dep_name_str),
            Span::call_site(),
        );

        dep_getters.push(quote! {
            &#getter_ident(&deps)
        });
    }
    dep_getters
}

#[proc_macro]
pub fn uses_test_r(_item: TokenStream) -> TokenStream {
    r#"
        #[cfg(test)]
        pub fn main() {
            test_r::core::test_runner();
        }
    "#
    .parse()
    .unwrap()
}

#[proc_macro]
pub fn inherit_test_dep(item: TokenStream) -> TokenStream {
    let ast: Type = syn::parse(item).expect("inherit_test_dep! expect a type as a parameter");
    let dep_type = match &ast {
        Type::Path(path) => path.clone(),
        _ => {
            panic!("Dependency constructor must return a single concrete type")
        }
    };
    let merged_ident = dep_type
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("_");
    let dep_name = Ident::new(&merged_ident, Span::call_site());
    let dep_name_str = dep_name.to_string().to_lowercase();
    let getter_ident = Ident::new(
        &format!("test_r_get_dep_{}", dep_name_str),
        Span::call_site(),
    );

    let result = quote! {
        fn #getter_ident<'a>(dependency_view: &'a impl test_r::core::DependencyView) -> Arc<#dep_type> {
            super::#getter_ident(dependency_view)
        }
    };

    result.into()
}

#[proc_macro_attribute]
pub fn test_dep(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let ast: syn::ItemFn = syn::parse(item).expect("test ast");
    let ctor_name = ast.sig.ident.clone();

    let dep_type = match &ast.sig.output {
        ReturnType::Default => {
            panic!("Dependency constructor must have a return type")
        }
        ReturnType::Type(_, typ) => match &**typ {
            Type::Path(path) => path.clone(),
            _ => {
                panic!("Dependency constructor must return a single concrete type")
            }
        },
    };
    let merged_ident = dep_type
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("_");
    let dep_name = Ident::new(&merged_ident, Span::call_site());
    let dep_name_str = dep_name.to_string().to_lowercase();

    let register_ident = Ident::new(
        &format!("test_r_register_{}", dep_name_str),
        Span::call_site(),
    );

    let is_async = ast.sig.asyncness.is_some();
    let dep_getters = get_dependency_params(&ast);

    let register_call = if is_async {
        quote! {
              test_r::core::register_dependency_constructor(
                  #dep_name_str,
                  module_path!(),
                  test_r::core::DependencyConstructor::Async(std::sync::Arc::new(|| Box::pin(async {
                    let result: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(#ctor_name(#(#dep_getters),*).await);
                    result
                  })))
              );
        }
    } else {
        quote! {
            test_r::core::register_dependency_constructor(
                #dep_name_str,
                module_path!(),
                test_r::core::DependencyConstructor::Sync(std::sync::Arc::new(|| std::sync::Arc::new(#ctor_name(#(#dep_getters),*))))
            );
        }
    };

    let getter_ident = Ident::new(
        &format!("test_r_get_dep_{}", dep_name_str),
        Span::call_site(),
    );

    let getter_body = quote! {
        dependency_view
            .get(#dep_name_str)
            .expect("Dependency not found")
            .downcast::<#dep_type>()
            .expect("Dependency type mismatch")
    };

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor]
        fn #register_ident() {
             #register_call
        }

        #[cfg(test)]
        fn #getter_ident<'a>(dependency_view: &'a impl test_r::core::DependencyView) -> Arc<#dep_type> {
            #getter_body
        }

        #ast
    };

    result.into()
}
