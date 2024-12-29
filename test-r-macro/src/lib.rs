use proc_macro::TokenStream;

use proc_macro2::{Ident, Span};
use quote::{quote, ToTokens};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    parse2, parse_macro_input, Expr, ExprClosure, FnArg, GenericArgument, ItemFn, ItemMod, LitStr,
    Pat, PatType, PathArguments, PathSegment, ReturnType, Token, Type, TypeParamBound, TypePath,
};
use test_r_core::internal::ShouldPanic;

#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    test_impl(attr, item, false)
}

#[proc_macro_attribute]
pub fn bench(attr: TokenStream, item: TokenStream) -> TokenStream {
    test_impl(attr, item, true)
}

fn test_impl(_attr: TokenStream, item: TokenStream, is_bench: bool) -> TokenStream {
    let ast: ItemFn = syn::parse(item).expect("test ast");
    let test_name = ast.sig.ident.clone();
    let test_name_str = test_name.to_string();

    let is_ignored = ast.attrs.iter().any(|attr| attr.path().is_ident("ignore"));
    let should_panic = ast
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("should_panic"))
        .map(should_panic_message)
        .unwrap_or(ShouldPanic::No);

    let should_panic = match should_panic {
        ShouldPanic::No => quote! { test_r::core::ShouldPanic::No },
        ShouldPanic::Yes => quote! { test_r::core::ShouldPanic::Yes },
        ShouldPanic::WithMessage(message) => {
            quote! { test_r::core::ShouldPanic::WithMessage(#message.to_string()) }
        }
    };

    let timeout_attr = ast
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("timeout"));
    let timeout = timeout_attr
        .map(|attr| {
            let timeout = attr
                .parse_args::<syn::LitInt>()
                .expect("timeout attribute's parameter must be an integer (timeout milliseconds)");
            let timeout = timeout
                .base10_parse::<u64>()
                .expect("timeout attribute's parameter must be an integer (timeout milliseconds)");
            quote! { Some(std::time::Duration::from_millis(#timeout)) }
        })
        .unwrap_or(quote! { None });

    let flaky_attr = ast.attrs.iter().find(|attr| attr.path().is_ident("flaky"));
    let non_flaky_attr = ast
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("non_flaky"));
    let flakiness_control = match (flaky_attr, non_flaky_attr) {
        (None, None) => quote! { test_r::core::FlakinessControl::None },
        (Some(_), Some(_)) => {
            panic!("Cannot have both #[flaky] and #[non_flaky] attributes")
        }
        (Some(attr), None) => {
            let n = attr
                .parse_args::<syn::LitInt>()
                .expect("flaky attribute's parameter must be an integer (max number of retries)");
            let n = n
                .base10_parse::<usize>()
                .expect("flaky attribute's parameter must be an integer (max number of retries)");
            quote! { test_r::core::FlakinessControl::RetryKnownFlaky(#n) }
        }
        (None, Some(attr)) => {
            let n = attr
                .parse_args::<syn::LitInt>()
                .expect("non_flaky attribute's parameter must be an integer (number of tries)");
            let n = n
                .base10_parse::<usize>()
                .expect("non_flaky attribute's parameter must be an integer (number of tries)");
            quote! { test_r::core::FlakinessControl::ProveNonFlaky(#n) }
        }
    };

    let capture_control = from_three_state_attrs(
        &ast,
        quote! { test_r::core::CaptureControl::Default },
        "always_capture",
        quote! { test_r::core::CaptureControl::AlwaysCapture },
        "never_capture",
        quote! { test_r::core::CaptureControl::NeverCapture },
    );
    let report_time_control = from_three_state_attrs(
        &ast,
        quote! { test_r::core::ReportTimeControl::Default },
        "always_report_time",
        quote! { test_r::core::ReportTimeControl::Enabled },
        "never_report_time",
        quote! { test_r::core::ReportTimeControl::Disabled },
    );
    let ensure_time_control = from_three_state_attrs(
        &ast,
        quote! { test_r::core::ReportTimeControl::Default },
        "always_ensure_time",
        quote! { test_r::core::ReportTimeControl::Enabled },
        "never_ensure_time",
        quote! { test_r::core::ReportTimeControl::Disabled },
    );

    let tag_attrs = ast
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("tag"))
        .map(|attr| {
            let tag = attr
                .parse_args::<Ident>()
                .expect("tag attribute's parameter must be a identifier");
            let tag_str = tag.to_string();
            quote! { #tag_str.to_string() }
        });
    let tags = quote! { vec![#(#tag_attrs),*] };

    let register_ident = Ident::new(
        &format!("test_r_register_{}", test_name_str),
        test_name.span(),
    );

    let is_async = ast.sig.asyncness.is_some();
    let (dep_getters, _dep_names) = get_dependency_params(&ast, is_bench);

    let register_call = if is_bench {
        if timeout_attr.is_some() {
            panic!("Benchmarks cannot have a timeout attribute")
        }

        if is_async {
            quote! {
                  test_r::core::register_test(
                      #test_name_str,
                      module_path!(),
                      #is_ignored,
                      #should_panic,
                      test_r::core::TestType::from_path(file!()),
                      None,
                      test_r::core::FlakinessControl::None,
                      #capture_control,
                      #tags,
                      #report_time_control,
                      #ensure_time_control,
                      test_r::core::TestFunction::AsyncBench(std::sync::Arc::new(|bencher, deps| Box::pin(async move { #test_name(bencher, #(#dep_getters),*).await })))
                  );
            }
        } else {
            quote! {
                test_r::core::register_test(
                    #test_name_str,
                    module_path!(),
                    #is_ignored,
                    #should_panic,
                    test_r::core::TestType::from_path(file!()),
                    None,
                    test_r::core::FlakinessControl::None,
                    #capture_control,
                    #tags,
                    #report_time_control,
                    #ensure_time_control,
                    test_r::core::TestFunction::SyncBench(std::sync::Arc::new(|bencher, deps| #test_name(bencher, #(#dep_getters),*)))
                );
            }
        }
    } else if is_async {
        quote! {
              test_r::core::register_test(
                  #test_name_str,
                  module_path!(),
                  #is_ignored,
                  #should_panic,
                  test_r::core::TestType::from_path(file!()),
                  #timeout,
                  #flakiness_control,
                  #capture_control,
                  #tags,
                  #report_time_control,
                  #ensure_time_control,
                  test_r::core::TestFunction::Async(std::sync::Arc::new(
                    move |deps| {
                        Box::pin(async move {
                            let result = #test_name(#(#dep_getters),*).await;
                            Box::new(result) as Box<dyn test_r::core::TestReturnValue>
                        })
                    }
                ))
              );
        }
    } else {
        if timeout_attr.is_some() {
            panic!("The #[timeout()] attribute is only supported for async tests");
        }

        quote! {
            test_r::core::register_test(
                #test_name_str,
                module_path!(),
                #is_ignored,
                #should_panic,
                test_r::core::TestType::from_path(file!()),
                None,
                #flakiness_control,
                #capture_control,
                #tags,
                #report_time_control,
                #ensure_time_control,
                test_r::core::TestFunction::Sync(std::sync::Arc::new(|deps| Box::new(#test_name(#(#dep_getters),*))))
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

fn from_three_state_attrs(
    ast: &ItemFn,
    default: proc_macro2::TokenStream,
    on_name: &str,
    on_value: proc_macro2::TokenStream,
    off_name: &str,
    off_value: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let on_attr = ast.attrs.iter().find(|attr| attr.path().is_ident(on_name));
    let off_attr = ast.attrs.iter().find(|attr| attr.path().is_ident(off_name));
    match (on_attr, off_attr) {
        (None, None) => default,
        (Some(_), Some(_)) => {
            panic!("Cannot have both #[{on_name}] and #[{off_name}] attributes")
        }
        (Some(_), None) => on_value,
        (None, Some(_)) => off_value,
    }
}

struct ShouldPanicArgs {
    pub expected: Option<LitStr>,
}

impl Parse for ShouldPanicArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        fn try_parse(input: ParseStream) -> syn::Result<Option<LitStr>> {
            let key: Ident = input.parse()?;
            if key != "expected" {
                return Err(syn::Error::new(key.span(), "Expected `expected`"));
            }
            input.parse::<Token![=]>()?;
            let message: LitStr = input.parse()?;
            Ok(Some(message))
        }

        let expected = try_parse(input).ok().flatten();
        Ok(ShouldPanicArgs { expected })
    }
}

fn should_panic_message(attr: &syn::Attribute) -> ShouldPanic {
    let args: ShouldPanicArgs = attr
        .parse_args()
        .unwrap_or(ShouldPanicArgs { expected: None });
    match args.expected {
        Some(message) => ShouldPanic::WithMessage(message.value()),
        None => ShouldPanic::Yes,
    }
}

#[proc_macro]
pub fn uses_test_r(_item: TokenStream) -> TokenStream {
    r#"
        #[cfg(test)]
        pub fn main() -> std::process::ExitCode {
            test_r::core::test_runner()
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
    let dep_name_str = type_path_to_string(&dep_type);
    let getter_ident = Ident::new(
        &format!("test_r_get_dep_{}", dep_name_str),
        Span::call_site(),
    );

    let result = quote! {
        fn #getter_ident<'a>(dependency_view: &'a impl test_r::core::DependencyView) -> std::sync::Arc<#dep_type> {
            super::#getter_ident(dependency_view)
        }
    };

    result.into()
}

#[proc_macro_attribute]
pub fn test_dep(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let ast: ItemFn = syn::parse(item).expect("test ast");
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
    let dep_name_str = type_path_to_string(&dep_type);
    let register_ident = Ident::new(
        &format!("test_r_register_{}", dep_name_str),
        Span::call_site(),
    );

    let is_async = ast.sig.asyncness.is_some();
    let (dep_getters, dep_names) = get_dependency_params(&ast, false);

    let register_call = if is_async {
        quote! {
              test_r::core::register_dependency_constructor(
                  #dep_name_str,
                  module_path!(),
                  test_r::core::DependencyConstructor::Async(std::sync::Arc::new(|deps| Box::pin(async move {
                    let result: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(#ctor_name(#(#dep_getters),*).await);
                    result
                  }))),
                 vec![#(#dep_names),*]
              );
        }
    } else {
        quote! {
            test_r::core::register_dependency_constructor(
                #dep_name_str,
                module_path!(),
                test_r::core::DependencyConstructor::Sync(std::sync::Arc::new(|deps| std::sync::Arc::new(#ctor_name(#(#dep_getters),*)))),
                vec![#(#dep_names),*]
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
        fn #getter_ident<'a>(dependency_view: &'a impl test_r::core::DependencyView) -> std::sync::Arc<#dep_type> {
            #getter_body
        }

        #ast
    };

    result.into()
}

#[proc_macro_attribute]
pub fn test_gen(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let ast: ItemFn = syn::parse(item).expect("test generator ast");
    let generator_name = ast.sig.ident.clone();
    let generator_name_str = generator_name.to_string();

    let is_ignored = ast.attrs.iter().any(|attr| attr.path().is_ident("ignore"));

    let register_ident = Ident::new(
        &format!("test_r_register_generator_{}", generator_name_str),
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
                let mut tests = DynamicTestRegistration::new();
                #ast
                #generator_name(&mut tests).await;
                tests.to_vec()
            }
        }
    } else {
        quote! {
            fn #generator_name() -> Vec<test_r::core::GeneratedTest> {
                let mut tests = DynamicTestRegistration::new();
                #ast
                #generator_name(&mut tests);
                tests.to_vec()
            }
        }
    };

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor]
        fn #register_ident() {
             #register_call
        }

        #wrapped_ast
    };

    result.into()
}

#[proc_macro_attribute]
pub fn sequential(_attr: TokenStream, item: TokenStream) -> TokenStream {
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
        #[test_r::ctor::ctor]
        fn #register_ident() {
             #register_call
        }

        #ast
    };

    result.into()
}

#[proc_macro]
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
            #dtr_expr.add_async_test(#name_expr, #test_props_expr, move |deps| {
                Box::pin(async move {
                    #(#lets)*
                    #body
                })
            });
        }
    } else {
        quote! {
            #dtr_expr.add_sync_test(#name_expr, #test_props_expr, move |deps| {
                let gen = #function_closure;
                gen(#(#dep_getters),*)
            });
        }
    };

    result.into()
}

#[proc_macro_attribute]
pub fn timeout(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn flaky(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn non_flaky(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn always_capture(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn never_capture(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn always_report_time(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn never_report_time(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn always_ensure_time(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn never_ensure_time(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
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
            #[test_r::ctor::ctor]
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

#[proc_macro]
pub fn tag_suite(input: TokenStream) -> TokenStream {
    let params = parse_macro_input!(input with Punctuated::<Ident, Token![,]>::parse_terminated);

    if params.len() != 2 {
        panic!("tag_suite! expects exactly 2 identifiers as parameters: the name of the suite module and the tag");
    }

    let mod_name_str = params[0].to_string();
    let tag_str = params[1].to_string();

    let random = rand::random::<u64>();
    let register_ident = Ident::new(
        &format!("test_r_register_mod_{}_tag_{}", mod_name_str, random),
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
        #[test_r::ctor::ctor]
        fn #register_ident() {
             #register_call
        }
    };

    result.into()
}

#[proc_macro]
pub fn sequential_suite(input: TokenStream) -> TokenStream {
    let params = parse_macro_input!(input with Punctuated::<Ident, Token![,]>::parse_terminated);

    if params.len() != 1 {
        panic!("sequential_suite! expects exactly 1 identifier as parameter: the name of the suite module");
    }

    let mod_name_str = params[0].to_string();

    let register_ident = Ident::new(
        &format!("test_r_register_mod_{}_sequential", mod_name_str),
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
        #[test_r::ctor::ctor]
        fn #register_ident() {
             #register_call
        }
    };

    result.into()
}

fn type_to_string(typ: &Type) -> String {
    match typ {
        Type::Array(array) => {
            let inner_type = type_to_string(&array.elem);
            format!("array_{}", inner_type)
        }
        Type::BareFn(_) => {
            panic!("Function pointers are not supported in dependency injection")
        }
        Type::Group(group) => type_to_string(&group.elem),
        Type::ImplTrait(impltrait) => {
            let mut result = "impl".to_string();
            for bound in &impltrait.bounds {
                if let TypeParamBound::Trait(trait_bound) = bound {
                    result.push('_');
                    result.push_str(
                        &trait_bound
                            .path
                            .segments
                            .iter()
                            .map(segment_to_string)
                            .collect::<Vec<_>>()
                            .join("_"),
                    );
                }
            }
            result
        }
        Type::Infer(_) => {
            panic!("Type inference is not supported in dependency injection type signatures")
        }
        Type::Macro(_) => {
            panic!("Macro invocations are not supported in dependency injection type signatures")
        }
        Type::Never(_) => "never".to_string(),
        Type::Paren(inner) => type_to_string(&inner.elem),
        Type::Path(path) => type_path_to_string(path),
        Type::Ptr(inner) => {
            let inner_type = type_to_string(&inner.elem);
            format!("ptr_{}", inner_type)
        }
        Type::Reference(inner) => {
            let inner_type = type_to_string(&inner.elem);
            format!("ref_{}", inner_type)
        }
        Type::Slice(inner) => {
            let inner_type = type_to_string(&inner.elem);
            format!("slice_{}", inner_type)
        }
        Type::TraitObject(to) => {
            let mut result = "dyn".to_string();
            for bound in &to.bounds {
                if let TypeParamBound::Trait(trait_bound) = bound {
                    result.push('_');
                    result.push_str(
                        &trait_bound
                            .path
                            .segments
                            .iter()
                            .map(segment_to_string)
                            .collect::<Vec<_>>()
                            .join("_"),
                    );
                }
            }
            result
        }
        Type::Tuple(tuple) => {
            let inner_types = tuple
                .elems
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join("_");
            format!("tuple_{}", inner_types)
        }
        _ => "".to_string(),
    }
}

fn type_path_to_string(dep_type: &TypePath) -> String {
    let merged_ident = dep_type
        .path
        .segments
        .iter()
        .map(segment_to_string)
        .collect::<Vec<_>>()
        .join("_");
    let dep_name = Ident::new(&merged_ident, Span::call_site());
    dep_name.to_string().to_lowercase()
}

fn segment_to_string(segment: &PathSegment) -> String {
    let mut result = segment.ident.to_string();
    match &segment.arguments {
        PathArguments::None => {}
        PathArguments::AngleBracketed(args) => {
            for arg in &args.args {
                result.push('_');
                result.push_str(&generic_argument_to_string(arg));
            }
        }
        PathArguments::Parenthesized(_args) => {
            panic!("Parenthesized type arguments are not supported - wrap the type in a newtype")
        }
    }
    result
}

fn generic_argument_to_string(arg: &GenericArgument) -> String {
    match arg {
        GenericArgument::Type(typ) => type_to_string(typ),
        GenericArgument::Const(_) => {
            panic!("Const generics are not supported in dependency injection")
        }
        GenericArgument::AssocType(_) => {
            panic!("Associated types are not supported in dependency injection")
        }
        GenericArgument::AssocConst(_) => {
            panic!("Associated constants are not supported in dependency injection")
        }
        GenericArgument::Constraint(_) => {
            panic!("Constraints are not supported in dependency injection; introduce a newtype")
        }
        _ => "".to_string(),
    }
}

fn get_dependency_params(
    ast: &ItemFn,
    is_bench: bool,
) -> (Vec<proc_macro2::TokenStream>, Vec<proc_macro2::TokenStream>) {
    let mut dep_getters = Vec::new();
    let mut dep_names = Vec::new();

    for (idx, param) in ast.sig.inputs.iter().enumerate() {
        if !is_bench || idx > 0 {
            // TODO: verify that the first bench arg is a Bencher/AsyncBencher
            let dep_type = match param {
                FnArg::Receiver(_) => {
                    panic!("Test functions cannot have a self parameter")
                }
                FnArg::Typed(typ) => get_dependency_param_from_pat_type(typ),
            };
            let dep_name_str = type_path_to_string(&dep_type);

            let getter_ident = Ident::new(
                &format!("test_r_get_dep_{}", dep_name_str),
                Span::call_site(),
            );

            dep_getters.push(quote! {
                &#getter_ident(&deps)
            });
            dep_names.push(quote! {
                #dep_name_str.to_string()
            });
        }
    }
    (dep_getters, dep_names)
}

fn get_dependency_params_for_closure<'a>(
    ast: impl Iterator<Item = &'a Pat>,
) -> (
    Vec<proc_macro2::TokenStream>,
    Vec<proc_macro2::TokenStream>,
    Vec<Ident>,
) {
    let mut dep_getters = Vec::new();
    let mut dep_names = Vec::new();
    let mut bindings = Vec::new();
    for pat in ast {
        let dep_type = match pat {
            Pat::Type(typ) => get_dependency_param_from_pat_type(typ),
            _ => {
                panic!("Test functions can only have parameters which are immutable references to concrete types, but got {:?}", pat.to_token_stream())
                // TODO: nicer error report
            }
        };
        let dep_name_str = type_path_to_string(&dep_type);
        let binding = match pat {
            Pat::Type(typ) => match &*typ.pat {
                Pat::Ident(ident) => ident.ident.clone(),
                _ => {
                    panic!("Test functions can only have parameters which are immutable references to concrete types, but got {:?}", typ.pat.to_token_stream())
                    // TODO: nicer error report
                }
            },
            _ => {
                panic!("Test functions can only have parameters which are immutable references to concrete types, but got {:?}", pat.to_token_stream())
                // TODO: nicer error report
            }
        };

        let getter_ident = Ident::new(
            &format!("test_r_get_dep_{}", dep_name_str),
            Span::call_site(),
        );

        dep_getters.push(quote! {
            &#getter_ident(&deps)
        });
        dep_names.push(quote! {
            #dep_name_str.to_string()
        });
        bindings.push(binding);
    }
    (dep_getters, dep_names, bindings)
}

fn get_dependency_param_from_pat_type(typ: &PatType) -> TypePath {
    match &*typ.ty {
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
    }
}
