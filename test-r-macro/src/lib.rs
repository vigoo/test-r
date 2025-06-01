mod deps;
mod dynamic;
mod helpers;
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
    suite::tag(attr, item)
}

#[proc_macro]
pub fn tag_suite(input: TokenStream) -> TokenStream {
    suite::tag_suite(input)
}

#[proc_macro_attribute]
pub fn sequential(_attr: TokenStream, item: TokenStream) -> TokenStream {
    suite::sequential(item)
}

#[proc_macro]
pub fn sequential_suite(input: TokenStream) -> TokenStream {
    suite::sequential_suite(input)
}
