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

/// `matrix_suite!(<module>, <dim>, <DepType>)` — apply a previously-defined
/// matrix dimension to every `#[test]` in the named module.
///
/// Unlike `tag_suite!` / `sequential_suite!` (which are pure runtime suite
/// properties applied to already-registered tests), `matrix_suite!` works by
/// **runtime test multiplication** (Strategy B): at test-collection time, each
/// registered test under `<module>` whose `dependencies` contain the untagged
/// `<DepType>` dep name is duplicated into one test per case of the `<dim>`
/// dimension. The multiplied cases are named `<test>_<case>`, carry the
/// `<dim>_<case>` auto-tag (selectable via `:tag:`), and have their dependency
/// rewritten to the case-specific tagged dep, with the test closure's
/// compiled getter redirected to that tagged dep via an aliased
/// `DependencyView`.
///
/// Because multiplication happens at runtime over already-registered tests,
/// `<module>` may be a **file-based module** (`mod worker;`) or a directory
/// module (`mod api;` with `api/mod.rs`), not just an inline module. This is
/// the key difference from the earlier compile-time attribute form, which
/// required an inline module body to rewrite test signatures.
///
/// Tests in the module that do not depend on `<DepType>` (or that already
/// carry an explicit `#[dimension]` / `#[tagged_as]` for that dep) are left
/// untouched and run exactly once.
///
/// `matrix_suite!` must be invoked in a scope where the
/// `test_r_get_dep_tags_<dim>()` helper emitted by `define_matrix_dimension!`
/// is in scope (typically the same parent module that declared the dimension).
///
/// See `book/src/advanced_features/dependency_injection.md` for the worked
/// example and the rationale for the runtime-multiplication (Strategy B)
/// approach.
#[proc_macro]
pub fn matrix_suite(item: TokenStream) -> TokenStream {
    suite::matrix_suite(item)
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
