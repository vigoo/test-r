//! `#[hosted_rpc]` attribute macro on a user-defined trait.
//!
//! This is the HR1.1 helper that eliminates the per-method boilerplate
//! around the 1C MVP `HostedRpcDep` trait: from a user trait declaration
//! it emits a worker-side `<Trait>Stub` struct that implements the trait
//! by routing each method through a [`test_r::core::HostedRpcChannel`],
//! and an owner-side `<Trait>Dispatch` helper trait with a method-table
//! dispatcher implemented for every `T: Trait`.
//!
//! Out of scope for HR1.1 (deferred and rejected at macro time):
//!
//! - attribute arguments on `#[hosted_rpc(...)]` itself
//! - associated types or `const` items on the trait
//! - generics on the trait or on individual methods
//! - supertraits
//! - `unsafe trait` / `unsafe fn` / non-default ABI / `extern fn`
//! - default-impl methods (they would not appear on the wire)
//! - `impl Trait` in argument or return position
//! - non-identifier argument patterns (`_`, destructuring, etc.)
//! - receivers other than `&self` (no `self`, `mut self`,
//!   `self: Box<Self>`, …, and **no `&mut self`** either, because
//!   test-r injects test deps as `&Stub` immutable references; a
//!   `&mut self` stub method would compile but be uncallable from a
//!   normal `#[test] fn (s: &MyStub)` parameter)
//! - `#[cfg(...)]` / `#[cfg_attr(...)]` on the trait or its methods
//!   (the generated sibling items + dispatch arms aren't cfg-propagated)
//!
//! Async methods:
//!
//! - `async fn` methods are auto-detected. A trait with one or more
//!   `async fn` methods is "async-mode": every method is required to
//!   be `async fn` (mixed sync/async is rejected so the worker-side
//!   stub doesn't end up with a mixed trait surface), the generated
//!   `<Trait>Dispatch` helper exposes
//!   `async fn dispatch_<snake>(...)`, and the user implements
//!   [`AsyncHostedRpcDep`] on the owner. A trait with only sync
//!   methods stays in legacy "sync-mode" and the user implements
//!   [`HostedRpcDep`] as before. No explicit flag on
//!   `#[hosted_rpc]` is required — the choice flows naturally from
//!   the user-authored trait signature.
//!
//! [`AsyncHostedRpcDep`]: ../test_r_core/internal/trait.AsyncHostedRpcDep.html
//! [`HostedRpcDep`]: ../test_r_core/internal/trait.HostedRpcDep.html
//!
//! Wire format:
//!
//! - method index is the trait's 0-based source order, encoded as `u32`.
//! - args are encoded as the method's parameter list after stripping `self`:
//!   the 0-arg case sends `()`, the 1-arg case sends the bare value of
//!   type `T` (NOT `(T,)`, see [`expand`] for the `desert_rust 0.1.7`
//!   tuple-1 asymmetry that motivates this), and the 2+-arg case sends
//!   a regular tuple `(T1, T2, …)`.
//! - return values are encoded directly; the unit case uses `()`.
//! - encoding is `desert_rust` (the same codec used by the IPC framing).
//!
//! Failure mode:
//!
//! - Transport, codec and dispatch failures **panic in the generated
//!   stub** with an `expect(...)` message that mentions the trait and
//!   method name. The user-facing trait signature carries its own
//!   return type unchanged, so user-level errors such as
//!   `Result<T, E>` are still encoded and returned normally.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, format_ident, quote};
use syn::{
    Attribute, FnArg, GenericArgument, ItemTrait, Pat, PatType, PathArguments, ReturnType,
    TraitItem, TraitItemFn, Type, Visibility, parse_macro_input,
};

pub fn hosted_rpc(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Reject any attribute payload now so we don't silently accept
    // `#[hosted_rpc(something)]` and confuse users about what the
    // macro actually supports.
    if !attr.is_empty() {
        let attr2: TokenStream2 = attr.into();
        return syn::Error::new_spanned(
            attr2,
            "`#[hosted_rpc]` does not take any attribute arguments in the MVP",
        )
        .to_compile_error()
        .into();
    }
    let item_trait = parse_macro_input!(item as ItemTrait);
    expand(item_trait).into()
}

fn expand(item_trait: ItemTrait) -> TokenStream2 {
    // Reject unsupported trait shapes early with a clear message.
    if item_trait.unsafety.is_some() {
        return syn::Error::new_spanned(
            item_trait.unsafety,
            "`#[hosted_rpc]` traits must not be `unsafe` in the MVP",
        )
        .to_compile_error();
    }
    if let Some(g) = item_trait.generics.params.first() {
        return syn::Error::new_spanned(g, "`#[hosted_rpc]` traits must be non-generic in the MVP")
            .to_compile_error();
    }
    if !item_trait.supertraits.is_empty() {
        // Supertraits are fine syntactically but the generated stub
        // would have to implement them too; reject in MVP to avoid
        // half-supported corners.
        return syn::Error::new_spanned(
            &item_trait.supertraits,
            "`#[hosted_rpc]` traits must not have supertraits in the MVP",
        )
        .to_compile_error();
    }
    if let Some(attr) = item_trait.attrs.iter().find(|a| is_cfg_attr(a)) {
        return syn::Error::new_spanned(
            attr,
            "`#[hosted_rpc]` does not support `#[cfg(...)]` / `#[cfg_attr(...)]` on the trait in the MVP \
             (the generated sibling items would not be cfg-propagated)",
        )
        .to_compile_error();
    }
    if let Some(item) = item_trait
        .items
        .iter()
        .find(|it| !matches!(it, TraitItem::Fn(_)))
    {
        return syn::Error::new_spanned(
            item,
            "`#[hosted_rpc]` traits must only declare methods (no consts, types, etc.)",
        )
        .to_compile_error();
    }

    let methods: Vec<&TraitItemFn> = item_trait
        .items
        .iter()
        .filter_map(|it| match it {
            TraitItem::Fn(f) => Some(f),
            _ => None,
        })
        .collect();

    // Auto-detect async mode: a trait whose first method is `async fn` is
    // async-mode; every method then must also be async. A trait with no
    // async methods stays in legacy sync-mode and generates the same code
    // it did before. Mixed sync/async is rejected because the generated
    // worker-side stub `impl <Trait> for <Trait>Stub` must match the
    // user-authored trait's signatures method-by-method — a mixed trait
    // would yield half-async stub methods that call sync `channel.call(...)`
    // and half-sync methods that would have to do the same: confusing for
    // implementors of `AsyncHostedRpcDep` on the owner side. Forcing
    // all-or-nothing keeps the runtime contract simple.
    let async_mode = methods.iter().any(|m| m.sig.asyncness.is_some());
    if async_mode && let Some(sync_method) = methods.iter().find(|m| m.sig.asyncness.is_none()) {
        return syn::Error::new_spanned(
            &sync_method.sig,
            "`#[hosted_rpc]` requires methods to be either all `async fn` or all sync \
             (mixed sync/async traits are rejected so the generated stub trait surface stays consistent)",
        )
        .to_compile_error();
    }

    for m in &methods {
        if let Some(g) = m.sig.generics.params.first() {
            return syn::Error::new_spanned(
                g,
                "`#[hosted_rpc]` methods must be non-generic in the MVP",
            )
            .to_compile_error();
        }
        // `async fn` is allowed: when present on every method, the macro
        // generates an async dispatch helper that lets owners implement
        // `AsyncHostedRpcDep`. We still reject `unsafe`, non-default ABI,
        // variadic, and default-bodied methods below.
        if m.sig.unsafety.is_some() {
            return syn::Error::new_spanned(
                m.sig.unsafety,
                "`#[hosted_rpc]` methods must not be `unsafe` in the MVP",
            )
            .to_compile_error();
        }
        if let Some(abi) = &m.sig.abi {
            return syn::Error::new_spanned(
                abi,
                "`#[hosted_rpc]` methods must use the default Rust ABI (no `extern ...`) in the MVP",
            )
            .to_compile_error();
        }
        if let Some(variadic) = &m.sig.variadic {
            return syn::Error::new_spanned(
                variadic,
                "`#[hosted_rpc]` methods must not be variadic in the MVP",
            )
            .to_compile_error();
        }
        if m.default.is_some() {
            return syn::Error::new_spanned(
                m,
                "`#[hosted_rpc]` methods must not have a default body in the MVP",
            )
            .to_compile_error();
        }
        if let Some(attr) = m.attrs.iter().find(|a| is_cfg_attr(a)) {
            return syn::Error::new_spanned(
                attr,
                "`#[hosted_rpc]` does not support `#[cfg(...)]` / `#[cfg_attr(...)]` on trait methods in the MVP \
                 (the generated dispatch arms would not be cfg-propagated)",
            )
            .to_compile_error();
        }
        // The first arg must be `&self`: a shared borrowed receiver
        // with no `mut`, no explicit `self: ...` type ascription, and
        // no by-value form. `&mut self` is rejected separately below
        // because test-r injects test deps as immutable references.
        let Some(first) = m.sig.inputs.first() else {
            return syn::Error::new_spanned(&m.sig, "`#[hosted_rpc]` methods must take `&self`")
                .to_compile_error();
        };
        let FnArg::Receiver(receiver) = first else {
            return syn::Error::new_spanned(first, "`#[hosted_rpc]` methods must take `&self`")
                .to_compile_error();
        };
        if receiver.reference.is_none() {
            return syn::Error::new_spanned(
                receiver,
                "`#[hosted_rpc]` methods must take `&self` (no by-value `self`)",
            )
            .to_compile_error();
        }
        if receiver.colon_token.is_some() {
            return syn::Error::new_spanned(
                receiver,
                "`#[hosted_rpc]` methods must take `&self` (no explicit `self: T` type)",
            )
            .to_compile_error();
        }
        if receiver.mutability.is_some() {
            // test-r injects test deps as **immutable** references, so a
            // `&mut self` stub method generated by the macro would parse
            // but be uncallable from a normal `#[test] fn (s: &Stub)`
            // parameter. Reject up-front to avoid the "compiles but
            // fails to call" UX trap.
            return syn::Error::new_spanned(
                receiver,
                "`#[hosted_rpc]` methods must take `&self` (test-r test deps are injected as `&Stub`; \
                 `&mut self` stub methods would be uncallable from injected test parameters)",
            )
            .to_compile_error();
        }
        // Reject `impl Trait` in argument or return position; we can't
        // ship an existential parameter or return type over the wire.
        for input in m.sig.inputs.iter() {
            if let FnArg::Typed(t) = input
                && contains_impl_trait(&t.ty)
            {
                return syn::Error::new_spanned(
                    &t.ty,
                    "`#[hosted_rpc]` does not support `impl Trait` in argument position in the MVP",
                )
                .to_compile_error();
            }
        }
        if let ReturnType::Type(_, ty) = &m.sig.output
            && contains_impl_trait(ty)
        {
            return syn::Error::new_spanned(
                ty,
                "`#[hosted_rpc]` does not support `impl Trait` in return position in the MVP",
            )
            .to_compile_error();
        }
        // Reject non-identifier argument patterns (`_`, destructuring,
        // etc.) — we re-use the pattern as an *expression* both in the
        // stub's args tuple and in the dispatcher's call site, which
        // would emit invalid Rust for anything that isn't a plain ident.
        for input in m.sig.inputs.iter() {
            if let FnArg::Typed(t) = input
                && !matches!(&*t.pat, Pat::Ident(_))
            {
                return syn::Error::new_spanned(
                    &t.pat,
                    "`#[hosted_rpc]` requires plain identifier argument patterns (no `_`, no destructuring) in the MVP",
                )
                .to_compile_error();
            }
        }
    }

    let trait_vis = &item_trait.vis;
    let trait_ident = &item_trait.ident;
    let stub_ident = format_ident!("{}Stub", trait_ident);
    let dispatch_ident = format_ident!("{}Dispatch", trait_ident);
    let dispatch_method_ident =
        format_ident!("dispatch_{}", to_snake_case(&trait_ident.to_string()));

    // For each method generate: stub impl arm, dispatch arm.
    //
    // Stub side: the generated stub method copies the user-authored
    // signature verbatim — sync stays sync, `async fn` stays `async fn`
    // — and uses the synchronous `channel.call(...)` in its body. That
    // is correct in both modes because a sync call inside an async fn
    // body is legal; the outer async signature is what lets the test
    // body `.await` the stub.
    //
    // Dispatch side: in async-mode the per-method arm awaits the user's
    // owner-side trait method; in sync-mode it calls it synchronously
    // exactly as before.
    let mut stub_impl_arms: Vec<TokenStream2> = Vec::new();
    let mut dispatch_arms: Vec<TokenStream2> = Vec::new();
    for (idx, m) in methods.iter().enumerate() {
        let method_idx = idx as u32;
        let sig = &m.sig;
        let method_ident = &sig.ident;
        // Preserve the user's asyncness on both the stub method
        // signature and the matching `impl <Trait> for <Trait>Stub`
        // method.
        let asyncness = &sig.asyncness;
        let await_token = if asyncness.is_some() {
            quote!(.await)
        } else {
            quote!()
        };
        let (receiver, typed_args): (TokenStream2, Vec<&PatType>) = {
            let mut recv = quote!();
            let mut others: Vec<&PatType> = Vec::new();
            for input in sig.inputs.iter() {
                match input {
                    FnArg::Receiver(r) => recv = r.to_token_stream(),
                    FnArg::Typed(t) => others.push(t),
                }
            }
            (recv, others)
        };
        let arg_idents: Vec<TokenStream2> = typed_args
            .iter()
            .map(|t| match &*t.pat {
                Pat::Ident(p) => {
                    let i = &p.ident;
                    quote!(#i)
                }
                // Non-identifier patterns are rejected by the validation
                // pass above; this arm should be unreachable in valid
                // expansions and is kept only as a defensive fallback.
                other => other.to_token_stream(),
            })
            .collect();
        let arg_types: Vec<TokenStream2> =
            typed_args.iter().map(|t| t.ty.to_token_stream()).collect();
        let ret_ty: TokenStream2 = match &sig.output {
            ReturnType::Default => quote!(()),
            ReturnType::Type(_, t) => t.to_token_stream(),
        };
        // Build args wire expressions both for encode (in stub) and
        // decode (in dispatcher).
        //
        // Wire shape by arity:
        //   - 0 args: encode/decode the unit value `()`.
        //   - 1 arg:  encode/decode the bare value of type `T` (NOT a 1-tuple
        //             — `desert_rust 0.1.7`'s `BinarySerializer` for `(T,)`
        //             does not write a version byte while its matching
        //             `BinaryDeserializer` for `(T,)` always reads one, so a
        //             round-trip through `(T,)` would desync the input
        //             stream and surface as `InputEndedUnexpectedly` on the
        //             dispatch side).
        //   - >=2 args: encode/decode a regular tuple `(T1, T2, …)`.
        let args_pack: TokenStream2 = if arg_idents.is_empty() {
            quote!(())
        } else if arg_idents.len() == 1 {
            let id = &arg_idents[0];
            quote!(#id)
        } else {
            quote!((#(#arg_idents),*))
        };
        let args_tuple_ty: TokenStream2 = if arg_types.is_empty() {
            quote!(())
        } else if arg_types.len() == 1 {
            let t = &arg_types[0];
            quote!(#t)
        } else {
            quote!((#(#arg_types),*))
        };
        // Reconstruct individual bindings from the decoded args value.
        let arg_unpack: Vec<TokenStream2> = if arg_idents.is_empty() {
            Vec::new()
        } else if arg_idents.len() == 1 {
            let id = &arg_idents[0];
            vec![quote!(let #id = __args;)]
        } else {
            vec![quote!(let (#(#arg_idents),*) = __args;)]
        };

        let attrs = &m.attrs;
        let stub_label = format!("{}::{}", trait_ident, method_ident);
        let stub_encode_msg = format!("hosted_rpc({stub_label}): encode args");
        let stub_call_msg = format!("hosted_rpc({stub_label}): rpc call failed");
        let stub_decode_msg = format!("hosted_rpc({stub_label}): decode reply");
        let dispatch_decode_args_fmt = format!(
            "hosted_rpc dispatch ({stub_label}, method_idx={method_idx}): decode args: {{:?}}"
        );
        let dispatch_encode_reply_fmt = format!(
            "hosted_rpc dispatch ({stub_label}, method_idx={method_idx}): encode reply: {{:?}}"
        );
        stub_impl_arms.push(quote! {
            #(#attrs)*
            #asyncness fn #method_ident(#receiver, #(#typed_args),*) -> #ret_ty {
                let __args: #args_tuple_ty = #args_pack;
                let __args_bytes: ::std::vec::Vec<u8> =
                    ::test_r::core::desert_rust::serialize_to_byte_vec(&__args)
                        .expect(#stub_encode_msg);
                let __reply: ::std::vec::Vec<u8> = self
                    .channel
                    .call(#method_idx, __args_bytes)
                    .expect(#stub_call_msg);
                ::test_r::core::desert_rust::deserialize::<#ret_ty>(&__reply)
                    .expect(#stub_decode_msg)
            }
        });

        dispatch_arms.push(quote! {
            #method_idx => {
                let __args: #args_tuple_ty =
                    ::test_r::core::desert_rust::deserialize(args)
                        .map_err(|e| ::std::format!(#dispatch_decode_args_fmt, e))?;
                #(#arg_unpack)*
                let __result: #ret_ty = self.#method_ident(#(#arg_idents),*) #await_token;
                ::test_r::core::desert_rust::serialize_to_byte_vec(&__result)
                    .map_err(|e| ::std::format!(#dispatch_encode_reply_fmt, e))
            }
        });
    }

    // Stub visibility mirrors the trait's visibility so users can
    // parameterise their tests on `&MyStub` from the same module.
    let stub_vis: &Visibility = trait_vis;

    let trait_decl = &item_trait;

    let dispatch_unknown_method_text = format!("{}: unknown method_idx {{}}", trait_ident);

    let stub_struct_name_text = stub_ident.to_string();

    // In async-mode, the dispatch helper exposes
    //   `async fn dispatch_<snake>(&mut self, method_idx, args) -> Result<...>`
    // so an owner-side `AsyncHostedRpcDep::dispatch` impl can delegate to it
    // with `.await`. In sync-mode the dispatch helper stays synchronous,
    // matching the legacy `HostedRpcDep::dispatch` signature.
    //
    // In async-mode the dispatch helper's blanket impl adds
    // `Send + Sync` to the `T: Trait` bound. Two reasons:
    //
    // 1. `AsyncHostedRpcDep` requires the owner to be `Send + Sync`, so
    //    the helper has to admit the same set of owner types.
    // 2. The returned future for an `async fn` only inherits `Send`
    //    when every captured `&self` / `&mut self` is itself `Send`
    //    (which requires `T: Sync` / `T: Send`). The parent dispatcher
    //    polls this future on a tokio task that requires `Send`, so
    //    insufficient bounds here would show up as a `Send` error at
    //    the owner's `AsyncHostedRpcDep::dispatch` impl site.
    //
    // In sync-mode no extra bounds are needed — the helper's
    // `dispatch_<snake>` is a plain `fn`.
    let dispatch_asyncness = if async_mode { quote!(async) } else { quote!() };
    let blanket_bound = if async_mode {
        quote!(#trait_ident + ::core::marker::Send + ::core::marker::Sync + ?Sized)
    } else {
        quote!(#trait_ident + ?Sized)
    };

    quote! {
        #trait_decl

        /// Worker-side stub generated by `#[hosted_rpc]`. Holds a
        /// [`::test_r::core::HostedRpcChannel`] and implements the
        /// host trait by routing each method through the channel.
        #stub_vis struct #stub_ident {
            channel: ::test_r::core::HostedRpcChannel,
        }

        impl #stub_ident {
            /// Constructor used by the runtime's `build_stub` glue.
            pub fn new(channel: ::test_r::core::HostedRpcChannel) -> Self {
                Self { channel }
            }
        }

        impl ::core::fmt::Debug for #stub_ident {
            /// Generated by `#[hosted_rpc]`. Stubs only carry an
            /// opaque `HostedRpcChannel`, so the formatter just prints
            /// the stub type name plus the dep id the channel routes
            /// to. This lets stubs be used as parameters of test
            /// fixtures decorated with attributes (such as
            /// `#[tracing::instrument]`) that require every parameter
            /// to implement `Debug`.
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(#stub_struct_name_text)
                    .field("dep_id", &self.channel.dep_id())
                    .finish()
            }
        }

        impl #trait_ident for #stub_ident {
            #(#stub_impl_arms)*
        }

        /// Owner-side helper trait generated by `#[hosted_rpc]`. It is
        /// blanket-implemented for every type that implements the host
        /// trait, so an owner's `HostedRpcDep::dispatch` (sync mode) or
        /// `AsyncHostedRpcDep::dispatch` (async mode) impl can delegate
        /// to `Self::dispatch_<snake_case_trait_name>(self, method_idx, args)`
        /// without writing the per-method match arms by hand.
        ///
        /// In async-mode the helper method is itself `async fn`; the
        /// generated dispatch arms `.await` each owner-side method call
        /// so the user's `async fn next(&self) -> u64` runs to completion
        /// before its result is encoded for the worker.
        #trait_vis trait #dispatch_ident {
            #dispatch_asyncness fn #dispatch_method_ident(
                &mut self,
                method_idx: u32,
                args: &[u8],
            ) -> ::std::result::Result<::std::vec::Vec<u8>, ::std::string::String>;
        }

        impl<__T: #blanket_bound> #dispatch_ident for __T {
            #dispatch_asyncness fn #dispatch_method_ident(
                &mut self,
                method_idx: u32,
                args: &[u8],
            ) -> ::std::result::Result<::std::vec::Vec<u8>, ::std::string::String> {
                match method_idx {
                    #(#dispatch_arms)*
                    other => ::std::result::Result::Err(
                        ::std::format!(#dispatch_unknown_method_text, other),
                    ),
                }
            }
        }
    }
}

/// True for `#[cfg(...)]` and `#[cfg_attr(...)]`. We reject both on the
/// trait and on individual methods because the generated sibling items
/// (the stub struct, the dispatch helper trait, and the per-method
/// dispatch arms) aren't cfg-propagated in the MVP, so a feature-gated
/// item would compile-break the generated code.
fn is_cfg_attr(attr: &Attribute) -> bool {
    attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr")
}

/// Walk a [`syn::Type`] and return `true` if it contains an `impl Trait`
/// existential anywhere (top-level, inside a reference, generic arg,
/// tuple element, etc.). We can't serialise an existential type, so we
/// reject those forms at macro time.
///
/// `syn`'s built-in [`syn::visit`] would do this in one line, but it's
/// hidden behind the optional `visit` feature; we avoid pulling that in
/// for a single helper by recursing manually over the type shapes the
/// MVP accepts in argument and return positions.
fn contains_impl_trait(ty: &Type) -> bool {
    match ty {
        Type::ImplTrait(_) => true,
        Type::Reference(r) => contains_impl_trait(&r.elem),
        Type::Paren(p) => contains_impl_trait(&p.elem),
        Type::Group(g) => contains_impl_trait(&g.elem),
        Type::Slice(s) => contains_impl_trait(&s.elem),
        Type::Array(a) => contains_impl_trait(&a.elem),
        Type::Ptr(p) => contains_impl_trait(&p.elem),
        Type::Tuple(t) => t.elems.iter().any(contains_impl_trait),
        Type::Path(p) => {
            // Walk through the generic argument types inside any segment
            // (e.g. `Vec<impl Trait>`, `Option<&impl Display>`, …).
            p.path.segments.iter().any(|seg| match &seg.arguments {
                PathArguments::None => false,
                PathArguments::AngleBracketed(args) => args.args.iter().any(|a| match a {
                    GenericArgument::Type(t) => contains_impl_trait(t),
                    _ => false,
                }),
                PathArguments::Parenthesized(args) => {
                    args.inputs.iter().any(contains_impl_trait)
                        || matches!(&args.output, ReturnType::Type(_, t) if contains_impl_trait(t))
                }
            })
        }
        _ => false,
    }
}

fn to_snake_case(s: &str) -> String {
    // Naive PascalCase / camelCase -> snake_case. Suitable for trait
    // identifier names where punctuation is restricted to ASCII alnum.
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::expand;
    use syn::parse_quote;

    /// Expand a trait declaration with `#[hosted_rpc]` and return the
    /// stringified token stream. Used by the rejection tests below to
    /// assert on the embedded `compile_error!` messages without going
    /// through `cargo` / `trybuild` machinery.
    fn expand_to_string(item: syn::ItemTrait) -> String {
        expand(item).to_string()
    }

    #[test]
    fn rejects_generic_trait() {
        let s = expand_to_string(parse_quote! {
            trait Foo<T> {
                fn one(&self) -> T;
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for generic traits, got: {s}"
        );
        assert!(
            s.contains("non-generic"),
            "expected the rejection to mention non-generic, got: {s}"
        );
    }

    #[test]
    fn rejects_generic_method() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one<T>(&self, x: T);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for generic methods, got: {s}"
        );
    }

    #[test]
    fn rejects_mixed_sync_and_async_methods() {
        // Pinning the all-or-nothing rule: a trait with at least one
        // `async fn` method and at least one sync method must be
        // rejected so the generated stub doesn't produce a half-async
        // surface for the worker side to implement.
        let s = expand_to_string(parse_quote! {
            trait Foo {
                async fn async_one(&self);
                fn sync_two(&self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for mixed sync/async methods, got: {s}"
        );
        assert!(
            s.contains("all `async fn`") || s.contains("all sync"),
            "expected the rejection to mention all-or-nothing async, got: {s}"
        );
    }

    #[test]
    fn accepts_all_async_methods_and_emits_async_dispatch() {
        // Pin async-mode auto-detection: every method `async fn` →
        // the generated dispatch helper trait method is also `async fn`.
        let s = expand_to_string(parse_quote! {
            trait Counter {
                async fn next(&self) -> u64;
                async fn reserve(&self, count: u32) -> u64;
            }
        });
        assert!(
            !s.contains("compile_error"),
            "valid async trait must not emit compile_error!, got: {s}"
        );
        let normalized: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        // Both the generated dispatcher trait method and its blanket
        // impl method must be `async fn`. The exact identifier name is
        // `dispatch_counter` (snake_case of `Counter`).
        assert!(
            normalized.contains("async fn dispatch_counter"),
            "async-mode must produce an async dispatch helper method, got: {normalized}"
        );
        // Stub methods must keep the user's `async fn` signature.
        assert!(
            normalized.contains("async fn next") && normalized.contains("async fn reserve"),
            "async-mode stub impl methods must stay `async fn`, got: {normalized}"
        );
    }

    #[test]
    fn rejects_default_body_method() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one(&self) { let _ = 1; }
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for default-body methods, got: {s}"
        );
        assert!(
            s.contains("default body"),
            "expected the rejection to mention default body, got: {s}"
        );
    }

    #[test]
    fn rejects_associated_type() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                type Item;
                fn one(&self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for associated types, got: {s}"
        );
    }

    #[test]
    fn rejects_method_without_self() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one(x: u32);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for non-`self` first argument, got: {s}"
        );
    }

    #[test]
    fn rejects_supertraits() {
        let s = expand_to_string(parse_quote! {
            trait Foo: Send {
                fn one(&self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for supertraits, got: {s}"
        );
        assert!(
            s.contains("supertraits"),
            "expected the rejection to mention supertraits, got: {s}"
        );
    }

    #[test]
    fn rejects_self_by_value_receiver() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one(self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for by-value self, got: {s}"
        );
        assert!(
            s.contains("by-value"),
            "expected the rejection to mention by-value, got: {s}"
        );
    }

    #[test]
    fn rejects_explicit_self_type() {
        // `self: Box<Self>` parses with `Receiver.reference = None` and
        // `Receiver.colon_token = Some(_)`. We hit the by-value branch
        // first (since it has no `&`), so either rejection wording is
        // acceptable as long as we surface a clear `&self`/`&mut self`
        // requirement.
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one(self: Box<Self>);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for explicit `self: T`, got: {s}"
        );
        assert!(
            s.contains("&self") || s.contains("by-value") || s.contains("self: T"),
            "expected the rejection to mention &self / by-value / self: T, got: {s}"
        );
    }

    #[test]
    fn rejects_unsafe_method() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                unsafe fn one(&self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for unsafe methods, got: {s}"
        );
        assert!(
            s.contains("unsafe"),
            "expected the rejection to mention unsafe, got: {s}"
        );
    }

    #[test]
    fn rejects_unsafe_trait() {
        let s = expand_to_string(parse_quote! {
            unsafe trait Foo {
                fn one(&self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for unsafe traits, got: {s}"
        );
        assert!(
            s.contains("unsafe"),
            "expected the rejection to mention unsafe, got: {s}"
        );
    }

    #[test]
    fn rejects_extern_abi_method() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                extern "C" fn one(&self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for non-default ABI, got: {s}"
        );
        assert!(
            s.contains("Rust ABI") || s.contains("extern"),
            "expected the rejection to mention ABI/extern, got: {s}"
        );
    }

    #[test]
    fn rejects_impl_trait_in_argument() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one(&self, x: impl ::std::fmt::Display);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for impl Trait in argument, got: {s}"
        );
        assert!(
            s.contains("argument position"),
            "expected the rejection to mention argument position, got: {s}"
        );
    }

    #[test]
    fn rejects_impl_trait_in_return() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one(&self) -> impl ::std::fmt::Display;
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for impl Trait in return, got: {s}"
        );
        assert!(
            s.contains("return position"),
            "expected the rejection to mention return position, got: {s}"
        );
    }

    #[test]
    fn rejects_wildcard_arg_pattern() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one(&self, _: u32);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for wildcard arg pattern, got: {s}"
        );
        assert!(
            s.contains("identifier"),
            "expected the rejection to mention identifier patterns, got: {s}"
        );
    }

    #[test]
    fn rejects_destructured_arg_pattern() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one(&self, (a, b): (u32, u32));
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for destructured arg pattern, got: {s}"
        );
    }

    #[test]
    fn rejects_cfg_on_trait() {
        let s = expand_to_string(parse_quote! {
            #[cfg(unix)]
            trait Foo {
                fn one(&self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for cfg on trait, got: {s}"
        );
        assert!(
            s.contains("cfg"),
            "expected the rejection to mention cfg, got: {s}"
        );
    }

    #[test]
    fn rejects_cfg_on_method() {
        let s = expand_to_string(parse_quote! {
            trait Foo {
                #[cfg(unix)]
                fn one(&self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for cfg on method, got: {s}"
        );
        assert!(
            s.contains("cfg"),
            "expected the rejection to mention cfg, got: {s}"
        );
    }

    #[test]
    fn rejects_mut_self_receiver() {
        // test-r injects test deps as `&Stub`, so `&mut self` on a
        // generated stub method would compile but be uncallable from
        // the user's `#[test] fn (s: &MyStub)` parameter. Reject up
        // front to avoid the "compiles but doesn't work in a test" UX.
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn one(&mut self);
            }
        });
        assert!(
            s.contains("compile_error"),
            "expected a compile_error! for `&mut self`, got: {s}"
        );
        assert!(
            s.contains("&Stub") && s.contains("uncallable"),
            "expected the rejection to mention the immutable `&Stub` injection rationale, got: {s}"
        );
    }

    #[test]
    fn accepts_two_arg_method() {
        // Pins the multi-arg wire shape (`(T1, T2)` tuple) — the 1-arg
        // bare-`T` special case does NOT apply here, so this exercises
        // the `>= 2` branch of `args_pack` / `args_tuple_ty`.
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn add(&self, a: u32, b: u32) -> u32;
            }
        });
        assert!(
            !s.contains("compile_error"),
            "valid `&self` + 2-arg trait must not emit compile_error!, got: {s}"
        );
        assert!(s.contains("struct FooStub"));
        assert!(s.contains("trait FooDispatch"));
    }

    #[test]
    fn accepts_unit_return_method() {
        // Pins the unit-return path (`ReturnType::Default`): the macro
        // must synthesise `()` for both encode and decode.
        let s = expand_to_string(parse_quote! {
            trait Foo {
                fn ping(&self);
            }
        });
        assert!(
            !s.contains("compile_error"),
            "valid `&self` + unit-return trait must not emit compile_error!, got: {s}"
        );
        assert!(s.contains("struct FooStub"));
    }

    #[test]
    fn accepts_simple_trait_and_emits_stub_and_dispatch() {
        let s = expand_to_string(parse_quote! {
            trait Counter {
                fn next(&self) -> u64;
                fn reserve(&self, count: u32) -> u64;
            }
        });
        // The compile_error! escape hatch must not have been emitted.
        assert!(
            !s.contains("compile_error"),
            "valid trait must not emit compile_error!, got: {s}"
        );
        // Generated items: the trait declaration itself, plus the stub
        // struct, the stub `impl`, the dispatch helper trait and its
        // blanket impl.
        assert!(s.contains("struct CounterStub"));
        assert!(s.contains("trait CounterDispatch"));
        assert!(s.contains("dispatch_counter"));
    }

    /// Stubs are accepted as parameters of test fixtures decorated with
    /// `#[tracing::instrument]`-style attributes that require every
    /// parameter to implement `Debug`. Pin that the macro generates a
    /// `Debug` impl for the stub, printing the dep id the underlying
    /// `HostedRpcChannel` routes to (the only meaningful field).
    #[test]
    fn emits_debug_impl_on_stub() {
        let s = expand_to_string(parse_quote! {
            trait Counter {
                fn next(&self) -> u64;
            }
        });
        assert!(
            !s.contains("compile_error"),
            "valid trait must not emit compile_error!, got: {s}"
        );
        let normalized: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        // Loose-match the impl header to ignore inserted whitespace
        // from `quote!`.
        assert!(
            normalized.contains("impl :: core :: fmt :: Debug for CounterStub")
                || normalized.contains("impl ::core::fmt::Debug for CounterStub")
                || normalized.contains("impl core :: fmt :: Debug for CounterStub")
                || normalized.contains("impl Debug for CounterStub"),
            "must emit Debug impl for CounterStub, got: {normalized}"
        );
        // The Debug body reports the stub type name and the dep id.
        assert!(
            normalized.contains("\"CounterStub\""),
            "Debug fmt must include the stub type name, got: {normalized}"
        );
        assert!(
            normalized.contains("dep_id"),
            "Debug fmt must include the dep_id field, got: {normalized}"
        );
    }
}
