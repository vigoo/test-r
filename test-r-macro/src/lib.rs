use darling::ast::NestedMeta;
use darling::{Error, FromMeta};
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::{quote, ToTokens};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    parse2, parse_macro_input, Attribute, Expr, ExprClosure, FnArg, GenericArgument, ItemFn,
    ItemMod, LitStr, Pat, PatType, PathArguments, PathSegment, ReturnType, Token, Type,
    TypeParamBound, TypePath,
};
use test_r_core::internal::ShouldPanic;

#[derive(Debug, Clone)]
enum DependencyTag {
    None,
    Tagged(String),
    Matrix(Ident),
}

impl DependencyTag {
    fn into_iter(self) -> impl Iterator<Item = String> {
        match self {
            DependencyTag::Tagged(tag) => Some(tag),
            _ => None,
        }
        .into_iter()
    }
}

impl From<Option<String>> for DependencyTag {
    fn from(value: Option<String>) -> Self {
        match value {
            Some(tag) => DependencyTag::Tagged(tag),
            None => DependencyTag::None,
        }
    }
}

#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    test_impl(attr, item, false)
}

#[proc_macro_attribute]
pub fn bench(attr: TokenStream, item: TokenStream) -> TokenStream {
    test_impl(attr, item, true)
}

fn test_impl(_attr: TokenStream, item: TokenStream, is_bench: bool) -> TokenStream {
    let mut ast: ItemFn = syn::parse(item).expect("test ast");
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
    let has_timeout = timeout_attr.is_some();

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
    let (dep_getters, _dep_names, dep_dimensions) = get_dependency_params(&ast, is_bench);

    if dep_dimensions.is_empty() {
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
                    test_r::core::TestFunction::Sync(std::sync::Arc::new(|deps| Box::new(#test_name(#(#dep_getters),*))))
                );
            }
        };

        filter_custom_parameter_attributes(&mut ast);
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
        // Dependency matrix, generating a test generator
        let test_name_impl = Ident::new(&format!("{}_impl", test_name), Span::call_site());
        ast.sig.ident = test_name_impl.clone();

        let mut overridden_dep_getters = dep_getters.clone();
        let mut clones = Vec::new();

        for (idx, _dim) in &dep_dimensions {
            let dep_var = Ident::new(&format!("dep_{}", idx), Span::call_site());
            overridden_dep_getters[*idx] = quote! { &#dep_var(deps.clone()) };
            clones.push(quote! {
                let #dep_var = #dep_var.clone();
            });
        }
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
                    test_r::core::TestProperties { test_type: test_r::core::TestType::from_path(file!()), ..Default::default() },
                    move |deps| {
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
                    test_r::core::TestProperties { test_type: test_r::core::TestType::from_path(file!()), ..Default::default() },
                    move |deps| {
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

        filter_custom_parameter_attributes(&mut ast);
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

fn should_panic_message(attr: &Attribute) -> ShouldPanic {
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

struct InheritTestDep {
    attr: Option<Attribute>,
    typ: Type,
}

impl Parse for InheritTestDep {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(Token![#]) {
            let mut attrs = Attribute::parse_outer(input)?;
            if attrs.len() != 1 {
                Err(syn::Error::new(
                    input.span(),
                    "Expected zero or one attribute",
                ))
            } else {
                Ok(InheritTestDep {
                    attr: Some(attrs.pop().unwrap()),
                    typ: input.parse()?,
                })
            }
        } else {
            Ok(InheritTestDep {
                attr: None,
                typ: input.parse()?,
            })
        }
    }
}

#[proc_macro]
pub fn inherit_test_dep(item: TokenStream) -> TokenStream {
    let def: InheritTestDep = parse_macro_input!(item as InheritTestDep);
    let dep_type = match &def.typ {
        Type::Path(path) => path.clone(),
        _ => {
            panic!("Dependency constructor must return a single concrete type")
        }
    };

    let tag_str = def.attr.and_then(|a| get_lit_str_attr(&[a], "tagged_as"));
    let tag = match tag_str {
        Some(tag) => DependencyTag::Tagged(tag),
        None => DependencyTag::None,
    };

    let dep_name_str = type_path_to_string(&dep_type, tag);
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

struct DefineMatrixDimension {
    dim: Ident,
    _colon: Token![:],
    typ: Type,
    _arrow: Token![->],
    tags: Punctuated<LitStr, Token![,]>,
}

impl Parse for DefineMatrixDimension {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(DefineMatrixDimension {
            dim: input.parse()?,
            _colon: input.parse()?,
            typ: input.parse()?,
            _arrow: input.parse()?,
            tags: input.parse_terminated(|i| i.parse::<LitStr>(), Token![,])?,
        })
    }
}

#[proc_macro]
pub fn define_matrix_dimension(item: TokenStream) -> TokenStream {
    let def = parse_macro_input!(item as DefineMatrixDimension);
    let get_dep_tags_fn = Ident::new(
        &format!("test_r_get_dep_tags_{}", def.dim),
        Span::call_site(),
    );
    let typ = def.typ;

    let typ_path = match &typ {
        Type::Path(path) => path,
        _ => {
            panic!("Must use a single concrete type in define_matrix_dimension")
        }
    };

    let mut pushes = Vec::new();

    for tag in def.tags {
        let dep_tag = DependencyTag::Tagged(tag.value());
        let dep_name_str = type_path_to_string(typ_path, dep_tag);
        let getter_ident = Ident::new(
            &format!("test_r_get_dep_{}", dep_name_str),
            Span::call_site(),
        );

        let name = tag.value();
        pushes.push(quote! {
            result.push((#name.to_string(), std::sync::Arc::new(|dependency_view: std::sync::Arc<dyn test_r::core::DependencyView + Send + Sync>| #getter_ident(&dependency_view))));
        });
    }

    let ast = quote! {
        fn #get_dep_tags_fn() -> Vec<(String, std::sync::Arc<dyn (Fn(std::sync::Arc<dyn test_r::core::DependencyView + Send + Sync>) -> std::sync::Arc<#typ>) + Send + Sync + 'static>)> {
            let mut result: Vec<(String, std::sync::Arc<dyn (Fn(std::sync::Arc<dyn test_r::core::DependencyView + Send + Sync>) -> std::sync::Arc<#typ>) + Send + Sync + 'static>)> = Vec::new();
            #(#pushes)*
            result
        }
    };
    ast.into()
}

#[derive(Debug, darling::FromMeta)]
struct TestDepArgs {
    #[darling(default)]
    tagged_as: Option<String>,
}

#[proc_macro_attribute]
pub fn test_dep(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(Error::from(e).write_errors());
        }
    };

    let args = match TestDepArgs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };

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
    let dep_name_str = type_path_to_string(&dep_type, args.tagged_as.into());
    let register_ident = Ident::new(
        &format!("test_r_register_dep_{}", dep_name_str),
        Span::call_site(),
    );

    let is_async = ast.sig.asyncness.is_some();
    let (dep_getters, dep_names, _dep_dimensions) = get_dependency_params(&ast, false);

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

fn type_to_string(typ: &Type, optional_tag: DependencyTag) -> String {
    match typ {
        Type::Array(array) => {
            let inner_type = type_to_string(&array.elem, optional_tag);
            format!("array_{}", inner_type)
        }
        Type::BareFn(_) => {
            panic!("Function pointers are not supported in dependency injection")
        }
        Type::Group(group) => type_to_string(&group.elem, optional_tag),
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
                            .map(|s| segment_to_string(s, DependencyTag::None))
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
        Type::Paren(inner) => type_to_string(&inner.elem, optional_tag),
        Type::Path(path) => type_path_to_string(path, optional_tag),
        Type::Ptr(inner) => {
            let inner_type = type_to_string(&inner.elem, optional_tag);
            format!("ptr_{}", inner_type)
        }
        Type::Reference(inner) => {
            let inner_type = type_to_string(&inner.elem, optional_tag);
            format!("ref_{}", inner_type)
        }
        Type::Slice(inner) => {
            let inner_type = type_to_string(&inner.elem, optional_tag);
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
                            .map(|s| segment_to_string(s, DependencyTag::None))
                            .collect::<Vec<_>>()
                            .join("_"),
                    );
                }
            }
            if let DependencyTag::Tagged(tag) = optional_tag {
                result.push('_');
                result.push_str(&tag);
            }
            result
        }
        Type::Tuple(tuple) => {
            let inner_types = tuple
                .elems
                .iter()
                .map(|t| type_to_string(t, DependencyTag::None))
                .chain(optional_tag.into_iter())
                .collect::<Vec<_>>()
                .join("_");
            format!("tuple_{}", inner_types)
        }
        _ => "".to_string(),
    }
}

fn type_path_to_string(dep_type: &TypePath, optional_tag: DependencyTag) -> String {
    let merged_ident = dep_type
        .path
        .segments
        .iter()
        .map(|s| segment_to_string(s, DependencyTag::None))
        .chain(optional_tag.into_iter())
        .collect::<Vec<_>>()
        .join("_");
    let dep_name = Ident::new(&merged_ident, Span::call_site());
    dep_name.to_string().to_lowercase()
}

fn segment_to_string(segment: &PathSegment, optional_tag: DependencyTag) -> String {
    let mut result = segment.ident.to_string();
    match &segment.arguments {
        PathArguments::None => {}
        PathArguments::AngleBracketed(args) => {
            for arg in &args.args {
                result.push('_');
                result.push_str(&generic_argument_to_string(arg, optional_tag.clone()));
            }
        }
        PathArguments::Parenthesized(_args) => {
            panic!("Parenthesized type arguments are not supported - wrap the type in a newtype")
        }
    }
    result
}

fn generic_argument_to_string(arg: &GenericArgument, optional_tag: DependencyTag) -> String {
    match arg {
        GenericArgument::Type(typ) => type_to_string(typ, optional_tag),
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

/// Removes custom attributes from parameters that are only interpreted by the #[test] macro
fn filter_custom_parameter_attributes(ast: &mut ItemFn) {
    ast.sig.inputs.iter_mut().for_each(|param| {
        if let FnArg::Typed(typed) = param {
            typed.attrs.retain(|attr| {
                !attr.path().is_ident("tagged_as") && !attr.path().is_ident("dimension")
            });
        }
    });
}

fn get_lit_str_attr(attrs: &[Attribute], ident: &str) -> Option<String> {
    attrs
        .iter()
        .find(|attr| attr.path().is_ident(ident))
        .map(|attr| {
            let tag = attr
                .parse_args::<LitStr>()
                .unwrap_or_else(|_| panic!("{ident} attribute's parameter must be a string"));
            tag.value()
        })
}

fn get_ident_attr(attrs: &[Attribute], ident: &str) -> Option<Ident> {
    attrs
        .iter()
        .find(|attr| attr.path().is_ident(ident))
        .map(|attr| {
            attr.parse_args::<Ident>()
                .unwrap_or_else(|_| panic!("{ident} attribute's parameter must be an identifier"))
        })
}

fn get_dependency_params(
    ast: &ItemFn,
    is_bench: bool,
) -> (
    Vec<proc_macro2::TokenStream>,
    Vec<proc_macro2::TokenStream>,
    Vec<(usize, Ident)>,
) {
    let mut dep_getters = Vec::new();
    let mut dep_names = Vec::new();
    let mut dep_dimensions = Vec::new();

    for (idx, param) in ast.sig.inputs.iter().enumerate() {
        if !is_bench || idx > 0 {
            // TODO: verify that the first bench arg is a Bencher/AsyncBencher
            let (dep_type, tag) = match param {
                FnArg::Receiver(_) => {
                    panic!("Test functions cannot have a self parameter")
                }
                FnArg::Typed(typ) => {
                    let tag_str = get_lit_str_attr(&typ.attrs, "tagged_as");
                    let dim_str = get_ident_attr(&typ.attrs, "dimension");

                    let dep_tag = match (tag_str, dim_str) {
                        (Some(tag), None) => DependencyTag::Tagged(tag),
                        (None, Some(dim)) => DependencyTag::Matrix(dim),
                        (Some(_), Some(_)) => panic!("Cannot have both a tag and a dimension attribute on the same test parameter"),
                        (None, None) => DependencyTag::None,
                    };

                    if let DependencyTag::Matrix(dim) = &dep_tag {
                        dep_dimensions.push((idx, dim.clone()));
                    }

                    let typ = get_dependency_param_from_pat_type(typ);
                    (typ, dep_tag)
                }
            };

            let dep_name_str = type_path_to_string(&dep_type, tag);
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
    (dep_getters, dep_names, dep_dimensions)
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
        let (dep_type, tag) = match pat {
            Pat::Type(typ) => {
                let optional_tag = match get_lit_str_attr(&typ.attrs, "tagged_as") {
                    Some(tag) => DependencyTag::Tagged(tag),
                    None => DependencyTag::None,
                };
                (get_dependency_param_from_pat_type(typ), optional_tag)
            }
            _ => {
                panic!("Test functions can only have parameters which are immutable references to concrete types, but got {:?}", pat.to_token_stream())
                // TODO: nicer error report
            }
        };
        let dep_name_str = type_path_to_string(&dep_type, tag);
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
