use crate::deps::get_dependency_params;
use crate::helpers::is_testr_attribute;
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Attribute, FnArg, ItemFn, LitStr, Token};
use test_r_core::internal::ShouldPanic;

pub fn test_impl(_attr: TokenStream, item: TokenStream, is_bench: bool) -> TokenStream {
    let mut ast: ItemFn = syn::parse(item).expect("test ast");
    let test_name = ast.sig.ident.clone();
    let test_name_str = test_name.to_string();

    let is_ignored = ast
        .attrs
        .iter()
        .any(|attr| is_testr_attribute(attr, "ignore"));
    let should_panic = ast
        .attrs
        .iter()
        .find(|attr| is_testr_attribute(attr, "should_panic"))
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
        .find(|attr| is_testr_attribute(attr, "timeout"));
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
    let has_timeout = timeout_attr.is_some();

    let flaky_attr = ast
        .attrs
        .iter()
        .find(|attr| is_testr_attribute(attr, "flaky"));
    let non_flaky_attr = ast
        .attrs
        .iter()
        .find(|attr| is_testr_attribute(attr, "non_flaky"));
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
        .filter(|attr| is_testr_attribute(attr, "tag"))
        .map(|attr| {
            let tag = attr
                .parse_args::<Ident>()
                .expect("tag attribute's parameter must be a identifier");
            let tag_str = tag.to_string();
            quote! { #tag_str.to_string() }
        });
    let tags = quote! { vec![#(#tag_attrs),*] };

    let is_async = ast.sig.asyncness.is_some();
    let (dep_getters, _dep_names, dep_dimensions) = get_dependency_params(&ast, is_bench);

    let details = TestDetails {
        test_name,
        test_name_str,
        is_bench,
        is_async,
        is_ignored,
        should_panic,
        timeout,
        has_timeout,
        flakiness_control,
        capture_control,
        report_time_control,
        ensure_time_control,
        tags,
        dep_getters,
    };

    if dep_dimensions.is_empty() {
        single_test_impl(&mut ast, details)
    } else {
        matrix_test_impl(&mut ast, details, dep_dimensions)
    }
}

struct TestDetails {
    test_name: Ident,
    test_name_str: String,
    is_bench: bool,
    is_async: bool,
    is_ignored: bool,
    should_panic: proc_macro2::TokenStream,
    timeout: proc_macro2::TokenStream,
    has_timeout: bool,
    flakiness_control: proc_macro2::TokenStream,
    capture_control: proc_macro2::TokenStream,
    report_time_control: proc_macro2::TokenStream,
    ensure_time_control: proc_macro2::TokenStream,
    tags: proc_macro2::TokenStream,
    dep_getters: Vec<proc_macro2::TokenStream>,
}

fn single_test_impl(ast: &mut ItemFn, details: TestDetails) -> TokenStream {
    let TestDetails {
        test_name,
        test_name_str,
        is_bench,
        is_async,
        is_ignored,
        should_panic,
        timeout,
        has_timeout,
        flakiness_control,
        capture_control,
        report_time_control,
        ensure_time_control,
        tags,
        dep_getters,
    } = details;

    let register_ident = Ident::new(
        &format!("test_r_register_{}", test_name_str),
        test_name.span(),
    );

    let register_call = if is_bench {
        if has_timeout {
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
                      test_r::core::TestFunction::AsyncBench(std::sync::Arc::new(|__test_r_bencher_arg, __test_r_deps_arg| Box::pin(async move { #test_name(__test_r_bencher_arg, #(#dep_getters),*).await })))
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
                    test_r::core::TestFunction::SyncBench(std::sync::Arc::new(|__test_r_bencher_arg, __test_r_deps_arg| #test_name(__test_r_bencher_arg, #(#dep_getters),*)))
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
                    move |__test_r_deps_arg| {
                        Box::pin(async move {
                            let result = #test_name(#(#dep_getters),*).await;
                            Box::new(result) as Box<dyn test_r::core::TestReturnValue>
                        })
                    }
                ))
              );
        }
    } else {
        if has_timeout {
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
                test_r::core::TestFunction::Sync(std::sync::Arc::new(|__test_r_deps_arg| Box::new(#test_name(#(#dep_getters),*))))
            );
        }
    };

    filter_custom_parameter_attributes(ast);
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

fn matrix_test_impl(
    ast: &mut ItemFn,
    details: TestDetails,
    dep_dimensions: Vec<(usize, Ident)>,
) -> TokenStream {
    // Dependency matrix, generating a test generator

    let TestDetails {
        test_name,
        test_name_str,
        is_bench,
        is_async,
        is_ignored,
        should_panic,
        timeout,
        has_timeout,
        flakiness_control,
        capture_control,
        report_time_control,
        ensure_time_control,
        tags,
        dep_getters,
    } = details;

    if is_bench {
        panic!("Matrix dependencies are not supported for benchmarks yet");
    }

    let test_name_impl = Ident::new(&format!("{}_impl", test_name), Span::call_site());
    ast.sig.ident = test_name_impl.clone();

    let mut overridden_dep_getters = dep_getters.clone();
    let mut clones = Vec::new();

    for (idx, _dim) in &dep_dimensions {
        let dep_var = Ident::new(&format!("dep_{}", idx), Span::call_site());
        overridden_dep_getters[*idx] = quote! { &#dep_var(__test_r_deps_arg.clone()) };
        clones.push(quote! {
            let #dep_var = #dep_var.clone();
        });
    }

    let test_props = {
        let mut props = Vec::new();

        props.push(quote! { should_panic: #should_panic });
        if has_timeout {
            props.push(quote! { timeout: #timeout });
        } else {
            props.push(quote! { timeout: None });
        }
        props.push(quote! { flakiness_control: #flakiness_control });
        props.push(quote! { capture_control: #capture_control });
        props.push(quote! { report_time_control: #report_time_control });
        props.push(quote! { ensure_time_control: #ensure_time_control });
        props.push(quote! { tags: #tags });
        props.push(quote! { is_ignored: #is_ignored });

        props
    };

    let mut loops = if is_async {
        quote! {
            let mut tags_as_string = String::new();
            for name in &name_stack {
                tags_as_string.push_str("_");
                tags_as_string.push_str(name);
            }
            #(#clones)*
            r.add_async_test(
                format!("{}{}", #test_name_str, tags_as_string),
                test_r::core::TestProperties {
                    test_type: test_r::core::TestType::from_path(file!()),
                    #(#test_props),*
                },
                move |__test_r_deps_arg| {
                    #(#clones)*
                    Box::pin(async move {
                        #test_name_impl(#(#overridden_dep_getters),*).await
                    })
                },
            );
        }
    } else {
        quote! {
            let mut tags_as_string = String::new();
            for name in &name_stack {
                tags_as_string.push_str("_");
                tags_as_string.push_str(name);
            }
            #(#clones)*
            r.add_sync_test(
                format!("{}{}", #test_name_str, tags_as_string),
                test_r::core::TestProperties {
                    test_type: test_r::core::TestType::from_path(file!()),
                    #(#test_props),*
                },
                move |__test_r_deps_arg| {
                    #test_name_impl(#(#overridden_dep_getters),*)
                },
            );
        }
    };

    for (idx, dim) in dep_dimensions {
        let dep_name_var = Ident::new(&format!("tag_{}", idx), Span::call_site());
        let dep_var = Ident::new(&format!("dep_{}", idx), Span::call_site());
        let get_dep_tags_fn =
            Ident::new(&format!("test_r_get_dep_tags_{}", dim), Span::call_site());
        loops = quote! {
            for (#dep_name_var, #dep_var) in #get_dep_tags_fn() {
                name_stack.push(#dep_name_var);
                #loops
                name_stack.pop();
            }
        };
    }

    filter_custom_parameter_attributes(ast);
    let result = quote! {
        #[test_r::test_gen]
        fn #test_name(r: &mut test_r::core::DynamicTestRegistration) {
            let mut name_stack = Vec::new();
            #loops
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
    let on_attr = ast
        .attrs
        .iter()
        .find(|attr| is_testr_attribute(attr, on_name));
    let off_attr = ast
        .attrs
        .iter()
        .find(|attr| is_testr_attribute(attr, off_name));
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

fn should_panic_message(attr: &Attribute) -> ShouldPanic {
    let args: ShouldPanicArgs = attr
        .parse_args()
        .unwrap_or(ShouldPanicArgs { expected: None });
    match args.expected {
        Some(message) => ShouldPanic::WithMessage(message.value()),
        None => ShouldPanic::Yes,
    }
}

/// Removes custom attributes from parameters that are only interpreted by the #[test] macro
fn filter_custom_parameter_attributes(ast: &mut ItemFn) {
    ast.sig.inputs.iter_mut().for_each(|param| {
        if let FnArg::Typed(typed) = param {
            typed.attrs.retain(|attr| {
                !is_testr_attribute(attr, "tagged_as") && !is_testr_attribute(attr, "dimension")
            });
        }
    });
}
