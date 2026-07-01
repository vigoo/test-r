use crate::deps::{DependencyTag, type_path_to_string};
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::{ToTokens, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, ItemMod, LitInt, LitStr, Token, Type, parse_macro_input};

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

pub fn timeout(attr: TokenStream, item: TokenStream) -> TokenStream {
    if let Ok(ast) = syn::parse::<ItemMod>(item.clone()) {
        let random = rand::random::<u64>();
        let register_ident = Ident::new(
            &format!("test_r_register_mod_{}_timeout_{}", ast.ident, random),
            Span::call_site(),
        );

        let mod_name_str = ast.ident.to_string();
        let timeout_millis = parse_timeout_millis(attr);

        let register_call = quote! {
              test_r::core::register_suite_timeout(
                  #mod_name_str,
                  module_path!(),
                  std::time::Duration::from_millis(#timeout_millis),
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

pub fn timeout_suite(input: TokenStream) -> TokenStream {
    let params = parse_macro_input!(input with Punctuated::<Expr, Token![,]>::parse_terminated);

    if params.len() != 2 {
        panic!(
            "timeout_suite! expects exactly 2 parameters: the name of the suite module and the timeout (milliseconds or duration string)"
        );
    }

    let mod_name_str = if let Expr::Path(path) = &params[0] {
        path.path
            .get_ident()
            .expect("first parameter must be an identifier")
            .to_string()
    } else {
        panic!("first parameter must be an identifier (the name of the suite module)");
    };

    let timeout_millis = match &params[1] {
        Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Int(int) => int.base10_parse::<u64>().expect(
                "timeout must be an integer (milliseconds) or a human-readable duration string",
            ),
            syn::Lit::Str(s) => {
                let duration = s.value().parse::<humantime::Duration>().expect(
                    "timeout must be an integer (milliseconds) or a human-readable duration string",
                );
                duration.as_millis() as u64
            }
            _ => panic!(
                "timeout must be an integer (milliseconds) or a human-readable duration string"
            ),
        },
        _ => {
            panic!("timeout must be an integer (milliseconds) or a human-readable duration string")
        }
    };

    let random = rand::random::<u64>();
    let register_ident = Ident::new(
        &format!("test_r_register_mod_{mod_name_str}_timeout_{random}"),
        Span::call_site(),
    );

    let register_call = quote! {
          test_r::core::register_suite_timeout(
              #mod_name_str,
              module_path!(),
              std::time::Duration::from_millis(#timeout_millis),
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

fn parse_timeout_millis(attr: TokenStream) -> u64 {
    if let Ok(timeout) = syn::parse::<LitInt>(attr.clone()) {
        timeout
            .base10_parse::<u64>()
            .expect("timeout attribute's parameter must be an integer (timeout milliseconds) or a human-readable duration string")
    } else if let Ok(timeout) = syn::parse::<LitStr>(attr) {
        let duration = timeout.value().parse::<humantime::Duration>()
            .expect("timeout attribute's parameter must be an integer (timeout milliseconds) or a human-readable duration string");
        duration.as_millis() as u64
    } else {
        panic!(
            "timeout attribute's parameter must be an integer (timeout milliseconds) or a human-readable duration string"
        );
    }
}

/// Parsed input of `matrix_suite!(<module>, <dim>, <DepType>)`.
struct MatrixSuiteInput {
    module: Ident,
    dim: Ident,
    dep_type: Type,
}

impl Parse for MatrixSuiteInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let module: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let dim: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let dep_type: Type = input.parse()?;
        Ok(MatrixSuiteInput {
            module,
            dim,
            dep_type,
        })
    }
}

/// `matrix_suite!(<module>, <dim>, <DepType>)` — see [`crate::matrix_suite`] in
/// `lib.rs` for the user-facing docs.
///
/// Function-like form (Strategy B, runtime test multiplication). Unlike
/// `tag_suite!` / `sequential_suite!`, the registration needs the dimension's
/// case list at runtime, so we synthesize a `#[cfg(test)]` ctor that:
///   1. calls the `test_r_get_dep_tags_<dim>()` helper emitted by
///      `define_matrix_dimension!` (it must be in scope at the invocation
///      site — typically the same parent module that declared the dimension),
///   2. maps each `(case_label, dep_name, _getter, auto_tag)` tuple to a
///      `test_r::core::MatrixCase` (dropping the per-case getter, since the
///      multiplied test reuses its own compiled getter with the dependency
///      view aliased to the case's tagged dep name), and
///   3. registers a suite-level `Matrix` property keyed by `<module>` and the
///      untagged dep name derived from `<DepType>`.
///
/// `<module>` is referenced by name only, so it may be a file-based module
/// (`mod worker;`) or a directory module (`mod api;` with `api/mod.rs`); no
/// compile-time introspection of the module body is performed.
pub fn matrix_suite(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as MatrixSuiteInput);

    let module_str = args.module.to_string();
    let dim_str = args.dim.to_string();
    let get_dep_tags_fn = Ident::new(&format!("test_r_get_dep_tags_{dim_str}"), Span::call_site());

    let type_path = match &args.dep_type {
        Type::Path(p) => p.clone(),
        Type::Paren(inner) => match &*inner.elem {
            Type::Path(p) => p.clone(),
            _ => return mismatched_dep_type(&args.dep_type),
        },
        _ => return mismatched_dep_type(&args.dep_type),
    };
    let dep_name_str = type_path_to_string(&type_path, DependencyTag::None);
    let dep_name_str_lit = dep_name_str.clone();

    let random = rand::random::<u64>();
    let register_ident = Ident::new(
        &format!("test_r_register_mod_{module_str}_matrix_{random}"),
        Span::call_site(),
    );

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor(crate_path=::test_r::ctor)]
        fn #register_ident() {
            let __cases: Vec<test_r::core::MatrixCase> = #get_dep_tags_fn()
                .into_iter()
                .map(|(__case_label, __dep_name, _getter, __auto_tag)| {
                    test_r::core::MatrixCase {
                        case_label: __case_label,
                        dep_name: __dep_name,
                        auto_tag: __auto_tag,
                    }
                })
                .collect();
            test_r::core::register_suite_matrix(
                #module_str,
                module_path!(),
                #dep_name_str_lit.to_string(),
                __cases,
            );
        }
    };
    result.into()
}

fn mismatched_dep_type(dep_type: &Type) -> TokenStream {
    let msg = format!(
        "matrix_suite expects a concrete dependency type, got `{}`",
        dep_type.to_token_stream()
    );
    syn::Error::new_spanned(dep_type, msg)
        .to_compile_error()
        .into()
}
