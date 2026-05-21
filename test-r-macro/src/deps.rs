use crate::helpers::{filter_custom_parameter_attributes, is_testr_attribute};
use darling::ast::NestedMeta;
use darling::{Error, FromMeta};
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::{ToTokens, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, FnArg, GenericArgument, ItemFn, LitStr, Pat, PatType, Path, PathArguments,
    PathSegment, ReturnType, Token, Type, TypeParamBound, TypePath, parse_macro_input,
};

/// Sharing strategy declared via `#[test_dep(scope = ...)]`. Parsed from
/// `darling` metadata; mirrors `test_r_core::internal::DepScope` but lives in
/// the proc-macro crate to avoid a runtime dependency.
///
/// We accept both bare identifiers (`scope = PerWorker`) and string literals
/// (`scope = "PerWorker"`), via a custom `FromMeta` implementation. The bare
/// identifier form is the documented surface; the string form remains for
/// users who prefer it or whose tooling otherwise rewrites attribute values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Scope {
    #[default]
    Shared,
    PerWorker,
    Cloneable,
    Hosted,
    /// Phase 1C: owner held in parent, workers call back over IPC via a
    /// generated stub (manually-written for MVP). Requires the dep return
    /// type to implement `HostedRpcDep` and a `stub = StubType` attribute
    /// on the macro that names the worker-visible handle type tests
    /// parameterise on.
    HostedRpc,
}

impl Scope {
    fn as_tokens(self) -> proc_macro2::TokenStream {
        match self {
            Scope::Shared => quote!(test_r::core::DepScope::Shared),
            Scope::PerWorker => quote!(test_r::core::DepScope::PerWorker),
            Scope::Cloneable => quote!(test_r::core::DepScope::Cloneable),
            Scope::Hosted => quote!(test_r::core::DepScope::Hosted),
            Scope::HostedRpc => quote!(test_r::core::DepScope::HostedRpc),
        }
    }

    fn from_ident_name(name: &str) -> Option<Self> {
        match name {
            "Shared" => Some(Scope::Shared),
            "PerWorker" => Some(Scope::PerWorker),
            "Cloneable" => Some(Scope::Cloneable),
            "Hosted" => Some(Scope::Hosted),
            "HostedRpc" => Some(Scope::HostedRpc),
            _ => None,
        }
    }
}

impl darling::FromMeta for Scope {
    fn from_meta(item: &syn::Meta) -> darling::Result<Self> {
        match item {
            // `scope = "PerWorker"` form.
            syn::Meta::NameValue(nv) => {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    let name = s.value();
                    Scope::from_ident_name(&name)
                        .ok_or_else(|| darling::Error::unknown_value(&name).with_span(s))
                } else if let syn::Expr::Path(p) = &nv.value {
                    // `scope = PerWorker` form (unquoted ident).
                    let ident = p.path.get_ident().ok_or_else(|| {
                        darling::Error::unsupported_format(
                            "scope must be one of Shared, PerWorker, Cloneable, Hosted, HostedRpc",
                        )
                        .with_span(p)
                    })?;
                    let name = ident.to_string();
                    Scope::from_ident_name(&name)
                        .ok_or_else(|| darling::Error::unknown_value(&name).with_span(ident))
                } else {
                    Err(darling::Error::unsupported_format(
                        "scope must be one of Shared, PerWorker, Cloneable, Hosted, HostedRpc",
                    )
                    .with_span(&nv.value))
                }
            }
            _ => Err(darling::Error::unsupported_format(
                "scope expects `scope = Value` or `scope = \"Value\"`",
            )
            .with_span(item)),
        }
    }

    fn from_string(value: &str) -> darling::Result<Self> {
        Scope::from_ident_name(value).ok_or_else(|| darling::Error::unknown_value(value))
    }
}

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

    let mut ast: ItemFn = syn::parse(item).expect("test ast");
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

    let scope = args.scope.unwrap_or_default();

    // For HostedRpc: tests parameterise on the *stub* type, not the owner
    // type. The constructor returns the owner; we register the dependency
    // under the stub type so that `#[test]` parameters of type `&Stub`
    // resolve correctly.
    let stub_type_path: Option<TypePath> = match (scope, args.stub.as_ref()) {
        (Scope::HostedRpc, Some(stub_path)) => Some(TypePath {
            qself: None,
            path: stub_path.clone(),
        }),
        (Scope::HostedRpc, None) => {
            panic!(
                "`scope = HostedRpc` requires `stub = StubType` so the runtime \
                 knows which type to inject into tests. The constructor returns \
                 the owner; tests parameterise on the stub."
            );
        }
        (_, Some(_)) => {
            panic!("`stub = ...` is only valid together with `scope = HostedRpc`.");
        }
        (_, None) => None,
    };

    let dep_name_str = match &stub_type_path {
        Some(stub_path) => type_path_to_string(stub_path, args.tagged_as.into()),
        None => type_path_to_string(&dep_type, args.tagged_as.into()),
    };
    let register_ident = Ident::new(
        &format!("test_r_register_dep_{dep_name_str}"),
        Span::call_site(),
    );

    let is_async = ast.sig.asyncness.is_some();
    let (dep_getters, dep_names, _dep_dimensions) = get_dependency_params(&ast, false);

    if !_dep_dimensions.is_empty() {
        panic!("Matrix dimensions are not supported on #[test_dep] constructor parameters");
    }
    filter_custom_parameter_attributes(&mut ast);

    // `worker = ...` is not currently a user-tunable knob: for `Cloneable`
    // the wire payload IS the dep value (so the runtime auto-generates the
    // worker reconstructor), and for `Hosted` the worker reconstructor is
    // derived from `HostedDep::from_descriptor`. Any user-supplied override
    // would silently be ignored, so reject it explicitly.
    if args.worker.is_some() {
        panic!(
            "`worker = ...` is not currently a configurable knob. \
             `scope = Cloneable` deps must implement `CloneableDep` and \
             `scope = Hosted` deps must implement `HostedDep`; the runtime \
             reconstructs the per-worker dep value from the wire / descriptor \
             bytes automatically."
        );
    }
    // Hosted owner constructors run exactly once in the top-level parent
    // process with an empty dep view. Owner-side dep wiring is reserved for
    // a future phase.
    if matches!(scope, Scope::Hosted) && !dep_names.is_empty() {
        panic!(
            "`scope = Hosted` deps may not depend on other test deps \
             (saw {dep_names:?}). The owner constructor runs once in the \
             top-level parent process with an empty dep view; owner-side \
             dependency wiring is reserved for a future phase."
        );
    }

    // Phase 1A restriction: Cloneable owner constructors must not consume any
    // other test deps. The owner runs once on the parent with an empty dep
    // view (see `TestSuiteExecution::collect_cloneable_wire_bytes_*`) and the
    // worker reconstructor is auto-derived from `CloneableDep`, so there is
    // no facility yet for owner-side or worker-side dependency wiring. Catch
    // this at compile time instead of letting it panic at runtime.
    if matches!(scope, Scope::Cloneable) && !dep_names.is_empty() {
        panic!(
            "Phase 1A `scope = Cloneable` deps may not depend on other test deps \
             (saw {dep_names:?}). The owner constructor runs once on the parent with an empty \
             dep view; owner-side and worker-side dependency wiring is reserved for Phase 1B."
        );
    }

    // Phase 1C restriction: HostedRpc owner constructors also run once on
    // the parent with an empty dep view. Mirrors Hosted's restriction.
    if matches!(scope, Scope::HostedRpc) && !dep_names.is_empty() {
        panic!(
            "`scope = HostedRpc` deps may not depend on other test deps \
             (saw {dep_names:?}). The owner constructor runs once in the \
             top-level parent process with an empty dep view."
        );
    }

    // Phase 1C MVP: async constructors for HostedRpc aren't wired through
    // the parent owner-cell collection path yet. Reject at macro time with
    // a clear message instead of letting it fail at runtime.
    if matches!(scope, Scope::HostedRpc) && is_async {
        panic!(
            "`scope = HostedRpc` constructors must currently be synchronous \
             (sync runner is the MVP target). Use a sync constructor that \
             returns the owner directly."
        );
    }

    let scope_tokens = scope.as_tokens();
    let dep_ty = &dep_type;

    // Cloneable codec + worker reconstructor, only emitted for scope=Cloneable.
    let (cloneable_codec_expr, cloneable_worker_fn_expr) = if matches!(scope, Scope::Cloneable) {
        let codec = quote! {
            Some(test_r::core::CloneableCodec {
                to_wire: std::sync::Arc::new(|__any: std::sync::Arc<dyn std::any::Any + Send + Sync>| {
                    let __value: std::sync::Arc<#dep_ty> = __any
                        .downcast::<#dep_ty>()
                        .expect("Cloneable dependency type mismatch in to_wire");
                    <#dep_ty as test_r::core::CloneableDep>::to_wire(&*__value)
                }),
                from_wire_bytes: std::sync::Arc::new(|__bytes: &[u8]| {
                    let __wire_value: #dep_ty = <#dep_ty as test_r::core::CloneableDep>::from_wire(__bytes);
                    let __boxed: std::sync::Arc<dyn std::any::Any + Send + Sync> =
                        std::sync::Arc::new(__wire_value);
                    __boxed
                }),
            })
        };
        let worker_fn = quote! {
            Some(test_r::core::WorkerReconstructor::Sync(std::sync::Arc::new(
                |__wire_payload: std::sync::Arc<dyn std::any::Any + Send + Sync>, _deps| {
                    // The wire payload is already the reconstructed dep value.
                    __wire_payload
                },
            )))
        };
        (codec, worker_fn)
    } else {
        (quote! { None }, quote! { None })
    };

    // Hosted descriptor codec + worker reconstructor, only emitted for scope=Hosted.
    // `to_wire` runs in the top-level parent process: it downcasts the owner
    // value, calls `HostedDep::descriptor`, and ships the bytes. `from_wire_bytes`
    // runs in each worker: it boxes the raw descriptor bytes, and the worker_fn
    // calls `HostedDep::from_descriptor` to produce the worker-side handle.
    let (hosted_codec_expr, hosted_worker_fn_expr) = if matches!(scope, Scope::Hosted) {
        let codec = quote! {
            Some(test_r::core::CloneableCodec {
                to_wire: std::sync::Arc::new(|__any: std::sync::Arc<dyn std::any::Any + Send + Sync>| {
                    let __value: std::sync::Arc<#dep_ty> = __any
                        .downcast::<#dep_ty>()
                        .expect("Hosted dependency type mismatch in descriptor()");
                    <#dep_ty as test_r::core::HostedDep>::descriptor(&*__value)
                }),
                from_wire_bytes: std::sync::Arc::new(|__bytes: &[u8]| {
                    // The "wire payload" for a Hosted dep is the raw
                    // descriptor bytes; the worker reconstructor will run
                    // HostedDep::from_descriptor against them.
                    let __boxed_bytes: Vec<u8> = __bytes.to_vec();
                    let __boxed: std::sync::Arc<dyn std::any::Any + Send + Sync> =
                        std::sync::Arc::new(__boxed_bytes);
                    __boxed
                }),
            })
        };
        let worker_fn = quote! {
            Some(test_r::core::WorkerReconstructor::Sync(std::sync::Arc::new(
                |__wire_payload: std::sync::Arc<dyn std::any::Any + Send + Sync>, _deps| {
                    let __bytes: std::sync::Arc<Vec<u8>> = __wire_payload
                        .downcast::<Vec<u8>>()
                        .expect("Hosted worker reconstructor expected Vec<u8> descriptor payload");
                    let __value: #dep_ty = <#dep_ty as test_r::core::HostedDep>::from_descriptor(&__bytes);
                    let __boxed: std::sync::Arc<dyn std::any::Any + Send + Sync> =
                        std::sync::Arc::new(__value);
                    __boxed
                },
            )))
        };
        (codec, worker_fn)
    } else {
        (quote! { None }, quote! { None })
    };

    // Phase 1C: RpcFactory + owner-cell-wrapping constructor for scope = HostedRpc.
    // The constructor returns the owner, wraps it in a HostedRpcOwnerCell,
    // and the factory tells the runtime how to (a) downcast the owner Arc
    // back to a cell and (b) build a worker-side stub from a channel.
    let rpc_factory_expr = if matches!(scope, Scope::HostedRpc) {
        let stub_ty = stub_type_path.as_ref().expect("stub type checked above");
        quote! {
            Some(test_r::core::RpcFactory {
                owner_into_cell: std::sync::Arc::new(
                    |__any: std::sync::Arc<dyn std::any::Any + Send + Sync>| {
                        __any
                            .downcast::<test_r::core::HostedRpcOwnerCell>()
                            .expect("HostedRpc owner downcast to HostedRpcOwnerCell failed")
                    },
                ),
                build_stub: std::sync::Arc::new(
                    |__channel: test_r::core::HostedRpcChannel| {
                        let __stub: #stub_ty =
                            <#dep_ty as test_r::core::HostedRpcDep>::build_stub(__channel);
                        let __boxed: std::sync::Arc<dyn std::any::Any + Send + Sync> =
                            std::sync::Arc::new(__stub);
                        __boxed
                    },
                ),
            })
        }
    } else {
        quote! { None }
    };

    // Pick the right codec/reconstructor pair based on scope. Only one of
    // them is populated at any time; we encode that invariant by choosing
    // the appropriate token expression here so the runtime never has to deal
    // with both being `Some` simultaneously.
    let (worker_fn_expr, codec_expr, hosted_codec_field_expr) = match scope {
        Scope::Cloneable => (
            cloneable_worker_fn_expr,
            cloneable_codec_expr,
            quote! { None },
        ),
        Scope::Hosted => (hosted_worker_fn_expr, quote! { None }, hosted_codec_expr),
        _ => (quote! { None }, quote! { None }, quote! { None }),
    };

    // For HostedRpc the constructor returns the owner type, but the runtime
    // stores a HostedRpcOwnerCell. Emit the wrapping in the constructor
    // closure itself so the downcast in `owner_into_cell` always succeeds.
    let ctor_call_sync = if matches!(scope, Scope::HostedRpc) {
        quote! {
            {
                let __owner = #ctor_name(#(#dep_getters),*);
                let __cell = test_r::core::HostedRpcOwnerCell::from_owner(__owner);
                let __arc: std::sync::Arc<dyn std::any::Any + Send + Sync> =
                    std::sync::Arc::new(__cell);
                __arc
            }
        }
    } else {
        quote! { std::sync::Arc::new(#ctor_name(#(#dep_getters),*)) }
    };

    let register_call = if is_async {
        quote! {
              test_r::core::register_dependency_constructor_with_scope(
                  #dep_name_str,
                  module_path!(),
                  test_r::core::DependencyConstructor::Async(std::sync::Arc::new(|__test_r_deps_arg| Box::pin(async move {
                    let result: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(#ctor_name(#(#dep_getters),*).await);
                    result
                  }))),
                 vec![#(#dep_names),*],
                 #scope_tokens,
                 #worker_fn_expr,
                 #codec_expr,
                 #hosted_codec_field_expr,
                 #rpc_factory_expr,
              );
        }
    } else {
        quote! {
            test_r::core::register_dependency_constructor_with_scope(
                #dep_name_str,
                module_path!(),
                test_r::core::DependencyConstructor::Sync(std::sync::Arc::new(|__test_r_deps_arg| #ctor_call_sync)),
                vec![#(#dep_names),*],
                #scope_tokens,
                #worker_fn_expr,
                #codec_expr,
                #hosted_codec_field_expr,
                #rpc_factory_expr,
            );
        }
    };

    let getter_ident = Ident::new(&format!("test_r_get_dep_{dep_name_str}"), Span::call_site());

    // The getter must downcast to the *injected* type, which is the stub
    // for HostedRpc and the constructor return type for every other scope.
    let injected_ty: proc_macro2::TokenStream = match &stub_type_path {
        Some(stub_path) => quote! { #stub_path },
        None => quote! { #dep_type },
    };

    let getter_body = quote! {
        dependency_view
            .get(#dep_name_str)
            .expect("Dependency not found")
            .downcast::<#injected_ty>()
            .expect("Dependency type mismatch")
    };

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor(crate_path=::test_r::ctor)]
        fn #register_ident() {
             #register_call
        }

        #[cfg(test)]
        fn #getter_ident<'a>(dependency_view: &'a impl test_r::core::DependencyView) -> std::sync::Arc<#injected_ty> {
            #getter_body
        }

        #ast
    };

    result.into()
}

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
    let getter_ident = Ident::new(&format!("test_r_get_dep_{dep_name_str}"), Span::call_site());

    let result = quote! {
        fn #getter_ident<'a>(dependency_view: &'a impl test_r::core::DependencyView) -> std::sync::Arc<#dep_type> {
            super::#getter_ident(dependency_view)
        }
    };

    result.into()
}

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
        let getter_ident = Ident::new(&format!("test_r_get_dep_{dep_name_str}"), Span::call_site());

        let name = tag.value();
        pushes.push(quote! {
            result.push((#name.to_string(), #dep_name_str.to_string(), std::sync::Arc::new(|dependency_view: std::sync::Arc<dyn test_r::core::DependencyView + Send + Sync>| #getter_ident(&dependency_view))));
        });
    }

    let ast = quote! {
        fn #get_dep_tags_fn() -> Vec<(String, String, std::sync::Arc<dyn (Fn(std::sync::Arc<dyn test_r::core::DependencyView + Send + Sync>) -> std::sync::Arc<#typ>) + Send + Sync + 'static>)> {
            let mut result: Vec<(String, String, std::sync::Arc<dyn (Fn(std::sync::Arc<dyn test_r::core::DependencyView + Send + Sync>) -> std::sync::Arc<#typ>) + Send + Sync + 'static>)> = Vec::new();
            #(#pushes)*
            result
        }
    };
    ast.into()
}

pub fn get_dependency_params(
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
                        (Some(_), Some(_)) => panic!(
                            "Cannot have both a tag and a dimension attribute on the same test parameter"
                        ),
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
            let getter_ident =
                Ident::new(&format!("test_r_get_dep_{dep_name_str}"), Span::call_site());

            dep_getters.push(quote! {
                &#getter_ident(&__test_r_deps_arg)
            });
            dep_names.push(quote! {
                #dep_name_str.to_string()
            });
        }
    }
    (dep_getters, dep_names, dep_dimensions)
}

pub fn get_dependency_params_for_closure<'a>(
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
                panic!(
                    "Test functions can only have parameters which are immutable references to concrete types, but got {:?}",
                    pat.to_token_stream()
                )
                // TODO: nicer error report
            }
        };
        let dep_name_str = type_path_to_string(&dep_type, tag);
        let binding = match pat {
            Pat::Type(typ) => match &*typ.pat {
                Pat::Ident(ident) => ident.ident.clone(),
                _ => {
                    panic!(
                        "Test functions can only have parameters which are immutable references to concrete types, but got {:?}",
                        typ.pat.to_token_stream()
                    )
                    // TODO: nicer error report
                }
            },
            _ => {
                panic!(
                    "Test functions can only have parameters which are immutable references to concrete types, but got {:?}",
                    pat.to_token_stream()
                )
                // TODO: nicer error report
            }
        };

        let getter_ident = Ident::new(&format!("test_r_get_dep_{dep_name_str}"), Span::call_site());

        dep_getters.push(quote! {
            &#getter_ident(&__test_r_deps_arg)
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
                    panic!(
                        "Test functions can only have parameters which are immutable references to concrete types, but got {:?}",
                        reference.elem.to_token_stream()
                    )
                    // TODO: nicer error report
                }
            }
        }
        _ => {
            panic!(
                "Test functions can only have parameters which are immutable references to concrete types, but got {:?}",
                typ.ty.to_token_stream()
            )
            // TODO: nicer error report
        }
    }
}

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

#[derive(Debug, darling::FromMeta)]
struct TestDepArgs {
    #[darling(default)]
    tagged_as: Option<String>,
    #[darling(default)]
    scope: Option<Scope>,
    /// Reserved for a future API. NOT currently configurable: for
    /// `Cloneable` the runtime auto-derives the worker reconstructor from
    /// `CloneableDep` and for `Hosted` it auto-derives it from
    /// `HostedDep::from_descriptor`. Supplying `worker = …` today is
    /// rejected at macro expansion to avoid a silent no-op.
    #[darling(default)]
    worker: Option<Path>,
    /// Phase 1C: required for `scope = HostedRpc`. Names the worker-visible
    /// stub type — i.e. `<Owner as HostedRpcDep>::Stub`. Tests parameterise
    /// on this type, not on the owner type. Rejected for any other scope.
    #[darling(default)]
    stub: Option<Path>,
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

fn get_lit_str_attr(attrs: &[Attribute], ident: &str) -> Option<String> {
    attrs
        .iter()
        .find(|attr| is_testr_attribute(attr, ident))
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
        .find(|attr| is_testr_attribute(attr, ident))
        .map(|attr| {
            attr.parse_args::<Ident>()
                .unwrap_or_else(|_| panic!("{ident} attribute's parameter must be an identifier"))
        })
}

fn type_to_string(typ: &Type, optional_tag: DependencyTag) -> String {
    match typ {
        Type::Array(array) => {
            let inner_type = type_to_string(&array.elem, optional_tag);
            format!("array_{inner_type}")
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
            format!("ptr_{inner_type}")
        }
        Type::Reference(inner) => {
            let inner_type = type_to_string(&inner.elem, optional_tag);
            format!("ref_{inner_type}")
        }
        Type::Slice(inner) => {
            let inner_type = type_to_string(&inner.elem, optional_tag);
            format!("slice_{inner_type}")
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
            format!("tuple_{inner_types}")
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
