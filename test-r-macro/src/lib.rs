mod deps;
mod dynamic;
mod helpers;
mod hosted_rpc;
mod suite;
mod test;

use proc_macro::TokenStream;

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

#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    test::test_impl(attr, item, false)
}

#[proc_macro_attribute]
pub fn bench(attr: TokenStream, item: TokenStream) -> TokenStream {
    test::test_impl(attr, item, true)
}

#[proc_macro]
pub fn inherit_test_dep(item: TokenStream) -> TokenStream {
    deps::inherit_test_dep(item)
}

#[proc_macro]
pub fn define_matrix_dimension(item: TokenStream) -> TokenStream {
    deps::define_matrix_dimension(item)
}

#[proc_macro_attribute]
pub fn test_dep(attr: TokenStream, item: TokenStream) -> TokenStream {
    deps::test_dep(attr, item)
}

#[proc_macro_attribute]
pub fn test_gen(_attr: TokenStream, item: TokenStream) -> TokenStream {
    dynamic::test_gen(item)
}

#[proc_macro]
pub fn add_test(input: TokenStream) -> TokenStream {
    dynamic::add_test(input)
}

#[proc_macro_attribute]
pub fn timeout(attr: TokenStream, item: TokenStream) -> TokenStream {
    suite::timeout(attr, item)
}

#[proc_macro]
pub fn timeout_suite(input: TokenStream) -> TokenStream {
    suite::timeout_suite(input)
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
pub fn ignore_detached_panics(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn tag(attr: TokenStream, item: TokenStream) -> TokenStream {
    suite::tag(attr, item)
}

#[proc_macro]
pub fn tag_suite(input: TokenStream) -> TokenStream {
    suite::tag_suite(input)
}

/// `#[matrix_suite(<dim>, <DepType>)]` — apply a previously-defined matrix
/// dimension to every `#[test]` in the annotated module.
///
/// Unlike `tag_suite!` / `sequential_suite!` (which are pure runtime suite
/// properties and can be applied post-hoc by naming the module), `matrix_suite`
/// must be applied **as an attribute on the inline module definition**, because
/// it rewrites the test function signatures at macro-expansion time: each
/// `#[test] fn` in the module whose signature contains a `&<DepType>` parameter
/// gets a `#[dimension(<dim>)]` injected onto that parameter before the inner
/// `#[test]` macro expands. Tests in the module that do not take a `&<DepType>`
/// parameter (or that already carry `#[dimension]` / `#[tagged_as]` on the
/// matching parameter) are left untouched and run exactly once.
///
/// See `book/src/advanced_features/dependency_injection.md` for the worked
/// example and the rationale for the compile-time (Strategy A) approach.
#[proc_macro_attribute]
pub fn matrix_suite(attr: TokenStream, item: TokenStream) -> TokenStream {
    suite::matrix_suite(attr, item)
}

#[proc_macro_attribute]
pub fn sequential(_attr: TokenStream, item: TokenStream) -> TokenStream {
    suite::sequential(item)
}

#[proc_macro]
pub fn sequential_suite(input: TokenStream) -> TokenStream {
    suite::sequential_suite(input)
}

/// HR1.1: trait-driven boilerplate eliminator for `HostedRpcDep` /
/// `AsyncHostedRpcDep`.
///
/// Apply to a user trait declaration to emit a `<Trait>Stub` worker-side
/// struct that implements the trait by routing each call through a
/// [`test_r::core::HostedRpcChannel`], plus a `<Trait>Dispatch` helper
/// trait blanket-implemented for every `T: Trait` so the owner-side
/// dispatch impl can delegate to a generated method-table dispatcher
/// instead of writing the per-method match arms by hand.
///
/// Async-mode is auto-detected: if every method in the trait is declared
/// `async fn`, the macro generates async stub methods and an async
/// dispatch helper, and the owner is expected to implement
/// [`test_r::core::AsyncHostedRpcDep`] instead of
/// [`test_r::core::HostedRpcDep`]. Mixing sync and `async fn` methods in
/// the same `#[hosted_rpc]` trait is a compile error. There is no
/// `#[hosted_rpc(async)]` flag.
///
/// See the rustdoc on the macro module for the precise wire format and
/// the remaining restrictions (no generics, no associated types).
#[proc_macro_attribute]
pub fn hosted_rpc(attr: TokenStream, item: TokenStream) -> TokenStream {
    hosted_rpc::hosted_rpc(attr, item)
}
