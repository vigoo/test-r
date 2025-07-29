use crate::helpers::is_testr_attribute;
use darling::ast::NestedMeta;
use darling::{Error, FromMeta};
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::{ToTokens, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, FnArg, GenericArgument, ItemFn, LitStr, Pat, PatType, PathArguments, PathSegment,
    ReturnType, Token, Type, TypeParamBound, TypePath, parse_macro_input,
};

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
        &format!("test_r_register_dep_{dep_name_str}"),
        Span::call_site(),
    );

    let is_async = ast.sig.asyncness.is_some();
    let (dep_getters, dep_names, _dep_dimensions) = get_dependency_params(&ast, false);

    let register_call = if is_async {
        quote! {
              test_r::core::register_dependency_constructor(
                  #dep_name_str,
                  module_path!(),
                  test_r::core::DependencyConstructor::Async(std::sync::Arc::new(|__test_r_deps_arg| Box::pin(async move {
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
                test_r::core::DependencyConstructor::Sync(std::sync::Arc::new(|__test_r_deps_arg| std::sync::Arc::new(#ctor_name(#(#dep_getters),*)))),
                vec![#(#dep_names),*]
            );
        }
    };

    let getter_ident = Ident::new(&format!("test_r_get_dep_{dep_name_str}"), Span::call_site());

    let getter_body = quote! {
        dependency_view
            .get(#dep_name_str)
            .expect("Dependency not found")
            .downcast::<#dep_type>()
            .expect("Dependency type mismatch")
    };

    let result = quote! {
        #[cfg(test)]
        #[test_r::ctor::ctor(crate_path=::test_r::ctor)]
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
