use crate::helpers::is_testr_attribute;
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::ToTokens;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Expr, ItemMod, LitInt, LitStr, Token, parse_macro_input};
use syn::{FnArg, Item, ItemFn, Type, parse_quote};

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

/// Parsed arguments of `#[matrix_suite(<dim>, <DepType>)]`.
struct MatrixSuiteArgs {
    dim: Ident,
    dep_type: Type,
}

impl syn::parse::Parse for MatrixSuiteArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let dim: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let dep_type: Type = input.parse()?;
        Ok(MatrixSuiteArgs { dim, dep_type })
    }
}

/// Render a path type to a stable comparison key. This is used to match a
/// `#[test]` fn's `&<DepType>` parameter against the `<DepType>` named in the
/// `#[matrix_suite(...)]` attribute, so we compare the path types as written by
/// the user (case-sensitive, full path, including generic arguments).
fn type_path_key(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => Some(p.to_token_stream().to_string()),
        Type::Paren(inner) => type_path_key(&inner.elem),
        _ => None,
    }
}

/// If `ty` is `&T` where `T` is a path type, return its path key. Returns
/// `None` for by-value parameters, mutable references, and anything more exotic
/// (references to slices, tuples, etc.) which the matrix machinery does not
/// support anyway.
fn param_type_key(ty: &Type) -> Option<String> {
    match ty {
        Type::Reference(r) if r.mutability.is_none() => type_path_key(&r.elem),
        _ => None,
    }
}

/// `#[matrix_suite(<dim>, <DepType>)]` — see [`crate::matrix_suite`] in
/// `lib.rs` for the user-facing docs.
///
/// Strategy A (compile-time module rewrite): walk the annotated module's
/// items, find each `#[test] fn` whose signature has a `&<DepType>` parameter
/// without an existing `#[dimension]` / `#[tagged_as]`, and inject
/// `#[dimension(<dim>)]` onto that parameter. The inner `#[test]` macro then
/// expands each such fn through the existing per-function `matrix_test_impl`,
/// multiplying it into one test per case and — courtesy of Feature 1 —
/// auto-tagging each generated case with `<dim>_<case>`.
///
/// We choose Strategy A over the runtime-multiplication alternative because
/// the per-function matrix generator is already the single, well-tested
/// expansion point that knows how to swap a dimension parameter's dep getter
/// per case. Duplicating that logic at runtime would require re-resolving the
/// case-specific tagged dep inside the (already-compiled) test closure, which
/// is not possible without macro support. Keeping it compile-time also lets
/// `matrix_suite` compose with Feature 1's auto-tags for free.
///
/// Note on syntax: this is an **attribute** macro on the inline module, not a
/// function-like `matrix_suite!(name, dim)` invocation referencing a separate
/// module item. A function-like macro that only receives the module's name
/// cannot rewrite the module's test fns at expansion time (Rust macros
/// cannot introspect sibling items), so the post-hoc function-like form used
/// by `tag_suite!` / `sequential_suite!` is not available here.
pub fn matrix_suite(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as MatrixSuiteArgs);
    let dim = args.dim;
    let target_key = match type_path_key(&args.dep_type) {
        Some(key) => key,
        None => {
            let msg = format!(
                "matrix_suite expects a concrete dependency type, got `{}`",
                args.dep_type.to_token_stream()
            );
            return syn::Error::new_spanned(&args.dep_type, msg)
                .to_compile_error()
                .into();
        }
    };

    let mut item_mod: ItemMod = parse_macro_input!(item as ItemMod);

    let content = match &mut item_mod.content {
        Some(content) => content,
        None => {
            return syn::Error::new(
                item_mod.span(),
                "matrix_suite must be applied to an inline module (`mod x { ... }`), \
                 not a non-inlined module (`mod x;`)",
            )
            .to_compile_error()
            .into();
        }
    };

    for item in content.1.iter_mut() {
        let item_fn = match item {
            Item::Fn(f) => f,
            _ => continue,
        };

        let is_test = item_fn.attrs.iter().any(|a| is_testr_attribute(a, "test"));
        let is_bench = item_fn.attrs.iter().any(|a| is_testr_attribute(a, "bench"));
        // Matrix expansion for benches is not supported (see matrix_test_impl),
        // and a fn that is not a #[test] at all is left entirely alone.
        if !is_test || is_bench {
            continue;
        }

        inject_dimension_attrs(item_fn, &dim, &target_key);
    }

    quote! { #item_mod }.into()
}

/// Inject `#[dimension(<dim>)]` onto every `&<target_key>` parameter of `fn`
/// that does not already carry a `#[dimension]` or `#[tagged_as]` attribute.
/// Parameters of any other type, and parameters already tagged/dimensioned,
/// are left untouched.
fn inject_dimension_attrs(item_fn: &mut ItemFn, dim: &Ident, target_key: &str) {
    // Only rewrite if the function has at least one matching parameter; we
    // touch the signature in place so the inner `#[test]` macro sees the
    // injected `#[dimension(...)]` helper attribute when it expands.
    for fn_arg in item_fn.sig.inputs.iter_mut() {
        let pat_type = match fn_arg {
            FnArg::Typed(t) => t,
            _ => continue,
        };

        let already_marked = pat_type
            .attrs
            .iter()
            .any(|a| is_testr_attribute(a, "dimension") || is_testr_attribute(a, "tagged_as"));
        if already_marked {
            continue;
        }

        let Some(param_key) = param_type_key(&pat_type.ty) else {
            continue;
        };
        if param_key != target_key {
            continue;
        }

        let dim_attr: syn::Attribute = parse_quote!(#[dimension(#dim)]);
        pat_type.attrs.push(dim_attr);
    }
}
